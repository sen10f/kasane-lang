// 依存パッケージの型 (構造体・列挙型) を、利用者の型と同じく KIR の型表に入れて下ろすことの試験。
// 2 パッケージ (`lib` が型と関数を定義し、`app` がそれを使う) で、`ksn build` (debug / release) のネイティブが
// `ksn run` と同じ出力になり、バンドルへの退避をしないことを確かめる。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn all(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).replace("\r\n", "\n")
}

fn manifest(name: &str, entry: Option<&str>, deps: &str) -> String {
    let e = entry.map(|e| format!("entry = \"{}\"\n", e)).unwrap_or_default();
    format!("[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = 1\n{}\n[dependencies]\n{}", name, e, deps)
}

const LIB_SRC: &str = "type lib.Pt pub
    x: u32 pub
    y: u32 pub
end lib.Pt

enum lib.Shape derive [Debug, Clone, Copy, PartialEq] pub
    Dot
    Edge
end lib.Shape

fn lib.Pt.sum(self: &lib.Pt) -> u32 uses [] panics never pub
    self.x.wrapping_add(self.y)
end lib.Pt.sum

fn lib.Shape.weight(self: lib.Shape) -> u32 uses [] panics never pub
    match self {
        lib.Shape.Dot => 1u32
        lib.Shape.Edge => 5u32
    }
end lib.Shape.weight

fn lib.make(a: u32) -> lib.Pt uses [] panics never pub
    lib.Pt {
        x: a
        y: 2u32
    }
end lib.make

fn lib.pick(p: lib.Pt, s: lib.Shape) -> lib.Shape uses [] panics never pub
    if p.sum() > 4u32 {
        return s
    }
    lib.Shape.Dot
end lib.pick
";

const APP_SRC: &str = "fn app.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let p = lib.make(3u32)
    let n0 = p.sum()
    let s = lib.pick(lib.make(3u32), lib.Shape.Edge)
    let n = n0.wrapping_add(s.weight())
    core.check.eq(n, 10u32)
    std.io.print_line(\"ok\")?
    Ok(())
end app.main
";

/// `app` (entry `app.main`) が path 依存 `lib` を持つ 2 パッケージを作る。
fn fixture(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ksn_dep_types_{}_{}_{}", std::process::id(), name, n));
    let _ = fs::remove_dir_all(&dir);
    let app = dir.join("app");
    write(&app.join("kasane.toml"), &manifest("app", Some("app.main"), "lib = { path = \"deps/lib\" }\n"));
    write(&app.join("src/app.ksnl"), APP_SRC);
    let lib = app.join("deps/lib");
    write(&lib.join("kasane.toml"), &manifest("lib", None, ""));
    write(&lib.join("src/lib.ksnl"), LIB_SRC);
    let u = ksn(&["update"], &app);
    assert!(u.status.success(), "ksn update: {}", all(&u));
    app
}

fn ksn(args: &[&str], cwd: &Path) -> Output {
    let cache = cwd.join("..").join("cache");
    fs::create_dir_all(&cache).unwrap();
    Command::new(exe()).args(args).current_dir(cwd).env("KSN_CACHE_DIR", &cache).output().expect("spawn ksn")
}

/// `ksn build` の出力のネイティブを走らせ、標準出力と終了コードを返す。
fn run_native(app: &Path, release: bool) -> (String, String) {
    let kind = if release { "release" } else { "debug" };
    let bin = app.join("target").join(kind).join(if cfg!(windows) { "app.exe" } else { "app" });
    let o = if release { ksn(&["build", "--release"], app) } else { ksn(&["build"], app) };
    let out = all(&o);
    assert!(!out.contains("KIR に下ろせない"), "下ろせない理由が出た: {}", out);
    assert!(!out.contains("skipped"), "ネイティブでなくバンドルへ退避した: {}", out);
    assert!(std::fs::read_to_string(&bin.parent().unwrap().join(".ksn-artifact")).map_or(false, |m| m.trim() == "native"), "ネイティブの実行ファイルがない: {}", out);
    let r = Command::new(&bin).current_dir(app).output().expect("run native");
    (all(&r), r.status.code().map(|c| c.to_string()).unwrap_or_default())
}

fn run_interp(app: &Path) -> (String, String) {
    let o = ksn(&["run"], app);
    (all(&o), o.status.code().map(|c| c.to_string()).unwrap_or_default())
}

#[test]
fn dep_types_native_debug_matches_run() {
    let app = fixture("debug");
    let want = run_interp(&app);
    assert_eq!(want.1, "0", "ksn run が成功しない: {}", want.0);
    let got = run_native(&app, false);
    assert_eq!(got, want);
}

#[test]
fn dep_types_native_release_matches_run() {
    let app = fixture("release");
    let want = run_interp(&app);
    assert_eq!(want.1, "0", "ksn run が成功しない: {}", want.0);
    let got = run_native(&app, true);
    assert_eq!(got, want);
}
