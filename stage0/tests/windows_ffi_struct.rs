//! Win64 (`x86_64-windows-gnu`) の構造体の値渡しの `extern` (設計_ネイティブUI.md の N0 の Windows 版)。
//!
//! 同じ C のライブラリを 2 つにビルドする (`x86_64-w64-mingw32-gcc` で `.dll`、gcc で `.so`) し、Kasane の
//! 同じプログラム (`link` の Biblioteca の名前が違うだけ) を x86_64 のネイティブと Windows にビルドして動かす。
//! Windows は wine。両者の標準出力と終了コードが x86_64 と一致することを確かめる (Win64 の呼び出し規約は
//! System V AMD64 と違うが、結果は同じはず。違えば分類の誤り)。
//!
//! Win64 x64 の構造体の分類: 1・2・4・8 バイトの構造体は 1 個のレジスタで値渡し (整数・ポインタだけのとき GP、
//! 浮動小数だけのとき XMM)、3・12・16・24 バイトは隠しポインタ (参照) で渡す。値返しも同じ規則 (8 バイトまで
//! レジスタ、超えれば呼び出し側の領域への隠しポインタ)。試験の構造体はその 8 つの大きさを、整数だけ・浮動小数
//! だけ・混在で覆う。Kasane → C (引数・値返し・ポインタ渡し・レジスタが足りない位置) と C → Kasane (構造体を
//! 受けて返すコールバック) の両方を試す。
//!
//! 未実装: KIR に `layout c` の構造体の値渡しを下ろせないので (N0)、x86_64 でも Windows でもネイティブ生成は
//! 退避する (`E9040`)。x86_64 の System V の値渡しと Win64 の分類の両方が実装されたら `#[ignore]` を外す。
//! wine・mingw-w64・mingw の sysroot がそろわなければ黙って飛ばす。

#![allow(dead_code, unused_imports)]

mod common;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::*;

const TRIPLE: &str = "x86_64-windows-gnu";
const DEFAULT_SYSROOT: &str = "/usr/x86_64-w64-mingw32";
const MINGW_CC: &str = "x86_64-w64-mingw32-gcc";
/// すべての検査を通ったプログラムが出す 1 行。
const WANT: &str = "win64 abi ok\n";

/// 既定の sysroot。`KSN_SYSROOT` で上書きできる (`ksn build` と同じ)。
fn sysroot() -> String {
    std::env::var("KSN_SYSROOT").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| DEFAULT_SYSROOT.to_string())
}

fn have(cmd: &str) -> bool {
    Command::new(cmd).arg("--version").stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false)
}

/// 実行を伴う試験を走らせられるか (wine・mingw-w64・sysroot・ホストが x86_64 Linux)。
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
        .find(|c| Command::new(*c).arg("--version").env("WINEDEBUG", "-all").stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false))
        .map(|s| s.to_string())
}

/// wine で `exe` を動かす。
fn run_wine(exe: &Path, dir: &Path) -> std::process::Output {
    // prefix は環境の `WINEPREFIX` か既定 (`~/.wine`)。初回だけ作る。
    static PREFIX: std::sync::Once = std::sync::Once::new();
    PREFIX.call_once(|| {
        let _ = Command::new(wine().unwrap()).args(["wineboot", "-u"]).env("WINEDEBUG", "-all").env("DISPLAY", "").stdin(Stdio::null()).output();
    });
    Command::new(wine().unwrap())
        .arg(exe)
        .current_dir(dir)
        .env("WINEDEBUG", "-all")
        .env("DISPLAY", "")
        .stdin(Stdio::null())
        .output()
        .expect("run under wine")
}

/// `ksn build` の標準出力 `native executable: <相対パス>` から、実行ファイルのパスを取る。
fn native_exe(out: &str, dir: &Path) -> PathBuf {
    let rel = out
        .lines()
        .find_map(|l| l.strip_prefix("native executable: "))
        .unwrap_or_else(|| panic!("no `native executable:` line in:\n{}", out));
    dir.join(rel.trim())
}

