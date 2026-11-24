//! 2026-11-20 S5 計器の続き (gap 8)。定数の型の拡張 (浮動小数・タプル・非整数欄の構造体の定数を
//! 使う場所で下ろす) と、固定長配列の `as_mut_slice` / `as_slice` をネイティブに下ろす。
//! ネイティブ (debug/release) が `ksn run` と同じ表示を出すことを確かめる。退避は許さない。

mod common;

use common::{build_native_strict, run, text, Tmp};

/// `ksn run` の出力と、ネイティブ (debug/release) の実行ファイルの出力が一致することを確かめる。
fn check_gap(name: &str, src: &str, want: &str) {
    let t = Tmp::new(name, src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "run: {}{}", text(&interp.stderr), text(&interp.stdout));
    assert_eq!(text(&interp.stdout), want, "run の表示が想定と違う");
    for release in [false, true] {
        let (bt, exe, out) = build_native_strict(&format!("{}_{}", name, release), src, release);
        let p = exe.unwrap_or_else(|| panic!("ネイティブの実行ファイルが無い (release: {}): {}", release, out));
        let o = std::process::Command::new(&p).current_dir(&bt.0).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "release {}: {}{}", release, text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), want, "native (release: {}) と run の表示が違う", release);
    }
}

/// 8-1. 浮動小数・タプルの定数を使う。
#[test]
fn const_float_and_tuple_native() {
    check_gap(
        "ngaps8_const_float_tuple",
        r#"const t.H: f32 = 36.0f32
const t.P: (u8, i32) = (1u8, 2i32)

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let (a, b) = t.P
    let ok = t.H > 1.0f32 && a == 1u8 && b == 2i32
    if ok {
        match std.io.print_line("ok") {
            Ok(_x) => Ok(())
            Err(_e) => core.panic.panic("io")
        }
    } else {
        core.panic.panic("const")
    }
end t.main
"#,
        "ok\n",
    );
}

/// 8-2. `[e; N]` の局所の `as_mut_slice()` を `&mut [T]` の引数に渡し、要素を書き換えて読む。
#[test]
fn array_rep_as_mut_slice_native() {
    check_gap(
        "ngaps8_array_rep_as_mut_slice",
        r#"fn t.sum(s: &mut [u32]) -> u32 uses [] panics may
    let mut total = 0u32
    let mut i = 0usize
    while i < s.len() {
        total = total + s[i]
        i = i + 1usize
    }
    total
end t.sum

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut a = [0u32; 8]
    let v = a.as_mut_slice()
    v[1usize] = 5u32
    let n = t.sum(a.as_mut_slice())
    if n == 5u32 {
        match std.io.print_line("ok") {
            Ok(_x) => Ok(())
            Err(_e) => core.panic.panic("io")
        }
    } else {
        core.panic.panic("sum")
    }
end t.main
"#,
        "ok\n",
    );
}

/// 8-3. 依存パッケージの `extern` (`ui.gpu.raw_ksnui_*` と同じ形): 構造体の値渡し・値返し (小さいものと 24 バイトの sret)・
/// 構造体へのポインタ渡しを、C の共有ライブラリへ呼ぶ。インタプリタとネイティブ (debug/release) が一致し、退避は無い。
#[test]
fn extern_with_layout_c_structs_by_value_and_pointer_native() {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) || !(std::process::Command::new("cc").arg("--version").output().is_ok()) {
        return;
    }
    let t = Tmp::new("ngaps8_extern_agg", "");
    std::fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", common::TOML)).unwrap();
    let c = t.0.join("agg.c");
    std::fs::write(
        &c,
        "typedef struct { int x; int y; } Pt;\n\
         typedef struct { double a, b, c; } Big;\n\
         int sum_pt(Pt p) { return p.x + p.y; }\n\
         Pt mk_pt(int x, int y) { Pt p = {x, y}; return p; }\n\
         Big mk_big(double s) { Big b = {s, s * 2.0, s * 3.0}; return b; }\n\
         double big_sum(Big b) { return b.a + b.b + b.c; }\n\
         int sum_ptr(const Pt *p, int n) { int t = 0; for (int i = 0; i < n; i++) t += p[i].x * 10 + p[i].y; return t; }\n",
    )
    .unwrap();
    let lib = t.0.join("libagg.so");
    let o = std::process::Command::new("cc")
        .args(["-shared", "-fPIC", "-O1", "-o", lib.to_str().unwrap(), c.to_str().unwrap()])
        .output()
        .expect("spawn cc");
    assert!(o.status.success(), "cc: {}", text(&o.stderr));
    let lp = lib.to_str().unwrap();
    let ksn = r#"type t.Pt layout c derive [Clone, Copy]
    x: i32
    y: i32
