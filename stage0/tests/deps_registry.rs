//! レジストリ依存の解決・lock・update・yanked・ハッシュ不一致 (R3c)。
//! 索引と取得元パッケージはすべてローカルに置き、`https://example.org/` はこの
//! ワークツリーの bare リポジトリへ `git` の `insteadOf` で向ける（ネットワークには出ない）。
//! パッケージは `ksn publish` で索引に出版する。

mod common;

use common::*;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// 8.1 のパッケージハッシュ (`kasane.toml` と `src/**/*.ksnl` をパス順に枠付けして SHA-256)。
/// `publish` で索引に書く `hash` と一致させるため、本体の `try_package_hash` と同じ順序・枠で計算する。
fn package_hash(root: &Path) -> String {
    let mut files: Vec<String> = vec!["kasane.toml".to_string()];
    collect_ksnl(&root.join("src"), "src/", &mut files);
    files.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    files.dedup();
    let mut h = Sha256::new();
    for rel in files {
        let data = fs::read(root.join(&rel)).unwrap_or_default();
        h.update(rel.as_bytes());
        h.update([0u8]);
        h.update((data.len() as u64).to_le_bytes());
        h.update(&data);
    }
    let digest = h.finalize();
    format!("sha256:{}", digest.iter().map(|b| format!("{:02x}", b)).collect::<String>())
}

fn collect_ksnl(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            collect_ksnl(&path, &format!("{prefix}{name}/"), out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("ksnl") {
            out.push(format!("{prefix}{name}"));
        }
    }
}

/// 索引の bare レポジトリを一時ワークツリーに展開し、`index/foo.json` を `edit` で書き換えて
/// コミット・push する。URL レジストリは `ksn publish`/`ksn yank` が push しない（shortcut）ので、
/// 呼び出し側がここで bare へ反映する。
fn with_index<F>(w: &Work, mut edit: F)
where
    F: FnMut(&mut String),
{
    let bare = w.repos.join("registry.git");
    let wt = w.dir.join("work_index");
    let _ = fs::remove_dir_all(&wt);
    git(
        &w.dir,
        &["clone", "--quiet", bare.to_str().unwrap(), wt.to_str().unwrap()],
    );
    let p = wt.join("index").join("foo.json");
    let mut body = fs::read_to_string(&p).unwrap_or_default();
    if body.trim().is_empty() {
        body = "[]".to_string();
    }
    edit(&mut body);
    fs::write(&p, &body).unwrap();
    git(&wt, &["add", "index"]);
    git(&wt, &["commit", "--quiet", "-m", "index"]);
    git(&wt, &["push", "--quiet", "origin", "HEAD:main"]);
    let _ = fs::remove_dir_all(&wt);
}

const REG_URL: &str = "https://example.org/registry.git";
const APP_SRC: &str = "fn app.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"ok\")\nend app.main\n";
const PKG_SRC: &str = "fn foo.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend foo.main\n";

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// 1 試験ごとの作業ツリー。`repos` に bare リポジトリ（registry.git, pkg-*.git）を置き、
/// `gitconfig` で `https://example.org/` をそこへ `insteadOf` する。
struct Work {
    dir: PathBuf,
    cache: PathBuf,
    repos: PathBuf,
    gitconfig: PathBuf,
}

impl Work {
    fn new(name: &str) -> Work {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ksn_r3_{}_{}_{}", std::process::id(), name, n));
        let _ = fs::remove_dir_all(&dir);
        let cache = dir.join("cache");
        let repos = dir.join("repos");
        fs::create_dir_all(&repos).unwrap();
        let gitconfig = dir.join("gitconfig");
        // `https://example.org/` はこのワークツリー配下の bare リポジトリに向ける（オフライン）。
        fs::write(
            &gitconfig,
            format!(
                "[url \"{}/\"]\n\tinsteadOf = https://example.org/\n",
                repos.to_string_lossy()
            ),
        )
        .unwrap();
        Work { dir, cache, repos, gitconfig }
    }

    fn app(&self) -> PathBuf {
        self.dir.join("app")
    }

