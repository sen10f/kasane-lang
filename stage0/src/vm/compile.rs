//! KIR からバイトコードへ (設計_KIR.md 4.2)。意味は参照実行器 (`kir_exec.rs`) と同じ。
//!
//! 呼び先の解決・型の判定・集成体の配置はここで終える。`Err` は「この KIR は VM が扱えない」理由 (呼び出し側が木走査へ退避する)。

use std::collections::HashMap;

use super::bc::*;
use crate::kir::{size_of, AggKind, Block, Func, Mode, Module, Op, Term, Ty, VId};

/// KIR のモジュール全体をバイトコードにする。
pub fn compile(m: &Module) -> Result<Prog, String> {
    let mut prog = Prog { aggs: m.aggs.clone(), ..Default::default() };
    let index: HashMap<&str, usize> = m.funcs.iter().enumerate().map(|(i, f)| (f.key.as_str(), i)).collect();
    for f in &m.funcs {
        let vf = if f.link.is_some() {
            // `extern` 宣言 (本体なし)。呼び出しは `CallExt` になる。
            VFunc { key: f.key.clone(), code: vec![], pool: vec![], nslots: 0, nparams: f.params().len() as u32, ret: f.ret, ret_heap: false }
        } else {
            compile_func(m, &index, &mut prog, f).map_err(|e| format!("{}: {}", f.key, e))?
        };
        prog.funcs.push(vf);
    }
    Ok(prog)
}

fn tyi(prog: &mut Prog, t: Ty) -> u32 {
    match prog.tys.iter().position(|x| *x == t) {
        Some(i) => i as u32,
        None => {
            prog.tys.push(t);
            (prog.tys.len() - 1) as u32
        }
    }
}

/// メモリ読み出しの種類。`unit` と `Option` / `Result` / 参照は読めない。
fn lk(t: Ty) -> Option<LK> {
    Some(match t {
        Ty::Bool => LK::Bool,
        Ty::I8 => LK::I8,
        Ty::U8 => LK::U8,
        Ty::I16 => LK::I16,
        Ty::U16 => LK::U16,
        Ty::I32 => LK::I32,
        Ty::U32 | Ty::F32 => LK::U32,
        Ty::I64 | Ty::Isize => LK::I64,
        Ty::U64 | Ty::Usize | Ty::F64 | Ty::Ptr => LK::U64,
        Ty::I128 | Ty::U128 => LK::I128,
        _ => return None,
    })
}

/// 書き込みの幅 (バイト)。
fn width(t: Ty) -> Option<u8> {
    Some(match t {
        Ty::Bool | Ty::I8 | Ty::U8 => 1,
        Ty::I16 | Ty::U16 => 2,
        Ty::I32 | Ty::U32 | Ty::F32 => 4,
        Ty::I64 | Ty::U64 | Ty::Isize | Ty::Usize | Ty::F64 | Ty::Ptr => 8,
        Ty::I128 | Ty::U128 => 16,
        _ => return None,
    })
}

struct Fc<'a> {
    m: &'a Module,
    index: &'a HashMap<&'a str, usize>,
    prog: &'a mut Prog,
    slot: Vec<u32>,
    vty: Vec<Ty>,
    uses: Vec<u32>,
    nslots: u32,
    temps: u32,
    code: Vec<Ins>,
    pool: Vec<u32>,
    /// 直前に出した比較 (`icmp`) の (結果の値, 命令の位置)。次の `br_if` が融合する。
    last_cmp: Option<(VId, usize)>,
}

