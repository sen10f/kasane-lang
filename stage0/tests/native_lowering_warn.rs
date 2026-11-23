//! ネイティブ (KIR) に下ろせない形の警告 (`ksn check` の W9040) と、`ksn build --release` の E9040 の一覧の中身。

#![allow(dead_code, unused_imports)]

mod common;

use common::{run, run_with_env, text, Tmp};

/// `ksn fmt` の出力と同じ形の入力 (E0020 にならないように、そのまま書く)。`std.thread.sleep` は KIR に下ろせない。
const SLEEP: &str = "fn t.main() -> Result<(), std.io.Error> uses [thread, time] panics never
    t.nap()
    Ok(())
end t.main

fn t.nap() -> () uses [thread, time] panics never
    std.thread.sleep(1u64)
end t.nap
";

/// 下ろせる形だけの入力 (整数の演算と戻り値)。警告は出ない。
const LOWERABLE: &str = "fn t.add(a: i32, b: i32) -> i32 uses [] panics never
    a.wrapping_add(b)
end t.add

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let _x = t.add(1i32, 2i32)
    Ok(())
end t.main
";

#[test]
fn unlowerable_use_is_warned_in_check_as_non_fatal() {
    let t = Tmp::new_raw("native_warn_check", SLEEP);
    let o = run(&["check", "--json", "--native"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "警告は致命でない: {}", out);
    assert!(out.contains("\"code\": \"W9040\"") && out.contains("\"fatal\": false"), "{}", out);
    assert!(out.contains("std.thread.sleep") && out.contains("t.nap"), "理由と関数が出る: {}", out);
    // 使う所 (`std.thread.sleep` の行 = 7 行目) を指す。
    assert!(out.contains("\"line\": 7"), "{}", out);
}

#[test]
fn warning_is_printed_in_human_format_with_non_fatal_marker() {
    let t = Tmp::new_raw("native_warn_human", SLEEP);
    let o = run(&["check", "--native"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert!(out.contains("W9040 (non-fatal):"), "{}", out);
}

#[test]
fn lowerable_program_gets_no_warning() {
    let t = Tmp::new_raw("native_warn_ok", LOWERABLE);
    let o = run(&["check", "--json", "--native"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert!(!out.contains("W9040"), "下ろせる形に警告が出た: {}", out);
}

#[test]
fn plain_check_gives_no_w9040() {
    // 通常の `ksn check` は W9040 を出さない (`--native` のときだけ)。下ろせない形でも、出力は変わらない。
    for (name, src) in [("native_warn_plain_sleep", SLEEP), ("native_warn_plain_ok", LOWERABLE)] {
        let t = Tmp::new_raw(name, src);
        let o = run(&["check", "--json"], &t.0);
        let out = text(&o.stdout);
        assert_eq!(o.status.code(), Some(0), "{}", out);
        assert!(!out.contains("W9040"), "通常の check に W9040 が出た: {}", out);
    }
}

#[test]
fn release_build_list_has_no_rt_panic_noise() {
    let t = Tmp::new_raw("native_warn_release", SLEEP);
    let cache = std::env::temp_dir().join(format!("w1c_native_warn_cache_{}", std::process::id()));
    let o = run_with_env(&["build", "--release"], &t.0, &[("KSN_CACHE_DIR", cache.to_str().unwrap())]);
    let err = text(&o.stderr) + &text(&o.stdout);
    let _ = std::fs::remove_dir_all(&cache);
    assert_ne!(o.status.code(), Some(0), "{}", err);
    assert!(err.contains("E9040") && err.contains("std.thread.sleep"), "{}", err);
    assert!(!err.contains("rt_panic"), "E9040 の一覧に無関係な rt_panic が出た: {}", err);
}
