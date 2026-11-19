//! 診断に付ける機械適用可能な修正案 (`fix`) の生成と適用 (仕様 7.1)。
//!
//! 修正は診断の位置とメッセージ、ソース行から作る。範囲は半開区間、行と列は 1 始まり、列は Unicode スカラ値単位。

use regex::Regex;

#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub replace: String,
}

/// 行 `text` の文字位置 (0 始まり) を、バイト位置から求める。
fn char_idx(text: &str, byte: usize) -> usize {
    text[..byte].chars().count()
}

fn span(file: &str, line: usize, text: &str, from: usize, to: usize, replace: String) -> Fix {
    Fix {
        file: file.to_string(),
        line,
        col: char_idx(text, from) + 1,
        end_line: line,
        end_col: char_idx(text, to) + 1,
        replace,
    }
}

/// 診断 (`code`, `msg`, 位置 `line`) に対する修正案。無ければ空。
pub fn fixes_for(code: &str, msg: &str, file: &str, line: usize, col: usize, lines: &[String]) -> Vec<Fix> {
    let get = |n: usize| lines.get(n.wrapping_sub(1)).map(|s| s.as_str());
    match code {
        "E1108" => {
            // 整数の `to_<整数型>` (types.rs の int_to_hint): `r.to_u32()` を `u32.from_u8(r)` にする。
            // 受け手は直前の単純な名前だけ、呼び出しはその行に 1 回だけのときに限る。
            let conv = Regex::new(r"unknown method `(to_[a-z0-9]+)` on `[a-z0-9]+`; use `([a-z0-9]+)\.(from_[a-z0-9]+)\(x\)` \(lossless\)").unwrap();
            if let Some(c) = conv.captures(msg) {
                let Some(text) = get(line) else { return vec![] };
                let call = format!(".{}()", &c[1]);
                if text.matches(&call).count() != 1 {
                    return vec![];
                }
                let at = text.find(&call).unwrap();
                let head = &text[..at];
                let ident_len: usize = head.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').map(char::len_utf8).sum();
                if ident_len == 0 {
                    return vec![];
                }
                let start = at - ident_len;
                let recv = &head[start..];
                if recv.starts_with(|c: char| c.is_ascii_digit()) || head[..start].ends_with(|c: char| c == '.' || c.is_alphanumeric() || c == '_') {
                    return vec![];
                }
                return vec![span(file, line, text, start, at + call.len(), format!("{}.{}({})", &c[2], &c[3], recv))];
            }
            // unknown method `X`; did you mean `Y`?
            let re = Regex::new(r"unknown method `([^`]+)`; did you mean `([^`]+)`").unwrap();
            let Some(c) = re.captures(msg) else { return vec![] };
            let (bad, good) = (&c[1], &c[2]);
            let Some(text) = get(line) else { return vec![] };
            let needle = format!(".{}", bad);
            let mut from = 0;
            while let Some(p) = text[from..].find(&needle) {
                let start = from + p + 1;
                let end = start + bad.len();
                let after = text[end..].chars().next();
                if !matches!(after, Some(c) if c.is_alphanumeric() || c == '_') {
                    return vec![span(file, line, text, start, end, good.to_string())];
                }
                from = end;
            }
            vec![]
        }
        // 字句の規則 (lexrules.rs): エスケープを唯一の書き方に直す (E0005)、NFC に直す (E0006)。
        "E0005" => {
            let Some(text) = get(line) else { return vec![] };
            let Some(tag) = msg.strip_prefix('[').and_then(|m| m.split(']').next()) else { return vec![] };
            let Some(lit) = crate::lexrules::Lit::of_tag(tag) else { return vec![] };
            let Some((len, rep)) = crate::lexrules::escape_fix(text, col, lit) else { return vec![] };
            let start: usize = text.char_indices().nth(col - 1).map_or(text.len(), |(b, _)| b);
            let end: usize = text.char_indices().nth(col - 1 + len).map_or(text.len(), |(b, _)| b);
            vec![span(file, line, text, start, end, rep)]
        }
        "E0006" => {
            let Some(text) = get(line) else { return vec![] };
            let Some(rep) = crate::lexrules::nfc_fix(text, col) else { return vec![] };
            let start: usize = text.char_indices().nth(col - 1).map_or(text.len(), |(b, _)| b);
            vec![span(file, line, text, start, text.len(), rep)]
        }
        // Rust 風の書き方 (rustisms.rs): `;` を消す、`::` を `.` にする、複合代入を展開する。
        "E0040" | "E0041" => {
            let Some(text) = get(line) else { return vec![] };
            let (len, rep) = if code == "E0040" { (1, "") } else { (2, ".") };
            let bytes: Vec<(usize, char)> = text.char_indices().collect();
            let Some(&(b0, _)) = bytes.get(col.wrapping_sub(1)) else { return vec![] };
            let b1 = bytes.get(col - 1 + len).map_or(text.len(), |&(b, _)| b);
            vec![span(file, line, text, b0, b1, rep.to_string())]
        }
        "E1101" => {
            let re = Regex::new(r"unresolved name (\S+); did you mean (\S+)\?").unwrap();
            let Some(c) = re.captures(msg) else { return vec![] };
            let (bad, good) = (&c[1], &c[2]);
            let Some(text) = get(line) else { return vec![] };
            let Some(p) = text.find(bad) else { return vec![] };
            vec![span(file, line, text, p, p + bad.len(), good.to_string())]
        }
        // 接尾辞のない整数リテラル (D12): 推論した型を接尾辞として足す。
        "E2028" => {
            let re = Regex::new(r"append `([a-z0-9]+)`").unwrap();
            let Some(c) = re.captures(msg) else { return vec![] };
            let Some(text) = get(line) else { return vec![] };
            let cs: Vec<(usize, char)> = text.char_indices().collect();
            let Some(&(b0, _)) = cs.get(col.wrapping_sub(1)) else { return vec![] };
            let mut b1 = b0;
            for &(b, ch) in &cs[col - 1..] {
                if ch.is_ascii_alphanumeric() || ch == '_' || (b == b0 && ch == '-') {
                    b1 = b + ch.len_utf8();
                } else {
                    break;
                }
            }
            if b1 == b0 {
                return vec![];
            }
            vec![span(file, line, text, b1, b1, c[1].to_string())]
        }
        // 構造体リテラルの欄を宣言順に並べ替える (D16)。初期化式に副作用・panic がありうる (呼び出し・添字・`?`) ときは直さない。
        "E2048" => struct_order_fix(file, msg, line, col, lines),
        // 受け手のあるメソッドの経路形の呼び出し (D8): `T.m(&x, a)` を `x.m(a)` にする。
        "E2029" => method_call_fix(file, line, col, lines),
        "E0049" => {
            let Some(text) = get(line) else { return vec![] };
            let bytes: Vec<(usize, char)> = text.char_indices().collect();
            let Some(&(b0, _)) = bytes.get(col.wrapping_sub(1)) else { return vec![] };
            vec![span(file, line, text, b0, b0 + 1, String::new())]
        }
        // match の腕: 次の行が `{` だけなら、`=>` の行末から `{` の行末までを ` {` にして改行を消す。
        "E0057" => {
            let (Some(text), Some(next)) = (get(line), get(line + 1)) else { return vec![] };
            if !text.trim_end().ends_with("=>") || next.trim() != "{" {
                return vec![];
            }
            vec![Fix {
                file: file.to_string(),
                line,
                col: text.trim_end().chars().count() + 1,
                end_line: line + 1,
                end_col: next.trim_end().chars().count() + 1,
                replace: " {".to_string(),
            }]
        }
        // match の腕の本体の代入 `Some(r) => result = r` を、腕の行に `{`、次の行に代入、閉じの `}` の 3 行にする。
        "E0058" => {
            let Some(text) = get(line) else { return vec![] };
            let re = Regex::new(r"^(\s*)(.*?=>)\s*([A-Za-z_][A-Za-z0-9_.]*\s*=[^=>].*)$").unwrap();
            let Some(c) = re.captures(text) else { return vec![] };
            let (indent, head, stmt) = (&c[1], &c[2], c[3].trim_end());
            vec![span(
                file,
                line,
                text,
                0,
                text.len(),
                format!("{}{} {{\n{}    {}\n{}}}", indent, head, indent, stmt, indent),
            )]
        }
        "E0045" => {
            let Some(text) = get(line) else { return vec![] };
            let re = Regex::new(r"^(\s*)(.+?)\s*([-+*/%&|^])=\s*(.+?)\s*$").unwrap();
            let Some(c) = re.captures(text) else { return vec![] };
            let new = format!("{}{} = {} {} {}", &c[1], &c[2], &c[2], &c[3], &c[4]);
            vec![span(file, line, text, 0, text.len(), new)]
        }
        // 演算子の片側だけ参照が多い (types.rs の operator_ref_operands): 単純な名前の被演算子に `*` を足す。
        "E2024" => {
            let re = Regex::new(r"^operands of `([^`]+)` are .*\(the (left|right) operand has more references\)").unwrap_or_else(|_| unreachable!());
            let Some(c) = re.captures(msg) else { return vec![] };
            let Some(text) = get(line) else { return vec![] };
            let bytes: Vec<(usize, char)> = text.char_indices().collect();
            let Some(&(b0, _)) = bytes.get(col.wrapping_sub(1)) else { return vec![] };
            let rest = &text[b0..];
            let id = r"[A-Za-z_][A-Za-z0-9_]*";
            let op = regex::escape(&c[1]);
            let at = if &c[2] == "left" {
                // 左の被演算子が単純な名前 (`c >= ..`)。
                let pat = format!(r"^{}\s*{}\s", id, op);
                if !Regex::new(&pat).unwrap().is_match(rest) {
                    return vec![];
                }
                b0
            } else {
                // 右の被演算子が単純な名前 (`.. >= n`)。左は空白を含まない一語に限る。
                let pat = format!(r"^(\S+\s*{}\s*){}\b([^.(\w\[']|$)", op, id);
                let Some(m) = Regex::new(&pat).unwrap().captures(rest) else { return vec![] };
                b0 + m[1].len()
            };
            vec![span(file, line, text, at, at, "*".to_string())]
        }
        "E2053" => {
            // 冗長な型の注釈 (D17): `let x: T = e` の `: T ` を外す (`let x = e`)。1 行の `let` に限る。
            let Some(text) = get(line) else { return vec![] };
            let re = Regex::new(r"^\s*let\s+(?:mut\s+)?[^:=]+(:\s*[^=]+\s)=").unwrap();
            let Some(c) = re.captures(text) else { return vec![] };
            let g = c.get(1).unwrap();
            vec![span(file, line, text, g.start(), g.end(), " ".to_string())]
        }
        "E2041" => {
            let Some(text) = get(line) else { return vec![] };
            let indent = text.len() - text.trim_start().len();
            let Some(expr) = text.trim_start().strip_prefix("let _ = ") else { return vec![] };
            if expr.ends_with(['(', ',', '{']) {
                return vec![];
            }
            vec![span(file, line, text, indent, text.len(), format!("core.mem.drop({})", expr))]
        }
        "E2042" => {
            let Some(text) = get(line) else { return vec![] };
            let indent = text.len() - text.trim_start().len();
            let stmt = text.trim_start();
            if stmt.is_empty() || stmt.ends_with(['(', ',', '{']) || stmt.starts_with("//") {
                return vec![];
            }
            vec![span(file, line, text, indent, text.len(), format!("core.mem.drop({})", stmt))]
        }
        "E1020" => {
            let re = Regex::new(r"^unused (parameter|variable) ([a-z][a-z0-9_]*)$").unwrap();
            let Some(c) = re.captures(msg) else { return vec![] };
            let (kind, name) = (&c[1], &c[2]);
            if kind == "parameter" {
                let Some(text) = get(line) else { return vec![] };
                let re = Regex::new(&format!(r"\b{}:", regex::escape(name))).unwrap();
                let Some(m) = re.find(text) else { return vec![] };
                return vec![span(file, line, text, m.start(), m.start() + name.len(), format!("_{}", name))];
            }
            let re = Regex::new(&format!(r"\b(?:let|with) (mut )?({})\b", regex::escape(name))).unwrap();
            let mut n = line;
            while let Some(text) = get(n) {
                if n > line && text.starts_with("end ") {
                    break;
                }
                if let Some(c) = re.captures(text) {
                    let m = c.get(2).unwrap();
                    return vec![span(file, n, text, m.start(), m.end(), format!("_{}", name))];
                }
                n += 1;
            }
            vec![]
        }
        // 効果の完全一致 (D5・D13、tast_rules/effects_exact.rs): 宣言を、メッセージの末尾の ``write `uses [..] panics ..` `` に書き直す。
        "E4025" | "E4020" => {
            let Some(text) = get(line) else { return vec![] };
            let Some(want) = msg.rsplit_once("write `").and_then(|(_, r)| r.strip_suffix('`')) else { return vec![] };
            if code == "E4025" {
                let re = Regex::new(r"uses \[[^\]]*\] panics [A-Za-z]+").unwrap();
                let Some(m) = re.find(text) else { return vec![] };
                return vec![span(file, line, text, m.start(), m.end(), want.to_string())];
            }
            // 省略: `stack` / `impls` / `pub` の前 (無ければ行末) に ` uses [..] panics ..` を足す。
            let t = text.trim_end();
            let mut at = t.len();
            for pat in [" stack ", " impls "] {
                if let Some(p) = t.find(pat) {
                    at = at.min(p);
                }
            }
            if at == t.len() && t.ends_with(" pub") {
                at = t.len() - 4;
            }
            vec![span(file, line, text, at, at, format!(" {}", want))]
        }
        _ => vec![],
    }
}

