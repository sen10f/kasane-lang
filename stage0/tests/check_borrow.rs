//! 借用検査・move・一時値の解放の診断のテスト。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use common::*;

/// `unsound/linear_*` は、線形型の診断 (E3002 / E3003 / E3006) で落ちなければならない
/// (別の診断で落ちているだけのケースを許さない)。
#[test]
fn linear_corpus_is_rejected_for_linear_reasons() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/unsound");
    let mut n = 0;
    for e in fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().map_or(true, |x| x != "ksnl") {
            continue;
        }
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        if !stem.starts_with("linear_") {
            continue;
        }
        n += 1;
        let t = Tmp::new(&format!("linear_why_{}", stem), &fs::read_to_string(&p).unwrap());
        let out = text(&run(&["check", "--json"], &t.0).stdout);
        assert!(codes(&out).iter().any(|c| matches!(c.as_str(), "E3002" | "E3003" | "E3006")), "{}: {}", stem, out);
    }
    assert!(n >= 20, "linear cases missing");
}

// ---- JSON パーサ作成中に見つかった誤検出の回帰テスト ----

#[test]
fn mut_ref_param_can_be_passed_repeatedly() {
    // &mut の引数を続けて渡すのは再借用であり move ではない (誤検出 E3002 の回帰)。
    let f = "fn t.a(v: &mut u8) -> () uses [] panics never\n    core.mem.drop(v)\nend t.a\n\n\
             fn t.b(v: &mut u8) -> () uses [] panics never\n    t.a(v)\n    t.a(v)\nend t.b\n\n";
    let t = Tmp::new("reborrow", &main_wrap(f, ""));
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E3002"), "{}", text(&o.stdout));
}

// ---- 借用・境界・注釈の検査 (types.rs) ----

#[test]
fn mut_method_on_an_immutable_local_is_rejected() {
    let (got, out) = check_codes(
        "bor_imm",
        &["let v = alloc.vec.Vec.new(std.alloc.SYSTEM)", "core.mem.drop(v.push(1u8))"],
    );
    assert!(got.iter().any(|c| c == "E3031"), "{}", out);
    assert!(out.contains("let mut v"), "{}", out);
}

