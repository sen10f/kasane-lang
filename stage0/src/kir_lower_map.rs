//! KIR の下ろし: `alloc.collections.HashMap` (設計_KIR.md 16 章、kir/hashmap)。`kir_lower.rs` の子モジュール。
//!
//! 表現: 局所変数は Vec / String と同じ 24 バイトのヘッダ `{buf: ptr, len: usize, cap: usize}` (フレームに置く)。
//! `len` は生きている組の数。`buf` は 1 本のヒープ領域で、所有と解放は Vec と同じ (`free(buf)`、スコープの終わり)。
//!
//! ```text
//! buf+0                 used   (使った組の数。削除した組も含む。挿入順の組の列の長さ)
//! buf+8                 組 0, 組 1, ... (cap 個。1 組 = key(ksz) + value(8) + live(8))
//! buf+8+cap*esz         索引表 (cap*2 個の usize。開番地法・線形探査。0 = 空、TOMB = 墓標、n = 組 n-1)
//! ```
//!
//! キーは 8 バイトの語 (整数・bool・char。上位は 0 埋め) か `&str` (ptr, len の 16 バイト)。値は 8 バイトの語 (整数・bool・char)。
//! 削除は組の `live` を 0 に、索引を墓標にするだけ (挿入順を保つ)。満杯 (`used == cap`) のときの挿入が、組を詰めて表を作り直す
//! (生きている組が半分以上なら容量を倍にする)。ハッシュは決定的 (種なし)。反復は組の列を前から見て、`live` のものを出す。
//!
//! 補助の関数 (`ksn.map.<w|s>.hash/find/add/remove`) は、キーの種類ごとに 1 組を `coll.extra` へ足す (`w` = 整数の語、`s` = `&str`)。

use std::collections::BTreeSet;

use crate::check::{items, nf, nlineno as nline, nname, st, strs};
use crate::kir::{Aop, Block, BoolOp, Cc, Func, Inst, Mode, Op, Pay, Term, Ty, VId};
use crate::parser::V;

use super::coll::{self, Kind};
use super::{Lw, LR};

const ENT0: i128 = 8;
const TOMB: i128 = 0x7FFF_FFFF_FFFF_FFFF;
/// キーの種類。`str_key` なら `&str` (ptr, len) か `String` (ptr, len, cap。`own`。組が所有する)、そうでなければ 8 バイトの語。
#[derive(Clone, Copy)]
struct Shape {
    str_key: bool,
    own: bool,
}

impl Shape {
    /// キーの型 `kt` から決める (`Ty::Ptr` = `&str`、Vec / String のヘッダ = `String`、それ以外 = 語)。
    fn of(lw: &Lw, kt: Ty) -> Shape {
        Shape { str_key: kt == Ty::Ptr || lw.aggs.borrow().is_vec(kt), own: lw.aggs.borrow().is_vec(kt) }
    }
    fn ksz(self) -> i128 {
        if self.own {
            24
        } else if self.str_key {
            16
        } else {
            8
        }
    }
    fn esz(self) -> i128 {
        self.ksz() + 16
    }
    fn tag(self) -> &'static str {
        if self.own {
            "o"
        } else if self.str_key {
            "s"
        } else {
            "w"
        }
    }
    /// `add` の引数のキー (組へ書く語)。`String` は ptr, len, cap。
    fn key_tys(self) -> Vec<Ty> {
        if self.own {
            vec![Ty::Ptr, Ty::Usize, Ty::Usize]
        } else {
            self.lookup_tys()
        }
    }
    /// `hash` / `find` / `remove` の引数のキー (照合に使う語)。
    fn lookup_tys(self) -> Vec<Ty> {
        if self.str_key {
            vec![Ty::Ptr, Ty::Usize]
        } else {
            vec![Ty::Usize]
        }
    }
    fn name(self, op: &str) -> String {
        format!("ksn.map.{}.{}", self.tag(), op)
    }
}

// ------------------------------------------------ 補助の関数を組み立てる道具

pub(super) struct Fb {
    blocks: Vec<(Vec<(VId, Ty)>, Vec<Inst>, Option<Term>)>,
    n: u32,
    cur: usize,
}

impl Fb {
    pub(super) fn new(params: &[Ty]) -> Fb {
        let ps = params.iter().enumerate().map(|(i, t)| (i as VId, *t)).collect();
        Fb { blocks: vec![(ps, vec![], None)], n: params.len() as u32, cur: 0 }
    }
    pub(super) fn emit(&mut self, ty: Ty, op: Op) -> VId {
        let dst = self.n;
        self.n += 1;
        self.blocks[self.cur].1.push(Inst { dst, ty, op });
        dst
    }
    pub(super) fn block(&mut self, params: &[Ty]) -> (usize, Vec<VId>) {
        let ids: Vec<VId> = params
            .iter()
            .map(|_| {
                self.n += 1;
                self.n - 1
            })
            .collect();
        self.blocks.push((ids.iter().copied().zip(params.iter().copied()).collect(), vec![], None));
        (self.blocks.len() - 1, ids)
    }
    pub(super) fn term(&mut self, t: Term) {
        self.blocks[self.cur].2 = Some(t);
    }
    pub(super) fn goto(&mut self, b: usize, args: Vec<VId>) {
        self.term(Term::Br((b, args)));
    }
    pub(super) fn at(&mut self, b: usize) {
        self.cur = b;
    }
    pub(super) fn c(&mut self, ty: Ty, v: i128) -> VId {
        self.emit(ty, Op::Const(v))
    }
    pub(super) fn cmp(&mut self, cc: Cc, a: VId, b: VId) -> VId {
        self.emit(Ty::Bool, Op::Cmp { cc, ty: Ty::Usize, a, b })
    }
    pub(super) fn br_if(&mut self, c: VId, t: usize, e: usize) {
        self.term(Term::BrIf(c, (t, vec![]), (e, vec![])));
    }
    pub(super) fn add(&mut self, a: VId, b: VId) -> VId {
        self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a, b })
    }
    pub(super) fn sub(&mut self, a: VId, b: VId) -> VId {
        self.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a, b })
    }
    pub(super) fn mul(&mut self, a: VId, b: VId) -> VId {
        self.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a, b })
    }
    pub(super) fn and(&mut self, a: VId, b: VId) -> VId {
        self.emit(Ty::Usize, Op::Bool(BoolOp::And, a, b))
    }
    pub(super) fn xor(&mut self, a: VId, b: VId) -> VId {
        self.emit(Ty::Usize, Op::Bool(BoolOp::Xor, a, b))
    }
    pub(super) fn load(&mut self, ty: Ty, p: VId, off: i128) -> VId {
        self.emit(ty, Op::MemLoad { p, off: off as u32 })
    }
    pub(super) fn store(&mut self, ty: Ty, p: VId, off: i128, v: VId) {
        self.emit(Ty::Unit, Op::MemStore { ty, p, off: off as u32, v });
    }
    pub(super) fn padd(&mut self, p: VId, off: VId) -> VId {
        self.emit(Ty::Ptr, Op::PtrAdd { p, off })
    }
    pub(super) fn build(self, key: &str, uses: &[&str], ret: Ty) -> Func {
        Func {
            fqn: key.to_string(),
            key: key.to_string(),
            uses: uses.iter().map(|s| s.to_string()).collect(),
            may_panic: false,
            stack: None,
            ret,
            link: None,
            blocks: self.blocks.into_iter().map(|(params, insts, t)| Block { params, insts, term: t.unwrap_or(Term::Unreachable) }).collect(),
        }
    }
    /// 組 `i` (添字) の番地: `buf + ENT0 + i*esz`。
    pub(super) fn entry(&mut self, buf: VId, i: VId, esz: i128) -> VId {
        let k = self.c(Ty::Usize, esz);
        let o = self.mul(i, k);
        let e0 = self.c(Ty::Usize, ENT0);
        let o = self.add(o, e0);
        self.padd(buf, o)
    }
}

