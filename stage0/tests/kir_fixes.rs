//! KIR → LLVM の下ろしの不足を直した形ごとのテスト (kir/fixes-todo)。
//!
//! 各形は `ksn build --release` (退避なし) が「native executable」を作り、その実行結果が
//! `ksn run` (インタプリタ) の出力・終了コードと一致することを確かめる。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use common::*;

/// `ksn build --release` (退避なし) でネイティブを作り、その出力が `ksn run` と一致することを確かめる。
/// 退避 (バンドル) で通ってしまうのを防ぐため、`native executable:` の表示と `skipped` の不在を見る。
fn strict_native_matches_run(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    let b = run(&["build", "--release"], &t.0);
    let out = text(&b.stdout) + &text(&b.stderr);
    assert_eq!(b.status.code(), Some(0), "build --release が失敗: {}", out);
    assert!(out.contains("native executable:"), "ネイティブが作れていない: {}", out);
    assert!(!out.contains("skipped"), "退避している: {}", out);
    let exe = t.0.join("target").join("release").join(if cfg!(windows) { "t.exe" } else { "t.native" });
    let n = Command::new(&exe).current_dir(&t.0).output().expect("native");
    let r = run(&["run"], &t.0);
    same_output(&r, &n);
    text(&n.stdout)
}

/// `Result` を返す `main` の中の、else の無い 1 腕の `if` (then 枝の値が `?` の結果 `u32`)。
/// 検査器はこの `if` の型を then 枝の型 (`u32`) にするが、KIR は unit の値として下ろす
/// (旧: 合流の引数の型が合わず `b3 の引数の型が合わない` で退避していた)。
#[test]
fn a_one_armed_if_with_a_non_unit_tail_lowers_natively() {
    let src = "fn t.hello(x: u32) -> Result<u32, std.io.Error> uses [io.console] panics may\n\
               \x20   std.io.print_line(\"hi\")?\n\
               \x20   Ok(x)\n\
               end t.hello\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n\
               \x20   let c = true\n\
               \x20   if c {\n\
               \x20       core.mem.drop(t.hello(3u32)?)\n\
               \x20   }\n\
               \x20   std.io.print_line(\"end\")?\n\
               \x20   Ok(())\n\
               end t.main\n";
    assert_eq!(strict_native_matches_run("one_armed_if", src), "hi\nend\n");
}

/// 同じ形で、then 枝の値が所有物 (`String`) のときも捨てて解放し、二重解放や漏れにならない。
#[test]
fn a_one_armed_if_discards_an_owned_tail_value() {
    let src = "fn t.name() -> alloc.string.String<std.alloc.System> uses [alloc] panics never\n\
               \x20   let mut s = alloc.string.String.new(std.alloc.SYSTEM)\n\
               \x20   match s.push_str(\"abc\") {\n\
               \x20       Ok(_) => 0u8\n\
               \x20       Err(_) => 1u8\n\
               \x20   }\n\
               \x20   s\n\
               end t.name\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n\
               \x20   let c = true\n\
               \x20   if c {\n\
               \x20       core.mem.drop(t.name())\n\
               \x20   }\n\
               \x20   std.io.print_line(\"end\")?\n\
               \x20   Ok(())\n\
               end t.main\n";
    assert_eq!(strict_native_matches_run("one_armed_if_owned", src), "end\n");
}

