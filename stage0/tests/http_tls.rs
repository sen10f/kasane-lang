//! `std.http` (要求の解析・上限・chunked・keep-alive)、`std.tls` (TLS の往復と検証)、`apps/shortener_https` の試験。
//! 通信は 127.0.0.1 の上だけ。TLS の試験は `openssl` と `curl` があるときだけ動く (無ければ理由を出して飛ばす)。
//! 起動したサーバは試験の終わりに必ず kill する (`Server` の Drop)。木走査 (`ksn run`) とネイティブ (`ksn build`) の
//! 出力が一致することを確かめる。

mod common;
use common::*;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const PARSE_SRC: &str = include_str!("data/http_tls/parse.ksnl");
const CLIENT_SRC: &str = include_str!("data/http_tls/client.ksnl");

/// `parse.ksnl` の期待出力 (`std.http` の解析・上限・chunked・pipelining・keep-alive の判定)。
const PARSE_OUT: &str = "plain GET /a body=0 keep=1
close GET /a body=0 keep=0
v10 GET /a body=0 keep=0
v10ka GET /a body=0 keep=1
body POST /s body=3 keep=1
chunked POST /c body=9 keep=1
partial
badreq
BadRequest
http2
BadRequest
badhdr
BadRequest
clte
BadRequest
gzip
Unsupported
clbig
BodyTooLarge
clnan
BadRequest
hexbad
BadRequest
chunkbig
BodyTooLarge
headbig
HeadTooLarge
headok GET / body=0 keep=1
crlfbad GET /x body=0 keep=1
pipe1 GET /1 body=0 keep=1
pipe2 GET /2 body=0 keep=0
pipe3 none
split1 none
split2 POST /c body=4 keep=1
";

/// `client.ksnl` の期待出力: 証明書を指定した接続の応答の最初の行、既定の CA の拒否、ホスト名の不一致の拒否。
const CLIENT_OUT: &str = "HTTP/1.1 200 OK
default CA: refused
wrong host: refused
";

/// `ksn run` (木走査) の出力 (標準出力) を返す。終了コードは 0 を期待する。
fn interp_stdout(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    text(&o.stdout)
}

/// ネイティブの実行ファイルを作って走らせ、標準出力を返す (`--release` でもバンドルへの退避は許さない)。
fn native_stdout(name: &str, src: &str) -> String {
    let (t, found, out) = build_native_strict(name, src, true);
    let Some(p) = found else { panic!("ネイティブの実行ファイルがない (退避は許さない): {}", out) };
    assert!(!out.contains("skipped"), "ネイティブに下ろせず退避した: {}", out);
    let o = Command::new(&p).current_dir(&t.0).output().expect("run native");
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    text(&o.stdout)
}

fn tls_tools() -> bool {
    let ok = |tool: &str, arg: &str| Command::new(tool).arg(arg).output().map(|o| o.status.success()).unwrap_or(false);
    let found = ok("openssl", "version") && ok("curl", "--version");
    if !found {
        eprintln!("skipped: openssl と curl が要る (TLS の試験)");
    }
    found
}