    fn ksn(&self, args: &[&str], cwd: &Path) -> Output {
        let cache = self.cache.to_string_lossy().to_string();
        let gitconfig = self.gitconfig.to_string_lossy().to_string();
        Command::new(exe())
            .args(args)
            .current_dir(cwd)
            .env("KSN_CACHE_DIR", &cache)
            .env("GIT_CONFIG_GLOBAL", &gitconfig)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.org")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.org")
            .output()
            .expect("spawn ksn")
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

fn git_env(w: &Work) -> ((&'static str, String), (&'static str, String)) {
    (
        ("KSN_CACHE_DIR", w.cache.to_string_lossy().to_string()),
        ("GIT_CONFIG_GLOBAL", w.gitconfig.to_string_lossy().to_string()),
    )
}

/// `index/<name>.json` に `body` を書いた索引の bare リポジトリを `<repos>/<name>.git` に作る。
fn make_registry(w: &Work, name: &str, body: &str) {
    let work = w.dir.join("work_registry");
    fs::create_dir_all(work.join("index")).unwrap();
    git(&work, &["init", "--quiet", "-b", "main"]);
    fs::write(work.join("index").join(format!("{name}.json")), body).unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "--quiet", "-m", "index"]);
    // 索引の URL (`https://example.org/registry.git`) は `insteadOf` で `<repos>/` 配下に向くので、
    // bare リポジトリは `registry.git` と名づける。
    let bare = w.repos.join("registry.git");
    let _ = fs::remove_dir_all(&bare);
    git(&w.dir, &["clone", "--quiet", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()]);
}

/// 出版可能な foo パッケージを `w.dir/<dir>` に作る。origin は `https://example.org/<dir>.git`
///（insteadOf で `<repos>/<dir>.git` へ向く）とし、`v<version>` を HEAD にタグする。
fn make_pkg(w: &Work, dir: &str, version: &str) -> PathBuf {
    let pkg = w.dir.join(dir);
    fs::create_dir_all(pkg.join("src")).unwrap();
    let toml = format!(
        "[package]\nname = \"foo\"\nversion = \"{version}\"\nedition = 1\nentry = \"foo.main\"\n\n[dependencies]\n",
    );
    fs::write(pkg.join("kasane.toml"), &toml).unwrap();
    fs::write(pkg.join("src").join("foo.ksnl"), PKG_SRC).unwrap();
    let o = Command::new(exe()).arg("fmt").current_dir(&pkg).output().expect("spawn ksn");
    assert!(o.status.success(), "pkg fmt failed: {}", text(&o.stderr));
    git(&pkg, &["init", "--quiet", "-b", "main"]);
    git(&pkg, &["add", "-A"]);
    git(&pkg, &["commit", "--quiet", "-m", "init"]);
    let bare = w.repos.join(format!("{dir}.git"));
    let _ = fs::remove_dir_all(&bare);
    git(&w.dir, &["clone", "--quiet", "--bare", pkg.to_str().unwrap(), bare.to_str().unwrap()]);
    git(&pkg, &["remote", "add", "origin", &format!("https://example.org/{dir}.git")]);
    git(&pkg, &["tag", &format!("v{version}")]);
    pkg
}

fn publish(pkg: &Path, version: &str, w: &Work) -> Output {
    let ((k1, v1), (k2, v2)) = git_env(w);
    let out = run_with_env(
        &["publish", "--registry", REG_URL],
        pkg,
        &[
            (k1, v1.as_str()),
            (k2, v2.as_str()),
            ("GIT_AUTHOR_NAME", "test"),
            ("GIT_AUTHOR_EMAIL", "test@example.org"),
            ("GIT_COMMITTER_NAME", "test"),
            ("GIT_COMMITTER_EMAIL", "test@example.org"),
        ],
    );
    assert!(out.status.success(), "publish failed: {}", all(&out));
    // URL レジストリは `ksn publish` が push しない（shortcut：コミットまで作って止める）ので、
    // ここで索引のコミットを bare リポジトリへ反映する（publish と同じエントリ内容）。
    let dir = pkg.file_name().unwrap().to_str().unwrap();
    let repository = format!("https://example.org/{dir}.git");
    let commit = git(&pkg, &["rev-parse", "HEAD"]);
    let hash = package_hash(pkg);
    let entry = format!(
        "{{\"version\":\"{version}\",\"tag\":\"v{version}\",\"repository\":\"{repository}\",\"commit\":\"{commit}\",\"hash\":\"{hash}\",\"owner\":\"{repository}\",\"yanked\":false}}"
    );
    with_index(w, |body| {
        let end = body.rfind(']').unwrap();
        body.truncate(end);
        if body.ends_with('[') {
            body.push_str(&entry);
        } else {
            body.push_str(&format!(",{entry}"));
        }
        body.push(']');
    });
    out
}

