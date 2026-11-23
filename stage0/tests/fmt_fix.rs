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
               fn t.nop(o: Result<u32, u32>) -> () uses [] panics never\n    match o {\n        Ok(_) => return\n        Err(_) => ()\n    }\nend t.nop\n\n\
               fn t.main() -> () uses [] panics never\n    core.mem.drop(t.pick(1u32))\n    t.nop(Ok(1u32))\n    return\nend t.main\n";
    let want = "fn t.main() -> () uses [] panics never\n    core.mem.drop(t.pick(1u32))\n    t.nop(Ok(1u32))\n    ()\nend t.main\n\n\
                fn t.nop(o: Result<u32, u32>) -> () uses [] panics never\n    match o {\n        Ok(_) => ()\n        Err(_) => ()\n    }\nend t.nop\n\n\
                fn t.pick(n: u32) -> u32 uses [] panics never\n    if n < 2u32 {\n        return n\n    }\n    n.wrapping_add(1u32)\nend t.pick\n";
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
               type t.S\n    items: std.vec.Vec<u32>\nend t.S\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let mut a = 1u32\n    let mut b = 2u32\n    let s = t.S { items: std.vec.Vec.new() }\n    let r: Result<(), std.io.Error> = match 1u32 {\n        1u32 => {\n            let v: std.vec.Vec<u32> = std.vec.Vec.new()\n            let p = t.Point { x: 1u32, y: 2u32 }\n            let l = [1u32, 2u32]\n            let n = 1u32.wrapping_add(2u32)\n            core.mem.swap(&mut a, &mut b)\n            core.mem.drop(p)\n            core.mem.drop(l)\n            core.mem.drop(v)\n            core.mem.drop(n)\n            Ok(())\n        },\n        _ => Ok(()),\n    }\n    core.mem.drop(r)\n    core.mem.drop(s)\n    Ok(())\nend t.main\n";
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

