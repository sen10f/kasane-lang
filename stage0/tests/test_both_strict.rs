//! `ksn test --both --native-required`: ネイティブのビルドが飛ばされたテストを失敗にする。
//! 退避 (インタプリタ同梱のバンドル) で走らせて通す従来の挙動は、フラグなしのとき変わらない。
//! あわせて、E9040 の本文が内部の言い方 (段階 1・型表) でなく利用者の言葉で、構文名と回避策を出すことを確かめる。

#![allow(dead_code, unused_imports)]

mod common;

use common::{run, text, Tmp};

/// 下ろせるテスト (整数の演算) と、連鎖した `std.io.stdout().flush()` を含むため下ろせないテスト。
/// `ksn fmt` の出力と同じ形で書く (E0020 にならないように)。
const MIXED: &str = "fn t.add(a: i32, b: i32) -> i32 uses [] panics never
    a.wrapping_add(b)
end t.add

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let _x = t.add(1i32, 2i32)
    Ok(())
end t.main

fn t.nap() -> Result<(), std.io.Error> uses [io.console] panics never
    std.io.stdout().flush()?
    Ok(())
end t.nap

test t.add.test_add
    core.check.eq(t.add(1i32, 2i32), 3i32)
end t.add.test_add

test t.nap.test_nap
    let _r = t.nap()
end t.nap.test_nap
";

/// 入口 (`main`) が下ろせない関数を呼ぶ。`ksn build --release` は E9040 で止まる。
const ENTRY_NAP: &str = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never
    t.nap()?
    Ok(())
end t.main

fn t.nap() -> Result<(), std.io.Error> uses [io.console] panics never
    std.io.stdout().flush()?
    Ok(())
end t.nap
";

#[test]
fn without_flag_a_skipped_native_build_still_passes_in_the_bundle() {
    let t = Tmp::new_raw("test_both_default", MIXED);
    let o = run(&["test", "--both", "t.nap"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "フラグなしは従来どおり通る: {}", out);
    assert!(out.contains("test t.nap.test_nap ... ok (both: bundle"), "{}", out);
    assert!(!out.contains("FAILED"), "{}", out);
}

#[test]
fn native_required_fails_the_test_whose_native_build_is_skipped() {
    let t = Tmp::new_raw("test_both_strict", MIXED);
    let o = run(&["test", "--both", "--native-required", "t.nap"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "スキップされたテストは失敗: {}", out);
    assert!(out.contains("test t.nap.test_nap ... FAILED: native build required, but it was skipped:"), "{}", out);
    assert!(out.contains("t.nap.test_nap") && out.contains("flush"), "理由に関数と構文が出る: {}", out);
    assert!(out.contains("bind the stdout handle first"), "回避策が出る: {}", out);
    assert!(out.contains("1 failed"), "{}", out);
    assert!(!out.contains("both: bundle"), "退避して走らせない: {}", out);
}

#[test]
fn native_required_keeps_passing_tests_that_build_natively() {
    let t = Tmp::new_raw("test_both_strict_ok", MIXED);
    let o = run(&["test", "--both", "--native-required", "t.add"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "下ろせるテストは通る: {}", out);
    assert!(out.contains("test t.add.test_add ... ok"), "{}", out);
    assert!(!out.contains("native build required"), "{}", out);
}

#[test]
fn native_required_needs_both() {
    let t = Tmp::new_raw("test_both_strict_needs", MIXED);
    let o = run(&["test", "--native-required"], &t.0);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(2), "{}", err);
    assert!(err.contains("needs `--both`"), "{}", err);
}

#[test]
fn e9040_names_the_construct_in_user_terms_with_the_workaround() {
    let t = Tmp::new_raw("e9040_user_terms", ENTRY_NAP);
    let o = run(&["build", "--release"], &t.0);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{}", err);
    assert!(err.contains("E9040"), "{}", err);
    assert!(err.contains("t.nap を KIR に下ろせない"), "関数名が出る: {}", err);
    assert!(err.contains("is not supported by native code yet"), "{}", err);
    assert!(err.contains("bind the stdout handle first"), "回避策が出る: {}", err);
    for internal in ["段階 1", "型表"] {
        assert!(!err.contains(internal), "内部の言い方 `{}` が出ている: {}", internal, err);
    }
}
