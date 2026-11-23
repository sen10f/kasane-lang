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

    /// `Box.into_dyn` の変換: 元の型の `impl Tr<..>` の効果を、変換先の `dyn Tr<..>` の実引数と比べる (型の実体は `[T, D, A]`)。
    fn into_dyn(&mut self, targs: &[Ty], line: usize) {
        if let (Some(t), Some(dd)) = (targs.first(), targs.get(1)) {
            if let (Ty::Named(n, _, _), Ty::Dyn(tr, da, _)) = (t.strip(), dd.strip()) {
                if let Some(root) = crate::check::resolve_trait_fqn(self.p, tr) {
                    if let Some(got) = self.impl_targs_of(&root, n) {
                        self.dyn_conv(tr, &da.0, &got, line);
                    }
                }
            }
        }
    }

    fn call(&mut self, c: &Def, off_recv: Option<&Ty>, args: &[TExpr], targs: &[Ty], line: usize) {
        let gn = c.gnames();
        let mut argt: Vec<Ty> = vec![];
        if let Some(r) = off_recv {
            argt.push(r.clone());
        }
        argt.extend(args.iter().map(|a| a.ty.clone()));
        if c.kw != "extern" {
            let mut bound: std::collections::HashMap<String, Ty> = std::collections::HashMap::new();
            for (k, a) in argt.iter().enumerate() {
                let Some(prm) = c.params.get(k) else { break };
                let raw = from_v(&prm.1, &gn);
                self.walk(&raw, a, line);
                // 型パラメータは、先の引数 (受け手を含む) が決めた型で受ける: `v.push(f)` の `T` は `v` の要素の型、
                // `mem.replace(&mut g, f)` の `T` は `g` の型。関数型の効果は型にあり、初期値のものではない。
                if bound.is_empty() {
                    for (j, aj) in argt.iter().enumerate() {
                        let Some(pj) = c.params.get(j) else { break };
                        let mut slots = vec![];
                        super::captures::param_slots(&from_v(&pj.1, &gn), aj, &mut slots);
                        for (n, t) in slots {
                            bound.entry(n).or_insert(t);
                        }
                    }
                }
                if !bound.is_empty() {
                    let inst = crate::types::subst(&raw, &bound);
                    self.walk(&inst, a, line);
                }
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
        // 局所の名前 → 束縛の型 (`let` と引数。同じ名前が複数あれば全部と比べる)。
        let mut locals: std::collections::HashMap<String, Vec<Ty>> = std::collections::HashMap::new();
        for q in &b.params {
            locals.entry(q.name.clone()).or_default().push(q.ty.clone());
        }
        super::impl_sig::walk(&b.body, &mut |e| match &e.kind {
            TKind::Block(ss) => {
                for st in ss {
                    if let TStmt::Let { pat, .. } = st {
                        fn pb(q: &TPat, m: &mut std::collections::HashMap<String, Vec<Ty>>) {
                            if let Some((n, t, _)) = q.info.as_ref().and_then(|i| i.bind.as_ref()) {
                                m.entry(n.clone()).or_default().push(t.clone());
                            }
                            q.children.iter().for_each(|c| pb(c, m));
                        }
                        pb(pat, &mut locals);
                    }
                }
            }
            TKind::Closure { params, .. } => {
                for q in params {
                    locals.entry(q.name.clone()).or_default().push(q.ty.clone());
                }
            }
            _ => {}
        });
        // 閉包ごとの宣言した戻り値の型と、その閉包に属する `return` (一番内側の閉包が持つ)。
        let mut closures: Vec<(Ty, Vec<*const TExpr>, usize)> = vec![];
        super::impl_sig::walk(&b.body, &mut |e| {
            if let (TKind::Closure { body, .. }, Ty::Closure { ret, .. }) = (&e.kind, e.ty.strip()) {
                let mut rs = vec![];
                let mut n = 0usize;
                super::impl_sig::walk(body, &mut |x| {
                    n += 1;
                    if matches!(&x.kind, TKind::Other { name: "Return", .. }) {
                        rs.push(x as *const TExpr);
                    }
                });
                // 閉包の本体の末尾の値も、宣言した戻り値の型へ入る。
                ck.walk(ret, &body.ty, e.line);
                closures.push(((**ret).clone(), rs, n));
            }
        });
        super::impl_sig::walk(&b.body, &mut |e| {
            for c in &e.conv {
                if let crate::tast::Coercion::Unsize { from, to } = c {
                    ck.walk(to, from, e.line);
                }
            }
            let is_into_dyn = match &e.kind {
                TKind::Call { callee, .. } => match &callee.kind {
                    TKind::Path(s) => s.len() >= 2 && s.last().map_or(false, |x| x == "into_dyn"),
                    _ => false,
                },
                TKind::MCall { name, .. } => name == "into_dyn",
                _ => false,
            };
            let is_into_dyn = is_into_dyn && match &e.res {
                Some(Callee::Def(ci)) => p.defs[*ci].name == "alloc.boxed.Box.into_dyn",
                _ => true,
            };
            if is_into_dyn {
                ck.into_dyn(&e.targs, e.line);
            }
            match &e.kind {
                TKind::Block(ss) => {
                    for st in ss {
                        if let TStmt::Assign { place, value } = st {
                            ck.walk(&place.ty, &value.ty, e.line);
                        }
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
                        let me = e as *const TExpr;
                        let inner = closures.iter().filter(|(_, rs, _)| rs.contains(&me)).min_by_key(|(_, _, n)| *n);
                        match inner {
                            Some((r, _, _)) => ck.walk(r, &c.ty, e.line),
                            None => ck.walk(&b.ret, &c.ty, 0),
                        }
                    }
                }
                TKind::Call { callee, args } if matches!(e.res, Some(Callee::Local(_))) => {
                    // 関数値・閉包の呼び出し: 実引数は、その型の仮引数の型 (関数型なら効果つき) に入る。
                    let Some(Callee::Local(n)) = &e.res else { return };
                    let _ = callee;
                    for lt in locals.get(n).into_iter().flatten() {
                        if let Ty::Closure { args: pa, .. } = strip(lt) {
                            for (w, a) in pa.iter().zip(args.iter()) {
                                ck.walk(w, &a.ty, e.line);
                            }
                        }
                    }
                }
                TKind::Call { args, .. } | TKind::MCall { args, .. } => {
                    let cis: Vec<usize> = match &e.res {
                        Some(Callee::Def(ci)) => vec![*ci],
                        Some(Callee::Methods(v)) => {
                            let mut o: Vec<usize> = vec![];
                            for &ci in v {
                                if !o.iter().any(|&x| p.defs[x].name == p.defs[ci].name && p.defs[x].params.len() == p.defs[ci].params.len()) {
                                    o.push(ci);
                                }
                            }
                            o
                        }
                        _ => return,
                    };
                    for ci in cis {
                    let c = &p.defs[ci];
                    let recv: Option<Ty> = match &e.kind {
                        TKind::MCall { recv, .. } => Some(recv.ty.clone()),
                        TKind::Call { callee, .. } if c.params.len() > args.len() && matches!(&callee.kind, TKind::Path(s) if s.len() >= 2) => Some(callee.ty.clone()),
                        _ => None,
                    };
                    ck.call(c, recv.as_ref(), args, &e.targs, e.line);
                }
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
