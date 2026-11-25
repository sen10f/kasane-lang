//! `std.thread.spawn` / `JoinHandle.join` の KIR 下ろし (kir_lower_thread)。
//! 木走査の意味 (実装計画.md 2.2、interp.rs の `threads`): `spawn` は保留するだけで走らせず、`join` が完了を待つとき、または
//! `recv` が空のキューで待つときに、保留中のスレッドを生成順に 1 本ずつ最後まで走らせる。join しないスレッドは走らない。
//!
//! KIR には間接呼び出しがないので、スレッドの表を大域領域 `ksn.thr` (`global.bytes`) に持ち、実行は補助の関数が引く (設計_KIR.md 22 章):
//! - 表 = `count (usize)` + 固定長 (`CAP` 本) の項目。項目 = `state (0 保留 / 1 実行中 / 2 完了)`、`tag (u64: クロージャの鍵の FNV-1a)`、
//!   取り込み値の領域 (`CAPB` バイト、8 バイト境界に 1 つずつ)、結果の領域 (`RESB` バイト)。
//! - `spawn`: 項目 `count` に tag と取り込み値を書き、`count += 1`。ハンドルは `{allocator, native = 項目の番号, null}`。
//!   番号は実行時の値なので、ループの中・呼び出しの先で spawn してハンドルを返す形も通る。表が満杯なら panic。
//! - クロージャごとに入口の関数 `ksn.thr.site.<鍵>(項目の番地)` (取り込み値を読んで呼び、結果を結果の領域へ書く) を `extra` に足す。
//!   `finish` (全関数を下ろした後) が、見つかった入口の一覧から `ksn.thr.exec` (tag で入口を選ぶ)、`ksn.thr.run_one`
//!   (保留の最初の 1 本を走らせて true、無ければ false)、`ksn.thr.join` (完了まで run_one を繰り返す。実行中の項目を待つか、
//!   走らせるものが無ければ木走査と同じ deadlock の panic) を作る。
//! - `join(h)`: `ksn.thr.join(h.native)` の後、その項目の結果の領域から結果を読む。

use std::collections::BTreeSet;

use crate::kir::{size_of as ty_size, Aop, Cc, Mode, Func, Op, Pay, Pointee, Term, Ty, VId};
use crate::parser::V;
use crate::types::Ty as Src;

use super::fb::Fb;
use super::{coll, Lw, LR};

/// 表の大域領域の名前。
pub(super) const TABLE: &str = "ksn.thr";
/// 表の項目の数 (満杯なら spawn が panic)。
const CAP: u32 = 1024;
/// 項目の取り込み値の領域・結果の領域の大きさ (バイト)。
const CAPB: u32 = 64;
const RESB: u32 = 64;
/// 項目の先頭 (`state`、`tag`、予備) の大きさ。取り込み値の領域は `HDR`、結果の領域は `HDR + CAPB` から。
const HDR: u32 = 24;
/// 項目の `spawn_deep` の深さの置き場 (`depth + 1`、0 は既定)。`kir_llvm.rs` の `THREAD_DEPTH_OFF` と同じ。
const DEPTH_OFF: u32 = 248;
/// 項目の大きさ。
const ESZ: u32 = HDR + CAPB + RESB;
/// 表の大きさ (`count` の 8 バイトを含む)。
const TOTAL: u32 = 8 + CAP * ESZ;
/// kir_llvm.rs の `THREAD_ENTRY_SIZE` (256) に収まる。
const _: () = assert!(ESZ <= 256);

