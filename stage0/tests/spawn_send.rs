//! 5.5: `std.thread.spawn` に取り込む値の `core.marker.Send` 検査。
//! 生ポインタ (`*const T` / `*mut T`) は `Send` でない。`[unsafe] modules` で生ポインタを許したモジュールの中でも、
//! スレッドへ取り込めば E2040 になる (許可は生ポインタを書く権利であって、`Send` の表明ではない)。

mod common;

use common::*;
use std::fs;

/// `[unsafe] modules = ["t"]` を持つ一時パッケージ (`t` の中では生ポインタを書ける) で `ksn check --json` を走らせる。
/// 戻り値は (終了コード, 診断コードの一覧, 出力全体)。
fn check_with_raw_ptrs_allowed(name: &str, src: &str) -> (Option<i32>, Vec<String>, String) {
    let d = std::env::temp_dir().join(format!("ksn_spawn_send_{}_{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("src")).unwrap();
    fs::write(d.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
    fs::write(d.join("src").join("t.ksnl"), src).unwrap();
    // 書き方は 1 通り (E0020): 試験の入力は `ksn fmt` の出力にそろえる。
    let _ = std::process::Command::new(env!("CARGO_BIN_EXE_ksn")).arg("fmt").current_dir(&d).output();
    let t = Tmp(d);
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    (o.status.code(), codes(&out), out)
}

/// `spawn` の本体の形 (戻り値を join して捨てる) を共通にする。`capture` の節と本体だけを変える。
fn spawn_program(prelude: &str, capture: &str, body: &str) -> String {
    format!(
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, thread] panics may\n\
         {prelude}    let spawned = std.thread.spawn(\n\
         \x20       std.alloc.SYSTEM,\n\
         \x20       fn() -> u32 capture [{capture}] {{\n\
         {body}\
         \x20       }},\n\
         \x20   )\n\
         \x20   match spawned {{\n\
         \x20       Ok(handle) => {{\n\
         \x20           let _h = handle.join()\n\
         \x20       }}\n\
         \x20       Err(_e) => core.panic.panic(\"spawn failed\")\n\
         \x20   }}\n\
         \x20   Ok(())\n\
         end t.main\n"
    )
}

#[test]
fn raw_mut_pointer_capture_is_not_send() {
    let src = spawn_program(
        "    let mut x = 10u32\n    let ptr = unsafe \"ptr\" \"the pointer is only read by the thread\" {\n        core.ptr.from_mut(&mut x)\n    }\n",
        "move ptr",
        "            unsafe \"ptr\" \"the thread reads through the pointer\" {\n                core.ptr.read(ptr)\n            }\n",
    );
    let (rc, codes, out) = check_with_raw_ptrs_allowed("raw_mut", &src);
    assert_eq!(rc, Some(1), "expected rejection:\n{}", out);
    assert!(codes.iter().any(|c| c == "E2040"), "expected E2040 for a *mut capture:\n{}", out);
}

#[test]
fn raw_const_pointer_capture_is_not_send() {
    let src = spawn_program(
        "    let x = 10u32\n    let ptr = unsafe \"ptr\" \"the pointer is only read by the thread\" {\n        core.ptr.from_ref(&x)\n    }\n",
        "move ptr",
        "            unsafe \"ptr\" \"the thread reads through the pointer\" {\n                core.ptr.read(ptr)\n            }\n",
    );
    let (rc, codes, out) = check_with_raw_ptrs_allowed("raw_const", &src);
    assert_eq!(rc, Some(1), "expected rejection:\n{}", out);
    assert!(codes.iter().any(|c| c == "E2040"), "expected E2040 for a *const capture:\n{}", out);
}

#[test]
fn struct_holding_raw_pointer_capture_is_not_send() {
    // 生ポインタを持つ構造体は、フィールドから自動導出で `Send` でなくなる (5.5)。
    let src = format!(
        "type t.Ptr\n    p: *mut u32\nend t.Ptr\n\n{}",
        spawn_program(
            "    let mut x = 10u32\n    let p = unsafe \"ptr\" \"the pointer is only read by the thread\" {\n        core.ptr.from_mut(&mut x)\n    }\n    let s = t.Ptr { p: p }\n",
            "move s",
            "            unsafe \"ptr\" \"the thread reads through the pointer\" {\n                core.ptr.read(s.p)\n            }\n",
        )
    );
    let (rc, codes, out) = check_with_raw_ptrs_allowed("raw_struct", &src);
    assert_eq!(rc, Some(1), "expected rejection:\n{}", out);
    assert!(codes.iter().any(|c| c == "E2040"), "expected E2040 for a struct holding *mut:\n{}", out);
}

#[test]
fn plain_value_capture_in_raw_allowed_module_is_send() {
    // 対照: 生ポインタを含まない値の取り込みは、同じパッケージで通る。
    let src = spawn_program("    let n = 7u32\n", "move n", "            n.wrapping_add(1u32)\n");
    let (rc, codes, out) = check_with_raw_ptrs_allowed("plain", &src);
    assert_eq!(rc, Some(0), "expected acceptance:\n{}", out);
    assert!(codes.is_empty(), "no diagnostics expected:\n{}", out);
}
