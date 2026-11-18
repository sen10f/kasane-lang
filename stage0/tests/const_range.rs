//! `const` の初期化式の範囲検査 (E2026)。`hunt_a18_const_usize_range_wasm` は
//! `--target wasm32-unknown-unknown` でのみ usize が 32 ビットとなり検出されるため、
//! `check_diag.rs` の `ksn check` (ターゲット指定なし) では検出できず、ここで個別に検査する。

mod common;
use common::*;

const WASM_SRC: &str = r#"const t.BIG: usize = 5000000000usize

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main
"#;

#[test]
fn const_usize_range_wasm32_is_rejected() {
    let t = Tmp::new("const_usize_range_wasm", WASM_SRC);
    let o = run(&["check", "--target", "wasm32-unknown-unknown", "--json"], &t.0);
    let got = codes(&text(&o.stdout));
    assert_eq!(o.status.code(), Some(1), "expected rejection: {}", text(&o.stdout));
    assert!(got.iter().any(|c| c == "E2026"), "want E2026, got {:?}", got);
}

#[test]
fn const_usize_range_wasm32_ok_without_target() {
    // デフォルトターゲット (64 ビット) では 5000000000usize は usize の範囲内。
    let t = Tmp::new("const_usize_range_wasm_default", WASM_SRC);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "expected acceptance: {}", text(&o.stdout));
}
