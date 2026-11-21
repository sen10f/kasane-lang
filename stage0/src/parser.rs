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
            // `ref` 型の 4 つ目 (`&'a T` の `'a`、設計 9 章) は型の同一性に入れない: 比べる・名前に使う `dump` には出さない。
            V::L(xs) => {
                let xs = if xs.len() == 4 && matches!(&xs[0], V::S(k) if k == "ref") { &xs[..3] } else { &xs[..] };
                format!("[{}]", xs.iter().map(|x| x.dump()).collect::<Vec<_>>().join(" "))
            }
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
/// 整数の literal が 0 か (`0`・`0usize`・`0u32` など。接尾辞は符号なしの整数型だけ)。
fn zero_lit(t: &Tok) -> bool {
    if t.kind != "int" {
        return false;
    }
    let body = ["usize", "u128", "u64", "u32", "u16", "u8"].iter().find_map(|sfx| t.text.strip_suffix(sfx)).unwrap_or(&t.text);
    body == "0"
}

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
    /// `if` / `while` / `match` の頭の式の中の `is` パターン: `{` は本体の始まりで、構造体パターンではない。
    pat_ns: bool,
    /// 直前に解析した式の「最上位の形」の強さ (0 = 式の位置にしか置けない `return` や `if` など、1 = `||`、2 = `&&`、3 = 比較・`is`、
    /// 4..8 = `|` `^` `&` `+ -` `* / %`、9 = 前置、10 = 後置・一次式)。冗長な括弧の判定 (D10) 用。
    lvl: u8,
    /// 括弧の群 (開きのトークン位置 → (閉じの位置, 中の式の強さ, 中の最上位に `{` があるか))。タプル・ユニットは含めない。
    pmap: std::collections::HashMap<usize, PInfo>,
    /// 冗長と判った括弧の群 (開きの位置, 閉じの位置)。
    pub(crate) red: Vec<(usize, usize)>,
    /// 書き換え (D8・D11・D15)。
    pub(crate) rw: Vec<Rw>,
    last_unary: Option<UnInfo>,
    last_cmp: Option<(usize, usize, usize)>,
    last_mcall: Option<McInfo>,
    last_block: Option<BlInfo>,
    /// 解析している関数の名前 (自分自身を呼ぶ形への書き換えをしないため)。
    fn_name: String,
    /// プログラムが `len` / `is_empty` を自分の型に定義している (D11 の `len()` の書き換えをしない)。
    /// 型は字句の段階では分からないので、`fmt` の呼び出し側 (パッケージの全ソース) が決める。
    own_len: bool,
}

/// 書き換えの部品。`Span` と `Dedent` は元のトークン列の区間 `[a, b)` の字面を写す (`Dedent` は 2 行目以降を 4 桁浅くする)。
#[derive(Clone, Debug)]
pub enum Part {
    Lit(String),
    Span(usize, usize),
    Dedent(usize, usize),
}

/// トークン `[a, b)` の字面を `parts` に置き換える。`del_line` なら、その文の行を丸ごと消す。
#[derive(Clone, Debug)]
pub struct Rw {
    pub a: usize,
    pub b: usize,
    pub parts: Vec<Part>,
    pub del_line: bool,
    /// 範囲の中のコメント行の移し先: 偽なら文の行の直前、真なら書き換えた文の 1 行目 (`else if ..`) の直後 (本体の字下げ)。
    pub inside: bool,
}

#[derive(Clone)]
struct UnInfo {
    start: usize,
    end: usize,
    op: String,
    operand_start: usize,
    operand_lvl: u8,
}

#[derive(Clone)]
struct McInfo {
    start: usize,
    end: usize,
    dot: usize,
    name: String,
    open: usize,
    close: usize,
}

#[derive(Clone)]
struct BlInfo {
    open: usize,
    close: usize,
    nstmts: usize,
    expr_stmt: bool,
    if_stmt: bool,
    unit_stmt: bool,
}

