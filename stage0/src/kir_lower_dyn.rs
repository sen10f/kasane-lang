//! `Box<T>` と `Box<dyn Tr>` の下ろし (設計_KIR.md 21 章)。
//!
//! `kir_lower` の子モジュールとして `Lw` の非公開の項目を使う。
//!
//! - `Box<T, System>` は所有するヒープの 1 要素へのポインタ (型 `box<..>`、`{ptr}`)。`Box.new` は確保して中身を書き、
//!   移動と解放は所有物を持つ型と同じ規則 (解放のとき中身を先に解放して確保を返す。`kir_lower_coll.rs` の `drop_value`)。
//! - `Box<dyn Tr, System>` はデータの番地と vtable の番号の組 (型 `dynbox<Tr>`、`{ptr, vt}`)。`into_dyn` が (トレイト, 具体型) の
//!   組を登録して番号を振る。KIR には間接呼び出しがないので、`dyn` のメソッド呼び出しは vtable の番号で分岐する補助関数
//!   `dyn.call.<Tr>.<m>` を呼ぶ (閉じた世界の単相化。補助関数は全関数を下ろした後に `synth` が作る)。
//!   データの解放も同じ作りの補助関数 `dyn.drop` が、番号ごとに具体型の中身を解放して確保を返す。

use std::collections::{BTreeMap, BTreeSet};

use crate::check::{items, nf, nlineno as nline, nname, st, strs};
use crate::kir::{size_of, Block, Cc, Func, Op, Pointee, Term, Ty, VId};
use crate::parser::V;
use crate::program::{Def, Program, Target as Tgt};
use crate::types::{from_v, ExprTy, Ty as Src};

use super::agg::{trait_fqn, AggTab, VtEntry};
use super::{coll, Lw, LR};

/// 呼び先の標準ライブラリの関数の名前 (`import` した別名の形 `boxed.Box.new` も FQN にする)。
fn canon(lw: &Lw, segs: &[String]) -> Option<String> {
    let r = lw.p.resolve(segs, &lw.def.file)?;
    if !r.rest.is_empty() {
        return None;
    }
    match r.target {
        Tgt::Native(n) => Some(n),
        Tgt::Sig(di) | Tgt::Fn(di) => Some(lw.p.defs[di].name.clone()),
        _ => None,
    }
}

/// 呼び出しの引数がアロケータ `std.alloc.SYSTEM` か。
fn is_system(a: &V) -> bool {
    nname(a) == "Path" && strs(&nf(a)[0]) == ["std", "alloc", "SYSTEM"]
}

/// 値 `v` が `Box` (またはその参照) なら、`Box` の値を取り出す。参照は読む。
fn unref(lw: &mut Lw, v: VId) -> VId {
    match lw.vty(v) {
        Ty::Ref(_, Pointee::Agg(ai)) => lw.emit(Ty::Agg(ai), Op::Load(v)),
        _ => v,
    }
}

/// `Box` 系の型 (`box<..>` / `dynbox<..>` とその参照) か。参照を外した型を返す。
fn box_kind(lw: &Lw, t: Ty) -> Option<Ty> {
    let t = match t {
        Ty::Ref(_, Pointee::Agg(ai)) => Ty::Agg(ai),
        t => t,
    };
    let a = lw.aggs.borrow();
    (a.box_elem(t).is_some() || a.is_dynbox(t)).then_some(t)
}

/// 局所からのフィールドの経路 `x.f.g` の型 (命令は作らない)。
fn path_ty(lw: &Lw, segs: &[String]) -> Option<Ty> {
    let s = lw.lookup(&segs[0])?;
    let mut t = lw.slots[s].1;
    for n in &segs[1..] {
        let ai = match t {
            Ty::Agg(ai) | Ty::Ref(_, Pointee::Agg(ai)) => ai,
            _ => return None,
        };
        let a = lw.aggs.borrow();
        match &a.defs[ai as usize].kind {
            crate::kir::AggKind::Struct(fs) => t = fs.iter().find(|f| f.name == *n)?.ty,
            _ => return None,
        }
    }
    Some(t)
}

