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

/// 任意の wasm 例パッケージを一時ディレクトリにコピーする。
fn copy_example_dir(src_name: &str, dst_name: &str) -> PathBuf {
    let src = root().join("tools").join("wasm").join(src_name);
    let dst = std::env::temp_dir().join(format!("ksn_test_{}_{}", std::process::id(), dst_name));
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
    copy(&src, &dst);
    dst
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

/// Finding 8: ksn_resume after program end (panic) should not re-enter the executor.
/// This test builds a program that panics during await_js, then simulates a spurious
/// ksn_resume call by calling the export directly. The second call should return -2
/// and not re-run the task.
#[test]
fn ksn_resume_after_panic_returns_minus2_and_does_not_rerun() {
    if !wasm_ready() {
        eprintln!("skip: node / wasm-ld / clang がない");
        return;
    }
    let dir = copy_example_dir("await_js_after_panic", "await_js_after_panic");
    let o = run(&["build", "--target", TRIPLE], &dir);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "wasm32 build\n{}{}", out, text(&o.stderr));
    let wasm = built_file(&out, &dir);
    
    // Load the wasm and call ksn_main, then ksn_resume twice (simulating spurious call after panic)
    let test_script = dir.join("test_resume_after_panic.mjs");
    fs::write(&test_script, r#"
import fs from 'node:fs';

class Exit extends Error {
    constructor(code) {
        super(`exit ${code}`);
        this.code = code;
    }
}

const file = process.argv[2];
const { instance } = await WebAssembly.instantiate(fs.readFileSync(file), {
    env: {
        ksn_write(fd, ptr, len) {
            const view = new Uint8Array(instance.exports.memory.buffer, ptr >>> 0, len >>> 0);
            let off = 0;
            try {
                while (off < view.length) {
                    off += fs.writeSync(fd, view, off, view.length - off);
                }
            } catch (e) {
                return -1;
            }
            return view.length;
        },
        ksn_exit(code) {
            throw new Exit(code);
        },
        ksn_timer_start(ms) {
            return 1; // dummy handle
        },
    },
});

let output = '';
const origWrite = instance.exports.ksn_write;
instance.exports.ksn_write = function(fd, ptr, len) {
    const view = new Uint8Array(instance.exports.memory.buffer, ptr >>> 0, len >>> 0);
    const decoder = new TextDecoder();
    output += decoder.decode(view);
    return origWrite(fd, ptr, len);
};

try {
    // First call: ksn_main returns -1 (waiting)
    const r1 = instance.exports.ksn_main();
    console.log(`ksn_main returned: ${r1}`);
    
    // Simulate timer firing: call ksn_resume with handle 1, value 7
    // This will cause the task to panic
    try {
        const r2 = instance.exports.ksn_resume(1, 7);
        console.log(`ksn_resume (1st) returned: ${r2}`);
    } catch (e) {
        if (e instanceof Exit) {
            console.log(`ksn_resume (1st) exited with: ${e.code}`);
        } else {
            console.log(`ksn_resume (1st) trapped: ${e}`);
        }
    }
    
    // Spurious second call to ksn_resume (simulating timer firing again)
    // Should return -2 and not re-run the task
    try {
        const r3 = instance.exports.ksn_resume(1, 7);
        console.log(`ksn_resume (2nd) returned: ${r3}`);
        if (r3 === -2) {
            console.log('SUCCESS: ksn_resume returned -2 as expected');
        } else {
            console.log('FAIL: ksn_resume should return -2');
            process.exitCode = 1;
        }
    } catch (e) {
        console.log(`ksn_resume (2nd) error: ${e}`);
        process.exitCode = 1;
    }
    
    console.log('OUTPUT:');
    console.log(output);
} catch (e) {
    console.error('Test error:', e);
    process.exitCode = 1;
}
"#).unwrap();
    
    let r = Command::new("node").arg(&test_script).arg(&wasm).stdin(Stdio::null()).output().expect("spawn node");
    let got = text(&r.stdout);
    let _ = fs::remove_dir_all(&dir);
    
    // The second ksn_resume should return -2
    assert!(got.contains("ksn_resume (2nd) returned: -2"), "stdout: {}\nstderr: {}", got, text(&r.stderr));
    // The task should not re-run (no duplicate "got 7, now panic")
    let panic_count = got.matches("got 7, now panic").count();
    assert_eq!(panic_count, 1, "task should not re-run after panic, got: {}", got);
    assert_eq!(r.status.code(), Some(0), "stderr: {}", text(&r.stderr));
}

/// Finding 9: ksn_resume returning -2 should not become exit code 254.
/// Test that a normal program completion followed by spurious ksn_resume returns -2
/// and the runner ignores it (exit code 0, not 254).
#[test]
fn ksn_resume_minus2_does_not_become_exit_code_254() {
    if !wasm_ready() {
        eprintln!("skip: node / wasm-ld / clang がない");
        return;
    }
    let dir = copy_example_dir("await_js_normal_end", "await_js_normal_end");
    let o = run(&["build", "--target", TRIPLE], &dir);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "wasm32 build\n{}{}", out, text(&o.stderr));
    let wasm = built_file(&out, &dir);
    
    // Use the standard runner which should exit with code 0 (not 254)
    let r = run_await(&wasm);
    let _ = fs::remove_dir_all(&dir);
    
    // The program should exit with code 0 (normal completion)
    // If -2 were treated as exit code, it would be 254
    assert_eq!(r.status.code(), Some(0), "exit code should be 0, not 254. stderr: {}", text(&r.stderr));
}
