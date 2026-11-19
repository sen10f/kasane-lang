//! TAST の上の閉包の取り込みの規則 (`src/tast_rules/captures.rs`、S3C): E1101 の閉包の部分 (本体が `capture` に無い外側の名前を使う・
//! `capture [..]` に外側の局所でない名前を書く) が、旧い検査と同じ (コード, 行) で出ること。`KSN_CHECKER=new` は新しい規則の診断だけを返す。
//! 書き方を変えた形 (ブロック・`match` の腕・呼び出しの引数・入れ子の閉包) でも拒まれ、正しい形は通ることを、手で書いた入力で確かめる。

mod common;
use common::*;
use std::collections::BTreeSet;
use std::fs;

fn diags(json: &str) -> BTreeSet<(String, u64)> {
    let mut out = BTreeSet::new();
    for l in json.lines() {
        let code = l.split("\"code\": \"").nth(1).and_then(|s| s.split('"').next()).unwrap_or("");
        if code != "E1101" {
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

fn line_of(src: &str, needle: &str, nth: usize) -> u64 {
    src.lines().enumerate().filter(|(_, l)| l.contains(needle)).nth(nth).map(|(i, _)| i as u64 + 1).unwrap_or(0)
}

#[test]
fn captures_rule_matches_old_checker_on_unsound() {
    let mut bad = vec![];
    let mut n = 0;
    let dir = root().join("stage0/tests/data/unsound");
    let mut files: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().map_or(false, |x| x == "ksnl")).collect();
    files.sort();
    for p in files {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let src = fs::read_to_string(&p).unwrap();
        if expect_value(&p, "expect").as_deref() != Some("E1101") || !src.contains("capture [") {
            continue;
        }
        let t = Tmp::new(&format!("tast_cap_{}", stem), &src);
        let old = diags(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "old")]).stdout));
        let new = diags(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "new")]).stdout));
        n += 1;
        let missing: Vec<_> = old.difference(&new).collect();
        let extra: Vec<_> = new.difference(&old).collect();
        if !missing.is_empty() || !extra.is_empty() {
            bad.push(format!("{}: only-old={:?} only-new={:?}", stem, missing, extra));
        }
    }
    assert!(n >= 5, "対象のケースが少なすぎる: {}", n);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

fn with_run(body: &str) -> String {
    format!("fn t.main() -> () uses [] panics never\n    t.run(true)\nend t.main\n\nfn t.run(c: bool) -> () uses [] panics never\n    let x = 7u8\n    let y = 1u8\n{}end t.run\n", body)
}

#[test]
fn missing_capture_is_rejected_in_many_spellings() {
    let cases: &[(&str, &str)] = &[
        ("direct", "    let f = fn() -> u8 capture [&y] {\n        x\n    }\n    core.mem.drop(f())\n"),
        ("field_path", "    let f = fn() -> u8 capture [] {\n        x.wrapping_add(1u8)\n    }\n    core.mem.drop(f())\n"),
        ("block", "    let f = fn() -> u8 capture [] {\n        let z = 2u8\n        {\n            z.wrapping_add(x)\n        }\n    }\n    core.mem.drop(f())\n"),
        ("match_arm", "    let f = fn() -> u8 capture [] {\n        match c {\n            true => x\n            false => 0u8\n        }\n    }\n    core.mem.drop(f())\n"),
        ("call_arg", "    core.mem.drop((fn(a: u8) -> u8 capture [] {\n        a.wrapping_add(x)\n    })(1u8))\n"),
        ("nested", "    let f = fn() -> u8 capture [&x] {\n        let g = fn() -> u8 capture [] {\n            x\n        }\n        g()\n    }\n    core.mem.drop(f())\n"),
        ("outer_not_reachable", "    let f = fn() -> u8 capture [&x] {\n        let g = fn() -> u8 capture [] {\n            y\n        }\n        g()\n    }\n    core.mem.drop(f())\n"),
    ];
    for (name, body) in cases {
        let src = with_run(body);
        let got = new_diags(&format!("tast_cap_miss_{}", name), &src);
        assert!(got.iter().any(|(c, _)| c == "E1101"), "{}: E1101 が出ない: {:?}", name, got);
    }
}

