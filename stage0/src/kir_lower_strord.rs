//! KIR の下ろし: 文字列の比較 (`<` / `<=` / `>` / `>=` / `cmp` / `partial_cmp`) と、`const` の `&str` へのメソッド呼び出し。
//! (旧 `kir_lower_mapstr.rs` の、文字列キーのマップに依らない部分。文字列のキーのマップは段 2 で Kasane 版へ移した。)
//! `cmp` の補助 (`ksn.bs.cmp`) は、文字列の辞書順の比較を 1 つの関数にしたもの。

use std::collections::BTreeSet;

use crate::check::{nf, nlineno as nline, nname, st};
use crate::kir::{Cc, Func, Op, Pay, Term, Ty, VId};
use crate::parser::V;

use super::coll::{self, Kind};
use super::fb::Fb;
use super::{Lw, LR};

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

impl<'a> Lw<'a> {
    /// `K.m(args)` (`K` は整数・浮動小数の `const`。`core.num.i64.MIN.checked_div(-1i64)` など)。
    /// 定数の値を作って局所に束縛し、`x.m(args)` (局所の受け手) の呼び出しに書き換えて、既存の下ろしに渡す。
    /// 受け手が整数・浮動小数の定数でなければ `None` (親の処理へ)。
    pub(super) fn const_scalar_call(&mut self, e: &V) -> Option<LR<Option<VId>>> {
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
        if r.rest.len() != 1 {
            return None;
        }
        let declared = crate::types::from_v(&self.p.defs[ci].ret, &std::collections::HashSet::new());
        let is_num = matches!(&declared, Src::Prim(p) if Ty::parse(p).is_some_and(|t| t.is_int() || t.is_float()));
        if !is_num {
            return None;
        }
        // 定数の式は下ろしが作る (型表にない) ので、番号 0 の式として渡す。型は宣言から決まる。
        let recv = match self.const_val(ci, &V::Nil) {
            Ok(v) => v,
            Err(why) => return Some(Err(why)),
        };
        let tmp = format!("$const{}", self.slots.len());
        let t = self.vty(recv);
        self.bind(&tmp, recv, t);
        let V::Node(pn, pf, pl, pid) = &f[0] else { return None };
        let mut pf = pf.clone();
        pf[0] = V::L(vec![V::S(tmp), V::S(r.rest[0].clone())]);
        let mut nfv = f.to_vec();
        nfv[0] = V::Node(pn, pf, *pl, *pid);
        let V::Node(en, _, el, eid) = e else { return None };
        Some(self.call(&V::Node(en, nfv, *el, *eid)))
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

