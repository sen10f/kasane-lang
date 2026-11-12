//! 構造化した定義 (Def) と定義表 (Program)。Python 参照実装の Def / Interp(add_def, resolve_global) に対応する。

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::defs;
use crate::lexer;
use crate::parser::{parse_body, Parser, V};

pub type R<T> = Result<T, String>;

/// `?` が型の違うエラーを変換するときに使うトレイト (4.1)。
pub const FROM_TRAIT: &str = "core.convert.From";

#[derive(Debug, Clone, Default)]
pub struct Def {
    pub kw: String,
    pub name: String,
    pub line: usize,
    pub end_line: usize,
    pub file: String,
    pub module: String,
    pub header: String,
    /// (種別, 名前, 境界)。種別は type / uses / panics / const / life
    pub generics: Vec<(String, String, V)>,
    pub params: Vec<(String, V)>,
    pub ret: V,
    /// `const fn` (コンパイル時評価可能関数)。
    pub is_const: bool,
    pub uses: Vec<String>,
    /// `uses` / `panics` を省略した関数 (本体から推論する)。
    pub inferred: bool,
    pub panics: Option<String>,
    pub stack: Option<u64>,
    pub derive: Vec<String>,
    pub linear: bool,
    pub fields: Vec<(String, V, bool)>,
    pub variants: Vec<(String, Vec<V>)>,
    pub body: Option<V>,
    pub init: Option<V>,
    pub has_impls: bool,
    /// `impls <トレイト>` のトレイト名 (基底。`has_impls` のときだけ意味を持つ)。
    pub impls_trait: String,
    /// `impls <トレイト><型引数>` の型引数 (`core.convert.From<E1>` の `E1`)。
    pub impls_args: Vec<V>,
    pub methods: Vec<String>,
    /// トレイトのメソッドのシグネチャ (名前, 引数, 戻り値)。解析できたものだけ。
    pub trait_sigs: Vec<(String, Vec<(String, V)>, V)>,
    /// トレイトの `requires` スーパートレイト (基底名, 型引数)。解析できたものだけ (5.1)。
    pub requires: Vec<(String, V)>,
    /// ジェネリクス `[..]` を持つトレイトメソッドの名前 (dyn 安全性 5.1)。
    pub generic_methods: Vec<String>,
    pub impl_trait: String,
    /// `extern` の `link "名前"` (省略時は FQN の末尾)
    pub link: String,
    pub impl_for: V,
}

impl Def {
    pub fn gnames(&self) -> HashSet<String> {
        self.generics.iter().filter(|g| g.0 != "life").map(|g| g.1.clone()).collect()
    }
    pub fn is_method(&self) -> bool {
        self.params.first().map_or(false, |p| p.0 == "self")
    }
}

