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
//!   `derive [Debug]` を持たない型・利用者の `debug_fmt` がある型・タプル・128 ビット整数は、理由付きで未対応 (`Vec<T>` / `String` は要素の表示に従う)。
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
    if SINK_MODE.with(|c| c.get()) {
        format!("{}.$debug.s", name)
    } else {
        format!("{}.$debug", name)
    }
}

/// 関数の鍵に使う型の名前。KIR の文字は `(` を含められない (`call` の括弧の読みが最初の `(` で切れる) ので、タプルの `(` は `tup<` に直す。
fn kname(show_src: &str) -> String {
    show_src.replace(' ', "").replace('(', "tup<").replace(')', ">")
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
    /// 出力先が `String` のとき、その先頭の `{ptr, len, cap}` を指す引数 (最後の引数)。`None` なら stderr。
    sink: Option<VId>,
}

thread_local! {
    /// 表示の関数を作る向き: `true` なら `String` へ追記する版 (`<型>.$debug.s`。最後の引数が `String` の先頭への参照)。
    static SINK_MODE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `String` へバイト列を追記する関数の鍵 `(h, ptr, len)`。
pub const APPEND: &str = "ksn.fmt.append";

impl Fb {
    /// 表示の関数用: `SINK_MODE` なら最後に `String` の参照の引数を足す。
    fn new_d(params: &[Ty], fat: Ty) -> Fb {
        if SINK_MODE.with(|c| c.get()) {
            let mut ps = params.to_vec();
            ps.push(Ty::Ptr);
            let mut b = Fb::new(&ps, fat);
            b.sink = Some(ps.len() as u32 - 1);
            b
        } else {
            Fb::new(params, fat)
        }
    }

    /// 呼び先へ渡す引数 (出力先が `String` なら最後に足す)。
    fn sargs(&self, mut a: Vec<VId>) -> Vec<VId> {
        if let Some(h) = self.sink {
            a.push(h);
        }
        a
    }

    fn new(params: &[Ty], fat: Ty) -> Fb {
        let ids: Vec<VId> = (0..params.len() as u32).collect();
        let ps = ids.iter().copied().zip(params.iter().copied()).collect();
        let v = ids.first().copied().unwrap_or(0);
        let w = ids.get(1).copied().unwrap_or(0);
        Fb { blocks: vec![(ps, vec![], None)], n: params.len() as u32, cur: 0, fat, v, w, sink: None }
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
        if let Some(h) = self.sink {
            self.emit(Ty::Unit, Op::Call { callee: APPEND.to_string(), args: vec![h, ptr, len] });
            return;
        }
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
            uses: if self.sink.is_some() || key == APPEND { vec!["alloc".to_string()] } else { vec![] },
            may_panic: may && self.sink.is_none() && key != APPEND,
            stack: None,
            ret,
            link: None,
            blocks: self.blocks.into_iter().map(|(params, insts, t)| Block { params, insts, term: t.unwrap_or(Term::Unreachable) }).collect(),
        }
    }
}

