//! KIR の下ろし: `Vec` / `String` / `&str` / `for` / 添字 / クロージャ (設計_KIR.md 10.9、kir/collections)。
//!
//! ヒープは clif と同じ実メモリ表現を使う。`Vec<T>` と `String` は {ptr, len, cap} の 24 バイトのヘッダ。
//! KIR の値は「ヘッダのアドレス」(`ptr` 型) で表し、ヘッダ自体はフレームの領域 (`frame.bytes`、関数の入口で確保) に置く。
//! 所有するバッファは `heap.free` で明示的に解放する。`&Vec<T>` / `&mut Vec<T>` / `&String` は借用で、同じヘッダのアドレスを渡す。
//! `&str` とスライス `&[T]` は {ptr, len} の値 (構造体 `fat`、16 バイト。設計_KIR.md 10.10) で、引数・戻り値・局所変数にそのまま使える。
//!
//! - 解放: 所有する値を束縛した時点で、そのスコープの所有の一覧 (`CollCx::owned`) に入れる。スコープの終わり、
//!   `return`、`?`、`break` / `continue` のたびに、対象のスコープの値を後入れ先出しで解放する (設計_KIR.md 13 章)。
//! - 移動: Vec / String の式の値は 24 バイトの集成体 `vec` (値渡し)。局所変数の移動は、ヘッダから 3 語を読んで
//!   元のヘッダの `ptr` を null にする (`free(null)` は何もしないので、元の場所の解放は自然に無効になる)。
//!   構造体・列挙型・`Option` / `Result` の局所変数は、移したかどうかの印 (フレームの 1 バイト) を持つ。
//!   `for x in v` は `v` を借りて反復するだけで、解放は `v` の寿命の終わりに行う (観測できない違いだけ生じる)。
//! - クロージャ: 捕獲のないクロージャは、本体を別の関数 (鍵 `関数#c式ID`) として下ろす。値は `unit` の印で、
//!   `let` で束縛した名前の呼び出しは直接の呼び出しになる。型引数に型パラメータを持つ関数 (`Fn` 境界) は、
//!   渡されたクロージャごとに単相インスタンス (鍵 `関数<クロージャの鍵>`) を作る。
//! - 対象外 (理由を付けて拒否): `Vec` の `push` / `pop` / `get` など (戻り値が `Result` / `Option` のため)、
//!   `String` の `push_str` (同じ理由)、`&[T]` のスライス (参照)、`capture` を持つクロージャ (参照の捕獲)、
//!   `for` の対象が `core.iter.range(a, b)` と要素型の分かる Vec の変数以外のもの。

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::check::{is_nil, items, nf, nlineno as nline, nname, st, strs};
use crate::kir::{AggKind, Aop, Block, BoolOp, Cc, Func, Inst, Mode, Op, Pay, Pointee, Term, Ty, VId};
use super::{conv, Lw, St, LR};
use crate::parser::{nid, V};
use crate::program::Target;
use crate::types::{from_v, Ty as Src};

const VEC: &str = "alloc.vec.Vec";
/// `alloc.raw.Buf<T, A>` は Vec と同じ 24 バイトのヘッダ {ptr, len, cap} (cap は要素数)。
const BUF: &str = "alloc.raw.Buf";

/// いま下ろしている関数が、`alloc/vec.ksnl` の `Vec` の基本メソッド (中身は `Buf` = 組み込みのヘッダ操作) か。
pub(crate) fn in_vec_base(lw: &Lw) -> bool {
    lw.def.name.strip_prefix("alloc.vec.Vec.").is_some_and(|m| crate::interp::KASANE_VEC_BASE.contains(&m))
}

/// いま下ろしている関数が、`alloc/string.ksnl` の `String` の基本メソッドか (中身は `Buf<u8>` = `String` と同じヘッダ)。
pub(crate) fn in_string_base(lw: &Lw) -> bool {
    lw.def.name.strip_prefix("alloc.string.String.").is_some_and(|m| crate::interp::KASANE_STRING_BASE.contains(&m))
}

/// ヘッダを持つ「Vec の仲間」(`Vec` と `Buf`) の名前か。
pub(crate) fn is_vecn(n: &str) -> bool {
    n == VEC || n == BUF
}
const STRING: &str = "alloc.string.String";
/// `Vec<T>` と `String` のヘッダの大きさ (ptr, len, cap)。
const HDR_VEC: u32 = 24;

/// コレクション系の値の種類 (スロットごと、または式ごと)。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    /// `Vec<T>`。要素型が分かれば `Some`。
    Vec(Option<Ty>),
    /// `String` (`len` と `is_empty` だけ)。
    String,
    /// `&str` (値は借用なので複製してよい。値は `fat` の {ptr, len})。
    Str,
    /// `&[T]` / `&mut [T]` (要素型が分かれば `Some`。値は `fat` の {ptr, len})。
    Slice(Option<Ty>),
    /// `HashMap<K, V>` (設計_KIR.md 16 章)。キーと値の型が分かれば `Some`。キーの `Ty::Ptr` は `&str` のキーの印。
    /// ヘッダは Vec と同じ 24 バイト (所有と移動と解放は Vec と同じ規則)。

    /// `BtreeMap<K, V>` (キーの順を保つ。ヘッダは Map と同じ 24 バイト。設計_KIR.md 17.21)。

    /// 下ろした関数の鍵と、引数・戻り値の型。`caps` は取り込みの値 (この関数の中の値番号。値の取り込みはその値、
    /// 参照の取り込みは参照の値) で、`cap_tys` がその型。呼び出しでは先頭の追加の引数として渡す (設計_KIR.md 13 章)。
    Closure { key: String, args: Vec<Ty>, ret: Ty, caps: Vec<VId>, cap_tys: Vec<Ty> },
}

/// 所有している値。スコープの終わりに、移していなければ解放する。
#[derive(Clone, Copy, Debug)]
pub(crate) enum Owned {
    /// `Vec` / `String` の局所変数。ヘッダのアドレス。移すと `ptr` が null になる。
    Hdr(VId),
    /// 構造体・列挙型・`Option` / `Result` の局所変数。スロットと、移していないとき 1 の印 (フレームの 1 バイト)。
    Val { slot: usize, flag: VId },
    /// 評価済みで、まだ束縛も消費もされていない所有値 (呼び出しの引数・構造体のフィールド・タプルの要素)。
    /// 後ろの式の評価が `?` / `return` / `break` で抜けるときに解放するため、消費の直前まで登録しておく (設計_KIR.md 17.3)。
    Tmp(VId),
    /// `with` の後始末式 (`coll.closes` の添字)。出口 (末尾・`return`・`?`・`break`・`continue`) で、
    /// 束縛した変数の解放より前に (登録の逆順で) 下ろす。
    With(usize),
    /// `static` の `Mutex` のガード: mutex の番地と、ガードをまだ外していないことの印 (フレームの 1 バイト)。
    /// 外すのは、束縛の変数のスコープの終わり・`core.mem.drop(g)`・一時のガードなら文の終わり (`end_guards`)。
    /// 外すときに印を落とすので、同じガードを二度外さない (`release_guard`)。
    Unlock { m: VId, flag: VId },
}

/// 下ろしの状態。`Lw` が 1 つ持つ。
#[derive(Default)]
pub(crate) struct CollCx {
    /// `std.thread.spawn` のクロージャ引数を下ろしている間だけ true (チャネルの端の `move` の取り込みを許し、本体の終わりで落とす)。
    pub(crate) spawn_arg: bool,
    /// スロット番号 → 種類。
    pub(crate) kinds: BTreeMap<usize, Kind>,
    /// 固定長配列への参照のスロット → 要素数 N (`&[T; N]` / `&mut [T; N]` のパラメータと、
    /// アドレスを取られた配列の局所変数)。長さは型から分かるので定数として扱う。
    pub(crate) arr_len: BTreeMap<usize, u32>,
    /// 文の終わりに寿命が尽きる一時値のヘッダ (借用検査の注釈 `scope.end` 用。設計_KIR.md 24 章)。
    pub(crate) temps: Vec<VId>,
    /// 追跡する局所変数 (名前, スロット番号) → 所有の注釈の識別子 (束縛の位置の unit 定数の値番号)。
    pub(crate) own_ids: BTreeMap<(String, usize), VId>,
    /// 所有する値 (スコープの番号, 所有物) の登録順。
    pub(crate) owned: Vec<(usize, Owned)>,
    /// `let g = M.lock()` で束縛された `Mutex` のガードの変数のスロット → (mutex の番地, 印)。`core.mem.drop(g)` で外すため。
    pub(crate) guard_slots: BTreeMap<usize, (VId, VId)>,
    /// いまの文の中で登録され、まだ束縛も外しもされていない `Mutex` のガード (値, mutex の番地, 印)。文の終わりに外す。
    pub(crate) stmt_guards: Vec<(VId, VId, VId)>,
    /// `with` の後始末式の木と、`with` の束縛を置いたスコープの番号。
    pub(crate) closes: Vec<(V, usize)>,
    /// `closes` と同じ添字: 束縛した名前とスロット (所有の注釈。後始末式が束縛を消費する)。
    pub(crate) close_own: Vec<Option<(String, usize)>>,
    /// 後始末式を下ろしている間の、所有の注釈を止めるスロット (式の中の消費は、後で 1 つだけ注釈する)。
    pub(crate) own_mute: Option<usize>,
    /// 後始末式を下ろしている間に起きた最初の下ろしの失敗 (`drop_from` は結果を返さないので持ち越す)。
    pub(crate) close_err: Option<String>,
    /// 関数の入口で確保する `alloca` (ブロック 0 の先頭に置く)。
    pub(crate) allocas: Vec<Inst>,
    /// 借用の (`&Vec` などの) 変数のスロット。所有しないので、移動の検査と解放の対象にしない。
    pub(crate) borrowed: BTreeSet<usize>,
    /// この関数から下ろしたクロージャの関数 (本体の関数とは別に出す)。
    pub(crate) extra: Vec<(Func, BTreeSet<String>)>,
    /// 下ろし済み (または下ろし中) の単相インスタンスの鍵。再帰で同じものを二度下ろさないため。
    pub(crate) instances: BTreeSet<String>,
    /// クロージャの鍵の接頭辞。単相インスタンスの中では、同じ式でもインスタンスごとに別の関数になるので鍵を分ける。
    pub(crate) prefix: String,
    /// パターンの `Some(x)` / `Ok(x)` / `Err(x)` の部分パターンの、型表の型 (Vec / String の種類を局所変数に結び付ける)。
    pub(crate) hint: Option<Src>,
    /// いま下ろしている `match` の対象の (KIR の型, 型表の型)。`Some(x)` の `x` の種類の手がかりを取るため。
    pub(crate) scrut_src: Option<(Ty, Src)>,
    /// いま判定している `match` の腕のパターンの束縛名 → 型 (`scrut_src` をパターンに沿って分解したもの)。
    /// `&str` などの借用の束縛に種類 (`str` など) を結び付けるため。腕の判定の間だけ入れ、済んだら空にする。
    pub(crate) pat_srcs: BTreeMap<String, Src>,
    /// `Option<&T>` の `Some(..)` の中の束縛 (判定のときは作らず、腕の頭 = 当たったあとで作る。`None` の番地を読まないため。設計_KIR.md 17.18)。
    pub(crate) pending_bref: Vec<(V, VId, Ty)>,
    /// Vec / String のヘッダ (フレームの領域) → 値の型 (`vec` または要素が所有物の `vec<#N>`)。移すときの型と、解放のとき要素を解放するかに使う。
    pub(crate) hdr_ty: BTreeMap<VId, Ty>,
    /// 構造体のフィールド (場所) を `match` の対象として読んだ直後か (`scrut_begin` が読んで下ろす)。値は移していない。
    pub(crate) peek_field: bool,
    /// 下ろしたクロージャの式 ID → (取り込みの値, その型)。`closure_kind_from_table` が読む。
    pub(crate) caps: BTreeMap<u32, (Vec<VId>, Vec<Ty>)>,
    /// 添字式 `v[i]` (所有物を持つ要素の Vec) の木の番地 → 要素の借用を持つスロット。`elem_prep` が作り、
    /// `local_slot` / `chain_of` が受け手として読む。
    pub(crate) elem_slots: BTreeMap<usize, usize>,
}

thread_local! {
    /// 下ろしたクロージャ・関数の鍵 → (uses, may_panic)。効果の型引数 (`uses [E] panics P`) を持つ単相インスタンスの効果を求める。
    static KEY_EFF: std::cell::RefCell<BTreeMap<String, (Vec<String>, bool)>> = std::cell::RefCell::new(BTreeMap::new());
}

fn key_eff(key: &str) -> Option<(Vec<String>, bool)> {
    KEY_EFF.with(|k| k.borrow().get(key).cloned())
}

/// 型表の式の型 (式 ID が 0 なら無い)。
pub(super) fn table_ty(lw: &Lw, e: &V) -> Option<Src> {
    let id = nid(e);
    if id == 0 {
        return None;
    }
    lw.tys.get(&(lw.def.name.clone(), id)).map(|x| if lw.subst.is_empty() { x.ty.clone() } else { super::mono::subst_ty(&x.ty, &lw.subst) })
}

fn strip_ref(t: &Src) -> &Src {
    let mut t = t;
    while let Src::Ref(_, inner) = t {
        t = inner;
    }
    t
}

/// 式の値の段階 1 の型 (Vec / String は `ptr`、`&str` とスライスは `fat`、クロージャは `unit` の値)。対象外なら `None`。
pub(crate) fn value_ty(lw: &Lw, t: &Src) -> Option<Ty> {
    // `&&str` (`&K` の K が `&str`) は、`&str` の値の番地 (参照の参照は段を外さない)。
    if let Src::Ref(_, i) = t {
        if matches!(&**i, Src::Ref(..)) {
            return None;
        }
    }
    // 値渡しの Vec / String は集成体 `vec` (所有の移動)、参照は借用なのでヘッダのアドレス。
    if let Src::Named(n, args) = t {
        if is_vecn(n) {
            let elem = args.first().and_then(|a| lw.conv_t(a));
            return Some(lw.aggs.borrow_mut().vec_of(elem));
        }
        if n == STRING {
            return Some(lw.aggs.borrow_mut().vec_hdr());
        }
    }
    match strip_ref(t) {
        Src::Named(n, _) if is_vecn(n) || n == STRING => Some(Ty::Ptr),
        Src::Prim(s) if s == "str" => Some(lw.fat_ty()),
        Src::Slice(e) if conv(e).or_else(|| lw.conv_t(e)).and_then(|t| elem_size(lw, t)).is_some() => Some(lw.fat_ty()),
        Src::Closure { .. } => Some(Ty::Unit),
        _ => None,
    }
}

/// `Vec` / `String` の名前の型か。
pub(crate) fn is_hdr_named(t: &Src) -> bool {
    matches!(t, Src::Named(n, _) if is_vecn(n) || n == STRING)
}

/// 関数の引数・戻り値の型。通常の型に加えて、Vec / String / `str` / スライスへの参照 (借用) を受ける。
pub(crate) fn sig_ty(lw: &Lw, t: &Src) -> Option<Ty> {
    // `&mut [T]` / `&[T]` (配列への参照) は、配列の値は `fat` だがパラメータとしては要素の先頭 `ptr` を受け取る。
    if let Src::Ref(_, inner) = t {
        if matches!(&**inner, Src::Array(_)) {
            return Some(Ty::Ptr);
        }
    }
    if let Some(x) = lw.conv_t(t) {
        return Some(x);
    }
    // 生ポインタ `*const T` / `*mut T` は `ptr`。
    if let Src::Ptr(_, inner) = t {
        if matches!(&**inner, Src::Prim(_)) {
            return Some(Ty::Ptr);
        }
    }
    let Src::Ref(mutbl, inner) = t else { return None };
    match &**inner {
        Src::Named(n, _) if is_vecn(n) || n == STRING => Some(Ty::Ptr),
        Src::Prim(s) if s == "str" => Some(lw.fat_ty()),
        Src::Slice(e) if conv(e).or_else(|| lw.conv_t(e)).and_then(|t| elem_size(lw, t)).is_some() => Some(lw.fat_ty()),
        Src::Named(n, _) if n == "std.net.TcpListener" || n == "std.net.TcpStream" || n == "std.net.UdpSocket" => {
            let ai = lw.aggs.borrow().agg_index(n).expect("std.net.Tcp* が型表にない");
            Some(Ty::Ref(*mutbl, Pointee::Agg(ai)))
        }
        _ => None,
    }
}

/// 引数を局所変数にする。値渡しの所有物は所有に入れ (入口のスコープ)、借用の引数は種類を結び付ける。
pub(crate) fn bind_param(lw: &mut Lw, name: &str, v: VId, t: Ty, src: &Src) {
    if owns_heap(lw, t) {
        lw.coll.hint = Some(src.clone());
        bind_owned(lw, name, v, t);
        lw.coll.hint = None;
        return;
    }
    lw.bind_let(name, v, t);
    bind_param_kind(lw, name, src);
}

/// 借用の引数のスロットに、種類 (Vec / String / str / スライス) を結び付ける。
pub(crate) fn bind_param_kind(lw: &mut Lw, name: &str, t: &Src) {
    if !matches!(t, Src::Ref(..)) {
        return;
    }
    if let (Some(s), Some(k)) = (lw.lookup(name), kind_of_src(lw, t)) {
        lw.coll.kinds.insert(s, k);
        lw.coll.borrowed.insert(s);
    }
}

/// `&v` / `&mut v` (v は Vec / String の局所変数): ヘッダのアドレスそのもの。
pub(crate) fn borrow_header(lw: &mut Lw, operand: &V, line: usize) -> LR<VId> {
    if let Some(s) = local_slot(lw, operand) {
        if matches!(lw.coll.kinds.get(&s), Some(Kind::Vec(_)) | Some(Kind::String)) {
            // 借用・メソッドの受け手の読みは、move 済みの値の読み (注釈)。
            if let Some(n) = super::path1(operand) {
                lw.own_emit(&n, s, crate::kir::OwnKind::Use);
            }
            return Ok(lw.slots[s].0);
        }
    }
    // `&x.f.g` (x は構造体の局所変数で、f.g は Vec / String のフィールド): フィールドのヘッダのアドレス。
    if nname(operand) == "Path" {
        let segs = strs(&nf(operand)[0]);
        if segs.len() > 1 {
            if let Some(s) = lw.lookup(&segs[0]) {
                let base = lw.slots[s].0;
                let mut cur = match lw.vty(base) {
                    Ty::Ref(_, Pointee::Agg(_)) => Some(base),
                    _ if lw.is_mem_val(base) => Some(base),
                    _ => None,
                };
                if let Some(mut c) = cur.take() {
                    for name in &segs[1..] {
                        c = lw.field_addr(c, name, line)?;
                    }
                    if let Ty::Ref(_, Pointee::Agg(k)) = lw.vty(c) {
                        if super::agg::is_vec_name(&lw.aggs.borrow().defs[k as usize].name) {
                            return Ok(lw.emit(Ty::Ptr, Op::Bitcast { src: lw.vty(c), a: c }));
                        }
                    }
                }
            }
        }
    }
    // 局所変数でも局所の経路でもない式 (`&t.mk()` の一時値): 評価した所有値をフレームのヘッダに置き、
    // 現在のスコープの終わりまで所有する (木走査では文の終わりまでだが、解放の時期は観測できない)。
    if !matches!(nname(operand), "Path" | "Index") {
        if let Some(v) = lw.expr(operand)? {
            if lw.aggs.borrow().is_vec(lw.vty(v)) {
                let h = adopt(lw, v);
                // 文の終わりで寿命が尽きる一時値 (借用検査の注釈。`let r = &t.mk()` の直接の借用は束縛まで延びるので `block` が除く)。
                lw.coll.temps.push(h);
                return Ok(h);
            }
        }
    }
    lw.err(line, "Vec / String の借用は局所変数だけ段階 1 の対象")
}

pub(super) fn kind_of_src(lw: &Lw, t: &Src) -> Option<Kind> {
    match strip_ref(t) {
        // 要素が所有物 (`Vec<String>`) のときは、要素の型 (集成体 `vec` など) を持つ
        Src::Named(n, args) if is_vecn(n) => Some(Kind::Vec(args.first().and_then(|a| conv(a).or_else(|| lw.conv_t(a)).or_else(|| sig_ty(lw, a).filter(|t| *t == Ty::Ptr))))),
        Src::Named(n, _) if n == STRING => Some(Kind::String),
        Src::Prim(s) if s == "str" => Some(Kind::Str),
        // 要素が所有物 (`String` など、集成体) のときも要素の型を持つ。`as_slice()` の一時値
        // が続く `get` / `len` / 添字 / `iter` で要素型を引き継ぐため、`size_of` ではなく
        // `elem_size` で弾く (F32/F64 など `elem_size` が `None` を返す型は早期に拒否し、
        // 行番号を正しく保つ)。
        Src::Slice(e) => Some(Kind::Slice(conv(e).or_else(|| lw.conv_t(e)).filter(|t| elem_size(lw, *t).is_some()))),
        // 固定長配列 `&[T; N]` の要素型。長さ N は `arr_len` で別途追跡する。
        Src::Array(e) => Some(Kind::Slice(conv(e).or_else(|| lw.conv_t(e)).filter(|t| elem_size(lw, *t).is_some()))),
        _ => None,
    }
}


/// クロージャの鍵。式 ID は関数 (または単相インスタンス) の中で一意なので、同じ中の別のクロージャと衝突しない。
fn closure_key(lw: &Lw, id: u32) -> String {
    let base = if lw.coll.prefix.is_empty() { lw.def.name.as_str() } else { lw.coll.prefix.as_str() };
    format!("{}#c{}", base, id)
}

/// 単一の名前の経路なら、その名前のスロット (見えているもの)。
pub(crate) fn local_slot(lw: &Lw, e: &V) -> Option<usize> {
    if nname(e) == "Index" {
        return lw.coll.elem_slots.get(&(e as *const V as usize)).copied();
    }
    if nname(e) != "Path" {
        return None;
    }
    let segs = strs(&nf(e)[0]);
    if segs.len() != 1 {
        return None;
    }
    lw.lookup(&segs[0])
}

/// `&[T; N]` / `&mut [T; N]` の型 V (`["ref", B, ["array", elem, N]]`) から N を取る。
/// 型表の `Src::Array` には長さが入らないので、元の型 V から読む。
pub(crate) fn ref_array_len(pt: &V) -> Option<u32> {
    let V::L(fs) = pt else { return None };
    if fs.len() < 3 || st(&fs[0]) != "ref" { return None; }
    let V::L(inner) = &fs[2] else { return None };
    if inner.is_empty() || st(&inner[0]) != "array" { return None; }
    let V::N(n) = &inner[2] else { return None };
    n.parse::<u32>().ok()
}

/// `&[T; N]` のパラメータのスロット `s` の長さ N。
pub(crate) fn arr_len_of(lw: &Lw, s: usize) -> Option<u32> {
    lw.coll.arr_len.get(&s).copied()
}

