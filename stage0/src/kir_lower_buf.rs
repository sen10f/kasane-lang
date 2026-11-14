//! KIR の下ろし: `alloc.raw.Buf<T, A>` (仕様 8.3、修正プラン 3-3、方式 A)。
//!
//! `Buf` は `Vec` と同じ 24 バイトのヘッダ {ptr, len, cap} (`cap` は要素数) で表し、所有・移動・解放 (要素が所有物なら
//! 各要素を先に解放) は `Vec` と同じ規則に乗る。ここは `Buf` だけのメソッドを下ろす:
//! `with_capacity` / `cap` / `grow` / `push_within_cap` / `insert_within_cap` / `swap_remove` / `truncate` /
//! `append_str_within_cap` / `as_str`。共通の `len` / `is_empty` / `pop` / `remove` / `get` / `clear` / `as_slice` は
//! `Vec` の下ろしを使う。すべて範囲外・容量不足を `Option` / `Result` で返し、panic しない。アロケータ `A` は
//! `std.alloc.System` だけ対象 (値は使わない)。

use super::coll::{self, Kind};
use super::{Lw, LR};
use crate::check::{items, nf, nlineno as nline};
use crate::kir::{Aop, Cc, Mode, Op, Pay, Pointee, Ty, VId};
use crate::parser::V;
use crate::types::Ty as Src;

fn usize_const(lw: &mut Lw, n: i128) -> VId {
    lw.emit(Ty::Usize, Op::Const(n))
}

fn arith(lw: &mut Lw, op: Aop, a: VId, b: VId) -> VId {
    lw.emit(Ty::Usize, Op::Arith { op, mode: Mode::Wrap, a, b })
}

fn cmp(lw: &mut Lw, cc: Cc, a: VId, b: VId) -> VId {
    lw.emit(Ty::Bool, Op::Cmp { cc, ty: Ty::Usize, a, b })
}

/// `idx` 番目の要素のバイト位置 (`idx * sz`)。
fn byte_off(lw: &mut Lw, idx: VId, sz: u32) -> VId {
    if sz == 1 {
        return idx;
    }
    let k = usize_const(lw, sz as i128);
    arith(lw, Aop::Mul, idx, k)
}

/// バッファの `idx` 番目の要素のアドレス。
fn elem_addr(lw: &mut Lw, h: VId, idx: VId, sz: u32) -> VId {
    let base = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let off = byte_off(lw, idx, sz);
    lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off })
}

/// 要素型 (分かっていて大きさがあるもの) と大きさ。
fn elem_of(lw: &mut Lw, line: usize, kind: &Kind, what: &str) -> LR<(Ty, u32)> {
    let Kind::Vec(Some(elem)) = kind else {
        return lw.err(line, format!("{} の対象は要素型の分かる Buf だけ段階 1 の対象", what));
    };
    match coll::elem_size(lw, *elem) {
        Some(sz) if Pay::of(*elem).is_some() => Ok((*elem, sz)),
        _ => lw.err(line, format!("{} の要素の型 {} は段階 1 の対象外", what, elem.name())),
    }
}

fn alloc_err_pay(lw: &mut Lw) -> Pay {
    match lw.aggs.borrow_mut().alloc_err() {
        Ty::Agg(i) => Pay::Agg(i),
        _ => unreachable!("AllocError は集成体"),
    }
}