fn items(v: &V) -> &[V] {
    match v {
        V::L(xs) => xs,
        _ => &[],
    }
}
fn text(v: &V) -> String {
    match v {
        V::S(s) | V::N(s) => s.clone(),
        other => other.dump(),
    }
}
fn conv_generics(v: &V) -> Vec<(String, String, V)> {
    items(v)
        .iter()
        .map(|g| {
            let f = items(g);
            (text(&f[0]), text(&f[1]), f[2].clone())
        })
        .collect()
}
fn conv_params(v: &V) -> Vec<(String, V)> {
    items(v).iter().map(|p| (text(&items(p)[0]), items(p)[1].clone())).collect()
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

/// ファイル1つを Def の列にする。module / layer は呼び出し側が与える。
pub fn load_file(path: &str, module: &str, src: &str) -> R<Vec<Def>> {
    let sig = path.ends_with(".sig");
    let lines: Vec<&str> = src.split('\n').collect();
    let mut out = Vec::new();
    for sp in defs::scan_with(src, sig)? {
        let l = lines[sp.line - 1];
        let mut p = Parser::new(line_tokens(l, sp.line)?);
        let mut d = Def {
            kw: sp.kw.clone(),
            name: sp.name.clone(),
            line: sp.line,
            end_line: sp.end_line,
            file: path.to_string(),
            module: module.to_string(),
            header: l.to_string(),
            ret: V::Nil,
            impl_for: V::Nil,
            ..Default::default()
        };
        let body_lines: Vec<&str> = if sp.end_line > 0 { lines[sp.line..sp.end_line - 1].to_vec() } else { vec![] };
        match sp.kw.as_str() {
            "import" | "test" => {
                p.i = 1;
                d.name = p.fqn()?;
            }
            "fn" => {
                p.expect_kw("fn")?;
                d.name = p.fqn()?;
                d.generics = conv_generics(&p.generics()?);
                p.expect_op("(")?;
                d.params = conv_params(&p.params()?);
                p.expect_op(")")?;
                p.expect_op("->")?;
                d.ret = p.ty()?;
                d.is_const = p.accept_kw("const");
                if p.is_kw("uses") {
                    p.expect_kw("uses")?;
                    d.uses = items(&p.effect_set()?).iter().map(text).collect();
                    p.expect_kw("panics")?;
                    d.panics = Some(p.panic_val()?);
                } else {
                    d.inferred = true;
                }
                if p.accept_kw("stack") {
                    d.stack = text(&V::N(crate::parser::split_number(&p.cur().text, false)?.0)).parse().ok();
                    p.i += 1;
                }
                if p.accept_kw("impls") {
                    let tr = p.trait_ref()?;
                    d.impls_trait = text(&items(&tr)[0]);
                    d.impls_args = items(&items(&tr)[1]).to_vec();
                    d.has_impls = true;
                }
            }
            "type" | "enum" => {
                p.i = 1;
                d.name = p.fqn()?;
                d.generics = conv_generics(&p.generics()?);
                if p.accept_kw("layout") {
                    p.name_tok()?;
                }
                if p.accept_kw("derive") {
                    p.expect_op("[")?;
                    loop {
                        d.derive.push(p.name_tok()?);
                        if !p.accept_op(",") {
                            break;
                        }
                    }
                    p.expect_op("]")?;
                }
                d.linear = p.accept_kw("linear");
                for bl in &body_lines {
                    let st = bl.trim();
                    if st.is_empty() {
                        continue;
                    }
                    let mut bp = Parser::new(line_tokens(st, 1)?);
                    if sp.kw == "type" {
                        let fnm = bp.name_tok()?;
                        bp.expect_op(":")?;
                        let t = bp.ty()?;
                        let pb = bp.accept_kw("pub");
                        d.fields.push((fnm, t, pb));
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
                        d.variants.push((vn, tys));
                    }
                }
            }
            "trait" => {
                p.i = 1;
                d.name = p.fqn()?;
                d.generics = conv_generics(&p.generics()?);
                if p.accept_kw("requires") {
                    loop {
                        let tr = p.trait_ref()?;
                        let f = items(&tr);
                        if f.len() >= 2 {
                            d.requires.push((text(&f[0]), f[1].clone()));
                        }
                        if !p.accept_op(",") {
                            break;
                        }
                    }
                }
                for bl in &body_lines {
                    let st = bl.trim();
                    if st.is_empty() {
                        continue;
                    }
                    let mut bp = Parser::new(line_tokens(st, 1)?);
                    bp.expect_kw("fn")?;
                    let mname = bp.name_tok()?;
                    d.methods.push(mname.clone());
                    let mut sig_ok = true;
                    match bp.generics() {
                        Ok(g) => {
                            if !items(&g).is_empty() {
                                d.generic_methods.push(mname.clone());
                            }
                        }
                        Err(_) => sig_ok = false,
                    }
                    let sig = if sig_ok {
                        (|| -> R<(Vec<(String, V)>, V)> {
                            bp.expect_op("(")?;
                            let ps = conv_params(&bp.params()?);
                            bp.expect_op(")")?;
                            bp.expect_op("->")?;
                            let r = bp.ty()?;
                            Ok((ps, r))
                        })()
                    } else {
                        Err(String::new())
                    };
                    if let Ok((ps, r)) = sig {
                        d.trait_sigs.push((mname, ps, r));
                    }
                }
            }
            "const" | "static" => {
                p.i = 1;
                d.name = p.fqn()?;
                p.expect_op(":")?;
                d.ret = p.ty()?;
                p.expect_op("=")?;
                let mut init: Vec<lexer::Tok> = p.t[p.i..].to_vec();
                if ends_with_pub(l) {
                    if let Some(k) = init.iter().rposition(|t| t.kind == "kw" && t.text == "pub") {
                        init.remove(k);
                    }
                }
                d.init = Some(Parser::new(init).expr(false)?);
            }
            "extern" => {
                p.i = 1;
                d.name = p.fqn()?;
                p.expect_op("(")?;
                d.params = conv_params(&p.params()?);
                p.expect_op(")")?;
                p.expect_op("->")?;
                d.ret = p.ty()?;
                d.link = d.name.rsplit('.').next().unwrap_or("").to_string();
                if p.cur().text == "link" {
                    p.i += 1;
                    let t = p.cur().text.clone();
                    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
                        d.link = t[1..t.len() - 1].to_string();
                        p.i += 1;
                    }
                }
            }
            "impl" => {
                p.i = 1;
                d.generics = conv_generics(&p.generics()?);
                let tr = p.trait_ref()?;
                d.impl_trait = text(&items(&tr)[0]);
                d.impls_args = items(&items(&tr)[1]).to_vec();
                p.expect_kw("for")?;
                d.impl_for = p.ty()?;
                d.name = l.to_string();
            }
            _ => {}
        }
        if (sp.kw == "fn" && !sig) || sp.kw == "test" {
            let body = body_lines.join("\n") + "\n";
            d.body = Some(parse_body(lexer::lex(&body)?, sp.line)?);
        }
        out.push(d);
    }
    normalize_short_names(&mut out);
    Ok(out)
}

