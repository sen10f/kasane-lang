//! KIR の下ろし: `BtreeMap<String, V>` / `BtreeMap<&str, V>` (設計_KIR.md 16.4、kir/string-keys)。`kir_lower.rs` の子モジュール。
//!
//! 表現: ヘッダは `BtreeMap` と同じ 24 バイト `{buf, len, cap}`。`buf` は組の配列で、1 組は キー + 値 (8 バイト)。
//! キーは `&str` なら (ptr, len) の 16 バイト、`String` なら (ptr, len, cap) の 24 バイトで、表が所有する (組を取り除くとき・表を
//! 解放するときにバッファを解放する)。組はキーのバイト列の辞書順 (`Ordering` の `String` の順と同じ) に並ぶ。
//! 検索は二分探索、挿入と削除は後ろの組を 1 つずらす。

use std::collections::BTreeSet;

use crate::check::{nf, nlineno as nline, nname, st};
use crate::kir::{Cc, Func, Op, Pay, Term, Ty, VId};
use crate::parser::V;

use super::coll::{self, Kind};
use super::map_lower::Fb;
use super::{Lw, LR};

#[derive(Clone, Copy)]
struct Sh {
    own: bool,
}

impl Sh {
    fn of(lw: &Lw, kt: Ty) -> Sh {
        Sh { own: lw.aggs.borrow().is_vec(kt) }
    }
    fn ksz(self) -> i128 {
        if self.own {
            24
        } else {
            16
        }
    }
    fn esz(self) -> i128 {
        self.ksz() + 8
    }
    fn tag(self) -> &'static str {
        if self.own {
            "bo"
        } else {
            "bs"
        }
    }
    fn name(self, vt: Ty, op: &str) -> String {
        format!("ksn.{}.{}.{}", self.tag(), vt.name(), op)
    }
    fn lname(self) -> String {
        format!("ksn.{}.lower", self.tag())
    }
    /// 組を書くキーの語の型 (`insert` の引数)。
    fn key_tys(self) -> Vec<Ty> {
        if self.own {
            vec![Ty::Ptr, Ty::Usize, Ty::Usize]
        } else {
            vec![Ty::Ptr, Ty::Usize]
        }
    }
}

const CMP: &str = "ksn.bs.cmp";

/// `cmp(p1, l1, p2, l2) -> usize`: バイト列の辞書順。0 = 前者が小さい、1 = 等しい、2 = 前者が大きい。
fn cmp_fn() -> Func {
    let mut b = Fb::new(&[Ty::Ptr, Ty::Usize, Ty::Ptr, Ty::Usize]);
    let (p1, l1, p2, l2): (VId, VId, VId, VId) = (0, 1, 2, 3);
    let lt = b.cmp(Cc::Lt, l1, l2);
    let (a, _) = b.block(&[]);
    let (c, _) = b.block(&[]);
    let (lp, lv) = b.block(&[Ty::Usize, Ty::Usize]);
    b.br_if(lt, a, c);
    let z = b.c(Ty::Usize, 0);
    b.at(a);
    b.goto(lp, vec![z, l1]);
    b.at(c);
    b.goto(lp, vec![z, l2]);
    // lp(i, min)
    b.at(lp);
    let (i, min) = (lv[0], lv[1]);
    let more = b.cmp(Cc::Lt, i, min);
    let (body, _) = b.block(&[]);
    let (tail, _) = b.block(&[]);
    b.br_if(more, body, tail);
    b.at(body);
    let qa = b.padd(p1, i);
    let qb = b.padd(p2, i);
    let ca = b.load(Ty::U8, qa, 0);
    let cb = b.load(Ty::U8, qb, 0);
    let less = b.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::U8, a: ca, b: cb });
    let (r0, _) = b.block(&[]);
    let (n1, _) = b.block(&[]);
    b.br_if(less, r0, n1);
    b.at(r0);
    let z0 = b.c(Ty::Usize, 0);
    b.term(Term::Ret(z0));
    b.at(n1);
    let gt = b.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::U8, a: ca, b: cb });
    let (r2, _) = b.block(&[]);
    let (nx, _) = b.block(&[]);
    b.br_if(gt, r2, nx);
    b.at(r2);
    let two = b.c(Ty::Usize, 2);
    b.term(Term::Ret(two));
    b.at(nx);
    let one = b.c(Ty::Usize, 1);
    let i1 = b.add(i, one);
    b.goto(lp, vec![i1, min]);
    // 共通の接頭辞が同じ: 長さで決める
    b.at(tail);
    let (s0, _) = b.block(&[]);
    let (s1, _) = b.block(&[]);
    let (s2, _) = b.block(&[]);
    let (s3, _) = b.block(&[]);
    let lt = b.cmp(Cc::Lt, l1, l2);
    b.br_if(lt, s0, s1);
    b.at(s0);
    let z0 = b.c(Ty::Usize, 0);
    b.term(Term::Ret(z0));
    b.at(s1);
    let eq = b.cmp(Cc::Eq, l1, l2);
    b.br_if(eq, s2, s3);
    b.at(s2);
    let one = b.c(Ty::Usize, 1);
    b.term(Term::Ret(one));
    b.at(s3);
    let two = b.c(Ty::Usize, 2);
    b.term(Term::Ret(two));
    b.build(CMP, &[], Ty::Usize)
}

