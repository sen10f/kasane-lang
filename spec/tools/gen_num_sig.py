#!/usr/bin/env python3
"""core.num の API シグネチャ (spec/stdlib/core/num.sig) を生成する。

整数12種・浮動小数2種の系統的なメソッド族を、仕様書 4.1 / 4.5 の規則
(checked_/wrapping_/saturating_、from_/try_from_/to_ の命名、FQN = core.num.<T>.<名前>) に従って列挙する。
使い方: python -I gen_num_sig.py > spec/stdlib/core/num.sig
"""
import sys
sys.stdout.reconfigure(encoding='utf-8', newline=chr(10))

INTS = [('i8', 8, True), ('i16', 16, True), ('i32', 32, True), ('i64', 64, True), ('i128', 128, True),
        ('isize', 64, True), ('u8', 8, False), ('u16', 16, False), ('u32', 32, False), ('u64', 64, False),
        ('u128', 128, False), ('usize', 64, False)]
FLOATS = [('f32', 'u32', 32), ('f64', 'u64', 64)]
TAIL = 'uses [] panics never pub'

items = []  # (group, key, text)


def fn(fqn, params, ret, extra=''):
    items.append((5, fqn, f'fn {fqn}({params}) -> {ret} {extra}{TAIL}'.replace('  ', ' ')))


def const(fqn, ty, init):
    items.append((1, fqn, f'const {fqn}: {ty} = {init} pub'))


def lossless(s, t):
    (_, sb, ss), (_, tb, ts) = s, t
    if ss == ts:
        return sb <= tb
    if not ss and ts:
        return sb < tb
    return False


for T in INTS:
    n, bits, signed = T
    P = f'core.num.{n}'
    lo = -(1 << (bits - 1)) if signed else 0
    hi = (1 << (bits - 1)) - 1 if signed else (1 << bits) - 1
    const(f'{P}.BITS', 'u32', f'{bits}u32')
    const(f'{P}.MAX', n, f'{hi}{n}')
    const(f'{P}.MIN', n, f'{lo}{n}')
    for op in ('add', 'sub', 'mul', 'div', 'rem'):
        fn(f'{P}.checked_{op}', f'self: {n}, other: {n}', f'Option<{n}>')
    fn(f'{P}.checked_neg', f'self: {n}', f'Option<{n}>')
    fn(f'{P}.checked_shl', f'self: {n}, amount: u32', f'Option<{n}>')
    fn(f'{P}.checked_shr', f'self: {n}, amount: u32', f'Option<{n}>')
    for op in ('add', 'sub', 'mul'):
        fn(f'{P}.wrapping_{op}', f'self: {n}, other: {n}', n)
        fn(f'{P}.saturating_{op}', f'self: {n}, other: {n}', n)
    fn(f'{P}.wrapping_neg', f'self: {n}', n)
    if signed:
        fn(f'{P}.saturating_neg', f'self: {n}', n)
    fn(f'{P}.wrapping_shl', f'self: {n}, amount: u32', n)
    fn(f'{P}.wrapping_shr', f'self: {n}, amount: u32', n)
    fn(f'{P}.min', f'self: {n}, other: {n}', n)
    fn(f'{P}.max', f'self: {n}, other: {n}', n)
    fn(f'{P}.count_ones', f'self: {n}', 'u32')
    fn(f'{P}.leading_zeros', f'self: {n}', 'u32')
    fn(f'{P}.trailing_zeros', f'self: {n}', 'u32')
    fn(f'{P}.from_str_radix', 'text: &str, radix: u32', f'Result<{n}, core.num.ParseIntError>')
    fn(f'{P}.parse', 'text: &str', f'Result<{n}, core.num.ParseIntError>')
    for S in INTS:
        if S[0] == n:
            continue
        if lossless(S, T):
            fn(f'{P}.from_{S[0]}', f'x: {S[0]}', n)
        else:
            fn(f'{P}.try_from_{S[0]}', f'x: {S[0]}', f'Result<{n}, core.num.TryFromIntError>')
    if bits <= 32:
        fn(f'{P}.to_f64', f'self: {n}', 'f64')
    else:
        fn(f'{P}.nearest_to_f64', f'self: {n}', 'f64')
    items.append((5, f'{P}.successor', f'impl core.iter.Step for {n}' + chr(10) + f'fn {P}.successor(self: &{n}) -> Option<{n}> uses [] panics never impls core.iter.Step'))
    if signed:
        fn(f'{P}.checked_abs', f'self: {n}', f'Option<{n}>')

for F, U, bits in FLOATS:
    P = f'core.num.{F}'
    k = {'f64': dict(MAX='0x7fefffffffffffffu64', EPSILON='0x3cb0000000000000u64', MIN_POSITIVE='0x0010000000000000u64',
                     INFINITY='0x7ff0000000000000u64', NEG_INFINITY='0xfff0000000000000u64', NAN='0x7ff8000000000000u64'),
         'f32': dict(MAX='0x7f7fffffu32', EPSILON='0x34000000u32', MIN_POSITIVE='0x00800000u32',
                     INFINITY='0x7f800000u32', NEG_INFINITY='0xff800000u32', NAN='0x7fc00000u32')}[F]
    for name, bitsval in k.items():
        const(f'{P}.{name}', F, f'{F}.from_bits({bitsval})')
    fn(f'{P}.from_bits', f'bits: {U}', F, 'const ')
    fn(f'{P}.to_bits', f'self: {F}', U)
    for m in ('abs', 'floor', 'ceil', 'trunc', 'round_ties_even', 'sqrt', 'exp', 'ln', 'sin', 'cos', 'tan'):
        fn(f'{P}.{m}', f'self: {F}', F)
    for m in ('rem', 'min', 'max', 'atan2'):
        fn(f'{P}.{m}', f'self: {F}, other: {F}', F)
    fn(f'{P}.powi', f'self: {F}, exponent: i32', F)
    fn(f'{P}.is_nan', f'self: {F}', 'bool')
    fn(f'{P}.is_finite', f'self: {F}', 'bool')
    for T in INTS:
        fn(f'{P}.trunc_to_{T[0]}', f'self: {F}', f'Option<{T[0]}>')
        fn(f'{P}.from_{T[0]}_nearest', f'x: {T[0]}', F)
    other = 'f64' if F == 'f32' else 'f32'
    if F == 'f32':
        fn(f'{P}.to_f64', f'self: {F}', 'f64')
    else:
        fn(f'{P}.nearest_to_f32', f'self: {F}', 'f32')

print('// 生成: spec/tools/gen_num_sig.py (手で編集しない)')
ordered = sorted(items, key=lambda x: (x[0], x[1]))
for g, key, text in ordered:
    if g == 1:
        print(text)
print()
print('type core.num.ParseIntError derive [Debug, Clone, Copy, PartialEq, Eq] pub')
print('    kind: u8')
print('end core.num.ParseIntError')
print()
print('type core.num.TryFromIntError derive [Debug, Clone, Copy, PartialEq, Eq] pub')
print('    kind: u8')
print('end core.num.TryFromIntError')
print()
for g, key, text in ordered:
    if g == 5:
        print(text)
