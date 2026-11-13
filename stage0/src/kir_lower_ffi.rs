//! `unsafe` ブロック、`extern` の呼び出し、`core.ptr.*` の下ろし (段階 1)。
//!
//! `kir_lower.rs` の子モジュール (`#[path]` で読み込む) なので、`Lw` の非公開の項目を使う.
//!
//! - `unsafe "名" "理由" { .. }`: 中身のブロックをそのまま下ろす.
//! - `core.ptr.read/write/add/null_mut/is_null/from_mut/from_ref/cast`: KIR の
//!   `MemLoad` / `MemStore` / `PtrAdd` / `Const(0)` / `Bitcast` (ref<->ptr) を使う.
//! - `extern` の呼び出し: `Op::Call` で表し、関数は `Func.link` にリンク名を持つ.
//!   LLVM 側で `declare` して C の呼び出し規約で呼ぶ。参照実行器は `ffi::call_generic`
//!   でホストの C 関数を呼ぶ.

use std::collections::HashSet;

use crate::check::{is_nil, items, nlineno as nline, strs};
use crate::kir::{self, Aop, Block, Func, Mode, Op, Pointee, Term, Ty, VId};
use crate::parser::V;
use crate::program::{Def, Program, Target as Tgt};
use crate::types::{from_v, Ty as Src};

use super::{agg, Lw, LR, show};

/// `core.ptr.*` と `extern` の呼び出し。`call` から `builtin_call` の後に呼ばれる。
/// 対象外なら `None`。
pub(super) fn ffi_call(lw: &mut Lw, e: &V) -> Option<LR<Option<VId>>> {
    let f = crate::check::nf(e);
    let segs = strs(&crate::check::nf(&f[0])[0]);
    if segs.len() >= 3 && segs[0] == "core" && segs[1] == "ptr" {
        return Some(ptr_call(lw, e, &segs[2], f.get(2).cloned().unwrap_or(crate::parser::V::Nil)));
    }
    // `extern` 宣言の呼び出し。
    let line = nline(e);
    let resolved = lw.p.resolve(&segs, &lw.def.file);
    let Some(target) = resolved.map(|r| r.target) else { return None };
    let Tgt::Sig(di) = target else { return None };
    let d = &lw.p.defs[di];
    if d.kw != "extern" {
        return None;
    }
    Some(extern_call(lw, e, d, line))
}

/// `unsafe "名" "理由" { .. }`。中身のブロックをそのまま下ろす。
pub(super) fn unsafe_block(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    let f = crate::check::nf(e);
    // `Unsafe(name, reason, block)`: f[2] はブロック。
    lw.expr(&f[2])
}

// ---------------------------------------------------------------- core.ptr.*

