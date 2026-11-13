//! KIR の下ろし: 所有の周りのメソッドと、構造体のフィールドの Vec / String (設計_KIR.md 13.7、kir/owned-more)。
//!
//! - フィールドの受け手 `x.f.v.m(..)`: `x` が参照かスタックスロットの構造体なら、`fieldaddr` でヘッダのアドレスを求めて `bitcast` で
//!   `ptr` にし、局所変数の Vec / String と同じメソッドを呼ぶ。`x` が値の構造体 (SSA の値) なら、フィールドの値を一時のヘッダへ写して
//!   メソッドを呼び、結果を `setfield` で `x` へ戻す (バッファの所有は動かない)。
//! - `String.push_str` / `String.push(char)` / `Vec.pop` / `Vec.clear` / `String.clear`: 要素・文字はスカラーだけ。

use std::collections::HashSet;

use super::coll::{self, Kind, Seq};
use super::{Lw, LR};
use crate::check::{items, nf, nline, nname, st, strs};
use crate::kir::{AggKind, Aop, BoolOp, Cc, Mode, Op, Pay, Pointee, Term, Ty, VId};
use crate::parser::V;
use crate::types::{from_v, Ty as Src};

/// 受け手の式が「局所変数から始まるフィールドの連なり」なら、(局所変数のスロット, フィールド名の列)。
pub(super) fn chain_of(lw: &Lw, e: &V) -> Option<(usize, Vec<String>)> {
    match nname(e) {
        "Path" => {
            let segs = strs(&nf(e)[0]);
            let s = lw.lookup(&segs[0])?;
            Some((s, segs[1..].to_vec()))
        }
        "Field" => {
            let f = nf(e);
            let (s, mut names) = chain_of(lw, &f[0])?;
            names.push(st(&f[1]));
            Some((s, names))
        }
        _ => None,
    }
}

/// 局所変数 `s` (構造体の値、またはその参照) のフィールドの連なり `names` の型表の型。利用者の構造体の定義をたどって求める。
pub(super) fn field_src(lw: &Lw, s: usize, names: &[String]) -> Option<Src> {
    let mut ai = match lw.slots[s].1 {
        Ty::Agg(i) | Ty::Ref(_, Pointee::Agg(i)) => i,
        _ => return None,
    };
    let mut src = None;
    for (k, n) in names.iter().enumerate() {
        let d = lw.aggs.borrow().defs[ai as usize].clone();
        let AggKind::Struct(fs) = &d.kind else { return None };
        let fty = fs.iter().find(|f| f.name == *n)?.ty;
        let def = lw.p.defs.iter().find(|x| x.kw == "type" && x.name == d.name)?;
        let v = &def.fields.iter().find(|f| f.0 == *n)?.1;
        src = Some(from_v(v, &HashSet::new()));
        if k + 1 < names.len() {
            let Ty::Agg(next) = fty else { return None };
            ai = next;
        }
    }
    src
}

/// フィールドのヘッダの置き場。値の構造体のフィールドは一時のヘッダに写してあり、`wb` があれば呼び出しの後に戻す。
pub(super) struct Place {
    hdr: VId,
    wb: Option<(usize, Vec<String>)>,
}

/// 局所変数 `s` のフィールド `names` (Vec / String) のヘッダのアドレス。
pub(super) fn place(lw: &mut Lw, s: usize, names: &[String], line: usize) -> LR<Place> {
    let (p, t) = lw.slots[s];
    let base = match t {
        Ty::Ref(_, Pointee::Agg(_)) => Some(lw.read_slot(s)),
        Ty::Agg(_) if lw.is_mem_val(p) => Some(p),
        _ => None,
    };
    if let Some(mut cur) = base {
        for n in names {
            cur = lw.field_addr(cur, n, line)?;
        }
        let Ty::Ref(_, Pointee::Agg(k)) = lw.vty(cur) else {
            return lw.err(line, "フィールドが Vec / String でない");
        };
        if lw.agg_def(k).name != "vec" {
            return lw.err(line, "フィールドが Vec / String でない");
        }
        let rt = lw.vty(cur);
        let hdr = lw.emit(Ty::Ptr, Op::Bitcast { src: rt, a: cur });
        return Ok(Place { hdr, wb: None });
    }
    // 値の構造体: フィールドの値を一時のヘッダへ写す (所有は構造体のまま)
    let b = lw.read_slot(s);
    let v = lw.field_path(b, names, line)?;
    let vt = lw.aggs.borrow_mut().vec_hdr();
    if lw.vty(v) != vt {
        return lw.err(line, "フィールドが Vec / String でない");
    }
    let hdr = coll::emit_alloca(lw, 24);
    coll::store_vec(lw, hdr, v);
    Ok(Place { hdr, wb: Some((s, names.to_vec())) })
}

