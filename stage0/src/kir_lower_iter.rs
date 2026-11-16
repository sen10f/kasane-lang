//! KIR の下ろし: 反復子の連なり (`core.iter.fold` / `core.iter.count`、設計_KIR.md 13 章、kir/closures)。
//!
//! `core.iter.range(a, b)` を元に、`core.iter.map(it, f)` / `core.iter.filter(it, p)` を重ねた連なりを、
//! `fold(it, init, f)` / `count(it)` の終端で 1 つのループに融合する (clif の `run_pipe` と同じ意味)。
//! 反復子の値は作らない。クロージャは取り込みの値を先頭の引数に取る関数 (`kir_lower_coll.rs`) として下ろし、
//! ループの中でその関数を呼ぶ。`filter` の述語には、要素を一時の領域に置いて共有参照で渡す。
//! 対象外 (理由を付けて拒否): `range` 以外の元 (Vec の `iter()`・`chars`・自作の反復子)、`take` / `enumerate`、
//! 値を持たない型 (要素や累積が集成体・Vec など) の連なり。

use super::coll::{closure_arg_kind, Kind};
use super::{Lw, LR};
use crate::check::{items, nf, nlineno as nline, nname, strs};
use crate::kir::{Aop, Cc, Mode, Op, Pointee, Ty, VId};
use crate::parser::V;

/// 連なりの段。
#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Map,
    Filter,
}

/// `core.iter.<name>` の呼び出しなら、その引数。
fn iter_call<'a>(e: &'a V, name: &str) -> Option<&'a [V]> {
    if nname(e) != "Call" || nname(&nf(e)[0]) != "Path" {
        return None;
    }
    (strs(&nf(&nf(e)[0])[0]) == ["core", "iter", name]).then(|| items(&nf(e)[1]))
}

/// 連なりの元の範囲: 式の 2 引数、または `let r = core.iter.range(a, b)` で評価済みの端。
enum Src<'a> {
    Exprs(&'a V, &'a V),
    Vals(VId, VId, Ty),
}

/// `let r = core.iter.range(a, b)` の右辺なら、その 2 引数。
pub(crate) fn range_args(e: &V) -> Option<(&V, &V)> {
    let a = iter_call(e, "range")?;
    (a.len() == 2).then(|| (&a[0], &a[1]))
}

