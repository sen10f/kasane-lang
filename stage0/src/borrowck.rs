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
//!
//! 1 つの式の中の借用 (構造体・タプル・配列リテラル、呼び出しの引数) は、左から右に評価して、式の最後まで
//! 生きる一時の持ち主に持たせる (`hold_temp`)。あとのオペランドの競合するアクセスが E3010 になる。メソッドの
//! レシーバは引数のあとに触れる (2 相借用)。クロージャは取り込んだ借用を持ち、本体は別に検査する
//! (`&x` の取り込みは本体の間ずっと共有借用)。関数の戻り値へ流れる引数は、ヘッダのライフタイムと省略規則
//! で決める (`flow_params`)。`core.mem.replace` / `swap` と `push` / `insert` は、場所が持つローンを殺さず足す。

use super::*;
use std::collections::{BTreeSet, HashMap, VecDeque};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Acc {
    Read,
    Shared,
    MutB,
    /// コンテナに値を入れる `&mut self` メソッド (`push` / `insert`)。可変借用として競合するが、
    /// すでに保存されたローンは殺さない (コンテナの型の領域は、入れたすべての借用が生きる間続く)。
    MutKeep,
    Write,
    /// 値としての消費 (move)。共有借用とも競合する。
    Move,
}

#[derive(Clone, Debug)]
enum Ev {
    Use(String),
    Def(String),
    Loan { holder: Holder, id: usize },
    Alias { to: Holder, from: Holder },
    /// 保存場所から借用を読み戻す (`let r = s.f` など)。そこに保存されたローンは読んだ側へ移る。
    Load { to: Holder, from_root: String, from_path: Vec<String> },
    /// `core.mem.swap`: 2 つの場所 (とその中) に保存されたローンを、互いに足し合う。
    Mix { a: (String, Vec<String>), b: (String, Vec<String>) },
    /// 保存場所から借用を写す (コピー。保存場所のローンは残す)。分解の束縛に使う。
    /// 道が重なる保存場所をすべて対象にする (タプルの要素は内部の道 `#i` に入っている)。
    Peek { to: Holder, root: String, path: Vec<String> },
    /// 場所の値を消費する (`core.mem.drop(s.f)`)。そこに保存されたローンは死ぬ。
    Kill { root: String, path: Vec<String> },
    /// ブロックで局所変数が宣言され、スコープに入る。
    ScopeAdd(String),
    /// ブロックの末尾で、そのブロックで宣言した局所変数の寿命が終わる。
    DropScope(String),
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
    loops: Vec<(usize, usize, usize)>,
    exit: usize,
    iters: usize,
    /// クロージャ本体を別に検査して出た診断 (行, 文面)。
    sub: Vec<(usize, String)>,
    /// 値を持つ式 (`match` / `if`-else / ブロック式。節点のアドレス) → その値を持つ合成の持ち主 (`$val0` など)。
    /// 各枝・各腕の値の借用を、枝の終わりでこの持ち主に集める (`let r = if .. { .. } else { .. }` で `r` へ移す)。
    value_vals: HashMap<usize, String>,
    /// 型検査が move と判定した場所 (式の位置, 場所の鍵)。`Acc::Move` に変える。
    moves: HashSet<(usize, String)>,
    /// ブロックごとの局所変数スタック。ブロックの末尾でそのブロックの変数の寿命が終わる。
    scope_stack: Vec<Vec<String>>,
    /// 関数の仮引数。スコープ解析の初期値 (関数の間ずっと生きている)。
    scope_seed: Vec<String>,
    /// `let x = &<一時値>` / `match &<一時値> { .. }` で、一時値の寿命が束縛先のスコープまで
    /// 延びる変数名。`$temp` ルールで寿命切れと誤検出しないため。
    temp_extended: HashSet<String>,
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

    /// `&<一時値>` / `&mut <一時値>` (例: `&(3i32 + 4i32)`、`&Some(x)`)。一時値への直接の
    /// 参照で、寿命が束縛先のスコープまで延びる。
    fn is_direct_temp_ref(&self, e: &V) -> bool {
        if nname(e) != "Unary" {
            return false;
        }
        let f = nf(e);
        let op = st(&f[0]);
        (op == "&" || op == "&mut") && self.place_of(&f[1]).is_none()
    }