/// 3.3: import 済みモジュールの短縮形 (`net.Error`) を FQN (`app.net.Error`) に展開する。
/// ヘッダの型・トレイト参照と本体の型注釈に適用し、以後の検査は常に FQN だけを見る。
/// 解決順: ジェネリクス名 > import 末尾名 (先頭成分が一致) > そのまま (FQN)。
fn normalize_short_names(defs: &mut [Def]) {
    let mut imps: HashMap<String, String> = HashMap::new();
    for d in defs.iter() {
        if d.kw == "import" {
            imps.insert(d.name.rsplit('.').next().unwrap().to_string(), d.name.clone());
        }
    }
    if imps.is_empty() {
        return;
    }
    for d in defs.iter_mut() {
        if d.kw == "import" {
            continue;
        }
        let gn = d.gnames();
        let ex = |name: &str| -> String { expand_short(name, &imps, &gn) };
        for g in d.generics.iter_mut() {
            if let V::L(bs) = &mut g.2 {
                for b in bs.iter_mut() {
                    norm_trait_ref(b, &ex);
                }
            }
        }
        for p in d.params.iter_mut() {
            norm_types(&mut p.1, &ex);
        }
        norm_types(&mut d.ret, &ex);
        norm_types(&mut d.impl_for, &ex);
        for f in d.fields.iter_mut() {
            norm_types(&mut f.1, &ex);
        }
        for v in d.variants.iter_mut() {
            for t in v.1.iter_mut() {
                norm_types(t, &ex);
            }
        }
        for s in d.trait_sigs.iter_mut() {
            for p in s.1.iter_mut() {
                norm_types(&mut p.1, &ex);
            }
            norm_types(&mut s.2, &ex);
        }
        for r in d.requires.iter_mut() {
            r.0 = ex(&r.0);
            norm_types(&mut r.1, &ex);
        }
        if !d.impl_trait.is_empty() {
            d.impl_trait = ex(&d.impl_trait);
        }
        if !d.impls_trait.is_empty() {
            d.impls_trait = ex(&d.impls_trait);
        }
        if let Some(b) = d.body.as_mut() {
            norm_types(b, &ex);
        }
        if let Some(b) = d.init.as_mut() {
            norm_types(b, &ex);
        }
    }
}

fn expand_short(name: &str, imps: &HashMap<String, String>, generics: &HashSet<String>) -> String {
    let Some((first, rest)) = name.split_once('.') else { return name.to_string() };
    if generics.contains(first) {
        return name.to_string();
    }
    match imps.get(first) {
        Some(full) => format!("{}.{}", full, rest),
        None => name.to_string(),
    }
}

fn norm_trait_ref(v: &mut V, ex: &dyn Fn(&str) -> String) {
    if let V::L(xs) = v {
        if xs.len() == 2 {
            if let V::S(n) = &xs[0] {
                xs[0] = V::S(ex(n));
            }
            norm_types(&mut xs[1], ex);
        }
    }
}