/// 受け手の種類。局所変数はスロットの種類、それ以外は型表の型から決める。
fn kind_of_expr(lw: &Lw, e: &V) -> Option<Kind> {
    if let Some(s) = local_slot(lw, e) {
        if let Some(k) = lw.coll.kinds.get(&s).cloned() {
            return Some(k);
        }
    }
    // 文字列リテラル / バイト列リテラルは式そのままなので、型表に無くても種類を返す。
    if nname(e) == "Lit" {
        let f = nf(e);
        match st(&f[0]).as_str() {
            "str" => return Some(Kind::Str),
            "bstr" => return Some(Kind::Slice(Some(Ty::U8))),
            _ => {}
        }
    }
    table_ty(lw, e).and_then(|t| kind_of_src(lw, &t))
}

pub(crate) fn emit_alloca(lw: &mut Lw, size: u32) -> VId {
    let dst = lw.new_val(Ty::Ptr);
    lw.coll.allocas.push(Inst { dst, ty: Ty::Ptr, op: Op::FrameBytes { size } });
    dst
}

pub(super) fn load_len(lw: &mut Lw, hdr: VId) -> VId {
    if matches!(lw.vty(hdr), Ty::Agg(_)) {
        return lw.emit(Ty::Usize, Op::Field { a: hdr, off: 8 });
    }
    lw.emit(Ty::Usize, Op::MemLoad { p: hdr, off: 8 })
}

/// 要素の型の大きさ。スカラーと集成体 (所有物の `vec` や構造体)。
pub(super) fn elem_size(lw: &Lw, t: Ty) -> Option<u32> {
    match t {
        Ty::Agg(i) => Some(lw.aggs.borrow().defs[i as usize].size),
        t => size_of(t),
    }
}

/// アドレス `addr` の要素を読む (集成体は参照へ `bitcast` して `load`)。
pub(super) fn load_elem(lw: &mut Lw, addr: VId, elem: Ty) -> VId {
    match elem {
        Ty::Agg(i) => {
            let r = lw.emit(Ty::Ref(true, Pointee::Agg(i)), Op::Bitcast { src: Ty::Ptr, a: addr });
            lw.emit(elem, Op::Load(r))
        }
        t => lw.emit(t, Op::MemLoad { p: addr, off: 0 }),
    }
}

/// アドレス `addr` へ要素 `v` を書く。
pub(super) fn store_elem(lw: &mut Lw, addr: VId, elem: Ty, v: VId) {
    match elem {
        Ty::Agg(i) => {
            let r = lw.emit(Ty::Ref(true, Pointee::Agg(i)), Op::Bitcast { src: Ty::Ptr, a: addr });
            lw.emit(Ty::Unit, Op::Store { ptr: r, val: v });
        }
        t => {
            lw.emit(Ty::Unit, Op::MemStore { ty: t, p: addr, off: 0, v });
        }
    }
}

pub(super) fn size_of(t: Ty) -> Option<u32> {
    Some(match t {
        Ty::I8 | Ty::U8 | Ty::Bool => 1,
        Ty::I16 | Ty::U16 => 2,
        Ty::I32 | Ty::U32 => 4,
        Ty::I64 | Ty::U64 | Ty::Isize | Ty::Usize | Ty::Ptr => 8,
        _ => return None,
    })
}

/// 局所変数のヘッダ `hdr` の所有物を解放する。要素が所有物の Vec は、各要素を解放してからバッファを解放する。
fn drop_hdr(lw: &mut Lw, hdr: VId) {
    let vt = lw.coll.hdr_ty.get(&hdr).copied();
    if let Some(vt) = vt.filter(|t| lw.aggs.borrow().vec_elem(*t).is_some()) {
        let p = lw.emit(Ty::Ptr, Op::MemLoad { p: hdr, off: 0 });
        let l = lw.emit(Ty::Usize, Op::MemLoad { p: hdr, off: 8 });
        let c = lw.emit(Ty::Usize, Op::MemLoad { p: hdr, off: 16 });
        let v = lw.emit(vt, Op::Make { variant: None, fields: vec![p, l, c] });
        drop_value(lw, v, vt);
        return;
    }
    emit_free(lw, hdr);
}

/// 所有するバッファを解放する命令列を、現在のブロックに置く (`null` なら何もしない)。
pub(super) fn emit_free(lw: &mut Lw, hdr: VId) {
    let buf = lw.emit(Ty::Ptr, Op::MemLoad { p: hdr, off: 0 });
    lw.emit(Ty::Unit, Op::HeapFree(buf));
}

/// 他の下ろしの子モジュールから使う `owns_heap` (所有物を持つ型か)。
pub(super) fn owns_heap_pub(lw: &Lw, t: Ty) -> bool {
    owns_heap(lw, t)
}

fn owns_heap(lw: &Lw, t: Ty) -> bool {
    lw.aggs.borrow().owns_heap(t)
}

/// 型 `t` の値が移動と解放の対象 (所有物を持つ) か。
pub(crate) fn owns(lw: &Lw, t: Ty) -> bool {
    owns_heap(lw, t)
}

/// ヘッダ `hdr` に、集成体 `vec` の値 (ptr, len, cap) を書く。
pub(super) fn store_vec(lw: &mut Lw, hdr: VId, v: VId) {
    let p = lw.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
    let l = lw.emit(Ty::Usize, Op::Field { a: v, off: 8 });
    let c = lw.emit(Ty::Usize, Op::Field { a: v, off: 16 });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: hdr, off: 0, v: p });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: hdr, off: 8, v: l });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: hdr, off: 16, v: c });
}

/// 集成体 `vec` の値を局所変数にする: フレームにヘッダを作って値を書き、現在のスコープの所有に入れる。ヘッダのアドレスを返す。
pub(crate) fn adopt(lw: &mut Lw, v: VId) -> VId {
    let hdr = emit_alloca(lw, HDR_VEC);
    store_vec(lw, hdr, v);
    lw.coll.hdr_ty.insert(hdr, lw.vty(v));
    let scope = lw.scopes.len() - 1;
    lw.coll.owned.push((scope, Owned::Hdr(hdr)));
    hdr
}

/// 所有するヘッダから値を移す: 3 語を読み、元の `ptr` を null にする (元の場所の解放は何もしなくなる)。
pub(super) fn move_hdr(lw: &mut Lw, hdr: VId) -> VId {
    let p = lw.emit(Ty::Ptr, Op::MemLoad { p: hdr, off: 0 });
    let l = lw.emit(Ty::Usize, Op::MemLoad { p: hdr, off: 8 });
    let c = lw.emit(Ty::Usize, Op::MemLoad { p: hdr, off: 16 });
    let null = lw.emit(Ty::Ptr, Op::Const(0));
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: hdr, off: 0, v: null });
    let vt = lw.coll.hdr_ty.get(&hdr).copied().unwrap_or_else(|| lw.aggs.borrow_mut().vec_hdr());
    // 要素が所有物なら、移した後の解放が (null のバッファの) 要素をたどらないように長さも 0 にする
    if lw.aggs.borrow().vec_elem(vt).is_some() {
        let zero = lw.emit(Ty::Usize, Op::Const(0));
        lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: hdr, off: 8, v: zero });
    }
    lw.emit(vt, Op::Make { variant: None, fields: vec![p, l, c] })
}

/// 局所変数の印 (移したかどうかのフラグ) のアドレス。印を持たない変数なら `None`。
fn val_flag(lw: &Lw, s: usize) -> Option<VId> {
    lw.coll.owned.iter().rev().find_map(|(_, o)| match o {
        Owned::Val { slot, flag } if *slot == s => Some(*flag),
        _ => None,
    })
}

fn set_flag(lw: &mut Lw, flag: VId, on: bool) {
    let b = lw.emit(Ty::Bool, Op::Const(on as i128));
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Bool, p: flag, off: 0, v: b });
}

/// 局所変数 (所有物の印を持つもの) か。式が単一の名前の経路で、そのスロットが印を持つなら、スロット番号。
pub(crate) fn val_slot(lw: &Lw, e: &V) -> Option<usize> {
    local_slot(lw, e).filter(|s| val_flag(lw, *s).is_some())
}

/// スロット `s` が所有物の印を持つ局所変数か。
pub(crate) fn val_slot_of(lw: &Lw, s: usize) -> bool {
    val_flag(lw, s).is_some()
}

/// 局所変数の読み (値としての使用 = 移動)。所有する Vec / String はヘッダから値を移し、印を持つ変数は印を下ろす。
pub(crate) fn path_read(lw: &mut Lw, s: usize) -> VId {
    if matches!(lw.coll.kinds.get(&s), Some(Kind::Vec(_)) | Some(Kind::String)) && !lw.coll.borrowed.contains(&s) {
        let h = lw.slots[s].0;
        return move_hdr(lw, h);
    }
    let v = lw.read_slot(s);
    if let Some(flag) = val_flag(lw, s) {
        set_flag(lw, flag, false);
    }
    v
}

/// 局所変数の値を移さずに読む (フィールドの読み・メソッドの受け手・`match` の対象など、場所としての使用)。
pub(crate) fn path_peek(lw: &mut Lw, s: usize) -> VId {
    lw.read_slot(s)
}

/// 要素が所有物の Vec の値 `v` (バッファ `buf`) の、先頭から `len` 個の要素を解放する。
pub(super) fn drop_elems(lw: &mut Lw, v: VId, buf: VId, elem: Ty) {
    let Ty::Agg(ei) = elem else { return };
    let size = lw.aggs.borrow().defs[ei as usize].size;
    let len = lw.emit(Ty::Usize, Op::Field { a: v, off: 8 });
    let zero = lw.emit(Ty::Usize, Op::Const(0));
    let (hd, hp) = lw.new_block(&[Ty::Usize]);
    let (body, _) = lw.new_block(&[]);
    let (exit, _) = lw.new_block(&[]);
    lw.goto(hd, vec![zero]);
    lw.cur = Some(hd);
    let i = hp[0];
    let c = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: i, b: len });
    lw.branch(c, (body, vec![]), (exit, vec![]));
    lw.cur = Some(body);
    let k = lw.emit(Ty::Usize, Op::Const(size as i128));
    let off = lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: i, b: k });
    let addr = lw.emit(Ty::Ptr, Op::PtrAdd { p: buf, off });
    let rt = Ty::Ref(true, Pointee::Agg(ei));
    let r = lw.emit(rt, Op::Bitcast { src: Ty::Ptr, a: addr });
    let e = lw.emit(elem, Op::Load(r));
    drop_value(lw, e, elem);
    let one = lw.emit(Ty::Usize, Op::Const(1));
    let i1 = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: i, b: one });
    lw.goto(hd, vec![i1]);
    lw.cur = Some(exit);
}

/// ヒープの番地 `p` から型 `t` の値を読む (`ptr` を同じアドレスの参照にして `load`)。
pub(crate) fn box_load(lw: &mut Lw, p: VId, t: Ty) -> VId {
    let pt = Pointee::of(t).expect("Box の中身は参照できる型");
    let r = lw.emit(Ty::Ref(true, pt), Op::Bitcast { src: Ty::Ptr, a: p });
    lw.emit(t, Op::Load(r))
}

/// `derive [Drop]` の型 `t` の解放の補助関数 `drop.<FQN>(p, skip)` を呼ぶ。`p` は `t` の値を置いた確保 (箱の確保かスロット)、
/// `skip` は解放しない番地 (箱なら `null`、スロットなら `p`)。補助関数の本体は `kir_lower_drop.rs`。
pub(crate) fn rec_drop_call(lw: &mut Lw, t: Ty, p: VId, skip: VId) {
    let Ty::Agg(i) = t else { return };
    let name = format!("drop.{}", lw.aggs.borrow().defs[i as usize].name);
    lw.aggs.borrow_mut().drop_need.insert(i);
    lw.callees.insert(name.clone());
    lw.emit(Ty::Unit, Op::Call { callee: name, args: vec![p, skip] });
}

/// 型 `t` の値 `v` (所有している) を解放する命令列を、現在のブロックに置く。所有物を持たない型は何もしない。
/// 列挙型と `Option` / `Result` は、タグで枝を分けて、持っている変種のペイロードだけを解放する。
pub(crate) fn drop_value(lw: &mut Lw, v: VId, t: Ty) {
    if lw.cur.is_none() || !owns_heap(lw, t) {
        return;
    }
    match t {
        Ty::Agg(i) => {
            let d = lw.aggs.borrow().defs[i as usize].clone();
            // `Box<dyn Tr>`: 中身の型は実行時の vtable の番号で決まるので、補助関数 `dyn.drop` が中身を解放してから確保を返す。
            if d.name.starts_with("dynbox<") {
                let p = lw.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
                let vt = lw.emit(Ty::Usize, Op::Field { a: v, off: 8 });
                lw.aggs.borrow_mut().dynr.drop_used = true;
                lw.callees.insert("dyn.drop".to_string());
                lw.emit(Ty::Unit, Op::Call { callee: "dyn.drop".to_string(), args: vec![p, vt] });
                return;
            }
            // `Box<T>`: 中身が所有物なら先に解放してから、確保を返す。
            let belem = lw.aggs.borrow().box_elem(t);
            if let Some(elem) = belem {
                let p = lw.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
                // `derive [Drop]` の再帰する型の箱: 反復する補助関数が、木全体を解放して箱の確保も返す。
                if lw.aggs.borrow().is_rec_drop(elem) {
                    let null = lw.emit(Ty::Ptr, Op::Const(0));
                    rec_drop_call(lw, elem, p, null);
                    return;
                }
                if owns_heap(lw, elem) {
                    let inner = box_load(lw, p, elem);
                    drop_value(lw, inner, elem);
                }
                lw.emit(Ty::Unit, Op::HeapFree(p));
                return;
            }
            if super::agg::is_vec_name(&d.name) {
                let p = lw.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
                // 要素が所有物なら、各要素を先に解放する (設計_KIR.md 13.8)
                let elem = lw.aggs.borrow().vec_elem(t);
                if let Some(elem) = elem {
                    drop_elems(lw, v, p, elem);
                }
                lw.emit(Ty::Unit, Op::HeapFree(p));
                return;
            }
            // チャネルの端: 本数を減らし、最後の 1 本なら状態を解放する (設計_KIR.md 22 章)。
            let end = lw.aggs.borrow().chan_kind(t);
            if let Some(sender) = end {
                let elem = lw.aggs.borrow().chan_elem.get(&i).copied().unwrap_or(Ty::U8);
                super::chan_lower::drop_end(lw, v, sender, elem);
                return;
            }
            // `derive [Drop]` の再帰する型の値そのもの: スロットへ置いて、補助関数に木全体を解放させる (スロット自体は解放しない)。
            if lw.aggs.borrow().is_rec_drop(t) {
                let size = lw.aggs.borrow().defs[i as usize].size.max(8);
                let slot = emit_alloca(lw, size);
                let pt = Pointee::of(t).expect("集成体は参照できる");
                let r = lw.emit(Ty::Ref(true, pt), Op::Bitcast { src: Ty::Ptr, a: slot });
                lw.emit(Ty::Unit, Op::Store { ptr: r, val: v });
                rec_drop_call(lw, t, slot, slot);
                return;
            }
            match d.kind {
                AggKind::Struct(fs) | AggKind::Tuple(fs) => {
                    for f in fs.iter().rev() {
                        if owns_heap(lw, f.ty) {
                            let fv = lw.emit(f.ty, Op::Field { a: v, off: f.off });
                            drop_value(lw, fv, f.ty);
                        }
                    }
                }
                AggKind::Enum(vs) => {
                    let (join, _) = lw.new_block(&[]);
                    let tag = lw.emit(Ty::I32, Op::Tag(v));
                    for (k, var) in vs.iter().enumerate() {
                        if !var.fields.iter().any(|f| owns_heap(lw, f.ty)) {
                            continue;
                        }
                        let kk = lw.emit(Ty::I32, Op::Const(k as i128));
                        let c = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::I32, a: tag, b: kk });
                        let (yes, _) = lw.new_block(&[]);
                        let (no, _) = lw.new_block(&[]);
                        lw.branch(c, (yes, vec![]), (no, vec![]));
                        lw.cur = Some(yes);
                        for f in var.fields.iter().rev() {
                            if owns_heap(lw, f.ty) {
                                let fv = lw.emit(f.ty, Op::Field { a: v, off: f.off });
                                drop_value(lw, fv, f.ty);
                            }
                        }
                        lw.goto(join, vec![]);
                        lw.cur = Some(no);
                    }
                    lw.goto(join, vec![]);
                    lw.cur = Some(join);
                }
            }
        }
        Ty::Opt(p) => {
            let tag = lw.emit(Ty::Bool, Op::Tag(v));
            let (yes, _) = lw.new_block(&[]);
            let (join, _) = lw.new_block(&[]);
            lw.branch(tag, (yes, vec![]), (join, vec![]));
            lw.cur = Some(yes);
            let pv = lw.emit(p.ty(), Op::Payload(v));
            drop_value(lw, pv, p.ty());
            lw.goto(join, vec![]);
            lw.cur = Some(join);
        }
        Ty::Res(a, e) => {
            let tag = lw.emit(Ty::Bool, Op::Tag(v));
            let (errb, _) = lw.new_block(&[]);
            let (okb, _) = lw.new_block(&[]);
            let (join, _) = lw.new_block(&[]);
            lw.branch(tag, (errb, vec![]), (okb, vec![]));
            for (b, pay) in [(errb, e), (okb, a)] {
                lw.cur = Some(b);
                if owns_heap(lw, pay.ty()) {
                    let pv = lw.emit(pay.ty(), Op::Payload(v));
                    drop_value(lw, pv, pay.ty());
                }
                lw.goto(join, vec![]);
            }
            lw.cur = Some(join);
        }
        _ => {}
    }
}

/// 所有物 1 つを解放する。印を持つ変数は、印が立っているときだけ現在の値を解放する。
fn drop_owned(lw: &mut Lw, o: Owned) {
    match o {
        Owned::Hdr(h) => {
            drop_hdr(lw, h);
            if lw.cur.is_some() {
                lw.emit(Ty::Unit, Op::ScopeEnd(h));
            }
        }
        Owned::Tmp(v) => {
            let t = lw.vty(v);
            drop_value(lw, v, t);
        }
        Owned::With(i) => run_close(lw, i),
        Owned::Unlock { m, flag } => release_guard(lw, m, flag),
        Owned::Val { slot, flag } => {
            let t = lw.slots[slot].1;
            let on = lw.emit(Ty::Bool, Op::MemLoad { p: flag, off: 0 });
            let (yes, _) = lw.new_block(&[]);
            let (join, _) = lw.new_block(&[]);
            lw.branch(on, (yes, vec![]), (join, vec![]));
            lw.cur = Some(yes);
            let v = lw.read_slot(slot);
            drop_value(lw, v, t);
            lw.goto(join, vec![]);
            lw.cur = Some(join);
        }
    }
}

/// `with` の後始末式を、いまの位置に下ろす。名前は `with` の束縛のスコープから見えるものだけ
/// (本体の内側の同名の束縛に隠されない)。自分より後に登録された所有物 (内側) はこの時点で解放済みなので、
/// 式の中の `?` / `return` が自分や内側を二重に後始末しないよう、登録を一時的に外す。
fn run_close(lw: &mut Lw, i: usize) {
    if lw.cur.is_none() {
        return;
    }
    let (expr, depth) = lw.coll.closes[i].clone();
    let pos = lw.coll.owned.iter().position(|(_, o)| matches!(o, Owned::With(j) if *j == i)).expect("登録済みの with");
    let hidden_owned = lw.coll.owned.split_off(pos);
    let hidden_scopes = lw.scopes.split_off(depth + 1);
    // 束縛の消費は後始末式 (`close`) が行う。式の中の消費・借用の注釈は止め、下ろした後に 1 つだけ付ける。
    let own = lw.coll.close_own[i].clone();
    let prev_mute = lw.coll.own_mute;
    if let Some((_, s)) = &own {
        lw.coll.own_mute = Some(*s);
    }
    let r = lw.expr(&expr);
    lw.coll.own_mute = prev_mute;
    lw.scopes.extend(hidden_scopes);
    lw.coll.owned.truncate(pos);
    lw.coll.owned.extend(hidden_owned);
    if let Some((name, s)) = own {
        lw.own_emit(&name, s, crate::kir::OwnKind::Consume);
    }
    match r {
        Ok(Some(v)) => drop_temp(lw, v),
        Ok(None) => {}
        Err(e) => {
            if lw.coll.close_err.is_none() {
                lw.coll.close_err = Some(e);
            }
        }
    }
}

/// `static` の `Mutex` の `lock()` の結果 `value` (ガード) を登録する。印を立てて、現在のスコープの所有に入れる。
/// 束縛されなければ、文の終わり (`end_guards`) に外す。束縛されれば (`bind_guard`) 変数の寿命か `core.mem.drop` まで保つ。
pub(crate) fn register_unlock(lw: &mut Lw, m: VId, value: VId) {
    let flag = emit_alloca(lw, 1);
    set_flag(lw, flag, true);
    let scope = lw.scopes.len() - 1;
    lw.coll.owned.push((scope, Owned::Unlock { m, flag }));
    lw.coll.stmt_guards.push((value, m, flag));
}

/// ガードを外す (印が立っているときだけ、`munlock` して印を落とす)。`munlock` は `mu` の下で `gen` を進めて待ちを起こす。
pub(crate) fn release_guard(lw: &mut Lw, m: VId, flag: VId) {
    if lw.cur.is_none() {
        return;
    }
    let on = lw.emit(Ty::Bool, Op::MemLoad { p: flag, off: 0 });
    let (yes, _) = lw.new_block(&[]);
    let (join, _) = lw.new_block(&[]);
    lw.branch(on, (yes, vec![]), (join, vec![]));
    lw.cur = Some(yes);
    set_flag(lw, flag, false);
    super::thread_lower::rt_call(lw, super::thread_lower::MUNLOCK, vec![m], Ty::Unit);
    lw.goto(join, vec![]);
    lw.cur = Some(join);
}

/// 文・条件の始まりの、未束縛のガードの数 (`end_guards` に渡す)。
pub(crate) fn guard_mark(lw: &Lw) -> usize {
    lw.coll.stmt_guards.len()
}

/// 文 (または条件式) の終わり: `mark` より後に登録された、束縛されなかったガードを外す。
pub(crate) fn end_guards(lw: &mut Lw, mark: usize) {
    let ended = lw.coll.stmt_guards.split_off(mark);
    for (_, m, flag) in ended {
        release_guard(lw, m, flag);
    }
}

/// `let g = M.lock()` の束縛: 値 `v` が未束縛のガードなら、その変数のスロットに結び付けて文の終わりで外さないようにする。
pub(crate) fn bind_guard(lw: &mut Lw, name: &str, v: VId) {
    let Some(pos) = lw.coll.stmt_guards.iter().position(|(x, _, _)| *x == v) else { return };
    let (_, m, flag) = lw.coll.stmt_guards.remove(pos);
    let s = lw.lookup(name).expect("直前に束縛した名前");
    lw.coll.guard_slots.insert(s, (m, flag));
}

/// `with` の本体の前に呼ぶ: 後始末式を現在のスコープ (束縛を置いたスコープ) に登録する。
pub(crate) fn register_close(lw: &mut Lw, expr: V, own: Option<(String, usize)>) {
    let scope = lw.scopes.len() - 1;
    lw.coll.closes.push((expr, scope));
    lw.coll.close_own.push(own);
    let i = lw.coll.closes.len() - 1;
    lw.coll.owned.push((scope, Owned::With(i)));
}

