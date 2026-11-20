// R2c: 版 (`git` + `version`) の git 依存を、タグからコミットへ解決して lock に記録し、
// `ksn vendor` と build の照合 (ハッシュ) を通す。取得元はローカルの git リポジトリ (タグ付き)。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Work {
    dir: PathBuf,
    cache: PathBuf,
}

impl Work {
    fn new(name: &str) -> Work {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ksn_dv_{}_{}_{}", std::process::id(), name, n));
        let _ = fs::remove_dir_all(&dir);
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
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "tag.gpgsign=false", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(o.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn manifest(name: &str, entry: bool, deps: &str) -> String {
    let e = if entry { format!("entry = \"{}.main\"\n", name) } else { String::new() };
    format!("[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = 1\n{}\n[dependencies]\n{}", name, e, deps)
}

fn make_pkg(root: &Path, name: &str, entry: bool, deps: &str, files: &[(&str, &str)]) {
    write(&root.join("kasane.toml"), &manifest(name, entry, deps));
    write(&root.join("kasane.lock"), "version = 1\n");
    for (rel, text) in files {
        write(&root.join(rel), text);
    }
}

/// `gl` の版の履歴: v1.0.0 → v1.1.0。`gl` は `(url, 各版のコミット)` を返す。
fn gl_repo(w: &Work) -> (PathBuf, String, String, String) {
    let repo = w.path("gl_repo");
    write(&repo.join("kasane.toml"), &manifest("gl", false, ""));
    write(&repo.join("src/gl.ksnl"), GL_V1);
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "-m", "v1.0.0"]);
    git(&repo, &["tag", "v1.0.0"]);
    let c1 = git(&repo, &["rev-parse", "HEAD"]);
    write(&repo.join("src/gl.ksnl"), GL_V11);
    git(&repo, &["commit", "--quiet", "-am", "v1.1.0"]);
    git(&repo, &["tag", "v1.1.0"]);
    let c2 = git(&repo, &["rev-parse", "HEAD"]);
    let url = format!("file://{}", repo.to_string_lossy().replace('\\', "/"));
    (repo, url, c1, c2)
}

const GL_V1: &str = "fn gl.double(x: u32) -> u32 uses [] panics never pub\n    x.wrapping_mul(2u32)\nend gl.double\n";
const GL_V11: &str = "fn gl.double(x: u32) -> u32 uses [] panics never pub\n    x.wrapping_mul(2u32)\nend gl.double\n\nfn gl.triple(x: u32) -> u32 uses [] panics never pub\n    x.wrapping_mul(3u32)\nend gl.triple\n";
const GL_V12: &str = "fn gl.double(x: u32) -> u32 uses [] panics never pub\n    x.wrapping_mul(2u32)\nend gl.double\n\nfn gl.quad(x: u32) -> u32 uses [] panics never pub\n    x.wrapping_mul(4u32)\nend gl.quad\n";
const APP_SRC: &str = "fn app.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"ok\")\nend app.main\n";

/// `app` を作り、`gl` を `version` で依存させる。
fn app_with_version(w: &Work, url: &str, range: &str) -> PathBuf {
    let app = w.path("app");
    let deps = format!("gl = {{ git = \"{}\", version = \"{}\" }}\n", url, range);
    make_pkg(&app, "app", true, &deps, &[("src/app.ksnl", APP_SRC)]);
    app
}

fn lock_source(app: &Path, dep: &str) -> String {
    let v: toml::Value = fs::read_to_string(app.join("kasane.lock")).unwrap().parse().unwrap();
    let pk = v["package"].as_array().unwrap();
    pk.iter().find(|p| p["name"].as_str() == Some(dep)).unwrap()["source"].as_str().unwrap().to_string()
}

fn lock_hash(app: &Path, dep: &str) -> String {
    let v: toml::Value = fs::read_to_string(app.join("kasane.lock")).unwrap().parse().unwrap();
    let pk = v["package"].as_array().unwrap();
    pk.iter().find(|p| p["name"].as_str() == Some(dep)).unwrap()["hash"].as_str().unwrap().to_string()
}

#[test]
fn version_resolves_to_the_highest_matching_tag_and_is_recorded_as_its_commit() {
    let w = Work::new("resolve");
    let (_repo, url, _c1, c2) = gl_repo(&w);
    let app = app_with_version(&w, &url, ">=1.0.0, <2.0.0");
    let o = w.ksn(&["update"], &app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    // 範囲 1.0 (= ^1.0.0 相当) に合う最大は v1.1.0 のコミット。
    assert_eq!(lock_source(&app, "gl"), format!("git+{}#{}", url, c2));
    assert_eq!(w.ksn(&["check"], &app).status.code(), Some(0));
}

#[test]
fn a_range_with_no_matching_tag_is_an_error_naming_the_dependency() {
    let w = Work::new("nomatch");
    let (_repo, url, ..) = gl_repo(&w);
    let app = app_with_version(&w, &url, "=2.0.0");
    let o = w.ksn(&["update"], &app);
    assert_ne!(o.status.code(), Some(0), "{}", all(&o));
    assert!(all(&o).contains("E9010") && all(&o).contains("gl"), "{}", all(&o));
}

#[test]
fn lock_pins_the_commit_when_a_newer_tag_appears_and_update_moves_it() {
    let w = Work::new("pin");
    let (repo, url, _c1, c2) = gl_repo(&w);
    let app = app_with_version(&w, &url, ">=1.0.0, <2.0.0");
    assert_eq!(w.ksn(&["update"], &app).status.code(), Some(0));
    let locked = lock_source(&app, "gl");
    assert_eq!(locked, format!("git+{}#{}", url, c2));
    // 新しい版 v1.2.0 を出す。build は lock のコミットのままで、ネットワークを見ない。
    write(&repo.join("src/gl.ksnl"), GL_V12);
    git(&repo, &["commit", "--quiet", "-am", "v1.2.0"]);
    git(&repo, &["tag", "v1.2.0"]);
    let c3 = git(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(w.ksn(&["check"], &app).status.code(), Some(0));
    assert_eq!(lock_source(&app, "gl"), locked, "check must not move the lock");
    // `ksn update` で v1.2.0 へ動き、lock が書き換わる。
    assert_eq!(w.ksn(&["update"], &app).status.code(), Some(0));
    assert_eq!(lock_source(&app, "gl"), format!("git+{}#{}", url, c3));
}

#[test]
fn vendor_keeps_the_locked_commit_and_builds_without_the_repository() {
    let w = Work::new("vendor");
    let (repo, url, _c1, c2) = gl_repo(&w);
    let app = app_with_version(&w, &url, ">=1.0.0, <2.0.0");
    assert_eq!(w.ksn(&["update"], &app).status.code(), Some(0));
    // 版の範囲が変わっても (タグが増えても) vendor は lock のコミットを取る。
    write(&repo.join("src/gl.ksnl"), GL_V12);
    git(&repo, &["commit", "--quiet", "-am", "v1.2.0"]);
    git(&repo, &["tag", "v1.2.0"]);
    let o = w.ksn(&["vendor"], &app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    assert_eq!(lock_source(&app, "gl"), format!("git+{}#{}", url, c2));
    assert!(app.join("vendor/gl/src/gl.ksnl").exists());
    let mark = fs::read_to_string(app.join("vendor/gl/.ksn-source")).unwrap();
    assert_eq!(mark.trim(), format!("git+{}#{}", url, c2));
    // 元のリポジトリとキャッシュを消しても、vendor だけで通る (ネットワーク不要)。
    fs::remove_dir_all(&repo).unwrap();
    fs::remove_dir_all(w.cache.join("git")).ok();
    for args in [&["check"][..], &["build"][..], &["run"][..]] {
        let o = w.ksn(args, &app);
        assert_eq!(o.status.code(), Some(0), "{:?}: {}", args, all(&o));
    }
}

#[test]
fn a_tampered_vendor_copy_fails_with_the_dependency_and_both_hashes() {
    let w = Work::new("tamper");
    let (_repo, url, ..) = gl_repo(&w);
    let app = app_with_version(&w, &url, ">=1.0.0, <2.0.0");
    assert_eq!(w.ksn(&["update"], &app).status.code(), Some(0));
    assert_eq!(w.ksn(&["vendor"], &app).status.code(), Some(0));
    let recorded = lock_hash(&app, "gl");
    let f = app.join("vendor/gl/src/gl.ksnl");
    fs::write(&f, fs::read_to_string(&f).unwrap().replace("2u32", "5u32")).unwrap();
    let o = w.ksn(&["check"], &app);
    let text = all(&o);
    assert_eq!(o.status.code(), Some(1), "{}", text);
    assert!(text.contains("E9022") && text.contains("gl") && text.contains(&recorded), "{}", text);
    // 同じ版の取り直し (`ksn vendor`) で戻る。
    assert_eq!(w.ksn(&["vendor"], &app).status.code(), Some(0));
    assert_eq!(w.ksn(&["check"], &app).status.code(), Some(0));
}

#[test]
fn offline_version_dependency_without_a_lock_entry_is_an_error() {
    let w = Work::new("offline");
    let (_repo, url, ..) = gl_repo(&w);
    let app = app_with_version(&w, &url, ">=1.0.0, <2.0.0");
    // lock が無い (update していない) ので、ネットワークに出ずに E9010 で止まる。
    let o = w.ksn(&["check"], &app);
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(all(&o).contains("E9010"), "{}", all(&o));
}
