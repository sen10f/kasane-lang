//! デフォルト確保器の短縮型名 (`std.vec.Vec<T>` など) と、コンストラクタの
//! `Vec.new()` (アロケータ省略) の試験。fmt の書き換え、E0020、および
//! `ksn run` (木走査) とネイティブ (LLVM) の出力の一致を確かめる。

mod common;

use std::fs;
use std::process::{Command, Output};

use common::*;

/// 短縮型名とアロケータ省略コンストラクタ (`Vec.new()`) が型検査・実行を通り、
// `run` とネイティブの出力が一致する。
#[test]
fn std_default_alloc_short_names_run_agree_with_native() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut v: std.vec.Vec<u8> = std.vec.Vec.new()
    match v.push(1u8) {
        Ok(()) => ()
        Err(_) => ()
    }
    match v.push(2u8) {
        Ok(()) => ()
        Err(_) => ()
    }
    core.check.eq(v.len(), 2usize)
    core.check.eq(v.get(0usize), Some(&1u8))
    core.check.eq(v.get(7usize), None)
    std.io.print_line("vec ok")?
    Ok(())
end t.main
"#;
    let t = Tmp::new("std_alloc_vec", src);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    assert_eq!(text(&interp.stdout), "vec ok\n");
    if cfg!(windows) {
        return;
    }
    let b = run(&["build", "--backend", "llvm"], &t.0);
    assert_eq!(b.status.code(), Some(0), "{}{}", text(&b.stdout), text(&b.stderr));
    if !clang_available() {
        return;
    }
    assert!(!text(&b.stdout).contains("backend skipped"), "{}", text(&b.stdout));
    let exe_path = t.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t" });
    let n = Command::new(&exe_path).current_dir(&t.0).output().expect("native");
    same_output(&interp, &n);
}

/// 長い形式 (`alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)`) は
// fmt で短縮名 (`std.vec.Vec<u8> = std.vec.Vec.new()`) に書き換えられ、書き換え後は
// fmt --check / check が通る。
#[test]
fn fmt_rewrites_alloc_long_form_to_short() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    match v.push(1u8) {
        Ok(()) => ()
        Err(_) => ()
    }
    core.check.eq(v.len(), 1usize)
    std.io.print_line("vec ok")?
    Ok(())
end t.main
"#;
    let t = Tmp::new_raw("std_alloc_fmt", src);
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(1));
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("std.vec.Vec<u8>"), "{}", fixed);
    assert!(fixed.contains("std.vec.Vec.new()"), "{}", fixed);
    assert!(!fixed.contains("alloc.vec.Vec"), "{}", fixed);
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{}{}", fixed, "");
}

/// 長い形式の型名 (`alloc.vec.Vec<u8, std.alloc.System>`) は fmt を通さなければ
// E0020 (整形の誤り) になる。
#[test]
fn long_form_alloc_type_reports_e0020() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    std.io.print_line("vec ok")?
    Ok(())
end t.main
"#;
    let t = Tmp::new_raw("std_alloc_e0020", src);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stdout).contains("E0020"), "{}", text(&o.stdout));
}

