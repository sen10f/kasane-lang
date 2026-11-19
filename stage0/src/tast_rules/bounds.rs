//! 規則 I の拡張: 型パラメータの境界の充足 (仕様 5.1、E2030)。呼び出しの型引数の実体 (`targs`) が、呼び先の宣言の境界 `T: Tr`
//! を満たすかを、型・`impl` の表・`derive`・呼び出し元の境界だけで決める。`derive` で得た実装は型引数の成り立ちに還元する
//! (`Option<T>: PartialEq` ⇔ `T: PartialEq`)。旧い検査 (`types.rs` の `check_bounds`・`unsatisfied`) と同じコード・行・文面。

use crate::check::{items, resolve_trait_fqn, st, tel, Diag, DERIVE_TRAITS};
use crate::parser::V;
use crate::program::{Def, Program};
use crate::tast::*;
use crate::types::{from_v, param_bound_implies, Ty};

struct B<'a> {
    p: &'a Program,
    caller: &'a Def,
}

impl<'a> B<'a> {
    fn implements(&self, ty: &str, tr: &str) -> bool {
        let Some(&i) = self.p.types.get(ty) else { return true };
        if self.p.impls.contains(&(tr.to_string(), ty.to_string())) {
            return true;
        }
        if tr == "alloc.alloc.TryClone" && self.implements(ty, "core.ops.Clone") {
            return true;
        }
        self.p.defs[i].derive.iter().any(|x| DERIVE_TRAITS.iter().any(|(k, v)| k == x && *v == tr))
    }

