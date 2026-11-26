//! インタプリタ上のソースレベル・デバッガ (`ksn debug` / `ksn test --debug` / `ksn dap`)。
//!
//! 仕組み: `interp.rs` の 5 か所 (文の実行・関数/クロージャ/テストの入口と出口・スレッドの切り替え) に
//! `if self.dbg.is_some()` の分岐を置き、デバッガが付いているときだけここの `impl Interp` を呼ぶ。
//! 付いていないときの費用は、文ごとと呼び出しごとの `Option` の判定 1 回だけ。
//!
//! 停止中はデバッガ本体を `Interp::dbg` から取り出し、フロントエンド (`Frontend`) がコマンドを読む。
//! 取り出している間は `dbg` が `None` なので、式の評価はフックに再入しない。
//!
//! 式の評価 (条件・`print`・DAP の `evaluate`) は `uses [] panics never` の範囲に限る:
//! (1) 構文木を許可リストで検査し、代入・ブロック・ループ・クロージャ・`?` などを拒否する (`check_pure`)。
//! (2) 評価中は `Interp::dbg_guard` を立て、呼び出し先の定義が `uses []` かつ `panics never` で `&mut` 引数を取らないものだけ許す
//!     (組み込みは仕様 (.sig) の宣言で同じ判定。メソッドは同名の全候補が条件を満たすときだけ)。
//!     `+` や添字などの演算子の失敗 (オーバーフロー・範囲外) は式の評価エラーとして返し、プログラムを panic させない。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{BufRead, Read, Write};
use std::rc::Rc;

use serde_json::{json, Value};

use crate::check::{children, nf, nline, nname};
use crate::interp::Interp;
use crate::parser::{unpack_pos, Parser, V};
use crate::value::*;

// ------------------------------------------------------------ 型

/// 1 つの呼び出し (関数・クロージャ・テスト)。
pub struct Frame {
    pub name: String,
    pub file: Rc<str>,
    pub line: usize,
    pub col: usize,
    /// 呼び出しの最上位のスコープ (ローカル変数を集める終点)
    root: Scope,
    /// 直近に実行を始めた文のスコープ
    cur: Scope,
    /// まだ文を 1 つも実行していない (関数ブレークポイントの判定用)
    fresh: bool,
    /// 利用者のパッケージのコードか (標準ライブラリの中では止まらない)
    user: bool,
    is_def: bool,
}

/// 1 つのスレッドの呼び出し列。インタプリタのスレッドは保留してから 1 つずつ最後まで走らせるので
/// (interp.rs の `threads`)、spawn されたクロージャの中で止まったときは、そのスレッドの呼び出し列が
/// 一番内側になる。join している側のスレッドの呼び出し列は、その外側に残る。
pub struct ThreadCtx {
    pub id: i64,
    pub name: String,
    pub frames: Vec<Frame>,
}

pub struct Cond {
    pub text: String,
    ast: V,
}

pub enum BpKind {
    Line { file: String, line: usize },
    Func { name: String },
}

pub struct Bp {
    pub id: u32,
    pub kind: BpKind,
    pub cond: Option<Cond>,
    pub hits: u32,
}

pub struct BpInfo {
    pub id: u32,
    pub verified: bool,
    pub kind: &'static str,
    pub file: String,
    pub line: usize,
    pub function: String,
    pub message: String,
    pub condition: Option<String>,
}

#[derive(Clone, Copy)]
enum Mode {
    Run,
    In,
    /// (スレッドの入れ子, 呼び出しの深さ) がこれ以下になった最初の文で止まる
    Over(usize, usize),
    /// これより小さくなった最初の文で止まる
    Out(usize, usize),
}

pub struct Stop {
    /// `breakpoint` / `function breakpoint` / `step` / `panic` / `entry`
    pub reason: String,
    pub message: String,
    pub bp_ids: Vec<u32>,
}

pub enum Resume {
    Continue,
    In,
    Next,
    Out,
    Quit,
}

/// プログラムの終わり方。
pub struct Outcome {
    pub code: i32,
    pub panic: Option<String>,
    pub error: Option<String>,
    pub quit: bool,
}

pub trait Frontend {
    /// 停止中。コマンドを処理して、再開のしかたを返す。
    fn stopped(&mut self, d: &mut Debugger, it: &mut Interp, s: &Stop) -> Resume;
    /// プログラムが終わった (`Interp::dbg` は `None`)。
    fn finished(&mut self, d: &mut Debugger, it: &mut Interp, o: &Outcome);
}

pub struct FrameInfo {
    pub index: usize,
    pub name: String,
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub user: bool,
}

pub struct Debugger {
    pub user_files: HashSet<String>,
    pub bps: Vec<Bp>,
    next_id: u32,
    mode: Mode,
    pub threads: Vec<ThreadCtx>,
    pub fe: Option<Box<dyn Frontend>>,
    panic_reported: bool,
    pub stop_on_panic: bool,
    pub quit: bool,
    stmt_cache: HashMap<String, BTreeSet<usize>>,
    src_cache: HashMap<String, Vec<String>>,
    /// 直前に止まった文 (継続せずに続けて止まるのを避ける目印)
    pub entry_stop: bool,
}

impl Debugger {
    pub fn new(user_files: HashSet<String>) -> Debugger {
        Debugger {
            user_files,
            bps: vec![],
            next_id: 1,
            mode: Mode::Run,
            threads: vec![ThreadCtx { id: 1, name: "main".into(), frames: vec![] }],
            fe: None,
            panic_reported: false,
            stop_on_panic: true,
            quit: false,
            stmt_cache: HashMap::new(),
            src_cache: HashMap::new(),
            entry_stop: false,
        }
    }

    /// 最初の文で止まる (DAP の `stopOnEntry`)。
    pub fn stop_on_entry(&mut self) {
        self.mode = Mode::In;
        self.entry_stop = true;
    }

    // -------------------------------------------------------- ブレークポイント
    fn stmt_lines(&mut self, it: &Interp, file: &str) -> BTreeSet<usize> {
        if let Some(s) = self.stmt_cache.get(file) {
            return s.clone();
        }
        fn walk(v: &V, out: &mut BTreeSet<usize>) {
            if matches!(nname(v), "Let" | "Assign" | "ExprStmt") {
                let l = unpack_pos(nline(v)).0;
                if l > 0 {
                    out.insert(l);
                }
            }
            for c in children(v) {
                walk(c, out);
            }
        }
        let mut out = BTreeSet::new();
        for d in it.p.defs.iter().filter(|d| d.file == file) {
            if let Some(b) = &d.body {
                walk(b, &mut out);
            }
        }
        self.stmt_cache.insert(file.to_string(), out.clone());
        out
    }

    fn find_user_file(&self, pat: &str) -> Option<String> {
        let mut fs: Vec<&String> = self.user_files.iter().filter(|f| same_file(f, pat)).collect();
        fs.sort();
        fs.first().map(|s| s.to_string())
    }

    fn mk_cond(it: &Interp, cond: Option<&str>) -> Result<Option<Cond>, String> {
        match cond.map(str::trim).filter(|c| !c.is_empty()) {
            None => Ok(None),
            Some(t) => {
                let ast = parse_expr(t)?;
                check_pure(&ast)?;
                check_calls(it, &ast)?;
                Ok(Some(Cond { text: t.to_string(), ast }))
            }
        }
    }