fn ent_at(b: &mut Fb, sh: Sh, buf: VId, i: VId) -> VId {
    let k = b.c(Ty::Usize, sh.esz());
    let o = b.mul(i, k);
    b.padd(buf, o)
}

/// 組 `ent` のキーと (kp, kl) を比べる: `cmp(ent のキー, kp/kl)`。
fn cmp_ent(b: &mut Fb, ent: VId, kp: VId, kl: VId) -> VId {
    let ep = b.load(Ty::Ptr, ent, 0);
    let el = b.load(Ty::Usize, ent, 8);
    b.emit(Ty::Usize, Op::Call { callee: CMP.to_string(), args: vec![ep, el, kp, kl] })
}

/// `lower(h, kp, kl) -> usize`: キー以上になる最初の添字。
fn lower_fn(sh: Sh) -> Func {
    let mut b = Fb::new(&[Ty::Ptr, Ty::Ptr, Ty::Usize]);
    let len = b.load(Ty::Usize, 0, 8);
    let zero = b.c(Ty::Usize, 0);
    let (lp, lv) = b.block(&[Ty::Usize, Ty::Usize]);
    let (body, _) = b.block(&[]);
    let (done, dv) = b.block(&[Ty::Usize]);
    b.goto(lp, vec![zero, len]);
    b.at(lp);
    let (lo, hi) = (lv[0], lv[1]);
    let more = b.cmp(Cc::Lt, lo, hi);
    b.term(Term::BrIf(more, (body, vec![]), (done, vec![lo])));
    b.at(body);
    let buf = b.load(Ty::Ptr, 0, 0);
    let d = b.sub(hi, lo);
    let one32 = b.emit(Ty::U32, Op::Const(1));
    let half = b.emit(Ty::Usize, Op::Shift { left: false, a: d, n: one32 });
    let mid = b.add(lo, half);
    let ent = ent_at(&mut b, sh, buf, mid);
    let c = cmp_ent(&mut b, ent, 1, 2);
    let z = b.c(Ty::Usize, 0);
    let less = b.cmp(Cc::Eq, c, z);
    let (yes, _) = b.block(&[]);
    let (no, _) = b.block(&[]);
    b.br_if(less, yes, no);
    b.at(yes);
    let one = b.c(Ty::Usize, 1);
    let m1 = b.add(mid, one);
    b.goto(lp, vec![m1, hi]);
    b.at(no);
    b.goto(lp, vec![lo, mid]);
    b.at(done);
    b.term(Term::Ret(dv[0]));
    b.build(&sh.lname(), &[], Ty::Usize)
}

/// 添字 `pos` (`< len`) の組のキーが (kp, kl) と等しければ、その組の番地を返す (続きのブロックで)。違えば `miss` へ。
fn found(b: &mut Fb, sh: Sh, pos: VId, kp: VId, kl: VId, miss: usize) -> VId {
    let buf = b.load(Ty::Ptr, 0, 0);
    let ent = ent_at(b, sh, buf, pos);
    let c = cmp_ent(b, ent, kp, kl);
    let one = b.c(Ty::Usize, 1);
    let eq = b.cmp(Cc::Eq, c, one);
    let (hit, _) = b.block(&[]);
    b.br_if(eq, hit, miss);
    b.at(hit);
    ent
}