/// 引数の型が期待型と参照の段数でちょうど1つずれる (E2023): `t.a(v)` (v: u8, a(x: &u8))
/// を `t.a(&v)` にする。段数の差が1でそれ以外の型が一致するときだけ機械的に直す。
#[test]
fn ksn_fix_e2023_adds_missing_reference_to_argument() {
    let src = "fn t.a(x: &u8) -> u8 uses [] panics never\n    *x\nend t.a\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let v = 1u8\n    core.mem.drop(t.a(v))\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("fix_e2023", src);
    let c = run(&["check", "--json"], &t.0);
    let cout = text(&c.stdout);
    assert!(codes(&cout).iter().any(|c| c == "E2023"), "{}", cout);
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("t.a(&v)"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    let again = run(&["fix"], &t.0);
    assert!(!text(&again.stdout).contains("applied 1 fix(es)"), "{}", text(&again.stdout));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

/// 呼び出し元の `uses` に足りない効果を追加する (E4001)。`pub` の関数はAPIを変えるので直さない。
#[test]
fn ksn_fix_e4001_adds_missing_effect_to_uses() {
    let src = "fn t.callee() -> u8 uses [io.console] panics never\n    core.mem.drop(std.io.print_line(\"x\"))\n    1u8\nend t.callee\n\n\
               fn t.caller() -> u8 uses [] panics never\n    core.mem.drop(t.callee())\n    1u8\nend t.caller\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    core.mem.drop(t.caller())\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("fix_e4001", src);
    let c = run(&["check", "--json"], &t.0);
    let cout = text(&c.stdout);
    assert!(codes(&cout).iter().any(|c| c == "E4001"), "{}", cout);
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("fn t.caller() -> u8 uses [io.console] panics never"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    let again = run(&["fix"], &t.0);
    assert!(!text(&again.stdout).contains("applied 1 fix(es)"), "{}", text(&again.stdout));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

/// `panics never` の関数から `panics may` を呼ぶ呼び出し元の `panics never` を `panics may` にする (E4002)。
/// `pub` の関数はAPIを変えるので直さない。
#[test]
fn ksn_fix_e4002_changes_panics_never_to_may() {
    // t.callee は `core.panic.panic` を呼ぶので本体が本当に `panics may` (E4025 で `never` に縮められない)。
    // 呼び出し元 t.caller の `panics never` は `panics may` に広げられ、元に戻されない。
    let src = "fn t.callee() -> u8 uses [] panics may\n    core.panic.panic(\"x\")\nend t.callee\n\n\
               fn t.caller() -> u8 uses [] panics never\n    core.mem.drop(t.callee())\n    1u8\nend t.caller\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    core.mem.drop(t.caller())\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("fix_e4002", src);
    let c = run(&["check", "--json"], &t.0);
    let cout = text(&c.stdout);
    assert!(codes(&cout).iter().any(|c| c == "E4002"), "{}", cout);
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("fn t.caller() -> u8 uses [] panics may"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    let again = run(&["fix"], &t.0);
    assert!(!text(&again.stdout).contains("applied 1 fix(es)"), "{}", text(&again.stdout));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

/// `&mut` を必要とする呼び出しのため、束縛を `let mut` にする (E3031)。文面から変数名を得て `let` を遡る。
#[test]
fn ksn_fix_e3031_adds_mut_to_borrowed_variable() {
    let src = "fn t.bump(r: &mut u8) -> u8 uses [] panics never\n    *r\nend t.bump\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let n = 1u8\n    core.mem.drop(t.bump(&mut n))\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("fix_e3031", src);
    let c = run(&["check", "--json"], &t.0);
    let cout = text(&c.stdout);
    assert!(codes(&cout).iter().any(|c| c == "E3031"), "{}", cout);
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("    let mut n = 1u8\n"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    let again = run(&["fix"], &t.0);
    assert!(!text(&again.stdout).contains("applied 1 fix(es)"), "{}", text(&again.stdout));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

/// 型が決まらない `let` 束縛に推論型の注釈を足す (E2044)。推論型に `_` の穴がある (`Option<_>` など) ときは
/// 注釈として書いても意味を変えないので、`ksn fix` は何も直さない。
#[test]
fn ksn_fix_e2044_skips_non_concrete_inferred_type() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let n = None\n    core.mem.drop(n)\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("fix_e2044_skip", src);
    let c = run(&["check", "--json"], &t.0);
    let cout = text(&c.stdout);
    assert!(codes(&cout).iter().any(|c| c == "E2044"), "{}", cout);
    assert!(cout.contains("inferred so far: `Option<_>`"), "{}", cout);
    // 推論型に `_` が含まれるので `ksn fix` は注釈を足さず、適用件数は 0。
    let fixed = run(&["fix"], &t.0);
    assert!(!text(&fixed.stdout).contains("applied 1 fix(es)"), "{}", text(&fixed.stdout));
    // 注釈を足さないので E2044 は残る。
    let c = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&c.stdout)).iter().any(|c| c == "E2044"), "{}", text(&c.stdout));
}

// 整形の形の規則 (設計_書き方は1通り.md 12 章): 悪い形は `ksn check` が E0020 で拒み、`ksn fmt` で正規形になり、
// その結果が検査を通り、もう一度 `ksn fmt` しても変わらない (冪等)。

fn canon_main(call: &str) -> String {
    format!("\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    core.mem.drop({})\n    Ok(())\nend t.main\n", call)
}

/// `bad` (悪い形) を整形すると `must` を含み `must_not` を含まず、検査が通り、冪等になることを確かめる。
fn canon_round(name: &str, bad: &str, must: &[&str], must_not: &[&str]) {
    let t = Tmp::new_raw(name, bad);
    let c = run(&["check"], &t.0);
    let out = format!("{}{}", text(&c.stdout), text(&c.stderr));
    assert!(out.contains("E0020"), "悪い形が E0020 で拒まれない:\n{}", out);
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    for m in must {
        assert!(fixed.contains(m), "整形後に {:?} が無い:\n{}", m, fixed);
    }
    for m in must_not {
        assert!(!fixed.contains(m), "整形後に {:?} が残る:\n{}", m, fixed);
    }
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "整形後も検査が通らない:\n{}{}", text(&c.stdout), text(&c.stderr));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
    let again = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert_eq!(fixed, again, "fmt が冪等でない");
}

#[test]
fn canon_bool_if_becomes_condition() {
    let src = format!(
        "fn t.f(c: bool) -> bool uses [] panics never\n    let a = if c {{ true }} else {{ false }}\n    let b = if c {{\n        false\n    }} else {{\n        true\n    }}\n    a && b\nend t.f\n{}",
        canon_main("t.f(true)")
    );
    canon_round("canon_bool_if", &src, &["let a = c\n", "let b = !c\n"], &["{ true }", "else"]);
}

#[test]
fn canon_no_op_ref_deref_pairs_are_removed() {
    let src = format!(
        "fn t.f(c: bool) -> u32 uses [] panics never\n    let x = &7u32\n    let p = *&*x\n    let q = &*&p\n    if c {{\n        p.wrapping_add(*q)\n    }} else {{\n        p\n    }}\nend t.f\n{}",
        canon_main("t.f(true)")
    );
    canon_round("canon_refs", &src, &["let p = *x\n", "let q = &p\n"], &["*&*", "&*&"]);
}

#[test]
fn canon_integer_literals_are_decimal() {
    let src = format!(
        "fn t.f() -> u32 uses [] panics never\n    let v = 0x10u32\n    let w = 0b1111_0000u32\n    let big = 0xFFFF_FFFF_FFFF_FFFF_FFFFu128\n    core.mem.drop(big)\n    v.wrapping_add(w)\nend t.f\n{}",
        canon_main("t.f()")
    );
    canon_round("canon_radix", &src, &["let v = 16u32\n", "let w = 240u32\n", "1_208_925_819_614_629_174_706_175u128"], &["0x", "0b"]);
}

#[test]
fn canon_blank_lines_inside_bodies_are_removed() {
    let src = format!(
        "fn t.f(c: bool) -> u32 uses [] panics never\n    let a = 1u32\n\n    let b = 2u32\n\n    if c {{\n        a\n    }} else {{\n        b\n    }}\nend t.f\n{}",
        canon_main("t.f(true)")
    );
    let t = Tmp::new_raw("canon_blank", &src);
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    let f = fixed.split("end t.f").next().unwrap();
    assert!(!f.contains("\n\n"), "本体の中に空行が残る:\n{}", fixed);
    assert!(fixed.contains("end t.f\n\nfn t.main"), "定義の間は空行 1 つ:\n{}", fixed);
}

#[test]
fn canon_return_then_else_moves_else_after_if() {
    let src = format!(
        "fn t.f(c: bool) -> u32 uses [] panics never\n    if c {{\n        return 1u32\n    }} else {{\n        2u32\n    }}\nend t.f\n{}",
        canon_main("t.f(true)")
    );
    canon_round("canon_ret_else", &src, &["        return 1u32\n    }\n    2u32\nend t.f"], &["} else {"]);
}

#[test]
fn canon_return_else_with_let_is_left_alone() {
    // else の `let` を後ろへ出すと、後の `let y` と同じ名前になる (E1021) ので、`else` は残す。
    let src = format!(
        "fn t.f(c: bool) -> u32 uses [] panics never\n    if c {{\n        return 1u32\n    }} else {{\n        let y = 2u32\n        core.mem.drop(y)\n    }}\n    let y = 3u32\n    y\nend t.f\n{}",
        canon_main("t.f(true)")
    );
    let t = Tmp::new_raw("canon_let_else", &src);
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("} else {"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

#[test]
fn canon_let_then_tail_use_becomes_the_expression() {
    let src = format!(
        "fn t.f(x: u32) -> u32 uses [] panics never\n    let y = x.wrapping_add(1u32)\n    y\nend t.f\n{}",
        canon_main("t.f(1u32)")
    );
    canon_round("canon_let_tail", &src, &["    x.wrapping_add(1u32)\nend t.f"], &["let y"]);
}

#[test]
fn canon_empty_else_is_dropped() {
    let src = format!(
        "fn t.f(c: bool) -> u32 uses [] panics never\n    if c {{\n        core.mem.drop(1u32)\n    }} else {{\n    }}\n    2u32\nend t.f\n{}",
        canon_main("t.f(true)")
    );
    canon_round("canon_empty_else", &src, &["    }\n    2u32"], &["else"]);
}

#[test]
fn canon_unused_match_binding_is_underscore() {
    let src = format!(
        "fn t.f(p: (u32, u32)) -> u32 uses [] panics never\n    match p {{\n        (a, _b) => a\n    }}\nend t.f\n{}",
        canon_main("t.f((1u32, 2u32))")
    );
    canon_round("canon_arm_binding", &src, &["        (a, _) => a"], &["_b"]);
}

#[test]
fn canon_static_str_let_is_str() {
    let src = format!(
        "fn t.f() -> u32 uses [] panics never\n    let s: &'static str = \"hi\"\n    core.mem.drop(s)\n    1u32\nend t.f\n{}",
        canon_main("t.f()")
    );
    canon_round("canon_static_str", &src, &["let s: &str = \"hi\""], &["'static"]);
}

#[test]
fn canon_star_amp_of_a_place_is_the_place() {
    let src = format!(
        "fn t.f(c: bool) -> u32 uses [] panics never\n    let p = *&c\n    core.mem.drop(p)\n    1u32\nend t.f\n{}",
        canon_main("t.f(true)")
    );
    canon_round("canon_star_amp", &src, &["    let p = c\n"], &["*&"]);
}

#[test]
fn canon_mut_through_shared_ref_is_left_alone() {
    // `*&x` は可変性を変えるので、`&mut *&x` は書き換えない (書き換えると `&mut x` になり、拒まれる形が通ってしまう)。
    let src = "fn t.bad() -> u32 uses [] panics never\n    let mut x = 1u32\n    let r2 = &mut *&x\n    *r2 = 2u32\n    x\nend t.bad\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    core.mem.drop(t.bad())\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("canon_mut_shared", src);
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("let r2 = &mut *&x\n"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(1), "通ってはいけない");
}


/// 規則 O (書き方は 1 通り、2026-11-21): 診断 → `ksn fix` → `check` が通る → 2 回目の `fix` は何も変えない。
fn oneway_round_trip(name: &str, src: &str, code: &str, want: &str) {
    let t = Tmp::new_raw(name, src);
    let before = run(&["check", "--json"], &t.0);
    assert_eq!(before.status.code(), Some(1), "{}", text(&before.stdout));
    assert!(codes(&text(&before.stdout)).iter().any(|c| c == code), "{}", text(&before.stdout));
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src").join("t.ksnl")).unwrap();
    assert!(fixed.contains(want), "{fixed}");
    let c = run(&["check", "--json"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}", text(&c.stdout));
    run(&["fix"], &t.0);
    assert_eq!(fs::read_to_string(t.0.join("src").join("t.ksnl")).unwrap(), fixed);
}

#[test]
fn oneway_option_is_some_becomes_is_pattern() {
    let src = "fn t.a(o: Option<u32>, q: Option<u32>) -> bool uses [] panics never\n    let b = q\n    !o.is_none() && b.is_some()\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_is_some", src, "E2055", "!(o is None) && b is Some(_)");
}

#[test]
fn oneway_result_is_err_becomes_is_pattern() {
    let src = "fn t.a(r: Result<u32, u32>) -> bool uses [] panics never\n    r.is_err()\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_is_err", src, "E2055", "r is Err(_)");
}

#[test]
fn oneway_todo_becomes_panic_with_message() {
    let src = "fn t.a() -> () uses [] panics may\n    core.panic.todo()\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_todo", src, "E2057", "core.panic.panic(\"todo\")");
}

#[test]
fn oneway_sort_unstable_becomes_sort() {
    let src = "fn t.a(v: &mut [u32]) -> () uses [] panics never\n    v.sort_unstable()\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_sort", src, "E2058", "v.sort()");
}

#[test]
fn oneway_is_true_eq_becomes_check_eq() {
    let src = "fn t.a(v: u32) -> () uses [] panics may\n    core.check.is_true(v == 1u32)\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_is_true", src, "E2059", "core.check.eq(v, 1u32)");
}

#[test]
fn oneway_trim_pair_becomes_trim() {
    let src = "fn t.a(s: &str) -> &str uses [] panics never\n    s.trim_start().trim_end()\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_trim", src, "E2060", "s.trim()");
}

