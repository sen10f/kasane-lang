//! 数学関数の自前実装 (仕様 8: `core.num` に自前実装、libm 非依存)。
//!
//! アルゴリズムは演算の抽象 `Ops` の上に一度だけ書く。インタプリタは `Host` (Rust の値) で、
//! 機械語 (LLVM) は KIR の命令で同じ関数を走らせる。両者は同じ
//! 演算列を同じ順で行うので、結果のビット列は一致する (IEEE 754 の丸め、FMA への縮約なし、
//! 分岐のない `sel`)。
//!
//! 方針:
//! - `f64` の関数が本体。`f32` の関数は引数を `f64` へ広げ、`f64` の関数を通して `f32` へ丸める。
//!   `f64` の誤差 (数 ulp の 2^-53 相当) が `f32` の丸めより十分小さいので、`f32` は 1 ulp 以内になる。
//! - 範囲の縮小 (sin/cos/tan は π/2、exp/ln は ln 2) の後に多項式 (Taylor / 奇関数の級数) で評価する。
//! - 特殊値 (NaN・±0・±無限大・範囲外) は演算結果を単一の quiet NaN に正規化する。
//! - 既知の精度: 引数の大きさが約 1e6 までの sin / cos / tan で、`f64` の誤差は数 ulp 以内。
//!   それより大きい引数は縮小の精度が落ちる (3 分割のπ/2 を使わない)。`powi` は
//!   二乗の繰り返しなので、指数 n に対して誤差が log2(n) 程度に比例して増える。

/// 演算の抽象。`F` は `f64`、`I` は 64 ビット整数、`B` は真偽。`f32` は `f64` へ広げて計算する (モジュールの方針)。
pub trait Ops {
    type F: Copy;
    type I: Copy;
    type B: Copy;
    fn c(&self, v: f64) -> Self::F;
    fn ic(&self, v: i64) -> Self::I;
    fn add(&self, a: Self::F, b: Self::F) -> Self::F;
    fn sub(&self, a: Self::F, b: Self::F) -> Self::F;
    fn mul(&self, a: Self::F, b: Self::F) -> Self::F;
    fn div(&self, a: Self::F, b: Self::F) -> Self::F;
    fn neg(&self, a: Self::F) -> Self::F;
    fn floor(&self, a: Self::F) -> Self::F;
    /// 順序つき比較。NaN を含むと偽。
    fn lt(&self, a: Self::F, b: Self::F) -> Self::B;
    fn gt(&self, a: Self::F, b: Self::F) -> Self::B;
    fn eq(&self, a: Self::F, b: Self::F) -> Self::B;
    /// `a != b`。NaN を含むと真 (`ne(x, x)` で NaN を判定できる)。
    fn ne(&self, a: Self::F, b: Self::F) -> Self::B;
    fn sel(&self, c: Self::B, a: Self::F, b: Self::F) -> Self::F;
    fn isel(&self, c: Self::B, a: Self::I, b: Self::I) -> Self::I;
    /// 0 方向へ切り捨て、範囲外は飽和、NaN は 0 (Rust の `as` と同じ)。
    fn to_i(&self, a: Self::F) -> Self::I;
    fn from_i(&self, a: Self::I) -> Self::F;
    fn bits(&self, a: Self::F) -> Self::I;
    fn from_bits(&self, a: Self::I) -> Self::F;
    fn iadd(&self, a: Self::I, b: Self::I) -> Self::I;
    fn isub(&self, a: Self::I, b: Self::I) -> Self::I;
    fn iand(&self, a: Self::I, b: Self::I) -> Self::I;
    /// 論理右シフト。
    fn ishr(&self, a: Self::I, n: u32) -> Self::I;
    fn ishl(&self, a: Self::I, n: u32) -> Self::I;
    fn nz(&self, a: Self::I) -> Self::B;
}

/// インタプリタ側の演算 (Rust の値そのもの)。
pub struct Host;

