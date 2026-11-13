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

use crate::check::{is_nil, items, nf, nlineno as nline, nname, st, strs};
use crate::kir::{AggKind, Aop, Cc, Mode, Op, Pointee, Ty, VId};
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

/// 受け手を渡す形: (引数の値, 書き戻す局所 (スロット, アドレス, 値の型), call_method で tmp_hold するか)。
/// `path_read` (`Path`) でスコープから移した値や、`&self` の alloca コピーはスコープ終了で解放されるので `false`。
/// `Call`/`MCall` の戻り値（スコープにまだ登録されていない一時値）は `true`。
type Recv = Option<(VId, Option<(usize, VId, Ty)>, bool)>;

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
        if m == "clone" {
            return Some(self.lower_clone_mcall(e, nline(e), &rt, &f[0]));
        }
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

    /// `E.V.m(..)` (列挙子の値 `E.V` へのメソッド呼び出し)。受け手は列挙子の値の式 (`E.V`)。メソッドでなければ `None`。
    pub(super) fn variant_method_call(&mut self, e: &V, en: &str, m: &str) -> Option<LR<Option<VId>>> {
        let mut segs: Vec<String> = en.split('.').map(String::from).collect();
        segs.push(m.to_string());
        let di = match self.p.resolve(&segs, &self.def.file)?.target {
            Tgt::Fn(di) if self.p.defs[di].is_method() => di,
            _ => return None,
        };
        let f = nf(e);
        let callee = strs(&nf(&f[0])[0]);
        let recv_segs: Vec<V> = callee[..callee.len() - 1].iter().map(|x| V::S(x.clone())).collect();
        let recv = V::Node("Path", vec![V::L(recv_segs), V::Nil], nline(e), 0);
        let args = items(&f[1]).to_vec();
        Some(self.call_method(e, di, Place::Expr(&recv), &args))
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
        let Some((rv, copy_back, need_hold)) = (match place {
            Place::Path(segs) => self.recv_path(&segs, self_ty, by_ref, want_mut, line)?,
            Place::Expr(r) => self.recv_expr(r, self_ty, by_ref, want_mut, line)?,
        }) else {
            return Ok(None);
        };
        if self.vty(rv) != self_ty {
            return self.err(line, format!("メソッド {} の受け手の型が合わない", cd.name));
        }
        // `Call`/`MCall` の戻り値（スコープにまだ登録されていない一時値）だけ tmp_hold する。
        // `Path` で移した値や `&self` の alloca コピーはスコープ終了で解放される。
        if need_hold {
            super::coll::tmp_hold(self, rv);
        }
        let mut vs = vec![];
        for a in args {
            match self.expr(a)? {
                Some(v) => {
                    super::coll::tmp_hold(self, v);
                    vs.push(v)
                }
                None => {
                    super::coll::tmp_release(self, &vs);
                    if need_hold {
                        super::coll::tmp_release(self, &[rv]);
                    }
                    return Ok(None);
                }
            }
        }
        super::coll::tmp_release(self, &vs);
        if need_hold {
            super::coll::tmp_release(self, &[rv]);
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
                // 値の受け手は関数の引数と同じ移動 (path_read: ヘッダを移し/印を下ろす)。
                // 呼び先が所有を引き継ぐので、呼び出し元での二重解放を防ぐ (設計_KIR.md 17.1)。
                let v = super::coll::path_read(self, s);
                // 移した値は、呼び出しまでの間 (引数の `?` で抜ける経路) は一時値として持つ。
                let hold = super::coll::owns(self, self.vty(v));
                return Ok(Some((v, None, hold)));
            }
            // アドレスを取られた局所: スタックスロットのアドレス (値の番号は別の参照の値に写す)
            if self.is_mem_val(sv) {
                let Some(p) = Pointee::of(stt) else {
                    return self.err(line, format!("受け手 {} の型は段階 1 の対象外", segs[0]));
                };
                return Ok(Some((self.ref_value(sv, p, !want_mut), None, false)));
            }
            // 参照の値 (引数の `&mut` や `let r = &mut x`)
            if let Ty::Ref(m, _) = self.vty(sv) {
                if want_mut && !m {
                    return self.err(line, "共有参照から可変参照は取れない (メソッドの受け手)");
                }
                return Ok(Some((self.coerce(sv, self_ty), None, false)));
            }
            // SSA の局所: 一時のスタックに写して、`&mut` のときは呼んだあとに書き戻す。
            // `sv` はスコープの局所変数の値なので、ここで tmp_hold せずスコープ終了で解放される (tmp_v=None)。
            let Some(p) = Pointee::of(stt) else {
                return self.err(line, format!("受け手 {} の型は段階 1 の対象外", segs[0]));
            };
            let ptr = self.alloca(p);
            self.emit(Ty::Unit, Op::Store { ptr, val: sv });
            let rv = self.ref_value(ptr, p, !want_mut);
            let back = if want_mut { Some((s, ptr, stt)) } else { None };
            return Ok(Some((rv, back, false)));
        }
        // `x.f.g`
        if !by_ref {
            let base = super::coll::path_read(self, s);
            let v = self.field_path(base, &segs[1..], line)?;
            return Ok(Some((v, None, false)));
        }
        // 参照の値・スタックスロットを根とするなら、そのアドレスをたどる。SSA の集成体を根とするときは一時に写す (`&mut` は書き戻す)。
        // `sv` はスコープの局所変数の値なので、ここで tmp_hold せずスコープ終了で解放される (tmp_v=None)。
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
        Ok(Some((self.coerce(cur, self_ty), back, false)))
    }

    /// 式の受け手。`*r` は指す先そのもの。値の受け手はそのまま、`&` の受け手は一時に写す。
    fn recv_expr(&mut self, r: &V, self_ty: Ty, by_ref: bool, want_mut: bool, line: usize) -> LR<Recv> {
        if by_ref && nname(r) == "Unary" && st(&nf(r)[0]) == "*" {
            let Some(rv) = self.expr(&nf(r)[1])? else { return Ok(None) };
            if want_mut && matches!(self.vty(rv), Ty::Ref(false, _)) {
                return self.err(line, "共有参照を通した &mut の受け手は段階 1 の対象外");
            }
            return Ok(Some((self.coerce(rv, self_ty), None, false)));
        }
        let Some(v) = self.expr(r)? else { return Ok(None) };
        if !by_ref {
            // `Path` の `Owned::Val`（スロットあり）はスコープ終了で drop されるので tmp_hold しない。
            // 一時値（Call の戻り値など）はスコープにまだ登録されていないので tmp_hold する。
            let need_hold = match (nname(r), super::coll::local_slot(self, r)) {
                ("Path", Some(slot)) if super::coll::val_slot_of(self, slot) => false,
                _ => true,
            };
            return Ok(Some((v, None, need_hold)));
        }
        if want_mut {
            return self.err(line, "&mut の受け手は名前・フィールド・*参照 だけ段階 1 の対象");
        }
        let Some(p) = Pointee::of(self.vty(v)) else {
            return self.err(line, "受け手の型は段階 1 の対象外");
        };
        let ptr = self.alloca(p);
        self.emit(Ty::Unit, Op::Store { ptr, val: v });
        // `v` のコピーを alloca (フレーム領域) に置いた。元の `v` がローカル変数 (Owned::Val) ならスコープ終了で既に解放されるので tmp_hold しない。
        // 一時値ならスコープ終了で解放されるよう tmp_hold する。alloca の中身はフレームとともに解放。二重解放を防ぐ (設計_KIR.md 17.1)。
        let is_local_val = nname(r) == "Path" && super::coll::local_slot(self, r).map_or(false, |s| super::coll::val_slot_of(self, s));
        if !is_local_val {
            super::coll::tmp_hold(self, v);
        }
        Ok(Some((self.ref_value(ptr, p, true), None, false)))
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
                // 所有物を持つ構造体の局所のフィールドからの移動 (`b.items` を値で使う) は、SSA の局所へ「空」を書き戻す。
                // 値で使うかどうかをここでは見分けず、根を全部入れる (合流で余分に受け渡すだけで害はない)。
                if *n == "Path" {
                    let segs = strs(&fs[0]);
                    if segs.len() >= 2 {
                        if let Some(s) = self.lookup(&segs[0]) {
                            if super::coll::owns(self, self.slots[s].1) {
                                out.push(segs[0].clone());
                            }
                        }
                    }
                }
                if *n == "Field" {
                    let mut r = &fs[0];
                    while nname(r) == "Field" {
                        r = &nf(r)[0];
                    }
                    if nname(r) == "Path" {
                        let root = strs(&nf(r)[0])[0].clone();
                        if let Some(s) = self.lookup(&root) {
                            if super::coll::owns(self, self.slots[s].1) {
                                out.push(root);
                            }
                        }
                    }
                }
                // 利用者の反復子の `for x in it`: `next` が進めた `it` を、外側の合流へ渡す (設計_KIR.md 17.4)。
                if *n == "For" && fs.len() >= 2 && nname(&fs[1]) == "Path" {
                    let segs = strs(&nf(&fs[1])[0]);
                    if segs.len() == 1 {
                        out.push(segs[0].clone());
                    }
                }
                if *n == "Call" && fs.len() >= 2 && nname(&fs[0]) == "Path" {
                    let segs = strs(&nf(&fs[0])[0]);
                    if segs.len() >= 2 {
                        out.extend(self.mut_root_of(&segs[..segs.len() - 1], &segs[segs.len() - 1]));
                    }
                    if segs.len() >= 3 {
                        out.extend(super::own::vec_field_root(self, &segs[..segs.len() - 1]));
                    }
                }
                if *n == "MCall" && fs.len() >= 2 {
                    if let Some(segs) = super::agg::field_segs(&fs[0]) {
                        out.extend(self.mut_root_of(&segs, &st(&fs[1])));
                        out.extend(super::own::vec_field_root(self, &segs));
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

    /// `e.clone()` (derive [Clone]) の下ろし (段階 2a/2b)。`recv_expr` は受け手の式。
    /// すべてスカラーの欄を持つ構造体・enum・タプルなら値の写しを返す。
    /// String / Vec<スカラー> の欄を持つ構造体なら新規確保して中身を複写する。
    /// それ以外は `None` を返し、未対応とする。
    fn lower_clone_mcall(&mut self, _e: &V, line: usize, _rt: &Src, recv_expr: &V) -> LR<Option<VId>> {
        let Some(recv) = self.expr_peek(recv_expr)? else { return Ok(None) };
        let recv_ty = self.vty(recv);
        self.lower_clone(line, recv, recv_ty)
    }

    /// `recv` (型 `recv_ty`: `Ty::Agg` または `Ty::Ref<_, Pointee::Agg>`) の clone を下ろす。
    /// すべてスカラーなら値をそのまま返す (KIR の値は不変)。
    /// struct の String / Vec 欄は新規確保してバイトを複写する。
    /// それ以外は `None`。
    pub(super) fn lower_clone(&mut self, line: usize, recv: VId, recv_ty: Ty) -> LR<Option<VId>> {
        let ai = match recv_ty {
            Ty::Agg(ai) | Ty::Ref(_, Pointee::Agg(ai)) => ai,
            _ => return self.err(line, "clone の受け手が集成体ではない"),
        };
        let agg = self.agg_def(ai);
        let name = self.aggs.borrow().defs[ai as usize].name.clone();
        let def = self.p.defs.iter().find(|d| d.name == name)
            .ok_or_else(|| format!("型 {} の定義がない", name))?;
        if !def.derive.iter().any(|x| x == "Clone") {
            return self.err(line, format!("clone は derive [Clone] を持つ型だけ段階 1 の対象"));
        }
        // 参照なら値を読み出す
        let val = match recv_ty {
            Ty::Ref(_, p) => self.emit(p.ty(), Op::Load(recv)),
            Ty::Agg(_) => recv,
            _ => return self.err(line, "clone の受け手が集成体ではない"),
        };
        // struct: 欄ごとにコピー。String / Vec は新規確保してバイトを複写。
        let fs = match &agg.kind {
            AggKind::Struct(fs) => fs,
            // enum / tuple: すべてスカラーなら値の写し、それ以外は未対応
            kind => {
                let all_scalar = match kind {
                    AggKind::Enum(vs) => vs.iter().all(|v| v.fields.iter().all(|f| f.ty.is_scalar())),
                    AggKind::Tuple(fs) => fs.iter().all(|f| f.ty.is_scalar()),
                    AggKind::Struct(_) => unreachable!(),
                };
                if all_scalar {
                    return Ok(Some(val));
                }
                // `Ok(None)` は「式が発散する」の意味なので使わない (Fable の最終レビュー B2)。
                return self.err(line, "clone の対象のペイロードに集成体がある (段階 1 の対象外)");
            }
        };
        let mut new_fields = vec![];
        for f in fs {
            let fv = self.emit(f.ty, Op::Field { a: val, off: f.off });
            if f.ty.is_scalar() {
                new_fields.push(fv);
            } else if self.aggs.borrow().is_vec(f.ty) {
                let nfv = self.clone_owned_field(line, fv, f.ty)?;
                new_fields.push(nfv);
            } else {
                return self.err(line, "clone の対象の欄に入れ子の集成体がある (段階 1 の対象外)");
            }
        }
        Ok(Some(self.emit(Ty::Agg(ai), Op::Make { variant: None, fields: new_fields })))
    }

    /// Vec / String の欄を深いコピーする: 新しいバッファを確保し、バイトを複写する。
    /// `fv` は Vec/String の集成体値 (ptr@0, len@8, cap@16)。
    fn clone_owned_field(&mut self, _line: usize, fv: VId, fty: Ty) -> LR<VId> {
        let ptr = self.emit(Ty::Ptr, Op::Field { a: fv, off: 0 });
        let len = self.emit(Ty::Usize, Op::Field { a: fv, off: 8 });
        let cap = self.emit(Ty::Usize, Op::Field { a: fv, off: 16 });
        // 新しいバッファを確保 (HeapAlloc は uses [alloc] を要る)
        let new_ptr = self.emit(Ty::Ptr, Op::HeapAlloc(len));
        // バイトを複写: for i in 0..len { new_ptr[i] = ptr[i] }
        let zero = self.emit(Ty::Usize, Op::Const(0));
        let one = self.emit(Ty::Usize, Op::Const(1));
        let (hd, hp) = self.new_block(&[Ty::Usize]);
        let (body, _) = self.new_block(&[]);
        let (exit, _) = self.new_block(&[]);
        self.goto(hd, vec![zero]);
        self.cur = Some(hd);
        let i = hp[0];
        let c = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: i, b: len });
        self.branch(c, (body, vec![]), (exit, vec![]));
        self.cur = Some(body);
        let old_addr = self.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: i });
        let byte = self.emit(Ty::U8, Op::MemLoad { p: old_addr, off: 0 });
        let new_addr = self.emit(Ty::Ptr, Op::PtrAdd { p: new_ptr, off: i });
        self.emit(Ty::Unit, Op::MemStore { ty: Ty::U8, p: new_addr, off: 0, v: byte });
        let i1 = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: i, b: one });
        self.goto(hd, vec![i1]);
        self.cur = Some(exit);
        // 新しい Vec/String 集成体を作る
        Ok(self.emit(fty, Op::Make { variant: None, fields: vec![new_ptr, len, cap] }))
    }
}