pub(super) const SITE: &str = "ksn.thr.site.";
pub(super) const RUN_ONE: &str = "ksn.thr.run_one";
/// 保留 (未開始) のスレッドがあるか (走らせない)。満杯の `send` が使う。
pub(super) const HAS_PENDING: &str = "ksn.thr.has_pending";
const EXEC: &str = "ksn.thr.exec";
pub(super) const JOIN: &str = "ksn.thr.join";
/// 実行器 (参照実行器・VM) では協調実行の本体を持ち、LLVM (`kir_llvm.rs` の `thread_override`) では pthread の本体に差し替える補助の関数。
/// 名前は `kir_llvm.rs` と対応している。インライン展開しない (`kir_opt.rs`)。
pub(super) const TNEW: &str = "ksn.thr.new";
pub(super) const TENT: &str = "ksn.thr.ent";
pub(super) const TSTART: &str = "ksn.thr.start";
pub(super) const TREL: &str = "ksn.thr.release";
pub(super) const LOCK: &str = "ksn.thr.lock";
pub(super) const UNLOCK: &str = "ksn.thr.unlock";
pub(super) const WAIT: &str = "ksn.thr.wait";
/// `static` の `Mutex` の再帰 mutex (番地を取る): 初期化・取得・解放。
pub(super) const MINIT: &str = "ksn.thr.minit";
pub(super) const MLOCK: &str = "ksn.thr.mlock";
pub(super) const MUNLOCK: &str = "ksn.thr.munlock";
pub(super) const RWRD: &str = "ksn.thr.rwrd";
pub(super) const RWWR: &str = "ksn.thr.rwwr";
pub(super) const RWUNLOCK: &str = "ksn.thr.rwunlock";

/// 補助の関数 `name(args)` の呼び出し (戻り値の型 `ret`)。
pub(super) fn rt_call(lw: &mut Lw, name: &str, args: Vec<VId>, ret: Ty) -> VId {
    lw.callees.insert(name.to_string());
    lw.emit(ret, Op::Call { callee: name.to_string(), args })
}

