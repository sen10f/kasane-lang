//! 正規形(文法)検査 (stage0)。Python 参照実装 spec/tools/ebnfcheck.py の移植。
//! spec/grammar.ebnf を読み込み、ソースが `file` 規則で導出できるかを調べる。

use std::collections::{BTreeSet, HashMap, HashSet};

use regex::Regex;

const GRAMMAR: &str = include_str!("../../spec/grammar.ebnf");

const RESERVED: &[&str] = &[
    "fn", "type", "enum", "trait", "impl", "const", "static", "extern", "test", "end", "import", "let", "mut", "if",
    "else", "match", "loop", "while", "for", "in", "break", "continue", "return", "true", "false", "self", "Self",
    "pub", "uses", "panics", "unsafe", "move", "with",
    "dyn", "blanket", "never", "may", "async", "await", "where", "ref", "as", "i8", "i16", "i32", "i64",
    "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "bool", "char", "str",
];
const HEADER_KW: &[&str] = &["fn", "type", "enum", "trait", "impl", "const", "static", "extern", "test", "import", "end"];
const DEF_START: &[&str] = &["fn", "type", "enum", "trait", "test"];

/// Python の repr(str) 相当。
pub fn py_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

// ---------------------------------------------------------------- EBNF
#[derive(Debug, Clone)]
enum Node {
    Term(String),
    Ref(usize),
    Seq(Vec<Node>),
    Alt(Vec<Node>),
    Opt(Box<Node>),
    Rep(Box<Node>),
}

#[derive(Debug, Clone, PartialEq)]
enum ET {
    Str(String),
    Id(String),
    P(char),
}

fn strip_comments(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("(*") {
        out.push_str(&rest[..i]);
        let j = rest[i..].find("*)").unwrap() + i + 2;
        rest = &rest[j..];
    }
    out.push_str(rest);
    out
}

fn ebnf_tokens(s: &str) -> Vec<ET> {
    let cs: Vec<char> = s.chars().collect();
    let mut toks = vec![];
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '"' || c == '\'' {
            let j = cs[i + 1..].iter().position(|&x| x == c).unwrap() + i + 1;
            toks.push(ET::Str(cs[i + 1..j].iter().collect()));
            i = j + 1;
        } else if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < cs.len() && (cs[j].is_alphanumeric() || cs[j] == '_') {
                j += 1;
            }
            toks.push(ET::Id(cs[i..j].iter().collect()));
            i = j;
        } else if c == '.' && cs[i..].starts_with(&['.', '.', '.']) {
            i += 3; // "..." は範囲規則 (除外済み)
        } else {
            toks.push(ET::P(c));
            i += 1;
        }
    }
    toks
}

struct EbnfParser {
    t: Vec<ET>,
    i: usize,
    names: HashMap<String, usize>,
    order: Vec<String>,
}

impl EbnfParser {
    fn id_of(&mut self, n: &str) -> usize {
        if let Some(&i) = self.names.get(n) {
            return i;
        }
        let i = self.order.len();
        self.names.insert(n.to_string(), i);
        self.order.push(n.to_string());
        i
    }
    fn peek(&self) -> Option<&ET> {
        self.t.get(self.i)
    }
    fn alt(&mut self) -> Node {
        let mut alts = vec![self.seq()];
        while self.peek() == Some(&ET::P('|')) {
            self.i += 1;
            alts.push(self.seq());
        }
        if alts.len() == 1 { alts.remove(0) } else { Node::Alt(alts) }
    }
    fn seq(&mut self) -> Node {
        let mut items = vec![self.factor()];
        while self.peek() == Some(&ET::P(',')) {
            self.i += 1;
            items.push(self.factor());
        }
        if items.len() == 1 { items.remove(0) } else { Node::Seq(items) }
    }
    fn factor(&mut self) -> Node {
        let t = self.peek().cloned();
        match t {
            Some(ET::Str(s)) => {
                self.i += 1;
                Node::Term(s)
            }
            Some(ET::Id(n)) => {
                self.i += 1;
                Node::Ref(self.id_of(&n))
            }
            Some(ET::P('(')) => {
                self.i += 1;
                let r = self.alt();
                self.i += 1;
                r
            }
            Some(ET::P('[')) => {
                self.i += 1;
                let r = self.alt();
                self.i += 1;
                Node::Opt(Box::new(r))
            }
            Some(ET::P('{')) => {
                self.i += 1;
                let r = self.alt();
                self.i += 1;
                Node::Rep(Box::new(r))
            }
            other => panic!("unexpected {:?} in grammar", other),
        }
    }
}

