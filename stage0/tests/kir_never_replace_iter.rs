//! KIR → LLVM の下ろしの不足 (セルフホストの LLVM 出力の段の報告、selfhost/README.md 13.5):
//! `!` を返す利用者の関数と `std.process.exit`、`core.mem.replace` / `swap`、`for` の対象の
//! `split` / `chars` / `slice.iter()` の本体での `break` / `continue`。
//!
//! 各プログラムは、退避なしのネイティブ (debug と release の `build_native_strict`) が作れて、
//! その標準出力と終了コードが `ksn run` (木走査) と一致し、さらに期待する出力であることを確かめる。

#![allow(dead_code, unused_imports)]

mod common;

use std::process::Command;
use common::*;

/// 退避なしのネイティブ (debug / release) が作れ、標準出力と終了コードが `ksn run` と一致し、
/// どちらも `want_out` と `want_code` であることを確かめる。
fn native_matches_run(name: &str, src: &str, want_out: &str, want_code: i32) {
    for release in [false, true] {
        let (t, exe, out) = build_native_strict(&format!("{}_{}", name, release), src, release);
        let p = exe.unwrap_or_else(|| panic!("{}: ネイティブが退避なしで作れない: {}", name, out));
        let r = run(&["run"], &t.0);
        assert_eq!(r.status.code(), Some(want_code), "{}: 木走査の終了コード: {}{}", name, text(&r.stdout), text(&r.stderr));
        assert_eq!(text(&r.stdout), want_out, "{}: 木走査の標準出力", name);
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(want_code), "{} (release={}): ネイティブの終了コード: {}{}", name, release, text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), text(&r.stdout), "{} (release={}): native and interpreter outputs differ", name, release);
    }
}

// ---- 1. `!` を返す関数と `std.process.exit` ----

/// `!` の関数を `match` の腕で使う (発散する腕)。終了コードは `std.process.exit` の値。
#[test]
fn a_never_returning_fn_in_a_match_arm_exits_with_its_code() {
    let src = r#"fn t.fail(msg: &str, code: i32) -> ! uses [io.console, process] panics never
    core.mem.drop(std.io.print_line(msg))
    std.process.exit(code)
end t.fail

fn t.pick(o: Option<u32>) -> u32 uses [io.console, process] panics never
    match o {
        Some(x) => x.wrapping_add(1u32)
        None => t.fail("none", 2i32)
    }
end t.pick

fn t.main() -> Result<(), std.io.Error> uses [io.console, process] panics may
    core.check.eq(t.pick(Some(4u32)), 5u32)
    std.io.print_line("picked")?
    let _v = t.pick(None)
    std.io.print_line("unreached")?
    Ok(())
end t.main
"#;
    native_matches_run("nri_never_match", src, "picked\nnone\n", 2);
}

/// `!` の関数が `!` の関数を呼ぶ連なり。終了コードは計算した値。
#[test]
fn a_never_returning_fn_can_call_another_never_returning_fn() {
    let src = r#"fn t.bail(code: i32) -> ! uses [io.console, process] panics never
    std.process.exit(code.wrapping_add(40i32))
end t.bail

fn t.check_all(n: u32) -> ! uses [io.console, process] panics never
    if n > 3u32 {
        t.bail(2i32)
    }
    t.bail(1i32)
end t.check_all

fn t.main() -> Result<(), std.io.Error> uses [io.console, process] panics may
    std.io.print_line("start")?
    t.check_all(9u32)
end t.main
"#;
    native_matches_run("nri_never_chain", src, "start\n", 42);
}

/// `std.process.exit(0)` は正常な終わりと同じ終了コードを返す。ループの後ろにあっても、前の出力は残る。
#[test]
fn process_exit_with_zero_after_a_loop_keeps_the_output() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console, process] panics may
    let mut i = 0u32
    loop {
        i = i.wrapping_add(1u32)
        if i == 5u32 {
            break
        }
    }
    std.io.print_line("five")?
    std.process.exit(0i32)
end t.main
"#;
    native_matches_run("nri_exit_zero", src, "five\n", 0);
}

// ---- 2. `core.mem.replace` / `swap` ----

#[test]
fn core_mem_replace_and_swap_on_scalars_run_natively() {
    let src = r#"fn t.bump(r: &mut u32) -> u32 uses [] panics never
    core.mem.replace(r, 9u32)
end t.bump

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut a = 1u32
    let old = t.bump(&mut a)
    core.check.eq(old, 1u32)
    core.check.eq(a, 9u32)
    let mut b = 2u32
    core.mem.swap(&mut a, &mut b)
    core.check.eq(a, 2u32)
    core.check.eq(b, 9u32)
    std.io.print_line("ok")?
    Ok(())
end t.main
"#;
    native_matches_run("nri_mem_scalar", src, "ok\n", 0);
}

