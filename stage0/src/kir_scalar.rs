//! KIR の数値の意味 (設計_KIR.md 10 章の拡張: 浮動小数・128 ビット整数・ビット演算・シフト・変換)。
//!
//! 値は `i128` の 1 語で持つ。整数は型の範囲の値 (`u128` はビットパターン)、`f32` / `f64` は IEEE のビット列
//! (`f32` は下位 32 ビット)。ここの規則は木走査インタプリタ (`interp.rs` / `natives.rs`) と同じ結果になるように書く。
//! 浮動小数は木走査と同じ Rust の演算と `fmath` (自前の超越関数) を使うので、結果のビット列も一致する。

use crate::fmath;
use crate::kir::{Aop, BitK, Cc, FBin, FUn, Ty};

/// 浮動小数の値 (ビット列) を `f64` へ広げる。`f32` は広げても値が変わらない。
pub fn to_f64(ty: Ty, bits: i128) -> f64 {
    match ty {
        Ty::F32 => f32::from_bits(bits as u32) as f64,
        _ => f64::from_bits(bits as u64),
    }
}

/// `f64` の結果を型 `ty` のビット列にする (`f32` へは `as f32` で丸める。木走査と同じ)。
pub fn from_f64(ty: Ty, x: f64) -> i128 {
    match ty {
        Ty::F32 => (x as f32).to_bits() as i128,
        _ => x.to_bits() as i128,
    }
}

/// ビットが見える所 (`to_bits`・メモリへの書き出し・FFI) で、NaN を正の quiet NaN にそろえる。NaN 以外と整数は変えない (17.8)。
pub fn canon_bits(ty: Ty, bits: i128) -> i128 {
    match ty {
        Ty::F32 if f32::from_bits(bits as u32).is_nan() => 0x7FC0_0000,
        Ty::F64 if f64::from_bits(bits as u64).is_nan() => 0x7FF8_0000_0000_0000,
        _ => bits,
    }
}

/// 浮動小数の `+ - * /`。`f32` は `f32` のまま計算する。`f64` の `/` は 0 除算を木走査 (`binary`) と同じ規則で扱う。
pub fn farith(op: Aop, ty: Ty, x: i128, y: i128) -> i128 {
    if ty == Ty::F32 {
        let (a, b) = (f32::from_bits(x as u32), f32::from_bits(y as u32));
        let r = match op {
            Aop::Add => a + b,
            Aop::Sub => a - b,
            Aop::Mul => a * b,
            _ => a / b,
        };
        return fmath::nan_rule32(a, b, r).to_bits() as i128;
    }
    let (a, b) = (f64::from_bits(x as u64), f64::from_bits(y as u64));
    let r = match op {
        Aop::Add => a + b,
        Aop::Sub => a - b,
        Aop::Mul => a * b,
        _ => a / b,
    };
    fmath::nan_rule64(a, b, r).to_bits() as i128
}

/// `f64` の 1 引数の演算 (木走査の `natives.rs` の `float_op_f64` と同じ式)。`Neg` / `IsNan` / `IsFinite` は呼び出し側で扱う。
fn fun64(op: FUn, x: f64) -> f64 {
    let simple = |f: fn(f64) -> f64| {
        if x.is_nan() || x.is_infinite() {
            x
        } else {
            f(x)
        }
    };
    match op {
        // IEEE 754 の演算そのもの (木走査の `natives.rs` と同じ。機械語の fabs / sqrt と同じ)。
        FUn::Abs => x.abs(),
        FUn::Sqrt => fmath::nan_rule64(x, x, x.sqrt()),
        FUn::Floor => simple(f64::floor),
        FUn::Ceil => simple(f64::ceil),
        FUn::Trunc => simple(f64::trunc),
        FUn::RoundEven => simple(f64::round_ties_even),
        FUn::Sin => fmath::sin(&fmath::Host, x),
        FUn::Cos => fmath::cos(&fmath::Host, x),
        FUn::Tan => fmath::tan(&fmath::Host, x),
        FUn::Exp => fmath::exp(&fmath::Host, x),
        FUn::Ln => fmath::ln(&fmath::Host, x),
        FUn::Neg | FUn::IsNan | FUn::IsFinite => x,
    }
}

