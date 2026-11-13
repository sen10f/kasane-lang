//! 配列リテラル `[e1, e2, ...]` の下ろし (段階 1)。`kir_lower` の子モジュール。
//!
//! KIR に配列の型はない。配列の値は、要素を並べたフレームの領域と長さの `{ptr, len}` (スライスと同じ値の形) で持つ。
//! 要素は整数・浮動小数・bool・ptr (バイト記憶で読み書きできる型) だけ。扱う形:
//! - `for x in [..]` (`coll::for_expr`)、`let a = [..]` と `for x in a` (`a` はスライスの局所として扱う)。
//! 配列の引数・戻り値・フィールドは対象外 (KIR に型がないので、理由付きで未対応にする)。
//! フレームの領域は式の評価ごとに確保する (ループの中で評価すると、領域はループの回数だけ増える)。

use crate::check::{items, nf, nlineno as nline};
use crate::kir::{size_of, Op, Ty, VId};
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
}