/// ハッシュの仕上げ (決定的な混ぜ)。
fn mix(b: &mut Fb, x: VId) -> VId {
    let m1 = b.c(Ty::Usize, 0x5851_F42D_4C95_7F2D);
    let h = b.mul(x, m1);
    let s29 = b.c(Ty::U32, 29);
    let sh = b.emit(Ty::Usize, Op::Shift { left: false, a: h, n: s29 });
    let h = b.xor(h, sh);
    let m2 = b.c(Ty::Usize, 0x2545_F491_4F6C_DD1D);
    let h = b.mul(h, m2);
    let s32 = b.c(Ty::U32, 32);
    let sh = b.emit(Ty::Usize, Op::Shift { left: false, a: h, n: s32 });
    b.xor(h, sh)
}

fn hash_fn(sh: Shape) -> Func {
    let mut b = Fb::new(&sh.lookup_tys());
    if !sh.str_key {
        let h = mix(&mut b, 0);
        b.term(Term::Ret(h));
        return b.build(&sh.name("hash"), &[], Ty::Usize);
    }
    // FNV-1a (バイト列) の後に混ぜる
    let (lp, lv) = b.block(&[Ty::Usize, Ty::Usize]);
    let (body, _) = b.block(&[]);
    let (fin, fv) = b.block(&[Ty::Usize]);
    let z = b.c(Ty::Usize, 0);
    let basis = b.c(Ty::Usize, 0x4BF2_9CE4_8422_2325);
    b.goto(lp, vec![z, basis]);
    b.at(lp);
    let done = b.cmp(Cc::Ge, lv[0], 1);
    b.term(Term::BrIf(done, (fin, vec![lv[1]]), (body, vec![])));
    b.at(body);
    let q = b.padd(0, lv[0]);
    let by = b.load(Ty::U8, q, 0);
    let bw = b.emit(Ty::Usize, Op::Conv { src: Ty::U8, a: by });
    let h = b.xor(lv[1], bw);
    let prime = b.c(Ty::Usize, 0x0000_0100_0000_01B3);
    let h = b.mul(h, prime);
    let one = b.c(Ty::Usize, 1);
    let i1 = b.add(lv[0], one);
    b.goto(lp, vec![i1, h]);
    b.at(fin);
    let h = mix(&mut b, fv[0]);
    b.term(Term::Ret(h));
    b.build(&sh.name("hash"), &[], Ty::Usize)
}

/// 組 `ent` のキーが `key` (関数の引数) と等しいか。等しければ `yes`、違えば `no` へ分岐する (現在のブロックを終端する)。
fn key_eq(b: &mut Fb, sh: Shape, ent: VId, key: &[VId], yes: usize, no: usize) {
    if !sh.str_key {
        let ek = b.load(Ty::Usize, ent, 0);
        let eq = b.cmp(Cc::Eq, ek, key[0]);
        b.br_if(eq, yes, no);
        return;
    }
    let el = b.load(Ty::Usize, ent, 8);
    let same = b.cmp(Cc::Eq, el, key[1]);
    let (pre, _) = b.block(&[]);
    b.br_if(same, pre, no);
    b.at(pre);
    let ep = b.load(Ty::Ptr, ent, 0);
    let (lp, lv) = b.block(&[Ty::Usize]);
    let (body, _) = b.block(&[]);
    let z = b.c(Ty::Usize, 0);
    b.goto(lp, vec![z]);
    b.at(lp);
    let done = b.cmp(Cc::Ge, lv[0], key[1]);
    b.term(Term::BrIf(done, (yes, vec![]), (body, vec![])));
    b.at(body);
    let qa = b.padd(ep, lv[0]);
    let qb = b.padd(key[0], lv[0]);
    let ca = b.load(Ty::U8, qa, 0);
    let cb = b.load(Ty::U8, qb, 0);
    let eq = b.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U8, a: ca, b: cb });
    let (nx, _) = b.block(&[]);
    b.br_if(eq, nx, no);
    b.at(nx);
    let one = b.c(Ty::Usize, 1);
    let i1 = b.add(lv[0], one);
    b.goto(lp, vec![i1]);
}

/// `find(hdr, key..) -> usize` (組の添字 + 1、無ければ 0) と `remove(hdr, key..) -> usize` (同じ。見つけたら削除する)。
fn probe_fn(sh: Shape, remove: bool) -> Func {
    let mut tys = vec![Ty::Ptr];
    tys.extend(sh.lookup_tys());
    let mut b = Fb::new(&tys);
    let key: Vec<VId> = (1..tys.len() as u32).collect();
    let esz = sh.esz();
    let (ret0, _) = b.block(&[]);
    let (probe, _) = b.block(&[]);
    let cap = b.load(Ty::Usize, 0, 16);
    let z = b.c(Ty::Usize, 0);
    let capz = b.cmp(Cc::Eq, cap, z);
    b.br_if(capz, ret0, probe);
    b.at(ret0);
    let r = b.c(Ty::Usize, 0);
    b.term(Term::Ret(r));
    b.at(probe);
    let buf = b.load(Ty::Ptr, 0, 0);
    let k = b.c(Ty::Usize, esz);
    let entsz = b.mul(cap, k);
    let e0 = b.c(Ty::Usize, ENT0);
    let ioff = b.add(entsz, e0);
    let ib = b.padd(buf, ioff);
    let icap = b.add(cap, cap);
    let one = b.c(Ty::Usize, 1);
    let mask = b.sub(icap, one);
    let mut hargs = key.clone();
    let h = b.emit(Ty::Usize, Op::Call { callee: sh.name("hash"), args: std::mem::take(&mut hargs) });
    let pos0 = b.and(h, mask);
    let (lp, lv) = b.block(&[Ty::Usize]);
    b.goto(lp, vec![pos0]);
    b.at(lp);
    let eight = b.c(Ty::Usize, 8);
    let so = b.mul(lv[0], eight);
    let sp = b.padd(ib, so);
    let slot = b.load(Ty::Usize, sp, 0);
    let z = b.c(Ty::Usize, 0);
    let empty = b.cmp(Cc::Eq, slot, z);
    let (chk, _) = b.block(&[]);
    b.br_if(empty, ret0, chk);
    b.at(chk);
    let tomb = b.c(Ty::Usize, TOMB);
    let is_tomb = b.cmp(Cc::Eq, slot, tomb);
    let (cand, _) = b.block(&[]);
    let (next, _) = b.block(&[]);
    b.br_if(is_tomb, next, cand);
    b.at(cand);
    let one = b.c(Ty::Usize, 1);
    let e = b.sub(slot, one);
    let ent = b.entry(buf, e, esz);
    let (hit, _) = b.block(&[]);
    key_eq(&mut b, sh, ent, &key, hit, next);
    b.at(hit);
    if remove {
        let t = b.c(Ty::Usize, TOMB);
        b.store(Ty::Usize, sp, 0, t);
        let zero = b.c(Ty::Usize, 0);
        b.store(Ty::Usize, ent, sh.ksz() + 8, zero);
        let live = b.load(Ty::Usize, 0, 8);
        let one = b.c(Ty::Usize, 1);
        let l1 = b.sub(live, one);
        b.store(Ty::Usize, 0, 8, l1);
    }
    b.term(Term::Ret(slot));
    b.at(next);
    let one = b.c(Ty::Usize, 1);
    let p1 = b.add(lv[0], one);
    let pn = b.and(p1, mask);
    b.goto(lp, vec![pn]);
    b.build(&sh.name(if remove { "remove" } else { "find" }), &[], Ty::Usize)
}