fn opt_of(vt: Ty) -> Ty {
    Ty::Opt(Pay::of(vt).expect("BtreeMap の値は Option のペイロードになる"))
}

/// `get(h, kp, kl) -> Option<V>`。
fn get_fn(sh: Sh, vt: Ty) -> Func {
    let opt = opt_of(vt);
    let mut b = Fb::new(&[Ty::Ptr, Ty::Ptr, Ty::Usize]);
    let pos = b.emit(Ty::Usize, Op::Call { callee: sh.lname(), args: vec![0, 1, 2] });
    let len = b.load(Ty::Usize, 0, 8);
    let inside = b.cmp(Cc::Lt, pos, len);
    let (chk, _) = b.block(&[]);
    let (no, _) = b.block(&[]);
    let (join, jv) = b.block(&[opt]);
    b.br_if(inside, chk, no);
    b.at(chk);
    let ent = found(&mut b, sh, pos, 1, 2, no);
    let v = b.load(vt, ent, sh.ksz());
    let s = b.emit(opt, Op::Some(v));
    b.goto(join, vec![s]);
    b.at(no);
    let n = b.emit(opt, Op::Const(0));
    b.goto(join, vec![n]);
    b.at(join);
    b.term(Term::Ret(jv[0]));
    b.build(&sh.name(vt, "get"), &[], opt)
}

/// `insert(h, key.., val) -> Option<V>`: 同じキーがあれば値を替えて古い値を返す (渡したキーは `String` なら解放する)。なければ挿入して `None`。
fn insert_fn(sh: Sh, vt: Ty) -> Func {
    let opt = opt_of(vt);
    let mut tys = vec![Ty::Ptr];
    tys.extend(sh.key_tys());
    tys.push(vt);
    let nk = sh.key_tys().len() as u32;
    let mut b = Fb::new(&tys);
    let val: VId = 1 + nk;
    let (kp, kl): (VId, VId) = (1, 2);
    let pos = b.emit(Ty::Usize, Op::Call { callee: sh.lname(), args: vec![0, kp, kl] });
    let len = b.load(Ty::Usize, 0, 8);
    let inside = b.cmp(Cc::Lt, pos, len);
    let (chk, _) = b.block(&[]);
    let (ins, _) = b.block(&[]);
    let (join, jv) = b.block(&[opt]);
    b.br_if(inside, chk, ins);
    b.at(chk);
    let ent = found(&mut b, sh, pos, kp, kl, ins);
    let old = b.load(vt, ent, sh.ksz());
    b.store(vt, ent, sh.ksz(), val);
    if sh.own {
        b.emit(Ty::Unit, Op::HeapFree(kp));
    }
    let s = b.emit(opt, Op::Some(old));
    b.goto(join, vec![s]);
    b.at(ins);
    let cap = b.load(Ty::Usize, 0, 16);
    let full = b.cmp(Cc::Ge, len, cap);
    let (grow, _) = b.block(&[]);
    let (moved, _) = b.block(&[]);
    b.br_if(full, grow, moved);
    b.at(grow);
    let oldbuf = b.load(Ty::Ptr, 0, 0);
    let two = b.c(Ty::Usize, 2);
    let c2 = b.mul(cap, two);
    let four = b.c(Ty::Usize, 4);
    let ncap = b.add(c2, four);
    let k = b.c(Ty::Usize, sh.esz());
    let bytes = b.mul(ncap, k);
    let np = b.emit(Ty::Ptr, Op::HeapRealloc { p: oldbuf, size: bytes });
    b.store(Ty::Ptr, 0, 0, np);
    b.store(Ty::Usize, 0, 16, ncap);
    b.goto(moved, vec![]);
    b.at(moved);
    let (sh_l, sp) = b.block(&[Ty::Usize]);
    let (shb, _) = b.block(&[]);
    let (after, _) = b.block(&[]);
    b.goto(sh_l, vec![len]);
    b.at(sh_l);
    let i = sp[0];
    let more = b.cmp(Cc::Gt, i, pos);
    b.br_if(more, shb, after);
    b.at(shb);
    let one = b.c(Ty::Usize, 1);
    let im = b.sub(i, one);
    let buf = b.load(Ty::Ptr, 0, 0);
    let src = ent_at(&mut b, sh, buf, im);
    let dst = ent_at(&mut b, sh, buf, i);
    for w in 0..(sh.esz() / 8) {
        let x = b.load(Ty::Usize, src, w * 8);
        b.store(Ty::Usize, dst, w * 8, x);
    }
    b.goto(sh_l, vec![im]);
    b.at(after);
    let buf = b.load(Ty::Ptr, 0, 0);
    let ent = ent_at(&mut b, sh, buf, pos);
    b.store(Ty::Ptr, ent, 0, kp);
    b.store(Ty::Usize, ent, 8, kl);
    if sh.own {
        b.store(Ty::Usize, ent, 16, 3);
    }
    b.store(vt, ent, sh.ksz(), val);
    let one = b.c(Ty::Usize, 1);
    let nl = b.add(len, one);
    b.store(Ty::Usize, 0, 8, nl);
    let n = b.emit(opt, Op::Const(0));
    b.goto(join, vec![n]);
    b.at(join);
    b.term(Term::Ret(jv[0]));
    b.build(&sh.name(vt, "insert"), &["alloc"], opt)
}