/// `Box.new` / `Box.into_dyn` / `Box.into_inner` の自由関数の形と、`x.f.m(..)` の形の `Box` のメソッド。扱わないなら `None`。
pub(super) fn try_call(lw: &mut Lw, e: &V) -> Option<LR<Option<VId>>> {
    let f = nf(e);
    if nname(&f[0]) != "Path" {
        return None;
    }
    let segs = strs(&nf(&f[0])[0]);
    let args = items(&f[1]).to_vec();
    // `x.f.m(..)` (局所から始まる経路で、型が Box)
    if segs.len() >= 2 && lw.lookup(&segs[0]).is_some() {
        let t = path_ty(lw, &segs[..segs.len() - 1])?;
        let bt = box_kind(lw, t)?;
        let m = segs[segs.len() - 1].clone();
        let consume = m == "into_inner";
        let s = lw.lookup(&segs[0])?;
        let base = if consume && segs.len() == 2 { coll::path_read(lw, s) } else { coll::path_peek(lw, s) };
        let recv = match lw.field_path(base, &segs[1..segs.len() - 1], nline(e)) {
            Ok(v) => v,
            Err(x) => return Some(Err(x)),
        };
        let _ = bt;
        return Some(method(lw, e, recv, &m, &args));
    }
    let name = canon(lw, &segs)?;
    match name.as_str() {
        "alloc.boxed.Box.new" => Some(box_new(lw, e, &args)),
        "alloc.boxed.Box.into_dyn" => Some(into_dyn(lw, e, &args)),
        "alloc.boxed.Box.into_inner" | "alloc.boxed.Box.as_ref" | "alloc.boxed.Box.as_mut" if args.len() == 1 => {
            let m = name.rsplit('.').next().unwrap().to_string();
            let recv = if m == "into_inner" { lw.expr(&args[0]) } else { lw.expr_peek(&args[0]) };
            match recv {
                Ok(Some(v)) => Some(method(lw, e, v, &m, &[])),
                Ok(None) => Some(Ok(None)),
                Err(x) => Some(Err(x)),
            }
        }
        _ => None,
    }
}

/// `e.m(..)` の形で受け手の型が Box のもの。
pub(super) fn try_mcall(lw: &mut Lw, e: &V) -> Option<LR<Option<VId>>> {
    let f = nf(e);
    let rt = lw.ty_of(&f[0]).ok()?;
    box_kind(lw, rt)?;
    let m = st(&f[1]);
    let args = items(&f[2]).to_vec();
    let recv = if m == "into_inner" { lw.expr(&f[0]) } else { lw.expr_peek(&f[0]) };
    match recv {
        Ok(Some(v)) => Some(method(lw, e, v, &m, &args)),
        Ok(None) => Some(Ok(None)),
        Err(x) => Some(Err(x)),
    }
}

/// `Box.new(std.alloc.SYSTEM, v)`: 確保して中身を書く。結果は `Ok(box)` (確保の失敗は起こさない)。
fn box_new(lw: &mut Lw, e: &V, args: &[V]) -> LR<Option<VId>> {
    let line = nline(e);
    if args.len() != 2 || !is_system(&args[0]) {
        return lw.err(line, "Box.new のアロケータは std.alloc.SYSTEM だけ対象");
    }
    let rt = lw.ty_of(e)?;
    let Ty::Res(a, _) = rt else { return lw.err(line, "Box.new の結果の型が Result でない") };
    let Some(elem) = lw.aggs.borrow().box_elem(a.ty()) else {
        return lw.err(line, "Box.new の結果が Box<T> でない (Box<dyn> は into_dyn で作る)");
    };
    let Some(v) = lw.expr(&args[1])? else { return Ok(None) };
    if lw.vty(v) != elem {
        return lw.err(line, "Box.new の中身の型が Box の型と合わない");
    }
    let sz = size_of(&lw.aggs.borrow().defs, elem).max(1);
    let n = lw.emit(Ty::Usize, Op::Const(sz as i128));
    let p = lw.emit(Ty::Ptr, Op::HeapAlloc(n));
    let pt = Pointee::of(elem).expect("Box の中身は参照できる型");
    let r = lw.emit(Ty::Ref(true, pt), Op::Bitcast { src: Ty::Ptr, a: p });
    lw.emit(Ty::Unit, Op::Store { ptr: r, val: v });
    let b = lw.emit(a.ty(), Op::Make { variant: None, fields: vec![p] });
    Ok(Some(lw.emit(rt, Op::Ok(b))))
}

