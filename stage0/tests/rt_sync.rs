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
        let p = t.0.join("target").join(kind).join("t.native");
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