    pub fn set_line_bp(&mut self, it: &Interp, pat: &str, line: usize, cond: Option<&str>) -> BpInfo {
        let id = self.next_id;
        self.next_id += 1;
        let mut info = BpInfo {
            id,
            verified: false,
            kind: "line",
            file: pat.to_string(),
            line,
            function: String::new(),
            message: String::new(),
            condition: cond.map(String::from),
        };
        let cond = match Self::mk_cond(it, cond) {
            Ok(c) => c,
            Err(e) => {
                info.message = e;
                return info;
            }
        };
        let Some(file) = self.find_user_file(pat) else {
            info.message = format!("no source file matches `{}` in this package", pat);
            return info;
        };
        let lines = self.stmt_lines(it, &file);
        let Some(&actual) = lines.range(line..).next() else {
            info.message = format!("no code at or after line {}", line);
            return info;
        };
        info.verified = true;
        info.line = actual;
        info.file = file.clone();
        self.bps.push(Bp { id, kind: BpKind::Line { file, line: actual }, cond, hits: 0 });
        info
    }

    pub fn set_func_bp(&mut self, it: &Interp, name: &str, cond: Option<&str>) -> BpInfo {
        let id = self.next_id;
        self.next_id += 1;
        let name = name.trim();
        let name = name.strip_prefix("fn ").unwrap_or(name).trim().to_string();
        let mut info = BpInfo {
            id,
            verified: false,
            kind: "function",
            file: String::new(),
            line: 0,
            function: name.clone(),
            message: String::new(),
            condition: cond.map(String::from),
        };
        let cond = match Self::mk_cond(it, cond) {
            Ok(c) => c,
            Err(e) => {
                info.message = e;
                return info;
            }
        };
        let Some(d) = it.p.defs.iter().find(|d| d.name == name && matches!(d.kw.as_str(), "fn" | "test") && d.body.is_some()) else {
            info.message = format!("no function `{}`", name);
            return info;
        };
        info.verified = true;
        info.file = d.file.clone();
        info.line = d.line;
        self.bps.push(Bp { id, kind: BpKind::Func { name }, cond, hits: 0 });
        info
    }

    pub fn delete_bp(&mut self, id: u32) -> bool {
        let n = self.bps.len();
        self.bps.retain(|b| b.id != id);
        self.bps.len() != n
    }

    pub fn delete_all(&mut self) {
        self.bps.clear();
    }

    /// DAP の `setBreakpoints`: そのファイルの行ブレークポイントを置き換える。
    pub fn clear_file_bps(&mut self, pat: &str) {
        self.bps.retain(|b| !matches!(&b.kind, BpKind::Line { file, .. } if same_file(file, pat)));
    }

    pub fn clear_func_bps(&mut self) {
        self.bps.retain(|b| !matches!(b.kind, BpKind::Func { .. }));
    }

    // -------------------------------------------------------- 呼び出し列
    pub fn cur_thread(&self) -> &ThreadCtx {
        self.threads.last().unwrap()
    }

    pub fn thread_pos(&self, id: i64) -> Option<usize> {
        self.threads.iter().position(|t| t.id == id)
    }

    /// スレッド `tp` の呼び出し列 (一番内側が先頭)。
    pub fn backtrace(&self, tp: usize) -> Vec<FrameInfo> {
        let Some(t) = self.threads.get(tp) else { return vec![] };
        t.frames
            .iter()
            .rev()
            .enumerate()
            .map(|(i, f)| FrameInfo { index: i, name: f.name.clone(), file: f.file.to_string(), line: f.line, col: f.col, user: f.user })
            .collect()
    }

    fn frame(&self, tp: usize, idx: usize) -> Option<&Frame> {
        let t = self.threads.get(tp)?;
        let n = t.frames.len();
        if idx < n { t.frames.get(n - 1 - idx) } else { None }
    }

    /// ファイルの `line` 行目の本文 (前後の空白を除く)。
    pub fn source_line(&mut self, file: &str, line: usize) -> String {
        let lines = self
            .src_cache
            .entry(file.to_string())
            .or_insert_with(|| std::fs::read_to_string(file).unwrap_or_default().lines().map(String::from).collect());
        lines.get(line.wrapping_sub(1)).map(|l| l.trim().to_string()).unwrap_or_default()
    }

    /// フレームのローカル変数 (内側のスコープから。同名は内側が優先。同じスコープ内は名前順)。
    pub fn locals(&self, it: &mut Interp, tp: usize, idx: usize) -> Result<Vec<(String, Val)>, String> {
        let f = self.frame(tp, idx).ok_or("no such frame")?;
        let mut out: Vec<(String, Val)> = vec![];
        let mut seen: HashSet<String> = HashSet::new();
        let mut cur = Some(f.cur.clone());
        let _ = it;
        while let Some(sc) = cur {
            let mut here: Vec<(String, Val)> = sc.borrow().vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            here.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, v) in here {
                if seen.insert(k.clone()) {
                    out.push((k, v));
                }
            }
            if Rc::ptr_eq(&sc, &f.root) {
                break;
            }
            cur = sc.borrow().parent.clone();
        }
        Ok(out)
    }

    // -------------------------------------------------------- 評価
    /// 式を (副作用なしで) 評価して値を返す。
    pub fn eval_val(&mut self, it: &mut Interp, tp: usize, idx: usize, text: &str) -> Result<Val, String> {
        let ast = parse_expr(text)?;
        check_pure(&ast)?;
        check_calls(it, &ast)?;
        let sc = self.frame(tp, idx).ok_or("no frame (the program is not stopped)")?.cur.clone();
        eval_guarded(it, &ast, &sc)
    }

    /// 式を評価して、表示用の文字列にする。
    pub fn eval_str(&mut self, it: &mut Interp, tp: usize, idx: usize, text: &str) -> Result<String, String> {
        let v = self.eval_val(it, tp, idx, text)?;
        Ok(fmt_val(it, &v))
    }

    pub fn fmt(&mut self, it: &mut Interp, v: &Val) -> String {
        fmt_val(it, v)
    }

    fn cond_holds(&mut self, it: &mut Interp, bp: usize) -> (bool, Option<String>) {
        let Some(c) = &self.bps[bp].cond else { return (true, None) };
        let ast = c.ast.clone();
        let Some(f) = self.threads.last().and_then(|t| t.frames.last()) else { return (true, None) };
        let sc = f.cur.clone();
        match eval_guarded(it, &ast, &sc) {
            Ok(v) => match dr(&v) {
                Val::Bool(b) => (b, None),
                _ => (true, Some("breakpoint condition is not a bool; stopping".into())),
            },
            Err(e) => (true, Some(format!("breakpoint condition failed ({}); stopping", e))),
        }
    }

    // -------------------------------------------------------- 停止
    fn stop(&mut self, it: &mut Interp, s: Stop) -> Result<(), Ctl> {
        let _ = std::io::Write::flush(&mut it.out);
        let Some(mut fe) = self.fe.take() else { return Ok(()) };
        let r = fe.stopped(self, it, &s);
        self.fe = Some(fe);
        let (lvl, depth) = (self.threads.len(), self.threads.last().map_or(0, |t| t.frames.len()));
        self.mode = match r {
            Resume::Continue => Mode::Run,
            Resume::In => Mode::In,
            Resume::Next => Mode::Over(lvl, depth),
            Resume::Out => Mode::Out(lvl, depth),
            Resume::Quit => {
                self.quit = true;
                return Err(Ctl::Exit(0));
            }
        };
        Ok(())
    }

    fn on_stmt(&mut self, it: &mut Interp, s: &V, sc: &Scope) -> Result<(), Ctl> {
        let lvl = self.threads.len();
        let Some(t) = self.threads.last_mut() else { return Ok(()) };
        let depth = t.frames.len();
        let Some(f) = t.frames.last_mut() else { return Ok(()) };
        let (ln, col) = unpack_pos(nline(s));
        f.line = ln;
        f.col = col;
        f.cur = sc.clone();
        let fresh = std::mem::replace(&mut f.fresh, false);
        if !f.user {
            return Ok(());
        }
        let file = f.file.clone();
        let fname = if f.is_def { Some(f.name.clone()) } else { None };
        let mut hit: Vec<u32> = vec![];
        let mut reason = String::new();
        let mut message = String::new();
        for i in 0..self.bps.len() {
            let m = match &self.bps[i].kind {
                BpKind::Line { file: bf, line } => *line == ln && same_file(&file, bf),
                BpKind::Func { name } => fresh && fname.as_deref() == Some(name.as_str()),
            };
            if !m {
                continue;
            }
            let (ok, note) = self.cond_holds(it, i);
            if !ok {
                continue;
            }
            self.bps[i].hits += 1;
            hit.push(self.bps[i].id);
            if reason.is_empty() {
                reason = if matches!(self.bps[i].kind, BpKind::Func { .. }) { "function breakpoint" } else { "breakpoint" }.into();
            }
            if let Some(n) = note {
                message = n;
            }
        }
        if reason.is_empty() {
            let stepping = match self.mode {
                Mode::Run => false,
                Mode::In => true,
                Mode::Over(l, d) => (lvl, depth) <= (l, d),
                Mode::Out(l, d) => (lvl, depth) < (l, d),
            };
            if !stepping {
                return Ok(());
            }
            reason = if std::mem::replace(&mut self.entry_stop, false) { "entry" } else { "step" }.into();
        }
        self.stop(it, Stop { reason, message, bp_ids: hit })
    }
}

