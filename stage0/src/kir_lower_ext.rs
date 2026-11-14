//! KIR の下ろし: スライス・`Vec` の拡張メソッドのうち、本体が Kasane の総称関数 (`spec/stdlib/core/slice.ksnl` の
//! `core.slice.sort[T: Ord]` / `binary_search` / `reverse`、`alloc/vec.ksnl` の `Vec.contains` ほか) の呼び出し。
//!
//! 受け手の要素型 `T` は KIR の型 (`Kind::Slice(Some(elem))` / `Kind::Vec(Some(elem))`) から型表の型に戻して決め、
//! 残りの型引数は引数・戻り値から推論して、呼び出しごとに単相インスタンスにする (`kir_lower_mono.rs`)。
//! 受け手は `&[T]` なら `fat` の値 (`Vec` は `hdr_view`)、`&Vec<T, A>` ならヘッダのアドレス。

use std::collections::HashSet;

use super::coll::{self, Kind};
use super::{Lw, LR};
use crate::check::{is_nil, nlineno as nline};
use crate::kir::{Op, Ty, VId};
use crate::parser::V;
use crate::types::{from_v, Ty as Src};

/// KIR の要素型を型表の型へ戻す (整数・bool・浮動小数・利用者の構造体)。戻せなければ `None`。
fn elem_src(lw: &Lw, t: Ty) -> Option<Src> {
    match t {
        Ty::Agg(i) => {
            let aggs = lw.aggs.borrow();
            if let Some((n, a)) = aggs.generic_args.get(&i) {
                return Some(Src::Named(n.clone(), a.clone()));
            }
            let n = &aggs.defs[i as usize].name;
            (n.contains('.') && !n.contains('<')).then(|| Src::Named(n.clone(), vec![]))
        }
        Ty::Unit | Ty::Ptr => None,
        t if t.is_scalar() => Some(Src::Prim(t.name().to_string())),
        _ => None,
    }
}

fn find_def(lw: &Lw, name: &str) -> Option<usize> {
    lw.p.defs.iter().position(|d| d.kw == "fn" && d.name == name && d.body.is_some() && d.is_method())
}

/// `s.swap(i, j)`: 両方が範囲内なら要素を入れ替えて `Ok(())`、そうでなければ `Err(IndexError { index })` (範囲外の大きい方)。
fn slice_swap(lw: &mut Lw, e: &V, line: usize, elem: Ty, view: VId, args: &[V]) -> LR<Option<VId>> {
    use crate::kir::{Aop, Cc, Mode, Pay};
    if args.len() != 2 {
        return lw.err(line, "swap の引数は 2 つのはず");
    }
    let Some(sz) = coll::elem_size(lw, elem) else {
        return lw.err(line, "swap の要素の型が段階 1 の対象外");
    };
    let rt = lw.ty_of(e)?;
    let Ty::Res(Pay::Unit, Pay::Agg(ei)) = rt else {
        return lw.err(line, "swap の結果の型が Result<(), IndexError> でない");
    };
    let Some(i) = lw.expr(&args[0])? else { return Ok(None) };
    let Some(j) = lw.expr(&args[1])? else { return Ok(None) };
    if lw.vty(i) != Ty::Usize || lw.vty(j) != Ty::Usize {
        return lw.err(line, "swap の添字が usize でない");
    }
    let base = lw.emit(Ty::Ptr, Op::Field { a: view, off: 0 });
    let len = lw.emit(Ty::Usize, Op::Field { a: view, off: 8 });
    let ci = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: i, b: len });
    let cj = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: j, b: len });
    let both = lw.emit(Ty::Bool, Op::Bool(crate::kir::BoolOp::And, ci, cj));
    let (yes, _) = lw.new_block(&[]);
    let (no, _) = lw.new_block(&[]);
    let (join, jp) = lw.new_block(&[rt]);
    lw.branch(both, (yes, vec![]), (no, vec![]));
    lw.cur = Some(yes);
    let addr = |lw: &mut Lw, k: VId| {
        let off = if sz == 1 { k } else {
            let c = lw.emit(Ty::Usize, Op::Const(sz as i128));
            lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: k, b: c })
        };
        lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off })
    };
    let (pi, pj) = (addr(lw, i), addr(lw, j));
    let xi = coll::load_elem(lw, pi, elem);
    let xj = coll::load_elem(lw, pj, elem);
    coll::store_elem(lw, pi, elem, xj);
    coll::store_elem(lw, pj, elem, xi);
    let u = lw.unit();
    let okv = lw.emit(rt, Op::Ok(u));
    lw.goto(join, vec![okv]);
    lw.cur = Some(no);
    let big = lw.emit(Ty::Usize, Op::MinMax { max: true, a: i, b: j });
    let ev = lw.emit(Ty::Agg(ei), Op::Make { variant: None, fields: vec![big] });
    let errv = lw.emit(rt, Op::Err(ev));
    lw.goto(join, vec![errv]);
    lw.cur = Some(join);
    Ok(Some(jp[0]))
}

