//! `ksn build --target wasm32-wasi` (修正プラン 5-3)。WASI preview 1 で、ファイル・引数と環境・実時刻・乱数を使える。
//!
//! - 道具に依らない検査は常に走る: 対応しない機能 (スレッド・ソケット・プロセス) の理由 (E9040)、三つ組の綴り。
//! - 実行を伴う検査は `node`・`wasm-ld-20`・`clang` がそろうときだけ走る (そろわなければ飛ばす。`wasm32.rs` と同じ作法)。
//!   `tools/wasm/run_wasi.mjs` (`node:wasi`) で走らせ、x86_64 のネイティブと標準出力・標準エラー・終了コードが一致することを確かめる。
//!   `linear_ok` は、ソースでスレッド・ソケット・プロセスの操作を使わないものを全件対象にする (ファイル・環境・時刻は WASI で使える)。
//!   examples/ で fs と時刻を使うものは無い (`shortener` はソケットも使う) ので、同じ操作を通す小さなプログラムで確かめる。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;

use common::*;

const TRIPLE: &str = "wasm32-wasi";

fn have(cmd: &str, arg: &str) -> bool {
    Command::new(cmd).arg(arg).stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false)
}

/// リンカ (`KSN_WASM_LD` で上書き。既定は `wasm-ld-20`)。
fn wasm_ld() -> String {
    std::env::var("KSN_WASM_LD").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "wasm-ld-20".to_string())
}

/// 実行を伴う検査の道具 (node・wasm-ld・clang) がそろうか。
fn wasi_ready() -> bool {
    have("node", "--version") && have(&wasm_ld(), "--version") && clang_available()
}

fn runner() -> PathBuf {
    root().join("tools").join("wasm").join("run_wasi.mjs")
}

/// `ksn build` の標準出力 `native executable: <相対パス>` から、生成したファイルの場所を取る。
fn built_file(out: &str, dir: &Path) -> PathBuf {
    let rel = out
        .lines()
        .find_map(|l| l.strip_prefix("native executable: "))
        .unwrap_or_else(|| panic!("no `native executable:` line in:\n{}", out));
    dir.join(rel.trim())
}

/// wasm32-wasi にビルドする。失敗なら理由を出して落とす。
fn build_wasi(dir: &Path) -> PathBuf {
    let o = run(&["build", "--target", TRIPLE], dir);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "wasm32-wasi build\n{}{}", out, text(&o.stderr));
    assert!(!out.contains("backend skipped"), "wasm32-wasi build fell back\n{}", out);
    built_file(&out, dir)
}

/// 今の機械 (x86_64 のネイティブ) にビルドする。
fn build_host(dir: &Path) -> PathBuf {
    let o = run(&["build"], dir);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "host build\n{}{}", out, text(&o.stderr));
    assert!(!out.contains("backend skipped"), "host build fell back to the bundle\n{}", out);
    built_file(&out, dir)
}

