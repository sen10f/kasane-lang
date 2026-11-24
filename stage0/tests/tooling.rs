//! refs・rename・doc・context・explain などのコマンドのテスト。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use common::*;

#[test]
fn explain_known_and_unknown_codes() {
    let r = root();
    let ok = run(&["explain", "E1101"], &r);
    assert_eq!(ok.status.code(), Some(0));
    assert!(text(&ok.stdout).starts_with("E1101"));
    let bad = run(&["explain", "E9999"], &r);
    assert_ne!(bad.status.code(), Some(0));
}

#[test]
fn rename_dry_run_does_not_modify_files() {
    let d = root().join("examples").join("demo");
    let before: Vec<_> = walk(&d.join("src")).into_iter().map(|p| (fs::read(&p).unwrap(), p)).collect();
    let o = run(&["rename", "--dry-run", "demo.main", "demo.start"], &d);
    assert!(o.status.code().is_some());
    for (bytes, p) in before {
        assert_eq!(fs::read(&p).unwrap(), bytes, "{:?} was modified", p);
    }
}

fn walk(d: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for e in fs::read_dir(d).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            v.extend(walk(&p));
        } else {
            v.push(p);
        }
    }
    v
}

// ---- ksn context ----

#[test]
fn context_prints_definition_and_referenced_signatures() {
    let d = root().join("examples").join("json");
    let o = run(&["context", "json.compact"], &d);
    assert_eq!(o.status.code(), Some(0));
    let out = text(&o.stdout);
    assert!(out.contains("// target: json.compact"), "{}", out);
    assert!(out.contains("fn json.parse("), "{}", out);
    assert!(out.contains("enum json.Error"), "{}", out);
    // 参照先の本体は含めない (署名のみ)。
    assert!(!out.contains("end json.parse"), "{}", out);
    assert_ne!(run(&["context", "json.nothing"], &d).status.code(), Some(0));
}

/// `File.sync` が通常ファイルで成功し、`with` の本体の `write` / `sync` のエラーが `?` で抜けても、後始末が1回走る
/// (`/dev/full` は Linux のみ。ある環境だけ検査する)。インタプリタとネイティブの両方で通る。
#[test]
fn std_fs_sync_succeeds_and_errors_leave_a_with_body_through_try() {
    let t = Tmp::new("fs_sync", include_str!("data/fs_sync.ksnl"));
    let o = run(&["test", "--both"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stdout).contains("2 passed; 0 failed"), "{}", text(&o.stdout));
}

