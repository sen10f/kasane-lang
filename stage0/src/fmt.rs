//! `ksn fmt` (stage0): 機械的に直せる正規形違反を直す。Python 参照実装 ksn/fmt.py の移植。

use regex::Regex;

use crate::lexer::{self, Tok};
use crate::program::load_file;
use std::collections::HashSet;

const DERIVE_ORDER: &[&str] = &["Debug", "Clone", "Copy", "TryClone", "Drop", "PartialEq", "Eq", "PartialOrd", "Ord", "Hash", "From", "ToJson", "FromJson"];

pub fn mechanical(text: &str) -> Vec<String> {
    let t = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out: Vec<String> = vec![];
    for l in t.split('\n') {
        let mut l = l.trim_end_matches(|c| c == ' ' || c == '\t').to_string();
        let tabs = l.chars().take_while(|c| *c == '\t').count();
        if tabs > 0 {
            l = format!("{}{}", "    ".repeat(tabs), &l[tabs..]);
        }
        out.push(l);
    }
    while out.last().map_or(false, |l| l.is_empty()) {
        out.pop();
    }
    out
}

/// cli 側の簡易整形 (構文エラーのファイル用): 連続する空行も1行にまとめる。
pub fn mechanical_collapse(text: &str) -> String {
    let lines = mechanical(text);
    let mut res: Vec<String> = vec![];
    for l in lines {
        if l.is_empty() && res.last().map_or(false, |x| x.is_empty()) {
            continue;
        }
        res.push(l);
    }
    res.join("\n") + "\n"
}

/// 名前の参照形 (3.3): ヘッダでは FQN、本体では import 済みモジュールの短縮形が正規形。
#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    Header,
    Body,
}

struct Imp {
    tail: String,
    full: Vec<String>,
}

#[derive(Default)]
struct Refs {
    /// 本体 (正規形では短縮形) から参照された import の末尾名。
    body: HashSet<String>,
    /// ヘッダから参照された import の末尾名 (短縮形・FQN どちらでも)。
    header: HashSet<String>,
}

fn name_tok(t: &Tok) -> bool {
    matches!(t.kind, "ident" | "upper" | "prim") || (t.kind == "kw" && matches!(t.text.as_str(), "self" | "Self"))
}

/// `a.b.c` の形の名前 (トークン列の区間と各成分)。直前が `.` のもの (メソッド連鎖) は含めない。
fn chains(toks: &[Tok]) -> Vec<(usize, usize, Vec<String>)> {
    let mut out = vec![];
    let mut i = 0;
    while i < toks.len() {
        if !name_tok(&toks[i]) {
            i += 1;
            continue;
        }
        let after_dot = i > 0 && toks[i - 1].kind == "op" && toks[i - 1].text == ".";
        let mut j = i;
        let mut segs = vec![toks[i].text.clone()];
        while j + 2 < toks.len()
            && toks[j + 1].kind == "op"
            && toks[j + 1].text == "."
            && toks[j + 1].col == toks[j].col + toks[j].text.chars().count()
            && name_tok(&toks[j + 2])
            && toks[j + 2].col == toks[j + 1].col + 1
        {
            segs.push(toks[j + 2].text.clone());
            j += 2;
        }
        if !after_dot {
            out.push((i, j + 1, segs));
        }
        i = j + 1;
    }
    out
}

/// `uses [..]` の角括弧の中 (効果名。モジュール名ではない) のトークン。
fn effect_tokens(toks: &[Tok]) -> HashSet<usize> {
    let mut out = HashSet::new();
    let mut i = 0;
    while i + 1 < toks.len() {
        if toks[i].kind == "kw" && toks[i].text == "uses" && toks[i + 1].text == "[" {
            let mut depth = 0;
            let mut j = i + 1;
            while j < toks.len() {
                match toks[j].text.as_str() {
                    "[" => depth += 1,
                    "]" => depth -= 1,
                    _ => {}
                }
                out.insert(j);
                if depth == 0 {
                    break;
                }
                j += 1;
            }
            i = j;
        }
        i += 1;
    }
    out
}

fn lex_line(line: &str) -> Option<Vec<Tok>> {
    if line.trim_start().starts_with("//") {
        return None;
    }
    lexer::lex(&format!("{}\n", line)).ok()
}

