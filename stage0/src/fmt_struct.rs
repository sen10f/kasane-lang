//! 構造体リテラル・構造体パターンの正規形 (2026-11-23 の決定。設計_書き方は1通り.md 12 章): いつも 1 行に 1 欄で、
//! 欄は改行で区切り `,` は書かない。`}` は `{` を書いた行の字下げに置く (`type` の本体・`match` の腕と同じ並び)。
//!
//! ```text
//! let p = t.Point {
//!     x: 1i32
//!     y: 2i32
//! }
//! ```
//! `ksn fmt` はここで旧い形 (`T { a: x, b: y }`・欄の末尾の `,`) を機械的に書き直す。

use crate::lexer::{self, Tok};

fn indent_of(l: &str) -> usize {
    l.chars().take_while(|c| *c == ' ').count()
}

fn lex(line: &str) -> Option<Vec<Tok>> {
    if line.trim_start().starts_with("//") {
        return None;
    }
    let toks = lexer::lex(&format!("{}\n", line)).ok()?;
    Some(toks.into_iter().filter(|t| t.kind != "nl" && t.kind != "eof").collect())
}

fn is_op(t: &Tok, s: &str) -> bool {
    t.kind == "op" && t.text == s
}

fn end_col(t: &Tok) -> usize {
    t.col - 1 + t.text.chars().count()
}

/// 1 行に閉じた構造体リテラル / パターン `Name { a: x, b: y }` の最初のもの (`{` と `}` のトークンの番号)。
fn first_inline(toks: &[Tok]) -> Option<(usize, usize)> {
    let first = toks.first()?;
    if first.kind == "kw" && matches!(first.text.as_str(), "fn" | "impl" | "trait" | "enum" | "type" | "extern" | "test") {
        return None;
    }
    for k in 1..toks.len().saturating_sub(2) {
        if !is_op(&toks[k], "{") || !matches!(toks[k - 1].kind, "upper") {
            continue;
        }
        if toks[k + 1].kind != "ident" || !is_op(&toks[k + 2], ":") {
            continue;
        }
        let mut depth = 0i32;
        for (m, t) in toks.iter().enumerate().skip(k) {
            if t.kind == "op" && matches!(t.text.as_str(), "{" | "(" | "[") {
                depth += 1;
            } else if t.kind == "op" && matches!(t.text.as_str(), "}" | ")" | "]") {
                depth -= 1;
                if depth == 0 {
                    return if is_op(t, "}") { Some((k, m)) } else { None };
                }
            }
        }
    }
    None
}

fn split_line(line: &str, out: &mut Vec<String>) {
    let Some(toks) = lex(line) else {
        out.push(line.to_string());
        return;
    };
    let Some((k, m)) = first_inline(&toks) else {
        out.push(line.to_string());
        return;
    };
    let chars: Vec<char> = line.chars().collect();
    let ind = indent_of(line);
    let pad = " ".repeat(ind);
    let open_end = end_col(&toks[k]);
    out.push(chars[..open_end].iter().collect::<String>().trim_end().to_string());
    // 深さ 0 の `,` で欄に分ける。
    let mut fields: Vec<(usize, usize)> = vec![];
    let mut start = k + 1;
    let mut depth = 0i32;
    for j in k + 1..m {
        let t = &toks[j];
        if t.kind == "op" && matches!(t.text.as_str(), "{" | "(" | "[") {
            depth += 1;
        } else if t.kind == "op" && matches!(t.text.as_str(), "}" | ")" | "]") {
            depth -= 1;
        } else if depth == 0 && is_op(t, ",") {
            if start < j {
                fields.push((start, j));
            }
            start = j + 1;
        }
    }
    if start < m {
        fields.push((start, m));
    }
    for (a, b) in fields {
        let text: String = chars[toks[a].col - 1..end_col(&toks[b - 1])].iter().collect();
        split_line(&format!("{}    {}", pad, text.trim()), out);
    }
    let rest: String = chars[toks[m].col - 1..].iter().collect();
    split_line(&format!("{}{}", pad, rest.trim_end()), out);
}