/// 同じファイルの修正を適用する。重なるものは後のものを捨てる。適用した件数を返す。
pub fn apply(text: &str, fixes: &[Fix]) -> (String, usize) {
    let lines: Vec<&str> = text.split('\n').collect();
    // 行頭からの文字位置を、テキスト全体のバイト位置へ変換する。
    let mut starts = vec![0usize];
    for l in &lines {
        starts.push(starts.last().unwrap() + l.len() + 1);
    }
    let to_byte = |line: usize, col: usize| -> Option<usize> {
        let l = lines.get(line.checked_sub(1)?)?;
        let mut idx = l.len();
        for (n, (b, _)) in l.char_indices().enumerate() {
            if n + 1 == col {
                idx = b;
                break;
            }
        }
        if col == l.chars().count() + 1 {
            idx = l.len();
        }
        Some(starts[line - 1] + idx)
    };
    let mut spans: Vec<(usize, usize, &str)> = fixes
        .iter()
        .filter_map(|f| Some((to_byte(f.line, f.col)?, to_byte(f.end_line, f.end_col)?, f.replace.as_str())))
        .collect();
    spans.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    let mut out = String::new();
    let mut pos = 0;
    let mut n = 0;
    for (s, e, r) in spans {
        if s < pos || e < s || e > text.len() {
            continue;
        }
        out.push_str(&text[pos..s]);
        out.push_str(r);
        pos = e;
        n += 1;
    }
    out.push_str(&text[pos..]);
    (out, n)
}

