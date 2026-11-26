//! `panics never` の関数で整数の `+` / `-` を許す範囲の証明 (4.1 の算術、2026-11-26 の決定 案 C)。
//! 関数の中・構文の形だけ・流れに沿う・迷えば証明しない (fail-closed)。
//! 証明する形:
//! - `i + 1` (と `1 + i`): 直前に支配する条件 `i < n` (`while` の条件、`if` の then / `else` の経路、`&&` の右) が成り立つとき。
//! - `x - k` (k は整数リテラル): `x >= k`・`x > k - 1`・`k <= x`・`x > 0` (k = 1)、符号なしなら `x != 0` のあと。
//! - `v.len() - 1`: `!v.is_empty()` や `v.len() > 0` のあと (`v.len()` を 1 つの項として扱う)。
//! 間にその変数への代入・`&mut` の借用・(`v.len()` なら読み取り以外の使い方) があれば事実は消える。
//! 掛け算・割り算・剰余は証明しない。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::check::{children, is_nil, items, nf, nline, nname, pat_binds, st, strs};
use crate::parser::{nid, V};
use crate::program::Def;

/// 移行のための単一のスイッチ: 証明できる所の `wrapping_add` / `wrapping_sub` を誤りにするか。
/// 今は false (check は通し、`ksn fix` が `+` / `-` に直す)。書き換えが終わったら true にする。
pub const PROVEN_WRAPPING_IS_ERROR: bool = false;

/// 範囲の証明を使うか。公開アルファ (0.1.0-alpha.1) では切る: 証明の取りこぼし (Mythos6 AR1/AR2) と、
/// 証明した `+`/`-` を KIR が下ろせない件 (AR4) が残るため。切っている間は E4004 がいつも出て、`ksn fix` も書き換えない。
pub const ENABLED: bool = false;

