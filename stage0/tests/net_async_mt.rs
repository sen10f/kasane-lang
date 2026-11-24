//! 複数スレッドの実行器 (`std.task.block_on_threads` / `std.task.spawn`、設計_多重化したネット.md 9〜17 章) の試験。
//! - 検査: 枠の `Send` (E2074)、取り込み・結果の `Send` (E2040)。
//! - 木走査は `n` を無視した決定的な協調実行で、結果がネイティブと同じ。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;

const SUM: &str = include_str!("data/net_async_mt_sum.ksnl");

#[test]
fn interp_sum_of_spawned_tasks() {
    let t = Tmp::new("net_async_mt_sum_interp", SUM);
    let o = run(&["run"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert_eq!(text(&o.stdout).trim(), "sum ok", "{}", out);
}

#[test]
fn check_accepts_send_frames() {
    let t = Tmp::new("net_async_mt_sum_check", SUM);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout) + &text(&o.stderr));
}

#[test]
fn generic_with_send_bound_is_accepted_across_suspension() {
    let src = SUM.replace(
        "fn t.leaf(i: usize) -> usize uses [suspend] panics never\n    std.task.yield_now()\n    i\nend t.leaf",
        "fn t.keep[T: core.marker.Send](x: T) -> T uses [suspend] panics never\n    std.task.yield_now()\n    x\nend t.keep\n\nfn t.leaf(i: usize) -> usize uses [suspend] panics never\n    t.keep(i)\nend t.leaf",
    );
    let t = Tmp::new("net_async_mt_keep", &src);
    let o = run(&["run"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
}
