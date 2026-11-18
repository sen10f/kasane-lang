//! 字句の「書き方は 1 通り」の規則 (設計_書き方は1通り.md の L1〜L4・L16、G38)。
//!
//! - ファイルのどこにも (文字列・コメント・`///` を含めて) 書けない文字 (E0004)。
//! - 文字列・文字のリテラルのエスケープは名前のあるもの・構文上要るもの・書けない文字の `\u{..}` だけ (E0005)。
//! - 文字列・コメントは NFC (E0006)。
//! - 不正な UTF-8 (E0007)。
//!
//! `ksn doc` / `ksn get` の出力では、書けない文字を `<U+202E>` の形に直して表示する (`visible`)。

use unicode_general_category::{get_general_category, GeneralCategory as G};
use unicode_normalization::{is_nfc, UnicodeNormalization};

/// 見つかった字句の違反。
pub struct Finding {
    pub code: &'static str,
    pub line: usize,
    pub col: usize,
    pub msg: String,
}

/// ソースのどこにもそのままの形では書けない文字か。
/// LF は行の区切りなので含めない。タブ・CR は既存の E0001 の担当なのでここでも含めない (`forbidden_in_literal` では含める)。
pub fn forbidden(c: char) -> bool {
    if matches!(c, '\n' | ' ' | '\t' | '\r') {
        return false;
    }
    matches!(
        get_general_category(c),
        G::Control
            | G::Format
            | G::LineSeparator
            | G::ParagraphSeparator
            | G::PrivateUse
            | G::Unassigned
            | G::SpaceSeparator
            | G::Surrogate
    )
}

/// リテラルの中で `\u{..}` で書く文字か (そのまま書けない文字。ただし名前のある 4 つは名前で書く)。
fn needs_u_escape(c: char) -> bool {
    !matches!(c, '\n' | '\t' | '\r' | '\0') && (forbidden(c) || c == '\x0b' || c == '\x0c')
}

fn what(c: char) -> &'static str {
    match c {
        '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}' => "text-direction control character",
        '\u{feff}' => "BOM / zero width no-break space",
        _ => match get_general_category(c) {
            G::Control => "control character",
            G::Format => "invisible format character",
            G::LineSeparator | G::ParagraphSeparator => "line/paragraph separator",
            G::PrivateUse => "private-use character",
            G::Unassigned => "unassigned code point",
            G::SpaceSeparator => "non-ASCII space",
            _ => "forbidden character",
        },
    }
}

/// 表示用: 書けない文字 (と、タブ・CR・LF 以外の制御文字) を `<U+202E>` の形にする。LF とタブは残す。
pub fn visible(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if c != '\n' && c != '\t' && (forbidden(c) || c == '\r') {
            o.push_str(&format!("<U+{:04X}>", c as u32));
        } else {
            o.push(c);
        }
    }
    o
}

/// ファイル全体の文字の検査 (E0004 と E0006)。位置は行・列 (1 始まり、Unicode スカラ値)。
pub fn scan(text: &str) -> Vec<Finding> {
    let mut out = vec![];
    for (i, line) in text.split('\n').enumerate() {
        let ln = i + 1;
        let mut n = 0;
        for (k, c) in line.chars().enumerate() {
            if forbidden(c) {
                n += 1;
                if n > 3 {
                    break;
                }
                let msg = if c == '\u{feff}' && ln == 1 && k == 0 {
                    "BOM is not allowed (U+FEFF)".to_string()
                } else {
                    format!("{} U+{:04X} is not allowed in source; write it as an escape `\\u{{{:x}}}` in a literal", what(c), c as u32, c as u32)
                };
                out.push(Finding { code: "E0004", line: ln, col: k + 1, msg });
            }
        }
        if !line.is_ascii() && !is_nfc(line) {
            let nfc: String = line.nfc().collect();
            let col = line.chars().zip(nfc.chars()).take_while(|(a, b)| a == b).count() + 1;
            out.push(Finding { code: "E0006", line: ln, col, msg: "text is not in Unicode normalization form NFC".to_string() });
        }
    }
    out
}

/// NFC に直す修正 (行 `line` の列 `col` から行末まで) の置換文字列。
pub fn nfc_fix(line: &str, col: usize) -> Option<String> {
    let nfc: String = line.nfc().collect();
    if nfc == line {
        return None;
    }
    Some(nfc.chars().skip(col - 1).collect())
}

/// リテラルの種類。
#[derive(Clone, Copy, PartialEq)]
pub enum Lit {
    Str,
    Char,
    BStr,
    BChar,
}