/// スコープ `depth` 以降に登録された所有値を、後入れ先出しで解放する (`break` / `continue` / `return` / `?` の後始末)。
/// 現在のブロックが無い (到達しない) ときは何もしない。登録は消さない (他の経路で必要なため)。
pub(crate) fn drop_from(lw: &mut Lw, depth: usize) {
    if lw.cur.is_none() {
        return;
    }
    let items: Vec<Owned> = lw.coll.owned.iter().rev().filter(|(d, _)| *d >= depth).map(|(_, o)| *o).collect();
    for o in items {
        drop_owned(lw, o);
    }
}

/// スコープの終わり: 所有値を解放し、そのスコープの登録を消す。スコープを閉じるのは呼び出し側 (`scopes.pop`)。
/// ブロックの値は移し済み (式の評価で所有が移っている) なので、ここでは調べない。
pub(crate) fn block_end(lw: &mut Lw) {
    let depth = lw.scopes.len() - 1;
    drop_from(lw, depth);
    lw.coll.owned.retain(|(d, _)| *d < depth);
}

/// 評価済みの値 `v` が所有物なら、消費されるまでの間、現在のスコープの一時の所有に登録する。
pub(crate) fn tmp_hold(lw: &mut Lw, v: VId) {
    let t = lw.vty(v);
    if owns_heap(lw, t) {
        let scope = lw.scopes.len() - 1;
        lw.coll.owned.push((scope, Owned::Tmp(v)));
    }
}

/// `tmp_hold` した値を外す (呼び出しや構築が消費する直前、または評価が途中で終わったとき)。
pub(crate) fn tmp_release(lw: &mut Lw, vs: &[VId]) {
    lw.coll.owned.retain(|(_, o)| !matches!(o, Owned::Tmp(x) if vs.contains(x)));
}

/// 捨てる値 (文として評価した式の値) が所有物なら解放する。
pub(crate) fn drop_temp(lw: &mut Lw, v: VId) {
    let t = lw.vty(v);
    drop_value(lw, v, t);
}

/// 型 `src` の値を局所変数 `name` として束縛する (所有物なら所有に入れる)。
/// Vec / String はフレームのヘッダに置き、それ以外の所有物は印 (フレームの 1 バイト) を付ける。
pub(crate) fn bind_owned(lw: &mut Lw, name: &str, v: VId, t: Ty) {
    if !owns_heap(lw, t) {
        lw.bind_let(name, v, t);
        return;
    }
    if lw.aggs.borrow().is_vec(t) {
        let h = adopt(lw, v);
        lw.bind(name, h, Ty::Ptr);
        let s = lw.lookup(name).expect("直前に束縛した名前");
        // 種類 (Vec の要素型 / String) は、パターンの型の手がかりか、無ければ Vec の要素型不明として持つ。
        let k = match lw.coll.hint.take() {
            Some(h) => kind_of_src(lw, &h),
            None => None,
        };
        lw.coll.kinds.insert(s, k.unwrap_or(Kind::Vec(None)));
        return;
    }
    lw.bind_let(name, v, t);
    let s = lw.lookup(name).expect("直前に束縛した名前");
    let flag = emit_alloca(lw, 1);
    set_flag(lw, flag, true);
    let scope = lw.scopes.len() - 1;
    lw.coll.owned.push((scope, Owned::Val { slot: s, flag }));
}

/// 局所変数への代入 `x = v`。所有物は、古い値を解放してから新しい値を入れる (Vec / String はヘッダを書き直す)。
/// 印を持つ変数は印を立て直す (移した後の再代入を含む)。
pub(crate) fn assign_checked(lw: &mut Lw, s: usize, v: VId, line: usize) -> LR<()> {
    if matches!(lw.coll.kinds.get(&s), Some(Kind::Vec(_)) | Some(Kind::String)) && !lw.coll.borrowed.contains(&s) {
        let h = lw.slots[s].0;
        let vt = lw.coll.hdr_ty.get(&h).copied().unwrap_or_else(|| lw.aggs.borrow_mut().vec_hdr());
        if lw.vty(v) != vt {
            return lw.err(line, "代入の型が変数の型と違う");
        }
        drop_hdr(lw, h);
        store_vec(lw, h, v);
        return Ok(());
    }
    if lw.vty(v) != lw.slots[s].1 {
        return lw.err(line, "代入の型が変数の型と違う");
    }
    if let Some(flag) = val_flag(lw, s) {
        drop_owned(lw, Owned::Val { slot: s, flag });
        lw.assign_slot(s, v);
        set_flag(lw, flag, true);
    } else {
        lw.assign_slot(s, v);
    }
    Ok(())
}

/// パターンが、所有物を束縛 (移動) するか。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Cover {
    /// 所有物を束縛しない (`_`、リテラル、所有物を持たない変種)。値は腕の頭で丸ごと解放する。
    No,
    /// 値の持つ所有物をすべて名前に束縛する (所有は束縛した変数が引き継ぐ)。
    All,
}

fn combine(lw: &Lw, _line: usize, subs: &[V], ftys: &[Ty]) -> LR<Cover> {
    let mut all = false;
    for (sp, ft) in subs.iter().zip(ftys) {
        if owns_heap(lw, *ft) && pat_cover(lw, sp, *ft)? == Cover::All {
            all = true;
        }
    }
    Ok(if all { Cover::All } else { Cover::No })
}

/// パターン `p` を所有物を持つ型 `t` の値に当てたとき、所有物を束縛するか。
fn pat_cover(lw: &Lw, p: &V, t: Ty) -> LR<Cover> {
    let line = nline(p);
    if !owns_heap(lw, t) {
        return Ok(Cover::No);
    }
    let agg = |t: Ty| match t {
        Ty::Agg(i) => Some(lw.aggs.borrow().defs[i as usize].clone()),
        _ => None,
    };
    match nname(p) {
        "PWild" | "PLit" | "PPath" => Ok(Cover::No),
        "PBind" => Ok(Cover::All),
        "PVariant" => {
            let segs = strs(&nf(p)[0]);
            let subs = items(&nf(p)[1]);
            let ftys: Vec<Ty> = match (segs.as_slice(), t) {
                ([n], Ty::Opt(p0)) if n == "Some" => vec![p0.ty()],
                ([n], Ty::Res(t0, _)) if n == "Ok" => vec![t0.ty()],
                ([n], Ty::Res(_, e0)) if n == "Err" => vec![e0.ty()],
                _ => {
                    let Some(d) = agg(t) else { return lw.err(line, "パターンの対象の型が合わない") };
                    let (Some(crate::program::Target::Variant(_, vn)), AggKind::Enum(vs)) = (lw.p.resolve(&segs, &lw.def.file).map(|r| r.target), &d.kind) else {
                        return lw.err(line, "列挙子のパターンの対象が列挙型でない");
                    };
                    let Some(v) = vs.iter().find(|x| x.name == vn) else { return lw.err(line, "列挙子がない") };
                    v.fields.iter().map(|f| f.ty).collect()
                }
            };
            combine(lw, line, subs, &ftys)
        }
        "PTuple" => {
            let Some(d) = agg(t) else { return lw.err(line, "タプルのパターンの対象が集成体でない") };
            let AggKind::Tuple(fs) = &d.kind else { return lw.err(line, "タプルのパターンの対象がタプルでない") };
            let ftys: Vec<Ty> = fs.iter().map(|f| f.ty).collect();
            combine(lw, line, items(&nf(p)[0]), &ftys)
        }
        "PStruct" => {
            let Some(d) = agg(t) else { return lw.err(line, "構造体のパターンの対象が集成体でない") };
            let AggKind::Struct(fs) = &d.kind else { return lw.err(line, "構造体のパターンの対象が構造体でない") };
            let mut all = false;
            for it in items(&nf(p)[1]) {
                let V::L(xs) = it else { return lw.err(line, "構造体のパターンの形が不正") };
                let Some(fd) = fs.iter().find(|x| x.name == st(&xs[0])) else { return lw.err(line, "フィールドがない") };
                if owns_heap(lw, fd.ty) && pat_cover(lw, &xs[1], fd.ty)? == Cover::All {
                    all = true;
                }
            }
            Ok(if all { Cover::All } else { Cover::No })
        }
        other => lw.err(line, format!("パターン {} は所有物を持つ型に対して段階 1 の対象外", other)),
    }
}

/// `match` の対象の所有の扱い。腕ごとに、所有物を束縛するかを持つ。
pub(crate) struct Scrut {
    /// この `match` が対象の値を所有しているか (束縛しない腕は頭で解放する)。
    owned: bool,
    t: Ty,
    covers: Vec<Cover>,
}

/// `match` の対象の値 `sv` (所有物を持つ型) について、所有を決める。持たない型なら `None`。
/// 局所変数 (印を持つ) で、どれかの腕が束縛するなら、対象を移す (印を下ろす)。束縛しないなら移さない。
/// 一時の値 (呼び出しの結果など) は、この `match` が所有する。
pub(crate) fn scrut_begin(lw: &mut Lw, scrut: &V, sv: VId, pats: &[&V]) -> LR<Option<Scrut>> {
    let t = lw.vty(sv);
    let peeked = std::mem::take(&mut lw.coll.peek_field);
    if !owns_heap(lw, t) {
        return Ok(None);
    }
    let covers: Vec<Cover> = pats.iter().map(|p| pat_cover(lw, p, t)).collect::<LR<_>>()?;
    let moves = covers.contains(&Cover::All);
    // 構造体のフィールド (場所) の値を対象にする `match`: 所有物を束縛しない腕だけなら借りて見るだけ。
    // 束縛する腕があるなら、フィールドから移し出す (部分的な移動。元のフィールドは空になり、`match` が値を所有する)。
    if peeked {
        if !moves {
            return Ok(Some(Scrut { owned: false, t, covers }));
        }
        let line = nline(scrut);
        let Some((s, names)) = super::own::chain_of(lw, scrut) else {
            return lw.err(line, "フィールドの値を所有物を束縛する match の対象にする形 (部分的な移動) は段階 1 の対象外");
        };
        if names.is_empty() {
            return lw.err(line, "フィールドの値を所有物を束縛する match の対象にする形 (部分的な移動) は段階 1 の対象外");
        }
        if let Some(root) = root_name(scrut) {
            lw.own_emit(&root, s, crate::kir::OwnKind::Part);
        }
        field_clear(lw, s, &names, sv, line)?;
        return Ok(Some(Scrut { owned: true, t, covers }));
    }
    let slot = val_slot(lw, scrut);
    if let (Some(s), true) = (slot, moves) {
        if let Some(flag) = val_flag(lw, s) {
            set_flag(lw, flag, false);
        }
    }
    Ok(Some(Scrut { owned: slot.is_none() || moves, t, covers }))
}

/// `x.f.g` の根の局所変数の名前 (`x`)。根が名前でなければ `None`。
fn root_name(e: &V) -> Option<String> {
    match nname(e) {
        "Path" => strs(&nf(e)[0]).first().cloned(),
        "Field" => root_name(&nf(e)[0]),
        _ => None,
    }
}

/// 腕 `i` の頭 (パターンが当たった後): 所有していて、この腕が所有物を束縛しないなら、対象の値を丸ごと解放する。
pub(crate) fn scrut_arm(lw: &mut Lw, sc: &Option<Scrut>, i: usize, pat: &V, sv: VId) -> LR<()> {
    if let Some(sc) = sc {
        if sc.owned {
            if sc.covers[i] == Cover::No {
                drop_value(lw, sv, sc.t);
            } else {
                drop_unbound(lw, pat, sv, sc.t)?;
            }
        }
    }
    Ok(())
}

/// パターン `p` が当たった所有物を持つ値 `v` (型 `t`) のうち、パターンが名前に束縛しなかった所有物を解放する
/// (分解の「束縛しなかった部分は分解の時点で解放する」。設計_KIR.md 13.7)。束縛した部分は束縛した変数が所有する。
pub(crate) fn drop_unbound(lw: &mut Lw, p: &V, v: VId, t: Ty) -> LR<()> {
    if lw.cur.is_none() || !owns_heap(lw, t) {
        return Ok(());
    }
    let line = nline(p);
    match nname(p) {
        "PBind" => {}
        "PWild" | "PLit" | "PPath" => drop_value(lw, v, t),
        "PTuple" | "PStruct" => {
            let Ty::Agg(i) = t else { return lw.err(line, "パターンの対象が集成体でない") };
            let d = lw.aggs.borrow().defs[i as usize].clone();
            let fs = match &d.kind {
                AggKind::Struct(fs) | AggKind::Tuple(fs) => fs.clone(),
                _ => return lw.err(line, "パターンの対象が構造体・タプルでない"),
            };
            // (フィールド, 部分パターン)。構造体のパターンに書かれなかったフィールドは束縛されない (`..`)
            let mut subs: Vec<(crate::kir::Field, Option<V>)> = vec![];
            if nname(p) == "PTuple" {
                let ps = items(&nf(p)[0]);
                if ps.len() != fs.len() {
                    return lw.err(line, "タプルのパターンの要素数が型と合わない");
                }
                for (f, sp) in fs.iter().zip(ps) {
                    subs.push((f.clone(), Some(sp.clone())));
                }
            } else {
                for f in &fs {
                    let sp = items(&nf(p)[1]).iter().find_map(|it| match it {
                        V::L(xs) if st(&xs[0]) == f.name => Some(xs[1].clone()),
                        _ => None,
                    });
                    subs.push((f.clone(), sp));
                }
            }
            for (f, sp) in subs.iter().rev() {
                if !owns_heap(lw, f.ty) {
                    continue;
                }
                let fv = lw.emit(f.ty, Op::Field { a: v, off: f.off });
                match sp {
                    Some(sp) => drop_unbound(lw, sp, fv, f.ty)?,
                    None => drop_value(lw, fv, f.ty),
                }
            }
        }
        "PVariant" => {
            let segs = strs(&nf(p)[0]);
            let ps = items(&nf(p)[1]);
            let (pays, via_payload): (Vec<(Ty, u32)>, bool) = match (segs.as_slice(), t) {
                ([n], Ty::Opt(p0)) if n == "Some" => (vec![(p0.ty(), 0)], true),
                ([n], Ty::Res(t0, _)) if n == "Ok" => (vec![(t0.ty(), 0)], true),
                ([n], Ty::Res(_, e0)) if n == "Err" => (vec![(e0.ty(), 0)], true),
                _ => {
                    let Ty::Agg(i) = t else { return lw.err(line, "列挙子のパターンの対象が集成体でない") };
                    let d = lw.aggs.borrow().defs[i as usize].clone();
                    let (Some(crate::program::Target::Variant(_, vn)), AggKind::Enum(vs)) = (lw.p.resolve(&segs, &lw.def.file).map(|r| r.target), &d.kind) else {
                        return lw.err(line, "列挙子のパターンの対象が列挙型でない");
                    };
                    let Some(var) = vs.iter().find(|x| x.name == vn) else { return lw.err(line, "列挙子がない") };
                    (var.fields.iter().map(|f| (f.ty, f.off)).collect(), false)
                }
            };
            if ps.len() != pays.len() {
                return lw.err(line, "列挙子のパターンの要素数が違う");
            }
            for ((ft, off), sp) in pays.iter().zip(ps).rev() {
                if !owns_heap(lw, *ft) {
                    continue;
                }
                let fv = if via_payload { lw.emit(*ft, Op::Payload(v)) } else { lw.emit(*ft, Op::Field { a: v, off: *off }) };
                drop_unbound(lw, sp, fv, *ft)?;
            }
        }
        other => return lw.err(line, format!("パターン {} は所有物を持つ型に対して段階 1 の対象外", other)),
    }
    Ok(())
}

/// パターン `p` の束縛名ごとの型表の型 (`src` は対象の型)。Vec / String の種類を局所変数に結び付けるための手がかり。分からない部分は載せない。
pub(crate) fn pat_srcs(lw: &Lw, p: &V, src: &Src, out: &mut BTreeMap<String, Src>) {
    let gn = HashSet::new();
    match (nname(p), src) {
        ("PBind", _) => {
            out.insert(st(&nf(p)[0]), src.clone());
        }
        ("PTuple", Src::Tuple(xs)) => {
            for (sp, x) in items(&nf(p)[0]).iter().zip(xs) {
                pat_srcs(lw, sp, x, out);
            }
        }
        ("PStruct", Src::Named(n, _)) => {
            let Some(def) = lw.p.defs.iter().find(|d| d.kw == "type" && d.name == *n) else { return };
            for it in items(&nf(p)[1]) {
                if let V::L(xs) = it {
                    if let Some(fd) = def.fields.iter().find(|f| f.0 == st(&xs[0])) {
                        pat_srcs(lw, &xs[1], &from_v(&fd.1, &gn), out);
                    }
                }
            }
        }
        ("PVariant", Src::Named(n, args)) => {
            let segs = strs(&nf(p)[0]);
            let ps = items(&nf(p)[1]);
            let tys: Vec<Src> = match (n.as_str(), segs.as_slice()) {
                ("Option", [c]) if c == "Some" => args.first().cloned().into_iter().collect(),
                ("Result", [c]) if c == "Ok" => args.first().cloned().into_iter().collect(),
                ("Result", [c]) if c == "Err" => args.get(1).cloned().into_iter().collect(),
                _ => {
                    let Some(def) = lw.p.defs.iter().find(|d| d.kw == "enum" && d.name == *n) else { return };
                    let Some(vn) = segs.last() else { return };
                    let Some(var) = def.variants.iter().find(|v| v.0 == *vn) else { return };
                    var.1.iter().map(|x| from_v(x, &gn)).collect()
                }
            };
            for (sp, x) in ps.iter().zip(&tys) {
                pat_srcs(lw, sp, x, out);
            }
        }
        _ => {}
    }
}

/// 局所変数 `s` から始まるフィールド `names` の所有物を、局所変数の値のまま移し出す (部分的な移動)。
/// 元の場所には「空」の値 (`vec` は `{null, 0, 0}`、`Option` は `None`、構造体・タプルは所有物のフィールドだけ空) を書くので、
/// 元の構造体を後で解放しても何も起きない。別のフィールドの使用や、移したフィールドへの再代入では元に戻る (4.1)。
pub(crate) fn field_move(lw: &mut Lw, s: usize, names: &[String], line: usize) -> LR<VId> {
    let v = field_read(lw, s, names, line)?;
    field_clear(lw, s, names, v, line)?;
    Ok(v)
}

/// 局所変数 `s` のフィールド `names` の現在の値 (読むだけ)。
fn field_read(lw: &mut Lw, s: usize, names: &[String], line: usize) -> LR<VId> {
    let (p, t) = lw.slots[s];
    match t {
        Ty::Ref(..) => lw.err(line, "参照を通したフィールドの移動は段階 1 の対象外 (借用の中身を移すことになる)"),
        Ty::Agg(_) if lw.is_mem_val(p) => {
            let cur = field_addr_path(lw, p, names, line)?;
            let Ty::Ref(_, pt) = lw.vty(cur) else { return lw.err(line, "フィールドのアドレスが参照でない") };
            Ok(lw.emit(pt.ty(), Op::Load(cur)))
        }
        Ty::Agg(_) => {
            let base = lw.read_slot(s);
            lw.field_path(base, names, line)
        }
        _ => lw.err(line, "フィールドの移動の根が構造体でない"),
    }
}

/// `field_read` で読んだ値 `v` (所有物を持つ) を、元のフィールドから移し出した後の「空」にする (解放しても何も起きない)。
/// `match` の対象のフィールドを束縛付きの腕で移すときも、同じ後始末を使う。
pub(crate) fn field_clear(lw: &mut Lw, s: usize, names: &[String], v: VId, line: usize) -> LR<()> {
    let (p, t) = lw.slots[s];
    let ft = lw.vty(v);
    let nv = null_value(lw, v, ft, line)?;
    match t {
        Ty::Agg(_) if lw.is_mem_val(p) => {
            let cur = field_addr_path(lw, p, names, line)?;
            lw.emit(Ty::Unit, Op::Store { ptr: cur, val: nv });
        }
        Ty::Agg(_) => {
            let mut segs = vec![String::new()];
            segs.extend(names.iter().cloned());
            lw.store_segs(s, &segs, nv, line, false)?;
        }
        _ => return lw.err(line, "フィールドの移動の根が構造体でない"),
    }
    Ok(())
}

/// 集成体への参照 (メモリ上の局所) `p` からフィールドの列 `names` をたどったアドレス。
fn field_addr_path(lw: &mut Lw, p: VId, names: &[String], line: usize) -> LR<VId> {
    let mut cur = p;
    for n in names {
        cur = lw.field_addr(cur, n, line)?;
    }
    Ok(cur)
}

/// 所有物を持つ値 `old` (型 `t`) の「空」の値。解放しても何も起きない。作れない型 (ペイロードを持つ列挙型など) は拒否する。
fn null_value(lw: &mut Lw, old: VId, t: Ty, line: usize) -> LR<VId> {
    if !owns_heap(lw, t) {
        return Ok(old);
    }
    if lw.aggs.borrow().is_vec(t) {
        let null = lw.emit(Ty::Ptr, Op::Const(0));
        let zero = lw.emit(Ty::Usize, Op::Const(0));
        return Ok(lw.emit(t, Op::Make { variant: None, fields: vec![null, zero, zero] }));
    }
    match t {
        Ty::Opt(_) => Ok(lw.emit(t, Op::Const(0))),
        Ty::Agg(i) => {
            let d = lw.aggs.borrow().defs[i as usize].clone();
            let fs = match &d.kind {
                AggKind::Struct(fs) | AggKind::Tuple(fs) => fs.clone(),
                _ => return lw.err(line, "ペイロードを持つ列挙型のフィールドの移動は段階 1 の対象外"),
            };
            let mut vals = vec![];
            for f in &fs {
                let fv = lw.emit(f.ty, Op::Field { a: old, off: f.off });
                vals.push(null_value(lw, fv, f.ty, line)?);
            }
            Ok(lw.emit(t, Op::Make { variant: None, fields: vals }))
        }
        _ => lw.err(line, "Result のフィールドの移動は段階 1 の対象外"),
    }
}

/// パターンの `Some(x)` / `Ok(x)` / `Err(x)` の部分パターンの型の手がかり (対象の型表の型から)。
pub(crate) fn set_payload_hint(lw: &mut Lw, scrut_src: &Src, ctor: &str) {
    lw.coll.hint = match (scrut_src, ctor) {
        (Src::Named(n, a), "Some") if n == "Option" => a.first().cloned(),
        (Src::Named(n, a), "Ok") if n == "Result" => a.first().cloned(),
        (Src::Named(n, a), "Err") if n == "Result" => a.get(1).cloned(),
        _ => None,
    };
}

/// 代入の先: クロージャの変数は代入できない。
pub(crate) fn check_assign(lw: &Lw, s: usize, line: usize) -> LR<()> {
    if matches!(lw.coll.kinds.get(&s), Some(Kind::Closure { .. })) {
        return lw.err(line, "クロージャの変数への代入は段階 1 の対象外");
    }
    Ok(())
}