/// 浮動小数の 1 引数の演算。`src` は被演算子の型、`res` は結果の型 (`IsNan` / `IsFinite` だけ `bool`)。
pub fn funary(op: FUn, src: Ty, res: Ty, a: i128) -> i128 {
    match op {
        FUn::Neg => {
            if src == Ty::F32 {
                (-f32::from_bits(a as u32)).to_bits() as i128
            } else {
                (-f64::from_bits(a as u64)).to_bits() as i128
            }
        }
        // f32 の `abs` は符号ビットを落とすだけ (ビットのまま。f64 へ広げると signaling NaN が変わる)。
        FUn::Abs if src == Ty::F32 => (a as u32 & 0x7fff_ffff) as i128,
        FUn::IsNan => to_f64(src, a).is_nan() as i128,
        FUn::IsFinite => {
            let x = to_f64(src, a);
            (!(x.is_nan() || x.is_infinite())) as i128
        }
        _ => from_f64(res, fun64(op, to_f64(src, a))),
    }
}

/// 浮動小数の 2 引数の演算 (`res` は浮動小数の型)。`Powi` の第 2 引数は `i32` の値。
pub fn fbin(op: FBin, res: Ty, a: i128, b: i128) -> i128 {
    let x = to_f64(res, a);
    let y = if op == FBin::Powi { b as f64 } else { to_f64(res, b) };
    let r = match op {
        FBin::Max => x.max(y),
        FBin::Min => x.min(y),
        FBin::Rem => {
            fmath::nan_rule64(x, y, x % y)
        }
        FBin::Atan2 => fmath::atan2(&fmath::Host, x, y),
        FBin::Powi => fmath::powi(&fmath::Host, x, y as i64),
    };
    from_f64(res, r)
}

/// 比較。浮動小数は IEEE の順序 (NaN を含むと `<` `<=` `>` `>=` `==` は偽、`!=` は真)。`u128` は符号なしで比べる。
pub fn cmp(cc: Cc, ty: Ty, x: i128, y: i128) -> bool {
    let ord = |o: std::cmp::Ordering| -> bool {
        use std::cmp::Ordering::*;
        match cc {
            Cc::Eq => o == Equal,
            Cc::Ne => o != Equal,
            Cc::Lt => o == Less,
            Cc::Le => o != Greater,
            Cc::Gt => o == Greater,
            Cc::Ge => o != Less,
        }
    };
    if ty.is_float() {
        let (a, b) = (to_f64(ty, x), to_f64(ty, y));
        return match cc {
            Cc::Eq => a == b,
            Cc::Ne => a != b,
            Cc::Lt => a < b,
            Cc::Le => a <= b,
            Cc::Gt => a > b,
            Cc::Ge => a >= b,
        };
    }
    if ty == Ty::U128 {
        return ord((x as u128).cmp(&(y as u128)));
    }
    ord(x.cmp(&y))
}

/// 整数の `max` / `min` (木走査の `s.max(o)` と同じ)。
pub fn minmax(max: bool, ty: Ty, x: i128, y: i128) -> i128 {
    let less = if ty == Ty::U128 { (x as u128) < (y as u128) } else { x < y };
    if max == less {
        y
    } else {
        x
    }
}

/// `wrapping_shl` / `wrapping_shr`。量は型幅で剰余する (木走査と同じ)。`shr` は符号付きなら算術、符号なしなら論理。
pub fn shift(left: bool, ty: Ty, x: i128, n: i128) -> i128 {
    let amt = (n % ty.bits() as i128) as u32;
    if ty == Ty::U128 {
        let u = x as u128;
        return if left { u.wrapping_shl(amt) } else { u.wrapping_shr(amt) } as i128;
    }
    if left {
        ty.wrap(x.wrapping_shl(amt))
    } else {
        // 値は型の範囲にあるので、符号付きは算術シフト、符号なしは非負の値の論理シフトと同じ。
        x >> amt
    }
}

/// 整数の `!` (ビット反転)。`bool` は論理否定を呼び出し側で扱う。
pub fn not_int(ty: Ty, x: i128) -> i128 {
    ty.wrap(!x)
}

