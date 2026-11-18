//! キャッシュ・増分検査・LSP のテスト。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use common::*;

#[test]
fn build_produces_artifacts_and_is_cached() {
    let d = root().join("examples").join("native");
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let b = |release: bool| {
        let mut c = Command::new(exe());
        c.arg("build").current_dir(&d).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        c.output().unwrap()
    };
    let o = b(true);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let tgt = d.join("target").join("release");
    assert!(tgt.join("ksn.exe").exists() || tgt.join("ksn").exists());
    let again = b(true);
    assert_eq!(again.status.code(), Some(0));
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn the_realtime_example_runs_natively_through_the_native_build() {
    let d = root().join("examples").join("realtime");
    let cache = std::env::temp_dir().join(format!("ksn_cache_rt_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&d).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        let kind = if release { "release" } else { "debug" };
        let p = d.join("target").join(kind).join(if cfg!(windows) { "realtime.exe" } else { "realtime.native" });
        let p = native_or_launcher(p, &out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn the_demo_example_runs_natively_through_the_native_build() {
    let d = root().join("examples").join("demo");
    let cache = std::env::temp_dir().join(format!("ksn_cache_demo_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    let o = Command::new(exe()).args(["build", "--backend", "llvm"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    let p = d.join("target").join("debug").join(if cfg!(windows) { "demo.exe" } else { "demo.native" });
    let p = native_or_launcher(p, &out);
    let r = Command::new(&p).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn the_json_example_runs_natively_through_the_native_build() {
    let d = root().join("examples").join("json");
    let cache = std::env::temp_dir().join(format!("ksn_cache_json_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&d).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        let kind = if release { "release" } else { "debug" };
        let p = d.join("target").join(kind).join(if cfg!(windows) { "json.exe" } else { "json.native" });
        let p = native_or_launcher(p, &out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
    let _ = fs::remove_dir_all(&cache);
}

/// `HashMap` / `BtreeMap` の `get_mut` (返り値の参照からの読み書き) と、`HashMap.iter` の挿入順。
/// `BtreeMap.iter` は仕様 (`spec/stdlib/alloc/collections.sig`) に無く、`ksn check` が E1108 で弾くので含めない。
fn clif_map_methods_program() -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)",
        "    core.mem.drop(match m.insert(3u32, 30u32) {",
        "        Ok(_o) => 0u8",
        "        Err(_e) => 1u8",
        "    })",
        "    core.mem.drop(match m.insert(1u32, 10u32) {",
        "        Ok(_o) => 0u8",
        "        Err(_e) => 1u8",
        "    })",
        "    core.mem.drop(match m.insert(2u32, 20u32) {",
        "        Ok(_o) => 0u8",
        "        Err(_e) => 1u8",
        "    })",
        "    let mut vseq = 0u32",
        "    let mut kseq = 0u32",
        "    for (k, v) in m.iter() {",
        "        vseq = vseq.wrapping_mul(100u32).wrapping_add(*v)",
        "        kseq = kseq.wrapping_mul(10u32).wrapping_add(*k)",
        "    }",
        "    core.check.eq(vseq, 301020u32)",
        "    core.check.eq(kseq, 312u32)",
        "    match m.get_mut(1u32) {",
        "        Some(r) => {",
        "            *r = 111u32",
        "        }",
        "        None => {",
        "            core.check.eq(true, false)",
        "        }",
        "    }",
        "    core.check.eq(m.get(&1u32), Some(&111u32))",
        "    core.check.eq(m.get_mut(9u32).is_none(), true)",
        "    let mut b = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)",
        "    core.mem.drop(match b.insert(3u32, 30u32) {",
        "        Ok(_o) => 0u8",
        "        Err(_e) => 1u8",
        "    })",
        "    core.mem.drop(match b.insert(1u32, 10u32) {",
        "        Ok(_o) => 0u8",
        "        Err(_e) => 1u8",
        "    })",
        "    core.mem.drop(match b.insert(2u32, 20u32) {",
        "        Ok(_o) => 0u8",
        "        Err(_e) => 1u8",
        "    })",
        "    match b.get_mut(2u32) {",
        "        Some(r) => {",
        "            *r = 222u32",
        "        }",
        "        None => {",
        "            core.check.eq(true, false)",
        "        }",
        "    }",
        "    core.check.eq(b.get(&2u32), Some(&222u32))",
        "    core.check.eq(b.get_mut(8u32).is_none(), true)",
        "    core.mem.drop(std.io.print_line(\"map methods ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_llvm_backend_compiles_map_get_mut_and_hashmap_iter() {
    let t = Tmp::new_raw("clif_map_meth", &clif_map_methods_program());
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let cache = std::env::temp_dir().join(format!("ksn_cache_mapmeth_{}_{}", std::process::id(), release));
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
        if release {
            c.args(["--release", "--allow-bundle"]);
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        let kind = if release { "release" } else { "debug" };
        let p = t.0.join("target").join(kind).join(if cfg!(windows) { "t.exe" } else { "t.native" });
        let p = native_or_launcher(p, &out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ ({})", kind);
        let _ = fs::remove_dir_all(&cache);
    }
}

/// `std.sync.Mutex`（`spawn` と同じく同期実行）。`lock` は値のアドレスをそのままガードとして返し、
/// `get` / `get_mut` は同じ場所を読み書きする。ガードの `Drop` は no-op。
fn clif_sync_program() -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.main() -> Result<(), std.io.Error> uses [io.console, thread] panics may",
        "    let m = std.sync.Mutex.new(5u32)",
        "    let mut g = m.lock()",
        "    core.check.eq(*g.get(), 5u32)",
        "    *g.get_mut() = 42u32",
        "    core.check.eq(*g.get(), 42u32)",
        "    core.mem.drop(g)",
        "    core.mem.drop(std.io.print_line(\"sync ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_llvm_backend_compiles_mutex_lock_and_guard() {
    let t = Tmp::new_raw("clif_sync", &clif_sync_program());
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let cache = std::env::temp_dir().join(format!("ksn_cache_sync_{}_{}", std::process::id(), release));
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
        if release {
            c.args(["--release", "--allow-bundle"]);
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        let kind = if release { "release" } else { "debug" };
        let p = t.0.join("target").join(kind).join(if cfg!(windows) { "t.exe" } else { "t.native" });
        let p = native_or_launcher(p, &out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ ({})", kind);
        let _ = fs::remove_dir_all(&cache);
    }
}

#[test]
fn the_features_example_runs_natively_through_the_native_build() {
    let d = root().join("examples").join("features");
    let cache = std::env::temp_dir().join(format!("ksn_cache_feat_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&d).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        let kind = if release { "release" } else { "debug" };
        let p = d.join("target").join(kind).join(if cfg!(windows) { "features.exe" } else { "features.native" });
        let p = native_or_launcher(p, &out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn the_threads_example_runs_natively_through_the_native_build() {
    let d = root().join("examples").join("threads");
    let cache = std::env::temp_dir().join(format!("ksn_cache_thr_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    let o = Command::new(exe()).args(["build", "--backend", "llvm"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    let p = d.join("target").join("debug").join(if cfg!(windows) { "threads.exe" } else { "threads.native" });
    let p = native_or_launcher(p, &out);
    let r = Command::new(&p).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn the_tour_example_runs_natively_through_the_native_build() {
    let d = root().join("examples").join("tour");
    let cache = std::env::temp_dir().join(format!("ksn_cache_tour_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&d).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        let kind = if release { "release" } else { "debug" };
        let p = d.join("target").join(kind).join(if cfg!(windows) { "tour.exe" } else { "tour.native" });
        let p = native_or_launcher(p, &out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn ffi_extern_calls_and_raw_pointers_work_natively() {
    let t = Tmp::new_raw("ffi_native", FFI_SRC);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_ffi", std::process::id()));
    let b = Command::new(exe())
        .args(["build", "--backend", "llvm"])
        .current_dir(&t.0)
        .env("KSN_CACHE_DIR", &cache)
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&cache);
    let bin = t.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t.native" });
    let bin = native_or_launcher(bin, &(text(&b.stdout) + &text(&b.stderr)));
    let r = Command::new(&bin).output().unwrap();
    assert_eq!(text(&r.stdout), "ffi ok\n");
}

/// 例 `examples/http` (ループバックの HTTP 往復) が、ネイティブでも `ksn run` と同じ出力・終了コードになる。
#[cfg(unix)]
#[test]
fn the_http_example_runs_natively_through_the_native_build() {
    let d = root().join("examples").join("http");
    let cache = std::env::temp_dir().join(format!("ksn_cache_http_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&d).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        assert_native_or_reasoned_skip(&out);
        let kind = if release { "release" } else { "debug" };
        let p = d.join("target").join(kind).join("http.native");
        let p = native_or_launcher(p, &out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), interp.status.code(), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn the_shortener_example_builds_natively_without_skips_and_matches_the_interpreter() {
    if cfg!(windows) {
        return;
    }
    let d = root().join("examples").join("shortener");
    let cache = std::env::temp_dir().join(format!("ksn_cache_short_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    let o = Command::new(exe()).args(["build", "--backend", "llvm"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_native_or_reasoned_skip(&out);
    let p = native_or_launcher(d.join("target").join("debug").join("shortener.native"), &out);
    let r = Command::new(&p).current_dir(&d).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), text(&interp.stdout));
    let _ = fs::remove_dir_all(&cache);
}

/// `examples/plugins` (`Vec<Slot>` の `Box<dyn Handler>`) は、ネイティブで skip なしに作れ、インタプリタと同じ出力になる。
#[test]
fn the_plugins_example_builds_natively_without_skips_and_matches_the_interpreter() {
    if cfg!(windows) {
        return;
    }
    let d = root().join("examples").join("plugins");
    let cache = std::env::temp_dir().join(format!("ksn_cache_plugins_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    assert_eq!(text(&interp.stdout), "start\nstop\nplugins ok\n");
    for release in [false, true] {
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&d).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        assert_native_or_reasoned_skip(&out);
        let p = native_or_launcher(d.join("target").join(if release { "release" } else { "debug" }).join("plugins.native"), &out);
        let r = Command::new(&p).current_dir(&d).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout));
    }
    let _ = fs::remove_dir_all(&cache);
}

#[cfg(unix)]
#[test]
fn ffi_unit_tests_run_in_both_the_interpreter_and_the_native_bundle() {
    let t = ffi_pkg("ffi_unit", "unit.ksnl");
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_ffi_unit", std::process::id()));
    let o = Command::new(exe()).args(["test", "--both"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stdout).contains("1 passed; 0 failed"), "{}", text(&o.stdout));
}

#[cfg(target_os = "linux")]
#[test]
fn the_ffi_example_checks_formats_tests_and_runs_the_same_in_both_backends() {
    // examples/ffi: libc の `qsort` (Kasane の比較関数) と libm の `cbrt` (動的ロード)。C の準備なしで動く。
    let d = root().join("examples").join("ffi");
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_ffi_example", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    assert_eq!(run(&["check"], &d).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &d).status.code(), Some(0));
    let t = Command::new(exe()).args(["test", "--both"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    assert_eq!(t.status.code(), Some(0), "{}{}", text(&t.stdout), text(&t.stderr));
    let interp = run(&["run"], &d);
    assert_eq!(text(&interp.stdout), "ffi ok\n", "{}", text(&interp.stderr));
    let b = Command::new(exe()).args(["build", "--backend", "llvm"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let out = text(&b.stdout) + &text(&b.stderr);
    assert_native_or_reasoned_skip(&out);
    let n = Command::new(native_or_launcher(d.join("target").join("debug").join("ffi.native"), &out)).current_dir(&d).output().unwrap();
    assert_eq!(text(&n.stdout), text(&interp.stdout));
    let _ = fs::remove_dir_all(d.join("target"));
    let _ = fs::remove_dir_all(&cache);
}

#[cfg(unix)]
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if e.file_type().unwrap().is_dir() {
            if e.file_name() != "target" {
                copy_dir(&src, &dst);
            }
        } else {
            fs::copy(&src, &dst).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn the_ffi_shapes_example_builds_its_c_library_and_passes_in_both_backends() {
    // examples/ffi/shapes: 同梱の C (c/shapes.c) を build.sh で共有ライブラリにして、動的ロードで呼ぶ。
    // 構造体の値渡し・値返し (MEMORY / INTEGER / SSE)・ポインタ渡し・ユーザーデータ付きのコールバック。
    let d = std::env::temp_dir().join(format!("ksn_shapes_{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    copy_dir(&root().join("examples").join("ffi").join("shapes"), &d);
    let o = Command::new("sh").arg("build.sh").current_dir(&d).output().unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_shapes", std::process::id()));
    assert_eq!(run(&["check"], &d).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &d).status.code(), Some(0));
    let t = Command::new(exe()).args(["test", "--both"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    assert_eq!(t.status.code(), Some(0), "{}{}", text(&t.stdout), text(&t.stderr));
    let interp = run(&["run"], &d);
    assert_eq!(text(&interp.stdout), "shapes ok\n", "{}", text(&interp.stderr));
    let b = Command::new(exe()).args(["build", "--backend", "llvm"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let out = text(&b.stdout) + &text(&b.stderr);
    assert_native_or_reasoned_skip(&out);
    let n = Command::new(native_or_launcher(d.join("target").join("debug").join("shapes.native"), &out)).current_dir(&d).output().unwrap();
    assert_eq!(text(&n.stdout), text(&interp.stdout));
    let _ = fs::remove_dir_all(&d);
    let _ = fs::remove_dir_all(&cache);
}

fn clif_hashmap_index_program() -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    match t.run() {",
        "        Ok(_u) => core.check.eq(1u8, 1u8)",
        "        Err(_e) => core.panic.panic(\"oom\")",
        "    }",
        "    std.io.print_line(\"index ok\")?",
        "    Ok(())",
        "end t.main",
        "",
        "fn t.run() -> Result<(), alloc.alloc.AllocError> uses [alloc] panics may",
        "    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)",
        "    let mut i = 0u32",
        "    while i < 300u32 {",
        "        core.check.eq(m.insert(i, i.wrapping_mul(3u32))?, None)",
        "        i = i.wrapping_add(1u32)",
        "    }",
        "    core.check.eq(m.len(), 300usize)",
        "    i = 0u32",
        "    while i < 300u32 {",
        "        if i % 2u32 == 0u32 {",
        "            match m.remove(&i) {",
        "                Some(v) => core.check.eq(v, i.wrapping_mul(3u32))",
        "                None => core.panic.panic(\"missing even\")",
        "            }",
        "        }",
        "        i = i.wrapping_add(1u32)",
        "    }",
        "    core.check.eq(m.len(), 150usize)",
        "    i = 0u32",
        "    while i < 300u32 {",
        "        match m.get(&i) {",
        "            Some(v) => core.check.eq(*v, i.wrapping_mul(3u32))",
        "            None => core.check.eq(i % 2u32, 0u32)",
        "        }",
        "        i = i.wrapping_add(1u32)",
        "    }",
        "    i = 0u32",
        "    while i < 300u32 {",
        "        if i % 2u32 == 0u32 {",
        "            core.check.eq(m.insert(i, i.wrapping_mul(5u32))?, None)",
        "        }",
        "        i = i.wrapping_add(1u32)",
        "    }",
        "    let mut j = 0u32",
        "    for (k, v) in m.iter() {",
        "        let mut want_k = 0u32",
        "        let mut want_v = 0u32",
        "        if j < 150u32 {",
        "            want_k = j.wrapping_mul(2u32).wrapping_add(1u32)",
        "            want_v = want_k.wrapping_mul(3u32)",
        "        } else {",
        "            want_k = j.wrapping_sub(150u32).wrapping_mul(2u32)",
        "            want_v = want_k.wrapping_mul(5u32)",
        "        }",
        "        core.check.eq(*k, want_k)",
        "        core.check.eq(*v, want_v)",
        "        j = j.wrapping_add(1u32)",
        "    }",
        "    core.check.eq(j, 300u32)",
        "    let mut s = alloc.collections.HashMap.new(std.alloc.SYSTEM)",
        "    core.check.eq(s.insert(\"x\", 1u32)?, None)",
        "    core.check.eq(s.insert(\"y\", 2u32)?, None)",
        "    core.check.eq(s.insert(\"z\", 3u32)?, None)",
        "    core.check.eq(s.remove(&\"y\"), Some(2u32))",
        "    core.check.eq(s.get(&\"z\"), Some(&3u32))",
        "    core.check.eq(s.insert(\"y\", 4u32)?, None)",
        "    core.check.eq(s.len(), 3usize)",
        "    Ok(())",
        "end t.run",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_hashmap_index_stays_consistent_across_growth_removal_and_reinsertion() {
    // 索引表（ハッシュ → 組の添字）は、表が育つとき・中ほどの削除で添字が詰まるとき・再挿入のときも、
    // 検索結果と挿入順を保つ。インタプリタと LLVM (デバッグ) で同じ出力になる。
    let t = Tmp::new_raw("hm_index", &clif_hashmap_index_program());
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    assert_eq!(text(&interp.stdout).trim(), "index ok");
    let cache = std::env::temp_dir().join(format!("ksn_cache_hmindex_{}", std::process::id()));
    let o = Command::new(exe())
        .args(["build", "--backend", "llvm"])
        .current_dir(&t.0)
        .env("KSN_CACHE_DIR", &cache)
        .output()
        .unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    let p = t.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t.native" });
    let p = native_or_launcher(p, &out);
    let r = Command::new(&p).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn ffi_same_symbol_with_two_signatures_falls_back_without_a_crash() {
    // 同じ link 名を別のシグネチャで 2 つ宣言しても、内部で panic しない。LLVM では `declare` の二重定義で clang が失敗するので、
    // その失敗が理由付きで報告され、バンドルへ退避する。
    let src = "extern t.abs32(x: i32) -> i32 link \"abs\"

extern t.abs64(x: i64) -> i64 link \"abs\"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe \"libc\" \"a\" {
        t.abs32(0i32 - 5i32)
    }
    let b = unsafe \"libc\" \"b\" {
        t.abs64(0i64 - 6i64)
    }
    core.check.eq(a, 5i32)
    core.check.eq(b, 6i64)
    core.mem.drop(std.io.print_line(\"ok\"))
    Ok(())
end t.main
";
    let t = Tmp::new_raw("ffi_dup_symbol", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_ffi_dup", std::process::id()));
    let b = Command::new(exe())
        .args(["build", "--backend", "llvm"])
        .current_dir(&t.0)
        .env("KSN_CACHE_DIR", &cache)
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&cache);
    let all = format!("{}{}", text(&b.stdout), text(&b.stderr));
    assert!(!all.contains("panicked"), "{}", all);
    assert!(all.contains("native backend skipped: clang が失敗した") && all.contains("redefinition of function 'abs'"), "{}", all);
    assert!(all.contains("the interpreter bundle is used"), "{}", all);
}

/// バンドル (インタプリタ同梱の起動スクリプト) が実行権限を持ち、相対パス・PATH 経由・
/// 別ディレクトリから起動しても同梱物を exec できること。基本型の `display_fmt` は LLVM 未対応なので
/// ネイティブ生成が失敗し、バンドルへフォールバックする。
#[cfg(unix)]
#[test]
fn bundle_launcher_is_executable_and_location_independent() {
    use std::os::unix::fs::PermissionsExt;
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let n = 42u32
    let mut f = core.fmt.Formatter.new(16usize)
    core.mem.drop(n.display_fmt(&mut f))
    match core.str.from_utf8(f.written()) {
        Ok(s) => std.io.print_line(s)?
        Err(_e) => std.io.print_line(\"bad\")?
    }
    Ok(())
end t.main
";
    let t = Tmp::new_raw("bundle_launcher", src);
    let cache = std::env::temp_dir().join(format!("ksn_bundle_cache_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let o = Command::new(exe())
        .args(["build", "--release", "--allow-bundle"])
        .current_dir(&t.0)
        .env("KSN_CACHE_DIR", &cache)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let rel_dir = Path::new("target").join("release");
    let rel_dir_abs = t.0.join(&rel_dir);
    let launcher = rel_dir_abs.join("t");
    let mode = fs::metadata(&launcher).expect("launcher exists").permissions().mode();
    assert!(mode & 0o111 != 0, "launcher is not executable: mode {:o}", mode);

    let check = |o: Output, how: &str| {
        assert_eq!(o.status.code(), Some(0), "{}: {}", how, text(&o.stderr));
        assert_eq!(text(&o.stdout), "42\n", "{}", how);
    };
    // 1. パッケージ直下から相対パスで起動
    check(Command::new(rel_dir.join("t")).current_dir(&t.0).output().unwrap(), "relative path");
    // 2. 親ディレクトリから、パッケージを含む相対パスで起動
    let parent = t.0.parent().unwrap().to_path_buf();
    let pkg_name = t.0.file_name().unwrap().to_os_string();
    let via_parent = Path::new(&pkg_name).join(&rel_dir).join("t");
    check(Command::new(&via_parent).current_dir(&parent).output().unwrap(), "relative path from parent");
    // 3. 無関係なディレクトリから絶対パスで起動
    check(Command::new(&launcher).current_dir(&cache).output().unwrap(), "absolute path");
    // 4. PATH 経由 (bare name。$0 にスラッシュが無い場合の解決)
    let path = format!("{}:{}", rel_dir_abs.display(), std::env::var("PATH").unwrap_or_default());
    check(Command::new("t").env("PATH", path).current_dir(&cache).output().unwrap(), "PATH lookup");

    let _ = fs::remove_dir_all(&cache);
}

// ------------------------------------------------------------ ビルドキャッシュ (生成物・キー・バンドル)

const CACHE_SRC: &str = "fn t.main() -> () uses [] panics never
end t.main

fn t.one() -> u32 uses [] panics never
    1u32
end t.one

test t.one.test_a
    core.check.eq(t.one(), 1u32)
end t.one.test_a
";

/// `ksn build` を、指定したキャッシュ dir と追加の引数・環境変数で実行する。
fn build_with_cache(ksn: &str, cwd: &Path, cache: &Path, extra: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut c = Command::new(ksn);
    c.arg("build").args(extra).current_dir(cwd).env("KSN_CACHE_DIR", cache).env("KSN_STDLIB", root().join("spec").join("stdlib"));
    for (k, v) in envs {
        c.env(k, v);
    }
    c.output().expect("spawn ksn build")
}

#[test]
fn concurrent_builds_with_the_same_key_both_succeed() {
    // 同じキー (同じソースの別のパッケージ dir) を同じキャッシュへ並行にビルドしても、どちらも成功する。
    // 以前は作業用の `<key>.tmp` を共有して互いに消し合い、`fs::rename(..).unwrap()` で panic しえた。
    let a = Tmp::new_raw("cache_race_a", CACHE_SRC);
    let b = Tmp::new_raw("cache_race_b", CACHE_SRC);
    for round in 0..4 {
        let cache = std::env::temp_dir().join(format!("ksn_cache_race_{}_{}", std::process::id(), round));
        let _ = fs::remove_dir_all(&cache);
        let spawn = |dir: &Path| {
            Command::new(exe())
                .args(["build", "--backend", "llvm"])
                .current_dir(dir)
                .env("KSN_CACHE_DIR", &cache)
                .env("KSN_STDLIB", root().join("spec").join("stdlib"))
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        };
        let (pa, pb) = (spawn(&a.0), spawn(&b.0));
        let (oa, ob) = (pa.wait_with_output().unwrap(), pb.wait_with_output().unwrap());
        let _ = fs::remove_dir_all(&cache);
        for o in [&oa, &ob] {
            assert_eq!(o.status.code(), Some(0), "round {}: {}{}", round, text(&o.stdout), text(&o.stderr));
        }
    }
}

#[test]
fn cache_hit_reuses_native_artifact_without_compiling() {
    // 1 回目は compile_llvm を通り、生成物がキャッシュに入る。2 回目は同じキーで
    // "cache hit" となり、compile_llvm (KSN_TIME の印) を通らずに生成物を複製する。
    let t = Tmp::new_raw("cache_artifact", CACHE_SRC);
    let cache = std::env::temp_dir().join(format!("ksn_cache_art_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let exe_file = if cfg!(windows) { "t.exe" } else { "t.native" };
    let exe_path = t.0.join("target").join("debug").join(exe_file);

    let first = build_with_cache(exe(), &t.0, &cache, &["--backend", "llvm"], &[("KSN_TIME", "1")]);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    assert!(!text(&first.stdout).contains("cache hit"), "{}", text(&first.stdout));
    assert!(text(&first.stderr).contains("KSN_TIME compile_llvm_total"), "{}", text(&first.stderr));
    let built = fs::read(&exe_path).expect("native executable after first build");

    // 生成物だけ消し、キャッシュから戻ることを確かめる。
    fs::remove_file(&exe_path).unwrap();
    let second = build_with_cache(exe(), &t.0, &cache, &["--backend", "llvm"], &[("KSN_TIME", "1")]);
    assert_eq!(second.status.code(), Some(0), "{}", text(&second.stderr));
    assert!(text(&second.stdout).contains("cache hit"), "{}", text(&second.stdout));
    assert!(
        !text(&second.stderr).contains("KSN_TIME compile_llvm"),
        "cache hit must skip compile_llvm: {}",
        text(&second.stderr)
    );
    assert!(text(&second.stdout).contains("native executable: "), "{}", text(&second.stdout));
    assert_eq!(fs::read(&exe_path).expect("restored executable"), built);
    let run = Command::new(&exe_path).output().unwrap();
    assert_eq!(run.status.code(), Some(0));

    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn build_key_changes_with_ksn_binary_profile_and_backend() {
    // キーは ksn 本体のバイト列・debug/release・バックエンドで分かれる。
    // ksn 本体の変化は、同じ実行ファイルの末尾にバイトを足した別物で代用する (Linux / Windows は末尾を無視して動く)。
    let t = Tmp::new_raw("cache_key", CACHE_SRC);
    let cache = std::env::temp_dir().join(format!("ksn_cache_key_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let bin_dir = std::env::temp_dir().join(format!("ksn_cache_key_bin_{}", std::process::id()));
    let _ = fs::remove_dir_all(&bin_dir);
    fs::create_dir_all(&bin_dir).unwrap();
    let ksn_a = bin_dir.join(format!("ksn_a{}", std::env::consts::EXE_SUFFIX));
    let ksn_b = bin_dir.join(format!("ksn_b{}", std::env::consts::EXE_SUFFIX));
    fs::copy(exe(), &ksn_a).unwrap();
    fs::copy(exe(), &ksn_b).unwrap();
    {
        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(&ksn_b).unwrap();
        f.write_all(b"\0ksn-cache-key-variant\0").unwrap();
    }
    let (a, b) = (ksn_a.to_str().unwrap(), ksn_b.to_str().unwrap());
    let hit = |o: &Output| text(&o.stdout).contains("cache hit");
    let ok = |o: &Output| assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));

    let o = build_with_cache(a, &t.0, &cache, &[], &[]);
    ok(&o);
    assert!(!hit(&o));
    let o = build_with_cache(a, &t.0, &cache, &[], &[]);
    ok(&o);
    assert!(hit(&o), "same ksn, same profile: {}", text(&o.stdout));

    // 別の ksn 本体 → 別のキー (ヒットしない)。
    let o = build_with_cache(b, &t.0, &cache, &[], &[]);
    ok(&o);
    assert!(!hit(&o), "different ksn binary must not hit: {}", text(&o.stdout));

    // release は別のキー。
    let o = build_with_cache(a, &t.0, &cache, &["--release"], &[]);
    ok(&o);
    assert!(!hit(&o), "release must not reuse debug entry: {}", text(&o.stdout));
    let o = build_with_cache(a, &t.0, &cache, &["--release"], &[]);
    ok(&o);
    assert!(hit(&o));

    // バックエンドが違えば出力も違うので別のキー。
    // (既定のバックエンドは llvm。`c` は別のキー。)
    let o = build_with_cache(a, &t.0, &cache, &["--backend", "c"], &[]);
    ok(&o);
    assert!(!hit(&o), "c must not reuse the default (llvm) entry: {}", text(&o.stdout));

    let _ = fs::remove_dir_all(&cache);
    let _ = fs::remove_dir_all(&bin_dir);
}

#[test]
fn bundle_is_rebuilt_when_sources_change() {
    // `ksn test --both` のバンドルは、ソースが変われば作り直す。
    // 以前は target/debug に ksn があるだけで再利用され、追加したテストが "no such test" になった。
    let t = Tmp::new_raw("bundle_fresh", CACHE_SRC);
    let cache = std::env::temp_dir().join(format!("ksn_cache_bundle_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let test_both = |name: &str| {
        Command::new(exe())
            .args(["test", "--both", name])
            .current_dir(&t.0)
            .env("KSN_CACHE_DIR", &cache)
            .env("KSN_STDLIB", root().join("spec").join("stdlib"))
            .output()
            .unwrap()
    };
    let o = test_both("t.one.test_a");
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));

    let src = format!("{}\ntest t.one.test_b\n    core.check.eq(t.one(), 1u32)\nend t.one.test_b\n", CACHE_SRC);
    fs::write(t.0.join("src").join("t.ksnl"), src).unwrap();
    let o = test_both("t.one.test_b");
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert!(out.contains("test t.one.test_b ... ok"), "{}", out);
    assert!(!out.contains("no such test"), "{}", out);

    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn fmath_functions_agree_across_backends_and_match_known_values() {
    // 数学関数 (`fmath`) は、インタプリタと LLVM で同じビット列を返し、既知の値に近い。
    // `tests/data/fmath_both.ksnl`: 既知値 (f32 は 2〜3 ulp 以内)、特殊値 (atan2 の ±∞ / NaN 組を含む)、9 関数の総和 (ビット列の突き合わせ)。
    let t = Tmp::new_raw("fmath_both", include_str!("data/fmath_both.ksnl"));
    let cache = std::env::temp_dir().join(format!("ksn_cache_fmath_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let o = Command::new(exe())
        .args(["test", "--both"])
        .current_dir(&t.0)
        .env("KSN_CACHE_DIR", &cache)
        .env("KSN_STDLIB", root().join("spec").join("stdlib"))
        .output()
        .unwrap();
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    // 6 本とも LLVM (clang が無ければ LLVM) で比べていること (バンドルへ退避した「空振り」を許さない)。
    if clang_available() {
        assert_eq!(out.matches("(both: llvm)").count(), 6, "{}", out);
        assert!(out.contains("llvm 6, bundle fallback 0"), "{}", out);
    } else {
        assert_eq!(out.matches("(both: native").count(), 6, "{}", out);
    }
    assert!(!out.contains("both: bundle"), "{}", out);
    assert!(out.contains("6 passed; 0 failed"), "{}", out);
    let _ = fs::remove_dir_all(&cache);
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn ffi_struct_by_value_works_on_both_backends_with_libc() {
    let t = Tmp::new_raw("ffi_struct_libc", FFI_LIBC_STRUCT_SRC);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    let r = run(&["run"], &t.0);
    assert_eq!(text(&r.stdout), "libc struct ok\n", "{}", text(&r.stderr));
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_ffi_struct", std::process::id()));
    let b = Command::new(exe())
        .args(["build", "--backend", "llvm"])
        .current_dir(&t.0)
        .env("KSN_CACHE_DIR", &cache)
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&cache);
    let bin = t.0.join("target").join("debug").join("t.native");
    let bin = native_or_launcher(bin, &(text(&b.stdout) + &text(&b.stderr)));
    let r = Command::new(&bin).output().unwrap();
    assert_eq!(text(&r.stdout), "libc struct ok\n");
}

/// 3-2: `ksn build --release` は、ネイティブ実行ファイルが作れないとき (下ろせない・clang が無い) インタプリタ同梱の
/// バンドルへ退避せず、E9040 と理由と逃げ道を出して終了コード 1 で止まる。成果物 (`target/release`) を残さない。
/// デバッグ (`--release` なし) は理由を出して退避する (終了コード 0)。`--allow-bundle` を付けると、リリースでも退避する。
#[test]
fn release_build_does_not_fall_back_to_the_bundle_but_debug_does() {
    let src = "trait t.Speak[E: uses, P: panics]
    fn speak(self: &Self) -> u32 uses [E] panics P
end t.Speak

type t.Dog
    n: u32
end t.Dog

impl t.Speak<[], never> for t.Dog
fn t.Dog.speak(self: &t.Dog) -> u32 uses [] panics never impls t.Speak<[], never>
    self.n
end t.Dog.speak

fn t.call(x: Option<i128>) -> u32 uses [] panics never
    match x {
        Some(_) => 1u32
        None => t.Dog { n: 3u32 }.speak()
    }
end t.call

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.call(None), 3u32)
    std.io.print_line(\"x\")
end t.main
";
    let t = Tmp::new_raw("release_no_bundle", src);
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_release_no_bundle", std::process::id()));
    let build = |extra: &[&str]| {
        let mut args = vec!["build"];
        args.extend_from_slice(extra);
        Command::new(exe()).args(&args).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap()
    };
    // リリース: 失敗し、理由と逃げ道を出し、何も残さない。
    let r = build(&["--release"]);
    let err = text(&r.stderr);
    assert_eq!(r.status.code(), Some(1), "{}{}", text(&r.stdout), err);
    assert!(err.contains("E9040") && err.contains("t.main を KIR に下ろせない") && err.contains("--allow-bundle"), "{}", err);
    assert!(!err.contains("interpreter bundle is used") && !text(&r.stdout).contains("built "), "{}", text(&r.stdout));
    assert!(!t.0.join("target").join("release").exists(), "失敗したリリースは target/release を残さない");
    // デバッグ: 今のとおり理由を出して退避し、終了コード 0。
    let d = build(&[]);
    assert_eq!(d.status.code(), Some(0), "{}{}", text(&d.stdout), text(&d.stderr));
    assert!(text(&d.stdout).contains("native backend skipped: t.main を KIR に下ろせない") && text(&d.stdout).contains("the interpreter bundle is used"), "{}", text(&d.stdout));
    assert!(t.0.join("target").join("debug").join("t").exists(), "デバッグはバンドルの起動スクリプトを作る");
    // リリースでも `--allow-bundle` を付ければ退避する (理由を出し、終了コード 0)。
    let a = build(&["--release", "--allow-bundle"]);
    assert_eq!(a.status.code(), Some(0), "{}{}", text(&a.stdout), text(&a.stderr));
    assert!(text(&a.stdout).contains("native backend skipped: t.main を KIR に下ろせない") && text(&a.stdout).contains("the interpreter bundle is used"), "{}", text(&a.stdout));
    assert!(t.0.join("target").join("release").join("t").exists(), "--allow-bundle はリリースのバンドルを作る");
    let _ = fs::remove_dir_all(&cache);
}

/// 3-2: clang が無いときも、リリースは退避せず E9040 で止まる (デバッグは理由を出して退避する)。
#[test]
fn release_build_without_clang_fails_instead_of_bundling() {
    let t = Tmp::new_raw("release_no_clang", "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    std.io.print_line(\"ok\")\nend t.main\n");
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_release_no_clang", std::process::id()));
    let build = |extra: &[&str]| {
        let mut args = vec!["build"];
        args.extend_from_slice(extra);
        Command::new(exe())
            .args(&args)
            .current_dir(&t.0)
            .env("KSN_CACHE_DIR", &cache)
            .env("KSN_CLANG", "/nonexistent/ksn-test-no-clang")
            .output()
            .unwrap()
    };
    let r = build(&["--release"]);
    let err = text(&r.stderr);
    assert_eq!(r.status.code(), Some(1), "{}{}", text(&r.stdout), err);
    assert!(err.contains("E9040") && err.contains("clang を起動できない") && err.contains("--allow-bundle"), "{}", err);
    assert!(!t.0.join("target").join("release").exists(), "失敗したリリースは target/release を残さない");
    let d = build(&[]);
    assert_eq!(d.status.code(), Some(0), "{}{}", text(&d.stdout), text(&d.stderr));
    assert!(text(&d.stdout).contains("native backend skipped: clang を起動できない") && text(&d.stdout).contains("the interpreter bundle is used"), "{}", text(&d.stdout));
    let _ = fs::remove_dir_all(&cache);
}

/// 実パッケージの LLVM ビルド (設計_KIR.md 12.4): KIR に全体を下ろせるパッケージは、`ksn build --backend llvm` の
/// 実行ファイルの stdout・stderr・終了コードが `ksn run` と一致する。ここに挙げたものが後退したら落ちる
/// (下ろせるパッケージが増えたら足す)。パッケージは一時 dir に写してから作る (リポジトリの `target/` を汚さない)。
#[test]
fn llvm_builds_of_real_packages_match_run() {
    if !clang_available() {
        eprintln!("clang が無いので飛ばす");
        return;
    }
    fn copy(src: &Path, dst: &Path) {
        fs::create_dir_all(dst).unwrap();
        for e in fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            let name = e.file_name();
            if name == "target" {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                copy(&p, &dst.join(&name));
            } else {
                fs::copy(&p, dst.join(&name)).unwrap();
            }
        }
    }
    for pkg in [
        "examples/demo",
        "examples/features",
        "examples/ffi",
        "examples/http",
        "examples/json",
        "examples/kvapp",
        "examples/more_coll",
        "examples/more_str",
        "examples/native",
        "examples/plugins",
        "examples/realtime",
        "examples/shortener",
        "examples/threads",
        "examples/tour",
        "perf/closure",
        "perf/fib",
        "perf/hashmap",
        "perf/loops",
        "perf/noop",
        "perf/strcat",
        "perf/structs",
        "perf/vecops",
    ] {
        let name = Path::new(pkg).file_name().unwrap().to_string_lossy().to_string();
        // 一時 dir の下に写す。path 依存 (`../kvlib`) のあるパッケージは、依存を兄弟の dir として写す。
        let base = std::env::temp_dir().join(format!("ksn_llvm_pkg_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&base);
        let dir = base.join(&name);
        copy(&root().join(pkg), &dir);
        if pkg == "examples/kvapp" {
            copy(&root().join("examples").join("kvlib"), &base.join("kvlib"));
        }
        let cache = dir.join(".cache");
        let b = Command::new(exe())
            .args(["build", "--backend", "llvm"])
            .current_dir(&dir)
            .env("KSN_CACHE_DIR", &cache)
            .env("KSN_STDLIB", root().join("spec").join("stdlib"))
            .output()
            .unwrap();
        let out = text(&b.stdout);
        assert_eq!(b.status.code(), Some(0), "{}: {}{}", pkg, out, text(&b.stderr));
        assert!(!out.contains("backend skipped"), "{}: {}", pkg, out);
        let r = Command::new(exe()).arg("run").current_dir(&dir).env("KSN_STDLIB", root().join("spec").join("stdlib")).output().unwrap();
        let n = Command::new(dir.join("target").join("debug").join(format!("{}.native", name))).current_dir(&dir).output().unwrap();
        assert_eq!(n.status.code(), r.status.code(), "{}: 終了コード", pkg);
        assert_eq!(text(&n.stdout), text(&r.stdout), "{}: stdout", pkg);
        assert_eq!(text(&n.stderr), text(&r.stderr), "{}: stderr", pkg);
        let _ = fs::remove_dir_all(&base);
    }
}

/// `examples/kvapp` で `ksn test --both` を走らせたとき、バンドル退避にならず `(both: llvm)` になることを確かめる。
#[test]
fn kvapp_test_both_is_llvm_not_bundle() {
    if !clang_available() {
        eprintln!("skip: clang が PATH に無いので、LLVM の比較は飛ばす");
        return;
    }
    fn copy_all(src: &Path, dst: &Path) {
        fs::create_dir_all(dst).unwrap();
        for e in fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            let name = e.file_name();
            let p = e.path();
            if p.is_dir() {
                copy_all(&p, &dst.join(&name));
            } else {
                let _ = fs::copy(&p, dst.join(&name));
            }
        }
    }
    let base = std::env::temp_dir().join(format!("ksn_kvapp_both_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let dir = base.join("kvapp");
    copy_all(&root().join("examples").join("kvapp"), &dir);
    copy_all(&root().join("examples").join("kvlib"), &base.join("kvlib"));
    let cache = dir.join(".cache");
    let stdlib = std::env::var("KSN_STDLIB")
        .ok()
        .unwrap_or_else(|| root().join("spec").join("stdlib").to_string_lossy().to_string());
    let o = Command::new(exe())
        .args(["test", "--both"])
        .current_dir(&dir)
        .env("KSN_CACHE_DIR", &cache)
        .env("KSN_STDLIB", &stdlib)
        .output()
        .unwrap();
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "kvapp test --both: {}{}", out, text(&o.stderr));
    // kvapp には 2 つのテストがあり、どちらも (both: llvm) で比較され、bundle 退避にはならない。
    assert_eq!(out.matches("(both: llvm)").count(), 2, "{}", out);
    assert!(out.contains("compared with: llvm 2, bundle fallback 0"), "{}", out);
    assert!(!out.contains("both: bundle"), "{}", out);
    assert!(out.contains("2 passed; 0 failed"), "{}", out);
    let _ = fs::remove_dir_all(&base);
}

// ---- check の増分キャッシュ (設計_増分検査.md)。どの場合もキャッシュあり・なしで診断が一致すること。

/// `check --json` の (終了コード, 標準出力, 標準エラー)。`cache` が `Some` なら `KSN_CHECK_CACHE=1` と置き場所を渡し、
/// `None` ならキャッシュ無効 (`KSN_CHECK_CACHE=0`) で走らせる。
fn check_cached(dir: &Path, cache: Option<&Path>) -> (Option<i32>, String, String) {
    let o = match cache {
        Some(c) => run_with_env(&["check", "--json"], dir, &[("KSN_CHECK_CACHE", "1"), ("KSN_CACHE_DIR", c.to_str().unwrap())]),
        None => run_with_env(&["check", "--json"], dir, &[("KSN_CHECK_CACHE", "0")]),
    };
    (o.status.code(), text(&o.stdout), text(&o.stderr))
}

fn cache_tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ksn_cc_{}_{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&d);
    d
}

/// 無効・初回 (キャッシュ空)・2 回目 (命中) の 3 通りが一致すること。
fn assert_cache_same(label: &str, dir: &Path, cache: &Path) {
    let base = check_cached(dir, None);
    let cold = check_cached(dir, Some(cache));
    let warm = check_cached(dir, Some(cache));
    assert_eq!(base, cold, "{}: cold cache differs from no cache", label);
    assert_eq!(base, warm, "{}: warm cache differs from no cache", label);
}

#[test]
fn check_cache_matches_uncached_for_examples() {
    let cache = cache_tmp("examples");
    let mut n = 0;
    for e in fs::read_dir(root().join("examples")).unwrap() {
        let p = e.unwrap().path();
        if p.join("kasane.toml").exists() {
            assert_cache_same(&p.display().to_string(), &p, &cache);
            n += 1;
        }
    }
    assert!(n >= 10, "examples missing");
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn check_cache_matches_uncached_for_unsound_and_linear_ok() {
    let cache = cache_tmp("unsound");
    for sub in ["unsound", "linear_ok"] {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(sub);
        let mut names: Vec<_> = fs::read_dir(&dir).unwrap()
            .map(|e| e.unwrap().path()).filter(|p| p.extension().map_or(false, |x| x == "ksnl")).collect();
        names.sort();
        assert!(!names.is_empty());
        for (i, p) in names.into_iter().enumerate() {
            let stem = p.file_stem().unwrap().to_string_lossy().to_string();
            let t = Tmp::new_raw(&format!("cc_{}_{}", sub, stem), &fs::read_to_string(&p).unwrap());
            if i % 3 == 0 || sub == "linear_ok" {
                assert_cache_same(&format!("{}/{}", sub, stem), &t.0, &cache);
            } else {
                // 時間の節約: 3 本に 2 本は、初回 (空のキャッシュ) だけを比べる。
                assert_eq!(check_cached(&t.0, None), check_cached(&t.0, Some(&cache)), "{}/{}", sub, stem);
            }
        }
    }
    let _ = fs::remove_dir_all(&cache);
}

/// 2 ファイルのパッケージ。`a` が呼ぶ `b.base` を変えるたびに、依存する定義の診断が (キャッシュなしと同じに) 更新される。
#[test]
fn check_cache_follows_edits_to_a_definition_and_its_dependents() {
    let cache = cache_tmp("edits");
    let t = Tmp::new_raw("cc_edits", "");
    let a_path = t.0.join("src").join("t.ksnl");
    let b_path = t.0.join("src").join("t").join("b.ksnl");
    fs::create_dir_all(b_path.parent().unwrap()).unwrap();
    let main = "fn t.main() -> () uses [] panics never\n    let _x = t.user(1u32)\nend t.main\n";
    let mk_a = |user_body: &str| format!("import t.b\n\nfn t.user(x: u32) -> u32 uses [] panics never\n    {}\nend t.user\n\n{}", user_body, main);
    let mk_b = |sig: &str, body: &str| format!("fn t.b.base{} pub\n    {}\nend t.b.base\n\nfn t.b.other(x: u32) -> u32 uses [] panics never pub\n    x\nend t.b.other\n", sig, body);
    let steps: Vec<(String, String)> = vec![
        (mk_a("b.base(x)"), mk_b("(x: u32) -> u32 uses [] panics never", "x")),
        // 呼ばれる側の戻り値型を変える: 呼ぶ側の `u32` と食い違う。
        (mk_a("b.base(x)"), mk_b("(x: u32) -> bool uses [] panics never", "true")),
        // 呼ぶ側だけを直す。
        (mk_a("b.other(x)"), mk_b("(x: u32) -> bool uses [] panics never", "true")),
        // 呼ばれる側の効果を宣言と食い違わせる。
        (mk_a("b.other(x)"), mk_b("(x: u32) -> bool uses [io.console] panics never", "true")),
        // 構文の誤り、正規形でない並び、未知の名前。
        (mk_a("b.nope(x)"), mk_b("(x: u32) -> u32 uses [] panics never", "x")),
        (mk_a("b.base(x"), mk_b("(x: u32) -> u32 uses [] panics never", "x")),
        (mk_a("b.base(x)"), mk_b("(x: u32) -> u32 uses [] panics never", "x")),
    ];
    for (i, (a, b)) in steps.iter().enumerate() {
        fs::write(&a_path, a).unwrap();
        fs::write(&b_path, b).unwrap();
        assert_cache_same(&format!("step {}", i), &t.0, &cache);
        // 同じ状態でもう一度 (命中) も一致する。
        assert_eq!(check_cached(&t.0, None), check_cached(&t.0, Some(&cache)), "step {} (hit)", i);
    }
    let _ = fs::remove_dir_all(&cache);
}

/// キャッシュを壊しても (切り詰め・ゴミ・別のファイルの中身・内容だけ書き換え) 結果は変わらない。
#[test]
fn check_cache_survives_corruption() {
    let cache = cache_tmp("corrupt");
    // lint の診断 (未使用の変数) が出るので、保存した内容を書き換えると結果が変わりうる。
    let src = "fn t.main() -> () uses [] panics never\n    let unused = 1u32\nend t.main\n";
    let t = Tmp::new_raw("cc_corrupt", src);
    let base = check_cached(&t.0, None);
    assert_eq!(base.0, Some(1), "{:?}", base);
    let _ = check_cached(&t.0, Some(&cache));
    let files: Vec<PathBuf> = slot_files(&cache);
    assert!(!files.is_empty(), "nothing was cached");
    let orig = fs::read_to_string(&files[0]).unwrap();
    let mut variants: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("garbage", b"\x00\xff not json".to_vec()),
        ("truncated", orig.as_bytes()[..orig.len() / 2].to_vec()),
        ("array", b"[1,2,3]".to_vec()),
        ("empty-object", b"{}".to_vec()),
    ];
    // 内容だけ書き換える (sum が合わない)。診断を消す・文法を「一致」にする。
    let v: serde_json::Value = serde_json::from_str(&orig).unwrap();
    let mut w = v.clone();
    w["data"]["lint"] = serde_json::json!({});
    variants.push(("lint-emptied", w.to_string().into_bytes()));
    let mut w = v.clone();
    w["key"] = serde_json::json!("0000");
    variants.push(("bad-key", w.to_string().into_bytes()));
    let mut w = v.clone();
    w["data"]["gram"] = serde_json::json!("00".repeat(16));
    variants.push(("gram-forged", w.to_string().into_bytes()));
    let mut w = v.clone();
    w["data"]["clean"] = serde_json::json!("zz");
    variants.push(("clean-malformed", w.to_string().into_bytes()));
    for (name, bytes) in variants {
        fs::write(&files[0], bytes).unwrap();
        assert_eq!(base, check_cached(&t.0, Some(&cache)), "corruption `{}` changed the result", name);
        // 壊れた項目は直されて、次は命中する。
        assert_eq!(base, check_cached(&t.0, Some(&cache)), "after repair of `{}`", name);
    }
    // スロットが読めないもの (ディレクトリ) でも落ちない。
    let _ = fs::remove_file(&files[0]);
    fs::create_dir_all(&files[0]).unwrap();
    assert_eq!(base, check_cached(&t.0, Some(&cache)), "slot is a directory");
    let _ = fs::remove_dir_all(&cache);
}

/// 定義単位のキャッシュ: 1 ファイルに複数の定義があるとき、定義を 1 つ変える・並べ替える・上に行を足す (行がずれる)・
/// 別の定義の外部面を変える、のどの操作でも、キャッシュあり (操作をまたいで同じ置き場) の診断が位置まで含めて
/// キャッシュなしと一致する。
const DEF_CACHE_BASE: &str = "\
const t.LIMIT: u32 = 5u32

type t.Pair derive [Debug, Clone] pub
    a: u32
    b: u32
end t.Pair

fn t.alpha(x: u32) -> u32 uses [] panics never
    let unused = 1u32
    x
end t.alpha

fn t.beta(x: u32) -> u32 uses [] panics never
    t.alpha(x)
end t.beta

fn t.gamma(p: t.Pair) -> u32 uses [] panics never
    p.a.wrapping_add(p.b)
end t.gamma

fn t.main() -> () uses [] panics never
    let _v = t.beta(t.LIMIT)
end t.main

test t.test_alpha
    core.check.eq(t.alpha(1u32), 1u32)
end t.test_alpha
";

fn def_cache_step(label: &str, t: &Tmp, cache: &Path, text: &str) {
    fs::write(t.0.join("src").join("t.ksnl"), text).unwrap();
    assert_eq!(check_cached(&t.0, None), check_cached(&t.0, Some(cache)), "{}: cold/partial", label);
    assert_eq!(check_cached(&t.0, None), check_cached(&t.0, Some(cache)), "{}: warm", label);
}

#[test]
fn check_cache_per_definition_edits_match_uncached() {
    let cache = cache_tmp("defs");
    let t = Tmp::new_raw("cc_defs", DEF_CACHE_BASE);
    def_cache_step("base", &t, &cache, DEF_CACHE_BASE);
    let ed = |from: &str, to: &str| {
        assert!(DEF_CACHE_BASE.contains(from), "{}", from);
        DEF_CACHE_BASE.replacen(from, to, 1)
    };
    // 定義を 1 つ変える (診断が増える・消える・別の種類になる)。
    def_cache_step("body edit", &t, &cache, &ed("    t.alpha(x)\n", "    let y = t.alpha(x)\n    t.alpha(y)\n"));
    def_cache_step("fix unused", &t, &cache, &ed("    let unused = 1u32\n", ""));
    def_cache_step("name case", &t, &cache, &ed("let unused = 1u32", "let Unused = 1u32"));
    def_cache_step("type error", &t, &cache, &ed("    x\nend t.alpha", "    true\nend t.alpha"));
    // 上に行を足す (下の定義が全部ずれる)。コメント・空行・import 風の行。
    def_cache_step("shift by comment", &t, &cache, &format!("// top\n{}", DEF_CACHE_BASE));
    def_cache_step("shift by blank", &t, &cache, &format!("\n{}", DEF_CACHE_BASE));
    def_cache_step("shift by a const", &t, &cache, &format!("const t.ZERO: u32 = 0u32\n\n{}", DEF_CACHE_BASE));
    def_cache_step("shift by 3 blank lines in a body", &t, &cache, &ed("    t.alpha(x)\n", "\n\n\n    t.alpha(x)\n"));
    // 並べ替え (定義順の違反は非致命で報告)。
    let a = "fn t.alpha(x: u32)";
    let g = "fn t.gamma(p: t.Pair)";
    let ia = DEF_CACHE_BASE.find(a).unwrap();
    let ib = DEF_CACHE_BASE.find("fn t.beta").unwrap();
    let ig = DEF_CACHE_BASE.find(g).unwrap();
    let im = DEF_CACHE_BASE.find("fn t.main").unwrap();
    let (alpha, beta, gamma) = (&DEF_CACHE_BASE[ia..ib], &DEF_CACHE_BASE[ib..ig], &DEF_CACHE_BASE[ig..im]);
    let head = &DEF_CACHE_BASE[..ia];
    let tail = &DEF_CACHE_BASE[im..];
    def_cache_step("swap alpha/beta", &t, &cache, &format!("{}{}{}{}{}", head, beta, alpha, gamma, tail));
    def_cache_step("reverse", &t, &cache, &format!("{}{}{}{}{}", head, gamma, beta, alpha, tail));
    def_cache_step("back", &t, &cache, DEF_CACHE_BASE);
    // 別の定義の外部面を変える: 呼ばれる側の戻り値型・引数・効果・pub・名前・型のフィールド。
    def_cache_step("callee returns bool", &t, &cache, &ed("fn t.alpha(x: u32) -> u32", "fn t.alpha(x: u32) -> bool"));
    def_cache_step("callee arity", &t, &cache, &ed("fn t.alpha(x: u32)", "fn t.alpha(x: u32, y: u32)"));
    def_cache_step("callee uses io", &t, &cache, &ed("fn t.alpha(x: u32) -> u32 uses []", "fn t.alpha(x: u32) -> u32 uses [io.console]"));
    def_cache_step("callee renamed", &t, &cache, &ed("fn t.alpha(x: u32)", "fn t.delta(x: u32)"));
    def_cache_step("field renamed", &t, &cache, &ed("    a: u32\n    b: u32\nend", "    a: u32\n    c: u32\nend"));
    def_cache_step("field type", &t, &cache, &ed("    b: u32\nend", "    b: bool\nend"));
    def_cache_step("const type", &t, &cache, &ed("const t.LIMIT: u32 = 5u32", "const t.LIMIT: bool = true"));
    // 文面の誤り (正規形・文法・構文)。
    def_cache_step("double space", &t, &cache, &ed("fn t.beta(x: u32) ->", "fn t.beta(x: u32)  ->"));
    def_cache_step("tab", &t, &cache, &ed("    t.alpha(x)\n", "\tt.alpha(x)\n"));
    def_cache_step("missing end", &t, &cache, &ed("end t.beta\n", ""));
    def_cache_step("end name", &t, &cache, &ed("end t.beta", "end t.betaa"));
    def_cache_step("comment then blank", &t, &cache, &ed("fn t.beta", "// c\n\nfn t.beta"));
    def_cache_step("trailing garbage", &t, &cache, &format!("{}garbage\n", DEF_CACHE_BASE));
    def_cache_step("final", &t, &cache, DEF_CACHE_BASE);
    let _ = fs::remove_dir_all(&cache);
}

/// 行単位の変異 (削除・複製・入れ替え・空行・コメント・字下げ・トークンの置換) を決まった順序で多数かけ、
/// そのたびにキャッシュあり (同じ置き場を使い続ける) とキャッシュなしの診断が一致すること。
#[test]
fn check_cache_line_mutations_match_uncached() {
    let cache = cache_tmp("mutations");
    let t = Tmp::new_raw("cc_mut", DEF_CACHE_BASE);
    let lines: Vec<&str> = DEF_CACHE_BASE.lines().collect();
    let mut seed: u64 = 0x2545F4914F6CDD1D;
    let mut rnd = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    let swaps = [("u32", "bool"), ("x", "y"), ("t.alpha", "t.beta"), ("uses []", "uses [io.console]"), (" pub", ""), ("let ", "let mut "), ("(", "(("), (")", "")];
    for i in 0..48 {
        let mut v: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        // 1 つの入力に 1〜2 個の変異を重ねる。
        for _ in 0..1 + rnd(2) {
            let k = rnd(v.len());
            match rnd(7) {
                0 => {
                    v.remove(k);
                }
                1 => v.insert(k, v[k].clone()),
                2 => {
                    let j = rnd(v.len());
                    v.swap(k, j);
                }
                3 => v.insert(k, String::new()),
                4 => v.insert(k, "// c".to_string()),
                5 => v[k] = format!("    {}", v[k]),
                _ => {
                    let (a, b) = swaps[rnd(swaps.len())];
                    v[k] = v[k].replacen(a, b, 1);
                }
            }
            if v.is_empty() {
                v.push(String::new());
            }
        }
        let text = format!("{}\n", v.join("\n"));
        def_cache_step(&format!("mutation {}", i), &t, &cache, &text);
        // ときどき元に戻す (定義ごとの結果が、別の版を挟んでも正しく使われる)。
        if i % 6 == 5 {
            def_cache_step(&format!("restore {}", i), &t, &cache, DEF_CACHE_BASE);
        }
    }
    let _ = fs::remove_dir_all(&cache);
}

/// `check --json` を追加の環境変数つきで走らせる。
fn check_cached_env(dir: &Path, env: &[(&str, &str)]) -> (Option<i32>, String, String) {
    let o = run_with_env(&["check", "--json"], dir, env);
    (o.status.code(), text(&o.stdout), text(&o.stderr))
}

/// 文面だけの検査 (未使用の変数の lint) が出るファイルを `n` 本持つパッケージ (`t.ksnl` と `t/f<i>.ksnl`)。
fn many_files_package(name: &str, n: usize) -> Tmp {
    let t = Tmp::new_raw(name, "fn t.main() -> () uses [] panics never\n    let unused = 1u32\nend t.main\n");
    let d = t.0.join("src").join("t");
    fs::create_dir_all(&d).unwrap();
    for i in 0..n {
        let src = format!("fn t.f{}.g() -> u32 uses [] panics never\n    let unused = {}u32\n    {}u32\nend t.f{}.g\n", i, i, i, i);
        fs::write(d.join(format!("f{}.ksnl", i)), src).unwrap();
    }
    t
}

/// 既定で有効 (`KSN_CHECK_CACHE` を書かなくても置き場に保存する)。`KSN_CHECK_CACHE=0` なら何も書かない。
#[test]
fn check_cache_is_on_by_default_and_can_be_turned_off() {
    let t = many_files_package("cc_default", 3);
    let base = check_cached(&t.0, None);
    let on = cache_tmp("default_on");
    assert_eq!(base, check_cached_env(&t.0, &[("KSN_CACHE_DIR", on.to_str().unwrap())]), "default run differs");
    let n = fs::read_dir(on.join("check")).map(|r| r.count()).unwrap_or(0);
    assert!(n >= 4, "default is on, but only {} entries were stored", n);
    let off = cache_tmp("default_off");
    assert_eq!(base, check_cached_env(&t.0, &[("KSN_CHECK_CACHE", "0"), ("KSN_CACHE_DIR", off.to_str().unwrap())]));
    assert!(!off.join("check").exists(), "KSN_CHECK_CACHE=0 still wrote to the cache");
    let _ = fs::remove_dir_all(&on);
    let _ = fs::remove_dir_all(&off);
}

/// 標準ライブラリの構文解析結果 (`<置き場>/std/*.bin`) は 1 本だけ作られ、命中しても結果は同じ。
/// 壊れていても (切れた・別の中身) 黙って作り直して同じ結果になる。`KSN_CHECK_CACHE=0` なら作らない。
#[test]
fn std_parse_cache_is_written_once_hit_and_survives_corruption() {
    let t = many_files_package("std_parse_cache", 2);
    let base = check_cached(&t.0, None);
    let c = cache_tmp("std_parse");
    let env = [("KSN_CACHE_DIR", c.to_str().unwrap())];
    let bins = |c: &Path| -> Vec<PathBuf> {
        fs::read_dir(c.join("std")).map(|r| r.map(|e| e.unwrap().path()).filter(|p| p.extension().map_or(false, |x| x == "bin")).collect()).unwrap_or_default()
    };
    assert_eq!(base, check_cached_env(&t.0, &env), "cold run differs");
    let b = bins(&c);
    assert_eq!(b.len(), 1, "expected one std cache file, got {:?}", b);
    let raw = fs::read(&b[0]).unwrap();
    assert_eq!(base, check_cached_env(&t.0, &env), "warm run differs");
    assert_eq!(raw, fs::read(&b[0]).unwrap(), "a hit rewrote the file");
    fs::write(&b[0], &raw[..raw.len() / 2]).unwrap();
    assert_eq!(base, check_cached_env(&t.0, &env), "truncated cache changed the result");
    assert_eq!(raw, fs::read(&b[0]).unwrap(), "the truncated file was not rebuilt");
    let mut bad = raw.clone();
    bad[10] ^= 0xff; // 鍵が合わない
    fs::write(&b[0], &bad).unwrap();
    assert_eq!(base, check_cached_env(&t.0, &env), "wrong key changed the result");
    assert_eq!(raw, fs::read(&b[0]).unwrap());
    let off = cache_tmp("std_parse_off");
    assert_eq!(base, check_cached_env(&t.0, &[("KSN_CHECK_CACHE", "0"), ("KSN_CACHE_DIR", off.to_str().unwrap())]));
    assert!(!off.join("std").exists(), "KSN_CHECK_CACHE=0 still wrote the std cache");
    let _ = fs::remove_dir_all(&c);
    let _ = fs::remove_dir_all(&off);
}

/// スロット (`.` で始まる記録を除く) のファイル。
fn slot_files(cache: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(cache.join("check"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| !p.file_name().unwrap().to_string_lossy().starts_with('.'))
        .collect();
    v.sort();
    v
}

/// 置き場の使用量。ディスクのブロック (4 KiB) に切り上げた合計で、記録 (`.stat.json`) は除く。
fn disk_usage(cache: &Path) -> u64 {
    slot_files(cache).iter().map(|p| fs::metadata(p).unwrap().len().div_ceil(4096) * 4096).sum()
}

/// 上限 (`KSN_CHECK_CACHE_MAX`) を小さくすると古いものが消え、置き場の使用量 (ブロック単位) は上限以下になる。結果は変わらない。
#[test]
fn check_cache_limit_evicts_old_entries_and_keeps_results() {
    let t = many_files_package("cc_limit", 8);
    let base = check_cached(&t.0, None);
    let cache = cache_tmp("limit");
    let c = cache.to_str().unwrap();
    let cdir = cache.join("check");
    // 1000 と 0 は 1 本も残らない。3 ブロック (12 KiB) は 2 本ほど残す。
    for max in [1000u64, 0, 3 * 4096] {
        let m = max.to_string();
        let env = [("KSN_CACHE_DIR", c), ("KSN_CHECK_CACHE_MAX", m.as_str())];
        for round in 0..2 {
            assert_eq!(base, check_cached_env(&t.0, &env), "max {} round {}", max, round);
            assert!(disk_usage(&cache) <= max, "max {}: {} bytes (blocks) are kept", max, disk_usage(&cache));
        }
    }
    // 上限が十分なら全部残る (t.ksnl と f0..f7 の 9 本)。
    let _ = fs::remove_dir_all(&cdir);
    assert_eq!(base, check_cached_env(&t.0, &[("KSN_CACHE_DIR", c), ("KSN_CHECK_CACHE_MAX", "100000000")]));
    assert_eq!(slot_files(&cache).len(), 9);
    let _ = fs::remove_dir_all(&cache);
}

/// 件数の上限 (`KSN_CHECK_CACHE_ENTRIES`) を超えたら、件数が上限以下になる。結果は変わらない。
#[test]
fn check_cache_entry_cap_is_enforced() {
    let t = many_files_package("cc_entries", 8);
    let base = check_cached(&t.0, None);
    let cache = cache_tmp("entries");
    let c = cache.to_str().unwrap();
    let env = [("KSN_CACHE_DIR", c), ("KSN_CHECK_CACHE_ENTRIES", "4")];
    assert_eq!(base, check_cached_env(&t.0, &env));
    assert!(slot_files(&cache).len() <= 4, "{} entries are kept", slot_files(&cache).len());
    let _ = fs::remove_dir_all(&cache);
}

/// 掃除の記録: 上限を超えない限り、全体を数え直すのは 1 時間に 1 回。2 回目以降の実行は記録の時刻を変えない。
/// 記録が壊れていたら作り直す。
#[test]
fn check_cache_record_avoids_recounting_on_every_run() {
    let t = many_files_package("cc_record", 3);
    let cache = cache_tmp("record");
    let c = cache.to_str().unwrap();
    let env = [("KSN_CACHE_DIR", c)];
    let scanned = || -> u64 {
        let raw = fs::read_to_string(cache.join("check").join(".stat.json")).unwrap();
        serde_json::from_str::<serde_json::Value>(&raw).unwrap()["scanned"].as_u64().unwrap()
    };
    assert_eq!(check_cached(&t.0, None), check_cached_env(&t.0, &env));
    let first = scanned();
    assert_eq!(check_cached(&t.0, None), check_cached_env(&t.0, &env));
    assert_eq!(first, scanned(), "a run within the hour recounted the cache");
    // 記録が壊れたら、次の実行で作り直す。診断は変わらない。
    fs::write(cache.join("check").join(".stat.json"), "garbage").unwrap();
    assert_eq!(check_cached(&t.0, None), check_cached_env(&t.0, &env));
    assert!(serde_json::from_str::<serde_json::Value>(&fs::read_to_string(cache.join("check").join(".stat.json")).unwrap()).is_ok());
    let _ = fs::remove_dir_all(&cache);
}

/// 同じ置き場を 8 本の `ksn check` が同時に使っても、どれの診断もキャッシュなしと一致する。
/// 空から同時に書く回・上限で同時に削る回・命中しながら同時に読む回を試す。
#[test]
fn check_cache_parallel_processes_agree() {
    let t = many_files_package("cc_parallel", 12);
    let base = check_cached(&t.0, None);
    let cache = cache_tmp("parallel");
    let c = cache.to_str().unwrap().to_string();
    for (label, max) in [("cold", "100000000"), ("tight", "500"), ("warm", "100000000"), ("tight-warm", "500")] {
        let outs: Vec<(Option<i32>, String, String)> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..8)
                .map(|_| {
                    let (dir, c) = (t.0.clone(), c.clone());
                    s.spawn(move || check_cached_env(&dir, &[("KSN_CACHE_DIR", c.as_str()), ("KSN_CHECK_CACHE_MAX", max)]))
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (i, o) in outs.iter().enumerate() {
            assert_eq!(&base, o, "{}: process {} differs from no cache", label, i);
        }
    }
    let _ = fs::remove_dir_all(&cache);
}

/// `derive [TryClone]` (トレイト `alloc.alloc.TryClone`): derive した型を `Vec` に入れて `try_clone` し、
/// `T: alloc.alloc.TryClone` のジェネリック関数経由でも複製できる。`run` と `build` (LLVM → 下ろせなければバンドル) の出力が一致する。
#[test]
fn derive_try_clone_runs_and_builds_alike() {
    let src = "type t.P derive [Debug, Clone, Copy, TryClone] pub\n    x: u32\n    y: u32\nend t.P\n\nenum t.E derive [TryClone] pub\n    A(u32)\n    B(t.P)\nend t.E\n\ntype t.W derive [TryClone] pub\n    p: t.P\n    n: u32\nend t.W\n\nfn t.dup[T: alloc.alloc.TryClone](v: &T) -> Result<T, alloc.alloc.AllocError> uses [alloc] panics never\n    v.try_clone()\nend t.dup\n\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    let w = t.W { p: t.P { x: 1u32, y: 2u32 }, n: 3u32 }\n    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    core.mem.drop(match v.push(w) {\n        Ok(_u) => 0u8\n        Err(_e) => 1u8\n    })\n    core.mem.drop(match t.dup(&v) {\n        Ok(c) => {\n            core.check.eq(c.len(), 1usize)\n            std.io.print_line(\"ok\")\n        }\n        Err(_e) => std.io.print_line(\"err\")\n    })\n    core.mem.drop(match v.try_clone() {\n        Ok(c) => core.check.eq(c.len(), 1usize)\n        Err(_e) => core.check.eq(1u8, 2u8)\n    })\n    core.mem.drop(match t.dup(&t.E.B(t.P { x: 1u32, y: 2u32 })) {\n        Ok(_c) => std.io.print_line(\"e ok\")\n        Err(_e) => std.io.print_line(\"err\")\n    })\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("derive_try_clone_ok", src);
    let chk = run(&["check"], &t.0);
    assert_eq!(chk.status.code(), Some(0), "{}{}", text(&chk.stdout), text(&chk.stderr));
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    assert_eq!(text(&interp.stdout), "ok\ne ok\n");
    let cache = std::env::temp_dir().join(format!("ksn_cache_tryclone_{}", std::process::id()));
    let mut c = Command::new(exe());
    c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
    let o = c.output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    let p = t.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t.native" });
    let p = native_or_launcher(p, &out);
    let r = Command::new(&p).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), text(&interp.stdout), "build and run outputs differ");
    let _ = fs::remove_dir_all(&cache);
}


// ---------------------------------------------------------------- LSP の増分検査 (修正プラン 6-5)

/// `ksn lsp` を起動して `didChange` で編集を流し、診断が `ksn check --json` (全体検査) と一致することを確かめる。
/// 変わったモジュールとその依存元だけを検査し直す経路 (`cli::analyze_inc`) が使われたことは、
/// `KSN_TIMING=1` の stderr の `incremental: redo` で確かめる。
struct LspDrive {
    child: std::process::Child,
    rx: std::sync::mpsc::Receiver<serde_json::Value>,
    root: PathBuf,
    next_id: i64,
    diags: std::collections::BTreeMap<String, Vec<String>>,
}

impl LspDrive {
    fn start(root: &Path, stderr: &Path) -> LspDrive {
        use std::io::{BufRead, Read};
        let mut child = Command::new(exe())
            .arg("lsp")
            .env("KSN_TIMING", "1")
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(fs::File::create(stderr).unwrap())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut r = std::io::BufReader::new(out);
            loop {
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 {
                        return;
                    }
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    if let Some(v) = h.strip_prefix("Content-Length: ") {
                        len = v.parse().unwrap();
                    }
                }
                let mut buf = vec![0u8; len];
                if r.read_exact(&mut buf).is_err() {
                    return;
                }
                let _ = tx.send(serde_json::from_slice(&buf).unwrap());
            }
        });
        let mut d = LspDrive { child, rx, root: root.to_path_buf(), next_id: 1, diags: Default::default() };
        d.call("initialize", serde_json::json!({"rootUri": format!("file://{}", root.display()), "capabilities": {}}));
        d.notify("initialized", serde_json::json!({}));
        d
    }
    fn send(&mut self, v: serde_json::Value) {
        use std::io::Write;
        let b = v.to_string();
        let si = self.child.stdin.as_mut().unwrap();
        write!(si, "Content-Length: {}\r\n\r\n{}", b.len(), b).unwrap();
        si.flush().unwrap();
    }
    fn notify(&mut self, m: &str, p: serde_json::Value) {
        self.send(serde_json::json!({"jsonrpc": "2.0", "method": m, "params": p}));
    }
    /// 応答までに届いた publishDiagnostics を `diags` (URI → 診断コードの並び) に反映する。
    fn call(&mut self, m: &str, p: serde_json::Value) {
        let id = self.next_id;
        self.next_id += 1;
        self.send(serde_json::json!({"jsonrpc": "2.0", "id": id, "method": m, "params": p}));
        loop {
            let v = self.rx.recv_timeout(std::time::Duration::from_secs(60)).expect("lsp timed out");
            if v.get("id") == Some(&serde_json::json!(id)) {
                return;
            }
            if v["method"] == "textDocument/publishDiagnostics" {
                let mut cs: Vec<String> = v["params"]["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap().to_string()).collect();
                cs.sort();
                self.diags.insert(v["params"]["uri"].as_str().unwrap().to_string(), cs);
            }
        }
    }
    fn uri(&self, rel: &str) -> String {
        format!("file://{}", self.root.join(rel).display())
    }
    fn open(&mut self, rel: &str) {
        let text = fs::read_to_string(self.root.join(rel)).unwrap();
        let uri = self.uri(rel);
        self.notify("textDocument/didOpen", serde_json::json!({"textDocument": {"uri": uri, "languageId": "ksnl", "version": 1, "text": text}}));
    }
    /// ファイルを書き換え (全体検査はディスクを読む)、同じ内容を `didChange` で送り、診断が出そろうまで待つ。
    fn edit(&mut self, rel: &str, text: &str, version: i64) {
        fs::write(self.root.join(rel), text).unwrap();
        let uri = self.uri(rel);
        self.notify("textDocument/didChange", serde_json::json!({"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": text}]}));
        self.sync(&uri);
    }
    /// `hover` は検査が古ければ先に走らせてから答える。応答が返れば、その検査の診断は届いている。
    fn sync(&mut self, uri: &str) {
        self.call("textDocument/hover", serde_json::json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 0}}));
    }
    /// 全体検査 (`check --json`) のファイルごとの診断コード。
    fn full(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        let o = run(&["check", "--json"], &self.root);
        let mut m: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for l in text(&o.stdout).lines().filter(|l| l.starts_with('{')) {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            let f = v["file"].as_str().unwrap_or("kasane.toml");
            m.entry(self.uri(f)).or_default().push(v["code"].as_str().unwrap().to_string());
        }
        m.values_mut().for_each(|v| v.sort());
        m
    }
    fn assert_same(&self, what: &str) {
        let mut got = self.diags.clone();
        got.retain(|_, v| !v.is_empty());
        assert_eq!(got, self.full(), "LSP と check --json の診断が違う: {}", what);
    }
}

impl Drop for LspDrive {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn lsp_incremental_matches_full_check() {
    let t = Tmp::new_raw("lsp_inc", "");
    let src = t.0.join("src");
    fs::create_dir_all(src.join("t")).unwrap();
    let mk_main = "import t.b\n\nfn t.main() -> u32 uses [] panics never\n    b.base(1u32)\nend t.main\n";
    let mk_b = |sig: &str| format!("fn t.b.base{} uses [] panics never pub\n    x\nend t.b.base\n", sig);
    let mk_leaf = |name: &str, body: &str| format!("fn t.{n}.f(x: u32) -> u32 uses [] panics never pub\n    {b}\nend t.{n}.f\n", n = name, b = body);
    fs::write(src.join("t.ksnl"), mk_main).unwrap();
    fs::write(src.join("t/b.ksnl"), mk_b("(x: u32) -> u32")).unwrap();
    for n in ["c", "d", "e"] {
        fs::write(src.join(format!("t/{}.ksnl", n)), mk_leaf(n, "x")).unwrap();
    }
    let err = std::env::temp_dir().join(format!("ksn_lsp_inc_err_{}", std::process::id()));
    let mut d = LspDrive::start(&t.0, &err);
    for f in ["src/t.ksnl", "src/t/b.ksnl", "src/t/c.ksnl", "src/t/d.ksnl", "src/t/e.ksnl"] {
        d.open(f);
    }
    let u = d.uri("src/t/b.ksnl");
    d.sync(&u);
    d.assert_same("初回");
    let mut v = 1;
    let mut step = |d: &mut LspDrive, what: &str, rel: &str, text: &str| {
        v += 1;
        d.edit(rel, text, v);
        d.assert_same(what);
    };
    // 依存先のシグネチャを変える: 依存元 (t.ksnl) に診断が出る。
    step(&mut d, "引数を足す", "src/t/b.ksnl", &mk_b("(x: u32, y: u32) -> u32"));
    assert!(!d.diags[&d.uri("src/t.ksnl")].is_empty(), "依存元に診断が出ていない: {:?}", d.diags);
    // 戻す: 診断が消える。
    step(&mut d, "引数を戻す", "src/t/b.ksnl", &mk_b("(x: u32) -> u32"));
    assert!(d.diags[&d.uri("src/t.ksnl")].is_empty(), "依存元の診断が消えていない: {:?}", d.diags);
    // 戻り値型を変える。
    step(&mut d, "戻り値を変える", "src/t/b.ksnl", &mk_b("(x: u32) -> bool"));
    step(&mut d, "戻り値を戻す", "src/t/b.ksnl", &mk_b("(x: u32) -> u32"));
    // 依存元の無い葉の編集で、その葉だけに診断が出る・消える。
    step(&mut d, "葉に型の誤り", "src/t/c.ksnl", &mk_leaf("c", "true"));
    assert!(!d.diags[&d.uri("src/t/c.ksnl")].is_empty());
    step(&mut d, "葉を直す", "src/t/c.ksnl", &mk_leaf("c", "x"));
    // 構文の誤り (全体検査に戻る) と回復。
    step(&mut d, "構文の誤り", "src/t/d.ksnl", &mk_leaf("d", "x +"));
    step(&mut d, "構文を直す", "src/t/d.ksnl", &mk_leaf("d", "x"));
    step(&mut d, "もう一度葉を編集", "src/t/e.ksnl", &mk_leaf("e", "x.wrapping_add(1u32)"));
    drop(d);
    let log = fs::read_to_string(&err).unwrap_or_default();
    let _ = fs::remove_file(&err);
    assert!(log.contains("incremental: redo"), "増分検査の経路が使われていない:\n{}", log);
}

#[test]
fn derive_json_round_trips_and_builds_alike() {
    let t = Tmp::new_raw("derive_json_round", DERIVE_JSON_SRC);
    let chk = run(&["check"], &t.0);
    assert_eq!(chk.status.code(), Some(0), "{}{}", text(&chk.stdout), text(&chk.stderr));
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    let want = "{\"items\":[{\"count\":3,\"id\":-5,\"label\":\"a\\\"b\\n\",\"ok\":true}],\"name\":\"doc\",\"note\":\"n\",\"shape\":{\"Rect\":[2,3]},\"tags\":[1,null]}\n";
    assert_eq!(text(&interp.stdout), want);
    let tb = run(&["test", "--both"], &t.0);
    assert_eq!(tb.status.code(), Some(0), "{}{}", text(&tb.stdout), text(&tb.stderr));
    assert!(text(&tb.stdout).contains("2 passed; 0 failed"), "{}", text(&tb.stdout));
    let cache = std::env::temp_dir().join(format!("ksn_cache_derive_json_{}", std::process::id()));
    let mut c = Command::new(exe());
    c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
    let o = c.output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    let p = t.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t.native" });
    let p = native_or_launcher(p, &out);
    let r = Command::new(&p).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), text(&interp.stdout), "build and run outputs differ");
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn build_key_changes_with_ksn_clang_args() {
    // `KSN_CLANG_ARGS` を変えるとキャッシュキーが変わり、前の成果物が `cache hit` で出ない。
    let t = Tmp::new_raw("cache_key_clang_args", CACHE_SRC);
    let cache = std::env::temp_dir().join(format!("ksn_cache_key_clang_args_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let hit = |o: &Output| text(&o.stdout).contains("cache hit");
    let ok = |o: &Output| assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));

    let o = build_with_cache(exe(), &t.0, &cache, &[], &[]);
    ok(&o);
    assert!(!hit(&o));
    let o = build_with_cache(exe(), &t.0, &cache, &[], &[]);
    ok(&o);
    assert!(hit(&o), "同じ引数ならキャッシュヒット: {}", text(&o.stdout));

    // KSN_CLANG_ARGS を変えると別のキー (ヒットしない)。
    let o = build_with_cache(exe(), &t.0, &cache, &[], &[("KSN_CLANG_ARGS", "-O2")]);
    ok(&o);
    assert!(!hit(&o), "KSN_CLANG_ARGS の変更は鍵を変える: {}", text(&o.stdout));

    // 同じ KSN_CLANG_ARGS で再度 → ヒット。
    let o = build_with_cache(exe(), &t.0, &cache, &[], &[("KSN_CLANG_ARGS", "-O2")]);
    ok(&o);
    assert!(hit(&o), "同じ KSN_CLANG_ARGS ならキャッシュヒット: {}", text(&o.stdout));

    let _ = fs::remove_dir_all(&cache);
}
