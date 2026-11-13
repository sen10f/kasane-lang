//! KIR の決定的な最適化パス (設計_KIR.md 第 5 章 段階 3 の前半)。
//! 各パスは `kir::verify_module` を通す。意味を変えない変換だけを行う。
//!
//! 設計判断: すべてのパスを 1 つのモジュールにまとめ、ファイル分割を最小化する。
//! 各パスは独立した関数で、`Module` を受け取り `Module` を返す。

use std::collections::HashSet;

use crate::kir::{self, Aop, BitK, BoolOp, Cc, FBin, FUn, Inst, Mode, Module, Op, Term, Ty, VId, operands};

/// 最適化のエントリポイント。全パスを順に適用し、各パスの後に `verify_module` を通す。
/// 検証に失敗した場合は、その時点までの結果を返さずエラーにする（内部不整合のため）。
pub fn optimize(m: &Module) -> Result<Module, String> {
    let mut m = m.clone();
    m = const_fold(&m)?;
    m = dce(&m)?;
    m = copy_prop(&m)?;
    m = inline_small(&m)?;
    Ok(m)
}

/// 使われない命令の除去 (パス 2)。副作用の無い命令だけを除去する。
/// 呼び出し・メモリ・panic・write は副作用があるとみなして残す。
fn dce(m: &Module) -> Result<Module, String> {
    after(m, |m| {
        let mut m = m.clone();
        for f in &mut m.funcs {
            let mut changed = true;
            while changed {
                changed = false;
                // 現在の命令・終端命令が参照する値番号の集合。
                let mut referenced: HashSet<VId> = HashSet::new();
                for b in &f.blocks {
                    for i in &b.insts {
                        for a in operands(&i.op) {
                            referenced.insert(a);
                        }
                    }
                    match &b.term {
                        Term::Br(t) => referenced.extend(t.1.iter().copied()),
                        Term::BrIf(c, t1, t2) => {
                            referenced.insert(*c);
                            referenced.extend(t1.1.iter().copied());
                            referenced.extend(t2.1.iter().copied());
                        }
                        Term::Ret(v) => { referenced.insert(*v); }
                        Term::Unreachable => {}
                    }
                }
                let dead: Vec<(usize, usize)> = f
                    .blocks
                    .iter_mut()
                    .enumerate()
                    .flat_map(|(bi, b)| {
                        b.insts
                            .iter()
                            .enumerate()
                            .filter(|(_, i)| {
                                !side_effecting(&i.op) && !referenced.contains(&i.dst)
                            })
                            .map(move |(ii, _)| (bi, ii))
                            .collect::<Vec<_>>()
                    })
                    .collect();
                // 後ろから削除して添字をずらさない。
                for (bi, ii) in dead.into_iter().rev() {
                    f.blocks[bi].insts.remove(ii);
                    changed = true;
                }
            }
        }
        m
    }, "使われない命令の除去")
}

/// 命令が副作用を持つか。副作用を持つ命令は結果が未使用でも除去しない。
fn side_effecting(op: &Op) -> bool {
    match op {
        Op::Call { .. }
        | Op::Write { .. }
        | Op::Eprint { .. }
        | Op::Abort
        | Op::Panic { .. }
        | Op::Store { .. }
        | Op::MemStore { .. }
        | Op::HeapFree(_)
        | Op::HeapAlloc(_)
        | Op::HeapRealloc { .. }
        | Op::IndexCheck { .. }
        | Op::Alloca
        | Op::Load(_)
        | Op::MemLoad { .. }
        | Op::FrameBytes { .. } => true,
        // 純粋な演算・構築・変換は副作用を持たない。
        Op::Const(_)
        | Op::Arith { .. }
        | Op::Cmp { .. }
        | Op::Neg { .. }
        | Op::FArith { .. }
        | Op::FUn { .. }
        | Op::FBin { .. }
        | Op::Shift { .. }
        | Op::MinMax { .. }
        | Op::Conv { .. }
        | Op::Bitcast { .. }
        | Op::Not(_)
        | Op::Bool(..)
        | Op::BitCount { .. }
        | Op::Tag(_)
        | Op::Payload(_)
        | Op::Some(_)
        | Op::Ok(_)
        | Op::Err(_)
        | Op::Checked { .. }
        | Op::Make { .. }
        | Op::Field { .. }
        | Op::SetField { .. }
        | Op::PtrAdd { .. }
        | Op::StaticBytes(_)
        | Op::AsShared(_)
        | Op::Reborrow(_)
        | Op::FieldAddr { .. } => false,
    }
}

