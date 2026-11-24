//! `std.sync.channel` / `Sender<T>` / `Receiver<T>` の KIR 下ろし (設計_KIR.md 22 章)。
//!
//! 木走査 (`natives.rs` の `std.sync.channel`、`native_method` の `Chan`) と同じ意味:
//! - 容量 (0 は 1) を超える `send` は、`recv` が空きを作るか受信側が全部落ちるまで待つ (保留のスレッドを走らせ、走らせるものが無ければ deadlock の panic)。受信側が 0 本なら `send` は `Err(SendError { value })`。
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

use super::thread_lower::{rt_call, HAS_PENDING, RUN_ONE};
use super::{Lw, LR};

const STATE: i128 = 176;
/// チャネルごとのロックと待ち (ネイティブ。`kir_llvm.rs` の `ksn.chan.rt.*`)。状態の `+48` に `pthread_mutex_t` (40 バイト)、`+88` に
/// `pthread_cond_t` (48 バイト)、`+136` に待っている数、`+144` に起こしの札の数。実行器 (協調実行) では何もしない (`wait` は保留のスレッドを 1 本走らせる)。
const CINIT: &str = "ksn.chan.rt.init";
const CLOCK: &str = "ksn.chan.rt.lock";
/// 起こさずに外す / 待っている 1 本だけ起こして外す / 待っている全員を起こして外す。
const CUNLOCK: &str = "ksn.chan.rt.unlock";
const CUNLOCK1: &str = "ksn.chan.rt.unlock1";
const CUNLOCKALL: &str = "ksn.chan.rt.unlockall";
const CWAIT: &str = "ksn.chan.rt.wait";
/// 満杯の `send` の待ち (待ちの本体は `wait` と同じで、全員待ちの panic の文言だけ違う) と、空きを作った `recv` のあとの解放
/// (待っている送り手がいれば全員を起こす。いなければ `unlock` と同じ)。
const CWAITS: &str = "ksn.chan.rt.waitsend";
const CUNLOCKR: &str = "ksn.chan.rt.unlockrecv";
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
/// 容量 (`channel` の引数。0 は 1)。待っている送り手の数 (ロックの下)。
const LIMIT: u32 = 152;
const SWAIT: u32 = 160;
/// 協調実行だけ: いま `recv` で止まっている枠の数 (満杯の `send` が容量を超えて続ける根拠)。
const RWAITC: u32 = 168;

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

/// 要素の枠の幅 (8 の倍数に切り上げ)。スカラーは 8 のまま。
fn stride_of(lw: &Lw, elem: Ty) -> i128 {
    let sz = match elem {
        Ty::Agg(i) => lw.aggs.borrow().defs[i as usize].size as i128,
        _ => 8,
    };
    (sz.max(1) + 7) / 8 * 8
}

/// 枠の番地から要素を読む / 枠へ書く (集成体は `ptr` を参照にして `load` / `store`)。
fn slot_load(lw: &mut Lw, slot: VId, elem: Ty) -> VId {
    match elem {
        Ty::Agg(i) => {
            let r = lw.emit(Ty::Ref(true, Pointee::Agg(i)), Op::Bitcast { src: Ty::Ptr, a: slot });
            lw.emit(elem, Op::Load(r))
        }
        _ => lw.emit(elem, Op::MemLoad { p: slot, off: 0 }),
    }
}

fn slot_store(lw: &mut Lw, slot: VId, elem: Ty, v: VId) {
    match elem {
        Ty::Agg(i) => {
            let r = lw.emit(Ty::Ref(true, Pointee::Agg(i)), Op::Bitcast { src: Ty::Ptr, a: slot });
            lw.emit(Ty::Unit, Op::Store { ptr: r, val: v });
        }
        _ => {
            lw.emit(Ty::Unit, Op::MemStore { ty: elem, p: slot, off: 0, v });
        }
    }
}

fn cmp_eq0(lw: &mut Lw, v: VId) -> VId {
    let z = us(lw, 0);
    lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: v, b: z })
}

