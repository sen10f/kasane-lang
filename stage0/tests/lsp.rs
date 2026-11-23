//! `ksn lsp` の統合テスト。実際に子プロセスを起動し、`Content-Length` 付きのメッセージをパイプで流して応答を確かめる。
//! 応答待ちにはすべてタイムアウトがあり、サーバが固まってもテストは失敗で終わる。

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";
const WAIT: Duration = Duration::from_secs(20);

/// 非 ASCII (あ = BMP、😀 = BMP 外で UTF-16 では 2 単位) の後ろに誤りのある行を含む。
const SRC_BAD: &str = "\
/// 2 つの数を足す。
/// 2 行目の説明。
fn t.add(a: u32, b: u32) -> u32 uses [] panics never pub
    a.wrapping_add(b)
end t.add

fn t.main() -> u32 uses [] panics never pub
    let n = \"あ😀\".len().wrapping_ad(1usize)
    t.add(1u32, 2u32)
end t.main
";

const SRC_GOOD: &str = "\
/// 2 つの数を足す。
/// 2 行目の説明。
fn t.add(a: u32, b: u32) -> u32 uses [] panics never pub
    a.wrapping_add(b)
end t.add

fn t.main() -> u32 uses [] panics never pub
    t.add(1u32, 2u32)
end t.main
";

struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str, src: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_lsp_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), TOML).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        // コメント (`///` の文書) のある入力は `.ksnh` が正で、`.ksnl` はその `strip` (`.ksnl` にコメントは置けない)。
        let ksnl: String = src.lines().filter(|l| !l.trim_start().starts_with("//")).map(|l| format!("{l}\n")).collect();
        if ksnl != src {
            fs::write(d.join("src").join("t.ksnh"), src).unwrap();
        }
        fs::write(d.join("src").join("t.ksnl"), ksnl).unwrap();
        // 書き方は 1 通り (E0020): 試験の入力は `ksn fmt` の出力にそろえる。
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_ksn")).arg("fmt").current_dir(&d).output();
        // 正規化 (macOS の /var など) してから使う。
        Tmp(fs::canonicalize(d).unwrap())
    }
    fn file(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
    fn uri(&self, rel: &str) -> String {
        format!("file://{}", self.file(rel).to_string_lossy())
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        scratch::remove(&self.0);
    }
}

struct Client {
    child: Child,
    rx: Receiver<Value>,
    next_id: i64,
    /// 待っている間に届いた通知。
    inbox: Vec<Value>,
}