/// 1行の名前を正規形にする。`mode_at` はトークン位置ごとの文脈 (定数の `=` の前後で変わる)。
fn rewrite_line(line: &str, header: bool, imps: &[Imp], keep: &HashSet<String>, refs: &mut Refs) -> String {
    let Some(toks) = lex_line(line) else { return line.to_string() };
    let eff = effect_tokens(&toks);
    let eq = toks.iter().position(|t| t.kind == "op" && t.text == "=");
    let const_like = header && matches!(toks.first(), Some(t) if t.kind == "kw" && matches!(t.text.as_str(), "const" | "static"));
    // 定義名 (行頭キーワードの直後) は参照ではない。
    let skip_name = header
        && matches!(toks.first(), Some(t) if t.kind == "kw"
            && matches!(t.text.as_str(), "fn" | "type" | "enum" | "trait" | "const" | "static" | "extern" | "test"));
    let mut reps: Vec<(usize, usize, String)> = vec![];
    for (a, _, segs) in chains(&toks) {
        if eff.contains(&a) || (skip_name && a == 1) || segs.len() < 2 {
            continue;
        }
        let ctx = if const_like && eq.map_or(false, |e| a > e) {
            Ctx::Body
        } else if header {
            Ctx::Header
        } else {
            Ctx::Body
        };
        let by_tail = imps.iter().find(|m| m.tail == segs[0]);
        let col0 = toks[a].col - 1;
        let len0 = segs[0].chars().count();
        match ctx {
            Ctx::Header => {
                if let Some(m) = by_tail {
                    if !keep.contains(&segs[0]) {
                        refs.header.insert(m.tail.clone());
                        reps.push((col0, col0 + len0, m.full.join(".")));
                    }
                } else if let Some(m) = imps.iter().find(|m| segs.len() > m.full.len() && segs[..m.full.len()] == m.full[..]) {
                    refs.header.insert(m.tail.clone());
                }
            }
            Ctx::Body => {
                if let Some(m) = by_tail {
                    refs.body.insert(m.tail.clone());
                } else if let Some(m) = imps
                    .iter()
                    .filter(|m| segs.len() > m.full.len() && segs[..m.full.len()] == m.full[..])
                    .max_by_key(|m| m.full.len())
                {
                    refs.body.insert(m.tail.clone());
                    if !keep.contains(&segs[0]) {
                        let end_tok = a + 2 * (m.full.len() - 1);
                        let end = toks[end_tok].col - 1 + toks[end_tok].text.chars().count();
                        reps.push((col0, end, m.tail.clone()));
                    }
                }
            }
        }
    }
    if reps.is_empty() {
        return line.to_string();
    }
    let mut chars: Vec<char> = line.chars().collect();
    reps.sort_by(|x, y| y.0.cmp(&x.0));
    for (s, e, r) in reps {
        chars.splice(s..e, r.chars());
    }
    chars.into_iter().collect()
}

/// 本体で局所束縛と衝突しうる先頭成分 (単独で現れる識別子)。これらで始まる FQN は短縮しない。
fn isolated_idents(lines: &[String]) -> HashSet<String> {
    let mut out = HashSet::new();
    for l in lines {
        let Some(toks) = lex_line(l) else { continue };
        for (i, t) in toks.iter().enumerate() {
            if t.kind != "ident" {
                continue;
            }
            let dot_before = i > 0 && toks[i - 1].kind == "op" && toks[i - 1].text == ".";
            let dot_after = toks.get(i + 1).map_or(false, |n| n.kind == "op" && n.text == "." && n.col == t.col + t.text.chars().count());
            if !dot_before && !dot_after {
                out.insert(t.text.clone());
            }
        }
    }
    out
}

fn fix_derive(header: &str) -> String {
    let re = Regex::new(r"derive \[([^\]]*)\]").unwrap();
    let Some(m) = re.captures(header) else { return header.to_string() };
    let mut names: Vec<String> = m[1].split(',').map(|x| x.trim().to_string()).collect();
    names.sort_by_key(|n| DERIVE_ORDER.iter().position(|x| x == n).unwrap_or(99));
    let whole = m.get(0).unwrap();
    format!("{}derive [{}]{}", &header[..whole.start()], names.join(", "), &header[whole.end()..])
}

fn group(kw: &str) -> usize {
    match kw {
        "import" => 0,
        "const" | "static" => 1,
        "type" | "enum" | "trait" => 2,
        "extern" => 3,
        "impl" => 4,
        "fn" => 5,
        _ => 6,
    }
}

struct It {
    kw: String,
    name: String,
    lines: Vec<String>,
    impl_idx: Option<usize>,
    has_impls: bool,
}

pub fn format_source(text: &str, path: &str) -> String {
    format_source_with(text, path, true)
}

