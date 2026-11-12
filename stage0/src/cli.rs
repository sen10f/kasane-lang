//! `ksn` コマンド (stage0)。Python 参照実装 ksn/cli.py の移植。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use regex::Regex;
use sha2::{Digest, Sha256};

use crate::check::Checker;
use crate::explain::EXPLAIN;
use crate::fmt::{format_source, mechanical_collapse};
use crate::grammar::{py_repr, Grammar};
use crate::interp::Interp;
use crate::lint::Lint;
use crate::natives::NATIVE_NAMES;
use crate::program::{Def, Program};
use crate::project::*;
use crate::value::Ctl;

const VERSION: &str = "0.1.0";

fn lint_code(c: &str) -> &'static str {
    match c {
        "fqn" => "E1001",
        "name" => "E0010",
        "order" => "E0020",
        "attr" => "E0021",
        "blank" => "E0030",
        "indent" => "E0031",
        "pub" => "E1010",
        "unused" => "E1020",
        "impcollide" => "E1111",
        _ => "E0020",
    }
}

// ------------------------------------------------------------ 診断
pub struct Item {
    pub code: String,
    pub fatal: bool,
    pub fqn: Option<String>,
    pub file: Option<String>,
    pub line: usize,
    pub col: usize,
    pub message: String,
    pub fix: Vec<crate::fixes::Fix>,
}

