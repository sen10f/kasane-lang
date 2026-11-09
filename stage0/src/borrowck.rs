//! NLL に基づく借用検査 (E3010)。関数本体を制御フローグラフの事象列に落として、
//! (1) 参照を持つ変数の生存解析 (逆向き)、(2) どの変数がどの借用を持つかの前向きの解析、
//! (3) 借用が生きている間の、競合するアクセスの検出、を行う。
//!
//! 借用の生存期間は、字句的なスコープではなく「借用を持つ変数が、そのあとで使われる経路があるか」で決まる。
//! 借用の出どころは、`let r = &x` / `&mut x` (フィールドを含む)、借用を返すメソッド・関数の戻り値
//! (`let first = v.first()`、`for x in v.iter()`)、`match` の被検査式、参照を取り込むクロージャ。
//! 場所は「根の変数 + フィールドの道」で見るので、`&mut s.a` と `s.b` への代入は競合しない。
//! 借用がコンテナ (`Vec` など) や構造体に入った場合は追わない (見落としはあっても、誤検出は出さない方針)。

use super::*;
use std::collections::VecDeque;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Acc {
    Read,
    Shared,
    MutB,
    Write,
}

#[derive(Clone, Debug)]
enum Ev {
    Use(String),
    Def(String),
    Loan { holder: String, id: usize },
    Alias { to: String, from: String },
    Access { root: String, path: Vec<String>, kind: Acc, line: usize },
}

struct LoanInfo {
    root: String,
    path: Vec<String>,
    mutable: bool,
}

#[derive(Default)]
struct Node {
    evs: Vec<Ev>,
    succ: Vec<usize>,
}

/// 借用の出どころ。
enum Src {
    /// `&x.f` / `&mut x.f` などで作る新しい借用
    Place { root: String, path: Vec<String>, mutable: bool },
    /// 参照を持つ別の変数の借用を引き継ぐ (`let r2 = r`、`&mut *r`)
    Alias(String),
}

struct Low<'a> {
    p: &'a Program,
    file: String,
    nodes: Vec<Node>,
    loans: Vec<LoanInfo>,
    locals: HashSet<String>,
    loops: Vec<(usize, usize)>,
    exit: usize,
    iters: usize,
}

/// メソッドの `self` が受ける形。
#[derive(Clone, Copy, PartialEq, Eq)]
enum SelfMode {
    Shared,
    Mut,
    Other,
}

impl<'a> Low<'a> {
    fn new_node(&mut self) -> usize {
        self.nodes.push(Node::default());
        self.nodes.len() - 1
    }

    fn emit(&mut self, n: usize, e: Ev) {
        self.nodes[n].evs.push(e);
    }

    fn edge(&mut self, a: usize, b: usize) {
        self.nodes[a].succ.push(b);
    }

    // ------------------------------------------------------------ 場所
    fn place_of(&self, e: &V) -> Option<(String, Vec<String>)> {
        match nname(e) {
            "Path" => {
                let segs = strs(&nf(e)[0]);
                if self.locals.contains(&segs[0]) { Some((segs[0].clone(), segs[1..].to_vec())) } else { None }
            }
            "Field" => {
                let (r, mut p) = self.place_of(&nf(e)[0])?;
                p.push(st(&nf(e)[1]));
                Some((r, p))
            }
            "Index" => self.place_of(&nf(e)[0]),
            "Unary" if st(&nf(e)[0]) == "*" => {
                let (r, mut p) = self.place_of(&nf(e)[1])?;
                p.push("*".into());
                Some((r, p))
            }
            _ => None,
        }
    }

    // ------------------------------------------------------------ メソッド・関数の署名
    /// 名前 `name` のメソッドの `self` の受け方 (すべての候補が一致するとき)。
    fn self_mode(&self, name: &str) -> SelfMode {
        let Some(cands) = self.p.method_idx.get(name) else { return SelfMode::Other };
        let mut mode: Option<SelfMode> = None;
        for &i in cands {
            let d = &self.p.defs[i];
            let m = match d.params.first() {
                Some((_, t)) if tag(t) == "ref" => {
                    if matches!(tel(t, 1), V::B(true)) { SelfMode::Mut } else { SelfMode::Shared }
                }
                _ => SelfMode::Other,
            };
            match mode {
                None => mode = Some(m),
                Some(x) if x == m => {}
                Some(_) => return SelfMode::Other,
            }
        }
        mode.unwrap_or(SelfMode::Other)
    }

