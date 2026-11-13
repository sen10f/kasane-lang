//! 型表付きの AST から KIR を作る (設計_KIR.md 2 章、段階 1 の下ろし)。
//!
//! 入力は推論済みの `Program` と、`TypeCk` の型表 (式 ID → 型、第 9 章)。型表の型だけを使い、
//! 型を推測しない。対象外の構文・型・呼び出しに出会った関数は理由を付けて「未対応」にし、
//! その関数を呼ぶ関数も (呼び先が無くなるので) 未対応にする。
//!
//! 変数は「スロット」で持つ。スロットは現在の値 (`VId`) と型を持ち、ブロックをまたぐ変化
//! (代入) は、合流点・ループの先頭・脱出先のブロック引数で渡す。代入される変数の集合は
//! 構文から求め、その集合だけを引数にする。

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::check::{is_nil, items, nf, nlineno as nline, nname, st, strs};
use crate::kir::{self, Aop, Block, BoolOp, Cc, Func, Inst, Mode, Module, Op, Term, Ty, VId};
use crate::parser::{nid, V};
use crate::program::{Def, Program, Target as Tgt};
use crate::types::{from_v, ExprTy, Ty as Src, TypeCk};

/// 数値の下ろし (浮動小数・128 ビット・ビット演算・数値のメソッド)。子モジュールとして `Lw` の非公開の関数を使う。
#[path = "kir_lower_scalar.rs"]
mod scalar;
#[path = "kir_lower_refs.rs"]
mod refs;
use refs::RefCx;
// `unsafe` ブロック、`extern` の呼び出し、`core.ptr.*` の下ろし。
#[path = "kir_lower_ffi.rs"]
mod ffi_lower;
// 組み込みの Option / Result の下ろし (子モジュール。親の非公開の項目を使う)。
#[path = "kir_lower_enum.rs"]
mod enum_lower;
// 文字列・バイト列の操作の下ろし (kir_lower_str.rs)。
#[path = "kir_lower_str.rs"]
mod str_lower;
// panic (`core.panic.panic("..")` と `unwrap`) の下ろし (子モジュール)。
#[path = "kir_lower_panic.rs"]
mod panic_lower;
// `core.check.eq` / `core.check.is_true` の下ろし (子モジュール。設計_KIR.md 13.2)。
#[path = "kir_lower_check.rs"]
mod check_lower;
// 利用者の型のメソッド呼び出し (`&mut self` の受け手を含む) の下ろし (子モジュール)。
#[path = "kir_lower_method.rs"]
mod method_lower;
// `std.thread.spawn` / `JoinHandle.join` の下ろし (子モジュール)。
#[path = "kir_lower_thread.rs"]
mod thread_lower;
// 配列リテラルの下ろし (子モジュール)。
#[path = "kir_lower_array.rs"]
mod array_lower;

// コレクション・文字列・クロージャの下ろし (子モジュール。`Lw` の非公開の項目を使う)。
#[path = "kir_lower_iter.rs"]
mod iter_lower;
#[path = "kir_lower_map.rs"]
mod map_lower;
#[path = "kir_lower_coll.rs"]
mod coll;
#[path = "kir_lower_for.rs"]
mod for_lower;
#[path = "kir_lower_own.rs"]
mod own;
use coll::CollCx;

// `std.net` (TCP) の下ろし (設計_KIR.md 12 章)。
#[path = "kir_lower_net.rs"]
mod net_lower;
use net_lower::{is_net_ty, net_call, net_method};

type LR<T> = Result<T, String>;

// 集成体 (構造体・タプル・列挙型) の下ろし。子モジュールなので、この file の非公開の部品を使う。
#[path = "kir_lower_agg.rs"]
mod agg;

// `derive [Debug]` の表示の関数 (入口の `main` の `Err` の表示に使う。設計_KIR.md 12.3)。
#[path = "kir_debug.rs"]
pub mod debug;

/// 下ろしの結果: 対象になった関数と、未対応の関数 (鍵, 理由)。
pub struct Lowered {
    pub module: Module,
    pub unsupported: Vec<(String, String)>,
}

/// 型表を作る。ユーザの関数だけを検査し、(FQN, 式 ID) → 型 を返す。
/// 型表を作る。`test_def` (テストを入口にするとき) は、テストの本体も検査する。
fn type_table(p: &Program, user: &[usize], test_def: Option<usize>) -> BTreeMap<(String, u32), ExprTy> {
    let mut tk = TypeCk::new(p);
    for &i in user {
        let d = &p.defs[i];
        if (d.kw == "fn" && d.body.is_some()) || test_def == Some(i) {
            tk.check_def(i);
        }
    }
    tk.tytab
}

fn is_candidate(d: &Def) -> bool {
    // ライフタイムだけの宣言は型引数ではない (型は変えない)。型引数があるものは単相化していないので対象外。
    // メソッド (`self` を持つ関数) も対象にする。呼ぶ側は `method_lower.rs` が型表から呼び先を決める。
    d.kw == "fn" && d.body.is_some() && d.generics.iter().all(|g| g.0 == "life")
}

/// 推論済みの型を段階 1 の型へ写す。対象外の型は `None`。
fn conv(t: &Src) -> Option<Ty> {
    match t {
        // `char` は Unicode のスカラー値を `u32` で持つ (KIR に char の型はない。Debug 表示は元の型で区別する。設計_KIR.md 12.3)。
        Src::Prim(s) if s == "char" => Some(Ty::U32),
        Src::Prim(s) => Ty::parse(s).filter(|t| *t != Ty::Unit),
        Src::Unit => Some(Ty::Unit),
        Src::Ref(m, x) => refs::conv_ref(*m, x),
        // `Option<&スカラー>` (`get` の結果) は値のペイロードで持つ (設計_KIR.md 10.10)。
        Src::Named(n, a) if n == "Option" && a.len() == 1 && matches!(&a[0], Src::Ref(false, i) if matches!(&**i, Src::Prim(_))) => {
            let Src::Ref(_, i) = &a[0] else { return None };
            conv(i).and_then(kir::Pay::of).map(Ty::Opt)
        }
        _ => enum_lower::conv_builtin(t),
    }
}

/// 引数・戻り値の型が対象外のときの理由。型の写し (`conv_t_why`) が理由を出せばそれ、出せなければ値渡しの所有物 (Vec / String) の型。
fn sig_why(lw: &Lw, t: &Src) -> String {
    match lw.conv_t_why(t) {
        Err(why) => why,
        Ok(_) => "値渡しの Vec / String を持つ型は未対応 (所有の移動)".to_string(),
    }
}

fn show(t: &Src) -> String {
    match t {
        Src::Prim(s) => s.clone(),
        Src::Unit => "()".into(),
        Src::Never => "!".into(),
        Src::Unknown => "未確定".into(),
        Src::Var(_) => "型変数".into(),
        Src::Param(p) => format!("型パラメータ {}", p),
        Src::Named(n, a) if a.is_empty() => n.clone(),
        Src::Named(n, a) => format!("{}<{}>", n, a.iter().map(show).collect::<Vec<_>>().join(", ")),
        Src::Tuple(xs) => format!("({})", xs.iter().map(show).collect::<Vec<_>>().join(", ")),
        Src::Ref(m, x) => format!("&{}{}", if *m { "mut " } else { "" }, show(x)),
        Src::Slice(x) => format!("[{}]", show(x)),
        other => format!("{:?}", other),
    }
}