/// 反復子の式を、元の範囲と、段 (内側から順) に分ける。
fn parse_pipe<'a>(lw: &Lw, it: &'a V, stages: &mut Vec<(Stage, &'a V)>) -> Option<Src<'a>> {
    if let Some(a) = iter_call(it, "range") {
        return (a.len() == 2).then(|| Src::Exprs(&a[0], &a[1]));
    }
    if nname(it) == "Path" {
        let segs = strs(&nf(it)[0]);
        if segs.len() == 1 {
            if let Some(super::coll::Kind::Range { start, end, ty }) = lw.lookup(&segs[0]).and_then(|s| lw.coll.kinds.get(&s)) {
                return Some(Src::Vals(*start, *end, *ty));
            }
        }
    }
    for (name, st) in [("map", Stage::Map), ("filter", Stage::Filter)] {
        if let Some(a) = iter_call(it, name) {
            if a.len() != 2 {
                return None;
            }
            let r = parse_pipe(lw, &a[0], stages)?;
            stages.push((st, &a[1]));
            return Some(r);
        }
    }
    None
}

/// 取り込みの値と鍵を持つクロージャ (式、または `let` で束縛した名前) を評価する。
fn closure_of(lw: &mut Lw, a: &V, line: usize) -> LR<Option<Kind>> {
    match closure_arg_kind(lw, a)? {
        Some(k) => Ok(Some(k)),
        None => lw.err(line, "反復子の段・終端に渡せるのはクロージャ (式か let で束縛した名前) だけ段階 1 の対象"),
    }
}

fn call(lw: &mut Lw, k: &Kind, args: Vec<VId>) -> VId {
    let Kind::Closure { key, ret, caps, .. } = k else { unreachable!("closure_of が保証する") };
    lw.callees.insert(key.clone());
    let all: Vec<VId> = caps.iter().copied().chain(args).collect();
    lw.emit(*ret, Op::Call { callee: key.clone(), args: all })
}

fn sig(k: &Kind) -> (&[Ty], Ty) {
    let Kind::Closure { args, ret, .. } = k else { unreachable!("closure_of が保証する") };
    (args, *ret)
}

/// `core.iter.fold(it, init, f)` (`fold = true`) / `core.iter.count(it)`。
pub(crate) fn pipe_call(lw: &mut Lw, e: &V, fold: bool) -> LR<Option<VId>> {
    let line = nline(e);
    let argv = items(&nf(e)[1]);
    if argv.len() != if fold { 3 } else { 1 } {
        return lw.err(line, "反復子の終端の引数の個数が違う");
    }
    let mut stages = vec![];
    let Some(src) = parse_pipe(lw, &argv[0], &mut stages) else {
        return lw.err(line, "反復子の元は core.iter.range(a, b) と map / filter の連なりだけ段階 1 の対象 (take・enumerate・Vec の iter は未対応)");
    };
    let (start, end, ity) = match src {
        Src::Exprs(a, b) => {
            let Some(start) = lw.expr(a)? else { return Ok(None) };
            let Some(end) = lw.expr(b)? else { return Ok(None) };
            (start, end, lw.ty_of(a)?)
        }
        Src::Vals(start, end, ty) => (start, end, ty),
    };
    if !ity.is_int() || lw.vty(end) != ity {
        return lw.err(line, "core.iter.range の範囲は同じ整数の型だけ段階 1 の対象");
    }
    let mut ks = vec![];
    for (_, c) in &stages {
        let Some(k) = closure_of(lw, c, line)? else { return Ok(None) };
        ks.push(k);
    }
    let (init, fk) = if fold {
        let Some(init) = lw.expr(&argv[1])? else { return Ok(None) };
        let Some(k) = closure_of(lw, &argv[2], line)? else { return Ok(None) };
        (init, Some(k))
    } else {
        (lw.emit(Ty::Usize, Op::Const(0)), None)
    };
    let acc_ty = lw.vty(init);
    if !(acc_ty.is_int() || acc_ty.is_float() || acc_ty == Ty::Bool) {
        return lw.err(line, "fold の累積はスカラーだけ段階 1 の対象");
    }

    let (xb, xp) = lw.new_block(&[acc_ty]);
    let (ib, ip) = lw.new_block(&[acc_ty]);
    let (hb, hp) = lw.new_block(&[ity, acc_ty]);
    lw.goto(hb, vec![start, init]);

    lw.cur = Some(hb);
    let (bb, _) = lw.new_block(&[]);
    let cond = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: ity, a: hp[0], b: end });
    lw.branch(cond, (bb, vec![]), (xb, vec![hp[1]]));

    lw.cur = Some(bb);
    let (mut item, mut item_ty) = (hp[0], ity);
    for ((st, _), k) in stages.iter().zip(&ks) {
        let (args, ret) = sig(k);
        match st {
            Stage::Map => {
                if args != [item_ty] {
                    return lw.err(line, "map のクロージャの引数の型が要素の型と合わない");
                }
                item = call(lw, k, vec![item]);
                item_ty = ret;
            }
            Stage::Filter => {
                let Some(p) = Pointee::of(item_ty) else {
                    return lw.err(line, "filter の要素はスカラーだけ段階 1 の対象");
                };
                if args != [Ty::Ref(false, p)] || ret != Ty::Bool {
                    return lw.err(line, "filter のクロージャの型が (&要素) -> bool と合わない");
                }
                let slot = lw.alloca(p);
                lw.emit(Ty::Unit, Op::Store { ptr: slot, val: item });
                let r = lw.ref_value(slot, p, true);
                let keep = call(lw, k, vec![r]);
                let (nb, _) = lw.new_block(&[]);
                lw.branch(keep, (nb, vec![]), (ib, vec![hp[1]]));
                lw.cur = Some(nb);
            }
        }
    }
    let acc2 = match &fk {
        Some(k) => {
            let (args, ret) = sig(k);
            if args != [acc_ty, item_ty] || ret != acc_ty {
                return lw.err(line, "fold のクロージャの型が (累積, 要素) -> 累積 と合わない");
            }
            call(lw, k, vec![hp[1], item])
        }
        None => {
            let one = lw.emit(Ty::Usize, Op::Const(1));
            lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: hp[1], b: one })
        }
    };
    lw.goto(ib, vec![acc2]);

    lw.cur = Some(ib);
    let one = lw.emit(ity, Op::Const(1));
    let next = lw.emit(ity, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: hp[0], b: one });
    lw.goto(hb, vec![next, ip[0]]);

    lw.cur = Some(xb);
    Ok(Some(xp[0]))
}
