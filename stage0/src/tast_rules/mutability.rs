//! 規則 W: 可変にできない場所を可変に使う (E3031)。局所の束縛の `mut` の有無と型 (共有参照 `&T` か) だけを見る。
//! - `&mut x` / `a[i] = ..` の根の束縛が `let mut` でも `&mut` の参照でもない
//! - `&mut r.f` / `r.f = ..` / `*r = ..` を共有参照 `r: &T` 越しに書く
//! - `&mut self` のメソッドを、根の束縛が共有参照・`let mut` でない所有値で呼ぶ
//! - `FnMut` の閉包を共有参照越しに呼ぶ、`*e = ..` の `e` が共有参照を返す式
//! 旧い検査 (`types.rs` の `need_mut_place`・`need_mut`・`Assign`・`&mut`) と同じコード・行・文面。名前の見え方は束縛の名前で引く (旧い検査と同じ)。

use std::collections::{HashMap, HashSet};

use crate::check::Diag;
use crate::program::Program;
use crate::tast::*;
use crate::types::{from_v, Cap, Ty};

struct W<'a> {
    p: &'a Program,
    /// (名前, `PBind` の ID) → 型。引数・閉包の引数は名前だけで引く。
    by_id: HashMap<(String, u32), Ty>,
    by_name: HashMap<String, Option<Ty>>,
    muts: HashSet<String>,
    found: Vec<(String, usize)>,
}

fn strip_ref_kind(t: &Ty) -> &Ty {
    t.strip()
}

impl<'a> W<'a> {
    /// 名前から引く型: 同じ名前を違う型で束縛していれば決めない (`None`)。
    fn note_name(&mut self, n: &str, t: &Ty) {
        match self.by_name.get(n) {
            Some(Some(o)) if o != t => {
                self.by_name.insert(n.to_string(), None);
            }
            Some(_) => {}
            None => {
                self.by_name.insert(n.to_string(), Some(t.clone()));
            }
        }
    }

    fn ty_of_local(&self, name: &str, id: Option<u32>) -> Option<Ty> {
        if let Some(i) = id {
            if let Some(t) = self.by_id.get(&(name.to_string(), i)) {
                return Some(t.clone());
            }
        }
        self.by_name.get(name).cloned().flatten()
    }

    /// 式の根の局所 (名前, ID)。`x.f.g`・`x[i]` の連なりも。
    fn root(e: &TExpr) -> Option<(String, Option<u32>)> {
        let mut cur = e;
        loop {
            match &cur.kind {
                TKind::Field { base, .. } | TKind::Index { base, .. } => cur = base,
                _ => break,
            }
        }
        let TKind::Path(segs) = &cur.kind else { return None };
        let id = match cur.place.as_ref() {
            Some(Place::Local(_, i)) => Some(*i),
            Some(Place::Field(b, _)) => {
                let mut x = &**b;
                loop {
                    match x {
                        Place::Field(b2, _) => x = b2,
                        Place::Local(_, i) => break Some(*i),
                        _ => break None,
                    }
                }
            }
            _ => None,
        };
        Some((segs[0].clone(), id))
    }

    fn is_direct(e: &TExpr) -> bool {
        matches!(&e.kind, TKind::Path(s) if s.len() == 1)
    }

    fn need_mut_place(&mut self, e: &TExpr, what: &str, line: usize) {
        let Some((root, id)) = Self::root(e) else { return };
        let direct = Self::is_direct(e);
        let Some(t) = self.ty_of_local(&root, id) else { return };
        if self.muts.contains(&root) {
            return;
        }
        let bad = match &t {
            Ty::Unknown | Ty::Var(_) | Ty::Never => false,
            Ty::Ref(..) if what == "borrow" && direct => false,
            Ty::Ref(true, _, _) | Ty::Param(_) if !direct => false,
            Ty::Ref(false, _, _) if what == "borrow" && !direct && matches!(e.kind, TKind::Field { .. } | TKind::Path(_)) => false,
            _ => true,
        };
        if bad {
            self.found.push((format!("cannot {} `{}` as mutable: declare it with `let mut {}`", what, root, root), line));
        }
    }