/// `v.reserve(n)`: `len + n` が容量を超えるなら `max(cap * 2 + 4, len + n)` 個分に広げる。結果は `Ok(())` (確保の失敗は起きない)。
fn vec_reserve(lw: &mut Lw, e: &V, line: usize, elem: Ty, h: VId, args: &[V]) -> LR<Option<VId>> {
    use crate::kir::{Aop, Cc, Mode, Pay};
    if args.len() != 1 {
        return lw.err(line, "reserve の引数は 1 つのはず");
    }
    let Some(sz) = coll::elem_size(lw, elem) else {
        return lw.err(line, "reserve の要素の型が段階 1 の対象外");
    };
    let rt = lw.ty_of(e)?;
    if !matches!(rt, Ty::Res(Pay::Unit, _)) {
        return lw.err(line, "reserve の結果の型が Result<(), AllocError> でない");
    }
    let Some(n) = lw.expr(&args[0])? else { return Ok(None) };
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let cap = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 });
    let need = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: len, b: n });
    let over = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::Usize, a: need, b: cap });
    let (grow, _) = lw.new_block(&[]);
    let (done, _) = lw.new_block(&[]);
    lw.branch(over, (grow, vec![]), (done, vec![]));
    lw.cur = Some(grow);
    let dbl = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: cap, b: cap });
    let four = lw.emit(Ty::Usize, Op::Const(4));
    let c2 = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: dbl, b: four });
    let nc = lw.emit(Ty::Usize, Op::MinMax { max: true, a: c2, b: need });
    let bytes = if sz == 1 { nc } else {
        let k = lw.emit(Ty::Usize, Op::Const(sz as i128));
        lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: nc, b: k })
    };
    let old = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let np = lw.emit(Ty::Ptr, Op::HeapRealloc { p: old, size: bytes });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: h, off: 0, v: np });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 16, v: nc });
    lw.goto(done, vec![]);
    lw.cur = Some(done);
    let u = lw.unit();
    Ok(Some(lw.emit(rt, Op::Ok(u))))
}

