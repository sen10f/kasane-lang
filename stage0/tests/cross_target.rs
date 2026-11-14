//! クロスコンパイル (`ksn build --target aarch64-linux-gnu`、修正プラン 5-2)。
//!
//! 実行を伴うテスト (`qemu-aarch64`・`aarch64-linux-gnu-gcc`・aarch64 の sysroot がそろうとき) は、
//! aarch64 の実行ファイルを `qemu-aarch64 -L <sysroot>` で動かし、x86_64 のネイティブと標準出力・終了コードが一致することを確かめる。
//! そろわなければ黙って飛ばす。エラーの経路 (未対応の三つ組・バックエンドの組合せ・構造体の値渡し) は道具に依らないので常に走る。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::*;

const TRIPLE: &str = "aarch64-linux-gnu";
const DEFAULT_SYSROOT: &str = "/usr/aarch64-linux-gnu";

/// 既定の sysroot。`KSN_SYSROOT` で上書きできる (`ksn build` と同じ)。
fn sysroot() -> String {
    std::env::var("KSN_SYSROOT").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| DEFAULT_SYSROOT.to_string())
}

fn have(cmd: &str) -> bool {
    Command::new(cmd).arg("--version").stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false)
}

/// 実行を伴うテストを走らせられるか (道具と sysroot がそろっているか)。そろわなければ `false` (黙って飛ばす)。
fn cross_ready() -> bool {
    have("qemu-aarch64") && have("aarch64-linux-gnu-gcc") && Path::new(&sysroot()).join("lib").join("libc.so.6").exists()
}

/// `ksn build` の標準出力 `native executable: <相対パス>` から、実行ファイルのパスを取る。
fn native_exe(out: &str, dir: &Path) -> PathBuf {
    let rel = out
        .lines()
        .find_map(|l| l.strip_prefix("native executable: "))
        .unwrap_or_else(|| panic!("no `native executable:` line in:\n{}", out));
    dir.join(rel.trim())
}

/// `src` の中身を `dst` へ複製する (`skip` の名前は除く)。
fn copy_tree_skipping(src: &Path, dst: &Path, skip: &[&str]) {
    fs::create_dir_all(dst).unwrap();
    for e in fs::read_dir(src).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if skip.contains(&name.as_str()) {
            continue;
        }
        let (s, d) = (e.path(), dst.join(&name));
        if s.is_dir() {
            copy_tree_skipping(&s, &d, skip);
        } else {
            fs::copy(&s, &d).unwrap();
        }
    }
}

