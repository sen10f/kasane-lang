//! `ksn lsp` の統合テスト。実際に子プロセスを起動し、`Content-Length` 付きのメッセージをパイプで流して応答を確かめる。
//! 応答待ちにはすべてタイムアウトがあり、サーバが固まってもテストは失敗で終わる。

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
        fs::write(d.join("src").join("t.ksnl"), src).unwrap();
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
        let _ = fs::remove_dir_all(&self.0);
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
    let uri = t.uri("src/t.ksnl");
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
    assert_eq!(fs::read_to_string(t.file("src/t.ksnl")).unwrap(), semi);
    c.shutdown();
}

#[test]
fn code_action_applies_the_fix() {
    let t = Tmp::new("fix", SRC_BAD);
    let uri = t.uri("src/t.ksnl");
    let mut c = Client::start();
    c.initialize(&t.0);
    c.open(&uri, SRC_BAD);
    c.diagnostics_until(&uri, |d| !d.is_empty());
    let acts = c.call(
        "textDocument/codeAction",
        json!({"textDocument": tdoc(&uri), "range": {"start": {"line": 7, "character": 0}, "end": {"line": 7, "character": 0}}, "context": {"diagnostics": []}}),
    );
    let a = acts.as_array().unwrap().iter().find(|a| a["title"].as_str().unwrap().contains("E1108")).expect("quickfix for E1108");
    assert_eq!(a["kind"], "quickfix");
    let fixed = apply(SRC_BAD, &a["edit"]["changes"][&uri]);
    assert!(fixed.contains("\"あ😀\".len().wrapping_add(1usize)"), "{}", fixed);
    // 範囲外の行には出ない。
    let none = c.call(
        "textDocument/codeAction",
        json!({"textDocument": tdoc(&uri), "range": {"start": {"line": 0, "character": 0}, "end": {"line": 1, "character": 0}}, "context": {"diagnostics": []}}),
    );
    assert!(none.as_array().unwrap().is_empty());
    c.shutdown();
}

#[test]
fn hover_definition_references() {
    let t = Tmp::new("nav", SRC_GOOD);
    let uri = t.uri("src/t.ksnl");
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
    let uri = t.uri("src/t.ksnl");
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
    let uri = t.uri("src/t.ksnl");
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
    let lone = std::env::temp_dir().join(format!("ksn_lsp_lone_{}", std::process::id()));
    fs::create_dir_all(&lone).unwrap();
    let lone = fs::canonicalize(lone).unwrap();
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
