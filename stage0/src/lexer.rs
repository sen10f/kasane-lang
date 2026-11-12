//! Kasane 字句解析 (stage0)。Python 参照実装 ksn/syntax.py の lex と同じトークン列を出す。

#[derive(Debug, Clone, PartialEq)]
pub struct Tok {
    pub kind: &'static str,
    pub text: String,
    pub line: usize,
    pub col: usize,
}

impl Tok {
    /// AST ノード用の符号化済み位置 (行と列)。
    pub fn pos(&self) -> usize {
        crate::parser::pack_pos(self.line, self.col)
    }
}

const PRIMS: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64",
    "bool", "char", "str",
];
const KEYWORDS: &[&str] = &[
    "fn", "type", "enum", "trait", "impl", "const", "static", "extern", "test", "end", "import", "let", "mut",
    "if", "else", "match", "loop", "while", "for", "in", "break", "continue", "return", "true", "false", "self",
    "Self", "pub", "uses", "panics", "unsafe", "move",
    "dyn", "blanket", "never", "may", "async", "await", "where", "ref", "as",
];
const OPS2: &[&str] = &["=>", "->", "==", "!=", "<=", ">=", "&&", "||", "::"];
const SUFFIXES: &[&str] = &[
    "i128", "u128", "isize", "usize", "i16", "i32", "i64", "u16", "u32", "u64", "i8", "u8", "f32", "f64",
];

fn lit_start_ok(toks: &[Tok]) -> bool {
    match toks.last() {
        None => true,
        Some(t) => match t.kind {
            "ident" | "upper" | "int" | "float" | "str" | "char" | "prim" | "byte" | "bstr" => false,
            "kw" => !matches!(t.text.as_str(), "self" | "true" | "false"),
            "op" => !matches!(t.text.as_str(), ")" | "]" | "}"),
            _ => true,
        },
    }
}

fn is_hex(c: char) -> bool {
    c.is_ascii_hexdigit()
}

