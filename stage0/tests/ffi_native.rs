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
    // インタプリタも構造体を引数・戻り値に持つコールバックをサポートしたので、`ksn run` との比較は下の試験で行う。
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
fn callback_with_struct_by_value_matches_native_on_x86_64() {
    // コールバックが `layout c` の構造体を値で取る/返すケース。インタプリタ (`ksn run`) とネイティブ (LLVM) の出力が一致することを確かめる。
    // 非 x86_64 ホストではインタプリタのコールバックは使えないのでスキップする。
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) || !(have("cc") || have("gcc") || have("clang")) {
        return;
    }
    let cc_cmd = if have("cc") { "cc" } else if have("gcc") { "gcc" } else { "clang" };
    let t = Tmp::new("ffi_cb_struct", "");
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let c = t.0.join("cb.c");
    fs::write(
        &c,
        "typedef struct { int x; int y; } Pair;\ntypedef void (*pair_cb)(Pair p, void *user);\nvoid run_cb(Pair p, pair_cb f, void *user) { f(p, user); }\n",
    )
    .unwrap();
    let lib = t.0.join("libcb.so");
    let o = Command::new(cc_cmd)
        .args(["-shared", "-fPIC", "-O1", "-o", lib.to_str().unwrap(), c.to_str().unwrap()])
        .output()
        .expect("spawn cc");
    assert!(o.status.success(), "{}: {}", cc_cmd, text(&o.stderr));
    let libpath = lib.to_str().unwrap();
    let ksn = r#"type t.Pair layout c derive [Clone, Copy]
    x: i32
    y: i32
end t.Pair

extern t.run_cb(p: t.Pair, f: fn(t.Pair, *mut t.Pair) -> () uses [] panics never, u: *mut t.Pair) -> () link "LIBPATH:run_cb"

fn t.collect(p: t.Pair, u: *mut t.Pair) -> () uses [] panics never
    unsafe "callback" "accumulate through the user pointer" {
        let acc = core.ptr.read(u)
        let dx = acc.x.wrapping_add(1i32)
        let dy = acc.y.wrapping_add(p.x.wrapping_add(p.y))
        let nw = t.Pair { x: dx, y: dy }
        core.ptr.write(u, nw)
    }
end t.collect

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut acc = t.Pair { x: 0i32, y: 0i32 }
    let ap = unsafe "ffi" "pointer to accumulator" {
        core.ptr.from_mut(&mut acc)
    }
    let p = t.Pair { x: 7i32, y: 8i32 }
    unsafe "ffi" "callback with a struct argument" {
        t.run_cb(p, t.collect, ap)
    }
    core.check.eq(acc.x, 1i32)
    core.check.eq(acc.y, 15i32)
    std.io.print_line("cb ok")?
    Ok(())
end t.main
"#.replace("LIBPATH", libpath);
    fs::write(t.0.join("src").join("t.ksnl"), ksn).unwrap();
    t.fmt();

    // `ksn check`: 構造体を値渡しするコールバックが型検査を通ること。
    let ck = run(&["check"], &t.0);
    assert_eq!(ck.status.code(), Some(0), "check: {}{}", text(&ck.stdout), text(&ck.stderr));

    // `ksn run` (インタプリタ): 構造体を値渡しするコールバックが動くこと。
    let ir = run(&["run"], &t.0);
    assert_eq!(ir.status.code(), Some(0), "run: {}{}", text(&ir.stdout), text(&ir.stderr));
    assert_eq!(text(&ir.stdout), "cb ok\n");

    // ネイティブ (LLVM): 同じ出力になること。退避は許さない。
    let b = run(&["build", "--backend", "llvm"], &t.0);
    let out = text(&b.stdout) + &text(&b.stderr);
    assert_eq!(b.status.code(), Some(0), "build: {}", out);
    assert!(!out.contains("backend skipped"), "fell back to bundle:\n{}", out);
    let exe = native_exe(&text(&b.stdout), &t.0);
    let n = Command::new(&exe).current_dir(&t.0).output().expect("run native");
    assert_eq!(n.status.code(), Some(0), "native: {}{}", text(&n.stdout), text(&n.stderr));
    assert_eq!(text(&n.stdout), "cb ok\n");
}

