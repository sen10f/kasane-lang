//! 規則 C (参照の段数、仕様 4.2): 期待する型と実際の型の参照の段数は、受け手の位置 (変換の印 `AutoRef`/`AutoDeref`) 以外で
//! 暗黙に調整しない。式の形ごとの場合分けは「期待する型がどこから来るか」(引数・欄・列挙子のペイロード・`let` の注釈・
//! `return` の戻り値型・二項演算の片側) を引くところだけで、食い違いの判定 `arg_conflict` / `value_conflict` /
//! `operand_depth` は型だけを見る。旧い検査 (`types.rs` の `arg_ref_conflict` ほか) と同じコード・同じ行・同じ文面で出す。
//!
//! 期待する型を引く元: 呼び出しの解決先 (`TExpr::res`、無ければ経路の解決と `call_res`)、型引数の実体 (`TExpr::targs`)、
//! 構造体リテラルの型の引数、`let` の束縛の型、関数・クロージャの戻り値型。

use std::collections::HashMap;

use crate::check::Diag;
use crate::program::{Def, Program, Target};
use crate::tast::*;
use crate::types::{deref_advice, from_v, ref_hint, Ty, TypeCk};

pub fn check(p: &Program, user: &[usize], tast: &Tast, tk: &TypeCk) -> Vec<Diag> {
    let mut out = vec![];
    for &di in user {
        let Some(b) = tast.bodies.get(&di) else { continue };
        let mut w = W { p, tk, di, out: &mut out, rets: vec![b.ret.clone()] };
        w.tail(b);
        w.expr(&b.body);
    }
    out
}

struct W<'a> {
    p: &'a Program,
    tk: &'a TypeCk<'a>,
    di: DefId,
    out: &'a mut Vec<Diag>,
    /// 囲んでいる関数・クロージャの戻り値型 (`return` の期待する型)。
    rets: Vec<Ty>,
}

// ------------------------------------------------------------ 型だけを見る判定

fn depth(t: &Ty) -> usize {
    let mut n = 0;
    let mut t = t;
    while let Ty::Ref(_, i) = t {
        n += 1;
        t = i;
    }
    n
}

fn plain(t: &Ty) -> bool {
    matches!(t, Ty::Prim(_) | Ty::Named(..) | Ty::Tuple(_) | Ty::Array(_, _) | Ty::Unit)
}

/// 型の中に決まっていない部分があるか。
fn undet(t: &Ty) -> bool {
    match t {
        Ty::Unknown | Ty::Var(_) => true,
        Ty::Named(_, a) | Ty::Tuple(a) => a.iter().any(undet),
        Ty::Ref(_, x) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => undet(x),
        Ty::Closure { args, ret, .. } => args.iter().any(undet) || undet(ret),
        _ => false,
    }
}

fn subst(t: &Ty, bind: &HashMap<String, Ty>) -> Ty {
    match t {
        Ty::Param(p) => bind.get(p).cloned().unwrap_or(Ty::Unknown),
        Ty::Named(n, a) => Ty::Named(n.clone(), a.iter().map(|x| subst(x, bind)).collect()),
        Ty::Ref(m, x) => Ty::Ref(*m, Box::new(subst(x, bind))),
        Ty::Ptr(m, x) => Ty::Ptr(*m, Box::new(subst(x, bind))),
        Ty::Tuple(xs) => Ty::Tuple(xs.iter().map(|x| subst(x, bind)).collect()),
        Ty::Array(x, n) => Ty::Array(Box::new(subst(x, bind)), *n),
        Ty::Slice(x) => Ty::Slice(Box::new(subst(x, bind))),
        other => other.clone(),
    }
}

