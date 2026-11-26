//! KIR からバイトコードへ (設計_KIR.md 4.2)。意味は参照実行器 (`kir_exec.rs`) と同じ。
//!
//! 呼び先の解決・型の判定・集成体の配置はここで終える。`Err` は「この KIR は VM が扱えない」理由 (呼び出し側が木走査へ退避する)。

use crate::kir::FsK;
use std::collections::HashMap;

use super::bc::*;
use crate::kir::{size_of, AggKind, Aop, Block, Cc, Func, Mode, Module, Op, Term, Ty, VId};

/// 定数の被演算子を命令に埋める (即値)。`Arith` (64 ビット以下の `+ - *`) は `(もう一方の値, 即値, 定数の値)`。
/// 加算と乗算は定数が左でも入れ替えて埋める。
fn imm_arith(op: Aop, mode: Mode, ty: Ty, a: VId, b: VId, cv: &HashMap<VId, i32>) -> Option<(VId, i32, VId)> {
    fast_ti(ty)?;
    if !matches!(mode, Mode::Trap | Mode::Wrap) || !matches!(op, Aop::Add | Aop::Sub | Aop::Mul) {
        return None;
    }
    if let Some(c) = cv.get(&b) {
        return Some((a, *c, b));
    }
    if op != Aop::Sub {
        if let Some(c) = cv.get(&a) {
            return Some((b, *c, a));
        }
    }
    None
}

fn swap_cc(cc: Cc) -> Cc {
    match cc {
        Cc::Lt => Cc::Gt,
        Cc::Le => Cc::Ge,
        Cc::Gt => Cc::Lt,
        Cc::Ge => Cc::Le,
        c => c,
    }
}

/// 整数 (浮動小数と `u128` を除く) の比較の定数の被演算子: `(もう一方の値, 比較, 即値, 定数の値)`。
fn imm_cmp(cc: Cc, oty: Ty, a: VId, b: VId, cv: &HashMap<VId, i32>) -> Option<(VId, Cc, i32, VId)> {
    if oty.is_float() || wide128(oty) {
        return None;
    }
    if let Some(c) = cv.get(&b) {
        return Some((a, cc, *c, b));
    }
    cv.get(&a).map(|c| (b, swap_cc(cc), *c, a))
}

fn wide128(t: Ty) -> bool {
    matches!(t, Ty::I128 | Ty::U128)
}

/// 比較の被演算子 (レジスタ同士、またはレジスタと即値)。
#[derive(Clone, Copy)]
enum CmpOpnd {
    Rr(S, S),
    Ri(S, i32),
}

/// 呼び先の VFunc の入れ物 (`ret_w` などは `compile_func` が決める)。

/// KIR のモジュール全体をバイトコードにする。
pub fn compile(m: &Module, lines: &HashMap<String, crate::kir_lower::FnLines>) -> Result<Prog, String> {
    let mut prog = Prog { aggs: m.aggs.clone(), depth: m.depth, ..Default::default() };
    let index: HashMap<&str, usize> = m.funcs.iter().enumerate().map(|(i, f)| (f.key.as_str(), i)).collect();
    for f in &m.funcs {
        let vf = if f.link.is_some() {
            // `extern` 宣言 (本体なし)。呼び出しは `CallExt` になる。
            VFunc { key: f.key.clone(), code: vec![], pool: vec![], marks: vec![], evs: vec![], file: String::new(), nslots: 0, nparams: f.params().iter().map(|(_, t)| slots_of(&m.aggs, *t)).sum(), ptys: f.params().iter().map(|(_, t)| *t).collect(), ret: f.ret, ret_w: slots_of(&m.aggs, f.ret) }
        } else {
            compile_func(m, &index, &mut prog, f, lines.get(&f.key)).map_err(|e| format!("{}: {}", f.key, e))?
        };
        prog.funcs.push(vf);
    }
    prog.by_key = prog.funcs.iter().enumerate().map(|(i, f)| (f.key.clone(), i as u32)).collect();
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
        Ty::U64 | Ty::Usize | Ty::F64 | Ty::Ptr | Ty::Ref(..) => LK::U64,
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
        Ty::I64 | Ty::U64 | Ty::Isize | Ty::Usize | Ty::F64 | Ty::Ptr | Ty::Ref(..) => 8,
        Ty::I128 | Ty::U128 => 16,
        _ => return None,
    })
}

/// 読み出し命令 (よく使う幅は専用の命令)。
fn load_ins(d: S, p: S, off: u32, k: LK) -> Ins {
    match k {
        LK::U64 => Ins::LoadU64 { d, p, off },
        LK::I64 => Ins::LoadI64 { d, p, off },
        LK::U32 => Ins::LoadU32 { d, p, off },
        LK::I32 => Ins::LoadI32 { d, p, off },
        LK::U8 => Ins::LoadU8 { d, p, off },
        k => Ins::Load { d, p, off, k },
    }
}

fn store_ins(p: S, off: u32, v: S, w: u8) -> Ins {
    match w {
        8 => Ins::Store64 { p, off, v },
        1 => Ins::Store8 { p, off, v },
        w => Ins::Store { p, off, v, w },
    }
}

thread_local! {
    static DEBUG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// デバッグ用のバイトコードを作るか (行の命令 `Line` を足し、値を畳み込まない)。`ksn debug` だけ。
pub fn set_debug(on: bool) {
    DEBUG.with(|c| c.set(on));
}

struct Fc<'a> {
    debug: bool,
    m: &'a Module,
    index: &'a HashMap<&'a str, usize>,
    prog: &'a mut Prog,
    /// 値番号 → 行 (行表の元)。
    fl: Option<&'a crate::kir_lower::FnLines>,
    marks: Vec<(u32, u32)>,
    last_mark_line: u32,
    slot: Vec<u32>,
    /// `i32` に収まる定数の値 (即値にできる)。
    cv: HashMap<VId, i32>,
    /// 定数のうち、使われる箇所がすべて即値に埋められるもの (定数の命令を出さない)。
    skip_const: std::collections::HashSet<VId>,
    /// 関数の入口で作る (ループの中の) 定数。
    hoisted: std::collections::HashSet<VId>,
    vty: Vec<Ty>,
    uses: Vec<u32>,
    nslots: u32,
    temps: u32,
    code: Vec<Ins>,
    pool: Vec<u32>,
    /// 直前に出した比較 (`icmp`) の (結果の値, 命令の位置)。次の `br_if` が融合する。
    last_cmp: Option<(VId, usize)>,
    /// 直前に出した `ptr.add` の (結果の値, 命令の位置)。次の `load` / `store` が融合する。
    last_pa: Option<(VId, usize)>,
    /// ブロック内の `fieldaddr` のうち、唯一の使い道が同じブロックの `load` / `store` のアドレスのもの (値 → (基底, オフセット))。
    /// 命令を出さずに、`load` / `store` のオフセットに畳み込む。
    folded: HashMap<VId, (VId, u32)>,
    pending: HashMap<VId, (VId, u32)>,
}