fn load_grammar() -> (HashMap<usize, Node>, HashMap<String, usize>) {
    let mut src = strip_comments(GRAMMAR);
    let re = Regex::new(r"(lower|upper|letter|digit|hex|bin|oct)\s*=[^;]*;").unwrap();
    // 直前が識別子文字でないものだけを除く (範囲規則は字句層が担う)
    let mut out = String::new();
    let mut last = 0;
    for m in re.find_iter(&src) {
        let prev = src[..m.start()].chars().next_back();
        if prev.map_or(true, |c| !(c.is_ascii_alphabetic() || c == '_')) {
            out.push_str(&src[last..m.start()]);
            last = m.end();
        }
    }
    out.push_str(&src[last..]);
    src = out;
    let mut p = EbnfParser { t: ebnf_tokens(&src), i: 0, names: HashMap::new(), order: vec![] };
    let mut rules = HashMap::new();
    while p.i < p.t.len() {
        let ET::Id(name) = p.t[p.i].clone() else { panic!("rule name expected") };
        p.i += 1;
        p.i += 1; // '='
        let id = p.id_of(&name);
        let body = p.alt();
        p.i += 1; // ';'
        rules.insert(id, body);
    }
    (rules, p.names)
}

// ---------------------------------------------------------------- 字句
#[derive(Debug, Clone)]
struct Tok {
    kind: String,
    text: String,
    sp: bool,
}

struct Lexer {
    token_re: Regex,
    groups: Vec<&'static str>,
    ident_re: Regex,
    upper_re: Regex,
    u_esc: Regex,
}

impl Lexer {
    fn new() -> Lexer {
        let suffix = r"(?:i8|i16|i32|i64|i128|isize|u8|u16|u32|u64|u128|usize)";
        let fsuffix = r"(?:f32|f64)";
        let ch = r#"'(?:\\(?:x[0-7][0-9a-f]|u\{[0-9a-f]{1,6}\}|[\\ntr0'])|[^'\\\n])'"#;
        let st = r#""(?:\\(?:x[0-7][0-9a-f]|u\{[0-9a-f]{1,6}\}|[\\ntr0"])|[^"\\\n])*""#;
        let bch = r#"b'(?:\\(?:x[0-9a-f]{2}|[\\ntr0'])|[\x20-\x26\x28-\x5b\x5d-\x7e])'"#;
        let bst = r#"b"(?:\\(?:x[0-9a-f]{2}|[\\ntr0"])|[\x20\x21\x23-\x5b\x5d-\x7e])*""#;
        let groups = vec!["bstr", "bchr", "str", "chr", "life", "float", "int", "word", "op"];
        let pats = vec![
            bst.to_string(),
            bch.to_string(),
            st.to_string(),
            ch.to_string(),
            r"'[a-z_][a-z0-9_]*".to_string(),
            format!(r"[0-9]+\.[0-9]+(?:e[+-]?[0-9]+)?{f}?|[0-9]+e[+-]?[0-9]+{f}?", f = fsuffix),
            format!(r"(?:0x[0-9a-f_]+|0b[01_]+|0o[0-7_]+|[0-9][0-9_]*){}?", suffix),
            r"[A-Za-z_][A-Za-z0-9_]*".to_string(),
            r"::|->|=>|==|!=|<=|>=|&&|\|\||[-+*/%&|^!<>=(){}\[\],.:;?]".to_string(),
        ];
        let alt: Vec<String> = groups.iter().zip(&pats).map(|(g, p)| format!("(?P<{}>{})", g, p)).collect();
        Lexer {
            token_re: Regex::new(&format!("^(?:{})", alt.join("|"))).unwrap(),
            groups,
            ident_re: Regex::new(r"^_?[a-z][a-z0-9_]*$").unwrap(),
            upper_re: Regex::new(r"^[A-Z][A-Za-z0-9_]*$").unwrap(),
            u_esc: Regex::new(r"\\u\{([0-9a-f]{1,6})\}").unwrap(),
        }
    }