/// 括弧の群の情報。
#[derive(Clone)]
struct PInfo {
    close: usize,
    lvl: u8,
    brace: bool,
    un: Option<UnInfo>,
    cmp: Option<usize>,
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
        Parser { t, i: 0, pat_ns: false, lvl: 10, pmap: Default::default(), red: vec![], rw: vec![], last_unary: None, last_cmp: None, last_mcall: None, last_block: None, fn_name: String::new(), own_len: false }
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
            let (digits, suffix) = split_number(&c.text, false)?;
            // 長さは `usize` (4.5): 別の整数型の接尾辞は付けられない。
            if let Some(sfx) = suffix.filter(|x| x != "usize") {
                return self.err(&format!("an array length is a `usize`: remove the suffix `{}`", sfx));
            }
            return Ok(V::N(digits));
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
        // 領域 (`&'a T` の `'a`) は 4 つ目の要素に残す (なければ `Nil`)。型の見た目・等しさには入れない (設計 9 章)。
        let mut life = V::Nil;
        if self.cur().kind == "life" {
            life = V::S(self.cur().text.clone());
            self.i += 1;
        }
        let m = self.accept_kw("mut");
        if self.is_op("[") {
            self.i += 1;
            let t = self.ty()?;
            if self.accept_op(";") {
                let n = self.const_expr()?;
                self.expect_op("]")?;
                return Ok(V::L(vec![s("ref"), V::B(m), V::L(vec![s("array"), t, n]), life]));
            }
            self.expect_op("]")?;
            return Ok(V::L(vec![s("ref"), V::B(m), V::L(vec![s("slice"), t]), life]));
        }
        if self.cur().kind == "prim" && self.cur().text == "str" {
            self.i += 1;
            return Ok(V::L(vec![s("ref"), V::B(m), V::L(vec![s("prim"), s("str")]), life]));
        }
        let t = self.ty()?;
        Ok(V::L(vec![s("ref"), V::B(m), t, life]))
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
    /// `s` から始まる直前の式が、括弧 1 つだけで、その強さが `need` 以上なら冗長な括弧として記録する。
    fn opnd(&mut self, s: usize, need: u8, ns: bool, prefix: &str) {
        let Some(&PInfo { close, lvl: lv, brace, .. }) = self.pmap.get(&s) else { return };
        if close + 1 != self.i || lv < need || (ns && brace) {
            return;
        }
        let inner = &self.t[s + 1];
        // `-(1i32)` を `-1i32` (負の数のリテラル) にしない・`&(&x)` を `&&x` (別のトークン) にしない。
        if prefix == "-" && matches!(inner.kind, "int" | "float") {
            return;
        }
        if prefix == "&" && inner.text.starts_with('&') {
            return;
        }
        self.red.push((s, close));
    }

    pub fn expr(&mut self, ns: bool) -> R<V> {
        let s = self.i;
        let r = self.expr_inner(ns)?;
        self.opnd(s, 0, ns, "");
        Ok(r)
    }

    fn expr_inner(&mut self, ns: bool) -> R<V> {
        let c = self.cur().clone();
        if c.kind == "kw" {
            match c.text.as_str() {
                "break" => {
                    self.i += 1;
                    self.lvl = 0;
                    return Ok(ndl(c.pos(), "Break", vec![]));
                }
                "continue" => {
                    self.i += 1;
                    self.lvl = 0;
                    return Ok(ndl(c.pos(), "Continue", vec![]));
                }
                "return" => {
                    self.i += 1;
                    let mut e = None;
                    if self.cur().kind != "nl" && self.cur().kind != "eof" && !self.is_op("}") {
                        e = Some(self.expr(ns)?);
                    }
                    self.lvl = 0;
                    return Ok(ndl(c.pos(), "Return", vec![opt(e)]));
                }
                _ => {}
            }
        }
        self.or_expr(ns)
    }

    pub(crate) fn or_expr(&mut self, ns: bool) -> R<V> {
        let s0 = self.i;
        let mut l = self.and_expr(ns)?;
        while self.is_op("||") {
            let ln = self.cur().pos();
            self.opnd(s0, 1, ns, "");
            self.i += 1;
            let s1 = self.i;
            let r = self.and_expr(ns)?;
            self.opnd(s1, 2, ns, "");
            l = ndl(anchor(ln, &l), "Bin", vec![s("||"), l, r]);
            self.lvl = 1;
        }
        Ok(l)
    }

    pub(crate) fn and_expr(&mut self, ns: bool) -> R<V> {
        let s0 = self.i;
        let mut l = self.cmp_expr(ns)?;
        while self.is_op("&&") {
            let ln = self.cur().pos();
            self.opnd(s0, 2, ns, "");
            self.i += 1;
            let s1 = self.i;
            let r = self.cmp_expr(ns)?;
            self.opnd(s1, 3, ns, "");
            l = ndl(anchor(ln, &l), "Bin", vec![s("&&"), l, r]);
            self.lvl = 2;
        }
        Ok(l)
    }

