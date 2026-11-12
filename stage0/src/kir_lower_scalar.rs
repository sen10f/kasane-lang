//! 数値の下ろし (設計_KIR.md 10 章の拡張): 浮動小数、128 ビット整数、ビット演算、シフト、数値の変換、数値のメソッド。
//!
//! `kir_lower.rs` の子モジュール (`#[path]` で読み込む) なので、`Lw` の非公開の関数 (`emit`、`expr`、`ty_of` など) を使う。
//! 対象外の形は `None` を返し、呼び出し側は従来の処理 (理由付きの未対応) へ進む。
//! 命令は、対象だと決まった後にだけ置く。

use super::{Lw, LR};
use crate::check::{items, nf, nline, nname, st, strs};
use crate::kir::{self, Aop, BitK, BoolOp, FBin, FUn, Mode, Op, Ty, VId};
use crate::kir_scalar;
use crate::parser::V;

/// 数値のメソッドの下ろし方。
#[derive(Clone, Copy)]
enum Plan {
    /// 受け手と引数の `+ - *` (溢れの扱いは `Mode`)。
    Arith(Aop, Mode),
    /// `wrapping_neg`。
    Neg,
    /// `saturating_neg` (符号付き整数)。`0 - x` を飽和させる。
    SatNeg,
    /// `wrapping_shl` / `wrapping_shr` (引数は `u32`)。
    Shift(bool),
    /// 整数の `max` / `min`。
    MinMax(bool),
    /// `count_ones` などの結果は `u32`。
    BitCount(BitK),
    /// 数値の変換 (`to_f64`、`nearest_to_f32` など)。結果の型は呼び出しの型。
    Conv,
    /// 浮動小数の 1 引数 (`abs`、`sqrt`、`is_nan` など)。
    Unary(FUn),
    /// `to_bits` (ビット列の再解釈)。
    Bitcast,
    /// 浮動小数の 2 引数 (`min`、`max`、`rem`、`atan2`、`powi`)。
    Binary(FBin),
}

/// 数値のメソッドの対応表。対象外なら `None` (引数の個数は一致している必要がある)。
fn num_plan(rt: Ty, name: &str, n: usize) -> Option<Plan> {
    let int = rt.is_int();
    let flt = rt.is_float();
    Some(match (name, n) {
        ("wrapping_add", 1) if int => Plan::Arith(Aop::Add, Mode::Wrap),
        ("wrapping_sub", 1) if int => Plan::Arith(Aop::Sub, Mode::Wrap),
        ("wrapping_mul", 1) if int => Plan::Arith(Aop::Mul, Mode::Wrap),
        ("saturating_add", 1) if int => Plan::Arith(Aop::Add, Mode::Sat),
        ("saturating_sub", 1) if int => Plan::Arith(Aop::Sub, Mode::Sat),
        ("saturating_mul", 1) if int => Plan::Arith(Aop::Mul, Mode::Sat),
        ("wrapping_neg", 0) if int => Plan::Neg,
        ("saturating_neg", 0) if int && rt.signed() => Plan::SatNeg,
        ("wrapping_shl", 1) if int => Plan::Shift(true),
        ("wrapping_shr", 1) if int => Plan::Shift(false),
        ("max", 1) if int => Plan::MinMax(true),
        ("min", 1) if int => Plan::MinMax(false),
        ("count_ones", 0) if int => Plan::BitCount(BitK::Ones),
        ("leading_zeros", 0) if int => Plan::BitCount(BitK::Leading),
        ("trailing_zeros", 0) if int => Plan::BitCount(BitK::Trailing),
        ("to_f64" | "nearest_to_f64", 0) if int => Plan::Conv,
        ("to_f64", 0) if rt == Ty::F32 => Plan::Conv,
        ("nearest_to_f32", 0) if rt == Ty::F64 => Plan::Conv,
        ("abs", 0) if flt => Plan::Unary(FUn::Abs),
        ("sqrt", 0) if flt => Plan::Unary(FUn::Sqrt),
        ("floor", 0) if flt => Plan::Unary(FUn::Floor),
        ("ceil", 0) if flt => Plan::Unary(FUn::Ceil),
        ("trunc", 0) if flt => Plan::Unary(FUn::Trunc),
        ("round_ties_even", 0) if flt => Plan::Unary(FUn::RoundEven),
        ("sin", 0) if flt => Plan::Unary(FUn::Sin),
        ("cos", 0) if flt => Plan::Unary(FUn::Cos),
        ("tan", 0) if flt => Plan::Unary(FUn::Tan),
        ("exp", 0) if flt => Plan::Unary(FUn::Exp),
        ("ln", 0) if flt => Plan::Unary(FUn::Ln),
        ("is_nan", 0) if flt => Plan::Unary(FUn::IsNan),
        ("is_finite", 0) if flt => Plan::Unary(FUn::IsFinite),
        ("to_bits", 0) if flt => Plan::Bitcast,
        ("min", 1) if flt => Plan::Binary(FBin::Min),
        ("max", 1) if flt => Plan::Binary(FBin::Max),
        ("rem", 1) if flt => Plan::Binary(FBin::Rem),
        ("atan2", 1) if flt => Plan::Binary(FBin::Atan2),
        ("powi", 1) if flt => Plan::Binary(FBin::Powi),
        _ => return None,
    })
}

