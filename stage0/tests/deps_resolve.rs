// R2a / R2b: 依存グラフ全体の版の解決 (`deps_resolve.rs`) と `ksn update [<依存>]`。
// 取得元はローカルの裸ではない git リポジトリ (タグ付き)。ネットワークには出ない。

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
        let dir = std::env::temp_dir().join(format!("ksn_dr_{}_{}_{}", std::process::id(), name, n));
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

fn manifest(name: &str, version: &str, entry: bool, deps: &str) -> String {
    let e = if entry { format!("entry = \"{}.main\"\n", name) } else { String::new() };
    format!("[package]\nname = \"{}\"\nversion = \"{}\"\nedition = 1\n{}\n[dependencies]\n{}", name, version, e, deps)
}

fn make_pkg(root: &Path, name: &str, version: &str, entry: bool, deps: &str, files: &[(&str, &str)]) {
    write(&root.join("kasane.toml"), &manifest(name, version, entry, deps));
    write(&root.join("kasane.lock"), "version = 1\n");
    for (rel, text) in files {
        write(&root.join(rel), text);
    }
}

fn lib_src(name: &str) -> String {
    format!("fn {n}.double(x: u32) -> u32 uses [] panics never pub\n    x.wrapping_mul(2u32)\nend {n}.double\n", n = name)
}

const APP_SRC: &str = "fn app.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"ok\")\nend app.main\n";

/// タグ付きの依存リポジトリ `name` を作る。`versions` の各版に 1 コミットずつ、タグ `v<版>` を打つ。
/// 戻り値は (リポジトリの場所, url, 各版のコミット)。`kasane.toml` の版はタグと一致させる。
fn tagged_repo(w: &Work, name: &str, versions: &[&str]) -> (PathBuf, String, Vec<String>) {
    let repo = w.path(&format!("repo_{}", name));
    let mut commits = vec![];
    for (i, v) in versions.iter().enumerate() {
        write(&repo.join("kasane.toml"), &manifest(name, v, false, ""));
        // 版の数 (`kasane.toml` の版) でコミットが別になる。
        write(&repo.join("src/lib.ksnl"), &lib_src(name));
        if i == 0 {
            git(&repo, &["init", "--quiet"]);
        }
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "--quiet", "-m", v]);
        git(&repo, &["tag", &format!("v{}", v)]);
        commits.push(git(&repo, &["rev-parse", "HEAD"]));
    }
    let url = format!("file://{}", repo.to_string_lossy().replace('\\', "/"));
    (repo, url, commits)
}

/// 版を 1 つ増やす (新しいタグ)。
fn publish(repo: &Path, name: &str, v: &str) -> String {
    write(&repo.join("kasane.toml"), &manifest(name, v, false, ""));
    write(&repo.join("src/lib.ksnl"), &lib_src(name));
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "--quiet", "-m", v]);
    git(repo, &["tag", &format!("v{}", v)]);
    git(repo, &["rev-parse", "HEAD"])
}

fn lock_entry(app: &Path, dep: &str) -> toml::Value {
    let v: toml::Value = fs::read_to_string(app.join("kasane.lock")).unwrap().parse().unwrap();
    v["package"].as_array().unwrap().iter().find(|p| p["name"].as_str() == Some(dep)).unwrap().clone()
}

fn lock_version(app: &Path, dep: &str) -> String {
    lock_entry(app, dep)["version"].as_str().unwrap().to_string()
}

fn lock_commit(app: &Path, dep: &str) -> String {
    let s = lock_entry(app, dep)["source"].as_str().unwrap().to_string();
    s.rsplit_once('#').unwrap().1.to_string()
}

fn gitdep(url: &str, range: &str) -> String {
    format!("{{ git = \"{}\", version = \"{}\" }}", url, range)
}

/// `app` が直接 `a` `b` (どちらも path 依存) を持ち、`a` と `b` がそれぞれ `gl` を範囲で要求する。
fn diamond(w: &Work, url: &str, range_a: &str, range_b: &str) -> PathBuf {
    let app = w.path("app");
    make_pkg(
        &app,
        "app",
        "0.1.0",
        true,
        "a = { path = \"deps/a\" }\nb = { path = \"deps/b\" }\n",
        &[("src/app.ksnl", APP_SRC)],
    );
    make_pkg(
        &app.join("deps/a"),
        "a",
        "0.1.0",
        false,
        &format!("gl = {}\n", gitdep(url, range_a)),
        &[("src/a.ksnl", &lib_src("a"))],
    );
    make_pkg(
        &app.join("deps/b"),
        "b",
        "0.1.0",
        false,
        &format!("gl = {}\n", gitdep(url, range_b)),
        &[("src/b.ksnl", &lib_src("b"))],
    );
    app
}

fn single(w: &Work, url: &str, range: &str) -> PathBuf {
    let app = w.path("app");
    make_pkg(&app, "app", "0.1.0", true, &format!("gl = {}\n", gitdep(url, range)), &[("src/app.ksnl", APP_SRC)]);
    app
}