/// `Box.into_dyn(b)`: データの番地はそのまま、(トレイト, 具体型) の vtable の番号を付ける。
fn into_dyn(lw: &mut Lw, e: &V, args: &[V]) -> LR<Option<VId>> {
    let line = nline(e);
    if args.len() != 1 {
        return lw.err(line, "Box.into_dyn の引数は 1 つのはず");
    }
    let dst = lw.src_ty(e)?;
    let src = lw.src_ty(&args[0])?;
    let (Src::Named(_, da), Src::Named(_, sa)) = (&dst, &src) else {
        return lw.err(line, "Box.into_dyn の型が Box でない");
    };
    let (Some(Src::Dyn(trw, _)), Some(conc)) = (da.first(), sa.first()) else {
        return lw.err(line, "Box.into_dyn の変換先が dyn でない");
    };
    let tr = trait_fqn(lw.p, &lw.def.file, trw);
    let concrete_name = match conc {
        Src::Named(n, a) if a.is_empty() => n.clone(),
        Src::Prim(s) => s.clone(),
        other => return lw.err(line, format!("dyn にする型 {} は対象外 (型引数付きなど)", super::show(other))),
    };
    let concrete = lw.conv_t_why(conc).or_else(|w| lw.err(line, format!("dyn にする型の下ろしに失敗: {}", w)))?;
    if Pointee::of(concrete).is_none() {
        return lw.err(line, "dyn にする型は参照できる型だけ対象");
    }
    let dt = lw.conv_t_why(&dst).or_else(|w| lw.err(line, format!("Box<dyn> の型の下ろしに失敗: {}", w)))?;
    let vt = {
        let mut a = lw.aggs.borrow_mut();
        match a.dynr.vts.iter().position(|v| v.tr == tr && v.concrete_name == concrete_name) {
            Some(k) => k,
            None => {
                a.dynr.vts.push(VtEntry { tr, concrete_name, concrete, file: lw.def.file.clone() });
                a.dynr.vts.len() - 1
            }
        }
    };
    let Some(b) = lw.expr(&args[0])? else { return Ok(None) };
    let b = unref(lw, b);
    let p = lw.emit(Ty::Ptr, Op::Field { a: b, off: 0 });
    let k = lw.emit(Ty::Usize, Op::Const(vt as i128));
    Ok(Some(lw.emit(dt, Op::Make { variant: None, fields: vec![p, k] })))
}