/// `add(hdr, key..) -> usize`: キーが無いことが前提。末尾に組 (value 0、live 1) を足して添字を返す。満杯なら先に作り直す。
fn add_fn(sh: Shape) -> Func {
    let mut tys = vec![Ty::Ptr];
    tys.extend(sh.key_tys());
    let mut b = Fb::new(&tys);
    let key: Vec<VId> = (1..tys.len() as u32).collect();
    let esz = sh.esz();
    let ksz = sh.ksz();
    let (grow, _) = b.block(&[]);
    let (chk, _) = b.block(&[]);
    let (ins, _) = b.block(&[]);
    let cap = b.load(Ty::Usize, 0, 16);
    let buf = b.load(Ty::Ptr, 0, 0);
    let z = b.c(Ty::Usize, 0);
    let capz = b.cmp(Cc::Eq, cap, z);
    b.br_if(capz, grow, chk);
    b.at(chk);
    let used = b.load(Ty::Usize, buf, 0);
    let full = b.cmp(Cc::Eq, used, cap);
    b.br_if(full, grow, ins);

    // --- 作り直し
    b.at(grow);
    let (nc_init, _) = b.block(&[]);
    let (nc_chk, _) = b.block(&[]);
    let (dbl, _) = b.block(&[]);
    let (same, _) = b.block(&[]);
    let (nc, ncv) = b.block(&[Ty::Usize]);
    let z = b.c(Ty::Usize, 0);
    let capz = b.cmp(Cc::Eq, cap, z);
    b.br_if(capz, nc_init, nc_chk);
    b.at(nc_init);
    let eight = b.c(Ty::Usize, 8);
    b.goto(nc, vec![eight]);
    b.at(nc_chk);
    let live = b.load(Ty::Usize, 0, 8);
    let live2 = b.add(live, live);
    let big = b.cmp(Cc::Ge, live2, cap);
    b.br_if(big, dbl, same);
    b.at(dbl);
    let c2 = b.add(cap, cap);
    b.goto(nc, vec![c2]);
    b.at(same);
    b.goto(nc, vec![cap]);

    b.at(nc);
    let newcap = ncv[0];
    let per = b.c(Ty::Usize, esz + 16);
    let tot = b.mul(newcap, per);
    let e0 = b.c(Ty::Usize, ENT0);
    let tot = b.add(tot, e0);
    let nb = b.emit(Ty::Ptr, Op::HeapAlloc(tot));
    let z = b.c(Ty::Usize, 0);
    b.store(Ty::Usize, nb, 0, z);
    let k = b.c(Ty::Usize, esz);
    let entsz = b.mul(newcap, k);
    let e0 = b.c(Ty::Usize, ENT0);
    let ioff = b.add(entsz, e0);
    let nib = b.padd(nb, ioff);
    let icap = b.add(newcap, newcap);
    let one = b.c(Ty::Usize, 1);
    let mask = b.sub(icap, one);
    // 索引表を 0 にする
    let (zl, zv) = b.block(&[Ty::Usize]);
    let (zb, _) = b.block(&[]);
    let (cp0, _) = b.block(&[]);
    let z = b.c(Ty::Usize, 0);
    b.goto(zl, vec![z]);
    b.at(zl);
    let done = b.cmp(Cc::Ge, zv[0], icap);
    b.br_if(done, cp0, zb);
    b.at(zb);
    let eight = b.c(Ty::Usize, 8);
    let so = b.mul(zv[0], eight);
    let sp = b.padd(nib, so);
    let z = b.c(Ty::Usize, 0);
    b.store(Ty::Usize, sp, 0, z);
    let one = b.c(Ty::Usize, 1);
    let j1 = b.add(zv[0], one);
    b.goto(zl, vec![j1]);

    // 生きている組を前から詰めて写す
    b.at(cp0);
    let (cp1, _) = b.block(&[]);
    let (fin, finv) = b.block(&[Ty::Usize]);
    let z = b.c(Ty::Usize, 0);
    let capz = b.cmp(Cc::Eq, cap, z);
    b.term(Term::BrIf(capz, (fin, vec![z]), (cp1, vec![])));
    b.at(cp1);
    let oused = b.load(Ty::Usize, buf, 0);
    let (cl, clv) = b.block(&[Ty::Usize, Ty::Usize]);
    let (cb, _) = b.block(&[]);
    let z = b.c(Ty::Usize, 0);
    b.goto(cl, vec![z, z]);
    b.at(cl);
    let done = b.cmp(Cc::Ge, clv[0], oused);
    b.term(Term::BrIf(done, (fin, vec![clv[1]]), (cb, vec![])));
    b.at(cb);
    let oent = b.entry(buf, clv[0], esz);
    let livef = b.load(Ty::Usize, oent, ksz + 8);
    let z = b.c(Ty::Usize, 0);
    let isl = b.cmp(Cc::Ne, livef, z);
    let (mv, _) = b.block(&[]);
    let (nx, _) = b.block(&[]);
    b.br_if(isl, mv, nx);
    b.at(nx);
    let one = b.c(Ty::Usize, 1);
    let i1 = b.add(clv[0], one);
    b.goto(cl, vec![i1, clv[1]]);
    b.at(mv);
    let nent = b.entry(nb, clv[1], esz);
    for w in 0..(esz / 8) {
        let v = b.load(Ty::Usize, oent, w * 8);
        b.store(Ty::Usize, nent, w * 8, v);
    }
    let kv: Vec<VId> = if sh.str_key {
        vec![b.load(Ty::Ptr, oent, 0), b.load(Ty::Usize, oent, 8)]
    } else {
        vec![b.load(Ty::Usize, oent, 0)]
    };
    let h = b.emit(Ty::Usize, Op::Call { callee: sh.name("hash"), args: kv });
    let pos0 = b.and(h, mask);
    let (pl, plv) = b.block(&[Ty::Usize]);
    let (pset, _) = b.block(&[]);
    let (pnext, _) = b.block(&[]);
    b.goto(pl, vec![pos0]);
    b.at(pl);
    let eight = b.c(Ty::Usize, 8);
    let so = b.mul(plv[0], eight);
    let sp = b.padd(nib, so);
    let slot = b.load(Ty::Usize, sp, 0);
    let z = b.c(Ty::Usize, 0);
    let empty = b.cmp(Cc::Eq, slot, z);
    b.br_if(empty, pset, pnext);
    b.at(pnext);
    let one = b.c(Ty::Usize, 1);
    let p1 = b.add(plv[0], one);
    let pn = b.and(p1, mask);
    b.goto(pl, vec![pn]);
    b.at(pset);
    let one = b.c(Ty::Usize, 1);
    let cnt1 = b.add(clv[1], one);
    b.store(Ty::Usize, sp, 0, cnt1);
    let i1 = b.add(clv[0], one);
    b.goto(cl, vec![i1, cnt1]);

    b.at(fin);
    b.store(Ty::Usize, nb, 0, finv[0]);
    b.emit(Ty::Unit, Op::HeapFree(buf));
    b.store(Ty::Ptr, 0, 0, nb);
    b.store(Ty::Usize, 0, 16, newcap);
    b.goto(ins, vec![]);

    // --- 末尾へ足す
    b.at(ins);
    let buf2 = b.load(Ty::Ptr, 0, 0);
    let used = b.load(Ty::Usize, buf2, 0);
    let ent = b.entry(buf2, used, esz);
    if sh.str_key {
        b.store(Ty::Ptr, ent, 0, key[0]);
        b.store(Ty::Usize, ent, 8, key[1]);
        if sh.own {
            b.store(Ty::Usize, ent, 16, key[2]);
        }
    } else {
        b.store(Ty::Usize, ent, 0, key[0]);
    }
    let z = b.c(Ty::Usize, 0);
    b.store(Ty::Usize, ent, ksz, z);
    let one = b.c(Ty::Usize, 1);
    b.store(Ty::Usize, ent, ksz + 8, one);
    let cap2 = b.load(Ty::Usize, 0, 16);
    let k = b.c(Ty::Usize, esz);
    let entsz = b.mul(cap2, k);
    let e0 = b.c(Ty::Usize, ENT0);
    let ioff = b.add(entsz, e0);
    let ib = b.padd(buf2, ioff);
    let icap = b.add(cap2, cap2);
    let mask = b.sub(icap, one);
    let h = b.emit(Ty::Usize, Op::Call { callee: sh.name("hash"), args: key[..sh.lookup_tys().len()].to_vec() });
    let pos0 = b.and(h, mask);
    let (pl, plv) = b.block(&[Ty::Usize]);
    let (set, _) = b.block(&[]);
    let (nxt, _) = b.block(&[]);
    b.goto(pl, vec![pos0]);
    b.at(pl);
    let eight = b.c(Ty::Usize, 8);
    let so = b.mul(plv[0], eight);
    let sp = b.padd(ib, so);
    let slot = b.load(Ty::Usize, sp, 0);
    let z = b.c(Ty::Usize, 0);
    let empty = b.cmp(Cc::Eq, slot, z);
    let tomb = b.c(Ty::Usize, TOMB);
    let is_tomb = b.cmp(Cc::Eq, slot, tomb);
    let avail = b.emit(Ty::Bool, Op::Bool(BoolOp::Or, empty, is_tomb));
    b.br_if(avail, set, nxt);
    b.at(nxt);
    let one = b.c(Ty::Usize, 1);
    let p1 = b.add(plv[0], one);
    let pn = b.and(p1, mask);
    b.goto(pl, vec![pn]);
    b.at(set);
    let one = b.c(Ty::Usize, 1);
    let u1 = b.add(used, one);
    b.store(Ty::Usize, sp, 0, u1);
    b.store(Ty::Usize, buf2, 0, u1);
    let live = b.load(Ty::Usize, 0, 8);
    let l1 = b.add(live, one);
    b.store(Ty::Usize, 0, 8, l1);
    b.term(Term::Ret(used));
    b.build(&sh.name("add"), &["alloc"], Ty::Usize)
}