#[test]
fn oneway_push_display_literal_becomes_push_str() {
    let src = "fn t.a() -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never\n    let mut s = alloc.string.String.from_str(std.alloc.SYSTEM, \"a\")?\n    s.push_display(&\"lit\")?\n    core.mem.drop(s)\n    Ok(())\nend t.a\n\nfn t.main() -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never\n    t.a()?\n    Ok(())\nend t.main\n";
    oneway_round_trip("oneway_push_display", src, "E2061", "s.push_str(\"lit\")?");
}

#[test]
fn oneway_noop_wrapping_add_zero_is_removed() {
    let src = "fn t.a(x: u32) -> u32 uses [] panics never\n    x.wrapping_add(0u32)\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_noop_add", src, "E2062", "    x\nend t.a");
}

#[test]
fn oneway_then_some_becomes_if() {
    let src = "fn t.a(c: bool) -> Option<u32> uses [] panics never\n    c.then_some(1u32)\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_then_some", src, "E2063", "if c");
}

#[test]
fn oneway_from_str_radix_ten_becomes_parse() {
    let src = "fn t.a(s: &str) -> Result<u32, core.num.ParseIntError> uses [] panics never\n    u32.from_str_radix(s, 10u32)\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_radix", src, "E2064", "u32.parse(s)");
}

