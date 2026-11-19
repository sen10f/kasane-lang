//! `ksn build --emit-llvm <ファイル>`: ネイティブを作らず、clang に渡す前の LLVM IR (`.ll`) を書き出す。
//! 書き出した `.ll` をランタイム C (`kir_rt.c`) と clang でリンクして実行し、`ksn build` のネイティブと
//! 出力が一致することを確かめる。

#![allow(dead_code, unused_imports)]

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::process::{Command, Output};

mod common;
use common::*;

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

/// `Result` を返す入口 `t.main` で、標準出力に 1 行書くだけのプログラム。
fn emit_prog() -> String {
    main_wrap("", &body(&["std.io.print_line(\"emit-llvm ok\")?"]))
}

#[test]
fn emit_llvm_matches_native_output() {
    let t = Tmp::new("emit_llvm", &emit_prog());
    let cache = scratch::Scratch::at(format!("ksn_emit_llvm_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let stdlib = root().join("spec").join("stdlib");

    // `ksn build --emit-llvm out.ll`: ネイティブは作らず `.ll` だけを書き出す。
    let ll = t.0.join("out.ll");
    let o = run_with_env(
        &["build", "--emit-llvm", "out.ll"],
        &t.0,
        &[("KSN_CACHE_DIR", cache.to_str().unwrap()), ("KSN_STDLIB", stdlib.to_str().unwrap())],
    );
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    assert!(ll.exists(), "emit-llvm should write the .ll file: {} {}", text(&o.stdout), text(&o.stderr));

    // `ksn build` のネイティブ。
    let o = run_with_env(
        &["build"],
        &t.0,
        &[("KSN_CACHE_DIR", cache.to_str().unwrap()), ("KSN_STDLIB", stdlib.to_str().unwrap())],
    );
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    let native = t.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t.native" });
    assert!(native.exists(), "native executable after ksn build");
    let n = Command::new(&native).output().unwrap();
    assert_eq!(n.status.code(), Some(0), "{} {}", text(&n.stdout), text(&n.stderr));

    // `.ll` を clang でランタイム C (`kir_rt.c`) とリンクして実行。
    let clang = std::env::var("KSN_CLANG").unwrap_or_else(|_| "clang".to_string());
    let rt = root().join("stage0").join("src").join("kir_rt.c");
    assert!(rt.exists(), "runtime C not found: {}", rt.display());
    let exe_out = t.0.join(if cfg!(windows) { "emit_llvm.exe" } else { "emit_llvm" });
    let _ = fs::remove_file(&exe_out);
    let co = Command::new(&clang)
        .args(["-O1", "-Wno-override-module"])
        .arg(&ll)
        .arg(&rt)
        .arg("-o")
        .arg(&exe_out)
        .arg("-lm")
        .arg("-pthread")
        .output()
        .expect("spawn clang");
    assert!(co.status.success(), "clang link should succeed: {} {}", text(&co.stdout), text(&co.stderr));
    let r = Command::new(&exe_out).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{} {}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), text(&n.stdout), "emit-llvm output must match the native output");

    let _ = fs::remove_dir_all(&cache);
}
