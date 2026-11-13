//! 利用者の型のメソッド呼び出し `x.m(..)` (段階 1 の続き。設計_KIR.md 10.10 の参照の仕組みを使う)。
//!
//! `kir_lower` の子モジュールとして `Lw` の非公開の項目を使う。
//!
//! - 呼ぶのは利用者の型 `S` のメソッド `fn S.m(self: ..)`。`self` が `&mut S` / `&S` なら受け手のアドレスを渡す。
//! - 受け手が局所 `x` (または `x.f.g`):
//!   - 参照の値 (`self` や `&mut` の引数) はその値、アドレスを取られた局所 (スタックスロット) はそのアドレス。
//!   - SSA の局所は一時のスタックに写して、呼んだあとに値を書き戻す (`&mut` のとき)。
//!   - `x.f.g` は x が参照かスタックスロットのときだけ (フィールドのアドレスをたどる)。値のフィールドの `&mut` は対象外。
//! - 受け手が `*r` なら、指す先そのもの。その他の式は値 (`&mut` の受け手は対象外、`&` の受け手は一時に写す)。
//! - 呼び先の型は型表から決める。メソッドでないと分かった呼び出しは、何も下ろさずに親へ返す (`None`)。
//! - 型引数付きのメソッドは対象外。

use std::collections::HashSet;

use crate::check::{is_nil, items, nf, nline, nname, st, strs};
use crate::kir::{AggKind, Op, Pointee, Ty, VId};
use crate::parser::V;
use crate::program::Target as Tgt;
use crate::types::{from_v, Ty as Src};

use super::{coll, Lw, LR};

/// 受け手の型の名前 (利用者の型の FQN)。`S` / `&S` / `&mut S` のとき `S`。
fn recv_type_name(t: &Src) -> Option<String> {
    match t {
        Src::Named(n, a) if a.is_empty() => Some(n.clone()),
        Src::Ref(_, inner) => recv_type_name(inner),
        _ => None,
    }
}

/// 受け手の置き場所 (呼び出しの形ごと)。
pub(super) enum Place<'v> {
    /// 局所の名前 `x` か、フィールドの経路 `x.f.g` (`segs` の先頭が局所)。
    Path(Vec<String>),
    /// 式 (`*r` は参照をたどる)。
    Expr(&'v V),
}

/// 受け手を渡す形: (引数の値, 書き戻す局所 (スロット, アドレス, 値の型))。
type Recv = Option<(VId, Option<(usize, VId, Ty)>)>;

impl<'a> Lw<'a> {
    /// 型 `t` (集成体かその参照) の利用者のメソッド `m` の定義の添字。
    pub(super) fn method_of_ty(&self, t: Ty, m: &str) -> Option<usize> {
        let ai = match t {
            Ty::Agg(ai) | Ty::Ref(_, Pointee::Agg(ai)) => ai,
            _ => return None,
        };
        let name = self.aggs.borrow().defs[ai as usize].name.clone();
        let mut segs: Vec<String> = name.split('.').map(String::from).collect();
        segs.push(m.to_string());
        match self.p.resolve(&segs, &self.def.file)?.target {
            Tgt::Fn(di) if self.p.defs[di].is_method() => Some(di),
            _ => None,
        }
    }

    /// 型 `t` の構造体のフィールド `n` の型 (計算だけで、命令は作らない)。
    fn field_ty_of(&self, t: Ty, n: &str) -> Option<Ty> {
        let ai = match t {
            Ty::Agg(ai) | Ty::Ref(_, Pointee::Agg(ai)) => ai,
            _ => return None,
        };
        let aggs = self.aggs.borrow();
        match &aggs.defs[ai as usize].kind {
            AggKind::Struct(fs) => fs.iter().find(|f| f.name == n).map(|f| f.ty),
            _ => None,
        }
    }

    /// メソッドの `self` が `&mut` か。
    pub(super) fn method_takes_mut(&self, di: usize) -> bool {
        self.p.defs[di].params.first().map_or(false, |(_, t)| matches!(from_v(t, &HashSet::new()), Src::Ref(true, _)))
    }

    /// `x.f.g.m(..)` の形 (`Call` で経路の先頭が局所) の利用者のメソッド。対象外なら `None` (何も下ろさない)。
    pub(super) fn user_path_call(&mut self, e: &V) -> Option<LR<Option<VId>>> {
        let f = nf(e);
        if nname(&f[0]) != "Path" {
            return None;
        }
        let segs = strs(&nf(&f[0])[0]);
        if segs.len() < 2 {
            return None;
        }
        let s = self.lookup(&segs[0])?;
        let mut t = self.slots[s].1;
        for n in &segs[1..segs.len() - 1] {
            t = self.field_ty_of(t, n)?;
        }
        let di = self.method_of_ty(t, &segs[segs.len() - 1])?;
        let args = items(&f[1]).to_vec();
        let recv = segs[..segs.len() - 1].to_vec();
        Some(self.call_method(e, di, Place::Path(recv), &args))
    }