pub struct Report {
    json: bool,
    items: Vec<Item>,
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '\u{8}' => o.push_str("\\b"),
            '\u{c}' => o.push_str("\\f"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn fix_json(fixes: &[crate::fixes::Fix]) -> String {
    let parts: Vec<String> = fixes
        .iter()
        .map(|f| {
            format!(
                "{{\"file\": {}, \"line\": {}, \"col\": {}, \"end_line\": {}, \"end_col\": {}, \"replace\": {}}}",
                json_str(&f.file),
                f.line,
                f.col,
                f.end_line,
                f.end_col,
                json_str(&f.replace)
            )
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

/// 各診断に修正案を付ける (ソースは診断のファイルから読む)。
fn attach_fixes(items: &mut [Item]) {
    let mut cache: HashMap<String, Vec<String>> = HashMap::new();
    for it in items.iter_mut() {
        let Some(file) = it.file.clone() else { continue };
        let lines = cache
            .entry(file.clone())
            .or_insert_with(|| read_text(&file).unwrap_or_default().split(char::from(10u8)).map(String::from).collect());
        it.fix = crate::fixes::fixes_for(&it.code, &it.message, &file, it.line, it.col, lines);
    }
}

/// 定義ヘッダの行に付く診断のうち、メッセージが名前を示すもの (未知の型・トレイト、未使用の引数) は、
/// その名前が行の中で最初に現れる列 (1 始まり、Unicode スカラ値) を返す。
fn header_name_col(msg: &str, line: &str) -> Option<usize> {
    let name = ["unknown type ", "unknown trait ", "unused parameter "].iter().find_map(|p| msg.strip_prefix(p))?;
    let name = name.split_whitespace().next()?;
    let cs: Vec<char> = line.chars().collect();
    let ns: Vec<char> = name.chars().collect();
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    (0..cs.len()).find(|&i| {
        cs[i..].starts_with(&ns)
            && (i == 0 || !(ident(cs[i - 1]) || cs[i - 1] == '.'))
            && cs.get(i + ns.len()).map_or(true, |&c| !ident(c))
    })
    .map(|i| i + 1)
}

impl Report {
    fn new(json: bool) -> Report {
        Report { json, items: vec![] }
    }
    fn add(&mut self, code: &str, msg: &str, file: Option<&str>, line: usize, col: usize, fatal: bool) {
        self.items.push(Item {
            code: code.into(),
            fatal,
            fqn: None,
            file: file.map(String::from),
            line,
            col,
            message: msg.into(),
            fix: vec![],
        });
    }
    /// 列が分からない診断 (col == 0) に、その行の最初の非空白文字の列を補う (行が読めなければ 1)。
    fn fill_cols(&mut self) {
        let mut cache: HashMap<String, Vec<String>> = HashMap::new();
        for it in self.items.iter_mut().filter(|i| i.col == 0) {
            let lines = it.file.as_ref().map(|f| {
                cache
                    .entry(f.clone())
                    .or_insert_with(|| read_text(f).unwrap_or_default().split('\n').map(String::from).collect())
            });
            let text = lines.and_then(|ls| ls.get(it.line.wrapping_sub(1)));
            it.col = text.map_or(1, |l| {
                header_name_col(&it.message, l).unwrap_or_else(|| l.chars().take_while(|c| *c == ' ').count() + 1)
            });
        }
    }
    /// 列の補完と安定した並べ替え (出力はしない)。
    fn finish(&mut self) {
        self.fill_cols();
        self.items.sort_by(|a, b| {
            (a.file.clone().unwrap_or_default(), a.line, a.col, &a.code).cmp(&(b.file.clone().unwrap_or_default(), b.line, b.col, &b.code))
        });
    }
    fn flush(&mut self) {
        self.finish();
        for it in &self.items {
            if self.json {
                println!(
                    "{{\"code\": {}, \"fatal\": {}, \"fqn\": {}, \"file\": {}, \"line\": {}, \"col\": {}, \"message\": {}, \"fix\": {}}}",
                    json_str(&it.code),
                    it.fatal,
                    it.fqn.as_ref().map_or("null".to_string(), |x| json_str(x)),
                    it.file.as_ref().map_or("null".to_string(), |x| json_str(x)),
                    it.line,
                    it.col,
                    json_str(&it.message),
                    fix_json(&it.fix)
                );
            } else {
                let loc = match &it.file {
                    Some(f) => format!("{}:{}:{}", f, it.line, it.col),
                    None => "<project>".to_string(),
                };
                println!("{}: {}{}: {}", loc, it.code, if it.fatal { "" } else { " (non-fatal)" }, it.message);
            }
        }
    }
    fn fatal(&self) -> bool {
        self.items.iter().any(|i| i.fatal)
    }
}

pub fn rel(path: &str) -> String {
    let cwd = norm(&std::env::current_dir().unwrap_or_default());
    let p = path.replace('\\', "/");
    match p.strip_prefix(&format!("{}/", cwd)) {
        Some(r) => r.to_string(),
        None => p,
    }
}

// ------------------------------------------------------------ 読み込み
pub struct Loaded {
    pub pkg: Package,
    pub deps: Vec<Package>,
    pub std: Vec<Source>,
}

pub(crate) fn load_all(root: &Path) -> Result<Loaded, ProjectError> {
    let pkg = load_package(root)?;
    let deps = load_deps(&pkg)?;
    Ok(Loaded { pkg, deps, std: load_stdlib() })
}

fn empty_program() -> Program {
    let mut p = Program::default();
    for n in NATIVE_NAMES {
        p.natives.insert(n.to_string());
    }
    p
}

/// std + 依存 + 本体の順に登録する。返す添字は本体の定義。
pub(crate) fn build_program(l: &Loaded) -> (Program, Vec<usize>) {
    let mut prog = empty_program();
    for s in &l.std {
        for d in &s.defs {
            prog.add_def(d.clone());
        }
    }
    for dp in &l.deps {
        for s in &dp.sources {
            for d in &s.defs {
                prog.add_def(d.clone());
            }
        }
    }
    let mut user = vec![];
    for s in &l.pkg.sources {
        for d in &s.defs {
            user.push(prog.defs.len());
            prog.add_def(d.clone());
        }
    }
    crate::infer::run(&mut prog);
    (prog, user)
}

fn check_lock(pkg: &Package, rep: &mut Report) {
    let deps = pkg.manifest.get("dependencies").and_then(|d| d.as_table()).cloned().unwrap_or_default();
    let lock = pkg.root.join("kasane.lock");
    if !deps.is_empty() && !lock.exists() {
        rep.add("E9020", "kasane.lock is missing; run `ksn update`", Some("kasane.lock"), 1, 1, true);
        return;
    }
    if lock.exists() {
        let v: toml::Value = fs::read_to_string(&lock).unwrap_or_default().parse().unwrap_or(toml::Value::Table(Default::default()));
        let names: Vec<String> = v
            .get("package")
            .and_then(|p| p.as_array())
            .map(|a| a.iter().filter_map(|x| x.get("name").and_then(|n| n.as_str()).map(String::from)).collect())
            .unwrap_or_default();
        let subset = deps.keys().all(|k| names.contains(k));
        if !subset || (deps.is_empty() && !names.is_empty()) {
            rep.add("E9021", "kasane.toml and kasane.lock disagree; run `ksn update`", Some("kasane.lock"), 1, 1, true);
        }
    }
}

/// 構文・正規形・意味の検査。構文エラーが無ければ Program を返す。
fn full_check(root: &Path, rep: &mut Report, run_mode: bool) -> Result<(Loaded, Option<Program>, Vec<usize>), ProjectError> {
    let l = load_all(root)?;
    check_lock(&l.pkg, rep);
    let srcroot = norm(&l.pkg.root.join("src"));
    let grammar = Grammar::new();
    for s in &l.pkg.sources {
        let fl = rel(&s.path);
        if s.path.ends_with(".ksnh") {
            rep.add("E1107", "the .ksnh extension is no longer supported; rename the file to .ksnl", Some(&fl), 1, 1, true);
        }
        // 構文エラーのとき、Rust 風の書き方をまとめて直し方つきで報告する。
        if s.error.is_some() || !grammar.check(&s.text).0 {
            for h in crate::rustisms::scan(&s.text) {
                rep.add(h.code, &h.msg, Some(&fl), h.line, h.col, true);
            }
        }
        if s.text.contains('\r') {
            rep.add("E0001", "CR (CRLF) is not allowed; use LF", Some(&fl), 1, 1, true);
        }
        if let Some(e) = &s.error {
            rep.add(&e.code, &e.msg, Some(&fl), e.line, e.col, true);
            continue;
        }
        for (i, line) in s.lines.iter().enumerate() {
            if line.contains('\t') {
                rep.add("E0001", "tab is not allowed", Some(&fl), i + 1, line.chars().position(|c| c == '\t').map_or(1, |p| p + 1), true);
            }
            if line != line.trim_end_matches(' ') && !line.ends_with('\r') {
                rep.add("E0001", "trailing whitespace", Some(&fl), i + 1, line.trim_end_matches(' ').chars().count() + 1, true);
            }
        }
        if !s.text.ends_with('\n') || s.text.ends_with("\n\n") {
            rep.add("E0001", "file must end with exactly one LF", Some(&fl), s.lines.len(), 1, true);
        }
        let relpath = s.path.strip_prefix(&format!("{}/", srcroot)).unwrap_or(&s.path);
        let lt = s.text.replace("\r\n", "\n");
        // 正規形でなくても、`ksn fmt` で正規形になり、その結果が検査を通るなら、並び順や空行は
        // エラーにしない (`fmt --check` が差分として報告する)。意味に関する誤りは従来どおり報告する。
        let formatted = crate::fmt::format_source(&s.text, &s.path);
        if formatted != s.text && !s.text.contains('\t') {
            let fixable = Lint::new(relpath, &formatted).run().is_empty() && grammar.check(&formatted).0;
            if fixable {
                continue;
            }
        }
        for (code, ln, msg) in Lint::new(relpath, &lt).run() {
            rep.add(lint_code(&code), &msg, Some(&fl), ln, 0, !(run_mode && code == "unused"));
        }
        let (ok, msg, ln) = grammar.check(&s.text);
        if !ok {
            rep.add("E0002", &msg, Some(&fl), ln, 0, true);
        }
    }
    if l.pkg.sources.iter().any(|s| s.error.is_some()) {
        return Ok((l, None, vec![]));
    }
    let (prog, user) = build_program(&l);
    let diags = {
        let mut ck = Checker::new(&prog, user.clone());
        ck.unsafe_modules = crate::project::unsafe_modules(&l.pkg.manifest);
        ck.run();
        ck.diags
    };
    // 同じ呼び出しに対する E1108 は、名前だけの検査と型に基づく検査が両方出しうる。最初の 1 件だけ残す。
    let mut seen_1108: std::collections::HashSet<(String, usize, usize)> = Default::default();
    for d in diags {
        if d.code == "E1108" && !seen_1108.insert((d.file.clone(), d.line, d.col)) {
            continue;
        }
        rep.items.push(Item {
            code: d.code,
            fatal: true,
            fqn: if d.fqn == "-" { None } else { Some(d.fqn) },
            file: Some(rel(&d.file)),
            line: d.line,
            col: d.col,
            message: d.msg,
            fix: vec![],
        });
    }
    Ok((l, Some(prog), user))
}

// ------------------------------------------------------------ コマンド
fn cmd_new(name: &str) -> i32 {
    if !is_pkg_name(name) || RESERVED_NAMES.contains(&name) {
        eprintln!("ksn: invalid package name {}", py_repr(name));
        return 1;
    }
    if Path::new(name).exists() {
        eprintln!("ksn: {} already exists", name);
        return 1;
    }
    fs::create_dir_all(Path::new(name).join("src")).unwrap();
    fs::write(
        Path::new(name).join("kasane.toml"),
        format!("[package]\nname = \"{0}\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"{0}.main\"\n\n[dependencies]\n", name),
    )
    .unwrap();
    fs::write(Path::new(name).join("kasane.lock"), "version = 1\n").unwrap();
    fs::write(
        Path::new(name).join("src").join(format!("{}.ksnl", name)),
        format!(
            "fn {0}.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    std.io.print_line(\"Hello, Kasane!\")\nend {0}.main\n",
            name
        ),
    )
    .unwrap();
    0
}

fn is_pkg_name(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_lowercase()) && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `ksn lsp` 用の検査結果。`check --json` と同じ診断 (修正案つき・並べ替え済み)。
pub struct Analysis {
    pub items: Vec<Item>,
    /// 読み込みに成功したとき: 読み込んだパッケージ一式と、構文エラーが無ければ検査済みの Program。
    pub loaded: Option<(Loaded, Option<Program>)>,
}

/// パッケージ `root` を検査して診断を返す (出力はしない。ソースは `overlay_set` の上書きを優先して読む)。
pub fn analyze(root: &Path) -> Analysis {
    let mut rep = Report::new(false);
    let loaded = match full_check(root, &mut rep, false) {
        Ok((l, p, _)) => Some((l, p)),
        Err(e) => {
            rep.add(&e.code, &e.msg, None, 1, 1, true);
            None
        }
    };
    attach_fixes(&mut rep.items);
    rep.finish();
    Analysis { items: rep.items, loaded }
}

fn cmd_check(json: bool) -> i32 {
    let mut rep = Report::new(json);
    match find_root().and_then(|r| full_check(&r, &mut rep, false).map(|_| ())) {
        Ok(()) => {}
        Err(e) => rep.add(&e.code, &e.msg, None, 1, 1, true),
    }
    attach_fixes(&mut rep.items);
    rep.flush();
    if rep.fatal() { 1 } else { 0 }
}

/// `ksn todo`: `core.panic.todo()` の呼び出し (未実装の穴) を一覧する。
fn cmd_todo() -> i32 {
    let root = match find_root().and_then(|r| load_package(&r)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 2;
        }
    };
    let mut n = 0;
    for s in &root.sources {
        for (i, line) in s.lines.iter().enumerate() {
            if line.contains("core.panic.todo(") {
                println!("{}:{}: {}", rel(&s.path), i + 1, line.trim());
                n += 1;
            }
        }
    }
    println!("{} todo(s)", n);
    0
}

/// `ksn fix`: `check` の修正案をすべて適用し、`fmt` で整える。
fn cmd_fix() -> i32 {
    let mut total = 0usize;
    for _ in 0..5 {
        let mut rep = Report::new(false);
        let root = match find_root() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("ksn: {}: {}", e.code, e.msg);
                return 2;
            }
        };
        if let Err(e) = full_check(&root, &mut rep, false) {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 2;
        }
        attach_fixes(&mut rep.items);
        let mut by_file: HashMap<String, Vec<crate::fixes::Fix>> = HashMap::new();
        for it in &rep.items {
            for f in &it.fix {
                by_file.entry(f.file.clone()).or_default().push(f.clone());
            }
        }
        if by_file.is_empty() {
            break;
        }
        let mut applied = 0;
        for (file, fixes) in by_file {
            let text = fs::read_to_string(&file).unwrap_or_default();
            let (new, n) = crate::fixes::apply(&text, &fixes);
            if n > 0 && new != text {
                fs::write(&file, new).unwrap();
                applied += n;
            }
        }
        if applied == 0 {
            break;
        }
        total += applied;
    }
    println!("applied {} fix(es)", total);
    let code = cmd_fmt(&[]);
    if code != 0 {
        return code;
    }
    cmd_check(false)
}

fn run_args(args: &[String]) -> (bool, bool, Vec<String>) {
    let mut json = false;
    let mut watch = false;
    let mut rest = vec![];
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "--watch" => watch = true,
            _ => rest.push(a.clone()),
        }
    }
    (json, watch, rest)
}

