//! `ksn build --target wasm32-unknown-unknown` (修正プラン 5-3 と 4-2)。
//!
//! - 道具に依らない検査は常に走る: `usize` の幅 (リテラルの範囲 E2026、`ksn check --target`)、対応しない機能の理由 (E9040)、
//!   三つ組の綴り。
//! - 実行を伴う検査は `node`・`wasm-ld-20`・`clang` がそろうときだけ走る (そろわなければ飛ばす。`cross_target.rs` と同じ作法)。
//!   wasm を `tools/wasm/run.mjs` で走らせ、x86_64 のネイティブと標準出力・標準エラー・終了コードが一致することを確かめる。
//!   `linear_ok` は全件を対象にし、ソースでスレッド・ファイル・ソケット・プロセス・環境・実時刻の操作を使うものだけを外す。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;

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

fn runner() -> PathBuf {
    root().join("tools").join("wasm").join("run.mjs")
}

/// `ksn build` の標準出力 `native executable: <相対パス>` から、生成したファイルの場所を取る。
fn built_file(out: &str, dir: &Path) -> PathBuf {
    let rel = out
        .lines()
        .find_map(|l| l.strip_prefix("native executable: "))
        .unwrap_or_else(|| panic!("no `native executable:` line in:\n{}", out));
    dir.join(rel.trim())
}

/// wasm32 にビルドする。失敗なら理由を出して落とす。
fn build_wasm(dir: &Path) -> PathBuf {
    let o = run(&["build", "--target", TRIPLE], dir);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "wasm32 build\n{}{}", out, text(&o.stderr));
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

/// `tools/wasm/run.mjs` で wasm を走らせる。
fn run_wasm(path: &Path) -> Output {
    Command::new("node").arg(runner()).arg(path).stdin(Stdio::null()).output().expect("spawn node")
}

/// 実行ファイルを直接走らせる (標準入力は空)。
fn run_native(path: &Path) -> Output {
    Command::new(path).stdin(Stdio::null()).output().expect("spawn native")
}

/// `dir` を一時の場所へ複製する (`target/` は持ち込まない)。
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
    let d = std::env::temp_dir().join(format!("ksn_wasm32_{}_ex_{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&d);
    copy(&root().join("examples").join(name), &d);
    Tmp(d)
}

/// 出力の両方 (標準出力と標準エラー) を 1 つにする (検査の診断は出す場所が決まっていないので)。
fn both(o: &Output) -> String {
    format!("{}{}", text(&o.stdout), text(&o.stderr))
}

// ---- 道具に依らない検査 ----

/// `usize` の幅はターゲットで決まる (4.5)。wasm32 では 4294967296 が範囲外、ネイティブでは範囲内。
/// 警告ではなくエラー (E2026)、`ksn check` も同じ。
#[test]
fn usize_literal_range_follows_the_target() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let n: usize = 4294967296usize\n    let m: usize = 4294967295usize\n    core.mem.drop(n)\n    core.mem.drop(m)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("wasm_literal", src);
    let host = run(&["check"], &t.0);
    assert!(!both(&host).contains("E2026"), "host: usize は 64 ビットなので 4294967296 は範囲内\n{}", both(&host));
    let w = run(&["check", "--target", TRIPLE], &t.0);
    let wo = both(&w);
    assert!(wo.contains("E2026"), "wasm32: 4294967296 は usize の範囲外のはず\n{}", wo);
    assert!(wo.contains("out of range for `usize` (max 4294967295)"), "{}", wo);
    assert_eq!(wo.matches("E2026").count(), 1, "4294967295 は範囲内なので 1 件だけのはず\n{}", wo);
    assert!(!wo.to_lowercase().contains("warning"), "警告は作らない (4.5)\n{}", wo);
}