/// 端 `v` (`Sender` なら `sender`) を落とす: 本数を 1 減らし、両方 0 なら `buf` と状態を解放する。
pub(crate) fn drop_end(lw: &mut Lw, v: VId, sender: bool, elem: Ty) {
    let st = lw.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
    let off = if sender { SENDERS } else { RECEIVERS };
    rt_call(lw, CLOCK, vec![st], Ty::Unit);
    let n = ld(lw, st, off);
    let one = us(lw, 1);
    let n1 = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: n, b: one });
    put(lw, st, off, n1);
    let s = ld(lw, st, SENDERS);
    let r = ld(lw, st, RECEIVERS);
    let sum = add(lw, s, r);
    let z = cmp_eq0(lw, sum);
    // 本数を変えたので待っている受け手を起こす。0 になったなら誰も状態に触れないので、ロックを外してから解放する。
    rt_call(lw, CUNLOCKALL, vec![st], Ty::Unit);
    let (free, _) = lw.new_block(&[]);
    let (join, _) = lw.new_block(&[]);
    lw.branch(z, (free, vec![]), (join, vec![]));
    lw.cur = Some(free);
    // 残った要素 (`buf[head..len]`) が所有物を持つなら、1 つずつ解放してから。
    if super::coll::owns_heap_pub(lw, elem) {
        let buf = lw.emit(Ty::Ptr, Op::MemLoad { p: st, off: BUF });
        let head = ld(lw, st, HEAD);
        let len = ld(lw, st, LEN);
        let stride = us(lw, stride_of(lw, elem));
        let (hd, hp) = lw.new_block(&[Ty::Usize]);
        let (body, _) = lw.new_block(&[]);
        let (exit, _) = lw.new_block(&[]);
        lw.goto(hd, vec![head]);
        lw.cur = Some(hd);
        let i = hp[0];
        let c = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: i, b: len });
        lw.branch(c, (body, vec![]), (exit, vec![]));
        lw.cur = Some(body);
        let o = lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: i, b: stride });
        let slot = lw.emit(Ty::Ptr, Op::PtrAdd { p: buf, off: o });
        let x = slot_load(lw, slot, elem);
        super::coll::drop_value(lw, x, elem);
        let one = us(lw, 1);
        let i1 = add(lw, i, one);
        lw.goto(hd, vec![i1]);
        lw.cur = Some(exit);
    }
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
        "close" => !sender && args.is_empty() && !via_ref,
        _ => false,
    };
    if !supported {
        return Some(lw.err(line, format!("チャネルの端のメソッド {} は段階 1 の対象外 (send / recv / clone / Receiver.close だけ)", meth)));
    }
    if meth == "close" {
        // 受け手を消費する (局所の印を下ろす)。ほかに受信側があるか、キューが空なら端を畳んで `Ok(())`、残りがあれば端をそのまま `Err` で返す。
        let end = super::coll::path_read(lw, s);
        lw.own_emit(&segs[0], s, crate::kir::OwnKind::Consume);
        return Some(close_end(lw, e, end, end_t, line));
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
            rt_call(lw, CLOCK, vec![st], Ty::Unit);
            let n = ld(lw, st, off);
            let one = us(lw, 1);
            let n1 = add(lw, n, one);
            put(lw, st, off, n1);
            rt_call(lw, CUNLOCK, vec![st], Ty::Unit);
            Ok(Some(lw.emit(end_t, Op::Make { variant: None, fields: vec![st] })))
        }
    })
}