/// 代入の対象の名前を、木の中から (出現順に) 集める。`x = ...` の `x` だけを見る。
fn scan_assigns(v: &V, out: &mut Vec<String>) {
    match v {
        V::Node(n, fs, _, _) => {
            if *n == "Assign" {
                if let Some(name) = fs.first().and_then(path_base) {
                    out.push(name);
                }
            }
            for f in fs {
                scan_assigns(f, out);
            }
        }
        V::L(xs) => {
            for x in xs {
                scan_assigns(x, out);
            }
        }
        _ => {}
    }
}

/// 単一の名前だけの経路 (`x`) なら、その名前。
fn path1(v: &V) -> Option<String> {
    if nname(v) != "Path" {
        return None;
    }
    let segs = strs(&nf(v)[0]);
    (segs.len() == 1).then(|| segs[0].clone())
}

/// 経路の先頭の名前 (`x` も `x.f.g` も `x`)。代入の対象の局所変数を求める。
fn path_base(v: &V) -> Option<String> {
    if nname(v) != "Path" {
        return None;
    }
    strs(&nf(v)[0]).first().cloned()
}

/// 文の結果。`Div` は制御がそこで終わった (以降は到達しない) ことを表す。
enum St {
    Div,
    Unit,
    Val(VId),
}

struct BlockB {
    params: Vec<(VId, Ty)>,
    insts: Vec<Inst>,
    term: Option<Term>,
    /// この block へ入る辺の数 (0 なら到達しない)。
    preds: usize,
}

struct LoopCx {
    header: usize,
    exit: usize,
    slots: Vec<usize>,
    /// break / continue が後始末する最初のスコープの番号 (これより内側の所有値を解放する)。
    depth: usize,
}

struct Lw<'a> {
    p: &'a Program,
    tys: &'a BTreeMap<(String, u32), ExprTy>,
    def: &'a Def,
    /// 集成体の型表 (全関数で共有。`lower_user` が作る)。
    aggs: &'a RefCell<agg::AggTab>,
    blocks: Vec<BlockB>,
    cur: Option<usize>,
    /// 値番号 → 型 (VId は 0 から順に振る)。
    vty: Vec<Ty>,
    /// スロット → (現在の値, 型)。
    slots: Vec<(VId, Ty)>,
    scopes: Vec<Vec<(String, usize)>>,
    loops: Vec<LoopCx>,
    callees: BTreeSet<String>,
    ret: Ty,
    /// 参照の下ろしの状態 (kir_lower_refs.rs)。
    rf: RefCx,
    /// コレクション・文字列・クロージャの下ろし (kir_lower_coll.rs) の状態。
    coll: CollCx,
}

