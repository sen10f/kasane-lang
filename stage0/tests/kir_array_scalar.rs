//! 配列リテラル `[e, ...]` と `[e; N]` の要素が `f32`・`f64`・`bool`・`char`・`i128` のとき (G41)。
//! 以前は `coll::size_of` が浮動小数と 128 ビット整数を知らず、`kir_lower_array` の `expect` が
//! 内部 panic (`ksn check` が終了コード 2) になっていた。ここでは、長さ・添字・合計が木走査 (`ksn run`)、
//! 機械語 (`kir --native`、ASan 付き含む)、ネイティブの release / debug で一致することを確かめる。
//! 溢れる大きさ (`[x; N]` の領域が u32 に入らない) は内部 panic ではなく、`check` は通り、`build --release` は E9040 で止まる。

mod common;
use common::*;

const SCALARS: &str = r#"fn t.f64_sum() -> f64 uses [] panics never
    let xs = [1.5f64, 2.25f64, -0.75f64]
    let mut s = 0.0f64
    for x in xs {
        s = s + x
    }
    s
end t.f64_sum

fn t.f64_index() -> f64 uses [] panics may
    let xs = [1.5f64, 2.25f64, -0.75f64]
    xs[1usize]
end t.f64_index

fn t.f64_len() -> usize uses [] panics never
    let xs = [1.0f64, 2.0f64]
    xs.len()
end t.f64_len

fn t.f32_sum() -> f32 uses [] panics never
    let xs = [1.5f32, 2.25f32, -0.75f32]
    let mut s = 0.0f32
    for x in xs {
        s = s + x
    }
    s
end t.f32_sum

fn t.bool_false_count() -> usize uses [] panics never
    let bs = [true, false, true]
    let mut n = 0usize
    for b in bs {
        if !b {
            n = n.wrapping_add(1usize)
        }
    }
    n
end t.bool_false_count

fn t.char_count() -> usize uses [] panics never
    let cs = ['a', 'b', 'c']
    let mut n = 0usize
    for c in cs {
        if c != 'b' {
            n = n.wrapping_add(1usize)
        }
    }
    n
end t.char_count

fn t.i128_sum() -> i128 uses [] panics never
    let xs = [1i128, -2i128, 3i128]
    let mut s = 0i128
    for x in xs {
        s = s.wrapping_add(x)
    }
    s
end t.i128_sum

fn t.rep_f64() -> f64 uses [] panics never
    let ys = [0.5f64; 4]
    let mut s = 0.0f64
    for y in ys {
        s = s + y
    }
    s
end t.rep_f64

fn t.rep_f32_len() -> usize uses [] panics never
    let ys = [2.0f32; 3]
    ys.len()
end t.rep_f32_len

fn t.rep_u128_last() -> u128 uses [] panics may
    let zs = [7u128; 5]
    zs[4usize]
end t.rep_u128_last

fn t.slice_sum(xs: &[f64]) -> f64 uses [] panics never
    let mut s = 0.0f64
    for x in xs {
        s = s + *x
    }
    s
end t.slice_sum

fn t.vec_sum() -> f64 uses [alloc] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(1.5f64))
    core.mem.drop(v.push(2.25f64))
    let mut s = 0.0f64
    for x in v.as_slice() {
        s = s + *x
    }
    s
end t.vec_sum

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.f64_sum(), 3.0f64)
    core.check.eq(t.f64_index(), 2.25f64)
    core.check.eq(t.f64_len(), 2usize)
    core.check.eq(t.f32_sum(), 3.0f32)
    core.check.eq(t.bool_false_count(), 1usize)
    core.check.eq(t.char_count(), 2usize)
    core.check.eq(t.i128_sum() == 2i128, true)
    core.check.eq(t.rep_f64(), 2.0f64)
    core.check.eq(t.rep_f32_len(), 3usize)
    core.check.eq(t.rep_u128_last() == 7u128, true)
    core.check.eq(t.slice_sum(&[0.25f64, 0.5f64]), 0.75f64)
    core.check.eq(t.vec_sum(), 3.75f64)
    std.io.print_line("scalar arrays ok")
end t.main
"#;

