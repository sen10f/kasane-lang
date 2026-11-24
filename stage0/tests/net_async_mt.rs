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

const STRESS: &str = include_str!("data/net_async_mt_stress.ksnl");
const ECHO: &str = include_str!("data/net_async_mt_echo.ksnl");

/// `batches` 個のタスクが、それぞれ `per` 個の小さなタスクを作る。合計は 1..=N の和 (N = batches * per)。
fn stress_src(batches: usize, per: usize) -> String {
    let n = (batches * per) as u64;
    STRESS
        .replace("core.iter.range(0usize, 1000usize)", &format!("core.iter.range(0usize, {}usize)", batches))
        .replace("b.wrapping_mul(100usize)", &format!("b.wrapping_mul({}usize)", per))
        .replace("t.batch(base, 100usize)", &format!("t.batch(base, {}usize)", per))
        .replace("5000050000usize", &format!("{}usize", n * (n + 1) / 2))
}

fn echo_src(clients: usize) -> String {
    ECHO.replace("let clients = 256usize", &format!("let clients = {}usize", clients))
}

#[test]
fn interp_stress_small_sum_is_exact() {
    let t = Tmp::new("net_async_mt_stress_interp", &stress_src(20, 25));
    let o = run(&["run"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert_eq!(text(&o.stdout).trim(), "stress done", "{}", out);
}

#[test]
fn native_stress_100k_tasks_sum_is_exact() {
    let (t, bin, log) = build_native_strict("net_async_mt_stress_native", &stress_src(1000, 100), false);
    let Some(bin) = bin else {
        if cfg!(target_os = "linux") {
            panic!("ネイティブが作れない: {}", log);
        }
        return;
    };
    let o = std::process::Command::new(&bin).current_dir(&t.0).output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert_eq!(text(&o.stdout).trim(), "stress done", "{}", out);
}

#[test]
fn interp_echo_256_threads() {
    let t = Tmp::new("net_async_mt_echo_interp", &echo_src(256));
    let o = run(&["run"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert_eq!(text(&o.stdout).trim(), "async echo done", "{}", out);
}

#[test]
fn native_echo_256_threads_matches_interp() {
    let (t, bin, log) = build_native_strict("net_async_mt_echo_native", &echo_src(256), true);
    let Some(bin) = bin else {
        if cfg!(target_os = "linux") {
            panic!("ネイティブが作れない: {}", log);
        }
        return;
    };
    let o = std::process::Command::new(&bin).current_dir(&t.0).output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    let i = Tmp::new("net_async_mt_echo_cmp", &echo_src(256));
    let oi = run(&["run"], &i.0);
    assert_eq!(text(&oi.stdout), text(&o.stdout));
}

fn expect_code(name: &str, src: &str, code: &str) {
    let t = Tmp::new_strict(name, src);
    let o = run(&["check"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{}", out);
    assert!(out.contains(code), "want {}: {}", code, out);
}

#[test]
fn non_send_frame_capture_and_result_are_rejected() {
    let d = |n: &str| std::fs::read_to_string(root().join("stage0/tests/data/unsound").join(n)).unwrap();
    expect_code("net_async_mt_rej_frame", &d("task_threads_frame_nonsend_generic.ksnl"), "E2074");
    expect_code("net_async_mt_rej_cap", &d("task_threads_capture_nonsend.ksnl"), "E2040");
    expect_code("net_async_mt_rej_res", &d("task_threads_result_nonsend.ksnl"), "E2040");
    expect_code("net_async_mt_rej_borrow", &d("task_threads_spawn_borrow_local.ksnl"), "E2037");
}