/// 呼び出しの後、一時のヘッダの 3 語を構造体のフィールドへ戻す。
fn commit(lw: &mut Lw, pl: &Place, line: usize) -> LR<()> {
    let Some((s, names)) = &pl.wb else { return Ok(()) };
    let h = pl.hdr;
    let p = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let l = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let c = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 });
    let vt = lw.aggs.borrow_mut().vec_hdr();
    let v = lw.emit(vt, Op::Make { variant: None, fields: vec![p, l, c] });
    let mut segs = vec![String::new()];
    segs.extend(names.iter().cloned());
    lw.store_segs(*s, &segs, v, line, false)?;
    Ok(())
}

/// `x.f.m(args)` で `x.f` が Vec / String のとき。そうでなければ `None` (別の経路へ)。
pub(super) fn field_call(lw: &mut Lw, e: &V, s: usize, names: &[String], m: &str, args: &[V]) -> Option<LR<Option<VId>>> {
    if names.is_empty() {
        return None;
    }
    let src = field_src(lw, s, names)?;
    let kind = coll::kind_of_src(&src).filter(|k| matches!(k, Kind::Vec(_) | Kind::String))?;
    // `&Vec` などのフィールドは借用: ヘッダのアドレスではない
    if matches!(src, Src::Ref(..)) {
        return None;
    }
    Some(field_call_inner(lw, e, s, names, kind, m, args))
}

fn field_call_inner(lw: &mut Lw, e: &V, s: usize, names: &[String], kind: Kind, m: &str, args: &[V]) -> LR<Option<VId>> {
    let line = nline(e);
    let pl = place(lw, s, names, line)?;
    let r = coll::method_on(lw, e, line, None, kind, pl.hdr, m, args)?;
    if r.is_some() {
        commit(lw, &pl, line)?;
    }
    Ok(r)
}

/// 所有の周りの新しいメソッド。扱わない名前は `None` (既存の経路へ)。
pub(super) fn method(lw: &mut Lw, e: &V, line: usize, kind: &Kind, h: VId, m: &str, args: &[V]) -> Option<LR<Option<VId>>> {
    match (kind, m) {
        (Kind::String, "push_str") => Some(string_push_str(lw, e, line, h, args)),
        (Kind::String, "push") => Some(string_push(lw, e, line, h, args)),
        (Kind::String, "clear") => Some(clear(lw, line, kind, h, args)),
        (Kind::Vec(_), "clear") => Some(clear(lw, line, kind, h, args)),
        (Kind::Vec(_), "pop") => Some(vec_pop(lw, e, line, kind, h, args)),
        (Kind::Vec(_) | Kind::String, "get_mut") => Some(lw.err(line, "get_mut は match の対象のときだけ段階 1 の対象")),
        _ => None,
    }
}

fn usize_const(lw: &mut Lw, n: i128) -> VId {
    lw.emit(Ty::Usize, Op::Const(n))
}

fn add(lw: &mut Lw, ty: Ty, a: VId, b: VId) -> VId {
    lw.emit(ty, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a, b })
}