impl Client {
    fn start() -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ksn"))
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ksn lsp");
        let out = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut r = BufReader::new(out);
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
                    } else {
                        // 標準出力にプロトコル以外が書かれた。
                        let _ = tx.send(json!({"__garbage": h}));
                    }
                }
                let mut buf = vec![0u8; len];
                if r.read_exact(&mut buf).is_err() {
                    return;
                }
                let _ = tx.send(serde_json::from_slice(&buf).expect("server sent invalid JSON"));
            }
        });
        Client { child, rx, next_id: 1, inbox: vec![] }
    }

    fn send_raw(&mut self, body: &[u8]) {
        let si = self.child.stdin.as_mut().unwrap();
        write!(si, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        si.write_all(body).unwrap();
        si.flush().unwrap();
    }

    fn send(&mut self, v: Value) {
        self.send_raw(v.to_string().as_bytes());
    }

    fn recv(&mut self, deadline: Instant) -> Value {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.rx.recv_timeout(left) {
            Ok(v) => {
                assert!(v.get("__garbage").is_none(), "non-protocol output on stdout: {}", v);
                v
            }
            Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for the server"),
            Err(RecvTimeoutError::Disconnected) => panic!("server closed stdout"),
        }
    }

    /// 要求を送り、同じ id の応答 (`result` または `error` を含むオブジェクト) を返す。
    fn call_raw(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + WAIT;
        loop {
            let v = self.recv(deadline);
            if v.get("id") == Some(&json!(id)) {
                return v;
            }
            self.inbox.push(v);
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let r = self.call_raw(method, params);
        assert!(r.get("error").is_none(), "{} failed: {}", method, r);
        r["result"].clone()
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// `uri` への publishDiagnostics のうち、`pred` を満たす最初のものを待つ。
    fn diagnostics_until(&mut self, uri: &str, pred: impl Fn(&Vec<Value>) -> bool) -> Vec<Value> {
        let deadline = Instant::now() + WAIT;
        let mut i = 0;
        loop {
            while i < self.inbox.len() {
                let m = &self.inbox[i];
                if m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri {
                    let ds = m["params"]["diagnostics"].as_array().unwrap().clone();
                    if pred(&ds) {
                        self.inbox.drain(..=i);
                        return ds;
                    }
                }
                i += 1;
            }
            let v = self.recv(deadline);
            self.inbox.push(v);
        }
    }

    fn initialize(&mut self, root: &Path) -> Value {
        let r = self.call("initialize", json!({"processId": null, "rootUri": format!("file://{}", root.display()), "capabilities": {}}));
        self.notify("initialized", json!({}));
        r
    }

    fn open(&mut self, uri: &str, text: &str) {
        self.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": "ksnl", "version": 1, "text": text}}));
    }

    fn shutdown(mut self) {
        let r = self.call("shutdown", Value::Null);
        assert_eq!(r, Value::Null);
        self.notify("exit", Value::Null);
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                assert!(st.success(), "exit status {:?}", st);
                return;
            }
            assert!(Instant::now() < deadline, "server did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tdoc(uri: &str) -> Value {
    json!({"uri": uri})
}

fn at(uri: &str, line: u32, ch: u32) -> Value {
    json!({"textDocument": tdoc(uri), "position": {"line": line, "character": ch}})
}

/// 本文に `edits` (LSP の TextEdit の列) を適用する。
fn apply(text: &str, edits: &Value) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let off = |p: &Value| -> usize {
        let (l, c) = (p["line"].as_u64().unwrap() as usize, p["character"].as_u64().unwrap() as usize);
        let mut o: usize = lines[..l].iter().map(|x| x.len() + 1).sum();
        let mut u = 0;
        for ch in lines[l].chars() {
            if u >= c {
                break;
            }
            u += ch.len_utf16();
            o += ch.len_utf8();
        }
        o
    };
    let mut es: Vec<(usize, usize, String)> =
        edits.as_array().unwrap().iter().map(|e| (off(&e["range"]["start"]), off(&e["range"]["end"]), e["newText"].as_str().unwrap().to_string())).collect();
    es.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out = text.to_string();
    for (a, b, t) in es {
        out.replace_range(a..b, &t);
    }
    out
}

#[test]
fn initialize_and_shutdown() {
    let t = Tmp::new("init", SRC_GOOD);
    let mut c = Client::start();
    let r = c.initialize(&t.0);
    let cap = &r["capabilities"];
    assert_eq!(cap["textDocumentSync"]["change"], 1);
    for k in ["hoverProvider", "definitionProvider", "referencesProvider", "documentFormattingProvider", "documentSymbolProvider", "workspaceSymbolProvider"] {
        assert_eq!(cap[k], true, "{}", k);
    }
    assert_eq!(cap["renameProvider"]["prepareProvider"], true);
    assert_eq!(r["serverInfo"]["name"], "ksn");
    c.shutdown();
}

#[test]
fn diagnostics_with_utf16_range_and_clearing_on_change() {
    // `;` は Rust 風の書き方 (E0040)。その手前に "あ😀" があるので、列の変換が効いていないと位置がずれる。
    let semi = SRC_GOOD.replace("    t.add(1u32, 2u32)\nend t.main", "    let n = \"あ😀\";\n    t.add(1u32, 2u32)\nend t.main");
    let t = Tmp::new("diag", &semi);
    // `ksn fmt` は行末の `;` を消すので、`;` の入ったままの `.ksnh` を置き直す (診断はエディタの内容から出る)。
    fs::write(t.file("src/t.ksnh"), &semi).unwrap();
    let uri = t.uri("src/t.ksnh");
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&uri, &semi);
    let ds = c.diagnostics_until(&uri, |d| !d.is_empty());
    let d = ds.iter().find(|d| d["code"] == "E0040").unwrap_or_else(|| panic!("no E0040 in {:?}", ds));
    assert_eq!(d["severity"], 1);
    assert_eq!(d["source"], "ksn");
    let line = semi.lines().nth(7).unwrap();
    let scalar = line.chars().position(|ch| ch == ';').unwrap();
    let utf16: usize = line.split(';').next().unwrap().encode_utf16().count();
    assert_eq!(utf16, scalar + 1, "the line must contain a non-BMP character");
    assert_eq!(d["range"]["start"], json!({"line": 7, "character": utf16}));
    assert_eq!(d["range"]["end"], json!({"line": 7, "character": utf16 + 1}));

    // 保存せずに直す (didChange): 診断が消える。
    c.notify("textDocument/didChange", json!({"textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"text": SRC_GOOD}]}));
    let ds = c.diagnostics_until(&uri, |d| d.is_empty());
    assert!(ds.is_empty());
    // ディスクは壊れたまま。
    assert_eq!(fs::read_to_string(t.file("src/t.ksnh")).unwrap(), semi);
    c.shutdown();
}

#[test]
fn code_action_applies_the_fix() {
    let t = Tmp::new("fix", SRC_BAD);
    let uri = t.uri("src/t.ksnh");
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&uri, SRC_BAD);
    c.diagnostics_until(&uri, |d| !d.is_empty());
    let acts = c.call(
        "textDocument/codeAction",
        json!({"textDocument": tdoc(&uri), "range": {"start": {"line": 7, "character": 0}, "end": {"line": 7, "character": 0}}, "context": {"diagnostics": []}}),
    );
    // 編集距離の当て推量 (`wrapping_ad` → `wrapping_add`) は直しとして出さない (2026-11-23、B3:
    // 意味が同じと言い切れる書き換えだけを直しにする)。案内は診断の文面に残る。
    assert!(!acts.as_array().unwrap().iter().any(|a| a["title"].as_str().unwrap().contains("E1108")), "{}", acts);
    // 範囲外の行には出ない。
    let none = c.call(
        "textDocument/codeAction",
        json!({"textDocument": tdoc(&uri), "range": {"start": {"line": 0, "character": 0}, "end": {"line": 1, "character": 0}}, "context": {"diagnostics": []}}),
    );
    assert!(none.as_array().unwrap().is_empty());
    c.shutdown();
}

#[test]
fn code_action_e2023_adds_missing_reference() {
    // `t.a(v)` (v: u8) は `a(x: &u8)` への呼び出しなので、引数に `&` が足りない (E2023)。
    let src = "\
fn t.a(x: &u8) -> u8 uses [] panics never pub
    *x
end t.a

fn t.main() -> u32 uses [] panics never pub
    let v = 1u8
    t.a(v)
end t.main
";
    let t = Tmp::new("e2023", src);
    let uri = t.uri("src/t.ksnl");
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&uri, src);
    c.diagnostics_until(&uri, |d| !d.is_empty());
    // `t.a(v)` は 7 行目 (0 始まり 6)。
    let acts = c.call(
        "textDocument/codeAction",
        json!({"textDocument": tdoc(&uri), "range": {"start": {"line": 6, "character": 0}, "end": {"line": 6, "character": 0}}, "context": {"diagnostics": []}}),
    );
    let a = acts.as_array().unwrap().iter().find(|a| a["title"].as_str().unwrap().contains("E2023")).expect("quickfix for E2023");
    assert_eq!(a["kind"], "quickfix");
    let fixed = apply(src, &a["edit"]["changes"][&uri]);
    assert!(fixed.contains("t.a(&v)"), "{}", fixed);
    c.shutdown();
}

#[test]
fn hover_definition_references() {
    let t = Tmp::new("nav", SRC_GOOD);
    let uri = t.uri("src/t.ksnh");
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&uri, SRC_GOOD);
    // 行 7 の `t.add(` の add の上。
    let h = c.call("textDocument/hover", at(&uri, 7, 8));
    let md = h["contents"]["value"].as_str().unwrap();
    assert!(md.contains("fn t.add(a: u32, b: u32) -> u32 uses [] panics never pub"), "{}", md);
    assert!(md.contains("2 つの数を足す。\n2 行目の説明。"), "{}", md);
    // 何もない場所は null。
    assert_eq!(c.call("textDocument/hover", at(&uri, 3, 0)), Value::Null);

    let d = c.call("textDocument/definition", at(&uri, 7, 8));
    let loc = &d.as_array().unwrap()[0];
    assert_eq!(loc["uri"], uri);
    assert_eq!(loc["range"]["start"], json!({"line": 2, "character": 3}));
    assert_eq!(loc["range"]["end"], json!({"line": 2, "character": 8}));

    let r = c.call("textDocument/references", json!({"textDocument": tdoc(&uri), "position": {"line": 2, "character": 5}, "context": {"includeDeclaration": false}}));
    let rs = r.as_array().unwrap();
    assert_eq!(rs.len(), 1, "{:?}", rs);
    assert_eq!(rs[0]["range"]["start"], json!({"line": 7, "character": 4}));
    assert_eq!(rs[0]["range"]["end"], json!({"line": 7, "character": 9}));
    let r = c.call("textDocument/references", json!({"textDocument": tdoc(&uri), "position": {"line": 2, "character": 5}, "context": {"includeDeclaration": true}}));
    assert_eq!(r.as_array().unwrap().len(), 2);
    c.shutdown();
}

#[test]
fn rename_with_prepare() {
    let t = Tmp::new("rename", SRC_GOOD);
    let uri = t.uri("src/t.ksnh");
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&uri, SRC_GOOD);
    let p = c.call("textDocument/prepareRename", at(&uri, 2, 5));
    assert_eq!(p["placeholder"], "add");
    assert_eq!(p["range"], json!({"start": {"line": 2, "character": 5}, "end": {"line": 2, "character": 8}}));
    let w = c.call("textDocument/rename", json!({"textDocument": tdoc(&uri), "position": {"line": 2, "character": 5}, "newName": "plus"}));
    let out = apply(SRC_GOOD, &w["changes"][&uri]);
    assert!(out.contains("fn t.plus(a: u32") && out.contains("end t.plus") && out.contains("    t.plus(1u32, 2u32)"), "{}", out);
    assert!(!out.contains("t.add"));
    // モジュールの移動を伴う名前は断る。
    let e = c.call_raw("textDocument/rename", json!({"textDocument": tdoc(&uri), "position": {"line": 2, "character": 5}, "newName": "x.y"}));
    assert!(e.get("error").is_some(), "{}", e);
    // 定義でない場所は改名できない。
    let e = c.call_raw("textDocument/prepareRename", at(&uri, 3, 0));
    assert!(e.get("error").is_some(), "{}", e);
    c.shutdown();
}

