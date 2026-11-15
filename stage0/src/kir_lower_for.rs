//! `for x in s.split(sep)` と `for c in s.chars()` の下ろし (設計_KIR.md 10.9)。
//!
//! - `split`: `s` と `sep` は `&str` (fat)。バイト比較で区切りを探し、各部分を元の `s`
//!   の切り出し ({ptr, len}) として出す (確保なし)。空の区切り・末尾の区切り・連続する
//!   区切りは木走査 (`natives.rs` の split) と同じ扱い。
//! - `chars`: `s` を 1 バイトずつ読み、UTF-8 を復号して `char` (u32) を出す (1〜4 バイト)。
//!
//! ループ変数はブロック引数で受け渡す (`counted` と同じ形)。`for _ in ...` も対応。

use crate::check::{items, nf, nlineno as nline, nname, st, strs};
use crate::kir::{Aop, BoolOp, Cc, Mode, Op, Ty, VId};
use crate::parser::V;
use super::{coll, Lw, LR};

pub(super) fn try_str_for(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    let f = nf(e);
    let line = nline(e);
    let name = match nname(&f[0]) {
        "PBind" if !matches!(nf(&f[0]).get(1), Some(V::B(true))) => Some(st(&nf(&f[0])[0])),
        "PWild" => None,
        _ => return lw.err(line, "for の変数はただの名前か _ だけ段階 1 の対象"),
    };
    let it = &f[1];
    // `s.split(sep)` / `s.chars()` は `Call [Path [s, split], [sep]]` または
    // `MCall [s, split, [sep]]` の形で現れる。
    let (recv, m, args): (V, String, &[V]) = match nname(it) {
        "Call" => {
            let cf = nf(it);
            if cf.len() < 2 {
                return Ok(None);
            }
            let segs = strs(&nf(&cf[0])[0]);
            let Some(m) = segs.last().cloned() else { return Ok(None); };
            let recv_segs: Vec<V> = segs[..segs.len() - 1]
                .iter()
                .map(|s| V::S(s.clone()))
                .collect();
            let recv = V::Node("Path", vec![V::L(recv_segs), V::Nil], 0, 0);
            (recv, m, items(&cf[1]))
        }
        "MCall" => {
            let cf = nf(it);
            if cf.len() < 3 {
                return Ok(None);
            }
            (cf[0].clone(), st(&cf[1]).to_string(), items(&cf[2]))
        }
        _ => return Ok(None),
    };
    let is_split = m == "split" && args.len() == 1;
    let is_chars = m == "chars" && args.is_empty();
    if !is_split && !is_chars {
        return Ok(None);
    }
    let body = &f[2];
    let Some(h) = lw.expr(&recv)? else { return Ok(None) };
    if lw.vty(h) != lw.fat_ty() {
        return lw.err(line, "for の対象は &str (fat) だけ段階 1 の対象");
    }
    if is_split {
        let sep = &args[0];
        let Some(sh) = lw.expr(sep)? else { return Ok(None) };
        if lw.vty(sh) != lw.fat_ty() {
            return lw.err(line, "split の区切りは &str (fat) だけ段階 1 の対象");
        }
        split_for(lw, line, h, sh, name, body)
    } else {
        chars_for(lw, line, h, name, body)
    }
}

// ---------------- split ----------------

