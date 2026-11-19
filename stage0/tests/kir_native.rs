//! KIR・LLVM・ネイティブビルドのテスト。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use common::*;

/// E2044 (型が決まらない式・束縛) は既定で有効。`--no-strict-types` と `KSN_STRICT_TYPES=0` で旧来の
/// 「黙る」挙動に戻り、`--strict-types` と `KSN_STRICT_TYPES=1` は受け付けるが何も変えない。
/// ケースはそれ 1 件だけで、他の診断を伴わない。
#[test]
fn strict_types_is_the_default_and_can_be_turned_off() {
    let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/diag/E2044.ksnl")).unwrap();
    let t = Tmp::new("strict_types", &src);
    // 既定: E2044 だけ。
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(codes(&text(&o.stdout)), vec!["E2044"], "{}", text(&o.stdout));
    // 旧フラグ・環境変数 (1) は何も変えない。
    let o = run(&["check", "--json", "--strict-types"], &t.0);
    assert_eq!(codes(&text(&o.stdout)), vec!["E2044"]);
    let o = Command::new(exe()).args(["check", "--json"]).env("KSN_STRICT_TYPES", "1").current_dir(&t.0).output().unwrap();
    assert_eq!(codes(&text(&o.stdout)), vec!["E2044"]);
    // 逃げ道: `--no-strict-types` と `KSN_STRICT_TYPES=0` で通る。
    let o = run(&["check", "--json", "--no-strict-types"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    let o = Command::new(exe()).args(["check", "--json"]).env("KSN_STRICT_TYPES", "0").current_dir(&t.0).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    // build / test / run も同じ既定 (E2044 で止まる) で、逃げ道も効く。
    for cmd in ["build", "test", "run"] {
        let o = run(&[cmd], &t.0);
        assert!(text(&o.stderr).contains("E2044") || text(&o.stdout).contains("E2044"), "{}: {}{}", cmd, text(&o.stdout), text(&o.stderr));
        assert_ne!(o.status.code(), Some(0), "{}", cmd);
    }
    // 注釈を書けば通る。
    let fixed = src.replace("let n = None", "let n: Option<u32> = None");
    let t2 = Tmp::new("strict_types_fixed", &fixed);
    let o = run(&["check", "--json"], &t2.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
}

#[test]
fn float_operators_do_not_panic() {
    let f = "fn t.add(a: f64, b: f64) -> f64 uses [] panics never\n    a + b\nend t.add\n\n";
    let t = Tmp::new("float_never", &main_wrap(f, ""));
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E4004"), "{}", text(&o.stdout));
}

#[test]
fn float_keys_do_not_satisfy_ord_bounds() {
    let (got, out) = check_codes(
        "bound_float",
        &["let mut m = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)", "core.mem.drop(m.insert(1.5f64, 1u8))"],
    );
    assert!(got.iter().any(|c| c == "E2030"), "{}", out);
}

#[test]
fn the_llvm_backend_builds_and_runs_a_native_executable() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_ok_{}", release), &clif_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "clif ok");
    }
}

#[test]
fn the_llvm_backend_compiles_checked_arithmetic_and_floats() {
    let (_t, exe_path, out) = build_native("clif_arith", &clif_arith_program("    core.check.eq(1u8, 1u8)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "arith ok");
}

#[test]
fn the_llvm_backend_compiles_structs_enums_match_and_methods() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_struct_{}", release), &clif_struct_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "struct ok");
    }
}

#[test]
fn the_llvm_backend_compiles_option_and_result() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_opt_{}", release), &clif_option_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "option ok");
    }
}

#[test]
fn the_llvm_backend_compiles_vec_and_str() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_vec_{}", release), &clif_vec_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "hello\nvec ok");
    }
}

fn clif_deque_program() -> String {
    let nl = char::from(10u8).to_string();
    let dq = "alloc.collections.VecDeque<u32, std.alloc.System>";
    let dqf = "alloc.collections.VecDeque<f64, std.alloc.System>";
    let sv = "alloc.string.String<std.alloc.System>";
    let dqs = format!("alloc.collections.VecDeque<{}, std.alloc.System>", sv);
    [
        "type t.Bag".to_string(),
        format!("    q: {}", dq),
        "    tag: u32".to_string(),
        "end t.Bag".to_string(),
        String::new(),
        format!("fn t.fresh() -> {}", dq),
        "    alloc.collections.VecDeque.new(std.alloc.SYSTEM)".to_string(),
        "end t.fresh".to_string(),
        String::new(),
        format!("fn t.fresh_f() -> {}", dqf),
        "    alloc.collections.VecDeque.new(std.alloc.SYSTEM)".to_string(),
        "end t.fresh_f".to_string(),
        String::new(),
        format!("fn t.fresh_s() -> {}", dqs),
        "    alloc.collections.VecDeque.new(std.alloc.SYSTEM)".to_string(),
        "end t.fresh_s".to_string(),
        String::new(),
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may".to_string(),
        "    let mut q = t.fresh()".to_string(),
        "    core.check.eq(q.is_empty(), true)".to_string(),
        "    core.check.eq(q.len(), 0usize)".to_string(),
        "    core.check.eq(q.pop_front(), None)".to_string(),
        "    core.check.eq(q.pop_back(), None)".to_string(),
        "    core.mem.drop(match q.push_back(1u32) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    })".to_string(),
        "    core.mem.drop(match q.push_back(2u32) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    })".to_string(),
        "    core.mem.drop(match q.push_back(3u32) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    })".to_string(),
        "    core.mem.drop(match q.push_front(0u32) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    })".to_string(),
        "    core.check.eq(q.len(), 4usize)".to_string(),
        "    core.check.eq(q.pop_front(), Some(0u32))".to_string(),
        "    core.check.eq(q.pop_back(), Some(3u32))".to_string(),
        "    core.check.eq(q.len(), 2usize)".to_string(),
        "    core.mem.drop(match q.push_front(9u32) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    })".to_string(),
        "    core.check.eq(q.pop_front(), Some(9u32))".to_string(),
        "    core.check.eq(q.pop_front(), Some(1u32))".to_string(),
        "    core.check.eq(q.pop_front(), Some(2u32))".to_string(),
        "    core.check.eq(q.is_empty(), true)".to_string(),
        "    // 先入れ先出し: 容量の拡張と先頭の詰めをまたいでも順序が保たれる。".to_string(),
        "    let mut i = 0u32".to_string(),
        "    let mut next = 0u32".to_string(),
        "    while i < 1000u32 {".to_string(),
        "        core.mem.drop(match q.push_back(i) {".to_string(),
        "            Ok(_) => 0u8".to_string(),
        "            Err(_) => 1u8".to_string(),
        "        })".to_string(),
        "        if i % 3u32 == 0u32 {".to_string(),
        "            core.check.eq(q.pop_front(), Some(next))".to_string(),
        "            next = next.wrapping_add(1u32)".to_string(),
        "        }".to_string(),
        "        i = i.wrapping_add(1u32)".to_string(),
        "    }".to_string(),
        "    while !q.is_empty() {".to_string(),
        "        core.check.eq(q.pop_front(), Some(next))".to_string(),
        "        next = next.wrapping_add(1u32)".to_string(),
        "    }".to_string(),
        "    core.check.eq(next, 1000u32)".to_string(),
        "    // 先頭に積んで末尾から取り出す (最後に入れたものが最初に出る)。".to_string(),
        "    let mut j = 0u32".to_string(),
        "    while j < 500u32 {".to_string(),
        "        core.mem.drop(match q.push_front(j) {".to_string(),
        "            Ok(_) => 0u8".to_string(),
        "            Err(_) => 1u8".to_string(),
        "        })".to_string(),
        "        j = j.wrapping_add(1u32)".to_string(),
        "    }".to_string(),
        "    let mut k = 0u32".to_string(),
        "    while k < 500u32 {".to_string(),
        "        core.check.eq(q.pop_back(), Some(k))".to_string(),
        "        k = k.wrapping_add(1u32)".to_string(),
        "    }".to_string(),
        "    core.check.eq(q.is_empty(), true)".to_string(),
        "    // 浮動小数と、解放が要る要素 (String) も Vec と同じように扱う。".to_string(),
        "    let mut f = t.fresh_f()".to_string(),
        "    core.mem.drop(match f.push_back(1.5f64) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    })".to_string(),
        "    core.mem.drop(match f.push_front(0.25f64) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    })".to_string(),
        "    core.check.eq(f.pop_back(), Some(1.5f64))".to_string(),
        "    core.check.eq(f.len(), 1usize)".to_string(),
        "    let mut s = t.fresh_s()".to_string(),
        "    let a = alloc.string.String.from_str(std.alloc.SYSTEM, \"hi\").unwrap()".to_string(),
        "    core.mem.drop(match s.push_front(a) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    })".to_string(),
        "    let b = alloc.string.String.from_str(std.alloc.SYSTEM, \"yo\").unwrap()".to_string(),
        "    core.mem.drop(match s.push_back(b) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    })".to_string(),
        "    core.check.eq(s.len(), 2usize)".to_string(),
        "    match s.pop_front() {".to_string(),
        "        Some(x) => core.check.eq(x.as_str(), \"hi\")".to_string(),
        "        None => core.check.eq(1u8, 2u8)".to_string(),
        "    }".to_string(),
        "    match s.pop_back() {".to_string(),
        "        Some(x) => core.check.eq(x.as_str(), \"yo\")".to_string(),
        "        None => core.check.eq(1u8, 2u8)".to_string(),
        "    }".to_string(),
        "    let mut bag = t.Bag { q: t.fresh(), tag: 7u32 }".to_string(),
        "    core.mem.drop(match bag.q.push_back(5u32) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    })".to_string(),
        "    core.check.eq(bag.q.len(), 1usize)".to_string(),
        "    core.check.eq(bag.tag, 7u32)".to_string(),
        "    core.mem.drop(std.io.print_line(\"deque ok\"))".to_string(),
        "    Ok(())".to_string(),
        "end t.main".to_string(),
        String::new(),
    ]
    .join(&nl)
}

#[test]
fn the_llvm_backend_runs_vecdeque_like_the_interpreter() {
    let src = clif_deque_program();
    let t = Tmp::new("clif_deque_interp", &src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    assert_eq!(text(&interp.stdout).trim(), "deque ok");
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_deque_{}", release), &src, release);
        assert_native_or_reasoned_skip(&out);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
}

/// `split` を変数に持って `next()` で進める書き方 (局所の反復子) は、kir/native-gaps-3 でネイティブになった。出力は木走査と同じ。
#[test]
fn a_split_iterator_held_in_a_variable_runs_natively() {
    let src = [
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let mut it = \"a,b\".split(\",\")",
        "    core.check.eq(it.next(), Some(\"a\"))",
        "    core.check.eq(it.next(), Some(\"b\"))",
        "    core.check.eq(it.next(), None)",
        "    core.mem.drop(std.io.print_line(\"it ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join("\n");
    let t = Tmp::new("str_split_var", &src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    assert_eq!(text(&interp.stdout).trim(), "it ok");
    let (t, exe_path, out) = build_native_strict("str_split_var_clif", &src, false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).current_dir(&t.0).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "it ok");
}

#[test]
fn the_llvm_backend_frees_owned_values_and_compiles_string() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_str_{}", release), &clif_string_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "hello, world\nstring ok");
    }
}

#[test]
fn the_llvm_backend_compiles_slices_and_for_loops() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_slice_{}", release), &clif_slice_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "slice ok");
    }
}

#[test]
fn the_llvm_backend_compiles_tuples_and_structural_equality() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_tuple_{}", release), &clif_tuple_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "tuple ok");
    }
}

#[test]
fn the_llvm_backend_compiles_checked_arithmetic_and_option_methods() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_meth_{}", release), &clif_methods_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "methods ok");
    }
}

#[test]
fn the_llvm_backend_compiles_mutable_references_to_scalars() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_mutref_{}", release), &clif_mutref_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "mutref ok");
    }
}

#[test]
fn the_llvm_backend_compiles_the_question_mark_and_infers_vec_types() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_try_{}", release), &clif_try_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "try ok");
    }
}

#[test]
fn the_llvm_backend_inlines_closures_and_fuses_iterator_pipelines() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_clo_{}", release), &clif_closure_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "closure ok");
    }
}

#[test]
fn the_llvm_backend_runs_user_defined_iterators() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_iter_{}", release), &clif_iterator_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "iterator ok");
    }
}

#[test]
fn the_llvm_backend_compiles_arrays_take_remove_conversions_and_char_methods() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_std_{}", release), &clif_std_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "std ok");
    }
}

