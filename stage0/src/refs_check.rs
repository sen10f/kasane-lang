//! 参照は二級の値 (仕様 4.1、`設計_二級の参照.md`、E3060)。
//!
//! 参照を運ぶ型を、型の欄・容れ物・チャネルに入れる形を、型の宣言と型の使用の位置から拒む。借用の流れ
//! (`borrowck.rs`) は見ず、字句 (`lexer`) と推論済みの型 (`types::Ty`) だけを読む。
//! - 型の宣言 (`type` / `enum`): ヘッダのライフタイム引数、欄・ペイロードの参照を運ぶ型。
//! - 容れ物・総称の型の型引数: ヘッダ・`let` の注釈・式中の型引数のどこに書いても (山括弧の対応を見る)。
//! - 参照を取り込んだクロージャ (`capture [&x]`) を、欄・容れ物・チャネル・`spawn` に渡す形。
//! - 推論で決まった `let` の型 (`let v = Vec::new(); v.push(&x)`)。
//! 領域 (ライフタイム) の流れは見ない。`&'static T` は借用ではないので許す。決まらない所は拒む側 (保守側)。

use std::collections::{HashMap, HashSet};

use crate::lexer::{lex, Tok};
use crate::program::Program;
use crate::types::Ty;

/// 診断 1 件 (定義の添字、行と列を符号化した位置、メッセージ)。
pub struct Hit {
    pub di: usize,
    pub pos: usize,
    pub msg: String,
}

/// 標準の容れ物・共有・送受信の型 (型名の末尾)。型引数に参照を運ぶ型を入れられない。
const CONTAINERS: &[&str] = &[
    "Vec", "HashMap", "BtreeMap", "VecDeque", "MapEntry", "Box", "Buf", "Rc", "Arc", "Mutex", "RwLock", "Sender", "Receiver",
    "SendError", "JoinHandle", "Task", "Cell", "RefCell",
];
/// 参照を運ぶ標準の型 (末尾 2 つの名前で照合する)。
const RC_STD: &[(&str, &str)] =
    &[("slice", "Iter"), ("str", "Chars"), ("str", "Split"), ("collections", "HashMapIter"), ("sync", "MutexGuard")];
/// 値を保存・送る関数・メソッドの名前 (クロージャを渡すと E3060)。
const STORE_FNS: &[&str] = &["push", "insert", "push_back", "push_front", "send", "try_send", "spawn", "spawn_local", "into_dyn", "set", "put"];
const STORE_NEW: &[&str] = &["Box", "Rc", "Arc", "Mutex", "RwLock", "Cell", "RefCell"];

fn is_op(t: &Tok, s: &str) -> bool {
    t.kind == "op" && t.text == s
}

fn is_path_tok(t: &Tok) -> bool {
    matches!(t.kind, "ident" | "upper")
}

/// `toks[end]` を最後の語とするパス (`a.b.C`) の語の並び。
fn path_ending_at(toks: &[Tok], end: usize) -> Vec<&str> {
    let mut segs = vec![toks[end].text.as_str()];
    let mut k = end;
    while k >= 2 && is_op(&toks[k - 1], ".") && is_path_tok(&toks[k - 2]) {
        segs.push(toks[k - 2].text.as_str());
        k -= 2;
    }
    segs.reverse();
    segs
}

/// `toks[open]` が開き括弧 (`(` `[` `<` `{`) のとき、対応する閉じの次の添字 (見つからなければ `toks.len()`)。
fn skip_group(toks: &[Tok], open: usize) -> usize {
    let (o, c) = match toks[open].text.as_str() {
        "(" => ("(", ")"),
        "[" => ("[", "]"),
        "<" => ("<", ">"),
        _ => ("{", "}"),
    };
    let mut depth = 0i32;
    let mut i = open;
    while i < toks.len() {
        if is_op(&toks[i], o) {
            depth += 1;
        } else if is_op(&toks[i], c) {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        } else if toks[i].kind == "nl" && o == "<" {
            return i;
        }
        i += 1;
    }
    toks.len()
}

