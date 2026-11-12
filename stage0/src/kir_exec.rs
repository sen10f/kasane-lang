//! KIR の参照実行器 (設計_KIR.md 5 章 段階 1 の「素朴なインタプリタ」)。
//!
//! 目的は、木走査インタプリタ (`interp.rs`) と出力・終了コード・panic の文言が一致することを確かめる基準を作ること。
//! 速さは求めない。呼び出しは明示的なフレーム列で持ち、Rust の再帰を使わない。深さの上限は木走査と同じ 2500 (終了コード 102)。
//! 浮動小数と 128 ビット整数の規則は `kir_scalar.rs` にある。

use std::collections::HashMap;

use crate::kir::{payload_bits, payload_of, size_of, AggDef, AggKind, Aop, BoolOp, Field, Mode, Module, Op, Term, Ty, TAG_BIT, VId};
use crate::kir_scalar;

/// 木走査インタプリタの呼び出し深さの上限 (`interp.rs` の `depth > 2500`)。
const MAX_DEPTH: usize = 2500;

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// 正常終了。値は型に応じた符号化 (整数はその値、`u128` はビットパターン、浮動小数はビット列、bool は 0 / 1、unit は 0、Option / Result はタグ bit とペイロード。`show_value` で読む)。
    Value(i128),
    /// 実行時 panic (文言は木走査と同じ)。
    Panic(String),
    /// 呼び出し深さの超過 (木走査と同じく終了コード 102)。
    StackOverflow,
}

struct Frame {
    f: usize,
    blk: usize,
    /// 次に実行する命令の番号。
    ip: usize,
    vals: Vec<i128>,
    /// 呼び出し元で結果を受ける値番号。
    dst: Option<VId>,
    /// この呼び出しが始まったときの集成体の置き場の長さ (戻るときにここまで切り詰める)。
    hbase: usize,
    /// この呼び出しが始まったときのセル列の長さ (返るときに切り詰める)。
    mem_base: usize,
}

/// 集成体の値の読み出し: `t` の値を、バイト列 `buf` の `off` から取る (集成体は置き場に複製して、その添字を返す)。
fn read_scalar(buf: &[u8], off: usize, t: Ty) -> i128 {
    match t {
        Ty::Bool => (buf[off] != 0) as i128,
        Ty::Unit => 0,
        _ => {
            let n = size_of(&[], t) as usize;
            let mut raw = [0u8; 16];
            raw[..n].copy_from_slice(&buf[off..off + n]);
            t.wrap(u128::from_le_bytes(raw) as i128)
        }
    }
}

const DIV_ZERO: &str = "attempt to divide by zero";
const REM_ZERO: &str = "attempt to calculate the remainder with a divisor of by zero";

/// `u128` の算術 (値はビットパターン)。
fn arith_u128(op: Aop, mode: Mode, x: u128, y: u128) -> Result<u128, String> {
    match mode {
        Mode::Wrap => Ok(match op {
            Aop::Add => x.wrapping_add(y),
            Aop::Sub => x.wrapping_sub(y),
            _ => x.wrapping_mul(y),
        }),
        Mode::Sat => Ok(match op {
            Aop::Add => x.saturating_add(y),
            Aop::Sub => x.saturating_sub(y),
            _ => x.saturating_mul(y),
        }),
        Mode::Trap => {
            if matches!(op, Aop::Div | Aop::Rem) && y == 0 {
                return Err(if op == Aop::Div { DIV_ZERO.to_string() } else { REM_ZERO.to_string() });
            }
            let r = match op {
                Aop::Add => x.checked_add(y),
                Aop::Sub => x.checked_sub(y),
                Aop::Mul => x.checked_mul(y),
                Aop::Div => x.checked_div(y),
                Aop::Rem => x.checked_rem(y),
            };
            r.ok_or_else(|| overflow_msg(op))
        }
    }
}

/// 値 `v` (スカラー、または置き場の添字の集成体) を、バイト列 `buf` の `off` に書く。
fn write_val(buf: &mut [u8], off: usize, aggs: &[AggDef], t: Ty, v: i128, heap: &[Vec<u8>]) {
    if let Ty::Agg(_) = t {
        let src = &heap[v as usize];
        buf[off..off + src.len()].copy_from_slice(src);
    } else {
        let n = size_of(aggs, t) as usize;
        let bytes = (v as u128).to_le_bytes();
        buf[off..off + n].copy_from_slice(&bytes[..n]);
    }
}

