//! 取り消された未開始のタスクの本体 (`Rc`) を、合図を渡したあとのスレッドの終了まで持ち越さない (木走査の heap 破壊の回帰)。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;

const SRC: &str = include_str!("data/task_cancel_unstarted.ksnl");

#[test]
fn tree_cancel_unstarted_tasks_never_corrupts_the_heap() {
    let t = Tmp::new("task_cancel_heap", SRC);
    for k in 0..60 {
        let o = run(&["run", "--engine=tree"], &t.0);
        let out = text(&o.stdout) + &text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "run {}: {}", k, out);
        assert_eq!(text(&o.stdout).trim(), "cancel done", "run {}: {}", k, out);
    }
}
