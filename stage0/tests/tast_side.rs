//! TAST の側の表と木の組み立て (設計_型付き検査.md 8 章)。担当ごとに接頭辞を付ける (`s1e_` = 木の組み立て)。
//! `KSN_DUMP_TAST=1 ksn check` が stderr に出す木の中身を確かめる。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;

/// 木のダンプ (stderr) を返す。診断は旧いまま 0 件であること。
fn dump_of(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    let out = run_with_env(&["check"], &t.0, &[("KSN_DUMP_TAST", "1")]);
    assert_eq!(out.status.code(), Some(0), "{}{}", text(&out.stdout), text(&out.stderr));
    text(&out.stderr)
}

const SRC: &str = "\
type t.P derive [Debug] pub
    x: u32
end t.P

fn t.go(p: &mut t.P, v: &[u32]) -> u32 uses [] panics may pub
    let a = 1u32
    let c = fn(y: u32) -> u32 capture [move a] {
        y.wrapping_add(a)
    }
    p.x = c(v[0])
    match v.len() {
        0 => *&p.x
        _ => a
    }
end t.go

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    Ok(())
end t.main
";

#[test]
fn s1e_tree_has_types_places_and_captures() {
    let d = dump_of("s1e_tree", SRC);
    assert!(d.contains("body t.go complete=true"), "{}", d);
    // 引数・欄・添字・参照外しの場所
    assert!(d.contains("place=p@0.x"), "{}", d);
    assert!(d.contains("place=v@0[]"), "{}", d);
    assert!(d.contains("place=*"), "{}", d);
    // `let` の束縛は PBind の ID で指される
    assert!(d.lines().any(|l| l.contains("Path a") && l.contains("place=a@") && !l.contains("place=a@0")), "{}", d);
    // 閉包の取り込みは式 ID に直されて `a` の型付きで入る
    assert!(d.contains("Closure caps=[move a: u32]"), "{}", d);
    // `Match` の被検査式と腕のパターン
    assert!(d.contains("Arm") && d.contains("PLit") && d.contains("PWild"), "{}", d);
    // 型付き (呼び先の経路は別勘定)
    let st = d.lines().find(|l| l.starts_with("stats")).unwrap();
    let n = |k: &str| -> usize { st.split(&format!(" {}=", k)).nth(1).unwrap().split(' ').next().unwrap().parse().unwrap() };
    assert!(n("exprs") > 10 && n("typed") * 100 >= n("exprs") * 95, "{}", st);
}

#[test]
fn s1e_empty_side_tables_build_without_res() {
    // A〜D の表が空でも組める (res / targs / conv / PatInfo は付かないだけ)。埋まれば同じ形で入る。
    let d = dump_of("s1e_empty", SRC);
    let st = d.lines().find(|l| l.starts_with("stats")).unwrap();
    assert!(st.contains("pats_info=") && st.contains("res="), "{}", st);
}
