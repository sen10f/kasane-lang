//! LSP の意味トークン (`textDocument/semanticTokens/full`) と補完 (`textDocument/completion`)。
//! トークンは開いている文書の字句から作る (`.ksnh` も `.ksnl` もそのまま)。名前の解決は読み込んだソース (`.ksnh`) の
//! 行で引くので、`.ksnl` を開いているときは `ksnh_line_for` で行を直す。

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::*;
use crate::program::Target;

pub const TOKEN_TYPES: &[&str] = &[
    "namespace", "type", "enum", "enumMember", "interface", "function", "method", "parameter", "variable", "property", "keyword",
    "macro", "number", "typeParameter",
];
pub const TOKEN_MODS: &[&str] = &["declaration", "readonly"];

const T_NS: u32 = 0;
const T_TYPE: u32 = 1;
const T_ENUM: u32 = 2;
const T_VARIANT: u32 = 3;
const T_TRAIT: u32 = 4;
const T_FN: u32 = 5;
const T_METHOD: u32 = 6;
const T_PARAM: u32 = 7;
const T_VAR: u32 = 8;
const T_PROP: u32 = 9;
const T_KW: u32 = 10;
const T_EFFECT: u32 = 11;
const T_NUM: u32 = 12;
const T_TPARAM: u32 = 13;
const M_DECL: u32 = 1;
const M_RO: u32 = 2;

const INT_SUFFIXES: &[&str] = &["i128", "u128", "isize", "usize", "i16", "i32", "i64", "u16", "u32", "u64", "i8", "u8", "f32", "f64"];
const HEADER_KWS: &[&str] = &["fn", "type", "enum", "trait", "test", "const", "static", "extern", "impl"];

/// 文書の位置から、名前解決に使う (ソースのパス, `.ksnh` 側の行 (0 始まり)) を求める。
struct View {
    root: PathBuf,
    /// 読み込んだソース (`.ksnh`) のパス。
    path: String,
    doc_path: String,
    text: String,
}

fn enclosing<'a>(prog: &'a Program, file: &str, line1: usize) -> Option<&'a Def> {
    prog.defs.iter().filter(|d| d.file == file && (d.kw == "fn" || d.kw == "test") && d.line <= line1 && line1 <= d.end_line).max_by_key(|d| d.line)
}

/// `d` の引数 `name` の型の所有者のキー (ヘッダの文字列から取る)。
fn param_owner(prog: &Program, file: &str, d: &Def, name: &str) -> Option<String> {
    if name == "self" && d.is_method() {
        if let Some((o, _)) = d.name.rsplit_once('.') {
            return Some(o.to_string());
        }
    }
    let h = &d.header;
    let open = h.find('(')?;
    let mut depth = 0i32;
    let mut parts: Vec<String> = vec![String::new()];
    for c in h[open + 1..].chars() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            ',' if depth == 0 => {
                parts.push(String::new());
                continue;
            }
            _ => {}
        }
        parts.last_mut().unwrap().push(c);
    }
    let part = parts.iter().find_map(|p| p.trim().strip_prefix(name).and_then(|r| r.strip_prefix(':')))?;
    let mut ty = part.trim();
    loop {
        let t = ty.trim_start_matches('&').trim_start();
        let t = t.strip_prefix("mut ").unwrap_or(t);
        let t = if t.starts_with('\'') { t.split_once(' ').map_or(t, |x| x.1) } else { t };
        if t == ty {
            break;
        }
        ty = t;
    }
    let base: String = ty.chars().take_while(|c| is_ident(*c) || *c == '.').collect();
    if base.is_empty() {
        return None;
    }
    if let Some(o) = prim_owner(&base) {
        return Some(o);
    }
    let segs: Vec<String> = base.split('.').map(String::from).collect();
    match prog.resolve(&segs, file) {
        Some(r) if r.rest.is_empty() => match r.target {
            Target::Type(i) => Some(prog.defs[i].name.clone()),
            _ => None,
        },
        _ => None,
    }
}

impl Server {
    fn view(&mut self, uri: &str) -> Result<Option<View>, (i64, String)> {
        let Some(root) = self.root_for(uri)? else { return Ok(None) };
        let d = self.doc(uri)?;
        let (doc_path, text) = (d.path.clone(), d.text.clone());
        let path = ksnh_sibling(&doc_path).unwrap_or_else(|| doc_path.clone());
        Ok(Some(View { root, path, doc_path, text }))
    }