pub fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let mut toks: Vec<Tok> = Vec::new();
    let lines: Vec<&str> = src.split('\n').collect();
    let n_lines = lines.len();
    for (idx, text) in lines.iter().enumerate() {
        let ln = idx + 1;
        if ln == n_lines && text.is_empty() {
            break;
        }
        let stripped = text.trim_start_matches(' ');
        if stripped.starts_with("//") || stripped.is_empty() {
            continue;
        }
        if stripped.starts_with('.') && !stripped.starts_with("..") {
            if matches!(toks.last(), Some(t) if t.kind == "nl") {
                toks.pop();
            }
        }
        let chars: Vec<char> = text.chars().collect();
        let l = chars.len();
        let mut i = l - stripped.chars().count();
        while i < l {
            let c = chars[i];
            let col = i + 1;
            if c == ' ' {
                i += 1;
                continue;
            }
            let is_b = c == 'b' && i + 1 < l && (chars[i + 1] == '"' || chars[i + 1] == '\'');
            if c == '"' || (is_b && chars[i + 1] == '"') {
                let mut j = i + if is_b { 2 } else { 1 };
                while j < l && chars[j] != '"' {
                    if chars[j] == '\\' {
                        check_escape(&chars, j, ln)?;
                        j += 2;
                    } else {
                        j += 1;
                    }
                }
                if j >= l {
                    return Err(format!("E0001|{}|{}|unterminated string", ln, col));
                }
                let t: String = chars[i..=j].iter().collect();
                toks.push(Tok { kind: if is_b { "bstr" } else { "str" }, text: t, line: ln, col });
                i = j + 1;
                continue;
            }
            if c == '\'' || (is_b && chars[i + 1] == '\'') {
                let j = i + if is_b { 2 } else { 1 };
                if j < l && chars[j] == '\\' {
                    check_escape(&chars, j, ln)?;
                }
                let k = if j < l && chars[j] == '\\' {
                    if j + 1 < l && chars[j + 1] == 'x' {
                        j + 4
                    } else if j + 1 < l && chars[j + 1] == 'u' {
                        chars[j..].iter().position(|&x| x == '}').map(|p| j + p + 1).unwrap_or(l)
                    } else {
                        j + 2
                    }
                } else {
                    j + 1
                };
                if k < l && chars[k] == '\'' && j < l {
                    let t: String = chars[i..=k].iter().collect();
                    toks.push(Tok { kind: if is_b { "byte" } else { "char" }, text: t, line: ln, col });
                    i = k + 1;
                    continue;
                }
                let mut m = i + 1;
                while m < l && (chars[m].is_ascii_lowercase() || chars[m].is_ascii_digit() || chars[m] == '_') {
                    m += 1;
                }
                if m > i + 1 && chars[i + 1].is_ascii_lowercase() {
                    toks.push(Tok { kind: "life", text: chars[i..m].iter().collect(), line: ln, col });
                    i = m;
                    continue;
                }
                return Err(format!("E0001|{}|{}|bad char literal", ln, col));
            }
            if c.is_ascii_digit()
                || (c == '-' && i + 1 < l && chars[i + 1].is_ascii_digit() && lit_start_ok(&toks))
            {
                let mut j = i + if c == '-' { 1 } else { 0 };
                let pre: String = chars[j..(j + 2).min(l)].iter().collect();
                let mut is_float = false;
                if pre == "0x" {
                    j += 2;
                    while j < l && (is_hex(chars[j]) || chars[j] == '_') {
                        j += 1;
                    }
                } else if pre == "0b" || pre == "0o" {
                    j += 2;
                    while j < l && (chars[j].is_ascii_digit() || chars[j] == '_') {
                        j += 1;
                    }
                } else {
                    while j < l && (chars[j].is_ascii_digit() || chars[j] == '_') {
                        j += 1;
                    }
                    if j + 1 < l && chars[j] == '.' && chars[j + 1].is_ascii_digit() {
                        is_float = true;
                        j += 1;
                        while j < l && chars[j].is_ascii_digit() {
                            j += 1;
                        }
                    }
                    if j < l && chars[j] == 'e' {
                        let mut k = j + 1;
                        if k < l && (chars[k] == '-' || chars[k] == '+') {
                            k += 1;
                        }
                        if k < l && chars[k].is_ascii_digit() {
                            is_float = true;
                            while k < l && chars[k].is_ascii_digit() {
                                k += 1;
                            }
                            j = k;
                        }
                    }
                }
                let rest: String = chars[j..].iter().collect();
                for s in SUFFIXES {
                    if rest.starts_with(s) {
                        let nx = rest[s.len()..].chars().next();
                        if !matches!(nx, Some(x) if x.is_alphanumeric() || x == '_') {
                            if s.starts_with('f') {
                                is_float = true;
                            }
                            j += s.len();
                            break;
                        }
                    }
                }
                let t: String = chars[i..j].iter().collect();
                toks.push(Tok { kind: if is_float { "float" } else { "int" }, text: t, line: ln, col });
                i = j;
                continue;
            }
            if c.is_ascii_alphabetic() || c == '_' {
                let mut j = i;
                while j < l && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                let w: String = chars[i..j].iter().collect();
                let kind = if KEYWORDS.contains(&w.as_str()) {
                    "kw"
                } else if PRIMS.contains(&w.as_str()) {
                    "prim"
                } else if w.chars().next().unwrap().is_ascii_uppercase() {
                    "upper"
                } else {
                    "ident"
                };
                toks.push(Tok { kind, text: w, line: ln, col });
                i = j;
                continue;
            }
            if i + 1 < l {
                let two: String = chars[i..i + 2].iter().collect();
                if OPS2.contains(&two.as_str()) {
                    toks.push(Tok { kind: "op", text: two, line: ln, col });
                    i += 2;
                    continue;
                }
            }
            if "()[]{},.:;=<>+-*/%&|^!?".contains(c) {
                toks.push(Tok { kind: "op", text: c.to_string(), line: ln, col });
                i += 1;
                continue;
            }
            return Err(format!("E0001|{}|{}|unexpected character {}", ln, col, crate::grammar::py_repr(&c.to_string())));
        }
        if matches!(toks.last(), Some(t) if t.kind == "op" && matches!(t.text.as_str(), "(" | "[" | ",") && t.line == ln)
        {
            continue;
        }
        toks.push(Tok { kind: "nl", text: "\n".into(), line: ln, col: l + 1 });
    }
    toks.push(Tok { kind: "eof", text: String::new(), line: n_lines, col: 1 });
    Ok(toks)
}

fn check_escape(chars: &[char], j: usize, ln: usize) -> Result<(), String> {
    match chars.get(j + 1) {
        Some(c) if matches!(*c, 'n' | 't' | 'r' | '\\' | '0' | '\'' | '"' | 'x' | 'u') => Ok(()),
        Some(c) => Err(format!("E0001|{}|1|bad escape \\{}", ln, c)),
        None => Err(format!("E0001|{}|1|unterminated string", ln)),
    }
}

/// ヘッダ位置でのみ特別な意味を持つ語。識別子としても使える (文脈キーワード)。
pub fn is_contextual(w: &str) -> bool {
    matches!(w, "stack" | "impls" | "derive" | "linear" | "layout" | "requires" | "capture" | "link")
}