/// 容量が `len + extra` バイトに足りなければ、`max(cap * 2 + 4, len + extra)` に広げる (要素はバイト)。
fn reserve(lw: &mut Lw, h: VId, extra: VId) {
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let cap = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 });
    let need = add(lw, Ty::Usize, len, extra);
    let over = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::Usize, a: need, b: cap });
    let (grow, _) = lw.new_block(&[]);
    let (cont, _) = lw.new_block(&[]);
    lw.branch(over, (grow, vec![]), (cont, vec![]));
    lw.cur = Some(grow);
    let dbl = add(lw, Ty::Usize, cap, cap);
    let four = usize_const(lw, 4);
    let c2 = add(lw, Ty::Usize, dbl, four);
    let nc = lw.emit(Ty::Usize, Op::MinMax { max: true, a: c2, b: need });
    let old = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let np = lw.emit(Ty::Ptr, Op::HeapRealloc { p: old, size: nc });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: h, off: 0, v: np });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 16, v: nc });
    lw.goto(cont, vec![]);
    lw.cur = Some(cont);
}

/// `n` バイトを `src` から `dst` へ写す (重ならない)。
fn copy_bytes(lw: &mut Lw, dst: VId, src: VId, n: VId) {
    let zero = usize_const(lw, 0);
    let (hd, hp) = lw.new_block(&[Ty::Usize]);
    let (body, _) = lw.new_block(&[]);
    let (exit, _) = lw.new_block(&[]);
    lw.goto(hd, vec![zero]);
    lw.cur = Some(hd);
    let i = hp[0];
    let c = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: i, b: n });
    lw.branch(c, (body, vec![]), (exit, vec![]));
    lw.cur = Some(body);
    let sp = lw.emit(Ty::Ptr, Op::PtrAdd { p: src, off: i });
    let b = lw.emit(Ty::U8, Op::MemLoad { p: sp, off: 0 });
    let dp = lw.emit(Ty::Ptr, Op::PtrAdd { p: dst, off: i });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::U8, p: dp, off: 0, v: b });
    let one = usize_const(lw, 1);
    let i1 = add(lw, Ty::Usize, i, one);
    lw.goto(hd, vec![i1]);
    lw.cur = Some(exit);
}

/// `Result<(), AllocError>` の結果の型と、`Ok(())` の値。
fn ok_unit(lw: &mut Lw, e: &V, line: usize, what: &str) -> LR<VId> {
    let rt = lw.ty_of(e)?;
    if !matches!(rt, Ty::Res(Pay::Unit, _)) {
        return lw.err(line, format!("{} の結果の型が Result<(), AllocError> でない", what));
    }
    let u = lw.unit();
    Ok(lw.emit(rt, Op::Ok(u)))
}

/// `s.push_str(text)`: 容量を広げて末尾へバイト列を写し、`len` を進める。確保に失敗することはない (`Ok(())`)。
fn string_push_str(lw: &mut Lw, e: &V, line: usize, h: VId, args: &[V]) -> LR<Option<VId>> {
    if args.len() != 1 {
        return lw.err(line, "push_str の引数は 1 つのはず");
    }
    let Some(t) = lw.expr(&args[0])? else { return Ok(None) };
    let fat = lw.fat_ty();
    if lw.vty(t) != fat {
        return lw.err(line, "push_str の引数が &str の値でない");
    }
    let src = lw.emit(Ty::Ptr, Op::Field { a: t, off: 0 });
    let n = lw.emit(Ty::Usize, Op::Field { a: t, off: 8 });
    reserve(lw, h, n);
    let base = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let dst = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off: len });
    copy_bytes(lw, dst, src, n);
    let nl = add(lw, Ty::Usize, len, n);
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: nl });
    Ok(Some(ok_unit(lw, e, line, "push_str")?))
}