impl Ops for Host {
    type F = f64;
    type I = i64;
    type B = bool;
    fn c(&self, v: f64) -> f64 {
        v
    }
    fn ic(&self, v: i64) -> i64 {
        v
    }
    fn add(&self, a: f64, b: f64) -> f64 {
        a + b
    }
    fn sub(&self, a: f64, b: f64) -> f64 {
        a - b
    }
    fn mul(&self, a: f64, b: f64) -> f64 {
        a * b
    }
    fn div(&self, a: f64, b: f64) -> f64 {
        a / b
    }
    fn neg(&self, a: f64) -> f64 {
        -a
    }
    fn floor(&self, a: f64) -> f64 {
        a.floor()
    }
    fn lt(&self, a: f64, b: f64) -> bool {
        a < b
    }
    fn gt(&self, a: f64, b: f64) -> bool {
        a > b
    }
    fn eq(&self, a: f64, b: f64) -> bool {
        a == b
    }
    fn ne(&self, a: f64, b: f64) -> bool {
        a != b
    }
    fn sel(&self, c: bool, a: f64, b: f64) -> f64 {
        if c {
            a
        } else {
            b
        }
    }
    fn isel(&self, c: bool, a: i64, b: i64) -> i64 {
        if c {
            a
        } else {
            b
        }
    }
    fn to_i(&self, a: f64) -> i64 {
        a as i64
    }
    fn from_i(&self, a: i64) -> f64 {
        a as f64
    }
    fn bits(&self, a: f64) -> i64 {
        a.to_bits() as i64
    }
    fn from_bits(&self, a: i64) -> f64 {
        f64::from_bits(a as u64)
    }
    fn iadd(&self, a: i64, b: i64) -> i64 {
        a.wrapping_add(b)
    }
    fn isub(&self, a: i64, b: i64) -> i64 {
        a.wrapping_sub(b)
    }
    fn iand(&self, a: i64, b: i64) -> i64 {
        a & b
    }
    fn ishr(&self, a: i64, n: u32) -> i64 {
        ((a as u64) >> n) as i64
    }
    fn ishl(&self, a: i64, n: u32) -> i64 {
        ((a as u64) << n) as i64
    }
    fn nz(&self, a: i64) -> bool {
        a != 0
    }
}

// ---- 定数 ----

const LOG2E: f64 = 1.442_695_040_888_963_4;
/// ln 2 の上位 (下位 32 ビットが 0 なので k * LN2_HI は |k| < 2^21 で厳密)。
const LN2_HI: f64 = 6.931_471_803_691_238_16e-01;
const LN2_LO: f64 = 1.908_214_929_270_587_7e-10;
const SQRT2: f64 = 1.414_213_562_373_095_1;
const TWO_OVER_PI: f64 = 0.636_619_772_367_581_4;
/// π/2 の上位 (下位ビットが 0 で、k * PIO2_HI は |k| < 2^20 で厳密) と残り。
const PIO2_HI: f64 = 1.570_796_326_734_125_6;
const PIO2_LO: f64 = 6.077_100_506_506_192e-11;
const PI_HI: f64 = 3.141_592_653_589_793_1;
const PI_LO: f64 = 1.224_646_799_147_353_2e-16;
/// 3π/4 の f64 最近似 (正しく丸めた値。`atan2(±∞, -∞)` の結果)。
const THREE_PI_4: f64 = 2.356_194_490_192_344_928_846_982_537_459_627_163;
/// atan の縮小に使う π/2 と π/4 (それぞれ f64 の最近似と残り)。
const ATAN_PIO2_HI: f64 = 1.570_796_326_794_896_6;
const ATAN_PIO2_LO: f64 = 6.123_233_995_736_766e-17;
const ATAN_PIO4_HI: f64 = 0.785_398_163_397_448_3;
const ATAN_PIO4_LO: f64 = 3.061_616_997_868_383e-17;

/// `1 / n!` (n <= 18 では厳密に近い)。係数は両バックエンドで同じ値になる。
fn inv_fact(n: u32) -> f64 {
    let mut f = 1.0;
    for i in 2..=n {
        f *= i as f64;
    }
    1.0 / f
}

fn sign(k: usize) -> f64 {
    if k % 2 == 0 {
        1.0
    } else {
        -1.0
    }
}

fn sin_coefs() -> Vec<f64> {
    (0..=10).map(|k| sign(k) * inv_fact(2 * k as u32 + 1)).collect()
}
fn cos_coefs() -> Vec<f64> {
    (0..=10).map(|k| sign(k) * inv_fact(2 * k as u32)).collect()
}
fn exp_coefs() -> Vec<f64> {
    (0..=18).map(inv_fact).collect()
}
/// `ln(m) = s * Σ 2/(2k+1) t^k` (s = (m-1)/(m+1), t = s^2)。
fn ln_coefs() -> Vec<f64> {
    (0..=12).map(|k| 2.0 / (2 * k + 1) as f64).collect()
}
/// `atan(z) = z * Σ (-1)^k/(2k+1) z^(2k)`。|z| <= tan(π/8) なので 23 項で十分。
fn atan_coefs() -> Vec<f64> {
    (0..=22).map(|k| sign(k) / (2 * k + 1) as f64).collect()
}

