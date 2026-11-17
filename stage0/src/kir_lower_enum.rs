//! 組み込みの `Option` / `Result` の下ろし (設計_KIR.md 11 章)。
//!
//! `kir_lower` の子モジュールとして読み込む (`kir_lower.rs` の `#[path]`)。親の非公開の項目
//! (`Lw` と、その変数・ブロックの操作) をそのまま使うので、親の構造を広げずに済む。扱うのは次の形だけ:
//!
//! - 構築: `Some(a)` `None` `Ok(a)` `Err(a)`
//! - 分岐: `match`。パターンは `Some(p)` `None` `Ok(p)` `Err(p)`、整数と bool のリテラル、`_`、名前。
//! - `e?`: `Option` は戻り値も `Option`、`Result` は E の型が戻り値の E と完全一致するときだけ (仕様 4.1、変換なし)。
//! - `checked_add` `checked_sub` `checked_mul` `checked_div` `checked_rem` (整数の受け手と引数)。
//!
//! 対象外の形に出会ったら理由を付けて止める。親はその関数を「未対応」にする。

use crate::check::{items, nf, nlineno as nline, nname, st, strs};
use crate::kir::{AggKind, Aop, BoolOp, Cc, Op, Pay, Pointee, Term, Ty, VId};
use crate::parser::V;
use crate::program::{FromVariant, Target as Tgt};
use crate::types::{from_v, Ty as Src};
use std::collections::HashSet;

use super::{conv, parse_int, show, Lw, LR};

/// パターン `p` を型 `src` (対象の型) に沿って分解し、束縛名とその型の組を `out` に積む。
/// 扱うのは `PBind` (名前)、`Some(p)` / `Ok(p)` / `Err(p)`、要素数の合うタプル `(p, ..)` だけ。
/// それ以外 (リテラル・`_`・構造体など) は何も積まない (型が決まらない束縛は種類を持たない)。
fn pat_src_binds(p: &V, src: &Src, out: &mut Vec<(String, Src)>) {
    match nname(p) {
        "PBind" => out.push((st(&nf(p)[0]), src.clone())),
        "PVariant" => {
            let segs = strs(&nf(p)[0]);
            let subs = items(&nf(p)[1]);
            if segs.len() != 1 || subs.len() != 1 {
                return;
            }
            let inner = match (segs[0].as_str(), src) {
                ("Some", Src::Named(n, a)) if n == "Option" && a.len() == 1 => &a[0],
                ("Ok", Src::Named(n, a)) if n == "Result" && a.len() == 2 => &a[0],
                ("Err", Src::Named(n, a)) if n == "Result" && a.len() == 2 => &a[1],
                _ => return,
            };
            pat_src_binds(&subs[0], inner, out);
        }
        "PTuple" => {
            let pats = items(&nf(p)[0]);
            if let Src::Tuple(elems) = src {
                if elems.len() == pats.len() {
                    for (sub, et) in pats.iter().zip(elems.iter()) {
                        pat_src_binds(sub, et, out);
                    }
                }
            }
        }
        _ => {}
    }
}

/// 型の写し: `Option<P>` と `Result<T, E>` (ペイロードはスカラーだけ)。親の `conv` の受け皿。
pub(super) fn conv_builtin(t: &Src) -> Option<Ty> {
    match t {
        Src::Named(n, a) if n == "Option" && a.len() == 1 => Pay::of(conv(&a[0])?).map(Ty::Opt),
        Src::Named(n, a) if n == "Result" && a.len() == 2 => Some(Ty::Res(Pay::of(conv(&a[0])?)?, Pay::of(conv(&a[1])?)?)),
        _ => None,
    }
}

/// `checked_<op>` の演算。対象外のメソッドは `None`。
fn checked_op(m: &str) -> Option<Aop> {
    Some(match m {
        "checked_add" => Aop::Add,
        "checked_sub" => Aop::Sub,
        "checked_mul" => Aop::Mul,
        "checked_div" => Aop::Div,
        "checked_rem" => Aop::Rem,
        _ => return None,
    })
}

/// `checked_*` と `trunc_to_*` の名前か (受け手が式・フィールドのときの入口の判定)。
pub(super) fn is_checked_name(m: &str) -> bool {
    checked_op(m).is_some() || is_checked_extra(m)
}

/// `checked_shl` / `checked_shr` / `checked_abs` (範囲の検査が分岐になる族)。`checked_neg` は符号付きなら MIN 以外、符号なしなら 0 のときだけ `Some`。
fn is_checked_extra(m: &str) -> bool {
    matches!(m, "checked_shl" | "checked_shr" | "checked_abs" | "checked_neg") || m.starts_with("trunc_to_")
}

