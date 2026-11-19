//! Windows ターゲット (`ksn build --target x86_64-windows-gnu`、設計_ネイティブUI.md N1w)。
//!
//! `wine` と mingw-w64 の sysroot (`/usr/x86_64-w64-mingw32`、`KSN_SYSROOT` で上書き) がそろうときだけ、Windows の実行ファイルを wine で動かし、
//! x86_64 Linux のネイティブと標準出力・終了コードが一致することを確かめる。そろわなければ黙って飛ばす。
//! wine が動かせないもの (本物の Windows でないと確かめられないもの) と本当の誤りは `実装計画.md` の N1w の節で分けて書く。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::*;

const TRIPLE: &str = "x86_64-windows-gnu";
const DEFAULT_SYSROOT: &str = "/usr/x86_64-w64-mingw32";

/// 既定の sysroot。`KSN_SYSROOT` で上書きできる (`ksn build` と同じ)。
fn sysroot() -> String {
    std::env::var("KSN_SYSROOT").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| DEFAULT_SYSROOT.to_string())
}

fn have(cmd: &str) -> bool {
    Command::new(cmd).arg("--version").stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false)
}

/// 実行を伴うテストを走らせられるか (道具と sysroot がそろっているか)。そろわなければ `false` (黙って飛ばす)。
fn cross_ready() -> bool {
    wine().is_some() && Path::new(&sysroot()).join("lib").join("crt2.o").exists() && cfg!(all(target_arch = "x86_64", target_os = "linux"))
}

/// `wine64` の場所 (PATH、なければ Debian / Ubuntu の `/usr/lib/wine/wine64`)。
fn wine() -> Option<String> {
    ["wine64", "/usr/lib/wine/wine64", "wine"].iter().find(|c| have_wine(c)).map(|s| s.to_string())
}

fn have_wine(cmd: &str) -> bool {
    Command::new(cmd).arg("--version").env("WINEDEBUG", "-all").stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false)
}

/// wine で `exe` を動かす。
fn run_wine(exe: &Path, dir: &Path) -> Output {
    // prefix は環境の `WINEPREFIX` か既定 (`~/.wine`)。初回だけ作る。
    static PREFIX: std::sync::Once = std::sync::Once::new();
    PREFIX.call_once(|| {
        let _ = Command::new(wine().unwrap()).args(["wineboot", "-u"]).env("WINEDEBUG", "-all").env("DISPLAY", "").stdin(Stdio::null()).output();
    });
    Command::new(wine().unwrap())
        .arg(exe)
        .current_dir(dir)
        .env("WINEDEBUG", "-all")
        .env("DISPLAY", "")
        .stdin(Stdio::null())
        .output()
        .expect("run under wine")
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

/// `dir` のパッケージを x86_64 のネイティブ (既定の `ksn build`) と windows (`--target`) にビルドし、
/// Windows は wine で動かして、標準出力と終了コードが x86_64 のネイティブと一致することを確かめる。
fn same_on_windows(dir: &Path, what: &str) {
    let h = run(&["build"], dir);
    let hs = text(&h.stdout);
    assert_eq!(h.status.code(), Some(0), "{}: host build\n{}{}", what, hs, text(&h.stderr));
    assert!(!hs.contains("backend skipped"), "{}: host build fell back to the bundle\n{}", what, hs);

    let a = run(&["build", "--target", TRIPLE], dir);
    let asout = text(&a.stdout);
    assert_eq!(a.status.code(), Some(0), "{}: windows build\n{}{}", what, asout, text(&a.stderr));
    assert!(!asout.contains("backend skipped"), "{}: windows build fell back\n{}", what, asout);

    let hexe = native_exe(&hs, dir);
    let aexe = native_exe(&asout, dir);
    assert!(aexe.starts_with(dir.join("target").join(TRIPLE)), "{}: output not under target/{}: {}", what, TRIPLE, aexe.display());

    let hr = Command::new(&hexe).current_dir(dir).stdin(Stdio::null()).output().expect("run x86_64 native");
    let ar = run_wine(&aexe, dir);
    assert_eq!(text(&ar.stdout), text(&hr.stdout), "{}: stdout (windows under wine / x86_64)", what);
    assert_eq!(ar.status.code(), hr.status.code(), "{}: exit code (windows under wine / x86_64)", what);
}

// ---- 実行を伴う一致の確認 (道具がそろうときだけ) ----

#[test]
fn examples_match_x86_64_native_on_windows() {
    if !cross_ready() {
        return;
    }
    // `ffi` は libc.so.6 を引く例なので Windows では動かない。
    for name in ["demo", "tour", "threads", "json", "native", "features"] {
        let t = copy_example(name);
        same_on_windows(&t.0, &format!("examples/{}", name));
    }
}

#[test]
fn linear_ok_matches_x86_64_native_on_windows() {
    if !cross_ready() {
        return;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/linear_ok");
    let mut names: Vec<PathBuf> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().map_or(false, |x| x == "ksnl")).collect();
    names.sort();
    assert!(!names.is_empty(), "linear_ok cases missing");
    par_map(&names, |p| {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("win_lin_{}", stem), &fs::read_to_string(p).unwrap());
        same_on_windows(&t.0, &format!("linear_ok/{}", stem));
    });
}

#[test]
fn windows_exe_is_a_pe_and_needs_no_winpthread_dll() {
    if !cross_ready() {
        return;
    }
    let t = copy_example("demo");
    let o = run(&["build", "--target", TRIPLE], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let exe = native_exe(&text(&o.stdout), &t.0);
    assert!(exe.to_string_lossy().ends_with("demo.exe"), "{}", exe.display());
    let b = fs::read(&exe).unwrap();
    assert_eq!(&b[..2], b"MZ");
    assert!(!String::from_utf8_lossy(&b).to_lowercase().contains("libwinpthread-1.dll"), "winpthreads must be linked statically");
}
