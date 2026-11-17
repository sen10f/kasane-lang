//! 段階 1 の下ろしの穴 (`checked_*` / `saturating_*` / `trunc_to_*`、定数の受け手、`from_utf8` の配列、`Vec.with_capacity` / `sub`、
//! 所有物の `Vec` の `for`) を埋めたことの確認。木走査 (`ksn run`) と LLVM (debug / release) の出力が一致することを
//! `agree_with_interpreter` で確かめる。

mod common;
use common::*;

/// release のネイティブ生成が、バンドルへ退避せず LLVM で成功すること (E9040 にならない)。
fn assert_native_release(name: &str, src: &str) {
    let (_t, exe, out) = build_native_strict(&format!("{}_strict", name), src, true);
    assert!(exe.is_some(), "release のネイティブ生成が失敗した: {}", out);
}

/// the checked_* / saturating_* / wrapping_neg / trunc_to_* families, including path-constant receivers (core.num.i64.MIN.checked_div(-1)); boundaries: i64::MIN / -1, shift 64, NaN, inf, 2^63, 2^64。
#[test]
fn native_checked_trunc_and_const_receivers_match_interpreter() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    t.show_opt_i64(core.num.i64.MIN.checked_div(-1i64))?
    t.show_opt_i64(core.num.i64.MIN.checked_rem(-1i64))?
    t.show_opt_i64(core.num.i64.MAX.checked_div(-1i64))?
    t.show_opt_i64(7i64.checked_div(0i64))?
    t.show_opt_i64(core.num.i64.MIN.checked_abs())?
    t.show_opt_i64((0i64 - 5i64).checked_abs())?
    t.show_opt_u64(1u64.checked_shl(64u32))?
    t.show_opt_u64(1u64.checked_shl(63u32))?
    t.show_opt_u64(core.num.u64.MAX.checked_shr(64u32))?
    t.show_opt_u64(core.num.u64.MAX.checked_shr(63u32))?
    let a = 200u8
    match a.checked_shl(8u32) {
        Some(_) => std.io.print_line("u8 some")?
        None => std.io.print_line("u8 none")?
    }
    match a.checked_shl(1u32) {
        Some(v) => std.io.print_line(t.tag_u8(v))?
        None => std.io.print_line("u8 none")?
    }
    let neg = 0i32 - 16i32
    match neg.checked_shr(2u32) {
        Some(v) => core.check.eq(v, 0i32 - 4i32)
        None => std.io.print_line("i32 none")?
    }
    core.check.eq(core.num.i32.MAX.saturating_add(1i32), core.num.i32.MAX)
    core.check.eq(core.num.i64.MIN.wrapping_neg(), core.num.i64.MIN)
    core.check.eq(core.num.i16.MIN.saturating_sub(1i16), core.num.i16.MIN)
    t.show_opt_i64(core.num.f64.INFINITY.trunc_to_i64())?
    t.show_opt_i64(core.num.f64.NEG_INFINITY.trunc_to_i64())?
    t.show_opt_i64(core.num.f64.NAN.trunc_to_i64())?
    t.show_opt_i64(9223372036854775807.0f64.trunc_to_i64())?
    t.show_opt_i64(9223372036854774784.0f64.trunc_to_i64())?
    t.show_opt_i64((0.0f64 - 9223372036854775808.0f64).trunc_to_i64())?
    t.show_opt_u64(18446744073709551615.0f64.trunc_to_u64())?
    t.show_opt_u64(18446744073709549568.0f64.trunc_to_u64())?
    t.show_opt_u64((0.0f64 - 0.5f64).trunc_to_u64())?
    t.show_opt_u64((0.0f64 - 1.0f64).trunc_to_u64())?
    core.check.eq(core.num.f64.NAN.trunc_to_u8(), None)
    core.check.eq(core.num.f32.INFINITY.trunc_to_i32(), None)
    core.check.eq(2147483520.0f32.trunc_to_i32(), Some(2147483520i32))
    core.check.eq(2147483648.0f32.trunc_to_i32(), None)
    core.check.eq(255.5f64.trunc_to_u8(), Some(255u8))
    core.check.eq((0.0f64 - 127.9f64).trunc_to_i8(), Some(0i8 - 127i8))
    core.check.eq((0.0f64 - 128.9f64).trunc_to_i8(), Some(core.num.i8.MIN))
    Ok(())
end t.main

fn t.show_opt_i64(v: Option<i64>) -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    match v {
        Some(x) => {
            let mut s = alloc.string.String.new(std.alloc.SYSTEM)
            core.mem.drop(alloc.fmt.write(&mut s, &x))
            std.io.print_line(s.as_str())?
        }
        None => std.io.print_line("none")?
    }
    Ok(())