/// `s.push(c)`: `c` (u32 の Unicode スカラー値) を UTF-8 の 1〜4 バイトにして末尾へ書く。
fn string_push(lw: &mut Lw, e: &V, line: usize, h: VId, args: &[V]) -> LR<Option<VId>> {
    if args.len() != 1 {
        return lw.err(line, "push の引数は 1 つのはず");
    }
    let Some(c) = lw.expr(&args[0])? else { return Ok(None) };
    if lw.vty(c) != Ty::U32 {
        return lw.err(line, "String.push の引数が char でない");
    }
    let four = usize_const(lw, 4);
    reserve(lw, h, four);
    let base = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let at = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off: len });
    let scratch = coll::emit_alloca(lw, 4);
    let (jb, jp) = lw.new_block(&[Ty::Usize]);
    // 文字の大きさの境目 (0x80 / 0x800 / 0x10000)。n 番目の符号化の先頭のバイトは `lead | (c >> shift)`、続きは `0x80 | ((c >> shift) & 0x3F)`。
    let bounds = [0x80i128, 0x800, 0x10000];
    for (k, bound) in bounds.iter().enumerate() {
        let b = lw.emit(Ty::U32, Op::Const(*bound));
        let lt = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::U32, a: c, b });
        let (yes, _) = lw.new_block(&[]);
        let (no, _) = lw.new_block(&[]);
        lw.branch(lt, (yes, vec![]), (no, vec![]));
        lw.cur = Some(yes);
        utf8_store(lw, at, c, k + 1, scratch);
        let n = usize_const(lw, (k + 1) as i128);
        lw.goto(jb, vec![n]);
        lw.cur = Some(no);
    }
    utf8_store(lw, at, c, 4, scratch);
    let n = usize_const(lw, 4);
    lw.goto(jb, vec![n]);
    lw.cur = Some(jb);
    let nl = add(lw, Ty::Usize, len, jp[0]);
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: nl });
    Ok(Some(ok_unit(lw, e, line, "push")?))
}

/// `c` を `n` バイト (1〜4) の UTF-8 として `at` から書く。
fn utf8_store(lw: &mut Lw, at: VId, c: VId, n: usize, scratch: VId) {
    let lead = [0x00u32, 0xC0, 0xE0, 0xF0][n - 1];
    for i in 0..n {
        let shift = 6 * (n - 1 - i) as i128;
        let sh = lw.emit(Ty::U32, Op::Const(shift));
        let x = lw.emit(Ty::U32, Op::Shift { left: false, a: c, n: sh });
        let byte = if n == 1 {
            x
        } else if i == 0 {
            let l = lw.emit(Ty::U32, Op::Const(lead as i128));
            lw.emit(Ty::U32, Op::Bool(BoolOp::Or, x, l))
        } else {
            let m = lw.emit(Ty::U32, Op::Const(0x3F));
            let lo = lw.emit(Ty::U32, Op::Bool(BoolOp::And, x, m));
            let k = lw.emit(Ty::U32, Op::Const(0x80));
            lw.emit(Ty::U32, Op::Bool(BoolOp::Or, lo, k))
        };
        // u32 → u8 の切り詰め (KIR に縮める変換はない): 一時の 4 バイトへ書いて、リトルエンディアンの先頭の 1 バイトを読む。
        lw.emit(Ty::Unit, Op::MemStore { ty: Ty::U32, p: scratch, off: 0, v: byte });
        let b8 = lw.emit(Ty::U8, Op::MemLoad { p: scratch, off: 0 });
        lw.emit(Ty::Unit, Op::MemStore { ty: Ty::U8, p: at, off: i as u32, v: b8 });
    }
}

/// `v.pop()` (要素はスカラー): 空なら `None`、そうでなければ末尾を取り除いて `Some(要素)`。
fn vec_pop(lw: &mut Lw, e: &V, line: usize, kind: &Kind, h: VId, args: &[V]) -> LR<Option<VId>> {
    if !args.is_empty() {
        return lw.err(line, "pop は引数を取らない");
    }
    let Kind::Vec(Some(elem)) = kind else {
        return lw.err(line, "pop の対象は要素型の分かる Vec だけ段階 1 の対象");
    };
    let elem = *elem;
    let (Some(sz), Some(pay)) = (coll::size_of(elem), Pay::of(elem)) else {
        return lw.err(line, format!("pop の要素の型 {} は段階 1 の対象外", elem.name()));
    };
    let opt = Ty::Opt(pay);
    if lw.ty_of(e)? != opt {
        return lw.err(line, "pop の結果の型が Option<要素> でない");
    }
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let zero = usize_const(lw, 0);
    let empty = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: len, b: zero });
    let (eb, _) = lw.new_block(&[]);
    let (tb, _) = lw.new_block(&[]);
    let (jb, jp) = lw.new_block(&[opt]);
    lw.branch(empty, (eb, vec![]), (tb, vec![]));
    lw.cur = Some(eb);
    let none = lw.emit(opt, Op::Const(0));
    lw.goto(jb, vec![none]);
    lw.cur = Some(tb);
    let one = usize_const(lw, 1);
    let nl = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: len, b: one });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: nl });
    let base = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let off = if sz == 1 {
        nl
    } else {
        let k = usize_const(lw, sz as i128);
        lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: nl, b: k })
    };
    let addr = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off });
    let x = lw.emit(elem, Op::MemLoad { p: addr, off: 0 });
    let some = lw.emit(opt, Op::Some(x));
    lw.goto(jb, vec![some]);
    lw.cur = Some(jb);
    Ok(Some(jp[0]))
}

