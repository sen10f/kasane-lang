//! `kasane.lock` 版 2: 依存の公開の署名と効果の記録 (E9060 広がり / E9061 署名の変化・消えた / E9062 狭まり・増えた)、
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
        let dir = std::env::temp_dir().join(format!("ksn_lockv2_{}_{}_{}", std::process::id(), name, n));
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

/// 依存 `gl` の 1 版目。`double` は引数 1 つ、`pi` は `panics may`。
const GL1: &str = "fn gl.double(x: u32) -> u32 uses [] panics never pub\n    x.wrapping_mul(2u32)\nend gl.double\n\nfn gl.pi() -> u32 uses [alloc] panics may pub\n    3u32\nend gl.pi\n\nfn gl.old() -> u32 uses [] panics never pub\n    1u32\nend gl.old\n";
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
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "-m", "v1"]);
    let rev = git(&repo, &["rev-parse", "HEAD"]);
    let url = format!("file://{}", repo.to_string_lossy());
    let app = w.path("app");
    write(&app.join("kasane.toml"), &manifest("app", true, &format!("gl = {{ git = \"{}\", rev = \"{}\" }}\n", url, rev)));
    write(&app.join("kasane.lock"), "version = 1\n");
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

#[test]
fn update_writes_version_2_with_the_public_headers() {
    let f = setup("record");
    let lock = f.lock();
    assert!(lock.starts_with("version = 2\n"), "{}", lock);
    assert!(lock.contains("[[package.api]]") && lock.contains("fqn = \"gl.double\""), "{}", lock);
    assert!(lock.contains("fn gl.double(x: u32) -> u32 uses [] panics never pub"), "{}", lock);
    assert!(lock.contains("fn gl.pi() -> u32 uses [alloc] panics may pub"), "{}", lock);
    assert_eq!(f.check().status.code(), Some(0), "{}", all(&f.check()));
    let first = lock.clone();
    assert_eq!(f.w.ksn(&["update"], &f.app).status.code(), Some(0));
    assert_eq!(f.lock(), first, "update is stable");
}

