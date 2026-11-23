//! `ksn check --signatures-only` (インターフェース先行の検査)。本体の無い `fn` (ヘッダの直後に `end`) は、
//! このフラグのときだけ受ける。シグネチャ・型・境界・import・他モジュールからの参照は検査し、本体に依る検査は飛ばす。
//! フラグの無い `ksn check` は従来どおり (戻り値が `()` でない本体の無い関数は E2020)。`ksn build` / `run` / `test` は E0067 で止める。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;
use std::fs;

const GEO_HEADER_ONLY: &str = "fn t.geo.area(n: i64) -> i64 uses [] panics never pub\nend t.geo.area\n";
const MAIN_USES_GEO: &str = "import t.geo\n\nfn t.main() -> i64 uses [] panics never\n    t.geo.area(2i64)\nend t.main\n";

fn both(out: &std::process::Output) -> String {
    format!("{}{}", text(&out.stdout), text(&out.stderr))
}

fn check_sigs(name: &str, files: &[(&str, &str)]) -> (std::process::Output, String) {
    let t = tmp_files(name, files);
    let out = run(&["check", "--signatures-only"], &t.0);
    let s = both(&out);
    (out, s)
}

#[test]
fn header_only_modules_pass_with_flag_and_fail_without() {
    let files = [("t.ksnl", MAIN_USES_GEO), ("t/geo.ksnl", GEO_HEADER_ONLY)];
    let (out, s) = check_sigs("sigs_pass", &files);
    assert!(out.status.success(), "signatures-only check should pass:\n{}", s);

    // フラグの無い `check` は従来どおり: 戻り値が `()` でない本体の無い `fn` は型の誤り (E2020)。
    let t = tmp_files("sigs_nofl", &files);
    let out = run(&["check"], &t.0);
    assert!(!out.status.success(), "without the flag a header-only fn must fail");
    assert!(both(&out).contains("E2020"), "{}", both(&out));

    // `build` / `run` / `test` は本体の無い定義を受けない (フラグは `check` だけのもの)。
    let out = run(&["run"], &t.0);
    assert!(!out.status.success());
    assert!(both(&out).contains("E0067"), "{}", both(&out));
    let out = run(&["build"], &t.0);
    assert!(!out.status.success());
    assert!(both(&out).contains("E0067"), "{}", both(&out));
}

#[test]
fn wrong_type_in_signature_still_fails_with_flag() {
    let geo = "fn t.geo.area(n: i64) -> t.geo.Missing uses [] panics never pub\nend t.geo.area\n";
    let (out, s) = check_sigs("sigs_badtype", &[("t.ksnl", MAIN_USES_GEO), ("t/geo.ksnl", geo)]);
    assert!(!out.status.success(), "{}", s);
    assert!(s.contains("E1102"), "{}", s);
}

#[test]
fn undefined_function_from_body_module_fails_with_flag() {
    let main = "import t.geo\n\nfn t.main() -> i64 uses [] panics never\n    t.geo.missing(2i64)\nend t.main\n";
    let (out, s) = check_sigs("sigs_undef", &[("t.ksnl", main), ("t/geo.ksnl", GEO_HEADER_ONLY)]);
    assert!(!out.status.success(), "{}", s);
    assert!(s.contains("E1101"), "{}", s);
}