/// Box の値 (または参照) `recv` へのメソッド呼び出し。
fn method(lw: &mut Lw, e: &V, recv: VId, m: &str, args: &[V]) -> LR<Option<VId>> {
    let line = nline(e);
    let recv = unref(lw, recv);
    let bt = lw.vty(recv);
    let elem = lw.aggs.borrow().box_elem(bt);
    if let Some(elem) = elem {
        let p = lw.emit(Ty::Ptr, Op::Field { a: recv, off: 0 });
        return match m {
            "as_ref" | "as_mut" => {
                if lw.aggs.borrow().is_vec(elem) {
                    return Ok(Some(p));
                }
                let pt = Pointee::of(elem).expect("Box の中身は参照できる型");
                Ok(Some(lw.emit(Ty::Ref(m == "as_mut", pt), Op::Bitcast { src: Ty::Ptr, a: p })))
            }
            "into_inner" => {
                let v = coll::box_load(lw, p, elem);
                lw.emit(Ty::Unit, Op::HeapFree(p));
                Ok(Some(v))
            }
            _ => lw.err(line, format!("Box のメソッド {} は対象外", m)),
        };
    }
    let Some(tr) = lw.aggs.borrow().dynbox_trait(bt) else {
        return lw.err(line, format!("Box のメソッド {} は対象外", m));
    };
    if matches!(m, "as_ref" | "as_mut" | "into_inner") {
        return lw.err(line, format!("Box<dyn> の {} (&dyn / &mut dyn を作る) は対象外", m));
    }
    let p = lw.emit(Ty::Ptr, Op::Field { a: recv, off: 0 });
    let vt = lw.emit(Ty::Usize, Op::Field { a: recv, off: 8 });
    let mut vs = vec![p, vt];
    let mut held = vec![];
    for a in args {
        match lw.expr(a)? {
            Some(v) => {
                coll::tmp_hold(lw, v);
                held.push(v);
                vs.push(v);
            }
            None => {
                coll::tmp_release(lw, &held);
                return Ok(None);
            }
        }
    }
    coll::tmp_release(lw, &held);
    let rt = lw.ty_of(e)?;
    let pts: Vec<Ty> = vs[2..].iter().map(|v| lw.vty(*v)).collect();
    {
        let mut a = lw.aggs.borrow_mut();
        let sig = (pts, rt);
        match a.dynr.calls.get(&(tr.clone(), m.to_string())) {
            Some(old) if *old != sig => {
                drop(a);
                return lw.err(line, format!("dyn {} の {} の呼び出しで引数・戻り値の型が食い違う", tr, m));
            }
            _ => {
                a.dynr.calls.insert((tr.clone(), m.to_string()), sig);
            }
        }
    }
    let callee = format!("dyn.call.{}.{}", tr, m);
    lw.callees.insert(callee.clone());
    Ok(Some(lw.emit(rt, Op::Call { callee, args: vs })))
}

// ------------------------------------------------ 補助関数 (全関数を下ろした後に作る)

/// 補助関数を 1 つ組み立てる (`Lw` の部品を借りて、手でブロックを置く)。
pub(super) fn build<'a>(
    p: &'a Program,
    tys: &'a BTreeMap<(String, u32), ExprTy>,
    aggs: &'a std::cell::RefCell<AggTab>,
    def: &'a Def,
    params: &[Ty],
    body: impl FnOnce(&mut Lw<'a>, &[VId]) -> Ty,
    fx: impl FnOnce(&BTreeSet<String>) -> (Vec<String>, bool),
    name: &str,
) -> (Func, BTreeSet<String>) {
    let mut lw = Lw::new(p, tys, aggs, def);
    let (b0, pv) = lw.new_block(params);
    debug_assert_eq!(b0, 0);
    lw.cur = Some(0);
    lw.scopes.push(vec![]);
    let ret = body(&mut lw, &pv);
    lw.flush_allocas();
    let (uses, may_panic) = fx(&lw.callees);
    let mut blocks: Vec<Block> = lw
        .blocks
        .into_iter()
        .map(|b| Block { params: b.params, insts: b.insts, term: b.term.unwrap_or(Term::Unreachable) })
        .collect();
    coll::finish(&mut blocks, std::mem::take(&mut lw.coll.allocas));
    let f = Func { fqn: name.to_string(), key: name.to_string(), uses, may_panic, stack: None, ret, link: None, blocks };
    (f, lw.callees)
}

