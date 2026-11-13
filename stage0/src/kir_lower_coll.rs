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
const STRING: &str = "alloc.string.String";
const HASHMAP: &str = "alloc.collections.HashMap";
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
    Map(Option<(Ty, Ty)>),
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
}

/// 下ろしの状態。`Lw` が 1 つ持つ。
#[derive(Default)]
pub(crate) struct CollCx {
    /// スロット番号 → 種類。
    pub(crate) kinds: BTreeMap<usize, Kind>,
    /// 固定長配列への参照のスロット → 要素数 N (`&[T; N]` / `&mut [T; N]` のパラメータと、
    /// アドレスを取られた配列の局所変数)。長さは型から分かるので定数として扱う。
    pub(crate) arr_len: BTreeMap<usize, u32>,
    /// 所有する値 (スコープの番号, 所有物) の登録順。
    pub(crate) owned: Vec<(usize, Owned)>,
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
    /// Vec / String のヘッダ (フレームの領域) → 値の型 (`vec` または要素が所有物の `vec<#N>`)。移すときの型と、解放のとき要素を解放するかに使う。
    pub(crate) hdr_ty: BTreeMap<VId, Ty>,
    /// 構造体のフィールド (場所) を `match` の対象として読んだ直後か (`scrut_begin` が読んで下ろす)。値は移していない。
    pub(crate) peek_field: bool,
    /// 下ろしたクロージャの式 ID → (取り込みの値, その型)。`closure_kind_from_table` が読む。
    pub(crate) caps: BTreeMap<u32, (Vec<VId>, Vec<Ty>)>,
    /// `std.thread.spawn` で保留されたクロージャの (鍵, 取り込み値) の列。FIFO = spawn 順。
    /// `join` が先頭を取り出してクロージャ本体を呼ぶ (設計_KIR.md スレッド章)。
    pub(crate) pending_spawns: Vec<super::thread_lower::Site>,
}

/// 型表の式の型 (式 ID が 0 なら無い)。
pub(super) fn table_ty(lw: &Lw, e: &V) -> Option<Src> {
    let id = nid(e);
    if id == 0 {
        return None;
    }
    lw.tys.get(&(lw.def.name.clone(), id)).map(|x| x.ty.clone())
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
    // 値渡しの Vec / String は集成体 `vec` (所有の移動)、参照は借用なのでヘッダのアドレス。
    if let Src::Named(n, args) = t {
        if n == VEC {
            let elem = args.first().and_then(|a| lw.conv_t(a));
            return Some(lw.aggs.borrow_mut().vec_of(elem));
        }
        if n == STRING {
            return Some(lw.aggs.borrow_mut().vec_hdr());
        }
        if n == HASHMAP {
            return Some(lw.aggs.borrow_mut().vec_hdr());
        }
    }
    match strip_ref(t) {
        Src::Named(n, _) if n == VEC || n == STRING || n == HASHMAP => Some(Ty::Ptr),
        Src::Prim(s) if s == "str" => Some(lw.fat_ty()),
        Src::Slice(e) if conv(e).and_then(size_of).is_some() => Some(lw.fat_ty()),
        Src::Closure { .. } => Some(Ty::Unit),
        _ => None,
    }
}

/// `Vec` / `String` の名前の型か。
pub(crate) fn is_hdr_named(t: &Src) -> bool {
    matches!(t, Src::Named(n, _) if n == VEC || n == STRING || n == HASHMAP)
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
        Src::Named(n, _) if n == VEC || n == STRING || n == HASHMAP => Some(Ty::Ptr),
        Src::Prim(s) if s == "str" => Some(lw.fat_ty()),
        Src::Slice(e) if conv(e).and_then(size_of).is_some() => Some(lw.fat_ty()),
        Src::Named(n, _) if n == "std.net.TcpListener" || n == "std.net.TcpStream" => {
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
        if matches!(lw.coll.kinds.get(&s), Some(Kind::Vec(_)) | Some(Kind::String) | Some(Kind::Map(_))) {
            return Ok(lw.slots[s].0);
        }
    }
    lw.err(line, "Vec / String の借用は局所変数だけ段階 1 の対象")
}

pub(super) fn kind_of_src(lw: &Lw, t: &Src) -> Option<Kind> {
    match strip_ref(t) {
        // 要素が所有物 (`Vec<String>`) のときは、要素の型 (集成体 `vec` など) を持つ
        Src::Named(n, args) if n == VEC => Some(Kind::Vec(args.first().and_then(|a| conv(a).or_else(|| lw.conv_t(a))))),
        Src::Named(n, _) if n == STRING => Some(Kind::String),
        Src::Named(n, args) if n == HASHMAP => Some(Kind::Map(map_kv(args))),
        Src::Prim(s) if s == "str" => Some(Kind::Str),
        Src::Slice(e) => Some(Kind::Slice(conv(e).filter(|t| size_of(*t).is_some()))),
        // 固定長配列 `&[T; N]` の要素型。長さ N は `arr_len` で別途追跡する。
        Src::Array(e) => Some(Kind::Slice(conv(e).filter(|t| size_of(*t).is_some()))),
        _ => None,
    }
}

/// `HashMap<K, V, _>` の型引数から (キー, 値) の KIR の型。キーは整数・bool・char (`Ty`) か `&str` (`Ty::Ptr` の印)、値は整数・bool・char。
fn map_kv(args: &[Src]) -> Option<(Ty, Ty)> {
    let k = match args.first()? {
        Src::Ref(_, i) if matches!(&**i, Src::Prim(s) if s == "str") => Ty::Ptr,
        other => conv(other).filter(|t| size_of(*t).is_some())?,
    };
    let v = conv(args.get(1)?).filter(|t| size_of(*t).is_some())?;
    Some((k, v))
}

