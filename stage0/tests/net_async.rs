//! 非同期ソケット (`accept_async` / `read_async` / `write_async`、設計_多重化したネット.md) の試験。
//! 1 スレッドの実行器 (`std.task.block_on`) で、256 本の接続を同時に捌く echo サーバ。
//! - 全クライアントが 200 バイトを送って同じ 200 バイトを受け取る (`core.check.eq`)。
//! - 何も送らない 1 本のクライアントがいても、ほかの接続は止まらない。
//! - その 1 本は `set_read_timeout(300)` で `read_async` が `TimedOut` になる。
//! - 木走査 (`ksn run`) とネイティブ (`ksn build`) の出力が一致する。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;
use std::process::Command;
use std::time::{Duration, Instant};

const SRC: &str = include_str!("data/net_async.ksnl");

fn with_clients(n: usize) -> String {
    SRC.replace("let clients = 256usize", &format!("let clients = {}usize", n))
}

#[test]
fn interp_echo_256() {
    let t = Tmp::new("net_async_interp", &with_clients(256));
    let o = run(&["run"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert_eq!(text(&o.stdout).trim(), "async echo done", "{}", out);
}

#[test]
fn native_echo_256_matches_interp() {
    let (t, bin, log) = build_native_strict("net_async_native", &with_clients(256), true);
    let Some(bin) = bin else {
        if cfg!(target_os = "linux") {
            panic!("ネイティブが作れない: {}", log);
        }
        return;
    };
    let start = Instant::now();
    let o = Command::new(&bin).current_dir(&t.0).output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert_eq!(text(&o.stdout).trim(), "async echo done", "{}", out);
    // 待ちの本体は 300 ms の期限だけ。256 本の echo は 1 スレッドで数十ミリ秒に収まる (リアクターが fd を待つ)。
    assert!(start.elapsed() < Duration::from_secs(10), "遅すぎる: {:?}", start.elapsed());
    let i = Tmp::new("net_async_cmp", &with_clients(256));
    let oi = run(&["run"], &i.0);
    assert_eq!(text(&oi.stdout), text(&o.stdout));
}

#[test]
fn native_small_counts() {
    for n in [2usize, 3, 64] {
        let (t, bin, log) = build_native_strict(&format!("net_async_n{}", n), &with_clients(n), false);
        let Some(bin) = bin else {
            if cfg!(target_os = "linux") {
                panic!("ネイティブが作れない: {}", log);
            }
            return;
        };
        let o = Command::new(&bin).current_dir(&t.0).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "n={} {}", n, text(&o.stdout) + &text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "async echo done", "n={}", n);
    }
}