#[test]
fn callback_24_byte_struct_matches_native_on_x86_64() {
    // メモリクラスの構造体 (24 バイト > 16 バイト) をコールバックの値引数で受け取り、
    // インタプリタがスタック経由で正しく読み、ネイティブと一致する出力を出すこと。
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) || !(have("cc") || have("gcc") || have("clang")) {
        return;
    }
    let cc_cmd = if have("cc") { "cc" } else if have("gcc") { "gcc" } else { "clang" };
    let t = Tmp::new("ffi_cb24", "");
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let c = t.0.join("cb24.c");
    fs::write(
        &c,
        "#include <stdint.h>\ntypedef struct { int64_t x; int64_t y; int64_t z; } S24;\ntypedef void (*cb24)(S24 p, void *user);\nvoid run_cb24(S24 p, cb24 f, void *user) { f(p, user); }\n",
    )
    .unwrap();
    let lib = t.0.join("libcb24.so");
    let o = Command::new(cc_cmd)
        .args(["-shared", "-fPIC", "-O1", "-o", lib.to_str().unwrap(), c.to_str().unwrap()])
        .output()
        .expect("spawn cc");
    assert!(o.status.success(), "{}: {}", cc_cmd, text(&o.stderr));
    let libpath = lib.to_str().unwrap();
    let ksn = r#"type t.S24 layout c derive [Clone, Copy]
    x: i64
    y: i64
    z: i64
end t.S24

extern t.run_cb24(p: t.S24, f: fn(t.S24, *mut t.S24) -> () uses [] panics never, u: *mut t.S24) -> () link "LIBPATH:run_cb24"

fn t.cb24(p: t.S24, u: *mut t.S24) -> () uses [] panics never
    unsafe "callback" "24 byte struct" {
        let dx = p.x.wrapping_add(1i64)
        let dy = p.y.wrapping_add(2i64)
        let dz = p.z.wrapping_add(3i64)
        core.ptr.write(u, t.S24 { x: dx, y: dy, z: dz })
    }
end t.cb24

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut a24 = t.S24 { x: 0i64, y: 0i64, z: 0i64 }
    let ap = unsafe "ffi" "ptr" {
        core.ptr.from_mut(&mut a24)
    }
    let s24 = t.S24 { x: 1i64, y: 2i64, z: 3i64 }
    unsafe "ffi" "cb24" {
        t.run_cb24(s24, t.cb24, ap)
    }
    core.check.eq(a24.x, 2i64)
    core.check.eq(a24.y, 4i64)
    core.check.eq(a24.z, 6i64)
    std.io.print_line("cb24 ok")?
    Ok(())
end t.main
"#.replace("LIBPATH", libpath);
    fs::write(t.0.join("src").join("t.ksnl"), ksn).unwrap();
    t.fmt();
    let ck = run(&["check"], &t.0);
    assert_eq!(ck.status.code(), Some(0), "check: {}{}", text(&ck.stdout), text(&ck.stderr));
    let ir = run(&["run"], &t.0);
    assert_eq!(ir.status.code(), Some(0), "run: {}{}", text(&ir.stdout), text(&ir.stderr));
    assert_eq!(text(&ir.stdout), "cb24 ok\n");
    let b = run(&["build", "--backend", "llvm"], &t.0);
    let out = text(&b.stdout) + &text(&b.stderr);
    assert_eq!(b.status.code(), Some(0), "build: {}", out);
    assert!(!out.contains("backend skipped"), "fell back to bundle:\n{}", out);
    let exe = native_exe(&text(&b.stdout), &t.0);
    let n = Command::new(&exe).current_dir(&t.0).output().expect("run native");
    assert_eq!(n.status.code(), Some(0), "native: {}{}", text(&n.stdout), text(&n.stderr));
    assert_eq!(text(&n.stdout), "cb24 ok\n");
}