/// `remove(h, kp, kl) -> Option<V>`: 見つかれば (`String` のキーを解放し) 後ろの組を 1 つ前へずらして古い値を返す。
fn remove_fn(sh: Sh, vt: Ty) -> Func {
    let opt = opt_of(vt);
    let mut b = Fb::new(&[Ty::Ptr, Ty::Ptr, Ty::Usize]);
    let pos = b.emit(Ty::Usize, Op::Call { callee: sh.lname(), args: vec![0, 1, 2] });
    let len = b.load(Ty::Usize, 0, 8);
    let inside = b.cmp(Cc::Lt, pos, len);
    let (chk, _) = b.block(&[]);
    let (no, _) = b.block(&[]);
    let (join, jv) = b.block(&[opt]);
    b.br_if(inside, chk, no);
    b.at(chk);
    let ent = found(&mut b, sh, pos, 1, 2, no);
    let old = b.load(vt, ent, sh.ksz());
    if sh.own {
        let kp = b.load(Ty::Ptr, ent, 0);
        b.emit(Ty::Unit, Op::HeapFree(kp));
    }
    let (sh_l, sp) = b.block(&[Ty::Usize]);
    let (shb, _) = b.block(&[]);
    let (after, _) = b.block(&[]);
    b.goto(sh_l, vec![pos]);
    b.at(sh_l);
    let i = sp[0];
    let one = b.c(Ty::Usize, 1);
    let i1 = b.add(i, one);
    let more = b.cmp(Cc::Lt, i1, len);
    b.br_if(more, shb, after);
    b.at(shb);
    let buf = b.load(Ty::Ptr, 0, 0);
    let src = ent_at(&mut b, sh, buf, i1);
    let dst = ent_at(&mut b, sh, buf, i);
    for w in 0..(sh.esz() / 8) {
        let x = b.load(Ty::Usize, src, w * 8);
        b.store(Ty::Usize, dst, w * 8, x);
    }
    b.goto(sh_l, vec![i1]);
    b.at(after);
    let nl = b.sub(len, one);
    b.store(Ty::Usize, 0, 8, nl);
    let s = b.emit(opt, Op::Some(old));
    b.goto(join, vec![s]);
    b.at(no);
    let n = b.emit(opt, Op::Const(0));
    b.goto(join, vec![n]);
    b.at(join);
    b.term(Term::Ret(jv[0]));
    b.build(&sh.name(vt, "remove"), &[], opt)
}

