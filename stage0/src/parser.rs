//! 式・文・パターン・型の構文解析 (stage0)。Python 参照実装 ksn/syntax.py の Parser と同じ構造を作る。
//! AST は動的な木 (V) で持ち、dump() が Python 側 ksn/dump.py と同じ文字列を返す。

use crate::lexer::Tok;

#[derive(Debug, Clone, Default)]
pub enum V {
    S(String),
    N(String),
    B(bool),
    L(Vec<V>),
    /// (種別, 子, 行, 式 ID)。式 ID は `number_nodes` が本体ごとに振る (0 = 未採番)。
    Node(&'static str, Vec<V>, usize, u32),
    #[default]
    Nil,
}

impl V {
    pub fn dump(&self) -> String {
        match self {
            V::S(s) | V::N(s) => s.clone(),
            V::B(b) => (if *b { "true" } else { "false" }).to_string(),
            V::Nil => "-".to_string(),
            V::L(xs) => format!("[{}]", xs.iter().map(|x| x.dump()).collect::<Vec<_>>().join(" ")),
            V::Node(n, fs, _, _) => {
                let mut s = format!("({}", n);
                let fs = if *n == "Lit" && matches!(fs.first(), Some(V::S(k)) if k == "float") { &fs[..2] } else { &fs[..] };
                for f in fs {
                    s.push(' ');
                    s.push_str(&f.dump());
                }
                s.push(')');
                s
            }
        }
    }
}

pub(crate) fn s(x: &str) -> V {
    V::S(x.to_string())
}
pub(crate) fn nd(n: &'static str, fs: Vec<V>) -> V {
    V::Node(n, fs, 0, 0)
}
/// 位置の符号化: 上位が行、下位 20 bit が列 (1 始まり、Unicode スカラ値)。
/// 列を持たない素の行番号 (< 1<<20) もそのまま「列不明」の位置として扱える。
pub(crate) const COL_BITS: usize = 20;
pub(crate) fn pack_pos(line: usize, col: usize) -> usize {
    (line << COL_BITS) | col.min((1 << COL_BITS) - 1)
}
/// 位置 `p` を (行, 列) に分ける。列が無ければ 0。
pub(crate) fn unpack_pos(p: usize) -> (usize, usize) {
    if p < (1 << COL_BITS) {
        (p, 0)
    } else {
        (p >> COL_BITS, p & ((1 << COL_BITS) - 1))
    }
}
/// 中置・後置ノードの位置。左項が同じ行で始まるなら、その開始位置 (項の先頭) を使う。
fn anchor(p: usize, left: &V) -> usize {
    let lp = match left {
        V::Node(_, _, l, _) => *l,
        _ => 0,
    };
    if lp != 0 && unpack_pos(lp).0 == unpack_pos(p).0 {
        lp
    } else {
        p
    }
}
pub(crate) fn ndl(line: usize, n: &'static str, fs: Vec<V>) -> V {
    V::Node(n, fs, line, 0)
}
/// 式 ID (`V::Node` の第 4 成分)。未採番・ノード以外は 0。
pub fn nid(v: &V) -> u32 {
    match v {
        V::Node(_, _, _, id) => *id,
        _ => 0,
    }
}
/// 木を前順で辿り、ノードに 1 から順に式 ID を振る。`next` は次に振る番号 (本体ごとに 1 から始める)。
/// 同じソースからは常に同じ番号になる。書き換えでノードを足すときは既存の番号を振り直さず、
/// `max + 1` 以降を使う (設計_KIR.md 7 章 5)。
pub(crate) fn number_nodes(v: &mut V, next: &mut u32) {
    match v {
        V::Node(_, fs, _, id) => {
            *next += 1;
            *id = *next;
            for f in fs.iter_mut() {
                number_nodes(f, next);
            }
        }
        V::L(xs) => {
            for x in xs.iter_mut() {
                number_nodes(x, next);
            }
        }
        _ => {}
    }
}
pub(crate) fn strs(xs: &[String]) -> V {
    V::L(xs.iter().map(|x| V::S(x.clone())).collect())
}
pub(crate) fn opt(x: Option<V>) -> V {
    x.unwrap_or(V::Nil)
}

pub(crate) type R<T> = Result<T, String>;

const INT_SUFFIXES: &[&str] = &[
    "i128", "u128", "isize", "usize", "i16", "i32", "i64", "u16", "u32", "u64", "i8", "u8",
];

pub struct Parser {
    pub(crate) t: Vec<Tok>,
    pub(crate) i: usize,
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

pub(crate) fn unescape(inner: &str) -> Result<Vec<char>, String> {
    let cs: Vec<char> = inner.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] != '\\' {
            out.push(cs[i]);
            i += 1;
            continue;
        }
        let c = *cs.get(i + 1).ok_or("bad escape")?;
        match c {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '\\' => out.push('\\'),
            '0' => out.push('\0'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'x' => {
                let h: String = cs[i + 2..i + 4].iter().collect();
                out.push(char::from_u32(u32::from_str_radix(&h, 16).map_err(|e| e.to_string())?).unwrap());
                i += 4;
                continue;
            }
            'u' => {
                let end = cs[i..].iter().position(|&x| x == '}').ok_or("bad \\u")? + i;
                let h: String = cs[i + 3..end].iter().collect();
                out.push(char::from_u32(u32::from_str_radix(&h, 16).map_err(|e| e.to_string())?).ok_or("bad scalar")?);
                i = end + 1;
                continue;
            }
            _ => return Err(format!("bad escape \\{}", c)),
        }
        i += 2;
    }
    Ok(out)
}

/// 整数/浮動小数トークンを (値の文字列, 接尾辞) に分ける。
pub(crate) fn split_number(text: &str, is_float: bool) -> R<(String, Option<String>)> {
    let neg = text.starts_with('-');
    let body = text.trim_start_matches('-');
    let is_hex = body.starts_with("0x");
    let mut suf: Option<String> = None;
    let mut core = body.to_string();
    let cands: Vec<&str> = if is_float || !is_hex {
        INT_SUFFIXES.iter().copied().chain(["f32", "f64"]).collect()
    } else {
        INT_SUFFIXES.to_vec()
    };
    let mut best: Option<&str> = None;
    for sfx in cands {
        if body.ends_with(sfx) && body.len() > sfx.len() && best.map_or(true, |b| sfx.len() > b.len()) {
            if is_hex || !body[..body.len() - sfx.len()].ends_with(|c: char| c.is_alphabetic() && c != 'e') {
                best = Some(sfx);
            }
        }
    }
    if let Some(b) = best {
        suf = Some(b.to_string());
        core = body[..body.len() - b.len()].to_string();
    }
    let core = core.replace('_', "");
    if is_float {
        return Ok((core, suf));
    }
    let v: u128 = if let Some(h) = core.strip_prefix("0x") {
        u128::from_str_radix(h, 16)
    } else if let Some(b) = core.strip_prefix("0b") {
        u128::from_str_radix(b, 2)
    } else if let Some(o) = core.strip_prefix("0o") {
        u128::from_str_radix(o, 8)
    } else {
        core.parse::<u128>()
    }
    .map_err(|e| format!("bad integer {}: {}", text, e))?;
    let sign = if neg && v != 0 { "-" } else { "" };
    Ok((format!("{}{}", sign, v), suf))
}

impl Parser {
    pub fn new(t: Vec<Tok>) -> Parser {
        Parser { t, i: 0 }
    }