fn cmd_run(args: &[String]) -> i32 {
    let (json, watch, rest) = run_args(args);
    let mut rep = Report::new(json);
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            rep.add(&e.code, &e.msg, None, 1, 1, true);
            rep.flush();
            return 1;
        }
    };
    let (l, prog, _) = match full_check(&root, &mut rep, true) {
        Ok(x) => x,
        Err(e) => {
            rep.add(&e.code, &e.msg, None, 1, 1, true);
            rep.flush();
            return 1;
        }
    };
    rep.flush();
    if rep.fatal() {
        return 1;
    }
    let Some(entry) = l.pkg.entry() else {
        eprintln!("ksn: E9030: this package is a library (no `entry` in kasane.toml)");
        return 1;
    };
    let prog = prog.unwrap();
    let Some(&di) = prog.funcs.get(&entry) else {
        eprintln!("ksn: E9030: entry function {} not found", entry);
        return 1;
    };
    let prog = Rc::new(prog);
    let mut it = Interp::new(prog);
    it.args = rest;
    if watch {
        it.reload = Some(spawn_watcher(&l));
    }
    let child_code = it.run_entry(di);
    child_code
}

/// ホットリロード: 外部面が不変の関数本体の変更だけを反映する。
fn spawn_watcher(l: &Loaded) -> std::sync::mpsc::Receiver<Program> {
    let (tx, rx) = std::sync::mpsc::channel::<Program>();
    let mut std_defs: Vec<Def> = vec![];
    for s in &l.std {
        std_defs.extend(s.defs.clone());
    }
    let mut dep_defs: Vec<Def> = vec![];
    for dp in &l.deps {
        for s in &dp.sources {
            dep_defs.extend(s.defs.clone());
        }
    }
    let srcroot = norm(&l.pkg.root.join("src"));
    struct W {
        path: String,
        lines: Vec<String>,
        defs: Vec<Def>,
        mtime: Option<std::time::SystemTime>,
    }
    let mut ws: Vec<W> = l
        .pkg
        .sources
        .iter()
        .map(|s| W {
            path: s.path.clone(),
            lines: s.lines.clone(),
            defs: s.defs.clone(),
            mtime: fs::metadata(&s.path).and_then(|m| m.modified()).ok(),
        })
        .collect();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut changed = false;
        let mut restart = false;
        for w in ws.iter_mut() {
            let m = fs::metadata(&w.path).and_then(|m| m.modified()).ok();
            if m == w.mtime {
                continue;
            }
            w.mtime = m;
            let new = Source::load(&w.path, &srcroot);
            if let Some(e) = &new.error {
                eprintln!("hot reload: {}: {} (kept old code)", rel(&w.path), e.msg);
                continue;
            }
            let old: HashMap<&str, &Def> = w.defs.iter().filter(|d| d.kw != "import").map(|d| (d.name.as_str(), d)).collect();
            let cur: HashMap<&str, &Def> = new.defs.iter().filter(|d| d.kw != "import").map(|d| (d.name.as_str(), d)).collect();
            let mut same_names: Vec<&&str> = old.keys().collect();
            same_names.sort();
            let mut cur_names: Vec<&&str> = cur.keys().collect();
            cur_names.sort();
            if same_names != cur_names {
                restart = true;
                continue;
            }
            let mut ok = true;
            for (n, d) in &cur {
                let o = old[n];
                if d.kw == "fn" {
                    if d.header != o.header || !d.generics.is_empty() {
                        ok = false;
                    } else if d.body.is_some() {
                        changed = true;
                    }
                } else if matches!(d.kw.as_str(), "type" | "enum" | "trait" | "const" | "static" | "impl") {
                    let a = &new.lines[d.line.min(new.lines.len())..d.end_line.max(d.line).min(new.lines.len())];
                    let b = &w.lines[o.line.min(w.lines.len())..o.end_line.max(o.line).min(w.lines.len())];
                    if d.header != o.header || a != b {
                        ok = false;
                    }
                }
            }
            if !ok {
                restart = true;
                continue;
            }
            w.lines = new.lines.clone();
            w.defs = new.defs.clone();
        }
        if restart {
            eprintln!("hot reload: header/type change, restart required");
        }
        if changed {
            let mut prog = empty_program();
            for d in &std_defs {
                prog.add_def(d.clone());
            }
            for d in &dep_defs {
                prog.add_def(d.clone());
            }
            for w in &ws {
                for d in &w.defs {
                    prog.add_def(d.clone());
                }
            }
            crate::infer::run(&mut prog);
            if tx.send(prog).is_err() {
                return;
            }
        }
    });
    rx
}