// ---- 共通の部品 ----

/// `cs[0] + cs[1] x + ...` を Horner 法で評価する (係数は低次から)。
fn horner<O: Ops>(o: &O, x: O::F, cs: &[f64]) -> O::F {
    let n = cs.len();
    let mut acc = o.c(cs[n - 1]);
    for i in (0..n - 1).rev() {
        let t = o.mul(acc, x);
        let k = o.c(cs[i]);
        acc = o.add(t, k);
    }
    acc
}

/// NaN を単一の quiet NaN に正規化する。
fn canon<O: Ops>(o: &O, v: O::F) -> O::F {
    let is_nan = o.ne(v, v);
    let n = o.c(f64::NAN);
    o.sel(is_nan, n, v)
}

/// 最近整数 `k` (x * 2/π を丸めた値) と、`r = x - k π/2` (|r| <= π/4 程度) を返す。
/// `k` の下位 2 ビットが象限になる。
fn reduce_pio2<O: Ops>(o: &O, x: O::F) -> (O::F, O::I) {
    let t = o.mul(x, o.c(TWO_OVER_PI));
    let t = o.add(t, o.c(0.5));
    let k = o.floor(t);
    let r = o.sub(x, o.mul(k, o.c(PIO2_HI)));
    let r = o.sub(r, o.mul(k, o.c(PIO2_LO)));
    (r, o.to_i(k))
}

// ---- 三角関数 ----

pub fn sin<O: Ops>(o: &O, x: O::F) -> O::F {
    let (r, q) = reduce_pio2(o, x);
    let r2 = o.mul(r, r);
    let sp = o.mul(r, horner(o, r2, &sin_coefs()));
    let cp = horner(o, r2, &cos_coefs());
    let odd = o.nz(o.iand(q, o.ic(1)));
    let v = o.sel(odd, cp, sp);
    let neg = o.nz(o.iand(q, o.ic(2)));
    let v = o.sel(neg, o.neg(v), v);
    canon(o, v)
}

pub fn cos<O: Ops>(o: &O, x: O::F) -> O::F {
    let (r, q) = reduce_pio2(o, x);
    let r2 = o.mul(r, r);
    let sp = o.mul(r, horner(o, r2, &sin_coefs()));
    let cp = horner(o, r2, &cos_coefs());
    let odd = o.nz(o.iand(q, o.ic(1)));
    let v = o.sel(odd, sp, cp);
    // 象限 1, 2 で負。(q + 1) & 2 != 0 と同じ。
    let neg = o.nz(o.iand(o.iadd(q, o.ic(1)), o.ic(2)));
    let v = o.sel(neg, o.neg(v), v);
    canon(o, v)
}

pub fn tan<O: Ops>(o: &O, x: O::F) -> O::F {
    let (r, q) = reduce_pio2(o, x);
    let r2 = o.mul(r, r);
    let sp = o.mul(r, horner(o, r2, &sin_coefs()));
    let cp = horner(o, r2, &cos_coefs());
    // 奇数象限では tan = -cos / sin、偶数では sin / cos。
    let odd = o.nz(o.iand(q, o.ic(1)));
    let num = o.sel(odd, o.neg(cp), sp);
    let den = o.sel(odd, sp, cp);
    canon(o, o.div(num, den))
}

// ---- 指数・対数 ----

/// `2^k` を作る (k は整数値の `f64`、範囲は [-1022, 1023])。
fn pow2<O: Ops>(o: &O, k: O::F) -> O::F {
    let e = o.iadd(o.to_i(k), o.ic(1023));
    o.from_bits(o.ishl(e, 52))
}

