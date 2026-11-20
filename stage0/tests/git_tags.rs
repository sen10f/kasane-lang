//! `git_tags` モジュール (`git ls-remote --tags` から semver タグを列挙) の試験。
//! `ksn git tags <url>` 命令を通して黒箱で試す。bare リポジトリは試験ごとに作って後片付けする。
//! 同じ版の重複は、実の git リポジトリでは同じ ref 名を 2 つ作れないため、
//! `stage0/src/git_tags.rs` の単体試験 (合成の ls-remote 出力) で併せて試す。

#![allow(dead_code, unused_imports)]

mod common;

#[path = "common/scratch.rs"]
mod scratch;

use std::path::Path;
use std::process::{Command, Stdio};
use common::*;
use scratch::Scratch;

fn git_env() -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.org")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.org");
    cmd
}

fn git(repo: &Path, args: &[&str]) -> String {
    let mut cmd = git_env();
    cmd.current_dir(repo)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.org")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.org");
    let out = cmd.args(args).output().expect("git を起動");
    assert!(
        out.status.success(),
        "git {args:?} が失敗: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// bare リポジトリを作り、空コミット 1 つとそのコミットを返す。
fn run(repo: &Path, args: &[&str]) -> String {
    let mut cmd = git_env();
    cmd.current_dir(repo);
    if args[0] == "mktree" {
        cmd.stdin(Stdio::null()); // 空の木 (標準入力が端子だと mktree が入力を待つ)
    }
    let out = cmd.args(args).output().expect("git を起動");
    assert!(
        out.status.success(),
        "git {args:?} が失敗: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn make_repo() -> (Scratch, String) {
    let repo = Scratch::at("git-tags-repo".to_string());
    let path = repo.to_str().unwrap().to_string();
    let out = git_env().args(["init", "--bare", &path]).output().expect("git を起動");
    assert!(
        out.status.success(),
        "git init --bare が失敗: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tree = run(&repo, &["mktree"]).trim().to_string();
    let commit = run(&repo, &["commit-tree", &tree, "-m", "init"]).trim().to_string();
    run(&repo, &["update-ref", "refs/heads/main", &commit]);
    (repo, commit)
}

/// 軽量タグ・annotated tag を作る。
fn tag(repo: &Scratch, name: &str, commit: &str, annotated: bool) {
    if annotated {
        run(repo, &["tag", "-a", name, "-m", format!("message {name}").as_str(), commit]);
    } else {
        run(repo, &["tag", name, commit]);
    }
}

/// `ksn git tags <url>` を実行する。
fn ksn_git_tags(url: &str) -> (i32, String, String) {
    let out = Command::new(exe())
        .args(["git", "tags", url])
        .output()
        .expect("ksn を起動");
    (
        out.status.code().unwrap_or(99),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn semver_tags_are_listed_sorted() {
    let (repo, commit) = make_repo();
    tag(&repo, "v2.0.0", &commit, false);
    tag(&repo, "v1.0.0", &commit, false);
    tag(&repo, "v1.1.0", &commit, true); // annotated: peel の先も同じコミット
    let (code, out, err) = ksn_git_tags(repo.to_str().unwrap());
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out,
        format!("1.0.0 {commit}\n1.1.0 {commit}\n2.0.0 {commit}\n"),
        "版順にソートされ、annotated tag はコミットへ peel される"
    );
}

#[test]
fn annotated_tag_peels_to_the_tagged_commit() {
    let (repo, commit) = make_repo();
    tag(&repo, "v1.0.0", &commit, true);
    let (code, out, _) = ksn_git_tags(repo.to_str().unwrap());
    assert_eq!(code, 0);
    assert_eq!(out, format!("1.0.0 {commit}\n"));
}

#[test]
fn non_semver_tags_are_ignored() {
    let (repo, commit) = make_repo();
    tag(&repo, "v1.0.0", &commit, false);
    tag(&repo, "v1.2", &commit, false);
    tag(&repo, "release-1", &commit, false);
    tag(&repo, "v01.2.3", &commit, false); // 先頭 0: 版ではない
    tag(&repo, "1.2.3", &commit, false); // `v` なし: 版ではない
    tag(&repo, "v1.2.3.4", &commit, false);
    tag(&repo, "v1.2.3-rc1", &commit, false);
    let (code, out, _) = ksn_git_tags(repo.to_str().unwrap());
    assert_eq!(code, 0);
    assert_eq!(out, format!("1.0.0 {commit}\n"));
}

#[test]
fn no_tags_is_empty() {
    let (repo, _commit) = make_repo();
    let (code, out, _) = ksn_git_tags(repo.to_str().unwrap());
    assert_eq!(code, 0);
    assert_eq!(out, "");
}

#[test]
fn unreachable_url_gives_clear_error() {
    let (code, _out, err) = ksn_git_tags("/nonexistent-repo-r1b.git");
    assert_eq!(code, 1);
    assert!(err.contains("/nonexistent-repo-r1b.git"), "エラーに URL が含まれる: {err}");
    assert!(!err.is_empty());
}

#[test]
fn missing_subcommand_shows_usage() {
    let out = Command::new(exe()).args(["git"]).output().expect("ksn を起動");
    assert_eq!(out.status.code().unwrap_or(99), 2);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("usage: ksn git tags <url>")
    );
}
