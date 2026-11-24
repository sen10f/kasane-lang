//! `std.task` の KIR 下ろし (仕様 5.6、設計_KIR.md 26 章): `block_on` / `spawn_local` / `yield_now` / `Task<T>.join_task`。
//!
//! - `block_on(閉包)`: 閉包の枠 `K.new(取り込み)` を作り、実行器 `ksn.task.run(番号, 枠)` を回す。根の結果を枠から読み、実行器を畳む。
//!   実行器は呼び出しの間だけ生きる (木走査と同じく、入れ子は実行時の panic)。
//! - `spawn_local(閉包)`: 枠を作って実行器の表へ登録する (`ksn.task.spawn`)。走らせるのは実行器の中で、譲ったとき。
//!   `Task<T>` は `{id: usize}` の集成体。
//! - `yield_now()` と `join_task`: 中断点 (`ksn.task.yield` / `ksn.task.wait`)。変換 (`kir_suspend`) が枠の形にする。
//!
//! 閉包の番号 (タスクの番号) は、下ろしの順に決まる。`finish` が、本体のある閉包の表 (`ksn.task.dispatch`) と実行器を足す。

use std::cell::RefCell;
use std::collections::BTreeSet;

use crate::kir::{Func, Op, Pointee, Term, Ty, VId};
use crate::kir_suspend as ks;
use crate::parser::V;

use super::fb::Fb;
use super::thread_lower as thr;
use super::{coll, Lw, LR};

thread_local! {
    /// 閉包の鍵の列 (番号 = タスクの番号)。`lower_defs` が始めに空にする。
    static TASKS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// 実行器を使ったか (使わなければ実行器は足さない)。
    static USED: RefCell<bool> = const { RefCell::new(false) };
    /// `block_on_threads` を使ったか (ワーカーの起動・合流の補助と入口を足す)。
    static MT_USED: RefCell<bool> = const { RefCell::new(false) };
}

/// ワーカーの入口の鍵 (`ksn.thr.site.<これ>` が入口、tag はこの鍵の FNV-1a)。
const WORKER_KEY: &str = "ksn.task.worker";

/// 登録された閉包の鍵 (番号順)。`kir_suspend::transform` が、その `.new` / `.poll` を作る。
pub(super) fn task_keys() -> Vec<String> {
    TASKS.with(|t| t.borrow().clone())
}

/// 下ろしの前に、表と使用の記録を空にする。
pub(super) fn reset() {
    TASKS.with(|t| t.borrow_mut().clear());
    USED.with(|u| *u.borrow_mut() = false);
    MT_USED.with(|u| *u.borrow_mut() = false);
}

/// 閉包 `key` の番号 (初めてなら登録する)。
fn task_no(key: &str) -> u32 {
    TASKS.with(|t| {
        let mut t = t.borrow_mut();
        match t.iter().position(|k| k == key) {
            Some(i) => i as u32,
            None => {
                t.push(key.to_string());
                (t.len() - 1) as u32
            }
        }
    })
}

fn mark_used() {
    USED.with(|u| *u.borrow_mut() = true);
}

/// スレッドの補助の関数 (`thread_lower::finish`) より前に呼ぶ: `block_on_threads` を使っていれば、ワーカーの入口 `ksn.thr.site.ksn.task.worker` を足す
/// (`ksn.thr.exec` が入口の一覧から作られるため)。
pub(super) fn pre_finish(ok: &mut Vec<(Func, BTreeSet<String>)>) {
    if !MT_USED.with(|u| *u.borrow()) {
        return;
    }
    let mut b = Fb::new(&[Ty::Ptr]);
    b.emit(Ty::Unit, Op::Call { callee: ks::WORKER.to_string(), args: vec![] });
    let u = b.emit(Ty::Unit, Op::Const(0));
    b.term(Term::Ret(u));
    let mut f = b.build(&format!("{}{}", thr::SITE, WORKER_KEY), &[], Ty::Unit);
    f.uses = ["alloc", "env", "io.console", "io.fs", "io.net", "process", "rand", "thread", "time"].iter().map(|s| s.to_string()).collect();
    f.may_panic = true;
    ok.push((f, BTreeSet::from([ks::WORKER.to_string()])));
}

