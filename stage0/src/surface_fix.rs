//! `ksn fix` の前段: 構文として読めないファイルでも当てられる、生の文面の書き換え。
//! Rust 風の表面の形 (E0056 1 行ブロック・E0053 構造体リテラルの折り返し・E0047 `enum X: A, B` /
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
        let rest: String = cs[b + 1..].iter().collect();
        let ptrim = prefix.trim_end();
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
            if ptrim.ends_with("=>") {
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
        out.push(format!("{}{{", prefix.trim_end().to_string() + " "));
        for s in stmts {
            out.push(format!("{}    {}", ind, s));
        }
        out.push(format!("{}}}{}", ind, rest.trim_end()));
    }
    out
}

/// E0053: `Name {` で終わる行から `}` だけの行までを 1 行に畳む。
fn wrapped_literals(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut i = 0;
    while i < lines.len() {
        let l = &lines[i];
        let t = l.trim();
        let first = t.split_whitespace().next().unwrap_or("");
        let lit = t.ends_with('{')
            && !t.starts_with("//")
            && !matches!(first, "if" | "while" | "for" | "loop" | "match" | "else" | "fn" | "impl" | "trait" | "enum" | "type" | "unsafe" | "with")
            && last_ident(&t[..t.len() - 1]).chars().next().map_or(false, |c| c.is_uppercase());
        if lit {
            let ind = indent_of(l);
            let mut j = i + 1;
            let mut fields: Vec<String> = vec![];
            let mut ok = false;
            while j < lines.len() {
                let u = lines[j].trim();
                if indent_of(&lines[j]) == ind && (u == "}" || u == "}," || u.starts_with("}")) {
                    ok = u == "}" || u == "},";
                    break;
                }
                if indent_of(&lines[j]) <= ind || u.starts_with("//") {
                    break;
                }
                fields.push(u.trim_end_matches(',').to_string());
                j += 1;
            }
            if ok && !fields.is_empty() && fields.iter().all(|f| balanced(f) && f.contains(':') && !f.is_empty()) {
                let tail = lines[j].trim().trim_start_matches('}').to_string();
                out.push(format!("{} {} }}{}", l.trim_end(), fields.join(", "), tail));
                i = j + 1;
                continue;
            }
        }
        out.push(l.clone());
        i += 1;
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
        let Some(items) = split_top(&body, ',') else {
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
                out.push(format!("end {}.{}", ty, n));
                out.push(String::new());
            }
            buf.clear();
        };
        for b in body {
            if indent_of(b) == 4 && b.trim_start().starts_with("fn ") {
                flush(&mut cur, &mut buf, &mut out);
                let sig = b.trim_start()[3..].trim_start();
                let name: String = sig.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                let rest = replace_self(&sig[name.len()..], &ty);
                cur = Some(name.clone());
                buf.push(format!("fn {}.{}{} impls {}", ty, name, rest.trim_end(), tr));
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

/// 生の文面にすべての表面の書き換えを、動かなくなるまで当てる。
pub fn rewrite(text: &str) -> String {
    let mut cur = text.to_string();
    for _ in 0..50 {
        let mut lines: Vec<String> = cur.split('\n').map(String::from).collect();
        lines = impl_methods(lines);
        lines = paren_bodies(lines);
        lines = wrapped_literals(lines);
        lines = inline_blocks(lines);
        let next = lines.join("\n");
        if next == cur {
            break;
        }
        cur = next;
    }
    cur
}
