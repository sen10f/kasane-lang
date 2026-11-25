//! 機械的な言い換えの生成器 (設計_型付き検査.md 4.1 / 5 章 #9)。
//!
//! `data/unsound/` の各ケース (通ってはいけないコード) に、意味を変えない変換を 1 か所ずつ当てて `ksn check` に掛け、
//! 元と同じ診断コードで拒まれることを期待する。通ってしまう言い換え (= 検査の抜け) と、別のコードに変わる言い換えは
//! 台帳 `data/rephrase_known.txt` に載せる。台帳に無いものが出たら失敗し、台帳にあって出なくなったもの (塞いだ) も失敗する
//! (`KNOWN_HOLES` と同じ作法)。`data/linear_ok/` (正しいコード) に同じ変換を当てて拒まれるもの (誤検出) は
//! `data/rephrase_known_fp.txt` に載せる。台帳はどちらも既定の検査器 (s4、2026-11-20 に old から切り替え) の結果。台帳を作り直すには `REPHRASE_UPDATE=1 cargo test --release --test rephrase`。
//! 1 ケース 1 変換あたりの言い換えの数は `REPHRASE_MAX_SITES` (既定 6) で抑える。
//!
//! 変換は行と字下げに基づく簡易なもの (文法を完全には読まない)。文法上できない入力は飛ばして数える。

mod common;
use common::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

// ---------------------------------------------------------------- 字句の補助

