//! 書き方は 1 通り (設計_書き方は1通り.md の段 3): 書き換えを伴う構文の揺れ。
//! `ksn fmt` が直せるものは `ksn check` が E0020 で拒み (致命)、`ksn fmt` で正規形になる。直せないものは個別の診断。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;
use std::fs;

fn wrap(body: &str) -> String {
    format!("fn t.main() -> () uses [] panics may\n{}end t.main\n", body)
}

/// 効果の宣言は本体どおりに (D5・D13): 試験の本体は意味でなく書き方を見るので、見出しの `uses` / `panics` だけ直す。
fn effects_fix(t: &Tmp) {
    let _ = std::process::Command::new(exe()).arg("fix").env("KSN_FIX_ONLY", "E4020,E4025").current_dir(&t.0).output();
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
    effects_fix(&t);
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}: canonical form must pass check: {}", name, text(&c.stdout));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{}", name);
}

/// 正規形はそのまま check を通り、`fmt` も変えない。
fn canonical(name: &str, body: &str) {
    let t = Tmp::new_raw(name, &wrap(body));
    effects_fix(&t);
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}: {}", name, text(&c.stdout));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{}", name);
}

// ---- D18: 数値リテラルの綴り ----

#[test]
fn d18_leading_zeros_and_digit_separators() {
    roundtrip("d18_lead", "    core.mem.drop(0016u32)\n", "    core.mem.drop(16u32)\n");
    roundtrip("d18_zero", "    core.mem.drop(000u8)\n", "    core.mem.drop(0u8)\n");
    roundtrip("d18_hex", "    core.mem.drop(0x00ffu32)\n", "    core.mem.drop(255u32)\n");
    roundtrip("d18_bin", "    core.mem.drop(0b0000_0101u8)\n", "    core.mem.drop(5u8)\n");
    roundtrip("d18_sep5", "    core.mem.drop(100000u64)\n", "    core.mem.drop(100_000u64)\n");
    roundtrip("d18_sep4", "    core.mem.drop(1_000i32)\n", "    core.mem.drop(1000i32)\n");
    roundtrip("d18_neg", "    core.mem.drop(-1234567i64)\n", "    core.mem.drop(-1_234_567i64)\n");
    canonical("d18_ok", "    core.mem.drop(3_735_928_559u32)\n    core.mem.drop(65_536u32)\n    core.mem.drop(0u8)\n    core.mem.drop(9999u16)\n");
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
    // `None` との比較の正規形は `is_none()` / `is_some()` (借用するので、線形の値でも判定できる)。
    roundtrip("d11_eq_none", "    let a = Some(1u32)\n    core.mem.drop(a == None)\n", "    let a = Some(1u32)\n    core.mem.drop(a.is_none())\n");
    roundtrip("d11_ne_none", "    let a = Some(1u32)\n    core.mem.drop(a != None)\n", "    let a = Some(1u32)\n    core.mem.drop(a.is_some())\n");
    roundtrip("d11_none_eq", "    let a = Some(1u32)\n    core.mem.drop(None == a)\n", "    let a = Some(1u32)\n    core.mem.drop(a.is_none())\n");
    roundtrip("d11_none_ne", "    let a = Some(1u32)\n    core.mem.drop(None != a)\n", "    let a = Some(1u32)\n    core.mem.drop(a.is_some())\n");
    // `is` の形 (`a is None` / `a is Some(_)`) は従来どおり正規形。
    canonical("d11_is_none_stays", "    let a = Some(1u32)\n    core.mem.drop(a is None)\n");
    canonical("d11_is_some_stays", "    let a = Some(1u32)\n    core.mem.drop(a is Some(_))\n");
    roundtrip("d11_len0", "    let s = \"x\"\n    core.mem.drop(s.len() == 0usize)\n", "    let s = \"x\"\n    core.mem.drop(s.is_empty())\n");
    roundtrip("d11_len_gt0", "    let s = \"x\"\n    core.mem.drop(s.len() > 0usize)\n", "    let s = \"x\"\n    core.mem.drop(!s.is_empty())\n");
}

