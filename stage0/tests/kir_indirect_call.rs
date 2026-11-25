//! 関数値 (`fn(A) -> R` の欄や局所) の間接呼び出し (`fnaddr` / `calli`)。
//! 木走査・VM・ネイティブ (debug / release) が同じ出力・終了コードになり、関数値を通した深い再帰は直接呼び出しと同じ深さ (上限 100000) で 102 になる。
//! ネイティブの IR は ASan 付きでも走らせる (clang があるとき)。

mod common;
use common::*;
use std::process::Command;

const LIMIT: u64 = 100_000;

const BASIC: &str = r#"type t.Op pub
    f: fn(i32) -> i32 uses [] panics never
end t.Op

fn t.add(x: i32) -> i32 uses [] panics never
    x.wrapping_add(7i32)
end t.add

fn t.dbl(x: i32) -> i32 uses [] panics never
    x.wrapping_mul(2i32)
end t.dbl

fn t.apply(o: t.Op, x: i32) -> i32 uses [] panics never
    let f = o.f
    f(x)
end t.apply

fn t.calc() -> i32 uses [] panics never
    let a = t.Op {
        f: t.add
    }
    let b = t.Op {
        f: t.dbl
    }
    t.apply(a, 3i32).wrapping_add(t.apply(b, 10i32))
end t.calc

fn t.main() -> Result<(), std.io.Error> uses [io.console, process] panics may
    let r = t.calc()
    std.io.print_line("applied")?
    std.process.exit(r)
end t.main
"#;

fn deep(k: u64) -> String {
    format!(
        "type t.R pub\n    f: fn(u64) -> u64 uses [] panics never\nend t.R\n\n\
         fn t.down(n: u64) -> u64 uses [] panics never\n    if n == 0u64 {{\n        return 0u64\n    }}\n    let r = t.R {{\n        f: t.down\n    }}\n    let g = r.f\n    g(n.wrapping_sub(1u64)).wrapping_add(1u64)\nend t.down\n\n\
         fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may\n    core.mem.drop(std.io.print_line(\"start\"))\n    let r = t.down({}u64)\n    core.mem.drop(std.io.print_line(\"done\"))\n    core.mem.drop(r)\n    Ok(())\nend t.main\n",
        k
    )
}

/// 木走査・VM・ネイティブ (debug / release) で走らせ、(終了コード, 標準出力, 標準エラー) を揃えて返す。
fn run_everywhere(name: &str, src: &str) -> (Option<i32>, String, String) {
    let t = Tmp::new(name, src);
    let tree = run(&["run", "--engine=tree"], &t.0);
    let vm = run(&["run", "--engine=vm"], &t.0);
    let want = (tree.status.code(), text(&tree.stdout), text(&tree.stderr));
    assert_eq!((vm.status.code(), text(&vm.stdout), text(&vm.stderr)), want, "{}: vm", name);
    if clang_available() && !cfg!(windows) {
        for release in [false, true] {
            let mut c = Command::new(exe());
            c.args(["build", "--backend", "llvm"]).current_dir(&t.0);
            if release {
                c.arg("--release");
            }
            let o = c.output().unwrap();
            let out = text(&o.stdout) + &text(&o.stderr);
            assert!(o.status.success() && !out.contains("skipped"), "{}: 間接呼び出しはネイティブに下りるはず: {}", name, out);
            let p = t.0.join("target").join(if release { "release" } else { "debug" }).join("t");
            assert!(artifact_is_native(&p), "{}: ネイティブの実行ファイルのはず", name);
            let r = Command::new(&p).output().unwrap();
            assert_eq!((r.status.code(), text(&r.stdout), text(&r.stderr)), want, "{}: native (release={})", name, release);
        }
    }
    want
}

#[test]
fn a_fn_pointer_field_is_called_on_every_engine() {
    let (code, out, _) = run_everywhere("icall_basic", BASIC);
    assert_eq!(code, Some(30));
    assert_eq!(out, "applied\n");
}

#[test]
fn the_kir_has_fnaddr_and_calli() {
    let t = Tmp::new("icall_kir", BASIC);
    let o = run(&["kir", "t.apply", "--opt"], &t.0);
    let s = text(&o.stdout);
    assert!(s.contains("calli %"), "{}", s);
    let o = run(&["kir", "t.main"], &t.0);
    assert!(text(&o.stdout).contains("= fnaddr t.add"), "{}", text(&o.stdout));
}

#[test]
fn deep_recursion_through_a_fn_pointer_just_below_the_limit_runs() {
    let (code, out, err) = run_everywhere("icall_deep_ok", &deep(LIMIT - 2));
    assert_eq!(code, Some(0), "{}", err);
    assert_eq!(out, "start\ndone\n");
}

#[test]
fn deep_recursion_through_a_fn_pointer_stops_with_102_at_the_same_depth() {
    let (code, out, err) = run_everywhere("icall_deep_over", &deep(LIMIT - 1));
    assert_eq!(code, Some(102));
    assert_eq!(err, "stack overflow\n");
    assert_eq!(out, "start\n");
}

/// clang と ASan のランタイムが使えるか (無ければ飛ばす。あるのにビルドが落ちたら失敗)。
fn asan_available() -> bool {
    if !clang_available() || cfg!(windows) {
        return false;
    }
    let d = std::env::temp_dir().join(format!("icall_asan_probe_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("a.c"), "int main(void){return 0;}\n").unwrap();
    let ok = Command::new("clang").args(["-fsanitize=address", "a.c", "-o", "a"]).current_dir(&d).output().is_ok_and(|o| o.status.success());
    let _ = std::fs::remove_dir_all(&d);
    ok
}

#[test]
fn the_native_build_runs_clean_under_asan() {
    if !asan_available() {
        eprintln!("asan skipped: clang or the ASan runtime is absent");
        return;
    }
    let t = Tmp::new("icall_asan", BASIC);
    let o = run_with_env(&["build", "--release", "--backend", "llvm"], &t.0, &[("KSN_CLANG_ARGS", "-fsanitize=address")]);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert!(o.status.success() && !out.contains("skipped"), "{}", out);
    let r = Command::new(t.0.join("target/release/t")).env("ASAN_OPTIONS", "detect_leaks=0").output().unwrap();
    assert_eq!(r.status.code(), Some(30), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), "applied\n");
    assert!(!text(&r.stderr).contains("AddressSanitizer"), "{}", text(&r.stderr));
}