/// `let x = init` の後に、`x` の種類を決める。
pub(crate) fn after_let(lw: &mut Lw, name: &str, init: &V) -> LR<()> {
    let Some(slot) = lw.lookup(name) else { return Ok(()) };
    // `let a = [e1, ..]`: 値は `{ptr, len}` (スライス)。要素型は先頭の要素の型
    if nname(init) == "ArrayE" {
        if let Some(first) = items(&nf(init)[0]).first() {
            let elem = lw.ty_of(first)?;
            lw.coll.kinds.insert(slot, Kind::Slice(Some(elem)));
        }
        return Ok(());
    }
    if nname(init) == "Closure" {
        if let Some(k) = closure_kind_from_table(lw, init) {
            lw.coll.kinds.insert(slot, k);
        }
        return Ok(());
    }
    if let Some(k) = fn_item_kind(lw, init) {
        lw.coll.kinds.insert(slot, k);
        return Ok(());
    }
    if let Some(s) = local_slot(lw, init) {
        if let Some(k) = lw.coll.kinds.get(&s).cloned() {
            lw.coll.kinds.insert(slot, k);
            if lw.coll.borrowed.contains(&s) {
                lw.coll.borrowed.insert(slot);
            }
        }
        return Ok(());
    }
    if let Some(t) = table_ty(lw, init) {
        if let Some(k) = kind_of_src(lw, &t) {
            lw.coll.kinds.insert(slot, k);
            if matches!(t, Src::Ref(..)) {
                lw.coll.borrowed.insert(slot);
            }
        }
    }
    Ok(())
}

/// クロージャの式 (本体は `closure_lit` で下ろした後) の種類。型表の `Closure` 型から作る。
fn closure_kind_from_table(lw: &Lw, e: &V) -> Option<Kind> {
    let Some(Src::Closure { args, ret, .. }) = table_ty(lw, e) else { return None };
    let ats: Option<Vec<Ty>> = args.iter().map(|a| lw.conv_t(a)).collect();
    let (args, ret) = (ats?, lw.conv_t(&ret)?);
    let (caps, cap_tys) = lw.coll.caps.get(&nid(e)).cloned().unwrap_or_default();
    Some(Kind::Closure { key: closure_key(lw, nid(e)), args, ret, caps, cap_tys })
}

/// 関数の本体の後: 入口の `alloca` をブロック 0 の先頭に置く。
pub(crate) fn finish(blocks: &mut [Block], allocas: Vec<Inst>) {
    if allocas.is_empty() {
        return;
    }
    let rest = std::mem::take(&mut blocks[0].insts);
    blocks[0].insts = allocas.into_iter().chain(rest).collect();
}

/// `Vec::new(std.alloc.SYSTEM)` / `String::new(std.alloc.SYSTEM)`: 空 (null, 0, 0) の値 `vec`。局所変数にするとき `adopt` がヘッダを作る。
fn new_header(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    let f = nf(e);
    let line = nline(e);
    let args = items(&f[1]);
    let system = args.len() == 1 && nname(&args[0]) == "Path" && strs(&nf(&args[0])[0]) == ["std", "alloc", "SYSTEM"];
    if !system {
        return lw.err(line, "Vec / String の生成は std.alloc.SYSTEM だけ段階 1 の対象");
    }
    let null = lw.emit(Ty::Ptr, Op::Const(0));
    let zero = lw.emit(Ty::Usize, Op::Const(0));
    // 型表の型 (`Vec<String>` なら要素が所有物の `vec<#N>`)。決まらなければ `vec`
    let vt = match lw.ty_of(e) {
        Ok(t) if lw.aggs.borrow().is_vec(t) => t,
        _ => lw.aggs.borrow_mut().vec_hdr(),
    };
    Ok(Some(lw.emit(vt, Op::Make { variant: None, fields: vec![null, zero, zero] })))
}

fn mem_drop(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    let args = items(&nf(e)[1]);
    if args.len() != 1 {
        return lw.err(nline(e), "core.mem.drop の引数は 1 つのはず");
    }
    // `core.mem.drop(g)` で束縛された `Mutex` のガードを、その場で外す (ガードは値として消費される。4.4)。
    if nname(&args[0]) == "Path" {
        let segs = strs(&nf(&args[0])[0]);
        if let [name] = segs.as_slice() {
            if let Some(s) = lw.lookup(name) {
                if let Some((m, flag)) = lw.coll.guard_slots.get(&s).copied() {
                    release_guard(lw, m, flag);
                    return Ok(Some(lw.unit()));
                }
            }
        }
    }
    let Some(v) = lw.expr(&args[0])? else { return Ok(None) };
    drop_temp(lw, v);
    Ok(Some(lw.unit()))
}

/// `&mut` の先 (値の読み書きの仕方を決めた形)。`Ref` は参照 (スカラー・所有物を持たない値・構造体)、
/// `Hdr` は所有する Vec / String のヘッダ (3 語の番地。`adopt` と同じ並び)。
enum MutPlace {
    Ref(VId, Ty),
    Hdr(VId, Ty),
}

impl MutPlace {
    fn ty(&self) -> Ty {
        match self {
            MutPlace::Ref(_, t) | MutPlace::Hdr(_, t) => *t,
        }
    }
}

/// `core.mem.replace` / `swap` の引数 `e` (`&mut 先` の式や、`&mut` の型の変数) を、先の形に直す。
fn mut_place(lw: &mut Lw, e: &V, line: usize) -> LR<Option<MutPlace>> {
    let inner = match lw.src_ty(e) {
        Ok(Src::Ref(true, inner)) => inner,
        _ => return lw.err(line, "core.mem の引数は &mut の先 (名前・フィールド・参照) だけ段階 1 の対象"),
    };
    if matches!(&*inner, Src::Named(n, _) if n == "alloc.collections.HashMap" || n == "alloc.collections.BtreeMap") {
        return lw.err(line, "core.mem の引数の HashMap は段階 1 の対象外 (ヘッダの種類を保てない)");
    }
    let Some(t) = lw.conv_t(&inner) else {
        return lw.err(line, format!("core.mem の引数の指す先の型 {} は段階 1 の対象外", super::show(&inner)));
    };
    let Some(v) = lw.expr(e)? else { return Ok(None) };
    if is_hdr_named(&inner) {
        if lw.vty(v) != Ty::Ptr {
            return lw.err(line, "所有する Vec / String の &mut の形が段階 1 の対象外");
        }
        return Ok(Some(MutPlace::Hdr(v, t)));
    }
    match lw.vty(v) {
        Ty::Ref(true, _) => Ok(Some(MutPlace::Ref(v, t))),
        _ => lw.err(line, "core.mem の引数は &mut の参照でない"),
    }
}

/// 先の値を読む。`Ref` は指す先を複写し、`Hdr` は 3 語から値を作る (どちらも先は書き換えるまで元の値を持つ)。
fn place_load(lw: &mut Lw, p: &MutPlace) -> VId {
    match p {
        MutPlace::Ref(r, t) => lw.emit(*t, Op::Load(*r)),
        MutPlace::Hdr(h, t) => {
            let ptr = lw.emit(Ty::Ptr, Op::MemLoad { p: *h, off: 0 });
            let len = lw.emit(Ty::Usize, Op::MemLoad { p: *h, off: 8 });
            let cap = lw.emit(Ty::Usize, Op::MemLoad { p: *h, off: 16 });
            lw.emit(*t, Op::Make { variant: None, fields: vec![ptr, len, cap] })
        }
    }
}

/// 先に値を書く。古い値は解放しない (呼び出し側が受け取るか、入れ替えの相手が持つ)。
fn place_store(lw: &mut Lw, p: &MutPlace, v: VId) {
    match p {
        MutPlace::Ref(r, _) => {
            lw.emit(Ty::Unit, Op::Store { ptr: *r, val: v });
        }
        MutPlace::Hdr(h, _) => store_vec(lw, *h, v),
    }
}

/// `core.mem.replace(dst, v)`: `dst` の古い値を返し、先に `v` を書く。古い値は解放しない (返した値の持ち主になる)。
fn mem_replace(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    let args = items(&nf(e)[1]);
    let line = nline(e);
    if args.len() != 2 {
        return lw.err(line, "core.mem.replace の引数は 2 つのはず");
    }
    let Some(dst) = mut_place(lw, &args[0], line)? else { return Ok(None) };
    let Some(v) = lw.expr(&args[1])? else { return Ok(None) };
    if lw.vty(v) != dst.ty() {
        return lw.err(line, "core.mem.replace の値の型が指す先の型と違う");
    }
    let old = place_load(lw, &dst);
    place_store(lw, &dst, v);
    Ok(Some(old))
}

/// `core.mem.swap(a, b)`: 二つの先の値を入れ替える (両方を読んでから書く)。
fn mem_swap(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    let args = items(&nf(e)[1]);
    let line = nline(e);
    if args.len() != 2 {
        return lw.err(line, "core.mem.swap の引数は 2 つのはず");
    }
    let Some(a) = mut_place(lw, &args[0], line)? else { return Ok(None) };
    let Some(b) = mut_place(lw, &args[1], line)? else { return Ok(None) };
    if a.ty() != b.ty() {
        return lw.err(line, "core.mem.swap の二つの値の型が違う");
    }
    let va = place_load(lw, &a);
    let vb = place_load(lw, &b);
    place_store(lw, &a, vb);
    place_store(lw, &b, va);
    Ok(Some(lw.unit()))
}

/// 関数呼び出しのうち、コレクションの生成と、クロージャ値の呼び出しを扱う。扱わないなら `None`。
pub(crate) fn try_call(lw: &mut Lw, e: &V) -> Option<LR<Option<VId>>> {
    let f = nf(e);
    if nname(&f[0]) != "Path" {
        return None;
    }
    let segs = strs(&nf(&f[0])[0]);
    if segs.len() == 1 {
        if let Some(s) = lw.lookup(&segs[0]) {
            if matches!(lw.coll.kinds.get(&s), Some(Kind::Closure { .. })) {
                return Some(call_closure(lw, e, s, false));
            }
        }
    }
    // 反復子の終端 (`fold` / `count`) は、連なりを 1 つのループにする。
    if segs == ["core", "iter", "fold"] || segs == ["core", "iter", "count"] {
        return Some(super::iter_lower::pipe_call(lw, e, segs[2] == "fold"));
    }
    // 型引数に型パラメータを持つ関数 (`Fn` 境界の引数) は、渡されたクロージャごとに単相化する。
    if let Some(Target::Fn(di)) = lw.p.resolve(&segs, &lw.def.file).map(|r| r.target) {
        if (lw.p.defs[di].generics.iter().any(|g| g.0 == "type") || has_fn_param(&lw.p.defs[di])) && has_closure_arg(lw, items(&f[1])) {
            return Some(generic_call(lw, e, di));
        }
    }
    if segs.len() == 1 {
        return None;
    }
    if segs.len() == 2 {
        if let Some(s) = lw.lookup(&segs[0]) {
            if let Some(kind) = lw.coll.kinds.get(&s).cloned() {
                if !matches!(kind, Kind::Closure { .. }) {
                    return Some(try_local_method(lw, e, s, kind, &segs[1]));
                }
                // `f.call(args)` / `f.call_once(args)` / `f.call_mut(args)`: 引数はタプル。`f(args..)` と同じ呼び出しにする。
                if matches!(segs[1].as_str(), "call" | "call_once" | "call_mut") {
                    return Some(call_closure(lw, e, s, true));
                }
            }
        }
    }
    if segs.len() >= 3 {
        if let Some(s) = lw.lookup(&segs[0]) {
            if let Some(r) = super::own::field_call(lw, e, s, &segs[1..segs.len() - 1], &segs[segs.len() - 1], items(&f[1])) {
                return Some(r);
            }
        }
    }
    // `import alloc.vec` の別名の形 (`vec.Vec.new`) も、解決した FQN で照合する。
    let segs: Vec<String> = match lw.p.resolve(&segs, &lw.def.file) {
        Some(r) if r.rest.is_empty() => match r.target {
            Target::Native(n) => n.split('.').map(String::from).collect(),
            Target::Sig(di) | Target::Fn(di) => lw.p.defs[di].name.split('.').map(String::from).collect(),
            _ => segs,
        },
        _ => segs,
    };
    let segs: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    match segs.as_slice() {
        ["alloc", "vec", "Vec", "new"] => Some(new_header(lw, e)),
        ["alloc", "raw", "Buf", "new"] => Some(super::buf::new(lw, e)),
        ["alloc", "raw", "Buf", "with_capacity"] => Some(super::buf::with_capacity(lw, e)),
        ["alloc", "string", "String", "new"] => Some(new_header(lw, e)),
        ["alloc", "string", "String", "from_str"] => {
            Some(lw.from_str_call(e, items(&f[1]), nline(e)))
        }
        ["core", "str", "from_utf8"] => {
            Some(lw.from_utf8_call(e, items(&f[1]), nline(e)))
        }
        // `core.mem.drop(x)`: 値を移して、その場で解放する。
        ["core", "mem", "drop"] => Some(mem_drop(lw, e)),
        ["core", "mem", "replace"] => Some(mem_replace(lw, e)),
        ["core", "mem", "swap"] => Some(mem_swap(lw, e)),
        _ => None,
    }
}

/// 型引数ではなく、`fn(..) -> R` の型で書かれた引数があるか (型引数と同じく、渡されたクロージャごとに単相化する)。
fn has_fn_param(cd: &crate::program::Def) -> bool {
    let gn = HashSet::new();
    cd.params.iter().any(|(_, t)| matches!(from_v(t, &gn), Src::Closure { .. }))
}

/// 境界に `core.ops.Fn` / `FnMut` / `FnOnce` を持つ型パラメータの名前 (クロージャを取る型引数)。
pub(crate) fn closure_params(cd: &crate::program::Def) -> HashSet<String> {
    cd.generics
        .iter()
        .filter(|g| g.0 == "type" && format!("{:?}", g.2).contains("core.ops.Fn"))
        .map(|g| g.1.clone())
        .collect()
}

/// 引数の型が型引数 `F: Fn` (または `&F`) か `fn(..)` の型なら、束縛の名前 (型引数の名前、または `fn:引数名`)。
/// `F` の位置の型パラメータが `Fn` の境界を持たないもの (`T`) は値の引数。
fn cparam(pt: &V, pname: &str, tnames: &HashSet<String>, cps: &HashSet<String>) -> Option<String> {
    let mut t = from_v(pt, tnames);
    while let Src::Ref(_, x) = t {
        t = *x;
    }
    match t {
        Src::Param(g) if cps.contains(&g) => Some(g),
        Src::Closure { .. } => Some(format!("fn:{}", pname)),
        _ => None,
    }
}

/// 単相化で束縛するクロージャの名前の列: 型引数 (宣言順)、続いて `fn(..)` の型の引数 (引数順)。
fn cnames(cd: &crate::program::Def, tnames: &HashSet<String>) -> Vec<String> {
    let cps = closure_params(cd);
    let mut v: Vec<String> = cd.generics.iter().filter(|g| g.0 == "type" && cps.contains(&g.1)).map(|g| g.1.clone()).collect();
    for (pn, pt) in &cd.params {
        if matches!(from_v(pt, tnames), Src::Closure { .. }) {
            v.push(format!("fn:{}", pn));
        }
    }
    v
}

/// 関数の項目の名前か (`fn_item_kind` が種類を返す形。副作用なしの判定)。
fn is_fn_item(lw: &Lw, e: &V) -> bool {
    if nname(e) != "Path" {
        return false;
    }
    let segs = strs(&nf(e)[0]);
    if segs.len() == 1 && lw.lookup(&segs[0]).is_some() {
        return false;
    }
    matches!(lw.p.resolve(&segs, &lw.def.file), Some(r) if r.rest.is_empty() && matches!(r.target, Target::Fn(di) if {
        let cd = &lw.p.defs[di];
        cd.kw == "fn" && cd.body.is_some() && !cd.is_method() && cd.generics.iter().all(|g| g.0 == "life")
    }))
}

/// `&x` / `&mut x` の `x` (クロージャを参照で渡す形)。それ以外はそのまま。
fn strip_addr(a: &V) -> &V {
    if nname(a) == "Unary" && matches!(st(&nf(a)[0]).as_str(), "&" | "&mut") {
        return &nf(a)[1];
    }
    a
}

/// 引数にクロージャ (式、またはクロージャを束縛した局所) があるか。型引数に渡すクロージャの単相化の判定に使う。
pub(super) fn has_closure_arg(lw: &Lw, args: &[V]) -> bool {
    args.iter().map(strip_addr).any(|a| nname(a) == "Closure" || is_fn_item(lw, a) || local_slot(lw, a).map_or(false, |s| matches!(lw.coll.kinds.get(&s), Some(Kind::Closure { .. }))))
}

/// 型引数に捕獲のないクロージャを取る関数の呼び出し。鍵は `関数名<型引数,クロージャの鍵,...>` で、
/// クロージャの引数は実行時の値を持たず (鍵で呼び先が決まる)、値の引数だけを渡す。
/// クロージャでない型引数 (`T`) は、引数の型から決めて `Lw::subst` で置き換えて下ろす。
fn generic_call(lw: &mut Lw, e: &V, di: usize) -> LR<Option<VId>> {
    let line = nline(e);
    let cd = &lw.p.defs[di];
    let f = nf(e);
    if cd.kw != "fn" || cd.body.is_none() || cd.is_method() || !matches!(f.get(2), Some(V::Nil) | None) {
        return lw.err(line, format!("呼び先 {} は段階 1 の対象外 (型引数の単相化は自由関数だけ)", cd.name));
    }
    generic_call_with(lw, e, di, None, items(&f[1]))
}

/// `generic_call` の本体。`recv` は、メソッドの受け手の値と型 (`self` の型と同じ形: `&mut [T]` など)。
pub(crate) fn generic_call_with(lw: &mut Lw, e: &V, di: usize, recv: Option<(VId, Src)>, argv: &[V]) -> LR<Option<VId>> {
    let line = nline(e);
    let p = lw.p;
    let cd = &p.defs[di];
    let tnames: HashSet<String> = cd.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.clone()).collect();
    let cps = closure_params(cd);
    let off = recv.is_some() as usize;
    if argv.len() + off != cd.params.len() {
        return lw.err(line, format!("呼び先 {} の引数の個数が違う", cd.name));
    }
    // クロージャでない型引数: 引数と戻り値の型から決める。
    let mut tb: std::collections::HashMap<String, Src> = std::collections::HashMap::new();
    for (k, (pn, pt)) in cd.params.iter().enumerate() {
        if cparam(pt, pn, &tnames, &cps).is_some() {
            continue;
        }
        let act = if k < off { recv.as_ref().map(|r| r.1.clone()) } else { lw.src_ty(&argv[k - off]).ok() };
        if let Some(a) = act {
            super::mono::unify(&from_v(pt, &tnames), &a, &mut tb);
        }
    }
    if let Ok(rt) = lw.src_ty(e) {
        super::mono::unify(&from_v(&cd.ret, &tnames), &rt, &mut tb);
    }
    let mut tkeys = vec![];
    for g in cd.generics.iter().filter(|g| g.0 == "type" && !cps.contains(&g.1)) {
        match tb.get(&g.1) {
            Some(t) if super::mono::concrete(t) => tkeys.push(t.show()),
            _ => return lw.err(line, format!("{} の型引数 {} が決まらない", cd.name, g.1)),
        }
    }
    let mut bound: BTreeMap<String, Kind> = BTreeMap::new();
    let mut vals = vec![];
    let mut ptys = vec![];
    for (k, (pn, pt)) in cd.params.iter().enumerate() {
        if let Some(g) = cparam(pt, pn, &tnames, &cps) {
            let a = strip_addr(&argv[k - off]);
            let Some(kd) = closure_arg_kind(lw, a)? else {
                return lw.err(line, "型引数に渡せるのは捕獲のないクロージャ (段階 1) だけ");
            };
            bound.insert(g, kd);
            continue;
        }
        let Some(pty) = sig_ty(lw, &super::mono::subst_ty(&from_v(pt, &tnames), &tb)) else {
            return lw.err(line, format!("呼び先 {} の引数の型は段階 1 の対象外", cd.name));
        };
        let v = if k < off {
            recv.as_ref().expect("off > 0").0
        } else {
            let Some(v) = lw.expr(&argv[k - off])? else {
                tmp_release(lw, &vals);
                return Ok(None);
            };
            tmp_hold(lw, v);
            v
        };
        vals.push(v);
        ptys.push(pty);
    }
    tmp_release(lw, &vals[off..]);
    let vals = lw.coerce_args(vals, &ptys);
    for (v, t) in vals.iter().zip(&ptys) {
        if lw.vty(*v) != *t {
            return lw.err(line, format!("呼び先 {} の引数の型が合わない", cd.name));
        }
    }
    let mut vals = vals;
    let mut keys = tkeys;
    for g in cnames(cd, &tnames) {
        match bound.get(&g) {
            Some(Kind::Closure { key, caps, .. }) => {
                keys.push(key.clone());
                // 取り込みの値は、値の引数の後ろに型引数の順で渡す
                vals.extend(caps.iter().copied());
            }
            _ => return lw.err(line, format!("型引数 {} が決まらない", g)),
        }
    }
    let ret = if is_nil(&cd.ret) {
        Some(Ty::Unit)
    } else {
        sig_ty(lw, &super::mono::subst_ty(&from_v(&cd.ret, &tnames), &tb))
    };
    let Some(ret) = ret else {
        return lw.err(line, format!("呼び先 {} の戻り値の型は段階 1 の対象外", cd.name));
    };
    let key = format!("{}<{}>", cd.name, keys.join(","));
    lw.callees.insert(key.clone());
    if !lw.coll.instances.contains(&key) {
        lw.coll.instances.insert(key.clone());
        let mut child = Lw::new(lw.p, lw.tys, lw.aggs, cd);
        child.subst = tb.clone();
        child.coll.instances = std::mem::take(&mut lw.coll.instances);
        let r = lower_instance(&mut child, cd, &key, &tnames, &cps, &bound);
        lw.coll.instances = std::mem::take(&mut child.coll.instances);
        let (f, callees) = r?;
        lw.coll.extra.push((f, callees));
        let nested = std::mem::take(&mut child.coll.extra);
        lw.coll.extra.extend(nested);
    }
    Ok(Some(lw.emit(ret, Op::Call { callee: key, args: vals })))
}

/// 関数の項目 (`t.loud` のような名前) の、クロージャとしての種類。取り込みはなく、鍵は関数の名前。
/// 局所変数・型引数付きの関数・メソッド・本体のない関数は対象外。呼び先と効果を記録する。
pub(crate) fn fn_item_kind(lw: &mut Lw, e: &V) -> Option<Kind> {
    if nname(e) != "Path" {
        return None;
    }
    let segs = strs(&nf(e)[0]);
    if segs.len() == 1 && lw.lookup(&segs[0]).is_some() {
        return None;
    }
    let r = lw.p.resolve(&segs, &lw.def.file)?;
    if !r.rest.is_empty() {
        return None;
    }
    let Target::Fn(di) = r.target else { return None };
    let cd = &lw.p.defs[di];
    if cd.kw != "fn" || cd.body.is_none() || cd.is_method() || !cd.generics.iter().all(|g| g.0 == "life") {
        return None;
    }
    let gn = HashSet::new();
    let args: Option<Vec<Ty>> = cd.params.iter().map(|(_, t)| sig_ty(lw, &from_v(t, &gn))).collect();
    let ret = if is_nil(&cd.ret) { Some(Ty::Unit) } else { sig_ty(lw, &from_v(&cd.ret, &gn)) };
    let (args, ret) = (args?, ret?);
    let name = cd.name.clone();
    KEY_EFF.with(|k| k.borrow_mut().insert(name.clone(), (cd.uses.clone(), cd.panics.as_deref() != Some("never"))));
    lw.callees.insert(name.clone());
    Some(Kind::Closure { key: name, args, ret, caps: vec![], cap_tys: vec![] })
}