fn cmd_test(args: &[String]) -> i32 {
    let mut both = false;
    let mut prefix: Option<String> = None;
    for a in args {
        if a == "--both" {
            both = true;
        } else {
            prefix = Some(a.clone());
        }
    }
    let mut rep = Report::new(false);
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            rep.add(&e.code, &e.msg, None, 1, 1, true);
            rep.flush();
            return 1;
        }
    };
    let (l, prog, user) = match full_check(&root, &mut rep, false) {
        Ok(x) => x,
        Err(e) => {
            rep.add(&e.code, &e.msg, None, 1, 1, true);
            rep.flush();
            return 1;
        }
    };
    if rep.fatal() {
        rep.flush();
        return 1;
    }
    rep.flush();
    let prog = Rc::new(prog.unwrap());
    let mut tests: Vec<usize> = user
        .iter()
        .copied()
        .filter(|&i| prog.defs[i].kw == "test" && prefix.as_ref().map_or(true, |p| prog.defs[i].name.starts_with(p.as_str())))
        .collect();
    tests.sort_by(|&a, &b| prog.defs[a].name.cmp(&prog.defs[b].name));
    let mut failed = 0;
    for ti in &tests {
        let name = prog.defs[*ti].name.clone();
        let mut results: Vec<Option<String>> = vec![];
        let mut it = Interp::new(prog.clone());
        let r = it.call_body(*ti);
        use std::io::Write;
        let _ = it.out.flush();
        results.push(match r {
            Err(Ctl::Panic(m)) => Some(m),
            Err(Ctl::Exit(c)) => Some(format!("exit({})", c)),
            _ => None,
        });
        if both {
            results.push(run_in_bundle(&root, &name, &l));
        }
        if results.len() > 1 && results[0].is_none() != results[1].is_none() {
            failed += 1;
            println!("test {} ... FAILED: interpreter and bundle disagree: {}", name, py_opt_list(&results));
        } else if results.iter().all(|r| r.is_none()) {
            println!("test {} ... ok", name);
        } else {
            failed += 1;
            println!("test {} ... FAILED: {}", name, results.iter().flatten().next().unwrap());
        }
    }
    println!("\n{} passed; {} failed", tests.len() - failed, failed);
    if failed > 0 { 1 } else { 0 }
}

fn py_opt_list(rs: &[Option<String>]) -> String {
    format!(
        "[{}]",
        rs.iter().map(|r| r.as_ref().map_or("None".to_string(), |s| py_repr(s))).collect::<Vec<_>>().join(", ")
    )
}

fn exe_name() -> &'static str {
    if cfg!(windows) { "ksn.exe" } else { "ksn" }
}

fn run_in_bundle(root: &Path, fqn: &str, l: &Loaded) -> Option<String> {
    let bdir = root.join("target").join("debug");
    if !bdir.join(exe_name()).exists() && cmd_build(&[], Some(l)) != 0 {
        return Some("build failed".into());
    }
    let r = std::process::Command::new(bdir.join(exe_name()))
        .args(["_exec", "--test", fqn])
        .env("KSN_STDLIB", bdir.join("stdlib"))
        .current_dir(bdir.join("pkg"))
        .output();
    match r {
        Ok(o) if o.status.success() => None,
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
            Some(if e.is_empty() { format!("exit {}", o.status.code().unwrap_or(-1)) } else { e })
        }
        Err(e) => Some(e.to_string()),
    }
}

// ------------------------------------------------------------ get / sig / refs / rename
struct Everything {
    l: Loaded,
}

impl Everything {
    fn sources(&self) -> Vec<&Source> {
        let mut out: Vec<&Source> = self.l.pkg.sources.iter().collect();
        for dp in &self.l.deps {
            out.extend(dp.sources.iter());
        }
        out.extend(self.l.std.iter());
        out
    }
}

fn load_everything() -> Result<Everything, ProjectError> {
    let root = find_root()?;
    let l = load_all(&root)?;
    Ok(Everything { l })
}

pub(crate) enum Found<'a> {
    Def(&'a Def, &'a Source),
    #[allow(dead_code)]
    Line(&'a Source, usize),
}

