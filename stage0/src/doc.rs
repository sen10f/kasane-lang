//! `ksn doc`: API ドキュメント (静的 HTML) の生成。
//!
//! 人間が読むのは「ヘッダ (契約)・テスト・公開 API」の 3 つ、という方針に沿い、
//! 各定義についてヘッダ行・`///` ドキュメント・契約ラベル (effects / panics など)・
//! テスト本体を載せる。定義データは `Program` / `Source` から得たものをそのまま使い、
//! ソース文字列の再パースはしない。出力は文字列組み立てのみで、順序は BTreeMap で固定する。

use std::collections::{BTreeMap, BTreeSet};

use crate::program::Def;

/// 1 つの定義とそのソース行。`d` は効果推論済みの定義。
pub struct Item<'a> {
    pub d: &'a Def,
    pub lines: &'a [String],
}

/// 生成結果: 相対ファイル名 -> HTML。
pub type Pages = BTreeMap<String, String>;

pub struct Config<'a> {
    pub title: &'a str,
    pub is_std: bool,
    pub private: bool,
    /// 標準ライブラリのページへリンクするときの定義群 (生成済みのときだけ与える)。
    pub std_link: Option<&'a [Item<'a>]>,
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            _ => o.push(c),
        }
    }
    o
}

/// アンカー・ファイル名に使える形へ (FQN は通常そのまま)。
fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '_' }).collect()
}

fn is_pub(d: &Def) -> bool {
    d.header.split_whitespace().last() == Some("pub")
}

fn def_kind(d: &Def) -> &'static str {
    match d.kw.as_str() {
        "fn" => "fn",
        "type" => "type",
        "enum" => "enum",
        "trait" => "trait",
        "const" => "const",
        "static" => "static",
        "extern" => "extern",
        _ => "",
    }
}

fn trim_cr(s: &str) -> &str {
    s.trim_end_matches('\r')
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

// ------------------------------------------------------------ リンク解決
struct Target {
    module: String,
    anchor: String,
}

struct Links {
    own: BTreeMap<String, Target>,
    other: BTreeMap<String, Target>,
    /// 短い名前 (プレリュード相当) -> FQN。標準ライブラリで一意なものだけ。
    short_own: BTreeMap<String, String>,
    short_other: BTreeMap<String, String>,
}

fn type_index(items: &[Item], private: bool) -> BTreeMap<String, Target> {
    let mut m = BTreeMap::new();
    for it in items {
        let d = it.d;
        if matches!(d.kw.as_str(), "type" | "enum" | "trait") && (private || is_pub(d)) {
            m.insert(d.name.clone(), Target { module: d.module.clone(), anchor: sanitize(&d.name) });
        }
    }
    m
}

fn short_index(m: &BTreeMap<String, Target>, only_core: bool) -> BTreeMap<String, String> {
    let mut cnt: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (k, t) in m {
        if k.contains('.') && (!only_core || t.module.starts_with("core.")) {
            cnt.entry(k.rsplit('.').next().unwrap().to_string()).or_default().push(k.clone());
        }
    }
    cnt.into_iter().filter(|(_, v)| v.len() == 1).map(|(k, mut v)| (k, v.pop().unwrap())).collect()
}

fn page_file(module: &str) -> String {
    format!("{}.html", sanitize(module))
}

struct Ctx<'a> {
    links: &'a Links,
    cur_module: &'a str,
    is_std: bool,
}