impl<'a> W<'a> {
    /// 引数・欄・ペイロードの位置: `want` は宣言の型 (型パラメータを含んでよい)、`inst` は型引数を入れた型。
    fn arg_conflict(&self, want: &Ty, inst: &Ty, got: &Ty) -> Option<(String, String)> {
        // 4.2: クロージャ → `fn(..)` はキャプチャなしのものだけ。
        if let (Ty::Closure { id: None, .. }, Ty::Closure { id: Some(node), .. }) = (want, got) {
            if self.tk.side.closure_caps.get(node).map_or(false, |c| !c.is_empty()) {
                return Some((want.show(), "a closure with captures".to_string()));
            }
        }
        // 4.2: `&[T; N]` → `&[T]` は参照の直下だけ。`Option<&[T]>` の位置に `Option<&[T; N]>` は渡せない。
        if !want.has_param() {
            if let (Ty::Named(n, xs), Ty::Named(m, ys)) = (want, got) {
                if n == m && matches!(n.as_str(), "Option" | "Result") {
                    for (x, y) in xs.iter().zip(ys) {
                        if let (Ty::Ref(_, p), Ty::Ref(_, q)) = (x, y) {
                            if matches!((&**p, &**q), (Ty::Slice(_), Ty::Array(..))) {
                                return Some((want.show(), got.show()));
                            }
                        }
                    }
                }
            }
        }
        let (mut wi, mut gi) = (want, got);
        loop {
            match (wi, gi) {
                (Ty::Ref(_, a), Ty::Ref(_, b)) => {
                    wi = a;
                    gi = b;
                }
                (w, Ty::Ref(..)) if plain(w) && !w.has_param() => return Some((want.show(), got.show())),
                (Ty::Ref(_, a), g) if plain(g) && !undet(g) && !matches!(**a, Ty::Param(_) | Ty::Unknown) && !undet(got) => {
                    return Some((want.show(), got.show()));
                }
                _ => break,
            }
        }
        // 宣言の型が型パラメータそのもの (`value: T`) で `T` が参照でない型に決まっているのに参照を渡す。
        if matches!(want, Ty::Param(_)) && matches!(got, Ty::Ref(..)) && matches!(inst, Ty::Prim(_) | Ty::Named(..) | Ty::Tuple(_) | Ty::Array(..)) && !undet(inst) {
            return Some((inst.show(), got.show()));
        }
        None
    }

    fn owned(&self, t: &Ty) -> bool {
        match t {
            Ty::Named(n, args) if n == "Option" || n == "Result" => args.iter().any(|a| self.owned(a)),
            Ty::Named(n, _) => self.p.types.get(n).map_or(true, |&i| !self.p.defs[i].derive.iter().any(|x| x == "Copy")),
            Ty::Tuple(xs) => xs.iter().any(|x| self.owned(x)),
            Ty::Array(x, _) => self.owned(x),
            _ => false,
        }
    }

    fn owned_clash(&self, want: &Ty, got: &Ty) -> bool {
        match (want, got) {
            (Ty::Ref(_, a), Ty::Ref(_, b)) => self.owned_clash(a, b),
            (x @ (Ty::Named(..) | Ty::Tuple(_) | Ty::Array(_, _)), Ty::Ref(..)) => self.owned(x) && !undet(x),
            (Ty::Named(n, xs), Ty::Named(m, ys)) if n == m && xs.len() == ys.len() => xs.iter().zip(ys.iter()).any(|(x, y)| self.owned_clash(x, y)),
            (Ty::Tuple(xs), Ty::Tuple(ys)) if xs.len() == ys.len() => xs.iter().zip(ys.iter()).any(|(x, y)| self.owned_clash(x, y)),
            _ => false,
        }
    }

    /// 値が要る位置 (`let` の注釈・戻り値) に参照が来ている (E2020)。
    fn value_conflict(&self, want: &Ty, got: &Ty) -> Option<(String, String)> {
        if undet(want) || undet(got) || want.has_param() || got.has_param() {
            return None;
        }
        let (mut wi, mut gi) = (want, got);
        loop {
            match (wi, gi) {
                (Ty::Ref(_, a), Ty::Ref(_, b)) => {
                    wi = a;
                    gi = b;
                }
                (x, Ty::Ref(..)) if plain(x) => return Some((want.show(), got.show())),
                _ => break,
            }
        }
        if self.owned_clash(want, got) {
            return Some((want.show(), got.show()));
        }
        None
    }