/// 登録された `dyn` の呼び出しと解放の補助関数を `ok` に足す。呼び先が下ろせなかった補助関数は、
/// 既存の「呼び先が未対応」の規則で呼び出し元ごと外れる。
pub(super) fn synth(p: &Program, tys: &BTreeMap<(String, u32), ExprTy>, aggs: &std::cell::RefCell<AggTab>, def: &Def, ok: &mut Vec<(Func, BTreeSet<String>)>) {
    let reg = aggs.borrow().dynr.clone();
    // 呼び出しの補助関数 `dyn.call.<Tr>.<m>(data, vt, args..)`
    for ((tr, m), (pts, ret)) in &reg.calls {
        let entries: Vec<(usize, &VtEntry)> = reg.vts.iter().enumerate().filter(|(_, v)| v.tr == *tr).collect();
        let mut params = vec![Ty::Ptr, Ty::Usize];
        params.extend(pts.iter().copied());
        let name = format!("dyn.call.{}.{}", tr, m);
        let existing: BTreeMap<String, (Vec<String>, bool)> =
            ok.iter().map(|(f, _)| (f.key.clone(), (f.uses.clone(), f.may_panic))).collect();
        let ret = *ret;
        let (f, callees) = build(
            p,
            tys,
            aggs,
            def,
            &params,
            |lw, pv| {
                for (k, ent) in &entries {
                    let mname = format!("{}.{}", ent.concrete_name, m);
                    let self_mut = p
                        .defs
                        .iter()
                        .find(|d| d.name == mname && d.kw == "fn" && !d.params.is_empty())
                        .map(|d| matches!(from_v(&d.params[0].1, &Default::default()), Src::Ref(true, _)))
                        .unwrap_or(true);
                    let kk = lw.emit(Ty::Usize, Op::Const(*k as i128));
                    let c = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: pv[1], b: kk });
                    let (yes, _) = lw.new_block(&[]);
                    let (no, _) = lw.new_block(&[]);
                    lw.branch(c, (yes, vec![]), (no, vec![]));
                    lw.cur = Some(yes);
                    let pt = Pointee::of(ent.concrete).expect("登録のとき確かめた");
                    let r = lw.emit(Ty::Ref(self_mut, pt), Op::Bitcast { src: Ty::Ptr, a: pv[0] });
                    let mut cargs = vec![r];
                    cargs.extend(pv[2..].iter().copied());
                    lw.callees.insert(mname.clone());
                    let res = lw.emit(ret, Op::Call { callee: mname, args: cargs });
                    lw.terminate(Term::Ret(res));
                    lw.cur = Some(no);
                }
                lw.terminate(Term::Unreachable);
                ret
            },
            |cs| {
                let mut uses = BTreeSet::new();
                let mut may = false;
                for c in cs {
                    match existing.get(c) {
                        Some((u, mp)) => {
                            uses.extend(u.iter().cloned());
                            may |= *mp;
                        }
                        None => may = true,
                    }
                }
                (uses.into_iter().collect(), may)
            },
            &name,
        );
        ok.push((f, callees));
    }
    // 解放の補助関数 `dyn.drop(data, vt)`: 具体型の中身を解放してから確保を返す
    if reg.drop_used {
        let (f, callees) = build(
            p,
            tys,
            aggs,
            def,
            &[Ty::Ptr, Ty::Usize],
            |lw, pv| {
                let (join, _) = lw.new_block(&[]);
                for (k, ent) in reg.vts.iter().enumerate() {
                    if !lw.aggs.borrow().owns_heap(ent.concrete) {
                        continue;
                    }
                    let kk = lw.emit(Ty::Usize, Op::Const(k as i128));
                    let c = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: pv[1], b: kk });
                    let (yes, _) = lw.new_block(&[]);
                    let (no, _) = lw.new_block(&[]);
                    lw.branch(c, (yes, vec![]), (no, vec![]));
                    lw.cur = Some(yes);
                    let v = coll::box_load(lw, pv[0], ent.concrete);
                    coll::drop_value(lw, v, ent.concrete);
                    lw.goto(join, vec![]);
                    lw.cur = Some(no);
                }
                lw.goto(join, vec![]);
                lw.cur = Some(join);
                lw.emit(Ty::Unit, Op::HeapFree(pv[0]));
                let u = lw.unit();
                lw.terminate(Term::Ret(u));
                Ty::Unit
            },
            |_| (vec![], false),
            "dyn.drop",
        );
        ok.push((f, callees));
    }
}