#[test]
fn name_tools_work_on_non_canonical_source() {
    let src = SN_CANON.replace("x: &t.lib.Item", "x: &lib.Item").replace("lib.make()", "t.lib.make()");
    let t = sn_pkg("sn_tools", &src);
    let g = run(&["get", "t.get"], &t.0);
    assert_eq!(g.status.code(), Some(0), "{}", text(&g.stderr));
    assert!(text(&g.stdout).contains("fn t.get("), "{}", text(&g.stdout));
    let s = run(&["sig", "t.get"], &t.0);
    assert_eq!(s.status.code(), Some(0), "{}", text(&s.stderr));
    let r = run(&["refs", "t.lib.make"], &t.0);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert!(text(&r.stdout).contains("t.main"), "{}", text(&r.stdout));
    let c = run(&["context", "t.main"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}", text(&c.stderr));
}

// ---------------------------------------------------------------- `?` のエラー変換 (4.1 `derive [From]`)

/// 変換が起きる (バリアントで包む)・型が同じなら変換しない・`Option` は変わらない・包まれたエラーが元の中身を運ぶ、を
/// インタプリタとネイティブ (デバッグ・リリース) が同じ結果で通る。`ksn test --both` も通る。
#[test]
fn try_wraps_the_error_in_a_derive_from_variant_and_both_backends_agree() {
    let src = include_str!("data/try_from.ksnl");
    let out = agree_with_interpreter("try_from", src);
    assert_eq!(out, "try-from ok\n");
    let t = Tmp::new("try_from_both", src);
    let o = run(&["test", "--both"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stdout).contains("1 passed; 0 failed"), "{}", text(&o.stdout));
}

#[test]
fn with_test_runs_in_both_the_interpreter_and_native_with_ksn_test_both() {
    // `ksn test --both`: 1 テスト = 1 プロセス。本体が `?` で抜けても後始末が走ることを `test` の中で確かめる。
    let src = format!(
        "{}\ntest t.nested.test_cleanup_on_question_mark\n    core.check.eq(t.nested(0u32), Err(7u8))\n    core.check.eq(t.nested(5u32), Ok(8u32))\nend t.nested.test_cleanup_on_question_mark\n",
        WITH_PROGRAM
    );
    let t = Tmp::new("with_test_both", &src);
    let o = run(&["test", "--both"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

/// 基本型の `Display` (`display_fmt` で `core.fmt.Formatter` に書く。仕様 4.2) と派生の比較 (`ne` / `partial_cmp`) は、
/// インタプリタとバンドルの両方で動く (以前は "unknown integer operation" で panic していた)。
/// `to_string` は仕様に無く検査で拒む (tests/data/unsound/)。
#[test]
fn scalar_display_and_derived_methods_run_in_both() {
    let test = "test t.main.test_scalar_display
    let n = -7i64
    let mut f = core.fmt.Formatter.new(32usize)
    core.mem.drop(n.display_fmt(&mut f))
    core.check.eq(core.str.from_utf8(f.written()), Ok(\"-7\"))
    let big = 4294967295u32
    let mut f2 = core.fmt.Formatter.new(16usize)
    core.mem.drop(big.display_fmt(&mut f2))
    core.check.eq(core.str.from_utf8(f2.written()), Ok(\"4294967295\"))
    let c = 'x'
    let mut f3 = core.fmt.Formatter.new(8usize)
    core.mem.drop(c.display_fmt(&mut f3))
    core.check.eq(core.str.from_utf8(f3.written()), Ok(\"x\"))
    let t = true
    let mut f4 = core.fmt.Formatter.new(8usize)
    core.mem.drop(t.display_fmt(&mut f4))
    core.check.eq(core.str.from_utf8(f4.written()), Ok(\"true\"))
    core.check.eq(n != 3i64, true)
    core.check.is_true(n.partial_cmp(&3i64) is Some(_))
end t.main.test_scalar_display
";
    let src = format!("{}\n{}", main_wrap("", ""), test);
    let t = Tmp::new("scalar_display_both", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    // 配列の繰り返し式は KIR に下ろせないので、機械語 (LLVM) で比べる。
    assert!(out.contains("test t.main.test_scalar_display ... ok (both: bundle (llvm skipped: "), "{}", out);
}

/// `str` / `String` の `to_string`、スライス `Iter.count`、`&str` / `&[T]` / Option / Result の派生比較
/// (`eq` `ne` `cmp` `partial_cmp`) は spec/stdlib の規範どおり check が受理する。以前はいずれも
/// インタプリタで "no method ..." の panic になっていた。インタプリタとバンドルの両方で動くことを確かめる。
#[test]
fn str_slice_option_result_sig_methods_run_in_both() {
    let test = "test t.main.test_a_option_result_cmp
    let o = Some(1u8)
    core.check.eq(o.eq(&Some(1u8)), true)
    core.check.eq(o.cmp(&None) == core.cmp.Ordering.Greater, true)
    let r: Result<u8, u8> = Ok(1u8)
    core.check.eq(r != Err(1u8), true)
end t.main.test_a_option_result_cmp

test t.main.test_b_slice_count
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(1u8))
    core.mem.drop(v.push(2u8))
    core.check.eq(v.iter().count(), 2usize)
    let mut it = v.iter()
    core.mem.drop(it.next())
    core.check.eq(it.count(), 1usize)
end t.main.test_b_slice_count

test t.main.test_c_str_derived_cmp
    let s: &str = \"ab\"
    let t: &str = \"ac\"
    core.check.eq(s.eq(&t), false)
    core.check.eq(s != t, true)
    core.check.eq(s.cmp(&t) == core.cmp.Ordering.Less, true)
    core.check.is_true(s.partial_cmp(&t) is Some(_))
end t.main.test_c_str_derived_cmp

test t.main.test_d_to_string
    let s: &str = \"héllo\"
    let mut owned = match alloc.string.String.from_str(std.alloc.SYSTEM, s) {
        Ok(o) => o
        Err(_e) => alloc.string.String.new(std.alloc.SYSTEM)
    }
    core.check.eq(owned.as_str(), \"héllo\")
    core.check.eq(owned.len(), 6usize)
    core.mem.drop(owned.push('!'))
    core.check.eq(owned.as_str(), \"héllo!\")
    let st = alloc.string.String.new(std.alloc.SYSTEM)
    core.check.eq(st.is_empty(), true)
end t.main.test_d_to_string
";
    let src = format!("{}\n{}", main_wrap("", ""), test);
    let t = Tmp::new("str_slice_option_result_sig", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    for name in ["test_a_option_result_cmp", "test_b_slice_count"] {
        // 機械語とインタプリタの結果が一致する (KIR に下ろせないので LLVM で比べる)。
        assert!(out.contains(&format!("t.main.{} ... ok (both: bundle (llvm skipped: ", name)), "{}", out);
    }
    // `&str` / `String` の比較 (`cmp` / `eq`) は KIR に下りる (Kasane 版のマップのキーに要る。段 2): 機械語で比べる。
    assert!(out.contains("t.main.test_c_str_derived_cmp ... ok (both: llvm)"), "{}", out);
    // 解放が要る値を作る `match` (`Ok(o) => o` / `Err(_e) => ..`) は機械語で動き、インタプリタと一致する。
    // `test` の入口は全部の効果を持つので、alloc を使う本体も LLVM で比べられる。
    assert!(out.contains("t.main.test_d_to_string ... ok (both: llvm)"), "{}", out);
}

#[test]
fn derived_cmp_and_iter_count_boundaries_run_native() {
    // `Option` / `Result` / `str` の派生の比較 (None < Some、Ok < Err、ペイロードは辞書順、str は UTF-8 バイト列の辞書順) と
    // `v.iter()` の `next` / `count` の境界を、機械語とインタプリタで比べる。
    let test = r#"test t.main.test_e_option_boundaries
    let none: Option<i8> = None
    let a = Some(-1i8)
    let b = Some(1i8)
    core.check.eq(none.cmp(&none) == core.cmp.Ordering.Equal, true)
    core.check.eq(none.cmp(&a) == core.cmp.Ordering.Less, true)
    core.check.eq(a.cmp(&none) == core.cmp.Ordering.Greater, true)
    core.check.eq(a.cmp(&a) == core.cmp.Ordering.Equal, true)
    core.check.eq(a.cmp(&b) == core.cmp.Ordering.Less, true)
    core.check.eq(b.cmp(&a) == core.cmp.Ordering.Greater, true)
    core.check.eq(none.eq(&none), true)
    core.check.eq(none.eq(&a), false)
    core.check.eq(a != b, true)
    core.check.eq(a != a, false)
    let big = Some(255u8)
    let small = Some(1u8)
    core.check.eq(big.cmp(&small) == core.cmp.Ordering.Greater, true)
    let sa: Option<&str> = Some("ab")
    let sb: Option<&str> = Some("abc")
    core.check.eq(sa.cmp(&sb) == core.cmp.Ordering.Less, true)
    core.check.eq(sb.cmp(&sa) == core.cmp.Ordering.Greater, true)
    core.check.eq(sa.eq(&sb), false)
    let pa = sa.partial_cmp(&sb)
    core.check.is_true(pa is Some(_))
end t.main.test_e_option_boundaries

test t.main.test_f_result_boundaries
    let ok1: Result<u8, u8> = Ok(1u8)
    let ok2: Result<u8, u8> = Ok(2u8)
    let er0: Result<u8, u8> = Err(0u8)
    let er9: Result<u8, u8> = Err(9u8)
    core.check.eq(ok1.cmp(&ok1) == core.cmp.Ordering.Equal, true)
    core.check.eq(ok1.cmp(&ok2) == core.cmp.Ordering.Less, true)
    core.check.eq(ok2.cmp(&ok1) == core.cmp.Ordering.Greater, true)
    core.check.eq(ok2.cmp(&er0) == core.cmp.Ordering.Less, true)
    core.check.eq(er0.cmp(&ok2) == core.cmp.Ordering.Greater, true)
    core.check.eq(er0.cmp(&er9) == core.cmp.Ordering.Less, true)
    core.check.eq(er9.cmp(&er0) == core.cmp.Ordering.Greater, true)
    core.check.eq(er9.cmp(&er9) == core.cmp.Ordering.Equal, true)
    core.check.eq(ok1.eq(&ok1), true)
    core.check.eq(ok1.eq(&er0), false)
    core.check.eq(ok1 != Ok(1u8), false)
    core.check.eq(er0 != Err(1u8), true)
end t.main.test_f_result_boundaries

test t.main.test_g_str_boundaries
    let empty: &str = ""
    let a: &str = "a"
    let ab: &str = "ab"
    let abc: &str = "abc"
    let b: &str = "b"
    core.check.eq(empty.cmp(&empty) == core.cmp.Ordering.Equal, true)
    core.check.eq(empty.cmp(&a) == core.cmp.Ordering.Less, true)
    core.check.eq(a.cmp(&empty) == core.cmp.Ordering.Greater, true)
    core.check.eq(ab.cmp(&abc) == core.cmp.Ordering.Less, true)
    core.check.eq(abc.cmp(&ab) == core.cmp.Ordering.Greater, true)
    core.check.eq(abc.cmp(&b) == core.cmp.Ordering.Less, true)
    core.check.eq(b.cmp(&abc) == core.cmp.Ordering.Greater, true)
    core.check.eq(abc.cmp(&abc) == core.cmp.Ordering.Equal, true)
    core.check.eq(ab.eq(&abc), false)
    core.check.eq(abc.eq(&abc), true)
    core.check.eq(empty.eq(&empty), true)
    core.check.eq(ab != abc, true)
    core.check.eq(abc != abc, false)
    let e1: &str = "é"
    let e2: &str = "f"
    let e3: &str = "あ"
    let e4: &str = "い"
    let e5: &str = "éa"
    core.check.eq(e1.cmp(&e2) == core.cmp.Ordering.Greater, true)
    core.check.eq(e2.cmp(&e1) == core.cmp.Ordering.Less, true)
    core.check.eq(e3.cmp(&e4) == core.cmp.Ordering.Less, true)
    core.check.eq(e1.cmp(&e5) == core.cmp.Ordering.Less, true)
    core.check.eq(e1.cmp(&e1) == core.cmp.Ordering.Equal, true)
    core.check.eq(e1.eq(&e1), true)
    core.check.eq(e1.eq(&e5), false)
    let p = ab.partial_cmp(&abc)
    core.check.is_true(p is Some(_))
    core.check.eq(p.unwrap() == core.cmp.Ordering.Less, true)
end t.main.test_g_str_boundaries

test t.main.test_h_count_boundaries
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.check.eq(v.iter().count(), 0usize)
    let mut it0 = v.iter()
    core.check.is_true(it0.next() is None)
    core.check.eq(it0.count(), 0usize)
    core.mem.drop(v.push(7u32))
    core.mem.drop(v.push(8u32))
    core.mem.drop(v.push(9u32))
    let mut it = v.iter()
    core.check.eq(it.next().unwrap(), 7u32)
    core.check.eq(it.next().unwrap(), 8u32)
    core.check.eq(it.count(), 1usize)
    let mut it2 = v.iter()
    core.mem.drop(it2.next())
    core.mem.drop(it2.next())
    core.mem.drop(it2.next())
    core.check.is_true(it2.next() is None)
    core.check.eq(it2.count(), 0usize)
end t.main.test_h_count_boundaries
"#;
    let src = format!("{}\n{}", main_wrap("", ""), test);
    let t = Tmp::new("derived_cmp_boundaries", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    for name in ["test_e_option_boundaries", "test_f_result_boundaries", "test_h_count_boundaries"] {
        assert!(out.contains(&format!("t.main.{} ... ok (both: bundle (llvm skipped: ", name)), "{}", out);
    }
    // `&str` の比較の境界は KIR に下りる (段 2 の `&str` のキー): 機械語で比べる。
    assert!(out.contains("t.main.test_g_str_boundaries ... ok (both: llvm)"), "{}", out);
}

/// 解放が要る値を返す `match` (腕が Vec / String を返す、解放が要る payload を束縛する・捨てる、腕の `return`、
/// 繰り返し) が機械語でも動き、インタプリタと一致する。二重解放や解放漏れがあれば機械語側が落ちるか、
/// 10 万回の繰り返しで結果が合わなくなる。
#[test]
fn match_producing_droppable_values_runs_natively() {
    let pick = r#"fn t.pick(s: &str) -> alloc.string.String uses [alloc] panics never
    match alloc.string.String.from_str(std.alloc.SYSTEM, s) {
        Ok(o) => return o
        Err(_e) => return alloc.string.String.new(std.alloc.SYSTEM)
    }
end t.pick

"#;
    let tests = r#"test t.main.test_e_vec_arms
    let n = 1u8
    let mut v: alloc.vec.Vec<u8, std.alloc.System> = match n {
        0u8 => alloc.vec.Vec.new(std.alloc.SYSTEM)
        _ => alloc.vec.Vec.new(std.alloc.SYSTEM)
    }
    core.mem.drop(v.push(7u8))
    core.check.eq(v.len(), 1usize)
end t.main.test_e_vec_arms

test t.main.test_f_block_arm_moves_local
    let n = 1u8
    let v = match n {
        0u8 => alloc.vec.Vec.new(std.alloc.SYSTEM)
        _ => {
            let mut w: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
            core.mem.drop(w.push(5u8))
            w
        }
    }
    core.check.eq(v.len(), 1usize)
end t.main.test_f_block_arm_moves_local

test t.main.test_g_unused_payloads
    let s: &str = "abc"
    let a = match alloc.string.String.from_str(std.alloc.SYSTEM, s) {
        Ok(_o) => 1usize
        Err(_e) => 0usize
    }
    core.check.eq(a, 1usize)
    let b = match alloc.string.String.from_str(std.alloc.SYSTEM, s) {
        Ok(_) => 2usize
        Err(_e) => 0usize
    }
    core.check.eq(b, 2usize)
    let c = match alloc.string.String.from_str(std.alloc.SYSTEM, s) {
        Ok(o) => o.len()
        Err(_e) => 9usize
    }
    core.check.eq(c, 3usize)
end t.main.test_g_unused_payloads

test t.main.test_h_loop_100k
    let mut i = 0usize
    let mut total = 0usize
    while i < 100000usize {
        let s = match alloc.string.String.from_str(std.alloc.SYSTEM, "héllo") {
            Ok(o) => o
            Err(_e) => alloc.string.String.new(std.alloc.SYSTEM)
        }
        total = total + s.len()
        i = i + 1usize
    }
    core.check.eq(total, 600000usize)
end t.main.test_h_loop_100k

test t.main.test_i_return_from_arm
    let s = t.pick("xy")
    core.check.eq(s.len(), 2usize)
    let e = t.pick("")
    core.check.eq(e.is_empty(), true)
end t.main.test_i_return_from_arm

test t.main.test_k_place_scrutinee_fresh_arms
    let r = alloc.string.String.from_str(std.alloc.SYSTEM, "xy")
    let s = match r {
        Ok(_o) => alloc.string.String.new(std.alloc.SYSTEM)
        Err(_e) => alloc.string.String.new(std.alloc.SYSTEM)
    }
    core.check.eq(s.is_empty(), true)
end t.main.test_k_place_scrutinee_fresh_arms
"#;
    let src = format!("{}\n{}", main_wrap(pick, ""), tests);
    let t = Tmp::new("match_droppable_native", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    // 6 本とも LLVM で比べられる (テストの入口は全部の効果を持つので、alloc の関数を呼ぶ本体も下ろせる)。
    assert_eq!(out.matches("(both: llvm)").count(), 6, "{}", out);
    assert!(!out.contains("both: bundle") && !out.contains("both: native"), "{}", out);
    assert!(out.contains("6 passed; 0 failed"), "{}", out);
}

/// 解放が要る値を持つ場所 (`let` の局所) から移動する `match` も、LLVM で比べられる (テストの入口は全部の効果を持つ)。
/// 以前は alloc の検査で退避していた形。インタプリタと同じ結果になる。
#[test]
fn match_moving_out_of_a_place_scrutinee_runs_on_llvm() {
    let tests = r#"test t.main.test_j_place_scrutinee_moves
    let r = alloc.string.String.from_str(std.alloc.SYSTEM, "x")
    let s = match r {
        Ok(o) => o
        Err(_e) => alloc.string.String.new(std.alloc.SYSTEM)
    }
    core.check.eq(s.len(), 1usize)
end t.main.test_j_place_scrutinee_moves
"#;
    let src = format!("{}\n{}", main_wrap("", ""), tests);
    let t = Tmp::new("match_place_fallback", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("t.main.test_j_place_scrutinee_moves ... ok (both: llvm)"), "{}", out);
}

// ---- spec/ptr-const-ne: add_const / cast_const (const を保つ加算・変換) と `ne` の撤去 (演算子 `!=` のみ)

#[test]
fn ptr_add_const_and_cast_const_keep_const_and_run_in_both() {
    // from_ref の `*const` を add_const (count 0) と cast_const::<i32, i32> で読み直す。from_mut の `*mut` を add_const で
    // const に落としたものも読める。不変のスカラー変数 `x` の `from_ref(&x)` も、スタックスロットのアドレスとして扱い、
    // インタプリタとネイティブの両方で同じ結果になる。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main

test t.main.test_add_const_cast_const
    let mut m = 7i32
    let p: *mut i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.from_mut(&mut m)
    }
    let x = 41i32
    let q: *const i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.from_ref(&x)
    }
    let r: *const i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.add_const(q, 0usize)
    }
    let c: *const i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.cast_const::<i32, i32>(r)
    }
    let w: *const i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.add_const(p, 0usize)
    }
    unsafe "libc" "read through the const helpers" {
        core.check.eq(core.ptr.read(r), 41i32)
        core.check.eq(core.ptr.read(c), 41i32)
        core.check.eq(core.ptr.read(w), 7i32)
    }
end t.main.test_add_const_cast_const
"#;
    let t = Tmp::new("ptr_add_const_both", src);
    fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t\"]\n\n[dependencies]", 1)).unwrap();
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("t.main.test_add_const_cast_const ... ok (both: bundle (llvm skipped: "), "{}", out);
}

// ---- `test --both`: 機械語 (KIR → LLVM、作れなければバンドル) との比較と、バンドルへの退避 ----

const BOTH_SRC: &str = "fn t.main() -> () uses [] panics never
end t.main

test t.main.test_ok
    core.check.eq(1u8, 1u8)
end t.main.test_ok
";

#[test]
fn test_both_runs_kir_tests_as_llvm_executables() {
    if !clang_available() {
        eprintln!("skip: clang が PATH に無いので、LLVM の比較は飛ばす");
        return;
    }
    let src = format!("{}\ntest t.main.test_fail\n    core.check.eq(1u8, 2u8)\nend t.main.test_fail\n", BOTH_SRC);
    let t = Tmp::new("both_llvm", &src);
    // `--both` を付けなければ、出力の形式は従来どおり (注記なし)。
    let plain = run(&["test"], &t.0);
    assert!(text(&plain.stdout).contains("test t.main.test_ok ... ok\n"), "{}", text(&plain.stdout));
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_ok ... ok (both: llvm)"), "{}", out);
    assert!(out.contains("test t.main.test_fail ... FAILED: assertion failed: left: 1, right: 2 (both: llvm)"), "{}", out);
    assert!(out.contains("1 passed; 1 failed"), "{}", out);
    assert!(out.contains("compared with: llvm 2, bundle fallback 0"), "{}", out);
    assert!(!out.contains("both: bundle") && !out.contains("disagree"), "{}", out);
    assert!(t.0.join("target").join("test-llvm").is_dir(), "no llvm test executable: {}", out);
}

#[test]
fn test_both_compares_llvm_panics_with_the_tree_walker() {
    // KIR (LLVM) の失敗の文言は木走査と同じ。除算・unwrap・overflow・assert が、木走査と同じ文言・同じ分類で出る。
    let src = format!(
        "{}\ntest t.main.test_div0\n    let z = core.num.u8.MIN\n    core.check.eq(7u8 / z, 0u8)\nend t.main.test_div0\n\n\
         test t.main.test_unwrap\n    let o: Option<u8> = None\n    core.check.eq(o.unwrap(), 0u8)\nend t.main.test_unwrap\n\n\
         test t.main.test_overflow\n    let x = 255u8\n    let y = x + 1u8\n    core.check.eq(y, 0u8)\nend t.main.test_overflow\n\n\
         test t.main.test_assert\n    core.check.eq(1u8, 2u8)\nend t.main.test_assert\n",
        BOTH_SRC
    );
    if !clang_available() {
        eprintln!("skip: clang が PATH に無いので、LLVM の比較は飛ばす");
        return;
    }
    let t = Tmp::new("both_llvm_panics", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_div0 ... FAILED: attempt to divide by zero (both: llvm)"), "{}", out);
    assert!(out.contains("test t.main.test_unwrap ... FAILED: called `Option::unwrap()` on a `None` value (both: llvm)"), "{}", out);
    assert!(out.contains("test t.main.test_overflow ... FAILED: attempt to add with overflow (both: llvm)"), "{}", out);
    assert!(out.contains("test t.main.test_assert ... FAILED: assertion failed: left: 1, right: 2 (both: llvm)"), "{}", out);
    assert!(!out.contains("disagree"), "{}", out);
    assert!(out.contains("compared with: llvm 5, bundle fallback 0"), "{}", out);
}

#[test]
fn test_both_runs_a_test_without_uses_that_calls_alloc_and_console_functions_on_llvm() {
    // `test` には uses の宣言がない。`ksn check` は test の本体で効果を見ない (全部の効果を許す) ので、
    // KIR の入口も全部の効果を持つとして下ろす。`alloc` と `io.console` を使う関数を呼んでも、LLVM で比べる。
    if !clang_available() {
        eprintln!("skip: clang が PATH に無いので、LLVM の比較は飛ばす");
        return;
    }
    let src = format!(
        "{}\nfn t.main.collect(n: u32) -> usize uses [alloc, io.console] panics may\n    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    match v.push(n) {{\n        Ok(_) => ()\n        Err(_e) => core.panic.panic(\"oom\")\n    }}\n    core.mem.drop(std.io.print_line(\"collected\"))\n    v.len()\nend t.main.collect\n\n\
         test t.main.test_alloc_and_console_callees\n    core.check.eq(t.main.collect(3u32), 1usize)\nend t.main.test_alloc_and_console_callees\n\n\
         test t.main.test_alloc_and_console_callees_fail\n    core.check.eq(t.main.collect(3u32), 2usize)\nend t.main.test_alloc_and_console_callees_fail\n",
        BOTH_SRC
    );
    let t = Tmp::new("both_llvm_entry_effects", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert!(out.contains("test t.main.test_alloc_and_console_callees ... ok (both: llvm)"), "{}", out);
    assert!(out.contains("test t.main.test_alloc_and_console_callees_fail ... FAILED: assertion failed: left: 1, right: 2 (both: llvm)"), "{}", out);
    assert!(!out.contains("(both: native") && !out.contains("both: bundle"), "{}", out);
    assert!(!out.contains("disagree"), "{}", out);
    assert!(out.contains("compared with: llvm 3, bundle fallback 0"), "{}", out);
    assert_eq!(o.status.code(), Some(1), "{}{}", out, text(&o.stderr));
}

#[test]
fn test_both_falls_back_from_llvm_to_the_bundle_with_the_reason() {
    // `i128.successor` は Option<i128> の戻り値のため KIR に下ろせない (段階 1 の対象外) ので、LLVM は理由を出してバンドルへ退避する (以前の例 `char.is_ascii_digit` は kir/native-gaps-3 で下りるようになった)。
    // バンドルの文言は木走査と同じなので一致になる。
    let src = format!(
        "{}\ntest t.main.test_mixed\n    let x = 255u8\n    let y = x + 1u8\n    core.check.eq(y, 0u8)\n    core.check.eq(i128.successor(&1i128), Some(2i128))\nend t.main.test_mixed\n",
        BOTH_SRC
    );
    let t = Tmp::new("both_llvm_to_native", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_mixed ... FAILED: attempt to add with overflow (both: bundle (llvm skipped: "), "{}", out);
    // `BOTH_SRC` の test_ok は LLVM で比べる (clang が無ければバンドル)。
    if clang_available() {
        assert!(out.contains("compared with: llvm 1, bundle fallback 1"), "{}", out);
    } else {
        assert!(out.contains("compared with: llvm 0, bundle fallback 2"), "{}", out);
    }
    assert!(!out.contains("disagree"), "{}", out);
}

#[test]
fn test_both_stack_overflow_is_exit_102_on_both_sides() {
    // 木走査は深さ上限で `Exit(102)`、LLVM は終了コード 102 (stderr に `stack overflow`)。どちらも終了の種類 `exit(102)` で比べて一致する。
    let src = "fn t.down(n: u64) -> u64 uses [] panics never stack 4096\n    if n == 0u64 {\n        return 0u64\n    }\n    t.down(n.wrapping_sub(1u64)).wrapping_add(1u64)\nend t.down\n\n\
               fn t.main() -> () uses [] panics never\nend t.main\n\n\
               test t.main.test_deep\n    core.check.eq(t.down(200000u64), 200000u64)\nend t.main.test_deep\n";
    let t = Tmp::new("both_stack_102", src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}{}", out, text(&o.stderr));
    // 失敗の表示は木走査の結果 (`exit(102)`)。機械語も同じ終了の種類なので、不一致にはならない。
    assert!(out.contains("test t.main.test_deep ... FAILED: exit(102) (both: llvm)"), "{}", out);
    assert!(!out.contains("disagree"), "{}", out);
}

#[test]
fn test_both_compares_failure_classes_when_both_backends_fail() {
    // 両方が失敗しても、理由の分類 (overflow / assert / ...) が違えば disagree。同じ分類なら文言が違っても一致。
    // 分類が違うケースの判定は `src/cli.rs` の単体テスト `both_failing_with_different_classes_disagree` で確かめる。
    let src = format!(
        "{}\ntest t.main.test_assert\n    core.check.eq(1u8, 2u8)\nend t.main.test_assert\n\ntest t.main.test_overflow\n    let x = 255u8\n    let y = x + 1u8\n    core.check.eq(y, 0u8)\nend t.main.test_overflow\n",
        BOTH_SRC
    );
    let t = Tmp::new("both_class", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}{}", out, text(&o.stderr));
    // 両方とも失敗し、文言が同じ (木走査と LLVM) なら一致。分類が違う組み合わせは `both_tests` の単体テストで確かめる。
    if clang_available() {
        assert!(out.contains("test t.main.test_overflow ... FAILED: attempt to add with overflow (both: llvm)"), "{}", out);
        assert!(out.contains("test t.main.test_assert ... FAILED: assertion failed: left: 1, right: 2 (both: llvm)"), "{}", out);
        assert!(out.contains("compared with: llvm 3, bundle fallback 0"), "{}", out);
    }
    assert!(!out.contains("disagree"), "{}", out);
}

#[test]
fn test_both_falls_back_to_the_bundle_when_llvm_cannot_build_128_bit_integers() {
    // 128 ビットの値・除算・剰余は KIR (LLVM) に下ろせないので、LLVM の理由を出してバンドルで比べる。
    let src = format!(
        "{}\ntest t.main.test_wide\n    core.check.eq(5i128.checked_add(0i128), Some(5i128))\nend t.main.test_wide\n\
         test t.main.test_div\n    let m = core.num.i128.MIN\n    core.check.eq(m.checked_rem(-1i128), None)\nend t.main.test_div\n",
        BOTH_SRC
    );
    let t = Tmp::new("both_bundle", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_wide ... ok (both: bundle (llvm skipped: "), "{}", out);
    assert!(out.contains("test t.main.test_div ... ok (both: bundle (llvm skipped: "), "{}", out);
    assert!(out.contains("3 passed; 0 failed"), "{}", out);
    if clang_available() {
        assert!(out.contains("test t.main.test_ok ... ok (both: llvm)"), "{}", out);
        assert!(out.contains("compared with: llvm 1, bundle fallback 2"), "{}", out);
    } else {
        assert!(out.contains("compared with: llvm 0, bundle fallback 3"), "{}", out);
    }
}

#[test]
fn float_specials_and_unsigned_from_str_radix_agree_with_native() {
    // 機械語 (`test --both`) で比べられる形だけを書く: 値の比較 (`!=` で NaN、`1.0 / x` で -0 を判別)。
    // LLVM は `to_bits` / `is_nan` を持たないので、それらは上の `float_specials_follow_ieee_on_both_backends` で確かめる。
    // 符号なしは `-` を受け付けない (`-0` も Err、Rust と同じ)。`+` は受け付ける。
    let mut src = String::from(FLOAT_SPECIAL_FNS);
    src.push_str("fn t.pinf() -> f64 uses [] panics never\n    1000.0f64.exp()\nend t.pinf\n\nfn t.main() -> () uses [] panics never\nend t.main\n");
    let checks: Vec<String> = vec![
        "core.check.eq(t.ninf().abs(), t.pinf())".into(),
        "core.check.eq(t.ninf().sqrt() != t.ninf().sqrt(), true)".into(),
        "core.check.eq((-2.0f64).sqrt() != (-2.0f64).sqrt(), true)".into(),
        "core.check.eq(t.pinf().sqrt(), t.pinf())".into(),
        "core.check.eq(1.0f64 / (-(0.0f64)).sqrt() < 0.0f64, true)".into(),
        "core.check.eq(u8.parse(\"-0\") != Ok(0u8), true)".into(),
        "core.check.eq(u8.parse(\"+7\"), Ok(7u8))".into(),
        "core.check.eq(i8.parse(\"-0\"), Ok(0i8))".into(),
    ];
    for (i, c) in checks.iter().enumerate() {
        src.push_str(&format!("\ntest t.main.test_{}\n    {}\nend t.main.test_{}\n", i, c, i));
    }
    let t = Tmp::new("float_specials_both", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains(&format!("{} passed; 0 failed", checks.len())), "{}", out);
    // 整数・浮動小数の比較と `from_str_radix` の一部は LLVM で比べる (下ろせないものは理由を出してバンドルへ退避する)。
    if clang_available() {
        // 浮動小数の `core.check.eq` は KIR に下ろせる (kir_rt.c の表示)。`from_str_radix` の 3 本は退避する。
        // `from_str_radix` と `Result != Result` (集成体のペイロード、kir/option-eq) も KIR に下ろせるので、全件 LLVM で比べる。
        assert_eq!(out.matches("(both: llvm)").count(), 8, "{}", out);
        assert_eq!(out.matches("(both: bundle (llvm skipped: ").count(), 0, "{}", out);
    } else {
        assert_eq!(out.matches("(both: bundle").count(), checks.len(), "{}", out);
    }
}

#[test]
fn i128_u128_checked_and_wrapping_native() {
    // i128 / u128 の極値 (MIN / MAX) での checked_add / wrapping_add なども機械語で比べる。
    let src = "fn t.main() -> () uses [] panics never\nend t.main\n\n\
        test t.main.test_i128_min\n    let m = core.num.i128.MIN\n    core.check.eq(m.wrapping_sub(1i128), core.num.i128.MAX)\n    core.check.eq(m.checked_add(-1i128), None)\nend t.main.test_i128_min\n\n\
        test t.main.test_u128_max\n    let m = core.num.u128.MAX\n    core.check.eq(m.wrapping_add(1u128), 0u128)\n    core.check.eq(m.checked_add(1u128), None)\nend t.main.test_u128_max\n";
    let t = Tmp::new("i128_u128_checked_wrapping", src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("2 passed; 0 failed"), "{}", out);
    // 128 ビットの定数は KIR に下ろせないので、LLVM の理由を出してバンドルで比べる。
    for name in ["t.main.test_i128_min", "t.main.test_u128_max"] {
        assert!(out.contains(&format!("{name} ... ok (both: bundle (llvm skipped: ")), "{}", out);
    }
}

/// 派生の比較 (`Option` の `eq` / `cmp`) は、ペイロードの利用者型の手書きの `eq` / `cmp` を使い、列挙子の順序は
/// その列挙型の宣言順 (4.3)。木走査は以前、入れ子を構造的に比べ、`Option` の中の列挙型の順序を取り違えていた。
/// LLVM は手書きの比較を持つ型を構造的に比べないよう退避する。
#[test]
fn derived_comparisons_use_nested_user_impls_and_declared_variant_order() {
    let src = r#"enum t.E derive [Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord] pub
    B
    A
end t.E

type t.P derive [Debug, Clone, Copy] pub
    x: u8
    y: u8
end t.P

impl core.cmp.Eq for t.P

impl core.cmp.Ord for t.P
fn t.P.cmp(self: &t.P, other: &t.P) -> core.cmp.Ordering uses [] panics never impls core.cmp.Ord
    if self.x > other.x {
        core.cmp.Ordering.Less
    } else if self.x < other.x {
        core.cmp.Ordering.Greater
    } else {
        core.cmp.Ordering.Equal
    }
end t.P.cmp

impl core.cmp.PartialEq<t.P> for t.P
fn t.P.eq(self: &t.P, other: &t.P) -> bool uses [] panics never impls core.cmp.PartialEq<t.P>
    self.x == other.x
end t.P.eq

impl core.cmp.PartialOrd<t.P> for t.P
fn t.P.partial_cmp(self: &t.P, other: &t.P) -> Option<core.cmp.Ordering> uses [] panics never impls core.cmp.PartialOrd<t.P>
    Some(self.cmp(other))
end t.P.partial_cmp

fn t.main() -> () uses [] panics never
end t.main

test t.main.test_nested_enum_order
    let b = Some(t.E.B)
    let a = Some(t.E.A)
    core.check.eq(b.cmp(&a) == core.cmp.Ordering.Less, true)
    core.check.eq(a.cmp(&b) == core.cmp.Ordering.Greater, true)
end t.main.test_nested_enum_order

test t.main.test_nested_user_cmp
    let a = Some(t.P { x: 1u8, y: 1u8 })
    let b = Some(t.P { x: 2u8, y: 1u8 })
    core.check.eq(a.cmp(&b) == core.cmp.Ordering.Greater, true)
    let c = Some(t.P { x: 1u8, y: 9u8 })
    core.check.eq(a == c, true)
    core.check.eq(a.eq(&c), true)
end t.main.test_nested_user_cmp
"#;
    let t = Tmp::new("nested_user_cmp", src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("2 passed; 0 failed"), "{}", out);
    assert!(out.contains("t.main.test_nested_enum_order ... ok (both: bundle (llvm skipped: "), "{}", out);
    assert!(out.contains("t.main.test_nested_enum_order を KIR に下ろせない"), "{}", out);
}

/// LLVM の 128 ビット整数: 確かめた演算 (比較・wrapping・checked_add / sub・単項 `-`・`from_*` の拡張) だけを機械語にし、
/// 除算・剰余・乗算・`try_from_*` など 64 ビットを前提にした経路は理由付きで退避する。以前は `checked_div` が
/// `Some(被除数)` を返す誤った機械語になっていた。どのテストも、インタプリタと食い違わない。
#[test]
fn clif_128_bit_integers_are_native_only_where_supported() {
    let src = r#"fn t.main() -> () uses [] panics never
end t.main

test t.main.test_a_div
    let x = 100i128
    core.check.eq(x.checked_div(7i128), Some(14i128))
end t.main.test_a_div

test t.main.test_b_rem
    let x = 100u128
    core.check.eq(x.checked_rem(7u128), Some(2u128))
end t.main.test_b_rem

test t.main.test_c_ops
    let x = 100i128
    let y = 7i128
    core.check.eq(x / y, 14i128)
    core.check.eq(x % y, 2i128)
end t.main.test_c_ops

test t.main.test_d_mul
    let x = 100000000000i128
    core.check.eq(x * x, 10000000000000000000000i128)
    core.check.eq(x.checked_mul(x), Some(10000000000000000000000i128))
    core.check.eq(x.wrapping_mul(3i128), 300000000000i128)
end t.main.test_d_mul

test t.main.test_e_cmp_shift
    let x = core.num.u128.MAX
    core.check.eq(x > 5u128, true)
    core.check.eq(x.wrapping_add(1u128), 0u128)
    core.check.eq(core.num.i32.MIN < 0i32, true)
    core.check.eq(core.num.i8.MIN, -128i8)
    core.check.eq(core.num.i16.MAX, 32767i16)
end t.main.test_e_cmp_shift

test t.main.test_f_neg_conv
    let x = 5i128
    core.check.eq(-x, -5i128)
    let y = i128.from_i64(-7i64)
    core.check.eq(y, -7i128)
    let z = u128.from_u64(18446744073709551615u64)
    core.check.eq(z.wrapping_add(1u128) > z, true)
end t.main.test_f_neg_conv

test t.main.test_g_try_conv
    let big = core.num.u128.MAX
    core.check.is_true(u8.try_from_u128(big) is Err(_))
end t.main.test_g_try_conv
"#;
    let t = Tmp::new("clif_i128_scope", src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("7 passed; 0 failed"), "{}", out);
    assert!(!out.contains("disagree"), "{}", out);
    // 2026-11-21: 128 ビットの `/` `%`・比較・符号反転・`from_i64`/`from_u64` は KIR に下りるようになった (LLVM で比べる)。
    // バンドルのまま残るのは `Option<i128>` の戻り値 (`checked_*`) と、`Result` の変換の形だけ。
    for name in ["test_c_ops", "test_e_cmp_shift", "test_f_neg_conv"] {
        assert!(out.contains(&format!("t.main.{} ... ok (both: llvm)", name)), "{}", out);
    }
    for name in ["test_a_div", "test_b_rem", "test_d_mul", "test_g_try_conv"] {
        assert!(out.contains(&format!("t.main.{} ... ok (both: bundle (llvm skipped: ", name)), "{}", out);
    }
}

#[test]
fn kir_lower_string_call_result_as_str_and_const_split_lower_and_match_run() {
    // 呼び出しの結果の String (集成体の値) に `.as_str()` / 定数の `&str` を `for .. in K.split(..)` の対象にする形は、
    // 下ろしの検査 (load は ptr から読む / 式の型が型表にある) で落ちずに LLVM まで下りて、木走査と一致する。
    let src = format!(
        "{}\n{}",
        main_wrap("", ""),
        "const t.main.SAMPLE: &str = \"a=b\\nc=d\\n\"

fn t.main.make(s: &str) -> alloc.string.String<std.alloc.System> uses [alloc] panics never
    let mut out = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(out.push_str(s))
    out
end t.main.make

test t.main.test_a_call_result_as_str
    core.check.eq(t.main.make(\"plain\").as_str(), \"plain\")
    core.check.eq(t.main.make(\"x\").as_str().len(), 1usize)
end t.main.test_a_call_result_as_str

test t.main.test_b_const_split
    let mut n = 0usize
    for _line in t.main.SAMPLE.split(\"\\n\") {
        n = n.wrapping_add(1usize)
    }
    core.check.eq(n, 3usize)
end t.main.test_b_const_split
"
    );
    let t = Tmp::new("kir_call_as_str_const_split", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    for name in ["test_a_call_result_as_str", "test_b_const_split"] {
        assert!(out.contains(&format!("t.main.{} ... ok (both: llvm)", name)), "{}", out);
    }
}

/// `String` の `==` / `!=` / `<` / `cmp` は `test --both` で LLVM と一致する (`<` / `cmp` は 2026-11-14 の kir/string-keys からネイティブ)。
#[test]
fn string_eq_ord_run_and_native_eq() {
    let eq = "test t.main.test_string_eq
    let a = alloc.string.String.from_str(std.alloc.SYSTEM, \"abc\").unwrap()
    let b = alloc.string.String.from_str(std.alloc.SYSTEM, \"abc\").unwrap()
    let c = alloc.string.String.from_str(std.alloc.SYSTEM, \"abd\").unwrap()
    core.check.is_true(a == b)
    core.check.is_true(a != c)
    core.check.is_true(!(a == c))
end t.main.test_string_eq
";
    let src = format!("{}\n{}", main_wrap("", ""), eq);
    let t = Tmp::new("string_eq_both", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_string_eq ... ok (both: llvm)"), "{}", out);
    let ord = "test t.main.test_string_ord
    let a = alloc.string.String.from_str(std.alloc.SYSTEM, \"abc\").unwrap()
    let c = alloc.string.String.from_str(std.alloc.SYSTEM, \"abd\").unwrap()
    core.check.is_true(a < c)
    core.check.is_true(a.cmp(&c) == core.cmp.Ordering.Less)
end t.main.test_string_ord
";
    let src = format!("{}\n{}", main_wrap("", ""), ord);
    let t = Tmp::new("string_ord_both", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_string_ord ... ok (both: llvm)"), "{}", out);
}

#[test]
fn every_subcommand_answers_help_without_side_effects() {
    let d = std::env::temp_dir().join(format!("ksn_test_{}_help", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    for cmd in ["new", "check", "run", "test", "build", "fmt", "fix", "explain", "doc", "lock"] {
        let o = run(&[cmd, "--help"], &d);
        assert_eq!(o.status.code(), Some(0), "{}", cmd);
        let t = text(&o.stdout);
        assert!(t.contains(&format!("usage: ksn {}", cmd)) && t.contains("example:"), "{}: {}", cmd, t);
        assert_eq!(text(&run(&["help", cmd], &d).stdout), t);
        assert_eq!(text(&run(&[cmd, "-h"], &d).stdout), t);
    }
    assert!(!d.join("--help").exists());
    assert_eq!(run(&["help"], &d).status.code(), Some(0));
    assert_eq!(run(&["help", "nope"], &d).status.code(), Some(2));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn package_commands_work_from_a_subdirectory_and_report_a_missing_package() {
    let t = Tmp::new("beta_cli_sub", "fn t.main() -> ()\nend t.main\n");
    let sub = t.0.join("src");
    let a = run(&["fmt", "--check"], &t.0);
    let b = run(&["fmt", "--check"], &sub);
    assert_eq!(a.status.code(), b.status.code());
    assert_eq!(text(&a.stdout), text(&b.stdout));
    let none = std::env::temp_dir().join(format!("ksn_test_{}_nopkg", std::process::id()));
    let _ = fs::remove_dir_all(&none);
    fs::create_dir_all(&none).unwrap();
    let o = run(&["check"], &none);
    let all = format!("{}{}", text(&o.stdout), text(&o.stderr));
    assert!(all.contains("E9001") && all.contains("no kasane.toml in this directory or any parent") && all.contains("ksn new <name>"), "{}", all);
    let _ = fs::remove_dir_all(&none);
}

#[test]
fn build_ends_with_the_produced_path_line() {
    let t = Tmp::new("beta_cli_build", "fn t.main() -> ()\nend t.main\n");
    let o = run_with_env(&["build"], &t.0, &[("KSN_CACHE_DIR", &t.0.join("cache").to_string_lossy())]);
    let out = text(&o.stdout);
    let last = out.lines().last().unwrap_or("");
    assert!(last.starts_with("built: target/debug/") || last.starts_with("built: target/"), "{}", out);
}