impl Lit {
    pub fn tag(self) -> &'static str {
        match self {
            Lit::Str => "str",
            Lit::Char => "char",
            Lit::BStr => "bstr",
            Lit::BChar => "bchar",
        }
    }
    pub fn of_tag(s: &str) -> Option<Lit> {
        Some(match s {
            "str" => Lit::Str,
            "char" => Lit::Char,
            "bstr" => Lit::BStr,
            "bchar" => Lit::BChar,
            _ => return None,
        })
    }
    fn bytes(self) -> bool {
        matches!(self, Lit::BStr | Lit::BChar)
    }
    fn quote(self) -> char {
        if matches!(self, Lit::Str | Lit::BStr) {
            '"'
        } else {
            '\''
        }
    }
}

/// 値 `v` を表す唯一の書き方 (エスケープまたはそのままの文字)。
pub fn canon_escape(v: u32, lit: Lit) -> Option<String> {
    let c = char::from_u32(v)?;
    Some(match c {
        '\n' => "\\n".into(),
        '\t' => "\\t".into(),
        '\r' => "\\r".into(),
        '\0' => "\\0".into(),
        '\\' => "\\\\".into(),
        c if c == lit.quote() => format!("\\{}", c),
        c if lit.bytes() => {
            if (0x20..0x7f).contains(&v) {
                c.to_string()
            } else {
                format!("\\x{:02x}", v)
            }
        }
        c if needs_u_escape(c) => format!("\\u{{{:x}}}", v),
        c => c.to_string(),
    })
}

/// 位置 `j` (`chars[j] == '\\'`) のエスケープの検査。誤りなら `(コード, 文面)`。正しければ長さ (文字数)。
pub fn check_escape(chars: &[char], j: usize, lit: Lit) -> Result<usize, (&'static str, String)> {
    let tag = lit.tag();
    let Some(&c) = chars.get(j + 1) else { return Err(("E0001", "unterminated string".into())) };
    match c {
        'n' | 't' | 'r' | '0' | '\\' => Ok(2),
        '"' | '\'' => {
            if c == lit.quote() {
                Ok(2)
            } else {
                Err(("E0005", format!("[{}] `\\{}` is not needed here; write `{}` as it is", tag, c, c)))
            }
        }
        'x' => {
            let h: String = chars.get(j + 2..j + 4).map(|s| s.iter().collect()).unwrap_or_default();
            if h.len() != 2 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(("E0001", "bad escape \\x".into()));
            }
            let v = u32::from_str_radix(&h, 16).unwrap();
            if !lit.bytes() {
                return Err(("E0005", format!("[{}] `\\x{}` is not allowed; write the character itself, or `\\u{{..}}` for a character that cannot be written", tag, h)));
            }
            let want = canon_escape(v, lit).unwrap();
            if want == format!("\\x{}", h) {
                Ok(4)
            } else {
                Err(("E0005", format!("[{}] `\\x{}` has one canonical spelling: `{}`", tag, h, want)))
            }
        }
        'u' => {
            if lit.bytes() {
                return Err(("E0001", "bad escape \\u (not allowed in a byte literal)".into()));
            }
            if chars.get(j + 2) != Some(&'{') {
                return Err(("E0001", "bad escape \\u".into()));
            }
            let Some(p) = chars[j..].iter().position(|&x| x == '}') else { return Err(("E0001", "bad escape \\u".into())) };
            let h: String = chars[j + 3..j + p].iter().collect();
            let len = p + 1;
            if h.is_empty() || h.len() > 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(("E0001", "bad escape \\u".into()));
            }
            let Some(ch) = u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) else {
                return Err(("E0001", format!("invalid \\u escape \\u{{{}}}", h)));
            };
            let want = canon_escape(ch as u32, lit).unwrap();
            if want == chars[j..j + len].iter().collect::<String>() {
                Ok(len)
            } else {
                Err(("E0005", format!("[{}] `\\u{{{}}}` has one canonical spelling: `{}`", tag, h, want)))
            }
        }
        _ => Err(("E0001", format!("bad escape \\{}", c))),
    }
}

/// E0005 の修正: 行 `line` の列 `col` (バックスラッシュの位置) から始まるエスケープの (文字数, 置換)。
pub fn escape_fix(line: &str, col: usize, lit: Lit) -> Option<(usize, String)> {
    let chars: Vec<char> = line.chars().collect();
    let j = col.checked_sub(1)?;
    if chars.get(j) != Some(&'\\') {
        return None;
    }
    let (len, v) = match chars.get(j + 1)? {
        'x' => (4, u32::from_str_radix(&chars.get(j + 2..j + 4)?.iter().collect::<String>(), 16).ok()?),
        'u' => {
            let p = chars[j..].iter().position(|&x| x == '}')?;
            (p + 1, u32::from_str_radix(&chars[j + 3..j + p].iter().collect::<String>(), 16).ok()?)
        }
        c @ ('"' | '\'') => (2, *c as u32),
        _ => return None,
    };
    Some((len, canon_escape(v, lit)?))
}