const TWO_MAIN: &str = "\
import t.util

fn t.main() -> u32 uses [] panics never pub
    util.inc(1u32)
end t.main
";

const TWO_UTIL: &str = "\
fn t.util.inc(a: u32) -> u32 uses [] panics never pub
    a.wrapping_add(1u32)
end t.util.inc
";

impl Tmp {
    /// 2 ファイルのパッケージ (t と t.util)。
    fn two(name: &str) -> Tmp {
        let t = Tmp::new(name, TWO_MAIN);
        fs::create_dir_all(t.file("src/t")).unwrap();
        fs::write(t.file("src/t/util.ksnl"), TWO_UTIL).unwrap();
        t
    }
}

fn initialize_with_file_ops(c: &mut Client, root: &Path) {
    let r = c.call(
        "initialize",
        json!({"processId": null, "rootUri": format!("file://{}", root.display()),
               "capabilities": {"workspace": {"workspaceEdit": {"documentChanges": true, "resourceOperations": ["rename"]}}}}),
    );
    assert_eq!(r["capabilities"]["renameProvider"]["prepareProvider"], true);
    c.notify("initialized", json!({}));
}

#[test]
fn rename_touching_two_files() {
    let t = Tmp::two("rename2");
    let main = t.uri("src/t.ksnl");
    let util = t.uri("src/t/util.ksnl");
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&main, TWO_MAIN);
    // `util.inc` の `inc` (短縮形の呼び出し) に置いて改名する。
    let w = c.call("textDocument/rename", json!({"textDocument": tdoc(&main), "position": {"line": 3, "character": 10}, "newName": "bump"}));
    let changes = &w["changes"];
    assert_eq!(changes.as_object().unwrap().len(), 2, "{}", w);
    let m = apply(TWO_MAIN, &changes[&main]);
    assert!(m.contains("    util.bump(1u32)"), "{}", m);
    let u = apply(TWO_UTIL, &changes[&util]);
    assert!(u.contains("fn t.util.bump(") && u.contains("end t.util.bump"), "{}", u);
    // 参照 (includeDeclaration なし) は呼び出し 1 件だけ。
    let r = c.call("textDocument/references", json!({"textDocument": tdoc(&main), "position": {"line": 3, "character": 10}, "context": {"includeDeclaration": false}}));
    assert_eq!(r.as_array().unwrap().len(), 1, "{}", r);
    c.shutdown();
}

