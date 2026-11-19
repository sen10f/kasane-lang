//! 閉包の取り込みの規則 (仕様 4.3・4.4): E1101 の閉包の部分。
//! - `capture [..]` に書いた名前は、閉包を書く位置から見える局所でなければならない (`unresolved name x in the capture list`)。
//! - 本体から見える外側の局所は `capture` に列挙したものだけ。列挙していない外側の局所を本体が使えば、漏れの E1101 (`blocked`)。
//!
//! 見る場所は TAST の式の木だけ。名前の見え方は `Let` / `match` の腕 / `For` / `With` / 引数のパターンの束縛名で数え、
//! 閉包に入るとき、外側の見えている名前のうち引数と `capture` の名前以外を「塞ぐ」(旧い検査の `closure_blocked` と同じ)。
//! 移動 (`move` の取り込みの二重移動・移動後の使用) は `moves.rs`、取り込みが作る借用の重なりは `borrows.rs` (S3E)、
//! `&` の取り込みが外へ逃げる寿命の検査 (E2037・E3030) は S3D の領域の制約が受け持つ。ここでは見ない。
//! 旧い検査 (`check.rs` の `visit` の `Closure` / `Path`) と同じコード・同じ行・同じ文面で出す。

use std::collections::HashSet;

use crate::check::Diag;
use crate::program::Program;
use crate::tast::*;

pub fn check(p: &Program, user: &[usize], tast: &Tast) -> Vec<Diag> {
    let mut out = vec![];
    for &di in user {
        let Some(b) = tast.bodies.get(&di) else { continue };
        let mut w = W { p, di, out: &mut out, scopes: vec![b.params.iter().map(|q| q.name.clone()).collect()], frames: vec![] };
        w.expr(&b.body);
        effects(p, di, &b.body, &mut out);
    }
    out
}