#[test]
fn widened_effects_stop_with_e9060_until_accepted() {
    let f = setup("widen");
    f.bump(&GL1.replace("fn gl.double(x: u32) -> u32 uses [] panics never pub", "fn gl.double(x: u32) -> u32 uses [io.console] panics never pub"));
    for sub in ["check", "build", "test", "run"] {
        let o = f.w.ksn(&[sub], &f.app);
        assert_eq!(o.status.code(), Some(1), "{}: {}", sub, all(&o));
        let out = all(&o);
        assert!(out.contains("E9060") && !out.contains("E9061") && !out.contains("E9062"), "{}: {}", sub, out);
        assert!(out.contains("- fn gl.double") && out.contains("+ fn gl.double") && out.contains("uses [io.console]"), "{}: {}", sub, out);
    }
    // `--diff` は書かずに差分だけ。
    let before = f.lock();
    let o = f.w.ksn(&["lock", "--diff"], &f.app);
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(all(&o).contains("E9060"), "{}", all(&o));
    assert_eq!(f.lock(), before);
    // `panics never` → `may` も広がり。
    let o = f.w.ksn(&["lock", "--accept", "gl"], &f.app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    assert!(f.lock().contains("uses [io.console] panics never pub"));
    assert_eq!(f.check().status.code(), Some(0), "{}", all(&f.check()));
}

#[test]
fn panics_never_to_may_and_a_new_extern_are_widenings() {
    let f = setup("widen2");
    f.bump(&GL1.replace("fn gl.double(x: u32) -> u32 uses [] panics never pub", "fn gl.double(x: u32) -> u32 uses [] panics may pub"));
    let out = all(&f.check());
    assert!(out.contains("E9060"), "{}", out);
}

#[test]
fn changed_or_removed_signatures_stop_with_e9061() {
    let f = setup("sig");
    f.bump(&GL1.replace("fn gl.double(x: u32) -> u32 uses [] panics never pub\n    x.wrapping_mul(2u32)", "fn gl.double(x: u64) -> u32 uses [] panics never pub\n    1u32"));
    let o = f.check();
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    let out = all(&o);
    assert!(out.contains("E9061") && !out.contains("E9060"), "{}", out);
    assert_eq!(f.w.ksn(&["lock", "--accept", "gl"], &f.app).status.code(), Some(0));
    assert!(f.lock().contains("x: u64"));
    // 注: app が呼ぶ gl.double(21u32) は型が合わなくなるので、診断は E9061 の後の型の誤りになる。
    let g = setup("removed");
    g.bump(&GL1.replace("fn gl.old() -> u32 uses [] panics never pub\n    1u32\nend gl.old\n", ""));
    let out = all(&g.check());
    assert!(out.contains("E9061") && out.contains("gl.old") && out.contains("was removed"), "{}", out);
    assert_eq!(g.w.ksn(&["lock", "--accept", "gl"], &g.app).status.code(), Some(0));
    assert_eq!(g.check().status.code(), Some(0), "{}", all(&g.check()));
}

#[test]
fn narrowed_and_added_definitions_stop_with_e9062() {
    let f = setup("narrow");
    f.bump(&GL1.replace("fn gl.pi() -> u32 uses [alloc] panics may pub", "fn gl.pi() -> u32 uses [] panics never pub"));
    let out = all(&f.check());
    assert!(out.contains("E9062") && !out.contains("E9060") && !out.contains("E9061"), "{}", out);
    assert_eq!(f.w.ksn(&["lock", "--accept", "gl"], &f.app).status.code(), Some(0));
    assert_eq!(f.check().status.code(), Some(0));
    let g = setup("added");
    g.bump(&format!("{}\nfn gl.extra() -> u32 uses [] panics never pub\n    5u32\nend gl.extra\n", GL1));
    let o = g.check();
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    let out = all(&o);
    assert!(out.contains("E9062") && out.contains("gl.extra") && out.contains("was added"), "{}", out);
    // 非公開の関数・本体だけの変更は止めない。
    let h = setup("private");
    h.bump(&format!("{}\nfn gl.hidden() -> u32 uses [io] panics may\n    5u32\nend gl.hidden\n", GL1.replace("x.wrapping_mul(2u32)", "x.wrapping_add(x)")));
    assert_eq!(h.check().status.code(), Some(0), "{}", all(&h.check()));
}

#[test]
fn accept_requires_a_dependency_and_path_dependencies_are_not_recorded() {
    let f = setup("accept_args");
    assert_eq!(f.w.ksn(&["lock", "--accept"], &f.app).status.code(), Some(2));
    assert_eq!(f.w.ksn(&["lock", "--accept", "nope"], &f.app).status.code(), Some(2));
    // path 依存は記録しない (編集のたびに落ちない)。
    let pl = f.w.path("pl");
    write(&pl.join("kasane.toml"), &manifest("pl", false, ""));
    write(&pl.join("src/pl.ksnl"), "fn pl.one() -> u32 uses [] panics never pub\n    1u32\nend pl.one\n");
    let toml = fs::read_to_string(f.app.join("kasane.toml")).unwrap();
    write(&f.app.join("kasane.toml"), &format!("{}pl = {{ path = \"../pl\" }}\n", toml));
    assert_eq!(f.w.ksn(&["update"], &f.app).status.code(), Some(0));
    let lock = f.lock();
    assert!(!lock.contains("fqn = \"pl.one\""), "{}", lock);
    assert_eq!(f.w.ksn(&["lock", "--accept", "pl"], &f.app).status.code(), Some(2));
    write(&pl.join("src/pl.ksnl"), "fn pl.one() -> u32 uses [io] panics may pub\n    1u32\nend pl.one\n");
    assert_eq!(f.check().status.code(), Some(0), "{}", all(&f.check()));
}

#[test]
fn version_1_locks_are_read_and_upgraded() {
    let f = setup("upgrade");
    // 版 1 の形 (api なし) に戻す。
    let v1: String = f.lock().lines().take_while(|l| !l.starts_with("unsafe_modules")).map(|l| format!("{}\n", l)).collect::<String>().replacen("version = 2", "version = 1", 1);
    write(&f.app.join("kasane.lock"), &v1);
    assert_eq!(f.check().status.code(), Some(0), "{}", all(&f.check()));
    let o = f.w.ksn(&["lock", "--upgrade"], &f.app);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    let lock = f.lock();
    assert!(lock.starts_with("version = 2\n") && lock.contains("fqn = \"gl.double\""), "{}", lock);
    assert_eq!(f.check().status.code(), Some(0));
    // 版 2 で記録が欠けていれば止める。
    write(&f.app.join("kasane.lock"), &v1.replacen("version = 1", "version = 2", 1));
    let o = f.check();
    assert_eq!(o.status.code(), Some(1), "{}", all(&o));
    assert!(all(&o).contains("E9021") && all(&o).contains("no recorded api"), "{}", all(&o));
    // 初めての `vendor` でも記録される。
    write(&f.app.join("kasane.lock"), &v1);
    assert_eq!(f.w.ksn(&["vendor"], &f.app).status.code(), Some(0));
    assert!(f.lock().contains("fqn = \"gl.double\""), "{}", f.lock());
    assert_eq!(f.check().status.code(), Some(0));
}

#[test]
fn explain_knows_the_new_codes() {
    let f = setup("explain");
    for c in ["E9060", "E9061", "E9062"] {
        let o = f.w.ksn(&["explain", c], &f.app);
        assert_eq!(o.status.code(), Some(0), "{}: {}", c, all(&o));
        assert!(all(&o).contains("lock --accept"), "{}: {}", c, all(&o));
    }
}