impl<'a> Lw<'a> {
    fn new(p: &'a Program, tys: &'a BTreeMap<(String, u32), ExprTy>, aggs: &'a RefCell<agg::AggTab>, def: &'a Def) -> Lw<'a> {
        Lw {
            p,
            tys,
            def,
            aggs,
            blocks: vec![],
            cur: None,
            vty: vec![],
            slots: vec![],
            scopes: vec![],
            loops: vec![],
            callees: BTreeSet::new(),
            ret: Ty::Unit,
            rf: RefCx::default(),
            coll: CollCx::default(),
        }
    }

    // ------------------------------------------------ 基本操作

    fn err<T>(&self, line: usize, msg: impl Into<String>) -> LR<T> {
        Err(format!("行 {}: {}", line, msg.into()))
    }

    fn new_val(&mut self, t: Ty) -> VId {
        self.vty.push(t);
        (self.vty.len() - 1) as VId
    }

    /// ブロックを作る。引数の値番号も返す。
    fn new_block(&mut self, params: &[Ty]) -> (usize, Vec<VId>) {
        let ids: Vec<VId> = params.iter().map(|t| self.new_val(*t)).collect();
        let ps = ids.iter().copied().zip(params.iter().copied()).collect();
        self.blocks.push(BlockB { params: ps, insts: vec![], term: None, preds: 0 });
        (self.blocks.len() - 1, ids)
    }

    /// 現在のブロックに命令を置く。`cur` が無いのは下ろしの不変条件の違反なので、ここでは止める。
    fn emit(&mut self, ty: Ty, op: Op) -> VId {
        let v = self.new_val(ty);
        let cur = self.cur.expect("KIR 下ろしの内部不変条件: 到達しないブロックへ命令を置いた");
        self.blocks[cur].insts.push(Inst { dst: v, ty, op });
        v
    }

    fn unit(&mut self) -> VId {
        self.emit(Ty::Unit, Op::Const(0))
    }

    fn terminate(&mut self, t: Term) {
        let cur = self.cur.take().expect("KIR 下ろしの内部不変条件: 終端命令を二重に置いた");
        self.blocks[cur].term = Some(t);
    }

    fn goto(&mut self, t: usize, args: Vec<VId>) {
        self.blocks[t].preds += 1;
        self.terminate(Term::Br((t, args)));
    }

    fn branch(&mut self, c: VId, a: (usize, Vec<VId>), b: (usize, Vec<VId>)) {
        self.blocks[a.0].preds += 1;
        self.blocks[b.0].preds += 1;
        self.terminate(Term::BrIf(c, a, b));
    }

    fn vty(&self, v: VId) -> Ty {
        self.vty[v as usize]
    }

    fn new_slot(&mut self, v: VId, t: Ty) -> usize {
        self.slots.push((v, t));
        let s = self.slots.len() - 1;
        // 枝の終わりで `slots` を切り詰めるので、番号は再利用される。前の持ち主の種類・借用の印は消す。
        self.coll.kinds.remove(&s);
        self.coll.borrowed.remove(&s);
        s
    }

    fn bind(&mut self, name: &str, v: VId, t: Ty) {
        let s = self.new_slot(v, t);
        self.scopes.last_mut().expect("スコープ").push((name.to_string(), s));
    }

    /// `match` の振り分け: 対象が `Option` / `Result` なら `match_` (タグの判定)、それ以外は `match_e` (集成体・スカラーの腕)。
    fn match_any(&mut self, e: &V) -> LR<Option<VId>> {
        let scrut = nf(e)[0].clone();
        if let Some(r) = own::match_get_mut(self, e) {
            return r;
        }
        let stt = self.src_ty(&scrut)?;
        match stt {
            Src::Named(n, _) if n == "Option" || n == "Result" => self.match_(e),
            _ => self.match_e(e),
        }
    }

    fn lookup(&self, name: &str) -> Option<usize> {
        for sc in self.scopes.iter().rev() {
            if let Some((_, s)) = sc.iter().rev().find(|(n, _)| n == name) {
                return Some(*s);
            }
        }
        None
    }

    /// `&str` とスライスの値の型 (`{ptr, len}`)。型表に登録する。
    fn fat_ty(&self) -> Ty {
        self.aggs.borrow_mut().fat()
    }

    fn vals_of(&self, slots: &[usize]) -> Vec<VId> {
        slots.iter().map(|s| self.slots[*s].0).collect()
    }

    /// 木の中で代入される (現在の有効範囲で見える) スロットを、出現順に集める。
    fn assigned(&self, nodes: &[&V]) -> Vec<usize> {
        let mut names = vec![];
        for n in nodes {
            scan_assigns(n, &mut names);
        }
        names.extend(self.mut_recv_names(nodes));
        let mut out: Vec<usize> = vec![];
        for n in names {
            if let Some(s) = self.lookup(&n) {
                if !out.contains(&s) && !self.rf.is_mem(self.slots[s].0) {
                    out.push(s);
                }
            }
        }
        out
    }

    // ------------------------------------------------ 型表

    fn src_ty(&self, e: &V) -> LR<Src> {
        let line = nline(e);
        let id = nid(e);
        match self.tys.get(&(self.def.name.clone(), id)) {
            Some(x) if id != 0 => Ok(x.ty.clone()),
            _ => self.err(line, format!("式 {} の型が決まらない (型表にない)", nname(e))),
        }
    }

    /// 式の型 (段階 1 の型)。対象外・未確定なら理由付きで止める。
    fn ty_of(&self, e: &V) -> LR<Ty> {
        // 型表が未確定の局所変数 (`for` の変数など) は、スロットの型で補う。
        let t = match self.src_ty(e) {
            Ok(t) if t != Src::Unknown => t,
            r => match coll::local_slot_ty(self, e) {
                Some(x) => return Ok(x),
                None => r?,
            },
        };
        if let Some(x) = coll::value_ty(self, &t) {
            return Ok(x);
        }
        // `Some(c)` で束縛した変数の型表は `&スカラー` のまま。値で持つ局所変数は、スロットの型 (スカラー) を使う。
        if let Src::Ref(false, inner) = &t {
            if let (Some(x), Some(want)) = (coll::local_slot_ty(self, e), conv(inner)) {
                if x == want {
                    return Ok(x);
                }
            }
        }
        match self.conv_t(&t) {
            Some(x) => Ok(x),
            None => self.err(nline(e), format!("型 {} は段階 1 の対象外", show(&t))),
        }
    }

    // ------------------------------------------------ 関数全体

    fn lower(mut self) -> LR<Vec<(Func, BTreeSet<String>)>> {
        let d = self.def;
        let gn = HashSet::new();
        let body = d.body.as_ref().expect("候補は本体を持つ");
        let mut params = vec![];
        for (_, t) in &d.params {
            let st = from_v(t, &gn);
            match coll::sig_ty(&self, &st) {
                Some(x) => params.push(x),
                None => return Err(format!("引数の型 {} は段階 1 の対象外 ({})", show(&st), sig_why(&self, &st))),
            }
        }
        let ret = if is_nil(&d.ret) {
            Ty::Unit
        } else {
            let st = from_v(&d.ret, &gn);
            match coll::sig_ty(&self, &st) {
                Some(x) => x,
                None => return Err(format!("戻り値の型 {} は段階 1 の対象外 ({})", show(&st), sig_why(&self, &st))),
            }
        };
        self.ret = ret;
        let (b0, pv) = self.new_block(&params);
        debug_assert_eq!(b0, 0);
        self.cur = Some(0);
        self.rf.begin(body);
        self.scopes.push(vec![]);
        for ((name, pt), (v, t)) in d.params.iter().zip(pv.iter().zip(params.iter())) {
            coll::bind_param(&mut self, name, *v, *t, &from_v(pt, &gn));
            // `&[T; N]` / `&mut [T; N]` のパラメータ: 長さ N は型から分かる (ptr にするだけ)。
            if let Some(n) = coll::ref_array_len(pt) {
                let s = self.lookup(name).expect("直後に束縛したパラメータ");
                self.coll.arr_len.insert(s, n);
            }
        }
        if let Some(v) = self.expr(body)? {
            if self.vty(v) != ret {
                return Err("本体の値の型が戻り値の型と合わない".into());
            }
            coll::drop_from(&mut self, 0);
            self.terminate(Term::Ret(v));
        }
        let mut uses = d.uses.clone();
        uses.sort();
        uses.dedup();
        self.flush_allocas();
        let mut blocks: Vec<Block> = self
            .blocks
            .into_iter()
            .map(|b| Block { params: b.params, insts: b.insts, term: b.term.unwrap_or(Term::Unreachable) })
            .collect();
        coll::finish(&mut blocks, std::mem::take(&mut self.coll.allocas));
        let f = Func {
            fqn: d.name.clone(),
            key: d.name.clone(),
            uses,
            may_panic: d.panics.as_deref() != Some("never"),
            stack: d.stack,
            ret,
            link: None,
            blocks,
        };
        let mut out = std::mem::take(&mut self.coll.extra);
        out.insert(0, (f, self.callees));
        Ok(out)
    }

    // ------------------------------------------------ 式

    /// 式を下ろす。`None` は制御がその先へ進まないこと (return / break / continue など) を表す。
    fn expr(&mut self, e: &V) -> LR<Option<VId>> {
        match nname(e) {
            "Lit" => self.lit(e).map(Some),
            "Path" => self.path_val(e).map(Some),
            "Bin" => self.bin(e),
            "Unary" => self.unary(e),
            "Call" => self.call(e),
            "MCall" => self.mcall(e),
            "Block" => self.block(e),
            "If" => self.if_(e),
            "While" => self.while_(e),
            "Loop" => self.loop_(e),
            "Break" => self.jump_loop(e, true),
            "Continue" => self.jump_loop(e, false),
            "Return" => self.ret_(e),
            "StructLit" | "TupleE" | "Field" => self.agg_expr(e),
            "Unsafe" => ffi_lower::unsafe_block(self, e),
            "Match" => self.match_any(e),
            "Try" => self.try_(e),
            "Index" => coll::index_read(self, e),
            "For" => coll::for_expr(self, e),
            "Closure" => coll::closure_lit(self, e),
            "ArrayE" => self.array_lit(e).map(|o| o.map(|(v, _)| v)),
            other => self.err(nline(e), format!("式 {} は段階 1 の対象外", other)),
        }
    }

    /// 式を下ろす。単一の名前の局所変数 (所有物の印を持つもの) は、移さずにそのまま読む (場所としての使用)。
    fn expr_peek(&mut self, e: &V) -> LR<Option<VId>> {
        self.coll.peek_field = false;
        if let Some(s) = coll::val_slot(self, e) {
            return Ok(Some(coll::path_peek(self, s)));
        }
        // 局所変数から始まるフィールドの連なり: 所有物でも移さずに読む (`match` の対象にするとき)
        if let Some((s, names)) = own::chain_of(self, e) {
            if !names.is_empty() && matches!(nname(e), "Path" | "Field") {
                let base = self.read_slot(s);
                if let Ok(v) = self.field_path(base, &names, nline(e)) {
                    if coll::owns(self, self.vty(v)) {
                        self.coll.peek_field = true;
                    }
                    return Ok(Some(v));
                }
            }
        }
        self.expr(e)
    }

    fn lit(&mut self, e: &V) -> LR<VId> {
        let f = nf(e);
        let line = nline(e);
        match st(&f[0]).as_str() {
            "int" => {
                let ty = self.ty_of(e)?;
                if !ty.is_int() {
                    return self.err(line, "整数リテラルの型が整数ではない");
                }
                let v = parse_int(&f[1]).ok_or_else(|| format!("行 {}: 整数リテラルが扱えない", line))?;
                if !ty.in_range(v) {
                    return self.err(line, format!("整数リテラルが {} の範囲にない", ty.name()));
                }
                Ok(self.emit(ty, Op::Const(v)))
            }
            "bool" => {
                let b = matches!(f[1], V::B(true));
                Ok(self.emit(Ty::Bool, Op::Const(b as i128)))
            }
            "unit" => Ok(self.unit()),
            "char" => {
                let v = parse_int(&f[1]).ok_or_else(|| format!("行 {}: 文字リテラルが扱えない", line))?;
                Ok(self.emit(Ty::U32, Op::Const(v)))
            }
            "float" => self.float_lit(e),
            // `b'a'` は u8 の定数 (字句で値に直してある)
            "byte" => {
                let v = parse_int(&f[1]).ok_or_else(|| format!("行 {}: バイトリテラルが扱えない", line))?;
                Ok(self.emit(Ty::U8, Op::Const(v)))
            }
            "str" => coll::str_lit(self, e),
            // `b"..."` は静的なバイト列と長さから `fat` (`{ptr, len}`) を作る。
            "bstr" => self.str_lit(e),
            other => self.err(line, format!("リテラル {} は段階 1 の対象外", other)),
        }
    }

    fn path_val(&mut self, e: &V) -> LR<VId> {
        let segs = strs(&nf(e)[0]);
        let line = nline(e);
        if segs.len() == 1 {
            if let Some(s) = self.lookup(&segs[0]) {
                return Ok(coll::path_read(self, s));
            }
            if segs[0] == "None" {
                return self.none_val(e);
            }
        }
        // `x.f.g`: 局所変数 x のフィールドの読み出し
        if segs.len() > 1 {
            if let Some(s) = self.lookup(&segs[0]) {
                let base = self.read_slot(s);
                let v = self.field_path(base, &segs[1..], line)?;
                if coll::owns(self, self.vty(v)) {
                    // 所有物のフィールドを値として使う: 部分的な移動 (設計_KIR.md 13.7)
                    return coll::field_move(self, s, &segs[1..], line);
                }
                return Ok(v);
            }
        }
        if !matches!(nf(e).get(1), Some(V::Nil) | None) {
            return self.err(line, "型引数付きの名前は段階 1 の対象外");
        }
        let resolved = self.p.resolve(&segs, &self.def.file);
        if matches!(&resolved, Some(r) if !r.rest.is_empty()) {
            return self.err(line, format!("名前 {} は段階 1 の対象外 (名前の一部だけが解決される)", segs.join(".")));
        }
        match resolved.map(|r| r.target) {
            Some(Tgt::Const(ci)) => self.const_val(ci, e),
            Some(Tgt::Variant(en, vn)) => self.variant_value(&en, &vn, line),
            _ => self.err(line, format!("名前 {} は段階 1 の対象外", segs.join("."))),
        }
    }

    /// 整数か bool のリテラルで書かれた定数 (`const f.N: u64 = 27u64`) だけを値として扱う。
    fn const_val(&mut self, ci: usize, e: &V) -> LR<VId> {
        let c = &self.p.defs[ci];
        let ty = self.ty_of(e)?;
        match c.init.as_ref() {
            Some(init) if nname(init) == "Lit" && st(&nf(init)[0]) == "int" && ty.is_int() => {
                let v = parse_int(&nf(init)[1]).ok_or_else(|| "定数の値が扱えない".to_string())?;
                if !ty.in_range(v) {
                    return self.err(nline(e), "定数の値が型の範囲にない");
                }
                Ok(self.emit(ty, Op::Const(v)))
            }
            _ => self.err(nline(e), format!("定数 {} は段階 1 の対象外", c.name)),
        }
    }

    fn bin(&mut self, e: &V) -> LR<Option<VId>> {
        if let Some(done) = self.scalar_bin(e)? {
            return Ok(done);
        }
        let f = nf(e);
        let line = nline(e);
        let op = st(&f[0]);
        let (l, r) = (&f[1], &f[2]);
        match op.as_str() {
            "&&" | "||" => self.short_circuit(op == "||", l, r, line),
            "==" | "!=" | "<" | "<=" | ">" | ">=" => {
                let Some(a) = self.expr(l)? else { return Ok(None) };
                let Some(b) = self.expr(r)? else { return Ok(None) };
                // `Some(c) => c == &y` の `&y` は読んでから比べる。比べる型は emit した値の型で決める。
                let (a, b) = self.load_scalar_ref_pair(a, b);
                let t = self.vty(a);
                if self.vty(b) != t {
                    return self.err(line, "比較の両辺の型が違う");
                }
                let cc = match op.as_str() {
                    "==" => Cc::Eq,
                    "!=" => Cc::Ne,
                    "<" => Cc::Lt,
                    "<=" => Cc::Le,
                    ">" => Cc::Gt,
                    _ => Cc::Ge,
                };
                if t.is_ref() || t == Ty::Unit || (t == Ty::Bool && !matches!(cc, Cc::Eq | Cc::Ne)) {
                    return self.err(line, format!("型 {} の比較は段階 1 の対象外", t.name()));
                }
                Ok(Some(self.emit(Ty::Bool, Op::Cmp { cc, ty: t, a, b })))
            }
            "+" | "-" | "*" | "/" | "%" => {
                let t = self.ty_of(e)?;
                if !t.is_int() {
                    return self.err(line, "算術の型が整数ではない");
                }
                if !self.def.panics.as_deref().map_or(true, |p| p == "may") {
                    return self.err(line, "panics never の関数に整数演算子 (溢れの検査が要る)");
                }
                let aop = match op.as_str() {
                    "+" => Aop::Add,
                    "-" => Aop::Sub,
                    "*" => Aop::Mul,
                    "/" => Aop::Div,
                    _ => Aop::Rem,
                };
                let Some(a) = self.expr(l)? else { return Ok(None) };
                let Some(b) = self.expr(r)? else { return Ok(None) };
                Ok(Some(self.emit(t, Op::Arith { op: aop, mode: Mode::Trap, a, b })))
            }
            "&" | "|" | "^" => {
                let t = self.ty_of(e)?;
                if t != Ty::Bool {
                    return self.err(line, "整数のビット演算は段階 1 の対象外");
                }
                let bop = match op.as_str() {
                    "&" => BoolOp::And,
                    "|" => BoolOp::Or,
                    _ => BoolOp::Xor,
                };
                let Some(a) = self.expr(l)? else { return Ok(None) };
                let Some(b) = self.expr(r)? else { return Ok(None) };
                Ok(Some(self.emit(Ty::Bool, Op::Bool(bop, a, b))))
            }
            other => self.err(line, format!("演算子 {} は段階 1 の対象外", other)),
        }
    }

    /// `a && b` / `a || b`。右辺は必要なときだけ評価する。
    fn short_circuit(&mut self, is_or: bool, l: &V, r: &V, line: usize) -> LR<Option<VId>> {
        let Some(a) = self.expr(l)? else { return Ok(None) };
        if self.vty(a) != Ty::Bool {
            return self.err(line, "論理演算の被演算子が bool ではない");
        }
        let (rhs, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[Ty::Bool]);
        let k = self.emit(Ty::Bool, Op::Const(is_or as i128));
        if is_or {
            self.branch(a, (join, vec![k]), (rhs, vec![]));
        } else {
            self.branch(a, (rhs, vec![]), (join, vec![k]));
        }
        self.cur = Some(rhs);
        if let Some(b) = self.expr(r)? {
            if self.vty(b) != Ty::Bool {
                return self.err(line, "論理演算の被演算子が bool ではない");
            }
            self.goto(join, vec![b]);
        }
        if self.blocks[join].preds == 0 {
            return Ok(None);
        }
        self.cur = Some(join);
        Ok(Some(jp[0]))
    }

    fn unary(&mut self, e: &V) -> LR<Option<VId>> {
        if let Some(done) = self.scalar_unary(e)? {
            return Ok(done);
        }
        let f = nf(e);
        let line = nline(e);
        let op = st(&f[0]);
        match op.as_str() {
            "-" => {
                let t = self.ty_of(e)?;
                if !(t.is_int() && t.signed()) {
                    return self.err(line, "単項 - は符号付き整数だけ段階 1 の対象");
                }
                if !self.def.panics.as_deref().map_or(true, |p| p == "may") {
                    return self.err(line, "panics never の関数に単項 - (溢れの検査が要る)");
                }
                let Some(a) = self.expr(&f[1])? else { return Ok(None) };
                Ok(Some(self.emit(t, Op::Neg { a, mode: Mode::Trap })))
            }
            "!" => {
                let t = self.ty_of(e)?;
                if t != Ty::Bool {
                    return self.err(line, "整数のビット反転は段階 1 の対象外");
                }
                let Some(a) = self.expr(&f[1])? else { return Ok(None) };
                Ok(Some(self.emit(Ty::Bool, Op::Not(a))))
            }
            "&" | "&mut" => self.addr_of(e),
            "*" => self.deref(e),
            other => self.err(line, format!("単項演算子 {} は段階 1 の対象外", other)),
        }
    }

    fn call(&mut self, e: &V) -> LR<Option<VId>> {
        if let Some(r) = coll::try_call(self, e) {
            return r;
        }
        if let Some(r) = self.check_call(e) {
            return r;
        }
        if let Some(r) = self.panic_call(e) {
            return r;
        }
        if let Some(r) = self.unwrap_path(e) {
            return r;
        }
        if let Some(r) = self.user_path_call(e) {
            return r;
        }
        if let Some(done) = self.scalar_call(e)? {
            return Ok(done);
        }
        if let Some(r) = self.builtin_call(e) {
            return r;
        }
        if let Some(r) = ffi_lower::ffi_call(self, e) {
            return r;
        }
        let f = nf(e);
        let line = nline(e);
        if !matches!(f.get(2), Some(V::Nil) | None) {
            return self.err(line, "型引数付きの呼び出しは段階 1 の対象外");
        }
        let callee = &f[0];
        if nname(callee) != "Path" {
            return self.err(line, "式を呼ぶ形は段階 1 の対象外");
        }
        let segs = strs(&nf(callee)[0]);
        let args = items(&f[1]);
        // `std.thread.spawn(allocator, closure)`。
        if segs == ["std", "thread", "spawn"] {
            let Some(r) = thread_lower::spawn_call(self, e, &segs, args, line) else {
                return self.err(line, "std.thread.spawn は段階 1 の対象外");
            };
            return r;
        }
        // `x.f.v.len()`: 構造体のフィールドの Vec / String の長さ (x は参照またはスタックスロットの構造体)
        if segs.len() >= 3 && matches!(segs[segs.len() - 1].as_str(), "len" | "is_empty") && args.is_empty() {
            if let Some(s) = self.lookup(&segs[0]) {
                if let Some(v) = self.vec_field_len(s, &segs, line)? {
                    return Ok(Some(v));
                }
            }
        }
        // `x.m(..)` / `x.f.m(..)`: 局所変数 x (またはそのフィールド) の整数メソッド (wrapping_* だけ)
        if segs.len() >= 2 {
            if let Some(s) = self.lookup(&segs[0]) {
                let base = self.read_slot(s);
                let recv = if segs.len() > 2 { self.field_path(base, &segs[1..segs.len() - 1], line)? } else { base };
                let rt = self.vty(recv);
                if is_net_ty(self, rt) {
                    let meth = segs[segs.len() - 1].clone();
                    if matches!(meth.as_str(), "accept" | "read" | "write" | "flush" | "close" | "local_port") {
                        let args = items(&f[1]).to_vec();
                        return net_method(self, e, &meth, recv, rt, &args, line);
                    }
                }
                let meth = segs[segs.len() - 1].clone();
                if meth == "clone" {
                    return self.lower_clone(line, recv, rt);
                }
                // `handle.join()`: スレッドの結果を取り出す。
                if meth == "join" {
                    if args.len() != 0 {
                        return self.err(line, "join の引数は取れない");
                    }
                    if let Some(r) = thread_lower::join_call(self, e, recv, rt, line) {
                        return r;
                    }
                }
                let aop = match meth.as_str() {
                    "wrapping_add" => Aop::Add,
                    "wrapping_sub" => Aop::Sub,
                    "wrapping_mul" => Aop::Mul,
                    m if matches!(rt, Ty::Agg(_)) => {
                        return self.err(line, format!("メソッド {} は段階 1 の対象外 (利用者の型のメソッド呼び出しは未対応。&mut の受け手は参照の対応が要る)", m))
                    }
                    m => return self.err(line, format!("メソッド {} は段階 1 の対象外", m)),
                };
                if !rt.is_int() || args.len() != 1 {
                    return self.err(line, format!("メソッド {} の受け手や引数が段階 1 の対象外", meth));
                }
                let Some(b) = self.expr(&args[0])? else { return Ok(None) };
                if self.vty(b) != rt {
                    return self.err(line, "wrapping_* の引数の型が受け手と違う");
                }
                return Ok(Some(self.emit(rt, Op::Arith { op: aop, mode: Mode::Wrap, a: recv, b })));
            }
        }
        let resolved = self.p.resolve(&segs, &self.def.file);
        // `E.V.m(..)`: 列挙子の値へのメソッド呼び出し。解決は `E.V` で止まり、残りの名前 (`m`) を落とすと別の値になる。
        if let Some(r) = &resolved {
            if let (Tgt::Variant(en, _), false) = (&r.target, r.rest.is_empty()) {
                let en = en.clone();
                if r.rest.len() == 1 {
                    if let Some(res) = self.variant_method_call(e, &en, &r.rest[0].clone()) {
                        return res;
                    }
                }
                return self.err(line, format!("呼び出し {} (列挙子の値のメソッド) は段階 1 の対象外", segs.join(".")));
            }
        }
        let target = resolved.map(|r| r.target);
        if matches!(&target, Some(Tgt::Native(n)) if n == "std.io.print_line") || matches!(&target, Some(Tgt::Sig(di) | Tgt::Fn(di)) if self.p.defs[*di].name == "std.io.print_line") {
            return self.print_line(args, line);
        }
        let callee_name = segs.join(".");
        if matches!(callee_name.as_str(), "std.net.listen" | "std.net.connect") {
            return net_call(self, e, callee_name == "std.net.listen", line);
        }
        if let Some(Tgt::Variant(en, vn)) = &target {
            let (en, vn) = (en.clone(), vn.clone());
            return self.variant_call(e, &en, &vn);
        }
        let Some(Tgt::Fn(di)) = target else {
            return self.err(line, format!("呼び出し {} は段階 1 の対象外", segs.join(".")));
        };
        let cd = &self.p.defs[di];
        if !is_candidate(cd) {
            return self.err(line, format!("呼び先 {} は段階 1 の対象外 (ジェネリクス・メソッド・本体なし)", cd.name));
        }
        let gn = HashSet::new();
        let mut pts = vec![];
        for (_, t) in &cd.params {
            match coll::sig_ty(self, &from_v(t, &gn)) {
                Some(x) => pts.push(x),
                None => return self.err(line, format!("呼び先 {} の引数の型は段階 1 の対象外", cd.name)),
            }
        }
        let rt = if is_nil(&cd.ret) {
            Ty::Unit
        } else {
            match coll::sig_ty(self, &from_v(&cd.ret, &gn)) {
                Some(x) => x,
                None => return self.err(line, format!("呼び先 {} の戻り値の型は段階 1 の対象外", cd.name)),
            }
        };
        if pts.len() != args.len() {
            return self.err(line, format!("呼び先 {} の引数の個数が違う", cd.name));
        }
        let name = cd.name.clone();
        let mut vs = vec![];
        for a in args {
            match self.expr(a)? {
                Some(v) => {
                    coll::tmp_hold(self, v);
                    vs.push(v)
                }
                None => {
                    coll::tmp_release(self, &vs);
                    return Ok(None);
                }
            }
        }
        coll::tmp_release(self, &vs);
        let vs = self.coerce_args(vs, &pts);
        for (v, pt) in vs.iter().zip(pts.iter()) {
            if self.vty(*v) != *pt {
                return self.err(line, format!("呼び先 {} の引数の型が合わない", name));
            }
        }
        self.callees.insert(name.clone());
        Ok(Some(self.emit(rt, Op::Call { callee: name, args: vs })))
    }

    /// `std.io.print_line(s)`: `write 1, s` の後に `write 1, "\n"` (設計_KIR.md 12.2)。どちらかが失敗したら、その `Err` が結果になる。
    fn print_line(&mut self, args: &[V], line: usize) -> LR<Option<VId>> {
        if args.len() != 1 {
            return self.err(line, "std.io.print_line の引数は 1 個のはず");
        }
        let Some(s) = self.expr(&args[0])? else { return Ok(None) };
        let fat = self.fat_ty();
        if self.vty(s) != fat {
            return self.err(line, "std.io.print_line の引数が &str ではない");
        }
        let io = self.aggs.borrow_mut().io_error(self.p).map_err(|e| format!("行 {}: {}", line, e))?;
        let res = Ty::Res(kir::Pay::Unit, kir::Pay::of(io).expect("集成体はペイロードになる"));
        let fd = self.emit(Ty::I32, Op::Const(1));
        let r1 = self.emit(res, Op::Write { fd, data: s });
        let failed = self.emit(Ty::Bool, Op::Tag(r1));
        let (nb, _) = self.new_block(&[]);
        let (jb, jp) = self.new_block(&[res]);
        self.branch(failed, (jb, vec![r1]), (nb, vec![]));
        self.cur = Some(nb);
        let data = self.emit(Ty::Ptr, Op::StaticBytes(vec![b'\n']));
        let one = self.emit(Ty::Usize, Op::Const(1));
        let nl = self.emit(fat, Op::Make { variant: None, fields: vec![data, one] });
        let r2 = self.emit(res, Op::Write { fd, data: nl });
        self.goto(jb, vec![r2]);
        self.cur = Some(jb);
        Ok(Some(jp[0]))
    }

    /// `e.m(args)` (受け手が式の形)。整数の `wrapping_add` / `wrapping_sub` / `wrapping_mul` だけを扱う。
    fn mcall(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let line = nline(e);
        // `handle.join()`: スレッドの結果を取り出す。
        if st(&f[1]) == "join" {
            let t = self.ty_of(&f[0])?;
            let Some(a) = self.expr(&f[0])? else { return Ok(None) };
            let args = items(&f[2]);
            if args.len() != 0 {
                return self.err(line, "join の引数は取れない");
            }
            if let Some(r) = thread_lower::join_call(self, e, a, t, line) {
                return r;
            }
        }
        if let Some(r) = coll::try_mcall(self, e) {
            return r;
        }
        // `std.net.TcpListener` / `TcpStream` のメソッド (accept / read / write / flush / close)。
        if let Ok(rt) = self.src_ty(&nf(e)[0]) {
            let is_net = match &rt {
                Src::Named(n, _) if n == "std.net.TcpListener" || n == "std.net.TcpStream" => true,
                Src::Ref(_, inner) => {
                    matches!(&**inner, Src::Named(n, _) if n == "std.net.TcpListener" || n == "std.net.TcpStream")
                }
                _ => false,
            };
            if is_net {
                let m = st(&nf(e)[1]);
                if matches!(m.as_str(), "accept" | "read" | "write" | "flush" | "close") {
                    let Some(recv) = self.expr(&nf(e)[0])? else { return Ok(None) };
                    let rt = self.vty(recv);
                    let args = items(&nf(e)[2]).to_vec();
                    return net_method(self, e, &m, recv, rt, &args, nline(e));
                }
            }
        }
        if let Some(r) = self.unwrap_mcall(e) {
            return r;
        }
        if let Some(r) = self.user_mcall(e) {
            return r;
        }
        if let Some(done) = self.scalar_mcall(e)? {
            return Ok(done);
        }
        if let Some(r) = self.builtin_mcall(e) {
            return r;
        }
        let f = nf(e);
        let line = nline(e);
        if !matches!(f.get(3), Some(V::Nil) | None) {
            return self.err(line, "型引数付きのメソッドは段階 1 の対象外");
        }
        let mname = st(&f[1]);
        let args = items(&f[2]);
        let aop = match mname.as_str() {
            "wrapping_add" => Aop::Add,
            "wrapping_sub" => Aop::Sub,
            "wrapping_mul" => Aop::Mul,
            m => return self.err(line, format!("メソッド {} は段階 1 の対象外", m)),
        };
        let t = self.ty_of(&f[0])?;
        if !t.is_int() || args.len() != 1 {
            return self.err(line, "wrapping_* の受け手か引数が段階 1 の対象外");
        }
        let Some(a) = self.expr(&f[0])? else { return Ok(None) };
        let Some(b) = self.expr(&args[0])? else { return Ok(None) };
        if self.vty(b) != t {
            return self.err(line, "wrapping_* の引数の型が受け手と違う");
        }
        Ok(Some(self.emit(t, Op::Arith { op: aop, mode: Mode::Wrap, a, b })))
    }

    /// `{ 文... 末尾の式 }`。末尾が式文なら、その値が block の値になる。
    fn block(&mut self, e: &V) -> LR<Option<VId>> {
        let stmts = items(&nf(e)[0]);
        self.scopes.push(vec![]);
        let mut out: Option<VId> = None;
        let mut diverged = false;
        for (i, s) in stmts.iter().enumerate() {
            let last = i + 1 == stmts.len();
            match self.stmt(s)? {
                St::Div => {
                    diverged = true;
                    break;
                }
                St::Unit => {
                    if last {
                        out = Some(self.unit());
                    }
                }
                St::Val(v) => {
                    if last {
                        out = Some(v);
                    } else {
                        // 文として評価した式の値は捨てる。所有物なら解放する。
                        coll::drop_temp(self, v);
                    }
                }
            }
        }
        if stmts.is_empty() {
            out = Some(self.unit());
        }
        coll::block_end(self);
        self.scopes.pop();
        if diverged {
            return Ok(None);
        }
        Ok(out)
    }

    fn stmt(&mut self, s: &V) -> LR<St> {
        let f = nf(s);
        let line = nline(s);
        match nname(s) {
            "Let" => {
                let pat = &f[0];
                if nname(pat) != "PBind" {
                    return self.let_pat(pat, &f[3]);
                }
                if matches!(nf(pat).get(1), Some(V::B(true))) {
                    return self.err(line, "let の束縛はただの名前だけ段階 1 の対象");
                }
                let name = st(&nf(pat)[0]);
                let Some(v) = self.expr(&f[3])? else { return Ok(St::Div) };
                let t = self.vty(v);
                coll::bind_owned(self, &name, v, t);
                coll::after_let(self, &name, &f[3])?;
                Ok(St::Unit)
            }
            "Assign" => {
                let target = &f[0];
                if nname(target) == "Unary" {
                    return self.store_deref(target, &f[1], line);
                }
                if nname(target) == "Index" {
                    return coll::assign_index(self, s);
                }
                let Some(name) = path1(target) else {
                    if nname(target) == "Path" && strs(&nf(target)[0]).len() > 1 {
                        return self.assign_path(target, &f[1], line);
                    }
                    // `self.f = e` (フィールドの式の形): 根が名前の鎖だけ
                    if nname(target) == "Field" {
                        if let Some(segs) = agg::field_segs(target) {
                            return self.assign_segs(segs, &f[1], line);
                        }
                    }
                    return self.err(line, "代入先は名前かフィールドだけ段階 1 の対象");
                };
                let Some(s_idx) = self.lookup(&name) else {
                    return self.err(line, format!("代入先 {} が見つからない", name));
                };
                coll::check_assign(self, s_idx, line)?;
                let Some(v) = self.expr(&f[1])? else { return Ok(St::Div) };
                coll::assign_checked(self, s_idx, v, line)?;
                Ok(St::Unit)
            }
            "ExprStmt" => match self.expr(&f[0])? {
                Some(v) => Ok(St::Val(v)),
                None => Ok(St::Div),
            },
            other => self.err(line, format!("文 {} は段階 1 の対象外", other)),
        }
    }

    fn if_(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let line = nline(e);
        let Some(c) = self.expr(&f[0])? else { return Ok(None) };
        if self.vty(c) != Ty::Bool {
            return self.err(line, "if の条件が bool ではない");
        }
        // 結果の型。両枝とも発散するなら `!` で、結果の値は持たない。unit の値は合流の引数に載せない。
        let res: Option<Ty> = match self.src_ty(e)? {
            Src::Never => None,
            t => Some(coll::value_ty(self, &t).or_else(|| self.conv_t(&t)).ok_or_else(|| format!("行 {}: if の型 {} は段階 1 の対象外", line, show(&t)))?),
        };
        let carry = matches!(res, Some(t) if t != Ty::Unit);
        let slots = self.assigned(&[&f[1], &f[2]]);
        let slot_tys: Vec<Ty> = slots.iter().map(|s| self.slots[*s].1).collect();
        let mut params: Vec<Ty> = if carry { res.into_iter().collect() } else { vec![] };
        params.extend(slot_tys.iter().copied());
        let (tb, _) = self.new_block(&[]);
        let (fb, _) = self.new_block(&[]);
        let (jb, jp) = self.new_block(&params);
        let saved = self.slots.clone();
        self.branch(c, (tb, vec![]), (fb, vec![]));

        // 枝の終わりから合流へ渡す引数: (結果の値、代入された変数の現在値)
        let join_args = |lw: &Self, v: Option<VId>| -> Vec<VId> {
            let mut a: Vec<VId> = if carry { v.into_iter().collect() } else { vec![] };
            a.extend(lw.vals_of(&slots));
            a
        };
        self.cur = Some(tb);
        if let Some(v) = self.expr(&f[1])? {
            if res.is_some() && self.vty(v) != res.unwrap() {
                return self.err(line, "if の then 枝の型が結果の型と違う");
            }
            let args = join_args(self, Some(v));
            self.goto(jb, args);
        }
        self.slots = saved.clone();
        self.cur = Some(fb);
        if matches!(f[2], V::Nil) {
            // else が無いときは値を作らずに合流する (結果は unit)。
            let v = if carry { Some(self.unit()) } else { None };
            let args = join_args(self, v);
            self.goto(jb, args);
        } else if let Some(v) = self.expr(&f[2])? {
            if res.is_some() && self.vty(v) != res.unwrap() {
                return self.err(line, "if の else 枝の型が結果の型と違う");
            }
            let args = join_args(self, Some(v));
            self.goto(jb, args);
        }
        if self.blocks[jb].preds == 0 {
            self.slots = saved;
            return Ok(None);
        }
        self.cur = Some(jb);
        let off = carry as usize;
        for (i, s) in slots.iter().enumerate() {
            self.slots[*s] = (jp[off + i], slot_tys[i]);
        }
        Ok(Some(if carry { jp[0] } else { self.unit() }))
    }

    fn while_(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let slots = self.assigned(&[&f[1]]);
        self.loop_common(Some(&f[0]), &f[1], slots, e)
    }

    fn loop_(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let slots = self.assigned(&[&f[0]]);
        self.loop_common(None, &f[0], slots, e)
    }

    /// `while` と `loop` の共通部分。先頭ブロックは代入される変数を引数に持つ。
    fn loop_common(&mut self, cond: Option<&V>, body: &V, slots: Vec<usize>, e: &V) -> LR<Option<VId>> {
        let tys: Vec<Ty> = slots.iter().map(|s| self.slots[*s].1).collect();
        let entry = self.vals_of(&slots);
        let (hb, hp) = self.new_block(&tys);
        self.goto(hb, entry);
        self.cur = Some(hb);
        for (i, s) in slots.iter().enumerate() {
            self.slots[*s] = (hp[i], tys[i]);
        }
        let (xb, xp) = self.new_block(&tys);
        let mut body_start = hb;
        if let Some(c) = cond {
            let Some(cv) = self.expr(c)? else { return Ok(None) };
            if self.vty(cv) != Ty::Bool {
                return self.err(nline(e), "while の条件が bool ではない");
            }
            let (bb, _) = self.new_block(&[]);
            let exit_args = self.vals_of(&slots);
            self.branch(cv, (bb, vec![]), (xb, exit_args));
            body_start = bb;
        }
        self.loops.push(LoopCx { header: hb, exit: xb, slots: slots.clone(), depth: self.scopes.len() });
        self.cur = Some(body_start);
        let r = self.expr(body)?;
        if r.is_some() {
            let args = self.vals_of(&slots);
            self.goto(hb, args);
        }
        self.loops.pop();
        if self.blocks[xb].preds == 0 {
            return Ok(None);
        }
        self.cur = Some(xb);
        for (i, s) in slots.iter().enumerate() {
            self.slots[*s] = (xp[i], tys[i]);
        }
        Ok(Some(self.unit()))
    }

    fn jump_loop(&mut self, e: &V, is_break: bool) -> LR<Option<VId>> {
        let Some(lc) = self.loops.last() else {
            return self.err(nline(e), "ループの外の break / continue");
        };
        let (target, slots, depth) = (if is_break { lc.exit } else { lc.header }, lc.slots.clone(), lc.depth);
        coll::drop_from(self, depth);
        let args = self.vals_of(&slots);
        self.goto(target, args);
        Ok(None)
    }

    fn ret_(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let line = nline(e);
        let v = if matches!(f[0], V::Nil) {
            self.unit()
        } else {
            match self.expr(&f[0])? {
                Some(v) => v,
                None => return Ok(None),
            }
        };
        if self.vty(v) != self.ret {
            return self.err(line, "return の値の型が戻り値の型と違う");
        }
        coll::drop_from(self, 0);
        self.terminate(Term::Ret(v));
        Ok(None)
    }
}