/// 配列の長さ・定数の型引数・パターンの `usize` リテラルも対象の幅で検査し、32 ビットで値を失いうる変換
/// (`usize.from_u64`) は wasm32 では `try_from_u64` だけになる (review3 U3・U4)。
#[test]
fn usize_positions_and_conversions_follow_the_target() {
    let src = "fn t.main() -> () uses [] panics never\n    let a: [u8; 4294967296] = [0u8; 4294967296]\n    let n = core.num.usize.from_u64(5u64)\n    core.mem.drop(a)\n    core.mem.drop(n)\nend t.main\n";
    let t = Tmp::new("wasm_usize_positions", src);
    let host = both(&run(&["check"], &t.0));
    assert!(!host.contains("E2026") && !host.contains("E1101"), "host は 64 ビット\n{}", host);
    let w = both(&run(&["check", "--target", TRIPLE], &t.0));
    assert!(w.contains("E2026"), "配列の長さ 4294967296 は wasm32 の usize の範囲外\n{}", w);
    assert!(w.contains("E1101") && w.contains("usize.from_u64"), "wasm32 に usize.from_u64 は無い\n{}", w);
}

/// wasm32 の `usize.BITS` / `usize.MAX` は 32 ビットの値で、ビルドして走らせても同じ (review3 U5)。
#[test]
fn usize_constants_are_32_bit_on_wasm32() {
    if !wasm_ready() {
        eprintln!("skipped: node / wasm-ld-20 / clang のどれかがない");
        return;
    }
    let src = "fn t.main() -> () uses [] panics may\n    core.check.eq(core.num.usize.BITS, 32u32)\n    core.check.eq(core.num.usize.MAX, 4294967295usize)\n    core.check.eq(core.num.isize.MIN, -2147483648isize)\n    let r = usize.try_from_u64(4294967296u64)\n    core.check.eq(r.is_err(), true)\nend t.main\n";
    let t = Tmp::new("wasm_usize_consts", src);
    let w = run_wasm(&build_wasm(&t.0));
    assert_eq!(w.status.code(), Some(0), "{}", both(&w));
}

