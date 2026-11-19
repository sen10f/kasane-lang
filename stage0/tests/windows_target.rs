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
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("ksn_win_{}_{}_ex_{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst), name));
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
    for name in ["demo", "tour", "threads", "json", "native", "features", "http", "shortener"] {
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

#[test]
fn large_struct_by_value_extern_compiles_on_windows() {
    // Win64 は 8 バイト超の構造体を隠しポインタ (引数は呼び出し側の複製・戻り値は sret) で渡す (N1w3)。
    // 拒まずにビルドできる。msvcrt.dll に無い `big_id` を動的に引こうとして wine では 101 で落ちる
    // (読み込みの経路は動く)。
    let src = "type t.Big layout c derive [Clone, Copy]
    a: i32
    b: i32
    c: i32
end t.Big

extern t.big_id(a: t.Big) -> t.Big link \"msvcrt.dll:big_id\"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let s = unsafe \"ffi\" \"big\" {
        t.big_id(t.Big { a: 1i32, b: 2i32, c: 3i32 })
    }
    core.check.eq(s.a, 1i32)
    Ok(())
end t.main
";
    let t = Tmp::new("win_big_struct", src);
    fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t\"]\n\n[dependencies]", 1)).unwrap();
    let o = run(&["build", "--target", TRIPLE], &t.0);
    let out = text(&o.stdout);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, err);
    assert!(!err.contains("E9040"), "{}", err);
    let exe = native_exe(&out, &t.0);
    assert!(exe.to_string_lossy().ends_with("t.exe"), "{}", exe.display());
    if !cross_ready() {
        return;
    }
    let r = run_wine(&exe, &t.0);
    assert_eq!(r.status.code(), Some(101));
    assert!(text(&r.stderr).contains("big_id"), "{}", text(&r.stderr));
}

#[test]
fn dynamic_load_of_a_dll_symbol_works_on_windows() {
    if !cross_ready() {
        return;
    }
    // `link "lib.dll:名前"` は LoadLibraryA / GetProcAddress で引く。msvcrt の `abs` を呼ぶ。
    let src = "extern t.c_abs(x: i32) -> i32 link \"msvcrt.dll:abs\"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let v = unsafe \"ffi\" \"abs\" {
        t.c_abs(-7i32)
    }
    core.check.eq(v, 7i32)
    std.io.print_line(\"abs ok\")?
    Ok(())
end t.main
";
    let t = Tmp::new("win_dll_abs", src);
    fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t\"]\n\n[dependencies]", 1)).unwrap();
    let o = run(&["build", "--target", TRIPLE], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    let exe = native_exe(&out, &t.0);
    let r = run_wine(&exe, &t.0);
    assert_eq!(text(&r.stdout), "abs ok\n", "{}", text(&r.stderr));
    assert_eq!(r.status.code(), Some(0));
}