#[test]
fn oneway_clone_on_copy_becomes_the_value() {
    let src = "fn t.a(x: u32) -> u32 uses [] panics never\n    let y = x.clone()\n    core.mem.drop(0u8)\n    y\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_clone", src, "E2065", "let y = x\n");
}

#[test]
fn oneway_negated_integer_comparison_is_flipped() {
    let src = "fn t.a(x: u32, y: u32) -> bool uses [] panics never\n    let a = !(x < y)\n    a && !(y >= 3u32)\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_neg_cmp", src, "E2066", "let a = x >= y\n    a && y < 3u32");
}

#[test]
fn oneway_negated_float_comparison_is_kept() {
    let src = "fn t.a(x: f64, y: f64) -> bool uses [] panics never\n    !(x < y)\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    let t = Tmp::new_raw("oneway_neg_float", src);
    let c = run(&["check", "--json"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}", text(&c.stdout));
}

#[test]
fn oneway_turbofish_decided_by_arguments_is_removed() {
    let src = "fn t.id[T](x: T) -> T uses [] panics never\n    x\nend t.id\n\nfn t.a(n: u32) -> u32 uses [] panics never\n    t.id::<u32>(n)\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_turbofish", src, "E2067", "t.id(n)");
}

#[test]
fn oneway_turbofish_nested_on_one_line_is_removed() {
    // 同一行にネストした 2 つの `::<` がある。内側のみ引数で型が決まり、
    // 外側は受け手の型から決まるので対象外。列指定で内側の `::<` だけを外す。
    // 列不明のフォールバックは `::<` が複数ある行を直さない。
    let src = "fn t.id[T](x: T) -> T uses [] panics never\n    x\nend t.id\n\nfn t.main() -> () uses [] panics never\n    let x = t.id::<u32>(t.id::<u32>(1u32))\n    core.mem.drop(x)\n    ()\nend t.main\n";
    oneway_round_trip("oneway_turbofish_nested", src, "E2067", "t.id::<u32>(t.id(1u32))");
}

#[test]
fn oneway_is_none_on_linear_payload_becomes_is() {
    let src = "type t.Res linear\n    id: u32\nend t.Res\n\nfn t.close(r: t.Res) -> u32 uses [] panics never\n    let t.Res { id: id } = r\n    id\nend t.close\n\nfn t.main() -> () uses [] panics never\n    let o = Some(t.Res { id: 1u32 })\n    core.mem.drop(o.is_none())\n    match o {\n        Some(r) => core.mem.drop(t.close(r))\n        None => core.mem.drop(0u32)\n    }\nend t.main\n";
    oneway_round_trip("oneway_is_linear", src, "E2055", "o is None");
}

