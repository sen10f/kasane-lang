//! `static` の `Mutex<構造体>` (Vec・String・HashMap を持つ) と `std.sync.RwLock<T>` の下ろし。
//! 入力は `tests/data/rt_static_shared/*.ksnl`。木走査・KIR 実行器・ネイティブ (`--release`、退避なし) の出力が同じであること。

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
    let src = fs::read_to_string(root().join("stage0").join("tests").join("data").join("rt_static_shared").join(format!("{}.ksnl", name))).unwrap();
    let t = Tmp::new(&format!("rt_static_shared_{}", name), &src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}: {}", name, text(&interp.stderr));
    assert!(text(&interp.stdout).ends_with(want), "{}: {}", name, text(&interp.stdout));
    let k = run(&["kir", "t.go", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "{} KIR: {}{}", name, text(&k.stdout), text(&k.stderr));
    for release in [false, true] {
        let cache = scratch::Scratch::at(format!("ksn_cache_rt_static_shared_{}_{}_{}", std::process::id(), name, release));
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{} (release={}): {}", name, release, out);
        let kind = if release { "release" } else { "debug" };
        let p = t.0.join("target").join(kind).join("t");
        assert!(p.exists(), "{}: ネイティブの実行ファイルがない: {}", name, out);
        for _ in 0..3 {
            let r = Command::new(&p).output().unwrap();
            assert_eq!(r.status.code(), Some(0), "{} (release={}): {}", name, release, text(&r.stderr));
            assert_eq!(text(&r.stdout), text(&interp.stdout), "{} (release={})", name, release);
        }
        let _ = fs::remove_dir_all(&cache);
    }
}

#[test]
fn rt_static_shared_struct_payload_under_16_threads() {
    same_output("store", "store ok\n");
}

#[test]
fn rt_static_shared_rwlock_many_readers_and_writers() {
    same_output("rwlock", "rw ok\n");
}

/// `ksn` を 60 秒の時間制限つきで走らせる (止まったら終了コード 124 で試験が落ちる)。
fn run_to(args: &[&str], dir: &std::path::Path) -> std::process::Output {
    Command::new("timeout").arg("60").arg(exe()).args(args).current_dir(dir).output().unwrap()
}

/// 全員待ち・同じスレッドの再取得は deadlock (終了コード 101、Mutex と同じ文言)。木走査・KIR 実行器・ネイティブのどれでも。
fn deadlock_everywhere(name: &str, entry: &str) {
    if cfg!(windows) {
        return;
    }
    let src = fs::read_to_string(root().join("stage0").join("tests").join("data").join("rt_static_shared").join(format!("{}.ksnl", name))).unwrap();
    let t = Tmp::new(&format!("rt_static_shared_{}", name), &src);
    let msg = "panic: deadlock: lock() on a Mutex that another thread holds";
    let tw = run_to(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(101), "{} 木走査: {}{}", name, text(&tw.stdout), text(&tw.stderr));
    assert!(text(&tw.stderr).contains("deadlock"), "{}", text(&tw.stderr));
    let k = run_to(&["kir", entry, "--run"], &t.0);
    assert_eq!(k.status.code(), Some(101), "{} KIR: {}{}", name, text(&k.stdout), text(&k.stderr));
    assert!(text(&k.stderr).contains("deadlock"), "{}", text(&k.stderr));
    for release in [false, true] {
        let cache = scratch::Scratch::at(format!("ksn_cache_rt_static_shared_dl_{}_{}_{}", std::process::id(), name, release));
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
        let p = t.0.join("target").join(if release { "release" } else { "debug" }).join("t");
        let r = Command::new("timeout").arg("30").arg(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(101), "{} release={}: {}", name, release, text(&r.stderr));
        assert!(text(&r.stderr).contains(msg), "{}", text(&r.stderr));
        let _ = fs::remove_dir_all(&cache);
    }
}

#[test]
fn rt_static_shared_rwlock_same_thread_write_write_is_a_deadlock_101() {
    deadlock_everywhere("relock_ww", "t.go");
}

#[test]
fn rt_static_shared_rwlock_same_thread_read_then_write_is_a_deadlock_101() {
    deadlock_everywhere("relock_rw", "t.go");
}

#[test]
fn rt_static_shared_rwlock_same_thread_write_then_read_is_a_deadlock_101() {
    deadlock_everywhere("relock_wr", "t.go");
}

#[test]
fn rt_static_shared_rwlock_all_threads_blocked_is_a_deadlock_101() {
    deadlock_everywhere("rw_blocked", "t.go");
}

/// 別のスレッドが生きていて (sleep のループ)、主スレッドが同じスレッドの再取得をしても、すぐ 101 (止まらない・回り続けない)。
#[test]
fn rt_static_shared_rwlock_relock_with_a_live_thread_is_101_at_once() {
    for n in ["live_ww", "live_rw", "live_wr"] {
        deadlock_everywhere(n, "t.go");
    }
}
