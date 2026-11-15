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

use crate::kir::{Func, Op, Pointee, Ty, VId};
use crate::kir_suspend as ks;
use crate::parser::V;

use super::{coll, Lw, LR};

thread_local! {
    /// 閉包の鍵の列 (番号 = タスクの番号)。`lower_defs` が始めに空にする。
    static TASKS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// 実行器を使ったか (使わなければ実行器は足さない)。
    static USED: RefCell<bool> = const { RefCell::new(false) };
}

/// 登録された閉包の鍵 (番号順)。`kir_suspend::transform` が、その `.new` / `.poll` を作る。
pub(super) fn task_keys() -> Vec<String> {
    TASKS.with(|t| t.borrow().clone())
}

/// 下ろしの前に、表と使用の記録を空にする。
pub(super) fn reset() {
    TASKS.with(|t| t.borrow_mut().clear());
    USED.with(|u| *u.borrow_mut() = false);
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
    let funcs = ks::exec_funcs(&present).expect("実行器の KIR は読める (kir_suspend の試験で確かめている)");
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
        ["std", "task", "spawn_local"] => Some(spawn_local(lw, args, line)),
        ["std", "task", "yield_now"] => Some(yield_now(lw, args, line)),
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

/// `std.task.spawn_local(閉包)`: 閉包を実行器の表へ登録し、`Task<T>` を返す。
fn spawn_local(lw: &mut Lw, args: &[V], line: usize) -> LR<Option<VId>> {
    let [arg] = args else { return lw.err(line, "std.task.spawn_local の引数の個数が違う") };
    let (key, caps, ret) = closure_of(lw, arg, line, true)?;
    mark_used();
    let tag = task_no(&key);
    let root = new_frame(lw, &key, &caps);
    let tagv = lw.emit(Ty::U32, Op::Const(tag as i128));
    let id = rt_call(lw, ks::SPAWN, vec![tagv, root], Ty::Usize);
    let hty = lw.aggs.borrow_mut().task_handle(ret);
    Ok(Some(lw.emit(hty, Op::Make { variant: None, fields: vec![id] })))
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
    lw.callees.insert(ks::WAIT.to_string());
    lw.emit(Ty::Unit, Op::Call { callee: ks::WAIT.to_string(), args: vec![id] });
    let fp = rt_call(lw, ks::FP, vec![id], Ty::Ptr);
    Some(result_of(lw, fp, elem, line).map(Some))
}
