// 生成: spec/tools/gen_num_sig.py (手で編集しない)
const core.num.f32.EPSILON: f32 = f32.from_bits(0x34000000u32) pub
const core.num.f32.INFINITY: f32 = f32.from_bits(0x7f800000u32) pub
const core.num.f32.MAX: f32 = f32.from_bits(0x7f7fffffu32) pub
const core.num.f32.MIN_POSITIVE: f32 = f32.from_bits(0x00800000u32) pub
const core.num.f32.NAN: f32 = f32.from_bits(0x7fc00000u32) pub
const core.num.f32.NEG_INFINITY: f32 = f32.from_bits(0xff800000u32) pub
const core.num.f64.EPSILON: f64 = f64.from_bits(0x3cb0000000000000u64) pub
const core.num.f64.INFINITY: f64 = f64.from_bits(0x7ff0000000000000u64) pub
const core.num.f64.MAX: f64 = f64.from_bits(0x7fefffffffffffffu64) pub
const core.num.f64.MIN_POSITIVE: f64 = f64.from_bits(0x0010000000000000u64) pub
const core.num.f64.NAN: f64 = f64.from_bits(0x7ff8000000000000u64) pub
const core.num.f64.NEG_INFINITY: f64 = f64.from_bits(0xfff0000000000000u64) pub
const core.num.i128.BITS: u32 = 128u32 pub
const core.num.i128.MAX: i128 = 170141183460469231731687303715884105727i128 pub
const core.num.i128.MIN: i128 = -170141183460469231731687303715884105728i128 pub
const core.num.i16.BITS: u32 = 16u32 pub
const core.num.i16.MAX: i16 = 32767i16 pub
const core.num.i16.MIN: i16 = -32768i16 pub
const core.num.i32.BITS: u32 = 32u32 pub
const core.num.i32.MAX: i32 = 2147483647i32 pub
const core.num.i32.MIN: i32 = -2147483648i32 pub
const core.num.i64.BITS: u32 = 64u32 pub
const core.num.i64.MAX: i64 = 9223372036854775807i64 pub
const core.num.i64.MIN: i64 = -9223372036854775808i64 pub
const core.num.i8.BITS: u32 = 8u32 pub
const core.num.i8.MAX: i8 = 127i8 pub
const core.num.i8.MIN: i8 = -128i8 pub
const core.num.isize.BITS: u32 = 64u32 pub
const core.num.isize.MAX: isize = 9223372036854775807isize pub
const core.num.isize.MIN: isize = -9223372036854775808isize pub
const core.num.u128.BITS: u32 = 128u32 pub
const core.num.u128.MAX: u128 = 340282366920938463463374607431768211455u128 pub
const core.num.u128.MIN: u128 = 0u128 pub
const core.num.u16.BITS: u32 = 16u32 pub
const core.num.u16.MAX: u16 = 65535u16 pub
const core.num.u16.MIN: u16 = 0u16 pub
const core.num.u32.BITS: u32 = 32u32 pub
const core.num.u32.MAX: u32 = 4294967295u32 pub
const core.num.u32.MIN: u32 = 0u32 pub
const core.num.u64.BITS: u32 = 64u32 pub
const core.num.u64.MAX: u64 = 18446744073709551615u64 pub
const core.num.u64.MIN: u64 = 0u64 pub
const core.num.u8.BITS: u32 = 8u32 pub
const core.num.u8.MAX: u8 = 255u8 pub
const core.num.u8.MIN: u8 = 0u8 pub
const core.num.usize.BITS: u32 = 64u32 pub
const core.num.usize.MAX: usize = 18446744073709551615usize pub
const core.num.usize.MIN: usize = 0usize pub

type core.num.ParseIntError derive [Debug, Clone, Copy, PartialEq, Eq] pub
    kind: u8
end core.num.ParseIntError

type core.num.TryFromIntError derive [Debug, Clone, Copy, PartialEq, Eq] pub
    kind: u8
end core.num.TryFromIntError

