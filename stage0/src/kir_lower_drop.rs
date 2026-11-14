//! KIR の下ろし: `derive [Drop]` の再帰する型の解放 (仕様 4.3 / 5.3)。
//!
//! 型 `T` ごとに補助関数 `drop.<T の FQN>(p: ptr, skip: ptr)` を作る。`p` は `T` の値を置いた確保 (`Box<T>` の確保、または
//! 値を一時に置いたスロット)、`skip` は解放しない番地 (箱なら `null`、スロットなら `p`)。再帰も、確保も、失敗もない:
//!
//! - 作業リストは、中身を読み終えた (解体済みの) 親の確保そのものに置く。作業の枠は `[次の枠, 残りの数 n, 子の番地 × n]`
//!   (`8 + 8 + 8n` バイト)。枠にする親は子の箱を 2 つ以上持つ (k ≥ 2) ので、親の確保 (タグの 8 バイト + 8k バイト) に収まる。
//! - 節 (確保) を 1 つ処理する: 値を読み、再帰でない所有物のフィールドを `drop_value` で解放し、`Box<T>` のフィールド (子)
//!   を集める。子が 0 個なら節の確保を返して作業リストから次へ、1 個なら確保を返してその子へ、k 個なら先頭の子へ降り、
//!   残りの k-1 個を節の確保に枠として書いて作業リストに積む (枠が空になったら、その確保を返す)。
//! - 制御はループ (ブロックの引数 `cur` / `stack`) なので、木の深さに依らず一定のスタックで済む。
//!
//! 対象は列挙型 (タグの後ろにペイロード) と、`Box<Self>` を直接持つ構造体。`Option<Box<Self>>` のフィールドは、KIR が
//! 集成体のフィールドに `Option` を許さないので、そもそも下ろせない (理由付きでバンドルへ退避する)。

use std::collections::{BTreeMap, BTreeSet};

use crate::kir::{Aop, Cc, Func, Mode, Op, Term, Ty, VId};
use crate::program::{Def, Program};
use crate::types::ExprTy;

use super::agg::AggTab;
use super::dyn_lower::build;
use super::{coll, Lw};

/// 値 `v` の型 `t` の欄の並びのうち、`Box<self_ty>` の欄 (子) を除いた所有物を解放し、子の番地を返す。
fn split_fields(lw: &mut Lw, v: VId, fields: &[crate::kir::Field], self_ty: Ty) -> Vec<VId> {
    let mut kids = vec![];
    for f in fields.iter().rev() {
        let is_kid = lw.aggs.borrow().box_elem(f.ty) == Some(self_ty);
        if is_kid {
            let fv = lw.emit(f.ty, Op::Field { a: v, off: f.off });
            kids.push(lw.emit(Ty::Ptr, Op::Field { a: fv, off: 0 }));
        } else if coll::owns(lw, f.ty) {
            let fv = lw.emit(f.ty, Op::Field { a: v, off: f.off });
            coll::drop_value(lw, fv, f.ty);
        }
    }
    kids.reverse();
    kids
}

/// 節の確保 `b` を返す (`skip` と同じ番地は返さない: 一時のスロット)。現在のブロックの続きに置く。
fn free_node(lw: &mut Lw, b: VId, skip: VId) {
    let ne = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Ne, ty: Ty::Ptr, a: b, b: skip });
    let (yes, _) = lw.new_block(&[]);
    let (join, _) = lw.new_block(&[]);
    lw.branch(ne, (yes, vec![]), (join, vec![]));
    lw.cur = Some(yes);
    lw.emit(Ty::Unit, Op::HeapFree(b));
    lw.goto(join, vec![]);
    lw.cur = Some(join);
}

/// 子 `kids` を持つ節 `cur` (作業リスト `stack`) の次の動き: 確保を返す・枠を積む・次の節へ (`lp`) か取り出しへ (`pop`)。
fn advance(lw: &mut Lw, cur: VId, stack: VId, skip: VId, kids: &[VId], lp: usize, pop: usize) {
    match kids.len() {
        0 => {
            free_node(lw, cur, skip);
            lw.goto(pop, vec![stack]);
        }
        1 => {
            free_node(lw, cur, skip);
            lw.goto(lp, vec![kids[0], stack]);
        }
        k => {
            lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: cur, off: 0, v: stack });
            let n = lw.emit(Ty::Usize, Op::Const((k - 1) as i128));
            lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: cur, off: 8, v: n });
            for (j, c) in kids[1..].iter().enumerate() {
                lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: cur, off: 16 + 8 * j as u32, v: *c });
            }
            lw.goto(lp, vec![kids[0], cur]);
        }
    }
}

