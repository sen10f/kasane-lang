//! TAST の上の効果の実体化 (`src/tast_rules/effects_inst.rs`・`effects_conv.rs`・`captures.rs`、S4B) と、一時値・欄ごとの借用
//! (`lifetimes.rs`): 旧い検査が出していた E4001 / E4002 / E3010 を `KSN_CHECKER=new` が同じ (コード, 行) で出すこと、
//! 言い換えても拒まれること、正しいコードは黙ることを確かめる。

mod common;
use common::*;
use std::collections::BTreeSet;
use std::fs;

fn diags(json: &str, codes: &[&str]) -> BTreeSet<(String, u64)> {
    let mut out = BTreeSet::new();
    for l in json.lines() {
        let code = l.split("\"code\": \"").nth(1).and_then(|s| s.split('"').next()).unwrap_or("");
        if !codes.contains(&code) {
            continue;
        }
        let line = l.split("\"line\": ").nth(1).and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next()).and_then(|s| s.parse().ok()).unwrap_or(0);
        out.insert((code.to_string(), line));
    }
    out
}

fn run(name: &str, src: &str, checker: &str, codes: &[&str]) -> BTreeSet<(String, u64)> {
    let t = Tmp::new(name, src);
    diags(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", checker)]).stdout), codes)
}

/// 新しい規則が (まだ) 出さない既知の差 (旧い検査の明示型引数 `f::<..>` の効果の実体化)。
const KNOWN_GAPS: &[&str] = &[];

#[test]
fn effect_rules_reject_every_unsound_effect_case() {
    let mut bad = vec![];
    let mut n = 0;
    let dir = root().join("stage0/tests/data/unsound");
    let mut files: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().map_or(false, |x| x == "ksnl")).collect();
    files.sort();
    for p in files {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        if !matches!(expect_value(&p, "expect").as_deref(), Some("E4001") | Some("E4002")) || KNOWN_GAPS.contains(&stem.as_str()) {
            continue;
        }
        let src = fs::read_to_string(&p).unwrap();
        let want = expect_value(&p, "expect").unwrap();
        let new = run(&format!("tast_eff_new_{}", stem), &src, "new", &[want.as_str()]);
        n += 1;
        if new.is_empty() {
            bad.push(format!("{}: 新しい規則が {} を出さない", stem, want));
        }
    }
    assert!(n >= 60, "対象のケースが少なすぎる: {}", n);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn temp_and_slot_borrows_match_old_checker() {
    for stem in [
        "temp_let_whole_ref", "temp_ref_vec_len", "temp_ref_str_len", "d06_temporary_borrow", "temp_as_str_bound",
        "return_swap_fncall_direct", "return_swap_fncall_let", "return_swap_opt_fncall_direct", "elided_flow_deref_store", "header_elided_write_named", "elided_flow_return_deref", "elided_mixed_inner", "flow_guard_temp_get_mut", "return_swap_tuple_let2", "return_swap_opt_let2", "lifetime_slot_opt_swap",
    ] {
        let p = root().join(format!("stage0/tests/data/unsound/{}.ksnl", stem));
        let src = fs::read_to_string(&p).unwrap();
        let old = run(&format!("tast_tmp_old_{}", stem), &src, "old", &["E3010"]);
        let new = run(&format!("tast_tmp_new_{}", stem), &src, "new", &["E3010"]);
        assert!(!old.is_empty(), "{}: 旧い検査が E3010 を出さない", stem);
        assert_eq!(old, new, "{}", stem);
    }
}

fn with_main(rest: &str) -> String {
    format!("fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n\n{}", rest)
}

#[test]
fn effect_variable_instantiation_sees_through_rephrasing() {
    // 局所の変数を挟んだ `for` の反復子・入れ子のアダプタ・`dyn` 越しのメソッド・未束縛の panic 変数。
    let cases: &[(&str, &str, &str)] = &[
        (
            "for_via_let",
            "fn t.f() -> () uses [] panics never\n    let it = core.iter.map(core.iter.range(0u32, 2u32), fn(i: u32) -> u32 capture [] {\n        core.mem.drop(std.io.print_line(\"x\"))\n        i\n    })\n    for x in it {\n        core.mem.drop(x)\n    }\nend t.f\n",
            "E4001",
        ),
        (
            "unbound_panic_var",
            "fn t.g[P: panics](a: u32) -> u32 uses [] panics P\n    a + 1u32\nend t.g\n\nfn t.f() -> u32 uses [] panics never\n    let v = t.g(1u32)\n    v\nend t.f\n",
            "E4002",
        ),
        (
            "dyn_var",
            "trait t.Tr[E: uses]\n    fn m(self: &Self) -> () uses [E] panics never\nend t.Tr\n\nfn t.f[E: uses](h: &dyn t.Tr<E>) -> () uses [] panics never\n    h.m()\nend t.f\n",
            "E4001",
        ),
        (
            "local_closure_panic",
            "fn t.f(xs: &[u32]) -> u32 uses [] panics never\n    let g = fn(i: usize) -> u32 capture [&xs] {\n        xs[i]\n    }\n    g(1usize)\nend t.f\n",
            "E4002",
        ),
    ];
    for (name, body, code) in cases {
        let src = with_main(body);
        let got = run(&format!("tast_eff_rephrase_{}", name), &src, "new", &[code]);
        assert!(!got.is_empty(), "{}: {} が出ない", name, code);
    }
}

#[test]
fn effect_and_temp_rules_are_quiet_on_correct_code() {
    for dir in ["check_ok", "linear_ok"] {
        let d = root().join(format!("stage0/tests/data/{}", dir));
        let mut files: Vec<_> = fs::read_dir(&d).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().map_or(false, |x| x == "ksnl")).collect();
        files.sort();
        // 全件は `tests/shadow_diff.py --full` が見る。ここは標本 (6 件に 1 件)。
        for p in files.into_iter().step_by(6) {
            let stem = p.file_stem().unwrap().to_string_lossy().to_string();
            let src = fs::read_to_string(&p).unwrap();
            let got = run(&format!("tast_eff_quiet_{}_{}", dir, stem), &src, "new", &["E4001", "E4002", "E3010"]);
            assert!(got.is_empty(), "{}/{}: {:?}", dir, stem, got);
        }
    }
}
