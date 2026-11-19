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

/// `ksn build` の標準出力 `native executable: <相対パス>` から、生成したファイルの場所を取る。
fn built_file(out: &str, dir: &Path) -> PathBuf {
    let rel = out.lines().find_map(|l| l.strip_prefix("native executable: ")).unwrap_or_else(|| panic!("no `native executable:` line in:\n{}", out));
    dir.join(rel.trim())
}

/// 今の機械 (x86_64 のネイティブ) にビルドする。
fn build_host(dir: &Path) -> PathBuf {
    let o = run(&["build"], dir);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "host build\n{}{}", out, text(&o.stderr));
    assert!(!out.contains("backend skipped"), "host build fell back to the bundle\n{}", out);
    built_file(&out, dir)
}

/// 実行ファイルを直接走らせる (標準入力は空)。
fn run_native(path: &Path) -> Output {
    Command::new(path).stdin(Stdio::null()).output().expect("spawn native")
}

/// `tools/wasm/run.mjs` で wasm を走らせる。
fn run_wasm(path: &Path) -> Output {
    Command::new("node").arg(root().join("tools/wasm/run.mjs")).arg(path).stdin(Stdio::null()).output().expect("spawn node")
}

/// `[unsafe] modules` を付けたパッケージを作る (`name` は FQN の先頭。エントリは `<name>.main`)。
fn pkg(name: &str, files: &[(&str, &str)]) -> Tmp {
    let t = Tmp::new(name, "");
    let _ = fs::remove_dir_all(t.0.join("src"));
    let toml = format!("[package]\nname = \"{0}\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"{0}.main\"\n\n[unsafe]\nmodules = [\"{0}\"]\n\n[dependencies]\n", name);
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
    let src = "extern w1.host_add(a: u32, b: u32) -> u32 link \"env:ksn_host_add\"\n\nfn w1.ksn_twice(x: u32) -> u32 uses [] panics never\n    unsafe \"w1\" \"call the JS import\" {\n        w1.host_add(x, x)\n    }\nend w1.ksn_twice\n\nfn w1.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    core.mem.drop(std.io.print_line(\"ok\"))\n    Ok(())\nend w1.main\n";
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

/// `ksn bindgen` の生成物: Kasane は `check` を通り、生成したファイルは `fmt --check` の指摘がなく、JS は構文が正しい。
/// `extern` の綴りは `link "env:<接頭辞><操作名>"`、安全な関数は `unsafe` の中で `raw_` を呼ぶ。
#[test]
fn bindgen_output_checks_formats_and_parses() {
    let caller = "import bgen.dom\n\nfn bgen.main() -> Result<(), std.io.Error> uses [] panics never\n    let root = dom.body()\n    let li = dom.create(\"li\")\n    dom.set_text(li, \"hi\")\n    dom.append(root, li)\n    Ok(())\nend bgen.main\n";
    let t = pkg("bgen", &[("bgen.ksnl", caller)]);
    fs::write(t.0.join("dom.bindgen"), "# テスト\nmodule bgen.dom\nksnl src/bgen/dom.ksnl\njs web/dom.mjs\nop body\nop create\nop set_text\nop append\nop input_value\nop fetch\nop fetch_body\n").unwrap();
    let o = run(&["bindgen", "dom.bindgen"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    let k = fs::read_to_string(t.0.join("src/bgen/dom.ksnl")).unwrap();
    assert!(k.contains("extern bgen.dom.raw_create(tag_ptr: *const u8, tag_len: usize) -> u32 link \"env:ksn_dom_create\""), "{}", k);
    assert!(k.contains("fn bgen.dom.input_value(node: u32, buf: &mut [u8]) -> i32 uses [] panics never pub"), "{}", k);
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "check\n{}", both(&c));
    let f = run(&["fmt", "--check"], &t.0);
    assert!(!both(&f).contains("dom.ksnl"), "生成物は fmt の指摘なし\n{}", both(&f));
    let js = t.0.join("web/dom.mjs");
    if have("node", "--version") {
        let s = Command::new("node").args(["--check"]).arg(&js).output().expect("spawn node");
        assert_eq!(s.status.code(), Some(0), "{}", both(&s));
    } else {
        eprintln!("skipped: node の構文検査 (node がない)");
    }
    let mjs = fs::read_to_string(&js).unwrap();
    assert!(mjs.contains("ksn_dom_fetch: (p0, n0, p1, n1, p2, n2, a3) => ops.fetch("), "{}", mjs);
    assert!(mjs.contains("ksn_dom_input_value: (a0, p1, n1) => ops.input_value(a0, p1, n1),"), "{}", mjs);
}

/// 定義の誤りは理由つきで止まる (終了コード 2、何も書かない)。
#[test]
fn bindgen_rejects_bad_definitions() {
    let cases = [
        ("module bgbad.dom\nksnl a.ksnl\njs a.mjs\nop nope\n", "未知の操作"),
        ("module BAD\nksnl a.ksnl\njs a.mjs\nop body\n", "module は"),
        ("module bgbad.dom\nksnl a.ksnl\njs a.mjs\n", "op が一つもない"),
        ("module bgbad.dom\njs a.mjs\nop body\n", "ksnl が要る"),
        ("module bgbad.dom\nksnl a.ksnl\njs a.mjs\nop body\nop body\n", "二度ある"),
        ("module bgbad.dom\nksnl a.ksnl\njs a.mjs\nprefix dom\nop body\n", "prefix"),
    ];
    for (i, (def, want)) in cases.iter().enumerate() {
        let t = pkg(&format!("bgbad{}", i), &[]);
        fs::write(t.0.join("d.bindgen"), def).unwrap();
        let o = run(&["bindgen", "d.bindgen"], &t.0);
        assert_eq!(o.status.code(), Some(2), "{}: {}", def, both(&o));
        assert!(both(&o).contains(want), "{}: {}", def, both(&o));
        assert!(!t.0.join("a.ksnl").exists() && !t.0.join("a.mjs").exists(), "失敗したら何も書かない");
    }
}

// ---- TODO の SPA (修正プラン 5-3 段 3・4) ----

/// 生成物を含めて、パッケージ `rel` (リポジトリ直下からの相対) を一時の場所へ複製する (`target/` は持ち込まない)。
fn copy_pkg(rel: &str, name: &str) -> Tmp {
    fn copy(src: &Path, dst: &Path) {
        fs::create_dir_all(dst).unwrap();
        for e in fs::read_dir(src).unwrap().flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n == "target" || n == ".git" {
                continue;
            }
            let (s, d) = (e.path(), dst.join(&n));
            if s.is_dir() {
                copy(&s, &d);
            } else {
                fs::copy(&s, &d).unwrap();
            }
        }
    }
    let d = std::env::temp_dir().join(format!("ksn_wasm_spa_{}_{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&d);
    copy(&root().join(rel), &d);
    Tmp(d)
}

/// 子プロセスを、テストが途中で落ちても必ず止める。
struct Kill(std::process::Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// 使えそうな待ち受けの番号 (取って、すぐ離す)。
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// 接続できるまで待つ (最大 10 秒)。
fn wait_port(port: u16) -> bool {
    for _ in 0..200 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

/// `apps/todo` のサーバを、ホストの実行ファイルとして作って起動する (`count` 件の要求で終わる。終わらなくても `Kill` が止める)。
fn start_todo_server(dir: &Path, port: u16) -> Kill {
    let o = run(&["build"], dir);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "todo server build\n{}{}", out, text(&o.stderr));
    let exe = built_file(&out, dir);
    let child = Command::new(&exe)
        .arg(format!("127.0.0.1:{}", port))
        .arg("300")
        .arg(dir.join("todos.json"))
        .arg("0")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn todo server");
    Kill(child)
}

/// `apps/todo_spa` を wasm32 にビルドし、`web/` と合わせて作る。戻りは (作業場所, `.wasm` の場所)。
fn build_spa() -> (Tmp, PathBuf) {
    let app = copy_pkg("apps/todo_spa", "spa");
    let o = run(&["build", "--target", TRIPLE, "--release"], &app.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "todo_spa wasm32 build\n{}{}", out, text(&o.stderr));
    let wasm = built_file(&out, &app.0);
    (app, wasm)
}

/// 生成物 (`src/todo_spa/dom.ksnl`、`web/dom.mjs`) は定義 (`bindgen/dom.bindgen`) から作り直したものと同じ。
/// 生成器を変えたら、生成物も作り直してコミットする。
#[test]
fn todo_spa_generated_files_match_the_definition() {
    let t = copy_pkg("apps/todo_spa", "gen");
    fs::remove_file(t.0.join("src/todo_spa/dom.ksnl")).unwrap();
    fs::remove_file(t.0.join("src/todo_spa/dom.ksnh")).unwrap();
    fs::remove_file(t.0.join("web/dom.mjs")).unwrap();
    let o = run(&["bindgen", "bindgen/dom.bindgen"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    for rel in ["src/todo_spa/dom.ksnh", "src/todo_spa/dom.ksnl", "web/dom.mjs"] {
        let now = fs::read_to_string(t.0.join(rel)).unwrap();
        let committed = fs::read_to_string(root().join("apps/todo_spa").join(rel)).unwrap();
        assert_eq!(now, committed, "{} が定義と違う (ksn bindgen で作り直す)", rel);
    }
}

/// 128 ビットの組み込み (`__multi3` など、`rt.c`) と `ksn_parse_int` が、x86_64 のネイティブと同じ値と範囲の誤りを出す。
#[test]
fn wasm32_parse_matches_native_at_the_limits() {
    if !wasm_ready() {
        eprintln!("skipped: node / wasm-ld-20 / clang のどれかがない");
        return;
    }
    let src = "fn t.line(text: &str) -> () uses [alloc, io.console] panics never\n    let mut s = alloc.string.String.new(std.alloc.SYSTEM)\n    match u64.parse(text) {\n        Ok(v) => core.mem.drop(s.push_u64(v))\n        Err(e) => {\n            core.mem.drop(e)\n            core.mem.drop(s.push_str(\"err\"))\n        }\n    }\n    core.mem.drop(std.io.print_line(s.as_str()))\nend t.line\n\nfn t.signed(text: &str) -> () uses [alloc, io.console] panics never\n    let mut s = alloc.string.String.new(std.alloc.SYSTEM)\n    match i64.parse(text) {\n        Ok(v) => core.mem.drop(s.push_i64(v))\n        Err(e) => {\n            core.mem.drop(e)\n            core.mem.drop(s.push_str(\"err\"))\n        }\n    }\n    core.mem.drop(std.io.print_line(s.as_str()))\nend t.signed\n\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never\n    t.line(\"0\")\n    t.line(\"18446744073709551615\")\n    t.line(\"18446744073709551616\")\n    t.line(\"123456789012345678\")\n    t.line(\"x\")\n    t.signed(\"-9223372036854775808\")\n    t.signed(\"9223372036854775808\")\n    t.signed(\"-9223372036854775807\")\n    Ok(())\nend t.main\n";
    let t = Tmp::new("wasm_parse_limits", src);
    let nat = run_native(&build_host(&t.0));
    let wasm = run_wasm(&build_wasm(&t.0));
    let want = "0\n18446744073709551615\nerr\n123456789012345678\nerr\n-9223372036854775808\nerr\n-9223372036854775807\n";
    assert_eq!(text(&nat.stdout), want, "native");
    assert_eq!(text(&wasm.stdout), want, "wasm32 (rt.c の 128 ビットの組み込み)");
    assert_eq!(wasm.status.code(), Some(0));
}

/// TODO の SPA を node の上で動かす (偽の DOM)。サーバ (`apps/todo` の実物) を起動して、
/// 追加・一覧・完了の切り替え・削除・localStorage・読み直しを確かめる。サイズは `.wasm` と JS・HTML の合計で 500 KB 以下。
#[test]
fn todo_spa_scenario_runs_against_the_todo_server() {
    if !wasm_ready() {
        eprintln!("skipped: node / wasm-ld-20 / clang のどれかがない");
        return;
    }
    let (app, wasm) = build_spa();
    let total: u64 = [wasm.clone(), app.0.join("web/dom.mjs"), app.0.join("web/boot.mjs"), app.0.join("web/index.html")]
        .iter()
        .map(|p| fs::metadata(p).unwrap().len())
        .sum();
    assert!(total <= 500 * 1024, "SPA の大きさ {} バイト (500 KB 以下)", total);

    let srv = copy_pkg("apps/todo", "srv");
    let port = free_port();
    let _server = start_todo_server(&srv.0, port);
    assert!(wait_port(port), "todo サーバが待ち受けない (port {})", port);

    let o = Command::new("node")
        .arg(root().join("tools/wasm/spa_check.mjs"))
        .arg(app.0.join("web"))
        .arg(&wasm)
        .arg(format!("http://127.0.0.1:{}", port))
        .stdin(Stdio::null())
        .output()
        .expect("spawn node");
    assert_eq!(o.status.code(), Some(0), "spa_check\n{}", both(&o));
    assert!(text(&o.stdout).contains("spa_check: ok"), "{}", both(&o));
    println!("todo_spa: {} バイト (500 KB 以下)", total);
}
