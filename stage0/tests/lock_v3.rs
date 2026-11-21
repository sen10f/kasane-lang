//! `kasane.lock` 版 3: `source` は解決後の commit、`version`/`hash`/`deps` を記録、
//! `ksn lock --accept / --diff / --upgrade`。ネットワークには出ない (一時のローカルの git リポジトリ)。

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Work {
    dir: PathBuf,
    cache: PathBuf,
}
impl Work {
    fn new(name: &str) -> Work {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ksn_lockv3_{}_{}_{}", std::process::id(), name, n));
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
        Command::new(env!("CARGO_BIN_EXE_ksn")).args(args).current_dir(cwd).env("KSN_CACHE_DIR", &self.cache).output().expect("spawn ksn")
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

fn manifest(name: &str, entry: bool, deps: &str) -> String {
    let e = if entry { format!("entry = \"{}.main\"\n", name) } else { String::new() };
    format!("[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = 1\n{}\n[dependencies]\n{}", name, e, deps)
}

/// 依存 `gl` の 1 版目。
const GL1: &str = "fn gl.double(x: u32) -> u32 uses [] panics never pub\n    x.wrapping_mul(2u32)\nend gl.double\n\nfn gl.pi() -> u32 uses [io.console] panics may pub\n    core.mem.drop(std.io.print_line(\"x\"))\n    core.check.is_true(true)\n    3u32\nend gl.pi\n\nfn gl.old() -> u32 uses [] panics never pub\n    1u32\nend gl.old\n";
const APP: &str = "fn app.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.eq(gl.double(21u32), 42u32)\n    std.io.print_line(\"ok\")\nend app.main\n";

struct Fx {
    w: Work,
    app: PathBuf,
    repo: PathBuf,
    url: String,
}

fn setup(name: &str) -> Fx {
    let w = Work::new(name);
    let repo = w.path("gl_repo");
    write(&repo.join("kasane.toml"), &manifest("gl", false, ""));
    write(&repo.join("src/gl.ksnl"), GL1);
    let _ = w.ksn(&["fmt"], &repo);
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "-m", "v1"]);
    let rev = git(&repo, &["rev-parse", "HEAD"]);
    let url = format!("file://{}", repo.to_string_lossy());
    let app = w.path("app");
    write(&app.join("kasane.toml"), &manifest("app", true, &format!("gl = {{ git = \"{}\", rev = \"{}\" }}\n", url, rev)));
    write(&app.join("src/app.ksnl"), APP);
    let o = w.ksn(&["update"], &app);
    assert_eq!(o.status.code(), Some(0), "update: {}", all(&o));
    let _ = rev;
    Fx { w, app, repo, url }
}

impl Fx {
    /// 依存の新しい版 (ソースを差し替えてコミット) に上げる。`ksn update gl` は `hash` / `source` を更新するが `api` は保つ。
    fn bump(&self, src: &str) {
        write(&self.repo.join("src/gl.ksnl"), src);
        let _ = self.w.ksn(&["fmt"], &self.repo);
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "--quiet", "-m", "v2"]);
        let rev = git(&self.repo, &["rev-parse", "HEAD"]);
        write(&self.app.join("kasane.toml"), &manifest("app", true, &format!("gl = {{ git = \"{}\", rev = \"{}\" }}\n", self.url, rev)));
        let o = self.w.ksn(&["update", "gl"], &self.app);
        assert_eq!(o.status.code(), Some(0), "update gl: {}", all(&o));
    }
    fn check(&self) -> Output {
        self.w.ksn(&["check"], &self.app)
    }
    fn lock(&self) -> String {
        fs::read_to_string(self.app.join("kasane.lock")).unwrap()
    }
}

