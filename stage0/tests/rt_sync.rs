//! 実スレッドの同期・時刻の下ろし (Mutex の futex 型の語・`static` のアトミック・`sleep`・`Instant`)。
//! 入力は `tests/data/rt_sync/*.ksnl`。インタプリタとネイティブ (`--release`、退避なし。下ろせなければ E9040 で失敗) の出力が同じであること。

#![allow(dead_code, unused_imports)]

mod common;

#[path = "common/scratch.rs"]
mod scratch;

use common::*;
use std::fs;
use std::process::Command;

fn same_output(name: &str, want: &str) {
    if cfg!(windows) {
        return; // 実スレッドは POSIX のみ
    }
    let src = fs::read_to_string(root().join("stage0").join("tests").join("data").join("rt_sync").join(format!("{}.ksnl", name))).unwrap();
    let t = Tmp::new(&format!("rt_sync_{}", name), &src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}: {}", name, text(&interp.stderr));
    assert!(text(&interp.stdout).ends_with(want), "{}: {}", name, text(&interp.stdout));
    for release in [false, true] {
        let cache = scratch::Scratch::at(format!("ksn_cache_rt_sync_{}_{}_{}", std::process::id(), name, release));
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        if !release && out.contains("skipped") {
            panic!("{}: debug ビルドがネイティブに下りない: {}", name, out);
        }
        assert_eq!(o.status.code(), Some(0), "{} (release={}): {}", name, release, out);
        let kind = if release { "release" } else { "debug" };
        let p = t.0.join("target").join(kind).join("t");
        assert!(p.exists(), "{}: ネイティブの実行ファイルがない: {}", name, out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{} (release={}): {}", name, release, text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "{} (release={})", name, release);
        let _ = fs::remove_dir_all(&cache);
    }
}

#[test]
fn rt_sync_mutex_counts_exactly_under_contention() {
    same_output("mutex", "mutex ok\n");
}

/// 16 スレッド x 20 万回の増分が Mutex の下でちょうど 320 万になる (待ちは Mutex 自身の語の futex)。
#[test]
fn rt_sync_mutex_stress_16_threads_exact_total() {
    same_output("mutex_stress", "mutex stress ok\n");
}

#[test]
fn rt_sync_static_atomics_lower_and_count_exactly() {
    same_output("static_atomic", "atomic ok\n");
}

#[test]
fn rt_sync_channel_with_many_producers_delivers_everything() {
    same_output("channel_many", "channel ok\n");
}

#[test]
fn rt_sync_sleep_waits_and_instant_is_monotonic() {
    same_output("time", "monotonic\ntime ok\n");
}

/// 同じスレッドの 2 回目の `lock` は deadlock (終了コード 101、同じ文言)。木走査・KIR 実行器・ネイティブのどれでも。
#[test]
fn rt_sync_same_thread_relock_is_a_deadlock_101_everywhere() {
    let src = fs::read_to_string(root().join("stage0").join("tests").join("data").join("rt_sync").join("relock.ksnl")).unwrap();
    let t = Tmp::new("rt_sync_relock", &src);
    let msg = "panic: deadlock: lock() on a Mutex that another thread holds";
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(101), "木走査: {}{}", text(&tw.stdout), text(&tw.stderr));
    assert!(text(&tw.stderr).contains(msg), "{}", text(&tw.stderr));
    let k = run(&["kir", "t.go", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(101), "KIR: {}{}", text(&k.stdout), text(&k.stderr));
    assert!(text(&k.stderr).contains(msg), "{}", text(&k.stderr));
    if cfg!(windows) {
        return;
    }
    for release in [false, true] {
        let cache = scratch::Scratch::at(format!("ksn_cache_rt_sync_relock_{}_{}", std::process::id(), release));
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
        if release {
            c.args(["--release", "--allow-bundle"]);
        }
        let o = c.output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
        let p = t.0.join("target").join(if release { "release" } else { "debug" }).join("t");
        let p = if p.exists() { p } else { p.with_extension("") };
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(101), "release={}: {}", release, text(&r.stderr));
        assert!(text(&r.stderr).contains(msg), "{}", text(&r.stderr));
        let _ = fs::remove_dir_all(&cache);
    }
}

/// 待ちが何本もいる中での再ロックも deadlock (101、同じ文言)。ネイティブ (debug / release)。
#[test]
fn rt_sync_contended_relock_is_a_deadlock_101_native() {
    if cfg!(windows) {
        return;
    }
    let src = fs::read_to_string(root().join("stage0").join("tests").join("data").join("rt_sync").join("relock_contended.ksnl")).unwrap();
    let t = Tmp::new("rt_sync_relock_contended", &src);
    let msg = "panic: deadlock: lock() on a Mutex that another thread holds";
    for release in [false, true] {
        let cache = scratch::Scratch::at(format!("ksn_cache_rt_sync_relock2_{}_{}", std::process::id(), release));
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
        if release {
            c.args(["--release", "--allow-bundle"]);
        }
        let o = c.output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
        let p = t.0.join("target").join(if release { "release" } else { "debug" }).join("t");
        let p = if p.exists() { p } else { p.with_extension("") };
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(101), "release={}: {}{}", release, text(&r.stdout), text(&r.stderr));
        assert!(text(&r.stderr).contains(msg), "{}", text(&r.stderr));
        let _ = fs::remove_dir_all(&cache);
    }
}

/// 別のスレッドが生きて (眠り続けて) いても、同じスレッドの再ロックはすぐ deadlock (101)。木走査・KIR・ネイティブ。60 秒眠るスレッドがいるので、
/// 全員待ちの検出に頼る実装は 10 秒の制限で落ちる。
#[test]
fn rt_sync_relock_with_busy_second_thread_is_a_deadlock_101_everywhere() {
    if cfg!(windows) {
        return;
    }
    let src = fs::read_to_string(root().join("stage0").join("tests").join("data").join("rt_sync").join("relock_busy.ksnl")).unwrap();
    let t = Tmp::new("rt_sync_relock_busy", &src);
    let msg = "panic: deadlock: lock() on a Mutex that another thread holds";
    let timed = |args: &[&str]| {
        let mut c = Command::new("timeout");
        c.arg("10").arg(exe()).args(args).current_dir(&t.0);
        c.output().unwrap()
    };
    let tw = timed(&["run"]);
    assert_eq!(tw.status.code(), Some(101), "木走査: {}{}", text(&tw.stdout), text(&tw.stderr));
    assert!(text(&tw.stderr).contains(msg), "{}", text(&tw.stderr));
    let k = timed(&["kir", "t.go", "--run"]);
    assert_eq!(k.status.code(), Some(101), "KIR: {}{}", text(&k.stdout), text(&k.stderr));
    assert!(text(&k.stderr).contains(msg), "{}", text(&k.stderr));
    for release in [false, true] {
        let cache = scratch::Scratch::at(format!("ksn_cache_rt_sync_relock3_{}_{}", std::process::id(), release));
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
        if release {
            c.args(["--release", "--allow-bundle"]);
        }
        let o = c.output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
        let p = t.0.join("target").join(if release { "release" } else { "debug" }).join("t");
        let p = if p.exists() { p } else { p.with_extension("") };
        let r = Command::new("timeout").arg("10").arg(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(101), "native release={}: {}{}", release, text(&r.stdout), text(&r.stderr));
        assert!(text(&r.stderr).contains(msg), "{}", text(&r.stderr));
        let _ = fs::remove_dir_all(&cache);
    }
}