fn opens_literal(t: &str) -> bool {
    let toks = match lex(t) {
        Some(x) => x,
        None => return false,
    };
    let n = toks.len();
    let blockish = toks[0].kind == "kw" && matches!(toks[0].text.as_str(), "if" | "while" | "for" | "loop" | "match" | "else" | "fn" | "impl" | "trait" | "enum" | "type" | "unsafe" | "with" | "extern" | "test");
    n >= 2 && is_op(&toks[n - 1], "{") && toks[n - 2].kind == "upper" && !blockish
}

/// 旧い形の複数行 (欄の末尾の `,`) から `,` を落とす。
fn strip_field_commas(lines: &mut [String]) {
    let mut stack: Vec<(usize, bool)> = vec![];
    for l in lines.iter_mut() {
        let t = l.trim().to_string();
        if t.is_empty() || t.starts_with("//") {
            continue;
        }
        let ind = indent_of(l);
        while let Some(&(top, _)) = stack.last() {
            if ind < top + 4 && !(ind == top && t.starts_with('}')) {
                stack.pop();
            } else {
                break;
            }
        }
        if let Some(&(top, is_field)) = stack.last() {
            if ind == top && t.starts_with('}') {
                stack.pop();
                // 欄の値の構造体リテラルを閉じる `},` だけ `,` を落とす (呼び出しの引数の `},` は引数の区切り)。
                if is_field && t.ends_with(',') && !t.contains("=>") {
                    *l = l.trim_end().trim_end_matches(',').to_string();
                }
                continue;
            }
            if ind == top + 4 && t.ends_with(',') && !t.contains('"') {
                let bal = t.matches('(').count() == t.matches(')').count() && t.matches('[').count() == t.matches(']').count();
                if bal && t.matches('{').count() == t.matches('}').count() {
                    *l = l.trim_end().trim_end_matches(',').to_string();
                }
            }
        }
        if t.ends_with('{') && !t.starts_with('}') && opens_literal(&t) {
            let is_field = lex(&t).map_or(false, |k| k.len() > 2 && k[0].kind == "ident" && is_op(&k[1], ":"));
            stack.push((ind, is_field));
        }
    }
}

/// 構造体リテラル / パターンを正規形 (1 行 1 欄・`,` 無し) にする。変えたら true。
pub fn canon(lines: &mut Vec<String>) -> bool {
    let before = lines.clone();
    strip_field_commas(lines);
    let mut out: Vec<String> = vec![];
    for l in lines.iter() {
        split_line(l, &mut out);
    }
    *lines = out;
    *lines != before
}

/// 解析用: 複数行の構造体リテラル / パターンを旧い 1 行の形 (`T { a: x, b: y }`) に畳む。
/// 正規形を行ごとの正規表現で読む補助 (`lint.rs` の束縛・未使用の判定) が、欄の並びを 1 行で見られるようにする。
pub fn fold(text: &str) -> String {
    let lines: Vec<String> = text.split('\n').map(String::from).collect();
    fold_lines(&lines).join("\n")
}

fn fold_lines(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut i = 0;
    while i < lines.len() {
        let l = &lines[i];
        let t = l.trim().to_string();
        if t.ends_with('{') && !t.starts_with('}') && opens_literal(&t) {
            let ind = indent_of(l);
            if let Some(off) = lines[i + 1..].iter().position(|x| indent_of(x) == ind && x.trim_start().starts_with('}')) {
                let j = i + 1 + off;
                let inner = fold_lines(&lines[i + 1..j]);
                let mut fields: Vec<String> = vec![];
                for x in &inner {
                    let xt = x.trim();
                    if xt.is_empty() {
                        continue;
                    }
                    let starts_field = indent_of(x) == ind + 4
                        && xt.split_once(':').map_or(false, |(a, _)| !a.is_empty() && a.chars().all(|c| c.is_alphanumeric() || c == '_'));
                    if starts_field || fields.is_empty() {
                        fields.push(xt.to_string());
                    } else {
                        let last = fields.last_mut().unwrap();
                        if !(last.ends_with('(') || last.ends_with('[') || xt.starts_with(')') || xt.starts_with(']') || xt.starts_with('.')) {
                            last.push(' ');
                        }
                        last.push_str(xt);
                    }
                }
                out.push(format!("{}{} {} {}", " ".repeat(ind), t, fields.join(", "), lines[j].trim()));
                i = j + 1;
                continue;
            }
        }
        out.push(l.clone());
        i += 1;
    }
    out
}