/// 構造体・タプルのオフセット `off` のフィールドの型。
fn field_ty_at(d: &AggDef, off: u32) -> Option<Ty> {
    match &d.kind {
        AggKind::Struct(fs) | AggKind::Tuple(fs) => fs.iter().find(|f: &&Field| f.off == off).map(|f| f.ty),
        AggKind::Enum(_) => None,
    }
}

fn overflow_msg(op: Aop) -> String {
    match op {
        Aop::Add => "attempt to add with overflow",
        Aop::Sub => "attempt to subtract with overflow",
        Aop::Mul => "attempt to multiply with overflow",
        Aop::Div => "attempt to divide with overflow",
        Aop::Rem => "attempt to calculate the remainder with overflow",
    }
    .to_string()
}

/// 整数の算術。結果は型の範囲内に収まる値か、panic の文言。
fn arith(op: Aop, mode: Mode, ty: Ty, x: i128, y: i128) -> Result<i128, String> {
    if ty == Ty::U128 {
        return arith_u128(op, mode, x as u128, y as u128).map(|v| v as i128);
    }
    if mode == Mode::Wrap {
        let r = match op {
            Aop::Add => x.wrapping_add(y),
            Aop::Sub => x.wrapping_sub(y),
            _ => x.wrapping_mul(y),
        };
        return Ok(ty.wrap(r));
    }
    if mode == Mode::Sat {
        // 16 ビット以下と i128 の両方を扱う。i128 の溢れは結果の符号で飽和する方向を決める (和と差は x の符号、積は符号の積)。
        let r = match op {
            Aop::Add => x.checked_add(y),
            Aop::Sub => x.checked_sub(y),
            _ => x.checked_mul(y),
        };
        let neg = match op {
            Aop::Add | Aop::Sub => x < 0,
            _ => (x < 0) != (y < 0),
        };
        return Ok(match r {
            Some(v) => v.clamp(ty.min(), ty.max()),
            None => {
                if neg {
                    ty.min()
                } else {
                    ty.max()
                }
            }
        });
    }
    if matches!(op, Aop::Div | Aop::Rem) && y == 0 {
        // 文言は木走査 (interp.rs の `int_arith`) と同じにする。剰余の文言は木走査の誤植 (`of by zero`) を写している。
        return Err(if op == Aop::Div { DIV_ZERO.to_string() } else { REM_ZERO.to_string() });
    }
    let r = match op {
        Aop::Add => x.checked_add(y),
        Aop::Sub => x.checked_sub(y),
        Aop::Mul => x.checked_mul(y),
        Aop::Div => x.checked_div(y),
        Aop::Rem => checked_rem(ty, x, y),
    };
    match r {
        Some(v) if ty.in_range(v) => Ok(v),
        _ => Err(overflow_msg(op)),
    }
}

/// 整数の `checked_rem` (案 A)。符号付きの `MIN % -1` は割り算が溢れるので余りも `None`。
/// i128 の `checked_rem` はこの組を溢れとして返さないので、型の最小値で別に判定する。
fn checked_rem(ty: Ty, x: i128, y: i128) -> Option<i128> {
    if ty.signed() && y == -1 && x == ty.min() {
        return None;
    }
    x.checked_rem(y)
}

/// `checked_<op>` の結果 (`Option<P>` の符号化)。溢れと 0 除算は `None` (= 0)。
/// 符号付きの `MIN / -1` と `MIN % -1` は溢れで `None` (案 A。木走査 `interp.rs` と同じ)。
fn checked(op: Aop, t: crate::kir::Ty, x: i128, y: i128) -> i128 {
    let crate::kir::Ty::Opt(p) = t else { return 0 };
    let pt = p.ty();
    if matches!(op, Aop::Div | Aop::Rem) && y == 0 {
        return 0;
    }
    let r = match op {
        Aop::Add => x.checked_add(y),
        Aop::Sub => x.checked_sub(y),
        Aop::Mul => x.checked_mul(y),
        Aop::Div => x.checked_div(y),
        Aop::Rem => checked_rem(pt, x, y),
    };
    match r {
        Some(v) if v >= pt.min() && v <= pt.max() => TAG_BIT | payload_bits(v),
        _ => 0,
    }
}

