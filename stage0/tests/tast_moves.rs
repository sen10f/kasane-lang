//! TAST の上の移動と線形の規則 (`src/tast_rules/moves.rs`、S3B): E3002 (move 済みの使用・参照の先からの move)・E3003 (線形の値の
//! 消費漏れ)・E3006 (線形な型を `linear` でない型パラメータへ)・E3040 (`with` の本体での消費) が、旧い検査と同じ (コード, 行) で出ること。
//! `KSN_CHECKER=new` は新しい規則の診断だけを返す。あわせて、書き方を変えた (変数を挟む・ブロック・`match` の腕・ループ) 形でも拒まれ、
//! 正しい形は通ることを、手で書いた入力で確かめる。

mod common;
use common::*;
use std::collections::BTreeSet;
use std::fs;

const MINE: &[&str] = &["E3002", "E3003", "E3006", "E3040"];

/// 新しい規則が旧い検査より多く出してよい (旧い検査が先に別の誤りで止まる、または旧い検査が型引数の位置でしか見ない) 診断。
/// どれも旧い検査も拒む入力。
const NEW_EXTRA: &[(&str, &str, u64)] = &[
    ("arena_linear_value", "E3006", 10),
    ("fnonce_linear_fn_bound", "E3006", 12),
    ("linear_array_len_discard", "E3006", 7),
    ("len_of_linear_array_then_consumed", "E3006", 7),
    ("linear_into_hashmap", "E3006", 6),
    ("linear_pushed_into_vec", "E3006", 3),
    ("linear_receiver_in_vec", "E3006", 6),
];

