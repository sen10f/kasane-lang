//! `f32` / `f64` の `Display` / `Debug` の境界 (非正規化数の最小・最大、正規化数の最小、MAX、±0、NaN、無限、
//! 極端な指数、整数に近い値、丸めの境目)。木走査 (`ksn run`) と LLVM (debug / release) の出力が一致する。
//! 非正規化数の `{}` は指数を使わず 330 字ほどになる (`FloatText` の枠はそれが入る大きさ)。

mod common;
use common::*;

const SRC: &str = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    t.p64(0x0000000000000001u64)?
    t.p64(0x0000000000000002u64)?
    t.p64(0x000fffffffffffffu64)?
    t.p64(0x0010000000000000u64)?
    t.p64(0x7fefffffffffffffu64)?
    t.p64(0x0000000000000000u64)?
    t.p64(0x8000000000000000u64)?
    t.p64(0x8000000000000001u64)?
    t.p64(0x7ff8000000000000u64)?
    t.p64(0x7ff0000000000000u64)?
    t.p64(0xfff0000000000000u64)?
    t.p64(0x3ff0000000000000u64)?
    t.p64(0x4330000000000001u64)?
    t.p64(0x4341c37937e08000u64)?
    t.p64(0x3fb999999999999au64)?
    t.p64(0x3ff0000000000001u64)?
    t.p64(0x3fefffffffffffffu64)?
    t.p64(0x4340000000000000u64)?
    t.p64(0x7e37e43c8800759cu64)?
    t.p64(0x01a56e1fc2f8f359u64)?
    t.p64(0x3fe0000000000000u64)?
    t.p64(0x4024000000000000u64)?
    t.p64(0x3fd3333333333333u64)?
    t.p32(0x00000001u32)?
    t.p32(0x00000002u32)?
    t.p32(0x007fffffu32)?
    t.p32(0x00800000u32)?
    t.p32(0x7f7fffffu32)?
    t.p32(0x00000000u32)?
    t.p32(0x80000000u32)?
    t.p32(0x80000001u32)?
    t.p32(0x7fc00000u32)?
    t.p32(0x7f800000u32)?
    t.p32(0xff800000u32)?
    t.p32(0x3f800000u32)?
    t.p32(0x4b800001u32)?
    t.p32(0x3dcccccdu32)?
    t.p32(0x3f800001u32)?
    t.p32(0x4b000000u32)?
    t.p32(0x5a0e1b1fu32)?
    Ok(())
end t.main

fn t.p32(bits: u32) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let x = core.num.f32.from_bits(bits)
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &x))
    core.mem.drop(s.push_str("|"))
    core.mem.drop(alloc.fmt.debug(&mut s, &x))
    std.io.print_line(s.as_str())?
    Ok(())
end t.p32

fn t.p64(bits: u64) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let x = core.num.f64.from_bits(bits)
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &x))
    core.mem.drop(s.push_str("|"))
    core.mem.drop(alloc.fmt.debug(&mut s, &x))
    std.io.print_line(s.as_str())?
    Ok(())
end t.p64
"#;

#[test]
fn native_float_fmt_boundaries() {
    let out = agree_with_interpreter("float_fmt_bounds", SRC);
    assert_eq!(out.lines().count(), 40, "{}", out);
    // 最小の非正規化数は指数を使わずに 0.000…5。
    let first = out.lines().next().unwrap();
    assert!(first.starts_with("0.000") && first.contains('|'), "{}", first);
    assert!(first.len() > 600, "{}", first.len());
}

#[test]
fn native_float_fmt_release_strict() {
    let (_t, exe, out) = build_native_strict("float_fmt_strict", SRC, true);
    assert!(exe.is_some(), "release のネイティブ生成が失敗した: {}", out);
}