#[test]
fn mut_method_on_a_mut_local_or_mut_ref_is_accepted() {
    let f = "fn t.fill(v: &mut alloc.vec.Vec<u8, std.alloc.System>) -> () uses [alloc] panics may\n    core.mem.drop(v.push(1u8))\nend t.fill\n\n";
    let src = main_wrap(f, &body(&["let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)", "core.mem.drop(v.push(2u8))", "t.fill(&mut v)"]));
    let t = Tmp::new("bor_ok", &src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(!codes(&out).iter().any(|c| c == "E3031"), "{}", out);
}

#[test]
fn mut_method_through_a_shared_reference_is_rejected() {
    let f = "fn t.fill(v: &alloc.vec.Vec<u8, std.alloc.System>) -> () uses [alloc] panics may\n    core.mem.drop(v.push(1u8))\nend t.fill\n\n";
    let t = Tmp::new("bor_shared", &main_wrap(f, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|c| c == "E3031"), "{}", out);
    assert!(out.contains("shared reference"), "{}", out);
}

// ---- NLL に基づく借用検査 (borrowck.rs) ----

fn has_e3010(name: &str, prefix: &str, b: &[&str]) -> (bool, String) {
    let t = Tmp::new(name, &main_wrap(prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out).iter().any(|c| c == "E3010"), out)
}

#[test]
fn nll_rejects_a_mutation_while_a_shared_borrow_is_still_used() {
    let (bad, out) = has_e3010(
        "nll_shared",
        "",
        &["let mut x = 1u8", "let r = &x", "x = 2u8", "core.check.eq(*r, 1u8)"],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_rejects_any_use_while_a_mutable_borrow_is_still_used() {
    let (bad, out) = has_e3010(
        "nll_mut",
        "",
        &["let mut x = 1u8", "let r = &mut x", "core.check.eq(x, 1u8)", "*r = 2u8"],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot use `x` while it is mutably borrowed by `r`"), "{}", out);
}

#[test]
fn nll_rejects_pushing_to_a_vec_while_a_reference_into_it_is_used() {
    let (bad, out) = has_e3010(
        "nll_first",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "match v.push(1u8) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "let first = v.first()",
            "match v.push(2u8) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "core.check.eq(first, Some(1u8))",
        ],
    );
    assert!(bad, "{}", out);
}

#[test]
fn nll_rejects_mutating_a_vec_inside_a_loop_over_its_iterator() {
    let (bad, out) = has_e3010(
        "nll_iter",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "match v.push(1u8) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "for x in v.iter() {",
            "    match v.push(*x) {",
            "        Ok(_u) => 0u8",
            "        Err(_e) => 1u8",
            "    }",
            "}",
        ],
    );
    assert!(bad, "{}", out);
}

#[test]
fn nll_accepts_code_where_the_borrow_is_dead_before_the_conflict() {
    // 借用の最後の使用が、変更より前にある。
    let (bad, out) = has_e3010("nll_dead", "", &["let mut x = 1u8", "let r = &x", "core.check.eq(*r, 1u8)", "x = 2u8", "core.check.eq(x, 2u8)"]);
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_reassigned_reference_even_if_the_old_borrow_conflicts() {
    // r は再代入される前の値をもう使わない: 古い借用は、再代入の前にすでに死んでいる。
    let (bad, out) = has_e3010(
        "nll_reassign",
        "",
        &[
            "let mut x = 1u8",
            "let mut y = 2u8",
            "let mut r = &mut x",
            "*r = 9u8",
            "core.check.eq(x, 9u8)",
            "r = &mut y",
            "*r = 3u8",
            "core.check.eq(y, 3u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_borrow_used_only_on_the_other_branch() {
    let (bad, out) = has_e3010(
        "nll_branch",
        "",
        &[
            "let mut x = 1u8",
            "let flag = true",
            "let r = &mut x",
            "if flag {",
            "    *r = 5u8",
            "} else {",
            "    core.check.eq(1u8, 1u8)",
            "}",
            "x = 7u8",
            "core.check.eq(x, 7u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_treats_distinct_fields_of_a_struct_as_disjoint() {
    let prefix = "type t.S\n    a: u8\n    b: u8\nend t.S\n\n";
    let (bad, out) = has_e3010(
        "nll_fields",
        prefix,
        &["let mut s = t.S { a: 1u8, b: 2u8 }", "let ra = &mut s.a", "s.b = 9u8", "*ra = 3u8", "core.check.eq(s.b, 9u8)"],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_two_phase_borrows_like_push_of_len() {
    let (bad, out) = has_e3010(
        "nll_twophase",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "match v.push(1u8) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "match v.push(2u8) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "core.check.eq(v.len(), 2usize)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_borrow_is_stored_in_a_struct_field() {
    let (bad, out) = has_e3010(
        "nll_store_field",
        NLL_REF_FIELD,
        &["let mut x = 1u8", "let s = t.S { f: &x, b: 2u8 }", "x = 2u8", "core.check.eq(s.b, 2u8)"],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `s.f`"), "{}", out);
}

#[test]
fn nll_rejects_any_use_while_a_mutable_borrow_is_stored_in_a_vec() {
    let (bad, out) = has_e3010(
        "nll_store_vec_mut",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "match v.push(&mut x) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "core.check.eq(x, 1u8)",
            "core.check.eq(v.len(), 1usize)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot use `x` while it is mutably borrowed by `v`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_stored_borrow_is_used_in_a_match_arm() {
    let (bad, out) = has_e3010(
        "nll_store_match",
        "",
        &[
            "let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "match m.insert(1u8, &x) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "match m.get(1u8) {",
            "    Some(rr) => {",
            "        x = 2u8",
            "        core.check.eq(*rr, 1u8)",
            "    }",
            "    None => 0u8",
            "}",
        ],
    );
    assert!(bad, "{}", out);
}

#[test]
fn nll_rejects_a_mutation_after_a_stored_borrow_is_read_back() {
    let (bad, out) = has_e3010(
        "nll_store_read",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "match v.push(&x) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "let r = v.first().unwrap()",
            "x = 2u8",
            "core.check.eq(*r, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_accepts_a_stored_borrow_overwritten_before_the_conflict() {
    // フィールドへの再代入で、前に保存された借用は死ぬ。
    let (bad, out) = has_e3010(
        "nll_store_overwrite",
        NLL_REF_FIELD,
        &[
            "let mut x = 1u8",
            "let mut y = 2u8",
            "let mut s = t.S { f: &x, b: 2u8 }",
            "s.f = &y",
            "core.check.eq(x, 1u8)",
            "core.check.eq(s.f, &y)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_vec_reassigned_before_the_conflict() {
    // コンテナごと作り直すと、保存されていた借用は死ぬ。
    let (bad, out) = has_e3010(
        "nll_store_reassign",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "match v.push(&x) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "core.check.eq(x, 1u8)",
            "core.check.eq(v.len(), 0usize)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_stored_borrow_dropped_before_the_conflict() {
    // `core.mem.drop(s.f)` でフィールドの値を消費すると、保存された借用は死ぬ。
    let (bad, out) = has_e3010(
        "nll_store_drop",
        NLL_REF_FIELD,
        &[
            "let mut x = 1u8",
            "let mut s = t.S { f: &x, b: 2u8 }",
            "core.mem.drop(s.f)",
            "core.check.eq(x, 1u8)",
            "core.check.eq(s.b, 2u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_mutation_after_the_storage_holder_is_dead() {
    // 保存場所の根の変数がもう使われないなら、借りた元は自由に使える。
    let (bad, out) = has_e3010(
        "nll_store_dead",
        NLL_REF_FIELD,
        &[
            "let mut x = 1u8",
            "let s = t.S { f: &x, b: 2u8 }",
            "core.check.eq(s.b, 2u8)",
            "x = 5u8",
            "core.check.eq(x, 5u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_rejects_a_mutation_of_the_borrowed_value_inside_a_loop_over_the_container() {
    let (bad, out) = has_e3010(
        "nll_store_loop",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "match v.push(&x) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "for r in v.iter() {",
            "    x = 2u8",
            "    core.check.eq(*r, 1u8)",
            "}",
        ],
    );
    assert!(bad, "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_borrow_is_carried_through_a_match_value() {
    // 腕の束縛が持つローンは、`match` の値として返した先 (let の束縛) へ運ばれる。
    let (bad, out) = has_e3010(
        "nll_match_carry",
        "",
        &[
            "let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "let mut y = 2u8",
            "match m.insert(1u8, &x) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "let r = match m.get(1u8) {",
            "    Some(rr) => rr",
            "    None => &y",
            "}",
            "x = 2u8",
            "core.check.eq(*r, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_accepts_a_match_binding_that_is_not_returned_as_the_value() {
    // 腕の束縛がローンを持っていても、腕の値として返さなければ match の値はローンを持たない。
    let (bad, out) = has_e3010(
        "nll_match_no_carry",
        "",
        &[
            "let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "match m.insert(1u8, &x) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "let n = match m.get(1u8) {",
            "    Some(rr) => {",
            "        core.check.eq(*rr, 1u8)",
            "        0u8",
            "    }",
            "    None => 1u8",
            "}",
            "x = 2u8",
            "core.check.eq(n, 0u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_borrow_is_stored_in_a_tuple() {
    let (bad, out) = has_e3010(
        "nll_store_tuple",
        "",
        &["let mut x = 1u8", "let t = (&x, 2u8)", "x = 2u8", "core.mem.drop(t)"],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `t`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_borrow_is_stored_in_an_array_literal() {
    let (bad, out) = has_e3010(
        "nll_store_array",
        "",
        &["let mut x = 1u8", "let a = [&x]", "x = 2u8", "core.mem.drop(a)"],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `a`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_after_a_tuple_is_destructured() {
    // `let (a, b) = t` の分解で、保存されたローンは各束縛へ読み戻される。
    let (bad, out) = has_e3010(
        "nll_store_tuple_destructure",
        "",
        &[
            "let mut x = 1u8",
            "let t = (&x, 2u8)",
            "let (a, b) = t",
            "x = 2u8",
            "core.check.eq(*a, 1u8)",
            "core.check.eq(b, 2u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `a`"), "{}", out);
}

#[test]
fn nll_accepts_a_tuple_overwritten_before_the_conflict() {
    // タプルごと上書きすると、保存されていた借用は死ぬ。
    let (bad, out) = has_e3010(
        "nll_store_tuple_overwrite",
        "",
        &[
            "let mut x = 1u8",
            "let mut y = 2u8",
            "let mut t = (&x, 1u8)",
            "t = (&y, 2u8)",
            "core.check.eq(x, 1u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_tuple_borrow_dropped_before_the_conflict() {
    let (bad, out) = has_e3010(
        "nll_store_tuple_drop",
        "",
        &[
            "let mut x = 1u8",
            "let t = (&x, 2u8)",
            "core.mem.drop(t)",
            "x = 2u8",
            "core.check.eq(x, 2u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

// ---- 式の中の借用・クロージャ・関数境界・コンテナ (borrowck.rs) ----

/// 診断が 1 件もないこと (他の検査も含めて) を確かめる。
fn nll_clean(name: &str, prefix: &str, b: &[&str]) {
    let src = main_wrap(prefix, &body(b)).replace("uses [io.console]", "uses [alloc, io.console]");
    let t = Tmp::new(name, &src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}: {}", name, text(&o.stdout));
}

const NLL_FNS: &str = "type t.S\n    a: u8\n    b: u8\nend t.S\n\n\
    fn t.f_pair['a](a: &'a mut u8, b: &'a mut u8) -> (&'a mut u8, &'a mut u8) uses [] panics never\n    (a, b)\nend t.f_pair\n\n\
    fn t.f_pick['a, 'b](x: &'a u8, _y: &'b u8) -> &'a u8 uses [] panics never\n    x\nend t.f_pick\n\n\
    fn t.f_two(a: &mut u8, b: &mut u8) -> () uses [] panics never\n    *a = 1u8\n    *b = 2u8\nend t.f_two\n\n\
    fn t.f_use(a: &u8, b: &u8) -> () uses [] panics never\n    core.mem.drop(a)\n    core.mem.drop(b)\nend t.f_use\n\n";

#[test]
fn nll_expr_accepts_disjoint_and_shared_borrows_in_one_expression() {
    nll_clean(
        "nll_expr_ok",
        NLL_FNS,
        &[
            "let mut s = t.S { a: 1u8, b: 2u8 }",
            "t.f_two(&mut s.a, &mut s.b)",
            "let (pa, pb) = (&mut s.a, &mut s.b)",
            "*pa = 5u8",
            "*pb = 6u8",
            "let x = 1u8",
            "t.f_use(&x, &x)",
            "let arr = [&x, &x]",
            "core.check.eq(arr.len(), 2usize)",
            "let mut y = 1u8",
            "t.f_two(&mut y, &mut s.a)",
            "t.f_two(&mut y, &mut s.a)",
            "core.check.eq(s.a, 1u8)",
        ],
    );
}

#[test]
fn nll_expr_accepts_two_phase_push_of_len() {
    nll_clean(
        "nll_expr_twophase",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "match v.push(1usize) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "match v.push(v.len()) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "core.check.eq(v.len(), 2usize)",
        ],
    );
}

#[test]
fn nll_expr_accepts_a_borrow_that_ends_before_the_conflicting_access() {
    nll_clean(
        "nll_expr_ended",
        NLL_FNS,
        &[
            "let mut x = 1u8",
            "let y = 2u8",
            "let r = t.f_pick(&x, &y)",
            "core.check.eq(*r, 1u8)",
            "x = 3u8",
            "core.check.eq(x, 3u8)",
        ],
    );
}

#[test]
fn nll_boundary_follows_only_the_arguments_the_header_returns() {
    // `f_pick` の戻り値は第 1 引数にだけ由来する。第 2 引数への書き込みは、戻り値が生きていても通る。
    nll_clean(
        "nll_boundary_other_arg",
        NLL_FNS,
        &["let x = 1u8", "let mut y = 2u8", "let r = t.f_pick(&x, &y)", "y = 3u8", "core.check.eq(*r, 1u8)", "core.check.eq(y, 3u8)"],
    );
    let (bad, out) = has_e3010(
        "nll_boundary_first_arg",
        NLL_FNS,
        &["let mut x = 1u8", "let y = 2u8", "let r = t.f_pick(&x, &y)", "x = 3u8", "core.check.eq(*r, 1u8)"],
    );
    assert!(bad, "{}", out);
}

#[test]
fn nll_boundary_struct_result_keeps_the_argument_borrows() {
    let (bad, out) = has_e3010(
        "nll_boundary_struct",
        NLL_FNS,
        &["let mut a = 1u8", "let mut b = 2u8", "let (_pa, pb) = t.f_pair(&mut a, &mut b)", "a = 3u8", "*pb = 4u8"],
    );
    assert!(bad, "{}", out);
    // 戻り値を使い終えてから触る分には通る。
    nll_clean(
        "nll_boundary_struct_ok",
        NLL_FNS,
        &["let mut a = 1u8", "let mut b = 2u8", "let (_pa, pb) = t.f_pair(&mut a, &mut b)", "*pb = 4u8", "a = 3u8", "core.check.eq(a, 3u8)"],
    );
}

#[test]
fn nll_closure_dead_before_the_captured_variable_is_mutated() {
    nll_clean(
        "nll_closure_dead",
        "",
        &[
            "let mut x = 1u8",
            "let c = fn() -> u8 capture [&x] {",
            "    x",
            "}",
            "core.check.eq(c(), 1u8)",
            "x = 2u8",
            "let d = fn() -> () capture [&mut x] {",
            "    x = 3u8",
            "}",
            "d()",
            "core.check.eq(x, 3u8)",
        ],
    );
}

#[test]
fn nll_closure_body_may_read_a_shared_capture_and_write_a_mut_capture() {
    nll_clean(
        "nll_closure_body_ok",
        "",
        &[
            "let x = 1u8",
            "let mut y = 2u8",
            "let c = fn() -> u8 capture [&x, &mut y] {",
            "    y = x",
            "    y",
            "}",
            "core.check.eq(c(), 1u8)",
        ],
    );
}

#[test]
fn nll_store_keeps_every_borrow_pushed_into_one_container() {
    // 参照は二級の値 (E3060): 容れ物に参照を入れる形は、借用の流れを見る前に型の位置で拒む。
    let src = main_wrap("", &body(&[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "let y = 2u8",
            "match v.push(&y) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "match v.push(&y) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "core.check.eq(v.len(), 2usize)",
            "x = 2u8",
            "core.check.eq(x, 2u8)",
        ])).replace("uses [io.console]", "uses [alloc, io.console]");
    let t = Tmp::new("nll_store_two_ok", &src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|c| c == "E3060"), "{}", out);
}

#[test]
fn nll_mem_replace_with_a_dead_holder_is_accepted() {
    nll_clean(
        "nll_mem_replace_ok",
        "type t.H\n    r: &'static u8\nend t.H\n\n",
        &[
            "let a = 1u8",
            "let mut x = 2u8",
            "let mut h = t.H { r: &a }",
            "let old = core.mem.replace(&mut h.r, &x)",
            "core.check.eq(*h.r, 2u8)",
            "core.check.eq(*old, 1u8)",
            "x = 3u8",
            "core.check.eq(x, 3u8)",
        ],
    );
}

#[test]
fn linear_ok_cases_pass_check() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/linear_ok");
    let mut names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    assert!(!names.is_empty(), "linear_ok cases missing");
    let mut failures = Vec::new();
    for p in names {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("ok_{}", stem), &fs::read_to_string(&p).unwrap());
        let o = run(&["check", "--json"], &t.0);
        let got = codes(&text(&o.stdout));
        if o.status.code() != Some(0) || !got.is_empty() {
            failures.push(format!("{}: rc={:?} codes={:?}", stem, o.status.code(), got));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// 再借用・`move` の参照の取り込み・共有参照越しの `Fn` / 可変参照越しの `FnMut` の呼び出しの、正しい形は
/// `ksn check` を通る (`KNOWN_HOLES` の規則の誤検出の回帰)。閉包を参照する形は KIR に下ろせないので
/// (ネイティブ一致の検査の対象になる) `linear_ok/` には置かず、ここで check だけを見る。
#[test]
fn reborrow_and_closure_call_controls_pass_check() {
    let cases: &[(&str, &str)] = &[
        (
            "ctl_move_ref_capture",
            r#"fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let mut x = 1u32
    let r = &mut x
    let c = fn() -> u32 capture [move r] {
        *r
    }
    let a = c()
    core.mem.drop(a)
    let y = x
    core.mem.drop(y)
    Ok(())
end t.main
"#,
        ),
        (
            "ctl_shared_fn_and_mut_fn_calls",
            r#"fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let mut x = 1u32
    let c = fn() -> u32 capture [&x] {
        x
    }
    let cr = &c
    let a = cr()
    core.mem.drop(a)
    let mut m = fn() -> () capture [&mut x] {
        x = 2u32
    }
    let mr = &mut m
    mr()
    core.mem.drop(x)
    Ok(())
end t.main
"#,
        ),
    ];
    for (name, src) in cases {
        let t = Tmp::new(name, src);
        let o = run(&["check", "--json"], &t.0);
        assert_eq!(o.status.code(), Some(0), "{}: {}", name, text(&o.stdout));
    }
}

// 木走査: 取り込みは作る時点で決まる (4.3)。外側の後の書き換えは見えない。
#[test]
fn move_capture_copies_the_value_when_the_closure_is_made() {
    run_ok(
        "clo_move_copy",
        &body(&[
            "let mut x = 1u64",
            "let f = fn() -> u64 capture [move x] {",
            "    x",
            "}",
            "x = 5u64",
            "core.check.eq(f(), 1u64)",
            "core.check.eq(x, 5u64)",
        ]),
    );
}

// 木走査: `&mut x` の本体では `*x = v` が外側の `x` へ書く (KIR と同じ)。
#[test]
fn mut_ref_capture_writes_through_deref_assignment() {
    run_ok(
        "clo_mut_deref",
        &body(&[
            "let mut x = 1u64",
            "let f = fn(v: u64) -> () capture [&mut x] {",
            "    *x = v",
            "}",
            "f(7u64)",
            "core.check.eq(x, 7u64)",
        ]),
    );
}

// 木走査: `&mut x` の本体で `x = v` と書いても、仕様の「本体で `a` と書く」形として外側へ書く。
#[test]
fn mut_ref_capture_writes_through_plain_name_assignment() {
    run_ok(
        "clo_mut_name",
        &body(&[
            "let mut x = 1u64",
            "let g = fn(v: u64) -> () capture [&mut x] {",
            "    x = v",
            "}",
            "g(9u64)",
            "core.check.eq(x, 9u64)",
        ]),
    );
}

// 木走査: `move` で取り込んだ値は、クロージャの呼び出しをまたいで残る (FnMut の状態)。外側は変わらない。
#[test]
fn move_captured_state_persists_across_calls_without_touching_the_outer() {
    run_ok(
        "clo_move_state",
        &body(&[
            "let mut n = 0u64",
            "let mut inc = fn() -> u64 capture [move n] {",
            "    n = n + 1u64",
            "    n",
            "}",
            "core.check.eq(inc(), 1u64)",
            "core.check.eq(inc(), 2u64)",
            "core.check.eq(n, 0u64)",
        ]),
    );
}

// ---- 借用の残り: match の値を通じた運搬、タプル・配列リテラルへの保存 ----

const NLL_PUSH1: [&str; 5] =
    ["let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)", "match v.push(1u8) {", "    Ok(_u) => 0u8", "    Err(_e) => 1u8", "}"];
const NLL_PUSH2: [&str; 4] = ["match v.push(2u8) {", "    Ok(_u) => 0u8", "    Err(_e) => 1u8", "}"];

fn nll_lines(parts: &[&[&'static str]]) -> Vec<&'static str> {
    parts.concat()
}

#[test]
fn nll_rejects_a_mutation_while_a_match_value_carries_the_arm_binding_borrow() {
    // `let r = match v.first() { Some(rr) => rr, .. }`: 被検査式のローンが腕の束縛を経て r に移る。
    let b = nll_lines(&[
        &NLL_PUSH1,
        &["let y = 9u8", "let r = match v.first() {", "    Some(rr) => rr", "    None => &y", "}"],
        &NLL_PUSH2,
        &["core.check.eq(*r, 1u8)"],
    ]);
    let (bad, out) = has_e3010("nll_match_val_bind", "", &b);
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `v` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_borrowed_by_any_arm_of_a_match_value() {
    // 両腕の和: どちらの腕の借用も r が持つ。
    let (bad, out) = has_e3010(
        "nll_match_val_arms",
        "",
        &[
            "let mut x = 1u8",
            "let y = 2u8",
            "let b = true",
            "let r = match b {",
            "    true => &x",
            "    false => &y",
            "}",
            "x = 3u8",
            "core.check.eq(*r, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_nested_match_value_carries_a_borrow() {
    let b = nll_lines(&[
        &NLL_PUSH1,
        &[
            "let y = 9u8",
            "let b = true",
            "let r = match b {",
            "    true => match v.first() {",
            "        Some(rr) => rr",
            "        None => &y",
            "    }",
            "    false => &y",
            "}",
        ],
        &NLL_PUSH2,
        &["core.check.eq(*r, 1u8)"],
    ]);
    let (bad, out) = has_e3010("nll_match_val_nested", "", &b);
    assert!(bad, "{}", out);
}

#[test]
fn nll_accepts_a_match_value_that_copies_out_of_the_borrow() {
    // 腕の値が `*rr` (コピー) なら、match の値は借用を運ばない。
    let b = nll_lines(&[
        &NLL_PUSH1,
        &["let n = match v.first() {", "    Some(rr) => *rr", "    None => 0u8", "}"],
        &NLL_PUSH2,
        &["core.check.eq(n, 1u8)"],
    ]);
    let (bad, out) = has_e3010("nll_match_val_copy", "", &b);
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_match_value_that_is_dead_before_the_mutation() {
    let b = nll_lines(&[
        &NLL_PUSH1,
        &["let y = 9u8", "let r = match v.first() {", "    Some(rr) => rr", "    None => &y", "}", "core.check.eq(*r, 1u8)"],
        &NLL_PUSH2,
        &["core.check.eq(v.len(), 2usize)"],
    ]);
    let (bad, out) = has_e3010("nll_match_val_dead", "", &b);
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_match_value_inside_a_loop_without_leaking_the_previous_iteration() {
    let b = nll_lines(&[
        &NLL_PUSH1,
        &[
            "let mut i = 0u8",
            "while i < 3u8 {",
            "    let n = match v.first() {",
            "        Some(rr) => *rr",
            "        None => 0u8",
            "    }",
            "    match v.push(n) {",
            "        Ok(_u) => 0u8",
            "        Err(_e) => 1u8",
            "    }",
            "    i = i + 1u8",
            "}",
            "core.check.eq(v.len(), 4usize)",
        ],
    ]);
    let (bad, out) = has_e3010("nll_match_val_loop", "", &b);
    assert!(!bad, "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_borrow_is_stored_in_a_tuple_literal() {
    let (bad, out) =
        has_e3010("nll_store_tuple_lit", "", &["let mut x = 1u8", "let t = (&x, 1u8)", "x = 2u8", "let _u = t"]);
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `t`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_tuple_with_a_borrow_is_pushed_into_a_vec() {
    let (bad, out) = has_e3010(
        "nll_store_push_tuple",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "match v.push((&x, 1u8)) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "x = 2u8",
            "core.check.eq(v.len(), 1usize)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `v`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_inside_a_loop_over_an_array_literal_of_borrows() {
    let (bad, out) = has_e3010(
        "nll_store_array_iter",
        "",
        &["let mut x = 1u8", "let y = 2u8", "let a = [&x, &y]", "for r in a.iter() {", "    x = 5u8", "    core.check.eq(**r, 2u8)", "}"],
    );
    assert!(bad, "{}", out);
}

#[test]
fn nll_accepts_a_tuple_literal_that_holds_no_borrow_or_is_dead() {
    let (bad, out) = has_e3010(
        "nll_store_tuple_ok",
        "",
        &[
            "let mut x = 1u8",
            "let t = (x, 1u8)",
            "let t2 = (&x, 1u8)",
            "let _u = t2",
            "x = 2u8",
            "let _w = t",
            "core.check.eq(x, 2u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_an_array_literal_overwritten_before_the_conflict() {
    // 変数の再代入・要素の上書きで、保存されていた借用は死ぬ。
    let (bad, out) = has_e3010(
        "nll_store_array_ok",
        "",
        &[
            "let mut x = 1u8",
            "let y = 2u8",
            "let mut a = [&x, &y]",
            "a = [&y, &y]",
            "let mut c = [&x]",
            "c[0] = &y",
            "x = 5u8",
            "core.check.eq(*a[0], 2u8)",
            "core.check.eq(*c[0], 2u8)",
            "core.check.eq(x, 5u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

// ---- 一時値の解放の残り: Field・タプルの型解決と E2042 / E3003 (4.4・4.6) ----

fn tdrop_codes(name: &str, b: &[&str]) -> (Vec<String>, String) {
    let nl = char::from(10u8).to_string();
    let prefix = tdrop_prefix().join(&nl) + &nl + &nl;
    let t = Tmp::new(name, &main_wrap(&prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

#[test]
fn tdrop_discarding_a_field_of_a_temporary_is_e2042() {
    // `t.get_w().lin` は Field の道。受け手の型 (t.W) からフィールドの型 (t.L) を解決する。
    let (cs, out) = tdrop_codes("tdrop_field", &["t.get_w().lin"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
    assert!(out.contains("core.mem.drop(t.get_w().lin)"), "{}", out);
}

#[test]
fn tdrop_discarding_a_tuple_of_temporaries_is_e2042() {
    let (cs, out) = tdrop_codes("tdrop_tuple", &["(t.get_l(), 1u8)"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tdrop_discarding_a_local_field_path_is_e2042() {
    // 複数セグメントの Path (`p.a`) も根の型からフィールドの型をたどる。
    let (cs, out) = tdrop_codes("tdrop_path", &["let p = t.P { a: 1u8 }", "p.a"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tdrop_struct_with_an_unconsumed_linear_field_is_e3003() {
    // t.W は linear と宣言していないが線形なフィールドを含むので線形 (4.4)。
    // 非線形なフィールド w.n だけを使っても w.lin は消費されていない。
    let (cs, out) = tdrop_codes(
        "tdrop_wfield",
        &["let w = t.W { lin: t.get_l(), n: 1u8 }", "core.mem.drop(w.n)"],
    );
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `w` is never consumed"), "{}", out);
}

#[test]
fn tdrop_consuming_the_linear_field_or_the_whole_struct_is_ok() {
    let (cs, out) = tdrop_codes(
        "tdrop_wfield_ok",
        &[
            "let w = t.W { lin: t.get_l(), n: 1u8 }",
            "t.eat(w.lin)",
            "let w2 = t.get_w()",
            "t.eat_w(w2)",
        ],
    );
    assert!(!tdrop_has(&cs, "E3003"), "{}", out);
}

#[test]
fn tdrop_tuple_destructuring_leaves_a_linear_value_unconsumed() {
    let (cs, out) = tdrop_codes(
        "tdrop_ptuple",
        &["let (a, b) = (t.get_l(), 1u8)", "core.mem.drop(b)"],
    );
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `a` is never consumed"), "{}", out);
}

#[test]
fn tdrop_tuple_binding_with_a_linear_element_is_e3003() {
    let (cs, out) = tdrop_codes("tdrop_tuple_bind", &["let _p = (t.get_l(), 1u8)"]);
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
}

#[test]
fn tdrop_struct_destructuring_leaves_a_linear_field_unconsumed() {
    let (cs, out) = tdrop_codes(
        "tdrop_pstruct",
        &["let t.W { lin: l, n: n } = t.get_w()", "core.mem.drop(n)"],
    );
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `l` is never consumed"), "{}", out);
}

#[test]
fn tdrop_destructured_linear_values_that_are_consumed_are_ok() {
    let (cs, out) = tdrop_codes(
        "tdrop_destr_ok",
        &[
            "let (a, b) = (t.get_l(), 1u8)",
            "t.eat(a)",
            "core.mem.drop(b)",
            "let t.W { lin: l, n: n } = t.get_w()",
            "t.eat(l)",
            "core.mem.drop(n)",
            "let x: u8 = t.get_p().a",
            "core.mem.drop(x)",
            "core.mem.drop((1u8, 2u8))",
        ],
    );
    assert!(
        !tdrop_has(&cs, "E3003") && !tdrop_has(&cs, "E2042") && !tdrop_has(&cs, "E2020"),
        "{}",
        out
    );
}

#[test]
fn tdrop_unknown_field_types_stay_silent() {
    // 型の分からない要素を含むタプル・フィールドの型が線形でないジェネリクス構造体リテラルは黙る。
    let (cs, out) = tdrop_codes(
        "tdrop_silent",
        &["let b = t.Box { v: 1u8 }", "core.mem.drop(b.v)", "core.mem.drop((b.v, 1u8))", "core.mem.drop(b)"],
    );
    assert!(!tdrop_has(&cs, "E2042") && !tdrop_has(&cs, "E3003"), "{}", out);
}

// ---- 借用の残り: if の値・ブロック式の値を通じた運搬 ----

#[test]
fn nll_rejects_a_mutation_while_an_if_value_carries_the_borrow() {
    // `let r = if c { &x } else { &y }`: 両枝の和なので、then 枝の借用 (x) も r が持つ。
    let (bad, out) = has_e3010(
        "nll_if_val_then",
        "",
        &[
            "let mut x = 1u8",
            "let y = 2u8",
            "let b = true",
            "let r = if b {",
            "    &x",
            "} else {",
            "    &y",
            "}",
            "x = 3u8",
            "core.check.eq(*r, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_borrowed_by_the_else_branch_of_an_if_value() {
    let (bad, out) = has_e3010(
        "nll_if_val_else",
        "",
        &[
            "let x = 1u8",
            "let mut y = 2u8",
            "let b = false",
            "let r = if b {",
            "    &x",
            "} else {",
            "    &y",
            "}",
            "y = 3u8",
            "core.check.eq(*r, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `y` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_block_value_carries_the_borrow() {
    // `let r = { &x }`: ブロック式の末尾の式の借用が r に移る。
    let (bad, out) = has_e3010(
        "nll_block_val",
        "",
        &["let mut x = 1u8", "let r = {", "    &x", "}", "x = 3u8", "core.check.eq(*r, 1u8)"],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_an_if_value_nested_in_a_match_arm_carries_a_borrow() {
    // 腕の中の `if` の値 (then 枝の `&x`) が match の値を経て r に移る。
    let (bad, out) = has_e3010(
        "nll_if_val_nested",
        "",
        &[
            "let mut x = 1u8",
            "let y = 9u8",
            "let c = true",
            "let r = match c {",
            "    true => if c {",
            "        &x",
            "    } else {",
            "        &y",
            "    }",
            "    false => &y",
            "}",
            "x = 2u8",
            "core.check.eq(*r, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_accepts_an_if_value_that_copies_out_of_the_borrow() {
    // 枝の値が `*r0` (コピー) なら、if の値は借用を運ばない。
    let (bad, out) = has_e3010(
        "nll_if_val_copy",
        "",
        &[
            "let mut x = 1u8",
            "let r0 = &x",
            "let b = true",
            "let n = if b {",
            "    *r0",
            "} else {",
            "    0u8",
            "}",
            "x = 3u8",
            "core.check.eq(n, 1u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_an_if_value_that_is_dead_before_the_mutation() {
    let (bad, out) = has_e3010(
        "nll_if_val_dead",
        "",
        &[
            "let mut x = 1u8",
            "let y = 2u8",
            "let b = true",
            "let r = if b {",
            "    &x",
            "} else {",
            "    &y",
            "}",
            "core.check.eq(*r, 1u8)",
            "x = 3u8",
            "core.check.eq(x, 3u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_an_if_value_inside_a_loop_when_the_borrow_dies_before_the_mutation() {
    // 毎周回の `if` の値は枝の終わりで入れ替わる。前の周回の借用 (x) は次の周回の値 (y) に漏れない。
    let (bad, out) = has_e3010(
        "nll_if_val_loop",
        "",
        &[
            "let mut x = 1u8",
            "let y = 2u8",
            "let mut i = 0u8",
            "let mut acc = 0u8",
            "while i < 3u8 {",
            "    let r = if i == 0u8 {",
            "        &x",
            "    } else {",
            "        &y",
            "    }",
            "    acc = acc + *r",
            "    x = x + 1u8",
            "    i = i + 1u8",
            "}",
            "core.check.eq(acc, 5u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

// `ty_of` の Index・MCall・ArrayE の型解決 (4.4)。`tdrop_prefix` に、メソッドを持つ型を足して使う。
fn tyof_codes(name: &str, b: &[&str]) -> (Vec<String>, String) {
    let nl = char::from(10u8).to_string();
    // 定義の並びは UTF-8 のバイト順 (E0020)。Maker・Other の型は `t.P` の前、
    // そのメソッドは `t.eat` の前に差し込む。
    let maker_types = [
        "type t.Maker",
        "    k: u8",
        "end t.Maker",
        "",
        "type t.Other",
        "    k: u8",
        "end t.Other",
        "",
    ];
    let maker_fns = [
        "fn t.Maker.make_l(self: &t.Maker) -> t.L uses [] panics never",
        "    t.L { a: self.k }",
        "end t.Maker.make_l",
        "",
        "fn t.Maker.make_w(self: &t.Maker) -> t.W uses [] panics never",
        "    t.W { lin: t.L { a: self.k }, n: self.k }",
        "end t.Maker.make_w",
        "",
        "fn t.Maker.touch(self: &t.Maker) -> () uses [] panics never",
        "    ()",
        "end t.Maker.touch",
        "",
        // 同じ名前のメソッドを別の型が持つ (戻り値は ()): 所有者で絞らないと一意に定まらない。
        "fn t.Other.make_l(self: &t.Other) -> () uses [] panics never",
        "    ()",
        "end t.Other.make_l",
        "",
    ];
    let mut pre: Vec<&str> = vec![];
    for l in tdrop_prefix() {
        if l == "type t.P" {
            pre.extend(maker_types.iter().copied());
        }
        if l == "fn t.eat(l: t.L) -> () uses [] panics never" {
            pre.extend(maker_fns.iter().copied());
        }
        pre.push(l);
    }
    // メソッドのレシーバに式を使う (`t.lmaker().m()`)。局所変数 `m.m()` は Call として解析されるため。
    // `t.lmaker` は `t.get_w` の後ろ (定義の並びは UTF-8 のバイト順)。
    pre.extend([
        "fn t.lmaker() -> t.Maker uses [] panics never",
        "    t.Maker { k: 1u8 }",
        "end t.lmaker",
        "",
    ]);
    let prefix = pre.join(&nl) + &nl;
    let t = Tmp::new(name, &main_wrap(&prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

#[test]
fn tyof_discarding_a_method_result_is_e2042() {
    // レシーバの式の型 (t.Maker) からメソッドを一意に定め、戻り値 (t.L) を解決する。
    // t.Other の同名メソッドは戻り値が () なので、所有者で絞らないと黙ってしまう。
    let (cs, out) = tyof_codes("tyof_mcall", &["t.lmaker().make_l()"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tyof_field_of_a_method_result_is_resolved() {
    // MCall → Field の道: `make_w()` の戻り値 (t.W) の `lin` (t.L) を捨てる。
    let (cs, out) = tyof_codes("tyof_mcall_field", &["t.lmaker().make_w().lin"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tyof_method_result_binding_with_unconsumed_linear_is_e3003() {
    // 束縛の型が MCall の戻り値から決まるので、線形なフィールドの消費漏れが見える (E3003)。
    let (cs, out) = tyof_codes(
        "tyof_mcall_bind",
        &["let w = t.lmaker().make_w()", "core.mem.drop(w.n)"],
    );
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `w` is never consumed"), "{}", out);
}

#[test]
fn tyof_method_result_consumed_is_ok() {
    let (cs, out) = tyof_codes(
        "tyof_mcall_ok",
        &[
            "let w = t.lmaker().make_w()",
            "t.eat(w.lin)",
            "core.mem.drop(w.n)",
            "let l = t.lmaker().make_l()",
            "t.eat(l)",
            "t.lmaker().touch()",
        ],
    );
    assert!(!tdrop_has(&cs, "E3003") && !tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tyof_discarding_an_array_literal_is_e2042() {
    let (cs, out) = tyof_codes("tyof_array", &["[1u8, 2u8]"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tyof_discarding_an_index_is_e2042() {
    let (cs, out) = tyof_codes("tyof_index", &["let a = [1u8, 2u8]", "a[0]"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tyof_index_of_an_array_of_structs_then_field_is_resolved() {
    // Index → Field の道: 要素 (t.P) の `a` (u8) を捨てる。
    let (cs, out) = tyof_codes("tyof_index_field", &["let a = [t.P { a: 1u8 }, t.P { a: 2u8 }]", "a[0].a"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tyof_index_through_a_reference_is_resolved() {
    let (cs, out) = tyof_codes("tyof_index_ref", &["let a = [1u8, 2u8]", "let r = &a", "r[1]"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tyof_unit_method_results_and_unknown_types_stay_silent() {
    // 戻り値が () のメソッド、型の決まらない要素 (型なし整数リテラル)・存在しないメソッドは黙る。
    let (cs, out) = tyof_codes(
        "tyof_silent",
        &[
            "t.lmaker().touch()",
            "t.lmaker().nosuch()",
            "let a = [1, 2]",
            "a[0]",
            "[1, 2]",
            "let b = [t.lmaker().touch(), t.lmaker().touch()]",
            "core.mem.drop(b[0])",
        ],
    );
    assert!(!tdrop_has(&cs, "E2042") && !tdrop_has(&cs, "E3003"), "{}", out);
}

#[test]
fn linear_array_consumed_by_index_only_one_element_is_e3003() {
    // `ls[1]` の消費漏れ (4.4)。配列の束縛は名前の使用があれば消費、という近似の穴を要素単位で塞ぐ。
    let (cs, out) = tyof_codes("lin_arr_one", &["let ls = [t.get_l(), t.get_l()]", "t.eat(ls[0])"]);
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `ls[1]` is never consumed"), "{}", out);
}

#[test]
fn linear_array_with_a_gap_in_the_indices_is_e3003() {
    // 添字 0 と 2 は使い、中央の 1 だけ消費しない。
    let (cs, out) = tyof_codes(
        "lin_arr_gap",
        &["let ls = [t.get_l(), t.get_l(), t.get_l()]", "t.eat(ls[0])", "t.eat(ls[2])"],
    );
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `ls[1]` is never consumed"), "{}", out);
}

#[test]
fn linear_array_never_used_is_e3003() {
    let (cs, out) = tyof_codes("lin_arr_unused", &["let ls = [t.get_l(), t.get_l()]"]);
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `ls` is never consumed"), "{}", out);
}

#[test]
fn linear_array_with_every_element_consumed_is_ok() {
    let (cs, out) = tyof_codes(
        "lin_arr_ok",
        &["let ls = [t.get_l(), t.get_l()]", "t.eat(ls[0])", "t.eat(ls[1])"],
    );
    assert!(!tdrop_has(&cs, "E3003"), "{}", out);
}

#[test]
fn linear_array_used_as_a_whole_or_by_a_variable_index_stays_silent() {
    // 配列全体を `for` で回す・添字に変数を使う (どの要素か判断できない) 場合は黙る。
    let (cs, out) = tyof_codes(
        "lin_arr_silent",
        &[
            "let ls = [t.get_l(), t.get_l()]",
            "for x in ls",
            "    t.eat(x)",
            "end for",
            "let us = [t.get_l(), t.get_l()]",
            "let i = 0usize",
            "t.eat(us[i])",
        ],
    );
    assert!(!tdrop_has(&cs, "E3003"), "{}", out);
}

// ---- 借用の残り: 分解 (let (a, b) = t、match t { (a, b) => .. }) での保存ローンの読み戻し ----

#[test]
fn nll_rejects_a_mutation_while_a_let_destructured_binding_carries_a_stored_borrow() {
    // `let t = (&x, 1u8)` に保存したローンは、分解した束縛 `a` に伝わる (要素ごとの道は無いので全体の和)。
    let (bad, out) = has_e3010(
        "nll_destr_let",
        "",
        &[
            "let mut x = 1u8",
            "let t = (&x, 1u8)",
            "let (a, n) = t",
            "x = 3u8",
            "core.check.eq(*a, 1u8)",
            "core.check.eq(n, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `a`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_destructured_literal_binding_carries_the_borrow() {
    // リテラルを直接分解する `let (a, n) = (&x, 1u8)` も、束縛 `a` が借用を持つ。
    let (bad, out) = has_e3010(
        "nll_destr_lit",
        "",
        &[
            "let mut x = 1u8",
            "let (a, n) = (&x, 1u8)",
            "x = 3u8",
            "core.check.eq(*a, 1u8)",
            "core.check.eq(n, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `a`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_match_destructured_binding_carries_the_stored_borrow() {
    // `match t { (a, n) => .. }` の腕の束縛 `a` も、保存された借用を持つ。
    let (bad, out) = has_e3010(
        "nll_destr_match",
        "",
        &[
            "let mut x = 1u8",
            "let t = (&x, 1u8)",
            "match t {",
            "    (a, n) => {",
            "        x = 3u8",
            "        core.check.eq(*a, 1u8)",
            "    }",
            "}",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `a`"), "{}", out);
}

#[test]
fn nll_accepts_a_destructured_binding_that_is_dead_before_the_mutation() {
    // 分解の束縛 `a` を使い終えてから書き換えるなら、借用は死んでいて黙る。
    let (bad, out) = has_e3010(
        "nll_destr_dead",
        "",
        &[
            "let mut x = 1u8",
            "let t = (&x, 1u8)",
            "let (a, n) = t",
            "core.check.eq(*a, 1u8)",
            "x = 3u8",
            "core.check.eq(x, 3u8)",
            "core.check.eq(n, 1u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_destructured_binding_that_only_copies_the_value_out() {
    // 束縛から値をコピー (`*a`) して取り出し、その後は束縛を使わなければ、書き換えは許される。
    let (bad, out) = has_e3010(
        "nll_destr_copy",
        "",
        &[
            "let mut x = 1u8",
            "let t = (&x, 1u8)",
            "let (a, n) = t",
            "let v = *a",
            "x = 3u8",
            "core.check.eq(v, 1u8)",
            "core.check.eq(n, 1u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_destructured_tuple_without_any_borrow() {
    // 借用を含まないタプル (`(1u8, 2u8)`) の分解は、無関係な変数の書き換えで黙る。
    let (bad, out) = has_e3010(
        "nll_destr_nolo",
        "",
        &[
            "let mut x = 1u8",
            "let t = (1u8, 2u8)",
            "let (a, b) = t",
            "x = 3u8",
            "core.check.eq(a + b, 3u8)",
            "core.check.eq(x, 3u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_destructured_binding_after_the_tuple_was_overwritten() {
    // `t` を書き換えると、前に保存された `&x` のローンは死ぬ。新しい `&y` だけが分解の束縛に移る。
    let (bad, out) = has_e3010(
        "nll_destr_overwrite",
        "",
        &[
            "let mut x = 1u8",
            "let mut y = 2u8",
            "let mut t = (&x, 1u8)",
            "t = (&y, 2u8)",
            "let (a, n) = t",
            "x = 3u8",
            "core.check.eq(*a, 2u8)",
            "core.check.eq(n, 2u8)",
            "y = 4u8",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_struct_destructured_field_carries_a_stored_borrow() {
    // 構造体のフィールドに保存された借用は、名前の道をたどって分解の束縛 `r` に伝わる。
    let (bad, out) = has_e3010(
        "nll_destr_struct",
        NLL_REF_FIELD,
        &[
            "let mut x = 1u8",
            "let s = t.S { f: &x, b: 2u8 }",
            "let t.S { f: r, b: n } = s",
            "x = 2u8",
            "core.check.eq(*r, 1u8)",
            "core.check.eq(n, 2u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_accepts_a_copy_element_of_a_literal_destructured_while_the_borrow_is_mutated() {
    // `(&x, 1u8)` の分解で、借用を持つ要素 `a` は死んでおり、コピー値の `n` だけを後で使うなら黙る。
    let (bad, out) = has_e3010(
        "nll_destr_lit_copy",
        "",
        &[
            "let mut x = 1u8",
            "let (a, n) = (&x, 1u8)",
            "core.check.eq(*a, 1u8)",
            "x = 3u8",
            "core.check.eq(n, 1u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_destructured_call_result_when_the_source_is_mutated_before_the_copy_is_used() {
    // 道が分からない分解 (関数の戻り値 `t.pick(&x)`) は、右辺全体の借用を束縛に配らない (従来どおり黙る)。
    // `n` はコピー値なので、`x` を書き換えたあとに使っても E3010 にしない。
    let prefix = "fn t.apick(r: &u8) -> (&u8, u8) uses [] panics never\n    (r, 1u8)\nend t.apick\n\n";
    let (bad, out) = has_e3010(
        "nll_destr_call_neg",
        prefix,
        &[
            "let mut x = 1u8",
            "let (_, n) = t.apick(&x)",
            "x = 3u8",
            "core.check.eq(n, 1u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

// ---- 分解の誤検出 (コピー値の束縛に借用を付けない): 道が分かる分解と、関数の戻り値の分解 ----

/// 診断が 1 つも出ないことを確かめる (陰性テスト用)。
fn no_diagnostics(name: &str, prefix: &str, b: &[&str]) -> String {
    let t = Tmp::new(name, &main_wrap(prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).is_empty(), "{}", out);
    out
}

#[test]
fn nll_accepts_a_copy_element_of_a_destructured_call_result_held_in_a_variable() {
    // `let p = t.apick(&x)` の p は x の借用を持つが、分解した `n` はコピー値なので黙る。
    let prefix = "fn t.apick(r: &u8) -> (&u8, u8) uses [] panics never\n    (r, 1u8)\nend t.apick\n\n";
    no_diagnostics(
        "nll_destr_call_var_neg",
        prefix,
        &[
            "let mut x = 1u8",
            "let p = t.apick(&x)",
            "let (a, n) = p",
            "core.check.eq(*a, 1u8)",
            "x = 3u8",
            "core.check.eq(n, 1u8)",
        ],
    );
}

#[test]
fn nll_accepts_a_copy_element_of_a_moved_call_result_destructured_later() {
    // `let t2 = t0` で move してから分解しても、コピー値 `n` には借用を付けない。
    let prefix = "fn t.apick(r: &u8) -> (&u8, u8) uses [] panics never\n    (r, 1u8)\nend t.apick\n\n";
    no_diagnostics(
        "nll_destr_call_move_neg",
        prefix,
        &[
            "let mut x = 1u8",
            "let t0 = t.apick(&x)",
            "let t2 = t0",
            "let (a, n) = t2",
            "core.check.eq(*a, 1u8)",
            "x = 3u8",
            "core.check.eq(n, 1u8)",
        ],
    );
}

#[test]
fn nll_accepts_a_copy_field_of_a_moved_struct_destructured_later() {
    // 構造体 `let s2 = s` で move してから分解しても、コピーのフィールド `b` (n) には借用を付けない。
    // 借用を持つフィールド `f` (r) は、書き換えより前に使い終えている。
    no_diagnostics(
        "nll_destr_struct_move_neg",
        "",
        &[
            "let mut x = 1u8",
            "let s = (&x, 2u8)",
            "let s2 = s",
            "let (r, n) = s2",
            "core.check.eq(*r, 1u8)",
            "x = 2u8",
            "core.check.eq(n, 2u8)",
        ],
    );
}

#[test]
fn nll_accepts_a_copy_element_of_a_match_on_a_call_result_while_the_source_is_mutated() {
    // 形が分からない scrutinee (関数呼び出し) の腕の束縛 `n` は、右辺の借用を受け取らない。
    let prefix = "fn t.apick(r: &u8) -> (&u8, u8) uses [] panics never\n    (r, 1u8)\nend t.apick\n\n";
    no_diagnostics(
        "nll_match_call_neg",
        prefix,
        &[
            "let mut x = 1u8",
            "match t.apick(&x) {",
            "    (a, n) => {",
            "        core.check.eq(*a, 1u8)",
            "        x = 3u8",
            "        core.check.eq(n, 1u8)",
            "    }",
            "}",
        ],
    );
}

#[test]
fn move_capture_that_is_rewritten_does_not_meet_the_fn_bound() {
    // `move` で捕まえた値を書き換えるクロージャは、読むだけの `Fn` を満たさない (FnMut まで)。
    let (got, out) = check_codes(
        "cl_move_mut",
        &[
            "let mut n = 0u32",
            "let f = fn(acc: u32, x: u32) -> u32 capture [move n] {",
            "    n = acc + x",
            "    n",
            "}",
            "let total = core.iter.fold(core.iter.range(0u32, 4u32), 0u32, f)",
            "core.mem.drop(total)",
        ],
    );
    assert!(got.iter().any(|c| c == "E2030"), "{}", out);
}

#[test]
fn move_capture_that_is_consumed_does_not_meet_the_fn_bound() {
    // `move` で捕まえた Copy でない値を値のまま消費するクロージャは FnOnce で、Fn / FnMut を満たさない。
    let (got, out) = check_codes(
        "cl_move_once",
        &[
            "let v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let f = fn(acc: u32, x: u32) -> u32 capture [move v] {",
            "    core.mem.drop(v)",
            "    acc + x",
            "}",
            "let total = core.iter.fold(core.iter.range(0u32, 4u32), 0u32, f)",
            "core.mem.drop(total)",
        ],
    );
    assert!(got.iter().any(|c| c == "E2030"), "{}", out);
}

#[test]
fn move_capture_that_is_only_read_meets_the_fn_bound() {
    // `move` で捕まえても本体が読むだけ (Copy の値、メソッドのレシーバ) なら Fn を満たす (Rust と同じ)。
    let (got, out) = check_codes(
        "cl_move_read",
        &[
            "let k = 7u32",
            "let s = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let f = fn(acc: u32, x: u32) -> u32 capture [move k, move s] {",
            "    core.mem.drop(s.len())",
            "    acc + x * k",
            "}",
            "let total = core.iter.fold(core.iter.range(0u32, 4u32), 0u32, f)",
            "core.mem.drop(total)",
        ],
    );
    assert!(!got.iter().any(|c| c == "E2030"), "{}", out);
}

#[test]
fn lin_option_of_linear_left_unconsumed_is_e3003() {
    let (cs, out) = lin_arg_codes("lin_opt_unused", &["let o = Some(t.get_l())"]);
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `o` is never consumed"), "{}", out);
}

#[test]
fn lin_result_with_a_linear_ok_left_unconsumed_is_e3003() {
    let (cs, out) = lin_arg_codes("lin_res_unused", &["let r: Result<t.L, u8> = Ok(t.get_l())"]);
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `r` is never consumed"), "{}", out);
}

#[test]
fn lin_generic_literal_with_a_linear_arg_left_unconsumed_is_e3003() {
    // 型引数の推論: `t.Box { v: t.get_l() }` は `t.Box<t.L>` (線形)。
    let (cs, out) = lin_arg_codes("lin_box_unused", &["let _bl = t.Box { v: t.get_l() }"]);
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `_bl` is never consumed"), "{}", out);
}

#[test]
fn lin_generic_struct_with_an_unconsumed_linear_field_is_e3003() {
    let (cs, out) = lin_arg_codes(
        "lin_pair_unused",
        &["let p = t.Pair { a: t.get_l(), b: 1u8 }", "core.mem.drop(p.b)"],
    );
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
}

#[test]
fn lin_option_consumed_by_match_passed_or_moved_is_ok() {
    let (cs, out) = lin_arg_codes(
        "lin_opt_ok",
        &[
            "let o = Some(t.get_l())",
            "match o {",
            "    Some(x) => {",
            "        t.eat(x)",
            "    }",
            "    None => {",
            "        core.mem.drop(0u8)",
            "    }",
            "}",
            "let p = Some(t.get_l())",
            "t.eat_opt(p)",
            "let q = Some(t.get_l())",
            "let q2 = q",
            "t.eat_opt(q2)",
            "let m = t.mk_opt()",
            "t.eat_opt(m)",
        ],
    );
    assert!(!tdrop_has(&cs, "E3003") && !tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn lin_result_try_and_generic_literal_consumed_are_ok() {
    let (cs, out) = lin_arg_codes(
        "lin_res_box_ok",
        &[
            "let b = t.Box { v: t.get_l() }",
            "t.eat_box(b)",
            "let p = t.Pair { a: t.get_l(), b: 1u8 }",
            "t.eat(p.a)",
            "core.mem.drop(p.b)",
            "let n: Option<u8> = Some(1u8)",
            "core.mem.drop(n)",
            "let r: Result<u8, t.L> = Err(t.get_l())",
            "match r {",
            "    Ok(v) => {",
            "        core.mem.drop(v)",
            "    }",
            "    Err(e) => {",
            "        t.eat(e)",
            "    }",
            "}",
        ],
    );
    assert!(!tdrop_has(&cs, "E3003") && !tdrop_has(&cs, "E2042"), "{}", out);
}

// ---- 4.6 (型表): 標準ライブラリのジェネリック型のメソッドの戻り値を捨てる式文 (E2042) ----

/// `Vec<u8>` の値 `v` を作る本体 (`alloc` の効果を使うので main は `uses [alloc, io.console]`)。
fn tdisc_src(extra: &str, b: &[&str]) -> String {
    format!(
        "{}fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    let mut v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n{}    Ok(())\nend t.main\n",
        extra,
        body(b)
    )
}

/// E2042 の診断が出た行 (1 診断 1 行の JSON の "line")。
fn e2042_lines(out: &str) -> Vec<usize> {
    out.lines()
        .filter(|l| l.starts_with('{') && l.contains("\"E2042\""))
        .filter_map(|l| {
            let i = l.find("\"line\": ")? + 8;
            let rest = &l[i..];
            rest[..rest.find(|c: char| !c.is_ascii_digit())?].parse().ok()
        })
        .collect()
}

fn tdisc_lines(name: &str, extra: &str, b: &[&str]) -> (Vec<usize>, String) {
    let t = Tmp::new(name, &tdisc_src(extra, b));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (e2042_lines(&out), out)
}

#[test]
fn tdisc_discarding_option_from_generic_method_is_e2042() {
    // `v.pop()` の戻り値は `Option<u8>` (型引数 T を Vec の型引数から代入して決まる)。`()` ではない。
    // 行 3 が `v.pop()` (行 1 は fn、行 2 は let)。
    let (lines, out) = tdisc_lines("tdisc_pop", "", &["v.pop()", "v.clear()"]);
    assert_eq!(lines, vec![3], "{}", out);
}

#[test]
fn tdisc_discarding_scalar_from_generic_method_is_e2042() {
    // `len` は usize を返す (型引数に依らない戻り値も、型表で決まる)。
    let (lines, out) = tdisc_lines("tdisc_len", "", &["v.len()"]);
    assert_eq!(lines, vec![3], "{}", out);
}

#[test]
fn tdisc_unit_returning_generic_methods_are_silent() {
    // `clear` / `truncate` は `()` を返すので、捨てても診断は出ない (陰性)。
    let (lines, out) = tdisc_lines("tdisc_unit", "", &["v.clear()", "v.truncate(0usize)"]);
    assert!(lines.is_empty(), "{}", out);
}

#[test]
fn tdisc_discarding_result_from_push_is_e2042() {
    // `push` は `Result<(), AllocError>` を返すので、`()` 以外を捨てることになる (4.6)。
    let (lines, out) = tdisc_lines("tdisc_push", "", &["v.push(1u8)"]);
    assert_eq!(lines, vec![3], "{}", out);
}

#[test]
fn tdisc_last_statement_of_a_block_is_not_discarded() {
    // ブロックの最後の式文はブロックの値なので捨てない (関数の戻り値は別の検査)。
    let extra = "fn t.last(v: &mut alloc.vec.Vec<u8, std.alloc.System>) -> Option<u8> uses [] panics never\n    v.pop()\nend t.last\n";
    let (lines, out) = tdisc_lines("tdisc_last", extra, &["let o = t.last(&mut v)", "core.mem.drop(o)"]);
    assert!(lines.is_empty(), "{}", out);
}

#[test]
fn tdisc_generic_element_type_stays_silent() {
    // 要素の型 T が決まらない (型パラメータ) ときは黙る。
    let extra = "fn t.drop_top[T](v: &mut alloc.vec.Vec<T, std.alloc.System>) -> () uses [] panics never\n    v.pop()\n    ()\nend t.drop_top\n";
    let (lines, out) = tdisc_lines("tdisc_generic", extra, &["t.drop_top(&mut v)"]);
    assert!(lines.is_empty(), "{}", out);
}

/// 省略した入力 (`&self`) の欄 (`&'static str`) を、`'static` の戻り値へ複写するのは正しい。欄の領域は引数の借用ではない
/// (修正プラン 1-2 の省略規則の検査で、欄の読み戻しを引数の領域と取り違えない)。借用検査 (AST) は通す。
#[test]
fn elided_self_static_field_copy_is_accepted() {
    let src = "type t.Q\n    src: &'static str\nend t.Q\n\nfn t.Q.rest['b](self: &t.Q, _y: &'b u32) -> &'static str uses [] panics never\n    self.src\nend t.Q.rest\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = Tmp::new("elided_self_static_field", src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    assert!(codes(&text(&o.stdout)).is_empty(), "{}", text(&o.stdout));
}