/// `T.m(&x, a, b)` / `m.f(x)` (位置 `line`:`col` から始まる呼び出し) を `x.m(a, b)` にする修正 (D8)。受け手は先頭の `&` `&mut ` を外す。
/// `*r` や演算子を含む受け手は括弧で包む (`(*r).m()` は `ksn fmt` が `r.m()` にする)。複数行の呼び出しも書き換える。
fn method_call_fix(file: &str, line: usize, col: usize, lines: &[String]) -> Vec<Fix> {
    let mut cs: Vec<(char, usize, usize)> = vec![];
    for (k, l) in lines.iter().enumerate().skip(line.saturating_sub(1)) {
        for (i, ch) in l.chars().enumerate() {
            if k + 1 == line && i + 1 < col {
                continue;
            }
            cs.push((ch, k + 1, i + 1));
        }
        cs.push(('\n', k + 1, l.chars().count() + 1));
        if cs.len() > 6000 {
            break;
        }
    }
    let mut i = 0;
    while i < cs.len() && (cs[i].0.is_ascii_alphanumeric() || cs[i].0 == '_' || cs[i].0 == '.') {
        i += 1;
    }
    if i == 0 || i >= cs.len() || cs[i].0 != '(' {
        return vec![];
    }
    let path: String = cs[..i].iter().map(|c| c.0).collect();
    let Some(m) = path.rsplit('.').next().filter(|m| !m.is_empty() && path.contains('.')) else { return vec![] };
    let open = i;
    // 引数の区切り (深さ 1 のカンマ) と閉じ括弧を探す。文字列・文字リテラルの中は飛ばす。
    let mut depth = 0i32;
    let mut commas: Vec<usize> = vec![];
    let mut close = None;
    let mut j = open;
    while j < cs.len() {
        let ch = cs[j].0;
        match ch {
            '"' => {
                j += 1;
                while j < cs.len() && cs[j].0 != '"' {
                    if cs[j].0 == '\\' {
                        j += 1;
                    }
                    j += 1;
                }
            }
            '\'' => {
                if cs.get(j + 1).map_or(false, |c| c.0 == '\\') {
                    j += 2;
                    while j < cs.len() && cs[j].0 != '\'' {
                        j += 1;
                    }
                } else if cs.get(j + 2).map_or(false, |c| c.0 == '\'') {
                    j += 2;
                }
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(j);
                    break;
                }
            }
            ',' if depth == 1 => commas.push(j),
            _ => {}
        }
        j += 1;
    }
    let Some(close) = close else { return vec![] };
    let txt = |a: usize, b: usize| -> String { cs[a..b].iter().map(|c| c.0).collect() };
    let first_end = commas.first().copied().unwrap_or(close);
    let a0 = txt(open + 1, first_end);
    let a0 = a0.trim();
    if a0.is_empty() {
        return vec![];
    }
    let recv = a0.strip_prefix("&mut ").or_else(|| a0.strip_prefix('&')).unwrap_or(a0).trim_start().to_string();
    let needs = {
        let mut d = 0i32;
        let mut in_str = false;
        let mut flag = recv.starts_with(|c| matches!(c, '*' | '-' | '!' | '&'));
        let rc: Vec<char> = recv.chars().collect();
        let mut k = 0;
        while k < rc.len() {
            let c = rc[k];
            if in_str {
                if c == '\\' {
                    k += 1;
                } else if c == '"' {
                    in_str = false;
                }
            } else if c == '"' {
                in_str = true;
            } else if matches!(c, '(' | '[' | '{') {
                d += 1;
            } else if matches!(c, ')' | ']' | '}') {
                d -= 1;
            } else if d == 0 && (matches!(c, '+' | '*' | '/' | '%' | '|' | '^' | '<' | '>' | '=') || (c == ' ' && rc.get(k + 1) == Some(&'i') && rc.get(k + 2) == Some(&'s'))) {
                flag = true;
            } else if d == 0 && matches!(c, '-' | '&' | '!') && k > 0 {
                flag = true;
            }
            k += 1;
        }
        flag
    };
    let rest = if commas.is_empty() { String::new() } else { txt(commas[0] + 1, close).trim_start_matches(' ').to_string() };
    let rest = rest.trim_end_matches(|c: char| c == ',').to_string();
    let recv = if needs { format!("({})", recv) } else { recv };
    let call = format!("{}.{}({})", recv, m, rest.trim_end());
    let (el, ec) = (cs[close].1, cs[close].2 + 1);
    vec![Fix { file: file.to_string(), line, col, end_line: el, end_col: ec, replace: call }]
}

