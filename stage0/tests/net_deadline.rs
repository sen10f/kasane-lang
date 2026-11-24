//! `std.net` の期限つき操作 (`set_read_deadline` / `set_write_deadline` / `clear_deadlines` / `set_nodelay` /
//! `accept_timeout` / `connect_timeout`) の試験。127.0.0.1 の上だけで動かす。木走査 (`ksn run`) と、LLVM の
//! ネイティブ (`ksn build` の debug) の出力が一致することを確かめる。

mod common;
#[path = "common/scratch.rs"]
mod scratch;

use common::*;

use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SRC: &str = include_str!("data/net_deadline.ksnl");

/// 期限切れの OS 番号 (Linux 110 / macOS 60)。
fn etimedout() -> i32 {
    if cfg!(target_os = "macos") {
        60
    } else {
        110
    }
}

/// `ksn build` (debug) でネイティブを作る。退避 (バンドル) は失敗とみなす。
fn native_bin(name: &str) -> (Tmp, std::path::PathBuf) {
    let t = Tmp::new(name, SRC);
    let cache = scratch::Scratch::at(format!("ksn_cache_netdl_{}_{}", std::process::id(), name));
    let o = Command::new(exe()).arg("build").current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().expect("spawn ksn build");
    let log = text(&o.stdout) + &text(&o.stderr);
    let _ = fs::remove_dir_all(&cache);
    assert_eq!(o.status.code(), Some(0), "ksn build が失敗:\n{}", log);
    assert!(!log.contains("skipped"), "ネイティブに下ろせず退避した:\n{}", log);
    let p = t.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t" });
    assert!(p.exists(), "ネイティブの実行ファイルがない:\n{}", log);
    (t, p)
}

/// 期限つきの接続・受理・ソケット設定の出力 (環境変数なしで走らせる部分)。
fn check_lines(out: &str) {
    let has = |l: &str| assert!(out.lines().any(|x| x == l), "出力に `{}` がない:\n{}", l, out);
    let starts = |p: &str| assert!(out.lines().any(|x| x.starts_with(p)), "出力に `{}` で始まる行がない:\n{}", p, out);
    // 閉じたポートへの接続は、待たずに拒否される (どの環境でも Err)。
    starts("closed port refused: net code ");
    // 届かない宛先は、期限の中で失敗する (Err)。
    has("unroutable failed: 1");
    // 誰も接続しないリスナの受理は期限切れの番号で失敗する。
    has(&format!("accept timeout: net code {}", etimedout()));
    has("nodelay ok: 1");
    has("write deadline ok: 1");
    has("clear deadlines ok: 1");
}

/// 木走査とネイティブの両方で、期限つきの接続・受理・ソケット設定が期待どおり。
#[test]
fn std_net_deadline_checks_match_in_both_engines() {
    if !cfg!(unix) {
        return;
    }
    let start = Instant::now();
    let t = Tmp::new("netdl_checks_i", SRC);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let interp = text(&o.stdout);
    check_lines(&interp);

    let (nt, p) = native_bin("netdl_checks_n");
    let o = Command::new(&p).current_dir(&nt.0).output().expect("run native");
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let native = text(&o.stdout);
    check_lines(&native);
    assert_eq!(interp, native, "木走査と LLVM ネイティブの出力が違う");
    // 期限は 0.5 s + 0.2 s + 0.15 s の上限。待ちきれずに止まっていないことを確かめる。
    assert!(start.elapsed() < Duration::from_secs(60), "期限つきの操作が上限を超えた");
}

/// 1 バイト/秒の送り手 (slowloris) に対して、`set_read_deadline(300)` の読みは期限で TimedOut になる。
/// 読みごとの待ちでは届き続けるので、期限が壁時計で効いていることは受け取ったバイト数で分かる。
fn trickle_client(port: u16) {
    let until = Instant::now() + Duration::from_secs(10);
    let mut conn = None;
    while conn.is_none() && Instant::now() < until {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(c) => conn = Some(c),
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let Some(mut c) = conn else { return };
    for _ in 0..20 {
        if c.write_all(b"x").is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    l.local_addr().expect("addr").port()
}

/// 出力を確かめる。TimedOut (kind 9) で止まり、期限の前に届いた分だけを読んでいる。
fn check_trickle(out: &str) {
    let has = |l: &str| assert!(out.lines().any(|x| x == l), "出力に `{}` がない:\n{}", l, out);
    has("deadline set: 1");
    has(&format!("read deadline: kind 9 code {}", etimedout()));
    assert!(!out.lines().any(|l| l == "eof: 1"), "期限の前に EOF になった:\n{}", out);
    let got: i64 = out
        .lines()
        .find_map(|l| l.strip_prefix("bytes before deadline: "))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(-1);
    assert!((0..=5).contains(&got), "期限の中で届いたバイト数が多すぎる (期限が効いていない): {}\n{}", got, out);
}

#[test]
fn std_net_read_deadline_stops_a_trickle_in_both_engines() {
    if !cfg!(unix) {
        return;
    }
    // 木走査: 子プロセスの `ksn run` を環境変数 KSN_DL_PORT つきで走らせる。
    let port = free_port();
    let t = Tmp::new("netdl_slow_i", SRC);
    let child = Command::new(exe())
        .arg("run")
        .current_dir(&t.0)
        .env("KSN_DL_PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ksn run");
    let client = std::thread::spawn(move || trickle_client(port));
    let o = child.wait_with_output().expect("wait ksn run");
    client.join().expect("client");
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let interp = text(&o.stdout);
    check_trickle(&interp);

    // ネイティブ: 同じ入力を ksn build の debug で作って走らせる。
    let port = free_port();
    let (nt, p) = native_bin("netdl_slow_n");
    let child = Command::new(&p)
        .current_dir(&nt.0)
        .env("KSN_DL_PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn native");
    let client = std::thread::spawn(move || trickle_client(port));
    let o = child.wait_with_output().expect("wait native");
    client.join().expect("client");
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    // 受け取った数は時間で決まる (期限の中で届いた分) ので、両方が期限の条件を満たすことだけを見る。
    check_trickle(&text(&o.stdout));
}
