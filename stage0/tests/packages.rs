//! 統合テスト (`tests/` パッケージ規約、3.2) と、依存パッケージ (`path` / `git`、8.1) の実動作テスト。
//! ネットワークには出ない。`git` 依存は、テストが一時ディレクトリに作るローカルのリポジトリ
//! (`file://` の URL) に対して検証する。キャッシュ (`KSN_CACHE_DIR`) もテストごとの一時ディレクトリ。

#[path = "common/scratch.rs"]
mod scratch;

use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// テストごとの作業ディレクトリ (終わると消える)。`cache` は `KSN_CACHE_DIR` 用。
struct Work {
    dir: PathBuf,
    cache: PathBuf,
}
impl Work {
    fn new(name: &str) -> Work {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ksn_pkg_{}_{}_{}", std::process::id(), name, n));
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
        scratch::remove(&self.dir);
    }
}

fn all(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).replace("\r\n", "\n")
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// `kasane.toml` の文字列。`deps` は `[dependencies]` の中身 (行)。
fn manifest(name: &str, entry: bool, deps: &str) -> String {
    let e = if entry { format!("entry = \"{}.main\"\n", name) } else { String::new() };
    format!("[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = 1\n{}\n[dependencies]\n{}", name, e, deps)
}

/// パッケージを作る。files は (ルートからの相対パス, 内容)。`kasane.lock` は依存なしの形 (あとで `ksn update`)。
fn make_pkg(root: &Path, name: &str, entry: bool, deps: &str, files: &[(&str, &str)]) {
    write(&root.join("kasane.toml"), &manifest(name, entry, deps));
    write(&root.join("kasane.lock"), "version = 1\n");
    for (rel, text) in files {
        write(&root.join(rel), text);
    }
}

fn code_in(o: &Output, code: &str) -> bool {
    all(o).contains(code)
}

// ------------------------------------------------------------ 統合テスト (tests/)

const LIB_SRC: &str = "fn t.add(a: u32, b: u32) -> u32 uses [] panics never pub\n    a.wrapping_add(b)\nend t.add\n\nfn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"t ok\")\nend t.main\n\nfn t.secret() -> u32 uses [] panics never\n    7u32\nend t.secret\n\ntest t.add.test_unit\n    core.check.eq(t.add(1u32, 2u32), 3u32)\nend t.add.test_unit\n";
const SUB_SRC: &str = "fn t.sub.twice(x: u32) -> u32 uses [] panics never pub\n    t.add(x, x)\nend t.sub.twice\n";
const INT_OK: &str = "fn t.tests.basic.helper(x: u32) -> u32 uses [] panics never\n    t.sub.twice(x)\nend t.tests.basic.helper\n\ntest t.tests.basic.test_modules_work_together\n    core.check.eq(t.tests.basic.helper(4u32), 8u32)\n    core.check.eq(t.add(1u32, 1u32), 2u32)\nend t.tests.basic.test_modules_work_together\n";

fn int_pkg(w: &Work, test_text: &str) -> PathBuf {
    let root = w.path("t");
    make_pkg(&root, "t", true, "", &[("src/t.ksnl", LIB_SRC), ("src/t/sub.ksnl", SUB_SRC), ("tests/basic.ksnl", test_text)]);
    root
}

#[test]
fn integration_test_passes_and_runs_with_unit_tests() {
    let w = Work::new("int_pass");
    let root = int_pkg(&w, INT_OK);
    for args in [&["check"][..], &["fmt", "--check"][..]] {
        let o = w.ksn(args, &root);
        assert_eq!(o.status.code(), Some(0), "{:?}: {}", args, all(&o));
    }
    for args in [&["test"][..], &["test", "--both"][..]] {
        let o = w.ksn(args, &root);
        let out = all(&o);
        assert_eq!(o.status.code(), Some(0), "{:?}: {}", args, out);
        assert!(out.contains("test t.add.test_unit ... ok"), "{}", out);
        assert!(out.contains("test t.tests.basic.test_modules_work_together ... ok"), "{}", out);
        assert!(out.contains("2 passed; 0 failed"), "{}", out);
    }
}

#[test]
fn integration_test_failure_is_reported() {
    let w = Work::new("int_fail");
    let bad = INT_OK.replace("core.check.eq(t.add(1u32, 1u32), 2u32)", "core.check.eq(t.add(1u32, 1u32), 3u32)");
    let root = int_pkg(&w, &bad);
    let o = w.ksn(&["test"], &root);
    let out = all(&o);
    assert_eq!(o.status.code(), Some(1), "{}", out);
    assert!(out.contains("test t.tests.basic.test_modules_work_together ... FAILED"), "{}", out);
    assert!(out.contains("1 passed; 1 failed"), "{}", out);
}