/// 整数リテラルの字句 (接尾辞は取り除かれている。`-` と `0x` を扱う)。
fn parse_int(v: &V) -> Option<i128> {
    let s = match v {
        V::N(s) | V::S(s) => s.replace('_', ""),
        _ => return None,
    };
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b.to_string()),
        None => (false, s.clone()),
    };
    let mag = match body.strip_prefix("0x") {
        Some(h) => i128::from_str_radix(h, 16).ok()?,
        None => body.parse::<i128>().ok()?,
    };
    Some(if neg { -mag } else { mag })
}

/// ユーザの関数を下ろす。呼び先が未対応・検査違反の関数は、その呼び出し元とともに外す。
pub fn lower_user(p: &Program, user: &[usize]) -> Lowered {
    lower_program(p, user, None)
}

/// `lower_user` に加えて、`entry` (入口の関数の FQN) が `Result<T, E>` を返すなら、`E` の Debug 表示の関数 (`<E>.$debug`) も作る。
/// 作れない (`derive [Debug]` がない、未対応の型を含むなど) ときは、入口を未対応にする (理由付き)。
pub fn lower_program(p: &Program, user: &[usize], entry: Option<&str>) -> Lowered {
    lower_defs(p, user, entry, None)
}

/// `ksn test --both` の機械語側: テスト本体 `test_def` (定義の添字) を入口にして下ろす。
/// テストは引数を持たず、戻り値は `()` (`test` の本体)。入口の名前はテストの FQN。
pub fn lower_test(p: &Program, user: &[usize], test_def: usize) -> Lowered {
    lower_defs(p, user, Some(&p.defs[test_def].name), Some(test_def))
}

