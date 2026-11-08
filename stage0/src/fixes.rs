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
        "E0045" => {
            let Some(text) = get(line) else { return vec![] };
            let re = Regex::new(r"^(\s*)(.+?)\s*([-+*/%&|^])=\s*(.+?)\s*$").unwrap();
            let Some(c) = re.captures(text) else { return vec![] };
            let new = format!("{}{} = {} {} {}", &c[1], &c[2], &c[2], &c[3], &c[4]);
            vec![span(file, line, text, 0, text.len(), new)]
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
            let re = Regex::new(&format!(r"\blet (mut )?({})\b", regex::escape(name))).unwrap();
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
