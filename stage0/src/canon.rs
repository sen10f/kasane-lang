//! 書き方は 1 通り (設計_書き方は1通り.md の段 3)。`ksn fmt` が直す、1 行の中の数値リテラルの正規形 (D18)。
//! `ksn check` は「`fmt` の出力と違う」ことで誤りにする (E0020)。
//!
//! - 整数: 10 進の先頭の 0 を落とす。`0x` `0b` `0o` の後の桁揃えの 0 も落とす (全部 0 なら `0`)。10 進の桁区切りは
//!   5 桁以上で 3 桁ごとに必須、4 桁以下は付けない。基数 (`0x` `0b` `0o`) は残す。
//! - 浮動小数: 絶対値が `1e-4` 以上 `1e16` 未満は `d.d` (指数なし。末尾の余分な 0 は落とし、小数部は 1 桁以上)、
//!   それ以外は `d.de-N` / `d.deN` (仮数は 1 以上 10 未満。`e` の後ろに `+` は付けない)。

use crate::lexer::{self, Tok};

const INT_SUFFIXES: &[&str] = &["i128", "u128", "isize", "usize", "i16", "i32", "i64", "u16", "u32", "u64", "i8", "u8"];

fn split_suffix<'a>(text: &'a str, sufs: &[&'a str], radix: bool) -> (&'a str, &'a str) {
    for s in sufs {
        if let Some(b) = text.strip_suffix(s) {
            // 16 進の桁に `f32` のような綴りが混ざるのを接尾辞と取り違えない (字句解析器と同じく、基数つきは整数の接尾辞だけ)。
            if radix && s.starts_with('f') {
                continue;
            }
            return (b, s);
        }
    }
    (text, "")
}

fn group3(digits: &str) -> String {
    if digits.len() < 5 {
        return digits.to_string();
    }
    let b = digits.as_bytes();
    let mut out = String::new();
    for (i, c) in b.iter().enumerate() {
        if i > 0 && (b.len() - i) % 3 == 0 {
            out.push('_');
        }
        out.push(*c as char);
    }
    out
}

pub fn canon_int(text: &str) -> String {
    let (sign, rest) = match text.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", text),
    };
    let (body, suf) = split_suffix(rest, INT_SUFFIXES, true);
    for p in ["0x", "0b", "0o"] {
        if let Some(d) = body.strip_prefix(p) {
            let t = d.trim_start_matches(|c| c == '0' || c == '_');
            let t = if t.is_empty() { "0" } else { t };
            return format!("{}{}{}{}", sign, p, t, suf);
        }
    }
    let digits: String = body.chars().filter(|c| *c != '_').collect();
    let t = digits.trim_start_matches('0');
    let t = if t.is_empty() { "0" } else { t };
    format!("{}{}{}", sign, group3(t), suf)
}

pub fn canon_float(text: &str) -> String {
    let (sign, rest) = match text.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", text),
    };
    let (body, suf) = split_suffix(rest, &["f32", "f64"], false);
    let (mant, exp) = match body.split_once('e') {
        Some((m, e)) => (m, e.trim_start_matches('+').parse::<i64>().unwrap_or(0)),
        None => (body, 0),
    };
    let (ip, fp) = match mant.split_once('.') {
        Some((a, b)) => (a, b),
        None => (mant, ""),
    };
    let ip: String = ip.chars().filter(|c| *c != '_').collect();
    let mut digits = format!("{}{}", ip, fp);
    let mut point = ip.len() as i64 + exp; // 値 = 0.digits × 10^point
    let lead = digits.len() - digits.trim_start_matches('0').len();
    digits = digits[lead..].to_string();
    point -= lead as i64;
    let digits = digits.trim_end_matches('0').to_string();
    if digits.is_empty() {
        return format!("{}0.0{}", sign, suf);
    }
    let e10 = point - 1;
    let n = digits.len() as i64;
    let s = if (-4..16).contains(&e10) {
        if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if point >= n {
            format!("{}{}.0", digits, "0".repeat((point - n) as usize))
        } else {
            format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
        }
    } else {
        let frac = if n > 1 { &digits[1..] } else { "0" };
        format!("{}.{}e{}", &digits[..1], frac, e10)
    };
    format!("{}{}{}", sign, s, suf)
}

/// 1 行の整数・浮動小数のリテラルを正規形にする。字句に失敗する行・コメント行はそのまま返す。
pub fn numbers_line(line: &str) -> String {
    if line.trim_start().starts_with("//") {
        return line.to_string();
    }
    let Ok(all) = lexer::lex(&format!("{}\n", line)) else { return line.to_string() };
    let toks: Vec<&Tok> = all.iter().filter(|t| matches!(t.kind, "int" | "float")).collect();
    if toks.is_empty() {
        return line.to_string();
    }
    let mut chars: Vec<char> = line.chars().collect();
    for t in toks.into_iter().rev() {
        let new = if t.kind == "int" { canon_int(&t.text) } else { canon_float(&t.text) };
        if new != t.text {
            let s = t.col - 1;
            chars.splice(s..s + t.text.chars().count(), new.chars());
        }
    }
    chars.into_iter().collect()
}