/// 関数・クロージャの値の効果が、渡す先の宣言を超えるときの E4001 / E4002 (仕様 4.3・5.1)。見るのは TAST の型の効果だけ:
/// (1) 効果を宣言した関数型 `fn(..) uses [..] panics ..` の位置へ入る値 (変換の印 `FnTo`)
/// (2) 呼び先の型パラメータの `Fn<.., E, P>` 境界が具体的な効果を持つとき、その型の実引数の閉包・関数。
/// 効果変数を持つ側は、呼び出し元の宣言との比べ (呼び出し先の `uses [E]` の実体化) が見るのでここでは見ない。
fn effects(p: &Program, di: DefId, body: &TExpr, out: &mut Vec<Diag>) {
    use crate::check::{covers, is_effect_var};
    use crate::types::Ty;
    let d = &p.defs[di];
    let mut found: Vec<(&'static str, String, usize)> = vec![];
    // 呼び出しの引数へ入る値は呼び出しの行、それ以外 (`let` の注釈・戻り値・欄) は旧い検査と同じく定義の行で報告する。
    let mut arg_line: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    super::impl_sig::walk(body, &mut |e| {
        if let TKind::Call { args, .. } | TKind::MCall { args, .. } = &e.kind {
            for a in args {
                arg_line.insert(a.id, e.line);
            }
        }
        if let TKind::StructLit { fields, .. } = &e.kind {
            for (_, a) in fields {
                arg_line.insert(a.id, e.line);
            }
        }
        let at = arg_line.get(&e.id).copied().unwrap_or(0);
        for c in &e.conv {
            let Coercion::FnTo(Ty::Closure { eff: crate::types::Eff(Some(want)), .. }) = c else { continue };
            let Ty::Closure { eff: crate::types::Eff(Some(got)), .. } = e.ty.strip() else { continue };
            if want.uses.iter().any(|x| is_effect_var(x)) || want.may.is_none() {
                continue;
            }
            for x in &got.uses {
                if is_effect_var(x) {
                    let who = if got.name.is_empty() { "the passed function type" } else { &got.name };
                    found.push(("E4001", format!("{} uses effect variable `{}` but the function type declares `uses [{}]`", who, x, want.uses.join(", ")), at));
                } else if !covers(&want.uses, x) {
                    found.push(("E4001", format!("{} uses effect `{}` but the function type declares `uses [{}]`", got.name, x, want.uses.join(", ")), at));
                }
            }
            if want.may == Some(false) && got.may != Some(false) {
                let who = if got.name.is_empty() { "the passed function type" } else { &got.name };
                found.push(("E4002", format!("{} may panic but the function type declares `panics never`", who), at));
            }
        }
        let (args, off) = match (&e.kind, &e.res) {
            (TKind::Call { args, .. }, Some(Callee::Def(ci))) | (TKind::MCall { args, .. }, Some(Callee::Def(ci))) => {
                let n = p.defs[*ci].params.len();
                (args, if n > args.len() { n - args.len() } else { 0 })
            }
            _ => return,
        };
        let Some(Callee::Def(ci)) = &e.res else { return };
        let c = &p.defs[*ci];
        let gn = c.gnames();
        for (k, a) in args.iter().enumerate() {
            let Some(prm) = c.params.get(k + off) else { break };
            let mut slots = vec![];
            param_slots(&crate::types::from_v(&prm.1, &gn), &a.ty, &mut slots);
            for (name, at) in slots {
                let Ty::Closure { eff: crate::types::Eff(Some(info)), .. } = &at else { continue };
                for g in &c.generics {
                    if g.0 != "type" || g.1 != name {
                        continue;
                    }
                    for b in crate::check::items(&g.2) {
                        if !matches!(b, crate::parser::V::L(bx) if crate::check::st(&bx[0]).starts_with("core.ops.Fn")) {
                            continue;
                        }
                        let bt = crate::check::items(crate::check::tel(b, 1));
                        if let Some(v) = bt.get(2).filter(|v| crate::check::tag(v) == "effects") {
                            let bl: Vec<String> = crate::check::items(crate::check::tel(v, 1)).iter().map(crate::check::st).collect();
                            if bl.iter().all(|x| x.chars().next().map_or(false, |ch| ch.is_lowercase())) {
                                for x in info.uses.iter().filter(|x| is_effect_var(x) || !covers(&bl, x)) {
                                    found.push((
                                        "E4001",
                                        format!("closure passed to `{}` uses effect `{}` outside the `Fn` bound's effects [{}]", c.name, x, bl.join(", ")),
                                        e.line,
                                    ));
                                }
                            }
                        }
                        if let Some(v) = bt.get(3).filter(|v| crate::check::tag(v) == "panic") {
                            if crate::check::st(crate::check::tel(v, 1)) == "never" && info.may.unwrap_or(false) {
                                found.push(("E4002", format!("closure passed to `{}` may panic but the `Fn` bound declares `panics never`", c.name), e.line));
                            }
                        }
                    }
                }
            }
        }
    });
    for (code, msg, line) in found {
        let line = if line != 0 { line } else { crate::parser::unpack_pos(d.line).0 };
        let dg = Diag { file: d.file.clone(), code: code.into(), fqn: d.name.clone(), line, col: 0, msg };
        if !out.iter().any(|o| o.file == dg.file && o.line == dg.line && o.code == dg.code && o.msg == dg.msg) {
            out.push(dg);
        }
    }
}

/// 仮引数の型 `pt` の中の型パラメータと、実引数の型 `a` の同じ位置の型の組 (参照は外して見る)。
fn param_slots(pt: &crate::types::Ty, a: &crate::types::Ty, out: &mut Vec<(String, crate::types::Ty)>) {
    use crate::types::Ty;
    let mut at = a.clone();
    while let Ty::Ref(_, x, _) = at {
        at = *x;
    }
    let mut pt = pt;
    while let Ty::Ref(_, x, _) = pt {
        pt = x;
    }
    match (pt, &at) {
        (Ty::Param(n), _) => out.push((n.clone(), at)),
        (Ty::Named(n, pa, _), Ty::Named(m, aa, _)) if n == m && pa.len() == aa.len() => pa.iter().zip(aa).for_each(|(x, y)| param_slots(x, y, out)),
        (Ty::Tuple(pa), Ty::Tuple(aa)) if pa.len() == aa.len() => pa.iter().zip(aa).for_each(|(x, y)| param_slots(x, y, out)),
        (Ty::Array(x, _), Ty::Array(y, _)) | (Ty::Slice(x), Ty::Slice(y)) => param_slots(x, y, out),
        _ => {}
    }
}

struct W<'a> {
    p: &'a Program,
    di: DefId,
    out: &'a mut Vec<Diag>,
    /// 名前の見え方 (内側が末尾)。
    scopes: Vec<HashSet<String>>,
    /// 今いる閉包ごとの (見える名前の始まる `scopes` の添字, 塞がれた外側の名前)。
    frames: Vec<(usize, HashSet<String>)>,
}

fn pat_names(q: &TPat, out: &mut HashSet<String>) {
    if q.kind == "PBind" {
        if let Some(n) = &q.name {
            out.insert(n.clone());
        }
    }
    for c in &q.children {
        pat_names(c, out);
    }
}

impl<'a> W<'a> {
    fn diag(&mut self, code: &str, msg: String, line: usize) {
        let d = &self.p.defs[self.di];
        let line = if line != 0 { line } else { crate::parser::unpack_pos(d.line).0 };
        let dg = Diag { file: d.file.clone(), code: code.into(), fqn: d.name.clone(), line, col: 0, msg };
        if !self.out.iter().any(|o| o.file == dg.file && o.line == dg.line && o.code == dg.code && o.msg == dg.msg) {
            self.out.push(dg);
        }
    }

    fn base(&self) -> usize {
        self.frames.last().map_or(0, |f| f.0)
    }

    /// 今の閉包 (なければ関数) から見える局所か。
    fn visible(&self, n: &str) -> bool {
        self.scopes[self.base()..].iter().any(|s| s.contains(n))
    }