// G56: 真偽と 0 の literal は左右どちらにあってもよく、`!=` も同じ正規形にする。
#[test]
fn d11_literal_on_either_side_and_ne() {
    roundtrip("d11_true_eq_x", "    let a = true\n    core.mem.drop(true == a)\n", "    let a = true\n    core.mem.drop(a)\n");
    roundtrip("d11_false_eq_x", "    let a = true\n    core.mem.drop(false == a)\n", "    let a = true\n    core.mem.drop(!a)\n");
    roundtrip("d11_ne_false", "    let a = true\n    core.mem.drop(a != false)\n", "    let a = true\n    core.mem.drop(a)\n");
    roundtrip("d11_true_ne_x", "    let a = true\n    core.mem.drop(true != a)\n", "    let a = true\n    core.mem.drop(!a)\n");
    roundtrip("d11_false_ne_x", "    let a = true\n    core.mem.drop(false != a)\n", "    let a = true\n    core.mem.drop(a)\n");
    roundtrip("d11_zero_eq_len", "    let s = \"x\"\n    core.mem.drop(0usize == s.len())\n", "    let s = \"x\"\n    core.mem.drop(s.is_empty())\n");
    roundtrip("d11_len_ne_zero", "    let s = \"x\"\n    core.mem.drop(s.len() != 0usize)\n", "    let s = \"x\"\n    core.mem.drop(!s.is_empty())\n");
    roundtrip("d11_zero_ne_len", "    let s = \"x\"\n    core.mem.drop(0usize != s.len())\n", "    let s = \"x\"\n    core.mem.drop(!s.is_empty())\n");
    roundtrip("d11_zero_lt_len", "    let s = \"x\"\n    core.mem.drop(0usize < s.len())\n", "    let s = \"x\"\n    core.mem.drop(!s.is_empty())\n");
    // 左が 1 トークンでない (`!true` は式全体の一部) ・ 比較の両辺に式があるものは書き換えない。
    canonical("d11_literal_inside_expr_ok", "    let a = true\n    core.mem.drop(!true == a)\n    core.mem.drop(a == (true || false))\n");
}

/// 書き換えた形が意味を保つ: 両辺の literal の向き・`!=`・0 の向きを変えた形の出力が、正規形の期待どおりになる。
#[test]
fn d11_literal_forms_keep_meaning() {
    // (条件, 期待する出力)。条件は fmt の前の非正規形そのまま。
    let cases: [(&str, &str); 10] = [
        ("true == a", "T"),
        ("false == a", "F"),
        ("a != true", "F"),
        ("a != false", "T"),
        ("true != b", "T"),
        ("false != a", "T"),
        ("0usize == e.len()", "T"),
        ("e.len() != 0usize", "F"),
        ("0usize != s.len()", "T"),
        ("0usize < s.len()", "T"),
    ];
    let mut body = String::from("    let a = true\n    let b = false\n    let s = \"x\"\n    let e = \"\"\n");
    for (cond, _) in cases.iter() {
        body.push_str(&format!(
            "    if {} {{\n        core.mem.drop(std.io.print_line(\"T\"))\n    }} else {{\n        core.mem.drop(std.io.print_line(\"F\"))\n    }}\n",
            cond
        ));
    }
    let t = Tmp::new_raw("d11_meaning", &main_wrap("", &body));
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    effects_fix(&t);
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}", text(&c.stdout));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let got: Vec<String> = text(&o.stdout).lines().map(|l| l.trim().to_string()).collect();
    let want: Vec<String> = cases.iter().map(|(_, w)| w.to_string()).collect();
    assert_eq!(got, want);
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
    effects_fix(&t);
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
    run(&["fix"], &t.0);
    // 接尾辞を足すと注釈が冗長になる (E2053): `let a = 1u32` が唯一の形。
    assert!(main_src(&t).contains("let a = 1u32"), "{}", main_src(&t));
    effects_fix(&t);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{}", text(&run(&["check"], &t.0).stdout));
}

// ---- D11 (G56): 独自の `len` / `is_empty` があるプログラムでは `len()` の比較を書き換えない ----

#[test]
fn d11_own_len_method_suppresses_the_rewrite() {
    let pre = "type t.Item\n    n: usize\nend t.Item\n\nfn t.Item.is_empty(self: &t.Item) -> bool uses [] panics never\n    self.n == 0usize\nend t.Item.is_empty\n\n\
               fn t.Item.len(self: &t.Item) -> usize uses [] panics never\n    self.n\nend t.Item.len\n\n";
    let mk = |body: &str| format!("{}fn t.main() -> () uses [] panics may\n{}end t.main\n", pre, body);
    let t = Tmp::new_raw(
        "d11_own_len",
        &mk("    let it = t.Item { n: 0usize }\n    core.check.eq(it.len() == 0usize, true)\n    core.check.eq(it.is_empty(), true)\n    core.mem.drop(it)\n"),
    );
    // 独自の `len` があるので `fmt` は `is_empty()` に書き換えない (型を見ないので、プログラム全体で抑制する)。
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    assert_eq!(
        main_src(&t),
        mk("    let it = t.Item { n: 0usize }\n    core.check.eq(it.len() == 0usize, true)\n    core.check.eq(it.is_empty(), true)\n    core.mem.drop(it)\n"),
        "{}",
        main_src(&t)
    );
    effects_fix(&t);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{}", text(&run(&["check"], &t.0).stdout));
}