#[test]
fn oneway_none_comparison_fmt_gives_is_directly() {
    let src = "fn t.a(o: Option<u32>) -> bool uses [] panics never\n    o != None\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    let t = Tmp::new_raw("oneway_none_cmp", src);
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    let f = fs::read_to_string(t.0.join("src").join("t.ksnl")).unwrap();
    assert!(f.contains("o is Some(_)"), "{f}");
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

#[test]
fn oneway_then_some_inside_a_larger_expression_becomes_if_expression() {
    let src = "fn t.a(c: bool) -> () uses [] panics never\n    core.mem.drop(c.then_some(1u32))\n    let o = Some(c.then_some(2u32))\n    core.mem.drop(o)\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_then_some_arg", src, "E2063", "core.mem.drop(if c {");
}

#[test]
fn oneway_bool_match_becomes_if() {
    let src = "fn t.a(c: bool, o: Option<u32>, x: u32, y: u32) -> u32 uses [] panics never\n    match c {\n        true => core.mem.drop(1u32)\n        false => core.mem.drop(2u32)\n    }\n    x\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_bool_match_becomes_if", src, "E2068", "if c {\n        core.mem.drop(1u32)\n    } else {\n        core.mem.drop(2u32)\n    }");
}

#[test]
fn oneway_bool_match_blocks_become_if() {
    let src = "fn t.a(c: bool, o: Option<u32>, x: u32, y: u32) -> u32 uses [] panics never\n    let r = match c {\n        false => {\n            let z = 1u32\n            core.mem.drop(0u8)\n            z\n        }\n        true => x\n    }\n    core.mem.drop(0u8)\n    r\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_bool_match_blocks_become_if", src, "E2068", "let r = if c {\n        x\n    } else {\n        let z = 1u32\n        core.mem.drop(0u8)\n        z\n    }");
}

#[test]
fn oneway_option_match_none_unit_becomes_if_let() {
    let src = "fn t.a(c: bool, o: Option<u32>, x: u32, y: u32) -> u32 uses [] panics never\n    match o {\n        Some(v) => core.mem.drop(v)\n        None => ()\n    }\n    x\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_option_match_none_unit_becomes_if_let", src, "E2069", "if let Some(v) = o {\n        core.mem.drop(v)\n    }");
}

#[test]
fn oneway_some_literal_comparison_becomes_is() {
    let src = "fn t.a(c: bool, o: Option<u32>, x: u32, y: u32) -> u32 uses [] panics never\n    core.mem.drop(o == Some(3u32))\n    core.mem.drop(o != Some(4u32))\n    x\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_some_literal_comparison_becomes_is", src, "E2071", "core.mem.drop(o is Some(3u32))");
}

#[test]
fn oneway_counting_while_becomes_for() {
    let src = "fn t.a(c: bool, o: Option<u32>, x: u32, y: u32) -> u32 uses [] panics never\n    let mut i = 0u32\n    let mut acc = 0u32\n    while i < y {\n        acc = acc.wrapping_add(i)\n        i = i.wrapping_add(1u32)\n    }\n    acc\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_counting_while_becomes_for", src, "E2070", "for i in core.iter.range(0u32, y) {");
}

#[test]
fn oneway_identity_bit_and_neg_forms_are_removed() {
    let src = "fn t.a(c: bool, o: Option<u32>, x: u32, y: u32) -> u32 uses [] panics never\n    let a = x.wrapping_neg().wrapping_neg()\n    let b = a | 0u32\n    let c2 = b.wrapping_add(3u32).wrapping_sub(3u32)\n    core.mem.drop(0u8)\n    c2.max(c2)\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_identity_bit_and_neg_forms_are_removed", src, "E2062", "let b = a\n");
}

#[test]
fn oneway_identity_self_comparison_and_sub_are_constants() {
    let src = "fn t.a(c: bool, o: Option<u32>, x: u32, y: u32) -> u32 uses [] panics never\n    core.mem.drop(x == x)\n    core.mem.drop(x.wrapping_sub(x))\n    x\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_identity_self_comparison_and_sub_are_constants", src, "E2062", "core.mem.drop(true)");
}

#[test]
fn oneway_counting_while_that_uses_the_counter_afterwards_is_kept() {
    let src = "fn t.a(y: u32) -> u32 uses [] panics never\n    let mut i = 0u32\n    while i < y {\n        core.mem.drop(i)\n        i = i.wrapping_add(1u32)\n    }\n    i\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    let t = Tmp::new_raw("oneway_while_keep", src);
    assert_eq!(run(&["check", "--json"], &t.0).status.code(), Some(0));
}

#[test]
fn oneway_unmutated_let_mut_becomes_let() {
    let src = "fn t.a(x: u32) -> u32 uses [] panics never\n    let mut z = x\n    let mut w = x\n    w = w.wrapping_add(1u32)\n    z.wrapping_add(w)\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_let_mut", src, "E2072", "let z = x\n    let mut w = x");
}

#[test]
fn oneway_trivial_closure_becomes_the_function_value() {
    let src = "fn t.add(a: u32, b: u32) -> u32 uses [] panics never\n    a.wrapping_add(b)\nend t.add\n\nfn t.a(x: u32) -> u32 uses [] panics never\n    let f = fn(p: u32, q: u32) -> u32 capture [] {\n        t.add(p, q)\n    }\n    f(x, x)\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_closure", src, "E2073", "let f = t.add\n");
}

