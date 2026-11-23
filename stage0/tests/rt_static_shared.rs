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
        let p = t.0.join("target").join(kind).join("t.native");
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