/// 値を返す関数の結果 (`kir --run` の表示。浮動小数は Rust の `{}` 表記で、整数値は小数点なし) を `kir --native` と比べて確かめる。
const VALUES: [(&str, &str); 8] = [
    ("t.f64_sum", "3"),
    ("t.f64_index", "2.25"),
    ("t.f32_sum", "3"),
    ("t.bool_false_count", "1"),
    ("t.char_count", "2"),
    ("t.i128_sum", "2"),
    ("t.rep_f64", "2"),
    ("t.rep_u128_last", "7"),
];

fn scalar_tmp(name: &str) -> Tmp {
    Tmp::new(name, SCALARS)
}

/// `ksn run` (木走査) の表示と、ネイティブの release / debug の実行ファイルの表示が一致する。
#[test]
fn scalar_array_literals_native_and_run_agree() {
    let t = scalar_tmp("kir_array_scalar_run");
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "run: {}\n{}", text(&interp.stderr), text(&interp.stdout));
    assert_eq!(text(&interp.stdout), "scalar arrays ok\n");
    for release in [false, true] {
        let (bt, exe, out) = build_native_strict(&format!("kir_array_scalar_{}", release), SCALARS, release);
        let p = exe.unwrap_or_else(|| panic!("ネイティブの実行ファイルが無い (release: {}): {}", release, out));
        let o = std::process::Command::new(&p).current_dir(&bt.0).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "release {}: {}{}", release, text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), text(&interp.stdout), "native (release: {}) と run の表示が違う", release);
    }
}

/// 値の結果を `kir --run` と `kir --native` (ASan 付き含む) で比べる。
#[test]
fn scalar_array_values_match_across_kir_engines() {
    let t = scalar_tmp("kir_array_scalar_values");
    for (f, want) in VALUES {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}: {}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
}

/// 下ろせない形は内部 panic で落ちず、理由付きの診断で止まる。`check` は内部 panic (終了コード 2) を出さない。
#[test]
fn scalar_array_rep_overflow_is_a_diagnostic_not_a_panic() {
    let huge = "fn t.main() -> () uses [] panics never\n    let xs = [0u64; 1000000000000]\n    core.mem.drop(xs.len())\nend t.main\n";
    let t = Tmp::new("kir_array_scalar_huge", huge);
    let c = run(&["check"], &t.0);
    assert_ne!(c.status.code(), Some(2), "check が内部 panic: {}", text(&c.stderr));
    assert!(!text(&c.stderr).contains("panicked"), "check が panic した: {}", text(&c.stderr));
    let b = run(&["build", "--release"], &t.0);
    let out = text(&b.stdout) + &text(&b.stderr);
    assert!(!out.contains("panicked"), "build が panic した: {}", out);
    assert_ne!(b.status.code(), Some(0), "溢れる配列の release ビルドは成功してはいけない: {}", out);
    assert!(out.contains("配列が大きすぎる"), "理由が出ていない: {}", out);
}

/// 要素数 2^32 (大きさが u32 に入らない) の `[x; N]` も同じ扱い。
#[test]
fn scalar_array_rep_u32_overflow_is_refused() {
    let src = "fn t.main() -> () uses [] panics never\n    let xs = [0.5f64; 4294967296]\n    core.mem.drop(xs.len())\nend t.main\n";
    let t = Tmp::new("kir_array_scalar_u32", src);
    let c = run(&["check"], &t.0);
    assert!(!text(&c.stderr).contains("panicked"), "check が panic した: {}", text(&c.stderr));
    let b = run(&["build", "--release"], &t.0);
    let out = text(&b.stdout) + &text(&b.stderr);
    assert!(!out.contains("panicked"), "build が panic した: {}", out);
    assert!(out.contains("配列が大きすぎる"), "理由が出ていない: {}", out);
}

/// 再現の形 (`f64` の配列リテラルを `xs.len()` で使う) は `ksn check` を通る。
#[test]
fn scalar_array_repro_checks_clean() {
    let src = "fn t.main() -> () uses [] panics never\n    let xs = [1.0f64, 2.0f64]\n    core.mem.drop(xs.len())\nend t.main\n";
    let t = Tmp::new("kir_array_scalar_repro", src);
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "check: {}{}", text(&c.stdout), text(&c.stderr));
}