/// 変換。整数どうしは値をそのまま保つ (検査が型の範囲に入る組だけを許す)。整数から浮動小数は最も近い値へ丸める
/// (`u128` は符号なしとして、`f32` は `f64` を経由して丸める。木走査の `from_*_nearest` と同じ)。
pub fn conv(src: Ty, dst: Ty, a: i128) -> i128 {
    if src.is_int() && dst.is_int() {
        // 狭める変換 (`try_from_*` が範囲を確かめた後に使う) は、LLVM の trunc と同じく下位のビットを取る。
        // 符号が変わる変換 (`i64` の負の値を `u64` へ) も、LLVM と同じくビットの読み直しで型の範囲に入れる (負のままにしない)。
        return if dst.bits() < src.bits() || src.signed() != dst.signed() { dst.wrap(a) } else { a };
    }
    if src.is_float() && dst.is_int() {
        // 飽和し、NaN は 0 (Rust の `as` と LLVM の `fptosi.sat` / `fptoui.sat` と同じ)。
        let x = to_f64(src, a);
        let v = if x.is_nan() {
            0
        } else if dst.signed() {
            (x as i64) as i128
        } else {
            (x as u64) as i128
        };
        return v.clamp(dst.min(), dst.max());
    }
    if src.is_int() {
        let x = if src == Ty::U128 { a as u128 as f64 } else { a as f64 };
        return from_f64(dst, x);
    }
    from_f64(dst, to_f64(src, a))
}

/// 整数の `wrapping_neg`。
pub fn wrap_neg(ty: Ty, x: i128) -> i128 {
    ty.wrap(x.wrapping_neg())
}

/// `count_ones` / `leading_zeros` / `trailing_zeros` (木走査の `natives.rs` と同じ式。幅は型のビット数)。
pub fn bitcount(k: BitK, ty: Ty, x: i128) -> i128 {
    let b = ty.bits();
    let mask: u128 = if b == 128 { u128::MAX } else { (1u128 << b) - 1 };
    let u = (x as u128) & mask;
    let r: i64 = match k {
        BitK::Ones => u.count_ones() as i64,
        BitK::Leading => b as i64 - (128 - u.leading_zeros() as i64),
        BitK::Trailing => {
            if u == 0 {
                b as i64
            } else {
                u.trailing_zeros() as i64
            }
        }
    };
    r as i128
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits64(x: f64) -> i128 {
        x.to_bits() as i128
    }

    #[test]
    fn float_division_by_zero_follows_the_tree_walker() {
        // 木走査: 0 / 0 と NaN / 0 は canonical な NaN、それ以外は符号付きの無限大。
        let zero = bits64(0.0);
        let one = bits64(1.0);
        let neg = bits64(-1.0);
        assert_eq!(farith(Aop::Div, Ty::F64, zero, zero), bits64(f64::NAN));
        assert_eq!(farith(Aop::Div, Ty::F64, one, zero), bits64(f64::INFINITY));
        assert_eq!(farith(Aop::Div, Ty::F64, neg, zero), bits64(f64::NEG_INFINITY));
        assert_eq!(farith(Aop::Div, Ty::F64, one, bits64(-0.0)), bits64(f64::NEG_INFINITY));
    }

    #[test]
    fn nan_compares_false_except_not_equal() {
        let nan = bits64(f64::NAN);
        let one = bits64(1.0);
        assert!(!cmp(Cc::Lt, Ty::F64, nan, one));
        assert!(!cmp(Cc::Eq, Ty::F64, nan, nan));
        assert!(cmp(Cc::Ne, Ty::F64, nan, nan));
    }

    #[test]
    fn u128_compares_and_shifts_as_unsigned() {
        let big = -1i128; // u128::MAX
        assert!(cmp(Cc::Gt, Ty::U128, big, 1));
        assert!(!cmp(Cc::Gt, Ty::I128, big, 1));
        assert_eq!(shift(false, Ty::U128, big, 127), 1);
        assert_eq!(shift(false, Ty::I128, -1, 127), -1);
    }

    #[test]
    fn narrow_shifts_wrap_the_amount_and_the_value() {
        assert_eq!(shift(true, Ty::U8, 0x81, 1), 0x02);
        assert_eq!(shift(true, Ty::I8, 1, 7), -128);
        assert_eq!(shift(false, Ty::I8, -128, 9), -64);
    }

    #[test]
    fn int_to_float_rounds_through_f64_for_f32() {
        // 木走査の from_i64_nearest (f32): i64 → f64 → f32 の二段の丸め。
        let v = (1i128 << 53) + 1;
        assert_eq!(conv(Ty::I64, Ty::F32, v), ((v as f64) as f32).to_bits() as i128);
    }
}