/// 型の木 (本体の式の木を含む) の `named` と `dyn` の名前を展開する。
fn norm_types(v: &mut V, ex: &dyn Fn(&str) -> String) {
    match v {
        V::L(xs) => {
            let tag = match xs.first() {
                Some(V::S(t)) => t.clone(),
                _ => String::new(),
            };
            if tag == "named" && xs.len() == 3 && matches!(xs[2], V::L(_)) {
                if let V::S(n) = &xs[1] {
                    xs[1] = V::S(ex(n));
                }
            } else if tag == "dyn" && xs.len() == 3 && matches!(xs[1], V::L(_)) {
                if let V::L(tr) = &mut xs[1] {
                    if let Some(V::S(n)) = tr.first() {
                        tr[0] = V::S(ex(n));
                    }
                }
            }
            for x in xs.iter_mut() {
                norm_types(x, ex);
            }
        }
        V::Node(_, fs, _) => {
            for f in fs.iter_mut() {
                norm_types(f, ex);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Fn(usize),
    Sig(usize),
    Const(usize),
    Type(usize),
    /// 列挙子: (enum 名, 列挙子名)。プレリュードは "Option" / "Result"。
    Variant(String, String),
    Native(String),
    Value(String),
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub target: Target,
    #[allow(dead_code)]
    pub name: String,
    pub rest: Vec<String>,
}

impl Resolved {
    #[allow(dead_code)]
    pub fn kind(&self) -> &'static str {
        match self.target {
            Target::Fn(_) => "fn",
            Target::Sig(_) => "sig",
            Target::Const(_) => "const",
            Target::Type(_) => "type",
            Target::Variant(..) => "variant",
            Target::Native(_) => "native",
            Target::Value(_) => "value",
        }
    }
}

const PRIMS: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64",
    "bool", "char", "str",
];
const INTS: &[&str] = &["i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize"];
const NUM_EXTRA: &[&str] = &[
    "abs", "max", "min", "count_ones", "leading_zeros", "trailing_zeros", "successor", "to_f64", "from_str_radix", "parse",
    "to_bits", "from_bits", "rem", "sqrt", "floor", "ceil", "sin", "cos", "tan", "exp", "ln", "powi", "trunc",
    "round_ties_even", "is_nan", "is_finite", "atan2", "nearest_to_f32", "then_some", "is_ascii_alphabetic",
    "is_ascii_digit", "is_ascii_whitespace", "len_utf8", "to_ascii_lowercase", "to_ascii_uppercase", "to_u32",
    "try_from_u32",
];

fn known_num(name: &str) -> bool {
    ["wrapping_", "checked_", "saturating_", "from_", "try_from_", "trunc_to_", "to_"].iter().any(|p| name.starts_with(p))
        || NUM_EXTRA.contains(&name)
}

fn num_const(ty: &str, name: &str) -> bool {
    if INTS.contains(&ty) {
        return matches!(name, "MAX" | "MIN" | "BITS");
    }
    (ty == "f32" || ty == "f64")
        && matches!(name, "INFINITY" | "NEG_INFINITY" | "NAN" | "EPSILON" | "MAX" | "MIN_POSITIVE")
}

#[derive(Default)]
pub struct Program {
    pub defs: Vec<Def>,
    pub funcs: HashMap<String, usize>,
    pub sigs: HashMap<String, usize>,
    pub consts: HashMap<String, usize>,
    pub types: HashMap<String, usize>,
    pub variants: HashMap<String, (usize, usize)>,
    pub natives: HashSet<String>,
    pub imports: HashMap<String, HashMap<String, String>>,
    pub method_idx: HashMap<String, Vec<usize>>,
    pub impls: HashSet<(String, String)>,
    /// `core.convert.From<E1> for E2` の変換メソッド (4.1 の `?`)。E2 の FQN → [(E1 の型表記, メソッドの FQN)]。
    /// `?` が型の違うエラーを受けるとき、検査・インタプリタ・ネイティブ生成がここを引く。
    pub from_impls: HashMap<String, Vec<(String, String)>>,
}

impl Program {
    pub fn add_def(&mut self, d: Def) {
        let idx = self.defs.len();
        match d.kw.as_str() {
            "fn" => {
                if d.body.is_some() {
                    self.funcs.insert(d.name.clone(), idx);
                } else {
                    self.sigs.insert(d.name.clone(), idx);
                }
                if d.has_impls && d.impls_trait == FROM_TRAIT && d.body.is_some() {
                    if let (Some((owner, _)), Some(src)) = (d.name.rsplit_once('.'), d.impls_args.first()) {
                        let key = crate::types::from_v(src, &HashSet::new()).show();
                        self.from_impls.entry(owner.to_string()).or_default().push((key, d.name.clone()));
                    }
                }
                if d.is_method() {
                    let mn = d.name.rsplit('.').next().unwrap().to_string();
                    self.method_idx.entry(mn).or_default().push(idx);
                }
            }
            "extern" => {
                self.sigs.insert(d.name.clone(), idx);
            }
            "const" | "static" => {
                self.consts.insert(d.name.clone(), idx);
            }
            "type" | "enum" | "trait" => {
                self.types.insert(d.name.clone(), idx);
                if d.kw == "enum" {
                    for (i, (vn, _)) in d.variants.iter().enumerate() {
                        self.variants.insert(format!("{}.{}", d.name, vn), (idx, i));
                    }
                }
            }
            "import" => {
                let tail = d.name.rsplit('.').next().unwrap().to_string();
                self.imports.entry(d.file.clone()).or_default().insert(tail, d.name.clone());
            }
            "impl" => {
                let tgt = match &d.impl_for {
                    V::L(xs) if matches!(xs.first(), Some(V::S(k)) if k == "named") => text(&xs[1]),
                    V::L(xs) => text(&xs[0]),
                    other => other.dump(),
                };
                self.impls.insert((d.impl_trait.clone(), tgt));
            }
            _ => {}
        }
        self.defs.push(d);
    }

    pub fn expand(&self, segs: &[String], file: &str) -> Vec<String> {
        if let Some(imps) = self.imports.get(file) {
            if let Some(full) = imps.get(&segs[0]) {
                let mut v: Vec<String> = full.split('.').map(String::from).collect();
                v.extend_from_slice(&segs[1..]);
                return v;
            }
        }
        segs.to_vec()
    }

    pub fn resolve(&self, segs: &[String], file: &str) -> Option<Resolved> {
        if segs.len() == 1 {
            let pre = match segs[0].as_str() {
                "Some" => Some(("Option", "Some")),
                "None" => Some(("Option", "None")),
                "Ok" => Some(("Result", "Ok")),
                "Err" => Some(("Result", "Err")),
                _ => None,
            };
            if let Some((en, vn)) = pre {
                return Some(Resolved {
                    target: Target::Variant(en.to_string(), vn.to_string()),
                    name: format!("{}.{}", en, vn),
                    rest: vec![],
                });
            }
        }
        let segs: Vec<String> = if PRIMS.contains(&segs[0].as_str()) {
            let m = if INTS.contains(&segs[0].as_str()) || segs[0] == "f32" || segs[0] == "f64" {
                "num".to_string()
            } else {
                segs[0].clone()
            };
            let mut v = vec!["core".to_string(), m];
            v.extend_from_slice(segs);
            v
        } else {
            self.expand(segs, file)
        };
        for k in (1..=segs.len()).rev() {
            let fq = segs[..k].join(".");
            let rest = segs[k..].to_vec();
            let target = if let Some(&i) = self.funcs.get(&fq) {
                Target::Fn(i)
            } else if let Some(&i) = self.consts.get(&fq) {
                Target::Const(i)
            } else if let Some(&(ei, vi)) = self.variants.get(&fq) {
                Target::Variant(self.defs[ei].name.clone(), self.defs[ei].variants[vi].0.clone())
            } else if self.natives.contains(&fq) {
                Target::Native(fq.clone())
            } else if let Some(&i) = self.types.get(&fq) {
                Target::Type(i)
            } else if let Some(&i) = self.sigs.get(&fq) {
                Target::Sig(i)
            } else {
                continue;
            };
            return Some(Resolved { target, name: fq, rest });
        }
        if segs.len() >= 4
            && segs[0] == "core"
            && matches!(segs[1].as_str(), "num" | "bool" | "char")
            && PRIMS.contains(&segs[2].as_str())
        {
            let nm = format!("{}.{}", segs[2], segs[3]);
            if known_num(&segs[3]) {
                return Some(Resolved { target: Target::Native(nm.clone()), name: nm, rest: segs[4..].to_vec() });
            }
            if num_const(&segs[2], &segs[3]) {
                return Some(Resolved { target: Target::Value(nm.clone()), name: nm, rest: segs[4..].to_vec() });
            }
        }
        None
    }

    pub fn fn_def(&self, r: &Resolved) -> Option<&Def> {
        match r.target {
            Target::Fn(i) | Target::Sig(i) => Some(&self.defs[i]),
            _ => None,
        }
    }
}

#[allow(dead_code)]
pub fn btree<T: Ord + Clone>(xs: &HashSet<T>) -> BTreeSet<T> {
    xs.iter().cloned().collect()
}
