//! 100 文字を超える行を、呼び出しの引数を `let` に括り出して縮める (`ksn fmt`。2026-11-23 の決定)。
//! 括り出すのは「文の右辺が 1 つの呼び出し `f(a, b, ..)` で、先頭から数えた引数」だけ。引数は元から
//! 左から順に、呼び出しより先に評価されるので、評価の順も意味も変わらない。短絡・分岐・閉包・借用・
//! 一時値を含む引数は括り出さない (直せなければ行は触らず、E0055 のまま)。

use std::collections::HashSet;

const HEADER_KW: &[&str] = &["fn", "type", "enum", "trait", "impl", "const", "static", "extern", "test", "import", "end"];
const RESERVED: &[&str] = &[
    "fn", "type", "enum", "trait", "impl", "const", "static", "extern", "test", "end", "import", "let", "mut", "if", "else", "match",
    "loop", "while", "for", "in", "break", "continue", "return", "true", "false", "self", "Self", "pub", "uses", "panics", "unsafe",
    "move", "with", "dyn", "blanket", "never", "may", "async", "await", "where", "ref", "as", "link",
];
/// 受け手が一時値のとき、結果がそれを借りるメソッド (括り出すと一時値が先に落ちる)。
const BORROWING: &[&str] = &[
    "as_slice", "as_str", "as_bytes", "as_mut_slice", "iter", "iter_mut", "chars", "bytes", "lines", "split", "trim", "get", "get_mut",
    "first", "last", "peek", "keys", "values", "borrow", "borrow_mut", "as_ref", "as_mut", "as_deref", "slice", "windows", "chunks",
];
/// 型が文脈で決まる (括り出すと E2044 になりうる) 引数の末尾。
const CONTEXT_TYPED: &[&str] = &["new", "default", "with_capacity", "from", "into", "parse", "collect", "unwrap_or_default"];

