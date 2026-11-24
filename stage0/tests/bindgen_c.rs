//! `ksn bindgen --c <header.h>` (N2) のテスト: libclang で C の見出しから Kasane の FFI 宣言を生成し、
//! 生成物が `ksn check` / `ksn fmt --check` を通り、C ライブラリ (gcc で共有ライブラリにしたもの) を
//! `ksn run` とネイティブで同じ結果になることを見る。
//! libclang がない環境では飛ばす。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use common::*;

const MAIN: &str = r#"fn sample.main.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe "ffi" "typedef は元の型に解決される" {
        sample.sample_add(3u64, 4u64)
    }
    core.check.eq(a, 7u64)
    core.check.eq(sample.SAMPLE_MAX, 64i32)
    core.check.eq(sample.SAMPLE_MASK, 255i32)
    core.check.eq(sample.SAMPLE_NEG, -3i32)
    core.check.eq(sample.SAMPLE_BIG, 4294967296i64)
    core.check.eq(sample.SAMPLE_BLUE, 4u32)
    let r = unsafe "ffi" "構造体を値で返す" {
        sample.sample_rect(0.0f64, 0.0f64, 3.0f64, 2.0f64)
    }
    let area = unsafe "ffi" "構造体を値で渡す" {
        sample.sample_area(r)
    }
    core.check.eq(area, 6i32)
    let total = unsafe "ffi" "C から Kasane のコールバックを呼ぶ" {
        sample.sample_each(r, sample.main.cb, core.ptr.null_mut())
    }
    core.check.eq(total, 30i32)
    let mut g = r
    unsafe "ffi" "構造体へのポインタで書き換える" {
        sample.sample_grow(core.ptr.from_mut(&mut g), 1.0f64)
    }
    let grown = unsafe "ffi" "grow 後の area" {
        sample.sample_area(g)
    }
    core.check.eq(grown, 20i32)
    core.mem.drop(std.io.print_line("bindgen_c ok"))
    Ok(())
end sample.main.main

fn sample.main.cb(code: i32, _user: *mut u8) -> i32 uses [] panics never
    code.wrapping_mul(10i32)
end sample.main.cb
"#;

const TOML: &str = "[package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"sample.main.main\"\n\n[unsafe]\nmodules = [\"sample\", \"sample.main\"]\n\n[dependencies]\n";

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/bindgen_c")
}

/// libclang があるか。なければこのテストは全部飛ぶ。
fn libclang_available() -> bool {
    let d = std::env::temp_dir().join(format!("ksn_bindgen_c_probe_{}", std::process::id()));
    let _ = fs::create_dir_all(&d);
    let o = Command::new(exe())
        .args(["bindgen", "--c", data_dir().join("sample.h").to_str().unwrap(), "-o", d.join("x.ksnh").to_str().unwrap()])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&d);
    let err = text(&o.stderr);
    if err.contains("libclang") {
        eprintln!("skip: libclang がないので bindgen --c のテストは飛ばす: {}", err);
        return false;
    }
    true
}

/// bindgen --c で生成したパッケージを作る (C ライブラリも gcc で .so にする)。libclang がないときは None。
#[cfg(unix)]
fn bindgen_pkg(name: &str) -> Option<Tmp> {
    if !libclang_available() {
        return None;
    }
    let t = Tmp::new_raw(name, "");
    let _ = fs::remove_file(t.0.join("src").join("t.ksnl"));
    fs::write(t.0.join("kasane.toml"), TOML).unwrap();
    let header = data_dir().join("sample.h");
    let o = run(
        &["bindgen", "--c", header.to_str().unwrap(), "--prefix", "sample", "--link", "./libsample.so", "-o", "src/sample.ksnh"],
        &t.0,
    );
    assert_eq!(o.status.code(), Some(0), "bindgen: {}{}", text(&o.stdout), text(&o.stderr));
    // C ライブラリを共有ライブラリにする
    let lib = t.0.join("libsample.so");
    let o = common::cc(
        &["-shared", "-fPIC", "-O2", "-I", data_dir().to_str().unwrap(), "-o", lib.to_str().unwrap(), data_dir().join("sample.c").to_str().unwrap()],
        &t.0,
    );
    assert!(o.status.success(), "cc: {}", text(&o.stderr));
    fs::create_dir_all(t.0.join("src").join("sample")).unwrap();
    fs::write(t.0.join("src").join("sample").join("main.ksnl"), MAIN).unwrap();
    let f = run(&["fmt"], &t.0);
    assert_eq!(f.status.code(), Some(0), "fmt: {}{}", text(&f.stdout), text(&f.stderr));
    Some(t)
}