fn yank(w: &Work, name: &str, version: &str) -> Output {
    let ((k1, v1), (k2, v2)) = git_env(w);
    let out = run_with_env(
        &["yank", "--registry", REG_URL, name, version],
        &w.dir,
        &[
            (k1, v1.as_str()),
            (k2, v2.as_str()),
            ("GIT_AUTHOR_NAME", "test"),
            ("GIT_AUTHOR_EMAIL", "test@example.org"),
            ("GIT_COMMITTER_NAME", "test"),
            ("GIT_COMMITTER_EMAIL", "test@example.org"),
        ],
    );
    assert!(out.status.success(), "yank failed: {}", all(&out));
    // URL レジストリは `ksn yank` が push しない（shortcut）ので、ここで bare を反映する。
    with_index(w, |body| {
        let marker = format!("\"version\":\"{version}\"");
        let start = body.find(&marker).unwrap_or_else(|| {
            panic!("index に version {version} が見つからない: {body}")
        });
        let end = start + body[start..].find('}').unwrap_or(0) + 1;
        let e = &body[start..end];
        let flipped = if e.contains("\"yanked\":true") {
            e.replace("\"yanked\":true", "\"yanked\":false")
        } else {
            e.replace("\"yanked\":false", "\"yanked\":true")
        };
        body.replace_range(start..end, &flipped);
    });
    out
}

/// アプリパッケージを `w.dir/app` に作る。`foo` へのレジストリ依存を `range` で持つ。
fn write_app(w: &Work, range: &str) {
    let app = w.app();
    fs::create_dir_all(app.join("src")).unwrap();
    write(
        &app.join("kasane.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"app.main\"\n\n[dependencies]\nfoo = {{ registry = \"{REG_URL}\", version = \"{range}\" }}\n",
        ),
    );
    fs::write(app.join("src").join("app.ksnl"), APP_SRC).unwrap();
    let o = Command::new(exe()).arg("fmt").current_dir(&app).output().expect("spawn ksn");
    assert!(o.status.success(), "app fmt failed: {}", text(&o.stderr));
}

fn lock_text(w: &Work) -> String {
    fs::read_to_string(w.app().join("kasane.lock")).unwrap()
}

#[test]
fn resolve_writes_lock_v3_with_registry_keys_and_offline_check_passes() {
    let w = Work::new("resolve");
    make_registry(&w, "foo", "[]");
    let pkg = make_pkg(&w, "pkg-100", "1.0.0");
    let out = publish(&pkg, "1.0.0", &w);
    assert!(out.status.success(), "publish failed: {}", all(&out));
    write_app(&w, ">=1.0.0, <2.0.0");
    let out = w.ksn(&["update"], &w.app());
    assert_eq!(out.status.code(), Some(0), "update failed: {}", all(&out));
    assert!(
        all(&out).contains("ksn update: foo (なし) -> 1.0.0"),
        "{}",
        all(&out)
    );
    let lock = lock_text(&w);
    assert!(lock.contains("version = 3"), "{}", lock);
    assert!(
        lock.contains("source = \"registry+https://example.org/registry.git#"),
        "{}",
        lock
    );
    assert!(lock.contains("registry = \"https://example.org/registry.git\""), "{}", lock);
    assert!(lock.contains("version = \"1.0.0\""), "{}", lock);
    assert!(lock.contains("hash = \"sha256:"), "{}", lock);
    assert!(lock.contains("deps = []"), "{}", lock);
    // オフライン（キャッシュのみ）の check が通り、lock は書き換わらない。
    let before = lock_text(&w);
    let out = w.ksn(&["check"], &w.app());
    assert_eq!(out.status.code(), Some(0), "check failed: {}", all(&out));
    assert_eq!(lock_text(&w), before);
}

#[test]
fn update_picks_highest_satisfying_version_and_named_update_moves_only_it() {
    let w = Work::new("update");
    make_registry(&w, "foo", "[]");
    let pkg1 = make_pkg(&w, "pkg-100", "1.0.0");
    let pkg2 = make_pkg(&w, "pkg-200", "2.0.0");
    let out = publish(&pkg1, "1.0.0", &w);
    assert!(out.status.success(), "publish 1.0.0 failed: {}", all(&out));
    let out = publish(&pkg2, "2.0.0", &w);
    assert!(out.status.success(), "publish 2.0.0 failed: {}", all(&out));
    write_app(&w, ">=1.0.0, <4.0.0");
    let out = w.ksn(&["update"], &w.app());
    assert_eq!(out.status.code(), Some(0), "update failed: {}", all(&out));
    assert!(lock_text(&w).contains("version = \"2.0.0\""), "{}", lock_text(&w));
    // 3.0.0 を出版し、`update foo` で foo だけを 3.0.0 に動かす。
    let pkg3 = make_pkg(&w, "pkg-300", "3.0.0");
    let out = publish(&pkg3, "3.0.0", &w);
    assert!(out.status.success(), "publish 3.0.0 failed: {}", all(&out));
    let out = w.ksn(&["update", "foo"], &w.app());
    assert_eq!(out.status.code(), Some(0), "update foo failed: {}", all(&out));
    assert!(
        all(&out).contains("ksn update: foo 2.0.0 -> 3.0.0"),
        "{}",
        all(&out)
    );
    assert!(lock_text(&w).contains("version = \"3.0.0\""), "{}", lock_text(&w));
}