#[test]
fn oneway_fmt_write_literal_becomes_push_str() {
    let src = "fn t.a() -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never\n    let mut s = alloc.string.String.from_str(std.alloc.SYSTEM, \"a\")?\n    alloc.fmt.write(&mut s, &\"lit\")?\n    core.mem.drop(s)\n    Ok(())\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_fmt_write", src, "E2061", "s.push_str(\"lit\")?");
}

#[test]
fn oneway_and_with_max_constant_is_removed() {
    let src = "fn t.a(x: u32) -> u32 uses [] panics never\n    let y = x & core.num.u32.MAX\n    core.mem.drop(0u8)\n    y\nend t.a\n\nfn t.main() -> () uses [] panics never\n    ()\nend t.main\n";
    oneway_round_trip("oneway_max", src, "E2062", "let y = x\n");
}


#[test]
fn fmt_drops_import_made_unused_by_short_names_only() {
    // 短縮名への書き換えで使われなくなった `import alloc.vec` は fmt が落とす。
    let src = "import alloc.vec\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let v: alloc.vec.Vec<i32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    let _ = v\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("fmtdropimp", src);
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    let out = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(!out.contains("import alloc.vec"), "{out}");
    assert!(out.contains("std.vec.Vec<i32>"), "{out}");
    // 元から未使用の import は落とさない (E1020 のまま)。
    let src2 = "import alloc.vec\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t2 = Tmp::new_raw("fmtkeepimp", src2);
    run(&["fmt"], &t2.0);
    let out2 = fs::read_to_string(t2.0.join("src/t.ksnl")).unwrap();
    assert!(out2.contains("import alloc.vec"), "{out2}");
}

// ---- Rust 風の表面の形を、構文として読めないファイルでも `ksn fix` が正規形に直す (surface_fix.rs) ----

fn surface_round(name: &str, src: &str, gone: &str, want: &str) {
    let t = Tmp::new_raw(name, src);
    run(&["fix"], &t.0);
    let f = t.0.join("src").join("t.ksnl");
    let once = fs::read_to_string(&f).unwrap();
    assert!(once.contains(want), "{}: want {:?} in\n{}", name, want, once);
    let o = run(&["check", "--json"], &t.0);
    let got = text(&o.stdout);
    assert!(!got.contains(gone), "{}: {} remains\n{}", name, gone, got);
    run(&["fix"], &t.0);
    assert_eq!(fs::read_to_string(&f).unwrap(), once, "{}: second fix changed the file", name);
}

#[test]
fn fix_surface_enum_header_body() {
    surface_round("sf_enum", "enum t.E: A(u8), B, C\nend t.E\n\nfn t.main() -> u8 uses [] panics never\n    1u8\nend t.main\n", "E0047", "    A(u8)\n    B\n    C\n");
}

#[test]
fn fix_surface_empty_arm_and_inline_block() {
    let src = "fn t.main() -> u8 uses [] panics never\n    let mut x = 1u8\n    match Some(2u8) {\n        Some(_) => { x = 3u8; x = 4u8; }\n        None => {}\n    }\n    x\nend t.main\n";
    surface_round("sf_block", src, "E0056", "x = 3u8\n        x = 4u8\n");
}

#[test]
fn fix_surface_wrapped_struct_literal() {
    let src = "type t.P\n    a: u8\n    b: u8\nend t.P\n\nfn t.main() -> t.P uses [] panics never\n    t.P {\n        a: 1u8,\n        b: 2u8\n    }\nend t.main\n";
    surface_round("sf_lit", src, "E0053", "t.P { a: 1u8, b: 2u8 }");
}

#[test]
fn fix_surface_impl_methods() {
    let src = "trait t.Tr[E: uses, P: panics] pub\n    fn get(self: &Self) -> u8 uses [E] panics P\nend t.Tr\n\ntype t.S\n    v: u8\nend t.S\n\nimpl t.Tr<[], never> for t.S\n    fn get(self: &Self) -> u8 uses [] panics never\n        self.v\nend t.S\n\nfn t.main() -> u8 uses [] panics never\n    1u8\nend t.main\n";
    surface_round("sf_impl", src, "E0062", "fn t.S.get(self: &t.S) -> u8 uses [] panics never impls t.Tr<[], never>");
}

#[test]
fn fix_surface_one_line_definitions_and_empty_derive() {
    let src = "type t.Item derive [Debug, Clone] pub a: u32 b: u32 end t.Item\n\nenum t.E derive [Debug] pub X(u8) Y end t.E\n\ntype t.C derive [] pub\n    p: u32, q: u32\nend t.C\n\nfn t.main() -> u8 uses [] panics never\n    1u8\nend t.main\n";
    surface_round("sf_oneline", src, "E0002", "type t.Item derive [Debug, Clone] pub\n    a: u32\n    b: u32\nend t.Item");
}

#[test]
fn fix_surface_brace_definitions() {
    let src = "type t.S {\n    a: u8,\n    b: u8,\n}\n\nfn t.main() -> u8 uses [] panics never {\n    1u8\n}\n";
    surface_round("sf_brace", src, "E0043", "type t.S\n    a: u8\n    b: u8\nend t.S");
}

