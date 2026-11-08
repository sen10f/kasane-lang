//! 型付きの式推論 (型付き IR の最初の段階)。
//!
//! 関数本体の各式に型 (`Ty`) を与え、レシーバの型に基づいてメソッドの存在・引数の個数と型を検査する。
//! 分からない型は `Ty::Unknown` にして、そこから先は何も言わない (誤検出より取りこぼしを選ぶ)。
//! 結果の型は呼び出しのシグネチャから単一化 (unify) して求める。

use std::collections::{HashMap, HashSet};

use crate::check::{edit_distance, is_nil, items, nf, nline, nname, pat_binds, st, strs, tag, tel, Diag};
use crate::parser::V;
use crate::program::{Def, Program, Target};

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Unknown,
    Never,
    Unit,
    Prim(String),
    Named(String, Vec<Ty>),
    Ref(bool, Box<Ty>),
    Tuple(Vec<Ty>),
    Array(Box<Ty>),
    Slice(Box<Ty>),
    /// ジェネリクスの型パラメータ (シグネチャの中だけに現れる)
    Param(String),
}

impl Ty {
    fn strip(&self) -> &Ty {
        let mut t = self;
        while let Ty::Ref(_, inner) = t {
            t = inner;
        }
        t
    }
    fn show(&self) -> String {
        match self {
            Ty::Unknown => "_".into(),
            Ty::Never => "!".into(),
            Ty::Unit => "()".into(),
            Ty::Prim(p) => p.clone(),
            Ty::Named(n, a) => {
                if a.is_empty() {
                    n.clone()
                } else {
                    format!("{}<{}>", n, a.iter().map(|x| x.show()).collect::<Vec<_>>().join(", "))
                }
            }
            Ty::Ref(m, t) => format!("&{}{}", if *m { "mut " } else { "" }, t.show()),
            Ty::Tuple(ts) => format!("({})", ts.iter().map(|x| x.show()).collect::<Vec<_>>().join(", ")),
            Ty::Array(t) => format!("[{}; _]", t.show()),
            Ty::Slice(t) => format!("[{}]", t.show()),
            Ty::Param(p) => p.clone(),
        }
    }
}

/// 型の構文木 (V) を `Ty` にする。`params` に含まれる名前は型パラメータ。
pub fn from_v(v: &V, params: &HashSet<String>) -> Ty {
    if is_nil(v) {
        return Ty::Unknown;
    }
    match tag(v).as_str() {
        "prim" => Ty::Prim(st(tel(v, 1))),
        "never" => Ty::Never,
        "tuple" => {
            let xs: Vec<Ty> = items(tel(v, 1)).iter().map(|x| from_v(x, params)).collect();
            if xs.is_empty() { Ty::Unit } else { Ty::Tuple(xs) }
        }
        "array" => Ty::Array(Box::new(from_v(tel(v, 1), params))),
        "slice" => Ty::Slice(Box::new(from_v(tel(v, 1), params))),
        "ref" => Ty::Ref(matches!(tel(v, 1), V::B(true)), Box::new(from_v(tel(v, 2), params))),
        "named" => {
            let name = st(tel(v, 1));
            if params.contains(&name) {
                return Ty::Param(name);
            }
            let args: Vec<Ty> = items(tel(v, 2))
                .iter()
                .filter(|a| matches!(a, V::L(_)))
                .map(|a| from_v(a, params))
                .collect();
            Ty::Named(name, args)
        }
        _ => Ty::Unknown,
    }
}

/// `pattern` (シグネチャ側) を `actual` に合わせて、型パラメータの束縛を集める。
fn unify(pattern: &Ty, actual: &Ty, bind: &mut HashMap<String, Ty>) {
    match (pattern, actual) {
        (Ty::Param(p), a) => {
            if !matches!(a, Ty::Unknown | Ty::Param(_)) {
                bind.entry(p.clone()).or_insert_with(|| a.clone());
            }
        }
        (Ty::Ref(_, p), Ty::Ref(_, a)) => unify(p, a, bind),
        (Ty::Ref(_, p), a) => unify(p, a, bind),
        (p, Ty::Ref(_, a)) => unify(p, a, bind),
        (Ty::Named(pn, pa), Ty::Named(an, aa)) if pn == an => {
            for (p, a) in pa.iter().zip(aa) {
                unify(p, a, bind);
            }
        }
        (Ty::Tuple(ps), Ty::Tuple(as_)) => {
            for (p, a) in ps.iter().zip(as_) {
                unify(p, a, bind);
            }
        }
        (Ty::Slice(p), Ty::Slice(a)) | (Ty::Slice(p), Ty::Array(a)) | (Ty::Array(p), Ty::Array(a)) => unify(p, a, bind),
        _ => {}
    }
}