/// 鍵の FNV-1a (入口を選ぶ tag)。
pub(super) fn tag_of(key: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn align8(n: u32) -> u32 {
    (n + 7) / 8 * 8
}

/// 項目 `idx` (usize の値。協調実行では表の番号、LLVM では項目の番地) の番地 + `extra` バイト。
fn entry_at(lw: &mut Lw, idx: VId, extra: u32) -> VId {
    let ent = rt_call(lw, TENT, vec![idx], Ty::Ptr);
    if extra == 0 {
        return ent;
    }
    let o = lw.emit(Ty::Usize, Op::Const(extra as i128));
    lw.emit(Ty::Ptr, Op::PtrAdd { p: ent, off: o })
}

/// 番地 `p` を `&mut t` として見る。
fn ref_at(lw: &mut Lw, p: VId, t: Ty) -> Option<VId> {
    let pt = Pointee::of(t)?;
    Some(lw.emit(Ty::Ref(true, pt), Op::Bitcast { src: Ty::Ptr, a: p }))
}

/// `std.thread.spawn(allocator, closure)` の呼び出し。対象外なら `None` (親へ戻す)。
pub(super) fn spawn_call(lw: &mut Lw, _e: &V, segs: &[String], args: &[V], line: usize) -> Option<LR<Option<VId>>> {
    let deep = segs == ["std", "thread", "spawn_deep"];
    if segs != ["std", "thread", "spawn"] && !deep {
        return None;
    }
    let mut args = args.iter();
    let Some(alloc_arg) = args.next() else { return None };
    let Some(closure_arg) = args.next() else { return None };
    let depth_arg = if deep { args.next() } else { None };
    if args.next().is_some() || (deep && depth_arg.is_none()) {
        return Some(lw.err(line, "std.thread.spawn / spawn_deep の引数の個数が違う"));
    }
    let ret = match lw.src_ty(closure_arg) {
        Ok(Src::Closure { ret, .. }) => match lw.conv_t(&ret) {
            Some(t) => t,
            None => return Some(lw.err(line, format!("spawn の戻り値の型 {} は段階 1 の対象外", super::show(&ret)))),
        },
        Ok(t) => return Some(lw.err(line, format!("spawn の第2引数はクロージャでなければならない (型 {})", super::show(&t)))),
        Err(err) => return Some(lw.err(line, err)),
    };
    let alloc_src = match lw.src_ty(alloc_arg) {
        Ok(t) => t,
        Err(err) => return Some(lw.err(line, err)),
    };
    let alloc_ty = match lw.conv_t(&alloc_src) {
        Some(ty) => ty,
        None => return Some(lw.err(line, format!("spawn のアロケータの型 {} は段階 1 の対象外", super::show(&alloc_src)))),
    };
    if ret != Ty::Unit && Pointee::of(ret).is_none() {
        return Some(lw.err(line, "spawn の結果の型は領域に置けない"));
    }
    let res_size = if ret == Ty::Unit { 0 } else { ty_size(&lw.aggs.borrow().defs, ret) };
    if res_size > RESB {
        return Some(lw.err(line, format!("spawn の結果の型が大きすぎる ({} バイト > {})", res_size, RESB)));
    }

    lw.coll.spawn_arg = true;
    let kind = coll::closure_arg_kind(lw, closure_arg);
    lw.coll.spawn_arg = false;
    let kind = match kind {
        Ok(c) => c,
        Err(err) => return Some(Err(err)),
    };
    let Some(coll::Kind::Closure { key, caps: cap_vals, cap_tys, .. }) = kind else {
        return Some(lw.err(line, "spawn のクロージャが取り込みを持てない"));
    };
    if cap_vals.iter().any(|v| matches!(lw.vty(*v), Ty::Ref(..))) {
        return Some(lw.err(line, "spawn のクロージャは参照を取り込めない ('static でなければならない)"));
    }
    // 取り込み値の置き場所 (8 バイト境界に 1 つずつ)。
    let mut offs = vec![];
    let mut at = 0u32;
    for v in &cap_vals {
        let t = lw.vty(*v);
        if Pointee::of(t).is_none() {
            return Some(lw.err(line, "spawn の取り込み値の型は領域に置けない"));
        }
        offs.push(at);
        at += align8(ty_size(&lw.aggs.borrow().defs, t));
    }
    if at > CAPB {
        return Some(lw.err(line, format!("spawn の取り込み値が大きすぎる ({} バイト > {})", at, CAPB)));
    }
    lw.callees.insert(key.clone());

    // 入口の関数 (クロージャの鍵ごとに 1 つ)。
    let site = site_func(lw, &key, &cap_vals, &cap_tys, &offs, ret);
    lw.callees.insert(site.0.key.clone());
    lw.coll.extra.push(site);

    let hty = lw.aggs.borrow_mut().join_handle(ret, alloc_ty);
    let spawnerr_ty = lw.aggs.borrow_mut().spawn_err();
    let idx = rt_call(lw, TNEW, vec![], Ty::Usize);
    let tag = lw.emit(Ty::U64, Op::Const(tag_of(&key) as i128));
    let ent = entry_at(lw, idx, 0);
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::U64, p: ent, off: 8, v: tag });
    for (v, off) in cap_vals.iter().zip(offs.iter()) {
        let t = lw.vty(*v);
        let off = lw.emit(Ty::Usize, Op::Const((HDR + off) as i128));
        let addr = lw.emit(Ty::Ptr, Op::PtrAdd { p: ent, off });
        let r = ref_at(lw, addr, t).expect("上で確かめた");
        lw.emit(Ty::Unit, Op::Store { ptr: r, val: *v });
    }
    // `spawn_deep` の深さ (`+DEPTH_OFF` に `depth + 1` を置く。0 は既定。ネイティブの `ksn.rt.tdepth` が読む)。
    if let Some(da) = depth_arg {
        let dv = match lw.expr(da) {
            Ok(Some(v)) => v,
            Ok(None) => return Some(Ok(None)),
            Err(e) => return Some(Err(e)),
        };
        let one = lw.emit(Ty::Usize, Op::Const(1));
        let d1 = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: dv, b: one });
        lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: ent, off: DEPTH_OFF, v: d1 });
    }
    rt_call(lw, TSTART, vec![idx], Ty::Unit);

    // アロケータの値は KIR では使わない。`std.alloc.System` は `{ id: u8 }` のゼロ埋めを代用する。
    let result_ty = Ty::Res(Pay::of(hty).expect("JoinHandle がペイロードになれない"), Pay::of(spawnerr_ty).expect("SpawnError がペイロードになれない"));
    let zero = lw.emit(Ty::U8, Op::Const(0));
    let alloc = lw.emit(alloc_ty, Op::Make { variant: None, fields: vec![zero] });
    let null = lw.emit(Ty::Ptr, Op::Const(0));
    let h = lw.emit(hty, Op::Make { variant: None, fields: vec![alloc, idx, null] });
    Some(Ok(Some(lw.emit(result_ty, Op::Ok(h)))))
}

