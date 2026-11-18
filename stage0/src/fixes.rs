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