#[test]
fn rename_module_returns_a_file_rename_operation() {
    let t = Tmp::two("renamemod");
    let main = t.uri("src/t.ksnl");
    // ファイル操作を扱えないクライアントには、断る。
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&main, TWO_MAIN);
    let e = c.call_raw("textDocument/rename", json!({"textDocument": tdoc(&main), "position": {"line": 0, "character": 10}, "newName": "t.tools"}));
    assert!(e.get("error").is_some(), "{}", e);
    c.shutdown();
    // 扱えるクライアントには documentChanges で、編集とファイルの移動を返す。
    let t = Tmp::two("renamemod2");
    let main = t.uri("src/t.ksnl");
    let util = t.uri("src/t/util.ksnl");
    let mut c = Client::start();
    initialize_with_file_ops(&mut c, &t.0);
    c.open(&main, TWO_MAIN);
    let p = c.call("textDocument/prepareRename", at(&main, 0, 10));
    assert_eq!(p["placeholder"], "util");
    let w = c.call("textDocument/rename", json!({"textDocument": tdoc(&main), "position": {"line": 0, "character": 10}, "newName": "t.tools"}));
    let dc = w["documentChanges"].as_array().unwrap_or_else(|| panic!("{}", w));
    let ops: Vec<&Value> = dc.iter().filter(|x| x.get("kind").is_some()).collect();
    assert_eq!(ops.len(), 1, "{}", w);
    assert_eq!(ops[0]["kind"], "rename");
    assert_eq!(ops[0]["oldUri"], util.as_str());
    assert_eq!(ops[0]["newUri"], t.uri("src/t/tools.ksnl").as_str());
    let edit_of = |uri: &str| dc.iter().find(|x| x["textDocument"]["uri"] == uri).map(|x| x["edits"].clone());
    let m = apply(TWO_MAIN, &edit_of(&main).unwrap());
    assert!(m.contains("import t.tools") && m.contains("tools.inc(1u32)"), "{}", m);
    let u = apply(TWO_UTIL, &edit_of(&util).unwrap());
    assert!(u.contains("fn t.tools.inc(") && u.contains("end t.tools.inc"), "{}", u);
    // 編集は元のファイル (旧 URI) に対するもので、移動の操作より前に並ぶ。
    let pos_edit = dc.iter().position(|x| x["textDocument"]["uri"] == util.as_str()).unwrap();
    let pos_op = dc.iter().position(|x| x.get("kind").is_some()).unwrap();
    assert!(pos_edit < pos_op);
    c.shutdown();
}

#[test]
fn formatting_and_symbols() {
    // 定義の並びが正規形でないソース (fn の後に type)。
    let src = "\
fn t.main() -> u32 uses [] panics never pub
    1u32
end t.main

const t.LIMIT: u32 = 5u32
";
    let t = Tmp::new("fmt", src);
    let uri = t.uri("src/t.ksnl");
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&uri, src);
    let e = c.call("textDocument/formatting", json!({"textDocument": tdoc(&uri), "options": {"tabSize": 4, "insertSpaces": true}}));
    let out = apply(src, &e);
    assert!(out.starts_with("const t.LIMIT"), "{}", out);
    assert!(out.contains("end t.main"));

    let s = c.call("textDocument/documentSymbol", json!({"textDocument": tdoc(&uri)}));
    let names: Vec<&str> = s.as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["t.main", "t.LIMIT"]);
    assert_eq!(s[0]["selectionRange"]["start"], json!({"line": 0, "character": 3}));
    let w = c.call("workspace/symbol", json!({"query": "limit"}));
    assert_eq!(w.as_array().unwrap().len(), 1);
    assert_eq!(w[0]["name"], "t.LIMIT");
    assert_eq!(w[0]["location"]["uri"], uri);
    c.shutdown();
}

