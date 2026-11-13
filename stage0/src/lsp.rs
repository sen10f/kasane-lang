//! `ksn lsp`: Language Server Protocol サーバ (stdin/stdout、JSON-RPC 2.0、`Content-Length` 区切り)。
//!
//! CLI が計算するものの薄い層で、検査 (`cli::analyze`)・`get`/`sig` の定義検索・`refs`・`rename`・`fmt` の
//! 下回りをそのまま呼ぶ。未保存のバッファは `project::overlay_set` の上書きとしてローダーに渡すので、
//! ディスクに保存しなくても検査される。
//!
//! 位置: Kasane の診断は 1 始まりの行・列 (列は Unicode スカラ値) だが、LSP は 0 始まりで列は UTF-16 コード単位。
//! 変換は `utf16_of` / `scalar_of` / `byte_pos` に集めてある。
//!
//! 標準出力にはプロトコルのメッセージしか書かない (ログは標準エラー)。
//! パッケージごとに 1 つの作業ディレクトリ (cwd = パッケージ直下) を前提とする CLI の関数を呼ぶため、
//! 処理の前に `set_current_dir` する。サーバは単一スレッドなので競合しない。

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use serde_json::{json, Value};

use crate::cli::{self, Analysis, Found, Loaded};
use crate::program::{Def, Program, Target};
use crate::project::{norm, overlay_set, Source};

type Resp = Result<Value, (i64, String)>;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const SERVER_NOT_INITIALIZED: i64 = -32002;
const REQUEST_FAILED: i64 = -32803;

/// 1 通のメッセージ上限。これを超える本文は読み捨てる。
const MAX_BODY: usize = 256 * 1024 * 1024;

// ------------------------------------------------------------ 位置の変換
/// 行 `line` の文字位置 (スカラ値、0 始まり) を UTF-16 コード単位の位置にする。
fn utf16_of(line: &str, scalar: usize) -> usize {
    line.chars().take(scalar).map(|c| c.len_utf16()).sum()
}

/// UTF-16 コード単位の位置を文字位置 (スカラ値) にする。行末を超えたら行末。
fn scalar_of(line: &str, utf16: usize) -> usize {
    let mut u = 0;
    for (i, c) in line.chars().enumerate() {
        if u >= utf16 {
            return i;
        }
        u += c.len_utf16();
    }
    line.chars().count()
}

/// `text` を `new` にする、行単位の差分による最小の TextEdit の列。
fn line_diff_edits(text: &str, new: &str) -> Vec<Value> {
    let diff = similar::TextDiff::from_lines(text, new);
    let mut starts = vec![0usize];
    for (i, c) in text.char_indices() {
        if c == '\n' {
            starts.push(i + 1);
        }
    }
    let off = |i: usize| starts.get(i).copied().filter(|_| i < starts.len() && starts[i] <= text.len()).unwrap_or(text.len());
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
    let mut edits = vec![];
    for op in diff.ops() {
        let (tag, o, n) = op.as_tag_tuple();
        if matches!(tag, similar::DiffTag::Equal) {
            continue;
        }
        let (a, b) = (off(o.start), off(o.end));
        let repl: String = new_lines[n.start..n.end].concat();
        edits.push(json!({"range": {"start": byte_pos(text, a), "end": byte_pos(text, b)}, "newText": repl}));
    }
    edits
}

fn lines_of(text: &str) -> Vec<&str> {
    text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect()
}

fn pos(line: usize, character: usize) -> Value {
    json!({"line": line, "character": character})
}

fn range(l0: usize, c0: usize, l1: usize, c1: usize) -> Value {
    json!({"start": pos(l0, c0), "end": pos(l1, c1)})
}

/// 同じ行の範囲 (スカラ値の列を UTF-16 に直す)。
fn line_range(lines: &[&str], line: usize, s: usize, e: usize) -> Value {
    let l = lines.get(line).copied().unwrap_or("");
    range(line, utf16_of(l, s), line, utf16_of(l, e))
}