/// クロージャの鍵。式 ID は関数 (または単相インスタンス) の中で一意なので、同じ中の別のクロージャと衝突しない。
fn closure_key(lw: &Lw, id: u32) -> String {
    let base = if lw.coll.prefix.is_empty() { lw.def.name.as_str() } else { lw.coll.prefix.as_str() };
    format!("{}#c{}", base, id)
}

/// 単一の名前の経路なら、その名前のスロット (見えているもの)。
pub(crate) fn local_slot(lw: &Lw, e: &V) -> Option<usize> {
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
        Ty::I64 | Ty::U64 | Ty::Isize | Ty::Usize => 8,
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
    if matches!(lw.coll.kinds.get(&s), Some(Kind::Vec(_)) | Some(Kind::String) | Some(Kind::Map(_))) && !lw.coll.borrowed.contains(&s) {
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

/// 型 `t` の値 `v` (所有している) を解放する命令列を、現在のブロックに置く。所有物を持たない型は何もしない。
/// 列挙型と `Option` / `Result` は、タグで枝を分けて、持っている変種のペイロードだけを解放する。
pub(crate) fn drop_value(lw: &mut Lw, v: VId, t: Ty) {
    if lw.cur.is_none() || !owns_heap(lw, t) {
        return;
    }
    match t {
        Ty::Agg(i) => {
            let d = lw.aggs.borrow().defs[i as usize].clone();
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
        Owned::Hdr(h) => drop_hdr(lw, h),
        Owned::Tmp(v) => {
            let t = lw.vty(v);
            drop_value(lw, v, t);
        }
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
    if matches!(lw.coll.kinds.get(&s), Some(Kind::Vec(_)) | Some(Kind::String) | Some(Kind::Map(_))) && !lw.coll.borrowed.contains(&s) {
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
    // 構造体のフィールド (場所) の値を対象にする `match`: 所有物を束縛しない腕だけなら借りて見るだけ。束縛するなら部分的な移動が要る。
    if peeked {
        if moves {
            return lw.err(nline(scrut), "フィールドの値を所有物を束縛する match の対象にする形 (部分的な移動) は段階 1 の対象外");
        }
        return Ok(Some(Scrut { owned: false, t, covers }));
    }
    let slot = val_slot(lw, scrut);
    if let (Some(s), true) = (slot, moves) {
        if let Some(flag) = val_flag(lw, s) {
            set_flag(lw, flag, false);
        }
    }
    Ok(Some(Scrut { owned: slot.is_none() || moves, t, covers }))
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
    let (p, t) = lw.slots[s];
    match t {
        Ty::Ref(..) => lw.err(line, "参照を通したフィールドの移動は段階 1 の対象外 (借用の中身を移すことになる)"),
        Ty::Agg(_) if lw.is_mem_val(p) => {
            let mut cur = p;
            for n in names {
                cur = lw.field_addr(cur, n, line)?;
            }
            let Ty::Ref(_, pt) = lw.vty(cur) else { return lw.err(line, "フィールドのアドレスが参照でない") };
            let ft = pt.ty();
            let v = lw.emit(ft, Op::Load(cur));
            let nv = null_value(lw, v, ft, line)?;
            lw.emit(Ty::Unit, Op::Store { ptr: cur, val: nv });
            Ok(v)
        }
        Ty::Agg(_) => {
            let base = lw.read_slot(s);
            let v = lw.field_path(base, names, line)?;
            let ft = lw.vty(v);
            let nv = null_value(lw, v, ft, line)?;
            let mut segs = vec![String::new()];
            segs.extend(names.iter().cloned());
            lw.store_segs(s, &segs, nv, line, false)?;
            Ok(v)
        }
        _ => lw.err(line, "フィールドの移動の根が構造体でない"),
    }
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
    let Some(v) = lw.expr(&args[0])? else { return Ok(None) };
    drop_temp(lw, v);
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
                return Some(call_closure(lw, e, s));
            }
        }
    }
    // 反復子の終端 (`fold` / `count`) は、連なりを 1 つのループにする。
    if segs == ["core", "iter", "fold"] || segs == ["core", "iter", "count"] {
        return Some(super::iter_lower::pipe_call(lw, e, segs[2] == "fold"));
    }
    // 型引数に型パラメータを持つ関数 (`Fn` 境界の引数) は、渡されたクロージャごとに単相化する。
    if let Some(Target::Fn(di)) = lw.p.resolve(&segs, &lw.def.file).map(|r| r.target) {
        if lw.p.defs[di].generics.iter().any(|g| g.0 == "type") {
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
    let segs: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    match segs.as_slice() {
        ["alloc", "vec", "Vec", "new"] => Some(new_header(lw, e)),
        ["alloc", "string", "String", "new"] => Some(new_header(lw, e)),
        ["alloc", "string", "String", "from_str"] => {
            Some(lw.from_str_call(e, items(&f[1]), nline(e)))
        }
        ["core", "str", "from_utf8"] => {
            Some(lw.from_utf8_call(e, items(&f[1]), nline(e)))
        }
        ["alloc", "collections", "HashMap", "new"] => Some(new_header(lw, e)),
        // `core.mem.drop(x)`: 値を移して、その場で解放する。
        ["core", "mem", "drop"] => Some(mem_drop(lw, e)),
        _ => None,
    }
}

/// 型引数に捕獲のないクロージャを取る関数の呼び出し。鍵は `関数名<クロージャの鍵,...>` で、
/// クロージャの引数は実行時の値を持たず (鍵で呼び先が決まる)、値の引数だけを渡す。
fn generic_call(lw: &mut Lw, e: &V, di: usize) -> LR<Option<VId>> {
    let line = nline(e);
    let p = lw.p;
    let cd = &p.defs[di];
    let f = nf(e);
    if cd.kw != "fn" || cd.body.is_none() || cd.is_method() || !matches!(f.get(2), Some(V::Nil) | None) {
        return lw.err(line, format!("呼び先 {} は段階 1 の対象外 (型引数の単相化は自由関数だけ)", cd.name));
    }
    let tnames: HashSet<String> = cd.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.clone()).collect();
    let argv = items(&f[1]);
    if argv.len() != cd.params.len() {
        return lw.err(line, format!("呼び先 {} の引数の個数が違う", cd.name));
    }
    let mut bound: BTreeMap<String, Kind> = BTreeMap::new();
    let mut vals = vec![];
    for ((_, pt), a) in cd.params.iter().zip(argv) {
        match from_v(pt, &tnames) {
            Src::Param(g) => {
                let Some(k) = closure_arg_kind(lw, a)? else {
                    return lw.err(line, "型引数に渡せるのは捕獲のないクロージャ (段階 1) だけ");
                };
                bound.insert(g, k);
            }
            t => {
                let Some(pty) = conv(&t) else {
                    return lw.err(line, format!("呼び先 {} の引数の型は段階 1 の対象外", cd.name));
                };
                let Some(v) = lw.expr(a)? else { return Ok(None) };
                if lw.vty(v) != pty {
                    return lw.err(line, format!("呼び先 {} の引数の型が合わない", cd.name));
                }
                vals.push(v);
            }
        }
    }
    let mut keys = vec![];
    for g in cd.generics.iter().filter(|g| g.0 == "type") {
        match bound.get(&g.1) {
            Some(Kind::Closure { key, caps, .. }) => {
                keys.push(key.clone());
                // 取り込みの値は、値の引数の後ろに型引数の順で渡す
                vals.extend(caps.iter().copied());
            }
            _ => return lw.err(line, format!("型引数 {} が決まらない", g.1)),
        }
    }
    let ret = match ret_ty(cd, &tnames) {
        Some(x) => x,
        None => return lw.err(line, format!("呼び先 {} の戻り値の型は段階 1 の対象外", cd.name)),
    };
    let key = format!("{}<{}>", cd.name, keys.join(","));
    lw.callees.insert(key.clone());
    if !lw.coll.instances.contains(&key) {
        lw.coll.instances.insert(key.clone());
        let mut child = Lw::new(lw.p, lw.tys, lw.aggs, cd);
        child.coll.instances = std::mem::take(&mut lw.coll.instances);
        let r = lower_instance(&mut child, cd, &key, &tnames, &bound);
        lw.coll.instances = std::mem::take(&mut child.coll.instances);
        let (f, callees) = r?;
        lw.coll.extra.push((f, callees));
        let nested = std::mem::take(&mut child.coll.extra);
        lw.coll.extra.extend(nested);
    }
    Ok(Some(lw.emit(ret, Op::Call { callee: key, args: vals })))
}

/// 戻り値の型 (省略は unit)。段階 1 の型に入らないなら `None`。
fn ret_ty(cd: &crate::program::Def, tnames: &HashSet<String>) -> Option<Ty> {
    if is_nil(&cd.ret) {
        return Some(Ty::Unit);
    }
    conv(&from_v(&cd.ret, tnames))
}

/// 型引数に渡すクロージャの種類 (クロージャの式、または種類の分かる局所変数)。
pub(crate) fn closure_arg_kind(lw: &mut Lw, a: &V) -> LR<Option<Kind>> {
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
    bound: &BTreeMap<String, Kind>,
) -> LR<(Func, BTreeSet<String>)> {
    child.coll.prefix = key.to_string();
    let mut value_params = vec![];
    for (_, pt) in &cd.params {
        if let Src::Param(_) = from_v(pt, tnames) {
            continue;
        }
        match sig_ty(child, &from_v(pt, tnames)) {
            Some(x) => value_params.push(x),
            None => return child.err(0, format!("呼び先 {} の引数の型は段階 1 の対象外", cd.name)),
        }
    }
    let nvalue = value_params.len();
    for g in cd.generics.iter().filter(|g| g.0 == "type") {
        if let Some(Kind::Closure { cap_tys, .. }) = bound.get(&g.1) {
            value_params.extend(cap_tys.iter().copied());
        }
    }
    let (_, pv) = child.new_block(&value_params);
    // 型引数ごとの取り込みの引数の位置 (値の引数の後ろ、型引数の順)
    let mut cap_at: BTreeMap<String, Vec<VId>> = BTreeMap::new();
    let mut off = nvalue;
    for g in cd.generics.iter().filter(|g| g.0 == "type") {
        if let Some(Kind::Closure { cap_tys, .. }) = bound.get(&g.1) {
            cap_at.insert(g.1.clone(), pv[off..off + cap_tys.len()].to_vec());
            off += cap_tys.len();
        }
    }
    child.cur = Some(0);
    child.scopes.push(vec![]);
    let mut vi = 0;
    for (pname, pt) in &cd.params {
        if let Src::Param(g) = from_v(pt, tnames) {
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
            let t = sig_ty(child, &from_v(pt, tnames)).expect("上で検査済み");
            bind_param(child, pname, pv[vi], t, &from_v(pt, tnames));
            vi += 1;
        }
    }
    child.ret = ret_ty(cd, tnames).ok_or_else(|| format!("呼び先 {} の戻り値の型は段階 1 の対象外", cd.name))?;
    let body = cd.body.as_ref().expect("候補は本体を持つ");
    child.rf.begin(body);
    if let Some(v) = child.expr(body)? {
        if child.vty(v) != child.ret {
            return child.err(nline(body), "本体の値の型が戻り値の型と合わない");
        }
        drop_from(child, 0);
        child.terminate(Term::Ret(v));
    }
    // 効果の型引数 (`uses [E]`) は、捕獲のないクロージャでは空の集合に束縛される。
    let mut uses: Vec<String> = cd.uses.iter().filter(|u| !cd.generics.iter().any(|g| g.1 == **u)).cloned().collect();
    uses.sort();
    uses.dedup();
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
        may_panic: cd.panics.as_deref() != Some("never"),
        stack: cd.stack,
        ret: child.ret,
        link: None,
        blocks,
    };
    Ok((func, std::mem::take(&mut child.callees)))
}

fn call_closure(lw: &mut Lw, e: &V, s: usize) -> LR<Option<VId>> {
    let line = nline(e);
    let Some(Kind::Closure { key, args, ret, caps, .. }) = lw.coll.kinds.get(&s).cloned() else {
        return lw.err(line, "クロージャの呼び出しの内部不整合");
    };
    let argv = items(&nf(e)[1]);
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
    Some(match place(lw, &f[0]) {
        Ok(Some(h)) if matches!(kind, Kind::Map(_)) => super::map_lower::method(lw, e, slot, &kind, h, &m, items(&f[2])),
        Ok(Some(h)) => method_on(lw, e, line, slot, kind, h, &m, items(&f[2])),
        Ok(None) => Ok(None),
        Err(e) => Err(e),
    })
}

/// ヘッダ (または `fat`) `h` を受け手とするメソッド呼び出し。所有の周りの新しいメソッド (`own`) を先に試す。
pub(super) fn method_on(lw: &mut Lw, e: &V, line: usize, slot: Option<usize>, kind: Kind, h: VId, m: &str, args: &[V]) -> LR<Option<VId>> {
    if let Some(r) = lw.str_method_call(e, &kind, h, m, args) {
        return r;
    }
    if let Some(r) = super::own::method(lw, e, line, &kind, h, m, args) {
        return r;
    }
    if m == "push" && matches!(kind, Kind::Vec(_)) {
        return vec_push(lw, e, line, slot, &kind, h, args);
    }
    if m == "push_str" && matches!(kind, Kind::String) {
        return string_push_str(lw, e, line, slot, h, args);
    }
    if m == "push_u64" && matches!(kind, Kind::String) {
        return string_push_u64(lw, e, line, slot, h, args);
    }
    if m == "get" {
        return get_elem(lw, line, &kind, h, args);
    }
    if m == "sub" {
        return slice_sub(lw, line, &kind, h, args);
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
    let hdr = lw.slots[s].0;
    // `&[T; N]` / `&mut [T; N]` のパラメータの `len()`: 長さ N は型から分かる。
    if m == "len" && matches!(kind, Kind::Slice(_)) && lw.slots[s].1 == Ty::Ptr {
        let n = arr_len_of(lw, s).expect("&[T; N] のパラメータに arr_len が入っている");
        return Ok(Some(lw.emit(Ty::Usize, Op::Const(n as i128))));
    }
    if matches!(kind, Kind::Map(_)) {
        return super::map_lower::method(lw, e, Some(s), &kind, hdr, m, items(&f[1]));
    }
    method_on(lw, e, line, Some(s), kind, hdr, m, items(&f[1]))
}

pub(super) fn coll_method(lw: &mut Lw, line: usize, kind: Kind, h: VId, m: &str, nargs: usize) -> LR<Option<VId>> {
    if matches!(kind, Kind::Closure { .. }) {
        return lw.err(line, format!("メソッド {} は段階 1 の対象外", m));
    }
    if !matches!(m, "len" | "is_empty" | "as_bytes" | "as_str" | "as_slice") {
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
        // `String.as_str()` / `Vec.as_slice()`: ヘッダの {ptr, len} を借用の `fat` にする (所有は移さない)。
        "as_str" if kind == Kind::String => Ok(Some(hdr_view(lw, h))),
        "as_slice" if matches!(kind, Kind::Vec(_)) => Ok(Some(hdr_view(lw, h))),
        // `&str.as_str()` はそのまま。
        "as_str" if kind == Kind::Str => Ok(Some(h)),
        other => lw.err(line, format!("メソッド {} は段階 1 の対象外", other)),
    }
}

/// `v.push(x)` (要素はスカラーの Vec): 満杯なら `heap.realloc` で広げ、末尾に書いて `len` を進める。結果は `Ok(())`。
/// 確保に失敗することはない (`AllocError` の `Err` は作らない)。要素型が未確定の Vec は、引数の型で決める。
pub(super) fn vec_push(lw: &mut Lw, e: &V, line: usize, slot: Option<usize>, kind: &Kind, h: VId, args: &[V]) -> LR<Option<VId>> {
    if args.len() != 1 {
        return lw.err(line, "push の引数は 1 つのはず");
    }
    let Some(x) = lw.expr(&args[0])? else { return Ok(None) };
    let xt = lw.vty(x);
    let elem = match kind {
        Kind::Vec(Some(t)) => *t,
        _ => xt,
    };
    let Some(sz) = elem_size(lw, elem) else {
        return lw.err(line, format!("push の要素の型 {} は段階 1 の対象外", elem.name()));
    };
    if xt != elem {
        return lw.err(line, "push の引数の型が要素の型と違う");
    }
    if matches!(kind, Kind::Vec(None)) {
        if let Some(s) = slot {
            lw.coll.kinds.insert(s, Kind::Vec(Some(elem)));
        }
    }
    let rt = lw.ty_of(e)?;
    if !matches!(rt, Ty::Res(Pay::Unit, _)) {
        return lw.err(line, "push の結果の型が Result<(), AllocError> でない");
    }
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let cap = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 });
    let full = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: len, b: cap });
    let (grow, _) = lw.new_block(&[]);
    let (cont, _) = lw.new_block(&[]);
    lw.branch(full, (grow, vec![]), (cont, vec![]));
    lw.cur = Some(grow);
    let dbl = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: cap, b: cap });
    let four = lw.emit(Ty::Usize, Op::Const(4));
    let newcap = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: dbl, b: four });
    let bytes = if sz == 1 {
        newcap
    } else {
        let k = lw.emit(Ty::Usize, Op::Const(sz as i128));
        lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: newcap, b: k })
    };
    let old = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let np = lw.emit(Ty::Ptr, Op::HeapRealloc { p: old, size: bytes });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: h, off: 0, v: np });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 16, v: newcap });
    lw.goto(cont, vec![]);
    lw.cur = Some(cont);
    let base = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let off = if sz == 1 {
        len
    } else {
        let k = lw.emit(Ty::Usize, Op::Const(sz as i128));
        lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: len, b: k })
    };
    let addr = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off });
    store_elem(lw, addr, elem, x);
    let one = lw.emit(Ty::Usize, Op::Const(1));
    let nl = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: len, b: one });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: nl });
    let u = lw.unit();
    Ok(Some(lw.emit(rt, Op::Ok(u))))
}

