//! KIR の参照実行器 (設計_KIR.md 5 章 段階 1 の「素朴なインタプリタ」)。
//!
//! 目的は、木走査インタプリタ (`interp.rs`) と出力・終了コード・panic の文言が一致することを確かめる基準を作ること。
//! 速さは求めない。呼び出しは明示的なフレーム列で持ち、Rust の再帰を使わない。深さの上限は木走査と同じ 2500 (終了コード 102)。

use std::collections::HashMap;

use crate::kir::{size_of, AggDef, AggKind, Aop, BoolOp, Cc, Field, Mode, Module, Op, Term, Ty, VId};

/// 木走査インタプリタの呼び出し深さの上限 (`interp.rs` の `depth > 2500`)。
const MAX_DEPTH: usize = 2500;

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// 正常終了。値は型に応じた整数 (bool は 0 / 1、unit は 0)。
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

/// 整数の算術。結果は型の範囲内に収まる値か、panic の文言。
fn arith(op: Aop, mode: Mode, ty: crate::kir::Ty, x: i128, y: i128) -> Result<i128, String> {
    if mode == Mode::Wrap {
        let r = match op {
            Aop::Add => x.wrapping_add(y),
            Aop::Sub => x.wrapping_sub(y),
            _ => x.wrapping_mul(y),
        };
        return Ok(ty.wrap(r));
    }
    if matches!(op, Aop::Div | Aop::Rem) && y == 0 {
        // 文言は木走査 (interp.rs の `int_arith`) と同じにする。剰余の文言は木走査の誤植 (`of by zero`) を写している。
        return Err(if op == Aop::Div {
            "attempt to divide by zero".to_string()
        } else {
            "attempt to calculate the remainder with a divisor of by zero".to_string()
        });
    }
    let r = match op {
        Aop::Add => x.checked_add(y),
        Aop::Sub => x.checked_sub(y),
        Aop::Mul => x.checked_mul(y),
        Aop::Div => x.checked_div(y),
        Aop::Rem => x.checked_rem(y),
    };
    match r {
        Some(v) if v >= ty.min() && v <= ty.max() => Ok(v),
        _ => Err(match op {
            Aop::Add => "attempt to add with overflow",
            Aop::Sub => "attempt to subtract with overflow",
            Aop::Mul => "attempt to multiply with overflow",
            Aop::Div => "attempt to divide with overflow",
            Aop::Rem => "attempt to calculate the remainder with overflow",
        }
        .to_string()),
    }
}

fn compare(cc: Cc, x: i128, y: i128) -> bool {
    match cc {
        Cc::Eq => x == y,
        Cc::Ne => x != y,
        Cc::Lt => x < y,
        Cc::Le => x <= y,
        Cc::Gt => x > y,
        Cc::Ge => x >= y,
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
    let enter = |f: usize, args: &[i128], dst: Option<VId>, hbase: usize| -> Frame {
        let mut vals = vec![0i128; counts[f]];
        for ((v, _), a) in m.funcs[f].params().iter().zip(args.iter()) {
            vals[*v as usize] = *a;
        }
        Frame { f, blk: 0, ip: 0, vals, dst, hbase }
    };
    let mut stack = vec![enter(entry, args, None, 0)];
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
                Op::Cmp { cc, a, b, .. } => compare(*cc, get(&stack[top], a), get(&stack[top], b)) as i128,
                Op::Neg { a } => {
                    let x = get(&stack[top], a);
                    if x == inst.ty.min() {
                        return Ok(Outcome::Panic("attempt to negate with overflow".to_string()));
                    }
                    -x
                }
                Op::Not(a) => (get(&stack[top], a) == 0) as i128,
                Op::Bool(bop, a, b) => {
                    let (x, y) = (get(&stack[top], a) != 0, get(&stack[top], b) != 0);
                    let r = match bop {
                        BoolOp::And => x & y,
                        BoolOp::Or => x | y,
                        BoolOp::Xor => x ^ y,
                    };
                    r as i128
                }
                Op::Call { callee, args } => {
                    if stack.len() >= MAX_DEPTH {
                        return Ok(Outcome::StackOverflow);
                    }
                    let Some(&ci) = index.get(callee.as_str()) else {
                        return Err(format!("呼び先 {} がない", callee));
                    };
                    let argv: Vec<i128> = args.iter().map(|a| get(&stack[top], a)).collect();
                    stack.push(enter(ci, &argv, Some(inst.dst), heap.len()));
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
                    let h = get(&stack[top], a) as usize;
                    i32::from_le_bytes([heap[h][0], heap[h][1], heap[h][2], heap[h][3]]) as i128
                }
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
    use crate::kir::{Block, Func, Inst, Ty};

    #[test]
    fn arith_matches_interpreter_messages() {
        assert_eq!(arith(Aop::Add, Mode::Trap, Ty::U8, 200, 100), Err("attempt to add with overflow".into()));
        assert_eq!(arith(Aop::Sub, Mode::Trap, Ty::U32, 0, 1), Err("attempt to subtract with overflow".into()));
        assert_eq!(arith(Aop::Div, Mode::Trap, Ty::I8, 1, 0), Err("attempt to divide by zero".into()));
        assert_eq!(arith(Aop::Div, Mode::Trap, Ty::I8, -128, -1), Err("attempt to divide with overflow".into()));
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I8, 1, 0), Err("attempt to calculate the remainder with a divisor of by zero".into()));
        assert_eq!(arith(Aop::Mul, Mode::Wrap, Ty::U8, 16, 17), Ok(16));
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