/// 鍵 `key` の関数を引数 `args` で実行する。鍵が無い・引数の個数が違う場合は `Err`。
pub fn run(m: &Module, key: &str, args: &[i128]) -> Result<Outcome, String> {
    let index: HashMap<&str, usize> = m.funcs.iter().enumerate().map(|(i, f)| (f.key.as_str(), i)).collect();
    let Some(&entry) = index.get(key) else {
        return Err(format!("関数 {} がない", key));
    };
    if args.len() != m.funcs[entry].params().len() {
        return Err(format!("関数 {} の引数の個数が違う", key));
    }
    let counts: Vec<usize> = m.funcs.iter().map(|f| f.value_count()).collect();
    // 集成体の置き場: 集成体の値は、ここにバイト列として置き、値 (i128) はその添字で表す。値は不変。
    let mut heap: Vec<Vec<u8>> = vec![];
    if matches!(m.funcs[entry].ret, Ty::Agg(_)) || m.funcs[entry].params().iter().any(|(_, t)| matches!(t, Ty::Agg(_))) {
        return Err(format!("関数 {} は集成体を引数・戻り値に持つ (この入口では扱えない)", key));
    }
    let enter = |f: usize, args: &[i128], dst: Option<VId>, hbase: usize, mem_base: usize| -> Frame {
        let mut vals = vec![0i128; counts[f]];
        for ((v, _), a) in m.funcs[f].params().iter().zip(args.iter()) {
            vals[*v as usize] = *a;
        }
        Frame { f, blk: 0, ip: 0, vals, dst, hbase, mem_base }
    };
    // 参照の指すセル。`alloca` が 1 セル確保し、その番号がアドレスになる。呼び出しの終わりに切り詰める。
    let mut mem: Vec<i128> = vec![];
    let mut stack = vec![enter(entry, args, None, 0, 0)];
    loop {
        let top = stack.len() - 1;
        let (fi, bi, ip) = (stack[top].f, stack[top].blk, stack[top].ip);
        let func = &m.funcs[fi];
        let blk = &func.blocks[bi];
        if ip < blk.insts.len() {
            stack[top].ip += 1;
            let inst = &blk.insts[ip];
            let get = |fr: &Frame, v: &VId| fr.vals[*v as usize];
            let result: i128 = match &inst.op {
                Op::Const(v) => *v,
                Op::Arith { op, mode, a, b } => {
                    let (x, y) = (get(&stack[top], a), get(&stack[top], b));
                    match arith(*op, *mode, inst.ty, x, y) {
                        Ok(v) => v,
                        Err(msg) => return Ok(Outcome::Panic(msg)),
                    }
                }
                Op::FArith { op, a, b } => kir_scalar::farith(*op, inst.ty, get(&stack[top], a), get(&stack[top], b)),
                Op::Cmp { cc, ty, a, b } => kir_scalar::cmp(*cc, *ty, get(&stack[top], a), get(&stack[top], b)) as i128,
                Op::Shift { left, a, n } => kir_scalar::shift(*left, inst.ty, get(&stack[top], a), get(&stack[top], n)),
                Op::MinMax { max, a, b } => kir_scalar::minmax(*max, inst.ty, get(&stack[top], a), get(&stack[top], b)),
                Op::FUn { f, src, a } => kir_scalar::funary(*f, *src, inst.ty, get(&stack[top], a)),
                Op::FBin { f, a, b } => kir_scalar::fbin(*f, inst.ty, get(&stack[top], a), get(&stack[top], b)),
                Op::Conv { src, a } => kir_scalar::conv(*src, inst.ty, get(&stack[top], a)),
                Op::Bitcast { a, .. } => get(&stack[top], a),
                Op::BitCount { k, src, a } => kir_scalar::bitcount(*k, *src, get(&stack[top], a)),
                Op::Neg { a, mode } => {
                    let x = get(&stack[top], a);
                    match mode {
                        Mode::Wrap => kir_scalar::wrap_neg(inst.ty, x),
                        _ => {
                            if x == inst.ty.min() {
                                return Ok(Outcome::Panic("attempt to negate with overflow".to_string()));
                            }
                            -x
                        }
                    }
                }
                Op::Not(a) => {
                    let x = get(&stack[top], a);
                    if inst.ty == Ty::Bool {
                        (x == 0) as i128
                    } else {
                        kir_scalar::not_int(inst.ty, x)
                    }
                }
                Op::Bool(bop, a, b) => {
                    // bool は 0 / 1 なので、ビット演算がそのまま論理演算になる。整数は符号拡張した 1 語なので、型の範囲の値を保つ。
                    let (x, y) = (get(&stack[top], a), get(&stack[top], b));
                    match bop {
                        BoolOp::And => x & y,
                        BoolOp::Or => x | y,
                        BoolOp::Xor => x ^ y,
                    }
                }
                Op::Some(a) => TAG_BIT | payload_bits(get(&stack[top], a)),
                Op::Ok(a) => payload_bits(get(&stack[top], a)),
                Op::Err(a) => TAG_BIT | payload_bits(get(&stack[top], a)),
                Op::Payload(a) => payload_of(inst.ty, get(&stack[top], a)),
                Op::Checked { op, a, b } => checked(*op, inst.ty, get(&stack[top], a), get(&stack[top], b)),
                Op::Call { callee, args } => {
                    if stack.len() >= MAX_DEPTH {
                        return Ok(Outcome::StackOverflow);
                    }
                    let Some(&ci) = index.get(callee.as_str()) else {
                        return Err(format!("呼び先 {} がない", callee));
                    };
                    let argv: Vec<i128> = args.iter().map(|a| get(&stack[top], a)).collect();
                    stack.push(enter(ci, &argv, Some(inst.dst), heap.len(), mem.len()));
                    continue;
                }
                Op::Make { variant, fields } => {
                    let Ty::Agg(ai) = inst.ty else {
                        return Err("make の結果が集成体でない".to_string());
                    };
                    let d = &m.aggs[ai as usize];
                    let mut buf = vec![0u8; d.size as usize];
                    let fdefs: &[Field] = match (&d.kind, variant) {
                        (AggKind::Enum(vs), Some(k)) => {
                            buf[0..4].copy_from_slice(&(*k as i32).to_le_bytes());
                            &vs[*k as usize].fields
                        }
                        (AggKind::Struct(fs) | AggKind::Tuple(fs), None) => fs,
                        _ => return Err("make の変種が集成体の種類と合わない".to_string()),
                    };
                    for (fd, a) in fdefs.iter().zip(fields) {
                        let v = get(&stack[top], a);
                        write_val(&mut buf, fd.off as usize, &m.aggs, fd.ty, v, &heap);
                    }
                    heap.push(buf);
                    (heap.len() - 1) as i128
                }
                Op::Field { a, off } => {
                    let h = get(&stack[top], a) as usize;
                    let o = *off as usize;
                    match inst.ty {
                        Ty::Agg(ai) => {
                            let n = m.aggs[ai as usize].size as usize;
                            let sub = heap[h][o..o + n].to_vec();
                            heap.push(sub);
                            (heap.len() - 1) as i128
                        }
                        t => read_scalar(&heap[h], o, t),
                    }
                }
                Op::SetField { a, off, v } => {
                    let h = get(&stack[top], a) as usize;
                    let Ty::Agg(ai) = inst.ty else {
                        return Err("setfield の結果が集成体でない".to_string());
                    };
                    let d = &m.aggs[ai as usize];
                    let Some(fty) = field_ty_at(d, *off) else {
                        return Err("setfield のフィールドがない".to_string());
                    };
                    let mut buf = heap[h].clone();
                    let vv = get(&stack[top], v);
                    write_val(&mut buf, *off as usize, &m.aggs, fty, vv, &heap);
                    heap.push(buf);
                    (heap.len() - 1) as i128
                }
                Op::Tag(a) => {
                    // 結果の型で区別する: i32 なら集成体の列挙型 (置き場の添字)、bool なら Option / Result (タグのビット)。
                    let v = get(&stack[top], a);
                    if inst.ty == Ty::I32 {
                        let h = v as usize;
                        i32::from_le_bytes([heap[h][0], heap[h][1], heap[h][2], heap[h][3]]) as i128
                    } else {
                        (v & TAG_BIT != 0) as i128
                    }
                }
                Op::Alloca => {
                    mem.push(0);
                    (mem.len() - 1) as i128
                }
                Op::Load(p) => mem[get(&stack[top], p) as usize],
                Op::Store { ptr, val } => {
                    let (a, v) = (get(&stack[top], ptr), get(&stack[top], val));
                    mem[a as usize] = v;
                    0
                }
                Op::AsShared(p) | Op::Reborrow(p) => get(&stack[top], p),
            };
            stack[top].vals[inst.dst as usize] = result;
            continue;
        }
        // 終端命令
        match &blk.term {
            Term::Br((t, targs)) => {
                jump(&mut stack[top], m, fi, *t, targs);
            }
            Term::BrIf(c, (t1, a1), (t2, a2)) => {
                if stack[top].vals[*c as usize] != 0 {
                    jump(&mut stack[top], m, fi, *t1, a1);
                } else {
                    jump(&mut stack[top], m, fi, *t2, a2);
                }
            }
            Term::Ret(v) => {
                let mut val = stack[top].vals[*v as usize];
                let done = stack.pop().expect("フレーム");
                // 戻る関数が作った集成体は捨てる。戻り値の集成体だけは、呼び出し元の置き場へ複製して残す。
                if let Ty::Agg(_) = func.ret {
                    let blob = heap[val as usize].clone();
                    heap.truncate(done.hbase);
                    heap.push(blob);
                    val = (heap.len() - 1) as i128;
                } else {
                    heap.truncate(done.hbase);
                }
                mem.truncate(done.mem_base);
                match stack.last_mut() {
                    None => return Ok(Outcome::Value(val)),
                    Some(caller) => {
                        if let Some(d) = done.dst {
                            caller.vals[d as usize] = val;
                        }
                    }
                }
            }
            Term::Unreachable => return Err("到達しないブロックに入った".to_string()),
        }
    }
}

