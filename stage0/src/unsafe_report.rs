//! `ksn unsafe [--json]`: パッケージと依存の `unsafe` の使用の一覧 (読み出し専用。設計_SendSyncとunsafe.md 第 3 波、H7)。
//! 検査はしない。ソースの行を走査して、レビューの見る場所 (許可モジュール・手動 Send/Sync・unsafe ブロック・extern・生ポインタを持つ型) を並べる。

use crate::project::{Package, Source};
use serde_json::{json, Value};

struct Item {
    kind: &'static str,
    name: String,
    module: String,
    file: String,
    line: usize,
    detail: String,
}

fn quoted(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut inq = false;
    let mut esc = false;
    for ch in s.chars() {
        if inq {
            if esc {
                cur.push(ch);
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                out.push(std::mem::take(&mut cur));
                inq = false;
            } else {
                cur.push(ch);
            }
        } else if ch == '"' {
            inq = true;
        }
    }
    out
}

fn scan(src: &Source, items: &mut Vec<Item>) {
    let file = crate::cli::rel(&src.path);
    let mut ty: Option<(String, usize, bool)> = None;
    for (i, raw) in src.lines.iter().enumerate() {
        let t = raw.trim();
        let line = i + 1;
        if t.starts_with("//") || t.is_empty() {
            continue;
        }
        let mk = |kind: &'static str, name: String, detail: String| Item { kind, name, module: src.module.clone(), file: file.clone(), line, detail };
        if let Some((n, l, found)) = &mut ty {
            if t.starts_with("end ") {
                let _ = (n, l, found);
                ty = None;
                continue;
            }
            if (t.contains("*const ") || t.contains("*mut ")) && !*found {
                *found = true;
                let (n, l) = (n.clone(), *l);
                items.push(Item { kind: "raw_pointer_type", name: n, module: src.module.clone(), file: file.clone(), line: l, detail: t.to_string() });
            }
            continue;
        }
        let head = t.strip_prefix("pub ").unwrap_or(t);
        if let Some(rest) = head.strip_prefix("type ").or_else(|| head.strip_prefix("enum ")) {
            let name: String = rest.chars().take_while(|c| !c.is_whitespace() && *c != '<' && *c != '[').collect();
            ty = Some((name, line, false));
        } else if head.starts_with("impl") && (t.contains("core.marker.Send for ") || t.contains("core.marker.Sync for ")) {
            let tr = if t.contains("core.marker.Send for ") { "Send" } else { "Sync" };
            let target = t.rsplit(" for ").next().unwrap_or("").trim().to_string();
            items.push(mk("manual_impl", target, tr.to_string()));
        } else if head.starts_with("extern ") {
            let name: String = head["extern ".len()..].chars().take_while(|c| *c != '(').collect();
            let link = head.split(" link ").nth(1).map(|l| quoted(l).into_iter().next().unwrap_or_default()).unwrap_or_default();
            items.push(mk("extern", name.trim().to_string(), link));
        } else if t.contains("unsafe \"") {
            let at = t.find("unsafe \"").unwrap_or(0);
            let q = quoted(&t[at..]);
            if q.len() >= 2 {
                items.push(mk("unsafe_block", q[0].clone(), q[1].clone()));
            }
        }
    }
}

fn package_entry(p: &Package, root: bool) -> (String, Value, Vec<String>) {
    let mut mods = crate::project::unsafe_modules(&p.manifest);
    mods.sort();
    let mut items = vec![];
    for s in &p.sources {
        scan(s, &mut items);
    }
    items.sort_by(|a, b| (&a.file, a.line, a.kind).cmp(&(&b.file, b.line, b.kind)));
    let jitems: Vec<Value> = items
        .iter()
        .map(|i| json!({"kind": i.kind, "name": i.name, "module": i.module, "file": i.file, "line": i.line, "detail": i.detail}))
        .collect();
    let mut text = vec![format!("package {} ({})", p.name, if root { "root" } else { "dependency" })];
    text.push(format!("  unsafe modules: {}", if mods.is_empty() { "(none)".to_string() } else { mods.join(", ") }));
    let nat = crate::project::native_libs(&p.manifest);
    let ack = crate::project::native_acknowledged(&p.manifest);
    if !nat.is_empty() {
        text.push(format!("  native libraries (acknowledged: {}):", ack));
        for (k, v) in [("static", &nat.archives), ("dynamic", &nat.dynamic), ("search", &nat.search)] {
            for x in v {
                text.push(format!("    native {} {}", k, x));
            }
        }
    }
    for i in &items {
        let what = match i.kind {
            "manual_impl" => format!("manual impl core.marker.{} for {}", i.detail, i.name),
            "unsafe_block" => format!("unsafe block \"{}\": {}", i.name, i.detail),
            "extern" => format!("extern {} link \"{}\"", i.name, i.detail),
            _ => format!("raw-pointer type {}: {}", i.name, i.detail),
        };
        text.push(format!("  {}:{}: [{}] {}", i.file, i.line, i.module, what));
    }
    let v = json!({"name": p.name, "root": root, "unsafe_modules": mods, "native": {"acknowledged": ack, "static": nat.archives, "dynamic": nat.dynamic, "search": nat.search}, "items": jitems});
    (p.name.clone(), v, text)
}

pub fn report(root: &Package, deps: &[Package], as_json: bool) -> i32 {
    let mut entries = vec![package_entry(root, true)];
    let mut ds: Vec<_> = deps.iter().map(|d| package_entry(d, false)).collect();
    ds.sort_by(|a, b| a.0.cmp(&b.0));
    entries.extend(ds);
    if as_json {
        let pk: Vec<Value> = entries.iter().map(|e| e.1.clone()).collect();
        println!("{}", serde_json::to_string_pretty(&json!({"packages": pk})).unwrap_or_default());
    } else {
        for e in &entries {
            for l in &e.2 {
                println!("{}", l);
            }
        }
    }
    0
}
