//! 規則 L (借用の寿命と出どころ、設計 9.4・S3D): 局所の借用が所有者より長く生きるもの (E3030)、`'static` が要る位置へ入る
//! 値 (E2037)、戻り値の領域の取り違え (E3010 の戻り値の位置) を、式の形ではなく**型の領域**で決める。
//!
//! 値ごとに「どの借用 (局所の借用 `Local`・一時値の借用 `Temp`・引数から来た借用 `Arg`) を運ぶか」を、制御の流れに沿って前向きに解く
//! (`Val`)。呼び出しの流れは呼び先の署名の領域 (`elide_sig` を当てた型) と型パラメータから決める (呼び出し側の式の形は見ない)。
//! 型が領域を持たない値 (`carries` が偽) は何も運ばない。局所の所有者が尽きる所 (ブロックの終わり・関数の終わり) で、
//! その借用を (a) 値として外へ出す、(b) 外側の生きている束縛へ格納している、ものを E3030 にする。
//!
//! 集めた領域の関係 (`SideTabs::regions::cstrs` の `let` / `assign` / `ret`) の「長い側」は、ここで解く `Val` が運ぶ借用の集合の
//! 部分集合として表れる (`'static` の位置・戻り値の位置は `Region::Static` / `Named` / `Elided` との照合で判定する)。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::check::Diag;
use crate::program::{Def, Program};
use crate::tast::*;
use crate::types::{elide_sig, from_v, subst, Region, Ty, TypeCk};

pub fn check(p: &Program, user: &[usize], tast: &Tast, _tk: &TypeCk) -> Vec<Diag> {
    let mut out = vec![];
    for &di in user {
        let Some(b) = tast.bodies.get(&di) else { continue };
        let d = &p.defs[di];
        if d.kw != "fn" {
            continue;
        }
        let mut w = W::new(p, di, b);
        w.run(b);
        out.extend(w.diags);
    }
    out
}

// ------------------------------------------------------------ 借用の集合

/// 行の印: 葉の式の行で報告する (明示の `&`・パターン・分解の束縛)。
const LEAF: usize = usize::MAX;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Loan {
    /// 局所の束縛 (名前, `PBind` の ID。引数は 0) への借用。
    Local(String, u32),
    /// 持ち主のスコープが尽きた局所への借用 (`Local` が尽きたもの)。
    Dead(String),
    /// 一時値への借用。
    Temp,
    /// 引数 (名前) から来た借用・型パラメータの値。
    Arg(String),
    /// 参照の参照の引数 (`&'a &u32`) の、内側の参照 (層) が持つ借用。
    ArgIn(String, u8),
}

/// 運ぶ借用 (借用 → その借用が入った行)。組は欄ごとに分ける。
#[derive(Clone, Debug, Default, PartialEq)]
struct Val {
    /// (借用, 参照の層) → その借用が入った行。層 0 は一番外側の参照が持つ借用、1 はその指す先の中の参照が持つ借用 (`&&T`)。
    set: BTreeMap<(Loan, u8), usize>,
    parts: Vec<Val>,
}

impl Val {
    fn loan(l: Loan, line: usize) -> Val {
        let mut v = Val::default();
        v.set.insert((l, 0), line);
        v
    }
    /// 借用した結果の層 (`&e`: 中身の層を 1 つ下げる)。
    fn up(&self) -> Val {
        Val { set: self.set.iter().map(|((l, n), x)| ((l.clone(), n.saturating_add(1)), *x)).collect(), parts: vec![] }
    }
    /// 参照を `n` 段外す (`*e`・自動参照外し): 外側の層を落とし、内側を繰り上げる。
    fn down(&self, n: usize) -> Val {
        Val { set: self.set.iter().filter(|((_, k), _)| *k as usize >= n).map(|((l, k), x)| ((l.clone(), k - n as u8), *x)).collect(), parts: vec![] }
    }
    /// 持ち主が尽きた局所の借用を `Dead` にする。
    fn kill(&mut self, dead: &HashSet<(String, u32)>) {
        let old = std::mem::take(&mut self.set);
        for ((l, k), x) in old {
            let l = match l {
                Loan::Local(n, id) if dead.contains(&(n.clone(), id)) => Loan::Dead(n),
                o => o,
            };
            self.set.entry((l, k)).and_modify(|y| *y = (*y).min(x)).or_insert(x);
        }
        for p in &mut self.parts {
            p.kill(dead);
        }
    }
    /// 一時値は文の終わりで尽きる: 束縛が持つ一時値の借用を `Dead("$temp")` にする (次の使用が E3010)。
    fn kill_temps(&mut self) {
        let old = std::mem::take(&mut self.set);
        for ((l, k), x) in old {
            let l = if l == Loan::Temp { Loan::Dead("$temp".to_string()) } else { l };
            self.set.entry((l, k)).and_modify(|y| *y = (*y).min(x)).or_insert(x);
        }
        for p in &mut self.parts {
            p.kill_temps();
        }
    }
    fn drop_dead_temps(&mut self) {
        self.set.retain(|(l, _), _| *l != Loan::Dead("$temp".to_string()));
        for p in &mut self.parts {
            p.drop_dead_temps();
        }
    }
    /// 層を問わない借用の一覧 (借用, 行)。
    fn loans(&self) -> Vec<(Loan, usize)> {
        let mut m: BTreeMap<Loan, usize> = BTreeMap::new();
        for ((l, _), x) in &self.set {
            m.entry(l.clone()).and_modify(|y| *y = (*y).min(*x)).or_insert(*x);
        }
        m.into_iter().collect()
    }
    fn add(&mut self, o: &Val) {
        for (k, l) in &o.set {
            self.set.entry(k.clone()).and_modify(|x| *x = (*x).min(*l)).or_insert(*l);
        }
        if self.parts.is_empty() {
            self.parts = o.parts.clone();
        } else if o.parts.len() == self.parts.len() {
            for (a, b) in self.parts.iter_mut().zip(&o.parts) {
                a.add(b);
            }
        } else if !o.parts.is_empty() {
            self.parts.clear();
        }
    }
    fn flat(&self) -> Val {
        Val { set: self.set.clone(), parts: vec![] }
    }
    fn union(mut self, o: &Val) -> Val {
        self.add(o);
        self
    }
    /// `let` に束縛したときの行の付け直し: 引数の借用は `let` の行、局所の借用は (明示の `&` で 0 でなければ) `let` の行。
    fn restamp(mut self, line: usize) -> Val {
        for ((l, _), x) in self.set.iter_mut() {
            if *x != LEAF {
                *x = line;
            }
        }
        for p in &mut self.parts {
            *p = std::mem::take(p).restamp(line);
        }
        self
    }
    /// 場所の違う束縛 (パターン) へ入れたとき: 局所の借用は葉の行で報告する。
    fn unstamp_locals(mut self) -> Val {
        for ((l, _), x) in self.set.iter_mut() {
            if !matches!(l, Loan::Arg(_) | Loan::ArgIn(..)) {
                *x = LEAF;
            }
        }
        for p in &mut self.parts {
            *p = std::mem::take(p).unstamp_locals();
        }
        self
    }
    fn with_line(mut self, line: usize) -> Val {
        for l in self.set.values_mut() {
            *l = line;
        }
        for p in &mut self.parts {
            *p = std::mem::take(p).with_line(line);
        }
        self
    }
    fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

#[derive(Clone, Debug)]
struct Var {
    val: Val,
    carries: bool,
    /// 宣言した `PBind` の ID (引数は 0)。
    decl: u32,
    /// 型の最上位が参照か。
    is_ref: bool,
    /// 宣言したスコープの深さ。
    depth: usize,
    /// 束縛の型。
    ty: Ty,
}

#[derive(Clone, Default)]
struct Env {
    vars: HashMap<String, Var>,
}

impl Env {
    fn merge(&mut self, o: &Env) {
        for (k, v) in &o.vars {
            match self.vars.get_mut(k) {
                Some(m) => m.val.add(&v.val),
                None => {
                    self.vars.insert(k.clone(), v.clone());
                }
            }
        }
    }
}

#[derive(Clone)]
struct Arg {
    /// 実引数の式の型 (自動参照外しの段数を数える)。自動参照した受け手は `&` を足した型。
    ty: Ty,
    full: Val,
    /// 借用式・自動参照の引数は、借りた先の中身 (参照そのものの借用を除く)。それ以外は `full` と同じ。
    inner: Val,
}

struct W<'a> {
    p: &'a Program,
    di: DefId,
    env: Env,
    /// 各スコープで宣言した名前。
    scopes: Vec<Vec<String>>,
    vals: HashMap<ExprId, Val>,
    /// 名前 → 経路の使用の式 ID。
    uses: HashMap<String, Vec<ExprId>>,
    /// 囲んでいるループの (最小 ID, 最大 ID)。
    loops: Vec<(u32, u32)>,
    ret: Ty,
    /// 戻り値の位置に出す葉 (値を返す式)。
    diags: Vec<Diag>,
    seen: HashSet<(usize, String)>,
    gn: HashSet<String>,
    /// 閉包の本体を擬似の関数として解くときの署名 (`None` は定義そのもの)。
    cl: Option<ClSig>,
}