    pub(super) fn semantic_tokens(&mut self, p: &Value) -> Resp {
        let uri = param_uri(p)?;
        let empty = Ok(json!({"data": []}));
        let Some(v) = self.view(&uri)? else { return empty };
        let Ok(toks) = lexer::lex(&v.text) else { return empty };
        self.ensure_tabs(&v.root);
        let (Some((_, prog)), Some(tabs)) = (self.ctx_ro(&v.root), self.roots.get(&v.root).and_then(|s| s.tabs.as_ref())) else { return empty };
        let lines = lines_of(&v.text);
        let mut kmap: HashMap<usize, usize> = HashMap::new();
        let mut kline = |ln1: usize| -> usize {
            *kmap.entry(ln1).or_insert_with(|| if v.path != v.doc_path { ksnh_line_for(&v.path, ln1 - 1, lines.get(ln1 - 1).copied().unwrap_or("")) + 1 } else { ln1 })
        };
        // (行 1 始まり, 桁 1 始まり (文字), 文字数, 種類, 修飾)
        let mut out: Vec<(usize, usize, usize, u32, u32)> = vec![];
        let n = toks.len();
        let mut i = 0;
        let mut line_first: Option<(usize, String)> = None;
        while i < n {
            let t = &toks[i];
            if t.kind == "nl" || t.kind == "eof" {
                i += 1;
                continue;
            }
            let first_kw = match &line_first {
                Some((l, k)) if *l == t.line => k.clone(),
                _ => {
                    let mut k = String::new();
                    for x in toks[i..].iter().take_while(|x| x.line == t.line) {
                        if x.kind == "kw" && x.text != "pub" && x.text != "unsafe" {
                            k = x.text.clone();
                            break;
                        }
                    }
                    line_first = Some((t.line, k.clone()));
                    k
                }
            };
            match t.kind {
                "int" | "float" => {
                    let len = t.text.chars().count();
                    let suf = INT_SUFFIXES.iter().find(|s| t.text.ends_with(*s) && !t.text.starts_with("0x"));
                    match suf {
                        Some(s) => {
                            out.push((t.line, t.col, len - s.len(), T_NUM, 0));
                            out.push((t.line, t.col + len - s.len(), s.len(), T_TYPE, 0));
                        }
                        None => out.push((t.line, t.col, len, T_NUM, 0)),
                    }
                    i += 1;
                }
                "life" => {
                    out.push((t.line, t.col, t.text.chars().count(), T_TPARAM, 0));
                    i += 1;
                }
                "kw" if t.text == "uses" => {
                    out.push((t.line, t.col, 4, T_KW, 0));
                    i += 1;
                    if i < n && toks[i].text == "[" {
                        i += 1;
                        while i < n && toks[i].text != "]" && toks[i].kind != "nl" {
                            let e = &toks[i];
                            if matches!(e.kind, "ident" | "upper" | "kw") {
                                let ty = if e.kind == "upper" { T_TPARAM } else { T_EFFECT };
                                out.push((e.line, e.col, e.text.chars().count(), ty, 0));
                            }
                            i += 1;
                        }
                    }
                }
                "kw" if t.text == "Self" => {
                    out.push((t.line, t.col, 4, T_TYPE, 0));
                    i += 1;
                }
                "kw" if t.text == "self" => {
                    out.push((t.line, t.col, 4, T_PARAM, 0));
                    i += 1;
                }
                "kw" => {
                    out.push((t.line, t.col, t.text.chars().count(), T_KW, 0));
                    i += 1;
                    // `import a.b` と `end a.b` / 宣言 `fn a.b` は、直後の経路を下で扱う。
                    if matches!(t.text.as_str(), "import" | "end" | "fn" | "type" | "enum" | "trait" | "const" | "static" | "test") {
                        let (segs, next) = path_at(&toks, i);
                        if !segs.is_empty() && !(t.text == "test" && segs.is_empty()) {
                            let (last, decl) = (segs.len() - 1, t.text != "import");
                            for (k, s) in segs.iter().enumerate() {
                                let (ty, md) = if k < last || t.text == "import" {
                                    (T_NS, 0)
                                } else {
                                    let full: String = segs.iter().map(|s| s.2.as_str()).collect::<Vec<_>>().join(".");
                                    match t.text.as_str() {
                                        "fn" | "end" => {
                                            let m = prog.funcs.get(&full).map_or(false, |&d| prog.defs[d].is_method());
                                            (if m { T_METHOD } else { T_FN }, if decl && t.text == "fn" { M_DECL } else { 0 })
                                        }
                                        "type" => (T_TYPE, M_DECL),
                                        "enum" => (T_ENUM, M_DECL),
                                        "trait" => (T_TRAIT, M_DECL),
                                        "const" | "static" => (T_VAR, M_DECL | M_RO),
                                        _ => (T_FN, 0),
                                    }
                                };
                                out.push((s.0, s.1, s.2.chars().count(), ty, md));
                            }
                            i = next;
                        }
                    }
                }
                "ident" | "upper" | "prim" => {
                    let ctx_kw = HEADER_KWS.contains(&first_kw.as_str());
                    let prev_dot = i > 0 && toks[i - 1].text == "." && toks[i - 1].line == t.line;
                    if t.kind == "ident" && lexer::is_contextual(&t.text) && ctx_kw && !prev_dot && !(i + 1 < n && toks[i + 1].text == "(") {
                        out.push((t.line, t.col, t.text.chars().count(), T_KW, 0));
                        i += 1;
                        continue;
                    }
                    if t.kind == "ident" && t.text == "capture" && i + 1 < n && toks[i + 1].text == "[" {
                        out.push((t.line, t.col, 7, T_KW, 0));
                        i += 1;
                        continue;
                    }
                    let (segs, next) = path_at(&toks, i);
                    let kl = kline(t.line);
                    let def = enclosing(prog, &v.path, kl);
                    let call_next = next < n && toks[next].text == "(" && toks[next].line == toks[next - 1].line;
                    classify(prog, tabs, &v.path, def, kl, &segs, call_next, &mut out);
                    i = next;
                }
                _ => i += 1,
            }
        }
        out.sort();
        out.dedup_by(|b, a| a.0 == b.0 && a.1 == b.1);
        let mut data: Vec<u32> = vec![];
        let (mut pl, mut pc) = (0usize, 0usize);
        for (ln, col, len, ty, md) in out {
            let l0 = ln - 1;
            let Some(ltxt) = lines.get(l0) else { continue };
            let c16 = utf16_of(ltxt, col - 1);
            let end16 = utf16_of(ltxt, col - 1 + len);
            let dl = l0.saturating_sub(pl);
            let dc = if dl == 0 { c16.saturating_sub(pc) } else { c16 };
            data.extend([dl as u32, dc as u32, (end16 - c16) as u32, ty, md]);
            pl = l0;
            pc = c16;
        }
        Ok(json!({"data": data}))
    }