/// `examples/<name>` を一時ディレクトリへ複製する (`target/` は持ち込まない。ビルド成果物はテストの中で作る)。
fn copy_example(name: &str) -> Tmp {
    let d = std::env::temp_dir().join(format!("ksn_cross_{}_ex_{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&d);
    copy_tree_skipping(&root().join("examples").join(name), &d, &["target", ".git"]);
    Tmp(d)
}

/// `dir` のパッケージを x86_64 のネイティブ (既定の `ksn build`) と aarch64 (`--target`) にビルドし、
/// aarch64 は `qemu-aarch64` で動かして、標準出力と終了コードが x86_64 のネイティブと一致することを確かめる。
fn same_on_aarch64(dir: &Path, what: &str) {
    let h = run(&["build"], dir);
    let hs = text(&h.stdout);
    assert_eq!(h.status.code(), Some(0), "{}: host build\n{}{}", what, hs, text(&h.stderr));
    assert!(!hs.contains("backend skipped"), "{}: host build fell back to the bundle\n{}", what, hs);

    let a = run(&["build", "--target", TRIPLE], dir);
    let asout = text(&a.stdout);
    assert_eq!(a.status.code(), Some(0), "{}: aarch64 build\n{}{}", what, asout, text(&a.stderr));
    assert!(!asout.contains("backend skipped"), "{}: aarch64 build fell back\n{}", what, asout);

    let hexe = native_exe(&hs, dir);
    let aexe = native_exe(&asout, dir);
    assert!(aexe.starts_with(dir.join("target").join(TRIPLE)), "{}: output not under target/{}: {}", what, TRIPLE, aexe.display());

    let hr = Command::new(&hexe).current_dir(dir).stdin(Stdio::null()).output().expect("run x86_64 native");
    let ar = Command::new("qemu-aarch64")
        .arg("-L")
        .arg(sysroot())
        .arg(&aexe)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("run aarch64 native under qemu-aarch64");
    assert_eq!(text(&ar.stdout), text(&hr.stdout), "{}: stdout (aarch64 under qemu / x86_64)", what);
    assert_eq!(ar.status.code(), hr.status.code(), "{}: exit code (aarch64 under qemu / x86_64)", what);
}

// ---- 実行を伴う一致の確認 (道具がそろうときだけ) ----

#[test]
fn examples_match_x86_64_native_on_aarch64() {
    if !cross_ready() {
        return;
    }
    // FFI を使う例 (ffi: libc の qsort に Kasane の比較関数を渡す / libm の cbrt を動的に呼ぶ) を含む。
    for name in ["demo", "tour", "threads", "json", "ffi", "native", "features"] {
        let t = copy_example(name);
        same_on_aarch64(&t.0, &format!("examples/{}", name));
    }
}

#[test]
fn linear_ok_matches_x86_64_native_on_aarch64() {
    if !cross_ready() {
        return;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/linear_ok");
    let mut names: Vec<PathBuf> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    assert!(!names.is_empty(), "linear_ok cases missing");
    for p in names {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("cross_lin_{}", stem), &fs::read_to_string(&p).unwrap());
        same_on_aarch64(&t.0, &format!("linear_ok/{}", stem));
    }
}

// ---- 対象の指定と、対応しない組合せの理由つきのエラー (道具に依らない) ----

#[test]
fn unsupported_target_is_refused_with_the_supported_list() {
    let t = Tmp::new("cross_unknown", &main_wrap("", ""));
    let o = run(&["build", "--target", "riscv64-linux-gnu"], &t.0);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{}", err);
    assert!(err.contains("E9040") && err.contains("unsupported target"), "{}", err);
    assert!(err.contains("x86_64-linux-gnu") && err.contains("aarch64-linux-gnu"), "{}", err);
    assert!(!t.0.join("target").exists(), "a failed --target build must not leave target/");
}

#[test]
fn explicit_target_needs_the_llvm_backend() {
    let t = Tmp::new("cross_c_backend", &main_wrap("", ""));
    let o = run(&["build", "--backend", "c", "--target", TRIPLE], &t.0);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{}", err);
    assert!(err.contains("needs `--backend llvm`"), "{}", err);
}

#[test]
fn struct_by_value_extern_fails_the_aarch64_build_with_a_reason() {
    // 値渡しの構造体の extern は KIR に下ろせない (今は全ターゲットで未対応)。aarch64 のビルドは、バンドルに退避せず理由を出して失敗する。
    // 呼び先は実在しなくてよい (ネイティブ生成の前に退ける)。
    let src = "type t.Small layout c derive [Clone, Copy]
    a: i32
    b: i32
end t.Small

extern t.small_add(a: t.Small, b: t.Small) -> t.Small link \"libc.so.6:small_add\"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let s = unsafe \"ffi\" \"small\" {
        t.small_add(t.Small { a: 1i32, b: 2i32 }, t.Small { a: 10i32, b: 20i32 })
    }
    core.check.eq(s.a, 11i32)
    Ok(())
end t.main
";
    let t = Tmp::new("cross_struct_extern", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let o = run(&["build", "--target", TRIPLE], &t.0);
    let out = text(&o.stdout);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{}{}", out, err);
    assert!(err.contains("E9040") && err.contains("cannot make a native executable for target aarch64-linux-gnu"), "{}{}", out, err);
    assert!(err.contains("extern"), "{}{}", out, err);
    assert!(!t.0.join("target").join(TRIPLE).join("debug").exists(), "no output for a refused build");
}

#[test]
fn explicit_host_triple_builds_under_its_own_directory() {
    if !clang_available() || !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return;
    }
    let t = Tmp::new("cross_x86_explicit", &main_wrap("", ""));
    let o = run(&["build", "--target", "x86_64-linux-gnu"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("target/x86_64-linux-gnu/debug/t.native"), "{}", out);
    let n = Command::new(t.0.join("target").join("x86_64-linux-gnu").join("debug").join("t.native")).current_dir(&t.0).output().expect("run");
    assert_eq!(n.status.code(), Some(0));
}
