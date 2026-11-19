//! 規則 U: `extern` の関数と `core.ptr` の関数は `unsafe` ブロックの中の直接の呼び出しでしか使えない (仕様 3.8、E5003)。
//! 呼び先は名前の解決先 (`Program::resolve` の `Sig`)、`unsafe` の中かは TAST の `Unsafe` の節点の下かで決める。
//! 関数を値として参照する形 (呼び出しの関数位置以外に名前が現れる) は `unsafe` の中でも E5003。旧い検査 (`check.rs` の
//! `check_extern_calls`) と同じコード・行・文面。`[unsafe] modules` の許可の検査 (E5001) は定義の署名・型の綴りを見る別の段に残す。

use std::collections::HashSet;

use crate::check::Diag;
use crate::program::{Program, Target};
use crate::tast::*;

pub fn check(p: &Program, user: &[usize], tast: &Tast) -> Vec<Diag> {
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
        for (line, msg) in found {
            out.push(Diag { file: d.file.clone(), code: "E5003".into(), fqn: d.name.clone(), line, col: 0, msg });
        }
    }
    out
}