fn diags(json: &str) -> BTreeSet<(String, u64)> {
    let mut out = BTreeSet::new();
    for l in json.lines() {
        let code = l.split("\"code\": \"").nth(1).and_then(|s| s.split('"').next()).unwrap_or("");
        if !MINE.contains(&code) {
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
fn moves_rule_matches_old_checker_on_unsound() {
    let mut bad = vec![];
    let mut n = 0;
    let dir = root().join("stage0/tests/data/unsound");
    let mut files: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().map_or(false, |x| x == "ksnl")).collect();
    files.sort();
    for p in files {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let want = expect_value(&p, "expect");
        if !want.as_deref().map_or(false, |w| MINE.contains(&w)) {
            continue;
        }
        let src = fs::read_to_string(&p).unwrap();
        let t = Tmp::new(&format!("tast_mv_{}", stem), &src);
        let target = expect_value(&p, "target");
        let mut args = vec!["check", "--json"];
        if let Some(tg) = target.as_deref() {
            args.extend(["--target", tg]);
        }
        let old = diags(&text(&run_with_env(&args, &t.0, &[("KSN_CHECKER", "old")]).stdout));
        let new = diags(&text(&run_with_env(&args, &t.0, &[("KSN_CHECKER", "new")]).stdout));
        n += 1;
        let missing: Vec<_> = old.difference(&new).collect();
        let extra: Vec<_> = new.difference(&old).filter(|(c, l)| !NEW_EXTRA.contains(&(stem.as_str(), c.as_str(), *l))).collect();
        if !missing.is_empty() || !extra.is_empty() {
            bad.push(format!("{}: only-old={:?} only-new={:?}", stem, missing, extra));
        }
    }
    assert!(n > 100, "対象のケースが少なすぎる: {}", n);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

const HEAD: &str = "type t.S\n    v: u32\nend t.S\n\nfn t.eat(s: t.S) -> u32 uses [] panics never\n    s.v\nend t.eat\n\nfn t.main() -> () uses [] panics never\n    t.run(true)\nend t.main\n\n";

fn with_run(body: &str) -> String {
    format!("{}fn t.run(c: bool) -> () uses [] panics never\n    let s = t.S {{ v: 1u32 }}\n{}end t.run\n", HEAD, body)
}

#[test]
fn use_after_move_is_rejected_in_many_spellings() {
    let cases: &[(&str, &str)] = &[
        ("direct", "    core.mem.drop(t.eat(s))\n    core.mem.drop(s.v)\n"),
        ("alias", "    let a = s\n    core.mem.drop(t.eat(a))\n    core.mem.drop(s.v)\n"),
        ("block", "    let a = {\n        s\n    }\n    core.mem.drop(a.v)\n    core.mem.drop(s.v)\n"),
        ("one_branch", "    if c {\n        core.mem.drop(t.eat(s))\n    }\n    core.mem.drop(s.v)\n"),
        ("one_arm", "    match c {\n        true => core.mem.drop(t.eat(s))\n        false => core.mem.drop(0u32)\n    }\n    core.mem.drop(s.v)\n"),
        ("loop", "    let mut i = 0u32\n    while i < 3u32 {\n        core.mem.drop(t.eat(s))\n        i = i.wrapping_add(1u32)\n    }\n"),
        ("closure", "    let f = fn() -> u32 capture [move s] {\n        t.eat(s)\n    }\n    core.mem.drop(f())\n    core.mem.drop(s.v)\n"),
        ("some", "    let o = Some(s)\n    core.mem.drop(o.is_some())\n    core.mem.drop(s.v)\n"),
    ];
    for (name, body) in cases {
        let src = with_run(body);
        let got = new_diags(&format!("tast_mv_use_{}", name), &src);
        assert!(got.iter().any(|(c, _)| c == "E3002"), "{}: E3002 が出ない: {:?}", name, got);
    }
}

#[test]
fn moves_that_are_fine_are_accepted() {
    let cases: &[(&str, &str)] = &[
        ("both_branches", "    if c {\n        core.mem.drop(t.eat(s))\n    } else {\n        core.mem.drop(t.eat(s))\n    }\n"),
        ("copy_field_then_move", "    core.mem.drop(s.v)\n    core.mem.drop(t.eat(s))\n"),
        ("reassign", "    let mut a = s\n    core.mem.drop(t.eat(a))\n    a = t.S { v: 2u32 }\n    core.mem.drop(t.eat(a))\n"),
        ("copy_prim_twice", "    let a = 3u32\n    core.mem.drop(a)\n    core.mem.drop(a)\n    core.mem.drop(s.v)\n"),
        ("move_then_return", "    if c {\n        core.mem.drop(t.eat(s))\n        return\n    }\n    core.mem.drop(s.v)\n"),
        ("loop_break_after_move", "    loop {\n        core.mem.drop(t.eat(s))\n        break\n    }\n"),
    ];
    for (name, body) in cases {
        let src = with_run(body);
        let got = new_diags(&format!("tast_mv_ok_{}", name), &src);
        assert!(got.is_empty(), "{}: 正しいコードが拒まれた: {:?}", name, got);
    }
}

#[test]
fn use_after_move_reports_the_use_line() {
    let src = with_run("    core.mem.drop(t.eat(s))\n    core.mem.drop(s.v)\n");
    let got = new_diags("tast_mv_line", &src);
    assert_eq!(got, BTreeSet::from([("E3002".to_string(), line_of(&src, "core.mem.drop(s.v)", 0))]));
}

const LIN: &str = "type t.Res linear\n    id: u32\nend t.Res\n\nfn t.open() -> t.Res uses [] panics never\n    t.Res { id: 1u32 }\nend t.open\n\nfn t.close(r: t.Res) -> u32 uses [] panics never\n    let t.Res { id: id } = r\n    id\nend t.close\n\nfn t.main() -> () uses [] panics never\n    core.mem.drop(1u8)\nend t.main\n\n";

fn lin(body: &str) -> String {
    format!("{}fn t.run(c: bool) -> () uses [] panics never\n{}end t.run\n", LIN, body)
}

#[test]
fn linear_leaks_are_rejected() {
    let cases: &[(&str, &str)] = &[
        ("unused", "    let r = t.open()\n"),
        ("one_branch", "    let r = t.open()\n    if c {\n        core.mem.drop(t.close(r))\n    }\n"),
        ("early_return", "    let r = t.open()\n    if c {\n        return\n    }\n    core.mem.drop(t.close(r))\n"),
        ("wildcard", "    let r = t.open()\n    match Some(r) {\n        Some(_) => core.mem.drop(1u8)\n        None => core.mem.drop(2u8)\n    }\n"),
        ("loop_break", "    let r = t.open()\n    loop {\n        break\n    }\n    core.mem.drop(1u8)\n"),
    ];
    for (name, body) in cases {
        let got = new_diags(&format!("tast_mv_lin_{}", name), &lin(body));
        assert!(got.iter().any(|(c, _)| c == "E3003"), "{}: E3003 が出ない: {:?}", name, got);
    }
}

#[test]
fn linear_consumed_on_every_path_is_accepted() {
    let cases: &[(&str, &str)] = &[
        ("direct", "    let r = t.open()\n    core.mem.drop(t.close(r))\n"),
        ("both_branches", "    let r = t.open()\n    if c {\n        core.mem.drop(t.close(r))\n    } else {\n        core.mem.drop(t.close(r))\n    }\n"),
        ("alias", "    let r = t.open()\n    let q = r\n    core.mem.drop(t.close(q))\n"),
        ("match_arms", "    let o = Some(t.open())\n    match o {\n        Some(r) => core.mem.drop(t.close(r))\n        None => core.mem.drop(0u32)\n    }\n"),
        ("return_after_consume", "    let r = t.open()\n    if c {\n        core.mem.drop(t.close(r))\n        return\n    }\n    core.mem.drop(t.close(r))\n"),
    ];
    for (name, body) in cases {
        let got = new_diags(&format!("tast_mv_linok_{}", name), &lin(body));
        assert!(got.is_empty(), "{}: 正しいコードが拒まれた: {:?}", name, got);
    }
}

#[test]
fn linear_into_non_linear_type_parameter_is_e3006() {
    let src = lin("    let r = t.open()\n    core.mem.drop(r)\n");
    let got = new_diags("tast_mv_e3006", &src);
    assert!(got.contains(&("E3006".to_string(), line_of(&src, "core.mem.drop(r)", 0))), "{:?}", got);
}

#[test]
fn with_body_consuming_the_binding_is_e3040() {
    let src = "type t.Res linear\n    id: u32\nend t.Res\n\nfn t.Res.shut(self: t.Res) -> () uses [] panics never\n    let t.Res { id: id } = self\n    core.mem.drop(id)\nend t.Res.shut\n\nfn t.take(r: t.Res) -> () uses [] panics never\n    r.shut()\nend t.take\n\nfn t.f() -> () uses [] panics never\n    with a = t.Res { id: 1u32 } close a.shut() {\n        t.take(a)\n    }\nend t.f\n\nfn t.main() -> () uses [] panics never\n    t.f()\nend t.main\n";
    let got = new_diags("tast_mv_e3040", src);
    assert!(got.iter().any(|(c, _)| c == "E3040"), "{:?}", got);
}