/// 型引数に渡すクロージャの種類 (クロージャの式、または種類の分かる局所変数)。
pub(crate) fn closure_arg_kind(lw: &mut Lw, a: &V) -> LR<Option<Kind>> {
    if let Some(k) = fn_item_kind(lw, a) {
        return Ok(Some(k));
    }
    if nname(a) == "Closure" {
        if lw.expr(a)?.is_none() {
            return Ok(None);
        }
        return Ok(closure_kind_from_table(lw, a));
    }
    Ok(local_slot(lw, a).and_then(|s| lw.coll.kinds.get(&s).cloned()).filter(|k| matches!(k, Kind::Closure { .. })))
}

/// 単相インスタンス (`cd` を `bound` のクロージャで束縛したもの) を下ろす。
/// 入口の引数のうち型引数の型のものは値を持たず、その型の種類 (クロージャ) で呼び先が決まる。
fn lower_instance(
    child: &mut Lw,
    cd: &crate::program::Def,
    key: &str,
    tnames: &HashSet<String>,
    cps: &HashSet<String>,
    bound: &BTreeMap<String, Kind>,
) -> LR<(Func, BTreeSet<String>)> {
    child.coll.prefix = key.to_string();
    let sub = super::mono::subst_ty;
    let mut value_params = vec![];
    for (pn, pt) in &cd.params {
        if cparam(pt, pn, tnames, cps).is_some() {
            continue;
        }
        match sig_ty(child, &sub(&from_v(pt, tnames), &child.subst)) {
            Some(x) => value_params.push(x),
            None => return child.err(0, format!("呼び先 {} の引数の型は段階 1 の対象外", cd.name)),
        }
    }
    let nvalue = value_params.len();
    for g in cnames(cd, tnames) {
        if let Some(Kind::Closure { cap_tys, .. }) = bound.get(&g) {
            value_params.extend(cap_tys.iter().copied());
        }
    }
    let (_, pv) = child.new_block(&value_params);
    // 型引数ごとの取り込みの引数の位置 (値の引数の後ろ、型引数の順)
    let mut cap_at: BTreeMap<String, Vec<VId>> = BTreeMap::new();
    let mut off = nvalue;
    for g in cnames(cd, tnames) {
        if let Some(Kind::Closure { cap_tys, .. }) = bound.get(&g) {
            cap_at.insert(g.clone(), pv[off..off + cap_tys.len()].to_vec());
            off += cap_tys.len();
        }
    }
    child.cur = Some(0);
    child.scopes.push(vec![]);
    let mut vi = 0;
    for (pname, pt) in &cd.params {
        if let Some(g) = cparam(pt, pname, tnames, cps) {
            let Some(mut k) = bound.get(&g).cloned() else {
                return child.err(0, format!("型引数 {} が決まらない", g));
            };
            if let (Kind::Closure { caps, .. }, Some(ps)) = (&mut k, cap_at.get(&g)) {
                *caps = ps.clone();
            }
            let tok = child.emit(Ty::Unit, Op::Const(0));
            child.bind(pname, tok, Ty::Unit);
            let s = child.lookup(pname).expect("直前に束縛した名前");
            child.coll.kinds.insert(s, k);
        } else {
            let src = sub(&from_v(pt, tnames), &child.subst);
            let t = sig_ty(child, &src).expect("上で検査済み");
            bind_param(child, pname, pv[vi], t, &src);
            vi += 1;
        }
    }
    child.ret = if is_nil(&cd.ret) { Some(Ty::Unit) } else { sig_ty(child, &sub(&from_v(&cd.ret, tnames), &child.subst)) }
        .ok_or_else(|| format!("呼び先 {} の戻り値の型は段階 1 の対象外", cd.name))?;
    let body = cd.body.as_ref().expect("候補は本体を持つ");
    child.rf.begin(body);
    if let Some(v) = child.expr(body)? {
        if child.vty(v) != child.ret {
            return child.err(nline(body), "本体の値の型が戻り値の型と合わない");
        }
        drop_from(child, 0);
        child.terminate(Term::Ret(v));
    }
    // 効果の型引数 (`uses [E]` / `panics P`) は、束縛されたクロージャ・関数の効果の和に束縛される。
    let mut uses: Vec<String> = cd.uses.iter().filter(|u| !cd.generics.iter().any(|g| g.1 == **u)).cloned().collect();
    let mut eff_may = false;
    let mut has_eff_var = false;
    if cd.uses.iter().any(|u| cd.generics.iter().any(|g| g.1 == *u)) || cd.panics.as_deref().is_some_and(|p| cd.generics.iter().any(|g| g.1 == p)) {
        has_eff_var = true;
        for k in bound.values() {
            if let Kind::Closure { key, .. } = k {
                if let Some((u, m)) = key_eff(key) {
                    uses.extend(u);
                    eff_may |= m;
                } else {
                    eff_may = true;
                }
            }
        }
    }
    uses.sort();
    uses.dedup();
    let may_panic = match cd.panics.as_deref() {
        Some("never") => false,
        Some(p) if has_eff_var && cd.generics.iter().any(|g| g.1 == p) => eff_may,
        _ => true,
    };
    // この中のクロージャ (効果の型引数の宣言の下で作ったもの) は、インスタンスと同じ効果を持つ。
    if has_eff_var {
        for (f, _) in child.coll.extra.iter_mut() {
            if !f.key.starts_with(key) {
                continue;
            }
            f.may_panic = may_panic;
            f.uses = uses.clone();
            KEY_EFF.with(|k| k.borrow_mut().insert(f.key.clone(), (uses.clone(), may_panic)));
        }
    }
    child.flush_allocas();
    let mut blocks: Vec<Block> = std::mem::take(&mut child.blocks)
        .into_iter()
        .map(|b| Block { params: b.params, insts: b.insts, term: b.term.unwrap_or(Term::Unreachable) })
        .collect();
    finish(&mut blocks, std::mem::take(&mut child.coll.allocas));
    let func = Func {
        fqn: cd.name.clone(),
        key: key.to_string(),
        uses,
        may_panic,
        stack: cd.stack,
        ret: child.ret,
        link: None,
        blocks,
    };
    Ok((func, std::mem::take(&mut child.callees)))
}

fn call_closure(lw: &mut Lw, e: &V, s: usize, tupled: bool) -> LR<Option<VId>> {
    let line = nline(e);
    let Some(Kind::Closure { key, args, ret, caps, .. }) = lw.coll.kinds.get(&s).cloned() else {
        return lw.err(line, "クロージャの呼び出しの内部不整合");
    };
    let mut argv: Vec<&V> = items(&nf(e)[1]).iter().collect();
    if tupled {
        // `f.call(( a, b ))`: 引数は 1 つのタプル。`()` は引数なし、括弧だけの 1 引数 `(a)` は a そのもの。
        if argv.len() != 1 {
            return lw.err(line, "call の引数はタプル 1 つのはず");
        }
        let a = argv[0];
        argv = match nname(a) {
            "TupleE" => items(&nf(a)[0]).iter().collect(),
            _ if args.is_empty() => vec![],
            _ => vec![a],
        };
    }
    if argv.len() != args.len() {
        return lw.err(line, "クロージャの引数の個数が違う");
    }
    let mut vs = vec![];
    for a in argv {
        match lw.expr(a)? {
            Some(v) => vs.push(v),
            None => return Ok(None),
        }
    }
    // `Some(c)` の値 (`&スカラー` の値表現) を `&T` の引数に渡す形などは、呼び出しと同じに調整する。
    let vs = lw.coerce_args(vs, &args);
    for (v, t) in vs.iter().zip(args.iter()) {
        if lw.vty(*v) != *t {
            return lw.err(line, "クロージャの引数の型が合わない");
        }
    }
    lw.callees.insert(key.clone());
    let all: Vec<VId> = caps.into_iter().chain(vs).collect();
    Ok(Some(lw.emit(ret, Op::Call { callee: key, args: all })))
}

/// 受け手の値 (ヘッダのアドレス)。局所変数は移さずにそのまま借りる。
fn place(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    if let Some(s) = local_slot(lw, e) {
        return Ok(Some(lw.slots[s].0));
    }
    lw.expr(e)
}

/// `e.m(args)` の形 (`MCall`)。受け手がコレクション系のときだけ扱い、それ以外は `None` (既定の経路へ)。
pub(crate) fn try_mcall(lw: &mut Lw, e: &V) -> Option<LR<Option<VId>>> {
    let f = nf(e);
    // `x.f.v.m(..)` (構造体のフィールドの Vec / String): ヘッダのアドレスをたどって扱う (設計_KIR.md 13.7)。
    if let Some((s, names)) = super::own::chain_of(lw, &f[0]) {
        if let Some(r) = super::own::field_call(lw, e, s, &names, &st(&f[1]), items(&f[2])) {
            return Some(r);
        }
    }
    let kind = kind_of_expr(lw, &f[0])?;
    let line = nline(e);
    if !matches!(f.get(3), Some(V::Nil) | None) {
        return Some(lw.err(line, "型引数付きのメソッドは段階 1 の対象外"));
    }
    let m = st(&f[1]);
    let slot = local_slot(lw, &f[0]);
    // 局所の受け手の読みは、move 済みの値の読み (注釈。値の受け手の move は下の呼び出しが出す)。
    if let (Some(s), Some(n)) = (slot, super::path1(&f[0])) {
        lw.own_emit(&n, s, crate::kir::OwnKind::Use);
    }
    Some(match place(lw, &f[0]) {
        // 一時の `String` / `Vec` (`f().unwrap().as_str()` など) の読むだけのメソッド: 値を所有のヘッダ (フレーム) に
        // 置いて、そのアドレスを受け手にする。ヘッダはスコープの終わりに解放する (設計_KIR.md 23 章)。
        Ok(Some(h))
            if slot.is_none()
                && matches!((&kind, m.as_str()), (Kind::String, "as_str" | "as_bytes" | "len" | "is_empty" | "contains" | "starts_with" | "ends_with") | (Kind::Vec(_), "len" | "is_empty" | "as_slice"))
                && lw.aggs.borrow().is_vec(lw.vty(h)) =>
        {
            let hdr = adopt(lw, h);
            lw.coll.temps.push(hdr);
            method_on(lw, e, line, None, kind, hdr, &m, items(&f[2]))
        }
        Ok(Some(h)) => method_on(lw, e, line, slot, kind, h, &m, items(&f[2])),
        Ok(None) => Ok(None),
        Err(e) => Err(e),
    })
}

/// ヘッダ (または `fat`) `h` を受け手とするメソッド呼び出し。所有の周りの新しいメソッド (`own`) を先に試す。
pub(super) fn method_on(lw: &mut Lw, e: &V, line: usize, slot: Option<usize>, kind: Kind, h: VId, m: &str, args: &[V]) -> LR<Option<VId>> {
    // `a.cmp(&b)` / `a.partial_cmp(&b)` (`&str` / `String`、仕様 8.3 の `Ord`)。
    if matches!(m, "cmp" | "partial_cmp") && args.len() == 1 && matches!(kind, Kind::Str | Kind::String) {
        let recv = match (kind.clone(), lw.vty(h)) {
            (Kind::Str, _) => load_fat_ref(lw, h),
            (_, Ty::Ref(_, Pointee::Agg(_))) => lw.emit(Ty::Ptr, Op::Bitcast { src: lw.vty(h), a: h }),
            _ => h,
        };
        return lw.string_cmp_method(e, &kind, recv, m, args);
    }
    // `a.eq(b)` (`&str` / `String` の受け手。仕様 4.2 の `PartialEq`)。`&&str` の値は読んでから比べる。
    if m == "eq" && args.len() == 1 && matches!(kind, Kind::Str | Kind::String) {
        let a = if kind == Kind::Str { load_fat_ref(lw, h) } else { hdr_view(lw, h) };
        let Some(b0) = lw.expr_peek(&args[0])? else { return Ok(None) };
        let b = if kind == Kind::Str { load_fat_ref(lw, b0) } else { hdr_view(lw, b0) };
        return Ok(Some(lw.str_eq(a, b)?));
    }
    // `x.hash(&mut state)` (`&str` / `String`、仕様 4.2): バイト列を `Fnv64.write_bytes` へ渡す。受け手が `&&str` なら値を読む。
    if m == "hash" && args.len() == 1 && matches!(kind, Kind::Str | Kind::String) {
        let bytes = if kind == Kind::Str { load_fat_ref(lw, h) } else { hdr_view(lw, h) };
        let Some(state) = lw.expr_peek(&args[0])? else { return Ok(None) };
        let callee = "core.hash.Fnv64.write_bytes".to_string();
        lw.callees.insert(callee.clone());
        return Ok(Some(lw.emit(Ty::Unit, Op::Call { callee, args: vec![state, bytes] })));
    }
    // `Vec` の基本メソッドを、`alloc/vec.ksnl` の Kasane 版 (`Buf` の上) の単相インスタンスで下ろす。
    // 中身 (`alloc.vec.Vec.*` の本体) は組み込みの下ろしを使う (同じヘッダ)。
    // 要素型が局所の初期化から決まらなかった Vec (別の場所の `push` で決まる) は、`push` / `insert` の引数の型で決める。
    let kind = match (&kind, m, args.last()) {
        (Kind::Vec(None), "push" | "insert", Some(a)) => match lw.ty_of(a) {
            Ok(t) if elem_size(lw, t).is_some() => {
                if let Some(s) = slot {
                    lw.coll.kinds.insert(s, Kind::Vec(Some(t)));
                }
                Kind::Vec(Some(t))
            }
            _ => kind,
        },
        _ => kind,
    };
    // `String` の基本メソッドを、`alloc/string.ksnl` の Kasane 版 (`Buf<u8>` の上) の単相インスタンスで下ろす。
    if matches!(kind, Kind::String) {
        if let Some(r) = super::ext::string_call(lw, e, line, h, m, args) {
            return r;
        }
    }
    if matches!(kind, Kind::Vec(Some(_))) && !in_vec_base(lw) && !in_string_base(lw) && crate::interp::KASANE_VEC_BASE.contains(&m) {
        if let Some(r) = super::ext::ext_call(lw, e, line, &kind, h, m, args, true) {
            return r;
        }
    }
    if let Some(r) = lw.str_method_call(e, &kind, h, m, args) {
        return r;
    }
    if let Some(r) = super::buf::method(lw, e, line, &kind, h, m, args) {
        return r;
    }
    if let Some(r) = super::own::method(lw, e, line, &kind, h, m, args) {
        return r;
    }
    if matches!(m, "cmp" | "partial_cmp") && matches!(kind, Kind::String | Kind::Str) {
        return lw.string_cmp_method(e, &kind, h, m, args);
    }
    // `push_display` / `push_debug` は組み込みの整形のまま (`alloc.fmt` の規範と `Formatter` の扱いを待つ)。
    if (m == "push_display" || m == "push_debug") && matches!(kind, Kind::String) {
        return lw.string_push_fmt(e, line, h, args, m == "push_debug");
    }
    if m == "get" {
        return get_elem(lw, line, &kind, h, args);
    }
    if (m == "first" || m == "last") && matches!(kind, Kind::Vec(_) | Kind::Slice(_)) {
        return first_last(lw, line, &kind, h, m, args.len());
    }
    if m == "contains" && matches!(kind, Kind::Vec(_) | Kind::Slice(_)) && args.len() == 1 {
        return contains_scalar(lw, line, &kind, h, args);
    }
    if m == "sub" || m == "sub_mut" {
        return slice_sub(lw, line, &kind, h, args);
    }
    if let Some(r) = super::ext::ext_call(lw, e, line, &kind, h, m, args, false) {
        return r;
    }
    coll_method(lw, line, kind, h, m, args.len())
}

/// 局所変数の受け手 `x.m(args)`。パーサは `x.m(..)` を `Path [x, m]` の呼び出しとして作るので、こちらで扱う。
fn try_local_method(lw: &mut Lw, e: &V, s: usize, kind: Kind, m: &str) -> LR<Option<VId>> {
    let f = nf(e);
    let line = nline(e);
    if !matches!(f.get(2), Some(V::Nil) | None) {
        return lw.err(line, "型引数付きのメソッドは段階 1 の対象外");
    }
    // アドレスを取られた配列 (`&mut chunk` の `[T; N]`) は、スロットが fat のアドレス (mem) なので、値を読んで fat にする。
    let hdr = if matches!(kind, Kind::Slice(_)) && lw.is_mem_val(lw.slots[s].0) { lw.read_slot(s) } else { lw.slots[s].0 };
    // `&[T; N]` / `&mut [T; N]` のパラメータの `len()`: 長さ N は型から分かる。
    if m == "len" && matches!(kind, Kind::Slice(_)) && lw.slots[s].1 == Ty::Ptr {
        let n = arr_len_of(lw, s).expect("&[T; N] のパラメータに arr_len が入っている");
        return Ok(Some(lw.emit(Ty::Usize, Op::Const(n as i128))));
    }
    method_on(lw, e, line, Some(s), kind, hdr, m, items(&f[1]))
}

pub(super) fn coll_method(lw: &mut Lw, line: usize, kind: Kind, h: VId, m: &str, nargs: usize) -> LR<Option<VId>> {
    if matches!(kind, Kind::Closure { .. }) {
        return lw.err(line, format!("メソッド {} は段階 1 の対象外", m));
    }
    if !matches!(m, "len" | "is_empty" | "as_bytes" | "as_str" | "as_slice" | "as_mut_slice") {
        return lw.err(line, format!("メソッド {} は段階 1 の対象外 (戻り値が Option / Result / 参照のため)", m));
    }
    if nargs != 0 {
        return lw.err(line, format!("メソッド {} の引数は段階 1 の対象外", m));
    }
    // `&str` とスライスは `fat` の値、Vec / String はヘッダのアドレス。
    let fat = matches!(kind, Kind::Str | Kind::Slice(_));
    match m {
        "len" => Ok(Some(if fat { fat_len(lw, h) } else { load_len(lw, h) })),
        "is_empty" => {
            let len = if fat { fat_len(lw, h) } else { load_len(lw, h) };
            let zero = lw.emit(Ty::Usize, Op::Const(0));
            Ok(Some(lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: len, b: zero })))
        }
        // `str` のバイト列: 同じ {ptr, len} の値。
        "as_bytes" if kind == Kind::Str => Ok(Some(h)),
        // `String.as_bytes()`: ヘッダの {ptr, len} を借用の `&[u8]` (fat) にする (所有は移さない)。
        "as_bytes" if kind == Kind::String => Ok(Some(hdr_view(lw, h))),
        // `String.as_str()`: ヘッダの {ptr, len} を借用の `fat` にする (検査を省く O(1))。`String` のバイト列は UTF-8 を保つ
        // `String` のメソッドだけが書く (非公開のフィールド、8.3)。`Buf<u8>.as_str` は検査つき (`alloc/raw.sig`)。
        "as_str" if kind == Kind::String => Ok(Some(hdr_view(lw, h))),
        "as_slice" | "as_mut_slice" if matches!(kind, Kind::Vec(_)) => Ok(Some(hdr_view(lw, h))),
        // `&str.as_str()` はそのまま。
        "as_str" if kind == Kind::Str => Ok(Some(h)),
        other => lw.err(line, format!("メソッド {} は段階 1 の対象外", other)),
    }
}


/// 呼べるもの (捕獲のないクロージャの式、クロージャの局所変数、関数の項目) の、呼び先の鍵と引数・戻り値の型。
/// 呼び先は `callees` に入れる。`Result::map_err` の引数に使う。
pub(crate) fn callable(lw: &mut Lw, f: &V) -> LR<(String, Vec<Ty>, Ty)> {
    let line = nline(f);
    if nname(f) == "Closure" {
        if lw.expr(f)?.is_none() {
            return lw.err(line, "クロージャが作れない");
        }
        let Some(Kind::Closure { key, args, ret, caps, .. }) = closure_kind_from_table(lw, f) else {
            return lw.err(line, "クロージャの型が決まらない");
        };
        if !caps.is_empty() {
            return lw.err(line, "取り込みのあるクロージャをここに渡すのは段階 1 の対象外");
        }
        lw.callees.insert(key.clone());
        return Ok((key, args, ret));
    }
    if let Some(s) = local_slot(lw, f) {
        if let Some(Kind::Closure { key, args, ret, caps, .. }) = lw.coll.kinds.get(&s).cloned() {
            if !caps.is_empty() {
                return lw.err(line, "取り込みのあるクロージャをここに渡すのは段階 1 の対象外");
            }
            lw.callees.insert(key.clone());
            return Ok((key, args, ret));
        }
    }
    if nname(f) == "Path" {
        let segs = strs(&nf(f)[0]);
        if let Some(Target::Fn(di)) = lw.p.resolve(&segs, &lw.def.file).map(|r| r.target) {
            let cd = &lw.p.defs[di];
            if cd.kw == "fn" && cd.body.is_some() && !cd.is_method() && cd.generics.iter().all(|g| g.0 == "life") {
                let gn = HashSet::new();
                let args: Option<Vec<Ty>> = cd.params.iter().map(|(_, t)| sig_ty(lw, &from_v(t, &gn))).collect();
                let ret = if is_nil(&cd.ret) { Some(Ty::Unit) } else { sig_ty(lw, &from_v(&cd.ret, &gn)) };
                if let (Some(args), Some(ret)) = (args, ret) {
                    let name = cd.name.clone();
                    lw.callees.insert(name.clone());
                    return Ok((name, args, ret));
                }
                return lw.err(line, format!("関数 {} の型は段階 1 の対象外", segs.join(".")));
            }
        }
    }
    lw.err(line, "呼べるものは捕獲のないクロージャか関数の項目だけ段階 1 の対象")
}

/// `s.sub(a, b)` (要素がスカラーのスライス): `a <= b <= len` なら `Some(s[a..b])`、外なら `None` (panic しない。木走査の `sub` と同じ)。
/// 結果は `Option<&[T]>` で、部分スライスの値 `fat` (`{ptr, len}`) をペイロードに持つ (設計_KIR.md 11.6)。
pub(super) fn slice_sub(lw: &mut Lw, line: usize, kind: &Kind, h: VId, args: &[V]) -> LR<Option<VId>> {
    let Kind::Slice(Some(elem)) = kind else {
        return lw.err(line, "sub の対象は要素型の分かるスライスだけ段階 1 の対象");
    };
    let elem = *elem;
    let Some(sz) = size_of(elem) else {
        return lw.err(line, format!("sub の要素の型 {} は段階 1 の対象外", elem.name()));
    };
    if args.len() != 2 {
        return lw.err(line, "sub の引数は 2 つのはず");
    }
    let Some(a) = lw.expr(&args[0])? else { return Ok(None) };
    let Some(b) = lw.expr(&args[1])? else { return Ok(None) };
    if lw.vty(a) != Ty::Usize || lw.vty(b) != Ty::Usize {
        return lw.err(line, "sub の引数が usize ではない");
    }
    let fat = lw.fat_ty();
    let Some(fp) = Pay::of(fat) else {
        return lw.err(line, "fat の型が Option のペイロードにならない");
    };
    let opt = Ty::Opt(fp);
    let (len, base) = Seq::Fat(h).len_and_base(lw, None);
    let le_ab = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::Usize, a, b: b });
    let le_bl = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::Usize, a: b, b: len });
    let ok = lw.emit(Ty::Bool, Op::Bool(BoolOp::And, le_ab, le_bl));
    let (tb, _) = lw.new_block(&[]);
    let (eb, _) = lw.new_block(&[]);
    let (jb, jp) = lw.new_block(&[opt]);
    lw.branch(ok, (tb, vec![]), (eb, vec![]));
    lw.cur = Some(tb);
    let off = if sz == 1 {
        a
    } else {
        let k = lw.emit(Ty::Usize, Op::Const(sz as i128));
        lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a, b: k })
    };
    let addr = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off });
    let n = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: b, b: a });
    let view = lw.emit(fat, Op::Make { variant: None, fields: vec![addr, n] });
    let some = lw.emit(opt, Op::Some(view));
    lw.goto(jb, vec![some]);
    lw.cur = Some(eb);
    let none = lw.emit(opt, Op::Const(0));
    lw.goto(jb, vec![none]);
    lw.cur = Some(jb);
    Ok(Some(jp[0]))
}