#[test]
fn fix_surface_colon_definitions_without_end_and_bare_end() {
    let src = "type t.S: a: u8, b: std.vec.Vec<u8, std.alloc.System>\nenum t.E: X, Y(u8)\n\nfn t.main() -> u8 uses [] panics never\n    1u8\nend\n";
    surface_round("sf_colon", src, "E0002", "type t.S\n    a: u8\n    b: std.vec.Vec<u8, std.alloc.System>\nend t.S");
}

#[test]
fn fix_surface_trait_colon_header_and_semicolon_methods() {
    let src = "trait t.Tr[E: uses, P: panics]: fn a(self: &Self) -> u8 uses [E] panics P; fn b(self: &Self) -> u8 uses [E] panics P\n\nfn t.main() -> u8 uses [] panics never\n    1u8\nend t.main\n";
    surface_round("sf_trait", src, "E0002", "trait t.Tr[E: uses, P: panics]\n    fn a(");
}

#[test]
fn fix_surface_one_line_if_else_chain() {
    let src = "fn t.main() -> u8 uses [] panics never\n    let c = true\n    let v = if c { 1u8 } else { 2u8 }\n    v\nend t.main\n";
    surface_round("sf_ifelse", src, "E0056", "if c {\n        1u8\n    } else {\n        2u8\n    }");
}

#[test]
fn fix_surface_impl_with_fqn_methods_and_indented_end() {
    let src = "trait t.Tr[E: uses, P: panics] pub\n    fn get(self: &Self) -> u8 uses [E] panics P\nend t.Tr\n\ntype t.S\n    v: u8\nend t.S\n\nimpl t.Tr<[], never> for t.S\n    fn t.S.get(self: &Self) -> u8 uses [] panics never\n        self.v\n    end t.S.get\nend t.S\n\nfn t.main() -> u8 uses [] panics never\n    1u8\nend t.main\n";
    surface_round("sf_impl_fqn", src, "E0062", "fn t.S.get(self: &t.S) -> u8 uses [] panics never impls t.Tr<[], never>");
}

#[test]
fn fix_surface_trailing_colon_header_and_spaced_static() {
    let src = "type t.S: a: std.boxed.Box<dyn t.Tr + ' static> end t.S\n\ntrait t.Tr[E: uses, P: panics]:\n    fn a(self: &Self) -> u8 uses [E] panics P\nend t.Tr\n\nfn t.main() -> u8 uses [] panics never\n    1u8\nend t.main\n";
    surface_round("sf_static", src, "E0002", "trait t.Tr[E: uses, P: panics]\n    fn a(");
}

#[test]
fn fix_surface_field_shorthand_and_empty_impl_end() {
    let src = "type t.P\n    a: u8\n    b: u8\nend t.P\n\ntrait t.Tr[E: uses, P: panics] pub\n    fn g(self: &Self) -> u8 uses [E] panics P\nend t.Tr\n\nimpl core.cmp.Eq for t.P\nend core.cmp.Eq for t.P\n\nfn t.main() -> t.P uses [] panics never\n    let a = 1u8\n    let b = 2u8\n    t.P { a, b }\nend t.main\n";
    surface_round("sf_short", src, "E0061", "t.P { a: a, b: b }");
}

// ---- `as` キャスト (E0042) の `ksn fix` ----

/// 幅が広がる `as` キャストは `T.from_src(x)` に直り、`ksn check` が通り、2 回目は無変化。
#[test]
fn fix_as_cast_widening() {
    surface_round(
        "as_widen",
        "fn t.main() -> u64 uses [] panics never\n    let x: u8 = 5u8\n    let y: u64 = x as u64\n    y\nend t.main\n",
        "E0042",
        "u64.from_u8(x)",
    );
}

/// 縮小・符号変更の `as` キャストは `try_from` が `Result` を返すため機械適用不可。
/// `ksn fix` は何も変えず、E0042 はそのまま残り、`ksn explain E0042` で直し方が示される。
#[test]
fn fix_as_cast_narrowing_left_unfixed() {
    let src = "fn t.main() -> u8 uses [] panics never\n    let x: u64 = 500u64\n    let y: u8 = x as u8\n    y\nend t.main\n";
    let t = Tmp::new_raw("as_narrow", src);
    // 修正前には E0042 が出る。
    let before = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&before.stdout)).iter().any(|c| c == "E0042"), "{}", text(&before.stdout));
    // `fix` は何も変えない（check は E0042 でまだ失敗する）。
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(1));
    let fixed = fs::read_to_string(t.0.join("src").join("t.ksnl")).unwrap();
    assert_eq!(fixed, src, "narrowing as cast must not be rewritten");
    // E0042 はそのまま。
    let after = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&after.stdout)).iter().any(|c| c == "E0042"), "{}", text(&after.stdout));
}