impl<'a> Lw<'a> {
    /// 値 `v` の型が `t` であることを確かめる (違えば理由付きで止める)。
    fn expect_ty(&self, v: VId, t: Ty, line: usize) -> LR<()> {
        if self.vty(v) == t {
            Ok(())
        } else {
            self.err(line, format!("値の型 {} が想定の {} と違う", self.vty(v).name(), t.name()))
        }
    }

    /// 浮動小数の定数 (リテラル)。`f32` は `f64` を経由して丸める (木走査と同じ)。
    pub(super) fn float_lit(&mut self, e: &V) -> LR<VId> {
        let f = nf(e);
        let line = nline(e);
        let ty = self.ty_of(e)?;
        if !ty.is_float() {
            return self.err(line, "浮動小数リテラルの型が浮動小数ではない");
        }
        let x = st(&f[2]).parse::<f64>().unwrap_or(0.0);
        Ok(self.emit(ty, Op::Const(kir_scalar::from_f64(ty, x))))
    }

    /// 二項演算子のうち、整数のビット演算 (`& | ^`) と浮動小数の `+ - * /`。`None` は対象外。
    pub(super) fn scalar_bin(&mut self, e: &V) -> LR<Option<Option<VId>>> {
        let f = nf(e);
        let line = nline(e);
        let op = st(&f[0]);
        let Ok(t) = self.ty_of(e) else { return Ok(None) };
        let bit = match op.as_str() {
            "&" => Some(BoolOp::And),
            "|" => Some(BoolOp::Or),
            "^" => Some(BoolOp::Xor),
            _ => None,
        };
        if let Some(bop) = bit.filter(|_| t.is_int()) {
            let Some(a) = self.expr(&f[1])? else { return Ok(Some(None)) };
            let Some(b) = self.expr(&f[2])? else { return Ok(Some(None)) };
            self.expect_ty(a, t, line)?;
            self.expect_ty(b, t, line)?;
            return Ok(Some(Some(self.emit(t, Op::Bool(bop, a, b)))));
        }
        let arith = match op.as_str() {
            "+" => Some(Aop::Add),
            "-" => Some(Aop::Sub),
            "*" => Some(Aop::Mul),
            "/" => Some(Aop::Div),
            _ => None,
        };
        if let Some(aop) = arith.filter(|_| t.is_float()) {
            let Some(a) = self.expr(&f[1])? else { return Ok(Some(None)) };
            let Some(b) = self.expr(&f[2])? else { return Ok(Some(None)) };
            self.expect_ty(a, t, line)?;
            self.expect_ty(b, t, line)?;
            return Ok(Some(Some(self.emit(t, Op::FArith { op: aop, a, b }))));
        }
        Ok(None)
    }

    /// 単項演算子のうち、浮動小数の `-` と整数の `!` (ビット反転)。`None` は対象外。
    pub(super) fn scalar_unary(&mut self, e: &V) -> LR<Option<Option<VId>>> {
        let f = nf(e);
        let line = nline(e);
        let op = st(&f[0]);
        let Ok(t) = self.ty_of(e) else { return Ok(None) };
        match op.as_str() {
            "-" if t.is_float() => {
                let Some(a) = self.expr(&f[1])? else { return Ok(Some(None)) };
                self.expect_ty(a, t, line)?;
                Ok(Some(Some(self.emit(t, Op::FUn { f: FUn::Neg, src: t, a }))))
            }
            "!" if t.is_int() => {
                let Some(a) = self.expr(&f[1])? else { return Ok(Some(None)) };
                self.expect_ty(a, t, line)?;
                Ok(Some(Some(self.emit(t, Op::Not(a)))))
            }
            _ => Ok(None),
        }
    }

