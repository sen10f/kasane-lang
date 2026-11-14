//! `core.check.eq(a, b)` / `core.check.is_true(c)` の下ろし (設計_KIR.md 13.2)。`kir_lower.rs` の子モジュール。
//!
//! - `core.check.eq(a, b)`: 等しさは `<型>.$eq` (kir_debug.rs) を呼ぶ。違えば、stderr に
//!   `panic: assertion failed: left: <a の表示>, right: <b の表示>` を書いて終了コード 101 (木走査と同じ文言)。
//!   表示は `<型>.$debug` を呼んで `eprint` で部分ごとに書く。文言は動的なので `Op::Panic` (静的な文言) を使わない。
//! - `core.check.is_true(c)`: `c` が偽なら `panic: assertion failed: condition is false` (静的な文言の `Op::Panic`)。
//!
//! 両方とも、式として値 `()` を返す (成功の枝は続く)。引数の型が下ろせない・表示できないときは理由付きで止める。

use crate::check::{items, nf, nlineno as nline, nname, strs};
use crate::kir::{Cc, Op, Pay, Pointee, Term, Ty, VId};
use crate::parser::V;
use crate::types::Ty as Src;

use super::debug::Gen;
use super::{LR, Lw};

impl<'a> Lw<'a> {
    /// `core.check.eq` / `core.check.is_true` の呼び出し。対象外の形なら `None` (親の処理に任せる)。
    pub(super) fn check_call(&mut self, e: &V) -> Option<LR<Option<VId>>> {
        let f = nf(e);
        if nname(&f[0]) != "Path" {
            return None;
        }
        let segs = strs(&nf(&f[0])[0]);
        if segs.len() != 3 || segs[0] != "core" || segs[1] != "check" {
            return None;
        }
        match segs[2].as_str() {
            "eq" => Some(self.check_eq(e)),
            "is_true" => Some(self.check_true(e)),
            _ => None,
        }
    }