/// 版 3: `ksn update` は `version = 3` と git 依存の `version` を書く。
#[test]
fn update_writes_version_3_with_the_public_headers() {
    let f = setup("record");
    let lock = f.lock();
    assert!(lock.starts_with("version = 3\n"), "{}", lock);
    // git 依存には `version` (package.version) と `hash` が記録される。
    assert!(lock.contains("version = \"0.1.0\"\n"), "{}", lock);
    assert!(lock.contains("hash = \"sha256:"), "{}", lock);
    assert!(lock.contains("[[package.api]]") && lock.contains("fqn = \"gl.double\""), "{}", lock);
    assert_eq!(f.check().status.code(), Some(0), "{}", all(&f.check()));
    let first = lock.clone();
    assert_eq!(f.w.ksn(&["update"], &f.app).status.code(), Some(0));
    assert_eq!(f.lock(), first, "update is stable");
}

/// 版 3 のロックは `ksn fmt` の正規形 (fixpoint)。
#[test]
fn version_3_lock_is_canonical_fixpoint() {
    let f = setup("canon");
    let lock = f.lock();
    let o = f.w.ksn(&["fmt", "--check"], &f.app);
    assert_eq!(o.status.code(), Some(0), "lock is not canonical: {}", all(&o));
}

/// 版 1 のロックは、git 依存があるとき止まる (S4。版 3 から落として承認の関門を外せない)。
#[test]
fn version_1_locks_with_a_git_dependency_are_rejected() {
    let f = setup("v1read");
    let v1: String = f.lock()
        .lines()
        .take_while(|l| !l.starts_with("unsafe_modules"))
        .filter(|l| !l.starts_with("version = \""))
        .map(|l| format!("{}\n", l))
        .collect::<String>()
        .replacen("version = 3", "version = 1", 1);
    write(&f.app.join("kasane.lock"), &v1);
    // 版 1 は api の記録を持たず承認の関門 (E9060-E9062) を無効にするので、git 依存があるとき check は止める (S4)。
    let o = f.check();
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(all(&o).contains("E9021") && all(&o).contains("version 1"), "{}", all(&o));
}

/// 版 2 のロックは読める。
#[test]
fn version_2_locks_are_read() {
    let f = setup("v2read");
    let v2: String = f.lock()
        .lines()
        .filter(|l| !l.starts_with("unsafe_modules = "))
        .filter(|l| !l.starts_with("version = \""))
        .map(|l| format!("{}\n", l))
        .collect::<String>()
        .replacen("version = 3\n", "version = 2\n", 1);
    write(&f.app.join("kasane.lock"), &v2);
    assert_eq!(f.check().status.code(), Some(0), "{}", all(&f.check()));
}