#[test]
fn prefix_filter_selects_unit_or_integration_tests() {
    let w = Work::new("int_filter");
    let root = int_pkg(&w, INT_OK);
    let o = w.ksn(&["test", "t.tests"], &root);
    let out = all(&o);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert!(out.contains("t.tests.basic.test_modules_work_together") && !out.contains("t.add.test_unit"), "{}", out);
    let o = w.ksn(&["test", "t.add"], &root);
    let out = all(&o);
    assert!(out.contains("t.add.test_unit") && !out.contains("t.tests.basic"), "{}", out);
    let o = w.ksn(&["test", "t.tests.basic.test_modules"], &root);
    assert!(all(&o).contains("1 passed; 0 failed"), "{}", all(&o));
}

#[test]
fn integration_test_cannot_reference_private_items() {
    let w = Work::new("int_private");
    let bad = INT_OK.replace("t.add(1u32, 1u32), 2u32", "t.secret(), 7u32");
    let root = int_pkg(&w, &bad);
    let o = w.ksn(&["check"], &root);
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(code_in(&o, "E1119") && all(&o).contains("tests/basic.ksnl"), "{}", all(&o));
    // `ksn test` も走らない。
    assert_eq!(w.ksn(&["test"], &root).status.code(), Some(1));
}

#[test]
fn private_items_are_rejected_across_src_modules_too() {
    let w = Work::new("src_private");
    let root = w.path("t");
    let main = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.eq(t.sub.hidden(), 1u32)\n    std.io.print_line(\"x\")\nend t.main\n";
    let sub = "fn t.sub.hidden() -> u32 uses [] panics never\n    1u32\nend t.sub.hidden\n";
    make_pkg(&root, "t", true, "", &[("src/t.ksnl", main), ("src/t/sub.ksnl", sub)]);
    let o = w.ksn(&["check"], &root);
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(code_in(&o, "E1119"), "{}", all(&o));
    // `pub` を付ければ通る。
    write(&root.join("src/t/sub.ksnl"), &sub.replace("never\n", "never pub\n"));
    assert_eq!(w.ksn(&["check"], &root).status.code(), Some(0));
}

#[test]
fn pub_helper_in_tests_is_rejected() {
    let w = Work::new("int_pub");
    let bad = INT_OK.replace("panics never\n    t.sub.twice", "panics never pub\n    t.sub.twice");
    let root = int_pkg(&w, &bad);
    let o = w.ksn(&["check"], &root);
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(code_in(&o, "E1120"), "{}", all(&o));
}

#[test]
fn integration_test_must_be_named_after_its_module() {
    let w = Work::new("int_name");
    // 対象が定義 (t.add) になっている / 別のファイルのモジュール名になっている。
    for wrong in ["t.add", "t.tests.other"] {
        let bad = INT_OK.replace("test t.tests.basic.test_modules_work_together", &format!("test {}.test_modules_work_together", wrong)).replace(
            "end t.tests.basic.test_modules_work_together",
            &format!("end {}.test_modules_work_together", wrong),
        );
        let root = int_pkg(&w, &bad);
        let o = w.ksn(&["check"], &root);
        assert_eq!(o.status.code(), Some(1), "{}: {}", wrong, all(&o));
        assert!(code_in(&o, "E1116"), "{}: {}", wrong, all(&o));
    }
}

#[test]
fn src_modules_must_not_use_the_reserved_tests_namespace() {
    let w = Work::new("reserved");
    let root = w.path("t");
    let m = "fn t.tests.x() -> u32 uses [] panics never pub\n    1u32\nend t.tests.x\n";
    make_pkg(&root, "t", false, "", &[("src/t/tests/x.ksnl", m)]);
    let o = w.ksn(&["check"], &root);
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(code_in(&o, "E1121"), "{}", all(&o));
}

#[test]
fn tests_dir_is_checked_and_formatted() {
    let w = Work::new("int_fmt");
    // 構文エラー
    let root = int_pkg(&w, "test t.tests.basic.test_x\n    core.check.eq(1u32 2u32)\nend t.tests.basic.test_x\n");
    let o = w.ksn(&["check"], &root);
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(all(&o).contains("tests/basic.ksnl"), "{}", all(&o));
    // 整形の差分 (末尾の空白は check のエラー、import の順は fmt が直す)
    let root = int_pkg(&w, &format!("import t.sub\nimport t\n\n{}", INT_OK));
    let o = w.ksn(&["fmt", "--check"], &root);
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(all(&o).contains("tests/basic.ksnl"), "{}", all(&o));
}

#[test]
fn release_build_excludes_tests_and_native_build_ignores_test_helpers() {
    let w = Work::new("int_release");
    let root = int_pkg(&w, INT_OK);
    let o = w.ksn(&["build", "--release"], &root);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    // ネイティブ実行ファイルができたときは、同梱物 (pkg/) を書かない。あっても (退避) tests/ は入らない。
    let pk = root.join("target/release/pkg");
    assert!(!pk.exists(), "release に同梱物は無い");
    assert!(!pk.join("tests").exists(), "release bundle must not contain tests/");
    let bin = root.join("target/release/t.native");
    assert!(bin.exists(), "{}", all(&o));
    let r = Command::new(&bin).output().unwrap();
    assert!(String::from_utf8_lossy(&r.stdout).contains("t ok"));
    // デバッグの同梱物には、`--both` が使うので tests/ が入る。
    let o = w.ksn(&["build"], &root);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    assert!(root.join("target/debug/pkg/tests/basic.ksnl").exists());
}

#[test]
fn both_does_not_reuse_a_stale_bundle() {
    let w = Work::new("int_stale");
    let root = int_pkg(&w, INT_OK);
    assert_eq!(w.ksn(&["test", "--both"], &root).status.code(), Some(0));
    // テストを足して再実行すると、新しいテストも同梱物で走る (古い同梱物のまま「食い違い」にならない)。
    let more = format!("{}\ntest t.tests.basic.test_second\n    core.check.eq(t.add(2u32, 2u32), 4u32)\nend t.tests.basic.test_second\n", INT_OK);
    write(&root.join("tests/basic.ksnl"), &more);
    let o = w.ksn(&["test", "--both"], &root);
    let out = all(&o);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert!(out.contains("test t.tests.basic.test_second ... ok"), "{}", out);
}

#[test]
fn dependency_tests_dir_is_not_loaded() {
    let w = Work::new("dep_tests");
    let lib = w.path("lib");
    make_pkg(&lib, "pl", false, "", &[("src/pl.ksnl", "fn pl.one() -> u32 uses [] panics never pub\n    1u32\nend pl.one\n"), ("tests/broken.ksnl", "this is not kasane\n")]);
    let app = w.path("app");
    let main = "fn app.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.eq(pl.one(), 1u32)\n    std.io.print_line(\"ok\")\nend app.main\n";
    make_pkg(&app, "app", true, "pl = { path = \"../lib\" }\n", &[("src/app.ksnl", main)]);
    assert_eq!(w.ksn(&["update"], &app).status.code(), Some(0));
    let o = w.ksn(&["check"], &app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    assert_eq!(w.ksn(&["run"], &app).status.code(), Some(0));
}

// ------------------------------------------------------------ 依存パッケージ

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

/// `files` を置いたローカルの git リポジトリを作り、(`file://` の URL, コミットの 40 桁 hex) を返す。
fn make_git_pkg(root: &Path, name: &str, deps: &str, files: &[(&str, &str)]) -> (String, String) {
    fs::create_dir_all(root).unwrap();
    write(&root.join("kasane.toml"), &manifest(name, false, deps));
    for (rel, text) in files {
        write(&root.join(rel), text);
    }
    git(root, &["init", "--quiet"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "--quiet", "-m", "init"]);
    let rev = git(root, &["rev-parse", "HEAD"]);
    (format!("file://{}", root.to_string_lossy().replace('\\', "/")), rev)
}

const GL_SRC: &str = "fn gl.double(x: u32) -> u32 uses [] panics never pub\n    x.wrapping_mul(2u32)\nend gl.double\n\ntest gl.double.test_two\n    core.check.eq(gl.double(2u32), 4u32)\nend gl.double.test_two\n";
const PL_SRC: &str = "fn pl.inc(x: u32) -> u32 uses [] panics never pub\n    x.wrapping_add(1u32)\nend pl.inc\n";
const APP_SRC: &str = "fn app.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.eq(gl.double(pl.inc(20u32)), 42u32)\n    std.io.print_line(\"deps ok\")\nend app.main\n\ntest app.main.test_uses_both_dependencies\n    core.check.eq(pl.inc(gl.double(1u32)), 3u32)\nend app.main.test_uses_both_dependencies\n";

/// 8.1 のハッシュの独立な再計算 (処理系のコードは使わない)。
fn spec_hash(root: &Path) -> String {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().map_or(false, |x| x == "ksnl") {
                out.push(p);
            }
        }
    }
    let mut files: Vec<String> = vec!["kasane.toml".to_string()];
    let mut found = vec![];
    walk(&root.join("src"), &mut found);
    for p in found {
        files.push(p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
    }
    files.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let mut cat: Vec<u8> = vec![];
    for rel in &files {
        let data = fs::read(root.join(rel)).unwrap();
        cat.extend_from_slice(rel.as_bytes());
        cat.push(0);
        cat.extend_from_slice(&(data.len() as u64).to_le_bytes());
        cat.extend_from_slice(&data);
    }
    let d = Sha256::digest(&cat);
    format!("sha256:{}", d.iter().map(|b| format!("{:02x}", b)).collect::<String>())
}

/// app (path 依存 pl + git 依存 gl) を作って `ksn update` まで済ませる。(作業, app のルート, git の URL, rev, gl のリポジトリ)
fn app_with_deps(name: &str) -> (Work, PathBuf, String, String, PathBuf) {
    let w = Work::new(name);
    let gl_repo = w.path("gl_repo");
    // ファイル名の並びが自明でないように、複数ファイル (サブディレクトリ込み) にする。
    let (url, rev) = make_git_pkg(
        &gl_repo,
        "gl",
        "",
        &[("src/gl.ksnl", GL_SRC), ("src/gl/extra.ksnl", "fn gl.extra.one() -> u32 uses [] panics never pub\n    1u32\nend gl.extra.one\n"), ("README.md", "not hashed\n")],
    );
    make_pkg(&w.path("pl"), "pl", false, "", &[("src/pl.ksnl", PL_SRC)]);
    let app = w.path("app");
    let deps = format!("gl = {{ git = \"{}\", rev = \"{}\" }}\npl = {{ path = \"../pl\" }}\n", url, rev);
    make_pkg(&app, "app", true, &deps, &[("src/app.ksnl", APP_SRC)]);
    let o = w.ksn(&["update"], &app);
    assert_eq!(o.status.code(), Some(0), "update: {}", all(&o));
    (w, app, url, rev, gl_repo)
}

#[test]
fn path_and_git_dependencies_work_end_to_end() {
    let (w, app, url, rev, gl_repo) = app_with_deps("e2e");
    // kasane.lock: source と hash
    let lock: toml::Value = fs::read_to_string(app.join("kasane.lock")).unwrap().parse().unwrap();
    assert_eq!(lock["version"].as_integer(), Some(3));
    let pk = lock["package"].as_array().unwrap();
    let find = |n: &str| pk.iter().find(|p| p["name"].as_str() == Some(n)).unwrap();
    assert_eq!(find("pl")["source"].as_str(), Some("path+../pl"));
    assert!(find("pl").get("hash").is_none(), "path dependencies have no hash");
    assert_eq!(find("gl")["source"].as_str(), Some(format!("git+{}#{}", url, rev).as_str()));
    // 8.1 の算出式をテスト側で独立に再計算して一致を確かめる。
    let expect = spec_hash(&gl_repo);
    assert_eq!(find("gl")["hash"].as_str(), Some(expect.as_str()));
    assert!(expect.starts_with("sha256:") && expect.len() == "sha256:".len() + 64);
    // README など対象外のファイルは、ハッシュに影響しない。
    fs::write(gl_repo.join("README.md"), "changed but not hashed\n").unwrap();
    assert_eq!(spec_hash(&gl_repo), expect);
    // check / fmt / build / test / run が依存を使う。
    for args in [&["check"][..], &["fmt", "--check"][..]] {
        let o = w.ksn(args, &app);
        assert_eq!(o.status.code(), Some(0), "{:?}: {}", args, all(&o));
    }
    let o = w.ksn(&["test", "--both"], &app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    assert!(all(&o).contains("1 passed; 0 failed"), "{}", all(&o));
    let o = w.ksn(&["run"], &app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    assert!(all(&o).contains("deps ok"));
    let o = w.ksn(&["build"], &app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    let r = Command::new(app.join("target/debug/app.native")).output().unwrap();
    assert!(String::from_utf8_lossy(&r.stdout).contains("deps ok"), "{}", all(&o));
    // 同梱物 (インタプリタ) も、path 依存と git 依存の両方を持って動く。
    let r = Command::new(app.join("target/debug/app")).output().unwrap();
    assert!(String::from_utf8_lossy(&r.stdout).contains("deps ok"), "{}", String::from_utf8_lossy(&r.stderr));
}

#[test]
fn update_is_stable_and_check_fails_without_a_matching_lock() {
    let (w, app, ..) = app_with_deps("lock");
    let first = fs::read_to_string(app.join("kasane.lock")).unwrap();
    assert_eq!(w.ksn(&["update"], &app).status.code(), Some(0));
    assert_eq!(fs::read_to_string(app.join("kasane.lock")).unwrap(), first, "update is deterministic");
    // ロックなし (依存あり) → E9020
    fs::remove_file(app.join("kasane.lock")).unwrap();
    let o = w.ksn(&["check"], &app);
    assert_eq!(o.status.code(), Some(1));
    assert!(code_in(&o, "E9020"), "{}", all(&o));
    assert!(!app.join("kasane.lock").exists(), "check must not create the lock");
    // 依存の欠落 / 余分 / source・deps の食い違い → E9021
    let mutate = |from: &str, to: &str| {
        fs::write(app.join("kasane.lock"), first.replace(from, to)).unwrap();
        let o = w.ksn(&["check"], &app);
        assert_eq!(o.status.code(), Some(1), "{} -> {}: {}", from, to, all(&o));
        assert!(code_in(&o, "E9021"), "{} -> {}: {}", from, to, all(&o));
    };
    mutate("path+../pl", "path+../elsewhere");
    mutate("deps = []", "deps = [\"zz\"]");
    mutate("name = \"pl\"", "name = \"pq\"");
    mutate("version = 3", "version = 4");
    // 元に戻せば通る。
    fs::write(app.join("kasane.lock"), &first).unwrap();
    assert_eq!(w.ksn(&["check"], &app).status.code(), Some(0));
}

#[test]
fn tampered_cache_or_vendor_fails_the_hash_check() {
    let (w, app, ..) = app_with_deps("tamper");
    // update はキャッシュに取得している。
    let gitdir = w.cache.join("git");
    let entry = fs::read_dir(&gitdir).unwrap().next().unwrap().unwrap().path();
    assert!(entry.join("src/gl.ksnl").exists() && !entry.join(".git").exists());
    assert_eq!(w.ksn(&["check"], &app).status.code(), Some(0));
    // キャッシュの改ざん → E9022
    let f = entry.join("src/gl.ksnl");
    let orig = fs::read_to_string(&f).unwrap();
    fs::write(&f, orig.replace("2u32", "3u32")).unwrap();
    let o = w.ksn(&["check"], &app);
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(code_in(&o, "E9022"), "{}", all(&o));
    for sub in ["build", "test"] {
        let o = w.ksn(&[sub], &app);
        assert_eq!(o.status.code(), Some(1), "{}: {}", sub, all(&o));
        assert!(code_in(&o, "E9022"), "{}: {}", sub, all(&o));
    }
    fs::write(&f, &orig).unwrap();
    assert_eq!(w.ksn(&["check"], &app).status.code(), Some(0));
    // ファイルの追加も改ざん (ハッシュ対象の *.ksnl)
    write(&entry.join("src/gl/evil.ksnl"), "fn gl.evil.x() -> u32 uses [] panics never pub\n    1u32\nend gl.evil.x\n");
    assert!(code_in(&w.ksn(&["check"], &app), "E9022"));
    fs::remove_file(entry.join("src/gl/evil.ksnl")).unwrap();
    // kasane.toml の改ざん
    let tf = entry.join("kasane.toml");
    let t0 = fs::read_to_string(&tf).unwrap();
    fs::write(&tf, t0.replace("0.1.0", "0.1.1")).unwrap();
    assert!(code_in(&w.ksn(&["check"], &app), "E9022"));
    fs::write(&tf, t0).unwrap();
    // vendor したコピーの改ざん → E9022。`ksn vendor` で戻る。
    assert_eq!(w.ksn(&["vendor"], &app).status.code(), Some(0));
    let vf = app.join("vendor/gl/src/gl.ksnl");
    assert!(vf.exists() && !app.join("vendor/gl/.git").exists());
    assert_eq!(w.ksn(&["check"], &app).status.code(), Some(0));
    fs::write(&vf, fs::read_to_string(&vf).unwrap().replace("2u32", "5u32")).unwrap();
    let o = w.ksn(&["check"], &app);
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(code_in(&o, "E9022"), "{}", all(&o));
    assert_eq!(w.ksn(&["vendor"], &app).status.code(), Some(0));
    assert_eq!(w.ksn(&["check"], &app).status.code(), Some(0));
}

#[test]
fn vendor_copies_git_dependencies_and_builds_work_without_the_repository() {
    let (w, app, _url, _rev, gl_repo) = app_with_deps("vendor");
    let o = w.ksn(&["vendor"], &app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    assert!(app.join("vendor/gl/src/gl.ksnl").exists());
    assert!(!app.join("vendor/pl").exists(), "path dependencies are not vendored");
    // 元のリポジトリもキャッシュも消す。
    fs::remove_dir_all(&gl_repo).unwrap();
    fs::remove_dir_all(w.cache.join("git")).unwrap();
    for args in [&["check"][..], &["test", "--both"][..], &["build"][..], &["run"][..]] {
        let o = w.ksn(args, &app);
        assert_eq!(o.status.code(), Some(0), "{:?}: {}", args, all(&o));
    }
}

#[test]
fn git_dependency_without_vendor_or_cache_is_an_error_and_never_hits_the_network() {
    let (w, app, ..) = app_with_deps("offline");
    fs::remove_dir_all(w.cache.join("git")).unwrap();
    let o = w.ksn(&["check"], &app);
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(code_in(&o, "E9010"), "{}", all(&o));
}

#[test]
fn git_fetch_failures_are_reported() {
    let w = Work::new("badgit");
    let app = w.path("app");
    // 存在しないリポジトリ
    let deps = format!("gl = {{ git = \"file://{}\", rev = \"{}\" }}\n", w.path("nowhere").to_string_lossy(), "0".repeat(40));
    make_pkg(&app, "app", false, &deps, &[("src/app.ksnl", "const app.X: u32 = 1u32 pub\n")]);
    let o = w.ksn(&["update"], &app);
    assert_eq!(o.status.code(), Some(2), "{}", all(&o));
    assert!(code_in(&o, "E9018"), "{}", all(&o));
    // リポジトリはあるが rev が無い
    let (url, _rev) = make_git_pkg(&w.path("gl_repo"), "gl", "", &[("src/gl.ksnl", GL_SRC)]);
    let deps = format!("gl = {{ git = \"{}\", rev = \"{}\" }}\n", url, "1".repeat(40));
    write(&app.join("kasane.toml"), &manifest("app", false, &deps));
    let o = w.ksn(&["update"], &app);
    assert_eq!(o.status.code(), Some(2), "{}", all(&o));
    assert!(code_in(&o, "E9018"), "{}", all(&o));
    // URL がオプションに見える・rev が 40 桁でない → マニフェストの検査で止まる。
    let deps = format!("gl = {{ git = \"--upload-pack=touch pwned\", rev = \"{}\" }}\n", "1".repeat(40));
    write(&app.join("kasane.toml"), &manifest("app", false, &deps));
    let o = w.ksn(&["update"], &app);
    assert_eq!(o.status.code(), Some(2), "{}", all(&o));
    assert!(code_in(&o, "E9008"), "{}", all(&o));
    let deps = format!("gl = {{ git = \"{}\", rev = \"main\" }}\n", url);
    write(&app.join("kasane.toml"), &manifest("app", false, &deps));
    assert!(code_in(&w.ksn(&["update"], &app), "E9008"));
}

#[test]
fn update_with_a_name_refreshes_only_that_dependency() {
    let (w, app, ..) = app_with_deps("update_one");
    let first = fs::read_to_string(app.join("kasane.lock")).unwrap();
    // 既存のロックの gl のハッシュを書き換えておくと、`update pl` では触らず、`update gl` では直る。
    let fake = format!("sha256:{}", "ab".repeat(32));
    let hash_line = first.lines().find(|l| l.starts_with("hash = ")).unwrap().to_string();
    let broken = first.replace(&hash_line, &format!("hash = \"{}\"", fake));
    fs::write(app.join("kasane.lock"), &broken).unwrap();
    assert_eq!(w.ksn(&["update", "pl"], &app).status.code(), Some(0));
    assert!(fs::read_to_string(app.join("kasane.lock")).unwrap().contains(&fake));
    assert_eq!(w.ksn(&["update", "gl"], &app).status.code(), Some(0));
    assert_eq!(fs::read_to_string(app.join("kasane.lock")).unwrap(), first);
    let o = w.ksn(&["update", "nope"], &app);
    assert_eq!(o.status.code(), Some(2), "{}", all(&o));
    assert!(code_in(&o, "E9017"), "{}", all(&o));
}

#[test]
fn diamond_dependencies_with_the_same_source_are_accepted() {
    let w = Work::new("diamond_ok");
    make_pkg(&w.path("c"), "cc", false, "", &[("src/cc.ksnl", "fn cc.one() -> u32 uses [] panics never pub\n    1u32\nend cc.one\n")]);
    for n in ["aa", "bb"] {
        let src = format!("fn {0}.get() -> u32 uses [] panics never pub\n    cc.one()\nend {0}.get\n", n);
        make_pkg(&w.path(n), n, false, "cc = { path = \"../c\" }\n", &[(&format!("src/{}.ksnl", n), &src)]);
    }
    let main = "fn app.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.eq(aa.get(), bb.get())\n    std.io.print_line(\"diamond ok\")\nend app.main\n";
    let app = w.path("app");
    make_pkg(&app, "app", true, "aa = { path = \"../aa\" }\nbb = { path = \"../bb\" }\n", &[("src/app.ksnl", main)]);
    let o = w.ksn(&["update"], &app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    let lock = fs::read_to_string(app.join("kasane.lock")).unwrap();
    assert_eq!(lock.matches("name = \"cc\"").count(), 1, "{}", lock);
    assert!(lock.contains("source = \"path+../c\""), "{}", lock);
    let o = w.ksn(&["run"], &app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    assert!(all(&o).contains("diamond ok"));
}

#[test]
fn diamond_dependencies_with_different_sources_are_rejected() {
    let w = Work::new("diamond_bad");
    let cc = "fn cc.one() -> u32 uses [] panics never pub\n    1u32\nend cc.one\n";
    make_pkg(&w.path("c1"), "cc", false, "", &[("src/cc.ksnl", cc)]);
    make_pkg(&w.path("c2"), "cc", false, "", &[("src/cc.ksnl", cc)]);
    for (n, c) in [("aa", "c1"), ("bb", "c2")] {
        let src = format!("fn {0}.get() -> u32 uses [] panics never pub\n    cc.one()\nend {0}.get\n", n);
        make_pkg(&w.path(n), n, false, &format!("cc = {{ path = \"../{}\" }}\n", c), &[(&format!("src/{}.ksnl", n), &src)]);
    }
    let app = w.path("app");
    make_pkg(&app, "app", false, "aa = { path = \"../aa\" }\nbb = { path = \"../bb\" }\n", &[("src/app.ksnl", "const app.X: u32 = 1u32 pub\n")]);
    for sub in ["update", "check"] {
        let o = w.ksn(&[sub], &app);
        assert_ne!(o.status.code(), Some(0), "{}: {}", sub, all(&o));
        assert!(code_in(&o, "E9014"), "{}: {}", sub, all(&o));
    }
}

#[test]
fn git_dependency_with_a_path_dependency_is_rejected() {
    let w = Work::new("git_path");
    make_pkg(&w.path("pl"), "pl", false, "", &[("src/pl.ksnl", PL_SRC)]);
    let (url, rev) = make_git_pkg(&w.path("gl_repo"), "gl", "pl = { path = \"../pl\" }\n", &[("src/gl.ksnl", GL_SRC)]);
    let app = w.path("app");
    make_pkg(&app, "app", false, &format!("gl = {{ git = \"{}\", rev = \"{}\" }}\n", url, rev), &[("src/app.ksnl", "const app.X: u32 = 1u32 pub\n")]);
    let o = w.ksn(&["update"], &app);
    assert_eq!(o.status.code(), Some(2), "{}", all(&o));
    assert!(code_in(&o, "E9013"), "{}", all(&o));
}

#[test]
fn git_dependency_of_a_git_dependency_is_vendored_flat() {
    let w = Work::new("git_git");
    let (u2, r2) = make_git_pkg(&w.path("inner_repo"), "inner", "", &[("src/inner.ksnl", "fn inner.k() -> u32 uses [] panics never pub\n    5u32\nend inner.k\n")]);
    let outer_src = "fn outer.k() -> u32 uses [] panics never pub\n    inner.k()\nend outer.k\n";
    let (u1, r1) = make_git_pkg(&w.path("outer_repo"), "outer", &format!("inner = {{ git = \"{}\", rev = \"{}\" }}\n", u2, r2), &[("src/outer.ksnl", outer_src)]);
    let app = w.path("app");
    let main = "fn app.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.eq(outer.k(), 5u32)\n    std.io.print_line(\"nested ok\")\nend app.main\n";
    make_pkg(&app, "app", true, &format!("outer = {{ git = \"{}\", rev = \"{}\" }}\n", u1, r1), &[("src/app.ksnl", main)]);
    assert_eq!(w.ksn(&["update"], &app).status.code(), Some(0));
    let lock = fs::read_to_string(app.join("kasane.lock")).unwrap();
    assert!(lock.contains("name = \"inner\"") && lock.contains("deps = [\"inner\"]"), "{}", lock);
    assert_eq!(w.ksn(&["vendor"], &app).status.code(), Some(0));
    assert!(app.join("vendor/outer").is_dir() && app.join("vendor/inner").is_dir());
    fs::remove_dir_all(w.path("inner_repo")).unwrap();
    fs::remove_dir_all(w.path("outer_repo")).unwrap();
    fs::remove_dir_all(w.cache.join("git")).unwrap();
    let o = w.ksn(&["run"], &app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    assert!(all(&o).contains("nested ok"));
}

#[cfg(unix)]
#[test]
fn symlinks_in_a_git_dependency_are_rejected() {
    let w = Work::new("symlink");
    let repo = w.path("gl_repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    write(&repo.join("real.ksnl"), "fn gl.x() -> u32 uses [] panics never pub\n    1u32\nend gl.x\n");
    std::os::unix::fs::symlink("../real.ksnl", repo.join("src/gl.ksnl")).unwrap();
    let (url, rev) = make_git_pkg(&repo, "gl", "", &[]);
    let app = w.path("app");
    make_pkg(&app, "app", false, &format!("gl = {{ git = \"{}\", rev = \"{}\" }}\n", url, rev), &[("src/app.ksnl", "const app.X: u32 = 1u32 pub\n")]);
    let o = w.ksn(&["update"], &app);
    assert_eq!(o.status.code(), Some(2), "{}", all(&o));
    assert!(code_in(&o, "E9016"), "{}", all(&o));
}

#[test]
fn dependency_name_must_match_the_package_name_and_the_root_must_not_be_a_dependency() {
    let w = Work::new("names");
    make_pkg(&w.path("lib"), "other", false, "", &[("src/other.ksnl", "const other.X: u32 = 1u32 pub\n")]);
    let app = w.path("app");
    make_pkg(&app, "app", false, "wanted = { path = \"../lib\" }\n", &[("src/app.ksnl", "const app.X: u32 = 1u32 pub\n")]);
    let o = w.ksn(&["check"], &app);
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(code_in(&o, "E9011"), "{}", all(&o));
    // 循環: app -> lib2 -> app
    make_pkg(&w.path("lib2"), "lib2", false, "app = { path = \"../app\" }\n", &[("src/lib2.ksnl", "const lib2.X: u32 = 1u32 pub\n")]);
    make_pkg(&app, "app", false, "lib2 = { path = \"../lib2\" }\n", &[("src/app.ksnl", "const app.X: u32 = 1u32 pub\n")]);
    let o = w.ksn(&["update"], &app);
    assert_eq!(o.status.code(), Some(2), "{}", all(&o));
    assert!(code_in(&o, "E9015"), "{}", all(&o));
}

// ------------------------------------------------------------ examples

#[test]
fn two_package_example_and_shortener_pass_all_commands() {
    let ex = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("examples");
    let w = Work::new("examples");
    for name in ["kvlib", "kvapp", "shortener"] {
        let d = ex.join(name);
        for args in [&["check"][..], &["fmt", "--check"][..], &["test", "--both"][..]] {
            let o = w.ksn(args, &d);
            assert_eq!(o.status.code(), Some(0), "{} {:?}: {}", name, args, all(&o));
        }
    }
    let app = ex.join("kvapp");
    let o = w.ksn(&["run"], &app);
    assert!(all(&o).contains("kvapp ok"), "{}", all(&o));
    let o = w.ksn(&["build"], &app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    let r = Command::new(app.join("target/debug/kvapp.native")).output().unwrap();
    assert!(String::from_utf8_lossy(&r.stdout).contains("kvapp ok"), "native build of kvapp: {}", all(&o));
    let o = w.ksn(&["test", "--both"], &ex.join("shortener"));
    assert!(all(&o).contains("35 passed; 0 failed"), "{}", all(&o));
}

// ------------------------------------------------------------ 可視性 (3.1): メソッド・フィールド・std.sys

const VIS_SUB: &str = "\
enum t.sub.Color derive [Debug, Clone, Copy, PartialEq]
    Red
end t.sub.Color

type t.sub.P pub
    x: u32
    y: u32 pub
end t.sub.P

trait t.sub.Tr
    fn tr_m(self: &Self) -> u32 uses [] panics never
end t.sub.Tr

impl t.sub.Tr for t.sub.P
fn t.sub.P.tr_m(self: &t.sub.P) -> u32 uses [] panics never impls t.sub.Tr
    self.x
end t.sub.P.tr_m

fn t.sub.P.make() -> t.sub.P uses [] panics never pub
    t.sub.P { x: 1u32, y: 2u32 }
end t.sub.P.make

fn t.sub.P.secret(self: &t.sub.P) -> u32 uses [] panics never
    self.x
end t.sub.P.secret

fn t.sub.P.shown(self: &t.sub.P) -> u32 uses [] panics never pub
    self.x
end t.sub.P.shown
";

fn vis_main(stmts: &str) -> String {
    format!(
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    let p = t.sub.P.make()\n{}    std.io.print_line(\"x\")\nend t.main\n",
        stmts
    )
}

/// 2 モジュールのパッケージを作って `ksn check` し、(終了コード, 出力) を返す。
fn vis_check(name: &str, main: &str, sub: &str) -> (i32, String) {
    let w = Work::new(name);
    let root = w.path("t");
    make_pkg(&root, "t", true, "", &[("src/t.ksnl", main), ("src/t/sub.ksnl", sub)]);
    // 書き方は 1 通り (E0020): 組み立てた入力は `ksn fmt` の出力にそろえる。
    let _ = w.ksn(&["fmt"], &root);
    let o = w.ksn(&["check"], &root);
    (o.status.code().unwrap_or(-1), all(&o))
}

#[test]
fn visibility_of_members_across_modules() {
    // 公開されたものだけを使うなら通る。
    let ok = vis_main("    core.check.eq(p.y, 2u32)\n    core.check.eq(p.shown(), 1u32)\n");
    let (rc, out) = vis_check("vis_ok", &ok, VIS_SUB);
    assert_eq!(rc, 0, "{}", out);
    // (本体の文, 期待するコード)
    let bad: &[(&str, &str)] = &[
        ("    core.check.eq(p.x, 1u32)\n", "E1122"),
        ("    let q = t.sub.P { x: 1u32, y: 2u32 }\n    core.check.eq(q.y, 2u32)\n", "E1122"),
        ("    match p {\n        t.sub.P { x: a, y: b } => core.check.eq(a, b)\n    }\n", "E1122"),
        ("    core.check.eq(p.secret(), 1u32)\n", "E1119"),
        // `impls` メソッドはトレイトが非 `pub` なので別モジュールからは呼べない。
        ("    core.check.eq(p.tr_m(), 1u32)\n", "E1119"),
        // バリアントは enum の可視性に従う。
        ("    core.check.eq(t.sub.Color.Red == t.sub.Color.Red, true)\n", "E1119"),
    ];
    for (k, (stmts, code)) in bad.iter().enumerate() {
        let (rc, out) = vis_check(&format!("vis_bad{}", k), &vis_main(stmts), VIS_SUB);
        assert_eq!(rc, 1, "{}: {}", stmts, out);
        assert!(out.contains(code), "{}: expected {}: {}", stmts, code, out);
    }
    // `pub` を付ければ通る。
    let fixed = VIS_SUB
        .replace("    x: u32\n", "    x: u32 pub\n")
        .replace("-> u32 uses [] panics never\n    self.x\nend t.sub.P.secret", "-> u32 uses [] panics never pub\n    self.x\nend t.sub.P.secret");
    let main = vis_main("    core.check.eq(p.x, 1u32)\n    core.check.eq(p.secret(), 1u32)\n");
    let (rc, out) = vis_check("vis_fixed", &main, &fixed);
    assert_eq!(rc, 0, "{}", out);
}

#[test]
fn std_sys_is_not_referenceable_from_user_packages() {
    let main = "import std.sys.os\n\nfn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"x\")\nend t.main\n";
    let (rc, out) = vis_check("std_sys_import", main, "");
    assert_eq!(rc, 1, "{}", out);
    assert!(out.contains("E1124"), "{}", out);
    let main2 = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    std.sys.os.getpid()\n    std.io.print_line(\"x\")\nend t.main\n";
    let (rc, out) = vis_check("std_sys_fqn", main2, "");
    assert_eq!(rc, 1, "{}", out);
    assert!(out.contains("E1124"), "{}", out);
}
