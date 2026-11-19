//! 規則 F: 関数型・`dyn` の効果の食い違い (仕様 5.1、E4001 / E4002)。
//!
//! 値が入る先の型 (受け側) の中の関数型 `fn(..) uses [..] panics ..` と、`dyn Tr<[..], P>` の効果・panic の実引数が、
//! 渡す側の型のものを含んでいるかを、型の木を辿って比べる (入れ子の `Option<fn>`・`fn` の引数 (反変)・`&mut` の下 (不変) も)。
//! 比べる場所は、呼び出しの実引数・注釈つきの `let`・関数の戻り値。`F: Fn<.., E, P>` の型パラメータを、より狭い境界の
//! 型パラメータへ渡す所も同じ規則。型だけを見る (`Ty::Closure` の `eff`・`Ty::Dyn` の実引数・`impl` の実引数)。
//! 旧い検査 (`types.rs` の `check_fn_conv_dir`・`check_fn_eff`・`check_dyn_conv`・`check_bound_effect_args`) と同じコード・行・文面。

use std::collections::HashSet;

use crate::check::{covers, is_effect_var, items, st, targ_effs, targ_panic, tel, Diag};
use crate::parser::V;
use crate::program::{Def, Program};
use crate::tast::*;
use crate::types::{from_v, Ty};

struct Ck<'a> {
    p: &'a Program,
    d: &'a Def,
    found: Vec<(&'static str, String, usize)>,
    seen: HashSet<(String, String, usize)>,
}

/// 受け側 `want` に渡し側 `got` の効果が含まれていなければ E4001 / E4002 (旧い検査の `check_fn_eff`)。
fn fn_eff(want_uses: &[String], want_may: Option<bool>, got_uses: &[String], got_may: Option<bool>, got_name: &str) -> Vec<(&'static str, String)> {
    let mut out = vec![];
    if want_uses.iter().any(|e| e.chars().next().map_or(false, |c| c.is_uppercase())) || want_may.is_none() {
        return out;
    }
    for e in got_uses {
        match e.chars().next() {
            Some(c) if c.is_lowercase() => {
                if !covers(want_uses, e) {
                    out.push(("E4001", format!("{} uses effect `{}` but the function type declares `uses [{}]`", got_name, e, want_uses.join(", "))));
                }
            }
            Some(c) if c.is_uppercase() => {
                out.push((
                    "E4001",
                    format!(
                        "{} uses effect variable `{}` but the function type declares `uses [{}]`",
                        if got_name.is_empty() { "the passed function type" } else { got_name },
                        e,
                        want_uses.join(", ")
                    ),
                ));
            }
            _ => {}
        }
    }
    if want_may == Some(false) && got_may != Some(false) {
        let g = if got_name.is_empty() { "the passed function type" } else { got_name };
        out.push(("E4002", format!("{} may panic but the function type declares `panics never`", g)));
    }
    out
}