fn clif_trunc_program() -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let nan = core.num.f64.NAN",
        "    let inf = core.num.f64.INFINITY",
        "    let ninf = core.num.f64.NEG_INFINITY",
        "    t.show_u8(42.9f64)?",
        "    t.show_u8(200.0f64)?",
        "    t.show_u8(255.9f64)?",
        "    t.show_u8(256.0f64)?",
        "    t.show_u8(0.0f64 - 1.0f64)?",
        "    t.show_u8(nan)?",
        "    t.show_u8(inf)?",
        "    t.show_u8(ninf)?",
        "    t.show_i32(7.5f64)?",
        "    t.show_i32(0.0f64 - 100.8f64)?",
        "    t.show_i32(2147483647.9f64)?",
        "    t.show_i32(2147483648.0f64)?",
        "    t.show_i32(nan)?",
        "    core.check.eq(42.9f64.trunc_to_u8(), Some(42u8))",
        "    core.check.eq(255.9f64.trunc_to_u8(), Some(255u8))",
        "    core.check.eq(256.0f64.trunc_to_u8(), None)",
        "    core.check.eq(nan.trunc_to_u8(), None)",
        "    core.check.eq(inf.trunc_to_u8(), None)",
        "    core.check.eq(ninf.trunc_to_u8(), None)",
        "    core.check.eq(7.5f64.trunc_to_i32(), Some(7i32))",
        "    core.check.eq((0.0f64 - 100.8f64).trunc_to_i32(), Some(0i32 - 100i32))",
        "    core.check.eq(2147483647.9f64.trunc_to_i32(), Some(2147483647i32))",
        "    core.check.eq(2147483648.0f64.trunc_to_i32(), None)",
        "    core.check.eq(nan.trunc_to_i32(), None)",
        "    core.check.eq(200.4f64.trunc_to_u64(), Some(200u64))",
        "    core.check.eq((0.0f64 - 1.0f64).trunc_to_i64(), Some(0i64 - 1i64))",
        "    core.check.eq(5.9f64.trunc_to_isize(), Some(5isize))",
        "    core.check.eq(5.9f64.trunc_to_usize(), Some(5usize))",
        "    core.check.eq(129.0f64.trunc_to_i8(), None)",
        "    core.check.eq((0.0f64 - 5.9f64).trunc_to_i16(), Some(0i16 - 5i16))",
        "    core.check.eq(5.9f64.trunc_to_u16(), Some(5u16))",
        "    core.check.eq(5.9f64.trunc_to_u32(), Some(5u32))",
        "    core.check.eq(5.9f32.trunc_to_u32(), Some(5u32))",
        "    core.mem.drop(std.io.print_line(\"trunc ok\"))",
        "    Ok(())",
        "end t.main",
        "",
        "fn t.show_i32(x: f64) -> Result<(), std.io.Error> uses [io.console] panics may",
        "    match x.trunc_to_i32() {",
        "        Some(7i32) => std.io.print_line(\"i32 7\")",
        "        Some(2147483647i32) => std.io.print_line(\"i32 max\")",
        "        Some(_rest) => std.io.print_line(\"i32 other\")",
        "        None => std.io.print_line(\"i32 none\")",
        "    }",
        "end t.show_i32",
        "",
        "fn t.show_u8(x: f64) -> Result<(), std.io.Error> uses [io.console] panics may",
        "    match x.trunc_to_u8() {",
        "        Some(42u8) => std.io.print_line(\"u8 42\")",
        "        Some(200u8) => std.io.print_line(\"u8 200\")",
        "        Some(255u8) => std.io.print_line(\"u8 255\")",
        "        Some(_rest) => std.io.print_line(\"u8 other\")",
        "        None => std.io.print_line(\"u8 none\")",
        "    }",
        "end t.show_u8",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_llvm_backend_compiles_float_to_int_truncation() {
    for release in [false, true] {
        let (t, exe_path, out) = build_native(&format!("clif_trunc_{}", release), &clif_trunc_program(), release);
        let interp = Command::new(exe()).arg("run").current_dir(&t.0).output().unwrap();
        assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
        assert!(text(&interp.stdout).contains("u8 none"), "{}", text(&interp.stdout));
        assert!(text(&interp.stdout).contains("i32 none"), "{}", text(&interp.stdout));
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
}

#[test]
fn the_llvm_backend_compiles_hashmap_and_btreemap() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_map_{}", release), &clif_map_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "map ok");
    }
}

#[test]
fn the_llvm_backend_monomorphizes_generic_types_and_functions() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_gen_{}", release), &clif_generic_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "generic ok");
    }
}

#[test]
fn the_llvm_backend_specializes_generic_functions_for_closure_arguments() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_fnb_{}", release), &clif_fn_bound_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "fn bound ok");
    }
}

#[test]
fn the_llvm_backend_passes_capturing_closures_to_generic_functions() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("clif_cap_{}", release), &clif_capture_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "capture ok");
    }
}

