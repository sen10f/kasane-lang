//! 1 行の中のトークンの間隔 (設計_書き方は1通り.md の L14)。`ksn fmt` はこの表で間隔を直し、
//! `ksn check` は「`fmt` の出力と違う」ことで誤りにする (E0020)。間隔は 0 か 1 だけ。行頭の字下げは触らない。
//!
//! 表 (前のトークン a、次のトークン b。上から最初に当たったものを使う):
//! 1. b が `)` `]` `,` `;` `?` `.` `::` `:` → 0
//! 2. a が `(` `[` `.` `::`、前置の演算子 (`&` `*` `-` `!`)、型引数の `<` → 0 (`&mut` `*const` の `mut` `const` も 0。その後ろは 1)
//! 3. b が型引数の `<` `>` → 0
//! 4. a が `{` で b が `}` → 0。それ以外で a が `{`、b が `}` `{` → 1
//! 5. b が `(` `[`: a が呼び出し・添字の頭 (名前・`)` `]` `?` `>`(型引数の閉じ)・`self` `Self`) なら 0、
//!    `fn(` (クロージャ) も 0、他 (予約語・`,`・二項演算子・`derive` `capture` `requires`) は 1
//! 6. 他はすべて 1 (二項演算子・`->` `=>` `=` の両側、`,` `;` `:` の後ろ、語と語)
//!
//! 前置か二項かは、直前が値で終わるか (名前・リテラル・`)` `]` `}` `?` `self` など) で決める。
//! `<` が型引数なのは、直前が型名 (UpperCamelCase、または次が型の始まりである 1 文字の大文字) か `::` のとき (式の中の `<` は比較)。

use crate::lexer::{self, Tok};

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Normal,
    Prefix,
    GenOpen,
    GenClose,
}

fn is_value_end(t: &Tok, role: Role) -> bool {
    match t.kind {
        "ident" | "upper" | "prim" | "int" | "float" | "str" | "char" | "byte" | "bstr" => true,
        "kw" => matches!(t.text.as_str(), "self" | "Self" | "true" | "false" | "never"),
        "op" => role == Role::GenClose || matches!(t.text.as_str(), ")" | "]" | "}" | "?"),
        _ => false,
    }
}

fn is_call_head(t: &Tok, role: Role, after_dot: bool, next_is_bracket: bool) -> bool {
    match t.kind {
        "ident" => after_dot || !next_is_bracket || !matches!(t.text.as_str(), "derive" | "capture" | "requires"),
        "upper" | "prim" => true,
        "kw" => matches!(t.text.as_str(), "self" | "Self" | "impl"),
        "op" => role == Role::GenClose || matches!(t.text.as_str(), ")" | "]" | "?"),
        _ => false,
    }
}

fn is_op(t: &Tok, s: &str) -> bool {
    t.kind == "op" && t.text == s
}

/// `<` の直前が型名か。UpperCamelCase (小文字を含む) は型名。1 文字の大文字 (`H<'a>`) は、`<` の次が型の始まり
/// (ライフタイム・型名・基本型・`&` `*` `[` `(` `dyn` `fn`) なら型名、値 (数字・小文字の名前) なら定数 (比較)。
/// 2 文字以上の全大文字 (`MAX`) は定数。
fn type_name(t: &Tok, next: Option<&Tok>) -> bool {
    if t.kind != "upper" {
        return false;
    }
    if t.text.chars().any(|c| c.is_ascii_lowercase()) {
        return true;
    }
    t.text.chars().count() == 1
        && next.map_or(false, |n| match n.kind {
            "life" | "upper" | "prim" => true,
            "kw" => matches!(n.text.as_str(), "dyn" | "fn" | "never"),
            "op" => matches!(n.text.as_str(), "&" | "&&" | "*" | "[" | "("),
            _ => false,
        })
}

/// 行頭のトークンが前置ではありえない演算子か (折り返した行の頭の二項演算子)。
fn binary_only(t: &Tok) -> bool {
    t.kind == "op" && matches!(t.text.as_str(), "&&" | "||" | "|" | "^" | "+" | "/" | "%" | "==" | "!=" | "<" | ">" | "<=" | ">=" | "=" | "=>" | "->")
}