    pub(crate) fn cmp_expr(&mut self, ns: bool) -> R<V> {
        let s0 = self.i;
        let l = self.bin_level(0, ns)?;
        let c = self.cur().clone();
        let left_mc = self.last_mcall.clone().filter(|m| m.start == s0 && m.end == self.i);
        if self.is_kw("is") {
            self.opnd(s0, 4, ns, "");
            // `式 is パターン` = `match 式 { パターン => true, <他> => false }` (名前を束縛しないパターンのみ)。
            self.i += 1;
            let saved = std::mem::replace(&mut self.pat_ns, ns);
            let pat = self.pattern();
            self.pat_ns = saved;
            let pat = pat?;
            let mut b = vec![];
            crate::check::pat_binds(&pat, &mut b);
            if !b.is_empty() {
                return self.err("a pattern after `is` cannot bind names; use `_`");
            }
            let t = ndl(c.pos(), "Lit", vec![s("bool"), V::B(true)]);
            let f = ndl(c.pos(), "Lit", vec![s("bool"), V::B(false)]);
            let arms = vec![V::L(vec![pat, t]), V::L(vec![ndl(c.pos(), "PWild", vec![V::B(true)]), f])];
            self.lvl = 3;
            return Ok(ndl(anchor(c.pos(), &l), "Match", vec![l, V::L(arms)]));
        }
        if c.kind == "op" && matches!(c.text.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=") {
            self.opnd(s0, 4, ns, "");
            let op_idx = self.i;
            self.i += 1;
            let s1 = self.i;
            let r = self.bin_level(0, ns)?;
            self.opnd(s1, 4, ns, "");
            let right_mc = self.last_mcall.clone().filter(|m| m.start == s1 && m.end == self.i);
            self.cmp_rewrites(s0, op_idx, s1, left_mc, right_mc);
            self.last_cmp = Some((s0, self.i, op_idx));
            self.lvl = 3;
            return Ok(ndl(anchor(c.pos(), &l), "Bin", vec![V::S(c.text), l, r]));
        }
        Ok(l)
    }