#[test]
fn unresolved_name_in_capture_list_is_rejected() {
    let src = with_run("    let f = fn() -> u8 capture [&zz] {\n        7u8\n    }\n    core.mem.drop(f())\n");
    let got = new_diags("tast_cap_unres", &src);
    assert_eq!(got, BTreeSet::from([("E1101".to_string(), line_of(&src, "capture [&zz]", 0))]));
    // `move` の取り込みと、閉包を書く位置より後ろで宣言する名前も同じ。
    let src = with_run("    let f = fn() -> u8 capture [move later] {\n        7u8\n    }\n    let later = 1u8\n    core.mem.drop(f())\n    core.mem.drop(later)\n");
    assert!(!new_diags("tast_cap_unres_later", &src).is_empty());
}

#[test]
fn missing_capture_reports_the_use_line() {
    let src = with_run("    let f = fn() -> u8 capture [&y] {\n        x\n    }\n    core.mem.drop(f())\n");
    let got = new_diags("tast_cap_line", &src);
    assert_eq!(got, BTreeSet::from([("E1101".to_string(), line_of(&src, "        x", 0))]));
}

#[test]
fn correct_captures_are_accepted() {
    let cases: &[(&str, &str)] = &[
        ("ref", "    let f = fn() -> u8 capture [&x] {\n        *x\n    }\n    core.mem.drop(f())\n"),
        ("move", "    let f = fn() -> u8 capture [move x] {\n        x\n    }\n    core.mem.drop(f())\n"),
        ("param_shadows", "    let f = fn(x: u8) -> u8 capture [] {\n        x\n    }\n    core.mem.drop(f(3u8))\n"),
        ("inner_let_shadows", "    let f = fn() -> u8 capture [] {\n        let x = 4u8\n        x\n    }\n    core.mem.drop(f())\n"),
        ("arm_binding_shadows", "    let f = fn() -> u8 capture [] {\n        match Some(5u8) {\n            Some(x) => x\n            None => 0u8\n        }\n    }\n    core.mem.drop(f())\n"),
        ("nested_ok", "    let f = fn() -> u8 capture [&x] {\n        let g = fn() -> u8 capture [&x] {\n            *x\n        }\n        g()\n    }\n    core.mem.drop(f())\n"),
        ("outer_use_after", "    let f = fn() -> u8 capture [&y] {\n        *y\n    }\n    core.mem.drop(f())\n    core.mem.drop(x)\n"),
    ];
    for (name, body) in cases {
        let src = with_run(body);
        let got = new_diags(&format!("tast_cap_ok_{}", name), &src);
        assert!(got.is_empty(), "{}: 正しいコードが拒まれた: {:?}", name, got);
    }
}

// ---- 閉包の寿命 (規則 L の閉包の本体)・Fn 境界と関数型の効果 (S3C2) ----

fn code_lines(json: &str, codes: &[&str]) -> BTreeSet<(String, u64)> {
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

fn new_codes(name: &str, src: &str, codes: &[&str]) -> BTreeSet<(String, u64)> {
    let t = Tmp::new(name, src);
    code_lines(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "new")]).stdout), codes)
}