    pub(super) fn completion(&mut self, p: &Value) -> Resp {
        let uri = param_uri(p)?;
        let (line, ch) = param_pos(p)?;
        let Some(v) = self.view(&uri)? else { return Ok(json!([])) };
        self.ensure_tabs(&v.root);
        let (Some((l, prog)), Some(tabs)) = (self.ctx_ro(&v.root), self.roots.get(&v.root).and_then(|s| s.tabs.as_ref())) else { return Ok(json!([])) };
        let lines = lines_of(&v.text);
        let Some(ltxt) = lines.get(line) else { return Ok(json!([])) };
        let cs: Vec<char> = ltxt.chars().collect();
        let idx = scalar_of(ltxt, ch).min(cs.len());
        let kline0 = if v.path != v.doc_path { ksnh_line_for(&v.path, line, ltxt) } else { line };
        let def = enclosing(prog, &v.path, kline0 + 1);
        let before: String = cs[..idx].iter().collect();
        // 候補: ラベル → (種類, 詳細)
        let mut items: BTreeMap<String, (u32, String)> = BTreeMap::new();
        // `uses [` の中。
        if let Some(p) = before.rfind("uses [") {
            if !before[p..].contains(']') {
                let mut effs: Vec<String> = ["alloc", "io", "io.console", "io.fs", "io.net", "process", "time", "thread"].iter().map(|s| s.to_string()).collect();
                for d in &prog.defs {
                    effs.extend(d.uses.iter().filter(|e| !crate::check::is_effect_var(e)).cloned());
                }
                for e in effs {
                    items.insert(e, (20, "effect".into()));
                }
                return Ok(finish(items));
            }
        }
        let mut a = idx;
        while a > 0 && is_ident(cs[a - 1]) {
            a -= 1;
        }
        if a > 0 && cs[a - 1] == '.' {
            let mut b = a - 1;
            while b > 0 && (is_ident(cs[b - 1]) || cs[b - 1] == '.') {
                b -= 1;
            }
            let recv: String = cs[b..a - 1].iter().collect();
            let segs: Vec<String> = recv.split('.').map(String::from).collect();
            if segs.iter().any(|s| s.is_empty()) {
                return Ok(json!([]));
            }
            // 局所・引数の上のメソッド。
            let mut owner: Option<String> = None;
            if segs.len() == 1 {
                if let Some(d) = def {
                    if segs[0] == "self" || d.params.iter().any(|p| p.0 == segs[0]) {
                        owner = param_owner(prog, &v.path, d, &segs[0]);
                    }
                    if owner.is_none() {
                        if let Some(es) = tabs.locals.get(&(v.path.clone(), d.name.clone(), segs[0].clone())) {
                            owner = es.iter().filter(|e| e.0 <= kline0 + 1).last().and_then(|e| e.1.clone());
                        }
                    }
                }
            }
            if let Some(o) = owner {
                members(prog, &o, true, &mut items);
                return Ok(finish(items));
            }
            // モジュールの中身。
            let mname = prog.expand(&segs, &v.path).join(".");
            let srcs = Server::sources(l);
            let pre = format!("{}.", mname);
            let mut is_mod = false;
            for s in &srcs {
                if let Some(rest) = s.module.strip_prefix(&pre) {
                    is_mod = true;
                    items.insert(rest.split('.').next().unwrap().to_string(), (9, format!("module {}", pre.to_string() + rest.split('.').next().unwrap())));
                }
                if s.module == mname {
                    is_mod = true;
                }
            }
            for d in &prog.defs {
                if let Some(rest) = d.name.strip_prefix(&pre) {
                    if !rest.contains('.') && !rest.is_empty() {
                        is_mod = true;
                        items.insert(rest.to_string(), (def_kind(d), d.header.clone()));
                    }
                }
            }
            if is_mod {
                return Ok(finish(items));
            }
            // 型の上 (`Point.new`、列挙子)。
            if let Some(r) = prog.resolve(&segs, &v.path) {
                if r.rest.is_empty() {
                    if let Target::Type(i) = r.target {
                        members(prog, &prog.defs[i].name, false, &mut items);
                    }
                }
            }
            return Ok(finish(items));
        }
        // 識別子: 局所・引数・import したモジュール名。
        if let Some(d) = def {
            for p in &d.params {
                if p.0 != "self" || d.is_method() {
                    items.insert(p.0.clone(), (6, "parameter".into()));
                }
            }
            for ((f, dn, name), es) in &tabs.locals {
                if f == &v.path && dn == &d.name && es.iter().any(|e| e.0 <= kline0 + 1) {
                    items.insert(name.clone(), (6, "local".into()));
                }
            }
        }
        if let Some(imps) = prog.imports.get(&v.path) {
            for (k, full) in imps {
                items.insert(k.clone(), (9, format!("module {}", full)));
            }
        }
        Ok(finish(items))
    }
}