end t.show_opt_i64

fn t.show_opt_u64(v: Option<u64>) -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    match v {
        Some(x) => {
            let mut s = alloc.string.String.new(std.alloc.SYSTEM)
            core.mem.drop(alloc.fmt.write(&mut s, &x))
            std.io.print_line(s.as_str())?
        }
        None => std.io.print_line("none")?
    }
    Ok(())
end t.show_opt_u64

fn t.tag_u8(v: u8) -> &'static str uses [] panics never
    match v {
        144u8 => "u8 144"
        _ => "u8 other"
    }
end t.tag_u8
"#;
    let out = agree_with_interpreter("native_checked_trunc_and_const_receivers", src);
    assert_native_release("native_checked_trunc_and_const_receivers", src);
    assert!(!out.is_empty(), "no output");
}

/// core.str.from_utf8(&[..]) / (&local_array)。
#[test]
fn native_from_utf8_of_array_literals_matches_interpreter() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never
    match core.str.from_utf8(&[0xffu8, 0xfeu8]) {
        Ok(_) => std.io.print_line("ok")?
        Err(_) => std.io.print_line("err")?
    }
    match core.str.from_utf8(&[0x68u8, 0x69u8]) {
        Ok(s) => std.io.print_line(s)?
        Err(_) => std.io.print_line("err")?
    }
    let a = [0x68u8, 0xc3u8]
    match core.str.from_utf8(&a) {
        Ok(s) => std.io.print_line(s)?
        Err(_) => std.io.print_line("err2")?
    }
    Ok(())
end t.main
"#;
    let out = agree_with_interpreter("native_from_utf8_of_array_literals_match", src);
    assert_native_release("native_from_utf8_of_array_literals_match", src);
    assert!(!out.is_empty(), "no output");
}

/// Vec.with_capacity(SYSTEM, usize::MAX) -> Err。
#[test]
fn native_vec_with_capacity_huge_is_err_like_interpreter() {
    let src = r#"fn t.huge_vec() -> Result<alloc.vec.Vec<u8, std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics never
    alloc.vec.Vec.with_capacity(std.alloc.SYSTEM, core.num.usize.MAX)
end t.huge_vec

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    match t.huge_vec() {
        Ok(v) => {
            core.mem.drop(v)
            std.io.print_line("ok")?
        }
        Err(_) => std.io.print_line("err")?
    }
    Ok(())
end t.main
"#;
    let out = agree_with_interpreter("native_vec_with_capacity_huge_is_err_lik", src);
    assert_native_release("native_vec_with_capacity_huge_is_err_lik", src);
    assert!(!out.is_empty(), "no output");
}

/// Vec.sub(0, 3) on len 2 -> None。
#[test]
fn native_vec_sub_out_of_range_and_reversed_match_interpreter() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(match v.push(1u8) {
        Ok(()) => {
        }
        Err(_) => {
        }
    })
    core.mem.drop(match v.push(2u8) {
        Ok(()) => {
        }
        Err(_) => {
        }
    })
    match v.sub(0usize, 3usize) {
        Some(_) => std.io.print_line("some")?
        None => std.io.print_line("none")?
    }
    core.mem.drop(v)
    Ok(())
end t.main
"#;
    let out = agree_with_interpreter("native_vec_sub_out_of_range_and_reversed", src);
    assert_native_release("native_vec_sub_out_of_range_and_reversed", src);
    assert!(!out.is_empty(), "no output");
}

/// Vec.sub(2, 1) -> None。
#[test]
fn native_vec_sub_reversed_matches_interpreter() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(match v.push(1u8) {
        Ok(()) => {
        }
        Err(_) => {
        }
    })
    core.mem.drop(match v.push(2u8) {
        Ok(()) => {
        }
        Err(_) => {
        }
    })
    match v.sub(2usize, 1usize) {
        Some(_) => std.io.print_line("some")?
        None => std.io.print_line("none")?
    }
    core.mem.drop(v)
    Ok(())
end t.main
"#;
    let out = agree_with_interpreter("native_vec_sub_reversed_matches_interpre", src);
    assert_native_release("native_vec_sub_reversed_matches_interpre", src);
    assert!(!out.is_empty(), "no output");
}

