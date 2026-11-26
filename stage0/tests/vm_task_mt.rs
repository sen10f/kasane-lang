//! `std.task.block_on_threads` と非同期ソケットを VM (`--engine=vm`) とネイティブで比べる (設計_VM一本化.md 6 章)。
//! 同じプログラムの標準出力と終了コードがそろうこと。VM はワーカー 1 本の協調実行、ネイティブは複数スレッド。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;
use std::process::Command;

fn data(name: &str) -> String {
    std::fs::read_to_string(root().join("stage0/tests/data").join(name)).unwrap()
}

/// VM で走らせた (標準出力, 終了コード)。VM が退避していたら失敗にする。
fn vm_run(name: &str, src: &str) -> (String, Option<i32>) {
    let t = Tmp::new(name, src);
    let o = run(&["run", "--engine=vm"], &t.0);
    let err = text(&o.stderr);
    assert!(!err.contains("vm engine skipped"), "{}: VM が退避した: {}", name, err);
    (text(&o.stdout), o.status.code())
}

fn native_run(name: &str, src: &str) -> (String, Option<i32>) {
    let (t, bin, log) = build_native_strict(name, src, true);
    let Some(bin) = bin else {
        if cfg!(target_os = "linux") {
            panic!("ネイティブが作れない: {}", log);
        }
        return (String::new(), None);
    };
    let o = Command::new(&bin).current_dir(&t.0).output().unwrap();
    (text(&o.stdout), o.status.code())
}

fn same(name: &str, src: &str) {
    let v = vm_run(&format!("vmmt_{}_vm", name), src);
    let n = native_run(&format!("vmmt_{}_nat", name), src);
    if cfg!(target_os = "linux") {
        assert_eq!(v, n, "{}: VM とネイティブの出力・終了コードが違う", name);
    }
}

#[test]
fn async_echo_256_vm_matches_native() {
    same("net_async", &data("net_async.ksnl"));
}

#[test]
fn threads_echo_256_vm_matches_native() {
    same("echo", &data("net_async_mt_echo.ksnl"));
}

#[test]
fn threads_stress_100k_tasks_vm_matches_native() {
    same("stress", &data("net_async_mt_stress.ksnl"));
}

#[test]
fn threads_cancel_storm_vm_matches_native() {
    same("cancel", &data("net_async_mt_cancel.ksnl"));
}

#[test]
fn threads_semantics_vm_matches_native() {
    for n in ["nested", "panic", "spawn_join_loop", "spawn_local", "stale_task"] {
        same(n, &data(&format!("net_async_mt_{}.ksnl", n)));
    }
}