/// 束縛を適用する。束縛のない型パラメータは `Unknown`。
fn subst(t: &Ty, bind: &HashMap<String, Ty>) -> Ty {
    match t {
        Ty::Param(p) => bind.get(p).cloned().unwrap_or(Ty::Unknown),
        Ty::Named(n, a) => Ty::Named(n.clone(), a.iter().map(|x| subst(x, bind)).collect()),
        Ty::Ref(m, x) => Ty::Ref(*m, Box::new(subst(x, bind))),
        Ty::Tuple(xs) => Ty::Tuple(xs.iter().map(|x| subst(x, bind)).collect()),
        Ty::Array(x) => Ty::Array(Box::new(subst(x, bind))),
        Ty::Slice(x) => Ty::Slice(Box::new(subst(x, bind))),
        other => other.clone(),
    }
}

fn concrete(t: &Ty) -> bool {
    match t {
        Ty::Unknown | Ty::Param(_) | Ty::Never => false,
        Ty::Prim(_) | Ty::Unit => true,
        Ty::Named(_, a) => a.iter().all(concrete),
        Ty::Ref(_, x) | Ty::Array(x) | Ty::Slice(x) => concrete(x),
        Ty::Tuple(xs) => xs.iter().all(concrete),
    }
}

const DERIVED: &[&str] = &[
    "clone", "eq", "ne", "cmp", "partial_cmp", "hash", "debug_fmt", "display_fmt", "call", "call_mut", "call_once", "drop",
    "try_clone", "to_string", "lt", "le", "gt", "ge", "min", "max",
];

pub struct TypeCk<'a> {
    p: &'a Program,
    /// 所有者 (型の FQN) → メソッド名 → 定義の添字
    owners: HashMap<String, HashMap<String, Vec<usize>>>,
    trait_methods: HashSet<String>,
    pub diags: Vec<Diag>,
    // 現在の関数
    cur: usize,
    scopes: Vec<HashMap<String, Ty>>,
}

impl<'a> TypeCk<'a> {
    pub fn new(p: &'a Program) -> Self {
        let mut owners: HashMap<String, HashMap<String, Vec<usize>>> = HashMap::new();
        for (i, d) in p.defs.iter().enumerate() {
            if d.kw == "fn" && d.is_method() {
                if let Some((o, m)) = d.name.rsplit_once('.') {
                    owners.entry(o.to_string()).or_default().entry(m.to_string()).or_default().push(i);
                }
            }
        }
        let mut trait_methods = HashSet::new();
        for d in &p.defs {
            if d.kw == "trait" {
                trait_methods.extend(d.methods.iter().cloned());
            }
        }
        TypeCk { p, owners, trait_methods, diags: vec![], cur: 0, scopes: vec![] }
    }