#[test]
fn robust_against_malformed_input_and_broken_manifest() {
    let t = Tmp::new("robust", SRC_GOOD);
    let uri = t.uri("src/t.ksnh");
    let mut c = Client::start();
    c.initialize(&t.0);
    // 壊れた JSON、オブジェクトでない値、method の無い要求、未知のメソッド、Content-Length の無いヘッダ。
    c.send_raw(b"{ this is not json");
    let r = c.recv(Instant::now() + WAIT);
    assert_eq!(r["error"]["code"], -32700);
    c.send_raw(b"[1, 2]");
    assert_eq!(c.recv(Instant::now() + WAIT)["error"]["code"], -32600);
    c.send_raw(br#"{"jsonrpc":"2.0","id":99}"#);
    assert_eq!(c.recv(Instant::now() + WAIT)["error"]["code"], -32600);
    let r = c.call_raw("no/such/method", json!({}));
    assert_eq!(r["error"]["code"], -32601);
    {
        let si = c.child.stdin.as_mut().unwrap();
        si.write_all(b"X-Junk: 1\r\n\r\n").unwrap();
        si.flush().unwrap();
    }
    assert_eq!(c.recv(Instant::now() + WAIT)["error"]["code"], -32600);
    // 引数が足りない要求。
    let r = c.call_raw("textDocument/hover", json!({}));
    assert!(r.get("error").is_some());
    // kasane.toml を壊す → 診断 (kasane.toml 宛て) が出て、落ちない。
    fs::write(t.file("kasane.toml"), "[package\nname =").unwrap();
    c.open(&uri, SRC_GOOD);
    let toml_uri = t.uri("kasane.toml");
    let ds = c.diagnostics_until(&toml_uri, |d| !d.is_empty());
    assert_eq!(ds[0]["code"], "E9002");
    // kasane.toml が無い場所の文書: 警告の診断が出る。
    // `lone_dir` は解体まで生かす (`canonicalize` に値で渡すと、その場で消える)。
    let lone_dir = scratch::Scratch::at(format!("ksn_lsp_lone_{}", std::process::id()));
    fs::create_dir_all(&lone_dir).unwrap();
    let lone = fs::canonicalize(&lone_dir).unwrap();
    fs::write(lone.join("a.ksnl"), "x\n").unwrap();
    let lone_uri = format!("file://{}", lone.join("a.ksnl").display());
    c.open(&lone_uri, "x\n");
    let ds = c.diagnostics_until(&lone_uri, |d| !d.is_empty());
    assert_eq!(ds[0]["code"], "E9001");
    let _ = fs::remove_dir_all(&lone);
    // まだ応答する。
    let r = c.call("textDocument/documentSymbol", json!({"textDocument": tdoc(&uri)}));
    assert!(r.is_array());
    c.shutdown();
}

#[test]
fn requests_before_initialize_are_rejected() {
    let mut c = Client::start();
    let r = c.call_raw("textDocument/hover", json!({}));
    assert_eq!(r["error"]["code"], -32002);
    // exit は shutdown なしだと異常終了コード。
    c.notify("exit", Value::Null);
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(st) = c.child.try_wait().unwrap() {
            assert_eq!(st.code(), Some(1));
            return;
        }
        assert!(Instant::now() < deadline, "server did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `.ksnl` を開いたとき: 同じモジュールの `.ksnh` から `///` の文書を引き、位置は `.ksnl` の座標で返す。
#[test]
fn ksnl_open_reads_docs_from_the_sibling_ksnh() {
    let t = Tmp::new("ksnl_docs", SRC_GOOD);
    let uri = t.uri("src/t.ksnl");
    let ksnl = fs::read_to_string(t.file("src/t.ksnl")).unwrap();
    assert!(!ksnl.contains("///"));
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&uri, &ksnl);
    // `.ksnl` では `t.add(` の呼び出しは 5 行目 (`///` の 2 行が無い)。
    let h = c.call("textDocument/hover", at(&uri, 5, 8));
    let md = h["contents"]["value"].as_str().unwrap();
    assert!(md.contains("2 つの数を足す。\n2 行目の説明。"), "{}", md);
    let d = c.call("textDocument/definition", at(&uri, 5, 8));
    let loc = &d.as_array().unwrap()[0];
    assert_eq!(loc["uri"], uri);
    assert_eq!(loc["range"]["start"], json!({"line": 0, "character": 3}));
    let r = c.call("textDocument/references", json!({"textDocument": tdoc(&uri), "position": {"line": 0, "character": 5}, "context": {"includeDeclaration": true}}));
    assert_eq!(r.as_array().unwrap().len(), 2);
    c.shutdown();
}

/// `.ksnh` の整形: コメントを残して整形し、保存すると横の `.ksnl` が `strip` で作り直される。
#[test]
fn ksnh_formatting_and_save_rebuild_the_ksnl() {
    let src = "/// 説明。\nconst t.LIMIT: u32 = 5u32\n";
    let bad = "const t.LIMIT: u32 = 5u32\n\n\n/// 説明。\nfn t.main() -> () uses [] panics never pub\n    ()\nend t.main\n";
    let t = Tmp::new("ksnh_fmt", src);
    let uri = t.uri("src/t.ksnh");
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&uri, bad);
    let e = c.call("textDocument/formatting", json!({"textDocument": tdoc(&uri), "options": {"tabSize": 4, "insertSpaces": true}}));
    let out = apply(bad, &e);
    assert!(out.contains("/// 説明。\nfn t.main") && !out.contains("\n\n\n"), "{}", out);
    c.notify("textDocument/didSave", json!({"textDocument": {"uri": uri}, "text": out}));
    c.call("textDocument/documentSymbol", json!({"textDocument": tdoc(&uri)}));
    let ksnl = fs::read_to_string(t.file("src/t.ksnl")).unwrap();
    assert!(!ksnl.contains("///") && ksnl.contains("fn t.main"), "{}", ksnl);
    c.shutdown();
}

/// FQN で書いた呼び出し (import なし) の上の hover / definition (G50)。カーソルは各行の語の上。
const FQN_MAIN: &str = "\
fn t.main() -> u32 uses [] panics never pub
    let r = t.util.inc(1u32)
    core.mem.drop(r)
    t.other.go(2u32)
end t.main

fn t.use_pair(p: t.other.Pair) -> u32 uses [] panics never pub
    p.x
end t.use_pair
";

const OTHER_MOD: &str = "\
type t.other.Pair pub
    x: u32
end t.other.Pair

fn t.other.go(a: u32) -> u32 uses [] panics never pub
    a.wrapping_add(1u32)
end t.other.go
";

#[test]
fn hover_definition_fqn_calls_across_modules_std_and_types() {
    let t = Tmp::new("fqn_nav", FQN_MAIN);
    fs::create_dir_all(t.file("src/t")).unwrap();
    fs::write(t.file("src/t/util.ksnl"), TWO_UTIL).unwrap();
    fs::write(t.file("src/t/other.ksnl"), OTHER_MOD).unwrap();
    let uri = t.uri("src/t.ksnl");
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&uri, FQN_MAIN);
    // 別のモジュールの関数 (`t.util.inc` の `inc` の上)。定義は `util.ksnl` の先頭行。
    let md = c.call("textDocument/hover", at(&uri, 1, 19))["contents"]["value"].as_str().unwrap().to_string();
    assert!(md.contains("fn t.util.inc(a: u32) -> u32"), "{}", md);
    let d = c.call("textDocument/definition", at(&uri, 1, 19));
    assert!(d[0]["uri"].as_str().unwrap().ends_with("src/t/util.ksnl"), "{}", d);
    assert_eq!(d[0]["range"]["start"], json!({"line": 0, "character": 3}));
    // 経路の途中のモジュール (`t.util` の `util` の上) はモジュール。
    let md = c.call("textDocument/hover", at(&uri, 1, 14))["contents"]["value"].as_str().unwrap().to_string();
    assert!(md.contains("モジュール `t.util`"), "{}", md);
    let d = c.call("textDocument/definition", at(&uri, 1, 14));
    assert!(d[0]["uri"].as_str().unwrap().ends_with("src/t/util.ksnl"), "{}", d);
    // 経路の途中のモジュールは改名・参照の対象にしない (null)。
    let r = c.call("textDocument/references", json!({"textDocument": tdoc(&uri), "position": {"line": 1, "character": 14}, "context": {"includeDeclaration": true}}));
    assert_eq!(r, Value::Null);
    // 標準ライブラリの関数 (`core.mem.drop` の `drop` の上): `.sig` の宣言。
    let md = c.call("textDocument/hover", at(&uri, 2, 14))["contents"]["value"].as_str().unwrap().to_string();
    assert!(md.contains("fn core.mem.drop[T](value: T) -> ()"), "{}", md);
    let d = c.call("textDocument/definition", at(&uri, 2, 14));
    assert!(d[0]["uri"].as_str().unwrap().ends_with("core/mem.sig"), "{}", d);
    // 同じく、標準ライブラリのモジュール (`core.mem` の `mem` の上) はモジュール。
    let md = c.call("textDocument/hover", at(&uri, 2, 9))["contents"]["value"].as_str().unwrap().to_string();
    assert!(md.contains("モジュール `core.mem`"), "{}", md);
    // 別のモジュールの関数 (`t.other.go` の `go` の上)、そのモジュール (`other` の上)。
    let md = c.call("textDocument/hover", at(&uri, 3, 12))["contents"]["value"].as_str().unwrap().to_string();
    assert!(md.contains("fn t.other.go(a: u32) -> u32"), "{}", md);
    let d = c.call("textDocument/definition", at(&uri, 3, 12));
    assert!(d[0]["uri"].as_str().unwrap().ends_with("src/t/other.ksnl"), "{}", d);
    let md = c.call("textDocument/hover", at(&uri, 3, 6))["contents"]["value"].as_str().unwrap().to_string();
    assert!(md.contains("モジュール `t.other`"), "{}", md);
    // 別のモジュールの型 (`t.other.Pair` の `Pair` の上、行 6)。
    let md = c.call("textDocument/hover", at(&uri, 6, 25))["contents"]["value"].as_str().unwrap().to_string();
    assert!(md.contains("type t.other.Pair pub"), "{}", md);
    let d = c.call("textDocument/definition", at(&uri, 6, 25));
    assert!(d[0]["uri"].as_str().unwrap().ends_with("src/t/other.ksnl"), "{}", d);
    // 何もない所 (行 2 の先頭の空白) は null。
    assert_eq!(c.call("textDocument/hover", at(&uri, 2, 0)), Value::Null);
    c.shutdown();
}

