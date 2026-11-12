//! NLL に基づく借用検査 (E3010)。関数本体を制御フローグラフの事象列に落として、
//! (1) 参照を持つ変数の生存解析 (逆向き)、(2) どの変数・場所がどの借用を持つかの前向きの解析、
//! (3) 借用が生きている間の、競合するアクセスの検出、を行う。
//!
//! 借用の生存期間は、字句的なスコープではなく「借用を持つ変数が、そのあとで使われる経路があるか」で決まる。
//! 借用の出どころは、`let r = &x` / `&mut x` (フィールドを含む)、借用を返すメソッド・関数の戻り値
//! (`let first = v.first()`、`for x in v.iter()`)、`match` の被検査式、参照を取り込むクロージャ。
//! 場所は「根の変数 + フィールドの道」で見るので、`&mut s.a` と `s.b` への代入は競合しない。
//!
//! 構造体・コンテナに入った借用 (領域の近似) も追う: `s.f = &mut x` / `v.push(&x)` のように借用が
//! 場所 (根の変数 + 道) に保存されたら、その保存場所を持ち主とするローンを記録する。保存場所の根の変数が
//! 生きている間、借りた元の場所への競合するアクセスを E3010 にする。保存場所への上書き (`s.f = ...`、
//! `v = ...`、`core.mem.drop(s.f)`) で、そこに保存されたローンは死ぬ。読み戻し (`let r = s.f`、
//! `v.first().unwrap()`、`match m.get(k) { Some(r) => .. }`) は、既存の引き継ぎと同じ仕組みでローンが移る。
//! 保存されたローンがいつまで生きるかは、実際の型が分からないので広めに近似する
//! (見落としはあっても、誤検出は出さない方針)。

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
    Loan { holder: Holder, id: usize },
    Alias { to: Holder, from: Holder },
    /// 保存場所から借用を読み戻す (`let r = s.f` など)。そこに保存されたローンは読んだ側へ移る。
    Load { to: Holder, from_root: String, from_path: Vec<String> },
    /// 保存場所から借用を写す (コピー。保存場所のローンは残す)。分解の束縛に使う。
    /// 道が重なる保存場所をすべて対象にする (タプルの要素は内部の道 `#i` に入っている)。
    Peek { to: Holder, root: String, path: Vec<String> },
    /// 場所の値を消費する (`core.mem.drop(s.f)`)。そこに保存されたローンは死ぬ。
    Kill { root: String, path: Vec<String> },
    Access { root: String, path: Vec<String>, kind: Acc, line: usize },
}

/// 借用の持ち主。変数が直接持つか、場所 (根の変数 + 道) に保存されたか。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Holder {
    Var(String),
    Place { root: String, path: Vec<String> },
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
#[derive(Clone)]
enum Src {
    /// `&x.f` / `&mut x.f` などで作る新しい借用
    Place { root: String, path: Vec<String>, mutable: bool },
    /// 参照を持つ別の変数の借用を引き継ぐ (`let r2 = r`、`&mut *r`)
    Alias(String),
    /// 場所に保存された借用を読み戻す (`let r = s.f`、`v.first().unwrap()`)
    Stored { root: String, path: Vec<String> },
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
    /// 値を持つ式 (`match` / `if`-else / ブロック式。節点のアドレス) → その値を持つ合成の持ち主 (`$val0` など)。
    /// 各枝・各腕の値の借用を、枝の終わりでこの持ち主に集める (`let r = if .. { .. } else { .. }` で `r` へ移す)。
    value_vals: HashMap<usize, String>,
}

