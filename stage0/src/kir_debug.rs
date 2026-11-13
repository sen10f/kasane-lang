//! `derive [Debug]` の表示を KIR の関数に下ろす (設計_KIR.md 12.3)。`kir_lower.rs` の子モジュール。
//!
//! 入口の `main` が `Err(e)` を返したときの `Error: <e の Debug 表示>` に使う。型ごとに 1 つ、関数 `<型>.$debug(v, fd) -> unit`
//! (`fd` に書く。`write` の失敗は捨てる。木走査の `eprintln!` と同じ) を作る。表示は木走査の `debug_str` と同じ:
//!
//! - 構造体は `名前 { f: v, g: w }` (名前は FQN の末尾)、列挙型は変種の名前 (ペイロードがあれば `名前(v, w)`)。
//! - 整数は 10 進 (除算と剰余のループ。新しい命令は使わない)、`bool` は `true` / `false`、`char` は `'x'`、`&str` は `"…"`
//!   (`\` `"` 改行だけを `\\` `\"` `\n` にする。木走査の `py_quote`)。
//! - 型は元の定義 (`Def`) の型で分類する (KIR の型は `char` と `u32`、`&str` と `&[T]` を区別しないため)。
//!   `derive [Debug]` を持たない型・利用者の `debug_fmt` がある型・タプル・浮動小数・128 ビット整数・Vec / String は、理由付きで未対応。
//! - `char` の表示は Rust の `{:?}` と同じ (`\n` `\r` `\t` `\0` `\'` `\\` と、制御文字の `\u{..}`)。ASCII 以外は、
//!   常に表示できる文字として UTF-8 のまま出す (表示できない Unicode の `\u{..}` 化はしない)。

use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet};

use crate::kir::{AggKind, Aop, Block, BoolOp, Cc, Func, Inst, Mode, Op, Pay, Term, Ty, VId};
use crate::program::Program;
use crate::types::{from_v, Ty as Src};

use super::agg::AggTab;
use super::show;

/// 型 `ty` (元の型 `src`) の Debug 表示の関数の鍵。
pub fn key(name: &str) -> String {
    format!("{}.$debug", name)
}

/// 入口の `main` の `Err(e)` を表示する関数の鍵 (`e` の型の表示の関数を呼ぶだけ)。LLVM の入口がこれを呼ぶ。
pub const ENTRY_DEBUG: &str = "ksn.entry.debug";

/// 関数 1 つを組み立てる道具。引数は `(v, fd)`。
struct Fb {
    blocks: Vec<(Vec<(VId, Ty)>, Vec<Inst>, Option<Term>)>,
    n: u32,
    cur: usize,
    fat: Ty,
    io: Ty,
    fd: VId,
    v: VId,
}

impl Fb {
    fn new(vt: Ty, fat: Ty, io: Ty) -> Fb {
        Fb { blocks: vec![(vec![(0, vt), (1, Ty::I32)], vec![], None)], n: 2, cur: 0, fat, io, fd: 1, v: 0 }
    }

    fn val(&mut self) -> VId {
        self.n += 1;
        self.n - 1
    }

    fn emit(&mut self, ty: Ty, op: Op) -> VId {
        let dst = self.val();
        self.blocks[self.cur].1.push(Inst { dst, ty, op });
        dst
    }

    fn block(&mut self, params: &[Ty]) -> (usize, Vec<VId>) {
        let ids: Vec<VId> = params.iter().map(|_| self.val()).collect();
        self.blocks.push((ids.iter().copied().zip(params.iter().copied()).collect(), vec![], None));
        (self.blocks.len() - 1, ids)
    }

    fn term(&mut self, t: Term) {
        self.blocks[self.cur].2 = Some(t);
    }

    fn goto(&mut self, b: usize, args: Vec<VId>) {
        self.term(Term::Br((b, args)));
    }

    fn at(&mut self, b: usize) {
        self.cur = b;
    }

    fn c(&mut self, ty: Ty, v: i128) -> VId {
        self.emit(ty, Op::Const(v))
    }

    fn cmp(&mut self, cc: Cc, ty: Ty, a: VId, b: VId) -> VId {
        self.emit(Ty::Bool, Op::Cmp { cc, ty, a, b })
    }

    fn br_if(&mut self, c: VId, t: usize, e: usize) {
        self.term(Term::BrIf(c, (t, vec![]), (e, vec![])));
    }

