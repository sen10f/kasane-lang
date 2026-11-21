//! 括弧・カンマの文面の補助 (型の文面を割る)。借用検査の本体は TAST (`tast_rules/borrows.rs`・`lifetimes.rs`) に移し、旧の検査はここから消した。



/// `s[open]` の括弧 (`(`・`[`・`<`) に対応する閉じ括弧の位置。`->` の `>` は数えない。
pub(crate) fn match_close(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'(' | b'[' | b'<' => depth += 1,
            b')' | b']' => depth -= 1,
            b'>' if i > 0 && b[i - 1] == b'-' => {}
            b'>' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 最上位のカンマで分ける (括弧の中のカンマは区切りにしない)。
pub(crate) fn split_top(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let (mut depth, mut start) = (0i32, 0usize);
    let mut out = vec![];
    for i in 0..b.len() {
        match b[i] {
            b'(' | b'[' | b'<' => depth += 1,
            b')' | b']' => depth -= 1,
            b'>' if i > 0 && b[i - 1] == b'-' => {}
            b'>' => depth -= 1,
            b',' if depth == 0 => {
                out.push(s[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    if !s[start..].trim().is_empty() {
        out.push(s[start..].trim().to_string());
    }
    out
}