/// for x in self.items.iter()。
#[test]
fn native_for_over_field_vec_of_strings_matches_interpreter() {
    let src = r#"type t.Env
    items: alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>
end t.Env

fn t.count(self: &t.Env) -> usize uses [] panics never
    let mut n = 0usize
    for x in self.items.iter() {
        n = n.wrapping_add(x.len())
    }
    n
end t.count

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut items = alloc.vec.Vec.new(std.alloc.SYSTEM)
    match alloc.string.String.from_str(std.alloc.SYSTEM, "abc") {
        Ok(s) => {
            match items.push(s) {
                Ok(()) => {
                }
                Err(_) => {
                }
            }
        }
        Err(_) => {
        }
    }
    let env = t.Env { items: items }
    core.check.eq(t.count(&env), 3usize)
    std.io.print_line("d1 ok")?
    Ok(())
end t.main
"#;
    let out = agree_with_interpreter("native_for_over_field_vec_of_strings_mat", src);
    assert_native_release("native_for_over_field_vec_of_strings_mat", src);
    assert!(!out.is_empty(), "no output");
}

/// for s in v.iter() (v: &mut Vec<String>)。
#[test]
fn native_for_over_mut_param_vec_of_strings_matches_interpreter() {
    let src = r#"fn t.fill(v: &mut alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>) -> usize uses [] panics never
    let mut n = 0usize
    for s in v.iter() {
        n = n.wrapping_add(s.len())
    }
    n
end t.fill

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut items = alloc.vec.Vec.new(std.alloc.SYSTEM)
    match alloc.string.String.from_str(std.alloc.SYSTEM, "abcd") {
        Ok(s) => {
            match items.push(s) {
                Ok(()) => {
                }
                Err(_) => {
                }
            }
        }
        Err(_) => {
        }
    }
    core.check.eq(t.fill(&mut items), 4usize)
    std.io.print_line("d2 ok")?
    Ok(())
end t.main
"#;
    let out = agree_with_interpreter("native_for_over_mut_param_vec_of_strings", src);
    assert_native_release("native_for_over_mut_param_vec_of_strings", src);
    assert!(!out.is_empty(), "no output");
}

/// for a in v.as_slice().iter() on Vec<String>。
#[test]
fn native_for_over_as_slice_of_returned_vec_of_strings_matches_interpreter() {
    let src = r#"fn t.names() -> alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System> uses [alloc] panics may
    let mut items = alloc.vec.Vec.new(std.alloc.SYSTEM)
    match alloc.string.String.from_str(std.alloc.SYSTEM, "abc") {
        Ok(s) => {
            match items.push(s) {
                Ok(()) => {
                }
                Err(_) => {
                }
            }
        }
        Err(_) => {
        }
    }
    items
end t.names

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let v = t.names()
    let mut n = 0usize
    for a in v.as_slice().iter() {
        n = n.wrapping_add(a.as_str().len())
    }
    core.check.eq(n, 3usize)
    std.io.print_line("d7 ok")?
    Ok(())
end t.main
"#;
    let out = agree_with_interpreter("native_for_over_as_slice_of_returned_vec", src);
    assert_native_release("native_for_over_as_slice_of_returned_vec", src);
    assert!(!out.is_empty(), "no output");
}

/// `x.f.checked_*(..)` (フィールドの整数が受け手。`x` は `&構造体`): csvtool の `stats.sum.checked_div(stats.numeric)` の形。
#[test]
fn native_checked_ops_on_struct_field_receivers_match_interpreter() {
    let src = r#"type t.Stats
    sum: i64
    n: i64
end t.Stats

fn t.avg(s: &t.Stats) -> i64 uses [] panics may
    match s.sum.checked_div(s.n) {
        Some(v) => v
        None => 0i64 - 1i64
    }
end t.avg

fn t.neg(s: &t.Stats) -> i64 uses [] panics may
    match s.sum.checked_abs() {
        Some(v) => v
        None => 0i64 - 7i64
    }
end t.neg

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = t.Stats { sum: 10i64, n: 4i64 }
    let b = t.Stats { sum: core.num.i64.MIN, n: 0i64 - 1i64 }
    let c = t.Stats { sum: 5i64, n: 0i64 }
    core.check.eq(t.avg(&a), 2i64)
    core.check.eq(t.avg(&b), 0i64 - 1i64)
    core.check.eq(t.avg(&c), 0i64 - 1i64)
    core.check.eq(t.neg(&b), 0i64 - 7i64)
    core.check.eq(t.neg(&a), 10i64)
    std.io.print_line("fields ok")?
    Ok(())
end t.main
"#;
    let out = agree_with_interpreter("native_checked_fields", src);
    assert_eq!(out, "fields ok\n");
    assert_native_release("native_checked_fields", src);
}