fn compile_func(m: &Module, index: &HashMap<&str, usize>, prog: &mut Prog, f: &Func, fl: Option<&crate::kir_lower::FnLines>) -> Result<VFunc, String> {
    let n = f.value_count();
    let mut slot = vec![u32::MAX; n];
    let mut vty = vec![Ty::Unit; n];
    let mut next = 0u32;
    let mut uses = vec![0u32; n];
    for b in &f.blocks {
        for (v, t) in &b.params {
            if slot[*v as usize] == u32::MAX {
                slot[*v as usize] = next;
                next += slots_of(&m.aggs, *t);
            }
            vty[*v as usize] = *t;
        }
        for i in &b.insts {
            if slot[i.dst as usize] == u32::MAX {
                slot[i.dst as usize] = next;
                next += slots_of(&m.aggs, i.ty);
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
    // ブロック引数の合流の `Mov` を減らす: 無条件分岐 (`Br`) の引数が、そのブロックの中で作られて他に使われない値なら、
    // その値を受け側のパラメータと同じスロットに置く (作った時点で直接書く)。パラメータの古い値がそれ以降に読まれないときだけ。
    for b in &f.blocks {
        let Term::Br((t, args)) = &b.term else { continue };
        let params = &f.blocks[*t].params;
        for (k, a) in args.iter().enumerate() {
            let Some((p, _)) = params.get(k) else { continue };
            let (a, p) = (*a, *p);
            if uses[a as usize] != 1 || slot[a as usize] == slot[p as usize] {
                continue;
            }
            let Some(idx) = b.insts.iter().position(|i| i.dst == a) else { continue };
            if b.params.iter().any(|(q, _)| *q == a) {
                continue;
            }
            let reads_p = |i: &crate::kir::Inst| crate::kir::operands(&i.op).contains(&p);
            let later = b.insts[idx + 1..].iter().any(reads_p);
            let fa = b.insts.iter().any(|i| matches!(i.op, Op::FieldAddr { .. }) && reads_p(i));
            let other_args = args.iter().enumerate().any(|(j, x)| j != k && *x == p);
            if later || fa || other_args {
                continue;
            }
            slot[a as usize] = slot[p as usize];
        }
    }
    let mut cv: HashMap<VId, i32> = HashMap::new();
    for b in &f.blocks {
        for i in &b.insts {
            if let Op::Const(v) = &i.op {
                if let Ok(x) = i32::try_from(*v) {
                    cv.insert(i.dst, x);
                }
            }
        }
    }
    let mut nfold: HashMap<VId, u32> = HashMap::new();
    for b in &f.blocks {
        for i in &b.insts {
            let c = match &i.op {
                Op::Arith { op, mode, a, b } => imm_arith(*op, *mode, i.ty, *a, *b, &cv).map(|x| x.2),
                Op::Cmp { cc, ty: oty, a, b } => imm_cmp(*cc, *oty, *a, *b, &cv).map(|x| x.3),
                _ => None,
            };
            if let Some(c) = c {
                *nfold.entry(c).or_insert(0) += 1;
            }
        }
    }
    let debug = DEBUG.with(|c| c.get());
    let skip_const: std::collections::HashSet<VId> = if debug { Default::default() } else { nfold.into_iter().filter(|(v, n)| uses[*v as usize] == *n).map(|(v, _)| v).collect() };
    // ループの中の定数は、関数の入口で 1 回だけ作る (後ろ辺 `to <= from` の範囲のブロックをループとみなす)。
    let mut in_loop = vec![false; f.blocks.len()];
    for (bi, b) in f.blocks.iter().enumerate() {
        let targets: Vec<usize> = match &b.term {
            Term::Br((t, _)) => vec![*t],
            Term::BrIf(_, (t1, _), (t2, _)) => vec![*t1, *t2],
            _ => vec![],
        };
        for t in targets {
            if t <= bi {
                in_loop[t..=bi].iter_mut().for_each(|x| *x = true);
            }
        }
    }
    let mut slot_users: HashMap<u32, u32> = HashMap::new();
    for b in &f.blocks {
        b.params.iter().for_each(|(v, _)| *slot_users.entry(slot[*v as usize]).or_insert(0) += 1);
        b.insts.iter().for_each(|i| *slot_users.entry(slot[i.dst as usize]).or_insert(0) += 1);
    }
    let mut hoisted: Vec<VId> = vec![];
    for (bi, b) in f.blocks.iter().enumerate() {
        if !in_loop[bi] {
            continue;
        }
        for i in &b.insts {
            if !debug && matches!(i.op, Op::Const(_)) && !skip_const.contains(&i.dst) && slots_of(&m.aggs, i.ty) == 1 && !matches!(i.ty, Ty::Opt(_) | Ty::Res(..)) && slot_users[&slot[i.dst as usize]] == 1 {
                hoisted.push(i.dst);
            }
        }
    }
    // 同じ値の定数は 1 つのスロットを共有する。
    let mut canon: HashMap<i128, u32> = HashMap::new();
    let mut prologue: Vec<(u32, i128)> = vec![];
    for b in &f.blocks {
        for i in &b.insts {
            if !hoisted.contains(&i.dst) {
                continue;
            }
            let Op::Const(v) = &i.op else { continue };
            match canon.get(v) {
                Some(s) => slot[i.dst as usize] = *s,
                None => {
                    canon.insert(*v, slot[i.dst as usize]);
                    prologue.push((slot[i.dst as usize], *v));
                }
            }
        }
    }
    let hoisted_set: std::collections::HashSet<VId> = hoisted.iter().copied().collect();
    let mut fc = Fc { debug, m, index, prog, fl, marks: vec![], last_mark_line: 0, slot, cv, skip_const, hoisted: hoisted_set, vty, uses, nslots: next, temps: 0, code: vec![], pool: vec![], last_cmp: None, last_pa: None, folded: HashMap::new(), pending: HashMap::new() };
    for (s, v) in prologue {
        let d = s as S;
        match i64::try_from(v) {
            Ok(x) => fc.code.push(Ins::ConstI { d, v: x }),
            Err(_) => {
                fc.prog.wide.push(v);
                fc.code.push(Ins::ConstW { d, k: (fc.prog.wide.len() - 1) as u32 });
            }
        }
    }
    let mut starts = vec![0u32; f.blocks.len()];
    // 分岐先が未確定の `Jmp` などの位置 → ブロック番号。
    let mut patches: Vec<(usize, usize)> = vec![];
    for (bi, b) in f.blocks.iter().enumerate() {
        starts[bi] = fc.code.len() as u32;
        fc.last_cmp = None;
        fc.last_pa = None;
        fc.folded.clear();
        fc.pending.clear();
        if !debug {
            fc.find_folds(&b.insts);
        }
        fc.last_mark_line = 0;
        for inst in &b.insts {
            fc.inst(inst)?;
        }
        fc.term(f, bi, b, &mut patches)?;
    }
    for (at, blk) in patches {
        let to = starts[blk];
        match &mut fc.code[at] {
            Ins::Jmp { to: t } | Ins::JmpIf { to: t, .. } | Ins::JmpIfNot { to: t, .. } | Ins::JCmpNot { to: t, .. } | Ins::JCmpNotI { to: t, .. } | Ins::JCmp { to: t, .. } | Ins::JCmpI { to: t, .. } => *t = to,
            _ => return Err("内部エラー: 分岐の書き換え先が分岐でない".into()),
        }
    }
    if fc.nslots + fc.temps > MAX_SLOTS {
        return Err(format!("スロットが {} 個を超える", MAX_SLOTS));
    }
    Ok(VFunc {
        key: f.key.clone(),
        code: fc.code,
        pool: fc.pool,
        marks: fc.marks,
        evs: fl.map(|x| x.evs.iter().map(|e| VEv { bid: e.bid, name: e.name.clone(), slot: if e.kill { 0 } else { fc.slot[e.vid as usize] }, ty: if e.kill { e.ty } else { match (e.mem, fc.vty[e.vid as usize]) { (true, Ty::Ref(_, p)) => p.ty(), (_, t) => t } }, mem: e.mem, kill: e.kill, at: e.at }).collect()).unwrap_or_default(),
        file: fl.map(|x| x.file.clone()).unwrap_or_default(),
        nslots: fc.nslots + fc.temps,
        nparams: f.params().iter().map(|(_, t)| slots_of(&m.aggs, *t)).sum(),
        ptys: f.params().iter().map(|(_, t)| *t).collect(),
        ret: f.ret,
        ret_w: slots_of(&m.aggs, f.ret),
    })
}

impl<'a> Fc<'a> {
    /// これから出す命令の行を行表に記す (同じ行が続くときは足さない)。
    /// 終端命令の行表。デバッグ用の `Line` は出さない (値の行は別の文の行のことがある)。
    fn mark_nl(&mut self, v: VId) {
        let d = std::mem::replace(&mut self.debug, false);
        self.mark(v);
        self.debug = d;
    }

    fn mark(&mut self, v: VId) {
        let line = self.fl.and_then(|f| f.lines.get(v as usize).copied()).unwrap_or(0);
        if line != 0 && self.marks.last().map(|m| m.1) != Some(line) {
            self.marks.push((self.code.len() as u32, line));
        }
        if self.debug && line != 0 && line != self.last_mark_line {
            self.last_mark_line = line;
            self.code.push(Ins::Line { line, vid: v });
        }
    }

    fn s(&self, v: VId) -> S {
        self.slot[v as usize] as S
    }

    fn ty_of(&mut self, t: Ty) -> u32 {
        tyi(self.prog, t)
    }

    fn pool_slots(&mut self, vs: &[VId]) -> (u32, u32) {
        let p = self.pool.len() as u32;
        for v in vs {
            let s = self.s(*v);
            let n = slots_of(&self.m.aggs, self.vty[*v as usize]);
            self.pool.push(pool_ent(s, n));
        }
        (p, vs.len() as u32)
    }

    /// 型 `t` の値のスロット数。
    fn w(&self, t: Ty) -> u32 {
        slots_of(&self.m.aggs, t)
    }

    /// ブロック `insts` の中で畳み込める `fieldaddr` を `folded` に入れる。
    fn find_folds(&mut self, insts: &[crate::kir::Inst]) {
        let mut defs: HashMap<VId, (VId, u32)> = HashMap::new();
        for i in insts {
            match &i.op {
                Op::FieldAddr { a, off } if self.uses[i.dst as usize] == 1 => {
                    defs.insert(i.dst, (*a, *off));
                }
                Op::Load(p) => {
                    if let Some(x) = defs.remove(p) {
                        self.folded.insert(*p, x);
                    }
                }
                Op::Store { ptr, val } if ptr != val => {
                    if let Some(x) = defs.remove(ptr) {
                        self.folded.insert(*ptr, x);
                    }
                }
                _ => {}
            }
        }
    }

    /// `load` / `store` のアドレス `p` を (基底スロット, オフセット) にする。畳み込めない集成体の読み書きのために `materialize` なら、
    /// 保留していた `fieldaddr` をここで出す。
    fn addr_of(&mut self, p: VId) -> (S, u32) {
        if let Some((a, off)) = self.pending.remove(&p) {
            (self.s(a), off)
        } else {
            (self.s(p), 0)
        }
    }

    /// 保留中の `fieldaddr` (`p`) を、実際の加算命令として出す。
    fn unfold(&mut self, p: VId) {
        if let Some((a, off)) = self.pending.remove(&p) {
            let (d, a) = (self.s(p), self.s(a));
            self.code.push(Ins::AddImm { d, a, imm: off as i64 });
        }
    }

    /// 浮動小数の値 `v` (型 `t`) を、ビットが見える使い方の前にその場で正規の NaN にする (整数・集成体は何もしない)。
    fn canon_val(&mut self, t: Ty, v: VId) {
        if matches!(t, Ty::F32 | Ty::F64) {
            let a = self.s(v);
            self.code.push(Ins::CanonF { a, bits: if t == Ty::F32 { 32 } else { 64 } });
        }
    }

    fn push(&mut self, i: Ins) {
        self.code.push(i);
    }

    fn inst(&mut self, inst: &crate::kir::Inst) -> Result<(), String> {
        let d = self.s(inst.dst);
        let ty = inst.ty;
        self.mark(inst.dst);
        self.last_cmp = None;
        let prev_pa = self.last_pa.take();
        // 直前の `ptr.add` の結果 `p` を 1 回だけ使う `load` / `store` なら、その加算を取り込む (基底と添字のスロット)。
        let fuse_pa = |fc: &mut Fc, p: VId| -> Option<(S, S)> {
            match prev_pa {
                Some((v, at)) if !fc.debug && v == p && fc.uses[p as usize] == 1 && at + 1 == fc.code.len() => match fc.code[at] {
                    Ins::PtrAdd { p: b, off: x, .. } => {
                        fc.code.pop();
                        Some((b, x))
                    }
                    _ => None,
                },
                _ => None,
            }
        };
        let i = match &inst.op {
            Op::ScopeEnd(_) | Op::Carry { .. } | Op::Own { .. } => return Ok(()),
            Op::Const(_) if self.skip_const.contains(&inst.dst) || self.hoisted.contains(&inst.dst) => return Ok(()),
            Op::Const(v) => match ty {
                Ty::Opt(_) | Ty::Res(..) => {
                    if *v != 0 {
                        return Err("Option / Result の 0 以外の定数は VM 未対応".into());
                    }
                    Ins::Zero { d, n: self.w(ty) as u16 }
                }
                Ty::Agg(_) => return Err("集成体の定数は VM 未対応".into()),
                Ty::I128 | Ty::U128 => {
                    self.prog.wide.push(*v);
                    Ins::Const128 { d, k: (self.prog.wide.len() - 1) as u32 }
                }
                _ => match i64::try_from(*v) {
                    Ok(x) => Ins::ConstI { d, v: x },
                    Err(_) => {
                        self.prog.wide.push(*v);
                        Ins::ConstW { d, k: (self.prog.wide.len() - 1) as u32 }
                    }
                },
            },
            Op::Arith { op, mode, a, b } => {
                if let Some((x, imm, _)) = imm_arith(*op, *mode, ty, *a, *b, &self.cv) {
                    let (a, t) = (self.s(x), fast_ti(ty).unwrap());
                    let trap = matches!(mode, Mode::Trap);
                    let i = match (op, trap, arith_class(t)) {
                        (Aop::Add, true, 0) => Ins::AddNI { d, a, t, imm },
                        (Aop::Sub, true, 0) => Ins::SubNI { d, a, t, imm },
                        (Aop::Mul, true, 0) => Ins::MulNI { d, a, t, imm },
                        (Aop::Add, true, 1) => Ins::AddSI { d, a, imm },
                        (Aop::Sub, true, 1) => Ins::SubSI { d, a, imm },
                        (Aop::Mul, true, 1) => Ins::MulSI { d, a, imm },
                        (Aop::Add, true, _) => Ins::AddUI { d, a, imm },
                        (Aop::Sub, true, _) => Ins::SubUI { d, a, imm },
                        (Aop::Mul, true, _) => Ins::MulUI { d, a, imm },
                        (Aop::Add, false, 0) => Ins::AddWI { d, a, t, imm },
                        (Aop::Sub, false, 0) => Ins::SubWI { d, a, t, imm },
                        (_, false, 0) => Ins::MulWI { d, a, t, imm },
                        (Aop::Add, false, _) => Ins::AddW64I { d, a, imm },
                        (Aop::Sub, false, _) => Ins::SubW64I { d, a, imm },
                        _ => Ins::MulW64I { d, a, imm },
                    };
                    self.push(i);
                    return Ok(());
                }
                let (a, b) = (self.s(*a), self.s(*b));
                use crate::kir::Aop::*;
                match (fast_ti(ty), op, mode) {
                    (Some(t), Add | Sub | Mul, Mode::Trap | Mode::Wrap) => {
                        let trap = matches!(mode, Mode::Trap);
                        match (op, trap, arith_class(t)) {
                            (Add, true, 0) => Ins::AddN { d, a, b, t },
                            (Sub, true, 0) => Ins::SubN { d, a, b, t },
                            (Mul, true, 0) => Ins::MulN { d, a, b, t },
                            (Add, true, 1) => Ins::AddS { d, a, b },
                            (Sub, true, 1) => Ins::SubS { d, a, b },
                            (Mul, true, 1) => Ins::MulS { d, a, b },
                            (Add, true, _) => Ins::AddU { d, a, b },
                            (Sub, true, _) => Ins::SubU { d, a, b },
                            (Mul, true, _) => Ins::MulU { d, a, b },
                            (Add, false, 0) => Ins::AddW { d, a, b, t },
                            (Sub, false, 0) => Ins::SubW { d, a, b, t },
                            (Mul, false, 0) => Ins::MulW { d, a, b, t },
                            (Add, false, _) => Ins::AddW64 { d, a, b },
                            (Sub, false, _) => Ins::SubW64 { d, a, b },
                            _ => Ins::MulW64 { d, a, b },
                        }
                    }
                    _ => Ins::Arith { d, a, b, op: *op, mode: *mode, ty: self.ty_of(ty) },
                }
            }
            Op::Cmp { cc, ty: oty, a, b } => {
                if let Some((x, cc, imm, _)) = imm_cmp(*cc, *oty, *a, *b, &self.cv) {
                    let a = self.s(x);
                    let cx = cx_of(cc, oty.signed());
                    self.code.push(Ins::CmpII { d, a, cx, imm });
                    self.last_cmp = Some((inst.dst, self.code.len() - 1));
                    return Ok(());
                }
                let (a, b) = (self.s(*a), self.s(*b));
                if oty.is_float() || wide128(*oty) {
                    Ins::Cmp { d, a, b, cc: *cc, ty: self.ty_of(*oty) }
                } else {
                    // 次の `br_if` が融合できるように覚えておく。
                    self.code.push(Ins::CmpI { d, a, b, cx: cx_of(*cc, oty.signed()) });
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
                if src.is_int() && ty.is_int() && !wide128(*src) && !wide128(ty) {
                    // `kir_scalar::conv` と同じ: 狭める変換と符号の変わる変換は型の範囲へ切り詰める。
                    if ty.bits() < src.bits() || src.signed() != ty.signed() {
                        Ins::WrapT { d, a, t: fast_ti(ty).unwrap() }
                    } else {
                        Ins::Mov { d, s: a }
                    }
                } else {
                    Ins::Conv { d, a, src: self.ty_of(*src), dst: self.ty_of(ty) }
                }
            }
            Op::Bitcast { src, a } => {
                self.canon_val(*src, *a);
                if wide128(ty) {
                    Ins::MovN { d, s: self.s(*a), n: 2 }
                } else {
                    Ins::Mov { d, s: self.s(*a) }
                }
            }
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
                if wide128(ty) {
                    Ins::BitOp128 { d, a: self.s(*a), b: self.s(*b), k }
                } else {
                    Ins::BitOp { d, a: self.s(*a), b: self.s(*b), k }
                }
            }
            Op::BitCount { k, src, a } => Ins::BitCount { d, a: self.s(*a), k: *k, src: self.ty_of(*src) },
            Op::Checked { op, a, b } => Ins::Checked { d, a: self.s(*a), b: self.s(*b), op: *op, ty: self.ty_of(ty) },
            Op::Call { callee, args } => {
                match (callee.as_str(), args.len()) {
                    ("ksn.mt.on", 0) => {
                        self.push(Ins::MtOn { d });
                        return Ok(());
                    }
                    ("ksn.mt.begin", 1) => {
                        let n = self.s(args[0]);
                        self.push(Ins::MtBegin { d, n });
                        return Ok(());
                    }
                    ("ksn.mt.end", 0) => {
                        self.push(Ins::MtEnd);
                        return Ok(());
                    }
                    _ => {}
                }
                let Some(&ci) = self.index.get(callee.as_str()) else {
                    return Err(format!("呼び先 {} がない", callee));
                };
                if self.m.funcs[ci].link.is_some() {
                    for a in args {
                        self.canon_val(self.vty[*a as usize], *a);
                    }
                }
                let (argp, argn) = self.pool_slots(args);
                let cf = &self.m.funcs[ci];
                if cf.link.is_some() {
                    if cf.blocks.first().map_or(false, |b| b.params.iter().any(|(_, t)| matches!(t, Ty::Agg(_)))) || matches!(cf.ret, Ty::Agg(_)) {
                        return Err(format!("extern 関数 {} は構造体を値で受け渡す (VM 未対応)", callee));
                    }
                    self.prog.ext.push(cf.clone());
                    Ins::CallExt { e: (self.prog.ext.len() - 1) as u32, d, argp, argn }
                } else {
                    Ins::Call { f: ci as u32, d, argp, argn }
                }
            }
            Op::FnAddr { callee } => {
                let Some(&ci) = self.index.get(callee.as_str()) else {
                    return Err(format!("fnaddr の呼び先 {} がない", callee));
                };
                if self.m.funcs[ci].link.is_some() {
                    return Err(format!("extern 関数 {} のアドレスは VM 未対応", callee));
                }
                Ins::FnAddr { d, f: ci as u32 }
            }
            Op::CallIndirect { f, args, .. } => {
                let (argp, argn) = self.pool_slots(args);
                Ins::CallInd { f: self.s(*f) as u32, d, argp, argn }
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
                for (f, a) in fdefs.iter().zip(fields.iter()) {
                    self.canon_val(f.ty, *a);
                }
                let info = MakeInfo {
                    nslots: self.w(ty),
                    variant: *variant,
                    fields: fdefs.iter().take(n).map(|f| (f.off, size_of(&self.m.aggs, f.ty))).collect(),
                    packs: fdefs.iter().take(n).map(|f| opt_shape(&self.m.aggs, f.ty).unwrap_or((0, 0))).collect(),
                };
                self.prog.makes.push(info);
                let (argp, _) = self.pool_slots(&fields[..n]);
                Ins::Make { d, info: (self.prog.makes.len() - 1) as u32, argp }
            }
            Op::Field { a, off } => match ty {
                Ty::Agg(ai) => Ins::FieldA { d, a: self.s(*a), off: *off, nb: self.m.aggs[ai as usize].size },
                Ty::Unit => Ins::ConstI { d, v: 0 },
                t if opt_shape(&self.m.aggs, t).is_some() => {
                    let (sb, pw) = opt_shape(&self.m.aggs, t).unwrap_or_default();
                    Ins::FieldOpt { d, a: self.s(*a), off: *off, sb, pw }
                }
                t => {
                    let Some(k) = lk(t) else { return Err(format!("field の型 {} が読めない", t.name())) };
                    Ins::FieldS { d, a: self.s(*a), off: *off, k }
                }
            },
            Op::SetField { a, off, v } => {
                let Ty::Agg(ai) = ty else { return Err("setfield の結果が集成体でない".into()) };
                let dd = &self.m.aggs[ai as usize];
                let Some(fty) = crate::kir_exec::field_ty_at(dd, *off) else { return Err("setfield のフィールドがない".into()) };
                self.canon_val(fty, *v);
                let nb = size_of(&self.m.aggs, fty);
                if nb > u16::MAX as u32 || self.w(ty) > u16::MAX as u32 {
                    return Err("setfield の集成体が大きすぎる".into());
                }
                match opt_shape(&self.m.aggs, fty) {
                    Some((sb, pw)) => Ins::SetFieldOpt { d, a: self.s(*a), v: self.s(*v), off: *off, ns: self.w(ty) as u16, sb, pw },
                    None => Ins::SetField { d, a: self.s(*a), v: self.s(*v), off: *off, ns: self.w(ty) as u16, nb: nb as u16 },
                }
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
                let Ty::Opt(p) = ty else { return Err("some の結果は Option のはず".into()) };
                Ins::WrapN { d, a: self.s(*a), n: self.w(p.ty()) as u16, tag: 1 }
            }
            Op::Ok(a) => {
                let Ty::Res(t0, _) = ty else { return Err("ok の結果は Result のはず".into()) };
                Ins::WrapN { d, a: self.s(*a), n: self.w(t0.ty()) as u16, tag: 0 }
            }
            Op::Err(a) => {
                let Ty::Res(_, e0) = ty else { return Err("err の結果は Result のはず".into()) };
                Ins::WrapN { d, a: self.s(*a), n: self.w(e0.ty()) as u16, tag: 1 }
            }
            Op::Payload(a) => match self.w(ty) {
                1 => Ins::Payload1 { d, a: self.s(*a) },
                n => Ins::PayloadN { d, a: self.s(*a), n: n as u16 },
            },
            Op::Alloca => {
                let Ty::Ref(_, p) = ty else { return Err("alloca の結果が参照でない".into()) };
                Ins::FrameBytes { d, size: size_of(&self.m.aggs, p.ty()) }
            }
            Op::FrameBytes { size } => Ins::FrameBytes { d, size: *size },
            Op::GlobalBytes { name, size } => {
                let p = match self.prog.globals.get(name) {
                    Some(p) => *p,
                    None => {
                        let p = super::arena::alloc_zeroed((*size as usize).max(1));
                        if p == 0 {
                            return Err("VM の領域が足りない".into());
                        }
                        self.prog.globals.insert(name.clone(), p);
                        p
                    }
                };
                self.prog.wide.push(p as i128);
                Ins::ConstW { d, k: (self.prog.wide.len() - 1) as u32 }
            }
            Op::StaticBytes(bytes) => {
                // 静的な領域はコンパイル時に 1 回だけ確保し、アドレスを定数にする (同じ命令は常に同じアドレス)。
                let mut v = bytes.clone();
                if v.is_empty() {
                    v.push(0);
                }
                let p = super::arena::leak(&v) as i128;
                if p == 0 {
                    return Err("VM の領域が足りない".into());
                }
                self.prog.wide.push(p);
                Ins::ConstW { d, k: (self.prog.wide.len() - 1) as u32 }
            }
            Op::PtrAdd { p, off } => {
                self.last_pa = Some((inst.dst, self.code.len()));
                Ins::PtrAdd { d, p: self.s(*p), off: self.s(*off) }
            }
            Op::MemLoad { p, off } => {
                let Some(k) = lk(ty) else { return Err(format!("mem.load の型 {} が読めない", ty.name())) };
                match fuse_pa(self, *p) {
                    Some((p, x)) => Ins::LoadX { d, p, x, off: *off, k },
                    None => load_ins(d, self.s(*p), *off, k),
                }
            }
            Op::MemStore { ty: sty, p, off, v } => {
                self.canon_val(*sty, *v);
                let Some(w) = width(*sty) else { return Err(format!("mem.store の型 {} が書けない", sty.name())) };
                match fuse_pa(self, *p) {
                    Some((p, x)) => Ins::StoreX { p, x, off: *off, v: self.s(*v), w },
                    None => store_ins(self.s(*p), *off, self.s(*v), w),
                }
            }
            Op::Load(p) => match ty {
                t if opt_shape(&self.m.aggs, t).is_some() => {
                    let (pp, off) = self.addr_of(*p);
                    let (sb, pw) = opt_shape(&self.m.aggs, t).unwrap_or_default();
                    Ins::LoadOpt { d, p: pp, off, sb, pw }
                }
                Ty::Agg(ai) => {
                    self.unfold(*p);
                    Ins::LoadAgg { d, p: self.s(*p), size: self.m.aggs[ai as usize].size }
                }
                t => {
                    let Some(k) = lk(t) else { return Err(format!("load の型 {} が読めない", t.name())) };
                    match fuse_pa(self, *p) {
                        Some((p, x)) => Ins::LoadX { d, p, x, off: 0, k },
                        None => {
                            let (pp, off) = self.addr_of(*p);
                            load_ins(d, pp, off, k)
                        }
                    }
                }
            },
            Op::Store { ptr, val } => match { self.canon_val(self.vty[*val as usize], *val); self.vty[*val as usize] } {
                Ty::Agg(ai) => {
                    self.unfold(*ptr);
                    Ins::StoreAgg { p: self.s(*ptr), v: self.s(*val), size: self.m.aggs[ai as usize].size }
                }
                t if opt_shape(&self.m.aggs, t).is_some() => {
                    let (pp, off) = self.addr_of(*ptr);
                    let (sb, pw) = opt_shape(&self.m.aggs, t).unwrap_or_default();
                    Ins::StoreOpt { p: pp, off, v: self.s(*val), sb, pw }
                }
                t => {
                    let Some(w) = width(t) else { return Err(format!("store の型 {} が書けない", t.name())) };
                    match fuse_pa(self, *ptr) {
                        Some((p, x)) => Ins::StoreX { p, x, off: 0, v: self.s(*val), w },
                        None => {
                            let (pp, off) = self.addr_of(*ptr);
                            store_ins(pp, off, self.s(*val), w)
                        }
                    }
                }
            },
            Op::FieldAddr { a, off } => {
                if let Some(x) = self.folded.get(&inst.dst) {
                    self.pending.insert(inst.dst, *x);
                    return Ok(());
                }
                Ins::AddImm { d, a: self.s(*a), imm: *off as i64 }
            }
            Op::AsShared(p) | Op::Reborrow(p) | Op::Borrow { a: p, .. } => Ins::Mov { d, s: self.s(*p) },
            Op::HeapAlloc(n) => Ins::HeapAlloc { d, n: self.s(*n) },
            Op::HeapRealloc { p, size } => Ins::HeapRealloc { d, p: self.s(*p), n: self.s(*size) },
            Op::HeapFree(p) => Ins::HeapFree { p: self.s(*p) },
            Op::IndexCheck { i, len } => Ins::IndexCheck { i: self.s(*i), len: self.s(*len) },
            Op::Utf8Valid { p, len } => Ins::Utf8Valid { d, p: self.s(*p), len: self.s(*len) },
            Op::Panic { msg, arg } => {
                self.prog.strs.push(msg.clone());
                let aty = match arg {
                    Some(a) => self.vty[*a as usize],
                    None => Ty::Unit,
                };
                Ins::Panic { msg: (self.prog.strs.len() - 1) as u32, arg: arg.map(|a| self.s(a)).unwrap_or(NO_SLOT), aty: self.ty_of(aty) }
            }
            Op::Abort => Ins::Abort,
            Op::Exit { code } => Ins::Exit { code: self.s(*code) },
            Op::Write { fd, data, nl } => Ins::Write { d, fd: self.s(*fd), data: self.s(*data), nl: *nl },
            Op::Eprint { data } => Ins::Eprint { data: self.s(*data) },
            Op::FmtFloat { bits, a } => Ins::FmtFloat { a: self.s(*a), bits: *bits },
            Op::FloatText { bits, a, dst } => Ins::FloatText { d, a: self.s(*a), dst: self.s(*dst), bits: *bits },
            Op::ParseInt { ty: pty, s, radix, strict } => Ins::ParseInt { d, s: self.s(*s), radix: self.s(*radix), ty: self.ty_of(*pty), strict: *strict },
            Op::EnvArgCount => Ins::EnvArgCount { d },
            Op::EnvArg { index } => Ins::EnvArg { d, i: self.s(*index) },
            Op::EnvVar { name } => Ins::EnvVar { d, name: self.s(*name) },
            Op::TimeNowNs => Ins::TimeNowNs { d },
            Op::NetListen { addr } => Ins::NetListen { d, addr: self.s(*addr) },
            Op::NetConnect { addr } => Ins::NetConnect { d, addr: self.s(*addr) },
            Op::NetAccept { sock } => Ins::NetAccept { d, sock: self.s(*sock) },
            Op::NetRead { sock, buf, len } => Ins::NetRead { d, sock: self.s(*sock), buf: self.s(*buf), len: self.s(*len) },
            Op::NetWrite { sock, buf, len } => Ins::NetWrite { d, sock: self.s(*sock), buf: self.s(*buf), len: self.s(*len) },
            Op::NetFlush { sock } => Ins::NetFlush { d, sock: self.s(*sock) },
            Op::NetClose { sock } => Ins::NetClose { d, sock: self.s(*sock) },
            Op::NetLocalPort { sock } => Ins::NetLocalPort { d, sock: self.s(*sock) },
            Op::FnPtr { callee } => {
                let Some(&ci) = self.index.get(callee.as_str()) else {
                    return Err(format!("関数ポインタの先 {} がない", callee));
                };
                if self.m.funcs[ci].link.is_some() {
                    return Err(format!("extern 関数 {} のアドレスは VM 未対応", callee));
                }
                // 構造体の値渡し・値返しは VM では扱えない (実行時の `FnPtr` と同じ条件。先に退避させる)。
                let cf = &self.m.funcs[ci];
                if cf.blocks.first().map_or(false, |b| b.params.iter().any(|(_, t)| matches!(t, Ty::Agg(_)))) || matches!(cf.ret, Ty::Agg(_)) {
                    return Err(format!("関数ポインタ {} は構造体を値で受け渡す (VM 未対応)", callee));
                }
                Ins::FnPtr { d, f: ci as u32 }
            }
            Op::FsCreate { path } => Ins::FsOpen { d, path: self.s(*path), create: true },
            Op::FsOpen { path } => Ins::FsOpen { d, path: self.s(*path), create: false },
            Op::FsExists { path } => Ins::FsExists { d, path: self.s(*path) },
            Op::FsRemove { path } => Ins::FsRemove { d, path: self.s(*path) },
            Op::FsSys { kind, args } => {
                let k = match kind {
                    FsK::Rename => 0,
                    FsK::CreateDir => 1,
                    FsK::RemoveDir => 2,
                    FsK::OpenAppend => 3,
                    FsK::Stat => 4,
                    FsK::ReadDir => 5,
                    FsK::OpenDir => 6,
                    FsK::OpenBeneath => 7,
                    FsK::FStat => 8,
                };
                let at = |i: usize| -> S { args.get(i).map_or(0, |v| self.s(*v)) };
                Ins::FsSys { d, kind: k, a: at(0), b: at(1), c: at(2) }
            }
            Op::FsClose { fd } => Ins::FsClose { d, fd: self.s(*fd) },
            Op::FsFlush { fd } | Op::FsSync { fd } => Ins::FsSync { d, fd: self.s(*fd) },
            Op::FsRead { fd, buf, len } => Ins::FsRead { d, fd: self.s(*fd), buf: self.s(*buf), len: self.s(*len) },
            Op::FsWrite { fd, buf, len } => Ins::FsWrite { d, fd: self.s(*fd), buf: self.s(*buf), len: self.s(*len) },
            Op::Proc { f, args } => {
                let k = crate::kir::ProcFn::ALL.iter().position(|x| x == f).ok_or("std.process の未知の命令")? as u8;
                let at = |i: usize| -> S { args.get(i).map_or(0, |v| self.s(*v)) };
                Ins::Proc { d, f: k, a: [at(0), at(1), at(2)] }
            }
            // OS の乱数 (`std.rand.fill_secure` / `from_entropy`) は VM 未対応 (木走査か参照実行器へ退避する)。
            Op::RandFill { buf, len, fatal } => Ins::RandFill { d, buf: self.s(*buf), len: self.s(*len), fatal: *fatal },
            // std.net の追加分 (タイムアウト・shutdown・アドレス・resolve・UDP) も VM 未対応 (木走査へ退避する)。
            Op::NetSys { .. } => return Err("std.net の追加分は VM 未対応".into()),
        };
        self.push(i);
        Ok(())
    }

    /// 移動 `d <- s` (`n` スロット)。
    fn mov(&mut self, d: S, s: S, n: u32) {
        if n == 1 {
            self.push(Ins::Mov { d, s });
        } else {
            self.push(Ins::MovN { d, s, n: n as u16 });
        }
    }

    /// 辺の引数の受け渡し (並列代入)。移動の前の値で受け取る必要があるとき (後の引数が前の移動先を読む) だけ、一時のスロットを通す。
    fn edge(&mut self, args: &[VId], params: &[(VId, Ty)]) {
        let pairs: Vec<(S, S, u32)> = params.iter().zip(args).map(|((p, t), a)| (self.s(*p), self.s(*a), self.w(*t))).filter(|(d, s, _)| d != s).collect();
        let hazard = (0..pairs.len()).any(|i| (i + 1..pairs.len()).any(|j| pairs[i].0 == pairs[j].1));
        if !hazard {
            for (d, s, n) in pairs {
                self.mov(d, s, n);
            }
            return;
        }
        let base = self.nslots;
        let total: u32 = pairs.iter().map(|p| p.2).sum();
        if base + total > MAX_SLOTS {
            self.temps = MAX_SLOTS + 1; // 呼び出し元が上限超過として退避させる
            return;
        }
        self.temps = self.temps.max(total);
        let mut at = base;
        let mut tmp = vec![];
        for (_, s, n) in pairs.iter() {
            self.mov(at as S, *s, *n);
            tmp.push(at);
            at += n;
        }
        for (k, (d, _, n)) in pairs.iter().enumerate() {
            self.mov(*d, tmp[k] as S, *n);
        }
    }

    /// ブロック `h` が「(定数を除いて) 整数の比較 1 つ + 引数のない分岐」だけなら、その比較と分岐先 (真・偽)。
    fn thread_target(&self, f: &Func, h: usize) -> Option<((Cx, CmpOpnd), usize, usize)> {
        let b = &f.blocks[h];
        let Term::BrIf(c, (t1, a1), (t2, a2)) = &b.term else { return None };
        if !a1.is_empty() || !a2.is_empty() || self.uses[*c as usize] != 1 {
            return None;
        }
        let mut real = b.insts.iter().filter(|i| !(matches!(i.op, Op::Const(_)) && (self.skip_const.contains(&i.dst) || self.hoisted.contains(&i.dst))));
        let i = real.next()?;
        if real.next().is_some() || i.dst != *c {
            return None;
        }
        let Op::Cmp { cc, ty: oty, a, b: bb } = &i.op else { return None };
        if oty.is_float() || wide128(*oty) {
            return None;
        }
        let cmp = match imm_cmp(*cc, *oty, *a, *bb, &self.cv) {
            Some((x, cc, imm, _)) => (cx_of(cc, oty.signed()), CmpOpnd::Ri(self.s(x), imm)),
            None => (cx_of(*cc, oty.signed()), CmpOpnd::Rr(self.s(*a), self.s(*bb))),
        };
        Some((cmp, *t1, *t2))
    }

    fn jump_to(&mut self, f: &Func, from: usize, to: usize, args: &[VId], fall: bool, patches: &mut Vec<(usize, usize)>) {
        let b: &Block = &f.blocks[to];
        self.edge(args, &b.params);
        // 後ろ辺の先が「比較 1 つだけで分岐するブロック」なら、その比較をここに写して、先頭への Jmp を省く。
        if to <= from {
            if let Some((cmp, t1, t2)) = self.thread_target(f, to) {
                patches.push((self.code.len(), t1));
                match cmp {
                    (cx, CmpOpnd::Rr(a, b)) => self.push(Ins::JCmp { cx, a, b, to: 0 }),
                    (cx, CmpOpnd::Ri(a, imm)) => self.push(Ins::JCmpI { cx, a, imm, to: 0 }),
                }
                if !(fall && t2 == from + 1) {
                    patches.push((self.code.len(), t2));
                    self.push(Ins::Jmp { to: 0 });
                }
                return;
            }
        }
        // 次のブロックへは落ちる (命令を出さない)。
        if !(fall && to == from + 1) {
            patches.push((self.code.len(), to));
            self.push(Ins::Jmp { to: 0 });
        }
    }

    /// 辺 (`args` → `params`) が出す `Mov` の数の見積り (0 かどうかだけを正確に返す)。
    fn moves(&self, args: &[VId], params: &[(VId, Ty)]) -> usize {
        params.iter().zip(args).filter(|((p, _), a)| self.s(*p) != self.s(**a)).count()
    }

    fn term(&mut self, f: &Func, bi: usize, b: &Block, patches: &mut Vec<(usize, usize)>) -> Result<(), String> {
        match &b.term {
            Term::Br((t, args)) => self.jump_to(f, bi, *t, args, true, patches),
            Term::BrIf(c, (t1, a1), (t2, a2)) => {
                self.mark_nl(*c);
                // 直前の比較を融合できるか (結果がこの分岐だけで使われる)。
                let fused = match self.last_cmp {
                    Some((v, at)) if !self.debug && v == *c && self.uses[*c as usize] == 1 && at + 1 == self.code.len() => {
                        match self.code[at].clone() {
                            Ins::CmpI { cx, a, b, .. } => {
                                self.code.pop();
                                Some((cx, CmpOpnd::Rr(a, b)))
                            }
                            Ins::CmpII { cx, a, imm, .. } => {
                                self.code.pop();
                                Some((cx, CmpOpnd::Ri(a, imm)))
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let at = self.code.len();
                let nm1 = self.moves(a1, &f.blocks[*t1].params);
                let nm2 = self.moves(a2, &f.blocks[*t2].params);
                // 辺の移動がない側へは直接飛ぶ (偽の側が移動なし: 偽で飛ぶ。真の側が移動なし: 比較を反転して真で飛ぶ)。
                // 両方なしなら、次のブロックへ落ちる側を落とす。
                let jump_true = nm2 != 0 && nm1 == 0 || nm1 == 0 && nm2 == 0 && *t1 != bi + 1 && *t2 == bi + 1;
                if jump_true {
                    match fused {
                        Some((cx, CmpOpnd::Rr(a, b))) => self.push(Ins::JCmp { cx, a, b, to: 0 }),
                        Some((cx, CmpOpnd::Ri(a, imm))) => self.push(Ins::JCmpI { cx, a, imm, to: 0 }),
                        None => {
                            let c = self.s(*c);
                            self.push(Ins::JmpIf { c, to: 0 })
                        }
                    }
                    patches.push((at, *t1));
                    self.jump_to(f, bi, *t2, a2, true, patches);
                } else if nm2 == 0 {
                    match fused {
                        Some((cx, CmpOpnd::Rr(a, b))) => self.push(Ins::JCmpNot { cx, a, b, to: 0 }),
                        Some((cx, CmpOpnd::Ri(a, imm))) => self.push(Ins::JCmpNotI { cx, a, imm, to: 0 }),
                        None => {
                            let c = self.s(*c);
                            self.push(Ins::JmpIfNot { c, to: 0 })
                        }
                    }
                    patches.push((at, *t2));
                    self.jump_to(f, bi, *t1, a1, true, patches);
                } else {
                    match fused {
                        Some((cx, CmpOpnd::Rr(a, b))) => self.push(Ins::JCmpNot { cx, a, b, to: 0 }),
                        Some((cx, CmpOpnd::Ri(a, imm))) => self.push(Ins::JCmpNotI { cx, a, imm, to: 0 }),
                        None => {
                            let c = self.s(*c);
                            self.push(Ins::JmpIfNot { c, to: 0 })
                        }
                    }
                    self.jump_to(f, bi, *t1, a1, false, patches);
                    // 偽の側の飛び先はここ (真の側の辺の移動と Jmp の後ろ)。
                    let else_pc = self.code.len() as u32;
                    match &mut self.code[at] {
                        Ins::JCmpNot { to, .. } | Ins::JCmpNotI { to, .. } | Ins::JmpIfNot { to, .. } => *to = else_pc,
                        _ => unreachable!(),
                    }
                    self.jump_to(f, bi, *t2, a2, true, patches);
                }
            }
            Term::Ret(v) => {
                self.mark_nl(*v);
                let v = self.s(*v);
                self.push(Ins::Ret { v });
            }
            Term::Unreachable => self.push(Ins::Unreachable),
        }
        Ok(())
    }
}