    /// 見えない名前が、外側の閉包で塞がれた名前 (今の閉包では塞ぎの対象でもない) で、定義にも解けない。
    /// 旧い検査はこれを名前の解決の失敗 (`unresolved name y`) として報告する。
    fn outer_blocked(&self, segs: &[String]) -> bool {
        let n = &segs[0];
        let nf = self.frames.len();
        nf >= 2 && !self.visible(n) && !self.frames[nf - 1].1.contains(n) && self.frames[..nf - 1].iter().any(|f| f.1.contains(n)) && self.p.resolve(segs, &self.p.defs[self.di].file).is_none()
    }

    fn bind(&mut self, q: &TPat) {
        let mut ns = HashSet::new();
        pat_names(q, &mut ns);
        if let Some(s) = self.scopes.last_mut() {
            s.extend(ns);
        }
    }

    fn expr(&mut self, e: &TExpr) {
        match &e.kind {
            TKind::Lit(_) => {}
            TKind::Path(segs) => {
                let n = &segs[0];
                if !self.visible(n) && self.frames.last().is_some_and(|f| f.1.contains(n)) {
                    self.diag(
                        "E1101",
                        format!("unresolved name {}: the closure body uses an outer binding that is not in its `capture` list (write `capture [&{0}]` or `capture [move {0}]`)", n),
                        e.line,
                    );
                } else if self.outer_blocked(segs) {
                    self.diag("E1101", format!("unresolved name {}", segs.join(".")), e.line);
                }
            }
            TKind::Call { callee, args } => {
                for a in args {
                    self.expr(a);
                }
                // 呼び先の経路 (`f(x)`・`v.push(x)`) は `Path` の漏れの文面ではなく、名前の解決 (`unresolved name f.g`) として拒まれる
                // (旧い検査の `resolve_path`)。外側の局所を塞がれた名前で、定義にも解けないときだけここで出す
                // (旧い検査が付ける「近い名前」の案内 `; did you mean ..?` は付けない。コードと行は同じ)。
                match &callee.kind {
                    TKind::Path(segs) => {
                        if self.outer_blocked(segs) || (!self.visible(&segs[0]) && self.frames.last().is_some_and(|f| f.1.contains(&segs[0])) && self.p.resolve(segs, &self.p.defs[self.di].file).is_none()) {
                            self.diag("E1101", format!("unresolved name {}", segs.join(".")), callee.line);
                        }
                    }
                    _ => self.expr(callee),
                }
            }
            TKind::MCall { recv, args, .. } => {
                self.expr(recv);
                for a in args {
                    self.expr(a);
                }
            }
            TKind::Field { base, .. } => self.expr(base),
            TKind::Index { base, index } => {
                self.expr(base);
                self.expr(index);
            }
            TKind::Unary { operand, .. } => self.expr(operand),
            TKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            TKind::StructLit { fields, .. } => {
                for (_, x) in fields {
                    self.expr(x);
                }
            }
            TKind::Block(stmts) => {
                self.scopes.push(HashSet::new());
                for s in stmts {
                    match s {
                        TStmt::Let { pat, init } => {
                            if let Some(i) = init {
                                self.expr(i);
                            }
                            self.bind(pat);
                        }
                        TStmt::Expr(x) => self.expr(x),
                        TStmt::Assign { place, value } => {
                            self.expr(place);
                            self.expr(value);
                        }
                    }
                }
                self.scopes.pop();
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
                    self.scopes.push(HashSet::new());
                    self.bind(&a.pat);
                    if let Some(g) = &a.guard {
                        self.expr(g);
                    }
                    self.expr(&a.body);
                    self.scopes.pop();
                }
            }
            TKind::Closure { params, body, written, .. } => {
                for (_, n) in written {
                    if !self.visible(n) {
                        self.diag("E1101", format!("unresolved name {} in the capture list", n), e.line);
                    }
                }
                let mut inner: HashSet<String> = params.iter().map(|q| q.name.clone()).collect();
                inner.extend(written.iter().map(|(_, n)| n.clone()));
                let b = self.base();
                let blocked: HashSet<String> = self.scopes[b..].iter().flatten().filter(|n| !inner.contains(*n)).cloned().collect();
                self.frames.push((self.scopes.len(), blocked));
                self.scopes.push(inner);
                self.expr(body);
                self.scopes.pop();
                self.frames.pop();
            }
            TKind::Other { children, .. } => {
                if e.pats.is_empty() {
                    for c in children {
                        self.expr(c);
                    }
                } else {
                    // `for` / `with`: 先頭の子 (繰り返す値・初期化式) は束縛の外、残りは束縛の中。
                    if let Some(c) = children.first() {
                        self.expr(c);
                    }
                    self.scopes.push(HashSet::new());
                    for q in &e.pats {
                        self.bind(q);
                    }
                    for c in children.iter().skip(1) {
                        self.expr(c);
                    }
                    self.scopes.pop();
                }
            }
        }
    }
}
