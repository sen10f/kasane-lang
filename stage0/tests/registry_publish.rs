//! `ksn publish` / `ksn yank` (R3b)。ローカルの bare レジストリを索引にして、
//! 出版・読み戻し・重複・下げ版・汚れた木の拒否・yank/unyank を試す。
//! 読み戻しは `ksn registry index` (= registry.rs の `index_versions`) を通す。

mod common;

use common::*;
use scratch::Scratch;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PKG_SRC: &str = "fn foo.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend foo.main\n";

/// `index/<name>.json` に `body` を書いた索引の bare リポジトリを作る。
fn make_registry(base: &Path, name: &str, body: &str) -> PathBuf {
    let work = base.join("work");
    fs::create_dir_all(work.join("index")).unwrap();
    git(&work, &["init", "--quiet", "-b", "main"]);
    fs::write(work.join("index").join(format!("{name}.json")), body).unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "--quiet", "-m", "index"]);
    let bare = base.join("registry.git");
    git(base, &["clone", "--quiet", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()]);
    bare
}

fn git(repo: &Path, args: &[&str]) {
    let st = Command::new("git")
        .current_dir(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.org")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.org")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git");
    assert!(st.success(), "git {:?} failed", args);
}

fn ksn(args: &[&str], cwd: &Path, cache: &Path) -> Output {
    let cache_s = cache.to_string_lossy().to_string();
    run_with_env(args, cwd, &[("KSN_CACHE_DIR", cache_s.as_str())])
}

/// `ksn registry index <reg> <name>` の stdout を行ごとに返す（registry.rs の読み経路を通す）。
fn index_lines(reg: &Path, name: &str, cache: &Path, base: &Path) -> Vec<String> {
    let out = ksn(
        &["registry", "index", reg.to_str().unwrap(), name],
        base,
        cache,
    );
    assert!(out.status.success(), "registry index: {}", text(&out.stderr));
    text(&out.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
}

/// 公開可能なパッケージを作る：version の kasane.toml、src/foo.ksnl、fmt の正規形、
/// git commit、origin remote、HEAD に v<version> タグ。
fn make_pkg(base: &Path, version: &str) -> PathBuf {
    let dir = base.join("pkg");
    fs::create_dir_all(dir.join("src")).unwrap();
    let toml = format!(
        "[package]\nname = \"foo\"\nversion = \"{version}\"\nedition = 1\nentry = \"foo.main\"\n\n[dependencies]\n",
    );
    fs::write(dir.join("kasane.toml"), &toml).unwrap();
    fs::write(dir.join("src").join("foo.ksnl"), PKG_SRC).unwrap();
    let o = Command::new(exe())
        .arg("fmt")
        .current_dir(&dir)
        .output()
        .expect("spawn ksn");
    assert!(o.status.success(), "fmt failed: {}", text(&o.stderr));
    git(&dir, &["init", "--quiet", "-b", "main"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "--quiet", "-m", "init"]);
    git(&dir, &["remote", "add", "origin", "https://example.org/foo.git"]);
    git(&dir, &["tag", &format!("v{version}")]);
    dir
}

fn publish(pkg: &Path, reg: &Path, cache: &Path) -> Output {
    ksn(&["publish", "--registry", reg.to_str().unwrap()], pkg, cache)
}

#[test]
fn publish_then_read_back() {
    let s = Scratch::at("registry_publish_readback".into());
    let base = &s.0;
    let cache = base.join("cache");
    let reg = make_registry(base, "foo", "[]");
    let pkg = make_pkg(base, "1.0.0");
    let out = publish(&pkg, &reg, &cache);
    assert!(
        out.status.success(),
        "publish failed: {}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    let lines = index_lines(&reg, "foo", &cache, base);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("1.0.0 "), "{lines:?}");
}

#[test]
fn refuses_duplicate_version() {
    let s = Scratch::at("registry_publish_dup".into());
    let base = &s.0;
    let cache = base.join("cache");
    let reg = make_registry(
        base,
        "foo",
        &format!(
            "[{{\"version\":\"1.0.0\",\"tag\":\"v1.0.0\",\"repository\":\"https://example.org/foo.git\",\"commit\":\"{}\",\"hash\":\"{}\",\"owner\":\"https://example.org/foo.git\",\"yanked\":false}}]",
            "0123456789abcdef0123456789abcdef01234567",
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ),
    );
    let pkg = make_pkg(base, "1.0.0");
    let out = publish(&pkg, &reg, &cache);
    assert!(!out.status.success(), "duplicate publish should fail");
    let err = text(&out.stderr);
    assert!(err.contains("not greater than existing"), "{err}");
}

#[test]
fn refuses_lower_version() {
    let s = Scratch::at("registry_publish_lower".into());
    let base = &s.0;
    let cache = base.join("cache");
    let reg = make_registry(
        base,
        "foo",
        &format!(
            "[{{\"version\":\"1.0.0\",\"tag\":\"v1.0.0\",\"repository\":\"https://example.org/foo.git\",\"commit\":\"{}\",\"hash\":\"{}\",\"owner\":\"https://example.org/foo.git\",\"yanked\":false}}]",
            "0123456789abcdef0123456789abcdef01234567",
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ),
    );
    let pkg = make_pkg(base, "0.9.0");
    let out = publish(&pkg, &reg, &cache);
    assert!(!out.status.success(), "lower publish should fail");
    let err = text(&out.stderr);
    assert!(err.contains("not greater than existing"), "{err}");
}

#[test]
fn refuses_dirty_tree() {
    let s = Scratch::at("registry_publish_dirty".into());
    let base = &s.0;
    let cache = base.join("cache");
    let reg = make_registry(base, "foo", "[]");
    let pkg = make_pkg(base, "1.0.0");
    // 汚れた木：追跡されていない無関係なファイルを置く（check/fmt/test は通るが git は dirty）。
    fs::write(pkg.join("notes.txt"), "memo\n").unwrap();
    let out = publish(&pkg, &reg, &cache);
    assert!(!out.status.success(), "dirty-tree publish should fail");
    let err = text(&out.stderr);
    assert!(err.contains("working tree is not clean"), "{err}");
}

#[test]
fn yank_then_unyank() {
    let s = Scratch::at("registry_publish_yank".into());
    let base = &s.0;
    let cache = base.join("cache");
    let reg = make_registry(base, "foo", "[]");
    let pkg = make_pkg(base, "1.0.0");
    let out = publish(&pkg, &reg, &cache);
    assert!(out.status.success(), "publish failed: {}", text(&out.stderr));
    // yank
    let out = ksn(
        &["yank", "--registry", reg.to_str().unwrap(), "foo", "1.0.0"],
        base,
        &cache,
    );
    assert!(
        out.status.success(),
        "yank failed: {}",
        text(&out.stderr)
    );
    let lines = index_lines(&reg, "foo", &cache, base);
    assert!(lines.is_empty(), "yanked version must be excluded: {lines:?}");
    // unyank
    let out = ksn(
        &["yank", "--undo", "--registry", reg.to_str().unwrap(), "foo", "1.0.0"],
        base,
        &cache,
    );
    assert!(
        out.status.success(),
        "unyank failed: {}",
        text(&out.stderr)
    );
    let lines = index_lines(&reg, "foo", &cache, base);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("1.0.0 "), "{lines:?}");
}