impl Ctx<'_> {
    fn href(&self, fqn: &str) -> Option<String> {
        let (t, other) = if let Some(t) = self.links.own.get(fqn) {
            (t, false)
        } else {
            (self.links.other.get(fqn)?, true)
        };
        Some(if !other && t.module == self.cur_module {
            format!("#{}", t.anchor)
        } else if other {
            format!("std/{}#{}", page_file(&t.module), t.anchor)
        } else {
            format!("{}#{}", page_file(&t.module), t.anchor)
        })
    }

    fn resolve(&self, tok: &str) -> Option<String> {
        let mut t = tok;
        loop {
            if let Some(h) = self.href(t) {
                return Some(h);
            }
            match t.rsplit_once('.') {
                Some((head, _)) if !head.is_empty() => t = head,
                _ => break,
            }
        }
        if !tok.contains('.') {
            let f = self.links.short_own.get(tok).or_else(|| self.links.short_other.get(tok))?;
            return self.href(f);
        }
        None
    }

    /// テキストをエスケープし、型・トレイト名をリンクにする。
    fn linkify(&self, text: &str, own_name: &str) -> String {
        let cs: Vec<(usize, char)> = text.char_indices().collect();
        let mut out = String::with_capacity(text.len() * 2);
        let mut i = 0;
        while i < cs.len() {
            let (b, c) = cs[i];
            if c.is_alphabetic() || c == '_' {
                let mut j = i;
                while j < cs.len() && (is_ident(cs[j].1) || cs[j].1 == '.') {
                    j += 1;
                }
                // 末尾の '.' は含めない
                while j > i + 1 && cs[j - 1].1 == '.' {
                    j -= 1;
                }
                let end = if j < cs.len() { cs[j].0 } else { text.len() };
                let tok = &text[b..end];
                let href = if tok == own_name { None } else { self.resolve(tok) };
                match href {
                    Some(h) => out.push_str(&format!("<a href=\"{}\">{}</a>", esc(&h), esc(tok))),
                    None => out.push_str(&esc(tok)),
                }
                i = j;
            } else {
                out.push_str(&esc(&c.to_string()));
                i += 1;
            }
        }
        let _ = self.is_std;
        out
    }
}

// ------------------------------------------------------------ ドキュメントコメント
fn doc_lines(it: &Item) -> Vec<String> {
    let mut out = vec![];
    let mut i = it.d.line as isize - 2;
    while i >= 0 {
        let l = trim_cr(&it.lines[i as usize]);
        if let Some(r) = l.strip_prefix("///") {
            out.push(r.strip_prefix(' ').unwrap_or(r).to_string());
            i -= 1;
        } else {
            break;
        }
    }
    out.reverse();
    out
}

fn inline_md(s: &str) -> String {
    let e = esc(s);
    let parts: Vec<&str> = e.split('`').collect();
    if parts.len() % 2 == 0 {
        return e;
    }
    let mut o = String::new();
    for (k, p) in parts.iter().enumerate() {
        if k % 2 == 1 {
            o.push_str("<code>");
            o.push_str(p);
            o.push_str("</code>");
        } else {
            o.push_str(p);
        }
    }
    o
}

fn render_doc(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut o = String::from("<div class=\"doc\">");
    let mut para: Vec<String> = vec![];
    let mut in_list = false;
    let mut i = 0;
    let flush = |para: &mut Vec<String>, o: &mut String| {
        if !para.is_empty() {
            o.push_str(&format!("<p>{}</p>", para.join("\n")));
            para.clear();
        }
    };
    while i < lines.len() {
        let l = &lines[i];
        if l.trim_start().starts_with("```") {
            flush(&mut para, &mut o);
            if in_list {
                o.push_str("</ul>");
                in_list = false;
            }
            let mut code = vec![];
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code.push(esc(&lines[i]));
                i += 1;
            }
            o.push_str(&format!("<pre><code>{}</code></pre>", code.join("\n")));
        } else if let Some(r) = l.strip_prefix("- ") {
            flush(&mut para, &mut o);
            if !in_list {
                o.push_str("<ul>");
                in_list = true;
            }
            o.push_str(&format!("<li>{}</li>", inline_md(r)));
        } else if l.trim().is_empty() {
            flush(&mut para, &mut o);
            if in_list {
                o.push_str("</ul>");
                in_list = false;
            }
        } else {
            if in_list {
                o.push_str("</ul>");
                in_list = false;
            }
            para.push(inline_md(l));
        }
        i += 1;
    }
    flush(&mut para, &mut o);
    if in_list {
        o.push_str("</ul>");
    }
    o.push_str("</div>");
    o
}

// ------------------------------------------------------------ 契約ラベル
fn tag(cls: &str, text: &str, title: &str) -> String {
    format!("<span class=\"tag {}\" title=\"{}\">{}</span>", cls, esc(title), esc(text))
}