/// 閉包を擬似の関数として見たときの署名と、外側の局所 (本体の中の局所と区別する)。
struct ClSig {
    name: String,
    params: Vec<(String, Ty)>,
    ret: Ty,
    outer: HashSet<(String, u32)>,
}

/// `v` が運ぶ借用と、それが入っている欄の型の領域 (欄に分けられない値は型全体の領域)。
fn slot_loans(v: &Val, ty: &Ty, out: &mut Vec<(Loan, usize, Vec<Region>)>) {
    let slots = slots_of(ty);
    if !v.parts.is_empty() && v.parts.len() == slots.len() {
        for (p, t) in v.parts.iter().zip(&slots) {
            slot_loans(p, t, out);
        }
        return;
    }
    let allowed = ty.regions();
    // 領域のない欄・決まらない領域の欄 (型の誤りで未決の型引数) は見ない。
    if allowed.is_empty() || allowed.iter().any(|r| matches!(r, Region::Unk)) {
        return;
    }
    for (l, line) in v.loans() {
        out.push((l, line, allowed.clone()));
    }
}

/// 値の欄 (`Val::parts` の並び): 組の各要素、`Option` / `Result` の型引数。
fn slots_of(t: &Ty) -> Vec<Ty> {
    match t {
        Ty::Tuple(ts) => ts.clone(),
        Ty::Named(n, a, _) if matches!(n.as_str(), "Option" | "Result") => a.clone(),
        _ => vec![],
    }
}

fn depth(t: &Ty) -> usize {
    let mut n = 0;
    let mut t = t;
    while let Ty::Ref(_, i, _) = t {
        n += 1;
        t = i;
    }
    n
}

fn is_ref(t: &Ty) -> bool {
    matches!(t, Ty::Ref(..))
}

fn mentions_param(t: &Ty, out: &mut BTreeSet<String>) {
    match t {
        Ty::Param(p) => {
            out.insert(p.clone());
        }
        Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().for_each(|x| mentions_param(x, out)),
        Ty::Ref(_, x, _) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => mentions_param(x, out),
        Ty::Closure { args, ret, .. } => {
            args.iter().for_each(|x| mentions_param(x, out));
            mentions_param(ret, out)
        }
        _ => {}
    }
}

fn real_regions(t: &Ty) -> Vec<Region> {
    t.regions().into_iter().filter(|r| !matches!(r, Region::Static | Region::Unk)).collect()
}

/// 領域の同一 (署名の `Named` / `Elided` どうし)。
fn same_r(a: &Region, b: &Region) -> bool {
    a.same(b)
}

fn walk<'e>(e: &'e TExpr, f: &mut dyn FnMut(&'e TExpr)) {
    f(e);
    match &e.kind {
        TKind::Call { callee, args } => {
            walk(callee, f);
            args.iter().for_each(|a| walk(a, f));
        }
        TKind::MCall { recv, args, .. } => {
            walk(recv, f);
            args.iter().for_each(|a| walk(a, f));
        }
        TKind::Field { base, .. } => walk(base, f),
        TKind::Index { base, index } => {
            walk(base, f);
            walk(index, f)
        }
        TKind::Unary { operand, .. } => walk(operand, f),
        TKind::Binary { lhs, rhs, .. } => {
            walk(lhs, f);
            walk(rhs, f)
        }
        TKind::Block(ss) => {
            for s in ss {
                match s {
                    TStmt::Let { init, .. } => {
                        if let Some(i) = init {
                            walk(i, f)
                        }
                    }
                    TStmt::Expr(x) => walk(x, f),
                    TStmt::Assign { place, value } => {
                        walk(place, f);
                        walk(value, f)
                    }
                }
            }
        }
        TKind::If { cond, then, els } => {
            walk(cond, f);
            walk(then, f);
            if let Some(x) = els {
                walk(x, f)
            }
        }
        TKind::Match { scrut, arms } => {
            walk(scrut, f);
            for a in arms {
                if let Some(g) = &a.guard {
                    walk(g, f)
                }
                walk(&a.body, f)
            }
        }
        TKind::Closure { body, .. } => walk(body, f),
        TKind::StructLit { fields, .. } => fields.iter().for_each(|(_, x)| walk(x, f)),
        TKind::Other { children, .. } => children.iter().for_each(|x| walk(x, f)),
        TKind::Lit(_) | TKind::Path(_) => {}
    }
}

fn max_id(e: &TExpr) -> u32 {
    let mut m = 0;
    walk(e, &mut |x| m = m.max(x.id));
    m
}

fn min_id(e: &TExpr) -> u32 {
    let mut m = u32::MAX;
    walk(e, &mut |x| m = m.min(x.id));
    m
}

