//! 文字列・バイト列の操作の下ろし (設計_KIR.md 10.9・10.10)。
//!
//! - `&str` の `starts_with` / `ends_with` / `eq`: バイト列のループを KIR 命令で書く。
//! - バイト列リテラル `b"..."`: 静的なバイト列 (`static.bytes`) と長さから `fat` (`{ptr, len}`) を作る。
//! - `alloc.string.String.from_str(alloc, s)`: `s` のコピーをヒープに確保して `Ok(String)` を返す。
//! - `core.str.from_utf8(bytes)`: Rust の `core::str::from_utf8` と同じ検証を KIR 命令で書き、
//!   `Result<&str, Utf8Error>` を返す。`valid_up_to` は最後に完全だった文字の終わり。
//!
//! `kir_lower.rs` の子モジュールとして `Lw` の非公開の項目を使う。

use crate::check::{items, nf, nlineno as nline, nname, st};
use crate::kir::{Aop, BoolOp, Cc, Mode, Op, Pay, Ty, VId};
use crate::parser::V;
use crate::types::Ty as Src;

use super::{coll, parse_int, Lw, LR};

impl<'a> Lw<'a> {
    /// `e.m(args)` のうち、`&str` / `&[u8]` のメソッド (`starts_with` / `ends_with` / `eq`)。
    /// 対象外なら `None` (親の処理に任せる)。
    pub(super) fn str_method_call(&mut self, e: &V, kind: &coll::Kind, h: VId, m: &str, args: &[V]) -> Option<LR<Option<VId>>> {
        // `String` の `contains` / `starts_with` / `ends_with`: 受け手はヘッダなので、先頭の {ptr, len} を借用の
        // `fat` として `&str` の規則で比べる (設計_KIR.md 23 章)。
        if matches!(kind, coll::Kind::String) && matches!(m, "contains" | "starts_with" | "ends_with") {
            let view = super::coll::hdr_view(self, h);
            return self.str_method_call(e, &coll::Kind::Str, view, m, args);
        }
        if !matches!(kind, coll::Kind::Str | coll::Kind::Slice(_)) {
            return None;
        }
        let line = nline(e);
        match m {
            // `&str` の部分列の有無 (`find` の一致を `Option` の tag で見る)。スライスの `contains` は要素の比較 (coll.rs)。
            "contains" if matches!(kind, coll::Kind::Str) => Some(self.contains_call(e, h, args)),
            // `s.split_at(m)`: `Option<(&str, &str)>` / `Option<(&[T], &[T])>`。`m` が長さの外、または `&str` の
            // UTF-8 の途中なら `None` (panic しない。木走査の `split_at` と同じ)。
            "split_at" => {
                if args.len() != 1 {
                    return Some(self.err(line, "メソッド split_at の引数は 1 個のはず"));
                }
                let Some(mv) = self.expr(&args[0]).ok()? else { return Some(Ok(None)) };
                if self.vty(mv) != Ty::Usize {
                    return Some(self.err(line, "メソッド split_at の引数は usize のはず"));
                }
                let t = self.ty_of(e).ok()?;
                if !matches!(t, Ty::Opt(_)) {
                    return Some(self.err(line, "メソッド split_at の結果の型が Option ではない"));
                }
                let is_str = matches!(kind, coll::Kind::Str);
                Some(self.split_at_call(e, line, h, mv, is_str).map(Some))
            }
            // `&str` の ASCII の大文字・小文字を無視した等しさ (長さが等しく、ASCII の A-Z だけを小文字にして比べる)。
            "eq_ignore_ascii_case" => {
                if !matches!(kind, coll::Kind::Str) {
                    return None;
                }
                if args.len() != 1 {
                    return Some(self.err(line, "メソッド eq_ignore_ascii_case の引数は 1 個のはず"));
                }
                let Some(o) = self.expr(&args[0]).ok()? else { return Some(Ok(None)) };
                if self.vty(o) != self.vty(h) {
                    return Some(self.err(line, "メソッド eq_ignore_ascii_case の引数の型が受け手と違う"));
                }
                let t = self.ty_of(e).ok()?;
                if !matches!(t, Ty::Bool) {
                    return Some(self.err(line, "メソッド eq_ignore_ascii_case の結果の型が bool ではない"));
                }
                Some(self.str_eq_with(h, o, true).map(Some))
            }
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
            // `s.chars()` を局所に持つ: 残りの `&str` そのものが反復子の状態 (`chars_next` が読んで進める)。
            "chars" => {
                if !matches!(kind, coll::Kind::Str) || !args.is_empty() {
                    return None;
                }
                if self.vty(h) != self.fat_ty() || self.ty_of(e).ok() != Some(self.fat_ty()) {
                    return None;
                }
                Some(Ok(Some(h)))
            }
            // `&str` の先頭/末尾から ASCII 空白だけを削る。結果は元の文字列の部分 (ptr, len) の fat 値。
            "trim" | "trim_start" | "trim_end" => {
                if !matches!(kind, coll::Kind::Str) {
                    return None;
                }
                if args.len() != 0 {
                    return Some(self.err(line, format!("メソッド {} の引数は 0 個のはず", m)));
                }
                let t = self.ty_of(e).ok()?;
                if !matches!(t, Ty::Agg(_)) {
                    return Some(self.err(line, format!("メソッド {} の結果の型が &str ではない", m)));
                }
                Some(match self.trim_call(line, h, m) {
                    Ok(v) => Ok(Some(v)),
                    Err(e) => Err(e),
                })
            }
            // `&str` のプレフィックス/サフィックスを削り、`Option<&str>` を返す。
            "strip_prefix" | "strip_suffix" => {
                if !matches!(kind, coll::Kind::Str) {
                    return None;
                }
                if args.len() != 1 {
                    return Some(self.err(line, format!("メソッド {} の引数は 1 個のはず", m)));
                }
                let Some(pat) = self.expr(&args[0]).ok()? else { return Some(Ok(None)) };
                if self.vty(pat) != self.vty(h) {
                    return Some(self.err(line, format!("メソッド {} の引数の型が受け手と違う", m)));
                }
                let t = self.ty_of(e).ok()?;
                if !matches!(t, Ty::Opt(_)) {
                    return Some(self.err(line, format!("メソッド {} の結果の型が Option<&str> ではない", m)));
                }
                Some(match self.strip_call(line, h, pat, m == "strip_prefix") {
                    Ok(v) => Ok(Some(v)),
                    Err(e) => Err(e),
                })
            }
            // `self.find(pat)`: 最初の一致の先頭のバイト位置。空のパターンは `Some(0)` (木走査の Rust と同じ)。
            "find" => {
                if !matches!(kind, coll::Kind::Str) {
                    return None;
                }
                if args.len() != 1 {
                    return Some(self.err(line, "メソッド find の引数は 1 個のはず"));
                }
                let Some(pat) = self.expr(&args[0]).ok()? else { return Some(Ok(None)) };
                if self.vty(pat) != self.vty(h) {
                    return Some(self.err(line, "メソッド find の引数の型が受け手と違う"));
                }
                let t = self.ty_of(e).ok()?;
                if t != Ty::Opt(Pay::Usize) {
                    return Some(self.err(line, "メソッド find の結果の型が Option<usize> ではない"));
                }
                Some(self.find_call(h, pat).map(Some))
            }
            // `self.rfind(pat)`: 最後の一致の先頭のバイト位置 (空のパターンは `Some(len)`)。組み込みは持たず、
            // Kasane で書いた `core.str.rfind` (`spec/stdlib/core/str.ksnl`) を単相化して呼ぶ (設計_KIR.md 29 章)。
            "rfind" => {
                if !matches!(kind, coll::Kind::Str) {
                    return None;
                }
                if args.len() != 1 {
                    return Some(self.err(line, "メソッド rfind の引数は 1 個のはず"));
                }
                let t = self.ty_of(e).ok()?;
                if t != Ty::Opt(Pay::Usize) {
                    return Some(self.err(line, "メソッド rfind の結果の型が Option<usize> ではない"));
                }
                let at = self.ty_of(&args[0]).ok()?;
                if at != self.fat_ty() {
                    return Some(self.err(line, "メソッド rfind の引数の型が受け手と違う"));
                }
                Some(self.str_kasane_call(e, line, h, "rfind", args))
            }
            // `self.split_once(sep)`。空の区切りは「見つからない」(None)。
            // 見つかれば `Option<(&str, &str)>` で、タプル集成体を Option のペイロードにする。
            "split_once" => {
                if !matches!(kind, coll::Kind::Str) {
                    return None;
                }
                if args.len() != 1 {
                    return Some(self.err(line, format!("メソッド {} の引数は 1 個のはず", m)));
                }
                let Some(sep) = self.expr(&args[0]).ok()? else { return Some(Ok(None)) };
                if self.vty(sep) != self.vty(h) {
                    return Some(self.err(line, format!("メソッド {} の引数の型が受け手と違う", m)));
                }
                let t = self.ty_of(e).ok()?;
                if !matches!(t, Ty::Opt(_)) {
                    return Some(self.err(line, format!("メソッド {} の結果の型が Option<(&str, &str)> ではない", m)));
                }
                Some(match self.split_once_call(e, line, h, sep) {
                    Ok(v) => Ok(Some(v)),
                    Err(e) => Err(e),
                })
            }
            _ => None,
        }
    }

