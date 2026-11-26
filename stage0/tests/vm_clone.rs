//! `clone` を KIR に下ろす形 (`&String.clone()`・`Option<&T>` / `Option<スカラー>` の `clone`) が、
//! 木走査・VM・ネイティブ (release) で同じ出力になること。VM は退避しないことも見る。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;
use std::process::Command;

const SRC: &str = r#"type t.P derive [Clone, Copy]
    x: u32
end t.P

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut s = std.string.String.new()
    core.mem.drop(s.push_str("hello clone"))
    let mut owned: Option<std.string.String> = None
    let f = fn(sl: &mut Option<std.string.String>) -> () capture [&s] {
        *sl = Some(s.clone())
    }
    f(&mut owned)
    core.mem.drop(s.push_str("!"))
    if let Some(c) = owned {
        std.io.print_line(c.as_str())?
    }
    std.io.print_line(s.as_str())?
    let p = t.P {
        x: 7u32
    }
    let o = Some(&p)
    let c = o.clone()
    let n = Some(3u32)
    let m = n.clone()
    let a = match c {
        Some(w) => w.x
        None => 0u32
    }
    let b = match m {
        Some(w) => w
        None => 0u32
    }
    if a.wrapping_add(b) == 10u32 {
        std.io.print_line("sum ok")?
    }
    Ok(())
end t.main
"#;
const WANT: &str = "hello clone\nhello clone!\nsum ok\n";

#[test]
fn clone_forms_agree_tree_vm_native() {
    let t = Tmp::new("vm_clone", SRC);
    let chk = run(&["check"], &t.0);
    assert!(text(&chk.stdout).trim().is_empty() && chk.status.success(), "check: {}{}", text(&chk.stdout), text(&chk.stderr));
    let tree = run(&["run", "--engine=tree"], &t.0);
    assert_eq!(text(&tree.stdout), WANT, "{}", text(&tree.stderr));
    let vm = run(&["run", "--engine=vm"], &t.0);
    assert!(!text(&vm.stderr).contains("vm engine skipped"), "VM が退避した: {}", text(&vm.stderr));
    assert_eq!(text(&vm.stdout), WANT);
    let (bt, exe_path, out) = build_native_strict("vm_clone_native", SRC, true);
    let p = exe_path.unwrap_or_else(|| panic!("ネイティブが作れない: {}", out));
    let o = Command::new(&p).current_dir(&bt.0).output().unwrap();
    assert_eq!(text(&o.stdout), WANT, "{}", text(&o.stderr));
}