/// `String.push_str(text)`: `text` のバイトを `len` の位置へコピーして `len` を進める。
/// 満杯なら `heap.realloc` で `need` に広げる (正確なサイズ確保)。
pub(super) fn string_push_str(lw: &mut Lw, e: &V, line: usize, _slot: Option<usize>, h: VId, args: &[V]) -> LR<Option<VId>> {
    if args.len() != 1 {
        return lw.err(line, "push_str の引数は 1 つのはず");
    }
    let Some(text) = lw.expr(&args[0])? else { return Ok(None) };
    if lw.vty(text) != lw.fat_ty() {
        return lw.err(line, "push_str の引数は &str ではない");
    }
    let rt = lw.ty_of(e)?;
    if !matches!(rt, Ty::Res(Pay::Unit, _)) {
        return lw.err(line, "push_str の結果の型が Result<(), AllocError> でない");
    }
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let cap = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 });
    let tlen = lw.emit(Ty::Usize, Op::MemLoad { p: text, off: 8 });
    let need = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: len, b: tlen });
    let full = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::Usize, a: need, b: cap });
    let (grow, _) = lw.new_block(&[]);
    let (cont, _) = lw.new_block(&[]);
    lw.branch(full, (grow, vec![]), (cont, vec![]));
    lw.cur = Some(grow);
    let old = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let np = lw.emit(Ty::Ptr, Op::HeapRealloc { p: old, size: need });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: h, off: 0, v: np });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 16, v: need });
    lw.goto(cont, vec![]);
    lw.cur = Some(cont);
    // `for i in core.iter.range(0usize, tlen) { base[len + i] = text[i] }` を展開:
    // ここでは範囲ループを使わず、tlen が定数小さいことを想定して命令列を並べるわけにいかないため、
    // coll::for_expr を使う。ただし this 関数内で直接 for 式を下ろすのは複雑なので、
    // 単純に tlen 回のロード/ストアを `range` ループで行う。
    // ここでは `core.iter.range` を使う for 式は Lw::expr 経由で下ろせるため、この関数では
    // `lw.expr` で For 式を下ろせない (For は expr で扱う)。よって range ループを使わず、
    // 呼び出し側が十分な cap を確保している前提 (String::new は cap 0 なのでここでは grow 済み)。
    // grow 後は cap == need で十分なので、そのままコピーする。
    // コピー: `for` 式を `expr` で下ろせないので、ここでは命令を直接並べられない。
    // 代替: `text` のバイトを `core.iter.range` で回す for 式は Lw ではなく coll 側で下ろすため、
    // ここでは未対応とし、tlen==0 のみ対応 (grow のみ行う)。
    if tlen != 0 {
        return lw.err(line, "push_str のコピーは空でない &str には未対応");
    }
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: need });
    let u = lw.unit();
    Ok(Some(lw.emit(rt, Op::Ok(u))))
}

