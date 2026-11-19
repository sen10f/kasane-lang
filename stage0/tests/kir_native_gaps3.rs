//! 段階 1 の下ろしの穴 (その 3、穴探し a63): `alloc.fmt.write` の `char` / `f64` / `f32`、`core.iter.range` を局所に持つ `fold`、
//! `Option.map` / `and_then` / `unwrap_or` の完全修飾の呼び出し、`String` の受け手の `find` ほか、`char` の ASCII 判定と `len_utf8`、
//! 局所の `s.split(sep)` の `next`、`Vec.extend_from_slice` (`&str` の `try_clone`)。
//! 木走査 (`ksn run`) と LLVM (debug / release) の出力が一致し、release がバンドルへ退避しないことを確かめる。

mod common;
use common::*;

fn assert_native_release(name: &str, src: &str) {
    let (_t, exe, out) = build_native_strict(&format!("{}_strict", name), src, true);
    assert!(exe.is_some(), "release のネイティブ生成が失敗した: {}", out);
}

fn check(name: &str, src: &str) -> String {
    let out = agree_with_interpreter(name, src);
    assert_native_release(name, src);
    out
}

const SHOW: &str = r#"
fn t.line(s: &alloc.string.String<std.alloc.System>) -> Result<(), std.io.Error> uses [io.console] panics may
    std.io.print_line(s.as_str())?
    Ok(())
end t.line
"#;

#[test]
fn native_fmt_write_char_and_floats() {
    let src = format!(
        r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    let a = 'x'
    let b = 'あ'
    let c = '😀'
    core.mem.drop(alloc.fmt.write(&mut s, &a))
    core.mem.drop(alloc.fmt.write(&mut s, &b))
    core.mem.drop(alloc.fmt.write(&mut s, &c))
    core.mem.drop(s.push_str("|"))
    let f1 = 1.5f64
    let f2 = 0.0f64
    let f3 = 0.0f64 - 0.0f64
    let f4 = 1.0f64 / 3.0f64
    let f5 = 1.0e21f64
    let f6 = 1.0e-7f64
    let f7 = 123456789.125f64
    let f8 = 1.0f64 / 0.0f64
    let f9 = 0.0f64 / 0.0f64
    let f10 = 0.0f64 - 2.5f64
    core.mem.drop(alloc.fmt.write(&mut s, &f1))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(alloc.fmt.write(&mut s, &f2))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(alloc.fmt.write(&mut s, &f3))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(alloc.fmt.write(&mut s, &f4))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(alloc.fmt.write(&mut s, &f5))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(alloc.fmt.write(&mut s, &f6))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(alloc.fmt.write(&mut s, &f7))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(alloc.fmt.write(&mut s, &f8))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(alloc.fmt.write(&mut s, &f9))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(alloc.fmt.write(&mut s, &f10))
    core.mem.drop(s.push_str(" "))
    let g1 = 0.1f32
    let g2 = 16777216.0f32
    core.mem.drop(alloc.fmt.write(&mut s, &g1))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(alloc.fmt.write(&mut s, &g2))
    t.line(&s)?
    Ok(())
end t.main
{SHOW}"#
    );
    let out = check("gaps3_fmt", &src);
    assert!(out.contains("xあ😀|1.5 0.0 "), "{}", out);
}

#[test]
fn native_range_local_fold_count_and_option_combinators() {
    let src = format!(
        r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let factor = 3i32
    let r = core.iter.range(0i32, 5i32)
    let sum = core.iter.fold(r, 0i32, fn(acc: i32, x: i32) -> i32 capture [&factor] {{
        acc.wrapping_add(x.wrapping_mul(*factor))
    }})
    let e = core.iter.range(7u32, 7u32)
    let none = core.iter.fold(e, 9u32, fn(acc: u32, x: u32) -> u32 capture [] {{
        acc.wrapping_add(x)
    }})
    let mut n = 4u8
    let r2 = core.iter.range(0u8, n)
    n = 200u8
    let cnt = core.iter.count(r2)
    let o = Some(7i32)
    let mo = core.option.Option.map(o, fn(x: i32) -> i32 capture [] {{
        x.wrapping_mul(2)
    }})
    let m = core.option.Option.unwrap_or(mo, 0i32)
    let z: Option<i32> = None
    let mz = core.option.Option.map(z, fn(x: i32) -> i32 capture [] {{
        x.wrapping_mul(2)
    }})
    let m2 = core.option.Option.unwrap_or(mz, -1i32)
    let big = Some(2147483647i32)
    let ab = core.option.Option.and_then(big, fn(x: i32) -> Option<i32> capture [] {{
        if x > 5i32 {{
            return None
        }}
        Some(x)
    }})
    let a1 = core.option.Option.unwrap_or(ab, -5i32)
    let lt = Some(3i32)
    let al = core.option.Option.and_then(lt, fn(x: i32) -> Option<i32> capture [] {{
        Some(x.wrapping_add(1i32))
    }})
    let a2 = core.option.Option.unwrap_or(al, -5i32)
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_i64(i64.from_i32(sum)))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(s.push_u64(u64.from_u32(none)))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(s.push_u64(u64.from_usize(cnt)))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(s.push_i64(i64.from_i32(m)))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(s.push_i64(i64.from_i32(m2)))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(s.push_i64(i64.from_i32(a1)))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(s.push_i64(i64.from_i32(a2)))
    t.line(&s)?
    Ok(())
end t.main
{SHOW}"#
    );
    let out = check("gaps3_range_opt", &src);
    assert!(out.contains("30 9 "), "{}", out);
}