/// 受け手の型で解決するメソッド呼び出し (G55)。同じ名前 `area` が 2 つの型 (`Sq` / `Ci`) にあり、
/// 総称の受け手 (境界 `T: t.Area` の `v.area()`)、閉包の中の `q.area()`、標準のメソッド `v.push` も並べる。
/// 指定の行・列は整形後の文書 (`ksn fmt` の出力) に合わせる。インデントは `concat!` で明示する (`\` の継続は空白を消すため)。
const METHODS: &str = concat!(
    "trait t.Area pub\n",
    "    fn area(self: &Self) -> u32 uses [] panics never\n",
    "end t.Area\n",
    "\n",
    "type t.Ci pub\n",
    "    r: u32\n",
    "end t.Ci\n",
    "\n",
    "type t.Sq pub\n",
    "    side: u32\n",
    "end t.Sq\n",
    "\n",
    "impl t.Area for t.Ci\n",
    "fn t.Ci.area(self: &t.Ci) -> u32 uses [] panics never impls t.Area\n",
    "    self.r\n",
    "end t.Ci.area\n",
    "\n",
    "impl t.Area for t.Sq\n",
    "fn t.Sq.area(self: &t.Sq) -> u32 uses [] panics never impls t.Area\n",
    "    self.side\n",
    "end t.Sq.area\n",
    "\n",
    "fn t.main() -> u32 uses [alloc] panics never\n",
    "    let s = t.Sq { side: 2u32 }\n",
    "    let c = t.Ci { r: 3u32 }\n",
    "    let mut v: alloc.vec.Vec<u32> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n",
    "    core.mem.drop(v.push(1u32))\n",
    "    let f = fn(q: &t.Sq) -> u32 capture [] {\n",
    "        q.area()\n",
    "    }\n",
    "    s.area().wrapping_add(c.area()).wrapping_add(t.total(&s)).wrapping_add(f(&s))\n",
    "end t.main\n",
    "\n",
    "fn t.total[T: t.Area](v: &T) -> u32 uses [] panics never\n",
    "    v.area()\n",
    "end t.total\n",
);

/// hover の本文 (無ければ空)。
fn hover_md(c: &mut Client, uri: &str, line: u32, ch: u32) -> String {
    c.call("textDocument/hover", at(uri, line, ch))["contents"]["value"].as_str().unwrap_or("").to_string()
}

#[test]
fn hover_definition_method_calls_resolve_by_receiver_type() {
    let t = Tmp::new("method_nav", METHODS);
    let uri = t.uri("src/t.ksnl");
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&uri, METHODS);
    // 同じ名前の 2 つのメソッド: `s.area()` は `Sq` の、`c.area()` は `Ci` の定義 (行 31 = 0 始まり 30)。
    let md = hover_md(&mut c, &uri, 30, 7);
    assert!(md.contains("fn t.Sq.area(self: &t.Sq) -> u32"), "{}", md);
    let d = c.call("textDocument/definition", at(&uri, 30, 7));
    assert_eq!(d[0]["range"]["start"], json!({"line": 18, "character": 3}), "{}", d);
    let md = hover_md(&mut c, &uri, 30, 29);
    assert!(md.contains("fn t.Ci.area(self: &t.Ci) -> u32"), "{}", md);
    let d = c.call("textDocument/definition", at(&uri, 30, 29));
    assert_eq!(d[0]["range"]["start"], json!({"line": 13, "character": 3}), "{}", d);
    // 閉包の中の `q.area()` (`q: &t.Sq`)。
    let md = hover_md(&mut c, &uri, 28, 11);
    assert!(md.contains("fn t.Sq.area"), "{}", md);
    // 総称の受け手 (境界 `T: t.Area`) の `v.area()`: 境界が宣言するメソッド (トレイトの宣言)。
    let md = hover_md(&mut c, &uri, 34, 6);
    assert!(md.contains("fn area(self: &Self) -> u32"), "{}", md);
    // 標準のメソッド `v.push` (`alloc.vec.Vec` の定義)。
    let md = hover_md(&mut c, &uri, 26, 21);
    assert!(md.contains("push"), "{}", md);
    let d = c.call("textDocument/definition", at(&uri, 26, 21));
    assert!(d.as_array().map_or(false, |a| !a.is_empty()), "{}", d);
    c.shutdown();
}