/// 全部の関数を下ろした後に、実行器 (`ksn.task.*`) を `ok` に足す。使われていなければ何もしない。
/// 表には、本体 (`K.poll` を作る閉包) が `ok` にあるものだけを入れる。
pub(super) fn finish(ok: &mut Vec<(Func, BTreeSet<String>)>) {
    if !USED.with(|u| *u.borrow()) {
        return;
    }
    let present: Vec<(u32, String)> = TASKS.with(|t| {
        t.borrow()
            .iter()
            .enumerate()
            .filter(|(_, k)| ok.iter().any(|(f, _)| &f.key == *k))
            .map(|(i, k)| (i as u32, k.clone()))
            .collect()
    });
    let mt = MT_USED.with(|u| *u.borrow());
    let funcs = ks::exec_funcs_mt(&present, if mt { Some(thr::tag_of(WORKER_KEY)) } else { None }).expect("実行器の KIR は読める (kir_suspend の試験で確かめている)");
    ok.extend(funcs.into_iter().map(|f| (f, BTreeSet::new())));
}

/// 呼び出し先 `name` (実行器の関数や `K.new` 以外。呼び先の集合に入れる) の呼び出し。
fn rt_call(lw: &mut Lw, name: &str, args: Vec<VId>, ret: Ty) -> VId {
    lw.callees.insert(name.to_string());
    lw.emit(ret, Op::Call { callee: name.to_string(), args })
}

/// 閉包 `key` の枠を作る呼び出し `K.new(取り込み)` (変換で作られる)。
fn new_frame(lw: &mut Lw, key: &str, caps: &[VId]) -> VId {
    lw.callees.insert(key.to_string());
    lw.emit(Ty::Ptr, Op::Call { callee: format!("{}.new", key), args: caps.to_vec() })
}

/// 枠 `base` の結果 (オフセット 8) を型 `t` で読む。単位型は単位の値。
fn result_of(lw: &mut Lw, base: VId, t: Ty, line: usize) -> LR<VId> {
    match t {
        Ty::Unit => Ok(lw.unit()),
        Ty::Agg(_) => {
            let pt = Pointee::of(t).ok_or_else(|| format!("{}行: 結果の型が参照できない", line))?;
            let c = lw.emit(Ty::Usize, Op::Const(8));
            let pa = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off: c });
            let r = lw.emit(Ty::Ref(true, pt), Op::Bitcast { src: Ty::Ptr, a: pa });
            Ok(lw.emit(t, Op::Load(r)))
        }
        _ if crate::kir::memable(t) => Ok(lw.emit(t, Op::MemLoad { p: base, off: 8 })),
        // `Result` は枠の補助関数 (`kir_suspend::res_helpers`、変換が作る) で読む。
        Ty::Res(..) => {
            let c = lw.emit(Ty::Usize, Op::Const(8));
            let pa = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off: c });
            let (_, get) = crate::kir_suspend::res_fn_names(t);
            Ok(lw.emit(t, Op::Call { callee: get, args: vec![pa] }))
        }
        _ => Err(format!("{}行: std.task の結果の型 {} は段階 1 の対象外", line, crate::kir::fmt_ty(t))),
    }
}

/// `std.task.*` の関数の呼び出し (パスの形)。対象外なら `None` (親へ戻す)。
pub(super) fn path_call(lw: &mut Lw, segs: &[String], args: &[V], line: usize) -> Option<LR<Option<VId>>> {
    let name = segs.iter().map(|s| s.as_str()).collect::<Vec<_>>();
    match name.as_slice() {
        ["std", "task", "block_on"] => Some(block_on(lw, args, line)),
        ["std", "task", "spawn_local"] => Some(spawn_local(lw, args, line, ks::SPAWN)),
        ["std", "task", "spawn"] => Some(spawn_local(lw, args, line, ks::SPAWN_ANY)),
        // 複数スレッドの実行器: アロケータは使わない。`n` はネイティブ (Linux) だけが使い、参照実行器・木走査は協調実行のまま (決定的)。
        ["std", "task", "block_on_threads"] if args.len() == 3 => Some(block_on_threads(lw, args, line)),
        ["std", "task", "yield_now"] => Some(yield_now(lw, args, line)),
        ["std", "task", "await_js"] => Some(await_js(lw, args, line)),
        ["std", "task", "wait_io"] => Some(wait_io(lw, args, line)),
        _ => None,
    }
}

/// 閉包の引数を調べる: 本体の鍵・取り込みの値・取り込みの型・戻り値の型 (引数のない閉包だけ)。
fn closure_of(lw: &mut Lw, arg: &V, line: usize, spawn: bool) -> LR<(String, Vec<VId>, Ty)> {
    lw.coll.spawn_arg = spawn;
    let kind = coll::closure_arg_kind(lw, arg);
    lw.coll.spawn_arg = false;
    match kind? {
        Some(coll::Kind::Closure { key, args, ret, caps, .. }) if args.is_empty() => {
            if spawn && caps.iter().any(|v| matches!(lw.vty(*v), Ty::Ref(..))) {
                return lw.err(line, "spawn_local の閉包は参照を取り込めない ('static でなければならない)");
            }
            Ok((key, caps, ret))
        }
        _ => lw.err(line, "std.task の引数は引数のない閉包でなければならない"),
    }
}