fn compile_func(m: &Module, index: &HashMap<&str, usize>, prog: &mut Prog, f: &Func) -> Result<VFunc, String> {
    let n = f.value_count();
    let mut slot = vec![u32::MAX; n];
    let mut vty = vec![Ty::Unit; n];
    let mut next = 0u32;
    let mut uses = vec![0u32; n];
    for b in &f.blocks {
        for (v, t) in &b.params {
            if slot[*v as usize] == u32::MAX {
                slot[*v as usize] = next;
                next += 1;
            }
            vty[*v as usize] = *t;
        }
        for i in &b.insts {
            if slot[i.dst as usize] == u32::MAX {
                slot[i.dst as usize] = next;
                next += 1;
            }
            vty[i.dst as usize] = i.ty;
            for v in crate::kir::operands(&i.op) {
                uses[v as usize] += 1;
            }
        }
        match &b.term {
            Term::Br((_, a)) => a.iter().for_each(|v| uses[*v as usize] += 1),
            Term::BrIf(c, (_, a), (_, b2)) => {
                uses[*c as usize] += 1;
                a.iter().chain(b2.iter()).for_each(|v| uses[*v as usize] += 1);
            }
            Term::Ret(v) => uses[*v as usize] += 1,
            Term::Unreachable => {}
        }
    }
    let mut fc = Fc { m, index, prog, slot, vty, uses, nslots: next, temps: 0, code: vec![], pool: vec![], last_cmp: None };
    let mut starts = vec![0u32; f.blocks.len()];
    // 分岐先が未確定の `Jmp` などの位置 → ブロック番号。
    let mut patches: Vec<(usize, usize)> = vec![];
    for (bi, b) in f.blocks.iter().enumerate() {
        starts[bi] = fc.code.len() as u32;
        fc.last_cmp = None;
        for inst in &b.insts {
            fc.inst(inst)?;
        }
        fc.term(f, bi, b, &mut patches)?;
    }
    for (at, blk) in patches {
        let to = starts[blk];
        match &mut fc.code[at] {
            Ins::Jmp { to: t } | Ins::JmpIfNot { to: t, .. } | Ins::JCmpNot { to: t, .. } => *t = to,
            _ => return Err("内部エラー: 分岐の書き換え先が分岐でない".into()),
        }
    }
    Ok(VFunc {
        key: f.key.clone(),
        code: fc.code,
        pool: fc.pool,
        nslots: fc.nslots + fc.temps,
        nparams: f.params().len() as u32,
        ret: f.ret,
        ret_heap: crate::kir_exec::has_heap(f.ret),
    })
}

impl<'a> Fc<'a> {
    fn s(&self, v: VId) -> S {
        self.slot[v as usize]
    }

    fn ty_of(&mut self, t: Ty) -> u32 {
        tyi(self.prog, t)
    }

    fn pool_slots(&mut self, vs: &[VId]) -> (u32, u32) {
        let p = self.pool.len() as u32;
        for v in vs {
            let s = self.s(*v);
            self.pool.push(s);
        }
        (p, vs.len() as u32)
    }

    fn push(&mut self, i: Ins) {
        self.code.push(i);
    }