fn is_id(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn idents(s: &str) -> HashSet<String> {
    s.split(|c: char| !is_id(c)).filter(|w| !w.is_empty()).map(String::from).collect()
}

/// `cs[open]` の `(` に対応する `)` の位置 (文字列は無い行しか来ない。`'x'` の文字リテラルは飛ばす)。
fn matching(cs: &[char], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < cs.len() {
        match cs[i] {
            '\'' => {
                if i + 2 < cs.len() && cs[i + 1] != '\\' && cs[i + 2] == '\'' {
                    i += 2;
                } else if i + 1 < cs.len() && cs[i + 1] == '\\' {
                    i += 2;
                    while i < cs.len() && cs[i] != '\'' {
                        i += 1;
                    }
                }
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn split_args(cs: &[char]) -> Option<Vec<String>> {
    let mut out = vec![];
    let (mut depth, mut cur, mut i) = (0i32, String::new(), 0usize);
    while i < cs.len() {
        let c = cs[i];
        match c {
            '\'' => {
                let mut j = i + 1;
                if j < cs.len() && cs[j] == '\\' {
                    while j < cs.len() && cs[j] != '\'' || j == i + 2 {
                        j += 1;
                    }
                } else {
                    j = i + 2;
                }
                if j >= cs.len() || cs[j] != '\'' {
                    return None;
                }
                cur.extend(&cs[i..=j]);
                i = j + 1;
                continue;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur = String::new();
                i += 1;
                continue;
            }
            _ => {}
        }
        cur.push(c);
        i += 1;
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    Some(out)
}

fn trivial(a: &str) -> bool {
    let a = a.strip_prefix('*').unwrap_or(a);
    !a.is_empty() && a.chars().all(|c| is_id(c) || c == '.')
}

/// 括り出してよい引数か (値だけを作り、借りも分岐も持たない式)。
fn hoistable(a: &str) -> bool {
    let b = a.replace("&&", "").replace("||", "");
    if b.contains('&') || b.contains('|') || b.contains('{') || b.contains('}') || b.contains('"') || b.contains("::") {
        return false;
    }
    if a.starts_with("None") || a.contains("=>") || a.contains("..") {
        return false;
    }
    let cs: Vec<char> = a.chars().collect();
    // 受け手が呼び出しの結果 (`)` の直後の `.name(`) で、借りるメソッドなら括り出さない。
    for i in 0..cs.len() {
        if cs[i] == '.' && i > 0 && cs[i - 1] == ')' {
            let name: String = cs[i + 1..].iter().take_while(|c| is_id(**c)).collect();
            if BORROWING.contains(&name.as_str()) {
                return false;
            }
        }
    }
    // 型が文脈で決まる末尾 (`Vec.new()` など) は括り出さない。
    if let Some(p) = a.rfind('(') {
        let name: String = a[..p].chars().rev().take_while(|c| is_id(*c)).collect::<Vec<_>>().into_iter().rev().collect();
        if CONTEXT_TYPED.contains(&name.as_str()) {
            return false;
        }
    }
    true
}

/// 引数の式から名前を作る: 末尾が呼び出しなら `<直前の語>_<名前>` か `<名前>`、それ以外は `val`。
fn base_name(a: &str) -> String {
    let a = a.trim_end_matches('?');
    let mut base = String::from("val");
    if a.ends_with(')') {
        let cs: Vec<char> = a.chars().collect();
        let mut depth = 0i32;
        let mut open = None;
        for i in (0..cs.len()).rev() {
            match cs[i] {
                ')' => depth += 1,
                '(' => {
                    depth -= 1;
                    if depth == 0 {
                        open = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(o) = open {
            let name: String = cs[..o].iter().rev().take_while(|c| is_id(**c)).collect::<Vec<_>>().into_iter().rev().collect();
            if !name.is_empty() && !name.chars().next().unwrap().is_ascii_digit() {
                let before = &cs[..o - name.chars().count()];
                let prev: String = if before.last() == Some(&'.') {
                    before[..before.len() - 1].iter().rev().take_while(|c| is_id(**c)).collect::<Vec<_>>().into_iter().rev().collect()
                } else {
                    String::new()
                };
                base = if prev.is_empty() || prev.chars().next().unwrap().is_ascii_digit() { name } else { format!("{}_{}", prev, name) };
            }
        }
    }
    if RESERVED.contains(&base.as_str()) {
        base.push_str("_v");
    }
    base
}

fn unique(base: &str, used: &HashSet<String>) -> String {
    if !used.contains(base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let c = format!("{}_{}", base, n);
        if !used.contains(&c) {
            return c;
        }
        n += 1;
    }
}

/// 1 行を縮める。縮められなければ None。`used` はその関数で使われている名前。
fn hoist_line(line: &str, used: &HashSet<String>) -> Option<Vec<String>> {
    let indent = line.len() - line.trim_start().len();
    let t = &line[indent..];
    if t.starts_with("//") || t.contains('"') || t.contains("::<") || t.contains("=>") || t.contains('{') || t.contains('}') {
        return None;
    }
    let first = t.split(|c| c == ' ' || c == '(' || c == '[').next().unwrap_or("");
    if HEADER_KW.contains(&first) {
        return None;
    }
    // 右辺の前の部分 (`let [mut] name[: T] = ` / `path = ` / なし)。
    let (head, rhs) = if first == "let" {
        let p = t.find(" = ")?;
        (&t[..p + 3], &t[p + 3..])
    } else if let Some(p) = t.find(" = ") {
        let lhs = &t[..p];
        if !trivial(lhs) || RESERVED.contains(&lhs) {
            return None;
        }
        (&t[..p + 3], &t[p + 3..])
    } else {
        ("", t)
    };
    // 単純な `let` / 代入 / 式だけ。`let ... else`・if・match などはここに来ない (callee の形で落とす)。
    let rcs: Vec<char> = rhs.chars().collect();
    let mut k = 0;
    while k < rcs.len() && (is_id(rcs[k]) || rcs[k] == '.') {
        k += 1;
    }
    if k == 0 || k >= rcs.len() || rcs[k] != '(' {
        return None;
    }
    let callee: String = rcs[..k].iter().collect();
    if RESERVED.contains(&callee.as_str()) || callee.starts_with('.') || callee.ends_with('.') {
        return None;
    }
    let close = matching(&rcs, k)?;
    let rest: String = rcs[close + 1..].iter().collect();
    if rest != "" && rest != "?" {
        return None;
    }
    let args = split_args(&rcs[k + 1..close])?;
    if args.is_empty() || args.iter().any(|a| a.is_empty()) {
        return None;
    }
    let callee_last = callee.rsplit('.').next().unwrap().to_string();
    let mut used = used.clone();
    let mut new_args: Vec<String> = args.clone();
    let mut lets: Vec<String> = vec![];
    for i in 0..args.len() {
        let a = &args[i];
        if trivial(a) {
            continue;
        }
        if !hoistable(a) {
            return None;
        }
        // 先に評価される引数を飛び越えるので、飛び越えた引数と同じ名前を触る式は括り出さない。
        let ai = idents(a);
        for j in 0..i {
            if new_args[j] == args[j] && !idents(&args[j]).is_disjoint(&ai) {
                return None;
            }
            if new_args[j] == args[j] && args[j].starts_with('&') {
                return None;
            }
        }
        let base = if base_name(a) == "val" { format!("{}_arg", callee_last) } else { base_name(a) };
        let name = unique(&base, &used);
        used.insert(name.clone());
        lets.push(format!("{}let {} = {}", " ".repeat(indent), name, a));
        new_args[i] = name;
        let rebuilt = format!("{}{}{}({}){}", " ".repeat(indent), head, callee, new_args.join(", "), rest);
        if rebuilt.chars().count() <= 100 && lets.iter().all(|l| l.chars().count() <= 100) {
            lets.push(rebuilt);
            return Some(lets);
        }
    }
    None
}

pub fn hoist_long_lines(lines: &mut Vec<String>) {
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].clone();
        if l.chars().count() > 100 {
            let prev_ok = (0..i).rev().map(|j| lines[j].trim_end()).find(|s| !s.is_empty()).map_or(true, |p| {
                !(p.ends_with(['(', '[', ',', '=', '+', '-', '*', '/', '.', '&', '|', '<', '>']) || p.ends_with("=>"))
            });
            if prev_ok {
                let start = (0..=i).rev().find(|&j| !lines[j].is_empty() && !lines[j].starts_with(' ') && !lines[j].starts_with("end ")).unwrap_or(0);
                let stop = (i..lines.len()).find(|&j| lines[j].starts_with("end ")).unwrap_or(lines.len() - 1);
                let used = idents(&lines[start..=stop].join("\n"));
                if let Some(new) = hoist_line(&l, &used) {
                    let n = new.len();
                    lines.splice(i..=i, new);
                    i += n;
                    continue;
                }
            }
        }
        i += 1;
    }
}