    /// `fd` へ `{ptr, len}` を書く (失敗は捨てる)。
    fn put(&mut self, ptr: VId, len: VId) {
        let fat = self.emit(self.fat, Op::Make { variant: None, fields: vec![ptr, len] });
        let res = Ty::Res(Pay::Unit, Pay::of(self.io).expect("集成体"));
        self.emit(res, Op::Write { fd: self.fd, data: fat });
    }

    /// `fd` へ固定の文字列を書く。
    fn s(&mut self, s: &str) {
        let p = self.emit(Ty::Ptr, Op::StaticBytes(s.as_bytes().to_vec()));
        let n = self.c(Ty::Usize, s.len() as i128);
        self.put(p, n);
    }

    fn finish(mut self, key: &str) -> Func {
        let u = self.c(Ty::Unit, 0);
        self.term(Term::Ret(u));
        Func {
            fqn: key.to_string(),
            key: key.to_string(),
            uses: vec!["io.console".to_string()],
            may_panic: true,
            stack: None,
            ret: Ty::Unit,
            blocks: self.blocks.into_iter().map(|(params, insts, t)| Block { params, insts, term: t.unwrap_or(Term::Unreachable) }).collect(),
        }
    }
}

/// 10 進の整数 (128 ビットは対象外)。符号付きは `-` と大きさ (u64) に分け、桁を末尾から `buf` に置く。
fn int_fn(t: Ty, fat: Ty, io: Ty) -> Func {
    let mut b = Fb::new(t, fat, io);
    let buf = b.emit(Ty::Ptr, Op::FrameBytes { size: 20 });
    let tmp = b.emit(Ty::Ptr, Op::FrameBytes { size: 8 });
    // i64 → u64 の再解釈 (値を保つ変換は無いので、同じ 8 バイトを書いて読み直す)
    let as_u64 = |b: &mut Fb, x: VId| -> VId {
        b.emit(Ty::Unit, Op::MemStore { ty: Ty::I64, p: tmp, off: 0, v: x });
        b.emit(Ty::U64, Op::MemLoad { p: tmp, off: 0 })
    };
    let (dig, dp) = b.block(&[Ty::U64]);
    if t.signed() {
        let x = if t == Ty::I64 { b.v } else { b.emit(Ty::I64, Op::Conv { src: t, a: b.v }) };
        let zero = b.c(Ty::I64, 0);
        let neg = b.cmp(Cc::Lt, Ty::I64, x, zero);
        let (bn, _) = b.block(&[]);
        let (bp, _) = b.block(&[]);
        b.br_if(neg, bn, bp);
        b.at(bn);
        b.s("-");
        let m = b.emit(Ty::I64, Op::Neg { a: x, mode: Mode::Wrap });
        let mu = as_u64(&mut b, m);
        b.goto(dig, vec![mu]);
        b.at(bp);
        let pu = as_u64(&mut b, x);
        b.goto(dig, vec![pu]);
    } else {
        let u = if t == Ty::U64 { b.v } else { b.emit(Ty::U64, Op::Conv { src: t, a: b.v }) };
        b.goto(dig, vec![u]);
    }
    let (lp, lv) = b.block(&[Ty::U64, Ty::Usize]);
    let (out, ov) = b.block(&[Ty::Usize]);
    b.at(dig);
    let i0 = b.c(Ty::Usize, 20);
    b.goto(lp, vec![dp[0], i0]);
    b.at(lp);
    let one = b.c(Ty::Usize, 1);
    let i2 = b.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: lv[1], b: one });
    let ten = b.c(Ty::U64, 10);
    let d = b.emit(Ty::U64, Op::Arith { op: Aop::Rem, mode: Mode::Trap, a: lv[0], b: ten });
    let q = b.emit(Ty::U64, Op::Arith { op: Aop::Div, mode: Mode::Trap, a: lv[0], b: ten });
    let table = b.emit(Ty::Ptr, Op::StaticBytes(b"0123456789".to_vec()));
    let du = b.emit(Ty::Usize, Op::Conv { src: Ty::U64, a: d });
    let tp = b.emit(Ty::Ptr, Op::PtrAdd { p: table, off: du });
    let ch = b.emit(Ty::U8, Op::MemLoad { p: tp, off: 0 });
    let bp = b.emit(Ty::Ptr, Op::PtrAdd { p: buf, off: i2 });
    b.emit(Ty::Unit, Op::MemStore { ty: Ty::U8, p: bp, off: 0, v: ch });
    let zero = b.c(Ty::U64, 0);
    let done = b.cmp(Cc::Eq, Ty::U64, q, zero);
    b.term(Term::BrIf(done, (out, vec![i2]), (lp, vec![q, i2])));
    b.at(out);
    let start = b.emit(Ty::Ptr, Op::PtrAdd { p: buf, off: ov[0] });
    let twenty = b.c(Ty::Usize, 20);
    let len = b.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: twenty, b: ov[0] });
    b.put(start, len);
    b.finish(&key(&t.name()))
}