    fn inst(&mut self, inst: &crate::kir::Inst) -> Result<(), String> {
        let d = self.s(inst.dst);
        let ty = inst.ty;
        self.last_cmp = None;
        let i = match &inst.op {
            Op::Const(v) => match i64::try_from(*v) {
                Ok(x) => Ins::ConstI { d, v: x },
                Err(_) => {
                    self.prog.wide.push(*v);
                    Ins::ConstW { d, k: (self.prog.wide.len() - 1) as u32 }
                }
            },
            Op::Arith { op, mode, a, b } => {
                let (a, b) = (self.s(*a), self.s(*b));
                use crate::kir::Aop::*;
                match (fast_ti(ty), op, mode) {
                    (Some(t), Add, Mode::Trap) => Ins::AddT { d, a, b, t },
                    (Some(t), Sub, Mode::Trap) => Ins::SubT { d, a, b, t },
                    (Some(t), Mul, Mode::Trap) => Ins::MulT { d, a, b, t },
                    (Some(t), Add, Mode::Wrap) => Ins::AddW { d, a, b, t },
                    (Some(t), Sub, Mode::Wrap) => Ins::SubW { d, a, b, t },
                    (Some(t), Mul, Mode::Wrap) => Ins::MulW { d, a, b, t },
                    _ => Ins::Arith { d, a, b, op: *op, mode: *mode, ty: self.ty_of(ty) },
                }
            }
            Op::Cmp { cc, ty: oty, a, b } => {
                let (a, b) = (self.s(*a), self.s(*b));
                if oty.is_float() || *oty == Ty::U128 {
                    Ins::Cmp { d, a, b, cc: *cc, ty: self.ty_of(*oty) }
                } else {
                    // 次の `br_if` が融合できるように覚えておく。
                    self.code.push(Ins::CmpI { d, a, b, cc: *cc });
                    self.last_cmp = Some((inst.dst, self.code.len() - 1));
                    return Ok(());
                }
            }
            Op::Neg { a, mode } => Ins::Neg { d, a: self.s(*a), mode: *mode, ty: self.ty_of(ty) },
            Op::FArith { op, a, b } => Ins::FArith { d, a: self.s(*a), b: self.s(*b), op: *op, ty: self.ty_of(ty) },
            Op::Shift { left, a, n } => Ins::Shift { d, a: self.s(*a), n: self.s(*n), left: *left, ty: self.ty_of(ty) },
            Op::MinMax { max, a, b } => Ins::MinMax { d, a: self.s(*a), b: self.s(*b), max: *max, ty: self.ty_of(ty) },
            Op::FUn { f, src, a } => Ins::FUn { d, a: self.s(*a), f: *f, src: self.ty_of(*src), res: self.ty_of(ty) },
            Op::FBin { f, a, b } => Ins::FBin { d, a: self.s(*a), b: self.s(*b), f: *f, res: self.ty_of(ty) },
            Op::Conv { src, a } => {
                let a = self.s(*a);
                if src.is_int() && ty.is_int() {
                    if ty.bits() < src.bits() {
                        match fast_ti(ty) {
                            Some(t) => Ins::WrapT { d, a, t },
                            None => Ins::Conv { d, a, src: self.ty_of(*src), dst: self.ty_of(ty) },
                        }
                    } else {
                        Ins::Mov { d, s: a }
                    }
                } else {
                    Ins::Conv { d, a, src: self.ty_of(*src), dst: self.ty_of(ty) }
                }
            }
            Op::Bitcast { a, .. } => Ins::Mov { d, s: self.s(*a) },
            Op::Not(a) => {
                let a = self.s(*a);
                if ty == Ty::Bool {
                    Ins::NotB { d, a }
                } else {
                    Ins::NotI { d, a, ty: self.ty_of(ty) }
                }
            }
            Op::Bool(op, a, b) => {
                use crate::kir::BoolOp::*;
                let k = match op {
                    And => 0,
                    Or => 1,
                    Xor => 2,
                };
                Ins::BitOp { d, a: self.s(*a), b: self.s(*b), k }
            }
            Op::BitCount { k, src, a } => Ins::BitCount { d, a: self.s(*a), k: *k, src: self.ty_of(*src) },
            Op::Checked { op, a, b } => Ins::Checked { d, a: self.s(*a), b: self.s(*b), op: *op, ty: self.ty_of(ty) },
            Op::Call { callee, args } => {
                let Some(&ci) = self.index.get(callee.as_str()) else {
                    return Err(format!("呼び先 {} がない", callee));
                };
                let (argp, argn) = self.pool_slots(args);
                let cf = &self.m.funcs[ci];
                if cf.link.is_some() {
                    self.prog.ext.push(cf.clone());
                    Ins::CallExt { e: (self.prog.ext.len() - 1) as u32, d, argp, argn }
                } else {
                    Ins::Call { f: ci as u32, d, argp, argn }
                }
            }
            Op::Make { variant, fields } => {
                let Ty::Agg(ai) = ty else { return Err("make の結果が集成体でない".into()) };
                let dd = &self.m.aggs[ai as usize];
                let fdefs = match (&dd.kind, variant) {
                    (AggKind::Enum(vs), Some(k)) => &vs[*k as usize].fields,
                    (AggKind::Struct(fs) | AggKind::Tuple(fs), None) => fs,
                    _ => return Err("make の変種が集成体の種類と合わない".into()),
                };
                let n = fdefs.len().min(fields.len());
                let info = MakeInfo { size: dd.size, variant: *variant, fields: fdefs.iter().take(n).map(|f| (f.off, f.ty)).collect() };
                self.prog.makes.push(info);
                let (argp, _) = self.pool_slots(&fields[..n]);
                Ins::Make { d, info: (self.prog.makes.len() - 1) as u32, argp }
            }
            Op::Field { a, off } => Ins::Field { d, a: self.s(*a), off: *off, ty: self.ty_of(ty) },
            Op::SetField { a, off, v } => {
                let Ty::Agg(ai) = ty else { return Err("setfield の結果が集成体でない".into()) };
                Ins::SetField { d, a: self.s(*a), v: self.s(*v), off: *off, agg: ai }
            }
            Op::Tag(a) => {
                let a = self.s(*a);
                if ty == Ty::I32 {
                    Ins::TagEnum { d, a }
                } else {
                    Ins::TagOpt { d, a }
                }
            }
            Op::Some(a) => {
                let Ty::Opt(_) = ty else { return Err("some の結果は Option のはず".into()) };
                Ins::Some { d, a: self.s(*a), pay: self.ty_of(ty) }
            }
            Op::Ok(a) => {
                let Ty::Res(..) = ty else { return Err("ok の結果は Result のはず".into()) };
                Ins::Ok { d, a: self.s(*a), pay: self.ty_of(ty) }
            }
            Op::Err(a) => {
                let Ty::Res(..) = ty else { return Err("err の結果は Result のはず".into()) };
                Ins::Err { d, a: self.s(*a), pay: self.ty_of(ty) }
            }
            Op::Payload(a) => Ins::Payload { d, a: self.s(*a), ty: self.ty_of(ty) },
            Op::Alloca => {
                let Ty::Ref(_, p) = ty else { return Err("alloca の結果が参照でない".into()) };
                Ins::FrameBytes { d, size: size_of(&self.m.aggs, p.ty()) }
            }
            Op::FrameBytes { size } => Ins::FrameBytes { d, size: *size },
            Op::StaticBytes(bytes) => {
                // 静的な領域はコンパイル時に 1 回だけ確保し、アドレスを定数にする (同じ命令は常に同じアドレス)。
                let mut v = bytes.clone();
                if v.is_empty() {
                    v.push(0);
                }
                let p = Box::leak(v.into_boxed_slice()).as_ptr() as usize as i128;
                self.prog.wide.push(p);
                Ins::ConstW { d, k: (self.prog.wide.len() - 1) as u32 }
            }
            Op::PtrAdd { p, off } => Ins::PtrAdd { d, p: self.s(*p), off: self.s(*off) },
            Op::MemLoad { p, off } => {
                let Some(k) = lk(ty) else { return Err(format!("mem.load の型 {} が読めない", ty.name())) };
                Ins::Load { d, p: self.s(*p), off: *off, k }
            }
            Op::MemStore { ty: sty, p, off, v } => {
                let Some(w) = width(*sty) else { return Err(format!("mem.store の型 {} が書けない", sty.name())) };
                Ins::Store { p: self.s(*p), off: *off, v: self.s(*v), w }
            }
            Op::Load(p) => match ty {
                Ty::Agg(ai) => Ins::LoadAgg { d, p: self.s(*p), size: self.m.aggs[ai as usize].size },
                t => {
                    let Some(k) = lk(t) else { return Err(format!("load の型 {} が読めない", t.name())) };
                    Ins::Load { d, p: self.s(*p), off: 0, k }
                }
            },
            Op::Store { ptr, val } => match self.vty[*val as usize] {
                Ty::Agg(_) => Ins::StoreAgg { p: self.s(*ptr), v: self.s(*val) },
                t => {
                    let Some(w) = width(t) else { return Err(format!("store の型 {} が書けない", t.name())) };
                    Ins::Store { p: self.s(*ptr), off: 0, v: self.s(*val), w }
                }
            },
            Op::FieldAddr { a, off } => Ins::AddImm { d, a: self.s(*a), imm: *off as i64 },
            Op::AsShared(p) | Op::Reborrow(p) => Ins::Mov { d, s: self.s(*p) },
            Op::HeapAlloc(n) => Ins::HeapAlloc { d, n: self.s(*n) },
            Op::HeapRealloc { p, size } => Ins::HeapRealloc { d, p: self.s(*p), n: self.s(*size) },
            Op::HeapFree(p) => Ins::HeapFree { p: self.s(*p) },
            Op::IndexCheck { i, len } => Ins::IndexCheck { i: self.s(*i), len: self.s(*len) },
            Op::Utf8Valid { p, len } => Ins::Utf8Valid { d, p: self.s(*p), len: self.s(*len) },
            Op::Panic { msg, arg } => {
                self.prog.strs.push(msg.clone());
                Ins::Panic { msg: (self.prog.strs.len() - 1) as u32, arg: arg.map(|a| self.s(a)).unwrap_or(NO_SLOT) }
            }
            Op::Abort => Ins::Abort,
            Op::Write { fd, data } => Ins::Write { d, fd: self.s(*fd), data: self.s(*data) },
            Op::Eprint { data } => Ins::Eprint { data: self.s(*data) },
            Op::FmtFloat { bits, a } => Ins::FmtFloat { a: self.s(*a), bits: *bits },
            Op::ParseInt { ty: pty, s, radix } => Ins::ParseInt { d, s: self.s(*s), radix: self.s(*radix), ty: self.ty_of(*pty) },
            // 参照実行器と同じく関数のアドレスは 0 (VM では C のコールバックは呼ばれない)。
            Op::FnPtr { .. } => Ins::ConstI { d, v: 0 },
            Op::EnvArgCount => Ins::EnvArgCount { d },
            Op::EnvArg { index } => Ins::EnvArg { d, i: self.s(*index) },
            Op::EnvVar { name } => Ins::EnvVar { d, name: self.s(*name) },
            Op::NetListen { .. }
            | Op::NetConnect { .. }
            | Op::NetAccept { .. }
            | Op::NetRead { .. }
            | Op::NetWrite { .. }
            | Op::NetFlush { .. }
            | Op::NetClose { .. }
            | Op::NetLocalPort { .. } => return Err("std.net の命令は VM 未対応".into()),
        };
        self.push(i);
        Ok(())
    }