/// 標準ライブラリの型のうち `.ksnl` の宣言で与えられるもの (`std.json.Error`) を、`Result` の E として下ろす。
/// 旧: 型が KIR の表に無く `std.json.Error は未対応` で退避していた。
#[test]
fn a_std_type_declared_in_kasane_source_lowers_natively() {
    let src = "fn t.read(text: &str) -> Result<i64, std.json.Error> uses [alloc] panics never\n\
               \x20   let doc = std.json.parse(text)?\n\
               \x20   std.json.decode_i64(doc.root())\n\
               end t.read\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n\
               \x20   match t.read(\"42\") {\n\
               \x20       Ok(n) => {\n\
               \x20           if n == 42i64 {\n\
               \x20               std.io.print_line(\"ok 42\")?\n\
               \x20           }\n\
               \x20       }\n\
               \x20       Err(_e) => std.io.print_line(\"bad\")?\n\
               \x20   }\n\
               \x20   match t.read(\"[]\") {\n\
               \x20       Ok(_n) => std.io.print_line(\"unexpected\")?\n\
               \x20       Err(_e) => std.io.print_line(\"err\")?\n\
               \x20   }\n\
               \x20   Ok(())\n\
               end t.main\n";
    assert_eq!(strict_native_matches_run("std_json_error", src), "ok 42\nerr\n");
}

/// `Result<&T, E>` (T は所有物を持つ型): 番地を借用として返し (`Ok(p)`)、`?` で取り出し、`match` の `Ok(p)` で束縛する。
#[test]
fn a_result_of_a_borrowed_owning_element_lowers_natively() {
    let src = "type t.P pub\n\
               \x20   name: alloc.string.String<std.alloc.System>\n\
               \x20   n: u32\n\
               end t.P\n\n\
               fn t.pick(v: &alloc.vec.Vec<t.P, std.alloc.System>) -> Result<&t.P, std.io.Error> uses [] panics never\n\
               \x20   match v.as_slice().last() {\n\
               \x20       Some(p) => Ok(p)\n\
               \x20       None => Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 0i32 })\n\
               \x20   }\n\
               end t.pick\n\n\
               fn t.n_of(v: &alloc.vec.Vec<t.P, std.alloc.System>) -> Result<u32, std.io.Error> uses [] panics never\n\
               \x20   let p = t.pick(v)?\n\
               \x20   Ok(p.n)\n\
               end t.n_of\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n\
               \x20   let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)\n\
               \x20   match v.push(t.P { name: alloc.string.String.new(std.alloc.SYSTEM), n: 7u32 }) {\n\
               \x20       Ok(_) => 0u8\n\
               \x20       Err(_) => 1u8\n\
               \x20   }\n\
               \x20   let n = t.n_of(&v)?\n\
               \x20   if n == 7u32 {\n\
               \x20       std.io.print_line(\"seven\")?\n\
               \x20   }\n\
               \x20   match t.pick(&v) {\n\
               \x20       Ok(p) => {\n\
               \x20           if p.n == 7u32 {\n\
               \x20               std.io.print_line(\"match ok\")?\n\
               \x20           }\n\
               \x20       }\n\
               \x20       Err(_e) => std.io.print_line(\"none\")?\n\
               \x20   }\n\
               \x20   Ok(())\n\
               end t.main\n";
    assert_eq!(strict_native_matches_run("result_borrow", src), "seven\nmatch ok\n");
}