/// 文字列・文字リテラル・行コメントを空白に置き換える (バイト長は保つ)。
fn mask(line: &str) -> String {
    let cs: Vec<(usize, char)> = line.char_indices().collect();
    let mut out = String::new();
    let mut i = 0;
    let sp = |out: &mut String, c: char| {
        for _ in 0..c.len_utf8() {
            out.push(' ');
        }
    };
    while i < cs.len() {
        let c = cs[i].1;
        if c == '/' && i + 1 < cs.len() && cs[i + 1].1 == '/' {
            while i < cs.len() {
                sp(&mut out, cs[i].1);
                i += 1;
            }
        } else if c == '"' {
            sp(&mut out, c);
            i += 1;
            while i < cs.len() {
                let d = cs[i].1;
                sp(&mut out, d);
                i += 1;
                if d == '\\' && i < cs.len() {
                    sp(&mut out, cs[i].1);
                    i += 1;
                } else if d == '"' {
                    break;
                }
            }
        } else if c == '\'' {
            let close = if i + 2 < cs.len() && cs[i + 1].1 != '\\' && cs[i + 2].1 == '\'' {
                Some(i + 2)
            } else if i + 1 < cs.len() && cs[i + 1].1 == '\\' {
                (i + 2..(i + 8).min(cs.len())).find(|&k| cs[k].1 == '\'')
            } else {
                None
            };
            match close {
                Some(k) => {
                    while i <= k {
                        sp(&mut out, cs[i].1);
                        i += 1;
                    }
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn indent(s: &str) -> usize {
    s.len() - s.trim_start().len()
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// 語として `w` が現れる位置 (masked の中)。
fn word_positions(m: &str, w: &str) -> Vec<usize> {
    let b = m.as_bytes();
    let mut v = Vec::new();
    let mut from = 0;
    while let Some(p) = m[from..].find(w) {
        let s = from + p;
        let e = s + w.len();
        let pre_ok = s == 0 || (!is_word(b[s - 1]) && b[s - 1] != b'.');
        let post_ok = e >= b.len() || !is_word(b[e]);
        if pre_ok && post_ok {
            v.push(s);
        }
        from = s + w.len().max(1);
    }
    v
}

const KEYWORDS: &[&str] = &["if", "match", "while", "for", "in", "let", "return", "loop", "else", "fn", "with", "unsafe", "break", "mut", "move"];

/// 局所変数を根とする単純な被演算子 (`x` `&x` `&mut x` `*x` `x.f` ...) なら根の名前を返す。
fn operand_root(a: &str, locals: &HashMap<String, Option<String>>) -> Option<String> {
    let t = a.trim();
    let t = t.strip_prefix("&mut ").or_else(|| t.strip_prefix('&')).or_else(|| t.strip_prefix('*')).unwrap_or(t);
    if t.is_empty() {
        return None;
    }
    let mut root = None;
    for (k, seg) in t.split('.').enumerate() {
        let sb = seg.as_bytes();
        if sb.is_empty() || !(sb[0].is_ascii_lowercase() || sb[0] == b'_') || !sb.iter().all(|&c| is_word(c)) {
            return None;
        }
        if k == 0 {
            root = Some(seg.to_string());
        }
    }
    let r = root?;
    if KEYWORDS.contains(&r.as_str()) || !locals.contains_key(&r) {
        return None;
    }
    Some(r)
}

// ---------------------------------------------------------------- 構造の読み取り

#[derive(Clone, Copy, PartialEq, Debug)]
enum K {
    Fn,
    Block,
    Match,
    Lit,
    Paren,
}

struct Func {
    s: usize,
    e: usize,
    name: String,
    /// 引数 (名前, 型)
    params: Vec<(String, String)>,
    /// 局所変数 → 宣言された型 (分かるものだけ)
    locals: HashMap<String, Option<String>>,
}

fn split_top(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut d = 0i32;
    let mut cur = String::new();
    let cs: Vec<char> = s.chars().collect();
    for (i, &c) in cs.iter().enumerate() {
        match c {
            '(' | '[' | '{' | '<' => d += 1,
            ')' | ']' | '}' => d -= 1,
            '>' if !(i > 0 && cs[i - 1] == '-') => d -= 1,
            _ => {}
        }
        if c == sep && d == 0 {
            out.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn matching(m: &str, open: usize) -> Option<usize> {
    let b = m.as_bytes();
    let mut d = 0i32;
    for (i, &c) in b.iter().enumerate().skip(open) {
        match c {
            b'(' | b'[' | b'{' => d += 1,
            b')' | b']' | b'}' => {
                d -= 1;
                if d == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_funcs(lines: &[String]) -> Vec<Func> {
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if !l.starts_with("fn ") {
            continue;
        }
        let rest = &l[3..];
        let ne = rest.find(|c| c == '(' || c == '[' || c == ' ').unwrap_or(rest.len());
        let name = rest[..ne].to_string();
        let endl = format!("end {}", name);
        let Some(e) = (i + 1..lines.len()).find(|&j| lines[j] == endl) else { continue };
        let m = mask(l);
        let mut p = 3 + ne;
        if m.as_bytes().get(p) == Some(&b'[') {
            match matching(&m, p) {
                Some(q) => p = q + 1,
                None => continue,
            }
        }
        let mut params = Vec::new();
        if m.as_bytes().get(p) == Some(&b'(') {
            if let Some(q) = matching(&m, p) {
                for prm in split_top(&l[p + 1..q], ',') {
                    if let Some((n, t)) = prm.split_once(':') {
                        params.push((n.trim().to_string(), t.trim().to_string()));
                    }
                }
            }
        }
        let mut locals: HashMap<String, Option<String>> = HashMap::new();
        for (n, t) in &params {
            locals.insert(n.clone(), Some(t.clone()));
        }
        for bl in &lines[i + 1..e] {
            let t = bl.trim_start();
            if let Some(r) = t.strip_prefix("let ") {
                let r = r.strip_prefix("mut ").unwrap_or(r);
                let ne = r.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(r.len());
                let n = &r[..ne];
                if n.is_empty() || !(n.as_bytes()[0].is_ascii_lowercase() || n.as_bytes()[0] == b'_') {
                    continue;
                }
                let after = r[ne..].trim_start();
                let ty = after.strip_prefix(':').and_then(|a| a.split_once(" = ")).map(|(t, _)| t.trim().to_string());
                if locals.get(n).map_or(true, |o| o.is_none()) {
                    locals.insert(n.to_string(), ty);
                }
            }
        }
        out.push(Func { s: i, e, name, params, locals });
    }
    out
}

/// 各行が「文として安全」か (行頭の文脈が関数本体かブロックで、行が文を 1 つ完結させる)。
fn stmt_safe(lines: &[String], funcs: &[Func]) -> Vec<bool> {
    let mut safe = vec![false; lines.len()];
    for f in funcs {
        let mut st: Vec<K> = vec![K::Fn];
        for i in f.s + 1..f.e {
            let m = mask(&lines[i]);
            let t = m.trim();
            let start_len = st.len();
            let top = *st.last().unwrap_or(&K::Lit);
            let mut ok = (top == K::Fn || top == K::Block) && !t.is_empty() && !t.starts_with('}') && !t.starts_with("else") && !t.starts_with("while ");
            let bytes = m.as_bytes();
            let last_brace = m.trim_end().len().saturating_sub(1);
            for (k, &c) in bytes.iter().enumerate() {
                match c {
                    b'(' | b'[' => st.push(K::Paren),
                    b')' | b']' => {
                        if st.last() == Some(&K::Paren) {
                            st.pop();
                        }
                    }
                    b'{' => {
                        let kind = if k == last_brace {
                            if t.contains("match ") {
                                K::Match
                            } else if t == "{"
                                || t.starts_with("if ")
                                || t.starts_with("else")
                                || t.starts_with("} else")
                                || t.starts_with("loop")
                                || t.starts_with("for ")
                                || t.starts_with("unsafe ")
                                || t.starts_with("with ")
                                || t.contains("=> {")
                                || t.contains("fn(")
                                || t.ends_with("= {")
                                || t.contains(" if ")
                                || t.contains(" loop ")
                                || t.contains("(if ")
                            {
                                K::Block
                            } else {
                                K::Lit
                            }
                        } else {
                            K::Lit
                        };
                        st.push(kind);
                    }
                    b'}' => {
                        if matches!(st.last(), Some(K::Block) | Some(K::Match) | Some(K::Lit)) {
                            st.pop();
                        }
                    }
                    _ => {}
                }
            }
            let end_ok = st.len() == start_len || (st.len() == start_len + 1 && matches!(st.last(), Some(K::Block) | Some(K::Match)));
            safe[i] = ok && end_ok && !m.contains("=>") && !m.contains("fn(") && !m.contains("capture") && !t.starts_with("//");
            ok = false;
            let _ = ok;
        }
    }
    safe
}

// ---------------------------------------------------------------- 式の場所

#[derive(Clone)]
struct Site {
    line: usize,
    a: usize,
    b: usize,
    text: String,
    root: String,
    whole: bool,
    func: usize,
}

fn find_sites(lines: &[String], funcs: &[Func], safe: &[bool]) -> Vec<Site> {
    let mut out = Vec::new();
    for (fi, f) in funcs.iter().enumerate() {
        for i in f.s + 1..f.e {
            if !safe[i] {
                continue;
            }
            let l = &lines[i];
            let m = mask(l);
            let t = m.trim();
            let ind = indent(l);
            // 文全体が被演算子 (末尾式)
            if let Some(r) = operand_root(t, &f.locals) {
                out.push(Site { line: i, a: ind, b: ind + t.len(), text: t.to_string(), root: r, whole: true, func: fi });
                continue;
            }
            // `let x = 被演算子`
            if let Some(r) = t.strip_prefix("let ") {
                if let Some(eq) = r.find(" = ") {
                    let rhs = r[eq + 3..].trim_end();
                    if let Some(root) = operand_root(rhs, &f.locals) {
                        let a = l.len() - l.trim_end().len();
                        let b = l.trim_end().len();
                        let a0 = b - rhs.len();
                        let _ = a;
                        out.push(Site { line: i, a: a0, b, text: rhs.to_string(), root, whole: false, func: fi });
                    }
                }
            }
            // 呼び出しの引数
            let mb = m.as_bytes();
            for p in 0..mb.len() {
                if mb[p] != b'(' || p == 0 || !is_word(mb[p - 1]) {
                    continue;
                }
                let mut s = p;
                while s > 0 && (is_word(mb[s - 1]) || mb[s - 1] == b'.') {
                    s -= 1;
                }
                if KEYWORDS.contains(&&m[s..p]) {
                    continue;
                }
                let Some(q) = matching(&m, p) else { continue };
                let inner = &m[p + 1..q];
                let mut off = p + 1;
                let mut d = 0i32;
                let mut parts: Vec<(usize, usize)> = Vec::new();
                let mut st = off;
                for (k, c) in inner.bytes().enumerate() {
                    match c {
                        b'(' | b'[' | b'{' => d += 1,
                        b')' | b']' | b'}' => d -= 1,
                        b',' if d == 0 => {
                            parts.push((st, off + k));
                            st = off + k + 1;
                        }
                        _ => {}
                    }
                }
                parts.push((st, q));
                off = 0;
                let _ = off;
                for (x, y) in parts {
                    let raw = &l[x..y];
                    let lead = raw.len() - raw.trim_start().len();
                    let a = x + lead;
                    let b = a + raw.trim().len();
                    if a >= b {
                        continue;
                    }
                    if let Some(root) = operand_root(&l[a..b], &f.locals) {
                        out.push(Site { line: i, a, b, text: l[a..b].to_string(), root, whole: false, func: fi });
                    }
                }
            }
        }
    }
    out
}

fn pick<T: Clone>(v: &[T], max: usize) -> Vec<T> {
    if v.len() <= max {
        return v.to_vec();
    }
    (0..max).map(|k| v[k * v.len() / max].clone()).collect()
}

// ---------------------------------------------------------------- 変換

const ID_HELPER: &str = "\nfn t.rp_id[T](x: T) -> T uses [] panics never\n    x\nend t.rp_id\n";

// `fqn2meth` / `meth2fqn` (`T.m(&x)` と `x.m()` の言い換え) は、段 3 の D8 で経路形が E2029 になったので外した。
pub const TRANSFORMS: &[&str] = &["hoist", "block", "id", "closure", "extract", "fold", "iflet2match", "match2iflet"];

/// 番号を英字だけの名前にする (snake_case の検査が数字の前の下線を嫌う)。
fn ab(n: usize) -> String {
    let mut n = n;
    let mut s = String::new();
    loop {
        s.insert(0, (b'a' + (n % 26) as u8) as char);
        n /= 26;
        if n == 0 {
            return s;
        }
    }
}

fn join(lines: &[String]) -> String {
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

fn site_variant(lines: &[String], funcs: &[Func], s: &Site, n: usize, tf: &str) -> Option<String> {
    let l = &lines[s.line];
    let ind = " ".repeat(indent(l));
    let pre = &l[..s.a];
    let post = &l[s.b..];
    let mut out: Vec<String> = lines[..s.line].to_vec();
    let mut tail = ID_HELPER.to_string();
    match tf {
        "hoist" => {
            out.push(format!("{}let rp{} = {}", ind, ab(n), s.text));
            out.push(format!("{}rp{}{}", pre, ab(n), post));
            tail.clear();
        }
        "block" => {
            if s.whole {
                out.push(format!("{}{{", ind));
                out.push(format!("{}    {}", ind, s.text));
                out.push(format!("{}}}", ind));
            } else {
                out.push(format!("{}{{", pre));
                out.push(format!("{}    {}", ind, s.text));
                out.push(format!("{}}}{}", ind, post));
            }
            tail.clear();
        }
        "id" => {
            out.push(format!("{}t.rp_id({}){}", pre, s.text, post));
        }
        "closure" => {
            let ty = funcs[s.func].locals.get(&s.root).cloned().flatten()?;
            if s.text != s.root || ty.contains('\'') || ty.starts_with('&') {
                return None;
            }
            out.push(format!("{}let rpc{} = fn() -> {} capture [move {}] {{", ind, ab(n), ty, s.root));
            out.push(format!("{}    {}", ind, s.root));
            out.push(format!("{}}}", ind));
            out.push(format!("{}rpc{}(){}", pre, ab(n), post));
            tail.clear();
        }
        _ => return None,
    }
    out.extend(lines[s.line + 1..].iter().cloned());
    let mut r = join(&out);
    if !tail.is_empty() && !r.contains("fn t.rp_id") {
        r.push_str(&tail);
    }
    Some(r)
}

fn extract_variant(lines: &[String], f: &Func) -> Option<String> {
    let h = &lines[f.s];
    if f.e == f.s + 1 || h.contains("stack ") || !h.contains(" panics ") || f.name.contains('[') {
        return None;
    }
    let (prefix, last) = f.name.rsplit_once('.')?;
    let hname = format!("{}.rp_{}", prefix, last);
    let mut header = h.replacen(&format!("fn {}", f.name), &format!("fn {}", hname), 1);
    // `impls Tr` はトレイトの実装の印なので、抽出した補助メソッドには付けない (2 つ付くと E1112)。
    if let Some(i) = header.find(" impls ") {
        header.truncate(i);
    }
    let args: Vec<String> = f.params.iter().map(|(n, _)| n.clone()).collect();
    let mut out: Vec<String> = lines[..f.s].to_vec();
    out.push(h.clone());
    // メソッド (第 1 引数が `self`) は `x.m(..)` の形で呼ぶ (経路の形は段 3 の D8 で E2029)。
    if args.first().map_or(false, |a| a == "self") {
        out.push(format!("    self.rp_{}({})", last, args[1..].join(", ")));
    } else {
        out.push(format!("    {}({})", hname, args.join(", ")));
    }
    out.push(lines[f.e].clone());
    out.push(String::new());
    out.push(header);
    out.extend(lines[f.s + 1..f.e].iter().cloned());
    out.push(format!("end {}", hname));
    out.extend(lines[f.e + 1..].iter().cloned());
    Some(join(&out))
}

fn fold_variants(lines: &[String], funcs: &[Func], safe: &[bool]) -> Vec<String> {
    let mut out = Vec::new();
    for f in funcs {
        for i in f.s + 1..f.e.saturating_sub(1) {
            let t = lines[i].trim();
            let Some(r) = t.strip_prefix("let ") else { continue };
            let Some((name, rhs)) = r.split_once(" = ") else { continue };
            if name.contains(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) || name.is_empty() {
                continue;
            }
            let m = mask(&lines[i]);
            if !safe[i] || m.trim_end().ends_with('{') || mask(rhs).trim().is_empty() || m.contains("capture") {
                continue;
            }
            // 初期化子が一時オブジェクトを借用する形 (`&x` / `&mut x` など) や、メソッド
            // 呼び出しの結果が借用される形 (`x.lock().get()` など) は、インライン展開すると
            // 一時オブジェクトの生存期間が変わり意味が変わるため、fold を飛ばす。
            let rhs = rhs.trim();
            if rhs_borrows_temporary(rhs) {
                continue;
            }
            let j = i + 1;
            if !safe[j] {
                continue;
            }
            let total: usize = (f.s + 1..f.e).map(|k| word_positions(&mask(&lines[k]), name).len()).sum();
            let mj = mask(&lines[j]);
            let ps = word_positions(&mj, name);
            if total != 2 || ps.len() != 1 {
                continue;
            }
            let rep = if mask(rhs).trim().contains(' ') { format!("({})", rhs) } else { rhs.to_string() };
            // `name` の直前が `&`/`*` で rep が `&`/`*` で始まるなど、`&&` や `**` といった
            // 文法エラー (E0002) を生む並びは作らない。
            if fold_insertion_unsafe(&lines[j], ps[0], name, &rep) {
                continue;
            }
            let mut nl: Vec<String> = lines[..i].to_vec();
            nl.push(format!("{}{}{}", &lines[j][..ps[0]], rep, &lines[j][ps[0] + name.len()..]));
            nl.extend(lines[j + 1..].iter().cloned());
            out.push(join(&nl));
        }
    }
    out
}

/// 初期化子が一時オブジェクトを借用する形 (`&` / `&mut` で始まる) か、メソッド呼び出し
/// (`x.m(...)`) を含むか。真なら `fold` でのインライン展開を飛ばす。
fn rhs_borrows_temporary(rhs: &str) -> bool {
    let r = rhs.trim();
    if r.starts_with('&') {
        return true;
    }
    let bytes = r.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'.' => {
                let k = i + 1;
                if k < bytes.len() && (bytes[k].is_ascii_alphabetic() || bytes[k] == b'_') {
                    let mut m = k;
                    while m < bytes.len() && (bytes[m].is_ascii_alphanumeric() || bytes[m] == b'_') {
                        m += 1;
                    }
                    if m < bytes.len() && bytes[m] == b'(' && depth == 0 {
                        return true;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// `lines[j]` の `pos` (名前 `name` の位置) に `rep` を差し替えたとき、`&&` や `**` などの
/// 記号が並んで文法エラー (E0002) になる形なら真。
fn fold_insertion_unsafe(line: &str, pos: usize, name: &str, rep: &str) -> bool {
    let bytes = line.as_bytes();
    let pre = pos.saturating_sub(1);
    let post = pos + name.len();
    let pre_sigil = pre < bytes.len() && matches!(bytes[pre], b'&' | b'*');
    let post_sigil = post < bytes.len() && matches!(bytes[post], b'&' | b'*');
    let rep_starts_sigil = !rep.is_empty() && matches!(rep.as_bytes()[0], b'&' | b'*');
    let rep_ends_sigil = !rep.is_empty() && matches!(rep.as_bytes().last(), Some(b'&' | b'*'));
    (pre_sigil && rep_starts_sigil) || (rep_ends_sigil && post_sigil)
}

fn block_end(lines: &[String], i: usize) -> Option<usize> {
    let ind = indent(&lines[i]);
    (i + 1..lines.len()).find(|&j| !lines[j].trim().is_empty() && indent(&lines[j]) == ind && lines[j].trim_start().starts_with('}'))
}

fn reindent(ls: &[String], delta: i32) -> Vec<String> {
    ls.iter()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else if delta >= 0 {
                format!("{}{}", " ".repeat(delta as usize), l)
            } else {
                l[(-delta) as usize..].to_string()
            }
        })
        .collect()
}

/// 補になる列挙子の腕 (`Some(..)` → `None` など)。
fn complement(pat: &str) -> Option<&'static str> {
    if pat.starts_with("Some(") {
        Some("None")
    } else if pat == "None" {
        Some("Some(_)")
    } else if pat.starts_with("Ok(") {
        Some("Err(_)")
    } else if pat.starts_with("Err(") {
        Some("Ok(_)")
    } else {
        None
    }
}

fn iflet_variants(lines: &[String], safe: &[bool]) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..lines.len() {
        let t = lines[i].trim();
        if !safe[i] || !t.starts_with("if let ") || !t.ends_with(" {") {
            continue;
        }
        let body = &t[7..t.len() - 2];
        let Some((pat, scr)) = body.split_once(" = ") else { continue };
        let Some(comp) = complement(pat.trim()) else { continue };
        let Some(j) = block_end(lines, i) else { continue };
        let ind = indent(&lines[i]);
        let sp = " ".repeat(ind);
        let (else_body, end): (Vec<String>, usize) = if lines[j].trim() == "} else {" {
            let Some(k) = block_end(lines, j) else { continue };
            if lines[k].trim() != "}" {
                continue;
            }
            (reindent(&lines[j + 1..k], 4), k)
        } else if lines[j].trim() == "}" {
            (vec![format!("{}        ()", sp)], j)
        } else {
            continue;
        };
        let mut nl: Vec<String> = lines[..i].to_vec();
        nl.push(format!("{}match {} {{", sp, scr));
        nl.push(format!("{}    {} => {{", sp, pat));
        nl.extend(reindent(&lines[i + 1..j], 4));
        nl.push(format!("{}    }}", sp));
        nl.push(format!("{}    {} => {{", sp, comp));
        nl.extend(else_body);
        nl.push(format!("{}    }}", sp));
        nl.push(format!("{}}}", sp));
        nl.extend(lines[end + 1..].iter().cloned());
        out.push(join(&nl));
    }
    out
}

fn match_variants(lines: &[String], safe: &[bool]) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..lines.len() {
        let t = lines[i].trim();
        if !safe[i] || !t.starts_with("match ") || !t.ends_with(" {") {
            continue;
        }
        let scr = &t[6..t.len() - 2];
        let Some(j) = block_end(lines, i) else { continue };
        let ind = indent(&lines[i]);
        let sp = " ".repeat(ind);
        // 腕を読む
        let mut arms: Vec<(String, Vec<String>)> = Vec::new();
        let mut k = i + 1;
        let mut ok = true;
        while k < j {
            let l = &lines[k];
            if l.trim().is_empty() {
                k += 1;
                continue;
            }
            if indent(l) != ind + 4 {
                ok = false;
                break;
            }
            let Some((pat, rest)) = l.trim().split_once(" => ") else {
                ok = false;
                break;
            };
            if pat.contains(" if ") {
                ok = false;
                break;
            }
            if rest.trim() == "{" {
                let Some(e) = block_end(lines, k) else {
                    ok = false;
                    break;
                };
                arms.push((pat.to_string(), reindent(&lines[k + 1..e], -4)));
                k = e + 1;
            } else {
                arms.push((pat.to_string(), vec![format!("{}    {}", sp, rest.trim())]));
                k += 1;
            }
        }
        if !ok || arms.len() != 2 || complement(&arms[0].0) != Some(arms[1].0.as_str()) {
            continue;
        }
        let mut nl: Vec<String> = lines[..i].to_vec();
        nl.push(format!("{}if let {} = {} {{", sp, arms[0].0, scr));
        nl.extend(arms[0].1.clone());
        nl.push(format!("{}}} else {{", sp));
        nl.extend(arms[1].1.clone());
        nl.push(format!("{}}}", sp));
        nl.extend(lines[j + 1..].iter().cloned());
        out.push(join(&nl));
    }
    out
}

/// ファイル内で宣言されたメソッド (`fn t.T.m(self: ..`): m → (FQN, self の型)。名前が重なれば除く。
fn declared_methods(lines: &[String]) -> HashMap<String, Option<(String, String)>> {
    let mut mp: HashMap<String, Option<(String, String)>> = HashMap::new();
    for l in lines {
        if let Some(r) = l.strip_prefix("fn ") {
            let Some(p) = r.find('(') else { continue };
            let fqn = &r[..p];
            if fqn.contains('[') || !r[p..].starts_with("(self: ") {
                continue;
            }
            let (_, m) = fqn.rsplit_once('.').unwrap();
            let ty = r[p + 7..].split(|c| c == ',' || c == ')').next().unwrap_or("").trim().to_string();
            let e = mp.entry(m.to_string()).or_insert(Some((fqn.to_string(), ty.clone())));
            if e.as_ref().map(|x| x.0.as_str()) != Some(fqn) {
                *e = None;
            }
        }
    }
    mp
}

fn call_variants(lines: &[String], funcs: &[Func], tf: &str) -> Vec<String> {
    let mut out = Vec::new();
    let meths = declared_methods(lines);
    let declared_fqns: BTreeSet<String> = meths.values().flatten().map(|x| x.0.clone()).collect();
    let all_fns: BTreeSet<String> = lines.iter().filter_map(|l| l.strip_prefix("fn ")).filter_map(|r| r.find('(').map(|p| r[..p].to_string())).collect();
    for f in funcs {
        for i in f.s + 1..f.e {
            let l = &lines[i];
            let m = mask(l);
            let mb = m.as_bytes();
            for p in 0..mb.len() {
                if mb[p] != b'(' || p == 0 || !is_word(mb[p - 1]) {
                    continue;
                }
                let mut s = p;
                while s > 0 && (is_word(mb[s - 1]) || mb[s - 1] == b'.') {
                    s -= 1;
                }
                let path = &m[s..p];
                let Some(q) = matching(&m, p) else { continue };
                let segs: Vec<&str> = path.split('.').collect();
                if tf == "fqn2meth" {
                    if segs.len() < 3 || !segs[segs.len() - 2].as_bytes()[0].is_ascii_uppercase() || !segs[segs.len() - 1].as_bytes()[0].is_ascii_lowercase() {
                        continue;
                    }
                    if all_fns.contains(path) && !declared_fqns.contains(path) {
                        continue; // ファイル内で宣言された、self を取らない関数
                    }
                    let parts = split_top(&l[p + 1..q], ',');
                    let Some(first) = parts.first() else { continue };
                    let Some(root) = operand_root(first, &f.locals) else { continue };
                    if first.trim().starts_with('*') {
                        continue;
                    }
                    let recv = first.trim().trim_start_matches("&mut ").trim_start_matches('&').to_string();
                    let _ = root;
                    let rest = parts[1..].join(", ");
                    let meth = segs[segs.len() - 1];
                    let mut nl: Vec<String> = lines[..i].to_vec();
                    nl.push(format!("{}{}.{}({}){}", &l[..s], recv, meth, rest, &l[q + 1..]));
                    nl.extend(lines[i + 1..].iter().cloned());
                    out.push(join(&nl));
                } else {
                    // meth2fqn: `x.m(` の `x`
                    if segs.len() != 2 || s == 0 {
                        continue;
                    }
                    let (recv, mname) = (segs[0], segs[1]);
                    if mb[s.saturating_sub(1)] == b'.' || recv.is_empty() {
                        continue;
                    }
                    let Some(Some((fqn, sty))) = meths.get(mname) else { continue };
                    if !f.locals.contains_key(recv) {
                        continue;
                    }
                    let lty = f.locals.get(recv).cloned().flatten().unwrap_or_default();
                    let arg0 = if lty.starts_with('&') {
                        recv.to_string()
                    } else if sty.starts_with("&mut ") {
                        format!("&mut {}", recv)
                    } else if sty.starts_with('&') {
                        format!("&{}", recv)
                    } else {
                        recv.to_string()
                    };
                    let inner = l[p + 1..q].trim();
                    let args = if inner.is_empty() { arg0 } else { format!("{}, {}", arg0, inner) };
                    let mut nl: Vec<String> = lines[..i].to_vec();
                    nl.push(format!("{}{}({}){}", &l[..s], fqn, args, &l[q + 1..]));
                    nl.extend(lines[i + 1..].iter().cloned());
                    out.push(join(&nl));
                }
            }
        }
    }
    out
}

/// 変換 `tf` を `src` に当てた言い換えの一覧 (できなければ空)。
pub fn variants(src: &str, tf: &str, max: usize) -> Vec<String> {
    let lines: Vec<String> = src.lines().map(|s| s.to_string()).collect();
    let funcs = parse_funcs(&lines);
    let safe = stmt_safe(&lines, &funcs);
    let v: Vec<String> = match tf {
        "hoist" | "block" | "id" | "closure" => {
            let sites = find_sites(&lines, &funcs, &safe);
            sites.iter().enumerate().filter_map(|(n, s)| site_variant(&lines, &funcs, s, n, tf)).collect()
        }
        "extract" => funcs.iter().filter_map(|f| extract_variant(&lines, f)).collect(),
        "fold" => fold_variants(&lines, &funcs, &safe),
        "iflet2match" => iflet_variants(&lines, &safe),
        "match2iflet" => match_variants(&lines, &safe),
        "fqn2meth" | "meth2fqn" => call_variants(&lines, &funcs, tf),
        _ => Vec::new(),
    };
    // 同じ言い換えと元と同じものは除く。番号は除く前の並びを保つため、ここでは単純に選ぶ。
    let v: Vec<String> = v.into_iter().filter(|x| x.trim_end() != src.trim_end()).collect();
    pick(&v, max)
}

// ---------------------------------------------------------------- 実行

#[derive(Clone, Debug, PartialEq)]
enum Out {
    Same,
    Hole,
    Changed(Vec<String>, i32),
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn check_src(src: &str, target: Option<&str>) -> (i32, Vec<String>) {
    check_src_mode(src, target, None)
}

/// `mode` は `KSN_CHECKER` の上書き (s4 台帳用)。`None` なら環境をそのまま引き継ぐ。
fn check_src_mode(src: &str, target: Option<&str>, mode: Option<&str>) -> (i32, Vec<String>) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("ksn_rephrase_{}_{}", std::process::id(), n));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("src")).unwrap();
    fs::write(d.join("kasane.toml"), TOML).unwrap();
    fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
    fs::write(d.join("src").join("t.ksnl"), src).unwrap();
    // 書き方は 1 通り (E0020): 試験の入力は `ksn fmt` の出力にそろえる。
    let _ = std::process::Command::new(env!("CARGO_BIN_EXE_ksn")).arg("fmt").current_dir(&d).output();
    let mut args = vec!["check"];
    if let Some(tg) = target {
        args.extend(["--target", tg]);
    }
    args.push("--json");
    let o = match mode {
        Some(m) => run_with_env(&args, &d, &[("KSN_CHECKER", m)]),
        None => run(&args, &d),
    };
    let _ = fs::remove_dir_all(&d);
    (o.status.code().unwrap_or(-1), codes(&text(&o.stdout)))
}

struct Report {
    /// (状態, 変換, 名前#番号)
    entries: BTreeSet<String>,
    detail: BTreeMap<String, String>,
    applied: BTreeMap<&'static str, usize>,
    skipped: BTreeMap<&'static str, usize>,
    total: usize,
}

fn max_sites() -> usize {
    std::env::var("REPHRASE_MAX_SITES").ok().and_then(|v| v.parse().ok()).unwrap_or(6)
}

fn run_dir(dir: &Path, unsound: bool) -> Report {
    run_dir_mode(dir, unsound, None)
}

/// `mode` は `KSN_CHECKER` の上書き (s4 台帳用)。
fn run_dir_mode(dir: &Path, unsound: bool, mode: Option<&str>) -> Report {
    let mut names: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().map_or(false, |x| x == "ksnl")).collect();
    names.sort();
    let rep = Mutex::new(Report { entries: BTreeSet::new(), detail: BTreeMap::new(), applied: BTreeMap::new(), skipped: BTreeMap::new(), total: 0 });
    let next = AtomicUsize::new(0);
    let max = max_sites();
    std::thread::scope(|sc| {
        for _ in 0..16 {
            sc.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::SeqCst);
                if k >= names.len() {
                    break;
                }
                let p = &names[k];
                let stem = p.file_stem().unwrap().to_string_lossy().to_string();
                let src = fold_struct_literals(&fs::read_to_string(p).unwrap());
                let want = expect_value(p, "expect");
                let target = expect_value(p, "target");
                for tf in TRANSFORMS {
                    let vs = variants(&src, tf, max);
                    let mut r = rep.lock().unwrap();
                    if vs.is_empty() {
                        *r.skipped.entry(tf).or_insert(0) += 1;
                        continue;
                    }
                    *r.applied.entry(tf).or_insert(0) += vs.len();
                    drop(r);
                    for (n, v) in vs.iter().enumerate() {
                        let (rc, got) = check_src_mode(v, target.as_deref(), mode);
                        let key = format!("{} {}#{}", tf, stem, n);
                        let mut r = rep.lock().unwrap();
                        r.total += 1;
                        if unsound {
                            let rejected = rc == 1;
                            if !rejected {
                                r.entries.insert(format!("hole {}", key));
                                r.detail.insert(key, format!("rc={}\n{}", rc, v));
                            } else if want.as_ref().map_or(true, |w| !got.contains(w)) {
                                r.entries.insert(format!("changed {}", key));
                                r.detail.insert(key, format!("want={:?} got={:?}\n{}", want, got, v));
                            }
                        } else if rc != 0 {
                            r.entries.insert(format!("fp {}", key));
                            r.detail.insert(key, format!("rc={} codes={:?}\n{}", rc, got, v));
                        }
                    }
                }
            });
        }
    });
    rep.into_inner().unwrap()
}

/// 構造体リテラル・パターンは正規形が複数行 (1 行 1 欄) だが、この生成器は「1 行 = 1 文」を前提にした行ごとの変換なので、
/// 入力は旧い 1 行の形 (`T { a: x, b: y }`) に畳んでから変換する。変換後の入力は `check_src_mode` が `ksn fmt` で正規形に直す。
fn fold_struct_literals(text: &str) -> String {
    fn ind(l: &str) -> usize {
        l.len() - l.trim_start_matches(' ').len()
    }
    fn opens_literal(t: &str) -> bool {
        let Some(head) = t.strip_suffix(" {") else { return false };
        let first = head.split_whitespace().next().unwrap_or("");
        if matches!(first, "if" | "while" | "for" | "loop" | "match" | "else" | "fn" | "impl" | "trait" | "enum" | "type" | "unsafe" | "with" | "extern" | "test" | "}") {
            return false;
        }
        let last = head.rsplit(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("");
        last.chars().next().map_or(false, |c| c.is_ascii_uppercase())
    }
    fn fold(lines: &[String]) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        let mut i = 0;
        while i < lines.len() {
            let l = &lines[i];
            let t = l.trim();
            if opens_literal(t) {
                let n = ind(l);
                if let Some(off) = lines[i + 1..].iter().position(|x| ind(x) == n && x.trim_start().starts_with('}')) {
                    let j = i + 1 + off;
                    let inner = fold(&lines[i + 1..j]);
                    let fields: Vec<String> = inner.iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
                    if fields.iter().all(|f| f.contains(':')) && !fields.is_empty() {
                        out.push(format!("{}{} {} {}", " ".repeat(n), t, fields.join(", "), lines[j].trim()));
                        i = j + 1;
                        continue;
                    }
                }
            }
            out.push(l.clone());
            i += 1;
        }
        out
    }
    let lines: Vec<String> = text.split('\n').map(String::from).collect();
    fold(&lines).join("\n")
}

fn ledger(path: &Path) -> BTreeSet<String> {
    fs::read_to_string(path).map(|s| s.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).map(|l| l.trim().to_string()).collect()).unwrap_or_default()
}

fn compare(name: &str, rep: &Report, ledger_path: &Path, header: &str) {
    eprintln!("[{}] 言い換え {} 件 (変換ごとの当てた数 {:?}、当てられず飛ばしたケース数 {:?})、台帳の対象 {} 件", name, rep.total, rep.applied, rep.skipped, rep.entries.len());
    if std::env::var("REPHRASE_UPDATE").is_ok() {
        let mut s = header.to_string();
        for e in &rep.entries {
            s.push_str(e);
            s.push('\n');
        }
        fs::write(ledger_path, s).unwrap();
        if let Ok(dir) = std::env::var("REPHRASE_DUMP") {
            let _ = fs::create_dir_all(&dir);
            for (k, v) in &rep.detail {
                let f = format!("{}/{}.txt", dir, k.replace([' ', '#'], "_"));
                let _ = fs::write(f, v);
            }
        }
        return;
    }
    let known = ledger(ledger_path);
    let new: Vec<_> = rep.entries.difference(&known).collect();
    let fixed: Vec<_> = known.difference(&rep.entries).collect();
    let mut msg = String::new();
    for e in new.iter().take(40) {
        let key = e.splitn(2, ' ').nth(1).unwrap();
        msg.push_str(&format!("NEW {}\n{}\n", e, rep.detail.get(key).map(|d| d.lines().take(30).collect::<Vec<_>>().join("\n")).unwrap_or_default()));
    }
    for e in fixed.iter().take(40) {
        msg.push_str(&format!("FIXED (台帳から外す) {}\n", e));
    }
    assert!(new.is_empty() && fixed.is_empty(), "台帳と食い違う: 新規 {} 件、塞いだ {} 件\n{}", new.len(), fixed.len(), msg);
}

fn data(rel: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(rel)
}

#[test]
fn rephrase_unsound_stay_rejected() {
    let rep = run_dir(&data("unsound"), true);
    compare("unsound", &rep, &data("rephrase_known.txt"), "# 言い換えで通ってしまう (hole) / 診断コードが変わる (changed) 台帳。REPHRASE_UPDATE=1 で再生成。\n");
}

#[test]
fn rephrase_valid_stay_accepted() {
    let rep = run_dir(&data("linear_ok"), false);
    compare("linear_ok", &rep, &data("rephrase_known_fp.txt"), "# 正しいコードの言い換えが拒まれる誤検出 (fp) の台帳。REPHRASE_UPDATE=1 で再生成。\n");
}

/// 生成器の自己検査: 正しい小さなプログラムの言い換えが、受理されること。
#[test]
fn rephrase_generator_sanity() {
    let src = "type t.S\n    n: u32\nend t.S\n\nfn t.S.get(self: &t.S, k: u32) -> u32 uses [] panics never\n    self.n.wrapping_add(k)\nend t.S.get\n\nfn t.use_it(s: &t.S, o: Option<u32>) -> u32 uses [] panics never\n    let a = s.get(1u32)\n    let b = s.get(a)\n    if let Some(v) = o {\n        v.wrapping_add(b)\n    } else {\n        b\n    }\nend t.use_it\n\nfn t.main() -> () uses [] panics never\n    let s = t.S { n: 1u32 }\n    core.mem.drop(t.use_it(&s, Some(2u32)))\nend t.main\n";
    assert_eq!(check_src(src, None).0, 0);
    let mut applied = BTreeMap::new();
    for tf in TRANSFORMS {
        let vs = variants(src, tf, 50);
        applied.insert(*tf, vs.len());
        for v in vs {
            let (rc, got) = check_src(&v, None);
            assert_eq!(rc, 0, "{} が壊した: {:?}\n{}", tf, got, v);
        }
    }
    for tf in ["hoist", "block", "id", "extract", "fold", "iflet2match"] {
        assert!(applied[tf] > 0, "{} が当たらない: {:?}", tf, applied);
    }
}