/// 値を持つ式 (`match` の腕、`if` の枝、ブロック式) の値として外へ運ぶ借用を、どの枝から集めるか。
/// `true` は「全枝の和」(`None => &y` や `else { &y }` のような束縛を使わない枝の値も含める)。
/// 誤検出が出る場合は `false` にすると、枝のパターンの束縛をそのまま値にする腕 (`Some(rr) => rr`)
/// だけに後退する (束縛を持たない `if` / ブロックの値は、この場合どれも運ばない)。
const MATCH_VALUE_ALL_ARMS: bool = true;

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

    /// `core.mem.drop` の呼び出しか (値を受け取って消費する組み込み)。
    fn is_drop_fn(&self, segs: &[String]) -> bool {
        if segs.len() < 2 {
            return false;
        }
        let Some(r) = self.p.resolve(segs, &self.file) else { return false };
        match r.target {
            Target::Fn(i) | Target::Sig(i) => r.rest.is_empty() && self.p.defs[i].name == "core.mem.drop",
            _ => false,
        }
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
                // 再借用 `&mut *r` は r の借用を引き継ぐ。`&mut *s.f` なら、s.f に保存された借用も読み戻す。
                if nname(operand) == "Unary" && st(&nf(operand)[0]) == "*" {
                    if let Some((root, path)) = self.place_of(&nf(operand)[1]) {
                        let mut out = vec![Src::Alias(root.clone())];
                        if !path.is_empty() {
                            out.push(Src::Stored { root, path });
                        }
                        return out;
                    }
                }
                match self.place_of(operand) {
                    Some((root, path)) => vec![Src::Place { root, path, mutable: op == "&mut" }],
                    None => vec![],
                }
            }
            "Path" => {
                let segs = strs(&nf(e)[0]);
                if segs.len() == 1 && self.locals.contains(&segs[0]) {
                    // 変数そのものの引き継ぎに加えて、変数に保存された借用 (コンテナなど) も読み戻す。
                    vec![Src::Alias(segs[0].clone()), Src::Stored { root: segs[0].clone(), path: vec![] }]
                } else if segs.len() >= 2 && self.locals.contains(&segs[0]) {
                    // `s.f` は 1 つの Path (複数セグメント) として構文解析される。道の先の読み戻し。
                    vec![Src::Stored { root: segs[0].clone(), path: segs[1..].to_vec() }]
                } else {
                    vec![]
                }
            }
            "Field" | "Index" => match self.place_of(e) {
                // 場所の値を束縛に移す。保存された借用があれば、それも読み戻す。
                Some((root, path)) if !path.is_empty() || nname(e) == "Index" => {
                    vec![Src::Stored { root, path }]
                }
                _ => vec![],
            },
            "Try" => self.loan_sources(&nf(e)[0]),
            // 値を持つ式: 各枝・各腕の値の借用の和 (枝の終わりで合成の持ち主に集めてある)
            "Match" | "If" | "Block" => match self.value_vals.get(&(e as *const V as usize)) {
                Some(h) => vec![Src::Alias(h.clone())],
                None => vec![],
            },
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
                        Some(m) => vec![
                            Src::Place { root: recv_segs[0].clone(), path: recv_segs[1..].to_vec(), mutable: m },
                            Src::Stored { root: recv_segs[0].clone(), path: recv_segs[1..].to_vec() },
                        ],
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
            Some((root, path)) => {
                // レシーバを借りた新しい借用に加えて、レシーバの中に保存された借用も読み戻す
                // (`v.push(&x)` のあとの `v.first().unwrap()`)。
                vec![Src::Place { root: root.clone(), path: path.clone(), mutable: m }, Src::Stored { root, path }]
            }
            None => {
                // `v.iter().rev()` のように、借用を返す呼び出しの連なり
                if matches!(nname(recv), "MCall") { self.loan_sources(recv) } else { vec![] }
            }
        }
    }

    /// `holder` が `srcs` の借用を持つようにする事象を出す。
    fn hold(&mut self, n: usize, holder: Holder, srcs: Vec<Src>) {
        for s in srcs {
            match s {
                Src::Place { root, path, mutable } => {
                    let id = self.loans.len();
                    self.loans.push(LoanInfo { root, path, mutable });
                    self.emit(n, Ev::Loan { holder: holder.clone(), id });
                }
                Src::Alias(from) => {
                    if Holder::Var(from.clone()) != holder {
                        self.emit(n, Ev::Alias { to: holder.clone(), from: Holder::Var(from) });
                    }
                }
                Src::Stored { root, path } => {
                    self.emit(n, Ev::Load { to: holder.clone(), from_root: root, from_path: path });
                }
            }
        }
    }

    /// 分解 (`let (a, b) = e` / `match e { (a, b) => .. }`) の束縛に、右辺の借用を伝える。
    /// タプルの要素は内部の道 `#i` (`store_value`)、構造体のフィールドは名前の道に保存されているので、
    /// 部分パターンごとに対応する要素・フィールドの借用だけを持たせる (コピー値の要素は黙る)。
    /// 型や出どころが分からない所は、右辺全体の借用を束縛すべてに持たせる (保守的な和)。
    fn hold_pat(&mut self, n: usize, pat: &V, e: &V) {
        match nname(pat) {
            "PTuple" => {
                let subs = items(&nf(pat)[0]);
                if nname(e) == "TupleE" {
                    let elems = items(&nf(e)[0]);
                    if elems.len() == subs.len() {
                        for (s, x) in subs.iter().zip(elems.iter()) {
                            self.hold_pat(n, s, x);
                        }
                        return;
                    }
                }
                if let Some((root, path)) = self.place_of(e) {
                    for (i, s) in subs.iter().enumerate() {
                        let mut p = path.clone();
                        p.push(format!("#{}", i));
                        self.hold_place(n, s, &root, &p);
                    }
                    return;
                }
            }
            "PStruct" => {
                let fields = items(&nf(pat)[1]);
                if nname(e) == "StructLit" {
                    for fl in fields {
                        let fp = items(fl);
                        if fp.len() < 2 {
                            continue;
                        }
                        let fname = st(&fp[0]);
                        let val = items(&nf(e)[1]).iter().find(|x| {
                            let it = items(x);
                            it.len() >= 2 && st(&it[0]) == fname
                        });
                        if let Some(x) = val {
                            self.hold_pat(n, &fp[1], &items(x)[1]);
                        }
                    }
                    return;
                }
                if let Some((root, path)) = self.place_of(e) {
                    for fl in fields {
                        let fp = items(fl);
                        if fp.len() < 2 {
                            continue;
                        }
                        let mut p = path.clone();
                        p.push(st(&fp[0]));
                        self.hold_place(n, &fp[1], &root, &p);
                    }
                    return;
                }
            }
            _ => {}
        }
        self.hold_whole(n, pat, e);
    }

    /// 保存場所 `root` + `path` の値を分解した束縛 (部分パターン `pat`) に、その場所の借用を写す。
    fn hold_place(&mut self, n: usize, pat: &V, root: &str, path: &[String]) {
        match nname(pat) {
            "PTuple" => {
                for (i, s) in items(&nf(pat)[0]).iter().enumerate() {
                    let mut p = path.to_vec();
                    p.push(format!("#{}", i));
                    self.hold_place(n, s, root, &p);
                }
            }
            "PStruct" => {
                for fl in items(&nf(pat)[1]) {
                    let fp = items(fl);
                    if fp.len() < 2 {
                        continue;
                    }
                    let mut p = path.to_vec();
                    p.push(st(&fp[0]));
                    self.hold_place(n, &fp[1], root, &p);
                }
            }
            _ => {
                let mut binds = vec![];
                pat_binds(pat, &mut binds);
                for b in binds {
                    self.emit(n, Ev::Alias { to: Holder::Var(b.clone()), from: Holder::Var(root.to_string()) });
                    self.emit(n, Ev::Peek { to: Holder::Var(b), root: root.to_string(), path: path.to_vec() });
                }
            }
        }
    }

    /// 分解の部分パターン `pat` (要素・フィールドの道が分からない形) の束縛に、右辺 `e` の借用の和を伝える。
    fn hold_whole(&mut self, n: usize, pat: &V, e: &V) {
        let mut srcs = self.loan_sources(e);
        srcs.extend(self.lit_sources(e));
        let mut binds = vec![];
        pat_binds(pat, &mut binds);
        for b in binds {
            for s in srcs.iter().cloned() {
                match s {
                    Src::Stored { root, path } => self.emit(n, Ev::Peek { to: Holder::Var(b.clone()), root, path }),
                    other => self.hold(n, Holder::Var(b.clone()), vec![other]),
                }
            }
        }
    }

    /// 構造体リテラルの各フィールドに入る借用を、`root` の `base` 道の先に保存された
    /// ローンとして記録する (`let s = t.S { f: &mut x }`、`s.f = t.S { g: &x }`)。
    fn store_struct_lit(&mut self, n: usize, root: &str, base: &[String], lit: &V) {
        for fld in items(&nf(lit)[1]) {
            let it = items(fld);
            if it.len() < 2 {
                continue;
            }
            let mut p = base.to_vec();
            p.push(st(&it[0]));
            if nname(&it[1]) == "StructLit" {
                let q = p.clone();
                self.store_struct_lit(n, root, &q, &it[1]);
                continue;
            }
            self.store_value(n, root, &p, &it[1]);
        }
    }

    /// タプル・配列リテラル (`(&x, 1)`、`[&x, &y]`、`[&x; 3]`) の要素に入る借用。入れ子のリテラル
    /// (構造体リテラルを含む) の中も集める。要素ごとの道は持たず、リテラル全体の場所に保存する
    /// (要素への上書きは全体を殺す — 広めに殺す方針)。リテラルでなければ空。
    fn lit_sources(&self, e: &V) -> Vec<Src> {
        let elems: Vec<&V> = match nname(e) {
            "TupleE" | "ArrayE" => items(&nf(e)[0]).iter().collect(),
            "ArrayRep" => vec![&nf(e)[0]],
            "StructLit" => items(&nf(e)[1]).iter().filter_map(|fld| items(fld).get(1)).collect(),
            _ => return vec![],
        };
        let mut out = vec![];
        for x in elems {
            if matches!(nname(x), "TupleE" | "ArrayE" | "ArrayRep" | "StructLit") {
                out.extend(self.lit_sources(x));
            } else {
                out.extend(self.loan_sources(x));
            }
        }
        out
    }

    /// 値の式 `e` に入る借用を、場所 `root` + `path` に保存されたローンとして記録する。
    /// 構造体リテラルはフィールドの道ごとに、タプル・配列リテラルは全体の場所に置く。
    /// それ以外は値の式そのものの出どころ (`&x`、参照の変数など)。
    fn store_value(&mut self, n: usize, root: &str, path: &[String], e: &V) {
        match nname(e) {
            "StructLit" => {
                self.store_struct_lit(n, root, path, e);
                return;
            }
            // タプルの要素は内部の道 `#i` に置く (分解のとき要素ごとに読めるように)。
            "TupleE" => {
                for (i, x) in items(&nf(e)[0]).iter().enumerate() {
                    let mut p = path.to_vec();
                    p.push(format!("#{}", i));
                    self.store_value(n, root, &p, x);
                }
                return;
            }
            _ => {}
        }
        let mut srcs = self.loan_sources(e);
        if srcs.is_empty() {
            srcs = self.lit_sources(e);
        }
        if !srcs.is_empty() {
            self.hold(n, Holder::Place { root: root.to_string(), path: path.to_vec() }, srcs);
        }
    }

    /// `match` の腕の値 (本体が式ならその式、ブロックなら最後の式文)。値を持たない腕は `None`。
    fn arm_tail(body: &V) -> Option<&V> {
        match nname(body) {
            "Block" => {
                let last = items(&nf(body)[0]).last()?;
                match nname(last) {
                    "ExprStmt" => Self::arm_tail(&nf(last)[0]),
                    "Let" | "Assign" => None,
                    _ => Self::arm_tail(last),
                }
            }
            _ => Some(body),
        }
    }

    /// 値を持つ式 `e` (`match` / `if`-else / ブロック式) の持ち主を作って登録する。
    fn value_holder(&mut self, e: &V) -> String {
        let holder = format!("$val{}", self.value_vals.len());
        self.value_vals.insert(e as *const V as usize, holder.clone());
        holder
    }

    /// 枝の値 `body` が運ぶ借用の出どころ。`Some(rr) => rr` なら腕の束縛 (= 被検査式のローン源を引き継いだもの)、
    /// `None => &y` / `else { &y }` なら新しい借用。`MATCH_VALUE_ALL_ARMS` が偽なら、
    /// 枝の束縛 `binds` をそのまま値にする枝 (`Some(rr) => rr`) だけ。
    fn value_sources(&self, binds: &[String], body: &V) -> Vec<Src> {
        let Some(tail) = Self::arm_tail(body) else { return vec![] };
        if !MATCH_VALUE_ALL_ARMS {
            let is_bind = nname(tail) == "Path" && {
                let segs = strs(&nf(tail)[0]);
                segs.len() == 1 && binds.contains(&segs[0])
            };
            if !is_bind {
                return vec![];
            }
        }
        self.loan_sources(tail)
    }

    /// 枝の終わり `end` で、枝の値の借用を合成の持ち主 `holder` に集める。
    /// `Def` で前の値 (ループの前の周回など) を捨ててから持たせる。合流点では全枝の和になる。
    fn collect_value(&mut self, end: usize, holder: &str, binds: &[String], body: &V) {
        let srcs = self.value_sources(binds, body);
        self.emit(end, Ev::Def(holder.to_string()));
        self.hold(end, Holder::Var(holder.to_string()), srcs);
    }

    /// `push` / `insert` など、コンテナに値を入れるメソッド。値の式に入る借用を、
    /// レシーバの場所 (根の変数 + 道) に保存されたローンとして記録する。
    fn store_into(&mut self, n: usize, name: &str, root: &str, path: &[String], args: &[V]) {
        let slot = match name {
            "push" | "push_back" | "push_front" => 0,
            "insert" => 1,
            _ => return,
        };
        let Some(a) = args.get(slot) else { return };
        if nname(a) == "StructLit" {
            // 構造体リテラルを丸ごと入れる形は、要素の道を持たないので全体に置く。
            let srcs = self.lit_sources(a);
            if !srcs.is_empty() {
                self.hold(n, Holder::Place { root: root.to_string(), path: path.to_vec() }, srcs);
            }
            return;
        }
        self.store_value(n, root, path, a);
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
                        self.hold(c, Holder::Var(b.clone()), srcs2);
                    }
                }
                // 構造体リテラルのフィールドに入る借用は、保存場所 (変数 + フィールド道) に記録する。
                // 変数に直接持たせないのは、`s.f = ...` の上書きでローンを殺せるようにするため。
                // タプル・配列リテラルの要素に入る借用は、変数全体の場所に記録する (`let t = (&x, 1)`)。
                if matches!(nname(init), "StructLit" | "TupleE" | "ArrayE" | "ArrayRep") && nname(&f[0]) == "PBind" {
                    if let Some(b) = binds.first() {
                        self.store_value(c, b, &[], init);
                    }
                }
                // 分解の束縛 (`let (a, b) = t`) は、右辺の要素・フィールドの借用 (保存されたローンを含む) を伝える。
                if matches!(nname(&f[0]), "PTuple" | "PStruct") && !is_nil(init) {
                    self.hold_pat(c, &f[0], init);
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
                    self.hold(c, Holder::Var(name.clone()), srcs);
                    // 値の代入そのものも、その変数への書き込み
                    self.emit(c, Ev::Access { root: name.clone(), path: vec![], kind: Acc::Write, line: nline(s) });
                    // `s = t.S { f: &mut y }` のように、構造体ごと新しい借用を保存することもある。
                    if matches!(nname(&f[1]), "StructLit" | "TupleE" | "ArrayE" | "ArrayRep") {
                        self.store_value(c, &name, &[], &f[1]);
                    }
                    return c;
                }
                // `*r = v` は r を使うだけ。`s.f = v` / `v[i] = x` は根の変数への書き込み。
                if nname(target) == "Unary" && st(&nf(target)[0]) == "*" {
                    return self.lower_expr(&nf(target)[1], c);
                }
                let c = self.lower_index_parts(target, c);
                if let Some((root, path)) = self.place_of(target) {
                    self.emit(c, Ev::Use(root.clone()));
                    self.emit(c, Ev::Access { root: root.clone(), path: path.clone(), kind: Acc::Write, line: nline(s) });
                    // 借用を場所に保存する (`s.f = &mut x` / `v[i] = &x`)。上書きなので、
                    // 前に保存されていたローンは上の Access (書き込み) で死ぬ。
                    self.store_value(c, &root, &path, &f[1]);
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
                // `else` のある `if` は値を持つ: 両枝の末尾の値の借用を、枝の終わりで合成の持ち主に集める
                let holder = if is_nil(&f[2]) { None } else { Some(self.value_holder(e)) };
                let t_end = self.lower_block(&f[1], then_n);
                if let Some(h) = &holder {
                    self.collect_value(t_end, h, &[], &f[1]);
                }
                self.edge(t_end, join);
                let e_end = if is_nil(&f[2]) { else_n } else { self.lower_block(&f[2], else_n) };
                if let Some(h) = &holder {
                    self.collect_value(e_end, h, &[], &f[2]);
                }
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
                self.hold(c, Holder::Var(holder.clone()), srcs);
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
                    self.hold(body, Holder::Var(b.clone()), srcs);
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
                let holder = self.value_holder(e);
                for arm in arms {
                    let a = items(arm);
                    let an = self.new_node();
                    self.edge(c, an);
                    let mut binds = vec![];
                    pat_binds(&a[0], &mut binds);
                    if matches!(nname(&a[0]), "PTuple" | "PStruct") {
                        // 分解の腕は、被検査式の借用 (保存されたローンを含む) を束縛全体に伝える。
                        for b in &binds {
                            self.emit(an, Ev::Def(b.clone()));
                        }
                        self.hold_pat(an, &a[0], &f[0]);
                    } else {
                        for b in &binds {
                            self.emit(an, Ev::Def(b.clone()));
                            let srcs = self.loan_sources(&f[0]);
                            self.hold(an, Holder::Var(b.clone()), srcs);
                        }
                    }
                    let end = self.lower_block(&a[1], an);
                    self.collect_value(end, &holder, &binds, &a[1]);
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
            "Block" => {
                // ブロック式の値: 末尾の式の借用を、ブロックの終わりで合成の持ち主に集める
                let holder = self.value_holder(e);
                let end = self.lower_block(e, cur);
                self.collect_value(end, &holder, &[], e);
                end
            }
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
                        self.emit(c, Ev::Access { root: root.clone(), path: path.clone(), kind, line: nline(e) });
                        self.store_into(c, &name, &root, &path, args);
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
                // `core.mem.drop(p)`: 場所の値を消費する。そこに保存された借用はここで死ぬ。
                if nname(callee) == "Path" && self.is_drop_fn(&strs(&nf(callee)[0])) {
                    if let Some(arg) = args.first() {
                        if let Some((root, path)) = self.place_of(arg) {
                            self.emit(c, Ev::Kill { root, path });
                        }
                    }
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
                    // `v.push(&x)` / `m.insert(k, &x)`: レシーバの中に借用を保存する。
                    if let Some((root, path)) = self.place_of(recv) {
                        self.store_into(c, &name, &root, &path, args);
                    }
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
            value_vals: HashMap::new(),
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
    // ---- 前向き: どの変数・場所がどの借用を持つか
    type State = BTreeMap<Holder, BTreeSet<usize>>;
    let mut state_in: Vec<Option<State>> = vec![None; n];
    state_in[0] = Some(State::new());
    let mut work: VecDeque<usize> = VecDeque::new();
    work.push_back(0);
    let apply = |st_: &mut State, ev: &Ev| match ev {
        Ev::Loan { holder, id } => {
            // 同じ変数・場所が複数の借用を持つこともある (`match` の被検査式など) ので、既存に足す。
            st_.entry(holder.clone()).or_default().insert(*id);
        }
        Ev::Alias { to, from } => {
            let src = st_.get(from).cloned().unwrap_or_default();
            st_.entry(to.clone()).or_default().extend(src);
        }
        Ev::Load { to, from_root, from_path } => {
            // 保存場所から読み戻す。保存されていたローンは読んだ側へ移る (move の近似。
            // 実際はコピーでも、消えるのは見落としでしかない)。道が重なる保存場所 (タプルの要素の道 `#i` を含む)
            // はすべて対象にする。
            for src in take_overlapping(st_, from_root, from_path) {
                st_.entry(to.clone()).or_default().extend(src);
            }
        }
        Ev::Peek { to, root, path } => {
            // 保存場所の借用を写す (保存場所のローンは残す)。
            let mut acc: BTreeSet<usize> = BTreeSet::new();
            for (h, ids) in st_.iter() {
                if let Holder::Place { root: r, path: p } = h {
                    if r == root && overlaps(p, path) {
                        acc.extend(ids.iter().copied());
                    }
                }
            }
            if !acc.is_empty() {
                st_.entry(to.clone()).or_default().extend(acc);
            }
        }
        Ev::Kill { root, path } => kill_stored(st_, root, path),
        Ev::Def(v) => {
            st_.remove(&Holder::Var(v.clone()));
            // 根の変数の再代入 (`v = ...`) で、その中に保存されたローンはすべて死ぬ。
            let dead: Vec<Holder> = st_
                .keys()
                .filter(|h| matches!(h, Holder::Place { root, .. } if root == v))
                .cloned()
                .collect();
            for h in dead {
                st_.remove(&h);
            }
        }
        Ev::Access { root, path, kind, .. } => {
            // 場所への上書き・`&mut` で、そこに保存されたローンは死ぬ。型が分からないので広めに殺す:
            // `v.push` のような要素を残す操作でも一度殺す (見落としはあっても、誤検出は出さない)。
            if matches!(kind, Acc::Write | Acc::MutB) {
                kill_stored(st_, root, path);
            }
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
                    // 変数のローンは変数が、保存場所のローンは根の変数が生きている間だけ競合する。
                    let live = match holder {
                        Holder::Var(v) => live_after[k].contains(v),
                        Holder::Place { root: r, .. } => live_after[k].contains(r),
                    };
                    if !live {
                        continue;
                    }
                    for &id in ids {
                        let l = &low.loans[id];
                        if &l.root != root || !overlaps(&l.path, path) {
                            continue;
                        }
                        let who = match holder {
                            Holder::Var(v) if v.starts_with("$iter") => "the loop iterator".to_string(),
                            Holder::Var(v) => format!("`{}`", v),
                            Holder::Place { root: r, path: p } => format!("`{}`", place_name(r, p)),
                        };
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

/// 場所 (root + path) への上書きで死ぬ保存ローンを取り除く。道が重なる (どちらかが他方の接頭辞の)
/// 保存場所をすべて殺す。`v = ...` のような根の再代入は `Def` が、`v[0] = ...` は `Access` が来る。
fn kill_stored(st: &mut BTreeMap<Holder, BTreeSet<usize>>, root: &str, path: &[String]) {
    let dead: Vec<Holder> = st
        .keys()
        .filter(|h| match h {
            Holder::Place { root: r, path: p } => r == root && overlaps(p, path),
            _ => false,
        })
        .cloned()
        .collect();
    for h in dead {
        st.remove(&h);
    }
}

/// 道が重なる保存場所 (`root` の下) をすべて取り除き、そこに保存されていたローンの集合を返す (読み戻し用)。
fn take_overlapping(st: &mut BTreeMap<Holder, BTreeSet<usize>>, root: &str, path: &[String]) -> Vec<BTreeSet<usize>> {
    let keys: Vec<Holder> = st
        .keys()
        .filter(|h| match h {
            Holder::Place { root: r, path: p } => r == root && overlaps(p, path),
            _ => false,
        })
        .cloned()
        .collect();
    keys.iter().filter_map(|h| st.remove(h)).collect()
}

/// 場所の表示 (`s.f`、コンテナなら `v`)。タプルの要素の内部の道 `#i` は表示しない。
fn place_name(root: &str, path: &[String]) -> String {
    let mut s = root.to_string();
    for p in path.iter().filter(|p| !p.starts_with('#')) {
        s.push('.');
        s.push_str(p);
    }
    s
}