/// バイト位置から LSP の位置 (行, UTF-16 の列) を求める。
fn byte_pos(text: &str, byte: usize) -> Value {
    let head = &text[..byte];
    let line = head.matches('\n').count();
    let start = head.rfind('\n').map_or(0, |i| i + 1);
    pos(line, head[start..].encode_utf16().count())
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 診断の範囲: 開始は報告された位置、終わりは (識別子ならその語の終わり、そうでなければ 1 文字)。
fn diag_range(lines: &[&str], line1: usize, col1: usize) -> Value {
    let li = line1.saturating_sub(1).min(lines.len().saturating_sub(1));
    let l = lines.get(li).copied().unwrap_or("");
    let cs: Vec<char> = l.chars().collect();
    let s = col1.saturating_sub(1).min(cs.len());
    let mut e = s;
    if s < cs.len() {
        if is_ident(cs[s]) {
            while e < cs.len() && (is_ident(cs[e]) || cs[e] == '.') {
                e += 1;
            }
            while e > s + 1 && cs[e - 1] == '.' {
                e -= 1;
            }
        } else {
            e = s + 1;
        }
    }
    line_range(lines, li, s, e)
}

// ------------------------------------------------------------ URI とパス
fn path_to_uri(path: &str) -> String {
    let mut out = String::from("file://");
    if !path.starts_with('/') {
        out.push('/');
    }
    for b in path.replace('\\', "/").bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn uri_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out: Vec<u8> = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    let mut p = String::from_utf8_lossy(&out).to_string();
    // file:///C:/x -> C:/x
    let b = p.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[2] == b':' && b[1].is_ascii_alphabetic() {
        p.remove(0);
    }
    Some(p)
}

/// 存在するファイルは正規化、存在しなければ親を正規化して名前を付ける。
fn canon(path: &str) -> String {
    let p = Path::new(path);
    let c = std::fs::canonicalize(p).or_else(|_| match (p.parent(), p.file_name()) {
        (Some(d), Some(n)) => std::fs::canonicalize(d).map(|d| d.join(n)),
        _ => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
    });
    let s = match c {
        Ok(c) => norm(&c),
        Err(_) => norm(p),
    };
    s.trim_start_matches("//?/").to_string()
}

/// `start` (ファイルならその親) から上へ辿って最初の `kasane.toml` のあるディレクトリ。
fn find_root(start: &str) -> Option<PathBuf> {
    let mut d = Some(Path::new(start));
    if Path::new(start).is_file() || !Path::new(start).is_dir() {
        d = Path::new(start).parent();
    }
    while let Some(x) = d {
        if x.join("kasane.toml").exists() {
            return Some(x.to_path_buf());
        }
        d = x.parent();
    }
    None
}

// ------------------------------------------------------------ 入出力
enum Inbound {
    Body(Vec<u8>),
    /// ヘッダが壊れていた (Content-Length が無い・数値でない・大きすぎる)。
    BadFrame(String),
    Eof,
}

/// 標準入力のフレームを読んで送るスレッド。
fn reader(tx: mpsc::Sender<Inbound>) {
    let stdin = std::io::stdin();
    let mut r = std::io::BufReader::new(stdin.lock());
    loop {
        let mut len: Option<usize> = None;
        let mut bad: Option<String> = None;
        let mut got_any = false;
        loop {
            let mut h = String::new();
            match r.read_line(&mut h) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(Inbound::Eof);
                    return;
                }
                Ok(_) => {}
            }
            let h = h.trim_end_matches(['\r', '\n']);
            if h.is_empty() {
                if got_any {
                    break;
                }
                continue;
            }
            got_any = true;
            if let Some((k, v)) = h.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    match v.trim().parse::<usize>() {
                        Ok(n) => len = Some(n),
                        Err(_) => bad = Some(format!("bad Content-Length: {}", v.trim())),
                    }
                }
            }
        }
        match (len, bad) {
            (_, Some(m)) => {
                let _ = tx.send(Inbound::BadFrame(m));
            }
            (None, None) => {
                let _ = tx.send(Inbound::BadFrame("missing Content-Length".into()));
            }
            (Some(n), None) => {
                if n > MAX_BODY {
                    // 読み捨てて次のフレームに進む。
                    let _ = std::io::copy(&mut r.by_ref().take(n as u64), &mut std::io::sink());
                    let _ = tx.send(Inbound::BadFrame(format!("message too large: {} bytes", n)));
                    continue;
                }
                let mut buf = vec![0u8; n];
                if r.read_exact(&mut buf).is_err() {
                    let _ = tx.send(Inbound::Eof);
                    return;
                }
                let _ = tx.send(Inbound::Body(buf));
            }
        }
    }
}