/// `alloc.raw.Buf.with_capacity(allocator, n)`: `n` 個分のバッファを確保して `Ok(Buf)` (確保の失敗は起きない)。
pub(super) fn with_capacity(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    let f = nf(e);
    let line = nline(e);
    let args = items(&f[1]);
    if args.len() != 2 {
        return lw.err(line, "Buf.with_capacity の引数は 2 つのはず");
    }
    match lw.src_ty(&args[0]) {
        Ok(Src::Named(a, _)) if a == "std.alloc.System" => {}
        _ => return lw.err(line, "Buf のアロケータは std.alloc.System だけ段階 1 の対象"),
    }
    // `std.alloc.SYSTEM` の定数は値を作らない (使わない)。変数・フィールドのアロケータは評価だけする。
    let literal = crate::check::nname(&args[0]) == "Path" && crate::check::strs(&nf(&args[0])[0]) == ["std", "alloc", "SYSTEM"];
    if !literal && lw.expr(&args[0])?.is_none() {
        return Ok(None);
    }
    let Some(n) = lw.expr(&args[1])? else { return Ok(None) };
    if lw.vty(n) != Ty::Usize {
        return lw.err(line, "Buf.with_capacity の容量が usize でない");
    }
    let rt = lw.ty_of(e)?;
    let Ty::Res(Pay::Agg(vi), _) = rt else {
        return lw.err(line, "Buf.with_capacity の結果の型が Result<Buf, AllocError> でない");
    };
    let vt = Ty::Agg(vi);
    if !lw.aggs.borrow().is_vec(vt) {
        return lw.err(line, "Buf.with_capacity の結果の型が Result<Buf, AllocError> でない");
    }
    let Ok(Src::Named(_, rargs)) = lw.src_ty(e) else {
        return lw.err(line, "Buf.with_capacity の結果の型が決まらない");
    };
    let elem = match rargs.first() {
        Some(Src::Named(_, ba)) => ba.first().and_then(|t| coll::sig_ty(lw, t)),
        _ => None,
    };
    let Some(sz) = elem.and_then(|t| coll::elem_size(lw, t)) else {
        return lw.err(line, "Buf の要素の型が段階 1 の対象外");
    };
    let bytes = byte_off(lw, n, sz);
    let p = lw.emit(Ty::Ptr, Op::HeapAlloc(bytes));
    let zero = usize_const(lw, 0);
    let hdr = lw.emit(vt, Op::Make { variant: None, fields: vec![p, zero, n] });
    Ok(Some(lw.emit(rt, Op::Ok(hdr))))
}

/// `Buf` だけのメソッド。扱わない名前は `None` (`Vec` と共通の下ろしへ)。
pub(super) fn method(lw: &mut Lw, e: &V, line: usize, kind: &Kind, h: VId, m: &str, args: &[V]) -> Option<LR<Option<VId>>> {
    if !matches!(kind, Kind::Vec(_)) {
        return None;
    }
    Some(match m {
        "cap" if args.is_empty() => Ok(Some(lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 }))),
        "push_within_cap" => push_within_cap(lw, e, line, kind, h, args),
        "insert_within_cap" => insert_within_cap(lw, e, line, kind, h, args),
        "swap_remove" => swap_remove(lw, e, line, kind, h, args),
        "truncate" => truncate(lw, line, kind, h, args),
        "grow" => grow(lw, e, line, kind, h, args),
        "append_str_within_cap" => append_str(lw, e, line, h, args),
        "as_str" => {
            let view = coll::hdr_view(lw, h);
            lw.from_utf8_of(view, line)
        }
        _ => return None,
    })
}

fn result_ty(lw: &mut Lw, e: &V, line: usize, what: &str) -> LR<Ty> {
    let rt = lw.ty_of(e)?;
    if !matches!(rt, Ty::Res(..)) {
        return lw.err(line, format!("{} の結果の型が Result でない", what));
    }
    Ok(rt)
}

