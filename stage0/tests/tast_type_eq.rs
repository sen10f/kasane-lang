//! TAST の上の規則 T (型の一致、`src/tast_rules/type_eq.rs`) が、旧い検査と同じ (コード, 行) を出すこと (S2C)。
//! `KSN_CHECKER=new` は新しい規則の診断だけを返す。対象は `data/unsound/` のうち、型の食い違いを旧い検査の
//! E2020 / E2023 / E2024 で拒むケース (台帳 `tests/shadow_baseline.json` から外れたもの)。あわせて、言い換え
//! (ブロック・`match` の腕・閉包・総称の関数・`Option` に包む) でも拒むことと、正しい変換 (`&mut` → `&`・`&[T; N]` → `&[T]`・
//! `!` の腕・`derive [From]` の `?`) を拒まないことを見る。

mod common;
use common::*;
use std::collections::BTreeSet;
use std::fs;

const STEMS: &[&str] = &[
    "and_operand_int",
    "arr_elem_mix_ints",
    "arr_elem_mix_str_int",
    "array_to_vec_arg",
    "array_to_vec_field",
    "array_to_vec_let",
    "array_to_vec_return",
    "asmut_temp_ref_return",
    "asref_temp_ref_return",
    "binary_arith_bool",
    "binary_bitand_float",
    "binary_rem_float",
    "box_dyn_without_into_dyn",
    "capture_mut_ref_assigned_as_value",
    "closure_call_uses_undeclared",
    "closure_returns_capture",
    "cond_if_else_string",
    "cond_if_int",
    "cond_while_int",
    "deref_nonref_let",
    "deref_nonref_match_bind",
    "deref_nonref_param",
    "deref_value_alias_chain",
    "deref_value_closure_param",
    "deref_value_field",
    "deref_value_generic_result",
    "deref_value_match_by_value",
    "enum_payload_int_width",
    "gen_ref_dyn_unbounded",
    "hunt_a14_enum_variant_payload_02",
    "hunt_a14_if_else_arm_type_mismatch_01",
    "hunt_a14_if_else_let_arm_mismatch_01",
    "hunt_a14_if_let_arm_mismatch_01",
    "hunt_a14_if_no_else_let_01",
    "hunt_a14_if_no_else_let_infer_01",
    "hunt_a14_if_no_else_value_02",
    "hunt_a14_match_arm_type_mismatch_02",
    "hunt_a14_match_bool_arm_mismatch_01",
    "hunt_a14_match_option_none_unit_01",
    "hunt_a14_match_result_arm_mismatch_01",
    "hunt_a14_match_unit_vs_value_01",
    "hunt_a14_nested_match_01",
    "hunt_a17_reserved_false_bind",
    "if_without_else_value",
    "lit_pat_iflet_wrong_suffix",
    "lit_pat_match_usize_u64",
    "lit_pat_match_wrong_suffix",
    "match_arm_mismatch_around_panic",
    "move_self_field_through_ref",
    "ok_string_ref_nested",
    "ok_unit_result_ref_arg",
    "option_str_as_string_arg",
    "pat_char_on_int",
    "pat_int_on_str",
    "pat_str_on_int",
    "pat_struct_wrong_name_iflet",
    "pat_struct_wrong_name_match",
    "pat_tuple_arity_iflet",
    "pat_tuple_arity_let",
    "pat_tuple_arity_short",
    "pat_variant_arity",
    "pat_variant_bare_with_payload",
    "pat_variant_is_cross_enum",
    "pat_variant_on_option",
    "pat_variant_wrong_enum_result",
    "pat_variant_wrong_enum_user",
    "ref_destructure_name_double_deref",
    "ref_destructure_shared_name_mut_borrowed",
    "ref_levels_option_eq",
    "ref_levels_option_ret",
    "ref_levels_result_arg",
    "ref_levels_tuple_elem",
    "ref_shared_as_mut_arg",
    "ref_shared_as_mut_generic",
    "ref_shared_as_mut_if_arm",
    "ref_shared_as_mut_let",
    "ref_shared_as_mut_some",
    "result_asref_temp_ref_return",
    "return_local_wrapped_v1",
    "return_local_wrapped_v2",
    "return_local_wrapped_v4",
    "return_no_value",
    "return_paren_reborrow_struct",
    "return_value_type_in_closure",
    "return_value_type_in_if",
    "return_value_type_in_loop_match",
    "return_wrong_int",
    "slice_to_vec_arg",
    "some_unit_option_ref_arg",
    "str_as_string_arg",
    "str_as_string_field",
    "str_as_string_let",
    "str_tuple_as_string_arg",
    "str_var_as_string_arg",
    "targs_expr_effect_counts",
    "targs_expr_mismatch_arg",
    "targs_expr_too_few",
    "targs_expr_too_many",
    "try_error_enum_without_derive",
    "unary_neg_bool",
    "unary_not_float",
    "undef_let_type",
    "undef_turbofish_type",
    "unify_arg_struct_option",
    "unify_array_for_prim",
    "unify_array_len",
    "unify_assign_array_len",
    "unify_assign_ref_mut",
    "unify_assign_rigid_param",
    "unify_assign_tuple_arity",
    "unify_closure_for_prim",
    "unify_if_branch_mut_order",
    "unify_mut_in_fn_param",
    "unify_mut_under_mut_ref",
    "unify_option_elem",
    "unify_param_arg",
    "unify_param_cast",
    "unify_prim_for_array",
    "unify_ref_as_rigid_param",
    "unify_slice_as_array_ref",
    "unify_struct_for_array",
    "unify_tuple_arity",
    "unify_vec_elem_arg",
    "unify_vec_from_prim",
    "unit_where_result_expected",
    "vec_to_array_arg",
];

