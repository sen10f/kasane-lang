//! `std.thread.spawn` / `JoinHandle.join` の KIR 下ろし (kir_lower_thread)。
//! 木走査の意味 (実装計画.md 2.2): `spawn` は保留するだけで走らせず、`join` の時に、そのハンドルが
//! 完了するまで保留中のスレッドを生成順に 1 本ずつ最後まで走らせる。join しないスレッドは走らない。
//!
//! KIR (間接呼び出しが無い) では spawn の位置 (呼び出し点) ごとに「サイト」を静的に持つ:
//! - サイト j = (クロージャの鍵、spawn 時点の取り込み値、結果の置き場、実行済みの旗)。結果の置き場と旗は入口の alloca。
//!   spawn は旗を 0 にして、ハンドル `{allocator, native = j, result}` を返す。`native` に実行時のサイト番号 j を持つので、
//!   `Ok(h) => h` と取り出しても値の流れで運ばれる。
//! - `join(h)`: `id = h.native`。サイト 0..n について「旗が 0 かつ j <= id」なら、取り込み値でクロージャを呼び、
//!   結果を置き場へ書いて旗を 1 にする (生成順)。その後、`id` のサイトの置き場から結果を読む。
//!   逆順に join しても、先に生成されたものが先に走る。
//! - 制限: spawn と join が同じ関数の中にあり、spawn が join を支配する位置 (取り込み値の SSA が届く) に限る。
//!   ループの中の spawn は対象外 (サイトが静的なので)。結果の型は alloca に置ける型 (スカラー・集成体・unit)。

use crate::kir::{BoolOp, Cc, Op, Pay, Pointee, Ty, VId};
use crate::parser::V;
use crate::types::Ty as Src;

use super::{coll, Lw, LR};

/// spawn の呼び出し点 1 つ。
#[derive(Clone)]
pub(crate) struct Site {
    pub key: String,
    pub caps: Vec<VId>,
    pub ret: Ty,
    pub hty: Ty,
    /// 結果の置き場 (`mutref<ret>`)。`ret` が unit なら無い。
    pub res: Option<VId>,
    /// 実行済みの旗 (`mutref<bool>`)。
    pub done: VId,
}

/// `std.thread.spawn(allocator, closure)` の呼び出し。対象外なら `None` (親へ戻す)。
pub(super) fn spawn_call(lw: &mut Lw, _e: &V, segs: &[String], args: &[V], line: usize) -> Option<LR<Option<VId>>> {
    if segs != ["std", "thread", "spawn"] {
        return None;
    }
    let mut args = args.iter();
    let Some(alloc_arg) = args.next() else { return None };
    let Some(closure_arg) = args.next() else { return None };
    if args.next().is_some() {
        return Some(lw.err(line, "std.thread.spawn の引数の個数が違う"));
    }
    if !lw.loops.is_empty() {
        return Some(lw.err(line, "ループの中の std.thread.spawn は対象外 (spawn の位置を静的に持つ)"));
    }
    let ret = match lw.src_ty(closure_arg) {
        Ok(Src::Closure { ret, .. }) => match lw.conv_t(&ret) {
            Some(t) => t,
            None => return Some(lw.err(line, format!("spawn の戻り値の型 {} は段階 1 の対象外", super::show(&ret)))),
        },
        Ok(t) => return Some(lw.err(line, format!("spawn の第2引数はクロージャでなければならない (型 {})", super::show(&t)))),
        Err(err) => return Some(lw.err(line, err)),
    };
    let alloc_src = match lw.src_ty(alloc_arg) {
        Ok(t) => t,
        Err(err) => return Some(lw.err(line, err)),
    };
    let alloc_ty = match lw.conv_t(&alloc_src) {
        Some(ty) => ty,
        None => return Some(lw.err(line, format!("spawn のアロケータの型 {} は段階 1 の対象外", super::show(&alloc_src)))),
    };
    let res_p = if ret == Ty::Unit {
        None
    } else {
        match Pointee::of(ret) {
            Some(p) => Some(p),
            None => return Some(lw.err(line, "spawn の結果の型は alloca に置けない")),
        }
    };

    let kind = match coll::closure_arg_kind(lw, closure_arg) {
        Ok(c) => c,
        Err(err) => return Some(Err(err)),
    };
    let Some(coll::Kind::Closure { key, caps: cap_vals, .. }) = kind else {
        return Some(lw.err(line, "spawn のクロージャが取り込みを持てない"));
    };
    if cap_vals.iter().any(|v| matches!(lw.vty(*v), Ty::Ref(..))) {
        return Some(lw.err(line, "spawn のクロージャは参照を取り込めない ('static でなければならない)"));
    }
    lw.callees.insert(key.clone());

    let hty = lw.aggs.borrow_mut().join_handle(ret, alloc_ty);
    let spawnerr_ty = match lw.conv_t(&Src::Named("std.thread.SpawnError".into(), vec![])) {
        Some(t) => t,
        None => return Some(lw.err(line, "std.thread.SpawnError は段階 1 の対象外")),
    };
    let res = res_p.map(|p| lw.alloca(p));
    let done = lw.alloca(Pointee::Bool);
    let f = lw.emit(Ty::Bool, Op::Const(0));
    lw.emit(Ty::Unit, Op::Store { ptr: done, val: f });
    let idx = lw.coll.pending_spawns.len();
    lw.coll.pending_spawns.push(Site { key, caps: cap_vals, ret, hty, res, done });

    // アロケータの値は KIR では使わない。`std.alloc.System` は `{ id: u8 }` のゼロ埋めを代用する。
    let zero = lw.emit(Ty::U8, Op::Const(0));
    let alloc = lw.emit(alloc_ty, Op::Make { variant: None, fields: vec![zero] });
    let result_ty = Ty::Res(Pay::of(hty).expect("JoinHandle がペイロードになれない"), Pay::of(spawnerr_ty).expect("SpawnError がペイロードになれない"));
    let native = lw.emit(Ty::Usize, Op::Const(idx as i128));
    let null = lw.emit(Ty::Ptr, Op::Const(0));
    let h = lw.emit(hty, Op::Make { variant: None, fields: vec![alloc, native, null] });
    Some(Ok(Some(lw.emit(result_ty, Op::Ok(h)))))
}