/// `tools/wasm/run_wasi.mjs` で wasm を走らせる。`dir` が前開きのディレクトリ (ゲストの `/`)、`args` がプログラムの引数。
fn run_wasi(path: &Path, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new("node");
    c.arg(runner()).arg("--dir").arg(dir).arg(path).args(args).stdin(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().expect("spawn node")
}

/// 実行ファイルを直接走らせる (標準入力は空)。カレントは `dir`、引数と環境は `run_wasi` と同じ。
fn run_native(path: &Path, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(path);
    c.current_dir(dir).args(args).stdin(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().expect("spawn native")
}

/// `dir` を一時の場所へ複製する (`target/` は持ち込まない)。examples の複製に使う。
fn copy_example(name: &str) -> Tmp {
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
    let d = std::env::temp_dir().join(format!("ksn_wasi_{}_ex_{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&d);
    copy(&root().join("examples").join(name), &d);
    Tmp(d)
}

/// 出力の両方 (標準出力と標準エラー) を 1 つにする。
fn both(o: &Output) -> String {
    format!("{}{}", text(&o.stdout), text(&o.stderr))
}

// ---- 道具に依らない検査 ----

/// 対応しない機能は、生成の前に理由つきで止まる (WASM ABI の WASI の項)。道具は要らない (clang の前で失敗する)。
#[test]
fn unsupported_features_fail_with_a_reason_on_wasi() {
    // スレッド (examples/threads)
    let th = copy_example("threads");
    let o = run(&["build", "--target", TRIPLE], &th.0);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(both(&o).contains("E9040") && both(&o).contains("スレッド"), "{}", both(&o));

    // ソケット (examples/http)
    let http = copy_example("http");
    let o = run(&["build", "--target", TRIPLE], &http.0);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(both(&o).contains("E9040") && both(&o).contains("std.net"), "{}", both(&o));

    // 外部コマンド (std.process)
    let proc_src = "fn t.main() -> Result<(), std.io.Error> uses [process] panics may\n    let c = std.process.Command.new(\"true\")\n    let child = c.spawn()?\n    core.mem.drop(child.wait()?)\n    Ok(())\nend t.main\n";
    let p = Tmp::new("wasi_process", proc_src);
    let o = run(&["build", "--target", TRIPLE], &p.0);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(both(&o).contains("E9040") && both(&o).contains("std.process"), "{}", both(&o));
}

/// 三つ組の綴りは仕様どおり。未知の三つ組は対応一覧に wasm32-wasi を出す。
#[test]
fn wasi_is_listed_as_a_supported_target() {
    let t = Tmp::new("wasi_unknown_target", "fn t.main() -> () uses [] panics never\nend t.main\n");
    let o = run(&["check", "--target", "riscv64-linux-gnu"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(both(&o).contains("wasm32-wasi"), "{}", both(&o));
    // wasm32-wasi の usize は 32 ビット (4.5)。wasm32-unknown-unknown と同じ幅。
    let src = "fn t.main() -> () uses [] panics never\n    let n: usize = 4294967296usize\n    core.mem.drop(n)\nend t.main\n";
    let u = Tmp::new("wasi_literal", src);
    let o = run(&["check", "--target", TRIPLE], &u.0);
    assert!(both(&o).contains("E2026"), "{}", both(&o));
}

// ---- 実行を伴う検査 (道具がそろうときだけ) ----

/// WASI の import と export: import は `wasi_snapshot_preview1` だけ (`env` は無い)、export は `memory` `_start` `ksn_main`。
/// 出力と終了を使うので `fd_write` と `proc_exit` は必ず入る。
#[test]
fn module_abi_is_wasi_preview1() {
    if !wasi_ready() {
        eprintln!("skipped: node / wasm-ld-20 / clang のどれかがない");
        return;
    }
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.mem.drop(std.io.print_line(\"hi\"))\n    Ok(())\nend t.main\n";
    let t = Tmp::new("wasi_abi", src);
    let wasm = build_wasi(&t.0);
    let o = Command::new("node").arg(runner()).arg("--abi").arg(&wasm).output().expect("spawn node");
    assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    let out = text(&o.stdout);
    for want in ["wasi_snapshot_preview1.fd_write:function", "wasi_snapshot_preview1.proc_exit:function"] {
        assert!(out.contains(want), "import {} がない: {}", want, out);
    }
    assert!(!out.contains("\"env."), "env の import は無いはず: {}", out);
    for want in ["\"memory:memory\"", "\"_start:function\"", "\"ksn_main:function\""] {
        assert!(out.contains(want), "export {} がない: {}", want, out);
    }
}

/// 入口の終了の規約は素の wasm と同じ (WASM ABI): panic は 101、`Err` は 1、呼び出しの深さ上限は 102。
/// 実行の経路 (`_start` → `main` → `proc_exit`) でも、終了コードと文言がネイティブと一致する。
#[test]
fn exit_codes_and_messages_match_native_on_wasi() {
    if !wasi_ready() {
        eprintln!("skipped: node / wasm-ld-20 / clang のどれかがない");
        return;
    }
    let idx = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.mem.drop(std.io.print_line(\"before\"))\n    let v: u8 = t.pick(5usize)\n    core.mem.drop(v)\n    Ok(())\nend t.main\n\nfn t.pick(i: usize) -> u8 uses [] panics may\n    let a: [u8; 2] = [1u8, 2u8]\n    a[i]\nend t.pick\n";
    let err = "fn t.main() -> Result<(), u8> uses [io.console] panics never\n    core.mem.drop(std.io.print_line(\"before\"))\n    Err(7u8)\nend t.main\n";
    let deep = "fn t.down(n: u32) -> u32 uses [] panics never stack 4096\n    if n == 0 {\n        0\n    } else {\n        t.down(n.wrapping_sub(1u32)).wrapping_add(1u32)\n    }\nend t.down\n\nfn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    core.mem.drop(std.io.print_line(\"start\"))\n    core.mem.drop(t.down(2600u32))\n    Ok(())\nend t.main\n";
    let cases = [
        ("wasi_exit_panic", idx, 101, "panic: index out of bounds: the len is 2 but the index is 5\n"),
        ("wasi_exit_err", err, 1, "Error: 7\n"),
        ("wasi_exit_stack", deep, 102, "stack overflow\n"),
    ];
    for (name, src, want, stderr) in cases {
        let t = Tmp::new(name, src);
        let nat = run_native(&build_host(&t.0), &t.0, &[], &[]);
        let wasm = run_wasi(&build_wasi(&t.0), &t.0, &[], &[]);
        assert_eq!(nat.status.code(), Some(want), "{}: native exit\n{}", name, both(&nat));
        assert_eq!(wasm.status.code(), Some(want), "{}: wasi exit\n{}", name, both(&wasm));
        assert_eq!(text(&nat.stdout), text(&wasm.stdout), "{}: stdout", name);
        assert_eq!(text(&nat.stderr), text(&wasm.stderr), "{}: stderr", name);
        assert_eq!(text(&wasm.stderr), stderr, "{}: stderr の文言", name);
    }
}

/// ファイル・環境・引数・実時刻・乱数・追記・ディレクトリの列挙と削除を、ネイティブと同じ結果にする。
/// 実行は一時のディレクトリの中で、`--dir` で前開きにする (ネイティブはカレントをそこにする)。
#[test]
fn fs_env_time_random_match_native() {
    if !wasi_ready() {
        eprintln!("skipped: node / wasm-ld-20 / clang のどれかがない");
        return;
    }
    // ネイティブと wasi は、それぞれ別の一時のディレクトリで走らせる (プログラムがファイルを作って消す)。
    let args = ["alpha", "beta"];
    let env = [("KSN_WASI_TEST_VAR", "from-env")];
    let nat_t = Tmp::new("wasi_fs_env_nat", FS_PROGRAM);
    let nat = run_native(&build_host(&nat_t.0), &nat_t.0, &args, &env);
    let wasi_t = Tmp::new("wasi_fs_env_wasi", FS_PROGRAM);
    let wasm = run_wasi(&build_wasi(&wasi_t.0), &wasi_t.0, &args, &env);
    assert_eq!(nat.status.code(), Some(0), "native\n{}", both(&nat));
    assert_eq!(wasm.status.code(), Some(0), "wasi\n{}", both(&wasm));
    assert_eq!(text(&nat.stdout), text(&wasm.stdout), "標準出力");
    assert_eq!(text(&nat.stderr), text(&wasm.stderr), "標準エラー");
    let out = text(&wasm.stdout);
    // 期待する形 (ネイティブと同じ値であることも上で確かめている)。
    for want in ["exists: 1", "read: 11", "open missing: kind 1 code 2", "listing bytes: 14", "moved away: 0", "sub gone: 0", "clock ok: 1", "random bytes: 16", "from-env", "argc: 2", "alpha", "appended: 1"] {
        assert!(out.contains(want), "{} がない:\n{}", want, out);
    }
}

/// `linear_ok` を wasm32-wasi にビルドし、x86_64 のネイティブと標準出力・標準エラー・終了コードが一致することを確かめる。
/// 対象は、ソースでスレッド・ソケット・プロセスの操作を使わないもの (ファイル・環境・時刻は WASI で使えるので残す)。
#[test]
fn linear_ok_on_wasi_matches_native() {
    if !wasi_ready() {
        eprintln!("skipped: node / wasm-ld-20 / clang のどれかがない");
        return;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/linear_ok");
    let mut names: Vec<PathBuf> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "ksnl")).collect();
    names.sort();
    let mut targets = Vec::new();
    let mut excluded = Vec::new();
    for p in names {
        let src = fs::read_to_string(&p).unwrap();
        match host_resource(&src) {
            Some(why) => excluded.push(format!("{} ({})", p.file_stem().unwrap().to_string_lossy(), why)),
            None => targets.push((p.file_stem().unwrap().to_string_lossy().to_string(), src)),
        }
    }
    let total = targets.len();
    let queue = Mutex::new(targets);
    let results: Mutex<Vec<(String, Result<(), String>)>> = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let next = queue.lock().unwrap().pop();
                let Some((name, src)) = next else { break };
                let r = compare_one(&name, &src);
                results.lock().unwrap().push((name, r));
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by(|a, b| a.0.cmp(&b.0));
    let bad: Vec<String> = results.iter().filter_map(|(n, r)| r.as_ref().err().map(|e| format!("{}: {}", n, e))).collect();
    let ok = total - bad.len();
    println!("wasm32-wasi linear_ok: {} / {} 一致", ok, total);
    println!("wasm32-wasi linear_ok 対象から外したもの ({}): {}", excluded.len(), excluded.join(", "));
    if !bad.is_empty() {
        println!("wasm32-wasi linear_ok 一致しないもの:\n{}", bad.join("\n"));
    }
    assert!(bad.is_empty(), "wasm32-wasi と x86_64 の結果が違う: {} 件\n{}", bad.len(), bad.join("\n"));
}

/// ソースで、WASI でも対応しないホストの資源 (スレッド・ソケット・プロセス) を使うか。使うならその理由 (外す根拠)。
/// `std.net.Error` は型なので使うには数えない。ファイル・環境・時刻は WASI で使えるので外さない。
fn host_resource(src: &str) -> Option<&'static str> {
    for (pat, why) in [("std.thread.", "スレッド"), ("std.process.", "プロセス")] {
        if src.contains(pat) {
            return Some(why);
        }
    }
    // `std.net.` の後ろが `Error` でなければ、ソケットの操作。
    let mut rest = src;
    while let Some(i) = rest.find("std.net.") {
        let after = &rest[i + "std.net.".len()..];
        if !after.starts_with("Error") {
            return Some("ソケット");
        }
        rest = after;
    }
    // 効果の宣言 (`uses [...]`) にスレッド・ソケット・プロセスがあるもの。
    for line in src.lines().filter(|l| l.contains("uses [")) {
        let inner = line.split("uses [").nth(1).and_then(|s| s.split(']').next()).unwrap_or("");
        if inner.split(',').map(|e| e.trim()).any(|e| matches!(e, "thread" | "io.net" | "process")) {
            return Some("効果");
        }
    }
    None
}

/// 1 本をビルドして、wasi と x86_64 の結果を比べる。違えば理由を返す。
fn compare_one(name: &str, src: &str) -> Result<(), String> {
    let t = Tmp::new(&format!("wasi_lo_{}", name), src);
    let host = run(&["build"], &t.0);
    let host_out = text(&host.stdout);
    if host.status.code() != Some(0) || host_out.contains("backend skipped") {
        return Err(format!("host の build が失敗: {}", host_out.trim()));
    }
    let nat = run_native(&built_file(&host_out, &t.0), &t.0, &[], &[]);
    let w = run(&["build", "--target", TRIPLE], &t.0);
    let w_out = text(&w.stdout);
    if w.status.code() != Some(0) || w_out.contains("backend skipped") {
        return Err(format!("wasi の build が失敗: {}{}", w_out.trim(), text(&w.stderr).trim()));
    }
    let wasm = run_wasi(&built_file(&w_out, &t.0), &t.0, &[], &[]);
    if text(&nat.stdout) != text(&wasm.stdout) {
        return Err("標準出力が違う".into());
    }
    if text(&nat.stderr) != text(&wasm.stderr) {
        return Err(format!("標準エラーが違う: native {:?} / wasi {:?}", text(&nat.stderr), text(&wasm.stderr)));
    }
    if nat.status.code() != wasm.status.code() {
        return Err(format!("終了コードが違う: native {:?} / wasi {:?}", nat.status.code(), wasm.status.code()));
    }
    Ok(())
}

/// fs・環境・引数・時刻・乱数を通すプログラム。`KSN_WASI_TEST_VAR` と引数 `alpha beta` を読む (テストが両方に同じ値を渡す)。
const FS_PROGRAM: &str = r#"fn t.kind_num(k: std.io.ErrorKind) -> i64 uses [] panics never
    match k {
        std.io.ErrorKind.NotFound => 1i64
        std.io.ErrorKind.PermissionDenied => 2i64
        std.io.ErrorKind.AlreadyExists => 3i64
        std.io.ErrorKind.InvalidInput => 4i64
        std.io.ErrorKind.UnexpectedEof => 5i64
        std.io.ErrorKind.BrokenPipe => 6i64
        std.io.ErrorKind.Interrupted => 7i64
        std.io.ErrorKind.OutOfMemory => 8i64
        std.io.ErrorKind.TimedOut => 10i64
        std.io.ErrorKind.Other => 9i64
    }
end t.kind_num

fn t.show(label: &str, n: i64) -> () uses [alloc, io.console] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(label))
    core.mem.drop(s.push_str(": "))
    core.mem.drop(s.push_i64(n))
    core.mem.drop(std.io.print_line(s.as_str()))
end t.show

fn t.show_err(label: &str, e: std.io.Error) -> () uses [alloc, io.console] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(label))
    core.mem.drop(s.push_str(": kind "))
    core.mem.drop(s.push_i64(t.kind_num(e.kind)))
    core.mem.drop(s.push_str(" code "))
    core.mem.drop(s.push_i64(i64.from_i32(e.code)))
    core.mem.drop(std.io.print_line(s.as_str()))
end t.show_err

fn t.flag(label: &str, b: bool) -> () uses [alloc, io.console] panics may
    if b {
        t.show(label, 1i64)
    } else {
        t.show(label, 0i64)
    }
end t.flag

fn t.num(n: usize) -> i64 uses [] panics never
    match i64.try_from_usize(n) {
        Ok(v) => v
        Err(_e) => -1i64
    }
end t.num

fn t.ok_file(r: Result<std.fs.File, std.io.Error>) -> std.fs.File uses [] panics may
    match r {
        Ok(v) => v
        Err(_e) => core.panic.panic("unexpected I/O error")
    }
end t.ok_file

fn t.ok_size(r: Result<usize, std.io.Error>) -> usize uses [] panics may
    match r {
        Ok(v) => v
        Err(_e) => core.panic.panic("unexpected I/O error")
    }
end t.ok_size

fn t.ok_unit(r: Result<(), std.io.Error>) -> () uses [] panics may
    match r {
        Ok(_u) => {
        }
        Err(_e) => core.panic.panic("unexpected I/O error")
    }
end t.ok_unit

fn t.open_missing() -> () uses [alloc, io.console, io.fs] panics may
    match std.fs.open("no_such_file.txt") {
        Ok(h) => t.ok_unit(h.close())
        Err(e) => t.show_err("open missing", e)
    }
end t.open_missing

fn t.write_file(path: &str, body: &[u8]) -> () uses [io.fs] panics may
    let mut f = t.ok_file(std.fs.create(path))
    core.mem.drop(t.ok_size(f.write(body)))
    t.ok_unit(f.sync())
    t.ok_unit(f.close())
end t.write_file

fn t.main() -> Result<(), std.io.Error> uses [alloc, env, io.console, io.fs, rand, time] panics may
    match std.env.arg(0usize) {
        Some(a) => std.io.print_line(a)?
        None => std.io.print_line("no arg0")?
    }
    match std.env.var("KSN_WASI_TEST_VAR") {
        Some(v) => std.io.print_line(v)?
        None => std.io.print_line("unset")?
    }
    t.show("argc", t.num(std.env.arg_count()))
    t.write_file("out.txt", b"hello, file")
    t.flag("exists", std.fs.exists("out.txt"))
    let mut g = t.ok_file(std.fs.open("out.txt"))
    let mut rest = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let total = t.ok_size(std.fs.read_to_end(&mut g, &mut rest))
    t.show("read", t.num(total))
    t.ok_unit(g.close())
    t.open_missing()
    t.ok_unit(std.fs.create_dir("sub"))
    t.write_file("sub/a.txt", b"a")
    t.write_file("sub/b.txt", b"bb")
    let mut listing = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let listed = t.ok_size(std.fs.read_dir_raw("sub", &mut listing))
    t.show("listing bytes", t.num(listed))
    t.ok_unit(std.fs.rename("out.txt", "moved.txt"))
    t.flag("moved away", std.fs.exists("out.txt"))
    t.ok_unit(std.fs.remove_file("moved.txt"))
    t.ok_unit(std.fs.remove_file("sub/a.txt"))
    t.ok_unit(std.fs.remove_file("sub/b.txt"))
    t.ok_unit(std.fs.remove_dir("sub"))
    t.flag("sub gone", std.fs.exists("sub"))
    let mut a = t.ok_file(std.fs.open_append("log.txt"))
    t.ok_unit(a.close())
    t.flag("appended", std.fs.exists("log.txt"))
    t.flag("clock ok", std.time.system_nanos() > 1000000000i64)
    let mut buf = [0u8; 16]
    t.ok_unit(std.rand.fill_secure(&mut buf))
    t.show("random bytes", 16i64)
    Ok(())
end t.main
"#;