impl<'a> Lw<'a> {
    /// `recv.checked_{shl,shr,abs}(..)`: 条件が成り立つときだけ `Some(値)`、そうでなければ `None`。
    /// 条件と値は木走査 (`natives.rs`) と同じ: シフトは量が型の幅未満のとき `wrapping_*`、
    /// `abs` は符号付きで MIN でない。
    fn checked_extra(&mut self, e: &V, recv: VId, method: &str, args: &[V]) -> LR<Option<VId>> {
        let line = nline(e);
        let rt = self.vty(recv);
        if let Some(dst) = method.strip_prefix("trunc_to_") {
            return self.trunc_to(e, recv, dst, args);
        }
        if !rt.is_int() || rt.bits() > 64 {
            return self.err(line, format!("{} の受け手が 64 ビット以下の整数ではない", method));
        }
        let shift = matches!(method, "checked_shl" | "checked_shr");
        if args.len() != usize::from(shift) {
            return self.err(line, format!("{} の引数の個数が違う", method));
        }
        if method == "checked_abs" && !rt.signed() {
            return self.err(line, "checked_abs の受け手が符号付きではない");
        }
        let res = self.ty_of(e)?;
        let want = Ty::Opt(Pay::of(rt).expect("整数はスカラー"));
        if res != want {
            return self.err(line, format!("{} の結果の型 {} が {} と違う", method, res.name(), want.name()));
        }
        let amount = if shift {
            let Some(n) = self.expr(&args[0])? else { return Ok(None) };
            if self.vty(n) != Ty::U32 {
                return self.err(line, format!("{} の量が u32 ではない", method));
            }
            Some(n)
        } else {
            None
        };
        let ok = match (method, amount) {
            (_, Some(n)) => {
                let lim = self.emit(Ty::U32, Op::Const(rt.bits() as i128));
                self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: Ty::U32, a: n, b: lim })
            }
            ("checked_neg", None) if !rt.signed() => {
                let zero = self.emit(rt, Op::Const(0));
                self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: rt, a: recv, b: zero })
            }
            _ => {
                let min = self.emit(rt, Op::Const(rt.min()));
                self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ne, ty: rt, a: recv, b: min })
            }
        };
        let (yes, _) = self.new_block(&[]);
        let (no, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[res]);
        self.branch(ok, (yes, vec![]), (no, vec![]));
        self.cur = Some(yes);
        let v = match (method, amount) {
            ("checked_shl", Some(n)) => self.emit(rt, Op::Shift { left: true, a: recv, n }),
            ("checked_shr", Some(n)) => self.emit(rt, Op::Shift { left: false, a: recv, n }),
            ("checked_neg", None) if !rt.signed() => recv,
            ("checked_neg", None) => self.emit(rt, Op::Neg { a: recv, mode: crate::kir::Mode::Wrap }),
            _ => {
                let neg = self.emit(rt, Op::Neg { a: recv, mode: crate::kir::Mode::Wrap });
                self.emit(rt, Op::MinMax { max: true, a: recv, b: neg })
            }
        };
        let some = self.emit(res, Op::Some(v));
        self.goto(join, vec![some]);
        self.cur = Some(no);
        let none = self.emit(res, Op::Const(0));
        self.goto(join, vec![none]);
        self.cur = Some(join);
        Ok(Some(jp[0]))
    }

    /// `x.trunc_to_<int>()` (`x` は `f32` / `f64`): 0 へ丸めた値が整数型の範囲にあれば `Some`、NaN・無限大・範囲外は `None`。
    /// 範囲は丸めた値と `[min, max + 1)` (どちらも 2 の冪で浮動小数に正確に載る) の比較で決める (NaN は順序つきの比較で偽になる)。
    /// 取り出しは飽和する `Conv` (範囲内だけを通るので値は正確)。
    fn trunc_to(&mut self, e: &V, recv: VId, dst: &str, args: &[V]) -> LR<Option<VId>> {
        let line = nline(e);
        let ft = self.vty(recv);
        let Some(it) = Ty::parse(dst).filter(|t| t.is_int() && t.bits() <= 64) else {
            return self.err(line, format!("trunc_to_{} は段階 1 の対象外 (64 ビット以下の整数だけ)", dst));
        };
        if !ft.is_float() || !args.is_empty() {
            return self.err(line, "trunc_to_* の受け手が浮動小数ではない");
        }
        let res = self.ty_of(e)?;
        let want = Ty::Opt(Pay::of(it).expect("整数はスカラー"));
        if res != want {
            return self.err(line, format!("trunc_to_{} の結果の型 {} が {} と違う", dst, res.name(), want.name()));
        }
        let fbits = |v: f64| if ft == Ty::F32 { (v as f32).to_bits() as i128 } else { v.to_bits() as i128 };
        let t = self.emit(ft, Op::FUn { f: crate::kir::FUn::Trunc, src: ft, a: recv });
        let lo = self.emit(ft, Op::Const(fbits(it.min() as f64)));
        let hi = self.emit(ft, Op::Const(fbits(it.max() as f64 + 1.0)));
        let c_lo = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Ge, ty: ft, a: t, b: lo });
        let c_hi = self.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: ft, a: t, b: hi });
        let ok = self.emit(Ty::Bool, Op::Bool(BoolOp::And, c_lo, c_hi));
        let (yes, _) = self.new_block(&[]);
        let (no, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[res]);
        self.branch(ok, (yes, vec![]), (no, vec![]));
        self.cur = Some(yes);
        let v = self.emit(it, Op::Conv { src: ft, a: t });
        let some = self.emit(res, Op::Some(v));
        self.goto(join, vec![some]);
        self.cur = Some(no);
        let none = self.emit(res, Op::Const(0));
        self.goto(join, vec![none]);
        self.cur = Some(join);
        Ok(Some(jp[0]))
    }

    /// `None` (型は式の型表から決まる `Option<P>`)。
    pub(super) fn none_val(&mut self, e: &V) -> LR<VId> {
        let t = self.ty_of(e)?;
        if !matches!(t, Ty::Opt(_)) {
            return self.err(nline(e), format!("None の型 {} が Option ではない", t.name()));
        }
        Ok(self.emit(t, Op::Const(0)))
    }

    /// `Some(a)` / `Ok(a)` / `Err(a)` と、局所の整数変数の `checked_*` の呼び出し。対象外なら `None`。
    pub(super) fn builtin_call(&mut self, e: &V) -> Option<LR<Option<VId>>> {
        let f = nf(e);
        if nname(&f[0]) != "Path" {
            return None;
        }
        let segs = strs(&nf(&f[0])[0]);
        if segs.len() == 1 && matches!(segs[0].as_str(), "Some" | "Ok" | "Err") {
            return Some(self.ctor(e, &segs[0]));
        }
        if segs.len() == 2 && matches!(segs[1].as_str(), "is_some" | "is_none" | "is_ok" | "is_err" | "unwrap_or") {
            if let Some(s) = self.lookup(&segs[0]) {
                let recv = super::coll::path_peek(self, s);
                let local = super::coll::val_slot_of(self, s);
                let args = items(&f[1]).to_vec();
                if matches!(self.vty(recv), Ty::Opt(_) | Ty::Res(..)) {
                    return Some(self.optres_on(e, recv, local, &segs[1], &args));
                }
            }
        }
        if segs.len() == 2 && segs[1] == "next" && items(&f[1]).is_empty() {
            if let Some(s) = self.lookup(&segs[0]) {
                if self.slots[s].1 == self.fat_ty() {
                    return Some(self.chars_next(e, s));
                }
                if matches!(self.slots[s].1, Ty::Agg(i) if self.aggs.borrow().defs[i as usize].name == "split_it") {
                    return Some(self.split_next(e, s));
                }
            }
        }
        if segs.len() == 2 && matches!(segs[1].as_str(), "map" | "and_then") && items(&f[1]).len() == 1 {
            if let Some(s) = self.lookup(&segs[0]) {
                let recv = super::coll::path_peek(self, s);
                if matches!(self.vty(recv), Ty::Opt(_)) {
                    let args = items(&f[1]).to_vec();
                    return Some(self.opt_map(e, recv, segs[1] == "and_then", &args));
                }
                if matches!(self.vty(recv), Ty::Res(..)) {
                    let recv = if super::coll::owns(self, self.vty(recv)) {
                        let r = super::coll::path_read(self, s);
                        self.own_emit(&segs[0], s, crate::kir::OwnKind::Consume);
                        r
                    } else {
                        recv
                    };
                    let args = items(&f[1]).to_vec();
                    return Some(self.res_map(e, recv, &segs[1], &args));
                }
            }
        }
        if segs.len() == 2 && segs[1] == "ok" && items(&f[1]).is_empty() {
            if let Some(s) = self.lookup(&segs[0]) {
                let recv = super::coll::path_peek(self, s);
                if matches!(self.vty(recv), Ty::Res(..)) {
                    let recv = if super::coll::owns(self, self.vty(recv)) {
                        let r = super::coll::path_read(self, s);
                        self.own_emit(&segs[0], s, crate::kir::OwnKind::Consume);
                        r
                    } else {
                        recv
                    };
                    return Some(self.res_map(e, recv, "ok", &[]));
                }
            }
        }
        if segs.len() == 2 && segs[1] == "take" {
            if let Some(s) = self.lookup(&segs[0]) {
                let recv = super::coll::path_peek(self, s);
                let args = items(&f[1]).to_vec();
                if matches!(self.vty(recv), Ty::Opt(_)) {
                    return Some(self.builtin_call_take(e, s, recv, &args));
                }
            }
        }
        if segs.len() == 2 && segs[1] == "map_err" {
            if let Some(s) = self.lookup(&segs[0]) {
                let recv = super::coll::path_read(self, s);
                self.own_emit(&segs[0], s, crate::kir::OwnKind::Consume);
                let args = items(&f[1]).to_vec();
                return Some(self.map_err(e, recv, &args));
            }
        }
        if segs.len() == 2 && (checked_op(&segs[1]).is_some() || is_checked_extra(&segs[1])) {
            if let Some(s) = self.lookup(&segs[0]) {
                // 参照を取られた局所は `load` を通して値を読む (`read_slot`)。
                let recv = self.read_slot(s);
                let args = items(&f[1]).to_vec();
                return Some(self.checked_with(e, recv, &segs[1], &args));
            }
        }
        None
    }

    /// `e.checked_*(b)` (受け手が式)、`e.is_some()` などの Option / Result のメソッド。他のメソッドは `None` で、親の処理に任せる。
    pub(super) fn builtin_mcall(&mut self, e: &V) -> Option<LR<Option<VId>>> {
        let m = st(&nf(e)[1]);
        if m.starts_with("checked_") || m.starts_with("trunc_to_") {
            return Some(self.checked_mcall(e));
        }
        if matches!(m.as_str(), "map" | "and_then" | "ok") {
            let f = nf(e);
            let args = items(&f[2]).to_vec();
            if (m == "ok") == args.is_empty() && self.ty_of(&f[0]).map_or(false, |t| matches!(t, Ty::Res(..))) {
                return Some(match self.expr(&f[0]) {
                    Ok(Some(recv)) => self.res_map(e, recv, &m, &args),
                    Ok(None) => Ok(None),
                    Err(x) => Err(x),
                });
            }
        }
        if m == "map_err" {
            let f = nf(e);
            let args = items(&f[2]).to_vec();
            return Some(match self.expr(&f[0]) {
                Ok(Some(recv)) => self.map_err(e, recv, &args),
                Ok(None) => Ok(None),
                Err(x) => Err(x),
            });
        }
        if matches!(m.as_str(), "is_some" | "is_none" | "is_ok" | "is_err" | "unwrap_or") {
            return Some(self.optres_mcall(e, &m));
        }
        None
    }

    /// `opt.take()` (`Call[Path[opt, take]]` 形。`self: &mut Option<T>`): タグを見て `Some` ならペイロードを返して
    /// `opt` を `None` に書き戻し、`None` なら `None` を返して `opt` は `None` のままにする。
    /// `T` はスカラーを想定し、ペイロードをコピーして取り出す。
    fn builtin_call_take(&mut self, e: &V, s: usize, recv: VId, args: &[V]) -> LR<Option<VId>> {
        let line = nline(e);
        if !args.is_empty() {
            return self.err(line, "take は引数を取らない");
        }
        let rt = self.vty(recv);
        let Ty::Opt(pay) = rt else {
            return self.err(line, format!("メソッド take は型 {} に対して段階 1 の対象外", rt.name()));
        };
        let t = self.ty_of(e)?;
        if t != rt {
            return self.err(line, format!("take の結果の型が {} と違う", rt.name()));
        }
        // `&mut self` の受け手: 局所変数のスロットを `None` で上書きする。
        let none = self.emit(rt, Op::Const(0));
        let tag = self.emit(Ty::Bool, Op::Tag(recv));
        let (yes, _) = self.new_block(&[]);
        let (no, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[rt]);
        self.branch(tag, (yes, vec![]), (no, vec![]));
        // Some: ペイロードを取り出して `Some(payload)` を返し、`opt` を `None` にする。
        self.cur = Some(yes);
        let pv = self.emit(pay.ty(), Op::Payload(recv));
        let some = self.emit(rt, Op::Some(pv));
        self.assign_slot(s, none);
        self.goto(join, vec![some]);
        // None: `None` を返し、`opt` は `None` のまま。
        self.cur = Some(no);
        self.assign_slot(s, none);
        self.goto(join, vec![none]);
        self.cur = Some(join);
        Ok(Some(jp[0]))
    }

    /// `Option` / `Result` のメソッド: `is_some` `is_none` `is_ok` `is_err` (タグを読むだけ) と `unwrap_or(d)` (タグで枝を分ける)。
    /// `unwrap` は kir_lower_panic.rs (panic の命令で下ろす)。ここには来ない。
    fn optres_mcall(&mut self, e: &V, m: &str) -> LR<Option<VId>> {
        let f = nf(e);
        let args = items(&f[2]).to_vec();
        let Some(recv) = self.expr_peek(&f[0])? else { return Ok(None) };
        let local = super::coll::val_slot(self, &f[0]).is_some();
        self.optres_on(e, recv, local, m, &args)
    }

    /// 値 `recv` (局所変数なら `local`) に対する `Option` / `Result` のメソッド。
    pub(super) fn optres_on(&mut self, e: &V, recv: VId, local: bool, m: &str, args: &[V]) -> LR<Option<VId>> {
        let line = nline(e);
        let rt = self.vty(recv);
        let want_args = if m == "unwrap_or" { 1 } else { 0 };
        if args.len() != want_args {
            return self.err(line, format!("メソッド {} の引数は {} 個のはず", m, want_args));
        }
        // タグの値 (Some / Err か)。Option は Some、Result は Err が 1 (`Op::Tag`)。
        let tag = match rt {
            Ty::Opt(_) | Ty::Res(..) => self.emit(Ty::Bool, Op::Tag(recv)),
            _ => return self.err(line, format!("メソッド {} は型 {} に対して段階 1 の対象外", m, rt.name())),
        };
        let is_opt = matches!(rt, Ty::Opt(_));
        if super::coll::owns(self, rt) {
            if m == "unwrap_or" {
                return self.err(line, "所有物を持つ Option / Result の unwrap_or は段階 1 の対象外 (捨てる側の解放が要る)");
            }
            // 一時の値は、タグを読んだ後で解放する (局所変数は移さず、スコープの終わりに解放する)。
            if !local {
                super::coll::drop_value(self, recv, rt);
            }
        }
        let pay_ty = match (rt, m) {
            (Ty::Opt(_), "is_some") => return Ok(Some(tag)),
            (Ty::Opt(_), "is_none") => return Ok(Some(self.emit(Ty::Bool, Op::Not(tag)))),
            (Ty::Res(..), "is_err") => return Ok(Some(tag)),
            (Ty::Res(..), "is_ok") => return Ok(Some(self.emit(Ty::Bool, Op::Not(tag)))),
            (Ty::Opt(p), "unwrap_or") => p.ty(),
            (Ty::Res(t0, _), "unwrap_or") => t0.ty(),
            _ => return self.err(line, format!("メソッド {} は型 {} に対して段階 1 の対象外", m, rt.name())),
        };
        // unwrap_or: 値を持つ枝 (Some、または Ok) はペイロードを読み、それ以外は既定値 d を取る。
        let Some(mut d) = self.expr(&args[0])? else { return Ok(None) };
        // `Option<&スカラー>` は値のペイロードで持つので、既定値の `&x` は読んで値にする。
        if let Ty::Ref(_, p) = self.vty(d) {
            if p.ty() == pay_ty && is_opt {
                d = self.emit(pay_ty, Op::Load(d));
            }
        }
        if self.vty(d) != pay_ty {
            return self.err(line, format!("unwrap_or の既定値の型が {} と違う", pay_ty.name()));
        }
        let good = if is_opt { tag } else { self.emit(Ty::Bool, Op::Not(tag)) };
        let (yes, _) = self.new_block(&[]);
        let (no, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[pay_ty]);
        self.branch(good, (yes, vec![]), (no, vec![]));
        self.cur = Some(yes);
        let pv = self.emit(pay_ty, Op::Payload(recv));
        self.goto(join, vec![pv]);
        self.cur = Some(no);
        self.goto(join, vec![d]);
        self.cur = Some(join);
        Ok(Some(jp[0]))
    }

    /// `o.map(f)` / `o.and_then(f)` (`o` は所有物を持たない `Option`、`f` は捕獲のないクロージャか関数の項目)。
    /// `Some(x)` は `Some(f(x))` (`and_then` は `f(x)` そのもの)、`None` は `None`。
    pub(super) fn opt_map(&mut self, e: &V, recv: VId, and_then: bool, args: &[V]) -> LR<Option<VId>> {
        let line = nline(e);
        let name = if and_then { "and_then" } else { "map" };
        if args.len() != 1 {
            return self.err(line, format!("{} の引数は 1 つのはず", name));
        }
        let rt = self.ty_of(e)?;
        let (Ty::Opt(p0), Ty::Opt(p1)) = (self.vty(recv), rt) else {
            return self.err(line, format!("{} の受け手と結果が Option でない", name));
        };
        if super::coll::owns(self, self.vty(recv)) || super::coll::owns(self, rt) {
            return self.err(line, format!("所有物を持つ Option の {} は段階 1 の対象外", name));
        }
        let (key, fargs, fret) = super::coll::callable(self, &args[0])?;
        let want_ret = if and_then { rt } else { p1.ty() };
        if fargs != [p0.ty()] || fret != want_ret {
            return self.err(line, format!("{} の関数の型が Option の型と合わない", name));
        }
        let tag = self.emit(Ty::Bool, Op::Tag(recv));
        let (some_b, _) = self.new_block(&[]);
        let (none_b, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[rt]);
        self.branch(tag, (some_b, vec![]), (none_b, vec![]));
        self.cur = Some(some_b);
        let pv = self.emit(p0.ty(), Op::Payload(recv));
        let r = self.emit(fret, Op::Call { callee: key, args: vec![pv] });
        let out = if and_then { r } else { self.emit(rt, Op::Some(r)) };
        self.goto(join, vec![out]);
        self.cur = Some(none_b);
        let none = self.emit(rt, Op::Const(0));
        self.goto(join, vec![none]);
        self.cur = Some(join);
        Ok(Some(jp[0]))
    }

    /// `r.map(f)` / `r.and_then(f)` / `r.ok()` (`f` は捕獲のないクロージャか関数の項目)。
    /// `Ok(t)` は `Ok(f(t))` / `f(t)` / `Some(t)`、`Err(x)` は `Err(x)` / `Err(x)` / `None`。
    pub(super) fn res_map(&mut self, e: &V, recv: VId, m: &str, args: &[V]) -> LR<Option<VId>> {
        let line = nline(e);
        let want_args = if m == "ok" { 0 } else { 1 };
        if args.len() != want_args {
            return self.err(line, format!("{} の引数の数が違う", m));
        }
        let rt = self.ty_of(e)?;
        let Ty::Res(t0, e0) = self.vty(recv) else {
            return self.err(line, format!("{} の受け手が Result でない", m));
        };
        let callee = if m == "ok" { None } else { Some(super::coll::callable(self, &args[0])?) };
        // 結果の型と関数の型の確認。
        match (m, rt) {
            ("ok", Ty::Opt(p)) if p == t0 => {}
            ("map", Ty::Res(t1, e1)) if e1 == e0 && callee.as_ref().map_or(false, |(_, a, r)| a[..] == [t0.ty()] && *r == t1.ty()) => {}
            ("and_then", Ty::Res(_, e1)) if e1 == e0 && callee.as_ref().map_or(false, |(_, a, r)| a[..] == [t0.ty()] && *r == rt) => {}
            _ => return self.err(line, format!("{} の型が Result の型と合わない", m)),
        }
        let tag = self.emit(Ty::Bool, Op::Tag(recv));
        let (errb, _) = self.new_block(&[]);
        let (okb, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[rt]);
        self.branch(tag, (errb, vec![]), (okb, vec![]));
        self.cur = Some(errb);
        let out = if m == "ok" {
            self.emit(rt, Op::Const(0))
        } else {
            let ep = self.emit(e0.ty(), Op::Payload(recv));
            self.emit(rt, Op::Err(ep))
        };
        self.goto(join, vec![out]);
        self.cur = Some(okb);
        let tv = self.emit(t0.ty(), Op::Payload(recv));
        let out = match m {
            "ok" => self.emit(rt, Op::Some(tv)),
            _ => {
                let (key, _, fret) = callee.unwrap();
                let r = self.emit(fret, Op::Call { callee: key, args: vec![tv] });
                if m == "map" { self.emit(rt, Op::Ok(r)) } else { r }
            }
        };
        self.goto(join, vec![out]);
        self.cur = Some(join);
        Ok(Some(jp[0]))
    }

    /// `r.map_err(f)` (`f` は捕獲のないクロージャか関数の項目): `Err(x)` は `Err(f(x))`、`Ok(t)` はそのまま。
    fn map_err(&mut self, e: &V, recv: VId, args: &[V]) -> LR<Option<VId>> {
        let line = nline(e);
        if args.len() != 1 {
            return self.err(line, "map_err の引数は 1 つのはず");
        }
        let rt = self.ty_of(e)?;
        let (Ty::Res(t0, e0), Ty::Res(t1, e1)) = (self.vty(recv), rt) else {
            return self.err(line, "map_err の受け手と結果が Result でない");
        };
        if t0 != t1 {
            return self.err(line, "map_err で Ok の型が変わっている");
        }
        let (key, fargs, fret) = super::coll::callable(self, &args[0])?;
        if fargs != [e0.ty()] || fret != e1.ty() {
            return self.err(line, "map_err の関数の型が Result の E と合わない");
        }
        let tag = self.emit(Ty::Bool, Op::Tag(recv));
        let (errb, _) = self.new_block(&[]);
        let (okb, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[rt]);
        self.branch(tag, (errb, vec![]), (okb, vec![]));
        self.cur = Some(errb);
        let ep = self.emit(e0.ty(), Op::Payload(recv));
        let r = self.emit(fret, Op::Call { callee: key, args: vec![ep] });
        let er = self.emit(rt, Op::Err(r));
        self.goto(join, vec![er]);
        self.cur = Some(okb);
        let tv = self.emit(t0.ty(), Op::Payload(recv));
        let ok = self.emit(rt, Op::Ok(tv));
        self.goto(join, vec![ok]);
        self.cur = Some(join);
        Ok(Some(jp[0]))
    }

    fn ctor(&mut self, e: &V, name: &str) -> LR<Option<VId>> {
        let line = nline(e);
        let args = items(&nf(e)[1]);
        if args.len() != 1 {
            return self.err(line, format!("{} の引数は 1 つのはず", name));
        }
        let t = self.ty_of(e)?;
        let Some(a) = self.expr(&args[0])? else { return Ok(None) };
        // `Some(&x)` / `Ok(&x)` で `Option<&T>` / `Result<&T, E>` (T は所有物を持つ型) を作る: 借用の番地を `bref` に包む。
        let bref_pay: Option<Pay> = match (name, t) {
            ("Some", Ty::Opt(p)) | ("Ok", Ty::Res(p, _)) => Some(p),
            _ => None,
        };
        let a = match bref_pay {
            Some(Pay::Agg(bi)) if self.agg_def(bi).name == "bref" && matches!(self.vty(a), Ty::Ptr | Ty::Ref(..)) => {
                let p = if self.vty(a) == Ty::Ptr { a } else { self.emit(Ty::Ptr, Op::Bitcast { src: self.vty(a), a }) };
                self.emit(Ty::Agg(bi), Op::Make { variant: None, fields: vec![p] })
            }
            _ => a,
        };
        let mut a = a;
        // `Some(&[..])` の引数は `ref<fat>` (一時の枠の番地) で来る。Option のペイロードは fat の値なので読み出す。
        if let (Ty::Opt(p), Ty::Ref(_, Pointee::Agg(i))) = (t, self.vty(a)) {
            if p.ty() == Ty::Agg(i) && name == "Some" {
                a = self.emit(Ty::Agg(i), Op::Load(a));
            }
        }
        // `Some(&x)` で `Option<&スカラー>` を作る: スカラーへの参照を持つ Option は値で持つ (設計_KIR.md 10.10) ので、参照の指す先を読む。
        if let (Ty::Opt(p), Ty::Ref(_, pt)) = (t, self.vty(a)) {
            if name == "Some" && pt.ty() == p.ty() && p.ty().is_scalar() {
                let r = a;
                a = self.emit(p.ty(), Op::Load(r));
                // 借用の運び (注釈。設計_KIR.md 24.11): 読んだ値は参照の借用を運ぶ (検査用。実行には影響しない)。
                self.emit(Ty::Unit, Op::Carry { a: r, b: a });
            }
        }
        let (op, want) = match (name, t) {
            ("Some", Ty::Opt(p)) => (Op::Some(a), p.ty()),
            ("Ok", Ty::Res(t0, _)) => (Op::Ok(a), t0.ty()),
            ("Err", Ty::Res(_, e0)) => (Op::Err(a), e0.ty()),
            _ => return self.err(line, format!("{} を型 {} で作る形は段階 1 の対象外", name, t.name())),
        };
        if self.vty(a) != want {
            return self.err(line, format!("{} の引数の型が {} と違う", name, want.name()));
        }
        Ok(Some(self.emit(t, op)))
    }

    fn checked_mcall(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let Some(recv) = self.expr(&f[0])? else { return Ok(None) };
        let args = items(&f[2]).to_vec();
        let method = st(&f[1]);
        self.checked_with(e, recv, &method, &args)
    }

    /// 整数の受け手 `recv` に対する `checked_*(args[0])`。結果は `Option<受け手の型>`。
    pub(super) fn checked_with(&mut self, e: &V, recv: VId, method: &str, args: &[V]) -> LR<Option<VId>> {
        let line = nline(e);
        if is_checked_extra(method) {
            return self.checked_extra(e, recv, method, args);
        }
        let Some(op) = checked_op(method) else {
            return self.err(line, format!("メソッド {} は段階 1 の対象外", method));
        };
        let rt = self.vty(recv);
        if !rt.is_int() {
            return self.err(line, format!("{} の受け手が整数ではない", method));
        }
        if args.len() != 1 {
            return self.err(line, format!("{} の引数は 1 つのはず", method));
        }
        let Some(b) = self.expr(&args[0])? else { return Ok(None) };
        if self.vty(b) != rt {
            return self.err(line, format!("{} の引数の型が受け手と違う", method));
        }
        let t = self.ty_of(e)?;
        let want = Ty::Opt(Pay::of(rt).expect("整数はスカラー"));
        if t != want {
            return self.err(line, format!("{} の結果の型 {} が {} と違う", method, t.name(), want.name()));
        }
        Ok(Some(self.emit(t, Op::Checked { op, a: recv, b })))
    }

    /// `match e { arms }`。腕ごとに「パターンの判定 → 腕の本体 → 合流」を作る。
    /// 最後の腕は判定を省く (下ろしの前に型検査の網羅性が効いている前提。網羅でない match は検査で止まる)。
    /// `match o { .. }` で `o` が `&Option<X>` / `&mut Option<X>` (値は `ref<Option>`) のとき。参照の先を 1 度読んで、
    /// `Some` ならペイロードの番地 (`bref`) を運ぶ `Option<&X>` を作り、それを対象にして `match_` を回す。
    /// 対象が参照の値でないなら `None` (呼び出し側が従来の経路に進む)。
    pub(super) fn match_ref_option(&mut self, e: &V, scrut: &V, m: bool, x: &Src) -> LR<Option<Option<VId>>> {
        let line = nline(e);
        // 参照の値を評価する。評価で副作用が起きるので、参照でないと分かったら従来の経路へ戻れない: 先に型だけ確かめる。
        if !matches!(self.ty_of(scrut), Ok(Ty::Ref(_, Pointee::Opt(_)))) {
            return Ok(None);
        }
        let Some(r) = self.expr(scrut)? else { return Ok(Some(None)) };
        let Ty::Ref(_, Pointee::Opt(pay)) = self.vty(r) else { return Ok(None) };
        let ot = Ty::Opt(pay);
        let ov = self.emit(ot, Op::Load(r));
        let bt = self.aggs.borrow_mut().bref();
        let Some(bp) = Pay::of(bt) else { return self.err(line, "bref を Option のペイロードにできない") };
        let opt = Ty::Opt(bp);
        let tag = self.emit(Ty::Bool, Op::Tag(ov));
        let (yb, _) = self.new_block(&[]);
        let (nb, _) = self.new_block(&[]);
        let (jb, jp) = self.new_block(&[opt]);
        self.branch(tag, (yb, vec![]), (nb, vec![]));
        self.cur = Some(yb);
        let addr = self.emit(Ty::Ptr, Op::Bitcast { src: self.vty(r), a: r });
        let b = self.emit(bt, Op::Make { variant: None, fields: vec![addr] });
        let some = self.emit(opt, Op::Some(b));
        self.goto(jb, vec![some]);
        self.cur = Some(nb);
        let none = self.emit(opt, Op::Const(0));
        self.goto(jb, vec![none]);
        self.cur = Some(jb);
        self.coll.scrut_override = Some((jp[0], Src::Named("Option".into(), vec![Src::Ref(m, Box::new(x.clone()))])));
        Ok(Some(self.match_(e)?))
    }

    pub(super) fn match_(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let line = nline(e);
        let res: Option<Ty> = match self.src_ty(e)? {
            Src::Never => None,
            t => Some(self.conv_t(&t).ok_or_else(|| format!("行 {}: match の型 {} は段階 1 の対象外", line, show(&t)))?),
        };
        // 所有の印を持つ局所の対象は、読む (`use`) だけ。移すのは、非 Copy の束縛をする腕の入口 (下の `consume`)。
        let scrut_local = super::path1(&f[0]).and_then(|n| self.lookup(&n).map(|s| (n, s)));
        let scrut_linear = self.src_ty(&f[0]).is_ok_and(|t| self.src_is_linear(&t, &mut std::collections::HashSet::new()));
        let ov = self.coll.scrut_override.take();
        let peeked = match &scrut_local {
            _ if ov.is_some() => ov.as_ref().map(|(v, _)| *v),
            Some((n, s)) if self.coll.own_ids.contains_key(&(n.clone(), *s)) => {
                self.coll.peek_field = false;
                self.own_emit(n, *s, crate::kir::OwnKind::Use);
                Some(super::coll::path_peek(self, *s))
            }
            _ => self.expr_peek(&f[0])?,
        };
        let Some(s) = peeked else { return Ok(None) };
        let sty = self.vty(s);
        let arms = items(&f[1]);
        let pats: Vec<&V> = arms.iter().map(|a| &items(a)[0]).collect();
        let sc = super::coll::scrut_begin(self, &f[0], s, &pats)?;
        let saved_src = self.coll.scrut_src.take();
        self.coll.scrut_src = match &ov {
            Some((_, src)) => Some((sty, src.clone())),
            None => self.src_ty(&f[0]).ok().map(|t| (sty, t)),
        };
        let bodies: Vec<&V> = arms.iter().map(|a| &items(a)[1]).collect();
        let slots = self.assigned(&bodies);
        let slot_tys: Vec<Ty> = slots.iter().map(|x| self.slots[*x].1).collect();
        let carry = matches!(res, Some(t) if t != Ty::Unit);
        let mut params: Vec<Ty> = if carry { res.into_iter().collect() } else { vec![] };
        params.extend(slot_tys.iter().copied());
        let (join, jp) = self.new_block(&params);
        let saved = self.slots.clone();
        let mut test = self.cur.expect("KIR 下ろしの内部不変条件: match は到達するブロックにある");
        let n = arms.len();
        for (i, arm) in arms.iter().enumerate() {
            let pat = &items(arm)[0];
            let body = &items(arm)[1];
            self.cur = Some(test);
            self.slots = saved.clone();
            self.scopes.push(vec![]);
            // 腕の束縛の型を、対象の型 (`scrut_src`) をパターンに沿って分解して決める (判定の間だけ持つ)。
            let mut binds = vec![];
            if let Some((_, ss)) = self.coll.scrut_src.clone() {
                pat_src_binds(pat, &ss, &mut binds);
                // 利用者の列挙型の変種 (`Ok(E.V(text))`) や構造体の中の束縛も、型から種類を結び付ける
                let mut deep = std::collections::BTreeMap::new();
                super::coll::pat_srcs(self, pat, &ss, &mut deep);
                for (k, v) in deep {
                    if !binds.iter().any(|b| b.0 == k) {
                        binds.push((k, v));
                    }
                }
            }
            self.coll.pat_srcs = binds.into_iter().collect();
            self.coll.pending_bref.clear();
            let c = self.pat_test(pat, s, sty)?;
            let arm_binds: Vec<(String, Src)> = self.coll.pat_srcs.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            self.coll.pat_srcs.clear();
            let (bb, _) = self.new_block(&[]);
            if i + 1 == n {
                self.goto(bb, vec![]);
            } else {
                let (nb, _) = self.new_block(&[]);
                self.branch(c, (bb, vec![]), (nb, vec![]));
                test = nb;
            }
            self.cur = Some(bb);
            super::coll::scrut_arm(self, &sc, i, pat, s)?;
            // 腕の束縛の所有 (線形な値の義務・move の注釈)。腕の本体に入ったときだけ義務が生じる (判定の経路には置かない)。
            for (name, src) in &arm_binds {
                if let Some(bs) = self.lookup(name) {
                    self.own_declare(name, bs, src);
                }
            }
            // 非 Copy の値を束縛する腕は、対象の局所を移す (以後の読みは move 済み)。
            // 線形な対象は、`_` 以外の腕では義務を果たす (空の変種の腕も含む)。
            if let Some((n, s)) = &scrut_local {
                // `if let` の省略された else (構文が作る `PWild`) は消費しない: 対象の線形値の義務が残る (AST と同じ)。
                if arm_binds.iter().any(|(_, src)| !self.src_is_copy(src)) || (scrut_linear && nname(pat) != "PWild") {
                    self.own_emit(n, *s, crate::kir::OwnKind::Consume);
                }
            }
            self.bref_bind(line)?;
            if let Some(v) = self.expr(body)? {
                if let Some(t) = res {
                    if self.vty(v) != t {
                        return self.err(line, "match の腕の型が結果の型と違う");
                    }
                }
                super::coll::block_end(self);
                let mut args: Vec<VId> = if carry { vec![v] } else { vec![] };
                args.extend(self.vals_of(&slots));
                self.goto(join, args);
            } else {
                super::coll::block_end(self);
            }
            self.scopes.pop();
        }
        self.coll.scrut_src = saved_src;
        if self.blocks[join].preds == 0 {
            self.slots = saved;
            return Ok(None);
        }
        self.cur = Some(join);
        let off = carry as usize;
        for (i, x) in slots.iter().enumerate() {
            self.rebind_slot(*x, jp[off + i], slot_tys[i]);
        }
        Ok(Some(if carry { jp[0] } else { self.unit() }))
    }

    /// パターン `p` の判定。値 `v` (型 `t`) について bool の値を返し、束縛を現在のスコープに入れる。
    /// 判定と束縛の値は今のブロックで先に計算する (タグを見ずにペイロードを読んでも害はない)。
    pub(super) fn pat_test(&mut self, p: &V, v: VId, t: Ty) -> LR<VId> {
        let line = nline(p);
        // 列挙型への参照 (`match e { E.A(x) => .. }`、`e: &E`): 判定は値で行い、名前は既定の参照の束縛としてペイロードの番地を指す。
        if let Ty::Ref(m, Pointee::Agg(ai)) = t {
            let is_enum = matches!(self.agg_def(ai).kind, AggKind::Enum(_));
            if is_enum && matches!(nname(p), "PVariant" | "PPath") {
                let cur = self.emit(Ty::Agg(ai), Op::Load(v));
                let c = self.agg_pat_test(p, cur, Ty::Agg(ai))?;
                if nname(p) == "PVariant" {
                    let vn = strs(&nf(p)[0]).last().cloned().unwrap_or_default();
                    let fields = match &self.agg_def(ai).kind {
                        AggKind::Enum(vs) => vs.iter().find(|x| x.name == vn).map(|x| x.fields.clone()).unwrap_or_default(),
                        _ => vec![],
                    };
                    for (sp, fd) in items(&nf(p)[1]).iter().zip(&fields) {
                        if nname(sp) != "PBind" {
                            continue;
                        }
                        let name = st(&nf(sp)[0]);
                        let Some(fp) = Pointee::of(fd.ty) else {
                            return self.err(line, "参照越しの列挙子のペイロードの型は段階 1 の対象外");
                        };
                        let base = self.emit(Ty::Ptr, Op::Bitcast { src: Ty::Ref(m, Pointee::Agg(ai)), a: v });
                        let k = self.emit(Ty::Usize, Op::Const(fd.off as i128));
                        let pa = self.emit(Ty::Ptr, Op::PtrAdd { p: base, off: k });
                        let addr = self.emit(Ty::Ref(m, fp), Op::Bitcast { src: Ty::Ptr, a: pa });
                        self.bind(&name, addr, Ty::Ref(m, fp));
                    }
                }
                return Ok(c);
            }
        }
        // 集成体の型のパターン (列挙子・タプル・構造体) は集成体の判定に任せる。束縛だけは下の `PBind` で作る。
        if matches!(t, Ty::Agg(_)) && !matches!(nname(p), "PWild" | "PBind") {
            return self.agg_pat_test(p, v, t);
        }
        match nname(p) {
            "PWild" => Ok(self.emit(Ty::Bool, Op::Const(1))),
            "PBind" => {
                let name = st(&nf(p)[0]);
                // 所有する Vec / String の束縛 (`Ok(E.V(text))`) は、型から種類 (String など) を結び付ける。
                let hinted = self.coll.pat_srcs.contains_key(&name);
                if hinted {
                    self.coll.hint = self.coll.pat_srcs.get(&name).cloned();
                }
                super::coll::bind_owned(self, &name, v, t);
                if hinted {
                    self.coll.hint = None;
                }
                // 借用 (`&str` など) の束縛は、腕の型 (`scrut_src` をパターンに沿って分解した型) から種類を結び付ける。
                if let Some(src) = self.coll.pat_srcs.get(&name).cloned() {
                    super::coll::bind_param_kind(self, &name, &src);
                }
                Ok(self.emit(Ty::Bool, Op::Const(1)))
            }
            "PLit" => {
                let lf = nf(&nf(p)[0]).to_vec();
                match st(&lf[0]).as_str() {
                    "unit" if t == Ty::Unit => Ok(self.emit(Ty::Bool, Op::Const(1))),
                    "int" | "byte" if t.is_int() => {
                        let x = parse_int(&lf[1]).ok_or_else(|| format!("行 {}: 整数パターンが扱えない", line))?;
                        if x < t.min() || x > t.max() {
                            return self.err(line, format!("整数パターンが {} の範囲にない", t.name()));
                        }
                        let k = self.emit(t, Op::Const(x));
                        Ok(self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: t, a: v, b: k }))
                    }
                    "char" if t == Ty::U32 => {
                        let x = parse_int(&lf[1]).ok_or_else(|| format!("行 {}: 文字パターンが扱えない", line))?;
                        let k = self.emit(Ty::U32, Op::Const(x));
                        Ok(self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::U32, a: v, b: k }))
                    }
                    "bool" if t == Ty::Bool => {
                        let k = self.emit(Ty::Bool, Op::Const(matches!(lf[1], V::B(true)) as i128));
                        Ok(self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Bool, a: v, b: k }))
                    }
                    "str" if t == self.fat_ty() => {
                        let k = super::coll::str_lit(self, &nf(p)[0])?;
                        self.str_eq(v, k)
                    }
                    other => self.err(line, format!("リテラル {} のパターンは型 {} に対して段階 1 の対象外", other, t.name())),
                }
            }
            "PPath" => {
                let segs = strs(&nf(p)[0]);
                match (segs.as_slice(), t) {
                    ([n], Ty::Opt(_)) if n == "None" => {
                        let tag = self.emit(Ty::Bool, Op::Tag(v));
                        Ok(self.emit(Ty::Bool, Op::Not(tag)))
                    }
                    _ => self.err(line, format!("パターン {} は型 {} に対して段階 1 の対象外", segs.join("."), t.name())),
                }
            }
            "PVariant" => {
                let segs = strs(&nf(p)[0]);
                let subs = items(&nf(p)[1]).to_vec();
                if segs.len() != 1 || subs.len() != 1 {
                    return self.err(line, "バリアントのパターンは要素を 1 つ取る形だけ段階 1 の対象");
                }
                let sub = &subs[0];
                // (タグが期待どおりか, ペイロードの型)
                let (want_tag, pay_ty) = match (segs[0].as_str(), t) {
                    ("Some", Ty::Opt(p0)) => (true, p0.ty()),
                    ("Ok", Ty::Res(t0, _)) => (false, t0.ty()),
                    ("Err", Ty::Res(_, e0)) => (true, e0.ty()),
                    (n, _) => return self.err(line, format!("パターン {} は型 {} に対して段階 1 の対象外", n, t.name())),
                };
                let tag = self.emit(Ty::Bool, Op::Tag(v));
                let tag_ok = if want_tag { tag } else { self.emit(Ty::Bool, Op::Not(tag)) };
                let pv = self.emit(pay_ty, Op::Payload(v));
                // `Option<&T>` (T は所有物を持つ型): ペイロードは要素の番地。腕では借用として束縛する (17.18)。
                if let (Ty::Agg(bi), "Some" | "Ok") = (pay_ty, segs[0].as_str()) {
                    if self.agg_def(bi).name == "bref" {
                        let c = self.bref_pat(sub, pv, t, tag_ok, line)?;
                        return Ok(self.emit(Ty::Bool, Op::Bool(BoolOp::And, tag_ok, c)));
                    }
                }
                if let Some((st_ty, st_src)) = self.coll.scrut_src.clone() {
                    if st_ty == t {
                        super::coll::set_payload_hint(self, &st_src, &segs[0]);
                    }
                }
                let c = self.pat_test(sub, pv, pay_ty)?;
                self.coll.hint = None;
                Ok(self.emit(Ty::Bool, Op::Bool(BoolOp::And, tag_ok, c)))
            }
            other => self.err(line, format!("パターン {} は段階 1 の対象外", other)),
        }
    }

    /// `Some(sub)` の `sub` を、`Option<&T>` のペイロード `pv` (`bref`) に当てる。名前だけ・`_` は判定なし。
    /// 構造体・列挙子の形は要素を読んで判定する。`None` (番地が無い) では読まないよう、`tag_ok` の枝の中で読む。
    /// 束縛は腕の頭 (`bref_bind`) で行う。
    fn bref_pat(&mut self, sub: &V, pv: VId, t: Ty, tag_ok: VId, line: usize) -> LR<VId> {
        if nname(sub) == "PWild" {
            return Ok(self.emit(Ty::Bool, Op::Const(1)));
        }
        if self.coll.scrut_src.as_ref().is_none_or(|(st_ty, _)| *st_ty != t) {
            return self.err(line, "Option<&T> の値の型が分からない (match の対象が必要)");
        }
        // 束縛は腕の頭 (`bref_bind`) で行うので、ここで保留する。
        self.coll.pending_bref.push((sub.clone(), pv, t));
        if nname(sub) == "PBind" {
            return Ok(self.emit(Ty::Bool, Op::Const(1)));
        }
        // 構造体・列挙子の形の判定: 要素を読むのは `tag_ok` の枝の中だけ。名前は `_` に置き換えた形を当てる。
        let test_sub = strip_binds(sub);
        let (_, elem) = self.bref_elem(t, line)?;
        let (yes, _) = self.new_block(&[]);
        let (join, jp) = self.new_block(&[Ty::Bool]);
        let zero = self.emit(Ty::Bool, Op::Const(0));
        self.branch(tag_ok, (yes, vec![]), (join, vec![zero]));
        self.cur = Some(yes);
        let ev = self.bref_load(pv, elem, line)?;
        let c = self.pat_test(&test_sub, ev, elem)?;
        self.goto(join, vec![c]);
        self.cur = Some(join);
        Ok(jp[0])
    }

    /// `Option<&T>` / `Result<&T, E>` の `T` (元の型とその下ろした型)。
    fn bref_elem(&mut self, t: Ty, line: usize) -> LR<(Src, Ty)> {
        let elem_src = match self.coll.scrut_src.clone() {
            Some((st_ty, Src::Named(n, a))) if st_ty == t && (n == "Option" && a.len() == 1 || n == "Result" && a.len() == 2) => match &a[0] {
                Src::Ref(_, i) => (**i).clone(),
                _ => return self.err(line, "Option<&T> の T が分からない"),
            },
            _ => return self.err(line, "Option<&T> の値の型が分からない (match の対象が必要)"),
        };
        let elem = self.conv_t(&elem_src).ok_or_else(|| format!("行 {}: 型 {} は段階 1 の対象外", line, show(&elem_src)))?;
        // 集成体と、スカラー (`Option<&mut u32>` の `get_mut` など) を番地で持つ。
        if !matches!(elem, Ty::Agg(_)) && !elem.is_scalar() {
            return self.err(line, format!("型 {} は借用の判定の対象として段階 1 の対象外", show(&elem_src)));
        }
        Ok((elem_src, elem))
    }

    /// 借用の番地 `pv` (`bref`) の指す要素を、値として読む (判定・束縛の補助。読むのは `Some` の枝の中だけ)。
    fn bref_load(&mut self, pv: VId, elem: Ty, _line: usize) -> LR<VId> {
        let addr = self.emit(Ty::Ptr, Op::Field { a: pv, off: 0 });
        let Ty::Agg(ai) = elem else {
            let p = crate::kir::Pointee::of(elem).expect("bref_elem がスカラーを確かめている");
            let r = self.emit(Ty::Ref(false, p), Op::Bitcast { src: Ty::Ptr, a: addr });
            return Ok(self.emit(elem, Op::Load(r)));
        };
        let r = self.emit(Ty::Ref(false, crate::kir::Pointee::Agg(ai)), Op::Bitcast { src: Ty::Ptr, a: addr });
        Ok(self.emit(elem, Op::Load(r)))
    }

    /// `bref_pat` が保留した束縛を、当たった腕の頭で作る。
    fn bref_bind(&mut self, line: usize) -> LR<()> {
        // 番地の型 (`bref`) は可変性を持たないので、対象の型 (`Option<&mut T>` / `Result<&mut T, _>`) から決める。
        let mutable = match self.coll.scrut_src.as_ref().map(|(_, s)| s) {
            Some(Src::Named(n, a)) if n == "Option" || n == "Result" => matches!(a.first(), Some(Src::Ref(true, _))),
            _ => false,
        };
        for (sub, pv, t) in std::mem::take(&mut self.coll.pending_bref) {
            let (elem_src, elem) = self.bref_elem(t, line)?;
            match nname(&sub) {
                "PBind" => {
                    let addr = self.emit(Ty::Ptr, Op::Field { a: pv, off: 0 });
                    let name = st(&nf(&sub)[0]);
                    if let Ty::Agg(_) = elem {
                        super::own::bind_elem_ref(self, line, &name, addr, elem, Some(&elem_src), mutable)?;
                    } else {
                        // スカラーの番地 (`&mut u32` など): 指す先への参照として束縛する。
                        let p = crate::kir::Pointee::of(elem).expect("bref_elem がスカラーを確かめている");
                        let rt = Ty::Ref(mutable, p);
                        let r = self.emit(rt, Op::Bitcast { src: Ty::Ptr, a: addr });
                        self.bind(&name, r, rt);
                    }
                }
                "PStruct" => {
                    let addr = self.emit(Ty::Ptr, Op::Field { a: pv, off: 0 });
                    let Ty::Agg(ai) = elem else { return self.err(line, "構造体のパターンの対象が集成体でない") };
                    let r = self.emit(Ty::Ref(false, crate::kir::Pointee::Agg(ai)), Op::Bitcast { src: Ty::Ptr, a: addr });
                    super::own::bind_struct_pat(self, line, &sub, r, Some(&elem_src))?;
                }
                "PVariant" | "PPath" => {
                    let addr = self.emit(Ty::Ptr, Op::Field { a: pv, off: 0 });
                    self.bind_variant_fields(line, &sub, addr, elem)?;
                }
                // 組 (`Some((k, v))` の `&(K, V)` など): 欄を番地から読んで束縛する (所有物は借用、スカラーと fat は値)。
                "PTuple" => {
                    let addr = self.emit(Ty::Ptr, Op::Field { a: pv, off: 0 });
                    let Ty::Agg(ai) = elem else { return self.err(line, "組のパターンの対象が集成体でない") };
                    let AggKind::Tuple(fs) = self.agg_def(ai).kind.clone() else {
                        return self.err(line, "組のパターンの対象が組でない");
                    };
                    let subs = items(&nf(&sub)[0]).to_vec();
                    if subs.len() != fs.len() {
                        return self.err(line, "組のパターンの要素数が型と合わない");
                    }
                    for (sp, fd) in subs.iter().zip(&fs) {
                        match nname(sp) {
                            "PWild" => continue,
                            "PBind" => {}
                            _ => return self.err(line, "借用の組のパターンは名前と _ だけ段階 1 の対象"),
                        }
                        let name = st(&nf(sp)[0]);
                        let fp = if fd.off == 0 {
                            addr
                        } else {
                            let k = self.emit(Ty::Usize, Op::Const(fd.off as i128));
                            self.emit(Ty::Ptr, Op::PtrAdd { p: addr, off: k })
                        };
                        if super::coll::owns(self, fd.ty) {
                            super::own::bind_elem_ref(self, line, &name, fp, fd.ty, None, mutable)?;
                        } else {
                            let Some(pt) = Pointee::of(fd.ty) else {
                                return self.err(line, "借用の組の欄の型は段階 1 の対象外");
                            };
                            let r = self.emit(Ty::Ref(false, pt), Op::Bitcast { src: Ty::Ptr, a: fp });
                            let v = self.emit(fd.ty, Op::Load(r));
                            self.bind(&name, v, fd.ty);
                        }
                    }
                }
                other => return self.err(line, format!("Option<&T> の Some の中のパターン {} は段階 1 の対象外 (名前・_・構造体・列挙子のパターンだけ)", other)),
            }
        }
        Ok(())
    }

    /// `e?`。`Option` は `None` をそのまま返し、`Result` は `Err(e)` を (変換せず) 返す。
    pub(super) fn try_(&mut self, e: &V) -> LR<Option<VId>> {
        let line = nline(e);
        let Some(v) = self.expr(&nf(e)[0])? else { return Ok(None) };
        let vt = self.vty(v);
        let rt = self.ret;
        match (vt, rt) {
            (Ty::Opt(p), Ty::Opt(_)) => {
                let tag = self.emit(Ty::Bool, Op::Tag(v));
                let (okb, _) = self.new_block(&[]);
                let (nob, _) = self.new_block(&[]);
                self.branch(tag, (okb, vec![]), (nob, vec![]));
                self.cur = Some(nob);
                let none = self.emit(rt, Op::Const(0));
                super::coll::drop_from(self, 0);
                self.terminate(Term::Ret(none));
                self.cur = Some(okb);
                let pv = self.emit(p.ty(), Op::Payload(v));
                Ok(Some(self.bref_out(pv, &nf(e)[0], line)?))
            }
            (Ty::Res(t0, e0), Ty::Res(_, e1)) if e0 == e1 => {
                let tag = self.emit(Ty::Bool, Op::Tag(v));
                let (okb, _) = self.new_block(&[]);
                let (errb, _) = self.new_block(&[]);
                self.branch(tag, (errb, vec![]), (okb, vec![]));
                self.cur = Some(errb);
                let ep = self.emit(e0.ty(), Op::Payload(v));
                let r = self.emit(rt, Op::Err(ep));
                super::coll::drop_from(self, 0);
                self.terminate(Term::Ret(r));
                self.cur = Some(okb);
                let pv = self.emit(t0.ty(), Op::Payload(v));
                Ok(Some(self.bref_out(pv, &nf(e)[0], line)?))
            }
            // `Result<T, E1>` の `?` を `Result<U, E2>` (E1 ≠ E2) で使う。E2 が `derive [From]`
            // でペイロードがちょうど E1 の変種 V をちょうど 1 つ持つとき、エラーを
            // `E2.V(err)` に包む (仕様 4.1)。`kir_lower_agg.rs` の `variant_make` と同じ
            // 列挙体の構築を使う。所有物を持つ E1 はペイロードとして移し出す。
            (Ty::Res(t0, e0), Ty::Res(_, e1)) => self.try_conv(e, v, vt, rt, t0, e0, e1),
            _ => self.err(line, format!("? の値の型 {} は戻り値の型 {} と対応しない (段階 1 の対象外)", vt.name(), rt.name())),
        }
    }

    /// `Result<T, E1>` の `?` を `Result<U, E2>` (E1 ≠ E2) で使うときのエラー変換。
    /// `E2.V(err)` を作って `Err` に包む。できなければ「対応しない」で止める。
    fn try_conv(&mut self, e: &V, v: VId, vt: Ty, rt: Ty, t0: Pay, e0: Pay, e1: Pay) -> LR<Option<VId>> {
        let line = nline(e);
        let unsupported = || {
            self.err(line, format!("? の値の型 {} は戻り値の型 {} と対応しない (段階 1 の対象外)", vt.name(), rt.name()))
        };
        // `?` の値の型 `Result<T, E1>` の E1 と、戻り値の型 `Result<U, E2>` の E2。
        let from = match self.src_ty(&nf(e)[0])? {
            Src::Named(_, args) if args.len() == 2 => args[1].show(),
            _ => return unsupported(),
        };
        let to = match from_v(&self.def.ret, &HashSet::new()) {
            Src::Named(_, args) if args.len() == 2 => args[1].show(),
            _ => return unsupported(),
        };
        let FromVariant::One(variant) = self.p.from_variant(&to, &from) else { return unsupported() };
        let Pay::Agg(ai2) = e1 else { return unsupported() };
        let d = self.agg_def(ai2);
        // 下ろしている戻り値の型 (クロージャならその戻り値) と、E2 の名前が一致するときだけ包む。
        // クロージャの中では `self.def` が外側の関数を指すので、名前で確かめる。
        if d.name != to {
            return unsupported();
        }
        let AggKind::Enum(vs) = &d.kind else { return unsupported() };
        let Some(k) = vs.iter().position(|x| x.name == variant) else { return unsupported() };
        let tag = self.emit(Ty::Bool, Op::Tag(v));
        let (okb, _) = self.new_block(&[]);
        let (errb, _) = self.new_block(&[]);
        self.branch(tag, (errb, vec![]), (okb, vec![]));
        self.cur = Some(errb);
        let ep = self.emit(e0.ty(), Op::Payload(v));      // E1 の値を `v` から移し出す
        let agg = self.emit(Ty::Agg(ai2), Op::Make { variant: Some(k as u32), fields: vec![ep] });  // E2.V(err)
        let r = self.emit(rt, Op::Err(agg));
        super::coll::drop_from(self, 0);
        self.terminate(Term::Ret(r));
        self.cur = Some(okb);
        let pv = self.emit(t0.ty(), Op::Payload(v));
        Ok(Some(self.bref_out(pv, &nf(e)[0], line)?))
    }

    /// 借用の番地 `addr` (要素の列挙体の番地) に、列挙子のパターン `V::Variant(sub..)` を当てて名前を束縛する。
    /// スカラーのフィールドは読んだ写しを、所有物 (`Vec` / `String` など) のフィールドは番地の借用を束縛する (`match &x` と同じ)。
    fn bind_variant_fields(&mut self, line: usize, sub: &V, addr: VId, elem: Ty) -> LR<()> {
        let Ty::Agg(ai) = elem else { return self.err(line, "列挙子のパターンの対象が集成体でない") };
        let segs = strs(&nf(sub)[0]);
        let subs = if nname(sub) == "PVariant" { items(&nf(sub)[1]).to_vec() } else { vec![] };
        let Some(Tgt::Variant(en, vn)) = self.p.resolve(&segs, &self.def.file).map(|r| r.target) else {
            return self.err(line, format!("パターン {} は段階 1 の対象外", segs.join(".")));
        };
        let d = self.agg_def(ai);
        let AggKind::Enum(vs) = &d.kind else { return self.err(line, format!("列挙子のパターンの対象 {} が列挙型でない", d.name)) };
        if d.name.split('<').next() != Some(en.as_str()) {
            return self.err(line, format!("パターン {} の型が対象の型 {} と違う", vn, d.name));
        }
        let Some(idx) = vs.iter().position(|x| x.name == vn) else { return self.err(line, format!("列挙子 {} がない", vn)) };
        let fields = vs[idx].fields.clone();
        if subs.len() != fields.len() {
            return self.err(line, "列挙子のパターンの要素数が違う");
        }
        // 束縛の型の付け方 (Vec / String の種類) に使う、宣言どおりの列挙子のフィールドの型。
        let fsrcs: Vec<Src> = self
            .p
            .defs
            .iter()
            .find(|x| x.kw == "enum" && x.name == en)
            .and_then(|x| x.variants.iter().find(|v| v.0 == vn))
            .map(|v| v.1.iter().map(|x| from_v(x, &HashSet::new())).collect())
            .unwrap_or_default();
        // 列挙体の番地に、フィールドの番地 (`addr + off`) を足す。列挙型には `fieldaddr` を使えない。
        for (i, (sp, fd)) in subs.iter().zip(&fields).enumerate() {
            match nname(sp) {
                "PWild" | "PLit" => {}
                "PBind" if !matches!(nf(sp).get(1), Some(V::B(true))) => {
                    let name = st(&nf(sp)[0]);
                    let fp = if fd.off == 0 {
                        addr
                    } else {
                        let k = self.emit(Ty::Usize, Op::Const(fd.off as i128));
                        self.emit(Ty::Ptr, Op::PtrAdd { p: addr, off: k })
                    };
                    if super::coll::owns(self, fd.ty) {
                        super::own::bind_elem_ref(self, line, &name, fp, fd.ty, fsrcs.get(i), false)?;
                    } else {
                        let Some(pt) = Pointee::of(fd.ty) else {
                            return self.err(line, format!("列挙子のフィールドの型 {} は借用の対象として段階 1 の対象外", crate::kir::fmt_ty(fd.ty)));
                        };
                        let r = self.emit(Ty::Ref(false, pt), Op::Bitcast { src: Ty::Ptr, a: fp });
                        let v = self.emit(fd.ty, Op::Load(r));
                        self.bind(&name, v, fd.ty);
                    }
                }
                other => return self.err(line, format!("借用の列挙子の中のパターン {} は段階 1 の対象外 (名前・_・リテラルだけ)", other)),
            }
        }
        Ok(())
    }

    /// `Option<&T>` / `Result<&T, E>` の値 (T は所有物を持つ型) から取り出したペイロード `pv` が `bref` なら、
    /// 借用の `&T` (`ref<T>`) に直す。`operand` はその値の式 (型は `src_ty` で `&T` の T を知る)。`bref` でなければ `pv` のまま。
    fn bref_out(&mut self, pv: VId, operand: &V, line: usize) -> LR<VId> {
        let Ty::Agg(bi) = self.vty(pv) else { return Ok(pv) };
        if self.agg_def(bi).name != "bref" {
            return Ok(pv);
        }
        let src = self.src_ty(operand)?;
        let inner = match &src {
            Src::Named(_, a) if !a.is_empty() => match &a[0] {
                Src::Ref(false, i) => (**i).clone(),
                _ => return self.err(line, "Option<&T> / Result<&T, E> の T が分からない"),
            },
            _ => return self.err(line, "Option<&T> / Result<&T, E> の値の型が分からない"),
        };
        let Some(Ty::Agg(ai)) = self.conv_t(&inner) else {
            return self.err(line, format!("型 {} は借用の対象として段階 1 の対象外", show(&inner)));
        };
        let addr = self.emit(Ty::Ptr, Op::Field { a: pv, off: 0 });
        Ok(self.emit(Ty::Ref(false, crate::kir::Pointee::Agg(ai)), Op::Bitcast { src: Ty::Ptr, a: addr }))
    }
}

/// 名前の束縛 (`PBind`) を `_` (`PWild`) に置き換えた判定用のパターン。判定の枝では束縛を作らないために使う。
fn strip_binds(v: &V) -> V {
    match v {
        V::Node("PBind", _, line, id) => V::Node("PWild", vec![], *line, *id),
        V::Node(n, fs, line, id) => V::Node(n, fs.iter().map(strip_binds).collect(), *line, *id),
        V::L(xs) => V::L(xs.iter().map(strip_binds).collect()),
        other => other.clone(),
    }
}
