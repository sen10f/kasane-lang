//! `ksn fmt` (stage0): 機械的に直せる正規形違反を直す。Python 参照実装 ksn/fmt.py の移植。

use regex::Regex;

use crate::lexer::{self, Tok};
use crate::program::load_file;
use std::collections::HashSet;

const DERIVE_ORDER: &[&str] = &["Debug", "Clone", "Copy", "TryClone", "Drop", "PartialEq", "Eq", "PartialOrd", "Ord", "Hash", "From", "ToJson", "FromJson"];

pub fn mechanical(text: &str) -> Vec<String> {
    // 単独の CR は触らない (文字列の中の CR を改行に変えて意味を変えない。CR は check が誤りにする)。
    let t = text.replace("\r\n", "\n");
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

/// 行末にだけ付く印 (`;` と match の腕の末尾の `,`) を消してから整形する (`ksn fmt` の先頭。
/// 設計_書き方は1通り.md 12 章)。`check` は従来どおり E0040 / E0049 (書き方の揺れなので E0020) で拒む。
/// 見つけ方も消し方も `ksn fix` の同じコードの修正と同じ (rustisms::scan と fixes::fixes_for)。
/// `;` は文字列の中・`[x; n]` のものは行末ではない、`,` は `=>` の無い行 (呼び出しの引数・
/// 構造体リテラル・タプル・配列・複数行の引数リスト) は腕の末尾ではない (rustisms が見ない) ので触らない。
pub fn strip_line_marks(text: &str, codes: &[&str]) -> String {
    let mut lines: Vec<String> = text.split('\n').map(String::from).collect();
    for code in codes {
        let hits: Vec<_> = crate::rustisms::scan(&lines.join("\n")).into_iter().filter(|h| h.code == *code).collect();
        if hits.is_empty() {
            continue;
        }
        for h in hits {
            for f in crate::fixes::fixes_for(h.code, &h.msg, "", h.line, h.col, &lines.clone()) {
                let chars: Vec<char> = lines[f.line - 1].chars().collect();
                let (a, b) = (f.col - 1, (f.end_col - 1).min(chars.len()));
                lines[f.line - 1] = chars[..a].iter().chain(chars[b..].iter()).collect::<String>().trim_end().to_string();
            }
        }
    }
    lines.join("\n")
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

/// `uses [..]`・`capture [..]`・`requires ..`・境界 `A + B` の並びを正規形にする (lint.rs の `order` と同じ規則)。
fn fix_lists(line: &str) -> String {
    // 文字列を含む行は触らない (リテラルの中身を書き換えない)。
    if line.contains('"') || line.trim_start().starts_with("//") {
        return line.to_string();
    }
    let mut l = line.to_string();
    // ヘッダの `depth M stack N` は `stack N depth M` の並びにする (正規形は stack が先)。
    if l.trim_start().starts_with("fn ") {
        if let Some(p) = l.find(" depth ") {
            let toks: Vec<&str> = l[p + 1..].split(' ').collect();
            if toks.len() >= 4 && toks[2] == "stack" && toks[1].chars().all(|c| c.is_ascii_digit()) && !toks[3].is_empty() && toks[3].chars().all(|c| c.is_ascii_digit()) {
                let rest = if toks.len() > 4 { format!(" {}", toks[4..].join(" ")) } else { String::new() };
                l = format!("{} stack {} depth {}{}", &l[..p], toks[3], toks[1], rest);
            }
        }
    }
    // uses: 具体の効果 (小文字始まり) をバイト順、効果変数 (大文字始まり) は元の順で後ろ。
    let mut from = 0;
    while let Some(p) = l[from..].find("uses [") {
        let open = from + p + 5;
        let inner = crate::lint::bracket(&l, open);
        let els = crate::lint::split_top(&inner, ',');
        let mut conc: Vec<String> = els.iter().filter(|e| e.chars().next().map_or(false, |c| c.is_lowercase())).cloned().collect();
        let var: Vec<String> = els.iter().filter(|e| e.chars().next().map_or(false, |c| c.is_uppercase())).cloned().collect();
        conc.sort();
        conc.extend(var);
        if conc.len() == els.len() && conc != els {
            l = format!("{}{}{}", &l[..open + 1], conc.join(", "), &l[open + 1 + inner.len()..]);
        }
        from = open + 1;
    }
    // capture: 名前 (`&` `&mut ` `move ` を除いた) の順。
    if let Some(p) = l.find("capture [") {
        let open = p + 8;
        let inner = crate::lint::bracket(&l, open);
        let els = crate::lint::split_top(&inner, ',');
        let key = |c: &String| -> String {
            for pre in ["&mut ", "&", "move "] {
                if let Some(r) = c.strip_prefix(pre) {
                    return r.to_string();
                }
            }
            c.clone()
        };
        let mut sorted = els.clone();
        sorted.sort_by_key(|c| key(c));
        if sorted != els {
            l = format!("{}{}{}", &l[..open + 1], sorted.join(", "), &l[open + 1 + inner.len()..]);
        }
    }
    // trait の requires (行末、`pub` が付くことがある)。
    if l.starts_with("trait ") {
        if let Some(p) = l.find(" requires ") {
            let rest = &l[p + 10..];
            let (body, tail) = match rest.strip_suffix(" pub") {
                Some(b) => (b, " pub"),
                None => (rest, ""),
            };
            let rs = crate::lint::split_top(body, ',');
            let mut sorted = rs.clone();
            sorted.sort();
            if sorted != rs {
                l = format!("{} requires {}{}", &l[..p], sorted.join(", "), tail);
            }
        }
    }
    // 境界の `+` の並び (`linear` が先、通常は名前順、ライフタイムは最後)。
    let gen_open = if l.starts_with("impl[") {
        Some(4)
    } else {
        Regex::new(r"^(?:fn|type|enum|trait)\s+[A-Za-z0-9_.]+\[").unwrap().find(&l).map(|m| m.end() - 1)
    };
    if let Some(open) = gen_open {
        let gen = crate::lint::bracket(&l, open);
        let ents = crate::lint::split_top(&gen, ',');
        let mut changed = false;
        let fixed: Vec<String> = ents
            .iter()
            .map(|ent| {
                let Some((name, b)) = ent.split_once(':') else { return ent.clone() };
                let bound = b.trim();
                if bound == "uses" || bound == "panics" || !crate::lint::strip_args(bound).contains('+') {
                    return ent.clone();
                }
                let parts: Vec<String> = crate::lint::split_top(bound, '+').iter().map(|p| p.trim().to_string()).collect();
                let rank = |p: &String| -> (u8, String) {
                    if p == "linear" {
                        (0, String::new())
                    } else if p.starts_with('\'') {
                        (2, p.clone())
                    } else {
                        (1, p.clone())
                    }
                };
                let mut sorted = parts.clone();
                sorted.sort_by_key(|p| rank(p));
                if sorted != parts {
                    changed = true;
                }
                format!("{}: {}", name, sorted.join(" + "))
            })
            .collect();
        if changed {
            l = format!("{}{}{}", &l[..open + 1], fixed.join(", "), &l[open + 1 + gen.len()..]);
        }
    }
    l
}

/// 末尾位置の `return` (4.6、S7): 関数・クロージャの本体のブロックの最後の式、その `if` (`else` あり) の枝と `match` の腕。
/// `(行, 列, 値あり)` を集める。値なしの `return` は `()` に、値ありは `return ` を外す。
fn tail_returns(v: &crate::parser::V, unit: bool, out: &mut Vec<(usize, usize, bool)>) {
    use crate::parser::V;
    let V::Node(kind, ch, pos, _) = v else { return };
    match *kind {
        "Block" => {
            if let Some(V::L(stmts)) = ch.first() {
                if let Some(V::Node("ExprStmt", e, _, _)) = stmts.last() {
                    if let Some(x) = e.first() {
                        tail_returns(x, unit, out);
                    }
                }
            }
        }
        "Return" => {
            let (l, c) = crate::parser::unpack_pos(*pos);
            let has = !matches!(ch.first(), Some(V::Nil) | None);
            // 値のない `return` は、戻り値が `()` の関数だけ `()` に直す (そうでないものは元から誤りで、意味を変えない)。
            if has || unit {
                out.push((l, c, has));
            }
        }
        "If" => {
            if ch.len() == 3 && !matches!(ch[2], V::Nil) {
                tail_returns(&ch[1], unit, out);
                tail_returns(&ch[2], unit, out);
            }
        }
        "Match" => {
            if let Some(V::L(arms)) = ch.get(1) {
                for a in arms {
                    if let V::L(pb) = a {
                        if let Some(b) = pb.get(1) {
                            tail_returns(b, unit, out);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn is_unit(t: &crate::parser::V) -> bool {
    use crate::parser::V;
    matches!(t, V::L(xs) if xs.len() == 2 && matches!(&xs[0], V::S(k) if k == "tuple") && matches!(&xs[1], V::L(i) if i.is_empty()))
}

/// 本体の全ノードを歩き、関数本体 (`root`) とクロージャの本体から末尾の `return` を集める。
fn collect_tail_returns(v: &crate::parser::V, root: Option<bool>, out: &mut Vec<(usize, usize, bool)>) {
    use crate::parser::V;
    match v {
        V::Node(kind, ch, _, _) => {
            if let Some(unit) = root {
                tail_returns(v, unit, out);
            }
            if *kind == "Closure" {
                if let Some(b) = ch.get(3) {
                    tail_returns(b, ch.get(1).map_or(false, is_unit), out);
                }
            }
            for c in ch {
                collect_tail_returns(c, None, out);
            }
        }
        V::L(xs) => {
            for c in xs {
                collect_tail_returns(c, None, out);
            }
        }
        _ => {}
    }
}

/// S7 (末尾の `return`) と S16 (`while true`) を直す。行数は変えない。
fn fix_tails(defs: &[crate::program::Def], lines: &mut [String]) {
    let mut edits: Vec<(usize, usize, bool)> = vec![];
    for d in defs {
        if matches!(d.kw.as_str(), "fn" | "test") {
            if let Some(b) = &d.body {
                collect_tail_returns(b, Some(is_unit(&d.ret)), &mut edits);
            }
        }
    }
    edits.sort();
    edits.dedup();
    for (l, c, has) in edits.into_iter().rev() {
        let Some(line) = lines.get_mut(l - 1) else { continue };
        let mut ch: Vec<char> = line.chars().collect();
        let at = c.saturating_sub(1);
        let word: Vec<char> = "return".chars().collect();
        if ch.get(at..at + 6) != Some(&word[..]) {
            continue;
        }
        if has && ch.get(at + 6) == Some(&' ') {
            ch.splice(at..at + 7, std::iter::empty());
        } else if !has {
            ch.splice(at..at + 6, "()".chars());
        } else {
            continue;
        }
        *line = ch.into_iter().collect();
    }
    for l in lines.iter_mut() {
        if l.trim_start() == "while true {" {
            *l = l.replacen("while true", "loop", 1);
        }
    }
}

/// 100 文字に収まる文の折り返し (引数・要素を 1 行 1 要素にした形、チェーンの `.` 行) を 1 行にする (D15 の S20)。
/// ブロックを含む文 (行末が `{`、行頭が `}`) とコメント行を含む文は、そのまま。
fn unwrap_short(lines: &mut Vec<String>) -> bool {
    let cont_end = |l: &str| {
        let t = l.trim_end();
        !t.trim_start().starts_with("//") && (t.ends_with('(') || t.ends_with('[') || t.ends_with(','))
    };
    let mut out: Vec<String> = vec![];
    let mut changed = false;
    let mut i = 0;
    while i < lines.len() {
        let l = &lines[i];
        let indent = l.chars().take_while(|c| *c == ' ').count();
        let next_dot = |k: usize| lines.get(k).map_or(false, |n| n.trim_start().starts_with('.') && !n.trim_start().starts_with(".."));
        let starts_group = indent > 0 && !l.trim_start().starts_with("//") && (cont_end(l) || next_dot(i + 1));
        if !starts_group {
            out.push(l.clone());
            i += 1;
            continue;
        }
        // 継続している行を集める。
        let mut j = i;
        while j + 1 < lines.len() && (cont_end(&lines[j]) || next_dot(j + 1)) {
            j += 1;
        }
        let group = &lines[i..=j];
        let blocky = group.iter().any(|g| {
            let t = g.trim();
            t.ends_with('{') || t.starts_with('}') || t.starts_with("//") || t.is_empty()
        });
        let mut joined = group[0].trim_end().to_string();
        if !blocky {
            for g in &group[1..] {
                let n = g.trim();
                let closing = n.starts_with(')') || n.starts_with(']');
                if closing && joined.ends_with(',') {
                    joined.pop();
                }
                let tight = joined.ends_with('(') || joined.ends_with('[') || n.starts_with('.') || closing;
                if !tight {
                    joined.push(' ');
                }
                joined.push_str(n);
            }
        }
        if !blocky && joined.chars().count() <= 100 && group.len() > 1 {
            out.push(joined);
            changed = true;
        } else {
            out.extend(group.iter().cloned());
        }
        i = j + 1;
    }
    *lines = out;
    changed
}

/// 構文の揺れ (D8・D10・D11・D15) を直す。パーサが書き換えの候補 (`parser::rewrites`) を返すので、重ならないものを適用し、
/// 候補が尽きるまで (新しい形が別の書き換えを生むので) 繰り返す。関数・テストの本体の中だけ。
fn fix_syntax(path: &str, own_len: bool, lines: &mut Vec<String>) {
    use crate::parser::{Part, Rw};
    for _ in 0..40 {
        let mut progress = false;
        {
            let mut cand = lines.clone();
            if unwrap_short(&mut cand) && load_file(path, "", &(cand.join("\n") + "\n")).is_ok() {
                *lines = cand;
                progress = true;
            }
        }
        let joined = lines.join("\n") + "\n";
        let Ok(defs) = load_file(path, "", &joined) else { return };
        let Ok(all) = lexer::lex(&joined) else { return };
        let chars: Vec<char> = joined.chars().collect();
        let mut starts: Vec<usize> = vec![0];
        for (i, c) in chars.iter().enumerate() {
            if *c == '\n' {
                starts.push(i + 1);
            }
        }
        let mut edits: Vec<(usize, usize, String)> = vec![];
        for d in &defs {
            if !matches!(d.kw.as_str(), "fn" | "test") || d.body.is_none() || d.end_line <= d.line + 1 {
                continue;
            }
            let toks: Vec<Tok> = all.iter().filter(|t| t.line > d.line && t.line < d.end_line && t.kind != "eof").cloned().collect();
            let (t, rws) = crate::parser::rewrites(toks, &d.name, own_len);
            let off = |k: usize| starts[t[k].line - 1] + t[k].col - 1;
            let end = |k: usize| off(k) + t[k].text.chars().count();
            let span = |a: usize, b: usize| -> String {
                if a >= b {
                    String::new()
                } else {
                    chars[off(a)..end(b - 1)].iter().collect()
                }
            };
            for rw in rws {
                let Rw { a, b, parts, del_line, inside } = rw;
                let (s0, e0) = (off(a), end(b - 1));
                let ln = t[a].line;
                if del_line {
                    let nxt = starts.get(ln).copied().unwrap_or(chars.len());
                    edits.push((starts[ln - 1], nxt, String::new()));
                    continue;
                }
                let mut text = String::new();
                for p in &parts {
                    match p {
                        Part::Lit(x) => text.push_str(x),
                        Part::Span(x, y) => text.push_str(&span(*x, *y)),
                        Part::Dedent(x, y) => text.push_str(&span(*x, *y).replace("\n    ", "\n")),
                    }
                }
                // 書き換える範囲の中のコメント行 (`.ksnh`) は、その文の行の直前に移す (`strip` が正規形を作れるように)。
                let region: String = chars[s0..e0].iter().collect();
                let mut moved: Vec<String> = vec![];
                if region.contains('\n') {
                    moved = region.lines().filter(|l| l.trim_start().starts_with("//")).map(|l| l.trim_start().to_string()).collect();
                    text = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
                }
                let mut ls = starts[ln - 1];
                if !inside && !moved.is_empty() {
                    // `match` の腕の間にコメントは置けないので、腕を含む文の行 (`=>` で終わる行・`}` で始まる行でない、字下げの浅い行) の直前へ。
                    let line_of = |k: usize| -> (usize, String) {
                        let b = starts[k];
                        let e = starts.get(k + 1).map_or(chars.len(), |x| x - 1);
                        let l: String = chars[b..e].iter().collect();
                        (l.chars().take_while(|c| *c == ' ').count(), l)
                    };
                    let mut k = ln - 1;
                    let mut ind = line_of(k).0;
                    while k > 0 {
                        k -= 1;
                        let (i2, l) = line_of(k);
                        if l.trim().is_empty() || i2 >= ind {
                            continue;
                        }
                        ind = i2;
                        let tr = l.trim_start();
                        if l.contains("=>") || tr.starts_with('}') {
                            continue;
                        }
                        ls = starts[k];
                        break;
                    }
                }
                let indent: String = chars[ls..].iter().take_while(|c| **c == ' ').collect();
                let prefix: String = chars[ls..s0].iter().collect();
                let mut full = String::new();
                if inside {
                    let (first, rest) = text.split_once('\n').unwrap_or((text.as_str(), ""));
                    full.push_str(&prefix);
                    full.push_str(first);
                    for c in &moved {
                        full.push_str(&format!("\n{}    {}", indent, c));
                    }
                    if !rest.is_empty() {
                        full.push('\n');
                        full.push_str(rest);
                    }
                } else {
                    for c in &moved {
                        full.push_str(&format!("{}{}\n", indent, c));
                    }
                    full.push_str(&prefix);
                    full.push_str(&text);
                }
                // 行が 100 文字を超えるなら書き換えない (折り返しは `fmt` が決めない)。引用符のある行は対象外 (文法の例外)。
                let tail: String = chars[e0..].iter().take_while(|c| **c != '\n').collect();
                let too_long = |l: &str| l.chars().count() > 100 && !l.contains('"');
                let res = format!("{}{}", full, tail);
                if res.lines().any(|l| too_long(l)) && !chars[ls..e0].iter().collect::<String>().lines().any(|l| too_long(l)) {
                    continue;
                }
                if moved.is_empty() {
                    edits.push((s0, e0, text));
                } else {
                    edits.push((ls, e0, full));
                }
            }
        }
        if edits.is_empty() {
            if progress {
                continue;
            }
            return;
        }
        edits.sort_by(|x, y| x.0.cmp(&y.0).then(y.1.cmp(&x.1)));
        let mut chosen: Vec<(usize, usize, String)> = vec![];
        let mut last = 0usize;
        for e in edits {
            if chosen.is_empty() || e.0 >= last {
                last = e.1;
                chosen.push(e);
            }
        }
        let mut out = chars.clone();
        for (s0, e0, text) in chosen.into_iter().rev() {
            out.splice(s0..e0, text.chars());
        }
        let new: String = out.into_iter().collect();
        if new == joined {
            return;
        }
        let canon_new: Vec<String> = new.trim_end_matches('\n').split('\n').map(|l| crate::spacing::canon_line(l)).collect();
        // 書き換えた結果が解析できないなら、書き換えない (括弧の片方だけが外れるなどの取りこぼしを残さない)。
        if load_file(path, "", &(canon_new.join("\n") + "\n")).is_err() {
            return;
        }
        *lines = canon_new;
    }
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
    format_source_with(text, path, true, false)
}

/// プログラム (パッケージの全ソース) が `len` / `is_empty` を std でない独自のメソッドとして定義しているか。
/// 定義しているときは、D11 の `x.len() == 0` → `x.is_empty()` の書き換えをしない (利用者の `len` と `is_empty` が
/// 別の意味かもしれず、受け手の型は字詞の段階では分からないため)。std の FQN (`core.` / `alloc.` / `std.`) は除く。
pub fn own_len_method(srcs: &[&str]) -> bool {
    srcs.iter().any(|t| t.lines().any(|l| is_own_len_def(l.trim_start())))
}

/// 行が `fn <FQN>.len(` / `fn <FQN>.is_empty(` の定義ヘッダで、FQN が std のものではないか。
/// 文字列・コメントが行をまたぐことはないので、行頭が `fn ` なら定義ヘッダ (字句の解析は要らない)。
fn is_own_len_def(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("fn ") else { return false };
    let name = rest.split('(').next().unwrap_or("").trim_end();
    // 名前の最後の成分 (`t.Item.len` の `len`)。`.` が無いフリーの `fn len(` も独自定義と数える (抑制側に倒す)。
    let Some(last) = name.rsplit('.').next() else { return false };
    if !matches!(last, "len" | "is_empty") {
        return false;
    }
    !matches!(name.split('.').next().unwrap_or(""), "core" | "alloc" | "std")
}

/// プログラムの `len` / `is_empty` の独自定義 (`own_len_method`) を知って整形する (`ksn fmt` / `ksn check`・LSP 用)。
pub fn format_source_prog(text: &str, path: &str, own_len: bool) -> String {
    format_source_with(text, path, true, own_len)
}

/// `canon` が偽なら、名前の参照形 (3.3) は書き換えず、定義の並びと空行だけを整える (`ksn rename` 用)。
pub fn format_source_with(text: &str, path: &str, canon: bool, own_len: bool) -> String {
    let stripped;
    let text = if canon && !path.ends_with(".sig") {
        stripped = strip_line_marks(text, &["E0040", "E0049"]);
        stripped.as_str()
    } else {
        text
    };
    let mut lines = mechanical(text);
    if canon {
        fix_forms(&mut lines);
        // 1 行の中のトークンの間隔 (spacing.rs の表)。
        for l in lines.iter_mut() {
            *l = fix_lists(&crate::spacing::canon_line(&crate::canon::numbers_line(l)));
        }
    }
    if canon {
        fix_syntax(path, own_len, &mut lines);
    }
    let joined = lines.join("\n") + "\n";
    let defs = match load_file(path, "", &joined) {
        Ok(d) => d,
        Err(_) => return joined,
    };
    if canon {
        fix_tails(&defs, &mut lines);
    }
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
    // 離れたコメント (空行を挟んで定義の前に置いたもの。3.2 の先頭の説明を除く) は、空行を詰めて次の定義の
    // 直前に付ける (定義と一緒に動く)。最後の定義より後のコメントは、直せないのでファイルの末尾に残す。
    let md0 = crate::doclink::module_doc_len(&lines);
    let last_end = defs.iter().map(|d| d.end_line.max(d.line)).max().unwrap_or(0);
    let tail: Vec<String> = lines.iter().skip(last_end).filter(|l| !l.trim().is_empty()).cloned().collect();
    for d in &defs {
        if d.line == 0 {
            continue;
        }
        let mut s = d.line - 1;
        while s > 0 && lines[s - 1].starts_with("//") {
            s -= 1;
        }
        let mut s0 = s;
        while s0 > md0 && (lines[s0 - 1].starts_with("//") || lines[s0 - 1].trim().is_empty()) {
            s0 -= 1;
        }
        if dropped.contains(&d.line) {
            continue;
        }
        let e = if d.end_line > 0 { d.end_line } else { d.line };
        let mut body: Vec<String> = lines[s0..s].iter().filter(|l| !l.trim().is_empty()).cloned().collect();
        let off = body.len();
        // 空行は定義の間だけ 1 行 (本体の中は 0。設計_書き方は1通り.md 12 章)。
        body.extend(lines[s..e].iter().filter(|l| !l.trim().is_empty()).cloned());
        for (i, l) in body.iter_mut().enumerate() {
            if (l.starts_with("type ") || l.starts_with("enum ")) && (d.kw == "type" || d.kw == "enum") && i == off + d.line - 1 - s {
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
    // import は 1 つの塊に並べる。直前のコメント (`//` `///`) は import の行と一緒に動く (失わない)。
    let imp: Vec<String> = groups[0].iter().flat_map(|&i| items[i].lines.clone()).collect();
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
    // ファイル先頭のモジュールの説明 (3.2) は、先頭に空行 1 つを挟んでそのまま残す。
    let md = crate::doclink::module_doc_len(&lines);
    if md > 0 {
        res.extend(lines[..md].iter().cloned());
        res.push(String::new());
    }
    for (k, chunk) in out.into_iter().enumerate() {
        if k > 0 {
            res.push(String::new());
        }
        res.extend(chunk);
    }
    if !tail.is_empty() {
        if !res.is_empty() {
            res.push(String::new());
        }
        res.extend(tail);
    }
    if res.is_empty() { String::new() } else { res.join("\n") + "\n" }
}

fn ind(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

fn pad(n: usize) -> String {
    " ".repeat(n)
}

/// 条件 `c` の否定。空白を含む式は括る (`!(a == b)`)。
fn neg(c: &str) -> String {
    if c.contains(' ') { format!("!({})", c) } else { format!("!{}", c) }
}

/// 1 行の形: `if C { true } else { false }` → `C` (値の位置・文の位置とも、行の終わりまでの式)、
/// `if C { false } else { true }` → `!C`。`C` は波括弧を含まないものだけ。
fn bool_if_line(l: &str) -> Option<String> {
    let re = Regex::new(r"^(.*?)if ([^{}]+?) \{ (true|false) \} else \{ (true|false) \}$").unwrap();
    let c = re.captures(l)?;
    let pre = c.get(1)?.as_str();
    // 前は行頭の字下げか、`let 名前 = ` / `return ` だけ (二項演算の中の `if` は書き換えない)。
    let ok_pre = pre.trim().is_empty() || pre.ends_with("= ") || pre.ends_with("return ");
    if !ok_pre {
        return None;
    }
    let cond = c.get(2)?.as_str();
    let rewritten = match (c.get(3)?.as_str(), c.get(4)?.as_str()) {
        ("true", "false") => cond.to_string(),
        ("false", "true") => neg(cond),
        _ => return None,
    };
    Some(format!("{}{}", pre, rewritten))
}

/// no-op の参照と間接参照の対 (`*&*r` → `*r`、`*&x` → `x`、`&*&x` → `&x`)。
/// 値の意味は同じだが、`*&` は可変性を変える (`&mut *&x`・`*&x = ..` は拒まれる形なのに `&mut x` へ直すと通ってしまう)。
/// そこで `*&` は `let 名前 = <全体>` の右辺だけ (値として読む)、`&*&` は `&mut` にならない形 (共有から共有) だけ直す。
fn noop_refs(l: &str) -> String {
    let t = l.trim_start();
    let lead = &l[..l.len() - t.len()];
    if t.starts_with("let ") {
        if let Some((head, rhs)) = t.split_once(" = ") {
            let rhs = rhs.trim_end();
            let body = if let Some(r) = rhs.strip_prefix("*&*") {
                if r.chars().all(|c| c.is_alphanumeric() || c == '_') && !r.is_empty() && r != "mut" { Some(format!("*{}", r)) } else { None }
            } else if let Some(r) = rhs.strip_prefix("*&") {
                if r.chars().all(|c| c.is_alphanumeric() || c == '_') && !r.is_empty() && r != "mut" && !r.starts_with(|c: char| c.is_ascii_digit()) { Some(r.to_string()) } else { None }
            } else {
                None
            };
            if let Some(b) = body {
                return format!("{}{} = {}", lead, head, b);
            }
        }
    }
    // `&*&x` → `&x` (共有から共有へ。`&mut` の形は変えない)。
    let b = Regex::new(r"(^|[\s(,=\[{!:])&\*&([A-Za-z_]\w*)").unwrap();
    b.replace_all(l, |caps: &regex::Captures| {
        if &caps[2] == "mut" {
            caps[0].to_string()
        } else {
            format!("{}&{}", &caps[1], &caps[2])
        }
    })
    .into_owned()
}

/// 複数行の形 (字下げ `I` の `if` 行 `i` から):
/// - `if C {` / `true` / `} else {` / `false` / `}` → `C` (偽と真の逆も `!C`)。
/// - then の末尾が `return` で else があるなら、else の本体を `if` の後ろへ出す (`else` を消す)。
///   else に `let` の文があるときは、名前の見え方が変わるので触らない。
/// 変えたら true。
fn block_forms(lines: &mut Vec<String>, i: usize) -> bool {
    let t = lines[i].trim_end().to_string();
    let ts = t.trim_start();
    // 行頭の `if`、または `let 名前 = if` / `return if` の値の `if` (真偽の形だけ)。
    let (pre, body) = match ts.find("if ") {
        Some(0) => (String::new(), ts),
        Some(p) if ts[..p].ends_with("= ") || ts[..p].ends_with("return ") => (ts[..p].to_string(), &ts[p..]),
        _ => return false,
    };
    if !(body.starts_with("if ") && body.ends_with(" {")) {
        return false;
    }
    let ii = ind(&lines[i]);
    let cond = body[3..body.len() - 2].trim().to_string();
    // (2) 複数行の真偽の形
    if lines.len() > i + 4 && !cond.is_empty() && !cond.contains('{') {
        let (a, b) = (lines[i + 1].trim(), lines[i + 3].trim());
        let closing = lines[i + 2] == format!("{}}} else {{", pad(ii)) && lines[i + 4] == format!("{}}}", pad(ii));
        let inner = ind(&lines[i + 1]) == ii + 4 && ind(&lines[i + 3]) == ii + 4;
        let pair = (a, b) == ("true", "false") || (a, b) == ("false", "true");
        if closing && inner && pair {
            let c = if a == "true" { cond.clone() } else { neg(&cond) };
            lines[i] = format!("{}{}{}", pad(ii), pre, c);
            lines.drain(i + 1..i + 5);
            return true;
        }
    }
    if !pre.is_empty() {
        return false;
    }
    // (8) return で終わる then と else
    let mut k = i + 1;
    while k < lines.len() && !(ind(&lines[k]) == ii && lines[k].trim_start().starts_with('}')) {
        k += 1;
    }
    if k >= lines.len() || lines[k].trim() != "} else {" || k == i + 1 {
        return false;
    }
    let mut p = k;
    while p > i + 1 && lines[p - 1].trim().is_empty() {
        p -= 1;
    }
    if p == i + 1 || ind(&lines[p - 1]) != ii + 4 || !(lines[p - 1].trim_start() == "return" || lines[p - 1].trim_start().starts_with("return ")) {
        return false;
    }
    let mut m = k + 1;
    while m < lines.len() && !(ind(&lines[m]) == ii && lines[m].trim() == "}") {
        if ind(&lines[m]) == ii && !lines[m].trim().is_empty() {
            return false;
        }
        m += 1;
    }
    if m >= lines.len() || m == k + 1 {
        return false;
    }
    let else_body = &lines[k + 1..m];
    if else_body.iter().any(|l| !l.trim().is_empty() && ind(l) < ii + 4) || else_body.iter().any(|l| l.trim_start().starts_with("let ") && ind(l) == ii + 4) {
        return false;
    }
    let moved: Vec<String> = else_body.iter().map(|l| if l.trim().is_empty() { String::new() } else { l[4..].to_string() }).collect();
    let mut out: Vec<String> = lines[..k].to_vec();
    out.push(format!("{}}}", pad(ii)));
    out.extend(moved);
    out.extend(lines[m + 1..].iter().cloned());
    *lines = out;
    true
}

/// 整形の形の規則 (設計_書き方は1通り.md 12 章。意味を変えないと機械で言い切れるものだけ)。
fn fix_forms(lines: &mut Vec<String>) {
    for l in lines.iter_mut() {
        *l = static_str_let(l);
        if l.trim_start().starts_with("//") || l.contains('"') || l.contains('\'') {
            continue;
        }
        if let Some(n) = bool_if_line(l) {
            *l = n;
        }
        *l = noop_refs(l);
        *l = unused_arm_binding(l);
    }
    let mut i = 0;
    while i < lines.len() {
        if !block_forms(lines, i) {
            if !let_tail(lines, i) {
                empty_else(lines, i);
            }
        }
        i += 1;
    }
}

/// `} else {` の直後が同じ字下げの `}` (空の else) なら、else を消す。
fn empty_else(lines: &mut Vec<String>, i: usize) -> bool {
    if i + 1 < lines.len() && lines[i].trim() == "} else {" && lines[i + 1].trim() == "}" && ind(&lines[i]) == ind(&lines[i + 1]) {
        lines[i] = format!("{}}}", pad(ind(&lines[i])));
        lines.remove(i + 1);
        return true;
    }
    false
}

/// `let x = e` の次の行が同じ字下げの `x` で、その次がブロックの終わり (字下げの浅い行) なら、`e` に直す。
/// 注釈・`mut`・複数行の `e` は触らない (x の型を変えない・式の途中で切らない)。
fn let_tail(lines: &mut Vec<String>, i: usize) -> bool {
    if i + 2 >= lines.len() {
        return false;
    }
    let l = lines[i].clone();
    let ii = ind(&l);
    let t = l.trim_start();
    let Some(rest) = t.strip_prefix("let ") else { return false };
    let Some((name, expr)) = rest.split_once(" = ") else { return false };
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') || name.starts_with(|c: char| c.is_ascii_digit()) {
        return false;
    }
    if expr.trim_end().ends_with('{') || expr.trim().is_empty() || expr.contains('"') || expr.contains('\'') {
        return false;
    }
    if lines[i + 1].trim_end() != format!("{}{}", pad(ii), name) || lines[i + 1].trim() != name {
        return false;
    }
    if !(ind(&lines[i + 2]) < ii && !lines[i + 2].trim().is_empty()) {
        return false;
    }
    lines[i] = format!("{}{}", pad(ii), expr);
    lines.remove(i + 1);
    true
}

/// 1 行の `_名前 => e` の腕で、パターンの `_名前` が本体にも無い (使わない) ものを `_` にする。
/// ガード (`if`) のある腕と、文字列を含む行は触らない。`_` だけの名前は変えない。
fn unused_arm_binding(l: &str) -> String {
    if l.contains('"') || l.contains('\'') || l.contains(" if ") {
        return l.to_string();
    }
    let Some((pat, body)) = l.split_once(" => ") else { return l.to_string() };
    let re = Regex::new(r"\b_[A-Za-z0-9_]+\b").unwrap();
    let mut out = pat.to_string();
    let names: Vec<String> = re.find_iter(pat).map(|m| m.as_str().to_string()).collect();
    for n in names {
        let used = Regex::new(&format!(r"\b{}\b", regex::escape(&n))).unwrap().is_match(body);
        if !used {
            out = Regex::new(&format!(r"\b{}\b", regex::escape(&n))).unwrap().replace_all(&out, "_").into_owned();
        }
    }
    format!("{} => {}", out, body)
}

/// `let 名前: &'static str = "文字列"` の `'static` は、文字列の値の型から同じ意味になる (`&str`)。
fn static_str_let(l: &str) -> String {
    let re = Regex::new(r#"^(\s*let \w+: )&'static str = ("[^"\\]*")$"#).unwrap();
    match re.captures(l) {
        Some(c) => format!("{}&str = {}", &c[1], &c[2]),
        None => l.to_string(),
    }
}