    /// `e.m(..)` の形 (受け手が式) の利用者のメソッド。対象外なら `None`。
    pub(super) fn user_mcall(&mut self, e: &V) -> Option<LR<Option<VId>>> {
        let f = nf(e);
        let rt = self.src_ty(&f[0]).ok()?;
        let name = recv_type_name(&rt)?;
        let m = st(&f[1]);
        let mut segs: Vec<String> = name.split('.').map(String::from).collect();
        segs.push(m);
        let di = match self.p.resolve(&segs, &self.def.file)?.target {
            Tgt::Fn(di) if self.p.defs[di].is_method() => di,
            _ => return None,
        };
        // 受け手が局所を根とするフィールドの鎖 (`self.acc`、`o.acc`) なら、名前の経路として扱う (アドレスをたどれる)
        let place = match super::agg::field_segs(&f[0]) {
            Some(s) if self.lookup(&s[0]).is_some() => Place::Path(s),
            _ => Place::Expr(&f[0]),
        };
        let args = items(&f[2]).to_vec();
        Some(self.call_method(e, di, place, &args))
    }

    /// 呼び出しの下ろし。`e` は呼び出しの式 (行の番号に使う)、`args` は受け手を除いた引数。
    pub(super) fn call_method(&mut self, e: &V, di: usize, place: Place, args: &[V]) -> LR<Option<VId>> {
        let line = nline(e);
        let p = self.p;
        let cd = &p.defs[di];
        if cd.generics.iter().any(|g| g.0 != "life") {
            return self.err(line, format!("メソッド {} は型引数付きで段階 1 の対象外", cd.name));
        }
        if cd.params.len() != args.len() + 1 {
            return self.err(line, format!("メソッド {} の引数の個数が違う", cd.name));
        }
        let gn = HashSet::new();
        let self_src = from_v(&cd.params[0].1, &gn);
        let (by_ref, want_mut) = match &self_src {
            Src::Ref(m, _) => (true, *m),
            _ => (false, false),
        };
        let Some(self_ty) = coll::sig_ty(self, &self_src) else {
            return self.err(line, format!("メソッド {} の self の型は段階 1 の対象外", cd.name));
        };
        let mut pts = vec![];
        for (_, t) in &cd.params[1..] {
            match coll::sig_ty(self, &from_v(t, &gn)) {
                Some(x) => pts.push(x),
                None => return self.err(line, format!("メソッド {} の引数の型は段階 1 の対象外", cd.name)),
            }
        }
        let ret = if is_nil(&cd.ret) {
            Ty::Unit
        } else {
            match coll::sig_ty(self, &from_v(&cd.ret, &gn)) {
                Some(x) => x,
                None => return self.err(line, format!("メソッド {} の戻り値の型は段階 1 の対象外", cd.name)),
            }
        };
        let Some((rv, copy_back)) = (match place {
            Place::Path(segs) => self.recv_path(&segs, self_ty, by_ref, want_mut, line)?,
            Place::Expr(r) => self.recv_expr(r, self_ty, by_ref, want_mut, line)?,
        }) else {
            return Ok(None);
        };
        if self.vty(rv) != self_ty {
            return self.err(line, format!("メソッド {} の受け手の型が合わない", cd.name));
        }
        let mut vs = vec![];
        for a in args {
            match self.expr(a)? {
                Some(v) => vs.push(v),
                None => return Ok(None),
            }
        }
        let vs = self.coerce_args(vs, &pts);
        for (v, pt) in vs.iter().zip(pts.iter()) {
            if self.vty(*v) != *pt {
                return self.err(line, format!("メソッド {} の引数の型が合わない", cd.name));
            }
        }
        let mut all = vec![rv];
        all.extend(vs);
        let name = cd.name.clone();
        self.callees.insert(name.clone());
        let r = self.emit(ret, Op::Call { callee: name, args: all });
        if let Some((slot, ptr, t)) = copy_back {
            let nv = self.emit(t, Op::Load(ptr));
            self.assign_slot(slot, nv);
        }
        Ok(Some(r))
    }

