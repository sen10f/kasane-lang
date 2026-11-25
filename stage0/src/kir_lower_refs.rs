//! 参照 (`&T` / `&mut T`、`*r`、`*r = v`) の下ろし (設計_KIR.md 2 章・段階 1)。
//!
//! `kir_lower.rs` の子モジュールとして読む (`#[path]` で宣言している)。`Lw` の内部の状態を使うため。
//!
//! 方針:
//! - アドレスを取られる局所変数 (関数の中に `&x` / `&mut x` と書かれた名前) は、入口ブロックで確保した
//!   スタックスロット (`alloca`) に置く。読みは `load`、書きは `store`。それ以外の局所は従来どおり SSA の値。
//! - `&x` / `&mut x` のアドレスは `alloca` の値そのもの (`mutref<T>`)。`&` のときは `asshared` で `ref<T>` にする。
//!   式のままの値 (`&5` など) は一時のスロットに入れてから、そのアドレスを取る。
//! - 参照どうしの一致は型だけで決める。`&mut T` を `&T` の位置に渡すときは `asshared` を挟む。
//! - 借用の検査は済んでいる前提で、ここでは検査しない。指す先はスカラーと集成体 (設計_KIR.md 10.10)。
//!   Vec / String / `&str` / スライスの参照は、ヘッダ・`{ptr, len}` の値で渡す (`kir_lower_coll.rs`。ここでは扱わない)。
//! - `&x.f.g` / `&mut x.f.g` は、x のスロット (または x の参照) から `fieldaddr` をたどる。

use std::collections::BTreeSet;

use super::{path1, path_base, Lw, St, LR};
use crate::check::{nf, nlineno as nline, nname, st, strs};
use crate::kir::{Inst, Op, Pointee, Ty, VId};
use crate::parser::V;
use crate::types::Ty as Src;

/// 参照の下ろしの状態。`Lw` が 1 つ持つ。
#[derive(Default)]
pub(super) struct RefCx {
    /// この関数の中で `&x` / `&mut x` と書かれた名前。
    addr: BTreeSet<String>,
    /// スタックスロットの値番号 (`alloca` の結果)。ここに入る変数は `load` / `store` で読み書きする。
    mem: BTreeSet<VId>,
    /// 入口ブロックに置く `alloca` 命令。`flush_allocas` で先頭に入れる。
    allocas: Vec<Inst>,
}

impl RefCx {
    /// 関数の本体から、アドレスを取られる名前を集める。
    pub(super) fn begin(&mut self, body: &V) {
        let mut names = BTreeSet::new();
        addr_names(body, &mut names);
        self.addr = names;
    }

    /// `&x` / `&mut x` (または `capture [&x]`) と書かれた名前か。
    pub(super) fn is_addr_name(&self, n: &str) -> bool {
        self.addr.contains(n)
    }

    /// 値番号 `v` がスタックスロットなら true。
    pub(super) fn is_mem(&self, v: VId) -> bool {
        self.mem.contains(&v)
    }
}

/// 木の中で `&x` / `&mut x` の `x` (単一の名前) を集める。
fn addr_names(v: &V, out: &mut BTreeSet<String>) {
    match v {
        V::Node(n, fs, _, _) => {
            if *n == "Unary" && fs.len() == 2 && matches!(st(&fs[0]).as_str(), "&" | "&mut") {
                // `&x` も `&x.f.g` も x のアドレスが要る (フィールドのアドレスはスロットの先頭から数える)。
                if let Some(name) = path_base(&fs[1]) {
                    out.insert(name);
                }
            }
            // `capture [&x]` / `[&mut x]` は x のアドレスを取る。
            if *n == "Closure" && fs.len() >= 4 {
                if let V::L(caps) = &fs[2] {
                    for c in caps {
                        if let V::L(kv) = c {
                            if kv.len() == 2 && matches!(st(&kv[0]).as_str(), "&" | "&mut") {
                                out.insert(st(&kv[1]));
                            }
                        }
                    }
                }
            }
            for f in fs {
                addr_names(f, out);
            }
        }
        V::L(xs) => {
            for x in xs {
                addr_names(x, out);
            }
        }
        _ => {}
    }
}