    /// 参照の注釈 `&T` に、参照でない値が来ている (`let r: &u64 = 5u64`)。
    fn ref_for_value(&self, want: &Ty, got: &Ty) -> Option<(String, String)> {
        if undet(want) || undet(got) || want.has_param() || got.has_param() {
            return None;
        }
        match (want, got) {
            (Ty::Ref(..), Ty::Prim(_) | Ty::Named(..) | Ty::Tuple(_) | Ty::Array(_, _) | Ty::Unit) => Some((want.show(), got.show())),
            _ => None,
        }
    }

    // ------------------------------------------------------------ 診断

    fn diag(&mut self, code: &str, msg: String, line: usize) {
        let d = &self.p.defs[self.di];
        let line = if line != 0 { line } else { crate::parser::unpack_pos(d.line).0 };
        self.out.push(Diag { file: d.file.clone(), code: code.into(), fqn: d.name.clone(), line, col: 0, msg });
    }

    fn arg(&mut self, what: String, raw: &Ty, inst: &Ty, a: &TExpr, line: usize) {
        if let Some((want, got)) = self.arg_conflict(raw, inst, &a.ty) {
            let copy = self.tk.ty_copy_ref(raw);
            self.diag("E2023", format!("{}: expected `{}`, found `{}`{}", what, want, got, ref_hint(&want, &got, copy)), line);
        }
    }

    /// 宣言 `d` の型パラメータ (型・定数) に `targs` を順に入れた束縛。
    fn bind(d: &Def, targs: &[Ty]) -> HashMap<String, Ty> {
        let names: Vec<&String> = d.generics.iter().filter(|g| g.0 == "type" || g.0 == "const").map(|g| &g.1).collect();
        if names.len() != targs.len() {
            return HashMap::new();
        }
        names.into_iter().cloned().zip(targs.iter().cloned()).collect()
    }

    /// 関数・メソッド `d` の引数の列 (`skip` = 先頭の `self` の数) を、実引数 `args` と照らす。
    fn check_args(&mut self, d: &Def, skip: usize, targs: &[Ty], args: &[TExpr], name: &str, line: usize) {
        let gn = d.gnames();
        let bind = Self::bind(d, targs);
        for (k, a) in args.iter().enumerate() {
            let Some(pr) = d.params.get(k + skip) else { break };
            let raw = from_v(&pr.1, &gn);
            let inst = subst(&raw, &bind);
            self.arg(format!("argument {} of `{}`", k + 1, name), &raw, &inst, a, line);
        }
    }

    // ------------------------------------------------------------ 木の歩き

    /// 関数の末尾の値と戻り値型 (E2020)。
    fn tail(&mut self, b: &TBody) {
        let (want, got) = (&b.ret, &b.body.ty);
        let line = match &b.body.kind {
            TKind::Block(stmts) => match stmts.last() {
                Some(TStmt::Expr(e)) => e.line,
                Some(TStmt::Let { pat, .. }) => pat.line,
                Some(TStmt::Assign { place, .. }) => place.line,
                None => 0,
            },
            _ => b.body.line,
        };
        if let Some((w, g)) = self.value_conflict(want, got) {
            self.diag("E2020", format!("type mismatch in return value: expected {}, found {}{}", w, g, deref_advice(self.tk.ty_copy_ref(want))), line);
        }
    }