impl<'a> W<'a> {
    fn new(p: &'a Program, di: DefId, b: &TBody) -> W<'a> {
        Self::for_body(p, di, &b.body, b.ret.clone())
    }

    fn for_body(p: &'a Program, di: DefId, body: &TExpr, ret: Ty) -> W<'a> {
        let mut uses: HashMap<String, Vec<ExprId>> = HashMap::new();
        walk(body, &mut |x| {
            if let TKind::Path(s) = &x.kind {
                uses.entry(s[0].clone()).or_default().push(x.id);
            }
        });
        let gn = p.defs[di].gnames();
        W { p, di, env: Env::default(), scopes: vec![vec![]], vals: HashMap::new(), uses, loops: vec![], ret, diags: vec![], seen: HashSet::new(), gn, cl: None }
    }

    fn diag(&mut self, code: &str, msg: String, line: usize) {
        if !self.seen.insert((line, format!("{code}{msg}"))) {
            return;
        }
        let d = &self.p.defs[self.di];
        self.diags.push(Diag { file: d.file.clone(), code: code.into(), fqn: d.name.clone(), line, col: 0, msg });
    }

    // -------------------------------------------------------- 型の判定

    /// 型が借用を運びうるか (参照・型パラメータ・領域つきの名義型・参照を欄に持つ名義型)。
    fn carries(&self, t: &Ty) -> bool {
        self.carries_in(t, &mut vec![])
    }

    fn carries_in(&self, t: &Ty, seen: &mut Vec<String>) -> bool {
        match t {
            Ty::Ref(..) | Ty::Unknown | Ty::Var(_) => true,
            Ty::Dyn(_, _, r) => !matches!(r, Region::Unk | Region::Static),
            Ty::Closure { id, .. } => id.is_some(),
            // 定義の欄の中の型パラメータは、実引数の側で見ている。
            Ty::Param(p) => seen.is_empty() && !self.static_param(p),
            Ty::Tuple(xs) => xs.iter().any(|x| self.carries_in(x, seen)),
            Ty::Array(x, _) | Ty::Slice(x) => self.carries_in(x, seen),
            Ty::Named(n, args, rgs) => {
                if !rgs.0.is_empty() || args.iter().any(|x| self.carries_in(x, seen)) {
                    return true;
                }
                if seen.contains(n) {
                    return false;
                }
                let Some(&idx) = self.p.types.get(n) else { return false };
                let d = &self.p.defs[idx];
                let gn = d.gnames();
                let tys: Vec<Ty> = if d.kw == "type" {
                    d.fields.iter().map(|f| from_v(&f.1, &gn)).collect()
                } else if d.kw == "enum" {
                    d.variants.iter().flat_map(|v| v.1.iter()).map(|x| from_v(x, &gn)).collect()
                } else {
                    return false;
                };
                seen.push(n.clone());
                let r = tys.iter().any(|t| self.carries_in(t, seen));
                seen.pop();
                r
            }
            _ => false,
        }
    }

    /// 今の定義が `T: 'static` と宣言した型パラメータ。
    fn static_param(&self, p: &str) -> bool {
        self.p.defs[self.di].generics.iter().any(|g| g.0 == "type" && g.1 == p && static_bound(&g.2))
    }

    fn keep(&self, v: Val, t: &Ty) -> Val {
        if self.carries(t) && !t.refs_all_static() {
            v
        } else {
            Val::default()
        }
    }

    // -------------------------------------------------------- 環境

    fn define(&mut self, name: &str, decl: u32, v: Val, t: &Ty) {
        let carries = self.carries(t) && !t.refs_all_static();
        let val = if carries { v } else { Val::default() };
        self.env.vars.insert(name.to_string(), Var { val, carries, decl, is_ref: is_ref(t), depth: self.scopes.len(), ty: t.clone() });
        if let Some(sc) = self.scopes.last_mut() {
            sc.push(name.to_string());
        }
    }

    fn bind_pat(&mut self, pat: &TPat, v: Val, reline: bool) {
        let v = if reline { v.unstamp_locals() } else { v };
        match pat.kind {
            "PBind" => {
                let t = pat.info.as_ref().and_then(|i| i.bind.as_ref().map(|b| b.1.clone())).unwrap_or(Ty::Unknown);
                if let Some(n) = &pat.name {
                    self.define(n, pat.id, v, &t);
                }
            }
            "PTuple" => {
                for (i, c) in pat.children.iter().enumerate() {
                    let cv = if v.parts.len() == pat.children.len() { v.parts[i].clone() } else { v.flat() };
                    self.bind_pat(c, cv, false);
                }
            }
            _ => {
                for c in &pat.children {
                    self.bind_pat(c, v.flat(), false);
                }
            }
        }
    }

    // -------------------------------------------------------- 実行

    /// 引数の借用の値: 参照の参照は、内側の層ごとに `ArgIn` を持つ。
    fn param_val(n: &str, t: &Ty) -> Val {
        let mut v = Val::loan(Loan::Arg(n.to_string()), 0);
        for lvl in 1..depth(t).min(200) {
            v.set.insert((Loan::ArgIn(n.to_string(), lvl as u8), lvl as u8), 0);
        }
        v
    }

    fn run(&mut self, b: &TBody) {
        let d = &self.p.defs[self.di];
        let params: Vec<(String, Ty)> = d.params.iter().map(|(n, t)| (n.clone(), from_v(t, &self.gn))).collect();
        for (n, t) in &params {
            let v = if self.carries(t) && !t.refs_all_static() { Self::param_val(n, t) } else { Val::default() };
            self.define(n, 0, v, t);
        }
        let _ = b;
        let v = self.ev(&b.body);
        let _ = v;
        // 関数の終わり: 戻り値の葉。
        let mut leaves = vec![];
        Self::leaves(&b.body, &mut leaves);
        let mut rets: Vec<&TExpr> = vec![];
        walk(&b.body, &mut |x| {
            if let TKind::Other { name: "Return", children } = &x.kind {
                rets.extend(children.iter());
            }
        });
        for r in rets {
            Self::leaves(r, &mut leaves);
        }
        if self.carries(&b.ret) {
            for l in leaves {
                self.escape_leaf(l, &|loan| !matches!(loan, Loan::Dead(_)), true);
                self.ret_position(l);
            }
        }
    }

    /// 閉包の本体を擬似の関数として解く (E3020・戻り値の領域の取り違え・本体の局所の借用が外へ出る・本体の中の `'static`)。
    /// 引数は借用の出どころ (`Arg`)、`move` の取り込みは外側の値が運ぶ借用をそのまま持つ。`&` / `&mut` の取り込みは
    /// 見逃す側に倒す (外側の局所への借用で、閉包より長く生きる)。
    fn closure_body(&mut self, e: &TExpr, params: &[TBinding], captures: &[Capture], body: &TExpr) {
        let Ty::Closure { ret, .. } = e.ty.strip() else { return };
        let ret: Ty = (**ret).clone();
        let d = &self.p.defs[self.di];
        let cname = format!("{}.<closure>", d.name);
        let ps: Vec<(String, Ty)> = params.iter().map(|b| (b.name.clone(), b.ty.clone())).collect();
        // 戻り値の参照は、引数の参照がちょうど 1 つのときだけ省略できる (関数の E3020 と同じ)。
        let has_ref = |t: &Ty| {
            let mut r = false;
            t.map_regions(&mut |x| {
                r = true;
                x.clone()
            });
            r
        };
        let rrs = ret.regions();
        if !rrs.is_empty() && rrs.iter().all(|r| matches!(r, Region::Unk)) && ps.iter().filter(|(_, t)| has_ref(t)).count() != 1 {
            self.diag("E3020", format!("{}: lifetime of the returned reference cannot be elided; declare lifetimes", cname), e.line);
        }
        let mut sub = W::for_body(self.p, self.di, body, ret.clone());
        let outer: HashSet<(String, u32)> = self.env.vars.iter().map(|(n, v)| (n.clone(), v.decl)).collect();
        sub.cl = Some(ClSig { name: cname, params: ps.clone(), ret: ret.clone(), outer });
        for c in captures {
            if c.kind != "&" && c.kind != "&mut" {
                if let Some(v) = self.env.vars.get(&c.name).cloned() {
                    sub.env.vars.insert(c.name.clone(), Var { depth: 1, ..v });
                    sub.scopes[0].push(c.name.clone());
                }
            }
        }
        for (n, t) in &ps {
            let v = if sub.carries(t) && !t.refs_all_static() { Self::param_val(n, t) } else { Val::default() };
            sub.define(n, 0, v, t);
        }
        sub.ev(body);
        let mut leaves = vec![];
        Self::leaves(body, &mut leaves);
        let mut rets: Vec<&TExpr> = vec![];
        walk(body, &mut |x| {
            if let TKind::Other { name: "Return", children } = &x.kind {
                rets.extend(children.iter());
            }
        });
        for r in rets {
            Self::leaves(r, &mut leaves);
        }
        if sub.carries(&ret) {
            let pnames: HashSet<String> = ps.iter().map(|(n, _)| n.clone()).collect();
            let outer = sub.cl.as_ref().map(|c| c.outer.clone()).unwrap_or_default();
            for l in leaves {
                // 本体の局所 (外側の局所ではないもの) への借用だけが、戻り値で外へ出る。
                let inside = |loan: &Loan| match loan {
                    Loan::Local(n, id) => (*id == 0 && pnames.contains(n)) || !outer.contains(&(n.clone(), *id)),
                    Loan::Temp => true,
                    _ => false,
                };
                sub.escape_leaf(l, &inside, true);
                sub.ret_position(l);
            }
        }
        for dg in std::mem::take(&mut sub.diags) {
            self.diag(&dg.code, dg.msg, dg.line);
        }
    }

    /// 値を返す葉の式 (`if` / `match` の腕・ブロックの末尾をたどる)。
    fn leaves<'e>(e: &'e TExpr, out: &mut Vec<&'e TExpr>) {
        match &e.kind {
            TKind::If { then, els, .. } => {
                Self::leaves(then, out);
                if let Some(x) = els {
                    Self::leaves(x, out)
                }
            }
            TKind::Match { arms, .. } => arms.iter().for_each(|a| Self::leaves(&a.body, out)),
            TKind::Block(ss) => {
                if let Some(TStmt::Expr(x)) = ss.last() {
                    Self::leaves(x, out)
                }
            }
            TKind::Other { name: "Return", .. } => {}
            _ => out.push(e),
        }
    }

    /// 葉の式が運ぶ借用のうち `dead` のものを E3030 にする。`ret` は関数の戻り値の位置か (文面が違う)。
    fn escape_leaf(&mut self, leaf: &TExpr, dead: &dyn Fn(&Loan) -> bool, ret: bool) {
        let Some(v) = self.vals.get(&leaf.id).cloned() else { return };
        let v = self.keep(v, &leaf.ty);
        for (l, line) in &v.loans() {
            if !dead(l) {
                continue;
            }
            let msg = match l {
                Loan::Local(n, _) | Loan::Dead(n) => {
                    format!("returns a reference to the local variable `{}`, which does not live long enough", n)
                }
                Loan::Temp => "returns a reference to a temporary value, which does not live long enough".to_string(),
                Loan::Arg(_) | Loan::ArgIn(..) => continue,
            };
            let at = if !ret || *line == 0 || *line == LEAF { leaf.line } else { *line };
            self.diag("E3030", msg, at);
        }
    }

    /// 戻り値の位置 (G14): 葉が運ぶ引数の借用が、戻り値の型の領域・宣言した包含関係のどれにも収まらなければ E3010。
    fn ret_position(&mut self, leaf: &TExpr) {
        let Some(v) = self.vals.get(&leaf.id).cloned() else { return };
        let v = self.keep(v, &leaf.ty);
        let d = &self.p.defs[self.di];
        // 閉包は、自分の引数だけで省略を解き、move で取り込んだ外側の引数 (名前付きの領域だけ) も出どころとして引ける。
        let (ps0, ps, ret, fname, rtext): (Vec<(String, Ty)>, Vec<Ty>, Ty, String, String) = match &self.cl {
            None => {
                let ps0: Vec<(String, Ty)> = d.params.iter().map(|(n, t)| (n.clone(), from_v(t, &self.gn))).collect();
                let (ps, ret) = elide_sig(&ps0, &from_v(&d.ret, &self.gn));
                (ps0, ps, ret, d.name.clone(), from_v(&d.ret, &self.gn).show_r().replace("'_ ", ""))
            }
            Some(c) => {
                let (ps, ret) = elide_sig(&c.params, &c.ret);
                let mut ps0 = c.params.clone();
                let mut ps = ps;
                let outer0: Vec<(String, Ty)> = d.params.iter().filter(|(n, _)| !c.params.iter().any(|(m, _)| m == n)).map(|(n, t)| (n.clone(), from_v(t, &self.gn))).collect();
                let (outer_ps, _) = elide_sig(&outer0, &Ty::Unit);
                for ((n, t), q) in outer0.into_iter().zip(outer_ps) {
                    // 省略された外側の領域は、閉包の署名の中では言えない。
                    if q.regions().iter().any(|r| matches!(r, Region::Elided(_))) {
                        continue;
                    }
                    ps0.push((n, t));
                    ps.push(q);
                }
                (ps0, ps, ret, c.name.clone(), c.ret.show_r().replace("'_ ", ""))
            }
        };
        let rr = ret.regions();
        if rr.is_empty() || rr.iter().any(|r| matches!(r, Region::Unk)) {
            return;
        }
        let header = d.header.split(" uses ").next().unwrap_or("").to_string();
        let mut found: Vec<(String, usize)> = vec![];
        // 借用がどの欄に入っているか: 欄 (組・`Option` / `Result` の型引数) ごとに、その欄の型の領域に収まるか。
        let mut placed: Vec<(Loan, usize, Vec<Region>)> = vec![];
        slot_loans(&v, &ret, &mut placed);
        for (l, line, allowed) in placed {
            let (q, inner) = match l {
                Loan::Arg(q) => (q, None),
                Loan::ArgIn(q, lvl) => (q, Some(lvl as usize)),
                _ => continue,
            };
            let Some(k) = ps0.iter().position(|(n, _)| *n == q) else { continue };
            let have = match inner {
                None => real_regions(&ps[k]),
                Some(lvl) => {
                    // 参照の連なりの `lvl` 番目の層の領域。
                    let mut chain = vec![];
                    let mut t = &ps[k];
                    while let Ty::Ref(_, x, r) = t {
                        chain.push(r.clone());
                        t = x;
                    }
                    match chain.get(lvl) {
                        Some(r) if !matches!(r, Region::Unk | Region::Static) => vec![r.clone()],
                        _ => continue,
                    }
                }
            };
            if have.is_empty() || ps[k].regions().iter().any(|r| matches!(r, Region::Static)) {
                continue;
            }
            if have.iter().any(|h| allowed.iter().any(|r| same_r(h, r))) {
                continue;
            }
            let named: Vec<String> = have.iter().filter_map(|h| if let Region::Named(n) = h { Some(n.clone()) } else { None }).collect();
            if named.iter().any(|h| header.contains(&format!("'{}:", h)) || header.contains(&format!("'{} :", h))) {
                continue;
            }
            let ptext = format!("{}: {}", ps0[k].0, ps0[k].1.show_r().replace("'_ ", ""));
            let what = if inner.is_some() && matches!(ps[k].outer_region(), Some(Region::Named(_))) { "the inner reference of" } else { "a borrow from" };
            let ret_named = rr.iter().any(|r| matches!(r, Region::Named(_)));
            let msg = if !named.is_empty() {
                format!(
                    "{}: the returned value carries {} argument `{}` (`{}`), but the return type `{}` does not name its lifetime; `{}` is not declared to outlive it",
                    fname,
                    what,
                    q,
                    ptext,
                    rtext,
                    named.iter().map(|h| format!("'{}", h)).collect::<Vec<_>>().join(", ")
                )
            } else if ret_named {
                format!(
                    "{}: the returned value carries {} argument `{}` (`{}`), whose lifetime is elided (a new region per call); the return type `{}` names a lifetime it does not outlive",
                    fname, what, q, ptext, rtext
                )
            } else {
                let src = ps.iter().position(|t| real_regions(t).iter().any(|h| rr.iter().any(|r| same_r(h, r)))).unwrap_or(0);
                format!(
                    "{}: the returned value carries {} argument `{}` (`{}`), but the elided return lifetime of `{}` comes from argument `{}` (spec 4.1)",
                    fname, what, q, ptext, rtext, ps0[src].0
                )
            };
            found.push((msg, line));
        }
        for (m, line) in found {
            self.diag("E3010", m, if line == 0 || line == LEAF { leaf.line } else { line });
        }
    }

    fn set_val(&mut self, e: &TExpr, v: Val) -> Val {
        self.vals.entry(e.id).and_modify(|o| o.add(&v)).or_insert_with(|| v.clone());
        v
    }

    fn ev_all(&mut self, es: &[TExpr]) -> Vec<Val> {
        es.iter().map(|x| self.ev(x)).collect()
    }

    fn proj(&self, v: &Val, name: &str) -> Val {
        match name.parse::<usize>() {
            Ok(i) if i < v.parts.len() => v.parts[i].clone(),
            _ => v.flat(),
        }
    }

    /// 式が定数 (静的な昇格を受ける)。
    fn is_const(e: &TExpr) -> bool {
        match &e.kind {
            TKind::Lit(_) => true,
            TKind::Path(_) => e.place.is_none(),
            TKind::Unary { op, operand } => op != "*" && op != "&mut" && Self::is_const(operand),
            TKind::Binary { lhs, rhs, .. } => Self::is_const(lhs) && Self::is_const(rhs),
            TKind::Other { name: "TupleE" | "ArrayE", children } => children.iter().all(Self::is_const),
            _ => false,
        }
    }

    fn local_of(e: &TExpr) -> Option<(String, u32)> {
        // `p.x.y` の経路形は欄の連なり: 根の局所を返す。
        let mut pl = e.place.as_ref()?;
        loop {
            match pl {
                Place::Local(n, id) => return Some((n.clone(), *id)),
                Place::Field(b, _) => pl = b,
                _ => return None,
            }
        }
    }

    /// `&e` の借用が運ぶもの。
    fn borrow_of(&mut self, e: &TExpr, explicit: bool) -> Val {
        let ln = if explicit { LEAF } else { e.line };
        let v = match &e.kind {
            TKind::Path(segs) => match Self::local_of(e) {
                Some((n, id)) => {
                    self.use_var(&n, e.line);
                    let var = self.env.vars.get(&n).cloned();
                    let (carries, is_r, val) = match &var {
                        Some(x) => (x.carries, x.is_ref, x.val.clone()),
                        None => (false, false, Val::default()),
                    };
                    if segs.len() > 1 && is_r {
                        val.flat()
                    } else {
                        let mut r = Val::loan(Loan::Local(n, id), ln);
                        if carries {
                            r.add(&val.flat().up());
                        }
                        r
                    }
                }
                None => Val::default(),
            },
            TKind::Field { base, .. } => {
                if is_ref(&base.ty) {
                    self.ev(base).flat()
                } else {
                    self.borrow_of(base, explicit)
                }
            }
            TKind::Index { base, index } => {
                self.ev(index);
                if is_ref(&base.ty) {
                    self.ev(base).flat()
                } else {
                    self.borrow_of(base, explicit)
                }
            }
            TKind::Unary { op, operand } if op == "*" => {
                if is_ref(&operand.ty) {
                    self.ev(operand).flat()
                } else {
                    self.borrow_of(operand, explicit)
                }
            }
            _ => {
                let v = self.ev(e).flat().up();
                if Self::is_const(e) {
                    v
                } else {
                    v.union(&Val::loan(Loan::Temp, ln))
                }
            }
        };
        if explicit { v.with_line(LEAF) } else { v }
    }

    fn arg_of(&mut self, a: &TExpr, auto_ref: bool) -> Arg {
        if auto_ref {
            let full = self.borrow_of(a, false);
            let inner = self.ev(a);
            // 受け手の中身 (参照そのものの借用を除く)。
            return Arg { ty: Ty::Ref(false, Box::new(a.ty.clone()), Region::Unk), full, inner };
        }
        if let TKind::Unary { op, operand } = &a.kind {
            if op == "&" || op == "&mut" {
                let inner = self.ev(operand);
                let full = self.ev(a);
                return Arg { ty: a.ty.clone(), full, inner };
            }
        }
        let full = self.ev(a);
        Arg { ty: a.ty.clone(), inner: full.clone(), full }
    }

    // -------------------------------------------------------- 式

    fn ev(&mut self, e: &TExpr) -> Val {
        let v = self.ev_inner(e);
        let v = if matches!(e.kind, TKind::Block(_) | TKind::If { .. } | TKind::Match { .. }) { v } else { self.keep(v, &e.ty) };
        self.set_val(e, v)
    }

    fn ev_inner(&mut self, e: &TExpr) -> Val {
        match &e.kind {
            TKind::Lit(_) => Val::default(),
            TKind::Path(segs) => match Self::local_of(e) {
                Some((n, _)) => {
                    self.use_var(&n, e.line);
                    let v = self.env.vars.get(&n).map(|x| x.val.clone()).unwrap_or_default();
                    let mut v = v;
                    for f in &segs[1..] {
                        v = self.proj(&v, f);
                    }
                    v
                }
                None => Val::default(),
            },
            TKind::Field { base, name } => {
                let b = self.ev(base);
                let b = if is_ref(&base.ty) { b.down(depth(&base.ty)) } else { b };
                self.proj(&b, name)
            }
            TKind::Index { base, index } => {
                self.ev(index);
                let b = self.ev(base);
                if is_ref(&base.ty) { b.down(depth(&base.ty)) } else { b.flat() }
            }
            TKind::Unary { op, operand } => {
                if op == "&" || op == "&mut" {
                    let v = self.borrow_of(operand, true);
                    let _ = self.ev(operand);
                    v
                } else if op == "*" {
                    let v = self.ev(operand);
                    if is_ref(&operand.ty) { v.down(1) } else { v.flat() }
                } else {
                    self.ev(operand);
                    Val::default()
                }
            }
            TKind::Binary { lhs, rhs, .. } => {
                self.ev(lhs);
                self.ev(rhs);
                Val::default()
            }
            TKind::Call { callee, args } => self.call(e, callee, args),
            TKind::MCall { recv, name, args } => self.mcall(e, recv, name, args),
            TKind::Block(stmts) => self.block(e, stmts),
            TKind::If { cond, then, els } => {
                self.ev(cond);
                let saved = self.env.clone();
                let a = self.ev(then);
                let after_then = std::mem::replace(&mut self.env, saved);
                let b = match els {
                    Some(x) => self.ev(x),
                    None => Val::default(),
                };
                self.env.merge(&after_then);
                a.union(&b)
            }
            TKind::Match { scrut, arms } => {
                let s = self.ev(scrut);
                let saved = self.env.clone();
                let mut merged: Option<Env> = None;
                let mut out = Val::default();
                for a in arms {
                    self.env = saved.clone();
                    self.scopes.push(vec![]);
                    self.bind_pat(&a.pat, s.clone(), true);
                    if let Some(g) = &a.guard {
                        self.ev(g);
                    }
                    let v = self.ev(&a.body);
                    self.escape_scope(&a.body);
                    let v = self.end_scope(v);
                    out.add(&v);
                    match &mut merged {
                        Some(m) => m.merge(&self.env),
                        None => merged = Some(self.env.clone()),
                    }
                }
                self.env = merged.unwrap_or(saved);
                out
            }
            TKind::Closure { captures, .. } => {
                let mut out = Val::default();
                for c in captures {
                    let var = self.env.vars.get(&c.name).cloned();
                    let id = var.as_ref().map_or(0, |x| x.decl);
                    let inner = var.map(|x| x.val).unwrap_or_default();
                    if c.kind == "&" || c.kind == "&mut" {
                        out.add(&Val::loan(Loan::Local(c.name.clone(), id), LEAF));
                    }
                    out.add(&inner.flat());
                }
                if let TKind::Closure { params, body, .. } = &e.kind {
                    self.closure_body(e, params, captures, body);
                }
                out
            }
            TKind::StructLit { name, fields } => {
                let mut out = Val::default();
                let vs: Vec<Val> = fields.iter().map(|(_, x)| self.ev(x)).collect();
                let segs: Vec<String> = name.split('.').map(String::from).collect();
                let file = self.p.defs[self.di].file.clone();
                let fq = self.p.expand(&segs, &file).join(".");
                let td = self.p.types.get(&fq).map(|&i| &self.p.defs[i]);
                for ((fname, _), v) in fields.iter().zip(&vs) {
                    out.add(&v.flat());
                    if let Some(td) = td {
                        self.static_field(td, &fq, fname, v, e.line);
                    }
                }
                out
            }
            TKind::Other { name, children } => self.other(e, name, children),
        }
    }

    /// 構造体の欄が `'static` の領域・`'static` 境界の型パラメータを持つとき、局所の借用を入れたら誤り。
    fn static_field(&mut self, td: &Def, fq: &str, fname: &str, v: &Val, line: usize) {
        if v.is_empty() {
            return;
        }
        let gn = td.gnames();
        let Some(f) = td.fields.iter().find(|f| f.0 == fname) else { return };
        let ft = from_v(&f.1, &gn);
        if ft.regions().iter().any(|r| matches!(r, Region::Static)) {
            for (l, _) in &v.loans() {
                let (code, msg) = match l {
                    Loan::Local(n, _) | Loan::Dead(n) => ("E3030", format!("returns a reference to the local variable `{}`, which does not live long enough", n)),
                    Loan::Temp => ("E3030", "returns a reference to a temporary value, which does not live long enough".to_string()),
                    Loan::Arg(_) | Loan::ArgIn(..) => continue,
                };
                self.diag(code, msg, line);
            }
        }
        let bounded: BTreeSet<String> = td.generics.iter().filter(|g| g.0 == "type" && static_bound(&g.2)).map(|g| g.1.clone()).collect();
        if !bounded.is_empty() {
            let mut m = BTreeSet::new();
            mentions_param(&ft, &mut m);
            if m.iter().any(|x| bounded.contains(x)) {
                self.diag("E2037", format!("the field `{}` of `{}` (a `'static` bound) must be 'static, but the value may hold a borrow of a local (5.5)", fname, fq), line);
            }
        }
    }

    fn other(&mut self, e: &TExpr, name: &str, children: &[TExpr]) -> Val {
        match name {
            "TupleE" => {
                let parts: Vec<Val> = self.ev_all(children);
                let mut v = Val::default();
                for p in &parts {
                    v.add(&p.flat());
                }
                v.parts = parts;
                v
            }
            "Try" => {
                let v = self.ev_all(children).into_iter().next().unwrap_or_default();
                // Err 側の借用は戻り値の位置へ出る (Ok 側が借用を運ばないと分かるときだけ)。
                if let Some(c) = children.first() {
                    if let Ty::Named(n, args, _) = &c.ty {
                        if n.ends_with("Result") && args.len() == 2 && !self.carries(&args[0]) && self.carries(&args[1]) && self.carries(&self.ret.clone()) {
                            self.escape_val(&v, e.line);
                        }
                    }
                }
                v
            }
            "Return" => {
                let mut leaves = vec![];
                for c in children {
                    self.ev(c);
                    Self::leaves(c, &mut leaves);
                }
                let _ = leaves;
                Val::default()
            }
            "For" | "While" | "Loop" => {
                let lo = children.iter().map(min_id).min().unwrap_or(0);
                let hi = children.iter().map(max_id).max().unwrap_or(0);
                self.loops.push((lo.min(e.id), hi));
                let saved = self.env.clone();
                let mut out = Val::default();
                for _ in 0..2 {
                    if name == "For" && !e.pats.is_empty() && !children.is_empty() {
                        // `for pat in it { .. }`: パターンの名前は、繰り返す値が運ぶ借用を持つ。
                        let it = self.ev(&children[0]);
                        self.scopes.push(vec![]);
                        for q in &e.pats {
                            self.bind_pat(q, it.flat(), true);
                        }
                        let mut last = Val::default();
                        for c in &children[1..] {
                            last = self.ev(c);
                        }
                        let _ = self.end_scope(last);
                        continue;
                    }
                    for c in children {
                        let v = self.ev(c);
                        out.add(&v);
                    }
                }
                self.loops.pop();
                // `loop` は本体を 1 回は通る: 本体が再代入する束縛の、ループの前の一時値の借用 (文の終わりで尽きたもの) は
                // 合流で戻さない (`let mut r = &mk()` の後の `loop { r = &a; break }`)。
                let mut saved = saved;
                if name == "Loop" {
                    let mut assigned: HashSet<String> = HashSet::new();
                    for c in children {
                        super::impl_sig::walk(c, &mut |x| {
                            if let TKind::Block(ss) = &x.kind {
                                for st in ss {
                                    if let TStmt::Assign { place, .. } = st {
                                        if let Some((n, _)) = Self::local_of(place) {
                                            assigned.insert(n);
                                        }
                                    }
                                }
                            }
                        });
                    }
                    for (n, var) in saved.vars.iter_mut() {
                        if assigned.contains(n) {
                            var.val.drop_dead_temps();
                        }
                    }
                }
                self.env.merge(&saved);
                let _ = out;
                Val::default()
            }
            _ => {
                let vs = self.ev_all(children);
                let mut out = Val::default();
                for v in &vs {
                    out.add(&v.flat());
                }
                out
            }
        }
    }

    fn escape_val(&mut self, v: &Val, line: usize) {
        for (l, _) in &v.loans() {
            let msg = match l {
                Loan::Local(n, _) | Loan::Dead(n) => format!("returns a reference to the local variable `{}`, which does not live long enough", n),
                Loan::Temp => "returns a reference to a temporary value, which does not live long enough".to_string(),
                Loan::Arg(_) | Loan::ArgIn(..) => continue,
            };
            self.diag("E3030", msg, line);
        }
    }

    // -------------------------------------------------------- ブロックと文

    fn block(&mut self, e: &TExpr, stmts: &[TStmt]) -> Val {
        self.scopes.push(vec![]);
        let mut tail = Val::default();
        for (i, s) in stmts.iter().enumerate() {
            match s {
                TStmt::Let { pat, init } => {
                    let v = init.as_ref().map(|x| self.ev(x)).unwrap_or_default();
                    let v = match init {
                        // 局所の読み出し (`let w = z`) は、`z` が借用を入れた行のまま。
                        Some(x) if pat.kind == "PBind" && !v.parts.is_empty() && matches!(&x.kind, TKind::Path(_)) && Self::local_of(x).is_some() => v,
                        Some(x) if pat.kind == "PBind" => v.restamp(x.line),
                        Some(_) => v.with_line(LEAF),
                        None => v,
                    };
                    self.bind_pat(pat, v, false);
                }
                TStmt::Expr(x) => {
                    let v = self.ev(x);
                    if i + 1 == stmts.len() {
                        tail = v;
                    }
                }
                TStmt::Assign { place, value } => {
                    let v = self.ev(value);
                    self.assign(place, v, value.line);
                }
            }
            if i + 1 != stmts.len() {
                for var in self.env.vars.values_mut() {
                    var.val.kill_temps();
                }
            }
        }
        // 値の位置の葉が、このブロックの局所の借用を運ぶ。
        let declared: Vec<String> = self.scopes.last().cloned().unwrap_or_default();
        let dead: HashSet<(String, u32)> = declared.iter().filter_map(|n| self.env.vars.get(n).map(|v| (n.clone(), v.decl))).collect();
        if let (true, Some(TStmt::Expr(x))) = (self.scopes.len() > 2, stmts.last()) {
            let _ = dead;
            self.escape_scope(x);
        }
        self.end_scope(tail)
    }

    /// 今のスコープの局所の借用を、値の位置の葉が運んで外へ出す (E3030)。
    fn escape_scope(&mut self, value: &TExpr) {
        let declared: Vec<String> = self.scopes.last().cloned().unwrap_or_default();
        let dead: HashSet<(String, u32)> = declared.iter().filter_map(|n| self.env.vars.get(n).map(|v| (n.clone(), v.decl))).collect();
        let mut leaves = vec![];
        Self::leaves(value, &mut leaves);
        for l in leaves {
            self.escape_leaf(l, &|loan| matches!(loan, Loan::Local(n, id) if dead.contains(&(n.clone(), *id))), false);
        }
    }

    /// スコープを閉じる: 宣言した局所が尽きる。外側の束縛・値が持つその借用は `Dead` になる (次の使用が E3010)。
    fn end_scope(&mut self, mut tail: Val) -> Val {
        let declared = self.scopes.pop().unwrap_or_default();
        let dead: HashSet<(String, u32)> = declared.iter().filter_map(|n| self.env.vars.get(n).map(|v| (n.clone(), v.decl))).collect();
        for n in &declared {
            self.env.vars.remove(n);
        }
        for var in self.env.vars.values_mut() {
            var.val.kill(&dead);
        }
        tail.kill(&dead);
        tail
    }

    /// 局所の束縛の使用。指す先が尽きた借用を持っていれば E3010。
    fn use_var(&mut self, name: &str, line: usize) {
        let Some(v) = self.env.vars.get(name) else { return };
        let dead: Vec<String> = v.val.loans().into_iter().filter_map(|(l, _)| if let Loan::Dead(n) = l { Some(n) } else { None }).collect();
        for n in dead {
            self.diag("E3010", format!("use of `{}` borrows `{}` which does not live long enough", name, n), line);
        }
    }

    /// 代入 `place = v`。
    fn assign(&mut self, place: &TExpr, v: Val, line: usize) {
        // 場所の根になる局所と、参照を通ったか。
        let mut cur = place;
        let mut through_ref = false;
        loop {
            match &cur.kind {
                TKind::Field { base, .. } | TKind::Index { base, .. } => {
                    if is_ref(&base.ty) {
                        through_ref = true;
                    }
                    cur = base;
                }
                TKind::Unary { op, operand } if op == "*" => {
                    if is_ref(&operand.ty) {
                        through_ref = true;
                    }
                    cur = operand;
                }
                _ => break,
            }
        }
        let direct = std::ptr::eq(cur, place) || matches!(&place.kind, TKind::Path(s) if s.len() == 1);
        let mut root = Self::local_of(cur);
        if let (None, TKind::Path(segs)) = (&root, &cur.kind) {
            let _ = segs;
        }
        // `p.x = ..` の経路形: 先頭が局所。
        if root.is_none() {
            if let TKind::Path(segs) = &cur.kind {
                if self.env.vars.contains_key(&segs[0]) {
                    root = Some((segs[0].clone(), 0));
                }
            }
        }
        let Some((n, _)) = root else { return };
        let Some(var) = self.env.vars.get(&n).cloned() else { return };
        if let TKind::Path(segs) = &cur.kind {
            if segs.len() > 1 && var.is_ref {
                through_ref = true;
            }
        }
        // 参照を通す書き込みも、その参照の使用 (指す先が尽きていれば E3010)。
        if through_ref {
            self.use_var(&n, line);
        }
        let target_is_ref = var.is_ref;
        let whole = direct && matches!(&place.kind, TKind::Path(s) if s.len() == 1);
        if whole && !through_ref {
            // ループの中で一時値の借用を外側のスコープの束縛へ入れるのは、一時値が尽きた後も使われる (E3010 を代入の行で出す)。
            if !self.loops.is_empty() && var.depth < self.scopes.len() && v.loans().iter().any(|(l, _)| *l == Loan::Temp) {
                self.diag("E3010", format!("use of `{}` borrows `$temp` which does not live long enough", n), line);
            }
            let nv = if var.carries { v } else { Val::default() };
            if let Some(x) = self.env.vars.get_mut(&n) {
                x.val = nv.with_line(line);
            }
            return;
        }
        // 参照を通る (`*r = ..`・`r.f = ..`)・欄への代入。
        if through_ref || target_is_ref {
            self.store_through(&var.val, &v, line, false);
        }
        if !through_ref {
            if let Some(x) = self.env.vars.get_mut(&n) {
                if x.carries {
                    x.val.add(&v.flat().with_line(line));
                }
            }
        }
    }

    /// 参照 (運ぶ借用 `refs`) の指す先へ `v` を入れる。指す先が局所ならその束縛に足す。引数の指す先 (呼び出し側の値) へ
    /// 局所の借用を入れたら E3030。
    fn store_through(&mut self, refs: &Val, v: &Val, line: usize, by_call: bool) {
        for (l, _) in &refs.loans() {
            match l {
                Loan::Local(n, _) => {
                    let hd = self.env.vars.get(n).map(|x| x.depth).unwrap_or(0);
                    if let Some(x) = self.env.vars.get_mut(n) {
                        if x.carries {
                            x.val.add(&v.flat().with_line(line));
                        }
                    }
                    // 指す先 (`n`) より内側のスコープの局所を借りた値を入れる: 内側の局所が先に尽きる。
                    for (k, _) in &v.loans() {
                        if let (true, Loan::Local(kn, _)) = (by_call, k) {
                            if self.env.vars.get(kn).map_or(false, |y| y.depth > hd) {
                                self.diag("E3030", format!("returns a reference to the local variable `{}`, which does not live long enough", kn), line);
                            }
                        }
                    }
                }
                Loan::Arg(pn) => {
                    for (k, _) in &v.loans() {
                        let msg = match k {
                            Loan::Local(n, _) | Loan::Dead(n) => format!("stores a reference to the local variable `{}` into a place that outlives it", n),
                            Loan::Temp => "stores a reference to a temporary value into a place that outlives it".to_string(),
                            Loan::Arg(qn) => {
                                self.unrelated_store(pn, qn, line);
                                continue;
                            }
                            Loan::ArgIn(..) => continue,
                        };
                        self.diag("E3030", msg, line);
                    }
                }
                Loan::Temp | Loan::Dead(_) | Loan::ArgIn(..) => {}
            }
        }
    }

    /// 引数 `q` の借用を引数 `p` の指す先へ入れる: `p` の指す先の領域と `q` の領域に共有する名前 (省略でない) が無ければ E3010。
    fn unrelated_store(&mut self, pn: &str, qn: &str, line: usize) {
        if self.cl.is_some() || pn == qn {
            return;
        }
        let d = &self.p.defs[self.di];
        let ps0: Vec<(String, Ty)> = d.params.iter().map(|(n, t)| (n.clone(), from_v(t, &self.gn))).collect();
        let (ps, _) = elide_sig(&ps0, &Ty::Unit);
        let (Some(pi), Some(qi)) = (ps0.iter().position(|(n, _)| n == pn), ps0.iter().position(|(n, _)| n == qn)) else { return };
        let Ty::Ref(true, inner, _) = &ps[pi] else { return };
        let target = real_regions(inner);
        let have = real_regions(&ps[qi]);
        if target.is_empty() || have.is_empty() || ps[qi].regions().iter().any(|r| matches!(r, Region::Static)) {
            return;
        }
        if have.iter().any(|h| target.iter().any(|t| same_r(h, t))) {
            return;
        }
        // 宣言した包含関係 (`'a: 'b`) があれば関係がある。
        let header = d.header.split(" uses ").next().unwrap_or("").to_string();
        let named: Vec<String> = have.iter().filter_map(|h| if let Region::Named(n) = h { Some(n.clone()) } else { None }).collect();
        if named.iter().any(|h| header.contains(&format!("'{}:", h)) || header.contains(&format!("'{} :", h))) {
            return;
        }
        let msg = format!(
            "{}: argument `{}`'s reference (`{}: {}`) is stored into argument `{}`'s pointee (`{}: {}`), but no shared named lifetime is declared, so the pointee's lifetime and the reference's lifetime are unrelated; declare `['a]` and use `&'a T` on both",
            d.name,
            qn,
            qn,
            ps0[qi].1.show_r().replace("'_ ", ""),
            pn,
            pn,
            ps0[pi].1.show_r().replace("'_ ", "")
        );
        self.diag("E3010", msg, line);
    }

    // -------------------------------------------------------- 呼び出し

    /// `dyn Tr` 越しのメソッド呼び出しの署名を、トレイトの宣言から実体化する (`Self` は受け手の `dyn` 型、トレイトの型パラメータは `dyn` の実引数へ)。
    fn dyn_sig(&self, ti: DefId, meth: &str, recv_ty: &Ty) -> Option<(Vec<(String, Ty)>, Ty)> {
        let td = &self.p.defs[ti];
        if td.kw != "trait" {
            return None;
        }
        let (_, ps_v, ret_v) = td.trait_sigs.iter().find(|s| s.0 == meth)?;
        let mut gn = td.gnames();
        gn.insert("Self".to_string());
        // 受け手の `dyn` 型 (参照と `Box` を外した先)。
        let mut dt = recv_ty.strip().clone();
        while let Ty::Named(n, a, _) = &dt {
            if n != "alloc.boxed.Box" || a.is_empty() {
                break;
            }
            dt = a[0].strip().clone();
        }
        let mut bind: HashMap<String, Ty> = HashMap::new();
        bind.insert("Self".into(), dt.clone());
        if let Ty::Dyn(_, ta, _) = &dt {
            for (g, a) in td.generics.iter().filter(|g| g.0 == "type").zip(&ta.0) {
                bind.insert(g.1.clone(), from_v(a, &gn));
            }
        }
        let ps0: Vec<(String, Ty)> = ps_v.iter().map(|(n, v)| (n.clone(), subst(&from_v(v, &gn), &bind))).collect();
        Some((ps0, subst(&from_v(ret_v, &gn), &bind)))
    }

    fn call(&mut self, e: &TExpr, callee: &TExpr, args: &[TExpr]) -> Val {
        // 呼び先が局所の経路 (`x.m(..)` の経路形) なら、受け手は先頭の局所。
        let mut recv_arg: Option<Arg> = None;
        let mut dyn_call: Option<(Vec<(String, Ty)>, Ty, DefId)> = None;
        let def = match &e.res {
            Some(Callee::Def(di)) => Some(*di),
            _ => None,
        };
        if let (Some(di), TKind::Path(segs)) = (def, &callee.kind) {
            let d = &self.p.defs[di];
            if d.is_method() && d.params.len() == args.len() + 1 && segs.len() >= 2 && self.env.vars.contains_key(&segs[0]) {
                self.use_var(&segs[0], e.line);
                let var = self.env.vars.get(&segs[0]).cloned().unwrap();
                let self_ref = matches!(from_v(&d.params[0].1, &self.gn), Ty::Ref(..));
                let mut inner = var.val.clone();
                let dn: usize = e.conv.iter().map(|c| if let Coercion::AutoDeref { n } = c { *n as usize } else { 0 }).sum();
                if dn > 0 {
                    inner = inner.down(dn);
                }
                let full = if self_ref && !var.is_ref {
                    let mut r = Val::loan(Loan::Local(segs[0].clone(), var.decl), e.line);
                    if var.carries {
                        r.add(&inner.flat());
                    }
                    r
                } else {
                    inner.clone()
                };
                let ty = if self_ref && !var.is_ref { Ty::Ref(false, Box::new(Ty::Unknown), Region::Unk) } else { Ty::Unknown };
                recv_arg = Some(Arg { ty, full, inner });
            }
        }
        // `dyn` 越し・解決できない経路形の呼び出し (`d.as_ref()`・`q.tag()`) も受け手を引数にする。
        if recv_arg.is_none() {
            if let TKind::Path(segs) = &callee.kind {
                if segs.len() >= 2 && self.env.vars.contains_key(&segs[0]) {
                    let rn = segs[0].clone();
                    let var = self.env.vars.get(&rn).cloned().unwrap();
                    match &e.res {
                        Some(Callee::Dyn(ti)) => {
                            if let Some((ps0, ret)) = self.dyn_sig(*ti, segs.last().unwrap(), &var.ty) {
                                self.use_var(&rn, e.line);
                                let self_ref = matches!(ps0.first().map(|p| &p.1), Some(Ty::Ref(..)));
                                let inner = var.val.clone();
                                let full = if self_ref && !var.is_ref {
                                    let mut r = Val::loan(Loan::Local(rn.clone(), var.decl), e.line);
                                    if var.carries {
                                        r.add(&inner.flat());
                                    }
                                    r
                                } else {
                                    inner.clone()
                                };
                                let ty = if self_ref && !var.is_ref { Ty::Ref(false, Box::new(Ty::Unknown), Region::Unk) } else { Ty::Unknown };
                                recv_arg = Some(Arg { ty, full, inner });
                                dyn_call = Some((ps0, ret, *ti));
                            }
                        }
                        // 解決できない呼び出しは安全側に読む: 全引数 (受け手を含む) の借用が戻り値へ流れる。
                        Some(Callee::Unknown) | None => {
                            self.use_var(&rn, e.line);
                            let mut full = Val::loan(Loan::Local(rn.clone(), var.decl), e.line);
                            if var.carries {
                                full.add(&var.val.clone().flat());
                            }
                            let inner = var.val.clone();
                            recv_arg = Some(Arg { ty: Ty::Unknown, full, inner });
                        }
                        _ => {}
                    }
                }
            }
        }
        let mut avs: Vec<Arg> = vec![];
        if let Some(r) = recv_arg {
            avs.push(r);
        }
        for a in args {
            avs.push(self.arg_of(a, false));
        }
        if !matches!(callee.kind, TKind::Path(_)) {
            self.ev(callee);
        }
        let line = e.line;
        if let Some((ps0, ret, ti)) = dyn_call {
            let td = &self.p.defs[ti];
            return self.apply_sig(&td.name, &td.generics, &ps0, &ret, &avs, line);
        }
        self.apply(e, def, &avs, line)
    }

    fn mcall(&mut self, e: &TExpr, recv: &TExpr, name: &str, args: &[TExpr]) -> Val {
        let def = match &e.res {
            Some(Callee::Def(di)) => Some(*di),
            _ => None,
        };
        let auto = recv.conv.iter().any(|c| matches!(c, Coercion::AutoRef(_))) && !is_ref(&recv.ty);
        let mut avs = vec![self.arg_of(recv, auto)];
        for a in args {
            avs.push(self.arg_of(a, false));
        }
        if def.is_none() {
            if let Some(Callee::Dyn(ti)) = &e.res {
                if let Some((ps0, ret)) = self.dyn_sig(*ti, name, &recv.ty) {
                    let td = &self.p.defs[*ti];
                    return self.apply_sig(&td.name, &td.generics, &ps0, &ret, &avs, e.line);
                }
            }
        }
        self.apply(e, def, &avs, e.line)
    }

    fn apply(&mut self, e: &TExpr, def: Option<DefId>, avs: &[Arg], line: usize) -> Val {
        let all = |avs: &[Arg]| avs.iter().fold(Val::default(), |a, x| a.union(&x.full.flat()));
        let Some(di) = def else {
            return match &e.res {
                Some(Callee::Variant(en, vn)) => {
                    let mut v = Val::default();
                    let parts: Vec<Val> = avs.iter().map(|a| a.full.clone()).collect();
                    for p in &parts {
                        v.add(&p.flat());
                    }
                    // `Some(x)` / `Ok(x)` / `Err(x)`: 型引数の欄ごとに分ける (`Err` は 2 つ目)。
                    let idx = match (en.as_str(), vn.as_str()) {
                        ("Option", "Some") | ("Result", "Ok") => Some(0),
                        ("Result", "Err") => Some(1),
                        _ => None,
                    };
                    if let (Some(i), Ty::Named(_, a, _)) = (idx, &e.ty) {
                        if avs.len() == 1 && i < a.len() {
                            let mut ps = vec![Val::default(); a.len()];
                            ps[i] = avs[0].full.clone();
                            v.parts = ps;
                        }
                    }
                    v
                }
                _ => all(avs),
            };
        };
        let d = &self.p.defs[di];
        let gn = d.gnames();
        let ps0: Vec<(String, Ty)> = d.params.iter().map(|(n, t)| (n.clone(), from_v(t, &gn))).collect();
        let ret0 = from_v(&d.ret, &gn);
        self.apply_sig(&d.name, &d.generics, &ps0, &ret0, avs, line)
    }

    /// 署名 (`ps0`・`ret0` は `from_v` 済み) に実引数を当てはめる。`dyn` 越しの呼び出しはトレイトの署名を実体化してここへ渡す。
    fn apply_sig(&mut self, name: &str, generics: &[(String, String, crate::parser::V)], ps0: &[(String, Ty)], ret0: &Ty, avs: &[Arg], line: usize) -> Val {
        let all = |avs: &[Arg]| avs.iter().fold(Val::default(), |a, x| a.union(&x.full.flat()));
        let (ps, ret) = elide_sig(ps0, ret0);
        if ps.len() != avs.len() {
            return all(avs);
        }
        // 実引数が仮引数より参照の段数が多いときは、自動参照外しで外側の借用を落とす (`p.trim()` の `p: &&str`)。
        // 仮引数が型パラメータを含むときは段数を合わせない (`&mut r` の `r: &str` を `&mut T` へ渡すとき `T = &str` で、外側の借用は落とさない)。
        let adj: Vec<Arg> = avs
            .iter()
            .zip(&ps)
            .map(|(a, pt)| {
                let n = depth(&a.ty).saturating_sub(depth(pt));
                let mut mp = BTreeSet::new();
                mentions_param(pt, &mut mp);
                if n == 0 || !mp.is_empty() || matches!(pt, Ty::Param(_) | Ty::Unknown) {
                    return a.clone();
                }
                let same = a.full == a.inner;
                let full = a.full.down(n);
                let inner = if same { full.clone() } else { a.inner.clone() };
                Arg { ty: a.ty.clone(), full, inner }
            })
            .collect();
        let avs = &adj[..];
        // 'static の位置。
        let bounded: BTreeSet<String> = generics.iter().filter(|g| g.0 == "type" && static_bound(&g.2)).map(|g| g.1.clone()).collect();
        for (k, pt) in ps.iter().enumerate() {
            let a = &avs[k];
            if a.full.is_empty() {
                continue;
            }
            let (is_static_ref, is_bound) = {
                let mut m = BTreeSet::new();
                mentions_param(pt, &mut m);
                (matches!(pt.outer_region(), Some(Region::Static)), m.iter().any(|x| bounded.contains(x)))
            };
            if is_static_ref || is_bound {
                let what = if is_static_ref { "(`&'static`)" } else { "(a `'static` bound)" };
                self.diag(
                    "E2037",
                    format!("the argument `{}` of `{}` {} must be 'static, but the value may hold a borrow of a local (5.5)", ps0[k].0, name, what),
                    line,
                );
            }
        }
        // 戻り値へ流れる引数。`flow` は戻り値の型 (の部分) へ流れる引数の借用。組・`Option`・`Result` は欄ごとに分ける。
        let flow = |ret: &Ty| -> Val {
            let rr = real_regions(ret);
            let ret_unk = ret.regions().iter().any(|r| matches!(r, Region::Unk));
            let mut ret_params = BTreeSet::new();
            mentions_param(ret, &mut ret_params);
            let mut out = Val::default();
            for (k, pt) in ps.iter().enumerate() {
                let outer_hit = pt.outer_region().map_or(false, |o| rr.iter().any(|r| same_r(o, r)));
                let mut pp = BTreeSet::new();
                mentions_param(pt, &mut pp);
                let inner_hit = real_regions(pt).iter().any(|o| rr.iter().any(|r| same_r(o, r))) || pp.iter().any(|x| ret_params.contains(x));
                if ret_unk || outer_hit {
                    out.add(&avs[k].full.flat());
                } else if inner_hit {
                    // 仮引数が `&X` なら中身 (`X` の中の借用)、`T` そのものなら実引数の全体が流れる。
                    out.add(&if matches!(pt, Ty::Ref(..)) { avs[k].inner.flat() } else { avs[k].full.flat() });
                }
            }
            out
        };
        fn nested(flow: &dyn Fn(&Ty) -> Val, t: &Ty) -> Val {
            let mut v = flow(t);
            let slots = slots_of(t);
            if !slots.is_empty() {
                v.parts = slots.iter().map(|x| nested(flow, x)).collect();
            }
            v
        }
        let mut out = nested(&flow, &ret);
        let slots = slots_of(&ret);
        if !slots.is_empty() {
        } else if let Ty::Param(rp) = &ret {
            // 総称の恒等関数 `fn id[T](x: T) -> T`: 唯一その型パラメータを取る引数の全体 (欄の構造ごと) が戻る。
            let hits: Vec<usize> = ps.iter().enumerate().filter(|(_, t)| matches!(t, Ty::Param(q) if q == rp)).map(|(k, _)| k).collect();
            if hits.len() == 1 && ps.iter().all(|t| { let mut m = BTreeSet::new(); mentions_param(t, &mut m); !m.contains(rp) || matches!(t, Ty::Param(_)) }) {
                out = avs[hits[0]].full.clone();
            }
        }
        // 引数の指す先への格納 (`&mut X` の X が、別の引数と領域・型パラメータを共有する)。
        for (k, pk) in ps.iter().enumerate() {
            let Ty::Ref(true, x, _) = pk else { continue };
            let xr = real_regions(x);
            let mut xp = BTreeSet::new();
            mentions_param(x, &mut xp);
            if xr.is_empty() && xp.is_empty() {
                continue;
            }
            let mut stored = Val::default();
            let mut via_region = false;
            for (j, pj) in ps.iter().enumerate() {
                if j == k {
                    continue;
                }
                let mut jp = BTreeSet::new();
                mentions_param(pj, &mut jp);
                let outer_hit = pj.outer_region().map_or(false, |o| xr.iter().any(|r| same_r(o, r)));
                let region_hit = real_regions(pj).iter().any(|o| xr.iter().any(|r| same_r(o, r)));
                let inner_hit = region_hit || jp.iter().any(|t| xp.contains(t));
                via_region |= region_hit;
                if outer_hit {
                    stored.add(&avs[j].full.flat());
                } else if inner_hit {
                    stored.add(&if matches!(pj, Ty::Ref(..)) { avs[j].inner.flat() } else { avs[j].full.flat() });
                }
            }
            if !stored.is_empty() {
                let target = avs[k].full.clone();
                self.store_through(&target, &stored, line, via_region);
            }
        }
        out
    }
}

fn static_bound(bounds: &crate::parser::V) -> bool {
    match bounds {
        crate::parser::V::L(xs) => xs.iter().any(|b| matches!(b, crate::parser::V::S(x) if x == "'static")),
        _ => false,
    }
}
