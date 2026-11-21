//! `kasane.toml` / `kasane.lock` の厳格な検査 (設計_tomlとlockの書き方.md 7 章)。
//!
//! - ロックの知らないキー (E9007)、`entry` の型 (E9007)、`deps` の中身と省略・path 依存の `hash` (E9021)。
//! - 字句は `.ksnl` と同じ: BOM・制御文字 (E0004)、NFC (E0006)、CR (E0001)。
//! - 整形は `ksn fmt` が正規形を作り、`ksn check` は違えば E0020。エスケープ・整数の別の書き方・
//!   単一引用符・複数行文字列・点付きキー・引用符付きキー・インラインの表・`[dependencies.x]` も
//!   正規形との差で E0020 になる。コメントは `ksn fmt` でも消えない。

#![allow(dead_code, unused_imports)]

mod common;

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::path::Path;
use common::*;

/// 依存 `lib` (path 依存) がある一時パッケージ。返り値は (本体, lib の置き場)。
/// lib は本体の隣 (`../<本体の名前>_lib`) に置き、`../lib` という書き方を実在する相対パスに置き換える。
/// with_dep が置き換える実在する相対パス (本体から lib へ。比較にも使う)。
fn dep_rel() -> String {
    format!("../ksn_test_{}_lib", std::process::id())
}

