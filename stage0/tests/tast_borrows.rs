//! TAST の上の規則 B (借用の重なり、`src/tast_rules/borrows.rs`) が、旧い検査と同じ (コード, 行) の E3010 を出すこと (S3E)。
//! `KSN_CHECKER=new` は新しい規則の診断だけを返す。対象は `data/unsound/` のうち、旧い検査が「借用が生きている間の競合するアクセス」
//! (`cannot use / mutate / move ... while ... borrowed`) で拒むケース。あわせて、言い換え (変数を 1 段挟む・ブロック・`match` の腕・
//! 総称の関数・`Option` に包む) でも拒むこと、`Copy` の値を借用から取り出す形・ループでの再代入は重ならないことを見る。

mod common;
use common::*;
use std::collections::BTreeSet;
use std::fs;

const STEMS: &[&str] = &[
    "alias_mut_call_args",
    "alias_mut_method_args",
    "alias_mut_nested_literal",
    "alias_mut_through_field",
    "alias_mut_tuple_literal",
    "alias_shared_and_mut_array",
    "borrow_as_str_then_consume",
    "borrow_extend_from_slice_literal",
    "borrow_in_struct_then_consume",
    "borrow_insert_key_btreemap",
    "borrow_insert_key_hashmap",
    "borrow_insert_value_hashmap",
    "borrow_then_branch_consume",
    "borrow_then_consume_use_later",
    "borrow_then_move_capture",
    "boundary_field_source",
    "boundary_method_result_source",
    "boundary_method_through_holder",
    "boundary_option_ref",
    "boundary_second_arg_lifetime",
    "boundary_struct_lifetime",
    "box_new_ref_assign_while_held",
    "box_recv_mut_then_drop",
    "c6_closure_call_flow",
    "c6_fnvalue_call_flow",
    "carry_get_then_push",
    "closure_body_aliases_mut",
    "closure_body_writes_shared_capture",
    "closure_getmut_capture_map_insert",
    "closure_getmut_capture_vec_push",
    "closure_in_args_with_mut",
    "closure_in_hashmap_used_later",
    "closure_in_vec_used_later",
    "closure_move_capture_hidden_hashmap_hidden",
    "closure_move_capture_hidden_move_drop",
    "closure_move_capture_hidden_struct_ref_field",
    "closure_move_capture_hidden_tuple_hide_cap",
    "closure_mut_capture_then_use",
    "closure_returns_capture",
    "closure_shared_capture_then_write",
    "copy_result_ref_pushed_then_mutated",
    "d08_move_receiver_while_field_borrowed",
    "d09_match_scrutinee_while_borrowed",
    "destructure_call_let",
    "destructure_call_match",
    "e01_method_chain_borrow",
    "e02_d22_move_s_while_borrowed_by_v",
    "e02_return_borrow",
    "flow_call_same_mut_ref_twice",
    "flow_closure_option_wrap",
    "flow_closure_tuple_destructure",
    "flow_closure_tuple_match",
    "flow_index_assign_keeps_other_element",
    "flow_mem_swap_same_ref_twice",
    "flow_reborrow_fn_then_method",
    "flow_reborrow_mut_arg_get_mut",
    "flow_reborrow_mut_arg_sub_mut",
    "flow_reborrow_mut_arg_user_fn",
    "flow_reborrow_mut_arg_vec_slice",
    "flow_reborrow_shared_arg_first",
    "flow_reborrow_shared_arg_iter",
    "flow_ref_arg_and_borrow_of_field",
    "flow_ref_arg_and_borrow_of_field_rev",
    "flow_ref_field_borrow_inherits_loan",
    "flow_swap_generic_relay",
    "flow_swap_option_ref_vars",
    "flow_swap_ref_vars",
    "fqn_same_name_arity_flow",
    "fqn_same_name_pop_flow",
    "fqn_same_name_shared_push",
    "generic_id_passes_borrow",
    "generic_id_passes_struct_borrow",
    "generic_tuple_first_borrow",
    "generic_tuple_nested_borrow",
    "hunt_a01_reborrow_conflict",
    "hunt_a01_reborrow_double_use_01",
    "hunt_a01_reborrow_field_01",
    "hunt_a01_reborrow_mut_then_shared_read_01",
    "hunt_a01_reborrow_reassign_01",
    "hunt_a01_reborrow_shared_then_mut_01",
    "hunt_a05_move_andmut_ref_then_use_x_01",
    "hunt_a31_extend_then_use",
    "infer_let_option_borrow_then_drop",
    "lifetime_ties_inputs_unit_ret",
    "mem_replace_index_after_swap_keeps_borrow",
    "mem_replace_index_old_value_keeps_borrow",
    "mem_replace_index_other_elem_keeps_borrow",
    "mem_replace_stores_borrow",
    "mem_replace_struct_borrow",
    "option_take_struct_literal_borrow",
    "option_unwrap_returns_borrow",
    "push_as_str_then_consume",
    "ref_of_ref_deref",
    "store_second_push_keeps_first",
    "stored_borrow_read_copy_keeps_source",
    "stored_borrow_survives_mut_method",
    "vec_pop_returns_stored_borrow",
];

