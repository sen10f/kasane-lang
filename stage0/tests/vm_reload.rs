//! `ksn run --watch --engine=vm` のホットリロード (設計_KIR.md 4.4 / 7 章 9)。ルートの `tests/run_tests.py` の木走査版と同じ作法:
//! 走らせながらソースを書き換え、標準出力・標準エラーで反映を確かめる。VM で実際に走る (木走査へ退避しない) ことも確かめる。

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

// 壁時計に依存しない: `spin` は KIR に下ろせて VM で実際に走り、60 回繰り返すので
// リロード（ウォッチャは 200 ms 毎にpoll、書き換え後すぐに着く）が着くまで必ず走り続ける。
// 書き換え後は新版（v2）が出るまで終了しない（本テストでは最初の v1 を待ってから書き換える）。
const HR: &str = "fn hr.main() -> Result<(), std.io.Error> uses [io.console] panics never
    let mut i = 0u64
    while i < 60u64 {
        let r = hr.msg()
        core.mem.drop(r)
        let s = hr.spin(7_000_000u64)
        core.mem.drop(s)
        i = i.wrapping_add(1u64)
    }
    Ok(())
end hr.main

fn hr.msg() -> Result<(), std.io.Error> uses [io.console] panics never
    core.mem.drop(std.io.print_line(\"v1\"))
    Ok(())
end hr.msg

fn hr.spin(n: u64) -> u64 uses [] panics never
    let mut a = 0u64
    let mut k = 0u64
    while k < n {
        a = a.wrapping_add(k.wrapping_mul(3u64))
        k = k.wrapping_add(1u64)
    }
    a.wrapping_add(2u64)
end hr.spin
";

static N: AtomicUsize = AtomicUsize::new(0);

fn package(src: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ksn_vm_reload_{}_{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("src")).unwrap();
    fs::write(d.join("kasane.toml"), "[package]\nname = \"hr\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"hr.main\"\n").unwrap();
    fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
    fs::write(d.join("src/hr.ksnl"), src).unwrap();
    d
}

struct Out {
    code: Option<i32>,
    out: String,
    err: String,
}

// `ksn run --watch --engine=vm` を走らせ、`ksn` が "hot reload: watching" を stderr に出すのを
// 待ってからソースを `edited` に書き換える。この信号は検査後に実行直前に出されるので、
// 書き換え時に旧版（v1）がまだ走っておらず、最初の hr.msg()（v1）がウォッチャの最初の
// poll（200 ms 毎）より前に必ず実行される。VM の標準出力はバッファされ、終了時にまとめて
// フラッシュされるので v1 を増分観測できず、この readiness 信号に依存する。
// 書き換え後はウォッチャが検出してリロードし、新版（v2）が走る。子プロセスは 60 回反復して
// 自然に終了するので、終了を待つ。
fn run_watch(d: &Path, edited: &str) -> Out {
    let cache = d.join(".cache");
    let mut child = Command::new(exe())
        .args(["run", "--watch", "--engine=vm"])
        .current_dir(d)
        .env("KSN_CACHE_DIR", &cache)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ksn");
    let src = d.join("src/hr.ksnl");
    let edited = edited.to_string();
    let out = Arc::new(Mutex::new(String::new()));
    let err = Arc::new(Mutex::new(String::new()));
    let out_c = out.clone();
    let err_c = err.clone();
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");
    let t_out = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => { out_c.lock().unwrap().push_str(&line); }
                Err(_) => break,
            }
        }
    });
    let t_err = std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => { err_c.lock().unwrap().push_str(&line); }
                Err(_) => break,
            }
        }
    });
    // ksn が "hot reload: watching" を出すまで待つ（検査後、実行直前。旧版がまだ走っていない）。
    let start = Instant::now();
    loop {
        if err.lock().unwrap().contains("hot reload: watching") {
            break;
        }
        if start.elapsed() >= Duration::from_secs(60) {
            panic!("timed out waiting for hot reload to start watching");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // ソースを書き換える。ウォッチャの最初の poll（200 ms 毎）より前に旧版の hr.msg() が
    // 少なくとも 1 回 v1 を出すので、v1 >= 1 が保証される。
    fs::write(&src, &edited).expect("write edited source");
    // リロード後に新版が走り、60 回反復して終了するのを待つ（正常なら ~10 s で自然終了）。
    let status = child.wait().expect("wait");
    // パイプを空にする（子終了で EOF になる）。
    let _ = t_out.join();
    let _ = t_err.join();
    let out_s = out.lock().unwrap().clone();
    let err_s = err.lock().unwrap().clone();
    Out { code: status.code(), out: out_s, err: err_s }
}

fn count(s: &str, line: &str) -> usize {
    s.lines().filter(|l| *l == line).count()
}

#[test]
fn vm_hot_reload_applies_a_body_change_while_running() {
    let d = package(HR);
    let r = run_watch(&d, &HR.replace("v1", "v2"));
    let _ = fs::remove_dir_all(&d);
    assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
    assert!(!r.err.contains("vm engine skipped"), "VM で走るはず: {}", r.err);
    assert!(r.err.contains("hot reload: function bodies updated"), "{}", r.err);
    // 書き換えの前は v1、後は v2。固定回数ではなく、両版が少なくとも 1 回出れば十分。
    let (v1, v2) = (count(&r.out, "v1"), count(&r.out, "v2"));
    assert!(v1 >= 1 && v2 >= 1, "v1={} v2={}\n{}", v1, v2, r.out);
    // v1 の後に v2 が来る (戻らない)。
    let first_v2 = r.out.lines().position(|l| l == "v2").unwrap();
    assert!(r.out.lines().skip(first_v2).all(|l| l == "v2"), "{}", r.out);
}

#[test]
fn vm_hot_reload_applies_an_added_function() {
    let d = package(HR);
    // 関数の追加を即時に反映する: 新しい関数 `hr.extra` を、書き換えた `hr.msg` が呼ぶ。
    let edited = format!(
        "{}\nfn hr.extra() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"v2\")\nend hr.extra\n",
        HR.replace("std.io.print_line(\"v1\")", "hr.extra()")
    );
    let r = run_watch(&d, &edited);
    let _ = fs::remove_dir_all(&d);
    assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
    assert!(!r.err.contains("vm engine skipped"), "{}", r.err);
    assert!(!r.err.contains("restart required"), "{}", r.err);
    assert!(r.err.contains("hot reload: function bodies updated"), "{}", r.err);
    let (v1, v2) = (count(&r.out, "v1"), count(&r.out, "v2"));
    assert!(v1 >= 1 && v2 >= 1, "v1={} v2={}\n{}", v1, v2, r.out);
}

#[test]
fn vm_hot_reload_asks_for_restart_on_a_header_change() {
    let d = package(HR);
    // 引数の追加などヘッダ変更は再起動を促すだけで反映しない。
    let edited = HR.replace("v1", "v2")
        .replace("fn hr.msg() -> Result<(), std.io.Error> uses [io.console] panics never",
                 "fn hr.msg(x: u64) -> Result<(), std.io.Error> uses [io.console] panics never")
        .replace("let r = hr.msg()", "let r = hr.msg(0u64)");
    let r = run_watch(&d, &edited);
    let _ = fs::remove_dir_all(&d);
    assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
    assert!(!r.err.contains("vm engine skipped"), "VM で走るはず: {}", r.err);
    assert!(r.err.contains("hot reload: header/type change, restart required"), "{}", r.err);
    // 旧版のまま続行するので v2 は出ず、v1 が少なくとも 1 回出る。
    assert_eq!(count(&r.out, "v2"), 0, "{}", r.out);
    assert!(count(&r.out, "v1") >= 1, "{}", r.out);
}

#[test]
fn vm_hot_reload_keeps_old_code_when_the_new_body_cannot_be_lowered() {
    let d = package(HR);
    // `std.thread.sleep` は KIR に下ろせない (段階 1 の対象外)。VM は古いコードのまま続ける。
    let edited = HR.replace("std.io.print_line(\"v1\")", "std.thread.sleep(1u64)\n    std.io.print_line(\"v2\")");
    let r = run_watch(&d, &edited);
    let _ = fs::remove_dir_all(&d);
    assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
    assert!(r.err.contains("(kept old code)"), "{}", r.err);
    // 旧版のままなので v2 は出ず、v1 が少なくとも 1 回出る。
    assert_eq!(count(&r.out, "v2"), 0, "{}", r.out);
    assert!(count(&r.out, "v1") >= 1, "{}", r.out);
}