    /// 比較の書き換え (D11)。literal は左右どちらにあってもよい (`true == x` = `x == true`、`0usize == v.len()` = `v.len() == 0usize`)。
    /// `a == None` → `a is None`、`a == true` / `true == a` → `a`、`a != true` / `a == false` → `!a` (`!` は括弧ごと),
    /// `x.len() == 0` / `0 == x.len()` → `x.is_empty()`、`!=` と `>` と (左が 0 の) `<` は `!x.is_empty()`。
    /// 比較の書き換え (D11)。literal は左右どちらにあってもよい (`true == x` = `x == true`、`0usize == v.len()` = `v.len() == 0usize`)。
    /// `a == None` / `None == a` → `a.is_none()`、`a != None` / `None != a` → `a.is_some()` (Option の判定の正規形)、
    /// `a == true` / `true == a` → `a`、`a != true` / `a == false` → `!a` (`!` は括弧ごと)、
    /// `x.len() == 0` / `0 == x.len()` → `x.is_empty()`、`!=` と `>` と (左が 0 の) `<` は `!x.is_empty()`。
    /// `None` と `len()` の正規形は std のメソッド (`is_none` / `is_some` / `is_empty`)。`own_len` (プログラムが
    /// `len` / `is_empty` を自分の型に定義している) のときは `len()` の比較を書き換えない (利用者の `len` と
    /// `is_empty` が別の意味かもしれず、型は字句の段階では分からないため)。
    fn cmp_rewrites(&mut self, s0: usize, op: usize, s1: usize, left_mc: Option<McInfo>, right_mc: Option<McInfo>) {
        let end = self.i;
        let ot = self.t[op].text.clone();
        let l = self.t[s0].clone();
        let r = self.t[s1].clone();
        // 両側とも 1 トークン (literal) か。`rone` は右が 1 トークン、`lone` は左が 1 トークン。
        let rone = end == s1 + 1;
        let lone = op == s0 + 1;
        if matches!(ot.as_str(), "==" | "!=") {
            // `None` との比較 (Option の判定)。残す側はもう片方の式。
            let none_keep = if rone && r.kind == "upper" && r.text == "None" {
                Some((s0, op))
            } else if lone && l.kind == "upper" && l.text == "None" {
                Some((s1, end))
            } else {
                None
            };
            if let Some(keep) = none_keep {
                if !self.fn_name.ends_with(".is_none") && !self.fn_name.ends_with(".is_some") {
                    let m = if ot == "==" { "is_none" } else { "is_some" };
                    self.rw.push(Rw {
                        a: s0,
                        b: end,
                        parts: vec![Part::Span(keep.0, keep.1), Part::Lit(format!(".{}()", m))],
                        del_line: false,
                        inside: false,
                    });
                }
                return;
            }
            // 真偽の literal: 片側だけが `true` / `false` のとき、もう片側を残す。
            let bool_lit = |t: &Tok| t.kind == "kw" && matches!(t.text.as_str(), "true" | "false");
            let (lit, keep_span) = if rone && bool_lit(&r) {
                (r.text.clone(), (s0, op))
            } else if lone && bool_lit(&l) {
                (l.text.clone(), (s1, end))
            } else {
                (String::new(), (0, 0))
            };
            if !lit.is_empty() {
                let keep = (ot == "==") == (lit == "true");
                let span = Part::Span(keep_span.0, keep_span.1);
                let parts = if keep { vec![span] } else { vec![Part::Lit("!(".into()), span, Part::Lit(")".into())] };
                self.rw.push(Rw { a: s0, b: end, parts, del_line: false, inside: false });
                return;
            }
        }
        // プログラムが `len` / `is_empty` を自分の型に定義しているときは、`len()` の比較を書き換えない
        // (利用者の `len` と `is_empty` が別の意味かもしれない。`x.is_empty()` が解決できないなら誤りになるが、意味は変わらない)。
        if self.own_len {
            return;
        }
        // `x.len() OP 0`: 右の literal が 0。
        if let Some(m) = left_mc.filter(|m| rone && zero_lit(&r) && m.name == "len" && m.close == m.open + 1 && matches!(ot.as_str(), "==" | "!=" | ">")) {
            if !self.fn_name.ends_with(".is_empty") {
                let mut parts = vec![];
                if ot != "==" {
                    parts.push(Part::Lit("!".into()));
                }
                parts.push(Part::Span(s0, m.dot));
                parts.push(Part::Lit(".is_empty()".into()));
                self.rw.push(Rw { a: s0, b: end, parts, del_line: false, inside: false });
            }
            return;
        }
        // `0 OP x.len()`: 左の literal が 0 (`0 < x.len()` は `x.len() > 0` と同じ)。
        if let Some(m) = right_mc.filter(|m| lone && zero_lit(&l) && m.name == "len" && m.close == m.open + 1 && matches!(ot.as_str(), "==" | "!=" | "<")) {
            if !self.fn_name.ends_with(".is_empty") {
                let mut parts = vec![];
                if ot != "==" {
                    parts.push(Part::Lit("!".into()));
                }
                parts.push(Part::Span(s1, m.dot));
                parts.push(Part::Lit(".is_empty()".into()));
                self.rw.push(Rw { a: s0, b: end, parts, del_line: false, inside: false });
            }
        }
    }

    pub(crate) fn bin_level(&mut self, lv: usize, ns: bool) -> R<V> {
        const LEVELS: [&[&str]; 5] = [&["|"], &["^"], &["&"], &["+", "-"], &["*", "/", "%"]];
        if lv == LEVELS.len() {
            return self.unary(ns);
        }
        let s0 = self.i;
        let mut l = self.bin_level(lv + 1, ns)?;
        while self.cur().kind == "op" && LEVELS[lv].contains(&self.cur().text.as_str()) {
            let op = self.cur().text.clone();
            let ln = self.cur().pos();
            self.opnd(s0, 4 + lv as u8, ns, "");
            self.i += 1;
            let s1 = self.i;
            let r = self.bin_level(lv + 1, ns)?;
            self.opnd(s1, 5 + lv as u8, ns, "");
            l = ndl(anchor(ln, &l), "Bin", vec![V::S(op), l, r]);
            self.lvl = 4 + lv as u8;
        }
        Ok(l)
    }