#[test]
fn callback_32_byte_struct_matches_native_on_x86_64() {
    // メモリクラスの構造体 (32 バイト) をコールバックの値引数で受け取り、スタック経由で正しく読めること。
    // 配列の添字アクセスは `panics never` 本体で `E4003` になるため、5 つの個別の `i64` 欄で書く。
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) || !(have("cc") || have("gcc") || have("clang")) {
        return;
    }
    let cc_cmd = if have("cc") { "cc" } else if have("gcc") { "gcc" } else { "clang" };
    let t = Tmp::new("ffi_cb32", "");
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let c = t.0.join("cb32.c");
    fs::write(
        &c,
        "#include <stdint.h>\ntypedef struct { int64_t a; int64_t b; int64_t c; int64_t d; int64_t e; } S32;\ntypedef void (*cb32)(S32 p, void *user);\nvoid run_cb32(S32 p, cb32 f, void *user) { f(p, user); }\n",
    )
    .unwrap();
    let lib = t.0.join("libcb32.so");
    let o = Command::new(cc_cmd)
        .args(["-shared", "-fPIC", "-O1", "-o", lib.to_str().unwrap(), c.to_str().unwrap()])
        .output()
        .expect("spawn cc");
    assert!(o.status.success(), "{}: {}", cc_cmd, text(&o.stderr));
    let libpath = lib.to_str().unwrap();
    let ksn = r#"type t.S32 layout c derive [Clone, Copy]
    a: i64
    b: i64
    c: i64
    d: i64
    e: i64
end t.S32

extern t.run_cb32(p: t.S32, f: fn(t.S32, *mut t.S32) -> () uses [] panics never, u: *mut t.S32) -> () link "LIBPATH:run_cb32"

fn t.cb32(p: t.S32, u: *mut t.S32) -> () uses [] panics never
    unsafe "callback" "32 byte struct" {
        let na = p.a.wrapping_add(1i64)
        let nb = p.b.wrapping_add(2i64)
        let nc = p.c.wrapping_add(3i64)
        let nd = p.d.wrapping_add(4i64)
        let ne = p.e.wrapping_add(5i64)
        core.ptr.write(u, t.S32 { a: na, b: nb, c: nc, d: nd, e: ne })
    }
end t.cb32

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut a32 = t.S32 { a: 0i64, b: 0i64, c: 0i64, d: 0i64, e: 0i64 }
    let ap = unsafe "ffi" "ptr" {
        core.ptr.from_mut(&mut a32)
    }
    let s32 = t.S32 { a: 1i64, b: 2i64, c: 3i64, d: 4i64, e: 5i64 }
    unsafe "ffi" "cb32" {
        t.run_cb32(s32, t.cb32, ap)
    }
    core.check.eq(a32.a, 2i64)
    core.check.eq(a32.b, 4i64)
    core.check.eq(a32.c, 6i64)
    core.check.eq(a32.d, 8i64)
    core.check.eq(a32.e, 10i64)
    std.io.print_line("cb32 ok")?
    Ok(())
end t.main
"#.replace("LIBPATH", libpath);
    fs::write(t.0.join("src").join("t.ksnl"), ksn).unwrap();
    t.fmt();
    let ck = run(&["check"], &t.0);
    assert_eq!(ck.status.code(), Some(0), "check: {}{}", text(&ck.stdout), text(&ck.stderr));
    let ir = run(&["run"], &t.0);
    assert_eq!(ir.status.code(), Some(0), "run: {}{}", text(&ir.stdout), text(&ir.stderr));
    assert_eq!(text(&ir.stdout), "cb32 ok\n");
    let b = run(&["build", "--backend", "llvm"], &t.0);
    let out = text(&b.stdout) + &text(&b.stderr);
    assert_eq!(b.status.code(), Some(0), "build: {}", out);
    assert!(!out.contains("backend skipped"), "fell back to bundle:\n{}", out);
    let exe = native_exe(&text(&b.stdout), &t.0);
    let n = Command::new(&exe).current_dir(&t.0).output().expect("run native");
    assert_eq!(n.status.code(), Some(0), "native: {}{}", text(&n.stdout), text(&n.stderr));
    assert_eq!(text(&n.stdout), "cb32 ok\n");
}

#[test]
fn callback_memory_struct_then_register_args_matches_native_on_x86_64() {
    // メモリクラスの構造体 (24 バイト) の後に整数/ポインタのレジスタ引数が続くコールバック。
    // 構造体はスタック (レジスタを消費しない) で、その後の引数は GP レジスタの先頭から割り当てられる。
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) || !(have("cc") || have("gcc") || have("clang")) {
        return;
    }
    let cc_cmd = if have("cc") { "cc" } else if have("gcc") { "gcc" } else { "clang" };
    let t = Tmp::new("ffi_cb24i", "");
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let c = t.0.join("cb24i.c");
    fs::write(
        &c,
        "#include <stdint.h>\ntypedef struct { int64_t x; int64_t y; int64_t z; } S24;\ntypedef void (*cb24i)(S24 p, int64_t extra, void *user);\nvoid run_cb24i(S24 p, int64_t extra, cb24i f, void *user) { f(p, extra, user); }\n",
    )
    .unwrap();
    let lib = t.0.join("libcb24i.so");
    let o = Command::new(cc_cmd)
        .args(["-shared", "-fPIC", "-O1", "-o", lib.to_str().unwrap(), c.to_str().unwrap()])
        .output()
        .expect("spawn cc");
    assert!(o.status.success(), "{}: {}", cc_cmd, text(&o.stderr));
    let libpath = lib.to_str().unwrap();
    let ksn = r#"type t.S24 layout c derive [Clone, Copy]
    x: i64
    y: i64
    z: i64