    /// `recv.name(args)` の数値のメソッド。受け手の値 `recv` は型 `rt`、呼び出しの結果の型は `res`。
    /// 対象外の名前や個数は、命令を置く前に `None` を返す。
    fn num_method(&mut self, line: usize, recv: VId, rt: Ty, res: Ty, name: &str, args: &[V]) -> LR<Option<Option<VId>>> {
        let Some(plan) = num_plan(rt, name, args.len()) else { return Ok(None) };
        let out = match plan {
            Plan::BitCount(_) => Ty::U32,
            Plan::Unary(FUn::IsNan | FUn::IsFinite) => Ty::Bool,
            Plan::Conv | Plan::Bitcast => res,
            _ => rt,
        };
        if res != out {
            return self.err(line, format!("{} の結果の型は {} のはず (実際は {})", name, out.name(), res.name()));
        }
        let want = match plan {
            Plan::Shift(_) => Ty::U32,
            Plan::Binary(FBin::Powi) => Ty::I32,
            _ => rt,
        };
        let mut argv = vec![];
        for a in args {
            let Some(v) = self.expr(a)? else { return Ok(Some(None)) };
            self.expect_ty(v, want, line)?;
            argv.push(v);
        }
        let v = match plan {
            Plan::Arith(op, mode) => self.emit(rt, Op::Arith { op, mode, a: recv, b: argv[0] }),
            Plan::Neg => self.emit(rt, Op::Neg { a: recv, mode: Mode::Wrap }),
            Plan::SatNeg => {
                let zero = self.emit(rt, Op::Const(0));
                self.emit(rt, Op::Arith { op: Aop::Sub, mode: Mode::Sat, a: zero, b: recv })
            }
            Plan::Shift(left) => self.emit(rt, Op::Shift { left, a: recv, n: argv[0] }),
            Plan::MinMax(max) => self.emit(rt, Op::MinMax { max, a: recv, b: argv[0] }),
            Plan::BitCount(k) => self.emit(Ty::U32, Op::BitCount { k, src: rt, a: recv }),
            Plan::Conv => {
                if !kir::conv_ok(rt, res) {
                    return self.err(line, format!("{} から {} への変換は段階 1 の対象外", rt.name(), res.name()));
                }
                self.emit(res, Op::Conv { src: rt, a: recv })
            }
            Plan::Unary(f) => self.emit(out, Op::FUn { f, src: rt, a: recv }),
            Plan::Bitcast => self.emit(res, Op::Bitcast { src: rt, a: recv }),
            Plan::Binary(f) => self.emit(rt, Op::FBin { f, a: recv, b: argv[0] }),
        };
        Ok(Some(Some(v)))
    }

    /// `e.name(args)` (受け手が式の形) の数値のメソッド。
    pub(super) fn scalar_mcall(&mut self, e: &V) -> LR<Option<Option<VId>>> {
        let f = nf(e);
        let line = nline(e);
        if !matches!(f.get(3), Some(V::Nil) | None) {
            return Ok(None);
        }
        let name = st(&f[1]);
        let args = items(&f[2]);
        let Ok(rt) = self.ty_of(&f[0]) else { return Ok(None) };
        if num_plan(rt, &name, args.len()).is_none() {
            return Ok(None);
        }
        let Ok(res) = self.ty_of(e) else { return Ok(None) };
        let Some(recv) = self.expr(&f[0])? else { return Ok(Some(None)) };
        self.num_method(line, recv, rt, res, &name, args)
    }

    /// `x.name(args)` (局所変数の受け手) と `T.from_X(v)` (数値の型の関連関数) の数値の演算。
    pub(super) fn scalar_call(&mut self, e: &V) -> LR<Option<Option<VId>>> {
        let f = nf(e);
        let line = nline(e);
        if !matches!(f.get(2), Some(V::Nil) | None) || nname(&f[0]) != "Path" {
            return Ok(None);
        }
        let segs = strs(&nf(&f[0])[0]);
        if segs.len() != 2 {
            return Ok(None);
        }
        let args = items(&f[1]);
        let Ok(res) = self.ty_of(e) else { return Ok(None) };
        if let Some(s) = self.lookup(&segs[0]) {
            let (recv, rt) = self.slots[s];
            return self.num_method(line, recv, rt, res, &segs[1], args);
        }
        let Some(dst) = Ty::parse(&segs[0]) else { return Ok(None) };
        let Some(rest) = segs[1].strip_prefix("from_") else { return Ok(None) };
        if args.len() != 1 || !(dst.is_int() || dst.is_float()) {
            return Ok(None);
        }
        if rest == "bits" {
            if !dst.is_float() {
                return Ok(None);
            }
            let src = if dst == Ty::F64 { Ty::U64 } else { Ty::U32 };
            let Some(a) = self.expr(&args[0])? else { return Ok(Some(None)) };
            self.expect_ty(a, src, line)?;
            return Ok(Some(Some(self.emit(dst, Op::Bitcast { src, a }))));
        }
        // `from_<int>` は整数どうし (値を保つ)、`from_<int>_nearest` は浮動小数へ。
        let (name, nearest) = match rest.strip_suffix("_nearest") {
            Some(n) => (n, true),
            None => (rest, false),
        };
        let Some(src) = Ty::parse(name).filter(|t| t.is_int()) else { return Ok(None) };
        if nearest != dst.is_float() {
            return Ok(None);
        }
        if !kir::conv_ok(src, dst) {
            return self.err(line, format!("{} から {} への変換は段階 1 の対象外", src.name(), dst.name()));
        }
        let Some(a) = self.expr(&args[0])? else { return Ok(Some(None)) };
        self.expect_ty(a, src, line)?;
        Ok(Some(Some(self.emit(dst, Op::Conv { src, a }))))
    }
}