/// 補助の関数を `coll.extra` に足し、呼び出しの鍵を `callees` に入れる。
fn need(lw: &mut Lw, sh: Shape, ops: &[&str]) {
    let hash = hash_fn(sh);
    lw.coll.extra.push((hash, BTreeSet::new()));
    let deps: BTreeSet<String> = [sh.name("hash")].into_iter().collect();
    for op in ops {
        let f = match *op {
            "find" => probe_fn(sh, false),
            "remove" => probe_fn(sh, true),
            _ => add_fn(sh),
        };
        lw.coll.extra.push((f, deps.clone()));
        lw.callees.insert(sh.name(op));
    }
}

/// `drop(hdr)`: `String` をキーにした表の、生きている組のキーのバッファを解放する (表の `buf` は呼び手が解放する)。`buf` が null なら何もしない。
fn drop_fn(sh: Shape) -> Func {
    let mut b = Fb::new(&[Ty::Ptr]);
    let (go, _) = b.block(&[]);
    let (done, _) = b.block(&[]);
    let buf = b.load(Ty::Ptr, 0, 0);
    let z = b.c(Ty::Usize, 0);
    let np = b.emit(Ty::Ptr, Op::Const(0));
    let isnull = b.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Ptr, a: buf, b: np });
    b.br_if(isnull, done, go);
    b.at(go);
    let used = b.load(Ty::Usize, buf, 0);
    let (lp, lv) = b.block(&[Ty::Usize]);
    let (body, _) = b.block(&[]);
    let (nx, _) = b.block(&[]);
    let (fr, _) = b.block(&[]);
    b.goto(lp, vec![z]);
    b.at(lp);
    let more = b.cmp(Cc::Lt, lv[0], used);
    b.br_if(more, body, done);
    b.at(body);
    let ent = b.entry(buf, lv[0], sh.esz());
    let live = b.load(Ty::Usize, ent, sh.ksz() + 8);
    let isl = b.cmp(Cc::Ne, live, z);
    b.br_if(isl, fr, nx);
    b.at(fr);
    let kp = b.load(Ty::Ptr, ent, 0);
    b.emit(Ty::Unit, Op::HeapFree(kp));
    b.goto(nx, vec![]);
    b.at(nx);
    let one = b.c(Ty::Usize, 1);
    let i1 = b.add(lv[0], one);
    b.goto(lp, vec![i1]);
    b.at(done);
    let u = b.emit(Ty::Unit, Op::Const(0));
    b.term(Term::Ret(u));
    b.build(&sh.name("drop"), &["alloc"], Ty::Unit)
}

/// 表のスコープの終わり (`drop_hdr`) で呼ぶ: ヘッダ `hdr` が `String` キーの `HashMap` の局所変数なら、キーを解放する命令列を置く。
pub(crate) fn drop_owned_keys(lw: &mut Lw, hdr: VId) {
    let bslot = lw.coll.kinds.iter().find(|(s, k)| matches!(k, Kind::BMap(Some(_))) && lw.slots.get(**s).map(|x| x.0) == Some(hdr)).map(|(s, _)| *s);
    if let Some(Kind::BMap(Some((kt, _)))) = bslot.and_then(|s| lw.coll.kinds.get(&s).cloned()) {
        return super::mapstr_lower::drop_owned_keys(lw, hdr, kt);
    }
    let slot = lw.coll.kinds.iter().find(|(s, k)| matches!(k, Kind::Map(Some(_))) && lw.slots.get(**s).map(|x| x.0) == Some(hdr)).map(|(s, _)| *s);
    let Some(s) = slot else { return };
    let Some(Kind::Map(Some((kt, _)))) = lw.coll.kinds.get(&s).cloned() else { return };
    let sh = Shape::of(lw, kt);
    if !sh.own {
        return;
    }
    lw.coll.extra.push((drop_fn(sh), BTreeSet::new()));
    lw.callees.insert(sh.name("drop"));
    lw.emit(Ty::Unit, Op::Call { callee: sh.name("drop"), args: vec![hdr] });
}

// ------------------------------------------------ 下ろし

fn scalar_word(t: Ty) -> bool {
    matches!(t, Ty::I8 | Ty::U8 | Ty::Bool | Ty::I16 | Ty::U16 | Ty::I32 | Ty::U32 | Ty::I64 | Ty::U64 | Ty::Isize | Ty::Usize)
}

/// スカラーを、上位を 0 で埋めた 8 バイトの語にする (フレームの 8 バイトを通す)。
fn to_word(lw: &mut Lw, v: VId, t: Ty) -> VId {
    let sc = coll::emit_alloca(lw, 8);
    let z = lw.emit(Ty::Usize, Op::Const(0));
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: sc, off: 0, v: z });
    lw.emit(Ty::Unit, Op::MemStore { ty: t, p: sc, off: 0, v });
    lw.emit(Ty::Usize, Op::MemLoad { p: sc, off: 0 })
}

/// キーの引数を評価する。`&k` は `k` の値を、`&str` はそのまま。結果は (補助の関数に渡す値の並び, キーの型の印)。
fn eval_key(lw: &mut Lw, a: &V, line: usize) -> LR<Option<(Vec<VId>, Ty)>> {
    let operand = if nname(a) == "Unary" && st(&nf(a)[0]) == "&" { &nf(a)[1] } else { a };
    let Some(mut v) = lw.expr(operand)? else { return Ok(None) };
    if let Ty::Ref(_, p) = lw.vty(v) {
        v = lw.emit(p.ty(), Op::Load(v));
    }
    let t = lw.vty(v);
    if t == lw.fat_ty() {
        let p = lw.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
        let n = lw.emit(Ty::Usize, Op::Field { a: v, off: 8 });
        return Ok(Some((vec![p, n], Ty::Ptr)));
    }
    if !scalar_word(t) {
        return lw.err(line, format!("HashMap のキーの型 {} は段階 1 の対象外 (整数・bool・char・&str・String だけ)", t.name()));
    }
    Ok(Some((vec![to_word(lw, v, t)], t)))
}

/// `get` / `contains_key` / `remove` のキー (`&k`): `String` のキーの表なら、借用のヘッダ (`&String`) から (ptr, len) を読む。
/// `k` を値として読むと所有が移ってしまうので、`&k` の式をそのまま評価する (`borrow_header`)。
fn eval_lookup_key(lw: &mut Lw, a: &V, kt: Ty, line: usize) -> LR<Option<(Vec<VId>, Ty)>> {
    if !lw.aggs.borrow().is_vec(kt) {
        return eval_key(lw, a, line);
    }
    let Some(v) = lw.expr(a)? else { return Ok(None) };
    let t = lw.vty(v);
    if t == lw.fat_ty() {
        let p = lw.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
        let n = lw.emit(Ty::Usize, Op::Field { a: v, off: 8 });
        return Ok(Some((vec![p, n], kt)));
    }
    if t != Ty::Ptr {
        return lw.err(line, format!("HashMap<String, _> の検索のキーの型 {} は段階 1 の対象外 (&String か &str だけ)", t.name()));
    }
    let p = lw.emit(Ty::Ptr, Op::MemLoad { p: v, off: 0 });
    let n = lw.emit(Ty::Usize, Op::MemLoad { p: v, off: 8 });
    Ok(Some((vec![p, n], kt)))
}