/// `std.thread.JoinHandle.join(self)`。対象外なら `None`。
pub(super) fn join_call(lw: &mut Lw, _e: &V, recv: VId, recv_ty: Ty, line: usize) -> Option<LR<Option<VId>>> {
    let ai = match recv_ty {
        Ty::Agg(ai) => ai,
        Ty::Ref(_, Pointee::Agg(ai)) => ai,
        _ => return None,
    };
    let (is_handle, off) = {
        let aggs = lw.aggs.borrow();
        let d = &aggs.defs[ai as usize];
        let off = match &d.kind {
            crate::kir::AggKind::Struct(fs) => fs.iter().find(|f| f.name == "native").map(|f| f.off),
            _ => None,
        };
        (d.name.starts_with("std.thread.JoinHandle<"), off)
    };
    if !is_handle {
        return None;
    }
    let off = off.expect("JoinHandle に native が無い");
    let hv = if matches!(recv_ty, Ty::Ref(..)) { lw.emit(Ty::Agg(ai), Op::Load(recv)) } else { recv };
    let sites = lw.coll.pending_spawns.clone();
    let cands: Vec<usize> = (0..sites.len()).filter(|j| sites[*j].hty == Ty::Agg(ai)).collect();
    if cands.is_empty() {
        return Some(lw.err(line, "join の相手の spawn がこの関数の中に無い (ハンドルを関数の外から受け取る形は未対応)"));
    }
    let ret = sites[cands[0]].ret;
    let id = lw.emit(Ty::Usize, Op::Field { a: hv, off });

    // 生成順に、未実行で j <= id のものを走らせる。
    for (j, s) in sites.iter().enumerate() {
        let cj = lw.emit(Ty::Usize, Op::Const(j as i128));
        let le = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::Usize, a: cj, b: id });
        let d = lw.emit(Ty::Bool, Op::Load(s.done));
        let nd = lw.emit(Ty::Bool, Op::Not(d));
        let c = lw.emit(Ty::Bool, Op::Bool(BoolOp::And, nd, le));
        let (run, _) = lw.new_block(&[]);
        let (cont, _) = lw.new_block(&[]);
        lw.branch(c, (run, vec![]), (cont, vec![]));
        lw.cur = Some(run);
        let v = lw.emit(s.ret, Op::Call { callee: s.key.clone(), args: s.caps.clone() });
        if let Some(r) = s.res {
            lw.emit(Ty::Unit, Op::Store { ptr: r, val: v });
        }
        let t = lw.emit(Ty::Bool, Op::Const(1));
        lw.emit(Ty::Unit, Op::Store { ptr: s.done, val: t });
        lw.goto(cont, vec![]);
        lw.cur = Some(cont);
    }

    // id のサイトの置き場から結果を読む。
    if ret == Ty::Unit {
        return Some(Ok(Some(lw.unit())));
    }
    let (fin, fp) = lw.new_block(&[ret]);
    for (n, &j) in cands.iter().enumerate() {
        let res = sites[j].res.expect("結果の置き場");
        if n + 1 == cands.len() {
            let v = lw.emit(ret, Op::Load(res));
            lw.goto(fin, vec![v]);
        } else {
            let cj = lw.emit(Ty::Usize, Op::Const(j as i128));
            let eq = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: id, b: cj });
            let (yes, _) = lw.new_block(&[]);
            let (no, _) = lw.new_block(&[]);
            lw.branch(eq, (yes, vec![]), (no, vec![]));
            lw.cur = Some(yes);
            let v = lw.emit(ret, Op::Load(res));
            lw.goto(fin, vec![v]);
            lw.cur = Some(no);
        }
    }
    lw.cur = Some(fin);
    Some(Ok(Some(fp[0])))
}
