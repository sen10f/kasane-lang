//! `ksn debug` / `ksn test --debug` / `ksn dap` の統合テスト。実際に子プロセスを起動する。
//! 待つ処理にはすべてタイムアウトがあり、デバッガが固まってもテストは失敗で終わる。

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";
const WAIT: Duration = Duration::from_secs(30);

const SRC: &str = "\
type t.P derive [Debug, Clone] pub
    x: i32
    y: i32
end t.P

fn t.add(a: i32, b: i32) -> i32 uses [] panics never pub
    let s = a.wrapping_add(b)
    s
end t.add

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let p = t.P { x: 3i32, y: 4i32 }
    let w = t.work(p)
    std.io.print_line(\"hello\")?
    let mut i = 0i32
    while i < 3i32 {
        i = i.wrapping_add(1i32)
    }
    core.check.eq(w, 14i32)
    Ok(())
end t.main

fn t.noisy() -> Result<(), std.io.Error> uses [alloc, io.console] panics may pub
    std.io.print_line(\"noisy\")
end t.noisy

fn t.work(p: t.P) -> i32 uses [] panics never pub
    let sum = t.add(p.x, p.y)
    let twice = sum.wrapping_mul(2i32)
    twice
end t.work

test t.add.test_basic
    core.check.eq(t.add(1i32, 2i32), 3i32)
end t.add.test_basic
";

const SRC_PANIC: &str = "\
fn t.boom(n: i32) -> i32 uses [] panics may pub
    let m = n.wrapping_add(1i32)
    core.panic.panic(\"boom\")
    m
end t.boom

fn t.main() -> () uses [] panics may
    let k = t.boom(41i32)
    core.check.eq(k, 0i32)
end t.main
";

fn line_of(src: &str, needle: &str) -> usize {
    src.lines().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("no line with {}", needle)) + 1
}

struct Pkg(PathBuf);
impl Pkg {
    fn new(name: &str, src: &str) -> Pkg {
        let d = std::env::temp_dir().join(format!("ksn_dbg_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), TOML).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(d.join("src").join("t.ksnl"), src).unwrap();
        Pkg(fs::canonicalize(d).unwrap())
    }
}
impl Drop for Pkg {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 標準出力の各行を読む子プロセスの入れ物。期限を過ぎたら殺して失敗にする。
struct Proc {
    child: Child,
    rx: Receiver<String>,
}

impl Proc {
    fn spawn(pkg: &Pkg, args: &[&str]) -> Proc {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ksn"))
            .args(args)
            .current_dir(&pkg.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ksn");
        let out = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        Proc { child, rx }
    }
    fn send(&mut self, s: &str) {
        let i = self.child.stdin.as_mut().unwrap();
        i.write_all(s.as_bytes()).unwrap();
        i.flush().unwrap();
    }
    fn recv(&mut self) -> String {
        match self.rx.recv_timeout(WAIT) {
            Ok(l) => l,
            Err(RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                panic!("timed out waiting for the debugger output");
            }
            Err(RecvTimeoutError::Disconnected) => panic!("the debugger closed its output"),
        }
    }
    fn finish(mut self) -> (Vec<String>, i32) {
        drop(self.child.stdin.take());
        let mut lines = vec![];
        let end = Instant::now() + WAIT;
        loop {
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(l) => lines.push(l),
                Err(RecvTimeoutError::Timeout) => {
                    if Instant::now() > end {
                        let _ = self.child.kill();
                        panic!("timed out; output so far: {:?}", lines);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        let code = self.child.wait().unwrap().code().unwrap_or(-1);
        (lines, code)
    }
}

/// `ksn debug --json` にコマンドをまとめて流し、出てきた事件を返す。
fn session(pkg: &Pkg, extra: &[&str], cmds: &[&str]) -> (Vec<Value>, i32) {
    let mut args = vec!["debug", "--json"];
    args.extend_from_slice(extra);
    let mut p = Proc::spawn(pkg, &args);
    p.send(&(cmds.join("\n") + "\n"));
    let (lines, code) = p.finish();
    let evs = lines.iter().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON: {} ({})", l, e))).collect();
    (evs, code)
}

fn of<'a>(evs: &'a [Value], name: &str) -> Vec<&'a Value> {
    evs.iter().filter(|e| e["event"] == name).collect()
}

fn stops(evs: &[Value]) -> Vec<&Value> {
    of(evs, "stopped")
}

#[test]
fn breakpoint_by_line_with_stack_and_locals() {
    let pkg = Pkg::new("line", SRC);
    let l = line_of(SRC, "let s = a.wrapping_add(b)");
    let (evs, code) = session(&pkg, &[], &[&format!("break src/t.ksnl:{}", l), "run", "bt", "locals", "locals 1", "continue"]);
    assert_eq!(code, 0, "{:?}", evs);
    let bp = of(&evs, "breakpoint");
    assert_eq!(bp[0]["verified"], true);
    assert_eq!(bp[0]["line"], l);
    let s = stops(&evs);
    assert_eq!(s.len(), 1, "{:?}", evs);
    assert_eq!(s[0]["reason"], "breakpoint");
    assert_eq!(s[0]["frame"]["function"], "t.add");
    assert_eq!(s[0]["frame"]["line"], l);
    assert_eq!(s[0]["frame"]["file"], "src/t.ksnl");
    assert_eq!(s[0]["frame"]["source"], "let s = a.wrapping_add(b)");
    // 呼び出し列: t.add <- t.work <- t.main (行は各フレームの現在の文)
    let st = of(&evs, "stack")[0];
    let names: Vec<&str> = st["frames"].as_array().unwrap().iter().map(|f| f["function"].as_str().unwrap()).collect();
    assert_eq!(names, ["t.add", "t.work", "t.main"]);
    assert_eq!(st["frames"][1]["line"], line_of(SRC, "let sum = t.add"));
    assert_eq!(st["frames"][2]["line"], line_of(SRC, "let w = t.work(p)"));
    // ローカル変数: 引数 a, b
    let lo = of(&evs, "locals");
    let vars: Vec<(String, String)> =
        lo[0]["vars"].as_array().unwrap().iter().map(|v| (v["name"].as_str().unwrap().into(), v["value"].as_str().unwrap().into())).collect();
    assert_eq!(vars, [("a".into(), "3".into()), ("b".into(), "4".into())]);
    // 1 つ外側のフレーム t.work の p (構造体の値)
    let vars1 = lo[1]["vars"].as_array().unwrap();
    assert!(vars1.iter().any(|v| v["name"] == "p" && v["value"] == "P { x: 3, y: 4 }"), "{:?}", vars1);
    assert_eq!(of(&evs, "exited")[0]["code"], 0);
}

#[test]
fn breakpoint_by_function_and_output_event() {
    let pkg = Pkg::new("func", SRC);
    let (evs, _) = session(&pkg, &[], &["break t.work", "run", "continue"]);
    let s = stops(&evs);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0]["reason"], "function breakpoint");
    assert_eq!(s[0]["frame"]["function"], "t.work");
    assert_eq!(s[0]["frame"]["line"], line_of(SRC, "let sum = t.add"));
    // プログラムの標準出力は output イベントになる
    let out: String = of(&evs, "output").iter().map(|e| e["text"].as_str().unwrap().to_string()).collect();
    assert_eq!(out, "hello\n");
    let kinds: Vec<&str> = evs.iter().map(|e| e["event"].as_str().unwrap()).collect();
    assert_eq!(kinds.last(), Some(&"exited"));
    // `fn` 付きの書き方も同じ
    let (evs2, _) = session(&pkg, &[], &["break fn t.work", "run", "quit"]);
    assert_eq!(stops(&evs2)[0]["reason"], "function breakpoint");
}

#[test]
fn conditional_breakpoint() {
    let pkg = Pkg::new("cond", SRC);
    let (evs, code) = session(&pkg, &[], &["break t.add if a == 99i32", "run"]);
    assert!(stops(&evs).is_empty(), "{:?}", evs);
    assert_eq!(code, 0);
    let (evs, _) = session(&pkg, &[], &["break t.add if b == 4i32 && a < 10i32", "run", "print a", "quit"]);
    assert_eq!(stops(&evs).len(), 1);
    assert_eq!(of(&evs, "value")[0]["value"], "3");
    // 条件に副作用のある式は設定時に拒否される
    let (evs, _) = session(&pkg, &[], &["break t.add if t.noisy()", "run"]);
    let bp = of(&evs, "breakpoint")[0];
    assert_eq!(bp["verified"], false);
    assert!(bp["message"].as_str().unwrap().contains("not allowed") || bp["message"].as_str().unwrap().contains("syntax"), "{:?}", bp);
}

#[test]
fn step_in_next_out_and_finish() {
    let pkg = Pkg::new("steps", SRC);
    let l_sum = line_of(SRC, "let sum = t.add");
    let l_twice = line_of(SRC, "let twice");
    let l_s = line_of(SRC, "let s = a.wrapping_add(b)");
    let l_ret = l_s + 1;
    // next: 呼び出しの中に入らず次の文へ
    let (evs, _) = session(&pkg, &[], &["break t.work", "run", "next", "next", "quit"]);
    let lines: Vec<u64> = stops(&evs).iter().map(|s| s["frame"]["line"].as_u64().unwrap()).collect();
    assert_eq!(lines[..2], [l_sum as u64, l_twice as u64], "{:?}", lines);
    assert_eq!(stops(&evs)[1]["reason"], "step");
    // step: 呼び出しの中へ入る。呼び出し列が 1 つ深くなる
    let (evs, _) = session(&pkg, &[], &["break t.work", "run", "step", "bt", "step", "step", "step", "quit"]);
    let s = stops(&evs);
    assert_eq!(s[1]["frame"]["function"], "t.add");
    assert_eq!(s[1]["frame"]["line"], l_s);
    assert_eq!(of(&evs, "stack")[0]["frames"].as_array().unwrap().len(), 3);
    assert_eq!(s[2]["frame"]["line"], l_ret);
    // t.add の最後の文から step すると、呼び出し元に戻って次の文に止まる
    assert_eq!(s[3]["frame"]["function"], "t.work");
    assert_eq!(s[3]["frame"]["line"], l_twice);
    // finish: 戻り値つきで呼び出し元に止まる
    let (evs, _) = session(&pkg, &[], &["break t.add", "run", "finish", "locals", "quit"]);
    let s = stops(&evs);
    assert_eq!(s[1]["frame"]["function"], "t.work");
    assert_eq!(s[1]["message"], "returned 7");
    // 実行前の step は最初の文で止まる
    let (evs, _) = session(&pkg, &[], &["step", "quit"]);
    assert_eq!(stops(&evs)[0]["reason"], "entry");
    assert_eq!(stops(&evs)[0]["frame"]["function"], "t.main");
}

#[test]
fn print_field_and_arithmetic_and_rejections() {
    let pkg = Pkg::new("print", SRC);
    let (evs, _) = session(
        &pkg,
        &[],
        &[
            "break t.work",
            "run",
            "print p.x",
            "print p.x + p.y * 2i32",
            "print p",
            "print t.add(p.x, 10i32)",
            "print p.x / 0i32",
            "print t.noisy()",
            "print p.x = 5i32",
            "print nothing_here",
            "print p.x",
            "quit",
        ],
    );
    let vals: Vec<(String, String)> =
        of(&evs, "value").iter().map(|v| (v["expr"].as_str().unwrap().into(), v["value"].as_str().unwrap().into())).collect();
    assert_eq!(vals[0], ("p.x".into(), "3".into()));
    assert_eq!(vals[1].1, "11");
    assert_eq!(vals[2].1, "P { x: 3, y: 4 }");
    assert_eq!(vals[3].1, "13", "pure functions may be called");
    // 失敗する演算は評価エラーで、プログラムは panic しない
    let errs: Vec<String> = of(&evs, "error").iter().map(|e| e["message"].as_str().unwrap().to_string()).collect();
    assert_eq!(errs.len(), 4, "{:?}", errs);
    assert!(errs[1].contains("t.noisy") && errs[1].contains("not allowed"), "{}", errs[1]);
    assert!(errs[2].contains("syntax") || errs[2].contains("not allowed"), "{}", errs[2]);
    // 拒否のあとも状態は変わらず、出力 ("noisy") も出ていない
    assert_eq!(vals.last().unwrap().1, "3");
    assert!(of(&evs, "output").iter().all(|o| !o.to_string().contains("noisy")));
    assert!(stops(&evs).iter().all(|s| s["reason"] != "panic"));
}

#[test]
fn stop_on_panic_shows_frame_then_exits_101() {
    let pkg = Pkg::new("panic", SRC_PANIC);
    let (evs, code) = session(&pkg, &[], &["run", "bt", "locals", "print n", "continue"]);
    let s = stops(&evs);
    assert_eq!(s.len(), 1, "{:?}", evs);
    assert_eq!(s[0]["reason"], "panic");
    assert_eq!(s[0]["message"], "boom");
    assert_eq!(s[0]["frame"]["function"], "t.boom");
    assert_eq!(s[0]["frame"]["line"], line_of(SRC_PANIC, "core.panic.panic"));
    let names: Vec<&str> = of(&evs, "stack")[0]["frames"].as_array().unwrap().iter().map(|f| f["function"].as_str().unwrap()).collect();
    assert_eq!(names, ["t.boom", "t.main"]);
    assert_eq!(of(&evs, "value")[0]["value"], "41");
    assert_eq!(code, 101);
    assert_eq!(of(&evs, "exited")[0]["code"], 101);
    let err: String = of(&evs, "output").iter().map(|e| e["text"].as_str().unwrap().to_string()).collect();
    assert!(err.contains("panic: boom"), "{}", err);
}

#[test]
fn debug_one_test() {
    let pkg = Pkg::new("test", SRC);
    let mut p = Proc::spawn(&pkg, &["test", "--debug", "--json", "t.add.test_basic"]);
    p.send("break t.add\nrun\nbt\ncontinue\n");
    let (lines, code) = p.finish();
    let evs: Vec<Value> = lines.iter().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(code, 0, "{:?}", lines);
    assert_eq!(of(&evs, "ready")[0]["kind"], "test");
    let names: Vec<&str> = of(&evs, "stack")[0]["frames"].as_array().unwrap().iter().map(|f| f["function"].as_str().unwrap()).collect();
    assert_eq!(names, ["t.add", "t.add.test_basic"]);
    // 存在しないテストは分かりやすく失敗する
    let mut p = Proc::spawn(&pkg, &["test", "--debug", "t.nothing"]);
    p.send("quit\n");
    let (_, code) = p.finish();
    assert_eq!(code, 2);
}

#[test]
fn json_events_are_one_object_per_line() {
    let pkg = Pkg::new("json", SRC);
    let (evs, _) = session(&pkg, &[], &["break t.add", "breakpoints", "run", "delete 1", "continue"]);
    let kinds: Vec<&str> = evs.iter().map(|e| e["event"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["ready", "breakpoint", "breakpoints", "stopped", "deleted", "output", "exited"]);
    let s = stops(&evs)[0];
    for k in ["reason", "message", "breakpoints", "thread", "frame"] {
        assert!(s.get(k).is_some(), "missing {}", k);
    }
    for k in ["index", "function", "file", "line", "column", "source"] {
        assert!(s["frame"].get(k).is_some(), "missing frame.{}", k);
    }
    // 不正なコマンドは error イベントで、セッションは続く
    let (evs, _) = session(&pkg, &[], &["bogus", "bt", "run"]);
    assert_eq!(of(&evs, "error").len(), 2);
    assert_eq!(of(&evs, "exited").len(), 1);
}

// ------------------------------------------------------------ DAP

struct Dap {
    p: Proc,
    seq: i64,
    msgs: Vec<Value>,
}

impl Dap {
    fn start(pkg: &Pkg) -> Dap {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ksn"))
            .arg("dap")
            .current_dir(&pkg.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ksn dap");
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let (tx, rx) = channel();
        std::thread::spawn(move || loop {
            let mut len = None;
            loop {
                let mut l = String::new();
                if out.read_line(&mut l).unwrap_or(0) == 0 {
                    return;
                }
                let l = l.trim_end();
                if l.is_empty() {
                    break;
                }
                if let Some(v) = l.strip_prefix("Content-Length:") {
                    len = v.trim().parse::<usize>().ok();
                }
            }
            let mut buf = vec![0u8; len.unwrap()];
            if out.read_exact(&mut buf).is_err() {
                return;
            }
            if tx.send(String::from_utf8(buf).unwrap()).is_err() {
                return;
            }
        });
        Dap { p: Proc { child, rx }, seq: 0, msgs: vec![] }
    }
    fn send(&mut self, command: &str, args: Value) -> i64 {
        self.seq += 1;
        let body = json!({"seq": self.seq, "type": "request", "command": command, "arguments": args}).to_string();
        self.p.send(&format!("Content-Length: {}\r\n\r\n{}", body.len(), body));
        self.seq
    }
    fn next_msg(&mut self) -> Value {
        serde_json::from_str(&self.p.recv()).unwrap()
    }
    /// 応答が来るまで読む。途中のイベントは `msgs` に溜める。
    fn request(&mut self, command: &str, args: Value) -> Value {
        let seq = self.send(command, args);
        loop {
            let m = self.next_msg();
            if m["type"] == "response" && m["request_seq"] == seq {
                assert_eq!(m["command"], command);
                return m;
            }
            self.msgs.push(m);
        }
    }
    fn event(&mut self, name: &str) -> Value {
        if let Some(i) = self.msgs.iter().position(|m| m["type"] == "event" && m["event"] == name) {
            return self.msgs.remove(i);
        }
        loop {
            let m = self.next_msg();
            if m["type"] == "event" && m["event"] == name {
                return m;
            }
            self.msgs.push(m);
        }
    }
}

#[test]
fn dap_session() {
    let pkg = Pkg::new("dap", SRC);
    let file = pkg.0.join("src").join("t.ksnl").to_string_lossy().to_string();
    let l_s = line_of(SRC, "let s = a.wrapping_add(b)");
    let mut d = Dap::start(&pkg);
    let init = d.request("initialize", json!({"adapterID": "ksn", "linesStartAt1": true, "columnsStartAt1": true}));
    assert_eq!(init["success"], true);
    assert_eq!(init["body"]["supportsConfigurationDoneRequest"], true);
    assert_eq!(init["body"]["supportsFunctionBreakpoints"], true);
    let l = d.request("launch", json!({"cwd": pkg.0.to_string_lossy()}));
    assert_eq!(l["success"], true, "{}", l);
    d.event("initialized");
    let bp = d.request("setBreakpoints", json!({"source": {"path": file}, "breakpoints": [{"line": l_s}]}));
    assert_eq!(bp["body"]["breakpoints"][0]["verified"], true);
    assert_eq!(bp["body"]["breakpoints"][0]["line"], l_s);
    let fb = d.request("setFunctionBreakpoints", json!({"breakpoints": [{"name": "t.work", "condition": "p.x == 99i32"}]}));
    assert_eq!(fb["body"]["breakpoints"][0]["verified"], true);
    d.request("configurationDone", json!({}));
    let st = d.event("stopped");
    assert_eq!(st["body"]["reason"], "breakpoint");
    assert_eq!(st["body"]["threadId"], 1);
    assert_eq!(st["body"]["hitBreakpointIds"][0], bp["body"]["breakpoints"][0]["id"]);
    let th = d.request("threads", json!({}));
    assert_eq!(th["body"]["threads"][0]["id"], 1);
    let stack = d.request("stackTrace", json!({"threadId": 1}));
    let frames = stack["body"]["stackFrames"].as_array().unwrap();
    let names: Vec<&str> = frames.iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["t.add", "t.work", "t.main"]);
    assert_eq!(frames[0]["line"], l_s);
    assert_eq!(frames[0]["column"], 5);
    assert_eq!(frames[0]["source"]["path"], file.as_str());
    let scopes = d.request("scopes", json!({"frameId": frames[0]["id"]}));
    let vref = scopes["body"]["scopes"][0]["variablesReference"].clone();
    let vars = d.request("variables", json!({"variablesReference": vref}));
    let vs: Vec<(&str, &str)> =
        vars["body"]["variables"].as_array().unwrap().iter().map(|v| (v["name"].as_str().unwrap(), v["value"].as_str().unwrap())).collect();
    assert_eq!(vs, [("a", "3"), ("b", "4")]);
    // 構造体は展開できる (t.work の p)
    let scopes1 = d.request("scopes", json!({"frameId": frames[1]["id"]}));
    let v1 = d.request("variables", json!({"variablesReference": scopes1["body"]["scopes"][0]["variablesReference"]}));
    let p = &v1["body"]["variables"][0];
    assert_eq!(p["name"], "p");
    assert!(p["variablesReference"].as_i64().unwrap() > 0);
    let kids = d.request("variables", json!({"variablesReference": p["variablesReference"]}));
    assert_eq!(kids["body"]["variables"][0]["name"], "x");
    assert_eq!(kids["body"]["variables"][0]["value"], "3");
    // evaluate: 式は評価できるが、副作用のあるものは拒否される
    let ev = d.request("evaluate", json!({"expression": "a + b", "frameId": frames[0]["id"], "context": "repl"}));
    assert_eq!(ev["body"]["result"], "7");
    let bad = d.request("evaluate", json!({"expression": "t.noisy()", "frameId": frames[0]["id"]}));
    assert_eq!(bad["success"], false);
    // next で 1 行進む
    d.request("next", json!({"threadId": 1}));
    let st2 = d.event("stopped");
    assert_eq!(st2["body"]["reason"], "step");
    let stack2 = d.request("stackTrace", json!({"threadId": 1}));
    assert_eq!(stack2["body"]["stackFrames"][0]["line"], l_s + 1);
    // continue でプログラムの終わりまで。出力イベントと exited / terminated が届く
    d.request("continue", json!({"threadId": 1}));
    let ex = d.event("exited");
    assert_eq!(ex["body"]["exitCode"], 0);
    d.event("terminated");
    let out: String = d
        .msgs
        .iter()
        .filter(|m| m["event"] == "output" && m["body"]["category"] == "stdout")
        .map(|m| m["body"]["output"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(out, "hello\n");
    d.request("disconnect", json!({}));
    let (_, code) = d.p.finish();
    assert_eq!(code, 0);
}

#[test]
fn dap_zero_based_lines_and_panic_stop() {
    let pkg = Pkg::new("dap0", SRC_PANIC);
    let file = pkg.0.join("src").join("t.ksnl").to_string_lossy().to_string();
    let l_m = line_of(SRC_PANIC, "let m = n.wrapping_add");
    let mut d = Dap::start(&pkg);
    d.request("initialize", json!({"linesStartAt1": false, "columnsStartAt1": false}));
    d.request("launch", json!({"cwd": pkg.0.to_string_lossy()}));
    d.event("initialized");
    let bp = d.request("setBreakpoints", json!({"source": {"path": file}, "breakpoints": [{"line": l_m - 1}]}));
    assert_eq!(bp["body"]["breakpoints"][0]["line"], l_m - 1);
    d.request("configurationDone", json!({}));
    assert_eq!(d.event("stopped")["body"]["reason"], "breakpoint");
    let stack = d.request("stackTrace", json!({"threadId": 1}));
    assert_eq!(stack["body"]["stackFrames"][0]["line"], l_m - 1);
    assert_eq!(stack["body"]["stackFrames"][0]["column"], 4);
    d.request("continue", json!({"threadId": 1}));
    // panic で止まる (reason は exception、メッセージは text)
    let st = d.event("stopped");
    assert_eq!(st["body"]["reason"], "exception");
    assert_eq!(st["body"]["text"], "boom");
    d.request("continue", json!({"threadId": 1}));
    assert_eq!(d.event("exited")["body"]["exitCode"], 101);
    d.event("terminated");
    d.request("disconnect", json!({}));
    let _ = d.p.finish();
}

#[test]
fn dap_launch_failure_is_reported() {
    let pkg = Pkg::new("dapbad", "fn t.main() -> () uses [] panics never\n    nothing_here()\nend t.main\n");
    let mut d = Dap::start(&pkg);
    d.request("initialize", json!({}));
    let l = d.request("launch", json!({"cwd": pkg.0.to_string_lossy()}));
    assert_eq!(l["success"], false);
    assert!(!l["message"].as_str().unwrap().is_empty());
    d.request("disconnect", json!({}));
    let _ = d.p.finish();
}
