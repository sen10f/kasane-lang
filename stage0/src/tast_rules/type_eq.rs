//! 規則 T (型の一致、仕様 4.1・4.2・4.3): 期待する型と実際の型は、暗黙の変換の表にあるもの以外は一致しなければならない。
//! `let` の注釈・戻り値 (末尾と `return`・クロージャ)・引数・構造体の欄・列挙子のペイロード・`if` の両枝と `match` の腕・
//! 代入・二項演算の両辺・配列リテラルの要素・パターン (タプルの要素数・構造体名・列挙子・リテラルの種類) ・`?` のエラー型・
//! 明示型引数の数。式の形ごとの場合分けは「期待する型がどこから来るか」を引くところだけで、食い違いの判定 `unify` は
//! 型だけを見る (旧い検査の `unify_v` を、推論の変数なしの型の上に写したもの)。変換の印は `?` の `ErrInto` を読む。
//! 旧い検査 (`types.rs`) と同じコード・同じ行・同じ文面で出す。
//!
//! 移さないもの (推論の途中でしか分からない): 型が決まらない E2044、整数リテラルの型変数の種類 (`{integer}` に文字列など)、
//! 型の注釈の中の型引数の個数 (`Option<u32, u8>`: 型の構文を見る宣言の検査で、TAST に型の構文が無い)。

use std::collections::HashMap;

use crate::check::Diag;
use crate::program::{Def, FromVariant, Program, Target};
use crate::tast::*;
use crate::types::{conv_hint, from_v, ref_hint, Ty, TypeCk};

pub fn check(p: &Program, user: &[usize], tast: &Tast, tk: &TypeCk) -> Vec<Diag> {
    let mut out = vec![];
    for &di in user {
        let Some(b) = tast.bodies.get(&di) else { continue };
        let mut w = W { p, tk, di, out: &mut out, rets: vec![b.ret.clone()], locals: b.params.iter().map(|q| (q.name.clone(), q.ty.clone())).collect() };
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
    rets: Vec<Ty>,
    /// 局所の名前 → 型 (束縛・引数。影は禁じられているので 1 つの表でよい)。関数値の呼び出しの引数を照らすのに使う。
    locals: HashMap<String, Ty>,
}

// ------------------------------------------------------------ 型だけを見る判定

const INTS: &[&str] = &["u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize"];

fn kind_of(t: &Ty) -> Option<&'static str> {
    match t {
        Ty::Unit => Some("unit"),
        Ty::Prim(_) => Some("prim"),
        Ty::Tuple(_) => Some("tuple"),
        Ty::Array(..) | Ty::Slice(_) => Some("seq"),
        Ty::Closure { .. } => Some("closure"),
        Ty::Named(n, _, _) if n != "alloc.vec.Vec" && n != "alloc.string.String" => Some("named"),
        _ => None,
    }
}

fn open(t: &Ty) -> bool {
    matches!(t, Ty::Unknown | Ty::Var(_) | Ty::Param(_) | Ty::Never)
}

