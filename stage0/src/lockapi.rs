//! `kasane.lock` 版 2: 依存の公開の署名と効果の記録・比較 (設計_ksnhとロック.md 3 章)。
//! 記録は公開の定義の正規のヘッダの全文 (`pub` の定義・`impl` 行・トレイトの `impls` の関数)。
//! 比べて違えば止める: 効果が広がった E9060、署名が変わった・消えた E9061、狭まった・増えた E9062。

use std::collections::BTreeMap;

use crate::project::{unsafe_modules, Package};

/// 公開の定義 1 つ = (FQN または `impl:<行>`, 正規のヘッダの全文)。FQN のバイト順。
pub type Api = Vec<(String, String)>;

fn is_pub_header(h: &str) -> bool {
    h.split_whitespace().last() == Some("pub")
}

/// 依存パッケージの現在の公開の署名。本体は含まない (ヘッダだけ)。
pub fn api_of(dp: &Package) -> Api {
    let mut m: BTreeMap<String, String> = BTreeMap::new();
    for s in &dp.sources {
        for d in &s.defs {
            let (key, sig) = match d.kw.as_str() {
                "impl" => (format!("impl:{}", d.header.trim()), d.header.trim().to_string()),
                "fn" | "type" | "enum" | "trait" | "const" | "static" | "extern" => {
                    // トレイトの実装のメソッドは `impl` 行の約束と同じ重さなので、`pub` でなくても記録する。
                    if !(is_pub_header(&d.header) || d.has_impls) {
                        continue;
                    }
                    let multi = matches!(d.kw.as_str(), "type" | "enum" | "trait");
                    let text = if multi && d.end_line >= d.line && d.line >= 1 && d.end_line <= s.lines.len() {
                        s.lines[d.line - 1..d.end_line].iter().map(|l| l.trim_end_matches('\r')).collect::<Vec<_>>().join("\n")
                    } else {
                        d.header.trim().to_string()
                    };
                    (d.name.clone(), text)
                }
                _ => continue,
            };
            m.insert(key, sig);
        }
    }
    let mut v: Api = m.into_iter().collect();
    v.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    v
}

/// 依存の `[unsafe] modules` (増えたら止める)。
pub fn unsafe_of(dp: &Package) -> Vec<String> {
    let mut v = unsafe_modules(&dp.manifest);
    v.sort();
    v.dedup();
    v
}

/// 依存の `[native]` の C のライブラリ (足されたら止める)。`static` のパスと `dynamic` の名前を合わせて 1 つの記録にする。
pub fn native_of(dp: &Package) -> Vec<String> {
    let n = crate::project::native_libs(&dp.manifest);
    let mut v: Vec<String> = n.archives.into_iter().chain(n.dynamic).collect();
    v.sort();
    v.dedup();
    v
}

/// ヘッダの `uses [..]` と `panics X` を取り出し、残りを返す: (効果を除いたヘッダ, uses, panics)。
fn split_effects(sig: &str) -> Option<(String, Vec<String>, String)> {
    let first = sig.lines().next().unwrap_or("");
    let u = first.find(" uses [")?;
    let after = &first[u + " uses [".len()..];
    let close = after.find(']')?;
    let uses: Vec<String> = after[..close].split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
    let rest = after[close + 1..].trim_start();
    let rest = rest.strip_prefix("panics ")?;
    let (pn, tail) = match rest.find(' ') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let stripped = format!("{}{}", &first[..u], tail);
    Some((stripped, uses, pn.to_string()))
}

fn covered(e: &str, by: &[String]) -> bool {
    by.iter().any(|o| e == o || (e.len() > o.len() && e.starts_with(o.as_str()) && e.as_bytes()[o.len()] == b'.'))
}

#[derive(Debug, PartialEq)]
enum Eff {
    Same,
    Wider,
    Narrower,
}

/// 効果だけが違うとき、広がったか狭まったか (どちらでもある・読めないときは `None` = 署名の変化)。
fn effect_change(old: &str, new: &str) -> Option<Eff> {
    let (os, ou, op) = split_effects(old)?;
    let (ns, nu, np) = split_effects(new)?;
    if os != ns || old.lines().count() != new.lines().count() || old.lines().skip(1).ne(new.lines().skip(1)) {
        return None;
    }
    let wider = nu.iter().any(|e| !covered(e, &ou)) || (op == "never" && np != "never") || (op != np && np == "may");
    let narrower = ou.iter().any(|e| !covered(e, &nu)) || (np == "never" && op != "never");
    Some(match (wider, narrower) {
        (true, _) => Eff::Wider,
        (false, true) => Eff::Narrower,
        _ => Eff::Same,
    })
}

