//! 書き方は 1 通り (設計_書き方は1通り.md の段 3): 書き換えを伴う構文の揺れ。
//! `ksn fmt` が直せるものは `ksn check` が E0020 で拒み (致命)、`ksn fmt` で正規形になる。直せないものは個別の診断。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;
use std::fs;

fn wrap(body: &str) -> String {
    format!("fn t.main() -> () uses [] panics never\n{}end t.main\n", body)
}

fn main_src(t: &Tmp) -> String {
    text(&fs::read(t.0.join("src/t.ksnl")).unwrap())
}

/// 非正規形の本体が E0020 で拒まれ、`fmt` が `want` にし (冪等)、正規形が check を通る。
fn roundtrip(name: &str, bad: &str, want: &str) {
    let t = Tmp::new_raw(name, &wrap(bad));
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(1), "{}: check must fail: {}{}", name, text(&c.stdout), text(&c.stderr));
    assert!(text(&c.stdout).contains("E0020"), "{}: {}", name, text(&c.stdout));
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0), "{}", name);
    assert_eq!(main_src(&t), wrap(want), "{}: fmt result", name);
    run(&["fmt"], &t.0);
    assert_eq!(main_src(&t), wrap(want), "{}: fmt must be idempotent", name);
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}: canonical form must pass check: {}", name, text(&c.stdout));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{}", name);
}

/// 正規形はそのまま check を通り、`fmt` も変えない。
fn canonical(name: &str, body: &str) {
    let t = Tmp::new_raw(name, &wrap(body));
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}: {}", name, text(&c.stdout));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{}", name);
}

// ---- D18: 数値リテラルの綴り ----

#[test]
fn d18_leading_zeros_and_digit_separators() {
    roundtrip("d18_lead", "    core.mem.drop(0016u32)\n", "    core.mem.drop(16u32)\n");
    roundtrip("d18_zero", "    core.mem.drop(000u8)\n", "    core.mem.drop(0u8)\n");
    roundtrip("d18_hex", "    core.mem.drop(0x00ffu32)\n", "    core.mem.drop(0xffu32)\n");
    roundtrip("d18_bin", "    core.mem.drop(0b0000_0101u8)\n", "    core.mem.drop(0b101u8)\n");
    roundtrip("d18_sep5", "    core.mem.drop(100000u64)\n", "    core.mem.drop(100_000u64)\n");
    roundtrip("d18_sep4", "    core.mem.drop(1_000i32)\n", "    core.mem.drop(1000i32)\n");
    roundtrip("d18_neg", "    core.mem.drop(-1234567i64)\n", "    core.mem.drop(-1_234_567i64)\n");
    canonical("d18_ok", "    core.mem.drop(0xdead_beefu32)\n    core.mem.drop(65_536u32)\n    core.mem.drop(0u8)\n    core.mem.drop(9999u16)\n");
}

#[test]
fn d18_float_spelling() {
    roundtrip("d18_f1", "    core.mem.drop(1.50f64)\n", "    core.mem.drop(1.5f64)\n");
    roundtrip("d18_f2", "    core.mem.drop(1e5f64)\n", "    core.mem.drop(100000.0f64)\n");
    roundtrip("d18_f3", "    core.mem.drop(0.00001f64)\n", "    core.mem.drop(1.0e-5f64)\n");
    roundtrip("d18_f4", "    core.mem.drop(1e16f64)\n", "    core.mem.drop(1.0e16f64)\n");
    roundtrip("d18_f5", "    core.mem.drop(1.0e+3f64)\n", "    core.mem.drop(1000.0f64)\n");
    roundtrip("d18_f6", "    core.mem.drop(12.5e-9f64)\n", "    core.mem.drop(1.25e-8f64)\n");
    canonical("d18_fok", "    core.mem.drop(0.0001f64)\n    core.mem.drop(9999999999999999.0f64)\n    core.mem.drop(2.5e-7f32)\n");
}
