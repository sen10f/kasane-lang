//! 数値の下ろし (設計_KIR.md 10 章の拡張): 浮動小数、128 ビット整数、ビット演算、シフト、数値の変換、数値のメソッド。
//!
//! `kir_lower.rs` の子モジュール (`#[path]` で読み込む) なので、`Lw` の非公開の関数 (`emit`、`expr`、`ty_of` など) を使う。
//! 対象外の形は `None` を返し、呼び出し側は従来の処理 (理由付きの未対応) へ進む。
//! 命令は、対象だと決まった後にだけ置く。

use super::{Lw, LR};
use crate::check::{items, nf, nlineno as nline, nname, st, strs};
use crate::kir::{self, Aop, BitK, BoolOp, Cc, FBin, FUn, Mode, Op, Pay, Ty, VId};
use crate::kir_scalar;
use crate::parser::V;
use crate::types::Ty as Src;

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
    /// `char.to_u32()`: 値はそのまま (`char` は `u32` で持つ)。
    Same,
    /// `char.to_ascii_uppercase()` / `to_ascii_lowercase()` (`true` で大文字)。ASCII の英字だけを変える。
    AsciiCase(bool),
    /// `char.is_ascii_alphabetic()` (0) / `is_ascii_digit()` (1) / `is_ascii_whitespace()` (2)。結果は `bool`。
    CharIs(u8),
    /// `char.len_utf8()`。結果は `usize`。
    LenUtf8,
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
        // `char` は `u32` で持つ。spec が `char` にだけ定めるメソッドなので、`u32` の受け手では現れない。
        ("to_u32", 0) if rt == Ty::U32 => Plan::Same,
        ("to_ascii_uppercase", 0) if rt == Ty::U32 => Plan::AsciiCase(true),
        ("to_ascii_lowercase", 0) if rt == Ty::U32 => Plan::AsciiCase(false),
        ("is_ascii_alphabetic", 0) if rt == Ty::U32 => Plan::CharIs(0),
        ("is_ascii_digit", 0) if rt == Ty::U32 => Plan::CharIs(1),
        ("is_ascii_whitespace", 0) if rt == Ty::U32 => Plan::CharIs(2),
        ("len_utf8", 0) if rt == Ty::U32 => Plan::LenUtf8,
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
    /// 評価済みの数値の受け手 `recv` (参照は外してある) の数値メソッド。数値の計画に無ければ `None`。
    pub(super) fn num_call_on(&mut self, e: &V, recv: VId, rt: Ty, name: &str, args: &[V]) -> LR<Option<Option<VId>>> {
        if num_plan(rt, name, args.len()).is_none() {
            return Ok(None);
        }
        let Ok(res) = self.ty_of(e) else { return Ok(None) };
        self.num_method(nline(e), recv, rt, res, name, args)
    }

    fn num_method(&mut self, line: usize, recv: VId, rt: Ty, res: Ty, name: &str, args: &[V]) -> LR<Option<Option<VId>>> {
        let Some(plan) = num_plan(rt, name, args.len()) else { return Ok(None) };
        let out = match plan {
            Plan::BitCount(_) => Ty::U32,
            Plan::Unary(FUn::IsNan | FUn::IsFinite) | Plan::CharIs(_) => Ty::Bool,
            Plan::LenUtf8 => Ty::Usize,
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
            Plan::Same => recv,
            Plan::AsciiCase(upper) => self.ascii_case(recv, upper),
            Plan::CharIs(k) => self.char_is(recv, k),
            Plan::LenUtf8 => {
                // 1 / 2 / 3 / 4 バイト (0x80・0x800・0x10000 を境にする)。
                let (join, jp) = self.new_block(&[Ty::Usize]);
                for (b, n) in [(0x80i128, 1i128), (0x800, 2), (0x10000, 3)] {
                    let bv = self.emit(Ty::U32, Op::Const(b));
                    let lt = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::U32, a: recv, b: bv });
                    let (yes, _) = self.new_block(&[]);
                    let (next, _) = self.new_block(&[]);
                    self.branch(lt, (yes, vec![]), (next, vec![]));
                    self.cur = Some(yes);
                    let nv = self.emit(Ty::Usize, Op::Const(n));
                    self.goto(join, vec![nv]);
                    self.cur = Some(next);
                }
                let four = self.emit(Ty::Usize, Op::Const(4));
                self.goto(join, vec![four]);
                self.cur = Some(join);
                jp[0]
            }
        };
        Ok(Some(Some(v)))
    }

    /// ASCII の分類 (`char` は `u32`)。0 = 英字、1 = 数字、2 = 空白 (`\t \n \x0C \r` と空白。0x0B は含まない)。
    fn char_is(&mut self, c: VId, k: u8) -> VId {
        let rng = |lw: &mut Self, lo: i128, hi: i128| {
            let l = lw.emit(Ty::U32, Op::Const(lo));
            let h = lw.emit(Ty::U32, Op::Const(hi));
            let ge = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::U32, a: c, b: l });
            let le = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::U32, a: c, b: h });
            lw.emit(Ty::Bool, Op::Bool(BoolOp::And, ge, le))
        };
        let or = |lw: &mut Self, a: VId, b: VId| lw.emit(Ty::Bool, Op::Bool(BoolOp::Or, a, b));
        match k {
            0 => {
                let u = rng(self, 65, 90);
                let l = rng(self, 97, 122);
                or(self, u, l)
            }
            1 => rng(self, 48, 57),
            _ => {
                let a = rng(self, 9, 10);
                let b = rng(self, 12, 13);
                let c2 = rng(self, 32, 32);
                let ab = or(self, a, b);
                or(self, ab, c2)
            }
        }
    }

    /// ASCII の英字の大文字・小文字 (`char` は `u32` で持つ)。英字以外はそのまま返す。
    pub(super) fn ascii_case(&mut self, c: VId, upper: bool) -> VId {
        let (lo, hi) = if upper { (97i128, 122i128) } else { (65, 90) };
        let lo_v = self.emit(Ty::U32, Op::Const(lo));
        let hi_v = self.emit(Ty::U32, Op::Const(hi));
        let ge = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: Ty::U32, a: c, b: lo_v });
        let le = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::U32, a: c, b: hi_v });
        let both = self.emit(Ty::Bool, Op::Bool(BoolOp::And, ge, le));
        let (yes, _) = self.new_block(&[]);
        let (no, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[Ty::U32]);
        self.branch(both, (yes, vec![]), (no, vec![]));
        self.cur = Some(yes);
        let d = self.emit(Ty::U32, Op::Const(32));
        let op = if upper { Aop::Sub } else { Aop::Add };
        let moved = self.emit(Ty::U32, Op::Arith { op, mode: Mode::Wrap, a: c, b: d });
        self.goto(join, vec![moved]);
        self.cur = Some(no);
        self.goto(join, vec![c]);
        self.cur = Some(join);
        jp[0]
    }

    /// `e.name(args)` (受け手が式の形) の数値のメソッド。
    /// `a.cmp(b)` (整数・`char`。値か、その参照): `core.cmp.Ordering` の `Less` / `Equal` / `Greater`。
    pub(super) fn int_cmp(&mut self, a: VId, b: VId, line: usize) -> LR<Option<VId>> {
        self.int_cmp_named("cmp", a, b, line)
    }

    /// `a.eq(b)` / `a.cmp(b)`。
    pub(super) fn int_cmp_named(&mut self, name: &str, a: VId, b: VId, line: usize) -> LR<Option<VId>> {
        let load = |lw: &mut Self, v: VId| match lw.vty(v) {
            Ty::Ref(_, p) => lw.emit(p.ty(), Op::Load(v)),
            _ => v,
        };
        let (a, b) = (load(self, a), load(self, b));
        let t = self.vty(a);
        if name == "try_clone" && t.is_int() {
            let ae = self.aggs.borrow_mut().alloc_err();
            let rt = Ty::Res(Pay::of(t).expect("整数はペイロードになる"), Pay::of(ae).expect("集成体はペイロードになる"));
            return Ok(Some(self.emit(rt, Op::Ok(a))));
        }
        if !t.is_int() || self.vty(b) != t {
            return self.err(line, "cmp の受け手と引数が同じ整数の型でない");
        }
        if name == "eq" {
            return Ok(Some(self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: t, a, b })));
        }
        if name == "try_clone" {
            let ae = self.aggs.borrow_mut().alloc_err();
            let rt = Ty::Res(Pay::of(t).expect("整数はペイロードになる"), Pay::of(ae).expect("集成体はペイロードになる"));
            return Ok(Some(self.emit(rt, Op::Ok(a))));
        }
        let lt = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: t, a, b });
        let eq = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: t, a, b });
        let (lb, _) = self.new_block(&[]);
        let (nl, _) = self.new_block(&[]);
        let (eb, _) = self.new_block(&[]);
        let (gb, _) = self.new_block(&[]);
        let ord = match self.conv_t(&crate::types::Ty::Named("core.cmp.Ordering".into(), vec![], crate::types::Rgs::default())) {
            Some(o) => o,
            None => return self.err(line, "core.cmp.Ordering が下ろせない"),
        };
        let (join, jp) = self.new_block(&[ord]);
        self.branch(lt, (lb, vec![]), (nl, vec![]));
        self.cur = Some(lb);
        let less = self.variant_value("core.cmp.Ordering", "Less", None, line)?;
        self.goto(join, vec![less]);
        self.cur = Some(nl);
        self.branch(eq, (eb, vec![]), (gb, vec![]));
        self.cur = Some(eb);
        let equal = self.variant_value("core.cmp.Ordering", "Equal", None, line)?;
        self.goto(join, vec![equal]);
        self.cur = Some(gb);
        let greater = self.variant_value("core.cmp.Ordering", "Greater", None, line)?;
        self.goto(join, vec![greater]);
        self.cur = Some(join);
        Ok(Some(jp[0]))
    }

    /// `x.hash(state)` (整数): 値を符号なし 64 ビットに広げ、型幅のバイト数を `core.hash.fnv_int` へ渡す。
    pub(super) fn int_hash(&mut self, a: VId, state: VId, line: usize) -> LR<VId> {
        let a = match self.vty(a) {
            Ty::Ref(_, p) => self.emit(p.ty(), Op::Load(a)),
            _ => a,
        };
        let t = self.vty(a);
        let w = match t {
            Ty::U8 | Ty::I8 => 1,
            Ty::U16 | Ty::I16 => 2,
            Ty::U32 | Ty::I32 => 4,
            Ty::U64 | Ty::I64 | Ty::Usize | Ty::Isize => 8,
            _ => return self.err(line, "hash の受け手の整数の型が段階 1 の対象外"),
        };
        let x = if t == Ty::U64 { a } else { self.emit(Ty::U64, Op::Conv { src: t, a }) };
        let wv = self.emit(Ty::Usize, Op::Const(w));
        let callee = "core.hash.fnv_int".to_string();
        self.callees.insert(callee.clone());
        Ok(self.emit(Ty::Unit, Op::Call { callee, args: vec![state, x, wv] }))
    }

    pub(super) fn scalar_mcall(&mut self, e: &V) -> LR<Option<Option<VId>>> {
        let f = nf(e);
        let line = nline(e);
        if !matches!(f.get(3), Some(V::Nil) | None) {
            return Ok(None);
        }
        let name = st(&f[1]);
        let args = items(&f[2]);
        let Ok(rt) = self.ty_of(&f[0]) else { return Ok(None) };
        if name == "try_clone" && args.is_empty() && deref_int(rt).is_some() {
            let Some(a) = self.expr(&f[0])? else { return Ok(Some(None)) };
            return Ok(Some(self.int_cmp_named("try_clone", a, a, line)?));
        }
        if matches!(name.as_str(), "cmp" | "eq") && args.len() == 1 && deref_int(rt).is_some() {
            let Some(a) = self.expr(&f[0])? else { return Ok(Some(None)) };
            let Some(b) = self.expr(&args[0])? else { return Ok(Some(None)) };
            return Ok(Some(self.int_cmp_named(&name, a, b, line)?));
        }
        // 整数の `Hash` (仕様 4.2): `Fnv64` の状態へ値のリトルエンディアンのバイトを足す (`core.hash.fnv_int`、`hash.ksnl`)。
        if name == "hash" && args.len() == 1 && deref_int(rt).is_some() {
            let Some(a) = self.expr(&f[0])? else { return Ok(Some(None)) };
            let Some(state) = self.expr(&args[0])? else { return Ok(Some(None)) };
            return Ok(Some(Some(self.int_hash(a, state, line)?)));
        }
        let (rt, by_ref) = match rt {
            Ty::Ref(_, p) if p.ty().is_int() || p.ty().is_float() => (p.ty(), true),
            t => (t, false),
        };
        if num_plan(rt, &name, args.len()).is_none() {
            return Ok(None);
        }
        let Ok(res) = self.ty_of(e) else { return Ok(None) };
        let Some(mut recv) = self.expr(&f[0])? else { return Ok(Some(None)) };
        if by_ref {
            recv = self.emit(rt, Op::Load(recv));
        }
        self.num_method(line, recv, rt, res, &name, args)
    }

    /// `x.name(args)` (局所変数の受け手) と `T.from_X(v)` (数値の型の関連関数) の数値の演算。
    pub(super) fn scalar_call(&mut self, e: &V) -> LR<Option<Option<VId>>> {
        let f = nf(e);
        let line = nline(e);
        if !matches!(f.get(2), Some(V::Nil) | None) || nname(&f[0]) != "Path" {
            return Ok(None);
        }
        let mut segs = strs(&nf(&f[0])[0]);
        let args = items(&f[1]);
        // 完全修飾の形 `core.num.f64.sin(x)` (受け手が第 1 引数) と `core.num.i64.from_..(v)` / `core.char.char.try_from_u32(x)` (関連関数)。
        if segs.len() == 4 && segs[0] == "core" && ((segs[1] == "num" && Ty::parse(&segs[2]).is_some()) || (segs[1] == "char" && segs[2] == "char" && segs[3] == "try_from_u32")) {
            let (ty, name) = (if segs[1] == "char" { "char".to_string() } else { segs[2].clone() }, segs[3].clone());
            let assoc = name.starts_with("from_") || name.starts_with("try_from") || name == "parse";
            if assoc {
                segs = vec![ty, name];
            } else if let Some((first, rest)) = args.split_first() {
                let Some(rt) = Ty::parse(&ty) else { return Ok(None) };
                let Ok(res) = self.ty_of(e) else { return Ok(None) };
                if num_plan(rt, &name, rest.len()).is_none() {
                    return Ok(None);
                }
                let Some(recv) = self.expr(first)? else { return Ok(Some(None)) };
                if self.vty(recv) != rt {
                    return self.err(line, "完全修飾の数値メソッドの受け手の型が違う");
                }
                return self.num_method(line, recv, rt, res, &name, rest);
            }
        }
        if segs.len() != 2 {
            return Ok(None);
        }
        let Ok(res) = self.ty_of(e) else { return Ok(None) };
        if let Some(s) = self.lookup(&segs[0]) {
            // 参照を取られた局所 (スタックスロット) は、読み出しの `load` を通す (`read_slot`)。
            let rt = self.slots[s].1;
            if segs[1] == "try_clone" && args.is_empty() && deref_int(rt).is_some() {
                let a = self.read_slot(s);
                return Ok(Some(self.int_cmp_named("try_clone", a, a, line)?));
            }
            if matches!(segs[1].as_str(), "cmp" | "eq") && args.len() == 1 && deref_int(rt).is_some() {
                let a = self.read_slot(s);
                let Some(b) = self.expr(&args[0])? else { return Ok(Some(None)) };
                return Ok(Some(self.int_cmp_named(&segs[1], a, b, line)?));
            }
            let (rt, by_ref) = match rt {
                Ty::Ref(_, p) if p.ty().is_int() || p.ty().is_float() => (p.ty(), true),
                t => (t, false),
            };
            if num_plan(rt, &segs[1], args.len()).is_none() {
                return Ok(None);
            }
            let mut recv = self.read_slot(s);
            if by_ref {
                recv = self.emit(rt, Op::Load(recv));
            }
            return self.num_method(line, recv, rt, res, &segs[1], args);
        }
        // `char.try_from_u32(x)`: 有効な文字 (0x110000 未満で、サロゲート 0xD800..=0xDFFF の外) なら `Some(x)`、そうでなければ `None`。
        if segs[0] == "char" && segs[1] == "try_from_u32" && args.len() == 1 {
            let Ok(res) = self.ty_of(e) else { return Ok(None) };
            let Ty::Opt(Pay::U32) = res else {
                return self.err(line, "char.try_from_u32 の結果が Option<char> ではない");
            };
            let Some(x) = self.expr(&args[0])? else { return Ok(Some(None)) };
            self.expect_ty(x, Ty::U32, line)?;
            let lim = self.emit(Ty::U32, Op::Const(0x110000));
            let sur_lo = self.emit(Ty::U32, Op::Const(0xD800));
            let sur_hi = self.emit(Ty::U32, Op::Const(0xDFFF));
            let below = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::U32, a: x, b: lim });
            let under = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::U32, a: x, b: sur_lo });
            let over = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Gt, ty: Ty::U32, a: x, b: sur_hi });
            let outside = self.emit(Ty::Bool, Op::Bool(BoolOp::Or, under, over));
            let ok = self.emit(Ty::Bool, Op::Bool(BoolOp::And, below, outside));
            let (yes, _) = self.new_block(&[]);
            let (no, _) = self.new_block(&[]);
            let (join, jp) = self.new_block(&[res]);
            self.branch(ok, (yes, vec![]), (no, vec![]));
            self.cur = Some(yes);
            let some = self.emit(res, Op::Some(x));
            self.goto(join, vec![some]);
            self.cur = Some(no);
            let none = self.emit(res, Op::Const(0));
            self.goto(join, vec![none]);
            self.cur = Some(join);
            return Ok(Some(Some(jp[0])));
        }
        let Some(dst) = Ty::parse(&segs[0]) else { return Ok(None) };
        // `u8.from_str_radix(text, radix)` など。`&str` をバイトごとに読み、checked な乗算・加算で解析する。
        // `i64.parse(text)` は基数 10 の `from_str_radix` と同じ (木走査 natives.rs も同じ経路)。
        let is_parse = segs[1] == "parse" && args.len() == 1 && dst.is_int();
        if (segs[1] == "from_str_radix" && args.len() == 2 && dst.is_int()) || is_parse {
            let Ok(res) = self.ty_of(e) else { return Ok(None) };
            let Ty::Res(t0, _) = res else {
                return self.err(line, format!("{} の結果が Result ではない", segs[1]));
            };
            let int_ty = t0.ty();
            if !int_ty.is_int() {
                return self.err(line, format!("{} の結果の整数型が不正", segs[1]));
            }
            let Some(s) = self.expr(&args[0])? else { return Ok(Some(None)) };
            let radix = if is_parse {
                self.emit(Ty::U32, Op::Const(10))
            } else {
                let Some(r) = self.expr(&args[1])? else { return Ok(Some(None)) };
                r
            };
            let fat = self.fat_ty();
            if self.vty(s) != fat {
                return self.err(line, format!("{} の第 1 引数は &str ではない", segs[1]));
            }
            if self.vty(radix) != Ty::U32 {
                return self.err(line, format!("{} の基数は u32 ではない", segs[1]));
            }
            return Ok(Some(Some(self.emit(res, Op::ParseInt { ty: int_ty, s, radix }))));
        }
        // `<dst>.try_from_<src>(x)`: x が dst の範囲に入るなら Ok(写し)、範囲外なら
        // Err(core.num.TryFromIntError { kind: 0u8 })。`try_from_*` は非可逆の幅変換なので、
        // `Op::Conv` で写す組だけを扱う (i128/u128 は Result のペイロードに入らない)。
        if let Some(rest) = segs[1].strip_prefix("try_from_") {
            if args.len() == 1 && dst.is_int() {
                let Some(src) = Ty::parse(rest) else { return Ok(None) };
                if !src.is_int() {
                    return Ok(None);
                }
                let Ok(res) = self.ty_of(e) else { return Ok(None) };
                let Ty::Res(t0, _) = res else {
                    return self.err(line, "try_from_* の結果が Result ではない");
                };
                if t0.ty() != dst {
                    return self.err(line, "try_from_* の結果の型が宛先と違う");
                }
                if !kir::conv_ok(src, dst) {
                    return Ok(None);
                }
                let Some(a) = self.expr(&args[0])? else { return Ok(Some(None)) };
                self.expect_ty(a, src, line)?;
                let Some(err_ty) = self.conv_t(&Src::Named("core.num.TryFromIntError".into(), vec![], crate::types::Rgs::default())) else {
                    return self.err(line, "TryFromIntError の型が読めない");
                };
                let Ty::Agg(_) = err_ty else {
                    return self.err(line, "TryFromIntError の型が集成体でない");
                };
                let rt = Ty::Res(Pay::of(dst).expect("整数はペイロードになる"),
                    Pay::of(err_ty).expect("エラー型はペイロードになる"));
                // `try_from_*` は非可逆なので `src` の範囲内にクランプした dst の境界を
                // `src` 型で比べる (u128/i128 は conv_ok で既に落とす)。
                let lo_v = dst.min().max(src.min());
                let hi_v = dst.max().min(src.max());
                let lo = self.emit(src, Op::Const(lo_v));
                let hi = self.emit(src, Op::Const(hi_v));
                let c_lo = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: src, a, b: lo });
                let c_hi = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: src, a, b: hi });
                let in_range = self.emit(Ty::Bool, Op::Bool(BoolOp::And, c_lo, c_hi));
                let (okb, _) = self.new_block(&[]);
                let (badb, _) = self.new_block(&[]);
                let (join, jp) = self.new_block(&[rt]);
                self.branch(in_range, (okb, vec![]), (badb, vec![]));
                self.cur = Some(okb);
                let conv = self.emit(dst, Op::Conv { src, a });
                let ok = self.emit(rt, Op::Ok(conv));
                self.goto(join, vec![ok]);
                self.cur = Some(badb);
                let kind = self.emit(Ty::U8, Op::Const(0));
                let err_val = self.emit(err_ty, Op::Make { variant: None, fields: vec![kind] });
                let err = self.emit(rt, Op::Err(err_val));
                self.goto(join, vec![err]);
                self.cur = Some(join);
                return Ok(Some(Some(jp[0])));
            }
        }
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

/// 値か参照の型 `t` の指す整数の型 (整数でなければ `None`)。
fn deref_int(t: Ty) -> Option<Ty> {
    match t {
        Ty::Ref(_, p) if p.ty().is_int() => Some(p.ty()),
        t if t.is_int() => Some(t),
        _ => None,
    }
}