    fn unsatisfied(&self, t: &Ty, tr: &str) -> Option<Ty> {
        match t {
            Ty::Named(n, args, _) => {
                let n = &match n.as_str() {
                    "Option" => "core.option.Option".to_string(),
                    "Result" => "core.result.Result".to_string(),
                    _ => n.clone(),
                };
                let &i = self.p.types.get(n)?;
                if !self.implements(n, tr) {
                    return Some(t.clone());
                }
                if self.p.impls.contains(&(tr.to_string(), n.clone())) {
                    let imp = self.p.defs.iter().find(|d| d.kw == "impl" && d.impl_trait == tr && crate::check::impl_target(&d.impl_for) == *n);
                    let Some(imp) = imp else { return None };
                    if let Ty::Named(_, iargs, _) = from_v(&imp.impl_for, &imp.gnames()) {
                        if iargs.len() == args.len() {
                            for (ia, a) in iargs.iter().zip(args) {
                                let Ty::Param(gp) = ia else { continue };
                                for g in imp.generics.iter().filter(|g| g.0 == "type" && g.1 == *gp) {
                                    for b in items(&g.2) {
                                        let V::L(bx) = b else { continue };
                                        if let Some(leaf) = self.unsatisfied(a.strip(), &st(&bx[0])) {
                                            return Some(leaf);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    return None;
                }
                let gs: Vec<&(String, String, V)> = self.p.defs[i].generics.iter().filter(|g| g.0 != "life").collect();
                if gs.len() != args.len() {
                    return None;
                }
                gs.iter().zip(args).filter(|(g, _)| g.0 == "type").find_map(|(_, a)| self.unsatisfied(a.strip(), tr))
            }
            Ty::Param(pn) => (param_bound_implies(self.p, self.caller, pn, tr) == Some(false)).then(|| t.clone()),
            Ty::Prim(p) if !(tr.starts_with("core.") || tr.starts_with("alloc.") || tr.starts_with("std.")) => {
                let has = self.p.impls.contains(&(tr.to_string(), p.clone())) || self.p.impls.contains(&(tr.to_string(), "blanket".to_string()));
                (!has).then(|| t.clone())
            }
            Ty::Prim(p) if p == "f32" || p == "f64" => matches!(tr, "core.cmp.Eq" | "core.cmp.Ord" | "core.hash.Hash").then(|| t.clone()),
            Ty::Tuple(ts) => ts.iter().find_map(|x| self.unsatisfied(x.strip(), tr)),
            Ty::Array(x, _) | Ty::Slice(x) => self.unsatisfied(x.strip(), tr),
            _ => None,
        }
    }

    fn call(&self, callee: &Def, targs: &[Ty], line: usize, out: &mut Vec<(String, usize)>) {
        // 型引数の表は `type` / `const` の宣言の順。
        let gens: Vec<&(String, String, V)> = callee.generics.iter().filter(|g| g.0 == "type" || g.0 == "const").collect();
        for (g, t) in gens.iter().zip(targs) {
            if g.0 != "type" || items(&g.2).is_empty() {
                continue;
            }
            let t = t.strip();
            for b in items(&g.2) {
                let V::L(bx) = b else { continue };
                let tr = st(&bx[0]);
                if tr == "core.marker.Send" || tr == "core.marker.Sync" || tr.starts_with("core.ops.Fn") {
                    continue;
                }
                let known = self.p.types.get(&tr).map_or(false, |&i| self.p.defs[i].kw == "trait") || DERIVE_TRAITS.iter().any(|(_, v)| *v == tr);
                let _ = tel;
                if !known {
                    continue;
                }
                if let Some(leaf) = self.unsatisfied(t, &tr) {
                    let mut msg = format!("the trait bound `{}: {}` of {} is not satisfied by {}", g.1, tr, callee.name, t.show());
                    if let Ty::Param(pn) = &leaf {
                        let short = tr.rsplit('.').next().unwrap_or(&tr);
                        msg.push_str(&format!(
                            ": `{}` has no bound `{}` in {}; add `{}: {}` to the type parameters of the caller",
                            pn, short, self.caller.name, pn, tr
                        ));
                    } else if leaf != *t {
                        let short = tr.rsplit('.').next().unwrap_or(&tr);
                        msg.push_str(&format!(
                            ": `{}` is not `{}` (the type arguments of a derived impl must satisfy it too); add `derive [{}]` to `{}`",
                            leaf.show(),
                            short,
                            short,
                            leaf.show()
                        ));
                    }
                    out.push((msg, line));
                }
            }
        }
        let _ = resolve_trait_fqn;
    }
}

pub fn check(p: &Program, user: &[usize], tast: &Tast) -> Vec<Diag> {
    let mut out: Vec<Diag> = vec![];
    for &di in user {
        let d = &p.defs[di];
        if d.kw != "fn" {
            continue;
        }
        let Some(b) = tast.bodies.get(&di) else { continue };
        let ck = B { p, caller: d };
        let mut found: Vec<(String, usize)> = vec![];
        super::impl_sig::walk(&b.body, &mut |e| {
            if let (TKind::Call { .. } | TKind::MCall { .. }, Some(Callee::Def(ci))) = (&e.kind, &e.res) {
                if !e.targs.is_empty() {
                    ck.call(&p.defs[*ci], &e.targs, e.line, &mut found);
                }
            }
        });
        // 比較演算子の被演算子が型パラメータのとき、境界が比較のトレイトを (スーパートレイトまで辿って) 含むこと。
        super::impl_sig::walk(&b.body, &mut |e| {
            let TKind::Binary { op, lhs, .. } = &e.kind else { return };
            if !matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=") {
                return;
            }
            let Ty::Param(pn) = lhs.ty.strip().clone() else { return };
            let Some(g) = d.generics.iter().find(|g| g.0 == "type" && g.1 == pn) else { return };
            let mut todo: Vec<String> = items(&g.2).iter().filter_map(|b| if let V::L(bx) = b { Some(st(&bx[0])) } else { None }).collect();
            let need = if op == "==" || op == "!=" { "core.cmp.PartialEq" } else { "core.cmp.PartialOrd" };
            let mut seen = std::collections::HashSet::new();
            while let Some(tr) = todo.pop() {
                if !seen.insert(tr.clone()) {
                    continue;
                }
                if tr == need {
                    return;
                }
                match p.types.get(&tr) {
                    Some(&ti) => todo.extend(p.defs[ti].requires.iter().map(|r| r.0.clone())),
                    None => return,
                }
            }
            found.push((format!("`{}` on values of type parameter `{}` requires the bound `{}: {}<{}>`", op, pn, pn, need, pn), e.line));
        });
        found.sort();
        found.dedup();
        for (msg, line) in found {
            out.push(Diag { file: d.file.clone(), code: "E2030".into(), fqn: d.name.clone(), line, col: 0, msg });
        }
    }
    out
}
