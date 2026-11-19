//! 段階 1 の下ろしの穴 (その 2): `String.replace` (Kasane 版の本体)、局所の `s.chars()` の反復子、`char` のリテラルパターン、
//! ループで受け渡す `Vec<String>` の局所、`checked_neg`、完全修飾の数値・スライスのメソッド呼び出し (`core.num.f64.sin(x)` など)。
//! 木走査 (`ksn run`) と LLVM (debug / release) の出力が一致し、release がバンドルへ退避しないことを確かめる。

mod common;
use common::*;

fn assert_native_release(name: &str, src: &str) {
    let (_t, exe, out) = build_native_strict(&format!("{}_strict", name), src, true);
    assert!(exe.is_some(), "release のネイティブ生成が失敗した: {}", out);
}

const SHOW: &str = r#"
fn t.p_u64(x: u64) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &x))
    std.io.print_line(s.as_str())?
    Ok(())
end t.p_u64
"#;

#[test]
fn native_string_replace_matches_interpreter() {
    let src = format!(
        r#"fn t.rep(text: &str, from: &str, to: &str) -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    match alloc.string.String.replace(std.alloc.SYSTEM, text, from, to) {{
        Ok(r) => std.io.print_line(r.as_str())?
        Err(_e) => std.io.print_line("err")?
    }}
    Ok(())
end t.rep

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    t.rep("a\r\nb\r\n", "\r\n", "\n")?
    t.rep("aaa", "aa", "b")?
    t.rep("aaaa", "aa", "")?
    t.rep("abc", "", "X")?
    t.rep("", "a", "X")?
    t.rep("", "", "X")?
    t.rep("abc", "abcd", "X")?
    t.rep("abc", "abc", "XYZ")?
    t.rep("xabcabc", "abc", "-")?
    t.rep("héllo wörld", "ö", "oo")?
    t.rep("日本語日本", "日本", "ja")?
    t.rep("ab", "b", "bb")?
    Ok(())
end t.main
{SHOW}"#
    );
    let out = agree_with_interpreter("native_replace", &src);
    assert!(out.contains("jaja") || out.contains("ja語ja"), "{}", out);
    assert_native_release("native_replace", &src);
}

#[test]
fn native_local_chars_iterator_and_char_patterns_match_interpreter() {
    let src = format!(
        r#"fn t.digits(body: &str) -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut it = body.chars()
    let mut int_part = 0u64
    let mut frac = 0u64
    loop {{
        match it.next() {{
            Some('.') => break
            Some(c) => {{
                let d = u64.from_u32(c.to_u32())
                int_part = int_part.wrapping_mul(10u64).wrapping_add(d)
            }}
            None => break
        }}
    }}
    loop {{
        match it.next() {{
            Some(c) => {{
                frac = frac.wrapping_add(u64.from_u32(c.to_u32()))
            }}
            None => break
        }}
    }}
    t.p_u64(int_part)?
    t.p_u64(frac)?
    Ok(())
end t.digits

fn t.codes(s: &str) -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut it = s.chars()
    let mut n = 0u64
    let mut sum = 0u64
    loop {{
        match it.next() {{
            Some(c) => {{
                n = n.wrapping_add(1u64)
                sum = sum.wrapping_add(u64.from_u32(c.to_u32()))
            }}
            None => break
        }}
    }}
    t.p_u64(n)?
    t.p_u64(sum)?
    Ok(())
end t.codes

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    t.codes("")?
    t.codes("abc")?
    t.codes("é")?
    t.codes("日本語 a")?
    t.codes("😀x")?
    t.digits("12.5")?
    t.digits("7")?
    t.digits(".")?
    Ok(())
end t.main
{SHOW}"#
    );
    let out = agree_with_interpreter("native_chars_iter", &src);
    assert!(out.starts_with("0\n0\n3\n294\n"), "{}", out);
    assert_native_release("native_chars_iter", &src);
}

#[test]
fn native_vec_of_strings_carried_through_loops_matches_interpreter() {
    let src = format!(
        r#"type t.Row
    fields: alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>
end t.Row

fn t.build(text: &str) -> Result<usize, alloc.alloc.AllocError> uses [alloc] panics never
    let mut rows = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut field = alloc.string.String.new(std.alloc.SYSTEM)
    let mut row = alloc.vec.Vec.new(std.alloc.SYSTEM)
    for c in text.chars() {{
        if c == ',' {{
            core.mem.drop(row.push(field)?)
            field = alloc.string.String.new(std.alloc.SYSTEM)
        }} else if c == '\n' {{
            core.mem.drop(row.push(field)?)
            field = alloc.string.String.new(std.alloc.SYSTEM)
            core.mem.drop(rows.push(t.Row {{ fields: row }})?)
            row = alloc.vec.Vec.new(std.alloc.SYSTEM)
        }} else {{
            core.mem.drop(field.push(c)?)
        }}
    }}
    Ok(rows.len())
end t.build

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    match t.build("a,b\nc,d\n\n") {{
        Ok(n) => t.p_u64(u64.from_usize(n))?
        Err(_e) => std.io.print_line("err")?
    }}
    match t.build("") {{
        Ok(n) => t.p_u64(u64.from_usize(n))?
        Err(_e) => std.io.print_line("err")?
    }}
    Ok(())
end t.main
{SHOW}"#
    );
    let out = agree_with_interpreter("native_vec_loop_carry", &src);
    assert_eq!(out, "3\n0\n");
    assert_native_release("native_vec_loop_carry", &src);
}

