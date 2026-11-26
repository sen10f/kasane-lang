//! `panics never` の `+` / `-` の範囲の証明 (arith_proof.rs) と、`ksn fix` による `wrapping_*` の書き換え。

mod common;
use common::*;
use std::fs;

const SRC: &str = "fn t.last(xs: &[u8]) -> usize uses [] panics never
    if xs.is_empty() {
        return 0usize
    }
    xs.len().wrapping_sub(1usize)
end t.last

fn t.pred(x: u32, y: u32) -> u32 uses [] panics never
    if x >= 3u32 {
        let _a = y.wrapping_sub(3u32)
        x.wrapping_sub(3u32)
    } else {
        0u32
    }
end t.pred

fn t.up(xs: &[u8]) -> usize uses [] panics never
    let mut i = 0usize
    let mut acc = 0usize
    while i < xs.len() {
        acc = acc.wrapping_add(1usize)
        i = i.wrapping_add(1usize)
    }
    acc
end t.up

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main
";

#[test]
#[ignore = "範囲の証明はアルファでは切っている (arith_proof::ENABLED)"]
fn fix_rewrites_only_proven_wrapping() {
    let t = Tmp::new_strict("arith_fix", SRC);
    // 移行の間は、証明できる `wrapping_*` も check を通る。
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    let o = run(&["fix"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let out = fs::read_to_string(t.0.join("src").join("t.ksnl")).unwrap();
    assert!(out.contains("xs.len() - 1usize"), "{}", out);
    assert!(out.contains("x - 3u32"), "{}", out);
    assert!(out.contains("i = i + 1usize"), "{}", out);
    // 証明できないものはそのまま。
    assert!(out.contains("y.wrapping_sub(3u32)"), "{}", out);
    assert!(out.contains("acc.wrapping_add(1usize)"), "{}", out);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
}

#[test]
#[ignore = "範囲の証明はアルファでは切っている (arith_proof::ENABLED)"]
fn proven_shapes_run_the_same_in_vm_and_native() {
    let src = "fn t.pred(x: u32) -> u32 uses [] panics never
    if x >= 3u32 {
        x - 3u32
    } else {
        0u32
    }
end t.pred

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main

test t.pred.test_both
    core.check.eq(t.pred(10u32), 7u32)
    core.check.eq(t.pred(2u32), 0u32)
end t.pred.test_both
";
    let t = Tmp::new_strict("arith_run", src);
    let o = run(&["test", "--both"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}