/// クロージャ `key` の入口 `ksn.thr.site.<key>(ent: ptr)`。
fn site_func(lw: &Lw, key: &str, caps: &[VId], cap_tys: &[Ty], offs: &[u32], ret: Ty) -> (Func, BTreeSet<String>) {
    let mut b = Fb::new(&[Ty::Ptr]);
    let mut args = vec![];
    for ((v, off), _) in caps.iter().zip(offs.iter()).zip(cap_tys.iter()) {
        let t = lw.vty(*v);
        let o = b.c(Ty::Usize, (HDR + off) as i128);
        let addr = b.padd(0, o);
        let r = b.emit(Ty::Ref(true, Pointee::of(t).expect("spawn_call が確かめた")), Op::Bitcast { src: Ty::Ptr, a: addr });
        args.push(b.emit(t, Op::Load(r)));
    }
    let res = b.emit(ret, Op::Call { callee: key.to_string(), args });
    if ret != Ty::Unit {
        let o = b.c(Ty::Usize, (HDR + CAPB) as i128);
        let addr = b.padd(0, o);
        let r = b.emit(Ty::Ref(true, Pointee::of(ret).expect("spawn_call が確かめた")), Op::Bitcast { src: Ty::Ptr, a: addr });
        b.emit(Ty::Unit, Op::Store { ptr: r, val: res });
    }
    let u = b.emit(Ty::Unit, Op::Const(0));
    b.term(Term::Ret(u));
    let mut f = b.build(&format!("{}{}", SITE, key), &[], Ty::Unit);
    f.uses = lw.def.uses.clone();
    f.may_panic = true;
    (f, BTreeSet::from([key.to_string()]))
}

/// `std.thread.JoinHandle.join(self)`。対象外なら `None`。
pub(super) fn join_call(lw: &mut Lw, _e: &V, recv: VId, recv_ty: Ty, line: usize) -> Option<LR<Option<VId>>> {
    let ai = match recv_ty {
        Ty::Agg(ai) => ai,
        Ty::Ref(_, Pointee::Agg(ai)) => ai,
        _ => return None,
    };
    let (is_handle, off, ret) = {
        let aggs = lw.aggs.borrow();
        let d = &aggs.defs[ai as usize];
        let off = match &d.kind {
            crate::kir::AggKind::Struct(fs) => fs.iter().find(|f| f.name == "native").map(|f| f.off),
            _ => None,
        };
        (d.name.starts_with("std.thread.JoinHandle<"), off, aggs.handle_elem.get(&ai).copied())
    };
    if !is_handle {
        return None;
    }
    let off = off.expect("JoinHandle に native が無い");
    let ret = ret.expect("JoinHandle の結果の型が無い");
    let hv = if matches!(recv_ty, Ty::Ref(..)) { lw.emit(Ty::Agg(ai), Op::Load(recv)) } else { recv };
    let id = lw.emit(Ty::Usize, Op::Field { a: hv, off });
    lw.callees.insert(JOIN.to_string());
    lw.emit(Ty::Unit, Op::Call { callee: JOIN.to_string(), args: vec![id] });
    if ret == Ty::Unit {
        rt_call(lw, TREL, vec![id], Ty::Unit);
        return Some(Ok(Some(lw.unit())));
    }
    let addr = entry_at(lw, id, HDR + CAPB);
    let Some(r) = ref_at(lw, addr, ret) else { return Some(lw.err(line, "join の結果の型は領域に置けない")) };
    let v = lw.emit(ret, Op::Load(r));
    rt_call(lw, TREL, vec![id], Ty::Unit);
    Some(Ok(Some(v)))
}