fn contract_tags(d: &Def) -> String {
    let mut o = String::new();
    let inf = d.inferred;
    let dash = if inf { " inf" } else { "" };
    match d.kw.as_str() {
        "fn" => {
            if d.uses.is_empty() {
                o.push_str(&tag(&format!("pure{}", dash), "pure", "副作用なし (uses [])"));
            } else {
                for u in &d.uses {
                    o.push_str(&tag(&format!("eff{}", dash), &format!("uses {}", u), "効果 (uses)"));
                }
            }
            match d.panics.as_deref() {
                Some("never") => o.push_str(&tag(&format!("never{}", dash), "panics never", "パニックしない")),
                Some("may") => o.push_str(&tag(&format!("may{}", dash), "panics may", "パニックし得る")),
                Some(p) => o.push_str(&tag(&format!("may{}", dash), &format!("panics {}", p), "パニック性 (多相)")),
                None => {}
            }
            if inf {
                o.push_str(&tag("infer", "inferred", "uses / panics を省略した関数: 本体から推論した契約"));
            }
            if d.is_const {
                o.push_str(&tag("const", "const", "コンパイル時評価可能"));
            }
            if let Some(n) = d.stack {
                o.push_str(&tag("stack", &format!("stack {}", n), "スタック上限"));
            }
        }
        "type" | "enum" => {
            if d.linear {
                o.push_str(&tag("linear", "linear", "線形型"));
            }
        }
        _ => {}
    }
    if !is_pub(d) && !d.has_impls {
        o.push_str(&tag("priv", "private", "非公開"));
    }
    o
}

// ------------------------------------------------------------ ブロック
struct Unique(BTreeSet<String>);
impl Unique {
    fn take(&mut self, base: String) -> String {
        let mut a = base.clone();
        let mut n = 2;
        while !self.0.insert(a.clone()) {
            a = format!("{}-{}", base, n);
            n += 1;
        }
        a
    }
}

fn body_text(it: &Item, from_header: bool) -> String {
    let d = it.d;
    let s = d.line - 1;
    let e = if from_header && d.end_line > 0 { d.end_line } else { d.line };
    it.lines[s..e.min(it.lines.len())].iter().map(|l| trim_cr(l)).collect::<Vec<_>>().join("\n")
}

fn tests_block(tests: &[&Item], uniq: &mut Unique) -> String {
    if tests.is_empty() {
        return String::new();
    }
    let mut o = format!("<details class=\"tests\" open><summary>テスト (仕様の例): {}</summary>", tests.len());
    for t in tests {
        let id = uniq.take(sanitize(&t.d.name));
        o.push_str(&format!(
            "<pre class=\"test\" id=\"{}\"><code>{}</code></pre>",
            id,
            esc(&body_text(t, true))
        ));
    }
    o.push_str("</details>");
    o
}

struct Module<'a> {
    items: Vec<&'a Item<'a>>,
}

fn kind_order(k: &str) -> u8 {
    match k {
        "type" => 0,
        "enum" => 1,
        "trait" => 2,
        "const" => 3,
        "static" => 4,
        "extern" => 5,
        _ => 6,
    }
}

fn impl_type_of(d: &Def) -> String {
    let h = d.header.trim_end();
    let h = h.strip_suffix(" pub").unwrap_or(h);
    let after = h.split_once(" for ").map_or("", |x| x.1).trim_start_matches(|c: char| c == '&' || c == ' ');
    after.chars().take_while(|c| is_ident(*c) || *c == '.').collect()
}

fn owner_of(name: &str) -> Option<&str> {
    name.rsplit_once('.').map(|x| x.0)
}

struct Global<'a> {
    /// 型名 -> impl 行 (ヘッダ文字列, トレイト名)
    impls_by_type: BTreeMap<String, Vec<(String, String)>>,
    /// トレイト名 -> 実装する型名
    impls_by_trait: BTreeMap<String, Vec<String>>,
    tests: BTreeMap<String, Vec<&'a Item<'a>>>,
}