#[cfg(unix)]
#[test]
fn bindgen_c_generates_declarations_that_pass_check_and_fmt() {
    let Some(t) = bindgen_pkg("bindgen_c_check") else { return };
    let ksnh = fs::read_to_string(t.0.join("src/sample.ksnh")).unwrap();
    let ksnl = fs::read_to_string(t.0.join("src/sample.ksnl")).unwrap();
    // 生成物の中身 (構造体・enum 定数・#define・typedef・関数ポインタ・extern)
    assert!(ksnl.contains("type sample.SampleRect layout c"), "{}", ksnl);
    assert!(ksnl.contains("    min: sample.SamplePoint"), "{}", ksnl);
    assert!(ksnl.contains("type sample.SampleBuf layout c derive [Clone, Copy] pub\n    name: [i8; 16]\n    len: u64"), "{}", ksnl);
    assert!(ksnl.contains("const sample.SAMPLE_MAX: i32 = 64i32 pub"), "{}", ksnl);
    assert!(ksnl.contains("const sample.SAMPLE_NEG: i32 = -3i32 pub"), "{}", ksnl);
    assert!(ksnl.contains("const sample.SAMPLE_BLUE: u32 = 4u32 pub"), "{}", ksnl);
    assert!(ksnl.contains("extern sample.raw_sample_add(a: u64, b: u64) -> u64 link \"./libsample.so:sample_add\""), "{}", ksnl);
    assert!(ksnl.contains("fn sample.sample_add(a: u64, b: u64) -> u64 uses [] panics may pub"), "{}", ksnl);
    assert!(ksnl.contains("extern sample.raw_sample_each(r: sample.SampleRect, f: fn(i32, *mut u8) -> i32 uses [] panics never, user: *mut u8) -> i32 link \"./libsample.so:sample_each\""), "{}", ksnl);
    assert!(ksnl.contains("extern sample.raw_sample_grow(r: *mut sample.SampleRect, d: f64) -> ()"), "{}", ksnl);
    assert!(ksnl.contains("type sample.SampleRect layout c derive [Clone, Copy] pub"), "{}", ksnl);
    // 飛ばしたものは理由つきでコメントに残る
    assert!(ksnh.contains("ビットフィールド"), "{}", ksnh);
    assert!(ksnh.contains("varargs"), "{}", ksnh);
    // .ksnh と .ksnl は strip の関係 (E9050) で、両方 ksn fmt の形 (E0020)
    assert_eq!(crate_ksnh_strip(&ksnh), ksnl);
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "check: {}{}", text(&c.stdout), text(&c.stderr));
    let f = run(&["fmt", "--check"], &t.0);
    assert_eq!(f.status.code(), Some(0), "fmt --check: {}{}", text(&f.stdout), text(&f.stderr));
}

#[cfg(unix)]
#[test]
fn bindgen_c_calls_the_c_library_the_same_in_run_and_native() {
    let Some(t) = bindgen_pkg("bindgen_c_run") else { return };
    let i = run(&["run"], &t.0);
    assert_eq!(i.status.code(), Some(0), "run: {}", text(&i.stderr));
    assert_eq!(text(&i.stdout), "bindgen_c ok\n", "{}", text(&i.stdout));
    // ネイティブ (LLVM) でも同じ出力。下ろせなければ理由つきのバンドル退避でよい
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_bindgen_c_run", std::process::id()));
    let b = Command::new(exe()).args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    let out = text(&b.stdout) + &text(&b.stderr);
    let bin = t.0.join("target/debug/sample");
    let launcher = if crate::common::artifact_is_native(&bin) { bin } else { t.0.join("target/debug/sample") };
    assert!(launcher.exists(), "ネイティブもバンドルもない: {}", out);
    let n = Command::new(&launcher).current_dir(&t.0).output().unwrap();
    assert_eq!(n.status.code(), Some(0), "native: {}", text(&n.stderr));
    assert_eq!(text(&n.stdout), text(&i.stdout), "native: {}", text(&n.stderr));
}