fn diags(json: &str) -> BTreeSet<(String, u64)> {
    let mut out = BTreeSet::new();
    for l in json.lines() {
        let code = l.split("\"code\": \"").nth(1).and_then(|s| s.split('"').next()).unwrap_or("");
        if !matches!(code, "E2020" | "E2023" | "E2024") {
            continue;
        }
        let line = l.split("\"line\": ").nth(1).and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next()).and_then(|s| s.parse().ok()).unwrap_or(0);
        out.insert((code.to_string(), line));
    }
    out
}

fn new_diags(name: &str, src: &str) -> BTreeSet<(String, u64)> {
    let t = Tmp::new(name, src);
    diags(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "new")]).stdout))
}

#[test]
fn type_eq_rule_matches_old_checker_on_unsound() {
    let mut bad = vec![];
    for stem in STEMS {
        let src = fs::read_to_string(root().join("stage0/tests/data/unsound").join(format!("{}.ksnl", stem))).unwrap();
        let t = Tmp::new(&format!("tast_te_{}", stem), &src);
        let old = diags(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "old")]).stdout));
        let new = diags(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "new")]).stdout));
        if new.is_empty() || !new.is_subset(&old) {
            bad.push(format!("{}: old={:?} new={:?}", stem, old, new));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

const MAIN: &str = "\nfn t.main() -> () uses [] panics never\n    core.mem.drop(t.f())\nend t.main\n";

#[test]
fn type_eq_rule_rejects_rephrasings() {
    let cases: &[(&str, &str, &str)] = &[
        ("let_block", "E2020", "fn t.f() -> u32 uses [] panics never\n    let a: u32 = {\n        let b = 1u8\n        b\n    }\n    a\nend t.f"),
        ("closure_ret", "E2020", "fn t.f() -> u32 uses [] panics never\n    let g = fn(x: u8) -> u32 capture [] {\n        x\n    }\n    g(1u8)\nend t.f"),
        ("option_wrapped", "E2020", "fn t.f() -> u32 uses [] panics never\n    let o: Option<u32> = Some(1u8)\n    match o {\n        Some(v) => v\n        None => 0u32\n    }\nend t.f"),
        ("generic_arg", "E2023", "fn t.id[T](x: T) -> T uses [] panics never\n    x\nend t.id\n\nfn t.f() -> u32 uses [] panics never\n    t.id::<u32>(1i32)\nend t.f"),
        ("arm_in_if", "E2020", "fn t.f() -> u32 uses [] panics never\n    if true {\n        match true {\n            true => 1u32\n            false => 2u8\n        }\n    } else {\n        3u32\n    }\nend t.f"),
        ("assign_tuple", "E2024", "fn t.f() -> u32 uses [] panics never\n    let mut p = (1u64, 2u64)\n    p = (1u64, 2u64, 3u64)\n    1u32\nend t.f"),
        ("tuple_pat_arity", "E2023", "fn t.f() -> u32 uses [] panics never\n    let p = (1u32, 2u32)\n    match p {\n        (a, b, c) => a\n    }\nend t.f"),
    ];
    let mut bad = vec![];
    for (name, code, body) in cases {
        let got = new_diags(&format!("tast_te_re_{}", name), &format!("{}{}", body, MAIN));
        if !got.iter().any(|(c, _)| c == code) {
            bad.push(format!("{}: expected {} got {:?}", name, code, got));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn type_eq_rule_accepts_legal_conversions() {
    let src = "fn t.sum(xs: &[u8]) -> u8 uses [] panics never\n    xs[0]\nend t.sum\n\n\
fn t.rd(x: &u8) -> u8 uses [] panics never\n    *x\nend t.rd\n\n\
fn t.f() -> u8 uses [] panics never\n    let a = [1u8, 2u8, 3u8]\n    let mut v = 5u8\n    let m: &mut u8 = &mut v\n    let r: &u8 = m\n    let x = if true {\n        t.rd(r)\n    } else {\n        return 0u8\n    }\n    t.sum(&a) + x\nend t.f\n";
    let got = new_diags("tast_te_ok", &format!("{}{}", src, MAIN));
    assert!(got.is_empty(), "{:?}", got);
}