pub fn exp<O: Ops>(o: &O, x: O::F) -> O::F {
    // 結果が inf / 0 になる範囲は端で飽和させる (NaN は変わらない)。
    let x = o.sel(o.gt(x, o.c(710.0)), o.c(710.0), x);
    let x = o.sel(o.lt(x, o.c(-746.0)), o.c(-746.0), x);
    let t = o.mul(x, o.c(LOG2E));
    let t = o.add(t, o.c(0.5));
    let k = o.floor(t);
    let r = o.sub(x, o.mul(k, o.c(LN2_HI)));
    let r = o.sub(r, o.mul(k, o.c(LN2_LO)));
    let p = horner(o, r, &exp_coefs());
    // 2^k を 2 つに分けて掛ける (指数が範囲外になる端と非正規数の結果のため)。
    let k1 = o.floor(o.mul(k, o.c(0.5)));
    let k2 = o.sub(k, k1);
    let s1 = pow2(o, k1);
    let s2 = pow2(o, k2);
    canon(o, o.mul(o.mul(p, s1), s2))
}

pub fn ln<O: Ops>(o: &O, x: O::F) -> O::F {
    // 非正規数は 2^54 倍して正規化し、指数を 54 減らす。
    let small = o.lt(x, o.c(2.225_073_858_507_201_4e-308));
    let xs = o.sel(small, o.mul(x, o.c(18_014_398_509_481_984.0)), x);
    let adj = o.isel(small, o.ic(54), o.ic(0));
    let b = o.bits(xs);
    let e_raw = o.iand(o.ishr(b, 52), o.ic(0x7ff));
    let e = o.isub(o.isub(e_raw, o.ic(1023)), adj);
    let mant = o.iand(b, o.ic(0x000f_ffff_ffff_ffff));
    let m = o.from_bits(o.iadd(mant, o.ic(0x3ff0_0000_0000_0000)));
    // m を [sqrt(1/2), sqrt(2)] へ寄せる。
    let big = o.gt(m, o.c(SQRT2));
    let m = o.sel(big, o.mul(m, o.c(0.5)), m);
    let e = o.isel(big, o.iadd(e, o.ic(1)), e);
    let ef = o.from_i(e);
    let one = o.c(1.0);
    let s = o.div(o.sub(m, one), o.add(m, one));
    let t = o.mul(s, s);
    let lnm = o.mul(s, horner(o, t, &ln_coefs()));
    let res = o.add(o.mul(ef, o.c(LN2_HI)), o.add(lnm, o.mul(ef, o.c(LN2_LO))));
    // 特殊値: ln(±0) = -inf、x < 0 は NaN、ln(inf) = inf、NaN は NaN (上の経路では有限値になるので明示する)。
    let res = o.sel(o.eq(x, o.c(0.0)), o.c(f64::NEG_INFINITY), res);
    let res = o.sel(o.lt(x, o.c(0.0)), o.c(f64::NAN), res);
    let res = o.sel(o.eq(x, o.c(f64::INFINITY)), o.c(f64::INFINITY), res);
    let res = o.sel(o.ne(x, x), o.c(f64::NAN), res);
    canon(o, res)
}

pub fn powi<O: Ops>(o: &O, x: O::F, n: O::I) -> O::F {
    // 二乗の繰り返し (指数の各ビットで掛ける)。負の指数は逆数を取る。
    let neg = o.nz(o.ishr(n, 63));
    let m = o.isel(neg, o.isub(o.ic(0), n), n);
    let mut acc = o.c(1.0);
    let mut base = x;
    for i in 0..32u32 {
        let bit = o.nz(o.iand(o.ishr(m, i), o.ic(1)));
        let prod = o.mul(acc, base);
        acc = o.sel(bit, prod, acc);
        base = o.mul(base, base);
    }
    let inv = o.div(o.c(1.0), acc);
    canon(o, o.sel(neg, inv, acc))
}

// ---- 逆三角 ----

/// `atan(x)`。|x| を π/8 と 3π/8 の境で縮小し、|z| <= tan(π/8) の級数で評価する。
pub fn atan<O: Ops>(o: &O, x: O::F) -> O::F {
    let xb = o.bits(x);
    let a = o.from_bits(o.iand(xb, o.ic(0x7fff_ffff_ffff_ffff)));
    let neg = o.nz(o.ishr(xb, 63));
    let big = o.gt(a, o.c(2.414_213_562_373_095));
    let mid = o.gt(a, o.c(0.414_213_562_373_095_04));
    let one = o.c(1.0);
    let z_mid = o.div(o.sub(a, one), o.add(a, one));
    let z_big = o.div(o.c(-1.0), a);
    let z = o.sel(big, z_big, o.sel(mid, z_mid, a));
    let zero = o.c(0.0);
    let hi = o.sel(big, o.c(ATAN_PIO2_HI), o.sel(mid, o.c(ATAN_PIO4_HI), zero));
    let lo = o.sel(big, o.c(ATAN_PIO2_LO), o.sel(mid, o.c(ATAN_PIO4_LO), zero));
    let z2 = o.mul(z, z);
    let at = o.mul(z, horner(o, z2, &atan_coefs()));
    let r = o.add(hi, o.add(lo, at));
    canon(o, o.sel(neg, o.neg(r), r))
}