fn close_end(lw: &mut Lw, e: &V, end: VId, end_t: Ty, line: usize) -> LR<Option<VId>> {
    let rty = lw.ty_of(e)?;
    let Ty::Res(_, Pay::Agg(back)) = rty else { return lw.err(line, "Receiver.close の結果の型が Result<(), Receiver<T>> でない") };
    if Ty::Agg(back) != end_t {
        return lw.err(line, "Receiver.close の結果の型が Result<(), Receiver<T>> でない");
    }
    let elem = end_info(lw, end_t).map(|x| x.2).unwrap_or(Ty::U8);
    let st = lw.emit(Ty::Ptr, Op::Field { a: end, off: 0 });
    let (fin, fp) = lw.new_block(&[rty]);
    rt_call(lw, CLOCK, vec![st], Ty::Unit);
    let rc = ld(lw, st, RECEIVERS);
    let one = us(lw, 1);
    let many = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::Usize, a: rc, b: one });
    let head = ld(lw, st, HEAD);
    let len = ld(lw, st, LEN);
    let empty = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::Usize, a: head, b: len });
    let (try_empty, _) = lw.new_block(&[]);
    let (shut, _) = lw.new_block(&[]);
    let (keep, _) = lw.new_block(&[]);
    lw.branch(many, (shut, vec![]), (try_empty, vec![]));
    lw.cur = Some(try_empty);
    lw.branch(empty, (shut, vec![]), (keep, vec![]));
    lw.cur = Some(shut);
    rt_call(lw, CUNLOCKALL, vec![st], Ty::Unit);
    drop_end(lw, end, false, elem);
    let u = lw.unit();
    let ok = lw.emit(rty, Op::Ok(u));
    lw.goto(fin, vec![ok]);
    lw.cur = Some(keep);
    rt_call(lw, CUNLOCK, vec![st], Ty::Unit);
    let er = lw.emit(rty, Op::Err(end));
    lw.goto(fin, vec![er]);
    lw.cur = Some(fin);
    Ok(Some(fp[0]))
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
    // アロケータは KIR では使わない。容量 0 は 1 として扱う。
    let Some(cap) = lw.expr(&args[1])? else { return Ok(None) };
    let cap0 = cmp_eq0(lw, cap);
    let (lim_blk, lp) = lw.new_block(&[Ty::Usize]);
    let (zero_blk, _) = lw.new_block(&[]);
    let (keep_blk, _) = lw.new_block(&[]);
    lw.branch(cap0, (zero_blk, vec![]), (keep_blk, vec![]));
    lw.cur = Some(zero_blk);
    let one0 = us(lw, 1);
    lw.goto(lim_blk, vec![one0]);
    lw.cur = Some(keep_blk);
    lw.goto(lim_blk, vec![cap]);
    lw.cur = Some(lim_blk);
    let limit = lp[0];
    let n = us(lw, STATE);
    let st = lw.emit(Ty::Ptr, Op::HeapAlloc(n));
    let null = lw.emit(Ty::Ptr, Op::Const(0));
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: st, off: BUF, v: null });
    let z = us(lw, 0);
    put(lw, st, LIMIT, limit);
    for off in [HEAD, LEN, CAPF, SWAIT, RWAITC] {
        put(lw, st, off, z);
    }
    let one = us(lw, 1);
    put(lw, st, SENDERS, one);
    put(lw, st, RECEIVERS, one);
    rt_call(lw, CINIT, vec![st], Ty::Unit);
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
    rt_call(lw, CLOCK, vec![st], Ty::Unit);
    let (sl, _) = lw.new_block(&[]);
    lw.goto(sl, vec![]);
    lw.cur = Some(sl);
    let rc = ld(lw, st, RECEIVERS);
    let closed = cmp_eq0(lw, rc);
    let (cl, _) = lw.new_block(&[]);
    let (chk, _) = lw.new_block(&[]);
    lw.branch(closed, (cl, vec![]), (chk, vec![]));
    lw.cur = Some(cl);
    rt_call(lw, CUNLOCK, vec![st], Ty::Unit);
    let se = lw.emit(Ty::Agg(serr), Op::Make { variant: None, fields: vec![v] });
    let er = lw.emit(rty, Op::Err(se));
    lw.goto(fin, vec![er]);
    lw.cur = Some(chk);
    // 満杯 (待っている要素が容量以上) なら、空きができるか受信側が全部落ちるまで待つ。
    let head0 = ld(lw, st, HEAD);
    let len0 = ld(lw, st, LEN);
    let queued = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: len0, b: head0 });
    let limit = ld(lw, st, LIMIT);
    let is_full = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::Usize, a: queued, b: limit });
    let (wt, _) = lw.new_block(&[]);
    let (open, _) = lw.new_block(&[]);
    lw.branch(is_full, (wt, vec![]), (open, vec![]));
    lw.cur = Some(wt);
    let sw = ld(lw, st, SWAIT);
    let one1 = us(lw, 1);
    let sw1 = add(lw, sw, one1);
    put(lw, st, SWAIT, sw1);
    rt_call(lw, CWAITS, vec![st], Ty::Unit);
    let sw2 = ld(lw, st, SWAIT);
    let sw3 = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: sw2, b: one1 });
    put(lw, st, SWAIT, sw3);
    lw.goto(sl, vec![]);
    lw.cur = Some(open);
    // 満杯なら伸ばす。
    let len = ld(lw, st, LEN);
    let cap = ld(lw, st, CAPF);
    let full = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: len, b: cap });
    let (grow, _) = lw.new_block(&[]);
    let (push, _) = lw.new_block(&[]);
    lw.branch(full, (grow, vec![]), (push, vec![]));
    lw.cur = Some(grow);
    // 先頭 (`head`) がバッファの半分以上なら、伸ばさずに残りを先頭へ詰める (有界のキューのメモリを抑える。要素の型のまま読み書きする)。
    let hd0 = ld(lw, st, HEAD);
    let cp0 = ld(lw, st, CAPF);
    let two = us(lw, 2);
    let h2 = lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: hd0, b: two });
    let big = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::Usize, a: h2, b: cp0 });
    let z0 = us(lw, 0);
    let pos = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::Usize, a: hd0, b: z0 });
    let want = lw.emit(Ty::Bool, Op::Bool(crate::kir::BoolOp::And, big, pos));
    let (pack, _) = lw.new_block(&[]);
    let (real, _) = lw.new_block(&[]);
    lw.branch(want, (pack, vec![]), (real, vec![]));
    lw.cur = Some(pack);
    let pbuf = lw.emit(Ty::Ptr, Op::MemLoad { p: st, off: BUF });
    let plen = ld(lw, st, LEN);
    let pn = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: plen, b: hd0 });
    let pst = us(lw, stride_of(lw, elem));
    let (ph, pp) = lw.new_block(&[Ty::Usize]);
    let (pb, _) = lw.new_block(&[]);
    let (pe, _) = lw.new_block(&[]);
    lw.goto(ph, vec![z0]);
    lw.cur = Some(ph);
    let pi = pp[0];
    let pc = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: pi, b: pn });
    lw.branch(pc, (pb, vec![]), (pe, vec![]));
    lw.cur = Some(pb);
    let si = add(lw, pi, hd0);
    let so = lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: si, b: pst });
    let sslot = lw.emit(Ty::Ptr, Op::PtrAdd { p: pbuf, off: so });
    let xv = slot_load(lw, sslot, elem);
    let dof = lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: pi, b: pst });
    let dslot = lw.emit(Ty::Ptr, Op::PtrAdd { p: pbuf, off: dof });
    slot_store(lw, dslot, elem, xv);
    let one2 = us(lw, 1);
    let pi1 = add(lw, pi, one2);
    lw.goto(ph, vec![pi1]);
    lw.cur = Some(pe);
    put(lw, st, HEAD, z0);
    put(lw, st, LEN, pn);
    lw.goto(push, vec![]);
    lw.cur = Some(real);
    let stride = us(lw, stride_of(lw, elem));
    lw.callees.insert(GROW.to_string());
    lw.emit(Ty::Unit, Op::Call { callee: GROW.to_string(), args: vec![st, stride] });
    lw.goto(push, vec![]);
    lw.cur = Some(push);
    let len = ld(lw, st, LEN);
    let buf = lw.emit(Ty::Ptr, Op::MemLoad { p: st, off: BUF });
    let eight = us(lw, stride_of(lw, elem));
    let o = lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: len, b: eight });
    let slot = lw.emit(Ty::Ptr, Op::PtrAdd { p: buf, off: o });
    slot_store(lw, slot, elem, v);
    let one = us(lw, 1);
    let l1 = add(lw, len, one);
    put(lw, st, LEN, l1);
    // 送り手が待っているなら、札の取り違えを避けて全員を起こす。
    let swn = ld(lw, st, SWAIT);
    let no_sw = cmp_eq0(lw, swn);
    let (w1, _) = lw.new_block(&[]);
    let (wall, _) = lw.new_block(&[]);
    let (wdone, _) = lw.new_block(&[]);
    lw.branch(no_sw, (w1, vec![]), (wall, vec![]));
    lw.cur = Some(w1);
    rt_call(lw, CUNLOCK1, vec![st], Ty::Unit);
    lw.goto(wdone, vec![]);
    lw.cur = Some(wall);
    rt_call(lw, CUNLOCKALL, vec![st], Ty::Unit);
    lw.goto(wdone, vec![]);
    lw.cur = Some(wdone);
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
    let (fin, fp) = lw.new_block(&[rty]);
    let (lp, _) = lw.new_block(&[]);
    rt_call(lw, CLOCK, vec![st], Ty::Unit);
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
    let eight = us(lw, stride_of(lw, elem));
    let o = lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: head, b: eight });
    let slot = lw.emit(Ty::Ptr, Op::PtrAdd { p: buf, off: o });
    let x = slot_load(lw, slot, elem);
    let one = us(lw, 1);
    let h1 = add(lw, head, one);
    put(lw, st, HEAD, h1);
    rt_call(lw, CUNLOCKR, vec![st], Ty::Unit);
    let ok = lw.emit(rty, Op::Ok(x));
    lw.goto(fin, vec![ok]);
    lw.cur = Some(empty);
    let senders = ld(lw, st, SENDERS);
    let none = cmp_eq0(lw, senders);
    let (closed, _) = lw.new_block(&[]);
    let (wait, _) = lw.new_block(&[]);
    lw.branch(none, (closed, vec![]), (wait, vec![]));
    lw.cur = Some(closed);
    rt_call(lw, CUNLOCK, vec![st], Ty::Unit);
    let zero = lw.emit(Ty::U8, Op::Const(0));
    let re = lw.emit(Ty::Agg(rerr), Op::Make { variant: None, fields: vec![zero] });
    let er = lw.emit(rty, Op::Err(re));
    lw.goto(fin, vec![er]);
    lw.cur = Some(wait);
    // 空で送り手が生きている: 協調実行では保留中のスレッドを 1 本走らせる (走らせるものが無ければ deadlock の panic)。
    // LLVM では条件変数で待つ (ロックを手放して、起こされたら取り直す)。
    rt_call(lw, CWAIT, vec![st], Ty::Unit);
    lw.goto(lp, vec![]);
    lw.cur = Some(fin);
    Ok(Some(fp[0]))
}