/// 10 進の整数 (128 ビットは対象外)。符号付きは `-` と大きさ (u64) に分け、桁を末尾から `buf` に置く。
fn int_fn(t: Ty, fat: Ty) -> Func {
    let mut b = Fb::new_d(&[t], fat);
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

/// `&str` / `String` の中身をそのまま `String` へ書く (`Display`)。値の先頭 2 語が `{ptr, len}`。
/// `f32` / `f64` の表示: 512 バイトの枠 (`{}` は指数を使わず、非正規化数は 330 字ほど) へ書式化して (`FloatText`)、書いた分を出す。
fn float_fn(t: Ty, fat: Ty) -> Func {
    let mut b = Fb::new_d(&[t], fat);
    let buf = b.emit(Ty::Ptr, Op::FrameBytes { size: 512 });
    let n = b.emit(Ty::Usize, Op::FloatText { bits: if t == Ty::F32 { 32 } else { 64 }, a: b.v, dst: buf });
    b.put(buf, n);
    b.finish(&key(&format!("{}.disp", t.name())))
}

fn raw_fn(ty: Ty, fat: Ty, name: &str) -> Func {
    let mut b = Fb::new_d(&[ty], fat);
    let (p, l) = if ty == Ty::Ptr {
        (b.emit(Ty::Ptr, Op::MemLoad { p: b.v, off: 0 }), b.emit(Ty::Usize, Op::MemLoad { p: b.v, off: 8 }))
    } else {
        (b.emit(Ty::Ptr, Op::Field { a: b.v, off: 0 }), b.emit(Ty::Usize, Op::Field { a: b.v, off: 8 }))
    };
    b.put(p, l);
    b.finish(&key(name))
}

/// `ksn.fmt.append(h, ptr, len)`: 文字列 `h` (`{ptr, len, cap}`) の末尾へ `len` バイトを書く (足りなければ広げる)。
pub(crate) fn append_fn(fat: Ty) -> Func {
    let mut b = Fb::new(&[Ty::Ptr, Ty::Ptr, Ty::Usize], fat);
    let (h, src, n) = (0 as VId, 1 as VId, 2 as VId);
    let len = b.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let cap = b.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 });
    let need = b.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: len, b: n });
    let full = b.cmp(Cc::Gt, Ty::Usize, need, cap);
    let (grow, _) = b.block(&[]);
    let (cont, _) = b.block(&[]);
    b.br_if(full, grow, cont);
    b.at(grow);
    let old = b.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let newcap = b.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: need, b: need });
    let np = b.emit(Ty::Ptr, Op::HeapRealloc { p: old, size: newcap });
    b.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: h, off: 0, v: np });
    b.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 16, v: newcap });
    b.goto(cont, vec![]);
    b.at(cont);
    let base = b.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let dst = b.emit(Ty::Ptr, Op::PtrAdd { p: base, off: len });
    let z = b.c(Ty::Usize, 0);
    let (lp, lv) = b.block(&[Ty::Usize]);
    let (body, _) = b.block(&[]);
    let (end, _) = b.block(&[]);
    b.goto(lp, vec![z]);
    b.at(lp);
    let done = b.cmp(Cc::Ge, Ty::Usize, lv[0], n);
    b.term(Term::BrIf(done, (end, vec![]), (body, vec![])));
    b.at(body);
    let sp = b.emit(Ty::Ptr, Op::PtrAdd { p: src, off: lv[0] });
    let byte = b.emit(Ty::U8, Op::MemLoad { p: sp, off: 0 });
    let dp = b.emit(Ty::Ptr, Op::PtrAdd { p: dst, off: lv[0] });
    b.emit(Ty::Unit, Op::MemStore { ty: Ty::U8, p: dp, off: 0, v: byte });
    let one = b.c(Ty::Usize, 1);
    let i1 = b.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: lv[0], b: one });
    b.goto(lp, vec![i1]);
    b.at(end);
    b.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: need });
    b.finish(APPEND)
}

fn bool_fn(fat: Ty) -> Func {
    let mut b = Fb::new_d(&[Ty::Bool], fat);
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
    let mut b = Fb::new_d(&[Ty::U32], fat);
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
    let mut b = Fb::new_d(&[fat], fat);
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
    ty == fat && matches!(src, Src::Ref(_, inner, _) if matches!(&**inner, Src::Slice(e) if matches!(&**e, Src::Prim(s) if s == "u8")))
}

fn is_str(src: &Src, ty: Ty, fat: Ty) -> bool {
    ty == fat && matches!(src, Src::Ref(_, inner, _) if matches!(&**inner, Src::Prim(s) if s == "str"))
}

