//! `.ksnh` (コメント付き) と `.ksnl` (コメント無し) の変換 (設計_ksnhとロック.md 1 章)。
//!
//! `.ksnh` は `.ksnl` にコメントの行 (`//`・`///`) を足しただけのもの。`strip` はコメントの行を消す。
//! 唯一の例外: ファイル先頭のコメントの塊の直後に空行があるときは、その空行も一緒に消す
//! (モジュールの説明を外したあとに、ファイルの先頭が空行にならないように)。

/// 先頭の空白を除いて `//` で始まる行か (`///` を含む)。
pub fn is_comment_line(l: &str) -> bool {
    l.trim_start_matches(' ').starts_with("//")
}

/// `.ksnh` の各行が `.ksnl` に残るか。`keep[i]` は i 行目 (0 始まり) を残すとき true。
pub fn keep_mask(lines: &[&str]) -> Vec<bool> {
    let mut keep: Vec<bool> = lines.iter().map(|l| !is_comment_line(l)).collect();
    // ファイル先頭のコメントの塊 + 直後の空行 1 つ。
    let head = lines.iter().take_while(|l| is_comment_line(l)).count();
    if head > 0 && head < lines.len() && lines[head].is_empty() && head + 1 < lines.len() {
        keep[head] = false;
    }
    keep
}

/// `.ksnh` から `.ksnl` を作る。
pub fn strip(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let keep = keep_mask(&lines);
    let out: Vec<&str> = lines.iter().zip(&keep).filter(|(_, k)| **k).map(|(l, _)| *l).collect();
    out.join("\n")
}

/// `.ksnh` の行 (1 始まり) → `.ksnl` の行 (1 始まり) の対応。消えた行は、次に残る行に対応させる
/// (最後の行より後ろは最後に残る行)。戻り値の添字 `i` が `.ksnh` の `i + 1` 行目。
pub fn line_map(text: &str) -> Vec<usize> {
    let lines: Vec<&str> = text.split('\n').collect();
    let keep = keep_mask(&lines);
    let total = keep.iter().filter(|k| **k).count().max(1);
    let mut map = vec![0usize; lines.len()];
    let mut n = 0usize;
    for (i, k) in keep.iter().enumerate() {
        if *k {
            n += 1;
        }
        map[i] = (if *k { n } else { n + 1 }).min(total);
    }
    map
}

/// `.ksnl` の中のコメントの行 (1 始まり)。`.ksnl` に置けない (E0050)。
pub fn comment_lines_in_ksnl(text: &str) -> Vec<usize> {
    text.split('\n').enumerate().filter(|(_, l)| is_comment_line(l)).map(|(i, _)| i + 1).collect()
}

/// `.ksnh` の `strip` が `.ksnl` と食い違う最初の `.ksnh` の行 (1 始まり)。一致すれば `None`。
pub fn first_mismatch(ksnh: &str, ksnl: &str) -> Option<usize> {
    let s = strip(ksnh);
    if s == ksnl {
        return None;
    }
    let map = line_map(ksnh);
    let a: Vec<&str> = s.split('\n').collect();
    let b: Vec<&str> = ksnl.split('\n').collect();
    let k = (0..a.len().max(b.len())).find(|&i| a.get(i) != b.get(i)).unwrap_or(0) + 1;
    // `.ksnl` の k 行目に当たる、最初の `.ksnh` の行。
    Some(map.iter().position(|&m| m >= k).map_or(1, |i| i + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_drops_comment_lines() {
        let h = "/// mod doc\n\nimport a\n\n// c\nfn a.f()\nend a.f\n";
        assert_eq!(strip(h), "import a\n\nfn a.f()\nend a.f\n");
        let m = line_map(h);
        assert_eq!(m[0], 1);
        assert_eq!(m[2], 1);
        assert_eq!(m[4], 3);
        assert_eq!(m[5], 3);
    }

    #[test]
    fn head_comment_attached_to_def_keeps_no_blank() {
        let h = "// c\nfn a.f()\nend a.f\n";
        assert_eq!(strip(h), "fn a.f()\nend a.f\n");
    }

    #[test]
    fn mismatch_line() {
        let h = "// c\nfn a.f()\n    1\nend a.f\n";
        assert_eq!(first_mismatch(h, "fn a.f()\n    1\nend a.f\n"), None);
        assert_eq!(first_mismatch(h, "fn a.f()\n    2\nend a.f\n"), Some(3));
    }
}