/// 指数の範囲全体: 2 進指数 0..2046 の境目 (`e<<52` とその直前・直後・仮数が全て 1)、10 の冪 `1e-308`〜`1e308` (10 倍ずつ)、
/// `f32` の同様の境目。出力は木走査と一致し、ネイティブ (debug / release) が落ちない。
#[test]
fn native_float_fmt_exponent_sweep() {
    let mut body = String::new();
    let mut e = 0u64;
    while e < 2047 {
        for bits in [e << 52, (e << 52).wrapping_sub(1) & 0x7FFF_FFFF_FFFF_FFFF, (e << 52) | 0xF_FFFF_FFFF_FFFF, (e << 52) | 0x1_2345_6789_ABCD] {
            body.push_str(&format!("    t.p64(0x{:016x}u64)?\n", bits));
        }
        e += if e < 60 || e > 1990 { 1 } else { 13 };
    }
    for k in -308i32..=308 {
        if k % 10 == 0 || k.abs() > 300 {
            let v: f64 = format!("1e{}", k).parse().unwrap();
            body.push_str(&format!("    t.p64(0x{:016x}u64)?\n", v.to_bits()));
        }
    }
    for e in 0u32..255 {
        for bits in [e << 23, (e << 23).wrapping_sub(1) & 0x7FFF_FFFF, (e << 23) | 0x7F_FFFF, (e << 23) | 0x12_3456] {
            body.push_str(&format!("    t.p32(0x{:08x}u32)?\n", bits));
        }
    }
    let (head, tail) = SRC.split_once("    t.p64(0x0000000000000001u64)?").unwrap();
    let rest = tail.split_once("    Ok(())\nend t.main\n").unwrap().1;
    let src = format!("{}{}    Ok(())\nend t.main\n{}", head, body, rest);
    let out = agree_with_interpreter("float_fmt_sweep", &src);
    assert!(out.lines().count() > 1000, "{}", out.lines().count());
    let (_t, exe, msg) = build_native_strict("float_fmt_sweep_strict", &src, true);
    assert!(exe.is_some(), "{}", msg);
}

/// スレッド 8 本が各 100 行を `std.io.print_line` で書いても、どの行も途中で混ざらない (順序は比べない)。
/// `print_line` は本文と改行を 1 回の書き込みで出す (`write.nl`)。
#[test]
fn native_print_line_threads_do_not_tear() {
    let src = r#"fn t.worker(id: u32) -> std.thread.JoinHandle<(), std.alloc.System> uses [alloc, io.console, thread] panics may
    let spawned = std.thread.spawn(
        std.alloc.SYSTEM,
        fn() -> () capture [move id] {
            let mut s = alloc.string.String.new(std.alloc.SYSTEM)
            core.mem.drop(s.push_str("w"))
            core.mem.drop(alloc.fmt.write(&mut s, &id))
            core.mem.drop(s.push_str("-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-end"))
            let mut i = 0u32
            while i < 100u32 {
                match std.io.print_line(s.as_str()) {
                    Ok(_) => ()
                    Err(_e) => core.panic.panic("print failed")
                }
                i = i + 1u32
            }
        },
    )
    match spawned {
        Ok(handle) => handle
        Err(_e) => core.panic.panic("spawn failed")
    }
end t.worker

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may
    let h0 = t.worker(0u32)
    let h1 = t.worker(1u32)
    let h2 = t.worker(2u32)
    let h3 = t.worker(3u32)
    let h4 = t.worker(4u32)
    let h5 = t.worker(5u32)
    let h6 = t.worker(6u32)
    let h7 = t.worker(7u32)
    h0.join()
    h1.join()
    h2.join()
    h3.join()
    h4.join()
    h5.join()
    h6.join()
    h7.join()
    Ok(())
end t.main
"#;
    let check = |out: &str, what: &str| {
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 800, "{}: {}", what, out.len());
        for id in 0..8 {
            let want = format!("w{}-{}-end", id, "a".repeat(96));
            let n = lines.iter().filter(|l| **l == want).count();
            assert_eq!(n, 100, "{}: w{} の行が壊れている", what, id);
        }
    };
    let t = Tmp::new("print_line_threads_interp", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    check(&text(&o.stdout), "interp");
    for release in [false, true] {
        let (bt, exe, out) = build_native(&format!("print_line_threads_{}", release), src, release);
        assert_native_or_reasoned_skip(&out);
        let p = exe.unwrap_or_else(|| panic!("no native executable: {}", out));
        for _ in 0..5 {
            let o = std::process::Command::new(&p).current_dir(&bt.0).output().unwrap();
            assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
            check(&text(&o.stdout), "native");
        }
    }
}
