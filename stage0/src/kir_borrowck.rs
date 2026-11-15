//! 型付き KIR の上の借用検査 (修正プラン 1-6、実装計画 P3、設計_KIR.md 24 章)。
//!
//! 既定の `ksn check` では動かない。`KSN_BORROWCK=kir ksn check` (または `ksn check --borrowck=kir`) のときだけ走り、
//! 結果を E3010 として出す (AST の検査 `borrowck.rs` の E3010 は出さない)。KIR に下ろせない関数があるときは
//! 「判断できない」を返す (`Outcome::Unknown`。stderr に理由を出し、診断は出さない)。
//!
//! KIR は単相・SSA・ライフタイムなしなので、領域は次のように表す:
//! - 借用の出どころ (`Loan`): 局所スロットのアドレス `Slot(r)`、スロット `r` が持つヒープ領域 `Buf(r)`、引数 `Param(i)`。
//!   値ごとに出どころの集合 `org` を前向きに解く (SSA の値は 1 回定義なので値ごとに 1 つ、ブロック引数は合流)。
//! - メモリに保存された借用は、スロットのセル `(r, オフセット)` が持つ (流れに敏感。ヌル書き込みは強い更新)。
//! - 領域の生存区間は、その出どころを持つ値が生きている区間 (SSA の生存解析) と、そのセルを後で読む区間で決まる。
//! - 制約: (1) `heap.free` / `heap.realloc` / 所有物の move (ヌル書き込み) は `Buf(r)` を無効にし、無効化の後に生きている
//!   `Buf(r)` を持つ値・セルがあれば違反 (2) 同じ場所の独立した借用 (`reborrow` / `as_shared` / `field_addr`) の同時生存で、
//!   一方が可変なら違反、場所を直接読み書きするときも同様 (3) 戻り値と引数経由の書き込みは、関数ヘッダのライフタイムに
//!   照らす (戻り値の領域に含まれない引数・局所の借用を返す/引数の指す先へ入れるのは違反)。呼び出しの戻り値は、
//!   呼び先のヘッダ (`flows`) で引数から流れる。

use crate::kir::{operands, Func, Module, Op, OwnKind, Term, Ty, VId};
use crate::kir_lower::Lowered;
use crate::program::{Def, Program};
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
enum Loan {
    /// 局所スロット (`alloca` / `frame.bytes`) のアドレス。
    Slot(VId),
    /// スロットが持つヒープ領域 (Vec / String の中身)。
    Buf(VId),
    /// 引数 (その借用元は呼び出し側)。
    Param(u32),
    /// 引数の指す先から読んだ値 (戻り値の領域の照合には引数と同じに使うが、引数の指す先への書き込みの出どころには数えない)。
    ParamIn(u32),
}

type Set = BTreeSet<Loan>;

pub struct Diag {
    pub code: &'static str,
    pub fqn: String,
    pub file: String,
    pub line: usize,
    pub msg: String,
}

pub enum Outcome {
    Checked(Vec<Diag>),
    /// KIR に下ろせない (理由)。診断は出さない。
    Unknown(String),
}

/// ヘッダの文面から読んだ借用の情報。
struct Hdr {
    params: Vec<String>,
    ret_has_ref: bool,
    /// 戻り値の型の文面に `&` がある (`Option<&u32>` など)。
    ret_amp: bool,
    /// 各引数の借用が戻り値の領域に入るか (`flow_params` と同じ規則)。
    flows: Vec<bool>,
    lts: Vec<BTreeSet<String>>,
    /// 各引数の型の文面に現れる型引数 (`value: T` の `T`)。`&mut Self` と同じ型引数を持つ引数の借用は、呼び出しで `&mut` の指す先へ入る。
    tps: Vec<BTreeSet<String>>,
    mut_ref: Vec<bool>,
    carries: Vec<bool>,
    /// 引数ごとの、参照そのものの領域 (`&'a T` の `'a`、省略なら引数ごとの新しい領域) と、中の領域 (`&mut H<'b>` の `'b`)。
    outer: Vec<Rg>,
    inner: Vec<Vec<Rg>>,
    /// 戻り値の領域 (`&'a T` / `H<'a>`)。書かれていなければ None (省略規則か、型引数経由)。
    ret_rg: Option<Rg>,
    /// ヘッダの `'a: 'b` (a は b より長生き)。
    facts: Vec<(Rg, Rg)>,
}

/// 領域 (ライフタイム) の定数。領域変数は、関数の本体を解くときの制約の両端として、ヘッダの領域を指す。
#[derive(Clone, PartialEq, Eq, Debug)]
enum Rg {
    Static,
    Named(String),
    /// 省略された引数の領域 (引数の番号)。
    Elided(usize),
    /// この関数の局所 (関数を出ると終わる)。
    Local,
}

fn rg_of(name: &str) -> Rg {
    if name == "static" { Rg::Static } else { Rg::Named(name.to_string()) }
}

/// 制約 `sup: sub` (sup は sub より長生き)。`sups` のどれか 1 つが満たせばよい (中の領域のどれかなど)。
struct Cons {
    sups: Vec<Rg>,
    sub: Rg,
    line: usize,
    msg: String,
}

fn outlives(facts: &[(Rg, Rg)], a: &Rg, b: &Rg) -> bool {
    if a == b || *a == Rg::Static {
        return true;
    }
    // ヘッダの `'a: 'b` の推移閉包。
    let mut seen = vec![a.clone()];
    let mut i = 0;
    while i < seen.len() {
        let x = seen[i].clone();
        i += 1;
        for (p, q) in facts {
            if *p == x && !seen.contains(q) {
                if q == b {
                    return true;
                }
                seen.push(q.clone());
            }
        }
    }
    false
}

/// 制約の集合を解く。満たされない制約を返す。
fn solve<'a>(facts: &[(Rg, Rg)], cons: &'a [Cons]) -> Vec<&'a Cons> {
    cons.iter().filter(|c| !c.sups.iter().any(|s| outlives(facts, s, &c.sub))).collect()
}

