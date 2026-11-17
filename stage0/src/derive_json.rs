//! `derive [ToJson, FromJson]` (仕様 4.3 / 8.3): 構造体・列挙の JSON 入出力を Kasane のソースに展開する。
//!
//! 展開した文面は `Program` の組み立て (`cli::build_program`) で型の定義のあとに足し、通常の定義と同じ経路 (検査・
//! インタプリタ・VM・KIR / LLVM) で動かす。文面の行番号は型の行から始める (診断が型の近くを指すように)。
//!
//! - `ToJson`: `impl std.json.ToJson for T` と `fn T.to_json(self: &T, out: ..)`。構造体は `{"フィールド名": 値, ..}`、
//!   列挙は `"Variant"` (ペイロードなし)、`{"Variant": 値}` (ペイロード 1 つ)、`{"Variant": [値, ..]}` (2 つ以上)。
//!   フィールドの出力はそのフィールドの型の `to_json` を呼ぶ (トレイトの動的な解決。型パラメータは `T: ToJson` を境界に足す)。
//! - `FromJson`: 印のトレイト `impl std.json.FromJson for T` と、関連関数 `fn T.from_json(doc: &std.json.Doc, v: std.json.Value)`。
//!   トレイトのメソッドは `self` が必須 (3.2) なので、解読は型ごとの関連関数で、フィールドの型から文面を決めて展開する
//!   (整数・`bool`・`alloc.string.String`・`Vec`・`Option` は `std.json.decode_*` と補助関数、名義型は `<型>.from_json`)。

use crate::check::{items, st, tag, tel};
use crate::parser::V;
use crate::program::{Def, Program};

const INTS: &[&str] = &["i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32", "u64", "usize"];

const OUT_TY: &str = "&mut alloc.string.String<std.alloc.System>";
const TO_RET: &str = "Result<(), alloc.alloc.AllocError>";

/// `FromJson` の解読を展開できる型の形。
#[derive(Clone, Debug)]
enum Shape {
    Int(String),
    Bool,
    Text,
    Vec(Box<Shape>),
    Opt(Box<Shape>),
    Named(String),
}

fn fqn(p: &Program, file: &str, name: &str) -> String {
    let segs: Vec<String> = name.split('.').map(String::from).collect();
    p.expand(&segs, file).join(".")
}

fn type_args(t: &V) -> Vec<&V> {
    items(tel(t, 2)).iter().filter(|a| matches!(a, V::L(_))).collect()
}

fn is_system(p: &Program, file: &str, t: &V) -> bool {
    tag(t) == "named" && fqn(p, file, &st(tel(t, 1))) == "std.alloc.System"
}

fn is_user_nominal(p: &Program, n: &str) -> bool {
    p.types.get(n).map_or(false, |&i| matches!(p.defs[i].kw.as_str(), "type" | "enum"))
}

fn no_form(what: &str) -> String {
    format!(
        "the field type {} has no JSON form (integers, bool, alloc.string.String, alloc.vec.Vec, Option and types that implement it)",
        what
    )
}

fn shape(p: &Program, file: &str, t: &V) -> Result<Shape, String> {
    match tag(t).as_str() {
        "prim" => {
            let n = st(tel(t, 1));
            if INTS.contains(&n.as_str()) {
                Ok(Shape::Int(n))
            } else if n == "bool" {
                Ok(Shape::Bool)
            } else {
                Err(no_form(&format!("`{}`", n)))
            }
        }
        "named" => {
            let n = fqn(p, file, &st(tel(t, 1)));
            let args = type_args(t);
            match n.as_str() {
                "alloc.string.String" if args.len() == 1 && is_system(p, file, args[0]) => Ok(Shape::Text),
                "alloc.vec.Vec" if args.len() == 2 && is_system(p, file, args[1]) => Ok(Shape::Vec(Box::new(shape(p, file, args[0])?))),
                "Option" | "core.option.Option" if args.len() == 1 => Ok(Shape::Opt(Box::new(shape(p, file, args[0])?))),
                "alloc.string.String" | "alloc.vec.Vec" => {
                    Err(format!("derive [FromJson] needs the allocator std.alloc.System in `{}`", n))
                }
                _ if args.is_empty() && is_user_nominal(p, &n) => Ok(Shape::Named(n)),
                _ => Err(no_form(&format!("`{}`", n))),
            }
        }
        _ => Err(no_form("(a tuple, array, reference or other type)")),
    }
}

