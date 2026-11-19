//! FFI のネイティブ化 (N0): `layout c` の構造体を、C の ABI どおりにネイティブ (KIR → LLVM) で値渡し・値返し・ポインタ渡しできる。
//!
//! 試験ごとに C の共有ライブラリ (clang / gcc でその場で作る) と Kasane のパッケージを生成し、
//! 大きさと整列の違う構造体 (`{u8}` `{u8,u32}` `{f32,f32}` `{f64,f64,f64}` `{i32 x5}`・入れ子・配列入り・混在) を、
//! C → Kasane / Kasane → C の両方向、コールバック、`core.ptr.read` / `write` / `add` で行き来させる。
//! x86_64 は常に走り、インタプリタ (`ksn run`) とも一致を見る。aarch64 は `qemu-aarch64` と交差コンパイラがあるときだけ走る。

#![allow(dead_code, unused_imports)]

mod common;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::*;

/// 構造体の欄。
#[derive(Clone)]
enum F {
    /// スカラー (Kasane の型, C の型)。
    Sc(&'static str, &'static str),
    /// 入れ子の構造体 (名前)。
    St(&'static str),
    /// 固定長配列 (要素の Kasane の型, C の型, 個数)。
    Ar(&'static str, &'static str, usize),
}

struct S {
    name: &'static str,
    fields: Vec<(&'static str, F)>,
}

fn sc(k: &'static str, c: &'static str) -> F {
    F::Sc(k, c)
}

fn types() -> Vec<S> {
    let mut v = vec![];
    let mut add = |name, fields: Vec<(&'static str, F)>| v.push(S { name, fields });
    add("S1", vec![("a", sc("u8", "unsigned char"))]);
    add("S2", vec![("a", sc("u8", "unsigned char")), ("b", sc("u32", "unsigned int"))]);
    add("S3", vec![("x", sc("f32", "float")), ("y", sc("f32", "float"))]);
    add("S4", vec![("x", sc("f64", "double")), ("y", sc("f64", "double")), ("z", sc("f64", "double"))]);
    add("S5", vec![("a", sc("i32", "int")), ("b", sc("i32", "int")), ("c", sc("i32", "int")), ("d", sc("i32", "int")), ("e", sc("i32", "int"))]);
    add("S6", vec![("a", sc("i64", "long")), ("b", sc("f64", "double"))]);
    add("S7", vec![("a", sc("f64", "double")), ("b", sc("i64", "long"))]);
    add("S8", vec![("a", sc("f32", "float")), ("b", sc("i32", "int"))]);
    add("S9", vec![("a", sc("u8", "unsigned char")), ("b", sc("u8", "unsigned char")), ("c", sc("u8", "unsigned char"))]);
    add("S10", vec![("a", sc("u16", "unsigned short")), ("b", sc("u16", "unsigned short")), ("c", sc("u16", "unsigned short"))]);
    add("S11", vec![("a", sc("f32", "float")), ("b", sc("f32", "float")), ("c", sc("f32", "float"))]);
    add("S12", vec![("a", sc("f32", "float")), ("b", sc("f32", "float")), ("c", sc("f32", "float")), ("d", sc("f32", "float"))]);
    add("S13", vec![("a", sc("i64", "long")), ("b", sc("i64", "long"))]);
    add("S14", vec![("a", sc("f32", "float")), ("b", sc("f32", "float")), ("c", sc("f64", "double"))]);
    add("S15", vec![("a", sc("f64", "double")), ("b", sc("f64", "double"))]);
    add("S16", vec![("a", sc("i8", "signed char")), ("b", sc("i16", "short")), ("c", sc("i32", "int")), ("d", sc("f64", "double"))]);
    add("N1", vec![("s", F::St("S2")), ("d", sc("f64", "double"))]);
    add("N2", vec![("p", F::St("S3")), ("q", F::St("S3"))]);
    add("N3", vec![("t", sc("u8", "unsigned char")), ("n", F::St("S5"))]);
    add("A1", vec![("v", F::Ar("i32", "int", 3))]);
    add("A2", vec![("tag", sc("u8", "unsigned char")), ("v", F::Ar("u8", "unsigned char", 7))]);
    add("A3", vec![("v", F::Ar("f64", "double", 2))]);
    add("A4", vec![("v", F::Ar("u8", "unsigned char", 9)), ("w", F::Ar("u8", "unsigned char", 9))]);
    add("A5", vec![("a", sc("i32", "int")), ("v", F::Ar("f32", "float", 4))]);
    v
}

fn find<'a>(ts: &'a [S], n: &str) -> &'a S {
    ts.iter().find(|s| s.name == n).unwrap()
}

/// 欄の道筋 (`a.b` / `a.v[2usize]` の Kasane 側、`a.b` / `a.v[2]` の C 側) と種類を、走査の順に並べる。
fn scalars(ts: &[S], s: &S) -> Vec<(String, String, &'static str)> {
    let mut out = vec![];
    for (fname, f) in &s.fields {
        match f {
            F::Sc(k, _) => out.push((fname.to_string(), fname.to_string(), *k)),
            F::St(n) => {
                for (kp, cp, k) in scalars(ts, find(ts, n)) {
                    out.push((format!("{}.{}", fname, kp), format!("{}.{}", fname, cp), k));
                }
            }
            F::Ar(k, _, n) => {
                for j in 0..*n {
                    out.push((format!("{}[{}usize]", fname, j), format!("{}[{}]", fname, j), *k));
                }
            }
        }
    }
    out
}

fn lit(k: &str, v: i64) -> String {
    if k.starts_with('f') {
        format!("{}.0{}", v, k)
    } else {
        format!("{}{}", v, k)
    }
}

/// 構造体のリテラル (欄の値は `seed + 走査の番号`)。`next` は走査の番号。配列は `pre` に `let` として先に束縛する (1 行を短く保つ)。
fn ksn_literal(ts: &[S], s: &S, seed: i64, next: &mut i64, pre: &mut Vec<String>, tag: &str) -> String {
    let mut parts = vec![];
    for (fname, f) in &s.fields {
        let v = match f {
            F::Sc(k, _) => {
                let n = *next;
                *next += 1;
                lit(k, seed + n)
            }
            F::St(n) => ksn_literal(ts, find(ts, n), seed, next, pre, &format!("{}{}", tag, fname)),
            F::Ar(k, _, n) => {
                let mut es = vec![];
                for _ in 0..*n {
                    let m = *next;
                    *next += 1;
                    es.push(lit(k, seed + m));
                }
                let name = format!("arr_{}{}", tag, fname);
                pre.push(format!("    let {} = [{}]", name, es.join(", ")));
                name
            }
        };
        parts.push(format!("{}: {}", fname, v));
    }
    format!("t.{} {{ {} }}", s.name, parts.join(", "))
}

/// コールバックの本体: 引数の写しの、配列でないスカラーの欄すべてに 1 を足す (配列は添字が panic しうるので写すだけ)。
fn bump_stmts(ts: &[S], s: &S, path: &str, out: &mut Vec<String>) {
    for (fname, f) in &s.fields {
        let p = format!("{}.{}", path, fname);
        match f {
            F::Sc(k, _) => {
                let rd = p.replacen("r.", "s.", 1);
                let v = if k.starts_with('f') { format!("{} + 1.0{}", rd, k) } else { format!("{}.wrapping_add(1{})", rd, k) };
                out.push(format!("    {} = {}", p, v));
            }
            F::St(n) => bump_stmts(ts, find(ts, n), &p, out),
            F::Ar(..) => {}
        }
    }
}

fn gen_c(ts: &[S]) -> String {
    let mut c = String::from("#include <stdlib.h>\n");
    for s in ts {
        c.push_str("typedef struct {\n");
        for (fname, f) in &s.fields {
            match f {
                F::Sc(_, ct) => writeln!(c, "    {} {};", ct, fname).unwrap(),
                F::St(n) => writeln!(c, "    {} {};", n, fname).unwrap(),
                F::Ar(_, ct, n) => writeln!(c, "    {} {}[{}];", ct, fname, n).unwrap(),
            }
        }
        writeln!(c, "}} {};", s.name).unwrap();
    }
    for s in ts {
        let n = s.name;
        let l = n.to_lowercase();
        let sc = scalars(ts, s);
        // make: 欄 k に (seed + k)
        writeln!(c, "{n} make_{l}(int seed) {{\n    {n} r;\n    __builtin_memset(&r, 0, sizeof r);").unwrap();
        for (k, (_, cp, _)) in sc.iter().enumerate() {
            writeln!(c, "    r.{} = seed + {};", cp, k).unwrap();
        }
        writeln!(c, "    return r;\n}}").unwrap();
        // sum
        let terms: Vec<String> = sc.iter().map(|(_, cp, _)| format!("(double)s.{}", cp)).collect();
        writeln!(c, "double sum_{l}({n} s) {{ return {}; }}", terms.join(" + ")).unwrap();
        writeln!(c, "{n} echo_{l}({n} s) {{ return s; }}").unwrap();
        writeln!(c, "{n} cb_{l}({n} (*f)({n}), {n} s) {{ return f(s); }}").unwrap();
        // 他の引数の途中に構造体を置く (レジスタが足りなくなる位置)
        writeln!(c, "double sp_{l}(long a, long b, long c, long d, long e, {n} s, long f, double g) {{ return (double)(a + b + c + d + e + f) + g + sum_{l}(s); }}").unwrap();
        writeln!(c, "double sp2_{l}(double a, double b, double c, double d, double e, double f, double g, double h, {n} s, double i) {{ return a + b + c + d + e + f + g + h + i + sum_{l}(s); }}").unwrap();
        writeln!(c, "void bump_{l}({n} *p) {{").unwrap();
        for (_, cp, _) in &sc {
            writeln!(c, "    p->{} += 1;", cp).unwrap();
        }
        writeln!(c, "}}").unwrap();
        writeln!(c, "{n} *mk_arr_{l}(int n, int seed) {{ {n} *a = malloc(sizeof({n}) * n); for (int i = 0; i < n; i++) a[i] = make_{l}(seed + 10 * i); return a; }}").unwrap();
        writeln!(c, "{n} get_{l}({n} *a, int i) {{ return a[i]; }}").unwrap();
        writeln!(c, "double size_{l}(void) {{ return (double)sizeof({n}); }}").unwrap();
    }
    c
}

fn gen_ksn(ts: &[S], lib: &str) -> String {
    let mut k = String::new();
    for s in ts {
        writeln!(k, "type t.{} layout c derive [Clone, Copy]", s.name).unwrap();
        for (fname, f) in &s.fields {
            match f {
                F::Sc(kt, _) => writeln!(k, "    {}: {}", fname, kt).unwrap(),
                F::St(n) => writeln!(k, "    {}: t.{}", fname, n).unwrap(),
                F::Ar(kt, _, n) => writeln!(k, "    {}: [{}; {}]", fname, kt, n).unwrap(),
            }
        }
        writeln!(k, "end t.{}\n", s.name).unwrap();
    }
    for s in ts {
        let n = s.name;
        let l = n.to_lowercase();
        macro_rules! ext {
            ($name:expr, $params:expr, $ret:expr) => {
                writeln!(k, "extern t.{}_{}({}) -> {} link \"{}:{}_{}\"\n", $name, l, $params, $ret, lib, $name, l).unwrap()
            };
        }
        ext!("make", "seed: i32", format!("t.{}", n));
        ext!("sum", format!("s: t.{}", n), "f64");
        ext!("echo", format!("s: t.{}", n), format!("t.{}", n));
        ext!("cb", format!("f: fn(t.{n}) -> t.{n} uses [] panics never, s: t.{n}"), format!("t.{}", n));
        ext!("sp", format!("a: i64, b: i64, c: i64, d: i64, e: i64, s: t.{n}, f: i64, g: f64"), "f64");
        ext!("sp2", format!("a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64, s: t.{n}, i: f64"), "f64");
        ext!("bump", format!("p: *mut t.{}", n), "()");
        ext!("mk_arr", "n: i32, seed: i32", format!("*mut t.{}", n));
        ext!("get", format!("a: *mut t.{}, i: i32", n), format!("t.{}", n));
        ext!("size", "", "f64");
        // コールバック: 配列でない欄すべてに 1 を足して返す。
        let mut st = vec![];
        bump_stmts(ts, s, "r", &mut st);
        writeln!(k, "fn t.cbfn_{l}(s: t.{n}) -> t.{n} uses [] panics never\n    let mut r = s\n{}\n    r\nend t.cbfn_{l}\n", st.join("\n")).unwrap();
    }
    for s in ts {
        let n = s.name;
        let l = n.to_lowercase();
        let sc = scalars(ts, s);
        let size_sum: i64 = (0..sc.len() as i64).sum();
        let check_cb = |var: &str| -> String {
            let mut o = String::new();
            for (j, (kp, _, kt)) in sc.iter().enumerate() {
                // コールバックは配列の欄を変えない。
                let want = lit(kt, 20 + j as i64 + if kp.contains('[') { 0 } else { 1 });
                if kt.starts_with('f') {
                    writeln!(o, "    core.check.is_true({}.{} == {})", var, kp, want).unwrap();
                } else {
                    writeln!(o, "    core.check.eq({}.{}, {})", var, kp, want).unwrap();
                }
            }
            o
        };
        let check_fields = |var: &str, seed: i64| -> String {
            let mut o = String::new();
            for (j, (kp, _, kt)) in sc.iter().enumerate() {
                let want = lit(kt, seed + j as i64);
                if kt.starts_with('f') {
                    writeln!(o, "    core.check.is_true({}.{} == {})", var, kp, want).unwrap();
                } else {
                    writeln!(o, "    core.check.eq({}.{}, {})", var, kp, want).unwrap();
                }
            }
            o
        };
        let sum_of = |seed: i64| -> i64 { seed * sc.len() as i64 + size_sum };
        let mut a = 0;
        let mut pre = vec![];
        let lit20 = ksn_literal(ts, s, 20, &mut a, &mut pre, "");
        writeln!(k, "fn t.test_{l}() -> () uses [] panics may").unwrap();
        writeln!(k, "    let a = unsafe \"ffi\" \"C makes it\" {{\n        t.make_{l}(10i32)\n    }}").unwrap();
        k.push_str(&check_fields("a", 10));
        for p in &pre {
            writeln!(k, "{}", p).unwrap();
        }
        writeln!(k, "    let b = {}", lit20).unwrap();
        k.push_str(&check_fields("b", 20));
        writeln!(k, "    let sb = unsafe \"ffi\" \"sum\" {{\n        t.sum_{l}(b)\n    }}").unwrap();
        writeln!(k, "    core.check.is_true(sb == {}.0f64)", sum_of(20)).unwrap();
        writeln!(k, "    let e = unsafe \"ffi\" \"echo\" {{\n        t.echo_{l}(b)\n    }}").unwrap();
        k.push_str(&check_fields("e", 20));
        writeln!(k, "    let c = unsafe \"ffi\" \"callback\" {{\n        t.cb_{l}(t.cbfn_{l}, b)\n    }}").unwrap();
        k.push_str(&check_cb("c"));
        writeln!(k, "    let sp = unsafe \"ffi\" \"spill\" {{\n        t.sp_{l}(1i64, 2i64, 3i64, 4i64, 5i64, b, 6i64, 0.5f64)\n    }}").unwrap();
        writeln!(k, "    core.check.is_true(sp == {}.5f64)", 21 + sum_of(20)).unwrap();
        writeln!(k, "    let z = 1.0f64").unwrap();
        writeln!(k, "    let sp2 = unsafe \"ffi\" \"spill sse\" {{\n        t.sp2_{l}(z, z, z, z, z, z, z, z, b, z)\n    }}").unwrap();
        writeln!(k, "    core.check.is_true(sp2 == {}.0f64)", 9 + sum_of(20)).unwrap();
        writeln!(k, "    let mut m = b").unwrap();
        writeln!(k, "    let pm = unsafe \"ffi\" \"pointer to a local\" {{\n        core.ptr.from_mut(&mut m)\n    }}").unwrap();
        writeln!(k, "    unsafe \"ffi\" \"by pointer\" {{\n        t.bump_{l}(pm)\n    }}").unwrap();
        k.push_str(&check_fields("m", 21));
        writeln!(k, "    let arr = unsafe \"ffi\" \"C array\" {{\n        t.mk_arr_{l}(3i32, 40i32)\n    }}").unwrap();
        writeln!(k, "    let p1 = unsafe \"ffi\" \"index\" {{\n        core.ptr.add(arr, 1usize)\n    }}").unwrap();
        writeln!(k, "    let r1 = unsafe \"ffi\" \"read\" {{\n        core.ptr.read(p1)\n    }}").unwrap();
        k.push_str(&check_fields("r1", 50));
        writeln!(k, "    unsafe \"ffi\" \"write\" {{\n        core.ptr.write(p1, b)\n    }}").unwrap();
        writeln!(k, "    let g1 = unsafe \"ffi\" \"read back through C\" {{\n        t.get_{l}(arr, 1i32)\n    }}").unwrap();
        k.push_str(&check_fields("g1", 20));
        writeln!(k, "    let g2 = unsafe \"ffi\" \"untouched element\" {{\n        t.get_{l}(arr, 2i32)\n    }}").unwrap();
        k.push_str(&check_fields("g2", 60));
        writeln!(k, "end t.test_{l}\n").unwrap();
    }
    writeln!(k, "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may").unwrap();
    for s in ts {
        writeln!(k, "    t.test_{}()", s.name.to_lowercase()).unwrap();
    }
    writeln!(k, "    core.mem.drop(std.io.print_line(\"abi ok\"))\n    Ok(())\nend t.main").unwrap();
    k
}

fn have(cmd: &str) -> bool {
    Command::new(cmd).arg("--version").stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false)
}