fn bool_fn(fat: Ty, io: Ty) -> Func {
    let mut b = Fb::new(Ty::Bool, fat, io);
    let (t, _) = b.block(&[]);
    let (f, _) = b.block(&[]);
    let (j, _) = b.block(&[]);
    b.br_if(b.v, t, f);
    b.at(t);
    b.s("true");
    b.goto(j, vec![]);
    b.at(f);
    b.s("false");
    b.goto(j, vec![]);
    b.at(j);
    b.finish(&key("bool"))
}

/// `c` の `n` ビット右シフト。
fn shr(b: &mut Fb, c: VId, n: i128) -> VId {
    let k = b.c(Ty::U32, n);
    b.emit(Ty::U32, Op::Shift { left: false, a: c, n: k })
}

fn bit(b: &mut Fb, op: BoolOp, x: VId, k: i128) -> VId {
    let kv = b.c(Ty::U32, k);
    b.emit(Ty::U32, Op::Bool(op, x, kv))
}

/// 16 進の 1 桁 (`d` は 0..16 の u32) を書く。表の中の 1 バイトを指して書くので、複写はしない。
fn hex_digit(b: &mut Fb, d: VId) {
    let table = b.emit(Ty::Ptr, Op::StaticBytes(b"0123456789abcdef".to_vec()));
    let du = b.emit(Ty::Usize, Op::Conv { src: Ty::U32, a: d });
    let p = b.emit(Ty::Ptr, Op::PtrAdd { p: table, off: du });
    let one = b.c(Ty::Usize, 1);
    b.put(p, one);
}

