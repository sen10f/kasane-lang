//! TAST の側の表 (`SideTabs`) の中身の試験。表ごとに担当の接頭辞 (`s1d_` = pat_tys) を付ける。
mod common;
use common::*;

fn dump_side(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    let o = run_with_env(&["check"], &t.0, &[("KSN_DUMP_SIDE", "1")]);
    text(&o.stderr)
}

#[test]
fn s1d_pat_tys_modes() {
    let src = "fn t.main() -> i32\n    let p: (i32, i32) = (1, 2)\n    let (a, b) = &p\n    let mut q: (i32, i32) = (3, 4)\n    let (c, d) = &mut q\n    let (e, f) = p\n    0\nend t.main\n";
    let out = dump_side("s1d_modes", src);
    assert!(out.lines().any(|l| l == "# pat_tys"), "{out}");
    assert!(out.contains("PBind a : &i32 Ref"), "{out}");
    assert!(out.contains("PBind c : &mut i32 RefMut"), "{out}");
    assert!(out.contains("PBind e : i32 Move"), "{out}");
    assert!(out.contains("let_tys_mismatch=0"), "{out}");
}

#[test]
fn s1d_pat_tys_nested_shared_wins() {
    let src = "fn t.main() -> i32\n    let mut o: Option<(i32, i32)> = Some((1, 2))\n    let r = &mut o\n    match &r {\n        Some((x, y)) => 0\n        None => 1\n    }\nend t.main\n";
    let out = dump_side("s1d_nested", src);
    assert!(out.contains("PBind x : &i32 Ref"), "{out}");
    assert!(out.contains("let_tys_mismatch=0"), "{out}");
}
