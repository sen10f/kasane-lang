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
            // チャネルの端 `Sender<T>` / `Receiver<T>` (`chan_elem` に要素の型がある)。
            let chan = aggs.chan_kind(t).zip(aggs.chan_elem.get(&i).copied());
            if let Some((sender, el)) = chan {
                drop(aggs);
                let inner = elem_src(lw, el)?;
                let n = if sender { "std.sync.Sender" } else { "std.sync.Receiver" };
                return Some(Src::Named(n.into(), vec![inner]));
            }
            let n = &aggs.defs[i as usize].name;
            // `Vec` / `String` のヘッダ (`vec`、要素が所有物なら `vec<#N>`): 配置と解放は同じなので、`String` / `Vec<要素>` に戻す。
            // `&str` と `&[T]` の値 (`fat`): 配置は同じなので `&str`。
            if n == "fat" {
                return Some(Src::Ref(false, Box::new(Src::Prim("str".into()))));
            }
            if n == "vec" {
                return Some(Src::Named("alloc.string.String".into(), vec![Src::Named("std.alloc.System".into(), vec![])]));
            }
            if let Some(k) = n.strip_prefix("vec<#").and_then(|r| r.strip_suffix('>')).and_then(|r| r.parse::<u32>().ok()) {
                drop(aggs);
                let inner = elem_src(lw, Ty::Agg(k))?;
                return Some(Src::Named("alloc.vec.Vec".into(), vec![inner, Src::Named("std.alloc.System".into(), vec![])]));
            }
            (n.contains('.') && !n.contains('<')).then(|| Src::Named(n.clone(), vec![]))
        }
        // 借用のヘッダのアドレス (`&String` / `&Vec<T>`): 8 バイトのポインタ。配置と扱いは同じなので `&String`。
        Ty::Ptr => Some(Src::Ref(false, Box::new(Src::Named("alloc.string.String".into(), vec![Src::Named("std.alloc.System".into(), vec![])])))),
        Ty::Unit => None,
        t if t.is_scalar() => Some(Src::Prim(t.name().to_string())),
        _ => None,
    }
}

pub(super) fn find_def(lw: &Lw, name: &str) -> Option<usize> {
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

/// `x.m(args)` (`x` は `Vec` / スライス) が Kasane の拡張メソッドなら下ろす。該当しなければ `None`。
pub(super) fn ext_call(lw: &mut Lw, e: &V, line: usize, kind: &Kind, h: VId, m: &str, args: &[V], force: bool) -> Option<LR<Option<VId>>> {
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
        // 基本メソッドの Kasane 版は、切り替え (`force`) のときだけ使う。
        let base = crate::interp::KASANE_VEC_BASE.contains(&m);
        match find_def(lw, &format!("alloc.vec.Vec.{}", m)).filter(|_| force || !base) {
            Some(d) => (d, true),
            None => (find_def(lw, &format!("core.slice.{}", m))?, false),
        }
    } else {
        (find_def(lw, &format!("core.slice.{}", m))?, false)
    };
    Some(ext_lower(lw, e, line, di, on_vec, elem, h, args, is_vec))
}

/// `s.m(args)` (`s` は `String` のヘッダのアドレス) の基本メソッドを、`alloc/string.ksnl` の Kasane 版の単相インスタンスで下ろす。
/// 名前が `KASANE_STRING_BASE` に無い、または Kasane 版の定義が無ければ `None` (組み込みの経路へ)。
pub(super) fn string_call(lw: &mut Lw, e: &V, line: usize, h: VId, m: &str, args: &[V]) -> Option<LR<Option<VId>>> {
    if !crate::interp::KASANE_STRING_BASE.contains(&m) {
        return None;
    }
    let di = find_def(lw, &format!("alloc.string.String.{}", m))?;
    let recv = Src::Named("alloc.string.String".into(), vec![Src::Named("std.alloc.System".into(), vec![])]);
    Some(kasane_call(lw, e, line, di, h, recv, args))
}

/// `String.push(c)` (`alloc/string.ksnl` の Kasane 版) を、評価済みの `char` の値 `c` で呼ぶ (`char` の `Display` の追記)。
pub(super) fn string_push_char(lw: &mut Lw, e: &V, line: usize, h: VId, c: VId) -> Option<LR<Option<VId>>> {
    let di = find_def(lw, "alloc.string.String.push")?;
    let recv = Src::Named("alloc.string.String".into(), vec![Src::Named("std.alloc.System".into(), vec![])]);
    let tm = match lw.mono_targs(e, di, &[], 1, super::mono::targs_of(e), Some(recv)) {
        Ok(t) => t,
        Err(x) => return Some(Err(x)),
    };
    let callee = match lw.mono_instance(di, &tm, line) {
        Ok(c) => c,
        Err(x) => return Some(Err(x)),
    };
    lw.callees.insert(callee.clone());
    let rt = match lw.ty_of(e) {
        Ok(t) => t,
        Err(x) => return Some(Err(x)),
    };
    Some(Ok(Some(lw.emit(rt, Op::Call { callee, args: vec![h, c] }))))
}

fn ext_lower(lw: &mut Lw, e: &V, line: usize, di: usize, on_vec: bool, elem: Ty, h: VId, args: &[V], is_vec: bool) -> LR<Option<VId>> {
    let cd = &lw.p.defs[di];
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
    let self_v = if is_vec && !on_vec { coll::hdr_view(lw, h) } else { h };
    kasane_call(lw, e, line, di, self_v, recv, args)
}

/// Kasane の定義 `di` (メソッド) を、受け手 `self_v` と受け手の型 `recv` で呼ぶ。総称の型引数は受け手と引数から推論して単相化する。
pub(super) fn kasane_call(lw: &mut Lw, e: &V, line: usize, di: usize, self_v: VId, recv: Src, args: &[V]) -> LR<Option<VId>> {
    let p = lw.p;
    let cd = &p.defs[di];
    // クロージャを引数に取る (`sort_by(fn ..)`): クロージャごとの単相インスタンス (`generic_call_with`)。
    if coll::has_closure_arg(lw, args) {
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