/// 補助の関数 (`ksn.chan.grow` / `ksn.chan.free`) を、呼ばれていれば足す。
pub(super) fn finish(ok: &mut Vec<(crate::kir::Func, std::collections::BTreeSet<String>)>) {
    use super::fb::Fb;
    use std::collections::BTreeSet;
    let called = |n: &str| ok.iter().any(|(_, cs)| cs.contains(n));
    let (want_grow, want_free) = (called(GROW), called(FREE));
    // ロックと待ちの補助 (実行器では協調実行なので何もしない。ネイティブは `kir_llvm.rs` が本体に差し替える)。
    let all: Vec<String> = ["alloc", "env", "io.console", "io.fs", "io.net", "process", "rand", "thread", "time"].iter().map(|s| s.to_string()).collect();
    let wanted_rt: Vec<&str> = [CINIT, CLOCK, CUNLOCK, CUNLOCK1, CUNLOCKALL, CWAIT, CWAITS, CUNLOCKR].into_iter().filter(|n| called(n)).collect();
    for name in wanted_rt {
        let mut b = Fb::new(&[Ty::Ptr]);
        let mut callees = BTreeSet::new();
        if name == CWAIT || name == CWAITS {
            // 保留中のスレッドを 1 本走らせる。走らせるものが無ければ: `send` は上の枠が同じチャネルの `recv` で止まっていれば
            // 容量を 1 つ上げて続け (+168 が 0 より大きい)、そうでなければ deadlock の panic。`recv` は deadlock の panic。
            callees.insert(if name == CWAITS { HAS_PENDING } else { RUN_ONE }.to_string());
            let one = b.c(Ty::Usize, 1);
            if name == CWAIT {
                let c0 = b.load(Ty::Usize, 0, RWAITC as i128);
                let c1 = b.add(c0, one);
                b.store(Ty::Usize, 0, RWAITC as i128, c1);
            }
            let r = b.emit(Ty::Bool, Op::Call { callee: (if name == CWAITS { HAS_PENDING } else { RUN_ONE }).to_string(), args: vec![] });
            if name == CWAIT {
                let c0 = b.load(Ty::Usize, 0, RWAITC as i128);
                let c1 = b.sub(c0, one);
                b.store(Ty::Usize, 0, RWAITC as i128, c1);
            }
            let (yes, _) = b.block(&[]);
            let (dead, _) = b.block(&[]);
            if name == CWAITS {
                // 保留のスレッドがあれば、満杯でも容量を 1 つ上げて続ける (そのスレッドが受け手でも、入れ子では main に戻れないため)。
                let (g1, _) = b.block(&[]);
                b.br_if(r, g1, dead);
                b.at(g1);
                let l = b.load(Ty::Usize, 0, LIMIT as i128);
                let l1 = b.add(l, one);
                b.store(Ty::Usize, 0, LIMIT as i128, l1);
                let u = b.emit(Ty::Unit, Op::Const(0));
                b.term(Term::Ret(u));
            } else {
                b.br_if(r, yes, dead);
            }
            b.at(dead);
            if name == CWAITS {
                let rc = b.load(Ty::Usize, 0, RWAITC as i128);
                let z = b.c(Ty::Usize, 0);
                let any = b.cmp(Cc::Gt, rc, z);
                let (grow, _) = b.block(&[]);
                let (pn, _) = b.block(&[]);
                b.br_if(any, grow, pn);
                b.at(grow);
                let l = b.load(Ty::Usize, 0, LIMIT as i128);
                let l1 = b.add(l, one);
                b.store(Ty::Usize, 0, LIMIT as i128, l1);
                let u = b.emit(Ty::Unit, Op::Const(0));
                b.term(Term::Ret(u));
                b.at(pn);
            }
            b.emit(Ty::Unit, Op::Panic { msg: (if name == CWAITS { crate::interp::DEADLOCK_SEND } else { crate::interp::DEADLOCK_RECV }).to_string(), arg: None });
            b.term(Term::Unreachable);
            b.at(yes);
        }
        let u = b.emit(Ty::Unit, Op::Const(0));
        b.term(Term::Ret(u));
        let mut f = b.build(name, &[], Ty::Unit);
        f.uses = all.clone();
        f.may_panic = true;
        ok.push((f, callees));
    }
    if want_grow {
        let mut b = Fb::new(&[Ty::Ptr, Ty::Usize]);
        let cap = b.load(Ty::Usize, 0, CAPF as i128);
        let two = b.c(Ty::Usize, 2);
        let c2 = b.mul(cap, two);
        let four = b.c(Ty::Usize, 4);
        let ncap = b.add(c2, four);
        let bytes = b.mul(ncap, 1);
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