fn same_file(a: &str, b: &str) -> bool {
    let (a, b) = (a.replace('\\', "/"), b.replace('\\', "/"));
    let (a, b) = (a.trim_start_matches("./"), b.trim_start_matches("./"));
    if a == b || a.ends_with(&format!("/{}", b)) || b.ends_with(&format!("/{}", a)) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

// ------------------------------------------------------------ 式

pub fn parse_expr(text: &str) -> Result<V, String> {
    let toks = crate::lexer::lex(&format!("{}\n", text.trim())).map_err(|e| format!("syntax error: {}", e))?;
    let mut p = Parser::new(toks);
    p.skip_nl();
    let e = p.expr(false).map_err(|e| format!("syntax error: {}", e))?;
    p.skip_nl();
    if !p.at_eof() {
        return Err("syntax error: trailing input after the expression".into());
    }
    Ok(e)
}

/// 構文木の許可リスト。値を読む・計算するノードだけを通す。呼び出し (`Call` / `MCall`) は構文の上では通し、
/// 実行時に呼び出し先ごとに `Interp::dbg_guard` が判定する。
pub fn check_pure(e: &V) -> Result<(), String> {
    match nname(e) {
        "Lit" | "Path" | "Field" | "Index" | "Unary" | "Bin" | "TupleE" | "ArrayE" | "ArrayRep" | "StructLit" | "If" | "Call" | "MCall" => {}
        "" => return Ok(()),
        n => return Err(format!("`{}` is not allowed in a debugger expression (only pure expressions: no assignment, blocks, loops, closures, `?`)", n)),
    }
    for c in children(e) {
        check_pure(c)?;
    }
    Ok(())
}

/// 呼び出し先が (名前だけで) 分かるものは、評価の前に効果を調べて断る。
/// 分からないもの (局所変数の関数値など) は、評価中の `dbg_guard` が判定する。
fn check_calls(it: &Interp, e: &V) -> Result<(), String> {
    use crate::check::strs;
    match nname(e) {
        "Call" if nname(&nf(e)[0]) == "Path" => {
            let segs = strs(&nf(&nf(e)[0])[0]);
            if segs.len() >= 2 {
                if let Some(&i) = it.p.funcs.get(&segs.join(".")) {
                    if !pure_def(&it.p.defs[i]) {
                        return Err(format!("`{}` is not allowed in a debugger expression: it is not `uses [] panics never` (or takes `&mut`)", segs.join(".")));
                    }
                }
            }
        }
        "MCall" => {
            let name = crate::check::st(&nf(e)[1]);
            if let Some(c) = it.p.method_idx.get(&name) {
                if let Some(&i) = c.iter().find(|&&i| !pure_def(&it.p.defs[i])) {
                    return Err(format!("`.{}()` is not allowed in a debugger expression: `{}` is not `uses [] panics never` (or takes `&mut`)", name, it.p.defs[i].name));
                }
            }
        }
        _ => {}
    }
    for c in children(e) {
        check_calls(it, c)?;
    }
    Ok(())
}

fn ctl_msg(c: Ctl) -> String {
    match c {
        Ctl::Panic(m) => m,
        Ctl::Exit(c) => format!("exit({})", c),
        _ => "control flow escaped the expression".into(),
    }
}

fn eval_guarded(it: &mut Interp, ast: &V, sc: &Scope) -> Result<Val, String> {
    let prev = std::mem::replace(&mut it.dbg_guard, true);
    let r = it.eval(ast, sc);
    it.dbg_guard = prev;
    r.map_err(ctl_msg)
}

fn fmt_val(it: &mut Interp, v: &Val) -> String {
    let prev = std::mem::replace(&mut it.dbg_guard, true);
    let r = it.debug_str(v);
    it.dbg_guard = prev;
    match r {
        Ok(s) => s,
        Err(c) => format!("<not displayed: {}>", ctl_msg(c)),
    }
}

/// 値の子 (DAP の展開と `print` 用)。
pub fn children_of(v: &Val) -> Vec<(String, Val)> {
    let v = as_raw(&dr(v));
    match &v {
        Val::Struct(s) => s.f.borrow().clone(),
        Val::Enum(e) => e.pl.iter().enumerate().map(|(i, x)| (i.to_string(), x.clone())).collect(),
        Val::Arr(_) | Val::Vec(_) | Val::View(..) | Val::Tuple(_) => {
            seq_items(&v).unwrap_or_default().into_iter().take(500).enumerate().map(|(i, x)| (i.to_string(), x)).collect()
        }
        _ => vec![],
    }
}

pub(crate) fn pure_def(d: &crate::program::Def) -> bool {
    d.uses.is_empty()
        && d.panics.as_deref() == Some("never")
        && d.params.iter().all(|(_, t)| {
            let dumped = t.dump();
            !(crate::check::tag(t) == "ref" && matches!(crate::check::tel(t, 1), V::B(true))) || dumped.contains("Formatter")
        })
}

// ------------------------------------------------------------ インタプリタ側のフック
impl Interp {
    pub(crate) fn dbg_stmt(&mut self, s: &V, sc: &Scope) -> Result<(), Ctl> {
        let mut d = self.dbg.take().unwrap();
        let r = d.on_stmt(self, s, sc);
        self.dbg = Some(d);
        r
    }

    pub(crate) fn dbg_enter(&mut self, name: String, sc: &Scope, is_def: bool) {
        let d = self.dbg.as_mut().unwrap();
        let file = sc.borrow().file.clone();
        let user = d.user_files.contains(&*file);
        let t = d.threads.last_mut().unwrap();
        let name = if is_def {
            name
        } else {
            match t.frames.last() {
                Some(f) => format!("{}.<closure>", f.name),
                None => "<closure>".to_string(),
            }
        };
        t.frames.push(Frame { name, file, line: 0, col: 0, root: sc.clone(), cur: sc.clone(), fresh: true, user, is_def });
    }

    /// 呼び出しの出口。panic で抜けるときは、まだ呼び出し列が残っているこの時点 (一番内側の呼び出し) で止まる。
    pub(crate) fn dbg_leave(&mut self, r: &Res) -> Result<(), Ctl> {
        let mut res = Ok(());
        if let Err(Ctl::Panic(m)) = r {
            let mut d = self.dbg.take().unwrap();
            if d.stop_on_panic && !d.panic_reported {
                d.panic_reported = true;
                res = d.stop(self, Stop { reason: "panic".into(), message: m.clone(), bp_ids: vec![] });
            }
            self.dbg = Some(d);
        }
        // `finish`: 戻った直後に、呼び出し元で (戻り値つきで) 止まる
        let ret = match r {
            Ok(v) | Err(Ctl::Return(v)) => Some(v.clone()),
            _ => None,
        };
        let mut finish = false;
        if let (Some(d), Some(_)) = (self.dbg.as_ref(), &ret) {
            let n = d.threads.last().unwrap().frames.len();
            finish = n > 1 && matches!(d.mode, Mode::Out(l, dp) if l == d.threads.len() && dp == n);
        }
        if let Some(d) = self.dbg.as_mut() {
            d.threads.last_mut().unwrap().frames.pop();
        }
        if finish && res.is_ok() {
            let mut d = self.dbg.take().unwrap();
            let msg = format!("returned {}", fmt_val(self, &ret.unwrap()));
            res = d.stop(self, Stop { reason: "step".into(), message: msg, bp_ids: vec![] });
            self.dbg = Some(d);
        }
        res
    }

    pub(crate) fn dbg_thread_enter(&mut self, idx: usize) {
        if let Some(d) = self.dbg.as_mut() {
            let id = idx as i64 + 2;
            d.threads.push(ThreadCtx { id, name: format!("thread {}", id), frames: vec![] });
        }
    }

    pub(crate) fn dbg_thread_exit(&mut self) {
        if let Some(d) = self.dbg.as_mut() {
            if d.threads.len() > 1 {
                d.threads.pop();
            }
        }
    }

    fn guard_fail<T>(&self, what: &str) -> Result<T, Ctl> {
        panic(format!(
            "debugger: `{}` is not allowed in a debugger expression (only `uses [] panics never` functions that take no `&mut`)",
            what
        ))
    }

    fn const_fail<T>(&self, what: &str) -> Result<T, Ctl> {
        panic(format!("{}: `{}` cannot run during constant evaluation (4.1)", crate::interp::CONST_EFFECT_MSG, what))
    }

    pub(crate) fn const_guard_def(&mut self, d: &crate::program::Def) -> Result<(), Ctl> {
        if d.uses.is_empty() { Ok(()) } else { self.const_fail(&d.name) }
    }

    pub(crate) fn const_guard_method(&mut self, name: &str) -> Result<(), Ctl> {
        match self.p.method_idx.get(name) {
            Some(c) if c.iter().all(|&i| self.p.defs[i].uses.is_empty()) => Ok(()),
            Some(_) => self.const_fail(&format!(".{}()", name)),
            None => Ok(()),
        }
    }

    pub(crate) fn const_guard_native(&mut self, name: &str) -> Result<(), Ctl> {
        for cand in [name.to_string(), format!("core.num.{}", name), format!("core.char.{}", name), format!("core.bool.{}", name)] {
            if let Some(d) = self.p.defs.iter().find(|d| d.name == cand && d.kw == "fn" && d.body.is_none()) {
                return if d.uses.is_empty() { Ok(()) } else { self.const_fail(name) };
            }
        }
        Ok(())
    }

    pub(crate) fn dbg_guard_def(&mut self, d: &crate::program::Def) -> Result<(), Ctl> {
        if pure_def(d) { Ok(()) } else { self.guard_fail(&d.name) }
    }

    pub(crate) fn dbg_guard_method(&mut self, name: &str) -> Result<(), Ctl> {
        let ok = match self.p.method_idx.get(name) {
            Some(c) => !c.is_empty() && c.iter().all(|&i| pure_def(&self.p.defs[i])),
            None => false,
        };
        if ok { Ok(()) } else { self.guard_fail(&format!(".{}()", name)) }
    }

    pub(crate) fn dbg_guard_native(&mut self, name: &str) -> Result<(), Ctl> {
        let mut found = None;
        for cand in [name.to_string(), format!("core.num.{}", name), format!("core.char.{}", name), format!("core.bool.{}", name)] {
            if let Some(i) = self.p.defs.iter().position(|d| d.name == cand && d.kw == "fn" && d.body.is_none()) {
                found = Some(i);
                break;
            }
        }
        match found {
            Some(i) if pure_def(&self.p.defs[i]) => Ok(()),
            _ => self.guard_fail(name),
        }
    }
}

// ------------------------------------------------------------ 実行の駆動

pub enum Run {
    Entry(usize),
    Test(usize),
}

/// 出力を `emit` に渡す書き込み先 (JSON / DAP のとき、プログラムの標準出力を事件にする)。
pub struct EventSink {
    pub emit: Box<dyn FnMut(&str)>,
    pend: Vec<u8>,
}

impl EventSink {
    pub fn new(emit: Box<dyn FnMut(&str)>) -> EventSink {
        EventSink { emit, pend: vec![] }
    }
}

impl Write for EventSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pend.extend_from_slice(buf);
        let ok = match std::str::from_utf8(&self.pend) {
            Ok(s) => s.len(),
            Err(e) => e.valid_up_to(),
        };
        if ok > 0 {
            let s = String::from_utf8_lossy(&self.pend[..ok]).to_string();
            (self.emit)(&s);
            self.pend.drain(..ok);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn outcome_of(it: &mut Interp, r: Res) -> Outcome {
    let quit = it.dbg.as_ref().map_or(false, |d| d.quit);
    let mut o = Outcome { code: 0, panic: None, error: None, quit };
    match r {
        Err(Ctl::Panic(m)) => {
            o.code = 101;
            o.panic = Some(m);
        }
        Err(Ctl::Exit(c)) => o.code = c,
        Err(_) => {}
        Ok(v) => {
            let v = dr(&v);
            if let Val::Enum(e) = &v {
                if e.en == "Result" && e.var == "Err" {
                    o.code = 1;
                    o.error = Some(it.debug_str(&e.pl[0]).unwrap_or_default());
                }
            }
        }
    }
    o
}

/// デバッガを付けてプログラム (またはテスト) を最後まで走らせ、終わりをフロントエンドに知らせる。終了コードを返す。
pub fn drive(mut it: Interp, mut dbg: Debugger, fe: Box<dyn Frontend>, run: Run) -> i32 {
    dbg.fe = Some(fe);
    it.dbg = Some(Box::new(dbg));
    let r = match run {
        Run::Entry(di) => it.call_def(di, vec![]),
        Run::Test(ti) => it.call_body(ti),
    };
    let o = outcome_of(&mut it, r);
    let _ = it.out.flush();
    let mut d = *it.dbg.take().unwrap();
    let mut fe = d.fe.take().unwrap();
    d.threads.truncate(1);
    d.threads[0].frames.clear();
    fe.finished(&mut d, &mut it, &o);
    let _ = it.out.flush();
    o.code
}

// ------------------------------------------------------------ 読み込み

pub struct Target {
    pub prog: Rc<crate::program::Program>,
    pub entry: Option<usize>,
    pub entry_name: String,
    pub tests: Vec<usize>,
    pub user_files: HashSet<String>,
    pub user: Vec<usize>,
}

impl Target {
    pub fn find_test(&self, name: &str) -> Result<usize, String> {
        let hits: Vec<usize> = self.tests.iter().copied().filter(|&i| self.prog.defs[i].name == name).collect();
        match hits.first() {
            Some(&i) => Ok(i),
            None => {
                let mut names: Vec<&str> = self.tests.iter().map(|&i| self.prog.defs[i].name.as_str()).collect();
                names.sort();
                Err(format!("no such test `{}` (tests: {})", name, names.join(", ")))
            }
        }
    }
}

// ------------------------------------------------------------ 端末フロントエンド

pub struct Term {
    json: bool,
    started: bool,
    /// 選択しているフレーム (`frame N`)。停止のたびに 0 に戻る
    sel: usize,
    stopped_now: bool,
    last: Option<Stop>,
}

enum Flow {
    Stay,
    Run,
    Resume(Resume),
}

fn rel(p: &str) -> String {
    crate::cli::rel(p)
}

impl Term {
    fn emit(&self, j: Value, text: String) {
        if self.json {
            println!("{}", j);
        } else if !text.is_empty() {
            println!("{}", text);
        }
        let _ = std::io::stdout().flush();
    }

    fn err(&self, msg: &str) {
        self.emit(json!({"event": "error", "message": msg}), format!("error: {}", msg));
    }

    fn frame_json(d: &mut Debugger, f: &FrameInfo) -> Value {
        let src = d.source_line(&f.file, f.line);
        json!({"index": f.index, "function": f.name, "file": rel(&f.file), "line": f.line, "column": f.col, "source": src})
    }

    fn show_bp(&self, b: &BpInfo) {
        let j = json!({
            "event": "breakpoint", "id": b.id, "kind": b.kind, "verified": b.verified,
            "file": if b.file.is_empty() { Value::Null } else { json!(rel(&b.file)) },
            "line": b.line, "function": b.function, "condition": b.condition, "message": b.message,
        });
        let t = if b.verified {
            let at = if b.kind == "line" { format!("{}:{}", rel(&b.file), b.line) } else { b.function.clone() };
            let c = b.condition.as_ref().map_or(String::new(), |c| format!(" if {}", c));
            format!("breakpoint {} at {}{}", b.id, at, c)
        } else {
            format!("breakpoint {} not set: {}", b.id, b.message)
        };
        self.emit(j, t);
    }

    /// `break` の対象を解釈する: `file:line` / `FQN` / `fn FQN` / (停止中なら) 行番号だけ。
    fn cmd_break(&mut self, d: &mut Debugger, it: &mut Interp, rest: &str) {
        let (target, cond) = match rest.find(" if ") {
            Some(i) => (rest[..i].trim(), Some(rest[i + 4..].trim())),
            None => (rest.trim(), None),
        };
        if target.is_empty() {
            return self.err("usage: break <file>:<line> | <function FQN> [if <expr>]");
        }
        let info = if let Some(t) = target.strip_prefix("fn ") {
            d.set_func_bp(it, t, cond)
        } else if let Some((f, l)) = target.rsplit_once(':').filter(|(_, l)| l.parse::<usize>().is_ok()) {
            d.set_line_bp(it, f, l.parse().unwrap(), cond)
        } else if let Ok(l) = target.parse::<usize>() {
            match d.frame(d.threads.len() - 1, self.sel).map(|f| f.file.to_string()) {
                Some(f) => d.set_line_bp(it, &f, l, cond),
                None => return self.err("a bare line number needs a stopped frame; use <file>:<line>"),
            }
        } else {
            d.set_func_bp(it, target, cond)
        };
        self.show_bp(&info);
    }

    fn cmd_bt(&mut self, d: &mut Debugger, arg: &str) {
        let tp = arg.trim().parse::<i64>().ok().and_then(|id| d.thread_pos(id)).unwrap_or(d.threads.len() - 1);
        let fs = d.backtrace(tp);
        let mut arr = vec![];
        let mut text = vec![];
        for f in &fs {
            arr.push(Self::frame_json(d, f));
            text.push(format!("#{} {} {}:{}", f.index, f.name, rel(&f.file), f.line));
        }
        self.emit(json!({"event": "stack", "thread": d.threads[tp].id, "frames": arr}), text.join("\n"));
    }

    fn cmd_locals(&mut self, d: &mut Debugger, it: &mut Interp, arg: &str) {
        let idx = arg.trim().parse::<usize>().unwrap_or(self.sel);
        let tp = d.threads.len() - 1;
        match d.locals(it, tp, idx) {
            Err(e) => self.err(&e),
            Ok(vars) => {
                let mut arr = vec![];
                let mut text = vec![];
                for (k, v) in vars {
                    let s = d.fmt(it, &v);
                    text.push(format!("{} = {}", k, s));
                    arr.push(json!({"name": k, "value": s}));
                }
                if text.is_empty() {
                    text.push("(no locals)".into());
                }
                self.emit(json!({"event": "locals", "frame": idx, "vars": arr}), text.join("\n"));
            }
        }
    }

    fn cmd_print(&mut self, d: &mut Debugger, it: &mut Interp, expr: &str) {
        if expr.trim().is_empty() {
            return self.err("usage: print <expr>");
        }
        let tp = d.threads.len() - 1;
        match d.eval_str(it, tp, self.sel, expr) {
            Ok(v) => self.emit(json!({"event": "value", "expr": expr.trim(), "value": v}), format!("{} = {}", expr.trim(), v)),
            Err(e) => self.err(&e),
        }
    }

    fn handle(&mut self, d: &mut Debugger, it: &mut Interp, line: &str) -> Flow {
        let line = line.trim();
        let (cmd, rest) = match line.split_once(char::is_whitespace) {
            Some((c, r)) => (c, r.trim()),
            None => (line, ""),
        };
        let running = self.stopped_now;
        let need_stop = |me: &Term| -> bool {
            if !running {
                me.err("the program is not stopped");
            }
            running
        };
        match cmd {
            "break" | "b" => self.cmd_break(d, it, rest),
            "delete" | "d" => {
                if rest == "all" || rest.is_empty() {
                    d.delete_all();
                    self.emit(json!({"event": "deleted", "ids": "all"}), "all breakpoints deleted".into());
                } else {
                    for w in rest.split_whitespace() {
                        match w.trim_start_matches('#').parse::<u32>() {
                            Ok(id) if d.delete_bp(id) => self.emit(json!({"event": "deleted", "ids": [id]}), format!("breakpoint {} deleted", id)),
                            _ => self.err(&format!("no such breakpoint `{}`", w)),
                        }
                    }
                }
            }
            "breakpoints" | "info" => {
                let mut arr = vec![];
                let mut text = vec![];
                for b in &d.bps {
                    let (kind, at) = match &b.kind {
                        BpKind::Line { file, line } => ("line", format!("{}:{}", rel(file), line)),
                        BpKind::Func { name } => ("function", name.clone()),
                    };
                    arr.push(json!({"id": b.id, "kind": kind, "at": at, "condition": b.cond.as_ref().map(|c| c.text.clone()), "hits": b.hits}));
                    text.push(format!("{} {} {}{} (hits {})", b.id, kind, at, b.cond.as_ref().map_or(String::new(), |c| format!(" if {}", c.text)), b.hits));
                }
                if text.is_empty() {
                    text.push("(no breakpoints)".into());
                }
                self.emit(json!({"event": "breakpoints", "breakpoints": arr}), text.join("\n"));
            }
            "run" | "r" | "start" => {
                if self.started {
                    self.err("the program is already running; use `continue`");
                } else {
                    self.started = true;
                    return Flow::Run;
                }
            }
            "continue" | "c" => {
                if !self.started {
                    self.err("the program has not started; use `run`");
                } else if need_stop(self) {
                    return Flow::Resume(Resume::Continue);
                }
            }
            "step" | "s" | "stepin" => {
                if !self.started {
                    // 最初の文で止まる
                    d.stop_on_entry();
                    self.started = true;
                    return Flow::Run;
                }
                if need_stop(self) {
                    return Flow::Resume(Resume::In);
                }
            }
            "next" | "n" => {
                if need_stop(self) {
                    return Flow::Resume(Resume::Next);
                }
            }
            "finish" | "out" => {
                if need_stop(self) {
                    return Flow::Resume(Resume::Out);
                }
            }
            "bt" | "backtrace" | "where" => {
                if need_stop(self) {
                    self.cmd_bt(d, rest);
                }
            }
            "frame" | "f" => {
                if need_stop(self) {
                    match rest.parse::<usize>() {
                        Ok(n) if n < d.cur_thread().frames.len() => {
                            self.sel = n;
                            let fs = d.backtrace(d.threads.len() - 1);
                            let j = Self::frame_json(d, &fs[n]);
                            self.emit(json!({"event": "frame", "frame": j}), format!("#{} {} {}:{}", n, fs[n].name, rel(&fs[n].file), fs[n].line));
                        }
                        _ => self.err("usage: frame <index>"),
                    }
                }
            }
            "locals" | "l" => {
                if need_stop(self) {
                    self.cmd_locals(d, it, rest);
                }
            }
            "print" | "p" | "eval" => {
                if need_stop(self) {
                    self.cmd_print(d, it, rest);
                }
            }
            "threads" => {
                let arr: Vec<Value> = d.threads.iter().map(|t| json!({"id": t.id, "name": t.name})).collect();
                let text = d.threads.iter().map(|t| format!("{} {}", t.id, t.name)).collect::<Vec<_>>().join("\n");
                self.emit(json!({"event": "threads", "threads": arr}), text);
            }
            "quit" | "q" | "exit" => return Flow::Resume(Resume::Quit),
            "" => {}
            c => self.err(&format!("unknown command `{}` (break delete breakpoints run continue step next finish bt frame locals print threads quit)", c)),
        }
        Flow::Stay
    }

    fn read_line() -> Option<String> {
        let mut s = String::new();
        loop {
            s.clear();
            match std::io::stdin().lock().read_line(&mut s) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {
                    let t = s.trim();
                    if t.is_empty() || t.starts_with('#') {
                        continue;
                    }
                    return Some(t.to_string());
                }
            }
        }
    }
}

impl Frontend for Term {
    fn stopped(&mut self, d: &mut Debugger, it: &mut Interp, s: &Stop) -> Resume {
        self.stopped_now = true;
        self.sel = 0;
        let fs = d.backtrace(d.threads.len() - 1);
        if let Some(f0) = fs.first() {
            let fj = Self::frame_json(d, f0);
            let src = d.source_line(&f0.file, f0.line);
            let mut t = match s.reason.as_str() {
                "panic" => format!("stopped: panic \"{}\" in {} at {}:{}", s.message, f0.name, rel(&f0.file), f0.line),
                r => format!("stopped: {}{} in {} at {}:{}", r, if s.bp_ids.is_empty() { String::new() } else { format!(" #{}", s.bp_ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")) }, f0.name, rel(&f0.file), f0.line),
            };
            if s.reason != "panic" && !s.message.is_empty() {
                t.push_str(&format!(" ({})", s.message));
            }
            t.push_str(&format!("\n  {} | {}", f0.line, src));
            self.emit(
                json!({"event": "stopped", "reason": s.reason, "message": s.message, "breakpoints": s.bp_ids,
                       "thread": d.cur_thread().id, "frame": fj}),
                t,
            );
        }
        self.last = Some(Stop { reason: s.reason.clone(), message: s.message.clone(), bp_ids: s.bp_ids.clone() });
        let r = loop {
            let Some(line) = Self::read_line() else { break Resume::Quit };
            match self.handle(d, it, &line) {
                Flow::Resume(r) => break r,
                Flow::Run | Flow::Stay => {}
            }
        };
        self.stopped_now = false;
        r
    }

    fn finished(&mut self, _d: &mut Debugger, _it: &mut Interp, o: &Outcome) {
        if o.quit {
            self.emit(json!({"event": "terminated", "reason": "quit"}), "quit".into());
            return;
        }
        if let Some(m) = &o.panic {
            self.emit(json!({"event": "output", "stream": "stderr", "text": format!("panic: {}\n", m)}), format!("panic: {}", m));
        }
        if let Some(m) = &o.error {
            self.emit(json!({"event": "output", "stream": "stderr", "text": format!("Error: {}\n", m)}), format!("Error: {}", m));
        }
        self.emit(json!({"event": "exited", "code": o.code}), format!("exited with code {}", o.code));
    }
}

/// `ksn debug [--json] [--test <FQN>] [-- <プログラムへの引数>]` / `ksn test --debug [--json] <FQN>`
pub fn cmd_debug(args: &[String]) -> i32 {
    let mut json_mode = false;
    let mut tree = false;
    let mut test: Option<String> = None;
    let mut prog_args: Vec<String> = vec![];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json_mode = true,
            "--engine=tree" => tree = true,
            "--engine=vm" => tree = false,
            "--test" => {
                i += 1;
                test = args.get(i).cloned();
            }
            "--" => {
                prog_args = args[i + 1..].to_vec();
                break;
            }
            a => prog_args.push(a.to_string()),
        }
        i += 1;
    }
    let tgt = match crate::cli::load_for_debug(tree) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };
    let run = match &test {
        Some(name) => match tgt.find_test(name) {
            Ok(ti) => Run::Test(ti),
            Err(e) => {
                eprintln!("ksn: {}", e);
                return 2;
            }
        },
        None => match tgt.entry {
            Some(di) => Run::Entry(di),
            None => {
                eprintln!("ksn: E9030: this package is a library (no `entry` in kasane.toml)");
                return 1;
            }
        },
    };
    if !tree {
        let target = match &run {
            Run::Entry(_) => crate::vm::DebugTarget::Entry(tgt.entry_name.clone()),
            Run::Test(ti) => crate::vm::DebugTarget::Test(*ti),
        };
        return match crate::vm::debug(&tgt.prog, &tgt.user, target, &prog_args, json_mode, tgt.user_files.clone(), None) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("ksn: debug on the VM is not available for this package: {} (use `--engine=tree`)", e);
                1
            }
        };
    }
    let mut it = Interp::new(tgt.prog.clone());
    it.args = prog_args;
    if json_mode {
        it.out = std::io::BufWriter::new(Box::new(EventSink::new(Box::new(|s| {
            println!("{}", json!({"event": "output", "stream": "stdout", "text": s}));
            let _ = std::io::stdout().flush();
        }))));
    }
    let mut d = Debugger::new(tgt.user_files.clone());
    let mut term = Term { json: json_mode, started: false, sel: 0, stopped_now: false, last: None };
    let what = match &run {
        Run::Entry(_) => tgt.entry_name.clone(),
        Run::Test(ti) => tgt.prog.defs[*ti].name.clone(),
    };
    term.emit(json!({"event": "ready", "target": what, "kind": if test.is_some() { "test" } else { "entry" }}), format!("ready: {} (commands: break run continue step next finish bt locals print quit)", what));
    loop {
        let Some(line) = Term::read_line() else { return 0 };
        match term.handle(&mut d, &mut it, &line) {
            Flow::Run => break,
            Flow::Resume(Resume::Quit) => {
                term.emit(json!({"event": "terminated", "reason": "quit"}), "quit".into());
                return 0;
            }
            _ => {}
        }
    }
    drive(it, d, Box::new(term), run)
}