/// `canon` が偽なら、名前の参照形 (3.3) は書き換えず、定義の並びと空行だけを整える (`ksn rename` 用)。
pub fn format_source_with(text: &str, path: &str, canon: bool) -> String {
    let lines = mechanical(text);
    let joined = lines.join("\n") + "\n";
    let defs = match load_file(path, "", &joined) {
        Ok(d) => d,
        Err(_) => return joined,
    };
    // 3.3: 名前の参照形を正規形にする (ヘッダは FQN、本体は import 済みモジュールの短縮形)。
    let mut lines = lines;
    let imps: Vec<Imp> = defs
        .iter()
        .filter(|d| d.kw == "import")
        .map(|d| Imp { tail: d.name.rsplit('.').next().unwrap().to_string(), full: d.name.split('.').map(String::from).collect() })
        .collect();
    let mut dropped: HashSet<usize> = HashSet::new();
    if canon && !imps.is_empty() {
        let mut refs = Refs::default();
        for d in &defs {
            if matches!(d.kw.as_str(), "import" | "end") || d.line == 0 {
                continue;
            }
            let hkeep = d.gnames();
            let end = if d.end_line > 0 { d.end_line - 1 } else { d.line };
            let body_mode = matches!(d.kw.as_str(), "fn" | "test");
            let span: Vec<String> = lines[d.line - 1..end].to_vec();
            let iso = isolated_idents(&span);
            for k in d.line - 1..end {
                let header = k == d.line - 1 || !body_mode;
                let keep: HashSet<String> = if !header {
                    iso.clone()
                } else if matches!(d.kw.as_str(), "const" | "static") {
                    hkeep.union(&iso).cloned().collect()
                } else {
                    hkeep.clone()
                };
                lines[k] = rewrite_line(&lines[k], header, &imps, &keep, &mut refs);
            }
        }
        for d in &defs {
            if d.kw == "import" {
                let tail = d.name.rsplit('.').next().unwrap();
                if refs.header.contains(tail) && !refs.body.contains(tail) {
                    dropped.insert(d.line);
                }
            }
        }
    }
    let mut items: Vec<It> = vec![];
    for d in &defs {
        if dropped.contains(&d.line) {
            continue;
        }
        let mut s = d.line - 1;
        while s > 0 && lines[s - 1].starts_with("//") {
            s -= 1;
        }
        let e = if d.end_line > 0 { d.end_line } else { d.line };
        let mut body: Vec<String> = lines[s..e].to_vec();
        for (i, l) in body.iter_mut().enumerate() {
            if (l.starts_with("type ") || l.starts_with("enum ")) && (d.kw == "type" || d.kw == "enum") && i == d.line - 1 - s {
                *l = fix_derive(l);
            }
        }
        items.push(It { kw: d.kw.clone(), name: d.name.clone(), lines: body, impl_idx: None, has_impls: d.has_impls });
    }
    // impl 行と直後の fn(impls) を結びつける
    let mut pending: Option<usize> = None;
    let mut children: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for i in 0..items.len() {
        if items[i].kw == "impl" {
            pending = Some(i);
            children.insert(i, vec![]);
        } else if items[i].kw == "fn" && items[i].has_impls && pending.is_some() {
            let p = pending.unwrap();
            children.get_mut(&p).unwrap().push(i);
            items[i].impl_idx = Some(p);
        } else {
            pending = None;
        }
    }
    let mut groups: Vec<Vec<usize>> = vec![vec![]; 7];
    for i in 0..items.len() {
        let kw = items[i].kw.as_str();
        if kw == "impl" {
            if children[&i].is_empty() {
                groups[4].push(i);
            }
        } else {
            groups[group(kw)].push(i);
        }
    }
    for g in groups.iter_mut() {
        g.sort_by(|a, b| items[*a].name.as_bytes().cmp(items[*b].name.as_bytes()));
    }
    let mut out: Vec<Vec<String>> = vec![];
    let imp: Vec<String> = groups[0].iter().map(|&i| items[i].lines[0].clone()).collect();
    if !imp.is_empty() {
        out.push(imp);
    }
    for g in [1, 2, 3, 4] {
        for &i in &groups[g] {
            out.push(items[i].lines.clone());
        }
    }
    let mut emitted: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for &i in &groups[5] {
        let mut chunk: Vec<String> = vec![];
        if let Some(p) = items[i].impl_idx {
            if !emitted.contains(&p) {
                let first = *children[&p].iter().min_by(|a, b| items[**a].name.as_bytes().cmp(items[**b].name.as_bytes())).unwrap();
                if first == i {
                    chunk.extend(items[p].lines.clone());
                    emitted.insert(p);
                }
            }
        }
        chunk.extend(items[i].lines.clone());
        out.push(chunk);
    }
    for &i in &groups[6] {
        out.push(items[i].lines.clone());
    }
    let mut res: Vec<String> = vec![];
    for (k, chunk) in out.into_iter().enumerate() {
        if k > 0 {
            res.push(String::new());
        }
        res.extend(chunk);
    }
    if res.is_empty() { String::new() } else { res.join("\n") + "\n" }
}