/// std.process (CreateProcessW): Kasane で作った子の `.exe` (`tests/data/windows/child.ksnl`) と `cmd` を起動する。
#[test]
fn std_process_works_on_windows() {
    if !cross_ready() {
        return;
    }
    // 子: 引数を 1 行ずつ出す (`args`)・環境変数 KSN_A を出す (`env`)・終了コード 7 (`exit`)・標準入力の長さを数える (`stdin`)。
    let cd = std::env::temp_dir().join(format!("ksn_win_child_{}", std::process::id()));
    let _guard = Tmp(cd.clone());
    let _ = fs::remove_dir_all(&cd);
    fs::create_dir_all(cd.join("src")).unwrap();
    fs::write(cd.join("kasane.toml"), TOML.replace("name = \"t\"", "name = \"h\"").replace("t.main", "h.main")).unwrap();
    fs::write(cd.join("kasane.lock"), "version = 1\n").unwrap();
    fs::write(cd.join("src").join("h.ksnl"), fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/windows/child.ksnl")).unwrap()).unwrap();
    let o = run(&["build", "--target", TRIPLE], &cd);
    assert_eq!(o.status.code(), Some(0), "child build\n{}{}", text(&o.stdout), text(&o.stderr));
    let helper = native_exe(&text(&o.stdout), &cd);
    let helper_str = format!("Z:{}", helper.to_string_lossy());

    let src = r#"fn t.helper(mode: &str) -> std.process.Command uses [process] panics never
    let mut c = std.process.Command.new("HELPER")
    c.arg(mode)
    c
end t.helper

fn t.inherit(c: std.process.Command) -> Result<i32, std.io.Error> uses [process] panics never
    let child = c.spawn()?
    let st = child.wait()?
    Ok(st.code)
end t.inherit

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, process] panics may
    let mut a = t.helper("args")
    a.arg("a b")
    a.arg("c\"d")
    a.arg("e\\")
    a.arg("")
    a.arg("x\\\\\"y")
    a.arg("plain")
    core.check.eq(t.inherit(a)?, 0i32)
    let mut e1 = t.helper("env")
    e1.env("KSN_A", "v1")
    core.check.eq(t.inherit(e1)?, 0i32)
    let mut e2 = t.helper("env")
    e2.env("KSN_A", "v2")
    e2.env_remove("KSN_A")
    core.check.eq(t.inherit(e2)?, 0i32)
    let mut e3 = t.helper("env")
    e3.env("KSN_A", "v3")
    e3.env_clear()
    core.check.eq(t.inherit(e3)?, 0i32)
    let x = t.helper("exit")
    core.check.eq(t.inherit(x)?, 7i32)
    let mut p = t.helper("stdin")
    p.stdin_piped()
    p.stdout_piped()
    let mut child = p.spawn()?
    let n = match child.write_stdin(b"hello, pipe") {
        Ok(n) => n
        Err(e) => {
            core.mem.drop(child.wait())
            return Err(e)
        }
    }
    core.check.eq(n, 11usize)
    match child.close_stdin() {
        Ok(()) => {
        }
        Err(e) => {
            core.mem.drop(child.wait())
            return Err(e)
        }
    }
    let mut total = 0usize
    let mut buf = [0u8; 4]
    loop {
        match child.read_stdout(&mut buf) {
            Ok(k) => {
                if k == 0usize {
                    break
                }
                total = total + k
            }
            Err(e) => {
                core.mem.drop(child.wait())
                return Err(e)
            }
        }
    }
    core.check.eq(total, 9usize)
    let st = child.wait()?
    core.check.eq(st.code, 0i32)
    let mut c = std.process.Command.new("cmd")
    c.arg("/c")
    c.arg("echo")
    c.arg("hi")
    let mut out = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut er = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let st2 = c.output(&mut out, &mut er)?
    core.check.eq(st2.code, 0i32)
    core.check.eq(out.len(), 4usize)
    let mut d = std.process.Command.new("cmd")
    d.arg("/c")
    d.arg("cd")
    d.current_dir("C:/windows")
    let mut out2 = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut er2 = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let st3 = d.output(&mut out2, &mut er2)?
    core.check.eq(st3.code, 0i32)
    core.check.eq(out2.len(), 12usize)
    let mut f = std.process.Command.new("cmd")
    f.arg("/c")
    f.arg("exit 3")
    let mut out3 = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut er3 = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let st4 = f.output(&mut out3, &mut er3)?
    core.check.eq(st4.code, 3i32)
    let m = std.process.Command.new("C:/no/such/ksn-program.exe")
    core.mem.drop(match m.spawn() {
        Ok(ch) => {
            core.mem.drop(ch.wait())
            std.io.print_line("spawned")
        }
        Err(e) => {
            core.check.eq(e.kind, std.io.ErrorKind.NotFound)
            std.io.print_line("not found")
        }
    })
    Ok(())
end t.main
"#
    .replace("HELPER", &helper_str);
    let t = Tmp::new("win_proc", &src);
    fs::write(t.0.join("kasane.toml"), TOML).unwrap();
    let o = run(&["build", "--target", TRIPLE], &t.0);
    assert_eq!(o.status.code(), Some(0), "parent build\n{}{}", text(&o.stdout), text(&o.stderr));
    let exe = native_exe(&text(&o.stdout), &t.0);
    let r = run_wine(&exe, &t.0);
    let want = "a b\nc\"d\ne\\\n\nx\\\\\"y\nplain\nv1\nnone\nnone\n".to_string() + "not found\n";
    assert_eq!(text(&r.stdout), want, "stderr: {}", text(&r.stderr));
    assert_eq!(r.status.code(), Some(0));
}