#[test]
fn yanked_version_is_excluded_on_update() {
    let w = Work::new("yank_exclude");
    make_registry(&w, "foo", "[]");
    let pkg1 = make_pkg(&w, "pkg-100", "1.0.0");
    let pkg2 = make_pkg(&w, "pkg-200", "2.0.0");
    let out = publish(&pkg1, "1.0.0", &w);
    assert!(out.status.success(), "publish 1.0.0 failed: {}", all(&out));
    let out = publish(&pkg2, "2.0.0", &w);
    assert!(out.status.success(), "publish 2.0.0 failed: {}", all(&out));
    // まだ何もロックしていない状態で 2.0.0 を yank し、update すると yanked 版を除外して 1.0.0 を選ぶ。
    let out = yank(&w, "foo", "2.0.0");
    assert!(out.status.success(), "yank failed: {}", all(&out));
    write_app(&w, ">=1.0.0, <3.0.0");
    let out = w.ksn(&["update"], &w.app());
    assert_eq!(out.status.code(), Some(0), "update after yank failed: {}", all(&out));
    assert!(lock_text(&w).contains("version = \"1.0.0\""), "{}", lock_text(&w));
    assert_eq!(w.ksn(&["check"], &w.app()).status.code(), Some(0));
}

#[test]
fn pinned_yanked_lock_is_kept_on_update() {
    let w = Work::new("yank_pinned");
    make_registry(&w, "foo", "[]");
    let pkg1 = make_pkg(&w, "pkg-100", "1.0.0");
    let pkg2 = make_pkg(&w, "pkg-200", "2.0.0");
    let out = publish(&pkg1, "1.0.0", &w);
    assert!(out.status.success(), "publish 1.0.0 failed: {}", all(&out));
    let out = publish(&pkg2, "2.0.0", &w);
    assert!(out.status.success(), "publish 2.0.0 failed: {}", all(&out));
    write_app(&w, ">=1.0.0, <3.0.0");
    let out = w.ksn(&["update"], &w.app());
    assert_eq!(out.status.code(), Some(0), "update failed: {}", all(&out));
    assert!(lock_text(&w).contains("version = \"2.0.0\""), "{}", lock_text(&w));
    let out = yank(&w, "foo", "2.0.0");
    assert!(out.status.success(), "yank failed: {}", all(&out));
    // lock が 2.0.0 を固定しているので、yanked 版でも解決から外れず 2.0.0 のまま。
    let out = w.ksn(&["update"], &w.app());
    assert_eq!(out.status.code(), Some(0), "update after yank failed: {}", all(&out));
    assert!(
        all(&out).contains("ksn update: foo 2.0.0 -> 2.0.0"),
        "{}",
        all(&out)
    );
    assert!(lock_text(&w).contains("version = \"2.0.0\""), "{}", lock_text(&w));
}

#[test]
fn hash_mismatch_is_e9022() {
    let w = Work::new("hash");
    let pkg = make_pkg(&w, "pkg-100", "1.0.0");
    let commit = git(&pkg, &["rev-parse", "HEAD"]);
    // 索引の hash を故意に間違えたエントリにする（publish 後のハッシュ改竄に相当）。
    // commit は実際のパッケージのコミットと一致させ、取得自体は成功するようにする。
    let zero = "0".repeat(64);
    let body = format!(
        "[{{\"version\":\"1.0.0\",\"tag\":\"v1.0.0\",\"repository\":\"https://example.org/pkg-100.git\",\"commit\":\"{commit}\",\"hash\":\"sha256:{zero}\",\"owner\":\"https://example.org/pkg-100.git\",\"yanked\":false}}]",
    );
    make_registry(&w, "foo", &body);
    write_app(&w, ">=1.0.0, <2.0.0");
    let out = w.ksn(&["update"], &w.app());
    assert_ne!(out.status.code(), Some(0), "{}", all(&out));
    assert!(all(&out).contains("E9022"), "{}", all(&out));
    assert!(all(&out).contains("hash mismatch"), "{}", all(&out));
    // 更新に失敗したので lock は書かれていない。
    assert!(!w.app().join("kasane.lock").exists());
}