/// 型の位置の語の並び `toks[a..b]` に、参照を運ぶ型があるか。あれば最初の語の添字。
/// `fn(..) -> T` の型と `dyn Fn<..>` / `Fn<(&T,), ..>` の引数は、保存される値ではないので読み飛ばす。
fn rc_in(toks: &[Tok], a: usize, b: usize) -> Option<usize> {
    let mut i = a;
    while i < b {
        let t = &toks[i];
        if t.kind == "op" && (t.text == "&" || t.text == "&&") {
            if i + 1 < b && toks[i + 1].kind == "life" && toks[i + 1].text == "'static" {
                i += 2;
                continue;
            }
            return Some(i);
        }
        if t.kind == "life" {
            if t.text != "'static" {
                return Some(i);
            }
            i += 1;
            continue;
        }
        if t.kind == "kw" && t.text == "fn" && i + 1 < b && is_op(&toks[i + 1], "(") {
            i = skip_group(toks, i + 1);
            if i < b && is_op(&toks[i], "->") {
                i += 1;
                let mut depth = 0i32;
                while i < b {
                    let x = &toks[i];
                    if x.kind == "op" && matches!(x.text.as_str(), "(" | "[" | "<") {
                        depth += 1;
                    } else if x.kind == "op" && matches!(x.text.as_str(), ")" | "]" | ">") {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    } else if depth == 0 && (x.kind == "nl" || (x.kind == "op" && x.text == ",") || (x.kind == "kw" && matches!(x.text.as_str(), "uses" | "panics")) || x.text == "capture") {
                        break;
                    }
                    i += 1;
                }
            }
            continue;
        }
        if is_path_tok(t) {
            let segs = path_ending_at(toks, i);
            let last = *segs.last().unwrap();
            let next_lt = i + 1 < b && is_op(&toks[i + 1], "<");
            if matches!(last, "Fn" | "FnMut" | "FnOnce") && next_lt {
                i = skip_group(toks, i + 1);
                continue;
            }
            let is_path_end = !(i + 1 < toks.len() && is_op(&toks[i + 1], "."));
            if is_path_end && segs.len() >= 2 && RC_STD.iter().any(|(m, n)| segs[segs.len() - 2] == *m && last == *n) {
                return Some(i);
            }
        }
        if t.kind == "kw" && t.text == "dyn" {
            // `dyn Tr<..>` の引数は読み飛ばす (`+ 'a` は次の周で見る)。
            let mut j = i + 1;
            while j < b && (is_path_tok(&toks[j]) || is_op(&toks[j], ".")) {
                j += 1;
            }
            if j < b && is_op(&toks[j], "<") {
                j = skip_group(toks, j);
            }
            i = j;
            continue;
        }
        i += 1;
    }
    None
}

/// 総称の `type` / `enum` の型名の末尾 (利用者の定義だけ)。
fn user_generic_names(p: &Program, user: &[usize]) -> HashSet<String> {
    let mut out = HashSet::new();
    for &i in user {
        let d = &p.defs[i];
        if matches!(d.kw.as_str(), "type" | "enum") && d.generics.iter().any(|g| g.0 == "type") {
            if let Some(n) = d.name.rsplit('.').next() {
                out.insert(n.to_string());
            }
        }
    }
    out
}

struct Scan<'a> {
    toks: Vec<Tok>,
    base_line: usize,
    di: usize,
    user_generic: &'a HashSet<String>,
    out: Vec<Hit>,
    seen: HashSet<(usize, &'static str)>,
}

impl<'a> Scan<'a> {
    fn hit(&mut self, kind: &'static str, tok: usize, msg: String) {
        let t = &self.toks[tok];
        let line = self.base_line + t.line - 1;
        if self.seen.insert((line, kind)) {
            self.out.push(Hit { di: self.di, pos: crate::parser::pack_pos(line, t.col), msg });
        }
    }