fn kind_label(d: &Def) -> &'static str {
    def_kind(d)
}

fn def_block(
    it: &Item,
    level: u8,
    ctx: &Ctx,
    g: &Global,
    uniq: &mut Unique,
    out: &mut String,
    with_def_body: bool,
) {
    let d = it.d;
    let anchor = uniq.take(sanitize(&d.name));
    let full = with_def_body && matches!(d.kw.as_str(), "type" | "enum" | "trait");
    out.push_str(&format!(
        "<article class=\"def {}\" id=\"{}\"><h{l}><span class=\"kind\">{}</span> <a class=\"name\" href=\"#{}\">{}</a> {}</h{l}>",
        kind_label(d),
        anchor,
        kind_label(d),
        anchor,
        esc(&d.name),
        contract_tags(d),
        l = level
    ));
    let src = body_text(it, full);
    out.push_str(&format!("<pre class=\"sig\"><code>{}</code></pre>", ctx.linkify(&src, &d.name)));
    if d.kw == "fn" && d.inferred {
        out.push_str(&format!(
            "<p class=\"note\">ヘッダは <code>uses</code> / <code>panics</code> を省略しているため、本体から推論した契約を表示しています: {}</p>",
            contract_tags(d)
        ));
    }
    if d.has_impls {
        out.push_str(&format!("<p class=\"note\">実装: <code>impls {}</code></p>", ctx.linkify(&d.impls_trait, "")));
    }
    out.push_str(&render_doc(&doc_lines(it)));
    if d.kw == "trait" {
        if let Some(ts) = g.impls_by_trait.get(&d.name) {
            out.push_str("<div class=\"impls\"><h5>実装している型</h5><ul>");
            for t in ts {
                out.push_str(&format!("<li>{}</li>", ctx.linkify(t, "")));
            }
            out.push_str("</ul></div>");
        }
    }
    if matches!(d.kw.as_str(), "type" | "enum") {
        if let Some(is) = g.impls_by_type.get(&d.name) {
            out.push_str("<div class=\"impls\"><h5>トレイト実装</h5><ul>");
            for (h, _) in is {
                out.push_str(&format!("<li><code>{}</code></li>", ctx.linkify(h, "")));
            }
            out.push_str("</ul></div>");
        }
    }
    if let Some(ts) = g.tests.get(&d.name) {
        out.push_str(&tests_block(ts, uniq));
    }
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '<' => o.push_str("\\u003c"),
            '>' => o.push_str("\\u003e"),
            '&' => o.push_str("\\u0026"),
            '\u{2028}' => o.push_str("\\u2028"),
            '\u{2029}' => o.push_str("\\u2029"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

const CSS: &str = r#":root{color-scheme:light dark;--bg:#fff;--fg:#1c2128;--mut:#59636e;--bd:#d1d9e0;--card:#f6f8fa;--link:#0550ae;--ok:#1a7f37;--warn:#9a6700;--bad:#cf222e;--eff:#8250df;--acc:#0969da}
@media(prefers-color-scheme:dark){:root{--bg:#0d1117;--fg:#e6edf3;--mut:#9198a1;--bd:#30363d;--card:#161b22;--link:#79c0ff;--ok:#3fb950;--warn:#d29922;--bad:#f85149;--eff:#d2a8ff;--acc:#58a6ff}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.6 system-ui,-apple-system,"Segoe UI","Hiragino Sans","Noto Sans JP",sans-serif}
a{color:var(--link);text-decoration:none}a:hover{text-decoration:underline}
header.top{position:sticky;top:0;z-index:5;display:flex;flex-wrap:wrap;gap:.5rem 1rem;align-items:center;padding:.5rem 1rem;background:var(--card);border-bottom:1px solid var(--bd)}
header.top .brand{font-weight:700}
header.top input{flex:1 1 14rem;min-width:0;padding:.35rem .6rem;border:1px solid var(--bd);border-radius:6px;background:var(--bg);color:var(--fg);font:inherit}
#res{position:absolute;left:0;right:0;top:100%;max-height:60vh;overflow:auto;margin:0;padding:.25rem 1rem;list-style:none;background:var(--card);border-bottom:1px solid var(--bd)}
#res li{padding:.15rem 0;overflow-wrap:anywhere}
main{max-width:62rem;margin:0 auto;padding:1rem}
h1{font-size:1.6rem;margin:.5rem 0}h2{font-size:1.25rem;margin:2rem 0 .5rem;border-bottom:1px solid var(--bd);padding-bottom:.2rem}
h3,h4{font-size:1.05rem;margin:0 0 .4rem;overflow-wrap:anywhere}h5{margin:.6rem 0 .2rem;font-size:.9rem;color:var(--mut)}
article.def{border:1px solid var(--bd);border-radius:8px;padding:.7rem .9rem;margin:.8rem 0;background:var(--card)}
article.def article.def{background:var(--bg)}
article.def:target{outline:2px solid var(--acc)}
pre{margin:.4rem 0;padding:.5rem .7rem;overflow-x:auto;background:var(--bg);border:1px solid var(--bd);border-radius:6px;font:.85rem/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
article.def article.def pre{background:var(--card)}
code{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:.88em}
.kind{display:inline-block;min-width:3.2rem;color:var(--mut);font-size:.8rem;font-weight:400}
.tag{display:inline-block;margin:0 .15rem;padding:0 .45rem;border:1px solid var(--bd);border-radius:999px;font:600 .72rem/1.5 ui-monospace,Menlo,Consolas,monospace;vertical-align:middle;white-space:nowrap}
.tag.eff{color:var(--eff);border-color:var(--eff)}.tag.pure,.tag.never{color:var(--ok);border-color:var(--ok)}
.tag.may{color:var(--warn);border-color:var(--warn)}.tag.linear{color:var(--bad);border-color:var(--bad)}
.tag.const,.tag.stack{color:var(--acc);border-color:var(--acc)}.tag.priv,.tag.infer{color:var(--mut)}
.tag.inf{border-style:dashed}
.note{color:var(--mut);font-size:.88rem;margin:.3rem 0}
.doc p{margin:.4rem 0;white-space:pre-line}.doc ul{margin:.3rem 0}
details.tests{margin-top:.5rem}details.tests summary{cursor:pointer;color:var(--mut);font-size:.9rem}
pre.test{border-left:3px solid var(--ok)}
.impls ul{margin:.2rem 0;padding-left:1.2rem}
table{border-collapse:collapse;width:100%}td,th{border-bottom:1px solid var(--bd);padding:.35rem .5rem;text-align:left;vertical-align:top}
nav.toc ul{columns:16rem;list-style:none;padding:0}nav.toc li{overflow-wrap:anywhere}
@media(max-width:600px){main{padding:.6rem}article.def{padding:.5rem .6rem}}
"#;

const SEARCH_JS: &str = r#"(function(){var q=document.getElementById('q'),r=document.getElementById('res');if(!q||!r)return;
q.addEventListener('input',function(){var s=q.value.trim().toLowerCase();r.textContent='';if(!s){r.hidden=true;return;}var n=0;
for(var m=0;m<D.length&&n<60;m++){var es=D[m][1];for(var i=0;i<es.length&&n<60;i++){var e=es[i];if(e[0].toLowerCase().indexOf(s)<0)continue;
var li=document.createElement('li'),a=document.createElement('a'),k=document.createElement('span');
a.href=D[m][0]+'#'+e[0].replace(/[^A-Za-z0-9_.-]/g,'_');a.textContent=e[0];k.className='kind';k.textContent=e[1];li.appendChild(k);li.appendChild(a);r.appendChild(li);n++;}}
r.hidden=n===0;});})();"#;

fn page(title: &str, brand: &str, index_href: &str, extra_nav: &str, body: &str, search: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"ja\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{t}</title>\n<style>\n{css}</style>\n</head>\n<body>\n<header class=\"top\"><a class=\"brand\" href=\"{ih}\">{b}</a>{x}<input id=\"q\" type=\"search\" placeholder=\"FQN を検索\" aria-label=\"FQN を検索\" autocomplete=\"off\"><ul id=\"res\" hidden></ul></header>\n<main>\n{body}\n</main>\n<script>\nvar D={s};\n{js}\n</script>\n</body>\n</html>\n",
        t = esc(title),
        css = CSS,
        ih = index_href,
        b = esc(brand),
        x = extra_nav,
        body = body,
        s = search,
        js = SEARCH_JS
    )
}

/// ドキュメント全体を生成する。
pub fn generate(items: &[Item], cfg: &Config) -> Pages {
    // 公開型の名前。トレイトを実装するメソッド (`impls`) は pub でなくても型と一緒に公開 API とみなす。
    let pub_types: BTreeSet<&str> = items
        .iter()
        .filter(|i| matches!(i.d.kw.as_str(), "type" | "enum" | "trait") && is_pub(i.d))
        .map(|i| i.d.name.as_str())
        .collect();
    let visible = |d: &Def| {
        def_kind(d) != ""
            && (cfg.private
                || is_pub(d)
                || (d.has_impls && owner_of(&d.name).map_or(false, |o| pub_types.contains(o))))
    };
    let own = type_index(items, cfg.private);
    let (other, std_ok) = match cfg.std_link {
        Some(s) => (type_index(s, false), true),
        None => (BTreeMap::new(), false),
    };
    let short_own = if cfg.is_std { short_index(&own, false) } else { BTreeMap::new() };
    let short_other = short_index(&other, true);
    let links = Links { own, other, short_own, short_other };

    // グローバル情報 (impl / test)
    let mut g = Global { impls_by_type: BTreeMap::new(), impls_by_trait: BTreeMap::new(), tests: BTreeMap::new() };
    for it in items {
        let d = it.d;
        if d.kw == "impl" {
            let ty = impl_type_of(d);
            g.impls_by_type.entry(ty.clone()).or_default().push((d.header.clone(), d.impl_trait.clone()));
            g.impls_by_trait.entry(d.impl_trait.clone()).or_default().push(ty);
        } else if d.kw == "test" {
            if let Some(t) = owner_of(&d.name) {
                g.tests.entry(t.to_string()).or_default().push(it);
            }
        }
    }
    for v in g.impls_by_type.values_mut() {
        v.sort();
        v.dedup();
    }
    for v in g.impls_by_trait.values_mut() {
        v.sort();
        v.dedup();
    }
    for v in g.tests.values_mut() {
        v.sort_by(|a, b| a.d.name.cmp(&b.d.name));
    }

    // モジュールごとの公開 (または --private で全) 定義
    let mut mods: BTreeMap<String, Module> = BTreeMap::new();
    for it in items {
        if visible(it.d) {
            mods.entry(it.d.module.clone()).or_insert_with(|| Module { items: vec![] }).items.push(it);
        }
    }
    for m in mods.values_mut() {
        m.items.sort_by(|a, b| {
            (kind_order(def_kind(a.d)), &a.d.name, a.d.line).cmp(&(kind_order(def_kind(b.d)), &b.d.name, b.d.line))
        });
    }

    // 検索インデックス
    let mut search = String::from("[");
    for (k, (mn, m)) in mods.iter().enumerate() {
        if k > 0 {
            search.push(',');
        }
        search.push_str(&format!("[{},[", json_str(&page_file(mn))));
        for (i, it) in m.items.iter().enumerate() {
            if i > 0 {
                search.push(',');
            }
            search.push_str(&format!("[{},{}]", json_str(&it.d.name), json_str(def_kind(it.d))));
        }
        search.push_str("]]");
    }
    search.push(']');

    let mut pages = Pages::new();
    let brand = cfg.title.to_string();
    let std_nav = if !cfg.is_std && std_ok { " <a href=\"std/index.html\">標準ライブラリ</a>" } else { "" };

    // 各モジュールのページ
    for (mn, m) in &mods {
        let ctx = Ctx { links: &links, cur_module: mn, is_std: cfg.is_std };
        let mut uniq = Unique(BTreeSet::new());
        let mut body = format!("<h1>モジュール {}</h1>", esc(mn));
        // 目次
        body.push_str("<nav class=\"toc\"><ul>");
        for it in &m.items {
            if owner_in(it.d, m).is_none() {
                body.push_str(&format!(
                    "<li><span class=\"kind\">{}</span> <a href=\"#{}\">{}</a></li>",
                    def_kind(it.d),
                    sanitize(&it.d.name),
                    esc(&it.d.name)
                ));
            }
        }
        body.push_str("</ul></nav>");
        let sections: [(&str, &[&str]); 5] = [
            ("型・列挙型・トレイト", &["type", "enum", "trait"]),
            ("関数", &["fn"]),
            ("定数・静的変数", &["const", "static"]),
            ("外部関数", &["extern"]),
            ("", &[]),
        ];
        for (title, kinds) in sections {
            if kinds.is_empty() {
                continue;
            }
            let sel: Vec<&&Item> = m
                .items
                .iter()
                .filter(|it| kinds.contains(&def_kind(it.d)) && owner_in(it.d, m).is_none())
                .collect();
            if sel.is_empty() {
                continue;
            }
            body.push_str(&format!("<h2>{}</h2>", title));
            for it in sel {
                let mut s = String::new();
                def_block(it, 3, &ctx, &g, &mut uniq, &mut s, true);
                // 型・トレイトに属するメソッド
                let mut inner = String::new();
                if matches!(it.d.kw.as_str(), "type" | "enum" | "trait") {
                    let ms: Vec<&&Item> = m
                        .items
                        .iter()
                        .filter(|x| x.d.kw == "fn" && owner_of(&x.d.name) == Some(it.d.name.as_str()))
                        .collect();
                    if !ms.is_empty() {
                        inner.push_str("<h5>メソッド</h5>");
                        for x in ms {
                            def_block(x, 4, &ctx, &g, &mut uniq, &mut inner, false);
                            inner.push_str("</article>");
                        }
                    }
                }
                s.push_str(&inner);
                s.push_str("</article>");
                body.push_str(&s);
            }
        }
        let title = format!("{} - {}", mn, cfg.title);
        pages.insert(page_file(mn), page(&title, &brand, "index.html", std_nav, &body, &search));
    }

    // 索引ページ
    let mut body = format!("<h1>{}</h1>", esc(cfg.title));
    if cfg.is_std {
        body.push_str("<p class=\"note\">標準ライブラリ (spec/stdlib の規範シグネチャ) の API ドキュメント。</p>");
    } else if cfg.private {
        body.push_str("<p class=\"note\">非公開の定義を含みます (--private)。</p>");
    }
    if std_ok && !cfg.is_std {
        body.push_str("<p><a href=\"std/index.html\">標準ライブラリのドキュメント</a></p>");
    }
    body.push_str("<h2>モジュール</h2><table><tr><th>モジュール</th><th>定義</th></tr>");
    for (mn, m) in &mods {
        let count = |k: &[&str]| m.items.iter().filter(|x| k.contains(&def_kind(x.d)) && owner_in(x.d, m).is_none()).count();
        let ntypes = count(&["type", "enum", "trait"]);
        let nfns = m.items.iter().filter(|x| x.d.kw == "fn").count();
        body.push_str(&format!(
            "<tr><td><a href=\"{}\">{}</a></td><td>型・トレイト {} / 関数 {}</td></tr>",
            page_file(mn),
            esc(mn),
            ntypes,
            nfns
        ));
    }
    body.push_str("</table>");
    if mods.is_empty() {
        body.push_str("<p class=\"note\">表示する定義がありません。</p>");
    }
    pages.insert("index.html".into(), page(cfg.title, &brand, "index.html", std_nav, &body, &search));
    pages
}

/// 型・列挙型・トレイトに属するメソッドなら、その持ち主の名前。
fn owner_in<'a>(d: &'a Def, m: &Module) -> Option<&'a str> {
    if d.kw != "fn" {
        return None;
    }
    let o = owner_of(&d.name)?;
    m.items.iter().any(|x| x.d.name == o && matches!(x.d.kw.as_str(), "type" | "enum" | "trait")).then_some(o)
}