// ------------------------------------------------------------ DAP

static SEQ: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);

fn dap_write(mut v: Value) {
    v["seq"] = json!(SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
    let body = v.to_string();
    let out = std::io::stdout();
    let mut o = out.lock();
    let _ = write!(o, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = o.flush();
}

fn dap_event(name: &str, body: Value) {
    dap_write(json!({"type": "event", "event": name, "body": body}));
}

fn dap_read() -> Option<Value> {
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if lock.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let l = line.trim_end();
        if l.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().ok();
            }
        }
    }
    let mut buf = vec![0u8; len?];
    lock.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

enum VarRef {
    Locals(usize, usize),
    Val(Val),
}

pub struct Dap {
    lines1: bool,
    cols1: bool,
    refs: Vec<VarRef>,
    stopped_now: bool,
    tgt_tests: Vec<String>,
}

enum DFlow {
    Stay,
    Run,
    Resume(Resume),
    Disconnect,
}

impl Dap {
    fn respond(req: &Value, ok: bool, body: Value, message: Option<&str>) {
        let mut r = json!({"type": "response", "request_seq": req["seq"], "success": ok, "command": req["command"], "body": body});
        if let Some(m) = message {
            r["message"] = json!(m);
        }
        dap_write(r);
    }

    fn line_out(&self, l: usize) -> usize {
        if self.lines1 { l } else { l.saturating_sub(1) }
    }
    fn col_out(&self, c: usize) -> usize {
        if self.cols1 { c } else { c.saturating_sub(1) }
    }
    fn line_in(&self, l: usize) -> usize {
        if self.lines1 { l } else { l + 1 }
    }