#[test]
fn diamond_picks_the_highest_tag_that_satisfies_every_requirer() {
    let w = Work::new("diamond");
    let (_r, url, c) = tagged_repo(&w, "gl", &["1.0.0", "1.1.0", "1.2.0"]);
    // a は >=1.0.0, <1.2.0、b は >=1.0.0 → 積は [1.0.0, 1.2.0)。最大は 1.1.0。
    let app = diamond(&w, &url, ">=1.0.0, <1.2.0", ">=1.0.0");
    let o = w.ksn(&["update"], &app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    assert_eq!(lock_version(&app, "gl"), "1.1.0");
    assert_eq!(lock_commit(&app, "gl"), c[1]);
    assert_eq!(w.ksn(&["check"], &app).status.code(), Some(0), "{}", all(&o));
}

#[test]
fn conflicting_requirements_name_both_requirers_and_their_ranges() {
    let w = Work::new("conflict");
    let (_r, url, _c) = tagged_repo(&w, "gl", &["1.0.0", "1.1.0"]);
    let app = diamond(&w, &url, "=1.0.0", "=1.1.0");
    let o = w.ksn(&["update"], &app);
    let text = all(&o);
    assert_ne!(o.status.code(), Some(0), "{}", text);
    assert!(text.contains("E9023"), "{}", text);
    assert!(text.contains("gl") && text.contains("a requires \"=1.0.0\"") && text.contains("b requires \"=1.1.0\""), "{}", text);
}

#[test]
fn a_range_that_no_tag_satisfies_is_an_error_naming_the_requirers() {
    let w = Work::new("nomatch_graph");
    let (_r, url, _c) = tagged_repo(&w, "gl", &["1.0.0"]);
    let app = diamond(&w, &url, ">=1.0.0", ">=2.0.0");
    let o = w.ksn(&["update"], &app);
    let text = all(&o);
    assert_eq!(o.status.code(), Some(2), "{}", text);
    assert!(text.contains("E9010") && text.contains("b requires \">=2.0.0\""), "{}", text);
}

#[test]
fn update_one_dependency_moves_only_it_and_prints_old_and_new_versions() {
    let w = Work::new("update_one");
    let (gl_repo, gl_url, _) = tagged_repo(&w, "gl", &["1.0.0", "1.1.0"]);
    let (hl_repo, hl_url, _) = tagged_repo(&w, "hl", &["1.0.0", "1.1.0"]);
    let app = w.path("app");
    make_pkg(
        &app,
        "app",
        "0.1.0",
        true,
        &format!("gl = {}\nhl = {}\n", gitdep(&gl_url, ">=1.0.0, <2.0.0"), gitdep(&hl_url, ">=1.0.0, <2.0.0")),
        &[("src/app.ksnl", APP_SRC)],
    );
    assert_eq!(w.ksn(&["update"], &app).status.code(), Some(0));
    assert_eq!(lock_version(&app, "gl"), "1.1.0");
    assert_eq!(lock_version(&app, "hl"), "1.1.0");
    publish(&gl_repo, "gl", "1.2.0");
    publish(&hl_repo, "hl", "1.2.0");
    let hl_before = lock_commit(&app, "hl");
    let o = w.ksn(&["update", "gl"], &app);
    let text = all(&o);
    assert_eq!(o.status.code(), Some(0), "{}", text);
    assert!(text.contains("ksn update: gl 1.1.0 -> 1.2.0"), "{}", text);
    assert_eq!(lock_version(&app, "gl"), "1.2.0");
    assert_eq!(lock_version(&app, "hl"), "1.1.0", "update gl must not move hl");
    assert_eq!(lock_commit(&app, "hl"), hl_before);
    // 名前が依存に無いときは E9017。
    let o = w.ksn(&["update", "nope"], &app);
    assert!(all(&o).contains("E9017"), "{}", all(&o));
}

#[test]
fn a_satisfied_lock_is_used_without_touching_the_repository() {
    let w = Work::new("nofetch");
    let (gl_repo, gl_url, _) = tagged_repo(&w, "gl", &["1.0.0", "1.1.0"]);
    let app = single(&w, &gl_url, ">=1.0.0, <2.0.0");
    assert_eq!(w.ksn(&["update"], &app).status.code(), Some(0));
    let locked = lock_commit(&app, "gl");
    // 新しい版が出ても、lock が要求を満たすなら check / build はそのまま通り、lock も動かない。
    publish(&gl_repo, "gl", "1.2.0");
    fs::remove_dir_all(&gl_repo).unwrap();
    for args in [&["check"][..], &["build"][..]] {
        let o = w.ksn(args, &app);
        assert_eq!(o.status.code(), Some(0), "{:?}: {}", args, all(&o));
    }
    assert_eq!(lock_commit(&app, "gl"), locked);
}

#[test]
fn a_manifest_the_lock_no_longer_satisfies_fails_offline_and_asks_for_update() {
    let w = Work::new("stale");
    let (_r, url, _c) = tagged_repo(&w, "gl", &["1.0.0", "1.1.0"]);
    let app = single(&w, &url, ">=1.0.0, <2.0.0");
    assert_eq!(w.ksn(&["update"], &app).status.code(), Some(0));
    // 範囲を lock の版 (1.1.0) の外へ動かす。
    make_pkg(&app, "app", "0.1.0", true, &format!("gl = {}\n", gitdep(&url, "=1.0.0")), &[("src/app.ksnl", APP_SRC)]);
    let o = w.ksn(&["check"], &app);
    let text = all(&o);
    assert_eq!(o.status.code(), Some(1), "{}", text);
    assert!(text.contains("E9010") && text.contains("ksn update"), "{}", text);
    // update で直る。
    assert_eq!(w.ksn(&["update"], &app).status.code(), Some(0));
    assert_eq!(lock_version(&app, "gl"), "1.0.0");
    assert_eq!(w.ksn(&["check"], &app).status.code(), Some(0));
}
