//! Win64 (`x86_64-windows-gnu`) インタプリタの `layout c` 構造体の値渡し (`extern` 呼び出しとコールバック)。
//!
//! Windows 版 `ksn.exe` を wine で動かし、`extern` で C の DLL の関数を呼ぶ。構造体の値渡し・値返し（引数・戻り値）
//! と、C から Kasane の関数を呼ぶコールバック（構造体を受けて返す）を試す。
//!
//! Win64 呼び出し規約: 整数/ポインタ引数は RCX/RDX/R8/R9、浮動小数は XMM0-3、32 バイトのシャドウスペース、
//! 構造体は 8 バイト以下なら整数レジスタ 1 つ、超えれば呼び出し側の複製へのポインタで渡す。戻り値も同様
//! （8 バイト以下は RAX/XMM0、超えれば sret）。
//!
//! wine・mingw-w64・sysroot がそろわなければ黙って飛ばす。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::*;

const TRIPLE: &str = "x86_64-windows-gnu";
const DEFAULT_SYSROOT: &str = "/usr/x86_64-w64-mingw32";
const MINGW_CC: &str = "x86_64-w64-mingw32-gcc";

fn sysroot() -> String {
    std::env::var("KSN_SYSROOT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SYSROOT.to_string())
}

fn have(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn cross_ready() -> bool {
    cfg!(all(target_arch = "x86_64", target_os = "linux"))
        && have(MINGW_CC)
        && wine().is_some()
        && Path::new(&sysroot()).join("lib").join("crt2.o").exists()
}

/// `wine64` の場所 (PATH、なければ Debian / Ubuntu の `/usr/lib/wine/wine64`)。
fn wine() -> Option<String> {
    ["wine64", "/usr/lib/wine/wine64", "wine"]
        .iter()
        .find(|c| {
            Command::new(*c)
                .arg("--version")
                .env("WINEDEBUG", "-all")
                .stdin(Stdio::null())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .map(|s| s.to_string())
}

/// Windows 版 `ksn.exe` のパス。
fn ksn_exe() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target")
        .join(TRIPLE)
        .join("release")
        .join("ksn.exe")
}

/// インタプリタ実行を試せるか (wine・mingw・sysroot・Windows 版 ksn.exe がそろっているか)。
fn interp_ready() -> bool {
    cross_ready() && ksn_exe().exists()
}

/// wine で Windows 版 `ksn.exe` を動かす。
fn run_ksn_wine(args: &[&str], cwd: &Path) -> Output {
    static PREFIX: std::sync::Once = std::sync::Once::new();
    PREFIX.call_once(|| {
        let _ = Command::new(wine().unwrap())
            .args(["wineboot", "-u"])
            .env("WINEDEBUG", "-all")
            .env("DISPLAY", "")
            .stdin(Stdio::null())
            .output();
    });
    Command::new(wine().unwrap())
        .arg(ksn_exe())
        .args(args)
        .current_dir(cwd)
        .env("WINEDEBUG", "-all")
        .env("DISPLAY", "")
        .stdin(Stdio::null())
        .output()
        .expect("run ksn under wine")
}

/// C のライブラリ。構造体の大きさを 4・8・16・24 バイトに覆う。
fn gen_c() -> String {
    let mut c = String::from("#include <stdlib.h>\n");
    c.push_str("typedef struct { int a; } S4;\n");
    c.push_str("typedef struct { int a; int b; } S8;\n");
    c.push_str("typedef struct { long long a; long long b; } S16;\n");
    c.push_str("typedef struct { long long a; long long b; long long c; } S24;\n");
    // make: 欄に seed をセットして返す。
    c.push_str("S4 make_s4(int seed) { S4 r; r.a = seed; return r; }\n");
    c.push_str("S8 make_s8(int seed) { S8 r; r.a = seed; r.b = seed + 1; return r; }\n");
    c.push_str("S16 make_s16(long long seed) { S16 r; r.a = seed; r.b = seed + 1; return r; }\n");
    c.push_str("S24 make_s24(long long seed) { S24 r; r.a = seed; r.b = seed + 1; r.c = seed + 2; return r; }\n");
    // sum: 欄の和 (double)。
    c.push_str("double sum_s4(S4 s) { return (double)s.a; }\n");
    c.push_str("double sum_s8(S8 s) { return (double)s.a + (double)s.b; }\n");
    c.push_str("double sum_s16(S16 s) { return (double)s.a + (double)s.b; }\n");
    c.push_str("double sum_s24(S24 s) { return (double)s.a + (double)s.b + (double)s.c; }\n");
    // echo: そのまま返す。
    c.push_str("S4 echo_s4(S4 s) { return s; }\n");
    c.push_str("S8 echo_s8(S8 s) { return s; }\n");
    c.push_str("S16 echo_s16(S16 s) { return s; }\n");
    c.push_str("S24 echo_s24(S24 s) { return s; }\n");
    // bump: ポインタ渡しで欄に 1 を足す。
    c.push_str("void bump_s4(S4 *p) { p->a += 1; }\n");
    c.push_str("void bump_s8(S8 *p) { p->a += 1; p->b += 1; }\n");
    c.push_str("void bump_s16(S16 *p) { p->a += 1; p->b += 1; }\n");
    c.push_str("void bump_s24(S24 *p) { p->a += 1; p->b += 1; p->c += 1; }\n");
    // sizeof。
    c.push_str("double size_s4(void) { return (double)sizeof(S4); }\n");
    c.push_str("double size_s8(void) { return (double)sizeof(S8); }\n");
    c.push_str("double size_s16(void) { return (double)sizeof(S16); }\n");
    c.push_str("double size_s24(void) { return (double)sizeof(S24); }\n");
    // コールバック: C が Kasane の関数を呼ぶ (構造体を受けて返す)。
    c.push_str("S4 cb_s4(S4 (*f)(S4), S4 s) { return f(s); }\n");
    c.push_str("S8 cb_s8(S8 (*f)(S8), S8 s) { return f(s); }\n");
    c.push_str("S16 cb_s16(S16 (*f)(S16), S16 s) { return f(s); }\n");
    c.push_str("S24 cb_s24(S24 (*f)(S24), S24 s) { return f(s); }\n");
    // レジスタが足りなくなる位置: 整数、構造体、整数、浮動小数。
    c.push_str("double spill_s4(long long a, S4 s, long long c, double d) { return (double)(a + c) + d + (double)s.a; }\n");
    c.push_str("double spill_s8(long long a, S8 s, long long c, double d) { return (double)(a + c) + d + sum_s8(s); }\n");
    c.push_str("double spill_s16(long long a, S16 s, long long c, double d) { return (double)(a + c) + d + sum_s16(s); }\n");
    c.push_str("double spill_s24(long long a, S24 s, long long c, double d) { return (double)(a + c) + d + sum_s24(s); }\n");
    c
}

/// Kasane のソース。`LIB:` を呼び出し側で DLL のパスに置き換える。
fn gen_ksn() -> String {
    let mut k = String::new();
    k.push_str("type t.S4 layout c derive [Clone, Copy]\n    a: i32\nend t.S4\n");
    k.push_str("type t.S8 layout c derive [Clone, Copy]\n    a: i32\n    b: i32\nend t.S8\n");
    k.push_str("type t.S16 layout c derive [Clone, Copy]\n    a: i64\n    b: i64\nend t.S16\n");
    k.push_str("type t.S24 layout c derive [Clone, Copy]\n    a: i64\n    b: i64\n    c: i64\nend t.S24\n");
    k.push_str("extern t.make_s4(seed: i32) -> t.S4 link \"LIB:make_s4\"\n");
    k.push_str("extern t.make_s8(seed: i32) -> t.S8 link \"LIB:make_s8\"\n");
    k.push_str("extern t.make_s16(seed: i64) -> t.S16 link \"LIB:make_s16\"\n");
    k.push_str("extern t.make_s24(seed: i64) -> t.S24 link \"LIB:make_s24\"\n");
    k.push_str("extern t.sum_s4(s: t.S4) -> f64 link \"LIB:sum_s4\"\n");
    k.push_str("extern t.sum_s8(s: t.S8) -> f64 link \"LIB:sum_s8\"\n");
    k.push_str("extern t.sum_s16(s: t.S16) -> f64 link \"LIB:sum_s16\"\n");
    k.push_str("extern t.sum_s24(s: t.S24) -> f64 link \"LIB:sum_s24\"\n");
    k.push_str("extern t.echo_s4(s: t.S4) -> t.S4 link \"LIB:echo_s4\"\n");
    k.push_str("extern t.echo_s8(s: t.S8) -> t.S8 link \"LIB:echo_s8\"\n");
    k.push_str("extern t.echo_s16(s: t.S16) -> t.S16 link \"LIB:echo_s16\"\n");
    k.push_str("extern t.echo_s24(s: t.S24) -> t.S24 link \"LIB:echo_s24\"\n");
    k.push_str("extern t.bump_s4(p: *mut t.S4) -> () link \"LIB:bump_s4\"\n");
    k.push_str("extern t.bump_s8(p: *mut t.S8) -> () link \"LIB:bump_s8\"\n");
    k.push_str("extern t.bump_s16(p: *mut t.S16) -> () link \"LIB:bump_s16\"\n");
    k.push_str("extern t.bump_s24(p: *mut t.S24) -> () link \"LIB:bump_s24\"\n");
    k.push_str("extern t.size_s4() -> f64 link \"LIB:size_s4\"\n");
    k.push_str("extern t.size_s8() -> f64 link \"LIB:size_s8\"\n");
    k.push_str("extern t.size_s16() -> f64 link \"LIB:size_s16\"\n");
    k.push_str("extern t.size_s24() -> f64 link \"LIB:size_s24\"\n");
    k.push_str("extern t.spill_s4(a: i64, s: t.S4, c: i64, d: f64) -> f64 link \"LIB:spill_s4\"\n");
    k.push_str("extern t.spill_s8(a: i64, s: t.S8, c: i64, d: f64) -> f64 link \"LIB:spill_s8\"\n");
    k.push_str("extern t.spill_s16(a: i64, s: t.S16, c: i64, d: f64) -> f64 link \"LIB:spill_s16\"\n");
    k.push_str("extern t.spill_s24(a: i64, s: t.S24, c: i64, d: f64) -> f64 link \"LIB:spill_s24\"\n");
    // コールバック: 欄すべてに 1 を足して返す。
    k.push_str("fn t.cbfn_s4(s: t.S4) -> t.S4 uses [] panics never\n    s.a = s.a.wrapping_add(1i32)\n    s\nend t.cbfn_s4\n");
    k.push_str("fn t.cbfn_s8(s: t.S8) -> t.S8 uses [] panics never\n    s.a = s.a.wrapping_add(1i32)\n    s.b = s.b.wrapping_add(1i32)\n    s\nend t.cbfn_s8\n");
    k.push_str("fn t.cbfn_s16(s: t.S16) -> t.S16 uses [] panics never\n    s.a = s.a.wrapping_add(1i64)\n    s.b = s.b.wrapping_add(1i64)\n    s\nend t.cbfn_s16\n");
    k.push_str("fn t.cbfn_s24(s: t.S24) -> t.S24 uses [] panics never\n    s.a = s.a.wrapping_add(1i64)\n    s.b = s.b.wrapping_add(1i64)\n    s.c = s.c.wrapping_add(1i64)\n    s\nend t.cbfn_s24\n");
    k.push_str("extern t.cb_s4(f: fn(t.S4) -> t.S4 uses [] panics never, s: t.S4) -> t.S4 link \"LIB:cb_s4\"\n");
    k.push_str("extern t.cb_s8(f: fn(t.S8) -> t.S8 uses [] panics never, s: t.S8) -> t.S8 link \"LIB:cb_s8\"\n");
    k.push_str("extern t.cb_s16(f: fn(t.S16) -> t.S16 uses [] panics never, s: t.S16) -> t.S16 link \"LIB:cb_s16\"\n");
    k.push_str("extern t.cb_s24(f: fn(t.S24) -> t.S24 uses [] panics never, s: t.S24) -> t.S24 link \"LIB:cb_s24\"\n");
    k.push_str("fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n");
    k.push_str("    unsafe \"ffi\" \"struct by value on Win64\" {\n");
    k.push_str("        core.check.eq(t.size_s4(), 4.0f64)\n");
    k.push_str("        core.check.eq(t.size_s8(), 8.0f64)\n");
    k.push_str("        core.check.eq(t.size_s16(), 16.0f64)\n");
    k.push_str("        core.check.eq(t.size_s24(), 24.0f64)\n");
    // make / sum / echo: 構造体の値渡しと値返し。
    k.push_str("        let m4 = t.make_s4(10i32)\n");
    k.push_str("        core.check.eq(m4.a, 10i32)\n");
    k.push_str("        core.check.eq(t.sum_s4(m4), 10.0f64)\n");
    k.push_str("        let e4 = t.echo_s4(m4)\n");
    k.push_str("        core.check.eq(e4.a, 10i32)\n");
    k.push_str("        let m8 = t.make_s8(10i32)\n");
    k.push_str("        core.check.eq(m8.a, 10i32)\n");
    k.push_str("        core.check.eq(m8.b, 11i32)\n");
    k.push_str("        core.check.eq(t.sum_s8(m8), 21.0f64)\n");
    k.push_str("        let e8 = t.echo_s8(m8)\n");
    k.push_str("        core.check.eq(e8.a, 10i32)\n");
    k.push_str("        core.check.eq(e8.b, 11i32)\n");
    k.push_str("        let m16 = t.make_s16(10i64)\n");
    k.push_str("        core.check.eq(m16.a, 10i64)\n");
    k.push_str("        core.check.eq(m16.b, 11i64)\n");
    k.push_str("        core.check.eq(t.sum_s16(m16), 21.0f64)\n");
    k.push_str("        let e16 = t.echo_s16(m16)\n");
    k.push_str("        core.check.eq(e16.a, 10i64)\n");
    k.push_str("        core.check.eq(e16.b, 11i64)\n");
    k.push_str("        let m24 = t.make_s24(10i64)\n");
    k.push_str("        core.check.eq(m24.a, 10i64)\n");
    k.push_str("        core.check.eq(m24.b, 11i64)\n");
    k.push_str("        core.check.eq(m24.c, 12i64)\n");
    k.push_str("        core.check.eq(t.sum_s24(m24), 33.0f64)\n");
    k.push_str("        let e24 = t.echo_s24(m24)\n");
    k.push_str("        core.check.eq(e24.a, 10i64)\n");
    k.push_str("        core.check.eq(e24.b, 11i64)\n");
    k.push_str("        core.check.eq(e24.c, 12i64)\n");
    // C → Kasane コールバック: 構造体を受けて 1 を足して返す。
    k.push_str("        let c4 = t.cb_s4(t.cbfn_s4, m4)\n");
    k.push_str("        core.check.eq(c4.a, 11i32)\n");
    k.push_str("        let c8 = t.cb_s8(t.cbfn_s8, m8)\n");
    k.push_str("        core.check.eq(c8.a, 11i32)\n");
    k.push_str("        core.check.eq(c8.b, 12i32)\n");
    k.push_str("        let c16 = t.cb_s16(t.cbfn_s16, m16)\n");
    k.push_str("        core.check.eq(c16.a, 11i64)\n");
    k.push_str("        core.check.eq(c16.b, 12i64)\n");
    k.push_str("        let c24 = t.cb_s24(t.cbfn_s24, m24)\n");
    k.push_str("        core.check.eq(c24.a, 11i64)\n");
    k.push_str("        core.check.eq(c24.b, 12i64)\n");
    k.push_str("        core.check.eq(c24.c, 13i64)\n");
    // レジスタが足りなくなる位置。
    k.push_str("        core.check.eq(t.spill_s4(1i64, m4, 2i64, 0.5f64), 3.5f64)\n");
    k.push_str("        core.check.eq(t.spill_s8(1i64, m8, 2i64, 0.5f64), 11.5f64)\n");
    k.push_str("        core.check.eq(t.spill_s16(1i64, m16, 2i64, 0.5f64), 11.5f64)\n");
    k.push_str("        core.check.eq(t.spill_s24(1i64, m24, 2i64, 0.5f64), 12.5f64)\n");
    // ポインタ渡し: C が書き換える。
    k.push_str("        let mut mv4 = m4\n");
    k.push_str("        t.bump_s4(core.ptr.from_mut(&mut mv4))\n");
    k.push_str("        core.check.eq(mv4.a, 11i32)\n");
    k.push_str("        let mut mv8 = m8\n");
    k.push_str("        t.bump_s8(core.ptr.from_mut(&mut mv8))\n");
    k.push_str("        core.check.eq(mv8.a, 11i32)\n");
    k.push_str("        core.check.eq(mv8.b, 12i32)\n");
    k.push_str("        let mut mv16 = m16\n");
    k.push_str("        t.bump_s16(core.ptr.from_mut(&mut mv16))\n");
    k.push_str("        core.check.eq(mv16.a, 11i64)\n");
    k.push_str("        core.check.eq(mv16.b, 12i64)\n");
    k.push_str("        let mut mv24 = m24\n");
    k.push_str("        t.bump_s24(core.ptr.from_mut(&mut mv24))\n");
    k.push_str("        core.check.eq(mv24.a, 11i64)\n");
    k.push_str("        core.check.eq(mv24.b, 12i64)\n");
    k.push_str("        core.check.eq(mv24.c, 13i64)\n");
    k.push_str("    }\n");
    k.push_str("    std.io.print_line(\"win interp struct cb ok\")?\n");
    k.push_str("    Ok(())\nend t.main\n");
    k
}

/// 一時パッケージを作り、C を mingw で DLL にビルドして、Windows 版ソースを返す。
fn interp_pkg(name: &str) -> (Tmp, String) {
    let t = Tmp::new(name, "");
    let c = t.0.join("interp.c");
    fs::write(&c, gen_c()).unwrap();
    let dll = t.0.join("libinterp.dll");
    let o = Command::new(MINGW_CC)
        .args(["-shared", "-fPIC", "-O1", "-o", dll.to_str().unwrap(), c.to_str().unwrap()])
        .output()
        .expect("spawn mingw gcc");
    assert!(o.status.success(), "{}: {}", MINGW_CC, text(&o.stderr));
    // Windows の動的ロードは `LoadLibraryA`。絶対パスは wine では `Z:` が `/`。
    let win_src = gen_ksn().replace("LIB:", &format!("Z:{}:", dll.to_str().unwrap()));
    fs::write(t.0.join("src").join("t.ksnl"), &win_src).unwrap();
    t.fmt();
    (t, win_src)
}

#[test]
fn win64_interp_struct_by_value_extern_and_callback_under_wine() {
    if !interp_ready() {
        return;
    }
    let (t, _win_src) = interp_pkg("win_interp_struct_cb");
    // Windows 版 ksn.exe を wine で動かし、C の DLL を呼ぶインタプリタ実行。
    let r = run_ksn_wine(&["run"], &t.0);
    assert_eq!(
        text(&r.stdout),
        "win interp struct cb ok\n",
        "stdout: {}\nstderr: {}",
        text(&r.stdout),
        text(&r.stderr)
    );
    assert_eq!(r.status.code(), Some(0), "stderr: {}", text(&r.stderr));
}