/// `insert` の `String` のキー (所有が組へ移る)。値を評価して (ptr, len, cap) に分ける。
fn eval_own_key(lw: &mut Lw, a: &V, line: usize) -> LR<Option<(VId, Vec<VId>)>> {
    match lw.src_ty(a) {
        Ok(crate::types::Ty::Named(n, _)) if n == "alloc.string.String" => {}
        _ => return lw.err(line, "HashMap のキーの型は段階 1 の対象外 (整数・bool・char・&str・String だけ)"),
    }
    let Some(v) = lw.expr(a)? else { return Ok(None) };
    let t = lw.vty(v);
    let p = lw.emit(Ty::Ptr, Op::Field { a: v, off: 0 });
    let n = lw.emit(Ty::Usize, Op::Field { a: v, off: 8 });
    let c = lw.emit(Ty::Usize, Op::Field { a: v, off: 16 });
    let _ = t;
    Ok(Some((v, vec![p, n, c])))
}

/// 受け手の種類から (K, V) を決める。未確定なら `learn` で補う (`insert` のとき)。
fn types(lw: &mut Lw, slot: Option<usize>, kind: &Kind, learn: Option<(Ty, Ty)>, line: usize) -> LR<(Ty, Ty)> {
    match (kind, learn) {
        (Kind::Map(Some(kv)), _) => Ok(*kv),
        (Kind::Map(None), Some(kv)) => {
            if let Some(s) = slot {
                lw.coll.kinds.insert(s, Kind::Map(Some(kv)));
            }
            Ok(kv)
        }
        _ => lw.err(line, "HashMap のキー・値の型が決まらない"),
    }
}

fn entry_addr(lw: &mut Lw, h: VId, idx: VId, esz: i128) -> VId {
    let buf = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let k = lw.emit(Ty::Usize, Op::Const(esz));
    let o = lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: idx, b: k });
    let e0 = lw.emit(Ty::Usize, Op::Const(ENT0));
    let o = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: o, b: e0 });
    lw.emit(Ty::Ptr, Op::PtrAdd { p: buf, off: o })
}

fn dec(lw: &mut Lw, v: VId) -> VId {
    let one = lw.emit(Ty::Usize, Op::Const(1));
    lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: v, b: one })
}

/// `m.insert(k, v)` / `get` / `contains_key` / `remove` / `len` / `is_empty`。受け手のヘッダのアドレスは `h`。
pub(crate) fn method(lw: &mut Lw, e: &V, slot: Option<usize>, kind: &Kind, h: VId, m: &str, args: &[V]) -> LR<Option<VId>> {
    let line = nline(e);
    match (m, args.len()) {
        ("len", 0) => Ok(Some(lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 }))),
        ("is_empty", 0) => {
            let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
            let z = lw.emit(Ty::Usize, Op::Const(0));
            Ok(Some(lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: len, b: z })))
        }
        ("insert", 2) => insert(lw, e, line, slot, kind, h, args),
        ("get", 1) | ("contains_key", 1) | ("remove", 1) => lookup(lw, e, line, slot, kind, h, m, &args[0]),
        ("get_mut", _) => lw.err(line, "HashMap::get_mut は段階 1 の対象外 (Option<&mut V> を持てない。参照の段階の作業)"),
        ("iter", _) => lw.err(line, "HashMap::iter は for の対象としてだけ段階 1 の対象"),
        _ => lw.err(line, format!("HashMap のメソッド {} は段階 1 の対象外", m)),
    }
}

fn insert(lw: &mut Lw, e: &V, line: usize, slot: Option<usize>, kind: &Kind, h: VId, args: &[V]) -> LR<Option<VId>> {
    // `&str` のキーは借用 (組には `ptr` と長さだけが入る)。元の `String` を後で変えると再確保で組のキーが解放後の領域を指す。
    // `ksn check` はこの形を通すので、`s.as_str()` をキーにした `String` が関数のどこかで変えられるなら拒む (設計_KIR.md 17.6)。
    let operand = if nname(&args[0]) == "Unary" && st(&nf(&args[0])[0]) == "&" { &nf(&args[0])[1] } else { &args[0] };
    // `k.as_str()` は、パーサの形によって `Call` (名前の経路) にも `MCall` にもなる。元の String の名前を取る。
    let src = match nname(operand) {
        "MCall" if st(&nf(operand)[1]) == "as_str" => super::agg::field_segs(&nf(operand)[0]).map(|s| s[0].clone()),
        "Call" if nname(&nf(operand)[0]) == "Path" => {
            let segs = strs(&nf(&nf(operand)[0])[0]);
            (segs.len() == 2 && segs[1] == "as_str").then(|| segs[0].clone())
        }
        _ => None,
    };
    if let (Some(name), Some(body)) = (src, lw.def.body.as_ref()) {
        if super::coll::body_mutates(body, &name) {
            return lw.err(line, format!("キー {}.as_str() の元の String が関数の中で変更される形は段階 1 の対象外 (組のキーが解放後の領域を指す)", name));
        }
    }
    let own_key = matches!(lw.src_ty(&args[0]), Ok(crate::types::Ty::Named(n, _)) if n == "alloc.string.String");
    let (key, kt, held) = if own_key {
        let Some((kv, words)) = eval_own_key(lw, &args[0], line)? else { return Ok(None) };
        let kt = lw.vty(kv);
        // 値の評価が `?` / `return` で抜けるときにキーを解放する
        coll::tmp_hold(lw, kv);
        (words, kt, Some(kv))
    } else {
        let Some((key, kt)) = eval_key(lw, &args[0], line)? else { return Ok(None) };
        (key, kt, None)
    };
    let val = lw.expr(&args[1]);
    if let Some(kv) = held {
        coll::tmp_release(lw, &[kv]);
    }
    let Some(val) = val? else { return Ok(None) };
    let vt = lw.vty(val);
    if !scalar_word(vt) {
        return lw.err(line, format!("HashMap の値の型 {} は段階 1 の対象外 (整数・bool・char だけ)", vt.name()));
    }
    let (kt0, vt0) = types(lw, slot, kind, Some((kt, vt)), line)?;
    if (kt0, vt0) != (kt, vt) {
        return lw.err(line, "insert の引数の型が HashMap のキー・値の型と違う");
    }
    let rt = lw.ty_of(e)?;
    let Ty::Res(crate::kir::Pay::Opt(inner), _) = rt else {
        return lw.err(line, "insert の結果の型が Result<Option<V>, AllocError> でない");
    };
    let ot = Ty::Opt(*inner);
    let sh = Shape::of(lw, kt);
    need(lw, sh, &["find", "add"]);
    let vw = to_word(lw, val, vt);
    let mut fa = vec![h];
    fa.extend(key.iter().copied());
    let mut fl = vec![h];
    fl.extend(key[..sh.lookup_tys().len()].iter().copied());
    let idx1 = lw.emit(Ty::Usize, Op::Call { callee: sh.name("find"), args: fl });
    let z = lw.emit(Ty::Usize, Op::Const(0));
    let found = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Ne, ty: Ty::Usize, a: idx1, b: z });
    let (upd, _) = lw.new_block(&[]);
    let (newb, _) = lw.new_block(&[]);
    let (jb, jp) = lw.new_block(&[rt]);
    lw.branch(found, (upd, vec![]), (newb, vec![]));
    lw.cur = Some(upd);
    let idx = dec(lw, idx1);
    let ent = entry_addr(lw, h, idx, sh.esz());
    let old = lw.emit(vt, Op::MemLoad { p: ent, off: sh.ksz() as u32 });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: ent, off: sh.ksz() as u32, v: vw });
    // 同じキーが既にあれば、渡したキーは捨てる (表のキーは替えない)
    if sh.own {
        lw.emit(Ty::Unit, Op::HeapFree(key[0]));
    }
    let s = lw.emit(ot, Op::Some(old));
    let r = lw.emit(rt, Op::Ok(s));
    lw.goto(jb, vec![r]);
    lw.cur = Some(newb);
    let idx = lw.emit(Ty::Usize, Op::Call { callee: sh.name("add"), args: fa });
    let ent = entry_addr(lw, h, idx, sh.esz());
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: ent, off: sh.ksz() as u32, v: vw });
    let n = lw.emit(ot, Op::Const(0));
    let r = lw.emit(rt, Op::Ok(n));
    lw.goto(jb, vec![r]);
    lw.cur = Some(jb);
    Ok(Some(jp[0]))
}