    /// `type` / `enum`: ヘッダのライフタイム引数と、欄・ペイロードの参照を運ぶ型。
    fn type_decl(&mut self, name: &str) {
        let toks = std::mem::take(&mut self.toks);
        self.toks = toks.clone();
        let mut i = 0;
        // ヘッダ (最初の nl まで)
        while i < toks.len() && toks[i].kind != "nl" {
            if toks[i].kind == "life" {
                self.hit(
                    "decl",
                    i,
                    format!(
                        "{}: a type cannot declare a lifetime parameter (references are second-class values); hold a position index and pass the source data as an argument instead",
                        name
                    ),
                );
                break;
            }
            i += 1;
        }
        // 本体: 1 行ずつ、先頭の名前 (欄名・バリアント名) を除いて読む
        let mut line_start = i + 1;
        while line_start < toks.len() {
            let mut line_end = line_start;
            while line_end < toks.len() && toks[line_end].kind != "nl" {
                line_end += 1;
            }
            if line_end > line_start + 1 {
                if let Some(k) = rc_in(&toks, line_start + 1, line_end) {
                    self.hit(
                        "field",
                        k,
                        format!(
                            "{}: a field or variant payload cannot hold a reference (references are second-class values); hold an index or an owned value, and pass the source data as an argument",
                            name
                        ),
                    );
                }
            }
            line_start = line_end + 1;
        }
    }

    /// 容れ物・利用者の総称の型の型引数 (どの位置でも)。
    fn containers(&mut self) {
        let n = self.toks.len();
        let mut i = 0;
        while i < n {
            let t = &self.toks[i];
            if is_path_tok(t) {
                let mut j = i + 1;
                // `Vec::<T>` (式中の型引数) は `::` を挟む
                if j < n && is_op(&self.toks[j], "::") {
                    j += 1;
                }
                let end_of_path = !(i + 1 < n && is_op(&self.toks[i + 1], "."));
                if end_of_path && j < n && is_op(&self.toks[j], "<") {
                    let last = t.text.clone();
                    if CONTAINERS.contains(&last.as_str()) || self.user_generic.contains(&last) {
                        let close = skip_group(&self.toks, j);
                        if let Some(k) = rc_in(&self.toks, j + 1, close.saturating_sub(1)) {
                            self.hit(
                                "container",
                                k,
                                format!(
                                    "`{}<..>` cannot hold a reference (references are second-class values); use an owned value or an index (`&'static T` is allowed)",
                                    last
                                ),
                            );
                        }
                    }
                }
            }
            i += 1;
        }
    }