impl<'a> Ck<'a> {
    fn push(&mut self, v: Vec<(&'static str, String)>, line: usize) {
        for (c, m) in v {
            if self.seen.insert((c.to_string(), m.clone(), line)) {
                self.found.push((c, m, line));
            }
        }
    }

    fn impl_targs_of(&self, tr: &str, ty: &str) -> Option<Vec<V>> {
        self.p.defs.iter().find_map(|x| {
            if x.kw != "impl" || x.impl_trait != tr {
                return None;
            }
            let tgt = match &x.impl_for {
                V::L(xs) if matches!(xs.first(), Some(V::S(k)) if k == "named") => st(&xs[1]),
                _ => return None,
            };
            (tgt == ty).then(|| items(&x.impl_targs).to_vec())
        })
    }

    fn dyn_conv(&mut self, tr: &str, want: &[V], got: &[V], line: usize) {
        let Some(root) = crate::check::resolve_trait_fqn(self.p, tr) else { return };
        let Some(&ti) = self.p.types.get(&root) else { return };
        let gens = self.p.defs[ti].generics.clone();
        for (k, g) in gens.iter().enumerate() {
            let (Some(w), Some(x)) = (want.get(k), got.get(k)) else { continue };
            match g.0.as_str() {
                "uses" => {
                    let (Some(wu), Some(gu)) = (targ_effs(w), targ_effs(x)) else { continue };
                    let v = fn_eff(&wu, Some(true), &gu, Some(true), "");
                    self.push(v, line);
                }
                "panics" => {
                    let (Some(wp), Some(gp)) = (targ_panic(w), targ_panic(x)) else { continue };
                    let to_may = |p: &str| match p {
                        "never" => Some(false),
                        "may" => Some(true),
                        _ => None,
                    };
                    let v = fn_eff(&[], to_may(&wp), &[], to_may(&gp), "");
                    self.push(v, line);
                }
                _ => {}
            }
        }
    }

    /// 旧い検査の `check_fn_conv_dir` (向きつき)。
    fn walk(&mut self, want: &Ty, got: &Ty, line: usize) {
        let (mut w, mut g) = (want.clone(), got.clone());
        let mut inv = false;
        loop {
            match (&w, &g) {
                (Ty::Ref(true, a, _), Ty::Ref(true, b, _)) => {
                    inv = true;
                    (w, g) = ((**a).clone(), (**b).clone());
                }
                (Ty::Ref(_, a, _), _) => w = (**a).clone(),
                (_, Ty::Ref(_, b, _)) => g = (**b).clone(),
                _ => break,
            }
        }
        if inv {
            self.one(&w, &g, line);
            self.one(&g, &w, line);
            return;
        }
        self.one(&w, &g, line);
    }

    fn one(&mut self, want: &Ty, got: &Ty, line: usize) {
        match (want, got) {
            (Ty::Closure { eff: ew, args: wa, ret: wr, .. }, Ty::Closure { eff: eg, args: ga, ret: gr, .. }) => {
                if let (Some(ew), Some(eg)) = (&ew.0, &eg.0) {
                    let v = fn_eff(&ew.uses, ew.may, &eg.uses, eg.may, &eg.name);
                    self.push(v, line);
                }
                self.walk(wr, gr, line);
                for (w, g) in wa.iter().zip(ga.iter()) {
                    self.walk(g, w, line);
                }
            }
            (Ty::Named(_, ws, _), Ty::Named(_, gs, _)) => {
                for (w, g) in ws.iter().zip(gs.iter()) {
                    self.walk(w, g, line);
                }
            }
            (Ty::Tuple(ws), Ty::Tuple(gs)) => {
                for (w, g) in ws.iter().zip(gs.iter()) {
                    self.walk(w, g, line);
                }
            }
            (Ty::Array(w, _), Ty::Array(g, _)) | (Ty::Slice(w), Ty::Slice(g)) | (Ty::Ptr(_, w), Ty::Ptr(_, g)) => self.walk(w, g, line),
            (Ty::Dyn(wt, wa, _), Ty::Dyn(gt, ga, _)) if wt == gt => self.dyn_conv(wt, &wa.0, &ga.0, line),
            (Ty::Dyn(wt, wa, _), Ty::Named(n, _, _)) => {
                let Some(root) = crate::check::resolve_trait_fqn(self.p, wt) else { return };
                if let Some(got) = self.impl_targs_of(&root, n) {
                    self.dyn_conv(wt, &wa.0, &got, line);
                }
            }
            _ => {}
        }
    }

    /// `F: Fn<.., E, P>` の境界の効果実引数 (旧い検査の `fn_bound_effects`)。
    fn bound_effects(d: &Def, name: &str) -> Option<(Vec<String>, Option<bool>)> {
        for g in d.generics.iter().filter(|g| g.0 == "type" && g.1 == name) {
            for b in items(&g.2) {
                let V::L(bx) = b else { continue };
                if !st(&bx[0]).starts_with("core.ops.Fn") {
                    continue;
                }
                let targs = items(tel(b, 1));
                let uses = match targs.get(2) {
                    Some(v) if crate::check::tag(v) == "effects" => items(tel(v, 1)).iter().map(st).collect(),
                    Some(v) if crate::check::tag(v) == "named" => {
                        let nm = st(tel(v, 1));
                        if d.generics.iter().any(|g| g.0 == "uses" && g.1 == nm) {
                            vec![nm]
                        } else {
                            continue;
                        }
                    }
                    _ => continue,
                };
                let may = match targs.get(3) {
                    Some(v) if crate::check::tag(v) == "panic" => match st(tel(v, 1)).as_str() {
                        "never" => Some(false),
                        "may" => Some(true),
                        _ => None,
                    },
                    Some(v) if crate::check::tag(v) == "named" => {
                        let nm = st(tel(v, 1));
                        if d.generics.iter().any(|g| g.0 == "panics" && g.1 == nm) {
                            None
                        } else {
                            continue;
                        }
                    }
                    _ => continue,
                };
                return Some((uses, may));
            }
        }
        None
    }

    /// 境界 `T: Tr<.., [E..], P>` の効果の実引数を、`T` に当たる型の `impl Tr<..>` (型パラメータなら呼び出し元の同じトレイトの境界) と比べる。
    fn bound_args(&mut self, tr: &str, want: &[V], t: &Ty, line: usize) {
        let Some(&ti) = self.p.types.get(tr) else { return };
        let td = &self.p.defs[ti];
        if td.kw != "trait" || !td.generics.iter().any(|g| g.0 == "uses" || g.0 == "panics") {
            return;
        }
        let got: Vec<V> = match t {
            Ty::Named(n, _, _) => match self.impl_targs_of(tr, n) {
                Some(a) => a,
                None => return,
            },
            Ty::Param(pn) => {
                let found = self.d.generics.iter().filter(|g| g.0 == "type" && &g.1 == pn).flat_map(|g| items(&g.2).to_vec()).find_map(|b| match &b {
                    V::L(bx) if st(&bx[0]) == tr => Some(items(tel(&b, 1)).to_vec()),
                    _ => None,
                });
                match found {
                    Some(a) => a,
                    None => return,
                }
            }
            _ => return,
        };
        let mut out: Vec<(&'static str, String)> = vec![];
        for (k, g) in td.generics.iter().enumerate() {
            match g.0.as_str() {
                "uses" => {
                    let (Some(w), Some(h)) = (want.get(k).and_then(targ_effs), got.get(k).and_then(targ_effs)) else { continue };
                    if w.iter().chain(h.iter()).any(|e| is_effect_var(e)) {
                        continue;
                    }
                    for e in h.iter().filter(|e| !covers(&w, e)) {
                        out.push(("E4001", format!("the impl of `{}` for `{}` uses effect `{}` but the bound declares `uses [{}]`", tr, t.show(), e, w.join(", "))));
                    }
                }
                "panics" => {
                    let (Some(w), Some(h)) = (want.get(k).and_then(targ_panic), got.get(k).and_then(targ_panic)) else { continue };
                    if w == "never" && h == "may" {
                        out.push(("E4002", format!("the impl of `{}` for `{}` may panic but the bound declares `panics never`", tr, t.show())));
                    }
                }
                _ => {}
            }
        }
        self.push(out, line);
    }

    fn call(&mut self, c: &Def, off_recv: Option<&Ty>, args: &[TExpr], targs: &[Ty], line: usize) {
        let gn = c.gnames();
        let mut argt: Vec<Ty> = vec![];
        if let Some(r) = off_recv {
            argt.push(r.clone());
        }
        argt.extend(args.iter().map(|a| a.ty.clone()));
        if c.kw != "extern" {
            for (k, a) in argt.iter().enumerate() {
                let Some(prm) = c.params.get(k) else { break };
                let raw = from_v(&prm.1, &gn);
                self.walk(&raw, a, line);
                // `F: Fn<..>` の型パラメータへ、より広い (効果変数・panic 変数を持つ) 境界の型パラメータを渡す。
                let mut wslots = vec![];
                collect_params(&raw, &mut wslots);
                for wname in wslots {
                    let Some((wu, wp)) = Self::bound_effects(c, &wname) else { continue };
                    let got = strip(a);
                    let Ty::Param(gname) = got else { continue };
                    let Some((gu, gp)) = Self::bound_effects(self.d, &gname) else { continue };
                    let gname_msg = format!("type parameter `{}` of `{}`", gname, self.d.name);
                    let v = fn_eff(&wu, wp, &gu, gp, &gname_msg);
                    self.push(v, line);
                }
            }
        }
        // 型パラメータの境界の効果の実引数 (`H: Tr<[], never>`) と実引数の型の `impl` を比べる。
        let tps: Vec<&(String, String, V)> = c.generics.iter().filter(|g| g.0 == "type").collect();
        for (g, t) in tps.iter().zip(targs.iter()) {
            for b in items(&g.2) {
                let V::L(bx) = b else { continue };
                let tr = st(&bx[0]);
                let tr = crate::check::resolve_trait_fqn(self.p, &tr).unwrap_or(tr);
                let want = items(tel(b, 1)).to_vec();
                self.bound_args(&tr, &want, &strip(t), line);
            }
        }
    }
}

fn strip(t: &Ty) -> Ty {
    let mut t = t.strip().clone();
    while let Ty::Ref(_, x, _) = t {
        t = x.strip().clone();
    }
    t
}

/// 仮引数の型の中の型パラメータの名前 (参照・組・型引数を辿る。`F` と `&F` の `F`)。
fn collect_params(t: &Ty, out: &mut Vec<String>) {
    match t {
        Ty::Param(n) => out.push(n.clone()),
        Ty::Ref(_, x, _) => collect_params(x, out),
        _ => {}
    }
}

pub fn check(p: &Program, user: &[usize], tast: &Tast) -> Vec<Diag> {
    let mut out = vec![];
    for &di in user {
        let d = &p.defs[di];
        if d.kw != "fn" || d.inferred {
            continue;
        }
        let Some(b) = tast.bodies.get(&di) else { continue };
        let mut ck = Ck { p, d, found: vec![], seen: HashSet::new() };
        // 戻り値・注釈つきの `let` は旧い検査と同じく定義の行 (0) で報告する。
        ck.walk(&b.ret, &b.body.ty, 0);
        super::impl_sig::walk(&b.body, &mut |e| {
            match &e.kind {
                TKind::Block(ss) => {
                    for st in ss {
                        if let TStmt::Let { pat, init: Some(init) } = st {
                            if pat.kind == "PBind" && pat.lit.iter().any(|l| l == "ann") {
                                if let Some((_, ty, _)) = pat.info.as_ref().and_then(|i| i.bind.as_ref()) {
                                    ck.walk(ty, &init.ty, 0);
                                }
                            }
                        }
                    }
                }
                TKind::Other { name: "Return", children } => {
                    if let Some(c) = children.first() {
                        ck.walk(&b.ret, &c.ty, 0);
                    }
                }
                TKind::Call { args, .. } | TKind::MCall { args, .. } => {
                    let Some(Callee::Def(ci)) = &e.res else { return };
                    let c = &p.defs[*ci];
                    let recv: Option<Ty> = match &e.kind {
                        TKind::MCall { recv, .. } => Some(recv.ty.clone()),
                        TKind::Call { callee, .. } if c.params.len() > args.len() && matches!(&callee.kind, TKind::Path(s) if s.len() >= 2) => Some(callee.ty.clone()),
                        _ => None,
                    };
                    ck.call(c, recv.as_ref(), args, &e.targs, e.line);
                }
                _ => {}
            }
        });
        for (code, msg, line) in ck.found {
            let line = if line != 0 { line } else { crate::parser::unpack_pos(d.line).0 };
            let dg = Diag { file: d.file.clone(), code: code.into(), fqn: d.name.clone(), line, col: 0, msg };
            if !out.iter().any(|o: &Diag| o.file == dg.file && o.line == dg.line && o.code == dg.code && o.msg == dg.msg) {
                out.push(dg);
            }
        }
    }
    out
}
