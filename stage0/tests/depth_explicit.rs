//! 入口の `depth M` と `std.thread.spawn_deep` (仕様 5.3、設計_深さの上限.md): 上限をプログラムの中で上げる・下げる。
//! 木走査・VM・ネイティブ (debug / release) が、同じ深さで同じ終了コード 102 と `stack overflow` で止まる。
//! 入口 `t.main` が 1 回目の呼び出しで、`t.down(k)` は k + 1 回の連鎖。k = 上限 - 2 が最後に通る深さ、k = 上限 - 1 が超過。

mod common;
use common::*;
use std::process::Command;

fn src(header_tail: &str, k: u64) -> String {
    format!(
        "fn t.down(n: u64) -> u64 uses [] panics never stack 4096\n    if n == 0u64 {{\n        return 0u64\n    }}\n    t.down(n.wrapping_sub(1u64)).wrapping_add(1u64)\nend t.down\n\n\
         fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may{}\n    core.mem.drop(std.io.print_line(\"start\"))\n    let r = t.down({}u64)\n    core.mem.drop(std.io.print_line(\"done\"))\n    core.mem.drop(r)\n    Ok(())\nend t.main\n",
        header_tail, k
    )
}

fn native_runs(t: &Tmp) -> Vec<(Option<i32>, String, String)> {
    let mut v = vec![];
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
            let p = t.0.join("target").join(if release { "release" } else { "debug" }).join("t.native");
            let p = native_or_launcher(p, &out);
            let r = Command::new(&p).output().unwrap();
            v.push((r.status.code(), text(&r.stdout), text(&r.stderr)));
        }
    }
    v
}

fn run_everywhere(name: &str, source: &str) -> (Option<i32>, String, String) {
    let t = Tmp::new(name, source);
    let tree = run(&["run", "--engine=tree"], &t.0);
    let vm = run(&["run", "--engine=vm"], &t.0);
    let want = (tree.status.code(), text(&tree.stdout), text(&tree.stderr));
    assert_eq!((vm.status.code(), text(&vm.stdout), text(&vm.stderr)), want, "{}: vm", name);
    for (i, got) in native_runs(&t).into_iter().enumerate() {
        assert_eq!(got, want, "{}: native (release={})", name, i == 1);
    }
    want
}

#[test]
fn a_larger_entry_depth_raises_the_limit_on_every_engine() {
    let m = 200_000u64;
    let (code, out, err) = run_everywhere("depthx_big_ok", &src(&format!(" depth {}", m), m - 2));
    assert_eq!(code, Some(0), "{}", err);
    assert_eq!(out, "start\ndone\n");
    let (code, out, err) = run_everywhere("depthx_big_over", &src(&format!(" depth {}", m), m - 1));
    assert_eq!(code, Some(102));
    assert_eq!(err, "stack overflow\n");
    assert_eq!(out, "start\n");
}

#[test]
fn a_smaller_entry_depth_lowers_the_limit_on_every_engine() {
    let m = 1000u64;
    let (code, out, err) = run_everywhere("depthx_small_ok", &src(&format!(" depth {}", m), m - 2));
    assert_eq!(code, Some(0), "{}", err);
    assert_eq!(out, "start\ndone\n");
    let (code, out, err) = run_everywhere("depthx_small_over", &src(&format!(" depth {}", m), m - 1));
    assert_eq!(code, Some(102));
    assert_eq!(err, "stack overflow\n");
    assert_eq!(out, "start\n");
}

#[test]
fn fmt_puts_stack_before_depth() {
    let t = Tmp::new_raw(
        "depthx_fmt",
        "fn t.main() -> Result<(), std.io.Error> uses [] panics never depth 5000 stack 4096\n    Ok(())\nend t.main\n",
    );
    assert_ne!(run(&["check"], &t.0).status.code(), Some(0));
    run(&["fmt"], &t.0);
    let s = std::fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(s.contains("panics never stack 4096 depth 5000\n"), "{}", s);
}

fn spawn_src(spawn: &str, depth_arg: &str, k: u64) -> String {
    format!(
        "fn t.down(n: u64) -> u64 uses [] panics may\n    if n == 0u64 {{\n        return 0u64\n    }}\n    t.down(n.wrapping_sub(1u64)).wrapping_add(1u64)\nend t.down\n\n\
         fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may\n    let spawned = std.thread.{}(\n        std.alloc.SYSTEM,\n        fn() -> u64 capture [] {{\n            t.down({}u64)\n        }},\n{}    )\n    let h = match spawned {{\n        Ok(handle) => handle\n        Err(_e) => core.panic.panic(\"spawn failed\")\n    }}\n    let r = h.join()\n    core.mem.drop(std.io.print_line(\"done\"))\n    core.mem.drop(r)\n    Ok(())\nend t.main\n",
        spawn, k, depth_arg
    )
}

/// `spawn` したスレッドの深さは、そのスレッドの根から数え、既定は 100000。`spawn_deep` で上げられる (木走査とネイティブ)。
#[test]
fn spawn_deep_raises_the_limit_of_a_thread_and_plain_spawn_keeps_the_default() {
    // 既定 (100000): 根の呼び出しの数え方 (クロージャの枠・入口の関数) は実行系で 1〜2 回の違いがあるので、境界から離れた値で確かめる。
    for (k, code) in [(99_990u64, 0), (100_010u64, 102)] {
        let t = Tmp::new(&format!("depthx_sp_{}", k), &spawn_src("spawn", "", k));
        let mut outs = vec![run(&["run", "--engine=tree"], &t.0)].into_iter().map(|o| (o.status.code(), text(&o.stderr))).collect::<Vec<_>>();
        outs.extend(native_runs(&t).into_iter().map(|(c, _o, e)| (c, e)));
        for (c, e) in outs {
            assert_eq!(c, Some(code), "k={}: {}", k, e);
        }
    }
    // 200000 に上げれば 150000 の再帰が通る。
    let t = Tmp::new("depthx_sp_deep", &spawn_src("spawn_deep", "        200000usize,\n", 150_000));
    let mut outs = vec![run(&["run", "--engine=tree"], &t.0)].into_iter().map(|o| (o.status.code(), text(&o.stderr))).collect::<Vec<_>>();
    outs.extend(native_runs(&t).into_iter().map(|(c, _o, e)| (c, e)));
    for (c, e) in outs {
        assert_eq!(c, Some(0), "{}", e);
    }
}

/// 1 回の枠が大きく、深さの上限より先にスタックが尽きるときは、SIGSEGV ではなく残りの検査で `stack overflow` (102)。
#[test]
fn a_huge_frame_stops_with_102_before_the_os_stack_runs_out() {
    if !clang_available() || cfg!(windows) {
        return;
    }
    let source = "fn t.big(n: u64, seed: u8) -> u64 uses [] panics may\n    let mut chunk = [seed; 20000]\n    chunk[0] = seed.wrapping_add(1u8)\n    if n == 0u64 {\n        return 0u64\n    }\n    let r = t.big(n.wrapping_sub(1u64), chunk[usize.from_u64(n % 20000u64)])\n    u64.from_u8(chunk[0]).wrapping_sub(r)\nend t.big\n\n\
                  fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    let r = t.big(90000u64, 1u8)\n    core.mem.drop(std.io.print_line(\"done\"))\n    core.mem.drop(r)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("depthx_huge_frame", source);
    for (c, o, e) in native_runs(&t) {
        assert_eq!((c, o.as_str(), e.as_str()), (Some(102), "", "stack overflow\n"));
    }
}
