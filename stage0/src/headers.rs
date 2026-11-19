//! 定義ヘッダの解析 (stage0)。Python 参照実装 ksn/syntax.py の parse_source / parse_header_fn と同じ構造を作る。

use crate::defs;
use crate::lexer;
use crate::parser::{nd, opt, s, split_number, strs, Parser, R, V};

impl Parser {
    pub(crate) fn generics(&mut self) -> R<V> {
        let mut out = Vec::new();
        if !self.is_op("[") {
            return Ok(V::L(out));
        }
        self.i += 1;
        loop {
            let c = self.cur().clone();
            if c.kind == "life" {
                out.push(V::L(vec![s("life"), V::S(c.text), V::Nil]));
                self.i += 1;
            } else if self.is_kw("const") {
                self.i += 1;
                let nm = self.name_tok()?;
                self.expect_op(":")?;
                let t = self.ty()?;
                out.push(V::L(vec![s("const"), V::S(nm), t]));
            } else {
                let nm = self.name_tok()?;
                let mut bounds = Vec::new();
                let mut kind = "type";
                if self.accept_op(":") {
                    if self.is_kw("uses") {
                        self.i += 1;
                        kind = "uses";
                    } else if self.is_kw("panics") {
                        self.i += 1;
                        kind = "panics";
                    } else {
                        loop {
                            if self.is_kw("linear") {
                                self.i += 1;
                                bounds.push(s("linear"));
                            } else if self.cur().kind == "life" {
                                bounds.push(V::S(self.cur().text.clone()));
                                self.i += 1;
                            } else {
                                bounds.push(self.trait_ref()?);
                            }
                            if !self.accept_op("+") {
                                break;
                            }
                        }
                    }
                }
                out.push(V::L(vec![s(kind), V::S(nm), V::L(bounds)]));
            }
            if !self.accept_op(",") {
                break;
            }
        }
        self.expect_op("]")?;
        Ok(V::L(out))
    }

    pub(crate) fn header_params(&mut self) -> R<V> {
        self.params()
    }
}

fn line_tokens(line: &str, ln: usize) -> R<Vec<lexer::Tok>> {
    let mut toks = lexer::lex(&format!("{}\n", line))?;
    for t in toks.iter_mut() {
        t.line = ln;
    }
    Ok(toks)
}

fn ends_with_pub(line: &str) -> bool {
    let t = line.trim_end();
    if t.ends_with("\"pub\"") || !t.ends_with("pub") {
        return false;
    }
    let before = &t[..t.len() - 3];
    !before.chars().last().map_or(false, |c| c.is_alphanumeric() || c == '_')
}

fn fn_header(p: &mut Parser) -> R<String> {
    p.expect_kw("fn")?;
    let name = p.fqn()?;
    let g = p.generics()?;
    p.expect_op("(")?;
    let params = p.header_params()?;
    p.expect_op(")")?;
    p.expect_op("->")?;
    let ret = p.ty()?;
    let cst = p.accept_kw("const");
    let (uses, pn) = if p.is_kw("uses") {
        p.expect_kw("uses")?;
        let uses = p.effect_set()?;
        p.expect_kw("panics")?;
        (uses, p.panic_val()?)
    } else {
        (V::L(vec![]), "inferred".to_string())
    };
    let mut stack = V::Nil;
    let mut depth = V::Nil;
    loop {
        if matches!(stack, V::Nil) && p.accept_kw("stack") {
            stack = V::N(split_number(&p.cur().text, false)?.0);
            p.i += 1;
        } else if matches!(depth, V::Nil) && p.accept_kw("depth") {
            depth = V::N(split_number(&p.cur().text, false)?.0);
            p.i += 1;
        } else {
            break;
        }
    }
    let mut impls = V::Nil;
    if p.accept_kw("impls") {
        impls = p.trait_ref()?;
    }
    let pubf = p.accept_kw("pub");
    Ok(nd("fn", vec![V::S(name), g, params, ret, V::B(cst), uses, V::S(pn), stack, impls, V::B(pubf), depth]).dump())
}

fn type_enum_header(p: &mut Parser, kw: &'static str, body: &[&str]) -> R<String> {
    p.i = 1;
    let name = p.fqn()?;
    let g = p.generics()?;
    let mut layout = V::Nil;
    if p.accept_kw("layout") {
        p.name_tok()?;
        layout = s("c");
    }
    let mut derive = Vec::new();
    if p.accept_kw("derive") {
        p.expect_op("[")?;
        loop {
            derive.push(V::S(p.name_tok()?));
            if !p.accept_op(",") {
                break;
            }
        }
        p.expect_op("]")?;
    }
    let linear = p.accept_kw("linear");
    let pubf = p.accept_kw("pub");
    let mut items = Vec::new();
    for bl in body {
        let st = bl.trim();
        if st.is_empty() {
            continue;
        }
        let mut bp = Parser::new(line_tokens(st, 0)?);
        if kw == "type" {
            let fnm = bp.name_tok()?;
            bp.expect_op(":")?;
            let t = bp.ty()?;
            let pb = bp.accept_kw("pub");
            items.push(V::L(vec![V::S(fnm), t, V::B(pb)]));
        } else {
            let vn = bp.name_tok()?;
            let mut tys = Vec::new();
            if bp.accept_op("(") {
                tys.push(bp.ty()?);
                while bp.accept_op(",") {
                    tys.push(bp.ty()?);
                }
                bp.expect_op(")")?;
            }
            items.push(V::L(vec![V::S(vn), V::L(tys)]));
        }
    }
    Ok(nd(kw, vec![V::S(name), g, layout, V::L(derive), V::B(linear), V::B(pubf), V::L(items)]).dump())
}

