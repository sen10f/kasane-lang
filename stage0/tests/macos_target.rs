//! macOS ターゲット (`ksn build --target aarch64-apple-darwin` / `x86_64-apple-darwin`、設計_macOS.md)。
//! 実際の Mac は無いので、目的ファイル (`.o`) の魔術番号とリンク失敗の理由だけを確かめ、実行は行わない。
//! macOS SDK (`KSN_SYSROOT`) が無ければ黙って飛ばす (本物の Mac でないと確かめられないもの)。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use common::*;

const TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];

fn have(cmd: &str) -> bool {
    Command::new(cmd).arg("--version").stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false)
}

fn cross_ready() -> bool {
    // macOS 実行ファイルをこの機械で動かせないので、obj の魔術番号とリンク失敗の理由だけ。
    cfg!(all(target_arch = "x86_64", target_os = "linux"))
}

const SRC: &str = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    Ok(())\nend t.main\n";

/// `ksn build --target <t> --emit obj <file>` で作った目的ファイルが Mach-O (`cf fa ed fe`) か。
#[test]
fn macos_obj_is_macho() {
    if !cross_ready() {
        return;
    }
    for t in TARGETS {
        let tmp = Tmp::new("macos_obj", SRC);
        let obj = tmp.0.join("t.o");
        let o = run(&["build", "--target", t, "--emit", "obj", obj.to_str().unwrap()], &tmp.0);
        assert_eq!(
            o.status.code(),
            Some(0),
            "{}: obj\n{}{}",
            t,
            text(&o.stdout),
            text(&o.stderr)
        );
        let b = fs::read(&obj).unwrap();
        assert!(b.len() >= 4, "{}: obj が短すぎる", t);
        assert_eq!(
            &b[..4],
            &[0xcf, 0xfa, 0xed, 0xfe],
            "{}: Mach-O の魔術番号が違う",
            t
        );
    }
}

/// `KSN_SYSROOT` (macOS SDK) なしで macOS 向けをリンクしようとすると、SDK を名指しして失敗する。
#[test]
fn macos_link_needs_sdk() {
    if !cross_ready() {
        return;
    }
    let tmp = Tmp::new("macos_link", SRC);
    let mut cmd = Command::new(exe());
    cmd.args(["build", "--target", "x86_64-apple-darwin"])
        .current_dir(&tmp.0)
        .env_remove("KSN_SYSROOT");
    let o = cmd.output().expect("spawn ksn");
    assert_ne!(o.status.code(), Some(0), "KSN_SYSROOT なしでも macOS リンクが通ってしまった");
    let err = text(&o.stderr);
    assert!(err.contains("KSN_SYSROOT"), "KSN_SYSROOT を名指ししていない: {}", err);
}

/// macOS SDK (`KSN_SYSROOT`) がそろっているときだけリンクまでを確かめる。
#[test]
fn macos_link_with_sdk_when_present() {
    if !cross_ready() {
        return;
    }
    let sdk = match std::env::var("KSN_SYSROOT").ok().filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return, // SDK が無いので黙って飛ばす
    };
    if !PathBuf::from(&sdk).join("usr").join("lib").is_dir() {
        return;
    }
    for t in TARGETS {
        let tmp = Tmp::new("macos_link_sdk", SRC);
        let out = tmp.0.join(format!("t.{}", t));
        let o = Command::new(exe())
            .args(["build", "--target", t])
            .current_dir(&tmp.0)
            .env("KSN_SYSROOT", &sdk)
            .output()
            .expect("spawn ksn");
        assert_eq!(
            o.status.code(),
            Some(0),
            "{}: link with SDK\n{}{}",
            t,
            text(&o.stdout),
            text(&o.stderr)
        );
        let b = fs::read(&out).unwrap();
        assert!(b.len() >= 4, "{}: exe が短すぎる", t);
        assert_eq!(
            &b[..4],
            &[0xcf, 0xfa, 0xed, 0xfe],
            "{}: 実行ファイルが Mach-O ではない",
            t
        );
    }
}