#[test]
fn bindgen_c_rejects_bad_arguments() {
    if !libclang_available() {
        return;
    }
    let d = std::env::temp_dir().join(format!("ksn_bindgen_c_args_{}", std::process::id()));
    let _ = fs::create_dir_all(&d);
    let h = data_dir().join("sample.h");
    // -o がない
    let o = run(&["bindgen", "--c", h.to_str().unwrap()], &d);
    assert_eq!(o.status.code(), Some(2));
    // 未知の引数
    let o = run(&["bindgen", "--c", h.to_str().unwrap(), "-o", d.join("x.ksnh").to_str().unwrap(), "--wat"], &d);
    assert_eq!(o.status.code(), Some(2));
    // prefix が予約語
    let o = run(&["bindgen", "--c", h.to_str().unwrap(), "--prefix", "fn", "-o", d.join("x.ksnh").to_str().unwrap()], &d);
    assert_eq!(o.status.code(), Some(2));
    // 出力の拡張子が違う
    let o = run(&["bindgen", "--c", h.to_str().unwrap(), "-o", d.join("x.txt").to_str().unwrap()], &d);
    assert_eq!(o.status.code(), Some(2));
    let _ = fs::remove_dir_all(&d);
}

const ENV_TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main.main\"\n\n[unsafe]\nmodules = [\"t.c\", \"t.main\"]\n\n[dependencies]\n";

const ENV_MAIN: &str = r#"fn t.main.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.c.MINI_LIMIT, 42i32)
    let r = unsafe "ffi" "bindgen の生成物を呼ぶ" {
        t.c.mini_add(1i32, 2i32)
    }
    core.check.eq(r, 3i32)
    core.mem.drop(std.io.print_line("bindgen env ok"))
    Ok(())
end t.main.main
"#;

/// 素のシェル相当の環境 (`env_clear` して PATH と HOME だけ) でも、<stdint.h>/<stddef.h> を含む
/// 見出しが解析できて、生成物が `ksn check` を通ること (resource dir とシステム include を明示する話)。
/// 出力先 `src/t/c.ksnh` は module `t.c` の正しい置き場なので警告が出ないことも見る。
#[cfg(unix)]
#[test]
fn bindgen_c_standalone_minimal_env() {
    if !libclang_available() {
        return;
    }
    let t = Tmp::new_raw("bindgen_c_env", "");
    let _ = fs::remove_file(t.0.join("src").join("t.ksnl"));
    fs::write(t.0.join("kasane.toml"), ENV_TOML).unwrap();
    fs::create_dir_all(t.0.join("src").join("t")).unwrap();
    fs::write(t.0.join("src").join("t").join("main.ksnl"), ENV_MAIN).unwrap();
    let f = run(&["fmt"], &t.0);
    assert_eq!(f.status.code(), Some(0), "fmt: {}{}", text(&f.stdout), text(&f.stderr));
    let h = t.0.join("h.h");
    fs::write(
        &h,
        "#include <stdint.h>\n#include <stddef.h>\n\ntypedef struct { int32_t x; size_t n; } MiniPoint;\n#define MINI_LIMIT 42\nint32_t mini_add(int32_t a, int32_t b);\n",
    )
    .unwrap();
    let mut cmd = Command::new(exe());
    cmd.args(["bindgen", "--c", h.to_str().unwrap(), "--prefix", "t.c", "-o", "src/t/c.ksnh"])
        .current_dir(&t.0)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default());
    let o = cmd.output().unwrap();
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "bindgen (環境最小): {}{}", text(&o.stdout), err);
    assert!(!err.contains("module 名と違う"), "誤った警告が出た: {}", err);
    let ksnl = fs::read_to_string(t.0.join("src").join("t").join("c.ksnl")).unwrap();
    assert!(ksnl.contains("type t.c.MiniPoint layout c"), "{}", ksnl);
    assert!(ksnl.contains("const t.c.MINI_LIMIT: i32 = 42i32 pub"), "{}", ksnl);
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "check: {}{}", text(&c.stdout), text(&c.stderr));
}

/// テスト側から `ksnh::strip` と同じ変換 (コメント行を消す) をする。
fn crate_ksnh_strip(ksnh: &str) -> String {
    let lines: Vec<&str> = ksnh.split('\n').collect();
    let head = lines.iter().take_while(|l| l.trim_start_matches(' ').starts_with("//")).count();
    let mut keep: Vec<bool> = lines.iter().map(|l| !l.trim_start_matches(' ').starts_with("//")).collect();
    if head > 0 && head < lines.len() && lines[head].is_empty() && head + 1 < lines.len() {
        keep[head] = false;
    }
    let out: Vec<&str> = lines.iter().zip(&keep).filter(|(_, k)| **k).map(|(l, _)| *l).collect();
    out.join("\n")
}