/// `v.clear()` / `s.clear()` (要素はスカラー): `len` を 0 にする (容量は残す)。
fn clear(lw: &mut Lw, line: usize, kind: &Kind, h: VId, args: &[V]) -> LR<Option<VId>> {
    if !args.is_empty() {
        return lw.err(line, "clear は引数を取らない");
    }
    match kind {
        Kind::Vec(Some(t)) if coll::size_of(*t).is_some() => {}
        Kind::Vec(_) => return lw.err(line, "clear の要素の型は段階 1 の対象外 (スカラーだけ)"),
        _ => {}
    }
    let zero = usize_const(lw, 0);
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: zero });
    Ok(Some(lw.unit()))
}

/// `match v.get_mut(i) { Some(x) => ..., None => ... }` (`v` は局所変数かフィールドの、スカラー要素の Vec / スライス)。
/// `Option<&mut T>` の値は作らず、境界の内なら要素のアドレスを `&mut T` の変数 `x` に束縛して腕を下ろす。扱わない形は `None`。
pub(super) fn match_get_mut(lw: &mut Lw, e: &V) -> Option<LR<Option<VId>>> {
    let f = nf(e);
    let scrut = &f[0];
    // 受け手と添字の式
    let (recv, idx): ((usize, Vec<String>), &V) = match nname(scrut) {
        "MCall" => {
            let g = nf(scrut);
            let a = items(&g[2]);
            if st(&g[1]) != "get_mut" || a.len() != 1 {
                return None;
            }
            (chain_of(lw, &g[0])?, &a[0])
        }
        "Call" => {
            let g = nf(scrut);
            if nname(&g[0]) != "Path" {
                return None;
            }
            let segs = strs(&nf(&g[0])[0]);
            let a = items(&g[1]);
            if segs.len() < 2 || segs[segs.len() - 1] != "get_mut" || a.len() != 1 {
                return None;
            }
            ((lw.lookup(&segs[0])?, segs[1..segs.len() - 1].to_vec()), &a[0])
        }
        _ => return None,
    };
    // 腕は `Some(名前 | _)` と `None | _` の 2 つだけ
    let arms: Vec<(&V, &V)> = items(&f[1]).iter().filter_map(|a| match a {
        V::L(xs) if xs.len() == 2 => Some((&xs[0], &xs[1])),
        _ => None,
    }).collect();
    if arms.len() != 2 || arms.len() != items(&f[1]).len() {
        return None;
    }
    let is_some = |p: &V| nname(p) == "PVariant" && strs(&nf(p)[0]) == ["Some"] && items(&nf(p)[1]).len() == 1;
    let is_none = |p: &V| (nname(p) == "PPath" && strs(&nf(p)[0]) == ["None"]) || nname(p) == "PWild";
    let (some_i, none_i) = if is_some(arms[0].0) && is_none(arms[1].0) {
        (0, 1)
    } else if is_some(arms[1].0) && is_none(arms[0].0) {
        (1, 0)
    } else {
        return None;
    };
    let bind = {
        let sub = &items(&nf(arms[some_i].0)[1])[0];
        match nname(sub) {
            "PBind" => Some(st(&nf(sub)[0])),
            "PWild" => None,
            _ => return None,
        }
    };
    Some(get_mut_match(lw, e, recv, idx, &arms, some_i, none_i, bind))
}