fn ptr_call(lw: &mut Lw, e: &V, name: &str, targs: V) -> LR<Option<VId>> {
    let f = crate::check::nf(e);
    let line = nline(e);
    let args = items(&f[1]);
    match name {
        "null_mut" => {
            if !args.is_empty() {
                return lw.err(line, "core.ptr.null_mut は引数を取らない");
            }
            Ok(Some(lw.emit(Ty::Ptr, Op::Const(0))))
        }
        "is_null" => {
            if args.len() != 1 {
                return lw.err(line, "core.ptr.is_null は 1 個の引数を取る");
            }
            let Some(p) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(p) != Ty::Ptr {
                return lw.err(line, "core.ptr.is_null の引数はポインタのはず");
            }
            let zero = lw.emit(Ty::Ptr, Op::Const(0));
            Ok(Some(lw.emit(Ty::Bool, Op::Cmp { cc: kir::Cc::Eq, ty: Ty::Ptr, a: p, b: zero })))
        }
        "read" => {
            if args.len() != 1 {
                return lw.err(line, "core.ptr.read は 1 個の引数を取る");
            }
            let Some(p) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(p) != Ty::Ptr {
                return lw.err(line, "core.ptr.read の引数はポインタのはず");
            }
            let ty = lw.ty_of(e)?;
            if !crate::kir::memable(ty) {
                return lw.err(line, format!("core.ptr.read の型 {} は未対応", ty.name()));
            }
            Ok(Some(lw.emit(ty, Op::MemLoad { p, off: 0 })))
        }
        "write" => {
            if args.len() != 2 {
                return lw.err(line, "core.ptr.write は 2 個の引数を取る");
            }
            let Some(p) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(p) != Ty::Ptr {
                return lw.err(line, "core.ptr.write の第 1 引数はポインタのはず");
            }
            let Some(v) = lw.expr(&args[1])? else { return Ok(None) };
            let ty = lw.vty(v);
            if !crate::kir::memable(ty) {
                return lw.err(line, format!("core.ptr.write の型 {} は未対応", ty.name()));
            }
            Ok(Some(lw.emit(Ty::Unit, Op::MemStore { ty, p, off: 0, v })))
        }
        "add" => {
            if args.len() != 2 {
                return lw.err(line, "core.ptr.add は 2 個の引数を取る");
            }
            let Some(p) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(p) != Ty::Ptr {
                return lw.err(line, "core.ptr.add の第 1 引数はポインタのはず");
            }
            let Some(n) = lw.expr(&args[1])? else { return Ok(None) };
            if lw.vty(n) != Ty::Usize {
                return lw.err(line, "core.ptr.add の第 2 引数は usize のはず");
            }
            let elem = ptr_elem_ty(lw, &targs, line)?;
            let sz = kir::size_of(&lw.aggs.borrow().defs, elem) as u64;
            let szv = lw.emit(Ty::Usize, Op::Const(sz as i128));
            let off = lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: n, b: szv });
            Ok(Some(lw.emit(Ty::Ptr, Op::PtrAdd { p, off })))
        }
        "from_mut" | "from_ref" => {
            if args.len() != 1 {
                return lw.err(line, "core.ptr.from_mut/from_ref は 1 個の引数を取る");
            }
            let Some(r) = lw.expr(&args[0])? else { return Ok(None) };
            let rt = lw.vty(r);
            // `&mut T` / `&T` (スカラー・集成体への参照): ref → ptr のビットキャスト。
            if let Some(p) = pointee_of(rt) {
                let m = matches!(rt, Ty::Ref(true, _));
                return Ok(Some(lw.emit(Ty::Ptr, Op::Bitcast { src: Ty::Ref(m, p), a: r })));
            }
            // `{ptr, len}` の fat ポインタ (スライス参照): 先頭の ptr フィールドを取る。
            if let Ty::Agg(ai) = rt {
                let aggs = &lw.aggs.borrow().defs;
                if aggs[ai as usize].name == "fat" {
                    return Ok(Some(lw.emit(Ty::Ptr, Op::Field { a: r, off: 0 })));
                }
            }
            return lw.err(line, "core.ptr.from_mut/from_ref の引数は参照のはず");
        }
        "cast" | "cast_const" => {
            if args.len() != 1 {
                return lw.err(line, "core.ptr.cast は 1 個の引数を取る");
            }
            let Some(p) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(p) != Ty::Ptr {
                return lw.err(line, "core.ptr.cast の引数はポインタのはず");
            }
            Ok(Some(p))
        }
        _ => lw.err(line, format!("core.ptr.{} は段階 1 の対象外", name)),
    }
}

fn pointee_of(t: Ty) -> Option<Pointee> {
    match t {
        Ty::Ref(_, Pointee::Agg(i)) => Some(Pointee::Agg(i)),
        Ty::Ref(_, Pointee::I8) => Some(Pointee::I8),
        Ty::Ref(_, Pointee::I16) => Some(Pointee::I16),
        Ty::Ref(_, Pointee::I32) => Some(Pointee::I32),
        Ty::Ref(_, Pointee::I64) => Some(Pointee::I64),
        Ty::Ref(_, Pointee::I128) => Some(Pointee::I128),
        Ty::Ref(_, Pointee::Isize) => Some(Pointee::Isize),
        Ty::Ref(_, Pointee::U8) => Some(Pointee::U8),
        Ty::Ref(_, Pointee::U16) => Some(Pointee::U16),
        Ty::Ref(_, Pointee::U32) => Some(Pointee::U32),
        Ty::Ref(_, Pointee::U64) => Some(Pointee::U64),
        Ty::Ref(_, Pointee::U128) => Some(Pointee::U128),
        Ty::Ref(_, Pointee::Usize) => Some(Pointee::Usize),
        Ty::Ref(_, Pointee::F32) => Some(Pointee::F32),
        Ty::Ref(_, Pointee::F64) => Some(Pointee::F64),
        Ty::Ref(_, Pointee::Bool) => Some(Pointee::Bool),
        _ => None,
    }
}

/// `core.ptr.*` の型実引数から要素型を取る。
fn ptr_elem_ty(lw: &Lw, targs: &V, line: usize) -> LR<Ty> {
    let ts = items(targs);
    let Some(first) = ts.first() else {
        return lw.err(line, "core.ptr.* に型実引数が必要");
    };
    let st = from_v(first, &HashSet::new());
    lw.conv_t(&st)
        .ok_or_else(|| format!("core.ptr.* の型 {} は未対応", show(&st)))
}

// ---------------------------------------------------------------- extern