/// 全関数を下ろした後に、補助の関数 (`exec` / `run_one` / `join`) を作って足す。どれも呼ばれていなければ何もしない。
pub(super) fn finish(ok: &mut Vec<(Func, BTreeSet<String>)>) {
    let wanted = |n: &str| n == JOIN || n == RUN_ONE || n == WAIT || n == TNEW || n == TENT || n == TSTART || n == TREL || n == LOCK || n == UNLOCK || n == MINIT || n == MLOCK || n == MUNLOCK || n == RWRD || n == RWWR || n == RWUNLOCK || n.starts_with("ksn.chan.rt.");
    if !ok.iter().any(|(_, cs)| cs.iter().any(|c| wanted(c))) {
        return;
    }
    let mut sites: Vec<String> = ok.iter().filter_map(|(f, _)| f.key.strip_prefix(SITE).map(String::from)).collect();
    sites.sort();
    sites.dedup();
    let all: Vec<String> = ["alloc", "env", "io.console", "io.fs", "io.net", "process", "rand", "thread", "time"].iter().map(|s| s.to_string()).collect();
    let mk = |mut f: Func| {
        f.uses = all.clone();
        f.may_panic = true;
        f
    };
    // exec(i): tag で入口を選ぶ。
    {
        let mut b = Fb::new(&[Ty::Usize]);
        let ent = b.emit(Ty::Ptr, Op::Call { callee: TENT.to_string(), args: vec![0] });
        let tag = b.load(Ty::U64, ent, 8);
        for k in &sites {
            let want = b.c(Ty::U64, tag_of(k) as i128);
            let eq = b.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U64, a: tag, b: want });
            let (yes, _) = b.block(&[]);
            let (no, _) = b.block(&[]);
            b.br_if(eq, yes, no);
            b.at(yes);
            b.emit(Ty::Unit, Op::Call { callee: format!("{}{}", SITE, k), args: vec![ent] });
            let u = b.emit(Ty::Unit, Op::Const(0));
            b.term(Term::Ret(u));
            b.at(no);
        }
        b.emit(Ty::Unit, Op::Panic { msg: "unknown thread entry".into(), arg: None });
        b.term(Term::Unreachable);
        let mut callees: BTreeSet<String> = sites.iter().map(|k| format!("{}{}", SITE, k)).collect();
        callees.insert(TENT.to_string());
        ok.push((mk(b.build(EXEC, &[], Ty::Unit)), callees));
    }
    // run_one(): 保留の最初の 1 本を最後まで走らせて true。無ければ false。
    for (fname, run) in [(RUN_ONE, true), (HAS_PENDING, false)] {
        let mut b = Fb::new(&[]);
        let g = b.emit(Ty::Ptr, Op::GlobalBytes { name: TABLE.to_string(), size: TOTAL });
        let (lp, lv) = b.block(&[Ty::Usize]);
        let z = b.c(Ty::Usize, 0);
        b.goto(lp, vec![z]);
        b.at(lp);
        let n = b.load(Ty::Usize, g, 0);
        let end = b.cmp(crate::kir::Cc::Ge, lv[0], n);
        let (fail, _) = b.block(&[]);
        let (chk, _) = b.block(&[]);
        b.br_if(end, fail, chk);
        b.at(fail);
        let f = b.emit(Ty::Bool, Op::Const(0));
        b.term(Term::Ret(f));
        b.at(chk);
        let esz = b.c(Ty::Usize, ESZ as i128);
        let o = b.mul(lv[0], esz);
        let base = b.c(Ty::Usize, 8);
        let o = b.add(o, base);
        let ent = b.padd(g, o);
        let st = b.load(Ty::Usize, ent, 0);
        let zero = b.c(Ty::Usize, 0);
        let pending = b.cmp(Cc::Eq, st, zero);
        let (start, _) = b.block(&[]);
        let (next, _) = b.block(&[]);
        b.br_if(pending, start, next);
        b.at(next);
        let one = b.c(Ty::Usize, 1);
        let i1 = b.add(lv[0], one);
        b.goto(lp, vec![i1]);
        b.at(start);
        if run {
            let one = b.c(Ty::Usize, 1);
            b.store(Ty::Usize, ent, 0, one);
            b.emit(Ty::Unit, Op::Call { callee: EXEC.to_string(), args: vec![lv[0]] });
            let two = b.c(Ty::Usize, 2);
            b.store(Ty::Usize, ent, 0, two);
        }
        let t = b.emit(Ty::Bool, Op::Const(1));
        b.term(Term::Ret(t));
        let cs = if run { BTreeSet::from([EXEC.to_string()]) } else { BTreeSet::new() };
        ok.push((mk(b.build(fname, &[], Ty::Bool)), cs));
    }
    // join(id): 完了まで待つ。
    {
        let mut b = Fb::new(&[Ty::Usize]);
        let ent = b.emit(Ty::Ptr, Op::Call { callee: TENT.to_string(), args: vec![0] });
        let (lp, _) = b.block(&[]);
        b.goto(lp, vec![]);
        b.at(lp);
        let st = b.load(Ty::Usize, ent, 0);
        let two = b.c(Ty::Usize, 2);
        let done = b.cmp(Cc::Eq, st, two);
        let (fin, _) = b.block(&[]);
        let (chk, _) = b.block(&[]);
        b.br_if(done, fin, chk);
        b.at(fin);
        let u = b.emit(Ty::Unit, Op::Const(0));
        b.term(Term::Ret(u));
        b.at(chk);
        let one = b.c(Ty::Usize, 1);
        let running = b.cmp(Cc::Eq, st, one);
        let (dead, _) = b.block(&[]);
        let (step, _) = b.block(&[]);
        b.br_if(running, dead, step);
        b.at(step);
        let r = b.emit(Ty::Bool, Op::Call { callee: RUN_ONE.to_string(), args: vec![] });
        let (again, _) = b.block(&[]);
        b.br_if(r, again, dead);
        b.at(again);
        b.goto(lp, vec![]);
        b.at(dead);
        b.emit(Ty::Unit, Op::Panic { msg: crate::interp::DEADLOCK_JOIN.to_string(), arg: None });
        b.term(Term::Unreachable);
        ok.push((mk(b.build(JOIN, &[], Ty::Unit)), BTreeSet::from([RUN_ONE.to_string(), TENT.to_string()])));
    }
    // 補助の関数 (協調実行の本体。LLVM は pthread の本体に差し替える。kir_llvm.rs の `thread_override`)。
    let none = || BTreeSet::new();
    // new(): 表の次の項目の番号。満杯なら panic。
    {
        let mut b = Fb::new(&[]);
        let g = b.emit(Ty::Ptr, Op::GlobalBytes { name: TABLE.to_string(), size: TOTAL });
        let idx = b.load(Ty::Usize, g, 0);
        let cap = b.c(Ty::Usize, CAP as i128);
        let full = b.cmp(Cc::Ge, idx, cap);
        let (bad, _) = b.block(&[]);
        let (good, _) = b.block(&[]);
        b.br_if(full, bad, good);
        b.at(bad);
        b.emit(Ty::Unit, Op::Panic { msg: format!("thread table full (at most {} threads can be spawned in the KIR interpreters; native code has no limit)", CAP), arg: None });
        b.term(Term::Unreachable);
        b.at(good);
        let one = b.c(Ty::Usize, 1);
        let next = b.add(idx, one);
        b.store(Ty::Usize, g, 0, next);
        b.term(Term::Ret(idx));
        ok.push((mk(b.build(TNEW, &[], Ty::Usize)), none()));
    }
    // ent(i): 項目の番地。
    {
        let mut b = Fb::new(&[Ty::Usize]);
        let g = b.emit(Ty::Ptr, Op::GlobalBytes { name: TABLE.to_string(), size: TOTAL });
        let esz = b.c(Ty::Usize, ESZ as i128);
        let o = b.mul(0, esz);
        let base = b.c(Ty::Usize, 8);
        let o = b.add(o, base);
        let ent = b.padd(g, o);
        b.term(Term::Ret(ent));
        ok.push((mk(b.build(TENT, &[], Ty::Ptr)), none()));
    }
    // start(i) / release(i) / lock() / unlock(): 協調実行では何もしない。
    for (name, params) in [(TSTART, vec![Ty::Usize]), (TREL, vec![Ty::Usize]), (LOCK, vec![]), (UNLOCK, vec![]), (MINIT, vec![Ty::Ptr])] {
        let mut b = Fb::new(&params);
        let u = b.emit(Ty::Unit, Op::Const(0));
        b.term(Term::Ret(u));
        ok.push((mk(b.build(name, &[], Ty::Unit)), none()));
    }
    // rwrd / rwwr / rwunlock (RwLock): 語 (u32) は 0 空き / 0x8000_0000 書き手 / それ以外は読み手の数。協調実行では持たれた語を見るのは
    // 同じスレッドの再ロックだけで、衝突は deadlock の panic (Mutex と同じ文言・101)。
    for name in [RWRD, RWWR, RWUNLOCK] {
        let mut b = Fb::new(&[Ty::Ptr]);
        let w = b.load(Ty::U32, 0, 0);
        let wr = b.c(Ty::U32, 0x8000_0000);
        let zero = b.c(Ty::U32, 0);
        let one = b.c(Ty::U32, 1);
        let (cmp_to, conflict_if_eq) = match name {
            RWRD => (wr, true),
            RWWR => (zero, false),
            _ => (wr, true),
        };
        let is = b.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U32, a: w, b: cmp_to });
        let (yes, _) = b.block(&[]);
        let (no, _) = b.block(&[]);
        b.br_if(is, yes, no);
        // 「等しい」側 (yes) と「等しくない」側 (no)。
        let (ok_blk, bad_blk) = if conflict_if_eq { (no, yes) } else { (yes, no) };
        if name == RWUNLOCK {
            b.at(yes);
            b.store(Ty::U32, 0, 0, zero);
            let u = b.emit(Ty::Unit, Op::Const(0));
            b.term(Term::Ret(u));
            b.at(no);
            let nw = b.emit(Ty::U32, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: w, b: one });
            b.store(Ty::U32, 0, 0, nw);
            let u = b.emit(Ty::Unit, Op::Const(0));
            b.term(Term::Ret(u));
        } else {
            b.at(bad_blk);
            b.emit(Ty::Unit, Op::Panic { msg: crate::interp::DEADLOCK_LOCK.to_string(), arg: None });
            b.term(Term::Unreachable);
            b.at(ok_blk);
            let nw = if name == RWRD { b.emit(Ty::U32, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: w, b: one }) } else { wr };
            b.store(Ty::U32, 0, 0, nw);
            let u = b.emit(Ty::Unit, Op::Const(0));
            b.term(Term::Ret(u));
        }
        ok.push((mk(b.build(name, &[], Ty::Unit)), none()));
    }
    // mlock(p) / munlock(p): 協調実行では各スレッドが最後まで走るので、持たれている語 (u32) を見るのは同じスレッドの再ロックだけ
    // (ネイティブと同じく deadlock の panic)。
    {
        let mut b = Fb::new(&[Ty::Ptr]);
        let w = b.load(Ty::U32, 0, 0);
        let z = b.c(Ty::U32, 0);
        let free = b.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U32, a: w, b: z });
        let (yes, _) = b.block(&[]);
        let (no, _) = b.block(&[]);
        b.br_if(free, yes, no);
        b.at(no);
        b.emit(Ty::Unit, Op::Panic { msg: crate::interp::DEADLOCK_LOCK.to_string(), arg: None });
        b.term(Term::Unreachable);
        b.at(yes);
        let one = b.c(Ty::U32, 1);
        b.store(Ty::U32, 0, 0, one);
        let u = b.emit(Ty::Unit, Op::Const(0));
        b.term(Term::Ret(u));
        ok.push((mk(b.build(MLOCK, &[], Ty::Unit)), none()));
        let mut b = Fb::new(&[Ty::Ptr]);
        let zero = b.c(Ty::U32, 0);
        b.store(Ty::U32, 0, 0, zero);
        let u = b.emit(Ty::Unit, Op::Const(0));
        b.term(Term::Ret(u));
        ok.push((mk(b.build(MUNLOCK, &[], Ty::Unit)), none()));
    }
    // wait(): 空の recv の待ち。保留中のスレッドを 1 本走らせる。走らせるものが無ければ deadlock の panic。
    {
        let mut b = Fb::new(&[]);
        let r = b.emit(Ty::Bool, Op::Call { callee: RUN_ONE.to_string(), args: vec![] });
        let (yes, _) = b.block(&[]);
        let (dead, _) = b.block(&[]);
        b.br_if(r, yes, dead);
        b.at(yes);
        let u = b.emit(Ty::Unit, Op::Const(0));
        b.term(Term::Ret(u));
        b.at(dead);
        b.emit(Ty::Unit, Op::Panic { msg: crate::interp::DEADLOCK_RECV.to_string(), arg: None });
        b.term(Term::Unreachable);
        ok.push((mk(b.build(WAIT, &[], Ty::Unit)), BTreeSet::from([RUN_ONE.to_string()])));
    }
}
