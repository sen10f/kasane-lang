//! `core.check.eq(a, b)` / `core.check.is_true(c)` の下ろし (設計_KIR.md 13.2)。`kir_lower.rs` の子モジュール。
//!
//! - `core.check.eq(a, b)`: 等しさは `<型>.$eq` (kir_debug.rs) を呼ぶ。違えば、stderr に
//!   `panic: assertion failed: left: <a の表示>, right: <b の表示>` を書いて終了コード 101 (木走査と同じ文言)。
//!   表示は `<型>.$debug` を呼んで `eprint` で部分ごとに書く。文言は動的なので `Op::Panic` (静的な文言) を使わない。
//! - `core.check.is_true(c)`: `c` が偽なら `panic: assertion failed: condition is false` (静的な文言の `Op::Panic`)。
//!
//! 両方とも、式として値 `()` を返す (成功の枝は続く)。引数の型が下ろせない・表示できないときは理由付きで止める。

use crate::check::{items, nf, nlineno as nline, nname, strs};
use crate::kir::{Cc, Op, Term, Ty, VId};
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
        let Some(a) = self.expr(&args[0])? else { return Ok(None) };
        let Some(b) = self.expr(&args[1])? else { return Ok(None) };
        let (ta, tb) = (self.vty(a), self.vty(b));
        if ta != tb {
            return self.err(line, "core.check.eq の両辺の型が違う");
        }
        let sa = self.src_ty(&args[0])?;
        let file = self.def.file.clone();
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
        // 成功の枝
        self.cur = Some(yes);
        Ok(Some(self.emit(Ty::Unit, Op::Const(0))))
    }

    /// 集成体型 `ty` (元の型 `src`) の等しさ関数 `<型>.$eq` を (まだなければ) 生成し、その鍵を返す。
    /// `bin()` の `==`/`!=` で集成体を比べるために使う。
    pub(super) fn agg_eq_key(&mut self, src: &Src, ty: Ty, file: &str) -> Result<String, String> {
        let mut g = Gen::new(self.p, self.aggs);
        let eq = g.need_eq(src, ty, file)?;
        for (f, c) in std::mem::take(&mut g.out) {
            self.coll.extra.push((f, c));
        }
        self.callees.insert(eq.clone());
        Ok(eq)
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

    /// 文字列リテラルを stderr へ書く (`eprint`)。
    fn put_static(&mut self, fat: Ty, s: &str) {
        let p = self.emit(Ty::Ptr, Op::StaticBytes(s.as_bytes().to_vec()));
        let n = self.emit(Ty::Usize, Op::Const(s.len() as i128));
        let f = self.emit(fat, Op::Make { variant: None, fields: vec![p, n] });
        self.emit(Ty::Unit, Op::Eprint { data: f });
    }

    /// 表示の関数 `dbg` を値 `v` に呼ぶ。
    fn call_debug(&mut self, dbg: &str, v: VId) {
        self.emit(Ty::Unit, Op::Call { callee: dbg.to_string(), args: vec![v] });
    }
}