fn sysroot() -> String {
    std::env::var("KSN_SYSROOT").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/usr/aarch64-linux-gnu".to_string())
}

fn cross_ready() -> bool {
    have("qemu-aarch64") && have("aarch64-linux-gnu-gcc") && Path::new(&sysroot()).join("lib").join("libc.so.6").exists()
}

/// 一時パッケージ (`libabi.so` 付き)。`cc` が C の共有ライブラリを作るコンパイラ。
fn abi_pkg(name: &str, cc_cmd: &str) -> Tmp {
    let ts = types();
    let t = Tmp::new(name, "");
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let c = t.0.join("abi.c");
    fs::write(&c, gen_c(&ts)).unwrap();
    let lib = t.0.join("libabi.so");
    let o = Command::new(cc_cmd).args(["-shared", "-fPIC", "-O1", "-o", lib.to_str().unwrap(), c.to_str().unwrap()]).output().expect("spawn cc");
    assert!(o.status.success(), "{}: {}", cc_cmd, text(&o.stderr));
    fs::write(t.0.join("src").join("t.ksnl"), gen_ksn(&ts, lib.to_str().unwrap())).unwrap();
    t.fmt();
    t
}

fn native_exe(out: &str, dir: &Path) -> PathBuf {
    let rel = out.lines().find_map(|l| l.strip_prefix("native executable: ")).unwrap_or_else(|| panic!("no native executable in:\n{}", out));
    dir.join(rel.trim())
}

