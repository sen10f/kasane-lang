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
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::check::{is_nil, items, nf, nlineno as nline, nname, st, strs};
use crate::kir::{self, Aop, Block, BoolOp, Cc, Func, Inst, Mode, Module, Op, Term, Ty, VId};
use crate::kir_suspend;
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
// `std.task` (`block_on` / `spawn_local` / `yield_now` / `join_task`) の下ろし (子モジュール、設計_KIR.md 26 章)。
#[path = "kir_lower_task.rs"]
mod task_lower;
#[path = "kir_lower_sync.rs"]
mod sync_lower;
#[path = "kir_lower_chan.rs"]
mod chan_lower;
// 型引数付きの関数・メソッドの単相化 (子モジュール)。
#[path = "kir_lower_mono.rs"]
mod mono;
// 配列リテラルの下ろし (子モジュール)。
#[path = "kir_lower_array.rs"]
mod array_lower;
// `Box<T>` と `Box<dyn Tr>` の下ろし (子モジュール。設計_KIR.md 21 章)。
#[path = "kir_lower_dyn.rs"]
mod dyn_lower;

// `derive [Drop]` の再帰する型の解放の補助関数 (子モジュール)。
#[path = "kir_lower_drop.rs"]
mod drop_lower;

// コレクション・文字列・クロージャの下ろし (子モジュール。`Lw` の非公開の項目を使う)。
#[path = "kir_lower_iter.rs"]
mod iter_lower;
#[path = "kir_lower_fb.rs"]
mod fb;
#[path = "kir_lower_strord.rs"]
mod strord;
#[path = "kir_lower_coll.rs"]
mod coll;
#[path = "kir_lower_for.rs"]
mod for_lower;
#[path = "kir_lower_own.rs"]
mod own;
#[path = "kir_lower_buf.rs"]
mod buf;
#[path = "kir_lower_ext.rs"]
mod ext;
use coll::CollCx;

// `std.net` (TCP) の下ろし (設計_KIR.md 12 章)。
#[path = "kir_lower_net.rs"]
mod net_lower;
use net_lower::{is_net_ty, net_call, net_free_call, net_method};

// `std.fs` (ファイル) の下ろし (設計_KIR.md 12 章)。
#[path = "kir_lower_fs.rs"]
mod fs_lower;
use fs_lower::{is_fs_ty, fs_call, fs_method};
#[path = "kir_lower_proc.rs"]
mod proc_lower;
use proc_lower::{is_proc_ty, proc_call, proc_method};

// `std.env` (引数・環境変数) の下ろし。
#[path = "kir_lower_env.rs"]
mod env_lower;
use env_lower::env_call;

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
    /// 関数の鍵 → 命令ごとの行 (VM の行表の元。設計_KIR.md 7 章 13)。下ろした関数だけ (合成した補助関数にはない)。
    pub lines: HashMap<String, FnLines>,
}

/// 1 つの関数の行の対応: ソースのファイルと、値番号 (命令の結果) → その命令を作った式の行 (0 は不明)。
#[derive(Clone, Debug, Default)]
pub struct FnLines {
    pub file: String,
    pub lines: Vec<u32>,
}

thread_local! {
    /// `lower_defs` の間に下ろした関数の行の対応 (終わりに `Lowered::lines` へ移す)。
    static LINES: RefCell<HashMap<String, FnLines>> = RefCell::new(HashMap::new());
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
    /// `continue` が header へ渡す先頭の引数 (header が取る値のうち、スロットより前の分)。無ければ空。
    /// `for x in s.split(sep)` / `chars()` は、区切りの位置などをここで渡す。
    cont: Vec<VId>,
}

