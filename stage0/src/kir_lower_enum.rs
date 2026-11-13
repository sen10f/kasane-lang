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
use crate::kir::{AggKind, Aop, BoolOp, Cc, Op, Pay, Term, Ty, VId};
use crate::parser::V;
use crate::program::FromVariant;
use crate::types::{from_v, Ty as Src};
use std::collections::HashSet;

use super::{conv, parse_int, show, Lw, LR};

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

impl<'a> Lw<'a> {
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
                let args = items(&f[1]).to_vec();
                return Some(self.map_err(e, recv, &args));
            }
        }
        if segs.len() == 2 && checked_op(&segs[1]).is_some() {
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
        if m.starts_with("checked_") {
            return Some(self.checked_mcall(e));
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
    fn optres_on(&mut self, e: &V, recv: VId, local: bool, m: &str, args: &[V]) -> LR<Option<VId>> {
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
        let Some(d) = self.expr(&args[0])? else { return Ok(None) };
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
    fn checked_with(&mut self, e: &V, recv: VId, method: &str, args: &[V]) -> LR<Option<VId>> {
        let line = nline(e);
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
    pub(super) fn match_(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let line = nline(e);
        let res: Option<Ty> = match self.src_ty(e)? {
            Src::Never => None,
            t => Some(self.conv_t(&t).ok_or_else(|| format!("行 {}: match の型 {} は段階 1 の対象外", line, show(&t)))?),
        };
        let Some(s) = self.expr_peek(&f[0])? else { return Ok(None) };
        let sty = self.vty(s);
        let arms = items(&f[1]);
        let pats: Vec<&V> = arms.iter().map(|a| &items(a)[0]).collect();
        let sc = super::coll::scrut_begin(self, &f[0], s, &pats)?;
        let saved_src = self.coll.scrut_src.take();
        self.coll.scrut_src = self.src_ty(&f[0]).ok().map(|t| (sty, t));
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
            let c = self.pat_test(pat, s, sty)?;
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
            self.slots[*x] = (jp[off + i], slot_tys[i]);
        }
        Ok(Some(if carry { jp[0] } else { self.unit() }))
    }

    /// パターン `p` の判定。値 `v` (型 `t`) について bool の値を返し、束縛を現在のスコープに入れる。
    /// 判定と束縛の値は今のブロックで先に計算する (タグを見ずにペイロードを読んでも害はない)。
    pub(super) fn pat_test(&mut self, p: &V, v: VId, t: Ty) -> LR<VId> {
        let line = nline(p);
        // 集成体の型のパターン (列挙子・タプル・構造体) は集成体の判定に任せる。束縛だけは下の `PBind` で作る。
        if matches!(t, Ty::Agg(_)) && !matches!(nname(p), "PWild" | "PBind") {
            return self.agg_pat_test(p, v, t);
        }
        match nname(p) {
            "PWild" => Ok(self.emit(Ty::Bool, Op::Const(1))),
            "PBind" => {
                let name = st(&nf(p)[0]);
                super::coll::bind_owned(self, &name, v, t);
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
                    "bool" if t == Ty::Bool => {
                        let k = self.emit(Ty::Bool, Op::Const(matches!(lf[1], V::B(true)) as i128));
                        Ok(self.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Bool, a: v, b: k }))
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
                Ok(Some(self.emit(p.ty(), Op::Payload(v))))
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
                Ok(Some(self.emit(t0.ty(), Op::Payload(v))))
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
        Ok(Some(self.emit(t0.ty(), Op::Payload(v))))
    }
}