/// `drop(h)`: `String` のキーの表のキーのバッファを解放する (表の `buf` は呼び手が解放する)。`buf` が null なら何もしない。
fn drop_fn(sh: Sh) -> Func {
    let mut b = Fb::new(&[Ty::Ptr]);
    let (go, _) = b.block(&[]);
    let (done, _) = b.block(&[]);
    let buf = b.load(Ty::Ptr, 0, 0);
    let np = b.emit(Ty::Ptr, Op::Const(0));
    let isnull = b.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Ptr, a: buf, b: np });
    b.br_if(isnull, done, go);
    b.at(go);
    let len = b.load(Ty::Usize, 0, 8);
    let z = b.c(Ty::Usize, 0);
    let (lp, lv) = b.block(&[Ty::Usize]);
    let (body, _) = b.block(&[]);
    b.goto(lp, vec![z]);
    b.at(lp);
    let more = b.cmp(Cc::Lt, lv[0], len);
    b.br_if(more, body, done);
    b.at(body);
    let ent = ent_at(&mut b, sh, buf, lv[0]);
    let kp = b.load(Ty::Ptr, ent, 0);
    b.emit(Ty::Unit, Op::HeapFree(kp));
    let one = b.c(Ty::Usize, 1);
    let i1 = b.add(lv[0], one);
    b.goto(lp, vec![i1]);
    b.at(done);
    let u = b.emit(Ty::Unit, Op::Const(0));
    b.term(Term::Ret(u));
    b.build(&format!("ksn.{}.drop", sh.tag()), &["alloc"], Ty::Unit)
}

fn need(lw: &mut Lw, sh: Sh, vt: Ty, ops: &[&str]) {
    lw.coll.extra.push((cmp_fn(), BTreeSet::new()));
    lw.coll.extra.push((lower_fn(sh), [CMP.to_string()].into_iter().collect()));
    let deps: BTreeSet<String> = [sh.lname(), CMP.to_string()].into_iter().collect();
    for op in ops {
        let f = match *op {
            "get" => get_fn(sh, vt),
            "remove" => remove_fn(sh, vt),
            _ => insert_fn(sh, vt),
        };
        lw.coll.extra.push((f, deps.clone()));
        lw.callees.insert(sh.name(vt, op));
    }
}

/// 表のスコープの終わり (`drop_hdr`) で呼ぶ: `String` キーの `BtreeMap` の局所変数なら、キーを解放する命令列を置く。
pub(crate) fn drop_owned_keys(lw: &mut Lw, hdr: VId, kt: Ty) {
    let sh = Sh::of(lw, kt);
    if !sh.own {
        return;
    }
    lw.coll.extra.push((drop_fn(sh), BTreeSet::new()));
    lw.callees.insert(format!("ksn.{}.drop", sh.tag()));
    lw.emit(Ty::Unit, Op::Call { callee: format!("ksn.{}.drop", sh.tag()), args: vec![hdr] });
}

/// 値の語 (整数・bool・char)。`&k` などは評価して参照を外す。
fn scalar(lw: &mut Lw, a: &V, line: usize) -> LR<Option<VId>> {
    let operand = if nname(a) == "Unary" && st(&nf(a)[0]) == "&" { &nf(a)[1] } else { a };
    let Some(mut v) = lw.expr(operand)? else { return Ok(None) };
    if let Ty::Ref(_, p) = lw.vty(v) {
        v = lw.emit(p.ty(), Op::Load(v));
    }
    let t = lw.vty(v);
    if !matches!(t, Ty::I8 | Ty::U8 | Ty::Bool | Ty::I16 | Ty::U16 | Ty::I32 | Ty::U32 | Ty::I64 | Ty::U64 | Ty::Isize | Ty::Usize) {
        return lw.err(line, format!("BtreeMap の値の型 {} は段階 1 の対象外 (整数・bool・char だけ)", t.name()));
    }
    Ok(Some(v))
}

/// 検索のキー (`&k`) から (ptr, len)。`&String` はヘッダから、`&str` は fat から。
fn lookup_key(lw: &mut Lw, a: &V, line: usize) -> LR<Option<(VId, VId)>> {
    let Some(v) = lw.expr(a)? else { return Ok(None) };
    let t = lw.vty(v);
    if t == lw.fat_ty() {
        let p = lw.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
        let n = lw.emit(Ty::Usize, Op::Field { a: v, off: 8 });
        return Ok(Some((p, n)));
    }
    if t != Ty::Ptr {
        return lw.err(line, format!("BtreeMap<String, _> の検索のキーの型 {} は段階 1 の対象外 (&String か &str だけ)", t.name()));
    }
    let p = lw.emit(Ty::Ptr, Op::MemLoad { p: v, off: 0 });
    let n = lw.emit(Ty::Usize, Op::MemLoad { p: v, off: 8 });
    Ok(Some((p, n)))
}