/// `alloc.vec.Vec.new()` / `alloc.string.String.new()`（アロケータ未指定）は E2001 で、
// メッセージに短い書き方の案内が入り、`ksn fix` で `std.x.Y.new()` に短縮され、その後 check が通る。
// `from_str`/`with_capacity` 系には短縮形（short_ctor_call で書き換えられるのは `.new` だけ）がないので、
// 案内文は付かず E2001 だけが出る。
#[test]
fn e2001_alloc_ctor_missing_allocator_hint_and_fix() {
    let cases = [
        ("alloc.vec.Vec.new()", "alloc.vec.Vec.new(std.alloc.SYSTEM)", "std.vec.Vec.new()", "std.vec.Vec<u8>"),
        (
            "alloc.string.String.new()",
            "alloc.string.String.new(std.alloc.SYSTEM)",
            "std.string.String.new()",
            "std.string.String",
        ),
    ];
    for (ctor, pass_ctor, short, ty) in cases {
        let src = format!(
            "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let v: {} = {}\n    core.mem.drop(v)\n    Ok(())\nend t.main\n",
            ty, ctor
        );
        let t = Tmp::new_raw(&format!("e2001_alloc_ctor_{}", short), &src);
        let o = run(&["check", "--json"], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}{}", text(&o.stdout), text(&o.stderr));
        let out = text(&o.stdout);
        assert!(out.contains("E2001"), "{}", out);
        // `--json` はメッセージ中の `"` を `\"` にエスケープする。
        let j = |s: &str| s.replace('"', "\\\"");
        assert!(
            out.contains(&format!(" write `{}` (default allocator) or pass the allocator: `{}`", j(short), j(pass_ctor))),
            "{}",
            out
        );
        let o = run(&["fix"], &t.0);
        assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
        let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
        assert!(fixed.contains(short), "{}", fixed);
        let o = run(&["check"], &t.0);
        assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    }

    // `alloc.string.String.from_str(text)` / `alloc.vec.Vec.with_capacity(n)`（アロケータ未指定）は E2001 で、
    // 案内文が入り、`ksn fix` で `std.x.Y.<ctor>(args)` に短縮され、その後 check/run/ネイティブが通る。
    for (ctor, pass_ctor, short, ty) in [
        (
            "alloc.string.String.from_str(\"x\")",
            "alloc.string.String.from_str(std.alloc.SYSTEM, \"x\")",
            "std.string.String.from_str(\"x\")",
            "std.string.String",
        ),
        (
            "alloc.vec.Vec.with_capacity(4usize)",
            "alloc.vec.Vec.with_capacity(std.alloc.SYSTEM, 4usize)",
            "std.vec.Vec.with_capacity(4usize)",
            "std.vec.Vec<u8>",
        ),
    ] {
        let src = format!(
            "fn t.main() -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never\n    let v: {} = {}?\n    core.mem.drop(v)\n    Ok(())\nend t.main\n",
            ty, ctor
        );
        let stem = if short.starts_with("std.string") { "e2001_from_str" } else { "e2001_with_capacity" };
        let t = Tmp::new(stem, &src);
        let o = run(&["check", "--json"], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}{}", text(&o.stdout), text(&o.stderr));
        let out = text(&o.stdout);
        assert!(out.contains("E2001"), "{}", out);
        // `--json` はメッセージ中の `"` を `\"` にエスケープする。
        let j = |s: &str| s.replace('"', "\\\"");
        assert!(
            out.contains(&format!(" write `{}` (default allocator) or pass the allocator: `{}`", j(short), j(pass_ctor))),
            "{}",
            out
        );
        let o = run(&["fix"], &t.0);
        assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
        let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
        assert!(fixed.contains(short), "{}", fixed);
        let c = run(&["check"], &t.0);
        assert_eq!(c.status.code(), Some(0), "{}{}{}{}", fixed, text(&c.stdout), text(&c.stderr), "");
        assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
        let interp = run(&["run"], &t.0);
        assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
        if cfg!(windows) {
            continue;
        }
        let b = run(&["build", "--backend", "llvm"], &t.0);
        assert_eq!(b.status.code(), Some(0), "{}{}", text(&b.stdout), text(&b.stderr));
        if !clang_available() {
            continue;
        }
        assert!(!text(&b.stdout).contains("backend skipped"), "{}", text(&b.stdout));
        let exe_path = t.0.join("target").join("debug").join("t");
        let n = std::process::Command::new(&exe_path).current_dir(&t.0).output().expect("native");
        assert_eq!(text(&interp.stdout), text(&n.stdout), "{}", text(&n.stderr));
    }
}

/// 短縮型名は文字列・ハッシュマップなど他の確保器付き型でも型検査を通る。
#[test]
fn short_names_of_other_alloc_types_are_accepted() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut m: std.collections.HashMap<u8, u8> = std.collections.HashMap.new()
    match m.insert(1u8, 2u8) {
        Ok(_old) => ()
        Err(_e) => ()
    }
    core.check.eq(m.get(&1u8), Some(&2u8))
    std.io.print_line("ok")?
    Ok(())
end t.main
"#;
    let t = Tmp::new("std_alloc_other", src);
    let check = run(&["check"], &t.0);
    assert_eq!(check.status.code(), Some(0), "{}{}", text(&check.stdout), text(&check.stderr));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
}