fn lookup(lw: &mut Lw, e: &V, line: usize, slot: Option<usize>, kind: &Kind, h: VId, m: &str, arg: &V) -> LR<Option<VId>> {
    let (kt0, vt) = types(lw, slot, kind, None, line)?;
    let Some((key, kt)) = eval_lookup_key(lw, arg, kt0, line)? else { return Ok(None) };
    if kt0 != kt {
        return lw.err(line, "キーの型が HashMap のキーの型と違う");
    }
    let sh = Shape::of(lw, kt);
    let op = if m == "remove" { "remove" } else { "find" };
    need(lw, sh, &[op]);
    let mut fa = vec![h];
    fa.extend(key.iter().copied());
    let idx1 = lw.emit(Ty::Usize, Op::Call { callee: sh.name(op), args: fa });
    let z = lw.emit(Ty::Usize, Op::Const(0));
    let found = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Ne, ty: Ty::Usize, a: idx1, b: z });
    if m == "contains_key" {
        return Ok(Some(found));
    }
    let rt = lw.ty_of(e)?;
    if !matches!(rt, Ty::Opt(_)) {
        return lw.err(line, "get / remove の結果の型が Option でない");
    }
    let (yes, _) = lw.new_block(&[]);
    let (no, _) = lw.new_block(&[]);
    let (jb, jp) = lw.new_block(&[rt]);
    lw.branch(found, (yes, vec![]), (no, vec![]));
    lw.cur = Some(yes);
    let idx = dec(lw, idx1);
    let ent = entry_addr(lw, h, idx, sh.esz());
    let v = lw.emit(vt, Op::MemLoad { p: ent, off: sh.ksz() as u32 });
    // 削除した組のキーのバッファは (組が所有していたので) ここで解放する
    if sh.own && m == "remove" {
        let kp = lw.emit(Ty::Ptr, Op::MemLoad { p: ent, off: 0 });
        lw.emit(Ty::Unit, Op::HeapFree(kp));
    }
    let s = lw.emit(rt, Op::Some(v));
    lw.goto(jb, vec![s]);
    lw.cur = Some(no);
    let n = lw.emit(rt, Op::Const(0));
    lw.goto(jb, vec![n]);
    lw.cur = Some(jb);
    Ok(Some(jp[0]))
}

// ------------------------------------------------ for (k, v) in m.iter()

/// `for (k, v) in m.iter() { .. }` (組の列を前から。削除済みは飛ばす)。対象外の形なら `None` (親の処理へ)。
pub(crate) fn try_for(lw: &mut Lw, e: &V) -> Option<LR<Option<VId>>> {
    let f = nf(e);
    let it = &f[1];
    let recv_slot = match nname(it) {
        "Call" => {
            let cf = nf(it);
            if nname(&cf[0]) != "Path" || !items(&cf[1]).is_empty() {
                return None;
            }
            let segs = strs(&nf(&cf[0])[0]);
            if segs.len() != 2 || segs[1] != "iter" {
                return None;
            }
            lw.lookup(&segs[0])?
        }
        "MCall" => {
            let mf = nf(it);
            if st(&mf[1]) != "iter" || !items(&mf[2]).is_empty() {
                return None;
            }
            coll::local_slot(lw, &mf[0])?
        }
        _ => return None,
    };
    let Some(Kind::Map(kv)) = lw.coll.kinds.get(&recv_slot).cloned() else { return None };
    Some(for_iter(lw, e, recv_slot, kv))
}

fn for_iter(lw: &mut Lw, e: &V, recv: usize, kv: Option<(Ty, Ty)>) -> LR<Option<VId>> {
    let f = nf(e);
    let line = nline(e);
    let Some((kt, vt)) = kv else { return lw.err(line, "HashMap のキー・値の型が決まらない") };
    if nname(&f[0]) != "PTuple" || items(&nf(&f[0])[0]).len() != 2 {
        return lw.err(line, "HashMap の for の変数は (k, v) の形だけ段階 1 の対象");
    }
    let mut names: Vec<Option<String>> = vec![];
    for p in items(&nf(&f[0])[0]) {
        match nname(p) {
            "PBind" if !matches!(nf(p).get(1), Some(V::B(true))) => names.push(Some(st(&nf(p)[0]))),
            "PWild" => names.push(None),
            _ => return lw.err(line, "HashMap の for の変数はただの名前か _ だけ段階 1 の対象"),
        }
    }
    let sh = Shape::of(lw, kt);
    let body = &f[2];
    let h = lw.slots[recv].0;
    let fat = lw.fat_ty();

    let slots = lw.assigned(&[body]);
    let slot_tys: Vec<Ty> = slots.iter().map(|s| lw.slots[*s].1).collect();
    let (xb, xp) = lw.new_block(&slot_tys);
    let (ib, ip) = lw.new_block(&slot_tys);
    let mut hty = vec![Ty::Usize];
    hty.extend(slot_tys.iter().copied());
    let (hb, hp) = lw.new_block(&hty);

    // 使った組の数 (空の表は buf が null なので 0)
    let cap = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 });
    let z = lw.emit(Ty::Usize, Op::Const(0));
    let capz = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: cap, b: z });
    let (u0, _) = lw.new_block(&[]);
    let (u1, _) = lw.new_block(&[]);
    let (uj, ujp) = lw.new_block(&[Ty::Usize]);
    lw.branch(capz, (u0, vec![]), (u1, vec![]));
    lw.cur = Some(u0);
    lw.goto(uj, vec![z]);
    lw.cur = Some(u1);
    let buf = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let used = lw.emit(Ty::Usize, Op::MemLoad { p: buf, off: 0 });
    lw.goto(uj, vec![used]);
    lw.cur = Some(uj);
    let used = ujp[0];
    let start = lw.emit(Ty::Usize, Op::Const(0));
    let mut entry = vec![start];
    entry.extend(lw.vals_of(&slots));
    lw.goto(hb, entry);

    lw.cur = Some(hb);
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (hp[k + 1], slot_tys[k]);
    }
    let (cb, _) = lw.new_block(&[]);
    let cond = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: hp[0], b: used });
    let exit_args = lw.vals_of(&slots);
    lw.branch(cond, (cb, vec![]), (xb, exit_args));

    lw.cur = Some(cb);
    let ent = entry_addr(lw, h, hp[0], sh.esz());
    let live = lw.emit(Ty::Usize, Op::MemLoad { p: ent, off: (sh.ksz() + 8) as u32 });
    let z = lw.emit(Ty::Usize, Op::Const(0));
    let isl = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Ne, ty: Ty::Usize, a: live, b: z });
    let (bb, _) = lw.new_block(&[]);
    let skip_args = lw.vals_of(&slots);
    lw.branch(isl, (bb, vec![]), (ib, skip_args));

    lw.cur = Some(bb);
    lw.scopes.push(vec![]);
    let depth = lw.scopes.len() - 1;
    if let (Some(n), true) = (&names[0], sh.own) {
        // `String` のキーは組の中のヘッダ。借用の `&String` として束縛する (解放しない)
        lw.bind(n, ent, Ty::Ptr);
        let s = lw.lookup(n).expect("束縛した名前");
        lw.coll.kinds.insert(s, Kind::String);
        lw.coll.borrowed.insert(s);
    } else if let Some(n) = &names[0] {
        let v = if sh.str_key {
            let p = lw.emit(Ty::Ptr, Op::MemLoad { p: ent, off: 0 });
            let l = lw.emit(Ty::Usize, Op::MemLoad { p: ent, off: 8 });
            lw.emit(fat, Op::Make { variant: None, fields: vec![p, l] })
        } else {
            lw.emit(kt, Op::MemLoad { p: ent, off: 0 })
        };
        let t = lw.vty(v);
        lw.bind(n, v, t);
    }
    if let Some(n) = &names[1] {
        let v = lw.emit(vt, Op::MemLoad { p: ent, off: sh.ksz() as u32 });
        lw.bind(n, v, vt);
    }
    lw.loops.push(super::LoopCx { header: ib, exit: xb, slots: slots.clone(), depth });
    let r = lw.expr(body)?;
    if r.is_some() {
        let args = lw.vals_of(&slots);
        lw.goto(ib, args);
    }
    lw.loops.pop();
    coll::block_end(lw);
    lw.scopes.pop();

    lw.cur = Some(ib);
    let one = lw.emit(Ty::Usize, Op::Const(1));
    let next = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: hp[0], b: one });
    let mut back = vec![next];
    back.extend(ip.iter().copied());
    lw.goto(hb, back);

    lw.cur = Some(xb);
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (xp[k], slot_tys[k]);
    }
    Ok(Some(lw.unit()))
}