/// `T { b: e2, a: e1 }` を宣言順 (メッセージの `: a, b` の並び) にする修正 (D16)。リテラルは 1 行 (折り返せない)。
fn struct_order_fix(file: &str, msg: &str, line: usize, col: usize, lines: &[String]) -> Vec<Fix> {
    let Some(order) = msg.rsplit_once(": ").map(|(_, o)| o.split(", ").map(String::from).collect::<Vec<String>>()) else { return vec![] };
    let Some(text) = lines.get(line.wrapping_sub(1)) else { return vec![] };
    let cs: Vec<char> = text.chars().collect();
    let Some(open) = (col.saturating_sub(1)..cs.len()).find(|&i| cs[i] == '{') else { return vec![] };
    // 欄 (深さ 1 のカンマ区切り) を集める。文字列・文字リテラルの中は飛ばす。
    let mut depth = 0i32;
    let mut items: Vec<(usize, usize)> = vec![];
    let mut from = open + 1;
    let mut close = None;
    let mut j = open;
    while j < cs.len() {
        match cs[j] {
            '"' => {
                j += 1;
                while j < cs.len() && cs[j] != '"' {
                    if cs[j] == '\\' {
                        j += 1;
                    }
                    j += 1;
                }
            }
            '\'' => {
                if cs.get(j + 1) == Some(&'\\') {
                    j += 2;
                    while j < cs.len() && cs[j] != '\'' {
                        j += 1;
                    }
                } else if cs.get(j + 2) == Some(&'\'') {
                    j += 2;
                }
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    items.push((from, j));
                    close = Some(j);
                    break;
                }
            }
            ',' if depth == 1 => {
                items.push((from, j));
                from = j + 1;
            }
            _ => {}
        }
        j += 1;
    }
    let Some(close) = close else { return vec![] };
    let mut by_name: Vec<(String, String)> = vec![];
    for (a, b) in items {
        let seg: String = cs[a..b].iter().collect();
        let seg = seg.trim().to_string();
        if seg.is_empty() {
            continue;
        }
        let Some((n, init)) = seg.split_once(':') else { return vec![] };
        // 副作用・panic がありうる式 (呼び出し・添字・`?`・ブロック・クロージャ) があれば直さない。
        let ic: Vec<char> = init.chars().collect();
        for (k, c) in ic.iter().enumerate() {
            let prev = if k > 0 { ic[k - 1] } else { ' ' };
            if matches!(c, '(' | '[') && (prev.is_alphanumeric() || matches!(prev, '_' | ')' | ']' | '>')) || matches!(c, '?' | '{') {
                return vec![];
            }
        }
        by_name.push((n.trim().to_string(), init.trim().to_string()));
    }
    let mut out: Vec<String> = vec![];
    for n in &order {
        let Some((_, init)) = by_name.iter().find(|(x, _)| x == n) else { return vec![] };
        out.push(format!("{}: {}", n, init));
    }
    if out.len() != by_name.len() {
        return vec![];
    }
    let new = format!("{{ {} }}", out.join(", "));
    vec![Fix { file: file.to_string(), line, col: open + 1, end_line: line, end_col: close + 2, replace: new }]
}