end t.S24

extern t.run_cb24i(p: t.S24, extra: i64, f: fn(t.S24, i64, *mut t.S24) -> () uses [] panics never, u: *mut t.S24) -> () link "LIBPATH:run_cb24i"

fn t.cb24i(p: t.S24, extra: i64, u: *mut t.S24) -> () uses [] panics never
    unsafe "callback" "memory struct then register args" {
        let acc = core.ptr.read(u)
        let nx = acc.x.wrapping_add(p.x).wrapping_add(extra)
        core.ptr.write(u, t.S24 { x: nx, y: acc.y, z: acc.z })
    }
end t.cb24i

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut a24 = t.S24 { x: 0i64, y: 0i64, z: 0i64 }
    let ap = unsafe "ffi" "ptr" {
        core.ptr.from_mut(&mut a24)
    }
    let s24 = t.S24 { x: 1i64, y: 2i64, z: 3i64 }
    unsafe "ffi" "cb24i" {
        t.run_cb24i(s24, 100i64, t.cb24i, ap)
    }
    core.check.eq(a24.x, 101i64)
    core.check.eq(a24.y, 0i64)
    core.check.eq(a24.z, 0i64)
    std.io.print_line("cb24i ok")?
    Ok(())
end t.main
"#.replace("LIBPATH", libpath);
    fs::write(t.0.join("src").join("t.ksnl"), ksn).unwrap();
    t.fmt();
    let ck = run(&["check"], &t.0);
    assert_eq!(ck.status.code(), Some(0), "check: {}{}", text(&ck.stdout), text(&ck.stderr));
    let ir = run(&["run"], &t.0);
    assert_eq!(ir.status.code(), Some(0), "run: {}{}", text(&ir.stdout), text(&ir.stderr));
    assert_eq!(text(&ir.stdout), "cb24i ok\n");
    let b = run(&["build", "--backend", "llvm"], &t.0);
    let out = text(&b.stdout) + &text(&b.stderr);
    assert_eq!(b.status.code(), Some(0), "build: {}", out);
    assert!(!out.contains("backend skipped"), "fell back to bundle:\n{}", out);
    let exe = native_exe(&text(&b.stdout), &t.0);
    let n = Command::new(&exe).current_dir(&t.0).output().expect("run native");
    assert_eq!(n.status.code(), Some(0), "native: {}{}", text(&n.stdout), text(&n.stderr));
    assert_eq!(text(&n.stdout), "cb24i ok\n");
}

#[test]
fn callback_return_24_byte_struct_matches_native_on_x86_64() {
    // コールバックがメモリクラスの構造体 (24 バイト) を値で返す (`sret` 経由) ケース。
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) || !(have("cc") || have("gcc") || have("clang")) {
        return;
    }
    let cc_cmd = if have("cc") { "cc" } else if have("gcc") { "gcc" } else { "clang" };
    let t = Tmp::new("ffi_cb24ret", "");
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let c = t.0.join("cb24ret.c");
    fs::write(
        &c,
        "#include <stdint.h>\ntypedef struct { int64_t x; int64_t y; int64_t z; } S24;\ntypedef S24 (*cb24ret)(S24 p);\nvoid run_cb24ret(S24 p, cb24ret f, void *user) { *(S24 *)user = f(p); }\n",
    )
    .unwrap();
    let lib = t.0.join("libcb24ret.so");
    let o = Command::new(cc_cmd)
        .args(["-shared", "-fPIC", "-O1", "-o", lib.to_str().unwrap(), c.to_str().unwrap()])
        .output()
        .expect("spawn cc");
    assert!(o.status.success(), "{}: {}", cc_cmd, text(&o.stderr));
    let libpath = lib.to_str().unwrap();
    let ksn = r#"type t.S24 layout c derive [Clone, Copy]
    x: i64
    y: i64
    z: i64