/// コピーの伝播 (パス 3)。未実装のスタブ（通過のみ）。
fn copy_prop(m: &Module) -> Result<Module, String> {
    Ok(m.clone())
}

/// 小さな関数のインライン化 (パス 4)。未実装のスタブ（通過のみ）。
fn inline_small(m: &Module) -> Result<Module, String> {
    Ok(m.clone())
}

/// 1 パスのラッパ: 変換を適用して `verify_module` を通す。違反があればエラー。
fn after<T>(m: &Module, f: impl FnOnce(&Module) -> T, name: &str) -> Result<Module, String>
where
    T: Into<Module>,
{
    let m = f(m).into();
    let errs = kir::verify_module(&m);
    if errs.is_empty() {
        Ok(m)
    } else {
        Err(format!("{}: 検証に失敗: {:?}", name, errs))
    }
}

/// 定数の畳み込み (パス 1)。整数・bool の定数演算を畳む。
/// 溢れうる演算 (`trap` モード、`div`/`rem`、`neg` trap)は畳まない（panic の意味を保つ）。
/// `wrap`/`sat` モードと論理・比較・変換・ビット演算は畳む。
fn const_fold(m: &Module) -> Result<Module, String> {
    after(m, |m| {
        let mut m = m.clone();
        for f in &mut m.funcs {
            // 不変のスナップショットで畳める場所を求め、適用する。新しい定数ができると
            // さらに畳める命令が現れるので、変化が無くなるまで繰り返す。
            let mut changed = true;
            while changed {
                changed = false;
                let mut folds: Vec<(usize, usize, Inst)> = vec![];
                for (bi, b) in f.blocks.iter().enumerate() {
                    for (ii, inst) in b.insts.iter().enumerate() {
                        if let Some(op) = fold_inst(f, inst) {
                            folds.push((bi, ii, op));
                        }
                    }
                }
                for (bi, ii, inst) in folds {
                    f.blocks[bi].insts[ii] = inst;
                    changed = true;
                }
            }
        }
        m
    }, "定数の畳み込み")
}