fn body(lw: &mut Lw, ty: Ty, pv: &[VId]) -> Ty {
    let (p, skip) = (pv[0], pv[1]);
    let def = lw.aggs.borrow().defs[match ty { Ty::Agg(i) => i as usize, _ => unreachable!() }].clone();
    let null = lw.emit(Ty::Ptr, Op::Const(0));
    let (lp, lpv) = lw.new_block(&[Ty::Ptr, Ty::Ptr]);
    let (node, nv) = lw.new_block(&[Ty::Ptr, Ty::Ptr]);
    let (pop, popv) = lw.new_block(&[Ty::Ptr]);
    let (take, tv) = lw.new_block(&[Ty::Ptr]);
    let (done, _) = lw.new_block(&[]);
    lw.goto(lp, vec![p, null]);

    // lp(cur, stack): cur が null なら取り出しへ、そうでなければ節の処理へ。
    lw.cur = Some(lp);
    let isnull = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Ptr, a: lpv[0], b: null });
    lw.branch(isnull, (pop, vec![lpv[1]]), (node, vec![lpv[0], lpv[1]]));

    // node(cur, stack): 値を読み、変種ごとに処理する。
    lw.cur = Some(node);
    let (cur, stack) = (nv[0], nv[1]);
    let v = coll::box_load(lw, cur, ty);
    match def.kind.clone() {
        crate::kir::AggKind::Struct(fs) | crate::kir::AggKind::Tuple(fs) => {
            let kids = split_fields(lw, v, &fs, ty);
            advance(lw, cur, stack, skip, &kids, lp, pop);
        }
        crate::kir::AggKind::Enum(vs) => {
            let tag = lw.emit(Ty::I32, Op::Tag(v));
            for (k, var) in vs.iter().enumerate() {
                let kk = lw.emit(Ty::I32, Op::Const(k as i128));
                let c = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::I32, a: tag, b: kk });
                let (yes, _) = lw.new_block(&[]);
                let (no, _) = lw.new_block(&[]);
                lw.branch(c, (yes, vec![]), (no, vec![]));
                lw.cur = Some(yes);
                let kids = split_fields(lw, v, &var.fields, ty);
                advance(lw, cur, stack, skip, &kids, lp, pop);
                lw.cur = Some(no);
            }
            lw.terminate(Term::Unreachable);
        }
    }

    // pop(stack): 作業リストが空なら終わり、そうでなければ枠の最後の子を取る。
    lw.cur = Some(pop);
    let empty = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Ptr, a: popv[0], b: null });
    lw.branch(empty, (done, vec![]), (take, vec![popv[0]]));
    lw.cur = Some(take);
    let fr = tv[0];
    let n = lw.emit(Ty::Usize, Op::MemLoad { p: fr, off: 8 });
    let one = lw.emit(Ty::Usize, Op::Const(1));
    let n1 = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: n, b: one });
    let eight = lw.emit(Ty::Usize, Op::Const(8));
    let o = lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: n1, b: eight });
    let q = lw.emit(Ty::Ptr, Op::PtrAdd { p: fr, off: o });
    let c = lw.emit(Ty::Ptr, Op::MemLoad { p: q, off: 16 });
    let zero = lw.emit(Ty::Usize, Op::Const(0));
    let last = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: n1, b: zero });
    let (empty_fr, _) = lw.new_block(&[]);
    let (more, _) = lw.new_block(&[]);
    lw.branch(last, (empty_fr, vec![]), (more, vec![]));
    lw.cur = Some(empty_fr);
    let next = lw.emit(Ty::Ptr, Op::MemLoad { p: fr, off: 0 });
    free_node(lw, fr, skip);
    lw.goto(lp, vec![c, next]);
    lw.cur = Some(more);
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: fr, off: 8, v: n1 });
    lw.goto(lp, vec![c, fr]);

    lw.cur = Some(done);
    let u = lw.unit();
    lw.terminate(Term::Ret(u));
    Ty::Unit
}

/// 呼ばれた `derive [Drop]` の型の解放の補助関数を `ok` に足す。補助関数の中の `drop_value` が別の型の補助関数を要ることがあるので、
/// 要る型がなくなるまで繰り返す。
pub(super) fn synth(p: &Program, tys: &BTreeMap<(String, u32), ExprTy>, aggs: &std::cell::RefCell<AggTab>, def: &Def, ok: &mut Vec<(Func, BTreeSet<String>)>) {
    let mut done: BTreeSet<u32> = BTreeSet::new();
    loop {
        let todo: Vec<u32> = aggs.borrow().drop_need.iter().copied().filter(|i| !done.contains(i)).collect();
        if todo.is_empty() {
            return;
        }
        for i in todo {
            done.insert(i);
            let name = format!("drop.{}", aggs.borrow().defs[i as usize].name);
            let (f, callees) = build(p, tys, aggs, def, &[Ty::Ptr, Ty::Ptr], |lw, pv| body(lw, Ty::Agg(i), pv), |_| (vec![], false), &name);
            ok.push((f, callees));
        }
    }
}