fn extern_call(lw: &mut Lw, e: &V, d: &Def, line: usize) -> LR<Option<VId>> {
    let f = crate::check::nf(e);
    let args = items(&f[1]);
    let gn = HashSet::new();
    let mut pts: Vec<Ty> = vec![];
    for (_, t) in &d.params {
        let st = from_v(t, &gn);
        pts.push(src_to_kir(lw, &st, line)?);
    }
    let ret = if is_nil(&d.ret) {
        Ty::Unit
    } else {
        let st = from_v(&d.ret, &gn);
        src_to_kir(lw, &st, line)?
    };
    if pts.len() != args.len() {
        return lw.err(line, format!("extern {} の引数の個数が違う", d.name));
    }
    let mut vs = vec![];
    for a in args {
        match lw.expr(a)? {
            Some(v) => vs.push(v),
            None => return Ok(None),
        }
    }
    for (v, pt) in vs.iter().zip(pts.iter()) {
        if lw.vty(*v) != *pt {
            return lw.err(line, format!("extern {} の引数の型が合わない", d.name));
        }
    }
    lw.callees.insert(d.name.clone());
    Ok(Some(lw.emit(ret, Op::Call { callee: d.name.clone(), args: vs })))
}

/// 推論済みの型を段階 1 の KIR 型にする (`extern` のシグネチャ用。スカラー・ptr)。
fn src_to_kir(lw: &Lw, t: &Src, line: usize) -> LR<Ty> {
    if let Some(x) = lw.conv_t(t) {
        return Ok(x);
    }
    match t {
        Src::Ptr(_, inner) => match &**inner {
            Src::Prim(s) => Ty::parse(s)
                .and_then(|t| kir::Pointee::of(t))
                .map(|_| Ty::Ptr)
                .ok_or_else(|| format!("ポインタ型 {} は未対応", show(t))),
            _ => lw.err(line, format!("ポインタ型 {} は未対応", show(t))),
        },
        _ => lw.err(line, format!("型 {} は段階 1 の対象外", show(t))),
    }
}

/// `extern` 定義を KIR の関数宣言 (本体なし) にする。`lower_defs` から呼ばれる。
pub(super) fn extern_func(p: &Program, d: &Def, aggs: &std::cell::RefCell<agg::AggTab>) -> LR<Func> {
    let gn = HashSet::new();
    let mut params = vec![];
    for (_, t) in &d.params {
        let st = from_v(t, &gn);
        params.push(src_to_kir_extern(aggs, p, &d.file, &st)?);
    }
    let ret = if is_nil(&d.ret) {
        Ty::Unit
    } else {
        let st = from_v(&d.ret, &gn);
        src_to_kir_extern(aggs, p, &d.file, &st)?
    };
    let uses: Vec<String> = d.uses.clone();
    // C 関数は Kasane の panic を起こさないので `panics never` とする。
    let may_panic = false;
    let bparams: Vec<(VId, Ty)> = params.iter().enumerate().map(|(j, t)| (j as VId, *t)).collect();
    Ok(Func {
        fqn: d.name.clone(),
        key: d.name.clone(),
        uses,
        may_panic,
        stack: d.stack,
        ret,
        link: Some(d.link.clone()),
        blocks: vec![Block { params: bparams, insts: vec![], term: Term::Unreachable }],
    })
}

/// `extern` の引数・戻り値の型変換 (スカラー・ptr・集成体へのptr)。
fn src_to_kir_extern(aggs: &std::cell::RefCell<agg::AggTab>, p: &Program, file: &str, t: &Src) -> LR<Ty> {
    if let Some(x) = conv_src(t) {
        return Ok(x);
    }
    match t {
        Src::Ptr(_, inner) => match &**inner {
            Src::Prim(s) => Ty::parse(s)
                .and_then(|t| kir::Pointee::of(t))
                .map(|_| Ty::Ptr)
                .ok_or_else(|| format!("ポインタ型 {} は未対応", show(t))),
            Src::Named(n, _) => {
                let segs: Vec<String> = n.split('.').map(String::from).collect();
                match p.resolve(&segs, file).map(|r| r.target) {
                    Some(Tgt::Type(_)) => {
                        let ty = aggs.borrow_mut().lower_ty(p, file, &**inner).map_err(|e| format!("{}", e))?;
                        if matches!(ty, Ty::Agg(_)) {
                            Ok(Ty::Ptr)
                        } else {
                            Err(format!("ポインタ型 {} は未対応", show(t)))
                        }
                    }
                    _ => Err(format!("ポインタ型 {} は未対応", show(t))),
                }
            }
            _ => Err(format!("ポインタ型 {} は未対応", show(t))),
        },
        _ => Err(format!("extern の型 {} は未対応", show(t))),
    }
}

fn conv_src(t: &Src) -> Option<Ty> {
    match t {
        Src::Prim(s) => Ty::parse(s).filter(|t| *t != Ty::Unit),
        Src::Unit => Some(Ty::Unit),
        Src::Ptr(_, inner) => match &**inner {
            Src::Prim(s) => Ty::parse(s).and_then(|t| kir::Pointee::of(t)).map(|_| Ty::Ptr),
            _ => None,
        },
        _ => None,
    }
}
