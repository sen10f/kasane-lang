//! P1/P2/P3: 定数評価と汎用呼び出し結果の呼び出しの robustness 修正の試験。
//! (mythos2 の low 発見 P1, P2, P3 への再現と修正の確かめ)

#![allow(dead_code, unused_imports)]

mod common;

#[path = "common/scratch.rs"]
mod scratch;

use common::*;

/// P1: `t.id(t.eff)()` (汎用呼び出しの結果をその場で呼ぶ) で検査器が
/// `program.rs` の `segs[0]` で panic (rc=2) しない。
/// 呼び出しの結果を呼ぶ式は名前の道を持たないため、段階 1 で未対応として
/// 扱われ、正しいコード (`t.id(t.pure)()` 等) と同様に panic しない。
#[test]
fn p1_call_result_does_not_panic() {
    let src = "\
fn t.eff() -> () uses [io.console] panics never
    core.mem.drop(std.io.print_line(\"x\"))
end t.eff

fn t.go() -> () uses [] panics never
    t.id(t.eff)()
end t.go

fn t.id[T](a: T) -> T uses [] panics never
    a
end t.id

fn t.main() -> () uses [] panics never
    t.go()
end t.main
";
    let t = Tmp::new("p1_call_result", src);
    let o = run(&["check"], &t.0);
    assert_ne!(o.status.code(), Some(2), "panic must not happen: {}", text(&o.stdout));
}

/// P2: `[0u64; 4_000_000_000]` の配列反復の要素数を定数評価のステップ上限に
/// 数え入れ、巨大なメモリ確保をせずに E4024 で落ちる。
#[test]
fn p2_huge_array_repeat_hits_step_limit() {
    let src = "\
static t.X: [u64; 4_000_000_000] = [0u64; 4_000_000_000]

fn t.main() -> () uses [] panics never
    core.mem.drop(t.X.len())
end t.main
";
    let t = Tmp::new("p2_huge_array", src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}: {}", text(&o.stdout), text(&o.stderr));
    assert!(
        codes(&text(&o.stdout)).iter().any(|c| c == "E4024"),
        "E4024 expected: {}",
        text(&o.stdout)
    );
}

/// P3: 定数評価の初期化式がスタックオーバーフロー (`Exit(102)`) で止まっても
/// `ksn check` が rc=0 にならず、E4024 を出して rc=1 にする。
#[test]
fn p3_stack_overflow_reports_e4024() {
    let src = "\
static t.X: u32 = t.deep(0u32)

fn t.deep(a: u32) -> u32 const uses [] panics never stack 128
    if a == 4_000_000_000u32 {
        a
    } else {
        t.deep(a.wrapping_add(1u32))
    }
end t.deep

fn t.main() -> () uses [] panics never
    core.mem.drop(t.X)
end t.main
";
    let t = Tmp::new("p3_stack_overflow", src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}: {}", text(&o.stdout), text(&o.stderr));
    assert!(
        codes(&text(&o.stdout)).iter().any(|c| c == "E4024"),
        "E4024 expected: {}",
        text(&o.stdout)
    );
}