pub(crate) fn find_def<'a>(srcs: &[&'a Source], fqn: &str) -> Option<Found<'a>> {
    for s in srcs {
        for d in &s.defs {
            if d.name == fqn && matches!(d.kw.as_str(), "fn" | "type" | "enum" | "trait" | "test" | "const" | "static" | "extern") {
                return Some(Found::Def(d, s));
            }
        }
    }
    let (head, last) = fqn.rsplit_once('.')?;
    for s in srcs {
        for d in &s.defs {
            if d.name == head {
                match d.kw.as_str() {
                    "type" => {
                        if let Some(k) = d.fields.iter().position(|f| f.0 == last) {
                            return Some(Found::Line(s, d.line + 1 + k));
                        }
                    }
                    "enum" => {
                        if let Some(k) = d.variants.iter().position(|f| f.0 == last) {
                            return Some(Found::Line(s, d.line + 1 + k));
                        }
                    }
                    "trait" => {
                        if let Some(k) = d.methods.iter().position(|m| m == last) {
                            return Some(Found::Line(s, d.line + 1 + k));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    None
}

pub(crate) fn def_start(s: &Source, d: &Def) -> usize {
    let mut i = d.line - 1;
    while i > 0 && s.lines[i - 1].starts_with("//") {
        i -= 1;
    }
    i
}

fn derived_impls(d: &Def) -> Option<String> {
    if !matches!(d.kw.as_str(), "type" | "enum") || d.derive.is_empty() {
        return None;
    }
    let n = &d.name;
    let mut out: Vec<String> = vec![];
    for tr in &d.derive {
        match tr.as_str() {
            "Copy" => out.push(format!("impl core.ops.Copy for {}", n)),
            "Eq" => out.push(format!("impl core.cmp.Eq for {}", n)),
            "Clone" => {
                out.push(format!(
                    "impl core.ops.Clone for {0}\nfn {0}.clone(self: &{0}) -> {0} uses [] panics never impls core.ops.Clone",
                    n
                ));
                if d.kw == "type" {
                    out.push(format!(
                        "    {} {{ {} }}",
                        n,
                        d.fields.iter().map(|f| format!("{0}: self.{0}.clone()", f.0)).collect::<Vec<_>>().join(", ")
                    ));
                } else {
                    out.push("    // clone each variant payload".into());
                }
                out.push(format!("end {}.clone", n));
            }
            "PartialEq" => {
                out.push(format!(
                    "impl core.cmp.PartialEq<{0}> for {0}\nfn {0}.eq(self: &{0}, other: &{0}) -> bool uses [] panics never impls core.cmp.PartialEq<{0}>",
                    n
                ));
                if d.kw == "type" {
                    out.push(format!(
                        "    {}",
                        d.fields.iter().map(|f| format!("self.{0} == other.{0}", f.0)).collect::<Vec<_>>().join(" && ")
                    ));
                } else {
                    out.push("    // equal variants with equal payloads".into());
                }
                out.push(format!("end {}.eq", n));
            }
            other => out.push(format!("// {}: built-in derived implementation for {}", other, n)),
        }
        out.push(String::new());
    }
    Some(out.join("\n").trim_end().to_string())
}

fn cmd_get(args: &[String]) -> i32 {
    let derived = args.iter().any(|a| a == "--derived");
    let Some(fqn) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("ksn: usage: ksn get [--derived] <FQN>");
        return 2;
    };
    let ev = match load_everything() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let srcs = ev.sources();
    match find_def(&srcs, fqn) {
        None => {
            eprintln!("ksn: {} not found", fqn);
            1
        }
        Some(Found::Line(s, ln)) => {
            println!("{}", s.lines[ln - 1]);
            0
        }
        Some(Found::Def(d, s)) => {
            if derived {
                match derived_impls(d) {
                    Some(o) => println!("{}", o),
                    None => {
                        eprintln!("ksn: {} has no derive", fqn);
                        return 1;
                    }
                }
                return 0;
            }
            let start = def_start(s, d);
            let end = if d.end_line > 0 { d.end_line } else { d.line };
            println!("{}", s.lines[start..end].join("\n"));
            0
        }
    }
}

/// `ksn context <FQN>`: 定義全体と、そこから参照される定義の署名 (型は全体) を出力する。
/// 小さな文脈で 1 つの定義を読み書きするための表示。
fn cmd_context(args: &[String]) -> i32 {
    let Some(fqn) = args.first() else {
        eprintln!("ksn: usage: ksn context <FQN>");
        return 2;
    };
    let ev = match load_everything() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let srcs = ev.sources();
    let Some(Found::Def(d, s)) = find_def(&srcs, fqn) else {
        eprintln!("ksn: {} not found", fqn);
        return 1;
    };
    let start = def_start(s, d);
    let end = if d.end_line > 0 { d.end_line } else { d.line };
    let text = s.lines[start..end].join("
");
    println!("// target: {}", d.name);
    println!("{}", text);
    let mut refs: Vec<(String, String)> = vec![];
    for s2 in &srcs {
        for d2 in &s2.defs {
            if d2.name == d.name || !matches!(d2.kw.as_str(), "fn" | "type" | "enum" | "trait" | "const" | "static") {
                continue;
            }
            if d2.kw == "fn" && d2.name.contains('.') && !fqn_occurs(&text, &d2.name, false) {
                continue;
            }
            if d2.kw != "fn" && !fqn_occurs(&text, &d2.name, false) {
                continue;
            }
            let shown = if d2.kw == "fn" || d2.kw == "const" || d2.kw == "static" {
                d2.header.clone()
            } else {
                let st = def_start(s2, d2);
                let en = if d2.end_line > 0 { d2.end_line } else { d2.line };
                s2.lines[st..en].join("
")
            };
            refs.push((d2.name.clone(), shown));
        }
    }
    refs.sort();
    refs.dedup();
    if !refs.is_empty() {
        println!();
        println!("// referenced definitions (signatures)");
        for (_, shown) in refs {
            println!("{}", shown);
        }
    }
    0
}

fn cmd_sig(args: &[String]) -> i32 {
    let Some(fqn) = args.first() else {
        eprintln!("ksn: usage: ksn sig <FQN>");
        return 2;
    };
    let ev = match load_everything() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    match find_def(&ev.sources(), fqn) {
        None => {
            eprintln!("ksn: {} not found", fqn);
            1
        }
        Some(Found::Line(s, ln)) => {
            println!("{}", s.lines[ln - 1]);
            0
        }
        Some(Found::Def(d, _)) => {
            println!("{}", d.header);
            0
        }
    }
}

/// `ksn doc [--std] [--private]`: 静的 HTML の API ドキュメントを `target/doc/` に生成する (生成器は doc.rs)。
fn cmd_doc(args: &[String]) -> i32 {
    for a in args {
        if a != "--std" && a != "--private" {
            eprintln!("ksn: usage: ksn doc [--std] [--private]");
            return 2;
        }
    }
    let std_only = args.iter().any(|a| a == "--std");
    let private = args.iter().any(|a| a == "--private");
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let l = match load_all(&root) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let (prog, _) = build_program(&l);
    // build_program は std -> 依存 -> 本体の順に定義を登録する。同じ順で推論済みの定義と対応づける。
    let mut std_items = vec![];
    let mut pkg_items = vec![];
    let mut k = 0;
    for s in &l.std {
        for _ in &s.defs {
            std_items.push(crate::doc::Item { d: &prog.defs[k], lines: &s.lines });
            k += 1;
        }
    }
    for dp in &l.deps {
        for s in &dp.sources {
            k += s.defs.len();
        }
    }
    for s in &l.pkg.sources {
        for _ in &s.defs {
            pkg_items.push(crate::doc::Item { d: &prog.defs[k], lines: &s.lines });
            k += 1;
        }
    }
    let out = root.join("target").join("doc");
    let std_dir = out.join("std");
    let (dir, pages) = if std_only {
        let cfg = crate::doc::Config { title: "標準ライブラリ", is_std: true, private, std_link: None };
        (std_dir, crate::doc::generate(&std_items, &cfg))
    } else {
        let name = l.pkg.name.clone();
        let link = if std_dir.join("index.html").exists() { Some(&std_items[..]) } else { None };
        let cfg = crate::doc::Config { title: &name, is_std: false, private, std_link: link };
        (out, crate::doc::generate(&pkg_items, &cfg))
    };
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("ksn: cannot create {}: {}", norm(&dir), e);
        return 1;
    }
    // 古いページ (消えたモジュール) を残さない。
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            if e.path().extension().map_or(false, |x| x == "html") {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    for (f, html) in &pages {
        if let Err(e) = fs::write(dir.join(f), html) {
            eprintln!("ksn: cannot write {}: {}", f, e);
            return 1;
        }
    }
    println!("{}", norm(&dir.join("index.html")));
    0
}

/// `(?<![\w.])OLD(?![\w])(?!\.[A-Za-z_])` に一致するか。
pub(crate) fn fqn_occurs(text: &str, target: &str, tail_dot_ok: bool) -> bool {
    text.match_indices(target).any(|(i, _)| fqn_boundary(text, i, target.len(), tail_dot_ok))
}
fn fqn_boundary(text: &str, i: usize, len: usize, tail_dot_ok: bool) -> bool {
    let w = |c: char| c.is_alphanumeric() || c == '_';
    let before = text[..i].chars().next_back();
    let rest = &text[i + len..];
    let after = rest.chars().next();
    let mut second = rest.chars();
    second.next();
    before.map_or(true, |c| !(w(c) || c == '.'))
        && after.map_or(true, |c| !w(c))
        && (tail_dot_ok || !(after == Some('.') && second.next().map_or(false, |c| c.is_ascii_alphabetic() || c == '_')))
}

fn cmd_refs(args: &[String]) -> i32 {
    let Some(target) = args.first() else {
        eprintln!("ksn: usage: ksn refs <FQN>");
        return 2;
    };
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let l = match load_all(&root) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    match crate::refactor::refs(&l, target) {
        Ok(sites) => {
            for s in sites {
                println!("{}:{}:{}: {} [{}]", rel(&s.file), s.line, s.col, s.owner, s.kind);
            }
            0
        }
        Err(m) => {
            eprintln!("ksn: {}", m);
            1
        }
    }
}

/// `ksn check` と同じ検査を走らせ、致命的な診断の一覧 ("file:line: code: message") を返す。
/// 改名の検証 (改名の前後で診断が増えないこと) に使う。
pub(crate) fn check_error_lines(root: &Path) -> Vec<String> {
    let mut rep = Report::new(false);
    if let Err(e) = full_check(root, &mut rep, false) {
        return vec![format!("{}: {}", e.code, e.msg)];
    }
    rep.items
        .iter()
        .filter(|i| i.fatal)
        .map(|i| format!("{}:{}: {}: {}", i.file.clone().unwrap_or_default(), i.line, i.code, i.message))
        .collect()
}

fn cmd_rename(args: &[String]) -> i32 {
    let dry = args.iter().any(|a| a == "--dry-run");
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if pos.len() < 2 {
        eprintln!("ksn: usage: ksn rename [--dry-run] <old> <new>");
        return 2;
    }
    let (old, new) = (pos[0], pos[1]);
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let l = match load_all(&root) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let plan = match crate::refactor::plan_rename(&l, old, new) {
        Ok(p) => p,
        Err(m) => {
            eprintln!("ksn: {}", m);
            return 1;
        }
    };
    if let Err(m) = crate::refactor::verify(&root, &plan) {
        eprintln!("ksn: {}", m);
        return 1;
    }
    for c in &plan.changes {
        match &c.old_path {
            Some(o) if *o != c.new_path => println!("move {} -> {}", rel(o), rel(&c.new_path)),
            _ => println!("update {}", rel(&c.new_path)),
        }
    }
    if let Some((p, _, _)) = &plan.manifest {
        println!("update {}", rel(p));
    }
    for n in &plan.notes {
        eprintln!("ksn: note: {}", n);
    }
    if dry {
        for c in &plan.changes {
            let diff = similar::TextDiff::from_lines(c.old_text.as_str(), c.new_text.as_str());
            let oldh = c.old_path.as_deref().map(rel).unwrap_or_default();
            print!("{}", diff.unified_diff().header(&oldh, &rel(&c.new_path)));
        }
        if let Some((p, o, n)) = &plan.manifest {
            let diff = similar::TextDiff::from_lines(o.as_str(), n.as_str());
            print!("{}", diff.unified_diff().header(&rel(p), &rel(p)));
        }
        return 0;
    }
    if let Err(m) = crate::refactor::apply(&plan) {
        eprintln!("ksn: {}", m);
        return 1;
    }
    0
}

fn cmd_fmt(args: &[String]) -> i32 {
    let check = args.iter().any(|a| a == "--check");
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let pkg = match load_package(&root) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let srcroot = norm(&pkg.root.join("src"));
    let grammar = Grammar::new();
    let mut diff = false;
    let mut changed = false;
    for s in &pkg.sources {
        let nt = if s.error.is_none() { format_source(&s.text, &s.path) } else { mechanical_collapse(&s.text) };
        let mut text = s.text.clone();
        if nt != s.text {
            changed = true;
            println!("{} {}", if check { "would reformat" } else { "reformatted" }, rel(&s.path));
            if !check {
                fs::write(&s.path, &nt).unwrap();
                text = nt;
            }
        }
        let relpath = s.path.strip_prefix(&format!("{}/", srcroot)).unwrap_or(&s.path).to_string();
        for (code, l, m) in Lint::new(&relpath, &text.replace("\r\n", "\n")).run() {
            if code != "unused" {
                diff = true;
                println!("{}:{}: {}: {} (not auto-fixable)", rel(&s.path), l, lint_code(&code), m);
            }
        }
        let (ok, msg, ln) = grammar.check(&text);
        if !ok {
            diff = true;
            println!("{}:{}: E0002: {}", rel(&s.path), ln, msg);
        }
    }
    if diff || (changed && check) { 1 } else { 0 }
}

fn cmd_explain(args: &[String]) -> i32 {
    let code = args.first().map(|s| s.as_str()).unwrap_or("");
    match EXPLAIN.iter().find(|(k, _)| *k == code) {
        Some((k, v)) => {
            println!("{}: {}", k, v);
            0
        }
        None => {
            eprintln!("ksn: unknown code {}", code);
            1
        }
    }
}

fn cmd_update() -> i32 {
    let (root, pkg, deps) = match find_root().and_then(|r| {
        let p = load_package(&r)?;
        let d = load_deps(&p)?;
        Ok((r, p, d))
    }) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 2;
        }
    };
    let m = pkg.manifest.get("dependencies").and_then(|d| d.as_table()).cloned().unwrap_or_default();
    let mut lines = vec!["version = 1".to_string(), String::new()];
    let mut sorted: Vec<&Package> = deps.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for dp in sorted {
        let dv = m.get(&dp.name);
        lines.push("[[package]]".into());
        lines.push(format!("name = \"{}\"", dp.name));
        if let Some(git) = dv.and_then(|d| d.get("git")).and_then(|g| g.as_str()) {
            let rev = dv.and_then(|d| d.get("rev")).and_then(|g| g.as_str()).unwrap_or("");
            lines.push(format!("source = \"git+{}#{}\"", git, rev));
            lines.push(format!("hash = \"{}\"", package_hash(&dp.root)));
        } else {
            let a = norm(&dp.root);
            let b = norm(&pkg.root);
            let p = relative_path(&a, &b);
            lines.push(format!("source = \"path+{}\"", p));
        }
        let mut dn: Vec<String> = dp
            .manifest
            .get("dependencies")
            .and_then(|d| d.as_table())
            .map(|t| t.keys().cloned().collect())
            .unwrap_or_default();
        dn.sort();
        lines.push(format!("deps = [{}]", dn.iter().map(|x| format!("\"{}\"", x)).collect::<Vec<_>>().join(", ")));
        lines.push(String::new());
    }
    let text = lines.join("\n").trim_end().to_string() + "\n";
    fs::write(root.join("kasane.lock"), text).unwrap();
    0
}

fn relative_path(target: &str, base: &str) -> String {
    let t: Vec<&str> = target.split('/').collect();
    let b: Vec<&str> = base.split('/').collect();
    let mut i = 0;
    while i < t.len() && i < b.len() && t[i] == b[i] {
        i += 1;
    }
    let mut out: Vec<String> = vec!["..".to_string(); b.len() - i];
    out.extend(t[i..].iter().map(|x| x.to_string()));
    if out.is_empty() { ".".into() } else { out.join("/") }
}

fn cmd_vendor() -> i32 {
    let (root, pkg) = match find_root().and_then(|r| load_package(&r).map(|p| (r, p))) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 2;
        }
    };
    let deps = pkg.manifest.get("dependencies").and_then(|d| d.as_table()).cloned().unwrap_or_default();
    for (dn, dv) in deps {
        let Some(git) = dv.get("git").and_then(|g| g.as_str()) else { continue };
        let dest = root.join("vendor").join(&dn);
        if dest.is_dir() {
            continue;
        }
        let rev = dv.get("rev").and_then(|r| r.as_str()).unwrap_or("");
        let ok = std::process::Command::new("git").args(["clone", git, &norm(&dest)]).status().map_or(false, |s| s.success())
            && std::process::Command::new("git").args(["-C", &norm(&dest), "checkout", rev]).status().map_or(false, |s| s.success());
        if !ok {
            eprintln!("ksn: failed to vendor {}", dn);
            return 2;
        }
    }
    0
}

// ------------------------------------------------------------ build
fn cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("KSN_CACHE_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".ksn").join("cache")
}

fn dir_size(p: &Path) -> u64 {
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            let q = e.path();
            n += if q.is_dir() { dir_size(&q) } else { e.metadata().map(|m| m.len()).unwrap_or(0) };
        }
    }
    n
}