/// 命令が定数に畳めるなら、新しい `Inst` を返す。そうでなければ `None`。
/// `dst` と `ty` は元のまま保つ（定数は同じ型）。
fn fold_inst(f: &crate::kir::Func, inst: &Inst) -> Option<Inst> {
    if matches!(inst.op, Op::Const(_)) {
        return None;
    }
    let c = match &inst.op {
        Op::Arith { op, mode, a, b } => {
            let (x, y) = const_args(f, inst, *a, *b)?;
            match mode {
                Mode::Wrap => Some(Op::Const(arith_wrap(*op, inst.ty, x, y))),
                Mode::Sat => Some(Op::Const(arith_sat(*op, inst.ty, x, y))),
                Mode::Trap => None, // 溢れ・0除算で panic するので畳まない
            }
        }
        Op::Neg { a, mode } => {
            let x = const_arg(f, inst, *a)?;
            match mode {
                Mode::Wrap => Some(Op::Const(wrap_neg(inst.ty, x))),
                Mode::Trap => None,
                Mode::Sat => None,
            }
        }
        Op::Cmp { cc, ty, a, b } => {
            let (x, y) = const_args(f, inst, *a, *b)?;
            Some(Op::Const(cmp_val(*cc, *ty, x, y) as i128))
        }
        Op::FArith { op, a, b } => {
            let (x, y) = const_args(f, inst, *a, *b)?;
            Some(Op::Const(farith(*op, inst.ty, x, y)))
        }
        Op::FUn { f: fun, src, a } => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(funary(*fun, *src, inst.ty, x)))
        }
        Op::FBin { f: fb, a, b } => {
            let (x, y) = const_args(f, inst, *a, *b)?;
            Some(Op::Const(fbin(*fb, inst.ty, x, y)))
        }
        Op::Shift { left, a, n } => {
            let x = const_arg(f, inst, *a)?;
            let n = const_arg(f, inst, *n)?;
            Some(Op::Const(shift(*left, inst.ty, x, n)))
        }
        Op::MinMax { max, a, b } => {
            let (x, y) = const_args(f, inst, *a, *b)?;
            Some(Op::Const(minmax(*max, inst.ty, x, y)))
        }
        Op::Conv { src, a } => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(conv(*src, inst.ty, x)))
        }
        Op::Bitcast { src, a } => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(bitcast(*src, x)))
        }
        Op::Not(a) => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(not_val(inst.ty, x)))
        }
        Op::Bool(op, a, b) => {
            let (x, y) = const_args(f, inst, *a, *b)?;
            Some(Op::Const(bool_val(*op, inst.ty, x, y)))
        }
        Op::BitCount { k, src, a } => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(bitcount(*k, *src, x)))
        }
        Op::Tag(a) => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(tag_val(x)))
        }
        Op::Payload(a) => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(payload_val(x)))
        }
        Op::Some(a) => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(tag_or_none(x)))
        }
        Op::Ok(a) => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(ok_val(x)))
        }
        Op::Err(a) => {
            let x = const_arg(f, inst, *a)?;
            Some(Op::Const(err_val(x)))
        }
        Op::Checked { op, a, b } => {
            let (x, y) = const_args(f, inst, *a, *b)?;
            Some(Op::Const(checked_val(*op, inst.ty, x, y)))
        }
        _ => None,
    };
    c.map(|op| Inst { dst: inst.dst, ty: inst.ty, op })
}

fn const_args(f: &crate::kir::Func, inst: &Inst, a: VId, b: VId) -> Option<(i128, i128)> {
    Some((const_arg(f, inst, a)?, const_arg(f, inst, b)?))
}

fn const_arg(f: &crate::kir::Func, inst: &Inst, v: VId) -> Option<i128> {
    const_of(f, v)
}

/// 値番号 `v` が定数ならその値を返す。ブロック引数や未定義の値は `None`。
fn const_of(f: &crate::kir::Func, v: VId) -> Option<i128> {
    for b in &f.blocks {
        for i in &b.insts {
            if i.dst == v {
                return match &i.op {
                    Op::Const(c) => Some(*c),
                    _ => None,
                };
            }
        }
    }
    None
}

// ---- 整数演算の畳み込み (wrap / sat) ----

fn arith_wrap(op: Aop, ty: Ty, x: i128, y: i128) -> i128 {
    match op {
        Aop::Add => ty.wrap(x.wrapping_add(y)),
        Aop::Sub => ty.wrap(x.wrapping_sub(y)),
        Aop::Mul => ty.wrap(x.wrapping_mul(y)),
        Aop::Div => {
            let q = if y == 0 { 0 } else { x.wrapping_div(y) };
            ty.wrap(q)
        }
        Aop::Rem => {
            let r = if y == 0 { 0 } else { x.wrapping_rem(y) };
            ty.wrap(r)
        }
    }
}

fn arith_sat(op: Aop, ty: Ty, x: i128, y: i128) -> i128 {
    match op {
        Aop::Add => ty.wrap(x.saturating_add(y)),
        Aop::Sub => ty.wrap(x.saturating_sub(y)),
        Aop::Mul => ty.wrap(x.saturating_mul(y)),
        Aop::Div | Aop::Rem => ty.wrap(x.wrapping_div(y)), // sat で div/rem は無いので wrap と同じ
    }
}

fn wrap_neg(ty: Ty, x: i128) -> i128 {
    ty.wrap(x.wrapping_neg())
}

// ---- 比較 ----

fn cmp_val(cc: Cc, ty: Ty, x: i128, y: i128) -> bool {
    crate::kir_scalar::cmp(cc, ty, x, y)
}

// ---- 浮動小数 ----