    /// 式 `e` の評価で一時値 (`$temp` への貸し出し) が作られるか。
    /// `match` / `for` / `if` / `while` の対象式の中の `$temp` は、その文の終わりまで
    /// 生きる (仕様 4.2)。
    fn has_temp(&self, e: &V) -> bool {
        self.loan_sources(e).iter().any(|s| matches!(s, Src::Place { root, .. } if root == "$temp"))
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

    /// 戻り値が参照を含むか。`ret_borrows` に加え、戻り値の型定義の欄が参照を持つ構造体
    /// (`t.H { r: &'a T }` など) も含む。省略規則で、戻り値は受け手・引数を借りる。
    fn ret_contains_ref(&self, d: &Def) -> bool {
        if Self::ret_borrows(d) {
            return true;
        }
        let V::L(parts) = &d.ret else { return false };
        let [V::S(a), V::S(name), _] = parts.as_slice() else { return false };
        if a != "named" { return false; }
        let segs: Vec<String> = name.split('.').map(|s| s.to_string()).collect();
        let Some(r) = self.p.resolve(&segs, &self.file) else { return false };
        let Target::Type(di) = r.target else { return false };
        let td = &self.p.defs[di];
        // 欄の参照が `&'static` (ライフタイムを明示し、構造体にライフタイム・ジェネリクを持たず、
        // エリジードでもない) なら、戻り値は呼び出し元を借りない。
        let life_generic = td.generics.iter().any(|g| g.0 == "life");
        td.fields.iter().any(|(name, ty, _)| {
            Self::ty_contains_ref(ty) && (td.elided_refs.contains(name) || life_generic)
        })
    }

    /// 型表現 `ty` が、どこかで参照 (`&T` / `&mut T`) を含むか。
    fn ty_contains_ref(ty: &V) -> bool {
        match ty {
            V::L(xs) => {
                if xs.first().map_or(false, |s| matches!(s, V::S(n) if n == "ref")) {
                    return true;
                }
                xs.iter().any(Self::ty_contains_ref)
            }
            _ => false,
        }
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
                    Some((root, path)) => {
                        // `&r` (r が参照を持つ変数) は、r を通して r の借用元も借りる (参照の参照の推移)。
                        let mut out = vec![];
                        if path.is_empty() && self.locals.contains(&root) {
                            out.push(Src::Alias(root.clone()));
                        }
                        out.push(Src::Place { root, path, mutable: op == "&mut" });
                        out
                    }
                    // `&t.mk()` のように、一時値への参照。一時値は文の終わりで死ぬので、
                    // その後の使用は dangling になる。`$temp` はスコープに登録されないため
                    // 常に「寿命切れ」と見なされる。
                    // ただし `&5u32` / `&-1i32` / `&t.LIMIT` (リテラルと、局所でないパス = `const` / `static` / 関数) は
                    // `'static` の値への共有参照 (定数の昇格。master と同じく借用を作らない。Fable の最終レビュー p02 / p10)。
                    None if op == "&" && is_static_operand(operand) => vec![],
                    None => vec![Src::Place { root: "$temp".into(), path: vec![], mutable: op == "&mut" }],
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
                self.method_sources(recv, &name, items(&nf(e)[2]))
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
                        Some(m) => {
                            let mut out = vec![
                                Src::Place { root: recv_segs[0].clone(), path: recv_segs[1..].to_vec(), mutable: m },
                                Src::Stored { root: recv_segs[0].clone(), path: recv_segs[1..].to_vec() },
                            ];
                            // レシーバの変数が持つ借用 (`let w = mk(&mut x); w.get()`) も結果へ流れる。
                            out.push(Src::Alias(recv_segs[0].clone()));
                            out.extend(self.method_arg_sources(&name, items(&nf(e)[1])));
                            out
                        }
                        None => vec![],
                    };
                }
                // 関数の戻り値が借用を含むなら、借用の引数 (`&x` / `&mut x` / 参照の変数) から借りる
                let Some(r) = self.p.resolve(&segs, &self.file) else { return vec![] };
                let di = match r.target {
                    Target::Fn(i) | Target::Sig(i) => i,
                    // `Some(&x)` / `Pair(&a, &b)`: 構築した値が引数の借用を持つ。
                    Target::Variant(..) | Target::Type(_) if r.rest.is_empty() => {
                        return items(&nf(e)[1]).iter().flat_map(|a| self.loan_sources(a)).collect();
                    }
                    _ => return vec![],
                };
                if !r.rest.is_empty() || !self.ret_contains_ref(&self.p.defs[di]) {
                    return vec![];
                }
                // ヘッダのライフタイム (省略規則を含む) で、戻り値へ流れる引数だけを取る。読めなければ全部。
                let flow = Self::flow_params(&self.p.defs[di]);
                items(&nf(e)[1])
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| flow.as_ref().map_or(true, |f| f.get(*i).copied().unwrap_or(false)))
                    .flat_map(|(_, a)| self.loan_sources(a))
                    .collect()
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

    fn method_sources(&self, recv: &V, name: &str, args: &[V]) -> Vec<Src> {
        let Some(m) = self.method_borrows(name) else { return vec![] };
        let mut out = match self.place_of(recv) {
            Some((root, path)) => {
                // レシーバを借りた新しい借用に加えて、レシーバの中に保存された借用も読み戻す
                // (`v.push(&x)` のあとの `v.first().unwrap()`)。レシーバの変数が持つ借用も結果へ流れる。
                vec![
                    Src::Place { root: root.clone(), path: path.clone(), mutable: m },
                    Src::Stored { root: root.clone(), path },
                    Src::Alias(root),
                ]
            }
            None => {
                // `v.iter().rev()` や `xs.as_slice().get(0)` のように、借用を返す呼び出しの連なり。
                // レシーバが `v.mk()` (Call 形) の連なりも辿り、連鎖の根の変数を借用元にする。
                if matches!(nname(recv), "MCall" | "Call") { self.loan_sources(recv) } else { vec![] }
            }
        };
        out.extend(self.method_arg_sources(name, args));
        out
    }

    /// メソッドの `self` 以外の引数のうち、戻り値へ流れるもの (すべての候補のヘッダが一致して流すとき) の借用。
    fn method_arg_sources(&self, name: &str, args: &[V]) -> Vec<Src> {
        let Some(cands) = self.p.method_idx.get(name) else { return vec![] };
        let mut flow: Vec<bool> = vec![true; args.len()];
        for &i in cands {
            let d = &self.p.defs[i];
            let f = Self::flow_params(d);
            for (k, slot) in flow.iter_mut().enumerate() {
                *slot &= d.params.len() == args.len() + 1 && f.as_ref().map_or(false, |f| f.get(k + 1).copied().unwrap_or(false));
            }
        }
        args.iter().zip(flow).filter(|(_, f)| *f).flat_map(|(a, _)| self.loan_sources(a)).collect()
    }

    /// ヘッダの文面 (`fn f['a, T](x: &'a T, y: &T) -> &'a T`) から、各引数の借用が戻り値へ流れるかを求める。
    /// 戻り値に名前付きライフタイムがあれば、それを持つ引数が流れる。なければ省略規則 (4.1): 第 1 引数が
    /// `self: &..` ならそれだけ、そうでなければ参照を持つ引数 (ちょうど 1 つのはず)。読めなければ None。
    fn flow_params(d: &Def) -> Option<Vec<bool>> {
        let head = d.header.split(" uses ").next()?;
        let b = head.as_bytes();
        let mut i = head.find(|c| c == '(' || c == '[')?;
        if b[i] == b'[' {
            i = match_close(head, i)? + 1;
        }
        if b.get(i) != Some(&b'(') {
            return None;
        }
        let close = match_close(head, i)?;
        let params = split_top(&head[i + 1..close]);
        if params.len() != d.params.len() {
            return None;
        }
        let ret = head[close + 1..].split_once("->").map_or("", |x| x.1);
        let ret_lts = lifetimes(ret);
        let flows = if !ret_lts.is_empty() {
            params.iter().map(|p| lifetimes(p).iter().any(|l| ret_lts.contains(l))).collect()
        } else if d.params.first().map_or(false, |p| p.0 == "self" && tag(&p.1) == "ref") {
            (0..params.len()).map(|k| k == 0).collect()
        } else {
            params.iter().map(|p| p.contains('&') || p.contains('\'')).collect()
        };
        Some(flows)
    }

    /// `fn keep['a](v: &mut Vec<&'a String>, s: &'a String) -> &'a String` のように、
    /// ヘッダの名前付きライフタイム `'a` が、`&mut X<..'a..>` 型の引数 (受け手 R) と
    /// 参照の引数 (借用元 S) の両方に現れるとき、S の借用を R の場所に結びつけ、
    /// R の最後の使用まで生かす。`v` が生きている間は `s` を move できない (E3010)。
    /// 戻り値への借用追跡 (flow_params / ret_contains_ref) と同じ仕組み。
    fn bind_receiver_arg_borrow(&mut self, c: usize, di: usize, args: &[V]) {
        let d = &self.p.defs[di];
        let Some(flow) = Self::flow_params(d) else { return };
        let lts: Vec<String> = d
            .generics
            .iter()
            .filter(|g| g.0 == "life")
            .map(|g| g.1.trim_start_matches('\'').to_string())
            .collect();
        if lts.is_empty() {
            return;
        }
        // ヘッダの仮引数の文面からライフタイムを取る (ty.dump() にはライフタイムが含まれない)。
        let head = match d.header.split(" uses ").next() { Some(h) => h, None => return };
        let b = head.as_bytes();
        let mut i = match head.find(|c| c == '(' || c == '[') { Some(i) => i, None => return };
        if b[i] == b'[' {
            i = match match_close(head, i) { Some(j) => j + 1, None => return };
        }
        if b.get(i) != Some(&b'(') {
            return;
        }
        let close = match match_close(head, i) { Some(j) => j, None => return };
        let ptexts: Vec<String> = split_top(&head[i + 1..close]);
        if ptexts.len() != d.params.len() { return; }

        let mut receivers: Vec<usize> = vec![];
        let mut sources: Vec<usize> = vec![];
        for (k, ((_, ty), &f)) in d.params.iter().zip(flow.iter()).enumerate() {
            let ltys = lifetimes(&ptexts[k]);
            if !f || tag(ty) != "ref" {
                continue;
            }
            if !lts.iter().any(|l| ltys.contains(l)) {
                continue;
            }
            if matches!(tel(ty, 1), V::B(true)) {
                receivers.push(k);
            } else {
                sources.push(k);
            }
        }
        if receivers.is_empty() || sources.is_empty() {
            return;
        }
        for &r in &receivers {
            let Some(a) = args.get(r) else { continue };
            let Some((rroot, _)) = self.arg_place(a) else { continue };
            for &s in &sources {
                let Some(b) = args.get(s) else { continue };
                let Some(sroot) = self.borrow_root(b) else { continue };
                if sroot == rroot {
                    continue;
                }
                let mutable = matches!(tel(&d.params[s].1, 1), V::B(true));
                let id = self.loans.len();
                self.loans.push(LoanInfo {
                    root: sroot.clone(),
                    path: vec![],
                    mutable,
                });
                self.emit(c, Ev::Loan {
                    holder: Holder::Var(rroot.clone()),
                    id,
                });
            }
        }
    }

    /// 式 `a` が `&<場所>` / `&mut <場所>` ならその場所の (根, 道)。
    fn arg_place(&self, a: &V) -> Option<(String, Vec<String>)> {
        match nname(a) {
            "Unary" => {
                let op = st(&nf(a)[0]);
                if op == "&" || op == "&mut" {
                    self.place_of(&nf(a)[1])
                } else {
                    None
                }
            }
            _ => self.place_of(a),
        }
    }

    /// `&<変数>` の借用元の変数名。それ以外は None。
    fn borrow_root(&self, a: &V) -> Option<String> {
        if nname(a) != "Unary" {
            return None;
        }
        let f = nf(a);
        let op = st(&f[0]);
        if op != "&" && op != "&mut" {
            return None;
        }
        let (root, path) = self.place_of(&f[1])?;
        if path.is_empty() {
            Some(root)
        } else {
            None
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
    /// 道が分からない形 (関数の戻り値などを分解する `let (a, n) = t.f(&x)`) は何もしない (`false` を返す。
    /// 呼び出し側が従来の読み戻しに任せる)。右辺全体の借用を全束縛に配ることはしない (誤検出を出さない)。
    fn hold_pat(&mut self, n: usize, pat: &V, e: &V) -> bool {
        match nname(pat) {
            "PTuple" => {
                let subs = items(&nf(pat)[0]);
                if nname(e) == "TupleE" {
                    let elems = items(&nf(e)[0]);
                    if elems.len() == subs.len() {
                        for (s, x) in subs.iter().zip(elems.iter()) {
                            self.hold_pat(n, s, x);
                        }
                        return true;
                    }
                }
                if let Some((root, path)) = self.place_of(e) {
                    for (i, s) in subs.iter().enumerate() {
                        let mut p = path.clone();
                        p.push(format!("#{}", i));
                        self.hold_place(n, s, &root, &p);
                    }
                    return true;
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
                    return true;
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
                    return true;
                }
            }
            _ => {}
        }
        // 部分パターンが分解でない葉 (`a`・`_`・バリアントなど) は、与えられた式の借用をその束縛に伝える。
        // 分解の形が分からない複合パターンは何もしない。
        if matches!(nname(pat), "PTuple" | "PStruct") {
            return false;
        }
        self.hold_whole(n, pat, e);
        true
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
                // 道の先の要素だけを写す。根の変数が直接持つローン (`let p = f(&x)` の p) は、
                // 要素の道が分からないので束縛へ配らない (コピー値の要素に借用を付けて誤検出しないため)。
                for b in binds {
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
        // このブロックで宣言する局所変数のスコープレベルを積む。末尾で寿命が終わる。
        self.scope_stack.push(vec![]);
        for s in items(&nf(b)[0]) {
            cur = self.lower_stmt(s, cur);
        }
        // ブロックの末尾で、このブロックで宣言した変数のスコープを抜ける。
        let vars = self.scope_stack.pop().unwrap_or_default();
        for v in vars {
            self.emit(cur, Ev::DropScope(v));
        }
        cur
    }

    /// `break` / `continue` で抜けるブロック (スコープ深さ `depth` 以降) の局所変数の寿命を、
    /// その場で終わらせる (ブロック末尾の `DropScope` は通らないため)。
    fn drop_scopes_from(&mut self, cur: usize, depth: usize) {
        let vars: Vec<String> = self.scope_stack.iter().skip(depth).flatten().cloned().collect();
        for v in vars {
            self.emit(cur, Ev::DropScope(v));
        }
    }

    fn lower_stmt(&mut self, s: &V, cur: usize) -> usize {
        match nname(s) {
            "Let" => {
                let f = nf(s);
                let init = &f[3];
                let c = if is_nil(init) { cur } else { self.lower_expr(init, cur) };
                let mut binds = vec![];
                pat_binds(&f[0], &mut binds);
                // `let x = &<一時値>` / `let x = &mut <一時値>` では、一時値の寿命が `x`
                // のスコープまで延びる (Rust と同じ)。
                if self.is_direct_temp_ref(init) {
                    for b in &binds {
                        self.temp_extended.insert(b.clone());
                    }
                }
                // `let` で宣言した変数は、このブロックのスコープに入る。
                if let Some(top) = self.scope_stack.last_mut() {
                    for b in &binds {
                        top.push(b.clone());
                    }
                }
                let srcs = if is_nil(init) || nname(&f[0]) != "PBind" { vec![] } else { self.loan_sources(init) };
                for (k, b) in binds.iter().enumerate() {
                    self.emit(c, Ev::ScopeAdd(b.clone()));
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

    /// 1 つの式の中の借用: 左から右に評価した各オペランドが作る借用 (と、持っている借用) を、
    /// 一時の持ち主に持たせる。持ち主は式の最後 (`end_temps`) まで使われるので、あとのオペランドの
    /// 競合するアクセスと E3010 になる。リテラルは要素の借用をまとめて 1 つのオペランドとして数える。
    fn hold_temp(&mut self, c: usize, op: &V, temps: &mut Vec<String>) {
        let srcs: Vec<Src> = self.operand_sources(op).into_iter().filter(|s| !matches!(s, Src::Stored { .. })).collect();
        if srcs.is_empty() {
            return;
        }
        let name = format!("$e{}", self.iters);
        self.iters += 1;
        self.emit(c, Ev::Def(name.clone()));
        self.hold(c, Holder::Var(name.clone()), srcs);
        temps.push(name);
    }

    fn end_temps(&mut self, c: usize, temps: Vec<String>) {
        for t in temps {
            self.emit(c, Ev::Use(t));
        }
    }

    fn operand_sources(&self, op: &V) -> Vec<Src> {
        match nname(op) {
            "StructLit" => items(&nf(op)[1]).iter().filter(|f| items(f).len() >= 2).flat_map(|f| self.operand_sources(&items(f)[1])).collect(),
            "TupleE" | "ArrayE" => items(&nf(op)[0]).iter().flat_map(|x| self.operand_sources(x)).collect(),
            "ArrayRep" => self.operand_sources(&nf(op)[0]),
            _ => self.loan_sources(op),
        }
    }

    /// オペランドを左から右へ評価する。借用は `temps` に積む。
    fn lower_operands(&mut self, ops: &[&V], cur: usize, temps: &mut Vec<String>) -> usize {
        let mut c = cur;
        for op in ops {
            c = self.lower_expr(op, c);
            self.hold_temp(c, op, temps);
        }
        c
    }

    /// `core.mem.replace` / `core.mem.swap` の呼び出しか。
    fn mem_fn(&self, segs: &[String]) -> Option<&'static str> {
        if segs.len() < 2 {
            return None;
        }
        let r = self.p.resolve(segs, &self.file)?;
        match r.target {
            Target::Fn(i) | Target::Sig(i) if r.rest.is_empty() => match self.p.defs[i].name.as_str() {
                "core.mem.replace" => Some("replace"),
                "core.mem.swap" => Some("swap"),
                _ => None,
            },
            Target::Native(n) if r.rest.is_empty() => match n.as_str() {
                "core.mem.replace" => Some("replace"),
                "core.mem.swap" => Some("swap"),
                _ => None,
            },
            _ => None,
        }
    }

    /// `&mut <場所>` の引数の場所。
    fn mut_place_arg(&self, a: &V) -> Option<(String, Vec<String>)> {
        if nname(a) == "Unary" && st(&nf(a)[0]) == "&mut" { self.place_of(&nf(a)[1]) } else { None }
    }

    /// `core.mem.replace(&mut d, v)` / `core.mem.swap(&mut a, &mut b)`: 場所のあいだで借用が入れ替わる。
    /// 保守的に、入れた値の借用を場所に足し (古いローンは殺さない)、swap は互いのローンを足し合う。
    fn lower_mem(&mut self, kind: &str, args: &[V], e: &V, cur: usize) -> Option<usize> {
        let (a0, a1) = (args.first()?, args.get(1)?);
        let d0 = self.mut_place_arg(a0)?;
        let mut c = self.lower_index_parts(&nf(a0)[1], cur);
        let d1 = if kind == "swap" {
            let d1 = self.mut_place_arg(a1)?;
            c = self.lower_index_parts(&nf(a1)[1], c);
            Some(d1)
        } else {
            c = self.lower_expr(a1, c);
            None
        };
        let line = nline(e);
        let mut temps = vec![];
        if kind == "replace" {
            self.hold_temp(c, a1, &mut temps);
        }
        for (root, path) in std::iter::once(&d0).chain(d1.iter()) {
            self.emit(c, Ev::Use(root.clone()));
            self.emit(c, Ev::Access { root: root.clone(), path: path.clone(), kind: Acc::MutKeep, line });
        }
        if let Some(d1) = d1 {
            self.emit(c, Ev::Mix { a: d0.clone(), b: d1 });
        } else {
            let srcs = self.operand_sources(a1);
            self.hold(c, Holder::Place { root: d0.0.clone(), path: d0.1.clone() }, srcs);
        }
        self.end_temps(c, temps);
        Some(c)
    }

    /// クロージャの本体を、取り込みを宣言どおりの参照として束縛して検査する。`&x` の取り込みは、本体の間
    /// ずっと `x` を共有で借りている (本体での書き込み・`&mut` は競合)。`&mut x` / `move x` は本体の中では
    /// 変数そのものとして扱う。本体の中の式の複数の借用は、関数本体と同じ規則で見る。
    fn check_closure_body(&mut self, e: &V) {
        let f = nf(e);
        let mut locals = self.locals.clone();
        for p in items(&f[0]) {
            locals.insert(st(&items(p)[0]));
        }
        let mut sub = Low {
            p: self.p,
            file: self.file.clone(),
            nodes: vec![],
            loans: vec![],
            loops: vec![],
            exit: 0,
            iters: self.iters + 1000,
            sub: vec![],
            scope_stack: vec![],
            scope_seed: locals.iter().cloned().collect(),
            value_vals: HashMap::new(),
            moves: self.moves.clone(),
            locals,
            temp_extended: HashSet::new(),
        };
        let entry = sub.new_node();
        let exit = sub.new_node();
        sub.exit = exit;
        for (k, c) in items(&f[2]).iter().enumerate() {
            let name = st(tel(c, 1));
            if st(tel(c, 0)) == "&" && self.locals.contains(&name) {
                let h = format!("$cap{}", k);
                let id = sub.loans.len();
                sub.loans.push(LoanInfo { root: name, path: vec![], mutable: false });
                sub.emit(entry, Ev::Def(h.clone()));
                sub.emit(entry, Ev::Loan { holder: Holder::Var(h.clone()), id });
                sub.emit(exit, Ev::Use(h));
            }
        }
        let end = sub.lower_block(&f[3], entry);
        sub.edge(end, exit);
        let mut diags = analyze(&sub);
        diags.extend(std::mem::take(&mut sub.sub));
        self.sub.extend(diags);
    }

    /// 呼び出しの引数の借用を式の最後まで保つ形で、引数を評価する。
    fn lower_args_held(&mut self, args: &[V], cur: usize, temps: &mut Vec<String>) -> usize {
        let ops: Vec<&V> = args.iter().collect();
        self.lower_operands(&ops, cur, temps)
    }

    /// レシーバに `&mut self` で触れるときのアクセスの種類。
    fn recv_kind(&self, name: &str) -> Acc {
        match self.self_mode(name) {
            SelfMode::Mut if matches!(name, "push" | "push_back" | "push_front" | "insert") => Acc::MutKeep,
            SelfMode::Mut => Acc::MutB,
            _ => Acc::Read,
        }
    }

    /// 場所 `e` へのアクセスを出す。`line` は `moves` との照合および診断行に使う（`match` の
    /// スクリューティニーは、check 側が `match` 式そのものの行で move を記録するため、
    /// パス節の行ではなく `match` 式の行を渡す）。
    fn access(&mut self, n: usize, e: &V, kind: Acc, line: usize) -> bool {
        match self.place_of(e) {
            Some((root, path)) => {
                self.emit(n, Ev::Use(root.clone()));
                let kind = if kind == Acc::Read && self.moves.contains(&(line, place_key(&root, &path))) { Acc::Move } else { kind };
                self.emit(n, Ev::Access { root, path, kind, line });
                true
            }
            None => false,
        }
    }

    /// `match` のスクリューティニーへのアクセス。check 側が `match` 式そのものの行で move を
    /// 記録するため、パス節の行ではなく `match` 式の行 (`line`) を `moves` の照合と診断行に使う。
    /// 場所でない式（`v.push(&x)` のような呼び出し）は従来の `lower_expr` に任せ、
    /// レシーバへの `store_into` などの副作用を失わない。
    fn access_match_scrutinee(&mut self, cur: usize, e: &V, line: usize) -> usize {
        // 場所 (`s`、`s.f` など) なら `match` 式の行で `moves` と照合する。それ以外
        // (`v.push(&x)` のような呼び出し) は従来の `lower_expr` に任せ、
        // レシーバへの `store_into` などの副作用を失わない。
        if self.place_of(e).is_some() && self.access(cur, e, Acc::Read, line) {
            return cur;
        }
        self.lower_expr(e, cur)
    }

    fn lower_expr(&mut self, e: &V, cur: usize) -> usize {
        let f = nf(e);
        match nname(e) {
            "Lit" => cur,
            "Path" | "Field" | "Index" => {
                if nname(e) == "Index" {
                    let c = self.lower_index_parts(e, cur);
                    self.access(c, e, Acc::Read, nline(e));
                    return c;
                }
                if self.access(cur, e, Acc::Read, nline(e)) {
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
                    if self.access(c, &f[1], kind, nline(&f[1])) {
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
                self.loops.push((head, exit, self.scope_stack.len()));
                let end = self.lower_block(&f[1], body);
                self.loops.pop();
                self.edge(end, head);
                exit
            }
            "Loop" => {
                let head = self.new_node();
                self.edge(cur, head);
                let exit = self.new_node();
                self.loops.push((head, exit, self.scope_stack.len()));
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
                // `for x in <式に一時値を含むイテレータ>` の一時値は `for` 文の終わりまで生きる。
                if self.has_temp(&f[1]) {
                    for b in &binds {
                        self.temp_extended.insert(b.clone());
                    }
                }
                for b in &binds {
                    self.emit(body, Ev::Def(b.clone()));
                    self.emit(body, Ev::ScopeAdd(b.clone()));
                    if let Some(top) = self.scope_stack.last_mut() {
                        top.push(b.clone());
                    }
                    let srcs = self.loan_sources(&f[1]);
                    self.hold(body, Holder::Var(b.clone()), srcs);
                }
                self.loops.push((head, exit, self.scope_stack.len()));
                let end = self.lower_block(&f[2], body);
                self.loops.pop();
                self.edge(end, head);
                exit
            }
            "Match" => {
                // スクリューティニーのアクセスは `match` 式の行で `moves` と照合する
                // (check 側が `match` 式の行で move を記録するため)。
                let c = self.access_match_scrutinee(cur, &f[0], nline(e));
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
                    // `match <一時値を含む式> { .. }` では、腕の束縛が持つ一時値の寿命が
                    // `match` 全体の終わりまで延びる (仕様 4.2)。
                    if self.has_temp(&f[0]) {
                        for b in &binds {
                            self.temp_extended.insert(b.clone());
                        }
                    }
                    // 分解の腕は、部分パターンごとに要素・フィールドの借用を伝える。形が分からなければ従来どおり。
                    let known = matches!(nname(&a[0]), "PTuple" | "PStruct") && {
                        for b in &binds {
                            self.emit(an, Ev::Def(b.clone()));
                            self.emit(an, Ev::ScopeAdd(b.clone()));
                            if let Some(top) = self.scope_stack.last_mut() {
                                top.push(b.clone());
                            }
                        }
                        self.hold_pat(an, &a[0], &f[0])
                    };
                    if !known {
                        for b in &binds {
                            self.emit(an, Ev::Def(b.clone()));
                            self.emit(an, Ev::ScopeAdd(b.clone()));
                            if let Some(top) = self.scope_stack.last_mut() {
                                top.push(b.clone());
                            }
                            // 形が分からない分解 (`(a, n) => ..` を関数呼び出しの戻り値に対して行う) は、
                            // 右辺の借用を束縛へ配らない (`let (a, n) = f(&x)` と同じく黙る)。
                            // バリアントの中身 (`Some(rr) => rr`) は右辺の借用そのものなので従来どおり伝える。
                            if !matches!(nname(&a[0]), "PTuple" | "PStruct") {
                                let srcs = self.loan_sources(&f[0]);
                                self.hold(an, Holder::Var(b.clone()), srcs);
                            }
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
                if let Some(&(_, exit, depth)) = self.loops.last() {
                    self.drop_scopes_from(cur, depth);
                    self.edge(cur, exit);
                }
                self.new_node()
            }
            "Continue" => {
                if let Some(&(head, _, depth)) = self.loops.last() {
                    self.drop_scopes_from(cur, depth);
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
                self.check_closure_body(e);
                // 取り込む変数は、作った時点で使われ、借用される。
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
                        _ if self.moves.contains(&(nline(e), name.clone())) => Acc::Move,
                        _ => Acc::Read,
                    };
                    self.emit(cur, Ev::Access { root: name, path: vec![], kind: k, line: nline(e) });
                }
                cur
            }
            "StructLit" => {
                let ops: Vec<&V> = items(&f[1]).iter().filter(|x| items(x).len() >= 2).map(|x| &items(x)[1]).collect();
                let mut temps = vec![];
                let c = self.lower_operands(&ops, cur, &mut temps);
                self.end_temps(c, temps);
                c
            }
            "TupleE" | "ArrayE" => {
                let ops: Vec<&V> = items(&f[0]).iter().collect();
                let mut temps = vec![];
                let c = self.lower_operands(&ops, cur, &mut temps);
                self.end_temps(c, temps);
                c
            }
            "Block" => {
                // ブロック式の値: 末尾の式の借用を、ブロックの終わりで合成の持ち主に集める
                let holder = self.value_holder(e);
                let end = self.lower_block(e, cur);
                self.collect_value(end, &holder, &[], e);
                end
            }
            "Unsafe" => self.lower_block(&f[2], cur),
            // `with 名前 = 初期化式 close 後始末式 { 本体 }`: 本体のあとに後始末式が続く経路で近似する
            // (本体の `return` / `?` は出口へ直接つなぐので、その経路の後始末は追わない)。
            "With" => {
                let c = self.lower_expr(&f[2], cur);
                self.emit(c, Ev::Def(st(&nf(&f[0])[0])));
                let end = self.lower_block(&f[4], c);
                self.lower_expr(&f[3], end)
            }
            "Call" => {
                let callee = &f[0];
                let args = items(&f[1]);
                if nname(callee) == "Path" {
                    let segs = strs(&nf(callee)[0]);
                    if segs.len() >= 2 && self.locals.contains(&segs[0]) {
                        // `v.push(x)`: メソッド呼び出し (レシーバは segs[..-1])
                        let mut temps = vec![];
                        let c = self.lower_args_held(args, cur, &mut temps);
                        let name = segs[segs.len() - 1].clone();
                        let root = segs[0].clone();
                        let path = segs[1..segs.len() - 1].to_vec();
                        let mut kind = self.recv_kind(&name);
                        // `self` を値で受けるメソッド (`x.into_name()` など) はレシーバを move する:
                        // check 側が move_sites に載せたレシーバの場所なら、Read を Move に格上げする。
                        if kind == Acc::Read && self.moves.contains(&(nline(e), place_key(&root, &path))) {
                            kind = Acc::Move;
                        }
                        self.emit(c, Ev::Use(root.clone()));
                        self.emit(c, Ev::Access { root: root.clone(), path: path.clone(), kind, line: nline(e) });
                        self.store_into(c, &name, &root, &path, args);
                        self.end_temps(c, temps);
                        return c;
                    }
                    if segs.len() == 1 && self.locals.contains(&segs[0]) {
                        self.emit(cur, Ev::Use(segs[0].clone()));
                    }
                    if let Some(k) = self.mem_fn(&segs) {
                        if let Some(c) = self.lower_mem(k, args, e, cur) {
                            return c;
                        }
                    }
                } else {
                    // 呼び出し先の式 (ここでは追わない)
                }
                let mut di: Option<usize> = None;
                if nname(callee) == "Path" {
                    if let Some(r) = self.p.resolve(&strs(&nf(callee)[0]), &self.file) {
                        di = match r.target {
                            Target::Fn(i) | Target::Sig(i) => Some(i),
                            _ => None,
                        };
                    }
                }
                let mut temps = vec![];
                let c = self.lower_args_held(args, cur, &mut temps);
                if let Some(di) = di {
                    self.bind_receiver_arg_borrow(c, di, args);
                }
                self.end_temps(c, temps);
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
                let mut temps = vec![];
                if self.place_of(recv).is_some() {
                    let c = self.lower_index_parts(recv, cur);
                    let c = self.lower_args_held(args, c, &mut temps);
                    self.access(c, recv, self.recv_kind(&name), nline(recv));
                    // `v.push(&x)` / `m.insert(k, &x)`: レシーバの中に借用を保存する。
                    if let Some((root, path)) = self.place_of(recv) {
                        self.store_into(c, &name, &root, &path, args);
                    }
                    self.end_temps(c, temps);
                    return c;
                }
                let mut c = self.lower_expr(recv, cur);
                // レシーバが借用を返す呼び出しなら、その借用も引数の評価のあいだ保つ。
                if matches!(nname(recv), "MCall" | "Call") {
                    self.hold_temp(c, recv, &mut temps);
                }
                c = self.lower_args_held(args, c, &mut temps);
                self.end_temps(c, temps);
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

    /// パラメータ `p` の型が標準のコレクション (`&mut alloc.vec.Vec<..>` / `& alloc.collections.HashMap<..>` など)
    /// への参照か。利用者の型 (t.Buf など) は対象外。
    fn is_std_collection_param(d: &Def, p: usize) -> bool {
        let mut ty = &d.params[p].1;
        if tag(ty) == "ref" {
            ty = tel(ty, 2);
        }
        let V::L(parts) = ty else { return false };
        let [V::S(_), V::S(name), ..] = parts.as_slice() else { return false };
        matches!(name.as_str(),
            "alloc.vec.Vec" | "alloc.collections.HashMap" | "alloc.collections.BtreeMap"
            | "alloc.collections.VecDeque" | "alloc.collections.LinkedList"
            | "alloc.collections.BTreeSet" | "alloc.collections.HashSet")
    }

    /// `v.push(&s)` / `s.f = &x` のように、別の引数が指す先 (`&mut` 引数の中の Vec 要素や
    /// 構造体の欄) に参照を保存するとき、両者に同じ名前付きライフタイムが宣言されていなければ
    /// (行, 文面) の診断を返す (§4.1 の省略規則)。ライフタイムが書かれていないと要素の寿命と
    /// 借用元の寿命は無関係になり、後で解放済みを読む (d22 の穴)。
    fn check_lifetime_store(&self, di: usize, body: &V) -> Vec<(usize, String)> {
        let d = &self.p.defs[di];
        let head = match d.header.split(" uses ").next() {
            Some(h) => h,
            None => return vec![],
        };
        let b = head.as_bytes();
        let mut i = match head.find(|c| c == '(' || c == '[') {
            Some(i) => i,
            None => return vec![],
        };
        if b[i] == b'[' {
            i = match match_close(head, i) {
                Some(j) => j + 1,
                None => return vec![],
            };
        }
        if b.get(i) != Some(&b'(') {
            return vec![];
        }
        let close = match match_close(head, i) {
            Some(j) => j,
            None => return vec![],
        };
        let ptexts: Vec<String> = split_top(&head[i + 1..close]);
        if ptexts.len() != d.params.len() {
            return vec![];
        }
        let lt: Vec<BTreeSet<String>> = ptexts.iter().map(|t| lifetimes(t)).collect();
        let is_ref: Vec<bool> = ptexts.iter().map(|t| t.contains('&')).collect();
        let param_of: HashMap<&str, usize> =
            d.params.iter().enumerate().map(|(k, p)| (p.0.as_str(), k)).collect();

        // `let x = init` (1 束縛)。`let r = &s` のように参照から得た値を辿るため。
        let mut inits: HashMap<String, &V> = HashMap::new();
        let mut all = vec![];
        walk(body, &mut all);
        for v in &all {
            if nname(v) == "Let" {
                let f = nf(v);
                if f.len() > 3 && nname(&f[0]) == "PBind" && !is_nil(&f[3]) {
                    inits.insert(st(&nf(&f[0])[0]), &f[3]);
                }
            }
        }

        // (保存先の引数, 保存される値の出どころの引数, 行) の組。同じ行の重複はまとめる。
        let mut flagged: BTreeSet<(usize, usize, usize)> = BTreeSet::new();
        let mut check_store = |p: usize, value: &V, line: usize| {
            let Some(q) = self.store_source(value, &param_of, &is_ref, &inits) else { return };
            // `&'static` 値を保存するのは寿命に制約されず常に安全。
            if is_ref[q] && ptexts[q].contains("'static") {
                return;
            }
            if lt[p].intersection(&lt[q]).next().is_none() {
                flagged.insert((line, p, q));
            }
        };
        for v in &all {
            let line = nlineno(v);
            match nname(v) {
                "MCall" | "Call" => {
                    let f = nf(v);
                    let info = match nname(v) {
                        "MCall" => {
                            if f.len() < 3 {
                                None
                            } else {
                                let name = st(&f[1]);
                                if !matches!(name.as_str(), "push" | "push_back" | "push_front" | "insert")
                                {
                                    None
                                } else {
                                    let args = items(&f[2]);
                                    match self.place_of(&f[0]) {
                                        Some((root, path)) => Some((name, args, root, path)),
                                        None => None,
                                    }
                                }
                            }
                        }
                        "Call" => {
                            let callee = &f[0];
                            if nname(callee) != "Path" {
                                None
                            } else {
                                let segs = strs(&nf(callee)[0]);
                                if segs.len() < 2 {
                                    None
                                } else {
                                    let name = segs.last().unwrap().clone();
                                    if !matches!(
                                        name.as_str(),
                                        "push" | "push_back" | "push_front" | "insert"
                                    ) {
                                        None
                                    } else {
                                        let args = items(&f[1]);
                                        let root = segs[0].clone();
                                        let path: Vec<String> = segs[1..segs.len() - 1].to_vec();
                                        Some((name, args, root, path))
                                    }
                                }
                            }
                        }
                        _ => None,
                    };
                    let Some((name, args, root, path)) = info else { continue };
                    if !path.is_empty() {
                        continue;
                    }
                    let slot = if name == "insert" { 1 } else { 0 };
                    let Some(value) = args.get(slot) else { continue };
                    let Some(&p) = param_of.get(root.as_str()) else { continue };
                    // 標準のコレクション (alloc.vec.Vec / alloc.collections.HashMap など) への
                    // push/insert だけを検査する。利用者の型のメソッドは対象外。
                    if !Self::is_std_collection_param(d, p) {
                        continue;
                    }
                    check_store(p, value, line);
                }
                "Assign" => {
                    let f = nf(v);
                    if f.len() < 2 {
                        continue;
                    }
                    let Some((root, path)) = self.place_of(&f[0]) else { continue };
                    let Some(&p) = param_of.get(root.as_str()) else { continue };
                    let element_lt = if path.is_empty() {
                        lt[p].clone()
                    } else {
                        self.field_lifetime(d, p, &path[0])
                    };
                    // 欄の型が参照でなければ型検査の担当なので黙る (欄の参照は省略不可のため、
                    // element_lt が空ならこの検査の対象外)。
                    if !path.is_empty() && element_lt.is_empty() {
                        continue;
                    }
                    let Some(value) = f.get(1) else { continue };
                    check_store(p, value, line);
                }
                _ => {}
            }
        }
        let mut out = vec![];
        for (line, p, q) in flagged {
            out.push((
                line,
                format!(
                    "{}: argument `{}`'s reference (`{}`) is stored into argument `{}`'s pointee (`{}`), but no shared named lifetime is declared, so the pointee's lifetime and the reference's lifetime are unrelated; declare `['a]` and use `&'a T` on both",
                    d.name,
                    d.params[q].0,
                    ptexts[q],
                    d.params[p].0,
                    ptexts[p],
                ),
            ));
        }
        out
    }

    /// 保存される値 `e` の借用が、どの参照型パラメータ (またはそこから経由した `let` 束縛)
    /// に由来するか。なければ None。
    fn store_source(
        &self,
        e: &V,
        param_of: &HashMap<&str, usize>,
        is_ref: &[bool],
        inits: &HashMap<String, &V>,
    ) -> Option<usize> {
        match nname(e) {
            "Unary" => {
                let f = nf(e);
                let op = st(&f[0]);
                if op != "&" && op != "&mut" {
                    return None;
                }
                let Some((root, path)) = self.place_of(&f[1]) else { return None };
                if !path.is_empty() {
                    return None;
                }
                param_of.get(root.as_str()).copied()
            }
            "Path" => {
                let segs = strs(&nf(e)[0]);
                if segs.len() == 1 {
                    if let Some(&q) = param_of.get(segs[0].as_str()) {
                        return is_ref[q].then_some(q);
                    }
                    if let Some(init) = inits.get(segs[0].as_str()) {
                        return self.store_source(init, param_of, is_ref, inits);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// 構造体パラメータ `p` の欄 `fname` の型に現れる名前付きライフタイム集合。
    /// 欄の参照はライフタイムを省略できないので、空なら参照の欄ではない (型検査に任せる)。
    fn field_lifetime(&self, d: &Def, p: usize, fname: &str) -> BTreeSet<String> {
        let mut ty = &d.params[p].1;
        while tag(ty) == "ref" {
            ty = tel(ty, 2);
        }
        let V::L(parts) = ty else { return BTreeSet::new() };
        if parts.is_empty() || st(&parts[0]) != "named" || parts.len() < 2 {
            return BTreeSet::new();
        }
        let name = st(&parts[1]);
        let segs: Vec<String> = name.split('.').map(|s| s.to_string()).collect();
        let Some(r) = self.p.resolve(&segs, &self.file) else { return BTreeSet::new() };
        let Target::Type(tdi) = r.target else { return BTreeSet::new() };
        let mut out = BTreeSet::new();
        for (fname2, fty, _) in &self.p.defs[tdi].fields {
            if fname2 == fname {
                out = lifetimes(&fty.dump());
            }
        }
        out
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
        let scope_seed: Vec<String> = d.params.iter().map(|p| p.0.clone()).collect();
        let mut all = vec![];
        walk(body, &mut all);
        for n in &all {
            match nname(n) {
                "Let" | "For" | "With" => {
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
            sub: vec![],
            value_vals: HashMap::new(),
            moves: self.move_sites.clone(),
            scope_stack: vec![],
            scope_seed,
            temp_extended: HashSet::new(),
        };
        // 参照を別の引数の指す先 (Vec 要素・構造体欄) に保存する際の
        // ライフタイム不一致 (d22)。
        for (line, msg) in low.check_lifetime_store(di, body) {
            self.tdiag("E3010", msg, line);
        }
        let entry = low.new_node();
        let exit = low.new_node();
        low.exit = exit;
        let end = low.lower_block(body, entry);
        low.edge(end, exit);
        let mut diags = analyze(&low);
        diags.extend(std::mem::take(&mut low.sub));
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
    // ---- 前向き: どの変数・場所がどの借用を持つか、どの局所変数がスコープ内か
    #[derive(Clone, Default)]
    struct State {
        holders: BTreeMap<Holder, BTreeSet<usize>>,
        scope: BTreeSet<String>,
    }
    let mut state_in: Vec<Option<State>> = vec![None; n];
    state_in[0] = Some(State {
        holders: BTreeMap::new(),
        scope: low.scope_seed.iter().cloned().collect(),
    });
    let mut work: VecDeque<usize> = VecDeque::new();
    work.push_back(0);
    let apply = |st_: &mut State, ev: &Ev| match ev {
        Ev::Loan { holder, id } => {
            // 同じ変数・場所が複数の借用を持つこともある (`match` の被検査式など) ので、既存に足す。
            st_.holders.entry(holder.clone()).or_default().insert(*id);
        }
        Ev::Alias { to, from } => {
            let src = st_.holders.get(from).cloned().unwrap_or_default();
            st_.holders.entry(to.clone()).or_default().extend(src);
        }
        Ev::Load { to, from_root, from_path } => {
            // 保存場所から読み戻す。保存されていたローンは読んだ側へ移る (move の近似。
            // 実際はコピーでも、消えるのは見落としでしかない)。道が重なる保存場所 (タプルの要素の道 `#i` を含む)
            // はすべて対象にする。
            for src in take_overlapping(&mut st_.holders, from_root, from_path) {
                st_.holders.entry(to.clone()).or_default().extend(src);
            }
        }
        Ev::Peek { to, root, path } => {
            // 保存場所の借用を写す (保存場所のローンは残す)。
            let mut acc: BTreeSet<usize> = BTreeSet::new();
            for (h, ids) in st_.holders.iter() {
                if let Holder::Place { root: r, path: p } = h {
                    if r == root && overlaps(p, path) {
                        acc.extend(ids.iter().copied());
                    }
                }
            }
            if !acc.is_empty() {
                st_.holders.entry(to.clone()).or_default().extend(acc);
            }
        }
        Ev::Kill { root, path } => kill_stored(&mut st_.holders, root, path),
        Ev::Mix { a, b } => {
            let under = |st_: &State, r: &str, p: &[String]| -> BTreeSet<usize> {
                st_.holders
                    .iter()
                    .filter(|(h, _)| matches!(h, Holder::Place { root, path } if root == r && path.starts_with(p)))
                    .flat_map(|(_, v)| v.iter().copied())
                    .collect()
            };
            let mut all = under(st_, &a.0, &a.1);
            all.extend(under(st_, &b.0, &b.1));
            if !all.is_empty() {
                st_.holders
                    .entry(Holder::Place { root: a.0.clone(), path: a.1.clone() })
                    .or_default()
                    .extend(all.iter().copied());
                st_.holders
                    .entry(Holder::Place { root: b.0.clone(), path: b.1.clone() })
                    .or_default()
                    .extend(all);
            }
        }
        Ev::ScopeAdd(v) => {
            st_.scope.insert(v.clone());
        }
        Ev::Def(v) => {
            st_.holders.remove(&Holder::Var(v.clone()));
            // 根の変数の再代入 (`v = ...`) で、その中に保存されたローンはすべて死ぬ。
            let dead: Vec<Holder> = st_
                .holders
                .keys()
                .filter(|h| matches!(h, Holder::Place { root, .. } if root == v))
                .cloned()
                .collect();
            for h in dead {
                st_.holders.remove(&h);
            }
        }
        Ev::DropScope(v) => {
            st_.scope.remove(v);
        }
        Ev::Access { root, path, kind, .. } => {
            // 場所への上書き・`&mut` で、そこに保存されたローンは死ぬ。型が分からないので広めに殺す:
            // `v.push` のような要素を残す操作でも一度殺す (見落としはあっても、誤検出は出さない)。
            if matches!(kind, Acc::Write | Acc::MutB) {
                kill_stored(&mut st_.holders, root, path);
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
                    for (k, v) in &stt.holders {
                        let e = old.holders.entry(k.clone()).or_default();
                        for x in v {
                            ch |= e.insert(*x);
                        }
                    }
                    for x in &stt.scope {
                        ch |= old.scope.insert(x.clone());
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
                for (holder, ids) in &stt.holders {
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
                            Holder::Var(v) if v.starts_with("$cap") => "the shared capture of the closure".to_string(),
                            Holder::Var(v) if v.starts_with("$e") => "an earlier operand of the same expression".to_string(),
                            Holder::Var(v) => format!("`{}`", v),
                            Holder::Place { root: r, path: p } => format!("`{}`", place_name(r, p)),
                        };
                        if l.mutable {
                            out.push((*line, format!("cannot use `{}` while it is mutably borrowed by {}", root, who)));
                        } else if matches!(kind, Acc::Write | Acc::MutB | Acc::MutKeep) {
                            out.push((*line, format!("cannot mutate `{}` while it is borrowed by {}", root, who)));
                        } else if *kind == Acc::Move {
                            out.push((*line, format!("cannot move `{}` while it is borrowed by {}", root, who)));
                        }
                    }
                }
            }
            // 借用を保持する生きている変数が、その借用元の局所をスコープの終わりで
            // 失った後で使われる (ブロックを抜けた `&b` や一時値への参照など)。寿命切れの借用。
            if let Ev::Access { root, path, kind, line } = ev {
                if !matches!(kind, Acc::Move) {
                    for (holder, ids) in &stt.holders {
                        let Holder::Var(v) = holder else { continue };
                        // 式の中だけで完結する一時の持ち主 (`$e0` など) は局所変数ではないので、
                        // そこに保存された一時値への参照は式の終わりまでしか生きず、
                        // その後の別の変数の使用と誤検出にならないようスキップする。
                        if !low.locals.contains(v) { continue; }
                        // このアクセスそのものが holder を使う (最後の使用など) ため、
                        // 直後の生存に含まれなくても「今使われている」と見なす。
                        if !live_after[k].contains(v) && v != root { continue; }
                        for &id in ids {
                            let l = &low.loans[id];
                            if l.root == *root && overlaps(&l.path, path) {
                                continue;
                            }
                            // 借用元がスコープを抜けている (関数の仮引数や、
                            // `$temp` のような一時値はスコープに登録されない)。
                            if stt.scope.contains(&l.root) {
                                continue;
                            }
                            // `let x = &<一時値>` / `match &<一時値> { .. }` で束縛した
                            // 変数が持つ一時値は、その変数のスコープまで生きる。
                            if l.root == "$temp" && low.temp_extended.contains(v) {
                                continue;
                            }
                            out.push((*line, format!("use of `{root}` borrows `{}` which does not live long enough", l.root)));
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

/// `s[open]` の括弧 (`(`・`[`・`<`) に対応する閉じ括弧の位置。`->` の `>` は数えない。
fn match_close(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'(' | b'[' | b'<' => depth += 1,
            b')' | b']' => depth -= 1,
            b'>' if i > 0 && b[i - 1] == b'-' => {}
            b'>' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 最上位のカンマで分ける (括弧の中のカンマは区切りにしない)。
fn split_top(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let (mut depth, mut start) = (0i32, 0usize);
    let mut out = vec![];
    for i in 0..b.len() {
        match b[i] {
            b'(' | b'[' | b'<' => depth += 1,
            b')' | b']' => depth -= 1,
            b'>' if i > 0 && b[i - 1] == b'-' => {}
            b'>' => depth -= 1,
            b',' if depth == 0 => {
                out.push(s[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    if !s[start..].trim().is_empty() {
        out.push(s[start..].trim().to_string());
    }
    out
}

/// 文面に現れる名前付きライフタイム (`'static` を除く)。
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
            let name: String = cs[i + 1..j].iter().collect();
            if !name.is_empty() && name != "static" {
                out.insert(name);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// `&` の対象が `'static` の値か (リテラル、`-` 付きのリテラル、局所でないパス)。局所かどうかは呼ぶ側の
/// `place_of` が先に調べている (局所なら `Some` で、ここには来ない)。
fn is_static_operand(e: &V) -> bool {
    match nname(e) {
        "Lit" | "Path" => true,
        "Unary" => st(&nf(e)[0]) == "-" && nname(&nf(e)[1]) == "Lit",
        _ => false,
    }
}