fn char_fn(fat: Ty, io: Ty) -> Func {
    let mut b = Fb::new(Ty::U32, fat, io);
    let c = b.v;
    let buf = b.emit(Ty::Ptr, Op::FrameBytes { size: 16 });
    b.s("'");
    let (close, _) = b.block(&[]);
    // 名前のある逃れ (`\'` `\\` `\n` `\r` `\t` `\0`)
    for (code, text) in [(0x27, "\\'"), (0x5c, "\\\\"), (0x0a, "\\n"), (0x0d, "\\r"), (0x09, "\\t"), (0x00, "\\0")] {
        let k = b.c(Ty::U32, code);
        let hit = b.cmp(Cc::Eq, Ty::U32, c, k);
        let (yes, _) = b.block(&[]);
        let (no, _) = b.block(&[]);
        b.br_if(hit, yes, no);
        b.at(yes);
        b.s(text);
        b.goto(close, vec![]);
        b.at(no);
    }
    // 制御文字 (0x20 未満と 0x7f) は `\u{..}`
    let k20 = b.c(Ty::U32, 0x20);
    let lo = b.cmp(Cc::Lt, Ty::U32, c, k20);
    let k7f = b.c(Ty::U32, 0x7f);
    let del = b.cmp(Cc::Eq, Ty::U32, c, k7f);
    let ctl = b.emit(Ty::Bool, Op::Bool(BoolOp::Or, lo, del));
    let (esc, _) = b.block(&[]);
    let (not_ctl, _) = b.block(&[]);
    b.br_if(ctl, esc, not_ctl);
    b.at(esc);
    b.s("\\u{");
    let k10 = b.c(Ty::U32, 16);
    let big = b.cmp(Cc::Ge, Ty::U32, c, k10);
    let (two, _) = b.block(&[]);
    let (low, _) = b.block(&[]);
    b.br_if(big, two, low);
    b.at(two);
    let hi = shr(&mut b, c, 4);
    hex_digit(&mut b, hi);
    b.goto(low, vec![]);
    b.at(low);
    let lo4 = bit(&mut b, BoolOp::And, c, 15);
    hex_digit(&mut b, lo4);
    b.s("}");
    b.goto(close, vec![]);
    b.at(not_ctl);
    // UTF-8 (1 〜 4 バイト)。バイトは u32 の下位 8 ビットを順に書く (次の書き込みが上位を上書きする。値は 256 未満)。
    let (emit, ev) = b.block(&[Ty::Usize]);
    let chain = |b: &mut Fb, limit: i128, n: usize| -> usize {
        // `c < limit` なら n バイトの形。そうでなければ次へ (戻り値は「そうでない」側のブロック)
        let kl = b.c(Ty::U32, limit);
        let small = b.cmp(Cc::Lt, Ty::U32, c, kl);
        let (yes, _) = b.block(&[]);
        let (no, _) = b.block(&[]);
        b.br_if(small, yes, no);
        b.at(yes);
        let lead: [i128; 4] = [0, 0xC0, 0xE0, 0xF0];
        let first = if n == 1 {
            c
        } else {
            let sh = shr(b, c, 6 * (n as i128 - 1));
            bit(b, BoolOp::Or, sh, lead[n - 1])
        };
        b.emit(Ty::Unit, Op::MemStore { ty: Ty::U32, p: buf, off: 0, v: first });
        for k in 1..n {
            let sh = shr(b, c, 6 * (n as i128 - 1 - k as i128));
            let low6 = bit(b, BoolOp::And, sh, 0x3f);
            let byte = bit(b, BoolOp::Or, low6, 0x80);
            b.emit(Ty::Unit, Op::MemStore { ty: Ty::U32, p: buf, off: k as u32, v: byte });
        }
        let len = b.c(Ty::Usize, n as i128);
        b.goto(emit, vec![len]);
        b.at(no);
        no
    };
    chain(&mut b, 0x80, 1);
    chain(&mut b, 0x800, 2);
    chain(&mut b, 0x10000, 3);
    // 残りは 4 バイト
    let first = {
        let sh = shr(&mut b, c, 18);
        bit(&mut b, BoolOp::Or, sh, 0xF0)
    };
    b.emit(Ty::Unit, Op::MemStore { ty: Ty::U32, p: buf, off: 0, v: first });
    for k in 1..4u32 {
        let sh = shr(&mut b, c, 6 * (3 - k as i128));
        let low6 = bit(&mut b, BoolOp::And, sh, 0x3f);
        let byte = bit(&mut b, BoolOp::Or, low6, 0x80);
        b.emit(Ty::Unit, Op::MemStore { ty: Ty::U32, p: buf, off: k, v: byte });
    }
    let four = b.c(Ty::Usize, 4);
    b.goto(emit, vec![four]);
    b.at(emit);
    b.put(buf, ev[0]);
    b.goto(close, vec![]);
    b.at(close);
    b.s("'");
    b.finish(&key("char"))
}