/// `b.push_within_cap(x)`: `len < cap` なら末尾へ書いて `Ok(())`、満杯なら `Err(x)` (値を返す)。
fn push_within_cap(lw: &mut Lw, e: &V, line: usize, kind: &Kind, h: VId, args: &[V]) -> LR<Option<VId>> {
    if args.len() != 1 {
        return lw.err(line, "push_within_cap の引数は 1 つのはず");
    }
    let (elem, sz) = elem_of(lw, line, kind, "push_within_cap")?;
    let rt = result_ty(lw, e, line, "push_within_cap")?;
    let Some(x) = lw.expr(&args[0])? else { return Ok(None) };
    if lw.vty(x) != elem {
        return lw.err(line, "push_within_cap の引数の型が要素の型と違う");
    }
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let cap = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 });
    let room = cmp(lw, Cc::Lt, len, cap);
    let (yes, _) = lw.new_block(&[]);
    let (no, _) = lw.new_block(&[]);
    let (join, jp) = lw.new_block(&[rt]);
    lw.branch(room, (yes, vec![]), (no, vec![]));
    lw.cur = Some(yes);
    let addr = elem_addr(lw, h, len, sz);
    coll::store_elem(lw, addr, elem, x);
    let one = usize_const(lw, 1);
    let nl = arith(lw, Aop::Add, len, one);
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: nl });
    let u = lw.unit();
    let okv = lw.emit(rt, Op::Ok(u));
    lw.goto(join, vec![okv]);
    lw.cur = Some(no);
    let errv = lw.emit(rt, Op::Err(x));
    lw.goto(join, vec![errv]);
    lw.cur = Some(join);
    Ok(Some(jp[0]))
}

/// `b.insert_within_cap(i, x)`: `i <= len && len < cap` なら `i` へ挿入 (後ろへ詰める) して `Ok(())`、そうでなければ `Err(x)`。
fn insert_within_cap(lw: &mut Lw, e: &V, line: usize, kind: &Kind, h: VId, args: &[V]) -> LR<Option<VId>> {
    if args.len() != 2 {
        return lw.err(line, "insert_within_cap の引数は 2 つのはず");
    }
    let (elem, sz) = elem_of(lw, line, kind, "insert_within_cap")?;
    let rt = result_ty(lw, e, line, "insert_within_cap")?;
    let Some(i) = lw.expr(&args[0])? else { return Ok(None) };
    let Some(x) = lw.expr(&args[1])? else { return Ok(None) };
    if lw.vty(i) != Ty::Usize || lw.vty(x) != elem {
        return lw.err(line, "insert_within_cap の引数の型が違う");
    }
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let cap = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 });
    let in_range = cmp(lw, Cc::Le, i, len);
    let room = cmp(lw, Cc::Lt, len, cap);
    let both = lw.emit(Ty::Bool, Op::Bool(crate::kir::BoolOp::And, in_range, room));
    let (yes, _) = lw.new_block(&[]);
    let (no, _) = lw.new_block(&[]);
    let (join, jp) = lw.new_block(&[rt]);
    lw.branch(both, (yes, vec![]), (no, vec![]));
    lw.cur = Some(yes);
    // 右へ詰める: j = len, len-1, .., i+1 について buf[j] = buf[j-1]。
    let one = usize_const(lw, 1);
    let (lh, lj) = lw.new_block(&[Ty::Usize]);
    let (ls, _) = lw.new_block(&[]);
    let (lexit, _) = lw.new_block(&[]);
    lw.goto(lh, vec![len]);
    lw.cur = Some(lh);
    let j = lj[0];
    let more = cmp(lw, Cc::Gt, j, i);
    lw.branch(more, (ls, vec![]), (lexit, vec![]));
    lw.cur = Some(ls);
    let j1 = arith(lw, Aop::Sub, j, one);
    let src = elem_addr(lw, h, j1, sz);
    let dst = elem_addr(lw, h, j, sz);
    let v = coll::load_elem(lw, src, elem);
    coll::store_elem(lw, dst, elem, v);
    lw.goto(lh, vec![j1]);
    lw.cur = Some(lexit);
    let at = elem_addr(lw, h, i, sz);
    coll::store_elem(lw, at, elem, x);
    let nl = arith(lw, Aop::Add, len, one);
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: nl });
    let u = lw.unit();
    let okv = lw.emit(rt, Op::Ok(u));
    lw.goto(join, vec![okv]);
    lw.cur = Some(no);
    let errv = lw.emit(rt, Op::Err(x));
    lw.goto(join, vec![errv]);
    lw.cur = Some(join);
    Ok(Some(jp[0]))
}