    fn bp_json(&self, b: &BpInfo) -> Value {
        let mut j = json!({"id": b.id, "verified": b.verified});
        if b.verified && b.kind == "line" {
            j["line"] = json!(self.line_out(b.line));
        }
        if !b.message.is_empty() {
            j["message"] = json!(b.message);
        }
        j
    }

    fn new_ref(&mut self, r: VarRef) -> i64 {
        self.refs.push(r);
        self.refs.len() as i64
    }

    fn var_json(&mut self, d: &mut Debugger, it: &mut Interp, name: &str, v: &Val) -> Value {
        let val = d.fmt(it, v);
        let has_kids = !children_of(v).is_empty();
        let r = if has_kids { self.new_ref(VarRef::Val(v.clone())) } else { 0 };
        json!({"name": name, "value": val, "variablesReference": r})
    }

    fn handle(&mut self, d: &mut Debugger, it: &mut Interp, req: &Value) -> DFlow {
        let cmd = req["command"].as_str().unwrap_or("");
        let a = &req["arguments"];
        let tp_cur = d.threads.len() - 1;
        match cmd {
            "setBreakpoints" => {
                let path = a["source"]["path"].as_str().unwrap_or("").to_string();
                d.clear_file_bps(&path);
                let mut out = vec![];
                let reqs: Vec<(usize, Option<String>)> = match a["breakpoints"].as_array() {
                    Some(bs) => bs.iter().map(|b| (b["line"].as_u64().unwrap_or(0) as usize, b["condition"].as_str().map(String::from))).collect(),
                    None => a["lines"].as_array().map(|ls| ls.iter().map(|l| (l.as_u64().unwrap_or(0) as usize, None)).collect()).unwrap_or_default(),
                };
                for (l, c) in reqs {
                    let info = d.set_line_bp(it, &path, self.line_in(l), c.as_deref());
                    out.push(self.bp_json(&info));
                }
                Self::respond(req, true, json!({"breakpoints": out}), None);
            }
            "setFunctionBreakpoints" => {
                d.clear_func_bps();
                let mut out = vec![];
                for b in a["breakpoints"].as_array().cloned().unwrap_or_default() {
                    let info = d.set_func_bp(it, b["name"].as_str().unwrap_or(""), b["condition"].as_str());
                    out.push(self.bp_json(&info));
                }
                Self::respond(req, true, json!({"breakpoints": out}), None);
            }
            "setExceptionBreakpoints" => {
                d.stop_on_panic = a["filters"].as_array().map_or(true, |f| f.iter().any(|x| x == "panic"));
                Self::respond(req, true, json!({}), None);
            }
            "configurationDone" => {
                Self::respond(req, true, json!({}), None);
                return DFlow::Run;
            }
            "threads" => {
                let ts: Vec<Value> = d.threads.iter().map(|t| json!({"id": t.id, "name": t.name})).collect();
                Self::respond(req, true, json!({"threads": ts}), None);
            }
            "stackTrace" => {
                let id = a["threadId"].as_i64().unwrap_or(1);
                let Some(tp) = d.thread_pos(id) else {
                    return {
                        Self::respond(req, false, json!({}), Some("unknown thread"));
                        DFlow::Stay
                    };
                };
                let fs = d.backtrace(tp);
                let start = a["startFrame"].as_u64().unwrap_or(0) as usize;
                let levels = a["levels"].as_u64().filter(|&n| n > 0).map_or(usize::MAX, |n| n as usize);
                let mut out = vec![];
                for f in fs.iter().skip(start).take(levels) {
                    let name = std::path::Path::new(&f.file).file_name().map_or(String::new(), |n| n.to_string_lossy().to_string());
                    out.push(json!({
                        "id": tp * 100000 + f.index + 1, "name": f.name,
                        "source": {"name": name, "path": f.file},
                        "line": self.line_out(f.line), "column": self.col_out(f.col.max(1)),
                        "presentationHint": if f.user { "normal" } else { "subtle" },
                    }));
                }
                Self::respond(req, true, json!({"stackFrames": out, "totalFrames": fs.len()}), None);
            }
            "scopes" => {
                let fid = a["frameId"].as_u64().unwrap_or(1) as usize - 1;
                let (tp, idx) = (fid / 100000, fid % 100000);
                let r = self.new_ref(VarRef::Locals(tp, idx));
                Self::respond(req, true, json!({"scopes": [{"name": "Locals", "variablesReference": r, "expensive": false}]}), None);
            }
            "variables" => {
                let r = a["variablesReference"].as_i64().unwrap_or(0);
                let kind = if r >= 1 { self.refs.get(r as usize - 1).map(|k| match k {
                    VarRef::Locals(t, i) => (Some((*t, *i)), None),
                    VarRef::Val(v) => (None, Some(v.clone())),
                }) } else { None };
                let mut out = vec![];
                match kind {
                    Some((Some((tp, idx)), _)) => {
                        for (k, v) in d.locals(it, tp, idx).unwrap_or_default() {
                            out.push(self.var_json(d, it, &k, &v));
                        }
                    }
                    Some((_, Some(v))) => {
                        for (k, c) in children_of(&v) {
                            out.push(self.var_json(d, it, &k, &c));
                        }
                    }
                    _ => {}
                }
                Self::respond(req, true, json!({"variables": out}), None);
            }
            "evaluate" => {
                let (tp, idx) = match a["frameId"].as_u64() {
                    Some(f) if f >= 1 => ((f as usize - 1) / 100000, (f as usize - 1) % 100000),
                    _ => (tp_cur, 0),
                };
                match d.eval_val(it, tp, idx, a["expression"].as_str().unwrap_or("")) {
                    Ok(v) => {
                        let s = d.fmt(it, &v);
                        let r = if children_of(&v).is_empty() { 0 } else { self.new_ref(VarRef::Val(v)) };
                        Self::respond(req, true, json!({"result": s, "variablesReference": r}), None);
                    }
                    Err(e) => Self::respond(req, false, json!({}), Some(&e)),
                }
            }
            "continue" | "next" | "stepIn" | "stepOut" => {
                if !self.stopped_now {
                    Self::respond(req, false, json!({}), Some("the program is not stopped"));
                    return DFlow::Stay;
                }
                let body = if cmd == "continue" { json!({"allThreadsContinued": true}) } else { json!({}) };
                Self::respond(req, true, body, None);
                return DFlow::Resume(match cmd {
                    "continue" => Resume::Continue,
                    "next" => Resume::Next,
                    "stepIn" => Resume::In,
                    _ => Resume::Out,
                });
            }
            "disconnect" | "terminate" => {
                Self::respond(req, true, json!({}), None);
                return DFlow::Disconnect;
            }
            "exceptionInfo" => {
                Self::respond(req, true, json!({"exceptionId": "panic", "breakMode": "always"}), None);
            }
            other => Self::respond(req, false, json!({}), Some(&format!("unsupported request `{}`", other))),
        }
        DFlow::Stay
    }
}