/// `unify_v` と同じ規則で、`want` (期待) に `got` (実際) が入るか。決まっていない部分 (`Unknown`・`Var`) とは一致とみなす。
/// `inv` = 不変の位置、`nest` = 型の構造の中の深さ (中では参照の段数も型の一部)。
fn unify(want: &Ty, got: &Ty, inv: bool, nest: u32) -> Result<(), (Ty, Ty)> {
    let (a, b) = (want, got);
    let err = || Err((a.clone(), b.clone()));
    match (a, b) {
        (Ty::Unknown, _) | (_, Ty::Unknown) | (Ty::Never, _) | (_, Ty::Never) => Ok(()),
        (Ty::Var(_), _) | (_, Ty::Var(_)) => Ok(()),
        (Ty::Param(p), Ty::Param(q)) => {
            if p == q {
                Ok(())
            } else {
                err()
            }
        }
        (Ty::Param(_), t) | (t, Ty::Param(_)) if !matches!(t, Ty::Var(_) | Ty::Ref(..)) => err(),
        (Ty::Param(_), Ty::Ref(..)) => err(),
        (Ty::Ptr(..), Ty::Ref(..)) | (Ty::Ref(..), Ty::Ptr(..)) => Ok(()),
        (Ty::Ptr(m, x), Ty::Ptr(n, y)) => {
            let (m, n) = (*m, *n);
            let (x, y): (&Ty, &Ty) = match (&**x, &**y) {
                (t, Ty::Slice(e) | Ty::Array(e, _)) if !inv && !matches!(t, Ty::Slice(_) | Ty::Array(..) | Ty::Var(_)) => (x, e),
                _ => (x, y),
            };
            unify(x, y, true, nest + 1).map_err(|_| (a.clone(), b.clone()))?;
            if m == n || (!inv && !m && n) {
                Ok(())
            } else {
                err()
            }
        }
        (Ty::Ptr(..), Ty::Prim(_) | Ty::Named(..)) | (Ty::Prim(_) | Ty::Named(..), Ty::Ptr(..)) => err(),
        (Ty::Ref(m, x, _), Ty::Ref(n, y, _)) => {
            let (m, n) = (*m, *n);
            if (m && !n) || (inv && m != n) {
                return err();
            }
            let inner_inv = inv || m;
            if matches!((&**x, &**y), (Ty::Named(n, _, _), Ty::Prim(p)) | (Ty::Prim(p), Ty::Named(n, _, _)) if n == "alloc.string.String" && p == "str") {
                return err();
            }
            if let (Ty::Slice(e), Ty::Array(f, _)) = (&**x, &**y) {
                if inv {
                    return err();
                }
                return unify(e, f, inner_inv, nest + 1);
            }
            unify(x, y, inner_inv, nest + 1)
        }
        (Ty::Ref(..), _) | (_, Ty::Ref(..)) if nest > 0 => err(),
        (Ty::Ref(_, x, _), y) => unify(x, y, inv, nest),
        (x, Ty::Ref(_, y, _)) => unify(x, y, inv, nest),
        (Ty::Prim(p), Ty::Prim(q)) => {
            if p == q {
                Ok(())
            } else {
                err()
            }
        }
        (Ty::Const(x), Ty::Const(y)) => {
            if x == y {
                Ok(())
            } else {
                err()
            }
        }
        (Ty::Dyn(p, _, _), Ty::Dyn(q, _, _)) => {
            if p == q {
                Ok(())
            } else {
                err()
            }
        }
        (Ty::Named(n, xs, _), Ty::Named(m, ys, _)) => {
            if n != m {
                return err();
            }
            if n == "alloc.boxed.Box" {
                if let (Some(x), Some(y)) = (xs.first(), ys.first()) {
                    if !open(x) && !open(y) && matches!(x, Ty::Dyn(..)) != matches!(y, Ty::Dyn(..)) {
                        return err();
                    }
                }
            }
            let arg_inv = inv || !matches!(n.as_str(), "Option" | "Result");
            for (x, y) in xs.iter().zip(ys) {
                unify(x, y, arg_inv, nest + 1)?;
            }
            Ok(())
        }
        (Ty::Tuple(xs), Ty::Tuple(ys)) => {
            if xs.len() != ys.len() {
                return err();
            }
            for (x, y) in xs.iter().zip(ys) {
                unify(x, y, inv, nest + 1)?;
            }
            Ok(())
        }
        (Ty::Closure { args: xs, ret: rx, .. }, Ty::Closure { args: ys, ret: ry, .. }) => {
            if xs.len() != ys.len() {
                return err();
            }
            for (x, y) in xs.iter().zip(ys) {
                unify(y, x, inv, nest + 1).map_err(|(p, q)| (q, p))?;
            }
            unify(rx, ry, inv, nest + 1)
        }
        (Ty::Array(_, Some(m)), Ty::Array(_, Some(n))) if m != n => err(),
        (Ty::Slice(x), Ty::Slice(y)) | (Ty::Array(x, _), Ty::Array(y, _)) => unify(x, y, inv, nest + 1),
        (Ty::Slice(_), Ty::Array(..)) | (Ty::Array(..), Ty::Slice(_)) => err(),
        (Ty::Unit, Ty::Unit) => Ok(()),
        (Ty::Unit, Ty::Named(..) | Ty::Array(..) | Ty::Slice(_)) | (Ty::Named(..) | Ty::Array(..) | Ty::Slice(_), Ty::Unit) => err(),
        (Ty::Named(n, _, _), Ty::Array(..) | Ty::Slice(_)) | (Ty::Array(..) | Ty::Slice(_), Ty::Named(n, _, _)) if n == "alloc.vec.Vec" => err(),
        (x, y) if kind_of(x).is_some() && kind_of(y).is_some() && kind_of(x) != kind_of(y) => err(),
        (Ty::Named(n, _, _), Ty::Prim(p)) | (Ty::Prim(p), Ty::Named(n, _, _)) if n == "alloc.string.String" && p == "str" => Ok(()),
        (Ty::Dyn(..), _) | (_, Ty::Dyn(..)) => Ok(()),
        _ => err(),
    }
}