#[test]
fn ffi_interpreter_calls_c_functions_with_integer_arguments() {
    let src = "extern t.abs(x: i32) -> i32 link \"abs\"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe \"libc\" \"abs of a literal\" {
        t.abs(0i32 - 7i32)
    }
    core.check.eq(a, 7i32)
    core.mem.drop(std.io.print_line(\"ok\"))
    Ok(())
end t.main
";
    let t = Tmp::new("ffi_interp", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let o = run(&["run"], &t.0);
    assert_eq!(text(&o.stdout), "ok\n", "{}", text(&o.stderr));
}

/// 浮動小数の引数・戻り値と生ポインタを扱う C の関数 (共有ライブラリにして `LD_PRELOAD` する)。
/// `bits_*` / `emit` / `ffi_done` は、値のビットパターンを 10 進で印字するための観測用。
#[cfg(unix)]
const FFI_FLOAT_C: &str = r#"#include <stdio.h>
#include <stdlib.h>

float add_f(float a, float b) { return a + b; }
double add_d(double a, double b) { return a + b; }
double mix_i(int n, double x, float y) { return (double)n + x + (double)y; }
double mix2(double a, int b, float c, double d) { return a + (double)b + (double)c + d; }
double six(double a, double b, double c, double d, double e, float f) { return a + b + c + d + e + (double)f; }
double six_i(int a, int b, int c, int d, int e, int f) { return (double)(a + b + c + d + e + f); }
int scale_i(float x) { return (int)(x * 2.0f); }
double *alloc_d(void) { return (double *)malloc(8 * 4); }
void store_d(double *p, double v) { *p = v; }
double load_d(const double *p) { return *p; }
unsigned long long bits_f(float x) { union { float f; unsigned u; } v; v.f = x; return (unsigned long long)v.u; }
unsigned long long bits_d(double x) { union { double d; unsigned long long u; } v; v.d = x; return v.u; }
void emit(unsigned long long v) { printf("%llu\n", v); }
void ffi_done(void) { printf("done\n"); }
"#;

/// 同じ C の関数を呼んで、参照値を印字するドライバ (同じ `ffi.c` と一緒にビルドする)。
#[cfg(unix)]
const FFI_FLOAT_DRIVER_C: &str = r#"#include <stdio.h>
#include <stdlib.h>

float add_f(float, float);
double add_d(double, double);
double mix_i(int, double, float);
double mix2(double, int, float, double);
double six(double, double, double, double, double, float);
double six_i(int, int, int, int, int, int);
int scale_i(float);
double *alloc_d(void);
void store_d(double *, double);
double load_d(const double *);
unsigned long long bits_f(float);
unsigned long long bits_d(double);
void emit(unsigned long long);
void ffi_done(void);

int main(void) {
    emit(bits_f(add_f(1.5f, 2.25f)));
    emit(bits_d(add_d(1.5, 2.25)));
    emit(bits_d(mix_i(3, 0.5, 0.25f)));
    emit(bits_d(mix2(0.5, 7, 0.25f, 1.5)));
    emit(bits_d(six(0.5, 1.5, 2.5, 3.5, 4.5, 0.25f)));
    emit(bits_d(six_i(1, 2, 3, 4, 5, 6)));
    emit((unsigned long long)scale_i(1.5f));
    double *p = alloc_d();
    store_d(p, 2.5);
    p[1] = 3.5;
    emit(bits_d(load_d(p)));
    emit(bits_d(p[1]));
    free(p);
    ffi_done();
    return 0;
}
"#;

/// ドライバと同じ入力で同じ C の関数を呼ぶ Kasane のプログラム (出力を突き合わせる)。
#[cfg(unix)]
const FFI_FLOAT_SRC: &str = r#"extern t.add_f(a: f32, b: f32) -> f32 link "add_f"

extern t.add_d(a: f64, b: f64) -> f64 link "add_d"

extern t.mix_i(n: i32, x: f64, y: f32) -> f64 link "mix_i"

extern t.mix2(a: f64, b: i32, c: f32, d: f64) -> f64 link "mix2"

extern t.six(a: f64, b: f64, c: f64, d: f64, e: f64, f: f32) -> f64 link "six"

extern t.six_i(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> f64 link "six_i"

extern t.scale_i(x: f32) -> i32 link "scale_i"

extern t.alloc_d() -> *mut f64 link "alloc_d"

extern t.store_d(pointer: *mut f64, value: f64) -> () link "store_d"

extern t.load_d(pointer: *const f64) -> f64 link "load_d"

extern t.free(pointer: *mut f64) -> () link "free"

extern t.bits_f(x: f32) -> u64 link "bits_f"

extern t.bits_d(x: f64) -> u64 link "bits_d"

extern t.emit(v: u64) -> () link "emit"

extern t.ffi_done() -> () link "ffi_done"

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    unsafe "libc" "floats and raw pointers" {
        t.emit(t.bits_f(t.add_f(1.5f32, 2.25f32)))
        t.emit(t.bits_d(t.add_d(1.5f64, 2.25f64)))
        t.emit(t.bits_d(t.mix_i(3i32, 0.5f64, 0.25f32)))
        t.emit(t.bits_d(t.mix2(0.5f64, 7i32, 0.25f32, 1.5f64)))
        t.emit(t.bits_d(t.six(0.5f64, 1.5f64, 2.5f64, 3.5f64, 4.5f64, 0.25f32)))
        t.emit(t.bits_d(t.six_i(1i32, 2i32, 3i32, 4i32, 5i32, 6i32)))
        match core.num.u64.try_from_i32(t.scale_i(1.5f32)) {
            Ok(v) => t.emit(v)
            Err(_e) => t.emit(0u64)
        }
        let p = t.alloc_d()
        t.store_d(p, 2.5f64)
        core.ptr.write(core.ptr.add(p, 1usize), 3.5f64)
        t.emit(t.bits_d(t.load_d(p)))
        t.emit(t.bits_d(core.ptr.read(core.ptr.add(p, 1usize))))
        t.free(p)
        t.ffi_done()
    }
    Ok(())
end t.main
"#;

#[cfg(unix)]
#[test]
fn ffi_interpreter_float_calls_match_a_c_driver() {
    let t = Tmp::new("ffi_float", FFI_FLOAT_SRC);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let dir = &t.0;
    fs::write(dir.join("ffi.c"), FFI_FLOAT_C).unwrap();
    fs::write(dir.join("drive.c"), FFI_FLOAT_DRIVER_C).unwrap();
    // 同じ C の関数を呼ぶドライバをビルドして、参照値を得る。
    let o = cc(&["-o", "drive", "drive.c", "ffi.c"], dir);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let want = text(&Command::new(dir.join("drive")).output().unwrap().stdout);
    assert!(want.ends_with("done\n"), "{}", want);
    // 同じ C を共有ライブラリにして、インタプリタの extern から呼ぶ。
    let lib = dir.join("libffitest.so");
    let o = cc(&["-shared", "-fPIC", "-o", lib.to_str().unwrap(), "ffi.c"], dir);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let o = Command::new(exe()).args(["run"]).current_dir(dir).env("LD_PRELOAD", &lib).output().unwrap();
    assert_eq!(text(&o.stdout), want, "{}", text(&o.stderr));
}

#[test]
fn ffi_interpreter_extern_calls_and_raw_pointers_work() {
    let t = Tmp::new("ffi_interp_raw", FFI_SRC);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let o = run(&["run"], &t.0);
    assert_eq!(text(&o.stdout), "ffi ok\n", "{}", text(&o.stderr));
}

#[test]
fn ffi_interpreter_returns_floats_from_libc_math() {
    let src = r#"extern t.sqrt(x: f64) -> f64 link "sqrt"

extern t.sqrtf(x: f32) -> f32 link "sqrtf"

extern t.fabsf(x: f32) -> f32 link "fabsf"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe "libc" "square root" {
        t.sqrt(9.0f64)
    }
    core.check.eq(a.to_bits(), 3.0f64.to_bits())
    let b = unsafe "libc" "square root of a float" {
        t.sqrtf(2.25f32)
    }
    core.check.eq(b.to_bits(), 1.5f32.to_bits())
    let c = unsafe "libc" "absolute value of a float" {
        t.fabsf(0.0f32 - 2.5f32)
    }
    core.check.eq(c.to_bits(), 2.5f32.to_bits())
    core.mem.drop(std.io.print_line("math ok"))
    Ok(())
end t.main
"#;
    let t = Tmp::new("ffi_math", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let o = run(&["run"], &t.0);
    assert_eq!(text(&o.stdout), "math ok\n", "{}", text(&o.stderr));
}

/// `Box<dyn Tr>` が `String` を持つ具象型を入れたまま捨てられても、漏れず・二重に解放されない。
/// (1) `malloc` / `realloc` / `free` を数える `LD_PRELOAD` の小さな共有ライブラリで、確保と解放が同数であること、
/// (2) 300 万回作って捨てるプログラムを、仮想メモリを 64 MiB に絞って最後まで走らせること
/// (漏れれば 1 回 100 バイト近く × 300 万回で枯渇する。二重解放なら glibc が abort する)。
#[test]
fn native_box_dyn_frees_every_allocation_exactly_once() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let cc = ["gcc", "cc", "clang"].iter().find(|c| Command::new(c).arg("--version").output().is_ok());
    let Some(cc) = cc else { return };
    let dir = std::env::temp_dir().join(format!("ksn_malloc_count_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let c_path = dir.join("malloc_count.c");
    let so_path = dir.join("malloc_count.so");
    fs::write(&c_path, include_str!("data/malloc_count.c")).unwrap();
    let r = Command::new(cc).args(["-shared", "-fPIC", "-O1", "-o"]).arg(&so_path).arg(&c_path).arg("-ldl").output().unwrap();
    assert!(r.status.success(), "{}", text(&r.stderr));
    for release in [false, true] {
        // 起動と終了の確保 (標準入出力・ロケールなど。解放しない) は一定なので、「確保 - 解放」の絶対値ではなく、
        // 周回数を 10 倍にしても変わらないこと (= 1 周ごとの漏れ・二重解放がない) を見る。
        let mut balance = vec![];
        for n in [300u32, 3000u32] {
            let (_t, exe_path, out) = build_native(&format!("box_dyn_cnt_{}_{}", release, n), &box_dyn_source(n.into()), release);
            assert_native_or_reasoned_skip(&out);
            let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
            let o = Command::new(&p).env("LD_PRELOAD", &so_path).output().unwrap();
            assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
            let err = text(&o.stderr);
            let line = err.lines().find(|l| l.starts_with("allocs=")).unwrap_or_else(|| panic!("no counter output: {}", err));
            let nums: Vec<i64> = line.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).map(|s| s.parse().unwrap()).collect();
            // 1 回ごとに `String` と `Box` の 2 回確保する。
            assert!(nums[0] >= 2 * i64::from(n), "the loop allocated too little ({})", line);
            balance.push(nums[0] - nums[1]);
        }
        assert_eq!(balance[0], balance[1], "allocations minus frees grew with the number of iterations ({:?}): leak or double free", balance);
    }
    let (_t, exe_path, out) = build_native_strict("box_dyn_limit", &box_dyn_source(3_000_000), true);
    // 仮想メモリを絞った実行はネイティブだけで意味がある (バンドルはインタプリタ本体を載せるので 64 MiB に収まらない)。
    let Some(p) = exe_path else {
        assert_native_or_reasoned_skip(&out);
        let _ = fs::remove_dir_all(&dir);
        return;
    };
    let o = Command::new("sh").args(["-c", "ulimit -v 65536 && exec \"$0\""]).arg(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), "box dyn ok\n");
    let _ = fs::remove_dir_all(&dir);
}

/// スカラーの `Box`。以前は Cranelift が理由を付けて退いていた形。LLVM のネイティブ (下ろせなければ理由付きでバンドル) で同じ出力になる。
#[test]
fn native_box_of_a_scalar_runs_in_the_native_build() {
    if cfg!(windows) {
        return;
    }
    let src = [
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    match alloc.boxed.Box.new(std.alloc.SYSTEM, 4i64) {",
        "        Ok(b) => core.check.eq(*b.as_ref(), 4i64)",
        "        Err(_e) => core.panic.panic(\"oom\")",
        "    }",
        "    core.mem.drop(std.io.print_line(\"scalar box ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join("\n");
    let t = Tmp::new("box_scalar", &src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    let (bt, exe_path, out) = build_native("box_scalar_native", &src, false);
    assert_native_or_reasoned_skip(&out);
    let o = Command::new(exe_path.unwrap()).current_dir(&bt.0).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), text(&interp.stdout));
}

/// `ffi_native` のうち、ネイティブ (LLVM) が作れなければ (`link "lib:name"` の動的ロードや、関数ポインタ・構造体ポインタの
/// 引数など LLVM が未対応の形)、理由付きの退避 (バンドル) を確かめて、バンドルの実行結果を返す。
/// 返す真偽は「ネイティブの実行ファイルで走らせたか」。
#[cfg(unix)]
fn ffi_native_or_bundle(t: &Tmp, name: &str) -> (String, Output, bool) {
    let (out, n) = ffi_native(t, name);
    if let Some(n) = n {
        return (out, n, true);
    }
    assert!(out.contains("native backend skipped: ") && out.contains("the interpreter bundle is used"), "{}", out);
    assert_native_or_reasoned_skip(&out);
    let n = Command::new(t.0.join("target").join("debug").join("t")).current_dir(&t.0).output().unwrap();
    (out, n, false)
}

/// インタプリタとネイティブで、同じ出力・同じ終了コードになる (どちらも成功)。
#[cfg(unix)]
fn ffi_both_ok(name: &str, data: &str, want: &str) {
    let t = ffi_pkg(name, data);
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}{}", text(&c.stdout), text(&c.stderr));
    let i = run(&["run"], &t.0);
    assert_eq!(i.status.code(), Some(0), "interpreter: {}", text(&i.stderr));
    assert_eq!(text(&i.stdout), want);
    let (_out, n, _native) = ffi_native_or_bundle(&t, name);
    assert_eq!(n.status.code(), Some(0), "native: {}", text(&n.stderr));
    assert_eq!(text(&n.stdout), want);
}

#[cfg(unix)]
#[test]
fn ffi_structs_by_value_by_pointer_as_returns_and_nested_match_between_backends() {
    // INTEGER (Small) / SSE (Vec2, Vec2d, F3) / 混在 (Mixed, Nested, Padded) / MEMORY (Big, 24 バイト)、
    // 返り値・ポインタ渡し (`*mut Small`)・入れ子 (`Nested`)。動的ロード (`link "lib:name"`) で呼ぶ。
    ffi_both_ok("ffi_structs", "structs.ksnl", "struct ok\n");
}

#[cfg(unix)]
#[test]
fn ffi_many_arguments_callbacks_and_user_data_match_between_backends() {
    // 整数 6 本・浮動小数 8 本を超える 18 引数、レジスタが足りなくなる位置の構造体、
    // ユーザーデータのポインタを通したコールバックの累積、`f64` / `f32` / `i32` のコールバック、クロージャのコールバック。
    ffi_both_ok("ffi_many", "many.ksnl", "many ok\n");
}

#[cfg(unix)]
#[test]
fn ffi_qsort_sorts_a_kasane_slice_with_a_kasane_comparator_in_both_backends() {
    ffi_both_ok("ffi_qsort", "qsort.ksnl", "qsort ok\n");
}

#[cfg(unix)]
#[test]
fn ffi_a_missing_library_or_symbol_is_a_defined_panic_with_exit_code_101() {
    for (name, data, needle) in [
        ("ffi_nolib", "missing_lib.ksnl", "cannot load the shared library"),
        ("ffi_nosym", "missing_symbol.ksnl", "was not found in"),
    ] {
        let t = ffi_pkg(name, data);
        let i = run(&["run"], &t.0);
        assert_eq!(i.status.code(), Some(101), "interpreter: {}", text(&i.stderr));
        assert!(text(&i.stderr).contains(needle), "{}", text(&i.stderr));
        assert!(text(&i.stdout).contains("before"));
        // LLVM は `link "lib:name"` の動的ロードを持たない (リンク時に未定義の参照で失敗する) ので、理由付きでバンドルへ退避する。
        let (_out, n, _native) = ffi_native_or_bundle(&t, name);
        assert_eq!(n.status.code(), Some(101), "native: {}", text(&n.stderr));
        assert!(text(&n.stderr).contains(needle), "{}", text(&n.stderr));
        assert!(text(&n.stdout).contains("before"));
    }
}

#[cfg(unix)]
#[test]
fn ffi_fixed_arrays_in_layout_c_structs_work_in_the_interpreter_and_skip_natively_with_a_reason() {
    let t = ffi_pkg("ffi_arrays", "arrays.ksnl");
    let i = run(&["run"], &t.0);
    assert_eq!(i.status.code(), Some(0), "{}", text(&i.stderr));
    assert_eq!(text(&i.stdout), "arrays ok\n");
    let (out, n) = ffi_native(&t, "ffi_arrays");
    assert!(n.is_none(), "native unexpectedly built: {}", out);
    assert!(out.contains("native backend skipped") && out.contains("KIR に下ろせない") && out.contains("t.Arr3"), "{}", out);
}

#[cfg(unix)]
#[test]
fn ffi_a_callback_that_overflows_the_stack_stops_the_process_without_unwinding_through_c() {
    // 5.2: スタックオーバーフローは panic ではなく異常終了 (インタプリタは 102 と `stack overflow`)。
    // C のフレームを越えて巻き戻さない。
    let t = ffi_pkg("ffi_overflow", "stack_overflow.ksnl");
    let i = run(&["run"], &t.0);
    assert_eq!(i.status.code(), Some(102), "{}", text(&i.stderr));
    assert!(text(&i.stderr).contains("stack overflow"), "{}", text(&i.stderr));
    assert!(text(&i.stdout).contains("start") && !text(&i.stdout).contains("unreachable"));
    // ネイティブも同じ。LLVM は動的ロードを持たないので理由付きでバンドルへ退避し、バンドル (インタプリタ) が同じ終了を返す。
    let (_out, n, _native) = ffi_native_or_bundle(&t, "ffi_overflow");
    assert_eq!(n.status.code(), Some(102), "{}", text(&n.stderr));
    assert_eq!(text(&n.stderr), "stack overflow\n");
    assert_eq!(text(&n.stdout), text(&i.stdout));
}

#[cfg(unix)]
#[test]
fn ffi_a_real_null_dereference_still_crashes_natively_instead_of_reporting_a_stack_overflow() {
    use std::os::unix::process::ExitStatusExt;
    let t = ffi_pkg("ffi_null", "null_deref.ksnl");
    let (out, n) = ffi_native(&t, "ffi_null");
    let n = n.unwrap_or_else(|| panic!("no native executable: {}", out));
    // スタック溢れ以外の fault は既定の動作のまま: SIGSEGV で落ちる (終了コード 102 でも 101 でもない)。
    // LLVM では null の読み出しが未定義動作として消えないよう、関数に `null_pointer_is_valid` を付けている。
    assert_eq!(n.status.signal(), Some(11), "code={:?} stderr={}", n.status.code(), text(&n.stderr));
    assert!(!text(&n.stderr).contains("stack overflow"), "{}", text(&n.stderr));
    assert_eq!(text(&n.stdout), "start\n");
}

fn ffi_check_codes(name: &str, src: &str, modules: &str) -> Vec<String> {
    let t = Tmp::new(name, src);
    if !modules.is_empty() {
        fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"{}\"]\n", TOML, modules)).unwrap();
    }
    let o = run(&["check", "--json"], &t.0);
    let mut c = codes(&text(&o.stdout));
    c.dedup();
    c
}

#[test]
fn ffi_boundary_diagnostics_report_exactly_their_own_code() {
    let main = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    // E5004: 関数型 (C のコールバック) が `uses [] panics never` でない
    let e = format!("extern t.each(f: fn(i32) -> i32 uses [io.console] panics may) -> () link \"each\"\n\n{}", main);
    assert_eq!(ffi_check_codes("ffi_e5004", &e, "t"), ["E5004"]);
    // E5005: 渡した関数が `panics may`
    let f = "extern t.each(f: fn(i32) -> i32 uses [] panics never) -> () link \"each\"\n\n\
             fn t.twice(x: i32) -> i32 uses [] panics may\n    x + 1i32\nend t.twice\n\n\
             fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    unsafe \"ffi\" \"callback\" {\n        t.each(t.twice)\n    }\n    Ok(())\nend t.main\n";
    assert_eq!(ffi_check_codes("ffi_e5005", f, "t"), ["E5005"]);
    // E5005: panic するクロージャ
    let g = "extern t.each(f: fn(i32) -> i32 uses [] panics never) -> () link \"each\"\n\n\
             fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    unsafe \"ffi\" \"callback\" {\n        t.each(fn(x: i32) -> i32 capture [] {\n            core.panic.panic(\"boom\")\n        })\n    }\n    Ok(())\nend t.main\n";
    assert!(ffi_check_codes("ffi_e5005c", g, "t").contains(&"E5005".to_string()));
    // E5006: `layout c` のフィールドが C の型でない
    let h = format!("type t.Rec layout c\n    name: alloc.string.String<std.alloc.System>\nend t.Rec\n\n{}", main);
    assert_eq!(ffi_check_codes("ffi_e5006", &h, "t"), ["E5006"]);
    // E5007: `layout c` でない構造体を `extern` に渡す
    let i = format!("type t.Pair\n    a: i32\n    b: i32\nend t.Pair\n\nextern t.take(p: t.Pair) -> () link \"take\"\n\n{}", main);
    assert_eq!(ffi_check_codes("ffi_e5007", &i, "t"), ["E5007"]);
    // 正しい形は通る (`layout c` の構造体と効果なしのコールバック)
    let ok = format!(
        "type t.Pair layout c derive [Clone, Copy]\n    a: i32\n    b: i32\nend t.Pair\n\n\
         extern t.take(p: t.Pair, f: fn(i32) -> i32 uses [] panics never) -> () link \"take\"\n\n{}",
        main
    );
    assert!(ffi_check_codes("ffi_ok", &ok, "t").is_empty());
}

#[test]
fn ffi_unsafe_permission_errors_are_still_enforced_with_callbacks_and_structs() {
    // E5003: `extern` の呼び出しは `unsafe` ブロックの中だけ (コールバックを渡す呼び出しでも)
    let a = "extern t.each(f: fn(i32) -> i32 uses [] panics never) -> () link \"each\"\n\n\
             fn t.id(x: i32) -> i32 uses [] panics never\n    x\nend t.id\n\n\
             fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    t.each(t.id)\n    Ok(())\nend t.main\n";
    assert_eq!(ffi_check_codes("ffi_e5003", a, "t"), ["E5003"]);
    // E5001: `[unsafe] modules` にないモジュールでは `unsafe` ブロックが使えない
    let b = a.replace("    t.each(t.id)\n", "    unsafe \"ffi\" \"callback\" {\n        t.each(t.id)\n    }\n");
    assert_eq!(ffi_check_codes("ffi_e5001", &b, ""), ["E5001"]);
    assert!(ffi_check_codes("ffi_permitted", &b, "t").is_empty());
}

#[cfg(unix)]
#[test]
fn launcher_works_via_symlink() {
    // 起動スクリプトがシンボリックリンク経由だと `cd: can't cd` に
    // なっていた (#C)。自分の実体をシンボリックリンクを辿って求める。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"ok\")\nend t.main\n";
    let t = Tmp::new("symlink_launcher", src);
    assert_eq!(
        run(&["build"], &t.0).status.code(),
        Some(0),
        "{}",
        text(&run(&["build"], &t.0).stderr)
    );
    let launcher = t.0.join("target").join("debug").join("t");
    assert!(launcher.is_file());
    let link = std::env::temp_dir().join(format!("ksn_link_{}", std::process::id()));
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(&launcher, &link).unwrap();
    let out = Command::new(&link).current_dir(&t.0).output().expect("spawn launcher via symlink");
    let _ = fs::remove_file(&link);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("ok"), "{}", text(&out.stdout));
}