fn trait_header(p: &mut Parser, body: &[&str]) -> R<String> {
    p.i = 1;
    let name = p.fqn()?;
    let g = p.generics()?;
    let mut req = Vec::new();
    if p.accept_kw("requires") {
        loop {
            req.push(p.trait_ref()?);
            if !p.accept_op(",") {
                break;
            }
        }
    }
    let pubf = p.accept_kw("pub");
    let mut ms = Vec::new();
    for bl in body {
        let st = bl.trim();
        if st.is_empty() {
            continue;
        }
        let mut bp = Parser::new(line_tokens(st, 0)?);
        bp.expect_kw("fn")?;
        let mn = bp.name_tok()?;
        let mg = bp.generics()?;
        bp.expect_op("(")?;
        let params = bp.header_params()?;
        bp.expect_op(")")?;
        bp.expect_op("->")?;
        let ret = bp.ty()?;
        bp.expect_kw("uses")?;
        let uses = bp.effect_set()?;
        bp.expect_kw("panics")?;
        let pn = bp.panic_val()?;
        ms.push(V::L(vec![V::S(mn), mg, params, ret, uses, V::S(pn)]));
    }
    Ok(nd("trait", vec![V::S(name), g, V::L(req), V::B(pubf), V::L(ms)]).dump())
}

fn const_header(p: &mut Parser, kw: &'static str, line: &str) -> R<String> {
    p.i = 1;
    let name = p.fqn()?;
    p.expect_op(":")?;
    let ret = p.ty()?;
    p.expect_op("=")?;
    let mut init: Vec<lexer::Tok> = p.t[p.i..].to_vec();
    let pubf = ends_with_pub(line);
    if pubf {
        if let Some(k) = init.iter().rposition(|t| t.kind == "kw" && t.text == "pub") {
            init.remove(k);
        }
    }
    let e = Parser::new(init).expr(false)?;
    Ok(nd(kw, vec![V::S(name), ret, e, V::B(pubf)]).dump())
}

fn extern_header(p: &mut Parser) -> R<String> {
    p.i = 1;
    let name = p.fqn()?;
    p.expect_op("(")?;
    let params = p.header_params()?;
    p.expect_op(")")?;
    p.expect_op("->")?;
    let ret = p.ty()?;
    let mut link = V::Nil;
    if p.accept_kw("link") {
        let c = p.cur().clone();
        let inner = &c.text[1..c.text.len() - 1];
        let v: String = crate::parser::unescape(inner)?.into_iter().collect();
        link = V::S(crate::parser::hex(v.as_bytes()));
    }
    let mut out = nd("extern", vec![V::S(name), params, ret]).dump();
    out.truncate(out.len() - 1);
    out.push(' ');
    out.push_str(&link.dump());
    out.push(')');
    Ok(out)
}

fn impl_header(p: &mut Parser) -> R<String> {
    p.i = 1;
    let g = p.generics()?;
    let tr = p.trait_ref()?;
    p.expect_kw("for")?;
    let t = p.ty()?;
    Ok(nd("impl", vec![g, tr, t]).dump())
}

/// ファイル内の全定義について "kw<TAB>name<TAB>dump" の行を返す。
pub fn headers(src: &str, sig: bool) -> R<Vec<String>> {
    let lines: Vec<&str> = src.split('\n').collect();
    let mut out = Vec::new();
    for d in defs::scan_with(src, sig)? {
        let l = lines[d.line - 1];
        let mut p = Parser::new(line_tokens(l, d.line)?);
        let body: Vec<&str> = if d.end_line > 0 { lines[d.line..d.end_line - 1].to_vec() } else { vec![] };
        let dump = match d.kw.as_str() {
            "import" => {
                p.i = 1;
                format!("(import {})", p.fqn()?)
            }
            "fn" => fn_header(&mut p)?,
            "type" => type_enum_header(&mut p, "type", &body)?,
            "enum" => type_enum_header(&mut p, "enum", &body)?,
            "trait" => trait_header(&mut p, &body)?,
            "const" => const_header(&mut p, "const", l)?,
            "static" => const_header(&mut p, "static", l)?,
            "extern" => extern_header(&mut p)?,
            "impl" => impl_header(&mut p)?,
            "test" => {
                p.i = 1;
                format!("(test {})", p.fqn()?)
            }
            k => format!("({})", k),
        };
        let _ = (opt, strs);
        out.push(format!("{}\t{}\t{}", d.kw, d.name, dump));
    }
    Ok(out)
}