    /// 辺の引数の受け渡し (並列代入)。移動の前の値で受け取る必要があるとき (後の引数が前の移動先を読む) だけ、一時のスロットを通す。
    fn edge(&mut self, args: &[VId], params: &[(VId, Ty)]) {
        let pairs: Vec<(S, S)> = params.iter().zip(args).map(|((p, _), a)| (self.s(*p), self.s(*a))).filter(|(d, s)| d != s).collect();
        let hazard = (0..pairs.len()).any(|i| (i + 1..pairs.len()).any(|j| pairs[i].0 == pairs[j].1));
        if !hazard {
            for (d, s) in pairs {
                self.push(Ins::Mov { d, s });
            }
            return;
        }
        let base = self.nslots;
        self.temps = self.temps.max(pairs.len() as u32);
        for (k, (_, s)) in pairs.iter().enumerate() {
            self.push(Ins::Mov { d: base + k as u32, s: *s });
        }
        for (k, (d, _)) in pairs.iter().enumerate() {
            self.push(Ins::Mov { d: *d, s: base + k as u32 });
        }
    }

    fn jump_to(&mut self, f: &Func, from: usize, to: usize, args: &[VId], fall: bool, patches: &mut Vec<(usize, usize)>) {
        let b: &Block = &f.blocks[to];
        self.edge(args, &b.params);
        // 次のブロックへは落ちる (命令を出さない)。
        if !(fall && to == from + 1) {
            patches.push((self.code.len(), to));
            self.push(Ins::Jmp { to: 0 });
        }
    }