/// 100 文字を超える行の `let` への括り出し (fmt_hoist.rs)。`ksn fmt` が縮め、`check` が通り、`run` の出力が同じ。
fn hoist_case(name: &str, body: &str) -> (String, String) {
    let src = format!("fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n{}\n    Ok(())\nend t.main\n", body);
    let t = Tmp::new_raw(name, &src);
    let o = run(&["fmt"], &t.0);
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "not a fixpoint: {}\n{}", fixed, text(&o.stdout));
    let c = run(&["check"], &t.0);
    let out = run(&["run"], &t.0);
    (fixed, format!("{}{}|{}", text(&c.stdout), text(&c.stderr), text(&out.stdout)))
}

#[test]
fn fmt_hoists_leading_call_arguments_into_lets() {
    let body = "    let b = 1000000000000u64\n    let t = b.wrapping_add(b.wrapping_add(2000000000000u64)).wrapping_add(b.wrapping_mul(3u64)).wrapping_add(b)\n    core.mem.drop(std.io.print_line(std.string.String.new().as_str()))\n    let q = t.min(b.wrapping_add(b.wrapping_mul(2u64)).wrapping_add(b.wrapping_sub(1u64))).wrapping_add(b.wrapping_mul(8u64)).wrapping_add(t)\n    core.mem.drop(q)";
    let (fixed, outcome) = hoist_case("hoist_chain", body);
    assert!(fixed.lines().all(|l| l.chars().count() <= 100), "{}", fixed);
    assert!(fixed.contains("let q_a = "), "{}", fixed);
    assert!(!outcome.contains("E0055") && !outcome.contains("error"), "{}\n{}", fixed, outcome);
}

#[test]
fn fmt_hoists_the_left_part_of_a_condition_chain() {
    let body = "    let c = 7u32\n    let ok = c < 1u32 || c > 2048u32 || c == 5u32 || c == 6u32 || c == 9u32 || c == 10u32 || c == 11u32 || c == 12u32\n    if ok {\n        core.mem.drop(std.io.print_line(\"big\"))\n    }";
    let (fixed, outcome) = hoist_case("hoist_cond", body);
    assert!(fixed.contains("let ok_a = c < 1u32 || "), "{}", fixed);
    assert!(fixed.contains("let ok = ok_a || "), "{}", fixed);
    assert!(!outcome.contains("E0055") && !outcome.contains("error"), "{}\n{}", fixed, outcome);
}

#[test]
fn fmt_keeps_e0055_when_hoisting_would_change_meaning() {
    // `&&` 混じり・`if` の枝・閉包・借用 (`&`)・一時値を借りるメソッドの引数は括り出さない。
    let long = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    for (k, line) in [
        format!("    let r = if c > 1u32 {{ u32.from_usize({}.len()) }} else {{ 0u32 }}", long),
        format!("    let r = c < 1u32 || c > 2u32 && c < 3u32 || c > 4u32 && c < 5u32 || c < 6u32 && c > 7u32 || c == 800000000u32"),
        format!("    let r = t.f(|v| v.wrapping_add({}u32), c, c)", "1000000000000000000000000000000000000000000000000000000000000000000000"),
        format!("    let r = t.f(&c.wrapping_add({}), c)", "1u32).wrapping_add(c.wrapping_mul(7777777u32).wrapping_add(88888888u32).wrapping_add(99999999u32"),
        format!("    let r = t.f(make().as_str().len(), {}, c)", "c.wrapping_add(1000000000u32).wrapping_add(1000000000u32).wrapping_add(1000000000u32).wrapping_add(7u32)"),
    ]
    .into_iter()
    .enumerate()
    {
        let src = format!("fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let c = 1u32\n{}\n    Ok(())\nend t.main\n", line);
        let t = Tmp::new_raw(&format!("hoist_refuse{}", k), &src);
        let before = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
        run(&["fmt"], &t.0);
        let after = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
        assert_eq!(before, after, "case {} must stay unchanged", k);
        assert!(text(&run(&["check"], &t.0).stdout).contains("E0055"), "case {} keeps E0055", k);
    }
}

#[test]
fn fmt_hoists_leading_call_arguments_in_order() {
    let src = "fn t.add4(a: u64, b: u64, c: u64, d: u64) -> u64 uses [] panics never\n    a.wrapping_add(b).wrapping_add(c).wrapping_add(d)\nend t.add4\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never\n    let base = 1000000000000u64\n    let total = t.add4(base.wrapping_add(2000000000000u64), base.wrapping_mul(3u64), base.wrapping_sub(4000000000000u64), 4u64)\n    let mut line = std.string.String.new()\n    core.mem.drop(line.push_u64(total))\n    std.io.print_line(line.as_str())\nend t.main\n";
    let t = Tmp::new_raw("hoist_call", src);
    run(&["fmt"], &t.0);
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    let (a, b, c) = (fixed.find("let base_wrapping_add = ").unwrap(), fixed.find("let base_wrapping_mul = ").unwrap(), fixed.find("let base_wrapping_sub = ").unwrap());
    assert!(a < b && b < c && fixed.contains("t.add4(base_wrapping_add, base_wrapping_mul, base_wrapping_sub, 4u64)"), "{}", fixed);
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
    let o = run(&["run"], &t.0);
    assert_eq!(text(&o.stdout).trim(), "3000000000004", "{}{}", text(&o.stdout), text(&o.stderr));
}