/// `ToJson` のフィールドの型が出力できる形か (`to_json` の実装の有無は一般の E2036 が見る)。
fn to_json_problem(p: &Program, file: &str, params: &[String], t: &V) -> Option<String> {
    match tag(t).as_str() {
        "prim" => {
            let n = st(tel(t, 1));
            (!INTS.contains(&n.as_str()) && n != "bool").then(|| no_form(&format!("`{}`", n)))
        }
        "named" => {
            let n = fqn(p, file, &st(tel(t, 1)));
            if params.contains(&n) {
                return None;
            }
            let args = type_args(t);
            match n.as_str() {
                "Result" | "core.result.Result" => Some(no_form("`Result`")),
                "alloc.vec.Vec" | "Option" | "core.option.Option" => {
                    args.first().and_then(|a| to_json_problem(p, file, params, a))
                }
                _ => None,
            }
        }
        _ => Some(no_form("(a tuple, array, reference or other type)")),
    }
}

fn payload_types(d: &Def) -> Vec<&V> {
    if d.kw == "type" {
        d.fields.iter().map(|f| &f.1).collect()
    } else {
        d.variants.iter().flat_map(|v| v.1.iter()).collect()
    }
}

fn type_params(d: &Def) -> Vec<String> {
    d.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.clone()).collect()
}

/// 展開できない理由 (`derive` したトレイトの短い名前, 理由)。空なら展開できる。
pub(crate) fn problems(p: &Program, d: &Def) -> Vec<(&'static str, String)> {
    let mut out = vec![];
    let params = type_params(d);
    let has_to = d.derive.iter().any(|x| x == "ToJson");
    let has_from = d.derive.iter().any(|x| x == "FromJson");
    if (has_to || has_from) && d.kw == "enum" && d.variants.is_empty() {
        out.push(("ToJson", "an enum without variants has no JSON form".to_string()));
    }
    if (has_to || has_from) && d.generics.iter().any(|g| g.0 != "type" || !items(&g.2).is_empty()) {
        let tr = if has_to { "ToJson" } else { "FromJson" };
        out.push((tr, "only plain type parameters (no lifetime, const or bounded parameter) are supported".to_string()));
    }
    if has_to {
        for t in payload_types(d) {
            if let Some(m) = to_json_problem(p, &d.file, &params, t) {
                out.push(("ToJson", m));
            }
        }
    }
    if has_from {
        if !params.is_empty() {
            out.push(("FromJson", "a type with type parameters cannot derive FromJson (the decoder is an associated function, not a trait method)".to_string()));
        } else {
            for t in payload_types(d) {
                if let Err(m) = shape(p, &d.file, t) {
                    out.push(("FromJson", m));
                }
            }
        }
    }
    out
}

fn ty_text(s: &Shape) -> String {
    match s {
        Shape::Int(n) => n.clone(),
        Shape::Bool => "bool".into(),
        Shape::Text => "alloc.string.String<std.alloc.System>".into(),
        Shape::Vec(x) => format!("alloc.vec.Vec<{}, std.alloc.System>", ty_text(x)),
        Shape::Opt(x) => format!("Option<{}>", ty_text(x)),
        Shape::Named(n) => n.clone(),
    }
}

struct Gen<'a> {
    owner: &'a str,
    helpers: Vec<String>,
}

