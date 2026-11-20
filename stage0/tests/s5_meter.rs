//! S5-0 / S6-0 の計器 (`ksn s5-meter`、`src/s5_meter.rs`) の試験: 報告が JSON 行として読め、
//! 型の突き合わせの食い違いが 0 件で、被覆の行があること。動作は変えない計器なので、結果の中身は見ない。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;

const SRC: &str = "fn t.add(a: i32, b: i32) -> i32 uses [] panics never\n    a.wrapping_add(b)\nend t.add\n\nfn t.main() -> Result<(), u8> uses [] panics never\n    core.mem.drop(t.add(1, 2))\n    Ok(())\nend t.main\n";

fn report_rows(o: &std::process::Output) -> Vec<serde_json::Value> {
    text(&o.stdout)
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap_or_else(|e| panic!("JSON でない行: {} ({})", l, e)))
        .collect()
}

#[test]
fn s5_meter_prints_valid_json_rows() {
    let t = Tmp::new("s5m", SRC);
    let o = run(&["s5-meter"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let rows = report_rows(&o);
    let cov = rows.iter().find(|r| r["kind"] == "coverage").expect("被覆の行");
    assert_eq!(cov["total"], 2, "{}", cov);
    assert_eq!(cov["lowered"], 2, "{}", cov);
    assert!(rows.iter().any(|r| r["kind"] == "site" && r["site"] == "src_ty"), "src_ty の行が無い");
}

#[test]
fn s5_meter_finds_no_type_disagreement() {
    let t = Tmp::new("s5m_dis", SRC);
    let o = run(&["s5-meter"], &t.0);
    let rows = report_rows(&o);
    let dis: Vec<_> = rows.iter().filter(|r| r["kind"] == "disagree").collect();
    assert!(dis.is_empty(), "TAST と食い違った式: {:?}", dis);
}

#[test]
fn s5_meter_env_var_prints_report_on_stderr() {
    let t = Tmp::new("s5m_env", SRC);
    let o = run_with_env(&["kir", "t.add"], &t.0, &[("KSN_S5_METER", "1")]);
    let err = text(&o.stderr);
    let rows: Vec<serde_json::Value> = err
        .lines()
        .filter(|l| l.starts_with('{'))
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("JSON でない行: {} ({})", l, e)))
        .collect();
    assert!(rows.iter().any(|r| r["kind"] == "coverage"), "被覆の行が無い: {}", err);
}