/// `b.swap_remove(i)`: `i < len` なら要素 `i` を取り出し、最後の要素をその位置へ移して `len` を 1 減らす。
fn swap_remove(lw: &mut Lw, e: &V, line: usize, kind: &Kind, h: VId, args: &[V]) -> LR<Option<VId>> {
    if args.len() != 1 {
        return lw.err(line, "swap_remove の引数は 1 つのはず");
    }
    let (elem, sz) = elem_of(lw, line, kind, "swap_remove")?;
    let Some(pay) = Pay::of(elem) else { return lw.err(line, "swap_remove の要素の型が対象外") };
    let opt = Ty::Opt(pay);
    if lw.ty_of(e)? != opt {
        return lw.err(line, "swap_remove の結果の型が Option<要素> でない");
    }
    let Some(i) = lw.expr(&args[0])? else { return Ok(None) };
    if lw.vty(i) != Ty::Usize {
        return lw.err(line, "swap_remove の添字が usize でない");
    }
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let inb = cmp(lw, Cc::Lt, i, len);
    let (yes, _) = lw.new_block(&[]);
    let (no, _) = lw.new_block(&[]);
    let (join, jp) = lw.new_block(&[opt]);
    lw.branch(inb, (yes, vec![]), (no, vec![]));
    lw.cur = Some(no);
    let none = lw.emit(opt, Op::Const(0));
    lw.goto(join, vec![none]);
    lw.cur = Some(yes);
    let at = elem_addr(lw, h, i, sz);
    let x = coll::load_elem(lw, at, elem);
    let one = usize_const(lw, 1);
    let last = arith(lw, Aop::Sub, len, one);
    let lastp = elem_addr(lw, h, last, sz);
    let lv = coll::load_elem(lw, lastp, elem);
    coll::store_elem(lw, at, elem, lv);
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: last });
    let some = lw.emit(opt, Op::Some(x));
    lw.goto(join, vec![some]);
    lw.cur = Some(join);
    Ok(Some(jp[0]))
}

/// `b.truncate(n)`: `n < len` なら `n..len` の要素 (所有物なら解放して) を捨て、`len` を `n` にする。
fn truncate(lw: &mut Lw, line: usize, kind: &Kind, h: VId, args: &[V]) -> LR<Option<VId>> {
    if args.len() != 1 {
        return lw.err(line, "truncate の引数は 1 つのはず");
    }
    let (elem, sz) = elem_of(lw, line, kind, "truncate")?;
    let Some(n) = lw.expr(&args[0])? else { return Ok(None) };
    if lw.vty(n) != Ty::Usize {
        return lw.err(line, "truncate の長さが usize でない");
    }
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let shrink = cmp(lw, Cc::Lt, n, len);
    let (yes, _) = lw.new_block(&[]);
    let (done, _) = lw.new_block(&[]);
    lw.branch(shrink, (yes, vec![]), (done, vec![]));
    lw.cur = Some(yes);
    // 長さを先に縮める (捨てる要素の解放の途中で観測されない)。
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: n });
    if coll::owns(lw, elem) {
        let (lh, lj) = lw.new_block(&[Ty::Usize]);
        let (ls, _) = lw.new_block(&[]);
        let (lexit, _) = lw.new_block(&[]);
        lw.goto(lh, vec![n]);
        lw.cur = Some(lh);
        let j = lj[0];
        let more = cmp(lw, Cc::Lt, j, len);
        lw.branch(more, (ls, vec![]), (lexit, vec![]));
        lw.cur = Some(ls);
        let at = elem_addr(lw, h, j, sz);
        let x = coll::load_elem(lw, at, elem);
        coll::drop_value(lw, x, elem);
        let one = usize_const(lw, 1);
        let j1 = arith(lw, Aop::Add, j, one);
        lw.goto(lh, vec![j1]);
        lw.cur = Some(lexit);
    }
    lw.goto(done, vec![]);
    lw.cur = Some(done);
    Ok(Some(lw.unit()))
}