    pub(crate) fn unary(&mut self, ns: bool) -> R<V> {
        let c = self.cur().clone();
        if c.kind == "op" {
            if matches!(c.text.as_str(), "-" | "!" | "*") {
                let s_op = self.i;
                self.i += 1;
                let s1 = self.i;
                let e = self.unary(ns)?;
                self.opnd(s1, 9, ns, &c.text);
                let ol = self.lvl;
                if c.text == "!" {
                    self.not_rewrites(s_op, s1);
                }
                self.lvl = 9;
                self.last_unary = Some(UnInfo { start: s_op, end: self.i, op: c.text.clone(), operand_start: s1, operand_lvl: ol });
                return Ok(ndl(c.pos(), "Unary", vec![V::S(c.text), e]));
            }
            if c.text == "&" {
                self.i += 1;
                if self.accept_kw("mut") {
                    let s1 = self.i;
                    let e = self.unary(ns)?;
                    self.opnd(s1, 9, ns, "&mut");
                    let ol = self.lvl;
                    self.lvl = 9;
                    self.last_unary = Some(UnInfo { start: s1 - 2, end: self.i, op: "&mut".into(), operand_start: s1, operand_lvl: ol });
                    return Ok(ndl(c.pos(), "Unary", vec![s("&mut"), e]));
                }
                let s_op = self.i - 1;
                let s1 = self.i;
                let e = self.unary(ns)?;
                self.opnd(s1, 9, ns, "&");
                let ol = self.lvl;
                self.lvl = 9;
                self.last_unary = Some(UnInfo { start: s_op, end: self.i, op: "&".into(), operand_start: s1, operand_lvl: ol });
                return Ok(ndl(c.pos(), "Unary", vec![s("&"), e]));
            }
        }
        self.postfix(ns)
    }

    /// `!!x` / `!(!x)` → `x`、`!(a == b)` → `a != b` (D11)。`s_op` は外側の `!`、`s1` はその operand の始まり。
    fn not_rewrites(&mut self, s_op: usize, s1: usize) {
        let end = self.i;
        let direct = self.last_unary.clone().filter(|u| u.start == s1 && u.end == end && u.op == "!");
        let pinfo = self.pmap.get(&s1).filter(|p| p.close + 1 == end).cloned();
        let paren_un = pinfo.as_ref().and_then(|p| p.un.clone()).filter(|u| u.op == "!");
        if let Some(u) = direct.or(paren_un) {
            self.rw.push(Rw { a: s_op, b: end, parts: vec![Part::Span(u.operand_start, u.end)], del_line: false, inside: false });
            return;
        }
        if let Some(p) = pinfo {
            if let Some(op) = p.cmp {
                let flip = match self.t[op].text.as_str() {
                    "==" => " != ",
                    "!=" => " == ",
                    _ => return,
                };
                self.rw.push(Rw {
                    a: s_op,
                    b: end,
                    parts: vec![Part::Lit("(".into()), Part::Span(s1 + 1, op), Part::Lit(flip.into()), Part::Span(op + 1, p.close), Part::Lit(")".into())],
                    del_line: false,
                    inside: false,
                });
            }
        }
    }