#[allow(clippy::too_many_arguments)]
fn get_mut_match(lw: &mut Lw, e: &V, recv: (usize, Vec<String>), idx: &V, arms: &[(&V, &V)], some_i: usize, none_i: usize, bind: Option<String>) -> LR<Option<VId>> {
    let line = nline(e);
    let (s, names) = recv;
    // 受け手の置き場と要素の型
    let (seq, elem) = if names.is_empty() {
        match lw.coll.kinds.get(&s).cloned() {
            Some(Kind::Vec(Some(t))) => (Seq::Hdr(lw.slots[s].0), t),
            Some(Kind::Slice(Some(t))) => (Seq::Fat(lw.slots[s].0), t),
            _ => return lw.err(line, "get_mut の対象は要素型の分かる Vec・スライスだけ段階 1 の対象"),
        }
    } else {
        let Some(src) = field_src(lw, s, &names) else { return lw.err(line, "get_mut の対象のフィールドの型が分からない") };
        let Some(Kind::Vec(Some(t))) = coll::kind_of_src(&src) else {
            return lw.err(line, "get_mut の対象は要素型の分かる Vec だけ段階 1 の対象");
        };
        let pl = place(lw, s, &names, line)?;
        (Seq::Hdr(pl.hdr), t)
    };
    let (Some(sz), Some(p)) = (coll::size_of(elem), Pointee::of(elem)) else {
        return lw.err(line, format!("get_mut の要素の型 {} は段階 1 の対象外", elem.name()));
    };
    let res: Option<Ty> = match lw.src_ty(e)? {
        Src::Never => None,
        t => Some(lw.conv_t(&t).ok_or_else(|| format!("行 {}: match の型 {:?} は段階 1 の対象外", line, t))?),
    };
    let carry = matches!(res, Some(t) if t != Ty::Unit);
    let bodies: Vec<&V> = arms.iter().map(|(_, b)| *b).collect();
    let slots = lw.assigned(&bodies);
    let slot_tys: Vec<Ty> = slots.iter().map(|x| lw.slots[*x].1).collect();
    let mut params: Vec<Ty> = if carry { res.into_iter().collect() } else { vec![] };
    params.extend(slot_tys.iter().copied());
    let Some(i) = lw.expr(idx)? else { return Ok(None) };
    if lw.vty(i) != Ty::Usize {
        return lw.err(line, "get_mut の添字が usize ではない");
    }
    let (len, base) = seq.len_and_base(lw, None);
    let inside = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: i, b: len });
    let (some_b, _) = lw.new_block(&[]);
    let (none_b, _) = lw.new_block(&[]);
    let (jb, jp) = lw.new_block(&params);
    lw.branch(inside, (some_b, vec![]), (none_b, vec![]));
    let saved = lw.slots.clone();
    for (blk, ai) in [(some_b, some_i), (none_b, none_i)] {
        lw.cur = Some(blk);
        lw.slots = saved.clone();
        lw.scopes.push(vec![]);
        if ai == some_i {
            let off = if sz == 1 {
                i
            } else {
                let k = usize_const(lw, sz as i128);
                lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: i, b: k })
            };
            let addr = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off });
            if let Some(name) = &bind {
                let rt = Ty::Ref(true, p);
                let r = lw.emit(rt, Op::Bitcast { src: Ty::Ptr, a: addr });
                lw.bind(name, r, rt);
            }
        }
        if let Some(v) = lw.expr(arms[ai].1)? {
            if let Some(t) = res {
                if carry && lw.vty(v) != t {
                    return lw.err(line, "match の腕の型が結果の型と違う");
                }
            }
            coll::block_end(lw);
            let mut args: Vec<VId> = if carry { vec![v] } else { vec![] };
            args.extend(lw.vals_of(&slots));
            lw.goto(jb, args);
        } else {
            coll::block_end(lw);
        }
        lw.scopes.pop();
    }
    lw.slots = saved;
    if lw.blocks[jb].preds == 0 {
        return Ok(None);
    }
    lw.cur = Some(jb);
    let off = carry as usize;
    for (k, x) in slots.iter().enumerate() {
        lw.slots[*x] = (jp[off + k], slot_tys[k]);
    }
    let _ = Term::Unreachable;
    Ok(Some(if carry { jp[0] } else { lw.unit() }))
}