/// 型 `&T` / `&mut T` を段階 1 の型へ写す。指す先がスカラーのときだけ (集成体への参照は `AggTab::lower_ty`)。
pub(super) fn conv_ref(m: bool, inner: &Src) -> Option<Ty> {
    match inner {
        Src::Prim(s) if s == "char" => Some(Ty::Ref(m, Pointee::of(Ty::U32)?)),
        Src::Prim(s) => Ty::parse(s).and_then(Pointee::of).map(|p| Ty::Ref(m, p)),
        _ => None,
    }
}

impl<'a> Lw<'a> {
    /// 名前 `name` の局所をスタックスロットに置く印 (`bind_let` より前に付ける。反復子のように、毎回番地で触る局所)。
    pub(super) fn mark_addr(&mut self, name: &str) {
        self.rf.addr.insert(name.to_string());
    }

    /// 局所変数 (let と引数) を束縛する。アドレスを取られる変数はスタックスロットに置く。
    pub(super) fn bind_let(&mut self, name: &str, v: VId, t: Ty) {
        match Pointee::of(t) {
            Some(p) if self.rf.addr.contains(name) => {
                let ptr = self.alloca(p);
                self.emit(Ty::Unit, Op::Store { ptr, val: v });
                self.rf.mem.insert(ptr);
                self.bind(name, ptr, t);
            }
            _ => self.bind(name, v, t),
        }
    }

    /// 値番号 `v` がスタックスロットか (他の子モジュールから使う入口)。
    pub(super) fn is_mem_val(&self, v: VId) -> bool {
        self.rf.is_mem(v)
    }

    /// 値番号 `v` をスタックスロットとして印す (`alloca` の結果をスロットに束ねるとき)。
    pub(super) fn mark_mem(&mut self, v: VId) {
        self.rf.mem.insert(v);
    }

    /// 入口ブロックに `alloca` を置く。結果は `mutref<p>`。
    pub(super) fn alloca(&mut self, p: Pointee) -> VId {
        let ty = Ty::Ref(true, p);
        let v = self.new_val(ty);
        self.rf.allocas.push(Inst {
            dst: v,
            ty,
            op: Op::Alloca,
        });
        v
    }

    /// 関数の本体を下ろし終えたら呼ぶ。`alloca` を入口ブロックの先頭に入れる。
    pub(super) fn flush_allocas(&mut self) {
        let allocas = std::mem::take(&mut self.rf.allocas);
        drop(self.blocks[0].insts.splice(0..0, allocas));
    }

    /// スロットの現在の値を読む。スタックスロットなら `load` を置く。
    pub(super) fn read_slot(&mut self, s: usize) -> VId {
        let (p, t) = self.slots[s];
        if self.rf.is_mem(p) {
            self.emit(t, Op::Load(p))
        } else {
            p
        }
    }

    /// スロットへ代入する。スタックスロットなら `store` を置く。
    pub(super) fn assign_slot(&mut self, s: usize, v: VId) {
        let (p, _) = self.slots[s];
        if self.rf.is_mem(p) {
            self.emit(Ty::Unit, Op::Store { ptr: p, val: v });
        } else {
            self.slots[s].0 = v;
        }
    }

    /// 参照の値を、`want` の参照に合わせる。`&mut T` を `&T` にする場合だけ `asshared` を置く。
    /// 合わないときは値をそのまま返す (呼び出し側の型検査が診断として扱う)。
    pub(super) fn coerce(&mut self, v: VId, want: Ty) -> VId {
        match (self.vty(v), want) {
            (have, want) if have == want => v,
            (Ty::Ref(true, p), Ty::Ref(false, q)) if p == q => self.emit(want, Op::AsShared(v)),
            _ => v,
        }
    }

    /// 比較の一方が `&T` の参照で、他方が `&スカラー` の値 (`Some(c)` の `c`) なら、参照の側を読んで値どうしにする。
    /// 参照どうしはそのまま (参照どうしの比較は段階 1 の対象外のまま)。
    pub(super) fn load_scalar_ref_pair(&mut self, a: VId, b: VId) -> (VId, VId) {
        if let Some(nb) = self.load_against(b, a) {
            return (a, nb);
        }
        if let Some(na) = self.load_against(a, b) {
            return (na, b);
        }
        (a, b)
    }

