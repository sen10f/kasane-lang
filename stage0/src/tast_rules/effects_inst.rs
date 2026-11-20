//! 規則 E: 効果変数・panic 変数の実体化 (仕様 4.3・5.1、設計_型付き検査.md の S4 の条件の「効果変数の実体化」)。
//!
//! 呼び出し先の宣言 `uses [E] panics P` の変数を、次のものから束ねて呼び出し元の宣言と比べる (E4001 / E4002)。
//! - 実引数の関数・クロージャの型の効果 (`F: Fn<.., E, P>` 境界、`fn(..) uses [E] panics P` の仮引数) (`Ty::Closure` の `eff`)
//! - `I: Iterator<T, E, P>` 境界の実引数の型の `Iterator` の impl の効果 (具体的なものだけ)
//! - `dyn Tr<E, P>` 越しのメソッド: トレイトの宣言を `dyn` の実引数で実体化する (スーパートレイトも辿る)
//! - `for x in it` の暗黙の `next` と、`Iterator` 境界の実引数 (アダプタの入れ子も) の `next` の効果
//! - 束縛されない panic 変数 `P` (呼び出し元が同じ `P` を持たなければ E4002)
//!
//! 見るのは TAST の解決先 (`res`)・実引数と受け手の型・`dyn` の実引数だけ。式の形は見ない。クロージャの本体の中の節点は、
//! クロージャの効果として数えるのでここでは見ない。旧い検査 (`types.rs` の `callee_effects`・`for_next_effects`・`dyn_method`、
//! `check.rs` の `violation_effs`) と同じコード・同じ行・同じ文面で出す。

use std::collections::{HashMap, HashSet};

use crate::check::{covers, is_effect_var, is_panic_var, items, st, tag, targ_effs, targ_panic, tel, Diag};
use crate::parser::V;
use crate::program::{Def, Program};
use crate::tast::*;
use crate::types::{from_v, Ty};

/// 呼び出し元の宣言 (`uses` と `panics`。省略は `may`)。
struct Caller<'a> {
    d: &'a Def,
    uses: Vec<String>,
    panics: String,
}

/// 呼び出し元の宣言が callee の実体化した効果を覆うか (旧い検査の `violation_effs`)。
fn violation(c: &Caller, name: &str, uses: &[String], panics: Option<&str>) -> Option<(&'static str, String)> {
    for eff in uses {
        let covered = if is_effect_var(eff) { c.uses.iter().any(|x| x == eff) } else { covers(&c.uses, eff) };
        if !covered {
            return Some(("E4001", format!("{} uses effect `{}` not declared by the caller", name, eff)));
        }
    }
    match panics {
        Some(pv) if is_panic_var(pv) && c.panics != pv => {
            Some(("E4002", format!("{} may panic (panic variable `{}`) but the caller does not declare `panics {}`", name, pv, pv)))
        }
        Some("may") if c.panics == "never" => Some(("E4002", format!("{} may panic but the caller declares `panics never`", name))),
        _ => None,
    }
}

#[derive(Default)]
struct Bind {
    uses: HashMap<String, Vec<String>>,
    panics: HashMap<String, bool>,
}

struct Ck<'a> {
    p: &'a Program,
    c: Caller<'a>,
    found: Vec<(&'static str, String, usize)>,
}

fn strip_refs(t: &Ty) -> Ty {
    let mut t = t.strip().clone();
    while let Ty::Ref(_, x, _) = t {
        t = x.strip().clone();
    }
    t
}

/// 参照と `Box` を外した先 (`&Box<dyn Tr>` の `dyn Tr`)。
fn dyn_of(t: &Ty) -> Ty {
    let mut t = strip_refs(t);
    while let Ty::Named(n, a, _) = &t {
        if n != "alloc.boxed.Box" || a.is_empty() {
            break;
        }
        t = strip_refs(&a[0]);
    }
    t
}

fn fn_effects(at: &Ty) -> Option<(Vec<String>, Option<bool>)> {
    match at {
        Ty::Closure { eff: crate::types::Eff(Some(info)), .. } => Some((info.uses.clone(), info.may)),
        _ => None,
    }
}

