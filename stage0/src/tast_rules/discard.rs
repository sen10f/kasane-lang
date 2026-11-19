//! 規則 D: `()` と `!` 以外の値を捨てる式文 (仕様 4.6、E2042)。
//! ブロックの途中の式文、`while` / `loop` / `for` の本体の末尾の値、`else` の無い途中の `if` の then 枝の値を、TAST の式の型だけで見る
//! (型に未知・型変数が残るものは見ない)。旧い検査 (`check.rs` の `discard_check`・`discarded_by_type`、`types.rs`) と同じコード・行・文面。

use crate::check::Diag;
use crate::program::Program;
use crate::tast::*;
use crate::types::Ty;

fn known(t: &Ty) -> bool {
    match t {
        Ty::Unknown | Ty::Var(_) => false,
        Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().all(known),
        Ty::Ref(_, x, _) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => known(x),
        Ty::Closure { args, ret, .. } => args.iter().all(known) && known(ret),
        _ => true,
    }
}

fn is_unit(t: &Ty) -> bool {
    matches!(t.strip(), Ty::Unit | Ty::Never) || matches!(t.strip(), Ty::Tuple(xs) if xs.is_empty())
}

pub fn check(p: &Program, user: &[usize], tast: &Tast) -> Vec<Diag> {
    let mut out: Vec<Diag> = vec![];
    for &di in user {
        let d = &p.defs[di];
        if d.kw != "fn" && d.kw != "test" {
            continue;
        }
        let Some(b) = tast.bodies.get(&di) else { continue };
        let mut lines: Vec<usize> = vec![];
        super::impl_sig::walk(&b.body, &mut |e| match &e.kind {
            TKind::Block(ss) => {
                for s in ss.iter().take(ss.len().saturating_sub(1)) {
                    if let TStmt::Expr(x) = s {
                        discarded(x, &mut lines);
                    }
                }
            }
            TKind::Other { name: "While" | "Loop" | "For", children } => {
                if let Some(body) = children.last() {
                    if let TKind::Block(ss) = &body.kind {
                        if let Some(TStmt::Expr(x)) = ss.last() {
                            discarded(x, &mut lines);
                        }
                    }
                }
            }
            _ => {}
        });
        lines.sort();
        lines.dedup();
        for line in lines {
            out.push(Diag { file: d.file.clone(), code: "E2042".into(), fqn: d.name.clone(), line, col: 0, msg: "a value other than () is discarded; use core.mem.drop(x)".into() });
        }
    }
    out
}

/// 捨てられる式 `x` (文の位置): 型が `()` / `!` 以外に決まっていれば行を足す。`else` の無い `if` は then 枝の値。
fn discarded(x: &TExpr, lines: &mut Vec<usize>) {
    if let TKind::If { then, els: None, .. } = &x.kind {
        if known(&then.ty) && !is_unit(&then.ty) {
            lines.push(x.line);
        }
        return;
    }
    if known(&x.ty) && !is_unit(&x.ty) {
        lines.push(x.line);
    }
}