/// 文字列のキーの `BtreeMap` のメソッド。`kt` はキーの型 (`Ty::Ptr` = `&str`、`vec` = `String`)。
pub(crate) fn method(lw: &mut Lw, e: &V, slot: Option<usize>, kind: &Kind, h: VId, m: &str, args: &[V], kt: Ty) -> LR<Option<VId>> {
    let line = nline(e);
    let sh = Sh::of(lw, kt);
    if m == "len" && args.is_empty() {
        return Ok(Some(lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 })));
    }
    if m == "is_empty" && args.is_empty() {
        let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
        let z = lw.emit(Ty::Usize, Op::Const(0));
        return Ok(Some(lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: len, b: z })));
    }
    let vt = match kind {
        Kind::BMap(Some((_, vt))) => *vt,
        Kind::BMap(None) if m == "insert" => {
            // 値の型は insert の値から決める (下で `learn`)
            Ty::Unit
        }
        _ => return lw.err(line, "BtreeMap のキー・値の型が決まらない"),
    };
    match (m, args.len()) {
        ("insert", 2) => {
            let (kv, words): (Option<VId>, Vec<VId>) = if sh.own {
                match lw.src_ty(&args[0]) {
                    Ok(crate::types::Ty::Named(n, _)) if n == "alloc.string.String" => {}
                    _ => return lw.err(line, "BtreeMap のキーの型は段階 1 の対象外 (整数・bool・char・&str・String だけ)"),
                }
                let Some(v) = lw.expr(&args[0])? else { return Ok(None) };
                coll::tmp_hold(lw, v);
                let p = lw.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
                let n = lw.emit(Ty::Usize, Op::Field { a: v, off: 8 });
                let c = lw.emit(Ty::Usize, Op::Field { a: v, off: 16 });
                (Some(v), vec![p, n, c])
            } else {
                let Some(v) = lw.expr(&args[0])? else { return Ok(None) };
                if lw.vty(v) != lw.fat_ty() {
                    return lw.err(line, "BtreeMap のキーの型は段階 1 の対象外 (整数・bool・char・&str・String だけ)");
                }
                let p = lw.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
                let n = lw.emit(Ty::Usize, Op::Field { a: v, off: 8 });
                (None, vec![p, n])
            };
            let val = scalar(lw, &args[1], line);
            if let Some(kv) = kv {
                coll::tmp_release(lw, &[kv]);
            }
            let Some(val) = val? else { return Ok(None) };
            let vt2 = lw.vty(val);
            if vt != Ty::Unit && vt != vt2 {
                return lw.err(line, "insert の値の型が BtreeMap の値の型と違う");
            }
            if let (Kind::BMap(None), Some(s)) = (kind, slot) {
                lw.coll.kinds.insert(s, Kind::BMap(Some((kt, vt2))));
            }
            let rt = lw.ty_of(e)?;
            let Ty::Res(Pay::Opt(inner), _) = rt else {
                return lw.err(line, "insert の結果の型が Result<Option<V>, AllocError> でない");
            };
            need(lw, sh, vt2, &["insert"]);
            let mut ca = vec![h];
            ca.extend(words);
            ca.push(val);
            let o = lw.emit(Ty::Opt(*inner), Op::Call { callee: sh.name(vt2, "insert"), args: ca });
            Ok(Some(lw.emit(rt, Op::Ok(o))))
        }
        ("get", 1) | ("contains_key", 1) | ("remove", 1) => {
            if vt == Ty::Unit {
                return lw.err(line, "BtreeMap のキー・値の型が決まらない");
            }
            let Some((p, n)) = lookup_key(lw, &args[0], line)? else { return Ok(None) };
            let op = if m == "remove" { "remove" } else { "get" };
            need(lw, sh, vt, &[op]);
            let Some(vp) = Pay::of(vt) else { return lw.err(line, "BtreeMap の値の型が Option に入らない") };
            let o = lw.emit(Ty::Opt(vp), Op::Call { callee: sh.name(vt, op), args: vec![h, p, n] });
            if m == "contains_key" {
                return Ok(Some(lw.emit(Ty::Bool, Op::Tag(o))));
            }
            Ok(Some(o))
        }
        _ => lw.err(line, format!("BtreeMap のメソッド {} は段階 1 の対象外", m)),
    }
}

// ------------------------------------------------ 文字列の順序 (`<` / `<=` / `>` / `>=` / `cmp` / `partial_cmp`)