/// `.ksnh` の文書コメントと、メソッド呼び出しの位置 (G55)。`.ksnl` を開くと、位置は `.ksnh` の行に直して引く。
#[test]
fn method_call_hover_through_ksnh_sibling_keeps_doc_and_lines() {
    let src = METHODS.replace("fn t.Sq.area", "/// 正方形の面積。\nfn t.Sq.area");
    let t = Tmp::new("method_ksnh", &src);
    let ksnl = fs::read_to_string(t.file("src/t.ksnl")).unwrap();
    assert!(!ksnl.contains("///") && ksnl.contains("fn t.Sq.area"));
    let mut c = Client::start();
    c.initialize(&t.0);
    // `.ksnl` を開く: `s.area()` は ksnl の 31 行目 (0 始まり 30)。ksnh では 1 行多い。
    let uri = t.uri("src/t.ksnl");
    c.open(&uri, &ksnl);
    let md = hover_md(&mut c, &uri, 30, 7);
    assert!(md.contains("fn t.Sq.area(self: &t.Sq) -> u32") && md.contains("正方形の面積。"), "{}", md);
    let d = c.call("textDocument/definition", at(&uri, 30, 7));
    assert!(d[0]["uri"].as_str().unwrap().ends_with("src/t.ksnl"), "{}", d);
    assert_eq!(d[0]["range"]["start"], json!({"line": 18, "character": 3}), "{}", d);
    c.shutdown();
    // `.ksnh` を開く: 同じ呼び出しは ksnh の 32 行目 (0 始まり 31)。定義は `.ksnh` の中。
    let mut c = Client::start();
    c.initialize(&t.0);
    let uri = t.uri("src/t.ksnh");
    c.open(&uri, &src);
    let md = hover_md(&mut c, &uri, 31, 7);
    assert!(md.contains("fn t.Sq.area(self: &t.Sq) -> u32") && md.contains("正方形の面積。"), "{}", md);
    let d = c.call("textDocument/definition", at(&uri, 31, 7));
    assert!(d[0]["uri"].as_str().unwrap().ends_with("src/t.ksnh"), "{}", d);
    assert_eq!(d[0]["range"]["start"], json!({"line": 19, "character": 3}), "{}", d);
    c.shutdown();
}

/// `dyn` トレイトの受け手 (`Box<dyn T>` / `&dyn T`) のメソッド呼び出しは、同名のメソッドが
/// 複数のトレイトにあっても、受け手のトレイトの宣言を解決先にする (G55 の残り)。
/// 2 つのトレイト `t.A` / `t.B` が同じメソッド名 `m` を持ち、`Box<dyn t.A>` と `&dyn t.B`
/// の受け手がそれぞれ自分のトレイトの宣言を解決する。
const DYN_METHODS: &str = concat!(
    "trait t.A pub\n",
    "    fn m(self: &Self) -> u32 uses [] panics never\n",
    "end t.A\n",
    "\n",
    "trait t.B pub\n",
    "    fn m(self: &Self) -> u32 uses [] panics never\n",
    "end t.B\n",
    "\n",
    "type t.X pub\n",
    "    v: u32\n",
    "end t.X\n",
    "\n",
    "type t.Y pub\n",
    "    v: u32\n",
    "end t.Y\n",
    "\n",
    "impl t.A for t.X\n",
    "fn t.X.m(self: &t.X) -> u32 uses [] panics never impls t.A\n",
    "    self.v\n",
    "end t.X.m\n",
    "\n",
    "impl t.B for t.Y\n",
    "fn t.Y.m(self: &t.Y) -> u32 uses [] panics never impls t.B\n",
    "    self.v\n",
    "end t.Y.m\n",
    "\n",
    "fn t.use_box(b: alloc.boxed.Box<dyn t.A + 'static, std.alloc.System>) -> u32 uses [alloc] panics never\n",
    "    b.m()\n",
    "end t.use_box\n",
    "\n",
    "fn t.use_ref(r: &dyn t.B + 'static) -> u32 uses [] panics never\n",
    "    r.m()\n",
    "end t.use_ref\n",
    "\n",
    "fn t.main() -> u32 uses [] panics never\n",
    "    0u32\n",
    "end t.main\n",
);

/// `Box<dyn t.A>` の `b.m()` は `t.A.m` の宣言を、`&dyn t.B` の `r.m()` は `t.B.m` の宣言を解決する。
#[test]
fn hover_definition_dyn_method_calls_resolve_to_trait() {
    let t = Tmp::new("dyn_method_nav", DYN_METHODS);
    let uri = t.uri("src/t.ksnl");
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&uri, DYN_METHODS);
    // `Box<dyn t.A>` の `b.m()` (行 27 = 0 始まり 27、m は文字 6)。
    let md = hover_md(&mut c, &uri, 27, 6);
    assert!(md.contains("fn m(self: &Self) -> u32 uses [] panics never"), "{}", md);
    let d = c.call("textDocument/definition", at(&uri, 27, 6));
    let loc = &d.as_array().unwrap()[0];
    assert_eq!(loc["range"]["start"], json!({"line": 1, "character": 7}), "{}", d);
    // `&dyn t.B` の `r.m()` (行 31 = 0 始まり 31、m は文字 6)。
    let md = hover_md(&mut c, &uri, 31, 6);
    assert!(md.contains("fn m(self: &Self) -> u32 uses [] panics never"), "{}", md);
    let d = c.call("textDocument/definition", at(&uri, 31, 6));
    let loc = &d.as_array().unwrap()[0];
    assert_eq!(loc["range"]["start"], json!({"line": 5, "character": 7}), "{}", d);
    c.shutdown();
}

const SRC_SEM: &str = "\
fn t.add(a: u32, b: u32) -> u32 uses [] panics never pub
    a.wrapping_add(b)
end t.add

fn t.main() -> u32 uses [io.console] panics never pub
    let x: u32 = t.add(1u32, 2u32)
    x.wrapping_add(3u32)
end t.main
";