end t.S24

extern t.run_cb24ret(p: t.S24, f: fn(t.S24) -> t.S24 uses [] panics never, u: *mut t.S24) -> () link "LIBPATH:run_cb24ret"

fn t.cb24ret(p: t.S24) -> t.S24 uses [] panics never
    unsafe "callback" "return 24 byte struct" {
        t.S24 { x: p.x.wrapping_add(1i64), y: p.y.wrapping_add(2i64), z: p.z.wrapping_add(3i64) }
    }
end t.cb24ret

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut a24 = t.S24 { x: 0i64, y: 0i64, z: 0i64 }
    let ap = unsafe "ffi" "ptr" {
        core.ptr.from_mut(&mut a24)
    }
    let s24 = t.S24 { x: 1i64, y: 2i64, z: 3i64 }
    unsafe "ffi" "cb24ret" {
        t.run_cb24ret(s24, t.cb24ret, ap)
    }
    core.check.eq(a24.x, 2i64)
    core.check.eq(a24.y, 4i64)
    core.check.eq(a24.z, 6i64)
    std.io.print_line("cb24ret ok")?
    Ok(())
end t.main
"#.replace("LIBPATH", libpath);
    fs::write(t.0.join("src").join("t.ksnl"), ksn).unwrap();
    t.fmt();
    let ck = run(&["check"], &t.0);
    assert_eq!(ck.status.code(), Some(0), "check: {}{}", text(&ck.stdout), text(&ck.stderr));
    let ir = run(&["run"], &t.0);
    assert_eq!(ir.status.code(), Some(0), "run: {}{}", text(&ir.stdout), text(&ir.stderr));
    assert_eq!(text(&ir.stdout), "cb24ret ok\n");
    let b = run(&["build", "--backend", "llvm"], &t.0);
    let out = text(&b.stdout) + &text(&b.stderr);
    assert_eq!(b.status.code(), Some(0), "build: {}", out);
    assert!(!out.contains("backend skipped"), "fell back to bundle:\n{}", out);
    let exe = native_exe(&text(&b.stdout), &t.0);
    let n = Command::new(&exe).current_dir(&t.0).output().expect("run native");
    assert_eq!(n.status.code(), Some(0), "native: {}{}", text(&n.stdout), text(&n.stderr));
    assert_eq!(text(&n.stdout), "cb24ret ok\n");
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

/// `core.ptr.from_mut(&mut buf)` / `core.ptr.from_ref(&buf)` で `buf: [u8; N]` (配列) を C に渡すと、
/// 配列への参照は fat ポインタ (`{ptr, len}`) として格納される。ネイティブコード生成は fat ポインタを
/// ロードして ptr フィールド (データポインタ) を取らなければならず、そのままのアドレスをビットキャスト
/// すると C が呼び出し側のスタック上の一時領域を書き壊してクラッシュする (インタプリタは正しい)。
/// 回帰試験。インタプリタ (`ksn run`) とネイティブ (デバッグ・リリース) の出力を比べる。
#[test]
fn ptr_from_mut_from_ref_array_reaches_caller_buffer_natively() {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux"))
        || !(have("cc") || have("gcc") || have("clang"))
    {
        return;
    }
    let cc_cmd = if have("cc") { "cc" } else if have("gcc") { "gcc" } else { "clang" };
    let t = Tmp::new("ffi_ptr_array", "");
    fs::write(
        t.0.join("kasane.toml"),
        TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t\"]\n\n[dependencies]", 1),
    )
    .unwrap();
    let c = r#"
