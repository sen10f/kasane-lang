//! wasm32 の JS 接続 (修正プラン 5-3): `link "env:名前"` の import、`ksn_` の関数の export、`ksn bindgen`、
//! TODO の SPA (`apps/todo_spa`) を node の上の偽の DOM で動かす確認。
//!
//! 道具 (node・wasm-ld-20・clang) がそろうときだけ実行を伴う検査を走らせる (`wasm32.rs` と同じ作法。そろわなければ飛ばす)。
//! 道具に依らない検査 (拒否の理由・ABI の形) は常に走る。

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

fn wasm_ld() -> String {
    std::env::var("KSN_WASM_LD").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "wasm-ld-20".to_string())
}

/// 実行を伴う検査の道具がそろうか。
fn wasm_ready() -> bool {
    have("node", "--version") && have(&wasm_ld(), "--version") && clang_available()
}

fn both(o: &Output) -> String {
    format!("{}{}", text(&o.stdout), text(&o.stderr))
}

/// `[unsafe] modules` を付けたパッケージを作る (`name` は FQN の先頭。エントリは `<name>.main`)。
fn pkg(name: &str, files: &[(&str, &str)]) -> Tmp {
    let t = Tmp::new(name, "");
    let _ = fs::remove_dir_all(t.0.join("src"));
    let toml = format!("[package]\nname = \"{0}\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"{0}.main\"\n\n[dependencies]\n\n[unsafe]\nmodules = [\"{0}\"]\n", name);
    fs::write(t.0.join("kasane.toml"), toml).unwrap();
    for (rel, body) in files {
        let p = t.0.join("src").join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }
    t
}

/// `ksn build --target wasm32` の出力 (`native executable: <相対パス>`) から `.wasm` の場所を取る。
fn build_wasm(dir: &Path) -> PathBuf {
    let o = run(&["build", "--target", TRIPLE], dir);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "wasm32 build\n{}{}", out, text(&o.stderr));
    let rel = out.lines().find_map(|l| l.strip_prefix("native executable: ")).unwrap_or_else(|| panic!("no executable line:\n{}", out));
    dir.join(rel.trim())
}

/// node で JS の一片を走らせる (標準出力を返す)。`wasm` のパスは環境変数 `KSN_TEST_WASM` で渡す。
fn node_eval(code: &str, wasm: &Path) -> Output {
    Command::new("node")
        .args(["--input-type=module", "-e", code])
        .env("KSN_TEST_WASM", wasm)
        .stdin(Stdio::null())
        .output()
        .expect("spawn node")
}

/// import `link "env:ksn_host_add"` と export `ksn_twice` (`ksn_` の関数) の ABI。JS の import を呼んで結果が返る。
#[test]
fn import_and_export_follow_the_wasm_abi() {
    if !wasm_ready() {
        eprintln!("skipped: node / wasm-ld-20 / clang のどれかがない");
        return;
    }
    let src = "extern w1.host_add(a: u32, b: u32) -> u32 link \"env:ksn_host_add\"\n\nfn w1.ksn_twice(x: u32) -> u32 uses [] panics never\n    unsafe \"w1\" \"call the JS import\" {\n        w1.host_add(x, x)\n    }\nend w1.ksn_twice\n\nfn w1.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"ok\")\n    Ok(())\nend w1.main\n";
    let t = pkg("w1", &[("w1.ksnl", src)]);
    let wasm = build_wasm(&t.0);
    let abi = Command::new("node").arg(root().join("tools/wasm/run.mjs")).arg("--abi").arg(&wasm).output().unwrap();
    let a = text(&abi.stdout);
    assert!(a.contains("\"env.ksn_host_add:function\""), "import が無い: {}", a);
    assert!(a.contains("\"ksn_twice:function\""), "export が無い: {}", a);
    let code = "import fs from 'node:fs';\nconst bytes = fs.readFileSync(process.env.KSN_TEST_WASM);\nlet memory;\nconst env = {\n  ksn_host_add: (a, b) => (a + b) >>> 0,\n  ksn_write: (fd, p, n) => n,\n  ksn_exit: (c) => { throw new Error('exit ' + c); },\n};\nconst { instance } = await WebAssembly.instantiate(bytes, { env });\nconsole.log(instance.exports.ksn_twice(21));\n";
    let o = node_eval(code, &wasm);
    assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    assert_eq!(text(&o.stdout).trim(), "42", "ksn_twice(21) は JS の import を 2 回通って 42");
}

/// `link "env:名前"` だけが JS の import。ほかの `link` (C の関数) は wasm32 では理由つきで止まる。
#[test]
fn c_ffi_is_still_refused_on_wasm32() {
    let src = "extern w2.cbrt(x: f64) -> f64 link \"libm.so.6:cbrt\"\n\nfn w2.main() -> () uses [] panics never\n    unsafe \"w2\" \"call libm\" {\n        core.mem.drop(w2.cbrt(8.0f64))\n    }\nend w2.main\n";
    let t = pkg("w2", &[("w2.ksnl", src)]);
    let o = run(&["build", "--target", TRIPLE], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    let s = both(&o);
    assert!(s.contains("FFI") && s.contains("未対応"), "{}", s);
}