    fn term(&mut self, f: &Func, bi: usize, b: &Block, patches: &mut Vec<(usize, usize)>) -> Result<(), String> {
        match &b.term {
            Term::Br((t, args)) => self.jump_to(f, bi, *t, args, true, patches),
            Term::BrIf(c, (t1, a1), (t2, a2)) => {
                // 直前の比較を融合できるか (結果がこの分岐だけで使われる)。
                let fused = match self.last_cmp {
                    Some((v, at)) if v == *c && self.uses[*c as usize] == 1 && at + 1 == self.code.len() => {
                        match self.code[at].clone() {
                            Ins::CmpI { cc, a, b, .. } => {
                                self.code.pop();
                                Some((cc, a, b))
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let at = self.code.len();
                match fused {
                    Some((cc, a, b)) => self.push(Ins::JCmpNot { cc, a, b, to: 0 }),
                    None => {
                        let c = self.s(*c);
                        self.push(Ins::JmpIfNot { c, to: 0 })
                    }
                }
                self.jump_to(f, bi, *t1, a1, false, patches);
                // 偽の側の飛び先はここ (真の側の辺の移動と Jmp の後ろ)。
                let else_pc = self.code.len() as u32;
                match &mut self.code[at] {
                    Ins::JCmpNot { to, .. } | Ins::JmpIfNot { to, .. } => *to = else_pc,
                    _ => unreachable!(),
                }
                self.jump_to(f, bi, *t2, a2, true, patches);
            }
            Term::Ret(v) => {
                let v = self.s(*v);
                self.push(Ins::Ret { v });
            }
            Term::Unreachable => self.push(Ins::Unreachable),
        }
        Ok(())
    }
}