    pub(crate) fn cur(&self) -> &Tok {
        &self.t[self.i]
    }
    pub(crate) fn peek(&self, k: usize) -> &Tok {
        &self.t[(self.i + k).min(self.t.len() - 1)]
    }
    pub(crate) fn err<T>(&self, msg: &str) -> R<T> {
        let c = self.cur();
        Err(format!("E0002|{}|{}|{} (found {})", c.line, c.col, msg, crate::grammar::py_repr(&c.text)))
    }
    pub(crate) fn is_op(&self, o: &str) -> bool {
        let c = self.cur();
        c.kind == "op" && c.text == o
    }
    pub(crate) fn is_kw(&self, k: &str) -> bool {
        let c = self.cur();
        (c.kind == "kw" && c.text == k) || (c.kind == "ident" && c.text == k && crate::lexer::is_contextual(k))
    }
    pub(crate) fn accept_op(&mut self, o: &str) -> bool {
        if self.is_op(o) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    pub(crate) fn accept_kw(&mut self, k: &str) -> bool {
        if self.is_kw(k) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    pub(crate) fn expect_op(&mut self, o: &str) -> R<()> {
        if !self.is_op(o) {
            return self.err(&format!("expected {}", crate::grammar::py_repr(o)));
        }
        self.i += 1;
        Ok(())
    }
    pub(crate) fn expect_kw(&mut self, k: &str) -> R<()> {
        if !self.is_kw(k) {
            return self.err(&format!("expected {}", crate::grammar::py_repr(k)));
        }
        self.i += 1;
        Ok(())
    }
    pub fn skip_nl(&mut self) {
        while self.cur().kind == "nl" {
            self.i += 1;
        }
    }
    pub(crate) fn expect_nl(&mut self) -> R<()> {
        if self.cur().kind != "nl" && self.cur().kind != "eof" {
            return self.err("expected end of line");
        }
        self.skip_nl();
        Ok(())
    }
    pub fn at_eof(&self) -> bool {
        self.cur().kind == "eof"
    }

    pub(crate) fn name_tok(&mut self) -> R<String> {
        let c = self.cur().clone();
        if matches!(c.kind, "ident" | "upper" | "prim") {
            self.i += 1;
            return Ok(c.text);
        }
        if c.kind == "kw" && matches!(c.text.as_str(), "blanket" | "self" | "Self") {
            self.i += 1;
            return Ok(c.text);
        }
        self.err("expected a name")
    }

    pub(crate) fn dot_continues(&self) -> bool {
        self.is_op(".")
            && (matches!(self.peek(1).kind, "ident" | "upper" | "prim")
                || (self.peek(1).kind == "kw" && self.peek(1).text == "blanket"))
    }

    pub(crate) fn fqn(&mut self) -> R<String> {
        let mut segs = vec![self.name_tok()?];
        while self.dot_continues() {
            self.i += 1;
            segs.push(self.name_tok()?);
        }
        Ok(segs.join("."))
    }

    // ---- 型
    pub(crate) fn targs(&mut self) -> R<Vec<V>> {
        self.expect_op("<")?;
        let mut out = vec![self.targ()?];
        while self.accept_op(",") {
            out.push(self.targ()?);
        }
        self.expect_op(">")?;
        Ok(out)
    }

    pub(crate) fn targ(&mut self) -> R<V> {
        let c = self.cur().clone();
        if c.kind == "life" {
            self.i += 1;
            return Ok(V::L(vec![s("life"), V::S(c.text)]));
        }
        if c.kind == "int" {
            self.i += 1;
            let (v, _) = split_number(&c.text, false)?;
            return Ok(V::L(vec![s("const"), V::N(v)]));
        }
        if c.kind == "kw" && (c.text == "never" || c.text == "may") {
            self.i += 1;
            return Ok(V::L(vec![s("panic"), V::S(c.text)]));
        }
        if c.kind == "kw" && (c.text == "true" || c.text == "false") {
            self.i += 1;
            return Ok(V::L(vec![s("const"), V::B(c.text == "true")]));
        }
        if c.kind == "char" {
            self.i += 1;
            let ch = unescape(&c.text[1..c.text.len() - 1])?;
            return Ok(V::L(vec![s("const"), V::S(ch.iter().collect())]));
        }
        if self.is_op("[") {
            let e = self.effect_set()?;
            return Ok(V::L(vec![s("effects"), e]));
        }
        self.ty()
    }

    pub(crate) fn effect_set(&mut self) -> R<V> {
        self.expect_op("[")?;
        let mut out = Vec::new();
        if !self.is_op("]") {
            loop {
                let mut nm = self.name_tok()?;
                while self.is_op(".") {
                    self.i += 1;
                    nm.push('.');
                    nm.push_str(&self.name_tok()?);
                }
                out.push(V::S(nm));
                if !self.accept_op(",") {
                    break;
                }
            }
        }
        self.expect_op("]")?;
        Ok(V::L(out))
    }

    pub(crate) fn const_expr(&mut self) -> R<V> {
        let c = self.cur().clone();
        if c.kind == "int" {
            self.i += 1;
            return Ok(V::N(split_number(&c.text, false)?.0));
        }
        if self.accept_op("(") {
            let v = self.const_expr()?;
            self.expect_op(")")?;
            return Ok(v);
        }
        let f = self.fqn()?;
        Ok(V::L(vec![s("cname"), V::S(f)]))
    }

    pub(crate) fn panic_val(&mut self) -> R<String> {
        let c = self.cur().clone();
        if (c.kind == "kw" && (c.text == "never" || c.text == "may")) || c.kind == "upper" {
            self.i += 1;
            return Ok(c.text);
        }
        self.err("expected never/may")
    }

    pub(crate) fn trait_ref(&mut self) -> R<V> {
        let nm = self.fqn()?;
        let args = if self.is_op("<") { self.targs()? } else { vec![] };
        Ok(V::L(vec![V::S(nm), V::L(args)]))
    }

    pub fn ty(&mut self) -> R<V> {
        let c = self.cur().clone();
        if c.kind == "prim" {
            self.i += 1;
            return Ok(V::L(vec![s("prim"), V::S(c.text)]));
        }
        if c.kind == "op" {
            match c.text.as_str() {
                "!" => {
                    self.i += 1;
                    return Ok(V::L(vec![s("never")]));
                }
                "(" => {
                    self.i += 1;
                    if self.accept_op(")") {
                        return Ok(V::L(vec![s("tuple"), V::L(vec![])]));
                    }
                    let mut items = vec![self.ty()?];
                    let mut comma = false;
                    while self.accept_op(",") {
                        comma = true;
                        if self.is_op(")") {
                            break;
                        }
                        items.push(self.ty()?);
                    }
                    self.expect_op(")")?;
                    if comma || items.len() != 1 {
                        return Ok(V::L(vec![s("tuple"), V::L(items)]));
                    }
                    return Ok(items.remove(0));
                }
                "[" => {
                    self.i += 1;
                    let t = self.ty()?;
                    if self.accept_op(";") {
                        let n = self.const_expr()?;
                        self.expect_op("]")?;
                        return Ok(V::L(vec![s("array"), t, n]));
                    }
                    self.expect_op("]")?;
                    return Ok(V::L(vec![s("slice"), t]));
                }
                "&" | "&&" => {
                    self.i += 1;
                    let inner = self.ref_rest()?;
                    if c.text == "&&" {
                        return Ok(V::L(vec![s("ref"), V::B(false), inner]));
                    }
                    return Ok(inner);
                }
                "*" => {
                    self.i += 1;
                    let m = self.is_kw("mut");
                    self.i += 1;
                    let t = self.ty()?;
                    return Ok(V::L(vec![s("ptr"), V::B(m), t]));
                }
                _ => {}
            }
        }
        if c.kind == "kw" {
            match c.text.as_str() {
                "fn" => {
                    self.i += 1;
                    self.expect_op("(")?;
                    let mut ps = Vec::new();
                    if !self.is_op(")") {
                        ps.push(self.ty()?);
                        while self.accept_op(",") {
                            ps.push(self.ty()?);
                        }
                    }
                    self.expect_op(")")?;
                    self.expect_op("->")?;
                    let r = self.ty()?;
                    self.expect_kw("uses")?;
                    let eff = self.effect_set()?;
                    self.expect_kw("panics")?;
                    let pn = self.panic_val()?;
                    return Ok(V::L(vec![s("fn"), V::L(ps), r, eff, V::S(pn)]));
                }
                "dyn" => {
                    self.i += 1;
                    let tr = self.trait_ref()?;
                    let mut lt = V::Nil;
                    if self.accept_op("+") {
                        lt = V::S(self.cur().text.clone());
                        self.i += 1;
                    }
                    return Ok(V::L(vec![s("dyn"), tr, lt]));
                }
                "Self" => {
                    self.i += 1;
                    return Ok(V::L(vec![s("self")]));
                }
                _ => {}
            }
        }
        if c.kind == "ident" || c.kind == "upper" {
            let nm = self.fqn()?;
            let args = if self.is_op("<") { self.targs()? } else { vec![] };
            return Ok(V::L(vec![s("named"), V::S(nm), V::L(args)]));
        }
        self.err("expected a type")
    }

    pub(crate) fn ref_rest(&mut self) -> R<V> {
        if self.cur().kind == "life" {
            self.i += 1;
        }
        let m = self.accept_kw("mut");
        if self.is_op("[") {
            self.i += 1;
            let t = self.ty()?;
            if self.accept_op(";") {
                let n = self.const_expr()?;
                self.expect_op("]")?;
                return Ok(V::L(vec![s("ref"), V::B(m), V::L(vec![s("array"), t, n])]));
            }
            self.expect_op("]")?;
            return Ok(V::L(vec![s("ref"), V::B(m), V::L(vec![s("slice"), t])]));
        }
        if self.cur().kind == "prim" && self.cur().text == "str" {
            self.i += 1;
            return Ok(V::L(vec![s("ref"), V::B(m), V::L(vec![s("prim"), s("str")])]));
        }
        let t = self.ty()?;
        Ok(V::L(vec![s("ref"), V::B(m), t]))
    }

    pub(crate) fn params(&mut self) -> R<V> {
        let mut out = Vec::new();
        if self.is_op(")") {
            return Ok(V::L(out));
        }
        loop {
            let nm = if self.is_kw("self") {
                self.i += 1;
                "self".to_string()
            } else {
                self.name_tok()?
            };
            self.expect_op(":")?;
            let t = self.ty()?;
            out.push(V::L(vec![V::S(nm), t]));
            if !self.accept_op(",") {
                break;
            }
        }
        Ok(V::L(out))
    }

    // ---- 式
    pub fn expr(&mut self, ns: bool) -> R<V> {
        let c = self.cur().clone();
        if c.kind == "kw" {
            match c.text.as_str() {
                "break" => {
                    self.i += 1;
                    return Ok(ndl(c.pos(), "Break", vec![]));
                }
                "continue" => {
                    self.i += 1;
                    return Ok(ndl(c.pos(), "Continue", vec![]));
                }
                "return" => {
                    self.i += 1;
                    let mut e = None;
                    if self.cur().kind != "nl" && self.cur().kind != "eof" && !self.is_op("}") {
                        e = Some(self.expr(ns)?);
                    }
                    return Ok(ndl(c.pos(), "Return", vec![opt(e)]));
                }
                _ => {}
            }
        }
        self.or_expr(ns)
    }

    pub(crate) fn or_expr(&mut self, ns: bool) -> R<V> {
        let mut l = self.and_expr(ns)?;
        while self.is_op("||") {
            let ln = self.cur().pos();
            self.i += 1;
            let r = self.and_expr(ns)?;
            l = ndl(anchor(ln, &l), "Bin", vec![s("||"), l, r]);
        }
        Ok(l)
    }

    pub(crate) fn and_expr(&mut self, ns: bool) -> R<V> {
        let mut l = self.cmp_expr(ns)?;
        while self.is_op("&&") {
            let ln = self.cur().pos();
            self.i += 1;
            let r = self.cmp_expr(ns)?;
            l = ndl(anchor(ln, &l), "Bin", vec![s("&&"), l, r]);
        }
        Ok(l)
    }

    pub(crate) fn cmp_expr(&mut self, ns: bool) -> R<V> {
        let l = self.bin_level(0, ns)?;
        let c = self.cur().clone();
        if self.is_kw("is") {
            // `式 is パターン` = `match 式 { パターン => true, <他> => false }` (名前を束縛しないパターンのみ)。
            self.i += 1;
            let pat = self.pattern()?;
            let mut b = vec![];
            crate::check::pat_binds(&pat, &mut b);
            if !b.is_empty() {
                return self.err("a pattern after `is` cannot bind names; use `_`");
            }
            let t = ndl(c.pos(), "Lit", vec![s("bool"), V::B(true)]);
            let f = ndl(c.pos(), "Lit", vec![s("bool"), V::B(false)]);
            let arms = vec![V::L(vec![pat, t]), V::L(vec![ndl(c.pos(), "PWild", vec![V::B(true)]), f])];
            return Ok(ndl(anchor(c.pos(), &l), "Match", vec![l, V::L(arms)]));
        }
        if c.kind == "op" && matches!(c.text.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=") {
            self.i += 1;
            let r = self.bin_level(0, ns)?;
            return Ok(ndl(anchor(c.pos(), &l), "Bin", vec![V::S(c.text), l, r]));
        }
        Ok(l)
    }

    pub(crate) fn bin_level(&mut self, lv: usize, ns: bool) -> R<V> {
        const LEVELS: [&[&str]; 5] = [&["|"], &["^"], &["&"], &["+", "-"], &["*", "/", "%"]];
        if lv == LEVELS.len() {
            return self.unary(ns);
        }
        let mut l = self.bin_level(lv + 1, ns)?;
        while self.cur().kind == "op" && LEVELS[lv].contains(&self.cur().text.as_str()) {
            let op = self.cur().text.clone();
            let ln = self.cur().pos();
            self.i += 1;
            let r = self.bin_level(lv + 1, ns)?;
            l = ndl(anchor(ln, &l), "Bin", vec![V::S(op), l, r]);
        }
        Ok(l)
    }

    pub(crate) fn unary(&mut self, ns: bool) -> R<V> {
        let c = self.cur().clone();
        if c.kind == "op" {
            if matches!(c.text.as_str(), "-" | "!" | "*") {
                self.i += 1;
                let e = self.unary(ns)?;
                return Ok(ndl(c.pos(), "Unary", vec![V::S(c.text), e]));
            }
            if c.text == "&" {
                self.i += 1;
                if self.accept_kw("mut") {
                    let e = self.unary(ns)?;
                    return Ok(ndl(c.pos(), "Unary", vec![s("&mut"), e]));
                }
                let e = self.unary(ns)?;
                return Ok(ndl(c.pos(), "Unary", vec![s("&"), e]));
            }
        }
        self.postfix(ns)
    }

    pub(crate) fn call_args(&mut self) -> R<V> {
        self.expect_op("(")?;
        let mut out = Vec::new();
        if !self.is_op(")") {
            loop {
                out.push(self.expr(false)?);
                if !self.accept_op(",") {
                    break;
                }
                if self.is_op(")") {
                    break;
                }
            }
        }
        self.expect_op(")")?;
        Ok(V::L(out))
    }

    pub(crate) fn postfix(&mut self, ns: bool) -> R<V> {
        let mut e = self.primary(ns)?;
        loop {
            let c = self.cur().clone();
            if c.kind != "op" {
                break;
            }
            match c.text.as_str() {
                "." => {
                    self.i += 1;
                    let name_pos = self.cur().pos();
                    let nm = self.name_tok()?;
                    let mut targs = V::Nil;
                    if self.is_op("::") {
                        self.i += 1;
                        targs = V::L(self.targs()?);
                    }
                    if self.is_op("(") {
                        let args = self.call_args()?;
                        e = ndl(anchor(c.pos(), &e), "MCall", vec![e, V::S(nm), args, targs, V::N(name_pos.to_string())]);
                    } else {
                        e = ndl(anchor(c.pos(), &e), "Field", vec![e, V::S(nm), V::N(name_pos.to_string())]);
                    }
                }
                "(" => {
                    let args = self.call_args()?;
                    e = ndl(anchor(c.pos(), &e), "Call", vec![e, args, V::Nil]);
                }
                "[" => {
                    self.i += 1;
                    let idx = self.expr(false)?;
                    self.expect_op("]")?;
                    e = ndl(anchor(c.pos(), &e), "Index", vec![e, idx]);
                }
                "?" => {
                    self.i += 1;
                    e = ndl(anchor(c.pos(), &e), "Try", vec![e]);
                }
                _ => break,
            }
        }
        Ok(e)
    }

    pub(crate) fn lit_tok(&mut self, c: &Tok) -> R<V> {
        self.i += 1;
        match c.kind {
            "int" => {
                let (v, sfx) = split_number(&c.text, false)?;
                Ok(ndl(c.pos(), "Lit", vec![s("int"), V::N(v), opt(sfx.map(V::S))]))
            }
            "float" => {
                let (core, sfx) = split_number(&c.text, true)?;
                let core = if c.text.starts_with('-') { format!("-{}", core) } else { core };
                Ok(ndl(c.pos(), "Lit", vec![s("float"), opt(sfx.map(V::S)), V::N(core)]))
            }
            "str" => {
                let inner = &c.text[1..c.text.len() - 1];
                let v: String = unescape(inner)?.into_iter().collect();
                Ok(ndl(c.pos(), "Lit", vec![s("str"), V::S(hex(v.as_bytes()))]))
            }
            "bstr" => {
                let inner = &c.text[2..c.text.len() - 1];
                let v = unescape(inner)?;
                Ok(ndl(c.pos(), "Lit", vec![s("bstr"), V::L(v.iter().map(|ch| V::N((*ch as u32).to_string())).collect())]))
            }
            "char" => {
                let v = unescape(&c.text[1..c.text.len() - 1])?;
                Ok(ndl(c.pos(), "Lit", vec![s("char"), V::N((v[0] as u32).to_string())]))
            }
            _ => {
                let v = unescape(&c.text[2..c.text.len() - 1])?;
                Ok(ndl(c.pos(), "Lit", vec![s("byte"), V::N((v[0] as u32).to_string())]))
            }
        }
    }

    pub(crate) fn primary(&mut self, ns: bool) -> R<V> {
        let c = self.cur().clone();
        match c.kind {
            "int" | "float" | "str" | "char" | "bstr" | "byte" => return self.lit_tok(&c),
            "kw" => match c.text.as_str() {
                "true" | "false" => {
                    self.i += 1;
                    return Ok(ndl(c.pos(), "Lit", vec![s("bool"), V::B(c.text == "true")]));
                }
                "self" => {
                    self.i += 1;
                    return Ok(ndl(c.pos(), "Path", vec![V::L(vec![s("self")]), V::Nil]));
                }
                "if" => return self.if_expr(),
                "match" => return self.match_expr(),
                "loop" => {
                    self.i += 1;
                    let b = self.block()?;
                    return Ok(ndl(c.pos(), "Loop", vec![b]));
                }
                "while" => {
                    self.i += 1;
                    let cond = self.expr(true)?;
                    let b = self.block()?;
                    return Ok(ndl(c.pos(), "While", vec![cond, b]));
                }
                "for" => {
                    self.i += 1;
                    let pat = self.pattern()?;
                    self.expect_kw("in")?;
                    let it = self.expr(true)?;
                    let b = self.block()?;
                    return Ok(ndl(c.pos(), "For", vec![pat, it, b]));
                }
                "unsafe" => {
                    self.i += 1;
                    let name = self.str_value()?;
                    let why = self.str_value()?;
                    let b = self.block()?;
                    return Ok(ndl(c.pos(), "Unsafe", vec![V::S(name), V::S(why), b]));
                }
                "with" => return self.with_expr(),
                "fn" => return self.closure(),
                _ => {}
            },
            "op" => match c.text.as_str() {
                "(" => {
                    self.i += 1;
                    if self.accept_op(")") {
                        return Ok(ndl(c.pos(), "Lit", vec![s("unit")]));
                    }
                    let first = self.expr(false)?;
                    if self.accept_op(")") {
                        return Ok(first);
                    }
                    let mut items = vec![first];
                    while self.accept_op(",") {
                        if self.is_op(")") {
                            break;
                        }
                        items.push(self.expr(false)?);
                    }
                    self.expect_op(")")?;
                    return Ok(ndl(c.pos(), "TupleE", vec![V::L(items)]));
                }
                "[" => {
                    self.i += 1;
                    if self.accept_op("]") {
                        return Ok(ndl(c.pos(), "ArrayE", vec![V::L(vec![])]));
                    }
                    let first = self.expr(false)?;
                    if self.accept_op(";") {
                        let n = self.const_expr()?;
                        self.expect_op("]")?;
                        return Ok(ndl(c.pos(), "ArrayRep", vec![first, n]));
                    }
                    let mut items = vec![first];
                    while self.accept_op(",") {
                        if self.is_op("]") {
                            break;
                        }
                        items.push(self.expr(false)?);
                    }
                    self.expect_op("]")?;
                    return Ok(ndl(c.pos(), "ArrayE", vec![V::L(items)]));
                }
                "{" => return self.block(),
                _ => {}
            },
            "ident" | "upper" | "prim" => {
                let mut segs = vec![c.text.clone()];
                self.i += 1;
                while self.dot_continues() {
                    self.i += 1;
                    segs.push(self.cur().text.clone());
                    self.i += 1;
                }
                let mut targs = V::Nil;
                if self.is_op("::") {
                    self.i += 1;
                    targs = V::L(self.targs()?);
                }
                if !ns && self.is_op("{") && self.looks_struct_lit() {
                    self.i += 1;
                    let mut fields = Vec::new();
                    loop {
                        let fnm = self.name_tok()?;
                        self.expect_op(":")?;
                        let e = self.expr(false)?;
                        fields.push(V::L(vec![V::S(fnm), e]));
                        if !self.accept_op(",") {
                            break;
                        }
                    }
                    self.expect_op("}")?;
                    return Ok(ndl(c.pos(), "StructLit", vec![V::S(segs.join(".")), V::L(fields)]));
                }
                let p = ndl(c.pos(), "Path", vec![strs(&segs), targs.clone()]);
                if !matches!(targs, V::Nil) && self.is_op("(") {
                    let args = self.call_args()?;
                    return Ok(ndl(c.pos(), "Call", vec![p, args, targs]));
                }
                return Ok(p);
            }
            _ => {}
        }
        self.err("expected an expression")
    }

    pub(crate) fn str_value(&mut self) -> R<String> {
        let c = self.cur().clone();
        self.i += 1;
        if c.kind != "str" {
            return Err(format!("{}:{}: expected string", c.line, c.col));
        }
        Ok(hex(unescape(&c.text[1..c.text.len() - 1])?.into_iter().collect::<String>().as_bytes()))
    }

    pub(crate) fn looks_struct_lit(&self) -> bool {
        let a = self.peek(1);
        let b = self.peek(2);
        a.kind == "ident" && b.kind == "op" && b.text == ":"
    }

    pub(crate) fn closure(&mut self) -> R<V> {
        let ln = self.cur().pos();
        self.expect_kw("fn")?;
        let hdr_start = self.i;
        self.expect_op("(")?;
        let ps = self.params()?;
        self.expect_op(")")?;
        self.expect_op("->")?;
        let ret = self.ty()?;
        // ヘッダの字面 (`( x : & u32 ) -> & 'static u32`)。型の `V` はライフタイムを持たないので、
        // 借用検査が省略の有無と戻り値の領域を読むためにここに残す (5 番目の要素)。
        let header = self.t[hdr_start..self.i].iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
        self.expect_kw("capture")?;
        self.expect_op("[")?;
        let mut caps = Vec::new();
        if !self.is_op("]") {
            loop {
                if self.accept_kw("move") {
                    let n = self.name_tok()?;
                    caps.push(V::L(vec![s("move"), V::S(n)]));
                } else {
                    self.expect_op("&")?;
                    let m = self.accept_kw("mut");
                    let n = self.name_tok()?;
                    caps.push(V::L(vec![s(if m { "&mut" } else { "&" }), V::S(n)]));
                }
                if !self.accept_op(",") {
                    break;
                }
            }
        }
        self.expect_op("]")?;
        let body = self.block()?;
        Ok(ndl(ln, "Closure", vec![ps, ret, V::L(caps), body, V::S(header)]))
    }

    /// `with [mut] 名前 = 初期化式 close 後始末式 { 本体 }` (4.4)。
    /// ノード: `With [PBind(名前), mut, 初期化式, 後始末式, 本体ブロック]`。
    pub(crate) fn with_expr(&mut self) -> R<V> {
        let c = self.cur().clone();
        self.expect_kw("with")?;
        let m = self.accept_kw("mut");
        let nc = self.cur().clone();
        if nc.kind != "ident" {
            return self.err("expected a binding name after `with`");
        }
        self.i += 1;
        self.expect_op("=")?;
        let init = self.expr(false)?;
        self.expect_kw("close")?;
        let cleanup = self.expr(true)?;
        let body = self.block()?;
        let pat = ndl(nc.pos(), "PBind", vec![V::S(nc.text), V::B(false)]);
        Ok(ndl(c.pos(), "With", vec![pat, V::B(m), init, cleanup, body]))
    }

    pub(crate) fn if_expr(&mut self) -> R<V> {
        let ln = self.cur().pos();
        self.expect_kw("if")?;
        if self.accept_kw("let") {
            // `if let パターン = 式 { .. } [else ..]` = `match 式 { パターン => { .. }, <他> => else側 }`。
            // 2 腕目の `PWild [true]` は構文から作った「他の全部」で、E2021 の対象外 (線形値は E3003)。
            let pat = self.pattern()?;
            self.expect_op("=")?;
            let scrut = self.expr(true)?;
            let then = self.block()?;
            let els = if self.accept_kw("else") {
                if self.is_kw("if") { self.if_expr()? } else { self.block()? }
            } else {
                ndl(ln, "Block", vec![V::L(vec![])])
            };
            let rest = ndl(ln, "PWild", vec![V::B(true)]);
            return Ok(ndl(ln, "Match", vec![scrut, V::L(vec![V::L(vec![pat, then]), V::L(vec![rest, els])])]));
        }
        let cond = self.expr(true)?;
        let then = self.block()?;
        let mut els = V::Nil;
        if self.accept_kw("else") {
            els = if self.is_kw("if") { self.if_expr()? } else { self.block()? };
        }
        Ok(ndl(ln, "If", vec![cond, then, els]))
    }

    pub(crate) fn match_expr(&mut self) -> R<V> {
        let ln = self.cur().pos();
        self.expect_kw("match")?;
        let scrut = self.expr(true)?;
        self.expect_op("{")?;
        self.skip_nl();
        let mut arms = Vec::new();
        while !self.is_op("}") {
            let pat = self.pattern()?;
            self.expect_op("=>")?;
            let body = self.expr(false)?;
            arms.push(V::L(vec![pat, body]));
            self.expect_nl()?;
        }
        self.expect_op("}")?;
        Ok(ndl(ln, "Match", vec![scrut, V::L(arms)]))
    }

    pub fn block(&mut self) -> R<V> {
        let ln = self.cur().pos();
        self.expect_op("{")?;
        self.skip_nl();
        let mut stmts = Vec::new();
        while !self.is_op("}") {
            stmts.push(self.stmt()?);
            if !self.is_op("}") {
                self.expect_nl()?;
            }
        }
        self.expect_op("}")?;
        Ok(ndl(ln, "Block", vec![V::L(stmts)]))
    }

    pub fn stmt(&mut self) -> R<V> {
        let ln = self.cur().pos();
        if self.is_kw("let") {
            self.i += 1;
            let m = self.accept_kw("mut");
            let pat = self.pattern()?;
            let mut ty = V::Nil;
            if self.accept_op(":") {
                ty = self.ty()?;
            }
            self.expect_op("=")?;
            let e = self.expr(false)?;
            return Ok(ndl(ln, "Let", vec![pat, V::B(m), ty, e]));
        }
        let e = self.expr(false)?;
        if self.is_op("=") {
            self.i += 1;
            let r = self.expr(false)?;
            return Ok(ndl(ln, "Assign", vec![e, r]));
        }
        Ok(ndl(ln, "ExprStmt", vec![e]))
    }

    // ---- パターン
    pub fn pattern(&mut self) -> R<V> {
        let c = self.cur().clone();
        if matches!(c.kind, "int" | "float" | "str" | "char" | "byte" | "bstr") {
            let l = self.lit_tok(&c)?;
            return Ok(ndl(c.pos(), "PLit", vec![l]));
        }
        if c.kind == "kw" && (c.text == "true" || c.text == "false") {
            self.i += 1;
            return Ok(ndl(c.pos(), "PLit", vec![nd("Lit", vec![s("bool"), V::B(c.text == "true")])]));
        }
        if c.kind == "op" {
            if c.text == "&" {
                self.i += 1;
                let p = self.pattern()?;
                return Ok(ndl(c.pos(), "PRef", vec![p]));
            }
            if c.text == "(" {
                self.i += 1;
                if self.accept_op(")") {
                    return Ok(ndl(c.pos(), "PLit", vec![nd("Lit", vec![s("unit")])]));
                }
                let mut items = vec![self.pattern()?];
                while self.accept_op(",") {
                    if self.is_op(")") {
                        break;
                    }
                    items.push(self.pattern()?);
                }
                self.expect_op(")")?;
                return Ok(ndl(c.pos(), "PTuple", vec![V::L(items)]));
            }
        }
        if matches!(c.kind, "ident" | "upper" | "prim") {
            if c.text == "_" {
                self.i += 1;
                return Ok(ndl(c.pos(), "PWild", vec![]));
            }
            let mut segs = vec![c.text.clone()];
            self.i += 1;
            while self.is_op(".") && matches!(self.peek(1).kind, "ident" | "upper" | "prim") {
                self.i += 1;
                segs.push(self.cur().text.clone());
                self.i += 1;
            }
            if self.is_op("(") {
                self.i += 1;
                let mut items = vec![self.pattern()?];
                while self.accept_op(",") {
                    items.push(self.pattern()?);
                }
                self.expect_op(")")?;
                return Ok(ndl(c.pos(), "PVariant", vec![strs(&segs), V::L(items)]));
            }
            if self.is_op("{") {
                self.i += 1;
                let mut fl = Vec::new();
                loop {
                    let fnm = self.name_tok()?;
                    self.expect_op(":")?;
                    let p = self.pattern()?;
                    fl.push(V::L(vec![V::S(fnm), p]));
                    if !self.accept_op(",") {
                        break;
                    }
                }
                self.expect_op("}")?;
                return Ok(ndl(c.pos(), "PStruct", vec![strs(&segs), V::L(fl)]));
            }
            if segs.len() == 1 && c.kind == "ident" {
                return Ok(ndl(c.pos(), "PBind", vec![V::S(c.text), V::B(false)]));
            }
            return Ok(ndl(c.pos(), "PPath", vec![strs(&segs)]));
        }
        self.err("expected a pattern")
    }
}

/// 本体(インデントされた行の並び)を Block として解析する。
pub fn parse_body(mut toks: Vec<Tok>, base: usize) -> R<V> {
    for t in toks.iter_mut() {
        t.line += base;
    }
    let mut p = Parser::new(toks);
    p.skip_nl();
    let mut stmts = Vec::new();
    while !p.at_eof() {
        stmts.push(p.stmt()?);
        p.expect_nl()?;
    }
    let mut body = nd("Block", vec![V::L(stmts)]);
    number_nodes(&mut body, &mut 0);
    Ok(body)
}