use crate::kir::Pointee;
use crate::types::Ty as Src;

/// 文字列の型 (`String` / `&String` / `&str`) か。
fn stringish(s: &Src) -> bool {
    match s {
        Src::Named(n, _) => n == "alloc.string.String",
        Src::Prim(p) => p == "str",
        Src::Ref(_, i) => stringish(i),
        _ => false,
    }
}

impl<'a> Lw<'a> {
    /// 文字列 (`String` の局所変数・`&String`・`&str`) の式 `e` を、所有を移さずに (ptr, len) にする。
    fn str_words(&mut self, e: &V, line: usize) -> LR<Option<(VId, VId)>> {
        let operand = if nname(e) == "Unary" && st(&nf(e)[0]) == "&" { &nf(e)[1] } else { e };
        if let Some(s) = coll::local_slot(self, operand) {
            if matches!(self.coll.kinds.get(&s), Some(Kind::String)) {
                let h = self.slots[s].0;
                let p = self.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
                let n = self.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
                return Ok(Some((p, n)));
            }
        }
        let Some(v) = self.expr_peek(operand)? else { return Ok(None) };
        let t = self.vty(v);
        if t == self.fat_ty() {
            let p = self.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
            let n = self.emit(Ty::Usize, Op::Field { a: v, off: 8 });
            return Ok(Some((p, n)));
        }
        if t == Ty::Ptr {
            let p = self.emit(Ty::Ptr, Op::MemLoad { p: v, off: 0 });
            let n = self.emit(Ty::Usize, Op::MemLoad { p: v, off: 8 });
            return Ok(Some((p, n)));
        }
        if let Ty::Ref(_, Pointee::Agg(_)) = t {
            let b = self.emit(Ty::Ptr, Op::Bitcast { src: t, a: v });
            let p = self.emit(Ty::Ptr, Op::MemLoad { p: b, off: 0 });
            let n = self.emit(Ty::Usize, Op::MemLoad { p: b, off: 8 });
            return Ok(Some((p, n)));
        }
        if self.aggs.borrow().is_vec(t) {
            // 値で読んだ String (フィールドなど): 移していないので、所有は元のまま
            let p = self.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
            let n = self.emit(Ty::Usize, Op::Field { a: v, off: 8 });
            return Ok(Some((p, n)));
        }
        self.err(line, format!("文字列の比較の被演算子の型 {} は段階 1 の対象外", t.name()))
    }

    /// `ksn.bs.cmp` を呼ぶ: 0 = 小さい、1 = 等しい、2 = 大きい (バイト列の辞書順)。
    fn str_cmp3(&mut self, a: (VId, VId), b: (VId, VId)) -> VId {
        self.coll.extra.push((cmp_fn(), BTreeSet::new()));
        self.callees.insert(CMP.to_string());
        self.emit(Ty::Usize, Op::Call { callee: CMP.to_string(), args: vec![a.0, a.1, b.0, b.1] })
    }