/// 構造体の欄: (フィールド名, Kasane の型, C の型)。
type Field = (&'static str, &'static str, &'static str);

struct S {
    name: &'static str,
    fields: Vec<Field>,
}

fn s(name: &'static str, fields: Vec<Field>) -> S {
    S { name, fields }
}

/// 覆う大きさ: 1・2・4・8 (Win64 ではレジスタ) と 3・12・16・24 (隠しポインタ)。
/// それぞれ整数だけ・浮動小数だけ・混在 (大きさが両方作れるもの) を用意する。
fn types() -> Vec<S> {
    vec![
        // 1・2・4・8 バイト: 1 個のレジスタで値渡し。
        s("W1", vec![("a", "u8", "unsigned char")]),
        s("W2", vec![("a", "u16", "unsigned short")]),
        s("W4i", vec![("a", "u16", "unsigned short"), ("b", "u16", "unsigned short")]),
        s("W4f", vec![("a", "f32", "float")]),
        s("W8i", vec![("a", "u32", "unsigned int"), ("b", "u32", "unsigned int")]),
        s("W8f", vec![("a", "f32", "float"), ("b", "f32", "float")]),
        s("W8d", vec![("a", "f64", "double")]),
        s("W8m", vec![("a", "u32", "unsigned int"), ("b", "f32", "float")]),
        // 3・12・16・24 バイト: 隠しポインタ (参照) で渡す。
        s("B3", vec![("a", "u8", "unsigned char"), ("b", "u8", "unsigned char"), ("c", "u8", "unsigned char")]),
        s("B12i", vec![("a", "i32", "int"), ("b", "i32", "int"), ("c", "i32", "int")]),
        s("B12f", vec![("a", "f32", "float"), ("b", "f32", "float"), ("c", "f32", "float")]),
        s("B16i", vec![("a", "i32", "int"), ("b", "i32", "int"), ("c", "i32", "int"), ("d", "i32", "int")]),
        s("B16d", vec![("a", "f64", "double"), ("b", "f64", "double")]),
        s("B16m", vec![("a", "f64", "double"), ("b", "i32", "int")]),
        s("B24i", vec![
            ("a", "i32", "int"),
            ("b", "i32", "int"),
            ("c", "i32", "int"),
            ("d", "i32", "int"),
            ("e", "i32", "int"),
            ("f", "i32", "int"),
        ]),
        s("B24d", vec![("a", "f64", "double"), ("b", "f64", "double"), ("c", "f64", "double")]),
        s("B24m", vec![("a", "f64", "double"), ("b", "f64", "double"), ("c", "i32", "int")]),
    ]
}

/// 欄の C の大きさと整列 (自然境界。3.8 の `layout c` の規則)。
fn size_align(k: &str) -> (usize, usize) {
    match k {
        "u8" => (1, 1),
        "u16" => (2, 2),
        "u32" | "i32" | "f32" => (4, 4),
        "u64" | "i64" | "f64" => (8, 8),
        _ => unreachable!("{}", k),
    }
}

/// 構造体の C の `sizeof` (宣言順・自然境界・詰め物・大きさの切り上げ)。
fn csize(s: &S) -> usize {
    let mut off = 0usize;
    let mut align = 1usize;
    for (_, k, _) in &s.fields {
        let (sz, al) = size_align(k);
        off = (off + al - 1) / al * al + sz;
        align = align.max(al);
    }
    (off + align - 1) / align * align
}

/// 欄の値のリテラル (Kasane 側)。
fn lit(k: &str, v: i64) -> String {
    if k.starts_with('f') {
        format!("{}.0{}", v, k)
    } else {
        format!("{}{}", v, k)
    }
}

/// 欄に 1 を足す式 (コールバック・`bump` の中身)。
fn bump(k: &str, path: &str) -> String {
    if k.starts_with('f') {
        format!("{} + 1.0{}", path, k)
    } else {
        format!("{}.wrapping_add(1{})", path, k)
    }
}

/// `var` の欄が `seed + 走査の番号` であることを確かめる行。`ind` は追加の字下げ (unsafe のブロックの中)。
fn check_fields(var: &str, fields: &[Field], seed: i64, ind: &str) -> String {
    let mut o = String::new();
    for (j, (f, k, _)) in fields.iter().enumerate() {
        let want = lit(k, seed + j as i64);
        if k.starts_with('f') {
            writeln!(o, "{}    core.check.is_true({}.{} == {})", ind, var, f, want).unwrap();
        } else {
            writeln!(o, "{}    core.check.eq({}.{}, {})", ind, var, f, want).unwrap();
        }
    }
    o
}

/// C のライブラリ。欄の型は Win64 でも Linux でも同じ幅になるように選ぶ
/// (`long long` は両方で 64 ビット。Win64 の `long` は 32 ビットなので i64 には使えない)。
fn gen_c(ts: &[S]) -> String {
    let mut c = String::from("#include <stdlib.h>\n");
    for s in ts {
        c.push_str("typedef struct {\n");
        for (f, _, ct) in &s.fields {
            writeln!(c, "    {} {};", ct, f).unwrap();
        }
        writeln!(c, "}} {};", s.name).unwrap();
    }
    for s in ts {
        let (n, l) = (s.name, s.name.to_lowercase());
        // make: 欄 j に (seed + j)。
        writeln!(c, "{n} make_{l}(int seed) {{").unwrap();
        writeln!(c, "    {n} r;").unwrap();
        for (j, (f, _, ct)) in s.fields.iter().enumerate() {
            writeln!(c, "    r.{} = ({})seed + {};", f, ct, j).unwrap();
        }
        writeln!(c, "    return r;\n}}").unwrap();
        // sum: 欄の和 (double)。Win64 と SysV で渡し方が違うとここが崩れる。
        let terms: Vec<String> = s.fields.iter().map(|(f, _, _)| format!("(double)s.{}", f)).collect();
        writeln!(c, "double sum_{l}({n} s) {{ return {}; }}", terms.join(" + ")).unwrap();
        writeln!(c, "{n} echo_{l}({n} s) {{ return s; }}").unwrap();
        // コールバック: C が Kasane を呼ぶ (C → Kasane。構造体の値渡し)。
        writeln!(c, "{n} cb_{l}({n} (*f)({n}), {n} s) {{ return f(s); }}").unwrap();
        // ポインタ渡し (C が書き換える)。
        writeln!(c, "void bump_{l}({n} *p) {{").unwrap();
        for (f, _, ct) in &s.fields {
            writeln!(c, "    p->{} = ({})(p->{} + 1);", f, ct, f).unwrap();
        }
        writeln!(c, "}}").unwrap();
        writeln!(c, "double size_{l}(void) {{ return (double)sizeof({n}); }}").unwrap();
        // レジスタが足りなくなる位置: 整数 2 個、構造体、整数、浮き小数。
        writeln!(c, "double spill_{l}(long long a, long long b, {n} s, long long c, double d) {{").unwrap();
        writeln!(c, "    return (double)(a + b + c) + d + sum_{l}(s);\n}}").unwrap();
        // 構造体を 2 個 (隠しポインタも 2 個)。
        writeln!(c, "double pair_{l}({n} s, long long a, {n} t, double b) {{").unwrap();
        writeln!(c, "    return sum_{l}(s) + (double)a + sum_{l}(t) + b;\n}}").unwrap();
    }
    c
}

/// Kasane のソース。`LIB:` を呼び出し側で共有ライブラリのパスに置き換える。
fn gen_ksn(ts: &[S]) -> String {
    let mut k = String::new();
    for s in ts {
        writeln!(k, "type t.{} layout c derive [Clone, Copy]", s.name).unwrap();
        for (f, kty, _) in &s.fields {
            writeln!(k, "    {}: {}", f, kty).unwrap();
        }
        writeln!(k, "end t.{}\n", s.name).unwrap();
    }
    for s in ts {
        let (n, l) = (s.name, s.name.to_lowercase());
        writeln!(k, "extern t.make_{}(seed: i32) -> t.{} link \"LIB:make_{}\"\n", l, n, l).unwrap();
        writeln!(k, "extern t.sum_{}(s: t.{}) -> f64 link \"LIB:sum_{}\"\n", l, n, l).unwrap();
        writeln!(k, "extern t.echo_{}(s: t.{}) -> t.{} link \"LIB:echo_{}\"\n", l, n, n, l).unwrap();
        writeln!(k, "extern t.cb_{}(f: fn(t.{}) -> t.{} uses [] panics never, s: t.{}) -> t.{} link \"LIB:cb_{}\"\n", l, n, n, n, n, l).unwrap();
        writeln!(k, "extern t.bump_{}(p: *mut t.{}) -> () link \"LIB:bump_{}\"\n", l, n, l).unwrap();
        writeln!(k, "extern t.spill_{}(a: i64, b: i64, s: t.{}, c: i64, d: f64) -> f64 link \"LIB:spill_{}\"\n", l, n, l).unwrap();
        writeln!(k, "extern t.pair_{}(s: t.{}, a: i64, t: t.{}, b: f64) -> f64 link \"LIB:pair_{}\"\n", l, n, n, l).unwrap();
        writeln!(k, "extern t.size_{}() -> f64 link \"LIB:size_{}\"\n", l, l).unwrap();
        // コールバック: 欄すべてに 1 を足して返す。
        writeln!(k, "fn t.cbfn_{}(s: t.{}) -> t.{} uses [] panics never", l, n, n).unwrap();
        writeln!(k, "    let mut r = s").unwrap();
        for (f, kty, _) in &s.fields {
            writeln!(k, "    r.{} = {}", f, bump(kty, &format!("r.{}", f))).unwrap();
        }
        writeln!(k, "    r\nend t.cbfn_{}\n", l).unwrap();
    }
    for s in ts {
        let (n, l) = (s.name, s.name.to_lowercase());
        let sum20: i64 = (0..s.fields.len() as i64).map(|j| 20 + j).sum();
        writeln!(k, "fn t.test_{}() -> () uses [] panics may", l).unwrap();
        let b: Vec<String> =
            s.fields.iter().enumerate().map(|(j, (f, kty, _))| format!("{}: {}", f, lit(kty, 20 + j as i64))).collect();
        writeln!(k, "    let b = t.{} {{ {} }}", n, b.join(", ")).unwrap();
        // C を呼ぶところは 1 つの unsafe のブロックにまとめる (E5003)。検査はその中でも外でも書ける。
        writeln!(k, "    let mut m = b").unwrap();
        writeln!(k, "    unsafe \"ffi\" \"C calls for t.{}\" {{", n).unwrap();
        writeln!(k, "        let a = t.make_{}(10i32)", l).unwrap();
        k.push_str(&check_fields("a", &s.fields, 10, "    "));
        writeln!(k, "        core.check.is_true(t.size_{}() == {}.0f64)", l, csize(s)).unwrap();
        writeln!(k, "        core.check.is_true(t.sum_{}(b) == {}.0f64)", l, sum20).unwrap();
        writeln!(k, "        let e = t.echo_{}(b)", l).unwrap();
        k.push_str(&check_fields("e", &s.fields, 20, "    "));
        // C → Kasane (構造体を受けて返すコールバック)。
        writeln!(k, "        let c = t.cb_{}(t.cbfn_{}, b)", l, l).unwrap();
        k.push_str(&check_fields("c", &s.fields, 21, "    "));
        // レジスタが足りない位置・構造体を 2 個。
        writeln!(k, "        core.check.is_true(t.spill_{}(1i64, 2i64, b, 6i64, 0.5f64) == {}.5f64)", l, 9 + sum20).unwrap();
        writeln!(k, "        core.check.is_true(t.pair_{}(b, 1i64, b, 0.5f64) == {}.5f64)", l, 2 * sum20 + 1).unwrap();
        // ポインタ渡し (C が書き換える)。
        writeln!(k, "        t.bump_{}(core.ptr.from_mut(&mut m))", l).unwrap();
        writeln!(k, "    }}").unwrap();
        k.push_str(&check_fields("m", &s.fields, 21, ""));
        writeln!(k, "end t.test_{}\n", l).unwrap();
    }
    writeln!(k, "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may").unwrap();
    for s in ts {
        writeln!(k, "    t.test_{}()", s.name.to_lowercase()).unwrap();
    }
    writeln!(k, "    core.mem.drop(std.io.print_line(\"win64 abi ok\"))").unwrap();
    writeln!(k, "    Ok(())\nend t.main").unwrap();
    k
}

/// 一時パッケージ: C を gcc で `.so`、mingw で `.dll` にビルドし、ホスト (x86_64) のソースを置く。
/// Windows のソース (`link` だけが違う) を返す。
fn abi_pkg(name: &str) -> (Tmp, String) {
    let ts = types();
    // Win64 の分類の自己確認: 1・2・4・8 はレジスタ、3・12・16・24 は隠しポインタ。
    let sizes: Vec<usize> = ts.iter().map(|s| csize(s)).collect();
    assert_eq!(sizes, vec![1, 2, 4, 4, 8, 8, 8, 8, 3, 12, 12, 16, 16, 16, 24, 24, 24], "構造体の大きさが Win64 の分類と違う");
    let t = Tmp::new(name, "");
    fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t\"]\n\n[dependencies]", 1)).unwrap();
    let c = t.0.join("abi.c");
    fs::write(&c, gen_c(&ts)).unwrap();
    let so = t.0.join("libabi.so");
    let o = cc(&["-shared", "-fPIC", "-O1", "-o", so.to_str().unwrap(), c.to_str().unwrap()], &t.0);
    assert!(o.status.success(), "gcc: {}", text(&o.stderr));
    let dll = t.0.join("libabi.dll");
    let o = Command::new(MINGW_CC)
        .args(["-shared", "-fPIC", "-O1", "-o", dll.to_str().unwrap(), c.to_str().unwrap()])
        .output()
        .expect("spawn mingw gcc");
    assert!(o.status.success(), "{}: {}", MINGW_CC, text(&o.stderr));
    // Windows の動的ロードは `LoadLibraryA`。絶対パスは wine では `Z:` が `/`。
    let win = gen_ksn(&ts).replace("LIB:", &format!("Z:{}:", dll.to_str().unwrap()));
    let host = gen_ksn(&ts).replace("LIB:", &format!("{}:", so.to_str().unwrap()));
    fs::write(t.0.join("src").join("t.ksnl"), host).unwrap();
    t.fmt();
    (t, win)
}