/// `None` との比較の 4 つの形を書き換えた結果が、`is_none()` / `is_some()` と同じ意味になる (`run` の出力)。
#[test]
fn d11_none_comparison_forms_keep_meaning() {
    let cases: [(&str, &str); 4] = [
        ("a != None && b == None", "T"),
        ("a == None && b != None", "F"),
        ("None != a && None == b", "T"),
        ("None == a || b != None", "F"),
    ];
    let mut body = String::from("    let a = Some(1u32)\n    let b: Option<u32> = None\n");
    for (cond, _) in cases.iter() {
        body.push_str(&format!(
            "    if {} {{\n        core.mem.drop(std.io.print_line(\"T\"))\n    }} else {{\n        core.mem.drop(std.io.print_line(\"F\"))\n    }}\n",
            cond
        ));
    }
    let t = Tmp::new_raw("d11_none_meaning", &main_wrap("", &body));
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    effects_fix(&t);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{}", text(&run(&["check"], &t.0).stdout));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let got: Vec<String> = text(&o.stdout).lines().map(|l| l.trim().to_string()).collect();
    let want: Vec<String> = cases.iter().map(|(_, w)| w.to_string()).collect();
    assert_eq!(got, want, "{}", main_src(&t));
}

// ---- Rust 風の構文 (LLM がよく書く形) の診断: E0061・E0062・`impl` の E0043 ----

#[test]
fn struct_field_shorthand_is_reported_and_fixed() {
    let src = concat!(
        "type t.Item\n",
        "    qty: u32\n",
        "    price: u32\n",
        "end t.Item\n\n",
        "fn t.main() -> () uses [] panics never\n",
        "    let qty = 2u32\n",
        "    let price = 3u32\n",
        "    let it = t.Item { qty, price }\n",
        "    core.mem.drop(it)\n",
        "end t.main\n",
    );
    let t = Tmp::new_raw("rs_shorthand", src);
    let got = text(&run(&["check", "--json"], &t.0).stdout);
    assert_eq!(got.matches("\"E0061\"").count(), 2, "{}", got);
    assert!(got.contains("(`ksn fix` で直せる)"), "{}", got);
    run(&["fix"], &t.0);
    assert!(main_src(&t).contains("let it = t.Item { qty: qty, price: price }"), "{}", main_src(&t));
    effects_fix(&t);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{}", text(&run(&["check"], &t.0).stdout));
}

#[test]
fn fn_without_module_path_is_reported() {
    // 構文として読めるファイルは lint の E0062 (実モジュール名つき) で受ける。
    let t = Tmp::new_raw("rs_fqn", "fn apply(n: u32) -> u32 uses [] panics never\n    n\nend apply\n");
    let got = text(&run(&["check", "--json"], &t.0).stdout);
    assert_eq!(got.matches("\"E0062\"").count(), 1, "{}", got);
    assert!(got.contains("fn t.apply"), "{}", got);
    // 構文エラーのファイルでは rustisms の E0062 も出る (解析できないので lint は拾えない)。
    let t = Tmp::new_raw(
        "rs_fqn_broken",
        "fn apply(n: u32) -> u32 uses [] panics never {\n    n\n}\n",
    );
    let got = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(got.contains("\"E0062\""), "{}", got);
    assert!(got.contains("\"E0043\""), "{}", got);
}

#[test]
fn impl_block_gets_its_own_guidance() {
    let src = concat!(
        "type t.Item\n",
        "    n: u32\n",
        "end t.Item\n\n",
        "trait t.Double\n",
        "    fn t.dbl(self: &t.Item) -> u32\n",
        "end t.Double\n\n",
        "impl t.Double for t.Item {\n",
        "    fn dbl(self: &t.Item) -> u32 {\n",
        "        self.n * 2u32\n",
        "    }\n",
        "}\n",
    );
    let t = Tmp::new_raw("rs_impl", src);
    let got = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(got.contains("Kasane has no `impl` block"), "{}", got);
    assert!(got.contains("`impl t.Double for t.Item` is a ONE-LINE declaration"), "{}", got);
    assert!(got.contains("fn t.Item.<method>(self: ...) -> .. impls t.Double"), "{}", got);
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
    effects_fix(&t);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    // 初期化式に呼び出しがあれば、直さず誤りのまま。
    let src = format!("{}fn t.f() -> u32 uses [] panics never\n    1u32\nend t.f\n\nfn t.main() -> () uses [] panics may\n    let p = t.P {{ b: t.f(), a: 1u32 }}\n    core.mem.drop(p)\nend t.main\n", pre);
    let t = Tmp::new_raw("d16_call", &src);
    run(&["fix"], &t.0);
    assert!(main_src(&t).contains("t.P { b: t.f(), a: 1u32 }"), "{}", main_src(&t));
    assert!(text(&run(&["check", "--json"], &t.0).stdout).contains("E2048"));
}
