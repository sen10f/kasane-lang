//! `std.net` の追加分 (`TcpStream` のタイムアウト・shutdown・アドレス、`resolve`、`UdpSocket`) の試験。
//! 127.0.0.1 の上だけで動かす (外部のネットワークには接続しない)。木走査 (`ksn run`) と、LLVM のネイティブ
//! (`ksn build` の debug / release。`--release` でも退避を許さない) の出力が一致することを確かめる。

mod common;
#[path = "common/scratch.rs"]
mod scratch;

use common::*;

use std::fs;

const SRC: &str = include_str!("data/net_more.ksnl");

/// `ksn build` でネイティブを作って走らせ、標準出力を返す。退避 (バンドル) は失敗とみなす (`--release` でも `--allow-bundle` を付けない)。
fn native_output(name: &str, release: bool) -> String {
    let t = Tmp::new(name, SRC);
    let cache = scratch::Scratch::at(format!("ksn_cache_netmore_{}_{}", std::process::id(), name));
    let mut c = std::process::Command::new(exe());
    c.arg("build").current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
    if release {
        c.arg("--release");
    }
    let o = c.output().expect("spawn ksn build");
    let log = text(&o.stdout) + &text(&o.stderr);
    let _ = fs::remove_dir_all(&cache);
    assert_eq!(o.status.code(), Some(0), "ksn build が失敗:\n{}", log);
    assert!(!log.contains("skipped"), "ネイティブに下ろせず退避した (退避は許さない):\n{}", log);
    let kind = if release { "release" } else { "debug" };
    let p = t.0.join("target").join(kind).join(if cfg!(windows) { "t.exe" } else { "t.native" });
    assert!(p.exists(), "ネイティブの実行ファイルがない:\n{}", log);
    let o = std::process::Command::new(&p).current_dir(&t.0).output().expect("run native");
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    text(&o.stdout)
}

fn interp_output(name: &str) -> String {
    let t = Tmp::new(name, SRC);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    text(&o.stdout)
}

/// 木走査の出力が期待どおり (タイムアウトは TimedOut、shutdown の EOF、UDP の往復、resolve)。
#[test]
fn std_net_more_interpreter_output_is_as_expected() {
    if !cfg!(unix) {
        return;
    }
    let out = interp_output("netm_interp");
    let has = |l: &str| assert!(out.lines().any(|x| x == l), "出力に `{}` がない:\n{}", l, out);
    // 相手と自分のアドレス (ポートは実行ごとに違うので、一致の判定だけを見る)
    has("client peer is listener: 1");
    has("server local is listener: 1");
    has("client local is server peer: 1");
    has("connect by name ok: 1");
    // タイムアウト: 何も届かない読み出しは TimedOut (kind 9)。code は EAGAIN (Linux の 11)。
    has("set_read_timeout ok: 1");
    assert!(out.lines().any(|l| l.starts_with("read timeout: kind 9 code ")), "TimedOut になっていない:\n{}", out);
    has("clear read timeout ok: 1");
    has("set_write_timeout ok: 1");
    // shutdown: 書きを閉じると相手の読みは 0 (EOF)。読みは閉じても書きは届く。
    has("client shutdown write ok: 1");
    has("server read after client shutdown: 0");
    has("server wrote: 4");
    has("client read: 4");
    has("client first byte: 112");
    has("client shutdown read ok: 1");
    has("client read after shutdown read: 0");
    // UDP の往復 (送り元の文字列も確かめる)
    has("udp sent: 5");
    has("udp received: 5");
    has("udp from is a: 1");
    has("udp reply sent: 4");
    has("udp reply received: 4");
    has("udp reply from is b: 1");
    // resolve: 数値の表記と localhost (IPv4 の 127.0.0.1 は必ず含まれる)
    has("numeric: 1");
    has("numeric: 127.0.0.1:80");
    has("localhost: 127.0.0.1:9");
}

/// 木走査と LLVM のネイティブ (debug / release) の出力が同じ。release は退避しない。
#[test]
fn std_net_more_agrees_between_interpreter_and_the_native_build() {
    if !cfg!(unix) {
        return;
    }
    let interp = interp_output("netm_agree_interp");
    assert_eq!(native_output("netm_agree_debug", false), interp, "debug のネイティブと木走査の出力が違う");
    assert_eq!(native_output("netm_agree_release", true), interp, "release のネイティブと木走査の出力が違う");
}

/// 新しい命令は KIR に下りる (`ksn kir` が検証を通り、`net.*` の命令を出す)。
#[test]
fn std_net_more_lowers_to_kir() {
    let t = Tmp::new("netm_kir", SRC);
    let mut all = String::new();
    for f in ["t.timeouts", "t.shutdowns", "t.udp", "t.udp_pair", "std.net.TcpStream.peer_addr", "std.net.resolve"] {
        let o = run(&["kir", f], &t.0);
        let out = text(&o.stdout) + &text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{}: {}", f, out);
        all.push_str(&out);
    }
    for op in ["net.set_read_timeout", "net.shutdown", "net.peer_addr", "net.udp_bind", "net.udp_send_to", "net.udp_recv_from", "net.resolve"] {
        assert!(all.contains(op), "KIR に {} がない", op);
    }
}