/// 移動先のブロックへ入る。引数は移動の前の値で受け取る (並列代入)。
fn jump(fr: &mut Frame, m: &Module, fi: usize, t: usize, args: &[VId]) {
    let vals: Vec<i128> = args.iter().map(|a| fr.vals[*a as usize]).collect();
    let params = &m.funcs[fi].blocks[t].params;
    for ((p, _), v) in params.iter().zip(vals) {
        fr.vals[*p as usize] = v;
    }
    fr.blk = t;
    fr.ip = 0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kir::{Block, Func, Inst, Pay, Ty};

    #[test]
    fn arith_matches_interpreter_messages() {
        assert_eq!(arith(Aop::Add, Mode::Trap, Ty::U8, 200, 100), Err("attempt to add with overflow".into()));
        assert_eq!(arith(Aop::Sub, Mode::Trap, Ty::U32, 0, 1), Err("attempt to subtract with overflow".into()));
        assert_eq!(arith(Aop::Div, Mode::Trap, Ty::I8, 1, 0), Err("attempt to divide by zero".into()));
        assert_eq!(arith(Aop::Div, Mode::Trap, Ty::I8, -128, -1), Err("attempt to divide with overflow".into()));
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I8, 1, 0), Err("attempt to calculate the remainder with a divisor of by zero".into()));
        assert_eq!(arith(Aop::Mul, Mode::Wrap, Ty::U8, 16, 17), Ok(16));
    }

    #[test]
    fn min_rem_minus_one_overflows_for_every_signed_width() {
        // 案 A: 符号付きの `MIN % -1` は割り算が溢れるので余りも溢れ (`checked_rem` は None)。
        let rem = "attempt to calculate the remainder with overflow";
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I8, -128, -1), Err(rem.into()));
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I16, i16::MIN as i128, -1), Err(rem.into()));
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I32, i32::MIN as i128, -1), Err(rem.into()));
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I64, i64::MIN as i128, -1), Err(rem.into()));
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I128, i128::MIN, -1), Err(rem.into()));
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I8, -7, 2), Ok(-1));
        assert_eq!(checked(Aop::Rem, Ty::Opt(Pay::I8), -128, -1), 0);
    }

    #[test]
    fn arith_saturates_and_handles_128_bits() {
        assert_eq!(arith(Aop::Add, Mode::Sat, Ty::U8, 250, 10), Ok(255));
        assert_eq!(arith(Aop::Sub, Mode::Sat, Ty::U8, 3, 10), Ok(0));
        assert_eq!(arith(Aop::Add, Mode::Sat, Ty::I128, i128::MIN, i128::MIN), Ok(i128::MIN));
        assert_eq!(arith(Aop::Add, Mode::Sat, Ty::I128, i128::MAX, 1), Ok(i128::MAX));
        assert_eq!(arith(Aop::Mul, Mode::Sat, Ty::I128, i128::MIN, 2), Ok(i128::MIN));
        // u128 の最大値 (-1 のビットパターン) に 1 を足すと panic、wrap は 0
        assert_eq!(arith(Aop::Add, Mode::Trap, Ty::U128, -1, 1), Err("attempt to add with overflow".into()));
        assert_eq!(arith(Aop::Add, Mode::Wrap, Ty::U128, -1, 1), Ok(0));
    }

    /// `fn f(n: u64) -> u64`: 引数を 1 つ減らして呼ぶ再帰 (深さの検査用)。
    fn deep() -> Module {
        let f = Func {
            fqn: "t.deep".into(),
            key: "t.deep".into(),
            uses: vec![],
            may_panic: false,
            stack: None,
            ret: Ty::U64,
            blocks: vec![Block {
                params: vec![(0, Ty::U64)],
                insts: vec![
                    Inst { dst: 1, ty: Ty::U64, op: Op::Const(1) },
                    Inst { dst: 2, ty: Ty::U64, op: Op::Call { callee: "t.deep".into(), args: vec![0] } },
                ],
                term: Term::Ret(2),
            }],
        };
        Module { funcs: vec![f], ..Default::default() }
    }

    #[test]
    fn deep_recursion_reports_stack_overflow() {
        assert_eq!(run(&deep(), "t.deep", &[1]), Ok(Outcome::StackOverflow));
    }
}