/// `v.get(i)` / `s.get(i)` (要素はスカラー): `Option<要素>`。境界の内なら `Some(要素)`、外なら `None` (panic しない)。
/// 木走査の `Option<&T>` はスカラーでは読み取りだけなので、値で持つ (設計_KIR.md 10.10)。
pub(super) fn get_elem(lw: &mut Lw, line: usize, kind: &Kind, h: VId, args: &[V]) -> LR<Option<VId>> {
    if args.len() != 1 {
        return lw.err(line, "get の引数の個数が違う");
    }
    pick_elem(lw, line, kind, h, Pick::At(&args[0]), "get")
}

/// `v.first()` / `v.last()` (要素の型は `get` と同じ): `Option<&T>` と同じ形で `Option<要素>` を作る。空なら `None`。
pub(super) fn first_last(lw: &mut Lw, line: usize, kind: &Kind, h: VId, m: &str, nargs: usize) -> LR<Option<VId>> {
    if nargs != 0 {
        return lw.err(line, format!("メソッド {} の引数は段階 1 の対象外", m));
    }
    let pick = if m == "first" { Pick::First } else { Pick::Last };
    pick_elem(lw, line, kind, h, pick, m)
}

/// `v.contains(&x)` (要素はスカラー): 先頭から順に `==` を見て、見つかれば `true`。木走査 (`natives.rs` の `contains`) と同じ順。
/// 引数は `&T` の参照 (`&3u8`) か、値 (`T`) のどちらでもよい。要素が所有物・集成体の形は理由付きで拒む。
pub(super) fn contains_scalar(lw: &mut Lw, line: usize, kind: &Kind, h: VId, args: &[V]) -> LR<Option<VId>> {
    let (seq, elem) = match kind {
        Kind::Vec(Some(e)) => (Seq::Hdr(h), *e),
        Kind::Slice(Some(e)) => (Seq::Fat(h), *e),
        _ => return lw.err(line, "contains の対象は要素型の分かる Vec・スライスだけ段階 1 の対象"),
    };
    if owns_heap(lw, elem) || matches!(elem, Ty::Agg(_)) {
        return lw.err(line, format!("contains の要素の型 {} は段階 1 の対象外 (スカラーだけ)", elem.name()));
    }
    let Some(sz) = elem_size(lw, elem) else {
        return lw.err(line, format!("contains の要素の型 {} は段階 1 の対象外", elem.name()));
    };
    let Some(a0) = lw.expr(&args[0])? else { return Ok(None) };
    let x = match lw.vty(a0) {
        Ty::Ref(_, p) if p.ty() == elem => lw.emit(elem, Op::Load(a0)),
        t if t == elem => a0,
        _ => return lw.err(line, "contains の引数の型が要素の型と違う"),
    };
    let (len, base) = seq.len_and_base(lw, None);
    let zero = lw.emit(Ty::Usize, Op::Const(0));
    // 先頭: (i) を持つ。`i < len` なら要素を見る。見つかれば結果 `true`、外れたら `i+1` へ。
    let (hb, hp) = lw.new_block(&[Ty::Usize]);
    let i = hp[0];
    let (bb, _) = lw.new_block(&[]);
    let (jb, jp) = lw.new_block(&[Ty::Bool]);
    lw.goto(hb, vec![zero]);
    lw.cur = Some(hb);
    let inside = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: i, b: len });
    // 範囲外は `inside` が偽なので `jb` へ (値は偽)。
    lw.branch(inside, (bb, vec![]), (jb, vec![inside]));
    lw.cur = Some(bb);
    let off = if sz == 1 {
        i
    } else {
        let k = lw.emit(Ty::Usize, Op::Const(sz as i128));
        lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: i, b: k })
    };
    let addr = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off });
    let e = load_elem(lw, addr, elem);
    let hit = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: elem, a: e, b: x });
    let one = lw.emit(Ty::Usize, Op::Const(1));
    let next = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: i, b: one });
    // 見つかった: `hit` (真) を結果にして抜ける。外れ: 次の添字へ。
    lw.branch(hit, (jb, vec![hit]), (hb, vec![next]));
    lw.cur = Some(jb);
    Ok(Some(jp[0]))
}

/// 添字の決め方。`At` は利用者の式 (usize)、`First` / `Last` は 0 と `len - 1` (空なら `None`)。
#[derive(Clone, Copy)]
pub(super) enum Pick<'a> {
    At(&'a V),
    First,
    Last,
}

fn pick_elem(lw: &mut Lw, line: usize, kind: &Kind, h: VId, pick: Pick, m: &str) -> LR<Option<VId>> {
    let (seq, elem) = match kind {
        Kind::Vec(Some(e)) => (Seq::Hdr(h), *e),
        Kind::Slice(Some(e)) => (Seq::Fat(h), *e),
        _ => return lw.err(line, format!("{} の対象は要素型の分かる Vec・スライスだけ段階 1 の対象", m)),
    };
    // 集成体の要素 (所有物を持つか否か) は、要素の番地を `bref` に包んで持つ (写さない。17.18)。
    // 写すと、`&T` の欄 (`(&K, &V)` など) の束縛が値になり、借用の型と合わなくなる。
    let borrowed = owns_heap(lw, elem) || matches!(elem, Ty::Agg(_));
    let pay = if borrowed {
        // 所有物を持つ要素: 要素の番地を `bref` に包んで持つ (写さない。17.18)
        let b = lw.aggs.borrow_mut().bref();
        Pay::of(b)
    } else {
        Pay::of(elem)
    };
    let (Some(sz), Some(pay)) = (elem_size(lw, elem), pay) else {
        return lw.err(line, format!("{} の要素の型 {} は段階 1 の対象外", m, elem.name()));
    };
    let idx = match pick {
        Pick::At(a) => {
            let Some(i) = lw.expr(a)? else { return Ok(None) };
            if lw.vty(i) != Ty::Usize {
                return lw.err(line, format!("{} の添字が usize ではない", m));
            }
            Some(i)
        }
        Pick::First | Pick::Last => None,
    };
    let opt = Ty::Opt(pay);
    let (len, base) = seq.len_and_base(lw, None);
    // 境界の内か (`i < len`)、添字 (`i`)。first / last は空でなければ内で、添字は 0 か len-1。
    let (inside, i) = match (pick, idx) {
        (Pick::At(_), Some(i)) => (lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: i, b: len }), i),
        (Pick::First, _) => {
            let zero = lw.emit(Ty::Usize, Op::Const(0));
            let nonempty = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: zero, b: len });
            (nonempty, zero)
        }
        _ => {
            let zero = lw.emit(Ty::Usize, Op::Const(0));
            let nonempty = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: zero, b: len });
            let one = lw.emit(Ty::Usize, Op::Const(1));
            // 空のときは使われない (wrap の減算は trap しない)
            let last = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: len, b: one });
            (nonempty, last)
        }
    };
    let (tb, _) = lw.new_block(&[]);
    let (eb, _) = lw.new_block(&[]);
    let (jb, jp) = lw.new_block(&[opt]);
    lw.branch(inside, (tb, vec![]), (eb, vec![]));
    lw.cur = Some(tb);
    let off = if sz == 1 {
        i
    } else {
        let k = lw.emit(Ty::Usize, Op::Const(sz as i128));
        lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: i, b: k })
    };
    let addr = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off });
    let x = if borrowed { lw.emit(pay.ty(), Op::Make { variant: None, fields: vec![addr] }) } else { load_elem(lw, addr, elem) };
    // スカラーの要素は値で持つ。`get` / `first` / `last` は木走査で `Option<&T>` なので、読んだ値は要素の借用を運ぶ (注釈。設計_KIR.md 24.11)。
    if !borrowed {
        // 借用の出どころは、要素の番地ではなく所有者の場所 (Vec のヘッダ・スライスの元): 後の `push` などの排他と照らすため。
        let src = match seq {
            Seq::Hdr(hh) if matches!(lw.vty(hh), Ty::Ptr | Ty::Ref(..)) => hh,
            _ => addr,
        };
        lw.emit(Ty::Unit, Op::Carry { a: src, b: x });
    }
    let some = lw.emit(opt, Op::Some(x));
    lw.goto(jb, vec![some]);
    lw.cur = Some(eb);
    let none = lw.emit(opt, Op::Const(0));
    lw.goto(jb, vec![none]);
    lw.cur = Some(jb);
    Ok(Some(jp[0]))
}

/// Vec / String のヘッダ `hdr` の中身を指す借用の `fat` ({ptr, len})。
pub(super) fn hdr_view(lw: &mut Lw, hdr: VId) -> VId {
    // 呼び出しの結果などヘッダが集成体の値のときは、メモリから読まずにフィールドを取る。
    let by_val = matches!(lw.vty(hdr), Ty::Agg(_));
    let hp = match lw.vty(hdr) {
        // 借用の番地 (`ref<集成体>`) はヘッダのポインタとして読む。
        Ty::Ref(_, Pointee::Agg(_)) => lw.emit(Ty::Ptr, Op::Bitcast { src: lw.vty(hdr), a: hdr }),
        _ => hdr,
    };
    let p = if by_val { lw.emit(Ty::Ptr, Op::Field { a: hdr, off: 0 }) } else { lw.emit(Ty::Ptr, Op::MemLoad { p: hp, off: 0 }) };
    let l = load_len(lw, if by_val { hdr } else { hp });
    let fat = lw.fat_ty();
    lw.emit(fat, Op::Make { variant: None, fields: vec![p, l] })
}

/// `fat` の長さ。
fn fat_len(lw: &mut Lw, v: VId) -> VId {
    lw.emit(Ty::Usize, Op::Field { a: v, off: 8 })
}

/// 添字・反復の対象の置き場: Vec はヘッダのアドレス、スライスは `fat` の値、
/// 固定長配列への参照 (`&[T; N]` / `&mut [T; N]`) は生ポインタ (長さ N は型から分かる)。
#[derive(Clone, Copy)]
pub(super) enum Seq {
    Hdr(VId),
    Fat(VId),
    Ptr(VId, u32),
}

impl Seq {
    /// (長さ, 先頭のアドレス)。
    pub(super) fn len_and_base(self, lw: &mut Lw, check: Option<VId>) -> (VId, VId) {
        match self {
            Seq::Hdr(h) => {
                let len = load_len(lw, h);
                if let Some(i) = check {
                    lw.emit(Ty::Unit, Op::IndexCheck { i, len });
                }
                let base = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
                (len, base)
            }
            Seq::Fat(v) => {
                let len = fat_len(lw, v);
                if let Some(i) = check {
                    lw.emit(Ty::Unit, Op::IndexCheck { i, len });
                }
                let base = lw.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
                (len, base)
            }
            Seq::Ptr(p, n) => {
                let len = lw.emit(Ty::Usize, Op::Const(n as i128));
                if let Some(i) = check {
                    lw.emit(Ty::Unit, Op::IndexCheck { i, len });
                }
                (len, p)
            }
        }
    }
}

/// 添字の値の番地: 境界検査のあと `ptr + i * size`。
fn elem_addr(lw: &mut Lw, seq: Seq, i: VId, elem: Ty) -> LR<VId> {
    let Some(sz) = elem_size(lw, elem) else {
        return lw.err(0, format!("要素の型 {} は段階 1 の対象外", elem.name()));
    };
    let (_, base) = seq.len_and_base(lw, Some(i));
    let off = if sz == 1 {
        i
    } else {
        let k = lw.emit(Ty::Usize, Op::Const(sz as i128));
        lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: i, b: k })
    };
    Ok(lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off }))
}

