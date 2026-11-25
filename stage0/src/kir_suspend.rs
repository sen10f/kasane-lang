//! 中断 `suspend` のネイティブ化 (設計_KIR.md 26 章、方式 (A): 状態機械)。KIR から KIR への変換。
//!
//! 中断する関数 F (`uses` に `suspend` を持つ) と、タスクの本体 K (`std.task.spawn_local` / `block_on` の閉包) を、
//! 次の 3 つに置き換える (F が中断しない K は、元の関数はそのまま残し、`K.new` / `K.poll` だけを足す):
//! - `F.new(引数…) -> ptr`: 枠 (`heap.alloc` の領域) を作り、状態 0 と引数を入れて返す。
//! - `F.poll(fp: ptr) -> bool`: 枠の状態から再開する。中断点で止まるなら `false`、最後まで走ったら `true` (結果は枠の `RES` 位置)。
//! - `F` (元の鍵): `new` → `poll` → 結果を読む → 枠を解放、の形。実行器の外では中断しない (`yield_now` は何もしない) ので、1 回の `poll` で終わる。
//!
//! 枠の構造: `[0]` 状態 (u32)、`[8]` 結果 (戻り値の型)、以降に、中断点をまたいで生きる値・引数・関数内の確保 (`alloca` / `frame.bytes`) の領域。
//! 関数内の確保は枠の中へ移す (関数が戻るとスタックは消えるので、中断点をまたぐ参照を置けるように)。
//!
//! 中断点 `d = g(args)` (g は中断しうる) は、次の形になる:
//! - 元のブロック: `ch = g.new(args)` の後、待ちのブロック W へ。
//! - W: `r = g.poll(ch)`。`true` なら結果を読んで枠を解放し、続き (元の残りの命令) へ。`false` なら保存のブロック S へ。
//! - S: 状態 (この中断点の番号) と、中断点をまたいで生きる値 (`ch` を含む) を枠へ書き、`false` を返す。
//! - 再開のブロック R (状態が一致したときに E から来る): 枠から値を読み直して W へ。
//!
//! 生存解析の結果を辺の引数にする (`block 引数` の形)。元の KIR は値を支配関係で参照できるが、変換後は
//! 中断点をまたぐ値を全部ブロック引数で運ぶので、枠からの読み直しと保存の場所が一意になる。
//!
//! 中断点の葉: `ksn.task.yield` (`yield_now`) と `ksn.task.wait` (`Task.join_task` の待ち)。その枠の形は `EXEC_TEXT` に
//! 手で書いてある。実行器 (`ksn.task.*`) は KIR のテキストで書き、`run` が run queue を回す (設計_KIR.md 26 章)。

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::kir::{self, AggDef, AggKind, Block, Cc, Field, Func, Inst, Module, Op, Pointee, Term, Ty, VId};

/// 枠の状態 (u32) の位置と、結果の位置 (戻り値の型、8 バイト境界)。
const STATE: u32 = 0;
const RES: u32 = 8;

/// 葉の中断点 (`std.task.yield_now` と `Task.join_task` の待ち)。枠の形は `EXEC_TEXT` が書く。
pub const YIELD: &str = "ksn.task.yield";
pub const WAIT: &str = "ksn.task.wait";
/// `Task<T>.cancel` (取り消し。枠を解放して、状態を取り消しにする)。
pub const CANCEL: &str = "ksn.task.cancel";
/// `std.task.await_js` (JS の完了を待つ中断、仕様 5.6 の第 5 段。葉。`ksn_resume` が結果を入れて再開する)。
pub const AWAIT: &str = "ksn.task.await";
/// `std.task.wait_io` (ソケットの準備完了の待ち。葉。リアクターが起こす)。
pub const IO: &str = "ksn.task.io";
/// 入口の枠の置き場 (大域のバイト列、16 バイト): `[0]` 待っている枠、`[8]` 待ちの有無 (u32)。JS の完了を待つ入口が使う。
pub const WRAP_PEND: &str = "ksn.wrap.pend";
/// 実行器の入口 (`block_on` の本体) と、生成 (`spawn_local`) と、後始末。
pub const RUN: &str = "ksn.task.run";
pub const SPAWN: &str = "ksn.task.spawn_local";
/// `std.task.spawn` (`Send` の要る方)。複数スレッドの実行器が動いているときは全体の注入列へ積み、そうでなければ `SPAWN` と同じ。
pub const SPAWN_ANY: &str = "ksn.task.spawn_any";
pub const MT_BEGIN: &str = "ksn.task.mt_begin";
pub const MT_END: &str = "ksn.task.mt_end";
pub const WORKER: &str = "ksn.task.worker_main";
pub const FINISH: &str = "ksn.task.finish";
/// 枠の番地 (タスク `i`)、タスクの id の数。`join_task` の結果の読み出しに使う。
pub const FP: &str = "ksn.task.fp";
/// 名前が `ksn.task.` で始まる、または `.new` / `.poll` で終わる関数の呼び出しは、呼び出し元の `uses` と照合しない。
/// 枠の関数の効果は、呼び出し元の宣言 (閉包の `uses` や `block_on` の `E`) で既に検査してあり、KIR の検査は
/// 生成した関数どうしの照合を要求しないため (`kir::verify_func` が使う)。
pub fn exempt_uses(key: &str) -> bool {
    key.starts_with("ksn.task.") || key.ends_with(".new") || key.ends_with(".poll")
}

fn align8(n: u32) -> u32 {
    n.div_ceil(8) * 8
}

/// 変換で作る値番号の源と、値の型の表 (関数ごと)。
struct Gen<'a> {
    next: VId,
    tys: HashMap<VId, Ty>,
    aggs: &'a [AggDef],
    /// `Option` / `Result` の型 → それを 1 つ持つ集成体 (`ksn.box<..>`)。メモリへ書くときの包み。
    boxes: &'a [(Ty, Ty)],
}

impl Gen<'_> {
    fn fresh(&mut self, t: Ty) -> VId {
        let v = self.next;
        self.next += 1;
        self.tys.insert(v, t);
        v
    }

    fn inst(&mut self, out: &mut Vec<Inst>, t: Ty, op: Op) -> VId {
        let d = self.fresh(t);
        out.push(Inst { dst: d, ty: t, op });
        d
    }

    fn konst(&mut self, out: &mut Vec<Inst>, t: Ty, v: i128) -> VId {
        self.inst(out, t, Op::Const(v))
    }

    fn ty(&self, v: VId) -> Result<Ty, String> {
        self.tys.get(&v).copied().ok_or_else(|| format!("値 %{} の型がない", v))
    }

    /// 枠の値の大きさ (配置は 8 バイト境界に切り上げる)。単位型は 0。
    fn slot_size(&self, t: Ty) -> Result<u32, String> {
        Ok(match t {
            Ty::Unit => 0,
            Ty::Ref(..) | Ty::Ptr => 8,
            Ty::Agg(_) | Ty::Opt(_) => kir::size_of(self.aggs, t),
            // `Result` は枠の中ではタグ (1 バイト、0 = Ok) と、Ok / Err の 2 つのペイロードの欄に分ける (`res_helpers`)。
            Ty::Res(p, e) => res_layout(self.aggs, p.ty(), e.ty())?.1,
            _ if kir::memable(t) => kir::size_of(self.aggs, t),
            _ => return Err(format!("中断をまたぐ値の型 {} は枠に置けない", kir::fmt_ty(t))),
        })
    }
}

