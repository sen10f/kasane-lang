//! `[native]` (簡易仕様書 3.8): 静的アーカイブ・動的ライブラリを実行ファイルにリンクし、インタプリタでは
//! `dynamic` を `dlopen` する (`static` はアーカイブを読めないので診断)。toml の厳密さと `kasane.lock` の記録。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use common::*;

/// 小さな C のライブラリ: 静的アーカイブに入れる関数と、共有ライブラリに入れる関数が 1 つずつ。
const ADD_C: &str = r#"int add3(int x) { return x + 3; }
int ext_val(void) { return 21; }
"#;

const ADD_SRC: &str = r#"extern t.add3(x: i32) -> i32 link "add3"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe "libc" "static archive" {
        t.add3(39i32)
    }
    core.check.eq(a, 42i32)
    core.mem.drop(std.io.print_line("static ok"))
    Ok(())
end t.main
"#;

const EXT_SRC: &str = r#"extern t.ext_val() -> i32 link "ext_val"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe "libc" "dynamic library" {
        t.ext_val()
    }
    core.check.eq(a, 21i32)
    core.mem.drop(std.io.print_line("dynamic ok"))
    Ok(())
end t.main
"#;

fn manifest(native: &str) -> String {
    format!(
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[unsafe]\nmodules = [\"t\"]\n\n[native]\n{}\n\n[dependencies]\n",
        native
    )
}

fn c_compiler() -> &'static str {
    ["cc", "gcc", "clang"].into_iter().find(|c| Command::new(c).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)).expect("a C compiler (cc / gcc / clang) is required for these tests")
}

#[cfg(unix)]
fn cc(args: &[&str], dir: &Path) -> Output {
    Command::new(c_compiler()).args(args).current_dir(dir).output().expect("spawn the C compiler")
}

/// `libadd.a` (静的アーカイブ) を作る。戻り値はアーカイブのパス。
#[cfg(unix)]
fn static_archive(dir: &Path) -> PathBuf {
    fs::write(dir.join("add.c"), ADD_C).unwrap();
    let o = cc(&["-c", "-fPIC", "-o", "add.o", "add.c"], dir);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let o = Command::new("ar").args(["crs", "libadd.a", "add.o"]).current_dir(dir).output().expect("spawn ar");
    assert!(o.status.success(), "{}", text(&o.stderr));
    dir.join("libadd.a")
}

/// `libext.so` (共有ライブラリ) を作る。戻り値はライブラリのパス。
#[cfg(unix)]
fn shared_lib(dir: &Path) -> PathBuf {
    fs::write(dir.join("ext.c"), ADD_C).unwrap();
    let so = dir.join("libext.so");
    let o = cc(&["-shared", "-fPIC", "-o", so.to_str().unwrap(), "ext.c"], dir);
    assert!(o.status.success(), "{}", text(&o.stderr));
    so
}

/// `ksn build` を、キャッシュをこの試験だけの場所にして走らせる。
fn build(t: &Tmp) -> Output {
    let cache = std::env::temp_dir().join(format!("ksn_cache_nat_{}_{}", std::process::id(), t.0.file_name().unwrap().to_string_lossy()));
    let _ = fs::remove_dir_all(&cache);
    let o = Command::new(exe()).args(["build"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    o
}

#[cfg(unix)]
#[test]
fn a_static_archive_is_linked_into_the_native_executable() {
    let t = Tmp::new("nat_static", "");
    let dir = &t.0;
    let ar = static_archive(dir);
    fs::write(dir.join("src/t.ksnl"), ADD_SRC).unwrap();
    fs::write(dir.join("kasane.toml"), manifest("static = [\"libadd.a\"]")).unwrap();
    let c = run(&["check"], dir);
    assert_eq!(c.status.code(), Some(0), "{}{}", text(&c.stdout), text(&c.stderr));
    let b = build(&t);
    assert_eq!(b.status.code(), Some(0), "{}{}", text(&b.stdout), text(&b.stderr));
    let exe = dir.join("target").join("debug").join("t");
    assert!(crate::common::artifact_is_native(&exe), "{}", text(&b.stdout));
    assert!(ar.exists());
    let o = Command::new(&exe).current_dir(dir).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), "static ok\n");
    // `run` (インタプリタ) ではアーカイブを読めないので、`ksn build` を引导する診断になる。
    let r = run(&["run"], dir);
    assert_eq!(r.status.code(), Some(101), "{} {}", text(&r.stdout), text(&r.stderr));
    let err = text(&r.stderr);
    assert!(err.contains("[native] static") && err.contains("ksn build"), "{}", err);
}