// ---------------------------------------------------------------- インタプリタの extern (f32/f64・ポインタ read/write/add)

/// 補助 C 側。共有ライブラリにして、`LD_PRELOAD` で `ksn run` に差し込む (インタプリタは dlsym で見つける)。
const FFI_C_SRC: &str = r#"
#include <stdint.h>
#include <stdlib.h>

double k_scale(double x, int32_t n, double y) { return x * n + y; }
float k_half(float x) { return x * 0.5f; }
double k_mix(int32_t a, float b, double c, int64_t d) { return a + b + c + d; }
float k_mix_f32(float a, int32_t b, float c, double d) { return a + b + c + (float)d; }
double *k_alloc_f64(size_t n) { return calloc(n, sizeof(double)); }
float *k_alloc_f32(size_t n) { return calloc(n, sizeof(float)); }
int32_t *k_alloc_i32(size_t n) { return calloc(n, sizeof(int32_t)); }
double k_sum_f64(const double *p, size_t n) {
    double s = 0;
    for (size_t i = 0; i < n; i++) s += p[i];
    return s;
}
void k_fill_f32(float *p, size_t n, float v) {
    for (size_t i = 0; i < n; i++) p[i] = v + (float)i;
}
int32_t k_sum_i32(const int32_t *p, size_t n) {
    int32_t s = 0;
    for (size_t i = 0; i < n; i++) s += p[i];
    return s;
}
"#;

fn c_compiler() -> Option<&'static str> {
    ["cc", "gcc", "clang"].into_iter().find(|c| Command::new(c).arg("--version").output().map(|o| o.status.success()).unwrap_or(false))
}

/// `src` を共有ライブラリ `libksn_helpers.so` にして、そのパスを返す。
fn build_c_helpers(dir: &Path, src: &str) -> PathBuf {
    let cc = c_compiler().expect("a C compiler (cc / gcc / clang) is required for the FFI tests");
    let c = dir.join("helpers.c");
    fs::write(&c, src).unwrap();
    let so = dir.join("libksn_helpers.so");
    let o = Command::new(cc).args(["-shared", "-fPIC", "-O1", "-o"]).arg(&so).arg(&c).output().unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    so
}

/// `ksn run` を、補助 C ライブラリを `LD_PRELOAD` で差し込んだ環境で実行する。
fn run_with_c_helpers(dir: &Path, so: &Path) -> Output {
    Command::new(exe()).arg("run").current_dir(dir).env("LD_PRELOAD", so).output().unwrap()
}

const FFI_FLOAT_SRC_M: &str = r#"extern t.k_scale(x: f64, n: i32, y: f64) -> f64 link "k_scale"

extern t.k_half(x: f32) -> f32 link "k_half"

extern t.k_mix(a: i32, b: f32, c: f64, d: i64) -> f64 link "k_mix"

extern t.k_mix_f32(a: f32, b: i32, c: f32, d: f64) -> f32 link "k_mix_f32"

extern t.k_alloc_f64(n: usize) -> *mut f64 link "k_alloc_f64"

extern t.k_alloc_f32(n: usize) -> *mut f32 link "k_alloc_f32"

extern t.k_sum_f64(values: *const f64, n: usize) -> f64 link "k_sum_f64"

extern t.k_fill_f32(values: *mut f32, n: usize, v: f32) -> () link "k_fill_f32"

extern t.free_f64(pointer: *mut f64) -> () link "free"

extern t.free_f32(pointer: *mut f32) -> () link "free"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe "libc" "f64 argument and result" {
        t.k_scale(1.5f64, 2i32, 0.25f64)
    }
    core.check.eq(a, 3.25f64)
    let h = unsafe "libc" "f32 argument and result" {
        t.k_half(3.0f32)
    }
    core.check.eq(h, 1.5f32)
    let m = unsafe "libc" "mixed integer and float arguments" {
        t.k_mix(1i32, 0.5f32, 0.25f64, 1000i64)
    }
    core.check.eq(m, 1001.75f64)
    let mf = unsafe "libc" "mixed f32 and f64 arguments" {
        t.k_mix_f32(1.0f32, 2i32, 0.5f32, 0.25f64)
    }
    core.check.eq(mf, 3.75f32)
    let p = unsafe "libc" "allocate f64 values" {
        t.k_alloc_f64(3usize)
    }
    unsafe "libc" "write, add and read f64 values" {
        core.ptr.write(p, 1.5f64)
        core.ptr.write(core.ptr.add(p, 1usize), 2.25f64)
        core.ptr.write(core.ptr.add(p, 2usize), 0.25f64)
        core.check.eq(core.ptr.read(core.ptr.add(p, 1usize)), 2.25f64)
        core.check.eq(t.k_sum_f64(core.ptr.as_const(p), 3usize), 4.0f64)
    }
    let q = unsafe "libc" "allocate f32 values" {
        t.k_alloc_f32(4usize)
    }
    unsafe "libc" "C writes f32 values, Kasane reads them" {
        t.k_fill_f32(q, 4usize, 0.5f32)
        core.check.eq(core.ptr.read(core.ptr.add(q, 3usize)), 3.5f32)
    }
    unsafe "libc" "free the buffers" {
        t.free_f64(p)
        t.free_f32(q)
    }
    core.mem.drop(std.io.print_line("ffi float ok"))
    Ok(())
end t.main
"#;

#[test]
#[cfg(target_os = "linux")]
fn ffi_interpreter_passes_and_returns_floats_and_reads_writes_pointers() {
    let t = Tmp::new("ffi_float_m", FFI_FLOAT_SRC_M);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_C_SRC);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "ffi float ok\n");
}

#[test]
#[cfg(target_os = "linux")]
fn ffi_interpreter_float_mismatch_fails_at_runtime() {
    let src = r#"extern t.k_half(x: f32) -> f32 link "k_half"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let h = unsafe "libc" "f32 result" {
        t.k_half(3.0f32)
    }
    core.check.eq(h, 2.0f32)
    Ok(())
end t.main
"#;
    let t = Tmp::new("ffi_float_bad", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_C_SRC);
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(101), "{}{}", text(&r.stdout), text(&r.stderr));
}

#[test]
#[cfg(target_os = "linux")]
fn ffi_interpreter_writes_and_reads_i32_pointers_across_c() {
    let src = r#"extern t.k_sum_i32(values: *const i32, n: usize) -> i32 link "k_sum_i32"

extern t.k_alloc_i32(n: usize) -> *mut i32 link "k_alloc_i32"

extern t.free_i32(pointer: *mut i32) -> () link "free"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let p = unsafe "libc" "allocate i32 values" {
        t.k_alloc_i32(3usize)
    }
    unsafe "libc" "write i32 values through the pointer" {
        core.ptr.write(p, 0i32 - 5i32)
        core.ptr.write(core.ptr.add(p, 1usize), 7i32)
        core.ptr.write(core.ptr.add(p, 2usize), 100i32)
        core.check.eq(core.ptr.read(p), 0i32 - 5i32)
        core.check.eq(t.k_sum_i32(core.ptr.as_const(p), 3usize), 102i32)
        t.free_i32(p)
    }
    core.mem.drop(std.io.print_line("i32 ok"))
    Ok(())
end t.main
"#;
    let t = Tmp::new("ffi_i32_ptr", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_C_SRC);
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "i32 ok\n");
}

// ---- rt/interp-ptr-more: null_mut::<T>() / cast::<T, U>() / from_mut / from_ref (インタプリタ) ----

/// `unsafe` を許す `t` で `src` を実行する (`ksn run`)。
fn run_unsafe_pkg(name: &str, src: &str) -> (Tmp, Output) {
    let t = Tmp::new(name, src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let r = run(&["run"], &t.0);
    (t, r)
}

#[test]
fn ptr_more_place_pointers_alias_their_places_and_null_is_detected() {
    // from_mut / from_ref は場所を指す。書き込みは場所に届き、from_ref は後の書き込みも見る。null_mut::<T>() は is_null で null と分かる。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut x = 41i32
    let n: *mut i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.null_mut::<i32>()
    }
    let p: *mut i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.from_mut(&mut x)
    }
    let q: *const i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.from_ref(&x)
    }
    unsafe "libc" "place pointers" {
        core.check.eq(core.ptr.is_null(n), true)
        core.check.eq(core.ptr.is_null(p), false)
        core.ptr.write(p, 42i32)
        core.check.eq(core.ptr.read(p), 42i32)
        core.check.eq(core.ptr.read(core.ptr.as_const(p)), 42i32)
        core.check.eq(core.ptr.read(q), 42i32)
        core.check.eq(core.ptr.read(core.ptr.add(p, 0usize)), 42i32)
        core.check.eq(core.ptr.read(core.ptr.cast::<i32, i32>(p)), 42i32)
    }
    core.mem.drop(std.io.print_line("place ok"))
    core.check.eq(x, 42i32)
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ptr_more_place", src);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "place ok\n");
}

#[test]
fn ptr_more_null_read_panics_with_null_dereference() {
    // 要素型が分かる null でも、読むと null 参照として 101 で止まる。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [] panics may
    unsafe "libc" "read null" {
        core.check.eq(core.ptr.read(core.ptr.null_mut::<i32>()), 0i32)
    }
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ptr_more_null_read", src);
    assert_eq!(r.status.code(), Some(101), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stderr).contains("null pointer dereference"), "{}", text(&r.stderr));
}

#[test]
fn ptr_more_cast_of_place_pointer_reinterprets_the_pinned_bytes() {
    // `from_mut` の結果は C に見せる写し (ピン) を指すので、幅の違う型への `cast` は、そのバイト列の読み替えになる
    // (リトルエンディアン: i32 の 1 は先頭の u8 が 1)。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut x = 1i32
    unsafe "libc" "cast place" {
        let b: *mut u8 = core.ptr.cast::<i32, u8>(core.ptr.from_mut(&mut x))
        core.check.eq(core.ptr.read(b), 1u8)
    }
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ptr_more_cast_place", src);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
}

#[test]
fn ptr_more_cast_without_type_argument_panics() {
    // 戻り値の型注釈からは要素型を推論しない (インタプリタは型引数しか見ない)。誤った要素型で黙って読まないよう 101 で止まる。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut x = 1i32
    unsafe "libc" "cast without targs" {
        let b: *mut u8 = core.ptr.cast(core.ptr.from_mut(&mut x))
        core.check.eq(core.ptr.read(b), 1u8)
    }
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ptr_more_cast_notarg", src);
    assert_eq!(r.status.code(), Some(101), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stderr).contains("explicit type argument"), "{}", text(&r.stderr));
}

#[test]
fn ptr_more_access_past_a_pinned_value_panics() {
    // ピン (`from_mut`) は値の幅の写しを C に見せるが、バッファには余白がある。値の幅を越えて読む・書くと、
    // 余白の不定の値を読んだり別の値を壊したりするので、幅が値の範囲に収まらない番地は 101 で止まる。
    let cases = [
        // 4 バイトの値を 8 バイトの型で読む (番地は値の先頭)。
        ("ptr_more_wide_read", "let w: *mut u64 = core.ptr.cast::<i32, u64>(core.ptr.from_mut(&mut x))\n        core.check.eq(core.ptr.read(w), 1u64)"),
        // 4 バイトの値を 8 バイトの型で書く。
        ("ptr_more_wide_write", "let w: *mut u64 = core.ptr.cast::<i32, u64>(core.ptr.from_mut(&mut x))\n        core.ptr.write(w, 5u64)"),
        // `add` で値の末尾ちょうどへ進んで読む (値の外)。
        ("ptr_more_add_past_end", "let p: *mut i32 = core.ptr.from_mut(&mut x)\n        let q: *mut i32 = core.ptr.add(p, 1usize)\n        core.check.eq(core.ptr.read(q), 0i32)"),
    ];
    for (name, body) in cases {
        let src = format!(
            "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    let mut x = 1i32\n    unsafe \"libc\" \"past end\" {{\n        {}\n    }}\n    core.mem.drop(std.io.print_line(\"after\"))\n    Ok(())\nend t.main\n",
            body
        );
        let (_t, r) = run_unsafe_pkg(name, &src);
        assert_eq!(r.status.code(), Some(101), "{}: {}{}", name, text(&r.stdout), text(&r.stderr));
        assert!(text(&r.stderr).contains("runs past the 4-byte value passed to C"), "{}: {}", name, text(&r.stderr));
        assert!(!text(&r.stdout).contains("after"), "{}", name);
    }
    // 値の幅に収まる読み書き (`cast` で 1 バイトだけ読む) は今までどおり通る。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut x = 1i32
    unsafe \"libc\" \"fits\" {
        let b: *mut u8 = core.ptr.cast::<i32, u8>(core.ptr.from_mut(&mut x))
        core.check.eq(core.ptr.read(b), 1u8)
        core.ptr.write(b, 7u8)
    }
    core.check.eq(x, 7i32)
    Ok(())
end t.main
";
    let (_t, r) = run_unsafe_pkg("ptr_more_fits_in_value", src);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
}