/// `std.task.block_on(閉包)`: 閉包を根の本体として実行器を回し、その結果を返す。
fn block_on(lw: &mut Lw, args: &[V], line: usize) -> LR<Option<VId>> {
    let [arg] = args else { return lw.err(line, "std.task.block_on の引数の個数が違う") };
    let (key, caps, ret) = closure_of(lw, arg, line, false)?;
    mark_used();
    let tag = task_no(&key);
    let root = new_frame(lw, &key, &caps);
    let tagv = lw.emit(Ty::U32, Op::Const(tag as i128));
    rt_call(lw, ks::RUN, vec![tagv, root], Ty::Unit);
    let zero = lw.emit(Ty::Usize, Op::Const(0));
    let rfp = rt_call(lw, ks::FP, vec![zero], Ty::Ptr);
    let v = result_of(lw, rfp, ret, line)?;
    rt_call(lw, ks::FINISH, vec![], Ty::Unit);
    Ok(Some(v))
}

/// `std.task.block_on_threads(アロケータ, n, 閉包)`: `n` 本のワーカー (ネイティブ Linux) で `block_on` と同じ意味に走らせる。
fn block_on_threads(lw: &mut Lw, args: &[V], line: usize) -> LR<Option<VId>> {
    let [_alloc, nv, arg] = args else { return lw.err(line, "std.task.block_on_threads の引数の個数が違う") };
    let (key, caps, ret) = closure_of(lw, arg, line, false)?;
    let Some(n) = lw.expr(nv)? else { return Ok(None) };
    mark_used();
    MT_USED.with(|u| *u.borrow_mut() = true);
    // ワーカーの入口 (`site_for_worker`) は `pre_finish` が足す。補助 (`ksn.thr.*`) を作らせるため、使う名前を呼び先に入れる。
    lw.callees.insert(format!("{}{}", thr::SITE, WORKER_KEY));
    for name in [thr::TNEW, thr::TENT, thr::TSTART, thr::TREL, thr::JOIN] {
        lw.callees.insert(name.to_string());
    }
    let tag = task_no(&key);
    let root = new_frame(lw, &key, &caps);
    let tagv = lw.emit(Ty::U32, Op::Const(tag as i128));
    rt_call(lw, ks::MT_BEGIN, vec![n], Ty::Unit);
    rt_call(lw, ks::RUN, vec![tagv, root], Ty::Unit);
    let zero = lw.emit(Ty::Usize, Op::Const(0));
    let rfp = rt_call(lw, ks::FP, vec![zero], Ty::Ptr);
    let v = result_of(lw, rfp, ret, line)?;
    rt_call(lw, ks::MT_END, vec![], Ty::Unit);
    rt_call(lw, ks::FINISH, vec![], Ty::Unit);
    Ok(Some(v))
}

/// `std.task.spawn_local(閉包)` / `std.task.spawn(閉包)`: 閉包を実行器の表へ登録し、`Task<T>` を返す (`spawn_fn` は `ksn.task.spawn` か `spawn_any`)。
fn spawn_local(lw: &mut Lw, args: &[V], line: usize, spawn_fn: &str) -> LR<Option<VId>> {
    let [arg] = args else { return lw.err(line, "std.task.spawn_local の引数の個数が違う") };
    let (key, caps, ret) = closure_of(lw, arg, line, true)?;
    mark_used();
    let tag = task_no(&key);
    let root = new_frame(lw, &key, &caps);
    let tagv = lw.emit(Ty::U32, Op::Const(tag as i128));
    let ptr = rt_call(lw, spawn_fn, vec![tagv, root], Ty::Ptr);
    let id = lw.emit(Ty::Usize, Op::MemLoad { p: ptr, off: 0 });
    let gen = lw.emit(Ty::Usize, Op::MemLoad { p: ptr, off: 8 });
    lw.emit(Ty::Unit, Op::HeapFree(ptr));
    let hty = lw.aggs.borrow_mut().task_handle(ret);
    Ok(Some(lw.emit(hty, Op::Make { variant: None, fields: vec![id, gen] })))
}

/// `std.task.yield_now()`: 中断点 (実行器の外では何もしない)。
fn yield_now(lw: &mut Lw, args: &[V], line: usize) -> LR<Option<VId>> {
    if !args.is_empty() {
        return lw.err(line, "std.task.yield_now の引数は取れない");
    }
    mark_used();
    lw.callees.insert(ks::YIELD.to_string());
    lw.emit(Ty::Unit, Op::Call { callee: ks::YIELD.to_string(), args: vec![] });
    Ok(Some(lw.unit()))
}

