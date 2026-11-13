//! `std.sync.channel` / `Sender<T>` / `Receiver<T>` の KIR 下ろし (設計_KIR.md 21 章)。
//!
//! 木走査 (`natives.rs` の `std.sync.channel`、`native_method` の `Chan`) と同じ意味:
//! - 容量は無視する (`send` は待たない)。受信側が 0 本なら `send` は `Err(SendError { value })`。
//! - `recv`: キューが空でなければ先頭を取る。空で、送信側が 0 本なら `Err(RecvError { code: 0 })`。空で送信側が生きていれば、
//!   保留中のスレッドを生成順に 1 本走らせて (`ksn.thr.run_one`) やり直し、走らせるものが無ければ deadlock の panic (interp.rs の文言)。
//! - 端の `clone` は同じチャネルの端を 1 本増やし、端が落ちる (スコープの終わり・関数の引数・スレッド終了で取り込み値が落ちる) と減る。
//!
//! 表現: 端 `Sender<T>` / `Receiver<T>` は `{ state: ptr }` (8 バイト)。状態はヒープの 48 バイト
//! `{ buf: ptr @0, head: usize @8, len: usize @16, cap: usize @24, senders: usize @32, receivers: usize @40 }`。
//! キューは `buf[head..len]` (要素は 8 バイトの枠に 1 つ。`len == cap` で `cap*2+4` に伸ばす)。
//! 端は「所有物を持つ型」で、落とす (`drop_end`) と本数を 1 減らし、両方が 0 になったら `buf` と状態を解放する。
//! 要素の型 `T` は 64 ビット以下のスカラー。

use crate::check::{items, nf, nlineno as nline, nname, strs};
use crate::kir::{Aop, Cc, Mode, Op, Pay, Pointee, Term, Ty, VId};
use crate::parser::V;

use super::thread_lower::RUN_ONE;
use super::{Lw, LR};

const STATE: i128 = 48;
/// 補助の関数: `grow(state)` は `cap*2+4` 要素に伸ばす、`free(state)` は `buf` と状態を解放する。`alloc` の効果は端の側 (`channel` の
/// `uses [alloc]`) が持つので、呼び出し側の効果の包含は見ない (kir.rs の検査)。
const GROW: &str = "ksn.chan.grow";
const FREE: &str = "ksn.chan.free";
const BUF: u32 = 0;
const HEAD: u32 = 8;
const LEN: u32 = 16;
const CAPF: u32 = 24;
const SENDERS: u32 = 32;
const RECEIVERS: u32 = 40;

fn us(lw: &mut Lw, n: i128) -> VId {
    lw.emit(Ty::Usize, Op::Const(n))
}

fn add(lw: &mut Lw, a: VId, b: VId) -> VId {
    lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a, b })
}

fn ld(lw: &mut Lw, st: VId, off: u32) -> VId {
    lw.emit(Ty::Usize, Op::MemLoad { p: st, off })
}

fn put(lw: &mut Lw, st: VId, off: u32, v: VId) {
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: st, off, v });
}

fn cmp_eq0(lw: &mut Lw, v: VId) -> VId {
    let z = us(lw, 0);
    lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: v, b: z })
}

/// 端 `v` (`Sender` なら `sender`) を落とす: 本数を 1 減らし、両方 0 なら `buf` と状態を解放する。
pub(crate) fn drop_end(lw: &mut Lw, v: VId, sender: bool) {
    let st = lw.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
    let off = if sender { SENDERS } else { RECEIVERS };
    let n = ld(lw, st, off);
    let one = us(lw, 1);
    let n1 = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: n, b: one });
    put(lw, st, off, n1);
    let s = ld(lw, st, SENDERS);
    let r = ld(lw, st, RECEIVERS);
    let sum = add(lw, s, r);
    let z = cmp_eq0(lw, sum);
    let (free, _) = lw.new_block(&[]);
    let (join, _) = lw.new_block(&[]);
    lw.branch(z, (free, vec![]), (join, vec![]));
    lw.cur = Some(free);
    lw.callees.insert(FREE.to_string());
    lw.emit(Ty::Unit, Op::Call { callee: FREE.to_string(), args: vec![st] });
    lw.goto(join, vec![]);
    lw.cur = Some(join);
}