/// 診断に出す (期待, 実際) の 2 つの型: 矛盾した箇所が基本型・名前の違う型ならそこだけ、他は全体 (旧い `ann_conflict` と同じ)。
fn shown(want: &Ty, got: &Ty, err: &(Ty, Ty)) -> (String, String) {
    let (w, g) = (want.strip(), got.strip());
    if let (Ty::Named(n, xs, _), Ty::Named(m, ys, _)) = (w, g) {
        if n == m && !xs.is_empty() && !ys.is_empty() {
            return (w.show(), g.show());
        }
    }
    match (&err.0, &err.1) {
        (Ty::Prim(a), Ty::Prim(b)) => (a.clone(), b.clone()),
        (Ty::Named(a, _, _), Ty::Named(b, _, _)) if a != b => (a.clone(), b.clone()),
        (Ty::Ref(true, _, _), Ty::Ref(false, _, _)) => (err.0.show(), err.1.show()),
        (Ty::Unit, x @ (Ty::Prim(_) | Ty::Named(..) | Ty::Tuple(_))) => ("()".into(), x.show()),
        (x @ (Ty::Prim(_) | Ty::Named(..) | Ty::Tuple(_)), Ty::Unit) => (x.show(), "()".into()),
        _ => (want.show(), got.show()),
    }
}

fn subst(t: &Ty, bind: &HashMap<String, Ty>) -> Ty {
    match t {
        Ty::Param(p) => bind.get(p).cloned().unwrap_or(Ty::Unknown),
        Ty::Named(n, a, _) => Ty::Named(n.clone(), a.iter().map(|x| subst(x, bind)).collect(), crate::types::Rgs::default()),
        Ty::Ref(m, x, _) => Ty::Ref(*m, Box::new(subst(x, bind)), crate::types::Region::Unk),
        Ty::Ptr(m, x) => Ty::Ptr(*m, Box::new(subst(x, bind))),
        Ty::Tuple(xs) => Ty::Tuple(xs.iter().map(|x| subst(x, bind)).collect()),
        Ty::Array(x, n) => Ty::Array(Box::new(subst(x, bind)), *n),
        Ty::Slice(x) => Ty::Slice(Box::new(subst(x, bind))),
        other => other.clone(),
    }
}

impl<'a> W<'a> {
    fn diag(&mut self, code: &str, msg: String, line: usize) {
        let d = &self.p.defs[self.di];
        let line = if line != 0 { line } else { crate::parser::unpack_pos(d.line).0 };
        self.out.push(Diag { file: d.file.clone(), code: code.into(), fqn: d.name.clone(), line, col: 0, msg });
    }

    /// 型が合わなければ E2020 (`what` は「in return value」など文面の一部)。
    fn eq2020(&mut self, what: &str, want: &Ty, got: &Ty, line: usize) {
        if let Err(err) = unify(want, got, false, 0) {
            let (w, g) = shown(want, got, &err);
            self.diag("E2020", format!("type mismatch in {}: expected {}, found {}{}", what, w, g, conv_hint(&w, &g)), line);
        }
    }

    fn bind(d: &Def, targs: &[Ty]) -> HashMap<String, Ty> {
        let names: Vec<&String> = d.generics.iter().filter(|g| g.0 == "type" || g.0 == "const").map(|g| &g.1).collect();
        if names.len() != targs.len() {
            return HashMap::new();
        }
        names.into_iter().cloned().zip(targs.iter().cloned()).collect()
    }

    /// 引数・欄・ペイロード: `raw` は宣言の型、`inst` は型引数を入れた型。
    fn eq2023(&mut self, what: String, raw: &Ty, inst: &Ty, got: &Ty, line: usize) {
        if let Err(err) = unify(inst, got, false, 0) {
            let (w, g) = shown(inst, got, &err);
            let copy = self.tk.ty_copy_ref(raw);
            self.diag("E2023", format!("{}: expected `{}`, found `{}`{}", what, w, g, ref_hint(&w, &g, copy)), line);
        }
    }