/// 添字・反復の対象になる Vec・スライスの変数または `b"..."` リテラル: (置き場, 要素の型)。要素型が分からなければ拒否する。
fn vec_place(lw: &mut Lw, e: &V, line: usize) -> LR<(Seq, Ty)> {
    if let Some(s) = local_slot(lw, e) {
        // `lw.coll` と `lw.slots` から必要な値を先にコピーし、その後の `lw.emit` との借用競合を避ける。
        let kind = lw.coll.kinds.get(&s).cloned();
        let slot_ptr = lw.slots[s].0;
        match kind {
            Some(Kind::Vec(Some(elem))) => return Ok((Seq::Hdr(slot_ptr), elem)),
            // `&[T; N]` / `&mut [T; N]` のパラメータ: ptr の値、長さ N は型から分かる。
            Some(Kind::Slice(Some(elem))) if lw.vty(slot_ptr) == Ty::Ptr => {
                let n = lw.coll.arr_len[&s];
                return Ok((Seq::Ptr(slot_ptr, n), elem));
            }
            // スロットが fat への参照なら、ロードして fat 値を得る。
            // slot_ptr の実際の型は Ref(true, Agg) なことがある (ローカル配列の &mut v など)。
            Some(Kind::Slice(Some(elem))) => {
                let fat_val = match lw.vty(slot_ptr) {
                    Ty::Ref(_, Pointee::Agg(agg_ai)) => {
                        lw.emit(Ty::Agg(agg_ai), Op::Load(slot_ptr))
                    }
                    _ => slot_ptr,
                };
                return Ok((Seq::Fat(fat_val), elem));
            }
            // 要素型が局所の初期化から決まらなかった Vec (別の関数の `push` で決まるもの) は、式の型表の型で決める。
            Some(Kind::Vec(None)) => {
                if let Some(Src::Named(n, args)) = table_ty(lw, e).as_ref().map(strip_ref) {
                    if is_vecn(n) {
                        if let Some(elem) = args.first().and_then(|a| sig_ty(lw, a)).filter(|t| elem_size(lw, *t).is_some()) {
                            return Ok((Seq::Hdr(slot_ptr), elem));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // 構造体のフィールドの Vec (`self.handlers[i]`): フィールドのヘッダの番地を置き場にする (読みと要素の書き込みだけ。ヘッダは変えない)。
    if nname(e) == "Field" {
        if let Some((s, names)) = super::own::chain_of(lw, e) {
            if let Some(src) = super::own::field_src(lw, s, &names) {
                if !matches!(src, Src::Ref(..)) {
                    if let Some(Kind::Vec(Some(elem))) = kind_of_src(lw, &src) {
                        let pl = super::own::place(lw, s, &names, line)?;
                        return Ok((Seq::Hdr(pl.hdr), elem));
                    }
                }
            }
        }
    }
    // `b"..."` リテラルは式そのままなので fat 値を作る。
    if nname(e) == "Lit" {
        let f = nf(e);
        if st(&f[0]).as_str() == "bstr" {
            if let Some(fat) = lw.expr(e)? {
                return Ok((Seq::Fat(fat), Ty::U8));
            }
        }
    }
    lw.err(line, "添字・for の対象は要素型の分かる Vec・スライスの変数だけ段階 1 の対象")
}

/// 局所変数の型 (スロットの型)。式が局所変数の経路でないなら `None`。
pub(crate) fn local_slot_ty(lw: &Lw, e: &V) -> Option<Ty> {
    // `*r` (r は参照の局所変数): 型表が未確定のとき (`capture [&x]` で取り込んだ名前など) は指す先の型
    if nname(e) == "Unary" && st(&nf(e)[0]) == "*" {
        return match local_slot(lw, &nf(e)[1]).map(|s| lw.slots[s].1) {
            Some(Ty::Ref(_, p)) => Some(p.ty()),
            _ => None,
        };
    }
    local_slot(lw, e).map(|s| lw.slots[s].1)
}

/// 所有物を持つ要素の `v[i]` を含む式 (`v[i].f` / `v[i].f.len()` / `v[i].len()` / `&v[i]`) の準備: 添字を評価して要素の番地を求め、
/// 借用を持つスロットを作る (`elem_slots`)。以降の下ろしは、その `Index` の木を「借用の局所変数」として扱う。解放はしない。
pub(crate) fn elem_prep(lw: &mut Lw, e: &V) -> LR<()> {
    // 読むだけのメソッド・`&v[i]` は共有の借用、変更しうるメソッド・`&mut v[i]` は可変の借用 (設計_KIR.md 17.15)
    const READ_ONLY: [&str; 12] = ["len", "is_empty", "get", "as_slice", "as_str", "first", "last", "contains", "capacity", "iter", "clone", "to_string"];
    let m = match nname(e) {
        "MCall" => !READ_ONLY.contains(&st(&nf(e)[1]).as_str()),
        "Unary" => st(&nf(e)[0]) == "&mut",
        _ => false,
    };
    elem_prep_m(lw, e, m)
}

/// `elem_prep` の本体。`m` なら構造体の要素の借用を可変の参照にする (代入の対象、`&mut v[i]`、`&mut self` のメソッド)。
pub(crate) fn elem_prep_m(lw: &mut Lw, e: &V, m: bool) -> LR<()> {
    let mut cur = match nname(e) {
        "MCall" | "Field" => &nf(e)[0],
        "Unary" if matches!(st(&nf(e)[0]).as_str(), "&" | "&mut") => &nf(e)[1],
        _ => return Ok(()),
    };
    while nname(cur) == "Field" {
        cur = &nf(cur)[0];
    }
    if nname(cur) != "Index" {
        return Ok(());
    }
    let line = nline(cur);
    let f = nf(cur);
    let Ok((seq, elem)) = vec_place(lw, &f[0], line) else { return Ok(()) };
    if !owns_heap(lw, elem) {
        return Ok(());
    }
    let Some(i) = lw.expr(&f[1])? else { return Ok(()) };
    if lw.vty(i) != Ty::Usize {
        return lw.err(line, "添字の型が usize ではない");
    }
    let addr = elem_addr(lw, seq, i, elem)?;
    let Ty::Agg(ai) = elem else { return lw.err(line, "要素の借用は集成体だけ段階 1 の対象") };
    let slot = if lw.aggs.borrow().is_vec(elem) {
        let kind = table_ty(lw, cur).and_then(|t| kind_of_src(lw, &t)).or_else(|| lw.aggs.borrow().vec_elem(elem).map(|e| Kind::Vec(Some(e))));
        let Some(kind) = kind else { return lw.err(line, "添字の要素の種類 (Vec / String) が分からない") };
        let s = lw.new_slot(addr, Ty::Ptr);
        lw.coll.kinds.insert(s, kind);
        lw.coll.borrowed.insert(s);
        s
    } else {
        let rt = Ty::Ref(m, Pointee::Agg(ai));
        let r = lw.emit(rt, Op::Bitcast { src: Ty::Ptr, a: addr });
        lw.new_slot(r, rt)
    };
    lw.coll.elem_slots.insert(cur as *const V as usize, slot);
    Ok(())
}

/// `v[i]` の読み取り。
pub(crate) fn index_read(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    let f = nf(e);
    let line = nline(e);
    // 所有物を持つ要素: `elem_prep` が作った借用 (これを値として読むのは、フィールドの読み出しなど場所としての使用)
    if let Some(s) = local_slot(lw, e) {
        return Ok(Some(lw.read_slot(s)));
    }
    let (seq, elem) = vec_place(lw, &f[0], line)?;
    let Some(i) = lw.expr(&f[1])? else { return Ok(None) };
    if lw.vty(i) != Ty::Usize {
        return lw.err(line, "添字の型が usize ではない");
    }
    let addr = elem_addr(lw, seq, i, elem)?;
    // 集成体 (コピーできるもの) の要素は、`ref<集成体>` へ `bitcast` して `load` で値として読む。
    // `MemLoad` は整数・浮動小数・bool・ptr しか読めないので、ここで使うと下ろしの検査 (`load は ptr から ...`) に落ちる。
    if let Ty::Agg(ai) = elem {
        let r = lw.emit(Ty::Ref(false, Pointee::Agg(ai)), Op::Bitcast { src: Ty::Ptr, a: addr });
        return Ok(Some(lw.emit(elem, Op::Load(r))));
    }
    Ok(Some(lw.emit(elem, Op::MemLoad { p: addr, off: 0 })))
}

/// `&v[i]` / `&mut v[i]` (所有物を持たない要素): 境界検査のあとの要素の番地を、参照の値にする。扱えない形なら `None`。
pub(crate) fn index_ref(lw: &mut Lw, operand: &V, shared: bool, line: usize) -> LR<Option<VId>> {
    let f = nf(operand);
    let (seq, elem) = vec_place(lw, &f[0], line)?;
    let Some(p) = Pointee::of(elem).filter(|_| !owns_heap(lw, elem)) else {
        return lw.err(line, "要素への参照の指す先の型は段階 1 の対象外");
    };
    let Some(i) = lw.expr(&f[1])? else { return Ok(None) };
    if lw.vty(i) != Ty::Usize {
        return lw.err(line, "添字の型が usize ではない");
    }
    let addr = elem_addr(lw, seq, i, elem)?;
    let r = lw.emit(Ty::Ref(true, p), Op::Bitcast { src: Ty::Ptr, a: addr });
    Ok(Some(lw.ref_value(r, p, shared)))
}

/// `v[i] = x`。右辺を先に評価する (Rust の順序)。
pub(crate) fn assign_index(lw: &mut Lw, s: &V) -> LR<St> {
    let f = nf(s);
    let line = nline(s);
    let tf = nf(&f[0]);
    let (seq, elem) = vec_place(lw, &tf[0], line)?;
    let Some(v) = lw.expr(&f[1])? else { return Ok(St::Div) };
    let Some(i) = lw.expr(&tf[1])? else { return Ok(St::Div) };
    if lw.vty(i) != Ty::Usize {
        return lw.err(line, "添字の型が usize ではない");
    }
    if lw.vty(v) != elem {
        return lw.err(line, "代入の型が要素の型と違う");
    }
    let addr = elem_addr(lw, seq, i, elem)?;
    if owns_heap(lw, elem) {
        // 所有物の要素: 古い値を解放してから書く (設計_KIR.md 17.15)
        let Ty::Agg(ei) = elem else { return lw.err(line, "所有物の要素への代入は集成体だけ段階 1 の対象") };
        let r = lw.emit(Ty::Ref(true, Pointee::Agg(ei)), Op::Bitcast { src: Ty::Ptr, a: addr });
        let old = lw.emit(elem, Op::Load(r));
        drop_value(lw, old, elem);
        lw.emit(Ty::Unit, Op::Store { ptr: r, val: v });
        return Ok(St::Unit);
    }
    lw.emit(Ty::Unit, Op::MemStore { ty: elem, p: addr, off: 0, v });
    Ok(St::Unit)
}

/// `v[i].f.g = e` (`v` は所有物を持つ要素の Vec): 右辺を先に評価し、要素の可変の借用を作って、そのフィールドへ書く
/// (所有物のフィールドは古い値を解放してから)。この形でなければ `None`。
pub(crate) fn assign_elem_field(lw: &mut Lw, target: &V, rhs: &V, line: usize) -> Option<LR<St>> {
    let mut cur = target;
    let mut names = vec![];
    while nname(cur) == "Field" {
        names.push(st(&nf(cur)[1]));
        cur = &nf(cur)[0];
    }
    if nname(cur) != "Index" {
        return None;
    }
    names.reverse();
    let idx = cur;
    let go = |lw: &mut Lw| -> LR<St> {
        let (_, elem) = vec_place(lw, &nf(idx)[0], line)?;
        if !owns_heap(lw, elem) {
            return lw.err(line, "代入先は名前かフィールドだけ段階 1 の対象");
        }
        let Some(v) = lw.expr(rhs)? else { return Ok(St::Div) };
        elem_prep_m(lw, target, true)?;
        let Some(s) = local_slot(lw, idx) else { return lw.err(line, "要素の借用が作れない") };
        let mut segs = vec![String::new()];
        segs.extend(names.iter().cloned());
        lw.store_segs(s, &segs, v, line, true)
    };
    Some(go(lw))
}

/// 文字列リテラル: 静的なバイト列と、{ptr, len} の値 (`fat`)。
pub(crate) fn str_lit(lw: &mut Lw, e: &V) -> LR<VId> {
    let f = nf(e);
    let bytes = unhex(&st(&f[1]));
    let data = lw.emit(Ty::Ptr, Op::StaticBytes(bytes.clone()));
    let n = lw.emit(Ty::Usize, Op::Const(bytes.len() as i128));
    let fat = lw.fat_ty();
    Ok(lw.emit(fat, Op::Make { variant: None, fields: vec![data, n] }))
}

pub(super) fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()).collect()
}

/// `for PAT in IT BODY`。`IT` は `core.iter.range(a, b)` か、要素型の分かる Vec の変数。
pub(crate) fn for_expr(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    let f = nf(e);
    let line = nline(e);
    let name = match nname(&f[0]) {
        "PBind" if !matches!(nf(&f[0]).get(1), Some(V::B(true))) => Some(st(&nf(&f[0])[0])),
        "PWild" => None,
        // 組などの形のパターン (`for (k, v) in m.iter()`): 反復子の `next` の結果を分解する。
        _ => match user_iter_for(lw, e, &f[0], &f[1], &f[2]) {
            Some(r) => return r,
            None => return lw.err(line, "for の変数はただの名前か _ だけ段階 1 の対象"),
        },
    };
    let it = &f[1];
    if nname(it) == "Call" {
        let cf = nf(it);
        let is_range = nname(&cf[0]) == "Path" && strs(&nf(&cf[0])[0]) == ["core", "iter", "range"];
        let args = items(&cf[1]);
        if is_range && args.len() == 2 {
            let Some(a) = lw.expr(&args[0])? else { return Ok(None) };
            let Some(b) = lw.expr(&args[1])? else { return Ok(None) };
            let ity = lw.ty_of(&args[0])?;
            if !ity.is_int() || lw.vty(b) != ity {
                return lw.err(line, "core.iter.range の範囲は同じ整数の型だけ段階 1 の対象");
            }
            return counted(lw, line, Counted { start: a, end: b, ity, elem: None, esrc: None, by_ref: false, by_mut: false, take: false }, name, &f[2]);
        }
    }
    // `for x in [e1, e2]`: 配列リテラルは `{ptr, len}` の値として、スライスと同じに反復する
    if nname(it) == "ArrayE" {
        let Some((fat, elem)) = lw.array_lit(it)? else { return Ok(None) };
        let (len, base) = Seq::Fat(fat).len_and_base(lw, None);
        let zero = lw.emit(Ty::Usize, Op::Const(0));
        return counted(lw, line, Counted { start: zero, end: len, ity: Ty::Usize, elem: Some((base, elem)), esrc: None, by_ref: false, by_mut: false, take: false }, name, &f[2]);
    }
    // `for x in &v` / `for x in v.iter()` (v は局所変数): 要素の借用を回す。`v` を値で回す形とは、所有物の要素の扱いが違う。
    let by_mut = nname(it) == "Unary" && st(&nf(it)[0]) == "&mut";
    let (it, by_ref) = if by_mut { (&nf(it)[1], true) } else { borrowed_iter(it) };
    // `v.iter()` が `Call` の経路 `v.iter` の形のとき、受け手の局所は経路の先頭
    let call_iter = by_ref && nname(it) == "Call";
    let slot = if call_iter { lw.lookup(&strs(&nf(&nf(it)[0])[0])[0]) } else { local_slot(lw, it) };
    if let Some(s) = slot {
        // `lw.coll` と `lw.slots` から必要な値を先にコピーし、その後の `lw.emit` との借用競合を避ける。
        let kind = lw.coll.kinds.get(&s).cloned();
        let slot_ptr = lw.slots[s].0;
        let seq = match kind {
            Some(Kind::Vec(Some(elem))) => Some((Seq::Hdr(slot_ptr), elem)),
            // `&[T; N]` / `&mut [T; N]` のパラメータ: ptr の値、長さ N は型から分かる。
            Some(Kind::Slice(Some(elem))) if lw.vty(slot_ptr) == Ty::Ptr => {
                let n = lw.coll.arr_len[&s];
                Some((Seq::Ptr(slot_ptr, n), elem))
            }
            // スロットが fat への参照なら、ロードして fat 値を得る。
            Some(Kind::Slice(Some(elem))) => {
                let fat_val = match lw.vty(slot_ptr) {
                    Ty::Ref(_, Pointee::Agg(agg_ai)) => {
                        lw.emit(Ty::Agg(agg_ai), Op::Load(slot_ptr))
                    }
                    _ => slot_ptr,
                };
                Some((Seq::Fat(fat_val), elem))
            }
            _ => None,
        };
        let nm = if call_iter { Some(strs(&nf(&nf(it)[0])[0])[0].clone()) } else { path1_name(it) };
        if let (Some(_), Some(n)) = (&seq, nm) {
            // 反復中に `v` を変える (`push` で再確保される、`&mut v` を渡す、代入する) と、反復が読む先頭と長さが古くなって
            // 解放後のバッファを読む。`ksn check` はこの形を通すので KIR では拒む (設計_KIR.md 17.6)。
            if body_mutates(&f[2], &n) {
                return lw.err(line, "for の本体で反復中の Vec を変更する形は段階 1 の対象外 (再確保で反復が解放後の領域を読む)");
            }
        }
        if let Some((seq, elem)) = seq {
            // `for x in v` で `v` が参照 (`&Vec` のパラメータなど) なら、借用で回す
            let by_mut = by_mut || (owns_heap(lw, elem) && matches!(lw.src_ty(it), Ok(Src::Ref(true, _))));
            let by_ref = by_ref || (owns_heap(lw, elem) && matches!(lw.src_ty(it), Ok(Src::Ref(..))));
            let take = owns_heap(lw, elem) && !by_ref;
            if take && (lw.coll.borrowed.contains(&s) || !matches!(seq, Seq::Hdr(_)) || !matches!(lw.coll.kinds.get(&s), Some(Kind::Vec(_)))) {
                return lw.err(line, "要素が所有物の Vec を値で回す for は、局所変数の Vec だけ段階 1 の対象");
            }
            let esrc = if owns_heap(lw, elem) {
                match lw.src_ty(it) {
                    Ok(Src::Named(n, a)) if is_vecn(&n) => a.into_iter().next(),
                    // `v.iter()` の型は `core.slice.Iter<T>`
                    Ok(Src::Named(n, a)) if n == "core.slice.Iter" => a.into_iter().next(),
                    Ok(Src::Ref(_, i)) => match *i {
                        Src::Named(n, a) if is_vecn(&n) => a.into_iter().next(),
                        _ => None,
                    },
                    _ => None,
                }
            } else {
                None
            };
            if take {
                // 値で回す: `v` の 3 語を隠しヘッダへ移して、このスコープの所有に入れる。1 周ごとに要素を `x` へ移し、
                // バッファの元の場所は空 (解放しても何も起きない値) にする。`break` / `return` で抜けたときの残りの要素とバッファは、
                // このスコープの終わりの隠しヘッダの解放で片付く。
                lw.scopes.push(vec![]);
                let hdr = lw.slots[s].0;
                let val = move_hdr(lw, hdr);
                let h2 = adopt(lw, val);
                let (len, base) = Seq::Hdr(h2).len_and_base(lw, None);
                let zero = lw.emit(Ty::Usize, Op::Const(0));
                let r = counted(lw, line, Counted { start: zero, end: len, ity: Ty::Usize, elem: Some((base, elem)), esrc, by_ref, by_mut, take: true }, name, &f[2]);
                if r.is_ok() {
                    block_end(lw);
                }
                lw.scopes.pop();
                return r;
            }
            let (len, base) = seq.len_and_base(lw, None);
            let zero = lw.emit(Ty::Usize, Op::Const(0));
            return counted(lw, line, Counted { start: zero, end: len, ity: Ty::Usize, elem: Some((base, elem)), esrc, by_ref, by_mut, take: false }, name, &f[2]);
        }
    }
    // `for x in self.items.iter()` などフィールドの Vec: ヘッダのアドレスから反復する (要素はその場で借りる)。
    if let Some(r) = field_vec_for(lw, e, it, by_ref, by_mut, name.clone(), &f[2])? {
        return Ok(Some(r));
    }
    // `for x in f(..)` で、式の型が `&[T]` (スカラーの要素): 評価した `fat` の値の要素を回す。
    if !by_mut && nname(it) == "Call" {
        if let Ok(Src::Ref(false, inner)) = lw.src_ty(it) {
            if let Src::Slice(el) = &*inner {
                // 要素はスカラー、または所有物を持たない集成体 (`Copy` の構造体。`for n in p.ast.nodes.as_slice().iter()`)
                let elem = conv(el).filter(|t| size_of(*t).is_some()).or_else(|| lw.conv_t(el).filter(|t| matches!(t, Ty::Agg(_)) && !owns_heap(lw, *t) && elem_size(lw, *t).is_some()));
                if let Some(elem) = elem {
                    let Some(fat) = lw.expr(it)? else { return Ok(None) };
                    let (len, base) = Seq::Fat(fat).len_and_base(lw, None);
                    let zero = lw.emit(Ty::Usize, Op::Const(0));
                    return counted(lw, line, Counted { start: zero, end: len, ity: Ty::Usize, elem: Some((base, elem)), esrc: None, by_ref: true, by_mut: false, take: false }, name, &f[2]);
                }
            }
        }
    }
    // 利用者の反復子の局所 (`core.iter.Iterator` を実装し、`&mut self` の `next` を持つ): `next` を呼んで `None` まで回す
    if let Some(s) = local_slot(lw, it) {
        if let Some(di) = user_iter_next(lw, s) {
            return user_iter(lw, e, it, s, di, name, &f[2]);
        }
    }
    // `for x in s.split(sep)` / `for c in s.chars()` (`s` は `&str` = fat)。
    if let Some(r) = super::for_lower::try_str_for(lw, e)? {
        return Ok(Some(r));
    }
    // 反復子の式 (`m.iter()` など、局所でない利用者の反復子): 隠しの局所に置いて `next` を回す。
    if let Some(r) = user_iter_for(lw, e, &f[0], &f[1], &f[2]) {
        return r;
    }
    lw.err(line, "for の対象は core.iter.range(a, b) か、要素型の分かる Vec の変数・利用者の反復子の変数だけ段階 1 の対象")
}

/// `for x in R BODY` の `R` が、構造体の Vec のフィールド (`self.items` / `p.defs` / `x.f.v`) のとき、
/// フィールドのヘッダのアドレス (局所の名前から `field_addr` でたどる) から反復する。要素は Vec の中を
/// その場で借りる (所有物の要素は借用で束縛し、解放しない)。対象外 (フィールドでない・Vec でない) なら `None`。
fn field_vec_for(lw: &mut Lw, e: &V, it: &V, by_ref: bool, by_mut: bool, name: Option<String>, body: &V) -> LR<Option<VId>> {
    let line = nline(e);
    let (recv, by_ref, by_mut) = strip_seq_wrap(it, by_ref, by_mut);
    let Some(segs) = super::agg::field_segs(&recv).filter(|s| s.len() >= 2) else { return Ok(None) };
    let Some(vs) = lw.lookup(&segs[0]) else { return Ok(None) };
    // フィールドの型 (型表の型)。`.iter()` の形の受け手 (`b.names`) は木に型の記録が無いので、定義の列から求める。
    let Some(src) = super::own::field_src(lw, vs, &segs[1..]) else { return Ok(None) };
    // 要素の型 (Vec の型引数の先頭)。参照越しの Vec も同じ。
    let vec_elem_src = match &src {
        Src::Named(n, a) if is_vecn(n) => a.first().cloned(),
        Src::Ref(_, i) => match &**i {
            Src::Named(n, a) if is_vecn(n) => a.first().cloned(),
            _ => None,
        },
        _ => None,
    };
    let Some(elem_src) = vec_elem_src else { return Ok(None) };
    let Some(elem) = lw.conv_t(&elem_src) else { return Ok(None) };
    // 先頭の局所: 構造体の参照 (`self`) か、メモリ上の値。それ以外 (スカラーなど) は対象外。
    let base = lw.slots[vs].0;
    let mut cur = match lw.vty(base) {
        Ty::Ref(_, Pointee::Agg(_)) => base,
        _ if lw.is_mem_val(base) => base,
        _ => return Ok(None),
    };
    for n in &segs[1..] {
        cur = lw.field_addr(cur, n, line)?;
    }
    if !matches!(lw.vty(cur), Ty::Ref(_, Pointee::Agg(k)) if super::agg::is_vec_name(&lw.agg_def(k).name)) {
        return Ok(None);
    }
    if by_mut && matches!(lw.vty(cur), Ty::Ref(false, _)) {
        return lw.err(line, "共有の参照の先の Vec を可変で回す形は段階 1 の対象外");
    }
    if !by_ref && owns_heap(lw, elem) {
        return lw.err(line, "フィールドの所有物の Vec を値で回す for は段階 1 の対象外 (借用の先から要素を移せない)");
    }
    if body_mutates_place(body, &segs) {
        return lw.err(line, "for の本体で反復中の Vec を変更する形は段階 1 の対象外 (再確保で反復が解放後の領域を読む)");
    }
    // 添字・反復の対象のヘッダは生のアドレス (`ptr`) で持つ (`Seq::Hdr` の約束)。参照は bitcast で同じ番地の ptr に変える。
    let cur_ty = lw.vty(cur);
    let hdr = lw.emit(Ty::Ptr, Op::Bitcast { src: cur_ty, a: cur });
    let (len, base) = Seq::Hdr(hdr).len_and_base(lw, None);
    let zero = lw.emit(Ty::Usize, Op::Const(0));
    let esrc = Some(elem_src);
    counted(lw, line, Counted { start: zero, end: len, ity: Ty::Usize, elem: Some((base, elem)), esrc, by_ref, by_mut, take: false }, name, body)
}

/// 反復の対象の木から、外側の借用・反復の包みを外す: `&x` `&mut x` `x.iter()` `x.as_slice()`
/// (引数なし)、経路の形 (`a.b.iter()` は `Call` の `a.b.iter`)。
/// 返すのは受け手の木と、共有・可変の借用の有無。
fn strip_seq_wrap(it: &V, by_ref: bool, by_mut: bool) -> (V, bool, bool) {
    let (mut cur, mut r, mut m) = (it.clone(), by_ref, by_mut);
    loop {
        let next: Option<V> = match nname(&cur) {
            "Unary" if st(&nf(&cur)[0]) == "&" => {
                r = true;
                Some(nf(&cur)[1].clone())
            }
            "Unary" if st(&nf(&cur)[0]) == "&mut" => {
                m = true;
                Some(nf(&cur)[1].clone())
            }
            "MCall" if items(&nf(&cur)[2]).is_empty() => {
                let recv = nf(&cur)[0].clone();
                match st(&nf(&cur)[1]).as_str() {
                    "iter" => {
                        r = true;
                        Some(recv)
                    }
                    "as_slice" => Some(recv),
                    _ => None,
                }
            }
            // `a.b.iter()` は `Call` の経路 `a.b.iter` (引数なし)。受け手は経路の先頭から一つ手前まで。
            "Call" if nf(&cur).len() >= 2 && nname(&nf(&cur)[0]) == "Path" && items(&nf(&cur)[1]).is_empty() => {
                let segs = strs(&nf(&nf(&cur)[0])[0]);
                match segs.last().map(|s| s.as_str()) {
                    Some(last @ ("iter" | "as_slice")) if segs.len() >= 3 => {
                        if last == "iter" {
                            r = true;
                        }
                        let rs: Vec<V> = segs[..segs.len() - 1].iter().map(|s| V::S(s.clone())).collect();
                        Some(V::Node("Path", vec![V::L(rs), V::Nil], 0, 0))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        match next {
            Some(n) => cur = n,
            None => return (cur, r, m),
        }
    }
}

/// `&v` / `v.iter()` (共有の借用) なら (`v`, true)。それ以外はそのまま (式, false)。
fn borrowed_iter(it: &V) -> (&V, bool) {
    match nname(it) {
        "Unary" if st(&nf(it)[0]) == "&" => (&nf(it)[1], true),
        "MCall" if st(&nf(it)[1]) == "iter" && items(&nf(it)[2]).is_empty() => (&nf(it)[0], true),
        // `v.iter()` は `Call` の経路 `v.iter` の形にもなる
        "Call" if nname(&nf(it)[0]) == "Path" && items(&nf(it)[1]).is_empty() => {
            let segs = strs(&nf(&nf(it)[0])[0]);
            if segs.len() == 2 && segs[1] == "iter" {
                (it, true)
            } else {
                (it, false)
            }
        }
        _ => (it, false),
    }
}

/// 単一の名前の経路 (`v`) ならその名前。
fn path1_name(e: &V) -> Option<String> {
    if nname(e) != "Path" {
        return None;
    }
    let segs = strs(&nf(e)[0]);
    (segs.len() == 1).then(|| segs[0].clone())
}

/// 木 `v` の中で、名前 `name` の局所を変えうる形があるか: 代入、`&mut name`、読むだけとは限らないメソッド呼び出し。
pub(super) fn body_mutates(v: &V, name: &str) -> bool {
    body_mutates_place(v, &[name.to_string()])
}

/// 木 `v` の中で、場所 `target` (局所 `x`、またはフィールド `x.f.g` の名前の並び) を変えうる形があるか。
/// 代入の左辺・`&mut` の対象・読むだけとは限らないメソッドの受け手の場所が、`target` と先頭から
/// 一致するもの (片方がもう片方の前の並び) を数える。`self.count` の書き換えは `self.items` の反復を変えない。
pub(super) fn body_mutates_place(v: &V, target: &[String]) -> bool {
    const READ_ONLY: [&str; 10] = ["len", "is_empty", "get", "as_slice", "as_str", "first", "last", "contains", "capacity", "iter"];
    let overlaps = |p: &[String]| {
        let n = p.len().min(target.len());
        !p.is_empty() && p[..n] == target[..n]
    };
    match v {
        V::Node(n, fs, _, _) => {
            let hit = match *n {
                "Assign" => fs.first().and_then(|l| {
                    super::agg::field_segs(l).or_else(|| super::path_base(l).map(|b| vec![b]))
                }).is_some_and(|p| overlaps(&p)),
                "Unary" => fs.len() == 2 && st(&fs[0]) == "&mut" && super::agg::field_segs(&fs[1]).is_some_and(|p| overlaps(&p)),
                "Call" if fs.len() >= 2 && nname(&fs[0]) == "Path" => {
                    let segs = strs(&nf(&fs[0])[0]);
                    segs.len() >= 2 && !READ_ONLY.contains(&segs[segs.len() - 1].as_str()) && overlaps(&segs[..segs.len() - 1])
                }
                "MCall" if fs.len() >= 2 => {
                    super::agg::field_segs(&fs[0]).is_some_and(|p| overlaps(&p)) && !READ_ONLY.contains(&st(&fs[1]).as_str())
                }
                _ => false,
            };
            hit || fs.iter().any(|f| body_mutates_place(f, target))
        }
        V::L(xs) => xs.iter().any(|x| body_mutates_place(x, target)),
        _ => false,
    }
}

/// 局所 `s` の型が `core.iter.Iterator` を実装する利用者の集成体で、`&mut self` の `next` を持つなら、その定義の番号。
fn user_iter_next(lw: &Lw, s: usize) -> Option<usize> {
    let t = lw.slots[s].1;
    let Ty::Agg(ai) = t else { return None };
    let tyname = lw.aggs.borrow().defs[ai as usize].name.clone();
    let is_iter = lw.p.impls.iter().any(|(tr, ty)| *ty == tyname && tr.split('<').next() == Some("core.iter.Iterator"));
    if !is_iter {
        return None;
    }
    let di = lw.method_of_ty(t, "next")?;
    lw.method_takes_mut(di).then_some(di)
}

/// `for PAT in it BODY` (`it` は利用者の反復子): `loop { match it.next() { Some(PAT) => BODY, None => break } }` に下ろす。
/// `next` は `&mut it` で進むので、`it` はループの受け渡しの値 (ヘッダの引数) に入れる。`continue` はヘッダへ戻る。
fn user_iter(lw: &mut Lw, e: &V, it: &V, s: usize, di: usize, name: Option<String>, body: &V) -> LR<Option<VId>> {
    let line = nline(e);
    let mut slots = lw.assigned(&[body]);
    if !slots.contains(&s) {
        slots.push(s);
    }
    let slot_tys: Vec<Ty> = slots.iter().map(|x| lw.slots[*x].1).collect();
    let (xb, xp) = lw.new_block(&slot_tys);
    let (bb, _) = lw.new_block(&[]);
    let (hb, hp) = lw.new_block(&slot_tys);
    let entry = lw.vals_of(&slots);
    lw.goto(hb, entry);

    lw.cur = Some(hb);
    for (k, x) in slots.iter().enumerate() {
        lw.slots[*x] = (hp[k], slot_tys[k]);
    }
    let Some(o) = lw.call_method(e, di, super::method_lower::Place::Path(strs(&nf(it)[0])), &[])? else {
        return Ok(None);
    };
    let Ty::Opt(pay) = lw.vty(o) else {
        return lw.err(line, "for の反復子の next は Option を返す段階 1 の対象だけ");
    };
    let tag = lw.emit(Ty::Bool, Op::Tag(o));
    let exit_args = lw.vals_of(&slots);
    lw.branch(tag, (bb, vec![]), (xb, exit_args));

    lw.cur = Some(bb);
    lw.scopes.push(vec![]);
    let depth = lw.scopes.len() - 1;
    if let Some(n) = &name {
        let v = lw.emit(pay.ty(), Op::Payload(o));
        // 反復子が所有物 (Vec / String / それを持つ構造体) を返すときは、束縛した変数が所有して 1 周の終わりに解放する (設計_KIR.md 17.4)。
        bind_owned(lw, n, v, pay.ty());
    } else {
        // `for _ in it`: 束縛しない所有物はその場で捨てる。
        let v = lw.emit(pay.ty(), Op::Payload(o));
        drop_temp(lw, v);
    }
    lw.loops.push(super::LoopCx { header: hb, exit: xb, slots: slots.clone(), depth, cont: vec![] });
    let r = lw.expr(body)?;
    // 1 周の終わり: 変数が所有する値を先に解放してから、ヘッダへ戻る (戻った後の解放は到達しない)。
    block_end(lw);
    if r.is_some() {
        let args = lw.vals_of(&slots);
        lw.goto(hb, args);
    }
    lw.loops.pop();
    lw.scopes.pop();

    lw.cur = Some(xb);
    for (k, x) in slots.iter().enumerate() {
        lw.slots[*x] = (xp[k], slot_tys[k]);
    }
    Ok(Some(lw.unit()))
}

struct Counted {
    start: VId,
    end: VId,
    /// 添字 (範囲の要素) の型。
    ity: Ty,
    /// Vec を反復するとき: (要素の先頭アドレス, 要素の型)。
    elem: Option<(VId, Ty)>,
    /// 所有物の要素を借用で束縛するときの、要素の型表の型 (Vec / String の種類を決める)。
    esrc: Option<Src>,
    /// 借用で回す (`&v` / `v.iter()`)。スカラーの要素の変数は値で持つが、`*x` は恒等として読めるよう借用の印を付ける。
    by_ref: bool,
    /// 可変の借用で回す (`&mut v`)。要素の変数は `&mut T` (構造体・スカラー) か、借用のヘッダ (Vec / String)。
    by_mut: bool,
    /// 値で回す (所有物の要素を 1 つずつ `x` へ移す)。
    take: bool,
}

/// 添字を 1 ずつ増やす数え上げのループ。代入される変数は、ループの先頭のブロック引数で受け渡す。
/// 構造: header(i, slots) → (i < end) ? body : exit。body の終わりと continue は incr(slots) へ、incr は header(i+1, slots) へ戻る。
fn counted(lw: &mut Lw, _line: usize, c: Counted, name: Option<String>, body: &V) -> LR<Option<VId>> {
    let slots = lw.assigned(&[body]);
    let slot_tys: Vec<Ty> = slots.iter().map(|s| lw.slots[*s].1).collect();
    let (xb, xp) = lw.new_block(&slot_tys);
    let (ib, ip) = lw.new_block(&slot_tys);
    let mut hty = vec![c.ity];
    hty.extend(slot_tys.iter().copied());
    let (hb, hp) = lw.new_block(&hty);
    let mut entry = vec![c.start];
    entry.extend(lw.vals_of(&slots));
    lw.goto(hb, entry);

    lw.cur = Some(hb);
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (hp[k + 1], slot_tys[k]);
    }
    let (bb, _) = lw.new_block(&[]);
    let cond = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: c.ity, a: hp[0], b: c.end });
    let exit_args = lw.vals_of(&slots);
    lw.branch(cond, (bb, vec![]), (xb, exit_args));

    lw.cur = Some(bb);
    lw.scopes.push(vec![]);
    let depth = lw.scopes.len() - 1;
    if let Some(n) = &name {
        match c.elem {
            None => lw.bind(n, hp[0], c.ity),
            Some((base, ety)) if owns_heap(lw, ety) => {
                let addr = elem_addr_from_base(lw, base, hp[0], ety)?;
                if c.take {
                    // 要素を `x` へ移す: 値を読み、元の場所を空にする。`x` が所有して 1 周の終わりに解放する。
                    let Ty::Agg(ei) = ety else { return lw.err(_line, "値で回す for の要素は集成体だけ段階 1 の対象") };
                    let rt = Ty::Ref(true, Pointee::Agg(ei));
                    let r = lw.emit(rt, Op::Bitcast { src: Ty::Ptr, a: addr });
                    let v = lw.emit(ety, Op::Load(r));
                    let nv = null_value(lw, v, ety, _line)?;
                    lw.emit(Ty::Unit, Op::Store { ptr: r, val: nv });
                    lw.coll.hint = c.esrc.clone();
                    bind_owned(lw, n, v, ety);
                    lw.coll.hint = None;
                } else {
                    // 借用で束縛する (解放しない)
                    super::own::bind_elem_ref(lw, _line, n, addr, ety, c.esrc.as_ref(), c.by_mut)?;
                }
            }
            Some((base, ety)) => {
                let addr = elem_addr_from_base(lw, base, hp[0], ety)?;
                if c.by_mut {
                    // `for x in &mut v` (スカラー): 要素のアドレスを `&mut T` に束縛する
                    let Some(p) = Pointee::of(ety) else { return lw.err(_line, "可変の借用で回す要素の型は段階 1 の対象外") };
                    let rt = Ty::Ref(true, p);
                    let r = lw.emit(rt, Op::Bitcast { src: Ty::Ptr, a: addr });
                    lw.bind(n, r, rt);
                } else {
                    let v = load_elem(lw, addr, ety);
                    lw.bind(n, v, ety);
                }
                if c.by_ref && !c.by_mut {
                    let s = lw.lookup(n).expect("束縛した名前");
                    lw.coll.borrowed.insert(s);
                }
            }
        }
    }
    lw.loops.push(super::LoopCx { header: ib, exit: xb, slots: slots.clone(), depth, cont: vec![] });
    let r = lw.expr(body)?;
    if r.is_some() {
        // 1 周の終わり: 変数が所有する値を先に解放してから戻る
        block_end(lw);
        let args = lw.vals_of(&slots);
        lw.goto(ib, args);
    }
    lw.loops.pop();
    block_end(lw);
    lw.scopes.pop();

    // incr: 添字を 1 進めて先頭へ戻る (添字は body の中で変わらないので、先頭の値をそのまま使う)
    lw.cur = Some(ib);
    let one = lw.emit(c.ity, Op::Const(1));
    let next = lw.emit(c.ity, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: hp[0], b: one });
    let mut back = vec![next];
    back.extend(ip.iter().copied());
    lw.goto(hb, back);

    lw.cur = Some(xb);
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (xp[k], slot_tys[k]);
    }
    Ok(Some(lw.unit()))
}

/// Vec の要素の番地 (境界は呼び出し側が保証する)。
fn elem_addr_from_base(lw: &mut Lw, base: VId, i: VId, elem: Ty) -> LR<VId> {
    let Some(sz) = elem_size(lw, elem) else {
        return lw.err(0, format!("要素の型 {} は段階 1 の対象外", elem.name()));
    };
    let off = if sz == 1 {
        i
    } else {
        let k = lw.emit(Ty::Usize, Op::Const(sz as i128));
        lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: i, b: k })
    };
    Ok(lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off }))
}