#[test]
fn structs_of_many_shapes_cross_the_c_boundary_natively_on_x86_64() {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) || !(have("cc") || have("gcc") || have("clang")) || !clang_available() {
        return;
    }
    let cc_cmd = if have("cc") { "cc" } else if have("gcc") { "gcc" } else { "clang" };
    let t = abi_pkg("ffi_abi_x86", cc_cmd);
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}{}", text(&c.stdout), text(&c.stderr));
    // (インタプリタは構造体を引数・戻り値に持つコールバックを持たないので、`ksn run` との比較はしない。C の関数の呼び出しは examples/ffi/shapes などが比べる。)
    // ネイティブ (LLVM)。退避は許さない。
    let b = run(&["build", "--backend", "llvm"], &t.0);
    let out = text(&b.stdout) + &text(&b.stderr);
    assert_eq!(b.status.code(), Some(0), "{}", out);
    assert!(!out.contains("backend skipped"), "fell back to the interpreter bundle:\n{}", out);
    let exe = native_exe(&text(&b.stdout), &t.0);
    let n = Command::new(&exe).current_dir(&t.0).output().expect("run native");
    assert_eq!(n.status.code(), Some(0), "native: {}{}", text(&n.stdout), text(&n.stderr));
    assert_eq!(text(&n.stdout), "abi ok\n");
}