    fn expr(&mut self, e: &TExpr) {
        match &e.kind {
            TKind::Call { callee, args } => {
                self.call(e, callee, args);
                for a in args {
                    self.expr(a);
                }
            }
            TKind::MCall { recv, name, args } => {
                self.expr(recv);
                let d = match &e.res {
                    Some(Callee::Def(d)) => Some(*d),
                    Some(Callee::Methods(v)) if v.len() == 1 => Some(v[0]),
                    _ => None,
                };
                if let Some(d) = d {
                    let d = &self.p.defs[d];
                    if d.is_method() {
                        self.check_args(d, 1, &e.targs, args, name, e.line);
                    }
                }
                for a in args {
                    self.expr(a);
                }
            }
            TKind::StructLit { name, fields } => {
                self.struct_lit(e, name, fields);
                for (_, x) in fields {
                    self.expr(x);
                }
            }
            TKind::Binary { op, lhs, rhs } => {
                if op != "&&" && op != "||" {
                    let known = |t: &Ty| !matches!(t, Ty::Unknown | Ty::Var(_) | Ty::Param(_) | Ty::Never);
                    if known(lhs.ty.strip()) && known(rhs.ty.strip()) {
                        let (na, nb) = (depth(&lhs.ty), depth(&rhs.ty));
                        if na != nb {
                            let side = if na > nb { "left" } else { "right" };
                            self.diag(
                                "E2024",
                                format!(
                                    "operands of `{}` are `{}` and `{}` (the {} operand has more references): a reference is not dereferenced implicitly: write `*c` (an operator does not dereference, 4.2)",
                                    op,
                                    lhs.ty.show(),
                                    rhs.ty.show(),
                                    side
                                ),
                                e.line,
                            );
                        }
                    }
                }
                self.expr(lhs);
                self.expr(rhs);
            }
            TKind::Unary { op, operand } => {
                if matches!(op.as_str(), "-" | "!") && matches!(&operand.ty, Ty::Ref(_, i) if matches!(&**i, Ty::Prim(p) if p != "str")) {
                    self.diag(
                        "E2024",
                        format!("operand of unary `{}` is `{}`: a reference is not dereferenced implicitly: write `*c` (an operator does not dereference, 4.2)", op, operand.ty.show()),
                        e.line,
                    );
                }
                self.expr(operand);
            }
            TKind::Field { base, .. } => self.expr(base),
            TKind::Index { base, index } => {
                self.expr(base);
                self.expr(index);
            }
            TKind::Block(stmts) => {
                for s in stmts {
                    match s {
                        TStmt::Let { pat, init } => {
                            if let (Some(i), Some(info)) = (init, &pat.info) {
                                self.let_ann(pat, &info.ty, i);
                            }
                            if let Some(i) = init {
                                self.expr(i);
                            }
                        }
                        TStmt::Expr(x) => self.expr(x),
                        TStmt::Assign { place, value } => {
                            self.expr(place);
                            self.expr(value);
                        }
                    }
                }
            }
            TKind::If { cond, then, els } => {
                self.expr(cond);
                self.expr(then);
                if let Some(x) = els {
                    self.expr(x);
                }
            }
            TKind::Match { scrut, arms } => {
                self.expr(scrut);
                for a in arms {
                    if let Some(g) = &a.guard {
                        self.expr(g);
                    }
                    self.expr(&a.body);
                }
            }
            TKind::Closure { body, .. } => {
                let ret = match &e.ty {
                    Ty::Closure { ret, .. } => (**ret).clone(),
                    _ => Ty::Unknown,
                };
                self.rets.push(ret);
                self.expr(body);
                self.rets.pop();
            }
            TKind::Other { name, children } => {
                if *name == "Return" {
                    if let (Some(v), Some(ret)) = (children.first(), self.rets.last().cloned()) {
                        if let Some((w, g)) = self.value_conflict(&ret, &v.ty) {
                            self.diag("E2020", format!("type mismatch in return value: expected {}, found {}{}", w, g, deref_advice(self.tk.ty_copy_ref(&ret))), e.line);
                        }
                    }
                }
                for c in children {
                    self.expr(c);
                }
            }
            TKind::Lit(_) | TKind::Path(_) => {}
        }
    }

    /// `let` の注釈 (束縛の型) と初期化式の型。注釈が無ければ同じ型なので何も出ない。
    fn let_ann(&mut self, pat: &TPat, ann: &Ty, init: &TExpr) {
        if let Some((w, g)) = self.value_conflict(ann, &init.ty) {
            self.diag("E2020", format!("type mismatch in let binding: expected {}, found {}{}", w, g, deref_advice(self.tk.ty_copy_ref(ann))), pat.line);
        } else if matches!(init.kind, TKind::Lit(_) | TKind::Path(_) | TKind::Call { .. } | TKind::MCall { .. } | TKind::StructLit { .. }) {
            if let Some((w, g)) = self.ref_for_value(ann, &init.ty) {
                self.diag("E2020", format!("type mismatch in let binding: expected {}, found {}; take a reference with `&`", w, g), pat.line);
            }
        }
    }

