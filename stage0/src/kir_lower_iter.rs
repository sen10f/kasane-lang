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
    /// `take(n)`: 通した個数が n に達したら、元を引かずに終わる。
    Take,
    /// `enumerate`: (通した個数, 要素) の組にする。個数は take と同じ受け渡しの数。
    Enumerate,
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
    /// 利用者の反復子 (`core.iter.Iterator` を実装した型の式。`next` の定義の番号を持つ)。
    User(&'a V, usize),
    /// `X.iter()` (局所の Vec・スライス、または `&[T]` の式。要素は `&T` で渡す)。
    Slice(&'a V),
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
    let iter_form = match nname(it) {
        "MCall" => crate::check::st(&nf(it)[1]) == "iter" && items(&nf(it)[2]).is_empty(),
        "Call" => nname(&nf(it)[0]) == "Path" && items(&nf(it)[1]).is_empty() && {
            let segs = strs(&nf(&nf(it)[0])[0]);
            segs.len() == 2 && segs[1] == "iter"
        },
        _ => false,
    };
    if iter_form {
        return Some(Src::Slice(it));
    }
    if let Some(a) = iter_call(it, "enumerate") {
        if a.len() != 1 {
            return None;
        }
        let r = parse_pipe(lw, &a[0], stages)?;
        stages.push((Stage::Enumerate, it));
        return Some(r);
    }
    if nname(it) == "Path" {
        let segs = strs(&nf(it)[0]);
        if segs.len() == 1 {
            if let Some(super::coll::Kind::Range { start, end, ty }) = lw.lookup(&segs[0]).and_then(|s| lw.coll.kinds.get(&s)) {
                return Some(Src::Vals(*start, *end, *ty));
            }
        }
    }
    // 利用者の反復子の値 (`t.Upto { .. }` や局所の名前)。`for` と同じく `Iterator` を実装する型の `&mut self` の `next` を回す。
    if let Some(di) = user_next(lw, it) {
        return Some(Src::User(it, di));
    }
    for (name, st) in [("map", Stage::Map), ("filter", Stage::Filter), ("take", Stage::Take)] {
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

/// `it` の型が、`&mut self` の `next` を持つ利用者の反復子なら、その定義の番号 (標準の反復子と `std.*` は除く)。
fn user_next(lw: &Lw, it: &V) -> Option<usize> {
    let Ok(super::Src::Named(base, _)) = lw.src_ty(it) else { return None };
    if base == "Option" || base == "Result" || base.starts_with("core.") || base.starts_with("std.") || base.starts_with("alloc.") {
        return None;
    }
    let mut segs: Vec<String> = base.split('.').map(String::from).collect();
    segs.push("next".to_string());
    match lw.p.resolve(&segs, &lw.def.file).map(|r| r.target) {
        Some(crate::program::Target::Fn(di)) if lw.p.defs[di].is_method() && lw.method_takes_mut(di) => Some(di),
        _ => None,
    }
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
        return lw.err(line, "反復子の元は core.iter.range(a, b)・利用者の反復子と map / filter / take の連なりだけ段階 1 の対象 (Vec の iter は局所の Vec・スライスの `.iter()` だけ)");
    };
    let mut hidden = String::new();
    let user_di = if let Src::User(_, di) = &src { Some(*di) } else { None };
    let user_it: Option<&V> = if let Src::User(it, _) = &src { Some(*it) } else { None };
    let mut slice_src: Option<(super::coll::Seq, Ty)> = None;
    let (start, end, ity) = match src {
        Src::Slice(it) => {
            let Some((seq, elem)) = super::coll::iter_source(lw, it)? else {
                return lw.err(line, "iter() の元は要素がスカラーか所有物を持たない集成体の、局所の Vec・スライスだけ段階 1 の対象");
            };
            let (len, base) = seq.len_and_base(lw, None);
            let zero = lw.emit(Ty::Usize, Op::Const(0));
            slice_src = Some((super::coll::Seq::Ptr(base, 0), elem));
            (zero, len, Ty::Usize)
        }
        Src::Exprs(a, b) => {
            let Some(start) = lw.expr(a)? else { return Ok(None) };
            let Some(end) = lw.expr(b)? else { return Ok(None) };
            (start, end, lw.ty_of(a)?)
        }
        Src::Vals(start, end, ty) => (start, end, ty),
        // 利用者の反復子: 値を隠しの局所 (スタックスロット) に置く。添字の代わりの数は使わない (0 のまま)。
        Src::User(it, _) => {
            let Some(itv) = lw.expr(it)? else { return Ok(None) };
            let ity = lw.vty(itv);
            if super::coll::owns_heap_pub(lw, ity) {
                return lw.err(line, "所有物を持つ利用者の反復子の連なりは段階 1 の対象外");
            }
            hidden = format!("#pipe{}", lw.slots.len());
            lw.mark_addr(&hidden);
            lw.bind_let(&hidden, itv, ity);
            let z = lw.emit(Ty::Usize, Op::Const(0));
            (z, z, Ty::Usize)
        }
    };
    if !ity.is_int() || lw.vty(end) != ity {
        return lw.err(line, "core.iter.range の範囲は同じ整数の型だけ段階 1 の対象");
    }
    let mut ks = vec![];
    // take の個数 (ループの前に 1 度だけ評価する)。クロージャの段は `ks` に、take の段は `ns` に置く。
    let mut ns: Vec<VId> = vec![];
    for (st, c) in &stages {
        if *st == Stage::Enumerate {
            continue;
        }
        if *st == Stage::Take {
            let Some(n) = lw.expr(c)? else { return Ok(None) };
            if lw.vty(n) != Ty::Usize {
                return lw.err(line, "take の個数は usize だけ段階 1 の対象");
            }
            ns.push(n);
            continue;
        }
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
    if !(acc_ty.is_int() || acc_ty.is_float() || acc_ty == Ty::Bool || (matches!(acc_ty, Ty::Agg(_)) && !super::coll::owns_heap_pub(lw, acc_ty))) {
        return lw.err(line, "fold の累積はスカラーか所有物を持たない集成体だけ段階 1 の対象");
    }

    // 個数の受け渡し: take と enumerate が 1 つずつ持つ (段の順)。take の個数の値 `ns` はその部分列。
    let nt = stages.iter().filter(|(st, _)| matches!(st, Stage::Take | Stage::Enumerate)).count();
    let mut ib_tys = vec![acc_ty];
    ib_tys.extend(std::iter::repeat(Ty::Usize).take(nt));
    let mut hb_tys = vec![ity];
    hb_tys.extend(ib_tys.iter().copied());
    let (xb, xp) = lw.new_block(&[acc_ty]);
    let (ib, ip) = lw.new_block(&ib_tys);
    let (hb, hp) = lw.new_block(&hb_tys);
    let mut entry = vec![start, init];
    for _ in 0..nt {
        entry.push(lw.emit(Ty::Usize, Op::Const(0)));
    }
    lw.goto(hb, entry);

    lw.cur = Some(hb);
    // hp = [i, acc, taken_0, ..]。通した個数が n に達した take があれば、元を引かずに終わる。
    {
        let (mut c, mut k) = (0usize, 0usize);
        for (st, _) in stages.iter() {
            match st {
                Stage::Take => {
                    let (nb, _) = lw.new_block(&[]);
                    let more = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: hp[2 + c], b: ns[k] });
                    lw.branch(more, (nb, vec![]), (xb, vec![hp[1]]));
                    lw.cur = Some(nb);
                    c += 1;
                    k += 1;
                }
                Stage::Enumerate => c += 1,
                _ => {}
            }
        }
    }
    let (bb, _) = lw.new_block(&[]);
    let mut user_item: Option<(VId, Ty)> = None;
    let cond = if let (Some(di), Some(it)) = (user_di, user_it) {
        // `next(&mut it)` の結果が `Some` なら続ける。
        let Some(o) = lw.call_method(it, di, super::method_lower::Place::Path(vec![hidden.clone()]), &[])? else { return Ok(None) };
        let Ty::Opt(pay) = lw.vty(o) else {
            return lw.err(line, "反復子の next は Option を返す段階 1 の対象だけ");
        };
        let pt = pay.ty();
        if !(pt.is_int() || pt.is_float() || pt == Ty::Bool) {
            return lw.err(line, "利用者の反復子の要素はスカラーだけ段階 1 の対象");
        }
        let tag = lw.emit(Ty::Bool, Op::Tag(o));
        let item = lw.emit(pt, Op::Payload(o));
        user_item = Some((item, pt));
        tag
    } else {
        lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: ity, a: hp[0], b: end })
    };
    lw.branch(cond, (bb, vec![]), (xb, vec![hp[1]]));

    lw.cur = Some(bb);
    let (mut item, mut item_ty) = if let Some((seq, elem)) = slice_src {
        let super::coll::Seq::Ptr(base, _) = seq else { unreachable!() };
        let sz = super::coll::elem_size(lw, elem).expect("要素の大きさ");
        let off = if sz == 1 {
            hp[0]
        } else {
            let k = lw.emit(Ty::Usize, Op::Const(sz as i128));
            lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: hp[0], b: k })
        };
        let addr = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off });
        let Some(pt) = Pointee::of(elem) else { return lw.err(line, "iter() の要素の型は段階 1 の対象外") };
        let rt = Ty::Ref(false, pt);
        (lw.emit(rt, Op::Bitcast { src: Ty::Ptr, a: addr }), rt)
    } else {
        user_item.unwrap_or((hp[0], ity))
    };
    let mut taken: Vec<VId> = hp[2..].to_vec();
    let (mut ki, mut ti) = (0usize, 0usize);
    for (st, _) in stages.iter() {
        if *st == Stage::Enumerate {
            let idx = taken[ti];
            let one = lw.emit(Ty::Usize, Op::Const(1));
            taken[ti] = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: taken[ti], b: one });
            ti += 1;
            let tt = lw.aggs.borrow_mut().intern_tuple(vec![Ty::Usize, item_ty]);
            let Ok(tt) = tt else { return lw.err(line, "enumerate の組の型を作れない") };
            item = lw.emit(tt, Op::Make { variant: None, fields: vec![idx, item] });
            item_ty = tt;
            continue;
        }
        if *st == Stage::Take {
            let one = lw.emit(Ty::Usize, Op::Const(1));
            taken[ti] = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: taken[ti], b: one });
            ti += 1;
            continue;
        }
        let k = &ks[ki];
        ki += 1;
        let (args, ret) = sig(k);
        match st {
            Stage::Map => {
                if args != [item_ty] {
                    return lw.err(line, "map のクロージャの引数の型が要素の型と合わない");
                }
                item = call(lw, k, vec![item]);
                item_ty = ret;
            }
            Stage::Take | Stage::Enumerate => unreachable!("take / enumerate は上で処理済み"),
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
                let mut skip = vec![hp[1]];
                skip.extend(taken.iter().copied());
                lw.branch(keep, (nb, vec![]), (ib, skip));
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
    let mut back = vec![acc2];
    back.extend(taken.iter().copied());
    lw.goto(ib, back);

    lw.cur = Some(ib);
    let one = lw.emit(ity, Op::Const(1));
    let next = lw.emit(ity, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: hp[0], b: one });
    let mut again = vec![next];
    again.extend(ip.iter().copied());
    lw.goto(hb, again);

    lw.cur = Some(xb);
    Ok(Some(xp[0]))
}