/// `for x in s.split(sep) BODY`。
fn split_for(lw: &mut Lw, _line: usize, s: VId, sep: VId, name: Option<String>, body: &V) -> LR<Option<VId>> {
    let slots = lw.assigned(&[body]);
    let slot_tys: Vec<Ty> = slots.iter().map(|s| lw.slots[*s].1).collect();
    let s_ptr = lw.emit(Ty::Ptr, Op::Field { a: s, off: 0 });
    let s_len = lw.emit(Ty::Usize, Op::Field { a: s, off: 8 });
    let sep_ptr = lw.emit(Ty::Ptr, Op::Field { a: sep, off: 0 });
    let sep_len = lw.emit(Ty::Usize, Op::Field { a: sep, off: 8 });
    let zero = lw.emit(Ty::Usize, Op::Const(0));
    let one = lw.emit(Ty::Usize, Op::Const(1));

    let (xb, xp) = lw.new_block(&slot_tys);              // exit
    let hty: Vec<Ty> = std::iter::once(Ty::Usize).chain(slot_tys.iter().copied()).collect();
    let (hb, hp) = lw.new_block(&hty);                    // header(from, slots)
    let fhty: Vec<Ty> = std::iter::once(Ty::Usize).chain(std::iter::once(Ty::Usize)).chain(slot_tys.iter().copied()).collect();
    let (fh, fhp) = lw.new_block(&fhty);                  // find(from, i, slots)
    let chty: Vec<Ty> = std::iter::once(Ty::Usize).chain(std::iter::once(Ty::Usize)).chain(std::iter::once(Ty::Usize)).chain(slot_tys.iter().copied()).collect();
    let (ch, chp) = lw.new_block(&chty);                  // cmp(from, k, i, slots)
    let (t1, _) = lw.new_block(&chty);                    // cmp 中継 (from, k, i, slots)
    let bhty: Vec<Ty> = std::iter::once(Ty::Usize).chain(std::iter::once(Ty::Usize)).chain(slot_tys.iter().copied()).collect();
    let (bb, bp) = lw.new_block(&bhty);                   // body(from, end, slots)
    let cbty: Vec<Ty> = std::iter::once(Ty::Usize).chain(slot_tys.iter().copied()).collect();
    let (aft, cbp) = lw.new_block(&cbty);                  // after(end, slots): 末尾なら抜け、次の区切りへ (本体の末尾と continue の飛び先)

    let entry: Vec<VId> = std::iter::once(zero).chain(lw.vals_of(&slots)).collect();
    lw.goto(hb, entry);

    // ---- header(from, slots) ----
    lw.cur = Some(hb);
    let from = hp[0];
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (hp[k + 1], slot_tys[k]);
    }
    let empty = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: sep_len, b: zero });
    let s_len0 = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: s_len, b: zero });
    let trivial = lw.emit(Ty::Bool, Op::Bool(BoolOp::Or, empty, s_len0));
    lw.branch(trivial, (bb, body_args(lw, &[from, s_len], &slots)),
                 (fh, head_args(lw, &[from, from], &slots)));

    // ---- find(from, i, slots) ----
    lw.cur = Some(fh);
    let ffrom = fhp[0];
    let fi = fhp[1];
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (fhp[k + 2], slot_tys[k]);
    }
    // `i + sep_len > s_len` なら、もう区切りは入らない (`s_len - sep_len` は sep が長いと wrap するので使わない。
    // 長さは確保できる大きさなので、足し算は溢れない)。
    let end = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: fi, b: sep_len });
    let i_past = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::Usize, a: end, b: s_len });
    lw.branch(i_past, (bb, body_args(lw, &[ffrom, s_len], &slots)),
                 (ch, head_args(lw, &[ffrom, zero, fi], &slots)));

    // ---- cmp(from, k, i, slots) ----
    lw.cur = Some(ch);
    let cfrom = chp[0];
    let k = chp[1];
    let ci = chp[2];
    for (k2, s) in slots.iter().enumerate() {
        lw.slots[*s] = (chp[k2 + 3], slot_tys[k2]);
    }
    let k_eq = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: k, b: sep_len });
    lw.branch(k_eq, (bb, body_args(lw, &[cfrom, ci], &slots)), (t1, head_args(lw, &[cfrom, k, ci], &slots)));

    // ---- t1(from, k, i, slots): s[i+k] != sep[k] ? find(i+1) : cmp(k+1) ----
    lw.cur = Some(t1);
    let t1from = chp[0];
    let t1k = chp[1];
    let t1i = chp[2];
    for (k2, s) in slots.iter().enumerate() {
        lw.slots[*s] = (chp[k2 + 3], slot_tys[k2]);
    }
    let ck = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: t1i, b: t1k });
    let sp = lw.emit(Ty::Ptr, Op::PtrAdd { p: s_ptr, off: ck });
    let sb = lw.emit(Ty::U8, Op::MemLoad { p: sp, off: 0 });
    let cp = lw.emit(Ty::Ptr, Op::PtrAdd { p: sep_ptr, off: t1k });
    let cb = lw.emit(Ty::U8, Op::MemLoad { p: cp, off: 0 });
    let diff = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Ne, ty: Ty::U8, a: sb, b: cb });
    let ni = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: t1i, b: one });
    let k1 = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: t1k, b: one });
    lw.branch(diff, (fh, head_args(lw, &[t1from, ni], &slots)),
                 (ch, head_args(lw, &[t1from, k1, t1i], &slots)));

    // ---- body(from, end, slots) ----
    lw.cur = Some(bb);
    let bfrom = bp[0];
    let bend = bp[1];
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (bp[k + 2], slot_tys[k]);
    }
    let piece_ptr = lw.emit(Ty::Ptr, Op::PtrAdd { p: s_ptr, off: bfrom });
    let piece_len = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: bend, b: bfrom });
    let fat = lw.fat_ty();
    let piece = lw.emit(fat, Op::Make { variant: None, fields: vec![piece_ptr, piece_len] });
    lw.scopes.push(vec![]);
    let depth = lw.scopes.len() - 1;
    if let Some(n) = &name {
        coll::bind_owned(lw, n, piece, fat);
        // 束縛した `x` は `&str` (fat) としてメソッド (`is_empty` など) から参照できるように種類を登録する。
        if let Some(s) = lw.lookup(n) {
            lw.coll.kinds.insert(s, coll::Kind::Str);
        }
    }
    // `continue` は後続 (aft) へ、`break` は抜けへ。aft は区切りの位置 (bend) を受け取る。
    lw.loops.push(super::LoopCx { header: aft, exit: xb, slots: slots.clone(), depth, cont: vec![bend] });
    let r = lw.expr(body)?;
    lw.loops.pop();
    lw.scopes.pop();
    if r.is_some() {
        let mut args = vec![bend];
        args.extend(lw.vals_of(&slots));
        lw.goto(aft, args);
    }

    // ---- after(end, slots): 末尾の区切りなら抜ける。そうでなければ次の片を探す ----
    lw.cur = Some(aft);
    let cend = cbp[0];
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (cbp[k + 1], slot_tys[k]);
    }
    let done = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: cend, b: s_len });
    let next_from = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: cend, b: sep_len });
    lw.branch(done, (xb, slot_vals(lw, &slots)), (hb, next_from_args(lw, next_from, &slots)));

    lw.cur = Some(xb);
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (xp[k], slot_tys[k]);
    }
    Ok(Some(lw.unit()))
}

