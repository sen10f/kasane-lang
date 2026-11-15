//! KIR ネイティブビルドのテスト: std.json.to_string と構造体フィールドの Vec 参照戻り値。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use common::*;

/// Issue 1: std.json.to_string (and derive [ToJson]) should work in release builds.
/// Previously failed with E9040 because core.fmt and core.hash modules were being
/// lowered to KIR but contained unsupported types (Formatter, Fnv64).
#[test]
fn json_to_string_works_in_native_release() {
    let src = [
        "import std.json",
        "",
        "type t.Point derive [ToJson, FromJson] pub",
        "    x: i64 pub",
        "    y: i64 pub",
        "end t.Point",
        "",
        "fn t.main() -> Result<(), alloc.alloc.AllocError> uses [alloc, io.console] panics never",
        "    let p = t.Point { x: 1i64, y: 2i64 }",
        "    let mut out = std.json.new_text()",
        "    p.to_json(&mut out)?",
        "    core.mem.drop(std.io.print_line(out.as_str()))",
        "    Ok(())",
        "end t.main",
        "",
    ].join("\n");

    // Test both debug and release builds
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("json_to_string_{}", if release { "release" } else { "debug" }), &src, release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        let out_text = text(&o.stdout);
        assert_eq!(o.status.code(), Some(0), "{}: {}{}", if release { "release" } else { "debug" }, out_text, text(&o.stderr));
        // The output should be the JSON representation of Point { x: 1, y: 2 }
        let stdout = out_text.trim();
        assert!(stdout.contains("x") && stdout.contains("y"), "expected JSON output, got: {}", stdout);
    }
}

/// Issue 2: Methods returning `&self.field` where field is a Vec should work.
/// Previously failed with E9040 as "field move" because `borrow_header` only handled
/// `Path` operands, not `Field` operands (like `self.notes` which is a `Field` node).
#[test]
fn struct_field_vec_ref_return_works_in_native() {
    let src = [
        "import alloc.string",
        "import alloc.vec",
        "",
        "type t.Store pub",
        "    notes: alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>",
        "end t.Store",
        "",
        "fn t.Store.all(self: &t.Store) -> &alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System> uses [] panics never",
        "    &self.notes",
        "end t.Store.all",
        "",
        "fn t.main() -> Result<(), alloc.alloc.AllocError> uses [alloc, io.console] panics never",
        "    let mut s = t.Store { notes: alloc.vec.Vec.new(std.alloc.SYSTEM) }",
        "    let a = alloc.string.String.from_str(std.alloc.SYSTEM, \"a\")?",
        "    let b = alloc.string.String.from_str(std.alloc.SYSTEM, \"b\")?",
        "    core.mem.drop(s.notes.push(a)?)",
        "    core.mem.drop(s.notes.push(b)?)",
        "    let all = t.Store.all(&s)",
        "    let mut i = 0usize",
        "    let n = all.len()",
        "    while i < n {",
        "        match all.get(i) {",
        "            Some(item) => core.mem.drop(std.io.print_line(item.as_str()))",
        "            None => core.mem.drop(0u8)",
        "        }",
        "        i = i.wrapping_add(1usize)",
        "    }",
        "    Ok(())",
        "end t.main",
        "",
    ].join("\n");

    // Test both debug and release builds
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("vec_ref_return_{}", if release { "release" } else { "debug" }), &src, release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        let out_text = text(&o.stdout);
        assert_eq!(o.status.code(), Some(0), "{}: {}{}", if release { "release" } else { "debug" }, out_text, text(&o.stderr));
        // The output should be "a" and "b" on separate lines
        let stdout = out_text.trim();
        assert!(stdout.contains("a") && stdout.contains("b"), "expected 'a' and 'b' in output, got: {}", stdout);
    }
}

/// Additional test: Verify that both issues are fixed together in the same program.
#[test]
fn json_and_vec_ref_together_in_native() {
    let src = [
        "import std.json",
        "import alloc.string",
        "import alloc.vec",
        "",
        "type t.Point derive [ToJson, FromJson] pub",
        "    x: i64 pub",
        "    y: i64 pub",
        "end t.Point",
        "",
        "type t.Store pub",
        "    notes: alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>",
        "    point: t.Point",
        "end t.Store",
        "",
        "fn t.Store.all_notes(self: &t.Store) -> &alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System> uses [] panics never",
        "    &self.notes",
        "end t.Store.all_notes",
        "",
        "fn t.Store.get_point(self: &t.Store) -> &t.Point uses [] panics never",
        "    &self.point",
        "end t.Store.get_point",
        "",
        "fn t.main() -> Result<(), alloc.alloc.AllocError> uses [alloc, io.console] panics never",
        "    let point = t.Point { x: 10i64, y: 20i64 }",
        "    let mut s = t.Store { notes: alloc.vec.Vec.new(std.alloc.SYSTEM), point: point }",
        "    let a = alloc.string.String.from_str(std.alloc.SYSTEM, \"hello\")?",
        "    let b = alloc.string.String.from_str(std.alloc.SYSTEM, \"world\")?",
        "    core.mem.drop(s.notes.push(a)?)",
        "    core.mem.drop(s.notes.push(b)?)",
        "    // Test Vec reference return",
        "    let all = t.Store.all_notes(&s)",
        "    let mut i = 0usize",
        "    let n = all.len()",
        "    while i < n {",
        "        match all.get(i) {",
        "            Some(item) => core.mem.drop(std.io.print_line(item.as_str()))",
        "            None => core.mem.drop(0u8)",
        "        }",
        "        i = i.wrapping_add(1usize)",
        "    }",
        "    // Test struct reference return and JSON serialization",
        "    let p = t.Store.get_point(&s)",
        "    let mut out = std.json.new_text()",
        "    p.to_json(&mut out)?",
        "    core.mem.drop(std.io.print_line(out.as_str()))",
        "    Ok(())",
        "end t.main",
        "",
    ].join("\n");

    // Test both debug and release builds
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("json_and_vec_ref_{}", if release { "release" } else { "debug" }), &src, release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        let out_text = text(&o.stdout);
        assert_eq!(o.status.code(), Some(0), "{}: {}{}", if release { "release" } else { "debug" }, out_text, text(&o.stderr));
        // The output should contain "hello", "world" and JSON with x: 10, y: 20
        let stdout = out_text.trim();
        assert!(stdout.contains("hello") && stdout.contains("world"), "expected 'hello' and 'world' in output, got: {}", stdout);
        assert!(stdout.contains("10") && stdout.contains("20"), "expected JSON with 10 and 20, got: {}", stdout);
    }
}