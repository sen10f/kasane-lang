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
use crate::kir::{Aop, BoolOp, Cc, Mode, Op, Pay, Term, Ty, VId};
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
        self.blocks[join].term = Some(Term::Ret(jp[0]));
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
        self.blocks[join].term = Some(Term::Ret(jp[0]));
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
        let ptr = self.emit(Ty::Ptr, Op::Field { a: bytes, off: 0 });
        let len = self.emit(Ty::Usize, Op::Field { a: bytes, off: 8 });
        let zero = self.emit(Ty::Usize, Op::Const(0));
        let one = self.emit(Ty::Usize, Op::Const(1));
        let b80 = self.emit(Ty::U8, Op::Const(0x80));
        let c2 = self.emit(Ty::U8, Op::Const(0xc2));
        let df = self.emit(Ty::U8, Op::Const(0xdf));
        let e0 = self.emit(Ty::U8, Op::Const(0xe0));
        let ef = self.emit(Ty::U8, Op::Const(0xef));
        let f0 = self.emit(Ty::U8, Op::Const(0xf0));
        let f4 = self.emit(Ty::U8, Op::Const(0xf4));
        let b_a0 = self.emit(Ty::U8, Op::Const(0xa0));
        let b_9f = self.emit(Ty::U8, Op::Const(0x9f));
        let b_bf = self.emit(Ty::U8, Op::Const(0xbf));
        let b_90 = self.emit(Ty::U8, Op::Const(0x90));
        let b_8f = self.emit(Ty::U8, Op::Const(0x8f));

        let (hd, hp) = self.new_block(&[Ty::Usize]);
        let (body, bodyp_) = self.new_block(&[Ty::Usize]);
        let (check2, check2p) = self.new_block(&[Ty::Usize, Ty::U8, Ty::Bool, Ty::Bool, Ty::Bool]);
        let (check3, check3p) = self.new_block(&[Ty::Usize, Ty::U8, Ty::Bool, Ty::Bool]);
        let (check4, check4p) = self.new_block(&[Ty::Usize, Ty::U8, Ty::Bool]);
        let (two, tp_) = self.new_block(&[Ty::Usize]);
        let (two2, two2p) = self.new_block(&[Ty::Usize, Ty::Usize]);
        let (three, thp_) = self.new_block(&[Ty::Usize]);
        let (three2, three2p) = self.new_block(&[Ty::Usize, Ty::Usize]);
        let (three3, three3p) = self.new_block(&[Ty::Usize, Ty::Usize, Ty::Usize, Ty::U8, Ty::U8]);
        let (four3, four3p) = self.new_block(&[Ty::Usize, Ty::Usize, Ty::Usize, Ty::U8, Ty::U8]);
        let (three4, three4p) = self.new_block(&[Ty::Usize, Ty::Usize]);
        let (four, fp_) = self.new_block(&[Ty::Usize]);
        let (four2, four2p) = self.new_block(&[Ty::Usize, Ty::Usize]);

        let (four4, four4p) = self.new_block(&[Ty::Usize, Ty::Usize, Ty::Usize]);
        let (okb, _) = self.new_block(&[]);
        let (badb, bp_) = self.new_block(&[Ty::Usize]);
        let (join, jp) = self.new_block(&[Ty::Res(Pay::Agg(fat_idx), Pay::Agg(err_idx))]);

        self.goto(hd, vec![zero]);
        self.cur = Some(hd);
        let i = hp[0];
        let done = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::Usize, a: i, b: len });
        self.branch(done, (okb, vec![]), (body, vec![i]));

        // ---------- body: 先頭バイト b の分岐 ----------
        self.cur = Some(body);
        let bi = bodyp_[0];
        let bp = self.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: bi });
        let b = self.emit(Ty::U8, Op::MemLoad { p: bp, off: 0 });
        let lt80 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::U8, a: b, b: b80 });
        let ge_c2 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::U8, a: b, b: c2 });
        let le_df = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::U8, a: b, b: df });
        let is_2 = self.emit(Ty::Bool, Op::Bool(BoolOp::And, ge_c2, le_df));
        let ge_e0 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::U8, a: b, b: e0 });
        let le_ef = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::U8, a: b, b: ef });
        let is_3 = self.emit(Ty::Bool, Op::Bool(BoolOp::And, ge_e0, le_ef));
        let ge_f0 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::U8, a: b, b: f0 });
        let le_f4 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::U8, a: b, b: f4 });
        let is_4 = self.emit(Ty::Bool, Op::Bool(BoolOp::And, ge_f0, le_f4));
        let ni = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: bi, b: one });
        self.branch(lt80, (hd, vec![ni]), (check2, vec![bi, b, is_2, is_3, is_4]));
        let _ = (is_2, is_3, is_4, ge_c2, le_df, ge_e0, le_ef, ge_f0, le_f4);

        // ---------- 先頭バイトが 0x80.. の分岐 ----------
        self.cur = Some(check2);
        let ci = check2p[0];
        let b = check2p[1];
        let is2 = check2p[2];
        let is3 = check2p[3];
        let is4 = check2p[4];
        self.branch(is2, (two, vec![ci]), (check3, vec![ci, b, is3, is4]));
        self.cur = Some(check3);
        let ci2 = check3p[0];
        let b = check3p[1];
        let is3b = check3p[2];
        let is4b = check3p[3];
        self.branch(is3b, (three, vec![ci2]), (check4, vec![ci2, b, is4b]));
        self.cur = Some(check4);
        let ci3 = check4p[0];
        let b = check4p[1];
        let is4b = check4p[2];
        self.branch(is4b, (four, vec![ci3]), (badb, vec![ci3]));

        // ---------- 2 バイト (C2..DF): 2 バイト目 80..BF ----------
        self.cur = Some(two);
        let ti = tp_[0];
        let i1 = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: ti, b: one });
        let c1 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::Usize, a: i1, b: len });
        self.branch(c1, (badb, vec![ti]), (two2, vec![ti, i1]));
        self.cur = Some(two2);
        let t0 = two2p[0];
        let t1 = two2p[1];
        let bp2 = self.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: t1 });
        let b2 = self.emit(Ty::U8, Op::MemLoad { p: bp2, off: 0 });
        let ge80 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::U8, a: b2, b: b80 });
        let lebf = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::U8, a: b2, b: b_bf });
        let is_cont = self.emit(Ty::Bool, Op::Bool(BoolOp::And, ge80, lebf));
        let i2 = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: t1, b: one });
        self.branch(is_cont, (hd, vec![i2]), (badb, vec![t0]));

        // ---------- 3 バイト (E0..EF) ----------
        self.cur = Some(three);
        let th = thp_[0];
        let i1 = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: th, b: one });
        let c1 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::Usize, a: i1, b: len });
        self.branch(c1, (badb, vec![th]), (three2, vec![th, i1]));
        self.cur = Some(three2);
        let t0 = three2p[0];
        let t1 = three2p[1];
        let bp2 = self.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: t0 });
        let b3 = self.emit(Ty::U8, Op::MemLoad { p: bp2, off: 0 });
        let bp2 = self.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: t1 });
        let b2 = self.emit(Ty::U8, Op::MemLoad { p: bp2, off: 0 });
        let ge_a0 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::U8, a: b2, b: b_a0 });
        let le_bf = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::U8, a: b2, b: b_bf });
        let ge80 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::U8, a: b2, b: b80 });
        let le_9f = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::U8, a: b2, b: b_9f });
        let _ = b2;
        let ok_e0 = self.emit(Ty::Bool, Op::Bool(BoolOp::And, ge_a0, le_bf));
        let ok_ed = self.emit(Ty::Bool, Op::Bool(BoolOp::And, ge80, le_9f));
        let ok_other = self.emit(Ty::Bool, Op::Bool(BoolOp::And, ge80, lebf));
        let eq_e0 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U8, a: b3, b: e0 });
        let eq_ed = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U8, a: b3, b: f0 });
        let special = self.emit(Ty::Bool, Op::Bool(BoolOp::Or, eq_e0, eq_ed));
        let special_or = self.emit(Ty::Bool, Op::Bool(BoolOp::Or, ok_e0, ok_ed));
        let special_ok = self.emit(Ty::Bool, Op::Bool(BoolOp::And, special, special_or));
        let ok2 = self.emit(Ty::Bool, Op::Bool(BoolOp::Or, ok_other, special_ok));
        let i2 = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: t1, b: one });
        self.branch(ok2, (three3, vec![t0, i1, i2, b3, b2]), (badb, vec![th]));
        self.cur = Some(three3);
        let t0 = three3p[0];
        let t1 = three3p[1];
        let t2 = three3p[2];
        let b3 = three3p[3];
        let b2 = three3p[4];
        let c2 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::Usize, a: t2, b: len });
        self.branch(c2, (badb, vec![t0]), (three4, vec![t0, i2]));
        let _ = b3;
        self.cur = Some(three4);
        let th = three4p[0];
        let t2 = three4p[1];
        let bp3 = self.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: t2 });
        let b3 = self.emit(Ty::U8, Op::MemLoad { p: bp3, off: 0 });
        let ge80 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::U8, a: b3, b: b80 });
        let lebf = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::U8, a: b3, b: b_bf });
        let is_cont3 = self.emit(Ty::Bool, Op::Bool(BoolOp::And, ge80, lebf));
        let i3 = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: t2, b: one });
        self.branch(is_cont3, (hd, vec![i3]), (badb, vec![th]));

        // ---------- 4 バイト (F0..F4) ----------
        self.cur = Some(four);
        let fo = fp_[0];
        let i1 = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: fo, b: one });
        let c1 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::Usize, a: i1, b: len });
        self.branch(c1, (badb, vec![fo]), (four2, vec![fo, i1]));
        self.cur = Some(four2);
        let v0 = four2p[0];
        let v1 = four2p[1];
        let bp2 = self.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: v0 });
        let b4 = self.emit(Ty::U8, Op::MemLoad { p: bp2, off: 0 });
        let bp2 = self.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: v1 });
        let b2 = self.emit(Ty::U8, Op::MemLoad { p: bp2, off: 0 });
        let eq_f4 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U8, a: b4, b: f4 });
        let ge_90 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::U8, a: b2, b: b_90 });
        let le_bf = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::U8, a: b2, b: b_bf });
        let ge80 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::U8, a: b2, b: b80 });
        let le_8f = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::U8, a: b2, b: b_8f });
        let _ = b2;
        let ok_f0 = self.emit(Ty::Bool, Op::Bool(BoolOp::And, ge_90, le_bf));
        let ok_f4 = self.emit(Ty::Bool, Op::Bool(BoolOp::And, ge80, le_8f));
        let ok_other = self.emit(Ty::Bool, Op::Bool(BoolOp::And, ge80, lebf));
        let eq_f0 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U8, a: b4, b: f0 });
        let eq_f4 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U8, a: b4, b: f4 });
        let special4 = self.emit(Ty::Bool, Op::Bool(BoolOp::Or, eq_f0, eq_f4));
        let special_or4 = self.emit(Ty::Bool, Op::Bool(BoolOp::Or, ok_f0, ok_f4));
        let special_ok4 = self.emit(Ty::Bool, Op::Bool(BoolOp::And, special4, special_or4));
        let ok4 = self.emit(Ty::Bool, Op::Bool(BoolOp::Or, ok_other, special_ok4));
        let i2 = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: v1, b: one });
        self.branch(ok4, (four3, vec![v0, i1, i2, b4, b2]), (badb, vec![fo]));
        self.cur = Some(four3);
        let w0 = four3p[0];
        let w1 = four3p[1];
        let w2 = four3p[2];
        let b4 = four3p[3];
        let b2 = four3p[4];

        let c2 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::Usize, a: w2, b: len });
        self.branch(c2, (badb, vec![w0]), (four4, vec![w0, w1, w2]));
        self.cur = Some(four4);
        let x1 = four4p[0];
        let x3 = four4p[1];
        let _ = b4;
        let bp3 = self.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: x3 });
        let b3 = self.emit(Ty::U8, Op::MemLoad { p: bp3, off: 0 });
        let ge80 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::U8, a: b3, b: b80 });
        let lebf = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::U8, a: b3, b: b_bf });
        let is_cont3 = self.emit(Ty::Bool, Op::Bool(BoolOp::And, ge80, lebf));
        let i4 = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: x3, b: one });
        self.branch(is_cont3, (hd, vec![i4]), (badb, vec![x1]));

        // ---------- 正常完了 / 不正 ----------
        self.cur = Some(okb);
        let ok = self.emit(Ty::Res(Pay::Agg(fat_idx), Pay::Agg(err_idx)), Op::Ok(bytes));
        self.goto(join, vec![ok]);
        self.cur = Some(badb);
        let vi = bp_[0];
        let err_val = self.emit(utf8_err, Op::Make { variant: None, fields: vec![vi] });
        let err = self.emit(Ty::Res(Pay::Agg(fat_idx), Pay::Agg(err_idx)), Op::Err(err_val));
        self.goto(join, vec![err]);
        self.cur = Some(join);
        self.blocks[join].term = Some(Term::Ret(jp[0]));
        Ok(Some(jp[0]))
    }
}