fn head_args(lw: &Lw, extra: &[VId], slots: &[usize]) -> Vec<VId> {
    let mut out = extra.to_vec();
    for s in slots {
        out.push(lw.slots[*s].0);
    }
    out
}
fn body_args(lw: &Lw, extra: &[VId], slots: &[usize]) -> Vec<VId> {
    let mut out = extra.to_vec();
    for s in slots {
        out.push(lw.slots[*s].0);
    }
    out
}
fn next_from_args(lw: &Lw, next_from: VId, slots: &[usize]) -> Vec<VId> {
    let mut out = vec![next_from];
    for s in slots {
        out.push(lw.slots[*s].0);
    }
    out
}
fn slot_vals(lw: &Lw, slots: &[usize]) -> Vec<VId> {
    slots.iter().map(|s| lw.slots[*s].0).collect()
}

// ---------------- chars ----------------

/// `for c in s.chars() BODY`。
fn chars_for(lw: &mut Lw, _line: usize, s: VId, name: Option<String>, body: &V) -> LR<Option<VId>> {
    let slots = lw.assigned(&[body]);
    let slot_tys: Vec<Ty> = slots.iter().map(|s| lw.slots[*s].1).collect();
    let s_ptr = lw.emit(Ty::Ptr, Op::Field { a: s, off: 0 });
    let s_len = lw.emit(Ty::Usize, Op::Field { a: s, off: 8 });
    let zero = lw.emit(Ty::Usize, Op::Const(0));
    let one = lw.emit(Ty::Usize, Op::Const(1));

    let (xb, xp) = lw.new_block(&slot_tys);              // exit
    let hty: Vec<Ty> = std::iter::once(Ty::Usize).chain(slot_tys.iter().copied()).collect();
    let (hb, hp) = lw.new_block(&hty);                    // header(i, slots)
    let (hb2, _) = lw.new_block(&hty);                    // header の continuation (i < s_len)
    let (t1, _) = lw.new_block(&hty);                     // i, slots (b0u32 < 0xE0 ?)
    let (t2, _) = lw.new_block(&hty);                     // i, slots (b0u32 < 0xF0 ?)
    let (w2, _) = lw.new_block(&hty);                     // width 2: (i, slots)
    let (w3, _) = lw.new_block(&hty);                     // width 3: (i, slots)
    let (w4, _) = lw.new_block(&hty);                     // width 4: (i, slots)
    let dhty: Vec<Ty> = std::iter::once(Ty::U32).chain(std::iter::once(Ty::Usize)).chain(std::iter::once(Ty::Usize)).chain(slot_tys.iter().copied()).collect();
    let (db, dp) = lw.new_block(&dhty);                   // done(ch:u32, width:usize, i, slots)

    let entry: Vec<VId> = std::iter::once(zero).chain(lw.vals_of(&slots)).collect();
    lw.goto(hb, entry);

    // ---- header(i, slots) ----
    lw.cur = Some(hb);
    let i = hp[0];
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (hp[k + 1], slot_tys[k]);
    }
    let i_eq = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: i, b: s_len });
    lw.branch(i_eq, (xb, slot_vals(lw, &slots)), (hb2, head_args(lw, &[i], &slots)));
    // hb2 は i < s_len の continuation。i とスロットを再受け取りし、先に読み出しと分岐を行う。
    lw.cur = Some(hb2);
    let off = lw.emit(Ty::Ptr, Op::PtrAdd { p: s_ptr, off: i });
    let b0 = lw.emit(Ty::U8, Op::MemLoad { p: off, off: 0 });
    let b0u32 = lw.emit(Ty::U32, Op::Conv { src: Ty::U8, a: b0 });
    let c80 = lw.emit(Ty::U32, Op::Const(128));
    let lt80 = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::U32, a: b0u32, b: c80 });
    lw.branch(lt80, (db, done_args(lw, b0u32, one, i, &slots)), (t1, head_args(lw, &[i], &slots)));

    // ---- t1(i, slots): b0u32 < 0xE0 ? w2 : t2 ----
    lw.cur = Some(t1);
    let t1i = hp[0];
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (hp[k + 1], slot_tys[k]);
    }
    let off = lw.emit(Ty::Ptr, Op::PtrAdd { p: s_ptr, off: t1i });
    let b0 = lw.emit(Ty::U8, Op::MemLoad { p: off, off: 0 });
    let b0u32 = lw.emit(Ty::U32, Op::Conv { src: Ty::U8, a: b0 });
    let c_e0 = lw.emit(Ty::U32, Op::Const(224));
    let lt_e0 = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::U32, a: b0u32, b: c_e0 });
    lw.branch(lt_e0, (w2, head_args(lw, &[t1i], &slots)), (t2, head_args(lw, &[t1i], &slots)));

    // ---- t2(i, slots): b0u32 < 0xF0 ? w3 : w4 ----
    lw.cur = Some(t2);
    let t2i = hp[0];
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (hp[k + 1], slot_tys[k]);
    }
    let off = lw.emit(Ty::Ptr, Op::PtrAdd { p: s_ptr, off: t2i });
    let b0 = lw.emit(Ty::U8, Op::MemLoad { p: off, off: 0 });
    let b0u32 = lw.emit(Ty::U32, Op::Conv { src: Ty::U8, a: b0 });
    let c_f0 = lw.emit(Ty::U32, Op::Const(240));
    let lt_f0 = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::U32, a: b0u32, b: c_f0 });
    lw.branch(lt_f0, (w3, head_args(lw, &[t2i], &slots)), (w4, head_args(lw, &[t2i], &slots)));

    // ---- w2(i, slots): 2 バイト ----
    width_block(lw, 2, 0x1F, 6, w2, db, &s_ptr, &slots, &slot_tys)?;
    // ---- w3(i, slots): 3 バイト ----
    width_block(lw, 3, 0x0F, 12, w3, db, &s_ptr, &slots, &slot_tys)?;
    // ---- w4(i, slots): 4 バイト ----
    width_block(lw, 4, 0x07, 18, w4, db, &s_ptr, &slots, &slot_tys)?;

    // ---- done(ch:u32, width:usize, i, slots) ----
    lw.cur = Some(db);
    let ch = dp[0];
    let width = dp[1];
    let di = dp[2];
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (dp[k + 3], slot_tys[k]);
    }
    let next_i = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: di, b: width });
    lw.scopes.push(vec![]);
    let depth = lw.scopes.len() - 1;
    if let Some(n) = &name {
        coll::bind_owned(lw, n, ch, Ty::U32);
    }
    // `continue` は次の文字の位置 (next_i) を header へ渡す。
    lw.loops.push(super::LoopCx { header: hb, exit: xb, slots: slots.clone(), depth, cont: vec![next_i] });
    let r = lw.expr(body)?;
    lw.loops.pop();
    lw.scopes.pop();
    if r.is_some() {
        lw.goto(hb, next_from_args(lw, next_i, &slots));
    }
    lw.cur = Some(xb);
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (xp[k], slot_tys[k]);
    }
    Ok(Some(lw.unit()))
}