impl<'a> Gen<'a> {
    /// 値 `v` (`std.json.Value`) を `s` の型に解読する式 (末尾に `?` を含む)。
    fn expr(&mut self, s: &Shape, v: &str) -> String {
        match s {
            Shape::Int(n) => format!("std.json.decode_{}(doc, {})?", n, v),
            Shape::Bool => format!("std.json.decode_bool(doc, {})?", v),
            Shape::Text => format!("std.json.decode_string(doc, {})?", v),
            Shape::Named(n) => format!("{}.from_json(doc, {})?", n, v),
            Shape::Vec(x) => {
                let inner = self.expr(x, "e");
                let k = self.helpers.len();
                let body = format!(
                    "fn {o}.from_json_{k}(doc: &std.json.Doc, v: std.json.Value) -> Result<{t}, std.json.Error> uses [alloc] panics never\n    let len = std.json.decode_len(doc, v)?\n    let mut out: {t} = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    for k in core.iter.range(0usize, len) {{\n        let e = std.json.decode_item(doc, v, k)?\n        out.push({inner})?\n    }}\n    Ok(out)\nend {o}.from_json_{k}\n",
                    o = self.owner,
                    k = k,
                    t = ty_text(s),
                    inner = inner
                );
                self.helpers.push(body);
                format!("{}.from_json_{}(doc, {})?", self.owner, k, v)
            }
            Shape::Opt(x) => {
                let inner = self.expr(x, "v");
                let k = self.helpers.len();
                let body = format!(
                    "fn {o}.from_json_{k}(doc: &std.json.Doc, v: std.json.Value) -> Result<{t}, std.json.Error> uses [alloc] panics never\n    if std.json.is_null(doc, v) {{\n        return Ok(None)\n    }}\n    Ok(Some({inner}))\nend {o}.from_json_{k}\n",
                    o = self.owner,
                    k = k,
                    t = ty_text(s),
                    inner = inner
                );
                self.helpers.push(body);
                format!("{}.from_json_{}(doc, {})?", self.owner, k, v)
            }
        }
    }
}

fn is_pub(d: &Def) -> bool {
    d.header.trim_end().ends_with(" pub")
}

fn to_json_text(d: &Def) -> String {
    let n = &d.name;
    let params = type_params(d);
    let (gen_impl, gen_fn, for_ty) = if params.is_empty() {
        (String::new(), String::new(), n.clone())
    } else {
        let bs = params.iter().map(|t| format!("{}: std.json.ToJson", t)).collect::<Vec<_>>().join(", ");
        (format!("[{}]", bs), format!("[{}]", bs), format!("{}<{}>", n, params.join(", ")))
    };
    let mut s = format!(
        "impl{gi} std.json.ToJson for {ft}\nfn {n}.to_json{gf}(self: &{ft}, out: {out}) -> {ret} uses [alloc] panics never impls std.json.ToJson\n",
        gi = gen_impl,
        gf = gen_fn,
        ft = for_ty,
        n = n,
        out = OUT_TY,
        ret = TO_RET
    );
    let push = |lit: &str| format!("out.push_str({:?})", lit);
    if d.kw == "type" {
        if d.fields.is_empty() {
            s += &format!("    {}\n", push("{}"));
        } else {
            for (i, f) in d.fields.iter().enumerate() {
                let key = format!("{}\"{}\":", if i == 0 { "{" } else { "," }, f.0);
                s += &format!("    {}?\n    self.{}.to_json(out)?\n", push(&key), f.0);
            }
            s += &format!("    {}\n", push("}"));
        }
    } else {
        s += "    match self {\n";
        for (vn, tys) in &d.variants {
            let pat = |k: usize| (0..k).map(|i| format!("p{}", i)).collect::<Vec<_>>().join(", ");
            match tys.len() {
                0 => s += &format!("        {}.{} => {}\n", n, vn, push(&format!("\"{}\"", vn))),
                1 => {
                    s += &format!("        {}.{}(p0) => {{\n", n, vn);
                    s += &format!("            {}?\n            p0.to_json(out)?\n            {}\n        }}\n", push(&format!("{{\"{}\":", vn)), push("}"));
                }
                k => {
                    s += &format!("        {}.{}({}) => {{\n", n, vn, pat(k));
                    s += &format!("            {}?\n", push(&format!("{{\"{}\":[", vn)));
                    for i in 0..k {
                        if i > 0 {
                            s += &format!("            {}?\n", push(","));
                        }
                        s += &format!("            p{}.to_json(out)?\n", i);
                    }
                    s += &format!("            {}\n        }}\n", push("]}"));
                }
            }
        }
        s += "    }\n";
    }
    s += &format!("end {}.to_json\n", n);
    s
}