fn send(v: &Value) {
    let body = v.to_string();
    let out = std::io::stdout();
    let mut o = out.lock();
    let _ = write!(o, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = o.flush();
}

fn notify(method: &str, params: Value) {
    send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
}

fn log(msg: &str) {
    eprintln!("ksn lsp: {}", msg);
}

fn log_message(kind: u8, msg: &str) {
    log(msg);
    notify("window/logMessage", json!({"type": kind, "message": msg}));
}

// ------------------------------------------------------------ サーバの状態
struct Doc {
    path: String,
    text: String,
}

#[derive(Default)]
struct RootState {
    items: Vec<cli::Item>,
    loaded: Option<Loaded>,
    prog: Option<Program>,
    analyzed: bool,
    dirty: bool,
    /// 診断を出した URI (次回、無くなったものを空で消す)。
    published: HashSet<String>,
}

struct Server {
    docs: HashMap<String, Doc>,
    roots: HashMap<PathBuf, RootState>,
    /// kasane.toml が見つからない文書の URI (診断を 1 件出している)。
    rootless: HashSet<String>,
    initialized: bool,
    shutdown: bool,
    /// クライアントが WorkspaceEdit のファイル操作 (rename) を扱えるか (initialize の `capabilities` から)。
    can_rename_files: bool,
}

/// 位置から見つけた記号。
struct Located {
    root: PathBuf,
    fqns: Vec<String>,
    line: usize,
    seg_start: usize,
    seg_end: usize,
    tok_start: usize,
}

fn param_uri(p: &Value) -> Result<String, (i64, String)> {
    p.pointer("/textDocument/uri").and_then(|u| u.as_str()).map(String::from).ok_or((INVALID_PARAMS, "textDocument.uri is required".into()))
}

fn param_pos(p: &Value) -> Result<(usize, usize), (i64, String)> {
    let l = p.pointer("/position/line").and_then(|x| x.as_u64());
    let c = p.pointer("/position/character").and_then(|x| x.as_u64());
    match (l, c) {
        (Some(l), Some(c)) => Ok((l as usize, c as usize)),
        _ => Err((INVALID_PARAMS, "position is required".into())),
    }
}

/// カーソル位置の `a.b.c` 形のトークン: (カーソルまでのセグメント列, セグメント位置, トークン開始, 手前に '.' があるか)。
fn token_at(line: &str, idx: usize) -> Option<(Vec<String>, usize, usize, usize, bool)> {
    let cs: Vec<char> = line.chars().collect();
    let tok = |c: char| is_ident(c) || c == '.';
    let i = if idx < cs.len() && tok(cs[idx]) {
        idx
    } else if idx > 0 && idx <= cs.len() && tok(cs[idx - 1]) {
        idx - 1
    } else {
        return None;
    };
    let (mut a, mut b) = (i, i + 1);
    while a > 0 && tok(cs[a - 1]) {
        a -= 1;
    }
    while b < cs.len() && tok(cs[b]) {
        b += 1;
    }
    while a < b && cs[a] == '.' {
        a += 1;
    }
    while b > a && cs[b - 1] == '.' {
        b -= 1;
    }
    if i < a || i >= b || cs[i] == '.' {
        return None;
    }
    let mut ss = i;
    while ss > a && cs[ss - 1] != '.' {
        ss -= 1;
    }
    let mut se = i;
    while se < b && cs[se] != '.' {
        se += 1;
    }
    let prefix: String = cs[a..se].iter().collect();
    let segs: Vec<String> = prefix.split('.').map(String::from).collect();
    let dot_before = ss > 0 && cs[ss - 1] == '.';
    Some((segs, ss, se, a, dot_before))
}

fn kind_ok(kw: &str) -> bool {
    matches!(kw, "fn" | "type" | "enum" | "trait" | "test" | "const" | "static" | "extern")
}

fn doc_comment(s: &Source, d: &Def) -> String {
    let start = cli::def_start(s, d);
    let hdr = d.line.saturating_sub(1);
    s.lines[start..hdr.min(s.lines.len())]
        .iter()
        .filter_map(|l| l.strip_prefix("///"))
        .map(|l| l.strip_prefix(' ').unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n")
}

impl Server {
    fn new() -> Server {
        Server { docs: HashMap::new(), roots: HashMap::new(), rootless: HashSet::new(), initialized: false, shutdown: false, can_rename_files: false }
    }

    // ---------------------------------------------------- 検査と診断の配信
    fn enter(root: &Path) {
        let _ = std::env::set_current_dir(root);
    }

    fn ensure_fresh(&mut self, root: &Path) {
        let need = self.roots.get(root).map_or(true, |s| s.dirty || !s.analyzed);
        if need {
            self.run_check(root);
        }
    }

    fn flush_dirty(&mut self) {
        let dirty: Vec<PathBuf> = self.roots.iter().filter(|(_, s)| s.dirty).map(|(r, _)| r.clone()).collect();
        for r in dirty {
            self.run_check(&r);
        }
    }

    fn run_check(&mut self, root: &Path) {
        Self::enter(root);
        let res = catch_unwind(AssertUnwindSafe(|| cli::analyze(root)));
        let st = self.roots.entry(root.to_path_buf()).or_default();
        st.dirty = false;
        st.analyzed = true;
        match res {
            Ok(Analysis { items, loaded }) => {
                st.items = items;
                let (l, p) = match loaded {
                    Some((l, p)) => (Some(l), p),
                    None => (None, None),
                };
                st.loaded = l;
                st.prog = p;
            }
            Err(_) => {
                log("internal error while checking; diagnostics skipped");
                st.items = vec![];
                st.loaded = None;
                st.prog = None;
            }
        }
        self.publish(root);
    }

    /// 現在の診断をファイルごとに配信する。診断の無くなったファイルは空で消す。
    fn publish(&mut self, root: &Path) {
        let Some(st) = self.roots.get_mut(root) else { return };
        let mut by_file: HashMap<String, Vec<&cli::Item>> = HashMap::new();
        for it in &st.items {
            let p = match &it.file {
                Some(f) => norm(&root.join(f)),
                None => norm(&root.join("kasane.toml")),
            };
            by_file.entry(p).or_default().push(it);
        }
        // 開いている文書は診断が無くても空を送る。
        for d in self.docs.values() {
            if d.path.starts_with(&format!("{}/", norm(root))) {
                by_file.entry(d.path.clone()).or_default();
            }
        }
        let mut now: HashSet<String> = HashSet::new();
        let mut files: Vec<_> = by_file.into_iter().collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, items) in files {
            let text = crate::project::read_text(&path).unwrap_or_default();
            let lines = lines_of(&text);
            let diags: Vec<Value> = items
                .iter()
                .map(|it| {
                    json!({
                        "range": diag_range(&lines, it.line, it.col),
                        "severity": if it.fatal { 1 } else { 2 },
                        "code": it.code,
                        "source": "ksn",
                        "message": it.message,
                    })
                })
                .collect();
            let uri = path_to_uri(&path);
            if !diags.is_empty() || self.docs.values().any(|d| d.path == path) || st.published.contains(&uri) {
                notify("textDocument/publishDiagnostics", json!({"uri": uri, "diagnostics": diags}));
            }
            if !items.is_empty() {
                now.insert(uri);
            }
        }
        for old in st.published.difference(&now) {
            if !self.docs.values().any(|d| &path_to_uri(&d.path) == old) {
                notify("textDocument/publishDiagnostics", json!({"uri": old, "diagnostics": []}));
            }
        }
        st.published = now;
    }

    fn doc_changed(&mut self, uri: &str) {
        let Some(d) = self.docs.get(uri) else { return };
        match find_root(&d.path) {
            Some(root) => {
                if self.rootless.remove(uri) {
                    notify("textDocument/publishDiagnostics", json!({"uri": uri, "diagnostics": []}));
                }
                self.roots.entry(root).or_default().dirty = true;
            }
            None => {
                let msg = format!("kasane.toml not found above {}", d.path);
                if self.rootless.insert(uri.to_string()) {
                    log_message(2, &msg);
                }
                notify(
                    "textDocument/publishDiagnostics",
                    json!({"uri": uri, "diagnostics": [{
                        "range": range(0, 0, 0, 0), "severity": 2, "code": "E9001", "source": "ksn",
                        "message": "kasane.toml not found (run inside a Kasane package); ksn cannot check this file"}]}),
                );
            }
        }
    }

    // ---------------------------------------------------- 文書
    fn open(&mut self, p: &Value) {
        let Some(uri) = p.pointer("/textDocument/uri").and_then(|u| u.as_str()) else { return };
        let text = p.pointer("/textDocument/text").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let Some(path) = uri_to_path(uri) else {
            log(&format!("unsupported URI: {}", uri));
            return;
        };
        let path = canon(&path);
        overlay_set(&path, Some(text.clone()));
        self.docs.insert(uri.to_string(), Doc { path, text });
        self.doc_changed(uri);
    }

    fn change(&mut self, p: &Value) {
        let Some(uri) = p.pointer("/textDocument/uri").and_then(|u| u.as_str()) else { return };
        let Some(changes) = p.get("contentChanges").and_then(|c| c.as_array()) else { return };
        let Some(d) = self.docs.get_mut(uri) else { return };
        for c in changes {
            let Some(t) = c.get("text").and_then(|t| t.as_str()) else { continue };
            match c.get("range").filter(|r| r.is_object()) {
                None => d.text = t.to_string(),
                Some(r) => {
                    let g = |p: &str| r.pointer(p).and_then(|x| x.as_u64()).map(|x| x as usize);
                    if let (Some(l0), Some(c0), Some(l1), Some(c1)) = (g("/start/line"), g("/start/character"), g("/end/line"), g("/end/character")) {
                        let a = offset_of(&d.text, l0, c0);
                        let b = offset_of(&d.text, l1, c1).max(a);
                        d.text.replace_range(a..b, t);
                    }
                }
            }
        }
        overlay_set(&d.path, Some(d.text.clone()));
        let uri = uri.to_string();
        self.doc_changed(&uri);
    }

    fn close(&mut self, p: &Value) {
        let Some(uri) = p.pointer("/textDocument/uri").and_then(|u| u.as_str()) else { return };
        if let Some(d) = self.docs.remove(uri) {
            overlay_set(&d.path, None);
            if self.rootless.remove(uri) {
                notify("textDocument/publishDiagnostics", json!({"uri": uri, "diagnostics": []}));
            }
            if let Some(root) = find_root(&d.path) {
                // ディスクの内容で検査し直す。
                self.roots.entry(root).or_default().dirty = true;
            }
        }
    }

    fn save(&mut self, p: &Value) {
        let Some(uri) = p.pointer("/textDocument/uri").and_then(|u| u.as_str()) else { return };
        if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
            if let Some(d) = self.docs.get_mut(uri) {
                d.text = t.to_string();
                overlay_set(&d.path, Some(t.to_string()));
            }
        }
        // 保存で他のファイルも変わりうるので、パッケージ全体を検査し直す。
        let uri = uri.to_string();
        self.doc_changed(&uri);
    }

    fn doc(&self, uri: &str) -> Result<&Doc, (i64, String)> {
        self.docs.get(uri).ok_or((INVALID_PARAMS, format!("document is not open: {}", uri)))
    }

    /// 文書のパッケージ。見つからなければ `None` (呼び出し側は null を返す)。
    fn root_for(&mut self, uri: &str) -> Result<Option<PathBuf>, (i64, String)> {
        let path = self.doc(uri)?.path.clone();
        let Some(root) = find_root(&path) else { return Ok(None) };
        self.ensure_fresh(&root);
        Ok(Some(root))
    }

    fn ctx(&mut self, root: &Path) -> Option<(&Loaded, &Program)> {
        Self::enter(root);
        let st = self.roots.get_mut(root)?;
        let l = st.loaded.as_ref()?;
        if st.prog.is_none() {
            st.prog = Some(cli::build_program(l).0);
        }
        Some((st.loaded.as_ref()?, st.prog.as_ref()?))
    }

    // ---------------------------------------------------- 記号の特定
    fn locate(&mut self, uri: &str, p: &Value) -> Result<Option<Located>, (i64, String)> {
        let (line, ch) = param_pos(p)?;
        let Some(root) = self.root_for(uri)? else { return Ok(None) };
        let d = self.doc(uri)?;
        let (path, text) = (d.path.clone(), d.text.clone());
        let lines = lines_of(&text);
        let Some(ltxt) = lines.get(line) else { return Ok(None) };
        let idx = scalar_of(ltxt, ch);
        // `import a.b` の行は、モジュール `a.b` そのもの (改名・参照の対象)。
        if let Some(m) = ltxt.strip_prefix("import ") {
            let name = m.trim_end();
            let start = "import ".chars().count();
            let end = start + name.chars().count();
            if idx >= start && idx <= end {
                let Some((l, _)) = self.ctx(&root) else { return Ok(None) };
                if l.pkg.sources.iter().any(|s| s.module == name) {
                    return Ok(Some(Located { root, fqns: vec![name.to_string()], line, seg_start: start, seg_end: end, tok_start: start }));
                }
            }
        }
        let Some((segs, ss, se, ts, dot_before)) = token_at(ltxt, idx) else { return Ok(None) };
        let Some((_, prog)) = self.ctx(&root) else { return Ok(None) };
        let mut fqns: Vec<String> = vec![];
        if let Some(r) = prog.resolve(&segs, &path) {
            if r.rest.is_empty() {
                match r.target {
                    Target::Fn(i) | Target::Sig(i) | Target::Const(i) | Target::Type(i) => fqns.push(prog.defs[i].name.clone()),
                    Target::Variant(en, vn) => fqns.push(format!("{}.{}", en, vn)),
                    _ => {}
                }
            }
        }
        if fqns.is_empty() && (segs.len() > 1 || dot_before) {
            // メソッド呼び出し: 名前だけで候補を集める (型は分からない)。
            let last = segs.last().unwrap();
            for d in &prog.defs {
                if d.kw == "fn" && d.is_method() && d.name.rsplit('.').next() == Some(last.as_str()) && !fqns.contains(&d.name) {
                    fqns.push(d.name.clone());
                }
            }
            if fqns.len() > 8 {
                fqns.clear();
            }
        }
        if fqns.is_empty() {
            return Ok(None);
        }
        Ok(Some(Located { root, fqns, line, seg_start: ss, seg_end: se, tok_start: ts }))
    }

    fn sources<'a>(l: &'a Loaded) -> Vec<&'a Source> {
        let mut out: Vec<&Source> = l.pkg.sources.iter().collect();
        for dp in &l.deps {
            out.extend(dp.sources.iter());
        }
        out.extend(l.std.iter());
        out
    }

    /// 定義の位置 (URI, 範囲)。名前はヘッダ行の中で探す。
    fn def_location(s: &Source, line1: usize, name: &str) -> Value {
        let text = crate::project::read_text(&s.path).unwrap_or_else(|| s.text.clone());
        let lines = lines_of(&text);
        let li = line1.saturating_sub(1);
        let l = lines.get(li).copied().unwrap_or("");
        let (a, b) = match l.find(name) {
            Some(bi) => {
                let a = l[..bi].chars().count();
                (a, a + name.chars().count())
            }
            None => (0, l.chars().count()),
        };
        json!({"uri": path_to_uri(&s.path), "range": line_range(&lines, li, a, b)})
    }

    // ---------------------------------------------------- 要求
    fn hover(&mut self, p: &Value) -> Resp {
        let uri = param_uri(p)?;
        let Some(loc) = self.locate(&uri, p)? else { return Ok(Value::Null) };
        if loc.fqns.len() != 1 {
            return Ok(Value::Null);
        }
        let Some((l, _)) = self.ctx(&loc.root) else { return Ok(Value::Null) };
        let srcs = Self::sources(l);
        let (header, doc) = match cli::find_def(&srcs, &loc.fqns[0]) {
            Some(Found::Def(d, s)) => (d.header.clone(), doc_comment(s, d)),
            Some(Found::Line(s, ln)) => (s.lines[ln - 1].trim().to_string(), String::new()),
            None => return Ok(Value::Null),
        };
        let mut md = format!("```ksn\n{}\n```", header);
        if !doc.is_empty() {
            md.push_str("\n\n");
            md.push_str(&doc);
        }
        let text = self.doc(&uri)?.text.clone();
        let lines = lines_of(&text);
        Ok(json!({"contents": {"kind": "markdown", "value": md}, "range": line_range(&lines, loc.line, loc.tok_start, loc.seg_end)}))
    }

    fn definition(&mut self, p: &Value) -> Resp {
        let uri = param_uri(p)?;
        let Some(loc) = self.locate(&uri, p)? else { return Ok(Value::Null) };
        let Some((l, _)) = self.ctx(&loc.root) else { return Ok(Value::Null) };
        let srcs = Self::sources(l);
        let mut out = vec![];
        for f in &loc.fqns {
            let last = f.rsplit('.').next().unwrap_or(f);
            match cli::find_def(&srcs, f) {
                Some(Found::Def(d, s)) => out.push(Self::def_location(s, d.line, &d.name)),
                Some(Found::Line(s, ln)) => out.push(Self::def_location(s, ln, last)),
                None => {}
            }
        }
        Ok(if out.is_empty() { Value::Null } else { Value::Array(out) })
    }

    fn references(&mut self, p: &Value) -> Resp {
        let uri = param_uri(p)?;
        let decl = p.pointer("/context/includeDeclaration").and_then(|b| b.as_bool()).unwrap_or(false);
        let Some(loc) = self.locate(&uri, p)? else { return Ok(Value::Null) };
        if loc.fqns.len() != 1 {
            return Ok(Value::Null);
        }
        let target = loc.fqns[0].clone();
        let root = loc.root.clone();
        Self::enter(&root);
        let Some((l, prog)) = self.ctx(&root) else { return Ok(Value::Null) };
        let _ = prog;
        let sites = crate::refactor::refs(l, &target).map_err(|m| (REQUEST_FAILED, m))?;
        let mut out: Vec<Value> = vec![];
        let mut texts: HashMap<String, String> = HashMap::new();
        for st in sites {
            // `end` 行と、書き換えない言及は参照に含めない。定義そのものは includeDeclaration のときだけ。
            if st.kind == "end" || st.new.is_none() && st.kind.starts_with("doc comment") {
                continue;
            }
            if st.kind == "definition" && !decl {
                continue;
            }
            let path = st.file.clone();
            let text = texts.entry(path.clone()).or_insert_with(|| crate::project::read_text(&path).unwrap_or_default());
            let lines = lines_of(text);
            let li = st.line.saturating_sub(1);
            out.push(json!({"uri": path_to_uri(&path), "range": line_range(&lines, li, st.col - 1, st.end_col - 1)}));
        }
        Ok(Value::Array(out))
    }

    /// 改名できる記号か確かめ、(旧 FQN, 現在位置の範囲) を返す。
    fn rename_target(&mut self, uri: &str, p: &Value) -> Result<(Located, String), (i64, String)> {
        let no = || (REQUEST_FAILED, "ここは改名できません (パッケージ内の定義の名前にカーソルを置いてください)".to_string());
        let Some(loc) = self.locate(uri, p)? else { return Err(no()) };
        if loc.fqns.len() != 1 {
            return Err(no());
        }
        let fqn = loc.fqns[0].clone();
        let Some((l, _)) = self.ctx(&loc.root) else { return Err(no()) };
        // カーソルが FQN の最後のセグメントにあること、定義がこのパッケージのものであること。
        let in_pkg = l.pkg.sources.iter().any(|s| s.module == fqn || s.defs.iter().any(|d| d.name == fqn && kind_ok(&d.kw)));
        if !in_pkg {
            return Err(no());
        }
        Ok((loc, fqn))
    }

    fn prepare_rename(&mut self, p: &Value) -> Resp {
        let uri = param_uri(p)?;
        let (loc, fqn) = self.rename_target(&uri, p)?;
        let text = self.doc(&uri)?.text.clone();
        let lines = lines_of(&text);
        Ok(json!({"range": line_range(&lines, loc.line, loc.seg_start, loc.seg_end),
                  "placeholder": fqn.rsplit('.').next().unwrap_or(&fqn)}))
    }

    /// 改名。`newName` が識別子 1 個なら FQN の最後のセグメントだけを変え、
    /// `a.b.c` のように `.` を含めば新しい FQN として扱う (別モジュールへの移動)。
    /// ファイルの移動を伴うときは WorkspaceEdit の `documentChanges` (rename 操作) で返す。
    fn rename(&mut self, p: &Value) -> Resp {
        let uri = param_uri(p)?;
        let new_name = p.get("newName").and_then(|n| n.as_str()).ok_or((INVALID_PARAMS, "newName is required".to_string()))?;
        let seg_ok = |s: &str| !s.is_empty() && s.chars().all(is_ident) && !s.chars().next().map_or(true, |c| c.is_ascii_digit());
        if !new_name.split('.').all(seg_ok) {
            return Err((REQUEST_FAILED, format!("改名後の名前が不正です: {}", new_name)));
        }
        let (loc, old) = self.rename_target(&uri, p)?;
        let new = if new_name.contains('.') {
            new_name.to_string()
        } else {
            match old.rsplit_once('.') {
                Some((h, _)) => format!("{}.{}", h, new_name),
                None => new_name.to_string(),
            }
        };
        let can_move = self.can_rename_files;
        let root = loc.root.clone();
        let Some((l, _)) = self.ctx(&root) else { return Ok(Value::Null) };
        let plan = crate::refactor::plan_rename(l, &old, &new).map_err(|m| (REQUEST_FAILED, m))?;
        crate::refactor::verify(&root, &plan).map_err(|m| (REQUEST_FAILED, m))?;
        let moves = plan.changes.iter().any(|c| c.old_path.as_deref().map_or(false, |o| o != c.new_path));
        if moves && !can_move {
            return Err((REQUEST_FAILED, "この改名はファイルの移動を伴いますが、クライアントがファイル操作 (WorkspaceEdit の rename) を扱えません。`ksn rename` を使ってください".to_string()));
        }
        let mut doc_changes: Vec<Value> = vec![];
        let mut changes = serde_json::Map::new();
        let mut file_ops: Vec<Value> = vec![];
        let mut manifest_note = false;
        for c in &plan.changes {
            let from = c.old_path.clone().unwrap_or_else(|| c.new_path.clone());
            let edits = line_diff_edits(&c.old_text, &c.new_text);
            if from != c.new_path {
                file_ops.push(json!({"kind": "rename", "oldUri": path_to_uri(&from), "newUri": path_to_uri(&c.new_path)}));
            }
            if !edits.is_empty() {
                doc_changes.push(json!({"textDocument": {"uri": path_to_uri(&from), "version": null}, "edits": edits}));
                changes.insert(path_to_uri(&from), Value::Array(edits));
            }
        }
        if let Some((mp, mo, mn)) = &plan.manifest {
            // kasane.toml の変更 (entry / [unsafe] modules)。
            doc_changes.push(json!({"textDocument": {"uri": path_to_uri(mp), "version": null}, "edits": line_diff_edits(mo, mn)}));
            changes.insert(path_to_uri(mp), Value::Array(line_diff_edits(mo, mn)));
            manifest_note = true;
        }
        let _ = manifest_note;
        if moves {
            doc_changes.extend(file_ops);
            return Ok(json!({"documentChanges": doc_changes}));
        }
        Ok(json!({"changes": changes}))
    }

    fn formatting(&mut self, p: &Value) -> Resp {
        let uri = param_uri(p)?;
        let d = self.doc(&uri)?;
        let (path, text) = (d.path.clone(), d.text.clone());
        if !path.ends_with(".ksnl") {
            return Ok(json!([]));
        }
        let root = find_root(&path);
        let srcdir = root.map(|r| norm(&r.join("src"))).unwrap_or_default();
        let parsed = Source::from_text(&path, &srcdir, text.clone());
        let nt = if parsed.error.is_none() {
            catch_unwind(AssertUnwindSafe(|| crate::fmt::format_source(&text, &path))).map_err(|_| (INTERNAL_ERROR, "formatter failed".to_string()))?
        } else {
            crate::fmt::mechanical_collapse(&text)
        };
        if nt == text {
            return Ok(json!([]));
        }
        let edits = line_diff_edits(&text, &nt);
        Ok(Value::Array(edits))
    }

    fn document_symbol(&mut self, p: &Value) -> Resp {
        let uri = param_uri(p)?;
        let d = self.doc(&uri)?;
        let (path, text) = (d.path.clone(), d.text.clone());
        let srcdir = find_root(&path).map(|r| norm(&r.join("src"))).unwrap_or_default();
        let s = Source::from_text(&path, &srcdir, text.clone());
        let lines = lines_of(&text);
        let out: Vec<Value> = s
            .defs
            .iter()
            .filter(|d| kind_ok(&d.kw))
            .map(|d| {
                let li = d.line.saturating_sub(1);
                let le = (if d.end_line > 0 { d.end_line } else { d.line }).saturating_sub(1);
                let hl = lines.get(li).copied().unwrap_or("");
                let (a, b) = match hl.find(&d.name) {
                    Some(bi) => {
                        let a = hl[..bi].chars().count();
                        (a, a + d.name.chars().count())
                    }
                    None => (0, hl.chars().count()),
                };
                let elen = lines.get(le).map_or(0, |l| l.chars().count());
                json!({
                    "name": d.name,
                    "detail": d.header,
                    "kind": symbol_kind(d),
                    "range": range(li, 0, le, utf16_of(lines.get(le).copied().unwrap_or(""), elen)),
                    "selectionRange": line_range(&lines, li, a, b),
                })
            })
            .collect();
        Ok(Value::Array(out))
    }

    fn workspace_symbol(&mut self, p: &Value) -> Resp {
        let q = p.get("query").and_then(|q| q.as_str()).unwrap_or("").to_lowercase();
        // 開いている文書のパッケージだけを対象にする (未解析なら解析する)。
        let mut roots: Vec<PathBuf> = self.docs.values().filter_map(|d| find_root(&d.path)).collect();
        roots.sort();
        roots.dedup();
        let mut out: Vec<Value> = vec![];
        for r in roots {
            self.ensure_fresh(&r);
            let Some((l, _)) = self.ctx(&r) else { continue };
            for s in &l.pkg.sources {
                for d in s.defs.iter().filter(|d| kind_ok(&d.kw)) {
                    if q.is_empty() || d.name.to_lowercase().contains(&q) {
                        out.push(json!({"name": d.name, "kind": symbol_kind(d), "location": Self::def_location(s, d.line, &d.name)}));
                    }
                }
            }
        }
        out.truncate(1000);
        Ok(Value::Array(out))
    }

    fn code_action(&mut self, p: &Value) -> Resp {
        let uri = param_uri(p)?;
        let Some(root) = self.root_for(&uri)? else { return Ok(json!([])) };
        let path = self.doc(&uri)?.path.clone();
        let g = |q: &str| p.pointer(q).and_then(|x| x.as_u64()).map(|x| x as usize);
        let (r0, r1) = (g("/range/start/line").unwrap_or(0), g("/range/end/line").unwrap_or(usize::MAX));
        let Some(st) = self.roots.get(&root) else { return Ok(json!([])) };
        let mut out = vec![];
        let mut texts: HashMap<String, String> = HashMap::new();
        for it in &st.items {
            let Some(f) = &it.file else { continue };
            if it.fix.is_empty() || norm(&root.join(f)) != path {
                continue;
            }
            let li = it.line.saturating_sub(1);
            if li < r0 || li > r1 {
                continue;
            }
            let mut changes = serde_json::Map::new();
            for fx in &it.fix {
                let fp = norm(&root.join(&fx.file));
                let text = texts.entry(fp.clone()).or_insert_with(|| crate::project::read_text(&fp).unwrap_or_default());
                let lines = lines_of(text);
                let conv = |line: usize, col: usize| {
                    let li = line.saturating_sub(1);
                    pos(li, utf16_of(lines.get(li).copied().unwrap_or(""), col.saturating_sub(1)))
                };
                let e = json!({"range": {"start": conv(fx.line, fx.col), "end": conv(fx.end_line, fx.end_col)}, "newText": fx.replace});
                changes.entry(path_to_uri(&fp)).or_insert_with(|| json!([])).as_array_mut().unwrap().push(e);
            }
            let lines = lines_of(texts.get(&path).map(|s| s.as_str()).unwrap_or(""));
            out.push(json!({
                "title": format!("Fix {}: {}", it.code, it.message),
                "kind": "quickfix",
                "isPreferred": true,
                "diagnostics": [{"range": diag_range(&lines, it.line, it.col), "severity": if it.fatal {1} else {2},
                                 "code": it.code, "source": "ksn", "message": it.message}],
                "edit": {"changes": changes},
            }));
        }
        Ok(Value::Array(out))
    }

    // ---------------------------------------------------- ディスパッチ
    fn initialize(&mut self, p: &Value) -> Resp {
        self.initialized = true;
        let ws = p.pointer("/capabilities/workspace/workspaceEdit");
        let ops = ws.and_then(|w| w.get("resourceOperations")).and_then(|r| r.as_array()).map_or(false, |a| a.iter().any(|x| x == "rename"));
        let dc = ws.and_then(|w| w.get("documentChanges")).and_then(|b| b.as_bool()).unwrap_or(false);
        self.can_rename_files = ops && dc;
        Ok(json!({
            "capabilities": {
                "textDocumentSync": {"openClose": true, "change": 1, "save": {"includeText": false}},
                "hoverProvider": true,
                "definitionProvider": true,
                "referencesProvider": true,
                "renameProvider": {"prepareProvider": true},
                "documentFormattingProvider": true,
                "documentSymbolProvider": true,
                "workspaceSymbolProvider": true,
                "codeActionProvider": {"codeActionKinds": ["quickfix"]},
            },
            "serverInfo": {"name": "ksn", "version": env!("CARGO_PKG_VERSION")},
        }))
    }

    fn request(&mut self, method: &str, p: &Value) -> Resp {
        match method {
            "initialize" => self.initialize(p),
            "shutdown" => {
                self.shutdown = true;
                Ok(Value::Null)
            }
            _ if !self.initialized => Err((SERVER_NOT_INITIALIZED, "server not initialized".into())),
            _ if self.shutdown => Err((INVALID_REQUEST, "server is shutting down".into())),
            "textDocument/hover" => self.hover(p),
            "textDocument/definition" => self.definition(p),
            "textDocument/references" => self.references(p),
            "textDocument/prepareRename" => self.prepare_rename(p),
            "textDocument/rename" => self.rename(p),
            "textDocument/formatting" => self.formatting(p),
            "textDocument/documentSymbol" => self.document_symbol(p),
            "workspace/symbol" => self.workspace_symbol(p),
            "textDocument/codeAction" => self.code_action(p),
            _ => Err((METHOD_NOT_FOUND, format!("method not found: {}", method))),
        }
    }

    fn notification(&mut self, method: &str, p: &Value) {
        if !self.initialized || self.shutdown {
            return;
        }
        match method {
            "textDocument/didOpen" => self.open(p),
            "textDocument/didChange" => self.change(p),
            "textDocument/didSave" => self.save(p),
            "textDocument/didClose" => self.close(p),
            "workspace/didChangeWatchedFiles" => {
                for s in self.roots.values_mut() {
                    s.dirty = true;
                }
            }
            _ => {}
        }
    }

    /// 1 通を処理する。`Some(code)` で終了。
    fn handle(&mut self, raw: &[u8]) -> Option<i32> {
        let v: Value = match serde_json::from_slice(raw) {
            Ok(v) => v,
            Err(e) => {
                send(&json!({"jsonrpc": "2.0", "id": null, "error": {"code": PARSE_ERROR, "message": format!("parse error: {}", e)}}));
                return None;
            }
        };
        let Some(obj) = v.as_object() else {
            send(&json!({"jsonrpc": "2.0", "id": null, "error": {"code": INVALID_REQUEST, "message": "request must be an object"}}));
            return None;
        };
        let id = obj.get("id").cloned();
        let Some(method) = obj.get("method").and_then(|m| m.as_str()) else {
            // クライアントからの応答 (こちらは要求を出さない) は無視。それ以外は不正な要求。
            if id.is_none() || obj.contains_key("result") || obj.contains_key("error") {
                if !(obj.contains_key("result") || obj.contains_key("error")) {
                    log("ignored a message without method");
                }
            } else {
                send(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": INVALID_REQUEST, "message": "missing method"}}));
            }
            return None;
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        match id {
            Some(id) if !id.is_null() => {
                let r = catch_unwind(AssertUnwindSafe(|| self.request(method, &params)))
                    .unwrap_or_else(|_| Err((INTERNAL_ERROR, "internal error".to_string())));
                match r {
                    Ok(res) => send(&json!({"jsonrpc": "2.0", "id": id, "result": res})),
                    Err((code, message)) => send(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})),
                }
                None
            }
            _ => match method {
                "exit" => Some(if self.shutdown { 0 } else { 1 }),
                _ => {
                    if catch_unwind(AssertUnwindSafe(|| self.notification(method, &params))).is_err() {
                        log("internal error while handling a notification");
                    }
                    None
                }
            },
        }
    }
}

