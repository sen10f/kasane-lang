//! 書き方は 1 通り (設計_書き方は1通り.md の段 3): 書き換えを伴う構文の揺れ。
//! `ksn fmt` が直せるものは `ksn check` が E0020 で拒み (致命)、`ksn fmt` で正規形になる。直せないものは個別の診断。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;
use std::fs;

fn wrap(body: &str) -> String {
    format!("fn t.main() -> () uses [] panics may\n{}end t.main\n", body)
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

// ---- D10: 冗長な括弧 ----

#[test]
fn d10_redundant_parens_are_removed() {
    roundtrip("d10_let", "    let a = (1u32)\n    core.mem.drop(a)\n", "    let a = 1u32\n    core.mem.drop(a)\n");
    roundtrip("d10_dbl", "    core.mem.drop(((1u32)))\n", "    core.mem.drop(1u32)\n");
    roundtrip("d10_if", "    let a = 1u32\n    if (a == 1u32) {\n        core.mem.drop(a)\n    }\n", "    let a = 1u32\n    if a == 1u32 {\n        core.mem.drop(a)\n    }\n");
    roundtrip(
        "d10_prec",
        "    let a = 1u32\n    let b = (a * 2u32) + (a)\n    core.mem.drop((b > 1u32) && (b < 9u32))\n",
        "    let a = 1u32\n    let b = a * 2u32 + a\n    core.mem.drop(b > 1u32 && b < 9u32)\n",
    );
    roundtrip("d10_recv", "    let a = 1u32\n    core.mem.drop((a).wrapping_add(1u32))\n", "    let a = 1u32\n    core.mem.drop(a.wrapping_add(1u32))\n");
    roundtrip("d10_unary", "    let a = 1i32\n    core.mem.drop(-(a))\n", "    let a = 1i32\n    core.mem.drop(-a)\n");
}

#[test]
fn d10_needed_parens_stay() {
    canonical("d10_ok1", "    let a = 1u32\n    core.mem.drop((a + 1u32) * 2u32)\n    core.mem.drop(a - (a - 1u32))\n");
    canonical("d10_ok2", "    let a = 1i32\n    core.mem.drop(-(1i32))\n    core.mem.drop(!(a < 2i32 || a > 9i32))\n    core.mem.drop((a, a))\n");
    canonical("d10_ok3", "    let a = 1u32\n    core.mem.drop((a == 1u32) == (a == 2u32))\n    core.mem.drop(a | 1u32 == 1u32 || a != 0u32)\n");
}

// ---- D11: 論理・比較・分岐の冗長 ----

#[test]
fn d11_redundant_logic_is_rewritten() {
    roundtrip("d11_not_eq", "    let a = 1u32\n    core.mem.drop(!(a == 2u32))\n", "    let a = 1u32\n    core.mem.drop(a != 2u32)\n");
    roundtrip("d11_not_ne", "    let a = 1u32\n    core.mem.drop(!(a != 2u32))\n", "    let a = 1u32\n    core.mem.drop(a == 2u32)\n");
    roundtrip("d11_notnot", "    let a = true\n    core.mem.drop(!!a)\n", "    let a = true\n    core.mem.drop(a)\n");
    roundtrip("d11_notnot_paren", "    let a = true\n    core.mem.drop(!(!a))\n", "    let a = true\n    core.mem.drop(a)\n");
    roundtrip("d11_eq_true", "    let a = true\n    core.mem.drop(a == true)\n", "    let a = true\n    core.mem.drop(a)\n");
    roundtrip("d11_eq_false", "    let a = true\n    core.mem.drop(a == false)\n", "    let a = true\n    core.mem.drop(!a)\n");
    roundtrip("d11_ne_true", "    let a = true\n    core.mem.drop(a != true)\n", "    let a = true\n    core.mem.drop(!a)\n");
    roundtrip("d11_eq_method", "    let a = 1u32\n    core.mem.drop(a.eq(&2u32))\n", "    let a = 1u32\n    core.mem.drop(a == 2u32)\n");
    roundtrip("d11_ne_method", "    let a = 1u32\n    core.mem.drop(a.ne(&2u32))\n", "    let a = 1u32\n    core.mem.drop(a != 2u32)\n");
}

#[test]
fn d11_option_and_len_checks_are_rewritten() {
    roundtrip("d11_is_some", "    let a = Some(1u32)\n    core.mem.drop(a.is_some())\n", "    let a = Some(1u32)\n    core.mem.drop(a is Some(_))\n");
    roundtrip("d11_is_none", "    let a = Some(1u32)\n    core.mem.drop(a.is_none())\n", "    let a = Some(1u32)\n    core.mem.drop(a is None)\n");
    roundtrip("d11_ne_none", "    let a = Some(1u32)\n    core.mem.drop(a != None)\n", "    let a = Some(1u32)\n    core.mem.drop(a is Some(_))\n");
    roundtrip(
        "d11_is_ok",
        "    let a: Result<u32, u32> = Ok(1u32)\n    core.mem.drop(a.is_ok())\n",
        "    let a: Result<u32, u32> = Ok(1u32)\n    core.mem.drop(a is Ok(_))\n",
    );
    roundtrip("d11_len0", "    let s = \"x\"\n    core.mem.drop(s.len() == 0usize)\n", "    let s = \"x\"\n    core.mem.drop(s.is_empty())\n");
    roundtrip("d11_len_gt0", "    let s = \"x\"\n    core.mem.drop(s.len() > 0usize)\n", "    let s = \"x\"\n    core.mem.drop(!s.is_empty())\n");
}

#[test]
fn d11_else_if_and_single_expression_arms() {
    roundtrip(
        "d11_else_if",
        "    let a = 1u32\n    if a == 1u32 {\n        core.mem.drop(1u8)\n    } else {\n        if a == 2u32 {\n            core.mem.drop(2u8)\n        }\n    }\n",
        "    let a = 1u32\n    if a == 1u32 {\n        core.mem.drop(1u8)\n    } else if a == 2u32 {\n        core.mem.drop(2u8)\n    }\n",
    );
    roundtrip(
        "d15_arm_block",
        "    let a = Some(1u32)\n    match a {\n        Some(x) => {\n            core.mem.drop(x)\n        }\n        None => {\n            ()\n        }\n    }\n",
        "    let a = Some(1u32)\n    match a {\n        Some(x) => core.mem.drop(x)\n        None => ()\n    }\n",
    );
    // 腕に複数の文があればブロックのまま。
    canonical("d15_arm_multi", "    let a = Some(1u32)\n    match a {\n        Some(x) => {\n            core.mem.drop(x)\n            core.mem.drop(1u8)\n        }\n        None => ()\n    }\n");
}

#[test]
fn d15_unit_statement_and_short_wrap() {
    roundtrip("d15_unit", "    ()\n    core.mem.drop(1u8)\n", "    core.mem.drop(1u8)\n");
    // 最後の `()` は残す (前の文の値を捨てる役 E2042 を担う)。
    canonical("d15_unit_last", "    core.mem.drop(1u8)\n    ()\n");
    roundtrip(
        "d15_wrap",
        "    core.check.eq(\n        1u32,\n        1u32,\n    )\n",
        "    core.check.eq(1u32, 1u32)\n",
    );
}

// ---- D8: メソッドの呼び方 ----

#[test]
fn d8_method_call_form() {
    let pre = "type t.P pub\n    n: u32\nend t.P\n\nfn t.P.get(self: &t.P) -> u32 uses [] panics never pub\n    self.n\nend t.P.get\n\n";
    let mk = |body: &str| format!("{}fn t.main() -> () uses [] panics may\n{}end t.main\n", pre, body);
    let t = Tmp::new_raw("d8_path", &mk("    let p = t.P { n: 1u32 }\n    core.mem.drop(t.P.get(&p))\n"));
    let o = run(&["check", "--json"], &t.0);
    assert!(text(&o.stdout).contains("E2029"), "{}", text(&o.stdout));
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    let fixed = main_src(&t);
    assert!(fixed.contains("core.mem.drop(p.get())"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{}", text(&run(&["check"], &t.0).stdout));
    // `u32.wrapping_add(a, b)` も同じ。
    let t = Tmp::new_raw("d8_prim", &wrap("    let a = 1u32\n    core.mem.drop(u32.wrapping_add(a, 2u32))\n"));
    assert!(text(&run(&["check", "--json"], &t.0).stdout).contains("E2029"));
    run(&["fix"], &t.0);
    assert!(main_src(&t).contains("core.mem.drop(a.wrapping_add(2u32))"), "{}", main_src(&t));
    // `(*r).m()` は `fmt` が `r.m()` にする。
    let src = mk("    let p = t.P { n: 1u32 }\n    let r = &p\n    core.mem.drop((*r).get())\n");
    let t = Tmp::new_raw("d8_deref", &src);
    assert!(text(&run(&["check"], &t.0).stdout).contains("E0020"));
    run(&["fmt"], &t.0);
    assert!(main_src(&t).contains("core.mem.drop(r.get())"), "{}", main_src(&t));
}

// ---- D12: 整数リテラルの接尾辞 ----

#[test]
fn d12_integer_literals_need_a_suffix() {
    let t = Tmp::new_raw("d12", &wrap("    let a: u32 = 1\n    core.mem.drop(a)\n"));
    let o = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(o.contains("E2028"), "{}", o);
    run(&["fix"], &t.0);
    assert!(main_src(&t).contains("let a: u32 = 1u32"), "{}", main_src(&t));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{}", text(&run(&["check"], &t.0).stdout));
}

// ---- D16: 構造体リテラルの欄の順 ----

#[test]
fn d16_struct_literal_field_order() {
    let pre = "type t.P pub\n    a: u32\n    b: u32\nend t.P\n\n";
    let src = format!("{}fn t.main() -> () uses [] panics may\n    let p = t.P {{ b: 2u32, a: 1u32 }}\n    core.mem.drop(p)\nend t.main\n", pre);
    let t = Tmp::new_raw("d16", &src);
    let o = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(o.contains("E2048"), "{}", o);
    run(&["fix"], &t.0);
    assert!(main_src(&t).contains("t.P { a: 1u32, b: 2u32 }"), "{}", main_src(&t));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    // 初期化式に呼び出しがあれば、直さず誤りのまま。
    let src = format!("{}fn t.f() -> u32 uses [] panics never\n    1u32\nend t.f\n\nfn t.main() -> () uses [] panics may\n    let p = t.P {{ b: t.f(), a: 1u32 }}\n    core.mem.drop(p)\nend t.main\n", pre);
    let t = Tmp::new_raw("d16_call", &src);
    run(&["fix"], &t.0);
    assert!(main_src(&t).contains("t.P { b: t.f(), a: 1u32 }"), "{}", main_src(&t));
    assert!(text(&run(&["check", "--json"], &t.0).stdout).contains("E2048"));
}