#[test]
#[ignore = "Win64 構造体の値渡しの extern 未実装"]
fn win64_struct_by_value_extern_matches_x86_64_native_under_wine() {
    if !cross_ready() {
        return;
    }
    let (t, win_src) = abi_pkg("win_ffi_struct");
    // x86_64 のネイティブ (System V AMD64)。退避は許さない。
    let h = run(&["build"], &t.0);
    let hs = text(&h.stdout);
    assert_eq!(h.status.code(), Some(0), "host build\n{}{}", hs, text(&h.stderr));
    assert!(!hs.contains("backend skipped"), "host build fell back to the bundle\n{}", hs);
    let hexe = native_exe(&hs, &t.0);
    let hr = Command::new(&hexe).current_dir(&t.0).stdin(Stdio::null()).output().expect("run x86_64 native");
    assert_eq!(text(&hr.stdout), WANT, "x86_64: {}{}", text(&hr.stdout), text(&hr.stderr));
    assert_eq!(hr.status.code(), Some(0), "x86_64: {}", text(&hr.stderr));

    // Windows (Win64)。wine で動かす。
    fs::write(t.0.join("src").join("t.ksnl"), &win_src).unwrap();
    let _ = run(&["fmt"], &t.0);
    let a = run(&["build", "--target", TRIPLE], &t.0);
    let asout = text(&a.stdout);
    assert_eq!(a.status.code(), Some(0), "windows build\n{}{}", asout, text(&a.stderr));
    assert!(!asout.contains("backend skipped"), "windows build fell back to the bundle\n{}", asout);
    let aexe = native_exe(&asout, &t.0);
    assert!(aexe.starts_with(t.0.join("target").join(TRIPLE)), "出力が target/{} の下にない: {}", TRIPLE, aexe.display());
    let ar = run_wine(&aexe, &t.0);
    assert_eq!(text(&ar.stdout), WANT, "windows: {}", text(&ar.stderr));
    assert_eq!(ar.status.code(), Some(0), "windows: {}", text(&ar.stderr));
    // x86_64 のネイティブと一致する (Win64 の呼び出し規約でも結果は同じ)。
    assert_eq!(text(&ar.stdout), text(&hr.stdout), "wine (Win64) と x86_64 で標準出力が違う");
    assert_eq!(ar.status.code(), hr.status.code(), "wine (Win64) と x86_64 で終了コードが違う");
}