/// 端の型 (`Agg`) と、その要素の型・送り手か。
fn end_info(lw: &Lw, t: Ty) -> Option<(Ty, bool, Ty)> {
    let aggs = lw.aggs.borrow();
    let sender = aggs.chan_kind(t)?;
    let Ty::Agg(i) = t else { return None };
    Some((t, sender, *aggs.chan_elem.get(&i)?))
}

/// パスの呼び出し (`std.sync.channel::<T, A>(..)`、`tx.send(v)`、`rx.recv()`、`tx.clone()`)。対象外なら `None`。
pub(super) fn chan_call(lw: &mut Lw, e: &V) -> Option<LR<Option<VId>>> {
    let f = nf(e);
    if nname(&f[0]) != "Path" {
        return None;
    }
    let segs = strs(&nf(&f[0])[0]);
    let args = items(&f[1]).to_vec();
    let line = nline(e);
    if segs.join(".") == "std.sync.channel" {
        return Some(channel(lw, e, &args, line));
    }
    if segs.len() != 2 {
        return None;
    }
    let s = lw.lookup(&segs[0])?;
    // 受け手: 端の値の局所か、端への参照 (`&Sender<T>`)。
    let (end_t, via_ref) = match lw.slots[s].1 {
        Ty::Agg(_) => (lw.slots[s].1, false),
        Ty::Ref(_, Pointee::Agg(i)) => (Ty::Agg(i), true),
        _ => return None,
    };
    let (_, sender, elem) = end_info(lw, end_t)?;
    let meth = segs[1].as_str();
    let supported = match meth {
        "send" => sender && args.len() == 1,
        "recv" => !sender && args.is_empty(),
        "clone" => args.is_empty(),
        _ => false,
    };
    if !supported {
        return Some(lw.err(line, format!("チャネルの端のメソッド {} は段階 1 の対象外 (send / recv / clone だけ)", meth)));
    }
    let end = if via_ref {
        let r = lw.read_slot(s);
        lw.emit(end_t, Op::Load(r))
    } else {
        lw.read_slot(s)
    };
    let st = lw.emit(Ty::Ptr, Op::Field { a: end, off: 0 });
    Some(match meth {
        "send" => send(lw, e, st, elem, &args[0], line),
        "recv" => recv(lw, e, st, elem, line),
        _ => {
            let off = if sender { SENDERS } else { RECEIVERS };
            let n = ld(lw, st, off);
            let one = us(lw, 1);
            let n1 = add(lw, n, one);
            put(lw, st, off, n1);
            Ok(Some(lw.emit(end_t, Op::Make { variant: None, fields: vec![st] })))
        }
    })
}

fn channel(lw: &mut Lw, e: &V, args: &[V], line: usize) -> LR<Option<VId>> {
    if args.len() != 2 {
        return lw.err(line, "std.sync.channel の引数は 2 個のはず");
    }
    let rty = lw.ty_of(e)?;
    let Ty::Res(Pay::Agg(tup), _) = rty else { return lw.err(line, "std.sync.channel の結果の型が (Sender, Receiver) の組でない") };
    let fields: Vec<Ty> = match &lw.aggs.borrow().defs[tup as usize].kind {
        crate::kir::AggKind::Tuple(fs) => fs.iter().map(|f| f.ty).collect(),
        _ => vec![],
    };
    if fields.len() != 2 {
        return lw.err(line, "std.sync.channel の結果の型が (Sender, Receiver) の組でない");
    }
    // 容量は評価だけして捨てる (木走査と同じく無視する)。アロケータは KIR では使わない。
    let Some(_cap) = lw.expr(&args[1])? else { return Ok(None) };
    let n = us(lw, STATE);
    let st = lw.emit(Ty::Ptr, Op::HeapAlloc(n));
    let null = lw.emit(Ty::Ptr, Op::Const(0));
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: st, off: BUF, v: null });
    let z = us(lw, 0);
    for off in [HEAD, LEN, CAPF] {
        put(lw, st, off, z);
    }
    let one = us(lw, 1);
    put(lw, st, SENDERS, one);
    put(lw, st, RECEIVERS, one);
    let tx = lw.emit(fields[0], Op::Make { variant: None, fields: vec![st] });
    let rx = lw.emit(fields[1], Op::Make { variant: None, fields: vec![st] });
    let pair = lw.emit(Ty::Agg(tup), Op::Make { variant: None, fields: vec![tx, rx] });
    Ok(Some(lw.emit(rty, Op::Ok(pair))))
}

