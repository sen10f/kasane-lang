//! 規則 M: `match` の網羅性 (仕様 4.1、E2021 と E2052)。パターン行列の判定は `exhaust.rs` をそのまま使い、
//! ここは TAST のパターン (`TPat`)・走査対象の型・列挙子の解決から行列の入力 (`XP`) を作るだけ。
//! 旧い検査 (`check.rs` の `exhaustive`・`to_xp`) と同じコード・行・文面。

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use crate::check::Diag;
use crate::exhaust::XP;
use crate::program::{Program, Resolved, Target};
use crate::tast::*;
use crate::types::Ty;

fn struct_fields(p: &TPat, out: &mut HashMap<String, BTreeSet<String>>) {
    match p.kind {
        "PStruct" => {
            let e = out.entry(p.path.join(".")).or_default();
            for n in &p.lit {
                e.insert(n.clone());
            }
            for c in &p.children {
                struct_fields(c, out);
            }
        }
        "PTuple" | "PVariant" | "PRef" => p.children.iter().for_each(|q| struct_fields(q, out)),
        _ => {}
    }
}

struct X<'a> {
    p: &'a Program,
    file: &'a str,
}

impl<'a> X<'a> {
    fn to_xp(&self, p: &TPat, sf: &HashMap<String, BTreeSet<String>>) -> Option<XP> {
        let single = |key: &str, n: usize| Some(Rc::new(vec![(key.to_string(), n)]));
        match p.kind {
            "PWild" | "PBind" => Some(XP::Wild),
            "PRef" => self.to_xp(p.children.first()?, sf),
            "PTuple" => {
                let args: Option<Vec<XP>> = p.children.iter().map(|q| self.to_xp(q, sf)).collect();
                let args = args?;
                Some(XP::Ctor { fam: single("tuple", args.len()), key: "tuple".into(), args })
            }
            "PStruct" => {
                let key = p.path.join(".");
                let names: Vec<String> = sf.get(&key)?.iter().cloned().collect();
                let mut args = vec![];
                for n in &names {
                    let sub = p.lit.iter().position(|x| x == n);
                    args.push(match sub.and_then(|i| p.children.get(i)) {
                        Some(x) => self.to_xp(x, sf)?,
                        None => XP::Wild,
                    });
                }
                let k = format!("struct:{}", key);
                Some(XP::Ctor { fam: single(&k, args.len()), key: k, args })
            }
            "PLit" => {
                let kind = p.lit.first().cloned().unwrap_or_default();
                if kind == "bool" {
                    let all = Rc::new(vec![("True".to_string(), 0), ("False".to_string(), 0)]);
                    let k = if p.path.get(1).map_or(false, |b| b == "true") { "True" } else { "False" };
                    return Some(XP::Ctor { fam: Some(all), key: k.into(), args: vec![] });
                }
                if kind == "unit" {
                    return Some(XP::Ctor { fam: single("()", 0), key: "()".into(), args: vec![] });
                }
                Some(XP::Ctor { fam: None, key: format!("lit:{}", p.path.first().cloned().unwrap_or_default()), args: vec![] })
            }
            "PVariant" | "PPath" => {
                let segs = &p.path;
                let args: Vec<XP> = if p.kind == "PVariant" { p.children.iter().map(|q| self.to_xp(q, sf)).collect::<Option<_>>()? } else { vec![] };
                let named = |en: &str, vn: &str| (en.to_string(), vn.to_string());
                let target = match self.p.resolve(segs, self.file) {
                    Some(Resolved { target: Target::Variant(en, vn), .. }) => Some((en, vn)),
                    _ if segs.len() == 1 && segs[0] == "None" => Some(named("Option", "None")),
                    _ if segs.len() == 1 && segs[0] == "Some" => Some(named("Option", "Some")),
                    _ if segs.len() == 1 && segs[0] == "Ok" => Some(named("Result", "Ok")),
                    _ if segs.len() == 1 && segs[0] == "Err" => Some(named("Result", "Err")),
                    _ => None,
                };
                let Some((en, vn)) = target else {
                    return Some(XP::Ctor { fam: None, key: format!("lit:{}", segs.join(".")), args });
                };
                let all: Vec<(String, usize)> = match en.as_str() {
                    "Option" => vec![("Some".into(), 1), ("None".into(), 0)],
                    "Result" => vec![("Ok".into(), 1), ("Err".into(), 1)],
                    _ => {
                        let ti = *self.p.types.get(&en)?;
                        self.p.defs[ti].variants.iter().map(|v| (v.0.clone(), v.1.len())).collect()
                    }
                };
                let want = all.iter().find(|x| x.0 == vn)?.1;
                if want != args.len() {
                    return None;
                }
                Some(XP::Ctor { fam: Some(Rc::new(all)), key: vn, args })
            }
            _ => None,
        }
    }
}