/// 自己署名の証明書 (CN=localhost、SAN に localhost と 127.0.0.1) を `dir` に作る。
fn make_cert(dir: &Path) -> (PathBuf, PathBuf) {
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    let o = Command::new("openssl")
        .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout"])
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .args(["-days", "2", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1"])
        .output()
        .expect("openssl");
    assert!(o.status.success(), "openssl req が失敗: {}", text(&o.stderr));
    (cert, key)
}

/// 起動したサーバ (`ksn run` の子プロセス)。`listening` のログからポートを読む。Drop で必ず kill する。
struct Server {
    child: Child,
    port: u16,
    rx: mpsc::Receiver<String>,
    lines: Vec<String>,
}

impl Server {
    fn start(cwd: &Path, args: &[&str], cache: &Path) -> Server {
        let mut child = Command::new(exe())
            .arg("run")
            .args(args)
            .current_dir(cwd)
            .env("KSN_CACHE_DIR", cache)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn ksn run");
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let mut s = Server { child, port: 0, rx, lines: vec![] };
        loop {
            match s.rx.recv_timeout(Duration::from_secs(120)) {
                Ok(l) => {
                    let listening = l.contains("\"msg\":\"listening\"");
                    if listening {
                        s.port = port_of(&l);
                    }
                    s.lines.push(l);
                    if listening {
                        return s;
                    }
                }
                Err(_) => panic!("サーバが待ち受けを知らせない: {:?}", s.lines),
            }
        }
    }

    /// これまでに出たログ (標準エラー) の全行。
    fn logs(&mut self) -> Vec<String> {
        while let Ok(l) = self.rx.try_recv() {
            self.lines.push(l);
        }
        self.lines.clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `"port":"NNNN"` の数。
fn port_of(line: &str) -> u16 {
    let i = line.find("\"port\":\"").expect("port") + 8;
    let rest = &line[i..];
    rest[..rest.find('"').unwrap()].parse().expect("port number")
}

/// `apps/shortener_https` のソース (パッケージ) を `dst` に写す。`target/` は含めない。
fn copy_app(dst: &Path) {
    let src = root().join("apps").join("shortener_https");
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for e in fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let name = e.file_name();
            if name == "target" {
                continue;
            }
            if p.is_dir() {
                copy(&p, &to.join(&name));
            } else {
                fs::copy(&p, to.join(&name)).unwrap();
            }
        }
    }
    copy(&src, dst);
}

/// 店のファイル (`<コード> <URL>` の行)。
fn write_db(dir: &Path) -> PathBuf {
    let db = dir.join("links.db");
    fs::write(&db, "1000 http://a.test/x\n").unwrap();
    db
}

/// `curl` で `urls` を 1 本の接続で GET する。検証は `cacert` で行う (`-k` は使わない)。戻り値は (標準出力, 標準エラー)。
fn curl_get(cacert: &Path, urls: &[String]) -> (String, String) {
    let mut c = Command::new("curl");
    c.args(["-sS", "-v", "--max-time", "30", "--cacert"]).arg(cacert);
    for u in urls {
        c.arg(u);
    }
    let o = c.output().expect("curl");
    assert_eq!(o.status.code(), Some(0), "curl が失敗: {}", text(&o.stderr));
    (text(&o.stdout), text(&o.stderr))
}

// ---- std.http ----

/// 解析・上限・chunked・pipelining・keep-alive の判定が、木走査とネイティブで同じ。
#[test]
fn std_http_parse_interpreter_and_native_agree() {
    assert_eq!(interp_stdout("http_parse_i", PARSE_SRC), PARSE_OUT, "木走査の出力");
    assert_eq!(native_stdout("http_parse_n", PARSE_SRC), PARSE_OUT, "ネイティブの出力");
}

// ---- std.tls の往復 (openssl と curl がある環境だけ) ----

/// `apps/shortener_https` を木走査で起動し、1 本の TLS 接続で複数の要求に答えることを、curl で確かめる。
#[test]
fn https_app_keeps_one_connection_for_several_requests() {
    if !tls_tools() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ksn_https_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let (cert, key) = make_cert(&dir);
    let db = write_db(&dir);
    let cache = dir.join("cache");
    let app = root().join("apps").join("shortener_https");
    let mut server = Server::start(
        &app,
        &["127.0.0.1:0", "0", db.to_str().unwrap(), cert.to_str().unwrap(), key.to_str().unwrap()],
        &cache,
    );
    let base = format!("https://127.0.0.1:{}", server.port);
    let urls = vec![
        format!("{}/stats", base),
        format!("{}/1000", base),
        format!("{}/1001", base),
        format!("{}/shorten", base),
    ];
    let (out, err) = curl_get(&cert, &urls);
    // 検証付き (`--cacert`) で 1 本の接続を 3 回以上使い回している。
    assert!(err.contains("Re-using existing connection"), "接続を使い回していない:\n{}", err);
    assert!(err.matches("SSL connection using TLSv1.3").count() == 1, "TLS の握手が 1 回でない:\n{}", err);
    let statuses: Vec<&str> = err.lines().filter(|l| l.starts_with("< HTTP/1.1 ")).collect();
    assert_eq!(
        statuses,
        vec!["< HTTP/1.1 200 OK", "< HTTP/1.1 302 Found", "< HTTP/1.1 404 Not Found", "< HTTP/1.1 405 Method Not Allowed"],
        "応答の状態:\n{}",
        err
    );
    assert!(out.contains("{\"links\":1}"), "stats の本体: {}", out);
    assert!(out.contains("{\"code\":\"1000\",\"url\":\"http://a.test/x\"}"), "redirect の本体: {}", out);
    assert!(err.contains("< Location: http://a.test/x"), "Location がない:\n{}", err);
    // 証明書の検証なしには通らない (既定の CA では自己署名は信頼されない)。
    let plain = Command::new("curl").args(["-sS", "--max-time", "10", &format!("{}/stats", base)]).output().unwrap();
    assert!(!plain.status.success(), "既定の CA で自己署名が通った");
    // 別の接続で POST (作成) を送る。
    let post = Command::new("curl")
        .args(["-sS", "--max-time", "30", "--cacert"])
        .arg(&cert)
        .args(["-X", "POST", "--data", "http://b.test/", &format!("{}/shorten", base)])
        .output()
        .unwrap();
    assert!(post.status.success(), "{}", text(&post.stderr));
    assert_eq!(text(&post.stdout), "{\"code\":\"1001\",\"url\":\"http://b.test/\"}");
    // ログは 1 行 1 件の JSON。要求ごとに `request` があり、接続を保った要求の `keep_alive` は "1"。
    let logs = server.logs();
    let requests: Vec<&String> = logs.iter().filter(|l| l.contains("\"msg\":\"request\"")).collect();
    assert!(requests.len() >= 5, "要求のログが足りない: {:?}", logs);
    assert!(requests.iter().all(|l| l.starts_with('{') && l.contains("\"level\":\"info\"")), "{:?}", requests);
    assert!(requests[0].contains("\"method\":\"GET\"") && requests[0].contains("\"path\":\"/stats\""));
    assert!(requests[0].contains("\"keep_alive\":\"1\""), "{}", requests[0]);
    let _ = fs::remove_dir_all(&dir);
}

/// `std.tls` のクライアント: 証明書を指定した接続、既定の CA での拒否、ホスト名の不一致での拒否。
/// 木走査とネイティブで同じ出力。サーバは `apps/shortener_https` (木走査で起動)。
#[test]
fn std_tls_client_interpreter_and_native_agree() {
    if !tls_tools() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ksn_tlsc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let (cert, key) = make_cert(&dir);
    let db = write_db(&dir);
    let app = root().join("apps").join("shortener_https");
    let server = Server::start(
        &app,
        &["127.0.0.1:0", "0", db.to_str().unwrap(), cert.to_str().unwrap(), key.to_str().unwrap()],
        &dir.join("cache"),
    );
    let addr = format!("127.0.0.1:{}", server.port);
    let args = [addr.as_str(), cert.to_str().unwrap()];

    let t = Tmp::new("tls_client_i", CLIENT_SRC);
    let o = run(&["run", args[0], args[1]], &t.0);
    assert_eq!(o.status.code(), Some(0), "木走査: {}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), CLIENT_OUT, "木走査の出力");

    let (nt, found, out) = build_native_strict("tls_client_n", CLIENT_SRC, true);
    let Some(p) = found else { panic!("クライアントのネイティブがない: {}", out) };
    assert!(!out.contains("skipped"), "ネイティブに下ろせず退避した: {}", out);
    let o = Command::new(&p).args(args).current_dir(&nt.0).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "ネイティブ: {}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), CLIENT_OUT, "ネイティブの出力");
    drop(server);
    let _ = fs::remove_dir_all(&dir);
}

/// ネイティブの `apps/shortener_https` (LLVM) も、木走査と同じ応答を返す (1 接続の keep-alive)。
#[test]
fn https_app_native_serves_the_same_replies() {
    if !tls_tools() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ksn_https_n_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let pkg = dir.join("app");
    copy_app(&pkg);
    fs::create_dir_all(&dir).unwrap();
    let (cert, key) = make_cert(&dir);
    let db = write_db(&dir);
    let cache = dir.join("cache");
    let build = Command::new(exe())
        .args(["build", "--release"])
        .current_dir(&pkg)
        .env("KSN_CACHE_DIR", &cache)
        .output()
        .unwrap();
    let log = text(&build.stdout) + &text(&build.stderr);
    assert_eq!(build.status.code(), Some(0), "{}", log);
    assert!(!log.contains("skipped"), "ネイティブに下ろせず退避した: {}", log);
    let bin = pkg.join("target").join("release").join("shortener_https.native");
    assert!(bin.exists(), "ネイティブの実行ファイルがない: {}", log);
    let mut child = Command::new(&bin)
        .args(["127.0.0.1:0", "0", db.to_str().unwrap(), cert.to_str().unwrap(), key.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut server = Server { child, port: 0, rx: {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        rx
    }, lines: vec![] };
    loop {
        let l = server.rx.recv_timeout(Duration::from_secs(60)).expect("listening");
        let listening = l.contains("\"msg\":\"listening\"");
        if listening {
            server.port = port_of(&l);
        }
        server.lines.push(l);
        if listening {
            break;
        }
    }
    let base = format!("https://127.0.0.1:{}", server.port);
    let (out, err) = curl_get(&cert, &[format!("{}/stats", base), format!("{}/1000", base)]);
    assert!(err.contains("Re-using existing connection"), "{}", err);
    assert!(err.contains("< HTTP/1.1 200 OK") && err.contains("< HTTP/1.1 302 Found"), "{}", err);
    assert!(out.contains("{\"links\":1}"), "{}", out);
    let _ = fs::remove_dir_all(&dir);
}

// ---- 動的ロード (3.8): 読めないライブラリは panic と同じ扱い (終了コード 101) ----

/// `link "ライブラリ:名前"` の読み込みに失敗したときは、木走査もネイティブも同じ panic と終了コード 101。
#[test]
fn ffi_dynamic_load_failure_is_a_panic_101() {
    let src = "extern t.nope(x: i32) -> i32 link \"libksn-no-such.so.9:nope\"\n\nfn t.main() -> () uses [] panics may\n    let v = unsafe \"t\" \"the library does not exist\" {\n        t.nope(1i32)\n    }\n    core.mem.drop(v)\nend t.main\n";
    let unsafe_toml = format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML);
    let check = |dir: &Path| {
        fs::write(dir.join("kasane.toml"), &unsafe_toml).unwrap();
    };
    let t = Tmp::new("ffi_missing_i", src);
    check(&t.0);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101), "木走査: {}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stderr).contains("cannot load the shared library `libksn-no-such.so.9`"), "{}", text(&o.stderr));

    let n = Tmp::new("ffi_missing_n", src);
    check(&n.0);
    let b = run(&["build", "--release"], &n.0);
    let log = text(&b.stdout) + &text(&b.stderr);
    assert_eq!(b.status.code(), Some(0), "{}", log);
    let p = n.0.join("target").join("release").join("t.native");
    assert!(p.exists(), "ネイティブの実行ファイルがない: {}", log);
    let o = Command::new(&p).current_dir(&n.0).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "ネイティブ: {}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stderr).contains("cannot load the shared library `libksn-no-such.so.9`"), "{}", text(&o.stderr));
}

/// `std.tls` の利用者側の試験に使うデータが、木走査で動く (型検査の確認も兼ねる)。
#[test]
fn tls_client_program_checks() {
    let t = Tmp::new("tls_client_check", CLIENT_SRC);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}