fn str_fn(fat: Ty, io: Ty) -> Func {
    let mut b = Fb::new(fat, fat, io);
    b.s("\"");
    let ptr = b.emit(Ty::Ptr, Op::Field { a: b.v, off: 0 });
    let len = b.emit(Ty::Usize, Op::Field { a: b.v, off: 8 });
    let (lp, lv) = b.block(&[Ty::Usize, Ty::Usize]);
    let (body, bv) = b.block(&[Ty::Usize, Ty::Usize]);
    let (spec, sv) = b.block(&[Ty::Usize, Ty::Usize, Ty::U8]);
    let (next, nv) = b.block(&[Ty::Usize, Ty::Usize]);
    let (end, ev) = b.block(&[Ty::Usize]);
    let z = b.c(Ty::Usize, 0);
    b.goto(lp, vec![z, z]);
    b.at(lp);
    let done = b.cmp(Cc::Ge, Ty::Usize, lv[0], len);
    b.term(Term::BrIf(done, (end, vec![lv[1]]), (body, vec![lv[0], lv[1]])));
    b.at(body);
    let bp = b.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: bv[0] });
    let ch = b.emit(Ty::U8, Op::MemLoad { p: bp, off: 0 });
    let mut special: Option<VId> = None;
    for code in [0x5c, 0x22, 0x0a] {
        let k = b.c(Ty::U8, code);
        let hit = b.cmp(Cc::Eq, Ty::U8, ch, k);
        special = Some(match special {
            None => hit,
            Some(s) => b.emit(Ty::Bool, Op::Bool(BoolOp::Or, s, hit)),
        });
    }
    b.term(Term::BrIf(special.expect("条件"), (spec, vec![bv[0], bv[1], ch]), (next, vec![bv[0], bv[1]])));
    b.at(spec);
    // 区間 [st, i) を書き、逃れを書いて、次の位置から区間を始め直す
    let rp = b.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: sv[1] });
    let rl = b.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: sv[0], b: sv[1] });
    b.put(rp, rl);
    let k5c = b.c(Ty::U8, 0x5c);
    let is_bs = b.cmp(Cc::Eq, Ty::U8, sv[2], k5c);
    let (bs, _) = b.block(&[]);
    let (not_bs, _) = b.block(&[]);
    let (after, _) = b.block(&[]);
    b.br_if(is_bs, bs, not_bs);
    b.at(bs);
    b.s("\\\\");
    b.goto(after, vec![]);
    b.at(not_bs);
    let k22 = b.c(Ty::U8, 0x22);
    let is_q = b.cmp(Cc::Eq, Ty::U8, sv[2], k22);
    let (q, _) = b.block(&[]);
    let (nl, _) = b.block(&[]);
    b.br_if(is_q, q, nl);
    b.at(q);
    b.s("\\\"");
    b.goto(after, vec![]);
    b.at(nl);
    b.s("\\n");
    b.goto(after, vec![]);
    b.at(after);
    let one = b.c(Ty::Usize, 1);
    let i1 = b.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: sv[0], b: one });
    b.goto(lp, vec![i1, i1]);
    b.at(next);
    let one = b.c(Ty::Usize, 1);
    let i2 = b.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: nv[0], b: one });
    b.goto(lp, vec![i2, nv[1]]);
    b.at(end);
    let rp = b.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off: ev[0] });
    let rl = b.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: len, b: ev[0] });
    b.put(rp, rl);
    b.s("\"");
    b.finish(&key("str"))
}

/// 生成の状態。
pub struct Gen<'a> {
    p: &'a Program,
    aggs: &'a RefCell<AggTab>,
    done: HashSet<String>,
    pub out: Vec<(Func, BTreeSet<String>)>,
}