    /// 参照を取り込んだクロージャを欄・容れ物・チャネルへ渡す形。
    fn closures(&mut self) {
        let n = self.toks.len();
        let mut tainted: HashSet<String> = HashSet::new();
        let mut i = 0;
        // クロージャ式 = `fn ( .. ) -> T capture [ .. ]`。`capture` を持たない `fn(..)` は型
        let mut lits: Vec<(usize, bool)> = Vec::new();
        while i < n {
            if self.toks[i].kind == "kw" && self.toks[i].text == "fn" && i + 1 < n && is_op(&self.toks[i + 1], "(") {
                let mut j = skip_group(&self.toks, i + 1);
                // 戻り値の型を `capture` まで読む (同じ行。括弧の中の `,` や `{` は深さで読み飛ばす)
                let mut depth = 0i32;
                while j < n && self.toks[j].kind != "nl" {
                    let x = &self.toks[j];
                    if x.kind == "op" && matches!(x.text.as_str(), "(" | "[" | "<") {
                        depth += 1;
                    } else if x.kind == "op" && matches!(x.text.as_str(), ")" | "]" | ">") {
                        depth -= 1;
                        if depth < 0 {
                            break;
                        }
                    } else if depth == 0 && (x.text == "capture" || is_op(x, "{") || is_op(x, ",")) {
                        break;
                    }
                    j += 1;
                }
                if j + 1 < n && self.toks[j].text == "capture" && is_op(&self.toks[j + 1], "[") {
                    let close = skip_group(&self.toks, j + 1);
                    let by_ref = self.toks[j + 1..close].iter().any(|t| is_op(t, "&") || is_op(t, "&&"));
                    lits.push((i, by_ref));
                }
            }
            i += 1;
        }
        for (start, by_ref) in lits {
            if !by_ref {
                continue;
            }
            // 直前の語で使われ方を決める
            let prev = if start > 0 { Some(start - 1) } else { None };
            let Some(pv) = prev else { continue };
            if is_op(&self.toks[pv], "(") || is_op(&self.toks[pv], ",") {
                if let Some(callee) = self.enclosing_callee(pv) {
                    if self.is_store(&callee) {
                        self.hit("closure", start, closure_msg());
                    }
                }
            } else if is_op(&self.toks[pv], ":") && pv > 0 && is_path_tok(&self.toks[pv - 1]) {
                // 構造体リテラルの欄の初期化 (`name: fn(..) capture [&x] {..}`)
                self.hit("closure", start, closure_msg());
            } else if is_op(&self.toks[pv], "=") && pv >= 2 && is_path_tok(&self.toks[pv - 1]) {
                tainted.insert(self.toks[pv - 1].text.clone());
            }
        }
        if tainted.is_empty() {
            return;
        }
        for i in 0..n {
            if is_path_tok(&self.toks[i]) && tainted.contains(&self.toks[i].text) && i > 0 {
                let pv = i - 1;
                if is_op(&self.toks[pv], "(") || is_op(&self.toks[pv], ",") {
                    if let Some(callee) = self.enclosing_callee(pv) {
                        if self.is_store(&callee) {
                            self.hit("closure", i, closure_msg());
                        }
                    }
                } else if is_op(&self.toks[pv], ":") && pv > 0 && is_path_tok(&self.toks[pv - 1]) {
                    self.hit("closure", i, closure_msg());
                }
            }
        }
    }

    /// `toks[pos]` を含む括弧の呼び出し先 (`a.b.push(`/`Box.new(` のパス)。決まらなければ `None`。
    fn enclosing_callee(&self, pos: usize) -> Option<Vec<String>> {
        let mut depth = 0i32;
        let mut k = pos as isize;
        while k >= 0 {
            let t = &self.toks[k as usize];
            if t.kind == "op" && matches!(t.text.as_str(), ")" | "]" | "}") {
                depth += 1;
            } else if t.kind == "op" && matches!(t.text.as_str(), "(" | "[" | "{") {
                if depth == 0 {
                    if t.text == "(" && k >= 1 && is_path_tok(&self.toks[k as usize - 1]) {
                        let segs = path_ending_at(&self.toks, k as usize - 1);
                        return Some(segs.iter().map(|s| s.to_string()).collect());
                    }
                    return None;
                }
                depth -= 1;
            } else if t.kind == "nl" && depth == 0 {
                // 括弧の外 (複数行の呼び出しは 1 行目まで戻って見る)
            }
            k -= 1;
        }
        None
    }

    fn is_store(&self, callee: &[String]) -> bool {
        let last = callee.last().map(|s| s.as_str()).unwrap_or("");
        if STORE_FNS.contains(&last) {
            return true;
        }
        if last == "new" && callee.len() >= 2 {
            return STORE_NEW.contains(&callee[callee.len() - 2].as_str());
        }
        false
    }
}

fn closure_msg() -> String {
    "a closure that captures a reference (`capture [&x]` / `[&mut x]`) cannot be stored in a field, a container, or a channel, and cannot be spawned (references are second-class values); capture by `move` an owned value instead".to_string()
}