/// `Option<&Enum>` (列挙体が所有物を持つ) の `match` で、列挙子のパターン (`Some(E.V(a, b))`) を判定し、
/// スカラーのフィールドは値で、所有物のフィールドは番地の借用で束縛する。`None` の番地は読まない。
#[test]
fn an_option_of_a_borrowed_enum_matches_variant_patterns() {
    let src = "enum t.Node pub\n\
               \x20   Num(alloc.string.String<std.alloc.System>)\n\
               \x20   Pair(u32, u32)\n\
               \x20   Empty\n\
               end t.Node\n\n\
               fn t.sum_at(nodes: &alloc.vec.Vec<t.Node, std.alloc.System>, at: usize) -> u32 uses [] panics never\n\
               \x20   match nodes.as_slice().get(at) {\n\
               \x20       Some(t.Node.Pair(a, b)) => {\n\
               \x20           let x = *a\n\
               \x20           x.wrapping_add(*b)\n\
               \x20       }\n\
               \x20       Some(t.Node.Num(s)) => {\n\
               \x20           if s.len() == 3usize {\n\
               \x20               100u32\n\
               \x20           } else {\n\
               \x20               1u32\n\
               \x20           }\n\
               \x20       }\n\
               \x20       Some(t.Node.Empty) => 0u32\n\
               \x20       None => 0u32\n\
               \x20   }\n\
               end t.sum_at\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n\
               \x20   let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)\n\
               \x20   let mut s = alloc.string.String.new(std.alloc.SYSTEM)\n\
               \x20   match s.push_str(\"abc\") {\n\
               \x20       Ok(_) => 0u8\n\
               \x20       Err(_) => 1u8\n\
               \x20   }\n\
               \x20   match v.push(t.Node.Num(s)) {\n\
               \x20       Ok(_) => 0u8\n\
               \x20       Err(_) => 1u8\n\
               \x20   }\n\
               \x20   match v.push(t.Node.Pair(3u32, 4u32)) {\n\
               \x20       Ok(_) => 0u8\n\
               \x20       Err(_) => 1u8\n\
               \x20   }\n\
               \x20   match v.push(t.Node.Empty) {\n\
               \x20       Ok(_) => 0u8\n\
               \x20       Err(_) => 1u8\n\
               \x20   }\n\
               \x20   if t.sum_at(&v, 0usize) == 100u32 {\n\
               \x20       std.io.print_line(\"num\")?\n\
               \x20   }\n\
               \x20   if t.sum_at(&v, 1usize) == 7u32 {\n\
               \x20       std.io.print_line(\"pair\")?\n\
               \x20   }\n\
               \x20   if t.sum_at(&v, 2usize) == 0u32 {\n\
               \x20       std.io.print_line(\"empty\")?\n\
               \x20   }\n\
               \x20   if t.sum_at(&v, 9usize) == 0u32 {\n\
               \x20       std.io.print_line(\"none\")?\n\
               \x20   }\n\
               \x20   Ok(())\n\
               end t.main\n";
    assert_eq!(strict_native_matches_run("option_enum_borrow", src), "num\npair\nempty\nnone\n");
}

/// 値の構造体の中の共有の参照 (`v.doc`) を通るフィールドの `Vec` (`v.doc.nodes.as_slice()`) を、読むだけのメソッドで使う。
#[test]
fn a_vec_field_reached_through_a_shared_reference_field_is_readable() {
    let src = "type t.Doc pub\n\
               \x20   nodes: alloc.vec.Vec<u32, std.alloc.System>\n\
               end t.Doc\n\n\
               type t.View['a] derive [Clone, Copy] pub\n\
               \x20   doc: &'a t.Doc\n\
               \x20   at: usize\n\
               end t.View\n\n\
               fn t.first(v: t.View) -> Option<u32> uses [] panics never\n\
               \x20   match v.doc.nodes.as_slice().get(v.at) {\n\
               \x20       Some(n) => Some(*n)\n\
               \x20       None => None\n\
               \x20   }\n\
               end t.first\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n\
               \x20   let mut nodes = alloc.vec.Vec.new(std.alloc.SYSTEM)\n\
               \x20   match nodes.push(41u32) {\n\
               \x20       Ok(_) => 0u8\n\
               \x20       Err(_) => 1u8\n\
               \x20   }\n\
               \x20   let d = t.Doc { nodes: nodes }\n\
               \x20   let v = t.View { doc: &d, at: 0usize }\n\
               \x20   match t.first(v) {\n\
               \x20       Some(x) => {\n\
               \x20           if x == 41u32 {\n\
               \x20               std.io.print_line(\"first 41\")?\n\
               \x20           }\n\
               \x20       }\n\
               \x20       None => std.io.print_line(\"none\")?\n\
               \x20   }\n\
               \x20   Ok(())\n\
               end t.main\n";
    assert_eq!(strict_native_matches_run("view_field_chain", src), "first 41\n");
}