/// `std.task.wait_io(fd, write)` (内部。`std.net` の `*_async` が使う): ソケット `fd` が読める (`write` = 0) / 書ける (1) か、期限が来るまで中断する。
/// 結果は 0 (準備完了) か 1 (期限切れ)。実行器の外・リアクターの無い対象では中断せず 0 を返す (その後の読み書きがそのまま待つ)。
fn wait_io(lw: &mut Lw, args: &[V], line: usize) -> LR<Option<VId>> {
    let [fd, w] = args else { return lw.err(line, "std.task.wait_io の引数の個数が違う") };
    let Some(fd) = lw.expr(fd)? else { return Ok(None) };
    let Some(w) = lw.expr(w)? else { return Ok(None) };
    let fd = lw.emit(Ty::U32, Op::Conv { src: Ty::I32, a: fd });
    let w = lw.emit(Ty::U32, Op::Conv { src: Ty::I32, a: w });
    mark_used();
    lw.callees.insert(ks::IO.to_string());
    Ok(Some(lw.emit(Ty::I32, Op::Call { callee: ks::IO.to_string(), args: vec![fd, w] })))
}

/// 型が `std.task.Task<T>` か (集成体の名前で決める)。
pub(super) fn is_task_ty(lw: &Lw, t: Ty) -> bool {
    let Ty::Agg(ai) = t else { return false };
    lw.aggs.borrow().defs[ai as usize].name.starts_with("std.task.Task<")
}

/// `h.cancel()` (`Task<T>` の受け手、仕様 5.6 の第 4 段): 枠を解放して取り消す。対象外なら `None`。
/// 取り消しは中断点ではない (呼び出し元の `uses` は要らない)。`Task<T>` は `{id, gen}` の集成体なので、番号と世代を取り出して渡す。
pub(super) fn cancel_call(lw: &mut Lw, recv: VId, recv_ty: Ty, line: usize) -> Option<LR<Option<VId>>> {
    let _ = line;
    if !is_task_ty(lw, recv_ty) {
        return None;
    }
    mark_used();
    let id = lw.emit(Ty::Usize, Op::Field { a: recv, off: 0 });
    let gen = lw.emit(Ty::Usize, Op::Field { a: recv, off: 8 });
    lw.callees.insert(ks::CANCEL.to_string());
    lw.emit(Ty::Unit, Op::Call { callee: ks::CANCEL.to_string(), args: vec![id, gen] });
    Some(Ok(Some(lw.unit())))
}

/// `std.task.await_js(h)` (仕様 5.6 の第 5 段): JS の Promise の完了を待つ中断点。結果は JS が `ksn_resume(h, 値)` で渡した `i32`。
/// wasm32 だけで使える (JS の host がない木走査とネイティブでは使えない)。
fn await_js(lw: &mut Lw, args: &[V], line: usize) -> LR<Option<VId>> {
    let [arg] = args else { return lw.err(line, "std.task.await_js の引数の個数が違う") };
    if crate::ptrwidth::bits() != 32 {
        return lw.err(line, "std.task.await_js は wasm32 のビルドだけで使える (JS の完了を待つ。E9040)");
    }
    let Some(h) = lw.expr(arg)? else { return Ok(None) };
    mark_used();
    lw.callees.insert(ks::AWAIT.to_string());
    Ok(Some(lw.emit(Ty::I32, Op::Call { callee: ks::AWAIT.to_string(), args: vec![h] })))
}

/// `h.join_task()` (`Task<T>` の受け手): 中断点 (完了まで待つ) の後、結果を取り出す。対象外なら `None`。
pub(super) fn join_call(lw: &mut Lw, recv: VId, recv_ty: Ty, line: usize) -> Option<LR<Option<VId>>> {
    let Ty::Agg(ai) = recv_ty else { return None };
    let (is_task, elem) = {
        let aggs = lw.aggs.borrow();
        let d = &aggs.defs[ai as usize];
        (d.name.starts_with("std.task.Task<"), aggs.handle_elem.get(&ai).copied())
    };
    if !is_task {
        return None;
    }
    let Some(elem) = elem else { return Some(lw.err(line, "Task の結果の型が無い")) };
    mark_used();
    let id = lw.emit(Ty::Usize, Op::Field { a: recv, off: 0 });
    let gen = lw.emit(Ty::Usize, Op::Field { a: recv, off: 8 });
    lw.callees.insert(ks::WAIT.to_string());
    lw.emit(Ty::Unit, Op::Call { callee: ks::WAIT.to_string(), args: vec![id, gen] });
    let fp = rt_call(lw, ks::FP, vec![id], Ty::Ptr);
    Some(result_of(lw, fp, elem, line).map(Some))
}
