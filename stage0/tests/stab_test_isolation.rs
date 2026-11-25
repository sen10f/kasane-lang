//! `ksn test` の VM: 試験ごとの隔離 (static・確保領域)、`?` の再実行なし、stderr、深さの上限が木走査・ネイティブと同じ (opus-stab S-02/03/05/10/12)。
mod common;
use common::*;
use std::fs;

fn go(name: &str, src: &str, args: &[&str]) -> (Option<i32>, String, String) {
    let t = Tmp::new(name, src);
    let _ = fs::remove_file(t.0.join("marker"));
    let o = run(args, &t.0);
    let r = (o.status.code(), text(&o.stdout), text(&o.stderr));
    let _ = fs::remove_dir_all(&t.0);
    r
}

fn lines(s: &str) -> Vec<String> {
    s.lines().filter(|l| l.starts_with("test ")).map(|l| l.split(" (").next().unwrap().to_string()).collect()
}

#[test]
fn s10_statics_fresh_per_test() {
    let src = include_str!("data/stab_test/s10.ksnl");
    let tree = go("stab10t", src, &["test", "--engine=tree"]);
    let vm = go("stab10v", src, &["test", "--engine=vm"]);
    assert_eq!(tree.0, Some(0), "{:?}", tree);
    assert_eq!(vm.0, Some(0), "{:?}", vm);
    assert_eq!(lines(&tree.1), lines(&vm.1));
    let both = go("stab10b", src, &["test", "--both"]);
    assert_eq!(both.0, Some(0), "{:?}", both);
}

#[test]
fn s12_arena_reclaimed_between_tests() {
    let src = include_str!("data/stab_test/s12.ksnl");
    let tree = go("stab12t", src, &["test", "--engine=tree"]);
    let vm = go("stab12v", src, &["test", "--engine=vm"]);
    assert_eq!(lines(&tree.1), lines(&vm.1), "vm: {}", vm.1);
    assert!(vm.1.contains("test t.grab.test_z_last ... ok"), "{}", vm.1);
}

#[test]
fn s03_try_failure_never_rerun() {
    let src = include_str!("data/stab_test/s03.ksnl");
    let tree = go("stab03t", src, &["test", "--engine=tree"]);
    let vm = go("stab03v", src, &["test", "--engine=vm"]);
    for r in [&tree, &vm] {
        assert_eq!(r.0, Some(1), "{:?}", r);
        assert_eq!(r.1.matches("test body started").count(), 1, "{:?}", r);
        assert!(r.1.contains("FAILED: returned Err("), "{:?}", r);
    }
    assert_eq!(lines(&tree.1), lines(&vm.1));
}

#[test]
fn s02_stderr_matches_tree() {
    let src = include_str!("data/stab_test/s02.ksnl");
    let tree = go("stab02t", src, &["test", "--engine=tree", "--json"]);
    let vm = go("stab02v", src, &["test", "--engine=vm", "--json"]);
    assert_eq!(tree.0, vm.0);
    assert_eq!(tree.1, vm.1);
    assert_eq!(tree.2, vm.2);
    let both = go("stab02b", src, &["test", "--both"]);
    assert_eq!(both.0, Some(1), "{:?}", both);
}

#[test]
fn s05_depth_limit_same_in_tests() {
    let src = include_str!("data/stab_test/s05.ksnl");
    let tree = go("stab05t", src, &["test", "--engine=tree"]);
    let vm = go("stab05v", src, &["test", "--engine=vm"]);
    let both = go("stab05b", src, &["test", "--both"]);
    assert_eq!(tree.0, vm.0, "{:?} {:?}", tree, vm);
    assert_eq!(lines(&tree.1), lines(&vm.1));
    assert_eq!(lines(&tree.1), lines(&both.1).iter().map(|l| l.to_string()).collect::<Vec<_>>());
}

#[test]
fn s03_try_failure_both_agrees() {
    let src = "fn t.main() -> () uses [] panics never\nend t.main\n\ntest t.main.test_try\n    let n = u32.parse(\"zz\")?\n    core.check.eq(n, 1u32)\nend t.main.test_try\n";
    let tree = go("stab03c", src, &["test", "--engine=tree"]);
    let both = go("stab03d", src, &["test", "--both"]);
    assert_eq!(both.0, Some(1), "{:?}", both);
    assert_eq!(lines(&tree.1), lines(&both.1), "{:?}", both);
}