/// 枠の中の `Result<T, E>` の配置: タグは `[0]` (1 バイト)、Ok のペイロードは `[8]`、Err のペイロードは `[8 + Ok の欄]`。
/// 戻り値は (Err のペイロードの位置, 全体の大きさ)。
fn res_layout(aggs: &[AggDef], t: Ty, e: Ty) -> Result<(u32, u32), String> {
    let slot = |x: Ty| -> Result<u32, String> {
        Ok(match x {
            Ty::Unit => 0,
            Ty::Ref(..) | Ty::Ptr => 8,
            Ty::Agg(_) | Ty::Opt(_) | Ty::Res(..) => kir::size_of(aggs, x),
            _ if kir::memable(x) => kir::size_of(aggs, x),
            _ => return Err(format!("中断をまたぐ値の型 {} は枠に置けない", kir::fmt_ty(x))),
        })
    };
    let eo = RES_OK + align8(slot(t)?);
    Ok((eo, eo + align8(slot(e)?)))
}

/// `Result` の枠の中の、Ok のペイロードの位置 (タグの後の 8 バイト)。
const RES_OK: u32 = 8;

/// `Result<T, E>` を枠 (`base` の `off`) に書く補助関数の名前 (`put`) と読む関数 (`get`)。
pub fn res_fn_names(t: Ty) -> (String, String) {
    (format!("ksn.res.put<{}>", kir::fmt_ty(t)), format!("ksn.res.get<{}>", kir::fmt_ty(t)))
}

/// `Result<T, E>` の枠の補助関数を作る。`put(p: ptr, v: Result) -> unit` と `get(p: ptr) -> Result` の 2 つ。
/// タグ (Err か) で分岐し、Ok / Err のペイロードを、枠の決まった欄へ書く・そこから読む。
/// `put` は `v` の Ok / Err のどちらかだけを書き、`get` はタグを見てどちらかを作る (もう一方の欄は読まない)。
pub fn res_helpers(t: Ty, aggs: &[AggDef], boxes: &[(Ty, Ty)]) -> Result<(Func, Func), String> {
    let Ty::Res(pt, pe) = t else { return Err(format!("型 {} は Result でない", kir::fmt_ty(t))) };
    let (tt, et) = (pt.ty(), pe.ty());
    let (eo, _) = res_layout(aggs, tt, et)?;
    let (put_name, get_name) = res_fn_names(t);
    let mk = |key: String, ret: Ty, blocks: Vec<Block>| Func {
        fqn: key.clone(),
        key,
        uses: vec![],
        may_panic: false,
        stack: None,
        ret,
        link: None,
        blocks,
    };
    // 書く: %0 = 枠の番地、%1 = 値。
    let mut g = Gen { next: 2, tys: HashMap::from([(0, Ty::Ptr), (1, t)]), aggs, boxes };
    let mut b0 = vec![];
    let tag = g.inst(&mut b0, Ty::Bool, Op::Tag(1));
    let mut b1 = vec![];
    let x = g.inst(&mut b1, tt, Op::Payload(1));
    put(&mut g, &mut b1, 0, RES_OK, x, tt).map_err(|e| e.to_string())?;
    let z = g.konst(&mut b1, Ty::Bool, 0);
    g.inst(&mut b1, Ty::Unit, Op::MemStore { ty: Ty::Bool, p: 0, off: 0, v: z });
    let u1 = g.konst(&mut b1, Ty::Unit, 0);
    let mut b2 = vec![];
    let y = g.inst(&mut b2, et, Op::Payload(1));
    put(&mut g, &mut b2, 0, eo, y, et).map_err(|e| e.to_string())?;
    let o = g.konst(&mut b2, Ty::Bool, 1);
    g.inst(&mut b2, Ty::Unit, Op::MemStore { ty: Ty::Bool, p: 0, off: 0, v: o });
    let u2 = g.konst(&mut b2, Ty::Unit, 0);
    let put_fn = mk(
        put_name.clone(),
        Ty::Unit,
        vec![
            Block { params: vec![(0, Ty::Ptr), (1, t)], insts: b0, term: Term::BrIf(tag, (2, vec![]), (1, vec![])) },
            Block { params: vec![], insts: b1, term: Term::Ret(u1) },
            Block { params: vec![], insts: b2, term: Term::Ret(u2) },
        ],
    );
    // 読む: %0 = 枠の番地。
    let mut g = Gen { next: 1, tys: HashMap::from([(0, Ty::Ptr)]), aggs, boxes };
    let mut b0 = vec![];
    let tg = g.inst(&mut b0, Ty::Bool, Op::MemLoad { p: 0, off: 0 });
    let mut b1 = vec![];
    let a = get(&mut g, &mut b1, 0, RES_OK, tt).map_err(|e| e.to_string())?;
    let ok = g.inst(&mut b1, t, Op::Ok(a));
    let mut b2 = vec![];
    let b = get(&mut g, &mut b2, 0, eo, et).map_err(|e| e.to_string())?;
    let er = g.inst(&mut b2, t, Op::Err(b));
    let get_fn = mk(
        get_name,
        t,
        vec![
            Block { params: vec![(0, Ty::Ptr)], insts: b0, term: Term::BrIf(tg, (2, vec![]), (1, vec![])) },
            Block { params: vec![], insts: b1, term: Term::Ret(ok) },
            Block { params: vec![], insts: b2, term: Term::Ret(er) },
        ],
    );
    Ok((put_fn, get_fn))
}

/// `Option` / `Result` の型 `t` を包む集成体の型。
fn box_of(g: &Gen, t: Ty) -> Result<Ty, String> {
    g.boxes.iter().find(|(a, _)| *a == t).map(|(_, b)| *b).ok_or_else(|| format!("型 {} の包みが無い", kir::fmt_ty(t)))
}

/// 枠 `base` の `off` の番地を集成体の参照として作る (集成体の値の読み書き用)。
fn addr_ref(g: &mut Gen, out: &mut Vec<Inst>, base: VId, off: u32, t: Ty) -> Result<VId, String> {
    let pt = Pointee::of(t).ok_or_else(|| format!("型 {} の参照は作れない", kir::fmt_ty(t)))?;
    let c = g.konst(out, Ty::Usize, off as i128);
    let pa = g.inst(out, Ty::Ptr, Op::PtrAdd { p: base, off: c });
    Ok(g.inst(out, Ty::Ref(true, pt), Op::Bitcast { src: Ty::Ptr, a: pa }))
}

