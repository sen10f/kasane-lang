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
        if cs[i] == '.' && i > 0 && (cs[i - 1] == ')' || cs[i - 1] == '?') {
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

/// 比較を含む式 (結果は bool で、中の借用は式の中で終わる) は、`&` を含んでも括り出せる。
fn hoistable_bool(a: &str) -> bool {
    let cmp = [" == ", " != ", " < ", " > ", " <= ", " >= "].iter().any(|c| a.contains(c));
    if cmp { hoistable(&a.replace("&mut ", "").replace('&', "")) } else { hoistable(a) }
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
                let no_args = o + 1 < cs.len() && cs[o + 1] == ')';
                base = if no_args || prev.is_empty() || prev.chars().next().unwrap().is_ascii_digit() { name } else { format!("{}_{}", prev, name) };
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
    hoist_cond(line, used)
        .or_else(|| hoist_call(line, used))
        .or_else(|| hoist_array(line, used))
        .or_else(|| hoist_match(line, used))
        .or_else(|| hoist_chain(line, used))
        .or_else(|| fold_line(line))
}

/// `fold_line` を除く、`let` に括り出すだけの縮め方。
fn hoist_lets(line: &str, used: &HashSet<String>) -> Option<Vec<String>> {
    hoist_cond(line, used)
        .or_else(|| hoist_call(line, used))
        .or_else(|| hoist_array(line, used))
        .or_else(|| hoist_match(line, used))
        .or_else(|| hoist_chain(line, used))
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
    let (head, rhs) = split_head(t)?;
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
    let items: Vec<(String, String)> = args.iter().map(|a| (String::new(), a.clone())).collect();
    let namer = |_: &str, a: &str| {
        let b = base_name(a);
        if b == "val" { format!("{}_arg", callee_last) } else { b }
    };
    hoist_items(indent, head, &format!("{}(", callee), &items, ")", &rest, used, &namer)
}

/// `callee(args)` + 末尾 (`?` か `.name()` の連なり) を、入らなければ引数 1 つ 1 行の形 (`f(` / 引数 `,` / `)`) にする。
/// 評価の順も意味も変わらない (書き方の正規形が元から受け付けている形)。入らない引数は同じ規則で入れ子に畳む。
fn fold_expr(pad: usize, prefix: &str, e: &str, suffix: &str) -> Option<Vec<String>> {
    let one = format!("{}{}{}{}", " ".repeat(pad), prefix, e, suffix);
    if one.chars().count() <= 100 {
        return Some(vec![one]);
    }
    // 先頭の `&` / `&mut ` / `*` は、頭に回す。
    for p in ["&mut ", "&", "*"] {
        if let Some(x) = e.strip_prefix(p) {
            return fold_expr(pad, &format!("{}{}", prefix, p), x, suffix);
        }
    }
    let cs: Vec<char> = e.chars().collect();
    // 左から見て、本体が `(` で始まる最初の呼び出し (引数あり) で、残りが `?` と引数なしのメソッドだけのもの。
    let valid_rest = |rest: &str| -> bool {
        let mut r = rest;
        while !r.is_empty() {
            if let Some(x) = r.strip_prefix('?') {
                r = x;
            } else if let Some(x) = r.strip_prefix('.') {
                let n = x.chars().take_while(|c| is_id(*c)).count();
                if n == 0 || !x[n..].starts_with("()") {
                    return false;
                }
                r = &x[n + 2..];
            } else {
                return false;
            }
        }
        true
    };
    let mut found = None;
    let mut depth = 0i32;
    for i in 0..cs.len() {
        match cs[i] {
            '(' if depth == 0 => {
                let name_ok = i > 0 && (is_id(cs[i - 1]));
                if let (true, Some(c)) = (name_ok, matching(&cs, i)) {
                    let rest: String = cs[c + 1..].iter().collect();
                    let nonempty = c > i + 1;
                    if nonempty && valid_rest(&rest) {
                        found = Some((i, c));
                        break;
                    }
                }
                depth += 1;
            }
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
    }
    let (k, close) = found?;
    let callee: String = cs[..k].iter().collect();
    if RESERVED.contains(&callee.as_str()) || callee.is_empty() || callee.ends_with('.') {
        return None;
    }
    let rest: String = cs[close + 1..].iter().collect();
    let args = split_args(&cs[k + 1..close])?;
    if args.is_empty() || args.iter().any(|a| a.is_empty()) {
        return None;
    }
    let mut out = vec![format!("{}{}{}(", " ".repeat(pad), prefix, callee)];
    for a in &args {
        out.extend(fold_expr(pad + 4, "", a, ",")?);
    }
    out.push(format!("{}){}{}", " ".repeat(pad), rest, suffix));
    Some(out)
}

/// 入らない行の最後の手段: 行の外側の呼び出しを、引数 1 つ 1 行の形にする。
fn fold_line(line: &str) -> Option<Vec<String>> {
    let indent = line.len() - line.trim_start().len();
    let t = &line[indent..];
    if t.starts_with("//") || t.contains('"') || t.contains("::<") || t.contains('{') && !t.ends_with(" {") || t.contains('|') {
        return None;
    }
    // match の腕 `P => 式`: 腕の頭 (`P => `) はそのまま、式を畳む。
    let (arm, t) = match t.find(" => ") {
        Some(p) if !t[..p].contains(['(', '{']) || t[..p].ends_with(')') && t[..p].matches('(').count() == t[..p].matches(')').count() => (&t[..p + 4], &t[p + 4..]),
        Some(_) => return None,
        None => ("", t),
    };
    if t.contains("=>") {
        return None;
    }
    let first = t.split(|c| c == ' ' || c == '(' || c == '[').next().unwrap_or("");
    if HEADER_KW.contains(&first) {
        return None;
    }
    let (head, expr, tail): (String, &str, &str) = if let Some(b) = t.strip_prefix("match ").and_then(|r| r.strip_suffix(" {")) {
        ("match ".into(), b, " {")
    } else if let Some(b) = t.strip_prefix("if ").and_then(|r| r.strip_suffix(" {")) {
        match b.strip_prefix('!') {
            Some(x) => ("if !".into(), x, " {"),
            None => ("if ".into(), b, " {"),
        }
    } else if t.ends_with('{') {
        return None;
    } else {
        let (h, r) = if arm.is_empty() { split_head(t)? } else { ("", t) };
        (h.to_string(), r, "")
    };
    let head = format!("{}{}", arm, head);
    let v = fold_expr(indent, &head, expr, tail)?;
    if v.len() > 1 && v.iter().all(|l| l.chars().count() <= 100) { Some(v) } else { None }
}

/// 並び (`f(a, b)` の引数・構造体リテラルの欄・配列の要素) の先頭から、括り出せる式を順に `let` にして行を縮める。
/// どれも書いてある順に、全体より先に評価されるので、先頭から括り出せば順は変わらない。
/// `items` は (欄の `name: ` か空, 式)。`namer` は (欄, 式) から名前の元を作る。
fn hoist_items(
    indent: usize,
    head: &str,
    pre: &str,
    items: &[(String, String)],
    post: &str,
    rest: &str,
    used: &HashSet<String>,
    namer: &dyn Fn(&str, &str) -> String,
) -> Option<Vec<String>> {
    let pad = " ".repeat(indent);
    let mut used = used.clone();
    let mut new_items: Vec<String> = items.iter().map(|(l, e)| format!("{}{}", l, e)).collect();
    let mut lets: Vec<String> = vec![];
    for i in 0..items.len() {
        let (label, a) = (&items[i].0, &items[i].1);
        if trivial(a) {
            continue;
        }
        if !hoistable(a) {
            return None;
        }
        // 先に評価される項目を飛び越えるので、飛び越えた項目と同じ名前を触る式は括り出さない。
        let ai = idents(a);
        for j in 0..i {
            let unhoisted = new_items[j] == format!("{}{}", items[j].0, items[j].1);
            if unhoisted && (!idents(&items[j].1).is_disjoint(&ai) || items[j].1.starts_with('&')) {
                return None;
            }
        }
        let base = namer(label, a);
        let name = unique(&base, &used);
        used.insert(name.clone());
        lets.push(format!("{}let {} = {}", pad, name, a));
        new_items[i] = format!("{}{}", label, name);
        let rebuilt = format!("{}{}{}{}{}{}", pad, head, pre, new_items.join(", "), post, rest);
        if rebuilt.chars().count() <= 100 && lets.iter().all(|l| l.chars().count() <= 100) {
            lets.push(rebuilt);
            return Some(lets);
        }
    }
    None
}

/// 配列リテラル `[a, b, c]` の要素 (書いた順に評価される)。
fn hoist_array(line: &str, used: &HashSet<String>) -> Option<Vec<String>> {
    let indent = line.len() - line.trim_start().len();
    let t = &line[indent..];
    if t.contains('"') || t.contains("=>") || t.contains(';') || t.contains('{') {
        return None;
    }
    let (head, rhs) = split_head(t)?;
    if head.is_empty() {
        return None;
    }
    let inner = rhs.strip_prefix('[')?.strip_suffix(']')?;
    let elems = split_args(&inner.chars().collect::<Vec<_>>())?;
    let items: Vec<(String, String)> = elems.into_iter().map(|e| (String::new(), e)).collect();
    let namer = |_: &str, a: &str| {
        let b = base_name(a);
        if b == "val" { "elem".to_string() } else { b }
    };
    hoist_items(indent, head, "[", &items, "]", "", used, &namer)
}

/// `match f(..) {` / `let p = match f(..) {` の被検査式を `let` に括り出す (どの腕よりも先に評価される)。
/// 借りる結果 (一時値の借用) になりそうな式は括り出さない。
fn hoist_match(line: &str, used: &HashSet<String>) -> Option<Vec<String>> {
    let indent = line.len() - line.trim_start().len();
    let t = &line[indent..];
    if t.contains('"') || t.contains("=>") {
        return None;
    }
    let first = t.split(|c| c == ' ' || c == '(' || c == '[').next().unwrap_or("");
    let (head, rhs) = if first == "match" { ("", t) } else if first == "let" { split_head(t)? } else { return None };
    let scrut = rhs.strip_prefix("match ")?.strip_suffix(" {")?;
    if scrut.contains('{') || scrut.contains('}') || trivial(scrut) {
        return None;
    }
    // 被検査式そのものが入らなければ、その呼び出しの引数を括り出す (引数も先に評価される)。
    if let Some(v) = hoist_match_whole(indent, head, scrut, used) {
        return Some(v);
    }
    let rcs: Vec<char> = scrut.chars().collect();
    let mut k = 0;
    while k < rcs.len() && (is_id(rcs[k]) || rcs[k] == '.') {
        k += 1;
    }
    if k == 0 || k >= rcs.len() || rcs[k] != '(' || matching(&rcs, k)? != rcs.len() - 1 {
        return None;
    }
    let callee: String = rcs[..k].iter().collect();
    let args = split_args(&rcs[k + 1..rcs.len() - 1])?;
    let items: Vec<(String, String)> = args.iter().map(|a| (String::new(), a.clone())).collect();
    let last = callee.rsplit('.').next().unwrap().to_string();
    let namer = |_: &str, a: &str| {
        let b = base_name(a);
        if b == "val" { format!("{}_arg", last) } else { b }
    };
    hoist_items(indent, &format!("{}match ", head), &format!("{}(", callee), &items, ")", " {", used, &namer)
}

fn hoist_match_whole(indent: usize, head: &str, scrut: &str, used: &HashSet<String>) -> Option<Vec<String>> {
    let relaxed = scrut.replace("::<", "<");
    if !hoistable(&relaxed) {
        return None;
    }
    // 型引数の `<` が `a < b` の比較とは別物であることだけ確かめる: 括り出す式は呼び出しで終わる (`f::<T>(..)`)。
    if !scrut.ends_with(')') || scrut.contains("||") || scrut.contains("&&") {
        return None;
    }
    let pad = " ".repeat(indent);
    let b = base_name(scrut);
    let name = unique(&if b == "val" { "matched".to_string() } else { b }, used);
    let l1 = format!("{}let {} = {}", pad, name, scrut);
    let l2 = format!("{}{}match {} {{", pad, head, name);
    if l1.chars().count() <= 100 && l2.chars().count() <= 100 {
        Some(vec![l1, l2])
    } else {
        None
    }
}

/// 先頭の `let [mut] x[: T] = ` / `path = ` を分ける (残りは右辺)。
fn split_head(t: &str) -> Option<(&str, &str)> {
    let first = t.split(|c| c == ' ' || c == '(' || c == '[').next().unwrap_or("");
    if first == "return" {
        Some(("return ", &t[7..]))
    } else if first == "let" {
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
            // `&&` は `||` より強いので、`||` で分けるときは項の中の `&&` を許す。`&&` で分けるときの `||` は許さない。
            if two == other && op == "&&" {
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
    if ops.iter().any(|o| o.is_empty() || !hoistable_bool(o)) {
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

/// `P => if c {` / `P => match x {` (本体が次の行から、腕と同じ字下げの `}` まで) を `P => {` + 字下げした本体に開く。
/// 腕の本体が 1 つの式文のままブロックに包まれるだけで、意味は変わらない。
fn open_arm(lines: &mut Vec<String>, i: usize, used: &HashSet<String>) -> bool {
    let l = lines[i].clone();
    let indent = l.len() - l.trim_start().len();
    let t = l.trim_start();
    if !t.ends_with(" {") || t.contains('"') {
        return false;
    }
    let Some(p) = t.find(" => ") else { return false };
    let (pat, body) = (&t[..p], &t[p + 4..]);
    if !(body.starts_with("if ") || body.starts_with("match ")) || pat.contains('{') || pat.contains("=>") {
        return false;
    }
    let pad = " ".repeat(indent);
    let close = format!("{}}}", pad);
    let Some(j) = (i + 1..lines.len()).find(|&j| lines[j] == close) else { return false };
    if lines[i + 1..=j].iter().any(|s| s.matches('"').count() % 2 == 1) || lines[i + 1..j].iter().any(|s| !s.is_empty() && s.len() - s.trim_start().len() < indent + 4) {
        return false;
    }
    // 開いた先で `let` に括り出せるときだけ開く (本体が 1 文のブロックの腕は、整形が 1 行の腕に戻してしまう)。
    let inner = format!("{}    {}", pad, body);
    let Some(new) = hoist_lets(&inner, used) else { return false };
    for k in i + 1..=j {
        if !lines[k].is_empty() {
            lines[k] = format!("    {}", lines[k]);
        }
    }
    lines.insert(j + 1, close);
    lines[i] = format!("{}{} => {{", pad, pat);
    lines.splice(i + 1..i + 1, new);
    true
}

pub fn hoist_long_lines(lines: &mut Vec<String>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].clone();
        // 文字列を含む行は事前検査が E0055 の対象にしない (整った正しいファイルを触らない)。
        if l.chars().count() > 100 && l.is_ascii() && !l.contains('"') {
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
                // 腕の頭 `P => if c {` / `P => match x {` が長いとき: 腕を `P => {` に開き、中で縮められるようにする。
                if open_arm(lines, i, &used) {
                    changed = true;
                    i += 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    changed
}


/// E0055 の文面に足す、括り出す式の名指し (`ksn fmt` が自分で縮められなかった行用)。
/// 括弧・欄の中のいちばん長い項目 (引数・欄・要素)、なければ `||` / `&&` の連なりの左の部分を挙げる。
pub fn suggest(line: &str) -> Option<String> {
    let t = line.trim_start();
    if t.starts_with("//") || !line.is_ascii() {
        return None;
    }
    let shorten = |e: &str| -> String {
        let e = e.trim();
        if e.chars().count() > 60 { format!("{}...", e.chars().take(57).collect::<String>()) } else { e.to_string() }
    };
    for op in ["||", "&&"] {
        let body = t.strip_prefix("if ").and_then(|r| r.strip_suffix(" {")).or_else(|| t.split_once(" = ").map(|x| x.1)).unwrap_or(t);
        if let Some(ops) = split_op(body, op) {
            if ops.len() >= 3 {
                let k = ops.len() / 2;
                return Some(format!("bind the left part `{}` to a `let` before this line", shorten(&ops[..k].join(&format!(" {} ", op)))));
            }
        }
    }
    let cs: Vec<char> = t.chars().collect();
    let (mut depth, mut cur, mut best): (i32, String, String) = (0, String::new(), String::new());
    for &c in &cs {
        match c {
            '(' | '[' | '{' => {
                depth += 1;
                if depth == 1 {
                    cur.clear();
                    continue;
                }
            }
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    if cur.trim().chars().count() > best.chars().count() {
                        best = cur.trim().to_string();
                    }
                    cur.clear();
                    continue;
                }
            }
            ',' if depth == 1 => {
                if cur.trim().chars().count() > best.chars().count() {
                    best = cur.trim().to_string();
                }
                cur.clear();
                continue;
            }
            _ => {}
        }
        if depth >= 1 {
            cur.push(c);
        }
    }
    let best = best.split_once(": ").map_or(best.clone(), |(n, e)| if n.chars().all(is_id) { e.to_string() } else { best.clone() });
    if best.chars().count() >= 12 && !trivial(&best) {
        return Some(format!("bind `{}` to a `let` before this line", shorten(&best)));
    }
    None
}


#[cfg(test)]
mod tests {
    use super::*;

    fn h(l: &str) -> Option<Vec<String>> {
        hoist_line(l, &HashSet::new())
    }

    #[test]
    fn return_with_a_call_argument() {
        let l = "                    return bench.http.render_json(400u16, request.keep_alive(), body.as_str().as_bytes())";
        let r = h(l);
        eprintln!("{:?}", r);
        assert!(r.is_some());
    }

    #[test]
    fn mixed_or_and_chain() {
        let l = "                    let is_alpha_num = (*b >= b'A' && *b <= b'Z') || (*b >= b'a' && *b <= b'z') || *b >= b'0' && *b <= b'9'";
        eprintln!("{:?}", h(l));
        assert!(h(l).is_some());
    }
}
