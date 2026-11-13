//! `core.check.eq(a, b)` / `core.check.is_true(c)` の下ろし (設計_KIR.md 13.2)。`kir_lower.rs` の子モジュール。
//!
//! - `core.check.eq(a, b)`: 等しさは `<型>.$eq` (kir_debug.rs) を呼ぶ。違えば、stderr に
//!   `panic: assertion failed: left: <a の表示>, right: <b の表示>` を書いて終了コード 101 (木走査と同じ文言)。
//!   表示は `<型>.$debug` を呼んで `eprint` で部分ごとに書く。文言は動的なので `Op::Panic` (静的な文言) を使わない。
//! - `core.check.is_true(c)`: `c` が偽なら `panic: assertion failed: condition is false` (静的な文言の `Op::Panic`)。
//!
//! 両方とも、式として値 `()` を返す (成功の枝は続く)。引数の型が下ろせない・表示できないときは理由付きで止める。

use crate::check::{items, nf, nlineno as nline, nname, strs};
use crate::kir::{Cc, Op, Pointee, Term, Ty, VId};
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
        // 成功の枝
        self.cur = Some(yes);
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