/// 幅 `n` バイトの UTF-8 復号ブロック。`lead_mask` は先頭バイトのマスク、`lead_shift` は
/// 先頭バイトのシフト量。復号した `ch` と `width=n` を `done` へ渡す。
fn width_block(lw: &mut Lw, n: u32, lead_mask: u32, lead_shift: u32,
    blk: usize, db: usize, s_ptr: &VId, slots: &[usize], slot_tys: &[Ty]) -> LR<()> {
    lw.cur = Some(blk);
    let i = lw.blocks[blk].params[0].0;
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (lw.blocks[blk].params[k + 1].0, slot_tys[k]);
    }
    let mut acc: Option<VId> = None;
    for k in 0..n {
        let kc = lw.emit(Ty::Usize, Op::Const(k as i128));
        let ik = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: i, b: kc });
        let off = lw.emit(Ty::Ptr, Op::PtrAdd { p: *s_ptr, off: ik });
        let b = lw.emit(Ty::U8, Op::MemLoad { p: off, off: 0 });
        let bu32 = lw.emit(Ty::U32, Op::Conv { src: Ty::U8, a: b });
        let masked = if k == 0 {
            let cm = lw.emit(Ty::U32, Op::Const(lead_mask as i128));
            lw.emit(Ty::U32, Op::Bool(BoolOp::And, bu32, cm))
        } else {
            let cm = lw.emit(Ty::U32, Op::Const(0x3F));
            lw.emit(Ty::U32, Op::Bool(BoolOp::And, bu32, cm))
        };
        let shifted = if k == 0 {
            let ns = lw.emit(Ty::U32, Op::Const(lead_shift as i128));
            lw.emit(Ty::U32, Op::Shift { left: true, a: masked, n: ns })
        } else {
            let shift_amt = (6 * (n - 1 - k)) as i128;
            let ns = lw.emit(Ty::U32, Op::Const(shift_amt));
            lw.emit(Ty::U32, Op::Shift { left: true, a: masked, n: ns })
        };
        acc = Some(match acc {
            None => shifted,
            Some(a) => lw.emit(Ty::U32, Op::Bool(BoolOp::Or, a, shifted)),
        });
    }
    let ch = acc.unwrap();
    let w = lw.emit(Ty::Usize, Op::Const(n as i128));
    lw.goto(db, done_args(lw, ch, w, i, slots));
    Ok(())
}

fn done_args(lw: &Lw, ch: VId, width: VId, i: VId, slots: &[usize]) -> Vec<VId> {
    let mut out = vec![ch, width, i];
    for s in slots {
        out.push(lw.slots[*s].0);
    }
    out
}