    fn need_mut(&mut self, base: &str, id: Option<u32>, method: &str, line: usize) {
        let Some(t) = self.ty_of_local(base, id) else { return };
        match &t {
            Ty::Ref(false, _, _) => self.found.push((format!("cannot call the `&mut self` method `{}` through the shared reference `{}`", method, base), line)),
            Ty::Ref(true, _, _) | Ty::Unknown | Ty::Var(_) | Ty::Param(_) | Ty::Never => {}
            _ => {
                if !self.muts.contains(base) {
                    self.found.push((format!("cannot borrow `{}` as mutable to call `{}`: declare it with `let mut {}`", base, method, base), line));
                }
            }
        }
    }

    fn shared_then_fnmut(t: &Ty) -> bool {
        let mut shared = false;
        let mut cur = t;
        loop {
            match cur {
                Ty::Ref(m, inner, _) => {
                    shared |= !*m;
                    cur = inner;
                }
                Ty::Closure { cap, .. } => return shared && *cap == Cap::FnMut,
                _ => return false,
            }
        }
    }

    fn assign(&mut self, place: &TExpr) {
        match &place.kind {
            TKind::Index { .. } => self.need_mut_place(place, "assign to", place.line),
            TKind::Field { .. } | TKind::Path(_) | TKind::Unary { .. } => {
                let (root, via_deref): (Option<(Vec<String>, Option<u32>)>, bool) = match &place.kind {
                    TKind::Unary { op, operand } if op == "*" && matches!(operand.kind, TKind::Path(_)) => {
                        let TKind::Path(s) = &operand.kind else { unreachable!() };
                        let id = match operand.place.as_ref() {
                            Some(Place::Local(_, i)) => Some(*i),
                            _ => None,
                        };
                        (Some((s.clone(), id)), true)
                    }
                    TKind::Unary { op, operand } => {
                        if op == "*" && matches!(operand.ty.strip(), Ty::Ref(false, _, _)) {
                            self.found.push(("cannot assign through a shared reference returned by an expression".to_string(), place.line));
                        }
                        (None, false)
                    }
                    _ => {
                        // 欄の連なり `a.b.c` か経路形 `a.b`: 根の局所 (名前, 欄の数)。
                        let mut cur = place;
                        let mut n = 0;
                        while let TKind::Field { base, .. } = &cur.kind {
                            cur = base;
                            n += 1;
                        }
                        match &cur.kind {
                            TKind::Path(s) => {
                                let mut segs = s.clone();
                                segs.extend(std::iter::repeat(String::new()).take(n));
                                let id = match cur.place.as_ref() {
                                    Some(Place::Local(_, i)) => Some(*i),
                                    Some(Place::Field(b, _)) => {
                                        let mut x = &**b;
                                        loop {
                                            match x {
                                                Place::Field(b2, _) => x = b2,
                                                Place::Local(_, i) => break Some(*i),
                                                _ => break None,
                                            }
                                        }
                                    }
                                    _ => None,
                                };
                                (Some((segs, id)), false)
                            }
                            _ => (None, false),
                        }
                    }
                };
                if let Some((segs, id)) = root {
                    let n = &segs[0];
                    let direct = segs.len() == 1;
                    if via_deref || !direct {
                        if let Some(t) = self.ty_of_local(n, id) {
                            if matches!(t, Ty::Ref(false, _, _)) {
                                self.found.push((format!("cannot assign through the shared reference `{}`", n), place.line));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

pub fn check(p: &Program, user: &[usize], tast: &Tast) -> Vec<Diag> {
    let mut out: Vec<Diag> = vec![];
    for &di in user {
        let d = &p.defs[di];
        if d.kw != "fn" && d.kw != "test" {
            continue;
        }
        let Some(b) = tast.bodies.get(&di) else { continue };
        let mut w = W { p, by_id: HashMap::new(), by_name: HashMap::new(), muts: HashSet::new(), found: vec![] };
        let _ = strip_ref_kind;
        for q in &b.params {
            w.note_name(&q.name, &q.ty);
        }
        fn pats(q: &TPat, w: &mut W) {
            if q.kind == "PBind" {
                if let (Some(n), Some((_, t, _))) = (&q.name, q.info.as_ref().and_then(|i| i.bind.as_ref())) {
                    w.by_id.insert((n.clone(), q.id), t.clone());
                    w.note_name(n, t);
                    if q.lit.iter().any(|l| l == "mut") {
                        w.muts.insert(n.clone());
                    }
                }
            }
            for c in &q.children {
                pats(c, w);
            }
        }
        super::impl_sig::walk(&b.body, &mut |e| {
            match &e.kind {
                TKind::Block(ss) => {
                    for s in ss {
                        if let TStmt::Let { pat, .. } = s {
                            pats(pat, &mut w);
                        }
                    }
                }
                TKind::Match { arms, .. } => arms.iter().for_each(|a| pats(&a.pat, &mut w)),
                TKind::Closure { params, .. } => {
                    for q in params {
                        w.note_name(&q.name, &q.ty);
                    }
                }
                _ => {}
            }
            e.pats.iter().for_each(|q| pats(q, &mut w));
        });
        let sdef = from_v;
        let _ = sdef;
        // 規則の本体: 式を前順に見る。文の `Assign` はブロックの文から取る。
        super::impl_sig::walk(&b.body, &mut |e| {
            if let TKind::Block(ss) = &e.kind {
                for s in ss {
                    if let TStmt::Assign { place, .. } = s {
                        w.assign(place);
                    }
                }
            }
        });
        super::impl_sig::walk(&b.body, &mut |e| match &e.kind {
            TKind::Unary { op, operand } if op == "&mut" => {
                w.need_mut_place(operand, "borrow", e.line);
                let proj = match &operand.kind {
                    TKind::Field { .. } => true,
                    TKind::Path(s) => s.len() >= 2,
                    _ => false,
                };
                if proj {
                    if let Some((root, id)) = W::root(operand) {
                        if let Some(t) = w.ty_of_local(&root, id) {
                            if matches!(t, Ty::Ref(false, _, _)) {
                                w.found.push((format!("cannot borrow a field of `{}` as mutable through the shared reference", root), e.line));
                            }
                        }
                    }
                }
            }
            TKind::Call { callee, .. } => match (&e.res, &callee.kind) {
                // 局所の関数値・閉包 `f()`。
                (Some(Callee::Local(f)), _) => {
                    let id = None;
                    if let Some(t) = w.ty_of_local(f, id) {
                        if W::shared_then_fnmut(&t) {
                            w.found.push((format!("cannot call the `FnMut` closure `{}` through a shared reference: borrow it with `&mut`", f), e.line));
                        }
                    }
                }
                // `x.m(..)` の経路形: 呼び先が `&mut self` のメソッドなら受け手の根の束縛を見る。
                (Some(Callee::Def(ci)), TKind::Path(segs)) if segs.len() >= 2 && w.ty_of_local(&segs[0], None).is_some() => {
                    let c = &p.defs[*ci];
                    if matches!(c.params.first().map(|q| from_v(&q.1, &c.gnames())), Some(Ty::Ref(true, _, _))) {
                        w.need_mut(&segs[0], None, &segs[segs.len() - 1], e.line);
                    }
                }
                _ => {}
            },
            TKind::MCall { recv, name, .. } => {
                if let Some(Callee::Def(ci)) = &e.res {
                    let c = &p.defs[*ci];
                    if matches!(c.params.first().map(|q| from_v(&q.1, &c.gnames())), Some(Ty::Ref(true, _, _))) {
                        if let Some((root, id)) = W::root(recv) {
                            w.need_mut(&root, id, name, e.line);
                        }
                    }
                }
            }
            _ => {}
        });
        w.found.sort();
        w.found.dedup();
        for (msg, line) in w.found {
            out.push(Diag { file: d.file.clone(), code: "E3031".into(), fqn: d.name.clone(), line, col: 0, msg });
        }
    }
    out
}