/// 未知の三つ組は理由つきで失敗する (対応一覧を出す)。
#[test]
fn unknown_target_is_rejected_with_the_supported_list() {
    let t = Tmp::new("wasm_unknown_target", "fn t.main() -> () uses [] panics never\nend t.main\n");
    let o = run(&["check", "--target", "riscv64-linux-gnu"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    let s = both(&o);
    assert!(s.contains("E9040") && s.contains("riscv64-linux-gnu") && s.contains("wasm32-unknown-unknown"), "{}", s);
    let b = run(&["build", "--target", "riscv64-linux-gnu"], &t.0);
    assert_eq!(b.status.code(), Some(1));
    assert!(both(&b).contains("E9040"), "{}", both(&b));
}

/// 対応しない機能は、生成の前に理由つきで止まる (WASM ABI の章)。道具は要らない (clang の前で失敗する)。
#[test]
fn unsupported_std_features_fail_with_a_reason() {
    // スレッド (examples/threads)
    let th = copy_example("threads");
    let o = run(&["build", "--target", TRIPLE], &th.0);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(both(&o).contains("E9040") && both(&o).contains("スレッド"), "{}", both(&o));

    // ファイル (std.fs)
    let fs_src = "fn t.main() -> Result<(), std.io.Error> uses [io.fs, io.console] panics may\n    let e = std.fs.exists(\"x.txt\")\n    core.mem.drop(e)\n    Ok(())\nend t.main\n";
    let f = Tmp::new("wasm_fs", fs_src);
    let o = run(&["build", "--target", TRIPLE], &f.0);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(both(&o).contains("std.fs") && both(&o).contains("未対応"), "{}", both(&o));

    // C の関数を呼ぶ FFI (examples/ffi)
    let ffi = copy_example("ffi");
    let o = run(&["build", "--target", TRIPLE], &ffi.0);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(both(&o).contains("extern") && both(&o).contains("FFI"), "{}", both(&o));

    // 浮動小数の書式化 (examples/tour)
    let tour = copy_example("tour");
    let o = run(&["build", "--target", TRIPLE], &tour.0);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(both(&o).contains("浮動小数の書式化"), "{}", both(&o));
}

// ---- 実行を伴う検査 (道具がそろうときだけ) ----

/// 同じ数の `usize` の結果が、幅で変わる (wasm32 は 32 ビット、ネイティブは 64 ビット)。仕様どおりの違いを確かめる。
#[test]
fn usize_wraps_at_the_target_width() {
    if !wasm_ready() {
        eprintln!("skipped: node / wasm-ld-20 / clang のどれかがない");
        return;
    }
    let src = "fn t.show(v: u64) -> () uses [alloc, io.console] panics never\n    let mut s = alloc.string.String.new(std.alloc.SYSTEM)\n    core.mem.drop(s.push_u64(v))\n    core.mem.drop(std.io.print_line(s.as_str()))\nend t.show\n\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    let z: usize = 0usize\n    let w: usize = z.wrapping_sub(1usize)\n    t.show(u64.from_usize(w))\n    Ok(())\nend t.main\n";
    let t = Tmp::new("wasm_usize_wrap", src);
    let nat = run_native(&build_host(&t.0));
    let wasm = run_wasm(&build_wasm(&t.0));
    assert_eq!(text(&nat.stdout), "18446744073709551615\n");
    assert_eq!(text(&wasm.stdout), "4294967295\n", "wasm32 の usize は 32 ビット");
    assert_eq!(nat.status.code(), Some(0));
    assert_eq!(wasm.status.code(), Some(0));
}

/// 入口の終了の規約 (WASM ABI): panic は 101 と `panic:` の文言、`Err` は 1 と `Error: <表示>`、呼び出しの深さ上限は 102。
#[test]
fn exit_codes_and_messages_match_native() {
    if !wasm_ready() {
        eprintln!("skipped: node / wasm-ld-20 / clang のどれかがない");
        return;
    }
    // panic: 添字が範囲外 (usize の添字と長さの文言)
    let idx = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.mem.drop(std.io.print_line(\"before\"))\n    let v: u8 = t.pick(5usize)\n    core.mem.drop(v)\n    Ok(())\nend t.main\n\nfn t.pick(i: usize) -> u8 uses [] panics may\n    let a: [u8; 2] = [1u8, 2u8]\n    a[i]\nend t.pick\n";
    // Err: 入口が Err を返す
    let err = "fn t.main() -> Result<(), u8> uses [io.console] panics never\n    core.mem.drop(std.io.print_line(\"before\"))\n    Err(7u8)\nend t.main\n";
    // スタック: 呼び出しの深さが上限 (100000) を超える
    let deep = "fn t.down(n: u32) -> u32 uses [] panics never stack 4096\n    if n == 0 {\n        0\n    } else {\n        t.down(n.wrapping_sub(1u32)).wrapping_add(1u32)\n    }\nend t.down\n\nfn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    core.mem.drop(std.io.print_line(\"start\"))\n    core.mem.drop(t.down(200000u32))\n    Ok(())\nend t.main\n";
    let cases = [
        ("wasm_exit_panic", idx, 101, "panic: index out of bounds: the len is 2 but the index is 5\n"),
        ("wasm_exit_err", err, 1, "Error: 7\n"),
        ("wasm_exit_stack", deep, 102, "stack overflow\n"),
    ];
    for (name, src, want, stderr) in cases {
        let t = Tmp::new(name, src);
        let nat = run_native(&build_host(&t.0));
        let wasm = run_wasm(&build_wasm(&t.0));
        assert_eq!(nat.status.code(), Some(want), "{}: native exit\n{}", name, both(&nat));
        assert_eq!(wasm.status.code(), Some(want), "{}: wasm exit\n{}", name, both(&wasm));
        assert_eq!(text(&nat.stdout), text(&wasm.stdout), "{}: stdout", name);
        assert_eq!(text(&nat.stderr), text(&wasm.stderr), "{}: stderr", name);
        assert_eq!(text(&wasm.stderr), stderr, "{}: stderr の文言", name);
    }
}

/// WASM ABI の import と export は仕様どおり: import は `env.ksn_write` と `env.ksn_exit` だけ、export は `memory` と `ksn_main`。
#[test]
fn module_abi_has_only_the_documented_imports_and_exports() {
    if !wasm_ready() {
        eprintln!("skipped: node / wasm-ld-20 / clang のどれかがない");
        return;
    }
    // 出力と panic の両方を持つプログラム: import は 2 つとも使われる。
    let io_panic = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.mem.drop(std.io.print_line(\"before\"))\n    let v: u8 = t.pick(5usize)\n    core.mem.drop(v)\n    Ok(())\nend t.main\n\nfn t.pick(i: usize) -> u8 uses [] panics may\n    let a: [u8; 2] = [1u8, 2u8]\n    a[i]\nend t.pick\n";
    let t = Tmp::new("wasm_abi", io_panic);
    let wasm = build_wasm(&t.0);
    let o = Command::new("node").arg(runner()).arg("--abi").arg(&wasm).output().expect("spawn node");
    assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    assert_eq!(
        text(&o.stdout).trim(),
        r#"{"imports":["env.ksn_write:function","env.ksn_exit:function"],"exports":["memory:memory","ksn_main:function"]}"#
    );
    // 何もしないプログラム: import は無し (使わないものは付けない)。export は同じ。
    let t = Tmp::new("wasm_abi_empty", "fn t.main() -> () uses [] panics never\nend t.main\n");
    let wasm = build_wasm(&t.0);
    let o = Command::new("node").arg(runner()).arg("--abi").arg(&wasm).output().expect("spawn node");
    assert_eq!(text(&o.stdout).trim(), r#"{"imports":[],"exports":["memory:memory","ksn_main:function"]}"#);
}

/// `linear_ok` を wasm32 にビルドし、x86_64 のネイティブと標準出力・標準エラー・終了コードが一致することを確かめる。
/// 対象は、ソースでスレッド・ファイル・ソケット・プロセス・環境・実時刻の操作を使わないもの。外したものは理由つきで出す。
#[test]
fn linear_ok_on_wasm32_matches_native() {
    if !wasm_ready() {
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
    println!("wasm32 linear_ok: {} / {} 一致", ok, total);
    println!("wasm32 linear_ok 対象から外したもの ({}): {}", excluded.len(), excluded.join(", "));
    if !bad.is_empty() {
        println!("wasm32 linear_ok 一致しないもの:\n{}", bad.join("\n"));
    }
    assert!(bad.is_empty(), "wasm32 と x86_64 の結果が違う: {} 件\n{}", bad.len(), bad.join("\n"));
}

/// ソースで、ホストの資源の操作を使うか。使うならその理由 (外す根拠)。`std.net.Error` は型なので使うには数えない。
fn host_resource(src: &str) -> Option<&'static str> {
    for (pat, why) in [
        ("std.thread.", "スレッド"),
        ("std.fs.", "ファイル"),
        ("std.process.", "プロセス"),
        ("std.env.", "環境"),
        ("std.time.", "実時刻"),
    ] {
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
    // 効果の宣言 (`uses [...]`) にホストの資源があるもの。
    for line in src.lines().filter(|l| l.contains("uses [")) {
        let inner = line.split("uses [").nth(1).and_then(|s| s.split(']').next()).unwrap_or("");
        if inner.split(',').map(|e| e.trim()).any(|e| matches!(e, "thread" | "io.fs" | "io.net" | "process" | "env" | "time")) {
            return Some("効果");
        }
    }
    None
}

/// 1 本をビルドして、wasm と x86_64 の結果を比べる。違えば理由を返す。
fn compare_one(name: &str, src: &str) -> Result<(), String> {
    let t = Tmp::new(&format!("wasm_lo_{}", name), src);
    let host = run(&["build"], &t.0);
    let host_out = text(&host.stdout);
    if host.status.code() != Some(0) || host_out.contains("backend skipped") {
        return Err(format!("host の build が失敗: {}", host_out.trim()));
    }
    let nat = run_native(&built_file(&host_out, &t.0));
    let w = run(&["build", "--target", TRIPLE], &t.0);
    let w_out = text(&w.stdout);
    if w.status.code() != Some(0) || w_out.contains("backend skipped") {
        return Err(format!("wasm32 の build が失敗: {}{}", w_out.trim(), text(&w.stderr).trim()));
    }
    let wasm = run_wasm(&built_file(&w_out, &t.0));
    if text(&nat.stdout) != text(&wasm.stdout) {
        return Err("標準出力が違う".into());
    }
    if text(&nat.stderr) != text(&wasm.stderr) {
        return Err(format!("標準エラーが違う: native {:?} / wasm {:?}", text(&nat.stderr), text(&wasm.stderr)));
    }
    if nat.status.code() != wasm.status.code() {
        return Err(format!("終了コードが違う: native {:?} / wasm {:?}", nat.status.code(), wasm.status.code()));
    }
    Ok(())
}
