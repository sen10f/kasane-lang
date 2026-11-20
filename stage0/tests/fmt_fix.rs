//! `fmt` / `fix` のテスト。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use common::*;

#[test]
fn fmt_check_fails_on_noncanonical_source() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never   \n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("fmtbad", src);
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(1));
    // 整形すると冪等に通る。
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
}

#[test]
fn ksn_fix_also_formats() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n\n\
               fn t.a() -> u8 uses [] panics never pub\n    1u8\nend t.a\n";
    let t = Tmp::new_raw("fix_fmt", src);
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
}

#[test]
fn inferred_functions_pass_fmt_check() {
    let src = "fn t.helper() -> u8 uses [] panics never
    1u8
end t.helper

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    core.mem.drop(t.helper())
    Ok(())
end t.main
";
    let t = Tmp::new("infer_fmt", src);
    let o = run(&["fmt", "--check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

#[test]
fn ksn_fix_removes_semicolons_and_rewrites_paths_and_compound_assignment() {
    let src = "fn t.a() -> u8 uses [] panics may\n    let mut x = 1u8;\n    x += 2u8;\n    t::b(x)\nend t.a\n\n\
               fn t.b(v: u8) -> u8 uses [] panics never\n    v\nend t.b\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    core.mem.drop(t.a())\n    Ok(())\nend t.main\n";
    let t = Tmp::new("rustisms_fix", src);
    let o = run(&["fix"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("let mut x = 1u8\n"), "{}", fixed);
    assert!(fixed.contains("x = x + 2u8"), "{}", fixed);
    assert!(fixed.contains("t.b(x)"), "{}", fixed);
}

#[test]
fn ksn_fix_adds_deref_to_a_reference_operand_of_an_operator() {
    let src = "fn t.a(c: &u8, n: u8) -> bool uses [] panics never\n    c >= b'0' && n == c && c < 58u8\nend t.a\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    core.mem.drop(t.a(&1u8, 2u8))\n    Ok(())\nend t.main\n";
    let t = Tmp::new("op_ref_fix", src);
    let o = run(&["fix"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("*c >= b'0' && n == *c && *c < 58u8"), "{}", fixed);
}

/// `impls` メソッドの `pub` を外す (E1123)。可視性はトレイトと型に従うので `pub` は不要。
#[test]
fn ksn_fix_removes_pub_from_impls_method() {
    let src = "trait t.Shape pub\n    fn area(self: &Self) -> i64 uses [] panics never\nend t.Shape\n\n\
               type t.Sq pub\n    side: i64\nend t.Sq\n\n\
               impl t.Shape for t.Sq\nfn t.Sq.area(self: &t.Sq) -> i64 uses [] panics never impls t.Shape pub\n    self.side\nend t.Sq.area\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"x\")\nend t.main\n";
    let t = Tmp::new_raw("fix_e1123", src);
    let c = run(&["check", "--json"], &t.0);
    let cout = text(&c.stdout);
    assert!(codes(&cout).iter().any(|c| c == "E1123"), "{}", cout);
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(!fixed.contains("impls t.Shape pub"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    // 冪等: 2 回目は何も直さない。
    let again = run(&["fix"], &t.0);
    assert!(!text(&again.stdout).contains("applied 1 fix(es)"), "{}", text(&again.stdout));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

/// `mut` でない変数への代入を `let mut` に直す (E3001)。
#[test]
fn ksn_fix_adds_mut_to_assigned_variable() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let x = 1u8\n    x = 2u8\n    core.mem.drop(x)\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("fix_e3001", src);
    let c = run(&["check", "--json"], &t.0);
    let cout = text(&c.stdout);
    assert!(codes(&cout).iter().any(|c| c == "E3001"), "{}", cout);
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("    let mut x = 1u8\n"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    let again = run(&["fix"], &t.0);
    assert!(!text(&again.stdout).contains("applied 1 fix(es)"), "{}", text(&again.stdout));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

/// 再帰しない `panics never` 関数から `stack N` を外す (E4011)。
#[test]
fn ksn_fix_removes_stack_from_non_recursive_function() {
    let src = "fn t.f(n: u8) -> u8 uses [] panics never stack 64\n    n\nend t.f\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    core.mem.drop(t.f(1u8))\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("fix_e4011", src);
    let c = run(&["check", "--json"], &t.0);
    let cout = text(&c.stdout);
    assert!(codes(&cout).iter().any(|c| c == "E4011"), "{}", cout);
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(!fixed.contains("stack 64"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    let again = run(&["fix"], &t.0);
    assert!(!text(&again.stdout).contains("applied 1 fix(es)"), "{}", text(&again.stdout));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

/// 構造体リテラルの丸括弧を波括弧に直す (E0048): `T(a: 1)` → `T { a: 1 }`。
#[test]
fn ksn_fix_converts_paren_struct_literal_to_braces() {
    let src = "type t.P\n    x: u32\nend t.P\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let p = t.P(x: 1u32)\n    core.mem.drop(p)\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("fix_e0048", src);
    let c = run(&["check", "--json"], &t.0);
    let cout = text(&c.stdout);
    assert!(codes(&cout).iter().any(|c| c == "E0048"), "{}", cout);
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("t.P { x: 1u32 }"), "{}", fixed);
    assert!(!fixed.contains("t.P(x: 1u32)"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    let again = run(&["fix"], &t.0);
    assert!(!text(&again.stdout).contains("applied 1 fix(es)"), "{}", text(&again.stdout));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

/// Formatter の境界: i64.MIN / u64.MAX / 0 / マルチバイトの char / バッファがちょうど・不足 / debug の char。
#[test]
fn formatter_edge_cases_run_natively() {
    let test = r#"test t.main.test_fmt_edges
    let mn = core.num.i64.MIN
    let mx = core.num.u64.MAX
    let m5 = -5i8
    let mut f1 = core.fmt.Formatter.new(32usize)
    core.mem.drop(mn.display_fmt(&mut f1))
    core.check.eq(core.str.from_utf8(f1.written()), Ok("-9223372036854775808"))
    let mut f2 = core.fmt.Formatter.new(32usize)
    core.mem.drop(mx.display_fmt(&mut f2))
    core.check.eq(core.str.from_utf8(f2.written()), Ok("18446744073709551615"))
    let z = 0u8
    let mut f3 = core.fmt.Formatter.new(4usize)
    core.mem.drop(z.display_fmt(&mut f3))
    core.check.eq(core.str.from_utf8(f3.written()), Ok("0"))
    let ce = 'é'
    let ca = 'あ'
    let mut f4 = core.fmt.Formatter.new(5usize)
    core.mem.drop(ce.display_fmt(&mut f4))
    core.mem.drop(ca.display_fmt(&mut f4))
    core.check.eq(core.str.from_utf8(f4.written()), Ok("éあ"))
    let mut f5 = core.fmt.Formatter.new(8usize)
    core.check.is_true(f5.written().len() == 0usize)
    core.check.is_true(core.fmt.Formatter.write_str(&mut f5, "12345678").is_ok())
    core.check.is_true(core.fmt.Formatter.write_str(&mut f5, "9").is_err())
    core.check.eq(core.str.from_utf8(f5.written()), Ok("12345678"))
    let big = 12345i32
    let mut f6 = core.fmt.Formatter.new(3usize)
    core.check.is_true(big.display_fmt(&mut f6).is_err())
    core.check.eq(core.str.from_utf8(f6.written()), Ok(""))
    let cx = 'x'
    let cn = '\n'
    let cq = '\''
    let ca2 = 'あ'
    let tt = true
    let mut f7 = core.fmt.Formatter.new(32usize)
    core.mem.drop(cx.debug_fmt(&mut f7))
    core.mem.drop(cn.debug_fmt(&mut f7))
    core.mem.drop(cq.debug_fmt(&mut f7))
    core.mem.drop(ca2.debug_fmt(&mut f7))
    core.mem.drop(tt.debug_fmt(&mut f7))
    core.mem.drop(m5.debug_fmt(&mut f7))
    core.check.eq(core.str.from_utf8(f7.written()), Ok("'x''\\n''\\'''あ'true-5"))
end t.main.test_fmt_edges
"#;
    let src = format!("{}\n{}", main_wrap("", ""), test);
    let t = Tmp::new("fmt_edges_both", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_fmt_edges ... ok (both: bundle (llvm skipped: "), "{}", out);
}


/// 整形 API (`alloc.fmt`): `Spec` による幅・0 埋め・16 進、整数・文字列・`derive [Debug]` の型の追記、
/// `String` の `==` と `Debug` / `Display`。`run` と `build` (LLVM) の出力が一致する。
#[test]
fn fmt_builder_runs_in_both() {
    let prefix = r#"type t.P derive [Debug] pub
    x: i32
    y: i32
end t.P

fn t.fill(b: &mut alloc.string.String<std.alloc.System>) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never
    let hex = alloc.fmt.Spec.new().with_radix(16u32).with_upper().with_width(6usize).with_fill('0')
    let dec = alloc.fmt.Spec.new().with_width(5usize)
    let lft = alloc.fmt.Spec.new().with_width(4usize).with_left().with_fill('.')
    let p = t.P { x: 1i32, y: -2i32 }
    alloc.fmt.write_u64(b, 48879u64, &hex)?
    alloc.fmt.write_str(b, "|", &dec)?
    alloc.fmt.write_i64(b, -42i64, &dec)?
    alloc.fmt.write_i64(b, -42i64, &hex)?
    alloc.fmt.write_str(b, "ab", &lft)?
    alloc.fmt.debug(b, &p)?
    alloc.fmt.write(b, &9i32)?
    alloc.fmt.write(b, &true)?
    Ok(())
end t.fill

"#;
    let body = "    let mut b = alloc.string.String.new(std.alloc.SYSTEM)\n    let mut c = alloc.string.String.new(std.alloc.SYSTEM)\n    match t.fill(&mut b) {\n        Ok(_u) => ()\n        Err(_e) => ()\n    }\n    std.io.print_line(b.as_str())?\n    match c.push_display(&b) {\n        Ok(_u) => ()\n        Err(_e) => ()\n    }\n    if b == c {\n        std.io.print_line(\"same\")?\n    }\n    match c.push_debug(&b) {\n        Ok(_u) => ()\n        Err(_e) => ()\n    }\n    std.io.print_line(c.as_str())?\n";
    let full = format!("{}fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", prefix, body);
    let t = Tmp::new("fmt_builder_both", &full);
    let o = run(&["run"], &t.0);
    let ran = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", ran, text(&o.stderr));
    let expect = "00BEEF    |  -42-0002Aab..P { x: 1, y: -2 }9true\nsame\n00BEEF    |  -42-0002Aab..P { x: 1, y: -2 }9true\"00BEEF    |  -42-0002Aab..P { x: 1, y: -2 }9true\"\n";
    assert_eq!(ran, expect);
    let o = run(&["build"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("native executable:"), "{}", out);
    let bin = t.0.join("target/debug/t.native");
    let n = Command::new(&bin).output().expect("spawn native");
    assert_eq!(text(&n.stdout), expect);
}

const MAIN2: &str = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
const FN_A: &str = "fn t.a() -> u8 uses [] panics never pub\n    1u8\nend t.a\n";

/// コメントのある `.ksnh` だけを置いた一時パッケージ (`.ksnl` は `ksn fmt` が作る)。
fn ksnh_only(name: &str, src: &str) -> Tmp {
    let t = Tmp::new_raw(name, "");
    fs::remove_file(t.0.join("src").join("t.ksnl")).unwrap();
    fs::write(t.0.join("src").join("t.ksnh"), src).unwrap();
    t
}

fn fmt_ok(name: &str, src: &str) -> String {
    let t = ksnh_only(name, src);
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0), "{name}");
    let out = fs::read_to_string(t.0.join("src").join("t.ksnh")).unwrap();
    // `.ksnl` は整形した `.ksnh` の `strip`。
    assert_eq!(fs::read_to_string(t.0.join("src").join("t.ksnl")).unwrap(), strip_ksnh(&out), "{name}");
    // 冪等で、整形後も check が通る。
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{name}: {out}");
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{name}: {out}");
    out
}

#[test]
fn fmt_keeps_header_comment() {
    let out = fmt_ok("cm_head", &format!("// header comment: keep me\n\n{MAIN2}"));
    assert_eq!(out, format!("// header comment: keep me\n{MAIN2}"));
}

#[test]
fn fmt_keeps_detached_comment_with_next_def_when_sorting() {
    // main, a の順を a, main に並べ替えても、離れたコメントは次の定義 (a) と動く。
    let out = fmt_ok("cm_mid", &format!("{MAIN2}\n// about a\n\n{FN_A}"));
    assert_eq!(out, format!("// about a\n{FN_A}\n{MAIN2}"));
}

#[test]
fn fmt_keeps_trailing_comment_and_reports_it() {
    let t = ksnh_only("cm_tail", &format!("{MAIN2}\n// trailing comment after last def\n"));
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(1));
    let out = fs::read_to_string(t.0.join("src").join("t.ksnh")).unwrap();
    assert!(out.contains("// trailing comment after last def"), "{out}");
}

#[test]
fn fmt_keeps_comments_on_imports_when_sorting() {
    let src = format!("// about io\nimport std.io\n// about fs\nimport std.fs\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    io.print(\"x\")\n    core.mem.drop(fs.exists(\"x\"))\n    Ok(())\nend t.main\n");
    let t = ksnh_only("cm_import", &src);
    run(&["fmt"], &t.0);
    let out = fs::read_to_string(t.0.join("src").join("t.ksnh")).unwrap();
    assert!(out.starts_with("// about fs\nimport std.fs\n// about io\nimport std.io\n"), "{out}");
    run(&["fmt"], &t.0);
    assert_eq!(fs::read_to_string(t.0.join("src").join("t.ksnh")).unwrap(), out);
}

// ---- 書き方は 1 通り (段 2、設計_書き方は1通り.md の D1・D4・D14・S7・S16): `ksn fmt` の出力と違えば check の誤り (E0020) ----

fn fmt_roundtrip(name: &str, src: &str, want: &str) {
    let t = Tmp::new_raw(name, src);
    let c = run(&["check", "--json"], &t.0);
    let out = text(&c.stdout);
    assert_eq!(c.status.code(), Some(1), "{}: check must fail: {}", name, out);
    assert!(codes(&out).iter().any(|c| c == "E0020"), "{}: {}", name, out);
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0), "{}", name);
    let got = text(&fs::read(t.0.join("src/t.ksnl")).unwrap());
    assert_eq!(got, want, "{}: fmt result", name);
    // 冪等 (2 回かけても同じ) で、直した結果は check を通る。
    let again = run(&["fmt"], &t.0);
    assert!(!text(&again.stdout).contains("reformatted"), "{}: {}", name, text(&again.stdout));
    assert_eq!(text(&fs::read(t.0.join("src/t.ksnl")).unwrap()), want, "{}: idempotent", name);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{}: {}", name, text(&run(&["check"], &t.0).stdout));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{}", name);
}

fn wrap_body(body: &str) -> String {
    format!(
        "fn t.main() -> Result<(), std.io.Error> uses [] panics may\n{}    Ok(())\nend t.main\n",
        body
    )
}

#[test]
fn token_spacing_is_one_way_extra_and_missing_spaces_are_errors() {
    let want = wrap_body("    let v = core.num.u32.from_u8(1u8)\n    core.check.eq(v, 1u32)\n");
    // 二重の空白・呼び出しの括弧の前の空白・`,` の後ろの詰め・括弧の内側の空白。
    let a = wrap_body("    let v = core.num.u32.from_u8 (  1u8)\n    core.check.eq(v, 1u32)\n");
    fmt_roundtrip("sp_call", &a, &want);
    let b = wrap_body("    let v = core . num . u32 . from_u8( 1u8 )\n    core.check.eq(v ,1u32 )\n");
    fmt_roundtrip("sp_dots", &b, &want);
    let c = wrap_body("    let v=core.num.u32.from_u8(1u8)\n    core.check.eq(v,1u32)\n");
    fmt_roundtrip("sp_eq", &c, &want);
}

#[test]
fn token_spacing_binary_prefix_generic_and_arrow() {
    let want = "fn t.f(x: &mut Vec<u32, std.alloc.System>, n: u32) -> Option<&u32> uses [] panics never\n    let k = !(n < 3u32)\n    let m = -1i32\n    let r = &*x\n    r.get(0usize)\nend t.f\n";
    let _ = want;
    // `-` と数字の間の空白 (仕様 3.7)、二項演算子の詰め、`->` と型引数の周りの空白、`&` と `mut` の間。
    let src = wrap_body("    let a = - 1i32\n    let b = a+1i32\n    let c = a  -  b\n    let d: Option < i32 > = None\n    core.mem.drop(c)\n    core.mem.drop(d)\n");
    let want = wrap_body("    let a = -1i32\n    let b = a + 1i32\n    let c = a - b\n    let d: Option<i32> = None\n    core.mem.drop(c)\n    core.mem.drop(d)\n");
    fmt_roundtrip("sp_ops", &src, &want);
    let hdr = "fn t.f(x: &mut u32)->() uses [] panics never\n    *x = 1u32\nend t.f\n\nfn t.main() -> () uses [] panics never\n    let mut v = 0u32\n    t.f(& mut v)\nend t.main\n";
    let hwant = "fn t.f(x: &mut u32) -> () uses [] panics never\n    *x = 1u32\nend t.f\n\nfn t.main() -> () uses [] panics never\n    let mut v = 0u32\n    t.f(&mut v)\nend t.main\n";
    fmt_roundtrip("sp_hdr", hdr, hwant);
}

#[test]
fn list_order_in_headers_is_fixed_by_fmt_and_is_an_error_in_check() {
    // uses・capture の並び (D14): fmt が並べ替え、check は E0020。
    let body = "    std.io.print_line(\"x\")?\n    core.mem.drop(std.fs.exists(\"x.txt\"))\n    Ok(())\nend t.main\n";
    let src = format!("fn t.main() -> Result<(), std.io.Error> uses [io.fs, io.console] panics never\n{}", body);
    let want = format!("fn t.main() -> Result<(), std.io.Error> uses [io.console, io.fs] panics never\n{}", body);
    fmt_roundtrip("order_uses", &src, &want);
    let src = wrap_body("    let a = 1u32\n    let b = 2u32\n    let f = fn() -> u32 capture [&b, &a] {\n        *a + *b\n    }\n    core.check.eq(f(), 3u32)\n");
    let want = wrap_body("    let a = 1u32\n    let b = 2u32\n    let f = fn() -> u32 capture [&a, &b] {\n        *a + *b\n    }\n    core.check.eq(f(), 3u32)\n");
    fmt_roundtrip("order_capture", &src, &want);
}

#[test]
fn a_tail_return_is_fixed_by_fmt_and_is_an_error_in_check() {
    // S7: 関数本体の末尾の `return x` は式 `x` に、`if` / `else` の枝と `match` の腕も同じ。値のない `return` は `()`。
    let src = "fn t.pick(n: u32) -> u32 uses [] panics never\n    if n < 2u32 {\n        return n\n    } else {\n        return n.wrapping_add(1u32)\n    }\nend t.pick\n\n\
               fn t.nop(o: Option<u32>) -> () uses [] panics never\n    match o {\n        Some(_) => return\n        None => ()\n    }\nend t.nop\n\n\
               fn t.main() -> () uses [] panics never\n    core.mem.drop(t.pick(1u32))\n    t.nop(None)\n    return\nend t.main\n";
    let want = "fn t.main() -> () uses [] panics never\n    core.mem.drop(t.pick(1u32))\n    t.nop(None)\n    ()\nend t.main\n\n\
                fn t.nop(o: Option<u32>) -> () uses [] panics never\n    match o {\n        Some(_) => ()\n        None => ()\n    }\nend t.nop\n\n\
                fn t.pick(n: u32) -> u32 uses [] panics never\n    if n < 2u32 {\n        n\n    } else {\n        n.wrapping_add(1u32)\n    }\nend t.pick\n";
    fmt_roundtrip("tail_return", src, want);
    // 途中の `return` (末尾でない) と、`else` の無い `if` の中の `return` は残る。
    let keep = "fn t.main() -> () uses [] panics never\n    core.mem.drop(t.pick(1u32))\nend t.main\n\n\
                fn t.pick(n: u32) -> u32 uses [] panics never\n    if n < 2u32 {\n        return n\n    }\n    n.wrapping_add(1u32)\nend t.pick\n";
    let t = Tmp::new_raw("tail_return_keep", keep);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{}", text(&run(&["check"], &t.0).stdout));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
}

#[test]
fn while_true_is_fixed_to_loop_by_fmt_and_is_an_error_in_check() {
    // S16
    let src = wrap_body("    let mut i = 0u32\n    while true {\n        i = i.wrapping_add(1u32)\n        if i == 3u32 {\n            break\n        }\n    }\n").replace("panics may", "panics never");
    let want = wrap_body("    let mut i = 0u32\n    loop {\n        i = i.wrapping_add(1u32)\n        if i == 3u32 {\n            break\n        }\n    }\n").replace("panics may", "panics never");
    fmt_roundtrip("while_true", &src, &want);
}

#[test]
fn fmt_is_idempotent_on_the_example_packages() {
    // 正しいコード (examples) は 1 バイトも変わらず、2 回かけても同じ。
    for name in ["demo", "features", "tour", "json"] {
        let d = root().join("examples").join(name);
        let o = run(&["fmt", "--check"], &d);
        assert_eq!(o.status.code(), Some(0), "{}: {}", name, text(&o.stdout));
        assert_eq!(run(&["check"], &d).status.code(), Some(0), "{}", name);
    }
}

#[test]
fn fmt_twice_equals_fmt_once_on_messy_sources() {
    let messy = wrap_body("    let   a=1u32\n    core . check . eq (a ,1u32)\n");
    let t = Tmp::new_raw("messy_twice", &messy);
    run(&["fmt"], &t.0);
    let once = text(&fs::read(t.0.join("src/t.ksnl")).unwrap());
    run(&["fmt"], &t.0);
    let twice = text(&fs::read(t.0.join("src/t.ksnl")).unwrap());
    assert_eq!(once, twice);
}

/// 構文の誤り (行末の `;`) と定義の順の乱れが重なるとき、`ksn fmt` は `;` を消し、定義も並べ替え、
/// `ksn check` は 0 件になる。2 回目は何も変えない (G53)。
#[test]
fn fmt_removes_semicolon_and_sorts_definitions() {
    let src = "fn t.b() -> u8 uses [] panics never\n    let x: u8 = 1u8;\n    x\nend t.b\n\n\
               fn t.a() -> u8 uses [] panics never\n    2u8\nend t.a\n";
    let t = Tmp::new_raw("fmt_semi_sort", src);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(1), "check still rejects `;` before fmt");
    let o = run(&["fmt"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stdout));
    let after = fs::read_to_string(t.0.join("src").join("t.ksnl")).unwrap();
    assert!(!after.contains(';'), "{after}");
    assert!(after.find("fn t.a").unwrap() < after.find("fn t.b").unwrap(), "{after}");
    let o = run(&["check"], &t.0);
    assert!(!String::from_utf8_lossy(&o.stdout).contains("E00"), "{}", String::from_utf8_lossy(&o.stdout));
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    assert_eq!(fs::read_to_string(t.0.join("src").join("t.ksnl")).unwrap(), after);
}

/// `;` を消しても読めないファイルは、本当の原因を行:列つきで出し、「直せない」とは言わない。
#[test]
fn fmt_reports_syntax_error_first_not_unfixable_order() {
    let src = "fn t.b() -> u8 uses [] panics never\n    let x: u8 = 1u8::foo\n    x\nend t.b\n\n\
               fn t.a() -> u8 uses [] panics never\n    2u8\nend t.a\n";
    let t = Tmp::new_raw("fmt_syntax_first", src);
    let o = run(&["fmt"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(out.contains("src/t.ksnl:2:") && out.contains("E004"), "{out}");
    assert!(!out.contains("not auto-fixable") && !out.contains("canonical grammar"), "{out}");
}

/// 文法に合わないときの E0002 は、先頭 (1 行目) ではなく本当の行を指す。
#[test]
fn grammar_mismatch_points_at_real_line() {
    let src = "fn t.main() -> u8 uses [] panics never\n    1u8\nend t.main\n\nfn t.f() -> u8 uses [] panics never\n    let = 3\nend t.f\n";
    let t = Tmp::new_raw("gram_line", src);
    let o = run(&["fmt"], &t.0);
    let out = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(out.contains("src/t.ksnl:6") || out.contains("src/t.ksnl:5"), "{out}");
}

// ---- 100 文字を超える行 (E0055) と、fmt が直せないときの位置 (G58) ----

/// 105 文字の構造体リテラルの行 (8 行目)。E0055 が行:列つきで、構造体リテラルの直し方を添えて出る。
const LONG_STRUCT: &str = "type t.Point derive [Clone, Copy] pub\n    aaaaaaaaaaaaaaaaaaaa: u32\n    bbbbbbbbbbbbbbbbbbbb: u32\n    cccccccccccccccccccc: u32\nend t.Point\n\n\
                           fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let p = t.Point { aaaaaaaaaaaaaaaaaaaa: 1u32, bbbbbbbbbbbbbbbbbbbb: 2u32, cccccccccccccccccccc: 3u32 }\n    core.mem.drop(p)\n    Ok(())\nend t.main\n";

/// 100 文字を超える行は E0055 (行:列つき、構造体リテラルの直し方つき) で、元のファイルの位置を指す。
#[test]
fn long_struct_literal_line_is_e0055_at_the_original_position() {
    let t = Tmp::new_raw("long_struct", LONG_STRUCT);
    let o = run(&["fmt"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    let out = text(&o.stdout);
    assert!(out.contains("src/t.ksnl:8:101: E0055"), "{out}");
    assert!(out.contains("struct literals are one line: bind field values with `let` first"), "{out}");
    // 半分だけ書き換えたファイルを残さない (次の診断が元のファイルの行を指す)。
    assert_eq!(fs::read_to_string(t.0.join("src/t.ksnl")).unwrap(), LONG_STRUCT);
    assert!(!out.contains("reformatted"), "{out}");
    // check も行:列つきの E0055 を出す (E0002 ではない)。
    let c = run(&["check", "--json"], &t.0);
    let cout = text(&c.stdout);
    assert!(codes(&cout).iter().any(|c| c == "E0055"), "{cout}");
    assert!(cout.contains("\"line\": 8"), "{cout}");
    assert!(cout.contains("\"col\": 101"), "{cout}");
    assert!(!codes(&cout).iter().any(|c| c == "E0002"), "{cout}");
}

/// fmt が直せないもの (100 文字を超える行) が残るとき、ファイルは書き換えず、診断は元のファイルの
/// 行:列を指す。`t.zz` (長い行) を `t.aa` より後に置くと、整形すると `t.aa` が先頭に来て行が動る。
#[test]
fn fmt_does_not_rewrite_and_points_at_the_original_line_when_a_long_line_remains() {
    let src = "fn t.zz() -> t.Point uses [] panics never\n    t.Point { aaaaaaaaaaaaaaaaaaaaaaaa: 1u32, bbbbbbbbbbbbbbbbbbbbbbbb: 2u32, cccccccccccccccccccccccc: 3u32 }\nend t.zz\n\n\
               type t.Point derive [Clone, Copy] pub\n    aaaaaaaaaaaaaaaaaaaaaaaa: u32\n    bbbbbbbbbbbbbbbbbbbbbbbb: u32\n    cccccccccccccccccccccccc: u32\nend t.Point\n\n\
               fn t.aa() -> u8 uses [] panics never\n    1u8\nend t.aa\n";
    let t = Tmp::new_raw("long_reorder", src);
    let o = run(&["fmt"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    let out = text(&o.stdout);
    assert!(out.contains("src/t.ksnl:2:101: E0055"), "{out}");
    // 並べ替えたあとの行 (6 行目) ではなく、元の 2 行目を指す。ファイルも元のまま。
    assert!(!out.contains("src/t.ksnl:6:"), "{out}");
    assert!(!out.contains("reformatted"), "{out}");
    assert_eq!(fs::read_to_string(t.0.join("src/t.ksnl")).unwrap(), src);
}

/// match の腕の末尾の `,` は `ksn fmt` が消す (G53 の `;` と同じ)。腕の本体の中の呼び出し・
/// 構造体リテラル・配列・タプルの `,` は消さない。チェックは 0 件になり、もう 1 回かけても変わらない。
#[test]
fn fmt_removes_only_the_trailing_commas_of_match_arms() {
    let src = "type t.Point derive [Clone, Copy] pub\n    x: u32\n    y: u32\nend t.Point\n\n\
               type t.S\n    items: alloc.vec.Vec<u32, std.alloc.System>\nend t.S\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let mut a = 1u32\n    let mut b = 2u32\n    let s = t.S { items: alloc.vec.Vec.new(std.alloc.SYSTEM) }\n    let r: Result<(), std.io.Error> = match 1u32 {\n        1u32 => {\n            let v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n            let p = t.Point { x: 1u32, y: 2u32 }\n            let l = [1u32, 2u32]\n            let n = 1u32.wrapping_add(2u32)\n            core.mem.swap(&mut a, &mut b)\n            core.mem.drop(p)\n            core.mem.drop(l)\n            core.mem.drop(v)\n            core.mem.drop(n)\n            Ok(())\n        },\n        _ => Ok(()),\n    }\n    core.mem.drop(r)\n    core.mem.drop(s)\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("arm_comma", src);
    // まだ整形できていないファイルは check が拒む (書き方の揺れ)。
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(1));
    let cout = text(&c.stdout);
    assert!(cout.contains("E0049") || cout.contains("E0002"), "{cout}");
    let o = run(&["fmt"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stdout));
    let after = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(!after.contains("},"), "{after}");
    assert!(!after.contains("Ok(()),"), "{after}");
    assert_eq!(after.matches(',').count(), src.matches(',').count() - 2, "{after}");
    // 残った `,` は腕の本体の中のものだけ。
    for keep in ["t.Point { x: 1u32, y: 2u32 }", "[1u32, 2u32]", "core.mem.swap(&mut a, &mut b)", "derive [Clone, Copy] pub"] {
        assert!(after.contains(keep), "{after}: {keep}");
    }
    // 直した結果は check を通り、もう 1 回かけても変わらない。
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}", text(&c.stdout));
    let again = run(&["fmt"], &t.0);
    assert!(!text(&again.stdout).contains("reformatted"), "{}", text(&again.stdout));
    assert_eq!(fs::read_to_string(t.0.join("src/t.ksnl")).unwrap(), after);
}