    /// 戻り値が借用を含むか (参照型、またはライフタイム引数を持つ関数)。
    fn ret_borrows(d: &Def) -> bool {
        d.ret.dump().contains("[ref ") || d.generics.iter().any(|g| g.0 == "life")
    }

    /// 名前 `name` のメソッドが、レシーバを借りた結果を返すか。(借りるなら `self` が可変か)
    fn method_borrows(&self, name: &str) -> Option<bool> {
        let cands = self.p.method_idx.get(name)?;
        let mut any_mut = false;
        for &i in cands {
            let d = &self.p.defs[i];
            let Some((_, t)) = d.params.first() else { return None };
            if tag(t) != "ref" || !Self::ret_borrows(d) {
                return None;
            }
            any_mut |= matches!(tel(t, 1), V::B(true));
        }
        Some(any_mut)
    }

    // ------------------------------------------------------------ 借用の出どころ
    fn loan_sources(&self, e: &V) -> Vec<Src> {
        match nname(e) {
            "Unary" => {
                let op = st(&nf(e)[0]);
                if op != "&" && op != "&mut" {
                    return vec![];
                }
                let operand = &nf(e)[1];
                // 再借用 `&mut *r` は r の借用を引き継ぐ
                if nname(operand) == "Unary" && st(&nf(operand)[0]) == "*" {
                    if let Some((root, _)) = self.place_of(&nf(operand)[1]) {
                        return vec![Src::Alias(root)];
                    }
                }
                match self.place_of(operand) {
                    Some((root, path)) => vec![Src::Place { root, path, mutable: op == "&mut" }],
                    None => vec![],
                }
            }
            "Path" => {
                let segs = strs(&nf(e)[0]);
                if segs.len() == 1 && self.locals.contains(&segs[0]) { vec![Src::Alias(segs[0].clone())] } else { vec![] }
            }
            "Try" => self.loan_sources(&nf(e)[0]),
            "MCall" => {
                let name = st(&nf(e)[1]);
                let recv = &nf(e)[0];
                if matches!(name.as_str(), "unwrap" | "expect" | "unwrap_or") {
                    return self.loan_sources(recv);
                }
                self.method_sources(recv, &name)
            }
            "Call" => {
                if nname(&nf(e)[0]) != "Path" {
                    return vec![];
                }
                let segs = strs(&nf(&nf(e)[0])[0]);
                if segs.len() >= 2 && self.locals.contains(&segs[0]) {
                    // `v.first()` の呼び出し形: レシーバは segs[..-1]
                    let name = segs[segs.len() - 1].clone();
                    if matches!(name.as_str(), "unwrap" | "expect" | "unwrap_or") {
                        return vec![];
                    }
                    let recv_segs = segs[..segs.len() - 1].to_vec();
                    return match self.method_borrows(&name) {
                        Some(m) => vec![Src::Place { root: recv_segs[0].clone(), path: recv_segs[1..].to_vec(), mutable: m }],
                        None => vec![],
                    };
                }
                // 関数の戻り値が借用を含むなら、借用の引数 (`&x` / `&mut x` / 参照の変数) から借りる
                let Some(r) = self.p.resolve(&segs, &self.file) else { return vec![] };
                let di = match r.target {
                    Target::Fn(i) | Target::Sig(i) => i,
                    _ => return vec![],
                };
                if !r.rest.is_empty() || !Self::ret_borrows(&self.p.defs[di]) {
                    return vec![];
                }
                items(&nf(e)[1]).iter().flat_map(|a| self.loan_sources(a)).collect()
            }
            "Closure" => {
                let mut out = vec![];
                for c in items(&nf(e)[2]) {
                    let kind = st(tel(c, 0));
                    let name = st(tel(c, 1));
                    if self.locals.contains(&name) && (kind == "&" || kind == "&mut") {
                        out.push(Src::Place { root: name, path: vec![], mutable: kind == "&mut" });
                    }
                }
                out
            }
            _ => vec![],
        }
    }

    fn method_sources(&self, recv: &V, name: &str) -> Vec<Src> {
        let Some(m) = self.method_borrows(name) else { return vec![] };
        match self.place_of(recv) {
            Some((root, path)) => vec![Src::Place { root, path, mutable: m }],
            None => {
                // `v.iter().rev()` のように、借用を返す呼び出しの連なり
                if matches!(nname(recv), "MCall") { self.loan_sources(recv) } else { vec![] }
            }
        }
    }