#[test]
#[cfg(target_os = "linux")]
fn ptr_more_cast_reinterprets_c_memory_and_null_goes_to_c() {
    // C が確保した i32 の領域を `cast::<i32, u8>` で読み替えると、リトルエンディアンのバイト列になる。
    // `null_mut::<f64>()` は C へ 0 として渡る (C 側は n = 0 なので読まない)。
    let src = r#"extern t.k_sum_f64(values: *const f64, n: usize) -> f64 link "k_sum_f64"

extern t.k_alloc_i32(n: usize) -> *mut i32 link "k_alloc_i32"

extern t.free_i32(pointer: *mut i32) -> () link "free"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let p = unsafe "libc" "allocate" {
        t.k_alloc_i32(1usize)
    }
    unsafe "libc" "cast and read bytes" {
        core.ptr.write(p, 67305985i32)
        let b: *mut u8 = core.ptr.cast::<i32, u8>(p)
        core.check.eq(core.ptr.read(b), 1u8)
        core.check.eq(core.ptr.read(core.ptr.add(b, 1usize)), 2u8)
        core.check.eq(core.ptr.read(core.ptr.add(b, 2usize)), 3u8)
        core.check.eq(core.ptr.read(core.ptr.add(b, 3usize)), 4u8)
        core.check.eq(core.ptr.read(core.ptr.cast::<u8, i32>(b)), 67305985i32)
        t.free_i32(p)
    }
    unsafe "libc" "null to C" {
        core.check.eq(t.k_sum_f64(core.ptr.null_mut::<f64>(), 0usize), 0.0f64)
    }
    core.mem.drop(std.io.print_line("cast ok"))
    Ok(())
end t.main
"#;
    let t = Tmp::new("ptr_more_c_cast", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_C_SRC);
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "cast ok\n");
}

#[test]
#[cfg(target_os = "linux")]
fn ptr_more_place_pointer_is_passed_to_c_as_a_pinned_copy() {
    // `from_mut` の結果 (C に見せる写しを指すポインタ) は、そのまま extern に渡せる (3.8: 呼び出しの前後で元の値と同期する)。
    let src = r#"extern t.k_sum_i32(values: *const i32, n: usize) -> i32 link "k_sum_i32"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut x = 5i32
    unsafe "libc" "place to C" {
        core.check.eq(t.k_sum_i32(core.ptr.as_const(core.ptr.from_mut(&mut x)), 1usize), 5i32)
    }
    Ok(())
end t.main
"#;
    let t = Tmp::new("ptr_more_place_to_c", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_C_SRC);
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
}

// ---- 修正 (fix/ffi-review): 可変長 C 関数への浮動小数の引数 / from_ref の const / extern の整数→浮動小数 ----

/// 可変長の補助 C 側。`k_va_sum` は `va_arg` で double を読む (呼び出し側が `%al` を設定しないと 0 を読む)。
const FFI_VARIADIC_SRC: &str = r#"
#include <stdarg.h>
#include <stdint.h>

const char *k_fmt_f1(void) { return "%.1f"; }
double k_va_sum(int32_t n, ...) {
    va_list ap;
    va_start(ap, n);
    double s = 0;
    for (int32_t i = 0; i < n; i++) s += va_arg(ap, double);
    va_end(ap);
    return s;
}
"#;

#[test]
#[cfg(target_os = "linux")]
fn ffi_variadic_c_functions_receive_float_arguments() {
    // `snprintf` (可変長) に f64 を渡すと、呼び出し側が `%al` を設定しないと値が 0 になる。
    let src = r#"extern t.free_u8(pointer: *mut u8) -> () link "free"

extern t.k_fmt_f1() -> *const u8 link "k_fmt_f1"

extern t.k_va_sum(n: i32, x: f64, y: f64) -> f64 link "k_va_sum"

extern t.malloc(n: usize) -> *mut u8 link "malloc"

extern t.snprintf(buf: *mut u8, n: usize, fmt: *const u8, x: f64) -> i32 link "snprintf"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let buf = unsafe "libc" "allocate the output buffer" {
        t.malloc(64usize)
    }
    let fmt = unsafe "libc" "format string from C" {
        t.k_fmt_f1()
    }
    let n = unsafe "libc" "snprintf with a float argument" {
        t.snprintf(buf, 64usize, fmt, 2.5f64)
    }
    unsafe "libc" "read the formatted text" {
        core.check.eq(n, 3i32)
        core.check.eq(core.ptr.read(buf), 50u8)
        core.check.eq(core.ptr.read(core.ptr.add(buf, 1usize)), 46u8)
        core.check.eq(core.ptr.read(core.ptr.add(buf, 2usize)), 53u8)
    }
    let s = unsafe "libc" "variadic C function reads doubles" {
        t.k_va_sum(2i32, 1.5f64, 0.25f64)
    }
    core.check.eq(s, 1.75f64)
    unsafe "libc" "free the buffer" {
        t.free_u8(buf)
    }
    core.mem.drop(std.io.print_line("variadic ok"))
    Ok(())
end t.main
"#;
    let t = Tmp::new("ffi_variadic", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_VARIADIC_SRC);
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "variadic ok\n");
}

#[test]
fn ffi_from_ref_pointer_is_const_and_write_through_it_is_rejected() {
    // `from_ref` は `*const` を返し、`core.ptr.write` は `*mut` を取る。書き込みは検査の段階で止まり、実行に届かない。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let x = 41i32
    let q: *const i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.from_ref(&x)
    }
    unsafe "libc" "write through a const pointer" {
        core.ptr.write(q, 42i32)
    }
    core.check.eq(x, 41i32)
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ffi_from_ref_const", src);
    assert_eq!(r.status.code(), Some(1), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stdout).contains("E2023") && text(&r.stdout).contains("`*const i32`"), "{}", text(&r.stdout));
}

#[test]
fn ffi_as_mut_makes_a_from_ref_pointer_writable() {
    // `as_mut` を通したものは書ける。書き込みは元の場所 (`x`) に届く。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut x = 41i32
    let q: *const i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.from_ref(&x)
    }
    let p: *mut i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.as_mut(q)
    }
    unsafe "libc" "write through as_mut" {
        core.ptr.write(p, 42i32)
    }
    core.check.eq(x, 42i32)
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ffi_as_mut_writable", src);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
}

#[test]
fn ffi_as_const_pointer_is_read_only_and_from_mut_stays_writable() {
    // `as_const` で作った `*const` は読めるが、書き込みは `*mut` を要求する検査で止まる (下のテスト)。元の `from_mut` のポインタは従来どおり書けて読める。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut x = 1i32
    let p: *mut i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.from_mut(&mut x)
    }
    let c: *const i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.as_const(p)
    }
    unsafe "libc" "write through the mutable pointer" {
        core.ptr.write(p, 2i32)
        core.check.eq(core.ptr.read(c), 2i32)
    }
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ffi_as_const_readonly", src);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
}

#[test]
fn ffi_write_through_as_const_pointer_is_rejected() {
    // `as_const` で作った `*const` への書き込みは、`*mut` を取る `core.ptr.write` の検査で E2023 になる。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let mut x = 1i32
    let p: *mut i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.from_mut(&mut x)
    }
    let c: *const i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.as_const(p)
    }
    unsafe "libc" "write through the const pointer" {
        core.ptr.write(c, 3i32)
    }
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ffi_as_const_write_rejected", src);
    assert_eq!(r.status.code(), Some(1), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stdout).contains("E2023") && text(&r.stdout).contains("`*mut i32`"), "{}", text(&r.stdout));
}

#[test]
#[cfg(target_os = "linux")]
fn ffi_integer_argument_for_float_parameter_is_e2023() {
    // 整数の値を f32 / f64 の引数に渡すのは型が合わないので、黙って変換せず E2023 で止まる。
    let src = r#"extern t.k_half(x: f32) -> f32 link "k_half"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let h = unsafe "libc" "integer for f32" {
        t.k_half(3i32)
    }
    core.check.eq(h, 1.5f32)
    Ok(())
end t.main
"#;
    let t = Tmp::new("ffi_int_for_f32", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    // 型の検査が先に止める (E2023)。黙って変換しない。
    let r = run(&["check"], &t.0);
    assert_eq!(r.status.code(), Some(1), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stdout).contains("E2023"), "{}", text(&r.stdout));
}

#[test]
#[cfg(target_os = "linux")]
fn ffi_integer_argument_for_f64_parameter_is_e2023() {
    let src = r#"extern t.k_scale(x: f64, n: i32, y: f64) -> f64 link "k_scale"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe "libc" "integer for f64" {
        t.k_scale(1i32, 2i32, 0.25f64)
    }
    core.check.eq(a, 2.25f64)
    Ok(())
end t.main
"#;
    let t = Tmp::new("ffi_int_for_f64", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    // 型の検査が先に止める (E2023)。黙って変換しない。
    let r = run(&["check"], &t.0);
    assert_eq!(r.status.code(), Some(1), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stdout).contains("E2023"), "{}", text(&r.stdout));
}

#[test]
fn float_literal_method_without_context_is_e2027() {
    // `let a = 9.0` の型が文脈から決まらないままメソッドを呼ぶ: 仕様どおり E2027。
    let bad = "fn t.root() -> f64 uses [] panics never
    let a = 9.0
    a.sqrt()
end t.root

";
    let t = Tmp::new("float_method_no_context", &main_wrap(bad, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E2027"), "{}", out);
    // 接尾辞を付ければ型が決まる。後から型が決まる場合は E2027 にしない。
    let ok = "fn t.suffixed() -> f64 uses [] panics never
    let a = 9.0f64
    a.sqrt()
end t.suffixed

fn t.later(b: f32) -> f32 uses [] panics never
    let a = 9.0
    let r = a.sqrt()
    let s: f32 = a
    b
end t.later

";
    let t = Tmp::new("float_method_context", &main_wrap(ok, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(!codes(&out).iter().any(|c| c == "E2027"), "{}", out);
}

#[test]
fn float_to_int_method_is_e1108_with_trunc_hint() {
    // 浮動小数の `to_<整数型>` は廃止された変換名。切り捨て・範囲外 None の `trunc_to_*` (Option) を案内する。
    // 型が Option に変わるので機械適用の修正は付けない。
    let bad = "fn t.g(x: f64) -> u32 uses [] panics never
    x.to_u32()
end t.g

fn t.h(x: f32) -> u8 uses [] panics never
    x.to_u8()
end t.h

";
    let t = Tmp::new("float_to_int_bad", &main_wrap(bad, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(codes(&out).iter().filter(|c| *c == "E1108").count(), 2, "{}", out);
    assert!(out.contains("x.trunc_to_u32()"), "{}", out);
    assert!(out.contains("x.trunc_to_u8()"), "{}", out);
    assert!(!out.contains("\"replace\""), "{}", out);
    // 正しい形 (`trunc_to_*` と、浮動小数どうしの `to_f64`) は E1108 にしない。
    let ok = "fn t.g(x: f64) -> Option<u32> uses [] panics never
    x.trunc_to_u32()
end t.g

fn t.h(x: f32) -> f64 uses [] panics never
    x.to_f64()
end t.h

";
    let t = Tmp::new("float_to_int_ok", &main_wrap(ok, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(!codes(&out).iter().any(|c| c == "E1108"), "{}", out);
}

// ---- check/raw-ptr-types: 生ポインタの型 (`*const T` / `*mut T`) を型検査が見る ----

/// `unsafe` を許したパッケージを `check --json` にかけ、出力文字列を返す (1 診断 1 行の JSON)。
fn raw_ptr_check(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let o = run(&["check", "--json"], &t.0);
    text(&o.stdout)
}

#[test]
fn raw_ptr_annotations_accept_the_correct_forms_without_errors() {
    // 正しい書き方: from_mut / from_ref / null_mut / as_const / as_mut / cast、`*mut` → `*const` の暗黙の縮小、読み書き。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut x = 1i32
    let p: *mut i32 = unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.from_mut(&mut x)
    }
    let c: *const i32 = unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.as_const(p)
    }
    let q: *const i32 = unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.from_ref(&x)
    }
    let m: *mut i32 = unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.as_mut(q)
    }
    let n: *mut i32 = unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.null_mut::<i32>()
    }
    let b: *mut u8 = unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.cast::<i32, u8>(p)
    }
    let r: *const u8 = b
    let u: *const i32 = p
    unsafe \"libc\" \"correct forms\" {
        core.ptr.write(m, 2i32)
        core.check.eq(core.ptr.read(c), 2i32)
        core.check.eq(core.ptr.is_null(n), true)
        core.check.eq(core.ptr.read(u), 2i32)
        core.check.eq(core.ptr.read(r), 2u8)
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_ok", src);
    assert!(!codes(&out).iter().any(|c| c == "E2020" || c == "E2023"), "{}", out);
}

#[test]
fn raw_ptr_let_annotation_rejects_a_plain_value() {
    // `let s: *mut i32 = 5i32`: 値をポインタの注釈へ入れると E2020。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let s: *mut i32 = 5i32
    unsafe \"ptr\" \"null check\" {
        core.check.eq(core.ptr.is_null(s), false)
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_val", src);
    assert!(out.contains("E2020") && out.contains("expected *mut i32, found i32"), "{}", out);
}

#[test]
fn raw_ptr_let_annotation_rejects_const_into_mut() {
    // `let r: *mut i32 = core.ptr.from_ref(&x)`: `from_ref` は `*const` を返すので、`*mut` の注釈は E2020。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let x: i32 = 1i32
    let r: *mut i32 = unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.from_ref(&x)
    }
    unsafe \"ptr\" \"null check\" {
        core.check.eq(core.ptr.is_null(r), false)
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_const_to_mut", src);
    assert!(out.contains("E2020") && out.contains("expected *mut i32, found *const i32"), "{}", out);
}

#[test]
fn raw_ptr_let_annotation_rejects_a_different_element_type() {
    // 要素型が確定して食い違う (`*const u8` と `*const i32`) のは E2020。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let x: i32 = 1i32
    let r: *const u8 = unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.from_ref(&x)
    }
    unsafe \"ptr\" \"null check\" {
        core.check.eq(core.ptr.is_null(r), false)
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_elem", src);
    assert!(out.contains("E2020") && out.contains("expected *const u8, found *const i32"), "{}", out);
}