    fn check_eq(&mut self, e: &V) -> LR<Option<VId>> {
        let line = nline(e);
        let args = items(&nf(e)[1]);
        if args.len() != 2 {
            return self.err(line, "core.check.eq の引数は 2 個のはず");
        }
        // 両辺は比べるだけ: 局所変数は移さずに読み (スコープの終わりに解放される)、一時の値は比べた後に解放する
        // (設計_KIR.md 23 章)。
        let Some(a) = self.expr_peek(&args[0])? else { return Ok(None) };
        let Some(b) = self.expr_peek(&args[1])? else { return Ok(None) };
        let temp_a = super::coll::val_slot(self, &args[0]).is_none() && super::own::chain_of(self, &args[0]).is_none();
        let temp_b = super::coll::val_slot(self, &args[1]).is_none() && super::own::chain_of(self, &args[1]).is_none();
        // `&[T; N]` の局所 (ref<fat>) と `&[T]` (fat) の比較: 参照を外して fat の値にそろえる。
        let fat = self.fat_ty();
        let unref = |lw: &mut Self, v: VId| match lw.vty(v) {
            Ty::Ref(_, Pointee::Agg(i)) if Ty::Agg(i) == fat => lw.emit(fat, Op::Load(v)),
            _ => v,
        };
        let (a, b) = (unref(self, a), unref(self, b));
        let (ta, tb) = (self.vty(a), self.vty(b));
        if ta != tb {
            return self.err(line, "core.check.eq の両辺の型が違う");
        }
        let sa = self.src_ty(&args[0])?;
        let file = self.def.file.clone();
        // 参照越しのスカラー (&T / &mut T) は参照先を読んでスカラーどうしにする。
        // `&u8` だけでなく整数・bool・char・浮動小数への参照も同様。
        let (a, b, sa, ta) = if matches!(ta, Ty::Ref(_, p) if !matches!(p, Pointee::Agg(_))) {
            if let Src::Ref(false, inner) = &sa {
                let t = match ta { Ty::Ref(_, p) => p.ty(), _ => unreachable!() };
                (self.emit(t, Op::Load(a)), self.emit(t, Op::Load(b)), (*inner).as_ref().clone(), t)
            } else {
                (a, b, sa, ta)
            }
        } else {
            (a, b, sa, ta)
        };
        // 浮動小数は $eq / $debug 未対応なので、Cmp (IEEE ==) と FmtFloat で下ろす。
        if ta.is_float() {
            let bits = match ta { Ty::F32 => 32, Ty::F64 => 64, _ => unreachable!() };
            let same = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: ta, a, b });
            let (yes, _) = self.new_block(&[]);
            let (no, _) = self.new_block(&[]);
            self.branch(same, (yes, vec![]), (no, vec![]));
            self.cur = Some(no);
            let fat = self.aggs.borrow_mut().stderr_fat(self.p)?;
            self.put_static(fat, "panic: assertion failed: left: ");
            self.emit(Ty::Unit, Op::FmtFloat { bits, a });
            self.put_static(fat, ", right: ");
            self.emit(Ty::Unit, Op::FmtFloat { bits, a: b });
            self.put_static(fat, "\n");
            self.emit(Ty::Unit, Op::Abort);
            self.terminate(Term::Unreachable);
            self.cur = Some(yes);
            return Ok(Some(self.emit(Ty::Unit, Op::Const(0))));
        }

        // 比べる関数と、表示の関数 (失敗の文言)。両方を作れるときだけ下ろす。
        let mut g = Gen::new(self.p, self.aggs);
        let eq = g.need_eq(&sa, ta, &file).map_err(|w| format!("行 {}: core.check.eq の比較を下ろせない: {}", line, w))?;
        let dbg = g.need(&sa, ta, &file).map_err(|w| format!("行 {}: core.check.eq の失敗の表示を下ろせない: {}", line, w))?;
        for (f, c) in std::mem::take(&mut g.out) {
            self.coll.extra.push((f, c));
        }
        self.callees.insert(eq.clone());
        self.callees.insert(dbg.clone());

        let fat = self.aggs.borrow_mut().stderr_fat(self.p)?;
        let same = self.emit(Ty::Bool, Op::Call { callee: eq, args: vec![a, b] });
        let (yes, _) = self.new_block(&[]);
        let (no, _) = self.new_block(&[]);
        self.branch(same, (yes, vec![]), (no, vec![]));
        // 失敗の枝: 文言を部分ごとに書き、終わる
        self.cur = Some(no);
        self.put_static(fat, "panic: assertion failed: left: ");
        self.call_debug(&dbg, a);
        self.put_static(fat, ", right: ");
        self.call_debug(&dbg, b);
        self.put_static(fat, "\n");
        self.emit(Ty::Unit, Op::Abort);
        self.terminate(Term::Unreachable);
        // 成功の枝: 一時の値 (所有物を持つ) は、比べ終えたので解放する
        self.cur = Some(yes);
        if temp_a {
            super::coll::drop_temp(self, a);
        }
        if temp_b {
            super::coll::drop_temp(self, b);
        }
        Ok(Some(self.emit(Ty::Unit, Op::Const(0))))
    }

    fn check_true(&mut self, e: &V) -> LR<Option<VId>> {
        let line = nline(e);
        let args = items(&nf(e)[1]);
        if args.len() != 1 {
            return self.err(line, "core.check.is_true の引数は 1 個のはず");
        }
        let Some(c) = self.expr(&args[0])? else { return Ok(None) };
        if self.vty(c) != Ty::Bool {
            return self.err(line, "core.check.is_true の引数は bool のはず");
        }
        let (yes, _) = self.new_block(&[]);
        let (no, _) = self.new_block(&[]);
        self.branch(c, (yes, vec![]), (no, vec![]));
        self.cur = Some(no);
        self.emit(Ty::Unit, Op::Panic { msg: "assertion failed: condition is false".to_string(), arg: None });
        self.terminate(Term::Unreachable);
        self.cur = Some(yes);
        Ok(Some(self.emit(Ty::Unit, Op::Const(0))))
    }

    /// 比較演算子 `==` / `!=` の被演算子 (値 `a` / `b`) が `Option` / `Result` / 集成体 / `&str` / `&[u8]` のとき、
    /// `<型>.$eq` を呼んで bool を返す (派生の比較。木走査の `v_eq` と同じ意味)。`neg` なら否定。
    pub(super) fn eq_by_call(&mut self, l: &V, r: &V, a: VId, b: VId, neg: bool, line: usize) -> LR<VId> {
        let ta = self.vty(a);
        // 所有物 (String / Vec) を持つ型は値で `$eq` に渡すと所有が移る。派生の比較は所有物のフィールドを許さず
        // (E2036)、手書きの eq は下ろせないので、理由を付けて拒否する (設計_KIR.md 17.15)。
        if super::coll::owns(self, ta) {
            return self.err(line, format!("型 {} の比較は段階 1 の対象外: 所有物を持つ型の == / != は値渡しで所有が移るため", ta.name()));
        }
        let mut why = String::new();
        let mut found = None;
        for e in [l, r] {
            let Ok(s) = self.src_ty(e) else { continue };
            let s = norm_src(&s);
            let file = self.def.file.clone();
            let mut g = Gen::new(self.p, self.aggs);
            match g.need_eq(&s, ta, &file) {
                Ok(k) => {
                    found = Some((k, std::mem::take(&mut g.out)));
                    break;
                }
                Err(w) => why = w,
            }
        }
        let Some((eq, out)) = found else {
            return self.err(line, format!("型 {} の比較は段階 1 の対象外: {}", ta.name(), why));
        };
        for (f, c) in out {
            self.coll.extra.push((f, c));
        }
        self.callees.insert(eq.clone());
        let same = self.emit(Ty::Bool, Op::Call { callee: eq, args: vec![a, b] });
        if neg {
            let one = self.emit(Ty::Bool, Op::Const(1));
            return Ok(self.emit(Ty::Bool, Op::Bool(crate::kir::BoolOp::Xor, same, one)));
        }
        Ok(same)
    }

    /// 文字列リテラルを stderr へ書く (`eprint`)。
    pub(super) fn put_static(&mut self, fat: Ty, s: &str) {
        let p = self.emit(Ty::Ptr, Op::StaticBytes(s.as_bytes().to_vec()));
        let n = self.emit(Ty::Usize, Op::Const(s.len() as i128));
        let f = self.emit(fat, Op::Make { variant: None, fields: vec![p, n] });
        self.emit(Ty::Unit, Op::Eprint { data: f });
    }

    /// 表示の関数 `dbg` を値 `v` に呼ぶ。
    /// `String.push_display(&x)` / `push_debug(&x)`: 値の型ごとの表示の関数 (`kir_debug` の `String` へ追記する版) を呼ぶ。
    /// `h` は `String` の先頭 (`{ptr, len, cap}`) の参照。結果は常に `Ok(())`。
    pub(super) fn string_push_fmt(&mut self, e: &V, line: usize, h: VId, args: &[V], debug: bool) -> LR<Option<VId>> {
        if args.len() != 1 {
            return self.err(line, "push_display / push_debug の引数は 1 つのはず");
        }
        let Some(mut a) = self.expr_peek(&args[0])? else { return Ok(None) };
        let mut ta = self.vty(a);
        let mut sa = self.src_ty(&args[0])?;
        let file = self.def.file.clone();
        // 参照越しのスカラーは参照先を読む (`core.check.eq` と同じ)。
        if matches!(ta, Ty::Ref(_, p) if !matches!(p, Pointee::Agg(_))) {
            if let Src::Ref(false, inner) = &sa {
                let t = match ta { Ty::Ref(_, p) => p.ty(), _ => unreachable!() };
                a = self.emit(t, Op::Load(a));
                sa = (**inner).clone();
                ta = t;
            }
        }
        // 集成体 (構造体・列挙型) への参照は、値を読んで渡す。
        if let Ty::Ref(_, Pointee::Agg(i)) = ta {
            ta = Ty::Agg(i);
            a = self.emit(ta, Op::Load(a));
            if let Src::Ref(_, inner) = &sa {
                sa = (**inner).clone();
            }
        }
        let rt = self.ty_of(e)?;
        if !matches!(rt, Ty::Res(Pay::Unit, _)) {
            return self.err(line, "push_display / push_debug の結果の型が Result<(), AllocError> でない");
        }
        let mut g = Gen::new_sink(self.p, self.aggs);
        let made = if debug { g.need(&sa, ta, &file) } else { g.need_display(&sa, ta, &file) };
        let k = match made {
            Ok(k) => k,
            Err(w) => return self.err(line, format!("push_{} を下ろせない: {}", if debug { "debug" } else { "display" }, w)),
        };
        for (f, c) in std::mem::take(&mut g.out) {
            self.coll.extra.push((f, c));
        }
        self.callees.insert(k.clone());
        self.emit(Ty::Unit, Op::Call { callee: k, args: vec![a, h] });
        let u = self.unit();
        Ok(Some(self.emit(rt, Op::Ok(u))))
    }

    /// 局所変数どうしの `String == String` / `!=`: ヘッダの参照を `<String>.$eq` に渡す (読むだけで移さない)。
    /// どちらかが局所変数の `String` でなければ `None` (従来の下ろしへ)。
    pub(super) fn string_eq_locals(&mut self, l: &V, r: &V, neg: bool) -> LR<Option<VId>> {
        let slot = |lw: &Lw, e: &V| super::coll::local_slot(lw, e).filter(|s| matches!(lw.coll.kinds.get(s), Some(super::coll::Kind::String)));
        let (Some(sl), Some(sr)) = (slot(self, l), slot(self, r)) else { return Ok(None) };
        let (hl, hr) = (self.slots[sl].0, self.slots[sr].0);
        let fat = self.aggs.borrow_mut().stderr_fat(self.p)?;
        let mut g = Gen::new(self.p, self.aggs);
        let src = Src::Named("alloc.string.String".to_string(), vec![]);
        let k = g.need_eq(&src, Ty::Ptr, &self.def.file.clone())?;
        let _ = fat;
        for (f, c) in std::mem::take(&mut g.out) {
            self.coll.extra.push((f, c));
        }
        self.callees.insert(k.clone());
        let same = self.emit(Ty::Bool, Op::Call { callee: k, args: vec![hl, hr] });
        if neg {
            return Ok(Some(self.emit(Ty::Bool, Op::Not(same))));
        }
        Ok(Some(same))
    }

    pub(super) fn call_debug(&mut self, dbg: &str, v: VId) {
        self.emit(Ty::Unit, Op::Call { callee: dbg.to_string(), args: vec![v] });
    }
}

/// 比較の型: スカラーへの参照 (`Option<&u8>` の `&u8`) は値で持つので、参照を外す。
fn norm_src(s: &crate::types::Ty) -> crate::types::Ty {
    use crate::types::Ty as S;
    match s {
        S::Ref(_, inner) if matches!(&**inner, S::Prim(p) if p != "str") => norm_src(inner),
        S::Ref(m, inner) => S::Ref(*m, Box::new(norm_src(inner))),
        S::Named(n, a) => S::Named(n.clone(), a.iter().map(norm_src).collect()),
        o => o.clone(),
    }
}