/// Option / Result の 2 つの枝 (タグが真の枝, タグが偽の枝)。各枝は (変種の名前, ペイロード (元の型, KIR の型))。
/// `Option`: 真が `Some(T)`、偽が `None`。`Result`: 真が `Err(E)`、偽が `Ok(T)` (`unwrap` と同じタグの読み方)。
type Arm = (&'static str, Option<(Src, Ty)>);
fn two_arms(src: &Src, ty: Ty) -> Option<(Arm, Arm)> {
    match (src, ty) {
        (Src::Named(n, a, _), Ty::Opt(p)) if n == "Option" && a.len() == 1 => Some((("Some", Some((a[0].clone(), p.ty()))), ("None", None))),
        (Src::Named(n, a, _), Ty::Res(t0, e0)) if n == "Result" && a.len() == 2 => {
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
    /// `String` へ追記する版を作るか (`String.push_display` / `push_debug` の下ろし)。
    sink: bool,
}

impl<'a> Gen<'a> {
    pub fn new(p: &'a Program, aggs: &'a RefCell<AggTab>) -> Gen<'a> {
        SINK_MODE.with(|c| c.set(false));
        Gen { p, aggs, done: HashSet::new(), out: vec![], sink: false }
    }

    /// `String` へ追記する版の表示の関数を作る生成 (鍵は `<型>.$debug.s`、最後の引数が `String` の先頭への参照)。
    pub fn new_sink(p: &'a Program, aggs: &'a RefCell<AggTab>) -> Gen<'a> {
        SINK_MODE.with(|c| c.set(true));
        Gen { p, aggs, done: HashSet::new(), out: vec![], sink: true }
    }

    /// 型 `src` を `Display` で `String` へ書く関数 `(値, String の先頭への参照)` の鍵。
    /// 整数・bool は `Debug` と同じ表示、`&str` / `String` はそのまま書く。それ以外 (`char` や利用者の `Display`) は未対応。
    pub fn need_display(&mut self, src: &Src, ty: Ty, file: &str) -> Result<String, String> {
        SINK_MODE.with(|c| c.set(self.sink));
        let fat = self.fat()?;
        let (src, ty) = Self::scalar_ref_src(src, ty);
        match (src, ty) {
            (Src::Prim(s), Ty::Bool) if s == "bool" => self.need(src, ty, file),
            (Src::Prim(s), Ty::U32) if s == "char" => Err("char の Display の String への追記は未対応".to_string()),
            (Src::Prim(_), t) if t.is_int() && t.bits() <= 64 => self.need(src, ty, file),
            // `f32` / `f64` の Display は Debug と同じ文字列 (`py_float` / `py_float32`)。
            (Src::Prim(_), t) if t.is_float() => Ok(self.scalar(&format!("{}.disp", t.name()), || float_fn(t, fat))),
            (_, t) if is_str(src, t, fat) => Ok(self.scalar("str.raw", || raw_fn(fat, fat, "str.raw"))),
            (Src::Named(name, _, _), t) if name == "alloc.string.String" && (matches!(t, Ty::Agg(_)) || t == Ty::Ptr) => {
                Ok(self.scalar("string.raw", || raw_fn(t, fat, "string.raw")))
            }
            (Src::Ref(_, inner, _), t) => self.need_display(inner, t, file),
            (other, t) => Err(format!("型 {} ({}) の Display の String への追記は未対応", show(other), t.name())),
        }
    }

    /// `eprint` の引数の型 (`fat`)。`std.io.Error` の表も入れる (LLVM の `write` の実体が要る)。
    fn fat(&self) -> Result<Ty, String> {
        self.aggs.borrow_mut().stderr_fat(self.p)
    }

    fn add(&mut self, f: Func, mut callees: BTreeSet<String>) {
        if self.sink {
            callees.insert(APPEND.to_string());
            if self.done.insert(APPEND.to_string()) {
                let fat = self.fat().unwrap_or(Ty::Ptr);
                self.out.push((append_fn(fat), BTreeSet::new()));
            }
        }
        self.out.push((f, callees));
    }

    fn scalar(&mut self, name: &str, f: impl FnOnce() -> Func) -> String {
        let k = key(name);
        if self.done.insert(k.clone()) {
            let mut func = f();
            func.key = k.clone();
            func.fqn = k.clone();
            self.add(func, BTreeSet::new());
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
        SINK_MODE.with(|c| c.set(self.sink));
        let fat = self.fat()?;
        // `&T` のソース型で、KIR の型が T のスカラーなら、T として扱う
        // (参照先を読んだ値。`Option<&u8>` のペイロードなど)。
        let (src, ty) = Self::scalar_ref_src(src, ty);
        match (src, ty) {
            (Src::Prim(s), Ty::Bool) if s == "bool" => Ok(self.scalar("bool", || bool_fn(fat))),
            (Src::Prim(s), Ty::U32) if s == "char" => Ok(self.scalar("char", || char_fn(fat))),
            (Src::Prim(_), t) if t.is_int() && t.bits() <= 64 => Ok(self.scalar(&t.name(), || int_fn(t, fat))),
            // `f32` / `f64` の Debug は Display と同じ文字列。
            (Src::Prim(_), t) if t.is_float() => Ok(self.scalar(&format!("{}.disp", t.name()), || float_fn(t, fat))),
            (_, t) if is_str(src, t, fat) => Ok(self.scalar("str", || str_fn(fat))),
            (Src::Named(name, _, _), t) if name == "alloc.string.String" && (matches!(t, Ty::Agg(_)) || (self.sink && t == Ty::Ptr)) => Ok(self.string_debug(t, fat)),
            // `Vec<T>` (T は `need` が表示できる型なら何でも): 先頭の {ptr, len} を `&[T]` の表示 (`[a, b]`) へ渡す。
            (Src::Named(name, args, _), t) if name == "alloc.vec.Vec" && !args.is_empty() && (matches!(t, Ty::Agg(_)) || (self.sink && t == Ty::Ptr)) => {
                self.vec_debug(&args[0], t, fat, file)
            }
            (Src::Named(name, args, _), Ty::Agg(i)) if args.is_empty() => self.agg(name, i, file),
            (_, t) if two_arms(src, t).is_some() => self.two(src, t, file, false),
            (Src::Slice(e) | Src::Array(e, _), t) if t == fat => self.slice_debug(e, file),
            (Src::Tuple(es), Ty::Agg(i)) => self.tuple(es, i, file),
            (Src::Ref(_, inner, _), t) => self.need(inner, t, file),
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
            (_, t) if is_str(src, t, fat) || is_bytes(src, t, fat) => Ok(self.str_eq(fat, fat, "str")),
            // `&[T]` / `&[T; N]` (T は 64 ビット以下の整数・bool・char): 長さと要素ごとの比較。
            (Src::Ref(_, inner, _), t) if t == fat && matches!(&**inner, Src::Slice(_) | Src::Array(_, _)) => {
                let (Src::Slice(e) | Src::Array(e, _)) = &**inner else { unreachable!() };
                match super::conv(e).filter(|x| (x.is_int() && x.bits() <= 64) || *x == Ty::Bool || x.is_float()) {
                    Some(et) => Ok(self.slice_eq_elem(fat, fat, &format!("slice.{}", et.name()), et)),
                    None => Err(format!("要素の型 {} の比較は未対応", show(e))),
                }
            }
            // `String` の等しさ: 値は {ptr, len, cap} の `vec`。先頭の {ptr, len} だけを `&str` と同じ規則で比べる (設計_KIR.md 23 章)。
            (Src::Named(name, _, _), t) if name == "alloc.string.String" && matches!(t, Ty::Agg(_)) => Ok(self.str_eq(fat, t, "string")),
            // 局所変数の `String` はヘッダの参照 (`ptr`) で渡る。
            (Src::Named(name, _, _), Ty::Ptr) if name == "alloc.string.String" => Ok(self.str_eq(fat, Ty::Ptr, "string.ptr")),
            (Src::Named(name, args, _), Ty::Agg(i)) => {
                // 型引数付きの集成体 (`alloc.arena.Handle<T>` など) は具体化した名前を鍵にする (欄は総称のままなので、欄がスカラー以外なら下で理由付きで落ちる)。
                if !args.is_empty() {
                    let inst = self.aggs.borrow().generic_args.get(&i).filter(|(fq, _)| fq == name).map(|(fq, as_)| {
                        format!("{}<{}>", fq, as_.iter().map(show).collect::<Vec<_>>().join(","))
                    });
                    let Some(inst) = inst else { return Err(format!("型 {} ({}) の比較は未対応", show(src), ty.name())) };
                    return self.agg_eq(name, &inst, i, file);
                }
                self.agg_eq(name, name, i, file)
            }
            (Src::Tuple(es), Ty::Agg(i)) => self.tuple_eq(es, i, file),
            (_, t) if two_arms(src, t).is_some() => self.two(src, t, file, true),
            // 浮動小数は IEEE の == (`fcmp oeq`。NaN は不一致、-0.0 == 0.0。木走査の veq と同じ)。
            (Src::Prim(_), t) if t.is_float() => Ok(self.scalar_eq(&t.name(), t, fat)),
            (Src::Ref(_, inner, _), t) => self.need_eq(inner, t, file),
            (other, t) => Err(format!("型 {} ({}) の比較は未対応", show(other), t.name())),
        }
    }

    /// `&T` のソース型で、KIR の型が T のスカラーなら、T として扱う。
    /// 参照先を読んだ値 (`Option<&u8>` のペイロードや、`core.check.eq(&a, &b)` の両辺) に対して使う。
    fn scalar_ref_src(src: &Src, ty: Ty) -> (&Src, Ty) {
        match src {
            Src::Ref(false, inner, _) if super::conv(inner) == Some(ty) => (inner, ty),
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

    /// `&[T]` / `&[T; N]` の表示 (`[a, b, c]`。木走査の `debug_str` と同じ)。値は `fat` (`{ptr, len}`)。
    /// 要素は `need` が表示できる型なら何でもよい (`String`・構造体・入れ子の `Vec` など。スカラーだけでなく)。
    /// 要素の表示の関数を要素ごとに呼ぶ (区切りは `, `)。
    fn slice_debug(&mut self, e: &Src, file: &str) -> Result<String, String> {
        let fat = self.fat()?;
        // 要素の KIR の型は型表に入れる側で決める (`String` など所有物を持つ要素も含む)。
        let et = self
            .aggs
            .borrow_mut()
            .lower_ty(self.p, file, e)
            .map_err(|w| format!("要素の型 {} の Debug 表示は未対応 ({})", show(e), w))?;
        let size = crate::kir::size_of(&self.aggs.borrow().defs, et);
        let name = format!("slice<{}>", show(e).replace(' ', ""));
        let k = key(&name);
        if !self.done.insert(k.clone()) {
            return Ok(k);
        }
        let ek = self.need(e, et, file)?;
        let mut b = Fb::new_d(&[fat], fat);
        let ptr = b.emit(Ty::Ptr, Op::Field { a: b.v, off: 0 });
        let len = b.emit(Ty::Usize, Op::Field { a: b.v, off: 8 });
        b.s("[");
        let z = b.c(Ty::Usize, 0);
        let (lp, lv) = b.block(&[Ty::Usize]);
        let (body, _) = b.block(&[]);
        let (sep, _) = b.block(&[]);
        let (elem, _) = b.block(&[]);
        let (end, _) = b.block(&[]);
        b.goto(lp, vec![z]);
        b.at(lp);
        let done = b.cmp(Cc::Ge, Ty::Usize, lv[0], len);
        b.term(Term::BrIf(done, (end, vec![]), (body, vec![])));
        b.at(body);
        let zero = b.c(Ty::Usize, 0);
        let first = b.cmp(Cc::Eq, Ty::Usize, lv[0], zero);
        b.term(Term::BrIf(first, (elem, vec![]), (sep, vec![])));
        b.at(sep);
        b.s(", ");
        b.goto(elem, vec![]);
        b.at(elem);
        let sz = b.c(Ty::Usize, size as i128);
        let off = b.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: lv[0], b: sz });
        let p = b.emit(Ty::Ptr, Op::PtrAdd { p: ptr, off });
        let x = b.emit(et, Op::MemLoad { p, off: 0 });
        let a = b.sargs(vec![x]);
        b.emit(Ty::Unit, Op::Call { callee: ek.clone(), args: a });
        let one = b.c(Ty::Usize, 1);
        let i1 = b.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: lv[0], b: one });
        b.goto(lp, vec![i1]);
        b.at(end);
        b.s("]");
        let f = b.finish(&k);
        self.add(f, [ek].into_iter().collect());
        Ok(k)
    }

    /// `&str` の等しさ: 長さが等しく、バイトがすべて一致する。
    /// `Vec<T>` の表示: 値は {ptr, len, cap} の `vec` (局所はヘッダの参照 `ptr`)。先頭の {ptr, len} から `&[T]` を作って
    /// `slice_debug` に渡す (木走査の `debug_str` と同じ `[a, b]`。要素が `String` なら `"aa"`、入れ子の `Vec` も recursing)。
    fn vec_debug(&mut self, e: &Src, ty: Ty, fat: Ty, file: &str) -> Result<String, String> {
        let name = format!("vec<{}>", show(e).replace(' ', ""));
        let k = key(&name);
        let sk = self.slice_debug(e, file)?;
        if !self.done.insert(k.clone()) {
            return Ok(k);
        }
        let mut b = Fb::new_d(&[ty], fat);
        let (p, l) = if ty == Ty::Ptr {
            (b.emit(Ty::Ptr, Op::MemLoad { p: b.v, off: 0 }), b.emit(Ty::Usize, Op::MemLoad { p: b.v, off: 8 }))
        } else {
            (b.emit(Ty::Ptr, Op::Field { a: b.v, off: 0 }), b.emit(Ty::Usize, Op::Field { a: b.v, off: 8 }))
        };
        let s = b.emit(fat, Op::Make { variant: None, fields: vec![p, l] });
        let a = b.sargs(vec![s]);
        b.emit(Ty::Unit, Op::Call { callee: sk.clone(), args: a });
        let f = b.finish(&k);
        self.add(f, BTreeSet::from([sk]));
        Ok(k)
    }

    /// `String` の表示: 値は {ptr, len, cap} の `vec`。先頭の {ptr, len} を `&str` の表示へ渡す (木走査の `StrBuf` の表示と同じ引用)。
    fn string_debug(&mut self, ty: Ty, fat: Ty) -> String {
        let k = key("string");
        if !self.done.insert(k.clone()) {
            return k;
        }
        let sk = self.scalar("str", || str_fn(fat));
        let mut b = Fb::new_d(&[ty], fat);
        let (p, l) = if ty == Ty::Ptr {
            (b.emit(Ty::Ptr, Op::MemLoad { p: b.v, off: 0 }), b.emit(Ty::Usize, Op::MemLoad { p: b.v, off: 8 }))
        } else {
            (b.emit(Ty::Ptr, Op::Field { a: b.v, off: 0 }), b.emit(Ty::Usize, Op::Field { a: b.v, off: 8 }))
        };
        let s = b.emit(fat, Op::Make { variant: None, fields: vec![p, l] });
        let a = b.sargs(vec![s]);
        b.emit(Ty::Unit, Op::Call { callee: sk.clone(), args: a });
        let f = b.finish(&k);
        self.add(f, BTreeSet::from([sk]));
        k
    }

    /// `ty` は比べる値の型 (`&str` / `&[u8]` は `fat`、`String` は先頭の {ptr, len} を持つ `vec`)。
    fn str_eq(&mut self, fat: Ty, ty: Ty, name: &str) -> String {
        self.slice_eq_elem(fat, ty, name, Ty::U8)
    }

    /// 要素がスカラー `et` のスライス (`fat`) の等しさ: 長さが同じで、要素が先頭から順にすべて等しい。
    fn slice_eq_elem(&mut self, fat: Ty, ty: Ty, name: &str, et: Ty) -> String {
        let k = eq_key(name);
        if !self.done.insert(k.clone()) {
            return k;
        }
        let mut b = Fb::new(&[ty, ty], fat);
        let (a, w) = (b.v, b.w);
        let (fail, _) = b.block(&[]);
        let (pa, pb, la, lb) = if ty == Ty::Ptr {
            (
                b.emit(Ty::Ptr, Op::MemLoad { p: a, off: 0 }),
                b.emit(Ty::Ptr, Op::MemLoad { p: w, off: 0 }),
                b.emit(Ty::Usize, Op::MemLoad { p: a, off: 8 }),
                b.emit(Ty::Usize, Op::MemLoad { p: w, off: 8 }),
            )
        } else {
            (
                b.emit(Ty::Ptr, Op::Field { a, off: 0 }),
                b.emit(Ty::Ptr, Op::Field { a: w, off: 0 }),
                b.emit(Ty::Usize, Op::Field { a, off: 8 }),
                b.emit(Ty::Usize, Op::Field { a: w, off: 8 }),
            )
        };
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
        let sz = crate::kir::size_of(&self.aggs.borrow().defs, et);
        let off = if sz == 1 {
            lv[0]
        } else {
            let kk = b.c(Ty::Usize, sz as i128);
            b.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: lv[0], b: kk })
        };
        let qa = b.emit(Ty::Ptr, Op::PtrAdd { p: pa, off });
        let qb = b.emit(Ty::Ptr, Op::PtrAdd { p: pb, off });
        let ca = b.emit(et, Op::MemLoad { p: qa, off: 0 });
        let cb = b.emit(et, Op::MemLoad { p: qb, off: 0 });
        let eq = b.cmp(Cc::Eq, et, ca, cb);
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
        let name = kname(&show(src));
        let k = if eq { eq_key(&name) } else { key(&name) };
        if !self.done.insert(k.clone()) {
            return Ok(k);
        }
        let (t_arm, f_arm) = two_arms(src, ty).expect("two_arms を確かめてから呼ぶ");
        let fat = self.fat()?;
        let mut callees: BTreeSet<String> = BTreeSet::new();
        let params: Vec<Ty> = if eq { vec![ty, ty] } else { vec![ty] };
        let mut b = if eq { Fb::new(&params, fat) } else { Fb::new_d(&params, fat) };
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
                        let a = b.sargs(vec![pv]);
                        b.emit(Ty::Unit, Op::Call { callee: kk.clone(), args: a });
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
        let mut b = Fb::new_d(&[Ty::Agg(i)], fat);
        let mut callees: BTreeSet<String> = BTreeSet::new();
        let gn = HashSet::new();
        // フィールドの表示: (元の型, 読み出し)
        let mut field = |g: &mut Gen, b: &mut Fb, fv: &crate::parser::V, ty: Ty, off: u32| -> Result<(), String> {
            let src = from_v(fv, &gn);
            let k = g.need(&src, ty, &d.file)?;
            let v = b.emit(ty, Op::Field { a: b.v, off });
            let a = b.sargs(vec![v]);
            b.emit(Ty::Unit, Op::Call { callee: k.clone(), args: a });
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
            AggKind::Tuple(_) | AggKind::Array(..) => return Err("タプル・配列の Debug 表示は未対応".to_string()),
        }
        let f = b.finish(&k);
        self.add(f, callees);
        Ok(k)
    }

    /// タプルの表示 `(a, b)` (要素が 1 つなら `(a,)`、木走査の `debug_str` と同じ)。要素は元の型 `es` の表示の関数で書く。
    fn tuple(&mut self, es: &[Src], i: u32, file: &str) -> Result<String, String> {
        let name = kname(&show(&Src::Tuple(es.to_vec())));
        let k = key(&name);
        if !self.done.insert(k.clone()) {
            return Ok(k);
        }
        let fat = self.fat()?;
        let def = self.aggs.borrow().defs[i as usize].clone();
        let AggKind::Tuple(fs) = &def.kind else {
            return Err(format!("{} は集成体のタプルでない", name));
        };
        let mut b = Fb::new_d(&[Ty::Agg(i)], fat);
        let mut callees: BTreeSet<String> = BTreeSet::new();
        b.s("(");
        for (j, (src, f)) in es.iter().zip(fs).enumerate() {
            if j > 0 {
                b.s(", ");
            }
            let kk = self.need(src, f.ty, file)?;
            let v = b.emit(f.ty, Op::Field { a: b.v, off: f.off });
            let a = b.sargs(vec![v]);
            b.emit(Ty::Unit, Op::Call { callee: kk.clone(), args: a });
            callees.insert(kk);
        }
        if es.len() == 1 {
            b.s(",");
        }
        b.s(")");
        let f = b.finish(&k);
        self.add(f, callees);
        Ok(k)
    }

    /// タプルの等しさ: 要素を順に比べる (木走査の `v_eq` と同じ)。
    fn tuple_eq(&mut self, es: &[Src], i: u32, file: &str) -> Result<String, String> {
        let name = kname(&show(&Src::Tuple(es.to_vec())));
        let k = eq_key(&name);
        if !self.done.insert(k.clone()) {
            return Ok(k);
        }
        let fat = self.fat()?;
        let def = self.aggs.borrow().defs[i as usize].clone();
        let AggKind::Tuple(fs) = &def.kind else {
            return Err(format!("{} は集成体のタプルでない", name));
        };
        let mut b = Fb::new(&[Ty::Agg(i), Ty::Agg(i)], fat);
        let mut callees: BTreeSet<String> = BTreeSet::new();
        let (fail, _) = b.block(&[]);
        let fl: Vec<(Src, Ty, u32)> = es.iter().zip(fs).map(|(s, f)| (s.clone(), f.ty, f.off)).collect();
        self.field_eq(&mut b, &fl, fail, &mut callees, file)?;
        let t = b.c(Ty::Bool, 1);
        b.term(Term::Ret(t));
        b.at(fail);
        let f = b.c(Ty::Bool, 0);
        b.term(Term::Ret(f));
        let func = b.build(&k, Ty::Bool, false);
        self.add(func, callees);
        Ok(k)
    }

    /// 構造体・列挙型の等しさ。手書きの `eq` を持つ型は未対応 (呼ぶ関数の下ろしが要る)。
    fn agg_eq(&mut self, name: &str, key: &str, i: u32, file: &str) -> Result<String, String> {
        let k = eq_key(key);
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
            AggKind::Tuple(_) | AggKind::Array(..) => return Err("タプル・配列の比較は未対応".to_string()),
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