#include <stdint.h>
void c_write(uint8_t *p, int n) { for (int i = 0; i < n; i++) p[i] = (uint8_t)(i + 1); }
uint8_t c_read(const uint8_t *p, int n) { return p[n - 1]; }
"#;
    fs::write(t.0.join("ptr.c"), c).unwrap();
    let lib = t.0.join("libptr.so");
    let o = Command::new(cc_cmd)
        .args([
            "-shared",
            "-fPIC",
            "-O1",
            "-o",
            lib.to_str().unwrap(),
            t.0.join("ptr.c").to_str().unwrap(),
        ])
        .output()
        .expect("spawn cc");
    assert!(o.status.success(), "cc: {}", text(&o.stderr));
    let src = format!(
        r#"extern t.c_read(p: *const u8, n: i32) -> u8 link "{}:c_read"
extern t.c_write(p: *mut u8, n: i32) -> () link "{}:c_write"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut buf = [0u8; 16]
    unsafe "ffi" "write" {{
        t.c_write(core.ptr.from_mut(&mut buf), 16i32)
    }}
    core.check.eq(buf[0usize], 1u8)
    core.check.eq(buf[15usize], 16u8)
    let b = unsafe "ffi" "read" {{
        t.c_read(core.ptr.from_ref(&buf), 16i32)
    }}
    core.check.eq(b, 16u8)
    core.mem.drop(std.io.print_line("ptr ok"))
    Ok(())
end t.main
"#,
        lib.to_str().unwrap(),
        lib.to_str().unwrap()
    );
    fs::write(t.0.join("src").join("t.ksnl"), src).unwrap();
    t.fmt();
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}{}", text(&c.stdout), text(&c.stderr));
    // インタプリタ (`ksn run`) は正しいはず。
    let want = "ptr ok\n";
    let i = run(&["run"], &t.0);
    assert_eq!(i.status.code(), Some(0), "interpreter: {}{}", text(&i.stdout), text(&i.stderr));
    assert_eq!(text(&i.stdout), want, "interpreter: {}", text(&i.stderr));
    // ネイティブ (デバッグ・リリース) もインタプリタと同じ出力。バンドル退避ではなく本当のネイティブを走らせる。
    for release in [false, true] {
        let kind = if release { "release" } else { "debug" };
        let mut cmd = Command::new(exe());
        cmd.args(["build", "--backend", "llvm"]).current_dir(&t.0);
        if release {
            cmd.arg("--release");
        }
        let b = cmd.output().expect("spawn ksn");
        let out = text(&b.stdout) + &text(&b.stderr);
        assert_eq!(b.status.code(), Some(0), "build {}: {}", kind, out);
        assert!(!out.contains("backend skipped"), "fell back to the interpreter bundle {}:\n{}", kind, out);
        let exe = native_exe(&text(&b.stdout), &t.0);
        let n = Command::new(exe).current_dir(&t.0).output().expect("run native");
        assert_eq!(n.status.code(), Some(0), "native {}: {}{}", kind, text(&n.stdout), text(&n.stderr));
        assert_eq!(text(&n.stdout), want, "native {}: {}", kind, text(&n.stderr));
    }
}

/// N3a: `core.ffi.trampoline` で、捕まえたクロージャを `(fn_ptr, void *user)` の C のコールバックに渡す。
/// C が 1..=10 と 5, 7 でコールバックし、クロージャが Kasane の構造体の計数を増やす。
/// `ksn run` とネイティブ (debug / release) の出力が一致する。
#[cfg(unix)]
#[test]
fn a_trampoline_carries_user_data_to_a_c_callback_natively() {
    let t = Tmp::new("n3a_trampoline", "");
    fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t\"]\n\n[dependencies]", 1)).unwrap();
    let c = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join("ffi_userdata").join("acc.c");
    let lib = t.0.join("libaccuser.so");
    let o = cc(&["-shared", "-fPIC", "-O1", "-o", lib.to_str().unwrap(), c.to_str().unwrap()], &t.0);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let src = include_str!("data/ffi_userdata/main.ksnl").replace("LIB:", &format!("{}:", lib.to_str().unwrap()));
    fs::write(t.0.join("src").join("t.ksnl"), src).unwrap();
    let i = run(&["run"], &t.0);
    assert_eq!(i.status.code(), Some(0), "{}{}", text(&i.stdout), text(&i.stderr));
    assert_eq!(text(&i.stdout), "userdata ok\n");
    for rel in [false, true] {
        let mut a = vec!["build", "--backend", "llvm"];
        if rel {
            a.push("--release");
        }
        let b = run(&a, &t.0);
        let out = text(&b.stdout) + &text(&b.stderr);
        assert_eq!(b.status.code(), Some(0), "{}", out);
        let exe = native_exe(&out, &t.0);
        let r = Command::new(&exe).current_dir(&t.0).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
        assert_eq!(text(&r.stdout), "userdata ok\n", "native (release={})", rel);
    }
}