#[test]
fn native_string_receiver_str_methods_and_char_classes() {
    let src = format!(
        r#"fn t.probe(text: &str, pat: &str) -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    let owned = alloc.string.String.replace(std.alloc.SYSTEM, text, "", "")
    match owned {{
        Ok(r) => {{
            match r.find(pat) {{
                Some(p) => core.mem.drop(s.push_u64(u64.from_usize(p)))
                None => core.mem.drop(s.push_str("none"))
            }}
            core.mem.drop(s.push_str(" "))
            match r.rfind(pat) {{
                Some(p) => core.mem.drop(s.push_u64(u64.from_usize(p)))
                None => core.mem.drop(s.push_str("none"))
            }}
            core.mem.drop(s.push_str(" "))
            match r.split_once(pat) {{
                Some((h, tl)) => {{
                    core.mem.drop(s.push_str("["))
                    core.mem.drop(s.push_str(h))
                    core.mem.drop(s.push_str("|"))
                    core.mem.drop(s.push_str(tl))
                    core.mem.drop(s.push_str("]"))
                }}
                None => core.mem.drop(s.push_str("nosplit"))
            }}
            core.mem.drop(s.push_str(" "))
            match r.strip_prefix(pat) {{
                Some(x) => core.mem.drop(s.push_str(x))
                None => core.mem.drop(s.push_str("noprefix"))
            }}
            core.mem.drop(s.push_str(" <"))
            core.mem.drop(s.push_str(r.trim()))
            core.mem.drop(s.push_str(">"))
        }}
        Err(_e) => core.mem.drop(s.push_str("err"))
    }}
    std.io.print_line(s.as_str())?
    Ok(())
end t.probe

fn t.classes(text: &str) -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut a = 0u64
    let mut d = 0u64
    let mut w = 0u64
    let mut bytes = 0u64
    for c in text.chars() {{
        if c.is_ascii_alphabetic() {{
            a = a + 1u64
        }}
        if c.is_ascii_digit() {{
            d = d + 1u64
        }}
        if c.is_ascii_whitespace() {{
            w = w + 1u64
        }}
        bytes = bytes + u64.from_usize(c.len_utf8())
    }}
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_u64(a))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(s.push_u64(d))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(s.push_u64(w))
    core.mem.drop(s.push_str(" "))
    core.mem.drop(s.push_u64(bytes))
    std.io.print_line(s.as_str())?
    Ok(())
end t.classes

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    t.probe("a,b;c,d", ",")?
    t.probe("  x  ", "x")?
    t.probe("abc", "")?
    t.probe("", "a")?
    t.probe("héllo", "l")?
    t.classes("aZ09 @[`{{\t\n\rあé😀z")?
    t.classes("")?
    t.classes("/:`{{@[")?
    Ok(())
end t.main
{SHOW}"#
    );
    let out = check("gaps3_str_char", &src);
    assert!(out.contains("1 5 [a|b;c,d]"), "{}", out);
}

#[test]
fn native_local_split_next_and_user_function() {
    let src = format!(
        r#"fn t.first_word(text: &str) -> &str uses [] panics never
    let mut parts = text.split(" ")
    match parts.next() {{
        Some(w) => w
        None => ""
    }}
end t.first_word

fn t.pieces(text: &str, sep: &str) -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut it = text.split(sep)
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    let mut go = true
    let mut n = 0usize
    while go {{
        match it.next() {{
            Some(p) => {{
                core.mem.drop(s.push_str("<"))
                core.mem.drop(s.push_str(p))
                core.mem.drop(s.push_str(">"))
                n = n + 1usize
            }}
            None => {{
                go = false
            }}
        }}
    }}
    core.mem.drop(s.push_str(" "))
    core.mem.drop(s.push_u64(u64.from_usize(n)))
    std.io.print_line(s.as_str())?
    Ok(())
end t.pieces

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    std.io.print_line(t.first_word("hello world"))?
    std.io.print_line(t.first_word(""))?
    std.io.print_line(t.first_word(" lead"))?
    t.pieces("a,b,c", ",")?
    t.pieces("a,", ",")?
    t.pieces(",a", ",")?
    t.pieces(",", ",")?
    t.pieces("", ",")?
    t.pieces("abc", "")?
    t.pieces("a::b::::c", "::")?
    t.pieces("日本,語", ",")?
    t.pieces("aaa", "aa")?
    Ok(())
end t.main
"#
    );
    let out = check("gaps3_split", &src);
    assert!(out.contains("<a><b><c> 3"), "{}", out);
}

#[test]
fn native_vec_extend_from_slice_str_and_ints() {
    let src = format!(
        r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push("a"))
    core.mem.drop(v.push("b"))
    let mut v2 = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v2.push("c"))
    core.mem.drop(v2.push("d"))
    core.mem.drop(v.extend_from_slice(v2.as_slice()))
    let mut out = alloc.string.String.new(std.alloc.SYSTEM)
    let mut i = 0usize
    while i < v.len() {{
        match v.as_slice().get(i) {{
            Some(r) => {{
                core.mem.drop(out.push_str(*r))
                core.mem.drop(out.push_str(","))
            }}
            None => core.mem.drop(0u8)
        }}
        i = i + 1usize
    }}
    let mut ns = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(ns.push(1u32))
    let mut ns2 = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(ns2.push(2u32))
    core.mem.drop(ns2.push(3u32))
    core.mem.drop(ns.extend_from_slice(ns2.as_slice()))
    core.mem.drop(out.push_u64(u64.from_usize(ns.len())))
    t.line(&out)?
    Ok(())
end t.main
{SHOW}"#
    );
    let out = check("gaps3_extend", &src);
    assert!(out.contains("a,b,c,d,"), "{}", out);
}
