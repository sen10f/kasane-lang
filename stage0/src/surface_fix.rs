//! `ksn fix` の前段: 構文として読めないファイルでも当てられる、生の文面の書き換え。
//! Rust 風の表面の形 (E0056 1 行ブロック・E0047 `enum X: A, B` /
//! `type X: f: T` と丸括弧の本体・E0062 `impl T for Y` の中の短い `fn`) を、直し方が 1 つに決まるものだけ
//! 正規形へ書き換える。決まらない形は触らない (診断が残る)。意味は変えない。

fn indent_of(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

/// 文字列・文字リテラルを読み飛ばす。`i` が引用符の先頭 (`"`・`'`) のとき、閉じた次の位置を返す。
fn skip_lit(cs: &[char], i: usize) -> usize {
    if cs[i] == '"' {
        let mut j = i + 1;
        while j < cs.len() {
            if cs[j] == '\\' {
                j += 2;
            } else if cs[j] == '"' {
                return j + 1;
            } else {
                j += 1;
            }
        }
        return cs.len();
    }
    if cs[i] == '\'' {
        if i + 1 < cs.len() && cs[i + 1] == '\\' {
            let mut j = i + 2;
            while j < cs.len() && cs[j] != '\'' {
                j += 1;
            }
            return (j + 1).min(cs.len());
        }
        if i + 2 < cs.len() && cs[i + 2] == '\'' {
            return i + 3;
        }
    }
    i + 1
}

/// 最上位 (括弧の深さ 0・リテラルの外) で `sep` の位置に区切る。深さの釣り合いが取れないときは None。
fn split_top(s: &str, sep: char) -> Option<Vec<String>> {
    split_top_a(s, sep, false)
}

/// `angle` なら `<` `>` も深さに数える (`->` と `=>` の `>` は数えない)。型の欄の区切り用。
fn split_top_a(s: &str, sep: char, angle: bool) -> Option<Vec<String>> {
    let cs: Vec<char> = s.chars().collect();
    let (mut depth, mut i, mut start) = (0i32, 0usize, 0usize);
    let mut out = vec![];
    while i < cs.len() {
        match cs[i] {
            '"' | '\'' => {
                i = skip_lit(&cs, i);
                continue;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '<' if angle => depth += 1,
            '>' if angle && i > 0 && cs[i - 1] != '-' && cs[i - 1] != '=' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(cs[start..i].iter().collect::<String>());
                start = i + 1;
            }
            _ => {}
        }
        if depth < 0 {
            return None;
        }
        i += 1;
    }
    if depth != 0 {
        return None;
    }
    out.push(cs[start..].iter().collect::<String>());
    Some(out)
}

fn balanced(s: &str) -> bool {
    split_top(s, '\u{0}').is_some()
}

/// 行の最初の最上位の `{` と、それに対応する `}` の位置 (文字の添字)。
fn first_brace(cs: &[char]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '"' | '\'' => {
                i = skip_lit(cs, i);
                continue;
            }
            '{' => {
                let (mut d, mut j) = (0i32, i);
                while j < cs.len() {
                    match cs[j] {
                        '"' | '\'' => {
                            j = skip_lit(cs, j);
                            continue;
                        }
                        '{' => d += 1,
                        '}' => {
                            d -= 1;
                            if d == 0 {
                                return Some((i, j));
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                return None;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn last_ident(s: &str) -> &str {
    s.trim_end().rsplit(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("")
}

/// E0056: `=> {}` は `=> ()`、`{ a; b; }` は本体を行に分ける。
fn inline_blocks(lines: Vec<String>) -> Vec<String> {
    let mut out = vec![];
    for l in lines {
        let t = l.trim_start();
        if t.starts_with("//") || t.is_empty() {
            out.push(l);
            continue;
        }
        let cs: Vec<char> = l.chars().collect();
        let Some((a, b)) = first_brace(&cs) else {
            out.push(l);
            continue;
        };
        let prefix: String = cs[..a].iter().collect();
        let body: String = cs[a + 1..b].iter().collect();
        let mut rest: String = cs[b + 1..].iter().collect();
        let ptrim = prefix.trim_end();
        // `if c { a } else { b }` / `else if c2 { .. }` の連なりを、組ごとに行へ展開する。
        let mut chain: Vec<(String, String)> = vec![];
        loop {
            let r = rest.trim_start().to_string();
            if !r.starts_with("else") {
                break;
            }
            let rc: Vec<char> = r.chars().collect();
            let Some((a2, b2)) = first_brace(&rc) else { break };
            chain.push((rc[..a2].iter().collect::<String>().trim().to_string(), rc[a2 + 1..b2].iter().collect()));
            rest = rc[b2 + 1..].iter().collect();
        }
        // 構造体リテラル・構造体パターン (直前の語が大文字始まり) と、`{ }` の後ろに続きがある形は触らない。
        if last_ident(ptrim).chars().next().map_or(false, |c| c.is_uppercase()) || !matches!(rest.trim(), "" | ",") || ptrim.is_empty() {
            out.push(l);
            continue;
        }
        let kw = t.split_whitespace().next().unwrap_or("");
        if matches!(kw, "type" | "enum" | "trait" | "impl") {
            out.push(l);
            continue;
        }
        if body.trim().is_empty() {
            if ptrim.ends_with("=>") && chain.is_empty() {
                out.push(format!("{} (){}", ptrim, rest.trim_end()));
            } else {
                out.push(l);
            }
            continue;
        }
        let Some(stmts) = split_top(&body, ';') else {
            out.push(l);
            continue;
        };
        let stmts: Vec<String> = stmts.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if stmts.is_empty() {
            out.push(l);
            continue;
        }
        let ind = " ".repeat(indent_of(&l));
        let mut groups: Vec<(String, Vec<String>)> = vec![(format!("{} {{", prefix.trim_end()), stmts)];
        let mut ok = true;
        for (h, bd) in &chain {
            match split_top(bd, ';') {
                Some(v) => {
                    let v: Vec<String> = v.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                    if v.is_empty() {
                        ok = false;
                    }
                    groups.push((format!("{}}} {} {{", ind, h), v));
                }
                None => ok = false,
            }
        }
        if !ok {
            out.push(l);
            continue;
        }
        for (h, v) in groups {
            out.push(h);
            for s in v {
                out.push(format!("{}    {}", ind, s));
            }
        }
        out.push(format!("{}}}{}", ind, rest.trim_end()));
    }
    out
}

/// E0047: `enum N: A, B(T)` / `enum N(A, B)` / `type N: f: T, g: U` / `type N(f: T)`。
fn paren_bodies(lines: Vec<String>) -> Vec<String> {
    let mut out = vec![];
    for l in lines {
        let kw = l.split_whitespace().next().unwrap_or("");
        if indent_of(&l) != 0 || !matches!(kw, "type" | "enum") {
            out.push(l);
            continue;
        }
        let rest = l[kw.len()..].trim_start();
        let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect();
        let after = rest[name.len()..].trim_start();
        let mut head = format!("{} {}", kw, name);
        let body: Option<String> = if let Some(b) = after.strip_prefix(':') {
            Some(b.to_string())
        } else if after.starts_with('(') && after.trim_end().ends_with(')') {
            Some(after.trim_end()[1..after.trim_end().len() - 1].to_string())
        } else {
            None
        };
        // `enum N: A(i32)` は `:` で始まる。`type N pub` などの後置語は決められないので触らない。
        let Some(body) = body else {
            out.push(l);
            continue;
        };
        let Some(items) = split_top_a(&body, ',', kw == "type") else {
            out.push(l);
            continue;
        };
        let items: Vec<String> = items.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let good = !items.is_empty() && items.iter().all(|s| if kw == "type" { s.contains(':') } else { !s.contains(':') });
        if !good {
            out.push(l);
            continue;
        }
        head.push(' ');
        out.push(head.trim_end().to_string());
        for s in items {
            out.push(format!("    {}", s));
        }
    }
    out
}

/// 深さ 0 (`()` `[]` `<>` `{}` の外) の空白で区切る。
fn split_ws(s: &str) -> Option<Vec<String>> {
    let (mut d, mut cur, mut out) = (0i32, String::new(), vec![]);
    for c in s.chars() {
        match c {
            '(' | '[' | '<' | '{' => d += 1,
            ')' | ']' | '>' | '}' => d -= 1,
            _ => {}
        }
        if d < 0 {
            return None;
        }
        if c.is_whitespace() && d == 0 {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if d != 0 {
        return None;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Some(out)
}

/// 本体の文面 (`f: T g: U` / `A B(T)` / `f: T, g: U`) を 1 項目ずつに分ける。
fn body_items(kw: &str, body: &str) -> Option<Vec<String>> {
    let toks = split_ws(body)?;
    let mut items: Vec<String> = vec![];
    for t in toks {
        let t = if t.ends_with(',') { t[..t.len() - 1].to_string() } else { t };
        let new = if kw == "type" { t.contains(':') } else { true };
        if new || items.is_empty() {
            items.push(t);
        } else {
            let l = items.last_mut().unwrap();
            l.push(' ');
            l.push_str(&t);
        }
    }
    if items.is_empty() || !items.iter().all(|i| if kw == "type" { i.contains(':') && !i.ends_with(':') } else { !i.contains(':') }) {
        return None;
    }
    Some(items)
}

/// 1 行の定義 (`type N derive [..] pub f: T g: U end N`) と、本体の 1 行に複数の項目 (`a: T, b: U`)、空の `derive []`。
fn one_line_defs(lines: Vec<String>) -> Vec<String> {
    let mut out = vec![];
    let mut in_def: Option<&'static str> = None;
    for l in lines {
        let kw = l.split_whitespace().next().unwrap_or("");
        if indent_of(&l) == 0 && matches!(kw, "type" | "enum" | "trait") {
            let kw: &'static str = match kw {
                "type" => "type",
                "trait" => "trait",
                _ => "enum",
            };
            let mut line = l.replace(" derive []", "");
            if kw == "trait" {
                in_def = None;
                out.push(line.trim_end().to_string());
                continue;
            }
            // 1 行に `end N` まである形
            let rest = line[kw.len()..].trim_start().to_string();
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect();
            if let Some(pos) = rest.rfind(&format!(" end {}", name)) {
                if rest[pos..].trim() == format!("end {}", name) && !name.is_empty() {
                    let mut mid = rest[name.len()..pos].trim().to_string();
                    let mut head = format!("{} {}", kw, name);
                    if mid.starts_with("derive") {
                        if let (Some(a), Some(b)) = (mid.find('['), mid.find(']')) {
                            let d = mid[a..=b].to_string();
                            head = format!("{} derive {}", head, d);
                            mid = mid[b + 1..].trim().to_string();
                        }
                    }
                    if let Some(m) = mid.strip_prefix(':') {
                        mid = m.trim().to_string();
                    }
                    if mid.starts_with("pub ") || mid == "pub" {
                        head.push_str(" pub");
                        mid = mid[3..].trim().to_string();
                    }
                    if let Some(items) = body_items(kw, &mid) {
                        out.push(head);
                        for i in items {
                            out.push(format!("    {}", i));
                        }
                        out.push(format!("end {}", name));
                        continue;
                    }
                }
            }
            line = line.trim_end().to_string();
            in_def = Some(kw);
            out.push(line);
            continue;
        }
        if indent_of(&l) == 0 && !l.trim().is_empty() {
            in_def = None;
        }
        if let (Some(kw), true) = (in_def, indent_of(&l) == 4) {
            let t = l.trim();
            if let Some(parts) = split_top_a(t, ',', kw == "type") {
                let parts: Vec<String> = parts.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
                if parts.len() > 1 && parts.iter().all(|p| if kw == "type" { p.contains(':') } else { !p.contains(':') }) {
                    for p in parts {
                        out.push(format!("    {}", p));
                    }
                    continue;
                }
            }
        }
        out.push(l);
    }
    out
}

/// E0062: `impl T for Y` の中の `fn name(self: &Self, ..)` を、`fn Y.name(..) .. impls T` の平らな関数にする。
fn impl_methods(lines: Vec<String>) -> Vec<String> {
    let mut out = vec![];
    let mut i = 0;
    while i < lines.len() {
        let l = &lines[i];
        let t = l.trim();
        let hdr = if indent_of(l) == 0 && t.starts_with("impl ") { t[5..].split_once(" for ") } else { None };
        let Some((tr, ty)) = hdr else {
            out.push(l.clone());
            i += 1;
            continue;
        };
        let (tr, ty) = (tr.trim().to_string(), ty.trim().to_string());
        let tr_plain = !tr.contains(' ') || tr.contains('<');
        // 型は 1 語の FQN (総称なし) のときだけ。
        if !tr_plain || ty.contains(|c: char| c == ' ' || c == '<' || c == '[') || !ty.contains('.') {
            out.push(l.clone());
            i += 1;
            continue;
        }
        // 本体は、`end <名前>` の行まで (最後の `end` が型の名前なら、それが閉じ)。
        let mut j = i + 1;
        while j < lines.len() && !(indent_of(&lines[j]) == 0 && !lines[j].trim().is_empty()) {
            j += 1;
        }
        let closed = j < lines.len() && lines[j].trim().starts_with("end ");
        let body = &lines[i + 1..j];
        let has_fn = body.iter().any(|b| indent_of(b) == 4 && b.trim_start().starts_with("fn "));
        let all_ok = body.iter().all(|b| b.trim().is_empty() || indent_of(b) >= 4);
        if !closed || !has_fn || !all_ok {
            out.push(l.clone());
            i += 1;
            continue;
        }
        out.push(l.clone());
        out.push(String::new());
        let mut cur: Option<String> = None;
        let mut buf: Vec<String> = vec![];
        let mut flush = |cur: &mut Option<String>, buf: &mut Vec<String>, out: &mut Vec<String>| {
            if let Some(n) = cur.take() {
                while buf.last().map_or(false, |b| b.trim().is_empty()) {
                    buf.pop();
                }
                out.append(buf);
                out.push(format!("end {}", n));
                out.push(String::new());
            }
            buf.clear();
        };
        for b in body {
            if indent_of(b) == 4 && b.trim_start().starts_with("fn ") {
                flush(&mut cur, &mut buf, &mut out);
                let sig = b.trim_start()[3..].trim_start();
                let name: String = sig.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect();
                let rest = replace_self(&sig[name.len()..], &ty);
                let full = if name.contains('.') { name.clone() } else { format!("{}.{}", ty, name) };
                cur = Some(full.clone());
                buf.push(format!("fn {}{} impls {}", full, rest.trim_end(), tr));
            } else if cur.as_ref().map_or(false, |c| indent_of(b) == 4 && (b.trim() == format!("end {}", c) || Some(b.trim()) == c.rsplit('.').next().map(|n| format!("end {}", n)).as_deref())) {
                continue;
            } else if cur.is_some() {
                let s = if b.len() >= 4 { &b[4..] } else { "" };
                buf.push(replace_self(s, &ty));
            }
        }
        flush(&mut cur, &mut buf, &mut out);
        while out.last().map_or(false, |b| b.is_empty()) {
            out.pop();
        }
        i = j + 1;
        // 重ねて書かれた閉じ (`end Y` の後の `end impl ..` など) も取る。
        while i < lines.len() && indent_of(&lines[i]) == 0 && lines[i].trim_start().starts_with("end ") && !lines[i].trim().starts_with("end fn") {
            let nxt = i + 1 < lines.len() && lines[i].contains(" for ") || lines[i].trim_start().starts_with("end impl ");
            if !nxt {
                break;
            }
            i += 1;
        }
    }
    out
}

/// 語としての `Self` を型の名前にする (`Self::` / `Self.` も含む。リテラルの中は見ない)。
fn replace_self(s: &str, ty: &str) -> String {
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '"' || cs[i] == '\'' {
            let j = skip_lit(&cs, i);
            out.extend(&cs[i..j]);
            i = j;
            continue;
        }
        let word_start = i == 0 || !(cs[i - 1].is_alphanumeric() || cs[i - 1] == '_');
        if word_start && cs[i..].starts_with(&['S', 'e', 'l', 'f']) && (i + 4 >= cs.len() || !(cs[i + 4].is_alphanumeric() || cs[i + 4] == '_')) {
            out.push_str(ty);
            i += 4;
            continue;
        }
        out.push(cs[i]);
        i += 1;
    }
    out
}

fn def_name(kw: &str, hdr: &str) -> String {
    let rest = hdr[kw.len()..].trim_start();
    let rest = if kw == "impl" { rest.rsplit_once(" for ").map_or(rest, |(_, y)| y.trim()) } else { rest };
    rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect()
}

/// E0043: 定義の `{ ... }` (`fn f(..) -> T {` ... `}`) を、波括弧なし・`end <名前>` で閉じる形に。
/// 閉じの `}` を `end` にするのは最上位の `}` だけ。`impl` は `end <型>` (impl_methods が続けて平らにする)。
fn brace_defs(lines: Vec<String>) -> Vec<String> {
    let mut out = vec![];
    // 外側 (最上位) の定義と、`impl` / `trait` の中のメソッド (字下げ 4) の開き。
    let mut open: Option<(String, bool, bool)> = None;
    let mut inner = false;
    for l in lines {
        let t = l.trim_end();
        let kw = t.split_whitespace().next().unwrap_or("");
        if indent_of(t) == 0 && open.is_none() && matches!(kw, "fn" | "trait" | "type" | "enum" | "impl" | "test") && t.ends_with(" {") {
            let name = def_name(kw, t);
            if !name.is_empty() {
                open = Some((name, matches!(kw, "type" | "enum"), matches!(kw, "impl" | "trait")));
                out.push(t[..t.len() - 2].trim_end().to_string());
                continue;
            }
        }
        if let Some((name, fields, methods)) = &open {
            if *methods && !inner && indent_of(t) == 4 && kw == "fn" && t.ends_with(" {") {
                inner = true;
                out.push(t[..t.len() - 2].trim_end().to_string());
                continue;
            }
            if inner && t == "    }" {
                inner = false;
                continue;
            }
            if t == "}" && !inner {
                out.push(format!("end {}", name));
                open = None;
                continue;
            }
            if *fields && indent_of(t) == 4 && t.ends_with(',') {
                out.push(t[..t.len() - 1].to_string());
                continue;
            }
        }
        out.push(l);
    }
    out
}

/// 単独の `end` を、直前に開いた定義の名前つきの `end <名前>` にする。
fn bare_end(lines: Vec<String>) -> Vec<String> {
    let mut out = vec![];
    let mut open: Option<String> = None;
    let mut imp: Option<String> = None;
    for l in lines {
        let kw = l.split_whitespace().next().unwrap_or("");
        if imp.is_some() && indent_of(&l) == 4 && l.trim() == "end" {
            continue;
        }
        if indent_of(&l) == 0 && !l.trim().is_empty() {
            if kw == "impl" {
                imp = Some(def_name("impl", l.trim_end())).filter(|n| !n.is_empty());
            } else if l.trim() == "end" {
                if let Some(n) = imp.take() {
                    out.push(format!("end {}", n));
                    continue;
                }
            } else {
                imp = None;
            }
        }
        if indent_of(&l) == 0 {
            if matches!(kw, "fn" | "trait" | "type" | "enum" | "test") {
                let n = def_name(kw, l.trim_end());
                open = if n.is_empty() { None } else { Some(n) };
            } else if l.trim() == "end" {
                if let Some(n) = open.take() {
                    out.push(format!("end {}", n));
                    continue;
                }
            } else if kw == "end" {
                open = None;
            }
        }
        out.push(l);
    }
    out
}

/// `trait N[..]: fn m(..)` (見出しと最初のメソッドが同じ行) を 2 行に分ける。
fn trait_header_colon(lines: Vec<String>) -> Vec<String> {
    let mut out = vec![];
    for l in lines {
        if indent_of(&l) == 0 && l.starts_with("trait ") {
            if let Some(p) = l.find(": fn ") {
                let head = &l[..p];
                if split_top(head, '\u{0}').is_some() {
                    out.push(head.trim_end().to_string());
                    out.push(format!("    {}", l[p + 2..].trim()));
                    continue;
                }
            }
        }
        if indent_of(&l) == 4 && l.trim_start().starts_with("fn ") {
            if let Some(parts) = split_top(l.trim(), ';') {
                let parts: Vec<String> = parts.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
                if parts.len() > 1 && parts.iter().all(|p| p.starts_with("fn ")) {
                    for p in parts {
                        out.push(format!("    {}", p));
                    }
                    continue;
                }
            }
        }
        out.push(l);
    }
    out
}

/// 文の末尾の `,` (`x = expr,`): 代入の行だけ消す。
fn stmt_commas(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|l| {
            let t = l.trim();
            if t.ends_with(',') && !t.starts_with("//") && balanced(t) {
                if let Some((lhs, _)) = t.split_once(" = ") {
                    if !lhs.is_empty() && lhs.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') && !lhs.contains(char::is_whitespace) && !t.contains("=>") {
                        return l.trim_end().trim_end_matches(',').to_string();
                    }
                }
            }
            l
        })
        .collect()
}

/// `type` / `enum` / `trait` の定義が `end <名前>` で閉じていなければ、本体の終わりに足す。
fn ensure_end(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut open: Option<String> = None;
    let close = |open: &mut Option<String>, out: &mut Vec<String>| {
        if let Some(n) = open.take() {
            let at = out.iter().rposition(|x| !x.trim().is_empty()).map_or(0, |k| k + 1);
            out.insert(at, format!("end {}", n));
        }
    };
    for l in lines {
        if indent_of(&l) == 0 && !l.trim().is_empty() {
            let kw = l.split_whitespace().next().unwrap_or("");
            if kw == "end" {
                open = None;
            } else {
                close(&mut open, &mut out);
                if matches!(kw, "type" | "enum" | "trait") {
                    let n = def_name(kw, l.trim_end());
                    if !n.is_empty() {
                        open = Some(n);
                    }
                }
            }
        }
        out.push(l);
    }
    close(&mut open, &mut out);
    out
}

/// 定義の見出しの末尾の `:` (`trait N[..]:` / `type N:`)、`' static` (アポストロフィの後の空白)。
fn header_colon(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|l| {
            let l = if l.contains("' static") && !l.contains('"') { l.replace("' static", "'static") } else { l };
            let kw = l.split_whitespace().next().unwrap_or("").to_string();
            let kw = kw.as_str();
            if indent_of(&l) == 0 && matches!(kw, "trait" | "type" | "enum") && l.trim_end().ends_with(':') {
                return l.trim_end().trim_end_matches(':').to_string();
            }
            l
        })
        .collect()
}

/// E0061: 構造体リテラル・パターンの欄の省略形 `T { x, y }` → `T { x: x, y: y }` (同じ行に閉じる形だけ)。
fn field_shorthand(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|l| {
            let t = l.trim_start();
            let first = t.split_whitespace().next().unwrap_or("");
            if t.starts_with("//") || matches!(first, "if" | "while" | "for" | "match" | "else" | "loop" | "with" | "unsafe" | "fn" | "impl" | "trait" | "type" | "enum") || t.starts_with('}') {
                return l;
            }
            let cs: Vec<char> = l.chars().collect();
            let Some((a, b)) = first_brace(&cs) else { return l };
            let before: String = cs[..a].iter().collect();
            let bt = before.trim_end();
            if !last_ident(bt).chars().next().map_or(false, |c| c.is_uppercase()) || before.contains(" if ") || before.contains(" match ") || before.contains(" while ") {
                return l;
            }
            let inner: String = cs[a + 1..b].iter().collect();
            let Some(items) = split_top(&inner, ',') else { return l };
            let items: Vec<String> = items.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let bare = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_') && s.chars().next().map_or(false, |c| c.is_lowercase() || c == '_');
            if items.is_empty() || !items.iter().any(|s| bare(s)) || !items.iter().all(|s| bare(s) || (s.contains(':') && !s.starts_with("..") && !s.contains(" if "))) {
                return l;
            }
            let new: Vec<String> = items.iter().map(|s| if bare(s) { format!("{}: {}", s, s) } else { s.clone() }).collect();
            let rest: String = cs[b + 1..].iter().collect();
            format!("{}{{ {} }}{}", before, new.join(", "), rest)
        })
        .collect()
}

/// 空の `impl T for Y` に付いた `end T for Y` を外す。
fn empty_impl_end(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for l in lines {
        if indent_of(&l) == 0 && l.starts_with("end ") && l.contains(" for ") {
            if let Some(prev) = out.iter().rev().find(|x| !x.trim().is_empty()) {
                if prev.starts_with("impl ") && prev[5..].trim() == l[4..].trim() {
                    continue;
                }
            }
        }
        out.push(l);
    }
    out
}

/// 生の文面にすべての表面の書き換えを、動かなくなるまで当てる。
pub fn rewrite(text: &str) -> String {
    let mut cur = text.to_string();
    for _ in 0..50 {
        let mut lines: Vec<String> = cur.split('\n').map(String::from).collect();
        lines = header_colon(lines);
        lines = empty_impl_end(lines);
        lines = field_shorthand(lines);
        lines = brace_defs(lines);
        lines = bare_end(lines);
        lines = trait_header_colon(lines);
        lines = impl_methods(lines);
        lines = stmt_commas(lines);
        lines = one_line_defs(lines);
        lines = paren_bodies(lines);
        lines = inline_blocks(lines);
        lines = ensure_end(lines);
        let next = lines.join("\n");
        if next == cur {
            break;
        }
        cur = next;
    }
    cur
}
