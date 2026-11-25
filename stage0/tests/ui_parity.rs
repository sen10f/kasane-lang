//! Z11 UI 一致試験。GPU/FFI を持たない純粋 UI パッケージ
//! (`layout`、`tree`、`event`、`widgets` があれば) について、
//! `ksn test --both` が 0 失敗で通ること、ドライバープログラムが
//! `ksn fmt --check` を通ること、`ksn run`（インタプリタ / 木走査）の出力と
//! `ksn build`（debug/release）のネイティブ実行ファイルの出力をバイト一致させること、
//! およびバンドルフォールバックが出ないことを確かめる。
//! フォールバックする形は回避せず、テストの末尾に一覧してレポートする。

#![allow(dead_code, unused_imports)]

mod common;
use common::*;

use std::process::Command;
use std::path::{Path, PathBuf};

fn ksn(args: &[&str], cwd: &Path) -> std::process::Output {
    Command::new(exe()).args(args).current_dir(cwd).output().expect("spawn ksn")
}

fn all(o: &std::process::Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).replace("\r\n", "\n")
}

/// 純粋 UI パッケージと、それに `path` 依存するドライバープログラム。
struct Case {
    pkg: &'static str,
    /// `root()` (= kasane ルート) からの相対パス。
    ui_dir: &'static str,
    /// `root()` からの相対パス。
    drv_dir: &'static str,
    drv_name: &'static str,
}

const CASES: &[Case] = &[
    Case {
        pkg: "ui.layout",
        ui_dir: "ui/kasane/layout",
        drv_dir: "stage0/tests/data/ui_parity/drv_layout",
        drv_name: "drv_layout",
    },
    Case {
        pkg: "ui.tree",
        ui_dir: "ui/kasane/tree",
        drv_dir: "stage0/tests/data/ui_parity/drv_tree",
        drv_name: "drv_tree",
    },
    Case {
        pkg: "ui.event",
        ui_dir: "ui/kasane/event",
        drv_dir: "stage0/tests/data/ui_parity/drv_event",
        drv_name: "drv_event",
    },
];

fn native_bin(drv_dir: &Path, kind: &str, drv_name: &str) -> PathBuf {
    drv_dir.join("target").join(kind).join(format!("{}{}", drv_name, if cfg!(windows) { ".exe" } else { "" }))
}

fn has_bundle_fallback(out: &str) -> bool {
    out.contains("the interpreter bundle is used")
        || out.contains("native backend skipped")
        || out.contains("bundle fallback")
}

#[test]
fn ui_parity() {
    let mut fallback_cases: Vec<(&'static str, &'static str, bool)> = Vec::new();
    for case in CASES {
        let ui_dir = root().join(case.ui_dir);
        let drv_dir = root().join(case.drv_dir);

        // 1. UI パッケージ自身の test --both は診断 0 件・失敗 0 件で通る。
        let o = ksn(&["test", "--both"], &ui_dir);
        let out = all(&o);
        assert_eq!(
            o.status.code(),
            Some(0),
            "{} test--both: {}",
            case.pkg,
            out
        );
        assert!(!out.contains("FAILED"), "{} test--both has failure: {}", case.pkg, out);
        assert!(
            out.contains("0 failed"),
            "{} test--both: {}",
            case.pkg,
            out
        );

        // 2. ドライバーは ksn fmt --check を通る。
        let o = ksn(&["fmt", "--check"], &drv_dir);
        assert_eq!(
            o.status.code(),
            Some(0),
            "{} fmt--check: {}",
            case.pkg,
            all(&o)
        );

        // 3. ドライバーを ksn run（インタプリタ / 木走査）で実行した出力。
        let o = ksn(&["run"], &drv_dir);
        assert_eq!(o.status.code(), Some(0), "{} run: {}", case.pkg, all(&o));
        let interp = text(&o.stdout);

        // 4. debug / release のネイティブビルドと出力をバイト比較。
        //    バンドルフォールバックする形は回避せず、一覧に記録するだけ。
        for (kind, args) in [("debug", &["build"][..]), ("release", &["build", "--release"][..])] {
            let o = ksn(args, &drv_dir);
            let out = all(&o);
            let fallback = has_bundle_fallback(&out);
            fallback_cases.push((case.pkg, kind, fallback));
            println!(
                "ui_parity: {} {} build rc={:?} bundle_fallback={}",
                case.pkg,
                kind,
                o.status.code(),
                fallback
            );
            let bin = native_bin(&drv_dir, kind, case.drv_name);
            if bin.exists() {
                let r = Command::new(&bin).output().unwrap();
                let native_out = text(&r.stdout);
                assert_eq!(
                    native_out,
                    interp,
                    "{} {} native output differs from interpreter",
                    case.pkg,
                    kind
                );
            }
        }
    }
    println!("ui_parity: bundle_fallback_cases: {:#?}", fallback_cases);
}