/// 旧い検査が出す (コード, 行) を、新しい規則がすべて出すこと。
fn assert_new_covers_old(stems: &[&str], codes: &[&str]) {
    let mut bad = vec![];
    for stem in stems {
        let p = root().join("stage0/tests/data/unsound").join(format!("{}.ksnl", stem));
        let src = fs::read_to_string(&p).unwrap();
        let t = Tmp::new(&format!("tast_cap2_{}", stem), &src);
        let old = code_lines(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "old")]).stdout), codes);
        let new = code_lines(&text(&run_with_env(&["check", "--json"], &t.0, &[("KSN_CHECKER", "new")]).stdout), codes);
        assert!(!old.is_empty(), "{}: 旧い検査が対象のコードを出さない", stem);
        let missing: Vec<_> = old.difference(&new).collect();
        if !missing.is_empty() {
            bad.push(format!("{}: only-old={:?}", stem, missing));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn closure_lifetime_rules_match_old_checker_on_unsound() {
    assert_new_covers_old(
        &["closure_returns_capture", "hunt_a26_13_closure_elided_as_static", "hunt_a26_14_closure_two_elided", "flow_for_binding_outlives", "elided_flow_return_for_binder"],
        &["E3020", "E3010", "E3030"],
    );
}

#[test]
fn closure_effect_rules_match_old_checker_on_unsound() {
    assert_new_covers_old(
        &[
            "fn_type_closure_arg",
            "fn_bound_inferred_uses",
            "fn_bound_inferred_panics",
            "fn_conv_arg_decl",
            "fn_type_let_uses",
            "fn_type_return",
            "fn_type_struct_field",
            "fnbound_named_fn_io",
            "effect_in_method_closure",
            "closure_inferred_callee_fn_bound",
        ],
        &["E4001", "E4002"],
    );
}

const LIFE_MAIN: &str = "fn t.main() -> () uses [] panics never\n    t.run(1u32)\nend t.main\n\n";

#[test]
fn closure_return_elision_is_rejected_in_many_spellings() {
    let cases: &[(&str, &str)] = &[
        ("no_ref_param", "    let f = fn() -> &u32 capture [&x] {\n        &x\n    }\n    core.mem.drop(*f())\n"),
        ("let_then_call", "    let f = fn() -> &u32 capture [&x] {\n        let r = &x\n        r\n    }\n    core.mem.drop(*f())\n"),
        ("two_ref_params", "    let f = fn(a: &u32, b: &u32) -> &u32 capture [] {\n        a\n    }\n    core.mem.drop(*f(&x, &x))\n"),
        ("in_block", "    let g = {\n        fn() -> &u32 capture [&x] {\n            &x\n        }\n    }\n    core.mem.drop(*g())\n"),
    ];
    for (name, body) in cases {
        let src = format!("{}fn t.run(x: u32) -> () uses [] panics never\n{}end t.run\n", LIFE_MAIN, body);
        let got = new_codes(&format!("tast_cap2_el_{}", name), &src, &["E3020"]);
        assert!(!got.is_empty(), "{}: E3020 が出ない", name);
    }
    // 引数の参照がちょうど 1 つ・領域を書いた形は通る。
    let ok: &[(&str, &str)] = &[
        ("one_ref_param", "    let f = fn(a: &u32) -> &u32 capture [] {\n        a\n    }\n    core.mem.drop(*f(&x))\n"),
        ("named", "    let f = fn(a: &u32) -> &'static u32 capture [] {\n        &7u32\n    }\n    core.mem.drop(*f(&x))\n"),
    ];
    for (name, body) in ok {
        let src = format!("{}fn t.run(x: u32) -> () uses [] panics never\n{}end t.run\n", LIFE_MAIN, body);
        let got = new_codes(&format!("tast_cap2_elok_{}", name), &src, &["E3020", "E3010", "E3030"]);
        assert!(got.is_empty(), "{}: 正しいコードが拒まれた: {:?}", name, got);
    }
}

#[test]
fn closure_body_local_borrow_escape_is_rejected() {
    let src = format!("{}fn t.run(x: u32) -> () uses [] panics never\n    let f = fn(a: u32) -> &u32 capture [] {{\n        &a\n    }}\n    core.mem.drop(*f(x))\nend t.run\n", LIFE_MAIN);
    let got = new_codes("tast_cap2_local_escape", &src, &["E3030", "E3020"]);
    assert!(got.iter().any(|(c, _)| c == "E3030" || c == "E3020"), "閉包の本体の局所の借用が出ていく形が拒まれない: {:?}", got);
}

#[test]
fn for_pattern_binds_the_borrow_of_the_iterated_value() {
    // `for x in v.iter()` の `x` は `v` の借用を運ぶ: 外側の束縛へ入れて `v` より長く使えば拒まれる。
    let bad = format!(
        "{}fn t.run(n: u32) -> () uses [] panics never\n    let mut keep = &n\n    {{\n        let v = [1u32, 2u32]\n        for x in v.iter() {{\n            keep = x\n        }}\n    }}\n    core.mem.drop(*keep)\nend t.run\n",
        LIFE_MAIN
    );
    let got = new_codes("tast_cap2_for_bad", &bad, &["E3010", "E3030"]);
    assert!(!got.is_empty(), "for の束縛が借用を運ばない: {:?}", got);
    let ok = format!(
        "{}fn t.run(n: u32) -> () uses [] panics never\n    let v = [1u32, 2u32]\n    let mut keep = &n\n    for x in v.iter() {{\n        keep = x\n    }}\n    core.mem.drop(*keep)\nend t.run\n",
        LIFE_MAIN
    );
    let got = new_codes("tast_cap2_for_ok", &ok, &["E3010", "E3030"]);
    assert!(got.is_empty(), "正しい for が拒まれた: {:?}", got);
}

const EFF_HEAD: &str = "fn t.main() -> () uses [] panics never\n    t.run()\nend t.main\n\nfn t.loud() -> u32 uses [io.console] panics never\n    core.mem.drop(std.io.print_line(\"x\"))\n    1u32\nend t.loud\n\nfn t.quiet() -> u32 uses [] panics never\n    1u32\nend t.quiet\n\nfn t.strict[F: core.ops.Fn<(), u32, [], never>](f: F) -> u32 uses [] panics never\n    f.call(())\nend t.strict\n\nfn t.apply(p: fn() -> u32 uses [] panics never) -> u32 uses [] panics never\n    p()\nend t.apply\n\n";

#[test]
fn closure_effect_over_the_bound_is_rejected_in_many_spellings() {
    let cases: &[(&str, &str)] = &[
        ("bound_direct", "    core.mem.drop(t.strict(fn() -> u32 capture [] {\n        t.loud()\n    }))\n"),
        ("bound_via_let", "    let f = fn() -> u32 capture [] {\n        t.loud()\n    }\n    core.mem.drop(t.strict(f))\n"),
        ("bound_named_fn", "    core.mem.drop(t.strict(t.loud))\n"),
        ("bound_in_block", "    let g = {\n        fn() -> u32 capture [] {\n            t.loud()\n        }\n    }\n    core.mem.drop(t.strict(g))\n"),
        ("fn_type_arg", "    core.mem.drop(t.apply(fn() -> u32 capture [] {\n        t.loud()\n    }))\n"),
        ("fn_type_let", "    let p: fn() -> u32 uses [] panics never = t.loud\n    core.mem.drop(p())\n"),
        ("fn_type_via_let_closure", "    let f = fn() -> u32 capture [] {\n        t.loud()\n    }\n    core.mem.drop(t.apply(f))\n"),
    ];
    for (name, body) in cases {
        let src = format!("{}fn t.run() -> () uses [] panics never\n{}end t.run\n", EFF_HEAD, body);
        let got = new_codes(&format!("tast_cap2_eff_{}", name), &src, &["E4001"]);
        assert!(!got.is_empty(), "{}: E4001 が出ない", name);
    }
}

#[test]
fn closure_effect_within_the_bound_is_accepted() {
    let cases: &[(&str, &str)] = &[
        ("quiet_closure", "    core.mem.drop(t.strict(fn() -> u32 capture [] {\n        t.quiet()\n    }))\n"),
        ("quiet_named", "    core.mem.drop(t.strict(t.quiet))\n"),
        ("quiet_fn_type", "    core.mem.drop(t.apply(t.quiet))\n"),
        ("quiet_let_type", "    let p: fn() -> u32 uses [] panics never = t.quiet\n    core.mem.drop(p())\n"),
    ];
    for (name, body) in cases {
        let src = format!("{}fn t.run() -> () uses [] panics never\n{}end t.run\n", EFF_HEAD, body);
        let got = new_codes(&format!("tast_cap2_effok_{}", name), &src, &["E4001", "E4002"]);
        assert!(got.is_empty(), "{}: 正しいコードが拒まれた: {:?}", name, got);
    }
    // 呼び出す側が効果を宣言していれば、境界が許す範囲で通る。
    let src = format!(
        "{}fn t.wide[F: core.ops.Fn<(), u32, [io.console], never>](f: F) -> u32 uses [io.console] panics never\n    f.call(())\nend t.wide\n\nfn t.run() -> () uses [io.console] panics never\n    core.mem.drop(t.wide(t.loud))\n    let g = fn() -> u32 capture [] {{\n        t.loud()\n    }}\n    core.mem.drop(g())\nend t.run\n",
        EFF_HEAD.replace("t.run()", "t.run()")
    );
    let got = new_codes("tast_cap2_eff_declared", &src, &["E4001", "E4002"]);
    assert!(got.is_empty(), "効果を宣言した呼び出しが拒まれた: {:?}", got);
}

#[test]
fn local_closure_call_effect_must_be_declared_by_the_caller() {
    let src = format!("{}fn t.run() -> () uses [] panics never\n    let f = fn() -> u32 capture [] {{\n        t.loud()\n    }}\n    core.mem.drop(f())\nend t.run\n", EFF_HEAD);
    let got = new_codes("tast_cap2_call_closure", &src, &["E4001"]);
    assert!(!got.is_empty(), "閉包の呼び出しの効果が宣言を超えても拒まれない");
}