fn fn_bound<'a>(d: &'a Def, name: &str) -> Vec<&'a V> {
    let mut out = vec![];
    for g in d.generics.iter().filter(|g| g.0 == "type" && g.1 == name) {
        for b in items(&g.2) {
            if matches!(b, V::L(bx) if st(&bx[0]).starts_with("core.ops.Fn")) {
                out.push(b);
            }
        }
    }
    out
}

fn bound_effects(d: &Def, name: &str) -> Option<(Vec<String>, Option<bool>)> {
    for b in fn_bound(d, name) {
        let targs = items(tel(b, 1));
        let uses = match targs.get(2) {
            Some(v) if tag(v) == "effects" => items(tel(v, 1)).iter().map(st).collect(),
            Some(v) if tag(v) == "named" => {
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
            Some(v) if tag(v) == "panic" => match st(tel(v, 1)).as_str() {
                "never" => Some(false),
                "may" => Some(true),
                _ => None,
            },
            Some(v) if tag(v) == "named" => {
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
    None
}

fn gen_var(d: &Def, v: Option<&V>, kind: &str) -> Option<String> {
    let v = v?;
    if tag(v) != "named" || !items(tel(v, 2)).is_empty() {
        return None;
    }
    let nm = st(tel(v, 1));
    d.generics.iter().any(|g| g.0 == kind && g.1 == nm).then_some(nm)
}

impl<'a> Ck<'a> {
    /// 型の中の `Iterator` の impl の実引数 (トレイトの型引数の列)。
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

    /// 呼び出し先 `d` の効果変数・panic 変数を、実引数の型 `argt` (メソッドの受け手を含む。`off` は引数と宣言のずれ) で束ねる。
    fn bind(&self, d: &Def, off: usize, argt: &[Ty]) -> Bind {
        let mut out = Bind::default();
        let gn = d.gnames();
        for (k, a) in argt.iter().enumerate() {
            let Some(p) = d.params.get(k + off) else { break };
            let mut pt = from_v(&p.1, &gn);
            while let Ty::Ref(_, x, _) = pt {
                pt = *x;
            }
            if matches!(pt, Ty::Closure { .. }) {
                if tag(&p.1) == "fn" {
                    if let Some((uses, may)) = fn_effects(&strip_refs(a)) {
                        for e in items(tel(&p.1, 3)).iter().map(st) {
                            if is_effect_var(&e) && d.generics.iter().any(|g| g.0 == "uses" && g.1 == e) {
                                out.uses.entry(e).or_default().extend(uses.iter().cloned());
                            }
                        }
                        let pn = st(tel(&p.1, 4));
                        if let Some(m) = may {
                            if is_panic_var(&pn) && d.generics.iter().any(|g| g.0 == "panics" && g.1 == pn) {
                                *out.panics.entry(pn).or_default() |= m;
                            }
                        }
                    }
                }
                continue;
            }
            let mut slots = vec![];
            super::captures::param_slots(&from_v(&p.1, &gn), a, &mut slots);
            for (name, at) in slots {
                let bounds = fn_bound(d, &name);
                if bounds.is_empty() {
                    continue;
                }
                let Some((uses, may)) = fn_effects(&at) else { continue };
                for b in bounds {
                    let bt = items(tel(b, 1));
                    if let Some(e) = gen_var(d, bt.get(2), "uses") {
                        out.uses.entry(e).or_default().extend(uses.iter().cloned());
                    }
                    if let (Some(pv), Some(m)) = (gen_var(d, bt.get(3), "panics"), may) {
                        *out.panics.entry(pv).or_default() |= m;
                    }
                }
            }
        }
        self.iter_bind(d, off, argt, &mut out);
        out
    }

    /// `I: Iterator<T, E, P>` 境界の引数の型の `Iterator` の impl の、具体的な効果で `E` / `P` を束ねる。
    fn iter_bind(&self, d: &Def, off: usize, argt: &[Ty], out: &mut Bind) {
        let gn = d.gnames();
        for (k, a) in argt.iter().enumerate() {
            let Some(p) = d.params.get(k + off) else { break };
            let mut pt = from_v(&p.1, &gn);
            while let Ty::Ref(_, x, _) = pt {
                pt = *x;
            }
            let Ty::Param(name) = pt else { continue };
            let Ty::Named(tn, _, _) = strip_refs(a) else { continue };
            for g in d.generics.iter().filter(|g| g.0 == "type" && g.1 == name) {
                for b in items(&g.2) {
                    let V::L(bx) = b else { continue };
                    if st(&bx[0]) != "core.iter.Iterator" {
                        continue;
                    }
                    let bt = items(tel(b, 1));
                    let Some(itargs) = self.impl_targs_of("core.iter.Iterator", &tn) else { continue };
                    if let (Some(e), Some(ie)) = (gen_var(d, bt.get(1), "uses"), itargs.get(1)) {
                        if let Some(set) = targ_effs(ie) {
                            if set.iter().all(|x| !is_effect_var(x)) {
                                out.uses.entry(e).or_default().extend(set);
                            }
                        }
                    }
                    if let (Some(pv), Some(ip)) = (gen_var(d, bt.get(2), "panics"), itargs.get(2)) {
                        match targ_panic(ip).as_deref() {
                            Some("never") => {
                                out.panics.entry(pv).or_insert(false);
                            }
                            Some("may") => {
                                out.panics.insert(pv, true);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    fn has_iterator_bound(d: &Def) -> bool {
        d.generics.iter().any(|g| g.0 == "type" && items(&g.2).iter().any(|b| matches!(b, V::L(bx) if st(&bx[0]) == "core.iter.Iterator")))
    }

    /// 呼び出し先の実体化した効果の、検査の義務の部分 (束縛から来た uses と、`may` / 束縛されない panic 変数)。
    fn derived(&self, d: &Def, off: usize, argt: &[Ty]) -> (Vec<String>, Option<String>) {
        let bind = self.bind(d, off, argt);
        let concrete: Vec<String> = d.uses.iter().filter(|e| e.chars().next().map_or(false, |c| c.is_lowercase())).cloned().collect();
        let mut derived = vec![];
        for e in &d.uses {
            if let Some(set) = bind.uses.get(e) {
                derived.extend(set.iter().filter(|x| is_effect_var(x) || !covers(&concrete, x)).cloned());
            }
        }
        let mut dp: Option<String> = None;
        if let Some(pv) = d.panics.as_deref() {
            match bind.panics.get(pv) {
                Some(&m) => {
                    if m {
                        dp = Some("may".into());
                    }
                }
                None => {
                    if is_panic_var(pv) && !Self::has_iterator_bound(d) {
                        dp = Some(pv.to_string());
                    }
                }
            }
        }
        (derived, dp)
    }

    fn report(&mut self, name: &str, uses: &[String], panics: Option<&str>, line: usize) {
        if uses.is_empty() && panics != Some("may") && !panics.map_or(false, is_panic_var) {
            return;
        }
        if let Some((code, msg)) = violation(&self.c, name, uses, panics) {
            self.found.push((code, msg, line));
        }
    }

    /// `for x in it` が暗黙に呼ぶ `next` の効果 (入れ子のアダプタの `next` も)。
    fn for_next(&mut self, it: &Ty, line: usize) {
        let base = strip_refs(it);
        match &base {
            Ty::Array(..) | Ty::Slice(_) => return,
            Ty::Named(n, _, _) if n == "alloc.vec.Vec" => return,
            Ty::Param(pn) => {
                let mut cands = vec![];
                for g in self.c.d.generics.iter().filter(|g| g.0 == "type" && &g.1 == pn) {
                    for b in items(&g.2) {
                        if !matches!(b, V::L(bx) if st(&bx[0]) == "core.iter.Iterator") {
                            continue;
                        }
                        let ta = items(tel(b, 1));
                        cands.push((ta.get(1).and_then(targ_effs).unwrap_or_default(), ta.get(2).and_then(targ_panic)));
                    }
                }
                for (u, pn) in cands {
                    self.report("core.iter.Iterator.next", &u, pn.as_deref(), line);
                }
                return;
            }
            Ty::Named(..) => {}
            _ => return,
        }
        let Ty::Named(n, _, _) = &base else { return };
        let nx = format!("{}.next", n);
        let Some(c) = self.p.defs.iter().find(|x| x.kw == "fn" && x.name == nx && x.params.len() == 1) else { return };
        let self_arg = Ty::Ref(true, Box::new(base.clone()), crate::types::Region::Unk);
        let (fu, fp) = self.derived_full(c, &[self_arg.clone()]);
        let fu: Vec<String> = fu.into_iter().filter(|e| !is_effect_var(e)).collect();
        let fp = fp.filter(|p| !is_panic_var(p));
        if !c.inferred {
            self.report(&c.name.clone(), &fu, fp.as_deref(), line);
        }
        let gn = c.gnames();
        let iter_params: Vec<String> = c
            .generics
            .iter()
            .filter(|g| g.0 == "type" && items(&g.2).iter().any(|b| matches!(b, V::L(bx) if st(&bx[0]) == "core.iter.Iterator")))
            .map(|g| g.1.clone())
            .collect();
        if !iter_params.is_empty() {
            let mut slots = vec![];
            super::captures::param_slots(&from_v(&c.params[0].1, &gn), &self_arg, &mut slots);
            for (nm, at) in slots {
                if iter_params.contains(&nm) {
                    self.for_next(&at, line);
                }
            }
        }
    }

    /// 旧い検査の `callee_effects` の戻り値の (full uses, panics): `next` の宣言の効果を束縛で実体化したもの。
    fn derived_full(&self, d: &Def, argt: &[Ty]) -> (Vec<String>, Option<String>) {
        let bind = self.bind(d, 0, argt);
        let mut full = vec![];
        for e in &d.uses {
            match bind.uses.get(e) {
                Some(set) => full.extend(set.iter().cloned()),
                None => full.push(e.clone()),
            }
        }
        let panics = d.panics.as_deref().map(|p| match bind.panics.get(p) {
            Some(true) => "may".to_string(),
            Some(false) => "never".to_string(),
            None => p.to_string(),
        });
        (full, panics)
    }

    /// `dyn Tr<..>` 越しのメソッド `name`: トレイトの宣言を `dyn` の実引数で実体化する。
    fn dyn_call(&mut self, tr: &str, dargs: &[V], name: &str, line: usize) {
        self.trait_call(tr, dargs, name, line, " (through dyn)");
    }

    /// トレイトのメソッド `name` の宣言の効果を実引数 `dargs` で実体化して呼び出し元と比べる (`dyn` 越しと境界越し)。
    fn trait_call(&mut self, tr: &str, dargs: &[V], name: &str, line: usize, suffix: &str) {
        let Some(root) = crate::check::resolve_trait_fqn(self.p, tr) else { return };
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: Vec<(String, Vec<V>)> = vec![(root, dargs.to_vec())];
        while let Some((cur, cur_targs)) = queue.pop() {
            if !seen.insert(cur.clone()) {
                continue;
            }
            let Some(&ti) = self.p.types.get(&cur) else { continue };
            let td = &self.p.defs[ti];
            if td.kw != "trait" {
                continue;
            }
            if td.trait_sigs.iter().all(|s| s.0 != name) {
                let pairs = crate::check::align_targs(&td.generics, &cur_targs).unwrap_or_default();
                for (sup, sargs) in &td.requires {
                    if let Some(fq) = crate::check::resolve_trait_fqn(self.p, sup) {
                        let sa: Vec<V> = items(sargs)
                            .iter()
                            .map(|a| {
                                let var = match (targ_effs(a), targ_panic(a)) {
                                    (Some(e), _) if e.len() == 1 => Some(e[0].clone()),
                                    (_, Some(p)) => Some(p),
                                    _ => None,
                                };
                                var.and_then(|v| pairs.iter().find(|(n, _, _)| *n == v.as_str()).map(|(_, _, t)| (*t).clone())).unwrap_or_else(|| a.clone())
                            })
                            .collect();
                        queue.push((fq, sa));
                    }
                }
                continue;
            }
            if let Some((_, u, pn)) = td.trait_effs.iter().find(|e| e.0 == name) {
                let (u2, p2) = crate::check::inst_effects(&td.generics, &cur_targs, u, pn);
                let nm = format!("{}.{}{}", cur, name, suffix);
                if let Some((code, msg)) = violation(&self.c, &nm, &u2, Some(p2.as_str())) {
                    self.found.push((code, msg, line));
                }
            }
            return;
        }
    }
}

/// 型パラメータの受け手のメソッド呼び出し (`a.go()` は `MCall`、または経路 `a.go` の `Call` で `a` の型が `Param`) の (型パラメータ名, メソッド名)。
fn bound_recv(e: &TExpr) -> Option<(String, String)> {
    match &e.kind {
        TKind::MCall { recv, name, .. } => match strip_refs(&recv.ty) {
            Ty::Param(pn) => Some((pn, name.clone())),
            _ => None,
        },
        TKind::Call { callee, .. } => match (&callee.kind, strip_refs(&callee.ty)) {
            (TKind::Path(s), Ty::Param(pn)) if s.len() == 2 => Some((pn, s[1].clone())),
            _ => None,
        },
        _ => None,
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
        let mut in_closure: HashSet<u32> = HashSet::new();
        super::impl_sig::walk(&b.body, &mut |e| {
            if let TKind::Closure { body, .. } = &e.kind {
                super::impl_sig::walk(body, &mut |x| {
                    in_closure.insert(x.id);
                });
            }
        });
        let mut ck = Ck { p, c: Caller { d, uses: d.uses.clone(), panics: d.panics.clone().unwrap_or_else(|| "may".into()) }, found: vec![] };
        super::impl_sig::walk(&b.body, &mut |e| {
            if in_closure.contains(&e.id) {
                return;
            }
            match (&e.kind, &e.res) {
                // `F: Fn<.., E, P>` の型パラメータの値の呼び出し `f(x)`: 境界が宣言する具体的な効果と `may`。
                (TKind::Call { callee, .. }, Some(Callee::Local(f))) => {
                    let Some(pt) = b.params.iter().find(|q| &q.name == f).map(|q| strip_refs(&q.ty)) else { return };
                    let Ty::Param(pn) = pt else { return };
                    if let Some((bu, bp)) = bound_effects(d, &pn) {
                        let uses: Vec<String> = bu.into_iter().filter(|u| !d.generics.iter().any(|g| g.0 == "uses" && &g.1 == u)).collect();
                        let panics = (bp == Some(true)).then(|| "may".to_string());
                        ck.report(&format!("the `{}` bound of `{}`", f, pn), &uses, panics.as_deref(), e.line);
                    }
                    let _ = callee;
                }
                // `f.call(x)`: 境界の `Fn<.., E, P>` の効果の実引数 (変数のままのものは呼び出し元が同じ変数を持つか)。
                (TKind::Call { callee, .. }, Some(Callee::Unknown)) if matches!(&callee.kind, TKind::Path(s) if s.len() == 2 && s[1].starts_with("call")) => {
                    let TKind::Path(segs) = &callee.kind else { return };
                    let Ty::Param(pn) = strip_refs(&callee.ty) else { return };
                    for g in d.generics.iter().filter(|g| g.0 == "type" && g.1 == pn) {
                        for bd in items(&g.2) {
                            let V::L(bx) = bd else { continue };
                            let tr = st(&bx[0]);
                            if !tr.starts_with("core.ops.Fn") {
                                continue;
                            }
                            let bt = items(tel(bd, 1));
                            let uses = bt.get(2).and_then(targ_effs).unwrap_or_default();
                            let panics = bt.get(3).and_then(targ_panic);
                            ck.report(&format!("{}.{}", tr, segs[1]), &uses, panics.as_deref(), e.line);
                        }
                    }
                }
                (TKind::Other { name: "For", children }, _) => {
                    if let Some(it) = children.first() {
                        ck.for_next(&it.ty, e.line);
                    }
                }
                (TKind::Call { callee, args }, Some(Callee::Dyn(_))) => {
                    let name = match &callee.kind {
                        TKind::Path(segs) => segs.last().cloned().unwrap_or_default(),
                        _ => String::new(),
                    };
                    if let Ty::Dyn(tr, ta, _) = dyn_of(&callee.ty) {
                        ck.dyn_call(&tr, &ta.0, &name, e.line);
                    }
                    let _ = args;
                }
                // 型パラメータの受け手のメソッド `a.go()`: 境界のトレイトの宣言の効果 (境界の実引数で実体化)。
                (TKind::MCall { .. } | TKind::Call { .. }, Some(Callee::Unknown)) if bound_recv(e).is_some() => {
                    let Some((pn, name)) = bound_recv(e) else { return };
                    let name = &name;
                    let mut bounds: Vec<(String, Vec<V>)> = vec![];
                    for g in d.generics.iter().filter(|g| g.0 == "type" && g.1 == pn) {
                        for b in items(&g.2) {
                            let V::L(bx) = b else { continue };
                            bounds.push((st(&bx[0]), items(tel(b, 1)).to_vec()));
                        }
                    }
                    for (tr, ta) in bounds {
                        if tr.starts_with("core.ops.Fn") {
                            continue;
                        }
                        ck.trait_call(&tr, &ta, name, e.line, "");
                    }
                }
                (TKind::MCall { recv, name, .. }, Some(Callee::Dyn(_))) => {
                    if let Ty::Dyn(tr, ta, _) = dyn_of(&recv.ty) {
                        ck.dyn_call(&tr, &ta.0, name, e.line);
                    }
                }
                (TKind::Call { args, .. }, Some(Callee::Def(ci))) | (TKind::MCall { args, .. }, Some(Callee::Def(ci))) => {
                    let c = &p.defs[*ci];
                    if c.inferred {
                        return;
                    }
                    if !e.etargs.0.is_empty() {
                        // 明示型引数 `f::<..>`: 効果・panic の実引数で宣言を実体化する (実体化できなければ宣言のまま)。
                        let (u, pn) = match crate::check::inst_def_effects(c, &e.etargs.0) {
                            Some((u, pn)) => (u, pn),
                            None => (c.uses.iter().filter(|x| !is_effect_var(x)).cloned().collect(), c.panics.clone().filter(|x| !is_panic_var(x))),
                        };
                        if let Some((code, msg)) = violation(&ck.c, &c.name, &u, pn.as_deref()) {
                            ck.found.push((code, msg, e.line));
                        }
                        return;
                    }
                    let mut argt: Vec<Ty> = vec![];
                    match &e.kind {
                        TKind::MCall { recv, .. } => argt.push(recv.ty.clone()),
                        // `o.map(f)` は呼び先の経路 `o.map` が受け手を運ぶ (型は受け手の型)。
                        TKind::Call { callee, .. } if c.params.len() > args.len() && matches!(&callee.kind, TKind::Path(s) if s.len() >= 2) => argt.push(callee.ty.clone()),
                        _ => {}
                    }
                    argt.extend(args.iter().map(|a| a.ty.clone()));
                    let off = 0;
                    let (mut u, mut pn) = ck.derived(c, off, &argt);
                    // 宣言のままの具体的な効果・panic (S4 で旧の `violation` に代わる)。
                    let mut conc: Vec<String> = c.uses.iter().filter(|x| !is_effect_var(x)).cloned().collect();
                    conc.extend(u);
                    u = conc;
                    if pn.is_none() {
                        pn = c.panics.clone().filter(|x| !is_panic_var(x));
                    }
                    ck.report(&c.name.clone(), &u, pn.as_deref(), e.line);
                    // `I: Iterator<..>` 境界の引数は、呼び出し先が中で `next` を呼びうる。
                    let gn = c.gnames();
                    let iter_params: Vec<String> = c
                        .generics
                        .iter()
                        .filter(|g| g.0 == "type" && items(&g.2).iter().any(|b| matches!(b, V::L(bx) if st(&bx[0]) == "core.iter.Iterator")))
                        .map(|g| g.1.clone())
                        .collect();
                    if !iter_params.is_empty() {
                        for (k, a) in argt.iter().enumerate() {
                            let Some(prm) = c.params.get(k) else { continue };
                            let mut slots = vec![];
                            super::captures::param_slots(&from_v(&prm.1, &gn), a, &mut slots);
                            for (nm, at) in slots {
                                if iter_params.contains(&nm) {
                                    ck.for_next(&at, e.line);
                                }
                            }
                        }
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