impl<'a> Gen<'a> {
    pub fn new(p: &'a Program, aggs: &'a RefCell<AggTab>) -> Gen<'a> {
        Gen { p, aggs, done: HashSet::new(), out: vec![] }
    }

    fn io_fat(&self) -> Result<(Ty, Ty), String> {
        let io = self.aggs.borrow_mut().io_error(self.p)?;
        let fat = self.aggs.borrow_mut().fat();
        Ok((fat, io))
    }

    fn add(&mut self, f: Func, callees: BTreeSet<String>) {
        self.out.push((f, callees));
    }

    fn scalar(&mut self, name: &str, f: impl FnOnce() -> Func) -> String {
        let k = key(name);
        if self.done.insert(k.clone()) {
            self.add(f(), BTreeSet::new());
        }
        k
    }

    /// 入口から呼ぶ表示の関数 `ksn.entry.debug(v, fd)` (型 `ty` の値を、鍵 `k` の表示の関数へ渡す)。
    pub fn entry_wrapper(&mut self, k: &str, ty: Ty) -> Result<(), String> {
        let (fat, io) = self.io_fat()?;
        let mut b = Fb::new(ty, fat, io);
        b.emit(Ty::Unit, Op::Call { callee: k.to_string(), args: vec![b.v, b.fd] });
        let f = b.finish(ENTRY_DEBUG);
        self.add(f, [k.to_string()].into_iter().collect());
        Ok(())
    }

    /// 元の型 `src` (KIR の型 `ty`) の表示関数を (まだなければ) 作り、その鍵を返す。
    pub fn need(&mut self, src: &Src, ty: Ty, file: &str) -> Result<String, String> {
        let (fat, io) = self.io_fat()?;
        match (src, ty) {
            (Src::Prim(s), Ty::Bool) if s == "bool" => Ok(self.scalar("bool", || bool_fn(fat, io))),
            (Src::Prim(s), Ty::U32) if s == "char" => Ok(self.scalar("char", || char_fn(fat, io))),
            (Src::Prim(_), t) if t.is_int() && t.bits() <= 64 => Ok(self.scalar(&t.name(), || int_fn(t, fat, io))),
            (Src::Ref(_, inner), t) if t == fat && matches!(&**inner, Src::Prim(s) if s == "str") => Ok(self.scalar("str", || str_fn(fat, io))),
            (Src::Named(name, args), Ty::Agg(i)) if args.is_empty() => self.agg(name, i, file),
            (Src::Tuple(_), _) => Err("タプルの Debug 表示は未対応".to_string()),
            (other, t) => Err(format!("型 {} ({}) の Debug 表示は未対応", show(other), t.name())),
        }
    }

    fn agg(&mut self, name: &str, i: u32, _file: &str) -> Result<String, String> {
        let k = key(name);
        if !self.done.insert(k.clone()) {
            return Ok(k);
        }
        let p = self.p;
        let Some(d) = p.defs.iter().find(|d| d.name == name && (d.kw == "type" || d.kw == "enum")) else {
            return Err(format!("型 {} の定義がない", name));
        };
        if !d.derive.iter().any(|x| x == "Debug") {
            return Err(format!("型 {} は derive [Debug] を持たない", name));
        }
        if p.defs.iter().any(|x| x.kw == "fn" && x.name == format!("{}.debug_fmt", name)) {
            return Err(format!("型 {} は利用者の debug_fmt を持つ (未対応)", name));
        }
        let (fat, io) = self.io_fat()?;
        let def = self.aggs.borrow().defs[i as usize].clone();
        let short = name.rsplit('.').next().unwrap_or(name).to_string();
        let mut b = Fb::new(Ty::Agg(i), fat, io);
        let mut callees: BTreeSet<String> = BTreeSet::new();
        let gn = HashSet::new();
        // フィールドの表示: (元の型, 読み出し)
        let mut field = |g: &mut Gen, b: &mut Fb, fv: &crate::parser::V, ty: Ty, off: u32| -> Result<(), String> {
            let src = from_v(fv, &gn);
            let k = g.need(&src, ty, &d.file)?;
            let v = b.emit(ty, Op::Field { a: b.v, off });
            b.emit(Ty::Unit, Op::Call { callee: k.clone(), args: vec![v, b.fd] });
            callees.insert(k);
            Ok(())
        };
        match &def.kind {
            AggKind::Struct(fs) => {
                b.s(&format!("{} {{ ", short));
                for (j, ((_, fv, _), f)) in d.fields.iter().zip(fs).enumerate() {
                    if j > 0 {
                        b.s(", ");
                    }
                    b.s(&format!("{}: ", f.name));
                    field(self, &mut b, fv, f.ty, f.off)?;
                }
                b.s(" }");
            }
            AggKind::Enum(vs) => {
                let tag = b.emit(Ty::I32, Op::Tag(b.v));
                let (done, _) = b.block(&[]);
                for (k, (v, (_, tys))) in vs.iter().zip(&d.variants).enumerate() {
                    let last = k + 1 == vs.len();
                    let (yes, no) = if last {
                        (None, None)
                    } else {
                        let kk = b.c(Ty::I32, k as i128);
                        let hit = b.cmp(Cc::Eq, Ty::I32, tag, kk);
                        let (y, _) = b.block(&[]);
                        let (n, _) = b.block(&[]);
                        b.br_if(hit, y, n);
                        (Some(y), Some(n))
                    };
                    if let Some(y) = yes {
                        b.at(y);
                    }
                    b.s(&v.name);
                    if !v.fields.is_empty() {
                        b.s("(");
                        for (j, (fv, f)) in tys.iter().zip(&v.fields).enumerate() {
                            if j > 0 {
                                b.s(", ");
                            }
                            field(self, &mut b, fv, f.ty, f.off)?;
                        }
                        b.s(")");
                    }
                    b.goto(done, vec![]);
                    if let Some(n) = no {
                        b.at(n);
                    }
                }
                if vs.is_empty() {
                    b.goto(done, vec![]);
                }
                b.at(done);
            }
            AggKind::Tuple(_) => return Err("タプルの Debug 表示は未対応".to_string()),
        }
        let f = b.finish(&k);
        self.add(f, callees);
        Ok(k)
    }
}
