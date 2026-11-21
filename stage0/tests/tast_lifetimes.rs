//! TAST の上の規則 L (借用の寿命と出どころ、`src/tast_rules/lifetimes.rs`、S3D): 検査器が、`data/unsound/` の (コード, 行) の E3030 / E2037 / E3010 (戻り値の位置) を出すこと、
//! 正しいコード (`data/linear_ok/`・`data/check_ok/`) に出さないこと、言い換えでも同じ診断が出ること。

mod common;
use common::*;
use std::collections::BTreeSet;
use std::fs;

/// (`data/unsound/` の名前, コード, 行): 規則 L が出す。
const CASES: &[(&str, &str, u64)] = &[
    ("asmut_temp_ref_return", "E3030", 2),
    ("asref_temp_ref_return", "E3030", 2),
    ("block_string_borrow_escape", "E3030", 5),
    ("block_string_borrow_escape", "E3010", 7),
    ("borrow_mutu32_fst_b_o_put", "E3030", 16),
    ("borrow_mutu32_fst_b_o_put", "E3010", 18),
    ("borrow_mutu32_fst_b_o_setter", "E3030", 16),
    ("borrow_mutu32_fst_b_o_setter", "E3010", 18),
    ("borrow_mutu32_fst_b_o_tuple", "E3030", 14),
    ("borrow_mutu32_fst_b_o_tuple", "E3010", 16),
    ("borrow_mutu32_fst_b_o_tuple", "E3010", 17),
    ("borrow_mutu32_id_put", "E3030", 14),
    ("borrow_mutu32_id_put", "E3010", 16),
    ("borrow_mutu32_id_setter", "E3030", 14),
    ("borrow_mutu32_id_setter", "E3010", 16),
    ("borrow_mutu32_id_tuple", "E3030", 12),
    ("borrow_mutu32_id_tuple", "E3010", 14),
    ("borrow_mutu32_id_tuple", "E3010", 15),
    ("borrow_mutu32_ifelse_put", "E3030", 20),
    ("borrow_mutu32_ifelse_put", "E3010", 22),
    ("borrow_mutu32_ifelse_setter", "E3030", 20),
    ("borrow_mutu32_ifelse_setter", "E3010", 22),
    ("borrow_mutu32_ifelse_tuple", "E3030", 18),
    ("borrow_mutu32_ifelse_tuple", "E3010", 20),
    ("borrow_mutu32_ifelse_tuple", "E3010", 21),
    ("borrow_mutu32_let_put", "E3030", 15),
    ("borrow_mutu32_let_put", "E3010", 17),
    ("borrow_mutu32_let_setter", "E3030", 15),
    ("borrow_mutu32_let_setter", "E3010", 17),
    ("borrow_mutu32_let_tuple", "E3030", 13),
    ("borrow_mutu32_let_tuple", "E3010", 15),
    ("borrow_mutu32_let_tuple", "E3010", 16),
    ("borrow_mutu32_plain_put", "E3030", 14),
    ("borrow_mutu32_plain_put", "E3010", 16),
    ("borrow_mutu32_plain_setter", "E3030", 14),
    ("borrow_mutu32_plain_setter", "E3010", 16),
    ("borrow_mutu32_plain_tuple", "E3030", 12),
    ("borrow_mutu32_plain_tuple", "E3010", 14),
    ("borrow_mutu32_plain_tuple", "E3010", 15),
    ("borrow_mutu32_reborrow_put", "E3030", 14),
    ("borrow_mutu32_reborrow_put", "E3010", 16),
    ("borrow_mutu32_reborrow_setter", "E3030", 14),
    ("borrow_mutu32_reborrow_setter", "E3010", 16),
    ("borrow_mutu32_reborrow_tuple", "E3030", 12),
    ("borrow_mutu32_reborrow_tuple", "E3010", 14),
    ("borrow_mutu32_reborrow_tuple", "E3010", 15),
    ("borrow_mutu32_snd_o_b_put", "E3030", 16),
    ("borrow_mutu32_snd_o_b_put", "E3010", 18),
    ("borrow_mutu32_snd_o_b_setter", "E3030", 16),
    ("borrow_mutu32_snd_o_b_setter", "E3010", 18),
    ("borrow_mutu32_snd_o_b_tuple", "E3030", 14),
    ("borrow_mutu32_snd_o_b_tuple", "E3010", 16),
    ("borrow_mutu32_snd_o_b_tuple", "E3010", 17),
    ("break_ref_escape_for", "E3010", 9),
    ("break_ref_escape_if", "E3010", 10),
    ("break_ref_escape_loop", "E3010", 8),
    ("break_ref_escape_while", "E3010", 8),
    ("d04_block_borrow_escape", "E3010", 7),
    ("d05_block_value_borrow_escape", "E3030", 4),
    ("d05_block_value_borrow_escape", "E3010", 6),
    ("dangling_static_field", "E3030", 7),
    ("dangling_via_let_struct", "E3030", 7),
    ("dangling_via_let_struct", "E3030", 9),
    ("dangling_via_match", "E3030", 5),
    ("dangling_via_match", "E3030", 6),
    ("elided_flow_return_closure_call", "E3010", 6),
    ("elided_flow_return_destructure", "E3010", 8),
    ("elided_flow_return_match_get", "E3010", 3),
    ("elided_flow_return_option_param", "E3010", 7),
    ("elided_flow_return_static_if_let", "E3010", 3),
    ("elided_self_return_a107_self_and_mut_input_mut_01", "E3010", 6),
    ("elided_self_return_a107_self_mut_input_mut_01", "E3010", 6),
    ("elided_self_return_a107_self_mut_while_01", "E3010", 8),
    ("elided_self_return_dyn_method_01", "E3010", 11),
    ("elided_self_return_gen_method", "E3010", 6),
    ("elided_self_return_if_expr", "E3010", 7),
    ("elided_self_return_let_alias", "E3010", 7),
    ("elided_self_return_match_open", "E3010", 6),
    ("elided_self_return_mut_self_plus_elided_return_other", "E3010", 6),
    ("elided_self_return_self_early_return_other", "E3010", 6),
    ("elided_self_return_self_option_return_other", "E3010", 6),
    ("elided_self_return_self_reborrow_chain_return_other", "E3010", 9),
    ("elided_self_return_tail", "E3010", 6),
    ("elided_self_return_trait_method_impl_01", "E3010", 11),
    ("escape_ctor_as_str_block_value", "E3030", 9),
    ("escape_ctor_as_str_block_value", "E3010", 11),
    ("escape_ctor_via_fn_block_value", "E3030", 8),
    ("escape_ctor_via_fn_block_value", "E3010", 10),
    ("escape_field_assign_reborrow", "E3010", 12),
    ("escape_put_as_str", "E3030", 6),
    ("escape_put_as_str", "E3010", 8),
    ("escape_return_static_via_fn", "E3030", 3),
    ("escape_return_struct_reborrow", "E3030", 7),
    ("escape_tuple_block_value", "E3030", 4),
    ("escape_tuple_block_value", "E3010", 6),
    ("escape_tuple_block_value", "E3010", 7),
    ("escape_vec_push_mut_reborrow", "E3010", 8),
    ("flow_append_moves_loans", "E3010", 9),
    ("flow_append_moves_loans", "E3010", 10),
    ("flow_deref_assign_inner_borrow", "E3010", 9),
    ("flow_deref_push_inner_borrow", "E3010", 8),
    ("flow_deref_push_inner_borrow", "E3010", 9),
    ("flow_guard_temp_return", "E3030", 2),
    ("flow_match_arm_binding_outlives", "E3010", 12),
    ("flow_name_reuse_dropscope", "E3010", 11),
    ("flow_store_through_mut_alias", "E3010", 12),
    ("flow_store_through_ref_push", "E3010", 9),
    ("flow_store_through_ref_push", "E3010", 10),
    ("fqn_same_name_setter", "E3030", 22),
    ("fqn_same_name_setter", "E3010", 24),
    ("h04_sibling_block_same_name_dangling", "E3010", 14),
    ("header_elided_field_read_named", "E3010", 6),
    ("header_elided_self_borrow_named", "E3010", 6),
    ("header_lifetime_elided_input_as_named", "E3010", 12),
    ("header_lifetime_method_setter", "E3030", 14),
    ("header_lifetime_method_setter", "E3010", 16),
    ("header_lifetime_return_other", "E3010", 14),
    ("header_lifetime_return_static", "E3010", 2),
    ("header_wrapped_return_option", "E3010", 2),
    ("header_wrapped_return_struct", "E3010", 6),
    ("hold_generic_escape", "E3010", 26),
    ("hold_generic_escape", "E3010", 27),
    ("hunt_a11_spawn_guard_03", "E2037", 4),
    ("hunt_a26_03_method_elided_named", "E3010", 6),
    ("hunt_a26_04_elided_as_static_struct", "E3010", 6),
    ("hunt_a26_05_option_elided_as_b", "E3010", 2),
    ("hunt_a26_06_elided_as_static_direct", "E3010", 2),
    ("hunt_a26_07_tuple_elided_as_b", "E3010", 2),
    ("hunt_a26_09_result_elided_as_static", "E3010", 2),
    ("hunt_a26_10_two_elided_return_a", "E3010", 2),
    ("hunt_a26_11_method_wrap_elided", "E3010", 6),
    ("hunt_a26_16_elided_as_declared_other", "E3010", 2),
    ("hunt_a26_17_nested_struct_elided", "E3010", 10),
    ("hunt_a26_18_triple_elided", "E3010", 2),
    ("hunt_a26_19_result_elided_as_a", "E3010", 2),
    ("hunt_a26_20_struct_elided_as_a", "E3010", 6),
    ("hunt_a26_22_option_elided_as_static", "E3010", 2),
    ("hunt_a26_23_method_option_elided", "E3010", 6),
    ("hunt_a26_24_tuple_option_elided", "E3010", 2),
    ("hunt_a26_25_pair_elided", "E3010", 7),
    ("hunt_a26_26_tuple_static_elided", "E3010", 2),
    ("hunt_a26_27_method_opt_elided_declared_a", "E3010", 6),
    ("hunt_a26_28_mut_ref_elided", "E3010", 2),
    ("hunt_a26_29_boxlike_elided", "E3010", 6),
    ("hunt_a26_30_result_tuple_elided", "E3010", 2),
    ("hunt_a29_if_let_named", "E3010", 2),
    ("hunt_a29_if_let_static", "E3010", 2),
    ("hunt_a29_match_arm_named", "E3010", 2),
    ("hunt_a29_match_arm_static", "E3010", 2),
    ("match_arm_chain_owned_as_str_escape", "E3030", 3),
    ("match_arm_chain_owned_as_str_escape", "E3010", 6),
    ("match_arm_iflet_owned_as_str_escape", "E3030", 3),
    ("match_arm_iflet_owned_as_str_escape", "E3010", 7),
    ("match_arm_let_owned_as_str_escape", "E3030", 6),
    ("match_arm_let_owned_as_str_escape", "E3010", 10),
    ("match_arm_option_owned_as_str_escape", "E3030", 7),
    ("match_arm_option_owned_as_str_escape", "E3010", 10),
    ("match_arm_owned_as_str_escape", "E3030", 3),
    ("match_arm_owned_as_str_escape", "E3010", 6),
    ("rebind_local_ref", "E3030", 7),
    ("rebind_sibling_scope", "E3030", 8),
    ("result_asref_temp_ref_return", "E3030", 2),
    ("return_bound_owner_c1", "E3030", 4),
    ("return_bound_owner_c2", "E3030", 3),
    ("return_bound_owner_c3", "E3030", 5),
    ("return_bound_owner_c6", "E3030", 4),
    ("return_guard_let_borrow", "E3030", 3),
    ("return_local_match_bind_dangling_via_let_struct_var", "E3030", 8),
    ("return_local_match_bind_dangling_via_match_var", "E3030", 5),
    ("return_local_match_bind_dangling_via_match_var", "E3030", 6),
    ("return_local_match_bind_escape_return_static_via_fn_var", "E3030", 4),
    ("return_local_wrapped_v1", "E3030", 3),
    ("return_local_wrapped_v2", "E3030", 3),
    ("return_local_wrapped_v3", "E3030", 3),
    ("return_local_wrapped_v4", "E3030", 4),
    ("spawn_closure_via_variable", "E2037", 10),
    ("spawn_local_capture_local_ref", "E2037", 7),
    ("spawn_move_type_param_capture", "E2037", 15),
    ("spawn_static_bound_wrapper", "E2037", 11),
    ("spawn_static_local_string_ref", "E2037", 4),
    ("spawn_static_reassigned_local_ref", "E2037", 5),
    ("spawn_static_ref_mut_then_local", "E2037", 6),
    ("static_arg_local_borrow_spawn", "E2037", 11),
    ("static_arg_local_borrow_struct", "E2037", 21),
    ("static_arg_method_param", "E2037", 12),
    ("static_arg_method_ref", "E2037", 12),
    ("static_match_arm_local_borrow", "E2037", 4),
    ("static_qmark_local_borrow", "E2037", 7),
    ("struct_param_static_bound", "E2037", 6),
    ("temp_new_as_str_static_return", "E3030", 2),
];