fn match_close(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    let (o, c) = match b[open] {
        b'(' => (b'(', b')'),
        b'[' => (b'[', b']'),
        _ => return None,
    };
    let mut d = 0;
    for (i, &x) in b.iter().enumerate().skip(open) {
        if x == o {
            d += 1;
        } else if x == c {
            d -= 1;
            if d == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn split_top(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut d = 0i32;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '(' | '[' | '<' => d += 1,
            ')' | ']' | '>' => d -= 1,
            _ => {}
        }
        if c == ',' && d == 0 {
            out.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn lifetimes(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '\'' {
            let mut j = i + 1;
            while j < cs.len() && (cs[j].is_alphanumeric() || cs[j] == '_') {
                j += 1;
            }
            if j > i + 1 {
                out.insert(cs[i + 1..j].iter().collect());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// 型の文面 `text` に識別子 `name` が単語として現れるか。
fn mentions(text: &str, name: &str) -> bool {
    let b = text.as_bytes();
    let n = name.len();
    if n == 0 {
        return false;
    }
    let w = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    text.match_indices(name).any(|(i, _)| (i == 0 || !w(b[i - 1])) && (i + n >= b.len() || !w(b[i + n])))
}

impl Hdr {
    /// 引数 `k` の借用が、`&mut` の引数 `j` の指す先へ入りうるか。`k` が借用を運びうる型で、`j` と `k` が
    /// ライフタイムか型引数を共有するとき (`Vec<T>::push(self: &mut Vec<T>, value: T)` の `T`、`fn f<'a>(v: &mut X<'a>, s: &'a S)` の `'a`)。
    fn writes_into(&self, j: usize, k: usize) -> bool {
        self.mut_ref[j]
            && k != j
            && (self.carries[k] || !self.tps[k].is_empty())
            && (self.lts[k].iter().any(|x| self.lts[j].contains(x)) || self.tps[k].iter().any(|x| self.tps[j].contains(x)))
    }
}

fn parse_hdr(d: &Def) -> Option<Hdr> {
    let head = d.header.split(" uses ").next()?;
    let head = head.split(" panics ").next()?;
    let mut i = head.find(|c| c == '(' || c == '[')?;
    if head.as_bytes()[i] == b'[' {
        i = match_close(head, i)? + 1;
    }
    if head.as_bytes().get(i) != Some(&b'(') {
        return None;
    }
    let close = match_close(head, i)?;
    let params = split_top(&head[i + 1..close]);
    if params.len() != d.params.len() {
        return None;
    }
    let ret = head[close + 1..].split_once("->").map_or("", |x| x.1).trim().to_string();
    let ret_lts = lifetimes(&ret);
    // 戻り値の型が型引数 `T` を含むなら、`T` を含む引数から借用が流れうる (呼び出しで `T` が参照になる)。
    let ret_tps: Vec<&str> = d.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.as_str()).filter(|g| mentions(&ret, g)).collect();
    let ret_has_ref = ret.contains('&') || !ret_lts.is_empty() || !ret_tps.is_empty();
    let lts: Vec<BTreeSet<String>> = params.iter().map(|p| lifetimes(p)).collect();
    let tp_names: Vec<&str> = d.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.as_str()).collect();
    let tps: Vec<BTreeSet<String>> = params
        .iter()
        .map(|p| {
            let ty = p.split_once(':').map_or(p.as_str(), |x| x.1);
            tp_names.iter().filter(|g| mentions(ty, g)).map(|g| g.to_string()).collect()
        })
        .collect();
    let carries: Vec<bool> = params.iter().map(|p| p.contains('&') || p.contains('\'') || ret_tps.iter().any(|g| mentions(p, g))).collect();
    let flows: Vec<bool> = if !ret_has_ref {
        vec![false; params.len()]
    } else if !ret_lts.is_empty() || !ret_tps.is_empty() {
        lts.iter().zip(params.iter()).map(|(l, p)| l.iter().any(|x| ret_lts.contains(x)) || ret_tps.iter().any(|g| mentions(p, g))).collect()
    } else if params.first().map_or(false, |p| p.starts_with("self") && p.contains('&')) {
        (0..params.len()).map(|k| k == 0).collect()
    } else {
        carries.clone()
    };
    let mut_ref = params
        .iter()
        .map(|p| {
            let t = p.split_once(':').map_or(p.as_str(), |x| x.1).trim();
            t.starts_with("&mut") || t.starts_with("&'") && t.split_whitespace().nth(1) == Some("mut")
        })
        .collect();
    let mut outer: Vec<Rg> = vec![];
    let mut inner: Vec<Vec<Rg>> = vec![];
    for (j, p) in params.iter().enumerate() {
        let ty = p.split_once(':').map_or(p.as_str(), |x| x.1).trim();
        let all = lifetimes(ty);
        let o = if let Some(r) = ty.strip_prefix('&') {
            match r.strip_prefix('\'') {
                Some(r2) => Some(r2.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<String>()),
                None => None,
            }
        } else {
            all.iter().next().cloned()
        };
        let orr = match &o {
            Some(n) => rg_of(n),
            None => Rg::Elided(j),
        };
        let rest: Vec<Rg> = all.iter().filter(|n| Some(*n) != o.as_ref()).map(|n| rg_of(n)).collect();
        outer.push(orr);
        inner.push(rest);
    }
    let ret_rg = match ret.strip_prefix("&'") {
        Some(r) => Some(rg_of(&r.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<String>())),
        None => ret_lts.iter().next().map(|n| rg_of(n)),
    };
    // ヘッダの境界 `['a: 'b + 'c]`。
    let mut facts: Vec<(Rg, Rg)> = vec![];
    if head.as_bytes()[head.find(|c| c == '(' || c == '[').unwrap_or(0)] == b'[' {
        let o = head.find('[').unwrap_or(0);
        if let Some(c) = match_close(head, o) {
            for g in split_top(&head[o + 1..c]) {
                if let Some((a, bs)) = g.split_once(':') {
                    let a = a.trim().trim_start_matches('\'');
                    if g.trim_start().starts_with('\'') {
                        for b in bs.split('+') {
                            facts.push((rg_of(a), rg_of(b.trim().trim_start_matches('\''))));
                        }
                    }
                }
            }
        }
    }
    let ret_amp = ret.contains('&');
    Some(Hdr { params, ret_has_ref, ret_amp, flows, lts, tps, mut_ref, carries, outer, inner, ret_rg, facts })
}

fn carries_ty(t: Ty) -> bool {
    matches!(t, Ty::Ptr | Ty::Ref(..) | Ty::Agg(_) | Ty::Opt(_) | Ty::Res(..))
}

const ELEMS: u32 = u32::MAX;

#[derive(Clone, Default, PartialEq)]
struct Cell {
    live: bool,
    loans: Set,
}

type State = HashMap<(VId, u32), Cell>;

fn join(a: &mut State, b: &State) -> bool {
    let mut ch = false;
    for (k, c) in b {
        let e = a.entry(*k).or_default();
        if c.live && !e.live {
            e.live = true;
            ch = true;
        }
        for l in &c.loans {
            if e.loans.insert(*l) {
                ch = true;
            }
        }
    }
    ch
}

/// 後で調べる要求 (前向きの最終走査で集め、後ろ向きの生存解析で答える)。
enum Req {
    /// `Buf` の無効化 (free / realloc / 所有物の move)。`skip` はその命令の被演算子 (無効化の後は使われない)。
    Inval { loans: Set, skip: Vec<VId>, holders: Vec<VId>, why: &'static str },
    /// 場所 `(root, off)` を値 `via` 経由で読み書きする。
    Access { via: VId, write: bool },
    /// 借用の同時生存の検査 (被演算子も含める)。
    Pairs,
    /// 局所の寿命の終わり (`scope.end`)。その後に生きている、このスロットの借用は違反。
    Scope { root: VId, holders: Vec<VId> },
    /// 所有物の move。その後に生きている、このスロットへの借用 (`borrow.*`) は違反。`holders` は借用を
    /// セルに持つ別の場所 (`v.push(&s)` の `v`)。その後で読まれるなら違反。
    MoveBorrowed { root: VId, path: Option<Vec<u32>>, holders: Vec<VId> },
}

struct An<'a> {
    f: &'a Func,
    hdr: &'a Hdr,
    callee: &'a HashMap<String, Hdr>,
    n: usize,
    ty: Vec<Ty>,
    org: Vec<Set>,
    place: Vec<Option<(VId, Option<u32>)>>,
    /// 借用を作る命令 (`reborrow` / `as_shared` / `field_addr`) → 親の値。
    parent: HashMap<VId, VId>,
    move_loads: HashSet<VId>,
    move_stores: HashSet<(usize, usize)>,
}

fn is_null_const(f: &Func, v: VId) -> bool {
    f.blocks.iter().any(|b| b.insts.iter().any(|i| i.dst == v && matches!(i.op, Op::Const(0))))
}

impl<'a> An<'a> {
    fn setorg(&mut self, v: VId, s: Set, ch: &mut bool) {
        let e = &mut self.org[v as usize];
        for l in s {
            if e.insert(l) {
                *ch = true;
            }
        }
    }

    fn union_ops(&self, ops: &[VId]) -> Set {
        let mut s = Set::new();
        for o in ops {
            s.extend(self.org[*o as usize].iter().copied());
        }
        s
    }

    fn load_from(&self, st: &State, p: VId, off: u32, t: Ty) -> Set {
        let mut out = Set::new();
        if !carries_ty(t) {
            return out;
        }
        let pl = self.place[p as usize];
        for l in &self.org[p as usize] {
            match *l {
                Loan::Slot(r) => {
                    let o = match pl {
                        Some((r2, Some(o))) if r2 == r => Some(o + off),
                        _ => None,
                    };
                    let mut take = |c: &Cell| {
                        if t == Ty::Ptr && c.live {
                            out.insert(Loan::Buf(r));
                        }
                        out.extend(c.loans.iter().copied());
                    };
                    match o {
                        Some(o) => {
                            if let Some(c) = st.get(&(r, o)) {
                                take(c);
                            }
                            if let Some(c) = st.get(&(r, ELEMS)) {
                                out.extend(c.loans.iter().copied());
                            }
                        }
                        None => {
                            for ((r2, _), c) in st {
                                if *r2 == r {
                                    take(c);
                                }
                            }
                        }
                    }
                }
                Loan::Buf(r) => {
                    if let Some(c) = st.get(&(r, ELEMS)) {
                        out.extend(c.loans.iter().copied());
                    }
                }
                Loan::Param(j) | Loan::ParamIn(j) => {
                    out.insert(Loan::ParamIn(j));
                }
            }
        }
        out
    }

    /// `p` の指す先へ値 `v` (型 `t`、オフセット `off`) を書く。
    fn store_to(&self, st: &mut State, p: VId, off: u32, v: VId, t: Ty) {
        if !carries_ty(t) {
            return;
        }
        let vorg = self.org[v as usize].clone();
        let pl = self.place[p as usize];
        let single = self.org[p as usize].len() == 1;
        for l in &self.org[p as usize] {
            match *l {
                Loan::Slot(r) => {
                    if let (true, Some((r2, Some(o)))) = (single, pl) {
                        if r2 == r {
                            let c = st.entry((r, o + off)).or_default();
                            if t == Ty::Ptr && is_null_const(self.f, v) {
                                c.live = false;
                                c.loans.clear();
                            } else {
                                c.live = true;
                                c.loans = vorg.iter().copied().filter(|x| *x != Loan::Buf(r)).collect();
                            }
                            continue;
                        }
                    }
                    let c = st.entry((r, ELEMS)).or_default();
                    c.loans.extend(vorg.iter().copied().filter(|x| *x != Loan::Buf(r)));
                }
                Loan::Buf(r) => {
                    let c = st.entry((r, ELEMS)).or_default();
                    c.loans.extend(vorg.iter().copied().filter(|x| *x != Loan::Buf(r)));
                }
                Loan::Param(_) | Loan::ParamIn(_) => {}
            }
        }
    }

    /// 命令 1 つの前向きの効果。`org` を伸ばして変化したら `ch` を立てる。
    fn step(&mut self, st: &mut State, bi: usize, ii: usize, ch: &mut bool) {
        let inst = &self.f.blocks[bi].insts[ii];
        let d = inst.dst;
        let t = inst.ty;
        let mut res = Set::new();
        match &inst.op {
            Op::Alloca | Op::FrameBytes { .. } => {
                res.insert(Loan::Slot(d));
            }
            Op::PtrAdd { p, .. } => res = self.org[*p as usize].clone(),
            Op::FieldAddr { a, .. } | Op::Reborrow(a) | Op::AsShared(a) | Op::Borrow { a, .. } => res = self.org[*a as usize].clone(),
            Op::MemLoad { p, off } => {
                res = self.load_from(st, *p, *off, t);
                if self.move_loads.contains(&d) {
                    res.retain(|l| !matches!(l, Loan::Buf(_)));
                }
            }
            Op::Load(p) => res = self.load_from(st, *p, 0, t),
            Op::MemStore { ty, p, off, v } => self.store_to(st, *p, *off, *v, *ty),
            Op::Store { ptr, val } => {
                let vt = self.ty[*val as usize];
                self.store_to(st, *ptr, 0, *val, vt);
            }
            Op::Call { callee, args } => {
                let key = callee.split('<').next().unwrap_or(callee);
                match self.callee.get(key) {
                    Some(h) if h.params.len() == args.len() => {
                        if carries_ty(t) && h.ret_has_ref {
                            for (j, a) in args.iter().enumerate() {
                                if h.flows[j] {
                                    res.extend(self.org[*a as usize].iter().copied());
                                }
                            }
                        }
                        // `&mut X<'a>` / `&mut X<T>` の引数の指す先へ、同じ `'a` か `T` を持つ別の引数の借用が入る (セッター。`push` の `value` など)。
                        for j in 0..args.len() {
                            if !h.mut_ref[j] {
                                continue;
                            }
                            for k in 0..args.len() {
                                if h.writes_into(j, k) {
                                    let vorg = self.org[args[k] as usize].clone();
                                    for l in self.org[args[j] as usize].clone() {
                                        if let Loan::Slot(r) | Loan::Buf(r) = l {
                                            st.entry((r, ELEMS)).or_default().loans.extend(vorg.iter().copied());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {
                        if carries_ty(t) {
                            res = self.union_ops(args);
                        }
                    }
                }
            }
            Op::Const(_) | Op::StaticBytes(_) | Op::GlobalBytes { .. } | Op::HeapAlloc(_) | Op::HeapRealloc { .. } | Op::FnPtr { .. } => {}
            op @ (Op::Field { .. } | Op::SetField { .. } | Op::Make { .. } | Op::Some(_) | Op::Ok(_) | Op::Err(_) | Op::Payload(_) | Op::Bitcast { .. } | Op::Conv { .. }) => {
                if carries_ty(t) {
                    res = self.union_ops(&operands(op));
                }
            }
            // 入出力・計算の命令の結果は所有する値 (借用を運ばない)。
            _ => {}
        }
        self.setorg(d, res, ch);
    }

    fn run(&mut self) -> Vec<State> {
        let nb = self.f.blocks.len();
        let mut ins: Vec<Option<State>> = vec![None; nb];
        ins[0] = Some(State::new());
        for (i, (v, _)) in self.f.params().iter().enumerate() {
            if self.hdr.carries[i] {
                self.org[*v as usize].insert(Loan::Param(i as u32));
            }
        }
        loop {
            let mut ch = false;
            for bi in 0..nb {
                let Some(mut st) = ins[bi].clone() else { continue };
                for ii in 0..self.f.blocks[bi].insts.len() {
                    self.step(&mut st, bi, ii, &mut ch);
                }
                let tgts: Vec<(usize, Vec<VId>)> = match &self.f.blocks[bi].term {
                    Term::Br(t) => vec![t.clone()],
                    Term::BrIf(_, a, b) => vec![a.clone(), b.clone()],
                    _ => vec![],
                };
                for (tb, args) in tgts {
                    let ps: Vec<VId> = self.f.blocks[tb].params.iter().map(|p| p.0).collect();
                    for (p, a) in ps.iter().zip(args.iter()) {
                        let s = self.org[*a as usize].clone();
                        self.setorg(*p, s, &mut ch);
                    }
                    match &mut ins[tb] {
                        None => {
                            ins[tb] = Some(st.clone());
                            ch = true;
                        }
                        Some(x) => {
                            if join(x, &st) {
                                ch = true;
                            }
                        }
                    }
                }
            }
            if !ch {
                break;
            }
        }
        ins.into_iter().map(|x| x.unwrap_or_default()).collect()
    }
}

fn succs(t: &Term) -> Vec<usize> {
    match t {
        Term::Br((b, _)) => vec![*b],
        Term::BrIf(_, (a, _), (b, _)) => vec![*a, *b],
        _ => vec![],
    }
}

fn term_uses(t: &Term) -> Vec<VId> {
    match t {
        Term::Br((_, a)) => a.clone(),
        Term::BrIf(c, (_, a), (_, b)) => {
            let mut v = vec![*c];
            v.extend(a.iter().copied());
            v.extend(b.iter().copied());
            v
        }
        Term::Ret(v) => vec![*v],
        Term::Unreachable => vec![],
    }
}

/// 呼び出し `ch(args)` で、`&mut` の引数の指す先へ別の引数の借用が入るときの制約 (前向きの効果は `An::step` のセッター)。
/// 指す先は呼び出し元 (`hdr`) の引数 `pi` の中の領域。入る借用が、その領域より長生きでなければ違反。
fn call_stores(an: &An, hdr: &Hdr, ch: &Hdr, args: &[VId], line: usize, cons: &mut Vec<Cons>) {
    for j in 0..args.len() {
        if !ch.mut_ref[j] {
            continue;
        }
        for pl in an.org[args[j] as usize].iter() {
            let Loan::Param(pi) = *pl else { continue };
            let pn = hdr.params[pi as usize].split(':').next().unwrap_or("").to_string();
            let target = hdr.inner[pi as usize].first().cloned().unwrap_or_else(|| hdr.outer[pi as usize].clone());
            for k in 0..args.len() {
                if !ch.writes_into(j, k) {
                    continue;
                }
                for vl in an.org[args[k] as usize].iter() {
                    match vl {
                        Loan::Slot(_) | Loan::Buf(_) => cons.push(Cons { sups: vec![Rg::Local], sub: target.clone(), line, msg: format!("cannot store a borrow of a local variable into `{}` (it outlives the function)", pn) }),
                        Loan::Param(m) if *m != pi => {
                            let mn = hdr.params[*m as usize].split(':').next().unwrap_or("");
                            cons.push(Cons { sups: vec![hdr.outer[*m as usize].clone()], sub: target.clone(), line, msg: format!("cannot store `{}` into `{}`: its lifetime does not outlive the one `{}` holds (header lifetimes)", mn, pn, pn) });
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// 根 `root` の借用 (`Slot` / `Buf`) を、いまセルに持っている別の根 (保持者)。
fn holders_of_root(st: &State, root: VId) -> Vec<VId> {
    let mut h: Vec<VId> = vec![];
    for ((r, _), c) in st {
        if *r != root && c.loans.iter().any(|l| matches!(l, Loan::Slot(x) | Loan::Buf(x) if *x == root)) && !h.contains(r) {
            h.push(*r);
        }
    }
    h
}

fn check_func<'a>(f: &'a Func, hdr: &'a Hdr, callee: &'a HashMap<String, Hdr>, file: &str, lines: &[u32]) -> Vec<Diag> {
    let n = f.value_count();
    let nb = f.blocks.len();
    let mut ty = vec![Ty::Unit; n];
    for b in &f.blocks {
        for (v, t) in &b.params {
            ty[*v as usize] = *t;
        }
        for i in &b.insts {
            ty[i.dst as usize] = i.ty;
        }
    }
    let mut parent = HashMap::new();
    for b in &f.blocks {
        for i in &b.insts {
            match &i.op {
                Op::Reborrow(a) | Op::AsShared(a) | Op::FieldAddr { a, .. } | Op::Borrow { a, .. } => {
                    parent.insert(i.dst, *a);
                }
                _ => {}
            }
        }
    }
    // 場所 (根のスロット + オフセット)。定義の順がブロック順と限らないので、変化がなくなるまで繰り返す。
    let mut place: Vec<Option<(VId, Option<u32>)>> = vec![None; n];
    loop {
        let mut ch = false;
        for b in &f.blocks {
            for i in &b.insts {
                let np = match &i.op {
                    Op::Alloca | Op::FrameBytes { .. } => Some((i.dst, Some(0))),
                    Op::Reborrow(a) | Op::AsShared(a) | Op::Borrow { a, .. } => place[*a as usize],
                    Op::Bitcast { a, .. } if matches!(i.ty, Ty::Ptr | Ty::Ref(..)) => place[*a as usize],
                    Op::FieldAddr { a, off } => place[*a as usize].map(|(r, o)| (r, o.map(|o| o + off))),
                    Op::PtrAdd { p, .. } => place[*p as usize].map(|(r, _)| (r, None)),
                    _ => None,
                };
                if np != place[i.dst as usize] {
                    place[i.dst as usize] = np;
                    ch = true;
                }
            }
        }
        if !ch {
            break;
        }
    }
    // フィールドの道 (根からの `fieldaddr` のオフセットの列)。`None` は不明 (何とでも重なるとみなす)。
    let mut ppath: Vec<Option<Vec<u32>>> = vec![None; n];
    loop {
        let mut ch = false;
        for b in &f.blocks {
            for i in &b.insts {
                let np = match &i.op {
                    Op::Alloca | Op::FrameBytes { .. } => Some(vec![]),
                    Op::Reborrow(a) | Op::AsShared(a) | Op::Borrow { a, .. } => ppath[*a as usize].clone(),
                    Op::Bitcast { a, .. } if matches!(i.ty, Ty::Ptr | Ty::Ref(..)) => ppath[*a as usize].clone(),
                    Op::FieldAddr { a, off } => ppath[*a as usize].clone().map(|mut p| {
                        p.push(*off);
                        p
                    }),
                    _ => None,
                };
                if np != ppath[i.dst as usize] {
                    ppath[i.dst as usize] = np;
                    ch = true;
                }
            }
        }
        if !ch {
            break;
        }
    }
    // move の検出: 同じブロックで `mem.load.ptr S+o` の後に `mem.store.ptr S+o, null`。
    let mut move_loads = HashSet::new();
    let mut move_stores = HashSet::new();
    for (bi, b) in f.blocks.iter().enumerate() {
        let mut pending: HashMap<(VId, u32), VId> = HashMap::new();
        for (ii, i) in b.insts.iter().enumerate() {
            match &i.op {
                Op::MemLoad { p, off } if i.ty == Ty::Ptr => {
                    if let Some((r, Some(o))) = place[*p as usize] {
                        pending.insert((r, o + off), i.dst);
                    }
                }
                Op::MemStore { ty: Ty::Ptr, p, off, v } if is_null_const(f, *v) => {
                    if let Some((r, Some(o))) = place[*p as usize] {
                        if let Some(l) = pending.remove(&(r, o + off)) {
                            move_loads.insert(l);
                            move_stores.insert((bi, ii));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // 集成体の move: `load p` (集成体) の後に、同じブロックで、所有の印 (`frame.bytes 1`) を false にするか、`p` へ全部 0 の値を書き戻す。
    let mut agg_moves: HashMap<(usize, usize), VId> = HashMap::new();
    let def_of = |v: VId| f.blocks.iter().flat_map(|b| b.insts.iter()).find(|i| i.dst == v);
    for (bi, b) in f.blocks.iter().enumerate() {
        let mut last_load: Option<VId> = None;
        let mut loaded: HashSet<VId> = HashSet::new();
        for (ii, i) in b.insts.iter().enumerate() {
            match &i.op {
                Op::Load(p) if matches!(i.ty, Ty::Agg(_)) => {
                    last_load = Some(*p);
                    loaded.insert(*p);
                }
                Op::MemStore { ty: Ty::Bool, p, v, .. } => {
                    let flag = matches!(def_of(*p), Some(d) if matches!(d.op, Op::FrameBytes { size: 1 }));
                    let off = matches!(def_of(*v), Some(d) if matches!(d.op, Op::Const(0)));
                    if flag && off {
                        if let Some(l) = last_load {
                            agg_moves.insert((bi, ii), l);
                        }
                    }
                }
                Op::Store { ptr, val } if loaded.contains(ptr) => {
                    let zero = match def_of(*val) {
                        Some(d) => match &d.op {
                            Op::Make { fields, .. } => fields.iter().all(|x| matches!(def_of(*x), Some(fd) if matches!(fd.op, Op::Const(0)))),
                            _ => false,
                        },
                        None => false,
                    };
                    if zero {
                        agg_moves.insert((bi, ii), *ptr);
                    }
                }
                _ => {}
            }
        }
    }
    let mut an = An { f, hdr, callee, n, ty, org: vec![Set::new(); n], place, parent, move_loads, move_stores };
    let ins = an.run();
    let mut diags: Vec<Diag> = vec![];
    let mut cons: Vec<Cons> = vec![];
    let line_of = |v: VId| lines.get(v as usize).copied().unwrap_or(0) as usize;
    // 命令の行。分からないときは、同じブロックの手前の命令の行、なければ後ろの命令の行。
    let line_at = |bi: usize, ii: usize| -> usize {
        let b = &f.blocks[bi];
        (0..=ii).rev().chain(ii + 1..b.insts.len()).map(|k| line_of(b.insts[k].dst)).find(|l| *l != 0).unwrap_or(0)
    };
    let mut add = |diags: &mut Vec<Diag>, line: usize, msg: String| {
        let code = if msg.contains("reference to a local variable") { "E3030" } else { "E3010" };
        if !diags.iter().any(|d| d.line == line && d.code == code) {
            diags.push(Diag { code, fqn: f.fqn.clone(), file: file.to_string(), line, msg });
        }
    };

    // 値の使用 (free 以外)。メモリを後で読むかの判定に使う。
    let mut used_non_free: HashSet<VId> = HashSet::new();
    for b in &f.blocks {
        for i in &b.insts {
            if !matches!(i.op, Op::HeapFree(_)) {
                used_non_free.extend(operands(&i.op));
            }
        }
        used_non_free.extend(term_uses(&b.term));
    }
    // 根ごとの「内容の使用」の位置。
    let mut mem_uses: HashMap<VId, Vec<(usize, usize)>> = HashMap::new();
    for (bi, b) in f.blocks.iter().enumerate() {
        for (ii, i) in b.insts.iter().enumerate() {
            let ops = operands(&i.op);
            let is_load = matches!(i.op, Op::MemLoad { .. } | Op::Load(_)) && used_non_free.contains(&i.dst);
            let is_call = matches!(i.op, Op::Call { .. });
            if !is_load && !is_call {
                continue;
            }
            let ps: &[VId] = if is_load { &ops[..1] } else { &ops };
            for p in ps {
                for l in &an.org[*p as usize] {
                    if let Loan::Slot(r) | Loan::Buf(r) = l {
                        mem_uses.entry(*r).or_default().push((bi, ii));
                    }
                }
            }
        }
    }
    // ブロックの到達可能性 (1 辺以上)。
    let mut reach: Vec<Vec<bool>> = vec![vec![false; nb]; nb];
    for s in 0..nb {
        let mut stack: Vec<usize> = succs(&f.blocks[s].term);
        while let Some(x) = stack.pop() {
            if !reach[s][x] {
                reach[s][x] = true;
                stack.extend(succs(&f.blocks[x].term));
            }
        }
    }
    // 前向きの最終走査: 要求を集める。
    let mut reqs: Vec<(usize, usize, usize, Req)> = vec![]; // (block, idx, line, req)
    let mut st_at: State;
    for bi in 0..nb {
        st_at = ins[bi].clone();
        let mut dummy = false;
        let b = &f.blocks[bi];
        for (ii, i) in b.insts.iter().enumerate() {
            let line = line_at(bi, ii);
            if let Some(p) = agg_moves.get(&(bi, ii)) {
                for l in an.org[*p as usize].iter() {
                    if let Loan::Slot(r) = l {
                        let holders = holders_of_root(&st_at, *r);
                        reqs.push((bi, ii, line, Req::MoveBorrowed { root: *r, path: ppath[*p as usize].clone(), holders }));
                    }
                }
            }
            let holders_of = |st: &State, loans: &Set| -> Vec<VId> {
                let mut h: Vec<VId> = vec![];
                for ((r, _), c) in st {
                    if c.loans.iter().any(|l| loans.contains(l)) && !h.contains(r) {
                        h.push(*r);
                    }
                }
                h
            };
            match &i.op {
                Op::HeapFree(p) | Op::HeapRealloc { p, .. } => {
                    let loans: Set = an.org[*p as usize].iter().copied().filter(|l| matches!(l, Loan::Buf(_))).collect();
                    if !loans.is_empty() {
                        let own: Vec<VId> = loans.iter().filter_map(|l| if let Loan::Buf(r) = l { Some(*r) } else { None }).collect();
                        let mut holders = holders_of(&st_at, &loans);
                        holders.retain(|h| !own.contains(h));
                        reqs.push((bi, ii, line, Req::Inval { loans, skip: vec![*p], holders, why: "freed or reallocated" }));
                    }
                }
                Op::MemStore { ty: Ty::Ptr, p, off, v } if an.move_stores.contains(&(bi, ii)) => {
                    let _ = (off, v);
                    let loans: Set = an.org[*p as usize].iter().copied().filter_map(|l| if let Loan::Slot(r) = l { Some(Loan::Buf(r)) } else { None }).collect();
                    let own: Vec<VId> = loans.iter().filter_map(|l| if let Loan::Buf(r) = l { Some(*r) } else { None }).collect();
                    let mut holders = holders_of(&st_at, &loans);
                    holders.retain(|h| !own.contains(h));
                    reqs.push((bi, ii, line, Req::Inval { loans, skip: vec![], holders, why: "moved" }));
                    for l in an.org[*p as usize].iter() {
                        if let Loan::Slot(r) = l {
                            let holders = holders_of_root(&st_at, *r);
                            reqs.push((bi, ii, line, Req::MoveBorrowed { root: *r, path: ppath[*p as usize].clone(), holders }));
                        }
                    }
                }
                Op::MemLoad { p, .. } | Op::Load(p) => reqs.push((bi, ii, line, Req::Access { via: *p, write: false })),
                Op::MemStore { p, .. } | Op::Store { ptr: p, .. } => {
                    reqs.push((bi, ii, line, Req::Access { via: *p, write: true }));
                    // 引数の指す先への書き込み: 局所の借用は入れられない。他の引数は同じライフタイムを共有するときだけ。
                    let (pp, vv, vt) = match &i.op {
                        Op::MemStore { p, v, ty, .. } => (*p, *v, *ty),
                        Op::Store { ptr, val } => (*ptr, *val, an.ty[*val as usize]),
                        _ => unreachable!(),
                    };
                    if carries_ty(vt) {
                        for l in &an.org[pp as usize] {
                            if let Loan::Param(j) | Loan::ParamIn(j) = l {
                                let jn = hdr.params[*j as usize].split(':').next().unwrap_or("").to_string();
                                // 書き込み先の領域: 引数の中の領域 (なければ参照そのもの)。
                                let target = hdr.inner[*j as usize].first().cloned().unwrap_or_else(|| hdr.outer[*j as usize].clone());
                                for vl in &an.org[vv as usize] {
                                    match vl {
                                        Loan::Slot(_) | Loan::Buf(_) => cons.push(Cons { sups: vec![Rg::Local], sub: target.clone(), line, msg: format!("cannot store a borrow of a local variable into `{}` (it outlives the function)", jn) }),
                                        Loan::Param(k) if k != j => {
                                            let kn = hdr.params[*k as usize].split(':').next().unwrap_or("");
                                            cons.push(Cons { sups: vec![hdr.outer[*k as usize].clone()], sub: target.clone(), line, msg: format!("cannot store `{}` into `{}`: its lifetime does not outlive the one `{}` holds (header lifetimes)", kn, jn, jn) });
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
                // 呼び出しで `&mut` の引数の指す先へ、別の引数の借用が入る (`v.push(s)` の `s`)。入る借用の領域が、指す先の領域より長生きか。
                Op::Call { callee, args } => {
                    let key = callee.split('<').next().unwrap_or(callee);
                    if let Some(ch) = an.callee.get(key).filter(|ch| ch.params.len() == args.len()) {
                        call_stores(&an, hdr, ch, args, line, &mut cons);
                    }
                }
                _ => {}
            }
            if let Op::ScopeEnd(a) = &i.op {
                if let Some((r, _)) = an.place[*a as usize] {
                    let loans: Set = [Loan::Slot(r), Loan::Buf(r)].into_iter().collect();
                    let holders: Vec<VId> = holders_of(&st_at, &loans).into_iter().filter(|h| *h != r).collect();
                    reqs.push((bi, ii, line, Req::Scope { root: r, holders }));
                }
            }
            if matches!(i.op, Op::Reborrow(_) | Op::AsShared(_) | Op::Borrow { .. } | Op::FieldAddr { .. } | Op::Call { .. }) {
                reqs.push((bi, ii, line, Req::Pairs));
            }
            let mut ch = false;
            an.step(&mut st_at, bi, ii, &mut ch);
            dummy |= ch;
        }
        // 戻り値をヘッダに照らす。
        if let Term::Ret(v) = &b.term {
            let line = if b.insts.is_empty() { 0 } else { line_at(bi, b.insts.len() - 1) };
            if hdr.ret_has_ref {
                for l in &an.org[*v as usize] {
                    match l {
                        Loan::Param(j) | Loan::ParamIn(j) => {
                            let ju = *j as usize;
                            // 戻り値の領域: 書かれていればそれ。省略 / 型引数経由なら、流れる引数自身の領域 (流れないなら該当なし)。
                            let sub = match &hdr.ret_rg {
                                Some(r) => r.clone(),
                                None if hdr.flows[ju] => hdr.outer[ju].clone(),
                                None => Rg::Named("<return>".into()),
                            };
                            let mut sups = vec![hdr.outer[ju].clone()];
                            if matches!(l, Loan::ParamIn(_)) {
                                sups.extend(hdr.inner[ju].iter().cloned());
                            }
                            cons.push(Cons { sups, sub, line, msg: format!("cannot return a borrow from `{}`: its lifetime does not outlive the return value's (header lifetimes)", hdr.params[ju].split(':').next().unwrap_or("")) });
                        }
                        Loan::Slot(_) => add(&mut diags, line, "cannot return a reference to a local variable".to_string()),
                        _ => {}
                    }
                }
            }
        }
    }
    for c in solve(&hdr.facts, &cons) {
        add(&mut diags, c.line, c.msg.clone());
    }
    // 所有の検査 (move 済みの再使用 E3002、線形な値の消費漏れ E3003)。`own.*` の注釈の上の前向きの解析。
    {
        type OS = (HashSet<VId>, HashSet<VId>); // (move 済み, 消費の義務が残っている)
        let mut ins: Vec<Option<OS>> = vec![None; nb];
        ins[0] = Some((HashSet::new(), HashSet::new()));
        let mut found: Vec<(usize, &'static str, String)> = vec![];
        for round in 0..2 {
            // round 0: 不動点まで。round 1: 診断を集める。
            loop {
                let mut ch = false;
                for bi in 0..nb {
                    let Some(mut st) = ins[bi].clone() else { continue };
                    let b = &f.blocks[bi];
                    for (ii, i) in b.insts.iter().enumerate() {
                        st.0.remove(&i.dst);
                        match &i.op {
                            Op::Own { a, kind: OwnKind::Consume } => {
                                if st.0.contains(a) && round == 1 {
                                    found.push((line_at(bi, ii), "E3002", "use of moved value (it was moved earlier on some path)".to_string()));
                                }
                                st.0.insert(*a);
                                st.1.remove(a);
                            }
                            Op::Own { a, kind: OwnKind::Owe } => {
                                if st.1.contains(a) && round == 1 {
                                    found.push((line_at(bi, ii), "E3003", "linear value is not consumed on some path before it is bound again".to_string()));
                                }
                                st.1.insert(*a);
                                st.0.remove(a);
                            }
                            Op::Own { a, kind: OwnKind::Part } => {
                                if st.0.contains(a) && round == 1 {
                                    found.push((line_at(bi, ii), "E3002", "use of moved value (its field is read or moved after the whole value was moved)".to_string()));
                                }
                                st.1.remove(a);
                            }
                            Op::Own { a, kind: OwnKind::Use } => {
                                if st.0.contains(a) && round == 1 {
                                    found.push((line_at(bi, ii), "E3002", "use of moved value (it was moved earlier on some path)".to_string()));
                                }
                            }
                            Op::Own { a, kind: OwnKind::Init } => {
                                st.0.remove(a);
                            }
                            Op::MemStore { p, .. } | Op::Store { ptr: p, .. } => {
                                if let Some((r, _)) = an.place[*p as usize] {
                                    st.0.remove(&r);
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Term::Ret(_) = &b.term {
                        if round == 1 && !st.1.is_empty() {
                            let l = if b.insts.is_empty() { 0 } else { line_at(bi, b.insts.len() - 1) };
                            found.push((l, "E3003", "linear value is not consumed on some path before the function returns".to_string()));
                        }
                    }
                    for s in succs(&b.term) {
                        let mut st2 = st.clone();
                        for (p, _) in &f.blocks[s].params {
                            st2.0.remove(p);
                        }
                        match &mut ins[s] {
                            None => {
                                ins[s] = Some(st2);
                                ch = true;
                            }
                            Some(x) => {
                                let (n0, n1) = (x.0.len(), x.1.len());
                                x.0.extend(st2.0.iter().copied());
                                x.1.extend(st2.1.iter().copied());
                                if x.0.len() != n0 || x.1.len() != n1 {
                                    ch = true;
                                }
                            }
                        }
                    }
                }
                if !ch {
                    break;
                }
            }
        }
        for (l, code, msg) in found {
            if !diags.iter().any(|d| d.line == l && d.code == code) {
                diags.push(Diag { code, fqn: f.fqn.clone(), file: file.to_string(), line: l, msg });
            }
        }
    }
    let _ = dummy_use(&an.n);
    // 後ろ向きの生存解析。
    let mut live_in: Vec<HashSet<VId>> = vec![HashSet::new(); nb];
    loop {
        let mut ch = false;
        for bi in (0..nb).rev() {
            let b = &f.blocks[bi];
            let mut live: HashSet<VId> = HashSet::new();
            for s in succs(&b.term) {
                live.extend(live_in[s].iter().copied());
            }
            // ブロック引数は後続で定義される: 後続の生存からそのブロックの引数を除き、渡す値を足す。
            for t in match &b.term {
                Term::Br(t) => vec![t],
                Term::BrIf(_, a, c) => vec![a, c],
                _ => vec![],
            } {
                for (p, _) in &f.blocks[t.0].params {
                    live.remove(p);
                }
            }
            live.extend(term_uses(&b.term));
            for i in b.insts.iter().rev() {
                live.remove(&i.dst);
                live.extend(operands(&i.op));
            }
            for (p, _) in &b.params {
                live.remove(p);
            }
            if live != live_in[bi] {
                live_in[bi] = live;
                ch = true;
            }
        }
        if !ch {
            break;
        }
    }
    // 要求ごとに、命令の直後に生きている値を求める。
    let is_borrow = |v: VId| matches!(an.parent.get(&v), Some(_));
    let chain = |v: VId| -> Vec<VId> {
        let mut c = vec![v];
        let mut x = v;
        while let Some(p) = an.parent.get(&x) {
            c.push(*p);
            x = *p;
            if c.len() > 64 {
                break;
            }
        }
        c
    };
    let overlap = |a: VId, b: VId| -> bool {
        match (an.place[a as usize], an.place[b as usize]) {
            (Some((r1, _)), Some((r2, _))) if r1 == r2 => match (&ppath[a as usize], &ppath[b as usize]) {
                (Some(x), Some(y)) => x.starts_with(y) || y.starts_with(x),
                _ => true,
            },
            _ => false,
        }
    };
    let mut bmut: HashSet<VId> = HashSet::new();
    let mut bshared: HashSet<VId> = HashSet::new();
    for b in &f.blocks {
        for i in &b.insts {
            if let Op::Borrow { mutable, .. } = i.op {
                if mutable { bmut.insert(i.dst); } else { bshared.insert(i.dst); }
            }
        }
    }
    let is_mut = |v: VId| bmut.contains(&v) || (matches!(an.ty[v as usize], Ty::Ref(true, _)) && !bshared.contains(&v));
    for bi in 0..nb {
        let b = &f.blocks[bi];
        let mut live: HashSet<VId> = HashSet::new();
        for s in succs(&b.term) {
            live.extend(live_in[s].iter().copied());
        }
        for t in match &b.term {
            Term::Br(t) => vec![t],
            Term::BrIf(_, a, c) => vec![a, c],
            _ => vec![],
        } {
            for (p, _) in &f.blocks[t.0].params {
                live.remove(p);
            }
        }
        live.extend(term_uses(&b.term));
        for ii in (0..b.insts.len()).rev() {
            let i = &b.insts[ii];
            // `live` はこの命令の直後に生きている値。
            for (rb, ri, line, req) in reqs.iter().filter(|r| r.0 == bi && r.1 == ii) {
                let _ = (rb, ri);
                match req {
                    Req::Inval { loans, skip, holders, why } => {
                        let bad = live.iter().any(|w| !skip.contains(w) && an.org[*w as usize].iter().any(|l| loans.contains(l)));
                        let mut bad_holder = false;
                        for h in holders {
                            if let Some(us) = mem_uses.get(h) {
                                if us.iter().any(|(ub, ui)| (*ub == bi && *ui > ii) || reach[bi][*ub]) {
                                    bad_holder = true;
                                }
                            }
                        }
                        if bad || bad_holder {
                            add(&mut diags, *line, format!("cannot invalidate (the buffer is {}) while it is still borrowed", why));
                        }
                    }
                    Req::Access { via, write } => {
                        let ch = chain(*via);
                        for w in live.iter() {
                            if is_borrow(*w) && !ch.contains(w) && overlap(*via, *w) && (is_mut(*w) || *write) {
                                add(&mut diags, *line, "cannot access this place while it is borrowed".to_string());
                            }
                            // 呼び出しの戻り値などを通して引き継いだ借用 (場所を持たない値) が、この場所を指している。
                            if let (None, Some((root, _))) = (an.place[*w as usize], an.place[*via as usize]) {
                                if !ch.contains(w) && an.org[*w as usize].contains(&Loan::Slot(root)) && (is_mut(*w) || *write) {
                                    add(&mut diags, *line, "cannot access this place while a borrow of it is still in use".to_string());
                                }
                            }
                        }
                    }
                    Req::Scope { root, holders } => {
                        let bad = live.iter().any(|w| {
                            (is_borrow(*w) && matches!(an.place[*w as usize], Some((r, _)) if r == *root))
                                || (an.place[*w as usize].is_none() && (an.org[*w as usize].contains(&Loan::Slot(*root)) || an.org[*w as usize].contains(&Loan::Buf(*root))))
                        });
                        let bad_holder = holders.iter().any(|h| mem_uses.get(h).map_or(false, |us| us.iter().any(|(ub, ui)| (*ub == bi && *ui > ii) || reach[bi][*ub])));
                        if bad || bad_holder {
                            add(&mut diags, *line, "borrowed value does not live long enough (the variable goes out of scope while it is still borrowed)".to_string());
                        }
                    }
                    Req::MoveBorrowed { root, path, holders } => {
                        let hit = |w: &VId| {
                            is_borrow(*w)
                                && matches!(an.place[*w as usize], Some((r, _)) if r == *root)
                                && match (path, &ppath[*w as usize]) {
                                    (Some(x), Some(y)) => x.starts_with(y) || y.starts_with(x),
                                    _ => true,
                                }
                        } || (an.place[*w as usize].is_none() && an.org[*w as usize].contains(&Loan::Slot(*root)));
                        let bad_holder = holders.iter().any(|h| mem_uses.get(h).map_or(false, |us| us.iter().any(|(ub, ui)| (*ub == bi && *ui > ii) || reach[bi][*ub])));
                        if live.iter().any(hit) || bad_holder {
                            add(&mut diags, *line, "cannot move out of a variable while it is borrowed".to_string());
                        }
                    }
                    Req::Pairs => {
                        let mut set: Vec<VId> = live.iter().copied().filter(|v| is_borrow(*v)).collect();
                        set.extend(operands(&i.op).into_iter().filter(|v| is_borrow(*v)));
                        if is_borrow(i.dst) {
                            set.push(i.dst);
                        }
                        set.sort();
                        set.dedup();
                        for x in 0..set.len() {
                            for y in x + 1..set.len() {
                                let (a, c) = (set[x], set[y]);
                                if chain(a).contains(&c) || chain(c).contains(&a) || !overlap(a, c) {
                                    continue;
                                }
                                if is_mut(a) || is_mut(c) {
                                    add(&mut diags, *line, "cannot borrow the same place twice at the same time (one is mutable)".to_string());
                                }
                            }
                        }
                    }
                }
            }
            live.remove(&i.dst);
            live.extend(operands(&i.op));
        }
    }
    diags.sort_by_key(|d| d.line);
    diags
}

fn dummy_use(_: &usize) {}

/// ユーザの関数を KIR の上で検査する。下ろせない関数があれば `Unknown`。
pub fn check(p: &Program, user: &[usize], lo: &Lowered, own: bool) -> Outcome {
    let m: &Module = &lo.module;
    let mut hdrs: HashMap<String, Hdr> = HashMap::new();
    for &i in user {
        let d = &p.defs[i];
        if d.kw == "fn" {
            if let Some(h) = parse_hdr(d) {
                hdrs.insert(d.name.clone(), h);
            }
        }
    }
    // 標準ライブラリ・依存パッケージの関数のヘッダ (呼び先の戻り値の借用の流れに使う)。
    for d in &p.defs {
        if d.kw == "fn" && !hdrs.contains_key(&d.name) {
            if let Some(h) = parse_hdr(d) {
                hdrs.insert(d.name.clone(), h);
            }
        }
    }
    let mut diags: Vec<Diag> = vec![];
    for &i in user {
        let d = &p.defs[i];
        if d.kw != "fn" || d.body.is_none() {
            continue;
        }
        if let Some((_, why)) = lo.unsupported.iter().find(|(k, _)| *k == d.name) {
            return Outcome::Unknown(format!("{}: {}", d.name, why));
        }
        if d.generics.iter().any(|g| g.0 != "life") {
            // 単相化したインスタンスを検査する (呼び出されないジェネリクスは判断できない)。
            let any = m.funcs.iter().any(|f| f.fqn == d.name);
            if !any {
                return Outcome::Unknown(format!("{}: 型引数を持つ関数で、下ろしたインスタンスがない", d.name));
            }
        } else if m.find(&d.name).is_none() {
            return Outcome::Unknown(format!("{}: KIR にない", d.name));
        }
    }
    for f in &m.funcs {
        let Some(h) = hdrs.get(&f.fqn) else { continue };
        let Some(&di) = p.funcs.get(&f.fqn) else { continue };
        if !user.contains(&di) {
            continue;
        }
        // `Option<&u32>` などを KIR が値 (`Option<u32>`) に下ろすと、借用の情報が消える: 判断できない。
        if h.ret_amp {
            let lost = match f.ret {
                Ty::Opt(p) => !matches!(p.ty(), Ty::Ptr | Ty::Ref(..) | Ty::Agg(_)),
                Ty::Res(t, _) => !matches!(t.ty(), Ty::Ptr | Ty::Ref(..) | Ty::Agg(_)),
                _ => false,
            };
            if lost {
                return Outcome::Unknown(format!("{}: 戻り値の借用が KIR では値になっている ({})", f.fqn, f.ret.name()));
            }
        }
        if h.params.len() != f.params().len() {
            return Outcome::Unknown(format!("{}: KIR の引数の数がヘッダと違う", f.fqn));
        }
        let (file, lines) = match lo.lines.get(&f.key) {
            Some(l) => (l.file.clone(), l.lines.clone()),
            None => (p.defs[di].file.clone(), vec![]),
        };
        let dl = p.defs[di].line;
        diags.extend(check_func(f, h, &hdrs, &file, &lines).into_iter().filter(|d| own || !matches!(d.code, "E3002" | "E3003")).map(|mut d| {
            if d.line == 0 {
                d.line = dl;
            }
            d
        }));
    }
    Outcome::Checked(diags)
}