/// 意味トークンの data を (行, 桁, 長さ, 種類名, 修飾) に戻す。
fn decode_tokens(res: &Value) -> Vec<(u32, u32, u32, String, u32)> {
    let legend = ["namespace", "type", "enum", "enumMember", "interface", "function", "method", "parameter", "variable", "property", "keyword", "macro", "number", "typeParameter"];
    let d: Vec<u64> = res["data"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    let (mut l, mut c) = (0u32, 0u32);
    d.chunks(5)
        .map(|k| {
            l += k[0] as u32;
            c = if k[0] == 0 { c + k[1] as u32 } else { k[1] as u32 };
            (l, c, k[2] as u32, legend[k[3] as usize].to_string(), k[4] as u32)
        })
        .collect()
}

fn has_tok(toks: &[(u32, u32, u32, String, u32)], l: u32, c: u32, len: u32, ty: &str) -> bool {
    toks.iter().any(|t| t.0 == l && t.1 == c && t.2 == len && t.3 == ty)
}

#[test]
fn semantic_tokens_and_legend() {
    let t = Tmp::new("sem", SRC_SEM);
    let uri = t.uri("src/t.ksnl");
    let mut c = Client::start();
    let init = c.initialize(&t.0);
    let legend = &init["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"];
    assert_eq!(legend[5], "function");
    c.open(&uri, SRC_SEM);
    let r = c.call("textDocument/semanticTokens/full", json!({"textDocument": tdoc(&uri)}));
    let toks = decode_tokens(&r);
    // fn t.add: モジュール t は namespace、add は宣言の function。
    assert!(has_tok(&toks, 0, 0, 2, "keyword"), "{:?}", toks);
    assert!(has_tok(&toks, 0, 3, 1, "namespace"), "{:?}", toks);
    assert!(toks.iter().any(|x| x.0 == 0 && x.1 == 5 && x.3 == "function" && x.4 & 1 == 1), "{:?}", toks);
    // 引数 a の型 u32 は type、`a.wrapping_add(b)` の a は parameter・wrapping_add は method・b は parameter。
    assert!(has_tok(&toks, 1, 4, 1, "parameter"), "{:?}", toks);
    assert!(has_tok(&toks, 1, 6, 12, "method"), "{:?}", toks);
    assert!(has_tok(&toks, 1, 19, 1, "parameter"), "{:?}", toks);
    // uses [io.console]: 効果。
    assert!(has_tok(&toks, 4, 49 - 11, 2, "macro") || toks.iter().any(|x| x.0 == 4 && x.3 == "macro"), "{:?}", toks);
    // 局所 x、整数の接尾辞。
    assert!(has_tok(&toks, 6, 4, 1, "variable"), "{:?}", toks);
    assert!(toks.iter().any(|x| x.0 == 5 && x.3 == "number" && x.2 == 1), "{:?}", toks);
    assert!(toks.iter().any(|x| x.0 == 5 && x.3 == "type" && x.2 == 3), "{:?}", toks);
    assert!(toks.iter().any(|x| x.0 == 5 && x.3 == "function"), "{:?}", toks);
    c.shutdown();
}

#[test]
fn semantic_tokens_on_ksnh() {
    let src = "// 説明\n".to_string() + SRC_SEM;
    let t = Tmp::new("semh", &src);
    let uri = t.uri("src/t.ksnh");
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&uri, &src);
    let r = c.call("textDocument/semanticTokens/full", json!({"textDocument": tdoc(&uri)}));
    let toks = decode_tokens(&r);
    // コメント行の分だけ 1 行ずれる。
    assert!(has_tok(&toks, 2, 6, 12, "method"), "{:?}", toks);
    assert!(has_tok(&toks, 7, 4, 1, "variable"), "{:?}", toks);
    c.shutdown();
}

fn labels(r: &Value) -> Vec<String> {
    r.as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_string()).collect()
}

#[test]
fn completion_receiver_effects_and_locals() {
    let t = Tmp::new("comp", SRC_SEM);
    let uri = t.uri("src/t.ksnl");
    let mut c = Client::start();
    let init = c.initialize(&t.0);
    assert!(init["capabilities"]["completionProvider"].is_object());
    c.open(&uri, SRC_SEM);
    // `a.` (引数 u32) の後ろ。
    let r = c.call("textDocument/completion", at(&uri, 1, 6));
    let ls = labels(&r);
    assert!(ls.contains(&"wrapping_add".to_string()), "{:?}", ls);
    let mut sorted = ls.clone();
    sorted.sort();
    assert_eq!(ls, sorted);
    let wa = r.as_array().unwrap().iter().find(|i| i["label"] == "wrapping_add").unwrap();
    assert_eq!(wa["kind"], 2);
    assert!(wa["detail"].as_str().unwrap().contains("wrapping_add"), "{}", wa);
    // 局所 `x.` (let の型は u32)。
    let r = c.call("textDocument/completion", at(&uri, 6, 6));
    assert!(labels(&r).contains(&"wrapping_add".to_string()));
    // `uses [` の中: 効果名。
    let r = c.call("textDocument/completion", at(&uri, 4, 31));
    let ls = labels(&r);
    assert!(ls.contains(&"io.console".to_string()) && ls.contains(&"alloc".to_string()), "{:?}", ls);
    // 識別子: 引数と局所。
    let r = c.call("textDocument/completion", at(&uri, 6, 4));
    let ls = labels(&r);
    assert!(ls.contains(&"x".to_string()), "{:?}", ls);
    // モジュールの中身: `t.` で add と main。
    let r = c.call("textDocument/completion", at(&uri, 5, 19));
    assert_eq!(labels(&r), vec!["add", "main"]);
    c.shutdown();
}

#[test]
fn completion_on_ksnh_maps_lines() {
    let src = "// 説明\n".to_string() + SRC_SEM;
    let t = Tmp::new("comph", &src);
    let uri = t.uri("src/t.ksnh");
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&uri, &src);
    let r = c.call("textDocument/completion", at(&uri, 7, 6));
    assert!(labels(&r).contains(&"wrapping_add".to_string()), "{:?}", r);
    c.shutdown();
}