impl Frontend for Dap {
    fn stopped(&mut self, d: &mut Debugger, it: &mut Interp, s: &Stop) -> Resume {
        self.stopped_now = true;
        self.refs.clear();
        let reason = match s.reason.as_str() {
            "panic" => "exception",
            r => r,
        };
        let mut body = json!({"reason": reason, "threadId": d.cur_thread().id, "allThreadsStopped": true});
        if !s.message.is_empty() {
            body["text"] = json!(s.message);
            body["description"] = json!(if s.reason == "panic" { format!("panic: {}", s.message) } else { s.message.clone() });
        }
        if !s.bp_ids.is_empty() {
            body["hitBreakpointIds"] = json!(s.bp_ids);
        }
        if s.reason == "panic" {
            dap_event("output", json!({"category": "stderr", "output": format!("panic: {}\n", s.message)}));
        }
        dap_event("stopped", body);
        let r = loop {
            let Some(req) = dap_read() else { break Resume::Quit };
            match self.handle(d, it, &req) {
                DFlow::Resume(r) => break r,
                DFlow::Disconnect => break Resume::Quit,
                _ => {}
            }
        };
        self.stopped_now = false;
        self.refs.clear();
        r
    }

    fn finished(&mut self, _d: &mut Debugger, _it: &mut Interp, o: &Outcome) {
        if o.quit {
            return;
        }
        if let Some(m) = &o.panic {
            dap_event("output", json!({"category": "stderr", "output": format!("panic: {}\n", m)}));
        }
        if let Some(m) = &o.error {
            dap_event("output", json!({"category": "stderr", "output": format!("Error: {}\n", m)}));
        }
        dap_event("exited", json!({"exitCode": o.code}));
        dap_event("terminated", json!({}));
        while let Some(req) = dap_read() {
            let cmd = req["command"].as_str().unwrap_or("");
            if matches!(cmd, "disconnect" | "terminate") {
                Self::respond(&req, true, json!({}), None);
                break;
            }
            if cmd == "threads" {
                Self::respond(&req, true, json!({"threads": []}), None);
            } else {
                Self::respond(&req, false, json!({}), Some("the program has exited"));
            }
        }
    }
}