    /// `r` が参照で、`v` がその指す先と同じスカラーの値なら、`*r` を読んだ値を返す。
    fn load_against(&mut self, r: VId, v: VId) -> Option<VId> {
        match self.vty(r) {
            Ty::Ref(_, p) if Pointee::of(self.vty(v)) == Some(p) => Some(self.emit(p.ty(), Op::Load(r))),
            _ => None,
        }
    }

    /// 引数の値を、呼び先の引数の型に合わせる (参照の場合だけ)。
    pub(super) fn coerce_args(&mut self, vs: Vec<VId>, pts: &[Ty]) -> Vec<VId> {
        let mut out = Vec::with_capacity(vs.len());
        for (v, pt) in vs.into_iter().zip(pts.iter().copied()) {
            out.push(match pt {
                // `Some(c)` の値 (`&スカラー` の値表現) を `&T` の引数に渡す: 値を一時のスロットに入れ、その共有参照を渡す。
                // 共有参照は読むだけなので、コピーのアドレスを渡しても木走査と同じ値になる (`&5` と同じ一時の扱い)。
                Ty::Ref(false, p) if !self.vty(v).is_ref() && Pointee::of(self.vty(v)) == Some(p) => {
                    let ptr = self.alloca(p);
                    self.emit(Ty::Unit, Op::Store { ptr, val: v });
                    self.ref_value(ptr, p, true)
                }
                _ if pt.is_ref() => self.coerce(v, pt),
                // `&T` の値を `&dyn Tr` の引数に渡す: (番地, vtable の番号) の組にする。
                Ty::Agg(_) if super::dyn_lower::is_dynref(self, pt) && matches!(self.vty(v), Ty::Ref(..)) => {
                    super::dyn_lower::coerce_dynref(self, v, pt, 0).unwrap_or(v)
                }
                // `&[T; N]` / `&mut [T; N]` のローカル (ref<fat> / mutref<fat>) を、要素の先頭 ptr に調整する。
                Ty::Ptr => match self.vty(v) {
                    // `Vec` / `String` のヘッダへの参照 (列挙型のペイロードを `match` で束縛した `&String` など): ヘッダのアドレスそのもの
                    // (読んで先頭を取ると、中身のバッファへの ptr を渡してしまう)。
                    Ty::Ref(_, Pointee::Agg(agg_ai)) if super::agg::is_vec_name(&self.agg_def(agg_ai).name) => {
                        self.emit(Ty::Ptr, Op::Bitcast { src: self.vty(v), a: v })
                    }
                    Ty::Ref(_, Pointee::Agg(agg_ai)) => {
                        let fat_val = self.emit(Ty::Agg(agg_ai), Op::Load(v));
                        self.emit(Ty::Ptr, Op::Field { a: fat_val, off: 0 })
                    },
                    _ => v,
                },
                // `&[T; N]` のローカル (ref<fat>) を `&[T]` の fat 値に調整する（参照を外す）。
                Ty::Agg(tgt_ai) => match self.vty(v) {
                    Ty::Ref(_, Pointee::Agg(src_ai)) if tgt_ai == src_ai => self.emit(Ty::Agg(tgt_ai), Op::Load(v)),
                    _ => v,
                },
                _ => v,
            });
        }
        out
    }

    /// アドレス `ptr` (`mutref<p>`) から参照の値を作る。`shared` なら `ref<p>` にする。
    /// 可変の参照も `ptr` そのものは返さず複製する (スロットの番号と区別するため。kir.rs の `Reborrow`)。
    pub(super) fn ref_value(&mut self, ptr: VId, p: Pointee, shared: bool) -> VId {
        if shared {
            self.emit(Ty::Ref(false, p), Op::AsShared(ptr))
        } else {
            self.emit(Ty::Ref(true, p), Op::Reborrow(ptr))
        }
    }

