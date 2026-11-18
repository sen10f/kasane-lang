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
    let t = Tmp::new("fmtbad", src);
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(1));
    // 整形すると冪等に通る。
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
}

#[test]
fn ksn_fix_also_formats() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n\n\
               fn t.a() -> u8 uses [] panics never pub\n    1u8\nend t.a\n";
    let t = Tmp::new("fix_fmt", src);
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
}

#[test]
fn inferred_functions_pass_fmt_check() {
    let src = "fn t.helper() -> u8
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

fn fmt_ok(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0), "{name}");
    let out = fs::read_to_string(t.0.join("src").join("t.ksnl")).unwrap();
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
    let t = Tmp::new("cm_tail", &format!("{MAIN2}\n// trailing comment after last def\n"));
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(1));
    let out = fs::read_to_string(t.0.join("src").join("t.ksnl")).unwrap();
    assert!(out.contains("// trailing comment after last def"), "{out}");
}

#[test]
fn fmt_keeps_comments_on_imports_when_sorting() {
    let src = format!("// about io\nimport std.io\n// about fs\nimport std.fs\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    io.print(\"x\")\n    core.mem.drop(fs.exists(\"x\"))\n    Ok(())\nend t.main\n");
    let t = Tmp::new("cm_import", &src);
    run(&["fmt"], &t.0);
    let out = fs::read_to_string(t.0.join("src").join("t.ksnl")).unwrap();
    assert!(out.starts_with("// about fs\nimport std.fs\n// about io\nimport std.io\n"), "{out}");
    run(&["fmt"], &t.0);
    assert_eq!(fs::read_to_string(t.0.join("src").join("t.ksnl")).unwrap(), out);
}