    /// メソッド呼び出しの書き換え: `a.is_some()` → `a is Some(_)`、`a.eq(&b)` → `a == b`、`(*r).m()` → `r.m()` (D8・D11)。
    fn note_mcall(&mut self, s0: usize, dot: usize, name: &str, open: usize, close: usize, has_targs: bool) {
        let info = McInfo { start: s0, end: close + 1, dot, name: name.to_string(), open, close };
        let empty = close == open + 1;
        if !has_targs {
            if empty {
                // `is_some()` / `is_none()` / `is_ok()` / `is_err()` は、`is` に置き換えない: `is` は値を消費するので、
                // 線形の値 (`Option<ハンドル>`) の判定ができなくなる。型が要るので、fmt では決められない。
                let pat: Option<&str> = None;
                if let Some(p) = pat.filter(|_| !self.fn_name.ends_with(&format!(".{}", name))) {
                    self.rw.push(Rw { a: s0, b: close + 1, parts: vec![Part::Lit("(".into()), Part::Span(s0, dot), Part::Lit(p.into())], del_line: false, inside: false });
                }
            } else if matches!(name, "eq" | "ne") && !self.fn_name.ends_with(&format!(".{}", name)) {
                if let Some(u) = self.last_unary.clone().filter(|u| u.start == open + 1 && u.end == close && u.op == "&") {
                    let op = if name == "eq" { " == " } else { " != " };
                    self.rw.push(Rw {
                        a: s0,
                        b: close + 1,
                        parts: vec![Part::Lit("(".into()), Part::Span(s0, dot), Part::Lit(op.into()), Part::Span(u.operand_start, close), Part::Lit(")".into())],
                        del_line: false,
                    inside: false,
                    });
                }
            }
        }
        // 受け手の `(*r)` / `(&p)` / `(&mut p)` (自動参照・参照外しがあるので括弧も `*` `&` も要らない)。
        if let Some(p) = self.pmap.get(&s0).filter(|p| p.close + 1 == dot).cloned() {
            // `(*r)` は常に、`(&p)` `(&mut p)` は借りる相手が単純な場所 (名前と `.` だけ) のときだけ (一時値の借用は意味が変わる)。
            let place = |u: &UnInfo, t: &[Tok], close: usize| t[u.operand_start..close].iter().all(|k| matches!(k.kind, "ident" | "upper") || k.text == "." || k.text == "self");
            let ok = p.un.clone().filter(|u| u.operand_lvl >= 10 && (u.op == "*" || (matches!(u.op.as_str(), "&" | "&mut") && place(u, &self.t, p.close))));
            if let Some(u) = ok {
                self.rw.push(Rw { a: s0, b: dot, parts: vec![Part::Span(u.operand_start, p.close)], del_line: false, inside: false });
            }
        }
        self.last_mcall = Some(info);
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
        let s0 = self.i;
        let mut e = self.primary(ns)?;
        let mut first = true;
        loop {
            let c = self.cur().clone();
            if c.kind != "op" {
                break;
            }
            if first && matches!(c.text.as_str(), "." | "[" | "?") {
                self.opnd(s0, 10, ns, "");
            }
            if matches!(c.text.as_str(), "." | "(" | "[" | "?") {
                first = false;
                self.lvl = 10;
            }
            match c.text.as_str() {
                "." => {
                    let dot = self.i;
                    self.i += 1;
                    let name_pos = self.cur().pos();
                    let nm = self.name_tok()?;
                    let mut targs = V::Nil;
                    if self.is_op("::") {
                        self.i += 1;
                        targs = V::L(self.targs()?);
                    }
                    if self.is_op("(") {
                        let open = self.i;
                        let args = self.call_args()?;
                        self.note_mcall(s0, dot, &nm, open, self.i - 1, !matches!(targs, V::Nil));
                        e = ndl(anchor(c.pos(), &e), "MCall", vec![e, V::S(nm), args, targs, V::N(name_pos.to_string())]);
                    } else {
                        e = ndl(anchor(c.pos(), &e), "Field", vec![e, V::S(nm), V::N(name_pos.to_string())]);
                    }
                }
                "(" => {
                    let open = self.i;
                    let args = self.call_args()?;
                    // `a.m(..)` の `a.m` は 1 つの経路 (`Path`) として解析される。最後の成分をメソッド名として書き換えの候補にする。
                    if let V::Node("Path", pf, _, _) = &e {
                        if let Some(V::L(segs)) = pf.first() {
                            if segs.len() >= 2 && matches!(pf.get(1), Some(V::Nil) | None) && open >= 2 && self.t[open - 2].kind == "op" && self.t[open - 2].text == "." {
                                let nm = self.t[open - 1].text.clone();
                                self.note_mcall(s0, open - 2, &nm, open, self.i - 1, false);
                            }
                        }
                    }
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
        let r = self.primary_inner(ns)?;
        let block = (c.kind == "kw" && matches!(c.text.as_str(), "if" | "match" | "loop" | "while" | "for" | "unsafe" | "with" | "fn"))
            || (c.kind == "op" && c.text == "{");
        if block {
            self.lvl = 0;
        } else {
            self.lvl = 10;
        }
        Ok(r)
    }

    fn primary_inner(&mut self, ns: bool) -> R<V> {
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
                    let open = self.i;
                    self.i += 1;
                    if self.accept_op(")") {
                        self.lvl = 10;
                        return Ok(ndl(c.pos(), "Lit", vec![s("unit")]));
                    }
                    let first = self.expr(false)?;
                    let ilvl = self.lvl;
                    if self.accept_op(")") {
                        let close = self.i - 1;
                        let mut depth = 0i32;
                        let mut brace = false;
                        for t in &self.t[open + 1..close] {
                            if t.kind == "op" {
                                match t.text.as_str() {
                                    "(" | "[" => depth += 1,
                                    ")" | "]" => depth -= 1,
                                    "{" if depth == 0 => brace = true,
                                    _ => {}
                                }
                            }
                        }
                        let un = self.last_unary.clone().filter(|u| u.start == open + 1 && u.end == close);
                        let cmp = self.last_cmp.filter(|c| c.0 == open + 1 && c.1 == close).map(|c| c.2);
                        self.pmap.insert(open, PInfo { close, lvl: ilvl, brace, un, cmp });
                        self.lvl = 10;
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
                if self.is_kw("if") { self.if_expr()? } else { self.else_block()? }
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
            els = if self.is_kw("if") { self.if_expr()? } else { self.else_block()? };
        }
        Ok(ndl(ln, "If", vec![cond, then, els]))
    }

    /// `else` の後のブロック。`else { if .. }` は `else if ..` にする (S15)。
    fn else_block(&mut self) -> R<V> {
        let b = self.block()?;
        if let Some(bi) = self.last_block.clone() {
            if bi.nstmts == 1 && bi.if_stmt {
                self.inline_block(&bi, true);
            }
        }
        Ok(b)
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
            let sb = self.i;
            let body = self.expr(false)?;
            if let Some(b) = self.last_block.clone() {
                if b.open == sb && b.close + 1 == self.i && b.nstmts == 1 && b.expr_stmt && !b.unit_stmt {
                    self.inline_block(&b, false);
                }
                if b.open == sb && b.close + 1 == self.i && b.unit_stmt {
                    // `=> {` の中の `()` だけ: 腕の本体を `()` にする。
                    self.rw.push(Rw { a: b.open, b: b.close + 1, parts: vec![Part::Lit("()".into())], del_line: false, inside: false });
                }
            }
            arms.push(V::L(vec![pat, body]));
            self.expect_nl()?;
        }
        self.expect_op("}")?;
        Ok(ndl(ln, "Match", vec![scrut, V::L(arms)]))
    }

    /// 式文の `()` (D15): 文が 2 つ以上あるブロックでは、その行を消す。
    fn unit_stmts(&mut self, units: &[(usize, usize)], n: usize) {
        if n < 2 {
            return;
        }
        for &(k, si) in units {
            // 最後の `()` は、前の文の値を捨てる (E2042) 役を担うので消さない (消すとブロックの値が変わる)。
            if k + 1 == n {
                continue;
            }
            self.rw.push(Rw { a: si, b: si + 2, parts: vec![], del_line: true, inside: false });
        }
    }

    /// 1 つの文だけのブロック `{ 文 }` (`{` の後と `}` の前で改行) の `{` から `}` までを、文だけに置き換える (D15 の S21・D11 の S15)。
    fn inline_block(&mut self, b: &BlInfo, inside: bool) {
        let (o, c) = (b.open, b.close);
        if c < o + 4 || self.t[o + 1].kind != "nl" || self.t[c - 1].kind != "nl" {
            return;
        }
        self.rw.push(Rw { a: o, b: c + 1, parts: vec![Part::Dedent(o + 2, c - 1)], del_line: false, inside });
    }

    pub fn block(&mut self) -> R<V> {
        let ln = self.cur().pos();
        let open = self.i;
        self.expect_op("{")?;
        self.skip_nl();
        let mut stmts = Vec::new();
        let mut units: Vec<(usize, usize)> = vec![];
        while !self.is_op("}") {
            let si = self.i;
            let st = self.stmt()?;
            if self.i == si + 2 && matches!(&st, V::Node("ExprStmt", c, _, _) if matches!(c.first(), Some(V::Node("Lit", l, _, _)) if matches!(l.first(), Some(V::S(k)) if k == "unit"))) {
                units.push((stmts.len(), si));
            }
            stmts.push(st);
            if !self.is_op("}") {
                self.expect_nl()?;
            }
        }
        let close = self.i;
        self.expect_op("}")?;
        self.unit_stmts(&units, stmts.len());
        let first = stmts.first();
        let expr_stmt = matches!(first, Some(V::Node("ExprStmt", _, _, _)));
        let if_stmt = matches!(first, Some(V::Node("ExprStmt", c, _, _)) if matches!(c.first(), Some(V::Node("If" | "Match", _, _, _))))
            && self.t.get(open + 2).map_or(false, |t| t.kind == "kw" && t.text == "if");
        self.last_block = Some(BlInfo { open, close, nstmts: stmts.len(), expr_stmt, if_stmt, unit_stmt: units.len() == 1 && stmts.len() == 1 });
        Ok(ndl(ln, "Block", vec![V::L(stmts)]))
    }

    pub fn stmt(&mut self) -> R<V> {
        let ln = self.cur().pos();
        if self.is_kw("let") {
            self.i += 1;
            let m = self.accept_kw("mut");
            let pat = self.pattern()?;
            let mut ty = V::Nil;
            // 注釈に寿命 (`'a`・`'static`) があるか。型の木には残らない (`ref_rest` が読み捨てる) ので、5 つ目の欄に印を付ける (D17)。
            let mut ann_life = false;
            if self.accept_op(":") {
                let start = self.i;
                ty = self.ty()?;
                ann_life = self.t[start..self.i].iter().any(|t| t.kind == "life");
            }
            self.expect_op("=")?;
            let e = self.expr(false)?;
            let mut fs = vec![pat, V::B(m), ty, e];
            if ann_life {
                fs.push(V::B(true));
            }
            return Ok(ndl(ln, "Let", fs));
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
            if self.is_op("{") && !self.pat_ns {
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

/// 本体のトークン列 (`nl` を含む。`eof` は無くてよい) の中の、冗長な括弧の群 `(開き, 閉じ)` (D10)。解析に失敗したら空。
pub fn redundant_parens(mut toks: Vec<Tok>) -> Vec<(Tok, Tok)> {
    let (line, col) = toks.last().map_or((1, 1), |t| (t.line, t.col + 1));
    toks.push(Tok { kind: "eof", text: String::new(), line, col });
    let mut p = Parser::new(toks);
    p.skip_nl();
    while !p.at_eof() {
        if p.stmt().is_err() || p.expect_nl().is_err() {
            return vec![];
        }
    }
    let mut r = p.red.clone();
    r.sort();
    r.dedup();
    r.into_iter().map(|(a, b)| (p.t[a].clone(), p.t[b].clone())).collect()
}

/// 本体のトークン列 (`nl` を含む) の書き換え (D8・D10・D11・D15)。返すのはトークン列と、トークン区間の書き換え。
/// 冗長な括弧は、`(` と `)` それぞれを「何も無い部品」に置き換える書き換えとして含める。解析に失敗したら空。
/// `own_len` はプログラムが `len` / `is_empty` を自分の型に定義しているか (D11 の `len()` の書き換えをしないため。
/// 型は字句の段階では分からないので `fmt` の呼び出し側が決める)。
pub fn rewrites(mut toks: Vec<Tok>, fn_name: &str, own_len: bool) -> (Vec<Tok>, Vec<Rw>) {
    let (line, col) = toks.last().map_or((1, 1), |t| (t.line, t.col + 1));
    toks.push(Tok { kind: "eof", text: String::new(), line, col });
    let mut p = Parser::new(toks);
    p.fn_name = fn_name.to_string();
    p.own_len = own_len;
    p.skip_nl();
    let mut units: Vec<(usize, usize)> = vec![];
    let mut n = 0;
    while !p.at_eof() {
        let si = p.i;
        let st = match p.stmt() {
            Ok(s) => s,
            Err(_) => return (vec![], vec![]),
        };
        if p.i == si + 2 && matches!(&st, V::Node("ExprStmt", c, _, _) if matches!(c.first(), Some(V::Node("Lit", l, _, _)) if matches!(l.first(), Some(V::S(k)) if k == "unit"))) {
            units.push((n, si));
        }
        n += 1;
        if p.expect_nl().is_err() {
            return (vec![], vec![]);
        }
    }
    p.unit_stmts(&units, n);
    let mut red = p.red.clone();
    red.sort();
    red.dedup();
    for (a, b) in red {
        p.rw.push(Rw { a, b: a + 1, parts: vec![], del_line: false, inside: false });
        p.rw.push(Rw { a: b, b: b + 1, parts: vec![], del_line: false, inside: false });
    }
    (p.t, p.rw)
}
