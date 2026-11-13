//! 文字列・バイト列の操作の下ろし (設計_KIR.md 10.9・10.10)。
//!
//! - `&str` の `starts_with` / `ends_with` / `eq`: バイト列のループを KIR 命令で書く。
//! - バイト列リテラル `b"..."`: 静的なバイト列 (`static.bytes`) と長さから `fat` (`{ptr, len}`) を作る。
//! - `alloc.string.String.from_str(alloc, s)`: `s` のコピーをヒープに確保して `Ok(String)` を返す。
//! - `core.str.from_utf8(bytes)`: Rust の `core::str::from_utf8` と同じ検証を KIR 命令で書き、
//!   `Result<&str, Utf8Error>` を返す。`valid_up_to` は最後に完全だった文字の終わり。
//!
//! `kir_lower.rs` の子モジュールとして `Lw` の非公開の項目を使う。

use crate::check::{items, nf, nlineno as nline};
use crate::kir::{Aop, Cc, Mode, Op, Pay, Ty, VId};
use crate::parser::V;
use crate::types::Ty as Src;

use super::{coll, parse_int, Lw, LR};

impl<'a> Lw<'a> {
    /// `e.m(args)` のうち、`&str` / `&[u8]` のメソッド (`starts_with` / `ends_with` / `eq`)。
    /// 対象外なら `None` (親の処理に任せる)。
    pub(super) fn str_method_call(&mut self, e: &V, kind: &coll::Kind, h: VId, m: &str, args: &[V]) -> Option<LR<Option<VId>>> {
        if !matches!(kind, coll::Kind::Str | coll::Kind::Slice(_)) {
            return None;
        }
        let line = nline(e);
        match m {
            "starts_with" | "ends_with" => {
                if args.len() != 1 {
                    return Some(self.err(line, format!("メソッド {} の引数は 1 個のはず", m)));
                }
                let Some(pat) = self.expr(&args[0]).ok()? else { return Some(Ok(None)) };
                if self.vty(pat) != self.vty(h) {
                    return Some(self.err(line, format!("メソッド {} の引数の型が受け手と違う", m)));
                }
                let t = self.ty_of(e).ok()?;
                if !matches!(t, Ty::Bool) {
                    return Some(self.err(line, format!("メソッド {} の結果の型が bool ではない", m)));
                }
                Some(match self.str_starts_or_ends_with(h, pat, m == "starts_with") {
                    Ok(v) => Ok(Some(v)),
                    Err(e) => Err(e),
                })
            }
            "eq" => {
                if args.len() != 1 {
                    return Some(self.err(line, "メソッド eq の引数は 1 個のはず"));
                }
                let Some(o) = self.expr(&args[0]).ok()? else { return Some(Ok(None)) };
                if self.vty(o) != self.vty(h) {
                    return Some(self.err(line, "メソッド eq の引数の型が受け手と違う"));
                }
                let t = self.ty_of(e).ok()?;
                if !matches!(t, Ty::Bool) {
                    return Some(self.err(line, "メソッド eq の結果の型が bool ではない"));
                }
                Some(match self.str_eq(h, o) {
                    Ok(v) => Ok(Some(v)),
                    Err(e) => Err(e),
                })
            }
            _ => None,
        }
    }