/// `ksn check --json` の出力から、E3010 の (行, 文面) を集める。`overlap` なら「競合するアクセス」の文面だけ。
fn e3010(json: &str, overlap: bool) -> BTreeSet<u64> {
    let mut out = BTreeSet::new();
    for l in json.lines() {
        if !l.contains("\"code\": \"E3010\"") {
            continue;
        }
        let msg = l.split("\"message\": \"").nth(1).unwrap_or("");
        if overlap && !(msg.starts_with("cannot use") || msg.starts_with("cannot mutate") || msg.starts_with("cannot move") || msg.starts_with("the `with`")) {
            continue;
        }
        let line = l.split("\"line\": ").nth(1).and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next()).and_then(|s| s.parse().ok()).unwrap_or(0);
        out.insert(line);
    }
    out
}

fn new_checker(name: &str, src: &str) -> BTreeSet<u64> {
    let t = Tmp::new(name, src);
    e3010(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "new")]).stdout), false)
}

#[test]
fn borrow_rule_matches_old_checker_on_unsound() {
    let bad: Vec<String> = par_map(STEMS, |stem| {
        let src = fs::read_to_string(root().join("stage0/tests/data/unsound").join(format!("{}.ksnl", stem))).unwrap();
        let t = Tmp::new(&format!("tast_bw_{}", stem), &src);
        let old = e3010(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "old")]).stdout), true);
        let new = e3010(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "new")]).stdout), false);
        if old.is_empty() || !old.is_subset(&new) {
            Some(format!("{}: old={:?} new={:?}", stem, old, new))
        } else {
            None
        }
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

const MAIN: &str = "\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";

#[test]
fn borrow_rule_rephrasings_still_rejected() {
    let cases: &[(&str, &str, u64)] = &[
        ("hop", "fn t.bad() -> u32 uses [] panics never\n    let mut x = 1u32\n    let r = &x\n    let q = r\n    x = 2u32\n    *q\nend t.bad\n", 5),
        ("block", "fn t.bad() -> u32 uses [] panics never\n    let mut x = 1u32\n    let r = {\n        let t = &x\n        core.mem.drop(0u8)\n        t\n    }\n    x = 2u32\n    *r\nend t.bad\n", 8),
        ("arm", "fn t.bad(n: u32) -> u32 uses [] panics never\n    let mut x = 1u32\n    let r = match n {\n        1u32 => &x\n        _ => &x\n    }\n    x = 2u32\n    *r\nend t.bad\n", 7),
        ("generic", "fn t.bad() -> u32 uses [] panics never\n    let mut x = 1u32\n    let r = t.id(&x)\n    x = 2u32\n    *r\nend t.bad\n\nfn t.id[T](v: T) -> T uses [] panics never\n    v\nend t.id\n", 4),
        ("option", "fn t.bad() -> u32 uses [] panics never\n    let mut x = 1u32\n    let o = Some(&x)\n    x = 2u32\n    match o {\n        Some(r) => *r\n        None => 0u32\n    }\nend t.bad\n", 4),
        ("mutmut", "fn t.bad() -> u32 uses [] panics never\n    let mut x = 1u32\n    let a = &mut x\n    let b = &mut x\n    *a = 2u32\n    *b\nend t.bad\n", 4),
    ];
    for (name, body, line) in cases {
        let got = new_checker(&format!("tast_bw_re_{}", name), &format!("{}{}", body, MAIN));
        assert!(got.contains(line), "{}: expected E3010 at line {}, got {:?}", name, line, got);
    }
}