fn copy_tree(src: &Path, dst: &Path, skip: &[&str]) {
    let _ = fs::create_dir_all(dst);
    if let Ok(rd) = fs::read_dir(src) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if skip.contains(&name.as_str()) {
                continue;
            }
            let (s, d) = (e.path(), dst.join(&name));
            if s.is_dir() {
                copy_tree(&s, &d, skip);
            } else {
                let _ = fs::copy(&s, &d);
            }
        }
    }
}

fn evict(cdir: &Path, keep: &str) {
    let limit: u64 = std::env::var("KSN_CACHE_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(2 * 1024 * 1024 * 1024);
    let Ok(rd) = fs::read_dir(cdir) else { return };
    let mut ents: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy() != keep)
        .map(|e| {
            let used = e.path().join(".used");
            let t = fs::metadata(&used).or_else(|_| e.metadata()).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            (t, e.path())
        })
        .collect();
    ents.sort();
    let mut total: u64 = ents.iter().map(|(_, p)| dir_size(p)).sum();
    while !ents.is_empty() && total > limit {
        let (_, p) = ents.remove(0);
        total -= dir_size(&p);
        let _ = fs::remove_dir_all(&p);
    }
}

fn build_key(l: &Loaded, release: bool, target: &str) -> String {
    let mut h = Sha256::new();
    let edition = l.pkg.manifest["package"].get("edition").map(|e| e.to_string()).unwrap_or_default();
    h.update(format!("{}|{}|{}|{}", VERSION, edition, if release { "release" } else { "debug" }, target).as_bytes());
    h.update(package_hash(&l.pkg.root).as_bytes());
    for dp in &l.deps {
        h.update(package_hash(&dp.root).as_bytes());
    }
    for s in &l.std {
        h.update(s.text.as_bytes());
    }
    if let Ok(exe) = std::env::current_exe() {
        h.update(fs::read(exe).unwrap_or_default());
    }
    h.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

fn cmd_build(args: &[String], preloaded: Option<&Loaded>) -> i32 {
    let release = args.iter().any(|a| a == "--release");
    let backend = args.iter().position(|a| a == "--backend").and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| "clif".into());
    let target = args.iter().position(|a| a == "--target").and_then(|i| args.get(i + 1)).cloned();
    let mut rep = Report::new(false);
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let (l, prog, user) = match full_check(&root, &mut rep, false) {
        Ok(x) => x,
        Err(e) => {
            rep.add(&e.code, &e.msg, None, 1, 1, true);
            rep.flush();
            return 1;
        }
    };
    let _ = preloaded;
    rep.flush();
    if rep.fatal() {
        return 1;
    }
    let target = target.unwrap_or("host".into());
    if target != "host" && !Regex::new(r"^(x86_64|aarch64)-(linux|macos|windows)$").unwrap().is_match(&target) {
        eprintln!("ksn: E9040: unsupported target {}", target);
        return 1;
    }
    let kind = if release { "release" } else { "debug" };
    let key = build_key(&l, release, &target);
    let cdir = cache_dir();
    let cached = cdir.join(&key);
    if cached.is_dir() {
        let _ = fs::write(cached.join(".used"), "");
        println!("cache hit");
    } else {
        let tmp = cdir.join(format!("{}.tmp", key));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        if let Ok(exe) = std::env::current_exe() {
            let _ = fs::copy(exe, tmp.join(exe_name()));
        }
        copy_tree(&stdlib_dir(), &tmp.join("stdlib"), &[]);
        let pk = tmp.join("pkg");
        copy_tree(&l.pkg.root, &pk, &["target", "__pycache__", ".git"]);
        for dp in &l.deps {
            copy_tree(&dp.root, &pk.join("vendor").join(&dp.name), &["target", "__pycache__", ".git"]);
        }
        let _ = fs::write(tmp.join(".used"), "");
        fs::rename(&tmp, &cached).unwrap();
        evict(&cdir, &key);
    }
    let out = l.pkg.root.join("target").join(kind);
    let _ = fs::remove_dir_all(&out);
    copy_tree(&cached, &out, &[".used"]);
    let name = l.pkg.name.clone();
    let _ = fs::write(out.join(format!("{}.cmd", name)), "@echo off\r\nset KSN_STDLIB=%~dp0stdlib\r\ncd /d \"%~dp0pkg\"\r\n\"%~dp0ksn.exe\" _exec %*\r\n");
    let _ = fs::write(out.join(&name), "#!/bin/sh\nd=\"$(dirname \"$0\")\"\nKSN_STDLIB=\"$d/stdlib\"\nexport KSN_STDLIB\ncd \"$d/pkg\" && exec \"$d/ksn\" _exec \"$@\"\n");
    let prog = prog.unwrap();
    let entry = l.pkg.entry();
    let exe = out.join(if cfg!(windows) { format!("{}.exe", name) } else { format!("{}.native", name) });
    let native: Result<(), String> = match &entry {
        None => Err("library package".into()),
        Some(e) => {
            let mut funcs: HashMap<String, &Def> = HashMap::new();
            for &i in &user {
                let d = &prog.defs[i];
                if d.kw == "fn" && d.body.is_some() {
                    funcs.insert(d.name.clone(), d);
                }
            }
            if backend == "clif" {
                let mut tdefs: HashMap<String, &Def> = HashMap::new();
                for &i in &user {
                    let d = &prog.defs[i];
                    if d.kw == "type" || d.kw == "enum" {
                        tdefs.insert(d.name.clone(), d);
                    }
                }
                crate::clif::compile_clif(&prog, &funcs, &tdefs, e, &norm(&exe), release)
            } else {
                crate::cnative::compile_native(&funcs, e, &norm(&exe), release)
            }
        }
    };
    match native {
        Ok(()) => println!("native executable: {}", rel(&norm(&exe))),
        Err(e) => println!("native backend skipped: {}", e),
    }
    println!("built {} ({}, target {}; portable interpreter bundle)", rel(&norm(&out)), kind, target);
    0
}