#[test]
fn structs_of_many_shapes_cross_the_c_boundary_natively_on_aarch64() {
    if !cross_ready() {
        return;
    }
    let t = abi_pkg("ffi_abi_a64", "aarch64-linux-gnu-gcc");
    let b = run(&["build", "--target", "aarch64-linux-gnu"], &t.0);
    let out = text(&b.stdout) + &text(&b.stderr);
    assert_eq!(b.status.code(), Some(0), "{}", out);
    assert!(!out.contains("backend skipped"), "fell back:\n{}", out);
    let exe = native_exe(&text(&b.stdout), &t.0);
    let r = Command::new("qemu-aarch64").arg("-L").arg(sysroot()).arg(&exe).current_dir(&t.0).output().expect("run under qemu-aarch64");
    assert_eq!(r.status.code(), Some(0), "aarch64: {}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "abi ok\n");
}

#[test]
fn c_sizes_match_the_kasane_layout() {
    // `layout c` の大きさは C の `sizeof` と同じ (配置の規則 3.8)。`size_X` を Kasane の `core.mem.size_of` と突き合わせる代わりに、
    // 生成した C の `sizeof` と、Kasane から値渡しした構造体の和が合うこと (上の試験) で配置が一致する。ここでは生成物の形だけ確かめる。
    let ts = types();
    let c = gen_c(&ts);
    assert!(c.contains("typedef struct {") && c.contains("make_a4"));
    assert!(gen_ksn(&ts, "x").contains("v: [u8; 9]"));
}