fn indent(s: &str, mark: &str) -> String {
    s.lines().map(|l| format!("    {} {}", mark, l)).collect::<Vec<_>>().join("\n")
}

/// ロックの記録 (`old`) と今 (`now`) を比べ、(コード, メッセージ) を並べて返す。
pub fn compare(
    pkg: &str,
    old: &Api,
    old_unsafe: &[String],
    now: &Api,
    now_unsafe: &[String],
    old_native: &[String],
    now_native: &[String],
) -> Vec<(String, String)> {
    let mut out = vec![];
    let hint = format!("; review the change, then run `ksn lock --accept {}`", pkg);
    let o: BTreeMap<&str, &str> = old.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let n: BTreeMap<&str, &str> = now.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    for m in now_unsafe.iter().filter(|m| !old_unsafe.contains(m)) {
        out.push(("E9060".to_string(), format!("{}: unsafe module {} was added to the dependency's [unsafe] modules{}", pkg, m, hint)));
    }
    for m in old_unsafe.iter().filter(|m| !now_unsafe.contains(m)) {
        out.push(("E9062".to_string(), format!("{}: unsafe module {} was removed from the dependency's [unsafe] modules{}", pkg, m, hint)));
    }
    // `[native]` の C のライブラリ: 足されたのは効果の広がりと同じ重さ (ホストに何が要るかが増える)。
    for m in now_native.iter().filter(|m| !old_native.contains(m)) {
        out.push(("E9060".to_string(), format!("{}: native library {} was added to the dependency's [native] table{}", pkg, m, hint)));
    }
    for m in old_native.iter().filter(|m| !now_native.contains(m)) {
        out.push(("E9062".to_string(), format!("{}: native library {} was removed from the dependency's [native] table{}", pkg, m, hint)));
    }
    for (k, ov) in &o {
        match n.get(k) {
            None => out.push(("E9061".into(), format!("{}: public definition {} was removed{}\n{}", pkg, k, hint, indent(ov, "-")))),
            Some(nv) if nv != ov => {
                let (code, what) = match effect_change(ov, nv) {
                    Some(Eff::Wider) => ("E9060", "effects widened"),
                    Some(Eff::Narrower) => ("E9062", "effects narrowed"),
                    Some(Eff::Same) | None => ("E9061", "signature changed"),
                };
                out.push((code.into(), format!("{}: {} of {}{}\n{}\n{}", pkg, what, k, hint, indent(ov, "-"), indent(nv, "+"))));
            }
            _ => {}
        }
    }
    for (k, nv) in &n {
        if !o.contains_key(k) {
            // C の関数を呼ぶ入口が増えるのは効果の広がりと同じ重さ。
            let code = if nv.starts_with("extern ") { "E9060" } else { "E9062" };
            out.push((code.into(), format!("{}: public definition {} was added{}\n{}", pkg, k, hint, indent(nv, "+"))));
        }
    }
    out
}

/// ロックの `[[package]]` の `api` / `unsafe_modules` / `native_libs` を読む。記録の無いもの (版 1 など) は `None`。
pub fn read_api(e: &toml::Value) -> Option<(Api, Vec<String>, Vec<String>)> {
    let arr = e.get("api").and_then(|a| a.as_array());
    let uns: Vec<String> = e.get("unsafe_modules").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let nat: Vec<String> = e.get("native_libs").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    if arr.is_none() && e.get("unsafe_modules").is_none() && e.get("native_libs").is_none() {
        return None;
    }
    let api = arr
        .map(|a| a.iter().filter_map(|i| Some((i.get("fqn")?.as_str()?.to_string(), i.get("sig")?.as_str()?.to_string()))).collect())
        .unwrap_or_default();
    Some((api, uns, nat))
}

/// `[[package.api]]`・`unsafe_modules`・`native_libs` の行。
pub fn render_api(api: &Api, uns: &[String], nat: &[String]) -> Vec<String> {
    let q = |s: &str| toml::Value::String(s.to_string()).to_string();
    let mut l = vec![format!("unsafe_modules = [{}]", uns.iter().map(|x| q(x)).collect::<Vec<_>>().join(", "))];
    if !nat.is_empty() {
        l.push(format!("native_libs = [{}]", nat.iter().map(|x| q(x)).collect::<Vec<_>>().join(", ")));
    }
    for (f, s) in api {
        l.push("[[package.api]]".into());
        l.push(format!("fqn = {}", q(f)));
        l.push(format!("sig = {}", q(s)));
    }
    l
}