    fn check_args(&mut self, d: &Def, skip: usize, targs: &[Ty], args: &[TExpr], name: &str, line: usize) {
        let gn = d.gnames();
        let bind = Self::bind(d, targs);
        for (k, a) in args.iter().enumerate() {
            let Some(pr) = d.params.get(k + skip) else { break };
            let raw = from_v(&pr.1, &gn);
            let inst = subst(&raw, &bind);
            self.eq2023(format!("argument {} of `{}`", k + 1, name), &raw, &inst, &a.ty, line);
        }
    }

    // ------------------------------------------------------------ 木の歩き

    fn tail(&mut self, b: &TBody) {
        let line = match &b.body.kind {
            TKind::Block(stmts) => match stmts.last() {
                Some(TStmt::Expr(e)) => e.line,
                Some(TStmt::Let { pat, .. }) => pat.line,
                Some(TStmt::Assign { place, .. }) => place.line,
                None => 0,
            },
            _ => b.body.line,
        };
        let what = if self.p.defs[self.di].body.is_none() { "initializer" } else { "return value" };
        self.eq2020(what, &b.ret, &b.body.ty, line);
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
                        self.turbofish(d, e);
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
                if op == "&&" || op == "||" {
                    for x in [lhs, rhs] {
                        if unify(&Ty::Prim("bool".into()), &x.ty, false, 0).is_err() {
                            self.diag("E2020", format!("type mismatch: an operand of `{}` must be `bool`, found {}", op, x.ty.show()), x.line);
                        }
                    }
                } else {
                    if let Err(err) = unify(&lhs.ty, &rhs.ty, false, 0) {
                        let (l, r) = shown(&lhs.ty, &rhs.ty, &err);
                        self.diag("E2024", format!("operands of `{}` have different types: `{}` and `{}`", op, l, r), e.line);
                    }
                    if matches!(op.as_str(), "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^") {
                        if let Ty::Prim(p) = lhs.ty.strip() {
                            let p = p.as_str();
                            let bad = match op.as_str() {
                                "%" => matches!(p, "bool" | "char" | "str" | "f32" | "f64"),
                                "&" | "|" | "^" => matches!(p, "char" | "str" | "f32" | "f64"),
                                _ => matches!(p, "bool" | "char"),
                            };
                            if bad {
                                self.diag("E2020", format!("type mismatch: operator `{}` cannot be applied to `{}`", op, p), e.line);
                            }
                        }
                    }
                }
                self.expr(lhs);
                self.expr(rhs);
            }
            TKind::Unary { op, operand } => {
                if matches!(op.as_str(), "-" | "!") {
                    let d = operand.ty.strip();
                    let neg = op == "-";
                    let bad = match d {
                        Ty::Prim(p) => {
                            if neg {
                                matches!(p.as_str(), "bool" | "char" | "str")
                            } else {
                                matches!(p.as_str(), "char" | "str" | "f32" | "f64")
                            }
                        }
                        Ty::Unit | Ty::Tuple(_) | Ty::Array(..) | Ty::Slice(_) | Ty::Ptr(..) | Ty::Named(..) => true,
                        _ => false,
                    };
                    if bad {
                        self.diag("E2020", format!("type mismatch: unary `{}` cannot be applied to {}", op, d.show()), e.line);
                    }
                }
                if op == "*" && matches!(&operand.ty, Ty::Prim(_) | Ty::Unit | Ty::Tuple(_) | Ty::Array(..) | Ty::Slice(_)) {
                    self.diag("E2020", format!("type mismatch: cannot dereference a value of type {}: `*` applies only to a reference or a raw pointer", operand.ty.show()), e.line);
                }
                self.expr(operand);
            }
            TKind::Field { base, .. } => self.expr(base),
            TKind::Index { base, index } => {
                self.expr(base);
                self.expr(index);
            }
            TKind::Block(stmts) => {
                let n = stmts.len();
                for (i, s) in stmts.iter().enumerate() {
                    match s {
                        TStmt::Let { pat, init } => {
                            if let (Some(i), Some(info)) = (init, &pat.info) {
                                self.eq2020("let binding", &info.ty, &i.ty, pat.line);
                            }
                            self.pat(pat, pat.line);
                            if let Some(i) = init {
                                self.expr(i);
                            }
                        }
                        TStmt::Expr(x) => {
                            // 途中の文の `else` の無い `if` は値を捨てる文 (E2042 が別に見る)。
                            let discarded = i + 1 < n && matches!(&x.kind, TKind::If { els: None, .. });
                            self.expr_in(x, discarded);
                        }
                        TStmt::Assign { place, value } => {
                            if let Err(err) = unify(&place.ty, &value.ty, false, 0) {
                                let (w, g) = shown(&place.ty, &value.ty, &err);
                                self.diag("E2024", format!("assignment: expected `{}`, found `{}`", w, g), value.line);
                            }
                            self.expr(place);
                            self.expr(value);
                        }
                    }
                }
            }
            TKind::If { .. } => self.expr_in(e, false),
            TKind::Match { scrut, arms } => {
                self.expr(scrut);
                let mut first: Option<&Ty> = None;
                for a in arms {
                    self.pat(&a.pat, a.pat.line);
                    if let Some(g) = &a.guard {
                        self.expr(g);
                    }
                    self.expr(&a.body);
                    if matches!(a.body.ty, Ty::Never) {
                        continue;
                    }
                    match first {
                        None => first = Some(&a.body.ty),
                        Some(f) => self.join(f, &a.body.ty, e.line, "`match` arms"),
                    }
                }
            }
            TKind::Closure { params, body, .. } => {
                for q in params {
                    self.locals.insert(q.name.clone(), q.ty.clone());
                }
                let ret = match &e.ty {
                    Ty::Closure { ret, .. } => (**ret).clone(),
                    _ => Ty::Unknown,
                };
                self.eq2020("return value of closure", &ret, &body.ty, e.line);
                self.rets.push(ret);
                self.expr(body);
                self.rets.pop();
            }
            TKind::Other { name, children } => {
                match *name {
                    "Return" => match (children.first(), self.rets.last().cloned()) {
                        (Some(v), Some(ret)) => self.eq2020("return value", &ret, &v.ty, e.line),
                        (None, Some(ret)) => {
                            if unify(&ret, &Ty::Unit, false, 0).is_err() {
                                let want = ret.show();
                                self.diag("E2020", format!("type mismatch in return value: expected {}, found (): `return` without a value in a function that returns {}", want, want), e.line);
                            }
                        }
                        _ => {}
                    },
                    "While" => {
                        if let Some(c) = children.first() {
                            self.cond(c, "`while`", e.line);
                        }
                    }
                    "ArrayE" => {
                        if let Some(first) = children.first() {
                            for (k, x) in children.iter().enumerate().skip(1) {
                                if let Err(err) = unify(&first.ty, &x.ty, false, 0) {
                                    let (w, g) = shown(&first.ty, &x.ty, &err);
                                    self.diag("E2023", format!("array element {}: expected `{}`, found `{}`", k + 1, w, g), e.line);
                                }
                            }
                        }
                    }
                    "Try" => {
                        if let Some(x) = children.first() {
                            self.try_err(e, x);
                        }
                    }
                    _ => {}
                }
                for c in children {
                    self.expr(c);
                }
            }
            TKind::Lit(_) | TKind::Path(_) => {}
        }
    }

    fn cond(&mut self, c: &TExpr, what: &str, line: usize) {
        if unify(&Ty::Prim("bool".into()), &c.ty, false, 0).is_err() {
            self.diag("E2020", format!("type mismatch: the condition of {} must be `bool`, found {}", what, c.ty.show()), line);
        }
    }

    /// `if` / `match` の枝の型を合わせる (`join_arm`)。`a` が期待、`b` が実際。
    fn join(&mut self, a: &Ty, b: &Ty, line: usize, what: &str) {
        if matches!(a, Ty::Unknown | Ty::Param(_)) || matches!(b, Ty::Unknown | Ty::Param(_)) {
            return;
        }
        self.eq2020(what, a, b, line);
        if let (Ty::Ref(m, _, _), Ty::Ref(n, _, _)) = (a, b) {
            if m != n {
                self.diag(
                    "E2020",
                    format!("type mismatch in {}: the branches have different reference mutability: {} and {}; write `&*` / `&mut *` explicitly so both are the same", what, a.show(), b.show()),
                    line,
                );
            }
        }
    }

    fn expr_in(&mut self, e: &TExpr, discarded: bool) {
        let TKind::If { cond, then, els } = &e.kind else {
            self.expr(e);
            return;
        };
        self.cond(cond, "`if`", e.line);
        match els {
            None => {
                if !matches!(then.ty, Ty::Never) && unify(&Ty::Unit, &then.ty, false, 0).is_err() && !discarded {
                    self.diag("E2020", format!("type mismatch in `if` without `else`: expected (), found {}", then.ty.show()), e.line);
                }
            }
            Some(x) => {
                if !matches!(then.ty, Ty::Never) && !matches!(x.ty, Ty::Never) {
                    self.join(&then.ty, &x.ty, e.line, "`if` branches");
                }
            }
        }
        self.expr(cond);
        self.expr(then);
        if let Some(x) = els {
            self.expr(x);
        }
    }

    /// `?`: エラー型は完全一致するか `derive [From]` の `enum` へ包める (`ErrInto` の印が付く)。
    fn try_err(&mut self, e: &TExpr, operand: &TExpr) {
        let Ty::Named(gn, ga, _) = operand.ty.strip() else { return };
        if gn != "Result" && gn != "Option" {
            return;
        }
        let Some(want) = self.rets.last().cloned() else { return };
        match want.strip() {
            Ty::Unknown | Ty::Param(_) | Ty::Var(_) | Ty::Never => {}
            Ty::Named(wn, wa, _) if wn == gn => {
                if gn == "Result" && wa.len() >= 2 && ga.len() >= 2 {
                    let (e_want, e_got) = (&wa[1], &ga[1]);
                    if e.conv.iter().any(|c| matches!(c, Coercion::ErrInto { .. })) {
                        return;
                    }
                    let (Ty::Named(en, _, _), false) = (e_want.strip(), has_open(e_got.strip())) else {
                        return self.try_unify(e, e_want, e_got);
                    };
                    let key = e_got.show();
                    if key != e_want.strip().show() && !matches!(self.p.from_variant(en, &key), FromVariant::None) {
                        return;
                    }
                    self.try_unify(e, e_want, e_got);
                }
            }
            other => {
                self.diag("E2020", format!("type mismatch in `?`: the return type here is `{}`, not `{}`", other.show(), gn), e.line);
            }
        }
    }

    fn try_unify(&mut self, e: &TExpr, e_want: &Ty, e_got: &Ty) {
        if let Err(err) = unify(e_want, e_got, false, 0) {
            let (w, g) = match (&err.0, &err.1) {
                (Ty::Prim(a), Ty::Prim(b)) if a != b => (a.clone(), b.clone()),
                (Ty::Named(a, _, _), Ty::Named(b, _, _)) if a != b => (a.clone(), b.clone()),
                _ => (err.0.show(), err.1.show()),
            };
            self.diag(
                "E2020",
                format!("type mismatch in `?`: expected error type `{w}`, found `{g}`; add a variant `V({g})` to `{w}` (an enum that declares `derive [From]`), or write `map_err`"),
                e.line,
            );
        }
    }

    /// 明示型引数の数 (型・定数・効果・panic の実引数。ライフタイムを除く) は宣言と一致する (E2023)。
    fn turbofish(&mut self, d: &Def, e: &TExpr) {
        let Some(got) = e.tf else { return };
        if got == 0 {
            return;
        }
        let want = d.generics.iter().filter(|g| g.0 != "life").count();
        if want != got {
            self.diag("E2023", format!("`{}` takes {} explicit type argument(s) (types, constants, effects and panics), found {}", d.name, want, got), e.line);
        }
    }

    fn call(&mut self, e: &TExpr, callee: &TExpr, args: &[TExpr]) {
        let TKind::Path(segs) = &callee.kind else { return };
        let file = &self.p.defs[self.di].file;
        // クロージャ・関数値の呼び出し: 引数は関数型の引数と照らす。
        if let Some(Callee::Local(n)) = &e.res {
            let cty = if matches!(callee.ty, Ty::Closure { .. }) { Some(callee.ty.clone()) } else { self.locals.get(n).cloned() };
            if let Some(Ty::Closure { args: cargs, .. }) = cty.as_ref().map(|t| t.strip().clone()) {
                if cargs.len() == args.len() {
                    for (k, (w, a)) in cargs.iter().zip(args).enumerate() {
                        if let Err(err) = unify(w, &a.ty, false, 0) {
                            let (w, g) = shown(w, &a.ty, &err);
                            self.diag("E2023", format!("argument {} of the closure: expected `{}`, found `{}`", k + 1, w, g), e.line);
                        }
                    }
                }
            }
            return;
        }
        let by_path = self.p.resolve(segs, file).filter(|r| r.rest.is_empty()).map(|r| r.target);
        let target = match (&by_path, &e.res) {
            (Some(t), _) => Some(t.clone()),
            (None, Some(Callee::Variant(en, vn))) => Some(Target::Variant(en.clone(), vn.clone())),
            _ => None,
        };
        match target {
            Some(Target::Fn(i)) | Some(Target::Sig(i)) => {
                let d = &self.p.defs[i];
                self.turbofish(d, e);
                self.check_args(d, 0, &e.targs, args, &d.name, e.line);
            }
            Some(Target::Variant(en, vn)) => {
                let Some(&ti) = self.p.types.get(&en) else { return };
                let td = &self.p.defs[ti];
                let Some((_, payload)) = td.variants.iter().find(|v| v.0 == vn) else { return };
                let gn = td.gnames();
                let order: Vec<&String> = td.generics.iter().filter(|g| g.0 == "type").map(|g| &g.1).collect();
                let bind: HashMap<String, Ty> = match &e.ty {
                    Ty::Named(n, a, _) if *n == en && a.len() == order.len() => order.iter().map(|s| (*s).clone()).zip(a.iter().cloned()).collect(),
                    _ => HashMap::new(),
                };
                for (pt, a) in payload.iter().zip(args) {
                    let raw = from_v(pt, &gn);
                    let inst = subst(&raw, &bind);
                    if let Err(err) = unify(&inst, &a.ty, false, 0) {
                        let (w, g) = shown(&inst, &a.ty, &err);
                        self.diag("E2020", format!("type mismatch in payload of `{}`: expected {}, found {}{}", vn, w, g, conv_hint(&w, &g)), e.line);
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
                        self.turbofish(d, e);
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
            Ty::Named(n, a, _) if *n == fq && a.len() == order.len() => order.iter().map(|s| (*s).clone()).zip(a.iter().cloned()).collect(),
            _ => HashMap::new(),
        };
        for (fname, v) in fields {
            let Some((_, fv, _)) = td.fields.iter().find(|(n, _, _)| n == fname) else { continue };
            let raw = from_v(fv, &gn);
            let inst = subst(&raw, &bind);
            self.eq2023(format!("field `{}` of `{}`", fname, fq), &raw, &inst, &v.ty, e.line);
        }
    }

    // ------------------------------------------------------------ パターン (照合する値の型 `PatInfo::ty` と形)

    fn pat(&mut self, q: &TPat, line: usize) {
        if let Some((n, t, _)) = q.info.as_ref().and_then(|i| i.bind.as_ref()) {
            self.locals.insert(n.clone(), t.clone());
        }
        if let Some(info) = &q.info {
            let scrut = &info.ty;
            let base = scrut.strip();
            match q.kind {
                "PTuple" => {
                    let n = q.children.len();
                    let bad = match base {
                        Ty::Tuple(ts) => ts.len() != n,
                        Ty::Prim(_) | Ty::Named(..) | Ty::Array(..) | Ty::Slice(_) | Ty::Unit => true,
                        _ => false,
                    };
                    if bad {
                        self.diag("E2023", format!("a tuple pattern of {} element(s) cannot match a value of type `{}`", n, scrut.show()), line);
                    }
                }
                "PStruct" => {
                    if let Some(r) = self.p.resolve(&q.path, &self.p.defs[self.di].file) {
                        if let (Target::Type(i), true) = (&r.target, r.rest.is_empty()) {
                            let td = &self.p.defs[*i];
                            if td.kw == "type" {
                                let bad = match base {
                                    Ty::Named(n, _, _) => *n != td.name,
                                    Ty::Prim(_) | Ty::Tuple(_) | Ty::Array(..) | Ty::Slice(_) | Ty::Unit => true,
                                    _ => false,
                                };
                                if bad {
                                    self.diag("E2023", format!("the pattern `{}` is a struct of type `{}`, but the value has type `{}`", q.path.join("."), td.name, scrut.show()), line);
                                }
                            }
                        }
                    }
                }
                "PVariant" | "PPath" => {
                    let shape_ok = self.variant_shape(&q.path, scrut, line);
                    if shape_ok {
                        if let Some(want) = self.variant_arity(&q.path) {
                            let got = if q.kind == "PVariant" { q.children.len() } else { 0 };
                            if q.kind == "PVariant" && want != got {
                                self.diag("E2023", format!("the pattern `{}` has {} field(s), but the variant has {}", q.path.join("."), got, want), line);
                            } else if q.kind == "PPath" && want != 0 {
                                self.diag("E2023", format!("the pattern `{}` has no fields, but the variant has {}", q.path.join("."), want), line);
                            }
                        }
                    }
                }
                "PLit" => self.lit_pat(q, scrut, line),
                _ => {}
            }
        }
        for c in &q.children {
            self.pat(c, line);
        }
    }

    /// 列挙子は照合する値の列挙型に属する (`Result` の値を `Some(..)` で照合するのは誤り)。属していれば true。
    fn variant_shape(&mut self, segs: &[String], scrut: &Ty, line: usize) -> bool {
        let name = segs.last().cloned().unwrap_or_default();
        let en: Option<String> = if segs.len() == 1 && matches!(name.as_str(), "Some" | "None") {
            Some("Option".into())
        } else if segs.len() == 1 && matches!(name.as_str(), "Ok" | "Err") {
            Some("Result".into())
        } else if let Some(r) = self.p.resolve(segs, &self.p.defs[self.di].file) {
            match r.target {
                Target::Variant(en, _) if self.p.types.contains_key(&en) => Some(en),
                _ => None,
            }
        } else {
            None
        };
        let Some(en) = en else { return true };
        let mismatch = match scrut.strip() {
            Ty::Named(n, _, _) => *n != en,
            Ty::Prim(_) | Ty::Tuple(_) | Ty::Array(..) | Ty::Slice(_) | Ty::Unit => true,
            _ => false,
        };
        if mismatch {
            self.diag("E2023", format!("the pattern `{}` is a variant of `{}`, but the value has type `{}`", segs.join("."), en, scrut.show()), line);
            return false;
        }
        true
    }

    fn variant_arity(&self, segs: &[String]) -> Option<usize> {
        let name = segs.last()?;
        if segs.len() == 1 {
            match name.as_str() {
                "Some" | "Ok" | "Err" => return Some(1),
                "None" => return Some(0),
                _ => {}
            }
        }
        let r = self.p.resolve(segs, &self.p.defs[self.di].file)?;
        if let Target::Variant(en, vn) = &r.target {
            let td = &self.p.defs[*self.p.types.get(en)?];
            return td.variants.iter().find(|v| &v.0 == vn).map(|v| v.1.len());
        }
        None
    }

    /// リテラルのパターンの種類・接尾辞は、照合する値の型と合う。
    fn lit_pat(&mut self, q: &TPat, scrut: &Ty, line: usize) {
        let Some(kind) = q.lit.first() else { return };
        let base = scrut.strip();
        let concrete = matches!(base, Ty::Prim(_) | Ty::Named(..) | Ty::Tuple(_) | Ty::Array(..) | Ty::Slice(_) | Ty::Unit);
        let is_int = |t: &Ty| matches!(t, Ty::Prim(p) if INTS.contains(&p.as_str()));
        let is_float = |t: &Ty| matches!(t, Ty::Prim(p) if p == "f32" || p == "f64");
        let (ok, what) = match kind.as_str() {
            "str" => (matches!(base, Ty::Prim(p) if p == "str"), "&str"),
            "char" => (matches!(base, Ty::Prim(p) if p == "char"), "char"),
            "byte" => (matches!(base, Ty::Prim(p) if p == "u8"), "u8"),
            "bool" => (matches!(base, Ty::Prim(p) if p == "bool"), "bool"),
            "int" => (is_int(base), "an integer"),
            "float" => (is_float(base), "a float"),
            _ => (true, ""),
        };
        if concrete && !ok {
            self.diag("E2023", format!("the {} literal pattern cannot match a value of type `{}` (expected {})", kind, scrut.show(), what), line);
        }
        if let Some(s) = q.lit.get(1) {
            if let Ty::Prim(w) = base {
                if w != s && INTS.iter().chain(["f32", "f64"].iter()).any(|x| x == w) {
                    self.diag("E2023", format!("literal pattern of type `{}` does not match the scrutinee type `{}`", s, w), line);
                }
            }
        }
    }
}

fn has_open(t: &Ty) -> bool {
    match t {
        Ty::Unknown | Ty::Var(_) => true,
        Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().any(has_open),
        Ty::Ref(_, x, _) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => has_open(x),
        _ => false,
    }
}
