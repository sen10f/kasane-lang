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