fn send(lw: &mut Lw, e: &V, st: VId, elem: Ty, arg: &V, line: usize) -> LR<Option<VId>> {
    let rty = lw.ty_of(e)?;
    let Ty::Res(_, Pay::Agg(serr)) = rty else { return lw.err(line, "send の結果の型が Result<(), SendError<T>> でない") };
    let Some(v) = lw.expr(arg)? else { return Ok(None) };
    if lw.vty(v) != elem {
        return lw.err(line, "send の引数の型が要素の型と違う");
    }
    let (fin, fp) = lw.new_block(&[rty]);
    let rc = ld(lw, st, RECEIVERS);
    let closed = cmp_eq0(lw, rc);
    let (cl, _) = lw.new_block(&[]);
    let (open, _) = lw.new_block(&[]);
    lw.branch(closed, (cl, vec![]), (open, vec![]));
    lw.cur = Some(cl);
    let se = lw.emit(Ty::Agg(serr), Op::Make { variant: None, fields: vec![v] });
    let er = lw.emit(rty, Op::Err(se));
    lw.goto(fin, vec![er]);
    lw.cur = Some(open);
    // 満杯なら伸ばす。
    let len = ld(lw, st, LEN);
    let cap = ld(lw, st, CAPF);
    let full = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: len, b: cap });
    let (grow, _) = lw.new_block(&[]);
    let (push, _) = lw.new_block(&[]);
    lw.branch(full, (grow, vec![]), (push, vec![]));
    lw.cur = Some(grow);
    let ncap = cap;
    let _ = ncap;
    lw.callees.insert(GROW.to_string());
    lw.emit(Ty::Unit, Op::Call { callee: GROW.to_string(), args: vec![st] });
    lw.goto(push, vec![]);
    lw.cur = Some(push);
    let len = ld(lw, st, LEN);
    let buf = lw.emit(Ty::Ptr, Op::MemLoad { p: st, off: BUF });
    let eight = us(lw, 8);
    let o = lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: len, b: eight });
    let slot = lw.emit(Ty::Ptr, Op::PtrAdd { p: buf, off: o });
    lw.emit(Ty::Unit, Op::MemStore { ty: elem, p: slot, off: 0, v });
    let one = us(lw, 1);
    let l1 = add(lw, len, one);
    put(lw, st, LEN, l1);
    let u = lw.unit();
    let ok = lw.emit(rty, Op::Ok(u));
    lw.goto(fin, vec![ok]);
    lw.cur = Some(fin);
    Ok(Some(fp[0]))
}