    fn lex_code(&self, line: &str) -> Result<Vec<Tok>, String> {
        let mut toks = vec![];
        let mut i = 0;
        let mut sp = false;
        while i < line.len() {
            let c = line[i..].chars().next().unwrap();
            if c == ' ' {
                i += 1;
                sp = true;
                continue;
            }
            let Some(caps) = self.token_re.captures(&line[i..]) else {
                let rest: String = line[i..].chars().take(20).collect();
                return Err(format!("lex error at {}", py_repr(&rest)));
            };
            let m = caps.get(0).unwrap();
            let kind = self.groups.iter().find(|g| caps.name(g).is_some()).unwrap();
            let t = m.as_str().to_string();
            i += m.end();
            if *kind == "str" || *kind == "chr" {
                for e in self.u_esc.captures_iter(&t) {
                    let v = u32::from_str_radix(&e[1], 16).unwrap_or(0);
                    if v > 0x10FFFF || (0xD800..=0xDFFF).contains(&v) {
                        return Err(format!("invalid \\u escape {}", &e[0]));
                    }
                }
            }
            if *kind == "int" {
                check_digit_groups(&t)?;
            }
            if *kind == "word" {
                let k = if RESERVED.contains(&t.as_str()) {
                    "kw"
                } else if self.ident_re.is_match(&t) {
                    "ident"
                } else if self.upper_re.is_match(&t) {
                    "upper"
                } else {
                    "word"
                };
                toks.push(Tok { kind: k.into(), text: t, sp });
            } else {
                toks.push(Tok { kind: kind.to_string(), text: t, sp });
            }
            sp = false;
        }
        Ok(toks)
    }
}

fn check_digit_groups(text: &str) -> Result<(), String> {
    let mut body = text;
    for s in ["i128", "u128", "isize", "usize", "i16", "i32", "i64", "u16", "u32", "u64", "i8", "u8"] {
        if let Some(b) = body.strip_suffix(s) {
            body = b;
            break;
        }
    }
    if !body.contains('_') {
        return Ok(());
    }
    let (digits, widths): (&str, Vec<usize>) = if body.starts_with("0x") || body.starts_with("0b") || body.starts_with("0o") {
        (&body[2..], vec![2, 4])
    } else {
        (body, vec![3])
    };
    let groups: Vec<&str> = digits.split('_').collect();
    if groups.iter().any(|g| g.is_empty()) {
        return Err(format!("bad digit separator in {}", text));
    }
    let w = groups[1].len();
    if !widths.contains(&w) || groups[1..].iter().any(|g| g.len() != w) || groups[0].len() > w {
        let ws = if widths.len() == 1 { format!("({},)", widths[0]) } else { format!("({}, {})", widths[0], widths[1]) };
        return Err(format!("digit groups of {} must be {}-wide", text, ws));
    }
    Ok(())
}

#[derive(Debug)]
struct Pre(String);

fn precheck(text: &str) -> Result<(), Pre> {
    if text.starts_with('\u{feff}') {
        return Err(Pre("BOM".into()));
    }
    if text.contains('\r') {
        return Err(Pre("CR (LF only)".into()));
    }
    if text.contains('\t') {
        return Err(Pre("TAB".into()));
    }
    if text.is_empty() || !text.ends_with('\n') || text.ends_with("\n\n") {
        return Err(Pre("file must end with exactly one LF".into()));
    }
    let lines: Vec<&str> = text.split('\n').collect();
    for (n, l) in lines[..lines.len() - 1].iter().enumerate() {
        let n = n + 1;
        if *l != l.trim_end_matches(' ') {
            return Err(Pre(format!("line {}: trailing space", n)));
        }
        let first = l.trim_start_matches(' ').split(|c| c == ' ' || c == '(' || c == '[').next().unwrap_or("");
        if l.chars().count() > 100 && !HEADER_KW.contains(&first) && !l.contains('"') {
            return Err(Pre(format!("line {}: longer than 100 chars", n)));
        }
    }
    Ok(())
}

