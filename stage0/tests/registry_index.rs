//! 最小レジストリの索引 (R3a) の試験。`ksn registry index` を通して、ローカルの bare リポジトリを索引にして試す。
//! 索引の形 (strict な読み・昇順・yank) と、cache の使い方 (オンライン取得・オフラインの再利用・未取得のエラー) を見る。

#![allow(dead_code, unused_imports)]

mod common;

#[path = "common/scratch.rs"]
mod scratch;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::*;
use scratch::Scratch;

const C1: &str = "0123456789abcdef0123456789abcdef01234567";
const C2: &str = "89abcdef0123456789abcdef0123456789abcdef";
const H: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

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

fn entry(version: &str, commit: &str, yanked: bool) -> String {
    format!(
        "{{\"version\":\"{version}\",\"tag\":\"v{version}\",\"repository\":\"https://example.org/foo.git\",\"commit\":\"{commit}\",\"hash\":\"{H}\",\"owner\":\"https://example.org/foo.git\",\"yanked\":{yanked}}}"
    )
}

/// `index/<name>.json` に `body` を書いた索引の bare リポジトリを作る。
fn make_registry(base: &Path, name: &str, body: &str) -> PathBuf {
    let work = base.join("work");
    std::fs::create_dir_all(work.join("index")).unwrap();
    git(&work, &["init", "--quiet", "-b", "main"]);
    std::fs::write(work.join("index").join(format!("{name}.json")), body).unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "--quiet", "-m", "index"]);
    let bare = base.join("registry.git");
    git(base, &["clone", "--quiet", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()]);
    bare
}

fn ksn_registry(args: &[&str], cache: &Path) -> Output {
    let cache_s = cache.to_string_lossy().to_string();
    run_with_env(&[&["registry"][..], args].concat(), cache.parent().unwrap(), &[("KSN_CACHE_DIR", &cache_s)])
}

fn arr(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

#[test]
fn lists_versions_ascending_and_skips_yanked() {
    let s = Scratch::at("registry_index_list".into());
    let base = &s.0;
    let body = arr(&[entry("1.0.0", C1, false), entry("1.1.0", C2, true), entry("1.2.0", C1, false)]);
    let reg = make_registry(base, "foo", &body);
    let out = ksn_registry(&["index", reg.to_str().unwrap(), "foo"], &base.join("cache"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].starts_with("1.0.0 "), "{text}");
    assert!(lines[1].starts_with("1.2.0 "), "{text}");
}

#[test]
fn offline_uses_cache_and_fails_without_it() {
    let s = Scratch::at("registry_index_offline".into());
    let base = &s.0;
    let body = arr(&[entry("1.0.0", C1, false)]);
    let reg = make_registry(base, "foo", &body);
    let cache = base.join("cache");
    // 未取得のオフラインは明確なエラー
    let miss = ksn_registry(&["index", reg.to_str().unwrap(), "foo", "--offline"], &cache);
    assert!(!miss.status.success());
    assert!(String::from_utf8_lossy(&miss.stderr).contains("not cached"), "{}", String::from_utf8_lossy(&miss.stderr));
    // オンラインで取得してから、オフラインで読める
    let on = ksn_registry(&["index", reg.to_str().unwrap(), "foo"], &cache);
    assert!(on.status.success(), "{}", String::from_utf8_lossy(&on.stderr));
    let off = ksn_registry(&["index", reg.to_str().unwrap(), "foo", "--offline"], &cache);
    assert!(off.status.success(), "{}", String::from_utf8_lossy(&off.stderr));
    assert!(String::from_utf8_lossy(&off.stdout).starts_with("1.0.0 "));
}

#[test]
fn missing_package_is_an_error() {
    let s = Scratch::at("registry_index_missing".into());
    let base = &s.0;
    let reg = make_registry(base, "foo", &arr(&[entry("1.0.0", C1, false)]));
    let out = ksn_registry(&["index", reg.to_str().unwrap(), "bar"], &base.join("cache"));
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not in registry"));
}

#[test]
fn rejects_bad_index_files() {
    let cases: Vec<(&str, String)> = vec![
        ("unknown_key", arr(&[entry("1.0.0", C1, false).replace("\"owner\"", "\"ownr\"")])),
        ("descending", arr(&[entry("1.2.0", C1, false), entry("1.0.0", C2, false)])),
        ("duplicate", arr(&[entry("1.0.0", C1, false), entry("1.0.0", C2, false)])),
        ("bad_hash", arr(&[entry("1.0.0", C1, false).replace(H, "md5:abc")])),
        ("bad_commit", arr(&[entry("1.0.0", "ABC", false)])),
        ("tag_mismatch", arr(&[entry("1.0.0", C1, false).replace("\"tag\":\"v1.0.0\"", "\"tag\":\"1.0.0\"")])),
        ("not_array", "{}".to_string()),
    ];
    for (label, body) in cases {
        let s = Scratch::at(format!("registry_index_bad_{label}"));
        let base = &s.0;
        let reg = make_registry(base, "foo", &body);
        let out = ksn_registry(&["index", reg.to_str().unwrap(), "foo"], &base.join("cache"));
        assert!(!out.status.success(), "{label} should be rejected");
        assert!(String::from_utf8_lossy(&out.stderr).contains("E9040"), "{label}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn rejects_path_like_package_names() {
    let s = Scratch::at("registry_index_name".into());
    let base = &s.0;
    let reg = make_registry(base, "foo", &arr(&[entry("1.0.0", C1, false)]));
    let out = ksn_registry(&["index", reg.to_str().unwrap(), "../foo"], &base.join("cache"));
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid registry package name"));
}