/// `dynamic` のライブラリを `search` から探してネイティブにリンクし、インタプリタでも `dlopen` する。
#[cfg(unix)]
#[test]
fn a_dynamic_library_is_linked_natively_and_loaded_by_the_interpreter() {
    let t = Tmp::new("nat_dynamic", "");
    let dir = &t.0;
    let so = shared_lib(dir);
    fs::write(dir.join("src/t.ksnl"), EXT_SRC).unwrap();
    fs::write(dir.join("kasane.toml"), manifest("dynamic = [\"ext\"]\nsearch = [\".\"]")).unwrap();
    let c = run(&["check"], dir);
    assert_eq!(c.status.code(), Some(0), "{}{}", text(&c.stdout), text(&c.stderr));
    // インタプリタ: `search` の `libext.so` を開く (環境変数は要らない)。
    let r = run(&["run"], dir);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "dynamic ok\n");
    // ネイティブ: `search` の `-L` から `-lext` でつなぐ (実行時の探索先は LD_LIBRARY_PATH)。
    let b = build(&t);
    assert_eq!(b.status.code(), Some(0), "{}{}", text(&b.stdout), text(&b.stderr));
    let exe = dir.join("target").join("debug").join("t");
    assert!(crate::common::artifact_is_native(&exe), "{}", text(&b.stdout));
    assert!(so.exists());
    let o = Command::new(&exe).current_dir(dir).env("LD_LIBRARY_PATH", dir).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), "dynamic ok\n");
}

#[test]
fn native_table_is_validated_strictly() {
    // 知らないキー・名前でない動的ライブラリ・配列でない値。
    for (native, code) in [
        ("junk = [\"x\"]", "E9007"),
        ("dynamic = [\"libext.so\"]", "E9012"),
        ("dynamic = [\"lib:ext\"]", "E9012"),
        ("dynamic = [\"-lext\"]", "E9012"),
        ("static = \"libadd.a\"", "E9012"),
        ("static = [1]", "E9012"),
    ] {
        let t = Tmp::new("nat_strict", "");
        fs::write(t.0.join("src/t.ksnl"), ADD_SRC).unwrap();
        fs::write(t.0.join("kasane.toml"), manifest(native)).unwrap();
        let o = run(&["check", "--json"], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}: {}", native, text(&o.stdout));
        assert_eq!(codes(&text(&o.stdout)), vec![code.to_string()], "{}: {}", native, text(&o.stdout));
    }
    // `native = 1` のような表でない指定は受けない (`[unsafe]` の中に落ちても知らないキー)。
    let t = Tmp::new("nat_not_table", "");
    fs::write(t.0.join("src/t.ksnl"), ADD_SRC).unwrap();
    fs::write(t.0.join("kasane.toml"), manifest("").replacen("[native]", "native = 1", 1)).unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(codes(&text(&o.stdout)), vec!["E9007".to_string()], "{}", text(&o.stdout));
    // 正しい指定は通る (診断 0 件)。
    let t = Tmp::new("nat_ok", "");
    fs::write(t.0.join("src/t.ksnl"), ADD_SRC).unwrap();
    fs::write(t.0.join("kasane.toml"), manifest("static = [\"libadd.a\"]\ndynamic = [\"m\"]\nsearch = [\"vendor\"]")).unwrap();
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}{}", text(&c.stdout), text(&c.stderr));
    // wasm32 には C のライブラリをつなげない (E9040)。
    let o = Command::new(exe()).args(["build", "--target", "wasm32-unknown-unknown"]).current_dir(&t.0).output().unwrap();
    let all = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{}", all);
    assert!(all.contains("E9040") && all.contains("[native]"), "{}", all);
}

// ---- kasane.lock: 依存の `[native]` の記録 (8.1) ----

static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct Work {
    dir: PathBuf,
    cache: PathBuf,
}
impl Work {
    fn new(name: &str) -> Work {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ksn_natlock_{}_{}_{}", std::process::id(), name, n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("cache");
        fs::create_dir_all(&cache).unwrap();
        Work { dir, cache }
    }
    fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }
    fn ksn(&self, args: &[&str], cwd: &Path) -> Output {
        Command::new(exe()).args(args).current_dir(cwd).env("KSN_CACHE_DIR", &self.cache).output().expect("spawn ksn")
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn all(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).replace("\r\n", "\n")
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git must be installed to run these tests");
    assert!(o.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

const DEP_LIB: &str = "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = 1\n\n[native]\ndynamic = [\"ext\"]\n\n[dependencies]\n";
const DEP_SRC: &str = "fn dep.one() -> u32 uses [] panics never pub\n    1u32\nend dep.one\n";
const APP: &str = "fn app.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.eq(dep.one(), 1u32)\n    core.mem.drop(std.io.print_line(\"ok\"))\n    Ok(())\nend app.main\n";

struct Fx {
    w: Work,
    app: PathBuf,
    repo: PathBuf,
    url: String,
}

fn setup(name: &str) -> Fx {
    let w = Work::new(name);
    let repo = w.path("dep_repo");
    write(&repo.join("kasane.toml"), DEP_LIB);
    write(&repo.join("src/dep.ksnl"), DEP_SRC);
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "-m", "v1"]);
    let rev = git(&repo, &["rev-parse", "HEAD"]);
    let url = format!("file://{}", repo.to_string_lossy());
    let app = w.path("app");
    write(
        &app.join("kasane.toml"),
        &format!("[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"app.main\"\n\n[dependencies]\ndep = {{ git = \"{}\", rev = \"{}\" }}\n", url, rev),
    );
    write(&app.join("kasane.lock"), "version = 1\n");
    write(&app.join("src/app.ksnl"), APP);
    let o = w.ksn(&["update"], &app);
    assert_eq!(o.status.code(), Some(0), "update: {}", all(&o));
    Fx { w, app, repo, url }
}

