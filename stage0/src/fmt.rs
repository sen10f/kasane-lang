//! `ksn fmt` (stage0): 機械的に直せる正規形違反を直す。Python 参照実装 ksn/fmt.py の移植。

use regex::Regex;

use crate::program::load_file;

const DERIVE_ORDER: &[&str] = &["Debug", "Clone", "Copy", "PartialEq", "Eq", "PartialOrd", "Ord", "Hash"];

pub fn mechanical(text: &str) -> Vec<String> {
    let t = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out: Vec<String> = vec![];
    for l in t.split('\n') {
        let mut l = l.trim_end_matches(|c| c == ' ' || c == '\t').to_string();
        let tabs = l.chars().take_while(|c| *c == '\t').count();
        if tabs > 0 {
            l = format!("{}{}", "    ".repeat(tabs), &l[tabs..]);
        }
        out.push(l);
    }
    while out.last().map_or(false, |l| l.is_empty()) {
        out.pop();
    }
    out
}

/// cli 側の簡易整形 (構文エラーのファイル用): 連続する空行も1行にまとめる。
pub fn mechanical_collapse(text: &str) -> String {
    let lines = mechanical(text);
    let mut res: Vec<String> = vec![];
    for l in lines {
        if l.is_empty() && res.last().map_or(false, |x| x.is_empty()) {
            continue;
        }
        res.push(l);
    }
    res.join("\n") + "\n"
}

fn fix_derive(header: &str) -> String {
    let re = Regex::new(r"derive \[([^\]]*)\]").unwrap();
    let Some(m) = re.captures(header) else { return header.to_string() };
    let mut names: Vec<String> = m[1].split(',').map(|x| x.trim().to_string()).collect();
    names.sort_by_key(|n| DERIVE_ORDER.iter().position(|x| x == n).unwrap_or(99));
    let whole = m.get(0).unwrap();
    format!("{}derive [{}]{}", &header[..whole.start()], names.join(", "), &header[whole.end()..])
}

fn group(kw: &str) -> usize {
    match kw {
        "import" => 0,
        "const" | "static" => 1,
        "type" | "enum" | "trait" => 2,
        "extern" => 3,
        "impl" => 4,
        "fn" => 5,
        _ => 6,
    }
}

struct It {
    kw: String,
    name: String,
    lines: Vec<String>,
    impl_idx: Option<usize>,
    has_impls: bool,
}

pub fn format_source(text: &str, path: &str) -> String {
    let lines = mechanical(text);
    let joined = lines.join("\n") + "\n";
    let defs = match load_file(path, "", &joined) {
        Ok(d) => d,
        Err(_) => return joined,
    };
    let mut items: Vec<It> = vec![];
    for d in &defs {
        let mut s = d.line - 1;
        while s > 0 && lines[s - 1].starts_with("//") {
            s -= 1;
        }
        let e = if d.end_line > 0 { d.end_line } else { d.line };
        let mut body: Vec<String> = lines[s..e].to_vec();
        for (i, l) in body.iter_mut().enumerate() {
            if (l.starts_with("type ") || l.starts_with("enum ")) && (d.kw == "type" || d.kw == "enum") && i == d.line - 1 - s {
                *l = fix_derive(l);
            }
        }
        items.push(It { kw: d.kw.clone(), name: d.name.clone(), lines: body, impl_idx: None, has_impls: d.has_impls });
    }
    // impl 行と直後の fn(impls) を結びつける
    let mut pending: Option<usize> = None;
    let mut children: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for i in 0..items.len() {
        if items[i].kw == "impl" {
            pending = Some(i);
            children.insert(i, vec![]);
        } else if items[i].kw == "fn" && items[i].has_impls && pending.is_some() {
            let p = pending.unwrap();
            children.get_mut(&p).unwrap().push(i);
            items[i].impl_idx = Some(p);
        } else {
            pending = None;
        }
    }
    let mut groups: Vec<Vec<usize>> = vec![vec![]; 7];
    for i in 0..items.len() {
        let kw = items[i].kw.as_str();
        if kw == "impl" {
            if children[&i].is_empty() {
                groups[4].push(i);
            }
        } else {
            groups[group(kw)].push(i);
        }
    }
    for g in groups.iter_mut() {
        g.sort_by(|a, b| items[*a].name.as_bytes().cmp(items[*b].name.as_bytes()));
    }
    let mut out: Vec<Vec<String>> = vec![];
    let imp: Vec<String> = groups[0].iter().map(|&i| items[i].lines[0].clone()).collect();
    if !imp.is_empty() {
        out.push(imp);
    }
    for g in [1, 2, 3, 4] {
        for &i in &groups[g] {
            out.push(items[i].lines.clone());
        }
    }
    let mut emitted: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for &i in &groups[5] {
        let mut chunk: Vec<String> = vec![];
        if let Some(p) = items[i].impl_idx {
            if !emitted.contains(&p) {
                let first = *children[&p].iter().min_by(|a, b| items[**a].name.as_bytes().cmp(items[**b].name.as_bytes())).unwrap();
                if first == i {
                    chunk.extend(items[p].lines.clone());
                    emitted.insert(p);
                }
            }
        }
        chunk.extend(items[i].lines.clone());
        out.push(chunk);
    }
    for &i in &groups[6] {
        out.push(items[i].lines.clone());
    }
    let mut res: Vec<String> = vec![];
    for (k, chunk) in out.into_iter().enumerate() {
        if k > 0 {
            res.push(String::new());
        }
        res.extend(chunk);
    }
    if res.is_empty() { String::new() } else { res.join("\n") + "\n" }
}
