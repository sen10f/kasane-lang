//! `derive [Debug]` の表示と、値の等しさ (`core.check.eq` の比較) を KIR の関数に下ろす (設計_KIR.md 12.3 / 13.2)。`kir_lower.rs` の子モジュール。
//!
//! 型ごとに 1 つずつ関数を作る:
//! - `<型>.$debug(v)`: 表示を stderr へ書く (`eprint` 命令)。入口の `main` の `Err(e)` の `Error: ...` と、`core.check.eq` の失敗の文言に使う。
//!   `eprint` は `uses` を要らないので、どの `panics may` の関数からも呼べる。
//! - `<型>.$eq(a, b) -> bool`: 木走査の `v_eq` と同じ等しさ (整数・bool・char・`&str` はバイトの一致・構造体・列挙型・
//!   `Option` / `Result` は構造的に比べる)。
//!
//! 表示は木走査の `debug_str` と同じ:
//! - 構造体は `名前 { f: v, g: w }` (名前は FQN の末尾)、列挙型は変種の名前 (ペイロードがあれば `名前(v, w)`)。
//!   `Option` / `Result` は `Some(v)` / `None` / `Ok(v)` / `Err(e)`。
//! - 整数は 10 進 (除算と剰余のループ。新しい命令は使わない)、`bool` は `true` / `false`、`char` は `'x'`、`&str` は `"…"`
//!   (`\` `"` 改行だけを `\\` `\"` `\n` にする。木走査の `py_quote`)。
//! - 型は元の定義 (`Def`) の型で分類する (KIR の型は `char` と `u32`、`&str` と `&[T]` を区別しないため)。
//!   `derive [Debug]` を持たない型・利用者の `debug_fmt` がある型・タプル・浮動小数・128 ビット整数・Vec / String は、理由付きで未対応。
//! - `char` の表示は Rust の `{:?}` と同じ (`\n` `\r` `\t` `\0` `\'` `\\` と、制御文字の `\u{..}`)。ASCII 以外は、
//!   常に表示できる文字として UTF-8 のまま出す (表示できない Unicode の `\u{..}` 化はしない)。
//!
//! 等しさ (`$eq`) は利用者の `eq` を持つ型を未対応にする (呼ぶ関数の下ろしが要る)。浮動小数は表示できないので、
//! 失敗の文言に値を出せず未対応 (比べること自体は IEEE の `==` で正しいが、表示との組で止める)。

use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet};

use crate::kir::{AggKind, Aop, Block, BoolOp, Cc, Func, Inst, Mode, Op, Term, Ty, VId};
use crate::program::Program;
use crate::types::{from_v, Ty as Src};

use super::agg::AggTab;
use super::show;

/// 型 `name` の表示の関数の鍵。
pub fn key(name: &str) -> String {
    format!("{}.$debug", name)
}

/// 型 `name` の等しさの関数の鍵。
pub fn eq_key(name: &str) -> String {
    format!("{}.$eq", name)
}

/// 入口の `main` の `Err(e)` を表示する関数の鍵 (`e` の型の表示の関数を呼ぶだけ)。LLVM の入口がこれを呼ぶ。
pub const ENTRY_DEBUG: &str = "ksn.entry.debug";

/// 関数 1 つを組み立てる道具。引数は 1 つ (表示) か 2 つ (等しさ)。`v` が 1 つ目、`w` が 2 つ目。
struct Fb {
    blocks: Vec<(Vec<(VId, Ty)>, Vec<Inst>, Option<Term>)>,
    n: u32,
    cur: usize,
    fat: Ty,
    v: VId,
    w: VId,
}