fn from_json_text(p: &Program, d: &Def) -> Result<String, String> {
    let n = &d.name;
    let mut g = Gen { owner: n, helpers: vec![] };
    let mut body = String::new();
    let vis = if is_pub(d) { " pub" } else { "" };
    if d.kw == "type" {
        let mut inits = vec![];
        for (i, f) in d.fields.iter().enumerate() {
            let sh = shape(p, &d.file, &f.1)?;
            let e = g.expr(&sh, &format!("std.json.decode_field(doc, v, \"{}\", {}usize)?", f.0, i));
            body += &format!("    let f{} = {}\n", i, e);
            inits.push(format!("{}: f{}", f.0, i));
        }
        body += &format!("    Ok({} {{ {} }})\n", n, inits.join(", "));
    } else {
        body += "    let (name, payload) = std.json.decode_variant(doc, v)?\n";
        for (vn, tys) in &d.variants {
            body += &format!("    if name == \"{}\" {{\n", vn);
            match tys.len() {
                0 => body += &format!("        return Ok({}.{})\n", n, vn),
                1 => {
                    let sh = shape(p, &d.file, &tys[0])?;
                    let e = g.expr(&sh, "p");
                    body += &format!("        let p = std.json.decode_payload(payload)?\n        return Ok({}.{}({}))\n", n, vn, e);
                }
                k => {
                    body += &format!("        let p = std.json.decode_payload(payload)?\n        std.json.decode_tuple(doc, p, {}usize)?\n", k);
                    let mut args = vec![];
                    for (i, t) in tys.iter().enumerate() {
                        let sh = shape(p, &d.file, t)?;
                        let e = g.expr(&sh, &format!("e{}", i));
                        body += &format!("        let e{} = std.json.decode_item(doc, p, {}usize)?\n", i, i);
                        args.push(e);
                    }
                    body += &format!("        return Ok({}.{}({}))\n", n, vn, args.join(", "));
                }
            }
            body += "    }\n";
        }
        body += "    Err(std.json.Error.UnknownVariant)\n";
    }
    let mut s = format!("impl std.json.FromJson for {}\n\n", n);
    s += &format!(
        "fn {n}.from_json(doc: &std.json.Doc, v: std.json.Value) -> Result<{n}, std.json.Error> uses [alloc] panics never{vis}\n{body}end {n}.from_json\n",
        n = n,
        vis = vis,
        body = body
    );
    for h in &g.helpers {
        s += "\n";
        s += h;
    }
    Ok(s)
}

/// `d` の `derive [ToJson, FromJson]` が生成する Kasane の文面。展開できない型は `Err` (理由は `problems`)。
pub(crate) fn text_for(p: &Program, d: &Def) -> Result<String, String> {
    if !matches!(d.kw.as_str(), "type" | "enum") {
        return Err("not a type".into());
    }
    let has_to = d.derive.iter().any(|x| x == "ToJson");
    let has_from = d.derive.iter().any(|x| x == "FromJson");
    if !has_to && !has_from {
        return Err("no JSON derive".into());
    }
    if let Some((_, m)) = problems(p, d).into_iter().next() {
        return Err(m);
    }
    let mut parts = vec![];
    if has_to {
        parts.push(to_json_text(d));
    }
    if has_from {
        parts.push(from_json_text(p, d)?);
    }
    Ok(parts.join("\n"))
}

/// `p.defs[from..]` のうち `derive [ToJson, FromJson]` を持つ型について、生成した定義を `p` に足す。
/// `user` に含まれる型から生成した定義の添字は `user` にも足す (検査・実行の対象に入れる)。
pub(crate) fn expand(p: &mut Program, from: usize, user: &mut Vec<usize>) {
    let n = p.defs.len();
    let mut made: Vec<(bool, String, String, Vec<Def>)> = vec![];
    for i in from..n {
        let d = &p.defs[i];
        if !matches!(d.kw.as_str(), "type" | "enum") || !d.derive.iter().any(|x| x == "ToJson" || x == "FromJson") {
            continue;
        }
        let Ok(text) = text_for(p, d) else { continue };
        let padded = format!("{}{}", "\n".repeat(d.line.saturating_sub(1)), text);
        match crate::program::load_file(&d.file, &d.module, &padded) {
            Ok(defs) => made.push((user.contains(&i), d.name.clone(), text, defs)),
            Err(e) => eprintln!("ksn: internal: cannot parse the code derived for {}: {}", d.name, e),
        }
    }
    for (is_user, name, text, defs) in made {
        p.derived_json.insert(name, text);
        for mut def in defs {
            def.end_line = def.end_line.max(def.line);
            if is_user {
                user.push(p.defs.len());
            }
            p.add_def(def);
        }
    }
}
