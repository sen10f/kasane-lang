//! TAST の上の規則 C (参照の段数、`src/tast_rules/ref_levels.rs`) が、旧い検査と同じ (コード, 行) を出すこと (S2A)。
//! `KSN_CHECKER=new` は新しい規則の診断だけを返す。対象は `data/unsound/` のうち、参照の段数の食い違いを旧い検査の
//! E2020 / E2023 / E2024 で拒むケース (台帳 `tests/shadow_baseline.json` から外れたもの)。

mod common;
use common::*;
use std::collections::BTreeSet;
use std::fs;

const STEMS: &[&str] = &[
    "capture_ref_used_as_value",
    "capturing_closure_as_fn_pointer",
    "closure_ref_capture_moves_body",
    "enum_payload_ref_string",
    "generic_arg_ref_chan_send",
    "generic_arg_ref_explicit",
    "generic_arg_ref_prim_push",
    "let_ref_ann_from_value",
    "literal_field_ref_direct",
    "literal_field_ref_generic",
    "literal_field_ref_nested",
    "literal_field_ref_variant",
    "move_let_deref_struct",
    "move_let_deref_tuple",
    "op_ref_arith",
    "op_ref_cmp",
    "op_ref_struct_depth",
    "option_arrayref_as_slice_arg",
    "ref_destructure_if_let_param_as_value",
    "ref_destructure_mut_name_as_value",
    "ref_destructure_name_as_value",
    "ref_destructure_tuple_let_as_value",
    "unit_fn_returns_string_ref",
    "wrong_ref_level",
];

/// `ksn check --json` の出力から、参照の段数の規則が担う 3 コードの (コード, 行) を集める。
fn ref_diags(json: &str) -> BTreeSet<(String, u64)> {
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

#[test]
fn ref_levels_rule_matches_old_checker_on_unsound() {
    let mut bad = vec![];
    for stem in STEMS {
        let src = fs::read_to_string(root().join("stage0/tests/data/unsound").join(format!("{}.ksnl", stem))).unwrap();
        let t = Tmp::new(&format!("tast_rl_{}", stem), &src);
        let old = ref_diags(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "old")]).stdout));
        let new = ref_diags(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "new")]).stdout));
        // 新しい規則は、旧いものが出す E2020 / E2023 / E2024 のうち参照の段数の食い違いを同じ行・同じコードで出す。
        // (旧いものの診断は型の単一化の食い違いも含むので、新が旧の部分集合であり、かつ空でないことを見る。)
        if new.is_empty() || !new.is_subset(&old) {
            bad.push(format!("{}: old={:?} new={:?}", stem, old, new));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