impl Fb {
    fn new(params: &[Ty], fat: Ty) -> Fb {
        let ids: Vec<VId> = (0..params.len() as u32).collect();
        let ps = ids.iter().copied().zip(params.iter().copied()).collect();
        let v = ids.first().copied().unwrap_or(0);
        let w = ids.get(1).copied().unwrap_or(0);
        Fb { blocks: vec![(ps, vec![], None)], n: params.len() as u32, cur: 0, fat, v, w }
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

    /// stderr へ `{ptr, len}` を書く (`eprint`)。
    fn put(&mut self, ptr: VId, len: VId) {
        let fat = self.fat;
        let f = self.emit(fat, Op::Make { variant: None, fields: vec![ptr, len] });
        self.emit(Ty::Unit, Op::Eprint { data: f });
    }

    /// stderr へ固定の文字列を書く。
    fn s(&mut self, s: &str) {
        let p = self.emit(Ty::Ptr, Op::StaticBytes(s.as_bytes().to_vec()));
        let n = self.c(Ty::Usize, s.len() as i128);
        self.put(p, n);
    }

    /// 表示の関数: unit を返して終わる。
    fn finish(mut self, key: &str) -> Func {
        let u = self.c(Ty::Unit, 0);
        self.term(Term::Ret(u));
        self.build(key, Ty::Unit, true)
    }

    /// 終端を置いた後の組み立て (終端のないブロックは到達しない扱い)。表示は `eprint` を使うので `may` を真にする。
    fn build(self, key: &str, ret: Ty, may: bool) -> Func {
        Func {
            fqn: key.to_string(),
            key: key.to_string(),
            uses: vec![],
            may_panic: may,
            stack: None,
            ret,
            link: None,
            blocks: self.blocks.into_iter().map(|(params, insts, t)| Block { params, insts, term: t.unwrap_or(Term::Unreachable) }).collect(),
        }
    }
}

/// 10 進の整数 (128 ビットは対象外)。符号付きは `-` と大きさ (u64) に分け、桁を末尾から `buf` に置く。
fn int_fn(t: Ty, fat: Ty) -> Func {
    let mut b = Fb::new(&[t], fat);
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

fn bool_fn(fat: Ty) -> Func {
    let mut b = Fb::new(&[Ty::Bool], fat);
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

fn char_fn(fat: Ty) -> Func {
    let mut b = Fb::new(&[Ty::U32], fat);
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

fn str_fn(fat: Ty) -> Func {
    let mut b = Fb::new(&[fat], fat);
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

/// `&str` の KIR の型か (`fat` と、元の型 `&str`)。
/// `&[u8]` (比較だけバイトの一致で下ろす。表示は未対応)。
fn is_bytes(src: &Src, ty: Ty, fat: Ty) -> bool {
    ty == fat && matches!(src, Src::Ref(_, inner) if matches!(&**inner, Src::Slice(e) if matches!(&**e, Src::Prim(s) if s == "u8")))
}

fn is_str(src: &Src, ty: Ty, fat: Ty) -> bool {
    ty == fat && matches!(src, Src::Ref(_, inner) if matches!(&**inner, Src::Prim(s) if s == "str"))
}

/// Option / Result の 2 つの枝 (タグが真の枝, タグが偽の枝)。各枝は (変種の名前, ペイロード (元の型, KIR の型))。
/// `Option`: 真が `Some(T)`、偽が `None`。`Result`: 真が `Err(E)`、偽が `Ok(T)` (`unwrap` と同じタグの読み方)。
type Arm = (&'static str, Option<(Src, Ty)>);
fn two_arms(src: &Src, ty: Ty) -> Option<(Arm, Arm)> {
    match (src, ty) {
        (Src::Named(n, a), Ty::Opt(p)) if n == "Option" && a.len() == 1 => Some((("Some", Some((a[0].clone(), p.ty()))), ("None", None))),
        (Src::Named(n, a), Ty::Res(t0, e0)) if n == "Result" && a.len() == 2 => {
            Some((("Err", Some((a[1].clone(), e0.ty()))), ("Ok", Some((a[0].clone(), t0.ty())))))
        }
        _ => None,
    }
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

    /// `eprint` の引数の型 (`fat`)。`std.io.Error` の表も入れる (LLVM の `write` の実体が要る)。
    fn fat(&self) -> Result<Ty, String> {
        self.aggs.borrow_mut().stderr_fat(self.p)
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

    /// 入口から呼ぶ表示の関数 `ksn.entry.debug(v)` (型 `ty` の値を、鍵 `k` の表示の関数へ渡す)。
    pub fn entry_wrapper(&mut self, k: &str, ty: Ty) -> Result<(), String> {
        let fat = self.fat()?;
        let mut b = Fb::new(&[ty], fat);
        b.emit(Ty::Unit, Op::Call { callee: k.to_string(), args: vec![b.v] });
        let u = b.c(Ty::Unit, 0);
        b.term(Term::Ret(u));
        let f = b.build(ENTRY_DEBUG, Ty::Unit, true);
        self.add(f, [k.to_string()].into_iter().collect());
        Ok(())
    }

    /// 元の型 `src` (KIR の型 `ty`) の表示関数を (まだなければ) 作り、その鍵を返す。
    pub fn need(&mut self, src: &Src, ty: Ty, file: &str) -> Result<String, String> {
        let fat = self.fat()?;
        // `&T` のソース型で、KIR の型が T のスカラーなら、T として扱う
        // (参照先を読んだ値。`Option<&u8>` のペイロードなど)。
        let (src, ty) = Self::scalar_ref_src(src, ty);
        match (src, ty) {
            (Src::Prim(s), Ty::Bool) if s == "bool" => Ok(self.scalar("bool", || bool_fn(fat))),
            (Src::Prim(s), Ty::U32) if s == "char" => Ok(self.scalar("char", || char_fn(fat))),
            (Src::Prim(_), t) if t.is_int() && t.bits() <= 64 => Ok(self.scalar(&t.name(), || int_fn(t, fat))),
            (_, t) if is_str(src, t, fat) => Ok(self.scalar("str", || str_fn(fat))),
            (Src::Named(name, args), Ty::Agg(i)) if args.is_empty() => self.agg(name, i, file),
            (_, t) if two_arms(src, t).is_some() => self.two(src, t, file, false),
            (Src::Tuple(_), _) => Err("タプルの Debug 表示は未対応".to_string()),
            (Src::Ref(_, inner), t) => self.need(inner, t, file),
            (other, t) => Err(format!("型 {} ({}) の Debug 表示は未対応", show(other), t.name())),
        }
    }

    /// 元の型 `src` (KIR の型 `ty`) の等しさの関数 `(a, b) -> bool` を (まだなければ) 作り、その鍵を返す。
    /// 木走査の `v_eq` と同じ比べ方。浮動小数は表示できないので未対応 (`core.check.eq` の失敗の文言に値を出せない)。
    pub fn need_eq(&mut self, src: &Src, ty: Ty, file: &str) -> Result<String, String> {
        let fat = self.fat()?;
        // `&T` のソース型で、KIR の型が T のスカラーなら、T として扱う
        // (参照先を読んだ値。`Option<&u8>` のペイロードなど)。
        let (src, ty) = Self::scalar_ref_src(src, ty);
        match (src, ty) {
            (Src::Prim(s), Ty::Bool) if s == "bool" => Ok(self.scalar_eq("bool", ty, fat)),
            (Src::Prim(s), Ty::U32) if s == "char" => Ok(self.scalar_eq("char", ty, fat)),
            (Src::Prim(_), t) if t.is_int() && t.bits() <= 64 => {
                let n = t.name();
                Ok(self.scalar_eq(&n, t, fat))
            }
            (_, t) if is_str(src, t, fat) || is_bytes(src, t, fat) => Ok(self.str_eq(fat)),
            (Src::Named(name, args), Ty::Agg(i)) if args.is_empty() => self.agg_eq(name, i, file),
            (_, t) if two_arms(src, t).is_some() => self.two(src, t, file, true),
            (Src::Prim(_), t) if t.is_float() => Err(format!("型 {} の比較は未対応 (浮動小数は失敗の文言に値を出せない)", t.name())),
            (Src::Ref(_, inner), t) => self.need_eq(inner, t, file),
            (other, t) => Err(format!("型 {} ({}) の比較は未対応", show(other), t.name())),
        }
    }

    /// `&T` のソース型で、KIR の型が T のスカラーなら、T として扱う。
    /// 参照先を読んだ値 (`Option<&u8>` のペイロードや、`core.check.eq(&a, &b)` の両辺) に対して使う。
    fn scalar_ref_src(src: &Src, ty: Ty) -> (&Src, Ty) {
        match src {
            Src::Ref(false, inner) if super::conv(inner) == Some(ty) => (inner, ty),
            _ => (src, ty),
        }
    }

    fn scalar_eq(&mut self, name: &str, ty: Ty, fat: Ty) -> String {
        let k = eq_key(name);
        if self.done.insert(k.clone()) {
            let mut b = Fb::new(&[ty, ty], fat);
            let r = b.cmp(Cc::Eq, ty, b.v, b.w);
            b.term(Term::Ret(r));
            let f = b.build(&k, Ty::Bool, false);
            self.add(f, BTreeSet::new());
        }
        k
    }

    /// `&str` の等しさ: 長さが等しく、バイトがすべて一致する。
    fn str_eq(&mut self, fat: Ty) -> String {
        let k = eq_key("str");
        if !self.done.insert(k.clone()) {
            return k;
        }
        let mut b = Fb::new(&[fat, fat], fat);
        let (a, w) = (b.v, b.w);
        let (fail, _) = b.block(&[]);
        let pa = b.emit(Ty::Ptr, Op::Field { a, off: 0 });
        let pb = b.emit(Ty::Ptr, Op::Field { a: w, off: 0 });
        let la = b.emit(Ty::Usize, Op::Field { a, off: 8 });
        let lb = b.emit(Ty::Usize, Op::Field { a: w, off: 8 });
        let same_len = b.cmp(Cc::Eq, Ty::Usize, la, lb);
        let (lp, lv) = b.block(&[Ty::Usize]);
        let (pre, _) = b.block(&[]);
        let (body, _) = b.block(&[]);
        let (yes, _) = b.block(&[]);
        let (next, _) = b.block(&[]);
        b.br_if(same_len, pre, fail);
        b.at(pre);
        let z = b.c(Ty::Usize, 0);
        b.goto(lp, vec![z]);
        b.at(lp);
        let done = b.cmp(Cc::Ge, Ty::Usize, lv[0], la);
        b.term(Term::BrIf(done, (yes, vec![]), (body, vec![])));
        b.at(body);
        let qa = b.emit(Ty::Ptr, Op::PtrAdd { p: pa, off: lv[0] });
        let qb = b.emit(Ty::Ptr, Op::PtrAdd { p: pb, off: lv[0] });
        let ca = b.emit(Ty::U8, Op::MemLoad { p: qa, off: 0 });
        let cb = b.emit(Ty::U8, Op::MemLoad { p: qb, off: 0 });
        let eq = b.cmp(Cc::Eq, Ty::U8, ca, cb);
        b.br_if(eq, next, fail);
        b.at(next);
        let one = b.c(Ty::Usize, 1);
        let i1 = b.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: lv[0], b: one });
        b.goto(lp, vec![i1]);
        b.at(yes);
        let t = b.c(Ty::Bool, 1);
        b.term(Term::Ret(t));
        b.at(fail);
        let f = b.c(Ty::Bool, 0);
        b.term(Term::Ret(f));
        let func = b.build(&k, Ty::Bool, false);
        self.add(func, BTreeSet::new());
        k
    }

    /// Option / Result の表示 (`eq == false`) か等しさ (`eq == true`)。
    fn two(&mut self, src: &Src, ty: Ty, file: &str, eq: bool) -> Result<String, String> {
        // 鍵は KIR の文字列の一部なので、空白を含めない (`Result<i32,i32>` のように書く)
        let name = show(src).replace(' ', "");
        let k = if eq { eq_key(&name) } else { key(&name) };
        if !self.done.insert(k.clone()) {
            return Ok(k);
        }
        let (t_arm, f_arm) = two_arms(src, ty).expect("two_arms を確かめてから呼ぶ");
        let fat = self.fat()?;
        let mut callees: BTreeSet<String> = BTreeSet::new();
        let params: Vec<Ty> = if eq { vec![ty, ty] } else { vec![ty] };
        let mut b = Fb::new(&params, fat);
        let (yes, _) = b.block(&[]);
        let (no, _) = b.block(&[]);
        if !eq {
            let done = b.block(&[]).0;
            let tag = b.emit(Ty::Bool, Op::Tag(b.v));
            b.br_if(tag, yes, no);
            for (blk, (name, pay)) in [(yes, t_arm), (no, f_arm)] {
                b.at(blk);
                b.s(name);
                if let Some((psrc, pty)) = pay {
                    b.s("(");
                    if pty == Ty::Unit {
                        b.s("()");
                    } else {
                        let pv = b.emit(pty, Op::Payload(b.v));
                        let kk = self.need(&psrc, pty, file)?;
                        b.emit(Ty::Unit, Op::Call { callee: kk.clone(), args: vec![pv] });
                        callees.insert(kk);
                    }
                    b.s(")");
                }
                b.goto(done, vec![]);
            }
            b.at(done);
            b.finish_ret_unit();
        } else {
            let fail = b.block(&[]).0;
            let (cont, _) = b.block(&[]);
            let (ta, tb) = (b.emit(Ty::Bool, Op::Tag(b.v)), b.emit(Ty::Bool, Op::Tag(b.w)));
            let same = b.cmp(Cc::Eq, Ty::Bool, ta, tb);
            b.br_if(same, cont, fail);
            b.at(cont);
            b.br_if(ta, yes, no);
            for (blk, (_, pay)) in [(yes, t_arm), (no, f_arm)] {
                b.at(blk);
                match pay {
                    None => {
                        let t = b.c(Ty::Bool, 1);
                        b.term(Term::Ret(t));
                    }
                    Some((_, Ty::Unit)) => {
                        let t = b.c(Ty::Bool, 1);
                        b.term(Term::Ret(t));
                    }
                    Some((psrc, pty)) => {
                        let pa = b.emit(pty, Op::Payload(b.v));
                        let pb = b.emit(pty, Op::Payload(b.w));
                        let kk = self.need_eq(&psrc, pty, file)?;
                        let r = b.emit(Ty::Bool, Op::Call { callee: kk.clone(), args: vec![pa, pb] });
                        callees.insert(kk);
                        let (ok, _) = b.block(&[]);
                        b.br_if(r, ok, fail);
                        b.at(ok);
                        let t = b.c(Ty::Bool, 1);
                        b.term(Term::Ret(t));
                    }
                }
            }
            b.at(fail);
            let f = b.c(Ty::Bool, 0);
            b.term(Term::Ret(f));
        }
        let f = if eq { b.build(&k, Ty::Bool, false) } else { b.build(&k, Ty::Unit, true) };
        self.add(f, callees);
        Ok(k)
    }

    /// 構造体・列挙型の表示。
    fn agg(&mut self, name: &str, i: u32, _file: &str) -> Result<String, String> {
        let k = key(name);
        if !self.done.insert(k.clone()) {
            return Ok(k);
        }
        let d = self.def_of(name, "Debug")?;
        let fat = self.fat()?;
        let def = self.aggs.borrow().defs[i as usize].clone();
        let short = name.rsplit('.').next().unwrap_or(name).to_string();
        let mut b = Fb::new(&[Ty::Agg(i)], fat);
        let mut callees: BTreeSet<String> = BTreeSet::new();
        let gn = HashSet::new();
        // フィールドの表示: (元の型, 読み出し)
        let mut field = |g: &mut Gen, b: &mut Fb, fv: &crate::parser::V, ty: Ty, off: u32| -> Result<(), String> {
            let src = from_v(fv, &gn);
            let k = g.need(&src, ty, &d.file)?;
            let v = b.emit(ty, Op::Field { a: b.v, off });
            b.emit(Ty::Unit, Op::Call { callee: k.clone(), args: vec![v] });
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

    /// 構造体・列挙型の等しさ。手書きの `eq` を持つ型は未対応 (呼ぶ関数の下ろしが要る)。
    fn agg_eq(&mut self, name: &str, i: u32, file: &str) -> Result<String, String> {
        let k = eq_key(name);
        if !self.done.insert(k.clone()) {
            return Ok(k);
        }
        let d = self.def_of(name, "PartialEq")?;
        let fat = self.fat()?;
        let def = self.aggs.borrow().defs[i as usize].clone();
        let mut b = Fb::new(&[Ty::Agg(i), Ty::Agg(i)], fat);
        let mut callees: BTreeSet<String> = BTreeSet::new();
        let (fail, _) = b.block(&[]);
        let gn = HashSet::new();
        match &def.kind {
            AggKind::Struct(fs) => {
                let fl: Vec<(Src, Ty, u32)> = d.fields.iter().zip(fs).map(|((_, fv, _), f)| (from_v(fv, &gn), f.ty, f.off)).collect();
                self.field_eq(&mut b, &fl, fail, &mut callees, file)?;
                let t = b.c(Ty::Bool, 1);
                b.term(Term::Ret(t));
            }
            AggKind::Enum(vs) => {
                let (ta, tb) = (b.emit(Ty::I32, Op::Tag(b.v)), b.emit(Ty::I32, Op::Tag(b.w)));
                let same = b.cmp(Cc::Eq, Ty::I32, ta, tb);
                let (cont, _) = b.block(&[]);
                b.br_if(same, cont, fail);
                b.at(cont);
                let (done, _) = b.block(&[]);
                for (kk, (v, (_, tys))) in vs.iter().zip(&d.variants).enumerate() {
                    let last = kk + 1 == vs.len();
                    let (yes, no) = if last {
                        (None, None)
                    } else {
                        let c = b.c(Ty::I32, kk as i128);
                        let hit = b.cmp(Cc::Eq, Ty::I32, ta, c);
                        let (y, _) = b.block(&[]);
                        let (n, _) = b.block(&[]);
                        b.br_if(hit, y, n);
                        (Some(y), Some(n))
                    };
                    if let Some(y) = yes {
                        b.at(y);
                    }
                    let fl: Vec<(Src, Ty, u32)> = tys.iter().zip(&v.fields).map(|(fv, f)| (from_v(fv, &gn), f.ty, f.off)).collect();
                    self.field_eq(&mut b, &fl, fail, &mut callees, file)?;
                    b.goto(done, vec![]);
                    if let Some(n) = no {
                        b.at(n);
                    }
                }
                if vs.is_empty() {
                    b.goto(done, vec![]);
                }
                b.at(done);
                let t = b.c(Ty::Bool, 1);
                b.term(Term::Ret(t));
            }
            AggKind::Tuple(_) => return Err("タプルの比較は未対応".to_string()),
        }
        b.at(fail);
        let f = b.c(Ty::Bool, 0);
        b.term(Term::Ret(f));
        let func = b.build(&k, Ty::Bool, false);
        self.add(func, callees);
        Ok(k)
    }

    /// 各フィールドを比べ、違えば `fail` へ飛ぶ。全部等しければ現在のブロックのまま続く。
    fn field_eq(&mut self, b: &mut Fb, fl: &[(Src, Ty, u32)], fail: usize, callees: &mut BTreeSet<String>, file: &str) -> Result<(), String> {
        for (src, ty, off) in fl {
            let kk = self.need_eq(src, *ty, file)?;
            let x = b.emit(*ty, Op::Field { a: b.v, off: *off });
            let y = b.emit(*ty, Op::Field { a: b.w, off: *off });
            let r = b.emit(Ty::Bool, Op::Call { callee: kk.clone(), args: vec![x, y] });
            callees.insert(kk);
            let (next, _) = b.block(&[]);
            b.br_if(r, next, fail);
            b.at(next);
        }
        Ok(())
    }

    /// 派生 (`derive [...]`) を持つ利用者の型の定義を探す。手書きの `debug_fmt` / `eq` がある型は未対応。
    fn def_of(&self, name: &str, derive: &str) -> Result<&'a crate::program::Def, String> {
        let p = self.p;
        let Some(d) = p.defs.iter().find(|d| d.name == name && (d.kw == "type" || d.kw == "enum")) else {
            return Err(format!("型 {} の定義がない", name));
        };
        if derive == "Debug" && !d.derive.iter().any(|x| x == "Debug") {
            return Err(format!("型 {} は derive [Debug] を持たない", name));
        }
        if derive == "Debug" && p.defs.iter().any(|x| x.kw == "fn" && x.name == format!("{}.debug_fmt", name)) {
            return Err(format!("型 {} は利用者の debug_fmt を持つ (未対応)", name));
        }
        if derive == "PartialEq" && p.defs.iter().any(|x| x.kw == "fn" && x.name == format!("{}.eq", name)) {
            return Err(format!("型 {} は利用者の eq を持つ (未対応。呼ぶ関数の下ろしが要る)", name));
        }
        Ok(d)
    }
}

impl Fb {
    /// 現在のブロックで unit を返して終わる。
    fn finish_ret_unit(&mut self) {
        let u = self.c(Ty::Unit, 0);
        self.term(Term::Ret(u));
    }
}