    /// `self.starts_with(pat)` / `self.ends_with(pat)`。`self` と `pat` は `fat` (`{ptr, len}`)。
    fn str_starts_or_ends_with(&mut self, s: VId, pat: VId, is_start: bool) -> LR<VId> {
        let sp = self.emit(Ty::Ptr, Op::Field { a: s, off: 0 });
        let sl = self.emit(Ty::Usize, Op::Field { a: s, off: 8 });
        let pp = self.emit(Ty::Ptr, Op::Field { a: pat, off: 0 });
        let pl = self.emit(Ty::Usize, Op::Field { a: pat, off: 8 });
        let zero = self.emit(Ty::Usize, Op::Const(0));
        let one = self.emit(Ty::Usize, Op::Const(1));
        // 空のパターンは常に真。
        let pat_empty = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: pl, b: zero });
        // len(self) < len(pat) なら偽。
        let too_short = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: sl, b: pl });
        let start = if is_start {
            zero
        } else {
            self.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: sl, b: pl })
        };
        let sp2 = self.emit(Ty::Ptr, Op::PtrAdd { p: sp, off: start });

        let (chk, _) = self.new_block(&[]);
        let (hd, hp) = self.new_block(&[Ty::Usize]);
        let (body, _) = self.new_block(&[]);
        let (okb, _) = self.new_block(&[]);
        let (bad, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[Ty::Bool]);
        let t = self.emit(Ty::Bool, Op::Const(1));
        let f = self.emit(Ty::Bool, Op::Const(0));
        self.branch(pat_empty, (okb, vec![]), (chk, vec![]));
        self.cur = Some(chk);
        self.branch(too_short, (bad, vec![]), (hd, vec![zero]));
        self.cur = Some(hd);
        let i = hp[0];
        let done = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::Usize, a: i, b: pl });
        self.branch(done, (okb, vec![]), (body, vec![]));
        self.cur = Some(body);
        let ba = self.emit(Ty::Ptr, Op::PtrAdd { p: sp2, off: i });
        let bb = self.emit(Ty::Ptr, Op::PtrAdd { p: pp, off: i });
        let ca = self.emit(Ty::U8, Op::MemLoad { p: ba, off: 0 });
        let cb = self.emit(Ty::U8, Op::MemLoad { p: bb, off: 0 });
        let eq = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U8, a: ca, b: cb });
        let ni = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: i, b: one });
        self.branch(eq, (hd, vec![ni]), (bad, vec![]));
        self.cur = Some(okb);
        self.goto(join, vec![t]);
        self.cur = Some(bad);
        self.goto(join, vec![f]);
        self.cur = Some(join);
        Ok(jp[0])
    }

    /// `self == other` (`&str` のバイト列の等しさ)。
    fn str_eq(&mut self, a: VId, b: VId) -> LR<VId> {
        let pa = self.emit(Ty::Ptr, Op::Field { a, off: 0 });
        let la = self.emit(Ty::Usize, Op::Field { a, off: 8 });
        let pb = self.emit(Ty::Ptr, Op::Field { a: b, off: 0 });
        let lb = self.emit(Ty::Usize, Op::Field { a: b, off: 8 });
        let zero = self.emit(Ty::Usize, Op::Const(0));
        let one = self.emit(Ty::Usize, Op::Const(1));
        let len_eq = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: la, b: lb });

        let (hd, hp) = self.new_block(&[Ty::Usize]);
        let (body, _) = self.new_block(&[]);
        let (okb, _) = self.new_block(&[]);
        let (bad, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[Ty::Bool]);
        let t = self.emit(Ty::Bool, Op::Const(1));
        let f = self.emit(Ty::Bool, Op::Const(0));
        self.branch(len_eq, (hd, vec![zero]), (bad, vec![]));
        self.cur = Some(hd);
        let i = hp[0];
        let done = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::Usize, a: i, b: la });
        self.branch(done, (okb, vec![]), (body, vec![]));
        self.cur = Some(body);
        let ba = self.emit(Ty::Ptr, Op::PtrAdd { p: pa, off: i });
        let bb = self.emit(Ty::Ptr, Op::PtrAdd { p: pb, off: i });
        let ca = self.emit(Ty::U8, Op::MemLoad { p: ba, off: 0 });
        let cb = self.emit(Ty::U8, Op::MemLoad { p: bb, off: 0 });
        let eq = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U8, a: ca, b: cb });
        let ni = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: i, b: one });
        self.branch(eq, (hd, vec![ni]), (bad, vec![]));
        self.cur = Some(okb);
        self.goto(join, vec![t]);
        self.cur = Some(bad);
        self.goto(join, vec![f]);
        self.cur = Some(join);
        Ok(jp[0])
    }

    /// バイト列リテラル `b"..."` (`&[u8]` の `fat` 値)。中身は整数リテラルのリスト。
    pub(super) fn str_lit(&mut self, e: &V) -> LR<VId> {
        let f = nf(e);
        let line = nline(e);
        let elems = items(&f[1]);
        let mut bytes: Vec<u8> = vec![];
        for x in elems {
            let v = parse_int(x).ok_or_else(|| format!("行 {}: バイト列リテラルの要素が扱えない", line))?;
            if v < 0 || v > 255 {
                return self.err(line, format!("バイト列リテラルの要素 {} が u8 の範囲にない", v));
            }
            bytes.push(v as u8);
        }
        let data = self.emit(Ty::Ptr, Op::StaticBytes(bytes.clone()));
        let n = self.emit(Ty::Usize, Op::Const(bytes.len() as i128));
        let fat = self.fat_ty();
        Ok(self.emit(fat, Op::Make { variant: None, fields: vec![data, n] }))
    }

    /// `alloc.string.String.from_str(allocator, text)`: `text` のコピーをヒープに確保して `Ok(String)` を返す。
    /// 確保は失敗しないので常に `Ok`。`String` は `vec` ヘッダ集成体 (`{ptr, len, cap}`)。
    pub(super) fn from_str_call(&mut self, _e: &V, args: &[V], line: usize) -> LR<Option<VId>> {
        if args.len() != 2 {
            return self.err(line, "alloc.string.String.from_str の引数は 2 個のはず");
        }
        let Some(text) = self.expr(&args[1])? else { return Ok(None) };
        let fat = self.vty(text);
        let Ty::Agg(fat_idx) = fat else {
            return self.err(line, "alloc.string.String.from_str の第 2 引数が &str ではない");
        };
        let _ = fat_idx;
        let ptr = self.emit(Ty::Ptr, Op::Field { a: text, off: 0 });
        let len = self.emit(Ty::Usize, Op::Field { a: text, off: 8 });
        let buf = self.emit(Ty::Ptr, Op::HeapAlloc(len));
        // バイト列をコピーするループ。
        let zero = self.emit(Ty::Usize, Op::Const(0));
        let one = self.emit(Ty::Usize, Op::Const(1));
        let (hd, hp) = self.new_block(&[Ty::Usize]);
        let (body, _) = self.new_block(&[]);
        let (done, _) = self.new_block(&[]);
        self.goto(hd, vec![zero]);
        self.cur = Some(hd);
        let i = hp[0];
        let done_cmp = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::Usize, a: i, b: len });
        self.branch(done_cmp, (done, vec![]), (body, vec![]));
        self.cur = Some(body);
        let bp = self.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: i });
        let bc = self.emit(Ty::U8, Op::MemLoad { p: bp, off: 0 });
        let dp = self.emit(Ty::Ptr, Op::PtrAdd { p: buf, off: i });
        self.emit(Ty::Unit, Op::MemStore { ty: Ty::U8, p: dp, off: 0, v: bc });
        let ni = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: i, b: one });
        self.goto(hd, vec![ni]);
        self.cur = Some(done);
        let vt = self.aggs.borrow_mut().vec_hdr();
        let vec_idx = match vt { Ty::Agg(i) => i, _ => return self.err(line, "vec の型が集成体でない") };
        let et = self.aggs.borrow_mut().alloc_err();
        let err_idx = match et { Ty::Agg(i) => i, _ => return self.err(line, "AllocError の型が集成体でない") };
        let null = self.emit(Ty::Ptr, Op::Const(0));
        let cap = len;
        let str_val = self.emit(vt, Op::Make { variant: None, fields: vec![buf, len, cap] });
        let vec_pay = Pay::of(vt).expect("vec は集成体");
        let err_pay = Pay::of(et).expect("AllocError は集成体");
        let rt = Ty::Res(vec_pay, err_pay);
        let ok = self.emit(rt, Op::Ok(str_val));
        let _ = (null, vec_idx, err_idx);
        Ok(Some(ok))
    }

    /// `core.str.from_utf8(bytes)`: Rust の `core::str::from_utf8` と同じ検証を KIR 命令で書き、
    /// `Result<&str, Utf8Error>` を返す。`valid_up_to` は最後に完全だった文字の終わり。
    pub(super) fn from_utf8_call(&mut self, _e: &V, args: &[V], line: usize) -> LR<Option<VId>> {
        if args.len() != 1 {
            return self.err(line, "core.str.from_utf8 の引数は 1 個のはず");
        }
        let Some(bytes) = self.expr(&args[0])? else { return Ok(None) };
        let fat = self.vty(bytes);
        let Ty::Agg(fat_idx) = fat else {
            return self.err(line, "core.str.from_utf8 の引数が &[u8] ではない");
        };
        let utf8_err = self.aggs.borrow_mut().lower_ty(self.p, &self.def.file, &Src::Named("core.str.Utf8Error".into(), vec![]))?;
        let Ty::Agg(err_idx) = utf8_err else {
            return self.err(line, "Utf8Error の型が集成体でない");
        };
        // 検証は命令 `utf8.valid` (正しい接頭辞の長さ) に任せる。参照実行器は Rust の `str::from_utf8`、
        // 機械語は `kir_rt.c` の `ksn_utf8_valid` (設計_KIR.md 17.12)。長さが全体と同じなら Ok。
        let ptr = self.emit(Ty::Ptr, Op::Field { a: bytes, off: 0 });
        let len = self.emit(Ty::Usize, Op::Field { a: bytes, off: 8 });
        let vu = self.emit(Ty::Usize, Op::Utf8Valid { p: ptr, len });
        let all = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: vu, b: len });
        let rt = Ty::Res(Pay::Agg(fat_idx), Pay::Agg(err_idx));
        let (okb, _) = self.new_block(&[]);
        let (badb, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[rt]);
        self.branch(all, (okb, vec![]), (badb, vec![]));
        self.cur = Some(okb);
        let ok = self.emit(rt, Op::Ok(bytes));
        self.goto(join, vec![ok]);
        self.cur = Some(badb);
        let err_val = self.emit(utf8_err, Op::Make { variant: None, fields: vec![vu] });
        let err = self.emit(rt, Op::Err(err_val));
        self.goto(join, vec![err]);
        self.cur = Some(join);
        Ok(Some(jp[0]))
    }
}