fn lifetime_diags(json: &str) -> BTreeSet<(String, u64)> {
    let mut out = BTreeSet::new();
    for l in json.lines() {
        let code = l.split("\"code\": \"").nth(1).and_then(|s| s.split('"').next()).unwrap_or("");
        if !matches!(code, "E3030" | "E2037" | "E3010") {
            continue;
        }
        let line = l.split("\"line\": ").nth(1).and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next()).and_then(|s| s.parse().ok()).unwrap_or(0);
        out.insert((code.to_string(), line));
    }
    out
}

fn new_diags(name: &str, src: &str) -> BTreeSet<(String, u64)> {
    let t = Tmp::new(name, src);
    lifetime_diags(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "s4")]).stdout))
}

#[test]
fn lifetimes_rule_matches_expected_on_unsound() {
    let mut bad = vec![];
    let mut stems: Vec<&str> = CASES.iter().map(|c| c.0).collect();
    stems.dedup();
    for stem in stems {
        let src = fs::read_to_string(root().join("stage0/tests/data/unsound").join(format!("{}.ksnl", stem))).unwrap();
        let new = new_diags(&format!("tast_lt_{}", stem), &src);
        for (s, code, line) in CASES.iter().filter(|c| c.0 == stem) {
            if !new.contains(&(code.to_string(), *line)) {
                bad.push(format!("{}: missing {} at line {}", s, code, line));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn lifetimes_rule_is_quiet_on_correct_code() {
    let mut bad = vec![];
    for dir in ["linear_ok"] {
        let d = root().join("stage0/tests/data").join(dir);
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.extension().map_or(true, |x| x != "ksnl") {
                continue;
            }
            let src = fs::read_to_string(&p).unwrap();
            let stem = p.file_stem().unwrap().to_string_lossy().to_string();
            let new = new_diags(&format!("tast_ltok_{}", stem), &src);
            if !new.is_empty() {
                bad.push(format!("{}/{}: {:?}", dir, stem, new));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// 言い換え: 局所の借用を、変数・`match`・`if`・組・`Some`・総称の恒等関数を挟んで返しても E3030。
#[test]
fn lifetimes_rule_sees_through_rephrasing() {
    let wrap = |body: &str| {
        format!(
            "fn t.id[T](x: T) -> T uses [] panics never\n    x\nend t.id\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n\nfn t.bad() -> Option<&u32> uses [] panics never\n    let v = 7u32\n{}\nend t.bad\n",
            body
        )
    };
    let forms = [
        "    Some(&v)",
        "    let r = &v\n    Some(r)",
        "    let r = &v\n    let q = r\n    Some(q)",
        "    let r = t.id(&v)\n    Some(r)",
        "    Some(t.id(&v))",
        "    let o = Some(&v)\n    match o {\n        Some(p) => Some(p)\n        None => None\n    }",
        "    let p = (&v, 1u8)\n    let (a, _b) = p\n    Some(a)",
        "    if true {\n        Some(&v)\n    } else {\n        None\n    }",
        "    let o = Some(&v)\n    let q = o\n    q",
    ];
    for f in forms {
        let d = new_diags("tast_lt_rephrase", &wrap(f));
        assert!(d.iter().any(|x| x.0 == "E3030"), "no E3030 for:\n{}\n{:?}", f, d);
    }
}

/// 言い換えても通るもの: 引数の借用を返す形は E3030 を出さない。
#[test]
fn lifetimes_rule_accepts_argument_borrows() {
    let src = "fn t.id[T](x: T) -> T uses [] panics never\n    x\nend t.id\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n\nfn t.ok(a: &u32) -> Option<&u32> uses [] panics never\n    let r = t.id(a)\n    let o = Some(r)\n    match o {\n        Some(p) => Some(p)\n        None => None\n    }\nend t.ok\n";
    let d = new_diags("tast_lt_arg_ok", src);
    assert!(d.is_empty(), "{:?}", d);
}