#[test]
fn raw_ptr_pointer_is_not_a_plain_integer_annotation() {
    // ポインタを整数の注釈へ入れるのは E2020 (`core.ptr.null_mut::<i32>()` を `i32` へ)。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let n: i32 = unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.null_mut::<i32>()
    }
    core.check.eq(n, 0i32)
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_to_int", src);
    assert!(out.contains("E2020") && out.contains("expected i32, found *mut i32"), "{}", out);
}

#[test]
fn raw_ptr_unsuffixed_literal_is_not_a_pointer() {
    // 整数リテラルをポインタの注釈へ入れるのは E2020 (型は `{integer}`)。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let q: *mut i32 = 5
    unsafe \"ptr\" \"null check\" {
        core.check.eq(core.ptr.is_null(q), false)
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_literal", src);
    assert!(out.contains("E2020") && out.contains("expected *mut i32, found {integer}"), "{}", out);
}

#[test]
fn raw_ptr_const_argument_to_mut_parameter_is_rejected() {
    // 宣言が `*mut i32` の関数へ `from_ref` (const) の値を渡すと E2023。
    let src = "fn t.take(p: *mut i32) -> () uses [] panics never
    unsafe \"ptr\" \"null check\" {
        core.check.eq(core.ptr.is_null(p), false)
    }
end t.take

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let x: i32 = 1i32
    unsafe \"ptr\" \"make or convert a raw pointer\" {
        t.take(core.ptr.from_ref(&x))
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_arg", src);
    assert!(out.contains("E2023") && out.contains("expected `*mut i32`, found `*const i32`"), "{}", out);
}

#[test]
fn raw_ptr_mut_argument_to_const_parameter_is_accepted() {
    // `*mut` → `*const` の縮小は暗黙に許す (宣言が `*const` の関数へ `*mut` を渡す)。
    let src = "fn t.look(p: *const i32) -> i32 uses [] panics never
    unsafe \"ptr\" \"read or write through a raw pointer\" {
        core.ptr.read(p)
    }
end t.look

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut x = 1i32
    let p: *mut i32 = unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.from_mut(&mut x)
    }
    unsafe \"libc\" \"read through a mutable pointer\" {
        core.check.eq(t.look(p), 1i32)
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_mut_to_const", src);
    assert!(!codes(&out).iter().any(|c| c == "E2020" || c == "E2023"), "{}", out);
}

#[test]
fn raw_ptr_return_value_mismatch_is_rejected() {
    // 戻り値が `*mut` の関数で `from_ref` (const) を返すと E2020 (戻り値)。
    let src = "fn t.back(x: &i32) -> *mut i32 uses [] panics never
    unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.from_ref(x)
    }
end t.back

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let x: i32 = 1i32
    let r = t.back(&x)
    unsafe \"ptr\" \"null check\" {
        core.check.eq(core.ptr.is_null(r), false)
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_ret", src);
    assert!(out.contains("E2020") && out.contains("type mismatch in return value: expected *mut i32, found *const i32"), "{}", out);
}

#[test]
fn raw_ptr_extern_mut_parameter_rejects_a_const_pointer() {
    // extern 宣言の `*mut u8` 引数へ `*const u8` (from_ref) を渡すと E2023。
    let src = "extern t.free(pointer: *mut u8) -> () link \"free\"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let b: u8 = 1u8
    unsafe \"libc\" \"free a const pointer\" {
        t.free(core.ptr.from_ref(&b))
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_extern", src);
    assert!(out.contains("E2023") && out.contains("expected `*mut u8`, found `*const u8`"), "{}", out);
}

#[test]
fn raw_ptr_reference_to_pointer_annotation_stays_silent() {
    // 参照は透過的に扱う方針なので、`&x` を `*const` の注釈へ入れても黙る (取りこぼしを選ぶ)。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let x: i32 = 1i32
    let s: *const i32 = &x
    unsafe \"ptr\" \"null check\" {
        core.check.eq(core.ptr.is_null(s), false)
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_ref_silent", src);
    assert!(!codes(&out).iter().any(|c| c == "E2020" || c == "E2023"), "{}", out);
}

#[test]
fn ptr_add_rejects_const_argument_and_add_const_accepts_it() {
    // `add` は `*mut` を取るので `*const` (from_ref) を渡すと E2023。`add_const` なら同じ引数が通る。
    let bad = "fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let x: i32 = 1i32
    let q: *const i32 = unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.from_ref(&x)
    }
    let r = unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.add(q, 0usize)
    }
    unsafe \"ptr\" \"null check\" {
        core.check.eq(core.ptr.is_null(r), false)
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("ptr_add_mut_rejects_const", bad);
    assert!(out.contains("E2023") && out.contains("expected `*mut i32`, found `*const i32`"), "{}", out);

    let ok = "fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let x: i32 = 1i32
    let q: *const i32 = unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.from_ref(&x)
    }
    let r: *const i32 = unsafe \"ptr\" \"make or convert a raw pointer\" {
        core.ptr.add_const(q, 0usize)
    }
    unsafe \"ptr\" \"null check\" {
        core.check.eq(core.ptr.is_null(r), false)
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("ptr_add_const_ok", ok);
    assert!(!codes(&out).iter().any(|c| c == "E2020" || c == "E2023"), "{}", out);
}

// ---- extern の構造体の値渡し・値の戻り値 (x86_64 SysV の分類。16 バイト以下はレジスタ、超えるものはメモリ渡し・sret)

/// 構造体を値で渡す・返す C 側。`Eight` は INTEGER 1 つ、`Dbl2` は SSE 2 つ、`Mixed` は INTEGER + SSE、`Big3` は 24 バイト (メモリ)。
const FFI_STRUCT_C_SRC: &str = r#"
#include <stdint.h>
typedef struct { int32_t a; int32_t b; } Eight;
typedef struct { double x; double y; } Dbl2;
typedef struct { int64_t a; double b; } Mixed;
typedef struct { double x; int64_t y; } MixedRev;
typedef struct { float x; float y; } Flt2;
typedef struct { int64_t a, b, c; } Big3;
typedef struct { uint8_t a; uint8_t b; int16_t c; } Small;
Eight k_eight_add(Eight p, int32_t k) { Eight r = { p.a + k, p.b + k }; return r; }
Dbl2 k_dbl_scale(Dbl2 p, double s) { Dbl2 r = { p.x * s, p.y * s }; return r; }
Mixed k_mixed_bump(Mixed p, int64_t n) { Mixed r = { p.a + n, p.b * 2.0 }; return r; }
MixedRev k_mixed_rev_bump(MixedRev p) { MixedRev r = { p.x + 1.0, p.y - 1 }; return r; }
Flt2 k_flt_twice(Flt2 p) { Flt2 r = { p.x * 2.0f, p.y * 2.0f }; return r; }
int64_t k_big_sum(Big3 p, int64_t n) { return p.a + p.b + p.c + n; }
Big3 k_big_make(int64_t a) { Big3 r = { a, a * 2, a * 3 }; return r; }
Small k_small_bump(Small p) { Small r = { p.a + 1, p.b + 2, p.c - 3 }; return r; }
int64_t k_seven(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, int64_t f, int64_t g, Big3 p, int64_t z) {
    return a + 2*b + 3*c + 4*d + 5*e + 6*f + 7*g + 10*p.a + 100*p.b + 1000*p.c + 10000*z;
}
double k_sum_then(double a, Dbl2 p, double b) { return a + p.x * 10.0 + p.y * 100.0 + b * 1000.0; }
"#;

const FFI_STRUCT_SRC: &str = r#"type t.Big3 layout c pub
    a: i64
    b: i64
    c: i64
end t.Big3

type t.Dbl2 layout c pub
    x: f64
    y: f64
end t.Dbl2

type t.Eight layout c pub
    a: i32
    b: i32
end t.Eight

type t.Flt2 layout c pub
    x: f32
    y: f32
end t.Flt2

type t.Mixed layout c pub
    a: i64
    b: f64
end t.Mixed

type t.MixedRev layout c pub
    x: f64
    y: i64
end t.MixedRev

type t.Small layout c pub
    a: u8
    b: u8
    c: i16
end t.Small

extern t.k_big_make(a: i64) -> t.Big3 link "k_big_make"

extern t.k_big_sum(p: t.Big3, n: i64) -> i64 link "k_big_sum"

extern t.k_dbl_scale(p: t.Dbl2, s: f64) -> t.Dbl2 link "k_dbl_scale"

extern t.k_eight_add(p: t.Eight, k: i32) -> t.Eight link "k_eight_add"

extern t.k_flt_twice(p: t.Flt2) -> t.Flt2 link "k_flt_twice"

extern t.k_mixed_bump(p: t.Mixed, n: i64) -> t.Mixed link "k_mixed_bump"

extern t.k_mixed_rev_bump(p: t.MixedRev) -> t.MixedRev link "k_mixed_rev_bump"

extern t.k_seven(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, p: t.Big3, z: i64) -> i64 link "k_seven"

extern t.k_small_bump(p: t.Small) -> t.Small link "k_small_bump"

extern t.k_sum_then(a: f64, p: t.Dbl2, b: f64) -> f64 link "k_sum_then"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let e = unsafe "libc" "eight" {
        t.k_eight_add(t.Eight { a: 1i32, b: 2i32 }, 10i32)
    }
    core.check.eq(e.a, 11i32)
    core.check.eq(e.b, 12i32)
    let d = unsafe "libc" "dbl2" {
        t.k_dbl_scale(t.Dbl2 { x: 1.5f64, y: 2.0f64 }, 2.0f64)
    }
    core.check.eq(d.x, 3.0f64)
    core.check.eq(d.y, 4.0f64)
    let m = unsafe "libc" "mixed" {
        t.k_mixed_bump(t.Mixed { a: 5i64, b: 1.25f64 }, 7i64)
    }
    core.check.eq(m.a, 12i64)
    core.check.eq(m.b, 2.5f64)
    let mr = unsafe "libc" "mixed rev" {
        t.k_mixed_rev_bump(t.MixedRev { x: 0.5f64, y: 9i64 })
    }
    core.check.eq(mr.x, 1.5f64)
    core.check.eq(mr.y, 8i64)
    let f = unsafe "libc" "flt2" {
        t.k_flt_twice(t.Flt2 { x: 1.5f32, y: 0.25f32 })
    }
    core.check.eq(f.x, 3.0f32)
    core.check.eq(f.y, 0.5f32)
    let s = unsafe "libc" "big3 argument" {
        t.k_big_sum(t.Big3 { a: 1i64, b: 2i64, c: 3i64 }, 4i64)
    }
    core.check.eq(s, 10i64)
    let b = unsafe "libc" "big3 result (sret)" {
        t.k_big_make(5i64)
    }
    core.check.eq(b.a, 5i64)
    core.check.eq(b.b, 10i64)
    core.check.eq(b.c, 15i64)
    let sm = unsafe "libc" "small" {
        t.k_small_bump(t.Small { a: 1u8, b: 2u8, c: 10i16 })
    }
    core.check.eq(sm.a, 2u8)
    core.check.eq(sm.b, 4u8)
    core.check.eq(sm.c, 7i16)
    let sv = unsafe "libc" "stack arguments" {
        t.k_seven(
            1i64,
            1i64,
            1i64,
            1i64,
            1i64,
            1i64,
            1i64,
            t.Big3 { a: 1i64, b: 1i64, c: 1i64 },
            1i64,
        )
    }
    core.check.eq(sv, 11138i64)
    let st = unsafe "libc" "double arguments around a struct" {
        t.k_sum_then(1.0f64, t.Dbl2 { x: 1.0f64, y: 1.0f64 }, 1.0f64)
    }
    core.check.eq(st, 1111.0f64)
    core.mem.drop(std.io.print_line("struct ffi ok"))
    Ok(())
end t.main
"#;

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn ffi_interpreter_passes_and_returns_structs_by_value() {
    let t = Tmp::new("ffi_struct_interp", FFI_STRUCT_SRC);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_STRUCT_C_SRC);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "struct ffi ok\n");
}

#[test]
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn ffi_struct_by_value_is_refused_outside_x86_64_linux() {
    // 構造体の値渡しは x86_64 SysV だけで扱う。ほかの環境では黙って壊れず、明示のエラーにする。
    let t = Tmp::new("ffi_struct_refused", FFI_LIBC_STRUCT_SRC);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let r = run(&["run"], &t.0);
    assert_eq!(r.status.code(), Some(101), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stderr).contains("x86_64"), "{}", text(&r.stderr));
}