pub fn check(p: &Program, user: &[usize], tast: &Tast) -> Vec<Diag> {
    let mut out: Vec<Diag> = vec![];
    for &di in user {
        let d = &p.defs[di];
        let Some(b) = tast.bodies.get(&di) else { continue };
        let x = X { p, file: &d.file };
        let mut found: Vec<(&'static str, String, usize)> = vec![];
        super::impl_sig::walk(&b.body, &mut |e| {
            let TKind::Match { scrut, arms } = &e.kind else { return };
            let pats: Vec<&TPat> = arms.iter().map(|a| &a.pat).collect();
            if pats.iter().any(|q| q.kind == "PWild" && q.lit.iter().any(|l| l == "other")) {
                return;
            }
            if pats.iter().any(|q| matches!(q.kind, "PWild" | "PBind")) {
                let by_arm = pats.iter().any(|q| match q.kind {
                    "PVariant" | "PPath" => {
                        (q.path.len() == 1 && matches!(q.path[0].as_str(), "None" | "Some" | "Ok" | "Err"))
                            || matches!(p.resolve(&q.path, &d.file), Some(Resolved { target: Target::Variant(..), .. }))
                    }
                    "PLit" => q.lit.first().map_or(false, |k| k == "bool"),
                    _ => false,
                });
                if by_arm {
                    found.push(("E2021", "`_` / binding patterns cannot match an enum or bool; list all cases".into(), e.line));
                    return;
                }
                let mut s = scrut.ty.strip().clone();
                while let Ty::Ref(_, x, _) = s {
                    s = x.strip().clone();
                }
                if matches!(&s, Ty::Prim(n) if n == "bool") {
                    found.push(("E2021", "`_` / binding patterns cannot match a bool; list `true` and `false`".into(), e.line));
                    return;
                }
                if let Ty::Named(n, _, _) = &s {
                    if let Some(&ti) = p.types.get(n) {
                        if p.defs[ti].kw == "enum" {
                            found.push(("E2021", "`_` / binding patterns cannot match an enum; list all variants".into(), e.line));
                            return;
                        }
                    }
                }
            }
            let mut sf: HashMap<String, BTreeSet<String>> = HashMap::new();
            for q in &pats {
                struct_fields(q, &mut sf);
            }
            let mut xps = vec![];
            for q in &pats {
                match x.to_xp(q, &sf) {
                    Some(v) => xps.push(v),
                    None => return,
                }
            }
            let (wit, dead) = crate::exhaust::analyze(&xps);
            if let Some(w) = wit {
                found.push(("E2021", format!("non-exhaustive match: pattern `{}` is not covered", w), e.line));
            }
            for i in dead {
                found.push(("E2052", "unreachable `match` arm: the earlier arms already cover every value it matches".into(), arms[i].pat.line));
            }
        });
        for (code, msg, line) in found {
            let dg = Diag { file: d.file.clone(), code: code.into(), fqn: d.name.clone(), line, col: 0, msg };
            if !out.iter().any(|o| o.file == dg.file && o.line == dg.line && o.code == dg.code && o.msg == dg.msg) {
                out.push(dg);
            }
        }
    }
    out
}