struct Lw<'a> {
    p: &'a Program,
    tys: &'a BTreeMap<(String, u32), ExprTy>,
    def: &'a Def,
    /// 集成体の型表 (全関数で共有。`lower_user` が作る)。
    aggs: &'a RefCell<agg::AggTab>,
    /// 単相インスタンスの型パラメータ名 → 具体的な型 (空なら単相の関数)。設計_KIR.md 20 章。
    subst: HashMap<String, Src>,
    /// 単相インスタンスの鍵 (`fqn<型引数,..>`)。単相の関数は `None` (鍵は FQN)。
    inst_key: Option<String>,
    /// 下ろし済み (下ろし中) の型引数付きインスタンスの鍵 (入れ子のインスタンスと共有する)。
    mono_done: std::rc::Rc<RefCell<BTreeSet<String>>>,
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
    /// 式の下ろしの間、いまの式の行 (`expr` が入るときに立て、出るときに戻す)。
    cur_line: u32,
    /// 値番号 → その命令を作った式の行。
    line_of: Vec<u32>,
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
            subst: HashMap::new(),
            inst_key: None,
            mono_done: Default::default(),
            blocks: vec![],
            cur: None,
            vty: vec![],
            slots: vec![],
            scopes: vec![],
            loops: vec![],
            callees: BTreeSet::new(),
            ret: Ty::Unit,
            cur_line: 0,
            line_of: vec![],
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
        if self.line_of.len() <= v as usize {
            self.line_of.resize(v as usize + 1, 0);
        }
        self.line_of[v as usize] = self.cur_line;
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
        // `match &e { Some(p) => .. }` (e は所有物を持たない Option / Result): 共有の借用で読むだけなので `match e` と同じ
        // (束縛した名前は値で持ち、`*p` は恒等になる)。
        if nname(&scrut) == "Unary" && st(&nf(&scrut)[0]) == "&" {
            if let Src::Ref(false, inner) = &stt {
                if matches!(&**inner, Src::Named(n, _) if n == "Option" || n == "Result") {
                    let operand = nf(&scrut)[1].clone();
                    if self.ty_of(&operand).map(|t| !coll::owns(self, t)).unwrap_or(false) && !matches!(self.src_ty(&operand), Ok(Src::Ref(..))) {
                        if let V::Node(n, fs, a, b) = e {
                            let mut fs = fs.clone();
                            fs[0] = operand;
                            let e2 = V::Node(n, fs, *a, *b);
                            return self.match_(&e2);
                        }
                    }
                }
            }
        }
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
            Some(x) if id != 0 => Ok(if self.subst.is_empty() { x.ty.clone() } else { mono::subst_ty(&x.ty, &self.subst) }),
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
        let body = d.body.as_ref().expect("候補は本体を持つ");
        let rewritten = ufcs_rewrite(body, self.p, &d.file);
        let body = rewritten.as_ref().unwrap_or(body);
        let mut params = vec![];
        for (_, t) in &d.params {
            let st = self.decl_ty(t);
            match coll::sig_ty(&self, &st) {
                Some(x) => params.push(x),
                None => return Err(format!("引数の型 {} は段階 1 の対象外 ({})", show(&st), sig_why(&self, &st))),
            }
        }
        // `-> !` は戻らない関数。KIR の戻り値は unit にして、本体の末尾は到達しないものとして扱う。
        let never_ret = !is_nil(&d.ret) && matches!(self.decl_ty(&d.ret), Src::Never);
        let ret = if is_nil(&d.ret) || never_ret {
            Ty::Unit
        } else {
            let st = self.decl_ty(&d.ret);
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
            let pst = self.decl_ty(pt);
            coll::bind_param(&mut self, name, *v, *t, &pst);
            if let Some(s) = self.lookup(name) {
                self.own_declare(name, s, &pst);
            }
            // `&[T; N]` / `&mut [T; N]` のパラメータ: 長さ N は型から分かる (ptr にするだけ)。
            if let Some(n) = coll::ref_array_len(pt) {
                let s = self.lookup(name).expect("直後に束縛したパラメータ");
                self.coll.arr_len.insert(s, n);
            }
        }
        if let Some(v) = self.expr(body)? {
            if never_ret {
                // 検査を通った `-> !` の本体は末尾に届かない (exit・発散する呼び出し・無限ループで終わる)。届くなら下ろせない。
                return Err(format!("! を返す関数 {} の本体が戻りうる (発散しない形は段階 1 の対象外)", d.name));
            }
            if self.vty(v) != ret {
                return Err(format!("本体の値の型が戻り値の型と合わない ({} / {})", self.name_ty_dbg(self.vty(v)), self.name_ty_dbg(ret)));
            }
            coll::drop_from(&mut self, 0);
            self.terminate(Term::Ret(v));
        }
        let mut uses = d.uses.clone();
        // `test` は下ろしの入口 (`lower_test`) としてだけ下ろされる (`is_candidate` は `fn` だけ)。
        if d.kw == "test" {
            uses.extend(ALL_EFFECTS.iter().map(|s| s.to_string()));
        }
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
            key: self.inst_key.clone().unwrap_or_else(|| d.name.clone()),
            uses,
            may_panic: d.panics.as_deref() != Some("never"),
            stack: d.stack,
            ret,
            link: None,
            blocks,
        };
        LINES.with(|l| {
            l.borrow_mut().insert(f.key.clone(), FnLines { file: d.file.clone(), lines: std::mem::take(&mut self.line_of) });
        });
        let mut out = std::mem::take(&mut self.coll.extra);
        out.insert(0, (f, self.callees));
        Ok(out)
    }

    // ------------------------------------------------ 式

    /// 式を下ろす。`None` は制御がその先へ進まないこと (return / break / continue など) を表す。
    fn expr(&mut self, e: &V) -> LR<Option<VId>> {
        let saved = self.cur_line;
        let l = nline(e);
        if l != 0 {
            self.cur_line = l as u32;
        }
        let r = self.expr_dispatch(e);
        self.cur_line = saved;
        r
    }

    fn expr_dispatch(&mut self, e: &V) -> LR<Option<VId>> {
        coll::elem_prep(self, e)?;
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
            "With" => self.with_(e),
            "StructLit" | "TupleE" | "Field" => self.agg_expr(e),
            "Unsafe" => ffi_lower::unsafe_block(self, e),
            "Match" => self.match_any(e),
            "Try" => self.try_(e),
            "Index" => coll::index_read(self, e),
            "For" => coll::for_expr(self, e),
            "Closure" => coll::closure_lit(self, e),
            "ArrayE" => self.array_lit(e).map(|o| o.map(|(v, _)| v)),
            "ArrayRep" => self.array_rep(e),
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

    /// 型が Copy か (`check.rs` の `is_copy_ty` と同じ規則。決まらない型は Copy とみなして黙る)。
    fn src_is_copy(&self, t: &Src) -> bool {
        match t {
            Src::Unknown | Src::Var(_) | Src::Param(_) | Src::Never | Src::Unit | Src::Prim(_) => true,
            // `&mut` の値渡しは再借用 (move ではない) として扱う。
            Src::Ref(..) => true,
            Src::Tuple(xs) => xs.iter().all(|x| self.src_is_copy(x)),
            Src::Named(nm, args) => {
                if nm == "Option" || nm == "Result" {
                    return args.iter().all(|a| self.src_is_copy(a));
                }
                self.p.types.get(nm).map_or(false, |&i| self.p.defs[i].derive.iter().any(|x| x == "Copy"))
            }
            Src::Array(el, _) | Src::Slice(el) => self.src_is_copy(el),
            Src::Ptr(..) | Src::Const(_) | Src::Dyn(..) | Src::Closure { .. } => true,
        }
    }

    /// 型が線形か (`check.rs` の `is_linear` と同じ規則。型パラメータは見ない)。
    fn src_is_linear(&self, t: &Src, seen: &mut HashSet<String>) -> bool {
        match t {
            Src::Tuple(xs) => xs.iter().any(|x| self.src_is_linear(x, seen)),
            Src::Array(el, _) => self.src_is_linear(el, seen),
            Src::Named(nm, args) => {
                if nm == "std.sync.Sender" {
                    return false;
                }
                if args.iter().any(|a| self.src_is_linear(a, seen)) {
                    return true;
                }
                let Some(&i) = self.p.types.get(nm) else { return false };
                let d = &self.p.defs[i];
                if d.linear {
                    return true;
                }
                if !seen.insert(nm.clone()) {
                    return false;
                }
                d.fields.iter().any(|f| self.v_is_linear(&f.1, seen)) || d.variants.iter().any(|v| v.1.iter().any(|x| self.v_is_linear(x, seen)))
            }
            _ => false,
        }
    }

    fn v_is_linear(&self, t: &V, seen: &mut HashSet<String>) -> bool {
        use crate::check::{tag, tel};
        match tag(t).as_str() {
            "tuple" => items(tel(t, 1)).iter().any(|x| self.v_is_linear(x, seen)),
            "array" => self.v_is_linear(tel(t, 1), seen),
            "named" => {
                let name = st(tel(t, 1));
                if name == "std.sync.Sender" {
                    return false;
                }
                if items(tel(t, 2)).iter().any(|a| matches!(a, V::L(_)) && self.v_is_linear(a, seen)) {
                    return true;
                }
                let Some(&i) = self.p.types.get(&name) else { return false };
                let d = &self.p.defs[i];
                if d.linear {
                    return true;
                }
                if !seen.insert(name) {
                    return false;
                }
                d.fields.iter().any(|f| self.v_is_linear(&f.1, seen)) || d.variants.iter().any(|v| v.1.iter().any(|x| self.v_is_linear(x, seen)))
            }
            _ => false,
        }
    }

    /// 局所変数 `name` (スロット `s`) を所有の注釈で追う。束縛の位置に識別子 (unit 定数) を作る。Copy でも線形でもない型だけ。
    fn own_declare(&mut self, name: &str, s: usize, t: &Src) {
        let linear = self.src_is_linear(t, &mut HashSet::new());
        if self.cur.is_none() || (self.src_is_copy(t) && !linear) {
            return;
        }
        let id = self.unit();
        self.coll.own_ids.insert((name.to_string(), s), id);
        if linear {
            self.emit(Ty::Unit, Op::Own { a: id, kind: kir::OwnKind::Owe });
        }
    }

    /// 束縛されない一時の線形な値を、借用する (`&f()`、借用の受け手 `f().m()`) とき: 借りるだけで消費しないので義務が残る (E3003)。
    pub(super) fn own_temp(&mut self, t: &Src) {
        if self.cur.is_none() || !self.src_is_linear(t, &mut HashSet::new()) {
            return;
        }
        let id = self.unit();
        self.emit(Ty::Unit, Op::Own { a: id, kind: kir::OwnKind::Owe });
    }

    fn own_emit(&mut self, name: &str, s: usize, kind: kir::OwnKind) {
        if self.cur.is_none() || self.coll.own_mute == Some(s) {
            return;
        }
        if let Some(id) = self.coll.own_ids.get(&(name.to_string(), s)).copied() {
            self.emit(Ty::Unit, Op::Own { a: id, kind });
        }
    }

    fn path_val(&mut self, e: &V) -> LR<VId> {
        let segs = strs(&nf(e)[0]);
        let line = nline(e);
        if segs.len() == 1 {
            if let Some(s) = self.lookup(&segs[0]) {
                let v = coll::path_read(self, s);
                // Copy でない局所変数の値渡し (move) の注釈 (借用・move の検査用。設計_KIR.md 24 章)。
                if let Ok(t) = self.src_ty(e) {
                    if !self.src_is_copy(&t) {
                        self.own_emit(&segs[0], s, kir::OwnKind::Consume);
                    }
                }
                return Ok(v);
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
                // Copy でないフィールドの値渡し: 部分的な move (義務は果たし、別のフィールドの move は許す。注釈)。
                // Copy のフィールドの読みは move 済みの値の読みになる (`use`)。
                if let Ok(t) = self.src_ty(e) {
                    if !self.src_is_copy(&t) {
                        self.own_emit(&segs[0], s, kir::OwnKind::Part);
                    } else {
                        self.own_emit(&segs[0], s, kir::OwnKind::Use);
                    }
                }
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
            Some(Tgt::Variant(en, vn)) => self.variant_value(&en, &vn, Some(e), line),
            // 関数の項目を値として使う: クロージャと同じ `unit` の印 (呼び先は鍵で決まる。種類は `coll::fn_item_kind`)。
            Some(Tgt::Fn(_)) if coll::fn_item_kind(self, e).is_some() => Ok(self.unit()),
            _ => self.err(line, format!("名前 {} は段階 1 の対象外", segs.join("."))),
        }
    }

    /// 整数か bool のリテラルで書かれた定数 (`const f.N: u64 = 27u64`) と、
    /// 文字列のリテラルで書かれた定数 (`const f.CONFIG: &str = "..."`) を値として扱う。
    /// 定数の値はコンパイル時に分かるので、使う場所で値を作る。
    fn const_val(&mut self, ci: usize, e: &V) -> LR<VId> {
        let c = &self.p.defs[ci];
        // `for x in K.split(..)` の `K` は下ろしが作った式 (番号 0) で型表にない。定数の宣言の型を使う。
        let ty = if nid(e) == 0 {
            let src = from_v(&c.ret, &HashSet::new());
            match coll::value_ty(self, &src).or_else(|| self.conv_t(&src)) {
                Some(t) => t,
                None => return self.err(nline(e), format!("定数 {} の型が決まらない", c.name)),
            }
        } else {
            self.ty_of(e)?
        };
        match c.init.as_ref() {
            Some(init) if nname(init) == "Lit" && st(&nf(init)[0]) == "int" && ty.is_int() => {
                let v = parse_int(&nf(init)[1]).ok_or_else(|| "定数の値が扱えない".to_string())?;
                if !ty.in_range(v) {
                    return self.err(nline(e), "定数の値が型の範囲にない");
                }
                Ok(self.emit(ty, Op::Const(v)))
            }
            // 浮動小数の定数 (`f64.from_bits(0x7ff8000000000000u64)`): ビット列の整数定数を `Bitcast` で浮動小数にする。
            Some(init) if ty.is_float() && nname(init) == "Call" && matches!(nname(&nf(init)[0]), "Path")
                && strs(&nf(&nf(init)[0])[0]) == [ty.name().to_string(), "from_bits".to_string()]
                && items(&nf(init)[1]).len() == 1
                && nname(&items(&nf(init)[1])[0]) == "Lit" && st(&nf(&items(&nf(init)[1])[0])[0]) == "int" => {
                let bits = parse_int(&nf(&items(&nf(init)[1])[0])[1]).ok_or_else(|| "定数の値が扱えない".to_string())?;
                let src = if ty == Ty::F64 { Ty::U64 } else { Ty::U32 };
                let a = self.emit(src, Op::Const(bits));
                Ok(self.emit(ty, Op::Bitcast { src, a }))
            }
            Some(init) if nname(init) == "Lit" && st(&nf(init)[0]) == "str" && ty == self.fat_ty() => {
                coll::str_lit(self, init)
            }
            // 整数リテラルのフィールドだけの構造体リテラルの定数 (`std.alloc.SYSTEM = std.alloc.System { id: 0u8 }`)。
            Some(init) if nname(init) == "StructLit" => {
                let Ty::Agg(ai) = ty else { return self.err(nline(e), format!("定数 {} の型が構造体でない", c.name)) };
                let crate::kir::AggKind::Struct(decl) = self.agg_def(ai).kind else { return self.err(nline(e), format!("定数 {} の型が構造体でない", c.name)) };
                let mut vals: Vec<Option<VId>> = vec![None; decl.len()];
                for it in items(&nf(init)[1]) {
                    let V::L(xs) = it else { return self.err(nline(e), format!("定数 {} の形が対象外", c.name)) };
                    let (fname, val) = (st(&xs[0]), &xs[1]);
                    let k = decl.iter().position(|x| x.name == fname);
                    match (k, nname(val)) {
                        (Some(k), "Lit") if st(&nf(val)[0]) == "int" && decl[k].ty.is_int() => {
                            let v = parse_int(&nf(val)[1]).ok_or_else(|| "定数の値が扱えない".to_string())?;
                            if !decl[k].ty.in_range(v) {
                                return self.err(nline(e), "定数の値が型の範囲にない");
                            }
                            vals[k] = Some(self.emit(decl[k].ty, Op::Const(v)));
                        }
                        _ => return self.err(nline(e), format!("定数 {} は段階 1 の対象外 (整数のフィールドだけ)", c.name)),
                    }
                }
                let Some(fields) = vals.into_iter().collect::<Option<Vec<VId>>>() else {
                    return self.err(nline(e), format!("定数 {} のフィールドが足りない", c.name));
                };
                Ok(self.emit(ty, Op::Make { variant: None, fields }))
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
                if matches!(op.as_str(), "==" | "!=") {
                    if let Some(r) = self.string_eq_locals(l, r, op == "!=")? {
                        return Ok(Some(r));
                    }
                }
                if matches!(op.as_str(), "<" | "<=" | ">" | ">=") {
                    if let Some(r) = self.string_order(l, r, op.as_str(), line)? {
                        return Ok(Some(r));
                    }
                }
                let Some(a) = self.expr(l)? else { return Ok(None) };
                let Some(b) = self.expr(r)? else { return Ok(None) };
                // `Some(c) => c == &y` の `&y` は読んでから比べる。比べる型は emit した値の型で決める。
                // `&str` どうしの `==` / `!=` は、バイト列の等しさ (`str.eq` と同じ)。
                if matches!(op.as_str(), "==" | "!=") && self.is_str_ref(l) && self.is_str_ref(r) {
                    let eq = self.str_eq(a, b)?;
                    return Ok(Some(if op == "==" { eq } else { self.emit(Ty::Bool, Op::Not(eq)) }));
                }
                let (a, b) = self.load_scalar_ref_pair(a, b);
                let t = self.vty(a);
                if self.vty(b) != t {
                    return self.err(line, "比較の両辺の型が違う");
                }
                if matches!(op.as_str(), "==" | "!=") && (matches!(t, Ty::Opt(_) | Ty::Res(..) | Ty::Agg(_))) {
                    return Ok(Some(self.eq_by_call(l, r, a, b, op == "!=", line)?));
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
                // `panics never` の関数でも、正のリテラルでの `/` `%` は溢れも 0 除算も起きない (`x / 2usize`)。
                let safe_div = matches!(op.as_str(), "/" | "%")
                    && nname(r) == "Lit"
                    && st(&nf(r)[0]) == "int"
                    && parse_int(&nf(r)[1]).is_some_and(|v| v > 0);
                if !safe_div && !self.def.panics.as_deref().map_or(true, |p| p == "may") {
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

    /// `x.m(..)` / `x.f.m(..)` の受け手の文字列の種類 (`&str` / `String`)。局所の種類、または欄の型から決める。
    fn str_kind_of_path(&self, s: usize, segs: &[String]) -> Option<coll::Kind> {
        if segs.len() == 2 {
            if let Some(k) = self.coll.kinds.get(&s) {
                if matches!(k, coll::Kind::String | coll::Kind::Str) {
                    return Some(k.clone());
                }
            }
            // 種類を結び付けていない局所: `&str` の値 (fat) か、`String` のヘッダ (集成体 `vec` の値・借用の番地)。
            let t = self.slots[s].1;
            let plain_vec = |n: &str| n == "vec";
            return match t {
                t if coll::is_str_value(self, t) => Some(coll::Kind::Str),
                Ty::Agg(i) | Ty::Ref(_, kir::Pointee::Agg(i)) if plain_vec(&self.aggs.borrow().defs[i as usize].name) => Some(coll::Kind::String),
                Ty::Ptr => Some(coll::Kind::String),
                _ => None,
            };
        }
        let mut t = own::field_src(self, s, &segs[1..segs.len() - 1])?;
        while let Src::Ref(_, i) = t {
            t = *i;
        }
        match t {
            Src::Prim(p) if p == "str" => Some(coll::Kind::Str),
            Src::Named(n, _) if n == "alloc.string.String" => Some(coll::Kind::String),
            _ => None,
        }
    }

    fn call(&mut self, e: &V) -> LR<Option<VId>> {
        if let Some(r) = self.const_str_call(e) {
            return r;
        }
        if let Some(r) = self.const_scalar_call(e) {
            return r;
        }
        if let Some(r) = self.trait_side_call(e) {
            return r;
        }
        if let Some(r) = dyn_lower::try_call(self, e) {
            return r;
        }
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
        if let Some(r) = sync_lower::sync_call(self, e) {
            return r;
        }
        if let Some(r) = chan_lower::chan_call(self, e) {
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
        let callee = &f[0];
        if nname(callee) != "Path" {
            return self.err(line, "式を呼ぶ形は段階 1 の対象外");
        }
        let segs = strs(&nf(callee)[0]);
        let args = items(&f[1]);
        // `std.task.block_on` / `spawn_local` / `yield_now` (仕様 5.6)。
        if let Some(r) = task_lower::path_call(self, &segs, args, line) {
            return r;
        }
        // `std.thread.spawn(allocator, closure)`。
        if segs == ["std", "thread", "spawn"] {
            let Some(r) = thread_lower::spawn_call(self, e, &segs, args, line) else {
                return self.err(line, "std.thread.spawn は段階 1 の対象外");
            };
            return r;
        }
        // `x.m(..)` (x は利用者の集成体の局所変数で、`m` がそのメソッド): 受け手は局所の番地 (Kasane 版の HashMap など)。
        if segs.len() == 2 {
            if let Some(s) = self.lookup(&segs[0]) {
                let rt = self.slots[s].1;
                if let Some(di) = self.method_of_ty(rt, &segs[1]) {
                    let args = items(&f[1]).to_vec();
                    return self.call_method(e, di, method_lower::Place::Path(vec![segs[0].clone()]), &args);
                }
            }
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
                    if matches!(meth.as_str(), "accept" | "read" | "write" | "flush" | "close" | "local_port" | "set_read_timeout" | "set_write_timeout" | "shutdown" | "peer_addr_raw" | "local_addr_raw" | "send_to" | "recv_from_raw" | "fd") {
                        // `x.close()` は値の受け手 (move)。
                        if meth == "close" {
                            self.own_emit(&segs[0], s, if segs.len() == 2 { crate::kir::OwnKind::Consume } else { crate::kir::OwnKind::Part });
                        }
                        let args = items(&f[1]).to_vec();
                        return net_method(self, e, &meth, recv, rt, &args, line);
                    }
                }
                if is_proc_ty(self, rt) {
                    let meth = segs[segs.len() - 1].clone();
                    let args = items(&f[1]).to_vec();
                    if let Some(r) = proc_method(self, &meth, recv, rt, &args, line)? {
                        return Ok(Some(r));
                    }
                }
                if is_fs_ty(self, rt) {
                    let meth = segs[segs.len() - 1].clone();
                    if matches!(meth.as_str(), "close" | "flush" | "read" | "sync" | "write") {
                        if meth == "close" {
                            self.own_emit(&segs[0], s, if segs.len() == 2 { crate::kir::OwnKind::Consume } else { crate::kir::OwnKind::Part });
                        }
                        let args = items(&f[1]).to_vec();
                        return fs_method(self, e, &meth, recv, rt, &args, line);
                    }
                }
                let meth = segs[segs.len() - 1].clone();
                if meth == "clone" {
                    return self.lower_clone(line, recv, rt);
                }
                // `task.join_task()`: 実行器のタスクの結果を取り出す (中断点、仕様 5.6)。
                if meth == "join_task" && args.len() == 0 {
                    if let Some(r) = task_lower::join_call(self, recv, rt, line) {
                        return r;
                    }
                }
                // `task.cancel()`: 実行器のタスクを取り消す (仕様 5.6 の第 4 段)。
                if meth == "cancel" && args.len() == 0 {
                    if let Some(r) = task_lower::cancel_call(self, recv, rt, line) {
                        return r;
                    }
                }
                // `handle.join()`: スレッドの結果を取り出す。
                if meth == "join" {
                    if args.len() != 0 {
                        return self.err(line, "join の引数は取れない");
                    }
                    if let Some(r) = thread_lower::join_call(self, e, recv, rt, line) {
                        // `join` は値の受け手 (move)。スレッドの値を取り出すとき、束縛の義務を果たす。
                        self.own_emit(&segs[0], s, if segs.len() == 2 { crate::kir::OwnKind::Consume } else { crate::kir::OwnKind::Part });
                        return r;
                    }
                }
                // `&u32` / `&mut u32` の受け手は、整数メソッドの前に参照を外す (自動の参照外し)
                let (recv, rt) = match rt {
                    Ty::Ref(_, p) if p.ty().is_int() => (self.emit(p.ty(), Op::Load(recv)), p.ty()),
                    _ => (recv, rt),
                };
                if meth == "try_clone" && args.is_empty() && rt.is_int() {
                    return self.int_cmp_named("try_clone", recv, recv, line);
                }
                if matches!(meth.as_str(), "cmp" | "eq") && args.len() == 1 && rt.is_int() {
                    let Some(b) = self.expr(&args[0])? else { return Ok(None) };
                    return self.int_cmp_named(&meth, recv, b, line);
                }
                if meth == "hash" && args.len() == 1 && rt.is_int() {
                    let Some(st) = self.expr(&args[0])? else { return Ok(None) };
                    return Ok(Some(self.int_hash(recv, st, line)?));
                }
                // `&str` の値 (fat、または `&&str` の番地) の `eq` / `hash`: 文字列の下ろし。
                if matches!(meth.as_str(), "eq" | "hash" | "cmp" | "partial_cmp" | "try_clone") {
                    if let Some(k) = self.str_kind_of_path(s, &segs) {
                        let args = items(&f[1]).to_vec();
                        return coll::method_on(self, e, line, None, k, recv, &meth, &args);
                    }
                }
                // `x.f.checked_*(..)` / `x.f.trunc_to_*()`: フィールドの数値の受け手。
                if enum_lower::is_checked_name(&meth) {
                    let args = items(&f[1]).to_vec();
                    return self.checked_with(e, recv, &meth, &args);
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
        if matches!(callee_name.as_str(), "std.net.resolve_raw" | "std.net.UdpSocket.bind") {
            return net_free_call(self, e, &callee_name, line);
        }
        if matches!(callee_name.as_str(), "std.fs.create" | "std.fs.open" | "std.fs.exists" | "std.fs.remove_file" | "std.fs.read_to_end" | "std.fs.open_append" | "std.fs.rename" | "std.fs.create_dir" | "std.fs.remove_dir" | "std.fs.stat_raw" | "std.fs.read_dir_raw") {
            return fs_call(self, e, &callee_name, args, line);
        }
        if callee_name == "std.process.Command.new" {
            return proc_call(self, args, line);
        }
        if callee_name == "std.process.exit" {
            return self.process_exit(args, line);
        }
        if let Some(r) = env_call(self, e, &callee_name, args, line)? {
            return Ok(Some(r));
        }
        if let Some(Tgt::Variant(en, vn)) = &target {
            let (en, vn) = (en.clone(), vn.clone());
            return self.variant_call(e, &en, &vn);
        }
        // `HashMap::new(a)` / `BtreeMap::new(a)` (組み込みの宣言だけ) は、Kasane 版の `new_empty` (本体あり) を呼ぶ。
        if matches!(callee_name.as_str(), "alloc.collections.HashMap.new" | "alloc.collections.BtreeMap.new") {
            let alt: Vec<String> = format!("{}_empty", callee_name).split('.').map(String::from).collect();
            let Some(Tgt::Fn(di)) = self.p.resolve(&alt, &self.def.file).map(|r| r.target) else {
                return self.err(line, format!("{} の Kasane 版がない", callee_name));
            };
            return self.call_def(e, di, args, line);
        }
        // `core.option.Option.map(o, f)` / `and_then(o, f)` (完全修飾の呼び出し)。
        if matches!(callee_name.as_str(), "core.option.Option.map" | "core.option.Option.and_then") && args.len() == 2 {
            let Some(recv) = self.expr(&args[0])? else { return Ok(None) };
            if matches!(self.vty(recv), Ty::Opt(_)) {
                return self.opt_map(e, recv, callee_name.ends_with("and_then"), &args[1..]);
            }
        }
        if matches!(callee_name.as_str(), "core.option.Option.unwrap_or" | "core.result.Result.unwrap_or") && args.len() == 2 {
            let Some(recv) = self.expr(&args[0])? else { return Ok(None) };
            if matches!(self.vty(recv), Ty::Opt(_) | Ty::Res(..)) {
                return self.optres_on(e, recv, false, "unwrap_or", &args[1..]);
            }
        }
        let Some(Tgt::Fn(di)) = target else {
            return self.err(line, format!("呼び出し {} は段階 1 の対象外", segs.join(".")));
        };
        self.call_def(e, di, args, line)
    }

    /// 関数の定義 `di` への呼び出し (総称関数は型引数を推論して単相インスタンスにする)。
    fn call_def(&mut self, e: &V, di: usize, args: &[V], line: usize) -> LR<Option<VId>> {
        let cd = &self.p.defs[di];
        if cd.kw != "fn" || cd.body.is_none() {
            return self.err(line, format!("呼び先 {} は段階 1 の対象外 (メソッド・本体なし)", cd.name));
        }
        let (name, tmap) = if cd.generics.iter().any(|g| g.0 != "life") {
            let tm = self.mono_targs(e, di, args, 0, &nf(e)[2], None)?;
            (self.mono_instance(di, &tm, line)?, tm)
        } else {
            (cd.name.clone(), HashMap::new())
        };
        let gn = mono::tnames(cd);
        let mut pts = vec![];
        for (_, t) in &cd.params {
            match coll::sig_ty(self, &mono::subst_ty(&from_v(t, &gn), &tmap)) {
                Some(x) => pts.push(x),
                None => return self.err(line, format!("呼び先 {} の引数の型は段階 1 の対象外", cd.name)),
            }
        }
        // `-> !` の呼び先は戻らない: 結果は持たず、呼び出しの後ろは到達しない (`panic` と同じ扱い)。
        let diverges = !is_nil(&cd.ret) && matches!(mono::subst_ty(&from_v(&cd.ret, &gn), &tmap), Src::Never);
        let rt = if is_nil(&cd.ret) || diverges {
            Ty::Unit
        } else {
            match coll::sig_ty(self, &mono::subst_ty(&from_v(&cd.ret, &gn), &tmap)) {
                Some(x) => x,
                None => return self.err(line, format!("呼び先 {} の戻り値の型は段階 1 の対象外", cd.name)),
            }
        };
        if pts.len() != args.len() {
            return self.err(line, format!("呼び先 {} の引数の個数が違う", cd.name));
        }
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
        let res = self.emit(rt, Op::Call { callee: name, args: vs });
        if diverges {
            self.terminate(Term::Unreachable);
            return Ok(None);
        }
        Ok(Some(res))
    }

    /// `std.process.exit(code)`: 終了コード `code` (i32) で戻らずに終わる。後ろは到達しない (`Ok(None)`)。
    fn process_exit(&mut self, args: &[V], line: usize) -> LR<Option<VId>> {
        if args.len() != 1 {
            return self.err(line, "std.process.exit の引数は 1 個のはず");
        }
        let Some(c) = self.expr(&args[0])? else { return Ok(None) };
        if self.vty(c) != Ty::I32 {
            return self.err(line, "std.process.exit の引数は i32 だけ段階 1 の対象");
        }
        self.emit(Ty::Unit, Op::Exit { code: c });
        self.terminate(Term::Unreachable);
        Ok(None)
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
        if st(&f[1]) == "join_task" && items(&f[2]).is_empty() {
            let t = self.ty_of(&f[0])?;
            let Some(a) = self.expr(&f[0])? else { return Ok(None) };
            if let Some(r) = task_lower::join_call(self, a, t, line) {
                return r;
            }
        }
        // `h.cancel()` (仕様 5.6 の第 4 段)。
        // 受け手の型で先に選ぶ (対象外の `cancel` は式を二度評価しないように、ここで止める)。
        if st(&f[1]) == "cancel" && items(&f[2]).is_empty() {
            let t = self.ty_of(&f[0])?;
            if task_lower::is_task_ty(self, t) {
                let Some(a) = self.expr(&f[0])? else { return Ok(None) };
                if let Some(r) = task_lower::cancel_call(self, a, t, line) {
                    return r;
                }
            }
        }
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
        if let Some(r) = self.trait_side_mcall(e) {
            return r;
        }
        if let Some(r) = dyn_lower::try_mcall(self, e) {
            return r;
        }
        if let Some(r) = coll::try_mcall(self, e) {
            return r;
        }
        // `std.net.TcpListener` / `TcpStream` のメソッド (accept / read / write / flush / close)。
        if let Ok(rt) = self.src_ty(&nf(e)[0]) {
            let is_net = match &rt {
                Src::Named(n, _) if n == "std.net.TcpListener" || n == "std.net.TcpStream" || n == "std.net.UdpSocket" => true,
                Src::Ref(_, inner) => {
                    matches!(&**inner, Src::Named(n, _) if n == "std.net.TcpListener" || n == "std.net.TcpStream" || n == "std.net.UdpSocket")
                }
                _ => false,
            };
            if is_net {
                let m = st(&nf(e)[1]);
                if matches!(m.as_str(), "accept" | "read" | "write" | "flush" | "close" | "local_port" | "set_read_timeout" | "set_write_timeout" | "shutdown" | "peer_addr_raw" | "local_addr_raw" | "send_to" | "recv_from_raw" | "fd") {
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
        if let Some(r) = self.builtin_mcall(e) {
            return r;
        }
        if let Some(done) = self.scalar_mcall(e)? {
            return Ok(done);
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
            let temp_mark = self.coll.temps.len();
            let guard_mark = coll::guard_mark(self);
            // `let r = &temp()` の一時値の寿命は束縛のスコープまで延びる (文の終わりでは尽きない)。
            let extended = nname(s) == "Let" && nf(s).get(3).map_or(false, |init| nname(init) == "Unary" && st(&nf(init)[0]).starts_with('&'));
            let r = self.stmt(s)?;
            // 文の終わりで、束縛されなかった `Mutex` のガードを外す (4.4: 一時の寿命は文の終わり)。
            coll::end_guards(self, guard_mark);
            // 文の終わりで一時値の寿命が尽きる (借用検査の注釈)。
            for h in self.coll.temps.split_off(temp_mark) {
                if self.cur.is_some() && !extended {
                    self.emit(Ty::Unit, Op::ScopeEnd(h));
                }
            }
            match r {
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
        // 局所変数の寿命の終わり (借用検査の注釈)。Vec / String のヘッダは `block_end` の解放が出す。ここはスタックスロットのスカラー。
        if self.cur.is_some() {
            let locals: Vec<usize> = self.scopes.last().map(|sc| sc.iter().map(|x| x.1).collect()).unwrap_or_default();
            for s in locals.into_iter().rev() {
                let v = self.slots[s].0;
                if self.is_mem_val(v) {
                    self.emit(Ty::Unit, Op::ScopeEnd(v));
                }
            }
        }
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
                // `let r = core.iter.range(a, b)`: 端だけ評価して覚える (反復子の値は作らない。`fold` / `count` が使う)。
                if let Some((a, b)) = iter_lower::range_args(&f[3]) {
                    let Some(start) = self.expr(a)? else { return Ok(St::Div) };
                    let Some(end) = self.expr(b)? else { return Ok(St::Div) };
                    let ty = self.vty(start);
                    if ty.is_int() && self.vty(end) == ty {
                        coll::bind_owned(self, &name, start, ty);
                        if let Some(sl) = self.lookup(&name) {
                            self.coll.kinds.insert(sl, coll::Kind::Range { start, end, ty });
                        }
                        return Ok(St::Unit);
                    }
                    return self.err(line, "core.iter.range の範囲は同じ整数の型だけ段階 1 の対象");
                }
                let Some(v) = self.expr(&f[3])? else { return Ok(St::Div) };
                let t = self.vty(v);
                coll::bind_owned(self, &name, v, t);
                coll::bind_guard(self, &name, v);
                coll::after_let(self, &name, &f[3])?;
                // 局所変数の move・線形な値の消費の義務を追う (注釈)。
                if let (Ok(it), Some(s)) = (self.src_ty(&f[3]), self.lookup(&name)) {
                    self.own_declare(&name, s, &it);
                }
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
                        // `v[i].f = e` (所有物を持つ要素の Vec): 要素の可変の借用の先のフィールドへ書く (設計_KIR.md 17.15)
                        if let Some(r) = coll::assign_elem_field(self, target, &f[1], line) {
                            return r;
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
                // 再代入: move 済みが解け、線形なら新しい値の義務を負う。
                let linear = self.coll.own_ids.contains_key(&(name.clone(), s_idx)) && self.src_ty(&f[1]).map_or(false, |t| self.src_is_linear(&t, &mut HashSet::new()));
                self.own_emit(&name, s_idx, if linear { kir::OwnKind::Owe } else { kir::OwnKind::Init });
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
        let gm = coll::guard_mark(self);
        let Some(c) = self.expr(&f[0])? else { return Ok(None) };
        // 条件の中の一時のガードは、分岐の前に外す。
        coll::end_guards(self, gm);
        if self.vty(c) != Ty::Bool {
            return self.err(line, "if の条件が bool ではない");
        }
        // 結果の型。両枝とも発散するなら `!` で、結果の値は持たない。unit の値は合流の引数に載せない。
        // else が無い `if` は値が unit (then 枝の値は捨てる)。検査器はこの形の型を then 枝の型にすることがあるので、
        // そのまま使うと合流の引数 (else 側の unit) と型が合わなくなる。
        let no_else = matches!(f[2], V::Nil);
        let res: Option<Ty> = match self.src_ty(e)? {
            _ if no_else => Some(Ty::Unit),
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
            if carry && self.vty(v) != res.unwrap() {
                return self.err(line, "if の then 枝の型が結果の型と違う");
            }
            if !carry {
                // 結果を運ばない枝の値は捨てる (所有物なら解放する)。
                coll::drop_temp(self, v);
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
            self.rebind_slot(*s, jp[off + i], slot_tys[i]);
        }
        Ok(Some(if carry { jp[0] } else { self.unit() }))
    }

    /// 局所変数 `s` の値をブロックの引数 `v` に替える (Vec / String のヘッダの型 `hdr_ty` を引き継ぐ)。
    pub(super) fn rebind_slot(&mut self, s: usize, v: VId, t: Ty) {
        let old = self.slots[s].0;
        if let Some(h) = self.coll.hdr_ty.get(&old).copied() {
            self.coll.hdr_ty.entry(v).or_insert(h);
        }
        self.slots[s] = (v, t);
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
            self.rebind_slot(*s, hp[i], tys[i]);
        }
        let (xb, xp) = self.new_block(&tys);
        let mut body_start = hb;
        if let Some(c) = cond {
            let gm = coll::guard_mark(self);
            let Some(cv) = self.expr(c)? else { return Ok(None) };
            // 条件の中の一時のガードは、毎回の条件の評価の後で外す (ループの各回で対になる)。
            coll::end_guards(self, gm);
            if self.vty(cv) != Ty::Bool {
                return self.err(nline(e), "while の条件が bool ではない");
            }
            let (bb, _) = self.new_block(&[]);
            let exit_args = self.vals_of(&slots);
            self.branch(cv, (bb, vec![]), (xb, exit_args));
            body_start = bb;
        }
        self.loops.push(LoopCx { header: hb, exit: xb, slots: slots.clone(), depth: self.scopes.len(), cont: vec![] });
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
            self.rebind_slot(*s, xp[i], tys[i]);
        }
        Ok(Some(self.unit()))
    }

    fn jump_loop(&mut self, e: &V, is_break: bool) -> LR<Option<VId>> {
        let Some(lc) = self.loops.last() else {
            return self.err(nline(e), "ループの外の break / continue");
        };
        let (target, slots, depth) = (if is_break { lc.exit } else { lc.header }, lc.slots.clone(), lc.depth);
        let lead = if is_break { vec![] } else { lc.cont.clone() };
        coll::drop_from(self, depth);
        // `break` / `continue` で抜けるスコープのスタックスロットの局所変数の寿命の終わり (借用検査の注釈)。
        if self.cur.is_some() {
            let locals: Vec<usize> = self.scopes.iter().skip(depth).flat_map(|sc| sc.iter().map(|x| x.1)).collect();
            for sl in locals.into_iter().rev() {
                let v = self.slots[sl].0;
                if self.is_mem_val(v) {
                    self.emit(Ty::Unit, Op::ScopeEnd(v));
                }
            }
        }
        let mut args = lead;
        args.extend(self.vals_of(&slots));
        self.goto(target, args);
        Ok(None)
    }

    fn name_ty_dbg(&self, t: Ty) -> String {
        let a = self.aggs.borrow();
        match t {
            Ty::Agg(i) => a.defs[i as usize].name.clone(),
            Ty::Opt(crate::kir::Pay::Agg(i)) => format!("Option<{}>", a.defs[i as usize].name),
            _ => crate::kir::fmt_ty(t),
        }
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
            return self.err(line, format!("return の値の型が戻り値の型と違う ({} / {})", self.name_ty_dbg(self.vty(v)), self.name_ty_dbg(self.ret)));
        }
        coll::drop_from(self, 0);
        self.terminate(Term::Ret(v));
        Ok(None)
    }

    /// `with [mut] 名前 = 初期化式 close 後始末式 { 本体 }` (仕様 4.4)。
    /// 本体を抜けるあらゆる経路 (末尾 / `return` / `?` / `break` / `continue`) で、
    /// 出口の値を決めたあとに後始末式をちょうど 1 回実行する。panic とプロセス終了は
    /// 実行しない。初期化式が失敗したら束縛も後始末もない。
    fn with_(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let name = st(&nf(&f[0])[0]);
        let init = &f[2];
        let close_expr = &f[3];
        let body = &f[4];

        // 1. 初期化式を評価。`?` などで抜けたら束縛も後始末もない。
        let Some(v) = self.expr(init)? else { return Ok(None) };
        let t = self.vty(v);

        // 2. 束縛のスコープを作り、束縛 → 後始末式の順に登録する。出口 (`return` / `?` / `break` /
        //    `continue`) は `drop_from` が登録の逆順に解放するので、後始末式 → 束縛の解放の順になり、
        //    出口の終端を置く前に後始末式が下ろされる (木走査と同じ)。
        self.scopes.push(vec![]);
        coll::bind_owned(self, &name, v, t);
        // 束縛した資源の move・線形の義務を追う (注釈)。後始末式の中の消費 (`r.close()` / `t.close(r)`) が義務を果たす。
        let own_slot = self.lookup(&name);
        if let (Ok(it), Some(s)) = (self.src_ty(init), own_slot) {
            self.own_declare(&name, s, &it);
        }
        coll::register_close(self, close_expr.clone(), own_slot.map(|s| (name.clone(), s)));

        // 3. 本体。
        let r = self.expr(body);
        let r = match r {
            Ok(r) => r,
            Err(e) => {
                self.scopes.pop();
                return Err(e);
            }
        };

        // 4. 末尾で普通に抜ける経路: 本体の値を決めたあとに後始末式、続けて束縛の解放。
        if r.is_some() {
            coll::block_end(self);
        } else {
            let depth = self.scopes.len() - 1;
            self.coll.owned.retain(|(d, _)| *d < depth);
        }
        self.scopes.pop();
        if let Some(e) = self.coll.close_err.take() {
            return Err(e);
        }
        Ok(r)
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

/// テストの入口が宣言として持つ効果 (`test` には uses の宣言がない)。`ksn check` は `test` の本体で効果を見ない
/// (check.rs の `check_fn_sema` で `cur_uses = None`) ので、入口はこの全部の効果を持つ関数として下ろす。
const ALL_EFFECTS: [&str; 9] = ["alloc", "env", "io.console", "io.fs", "io.net", "process", "rand", "thread", "time"];

/// `ksn test` (`--both` の LLVM 側・`--engine=vm`) の入口: テスト本体 `test_def` (定義の添字) を入口にして下ろす。
/// テストは引数を持たず、戻り値は `()` (`test` の本体)。入口の名前はテストの FQN。
/// 入口は全部の効果を持つ (上の `ALL_EFFECTS`)。呼び先の uses を満たさないことで下ろせなくなるのを避ける。
pub fn lower_test(p: &Program, user: &[usize], test_def: usize) -> Lowered {
    lower_defs(p, user, Some(&p.defs[test_def].name), Some(test_def))
}

fn lower_defs(p: &Program, user: &[usize], entry: Option<&str>, test_def: Option<usize>) -> Lowered {
    LINES.with(|l| l.borrow_mut().clear());
    task_lower::reset();
    // このパッケージから呼び出す依存パッケージ (別 module) の fn も下ろし対象にする。
    // そうしなければ、依存先が「呼び先が未対応」となり、このパッケージの関数も下ろせなくなる。
    let mut user_vec: Vec<usize> = user.to_vec();
    let cur_module = entry
        .and_then(|e| p.funcs.get(e).copied())
        .or_else(|| user.first().copied())
        .and_then(|i| Some(p.defs[i].module.clone()));
    if let Some(m) = cur_module {
        for (i, d) in p.defs.iter().enumerate() {
            if d.kw == "fn"
                && d.body.is_some()
                && d.module != m
                && !user_vec.contains(&i)
            {
                user_vec.push(i);
            }
        }
    }
    let user = &user_vec;
    let tys = type_table(p, user, test_def);
    let aggs = RefCell::new(agg::AggTab::new(user));
    // `std.net.listen`/`connect`/`accept` が返すソケット集成体と、その失敗の `std.net.Error` を、誰も参照しなくても型表に入れる。
    for name in ["std.net.TcpListener", "std.net.TcpStream", "std.net.UdpSocket", "std.net.Shutdown", "std.net.Error"] {
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
    // スレッドの補助の関数 (`ksn.thr.*`) は、全部の入口 (クロージャ) が揃ってから作る。
    thread_lower::finish(&mut ok);
    chan_lower::finish(&mut ok);
    // `std.task` の実行器 (使われたときだけ)。閉包の表は、この後の変換 (`kir_suspend::transform`) が `.poll` を作る。
    task_lower::finish(&mut ok);
    // `extern` 宣言を KIR の関数宣言 (本体なし) にする。リンク名は `Func.link` に持つ。
    // 同梱の `std` の extern (`std.tls` の OpenSSL の関数) も、呼ぶ側の関数の呼び先として下ろす (`user` に入らないため)。
    let mut externs: Vec<usize> = user.to_vec();
    for (i, d) in p.defs.iter().enumerate() {
        if d.kw == "extern" && d.name.starts_with("std.") && !externs.contains(&i) {
            externs.push(i);
        }
    }
    for &i in &externs {
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
    // `Box<dyn Tr>` の呼び出しと解放の補助関数 (登録された変換と呼び出しがそろった後に作る)。
    if let Some(&i0) = user.first() {
        dyn_lower::synth(p, &tys, &aggs, &p.defs[i0], &mut ok);
        drop_lower::synth(p, &tys, &aggs, &p.defs[i0], &mut ok);
    }
    // 同じ単相インスタンスを、別々の呼び出し元が下ろしていることがある (内容は同じ)。鍵の重複は先の一つだけ残す。
    let mut seen: HashSet<String> = HashSet::new();
    ok.retain(|(f, _)| seen.insert(f.key.clone()));
    let (funcs, all_aggs) = loop {
        let keys: HashSet<String> = ok.iter().map(|(f, _)| f.key.clone()).collect();
        let mut drop: Vec<(String, String)> = vec![];
        for (f, callees) in &ok {
            if let Some(c) = callees.iter().find(|c| !keys.contains(*c)) {
                drop.push((f.key.clone(), format!("呼び先 {} が未対応", c)));
            }
        }
        if drop.is_empty() {
            let mut m = Module { funcs: ok.iter().map(|(f, _)| f.clone()).collect(), aggs: aggs.borrow().defs.clone() };
            // 中断する関数と閉包の本体を、枠の形 (`.new` / `.poll`) にする (設計_KIR.md 26 章)。変換できなければ落とす。
            drop.extend(kir_suspend::transform(&mut m, &task_lower::task_keys()));
            for (key, msg) in kir::verify_module(&m) {
                drop.push((key, format!("下ろしの検査に失敗: {}", msg)));
            }
            if drop.is_empty() {
                break (m.funcs, m.aggs);
            }
        }
        ok.retain(|(f, _)| !drop.iter().any(|(k, _)| *k == f.key));
        bad.extend(drop);
    };
    let mut funcs = funcs;
    funcs.sort_by(|a, b| a.key.cmp(&b.key));
    bad.sort();
    let mut module = Module { funcs, aggs: all_aggs };
    kir::prune_aggs(&mut module);
    Lowered { module, unsupported: bad, lines: LINES.with(|l| std::mem::take(&mut *l.borrow_mut())) }
}

/// 入口 `e` の戻り値が `Result<T, E>` なら、`E` の Debug 表示の関数を `ok` に足す。
fn debug_of_entry(p: &Program, aggs: &RefCell<agg::AggTab>, e: &str, ok: &mut Vec<(Func, BTreeSet<String>)>, pos: usize) -> Result<(), String> {
    // テストの本体は `fn` ではないので、定義が無くてもよい (引数も戻り値の Debug 表示も要らない)。
    let Some(d) = p.defs.iter().find(|d| d.name == e && d.kw == "fn") else {
        return if matches!(ok[pos].0.ret, Ty::Res(..)) { Err(format!("入口 {} の定義がない", e)) } else { Ok(()) };
    };
    if !d.params.is_empty() {
        // 引数のある入口は、木走査と同じく「引数の個数が違う」panic で終わる入口に置き換える (引数を渡す経路がない)。
        let msg = format!("arity mismatch calling {}: expected {}, got 0", e, d.params.len());
        let f = &mut ok[pos].0;
        f.blocks = vec![Block {
            params: vec![],
            insts: vec![Inst { dst: 0, ty: Ty::Unit, op: Op::Panic { msg, arg: None } }],
            term: Term::Unreachable,
        }];
        f.may_panic = true;
    }
    // 戻り値が `()` の入口 (テストの本体を含む) は、Debug 表示の関数を要らない。
    let Ty::Res(_, err) = ok[pos].0.ret else { return Ok(()) };
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

/// 完全修飾のメソッド呼び出し `core.num.i32.checked_shl(x, n)` / `core.slice.get(&a, i)` を、受け手を第 1 引数にしたメソッド呼び出し
/// (`x.checked_shl(n)` / `a.get(i)`) の木に直す (式 ID は同じ)。直す所がなければ `None`。
fn ufcs_rewrite(v: &V, p: &crate::program::Program, file: &str) -> Option<V> {
    match v {
        V::Node(n, fs, line, id) => {
            let mut changed = false;
            let mut fs2: Vec<V> = fs
                .iter()
                .map(|x| match ufcs_rewrite(x, p, file) {
                    Some(y) => {
                        changed = true;
                        y
                    }
                    None => x.clone(),
                })
                .collect();
            if *n == "Call" && fs2.len() >= 2 && matches!(fs2.get(2), Some(V::Nil) | None) && nname(&fs2[0]) == "Path" {
                let segs = strs(&nf(&fs2[0])[0]);
                let m = segs.last().map(|x| x.as_str()).unwrap_or("");
                let assoc = m.starts_with("from_") || m.starts_with("try_from") || m == "parse" || m == "new" || m == "with_capacity";
                let ty_ok = (segs.len() == 4 && segs[1] == "num" && Ty::parse(&segs[2]).is_some()) || (segs.len() == 3 && segs[1] == "slice");
                let args = items(&fs2[1]).to_vec();
                let bodiless = matches!(p.resolve(&segs, file).map(|r| r.target), Some(Tgt::Sig(di) | Tgt::Fn(di)) if p.defs[di].body.is_none());
                if segs[0] == "core" && ty_ok && !assoc && bodiless && !args.is_empty() {
                    // `&x` / `&mut x` の受け手は、メソッド呼び出しの自動の借用と同じなので `x` にする。
                    let mut recv = args[0].clone();
                    if nname(&recv) == "Unary" && matches!(st(&nf(&recv)[0]).as_str(), "&" | "&mut") && nname(&nf(&recv)[1]) == "Path" {
                        recv = nf(&recv)[1].clone();
                    }
                    return Some(V::Node("MCall", vec![recv, V::S(m.to_string()), V::L(args[1..].to_vec()), V::Nil], *line, *id));
                }
            }
            if changed {
                fs2.shrink_to_fit();
                Some(V::Node(n, fs2, *line, *id))
            } else {
                None
            }
        }
        V::L(xs) => {
            let mut changed = false;
            let ys: Vec<V> = xs
                .iter()
                .map(|x| match ufcs_rewrite(x, p, file) {
                    Some(y) => {
                        changed = true;
                        y
                    }
                    None => x.clone(),
                })
                .collect();
            changed.then(|| V::L(ys))
        }
        _ => None,
    }
}