fn def_kind(d: &Def) -> u32 {
    match d.kw.as_str() {
        "fn" if d.is_method() => 2,
        "fn" => 3,
        "type" => 22,
        "enum" => 13,
        "trait" => 8,
        "const" | "static" => 21,
        _ => 6,
    }
}

fn finish(items: BTreeMap<String, (u32, String)>) -> Value {
    Value::Array(items.into_iter().map(|(k, (kind, detail))| json!({"label": k, "kind": kind, "detail": detail})).collect())
}

/// 型 (所有者のキー) のメソッド (`methods`)、または関連関数・列挙子・欄 (`!methods`) を足す。
fn members(prog: &Program, owner: &str, methods: bool, items: &mut BTreeMap<String, (u32, String)>) {
    let dotted = format!(".{}", owner);
    for d in &prog.defs {
        if d.kw != "fn" {
            continue;
        }
        let Some((o, m)) = d.name.rsplit_once('.') else { continue };
        if (o == owner || o.ends_with(&dotted)) && d.is_method() == methods {
            items.insert(m.to_string(), (def_kind(d), d.header.clone()));
        }
    }
    for d in &prog.defs {
        if d.name == owner || d.name.ends_with(&dotted) {
            if methods {
                for f in &d.fields {
                    items.insert(f.0.clone(), (5, format!("field of {}", short_ty(&d.name))));
                }
            } else if d.kw == "enum" {
                for v in &d.variants {
                    items.insert(v.0.clone(), (20, format!("variant of {}", d.name)));
                }
            }
        }
    }
}

