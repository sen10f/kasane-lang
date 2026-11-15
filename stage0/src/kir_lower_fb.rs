//! KIR の関数を組み立てる補助 (`Fb`)。チャネル・スレッドの補助の関数 (`kir_lower_chan.rs` / `kir_lower_thread.rs`) が使う。
//! (旧 `kir_lower_map.rs` の一部。HashMap / BtreeMap の型ごとの下ろしは段 2 で Kasane 版へ移した。)

use crate::kir::{Aop, Block, BoolOp, Cc, Func, Inst, Mode, Op, Term, Ty, VId};

pub(super) struct Fb {
    blocks: Vec<(Vec<(VId, Ty)>, Vec<Inst>, Option<Term>)>,
    n: u32,
    cur: usize,
}

impl Fb {
    pub(super) fn new(params: &[Ty]) -> Fb {
        let ps = params.iter().enumerate().map(|(i, t)| (i as VId, *t)).collect();
        Fb { blocks: vec![(ps, vec![], None)], n: params.len() as u32, cur: 0 }
    }
    pub(super) fn emit(&mut self, ty: Ty, op: Op) -> VId {
        let dst = self.n;
        self.n += 1;
        self.blocks[self.cur].1.push(Inst { dst, ty, op });
        dst
    }
    pub(super) fn block(&mut self, params: &[Ty]) -> (usize, Vec<VId>) {
        let ids: Vec<VId> = params
            .iter()
            .map(|_| {
                self.n += 1;
                self.n - 1
            })
            .collect();
        self.blocks.push((ids.iter().copied().zip(params.iter().copied()).collect(), vec![], None));
        (self.blocks.len() - 1, ids)
    }
    pub(super) fn term(&mut self, t: Term) {
        self.blocks[self.cur].2 = Some(t);
    }
    pub(super) fn goto(&mut self, b: usize, args: Vec<VId>) {
        self.term(Term::Br((b, args)));
    }
    pub(super) fn at(&mut self, b: usize) {
        self.cur = b;
    }
    pub(super) fn c(&mut self, ty: Ty, v: i128) -> VId {
        self.emit(ty, Op::Const(v))
    }
    pub(super) fn cmp(&mut self, cc: Cc, a: VId, b: VId) -> VId {
        self.emit(Ty::Bool, Op::Cmp { cc, ty: Ty::Usize, a, b })
    }
    pub(super) fn br_if(&mut self, c: VId, t: usize, e: usize) {
        self.term(Term::BrIf(c, (t, vec![]), (e, vec![])));
    }
    pub(super) fn add(&mut self, a: VId, b: VId) -> VId {
        self.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a, b })
    }
    pub(super) fn sub(&mut self, a: VId, b: VId) -> VId {
        self.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a, b })
    }
    pub(super) fn mul(&mut self, a: VId, b: VId) -> VId {
        self.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a, b })
    }
    pub(super) fn and(&mut self, a: VId, b: VId) -> VId {
        self.emit(Ty::Usize, Op::Bool(BoolOp::And, a, b))
    }
    pub(super) fn xor(&mut self, a: VId, b: VId) -> VId {
        self.emit(Ty::Usize, Op::Bool(BoolOp::Xor, a, b))
    }
    pub(super) fn load(&mut self, ty: Ty, p: VId, off: i128) -> VId {
        self.emit(ty, Op::MemLoad { p, off: off as u32 })
    }
    pub(super) fn store(&mut self, ty: Ty, p: VId, off: i128, v: VId) {
        self.emit(Ty::Unit, Op::MemStore { ty, p, off: off as u32, v });
    }
    pub(super) fn padd(&mut self, p: VId, off: VId) -> VId {
        self.emit(Ty::Ptr, Op::PtrAdd { p, off })
    }
    pub(super) fn build(self, key: &str, uses: &[&str], ret: Ty) -> Func {
        Func {
            fqn: key.to_string(),
            key: key.to_string(),
            uses: uses.iter().map(|s| s.to_string()).collect(),
            may_panic: false,
            stack: None,
            ret,
            link: None,
            blocks: self.blocks.into_iter().map(|(params, insts, t)| Block { params, insts, term: t.unwrap_or(Term::Unreachable) }).collect(),
        }
    }
}