fn lower_defs(p: &Program, user: &[usize], entry: Option<&str>, test_def: Option<usize>) -> Lowered {
    let tys = type_table(p, user, test_def);
    let aggs = RefCell::new(agg::AggTab::new(user));
    // `std.net.listen`/`connect`/`accept` が返すソケット集成体を、誰も参照しなくても型表に入れる。
    for name in ["std.net.TcpListener", "std.net.TcpStream"] {
        if let Some(di) = p.defs.iter().position(|d| d.name == name) {
            let _ = aggs.borrow_mut().intern_def(p, di);
        }
    }
    let mut ok: Vec<(Func, BTreeSet<String>)> = vec![];
    let mut bad: Vec<(String, String)> = vec![];
    for &i in user {
        let d = &p.defs[i];
        if !is_candidate(d) && test_def != Some(i) {
            continue;
        }
        match Lw::new(p, &tys, &aggs, d).lower() {
            Ok(x) => ok.extend(x),
            Err(why) => bad.push((d.name.clone(), why)),
        }
    }
    // `extern` 宣言を KIR の関数宣言 (本体なし) にする。リンク名は `Func.link` に持つ。
    for &i in user {
        let d = &p.defs[i];
        if d.kw != "extern" {
            continue;
        }
        match ffi_lower::extern_func(p, d, &aggs) {
            Ok(f) => ok.push((f, BTreeSet::new())),
            Err(why) => bad.push((d.name.clone(), why)),
        }
    }
    if let Some(e) = entry {
        if let Some(pos) = ok.iter().position(|(f, _)| f.key == e) {
            if let Err(why) = debug_of_entry(p, &aggs, e, &mut ok, pos) {
                let (f, _) = ok.remove(pos);
                bad.push((f.key, why));
            }
        }
    }
    // 同じ単相インスタンスを、別々の呼び出し元が下ろしていることがある (内容は同じ)。鍵の重複は先の一つだけ残す。
    let mut seen: HashSet<String> = HashSet::new();
    ok.retain(|(f, _)| seen.insert(f.key.clone()));
    let funcs = loop {
        let keys: HashSet<String> = ok.iter().map(|(f, _)| f.key.clone()).collect();
        let mut drop: Vec<(String, String)> = vec![];
        for (f, callees) in &ok {
            if let Some(c) = callees.iter().find(|c| !keys.contains(*c)) {
                drop.push((f.key.clone(), format!("呼び先 {} が未対応", c)));
            }
        }
        if drop.is_empty() {
            let m = Module { funcs: ok.iter().map(|(f, _)| f.clone()).collect(), aggs: aggs.borrow().defs.clone() };
            for (key, msg) in kir::verify_module(&m) {
                drop.push((key, format!("下ろしの検査に失敗: {}", msg)));
            }
            if drop.is_empty() {
                break m.funcs;
            }
        }
        ok.retain(|(f, _)| !drop.iter().any(|(k, _)| *k == f.key));
        bad.extend(drop);
    };
    let mut funcs = funcs;
    funcs.sort_by(|a, b| a.key.cmp(&b.key));
    bad.sort();
    let mut module = Module { funcs, aggs: aggs.into_inner().defs };
    kir::prune_aggs(&mut module);
    Lowered { module, unsupported: bad }
}