fn core.num.f32.abs(self: f32) -> f32 uses [] panics never pub
fn core.num.f32.atan2(self: f32, other: f32) -> f32 uses [] panics never pub
fn core.num.f32.ceil(self: f32) -> f32 uses [] panics never pub
fn core.num.f32.cos(self: f32) -> f32 uses [] panics never pub
fn core.num.f32.exp(self: f32) -> f32 uses [] panics never pub
fn core.num.f32.floor(self: f32) -> f32 uses [] panics never pub
fn core.num.f32.from_bits(bits: u32) -> f32 const uses [] panics never pub
fn core.num.f32.from_i128_nearest(x: i128) -> f32 uses [] panics never pub
fn core.num.f32.from_i16_nearest(x: i16) -> f32 uses [] panics never pub
fn core.num.f32.from_i32_nearest(x: i32) -> f32 uses [] panics never pub
fn core.num.f32.from_i64_nearest(x: i64) -> f32 uses [] panics never pub
fn core.num.f32.from_i8_nearest(x: i8) -> f32 uses [] panics never pub
fn core.num.f32.from_isize_nearest(x: isize) -> f32 uses [] panics never pub
fn core.num.f32.from_u128_nearest(x: u128) -> f32 uses [] panics never pub
fn core.num.f32.from_u16_nearest(x: u16) -> f32 uses [] panics never pub
fn core.num.f32.from_u32_nearest(x: u32) -> f32 uses [] panics never pub
fn core.num.f32.from_u64_nearest(x: u64) -> f32 uses [] panics never pub
fn core.num.f32.from_u8_nearest(x: u8) -> f32 uses [] panics never pub
fn core.num.f32.from_usize_nearest(x: usize) -> f32 uses [] panics never pub
fn core.num.f32.is_finite(self: f32) -> bool uses [] panics never pub
fn core.num.f32.is_nan(self: f32) -> bool uses [] panics never pub
fn core.num.f32.ln(self: f32) -> f32 uses [] panics never pub
fn core.num.f32.max(self: f32, other: f32) -> f32 uses [] panics never pub
fn core.num.f32.min(self: f32, other: f32) -> f32 uses [] panics never pub
fn core.num.f32.powi(self: f32, exponent: i32) -> f32 uses [] panics never pub
fn core.num.f32.rem(self: f32, other: f32) -> f32 uses [] panics never pub
fn core.num.f32.round_ties_even(self: f32) -> f32 uses [] panics never pub
fn core.num.f32.sin(self: f32) -> f32 uses [] panics never pub
fn core.num.f32.sqrt(self: f32) -> f32 uses [] panics never pub
fn core.num.f32.tan(self: f32) -> f32 uses [] panics never pub
fn core.num.f32.to_bits(self: f32) -> u32 uses [] panics never pub
fn core.num.f32.to_f64(self: f32) -> f64 uses [] panics never pub
fn core.num.f32.trunc(self: f32) -> f32 uses [] panics never pub
fn core.num.f32.trunc_to_i128(self: f32) -> Option<i128> uses [] panics never pub
fn core.num.f32.trunc_to_i16(self: f32) -> Option<i16> uses [] panics never pub
fn core.num.f32.trunc_to_i32(self: f32) -> Option<i32> uses [] panics never pub
fn core.num.f32.trunc_to_i64(self: f32) -> Option<i64> uses [] panics never pub
fn core.num.f32.trunc_to_i8(self: f32) -> Option<i8> uses [] panics never pub
fn core.num.f32.trunc_to_isize(self: f32) -> Option<isize> uses [] panics never pub
fn core.num.f32.trunc_to_u128(self: f32) -> Option<u128> uses [] panics never pub
fn core.num.f32.trunc_to_u16(self: f32) -> Option<u16> uses [] panics never pub
fn core.num.f32.trunc_to_u32(self: f32) -> Option<u32> uses [] panics never pub
fn core.num.f32.trunc_to_u64(self: f32) -> Option<u64> uses [] panics never pub
fn core.num.f32.trunc_to_u8(self: f32) -> Option<u8> uses [] panics never pub
fn core.num.f32.trunc_to_usize(self: f32) -> Option<usize> uses [] panics never pub
fn core.num.f64.abs(self: f64) -> f64 uses [] panics never pub
fn core.num.f64.atan2(self: f64, other: f64) -> f64 uses [] panics never pub
fn core.num.f64.ceil(self: f64) -> f64 uses [] panics never pub
fn core.num.f64.cos(self: f64) -> f64 uses [] panics never pub
fn core.num.f64.exp(self: f64) -> f64 uses [] panics never pub
fn core.num.f64.floor(self: f64) -> f64 uses [] panics never pub
fn core.num.f64.from_bits(bits: u64) -> f64 const uses [] panics never pub
fn core.num.f64.from_i128_nearest(x: i128) -> f64 uses [] panics never pub
fn core.num.f64.from_i16_nearest(x: i16) -> f64 uses [] panics never pub
fn core.num.f64.from_i32_nearest(x: i32) -> f64 uses [] panics never pub
fn core.num.f64.from_i64_nearest(x: i64) -> f64 uses [] panics never pub
fn core.num.f64.from_i8_nearest(x: i8) -> f64 uses [] panics never pub
fn core.num.f64.from_isize_nearest(x: isize) -> f64 uses [] panics never pub
fn core.num.f64.from_u128_nearest(x: u128) -> f64 uses [] panics never pub
fn core.num.f64.from_u16_nearest(x: u16) -> f64 uses [] panics never pub
fn core.num.f64.from_u32_nearest(x: u32) -> f64 uses [] panics never pub
fn core.num.f64.from_u64_nearest(x: u64) -> f64 uses [] panics never pub
fn core.num.f64.from_u8_nearest(x: u8) -> f64 uses [] panics never pub
fn core.num.f64.from_usize_nearest(x: usize) -> f64 uses [] panics never pub
fn core.num.f64.is_finite(self: f64) -> bool uses [] panics never pub
fn core.num.f64.is_nan(self: f64) -> bool uses [] panics never pub
fn core.num.f64.ln(self: f64) -> f64 uses [] panics never pub
fn core.num.f64.max(self: f64, other: f64) -> f64 uses [] panics never pub
fn core.num.f64.min(self: f64, other: f64) -> f64 uses [] panics never pub
fn core.num.f64.nearest_to_f32(self: f64) -> f32 uses [] panics never pub
fn core.num.f64.powi(self: f64, exponent: i32) -> f64 uses [] panics never pub
fn core.num.f64.rem(self: f64, other: f64) -> f64 uses [] panics never pub
fn core.num.f64.round_ties_even(self: f64) -> f64 uses [] panics never pub
fn core.num.f64.sin(self: f64) -> f64 uses [] panics never pub
fn core.num.f64.sqrt(self: f64) -> f64 uses [] panics never pub
fn core.num.f64.tan(self: f64) -> f64 uses [] panics never pub
fn core.num.f64.to_bits(self: f64) -> u64 uses [] panics never pub
fn core.num.f64.trunc(self: f64) -> f64 uses [] panics never pub
fn core.num.f64.trunc_to_i128(self: f64) -> Option<i128> uses [] panics never pub
fn core.num.f64.trunc_to_i16(self: f64) -> Option<i16> uses [] panics never pub
fn core.num.f64.trunc_to_i32(self: f64) -> Option<i32> uses [] panics never pub
fn core.num.f64.trunc_to_i64(self: f64) -> Option<i64> uses [] panics never pub
fn core.num.f64.trunc_to_i8(self: f64) -> Option<i8> uses [] panics never pub
fn core.num.f64.trunc_to_isize(self: f64) -> Option<isize> uses [] panics never pub
fn core.num.f64.trunc_to_u128(self: f64) -> Option<u128> uses [] panics never pub
fn core.num.f64.trunc_to_u16(self: f64) -> Option<u16> uses [] panics never pub
fn core.num.f64.trunc_to_u32(self: f64) -> Option<u32> uses [] panics never pub
fn core.num.f64.trunc_to_u64(self: f64) -> Option<u64> uses [] panics never pub
fn core.num.f64.trunc_to_u8(self: f64) -> Option<u8> uses [] panics never pub
fn core.num.f64.trunc_to_usize(self: f64) -> Option<usize> uses [] panics never pub
fn core.num.i128.checked_abs(self: i128) -> Option<i128> uses [] panics never pub
fn core.num.i128.checked_add(self: i128, other: i128) -> Option<i128> uses [] panics never pub
fn core.num.i128.checked_div(self: i128, other: i128) -> Option<i128> uses [] panics never pub
fn core.num.i128.checked_mul(self: i128, other: i128) -> Option<i128> uses [] panics never pub
fn core.num.i128.checked_neg(self: i128) -> Option<i128> uses [] panics never pub
fn core.num.i128.checked_rem(self: i128, other: i128) -> Option<i128> uses [] panics never pub
fn core.num.i128.checked_shl(self: i128, amount: u32) -> Option<i128> uses [] panics never pub
fn core.num.i128.checked_shr(self: i128, amount: u32) -> Option<i128> uses [] panics never pub
fn core.num.i128.checked_sub(self: i128, other: i128) -> Option<i128> uses [] panics never pub
fn core.num.i128.count_ones(self: i128) -> u32 uses [] panics never pub
fn core.num.i128.from_i16(x: i16) -> i128 uses [] panics never pub
fn core.num.i128.from_i32(x: i32) -> i128 uses [] panics never pub
fn core.num.i128.from_i64(x: i64) -> i128 uses [] panics never pub
fn core.num.i128.from_i8(x: i8) -> i128 uses [] panics never pub
fn core.num.i128.from_isize(x: isize) -> i128 uses [] panics never pub
fn core.num.i128.from_str_radix(text: &str, radix: u32) -> Result<i128, core.num.ParseIntError> uses [] panics never pub
fn core.num.i128.from_u16(x: u16) -> i128 uses [] panics never pub
fn core.num.i128.from_u32(x: u32) -> i128 uses [] panics never pub
fn core.num.i128.from_u64(x: u64) -> i128 uses [] panics never pub
fn core.num.i128.from_u8(x: u8) -> i128 uses [] panics never pub
fn core.num.i128.from_usize(x: usize) -> i128 uses [] panics never pub
fn core.num.i128.leading_zeros(self: i128) -> u32 uses [] panics never pub
fn core.num.i128.max(self: i128, other: i128) -> i128 uses [] panics never pub
fn core.num.i128.min(self: i128, other: i128) -> i128 uses [] panics never pub
fn core.num.i128.nearest_to_f64(self: i128) -> f64 uses [] panics never pub
fn core.num.i128.saturating_add(self: i128, other: i128) -> i128 uses [] panics never pub
fn core.num.i128.saturating_mul(self: i128, other: i128) -> i128 uses [] panics never pub
fn core.num.i128.saturating_neg(self: i128) -> i128 uses [] panics never pub
fn core.num.i128.saturating_sub(self: i128, other: i128) -> i128 uses [] panics never pub
impl core.iter.Step for i128
fn core.num.i128.successor(self: &i128) -> Option<i128> uses [] panics never impls core.iter.Step
fn core.num.i128.trailing_zeros(self: i128) -> u32 uses [] panics never pub
fn core.num.i128.try_from_u128(x: u128) -> Result<i128, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i128.wrapping_add(self: i128, other: i128) -> i128 uses [] panics never pub
fn core.num.i128.wrapping_mul(self: i128, other: i128) -> i128 uses [] panics never pub
fn core.num.i128.wrapping_neg(self: i128) -> i128 uses [] panics never pub
fn core.num.i128.wrapping_shl(self: i128, amount: u32) -> i128 uses [] panics never pub
fn core.num.i128.wrapping_shr(self: i128, amount: u32) -> i128 uses [] panics never pub
fn core.num.i128.wrapping_sub(self: i128, other: i128) -> i128 uses [] panics never pub
fn core.num.i16.checked_abs(self: i16) -> Option<i16> uses [] panics never pub
fn core.num.i16.checked_add(self: i16, other: i16) -> Option<i16> uses [] panics never pub
fn core.num.i16.checked_div(self: i16, other: i16) -> Option<i16> uses [] panics never pub
fn core.num.i16.checked_mul(self: i16, other: i16) -> Option<i16> uses [] panics never pub
fn core.num.i16.checked_neg(self: i16) -> Option<i16> uses [] panics never pub
fn core.num.i16.checked_rem(self: i16, other: i16) -> Option<i16> uses [] panics never pub
fn core.num.i16.checked_shl(self: i16, amount: u32) -> Option<i16> uses [] panics never pub
fn core.num.i16.checked_shr(self: i16, amount: u32) -> Option<i16> uses [] panics never pub
fn core.num.i16.checked_sub(self: i16, other: i16) -> Option<i16> uses [] panics never pub
fn core.num.i16.count_ones(self: i16) -> u32 uses [] panics never pub
fn core.num.i16.from_i8(x: i8) -> i16 uses [] panics never pub
fn core.num.i16.from_str_radix(text: &str, radix: u32) -> Result<i16, core.num.ParseIntError> uses [] panics never pub
fn core.num.i16.from_u8(x: u8) -> i16 uses [] panics never pub
fn core.num.i16.leading_zeros(self: i16) -> u32 uses [] panics never pub
fn core.num.i16.max(self: i16, other: i16) -> i16 uses [] panics never pub
fn core.num.i16.min(self: i16, other: i16) -> i16 uses [] panics never pub
fn core.num.i16.saturating_add(self: i16, other: i16) -> i16 uses [] panics never pub
fn core.num.i16.saturating_mul(self: i16, other: i16) -> i16 uses [] panics never pub
fn core.num.i16.saturating_neg(self: i16) -> i16 uses [] panics never pub
fn core.num.i16.saturating_sub(self: i16, other: i16) -> i16 uses [] panics never pub
impl core.iter.Step for i16
fn core.num.i16.successor(self: &i16) -> Option<i16> uses [] panics never impls core.iter.Step
fn core.num.i16.to_f64(self: i16) -> f64 uses [] panics never pub
fn core.num.i16.trailing_zeros(self: i16) -> u32 uses [] panics never pub
fn core.num.i16.try_from_i128(x: i128) -> Result<i16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i16.try_from_i32(x: i32) -> Result<i16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i16.try_from_i64(x: i64) -> Result<i16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i16.try_from_isize(x: isize) -> Result<i16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i16.try_from_u128(x: u128) -> Result<i16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i16.try_from_u16(x: u16) -> Result<i16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i16.try_from_u32(x: u32) -> Result<i16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i16.try_from_u64(x: u64) -> Result<i16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i16.try_from_usize(x: usize) -> Result<i16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i16.wrapping_add(self: i16, other: i16) -> i16 uses [] panics never pub
fn core.num.i16.wrapping_mul(self: i16, other: i16) -> i16 uses [] panics never pub
fn core.num.i16.wrapping_neg(self: i16) -> i16 uses [] panics never pub
fn core.num.i16.wrapping_shl(self: i16, amount: u32) -> i16 uses [] panics never pub
fn core.num.i16.wrapping_shr(self: i16, amount: u32) -> i16 uses [] panics never pub
fn core.num.i16.wrapping_sub(self: i16, other: i16) -> i16 uses [] panics never pub
fn core.num.i32.checked_abs(self: i32) -> Option<i32> uses [] panics never pub
fn core.num.i32.checked_add(self: i32, other: i32) -> Option<i32> uses [] panics never pub
fn core.num.i32.checked_div(self: i32, other: i32) -> Option<i32> uses [] panics never pub
fn core.num.i32.checked_mul(self: i32, other: i32) -> Option<i32> uses [] panics never pub
fn core.num.i32.checked_neg(self: i32) -> Option<i32> uses [] panics never pub
fn core.num.i32.checked_rem(self: i32, other: i32) -> Option<i32> uses [] panics never pub
fn core.num.i32.checked_shl(self: i32, amount: u32) -> Option<i32> uses [] panics never pub
fn core.num.i32.checked_shr(self: i32, amount: u32) -> Option<i32> uses [] panics never pub
fn core.num.i32.checked_sub(self: i32, other: i32) -> Option<i32> uses [] panics never pub
fn core.num.i32.count_ones(self: i32) -> u32 uses [] panics never pub
fn core.num.i32.from_i16(x: i16) -> i32 uses [] panics never pub
fn core.num.i32.from_i8(x: i8) -> i32 uses [] panics never pub
fn core.num.i32.from_str_radix(text: &str, radix: u32) -> Result<i32, core.num.ParseIntError> uses [] panics never pub
fn core.num.i32.from_u16(x: u16) -> i32 uses [] panics never pub
fn core.num.i32.from_u8(x: u8) -> i32 uses [] panics never pub
fn core.num.i32.leading_zeros(self: i32) -> u32 uses [] panics never pub
fn core.num.i32.max(self: i32, other: i32) -> i32 uses [] panics never pub
fn core.num.i32.min(self: i32, other: i32) -> i32 uses [] panics never pub
fn core.num.i32.saturating_add(self: i32, other: i32) -> i32 uses [] panics never pub
fn core.num.i32.saturating_mul(self: i32, other: i32) -> i32 uses [] panics never pub
fn core.num.i32.saturating_neg(self: i32) -> i32 uses [] panics never pub
fn core.num.i32.saturating_sub(self: i32, other: i32) -> i32 uses [] panics never pub
impl core.iter.Step for i32
fn core.num.i32.successor(self: &i32) -> Option<i32> uses [] panics never impls core.iter.Step
fn core.num.i32.to_f64(self: i32) -> f64 uses [] panics never pub
fn core.num.i32.trailing_zeros(self: i32) -> u32 uses [] panics never pub
fn core.num.i32.try_from_i128(x: i128) -> Result<i32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i32.try_from_i64(x: i64) -> Result<i32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i32.try_from_isize(x: isize) -> Result<i32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i32.try_from_u128(x: u128) -> Result<i32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i32.try_from_u32(x: u32) -> Result<i32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i32.try_from_u64(x: u64) -> Result<i32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i32.try_from_usize(x: usize) -> Result<i32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i32.wrapping_add(self: i32, other: i32) -> i32 uses [] panics never pub
fn core.num.i32.wrapping_mul(self: i32, other: i32) -> i32 uses [] panics never pub
fn core.num.i32.wrapping_neg(self: i32) -> i32 uses [] panics never pub
fn core.num.i32.wrapping_shl(self: i32, amount: u32) -> i32 uses [] panics never pub
fn core.num.i32.wrapping_shr(self: i32, amount: u32) -> i32 uses [] panics never pub
fn core.num.i32.wrapping_sub(self: i32, other: i32) -> i32 uses [] panics never pub
fn core.num.i64.checked_abs(self: i64) -> Option<i64> uses [] panics never pub
fn core.num.i64.checked_add(self: i64, other: i64) -> Option<i64> uses [] panics never pub
fn core.num.i64.checked_div(self: i64, other: i64) -> Option<i64> uses [] panics never pub
fn core.num.i64.checked_mul(self: i64, other: i64) -> Option<i64> uses [] panics never pub
fn core.num.i64.checked_neg(self: i64) -> Option<i64> uses [] panics never pub
fn core.num.i64.checked_rem(self: i64, other: i64) -> Option<i64> uses [] panics never pub
fn core.num.i64.checked_shl(self: i64, amount: u32) -> Option<i64> uses [] panics never pub
fn core.num.i64.checked_shr(self: i64, amount: u32) -> Option<i64> uses [] panics never pub
fn core.num.i64.checked_sub(self: i64, other: i64) -> Option<i64> uses [] panics never pub
fn core.num.i64.count_ones(self: i64) -> u32 uses [] panics never pub
fn core.num.i64.from_i16(x: i16) -> i64 uses [] panics never pub
fn core.num.i64.from_i32(x: i32) -> i64 uses [] panics never pub
fn core.num.i64.from_i8(x: i8) -> i64 uses [] panics never pub
fn core.num.i64.from_isize(x: isize) -> i64 uses [] panics never pub
fn core.num.i64.from_str_radix(text: &str, radix: u32) -> Result<i64, core.num.ParseIntError> uses [] panics never pub
fn core.num.i64.from_u16(x: u16) -> i64 uses [] panics never pub
fn core.num.i64.from_u32(x: u32) -> i64 uses [] panics never pub
fn core.num.i64.from_u8(x: u8) -> i64 uses [] panics never pub
fn core.num.i64.leading_zeros(self: i64) -> u32 uses [] panics never pub
fn core.num.i64.max(self: i64, other: i64) -> i64 uses [] panics never pub
fn core.num.i64.min(self: i64, other: i64) -> i64 uses [] panics never pub
fn core.num.i64.nearest_to_f64(self: i64) -> f64 uses [] panics never pub
fn core.num.i64.saturating_add(self: i64, other: i64) -> i64 uses [] panics never pub
fn core.num.i64.saturating_mul(self: i64, other: i64) -> i64 uses [] panics never pub
fn core.num.i64.saturating_neg(self: i64) -> i64 uses [] panics never pub
fn core.num.i64.saturating_sub(self: i64, other: i64) -> i64 uses [] panics never pub
impl core.iter.Step for i64
fn core.num.i64.successor(self: &i64) -> Option<i64> uses [] panics never impls core.iter.Step
fn core.num.i64.trailing_zeros(self: i64) -> u32 uses [] panics never pub
fn core.num.i64.try_from_i128(x: i128) -> Result<i64, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i64.try_from_u128(x: u128) -> Result<i64, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i64.try_from_u64(x: u64) -> Result<i64, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i64.try_from_usize(x: usize) -> Result<i64, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i64.wrapping_add(self: i64, other: i64) -> i64 uses [] panics never pub
fn core.num.i64.wrapping_mul(self: i64, other: i64) -> i64 uses [] panics never pub
fn core.num.i64.wrapping_neg(self: i64) -> i64 uses [] panics never pub
fn core.num.i64.wrapping_shl(self: i64, amount: u32) -> i64 uses [] panics never pub
fn core.num.i64.wrapping_shr(self: i64, amount: u32) -> i64 uses [] panics never pub
fn core.num.i64.wrapping_sub(self: i64, other: i64) -> i64 uses [] panics never pub
fn core.num.i8.checked_abs(self: i8) -> Option<i8> uses [] panics never pub
fn core.num.i8.checked_add(self: i8, other: i8) -> Option<i8> uses [] panics never pub
fn core.num.i8.checked_div(self: i8, other: i8) -> Option<i8> uses [] panics never pub
fn core.num.i8.checked_mul(self: i8, other: i8) -> Option<i8> uses [] panics never pub
fn core.num.i8.checked_neg(self: i8) -> Option<i8> uses [] panics never pub
fn core.num.i8.checked_rem(self: i8, other: i8) -> Option<i8> uses [] panics never pub
fn core.num.i8.checked_shl(self: i8, amount: u32) -> Option<i8> uses [] panics never pub
fn core.num.i8.checked_shr(self: i8, amount: u32) -> Option<i8> uses [] panics never pub
fn core.num.i8.checked_sub(self: i8, other: i8) -> Option<i8> uses [] panics never pub
fn core.num.i8.count_ones(self: i8) -> u32 uses [] panics never pub
fn core.num.i8.from_str_radix(text: &str, radix: u32) -> Result<i8, core.num.ParseIntError> uses [] panics never pub
fn core.num.i8.leading_zeros(self: i8) -> u32 uses [] panics never pub
fn core.num.i8.max(self: i8, other: i8) -> i8 uses [] panics never pub
fn core.num.i8.min(self: i8, other: i8) -> i8 uses [] panics never pub
fn core.num.i8.saturating_add(self: i8, other: i8) -> i8 uses [] panics never pub
fn core.num.i8.saturating_mul(self: i8, other: i8) -> i8 uses [] panics never pub
fn core.num.i8.saturating_neg(self: i8) -> i8 uses [] panics never pub
fn core.num.i8.saturating_sub(self: i8, other: i8) -> i8 uses [] panics never pub
impl core.iter.Step for i8
fn core.num.i8.successor(self: &i8) -> Option<i8> uses [] panics never impls core.iter.Step
fn core.num.i8.to_f64(self: i8) -> f64 uses [] panics never pub
fn core.num.i8.trailing_zeros(self: i8) -> u32 uses [] panics never pub
fn core.num.i8.try_from_i128(x: i128) -> Result<i8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i8.try_from_i16(x: i16) -> Result<i8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i8.try_from_i32(x: i32) -> Result<i8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i8.try_from_i64(x: i64) -> Result<i8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i8.try_from_isize(x: isize) -> Result<i8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i8.try_from_u128(x: u128) -> Result<i8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i8.try_from_u16(x: u16) -> Result<i8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i8.try_from_u32(x: u32) -> Result<i8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i8.try_from_u64(x: u64) -> Result<i8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i8.try_from_u8(x: u8) -> Result<i8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i8.try_from_usize(x: usize) -> Result<i8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.i8.wrapping_add(self: i8, other: i8) -> i8 uses [] panics never pub
fn core.num.i8.wrapping_mul(self: i8, other: i8) -> i8 uses [] panics never pub
fn core.num.i8.wrapping_neg(self: i8) -> i8 uses [] panics never pub
fn core.num.i8.wrapping_shl(self: i8, amount: u32) -> i8 uses [] panics never pub
fn core.num.i8.wrapping_shr(self: i8, amount: u32) -> i8 uses [] panics never pub
fn core.num.i8.wrapping_sub(self: i8, other: i8) -> i8 uses [] panics never pub
fn core.num.isize.checked_abs(self: isize) -> Option<isize> uses [] panics never pub
fn core.num.isize.checked_add(self: isize, other: isize) -> Option<isize> uses [] panics never pub
fn core.num.isize.checked_div(self: isize, other: isize) -> Option<isize> uses [] panics never pub
fn core.num.isize.checked_mul(self: isize, other: isize) -> Option<isize> uses [] panics never pub
fn core.num.isize.checked_neg(self: isize) -> Option<isize> uses [] panics never pub
fn core.num.isize.checked_rem(self: isize, other: isize) -> Option<isize> uses [] panics never pub
fn core.num.isize.checked_shl(self: isize, amount: u32) -> Option<isize> uses [] panics never pub
fn core.num.isize.checked_shr(self: isize, amount: u32) -> Option<isize> uses [] panics never pub
fn core.num.isize.checked_sub(self: isize, other: isize) -> Option<isize> uses [] panics never pub
fn core.num.isize.count_ones(self: isize) -> u32 uses [] panics never pub
fn core.num.isize.from_i16(x: i16) -> isize uses [] panics never pub
fn core.num.isize.from_i32(x: i32) -> isize uses [] panics never pub
fn core.num.isize.from_i64(x: i64) -> isize uses [] panics never pub
fn core.num.isize.from_i8(x: i8) -> isize uses [] panics never pub
fn core.num.isize.from_str_radix(text: &str, radix: u32) -> Result<isize, core.num.ParseIntError> uses [] panics never pub
fn core.num.isize.from_u16(x: u16) -> isize uses [] panics never pub
fn core.num.isize.from_u32(x: u32) -> isize uses [] panics never pub
fn core.num.isize.from_u8(x: u8) -> isize uses [] panics never pub
fn core.num.isize.leading_zeros(self: isize) -> u32 uses [] panics never pub
fn core.num.isize.max(self: isize, other: isize) -> isize uses [] panics never pub
fn core.num.isize.min(self: isize, other: isize) -> isize uses [] panics never pub
fn core.num.isize.nearest_to_f64(self: isize) -> f64 uses [] panics never pub
fn core.num.isize.saturating_add(self: isize, other: isize) -> isize uses [] panics never pub
fn core.num.isize.saturating_mul(self: isize, other: isize) -> isize uses [] panics never pub
fn core.num.isize.saturating_neg(self: isize) -> isize uses [] panics never pub
fn core.num.isize.saturating_sub(self: isize, other: isize) -> isize uses [] panics never pub
impl core.iter.Step for isize
fn core.num.isize.successor(self: &isize) -> Option<isize> uses [] panics never impls core.iter.Step
fn core.num.isize.trailing_zeros(self: isize) -> u32 uses [] panics never pub
fn core.num.isize.try_from_i128(x: i128) -> Result<isize, core.num.TryFromIntError> uses [] panics never pub
fn core.num.isize.try_from_u128(x: u128) -> Result<isize, core.num.TryFromIntError> uses [] panics never pub
fn core.num.isize.try_from_u64(x: u64) -> Result<isize, core.num.TryFromIntError> uses [] panics never pub
fn core.num.isize.try_from_usize(x: usize) -> Result<isize, core.num.TryFromIntError> uses [] panics never pub
fn core.num.isize.wrapping_add(self: isize, other: isize) -> isize uses [] panics never pub
fn core.num.isize.wrapping_mul(self: isize, other: isize) -> isize uses [] panics never pub
fn core.num.isize.wrapping_neg(self: isize) -> isize uses [] panics never pub
fn core.num.isize.wrapping_shl(self: isize, amount: u32) -> isize uses [] panics never pub
fn core.num.isize.wrapping_shr(self: isize, amount: u32) -> isize uses [] panics never pub
fn core.num.isize.wrapping_sub(self: isize, other: isize) -> isize uses [] panics never pub
fn core.num.u128.checked_add(self: u128, other: u128) -> Option<u128> uses [] panics never pub
fn core.num.u128.checked_div(self: u128, other: u128) -> Option<u128> uses [] panics never pub
fn core.num.u128.checked_mul(self: u128, other: u128) -> Option<u128> uses [] panics never pub
fn core.num.u128.checked_neg(self: u128) -> Option<u128> uses [] panics never pub
fn core.num.u128.checked_rem(self: u128, other: u128) -> Option<u128> uses [] panics never pub
fn core.num.u128.checked_shl(self: u128, amount: u32) -> Option<u128> uses [] panics never pub
fn core.num.u128.checked_shr(self: u128, amount: u32) -> Option<u128> uses [] panics never pub
fn core.num.u128.checked_sub(self: u128, other: u128) -> Option<u128> uses [] panics never pub
fn core.num.u128.count_ones(self: u128) -> u32 uses [] panics never pub
fn core.num.u128.from_str_radix(text: &str, radix: u32) -> Result<u128, core.num.ParseIntError> uses [] panics never pub
fn core.num.u128.from_u16(x: u16) -> u128 uses [] panics never pub
fn core.num.u128.from_u32(x: u32) -> u128 uses [] panics never pub
fn core.num.u128.from_u64(x: u64) -> u128 uses [] panics never pub
fn core.num.u128.from_u8(x: u8) -> u128 uses [] panics never pub
fn core.num.u128.from_usize(x: usize) -> u128 uses [] panics never pub
fn core.num.u128.leading_zeros(self: u128) -> u32 uses [] panics never pub
fn core.num.u128.max(self: u128, other: u128) -> u128 uses [] panics never pub
fn core.num.u128.min(self: u128, other: u128) -> u128 uses [] panics never pub
fn core.num.u128.nearest_to_f64(self: u128) -> f64 uses [] panics never pub
fn core.num.u128.saturating_add(self: u128, other: u128) -> u128 uses [] panics never pub
fn core.num.u128.saturating_mul(self: u128, other: u128) -> u128 uses [] panics never pub
fn core.num.u128.saturating_sub(self: u128, other: u128) -> u128 uses [] panics never pub
impl core.iter.Step for u128
fn core.num.u128.successor(self: &u128) -> Option<u128> uses [] panics never impls core.iter.Step
fn core.num.u128.trailing_zeros(self: u128) -> u32 uses [] panics never pub
fn core.num.u128.try_from_i128(x: i128) -> Result<u128, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u128.try_from_i16(x: i16) -> Result<u128, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u128.try_from_i32(x: i32) -> Result<u128, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u128.try_from_i64(x: i64) -> Result<u128, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u128.try_from_i8(x: i8) -> Result<u128, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u128.try_from_isize(x: isize) -> Result<u128, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u128.wrapping_add(self: u128, other: u128) -> u128 uses [] panics never pub
fn core.num.u128.wrapping_mul(self: u128, other: u128) -> u128 uses [] panics never pub
fn core.num.u128.wrapping_neg(self: u128) -> u128 uses [] panics never pub
fn core.num.u128.wrapping_shl(self: u128, amount: u32) -> u128 uses [] panics never pub
fn core.num.u128.wrapping_shr(self: u128, amount: u32) -> u128 uses [] panics never pub
fn core.num.u128.wrapping_sub(self: u128, other: u128) -> u128 uses [] panics never pub
fn core.num.u16.checked_add(self: u16, other: u16) -> Option<u16> uses [] panics never pub
fn core.num.u16.checked_div(self: u16, other: u16) -> Option<u16> uses [] panics never pub
fn core.num.u16.checked_mul(self: u16, other: u16) -> Option<u16> uses [] panics never pub
fn core.num.u16.checked_neg(self: u16) -> Option<u16> uses [] panics never pub
fn core.num.u16.checked_rem(self: u16, other: u16) -> Option<u16> uses [] panics never pub
fn core.num.u16.checked_shl(self: u16, amount: u32) -> Option<u16> uses [] panics never pub
fn core.num.u16.checked_shr(self: u16, amount: u32) -> Option<u16> uses [] panics never pub
fn core.num.u16.checked_sub(self: u16, other: u16) -> Option<u16> uses [] panics never pub
fn core.num.u16.count_ones(self: u16) -> u32 uses [] panics never pub
fn core.num.u16.from_str_radix(text: &str, radix: u32) -> Result<u16, core.num.ParseIntError> uses [] panics never pub
fn core.num.u16.from_u8(x: u8) -> u16 uses [] panics never pub
fn core.num.u16.leading_zeros(self: u16) -> u32 uses [] panics never pub
fn core.num.u16.max(self: u16, other: u16) -> u16 uses [] panics never pub
fn core.num.u16.min(self: u16, other: u16) -> u16 uses [] panics never pub
fn core.num.u16.saturating_add(self: u16, other: u16) -> u16 uses [] panics never pub
fn core.num.u16.saturating_mul(self: u16, other: u16) -> u16 uses [] panics never pub
fn core.num.u16.saturating_sub(self: u16, other: u16) -> u16 uses [] panics never pub
impl core.iter.Step for u16
fn core.num.u16.successor(self: &u16) -> Option<u16> uses [] panics never impls core.iter.Step
fn core.num.u16.to_f64(self: u16) -> f64 uses [] panics never pub
fn core.num.u16.trailing_zeros(self: u16) -> u32 uses [] panics never pub
fn core.num.u16.try_from_i128(x: i128) -> Result<u16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u16.try_from_i16(x: i16) -> Result<u16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u16.try_from_i32(x: i32) -> Result<u16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u16.try_from_i64(x: i64) -> Result<u16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u16.try_from_i8(x: i8) -> Result<u16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u16.try_from_isize(x: isize) -> Result<u16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u16.try_from_u128(x: u128) -> Result<u16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u16.try_from_u32(x: u32) -> Result<u16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u16.try_from_u64(x: u64) -> Result<u16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u16.try_from_usize(x: usize) -> Result<u16, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u16.wrapping_add(self: u16, other: u16) -> u16 uses [] panics never pub
fn core.num.u16.wrapping_mul(self: u16, other: u16) -> u16 uses [] panics never pub
fn core.num.u16.wrapping_neg(self: u16) -> u16 uses [] panics never pub
fn core.num.u16.wrapping_shl(self: u16, amount: u32) -> u16 uses [] panics never pub
fn core.num.u16.wrapping_shr(self: u16, amount: u32) -> u16 uses [] panics never pub
fn core.num.u16.wrapping_sub(self: u16, other: u16) -> u16 uses [] panics never pub
fn core.num.u32.checked_add(self: u32, other: u32) -> Option<u32> uses [] panics never pub
fn core.num.u32.checked_div(self: u32, other: u32) -> Option<u32> uses [] panics never pub
fn core.num.u32.checked_mul(self: u32, other: u32) -> Option<u32> uses [] panics never pub
fn core.num.u32.checked_neg(self: u32) -> Option<u32> uses [] panics never pub
fn core.num.u32.checked_rem(self: u32, other: u32) -> Option<u32> uses [] panics never pub
fn core.num.u32.checked_shl(self: u32, amount: u32) -> Option<u32> uses [] panics never pub
fn core.num.u32.checked_shr(self: u32, amount: u32) -> Option<u32> uses [] panics never pub
fn core.num.u32.checked_sub(self: u32, other: u32) -> Option<u32> uses [] panics never pub
fn core.num.u32.count_ones(self: u32) -> u32 uses [] panics never pub
fn core.num.u32.from_str_radix(text: &str, radix: u32) -> Result<u32, core.num.ParseIntError> uses [] panics never pub
fn core.num.u32.from_u16(x: u16) -> u32 uses [] panics never pub
fn core.num.u32.from_u8(x: u8) -> u32 uses [] panics never pub
fn core.num.u32.leading_zeros(self: u32) -> u32 uses [] panics never pub
fn core.num.u32.max(self: u32, other: u32) -> u32 uses [] panics never pub
fn core.num.u32.min(self: u32, other: u32) -> u32 uses [] panics never pub
fn core.num.u32.saturating_add(self: u32, other: u32) -> u32 uses [] panics never pub
fn core.num.u32.saturating_mul(self: u32, other: u32) -> u32 uses [] panics never pub
fn core.num.u32.saturating_sub(self: u32, other: u32) -> u32 uses [] panics never pub
impl core.iter.Step for u32
fn core.num.u32.successor(self: &u32) -> Option<u32> uses [] panics never impls core.iter.Step
fn core.num.u32.to_f64(self: u32) -> f64 uses [] panics never pub
fn core.num.u32.trailing_zeros(self: u32) -> u32 uses [] panics never pub
fn core.num.u32.try_from_i128(x: i128) -> Result<u32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u32.try_from_i16(x: i16) -> Result<u32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u32.try_from_i32(x: i32) -> Result<u32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u32.try_from_i64(x: i64) -> Result<u32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u32.try_from_i8(x: i8) -> Result<u32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u32.try_from_isize(x: isize) -> Result<u32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u32.try_from_u128(x: u128) -> Result<u32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u32.try_from_u64(x: u64) -> Result<u32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u32.try_from_usize(x: usize) -> Result<u32, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u32.wrapping_add(self: u32, other: u32) -> u32 uses [] panics never pub
fn core.num.u32.wrapping_mul(self: u32, other: u32) -> u32 uses [] panics never pub
fn core.num.u32.wrapping_neg(self: u32) -> u32 uses [] panics never pub
fn core.num.u32.wrapping_shl(self: u32, amount: u32) -> u32 uses [] panics never pub
fn core.num.u32.wrapping_shr(self: u32, amount: u32) -> u32 uses [] panics never pub
fn core.num.u32.wrapping_sub(self: u32, other: u32) -> u32 uses [] panics never pub
fn core.num.u64.checked_add(self: u64, other: u64) -> Option<u64> uses [] panics never pub
fn core.num.u64.checked_div(self: u64, other: u64) -> Option<u64> uses [] panics never pub
fn core.num.u64.checked_mul(self: u64, other: u64) -> Option<u64> uses [] panics never pub
fn core.num.u64.checked_neg(self: u64) -> Option<u64> uses [] panics never pub
fn core.num.u64.checked_rem(self: u64, other: u64) -> Option<u64> uses [] panics never pub
fn core.num.u64.checked_shl(self: u64, amount: u32) -> Option<u64> uses [] panics never pub
fn core.num.u64.checked_shr(self: u64, amount: u32) -> Option<u64> uses [] panics never pub
fn core.num.u64.checked_sub(self: u64, other: u64) -> Option<u64> uses [] panics never pub
fn core.num.u64.count_ones(self: u64) -> u32 uses [] panics never pub
fn core.num.u64.from_str_radix(text: &str, radix: u32) -> Result<u64, core.num.ParseIntError> uses [] panics never pub
fn core.num.u64.from_u16(x: u16) -> u64 uses [] panics never pub
fn core.num.u64.from_u32(x: u32) -> u64 uses [] panics never pub
fn core.num.u64.from_u8(x: u8) -> u64 uses [] panics never pub
fn core.num.u64.from_usize(x: usize) -> u64 uses [] panics never pub
fn core.num.u64.leading_zeros(self: u64) -> u32 uses [] panics never pub
fn core.num.u64.max(self: u64, other: u64) -> u64 uses [] panics never pub
fn core.num.u64.min(self: u64, other: u64) -> u64 uses [] panics never pub
fn core.num.u64.nearest_to_f64(self: u64) -> f64 uses [] panics never pub
fn core.num.u64.saturating_add(self: u64, other: u64) -> u64 uses [] panics never pub
fn core.num.u64.saturating_mul(self: u64, other: u64) -> u64 uses [] panics never pub
fn core.num.u64.saturating_sub(self: u64, other: u64) -> u64 uses [] panics never pub
impl core.iter.Step for u64
fn core.num.u64.successor(self: &u64) -> Option<u64> uses [] panics never impls core.iter.Step
fn core.num.u64.trailing_zeros(self: u64) -> u32 uses [] panics never pub
fn core.num.u64.try_from_i128(x: i128) -> Result<u64, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u64.try_from_i16(x: i16) -> Result<u64, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u64.try_from_i32(x: i32) -> Result<u64, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u64.try_from_i64(x: i64) -> Result<u64, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u64.try_from_i8(x: i8) -> Result<u64, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u64.try_from_isize(x: isize) -> Result<u64, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u64.try_from_u128(x: u128) -> Result<u64, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u64.wrapping_add(self: u64, other: u64) -> u64 uses [] panics never pub
fn core.num.u64.wrapping_mul(self: u64, other: u64) -> u64 uses [] panics never pub
fn core.num.u64.wrapping_neg(self: u64) -> u64 uses [] panics never pub
fn core.num.u64.wrapping_shl(self: u64, amount: u32) -> u64 uses [] panics never pub
fn core.num.u64.wrapping_shr(self: u64, amount: u32) -> u64 uses [] panics never pub
fn core.num.u64.wrapping_sub(self: u64, other: u64) -> u64 uses [] panics never pub
fn core.num.u8.checked_add(self: u8, other: u8) -> Option<u8> uses [] panics never pub
fn core.num.u8.checked_div(self: u8, other: u8) -> Option<u8> uses [] panics never pub
fn core.num.u8.checked_mul(self: u8, other: u8) -> Option<u8> uses [] panics never pub
fn core.num.u8.checked_neg(self: u8) -> Option<u8> uses [] panics never pub
fn core.num.u8.checked_rem(self: u8, other: u8) -> Option<u8> uses [] panics never pub
fn core.num.u8.checked_shl(self: u8, amount: u32) -> Option<u8> uses [] panics never pub
fn core.num.u8.checked_shr(self: u8, amount: u32) -> Option<u8> uses [] panics never pub
fn core.num.u8.checked_sub(self: u8, other: u8) -> Option<u8> uses [] panics never pub
fn core.num.u8.count_ones(self: u8) -> u32 uses [] panics never pub
fn core.num.u8.from_str_radix(text: &str, radix: u32) -> Result<u8, core.num.ParseIntError> uses [] panics never pub
fn core.num.u8.leading_zeros(self: u8) -> u32 uses [] panics never pub
fn core.num.u8.max(self: u8, other: u8) -> u8 uses [] panics never pub
fn core.num.u8.min(self: u8, other: u8) -> u8 uses [] panics never pub
fn core.num.u8.saturating_add(self: u8, other: u8) -> u8 uses [] panics never pub
fn core.num.u8.saturating_mul(self: u8, other: u8) -> u8 uses [] panics never pub
fn core.num.u8.saturating_sub(self: u8, other: u8) -> u8 uses [] panics never pub
impl core.iter.Step for u8
fn core.num.u8.successor(self: &u8) -> Option<u8> uses [] panics never impls core.iter.Step
fn core.num.u8.to_f64(self: u8) -> f64 uses [] panics never pub
fn core.num.u8.trailing_zeros(self: u8) -> u32 uses [] panics never pub
fn core.num.u8.try_from_i128(x: i128) -> Result<u8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u8.try_from_i16(x: i16) -> Result<u8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u8.try_from_i32(x: i32) -> Result<u8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u8.try_from_i64(x: i64) -> Result<u8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u8.try_from_i8(x: i8) -> Result<u8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u8.try_from_isize(x: isize) -> Result<u8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u8.try_from_u128(x: u128) -> Result<u8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u8.try_from_u16(x: u16) -> Result<u8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u8.try_from_u32(x: u32) -> Result<u8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u8.try_from_u64(x: u64) -> Result<u8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u8.try_from_usize(x: usize) -> Result<u8, core.num.TryFromIntError> uses [] panics never pub
fn core.num.u8.wrapping_add(self: u8, other: u8) -> u8 uses [] panics never pub
fn core.num.u8.wrapping_mul(self: u8, other: u8) -> u8 uses [] panics never pub
fn core.num.u8.wrapping_neg(self: u8) -> u8 uses [] panics never pub
fn core.num.u8.wrapping_shl(self: u8, amount: u32) -> u8 uses [] panics never pub
fn core.num.u8.wrapping_shr(self: u8, amount: u32) -> u8 uses [] panics never pub
fn core.num.u8.wrapping_sub(self: u8, other: u8) -> u8 uses [] panics never pub
fn core.num.usize.checked_add(self: usize, other: usize) -> Option<usize> uses [] panics never pub
fn core.num.usize.checked_div(self: usize, other: usize) -> Option<usize> uses [] panics never pub
fn core.num.usize.checked_mul(self: usize, other: usize) -> Option<usize> uses [] panics never pub
fn core.num.usize.checked_neg(self: usize) -> Option<usize> uses [] panics never pub
fn core.num.usize.checked_rem(self: usize, other: usize) -> Option<usize> uses [] panics never pub
fn core.num.usize.checked_shl(self: usize, amount: u32) -> Option<usize> uses [] panics never pub
fn core.num.usize.checked_shr(self: usize, amount: u32) -> Option<usize> uses [] panics never pub
fn core.num.usize.checked_sub(self: usize, other: usize) -> Option<usize> uses [] panics never pub
fn core.num.usize.count_ones(self: usize) -> u32 uses [] panics never pub
fn core.num.usize.from_str_radix(text: &str, radix: u32) -> Result<usize, core.num.ParseIntError> uses [] panics never pub
fn core.num.usize.from_u16(x: u16) -> usize uses [] panics never pub
fn core.num.usize.from_u32(x: u32) -> usize uses [] panics never pub
fn core.num.usize.from_u64(x: u64) -> usize uses [] panics never pub
fn core.num.usize.from_u8(x: u8) -> usize uses [] panics never pub
fn core.num.usize.leading_zeros(self: usize) -> u32 uses [] panics never pub
fn core.num.usize.max(self: usize, other: usize) -> usize uses [] panics never pub
fn core.num.usize.min(self: usize, other: usize) -> usize uses [] panics never pub
fn core.num.usize.nearest_to_f64(self: usize) -> f64 uses [] panics never pub
fn core.num.usize.saturating_add(self: usize, other: usize) -> usize uses [] panics never pub
fn core.num.usize.saturating_mul(self: usize, other: usize) -> usize uses [] panics never pub
fn core.num.usize.saturating_sub(self: usize, other: usize) -> usize uses [] panics never pub
impl core.iter.Step for usize
fn core.num.usize.successor(self: &usize) -> Option<usize> uses [] panics never impls core.iter.Step
fn core.num.usize.trailing_zeros(self: usize) -> u32 uses [] panics never pub
fn core.num.usize.try_from_i128(x: i128) -> Result<usize, core.num.TryFromIntError> uses [] panics never pub
fn core.num.usize.try_from_i16(x: i16) -> Result<usize, core.num.TryFromIntError> uses [] panics never pub
fn core.num.usize.try_from_i32(x: i32) -> Result<usize, core.num.TryFromIntError> uses [] panics never pub
fn core.num.usize.try_from_i64(x: i64) -> Result<usize, core.num.TryFromIntError> uses [] panics never pub
fn core.num.usize.try_from_i8(x: i8) -> Result<usize, core.num.TryFromIntError> uses [] panics never pub
fn core.num.usize.try_from_isize(x: isize) -> Result<usize, core.num.TryFromIntError> uses [] panics never pub
fn core.num.usize.try_from_u128(x: u128) -> Result<usize, core.num.TryFromIntError> uses [] panics never pub
fn core.num.usize.wrapping_add(self: usize, other: usize) -> usize uses [] panics never pub
fn core.num.usize.wrapping_mul(self: usize, other: usize) -> usize uses [] panics never pub
fn core.num.usize.wrapping_neg(self: usize) -> usize uses [] panics never pub
fn core.num.usize.wrapping_shl(self: usize, amount: u32) -> usize uses [] panics never pub
fn core.num.usize.wrapping_shr(self: usize, amount: u32) -> usize uses [] panics never pub
fn core.num.usize.wrapping_sub(self: usize, other: usize) -> usize uses [] panics never pub