/// ビルド成果物の実行入口 (内部コマンド)。
fn cmd_exec(args: &[String]) -> i32 {
    let root = std::env::current_dir().unwrap_or_default();
    let l = match load_all(&root) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 2;
        }
    };
    let (prog, user) = build_program(&l);
    let prog = Rc::new(prog);
    if args.first().map(|s| s.as_str()) == Some("--test") {
        let name = args.get(1).cloned().unwrap_or_default();
        let Some(&ti) = user.iter().find(|&&i| prog.defs[i].kw == "test" && prog.defs[i].name == name) else {
            eprintln!("ksn: no such test {}", name);
            return 2;
        };
        let mut it = Interp::new(prog.clone());
        let r = it.call_body(ti);
        use std::io::Write;
        let _ = it.out.flush();
        return match r {
            Err(Ctl::Panic(m)) => {
                eprintln!("panic: {}", m);
                101
            }
            _ => 0,
        };
    }
    let Some(entry) = l.pkg.entry() else { return 2 };
    let Some(&di) = prog.funcs.get(&entry) else { return 2 };
    let mut it = Interp::new(prog);
    it.args = args.to_vec();
    it.run_entry(di)
}

// ------------------------------------------------------------ エントリ
pub fn main_cli(args: Vec<String>) -> i32 {
    // E2044 (型が決まらない) は既定で有効。`--no-strict-types` で旧来の挙動に戻す (移行用)。
    // `--strict-types` は受け付けるだけで何も変えない。どちらも全コマンド共通 (check / build / test / run)。
    let mut rest: Vec<String> = args[1..].to_vec();
    // `--` より後ろはプログラムへの引数なので触らない。
    let cut = rest.iter().position(|a| a == "--").unwrap_or(rest.len());
    if rest[..cut].iter().any(|a| a == "--no-strict-types") {
        crate::types::set_strict_types(false);
    }
    let tail = rest.split_off(cut);
    rest.retain(|a| a != "--no-strict-types" && a != "--strict-types");
    rest.extend(tail);
    match args[0].as_str() {
        "new" => match rest.first() {
            Some(n) => cmd_new(n),
            None => {
                eprintln!("usage: ksn new <name>");
                2
            }
        },
        "check" => {
            cmd_check(rest.iter().any(|a| a == "--json"))
        }
        "run" => cmd_run(&rest),
        "test" => cmd_test(&rest),
        "get" => cmd_get(&rest),
        "sig" => cmd_sig(&rest),
        "doc" => cmd_doc(&rest),
        "refs" => cmd_refs(&rest),
        "rename" => cmd_rename(&rest),
        "fmt" => cmd_fmt(&rest),
        "fix" => cmd_fix(),
        "todo" => cmd_todo(),
        "context" => cmd_context(&rest),
        "explain" => cmd_explain(&rest),
        "lsp" => crate::lsp::run(),
        "build" => cmd_build(&rest, None),
        "update" => cmd_update(),
        "vendor" => cmd_vendor(),
        "_exec" => cmd_exec(&rest),
        "--version" | "-V" => {
            println!("ksn {}", VERSION);
            0
        }
        _ => {
            eprintln!(
                "usage: ksn <new|check|fix|todo|context|fmt|run|test|build|get|sig|doc|refs|rename|explain|lsp|update|vendor> [args]"
            );
            2
        }
    }
}