fn lex_file(lx: &Lexer, text: &str, sig: bool) -> Result<Vec<Tok>, String> {
    precheck(text).map_err(|e| e.0)?;
    let all: Vec<&str> = text.split('\n').collect();
    let lines = &all[..all.len() - 1];
    let info: Vec<(usize, &str, &str)> = lines.iter().enumerate().map(|(i, l)| (i + 1, *l, l.trim())).collect();
    let mut out: Vec<Tok> = vec![];
    let mut inside = false;
    let mut prev_comment = false;
    let mk = |k: &str, t: &str| Tok { kind: k.into(), text: t.into(), sp: false };
    for (idx, (n, l, s)) in info.iter().enumerate() {
        if s.is_empty() {
            if prev_comment {
                return Err(format!("line {}: blank line after comment", n));
            }
            if inside {
                out.push(mk("BLANK", ""));
            }
            prev_comment = false;
            continue;
        }
        let head = if !l.starts_with(' ') { l.split(' ').next().unwrap_or("") } else { "" };
        if DEF_START.contains(&head) && !(sig && head == "fn") {
            inside = true;
        }
        if s.starts_with("///") {
            out.push(mk("kw", "///"));
            out.push(mk("text", &s[3..]));
            out.push(mk("NL", ""));
            prev_comment = true;
            continue;
        }
        if s.starts_with("//") {
            out.push(mk("kw", "//"));
            out.push(mk("text", &s[2..]));
            out.push(mk("NL", ""));
            prev_comment = !l.starts_with(' ');
            continue;
        }
        prev_comment = false;
        let code = lx.lex_code(s).map_err(|e| format!("line {}: {}", n, e))?;
        let nxt = info[idx + 1..].iter().find(|x| !x.2.is_empty() && !x.2.starts_with("//"));
        let inhibit = s.ends_with('(')
            || s.ends_with('[')
            || s.ends_with(',')
            || nxt.map_or(false, |x| x.2.starts_with('.') && !x.2.starts_with(".."));
        out.extend(code);
        out.push(mk(if inhibit { "LF" } else { "NL" }, ""));
        if head == "end" {
            inside = false;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- マッチング
type PosSet = BTreeSet<usize>;

struct Matcher<'a> {
    rules: &'a HashMap<usize, Node>,
    names: Vec<String>,
    toks: &'a [Tok],
    lx: &'a Lexer,
    memo: HashMap<(usize, usize), PosSet>,
    term_cache: HashMap<String, Vec<String>>,
    #[allow(dead_code)]
    zero: HashSet<usize>,
}

impl<'a> Matcher<'a> {
    fn term_seq(&mut self, s: &str) -> Vec<String> {
        if let Some(v) = self.term_cache.get(s) {
            return v.clone();
        }
        let v = if s == "///" || s == "//" {
            vec![s.to_string()]
        } else {
            self.lx.lex_code(s).map(|t| t.into_iter().map(|x| x.text).collect()).unwrap_or_default()
        };
        self.term_cache.insert(s.to_string(), v.clone());
        v
    }

    fn one(p: usize) -> PosSet {
        let mut s = BTreeSet::new();
        s.insert(p + 1);
        s
    }

    fn override_rule(&self, name: &str, p: usize) -> Option<PosSet> {
        let t = self.toks.get(p);
        if matches!(name, "INDENT" | "INDENT_BACK" | "CONT_INDENT" | "CLOSE_INDENT") {
            return Some([p].into_iter().collect());
        }
        if matches!(name, "NL" | "LF" | "BLANK") {
            return Some(if t.map_or(false, |t| t.kind == name) { Self::one(p) } else { BTreeSet::new() });
        }
        let Some(t) = t else { return Some(BTreeSet::new()) };
        let hit = |c: bool| Some(if c { Self::one(p) } else { BTreeSet::new() });
        match name {
            "ident" => hit(t.kind == "ident"),
            "upper_ident" => hit(t.kind == "upper"),
            "lifetime" => hit(t.kind == "life"),
            "int_lit" => {
                let mut q = p;
                if t.text == "-" && t.kind == "op" {
                    q = p + 1;
                }
                match self.toks.get(q) {
                    Some(u) if u.kind == "int" && !(q > p && u.sp) => Some([q + 1].into_iter().collect()),
                    _ => Some(BTreeSet::new()),
                }
            }
            "int_dec" => hit(t.kind == "int" && !t.text.is_empty() && t.text.chars().next().unwrap().is_ascii_digit()
                && t.text.chars().all(|c| c.is_ascii_digit() || c == '_')),
            "float_lit" => hit(t.kind == "float"),
            "char_lit" => hit(t.kind == "chr"),
            "string" => hit(t.kind == "str"),
            "byte_lit" => hit(t.kind == "bchr"),
            "byte_string" => hit(t.kind == "bstr"),
            "text" | "char_any" | "ascii" => hit(t.kind == "text"),
            _ => None,
        }
    }

    fn m(&mut self, node: &Node, p: usize) -> PosSet {
        match node {
            Node::Term(s) => {
                let seq = self.term_seq(s);
                let mut q = p;
                for x in &seq {
                    match self.toks.get(q) {
                        Some(t) if t.text == *x && !matches!(t.kind.as_str(), "str" | "chr" | "bstr" | "bchr" | "text") => q += 1,
                        _ => return BTreeSet::new(),
                    }
                }
                [q].into_iter().collect()
            }
            Node::Ref(id) => self.rule(*id, p),
            Node::Seq(items) => {
                let mut cur: PosSet = [p].into_iter().collect();
                for it in items {
                    let mut nxt = BTreeSet::new();
                    for &c in &cur {
                        nxt.extend(self.m(it, c));
                    }
                    cur = nxt;
                    if cur.is_empty() {
                        return cur;
                    }
                }
                cur
            }
            Node::Alt(items) => {
                let mut r = BTreeSet::new();
                for it in items {
                    r.extend(self.m(it, p));
                }
                r
            }
            Node::Opt(n) => {
                let mut r = self.m(n, p);
                r.insert(p);
                r
            }
            Node::Rep(n) => {
                let mut ends: PosSet = [p].into_iter().collect();
                let mut frontier: PosSet = [p].into_iter().collect();
                while !frontier.is_empty() {
                    let mut new = BTreeSet::new();
                    for &c in &frontier {
                        new.extend(self.m(n, c));
                    }
                    new = new.difference(&ends).cloned().collect();
                    ends.extend(new.iter().cloned());
                    frontier = new;
                }
                ends
            }
        }
    }

    fn rule(&mut self, id: usize, p: usize) -> PosSet {
        if let Some(r) = self.memo.get(&(id, p)) {
            return r.clone();
        }
        let name = self.names[id].clone();
        if let Some(ov) = self.override_rule(&name, p) {
            self.memo.insert((id, p), ov.clone());
            return ov;
        }
        let rules: &'a HashMap<usize, Node> = self.rules;
        let Some(body) = rules.get(&id) else { return BTreeSet::new() };
        self.memo.insert((id, p), BTreeSet::new());
        let r = self.m(body, p);
        self.memo.insert((id, p), r.clone());
        r
    }
}

pub struct Grammar {
    rules: HashMap<usize, Node>,
    names: Vec<String>,
    file_id: usize,
    lx: Lexer,
    _zero: HashSet<usize>,
}

impl Grammar {
    pub fn new() -> Grammar {
        let (rules, ids) = load_grammar();
        let mut names = vec![String::new(); ids.len()];
        for (n, i) in &ids {
            names[*i] = n.clone();
        }
        Grammar { file_id: ids["file"], rules, names, lx: Lexer::new(), _zero: HashSet::new() }
    }

    /// (一致したか, メッセージ, 行)。Python 版 cli.grammar_check と同じ形。
    pub fn check(&self, text: &str) -> (bool, String, usize) {
        let toks = match lex_file(&self.lx, text, false) {
            Ok(t) => t,
            Err(e) => return (false, e, 1),
        };
        let mut m = Matcher {
            rules: &self.rules,
            names: self.names.clone(),
            toks: &toks,
            lx: &self.lx,
            memo: HashMap::new(),
            term_cache: HashMap::new(),
            zero: HashSet::new(),
        };
        let ends = m.rule(self.file_id, 0);
        if ends.contains(&toks.len()) {
            return (true, String::new(), 0);
        }
        let mut best = 0;
        for set in m.memo.values() {
            if let Some(&mx) = set.iter().next_back() {
                best = best.max(mx);
            }
        }
        let t = &toks[best.min(toks.len().saturating_sub(1))];
        let shown = if t.text.is_empty() { t.kind.clone() } else { t.text.clone() };
        (false, format!("does not match the canonical grammar near {}", py_repr(&shown)), 1)
    }
}
