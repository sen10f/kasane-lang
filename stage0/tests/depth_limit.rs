//! 呼び出し深さの上限 (`kir_exec::MAX_DEPTH` = 100000、仕様 5.3) の境界。
//! 入口 `t.main` が 1 回目の呼び出し。`t.down(k)` は k + 1 回の呼び出しの連鎖なので、k = 上限 - 2 が最後に通る深さ、k = 上限 - 1 が超過。
//! 木走査・VM・ネイティブ (debug / release) が同じ深さで、同じ終了コード 102 と `stack overflow` で止まる。

mod common;
use common::*;
use std::process::Command;

const LIMIT: u64 = 100_000;

fn src(k: u64) -> String {
    format!(
        "fn t.down(n: u64) -> u64 uses [] panics never stack 4096\n    if n == 0u64 {{\n        return 0u64\n    }}\n    t.down(n.wrapping_sub(1u64)).wrapping_add(1u64)\nend t.down\n\n\
         fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may\n    core.mem.drop(std.io.print_line(\"start\"))\n    let r = t.down({}u64)\n    core.mem.drop(std.io.print_line(\"done\"))\n    core.mem.drop(r)\n    Ok(())\nend t.main\n",
        k
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
            let p = t.0.join("target").join(if release { "release" } else { "debug" }).join("t.native");
            let p = native_or_launcher(p, &out);
            let r = Command::new(&p).output().unwrap();
            assert_eq!((r.status.code(), text(&r.stdout), text(&r.stderr)), want, "{}: native (release={})", name, release);
        }
    }
    want
}

#[test]
fn depth_just_below_the_limit_runs_on_every_engine() {
    let (code, out, err) = run_everywhere("depth_ok", LIMIT - 2);
    assert_eq!(code, Some(0), "{}", err);
    assert_eq!(out, "start\ndone\n");
}

#[test]
fn depth_at_the_limit_stops_with_102_on_every_engine() {
    let (code, out, err) = run_everywhere("depth_over", LIMIT - 1);
    assert_eq!(code, Some(102));
    assert_eq!(err, "stack overflow\n");
    assert_eq!(out, "start\n");
}