    /// `&e` / `&mut e`。
    pub(super) fn addr_of(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let line = nline(e);
        let shared = st(&f[0]) == "&";
        let operand = &f[1];
        // 所有物を持つ要素の `&v[i]`: `elem_prep` が作った借用 (Vec / String はヘッダのアドレス、構造体などは共有参照)
        if nname(operand) == "Index" {
            if let Some(s) = super::coll::local_slot(self, operand) {
                return Ok(Some(self.read_slot(s)));
            }
        }
        // スカラー (所有物を持たない) 要素の `&v[i]` / `&mut v[i]`: 要素のアドレス
        if nname(operand) == "Index" && !matches!(self.src_ty(e), Ok(Src::Ref(_, ref i, _)) if super::coll::is_hdr_named(i)) {
            return super::coll::index_ref(self, operand, shared, line);
        }
        // 0. Vec / String の借用は、ヘッダのアドレスそのもの (ヘッダは所有者の関数のフレームにある)。
        if let Ok(Src::Ref(_, inner, _)) = self.src_ty(e) {
            if super::coll::is_hdr_named(&inner) {
                let h = super::coll::borrow_header(self, operand, line)?;
                // 借用の開始の注釈 (設計_KIR.md 24 章): 実行では `h` の複製。借用検査が、この値の生きている間を借用の区間と読む。
                if self.cur.is_some() && self.vty(h) == Ty::Ptr {
                    return Ok(Some(self.emit(Ty::Ptr, Op::Borrow { a: h, mutable: !shared })));
                }
                return Ok(Some(h));
            }
        }
        // `&*s` (`s: &mut [T]` / `&str`): スライスの値 (`fat`) は参照そのものなので、そのまま渡す (借用の付け替え)。
        if nname(operand) == "Unary" && st(&nf(operand)[0]) == "*" && self.ty_of(e).is_ok_and(|t| t == self.fat_ty()) {
            return self.expr(&nf(operand)[1]);
        }
        // `&x.buf` / `&mut x.buf` (`buf` は配列の欄): 欄の先頭の番地と長さ N から、求められた形 (スライスの値 `fat` か `&[T; N]` の `ptr`) を作る。
        if matches!(nname(operand), "Path" | "Field") {
            if let Some((super::coll::Seq::Ptr(pp, n), _)) = super::coll::array_field_place(self, operand, line)? {
                let want = self.ty_of(e).ok();
                if want == Some(self.fat_ty()) {
                    let len = self.emit(Ty::Usize, Op::Const(n as i128));
                    let fat = self.fat_ty();
                    return Ok(Some(self.emit(fat, Op::Make { variant: None, fields: vec![pp, len] })));
                }
                if want == Some(Ty::Ptr) {
                    return Ok(Some(pp));
                }
                // `&[T; N]` (配列の値は `fat` なので参照は `ref<fat>`): `fat` を枠の領域に置いてそのアドレスを参照にする。
                if let Some(Ty::Ref(m, Pointee::Agg(fi))) = want {
                    if Ty::Agg(fi) == self.fat_ty() {
                        let len = self.emit(Ty::Usize, Op::Const(n as i128));
                        let fat = self.fat_ty();
                        let v = self.emit(fat, Op::Make { variant: None, fields: vec![pp, len] });
                        let buf = self.emit(Ty::Ptr, Op::FrameBytes { size: 16 });
                        let r = self.emit(Ty::Ref(true, Pointee::Agg(fi)), Op::Bitcast { src: Ty::Ptr, a: buf });
                        self.emit(Ty::Unit, Op::Store { ptr: r, val: v });
                        return Ok(Some(self.ref_value(r, Pointee::Agg(fi), !m)));
                    }
                }
            }
        }
        let Ty::Ref(m, p) = self.ty_of(e)? else {
            return self.err(line, format!("参照の型が参照でない ({} / {:?})", crate::kir::fmt_ty(self.ty_of(e).unwrap_or(Ty::Unit)), self.src_ty(e).ok().map(|t| t.show())));
        };
        let rt = Ty::Ref(m, p);
        // 1. 局所変数: 名前がスタックスロットに置かれていれば、そのアドレス
        if let Some(name) = path1(operand) {
            if let Some(s) = self.lookup(&name) {
                let (ptr, _) = self.slots[s];
                if !self.rf.is_mem(ptr) {
                    return self.err(
                        line,
                        format!("局所 {} のアドレスを取れない (段階 1 の対象外の型)", name),
                    );
                }
                // 借用は move 済みの値から作れない (注釈)。
                self.own_emit(&name, s, crate::kir::OwnKind::Use);
                return Ok(Some(self.ref_value(ptr, p, shared)));
            }
        }
        // 1'. `&x.f.g`: x のスロット (または x の参照) からフィールドのアドレスをたどる
        // `x.f` は `Path` (名前の並び)、`self.f` や入れ子は `Field` の木で来ることがある。どちらも名前の並びにして扱う。
        if matches!(nname(operand), "Path" | "Field") {
            let segs = super::agg::field_segs(operand).unwrap_or_default();
            if segs.len() > 1 {
                if let Some(s) = self.lookup(&segs[0]) {
                    let (base, _) = self.slots[s];
                    let mut cur = match self.vty(base) {
                        Ty::Ref(_, Pointee::Agg(_)) => base,
                        _ if self.rf.is_mem(base) => base,
                        _ => return self.err(line, format!("局所 {} のフィールドのアドレスを取れない (段階 1 の対象外の型)", segs[0])),
                    };
                    for name in &segs[1..] {
                        cur = self.field_addr(cur, name, line)?;
                    }
                    let v = self.coerce(cur, rt);
                    if self.vty(v) != rt {
                        return self.err(line, "参照の型が合わない (共有参照から可変参照は取れない)");
                    }
                    return Ok(Some(v));
                }
            }
        }
        // 1''. `&mut (*r.as_mut()).child` / `&(*f(x)).a.b`: 参照を返す式の先のフィールドのアドレス (名前でない根の `*`)。
        if nname(operand) == "Field" {
            if let Some(cur) = self.place_via_deref(operand, line)? {
                let Some(cur) = cur else { return Ok(None) };
                let v = self.coerce(cur, rt);
                if self.vty(v) != rt {
                    return self.err(line, "参照の型が合わない (共有参照から可変参照は取れない)");
                }
                return Ok(Some(v));
            }
        }
        // 2. `&*r` / `&mut *r`: 参照を付け替えるだけ
        if nname(operand) == "Unary" && st(&nf(operand)[0]) == "*" {
            let Some(r) = self.expr(&nf(operand)[1])? else {
                return Ok(None);
            };
            let v = self.coerce(r, rt);
            if self.vty(v) != rt {
                return self.err(line, "参照の型が合わない");
            }
            // `&mut *r` (可変の参照の再借用): 値は同じ複製だが、借用の開始を KIR に残す (設計_KIR.md 24.10)。
            // 同じ値を返すと、`r` と `&mut *r` が 1 つの借用になり、両方が生きている間の競合を検査できない。
            if let Ty::Ref(true, _) = rt {
                return Ok(Some(self.emit(rt, Op::Reborrow(v))));
            }
            return Ok(Some(v));
        }
        // 3. 式のままの値: 一時のスロットに入れて、そのアドレスを取る
        let Some(v) = self.expr(operand)? else { return Ok(None) };
        if let Ok(t) = self.src_ty(operand) {
            self.own_temp(&t);
        }
        if Pointee::of(self.vty(v)) != Some(p) {
            return self.err(line, "参照の指す先の型が合わない");
        }
        // 定数のスカラーへの共有参照 (`&10u32`、仕様の静的な昇格) は、静的な領域に置く。関数の枠 (`alloca`) に置くと、
        // `let r: &'static u32 = &10u32` を返した後にぶら下がる。
        if shared {
            if let Some(Inst { op: Op::Const(c), ty, .. }) = self.cur.and_then(|b| self.blocks[b].insts.last()).filter(|i| i.dst == v) {
                let w = match ty {
                    Ty::I8 | Ty::U8 | Ty::Bool => 1,
                    Ty::I16 | Ty::U16 => 2,
                    Ty::I32 | Ty::U32 | Ty::F32 => 4,
                    Ty::I64 | Ty::U64 | Ty::F64 => 8,
                    Ty::I128 | Ty::U128 => 16,
                    _ => 0,
                };
                if w > 0 {
                    let bytes = c.to_le_bytes()[..w].to_vec();
                    let sp = self.emit(Ty::Ptr, Op::StaticBytes(bytes));
                    let mr = self.emit(Ty::Ref(true, p), Op::Bitcast { src: Ty::Ptr, a: sp });
                    return Ok(Some(self.ref_value(mr, p, true)));
                }
            }
        }
        let ptr = self.alloca(p);
        self.emit(Ty::Unit, Op::Store { ptr, val: v });
        super::coll::hold_ref_temp(self, ptr, self.vty(v));
        Ok(Some(self.ref_value(ptr, p, shared)))
    }