/// 捕獲のないクロージャ。本体は別の関数 (鍵 `closure_key`) として下ろし、式の値は `unit` の印にする。
pub(crate) fn closure_lit(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    let f = nf(e);
    let line = nline(e);
    let Some(Src::Closure { args, ret, .. }) = table_ty(lw, e) else {
        return lw.err(line, "クロージャの型が決まらない");
    };
    let ats: Option<Vec<Ty>> = args.iter().map(|a| lw.conv_t(a)).collect();
    let (Some(ats), Some(rt)) = (ats, lw.conv_t(&ret)) else {
        return lw.err(line, "クロージャの引数か戻り値の型は段階 1 の対象外");
    };
    let names: Vec<String> = items(&f[0])
        .iter()
        .map(|p| match p {
            V::L(v) => st(&v[0]),
            _ => String::new(),
        })
        .collect();
    if names.len() != ats.len() {
        return lw.err(line, "クロージャの引数の個数が型と合わない");
    }
    let key = closure_key(lw, nid(e));
    let fn_once = matches!(table_ty(lw, e), Some(Src::Closure { cap: crate::types::Cap::FnOnce, .. }));
    let (cap_items, moved) = capture_values(lw, &f[2], &f[3], line, fn_once)?;
    let cap_vals: Vec<VId> = cap_items.iter().flat_map(|c| c.vals.iter().copied()).collect();
    let cap_tys: Vec<Ty> = cap_items.iter().flat_map(|c| c.tys.iter().copied()).collect();
    let mut child = Lw::new(lw.p, lw.tys, lw.aggs, lw.def);
    child.subst = lw.subst.clone();
    child.mono_done = lw.mono_done.clone();
    child.coll.prefix = lw.coll.prefix.clone();
    child.ret = rt;
    let mut ptys = cap_tys.clone();
    ptys.extend(ats.iter().copied());
    let (b0, pv) = child.new_block(&ptys);
    debug_assert_eq!(b0, 0);
    child.cur = Some(0);
    child.rf.begin(&f[3]);
    // 値で取り込んだ名前を本体が書き換える (または `&mut` で渡す) と、呼び出しをまたいで状態が残る必要がある。
    // 取り込みは呼び出しごとに値で渡すので、その形は未対応にする。
    let mut assigned = vec![];
    super::scan_assigns(&f[3], &mut assigned);
    for n in &moved {
        if assigned.contains(n) || child.rf.is_addr_name(n) {
            return lw.err(line, format!("move で取り込んだ {} を本体が変更する形は段階 1 の対象外 (呼び出しをまたぐ状態が要る)", n));
        }
    }
    child.scopes.push(vec![]);
    let nc = cap_vals.len();
    let mut at = 0;
    for c in &cap_items {
        let vs = &pv[at..at + c.vals.len()];
        at += c.vals.len();
        let (n, t) = (&c.name, c.tys[0]);
        match &c.how {
            // spawn のクロージャが取り込んだチャネルの端は、本体が所有して終わりで落とす (使わなかった端も)。
            CapHow::Plain if lw.coll.spawn_arg && child.aggs.borrow().owns_heap(t) => bind_owned(&mut child, n, vs[0], t),
            CapHow::Plain => child.bind_let(n, vs[0], t),
            // Vec / String / `&str` / スライスの借用 (または、呼び出しをまたいで取り込み側が所有する値の借用)。解放しない。
            CapHow::Borrow(k) => {
                child.bind_let(n, vs[0], t);
                let sl = child.lookup(n).expect("直前に束縛した名前");
                child.coll.kinds.insert(sl, k.clone());
                child.coll.borrowed.insert(sl);
            }
            // FnOnce のクロージャが値で取り込んだ所有物: 本体が所有する (消費しなければ終わりで解放)。
            CapHow::Own(k) => {
                bind_owned(&mut child, n, vs[0], t);
                let sl = child.lookup(n).expect("直前に束縛した名前");
                child.coll.kinds.insert(sl, k.clone());
            }
            // クロージャの値: 取り込みの値を引き継ぎ、本体の引数の位置に置き直す。
            CapHow::Closure(k) => {
                let tok = child.emit(Ty::Unit, Op::Const(0));
                child.bind(n, tok, Ty::Unit);
                let sl = child.lookup(n).expect("直前に束縛した名前");
                let mut k = k.clone();
                if let Kind::Closure { caps, .. } = &mut k {
                    *caps = vs.to_vec();
                }
                child.coll.kinds.insert(sl, k);
            }
        }
    }
    for ((n, v), t) in names.iter().zip(pv[nc..].iter()).zip(ats.iter()) {
        child.bind_let(n, *v, *t);
    }
    if let Some(v) = child.expr(&f[3])? {
        if child.vty(v) != rt {
            return lw.err(line, "クロージャの本体の型が戻り値の型と違う");
        }
        drop_from(&mut child, 0);
        child.terminate(Term::Ret(v));
    }
    child.flush_allocas();
    let mut blocks: Vec<Block> = child
        .blocks
        .into_iter()
        .map(|b| Block { params: b.params, insts: b.insts, term: b.term.unwrap_or(Term::Unreachable) })
        .collect();
    finish(&mut blocks, std::mem::take(&mut child.coll.allocas));
    KEY_EFF.with(|k| k.borrow_mut().insert(key.clone(), (lw.def.uses.clone(), lw.def.panics.as_deref() != Some("never"))));
    let func = Func {
        fqn: key.clone(),
        key,
        uses: lw.def.uses.clone(),
        may_panic: lw.def.panics.as_deref() != Some("never"),
        stack: None,
        ret: rt,
        link: None,
        blocks,
    };
    lw.coll.extra.push((func, child.callees));
    let nested = std::mem::take(&mut child.coll.extra);
    lw.coll.extra.extend(nested);
    lw.coll.caps.insert(nid(e), (cap_vals, cap_tys));
    Ok(Some(lw.unit()))
}

/// 取り込みの 1 項目。`vals` は呼び出し側の値 (クロージャの値は、その取り込みの値の列)、`tys` はその型。
struct CapItem {
    name: String,
    vals: Vec<VId>,
    tys: Vec<Ty>,
    how: CapHow,
}

enum CapHow {
    /// 値そのもの (スカラー・Copy の集成体・参照・チャネルの端)。
    Plain,
    /// 借用として使う (Vec / String は呼び出し側が所有するヘッダのアドレス、`&str` / スライスは `fat` の値)。
    Borrow(Kind),
    /// 値で取り込んだ所有物 (Vec / String)。FnOnce の本体が所有する。
    Own(Kind),
    /// クロージャの値 (取り込みを持ち回る)。
    Closure(Kind),
}

/// `capture [...]` の各項目を、クロージャを作る位置で評価する。戻り値: (項目, 値で取り込んだ名前)。
/// `move x` は x の現在の値、`&x` / `&mut x` は x のスロットへの参照。Vec / String は、`&` ならヘッダのアドレス (借用)、
/// `move` なら FnOnce のクロージャは値 (本体が所有)、それ以外は取り込み側の所有に移してヘッダのアドレスを借用として渡す。
/// クロージャの変数は、その取り込みの値を引き継ぐ。
fn capture_values(lw: &mut Lw, caps: &V, body: &V, line: usize, fn_once: bool) -> LR<(Vec<CapItem>, Vec<String>)> {
    let _ = body;
    let (mut out, mut moved) = (vec![], vec![]);
    for c in items(caps) {
        let kv = items(c);
        if kv.len() != 2 {
            return lw.err(line, "capture の項目の形が不正");
        }
        let (kind, name) = (st(&kv[0]), st(&kv[1]));
        let Some(s) = lw.lookup(&name) else {
            return lw.err(line, format!("capture の {} が見つからない", name));
        };
        if kind != "move" && kind != "&" && kind != "&mut" {
            return lw.err(line, "capture の種類が不明");
        }
        if kind == "move" {
            lw.own_emit(&name, s, crate::kir::OwnKind::Consume);
            moved.push(name.clone());
        }
        if let Some(k) = lw.coll.kinds.get(&s).cloned() {
            let borrowed = lw.coll.borrowed.contains(&s);
            let t = lw.slots[s].1;
            match &k {
                Kind::Closure { caps, cap_tys, .. } => {
                    if cap_tys.iter().any(|t| lw.aggs.borrow().owns_heap(*t)) {
                        return lw.err(line, format!("capture の {} は解放の要る値を取り込むクロージャで、段階 1 の対象外", name));
                    }
                    out.push(CapItem { name, vals: caps.clone(), tys: cap_tys.clone(), how: CapHow::Closure(k.clone()) });
                }
                Kind::Str | Kind::Slice(_) => {
                    let v = lw.read_slot(s);
                    out.push(CapItem { name, vals: vec![v], tys: vec![lw.vty(v)], how: CapHow::Borrow(k.clone()) });
                }
                // ヘッダのアドレス (借用の変数も同じ)
                _ if borrowed || kind != "move" => {
                    let h = lw.slots[s].0;
                    out.push(CapItem { name, vals: vec![h], tys: vec![lw.vty(h)], how: CapHow::Borrow(k.clone()) });
                }
                _ => {
                    let _ = t;
                    // 値で取り込む所有物 (move)
                    let v = path_read(lw, s);
                    if fn_once {
                        out.push(CapItem { name, vals: vec![v], tys: vec![lw.vty(v)], how: CapHow::Own(k.clone()) });
                    } else {
                        let h = adopt(lw, v);
                        out.push(CapItem { name, vals: vec![h], tys: vec![Ty::Ptr], how: CapHow::Borrow(k.clone()) });
                    }
                }
            }
            continue;
        }
        let t = lw.slots[s].1;
        // チャネルの端の `move` は取り込める (取り込み値を引き継いだ側が落とす。呼び出し元の印は `path_read` が下ろす)。
        let chan_move = kind == "move" && lw.coll.spawn_arg && lw.aggs.borrow().chan_kind(t).is_some();
        if lw.aggs.borrow().owns_heap(t) && !chan_move {
            return lw.err(line, format!("capture の {} は解放の要る値を持つので段階 1 の対象外 (所有の移動と解放の規則が要る)", name));
        }
        if t.is_ref() {
            return lw.err(line, format!("capture の {} は参照の変数で、取り込みは段階 1 の対象外", name));
        }
        let v = match kind.as_str() {
            "move" => {
                if chan_move {
                    path_read(lw, s)
                } else {
                    lw.read_slot(s)
                }
            }
            _ => {
                let ptr = lw.slots[s].0;
                let p = Pointee::of(t);
                match p {
                    Some(p) if lw.is_mem_val(ptr) => lw.ref_value(ptr, p, kind == "&"),
                    _ => return lw.err(line, format!("capture の {} のアドレスを取れない (段階 1 の対象外の型)", name)),
                }
            }
        };
        out.push(CapItem { name, vals: vec![v], tys: vec![lw.vty(v)], how: CapHow::Plain });
    }
    Ok((out, moved))
}

/// `&str` の値 (fat) か、その番地 (`&&str`) か。
pub(crate) fn is_str_value(lw: &Lw, t: Ty) -> bool {
    let fat = lw.fat_ty();
    t == fat || matches!(t, Ty::Ref(_, Pointee::Agg(i)) if fat == Ty::Agg(i))
}

/// `&str` の番地 (`&&str`) なら、読んだ fat の値。値ならそのまま。
fn load_fat_ref(lw: &mut Lw, v: VId) -> VId {
    match lw.vty(v) {
        // 集成体の値は番地から直接読めない (フィールドを読む): `&str` の番地は、ptr と len を読んで fat を作る。
        Ty::Ref(_, Pointee::Agg(_)) => hdr_view(lw, v),
        _ => v,
    }
}

/// 受け手の式の型から、文字列の種類 (`&str` / `String`) を決める。それ以外は `None`。
pub(crate) fn str_kind_of(lw: &Lw, e: &V) -> Option<Kind> {
    let t = lw.src_ty(e).ok()?;
    let mut t = &t;
    while let Src::Ref(_, i) = t {
        t = i;
    }
    match t {
        Src::Prim(s) if s == "str" => Some(Kind::Str),
        Src::Named(n, _) if n == STRING => Some(Kind::String),
        _ => None,
    }
}

/// `for PAT in IT BODY` で `IT` が `next` を持つ集成体 (反復子) のとき。反復子は局所に置き、毎周 `next(&mut it)` を呼ぶ。
/// `Some(x)` の中身を PAT に束縛して本体を回し、`None` で抜ける。`IT` が範囲・Vec・配列などなら `None` (従来の経路へ)。
fn user_iter_for(lw: &mut Lw, e: &V, pat: &V, it_e: &V, body: &V) -> Option<LR<Option<VId>>> {
    let line = nline(e);
    // 範囲・Vec・スライス・文字列などは従来の経路 (ここでは型だけ見る)。
    let it_src = lw.src_ty(it_e).ok()?;
    let base = match &it_src {
        Src::Named(n, _) => n.clone(),
        _ => return None,
    };
    if is_hdr_named(&it_src) || base.starts_with("core.iter.") || base.starts_with("std.") && !base.contains("Iter") {
        return None;
    }
    let mut segs: Vec<String> = base.split('.').map(String::from).collect();
    segs.push("next".to_string());
    let di = match lw.p.resolve(&segs, &lw.def.file).map(|r| r.target) {
        Some(Target::Fn(di)) if lw.p.defs[di].is_method() => di,
        _ => return None,
    };
    Some(user_iter_loop(lw, e, pat, it_e, body, di, line))
}

fn user_iter_loop(lw: &mut Lw, e: &V, pat: &V, it_e: &V, body: &V, di: usize, line: usize) -> LR<Option<VId>> {
    let Some(itv) = lw.expr(it_e)? else { return Ok(None) };
    let ity = lw.vty(itv);
    // 反復子を隠しの局所に置く (毎周 `next` の受け手)。
    let hidden = format!("#iter{}", lw.slots.len());
    // 反復子は毎周 `next` が書き換えるので、スタックスロットに置く (値のままだと周ごとに初めの値へ戻る)。
    lw.mark_addr(&hidden);
    if owns_heap(lw, ity) {
        lw.coll.hint = Some(lw.src_ty(it_e)?);
        lw.bind_let(&hidden, itv, ity);
        lw.coll.hint = None;
    } else {
        lw.bind_let(&hidden, itv, ity);
    }
    let slots = lw.assigned(&[body]);
    let slot_tys: Vec<Ty> = slots.iter().map(|s| lw.slots[*s].1).collect();
    let (xb, xp) = lw.new_block(&slot_tys);
    let (hb, hp) = lw.new_block(&slot_tys);
    let entry = lw.vals_of(&slots);
    lw.goto(hb, entry);

    // 先頭: `next(&mut it)` の結果で抜けるか決める。
    lw.cur = Some(hb);
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (hp[k], slot_tys[k]);
    }
    let Some(o) = lw.call_method(it_e, di, super::method_lower::Place::Path(vec![hidden.clone()]), &[])? else {
        return Ok(None);
    };
    let Ty::Opt(pay) = lw.vty(o) else {
        return lw.err(line, "for の反復子の next は Option を返すはず");
    };
    let (bb, _) = lw.new_block(&[]);
    let tag = lw.emit(Ty::Bool, Op::Tag(o));
    let exit_args = lw.vals_of(&slots);
    lw.branch(tag, (bb, vec![]), (xb, exit_args));

    // 本体: `Some(PAT)` を束縛して 1 周。
    lw.cur = Some(bb);
    lw.scopes.push(vec![]);
    let depth = lw.scopes.len() - 1;
    let pv = lw.emit(pay.ty(), Op::Payload(o));
    let binds = lw.pat(pat, pv, pay.ty(), None)?;
    for (n, v, t) in binds {
        // 借用のヘッダ (`&String` の欄は `ptr`): 借用の `String` として束縛する (メソッドの下ろしが種類を見る)。
        if t == Ty::Ptr {
            lw.bind(&n, v, Ty::Ptr);
            if let Some(sl) = lw.lookup(&n) {
                lw.coll.kinds.insert(sl, Kind::String);
                lw.coll.borrowed.insert(sl);
            }
            continue;
        }
        bind_owned(lw, &n, v, t);
    }
    lw.loops.push(super::LoopCx { header: hb, exit: xb, slots: slots.clone(), depth, cont: vec![] });
    let r = lw.expr(body)?;
    if r.is_some() {
        block_end(lw);
        let args = lw.vals_of(&slots);
        lw.goto(hb, args);
    }
    lw.loops.pop();
    block_end(lw);
    lw.scopes.pop();

    lw.cur = Some(xb);
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (xp[k], slot_tys[k]);
    }
    Ok(Some(lw.unit()))
}
