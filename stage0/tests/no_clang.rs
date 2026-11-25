//! clang なしでネイティブのビルドができること (L2、`実装計画.md` の「実行時の C を埋め込み」)。
//! `PATH` を空の dir にし、`KSN_CLANG=/nonexistent` にして `ksn build` を走らせる。実行ファイルはバンドルではなく、
//! 出力が `ksn run` と一致する。aarch64 は qemu と sysroot、wasm32-wasi は node があるときだけ走らせる (`cross_target.rs` の作法)。
//! 標準ライブラリの埋め込みも確かめる (`spec/` の無い場所へ写した `ksn` が `ksn check` を通す)。

#![allow(dead_code, unused_imports)]

mod common;

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::*;

const PROG: &str = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never
    core.mem.drop(std.io.print_line(\"no clang ok\"))
    Ok(())
end t.main
";

fn have(cmd: &str) -> bool {
    Command::new(cmd).arg("--version").stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false)
}

/// clang (と PATH のほかの道具) を見せない環境で `ksn` を走らせる。
fn ksn_noclang(ksn: &str, args: &[&str], cwd: &Path, cache: &Path, empty_path: &Path) -> Output {
    Command::new(ksn)
        .args(args)
        .current_dir(cwd)
        .env("PATH", empty_path)
        .env("KSN_CLANG", "/nonexistent/clang")
        .env("KSN_CACHE_DIR", cache)
        .env_remove("KSN_STDLIB")
        .output()
        .expect("spawn ksn")
}

fn exe_line(out: &str, dir: &Path) -> PathBuf {
    let rel = out.lines().find_map(|l| l.strip_prefix("native executable: ")).unwrap_or_else(|| panic!("no `native executable:` line in:\n{}", out));
    dir.join(rel.trim())
}

fn setup(name: &str) -> (Tmp, PathBuf, PathBuf) {
    let t = Tmp::new(name, PROG);
    let base = scratch::Scratch::at(format!("ksn_noclang_{}_{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&base);
    let (cache, empty) = (base.join("cache"), base.join("empty"));
    fs::create_dir_all(&cache).unwrap();
    fs::create_dir_all(&empty).unwrap();
    (t, cache, empty)
}

#[test]
fn host_release_build_is_native_without_clang() {
    let (t, cache, empty) = setup("host");
    let b = ksn_noclang(exe(), &["build", "--release"], &t.0, &cache, &empty);
    let out = text(&b.stdout);
    assert_eq!(b.status.code(), Some(0), "{}{}", out, text(&b.stderr));
    assert!(!out.contains("native backend skipped"), "バンドルに退避した: {}", out);
    let native = exe_line(&out, &t.0);
    assert!(crate::common::artifact_is_native(&native), "{}", out);
    let r = Command::new(&native).output().unwrap();
    let want = run(&["run"], &t.0);
    assert_eq!(r.status.code(), want.status.code());
    assert_eq!(r.stdout, want.stdout);
    assert_eq!(text(&r.stdout), "no clang ok\n");
    let _ = fs::remove_dir_all(cache.parent().unwrap());
}

#[test]
fn aarch64_build_is_native_without_clang() {
    let sysroot = std::env::var("KSN_SYSROOT").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/usr/aarch64-linux-gnu".to_string());
    if !(have("qemu-aarch64") && Path::new(&sysroot).join("lib").join("libc.so.6").exists()) {
        eprintln!("skipped: qemu-aarch64 か aarch64 の sysroot がない");
        return;
    }
    let (t, cache, empty) = setup("aarch64");
    let b = ksn_noclang(exe(), &["build", "--target", "aarch64-linux-gnu"], &t.0, &cache, &empty);
    let out = text(&b.stdout);
    assert_eq!(b.status.code(), Some(0), "{}{}", out, text(&b.stderr));
    let native = exe_line(&out, &t.0);
    let r = Command::new("qemu-aarch64").arg("-L").arg(&sysroot).arg(&native).output().unwrap();
    assert_eq!(text(&r.stdout), "no clang ok\n", "{}", text(&r.stderr));
    assert_eq!(r.status.code(), Some(0));
    let _ = fs::remove_dir_all(cache.parent().unwrap());
}

#[test]
fn wasm32_wasi_build_works_without_clang() {
    let runner = root().join("tools").join("wasm").join("run_wasi.mjs");
    if !(have("node") && runner.exists()) {
        eprintln!("skipped: node がない");
        return;
    }
    let (t, cache, empty) = setup("wasi");
    let b = ksn_noclang(exe(), &["build", "--target", "wasm32-wasi"], &t.0, &cache, &empty);
    let out = text(&b.stdout);
    assert_eq!(b.status.code(), Some(0), "{}{}", out, text(&b.stderr));
    let wasm = exe_line(&out, &t.0);
    let r = Command::new("node").arg(&runner).arg(&wasm).current_dir(&t.0).output().unwrap();
    assert_eq!(text(&r.stdout), "no clang ok\n", "{}", text(&r.stderr));
    let _ = fs::remove_dir_all(cache.parent().unwrap());
}

#[test]
fn embedded_stdlib_lets_a_lone_ksn_check() {
    let (t, cache, empty) = setup("lone");
    let lone = cache.parent().unwrap().join("lone");
    fs::create_dir_all(&lone).unwrap();
    let k = lone.join("ksn");
    fs::copy(exe(), &k).unwrap();
    // 写した ksn の周り (親を辿っても) に spec/stdlib は無い。カレント (パッケージ) の周りにも無い (一時の場所)。
    let o = ksn_noclang(k.to_str().unwrap(), &["check"], &t.0, &cache, &empty);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(fs::read_dir(&cache).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("stdlib-")), "埋め込みの標準ライブラリを展開していない");
    let _ = fs::remove_dir_all(cache.parent().unwrap());
}