    /// `(*X).f.g` の形 (`X` は名前でなく、集成体への参照を返す式) の場所のアドレス。この形でなければ外側の `None`。
    fn place_via_deref(&mut self, v: &V, line: usize) -> LR<Option<Option<VId>>> {
        match nname(v) {
            "Field" => {
                let f = nf(v);
                match self.place_via_deref(&f[0], line)? {
                    None => Ok(None),
                    Some(None) => Ok(Some(None)),
                    Some(Some(base)) => Ok(Some(Some(self.field_addr(base, &st(&f[1]), line)?))),
                }
            }
            "Unary" if st(&nf(v)[0]) == "*" && !matches!(nname(&nf(v)[1]), "Path" | "Field") => {
                if !matches!(self.ty_of(&nf(v)[1]), Ok(Ty::Ref(_, Pointee::Agg(_)))) {
                    return Ok(None);
                }
                Ok(Some(self.expr(&nf(v)[1])?))
            }
            _ => Ok(None),
        }
    }

    /// `*r` を読む。
    pub(super) fn deref(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let line = nline(e);
        let Some(r) = self.expr(&f[1])? else { return Ok(None) };
        // 結果の型は、指す先の型で決まる (型表は `capture [&x]` で取り込んだ名前の `*x` を未確定にするため、型表は見ない)。
        match self.vty(r) {
            Ty::Ref(_, p) => {
                let t = p.ty();
                // 所有物 (Vec / String / それを持つ型) の借用の中身を丸ごと読むと、借用元と同じバッファを指す「所有値」ができて二重解放になる。
                // `ksn check` はこの形を通し、木走査はバッファを共有するだけなので、KIR は理由を付けて拒む (設計_KIR.md 17.5)。
                if super::coll::owns(self, t) {
                    return self.err(line, "所有物を持つ型の借用の中身 (*r) を丸ごと読むのは段階 1 の対象外 (借用元と同じ領域を指す所有値ができる)");
                }
                Ok(Some(self.emit(t, Op::Load(r))))
            }
            // `Some(b)` で `Option<&スカラー>` から取った束縛は値で持つ (設計_KIR.md 10.10)。型表が `&T` で、
            // T の型が値の型と一致するなら、`*b` は値そのもの (恒等)。
            // `for x in &v` のスカラーの要素の変数 `x` は値で持つので、`*x` は値そのもの
            _ if super::coll::local_slot(self, &f[1]).is_some_and(|s| self.coll.borrowed.contains(&s)) => Ok(Some(r)),
            t => match self.src_ty(&f[1]) {
                Ok(Src::Ref(_, inner, _)) if super::conv(&inner) == Some(t) => Ok(Some(r)),
                // `match v.last() { Some(t) => *t }`: 所有物を持たない集成体 (`Copy` の構造体) の `Option<&T>` の束縛は値そのもの (複製)。
                Ok(Src::Ref(_, inner, _)) if matches!(t, Ty::Agg(_)) && !super::coll::owns(self, t) && self.conv_t(&inner) == Some(t) => Ok(Some(r)),
                // 参照越しの分解 (`match &x { Some(p) => *p }`) で得た名前は、型表では段数が決まらない (検査は済んでいる)。
                Ok(Src::Prim(s)) if Ty::parse(&s) == Some(t) => Ok(Some(r)),
                _ => self.err(line, "* の被演算子が参照でない"),
            },
        }
    }

    /// `*r = v`。値を先に評価し、次に参照を評価する (木走査インタプリタの順序と同じ)。
    pub(super) fn store_deref(&mut self, target: &V, rhs: &V, line: usize) -> LR<St> {
        let tf = nf(target);
        if st(&tf[0]) != "*" {
            return self.err(line, "代入先は名前か *参照 だけ段階 1 の対象");
        }
        let Some(v) = self.expr(rhs)? else { return Ok(St::Div) };
        let Some(r) = self.expr(&tf[1])? else {
            return Ok(St::Div);
        };
        match self.vty(r) {
            Ty::Ref(true, p) if p.ty() == self.vty(v) => {
                self.emit(Ty::Unit, Op::Store { ptr: r, val: v });
                Ok(St::Unit)
            }
            Ty::Ref(false, _) => self.err(line, "共有参照 &T を通した代入は段階 1 の対象外"),
            _ => self.err(line, "代入先の参照の型が合わない"),
        }
    }
}