/// (関数, 木走査の期待値 (Kasane の式), 参照実行器の期待値 (`ksn kir --run` の表示))。
const FLOAT_SPECIAL_CASES: &[(&str, &str, &str)] = &[
    ("t.rte_nhalf64", "9223372036854775808u64", "9223372036854775808"), // round_ties_even(-0.5) は -0
    ("t.rte_half32", "1073741824u32", "1073741824"),                    // round_ties_even(2.5f32) は 2
    ("t.abs_ninf64", "9218868437227405312u64", "9218868437227405312"), // abs(-inf) は +inf
    ("t.sqrt_ninf64", "true", "true"),                                  // sqrt(-inf) は NaN
    ("t.sqrt_neg64", "true", "true"),                                   // sqrt(負) は NaN
    ("t.sqrt_nzero64", "9223372036854775808u64", "9223372036854775808"), // sqrt(-0) は -0
    ("t.sqrt_inf64", "9218868437227405312u64", "9218868437227405312"),  // sqrt(+inf) は +inf
    ("t.abs_ninf32", "2139095040u32", "2139095040"),                    // f32 の abs(-inf) は +inf
    ("t.sqrt_ninf32", "true", "true"),
    ("t.nan32_bits", "2139095041u32", "2139095041"), // f32 の signaling NaN のビットは保つ
    ("t.nan64_bits", "9218868437227405313u64", "9218868437227405313"),
];

#[test]
fn float_specials_follow_ieee_on_both_backends() {
    // 木走査 (`ksn run`) と KIR (`ksn kir --run`) の両方で、無限大・NaN・符号つきゼロの値が IEEE 754 どおりであること。
    for (i, (f, want, kwant)) in FLOAT_SPECIAL_CASES.iter().enumerate() {
        let body = format!("    core.check.eq({}(), {})\n", f, want);
        let t = Tmp::new(&format!("float_special_{}", i), &format!("{}{}", FLOAT_SPECIAL_FNS, main_wrap("", &body)));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", f, text(&o.stderr));
        let k = kir_run(&["kir", *f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *kwant, "kir value of {}", f);
    }
}

#[test]
fn min_rem_minus_one_overflows_for_every_signed_width() {
    // 案 A: 符号付きの `MIN % -1` は割り算が溢れるので余りも溢れ。`%` は panic (終了コード 101)、`checked_rem` と `checked_div` は None。
    // 木走査 (`ksn run`) と参照実行器 (`ksn kir --run`) の両方で panic の文言を確かめる。
    for w in ["i8", "i16", "i32", "i64", "i128"] {
        let src = format!(
            "fn t.rem(a: {w}, b: {w}) -> {w} uses [] panics may\n    a % b\nend t.rem\n\n\
             fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    \
             core.mem.drop(t.rem(core.num.{w}.MIN, -1{w}))\n    Ok(())\nend t.main\n",
            w = w
        );
        let t = Tmp::new(&format!("min_rem_{}", w), &src);
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(101), "{}: {}", w, text(&o.stderr));
        assert!(text(&o.stderr).contains("panic: attempt to calculate the remainder with overflow"), "{}: {}", w, text(&o.stderr));
        if w == "i8" {
            let k = kir_run(&["kir", "t.rem", "--run", "-128", "-1"], &t.0);
            assert_eq!(k.status.code(), Some(101), "kir {}: {}", w, text(&k.stderr));
            assert_eq!(text(&k.stderr).trim(), "panic: attempt to calculate the remainder with overflow", "kir {}", w);
        }
    }
    // `checked_rem` と `checked_div` は None (`test --both` で木走査と機械語を比べる。i128 は機械語が未対応なのでバンドルで比べる)。
    let mut src = String::from("fn t.main() -> () uses [] panics never\nend t.main\n\n");
    for w in ["i8", "i16", "i32", "i64", "i128"] {
        src.push_str(&format!(
            "test t.main.test_{w}\n    let m = core.num.{w}.MIN\n    core.check.eq(m.checked_rem(-1{w}), None)\n    core.check.eq(m.checked_div(-1{w}), None)\nend t.main.test_{w}\n\n",
            w = w
        ));
    }
    let src = format!("{}\n", src.trim_end());
    let t = Tmp::new("min_rem_checked", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("5 passed; 0 failed"), "{}", out);
    if clang_available() {
        for w in ["i8", "i16", "i32", "i64"] {
            assert!(out.contains(&format!("t.main.test_{} ... ok (both: llvm)", w)), "{}", out);
        }
    }
    // i128 は KIR (定数の値が扱えない) に下ろせないので、理由を出してバンドルで比べる (誤った値を出さない)。
    assert!(out.contains("t.main.test_i128 ... ok (both: bundle (llvm skipped: "), "{}", out);
}

// ---- 型が決まった接尾辞なし整数リテラル: 木走査も型表の型で計算する (レビュー #1) ----

/// 辞書順 (count, dbl, main, plus, wrapped) に並べた定義。`main` の本体だけ差し替える。
fn lit_src(main_body: &str) -> String {
    format!(
        "fn t.count(n: u32) -> u8 uses [] panics may
    let mut c = 0
    let mut i = 0u32
    while i < n {{
        c = c + 1
        i = i + 1u32
    }}
    c
end t.count

fn t.dbl(a: u8) -> u8 uses [] panics never
    let z = if a > 1u8 {{
        200
    }} else {{
        a
    }}
    z.wrapping_add(z)
end t.dbl

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
{}    Ok(())
end t.main

fn t.plus(o: Option<u8>) -> u8 uses [] panics may
    let w = o.unwrap_or(255)
    w + 1
end t.plus

fn t.wrapped(o: Option<u8>) -> u8 uses [] panics never
    let w = o.unwrap_or(255)
    w.wrapping_add(1)
end t.wrapped

test t.dbl.test_lit
    core.check.eq(t.dbl(5u8), 144u8)
end t.dbl.test_lit

test t.wrapped.test_lit
    core.check.eq(t.wrapped(None), 0u8)
end t.wrapped.test_lit
",
        main_body
    )
}

#[test]
fn untyped_int_literals_take_their_inferred_type_in_the_tree_walker() {
    // 3 形とも、木走査 (`run` / `test`)・KIR (`kir --run`)・機械語 (`test --both`) の結果が一致する。
    let t = Tmp::new("lit_types", &lit_src("    let _x = t.count(300u32)\n"));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stderr).contains("panic: attempt to add with overflow"), "{}", text(&o.stderr));
    let k = kir_run(&["kir", "t.count", "--run", "300"], &t.0);
    assert_eq!(k.status.code(), Some(101));
    assert!(text(&k.stderr).contains("panic: attempt to add with overflow"), "{}", text(&k.stderr));
    assert_eq!(text(&kir_run(&["kir", "t.count", "--run", "200"], &t.0).stdout).trim(), "200");
    assert_eq!(text(&kir_run(&["kir", "t.dbl", "--run", "5"], &t.0).stdout).trim(), "144");
    for args in [&["test"][..], &["test", "--both"][..]] {
        let o = run(args, &t.0);
        let out = text(&o.stdout);
        assert_eq!(o.status.code(), Some(0), "{:?}\n{}{}", args, out, text(&o.stderr));
        assert!(out.contains("2 passed; 0 failed"), "{:?}\n{}", args, out);
    }
    // `w + 1` (`w` は u8 の 255): 木走査も overflow で panic する。
    let t2 = Tmp::new("lit_types_plus", &lit_src("    let _x = t.plus(None)\n"));
    let o = run(&["run"], &t2.0);
    assert_eq!(o.status.code(), Some(101), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stderr).contains("panic: attempt to add with overflow"), "{}", text(&o.stderr));
}