/// `toks[i]` から始まる `a.b.c` の経路: (行, 桁, 文字列) の列と、次のトークンの添字。
fn path_at(toks: &[Tok], mut i: usize) -> (Vec<(usize, usize, String)>, usize) {
    let mut segs = vec![];
    let ok = |t: &Tok| matches!(t.kind, "ident" | "upper" | "prim" | "int") || (t.kind == "kw" && matches!(t.text.as_str(), "self" | "Self"));
    if i >= toks.len() || !ok(&toks[i]) {
        return (segs, i);
    }
    segs.push((toks[i].line, toks[i].col, toks[i].text.clone()));
    while i + 2 < toks.len() {
        let (a, d, b) = (&toks[i], &toks[i + 1], &toks[i + 2]);
        if d.text == "." && d.kind == "op" && d.line == a.line && d.col == a.col + a.text.chars().count() && b.line == a.line && b.col == d.col + 1 && ok(b) {
            segs.push((b.line, b.col, b.text.clone()));
            i += 2;
        } else {
            break;
        }
    }
    (segs, i + 1)
}

fn target_kind(prog: &Program, t: &Target) -> (u32, u32) {
    match t {
        Target::Fn(i) | Target::Sig(i) => (if prog.defs[*i].is_method() { T_METHOD } else { T_FN }, 0),
        Target::Const(_) => (T_VAR, M_RO),
        Target::Type(i) => (
            match prog.defs[*i].kw.as_str() {
                "enum" => T_ENUM,
                "trait" => T_TRAIT,
                _ => T_TYPE,
            },
            0,
        ),
        Target::Variant(..) => (T_VARIANT, 0),
        Target::Native(_) => (T_FN, 0),
        Target::Value(_) => (T_VAR, 0),
    }
}

#[allow(clippy::too_many_arguments)]
fn classify(prog: &Program, tabs: &Tabs, file: &str, def: Option<&Def>, kline: usize, segs: &[(usize, usize, String)], call_next: bool, out: &mut Vec<(usize, usize, usize, u32, u32)>) {
    let names: Vec<String> = segs.iter().map(|s| s.2.clone()).collect();
    let mut push = |k: usize, ty: u32, md: u32| out.push((segs[k].0, segs[k].1, segs[k].2.chars().count(), ty, md));
    let local = def.map_or(None, |d| {
        if d.params.iter().any(|p| p.0 == names[0]) {
            Some(T_PARAM)
        } else if tabs.locals.contains_key(&(file.to_string(), d.name.clone(), names[0].clone())) {
            Some(T_VAR)
        } else {
            None
        }
    });
    // メソッド呼び出し / 欄: 解決表に載っていれば呼び出し。
    let member = |k: usize| -> u32 {
        let hit = [(segs[k].1, usize::MAX), (segs[0].1, k)].iter().any(|&(c, s)| tabs.sites.contains_key(&(file.to_string(), crate::parser::pack_pos(kline, c), s)));
        if hit || (call_next && k == segs.len() - 1) {
            T_METHOD
        } else {
            T_PROP
        }
    };
    if let Some(ty) = local {
        push(0, ty, 0);
        for k in 1..segs.len() {
            let m = member(k);
            push(k, m, 0);
        }
        return;
    }
    for k in (1..=segs.len()).rev() {
        if let Some(r) = prog.resolve(&names[..k], file) {
            if r.rest.is_empty() {
                let (ty, md) = target_kind(prog, &r.target);
                for j in 0..k - 1 {
                    let ns = if matches!(r.target, Target::Variant(..)) && j == k - 2 { T_ENUM } else { T_NS };
                    push(j, ns, 0);
                }
                push(k - 1, ty, md);
                for j in k..segs.len() {
                    let m = member(j);
                    push(j, m, 0);
                }
                return;
            }
        }
    }
    let n = segs.len();
    if n == 1 {
        let ty = if prog.imports.get(file).map_or(false, |m| m.contains_key(&names[0])) {
            T_NS
        } else if lexer::PRIMS.contains(&names[0].as_str()) || names[0].chars().next().map_or(false, |c| c.is_uppercase()) {
            T_TYPE
        } else if call_next {
            T_FN
        } else {
            T_VAR
        };
        push(0, ty, 0);
        return;
    }
    // 先頭がモジュールなら途中は名前空間、そうでなければ値の欄・メソッド。
    let mut k = n;
    while k > 0 && !prog.defs.iter().any(|d| d.module == prog.expand(&names[..k], file).join(".")) {
        k -= 1;
    }
    for j in 0..n {
        if j < k {
            push(j, T_NS, 0);
        } else if j == 0 {
            push(0, T_VAR, 0);
        } else {
            let m = member(j);
            push(j, m, 0);
        }
    }
}
