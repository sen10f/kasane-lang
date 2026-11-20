//! S4D: 旧い検査だけが出していた E2042 (捨てた値)・E2021 / E2052 (`match` の網羅性)・E3031 (可変にできない場所)・E5003 (`unsafe` の外の
//! `extern` / `core.ptr`)・E2030 (境界の充足) を、TAST の規則 (`discard.rs`・`exhaustive.rs`・`mutability.rs`・`unsafe_calls.rs`・`bounds.rs`)
//! が旧い検査と同じ (コード, 行) で出すこと。正しいコードで黙ることは `tests/shadow_diff.py --full` が全件で見る。

mod common;
use common::*;
use std::collections::BTreeSet;
use std::fs;

fn diags(json: &str, code: &str) -> BTreeSet<u64> {
    let mut out = BTreeSet::new();
    for l in json.lines() {
        if l.split("\"code\": \"").nth(1).and_then(|s| s.split('"').next()) != Some(code) {
            continue;
        }
        if let Some(n) = l.split("\"line\": ").nth(1).and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next()).and_then(|s| s.parse().ok()) {
            out.insert(n);
        }
    }
    out
}

fn same(stems: &[&str], code: &str) {
    for stem in stems {
        let p = root().join(format!("stage0/tests/data/unsound/{}.ksnl", stem));
        let src = fs::read_to_string(&p).unwrap();
        let t = Tmp::new(&format!("tast_s4d_{}_{}", code, stem), &src);
        let old = diags(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "old")]).stdout), code);
        let new = diags(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "new")]).stdout), code);
        assert!(!old.is_empty(), "{}: 旧い検査が {} を出さない", stem, code);
        assert_eq!(old, new, "{} {}", stem, code);
    }
}

#[test]
fn discarded_values_match_old() {
    same(&["discard_if_else_value_stmt", "discard_loop_tail_match_stmt", "discard_match_arm_value_stmt", "discard_method_result_stmt", "discard_result_call_stmt", "loop_body_discards_value", "while_body_discards_value", "one_armed_if_discards_bool_tail"], "E2042");
}

#[test]
fn match_exhaustiveness_matches_old() {
    same(&["exh_match_char_nonexhaustive", "exh_match_enum_payload_lit_nonexhaustive", "exh_match_int_lit_nonexhaustive", "exh_match_nested_nonexhaustive", "exh_match_result_option_nonexhaustive", "exh_match_str_lit_nonexhaustive", "exh_match_tuple_lit_nonexhaustive", "small7_bool_binding_arm"], "E2021");
}

#[test]
fn immutable_places_match_old() {
    same(
        &[
            "borrow_mut_immutable_closure_param", "borrow_mut_immutable_generic_result", "borrow_mut_immutable_match_binding", "hunt_a05_fnmut_call_through_shared_ref_01",
            "hunt_a13_assign_through_shared_ref_04", "mut_binding_index_assign", "mut_binding_ref_field", "mut_shared_deref_assign", "mut_shared_self_field_assign",
            "ref_destructure_shared_name_assigned", "shared_self_field_mut_ref", "shared_write_mutex_get", "shared_write_user_get",
        ],
        "E3031",
    );
}

#[test]
fn unsafe_calls_match_old() {
    same(&["hunt_a15_ptr_add_non_unsafe", "hunt_a15_ptr_from_mut_non_unsafe", "hunt_a15_ptr_is_null_non_unsafe", "hunt_a15_ptr_read_non_unsafe", "hunt_a15_ptr_type_annot_non_unsafe", "hunt_a11_spawn_rawptr_04"], "E5003");
}

#[test]
fn trait_bounds_match_old() {
    same(&["bound_option_eq", "bound_result_eq", "bound_tuple_eq", "bound_vec_eq", "eq_without_bound", "gen_bound_user_trait_prim_fn", "hunt_a12_generic_bound_leak_01"], "E2030");
}