    fn call(&mut self, e: &TExpr, callee: &TExpr, args: &[TExpr]) {
        let TKind::Path(segs) = &callee.kind else { return };
        let file = &self.p.defs[self.di].file;
        // 経路そのものが解決できれば、それが解決先 (自由関数・列挙子)。解決できず `x.m(..)` の経路形なら、印の `Def` か
        // `call_res` のメソッドで、先頭の `self` は経路の `x`。
        let by_path = self.p.resolve(segs, file).filter(|r| r.rest.is_empty()).map(|r| r.target);
        let target = match (&by_path, &e.res) {
            (Some(t), _) => Some(t.clone()),
            (None, Some(Callee::Variant(en, vn))) => Some(Target::Variant(en.clone(), vn.clone())),
            _ => None,
        };
        match target {
            Some(Target::Fn(i)) | Some(Target::Sig(i)) => {
                let d = &self.p.defs[i];
                self.check_args(d, 0, &e.targs, args, &d.name, e.line);
            }
            Some(Target::Variant(en, vn)) => {
                let Some(&ti) = self.p.types.get(&en) else { return };
                let td = &self.p.defs[ti];
                let Some((_, payload)) = td.variants.iter().find(|v| v.0 == vn) else { return };
                let gn = td.gnames();
                let order: Vec<&String> = td.generics.iter().filter(|g| g.0 == "type").map(|g| &g.1).collect();
                // 列挙子の型 (戻り値) の型引数から束縛を作る。
                let bind: HashMap<String, Ty> = match &e.ty {
                    Ty::Named(n, a) if *n == en && a.len() == order.len() => order.iter().map(|s| (*s).clone()).zip(a.iter().cloned()).collect(),
                    _ => HashMap::new(),
                };
                for (pt, a) in payload.iter().zip(args) {
                    let raw = from_v(pt, &gn);
                    let inst = subst(&raw, &bind);
                    self.arg(format!("payload of `{}`", vn), &raw, &inst, a, e.line);
                    // 所有型のペイロードに参照を渡す (Copy のスカラーの参照は通す)。
                    if !inst.has_param() && !a.ty.has_param() && self.owned_clash(&inst, &a.ty) {
                        self.diag("E2020", format!("type mismatch in payload of `{}`: expected {}, found {}", vn, inst.show(), a.ty.show()), e.line);
                    }
                }
            }
            _ => {
                let d = match &e.res {
                    Some(Callee::Def(d)) => Some(*d),
                    Some(Callee::Methods(v)) if v.len() == 1 => Some(v[0]),
                    _ => None,
                };
                if let Some(d) = d {
                    let d = &self.p.defs[d];
                    if d.is_method() {
                        self.check_args(d, 1, &e.targs, args, segs.last().map(|s| s.as_str()).unwrap_or(""), e.line);
                    }
                }
            }
        }
    }

    fn struct_lit(&mut self, e: &TExpr, name: &str, fields: &[(String, TExpr)]) {
        let segs: Vec<String> = name.split('.').map(String::from).collect();
        let fq = self.p.expand(&segs, &self.p.defs[self.di].file).join(".");
        let Some(&ti) = self.p.types.get(&fq) else { return };
        let td = &self.p.defs[ti];
        if td.kw != "type" {
            return;
        }
        let gn = td.gnames();
        let order: Vec<&String> = td.generics.iter().filter(|g| g.0 == "type").map(|g| &g.1).collect();
        let bind: HashMap<String, Ty> = match &e.ty {
            Ty::Named(n, a) if *n == fq && a.len() == order.len() => order.iter().map(|s| (*s).clone()).zip(a.iter().cloned()).collect(),
            _ => HashMap::new(),
        };
        for (fname, v) in fields {
            let Some((_, fv, _)) = td.fields.iter().find(|(n, _, _)| n == fname) else { continue };
            let raw = from_v(fv, &gn);
            let inst = subst(&raw, &bind);
            self.arg(format!("field `{}` of `{}`", fname, fq), &raw, &inst, v, e.line);
        }
    }
}