/// テキスト中の位置 (行, UTF-16 の列) のバイトオフセット。範囲外は末尾 / 行末。
fn offset_of(text: &str, line: usize, utf16: usize) -> usize {
    let mut start = 0;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return text.len(),
        }
    }
    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let l = &text[start..end];
    let sc = scalar_of(l, utf16);
    start + l.char_indices().nth(sc).map_or(l.len(), |(b, _)| b)
}

fn symbol_kind(d: &Def) -> u32 {
    match d.kw.as_str() {
        "fn" if d.is_method() => 6,
        "fn" | "extern" | "test" => 12,
        "type" => 23,
        "enum" => 10,
        "trait" => 11,
        "const" => 14,
        _ => 13,
    }
}

pub fn run() -> i32 {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || reader(tx));
    let mut srv = Server::new();
    let mut next = rx.recv().ok();
    while let Some(m) = next {
        match m {
            Inbound::Eof => return if srv.shutdown { 0 } else { 1 },
            Inbound::BadFrame(msg) => {
                log(&msg);
                send(&json!({"jsonrpc": "2.0", "id": null, "error": {"code": INVALID_REQUEST, "message": msg}}));
            }
            Inbound::Body(b) => {
                if let Some(code) = srv.handle(&b) {
                    return code;
                }
            }
        }
        // 続きのメッセージが既に届いていれば、検査はまとめて 1 回にする。
        next = match rx.try_recv() {
            Ok(m) => Some(m),
            Err(mpsc::TryRecvError::Empty) => {
                srv.flush_dirty();
                rx.recv().ok()
            }
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
    }
    if srv.shutdown {
        0
    } else {
        1
    }
}