/// `ksn dap`: stdin/stdout で Debug Adapter Protocol を話す。
pub fn cmd_dap(tree: bool) -> i32 {
    let mut dap = Dap { lines1: true, cols1: true, refs: vec![], stopped_now: false, tgt_tests: vec![] };
    let mut launched: Option<(Interp, Debugger, Run)> = None;
    let mut stop_on_entry = false;
    while launched.is_none() {
        let Some(req) = dap_read() else { return 0 };
        let a = &req["arguments"];
        match req["command"].as_str().unwrap_or("") {
            "initialize" => {
                dap.lines1 = a["linesStartAt1"].as_bool().unwrap_or(true);
                dap.cols1 = a["columnsStartAt1"].as_bool().unwrap_or(true);
                Dap::respond(
                    &req,
                    true,
                    json!({
                        "supportsConfigurationDoneRequest": true,
                        "supportsFunctionBreakpoints": true,
                        "supportsConditionalBreakpoints": true,
                        "supportsEvaluateForHovers": true,
                        "supportsTerminateRequest": true,
                        "exceptionBreakpointFilters": [{"filter": "panic", "label": "Panics", "default": true}],
                    }),
                    None,
                );
            }
            "launch" => {
                if let Some(cwd) = a["cwd"].as_str() {
                    let _ = std::env::set_current_dir(cwd);
                }
                match crate::cli::load_for_debug(tree) {
                    Err(e) => {
                        Dap::respond(&req, false, json!({}), Some(&e));
                        dap_event("terminated", json!({}));
                        continue;
                    }
                    Ok(tgt) if !tree => {
                        let target = match a["test"].as_str() {
                            Some(name) => tgt.find_test(name).map(crate::vm::DebugTarget::Test),
                            None if tgt.entry.is_some() => Ok(crate::vm::DebugTarget::Entry(tgt.entry_name.clone())),
                            None => Err("this package is a library (no `entry` in kasane.toml)".to_string()),
                        };
                        let target = match target {
                            Ok(t) => t,
                            Err(e) => {
                                Dap::respond(&req, false, json!({}), Some(&e));
                                continue;
                            }
                        };
                        let args: Vec<String> = a["args"].as_array().map(|x| x.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default();
                        Dap::respond(&req, true, json!({}), None);
                        dap_event("initialized", json!({}));
                        let st = crate::vm::run_dap_state(dap.lines1, dap.cols1, a["stopOnEntry"].as_bool().unwrap_or(false));
                        return match crate::vm::debug(&tgt.prog, &tgt.user, target, &args, false, tgt.user_files.clone(), Some(st)) {
                            Ok(c) => c,
                            Err(e) => {
                                dap_event("output", json!({"category": "stderr", "output": format!("ksn: debug on the VM is not available for this package: {} (use `--engine=tree`)\n", e)}));
                                dap_event("terminated", json!({}));
                                1
                            }
                        };
                    }
                    Ok(tgt) => {
                        let run = match a["test"].as_str() {
                            Some(name) => tgt.find_test(name).map(Run::Test),
                            None => tgt.entry.map(Run::Entry).ok_or_else(|| "this package is a library (no `entry` in kasane.toml)".to_string()),
                        };
                        let run = match run {
                            Ok(r) => r,
                            Err(e) => {
                                Dap::respond(&req, false, json!({}), Some(&e));
                                continue;
                            }
                        };
                        let mut it = Interp::new(tgt.prog.clone());
                        it.args = a["args"].as_array().map(|x| x.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default();
                        it.out = std::io::BufWriter::new(Box::new(EventSink::new(Box::new(|s| {
                            dap_event("output", json!({"category": "stdout", "output": s}));
                        }))));
                        stop_on_entry = a["stopOnEntry"].as_bool().unwrap_or(false);
                        dap.tgt_tests = tgt.tests.iter().map(|&i| tgt.prog.defs[i].name.clone()).collect();
                        launched = Some((it, Debugger::new(tgt.user_files.clone()), run));
                        Dap::respond(&req, true, json!({}), None);
                        dap_event("initialized", json!({}));
                    }
                }
            }
            "disconnect" | "terminate" => {
                Dap::respond(&req, true, json!({}), None);
                return 0;
            }
            _ => Dap::respond(&req, false, json!({}), Some("send `launch` first")),
        }
    }
    let (mut it, mut d, run) = launched.unwrap();
    // 構成 (ブレークポイントなど) を受け取り、configurationDone で走らせる
    loop {
        let Some(req) = dap_read() else { return 0 };
        match dap.handle(&mut d, &mut it, &req) {
            DFlow::Run => break,
            DFlow::Disconnect => return 0,
            _ => {}
        }
    }
    if stop_on_entry {
        d.stop_on_entry();
    }
    dap_event("process", json!({"name": "ksn", "isLocalProcess": true, "startMethod": "launch"}));
    drive(it, d, Box::new(dap), run);
    0
}
