//! lint (stage0)。Python 参照実装 spec/tools/ksnlint.py の移植。
//! FQN とパスの一致・命名・定義順・空行・属性の並び・インデント・未使用・pub の伝播を検査する。

use std::collections::HashMap;

use regex::Regex;

pub type LintErr = (String, usize, String);

const GROUP_IMPL_ONLY: usize = 4;
const DERIVE_ORDER: &[&str] = &["Debug", "Clone", "Copy", "PartialEq", "Eq", "PartialOrd", "Ord", "Hash"];
const PRIMS: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "bool",
    "char", "str",
];

fn group(kw: &str) -> usize {
    match kw {
        "import" => 0,
        "const" | "static" => 1,
        "type" | "enum" | "trait" => 2,
        "extern" => 3,
        "impl_only" => 4,
        "fn" => 5,
        _ => 6,
    }
}

fn is_field_kind(k: &str) -> bool {
    matches!(k, "type" | "enum" | "trait")
}
fn is_body_kind(k: &str) -> bool {
    matches!(k, "fn" | "test")
}

#[derive(Clone, Default)]
struct Item {
    kw: String,
    line: usize,
    start: usize,
    text: String,
    name: String,
    end: usize,
    body: Vec<String>,
    body_start: usize,
    attached: bool,
}

fn word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `(?<![A-Za-z0-9_.])nm(?![A-Za-z0-9_])` の出現回数。
fn count_word(text: &str, nm: &str) -> usize {
    if nm.is_empty() {
        return 0;
    }
    let mut n = 0;
    for (i, _) in text.match_indices(nm) {
        let before = text[..i].chars().next_back();
        let after = text[i + nm.len()..].chars().next();
        if before.map_or(true, |c| !(word_char(c) || c == '.')) && after.map_or(true, |c| !word_char(c)) {
            n += 1;
        }
    }
    n
}

fn is_snake(s: &str) -> bool {
    // _?[a-z][a-z0-9]*(?:_[a-z][a-z0-9]*)*
    let s = s.strip_prefix('_').unwrap_or(s);
    if s.is_empty() {
        return false;
    }
    s.split('_').all(|w| {
        let mut cs = w.chars();
        matches!(cs.next(), Some(c) if c.is_ascii_lowercase()) && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    })
}
fn is_upper_camel(s: &str) -> bool {
    // [A-Z][a-z0-9]*(?:[A-Z][a-z0-9]*)*
    let mut it = s.chars().peekable();
    if !matches!(it.peek(), Some(c) if c.is_ascii_uppercase()) {
        return false;
    }
    while let Some(c) = it.next() {
        if !(c.is_ascii_uppercase() || c.is_ascii_lowercase() || c.is_ascii_digit()) {
            return false;
        }
        let _ = c;
    }
    // 先頭以外の語は大文字で始まり小文字/数字が続く。大文字連続は別途検査される。
    s.chars().all(|c| c.is_ascii_alphanumeric())
}
fn is_screaming(s: &str) -> bool {
    // [A-Z][A-Z0-9]*(?:_[A-Z][A-Z0-9]*)*
    !s.is_empty()
        && s.split('_').all(|w| {
            let mut cs = w.chars();
            matches!(cs.next(), Some(c) if c.is_ascii_uppercase()) && cs.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        })
}
fn two_upper(s: &str) -> bool {
    let cs: Vec<char> = s.chars().collect();
    cs.windows(2).any(|w| w[0].is_ascii_uppercase() && w[1].is_ascii_uppercase())
}