/// `b.grow(new_cap)`: `new_cap > cap` ならバッファを広げて `cap` を `new_cap` にする。結果は `Ok(())`。
fn grow(lw: &mut Lw, e: &V, line: usize, kind: &Kind, h: VId, args: &[V]) -> LR<Option<VId>> {
    if args.len() != 1 {
        return lw.err(line, "grow の引数は 1 つのはず");
    }
    let (_elem, sz) = elem_of(lw, line, kind, "grow")?;
    let rt = result_ty(lw, e, line, "grow")?;
    let _ = alloc_err_pay(lw);
    let Some(n) = lw.expr(&args[0])? else { return Ok(None) };
    if lw.vty(n) != Ty::Usize {
        return lw.err(line, "grow の容量が usize でない");
    }
    let cap = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 });
    let bigger = cmp(lw, Cc::Gt, n, cap);
    let (yes, _) = lw.new_block(&[]);
    let (done, _) = lw.new_block(&[]);
    lw.branch(bigger, (yes, vec![]), (done, vec![]));
    lw.cur = Some(yes);
    let bytes = byte_off(lw, n, sz);
    let old = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let np = lw.emit(Ty::Ptr, Op::HeapRealloc { p: old, size: bytes });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: h, off: 0, v: np });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 16, v: n });
    lw.goto(done, vec![]);
    lw.cur = Some(done);
    let u = lw.unit();
    Ok(Some(lw.emit(rt, Op::Ok(u))))
}

/// `Buf<u8>.append_str_within_cap(text)`: 余裕が `text` の長さ以上ならバイトを写して `Ok(())`、なければ `Err(())`。
fn append_str(lw: &mut Lw, e: &V, line: usize, h: VId, args: &[V]) -> LR<Option<VId>> {
    if args.len() != 1 {
        return lw.err(line, "append_str_within_cap の引数は 1 つのはず");
    }
    let rt = result_ty(lw, e, line, "append_str_within_cap")?;
    let Some(text) = lw.expr(&args[0])? else { return Ok(None) };
    if lw.vty(text) != lw.fat_ty() {
        return lw.err(line, "append_str_within_cap の引数が &str でない");
    }
    let src = lw.emit(Ty::Ptr, Op::Field { a: text, off: 0 });
    let tlen = lw.emit(Ty::Usize, Op::Field { a: text, off: 8 });
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let cap = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 });
    let need = arith(lw, Aop::Add, len, tlen);
    // `need` が溢れる (len + tlen < len) 形は、容量を超えるのと同じに扱う。
    let wrapped = cmp(lw, Cc::Lt, need, len);
    let over = cmp(lw, Cc::Gt, need, cap);
    let bad = lw.emit(Ty::Bool, Op::Bool(crate::kir::BoolOp::Or, wrapped, over));
    let (no, _) = lw.new_block(&[]);
    let (yes, _) = lw.new_block(&[]);
    let (join, jp) = lw.new_block(&[rt]);
    lw.branch(bad, (no, vec![]), (yes, vec![]));
    lw.cur = Some(no);
    let u = lw.unit();
    let errv = lw.emit(rt, Op::Err(u));
    lw.goto(join, vec![errv]);
    lw.cur = Some(yes);
    let base = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let dst = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off: len });
    super::own::copy_bytes(lw, dst, src, tlen);
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: need });
    let u = lw.unit();
    let okv = lw.emit(rt, Op::Ok(u));
    lw.goto(join, vec![okv]);
    lw.cur = Some(join);
    Ok(Some(jp[0]))
}

#[allow(dead_code)]
fn _unused(_: Pointee) {}