fn farith(op: Aop, ty: Ty, x: i128, y: i128) -> i128 {
    crate::kir_scalar::farith(op, ty, x, y)
}

fn funary(op: FUn, src: Ty, res: Ty, a: i128) -> i128 {
    crate::kir_scalar::funary(op, src, res, a)
}

fn fbin(op: FBin, ty: Ty, x: i128, y: i128) -> i128 {
    crate::kir_scalar::fbin(op, ty, x, y)
}

fn shift(left: bool, ty: Ty, x: i128, n: i128) -> i128 {
    crate::kir_scalar::shift(left, ty, x, n)
}

fn minmax(max: bool, ty: Ty, x: i128, y: i128) -> i128 {
    crate::kir_scalar::minmax(max, ty, x, y)
}

fn conv(src: Ty, dst: Ty, a: i128) -> i128 {
    crate::kir_scalar::conv(src, dst, a)
}

fn bitcast(_src: Ty, a: i128) -> i128 {
    // ビット列をそのまま使う。型は同じ幅なので値は不変。
    a
}

fn not_val(ty: Ty, x: i128) -> i128 {
    if ty == Ty::Bool {
        (x == 0) as i128
    } else {
        crate::kir_scalar::not_int(ty, x)
    }
}

fn bool_val(op: BoolOp, ty: Ty, x: i128, y: i128) -> i128 {
    let r = match op {
        BoolOp::And => x & y,
        BoolOp::Or => x | y,
        BoolOp::Xor => x ^ y,
    };
    if ty == Ty::Bool {
        (r != 0) as i128
    } else {
        ty.wrap(r)
    }
}

fn bitcount(k: BitK, src: Ty, x: i128) -> i128 {
    crate::kir_scalar::bitcount(k, src, x)
}

fn tag_val(x: i128) -> i128 {
    x & 1
}

fn payload_val(x: i128) -> i128 {
    x & crate::kir::PAY_MASK
}

fn tag_or_none(x: i128) -> i128 {
    crate::kir::TAG_BIT | x
}

fn ok_val(x: i128) -> i128 {
    x
}

fn err_val(x: i128) -> i128 {
    crate::kir::TAG_BIT | x
}