/// 枠 `base` の `off` に値 `v` (型 `t`) を書く。単位型は何もしない。
fn put(g: &mut Gen, out: &mut Vec<Inst>, base: VId, off: u32, v: VId, t: Ty) -> Result<(), String> {
    match t {
        Ty::Unit => {}
        Ty::Ref(..) => {
            let p = g.inst(out, Ty::Ptr, Op::Bitcast { src: t, a: v });
            g.inst(out, Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: base, off, v: p });
        }
        _ if kir::memable(t) => {
            g.inst(out, Ty::Unit, Op::MemStore { ty: t, p: base, off, v });
        }
        Ty::Agg(_) => {
            let r = addr_ref(g, out, base, off, t)?;
            g.inst(out, Ty::Unit, Op::Store { ptr: r, val: v });
        }
        Ty::Opt(_) => {
            let bt = box_of(g, t)?;
            let w = g.inst(out, bt, Op::Make { variant: None, fields: vec![v] });
            put(g, out, base, off, w, bt)?;
        }
        Ty::Res(..) => {
            res_helpers(t, g.aggs, g.boxes)?;
            let p = ptr_at(g, out, base, off);
            let (name, _) = res_fn_names(t);
            g.inst(out, Ty::Unit, Op::Call { callee: name, args: vec![p, v] });
        }
        _ => return Err(format!("中断をまたぐ値の型 {} は枠に置けない", kir::fmt_ty(t))),
    }
    Ok(())
}

/// 枠 `base` + `off` の番地 (ポインタ)。
fn ptr_at(g: &mut Gen, out: &mut Vec<Inst>, base: VId, off: u32) -> VId {
    let c = g.konst(out, Ty::Usize, off as i128);
    g.inst(out, Ty::Ptr, Op::PtrAdd { p: base, off: c })
}

/// 枠 `base` の `off` から型 `t` の値を読む。単位型は単位の定数。
fn get(g: &mut Gen, out: &mut Vec<Inst>, base: VId, off: u32, t: Ty) -> Result<VId, String> {
    Ok(match t {
        Ty::Unit => g.konst(out, Ty::Unit, 0),
        Ty::Ref(..) => {
            let p = g.inst(out, Ty::Ptr, Op::MemLoad { p: base, off });
            g.inst(out, t, Op::Bitcast { src: Ty::Ptr, a: p })
        }
        _ if kir::memable(t) => g.inst(out, t, Op::MemLoad { p: base, off }),
        Ty::Agg(_) => {
            let r = addr_ref(g, out, base, off, t)?;
            g.inst(out, t, Op::Load(r))
        }
        Ty::Opt(_) => {
            let bt = box_of(g, t)?;
            let w = get(g, out, base, off, bt)?;
            g.inst(out, t, Op::Field { a: w, off: 0 })
        }
        Ty::Res(..) => {
            res_helpers(t, g.aggs, g.boxes)?;
            let p = ptr_at(g, out, base, off);
            let (_, name) = res_fn_names(t);
            g.inst(out, t, Op::Call { callee: name, args: vec![p] })
        }
        _ => return Err(format!("中断をまたぐ値の型 {} は枠に置けない", kir::fmt_ty(t))),
    })
}

/// 番地 `base` + `off` から `size` バイトを 0 にする (関数内の確保の初期化)。
fn zero(g: &mut Gen, out: &mut Vec<Inst>, base: VId, off: u32, size: u32) {
    let mut k = 0;
    while k + 8 <= size {
        let z = g.konst(out, Ty::U64, 0);
        g.inst(out, Ty::Unit, Op::MemStore { ty: Ty::U64, p: base, off: off + k, v: z });
        k += 8;
    }
    while k < size {
        let z = g.konst(out, Ty::U8, 0);
        g.inst(out, Ty::Unit, Op::MemStore { ty: Ty::U8, p: base, off: off + k, v: z });
        k += 1;
    }
}

/// 変換の途中のブロック。`forced` は、辺の外から渡される値 (ブロック引数) を固定した形 (再開・保存のブロック)。
#[derive(Clone, Debug)]
struct Dr {
    params: Vec<(VId, Ty)>,
    insts: Vec<Inst>,
    term: Term,
    forced: Option<Vec<VId>>,
    /// 元の終端 (移動先は元のブロックの番号。組み立ての最後に直す)。
    pending: Option<Term>,
}

fn dr(params: Vec<(VId, Ty)>, insts: Vec<Inst>, term: Term, forced: Option<Vec<VId>>) -> Dr {
    Dr { params, insts, term, forced, pending: None }
}

/// 中断点 1 つぶんの、ブロックの番号と値。
struct Site {
    j: usize,
    w: usize,
    s: usize,
    chw: VId,
}

/// 変換の結果: ブロックと、各ブロックが辺の外から受け取る値 (元の値番号)、元の値番号 → そのブロック内の新しい値番号。
struct Cv {
    blocks: Vec<Block>,
    ext: Vec<Vec<VId>>,
    rn: Vec<HashMap<VId, VId>>,
}

fn targets_of(t: &Term) -> Vec<usize> {
    match t {
        Term::Br((k, _)) => vec![*k],
        Term::BrIf(_, (a, _), (b, _)) => vec![*a, *b],
        Term::Ret(_) | Term::Unreachable => vec![],
    }
}

fn term_uses(t: &Term) -> Vec<VId> {
    match t {
        Term::Br((_, args)) => args.clone(),
        Term::BrIf(c, (_, a1), (_, a2)) => std::iter::once(*c).chain(a1.iter().copied()).chain(a2.iter().copied()).collect(),
        Term::Ret(v) => vec![*v],
        Term::Unreachable => vec![],
    }
}

