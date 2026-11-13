//! VM の行表 (設計_KIR.md 7 章 13): panic の位置 (`KSN_PANIC_LOC=1` で出る `in <関数> at <ファイル>:<行>`) が、
//! 木走査のデバッガ (`ksn debug` の `stopped: panic ... in <関数> at <ファイル>:<行>`) と同じになること。

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

static N: AtomicUsize = AtomicUsize::new(0);

fn package(src: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ksn_vm_lines_{}_{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("src")).unwrap();
    fs::write(d.join("kasane.toml"), "[package]\nname = \"pt\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"pt.main\"\n").unwrap();
    fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
    fs::write(d.join("src/pt.ksnl"), src).unwrap();
    d
}

/// 木走査のデバッガが panic で止まった位置 (`in X at F:L` の部分)。
fn tree_location(d: &PathBuf) -> String {
    let mut c = Command::new(exe()).arg("debug").current_dir(d).env("KSN_CACHE_DIR", d.join(".cache")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    c.stdin.take().unwrap().write_all(b"run\nquit\n").unwrap();
    let o = c.wait_with_output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    let line = out.lines().find(|l| l.starts_with("stopped: panic")).unwrap_or_else(|| panic!("debug が panic で止まらない: {}", out));
    line[line.rfind("\" in ").expect("in") + 2..].to_string()
}

/// VM が出した位置 (`in X at F:L`)。VM で走った (退避しない) ことも確かめる。
fn vm_location(d: &PathBuf) -> String {
    let o = Command::new(exe()).args(["run", "--engine=vm"]).current_dir(d).env("KSN_CACHE_DIR", d.join(".cache")).env("KSN_PANIC_LOC", "1").stdin(Stdio::null()).output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(!err.contains("vm engine skipped"), "{}", err);
    assert_eq!(o.status.code(), Some(101), "{}", err);
    err.lines().find(|l| l.starts_with("  in ")).unwrap_or_else(|| panic!("位置が出ない: {}", err)).trim().to_string()
}

fn check(src: &str, expect_line: usize) {
    let d = package(src);
    let (t, v) = (tree_location(&d), vm_location(&d));
    let _ = fs::remove_dir_all(&d);
    assert_eq!(t, v, "木走査と VM の位置が違う");
    assert!(v.ends_with(&format!(":{}", expect_line)), "{} (期待 {} 行)", v, expect_line);
}

#[test]
fn division_by_zero_in_a_callee() {
    check(
        "fn pt.div(a: u64, b: u64) -> u64 uses [] panics may pub
    a / b
end pt.div

fn pt.main() -> Result<(), std.io.Error> uses [] panics may
    let x = pt.div(5u64, 0u64)
    core.check.eq(x, 1u64)
    Ok(())
end pt.main
",
        2,
    );
}

#[test]
fn failed_check_in_main() {
    check(
        "fn pt.main() -> Result<(), std.io.Error> uses [] panics may
    let a = 2u64
    let b = a.wrapping_add(1u64)
    core.check.eq(b, 4u64)
    Ok(())
end pt.main
",
        4,
    );
}

#[test]
fn overflow_after_a_loop_reports_the_failing_line() {
    check(
        "fn pt.grow(n: u8) -> u8 uses [] panics may pub
    let mut acc = 1u8
    let mut i = 0u8
    while i < n {
        acc = acc.wrapping_add(acc)
        i = i.wrapping_add(1u8)
    }
    let big = acc * 200u8
    big
end pt.grow

fn pt.main() -> Result<(), std.io.Error> uses [] panics may
    let r = pt.grow(3u8)
    core.check.eq(r, 0u8)
    Ok(())
end pt.main
",
        8,
    );
}