fn with_dep(name: &str, toml: &str, lock: &str) -> (Tmp, std::path::PathBuf) {
    // 依存は同じ実行ファイルの試験で共有するので、1 試験の解体で消さない (`Scratch` にしない)。小さい (数 KB) ので 1 実行に 1 つだけ残る。
    let libdir = std::env::temp_dir().join(format!("ksn_test_{}_lib", std::process::id()));
    // 依存の中身はどの試験でも同じ。同じ実行ファイルの試験は並列に走るので、消して作り直すと
    // ほかの試験の検査の途中で依存が消える (2026-11-20、速い検証機で落ちた)。最初の 1 回だけ作る。
    static MAKE_LIB: std::sync::Once = std::sync::Once::new();
    MAKE_LIB.call_once(|| {
        let _ = fs::remove_dir_all(&libdir);
        fs::create_dir_all(libdir.join("src")).unwrap();
        fs::write(libdir.join("kasane.toml"), "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = 1\n\n[dependencies]\n").unwrap();
        fs::write(libdir.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(libdir.join("src").join("lib.ksnl"), "fn lib.hello() -> u32 uses [] panics never pub\n    7u32\nend lib.hello\n").unwrap();
    });
    let rel = format!("../{}", libdir.file_name().unwrap().to_string_lossy());
    let t = Tmp::new_raw(name, "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let _x = lib.hello()\n    Ok(())\nend t.main\n");
    fs::write(t.0.join("kasane.toml"), toml.replace("../lib", &rel)).unwrap();
    fs::write(t.0.join("kasane.lock"), lock.replace("../lib", &rel)).unwrap();
    (t, libdir)
}
const DEP_TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\nlib = { path = \"../lib\" }\n";
const DEP_LOCK: &str = "version = 2\n\n[[package]]\nname = \"lib\"\nsource = \"path+../lib\"\ndeps = []\n";

fn check_codes(t: &Tmp) -> (i32, Vec<String>) {
    let o = run(&["check", "--json"], &t.0);
    (o.status.code().unwrap_or(-1), codes(&text(&o.stdout)))
}

#[test]
fn entry_must_be_a_string() {
    let t = Tmp::new_raw("entrytype", "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"hi\")\nend t.main\n");
    fs::write(t.0.join("kasane.toml"), TOML.replace("entry = \"t.main\"", "entry = 123")).unwrap();
    let (rc, cs) = check_codes(&t);
    assert_eq!(rc, 1);
    assert!(cs.iter().any(|c| c == "E9007"), "{:?}", cs);
}

#[test]
fn lock_unknown_keys_are_rejected() {
    let t = Tmp::new_raw("lockkey1", "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"hi\")\nend t.main\n");
    // 最上位の知らないキー。
    fs::write(t.0.join("kasane.lock"), "version = 1\nextra = 2\n").unwrap();
    let (rc, cs) = check_codes(&t);
    assert_eq!(rc, 1);
    assert!(cs.iter().any(|c| c == "E9007"), "{:?}", cs);
    // [[package]] の中の知らないキー。
    let (t2, _) = with_dep("lockkey2", DEP_TOML, DEP_LOCK);
    fs::write(t2.0.join("kasane.lock"), DEP_LOCK.replace("deps = []", "extra = 1\ndeps = []")).unwrap();
    let (rc, cs) = check_codes(&t2);
    assert_eq!(rc, 1);
    assert!(cs.iter().any(|c| c == "E9007"), "{:?}", cs);
}

#[test]
fn lock_deps_must_be_strings_and_present() {
    // L2: 文字列でない要素は E9021。
    let (t, _) = with_dep("lockdep1", DEP_TOML, DEP_LOCK);
    fs::write(t.0.join("kasane.lock"), DEP_LOCK.replace("deps = []", "deps = [1]")).unwrap();
    let (rc, cs) = check_codes(&t);
    assert_eq!(rc, 1);
    assert!(cs.iter().any(|c| c == "E9021"), "{:?}", cs);
    // L6: deps の省略も E9021。
    let (t, _) = with_dep("lockdep2", DEP_TOML, DEP_LOCK);
    fs::write(t.0.join("kasane.lock"), "version = 1\n\n[[package]]\nname = \"lib\"\nsource = \"path+../lib\"\n").unwrap();
    let (rc, cs) = check_codes(&t);
    assert_eq!(rc, 1);
    assert!(cs.iter().any(|c| c == "E9021"), "{:?}", cs);
}

#[test]
fn lock_hash_only_for_git_deps() {
    let (t, _) = with_dep("lockhash", DEP_TOML, DEP_LOCK);
    fs::write(t.0.join("kasane.lock"), DEP_LOCK.replace("deps = []", "hash = \"sha256:00\"\ndeps = []")).unwrap();
    let (rc, cs) = check_codes(&t);
    assert_eq!(rc, 1);
    assert!(cs.iter().any(|c| c == "E9021"), "{:?}", cs);
}

#[test]
fn lexical_rules_match_ksnl() {
    let t = Tmp::new_raw("lockbom", "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"hi\")\nend t.main\n");
    // BOM (E0004)。
    let mut toml = String::from("\u{feff}");
    toml.push_str(TOML);
    fs::write(t.0.join("kasane.toml"), toml).unwrap();
    let (rc, cs) = check_codes(&t);
    assert_eq!(rc, 1);
    assert!(cs.iter().any(|c| c == "E0004"), "{:?}", cs);
    // CR (E0001)。
    fs::write(t.0.join("kasane.toml"), TOML.replace('\n', "\r\n")).unwrap();
    let (rc, cs) = check_codes(&t);
    assert_eq!(rc, 1);
    assert!(cs.iter().any(|c| c == "E0001"), "{:?}", cs);
    // NFC でないコメント (E0006)。
    fs::write(t.0.join("kasane.toml"), format!("# cafe\u{301}\n{}", TOML)).unwrap();
    let (rc, cs) = check_codes(&t);
    assert_eq!(rc, 1);
    assert!(cs.iter().any(|c| c == "E0006"), "{:?}", cs);
}

#[test]
fn deviations_are_rejected_by_check() {
    // 書き方が 1 通り (E0020)。意味は同じでも fmt の出力と違う文面は誤り。
    let tomls = [
        // 整数の 10 進以外。
        TOML.replace("edition = 1", "edition = 0x1"),
        // 単一引用符。
        TOML.replace("entry = \"t.main\"", "entry = 't.main'"),
        // 複数行文字列。
        TOML.replace("entry = \"t.main\"", "entry = \"\"\"t.main\"\"\""),
        // インラインの表。
        "package = { name = \"t\", version = \"0.1.0\", edition = 1, entry = \"t.main\" }\n".to_string(),
        // 引用符付きキー。
        TOML.replace("entry = \"t.main\"", "\"entry\" = \"t.main\""),
        // 末尾の改行がない。
        TOML.trim_end_matches('\n').to_string(),
    ];
    for (i, toml) in tomls.iter().enumerate() {
        let t = Tmp::new_raw(&format!("deviate{}", i), "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"hi\")\nend t.main\n");
        fs::write(t.0.join("kasane.toml"), toml).unwrap();
        let (rc, cs) = check_codes(&t);
        assert_eq!(rc, 1, "{}", toml);
        assert!(cs.iter().any(|c| c == "E0020"), "{}: {:?}", toml, cs);
    }
    // 点付きキー (全部が点付きなら意味は同じなので、E0020 で正規形に直させる)。
    let t = Tmp::new_raw("deviatedot", "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"hi\")\nend t.main\n");
    fs::write(
        t.0.join("kasane.toml"),
        "package.name = \"t\"\npackage.version = \"0.1.0\"\npackage.edition = 1\npackage.entry = \"t.main\"\n",
    )
    .unwrap();
    let (rc, cs) = check_codes(&t);
    assert_eq!(rc, 1);
    assert!(cs.iter().any(|c| c == "E0020"), "{:?}", cs);
    // [dependencies.lib] の通常表 (インラインが正) と、ロックの `package = [...]`。
    let tbl = format!("[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies.lib]\npath = \"{}\"\n", dep_rel());
    let (t, _) = with_dep("deviatetbl", &tbl, DEP_LOCK);
    let (rc, cs) = check_codes(&t);
    assert_eq!(rc, 1);
    assert!(cs.iter().any(|c| c == "E0020"), "{:?}", cs);
    let (t, _) = with_dep("deviatearr", DEP_TOML, DEP_LOCK);
    fs::write(t.0.join("kasane.lock"), "version = 1\n\npackage = [{name = \"lib\", source = \"path+../lib\", deps = []}]\n").unwrap();
    let (rc, cs) = check_codes(&t);
    assert_eq!(rc, 1);
    assert!(cs.iter().any(|c| c == "E9007" || c == "E0020"), "{:?}", cs);
}

#[test]
fn fmt_normalizes_and_is_idempotent() {
    let (t, _) = with_dep("fmttoml", &DEP_TOML.replace("edition = 1", "edition = 0x1"), DEP_LOCK);
    let o = run(&["fmt"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let toml1 = fs::read_to_string(t.0.join("kasane.toml")).unwrap();
    assert_eq!(toml1, DEP_TOML.replace("../lib", &dep_rel()), "{}", toml1);
    // 2 回目で変わらない (不動点)。
    let o = run(&["fmt"], &t.0);
    let toml2 = fs::read_to_string(t.0.join("kasane.toml")).unwrap();
    assert_eq!(toml1, toml2);
    assert!(text(&o.stdout).is_empty(), "{}", text(&o.stdout));
    // 整形後は check を通る。
    let (rc, cs) = check_codes(&t);
    assert_eq!(rc, 0, "{:?}", cs);
}

#[test]
fn fmt_preserves_comments() {
    let toml = "# the package\n[package]\nname = \"t\" # main\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";
    let t = Tmp::new_raw("fmtcomment", "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"hi\")\nend t.main\n");
    fs::write(t.0.join("kasane.toml"), toml).unwrap();
    let o = run(&["fmt"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let after = fs::read_to_string(t.0.join("kasane.toml")).unwrap();
    for c in ["# the package", "# main"] {
        assert!(after.contains(c), "{} missing in {}", c, after);
    }
    let (rc, cs) = check_codes(&t);
    assert_eq!(rc, 0, "{:?}", cs);
}

#[test]
fn update_writes_canonical_lock() {
    let (t, _) = with_dep("updlock", DEP_TOML, DEP_LOCK);
    let o = run(&["update"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let lock = fs::read_to_string(t.0.join("kasane.lock")).unwrap();
    assert_eq!(lock, DEP_LOCK.replace("../lib", &dep_rel()), "{}", lock);
    let (rc, cs) = check_codes(&t);
    assert_eq!(rc, 0, "{:?}", cs);
}