fn recv(lw: &mut Lw, e: &V, st: VId, elem: Ty, line: usize) -> LR<Option<VId>> {
    let rty = lw.ty_of(e)?;
    let Ty::Res(okp, Pay::Agg(rerr)) = rty else { return lw.err(line, "recv の結果の型が Result<T, RecvError> でない") };
    if okp.ty() != elem {
        return lw.err(line, "recv の結果の型が要素の型と違う");
    }
    lw.callees.insert(RUN_ONE.to_string());
    let (fin, fp) = lw.new_block(&[rty]);
    let (lp, _) = lw.new_block(&[]);
    lw.goto(lp, vec![]);
    lw.cur = Some(lp);
    let head = ld(lw, st, HEAD);
    let len = ld(lw, st, LEN);
    let has = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: head, b: len });
    let (take, _) = lw.new_block(&[]);
    let (empty, _) = lw.new_block(&[]);
    lw.branch(has, (take, vec![]), (empty, vec![]));
    lw.cur = Some(take);
    let buf = lw.emit(Ty::Ptr, Op::MemLoad { p: st, off: BUF });
    let eight = us(lw, 8);
    let o = lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: head, b: eight });
    let slot = lw.emit(Ty::Ptr, Op::PtrAdd { p: buf, off: o });
    let x = lw.emit(elem, Op::MemLoad { p: slot, off: 0 });
    let one = us(lw, 1);
    let h1 = add(lw, head, one);
    put(lw, st, HEAD, h1);
    let ok = lw.emit(rty, Op::Ok(x));
    lw.goto(fin, vec![ok]);
    lw.cur = Some(empty);
    let senders = ld(lw, st, SENDERS);
    let none = cmp_eq0(lw, senders);
    let (closed, _) = lw.new_block(&[]);
    let (wait, _) = lw.new_block(&[]);
    lw.branch(none, (closed, vec![]), (wait, vec![]));
    lw.cur = Some(closed);
    let zero = lw.emit(Ty::U8, Op::Const(0));
    let re = lw.emit(Ty::Agg(rerr), Op::Make { variant: None, fields: vec![zero] });
    let er = lw.emit(rty, Op::Err(re));
    lw.goto(fin, vec![er]);
    lw.cur = Some(wait);
    let ran = lw.emit(Ty::Bool, Op::Call { callee: RUN_ONE.to_string(), args: vec![] });
    let (dead, _) = lw.new_block(&[]);
    lw.branch(ran, (lp, vec![]), (dead, vec![]));
    lw.cur = Some(dead);
    lw.emit(Ty::Unit, Op::Panic { msg: crate::interp::DEADLOCK_RECV.to_string(), arg: None });
    lw.terminate(Term::Unreachable);
    lw.cur = Some(fin);
    Ok(Some(fp[0]))
}

/// 補助の関数 (`ksn.chan.grow` / `ksn.chan.free`) を、呼ばれていれば足す。
pub(super) fn finish(ok: &mut Vec<(crate::kir::Func, std::collections::BTreeSet<String>)>) {
    use super::map_lower::Fb;
    use std::collections::BTreeSet;
    let called = |n: &str| ok.iter().any(|(_, cs)| cs.contains(n));
    let (want_grow, want_free) = (called(GROW), called(FREE));
    if want_grow {
        let mut b = Fb::new(&[Ty::Ptr]);
        let cap = b.load(Ty::Usize, 0, CAPF as i128);
        let two = b.c(Ty::Usize, 2);
        let c2 = b.mul(cap, two);
        let four = b.c(Ty::Usize, 4);
        let ncap = b.add(c2, four);
        let eight = b.c(Ty::Usize, 8);
        let bytes = b.mul(ncap, eight);
        let buf = b.load(Ty::Ptr, 0, BUF as i128);
        let nb = b.emit(Ty::Ptr, Op::HeapRealloc { p: buf, size: bytes });
        b.store(Ty::Ptr, 0, BUF as i128, nb);
        b.store(Ty::Usize, 0, CAPF as i128, ncap);
        let u = b.emit(Ty::Unit, Op::Const(0));
        b.term(Term::Ret(u));
        let mut f = b.build(GROW, &["alloc"], Ty::Unit);
        f.may_panic = false;
        ok.push((f, BTreeSet::new()));
    }
    if want_free {
        let mut b = Fb::new(&[Ty::Ptr]);
        let buf = b.load(Ty::Ptr, 0, BUF as i128);
        b.emit(Ty::Unit, Op::HeapFree(buf));
        b.emit(Ty::Unit, Op::HeapFree(0));
        let u = b.emit(Ty::Unit, Op::Const(0));
        b.term(Term::Ret(u));
        let f = b.build(FREE, &["alloc"], Ty::Unit);
        ok.push((f, BTreeSet::new()));
    }
}