/// `|v| == ∞` (符号ビットを外して比べる)。
fn is_inf<O: Ops>(o: &O, v: O::F) -> O::B {
    let a = o.from_bits(o.iand(o.bits(v), o.ic(0x7fff_ffff_ffff_ffff)));
    o.eq(a, o.c(f64::INFINITY))
}

/// `atan2(y, x)` (Rust の `y.atan2(x)` と同じ引数順)。
pub fn atan2<O: Ops>(o: &O, y: O::F, x: O::F) -> O::F {
    let x_zero = o.eq(x, o.c(0.0));
    let x0 = o.sel(x_zero, o.c(1.0), x);
    let q = atan(o, o.div(y, x0));
    let x_neg = o.nz(o.ishr(o.bits(x), 63));
    let y_neg = o.nz(o.ishr(o.bits(y), 63));
    // x < 0 (符号ビットを含む) のときは y の符号で ±π を足す。
    let adj_hi = o.sel(y_neg, o.c(-PI_HI), o.c(PI_HI));
    let adj_lo = o.sel(y_neg, o.c(-PI_LO), o.c(PI_LO));
    let shifted = o.add(adj_hi, o.add(adj_lo, q));
    let r = o.sel(x_neg, shifted, q);
    // x == ±0 かつ y != 0 は ±π/2。y == 0 のときは上の結果 (±0 または ±π) を使う。
    let half = o.sel(y_neg, o.c(-std::f64::consts::FRAC_PI_2), o.c(std::f64::consts::FRAC_PI_2));
    let y_zero = o.eq(y, o.c(0.0));
    let r = o.sel(x_zero, o.sel(y_zero, r, half), r);
    // y == ±∞ は x が有限なら ±π/2 (x が ±∞ なら ±π/4 か ±3π/4)。割り算に任せず直接定める。
    let y_inf = is_inf(o, y);
    let x_inf = is_inf(o, x);
    let mag = o.sel(x_neg, o.c(THREE_PI_4), o.c(std::f64::consts::FRAC_PI_4));
    let both = o.sel(y_neg, o.neg(mag), mag);
    let r = o.sel(y_inf, o.sel(x_inf, both, half), r);
    // NaN はどちらの引数でも NaN (上の分岐で ±π/2 になる場合も含めて上書きする)。
    let r = o.sel(o.ne(y, y), o.c(f64::NAN), r);
    let r = o.sel(o.ne(x, x), o.c(f64::NAN), r);
    canon(o, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ulp_f64(a: f64, b: f64) -> u64 {
        if a.is_nan() || b.is_nan() {
            return if a.is_nan() && b.is_nan() { 0 } else { u64::MAX };
        }
        if a == b {
            return 0;
        }
        let key = |v: f64| {
            let bits = v.to_bits() as i64;
            if bits < 0 {
                -(bits & i64::MAX)
            } else {
                bits
            }
        };
        (key(a) - key(b)).unsigned_abs()
    }

    fn ulp_f32(a: f32, b: f32) -> u64 {
        if a.is_nan() || b.is_nan() {
            return if a.is_nan() && b.is_nan() { 0 } else { u64::MAX };
        }
        if a == b {
            return 0;
        }
        let key = |v: f32| {
            let bits = v.to_bits() as i32;
            if bits < 0 {
                -((bits & i32::MAX) as i64)
            } else {
                bits as i64
            }
        };
        (key(a) - key(b)).unsigned_abs()
    }

    /// 検査点 (一様な格子 + 小さい値と大きい値)。
    fn grid(lo: f64, hi: f64, n: usize) -> Vec<f64> {
        (0..=n).map(|i| lo + (hi - lo) * (i as f64) / (n as f64)).collect()
    }

    #[test]
    fn known_values_f64() {
        let h = Host;
        // 既知の値 (倍精度で正しく丸めた値)。
        assert!(ulp_f64(sin(&h, 1.0), 0.841_470_984_807_896_5) <= 1);
        assert!(ulp_f64(cos(&h, 1.0), 0.540_302_305_868_139_8) <= 1);
        assert!(ulp_f64(tan(&h, 1.0), 1.557_407_724_654_902_3) <= 2);
        assert!(ulp_f64(exp(&h, 1.0), std::f64::consts::E) <= 1);
        assert!(ulp_f64(ln(&h, 2.0), std::f64::consts::LN_2) <= 1);
        assert!(ulp_f64(atan2(&h, 1.0, 1.0), std::f64::consts::FRAC_PI_4) <= 1);
        assert!(ulp_f64(atan2(&h, 1.0, -1.0), 3.0 * std::f64::consts::FRAC_PI_4) <= 1);
        assert!(ulp_f64(powi(&h, 2.0, 10), 1024.0) == 0);
        assert!(ulp_f64(powi(&h, 2.0, -2), 0.25) == 0);
        assert!(ulp_f64(powi(&h, 1.5, 3), 3.375) == 0);
        // 特殊値。
        assert!(sin(&h, f64::INFINITY).is_nan());
        assert!(ln(&h, -1.0).is_nan());
        assert_eq!(ln(&h, 0.0), f64::NEG_INFINITY);
        assert_eq!(exp(&h, 1000.0), f64::INFINITY);
        assert_eq!(exp(&h, -1000.0), 0.0);
        assert_eq!(atan2(&h, 0.0, -0.0).to_bits(), std::f64::consts::PI.to_bits());
    }

    #[test]
    fn f64_error_against_std_is_a_few_ulp() {
        let h = Host;
        let mut worst = [0u64; 7];
        for x in grid(-20.0, 20.0, 40_000) {
            worst[0] = worst[0].max(ulp_f64(sin(&h, x), x.sin()));
            worst[1] = worst[1].max(ulp_f64(cos(&h, x), x.cos()));
            worst[2] = worst[2].max(ulp_f64(exp(&h, x), x.exp()));
        }
        for x in grid(1e-6, 1e6, 40_000) {
            worst[3] = worst[3].max(ulp_f64(ln(&h, x), x.ln()));
        }
        for x in grid(-50.0, 50.0, 20_000) {
            worst[4] = worst[4].max(ulp_f64(atan(&h, x), x.atan()));
            worst[5] = worst[5].max(ulp_f64(atan2(&h, x, 1.5), x.atan2(1.5)));
        }
        for x in grid(-1.5, 1.5, 20_000) {
            // tan は極に近いと相対誤差が大きくなるので、絶対誤差で見る。
            assert!((tan(&h, x) - x.tan()).abs() <= 1e-13 * x.tan().abs().max(1.0), "tan {x}");
        }
        for x in grid(-30.0, 30.0, 20_000) {
            let got = exp(&h, x);
            let want = x.exp();
            worst[6] = worst[6].max(ulp_f64(got, want));
        }
        // 検査: sin / cos / exp / ln / atan は 2 ulp 以内。
        for (i, w) in worst.iter().enumerate() {
            if i == 6 {
                continue;
            }
            assert!(*w <= 2, "f64 function {i} has error {w} ulp");
        }
        assert!(worst[6] <= 2, "exp over [-30, 30] has error {} ulp", worst[6]);
    }

    #[test]
    fn f32_results_are_within_one_ulp_of_the_rounded_reference() {
        // 参照: f64 の標準関数の値を f32 へ丸めたもの (f32 の正確丸めに近い)。
        let h = Host;
        let mut worst = [0u64; 5];
        let f32_grid = |lo: f32, hi: f32, n: usize| -> Vec<f32> {
            (0..=n).map(|i| lo + (hi - lo) * (i as f32) / (n as f32)).collect()
        };
        for x in f32_grid(-20.0, 20.0, 40_000) {
            let xf = x as f64;
            worst[0] = worst[0].max(ulp_f32(sin(&h, x as f64) as f32, xf.sin() as f32));
            worst[1] = worst[1].max(ulp_f32(cos(&h, x as f64) as f32, xf.cos() as f32));
            worst[2] = worst[2].max(ulp_f32(exp(&h, x as f64) as f32, xf.exp() as f32));
        }
        for x in f32_grid(1e-4, 1e4, 40_000) {
            worst[3] = worst[3].max(ulp_f32(ln(&h, x as f64) as f32, (x as f64).ln() as f32));
        }
        for x in f32_grid(-10.0, 10.0, 20_000) {
            worst[4] = worst[4].max(ulp_f32(atan(&h, x as f64) as f32, (x as f64).atan() as f32));
        }
        for (i, w) in worst.iter().enumerate() {
            assert!(*w <= 1, "f32 function {i} has error {w} ulp");
        }
    }

    /// ビット一致 (NaN は NaN 同士で一致とみなす。±0 の符号は区別する)。
    fn same(a: f64, b: f64) -> bool {
        if a.is_nan() || b.is_nan() {
            a.is_nan() && b.is_nan()
        } else {
            a.to_bits() == b.to_bits()
        }
    }

    /// C の Annex F (IEEE 754 の atan2 / sin / cos / tan / exp / log / pow の特殊値) との照合。
    #[test]
    fn special_values_match_c_annex_f() {
        let h = Host;
        let inf = f64::INFINITY;
        let nan = f64::NAN;
        let pi = std::f64::consts::PI;
        let half_pi = std::f64::consts::FRAC_PI_2;
        let quarter_pi = std::f64::consts::FRAC_PI_4;
        let three_quarter_pi = 2.356_194_490_192_344_928_846_982_537_459_627_163_f64;
        let mut bad: Vec<String> = Vec::new();
        let mut check = |name: &str, got: f64, want: f64| {
            if !same(got, want) {
                bad.push(format!(
                    "{name}: got {got:?} ({:#x}), want {want:?} ({:#x})",
                    got.to_bits(),
                    want.to_bits()
                ));
            }
        };
        // atan2(y, x)。
        let atan2_cases: [(f64, f64, f64); 37] = [
            (0.0, 0.0, 0.0),
            (0.0, -0.0, pi),
            (-0.0, 0.0, -0.0),
            (-0.0, -0.0, -pi),
            (0.0, 1.0, 0.0),
            (-0.0, 1.0, -0.0),
            (0.0, -1.0, pi),
            (-0.0, -1.0, -pi),
            (1.0, 0.0, half_pi),
            (-1.0, 0.0, -half_pi),
            (1.0, -0.0, half_pi),
            (-1.0, -0.0, -half_pi),
            (1.0, -inf, pi),
            (-1.0, -inf, -pi),
            (0.0, -inf, pi),
            (-0.0, -inf, -pi),
            (1.0, inf, 0.0),
            (-1.0, inf, -0.0),
            (0.0, inf, 0.0),
            (-0.0, inf, -0.0),
            (inf, 1.0, half_pi),
            (inf, -1.0, half_pi),
            (-inf, 1.0, -half_pi),
            (-inf, -1.0, -half_pi),
            (inf, 0.0, half_pi),
            (-inf, -0.0, -half_pi),
            (inf, inf, quarter_pi),
            (inf, -inf, three_quarter_pi),
            (-inf, inf, -quarter_pi),
            (-inf, -inf, -three_quarter_pi),
            (nan, 1.0, nan),
            (1.0, nan, nan),
            (nan, 0.0, nan),
            (nan, -0.0, nan),
            (0.0, nan, nan),
            (inf, nan, nan),
            (nan, inf, nan),
        ];
        for (y, x, want) in atan2_cases {
            check(&format!("atan2({y:?}, {x:?})"), atan2(&h, y, x), want);
        }
        // atan(±0, ±∞, NaN)。
        check("atan(-0)", atan(&h, -0.0), -0.0);
        check("atan(0)", atan(&h, 0.0), 0.0);
        check("atan(inf)", atan(&h, inf), half_pi);
        check("atan(-inf)", atan(&h, -inf), -half_pi);
        check("atan(NaN)", atan(&h, nan), nan);
        // 三角関数。
        check("sin(-0)", sin(&h, -0.0), -0.0);
        check("sin(0)", sin(&h, 0.0), 0.0);
        check("cos(-0)", cos(&h, -0.0), 1.0);
        check("cos(0)", cos(&h, 0.0), 1.0);
        check("tan(-0)", tan(&h, -0.0), -0.0);
        check("tan(0)", tan(&h, 0.0), 0.0);
        for f in [sin as fn(&Host, f64) -> f64, cos, tan] {
            check("trig(inf)", f(&h, inf), nan);
            check("trig(-inf)", f(&h, -inf), nan);
            check("trig(NaN)", f(&h, nan), nan);
        }
        // exp。
        check("exp(0)", exp(&h, 0.0), 1.0);
        check("exp(-0)", exp(&h, -0.0), 1.0);
        check("exp(inf)", exp(&h, inf), inf);
        check("exp(-inf)", exp(&h, -inf), 0.0);
        check("exp(NaN)", exp(&h, nan), nan);
        check("exp(710)", exp(&h, 710.0), inf);
        check("exp(1000)", exp(&h, 1000.0), inf);
        check("exp(-746)", exp(&h, -746.0), 0.0);
        check("exp(-1000)", exp(&h, -1000.0), 0.0);
        for x in [709.782_712_893_384_f64, -708.0, 700.0, -700.0] {
            let got = exp(&h, x);
            assert!(
                ulp_f64(got, x.exp()) <= 2,
                "exp({x}) = {got:?}, std {:?}",
                x.exp()
            );
        }
        // ln。
        check("ln(0)", ln(&h, 0.0), -inf);
        check("ln(-0)", ln(&h, -0.0), -inf);
        check("ln(1)", ln(&h, 1.0), 0.0);
        check("ln(inf)", ln(&h, inf), inf);
        check("ln(-inf)", ln(&h, -inf), nan);
        check("ln(-1)", ln(&h, -1.0), nan);
        check("ln(-tiny)", ln(&h, -5e-324), nan);
        check("ln(NaN)", ln(&h, nan), nan);
        // powi (Rust の powi と C の pow(x, 整数) に合わせる)。
        check("powi(NaN, 0)", powi(&h, nan, 0), 1.0);
        check("powi(NaN, 1)", powi(&h, nan, 1), nan);
        check("powi(NaN, -1)", powi(&h, nan, -1), nan);
        check("powi(0, 0)", powi(&h, 0.0, 0), 1.0);
        check("powi(-0, 3)", powi(&h, -0.0, 3), -0.0);
        check("powi(-0, 2)", powi(&h, -0.0, 2), 0.0);
        check("powi(0, -1)", powi(&h, 0.0, -1), inf);
        check("powi(-0, -1)", powi(&h, -0.0, -1), -inf);
        check("powi(-0, -2)", powi(&h, -0.0, -2), inf);
        check("powi(inf, 3)", powi(&h, inf, 3), inf);
        check("powi(-inf, 3)", powi(&h, -inf, 3), -inf);
        check("powi(-inf, 2)", powi(&h, -inf, 2), inf);
        check("powi(inf, -1)", powi(&h, inf, -1), 0.0);
        check("powi(-inf, -1)", powi(&h, -inf, -1), -0.0);
        check("powi(-inf, -2)", powi(&h, -inf, -2), 0.0);
        check("powi(2, 2000)", powi(&h, 2.0, 2000), inf);
        check("powi(-2, 2001)", powi(&h, -2.0, 2001), -inf);
        check("powi(0.5, 2000)", powi(&h, 0.5, 2000), 0.0);
        check("powi(NaN, 2)", powi(&h, nan, 2), nan);
        assert!(
            bad.is_empty(),
            "special values differ from C:\n{}",
            bad.join("\n")
        );
    }
}

/// NaN の規則 (設計_KIR.md 17.8): 浮動小数の `+ - * /`・`%`・`sqrt` で、結果が NaN になるとき、
/// 第 1 被演算子が NaN ならそれ (quiet 化)、そうでなく第 2 被演算子が NaN ならそれ (quiet 化)、どちらでもなければ正の quiet NaN (`f64::NAN`)。
/// 実機は被演算子が NaN でないとき符号ビットの立った NaN を作る (x86) が、LLVM の定数畳み込みは正の NaN を作る。
/// 木走査・参照実行器・機械語のどれも、この規則で結果を決めてビット列を揃える。
pub fn nan_rule64(a: f64, b: f64, r: f64) -> f64 {
    if !r.is_nan() {
        r
    } else if a.is_nan() {
        f64::from_bits(a.to_bits() | 0x0008_0000_0000_0000)
    } else if b.is_nan() {
        f64::from_bits(b.to_bits() | 0x0008_0000_0000_0000)
    } else {
        f64::NAN
    }
}

/// `nan_rule64` の `f32` 版 (正の quiet NaN は `f32::NAN` = 0x7fc00000)。
pub fn nan_rule32(a: f32, b: f32, r: f32) -> f32 {
    if !r.is_nan() {
        r
    } else if a.is_nan() {
        f32::from_bits(a.to_bits() | 0x0040_0000)
    } else if b.is_nan() {
        f32::from_bits(b.to_bits() | 0x0040_0000)
    } else {
        f32::NAN
    }
}
