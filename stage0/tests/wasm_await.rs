//! `std.task.await_js` (仕様 5.6 の第 5 段、WASM ABI の「待ち」): wasm32 の入口が JS の Promise の完了を待つ。
//!
//! - 例 `tools/wasm/await` を一時ディレクトリにコピーして `ksn build --target wasm32-unknown-unknown` し、
//!   `tools/wasm/run_await.mjs` (node) で走らせる。待ちの結果と順 (結果が待つ前に届く場合を含む) を確かめる。
//!   node・`wasm-ld-20`・`clang` がそろうときだけ走る (そろわなければ飛ばす。`wasm32.rs` と同じ作法)。
//! - 道具に依らない検査は常に走る: 木走査では JS の相手がいないので panic (101)、x86_64 のネイティブの
//!   `--release` は E9040 で止まる (wasm32 だけで使える)、`ksn check` は例を通す。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::*;

const TRIPLE: &str = "wasm32-unknown-unknown";

fn have(cmd: &str, arg: &str) -> bool {
    Command::new(cmd).arg(arg).stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false)
}

/// リンカ (`KSN_WASM_LD` で上書き。既定は `wasm-ld-20`)。
fn wasm_ld() -> String {
    std::env::var("KSN_WASM_LD").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "wasm-ld-20".to_string())
}

/// 実行を伴う検査の道具 (node・wasm-ld・clang) がそろうか。
fn wasm_ready() -> bool {
    have("node", "--version") && have(&wasm_ld(), "--version") && clang_available()
}

/// 例のパッケージ (`tools/wasm/await`)。
fn example() -> PathBuf {
    root().join("tools").join("wasm").join("await")
}

/// 例を一時ディレクトリにコピーする (`target/` は持っていかない)。
fn copy_example(name: &str) -> PathBuf {
    let dst = std::env::temp_dir().join(format!("ksn_test_{}_{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&dst);
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for e in fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let name = e.file_name();
            if name == "target" {
                continue;
            }
            if p.is_dir() {
                copy(&p, &to.join(&name));
            } else {
                fs::copy(&p, to.join(&name)).unwrap();
            }
        }
    }
    copy(&example(), &dst);
    dst
}

/// `ksn build` の標準出力 `native executable: <相対パス>` から、生成したファイルの場所を取る。
fn built_file(out: &str, dir: &Path) -> PathBuf {
    let rel = out
        .lines()
        .find_map(|l| l.strip_prefix("native executable: "))
        .unwrap_or_else(|| panic!("no `native executable:` line in:\n{}", out));
    dir.join(rel.trim())
}

/// `tools/wasm/run_await.mjs` で wasm を走らせる。
fn run_await(path: &Path) -> Output {
    let runner = root().join("tools").join("wasm").join("run_await.mjs");
    Command::new("node").arg(runner).arg(path).stdin(Stdio::null()).output().expect("spawn node")
}

/// 例が JS の完了を待って、順どおりに出て、終了コード 0 で終わる。
/// 速い方の結果は、待つ前に届く (結果の置き場に残る) ので、`slow done` の後に `buffered ok` が出る。
#[test]
fn await_js_example_resumes_from_javascript_and_exits_0() {
    if !wasm_ready() {
        eprintln!("skip: node / wasm-ld / clang がない");
        return;
    }
    let dir = copy_example("await_js_example");
    let o = run(&["build", "--target", TRIPLE], &dir);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "wasm32 build\n{}{}", out, text(&o.stderr));
    let wasm = built_file(&out, &dir);
    let r = run_await(&wasm);
    let got = text(&r.stdout);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(got, "waiting\nfirst\nboth 7\nslow done\nbuffered ok\n", "stderr: {}", text(&r.stderr));
    assert_eq!(r.status.code(), Some(0), "stderr: {}", text(&r.stderr));
}

/// wasm32 の例は、`ksn_resume` と `ksn_main` を export し、JS の import (`ksn_timer_start`) を使う。
#[test]
fn await_js_example_exports_resume_and_imports_the_timer() {
    if !wasm_ready() {
        eprintln!("skip: node / wasm-ld / clang がない");
        return;
    }
    let dir = copy_example("await_js_abi");
    let o = run(&["build", "--target", TRIPLE], &dir);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "wasm32 build\n{}{}", out, text(&o.stderr));
    let wasm = built_file(&out, &dir);
    let runner = root().join("tools").join("wasm").join("run.mjs");
    let abi = Command::new("node").arg(runner).arg("--abi").arg(&wasm).output().expect("spawn node");
    let _ = fs::remove_dir_all(&dir);
    let json = text(&abi.stdout);
    assert!(json.contains("env.ksn_timer_start:function"), "{}", json);
    assert!(json.contains("ksn_resume:function"), "{}", json);
    assert!(json.contains("ksn_main:function"), "{}", json);
}

/// 例は `ksn check` を通る (例外的な診断がない)。
#[test]
fn await_js_example_passes_check() {
    let dir = copy_example("await_js_check");
    let o = run(&["check", "--json"], &dir);
    let _ = fs::remove_dir_all(&dir);
    let codes = codes(&text(&o.stdout));
    assert!(codes.is_empty(), "{:?}", codes);
}

/// 木走査には JS の相手がいない: `await_js` は理由つきの panic (終了コード 101)。
#[test]
fn await_js_panics_in_the_tree_walker() {
    let src = "fn t.wait() -> i32 uses [suspend] panics may
    std.task.await_js(1u32)
end t.wait

fn t.main() -> () uses [suspend] panics may
    std.task.block_on(fn() -> () capture [] {
        let v = t.wait()
        core.mem.drop(v)
    })
end t.main
";
    let t = Tmp::new("await_js_tree", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101), "stdout: {}\nstderr: {}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stderr).contains("needs a JavaScript host"), "{}", text(&o.stderr));
}

/// x86_64 のネイティブの `--release` は、`await_js` を理由つきの E9040 で止める (wasm32 だけで使える)。
#[test]
fn await_js_is_refused_by_the_native_release_build() {
    let dir = copy_example("await_js_native");
    let o = run(&["build", "--release"], &dir);
    let _ = fs::remove_dir_all(&dir);
    let all = format!("{}{}", text(&o.stdout), text(&o.stderr));
    assert_ne!(o.status.code(), Some(0), "{}", all);
    assert!(all.contains("E9040"), "{}", all);
    assert!(all.contains("await_js"), "{}", all);
}
