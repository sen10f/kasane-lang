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
    // snake_case は `_2` のような数字だけの部分を認めないので、b, c, .. z, ba, bb, .. の文字で区別する。
    let mut n = 1usize;
    loop {
        let mut digits = vec![];
        let mut k = n;
        while k > 0 {
            digits.push((b'a' + (k % 26) as u8) as char);
            k /= 26;
        }
        let c = format!("{}_{}", base, digits.iter().rev().collect::<String>());
        if !used.contains(&c) {
            return c;
        }
        n += 1;
    }
}

/// 1 行を縮める。縮められなければ None。`used` はその関数で使われている名前。
fn hoist_line(line: &str, used: &HashSet<String>) -> Option<Vec<String>> {
    hoist_cond(line, used).or_else(|| hoist_call(line, used)).or_else(|| hoist_chain(line, used))
}

fn hoist_call(line: &str, used: &HashSet<String>) -> Option<Vec<String>> {
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

/// 先頭の `let [mut] x[: T] = ` / `path = ` を分ける (残りは右辺)。
fn split_head(t: &str) -> Option<(&str, &str)> {
    let first = t.split(|c| c == ' ' || c == '(' || c == '[').next().unwrap_or("");
    if first == "let" {
        let p = t.find(" = ")?;
        Some((&t[..p + 3], &t[p + 3..]))
    } else if let Some(p) = t.find(" = ") {
        let lhs = &t[..p];
        if !trivial(lhs) || RESERVED.contains(&lhs) {
            return None;
        }
        Some((&t[..p + 3], &t[p + 3..]))
    } else {
        Some(("", t))
    }
}

/// 深さ 0 の `op` (`||` か `&&`) で式を分ける。ほかの深さ 0 の二項演算子 (`||` と `&&` の混在) があれば None。
fn split_op<'a>(e: &'a str, op: &str) -> Option<Vec<&'a str>> {
    let cs: Vec<char> = e.chars().collect();
    let other = if op == "||" { "&&" } else { "||" };
    let (mut depth, mut last, mut out) = (0i32, 0usize, vec![]);
    let bytes = e.as_bytes();
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '\'' => {
                if i + 2 < cs.len() && cs[i + 1] != '\\' && cs[i + 2] == '\'' {
                    i += 3;
                    continue;
                } else if i + 1 < cs.len() && cs[i + 1] == '\\' {
                    i += 2;
                    while i < cs.len() && cs[i] != '\'' {
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
                return None;
            }
            _ => {}
        }
        if depth == 0 && i + 1 < cs.len() {
            let two = &e[i..i + 2];
            if two == op {
                out.push(e[last..i].trim());
                last = i + 2;
                i += 2;
                continue;
            }
            if two == other {
                return None;
            }
        }
        i += 1;
    }
    let _ = bytes;
    out.push(e[last..].trim());
    if out.len() < 2 { None } else { Some(out) }
}

/// `if a || b || c {` と `let x = a && b && c` の長い連なり: 左の部分を `let` に括り出す。
/// 同じ演算子だけの連なりなので、左の連なりを束縛しても短絡の順は変わらない。
fn hoist_cond(line: &str, used: &HashSet<String>) -> Option<Vec<String>> {
    let indent = line.len() - line.trim_start().len();
    let t = &line[indent..];
    if t.contains('"') || t.contains("::<") || t.contains("=>") {
        return None;
    }
    let (head, body, tail) = if let Some(b) = t.strip_prefix("if ").and_then(|r| r.strip_suffix(" {")) {
        ("if ", b, " {")
    } else if t.starts_with("let ") {
        let (h, r) = split_head(t)?;
        (h, r, "")
    } else {
        return None;
    };
    if body.contains('{') || body.contains('}') || body.contains('|') && !body.contains("||") {
        return None;
    }
    let op = if split_op(body, "||").is_some() { "||" } else { "&&" };
    let ops = split_op(body, op)?;
    if ops.iter().any(|o| o.is_empty() || !hoistable(o)) {
        return None;
    }
    let pad = " ".repeat(indent);
    let mut used = used.clone();
    let mut cur: Vec<String> = ops.iter().map(|s| s.to_string()).collect();
    let mut lets: Vec<String> = vec![];
    loop {
        let line = format!("{}{}{}{}", pad, head, cur.join(&format!(" {} ", op)), tail);
        if line.chars().count() <= 100 {
            lets.push(line);
            return Some(lets);
        }
        let target = head.strip_prefix("let ").map(|h| h.trim_start_matches("mut ").split(|c| c == ':' || c == ' ').next().unwrap_or("").to_string());
        let base = match target {
            Some(t) if !t.is_empty() => format!("{}_{}", t, (b'a' + lets.len().min(25) as u8) as char),
            _ => "cond".to_string(),
        };
        let name = unique(&base, &used);
        // 先頭から入る最長の連なり (2 項以上) を `let` にする。
        let mut m = 0;
        for k in 2..cur.len() {
            if format!("{}let {} = {}", pad, name, cur[..k].join(&format!(" {} ", op))).chars().count() <= 100 {
                m = k;
            }
        }
        if m < 2 || m >= cur.len() && head != "if " {
            return None;
        }
        used.insert(name.clone());
        lets.push(format!("{}let {} = {}", pad, name, cur[..m].join(&format!(" {} ", op))));
        let rest: Vec<String> = cur[m..].to_vec();
        cur = std::iter::once(name).chain(rest).collect();
    }
}