end t.Pt

type t.Big layout c derive [Clone, Copy]
    a: f64
    b: f64
    c: f64
end t.Big

extern t.sum_pt(p: t.Pt) -> i32 link "LIBPATH:sum_pt"
extern t.mk_pt(x: i32, y: i32) -> t.Pt link "LIBPATH:mk_pt"
extern t.mk_big(s: f64) -> t.Big link "LIBPATH:mk_big"
extern t.big_sum(b: t.Big) -> f64 link "LIBPATH:big_sum"
extern t.sum_ptr(p: *mut t.Pt, n: i32) -> i32 link "LIBPATH:sum_ptr"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let p = t.Pt { x: 3i32, y: 4i32 }
    let q = unsafe "ffi" "struct returned by value" {
        t.mk_pt(5i32, 6i32)
    }
    let s1 = unsafe "ffi" "struct by value" {
        t.sum_pt(p)
    }
    core.check.eq(s1, 7i32)
    core.check.eq(q.x, 5i32)
    let b = unsafe "ffi" "large struct returned by value" {
        t.mk_big(1.5f64)
    }
    let bs = unsafe "ffi" "large struct by value" {
        t.big_sum(b)
    }
    core.check.eq(bs, 9.0f64)
    let mut q2 = q
    let qp = unsafe "ffi" "pointer to the struct" {
        core.ptr.from_mut(&mut q2)
    }
    let s2 = unsafe "ffi" "struct behind a pointer" {
        t.sum_ptr(qp, 1i32)
    }
    core.check.eq(s2, 56i32)
    std.io.print_line("ffi agg ok")?
    Ok(())
end t.main
"#
    .replace("LIBPATH", lp);
    std::fs::write(t.0.join("src").join("t.ksnl"), ksn).unwrap();
    t.fmt();
    let ck = run(&["check"], &t.0);
    assert_eq!(ck.status.code(), Some(0), "check: {}{}", text(&ck.stdout), text(&ck.stderr));
    let ir = run(&["run"], &t.0);
    assert_eq!(ir.status.code(), Some(0), "run: {}{}", text(&ir.stdout), text(&ir.stderr));
    assert_eq!(text(&ir.stdout), "ffi agg ok\n");
    for release in [false, true] {
        let mut args = vec!["build", "--backend", "llvm"];
        if release {
            args.push("--release");
        }
        let b = run(&args, &t.0);
        let out = text(&b.stdout) + &text(&b.stderr);
        assert_eq!(b.status.code(), Some(0), "build (release: {}): {}", release, out);
        assert!(!out.contains("backend skipped"), "fell back (release: {}):\n{}", release, out);
        let rel = if release { "release" } else { "debug" };
        let exe = t.0.join("target").join(rel).join("t");
        let n = std::process::Command::new(&exe).current_dir(&t.0).output().expect("run native");
        assert_eq!(n.status.code(), Some(0), "native (release: {}): {}{}", release, text(&n.stdout), text(&n.stderr));
        assert_eq!(text(&n.stdout), "ffi agg ok\n");
    }
}