impl Fx {
    /// 依存の `[native]` を書き換えて新しい版に上げる。
    fn bump(&self, native: &str) {
        let toml = format!("[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = 1\n\n[native]\n{}\n\n[dependencies]\n", native);
        write(&self.repo.join("kasane.toml"), &toml);
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "--quiet", "-m", "v2"]);
        let rev = git(&self.repo, &["rev-parse", "HEAD"]);
        write(
            &self.app.join("kasane.toml"),
            &format!("[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"app.main\"\n\n[dependencies]\ndep = {{ git = \"{}\", rev = \"{}\" }}\n", self.url, rev),
        );
        let o = self.w.ksn(&["update", "dep"], &self.app);
        assert_eq!(o.status.code(), Some(0), "update dep: {}", all(&o));
    }
    fn check(&self) -> Output {
        self.w.ksn(&["check"], &self.app)
    }
    fn lock(&self) -> String {
        fs::read_to_string(self.app.join("kasane.lock")).unwrap()
    }
}

#[test]
fn a_native_library_added_to_a_dependency_stops_with_e9060() {
    let f = setup("added");
    assert_eq!(f.check().status.code(), Some(0), "{}", all(&f.check()));
    assert!(f.lock().contains("native_libs = [\"ext\"]\n"), "{}", f.lock());
    // もう 1 つ足されたのは広がり (E9060)。受け入れるまで check / build / test / run が止まる。
    f.bump("dynamic = [\"ext\", \"m\"]");
    for sub in ["check", "build", "test", "run"] {
        let o = f.w.ksn(&[sub], &f.app);
        let out = all(&o);
        assert_eq!(o.status.code(), Some(1), "{}: {}", sub, out);
        assert!(out.contains("E9060") && out.contains("native library m was added"), "{}: {}", sub, out);
        assert!(!out.contains("E9061") && !out.contains("E9062"), "{}: {}", sub, out);
    }
    let before = f.lock();
    let o = f.w.ksn(&["lock", "--diff"], &f.app);
    assert!(all(&o).contains("E9060"), "{}", all(&o));
    assert_eq!(f.lock(), before, "--diff は書かない");
    assert_eq!(f.w.ksn(&["lock", "--accept", "dep"], &f.app).status.code(), Some(0));
    assert!(f.lock().contains("native_libs = [\"ext\", \"m\"]\n"), "{}", f.lock());
    assert_eq!(f.check().status.code(), Some(0), "{}", all(&f.check()));
}

#[test]
fn a_native_library_removed_from_a_dependency_stops_with_e9062() {
    let f = setup("removed");
    // 初めから `[native]` が無い版にして `ksn update` したあとでは、記録が空になる。
    let toml = "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = 1\n\n[dependencies]\n";
    write(&f.repo.join("kasane.toml"), toml);
    git(&f.repo, &["add", "-A"]);
    git(&f.repo, &["commit", "--quiet", "-m", "no native"]);
    let rev = git(&f.repo, &["rev-parse", "HEAD"]);
    write(
        &f.app.join("kasane.toml"),
        &format!("[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"app.main\"\n\n[dependencies]\ndep = {{ git = \"{}\", rev = \"{}\" }}\n", f.url, rev),
    );
    assert_eq!(f.w.ksn(&["update", "dep"], &f.app).status.code(), Some(0), "update dep");
    let o = f.check();
    let out = all(&o);
    assert_eq!(o.status.code(), Some(1), "{}", out);
    assert!(out.contains("E9062") && out.contains("native library ext was removed"), "{}", out);
    assert_eq!(f.w.ksn(&["lock", "--accept", "dep"], &f.app).status.code(), Some(0));
    assert_eq!(f.check().status.code(), Some(0), "{}", all(&f.check()));
    assert!(!f.lock().contains("native_libs"), "{}", f.lock());
}