/// 証明できた節点。
#[derive(Clone, Debug)]
pub struct Site {
    /// 符号なしの整数でだけ成り立つ証明か (`x != 0` からの `x - 1`)。
    pub unsigned_only: bool,
    /// `x.wrapping_add(c)` / `x.wrapping_sub(k)` の形なら、(位置, 受け手の文面, "add"/"sub", リテラル)。
    pub wrap: Option<(usize, String, &'static str, String)>,
}

/// `ksn fix` が読む、証明できる `wrapping_*` の場所 (ファイル, 位置, 受け手, 演算, リテラル)。
pub static WRAP_SITES: Mutex<Vec<(String, usize, String, &'static str, String)>> = Mutex::new(Vec::new());

#[derive(Clone, Default)]
struct Facts {
    lt: HashSet<String>,
    ge: HashMap<String, u128>,
    ge_u: HashMap<String, u128>,
    locals: HashSet<String>,
}

fn base_of(k: &str) -> &str {
    k.strip_suffix("#len").unwrap_or(k)
}

impl Facts {
    /// 名前 `k` (`x`) なら `x` と `x#len` を、`x#len` ならそれだけを消す。
    fn remove_key(&mut self, k: &str) {
        let mut ks = vec![k.to_string()];
        if !k.ends_with("#len") {
            ks.push(format!("{k}#len"));
        }
        for k in ks {
            self.lt.remove(&k);
            self.ge.remove(&k);
            self.ge_u.remove(&k);
        }
    }
    fn remove_all(&mut self, ks: &HashSet<String>) {
        for k in ks {
            self.remove_key(k);
        }
    }
    fn minus(&self, ks: &HashSet<String>) -> Facts {
        let mut f = self.clone();
        f.remove_all(ks);
        f
    }
    fn add_ge(&mut self, k: String, v: u128) {
        let e = self.ge.entry(k).or_insert(0);
        *e = (*e).max(v);
    }
}

const READ_METHODS: &[&str] = &["len", "is_empty", "get", "iter", "first", "last", "contains", "starts_with", "ends_with"];

fn lit_u(e: &V) -> Option<u128> {
    if nname(e) != "Lit" || st(&nf(e)[0]) != "int" {
        return None;
    }
    let t = st(&nf(e)[1]).replace('_', "");
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

fn path1(e: &V) -> Option<String> {
    if nname(e) != "Path" {
        return None;
    }
    let s = strs(&nf(e)[0]);
    (s.len() == 1).then(|| s[0].clone())
}

/// `v.len()` / `v.is_empty()` の形 (`Call(Path [v, m], [])` または `MCall(Path [v], m, [])`): (v, m)。
fn recv_method(e: &V) -> Option<(String, String)> {
    let f = nf(e);
    match nname(e) {
        "Call" if nname(&f[0]) == "Path" => {
            let s = strs(&nf(&f[0])[0]);
            (s.len() == 2 && items(&f[1]).is_empty()).then(|| (s[0].clone(), s[1].clone()))
        }
        "MCall" => {
            let r = path1(&f[0])?;
            items(&f[2]).is_empty().then(|| (r, st(&f[1])))
        }
        _ => None,
    }
}

/// 項の鍵: 局所の名前 `x`、または `v.len()` の `v#len`。
fn key(e: &V) -> Option<String> {
    if let Some(n) = path1(e) {
        return Some(n);
    }
    match recv_method(e) {
        Some((v, m)) if m == "len" => Some(format!("{v}#len")),
        _ => None,
    }
}

fn is_local_key(k: &str, f: &Facts, poison: &HashSet<String>) -> bool {
    let b = base_of(k);
    f.locals.contains(b) && !poison.contains(b)
}

/// 部分木が変えうる鍵の集合。代入の根・`&mut` の根・(`v#len` は) 読み取り以外の使い方。
fn kills(e: &V, out: &mut HashSet<String>) {
    fn root(t: &V) -> Option<String> {
        match nname(t) {
            "Path" => strs(&nf(t)[0]).first().cloned(),
            "Field" | "Index" => root(&nf(t)[0]),
            "Unary" => root(&nf(t)[1]),
            "Paren" => root(&nf(t)[0]),
            _ => None,
        }
    }
    let f = nf(e);
    match nname(e) {
        "Assign" => {
            if let Some(r) = root(&f[0]) {
                out.insert(r);
            }
        }
        "Unary" if st(&f[0]) == "&mut" => {
            if let Some(r) = root(&f[1]) {
                out.insert(r);
            }
        }
        "Path" => {
            let s = strs(&f[0]);
            if s.len() == 1 {
                out.insert(format!("{}#len", s[0]));
            }
            return;
        }
        "Call" if nname(&f[0]) == "Path" => {
            let s = strs(&nf(&f[0])[0]);
            if s.len() == 2 && !READ_METHODS.contains(&s[1].as_str()) {
                out.insert(format!("{}#len", s[0]));
            }
            if s.len() == 1 {
                // 関数の名前: 何もしない
            }
            for a in items(&f[1]) {
                kills(a, out);
            }
            return;
        }
        "MCall" if path1(&f[0]).is_some() && READ_METHODS.contains(&st(&f[1]).as_str()) => {
            for a in items(&f[2]) {
                kills(a, out);
            }
            return;
        }
        "Index" if path1(&f[0]).is_some() => {
            kills(&f[1], out);
            return;
        }
        _ => {}
    }
    for c in children(e) {
        kills(c, out);
    }
}

fn kills_of(e: &V) -> HashSet<String> {
    let mut s = HashSet::new();
    kills(e, &mut s);
    s
}

fn negate(op: &str) -> &'static str {
    match op {
        "<" => ">=",
        "<=" => ">",
        ">" => "<=",
        ">=" => "<",
        "==" => "!=",
        _ => "==",
    }
}

struct W {
    poison: HashSet<String>,
    out: HashMap<u32, Site>,
    file: String,
    never: bool,
}

impl W {
    /// 条件 `c` が `truth` のときに成り立つ事実を足す。
    fn facts(&self, c: &V, truth: bool, f: &mut Facts) {
        let a = nf(c);
        match nname(c) {
            "Unary" if st(&a[0]) == "!" => self.facts(&a[1], !truth, f),
            "Bin" => {
                let op = st(&a[0]);
                match op.as_str() {
                    "&&" => {
                        if truth {
                            self.facts(&a[1], true, f);
                            self.facts(&a[2], true, f);
                        }
                    }
                    "||" => {
                        if !truth {
                            self.facts(&a[1], false, f);
                            self.facts(&a[2], false, f);
                        }
                    }
                    "<" | "<=" | ">" | ">=" | "==" | "!=" => {
                        let op = if truth { op.as_str() } else { negate(&op) };
                        self.cmp(op, &a[1], &a[2], f);
                    }
                    _ => {}
                }
            }
            _ => {
                if !truth {
                    if let Some((v, m)) = recv_method(c) {
                        if m == "is_empty" && f.locals.contains(&v) && !self.poison.contains(&v) {
                            f.add_ge(format!("{v}#len"), 1);
                        }
                    }
                }
            }
        }
    }

    fn cmp(&self, op: &str, a: &V, b: &V, f: &mut Facts) {
        match op {
            ">" => self.cmp("<", b, a, f),
            ">=" => self.cmp("<=", b, a, f),
            "<" => {
                // a < b: a + 1 は溢れない。a が (リテラル k なら) b >= k + 1。
                if let Some(k) = key(a) {
                    if is_local_key(&k, f, &self.poison) {
                        f.lt.insert(k);
                    }
                }
                if let (Some(n), Some(k)) = (lit_u(a), key(b)) {
                    if is_local_key(&k, f, &self.poison) {
                        f.add_ge(k, n + 1);
                    }
                }
            }
            "<=" => {
                // a <= b: a がリテラル k なら b >= k。
                if let (Some(n), Some(k)) = (lit_u(a), key(b)) {
                    if is_local_key(&k, f, &self.poison) {
                        f.add_ge(k, n);
                    }
                }
            }
            "!=" => {
                for (x, y) in [(a, b), (b, a)] {
                    if lit_u(x) == Some(0) {
                        if let Some(k) = key(y) {
                            if is_local_key(&k, f, &self.poison) {
                                let e = f.ge_u.entry(k).or_insert(0);
                                *e = (*e).max(1);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// `l + c` / `l - c` の証明 (`l` と `c` は演算子の両辺。`add` なら足し算)。
    fn prove(&self, add: bool, l: &V, r: &V, f: &Facts) -> Option<bool> {
        if add {
            for (x, y) in [(l, r), (r, l)] {
                if lit_u(y) == Some(1) {
                    if let Some(k) = key(x) {
                        if f.lt.contains(&k) && is_local_key(&k, f, &self.poison) {
                            return Some(false);
                        }
                    }
                }
            }
            None
        } else {
            let n = lit_u(r)?;
            let k = key(l)?;
            if !is_local_key(&k, f, &self.poison) {
                return None;
            }
            if f.ge.get(&k).is_some_and(|&g| g >= n) {
                return Some(false);
            }
            if f.ge_u.get(&k).is_some_and(|&g| g >= n) {
                return Some(true);
            }
            None
        }
    }

    fn site(&mut self, e: &V, unsigned_only: bool, wrap: Option<(String, &'static str, String)>) {
        let id = nid(e);
        if id == 0 {
            return;
        }
        let wrap = wrap.map(|(r, o, l)| (nline(e), r, o, l));
        if let (Some(w), true) = (&wrap, self.never && !unsigned_only) {
            WRAP_SITES.lock().unwrap().push((self.file.clone(), w.0, w.1.clone(), w.2, w.3.clone()));
        }
        self.out.insert(id, Site { unsigned_only, wrap });
    }

    fn node_proof(&mut self, e: &V, f: &Facts) {
        let a = nf(e);
        match nname(e) {
            "Bin" if matches!(st(&a[0]).as_str(), "+" | "-") => {
                if let Some(u) = self.prove(st(&a[0]) == "+", &a[1], &a[2], f) {
                    self.site(e, u, None);
                }
            }
            "Call" | "MCall" => {
                let (recv_text, recv_e, name, args): (String, Option<&V>, String, &[V]) = if nname(e) == "Call" && nname(&a[0]) == "Path" {
                    let s = strs(&nf(&a[0])[0]);
                    if s.len() != 2 {
                        return;
                    }
                    (s[0].clone(), None, s[1].clone(), items(&a[1]))
                } else if nname(e) == "MCall" {
                    let t = match recv_method(&a[0]) {
                        Some((v, m)) if m == "len" => format!("{v}.len()"),
                        _ => match path1(&a[0]) {
                            Some(n) => n,
                            None => return,
                        },
                    };
                    (t, Some(&a[0]), st(&a[1]), items(&a[2]))
                } else {
                    return;
                };
                let (add, op) = match name.as_str() {
                    "wrapping_add" => (true, "add"),
                    "wrapping_sub" => (false, "sub"),
                    _ => return,
                };
                if args.len() != 1 {
                    return;
                }
                let lit = &args[0];
                if lit_u(lit).is_none() {
                    return;
                }
                let lit_text = st(&nf(lit)[1]);
                // 受け手の項: `x` か `v.len()`
                let recv_key = match recv_e {
                    Some(r) => key(r),
                    None => Some(recv_text.clone()),
                };
                let Some(rk) = recv_key else { return };
                let proven = if add {
                    lit_u(lit) == Some(1) && f.lt.contains(&rk) && is_local_key(&rk, f, &self.poison)
                } else {
                    let n = lit_u(lit).unwrap_or(u128::MAX);
                    if !is_local_key(&rk, f, &self.poison) {
                        return;
                    }
                    if f.ge.get(&rk).is_some_and(|&g| g >= n) {
                        self.site(e, false, Some((recv_text, op, lit_text)));
                        return;
                    }
                    if f.ge_u.get(&rk).is_some_and(|&g| g >= n) {
                        self.site(e, true, Some((recv_text, op, lit_text)));
                    }
                    return;
                };
                if proven {
                    self.site(e, false, Some((recv_text, op, lit_text)));
                }
            }
            _ => {}
        }
    }

    fn diverges(b: &V) -> bool {
        let last = match nname(b) {
            "Block" => items(&nf(b)[0]).last(),
            _ => return false,
        };
        let Some(s) = last else { return false };
        let inner = if nname(s) == "ExprStmt" { &nf(s)[0] } else { return false };
        matches!(nname(inner), "Return" | "Break" | "Continue")
    }

    fn block(&mut self, stmts: &[V], mut f: Facts) {
        for s in stmts {
            let a = nf(s);
            match nname(s) {
                "Let" => {
                    self.expr(&a[3], &f);
                    f.remove_all(&kills_of(&a[3]));
                    let mut bs = vec![];
                    pat_binds(&a[0], &mut bs);
                    for b in bs {
                        f.remove_key(&b);
                        f.locals.insert(b);
                    }
                }
                "Assign" => {
                    self.expr(&a[1], &f);
                    self.expr(&a[0], &f);
                    f.remove_all(&kills_of(s));
                }
                "ExprStmt" => {
                    let e = &a[0];
                    self.expr(e, &f);
                    let k = kills_of(e);
                    f.remove_all(&k);
                    if nname(e) == "If" {
                        let c = nf(e);
                        let kc = kills_of(&c[0]);
                        let mut g = Facts { locals: f.locals.clone(), ..Facts::default() };
                        if is_nil(&c[2]) && Self::diverges(&c[1]) {
                            self.facts(&c[0], false, &mut g);
                        } else if !is_nil(&c[2]) && Self::diverges(&c[2]) && !Self::diverges(&c[1]) {
                            self.facts(&c[0], true, &mut g);
                        }
                        g.remove_all(&kc);
                        for k in g.lt {
                            f.lt.insert(k);
                        }
                        for (k, v) in g.ge {
                            f.add_ge(k, v);
                        }
                        for (k, v) in g.ge_u {
                            let e = f.ge_u.entry(k).or_insert(0);
                            *e = (*e).max(v);
                        }
                    }
                }
                _ => {
                    self.expr(s, &f);
                    f.remove_all(&kills_of(s));
                }
            }
        }
    }

    fn expr(&mut self, e: &V, f: &Facts) {
        self.node_proof(e, f);
        let a = nf(e);
        match nname(e) {
            "Block" => self.block(items(&a[0]), f.clone()),
            "If" => {
                self.expr(&a[0], f);
                let kc = kills_of(&a[0]);
                let base = f.minus(&kc);
                let mut t = base.clone();
                self.facts(&a[0], true, &mut t);
                t.remove_all(&kc);
                self.expr(&a[1], &t);
                if !is_nil(&a[2]) {
                    let mut el = base;
                    self.facts(&a[0], false, &mut el);
                    el.remove_all(&kc);
                    self.expr(&a[2], &el);
                }
            }
            "While" => {
                let k = kills_of(e);
                let entry = f.minus(&k);
                self.expr(&a[0], &entry);
                let kc = kills_of(&a[0]);
                let mut body = entry;
                self.facts(&a[0], true, &mut body);
                body.remove_all(&kc);
                self.expr(&a[1], &body);
            }
            "Loop" => {
                let entry = f.minus(&kills_of(e));
                self.expr(&a[0], &entry);
            }
            "For" => {
                self.expr(&a[1], f);
                let mut entry = f.minus(&kills_of(e));
                let mut bs = vec![];
                pat_binds(&a[0], &mut bs);
                for b in bs {
                    entry.remove_key(&b);
                    entry.locals.insert(b);
                }
                self.expr(&a[2], &entry);
            }
            "Match" => {
                self.expr(&a[0], f);
                let base = f.minus(&kills_of(&a[0]));
                for arm in items(&a[1]) {
                    let xs = items(arm);
                    let mut g = base.clone();
                    let mut bs = vec![];
                    if let Some(p) = xs.first() {
                        pat_binds(p, &mut bs);
                    }
                    for b in bs {
                        g.remove_key(&b);
                        g.locals.insert(b);
                    }
                    let ka: HashSet<String> = xs.iter().skip(1).flat_map(|x| kills_of(x)).collect();
                    let _ = ka;
                    for x in xs.iter().skip(1) {
                        self.expr(x, &g);
                    }
                }
            }
            "Closure" => {
                let mut g = Facts::default();
                g.locals = f.locals.clone();
                for c in children(e) {
                    if !crate::check::is_pat(c) {
                        self.expr(c, &g);
                    }
                }
            }
            "Bin" if matches!(st(&a[0]).as_str(), "&&" | "||") => {
                self.expr(&a[1], f);
                let kc = kills_of(&a[1]);
                let mut g = f.minus(&kc);
                self.facts(&a[1], st(&a[0]) == "&&", &mut g);
                g.remove_all(&kc);
                self.expr(&a[2], &g);
            }
            _ => {
                let k = kills_of(e);
                let g = if k.is_empty() { f.clone() } else { f.minus(&k) };
                for c in children(e) {
                    self.expr(c, &g);
                }
            }
        }
    }
}

/// 閉包の中で代入・`&mut` される名前 (どの場所でも事実を作らない)。
fn poisoned(body: &V, out: &mut HashSet<String>) {
    if nname(body) == "Closure" {
        for k in kills_of(body) {
            out.insert(base_of(&k).to_string());
        }
    }
    for c in children(body) {
        poisoned(c, out);
    }
}

/// 関数 `d` の本体で証明できた `+` / `-` (と `wrapping_*`) の節点。
pub fn prove(d: &Def) -> HashMap<u32, Site> {
    if !ENABLED {
        return HashMap::new();
    }
    let Some(body) = &d.body else { return HashMap::new() };
    let mut poison = HashSet::new();
    poisoned(body, &mut poison);
    let mut w = W { poison, out: HashMap::new(), file: d.file.clone(), never: d.panics.as_deref() == Some("never") };
    let mut f = Facts::default();
    for (n, _) in &d.params {
        f.locals.insert(n.clone());
    }
    w.expr(body, &f);
    w.out
}

/// `ksn fix`: 証明できる `x.wrapping_add(c)` / `x.wrapping_sub(k)` を `x + c` / `x - k` に直す案。
/// 検査 (`check`) が溜めた `WRAP_SITES` を読み、行の文面で見つかる同じ形の数と場所の数が合うときだけ、
/// 式が単独に立つ文脈 (前後の字が優先順位を変えない所) で書き換える。
pub fn wrap_fixes() -> Vec<crate::fixes::Fix> {
    let sites = std::mem::take(&mut *WRAP_SITES.lock().unwrap());
    let mut groups: std::collections::BTreeMap<(String, usize, String, &'static str), usize> = Default::default();
    let mut seen = std::collections::BTreeSet::new();
    for s in sites {
        if !seen.insert(s.clone()) {
            continue;
        }
        let line = crate::parser::unpack_pos(s.1).0;
        *groups.entry((s.0, line, s.2, s.3)).or_insert(0) += 1;
    }
    let mut out = vec![];
    for ((file, line, recv, op), count) in groups {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let Some(l) = text.split('\n').nth(line.wrapping_sub(1)) else { continue };
        let needle = format!("{recv}.wrapping_{op}(");
        let ident = |c: char| c.is_alphanumeric() || c == '_';
        let mut found = vec![];
        let mut from = 0;
        while let Some(i) = l[from..].find(&needle) {
            let a = from + i;
            from = a + needle.len();
            let rest = &l[from..];
            let Some(close) = rest.find(')') else { break };
            let arg = &rest[..close];
            if arg.is_empty() || !arg.chars().next().unwrap().is_ascii_digit() || !arg.chars().all(ident) {
                continue;
            }
            let end = from + close + 1;
            let before = l[..a].chars().last();
            if before.is_some_and(|c| ident(c) || c == '.') {
                continue;
            }
            let b = l[..a].trim_end();
            let prev_ok = b.is_empty() || b.ends_with(['=', '<', '>', '(', ',', '[']) || b.ends_with("return") || b.ends_with("if") || b.ends_with("while");
            let after = l[end..].trim_start();
            let next_ok = after.is_empty() || after.starts_with([')', ',', ']', '}', ';', '{']) || after.starts_with("==") || after.starts_with("!=") || after.starts_with('<') || after.starts_with('>');
            if prev_ok && next_ok {
                found.push((a, end, arg.to_string()));
            }
            from = end;
        }
        if found.len() != count || count != l.matches(&needle).count() {
            continue;
        }
        let ci = |byte: usize| l[..byte].chars().count();
        for (a, end, arg) in found {
            out.push(crate::fixes::Fix {
                file: file.clone(),
                line,
                col: ci(a) + 1,
                end_line: line,
                end_col: ci(end) + 1,
                replace: format!("{} {} {}", recv, if op == "add" { "+" } else { "-" }, arg),
            });
        }
    }
    out
}
