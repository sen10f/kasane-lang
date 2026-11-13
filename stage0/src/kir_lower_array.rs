//! 配列リテラル `[e1, e2, ...]` の下ろし (段階 1)。`kir_lower` の子モジュール。
//!
//! KIR に配列の型はない。配列の値は、要素を並べたフレームの領域と長さの `{ptr, len}` (スライスと同じ値の形) で持つ。
//! 要素は整数・浮動小数・bool・ptr (バイト記憶で読み書きできる型) だけ。扱う形:
//! - `for x in [..]` (`coll::for_expr`)、`let a = [..]` と `for x in a` (`a` はスライスの局所として扱う)。
//! `[e; N]` (`ArrayRep`) は、長さ N が整数リテラルで、要素がスカラーのときだけ下ろす。N 回の書き込みは
//! 小さなループ (ブロック 2 つ) にする (N を展開しない)。
//! 配列の引数・戻り値・フィールドは対象外 (KIR に型がないので、理由付きで未対応にする)。
//! フレームの領域は式の評価ごとに確保する (ループの中で評価すると、領域はループの回数だけ増える)。

use crate::check::{items, nf, nlineno as nline};
use crate::kir::{size_of, Aop, Cc, Mode, Op, Ty, VId};
use crate::parser::V;

use super::{Lw, LR};

impl<'a> Lw<'a> {
    /// `[e1, e2, ...]` の値 (`{ptr, len}`) と要素の型。対象外なら理由付きで止める。
    pub(super) fn array_lit(&mut self, e: &V) -> LR<Option<(VId, Ty)>> {
        let line = nline(e);
        let elems = items(&nf(e)[0]).to_vec();
        if elems.is_empty() {
            return self.err(line, "空の配列リテラルは段階 1 の対象外");
        }
        let mut vs = vec![];
        for x in &elems {
            match self.expr(x)? {
                Some(v) => vs.push(v),
                None => return Ok(None),
            }
        }
        let et = self.vty(vs[0]);
        if !(et.is_int() || et.is_float() || matches!(et, Ty::Bool | Ty::Ptr)) {
            return self.err(line, "配列の要素の型は段階 1 の対象外 (要素は整数・浮動小数・bool・ptr だけ)");
        }
        if vs.iter().any(|v| self.vty(*v) != et) {
            return self.err(line, "配列の要素の型がそろっていない");
        }
        let size = size_of(&self.aggs.borrow().defs, et);
        let base = self.emit(Ty::Ptr, Op::FrameBytes { size: size * vs.len() as u32 });
        for (i, v) in vs.iter().enumerate() {
            self.emit(Ty::Unit, Op::MemStore { ty: et, p: base, off: size * i as u32, v: *v });
        }
        let n = self.emit(Ty::Usize, Op::Const(vs.len() as i128));
        let fat = self.fat_ty();
        Ok(Some((self.emit(fat, Op::Make { variant: None, fields: vec![base, n] }), et)))
    }

    /// `[e; N]` の値 (`{ptr, len}`)。N は整数リテラル、要素 `e` はスカラー (整数・浮動小数・bool・ptr)。
    /// 要素を 1 つ評価し、N 回の書き込みを loop で行う (ブロック `header(i)` → `body` → `header(i+1)`、終わりは `exit`)。
    pub(super) fn array_rep(&mut self, e: &V) -> LR<Option<VId>> {
        let line = nline(e);
        let f = nf(e);
        let Some(n) = (match &f[1] {
            crate::parser::V::N(s) => s.replace('_', "").parse::<u64>().ok(),
            _ => None,
        }) else {
            return self.err(line, "配列の長さはリテラルだけ段階 1 の対象");
        };
        let Some(x) = self.expr(&f[0])? else { return Ok(None) };
        let et = self.vty(x);
        if !(et.is_int() || et.is_float() || matches!(et, Ty::Bool | Ty::Ptr)) {
            return self.err(line, "配列の要素の型は段階 1 の対象外 (要素は整数・浮動小数・bool・ptr だけ)");
        }
        let size = size_of(&self.aggs.borrow().defs, et);
        // 空の配列 (`[x; 0]`) も領域は 1 バイト確保する (`frame.bytes` は 1 以上のはず。長さ 0 なので読まれない)。
        let base = self.emit(Ty::Ptr, Op::FrameBytes { size: (size * n as u32).max(1) });
        let zero = self.emit(Ty::Usize, Op::Const(0));
        let (hb, hp) = self.new_block(&[Ty::Usize]);
        self.goto(hb, vec![zero]);

        self.cur = Some(hb);
        let limit = self.emit(Ty::Usize, Op::Const(n as i128));
        let (bb, _) = self.new_block(&[]);
        let (xb, _) = self.new_block(&[]);
        let cond = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: hp[0], b: limit });
        self.branch(cond, (bb, vec![]), (xb, vec![]));

        self.cur = Some(bb);
        let off = if size == 1 {
            hp[0]
        } else {
            let k = self.emit(Ty::Usize, Op::Const(size as i128));
            self.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: hp[0], b: k })
        };
        let p = self.emit(Ty::Ptr, Op::PtrAdd { p: base, off });
        self.emit(Ty::Unit, Op::MemStore { ty: et, p, off: 0, v: x });
        let one = self.emit(Ty::Usize, Op::Const(1));
        let next = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: hp[0], b: one });
        self.goto(hb, vec![next]);

        self.cur = Some(xb);
        let len = self.emit(Ty::Usize, Op::Const(n as i128));
        let fat = self.fat_ty();
        Ok(Some(self.emit(fat, Op::Make { variant: None, fields: vec![base, len] })))
    }
}