    /// `holder` が `srcs` の借用を持つようにする事象を出す。
    fn hold(&mut self, n: usize, holder: &str, srcs: Vec<Src>) {
        for s in srcs {
            match s {
                Src::Place { root, path, mutable } => {
                    let id = self.loans.len();
                    self.loans.push(LoanInfo { root, path, mutable });
                    self.emit(n, Ev::Loan { holder: holder.to_string(), id });
                }
                Src::Alias(from) => {
                    if from != holder {
                        self.emit(n, Ev::Alias { to: holder.to_string(), from });
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ 式・文
    fn lower_block(&mut self, b: &V, mut cur: usize) -> usize {
        if nname(b) != "Block" {
            return self.lower_expr(b, cur);
        }
        for s in items(&nf(b)[0]) {
            cur = self.lower_stmt(s, cur);
        }
        cur
    }

    fn lower_stmt(&mut self, s: &V, cur: usize) -> usize {
        match nname(s) {
            "Let" => {
                let f = nf(s);
                let init = &f[3];
                let c = if is_nil(init) { cur } else { self.lower_expr(init, cur) };
                let mut binds = vec![];
                pat_binds(&f[0], &mut binds);
                let srcs = if is_nil(init) || nname(&f[0]) != "PBind" { vec![] } else { self.loan_sources(init) };
                for (k, b) in binds.iter().enumerate() {
                    self.emit(c, Ev::Def(b.clone()));
                    if k == 0 && !srcs.is_empty() {
                        // 借用の出どころは、束縛が 1 つのときだけ (`let r = &x`)
                        let srcs2 = self.loan_sources(init);
                        self.hold(c, b, srcs2);
                    }
                }
                c
            }
            "Assign" => {
                let f = nf(s);
                let c = self.lower_expr(&f[1], cur);
                let target = &f[0];
                if nname(target) == "Path" && strs(&nf(target)[0]).len() == 1 && self.locals.contains(&strs(&nf(target)[0])[0]) {
                    let name = strs(&nf(target)[0])[0].clone();
                    // `r = &y`: 古い借用はここで手放す (NLL)。新しい借用を持つ。
                    let srcs = self.loan_sources(&f[1]);
                    self.emit(c, Ev::Def(name.clone()));
                    self.hold(c, &name, srcs);
                    // 値の代入そのものも、その変数への書き込み
                    self.emit(c, Ev::Access { root: name, path: vec![], kind: Acc::Write, line: nline(s) });
                    return c;
                }
                // `*r = v` は r を使うだけ。`s.f = v` / `v[i] = x` は根の変数への書き込み。
                if nname(target) == "Unary" && st(&nf(target)[0]) == "*" {
                    return self.lower_expr(&nf(target)[1], c);
                }
                let c = self.lower_index_parts(target, c);
                if let Some((root, path)) = self.place_of(target) {
                    self.emit(c, Ev::Use(root.clone()));
                    self.emit(c, Ev::Access { root, path, kind: Acc::Write, line: nline(s) });
                }
                c
            }
            "ExprStmt" => self.lower_expr(&nf(s)[0], cur),
            _ => self.lower_expr(s, cur),
        }
    }

    /// 場所の式の中にある添字の式 (`v[i + 1]` の `i + 1`) を先に評価する。
    fn lower_index_parts(&mut self, e: &V, cur: usize) -> usize {
        match nname(e) {
            "Index" => {
                let c = self.lower_index_parts(&nf(e)[0], cur);
                self.lower_expr(&nf(e)[1], c)
            }
            "Field" => self.lower_index_parts(&nf(e)[0], cur),
            _ => cur,
        }
    }

    fn access(&mut self, n: usize, e: &V, kind: Acc) -> bool {
        match self.place_of(e) {
            Some((root, path)) => {
                self.emit(n, Ev::Use(root.clone()));
                self.emit(n, Ev::Access { root, path, kind, line: nline(e) });
                true
            }
            None => false,
        }
    }

    fn lower_expr(&mut self, e: &V, cur: usize) -> usize {
        let f = nf(e);
        match nname(e) {
            "Lit" => cur,
            "Path" | "Field" | "Index" => {
                if nname(e) == "Index" {
                    let c = self.lower_index_parts(e, cur);
                    self.access(c, e, Acc::Read);
                    return c;
                }
                if self.access(cur, e, Acc::Read) {
                    return cur;
                }
                let mut c = cur;
                for ch in children(e) {
                    c = self.lower_expr(ch, c);
                }
                c
            }
            "Unary" => {
                let op = st(&f[0]);
                if op == "&" || op == "&mut" {
                    let c = self.lower_index_parts(&f[1], cur);
                    // `&*r` / `&mut *r` は r の使用
                    if nname(&f[1]) == "Unary" && st(&nf(&f[1])[0]) == "*" {
                        return self.lower_expr(&nf(&f[1])[1], c);
                    }
                    let kind = if op == "&mut" { Acc::MutB } else { Acc::Shared };
                    if self.access(c, &f[1], kind) {
                        return c;
                    }
                    return self.lower_expr(&f[1], c);
                }
                self.lower_expr(&f[1], cur)
            }
            "Bin" => {
                let op = st(&f[0]);
                if op == "&&" || op == "||" {
                    let c = self.lower_expr(&f[1], cur);
                    let rhs = self.new_node();
                    let join = self.new_node();
                    self.edge(c, rhs);
                    self.edge(c, join);
                    let r_end = self.lower_expr(&f[2], rhs);
                    self.edge(r_end, join);
                    return join;
                }
                let c = self.lower_expr(&f[1], cur);
                self.lower_expr(&f[2], c)
            }
            "If" => {
                let c = self.lower_expr(&f[0], cur);
                let (then_n, else_n, join) = (self.new_node(), self.new_node(), self.new_node());
                self.edge(c, then_n);
                self.edge(c, else_n);
                let t_end = self.lower_block(&f[1], then_n);
                self.edge(t_end, join);
                let e_end = if is_nil(&f[2]) { else_n } else { self.lower_block(&f[2], else_n) };
                self.edge(e_end, join);
                join
            }
            "While" => {
                let head = self.new_node();
                self.edge(cur, head);
                let c = self.lower_expr(&f[0], head);
                let (body, exit) = (self.new_node(), self.new_node());
                self.edge(c, body);
                self.edge(c, exit);
                self.loops.push((head, exit));
                let end = self.lower_block(&f[1], body);
                self.loops.pop();
                self.edge(end, head);
                exit
            }
            "Loop" => {
                let head = self.new_node();
                self.edge(cur, head);
                let exit = self.new_node();
                self.loops.push((head, exit));
                let end = self.lower_block(&f[0], head);
                self.loops.pop();
                self.edge(end, head);
                exit
            }
            "For" => {
                let c = self.lower_expr(&f[1], cur);
                // 反復子そのものが借用を持ち続ける (ループの間ずっと使われる)
                let holder = format!("$iter{}", self.iters);
                self.iters += 1;
                let srcs = self.loan_sources(&f[1]);
                self.emit(c, Ev::Def(holder.clone()));
                self.hold(c, &holder, srcs);
                let head = self.new_node();
                self.edge(c, head);
                self.emit(head, Ev::Use(holder.clone()));
                let (body, exit) = (self.new_node(), self.new_node());
                self.edge(head, body);
                self.edge(head, exit);
                let mut binds = vec![];
                pat_binds(&f[0], &mut binds);
                for b in &binds {
                    self.emit(body, Ev::Def(b.clone()));
                    let srcs = self.loan_sources(&f[1]);
                    self.hold(body, b, srcs);
                }
                self.loops.push((head, exit));
                let end = self.lower_block(&f[2], body);
                self.loops.pop();
                self.edge(end, head);
                exit
            }
            "Match" => {
                let c = self.lower_expr(&f[0], cur);
                let join = self.new_node();
                let arms = items(&f[1]);
                if arms.is_empty() {
                    return c;
                }
                for arm in arms {
                    let a = items(arm);
                    let an = self.new_node();
                    self.edge(c, an);
                    let mut binds = vec![];
                    pat_binds(&a[0], &mut binds);
                    for b in &binds {
                        self.emit(an, Ev::Def(b.clone()));
                        let srcs = self.loan_sources(&f[0]);
                        self.hold(an, b, srcs);
                    }
                    let end = self.lower_block(&a[1], an);
                    self.edge(end, join);
                }
                join
            }
            "Return" => {
                let c = if is_nil(&f[0]) { cur } else { self.lower_expr(&f[0], cur) };
                let exit = self.exit;
                self.edge(c, exit);
                self.new_node()
            }
            "Break" => {
                if let Some(&(_, exit)) = self.loops.last() {
                    self.edge(cur, exit);
                }
                self.new_node()
            }
            "Continue" => {
                if let Some(&(head, _)) = self.loops.last() {
                    self.edge(cur, head);
                }
                self.new_node()
            }
            "Try" => {
                let c = self.lower_expr(&f[0], cur);
                let exit = self.exit;
                self.edge(c, exit);
                c
            }
            "Closure" => {
                // 取り込む変数は、作った時点で使われ、借用される。本体の中は追わない。
                for c in items(&f[2]) {
                    let kind = st(tel(c, 0));
                    let name = st(tel(c, 1));
                    if !self.locals.contains(&name) {
                        continue;
                    }
                    self.emit(cur, Ev::Use(name.clone()));
                    let k = match kind.as_str() {
                        "&mut" => Acc::MutB,
                        "&" => Acc::Shared,
                        _ => Acc::Read,
                    };
                    self.emit(cur, Ev::Access { root: name, path: vec![], kind: k, line: nline(e) });
                }
                cur
            }
            "Block" => self.lower_block(e, cur),
            "Unsafe" => self.lower_block(&f[2], cur),
            "Call" => {
                let callee = &f[0];
                let args = items(&f[1]);
                if nname(callee) == "Path" {
                    let segs = strs(&nf(callee)[0]);
                    if segs.len() >= 2 && self.locals.contains(&segs[0]) {
                        // `v.push(x)`: メソッド呼び出し (レシーバは segs[..-1])
                        let mut c = cur;
                        for a in args {
                            c = self.lower_expr(a, c);
                        }
                        let name = segs[segs.len() - 1].clone();
                        let kind = match self.self_mode(&name) {
                            SelfMode::Mut => Acc::MutB,
                            _ => Acc::Read,
                        };
                        let root = segs[0].clone();
                        let path = segs[1..segs.len() - 1].to_vec();
                        self.emit(c, Ev::Use(root.clone()));
                        self.emit(c, Ev::Access { root, path, kind, line: nline(e) });
                        return c;
                    }
                    if segs.len() == 1 && self.locals.contains(&segs[0]) {
                        self.emit(cur, Ev::Use(segs[0].clone()));
                    }
                } else {
                    // 呼び出し先の式 (ここでは追わない)
                }
                let mut c = cur;
                for a in args {
                    c = self.lower_expr(a, c);
                }
                c
            }
            "MCall" => {
                let recv = &f[0];
                let name = st(&f[1]);
                let args = items(&f[2]);
                if self.place_of(recv).is_some() {
                    let mut c = self.lower_index_parts(recv, cur);
                    for a in args {
                        c = self.lower_expr(a, c);
                    }
                    let kind = match self.self_mode(&name) {
                        SelfMode::Mut => Acc::MutB,
                        _ => Acc::Read,
                    };
                    self.access(c, recv, kind);
                    return c;
                }
                let mut c = self.lower_expr(recv, cur);
                for a in args {
                    c = self.lower_expr(a, c);
                }
                c
            }
            _ => {
                let mut c = cur;
                for ch in children(e) {
                    c = self.lower_expr(ch, c);
                }
                c
            }
        }
    }
}

/// 2 つの場所の道が重なる (一方が他方の接頭辞) か。
fn overlaps(a: &[String], b: &[String]) -> bool {
    let n = a.len().min(b.len());
    a[..n] == b[..n]
}

impl<'a> Checker<'a> {
    /// 関数 `di` の本体に NLL の借用検査をかけて、E3010 を報告する。
    pub(super) fn borrowck(&mut self, di: usize) {
        let d = self.def(di);
        let Some(body) = &d.body else { return };
        // 局所変数 (引数・`let`・パターン・`for`・`match` の束縛)
        let mut locals: HashSet<String> = d.params.iter().map(|p| p.0.clone()).collect();
        let mut all = vec![];
        walk(body, &mut all);
        for n in &all {
            match nname(n) {
                "Let" | "For" => {
                    let mut b = vec![];
                    pat_binds(&nf(n)[0], &mut b);
                    locals.extend(b);
                }
                "Match" => {
                    for arm in items(&nf(n)[1]) {
                        let mut b = vec![];
                        pat_binds(&items(arm)[0], &mut b);
                        locals.extend(b);
                    }
                }
                _ => {}
            }
        }
        let mut low = Low {
            p: self.p,
            file: d.file.clone(),
            nodes: vec![],
            loans: vec![],
            locals,
            loops: vec![],
            exit: 0,
            iters: 0,
        };
        let entry = low.new_node();
        let exit = low.new_node();
        low.exit = exit;
        let end = low.lower_block(body, entry);
        low.edge(end, exit);
        let diags = analyze(&low);
        self.d = di;
        let mut seen: HashSet<(usize, String)> = HashSet::new();
        for (line, msg) in diags {
            if seen.insert((line, msg.clone())) {
                self.tdiag("E3010", msg, line);
            }
        }
    }
}

/// 生存解析と借用の保持の解析をして、競合を (行, 文面) の列で返す。
fn analyze(low: &Low) -> Vec<(usize, String)> {
    let n = low.nodes.len();
    // ---- 逆向き: 変数の生存
    let mut live_in: Vec<BTreeSet<String>> = vec![BTreeSet::new(); n];
    loop {
        let mut changed = false;
        for i in (0..n).rev() {
            let mut cur: BTreeSet<String> = BTreeSet::new();
            for &s in &low.nodes[i].succ {
                cur.extend(live_in[s].iter().cloned());
            }
            for ev in low.nodes[i].evs.iter().rev() {
                match ev {
                    Ev::Use(v) => {
                        cur.insert(v.clone());
                    }
                    Ev::Def(v) => {
                        cur.remove(v);
                    }
                    _ => {}
                }
            }
            if cur != live_in[i] {
                live_in[i] = cur;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // ---- 前向き: どの変数がどの借用を持つか
    type State = BTreeMap<String, BTreeSet<usize>>;
    let mut state_in: Vec<Option<State>> = vec![None; n];
    state_in[0] = Some(State::new());
    let mut work: VecDeque<usize> = VecDeque::new();
    work.push_back(0);
    let apply = |st_: &mut State, ev: &Ev| match ev {
        Ev::Loan { holder, id } => {
            // 同じ変数が複数の借用を持つこともある (`match` の被検査式など) ので、既存に足す。
            st_.entry(holder.clone()).or_default().insert(*id);
        }
        Ev::Alias { to, from } => {
            let src = st_.get(from).cloned().unwrap_or_default();
            st_.entry(to.clone()).or_default().extend(src);
        }
        Ev::Def(v) => {
            st_.remove(v);
        }
        _ => {}
    };
    while let Some(i) = work.pop_front() {
        let mut stt = state_in[i].clone().unwrap_or_default();
        for ev in &low.nodes[i].evs {
            apply(&mut stt, ev);
        }
        for &s in &low.nodes[i].succ {
            let changed = match &mut state_in[s] {
                None => {
                    state_in[s] = Some(stt.clone());
                    true
                }
                Some(old) => {
                    let mut ch = false;
                    for (k, v) in &stt {
                        let e = old.entry(k.clone()).or_default();
                        for x in v {
                            ch |= e.insert(*x);
                        }
                    }
                    ch
                }
            };
            if changed {
                work.push_back(s);
            }
        }
    }
    // ---- 競合の検出
    let mut out = vec![];
    for i in 0..n {
        let Some(mut stt) = state_in[i].clone() else { continue };
        let node = &low.nodes[i];
        // 各事象の直後に生きている変数
        let mut live_after: Vec<BTreeSet<String>> = vec![BTreeSet::new(); node.evs.len()];
        let mut cur: BTreeSet<String> = BTreeSet::new();
        for &s in &node.succ {
            cur.extend(live_in[s].iter().cloned());
        }
        for (k, ev) in node.evs.iter().enumerate().rev() {
            live_after[k] = cur.clone();
            match ev {
                Ev::Use(v) => {
                    cur.insert(v.clone());
                }
                Ev::Def(v) => {
                    cur.remove(v);
                }
                _ => {}
            }
        }
        for (k, ev) in node.evs.iter().enumerate() {
            if let Ev::Access { root, path, kind, line } = ev {
                for (holder, ids) in &stt {
                    if !live_after[k].contains(holder) {
                        continue;
                    }
                    for &id in ids {
                        let l = &low.loans[id];
                        if &l.root != root || !overlaps(&l.path, path) {
                            continue;
                        }
                        let who = if holder.starts_with("$iter") { "the loop iterator".to_string() } else { format!("`{}`", holder) };
                        if l.mutable {
                            out.push((*line, format!("cannot use `{}` while it is mutably borrowed by {}", root, who)));
                        } else if matches!(kind, Acc::Write | Acc::MutB) {
                            out.push((*line, format!("cannot mutate `{}` while it is borrowed by {}", root, who)));
                        }
                    }
                }
            }
            apply(&mut stt, ev);
        }
    }
    out
}