    /// 局所 (`x` または `x.f.g`) の受け手。
    fn recv_path(&mut self, segs: &[String], self_ty: Ty, by_ref: bool, want_mut: bool, line: usize) -> LR<Recv> {
        let Some(s) = self.lookup(&segs[0]) else {
            return self.err(line, format!("受け手 {} が局所でない", segs[0]));
        };
        let (sv, stt) = self.slots[s];
        if segs.len() == 1 {
            if !by_ref {
                let v = self.read_slot(s);
                return Ok(Some((v, None)));
            }
            // アドレスを取られた局所: スタックスロットのアドレス (値の番号は別の参照の値に写す)
            if self.is_mem_val(sv) {
                let Some(p) = Pointee::of(stt) else {
                    return self.err(line, format!("受け手 {} の型は段階 1 の対象外", segs[0]));
                };
                return Ok(Some((self.ref_value(sv, p, !want_mut), None)));
            }
            // 参照の値 (引数の `&mut` や `let r = &mut x`)
            if let Ty::Ref(m, _) = self.vty(sv) {
                if want_mut && !m {
                    return self.err(line, "共有参照から可変参照は取れない (メソッドの受け手)");
                }
                return Ok(Some((self.coerce(sv, self_ty), None)));
            }
            // SSA の局所: 一時のスタックに写して、`&mut` のときは呼んだあとに書き戻す
            let Some(p) = Pointee::of(stt) else {
                return self.err(line, format!("受け手 {} の型は段階 1 の対象外", segs[0]));
            };
            let ptr = self.alloca(p);
            self.emit(Ty::Unit, Op::Store { ptr, val: sv });
            let rv = self.ref_value(ptr, p, !want_mut);
            let back = if want_mut { Some((s, ptr, stt)) } else { None };
            return Ok(Some((rv, back)));
        }
        // `x.f.g`
        if !by_ref {
            let base = self.read_slot(s);
            let v = self.field_path(base, &segs[1..], line)?;
            return Ok(Some((v, None)));
        }
        // 参照の値・スタックスロットを根とするなら、そのアドレスをたどる。SSA の集成体を根とするときは一時に写す (`&mut` は書き戻す)。
        let (base, back) = if matches!(self.vty(sv), Ty::Ref(_, Pointee::Agg(_))) || self.is_mem_val(sv) {
            (sv, None)
        } else {
            let Some(p) = Pointee::of(stt) else {
                return self.err(line, format!("局所 {} の型は段階 1 の対象外", segs[0]));
            };
            let ptr = self.alloca(p);
            self.emit(Ty::Unit, Op::Store { ptr, val: sv });
            (ptr, if want_mut { Some((s, ptr, stt)) } else { None })
        };
        let mut cur = base;
        for n in &segs[1..] {
            cur = self.field_addr(cur, n, line)?;
        }
        Ok(Some((self.coerce(cur, self_ty), back)))
    }

    /// 式の受け手。`*r` は指す先そのもの。値の受け手はそのまま、`&` の受け手は一時に写す。
    fn recv_expr(&mut self, r: &V, self_ty: Ty, by_ref: bool, want_mut: bool, line: usize) -> LR<Recv> {
        if by_ref && nname(r) == "Unary" && st(&nf(r)[0]) == "*" {
            let Some(rv) = self.expr(&nf(r)[1])? else { return Ok(None) };
            if want_mut && matches!(self.vty(rv), Ty::Ref(false, _)) {
                return self.err(line, "共有参照を通した &mut の受け手は段階 1 の対象外");
            }
            return Ok(Some((self.coerce(rv, self_ty), None)));
        }
        let Some(v) = self.expr(r)? else { return Ok(None) };
        if !by_ref {
            return Ok(Some((v, None)));
        }
        if want_mut {
            return self.err(line, "&mut の受け手は名前・フィールド・*参照 だけ段階 1 の対象");
        }
        let Some(p) = Pointee::of(self.vty(v)) else {
            return self.err(line, "受け手の型は段階 1 の対象外");
        };
        let ptr = self.alloca(p);
        self.emit(Ty::Unit, Op::Store { ptr, val: v });
        Ok(Some((self.ref_value(ptr, p, true), None)))
    }

    /// 木の中で、利用者の `&mut self` のメソッドを呼ぶ局所 (`x.m(..)` の `x`、`x` は SSA の集成体) の名前。
    /// 代入と同じく、ループや分岐の合流で受け渡す値の集合に入れる (書き戻しが見えるように)。
    pub(super) fn mut_recv_names(&self, nodes: &[&V]) -> Vec<String> {
        let mut out = vec![];
        for n in nodes {
            self.scan_mut(n, &mut out);
        }
        out
    }

    /// `segs` (根の局所と、その後のフィールド) の集成体のメソッド `m` が `&mut self` なら、根の局所の名前。
    fn mut_root_of(&self, segs: &[String], m: &str) -> Option<String> {
        let s = self.lookup(&segs[0])?;
        let mut t = Some(self.slots[s].1);
        for n in &segs[1..] {
            t = t.and_then(|x| self.field_ty_of(x, n));
        }
        let di = t.and_then(|x| self.method_of_ty(x, m))?;
        self.method_takes_mut(di).then(|| segs[0].clone())
    }

    fn scan_mut(&self, v: &V, out: &mut Vec<String>) {
        match v {
            V::Node(n, fs, _, _) => {
                if *n == "Call" && fs.len() >= 2 && nname(&fs[0]) == "Path" {
                    let segs = strs(&nf(&fs[0])[0]);
                    if segs.len() >= 2 {
                        out.extend(self.mut_root_of(&segs[..segs.len() - 1], &segs[segs.len() - 1]));
                    }
                }
                if *n == "MCall" && fs.len() >= 2 {
                    if let Some(segs) = super::agg::field_segs(&fs[0]) {
                        out.extend(self.mut_root_of(&segs, &st(&fs[1])));
                    }
                }
                for f in fs {
                    self.scan_mut(f, out);
                }
            }
            V::L(xs) => {
                for x in xs {
                    self.scan_mut(x, out);
                }
            }
            _ => {}
        }
    }
}