/// 所有物 (String・Vec) を `&mut` の先 (局所・参照の引数・構造体のフィールド) で入れ替える。
/// 古い値は返った値の持ち主になり、文として捨てた値は解放される。
#[test]
fn core_mem_replace_moves_owned_strings_through_locals_params_and_fields() {
    let src = r#"type t.Rec
    name: alloc.string.String<std.alloc.System>
    n: u32
end t.Rec

fn t.put(s: &mut alloc.string.String<std.alloc.System>, v: alloc.string.String<std.alloc.System>) -> alloc.string.String<std.alloc.System> uses [alloc] panics may
    core.mem.replace(s, v)
end t.put

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut x = alloc.string.String.from_str(std.alloc.SYSTEM, "a").unwrap()
    let old = core.mem.replace(&mut x, alloc.string.String.from_str(std.alloc.SYSTEM, "b").unwrap())
    std.io.print_line(old.as_str())?
    std.io.print_line(x.as_str())?
    let back = t.put(&mut x, alloc.string.String.from_str(std.alloc.SYSTEM, "c").unwrap())
    std.io.print_line(back.as_str())?
    std.io.print_line(x.as_str())?
    let mut r = t.Rec { name: alloc.string.String.from_str(std.alloc.SYSTEM, "rec").unwrap(), n: 1u32 }
    let prev = core.mem.replace(&mut r.name, alloc.string.String.from_str(std.alloc.SYSTEM, "new").unwrap())
    std.io.print_line(prev.as_str())?
    std.io.print_line(r.name.as_str())?
    core.mem.drop(core.mem.replace(&mut x, alloc.string.String.from_str(std.alloc.SYSTEM, "discard").unwrap()))
    std.io.print_line(x.as_str())?
    Ok(())
end t.main
"#;
    native_matches_run("nri_mem_string", src, "a\nb\nb\nc\nrec\nnew\ndiscard\n", 0);
}

/// `swap` で所有物 (String・Vec) を入れ替える。
#[test]
fn core_mem_swap_exchanges_owned_strings_and_vecs() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut a = alloc.string.String.from_str(std.alloc.SYSTEM, "left").unwrap()
    let mut b = alloc.string.String.from_str(std.alloc.SYSTEM, "right").unwrap()
    core.mem.swap(&mut a, &mut b)
    std.io.print_line(a.as_str())?
    std.io.print_line(b.as_str())?
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut w = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(match w.push(7u32) {
        Ok(_) => 0u8
        Err(_) => 1u8
    })
    core.mem.swap(&mut v, &mut w)
    core.check.eq(v.len(), 1usize)
    core.check.eq(w.len(), 0usize)
    core.check.eq(v[0usize], 7u32)
    std.io.print_line("swap ok")?
    Ok(())
end t.main
"#;
    native_matches_run("nri_mem_swap_owned", src, "right\nleft\nswap ok\n", 0);
}

// ---- 3. `for` の対象: `split` / `chars` の値と `slice.iter()`、本体の `break` / `continue` ----

#[test]
fn for_over_a_split_value_supports_break_and_continue() {
    let src = r#"fn t.count_words(s: &str) -> usize uses [] panics never
    let mut n = 0usize
    for w in s.split(",") {
        if w.is_empty() {
            continue
        }
        if w == "stop" {
            break
        }
        n = n.wrapping_add(1usize)
    }
    n
end t.count_words

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.count_words("a,,b,stop,c"), 2usize)
    core.check.eq(t.count_words("x,,,"), 1usize)
    std.io.print_line("split ok")?
    Ok(())
end t.main
"#;
    native_matches_run("nri_for_split", src, "split ok\n", 0);
}

#[test]
fn for_over_a_chars_value_supports_break_and_continue() {
    let src = r#"fn t.count_chars(s: &str) -> u32 uses [] panics never
    let mut n = 0u32
    for c in s.chars() {
        if c == 'z' {
            break
        }
        if c == 'a' {
            continue
        }
        n = n.wrapping_add(1u32)
    }
    n
end t.count_chars

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.count_chars("banana"), 3u32)
    core.check.eq(t.count_chars("bzb"), 1u32)
    core.check.eq(t.count_chars("héllo"), 5u32)
    std.io.print_line("chars ok")?
    Ok(())
end t.main
"#;
    native_matches_run("nri_for_chars", src, "chars ok\n", 0);
}

/// `&[T]` の引数の `slice.iter()` で `break` / `continue` を使う。
#[test]
fn for_over_slice_iter_of_a_parameter_supports_break_and_continue() {
    let src = r#"fn t.first_over(s: &[u32], limit: u32) -> u32 uses [] panics never
    let mut found = 0u32
    for x in s.iter() {
        if *x <= limit {
            continue
        }
        found = *x
        break
    }
    found
end t.first_over

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(match v.push(1u32) {
        Ok(_) => 0u8
        Err(_) => 1u8
    })
    core.mem.drop(match v.push(5u32) {
        Ok(_) => 0u8
        Err(_) => 1u8
    })
    core.mem.drop(match v.push(9u32) {
        Ok(_) => 0u8
        Err(_) => 1u8
    })
    core.check.eq(t.first_over(v.as_slice(), 3u32), 5u32)
    core.check.eq(t.first_over(v.as_slice(), 9u32), 0u32)
    std.io.print_line("slice ok")?
    Ok(())
end t.main
"#;
    native_matches_run("nri_for_slice", src, "slice ok\n", 0);
}

/// 値の `split` の受け手が式 (`text.trim()`) のとき。
#[test]
fn for_over_a_split_of_a_trimmed_value_runs_natively() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let text = " a,bb ,ccc "
    let mut n = 0usize
    for w in text.trim().split(",") {
        n = n.wrapping_add(w.len())
    }
    core.check.eq(n, 7usize)
    std.io.print_line("trim ok")?
    Ok(())
end t.main
"#;
    native_matches_run("nri_for_trim", src, "trim ok\n", 0);
}
