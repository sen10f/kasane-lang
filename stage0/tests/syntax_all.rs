//! 構文エラーは定義ごとに全部報告し、正しい定義には診断を付けない。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;

const GOOD: &str = "fn t.good() -> String uses [] panics never\n    match Some(1u8) {\n        Some(_) => \"some\"\n        None => \"\"\n    }\nend t.good\n";

fn src(bad: &[&str]) -> String {
    let mut s = String::new();
    for (i, b) in bad.iter().enumerate() {
        s.push_str(&format!("fn t.bad{}() -> u8 uses [] panics never\n    {}\nend t.bad{}\n\n", i, b, i));
    }
    s.push_str(GOOD);
    s.push_str("\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n");
    s
}

#[test]
fn every_broken_definition_is_reported_once_and_good_ones_are_silent() {
    let t = Tmp::new_raw("syn_all", &src(&["let f = |x| x", "let g = |y| y", "let h = |z| z"]));
    let out = run(&["check"], &t.0);
    let o = text(&out.stderr) + &text(&out.stdout);
    for ln in [2, 6, 10] {
        assert!(o.contains(&format!(":{}:", ln)) && o.matches("E0050").count() == 3, "{}", o);
    }
    assert!(!o.contains("E0057") && !o.contains("E0055"), "{}", o);
    let f = run(&["fmt"], &t.0);
    let fo = text(&f.stderr) + &text(&f.stdout);
    assert_eq!(fo.matches("E0050").count(), 3, "{}", fo);
    assert!(!fo.contains("E0057"), "{}", fo);
}