#[test]
fn borrow_rule_allows_copy_out_and_loop_reassign() {
    let copy = "fn t.ok() -> u32 uses [] panics never\n    let mut x = 1u32\n    let a = &x\n    let c = *a\n    x = 2u32\n    c.wrapping_add(x)\nend t.ok\n";
    let looped = "fn t.ok() -> u32 uses [] panics never\n    let mut x = 1u32\n    let mut r = &x\n    let mut i = 0u32\n    while i < 2u32 {\n        x = 2u32\n        r = &x\n        i = i.wrapping_add(1u32)\n    }\n    *r\nend t.ok\n";
    for (name, body) in [("copy", copy), ("loop", looped)] {
        let got = new_checker(&format!("tast_bw_ok_{}", name), &format!("{}{}", body, MAIN));
        assert!(got.is_empty(), "{}: unexpected E3010 {:?}", name, got);
    }
}

/// 正しいコード (`linear_ok/`・`check_ok/`) で規則 B が何も出さないこと (配列の定数添字の要素への `mem.replace` の強い更新を含む)。
#[test]
fn borrow_rule_silent_on_correct_code() {
    let mut files = Vec::new();
    for dir in ["linear_ok", "check_ok"] {
        let d = root().join("stage0/tests/data").join(dir);
        let mut v: Vec<_> = fs::read_dir(&d).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().map_or(false, |x| x == "ksnl")).collect();
        v.sort();
        files.extend(v.into_iter().map(|f| (dir, f)));
    }
    let bad: Vec<String> = par_map(&files, |(dir, f)| {
        let stem = f.file_stem().unwrap().to_string_lossy().to_string();
        let src = fs::read_to_string(f).unwrap();
        let got = new_checker(&format!("tast_bw_c_{}", stem), &src);
        if !got.is_empty() {
            Some(format!("{}/{}: {:?}", dir, stem, got))
        } else {
            None
        }
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// 安全だが、慎重さのために拒む形 (`unsound/` には置かない)。別名 `r` を通した `mem.replace` は、`a[1]` の借用 (`&mut z`) を
/// 実際には外す (`z = 4u32` は実行時には安全) が、強い更新は定数の添字を直接書いた形だけに限るので、旧い検査も新しい検査も拒む。
/// 言い換え (別名を畳んで `&mut a[1]` を直接書く) は強い更新が効いて通る。それが正しい (2026-11-19、S4A の統合で分け直した)。
#[test]
fn borrow_rule_conservative_on_alias_into_array_elem() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    let mut x = 1u32\n    let mut z = 3u32\n    let mut y = 2u32\n    let mut y2 = 5u32\n    let mut a = [&mut x, &mut z]\n    let r = &mut a[1]\n    core.mem.drop(core.mem.replace(r, &mut y2))\n    core.mem.drop(core.mem.replace(&mut a[0], &mut y))\n    z = 4u32\n    core.mem.drop(a)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("tast_bw_cons_alias", src);
    let old = e3010(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "old")]).stdout), false);
    let new = e3010(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "new")]).stdout), false);
    assert!(old.contains(&10) && new.contains(&10), "old={:?} new={:?}", old, new);
    let folded = src.replace("    let r = &mut a[1]\n    core.mem.drop(core.mem.replace(r, &mut y2))\n", "    core.mem.drop(core.mem.replace(&mut a[1], &mut y2))\n");
    assert_ne!(folded, src);
    let t = Tmp::new("tast_bw_cons_alias_folded", &folded);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "folded form must pass");
}