#[test]
fn native_checked_neg_matches_interpreter() {
    let src = format!(
        r#"fn t.show(v: Option<i64>) -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    match v {{
        Some(x) => {{
            let mut s = alloc.string.String.new(std.alloc.SYSTEM)
            core.mem.drop(alloc.fmt.write(&mut s, &x))
            std.io.print_line(s.as_str())?
        }}
        None => std.io.print_line("none")?
    }}
    Ok(())
end t.show

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    match core.num.i8.MIN.checked_neg() {{
        Some(_) => std.io.print_line("i8 some")?
        None => std.io.print_line("i8 none")?
    }}
    match 5i32.checked_neg() {{
        Some(v) => core.check.eq(v, 0i32 - 5i32)
        None => std.io.print_line("bad")?
    }}
    t.show(core.num.i64.MIN.checked_neg())?
    t.show((0i64 - 9i64).checked_neg())?
    match 0u32.checked_neg() {{
        Some(v) => core.check.eq(v, 0u32)
        None => std.io.print_line("bad")?
    }}
    match 7u32.checked_neg() {{
        Some(_) => std.io.print_line("u32 some")?
        None => std.io.print_line("u32 none")?
    }}
    match core.num.u8.MAX.checked_neg() {{
        Some(_) => std.io.print_line("u8 some")?
        None => std.io.print_line("u8 none")?
    }}
    Ok(())
end t.main
"#
    );
    let out = agree_with_interpreter("native_checked_neg", &src);
    assert_eq!(out, "i8 none\nnone\n9\nu32 none\nu8 none\n");
    assert_native_release("native_checked_neg", &src);
}

#[test]
fn native_fully_qualified_numeric_slice_and_char_calls_match_interpreter() {
    let src = format!(
        r#"fn t.bits(x: f64) -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    t.p_u64(x.to_bits())?
    Ok(())
end t.bits

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    t.bits(core.num.f64.sin(0.5f64))?
    t.bits(core.num.f64.cos(0.5f64))?
    t.bits(core.num.f64.exp(1.0f64))?
    t.bits(core.num.f64.ln(2.0f64))?
    t.bits(core.num.f64.atan2(1.0f64, 2.0f64))?
    t.bits(core.num.f64.powi(1.5f64, 3i32))?
    t.bits(core.num.f64.sqrt(2.0f64))?
    t.bits(core.num.f64.max(core.num.f64.NAN, 1.0f64))?
    match core.num.u32.checked_shl(1u32, 33u32) {{
        Some(_) => std.io.print_line("some")?
        None => std.io.print_line("none")?
    }}
    match core.num.i32.checked_shl(1i32, 31u32) {{
        Some(v) => core.check.eq(v, core.num.i32.MIN)
        None => std.io.print_line("none")?
    }}
    match core.num.f64.trunc_to_i64(9223372036854775808.0f64) {{
        Some(_) => std.io.print_line("some")?
        None => std.io.print_line("trunc none")?
    }}
    match core.num.f64.trunc_to_i64(0.0f64 - 9223372036854775808.0f64) {{
        Some(v) => core.check.eq(v, core.num.i64.MIN)
        None => std.io.print_line("none")?
    }}
    let cps = [65u32, 55295u32, 55296u32, 57343u32, 57344u32, 1114111u32, 1114112u32]
    for cp in cps {{
        match core.char.char.try_from_u32(cp) {{
            Some(c) => t.p_u64(u64.from_u32(c.to_u32()))?
            None => std.io.print_line("char none")?
        }}
    }}
    let a = [1u8, 2u8, 3u8]
    match core.slice.get(&a, 5usize) {{
        Some(_) => std.io.print_line("some")?
        None => std.io.print_line("get none")?
    }}
    match core.slice.get(&a, 2usize) {{
        Some(c) => t.p_u64(u64.from_u8(*c))?
        None => std.io.print_line("get none")?
    }}
    match core.slice.sub(&a, 1usize, 5usize) {{
        Some(_) => std.io.print_line("some")?
        None => std.io.print_line("sub none")?
    }}
    match core.slice.sub(&a, 1usize, 3usize) {{
        Some(r) => t.p_u64(u64.from_usize(r.len()))?
        None => std.io.print_line("sub none")?
    }}
    Ok(())
end t.main
{SHOW}"#
    );
    let out = agree_with_interpreter("native_fqn_calls", &src);
    assert!(out.contains("trunc none"), "{}", out);
    assert!(out.contains("get none") && out.contains("sub none"), "{}", out);
    assert_native_release("native_fqn_calls", &src);
}