// ------------------------------------------------ alloc.collections.BtreeMap (キーの順を保つ)
//
// 表現: ヘッダは HashMap と同じ 24 バイト `{buf, len, cap}` (所有と解放は Vec と同じ)。`buf` は組の配列で、1 組は
// キー (型どおりの大きさ、欄は 0 から) と値 (欄は +8) の 16 バイト。組はキーの昇順に並ぶ。
// 検索は二分探索 (`lower`)、挿入と削除は後ろの組を 1 つずらす。容量が足りなければ `2*cap+4` の組へ広げる。
// キーは整数・bool・char、値は整数・bool・char だけ (`&str` のキーは未対応)。反復 (`for`) は未対応。

const BENT: i128 = 16;

/// `lower` (キーの型だけで決まる) の鍵。
fn b_lower_name(kt: Ty) -> String {
    format!("ksn.bmap.{}.lower", kt.name())
}

/// `get` / `insert` / `remove` の鍵 (キーと値の型ごと)。
fn b_name(kt: Ty, vt: Ty, op: &str) -> String {
    format!("ksn.bmap.{}.{}.{}", kt.name(), vt.name(), op)
}

/// 組 `i` の番地: `buf + i*16`。
fn b_ent(b: &mut Fb, buf: VId, i: VId) -> VId {
    let k = b.c(Ty::Usize, BENT);
    let o = b.mul(i, k);
    b.padd(buf, o)
}

/// `(h, key) -> usize`: キー以上になる最初の添字 (組は昇順)。
fn b_lower_fn(kt: Ty) -> Func {
    let mut b = Fb::new(&[Ty::Ptr, kt]);
    let key: VId = 1;
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
    // (hi - lo) / 2 は 1 ビット右へずらす (割り算は trap を持つので、panics never の関数では使えない)
    let one32 = b.emit(Ty::U32, Op::Const(1));
    let half = b.emit(Ty::Usize, Op::Shift { left: false, a: d, n: one32 });
    let mid = b.add(lo, half);
    let ent = b_ent(&mut b, buf, mid);
    let k = b.load(kt, ent, 0);
    let less = b.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: kt, a: k, b: key });
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
    b.build(&b_lower_name(kt), &[], Ty::Usize)
}

/// 添字 `pos` の組のキーが `key` と等しいなら、その組の番地 (`buf` と `ent` を作る)。`pos < len` は呼び手が確かめる。
/// 戻りは (値の判定, 組の番地)。
fn b_found(b: &mut Fb, kt: Ty, pos: VId, key: VId, miss: usize) -> VId {
    let buf = b.load(Ty::Ptr, 0, 0);
    let ent = b_ent(b, buf, pos);
    let k = b.load(kt, ent, 0);
    let eq = b.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: kt, a: k, b: key });
    let (hit, _) = b.block(&[]);
    b.br_if(eq, hit, miss);
    b.at(hit);
    ent
}

/// `get(h, key) -> Option<V>`。
fn b_get_fn(kt: Ty, vt: Ty) -> Func {
    let opt = Ty::Opt(Pay::of(vt).expect("BtreeMap の値は Option のペイロードになる"));
    let mut b = Fb::new(&[Ty::Ptr, kt]);
    let pos = b.emit(Ty::Usize, Op::Call { callee: b_lower_name(kt), args: vec![0, 1] });
    let len = b.load(Ty::Usize, 0, 8);
    let inside = b.cmp(Cc::Lt, pos, len);
    let (chk, _) = b.block(&[]);
    let (no, _) = b.block(&[]);
    let (join, jv) = b.block(&[opt]);
    b.br_if(inside, chk, no);
    b.at(chk);
    let ent = b_found(&mut b, kt, pos, 1, no);
    let v = b.load(vt, ent, 8);
    let s = b.emit(opt, Op::Some(v));
    b.goto(join, vec![s]);
    b.at(no);
    let n = b.emit(opt, Op::Const(0));
    b.goto(join, vec![n]);
    b.at(join);
    b.term(Term::Ret(jv[0]));
    b.build(&b_name(kt, vt, "get"), &[], opt)
}

/// `insert(h, key, val) -> Option<V>` (同じキーがあれば値を替えて古い値を返す。なければ挿入して `None`)。
fn b_insert_fn(kt: Ty, vt: Ty) -> Func {
    let opt = Ty::Opt(Pay::of(vt).expect("BtreeMap の値は Option のペイロードになる"));
    let mut b = Fb::new(&[Ty::Ptr, kt, vt]);
    let (key, val): (VId, VId) = (1, 2);
    let pos = b.emit(Ty::Usize, Op::Call { callee: b_lower_name(kt), args: vec![0, key] });
    let len = b.load(Ty::Usize, 0, 8);
    let inside = b.cmp(Cc::Lt, pos, len);
    let (chk, _) = b.block(&[]);
    let (ins, _) = b.block(&[]);
    let (join, jv) = b.block(&[opt]);
    b.br_if(inside, chk, ins);
    // 同じキーがあれば値を替える
    b.at(chk);
    let ent = b_found(&mut b, kt, pos, key, ins);
    let old = b.load(vt, ent, 8);
    b.store(vt, ent, 8, val);
    let s = b.emit(opt, Op::Some(old));
    b.goto(join, vec![s]);
    // 挿入: 満杯なら広げ、`pos` から後ろの組を 1 つずらして書く
    b.at(ins);
    let cap = b.load(Ty::Usize, 0, 16);
    let full = b.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::Usize, a: len, b: cap });
    let (grow, _) = b.block(&[]);
    let (moved, _) = b.block(&[]);
    b.br_if(full, grow, moved);
    b.at(grow);
    let oldbuf = b.load(Ty::Ptr, 0, 0);
    let two = b.c(Ty::Usize, 2);
    let c2 = b.mul(cap, two);
    let four = b.c(Ty::Usize, 4);
    let ncap = b.add(c2, four);
    let bytes = {
        let k = b.c(Ty::Usize, BENT);
        b.mul(ncap, k)
    };
    let np = b.emit(Ty::Ptr, Op::HeapRealloc { p: oldbuf, size: bytes });
    b.store(Ty::Ptr, 0, 0, np);
    b.store(Ty::Usize, 0, 16, ncap);
    b.goto(moved, vec![]);
    b.at(moved);
    let (sh, sp) = b.block(&[Ty::Usize]);
    let (shb, _) = b.block(&[]);
    let (after, _) = b.block(&[]);
    b.goto(sh, vec![len]);
    b.at(sh);
    let i = sp[0];
    let more = b.cmp(Cc::Gt, i, pos);
    b.br_if(more, shb, after);
    b.at(shb);
    let im = {
        let one = b.c(Ty::Usize, 1);
        b.sub(i, one)
    };
    let buf = b.load(Ty::Ptr, 0, 0);
    let src = b_ent(&mut b, buf, im);
    let dst = b_ent(&mut b, buf, i);
    let k = b.load(kt, src, 0);
    let v = b.load(vt, src, 8);
    b.store(kt, dst, 0, k);
    b.store(vt, dst, 8, v);
    b.goto(sh, vec![im]);
    b.at(after);
    let buf = b.load(Ty::Ptr, 0, 0);
    let ent = b_ent(&mut b, buf, pos);
    b.store(kt, ent, 0, key);
    b.store(vt, ent, 8, val);
    let one = b.c(Ty::Usize, 1);
    let nl = b.add(len, one);
    b.store(Ty::Usize, 0, 8, nl);
    let n = b.emit(opt, Op::Const(0));
    b.goto(join, vec![n]);
    b.at(join);
    b.term(Term::Ret(jv[0]));
    b.build(&b_name(kt, vt, "insert"), &["alloc"], opt)
}