/// f32 の `abs` は符号ビットを落とすだけ (ビットのまま)。木走査も KIR も、signaling NaN のビットを保つ。f64 へ広げると
/// quiet NaN に変わっていた (0x7f800001 → 0x7fc00001)。KIR は参照実行器と `--native` の結果も一致すること。
#[test]
fn f32_abs_keeps_signaling_nan_bits() {
    let src = r#"fn t.absbits(x: f32) -> u32 uses [] panics never
    x.abs().to_bits()
end t.absbits

fn t.main() -> () uses [] panics may
    let s = core.num.f32.from_bits(2139095041u32)
    core.check.eq(s.abs().to_bits(), 2139095041u32)
    core.check.eq((-s).abs().to_bits(), 2139095041u32)
    core.check.eq(t.absbits(s), 2139095041u32)
end t.main
"#;
    let t = Tmp::new("f32_abs_snan", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    // 0x7f800001 と 0xff800001 (負の signaling NaN) の abs は、どちらも 0x7f800001 のまま。
    for arg in ["2139095041", "4286578689"] {
        let k = kir_run(&["kir", "t.absbits", "--run", arg], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", arg, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), "2139095041", "kir abs of bits {}", arg);
    }
}

#[test]
fn llvm_build_of_print_line_main_matches_run() {
    let t = Tmp::new("llvm_build_print", IO_SAY);
    if let Some((r, n)) = llvm_build_and_run(&t) {
        assert_eq!(text(&r.stdout), "main\n");
        same_output(&r, &n);
    }
}

#[test]
fn llvm_build_owned_strings_rings_and_moves_match_run() {
    // `String.push_str` / `push(char)`、`&mut self` の構造体のフィールドの Vec (`get` / `get_mut`)、分解、フィールドからの移動、
    // `Vec<String>` を含む `main` が、`ksn run` と同じ表示・終了コードで LLVM から作れる (設計_KIR.md 13.7)。
    let body = "    core.check.eq(t.str_build(50usize), 100usize)\n    core.check.eq(t.str_chars(), 2031u32)\n    core.check.eq(t.ring_run(3usize), 528u32)\n    core.check.eq(t.pair_run(2u32), 13usize)\n    core.check.eq(t.destruct_struct(4u32), 7usize)\n    core.check.eq(t.field_move_reassign(4u32), 6usize)\n    core.check.eq(t.vs_run(5u32), 20usize)\n    core.check.eq(t.names_push(4u32), 4usize)\n    core.check.eq(t.mode_run(), 2u32)\n    core.mem.drop(std.io.print_line(\"owned ok\"))\n";
    let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
    let t = Tmp::new("llvm_build_owned", &format!("{}{}", KIR_OWN2_FNS, main));
    if let Some((r, n)) = llvm_build_and_run(&t) {
        assert_eq!(text(&r.stdout), "owned ok\n");
        same_output(&r, &n);
    }
}

#[test]
fn llvm_build_main_err_prints_debug_and_exits_1() {
    // `Err(e)` は stderr に `Error: <Debug>` と改行、終了コード 1 (木走査と同じ)。std.io.Error の Debug も同じ。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never
    std.io.print_line(\"before\")?
    let e = std.io.Error { kind: std.io.ErrorKind.BrokenPipe, code: 32i32 }
    Err(e)
end t.main
";
    let t = Tmp::new("llvm_build_io_err", src);
    if let Some((r, n)) = llvm_build_and_run(&t) {
        assert_eq!(r.status.code(), Some(1));
        assert_eq!(text(&r.stderr), "Error: Error { kind: BrokenPipe, code: 32 }\n");
        assert_eq!(text(&r.stdout), "before\n");
        same_output(&r, &n);
    }
}

#[test]
fn llvm_build_debug_covers_structs_enums_ints_bool_char_and_str() {
    // 構造体・列挙型 (ペイロード付き)・入れ子・整数 (負数・0・64 ビットの端)・bool・char (逃れと非 ASCII)・&str (逃れ) の Debug は木走査と一致する。
    let src = "enum t.Kind derive [Debug] pub
    Plain
    Pair(i32, bool)
    Named(t.Inner)
end t.Kind

type t.Inner derive [Debug] pub
    a: i8
    b: u64
    c: char
end t.Inner

type t.Bad derive [Debug] pub
    neg: i64
    zero: i32
    flag: bool
    ch: char
    text: &'static str
    inner: t.Inner
    kind: t.Kind
    k2: t.Kind
    k3: t.Kind
    small: u8
    min: i64
    quote: char
end t.Bad

fn t.mk() -> t.Bad uses [] panics never
    let inner = t.Inner { a: -128i8, b: 18446744073709551615u64, c: '\\n' }
    let kind = t.Kind.Pair(-7i32, false)
    let inner2 = t.Inner { a: 0i8, b: 0u64, c: 'é' }
    let k2 = t.Kind.Named(inner2)
    t.Bad { neg: -12345i64, zero: 0i32, flag: true, ch: 'x', text: \"a\\\"b\\\\c\\nd é\", inner: inner, kind: kind, k2: k2, k3: t.Kind.Plain, small: 255u8, min: -9223372036854775808i64, quote: '\\'' }
end t.mk

fn t.main() -> Result<(), t.Bad> uses [] panics never
    Err(t.mk())
end t.main
";
    let t = Tmp::new("llvm_build_debug", src);
    if let Some((r, n)) = llvm_build_and_run(&t) {
        assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
        let e = text(&r.stderr);
        assert!(e.starts_with("Error: Bad { neg: -12345, zero: 0, flag: true, ch: 'x',") && e.contains("kind: Pair(-7, false)") && e.contains("k3: Plain"), "{}", e);
        same_output(&r, &n);
    }
}

#[test]
fn llvm_build_panic_keeps_the_panic_exit_code_and_message() {
    // 溢れの panic は 101 と `panic: ...` (木走査と同じ)。それまでの出力は残る。
    let src = "fn t.bump(x: u8) -> u8 uses [] panics may
    x + 200u8
end t.bump

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    std.io.print_line(\"start\")?
    let y = t.bump(100u8)
    match y {
        0u8 => std.io.print_line(\"zero\")
        _ => std.io.print_line(\"unreachable\")
    }
end t.main
";
    let t = Tmp::new("llvm_build_panic", src);
    if let Some((r, n)) = llvm_build_and_run(&t) {
        assert_eq!(r.status.code(), Some(101), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), "start\n");
        same_output(&r, &n);
    }
}

#[test]
fn llvm_build_falls_back_with_a_reason_when_a_function_cannot_be_lowered() {
    // 下ろせない関数 (ここでは `dyn` のトレイトオブジェクトを引数に取る関数を呼ぶ main) は、理由を出して既定の経路へ退避する。ビルドは成功する。
    // (型引数付きの関数・メソッドは単相化して下ろせるので、退避の例に使わない。設計_KIR.md 20 章)
    let src = "trait t.Speak[E: uses, P: panics]
    fn speak(self: &Self) -> u32 uses [E] panics P
end t.Speak

type t.Dog
    n: u32
end t.Dog

impl t.Speak<[], never> for t.Dog
fn t.Dog.speak(self: &t.Dog) -> u32 uses [] panics never impls t.Speak<[], never>
    self.n
end t.Dog.speak

fn t.call(x: Option<i128>) -> u32 uses [] panics never
    match x {
        Some(_) => 1u32
        None => t.Dog { n: 3u32 }.speak()
    }
end t.call

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.call(None), 3u32)
    std.io.print_line(\"x\")
end t.main
";
    let t = Tmp::new("llvm_build_fallback", src);
    let b = run(&["build", "--backend", "llvm"], &t.0);
    let out = text(&b.stdout);
    assert_eq!(b.status.code(), Some(0), "{}{}", out, text(&b.stderr));
    assert!(out.contains("native backend skipped: t.main を KIR に下ろせない") && out.contains("the interpreter bundle is used"), "{}", out);
    assert!(out.contains("built target/debug"), "{}", out);
    // 既定は llvm なので、`--backend` なしでも同じ理由とバンドルへの退避が出る。
    let d = run(&["build"], &t.0);
    assert!(text(&d.stdout).contains("native backend skipped: t.main を KIR に下ろせない"), "{}", text(&d.stdout));
    // `--backend c` は残してある: llvm の理由は出さない。
    let c = run(&["build", "--backend", "c"], &t.0);
    assert!(!text(&c.stdout).contains("KIR に下ろせない"), "{}", text(&c.stdout));
}

#[test]
fn the_removed_cranelift_backend_is_refused_with_a_reason() {
    // `--backend clif` は削除した (設計_KIR.md 7 章 19)。何も作らず、廃止の理由と代わりの指定を出して失敗する。
    let t = Tmp::new("clif_removed", &clif_program("    core.check.eq(1u8, 1u8)"));
    let o = run(&["build", "--backend", "clif"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}{}", text(&o.stdout), text(&o.stderr));
    let err = text(&o.stderr);
    assert!(err.contains("E9040") && err.contains("--backend clif") && err.contains("removed") && err.contains("--backend llvm"), "{}", err);
    assert!(!t.0.join("target").exists(), "廃止のバックエンドでは何も作らない");
    // 知らない名前も同じく失敗する。
    let o = run(&["build", "--backend", "nope"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("unknown backend"), "{}", text(&o.stderr));
}

/// 依存パッケージ (別 module) の `kvlib.lookup.find`/`count` を KIR に下ろし、
/// 依存元の main から呼ぶ。`ksn run` / `ksn kir t.main --run` / `--native` が一致することを確かめる。
#[test]
fn dep_package_fn_is_lowered_and_matches_run_kir_native() {
    if !clang_available() {
        eprintln!("skip: clang が PATH に無いので、--native の比較は飛ばす");
        return;
    }
    let src = "import kvlib.lookup\n\nfn t.main() -> () uses [] panics never\n    let r = lookup.find(\"name = kasane\\nmode = fast\\n\", \"name\")\n    core.mem.drop(r)\n    let r2 = lookup.find(\"name = kasane\\nmode = fast\\n\", \"missing\")\n    core.mem.drop(r2)\n    let n = lookup.count(\"a = 1\\nb = 2\\n\")\n    core.mem.drop(n)\nend t.main\n";
    let t = Tmp::new("dep_lower", src);
    let kvlib = root().join("examples").join("kvlib").display().to_string();
    fs::write(
        t.0.join("kasane.toml"),
        format!("{}kvlib = {{ path = \"{}\" }}\n", TOML, kvlib),
    )
    .unwrap();
    let u = run(&["update"], &t.0);
    assert_eq!(u.status.code(), Some(0), "update: {}{}", text(&u.stdout), text(&u.stderr));
    let r = run(&["run"], &t.0);
    assert_eq!(r.status.code(), Some(0), "run: {}{}", text(&r.stdout), text(&r.stderr));
    kir_run(&["kir", "t.main", "--run"], &t.0);
}

#[test]
fn struct_value_order_is_declaration_order() {
    // 構造体の値はリテラルに書いた順ではなく型定義の宣言順で持つ (4.3)。そのため、同じ値の `==`・HashMap の鍵・Debug の表示は
    // フィールドの書き方に依らない。木走査 (`ksn run`) と参照実行器 (`ksn kir --run`) の表示も一致すること。
    let src = "type t.P derive [Clone, Copy, Debug, PartialEq, Eq, Hash]
    a: u8
    b: u8
end t.P

fn t.main() -> () uses [alloc, io.console] panics may
    let x = t.P { b: 1u8, a: 2u8 }
    let y = t.P { a: 2u8, b: 1u8 }
    core.check.eq(x == y, true)
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    core.mem.drop(m.insert(x, 1u8))
    core.mem.drop(m.insert(y, 2u8))
    core.mem.drop(std.io.print_line(if m.len() == 1usize {
        \"len 1\"
    } else {
        \"len 2\"
    }))
    core.mem.drop(std.io.print_line(if m.get(&x) == Some(&2u8) {
        \"x -> 2\"
    } else {
        \"x -> other\"
    }))
end t.main
";
    let t = Tmp::new("struct_order_hash", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout), "len 1\nx -> 2\n");

    // Debug の表示は宣言順 (リテラルの書き方に依らない)。左辺は逆順で書いた値。
    let src = "type t.P derive [Clone, Copy, Debug, PartialEq, Eq]
    a: u8
    b: u8
end t.P

fn t.main() -> () uses [] panics may
    let x = t.P { b: 1u8, a: 2u8 }
    core.check.eq(x, t.P { a: 9u8, b: 1u8 })
end t.main
";
    let t = Tmp::new("struct_order_debug", src);
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(101), "{}", text(&tw.stderr));
    assert_eq!(text(&tw.stderr).trim(), "panic: assertion failed: left: P { a: 2, b: 1 }, right: P { a: 9, b: 1 }");
    let k = kir_run(&["kir", "t.main", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(101), "{}", text(&k.stderr));
    assert_eq!(text(&k.stderr).trim(), text(&tw.stderr).trim());
}

/// Fable の最終レビュー B2: 欄 (ペイロード) に集成体を持つ型の `clone()` は、`unreachable` に下ろさず未対応にする。
#[test]
fn final_review_nested_clone_is_unsupported_not_unreachable() {
    let src = "type t.P derive [Clone, Copy] pub\n    x: u32\nend t.P\n\ntype t.Seg derive [Clone] pub\n    a: t.P\n    n: u32\nend t.Seg\n\n\
fn t.c(k: u32) -> u32 uses [] panics never\n    let g = t.Seg { a: t.P { x: k }, n: 1u32 }\n    let h = g.clone()\n    h.a.x.wrapping_add(h.n)\nend t.c\n\n\
fn t.main() -> () uses [] panics may\n    core.check.eq(t.c(4u32), 5u32)\nend t.main\n";
    let t = Tmp::new("final_nested_clone", src);
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
    let k = run(&["kir", "t.c", "--run", "4"], &t.0);
    assert!(text(&k.stderr).contains("未対応"), "{}{}", text(&k.stdout), text(&k.stderr));
}

/// Fable の最終レビュー M1: `--opt` の定数の畳み込みの `saturating_*` は型の最小・最大で切る (参照実行器と同じ算術)。
#[test]
fn final_review_opt_folds_saturating_like_the_reference() {
    let fns = "fn t.s(k: u32) -> u64 uses [] panics never\n    let a = 200u8.saturating_add(100u8)\n    let b = 0u32.saturating_sub(1u32)\n    let c = -100i8.saturating_sub(100i8)\n    let d = u64.from_u8(a).wrapping_add(u64.from_u32(b))\n    d.wrapping_add(u64.from_u32(c.count_ones())).wrapping_add(u64.from_u32(k))\nend t.s\n";
    kir_review_cases("final_sat", fns, "t.s", &[(&["0"], "t.s(0u32)", "256u64", "256")]);
    let t = Tmp::new("final_sat_opt", &format!("{}\nfn t.main() -> () uses [] panics never\nend t.main\n", fns));
    let k = kir_run(&["kir", "t.s", "--opt", "--run", "0"], &t.0);
    assert_eq!(text(&k.stdout).trim(), "256", "{}", text(&k.stderr));
}

#[test]
fn elided_fn_effects_carry_the_closure_effect_to_the_caller() {
    let leak = "        core.mem.drop(std.io.print_line(\"x\"))\n        7";
    let t = Tmp::new("elide_leak", &elide_main("", "never", leak));
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
    assert!(text(&o.stderr).contains("E4001") || text(&o.stdout).contains("E4001"), "{}{}", text(&o.stdout), text(&o.stderr));
    let t = Tmp::new("elide_ok", &elide_main("io.console", "never", leak));
    for args in [&["check"][..], &["run"][..], &["fmt", "--check"][..]] {
        let o = run(args, &t.0);
        assert_eq!(o.status.code(), Some(0), "{:?}: {}{}", args, text(&o.stdout), text(&o.stderr));
    }
    let o = run(&["run"], &t.0);
    assert_eq!(text(&o.stdout).trim(), "x");
    let o = run(&["test", "--both"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let o = run(&["build"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}


/// `linear_ok/` の正しい小プログラムは、すべてネイティブになり、出力・終了コードが `ksn run` と一致する (修正プラン 3-1)。
#[test]
fn linear_ok_programs_build_native_and_match_run() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/linear_ok");
    let mut names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().map_or(false, |x| x == "ksnl")).collect();
    names.sort();
    assert!(names.len() >= 100, "linear_ok cases missing");
    let bad: Vec<String> = par_map(&names, |p| {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("linear_native_{}", stem), &fs::read_to_string(p).unwrap());
        let b = run(&["build"], &t.0);
        let out = format!("{}{}", text(&b.stdout), text(&b.stderr));
        let exe = t.0.join("target").join("debug").join("t.native");
        if out.contains("native backend skipped") || !exe.exists() {
            return Some(format!("{}: バンドルへ退避: {}", stem, out.lines().find(|l| l.contains("skipped")).unwrap_or("")));
        }
        let n = Command::new(&exe).current_dir(&t.0).output().unwrap();
        let r = run(&["run"], &t.0);
        if n.status.code() != r.status.code() || text(&n.stdout) != text(&r.stdout) || text(&n.stderr) != text(&r.stderr) {
            return Some(format!("{}: native と run が違う: {:?} {:?} / {:?} {:?}", stem, n.status.code(), text(&n.stdout), r.status.code(), text(&r.stdout)));
        }
        None
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// `Vec.reserve` / `Buf.with_capacity` で `n * size_of(T)` があふれる要求は `Err(AllocError)` (木走査とネイティブで同じ。Fable レビュー R2#1・#2)。
#[test]
fn huge_alloc_requests_are_err_not_wrapped() {
    let src = [
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "    core.mem.drop(v.push(1u64))",
        "    match v.reserve(2305843009213693952usize) {",
        "        Ok(_u) => std.io.print_line(\"reserve ok\")?",
        "        Err(_e) => std.io.print_line(\"reserve err\")?",
        "    }",
        "    match t.huge_buf() {",
        "        Ok(_b) => std.io.print_line(\"cap ok\")?",
        "        Err(_e) => std.io.print_line(\"cap err\")?",
        "    }",
        "    core.mem.drop(v.push(2u64))",
        "    Ok(())",
        "end t.main",
        "",
        "fn t.huge_buf() -> Result<alloc.raw.Buf<u64, std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics never",
        "    alloc.raw.Buf.with_capacity(std.alloc.SYSTEM, 2305843009213693953usize)",
        "end t.huge_buf",
        "",
    ]
    .join("\n");
    let out = agree_with_interpreter("huge_alloc", &src);
    assert_eq!(out, "reserve err\ncap err\n");
}

/// 長い形 `core.num.<T>.try_from_<S>(..)` / `from_<S>(..)` は、短い形と同じに KIR へ下ろせ、ネイティブの出力が `ksn run` と一致する。
#[test]
fn long_form_num_conversions_lower_to_kir_and_match_the_interpreter() {
    let src = r#"fn t.main() -> () uses [io.console] panics may
    let a = core.num.u32.try_from_u64(5u64)
    let b = core.num.u64.from_u32(7u32)
    let c = 4000000000u32.wrapping_add(300000000u32)
    let d = core.num.u8.try_from_u32(300u32)
    match a {
        Ok(v) => core.check.eq(v, 5u32)
        Err(_e) => core.panic.panic("bad")
    }
    core.check.eq(b, 7u64)
    core.check.eq(c, 5032704u32)
    core.check.eq(core.num.u32.from_u8(3u8), 3u32)
    match d {
        Ok(_v) => core.panic.panic("bad2")
        Err(_e) => ()
    }
    match std.io.print_line("ok") {
        Ok(u) => u
        Err(_e) => ()
    }
end t.main
"#;
    let t = Tmp::new("long_num_conv", src);
    let i = run(&["run"], &t.0);
    assert_eq!((text(&i.stdout).trim(), i.status.code()), ("ok", Some(0)), "{}", text(&i.stderr));
    let b = run(&["build", "--release"], &t.0);
    assert!(b.status.success(), "{}{}", text(&b.stdout), text(&b.stderr));
    let o = Command::new(t.0.join("target").join("release").join("t.native")).output().unwrap();
    assert_eq!((text(&o.stdout).trim(), o.status.code()), ("ok", Some(0)));
}
