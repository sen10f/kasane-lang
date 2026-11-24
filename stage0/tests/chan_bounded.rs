//! `std.sync.channel(allocator, capacity)` の容量 (満杯の send は待つ)。ネイティブは実スレッドで待つ。協調実行 (木走査・KIR 実行器) は、
//! 保留のスレッドか同じチャネルの recv で止まった上の枠があるあいだだけ容量を超えて続け、そうでなければ native と同じに deadlock (101)。

#![allow(dead_code, unused_imports)]

mod common;

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use common::*;

fn pkg(name: &str) -> Tmp {
    let src = fs::read_to_string(root().join("stage0").join("tests").join("data").join("chan_bounded").join(format!("{}.ksnl", name))).unwrap();
    Tmp::new(&format!("chanb_{}", name), &src)
}

fn build(t: &Tmp, release: bool) -> PathBuf {
    let cache = scratch::Scratch::at(format!("ksn_cache_chanb_{}_{}", std::process::id(), t.0.file_name().unwrap().to_string_lossy()));
    let mut c = Command::new(exe());
    c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
    if release {
        c.args(["--release", "--allow-bundle"]);
    }
    let o = c.output().unwrap();
    assert_native_or_reasoned_skip(&(text(&o.stdout) + &text(&o.stderr)));
    let kind = if release { "release" } else { "debug" };
    let p = t.0.join("target").join(kind).join(if cfg!(windows) { "t.exe" } else { "t.native" });
    let p = native_or_launcher(p, &(text(&o.stdout) + &text(&o.stderr)));
    let _ = fs::remove_dir_all(&cache);
    p
}

/// 木走査と native (debug / release) が同じ表示で終了コード 0。
fn same_output(name: &str, want: &str) {
    let t = pkg(name);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    assert_eq!(text(&interp.stdout).trim(), want);
    if cfg!(windows) {
        return;
    }
    for release in [false, true] {
        let r = Command::new(build(&t, release)).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout));
    }
}

/// 木走査・native のどちらも 101 で、満杯の send の文言。
fn deadlocks_everywhere(name: &str, exact: bool) {
    let t = pkg(name);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(101), "{}", text(&interp.stderr));
    assert!(text(&interp.stderr).contains("send() on a full channel"), "{}", text(&interp.stderr));
    if cfg!(windows) {
        return;
    }
    for release in [false, true] {
        let r = Command::new(build(&t, release)).output().unwrap();
        assert_eq!(r.status.code(), Some(101), "{}", text(&r.stderr));
        // join で待つ側と送り手のどちらが先に全員待ちを見つけても deadlock なので、join の形は文言を "deadlock" だけ見る。
        assert!(text(&r.stderr).contains(if exact { "send() on a full channel" } else { "deadlock" }), "{}", text(&r.stderr));
    }
}

#[test]
fn chan_bounded_capacity_one_ping_pong_matches_across_paths() {
    same_output("cap1_pingpong", "cap1 ok");
}

#[test]
fn chan_bounded_send_on_full_without_a_receiver_is_deadlock_101() {
    deadlocks_everywhere("deadlock_full", true);
}

#[test]
fn chan_bounded_send_on_full_while_joining_is_deadlock_101() {
    deadlocks_everywhere("deadlock_join_full", false);
}

/// 実スレッドだけの性質: 生産者が速くても、受け手から見える遅れは容量 + 数個に収まる (キューが伸びない)。
#[test]
fn chan_bounded_native_fast_producer_stays_within_capacity() {
    if cfg!(windows) {
        return;
    }
    let t = pkg("bounded_memory");
    for release in [false, true] {
        let r = Command::new(build(&t, release)).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout).trim(), "bounded ok");
    }
}

/// 実スレッドだけ: 満杯で待っている送り手は、受信側が全部落ちると `Err(SendError { value })` で起きる。
#[test]
fn chan_bounded_native_blocked_sender_wakes_with_err_when_receivers_drop() {
    if cfg!(windows) {
        return;
    }
    let t = pkg("drop_receiver_while_blocked");
    for release in [false, true] {
        let r = Command::new(build(&t, release)).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout).trim(), "dropped ok");
    }
}

/// 容量 0 は 1 として扱う (待ち合わせではない): 受け手なしの 1 回の `send` は止まらない。
#[test]
fn chan_bounded_capacity_zero_is_one() {
    same_output("cap0", "cap0 ok");
}