/// 推論で決まった型 (`let v = Vec::new(); v.push(&x)` の `Vec<&u32>`) が、容れ物に参照を入れているか。
/// 型には領域が残らないので、静的な文字列・配列の可能性が高い `&str` / `&[T]` は見逃す
/// (`&'static` かどうかは書かれた型だけが決める。流れは `borrowck.rs` の E3010 が見る)。
pub fn inferred_container_ref(ty: &Ty) -> Option<(String, bool)> {
    /// 戻り値の真偽: Some(weak)。weak = 参照の先が `str` / スライス / 配列だけ (リテラルなら `'static` でありうる)。
    fn has_ref(ty: &Ty) -> Option<bool> {
        match ty {
            Ty::Ref(_, inner) => Some(matches!(**inner, Ty::Prim(ref s) if s == "str") || matches!(**inner, Ty::Slice(_) | Ty::Array(_, _))),
            Ty::Tuple(xs) => xs.iter().filter_map(has_ref).fold(None, |a, w| Some(a.unwrap_or(true) && w)),
            Ty::Named(n, args) => {
                let last = n.rsplit('.').next().unwrap_or("");
                let segs: Vec<&str> = n.split('.').collect();
                if segs.len() >= 2 && RC_STD.iter().any(|(m, k)| segs[segs.len() - 2] == *m && last == *k) {
                    return Some(false);
                }
                args.iter().filter_map(has_ref).fold(None, |a, w| Some(a.unwrap_or(true) && w))
            }
            Ty::Array(x, _) | Ty::Slice(x) => has_ref(x),
            _ => None,
        }
    }
    match ty {
        Ty::Named(n, args) => {
            let last = n.rsplit('.').next().unwrap_or("");
            if CONTAINERS.contains(&last) {
                if let Some(w) = args.iter().filter_map(has_ref).fold(None, |a, w| Some(a.unwrap_or(true) && w)) {
                    return Some((last.to_string(), w));
                }
            }
            args.iter().find_map(inferred_container_ref)
        }
        Ty::Tuple(xs) => xs.iter().find_map(inferred_container_ref),
        Ty::Ref(_, x) | Ty::Array(x, _) | Ty::Slice(x) => inferred_container_ref(x),
        _ => None,
    }
}

/// `let [mut] name = a.b.f(..)` の `f` が、戻り値に `&'static` を書いた関数 (`-> Vec<&'static str, A>`)。
fn init_from_static_fn(p: &Program, toks: &[Tok], name: &str) -> bool {
    for i in 0..toks.len().saturating_sub(4) {
        let mut j = i;
        if !(toks[j].kind == "kw" && toks[j].text == "let") {
            continue;
        }
        j += 1;
        if toks[j].kind == "kw" && toks[j].text == "mut" {
            j += 1;
        }
        if !(is_path_tok(&toks[j]) && toks[j].text == name && j + 2 < toks.len() && is_op(&toks[j + 1], "=")) {
            continue;
        }
        let mut k = j + 2;
        let mut path = String::new();
        while k < toks.len() && (is_path_tok(&toks[k]) || is_op(&toks[k], ".")) {
            path.push_str(&toks[k].text);
            k += 1;
        }
        if k < toks.len() && is_op(&toks[k], "(") {
            let suffix = format!(".{}", path);
            return p.defs.iter().any(|d| {
                d.kw == "fn" && (d.name == path || d.name.ends_with(&suffix)) && d.header.split("->").nth(1).map_or(false, |r| r.contains("&'static"))
            });
        }
    }
    false
}