fn roles(toks: &[Tok]) -> Vec<Role> {
    let mut roles = vec![Role::Normal; toks.len()];
    let mut depth = 0usize;
    for i in 0..toks.len() {
        let t = &toks[i];
        if t.kind != "op" {
            continue;
        }
        let prev = if i > 0 { Some((&toks[i - 1], roles[i - 1])) } else { None };
        match t.text.as_str() {
            "<" => {
                if prev.map_or(false, |(p, _)| type_name(p, toks.get(i + 1)) || is_op(p, "::")) {
                    roles[i] = Role::GenOpen;
                    depth += 1;
                }
            }
            ">" => {
                if depth > 0 {
                    roles[i] = Role::GenClose;
                    depth -= 1;
                }
            }
            "!" => roles[i] = Role::Prefix,
            "-" | "*" | "&" | "&&" => {
                let binary = match prev {
                    Some((p, r)) => is_value_end(p, r),
                    None => t.text == "&&" && binary_only(t),
                };
                if !binary {
                    roles[i] = Role::Prefix;
                }
            }
            _ => {}
        }
    }
    roles
}

fn gap(toks: &[Tok], roles: &[Role], i: usize) -> usize {
    let (a, b) = (&toks[i], &toks[i + 1]);
    let (ra, rb) = (roles[i], roles[i + 1]);
    if b.kind == "op" && matches!(b.text.as_str(), ")" | "]" | "," | ";" | "?" | "." | "::" | ":") {
        return 0;
    }
    if a.kind == "op" && matches!(a.text.as_str(), "(" | "[" | "." | "::") {
        return 0;
    }
    if ra == Role::Prefix && a.text == "&" && b.kind == "op" && b.text == "&" {
        // `& &'a u32`: 続けて書くと 1 つの `&&` になってしまう (別のトークン) ので、間を 1 つ空ける。
        return 1;
    }
    if ra == Role::Prefix {
        // `-> ! uses []` のような、型の `!` の後ろの予約語。
        return if b.kind == "kw" && matches!(b.text.as_str(), "uses" | "panics" | "pub") { 1 } else { 0 };
    }
    if ra == Role::GenOpen || rb == Role::GenOpen || rb == Role::GenClose {
        return 0;
    }
    if is_op(a, "{") && is_op(b, "}") {
        return 0;
    }
    if is_op(a, "{") || is_op(b, "}") || is_op(b, "{") {
        return 1;
    }
    if is_op(b, "(") || is_op(b, "[") {
        if is_call_head(a, ra, i > 0 && is_op(&toks[i - 1], "."), is_op(b, "[")) || (a.kind == "kw" && a.text == "fn" && is_op(b, "(")) {
            return 0;
        }
        return 1;
    }
    1
}

/// 1 行 (字下げを含む) のトークンの間隔を表のとおりにする。字句に失敗する行・コメント行はそのまま返す。
pub fn canon_line(line: &str) -> String {
    if line.trim_start().starts_with("//") {
        return line.to_string();
    }
    let Ok(all) = lexer::lex(&format!("{}\n", line)) else { return line.to_string() };
    let toks: Vec<Tok> = all.into_iter().filter(|t| !matches!(t.kind, "nl" | "eof")).collect();
    if toks.len() < 2 {
        return line.to_string();
    }
    let rs = roles(&toks);
    let chars: Vec<char> = line.chars().collect();
    // 右から直す (左の位置がずれない)。
    let mut edits: Vec<(usize, usize, usize)> = vec![];
    for i in 0..toks.len() - 1 {
        let next = toks[i + 1].col - 1;
        let mut start = next;
        while start > 0 && chars[start - 1] == ' ' {
            start -= 1;
        }
        // 同じ行で前のトークンの終わりより手前には戻らない (字下げの先頭は対象外)。
        if toks[i + 1].line != toks[i].line || start < toks[i].col {
            continue;
        }
        let want = gap(&toks, &rs, i);
        if next - start != want {
            edits.push((start, next, want));
        }
    }
    if edits.is_empty() {
        return line.to_string();
    }
    let mut out = chars;
    for (s, e, w) in edits.into_iter().rev() {
        out.splice(s..e, std::iter::repeat(' ').take(w));
    }
    out.into_iter().collect()
}