    fn def(&self) -> &'a Def {
        &self.p.defs[self.cur]
    }

    fn diag(&mut self, code: &str, msg: String, line: usize) {
        let d = self.def();
        self.diags.push(Diag { file: d.file.clone(), code: code.into(), fqn: d.name.clone(), line: if line != 0 { line } else { d.line }, msg });
    }

    pub fn check_def(&mut self, di: usize) {
        let d = &self.p.defs[di];
        let Some(body) = &d.body else { return };
        self.cur = di;
        self.scopes = vec![HashMap::new()];
        let gn = d.gnames();
        for (n, t) in &d.params {
            let ty = from_v(t, &gn);
            self.scopes[0].insert(n.clone(), ty);
        }
        self.infer(body);
    }

    fn lookup(&self, name: &str) -> Option<Ty> {
        self.scopes.iter().rev().find_map(|s| s.get(name).cloned())
    }

    fn bind(&mut self, name: &str, ty: Ty) {
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string(), ty);
        }
    }

    // ------------------------------------------------------------ 型の情報
    fn type_def(&self, fqn: &str) -> Option<&'a Def> {
        self.p.types.get(fqn).map(|&i| &self.p.defs[i])
    }

    fn field_ty(&self, recv: &Ty, name: &str) -> Ty {
        let Ty::Named(fqn, args) = recv.strip() else { return Ty::Unknown };
        let Some(td) = self.type_def(fqn) else { return Ty::Unknown };
        if td.kw != "type" {
            return Ty::Unknown;
        }
        let gn = td.gnames();
        let Some(f) = td.fields.iter().find(|f| f.0 == name) else { return Ty::Unknown };
        let t = from_v(&f.1, &gn);
        let order: Vec<&String> = td.generics.iter().filter(|g| g.0 == "type").map(|g| &g.1).collect();
        let mut bind = HashMap::new();
        for (n, a) in order.iter().zip(args) {
            bind.insert((*n).clone(), a.clone());
        }
        subst(&t, &bind)
    }

    /// レシーバの型から、メソッドを探す所有者の列と「その型のメソッド表が完全か」を返す。
    fn owners_for(&self, t: &Ty) -> (Vec<String>, bool) {
        match t.strip() {
            Ty::Prim(p) if p == "str" => (vec!["core.str".into()], true),
            Ty::Prim(p) if p == "bool" => (vec!["core.bool".into()], false),
            Ty::Prim(p) if p == "char" => (vec!["core.char".into()], false),
            Ty::Prim(p) => (vec![format!("core.num.{}", p)], false),
            Ty::Slice(_) | Ty::Array(_) => (vec!["core.slice".into(), "core.array".into()], true),
            Ty::Named(n, _) => match n.as_str() {
                "Option" => (vec!["core.option.Option".into()], true),
                "Result" => (vec!["core.result.Result".into()], true),
                "alloc.vec.Vec" => (vec!["alloc.vec.Vec".into(), "core.slice".into()], true),
                "alloc.string.String" => (vec!["alloc.string.String".into(), "core.str".into()], true),
                "alloc.boxed.Box" => (vec![n.clone()], false),
                _ => (vec![n.clone()], self.p.types.contains_key(n)),
            },
            _ => (vec![], false),
        }
    }

    // ------------------------------------------------------------ メソッド呼び出し
    fn call_method(&mut self, recv: &Ty, name: &str, args: &[Ty], line: usize) -> Ty {
        let (owners, complete) = self.owners_for(recv);
        let mut cands: Vec<usize> = vec![];
        for o in &owners {
            if let Some(m) = self.owners.get(o).and_then(|m| m.get(name)) {
                cands.extend(m);
            }
        }
        if cands.is_empty() {
            let exists_elsewhere = self.p.method_idx.contains_key(name);
            if complete && exists_elsewhere && !DERIVED.contains(&name) && !self.trait_methods.contains(name) {
                let mut names: Vec<&String> = owners.iter().filter_map(|o| self.owners.get(o)).flat_map(|m| m.keys()).collect();
                names.sort();
                names.dedup();
                let best = names
                    .iter()
                    .map(|n| (edit_distance(name, n), *n))
                    .filter(|(d, n)| *d <= (name.len().max(n.len()) / 3).max(1) || n.contains(name) || name.contains(n.as_str()))
                    .min_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(b.1)));
                let hint = best.map(|(_, n)| format!("; did you mean `{}`?", n)).unwrap_or_default();
                self.diag("E1108", format!("type `{}` has no method `{}`{}", recv.strip().show(), name, hint), line);
            }
            return Ty::Unknown;
        }
        let fit: Vec<usize> = cands.iter().copied().filter(|&i| self.p.defs[i].params.len() == args.len() + 1).collect();
        if fit.is_empty() {
            if cands.len() == 1 && complete {
                let d = &self.p.defs[cands[0]];
                self.diag(
                    "E2022",
                    format!("method `{}` takes {} argument(s) but {} given", name, d.params.len() - 1, args.len()),
                    line,
                );
            }
            return Ty::Unknown;
        }
        let d = &self.p.defs[fit[0]];
        let gn = d.gnames();
        let mut bind: HashMap<String, Ty> = HashMap::new();
        let self_ty = from_v(&d.params[0].1, &gn);
        unify(&self_ty, recv, &mut bind);
        for (k, a) in args.iter().enumerate() {
            let pt = from_v(&d.params[k + 1].1, &gn);
            unify(&pt, a, &mut bind);
        }
        if fit.len() == 1 {
            for (k, a) in args.iter().enumerate() {
                let pt = subst(&from_v(&d.params[k + 1].1, &gn), &bind);
                if let (Ty::Prim(want), Ty::Prim(got)) = (pt.strip(), a.strip()) {
                    if want != got && concrete(&pt) && concrete(a) {
                        self.diag(
                            "E2023",
                            format!("argument {} of `{}`: expected `{}`, found `{}`", k + 1, name, want, got),
                            line,
                        );
                    }
                }
            }
        }
        let gn_ret = from_v(&d.ret, &gn);
        subst(&gn_ret, &bind)
    }

    // ------------------------------------------------------------ 式
    fn infer(&mut self, e: &V) -> Ty {
        let f = nf(e);
        match nname(e) {
            "Block" => {
                self.scopes.push(HashMap::new());
                let mut last = Ty::Unit;
                let stmts = items(&f[0]);
                for (i, s) in stmts.iter().enumerate() {
                    let t = self.stmt(s);
                    if i + 1 == stmts.len() && nname(s) == "ExprStmt" {
                        last = t;
                    }
                }
                self.scopes.pop();
                last
            }
            "Lit" => match st(&f[0]).as_str() {
                "int" => match &f[2] {
                    V::S(s) => Ty::Prim(s.clone()),
                    _ => Ty::Unknown,
                },
                "float" => match &f[1] {
                    V::S(s) => Ty::Prim(s.clone()),
                    _ => Ty::Unknown,
                },
                "bool" => Ty::Prim("bool".into()),
                "char" => Ty::Prim("char".into()),
                "byte" => Ty::Prim("u8".into()),
                "str" => Ty::Ref(false, Box::new(Ty::Prim("str".into()))),
                "bstr" => Ty::Ref(false, Box::new(Ty::Slice(Box::new(Ty::Prim("u8".into()))))),
                "unit" => Ty::Unit,
                _ => Ty::Unknown,
            },
            "Path" => self.path(&strs(&f[0])),
            "Call" => self.call(&f[0], items(&f[1]), nline(e)),
            "MCall" => {
                let recv = self.infer(&f[0]);
                let args: Vec<Ty> = items(&f[2]).iter().map(|a| self.infer(a)).collect();
                self.call_method(&recv, &st(&f[1]), &args, nline(e))
            }
            "Field" => {
                let r = self.infer(&f[0]);
                self.field_ty(&r, &st(&f[1]))
            }
            "Try" => match self.infer(&f[0]).strip() {
                Ty::Named(n, a) if n == "Result" || n == "Option" => a.first().cloned().unwrap_or(Ty::Unknown),
                _ => Ty::Unknown,
            },
            "Index" => {
                let t = self.infer(&f[0]);
                self.infer(&f[1]);
                match t.strip() {
                    Ty::Array(x) | Ty::Slice(x) => (**x).clone(),
                    _ => Ty::Unknown,
                }
            }
            "Unary" => {
                let t = self.infer(&f[1]);
                match st(&f[0]).as_str() {
                    "&" => Ty::Ref(false, Box::new(t)),
                    "&mut" => Ty::Ref(true, Box::new(t)),
                    "*" => match t {
                        Ty::Ref(_, x) => *x,
                        _ => Ty::Unknown,
                    },
                    _ => t,
                }
            }
            "Bin" => {
                let a = self.infer(&f[1]);
                let b = self.infer(&f[2]);
                match st(&f[0]).as_str() {
                    "==" | "!=" | "<" | "<=" | ">" | ">=" | "&&" | "||" => Ty::Prim("bool".into()),
                    _ => {
                        if matches!(a, Ty::Prim(_)) { a } else { b }
                    }
                }
            }
            "StructLit" => {
                for x in items(&f[1]) {
                    self.infer(&items(x)[1]);
                }
                let segs: Vec<String> = st(&f[0]).split('.').map(String::from).collect();
                let fq = self.p.expand(&segs, &self.def().file).join(".");
                if self.p.types.contains_key(&fq) { Ty::Named(fq, vec![]) } else { Ty::Unknown }
            }
            "TupleE" => {
                let ts: Vec<Ty> = items(&f[0]).iter().map(|x| self.infer(x)).collect();
                if ts.is_empty() { Ty::Unit } else { Ty::Tuple(ts) }
            }
            "ArrayE" => {
                let ts: Vec<Ty> = items(&f[0]).iter().map(|x| self.infer(x)).collect();
                Ty::Array(Box::new(ts.into_iter().find(|t| *t != Ty::Unknown).unwrap_or(Ty::Unknown)))
            }
            "If" => {
                self.infer(&f[0]);
                let a = self.infer(&f[1]);
                let b = if is_nil(&f[2]) { Ty::Unit } else { self.infer(&f[2]) };
                if a == Ty::Never { b } else { a }
            }
            "Match" => {
                let s = self.infer(&f[0]);
                let mut out = Ty::Unknown;
                for arm in items(&f[1]) {
                    self.scopes.push(HashMap::new());
                    self.bind_pat(&items(arm)[0], &s);
                    let t = self.infer(&items(arm)[1]);
                    self.scopes.pop();
                    if out == Ty::Unknown && t != Ty::Never {
                        out = t;
                    }
                }
                out
            }
            "For" => {
                let it = self.infer(&f[1]);
                self.scopes.push(HashMap::new());
                let elem = match it.strip() {
                    Ty::Named(n, a) if n == "core.iter.Range" => a.first().cloned().unwrap_or(Ty::Unknown),
                    _ => Ty::Unknown,
                };
                self.bind_pat(&f[0], &elem);
                self.infer(&f[2]);
                self.scopes.pop();
                Ty::Unit
            }
            "Closure" => {
                self.scopes.push(HashMap::new());
                let gn = self.def().gnames();
                for p in items(&f[0]) {
                    let ty = from_v(&items(p)[1], &gn);
                    self.bind(&st(&items(p)[0]), ty);
                }
                self.infer(&f[3]);
                self.scopes.pop();
                Ty::Unknown
            }
            "Return" | "Break" | "Continue" => {
                if nname(e) == "Return" && !is_nil(&f[0]) {
                    self.infer(&f[0]);
                }
                Ty::Never
            }
            _ => {
                // While / Loop / Unsafe / その他: 子の式をたどる (パターンは除く)。
                for c in crate::check::children(e) {
                    if !crate::check::is_pat(c) {
                        self.infer(c);
                    }
                }
                Ty::Unknown
            }
        }
    }

    fn stmt(&mut self, s: &V) -> Ty {
        let f = nf(s);
        match nname(s) {
            "Let" => {
                let rhs = self.infer(&f[3]);
                let gn = self.def().gnames();
                let ty = if !is_nil(&f[2]) { from_v(&f[2], &gn) } else { rhs };
                self.bind_pat(&f[0], &ty);
                Ty::Unit
            }
            "Assign" => {
                self.infer(&f[0]);
                self.infer(&f[1]);
                Ty::Unit
            }
            _ => self.infer(&f[0]),
        }
    }

    fn bind_pat(&mut self, pat: &V, ty: &Ty) {
        let f = nf(pat);
        match nname(pat) {
            "PBind" => self.bind(&st(&f[0]), ty.clone()),
            "PRef" => {
                let inner = ty.strip().clone();
                self.bind_pat(&f[0], &inner)
            }
            "PTuple" => {
                let ts: Vec<Ty> = match ty.strip() {
                    Ty::Tuple(ts) => ts.clone(),
                    _ => vec![],
                };
                for (i, p) in items(&f[0]).iter().enumerate() {
                    let t = ts.get(i).cloned().unwrap_or(Ty::Unknown);
                    self.bind_pat(p, &t);
                }
            }
            "PVariant" => {
                let segs = strs(&f[0]);
                let payload: Vec<Ty> = self.variant_payload(&segs, ty);
                for (i, p) in items(&f[1]).iter().enumerate() {
                    let t = payload.get(i).cloned().unwrap_or(Ty::Unknown);
                    self.bind_pat(p, &t);
                }
            }
            _ => {
                let mut b = vec![];
                pat_binds(pat, &mut b);
                for n in b {
                    self.bind(&n, Ty::Unknown);
                }
            }
        }
    }

    fn variant_payload(&self, segs: &[String], ty: &Ty) -> Vec<Ty> {
        let name = segs.last().cloned().unwrap_or_default();
        let args = match ty.strip() {
            Ty::Named(_, a) => a.clone(),
            _ => vec![],
        };
        if segs.len() == 1 {
            match name.as_str() {
                "Some" | "Ok" => return vec![args.first().cloned().unwrap_or(Ty::Unknown)],
                "Err" => return vec![args.get(1).cloned().unwrap_or(Ty::Unknown)],
                _ => {}
            }
        }
        if let Some(r) = self.p.resolve(segs, &self.def().file) {
            if let Target::Variant(en, vn) = &r.target {
                if let Some(td) = self.type_def(en) {
                    if let Some((_, payload)) = td.variants.iter().find(|v| &v.0 == vn) {
                        let gn = td.gnames();
                        let order: Vec<&String> = td.generics.iter().filter(|g| g.0 == "type").map(|g| &g.1).collect();
                        let mut bind = HashMap::new();
                        for (n, a) in order.iter().zip(&args) {
                            bind.insert((*n).clone(), a.clone());
                        }
                        return payload.iter().map(|t| subst(&from_v(t, &gn), &bind)).collect();
                    }
                }
            }
        }
        vec![]
    }

    /// パス式の型。局所変数 (+ フィールド連鎖) か、定数、列挙子。
    fn path(&mut self, segs: &[String]) -> Ty {
        if let Some(mut t) = self.lookup(&segs[0]) {
            for s in &segs[1..] {
                t = self.field_ty(&t, s);
            }
            return t;
        }
        match self.p.resolve(segs, &self.def().file) {
            Some(r) if r.rest.is_empty() => match r.target {
                Target::Const(i) => from_v(&self.p.defs[i].ret, &HashSet::new()),
                Target::Variant(en, vn) => {
                    if en == "Option" && vn == "None" {
                        Ty::Named("Option".into(), vec![Ty::Unknown])
                    } else if self.p.types.contains_key(&en) {
                        Ty::Named(en, vec![])
                    } else {
                        Ty::Unknown
                    }
                }
                _ => Ty::Unknown,
            },
            _ => Ty::Unknown,
        }
    }

    fn call(&mut self, callee: &V, args: &[V], line: usize) -> Ty {
        let argt: Vec<Ty> = args.iter().map(|a| self.infer(a)).collect();
        if nname(callee) != "Path" {
            self.infer(callee);
            return Ty::Unknown;
        }
        let segs = strs(&nf(callee)[0]);
        // `v.items.push(x)` のように、局所変数から始まるパスはメソッド呼び出し。
        if self.lookup(&segs[0]).is_some() {
            if segs.len() < 2 {
                return Ty::Unknown;
            }
            let mut t = self.lookup(&segs[0]).unwrap();
            for s in &segs[1..segs.len() - 1] {
                t = self.field_ty(&t, s);
            }
            return self.call_method(&t, &segs[segs.len() - 1], &argt, line);
        }
        let Some(r) = self.p.resolve(&segs, &self.def().file) else { return Ty::Unknown };
        if !r.rest.is_empty() {
            return Ty::Unknown;
        }
        match r.target {
            Target::Variant(en, vn) => {
                let a0 = argt.first().cloned().unwrap_or(Ty::Unknown);
                match (en.as_str(), vn.as_str()) {
                    ("Option", "Some") => Ty::Named("Option".into(), vec![a0]),
                    ("Result", "Ok") => Ty::Named("Result".into(), vec![a0, Ty::Unknown]),
                    ("Result", "Err") => Ty::Named("Result".into(), vec![Ty::Unknown, a0]),
                    _ if self.p.types.contains_key(&en) => Ty::Named(en, vec![]),
                    _ => Ty::Unknown,
                }
            }
            Target::Native(ref fq) | Target::Value(ref fq) if self.p.sigs.contains_key(fq) => {
                let i = self.p.sigs[fq];
                self.call_def(i, &argt)
            }
            Target::Fn(i) | Target::Sig(i) => self.call_def(i, &argt),
            _ => Ty::Unknown,
        }
    }

    fn call_def(&mut self, i: usize, argt: &[Ty]) -> Ty {
        {
            {
                let d = &self.p.defs[i];
                let gn = d.gnames();
                let mut bind = HashMap::new();
                for (k, a) in argt.iter().enumerate() {
                    if let Some(p) = d.params.get(k) {
                        unify(&from_v(&p.1, &gn), a, &mut bind);
                    }
                }
                subst(&from_v(&d.ret, &gn), &bind)
            }
        }
    }
}