/// `x.m(args)` (`x` は `Vec` / スライス) が Kasane の拡張メソッドなら下ろす。該当しなければ `None`。
pub(super) fn ext_call(lw: &mut Lw, e: &V, line: usize, kind: &Kind, h: VId, m: &str, args: &[V]) -> Option<LR<Option<VId>>> {
    if m == "reserve" {
        if let Kind::Vec(Some(t)) = kind {
            return Some(vec_reserve(lw, e, line, *t, h, args));
        }
    }
    if m == "swap" {
        if let Kind::Vec(Some(t)) | Kind::Slice(Some(t)) = kind {
            let view = if matches!(kind, Kind::Vec(_)) { coll::hdr_view(lw, h) } else { h };
            return Some(slice_swap(lw, e, line, *t, view, args));
        }
    }
    let (elem, is_vec) = match kind {
        Kind::Vec(Some(t)) => (*t, true),
        Kind::Slice(Some(t)) => (*t, false),
        _ => return None,
    };
    let (di, on_vec) = if is_vec {
        match find_def(lw, &format!("alloc.vec.Vec.{}", m)) {
            Some(d) => (d, true),
            None => (find_def(lw, &format!("core.slice.{}", m))?, false),
        }
    } else {
        (find_def(lw, &format!("core.slice.{}", m))?, false)
    };
    Some(ext_lower(lw, e, line, di, on_vec, elem, h, args, is_vec))
}

fn ext_lower(lw: &mut Lw, e: &V, line: usize, di: usize, on_vec: bool, elem: Ty, h: VId, args: &[V], is_vec: bool) -> LR<Option<VId>> {
    let p = lw.p;
    let cd = &p.defs[di];
    if cd.params.len() != args.len() + 1 {
        return lw.err(line, format!("メソッド {} の引数の個数が違う", cd.name));
    }
    let Some(es) = elem_src(lw, elem) else {
        return lw.err(line, format!("メソッド {} の要素の型が型表に戻せない", cd.name));
    };
    let recv = if on_vec {
        Src::Named("alloc.vec.Vec".into(), vec![es, Src::Named("std.alloc.System".into(), vec![])])
    } else {
        Src::Slice(Box::new(es))
    };
    // クロージャを引数に取る (`sort_by(fn ..)`): クロージャごとの単相インスタンス (`generic_call_with`)。
    if coll::has_closure_arg(lw, args) {
        let self_v = if is_vec && !on_vec { coll::hdr_view(lw, h) } else { h };
        let rs = Src::Ref(true, Box::new(recv));
        return coll::generic_call_with(lw, e, di, Some((self_v, rs)), args);
    }
    let tm = lw.mono_targs(e, di, args, 1, super::mono::targs_of(e), Some(recv))?;
    let callee = if cd.generics.iter().any(|g| g.0 != "life") { lw.mono_instance(di, &tm, line)? } else { cd.name.clone() };
    let gn = super::mono::tnames(cd);
    let sub = |t: Src| super::mono::subst_ty(&t, &tm);
    let mut pts = vec![];
    for (_, t) in &cd.params {
        match coll::sig_ty(lw, &sub(from_v(t, &gn))) {
            Some(x) => pts.push(x),
            None => return lw.err(line, format!("メソッド {} の引数の型は段階 1 の対象外", cd.name)),
        }
    }
    let ret = if is_nil(&cd.ret) {
        Ty::Unit
    } else {
        match coll::sig_ty(lw, &sub(from_v(&cd.ret, &gn))) {
            Some(x) => x,
            None => return lw.err(line, format!("メソッド {} の戻り値の型は段階 1 の対象外", cd.name)),
        }
    };
    let self_v = if is_vec && !on_vec { coll::hdr_view(lw, h) } else { h };
    let mut vs = vec![self_v];
    for a in args {
        match lw.expr(a)? {
            Some(v) => {
                coll::tmp_hold(lw, v);
                vs.push(v)
            }
            None => {
                coll::tmp_release(lw, &vs[1..]);
                return Ok(None);
            }
        }
    }
    coll::tmp_release(lw, &vs[1..]);
    let vs = lw.coerce_args(vs, &pts);
    for (v, pt) in vs.iter().zip(pts.iter()) {
        if lw.vty(*v) != *pt {
            return lw.err(line, format!("メソッド {} の引数の型が合わない", cd.name));
        }
    }
    lw.callees.insert(callee.clone());
    let _ = (nline(e), HashSet::<u8>::new());
    Ok(Some(lw.emit(ret, Op::Call { callee, args: vs })))
}
