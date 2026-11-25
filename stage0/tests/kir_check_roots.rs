//! `ksn check` の KIR の借用検査は、`main` から届かない利用者の関数 (ライブラリの関数・死んだコード) も検査する。
//! AST の借用検査を `KSN_SKIP_BORROW_DIAGS=1` で外し、KIR の側だけが出す診断 (`[kir]` E3010) が残ることを見る。
//! 隠しの切り替えは配布ビルドでは効かない: `cargo test --release --features test-hooks --test kir_check_roots` で回す。
#![cfg(feature = "test-hooks")]
#![allow(dead_code, unused_imports)]

mod common;

use common::*;
use std::fs;

const BAD: &str = "fn t.a_two(a: &mut u32, b: &mut u32) -> () uses [] panics never\n    *a = 1u32\n    *b = 2u32\nend t.a_two\n\nfn t.bad() -> u32 uses [] panics never\n    let mut x = 1u32\n    t.a_two(&mut x, &mut x)\n    x\nend t.bad\n";
const MAIN: &str = "\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";

fn kir_codes(t: &Tmp) -> String {
    let cache = std::env::temp_dir().join(format!("ksn_kirroots_cache_{}", std::process::id()));
    let o = run_with_env(&["check", "--json"], &t.0, &[("KSN_SKIP_BORROW_DIAGS", "1"), ("KSN_CACHE_DIR", cache.to_str().unwrap())]);
    text(&o.stdout)
}

#[test]
fn unreachable_function_in_a_package_with_main_is_kir_checked() {
    let t = Tmp::new_strict("kirroots_main", &format!("{}{}", BAD, MAIN));
    let out = kir_codes(&t);
    assert!(out.contains("E3010") && out.contains("[kir]"), "{}", out);
}

#[test]
fn unreachable_function_in_a_library_is_kir_checked() {
    let t = Tmp::new_strict("kirroots_lib", BAD);
    fs::write(t.0.join("kasane.toml"), "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\n\n[dependencies]\n").unwrap();
    let out = kir_codes(&t);
    assert!(out.contains("E3010") && out.contains("[kir]"), "{}", out);
}