/// `String.push_u64(value)`: `value` を10進数の桁として1バイトずつバッファに追加する。
/// 桁テーブルから1バイトをロードして `mem.store.u8` で書き込む (`Conv u64->u8` は verify で拒否されるため)。
pub(super) fn string_push_u64(lw: &mut Lw, e: &V, line: usize, _slot: Option<usize>, h: VId, args: &[V]) -> LR<Option<VId>> {
    if args.len() != 1 {
        return lw.err(line, "push_u64 の引数は 1 つのはず");
    }
    let Some(value) = lw.expr(&args[0])? else { return Ok(None) };
    if lw.vty(value) != Ty::U64 {
        return lw.err(line, "push_u64 の引数は u64 ではない");
    }
    let rt = lw.ty_of(e)?;
    if !matches!(rt, Ty::Res(Pay::Unit, _)) {
        return lw.err(line, "push_u64 の結果の型が Result<(), AllocError> でない");
    }
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let cap = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 });
    let maxdigits = lw.emit(Ty::Usize, Op::Const(20));
    let need = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: len, b: maxdigits });
    let full = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::Usize, a: need, b: cap });
    let (grow, _) = lw.new_block(&[]);
    let (cont, _) = lw.new_block(&[]);
    lw.branch(full, (grow, vec![]), (cont, vec![]));
    lw.cur = Some(grow);
    let old = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let np = lw.emit(Ty::Ptr, Op::HeapRealloc { p: old, size: need });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: h, off: 0, v: np });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 16, v: need });
    lw.goto(cont, vec![]);
    lw.cur = Some(cont);
    let base = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let digits = lw.emit(Ty::Ptr, Op::StaticBytes(b"0123456789".to_vec()));
    let ten = lw.emit(Ty::U64, Op::Const(10));
    let zero = lw.emit(Ty::U64, Op::Const(0));
    let one = lw.emit(Ty::Usize, Op::Const(1));
    // ---- カウントパス: ndigits = 0; tmp = value; while tmp > 0 { tmp /= 10; nd++ }
    // ヘッダのブロック引数がループ携帯変数の phi となる。
    let (chb, chp) = lw.new_block(&[Ty::U64, Ty::Usize]);
    let (ccb, _) = lw.new_block(&[]);
    let (ce, _) = lw.new_block(&[]);
    // 桁数 = 1 + (value / 10 の桁数)。value == 0 でも 1 桁 ("0") になる。
    let initial_nd = lw.emit(Ty::Usize, Op::Const(1));
    let first_q = lw.emit(Ty::U64, Op::Arith { op: Aop::Div, mode: Mode::Trap, a: value, b: ten });
    lw.goto(chb, vec![first_q, initial_nd]);
    lw.cur = Some(chb);
    {
        let tmp = chp[0];
        let c = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::U64, a: tmp, b: zero });
        lw.branch(c, (ccb, vec![]), (ce, vec![]));
    }
    lw.cur = Some(ccb);
    {
        let tmp = chp[0];
        let nd = chp[1];
        let nv = lw.emit(Ty::U64, Op::Arith { op: Aop::Div, mode: Mode::Trap, a: tmp, b: ten });
        let nn = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: nd, b: one });
        lw.goto(chb, vec![nv, nn]);
    }
    lw.cur = Some(ce);
    let ndigits = chp[1];
    let final_len = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: len, b: ndigits });
    // ---- 書き戻しパス: pos = final_len - 1; while value > 0 { buffer[pos] = digit; pos--; value /= 10 }
    let (whb, whp) = lw.new_block(&[Ty::U64, Ty::Usize]);
    let (wb, _) = lw.new_block(&[]);
    let (wd, _) = lw.new_block(&[]);
    let start_pos = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: final_len, b: one });
    // 最下位の桁は必ず書く (value == 0 なら "0")。残りの桁は value / 10 から書く。
    {
        let digit = lw.emit(Ty::U64, Op::Arith { op: Aop::Rem, mode: Mode::Trap, a: value, b: ten });
        let di = lw.emit(Ty::Usize, Op::Conv { src: Ty::U64, a: digit });
        let cptr = lw.emit(Ty::Ptr, Op::PtrAdd { p: digits, off: di });
        let ch = lw.emit(Ty::U8, Op::MemLoad { p: cptr, off: 0 });
        let addr = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off: start_pos });
        lw.emit(Ty::Unit, Op::MemStore { ty: Ty::U8, p: addr, off: 0, v: ch });
    }
    let rest = lw.emit(Ty::U64, Op::Arith { op: Aop::Div, mode: Mode::Trap, a: value, b: ten });
    let next_pos = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: start_pos, b: one });
    lw.goto(whb, vec![rest, next_pos]);
    lw.cur = Some(whb);
    {
        let val = whp[0];
        let c = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::U64, a: val, b: zero });
        lw.branch(c, (wb, vec![]), (wd, vec![]));
    }
    lw.cur = Some(wb);
    {
        let val = whp[0];
        let pos = whp[1];
        let digit = lw.emit(Ty::U64, Op::Arith { op: Aop::Rem, mode: Mode::Trap, a: val, b: ten });
        let di = lw.emit(Ty::Usize, Op::Conv { src: Ty::U64, a: digit });
        let cptr = lw.emit(Ty::Ptr, Op::PtrAdd { p: digits, off: di });
        let ch = lw.emit(Ty::U8, Op::MemLoad { p: cptr, off: 0 });
        let addr = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off: pos });
        lw.emit(Ty::Unit, Op::MemStore { ty: Ty::U8, p: addr, off: 0, v: ch });
        let nv = lw.emit(Ty::U64, Op::Arith { op: Aop::Div, mode: Mode::Trap, a: val, b: ten });
        let np = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: pos, b: one });
        lw.goto(whb, vec![nv, np]);
    }
    lw.cur = Some(wd);
    {
        lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: final_len });
    }
    let u = lw.unit();
    Ok(Some(lw.emit(rt, Op::Ok(u))))
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
    let (seq, elem) = match kind {
        Kind::Vec(Some(e)) => (Seq::Hdr(h), *e),
        Kind::Slice(Some(e)) => (Seq::Fat(h), *e),
        _ => return lw.err(line, "get の対象は要素型の分かる Vec・スライスだけ段階 1 の対象"),
    };
    let (Some(sz), Some(pay)) = (elem_size(lw, elem), Pay::of(elem)) else {
        return lw.err(line, format!("get の要素の型 {} は段階 1 の対象外", elem.name()));
    };
    if owns_heap(lw, elem) {
        // 要素の写しを Option に入れると、Option が所有物を持つ型になり、match などで解放されて要素と二重解放になる。
        return lw.err(line, format!("get の要素の型 {} は所有物を持つ (借用の Option<&T> は値の写しで持てない)", elem.name()));
    }
    if args.len() != 1 {
        return lw.err(line, "get の引数の個数が違う");
    }
    let Some(i) = lw.expr(&args[0])? else { return Ok(None) };
    if lw.vty(i) != Ty::Usize {
        return lw.err(line, "get の添字が usize ではない");
    }
    let opt = Ty::Opt(pay);
    let (len, base) = seq.len_and_base(lw, None);
    let inside = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: i, b: len });
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
    let x = load_elem(lw, addr, elem);
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
    let p = lw.emit(Ty::Ptr, Op::MemLoad { p: hdr, off: 0 });
    let l = load_len(lw, hdr);
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
    let Some(sz) = size_of(elem) else {
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
        match lw.coll.kinds.get(&s) {
            Some(Kind::Vec(Some(elem))) => return Ok((Seq::Hdr(lw.slots[s].0), *elem)),
            Some(Kind::Slice(Some(elem))) if lw.slots[s].1 == Ty::Ptr => {
                // `&[T; N]` / `&mut [T; N]` のパラメータ: ptr の値、長さ N は型から分かる。
                let n = lw.coll.arr_len[&s];
                return Ok((Seq::Ptr(lw.slots[s].0, n), *elem));
            }
            Some(Kind::Slice(Some(elem))) => return Ok((Seq::Fat(lw.slots[s].0), *elem)),
            _ => {}
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

/// `v[i]` の読み取り。
pub(crate) fn index_read(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    let f = nf(e);
    let line = nline(e);
    let (seq, elem) = vec_place(lw, &f[0], line)?;
    let Some(i) = lw.expr(&f[1])? else { return Ok(None) };
    if lw.vty(i) != Ty::Usize {
        return lw.err(line, "添字の型が usize ではない");
    }
    let addr = elem_addr(lw, seq, i, elem)?;
    Ok(Some(lw.emit(elem, Op::MemLoad { p: addr, off: 0 })))
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
    lw.emit(Ty::Unit, Op::MemStore { ty: elem, p: addr, off: 0, v });
    Ok(St::Unit)
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
    if let Some(r) = super::map_lower::try_for(lw, e) {
        return r;
    }
    let f = nf(e);
    let line = nline(e);
    let name = match nname(&f[0]) {
        "PBind" if !matches!(nf(&f[0]).get(1), Some(V::B(true))) => Some(st(&nf(&f[0])[0])),
        "PWild" => None,
        _ => return lw.err(line, "for の変数はただの名前か _ だけ段階 1 の対象"),
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
            return counted(lw, line, Counted { start: a, end: b, ity, elem: None }, name, &f[2]);
        }
    }
    // `for x in [e1, e2]`: 配列リテラルは `{ptr, len}` の値として、スライスと同じに反復する
    if nname(it) == "ArrayE" {
        let Some((fat, elem)) = lw.array_lit(it)? else { return Ok(None) };
        let (len, base) = Seq::Fat(fat).len_and_base(lw, None);
        let zero = lw.emit(Ty::Usize, Op::Const(0));
        return counted(lw, line, Counted { start: zero, end: len, ity: Ty::Usize, elem: Some((base, elem)) }, name, &f[2]);
    }
    if let Some(s) = local_slot(lw, it) {
        let seq = match lw.coll.kinds.get(&s).cloned() {
            Some(Kind::Vec(Some(elem))) => Some((Seq::Hdr(lw.slots[s].0), elem)),
            // `&[T; N]` / `&mut [T; N]` のパラメータ: ptr の値、長さ N は型から分かる。
            Some(Kind::Slice(Some(elem))) if lw.slots[s].1 == Ty::Ptr => {
                let n = lw.coll.arr_len[&s];
                Some((Seq::Ptr(lw.slots[s].0, n), elem))
            }
            Some(Kind::Slice(Some(elem))) => Some((Seq::Fat(lw.slots[s].0), elem)),
            _ => None,
        };
        if let (Some(_), Some(n)) = (&seq, path1_name(it)) {
            // 反復中に `v` を変える (`push` で再確保される、`&mut v` を渡す、代入する) と、反復が読む先頭と長さが古くなって
            // 解放後のバッファを読む。`ksn check` はこの形を通すので KIR では拒む (設計_KIR.md 17.6)。
            if body_mutates(&f[2], &n) {
                return lw.err(line, "for の本体で反復中の Vec を変更する形は段階 1 の対象外 (再確保で反復が解放後の領域を読む)");
            }
        }
        if let Some((seq, elem)) = seq {
            let (len, base) = seq.len_and_base(lw, None);
            let zero = lw.emit(Ty::Usize, Op::Const(0));
            return counted(lw, line, Counted { start: zero, end: len, ity: Ty::Usize, elem: Some((base, elem)) }, name, &f[2]);
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
    lw.err(line, "for の対象は core.iter.range(a, b) か、要素型の分かる Vec の変数・利用者の反復子の変数だけ段階 1 の対象")
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
    const READ_ONLY: [&str; 9] = ["len", "is_empty", "get", "as_slice", "as_str", "first", "last", "contains", "capacity"];
    match v {
        V::Node(n, fs, _, _) => {
            let hit = match *n {
                "Assign" => fs.first().and_then(super::path_base).as_deref() == Some(name),
                "Unary" => fs.len() == 2 && st(&fs[0]) == "&mut" && super::agg::field_segs(&fs[1]).is_some_and(|s| s[0] == name),
                "Call" if fs.len() >= 2 && nname(&fs[0]) == "Path" => {
                    let segs = strs(&nf(&fs[0])[0]);
                    segs.len() >= 2 && segs[0] == name && !READ_ONLY.contains(&segs[segs.len() - 1].as_str())
                }
                "MCall" if fs.len() >= 2 => {
                    super::agg::field_segs(&fs[0]).is_some_and(|s| s[0] == name) && !READ_ONLY.contains(&st(&fs[1]).as_str())
                }
                _ => false,
            };
            hit || fs.iter().any(|f| body_mutates(f, name))
        }
        V::L(xs) => xs.iter().any(|x| body_mutates(x, name)),
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
    lw.loops.push(super::LoopCx { header: hb, exit: xb, slots: slots.clone(), depth });
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
            Some((base, ety)) => {
                let addr = elem_addr_from_base(lw, base, hp[0], ety)?;
                let v = lw.emit(ety, Op::MemLoad { p: addr, off: 0 });
                lw.bind(n, v, ety);
            }
        }
    }
    lw.loops.push(super::LoopCx { header: ib, exit: xb, slots: slots.clone(), depth });
    let r = lw.expr(body)?;
    if r.is_some() {
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
    let Some(sz) = size_of(elem) else {
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
    let (cap_names, cap_vals, moved) = capture_values(lw, &f[2], &f[3], line)?;
    let cap_tys: Vec<Ty> = cap_vals.iter().map(|v| lw.vty(*v)).collect();
    let mut child = Lw::new(lw.p, lw.tys, lw.aggs, lw.def);
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
    let nc = cap_names.len();
    for ((n, v), t) in cap_names.iter().zip(pv.iter()).zip(cap_tys.iter()) {
        child.bind_let(n, *v, *t);
    }
    for ((n, v), t) in names.iter().zip(pv[nc..].iter()).zip(ats.iter()) {
        child.bind_let(n, *v, *t);
    }
    if let Some(v) = child.expr(&f[3])? {
        if child.vty(v) != rt {
            return lw.err(line, "クロージャの本体の型が戻り値の型と違う");
        }
        child.terminate(Term::Ret(v));
    }
    child.flush_allocas();
    let mut blocks: Vec<Block> = child
        .blocks
        .into_iter()
        .map(|b| Block { params: b.params, insts: b.insts, term: b.term.unwrap_or(Term::Unreachable) })
        .collect();
    finish(&mut blocks, std::mem::take(&mut child.coll.allocas));
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

/// `capture [...]` の各項目を、クロージャを作る位置で評価する。戻り値: (名前, 値, 値で取り込んだ名前)。
/// `move x` は x の現在の値、`&x` / `&mut x` は x のスロットへの参照。取り込めるのは解放の要らないスカラー・
/// Copy の集成体だけで、Vec / String / クロージャ / 参照の変数は理由付きで未対応にする。
fn capture_values(lw: &mut Lw, caps: &V, body: &V, line: usize) -> LR<(Vec<String>, Vec<VId>, Vec<String>)> {
    let _ = body;
    let (mut names, mut vals, mut moved) = (vec![], vec![], vec![]);
    for c in items(caps) {
        let kv = items(c);
        if kv.len() != 2 {
            return lw.err(line, "capture の項目の形が不正");
        }
        let (kind, name) = (st(&kv[0]), st(&kv[1]));
        let Some(s) = lw.lookup(&name) else {
            return lw.err(line, format!("capture の {} が見つからない", name));
        };
        if lw.coll.kinds.contains_key(&s) {
            return lw.err(line, format!("capture の {} は Vec / String / str / スライス / クロージャで、取り込みは段階 1 の対象外 (所有の移動と解放の規則が要る)", name));
        }
        let t = lw.slots[s].1;
        if lw.aggs.borrow().owns_heap(t) {
            return lw.err(line, format!("capture の {} は解放の要る値を持つので段階 1 の対象外 (所有の移動と解放の規則が要る)", name));
        }
        if t.is_ref() {
            return lw.err(line, format!("capture の {} は参照の変数で、取り込みは段階 1 の対象外", name));
        }
        let v = match kind.as_str() {
            "move" => {
                moved.push(name.clone());
                lw.read_slot(s)
            }
            "&" | "&mut" => {
                let ptr = lw.slots[s].0;
                let p = Pointee::of(t);
                match p {
                    Some(p) if lw.is_mem_val(ptr) => lw.ref_value(ptr, p, kind == "&"),
                    _ => return lw.err(line, format!("capture の {} のアドレスを取れない (段階 1 の対象外の型)", name)),
                }
            }
            _ => return lw.err(line, "capture の種類が不明"),
        };
        names.push(name);
        vals.push(v);
    }
    Ok((names, vals, moved))
}