fn split_top(s: &str, sep: char) -> Vec<String> {
    let mut out = vec![];
    let mut d = 0i32;
    let mut cur = String::new();
    for ch in s.chars() {
        if "[(<{".contains(ch) {
            d += 1;
        } else if "])>}".contains(ch) {
            d -= 1;
        }
        if ch == sep && d == 0 {
            out.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn bracket(s: &str, start: usize) -> String {
    let mut d = 0;
    for (i, ch) in s[start..].char_indices() {
        if ch == '[' {
            d += 1;
        } else if ch == ']' {
            d -= 1;
            if d == 0 {
                return s[start + 1..start + i].to_string();
            }
        }
    }
    String::new()
}

fn strip_args(s: &str) -> String {
    let mut out = String::new();
    let mut d = 0i32;
    for ch in s.chars() {
        if "<([".contains(ch) {
            d += 1;
        } else if ">)]".contains(ch) {
            d -= 1;
        } else if d == 0 {
            out.push(ch);
        }
    }
    out
}

fn first_word_lower(e: &str) -> bool {
    e.chars().next().map_or(false, |c| c.is_lowercase())
}
fn first_word_upper(e: &str) -> bool {
    e.chars().next().map_or(false, |c| c.is_uppercase())
}

pub struct Lint {
    module: String,
    sig: bool,
    lines: Vec<String>,
    errs: Vec<LintErr>,
}

impl Lint {
    /// `rel` は <root> からの相対パス ("app/net.ksnh")。
    pub fn new(rel: &str, text: &str) -> Lint {
        let sig = rel.ends_with(".sig");
        let module = {
            let m = rel.trim_end_matches(".ksnh").trim_end_matches(".ksnl").trim_end_matches(".sig");
            m.replace('/', ".")
        };
        let mut lines: Vec<String> = text.split('\n').map(String::from).collect();
        if lines.last().map_or(false, |l| l.is_empty()) {
            lines.pop();
        }
        Lint { module, sig, lines, errs: vec![] }
    }

    fn err(&mut self, code: &str, n: usize, msg: String) {
        self.errs.push((code.to_string(), n, msg));
    }

    fn items(&mut self) -> Vec<Item> {
        let re_kw = Regex::new(r"^(import|const|static|extern|type|enum|trait|impl|fn|test|end)\b").unwrap();
        let re_name = Regex::new(r"^\w+\s+([A-Za-z0-9_.]+)").unwrap();
        let l = self.lines.clone();
        let mut items = vec![];
        let mut i = 0;
        let mut lead_start: Option<usize> = None;
        while i < l.len() {
            let line = &l[i];
            if line.is_empty() {
                lead_start = None;
                i += 1;
                continue;
            }
            if line.starts_with("//") {
                if lead_start.is_none() {
                    lead_start = Some(i);
                }
                i += 1;
                continue;
            }
            let m = re_kw.captures(line);
            if m.is_none() || line.starts_with(' ') {
                let head: String = line.chars().take(30).collect();
                self.err("indent", i + 1, format!("top-level line not at column 0 or unknown: {}", crate::grammar::py_repr(&head)));
                i += 1;
                lead_start = None;
                continue;
            }
            let kw = m.unwrap().get(1).unwrap().as_str().to_string();
            let start = lead_start.unwrap_or(i);
            let mut it = Item { kw: kw.clone(), line: i, start, text: line.clone(), ..Default::default() };
            it.name = match kw.as_str() {
                "import" => line.split_whitespace().nth(1).unwrap_or("").to_string(),
                "impl" => line.clone(),
                _ => re_name.captures(line).map_or(String::new(), |c| c[1].to_string()),
            };
            if is_field_kind(&kw) || (is_body_kind(&kw) && !(self.sig && kw == "fn")) {
                let mut j = i + 1;
                while j < l.len() && !l[j].starts_with("end ") {
                    j += 1;
                }
                if j >= l.len() {
                    self.err("fqn", i + 1, "missing end".into());
                    j = l.len() - 1;
                } else {
                    let endname = l[j].split_whitespace().nth(1).unwrap_or("").to_string();
                    if endname != it.name {
                        self.err("fqn", j + 1, format!("end {} != header {}", endname, it.name));
                    }
                }
                it.end = j;
                it.body = l[i + 1..j.max(i + 1)].to_vec();
                it.body_start = i + 1;
                i = j + 1;
            } else {
                it.end = i;
                i += 1;
            }
            items.push(it);
            lead_start = None;
        }
        items
    }

    fn check_fqn_and_names(&mut self, items: &[Item]) {
        let pref = format!("{}.", self.module);
        for it in items {
            let (kw, name, n) = (it.kw.as_str(), &it.name, it.line + 1);
            if kw == "import" || kw == "impl" {
                continue;
            }
            if !name.starts_with(&pref) {
                self.err("fqn", n, format!("{} does not start with module {}.", name, self.module));
                continue;
            }
            let rest: Vec<&str> = name[pref.len()..].split('.').collect();
            match kw {
                "const" | "static" => {
                    if !is_screaming(rest[rest.len() - 1]) {
                        self.err("name", n, format!("{} not SCREAMING_SNAKE_CASE", rest[rest.len() - 1]));
                    }
                }
                "type" | "enum" | "trait" => {
                    if rest.len() != 1 || !is_upper_camel(rest[0]) || two_upper(rest[0]) {
                        self.err("name", n, format!("{} not UpperCamelCase (no consecutive capitals)", py_list(&rest)));
                    }
                }
                "test" => {
                    if !rest[rest.len() - 1].starts_with("test_") {
                        self.err("name", n, "test name must start with test_".into());
                    }
                }
                _ => {
                    for c in rest {
                        if PRIMS.contains(&c) || c == "blanket" {
                            continue;
                        }
                        if c.chars().next().map_or(false, |x| x.is_uppercase()) {
                            if !is_upper_camel(c) || two_upper(c) {
                                self.err("name", n, format!("{} not UpperCamelCase", c));
                            }
                        } else if !is_snake(c) {
                            self.err("name", n, format!("{} not snake_case", c));
                        }
                    }
                }
            }
        }
    }

    fn check_order(&mut self, items: &mut Vec<Item>) {
        let re_impls = Regex::new(r" impls (.+?)(?: pub)?$").unwrap();
        let re_gen = Regex::new(r"^impl(\[(?:[^\[\]]|\[[^\]]*\])*\])?\s+").unwrap();
        let mut prev_g: i64 = -1;
        let mut prev_key: Option<String> = None;
        let mut entries: Vec<(usize, String, usize)> = vec![];
        for idx in 0..items.len() {
            let kw = items[idx].kw.clone();
            if kw == "end" {
                continue;
            }
            let (g, key);
            if kw == "impl" {
                let attached = items.get(idx + 1).map_or(false, |n| n.kw == "fn" && n.text.contains(" impls "));
                items[idx].attached = attached;
                if attached {
                    continue;
                }
                g = GROUP_IMPL_ONLY;
                key = items[idx].text.clone();
            } else if kw == "fn" && idx > 0 && items[idx - 1].kw == "impl" && items[idx - 1].attached {
                g = group("fn");
                key = items[idx].name.clone();
            } else {
                g = group(&kw);
                key = items[idx].name.clone();
            }
            entries.push((g, key, items[idx].line + 1));
        }
        let mut first_seen: Vec<(String, String)> = vec![];
        for idx in 0..items.len() {
            if items[idx].kw != "fn" {
                continue;
            }
            let Some(mi) = re_impls.captures(&items[idx].text) else { continue };
            let tr = mi[1].to_string();
            let owner = items[idx].name.rsplit_once('.').map_or(items[idx].name.clone(), |x| x.0.to_string());
            let key = (tr.clone(), owner.clone());
            if first_seen.contains(&key) {
                continue;
            }
            first_seen.push(key);
            let mut ok = false;
            if idx > 0 && items[idx - 1].kw == "impl" {
                let prev = &items[idx - 1].text;
                let body = re_gen.replace(prev, "").to_string();
                if let Some((trait_, target)) = body.split_once(" for ") {
                    let target = target.trim();
                    let tg = target.split(|c| c == '<' || c == '[' || c == ' ').next().unwrap_or("");
                    let last = tg.rsplit('.').next().unwrap_or("");
                    ok = trait_.trim() == tr && (owner == tg || owner.ends_with(&format!(".{}", last)));
                }
            }
            if !ok {
                let n = items[idx].line + 1;
                self.err("order", n, format!("first method of impl {} for {} must be directly preceded by its impl line", tr, owner));
            }
        }
        for (g, key, n) in entries {
            if (g as i64) < prev_g {
                self.err("order", n, format!("group order violated at {}", key));
            } else if g as i64 == prev_g && prev_key.as_ref().map_or(false, |p| key < *p) {
                let pk = prev_key.clone().unwrap();
                self.err("order", n, format!("{} should come before {} (UTF-8 byte order)", key, pk));
            }
            prev_g = g as i64;
            prev_key = Some(key);
        }
    }

    fn check_blank(&mut self, items: &[Item]) {
        if self.sig {
            return;
        }
        for k in 1..items.len() {
            let (a, b) = (&items[k - 1], &items[k]);
            let gap = b.start as i64 - a.end as i64 - 1;
            let both_imports = a.kw == "import" && b.kw == "import";
            let impl_attach = a.kw == "impl" && b.kw == "fn" && b.text.contains(" impls ");
            if both_imports || impl_attach {
                if gap != 0 {
                    self.err("blank", b.line + 1, "no blank line allowed here".into());
                }
            } else if gap != 1 {
                self.err("blank", b.line + 1, format!("exactly one blank line required (found {})", gap));
            }
        }
        for n in 0..self.lines.len().saturating_sub(1) {
            if self.lines[n].starts_with("//") && self.lines[n + 1].is_empty() {
                self.err("blank", n + 2, "blank line after comment".into());
            }
        }
    }

    fn check_attrs(&mut self, items: &[Item]) {
        let re_uses = Regex::new(r"\buses \[").unwrap();
        let re_derive = Regex::new(r"\bderive \[([^\]]*)\]").unwrap();
        let re_req = Regex::new(r"\brequires (.*?)(?: pub)?$").unwrap();
        let re_gen = Regex::new(r"^(?:fn|type|enum|trait)\s+[A-Za-z0-9_.]+\[").unwrap();
        for it in items {
            let (text, n) = (&it.text, it.line + 1);
            for m in re_uses.find_iter(text) {
                let inner = bracket(text, m.end() - 1);
                let els = split_top(&inner, ',');
                let mut conc: Vec<String> = els.iter().filter(|e| first_word_lower(e)).cloned().collect();
                let var: Vec<String> = els.iter().filter(|e| first_word_upper(e)).cloned().collect();
                let mut expect = conc.clone();
                expect.sort();
                expect.extend(var);
                if els != expect {
                    self.err("order", n, format!("uses list order: {}", py_str_list(&els)));
                }
                conc.sort();
                for a in &conc {
                    for b in &conc {
                        if a != b && b.starts_with(&format!("{}.", a)) {
                            self.err("attr", n, format!("redundant uses: {} includes {}", a, b));
                        }
                    }
                }
                let mut seen = els.clone();
                seen.sort();
                seen.dedup();
                if seen.len() != els.len() {
                    self.err("attr", n, "duplicate uses element".into());
                }
            }
            if let Some(m) = re_derive.captures(text) {
                let ds: Vec<String> = m[1].split(',').map(|d| d.trim().to_string()).collect();
                let idx: Vec<usize> = ds.iter().filter_map(|d| DERIVE_ORDER.iter().position(|x| x == d)).collect();
                let mut sorted = idx.clone();
                sorted.sort();
                if idx != sorted || idx.len() != ds.len() {
                    self.err("order", n, format!("derive order: {}", py_str_list(&ds)));
                }
            }
            if it.kw == "trait" {
                if let Some(m) = re_req.captures(text) {
                    let rs = split_top(&m[1], ',');
                    let mut s = rs.clone();
                    s.sort();
                    if rs != s {
                        self.err("order", n, format!("requires order: {}", py_str_list(&rs)));
                    }
                }
            }
            let gen: Option<String> = if text.starts_with("impl[") {
                Some(bracket(text, 4))
            } else {
                re_gen.find(text).map(|m| bracket(text, m.end() - 1))
            };
            if let Some(gen) = gen {
                for ent in split_top(&gen, ',') {
                    let Some((_, b)) = ent.split_once(':') else { continue };
                    let bound = b.trim();
                    if bound == "uses" || bound == "panics" || !strip_args(bound).contains('+') {
                        continue;
                    }
                    let parts: Vec<String> = split_top(bound, '+').iter().map(|p| p.trim().to_string()).collect();
                    let rank = |p: &String| -> (u8, String) {
                        if p == "linear" {
                            (0, String::new())
                        } else if p.starts_with('\'') {
                            (2, p.clone())
                        } else {
                            (1, p.clone())
                        }
                    };
                    let mut sorted = parts.clone();
                    sorted.sort_by_key(|p| rank(p));
                    if parts != sorted {
                        self.err("order", n, format!("bound order: {}", py_str_list(&parts)));
                    }
                }
            }
        }
        let re_cap = Regex::new(r"\bcapture \[([^\]]*)\]").unwrap();
        for k in 0..self.lines.len() {
            let l = self.lines[k].clone();
            if let Some(m) = re_cap.captures(&l) {
                if m[1].trim().is_empty() {
                    continue;
                }
                let names: Vec<String> = m[1]
                    .split(',')
                    .map(|c| {
                        let c = c.trim();
                        for p in ["&mut ", "&", "move "] {
                            if let Some(r) = c.strip_prefix(p) {
                                return r.to_string();
                            }
                        }
                        c.to_string()
                    })
                    .collect();
                let mut s = names.clone();
                s.sort();
                if names != s {
                    self.err("order", k + 1, format!("capture order: {}", py_str_list(&names)));
                }
            }
        }
    }

    fn check_indent(&mut self, items: &[Item]) {
        for it in items {
            if is_field_kind(&it.kw) {
                for (k, l) in it.body.iter().enumerate() {
                    if !l.is_empty() && !(l.starts_with("    ") && !l.starts_with("     ")) {
                        self.err("indent", it.body_start + k + 1, "declaration line must be indented by exactly 4".into());
                    }
                }
                continue;
            }
            if !is_body_kind(&it.kw) || (self.sig && it.kw == "fn") {
                continue;
            }
            let mut stack: Vec<(&str, usize)> = vec![];
            let mut stmt_base = 0usize;
            for (k, l) in it.body.iter().enumerate() {
                if l.is_empty() {
                    continue;
                }
                let n = it.body_start + k + 1;
                let ind = l.len() - l.trim_start_matches(' ').len();
                let s = l.trim();
                let mut errs: Vec<String> = vec![];
                let expect = |e: usize, errs: &mut Vec<String>| {
                    if ind != e {
                        errs.push(format!("indent {}, expected {}", ind, e));
                    }
                };
                if ind % 4 != 0 {
                    self.err("indent", n, format!("indent {} not multiple of 4", ind));
                }
                let first = s.chars().next().unwrap();
                if s.starts_with("//") {
                    expect(stack.last().map_or(4, |t| t.1 + 4), &mut errs);
                } else if first == '}' {
                    if stack.last().map_or(true, |t| t.0 != "brace") {
                        errs.push("unbalanced }".into());
                    } else {
                        expect(stack.last().unwrap().1, &mut errs);
                        stack.pop();
                    }
                    if s.ends_with('{') {
                        stack.push(("brace", ind));
                    }
                } else if first == ')' || first == ']' {
                    if stack.last().map_or(true, |t| t.0 != "wrap") {
                        errs.push("unbalanced closing bracket".into());
                    } else {
                        expect(stack.last().unwrap().1, &mut errs);
                        stack.pop();
                    }
                } else if s.starts_with('.') {
                    expect(stmt_base + 4, &mut errs);
                    if s.ends_with('(') || s.ends_with('[') {
                        stack.push(("wrap", ind));
                    }
                } else {
                    let in_wrap = stack.last().map_or(false, |t| t.0 == "wrap");
                    expect(stack.last().map_or(4, |t| t.1 + 4), &mut errs);
                    if !in_wrap {
                        stmt_base = ind;
                    }
                    if s.ends_with('{') {
                        stack.push(("brace", ind));
                    } else if s.ends_with('(') || s.ends_with('[') {
                        stack.push(("wrap", ind));
                    }
                }
                for e in errs {
                    self.err("indent", n, e);
                }
            }
            if !stack.is_empty() {
                self.err("indent", it.end + 1, "unclosed block/bracket in body".into());
            }
        }
    }

    fn idents_declared(&self, it: &Item) -> (Vec<(&'static str, String)>, String) {
        let mut names: Vec<(&'static str, String)> = vec![];
        let head = &it.text;
        if is_body_kind(&it.kw) {
            let re_h = Regex::new(r"^(?:fn|test)\s+[A-Za-z0-9_.]+").unwrap();
            let mut pos = re_h.find(head).map_or(0, |m| m.end());
            if head[pos..].starts_with('[') {
                let mut d = 0;
                for (i, ch) in head[pos..].char_indices() {
                    if ch == '[' {
                        d += 1;
                    } else if ch == ']' {
                        d -= 1;
                        if d == 0 {
                            pos += i + 1;
                            break;
                        }
                    }
                }
            }
            if let Some(off) = head[pos..].find('(') {
                let i = pos + off;
                let mut depth = 0;
                let mut j = head.len();
                for (k, ch) in head[i..].char_indices() {
                    if ch == '(' {
                        depth += 1;
                    } else if ch == ')' {
                        depth -= 1;
                        if depth == 0 {
                            j = i + k;
                            break;
                        }
                    }
                }
                for p in split_top(&head[i + 1..j], ',') {
                    let nm = p.split(':').next().unwrap().trim().to_string();
                    if !nm.is_empty() && nm != "self" {
                        names.push(("param", nm));
                    }
                }
            }
        }
        let body = it.body.join("\n");
        let re_let = Regex::new(r"\blet (?:mut )?([a-z_][A-Za-z0-9_]*)\b").unwrap();
        for m in re_let.captures_iter(&body) {
            let end = m.get(0).unwrap().end();
            let rest = body[end..].trim_start();
            // `let t.L { .. }` / `let t.V(..)` のパターン先頭のパス成分（`t`）は束縛ではない。
            if rest.starts_with('{') || rest.starts_with('.') {
                continue;
            }
            names.push(("let", m[1].to_string()));
        }
        let re_tup = Regex::new(r"\blet \(([^)]*)\)").unwrap();
        for m in re_tup.captures_iter(&body) {
            for nm in m[1].split(',') {
                let nm = nm.trim();
                if !nm.is_empty() {
                    names.push(("let", nm.to_string()));
                }
            }
        }
        let re_st = Regex::new(r"\blet [A-Za-z0-9_.]+ \{([^}]*)\}").unwrap();
        for m in re_st.captures_iter(&body) {
            for f in m[1].split(',') {
                // フィールドの束縛先が単純な識別子のときだけ束縛とみなす（入れ子パターン `a: t.V(x)`・
                // リテラル `a: 1u8` の文字列全体を名前として数えない）。
                if let Some((_, v)) = f.split_once(':') {
                    let v = v.split(':').next().unwrap().trim();
                    if !v.is_empty() && v.chars().all(word_char) && !v.starts_with(|c: char| c.is_ascii_digit()) {
                        names.push(("let", v.to_string()));
                    }
                }
            }
        }
        // `let t.V(x, _) = ..` のバリアント分解: 引数のうち単純な識別子 (小文字始まり) だけを束縛とみなす。
        // 入れ子のパターン `t.W(..)`・リテラル・大文字の名前は数えない (黙る)。
        let re_letv = Regex::new(r"\blet ([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)\(").unwrap();
        for m in re_letv.captures_iter(&body) {
            let open = m.get(0).unwrap().end() - 1;
            let mut depth = 0i32;
            let mut close = None;
            for (k, ch) in body[open..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(open + k);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(close) = close else { continue };
            for arg in split_top(&body[open + 1..close], ',') {
                let plain = arg.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
                    && arg.chars().all(word_char)
                    && arg != "_";
                if plain {
                    names.push(("let", arg));
                }
            }
        }
        let re_fn = Regex::new(r"\bfn\(([^)]*)\)").unwrap();
        for m in re_fn.captures_iter(&body) {
            for p in split_top(&m[1], ',') {
                let nm = p.split(':').next().unwrap().trim().to_string();
                if !nm.is_empty() {
                    names.push(("param", nm));
                }
            }
        }
        let re_for = Regex::new(r"\bfor ([a-z_][a-z0-9_]*) in\b").unwrap();
        for m in re_for.captures_iter(&body) {
            names.push(("let", m[1].to_string()));
        }
        // 構造体パターンのフィールドラベル (`b` in `let t.L { b: b } = l`) は変数の使用ではない。
        // 使用回数を数える前に空白へ置き換え、束縛名と同じラベルを使用として数えないようにする。
        // (置換は ASCII の `{` `,` `:` の位置だけで行うので UTF-8 の境界は壊れない。)
        let mut bytes = body.clone().into_bytes();
        for m in re_st.captures_iter(&body) {
            let g = m.get(1).unwrap();
            let mut off = g.start();
            for f in g.as_str().split(',') {
                if let Some(c) = f.find(':') {
                    for b in &mut bytes[off..off + c] {
                        *b = b' ';
                    }
                }
                off += f.len() + 1;
            }
        }
        let count_body = String::from_utf8(bytes).unwrap_or_else(|_| body.clone());
        (names, count_body)
    }

    fn check_names_and_unused(&mut self, items: &[Item]) {
        let re_arm = Regex::new(r"(?m)^\s*[A-Za-z0-9_.]+\(([^)]*)\)\s*=>(.*)$").unwrap();
        for it in items {
            let n = it.line + 1;
            if it.kw == "type" {
                for (k, l) in it.body.iter().enumerate() {
                    let nm = l.trim().split(':').next().unwrap();
                    if !is_snake(nm) {
                        self.err("name", it.body_start + k + 1, format!("field {} not snake_case", nm));
                    }
                }
            }
            if !is_body_kind(&it.kw) || (self.sig && it.kw == "fn") {
                continue;
            }
            let (names, body) = self.idents_declared(it);
            for (kind, nm) in names {
                if nm == "_" {
                    continue;
                }
                if !is_snake(&nm) {
                    self.err("name", n, format!("{} {} not snake_case", kind, nm));
                    continue;
                }
                let cnt = count_word(&body, &nm);
                if kind == "param" {
                    if nm.starts_with('_') {
                        if cnt > 0 {
                            self.err("unused", n, format!("{} is marked unused but used", nm));
                        }
                    } else if cnt == 0 {
                        self.err("unused", n, format!("unused parameter {}", nm));
                    }
                } else if nm.starts_with('_') {
                    if cnt > 1 {
                        self.err("unused", n, format!("{} is marked unused but used", nm));
                    }
                } else if cnt < 2 {
                    self.err("unused", n, format!("unused variable {}", nm));
                }
            }
            for m in re_arm.captures_iter(&body) {
                // 腕の本体: 同じ行の残り。`{` で終わるなら対応する `}` までのブロック全体。
                let mut arm_body = m[2].to_string();
                if arm_body.trim_end().ends_with('{') {
                    let rest = &body[m.get(0).unwrap().end()..];
                    let mut depth = 1i32;
                    let mut cut = rest.len();
                    for (i, ch) in rest.char_indices() {
                        match ch {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    cut = i;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    arm_body.push(chr_nl());
                    arm_body.push_str(&rest[..cut]);
                }
                for nm in m[1].split(',').map(|x| x.trim()) {
                    if !nm.is_empty() && nm != "_" && is_snake(nm) && !nm.starts_with('_') && count_word(&arm_body, nm) == 0 {
                        self.err("unused", n, format!("unused pattern binding {}", nm));
                    }
                }
            }
        }
    }

    fn check_imports(&mut self, items: &[Item]) {
        let text = self.lines.iter().filter(|l| !l.starts_with("import ")).cloned().collect::<Vec<_>>().join("\n");
        // 3.3: 末尾名が衝突する2つのモジュールは同一ファイルで import できない。
        let mut tails: HashMap<&str, ()> = HashMap::new();
        for it in items {
            if it.kw != "import" {
                continue;
            }
            let tail = it.name.rsplit('.').next().unwrap();
            if tails.insert(tail, ()).is_some() {
                self.err(
                    "impcollide",
                    it.line + 1,
                    format!("import tail `{}` collides with another import; use the FQN instead", tail),
                );
            }
            let needle = format!("{}.", tail);
            let used = text.match_indices(&needle).any(|(i, _)| {
                text[..i].chars().next_back().map_or(true, |c| !(word_char(c) || c == '.'))
            });
            if !used {
                self.err("unused", it.line + 1, format!("unused import {}", it.name));
            }
        }
    }

    fn check_pub(&mut self, items: &[Item]) {
        let private: Vec<String> = items
            .iter()
            .filter(|it| is_field_kind(&it.kw) && !it.text.trim_end().ends_with(" pub"))
            .map(|it| it.name.clone())
            .collect();
        for it in items {
            if matches!(it.kw.as_str(), "fn" | "type" | "enum" | "trait") && it.text.trim_end().ends_with(" pub") {
                for p in &private {
                    if count_word(&it.text, p) > 0 {
                        self.err("pub", it.line + 1, format!("pub definition mentions private {}", p));
                    }
                }
            }
        }
    }

    pub fn run(mut self) -> Vec<LintErr> {
        let mut items = self.items();
        self.check_fqn_and_names(&items);
        self.check_order(&mut items);
        self.check_blank(&items);
        self.check_attrs(&items);
        self.check_indent(&items);
        self.check_names_and_unused(&items);
        self.check_imports(&items);
        self.check_pub(&items);
        let mut e = self.errs;
        e.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
        e
    }
}

fn py_list(xs: &[&str]) -> String {
    format!("[{}]", xs.iter().map(|x| crate::grammar::py_repr(x)).collect::<Vec<_>>().join(", "))
}
fn py_str_list(xs: &[String]) -> String {
    format!("[{}]", xs.iter().map(|x| crate::grammar::py_repr(x)).collect::<Vec<_>>().join(", "))
}

fn chr_nl() -> char {
    char::from(10u8)
}