    /// `l < r` などで両辺が文字列のとき。そうでなければ `None`。
    pub(super) fn string_order(&mut self, l: &V, r: &V, op: &str, line: usize) -> LR<Option<VId>> {
        let (Ok(sl), Ok(sr)) = (self.src_ty(l), self.src_ty(r)) else { return Ok(None) };
        if !stringish(&sl) || !stringish(&sr) {
            return Ok(None);
        }
        let Some(a) = self.str_words(l, line)? else { return Ok(None) };
        let Some(b) = self.str_words(r, line)? else { return Ok(None) };
        let c = self.str_cmp3(a, b);
        let k = |lw: &mut Lw, n: i128| lw.emit(Ty::Usize, Op::Const(n));
        Ok(Some(match op {
            "<" => {
                let z = k(self, 0);
                self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: c, b: z })
            }
            "<=" => {
                let t = k(self, 2);
                self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ne, ty: Ty::Usize, a: c, b: t })
            }
            ">" => {
                let t = k(self, 2);
                self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: c, b: t })
            }
            _ => {
                let z = k(self, 0);
                self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ne, ty: Ty::Usize, a: c, b: z })
            }
        }))
    }

    /// `a.cmp(&b)` / `a.partial_cmp(&b)` (受け手は `String` / `&str`、`h` は `String` のヘッダか `&str` の fat)。
    /// 結果は `core.cmp.Ordering` (列挙型。`Less` `Equal` `Greater` の順) か、その `Option`。
    pub(super) fn string_cmp_method(&mut self, e: &V, kind: &Kind, h: VId, m: &str, args: &[V]) -> LR<Option<VId>> {
        let line = nline(e);
        if args.len() != 1 {
            return self.err(line, format!("メソッド {} の引数は 1 個のはず", m));
        }
        let a = if *kind == Kind::Str {
            let p = self.emit(Ty::Ptr, Op::Field { a: h, off: 0 });
            let n = self.emit(Ty::Usize, Op::Field { a: h, off: 8 });
            (p, n)
        } else {
            let p = self.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
            let n = self.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
            (p, n)
        };
        let Some(b) = self.str_words(&args[0], line)? else { return Ok(None) };
        let rt = self.ty_of(e)?;
        let ord = match rt {
            Ty::Opt(_) => {
                let file = self.def.file.clone();
                self.aggs.borrow_mut().lower_ty(self.p, &file, &Src::Named("core.cmp.Ordering".to_string(), vec![]))?
            }
            t => t,
        };
        let c = self.str_cmp3(a, b);
        let (lt, _) = self.new_block(&[]);
        let (ge, _) = self.new_block(&[]);
        let (eq, _) = self.new_block(&[]);
        let (gt, _) = self.new_block(&[]);
        let (jb, jp) = self.new_block(&[ord]);
        let z = self.emit(Ty::Usize, Op::Const(0));
        let is_lt = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: c, b: z });
        self.branch(is_lt, (lt, vec![]), (ge, vec![]));
        self.cur = Some(ge);
        let one = self.emit(Ty::Usize, Op::Const(1));
        let is_eq = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: c, b: one });
        self.branch(is_eq, (eq, vec![]), (gt, vec![]));
        for (blk, idx) in [(lt, 0u32), (eq, 1), (gt, 2)] {
            self.cur = Some(blk);
            let v = self.emit(ord, Op::Make { variant: Some(idx), fields: vec![] });
            self.goto(jb, vec![v]);
        }
        self.cur = Some(jb);
        let o = jp[0];
        Ok(Some(match rt {
            Ty::Opt(_) => self.emit(rt, Op::Some(o)),
            _ => o,
        }))
    }
}

// ------------------------------------------------ `const` の `&str` への直接のメソッド呼び出し (`K.as_bytes()` / `K.split(" ")`)

impl<'a> Lw<'a> {
    /// `K.m(args)` (`K` は `&str` の `const`)。定数の値 (`&str` の fat) を受け手にして、`&str` のメソッドとして下ろす。
    /// `K` が定数でなければ `None` (親の処理へ)。
    pub(super) fn const_str_call(&mut self, e: &V) -> Option<LR<Option<VId>>> {
        let f = nf(e);
        if nname(&f[0]) != "Path" {
            return None;
        }
        let segs = crate::check::strs(&nf(&f[0])[0]);
        if segs.len() < 2 || self.lookup(&segs[0]).is_some() {
            return None;
        }
        let r = self.p.resolve(&segs, &self.def.file)?;
        let crate::program::Target::Const(ci) = r.target else { return None };
        if r.rest.len() != 1 || !matches!(f.get(2), Some(V::Nil) | None) {
            return None;
        }
        // 宣言の型が `&str` の定数だけ (`static` の `Mutex` などは別の経路)
        let declared = crate::types::from_v(&self.p.defs[ci].ret, &std::collections::HashSet::new());
        if !matches!(&declared, Src::Ref(_, i) if matches!(&**i, Src::Prim(p) if p == "str")) {
            return None;
        }
        let m = r.rest[0].clone();
        let line = nline(e);
        // 定数の式は下ろしが作る (型表にない) ので、番号 0 の式として渡す。型は宣言から決まる。
        let recv = match self.const_val(ci, &V::Nil) {
            Ok(v) => v,
            Err(why) => return Some(Err(why)),
        };
        if self.vty(recv) != self.fat_ty() {
            return Some(self.err(line, format!("定数のメソッド {} は &str の定数だけ段階 1 の対象", m)));
        }
        let args = crate::check::items(&f[1]).to_vec();
        Some(coll::method_on(self, e, line, None, Kind::Str, recv, &m, &args))
    }
}