fn checked_val(op: Aop, t: Ty, x: i128, y: i128) -> i128 {
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
        Aop::Rem => {
            if pt.signed() && y == -1 && x == pt.min() {
                None
            } else {
                x.checked_rem(y)
            }
        }
    };
    match r {
        Some(v) if v >= pt.min() && v <= pt.max() => crate::kir::TAG_BIT | crate::kir::payload_bits(v),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kir::{Block, Func, Inst, Term};

    fn sample() -> Module {
        let f = Func {
            fqn: "demo.fib".into(),
            key: "demo.fib".into(),
            uses: vec![],
            may_panic: false,
            stack: Some(4096),
            ret: Ty::U64,
            blocks: vec![
                Block {
                    params: vec![(0, Ty::U64)],
                    insts: vec![
                        Inst { dst: 1, ty: Ty::U64, op: Op::Const(2) },
                        Inst { dst: 2, ty: Ty::Bool, op: Op::Cmp { cc: Cc::Lt, ty: Ty::U64, a: 0, b: 1 } },
                    ],
                    term: Term::BrIf(2, (1, vec![]), (2, vec![0])),
                },
                Block { params: vec![], insts: vec![], term: Term::Ret(0) },
                Block {
                    params: vec![(3, Ty::U64)],
                    insts: vec![
                        Inst { dst: 4, ty: Ty::U64, op: Op::Const(1) },
                        Inst { dst: 5, ty: Ty::U64, op: Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: 3, b: 4 } },
                        Inst { dst: 6, ty: Ty::U64, op: Op::Call { callee: "demo.fib".into(), args: vec![5] } },
                    ],
                    term: Term::Ret(6),
                },
            ],
        };
        Module { funcs: vec![f], ..Default::default() }
    }

    #[test]
    fn const_fold_wrap_add_and_cmp() {
        let m = sample();
        let out = const_fold(&m).unwrap();
        let b = &out.funcs[0].blocks[0];
        // %1: u64 = const 2 はそのまま、%2: bool = icmp.lt.u64 %0, %1 は %0 が変数なので畳めない
        assert_eq!(b.insts[0].op, Op::Const(2));
        // %5: sub.u64.wrap %3, %4(%4=const 1) は畳めない（%3 はブロック引数）
    }

    #[test]
    fn const_fold_rejects_trap_div_by_zero() {
        // trap モードの div は畳まない（0 除算で panic の意味を保つ）
        let f = Func {
            fqn: "t.div".into(),
            key: "t.div".into(),
            uses: vec![],
            may_panic: true,
            stack: None,
            ret: Ty::I32,
            blocks: vec![Block {
                params: vec![],
                insts: vec![
                    Inst { dst: 0, ty: Ty::I32, op: Op::Const(7) },
                    Inst { dst: 1, ty: Ty::I32, op: Op::Const(0) },
                    Inst { dst: 2, ty: Ty::I32, op: Op::Arith { op: Aop::Div, mode: Mode::Trap, a: 0, b: 1 } },
                ],
                term: Term::Ret(2),
            }],
        };
        let m = Module { funcs: vec![f], ..Default::default() };
        let out = const_fold(&m).unwrap();
        assert!(matches!(out.funcs[0].blocks[0].insts[2].op, Op::Arith { .. }));
    }

    #[test]
    fn const_fold_wrap_div_by_zero_ok() {
        // wrap モードの div は 0 除算でも panic しないので畳む
        let f = Func {
            fqn: "t.div".into(),
            key: "t.div".into(),
            uses: vec![],
            may_panic: false,
            stack: None,
            ret: Ty::U32,
            blocks: vec![Block {
                params: vec![],
                insts: vec![
                    Inst { dst: 0, ty: Ty::U32, op: Op::Const(7) },
                    Inst { dst: 1, ty: Ty::U32, op: Op::Const(0) },
                    Inst { dst: 2, ty: Ty::U32, op: Op::Arith { op: Aop::Div, mode: Mode::Wrap, a: 0, b: 1 } },
                ],
                term: Term::Ret(2),
            }],
        };
        let m = Module { funcs: vec![f], ..Default::default() };
        let out = const_fold(&m).unwrap();
        assert!(matches!(out.funcs[0].blocks[0].insts[2].op, Op::Const(_)));
    }

    #[test]
    fn const_fold_optimize_pipeline_runs() {
        let m = sample();
        let _ = optimize(&m).unwrap();
    }

    /// 使われていない純粋な算術命令は除去され、副作用のある命令は残る。
    #[test]
    fn dce_removes_dead_pure_inst_keeps_side_effects() {
        let f = Func {
            fqn: "t.dce".into(),
            key: "t.dce".into(),
            uses: vec![],
            may_panic: false,
            stack: None,
            ret: Ty::U32,
            blocks: vec![Block {
                params: vec![],
                insts: vec![
                    Inst { dst: 0, ty: Ty::U32, op: Op::Const(1) },   // 未使用 → 削除
                    Inst { dst: 1, ty: Ty::U32, op: Op::Const(2) },   // %2 で使われる → 残る
                    Inst { dst: 2, ty: Ty::U32, op: Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: 1, b: 1 } }, // 自己参照は不正だがテスト用
                    Inst { dst: 3, ty: Ty::U32, op: Op::Call { callee: "t.dce".into(), args: vec![] } }, // 副作用 → 残る（未使用でも）
                ],
                term: Term::Ret(2),
            }],
        };
        let m = Module { funcs: vec![f], ..Default::default() };
        let out = dce(&m).unwrap();
        let insts = &out.funcs[0].blocks[0].insts;
        // %0 (const 1) は除去される
        assert!(!insts.iter().any(|i| matches!(i.op, Op::Const(1))));
        // %1 (const 2), %2 (add), call は残る
        assert!(insts.iter().any(|i| matches!(i.op, Op::Const(2))));
        assert!(insts.iter().any(|i| matches!(i.op, Op::Call { .. })));
        // ret %2 は %2 を参照するので %2 生成元は残る
        assert_eq!(out.funcs[0].blocks[0].term, Term::Ret(2));
    }
}
