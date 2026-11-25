//! 呼び出し深さの上限 (`kir_exec::MAX_DEPTH` = 100000、仕様 5.3) の境界。
//! 入口 `t.main` が 1 回目の呼び出し。`t.down(k)` は k + 1 回の呼び出しの連鎖なので、k = 上限 - 2 が最後に通る深さ、k = 上限 - 1 が超過。
//! 木走査・VM・ネイティブ (debug / release) が同じ深さで、同じ終了コード 102 と `stack overflow` で止まる。

mod common;
use common::*;
use std::process::Command;

const LIMIT: u64 = 100_000;

fn src(k: u64) -> String {
    let mut chain = String::new();
    for i in 1..=5 {
        let call = if i < 5 { format!("    t.c{}(x)\n", i + 1) } else { "    x.wrapping_add(1u64)\n".to_string() };
        chain.push_str(&format!(
            "fn t.c{i}(x: u64) -> u64 uses [io.console] panics never\n    core.mem.drop(std.io.print_line(\"c{i}\"))\n{call}end t.c{i}\n\n"
        ));
    }
    format!(
        "{chain}fn t.down(n: u64) -> u64 uses [io.console] panics never stack 4096\n    if n == 0u64 {{\n        return t.c1(0u64)\n    }}\n    t.down(n.wrapping_sub(1u64)).wrapping_add(1u64)\nend t.down\n\n\
         fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may\n    core.mem.drop(std.io.print_line(\"start\"))\n    let r = t.down({k}u64)\n    core.mem.drop(std.io.print_line(\"done\"))\n    core.mem.drop(r)\n    Ok(())\nend t.main\n"
    )
}

/// 4 つ (木走査・VM・debug / release のネイティブ) で走らせ、(終了コード, 標準出力, 標準エラー) を揃えて返す。
fn run_everywhere(name: &str, k: u64) -> (Option<i32>, String, String) {
    let t = Tmp::new(name, &src(k));
    let tree = run(&["run", "--engine=tree"], &t.0);
    let vm = run(&["run", "--engine=vm"], &t.0);
    let want = (tree.status.code(), text(&tree.stdout), text(&tree.stderr));
    assert_eq!((vm.status.code(), text(&vm.stdout), text(&vm.stderr)), want, "{}: vm", name);
    if clang_available() && !cfg!(windows) {
        for release in [false, true] {
            let mut c = Command::new(exe());
            c.args(["build", "--backend", "llvm"]).current_dir(&t.0);
            if release {
                c.args(["--release", "--allow-bundle"]);
            }
            let o = c.output().unwrap();
            let out = text(&o.stdout) + &text(&o.stderr);
            assert_native_or_reasoned_skip(&out);
            let p = t.0.join("target").join(if release { "release" } else { "debug" }).join("t");
            let p = native_or_launcher(p, &out);
            let r = Command::new(&p).output().unwrap();
            assert_eq!((r.status.code(), text(&r.stdout), text(&r.stderr)), want, "{}: native (release={})", name, release);
        }
    }
    want
}

/// 再帰が上限の手前まで届いたあとで、表示する 5 段の鎖を呼ぶ。超過が鎖のどこで起きても、表示した行と 102 が一致する。
#[test]
fn chain_near_the_limit_prints_the_same_lines_on_every_engine() {
    for k in (LIMIT - 9)..=(LIMIT - 2) {
        let (code, out, _) = run_everywhere(&format!("depth_chain_{}", k), k);
        let lines = out.lines().filter(|l| l.starts_with('c')).count();
        if code == Some(0) {
            assert_eq!(lines, 5, "k={}", k);
        } else {
            assert_eq!(code, Some(102), "k={}", k);
        }
    }
}

/// 鎖が純粋域 (深さの記録つきの複製 `.slow` を持つ) になっていて、試験が振り分けを通っている。
#[test]
fn the_printing_chain_is_in_the_dispatched_region() {
    let t = Tmp::new("depth_chain_ir", &src(5));
    let o = run(&["build", "--emit-llvm", "out.ll"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let ll = std::fs::read_to_string(t.0.join("out.ll")).unwrap();
    assert!(ll.contains("@\"k.t.c1.slow\"") && ll.contains("@\"k.t.c1.w\""), "t.c1 should be dispatched");
}

/// 鎖の関数が 2 MB の局所の配列を持つとき、残りの検査は鎖の枠の上限を見て遅い道へ回る。
/// スレッドの再帰が底に近づいても、ネイティブは SIGSEGV (終了コードなし) ではなく `stack overflow` (102) で止まる。
#[test]
fn a_chain_with_a_huge_local_stops_with_102_not_sigsegv() {
    if !clang_available() || cfg!(windows) {
        return;
    }
    let source = "fn t.leaf(seed: u8) -> u64 uses [] panics may\n    let mut big = [seed; 2000000]\n    big[0] = seed.wrapping_add(1u8)\n    u64.from_u8(big[usize.from_u64(u64.from_u8(seed))])\nend t.leaf\n\n\
fn t.mid(seed: u8) -> u64 uses [] panics may\n    t.leaf(seed.wrapping_add(1u8))\nend t.mid\n\n\
fn t.down(n: u64, seed: u8) -> u64 uses [] panics may\n    let mut chunk = [seed; 100000]\n    chunk[0] = seed.wrapping_add(1u8)\n    let a = t.mid(chunk[usize.from_u64(n % 100000u64)])\n    if n == 0u64 {\n        return a\n    }\n    t.down(n.wrapping_sub(1u64), chunk[1]).wrapping_add(a)\nend t.down\n\n\
fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may\n    let spawned = std.thread.spawn(\n        std.alloc.SYSTEM,\n        fn() -> u64 capture [] {\n            t.down(5000u64, 1u8)\n        },\n    )\n    let h = match spawned {\n        Ok(handle) => handle\n        Err(_e) => core.panic.panic(\"spawn failed\")\n    }\n    let r = h.join()\n    core.mem.drop(std.io.print_line(\"done\"))\n    core.mem.drop(r)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("depth_chain_huge", source);
    let mut n = 0;
    for release in [false, true] {
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&t.0);
        if release {
            c.args(["--release", "--allow-bundle"]);
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        assert_native_or_reasoned_skip(&out);
        let p = native_or_launcher(t.0.join("target").join(if release { "release" } else { "debug" }).join("t"), &out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!((r.status.code(), text(&r.stderr)), (Some(102), "stack overflow\n".to_string()), "release={}", release);
        n += 1;
    }
    assert!(n > 0);
}