/// 版 1 のロックを `--upgrade` で版 3 に上げる。
#[test]
fn version_1_locks_are_upgraded_to_version_3() {
    let f = setup("upgrade");
    let v1: String = f.lock()
        .lines()
        .take_while(|l| !l.starts_with("unsafe_modules"))
        .filter(|l| !l.starts_with("version = \""))
        .map(|l| format!("{}\n", l))
        .collect::<String>()
        .replacen("version = 3", "version = 1", 1);
    write(&f.app.join("kasane.lock"), &v1);
    // 版 1 は api の記録を持たず承認の関門 (E9060-E9062) を無効にするので、git 依存があるとき check は止める (S4)。
    let o = f.check();
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(all(&o).contains("E9021") && all(&o).contains("version 1"), "{}", all(&o));
    let o = f.w.ksn(&["lock", "--upgrade"], &f.app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    let lock = f.lock();
    assert!(lock.starts_with("version = 3\n") && lock.contains("fqn = \"gl.double\""), "{}", lock);
    assert_eq!(f.check().status.code(), Some(0));
}

/// 版 3 で `api` の記録が欠けていれば `ksn check` が止める。
#[test]
fn version_3_without_api_fails() {
    let f = setup("noapi");
    let v3_no_api: String = f.lock()
        .lines()
        .take_while(|l| !l.starts_with("unsafe_modules"))
        .map(|l| format!("{}\n", l))
        .collect::<String>();
    write(&f.app.join("kasane.lock"), &v3_no_api);
    let o = f.check();
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(all(&o).contains("E9021") && all(&o).contains("no recorded api"), "{}", all(&o));
}

/// E9060: 効果の広がりは承認前は止める (版 3 でも門は変わらない)。
#[test]
fn widened_effects_stop_with_e9060_until_accepted() {
    let f = setup("widen");
    f.bump(&GL1.replace("fn gl.double(x: u32) -> u32 uses [] panics never pub\n    x.wrapping_mul(2u32)", "fn gl.double(x: u32) -> u32 uses [io.console] panics never pub\n    core.mem.drop(std.io.print_line(\"x\"))\n    x.wrapping_mul(2u32)"));
    for sub in ["check", "build", "test", "run"] {
        let o = f.w.ksn(&[sub], &f.app);
        assert_eq!(o.status.code(), Some(1), "{}: {}", sub, all(&o));
        assert!(all(&o).contains("E9060"), "{}: {}", sub, all(&o));
    }
    // `--diff` は書かずに差分だけ。
    let before = f.lock();
    let o = f.w.ksn(&["lock", "--diff"], &f.app);
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(all(&o).contains("E9060"), "{}", all(&o));
    assert_eq!(f.lock(), before);
}

/// E9061: 署名の変化・消えたは承認前は止める。
#[test]
fn changed_or_removed_signatures_stop_with_e9061() {
    let f = setup("sigchange");
    f.bump(&GL1.replace("fn gl.double(x: u32) -> u32 uses [] panics never pub\n    x.wrapping_mul(2u32)", "fn gl.double(x: u64) -> u32 uses [] panics never pub\n    1u32"));
    let o = f.check();
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(all(&o).contains("E9061"), "{}", all(&o));
}

/// E9062: 狭まり・増えたは承認前は止める。
#[test]
fn narrowed_and_added_definitions_stop_with_e9062() {
    let f = setup("narrow");
    f.bump(&GL1.replace("fn gl.pi() -> u32 uses [io.console] panics may pub\n    core.mem.drop(std.io.print_line(\"x\"))\n    core.check.is_true(true)\n    3u32\nend gl.pi", "fn gl.pi() -> u32 uses [] panics never pub\n    3u32\nend gl.pi"));
    let o = f.check();
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(all(&o).contains("E9062"), "{}", all(&o));
}

/// path 依存は `api` を記録しない。
#[test]
fn path_dependencies_are_not_recorded() {
    let w = Work::new("path");
    let app = w.path("app");
    write(&app.join("kasane.toml"), &manifest("app", true, "pl = { path = \"../pl\" }\n"));
    write(&app.join("src/app.ksnl"), "fn app.main() -> u32 uses [] panics never pub\n    pl.one()\nend app.main\n");
    make_pl_pkg(&w);
    let o = w.ksn(&["update"], &app);
    assert_eq!(o.status.code(), Some(0), "update: {}", all(&o));
    let lock = fs::read_to_string(app.join("kasane.lock")).unwrap();
    assert!(lock.starts_with("version = 2\n"), "path だけの依存は版 2 のまま: {}", lock);
    assert!(!lock.contains("[[package.api]]"), "path 依存は api を記録しない: {}", lock);
    assert_eq!(w.ksn(&["check"], &app).status.code(), Some(0), "{}", all(&w.ksn(&["check"], &app)));
}

/// `ksn lock --accept` は差分 token なしでは受け付けない (E906x 門)。
#[test]
fn accept_requires_a_dependency() {
    let f = setup("accept");
    assert_eq!(f.w.ksn(&["lock", "--accept"], &f.app).status.code(), Some(2));
    assert_eq!(f.w.ksn(&["lock", "--accept", "nope"], &f.app).status.code(), Some(2));
}

fn make_pl_pkg(w: &Work) {
    let pl = w.path("pl");
    fs::create_dir_all(pl.join("src")).unwrap();
    write(&pl.join("kasane.toml"), &manifest("pl", false, ""));
    write(&pl.join("src/pl.ksnl"), "fn pl.one() -> u32 uses [] panics never pub\n    1u32\nend pl.one\n");
}