/// `remove(h, key) -> Option<V>`: 見つかれば後ろの組を 1 つ前へずらし、`len` を減らして古い値を返す。
fn b_remove_fn(kt: Ty, vt: Ty) -> Func {
    let opt = Ty::Opt(Pay::of(vt).expect("BtreeMap の値は Option のペイロードになる"));
    let mut b = Fb::new(&[Ty::Ptr, kt]);
    let pos = b.emit(Ty::Usize, Op::Call { callee: b_lower_name(kt), args: vec![0, 1] });
    let len = b.load(Ty::Usize, 0, 8);
    let inside = b.cmp(Cc::Lt, pos, len);
    let (chk, _) = b.block(&[]);
    let (no, _) = b.block(&[]);
    let (join, jv) = b.block(&[opt]);
    b.br_if(inside, chk, no);
    b.at(chk);
    let ent = b_found(&mut b, kt, pos, 1, no);
    let old = b.load(vt, ent, 8);
    let (sh, sp) = b.block(&[Ty::Usize]);
    let (shb, _) = b.block(&[]);
    let (after, _) = b.block(&[]);
    b.goto(sh, vec![pos]);
    b.at(sh);
    let i = sp[0];
    let one = b.c(Ty::Usize, 1);
    let i1 = b.add(i, one);
    let more = b.cmp(Cc::Lt, i1, len);
    b.br_if(more, shb, after);
    b.at(shb);
    let buf = b.load(Ty::Ptr, 0, 0);
    let src = b_ent(&mut b, buf, i1);
    let dst = b_ent(&mut b, buf, i);
    let k = b.load(kt, src, 0);
    let v = b.load(vt, src, 8);
    b.store(kt, dst, 0, k);
    b.store(vt, dst, 8, v);
    let i2 = b.add(i, one);
    b.goto(sh, vec![i2]);
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
    b.build(&b_name(kt, vt, "remove"), &[], opt)
}

/// 補助の関数を登録する (`need` と同じ作法。`lower` は `get` / `insert` / `remove` が呼ぶ)。
fn b_need(lw: &mut Lw, kt: Ty, vt: Ty, ops: &[&str]) {
    lw.coll.extra.push((b_lower_fn(kt), BTreeSet::new()));
    let deps: BTreeSet<String> = [b_lower_name(kt)].into_iter().collect();
    for op in ops {
        let f = match *op {
            "get" => b_get_fn(kt, vt),
            "remove" => b_remove_fn(kt, vt),
            _ => b_insert_fn(kt, vt),
        };
        lw.coll.extra.push((f, deps.clone()));
        lw.callees.insert(b_name(kt, vt, op));
    }
}

/// `BtreeMap` のキーの値 (整数・bool・char)。`&k` と `&` の参照は値に直す。
fn b_scalar(lw: &mut Lw, a: &V, line: usize) -> LR<Option<VId>> {
    let operand = if nname(a) == "Unary" && st(&nf(a)[0]) == "&" { &nf(a)[1] } else { a };
    let Some(mut v) = lw.expr(operand)? else { return Ok(None) };
    if let Ty::Ref(_, p) = lw.vty(v) {
        v = lw.emit(p.ty(), Op::Load(v));
    }
    let t = lw.vty(v);
    if !scalar_word(t) {
        return lw.err(line, format!("BtreeMap のキー・値の型 {} は段階 1 の対象外 (整数・bool・char だけ)", t.name()));
    }
    Ok(Some(v))
}

/// 受け手の種類から (K, V) を決める。未確定なら `learn` で補う (`insert` のとき)。
fn b_types(lw: &mut Lw, slot: Option<usize>, kind: &Kind, learn: Option<(Ty, Ty)>, line: usize) -> LR<(Ty, Ty)> {
    match (kind, learn) {
        (Kind::BMap(Some(kv)), _) => Ok(*kv),
        (Kind::BMap(None), Some(kv)) => {
            if let Some(s) = slot {
                lw.coll.kinds.insert(s, Kind::BMap(Some(kv)));
            }
            Ok(kv)
        }
        _ => lw.err(line, "BtreeMap のキー・値の型が決まらない"),
    }
}

/// `m.insert(k, v)` / `get` / `contains_key` / `remove` / `len` / `is_empty` (`BtreeMap`)。受け手のヘッダは `h`。
pub(crate) fn bmap_method(lw: &mut Lw, e: &V, slot: Option<usize>, kind: &Kind, h: VId, m: &str, args: &[V]) -> LR<Option<VId>> {
    let line = nline(e);
    // 文字列のキー (`&str` / `String`) は別の表現 (`kir_lower_mapstr.rs`)。`insert` で初めて決まる表はキーの式の型から。
    let skey = match kind {
        Kind::BMap(Some((kt, _))) if *kt == Ty::Ptr || lw.aggs.borrow().is_vec(*kt) => Some(*kt),
        Kind::BMap(None) if m == "insert" && args.len() == 2 => match lw.src_ty(&args[0]) {
            Ok(crate::types::Ty::Named(n, _)) if n == "alloc.string.String" => Some(lw.aggs.borrow_mut().vec_hdr()),
            Ok(crate::types::Ty::Ref(_, i)) if matches!(&*i, crate::types::Ty::Prim(s) if s == "str") => Some(Ty::Ptr),
            _ => None,
        },
        _ => None,
    };
    if let Some(kt) = skey {
        return super::mapstr_lower::method(lw, e, slot, kind, h, m, args, kt);
    }
    match (m, args.len()) {
        ("len", 0) => Ok(Some(lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 }))),
        ("is_empty", 0) => {
            let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
            let z = lw.emit(Ty::Usize, Op::Const(0));
            Ok(Some(lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: len, b: z })))
        }
        ("insert", 2) => {
            let Some(key) = b_scalar(lw, &args[0], line)? else { return Ok(None) };
            let Some(val) = b_scalar(lw, &args[1], line)? else { return Ok(None) };
            let (kt, vt) = (lw.vty(key), lw.vty(val));
            let (kt0, vt0) = b_types(lw, slot, kind, Some((kt, vt)), line)?;
            if (kt0, vt0) != (kt, vt) {
                return lw.err(line, "insert の引数の型が BtreeMap のキー・値の型と違う");
            }
            let rt = lw.ty_of(e)?;
            let Ty::Res(Pay::Opt(inner), _) = rt else {
                return lw.err(line, "insert の結果の型が Result<Option<V>, AllocError> でない");
            };
            b_need(lw, kt, vt, &["insert"]);
            let ot = Ty::Opt(*inner);
            let o = lw.emit(ot, Op::Call { callee: b_name(kt, vt, "insert"), args: vec![h, key, val] });
            Ok(Some(lw.emit(rt, Op::Ok(o))))
        }
        ("get", 1) | ("contains_key", 1) | ("remove", 1) => {
            let Some(key) = b_scalar(lw, &args[0], line)? else { return Ok(None) };
            let kt = lw.vty(key);
            let (kt0, vt) = b_types(lw, slot, kind, None, line)?;
            if kt0 != kt {
                return lw.err(line, "キーの型が BtreeMap のキーの型と違う");
            }
            let op = if m == "remove" { "remove" } else { "get" };
            b_need(lw, kt, vt, &[op]);
            let Some(vp) = Pay::of(vt) else { return lw.err(line, "BtreeMap の値の型が Option に入らない") };
            let ot = Ty::Opt(vp);
            let o = lw.emit(ot, Op::Call { callee: b_name(kt, vt, op), args: vec![h, key] });
            if m == "contains_key" {
                return Ok(Some(lw.emit(Ty::Bool, Op::Tag(o))));
            }
            Ok(Some(o))
        }
        _ => lw.err(line, format!("BtreeMap のメソッド {} は段階 1 の対象外", m)),
    }
}