/// `name.push(..)` / `insert` / `push_back` / `push_front` の引数がすべて文字列リテラルで始まり、1 回以上ある
/// (= 入る参照が `'static` だと示せる)。
fn static_stores_only(p: &Program, toks: &[Tok], name: &str) -> bool {
    if init_from_static_fn(p, toks, name) {
        return true;
    }
    let mut n = 0;
    let mut i = 0;
    while i + 4 < toks.len() {
        if is_path_tok(&toks[i]) && toks[i].text == name && is_op(&toks[i + 1], ".") && matches!(toks[i + 2].text.as_str(), "push" | "insert" | "push_back" | "push_front") && is_op(&toks[i + 3], "(") {
            let close = skip_group(toks, i + 3);
            // 最上位の引数ごとの先頭の語 (最初の引数が `&alloc` などのアロケータ引数でも、最後の引数を見る)
            let mut depth = 0;
            let mut starts = vec![i + 4];
            for k in i + 4..close.saturating_sub(1) {
                if toks[k].kind == "op" && matches!(toks[k].text.as_str(), "(" | "[" | "{" | "<") {
                    depth += 1;
                } else if toks[k].kind == "op" && matches!(toks[k].text.as_str(), ")" | "]" | "}" | ">") {
                    depth -= 1;
                } else if depth == 0 && is_op(&toks[k], ",") {
                    starts.push(k + 1);
                }
            }
            // どの引数もリテラルで始まり (変数・参照は `'static` と示せない)、文字列リテラルが 1 つ以上ある
            let lit = |k: usize| k < toks.len() && matches!(toks[k].kind, "str" | "int" | "float" | "char" | "byte" | "bstr");
            if !starts.iter().all(|&k| lit(k) || (k < toks.len() && toks[k].kind == "kw" && matches!(toks[k].text.as_str(), "true" | "false"))) || !starts.iter().any(|&k| k < toks.len() && toks[k].kind == "str") {
                return false;
            }
            n += 1;
        }
        i += 1;
    }
    n > 0
}

/// 利用者の定義 (`user`) を検査する。`let_tys` は推論済みの `let` の型 (`(定義, 行, 名前)`)。
pub fn check(p: &Program, user: &[usize], let_tys: &HashMap<(usize, usize, String), Ty>) -> Vec<Hit> {
    let generic = user_generic_names(p, user);
    let mut cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
    let mut out: Vec<Hit> = Vec::new();
    let mut flagged: HashSet<(usize, usize)> = HashSet::new();
    let mut toks_of: HashMap<usize, Vec<Tok>> = HashMap::new();
    for &di in user {
        let d = &p.defs[di];
        if d.file.ends_with(".sig") || d.line == 0 {
            continue;
        }
        let lines = cache.entry(d.file.clone()).or_insert_with(|| std::fs::read_to_string(&d.file).ok().map(|s| s.split('\n').map(|l| l.to_string()).collect()));
        let Some(lines) = lines else { continue };
        let end = if d.end_line >= d.line { d.end_line } else { d.line };
        if d.line > lines.len() {
            continue;
        }
        let text = lines[d.line - 1..end.min(lines.len())].join("\n") + "\n";
        let Ok(toks) = lex(&text) else { continue };
        toks_of.insert(di, toks.clone());
        let mut sc = Scan { toks, base_line: d.line, di, user_generic: &generic, out: Vec::new(), seen: HashSet::new() };
        match d.kw.as_str() {
            "type" | "enum" => sc.type_decl(&d.name),
            "fn" | "test" | "impl" | "const" | "static" | "trait" => {
                sc.containers();
                sc.closures();
            }
            _ => {}
        }
        for h in sc.out {
            flagged.insert((di, crate::parser::unpack_pos(h.pos).0));
            out.push(h);
        }
    }
    // 推論で決まった型 (書かれた型で拒んだ行は重ねない)
    let mut keys: Vec<_> = let_tys.keys().collect();
    keys.sort();
    for (di, line, name) in keys {
        if !user.contains(di) || flagged.contains(&(*di, crate::parser::unpack_pos(*line).0)) {
            continue;
        }
        let ty = &let_tys[&(*di, *line, name.clone())];
        if let Some((c, weak)) = inferred_container_ref(ty) {
            if weak && toks_of.get(di).map_or(false, |t| static_stores_only(p, t, name)) {
                continue;
            }
            out.push(Hit {
                di: *di,
                pos: *line,
                msg: format!(
                    "`{}` has the inferred type `{}<..>` holding a reference (references are second-class values) and `'static` cannot be shown; annotate it (`let {}: alloc.vec.Vec<&'static str, ..> = ..`, only for string literals and `'static` data) or hold an owned value (`String`) or an index",
                    name, c, name
                ),
            });
        }
    }
    out
}
