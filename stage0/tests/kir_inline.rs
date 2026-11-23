//! 小さい非再帰の関数の連鎖は、深さの記録 (入口の検査と加算) を外し、呼び出し側が高さ分を先に検査する。
//! これで LLVM が展開でき、`-O2` のあとに連鎖への呼び出しが残らない。

#![allow(dead_code, unused_imports)]

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::process::Command;

mod common;
use common::*;

const PROG: &str = "fn t.f1(x: u64) -> u64 uses [] panics never
    t.f2(x.wrapping_add(5u64))
end t.f1

fn t.f2(x: u64) -> u64 uses [] panics never
    t.f3(x.wrapping_add(4u64))
end t.f2

fn t.f3(x: u64) -> u64 uses [] panics never
    x.wrapping_add(1u64)
end t.f3

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = t.f1(1u64)
    core.check.eq(a, 11u64)
    std.io.print_line(\"inline ok\")?
    Ok(())
end t.main
";

#[test]
fn small_call_chain_is_inlined_without_depth_guards() {
    let t = Tmp::new("kir_inline", PROG);
    let cache = scratch::Scratch::at(format!("ksn_kir_inline_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let stdlib = root().join("spec").join("stdlib");
    let env = [("KSN_CACHE_DIR", cache.to_str().unwrap()), ("KSN_STDLIB", stdlib.to_str().unwrap())];
    let o = run_with_env(&["build", "--emit-llvm", "out.ll"], &t.0, &env);
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    let ll = fs::read_to_string(t.0.join("out.ll")).unwrap();
    // 純粋域の f1..f3 は深さを書かない。呼び出し側 (main) が高さを検査する。
    for f in ["t.f1", "t.f2", "t.f3"] {
        let pat = format!("@\"k.{}\"(", f);
        let at = ll.lines().position(|l| l.starts_with("define") && l.contains(&pat)).unwrap_or_else(|| panic!("define of {} not found", f));
        let at: usize = ll.lines().take(at).map(|l| l.len() + 1).sum();
        let body = &ll[at..at + ll[at..].find("\n}\n").unwrap()];
        assert!(!body.contains("@ksn.depth"), "{} should have no depth guard:\n{}", f, body);
    }
    assert!(ll.contains("call void @ksn.chk(i64 %depth1, i64 2)"), "caller should check the chain height");
    // -O2 のあと、連鎖への呼び出しは残らない。
    let clang = std::env::var("KSN_CLANG").unwrap_or_else(|_| "clang".to_string());
    let o2 = t.0.join("o2.ll");
    let co = Command::new(&clang).args(["-O2", "-S", "-emit-llvm", "-Wno-override-module"]).arg(t.0.join("out.ll")).arg("-o").arg(&o2).output();
    let Ok(co) = co else { return };
    if !co.status.success() {
        return;
    }
    let opt = fs::read_to_string(&o2).unwrap();
    for f in ["t.f1", "t.f2", "t.f3"] {
        assert!(!opt.lines().any(|l| l.contains("call ") && l.contains(&format!("@\"k.{}\"(", f))), "call to {} remains after -O2", f);
    }
}
