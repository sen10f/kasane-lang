//! 規則 U: `extern` の関数と `core.ptr` の関数は `unsafe` ブロックの中の直接の呼び出しでしか使えない (仕様 3.8、E5003)。
//! 呼び先は名前の解決先 (`Program::resolve` の `Sig`)、`unsafe` の中かは TAST の `Unsafe` の節点の下かで決める。
//! 関数を値として参照する形 (呼び出しの関数位置以外に名前が現れる) は `unsafe` の中でも E5003。旧い検査 (`check.rs` の
//! `check_extern_calls`) と同じコード・行・文面。`[unsafe] modules` の許可の検査 (E5001) は定義の署名・型の綴りを見る別の段に残す。

use std::collections::HashSet;

use crate::check::Diag;
use crate::program::{Program, Target};
use crate::tast::*;

pub fn check(p: &Program, user: &[usize], tast: &Tast, unsafe_modules: &[(String, String)]) -> Vec<Diag> {
    let mut out: Vec<Diag> = vec![];
    for &di in user {
        let d = &p.defs[di];
        let Some(b) = tast.bodies.get(&di) else { continue };
        let mut in_unsafe: HashSet<u32> = HashSet::new();
        let mut callee_ids: HashSet<u32> = HashSet::new();
        super::impl_sig::walk(&b.body, &mut |e| match &e.kind {
            TKind::Other { name: "Unsafe", children } => {
                for c in children {
                    super::impl_sig::walk(c, &mut |x| {
                        in_unsafe.insert(x.id);
                    });
                }
            }
            TKind::Call { callee, .. } => {
                callee_ids.insert(callee.id);
            }
            _ => {}
        });
        let target = |segs: &[String]| -> Option<String> {
            let r = p.resolve(segs, &d.file)?;
            if let Target::Sig(i) = r.target {
                let t = &p.defs[i];
                if r.rest.is_empty() && (t.kw == "extern" || t.name.starts_with("core.ptr.")) {
                    return Some(t.name.clone());
                }
            }
            None
        };
        let mut found: Vec<(usize, String)> = vec![];
        super::impl_sig::walk(&b.body, &mut |e| match &e.kind {
            TKind::Call { callee, .. } if !in_unsafe.contains(&e.id) => {
                if let TKind::Path(segs) = &callee.kind {
                    if let Some(n) = target(segs) {
                        found.push((e.line, format!("the external or raw-pointer function `{}` can only be called inside an `unsafe` block", n)));
                    }
                }
            }
            TKind::Path(segs) if !callee_ids.contains(&e.id) => {
                if let Some(n) = target(segs) {
                    found.push((e.line, format!("the external or raw-pointer function `{}` cannot be used as a value (it can only be called directly, e.g. `{}(..)`)", n, n)));
                }
            }
            _ => {}
        });
        found.sort();
        found.dedup();
        // E5001: 許可の無いモジュールでは、呼び先の署名が生ポインタを持つ呼び出しを、呼び方 (経路・メソッド・総称の境界・閉包の中) に関わらず拒む。
        if !crate::check::module_allows_unsafe(unsafe_modules, &d.module, &d.file) {
            let mut raw: Vec<(usize, String)> = vec![];
            super::impl_sig::walk(&b.body, &mut |e| {
                if !matches!(e.kind, TKind::Call { .. } | TKind::MCall { .. }) {
                    return;
                }
                let mut ids: Vec<usize> = vec![];
                match &e.res {
                    Some(Callee::Def(i)) | Some(Callee::Dyn(i)) => ids.push(*i),
                    Some(Callee::Methods(v)) => ids.extend(v.iter().copied()),
                    _ => {}
                }
                for i in ids {
                    let t = &p.defs[i];
                    let mname = if let TKind::MCall { name, .. } = &e.kind { name.as_str() } else { "" };
                    let hit = crate::check::is_unsafe_only(t)
                        || crate::check::sig_has_ptr(t)
                        || (t.kw == "trait" && t.trait_sigs.iter().any(|(n, ps, r)| n == mname && (ps.iter().any(|q| crate::check::find_ptr_ty(&q.1, 0).is_some()) || crate::check::find_ptr_ty(r, 0).is_some())));
                    if hit {
                        raw.push((e.line, format!("`{}` uses raw pointers and is only allowed in modules listed in [unsafe] modules", t.name)));
                    }
                }
            });
            raw.sort();
            raw.dedup();
            for (line, msg) in raw {
                out.push(Diag { file: d.file.clone(), code: "E5001".into(), fqn: d.name.clone(), line, col: 0, msg });
            }
        }
        for (line, msg) in found {
            out.push(Diag { file: d.file.clone(), code: "E5003".into(), fqn: d.name.clone(), line, col: 0, msg });
        }
    }
    out
}