/// 生存解析で、各ブロックが辺の外から受け取る値 (ブロック引数) を決め、ブロック引数の形に直す。
/// 固定 (`forced`) のブロックは、その集合を受け取る。固定でないブロックの値は、定義するか、後続が要る分を受け取る。
fn convert(drs: &[Dr], g: &mut Gen) -> Result<Cv, String> {
    let n = drs.len();
    let mut live: Vec<BTreeSet<VId>> = drs.iter().map(|d| d.forced.clone().unwrap_or_default().into_iter().collect()).collect();
    loop {
        let mut changed = false;
        for k in (0..n).rev() {
            let d = &drs[k];
            if d.forced.is_some() {
                continue;
            }
            let mut l: BTreeSet<VId> = BTreeSet::new();
            for t in targets_of(&d.term) {
                l.extend(live[t].iter().copied());
            }
            l.extend(term_uses(&d.term));
            for i in d.insts.iter().rev() {
                l.remove(&i.dst);
                l.extend(kir::operands(&i.op));
            }
            for (p, _) in &d.params {
                l.remove(p);
            }
            if l != live[k] {
                live[k] = l;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    if !live[0].is_empty() {
        return Err(format!("入口で定義されていない値を使っている ({:?})", live[0]));
    }
    let ext: Vec<Vec<VId>> = live.iter().map(|s| s.iter().copied().collect()).collect();
    let mut rn: Vec<HashMap<VId, VId>> = vec![HashMap::new(); n];
    for k in 0..n {
        for &v in &ext[k] {
            let t = g.ty(v)?;
            rn[k].insert(v, g.fresh(t));
        }
    }
    let mut blocks = Vec::with_capacity(n);
    for k in 0..n {
        let d = &drs[k];
        let ren_k = |v: VId| rn[k].get(&v).copied().unwrap_or(v);
        let mut params = d.params.clone();
        for &v in &ext[k] {
            params.push((rn[k][&v], g.ty(v)?));
        }
        let mut insts = Vec::with_capacity(d.insts.len());
        for i in &d.insts {
            let mut op = i.op.clone();
            for o in crate::kir_opt::operands_mut(&mut op) {
                *o = ren_k(*o);
            }
            insts.push(Inst { dst: i.dst, ty: i.ty, op });
        }
        let edge = |s: usize, args: &[VId]| -> Vec<VId> {
            let mut a: Vec<VId> = args.iter().map(|v| ren_k(*v)).collect();
            a.extend(ext[s].iter().map(|v| ren_k(*v)));
            a
        };
        let term = match &d.term {
            Term::Br((s, args)) => Term::Br((*s, edge(*s, args))),
            Term::BrIf(c, (a, aa), (b, bb)) => Term::BrIf(ren_k(*c), (*a, edge(*a, aa)), (*b, edge(*b, bb))),
            Term::Ret(v) => Term::Ret(ren_k(*v)),
            Term::Unreachable => Term::Unreachable,
        };
        blocks.push(Block { params, insts, term });
    }
    Ok(Cv { blocks, ext, rn })
}

/// 元の終端の移動先 (元のブロック番号) を、変換後の先頭のブロック番号に直す。
fn remap(t: &Term, first: &[usize]) -> Term {
    match t {
        Term::Br((k, a)) => Term::Br((first[*k], a.clone())),
        Term::BrIf(c, (x, a), (y, b)) => Term::BrIf(*c, (first[*x], a.clone()), (first[*y], b.clone())),
        other => other.clone(),
    }
}

/// 関数 1 つぶんの変換の結果: 元の鍵の関数 (中断するときは包み)、`.new`、`.poll`、再開 `.resume` (待っている枠を進める)、
/// 待ちの有無 `.pending`。`resume` と `pending` は、入口が JS の完了を待つとき (`ksn_resume`) に使う。
pub struct Parts {
    pub wrapper: Func,
    pub new: Func,
    pub poll: Func,
    pub resume: Func,
    pub pending: Func,
}

/// 関数 `f` の枠の形 (`new` / `poll` / 包み) を作る。`susp` は中断しうる関数の鍵 (葉を含む)。
pub fn lower_func(f: &Func, aggs: &[AggDef], boxes: &[(Ty, Ty)], susp: &HashSet<String>) -> Result<Parts, String> {
    let err = |m: String| format!("{}: {}", f.key, m);
    if f.blocks.is_empty() {
        return Err(err("ブロックがない".into()));
    }
    let mut next: VId = 0;
    let mut tys = HashMap::new();
    for b in &f.blocks {
        for (v, t) in &b.params {
            tys.insert(*v, *t);
            next = next.max(v + 1);
        }
        for i in &b.insts {
            tys.insert(i.dst, i.ty);
            next = next.max(i.dst + 1);
        }
    }
    let mut g = Gen { next, tys, aggs, boxes };
    let fp = g.fresh(Ty::Ptr);
    let ret = f.ret;
    // 枠の先頭: 状態・結果。その後に、関数内の確保 (A)、値の保存の場所 (B) を並べる。
    let ret_sz = g.slot_size(ret).map_err(err)?;
    let mut off = RES + align8(ret_sz);

    // (A) 関数内の確保を枠へ移し、戻りを「結果を書いて true を返す」にする。
    let mut orig: Vec<(Vec<(VId, Ty)>, Vec<Inst>, Term)> = vec![];
    for b in &f.blocks {
        let mut ins: Vec<Inst> = vec![];
        for i in &b.insts {
            match &i.op {
                Op::Alloca => {
                    let Ty::Ref(_, pt) = i.ty else { return Err(err("alloca の型が参照でない".into())) };
                    let sz = kir::size_of(aggs, pt.ty());
                    let o = off;
                    off += align8(sz);
                    let c = g.konst(&mut ins, Ty::Usize, o as i128);
                    let pa = g.inst(&mut ins, Ty::Ptr, Op::PtrAdd { p: fp, off: c });
                    zero(&mut g, &mut ins, pa, 0, sz);
                    ins.push(Inst { dst: i.dst, ty: i.ty, op: Op::Bitcast { src: Ty::Ptr, a: pa } });
                }
                Op::FrameBytes { size } => {
                    let o = off;
                    off += align8(*size);
                    let c = g.konst(&mut ins, Ty::Usize, o as i128);
                    ins.push(Inst { dst: i.dst, ty: Ty::Ptr, op: Op::PtrAdd { p: fp, off: c } });
                    zero(&mut g, &mut ins, i.dst, 0, *size);
                }
                _ => ins.push(i.clone()),
            }
        }
        let term = match &b.term {
            Term::Ret(v) => {
                put(&mut g, &mut ins, fp, RES, *v, ret).map_err(err)?;
                let t = g.konst(&mut ins, Ty::Bool, 1);
                Term::Ret(t)
            }
            t => t.clone(),
        };
        orig.push((b.params.clone(), ins, term));
    }

    // 中断点の数 (E の段の数)。
    let is_site = |i: &Inst| matches!(&i.op, Op::Call { callee, .. } if susp.contains(callee));
    let m: usize = orig.iter().map(|(_, ins, _)| ins.iter().filter(|i| is_site(i)).count()).sum();

    // 固定の並び: E_0..E_m (状態の判定)、UNREACH (m+1)、R_0..R_m (再開。R_0 は開始)。元のブロックの先頭は m+2+m+1 以降。
    let mut drs: Vec<Dr> = vec![];
    for k in 0..=m {
        let mut ins = vec![];
        let s = g.inst(&mut ins, Ty::U32, Op::MemLoad { p: fp, off: STATE });
        let c = g.konst(&mut ins, Ty::U32, k as i128);
        let e = g.inst(&mut ins, Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U32, a: s, b: c });
        let next_e = if k == m { m + 1 } else { k + 1 };
        let params = if k == 0 { vec![(fp, Ty::Ptr)] } else { vec![] };
        drs.push(dr(params, ins, Term::BrIf(e, (m + 2 + k, vec![]), (next_e, vec![])), None));
    }
    drs.push(dr(vec![], vec![], Term::Unreachable, None));
    for _ in 0..=m {
        drs.push(dr(vec![], vec![], Term::Unreachable, Some(vec![fp])));
    }

    // 元のブロックを、中断点ごとに切る。
    let mut first = vec![0usize; orig.len()];
    let mut sites: Vec<Site> = vec![];
    let mut j = 0usize;
    for (k, (params, ins, term)) in orig.iter().enumerate() {
        let mut cur = drs.len();
        first[k] = cur;
        drs.push(dr(params.clone(), vec![], Term::Unreachable, None));
        for i in ins {
            if let (Op::Call { callee, args }, true) = (&i.op, is_site(i)) {
                j += 1;
                let ch = g.fresh(Ty::Ptr);
                drs[cur].insts.push(Inst { dst: ch, ty: Ty::Ptr, op: Op::Call { callee: format!("{}.new", callee), args: args.clone() } });
                let chw = g.fresh(Ty::Ptr);
                let w = drs.len();
                let d = w + 1;
                let s = w + 2;
                let c = w + 3;
                let r = g.fresh(Ty::Bool);
                drs[cur].term = Term::Br((w, vec![ch]));
                // W: 子を進める。
                drs.push(dr(
                    vec![(chw, Ty::Ptr)],
                    vec![Inst { dst: r, ty: Ty::Bool, op: Op::Call { callee: format!("{}.poll", callee), args: vec![chw] } }],
                    Term::BrIf(r, (d, vec![]), (s, vec![])),
                    None,
                ));
                // D: 子の結果を読んで、枠を解放し、続きへ。
                let mut di = vec![];
                let res = get(&mut g, &mut di, chw, RES, i.ty).map_err(err)?;
                di.push(Inst { dst: g.fresh(Ty::Unit), ty: Ty::Unit, op: Op::HeapFree(chw) });
                drs.push(dr(vec![], di, Term::Br((c, vec![res])), None));
                // S: 状態と値を保存して、中断する (固定の集合は後で決める)。
                drs.push(dr(vec![], vec![], Term::Unreachable, Some(vec![fp])));
                sites.push(Site { j, w, s, chw });
                // 続き (C): 結果を受け取る引数 (元の呼び出しの値) を持つ。
                cur = drs.len();
                drs.push(dr(vec![(i.dst, i.ty)], vec![], Term::Unreachable, None));
                let _ = d;
                continue;
            }
            drs[cur].insts.push(i.clone());
        }
        drs[cur].pending = Some(term.clone());
    }
    for d in drs.iter_mut() {
        if let Some(t) = d.pending.take() {
            d.term = remap(&t, &first);
        }
    }

    // 固定の集合: 保存のブロック S は、W が受け取る値と、子の値 chw と、枠 fp。W の受け取りは S の要求から決まるので繰り返す。
    let mut cv = convert(&drs, &mut g)?;
    for _ in 0..6 {
        let mut changed = false;
        for s in &sites {
            let mut want: BTreeSet<VId> = cv.ext[s.w].iter().copied().collect();
            want.insert(s.chw);
            want.insert(fp);
            let want: Vec<VId> = want.into_iter().collect();
            if drs[s.s].forced.as_deref() != Some(want.as_slice()) {
                drs[s.s].forced = Some(want);
                changed = true;
            }
        }
        if !changed {
            break;
        }
        cv = convert(&drs, &mut g)?;
    }

    // 枠の場所: 保存する値 (S の固定の集合から fp を除いたもの) と、引数 (元の関数の params)。
    let mut keys: BTreeSet<VId> = BTreeSet::new();
    for s in &sites {
        for v in drs[s.s].forced.clone().unwrap_or_default() {
            if v != fp {
                keys.insert(v);
            }
        }
    }
    for (p, _) in &orig[0].0 {
        keys.insert(*p);
    }
    let mut slot: HashMap<VId, u32> = HashMap::new();
    for v in keys {
        let t = g.ty(v).map_err(err)?;
        let sz = g.slot_size(t).map_err(err)?;
        if sz == 0 {
            continue;
        }
        slot.insert(v, off);
        off += align8(sz);
    }
    let frame = align8(off);
    let slot_of = |v: VId| slot.get(&v).copied().unwrap_or(0);

    // (B) 保存: 状態を書き、中断点をまたぐ値を書いて false を返す。
    for s in &sites {
        let base = cv.rn[s.s][&fp];
        let mut out = vec![];
        let c = g.konst(&mut out, Ty::U32, s.j as i128);
        g.inst(&mut out, Ty::Unit, Op::MemStore { ty: Ty::U32, p: base, off: STATE, v: c });
        for v in drs[s.s].forced.clone().unwrap_or_default() {
            if v == fp {
                continue;
            }
            let val = cv.rn[s.s][&v];
            let t = g.ty(v).map_err(err)?;
            put(&mut g, &mut out, base, slot_of(v), val, t).map_err(err)?;
        }
        let f0 = g.konst(&mut out, Ty::Bool, 0);
        cv.blocks[s.s].insts = out;
        cv.blocks[s.s].term = Term::Ret(f0);
    }
    // (C) 再開: 枠から読み直して、W へ入る (再開の番号 j は E の段が選ぶ)。
    for s in &sites {
        let r = m + 2 + s.j;
        let base = cv.rn[r][&fp];
        let mut out = vec![];
        let mut args = vec![];
        let ch = get(&mut g, &mut out, base, slot_of(s.chw), Ty::Ptr).map_err(err)?;
        args.push(ch);
        for &v in &cv.ext[s.w] {
            if v == fp {
                args.push(base);
            } else {
                let t = g.ty(v).map_err(err)?;
                args.push(get(&mut g, &mut out, base, slot_of(v), t).map_err(err)?);
            }
        }
        cv.blocks[r].insts = out;
        cv.blocks[r].term = Term::Br((s.w, args));
    }
    // (D) 開始 (R_0): 引数を枠から読んで、元のブロック 0 へ入る。
    {
        let r0 = m + 2;
        let base = cv.rn[r0][&fp];
        let mut out = vec![];
        let mut args = vec![];
        for (p, t) in &orig[0].0 {
            args.push(get(&mut g, &mut out, base, slot_of(*p), *t).map_err(err)?);
        }
        // 元のブロック 0 が辺の外から受け取るのは枠 (fp) だけ (入口には先行する定義が無い)。
        for &v in &cv.ext[first[0]] {
            if v != fp {
                return Err(err(format!("入口が枠以外の値 %{} を要求している", v)));
            }
            args.push(base);
        }
        cv.blocks[r0].insts = out;
        cv.blocks[r0].term = Term::Br((first[0], args));
    }

    // 新しい枠を作る関数 (`F.new`): 枠を確保し、状態 0 と引数を入れる。
    let new_fn = {
        let mut ins = vec![];
        let sz = g.konst(&mut ins, Ty::Usize, frame as i128);
        let fpn = g.inst(&mut ins, Ty::Ptr, Op::HeapAlloc(sz));
        let st = g.konst(&mut ins, Ty::U32, 0);
        ins.push(Inst { dst: g.fresh(Ty::Unit), ty: Ty::Unit, op: Op::MemStore { ty: Ty::U32, p: fpn, off: STATE, v: st } });
        // 結果の置き場を 0 にしておく (`poll` が偽のまま wrapper が結果を読んでも、未初期化の値にならない)。
        let z64 = g.konst(&mut ins, Ty::U64, 0);
        for k in 0..(align8(ret_sz) / 8) {
            ins.push(Inst { dst: g.fresh(Ty::Unit), ty: Ty::Unit, op: Op::MemStore { ty: Ty::U64, p: fpn, off: RES + 8 * k, v: z64 } });
        }
        for (p, t) in f.params() {
            put(&mut g, &mut ins, fpn, slot_of(*p), *p, *t).map_err(err)?;
        }
        Func {
            fqn: format!("{}.new", f.fqn),
            key: format!("{}.new", f.key),
            uses: vec!["alloc".into()],
            may_panic: false,
            stack: None,
            ret: Ty::Ptr,
            link: None,
            blocks: vec![Block { params: f.params().to_vec(), insts: ins, term: Term::Ret(fpn) }],
        }
    };
    let mut uses = f.uses.clone();
    if !uses.iter().any(|u| u == "alloc") {
        uses.push("alloc".into());
        uses.sort();
    }
    let poll_fn = Func {
        fqn: format!("{}.poll", f.fqn),
        key: format!("{}.poll", f.key),
        uses: uses.clone(),
        may_panic: f.may_panic,
        stack: None,
        ret: Ty::Bool,
        link: None,
        blocks: cv.blocks,
    };
    // 元の鍵の関数: 枠を作って、一度だけ進め、結果を取り出して解放する (中断しない実行の形)。
    // 待ちが残った (JS の完了を待つ) ときは、枠を `WRAP_PEND` に置いて戻る (結果は読めない。`pending` で確かめてから使う)。
    // 再開 `resume` は、置いた枠を進めて、同じ形で結果を返す。
    let fin = |g: &mut Gen, ret: Ty| -> Result<Vec<Block>, String> {
        let mut b1 = vec![];
        let fpd = g.fresh(Ty::Ptr);
        let res = get(g, &mut b1, fpd, RES, ret)?;
        b1.push(Inst { dst: g.fresh(Ty::Unit), ty: Ty::Unit, op: Op::HeapFree(fpd) });
        let gb = g.inst(&mut b1, Ty::Ptr, Op::GlobalBytes { name: WRAP_PEND.into(), size: 16 });
        let z = g.konst(&mut b1, Ty::U32, 0);
        b1.push(Inst { dst: g.fresh(Ty::Unit), ty: Ty::Unit, op: Op::MemStore { ty: Ty::U32, p: gb, off: 8, v: z } });
        let mut b2 = vec![];
        let fpp = g.fresh(Ty::Ptr);
        let res2 = get(g, &mut b2, fpp, RES, ret)?;
        let gb2 = g.inst(&mut b2, Ty::Ptr, Op::GlobalBytes { name: WRAP_PEND.into(), size: 16 });
        b2.push(Inst { dst: g.fresh(Ty::Unit), ty: Ty::Unit, op: Op::MemStore { ty: Ty::Ptr, p: gb2, off: 0, v: fpp } });
        let one = g.konst(&mut b2, Ty::U32, 1);
        b2.push(Inst { dst: g.fresh(Ty::Unit), ty: Ty::Unit, op: Op::MemStore { ty: Ty::U32, p: gb2, off: 8, v: one } });
        Ok(vec![Block { params: vec![(fpd, Ty::Ptr)], insts: b1, term: Term::Ret(res) }, Block { params: vec![(fpp, Ty::Ptr)], insts: b2, term: Term::Ret(res2) }])
    };
    let blocks_after = fin(&mut g, ret).map_err(err)?;
    let wrapper = {
        let params = f.params().to_vec();
        let mut b0 = vec![];
        let fpw = g.fresh(Ty::Ptr);
        let okw = g.fresh(Ty::Bool);
        b0.push(Inst { dst: fpw, ty: Ty::Ptr, op: Op::Call { callee: format!("{}.new", f.key), args: params.iter().map(|p| p.0).collect() } });
        b0.push(Inst { dst: okw, ty: Ty::Bool, op: Op::Call { callee: format!("{}.poll", f.key), args: vec![fpw] } });
        let mut blocks = vec![Block { params, insts: b0, term: Term::BrIf(okw, (1, vec![fpw]), (2, vec![fpw])) }];
        blocks.extend(blocks_after.clone());
        Func {
            fqn: f.fqn.clone(),
            key: f.key.clone(),
            uses: uses.clone(),
            may_panic: f.may_panic,
            stack: f.stack,
            ret,
            link: None,
            blocks,
        }
    };
    // 再開: 大域に置いた枠を進める (`ksn_resume` が使う)。
    let resume_fn = {
        let mut ins = vec![];
        let gb = g.inst(&mut ins, Ty::Ptr, Op::GlobalBytes { name: WRAP_PEND.into(), size: 16 });
        let fpr = g.inst(&mut ins, Ty::Ptr, Op::MemLoad { p: gb, off: 0 });
        let okr = g.inst(&mut ins, Ty::Bool, Op::Call { callee: format!("{}.poll", f.key), args: vec![fpr] });
        let mut blocks = vec![Block { params: vec![], insts: ins, term: Term::BrIf(okr, (1, vec![fpr]), (2, vec![fpr])) }];
        blocks.extend(blocks_after.clone());
        Func {
            fqn: format!("{}.resume", f.fqn),
            key: format!("{}.resume", f.key),
            uses: uses.clone(),
            may_panic: f.may_panic,
            stack: None,
            ret,
            link: None,
            blocks,
        }
    };
    // 待ちの有無: `WRAP_PEND` の印 (待ちの枠を置いたとき 1)。
    let pending_fn = {
        let mut ins = vec![];
        let gb = g.inst(&mut ins, Ty::Ptr, Op::GlobalBytes { name: WRAP_PEND.into(), size: 16 });
        let fl = g.inst(&mut ins, Ty::U32, Op::MemLoad { p: gb, off: 8 });
        let z = g.konst(&mut ins, Ty::U32, 0);
        let r = g.inst(&mut ins, Ty::Bool, Op::Cmp { cc: Cc::Ne, ty: Ty::U32, a: fl, b: z });
        Func {
            fqn: format!("{}.pending", f.fqn),
            key: format!("{}.pending", f.key),
            uses: vec![],
            may_panic: false,
            stack: None,
            ret: Ty::Bool,
            link: None,
            blocks: vec![Block { params: vec![], insts: ins, term: Term::Ret(r) }],
        }
    };
    Ok(Parts { wrapper, new: new_fn, poll: poll_fn, resume: resume_fn, pending: pending_fn })
}

/// `subjects` (鍵と、タスクの番号) を枠の形にする。`tasks` の順 (番号) は `DISPATCH` の表 (`exec_funcs`) と合わせる。
/// 戻り値は、変換を通した関数の列と、変換できなかった関数 (鍵と理由)。
pub fn transform(m: &mut Module, tasks: &[String]) -> Vec<(String, String)> {
    let susp: HashSet<String> = m.funcs.iter().filter(|f| f.uses.iter().any(|u| u == "suspend")).map(|f| f.key.clone()).collect();
    let task_keys: HashSet<&str> = tasks.iter().map(|s| s.as_str()).collect();
    let targets = |f: &Func| -> bool {
        let is_susp = f.uses.iter().any(|u| u == "suspend");
        let is_task = task_keys.contains(f.key.as_str());
        !f.key.starts_with("ksn.task.") && (is_susp || is_task)
    };
    // 枠へ書く `Option` / `Result` の包み (1 つの値を持つ集成体) を、変換の前に表へ足す。添字を関数の間で揃えるため。
    let mut boxes: Vec<(Ty, Ty)> = vec![];
    let mut need: Vec<Ty> = vec![];
    let mut res: Vec<Ty> = vec![];
    let tys_of = |f: &Func| -> Vec<Ty> {
        std::iter::once(f.ret).chain(f.blocks.iter().flat_map(|b| b.params.iter().map(|p| p.1).chain(b.insts.iter().map(|i| i.ty)))).collect()
    };
    for f in m.funcs.iter().filter(|f| targets(f)) {
        for t in tys_of(f) {
            collect_wrapped(t, &mut need, &mut res);
        }
    }
    // 非中断の関数でも、`block_on` / `join_task` の結果 (`ksn.task.fp` / `ksn.task.wait` を呼ぶ関数) は枠から `Result` を読む。
    // その型の補助関数だけ要る。それ以外の関数の `Result` は補助関数を使わないので、作らない (非中断の出力を変えないため)。
    let mut dummy = vec![];
    let reads_frame = |f: &Func| f.blocks.iter().any(|b| b.insts.iter().any(|i| matches!(&i.op, Op::Call { callee, .. } if callee == FP || callee == WAIT)));
    for f in m.funcs.iter().filter(|f| !targets(f) && reads_frame(f)) {
        for t in tys_of(f) {
            collect_wrapped(t, &mut dummy, &mut res);
        }
    }
    // `Result` は集成体のフィールドに置けない (LLVM の制約) ので、包まない。枠へは補助関数 (`res_helpers`) で書く。
    for t in need {
        let size = kir::size_of(&m.aggs, t);
        let align = kir::align_of(&m.aggs, t);
        let name = format!("ksn.box<{}>", kir::fmt_ty(t));
        m.aggs.push(AggDef { name, size, align, kind: AggKind::Struct(vec![Field { name: "v".into(), ty: t, off: 0 }]) });
        boxes.push((t, Ty::Agg((m.aggs.len() - 1) as u32)));
    }
    let aggs = &m.aggs;
    let mut out = Vec::with_capacity(m.funcs.len());
    let mut fails = vec![];
    for f in std::mem::take(&mut m.funcs) {
        let is_susp = f.uses.iter().any(|u| u == "suspend");
        if !targets(&f) {
            out.push(f);
            continue;
        }
        match lower_func(&f, aggs, &boxes, &susp) {
            Ok(p) => {
                if is_susp {
                    out.push(p.wrapper);
                    out.push(p.resume);
                    out.push(p.pending);
                } else {
                    out.push(f);
                }
                out.push(p.new);
                out.push(p.poll);
            }
            Err(why) => {
                fails.push((f.key.clone(), why));
                out.push(f);
            }
        }
    }
    // `Result` の枠の補助関数。作れない型は作らない (枠へ書く・読む側が `res_helpers` を確かめて、理由つきで失敗する)。
    for t in res {
        if let Ok((p, g)) = res_helpers(t, aggs, &boxes) {
            out.push(p);
            out.push(g);
        }
    }
    m.funcs = out;
    fails
}

/// 型 `t` の中の `Option` (包みが要る) と `Result` (補助関数が要る) を集める。`Result` の中の型も見る。
fn collect_wrapped(t: Ty, opts: &mut Vec<Ty>, ress: &mut Vec<Ty>) {
    match t {
        Ty::Opt(p) => {
            let inner = p.ty();
            if !opts.contains(&t) {
                opts.push(t);
            }
            collect_wrapped(inner, opts, ress);
        }
        Ty::Res(a, b) => {
            if !ress.contains(&t) {
                ress.push(t);
            }
            collect_wrapped(a.ty(), opts, ress);
            collect_wrapped(b.ty(), opts, ress);
        }
        _ => {}
    }
}

/// タスクの番号を `ksn.task.dispatch` が引くための表 (番号 → 本体の `.poll`) の KIR テキスト。
fn dispatch_text(tasks: &[(u32, String)]) -> String {
    let mut s = String::new();
    s.push_str("fn ksn.task.dispatch key=ksn.task.dispatch uses=[alloc,suspend] panics=may stack=- ret=bool\n");
    let n = tasks.len();
    if n == 0 {
        s.push_str("  b0(%0: u32, %1: ptr):\n    %2: bool = const.bool false\n    ret %2\nend ksn.task.dispatch\n");
        return s;
    }
    // 段 i: b_i(%tg, %fp) で番号を比べる。一致は b_{n+i}(%fp) で呼ぶ。最後の段の外れは b_{2n} (到達しない)。
    for (i, (tag, key)) in tasks.iter().enumerate() {
        let b = 4 * i;
        s.push_str(&format!("  b{}(%{}: u32, %{}: ptr):\n", i, b, b + 1));
        s.push_str(&format!("    %{}: u32 = const.u32 {}\n", b + 2, tag));
        s.push_str(&format!("    %{}: bool = icmp.eq.u32 %{}, %{}\n", b + 3, b, b + 2));
        let next = if i + 1 == n { 2 * n } else { i + 1 };
        let next_args = if next == 2 * n { format!("%{}, %{}", b, b + 1) } else { format!("%{}, %{}", b, b + 1) };
        s.push_str(&format!("    br_if %{}, b{}(%{}), b{}({})\n", b + 3, n + i, b + 1, next, next_args));
        let _ = key;
    }
    for (i, (_, key)) in tasks.iter().enumerate() {
        let p = 4 * n + 2 * i;
        s.push_str(&format!("  b{}(%{}: ptr):\n", n + i, p));
        s.push_str(&format!("    %{}: bool = call {}.poll(%{})\n", p + 1, key, p));
        s.push_str(&format!("    ret %{}\n", p + 1));
    }
    let f = 4 * n + 2 * n;
    s.push_str(&format!("  b{}(%{}: u32, %{}: ptr):\n    unreachable\n", 2 * n, f, f + 1));
    s.push_str("end ksn.task.dispatch\n");
    s
}

/// 実行器の KIR (テキスト)。`ksn.task.g` は大域の領域: `[0]` 表の番地、`[8]` 個数、`[16]` 容量、`[24]` 走っている番号、`[32]` 根が終わったか、`[40]` `run.poll` の再開の位置、`[48]` セッション番号 (`block_on` ごとに `ksn.task.session` が返すプロセス全体で一意の番号を入れ、`finish` でも消さない。別スレッド・以前の `block_on` の古い `Task` を拒む)、`[56]` 用意リスト (FIFO の輪) の番地 (容量は `[16]`)、`[64]` 先頭・`[72]` 末尾の通し番号、`[80]` / `[88]` join 待ち (状態 1) に入った数 / 出た数 (違えば待ちがいる)、`[96]` `pick` の回数。`[104]` 空き枠の連結の先頭 (枠の番号 + 1。0 は空き無し。join で結果を読み終えた枠を再利用する)。全体の大きさは 112。各タスクの枠 (56 バイト。`+48` は join 待ちの者の番号 + 1) の `+4` は輪に載っている印。
/// 表の項目 (32 バイト): `[0]` 状態 (0 走れる / 1 待ち / 2 終わり)、`[8]` 待つ相手、`[16]` 本体の番号 (u32)、`[24]` 枠。
const EXEC_TEXT: &str = include_str!("kir_suspend_exec.kir");
/// 複数スレッドの実行器のワーカーの起動・合流 (`ksn.thr.*` を呼ぶので、`block_on_threads` を使うときだけ足す)。`@WTAG@` はワーカーの入口の tag。
const EXEC_MT_TEXT: &str = include_str!("kir_suspend_exec_mt.kir");

/// 実行器と葉の関数 (`ksn.task.*`) と、本体の番号の表 (`dispatch`) を KIR の関数の列にする。
/// `tasks` は (番号, 本体の鍵) の列。鍵は変換で `.poll` を作るもの (`transform` が作る) だけを渡す。
pub fn exec_funcs(tasks: &[(u32, String)]) -> Result<Vec<Func>, String> {
    exec_funcs_mt(tasks, None)
}

/// `exec_funcs` に、複数スレッドの実行器のワーカーの起動・合流 (`mt_worker_tag` = ワーカーの入口の tag) を足したもの。
pub fn exec_funcs_mt(tasks: &[(u32, String)], mt_worker_tag: Option<u64>) -> Result<Vec<Func>, String> {
    let mt = mt_worker_tag.map(|t| EXEC_MT_TEXT.replace("@WTAG@", &t.to_string())).unwrap_or_default();
    let text = format!("kir 1\n\n{}\n{}\n{}", EXEC_TEXT, mt, dispatch_text(tasks));
    let m = kir::parse_module(&text).map_err(|e| format!("実行器の KIR が読めない: {}", e))?;
    Ok(m.funcs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 中断する関数 `t.s` (`yield_now` を 1 回挟む)、タスクの本体 `t.body` (`t.s` を呼ぶ)、入口 `t.main` (実行器を回し、根の結果を読む)。
    /// 実行器は `exec_funcs` の KIR。変換の後で検証し、参照実行器で 5 + 1 + 10 = 16 になることを確かめる。
    const SRC: &str = "kir 1

fn t.s key=t.s uses=[suspend] panics=never stack=- ret=u32
  b0(%0: u32):
    %1: unit = call ksn.task.yield()
    %2: u32 = const.u32 1
    %3: u32 = add.u32.wrap %0, %2
    ret %3
end t.s

fn t.body key=t.body uses=[suspend] panics=never stack=- ret=u32
  b0(%0: u32):
    %1: u32 = call t.s(%0)
    %2: u32 = const.u32 10
    %3: u32 = add.u32.wrap %1, %2
    ret %3
end t.body

fn t.main key=t.main uses=[suspend] panics=may stack=- ret=u32
  b0():
    %0: u32 = const.u32 5
    %1: ptr = call t.body.new(%0)
    %2: u32 = const.u32 0
    %3: unit = call ksn.task.run(%2, %1)
    %4: usize = const.usize 0
    %5: ptr = call ksn.task.fp(%4)
    %6: u32 = mem.load.u32 %5 +8
    %7: unit = call ksn.task.finish()
    ret %6
end t.main
";

    fn module_of(src: &str, tasks: &[(u32, &str)]) -> Module {
        let mut m = kir::parse_module(src).expect("KIR が読める");
        let ts: Vec<(u32, String)> = tasks.iter().map(|(n, k)| (*n, k.to_string())).collect();
        m.funcs.extend(exec_funcs(&ts).expect("実行器が読める"));
        let names: Vec<String> = tasks.iter().map(|(_, k)| k.to_string()).collect();
        let fails = transform(&mut m, &names);
        assert!(fails.is_empty(), "{:?}", fails);
        m
    }

    #[test]
    fn exec_text_parses_and_verifies() {
        let m = Module { funcs: exec_funcs(&[]).expect("実行器が読める"), aggs: vec![], depth: 0 };
        let bad = kir::verify_module(&m);
        assert!(bad.is_empty(), "{:?}", bad);
    }

    #[test]
    fn suspending_call_runs_under_the_executor() {
        let m = module_of(SRC, &[(0, "t.body")]);
        let bad = kir::verify_module(&m);
        assert!(bad.is_empty(), "{:?}", bad);
        match crate::kir_exec::run(&m, "t.main", &[]) {
            Ok(crate::kir_exec::Outcome::Value(v)) => assert_eq!(v, 16),
            other => panic!("結果が違う: {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn suspending_wrapper_runs_once_outside_the_executor() {
        // 実行器の外の `t.s` は `yield_now` で止まらず、そのまま値を返す (木走査の既定と同じ)。
        let src = "kir 1

fn t.s key=t.s uses=[suspend] panics=never stack=- ret=u32
  b0(%0: u32):
    %1: unit = call ksn.task.yield()
    %2: u32 = const.u32 1
    %3: u32 = add.u32.wrap %0, %2
    ret %3
end t.s

fn t.main key=t.main uses=[suspend] panics=never stack=- ret=u32
  b0():
    %0: u32 = const.u32 41
    %1: u32 = call t.s(%0)
    ret %1
end t.main
";
        let m = module_of(src, &[]);
        let bad = kir::verify_module(&m);
        assert!(bad.is_empty(), "{:?}", bad);
        match crate::kir_exec::run(&m, "t.main", &[]) {
            Ok(crate::kir_exec::Outcome::Value(v)) => assert_eq!(v, 42),
            other => panic!("結果が違う: {:?}", other.map(|_| ())),
        }
    }
}