fn numeric_method(n: &str) -> bool {
    n.starts_with("wrapping_") || n.starts_with("saturating_") || n.starts_with("checked_") || matches!(n, "len" | "abs" | "min" | "max" | "pow" | "rem_euclid")
}

/// 値を作る数のメソッドの連なり `a.wrapping_add(x).wrapping_mul(y)..`: 先頭の部分を `let` に括り出す。
/// 括り出す部分は数 (Copy) を返すので、一時値の寿命は変わらない。
fn hoist_chain(line: &str, used: &HashSet<String>) -> Option<Vec<String>> {
    let indent = line.len() - line.trim_start().len();
    let t = &line[indent..];
    if t.contains('"') || t.contains("::<") || t.contains("=>") || t.contains('{') || t.contains('}') || t.contains('\'') {
        return None;
    }
    let (head, rhs) = split_head(t)?;
    if head.is_empty() || head.starts_with("if ") {
        // 文の頭が呼び出しの式 (`line.push(..).unwrap()` など) は、受け手の可変性が変わりうるので対象にしない。
        return None;
    }
    let pad = " ".repeat(indent);
    let mut used = used.clone();
    let mut rhs = rhs.to_string();
    let mut lets: Vec<String> = vec![];
    loop {
        let fin = format!("{}{}{}", pad, head, rhs);
        if fin.chars().count() <= 100 {
            lets.push(fin);
            return if lets.len() > 1 { Some(lets) } else { None };
        }
        let cs: Vec<char> = rhs.chars().collect();
        // 最上位の `)` の直後が `.` になる位置 (連なりの切れ目)。
        let (mut depth, mut cuts) = (0i32, vec![]);
        for i in 0..cs.len() {
            match cs[i] {
                '(' | '[' => depth += 1,
                ')' | ']' => {
                    depth -= 1;
                    if depth == 0 && i + 1 < cs.len() && cs[i + 1] == '.' {
                        cuts.push(i + 1);
                    }
                }
                _ => {}
            }
        }
        if depth != 0 {
            return None;
        }
        let mut done = false;
        for c in cuts.into_iter().rev() {
            let prefix: String = cs[..c].iter().collect();
            let remain: String = cs[c..].iter().collect();
            let p: Vec<char> = prefix.chars().collect();
            let Some(o) = (0..p.len()).rev().find(|&i| p[i] == '(' && matching(&p, i) == Some(p.len() - 1)) else { continue };
            let last: String = p[..o].iter().rev().take_while(|c| is_id(**c)).collect::<Vec<_>>().into_iter().rev().collect();
            if !numeric_method(&last) || !hoistable(&prefix) {
                continue;
            }
            let target = head.strip_prefix("let ").map(|h| h.trim_start_matches("mut ").split(|c| c == ':' || c == ' ').next().unwrap_or("").to_string());
            let base = match target {
                Some(t) if !t.is_empty() => format!("{}_{}", t, (b'a' + lets.len() as u8) as char),
                _ => format!("{}_val", last),
            };
            let name = unique(&base, &used);
            let l1 = format!("{}let {} = {}", pad, name, prefix);
            if l1.chars().count() > 100 {
                continue;
            }
            used.insert(name.clone());
            lets.push(l1);
            rhs = format!("{}{}", name, remain);
            done = true;
            break;
        }
        if !done {
            return None;
        }
    }
}

pub fn hoist_long_lines(lines: &mut Vec<String>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].clone();
        if l.chars().count() > 100 && l.is_ascii() {
            let prev_ok = (0..i).rev().map(|j| lines[j].trim_end()).find(|s| !s.is_empty()).map_or(true, |p| {
                !(p.ends_with(['(', '[', ',', '=', '+', '-', '*', '/', '.', '&', '|', '<', '>']) || p.ends_with("=>"))
            });
            if prev_ok {
                let start = (0..=i).rev().find(|&j| !lines[j].is_empty() && !lines[j].starts_with(' ') && !lines[j].starts_with("end ")).unwrap_or(0);
                let stop = (i..lines.len()).find(|&j| lines[j].starts_with("end ")).unwrap_or(lines.len() - 1);
                let used = idents(&lines[start..=stop].join("\n"));
                if let Some(new) = hoist_line(&l, &used) {
                    let n = new.len();
                    changed = true;
                    lines.splice(i..=i, new);
                    i += n;
                    continue;
                }
            }
        }
        i += 1;
    }
    changed
}

