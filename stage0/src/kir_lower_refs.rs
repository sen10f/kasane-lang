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
use crate::check::{nf, nline, nname, st, strs};
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
            if *n == "Closure" && fs.len() == 4 {
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
        Src::Prim(s) => Ty::parse(s).and_then(Pointee::of).map(|p| Ty::Ref(m, p)),
        _ => None,
    }
}

impl<'a> Lw<'a> {
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

    /// 引数の値を、呼び先の引数の型に合わせる (参照の場合だけ)。
    pub(super) fn coerce_args(&mut self, vs: Vec<VId>, pts: &[Ty]) -> Vec<VId> {
        let mut out = Vec::with_capacity(vs.len());
        for (v, pt) in vs.into_iter().zip(pts.iter().copied()) {
            out.push(if pt.is_ref() { self.coerce(v, pt) } else { v });
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
        // 0. Vec / String の借用は、ヘッダのアドレスそのもの (ヘッダは所有者の関数のフレームにある)。
        if let Ok(Src::Ref(_, inner)) = self.src_ty(e) {
            if super::coll::is_hdr_named(&inner) {
                return super::coll::borrow_header(self, operand, line).map(Some);
            }
        }
        let Ty::Ref(m, p) = self.ty_of(e)? else {
            return self.err(line, "参照の型が参照でない");
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
                return Ok(Some(self.ref_value(ptr, p, shared)));
            }
        }
        // 1'. `&x.f.g`: x のスロット (または x の参照) からフィールドのアドレスをたどる
        if nname(operand) == "Path" {
            let segs = strs(&nf(operand)[0]);
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
        // 2. `&*r` / `&mut *r`: 参照を付け替えるだけ
        if nname(operand) == "Unary" && st(&nf(operand)[0]) == "*" {
            let Some(r) = self.expr(&nf(operand)[1])? else {
                return Ok(None);
            };
            let v = self.coerce(r, rt);
            if self.vty(v) != rt {
                return self.err(line, "参照の型が合わない");
            }
            return Ok(Some(v));
        }
        // 3. 式のままの値: 一時のスロットに入れて、そのアドレスを取る
        let Some(v) = self.expr(operand)? else { return Ok(None) };
        if Pointee::of(self.vty(v)) != Some(p) {
            return self.err(line, "参照の指す先の型が合わない");
        }
        let ptr = self.alloca(p);
        self.emit(Ty::Unit, Op::Store { ptr, val: v });
        Ok(Some(self.ref_value(ptr, p, shared)))
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
                Ok(Some(self.emit(t, Op::Load(r))))
            }
            // `get` / `for (k, v) in m.iter()` で束縛した `&スカラー` は値で持つ (設計_KIR.md 10.10 / 16 章)。`*` は値そのもの。
            t if t.is_scalar() => Ok(Some(r)),
            _ => self.err(line, "* の被演算子が参照でない"),
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