    /// `s.m(args)` (`s` は `&str` の fat): Kasane で書いた `core.str.<m>` (`spec/stdlib/core/str.ksnl`) を、受け手 `h` で呼ぶ。
    /// 引数は `kasane_call` が評価する (ここでは先に評価しない)。定義が無ければ診断を出す。
    fn str_kasane_call(&mut self, e: &V, line: usize, h: VId, m: &str, args: &[V]) -> LR<Option<VId>> {
        let Some(di) = super::ext::find_def(self, &format!("core.str.{}", m)) else {
            return self.err(line, format!("メソッド {} は段階 1 の対象外 (core.str.{} の Kasane 版がない)", m, m));
        };
        let recv = Src::Ref(false, Box::new(Src::Prim("str".into())));
        super::ext::kasane_call(self, e, line, di, h, recv, args)
    }

    /// ASCII 空白 (`\t` `\n` `\x0C` `\r` ` `) か。0x0B は含まない (natives.rs と合わせる)。
    fn is_ascii_space(&mut self, c: VId) -> LR<VId> {
        let c32 = self.emit(Ty::U32, Op::Conv { src: Ty::U8, a: c });
        let mut eq = |val: u32| {
            let cv = self.emit(Ty::U32, Op::Const(val as i128));
            self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U32, a: c32, b: cv })
        };
        // ASCII 空白は 0x20, \t 0x09, \n 0x0A, \r 0x0D, \x0C の 5 つ。0x0B は含まない (natives.rs と合わせる)。
        let r9 = eq(0x09);
        let r0a = eq(0x0A);
        let r0c = eq(0x0C);
        let r0d = eq(0x0D);
        let r20 = eq(0x20);
        let r90a = self.emit(Ty::Bool, Op::Bool(BoolOp::Or, r9, r0a));
        let r0c0d = self.emit(Ty::Bool, Op::Bool(BoolOp::Or, r0c, r0d));
        let r90a0c0d = self.emit(Ty::Bool, Op::Bool(BoolOp::Or, r90a, r0c0d));
        Ok(self.emit(Ty::Bool, Op::Bool(BoolOp::Or, r90a0c0d, r20)))
    }

    /// `self.trim()` / `trim_start()` / `trim_end()`。結果は元の `self` の部分 (ptr, len) の fat 値。
    fn trim_call(&mut self, line: usize, s: VId, m: &str) -> LR<VId> {
        let sp = self.emit(Ty::Ptr, Op::Field { a: s, off: 0 });
        let sl = self.emit(Ty::Usize, Op::Field { a: s, off: 8 });
        let zero = self.emit(Ty::Usize, Op::Const(0));
        let one = self.emit(Ty::Usize, Op::Const(1));
        let fat = self.fat_ty();
        let Ty::Agg(_fat_idx) = fat else {
            return self.err(line, "fat の型が集成体でない");
        };

        // ---- 先頭の空白をスキップして start を求める (i < sl && space(s[i]) なら i+1) ----
        let (sh, shp) = self.new_block(&[Ty::Usize]);
        let (ss, _) = self.new_block(&[]);
        let (sd, sdp) = self.new_block(&[Ty::Usize]);
        self.goto(sh, vec![zero]);
        self.cur = Some(sh);
        let i = shp[0];
        let i_lt = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: i, b: sl });
        self.branch(i_lt, (ss, vec![]), (sd, vec![i]));
        self.cur = Some(ss);
        let bp = self.emit(Ty::Ptr, Op::PtrAdd { p: sp, off: i });
        let bc = self.emit(Ty::U8, Op::MemLoad { p: bp, off: 0 });
        let spc = self.is_ascii_space(bc)?;
        let ni = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: i, b: one });
        self.branch(spc, (sh, vec![ni]), (sd, vec![i]));
        self.cur = Some(sd);
        let start = sdp[0];

        // trim_start ならここで結果を作る。
        if m == "trim_start" {
            let new_ptr = self.emit(Ty::Ptr, Op::PtrAdd { p: sp, off: start });
            let new_len = self.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: sl, b: start });
            return Ok(self.emit(fat, Op::Make { variant: None, fields: vec![new_ptr, new_len] }));
        }

        // ---- 末尾の空白をスキップして end を求める (j > start && space(s[j-1]) なら j-1) ----
        let (eh, ehp) = self.new_block(&[Ty::Usize]);
        let (es, _) = self.new_block(&[]);
        let (ed, edp) = self.new_block(&[Ty::Usize]);
        self.goto(eh, vec![sl]);
        self.cur = Some(eh);
        let j = ehp[0];
        let j_gt = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::Usize, a: j, b: start });
        self.branch(j_gt, (es, vec![]), (ed, vec![j]));
        self.cur = Some(es);
        let jm1 = self.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: j, b: one });
        let bp = self.emit(Ty::Ptr, Op::PtrAdd { p: sp, off: jm1 });
        let bc = self.emit(Ty::U8, Op::MemLoad { p: bp, off: 0 });
        let spc = self.is_ascii_space(bc)?;
        let nj = self.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: j, b: one });
        self.branch(spc, (eh, vec![nj]), (ed, vec![j]));
        self.cur = Some(ed);
        let end = edp[0];

        let new_ptr = if m == "trim_end" { sp } else { self.emit(Ty::Ptr, Op::PtrAdd { p: sp, off: start }) };
        let new_len = match m {
            "trim_end" => end,
            _ => self.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: end, b: start }),
        };
        Ok(self.emit(fat, Op::Make { variant: None, fields: vec![new_ptr, new_len] }))
    }

    /// `self.strip_prefix(pat)` / `strip_suffix(pat)`。マッチすれば `Some(部分)`、そうでなければ `None`。
    fn strip_call(&mut self, line: usize, s: VId, pat: VId, is_prefix: bool) -> LR<VId> {
        let sp = self.emit(Ty::Ptr, Op::Field { a: s, off: 0 });
        let sl = self.emit(Ty::Usize, Op::Field { a: s, off: 8 });
        let pl = self.emit(Ty::Usize, Op::Field { a: pat, off: 8 });
        let fat = self.fat_ty();
        let Ty::Agg(fat_idx) = fat else {
            return self.err(line, "fat の型が集成体でない");
        };
        let opt_ty = Ty::Opt(Pay::Agg(fat_idx));

        let is_match = self.str_starts_or_ends_with(s, pat, is_prefix)?;

        let (yesb, _) = self.new_block(&[]);
        let (nob, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[opt_ty]);
        let none = self.emit(opt_ty, Op::Const(0));
        self.branch(is_match, (yesb, vec![]), (nob, vec![]));
        self.cur = Some(yesb);
        let new_ptr = if is_prefix {
            self.emit(Ty::Ptr, Op::PtrAdd { p: sp, off: pl })
        } else {
            sp
        };
        let new_len = self.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: sl, b: pl });
        let fat_val = self.emit(fat, Op::Make { variant: None, fields: vec![new_ptr, new_len] });
        let some = self.emit(opt_ty, Op::Some(fat_val));
        self.goto(join, vec![some]);
        self.cur = Some(nob);
        self.goto(join, vec![none]);
        self.cur = Some(join);
        Ok(jp[0])
    }

    /// `self.split_once(sep)`。空の区切りは None。見つかれば `Option<(&str, &str)>` で、
    /// タプル `{ptr1, len1, ptr2, len2}` を Option のペイロードにする。
    fn split_once_call(&mut self, e: &V, line: usize, s: VId, sep: VId) -> LR<VId> {
        let sp = self.emit(Ty::Ptr, Op::Field { a: s, off: 0 });
        let sl = self.emit(Ty::Usize, Op::Field { a: s, off: 8 });
        let spp = self.emit(Ty::Ptr, Op::Field { a: sep, off: 0 });
        let spl = self.emit(Ty::Usize, Op::Field { a: sep, off: 8 });
        let zero = self.emit(Ty::Usize, Op::Const(0));
        let one = self.emit(Ty::Usize, Op::Const(1));
        let opt_ty = self.ty_of(e)?;
        let Ty::Opt(p) = opt_ty else {
            return self.err(line, "split_once の結果の型が Option ではない");
        };
        let tuple_ty = p.ty();

        let (nf, _) = self.new_block(&[]);
        let (hd, hp) = self.new_block(&[Ty::Usize]);
        let (cmp, cdp) = self.new_block(&[Ty::Usize, Ty::Usize]);
        let (t1, _) = self.new_block(&[Ty::Usize, Ty::Usize]);
        let (fb, fp) = self.new_block(&[Ty::Usize]);
        let (join, jp) = self.new_block(&[opt_ty]);

        let sep_empty = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: spl, b: zero });
        let sep_gt = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::Usize, a: spl, b: sl });
        let no_sep = self.emit(Ty::Bool, Op::Bool(BoolOp::Or, sep_empty, sep_gt));
        self.branch(no_sep, (nf, vec![]), (hd, vec![zero]));

        self.cur = Some(hd);
        let i = hp[0];
        let end = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: i, b: spl });
        let i_past = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::Usize, a: end, b: sl });
        self.branch(i_past, (nf, vec![]), (cmp, vec![i, zero]));

        self.cur = Some(cmp);
        let ci = cdp[0];
        let ck = cdp[1];
        let k_eq = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: ck, b: spl });
        self.branch(k_eq, (fb, vec![ci]), (t1, vec![ci, ck]));

        self.cur = Some(t1);
        let t1i = cdp[0];
        let t1k = cdp[1];
        let ck = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: t1i, b: t1k });
        let sp_ = self.emit(Ty::Ptr, Op::PtrAdd { p: sp, off: ck });
        let sb = self.emit(Ty::U8, Op::MemLoad { p: sp_, off: 0 });
        let cp = self.emit(Ty::Ptr, Op::PtrAdd { p: spp, off: t1k });
        let cb = self.emit(Ty::U8, Op::MemLoad { p: cp, off: 0 });
        let diff = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ne, ty: Ty::U8, a: sb, b: cb });
        let ni = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: t1i, b: one });
        let k1 = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: t1k, b: one });
        self.branch(diff, (hd, vec![ni]), (cmp, vec![t1i, k1]));

        self.cur = Some(fb);
        let fi = fp[0];
        let fat = self.fat_ty();
        let right_start = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: fi, b: spl });
        let right_ptr = self.emit(Ty::Ptr, Op::PtrAdd { p: sp, off: right_start });
        let right_len = self.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: sl, b: right_start });
        let left_fat = self.emit(fat, Op::Make { variant: None, fields: vec![sp, fi] });
        let right_fat = self.emit(fat, Op::Make { variant: None, fields: vec![right_ptr, right_len] });
        let tuple = self.emit(tuple_ty, Op::Make { variant: None, fields: vec![left_fat, right_fat] });
        let some = self.emit(opt_ty, Op::Some(tuple));
        self.goto(join, vec![some]);

        self.cur = Some(nf);
        let none = self.emit(opt_ty, Op::Const(0));
        self.goto(join, vec![none]);
        self.cur = Some(join);
        Ok(jp[0])
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

    /// 式 `e` の型が `&str` か (演算子 `==` を `str_eq` に回すかの判定)。型表に無ければ false。
    /// `s.find(pat)` (`&str` のパターン)。空のパターンは `Some(0)`、`pat` が `s` より長ければ `None`。
    /// 外側は開始位置 `i` (0..=len-plen)、内側は `k` (0..plen) で `s[i+k] == pat[k]` を比べる。
    fn find_call(&mut self, s: VId, pat: VId) -> LR<VId> {
        let sp = self.emit(Ty::Ptr, Op::Field { a: s, off: 0 });
        let sl = self.emit(Ty::Usize, Op::Field { a: s, off: 8 });
        let pp = self.emit(Ty::Ptr, Op::Field { a: pat, off: 0 });
        let pl = self.emit(Ty::Usize, Op::Field { a: pat, off: 8 });
        let zero = self.emit(Ty::Usize, Op::Const(0));
        let one = self.emit(Ty::Usize, Op::Const(1));
        let opt = Ty::Opt(Pay::Usize);
        let pat_empty = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: pl, b: zero });
        let too_short = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::Usize, a: sl, b: pl });
        // 最後に試す開始位置 (sl - pl)。`too_short` のときは使わない (wrap の減算は trap しない)。
        let last = self.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: sl, b: pl });

        let (c1, _) = self.new_block(&[]);
        let (oh, ohp) = self.new_block(&[Ty::Usize]);
        let (ih, ihp) = self.new_block(&[Ty::Usize]);
        let (cmpb, _) = self.new_block(&[]);
        let (nexti, _) = self.new_block(&[]);
        let (foundb, _) = self.new_block(&[]);
        let (someb, _) = self.new_block(&[]);
        let (noneb, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[opt]);

        self.branch(pat_empty, (someb, vec![]), (c1, vec![]));
        self.cur = Some(c1);
        self.branch(too_short, (noneb, vec![]), (oh, vec![zero]));
        self.cur = Some(oh);
        let i = ohp[0];
        let past = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::Usize, a: i, b: last });
        self.branch(past, (noneb, vec![]), (ih, vec![zero]));
        self.cur = Some(ih);
        let k = ihp[0];
        let done = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::Usize, a: k, b: pl });
        self.branch(done, (foundb, vec![]), (cmpb, vec![]));
        self.cur = Some(cmpb);
        let ik = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: i, b: k });
        let ap = self.emit(Ty::Ptr, Op::PtrAdd { p: sp, off: ik });
        let bp = self.emit(Ty::Ptr, Op::PtrAdd { p: pp, off: k });
        let ca = self.emit(Ty::U8, Op::MemLoad { p: ap, off: 0 });
        let cb = self.emit(Ty::U8, Op::MemLoad { p: bp, off: 0 });
        let eq = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U8, a: ca, b: cb });
        let nk = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: k, b: one });
        self.branch(eq, (ih, vec![nk]), (nexti, vec![]));
        self.cur = Some(nexti);
        let ni = self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: i, b: one });
        self.goto(oh, vec![ni]);
        self.cur = Some(foundb);
        let found = self.emit(opt, Op::Some(i));
        self.goto(join, vec![found]);
        self.cur = Some(someb);
        let at0 = self.emit(opt, Op::Some(zero));
        self.goto(join, vec![at0]);
        self.cur = Some(noneb);
        let none = self.emit(opt, Op::Const(0));
        self.goto(join, vec![none]);
        self.cur = Some(join);
        Ok(jp[0])
    }

    pub(super) fn is_str_ref(&self, e: &V) -> bool {
        matches!(self.src_ty(e), Ok(Src::Ref(_, i)) if matches!(&*i, Src::Prim(s) if s == "str"))
    }

    /// `self.contains(pat)` (`&str`)。`find` の一致の有無 (`Option` の tag) で決める。
    fn contains_call(&mut self, e: &V, h: VId, args: &[V]) -> LR<Option<VId>> {
        let line = nline(e);
        if args.len() != 1 {
            return self.err(line, "メソッド contains の引数は 1 個のはず");
        }
        let Some(pat) = self.expr(&args[0])? else { return Ok(None) };
        let fat = self.fat_ty();
        if self.vty(pat) != fat {
            return self.err(line, "メソッド contains の引数は &str のはず");
        }
        if self.ty_of(e)? != Ty::Bool {
            return self.err(line, "メソッド contains の結果の型が bool ではない");
        }
        let found = self.find_call(h, pat)?;
        Ok(Some(self.emit(Ty::Bool, Op::Tag(found))))
    }

    /// `s.split_at(m)` (`&str` / スライス)。`m <= len` (`&str` は `m` が文字の境界) なら `Some((前, 後))`、外なら `None`。
    fn split_at_call(&mut self, e: &V, line: usize, s: VId, m: VId, is_str: bool) -> LR<VId> {
        let sp = self.emit(Ty::Ptr, Op::Field { a: s, off: 0 });
        let sl = self.emit(Ty::Usize, Op::Field { a: s, off: 8 });
        let opt_ty = self.ty_of(e)?;
        let Ty::Opt(p) = opt_ty else {
            return self.err(line, "split_at の結果の型が Option ではない");
        };
        let tuple_ty = p.ty();
        let fat = self.fat_ty();

        let (chk, _) = self.new_block(&[]);
        let (bnd, _) = self.new_block(&[]);
        let (ok, _) = self.new_block(&[]);
        let (none, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[opt_ty]);

        let in_range = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::Usize, a: m, b: sl });
        self.branch(in_range, (chk, vec![]), (none, vec![]));

        // `&str` は `m` が長さと等しいか、そこのバイトが UTF-8 の続きバイト (0x80..=0xBF) でないとき境界
        self.cur = Some(chk);
        if is_str {
            let at_end = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: m, b: sl });
            self.branch(at_end, (ok, vec![]), (bnd, vec![]));
            self.cur = Some(bnd);
            let bp = self.emit(Ty::Ptr, Op::PtrAdd { p: sp, off: m });
            let byte = self.emit(Ty::U8, Op::MemLoad { p: bp, off: 0 });
            let lo = self.emit(Ty::U8, Op::Const(0x80));
            let hi = self.emit(Ty::U8, Op::Const(0xBF));
            let ge = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::U8, a: byte, b: lo });
            let le = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::U8, a: byte, b: hi });
            let cont = self.emit(Ty::Bool, Op::Bool(BoolOp::And, ge, le));
            self.branch(cont, (none, vec![]), (ok, vec![]));
        } else {
            self.goto(ok, vec![]);
        }

        self.cur = Some(ok);
        let left = self.emit(fat, Op::Make { variant: None, fields: vec![sp, m] });
        let right_ptr = self.emit(Ty::Ptr, Op::PtrAdd { p: sp, off: m });
        let right_len = self.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: sl, b: m });
        let right = self.emit(fat, Op::Make { variant: None, fields: vec![right_ptr, right_len] });
        let tuple = self.emit(tuple_ty, Op::Make { variant: None, fields: vec![left, right] });
        let some = self.emit(opt_ty, Op::Some(tuple));
        self.goto(join, vec![some]);

        self.cur = Some(none);
        let nothing = self.emit(opt_ty, Op::Const(0));
        self.goto(join, vec![nothing]);
        self.cur = Some(join);
        Ok(jp[0])
    }

    /// `self == other` (`&str` のバイト列の等しさ)。
    pub(super) fn str_eq(&mut self, a: VId, b: VId) -> LR<VId> {
        self.str_eq_with(a, b, false)
    }

    /// `&str` の等しさ。`fold` なら ASCII の A-Z を小文字にして比べる (`eq_ignore_ascii_case`)。
    fn str_eq_with(&mut self, a: VId, b: VId, fold: bool) -> LR<VId> {
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
        let eq = if fold {
            // ASCII の大文字・小文字を無視する: 両方を u32 に広げ、A-Z を小文字にしてから比べる (他のバイトは変えない)。
            let wa = self.emit(Ty::U32, Op::Conv { src: Ty::U8, a: ca });
            let wb = self.emit(Ty::U32, Op::Conv { src: Ty::U8, a: cb });
            let la = self.ascii_case(wa, false);
            let lb = self.ascii_case(wb, false);
            self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U32, a: la, b: lb })
        } else {
            self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U8, a: ca, b: cb })
        };
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
        // `&[0xffu8, 0xfeu8]` (配列リテラルへの参照): 配列の値がすでにスライスと同じ `{ptr, len}` なので、それをそのまま検査する。
        let a0 = &args[0];
        let bytes = if nname(a0) == "Unary" && st(&nf(a0)[0]) == "&" && nname(&nf(a0)[1]) == "ArrayE" {
            let Some((v, _)) = self.array_lit(&nf(a0)[1])? else { return Ok(None) };
            v
        } else if let Some(v) = self.array_local_fat(a0) {
            v
        } else {
            let Some(v) = self.expr(a0)? else { return Ok(None) };
            v
        };
        self.from_utf8_of(bytes, line)
    }

    /// `&a` (`a` は配列リテラルで作ったスライスの局所): 局所が持つスライスの値 (`fat`)。そうでなければ `None`。
    fn array_local_fat(&mut self, a: &V) -> Option<VId> {
        if nname(a) != "Unary" || st(&nf(a)[0]) != "&" {
            return None;
        }
        let name = super::path1(&nf(a)[1])?;
        let s = self.lookup(&name)?;
        if !matches!(self.coll.kinds.get(&s), Some(coll::Kind::Slice(Some(_)))) || self.slots[s].1 != self.fat_ty() {
            return None;
        }
        Some(if self.is_mem_val(self.slots[s].0) { self.read_slot(s) } else { self.slots[s].0 })
    }

    /// `&[u8]` の値 `bytes` (`fat`) を UTF-8 として検査し、`Result<&str, Utf8Error>` にする。
    pub(super) fn from_utf8_of(&mut self, bytes: VId, line: usize) -> LR<Option<VId>> {
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



impl<'a> Lw<'a> {
    /// `it.next()` (`it` は局所の `s.chars()` の反復子 = 残りの `&str`): 空なら `None`、そうでなければ先頭の文字を UTF-8 から復号して
    /// `Some(c)` にし、`it` を文字の幅だけ進める (`for c in s.chars()` と同じ復号。`it` は合流へ渡す局所)。
    pub(super) fn chars_next(&mut self, e: &V, s: usize) -> LR<Option<VId>> {
        let line = nline(e);
        let res = self.ty_of(e)?;
        if res != Ty::Opt(Pay::U32) {
            return self.err(line, "chars の next の結果が Option<char> ではない");
        }
        let fat = self.fat_ty();
        let cur = self.read_slot(s);
        let ptr = self.emit(Ty::Ptr, Op::Field { a: cur, off: 0 });
        let len = self.emit(Ty::Usize, Op::Field { a: cur, off: 8 });
        let zero = self.emit(Ty::Usize, Op::Const(0));
        let (some_b, _) = self.new_block(&[]);
        let (t1, _) = self.new_block(&[]);
        let (t2, _) = self.new_block(&[]);
        let (w1, _) = self.new_block(&[]);
        let (w2, _) = self.new_block(&[]);
        let (w3, _) = self.new_block(&[]);
        let (w4, _) = self.new_block(&[]);
        // join(結果, 残りの ptr, 残りの len)
        let (join, jp) = self.new_block(&[res, Ty::Ptr, Ty::Usize]);
        let empty = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: len, b: zero });
        let none = self.emit(res, Op::Const(0));
        self.branch(empty, (join, vec![none, ptr, len]), (some_b, vec![]));
        self.cur = Some(some_b);
        let b0 = self.emit(Ty::U8, Op::MemLoad { p: ptr, off: 0 });
        let b0u = self.emit(Ty::U32, Op::Conv { src: Ty::U8, a: b0 });
        let c80 = self.emit(Ty::U32, Op::Const(0x80));
        let lt80 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::U32, a: b0u, b: c80 });
        self.branch(lt80, (w1, vec![]), (t1, vec![]));
        self.cur = Some(t1);
        let ce0 = self.emit(Ty::U32, Op::Const(0xE0));
        let lte0 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::U32, a: b0u, b: ce0 });
        self.branch(lte0, (w2, vec![]), (t2, vec![]));
        self.cur = Some(t2);
        let cf0 = self.emit(Ty::U32, Op::Const(0xF0));
        let ltf0 = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::U32, a: b0u, b: cf0 });
        self.branch(ltf0, (w3, vec![]), (w4, vec![]));
        for (blk, n, mask) in [(w1, 1u32, 0x7Fi128), (w2, 2, 0x1F), (w3, 3, 0x0F), (w4, 4, 0x07)] {
            self.cur = Some(blk);
            let mut acc: Option<VId> = None;
            for k in 0..n {
                let ch = if k == 0 {
                    b0u
                } else {
                    let kc = self.emit(Ty::Usize, Op::Const(k as i128));
                    let p = self.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: kc });
                    let b = self.emit(Ty::U8, Op::MemLoad { p, off: 0 });
                    self.emit(Ty::U32, Op::Conv { src: Ty::U8, a: b })
                };
                let m = self.emit(Ty::U32, Op::Const(if k == 0 { mask } else { 0x3F }));
                let masked = self.emit(Ty::U32, Op::Bool(BoolOp::And, ch, m));
                let sh = 6 * (n - 1 - k);
                let part = if sh == 0 {
                    masked
                } else {
                    let ns = self.emit(Ty::U32, Op::Const(sh as i128));
                    self.emit(Ty::U32, Op::Shift { left: true, a: masked, n: ns })
                };
                acc = Some(match acc {
                    None => part,
                    Some(a) => self.emit(Ty::U32, Op::Bool(BoolOp::Or, a, part)),
                });
            }
            let w = self.emit(Ty::Usize, Op::Const(n as i128));
            // 不正な長さ (残りが幅より短い) は `&str` の不変条件 (UTF-8) で起きない。
            let np = self.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: w });
            let nl = self.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: len, b: w });
            let some = self.emit(res, Op::Some(acc.expect("n >= 1")));
            self.goto(join, vec![some, np, nl]);
        }
        self.cur = Some(join);
        let nf2 = self.emit(fat, Op::Make { variant: None, fields: vec![jp[1], jp[2]] });
        self.assign_slot(s, nf2);
        Ok(Some(jp[0]))
    }
}