/// 入口 `e` の戻り値が `Result<T, E>` なら、`E` の Debug 表示の関数を `ok` に足す。
fn debug_of_entry(p: &Program, aggs: &RefCell<agg::AggTab>, e: &str, ok: &mut Vec<(Func, BTreeSet<String>)>, pos: usize) -> Result<(), String> {
    // 戻り値が `()` の入口 (テストの本体を含む) は、Debug 表示の関数を要らない。
    let Ty::Res(_, err) = ok[pos].0.ret else { return Ok(()) };
    let d = p.defs.iter().find(|d| d.name == e && d.kw == "fn").ok_or_else(|| format!("入口 {} の定義がない", e))?;
    if !d.params.is_empty() {
        return Err("入口の関数に引数がある".to_string());
    }
    let rt = from_v(&d.ret, &HashSet::new());
    let Src::Named(n, args) = &rt else { return Err("入口の戻り値の型が読めない".to_string()) };
    if n != "Result" || args.len() != 2 {
        return Err("入口の戻り値の型が読めない".to_string());
    }
    let mut g = debug::Gen::new(p, aggs);
    let k = g.need(&args[1], err.ty(), &d.file).map_err(|w| format!("main の Err の Debug 表示を作れない: {}", w))?;
    g.entry_wrapper(&k, err.ty())?;
    ok.extend(g.out);
    Ok(())
}

/// `KSN_DUMP_KIR=1 ksn check` のときだけ、KIR と未対応の一覧を stderr に出す (隠し機能)。
pub fn dump_hidden(prog: Option<&Program>, user: &[usize]) {
    if std::env::var_os("KSN_DUMP_KIR").is_none() {
        return;
    }
    let Some(p) = prog else { return };
    let lo = lower_user(p, user);
    eprint!("{}", kir::print_module(&lo.module));
    for (f, why) in &lo.unsupported {
        eprintln!("kir: 未対応 {}: {}", f, why);
    }
}
