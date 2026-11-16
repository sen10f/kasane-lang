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
//! 保存されたローンがいつまで生きるかは、実際の型が分からないので広めに近似する。
//! 近似は fail-closed に倒す: 借用を持つか分からない値は「持つ」とみなして E3010 側へ寄せ、型が決まっていて
//! 借用を持ち得ない束縛 (`plain`) と、例・参照解で誤検出になった形だけを精密にして外す。
//! 近似の一覧と、対応する `unsound/` のケースは `stage0/tests/data/unsound/README.md`。
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
    /// 参照を外して中身を読む (`*rr`)。`from` が持つローンの指す先 (借りた場所) の値の中の借用が `to` へ移る。
    Deref { to: Holder, from: String },
    /// 参照の変数 `root` の場所 (`root` + `path`) に保存されたローンを、`root` が (可変に) 指す先の場所にも保存する
    /// (`p = &mut h; p.r = s.as_str()` は `h.r` に保存することでもある。`p` が死んでも `h` は借用を持ち続ける)。
    Through { root: String, path: Vec<String> },
    /// 保存場所から借用を写す (コピー。保存場所のローンは残す)。分解の束縛に使う。
    /// 道が重なる保存場所をすべて対象にする (タプルの要素は内部の道 `#i` に入っている)。
    Peek { to: Holder, root: String, path: Vec<String> },
    /// 場所の値を消費する (`core.mem.drop(s.f)`)。そこに保存されたローンは死ぬ。
    Kill { root: String, path: Vec<String> },
    /// 場所の下 (その道を接頭辞に持つ保存場所だけ。上位の保存場所は残す) に保存されたローンを殺す。定数の添字への上書きに使う。
    KillUnder { root: String, path: Vec<String> },
    /// 中断点 (`uses [suspend]` の関数の呼び出しの直後)。ここで生きている借用は、中断をまたぐので E3050 (仕様 5.6)。
    Suspend { line: usize },
    /// ブロックで局所変数が宣言され、スコープに入る。
    ScopeAdd(String),
    /// ブロックの末尾で、そのブロックで宣言した局所変数の寿命が終わる。
    DropScope(String, usize),
    /// ブロック・`match` の腕・`if` の枝の値 (持ち主 `holder`) が、その枝で束縛した所有物 `vars` の借用を運ぶ。
    /// 枝の終わりで `vars` は寿命が尽きるので、運んでいれば E3030 (`analyze` が見る)。
    Escape { holder: String, vars: Vec<(String, usize)>, line: usize },
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
    /// 借用元 (`root`) の束縛の位置 (束縛点)。同じ名前の別の束縛と取り違えない。仮引数・分からないときは `None`。
    site: Option<usize>,
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
    /// 参照を外した中身 (`*rr`) が持つ借用。内側は参照の式の出どころ。指す先の値の借用に写る。
    Deref(Vec<Src>),
    /// 場所 `root` + `path` が `inner` の借用を持つ (その場所を借りるローンは別に作る)。一時値への参照 `&s.as_str()` の
    /// 一時値が、中に持つ借用を運ぶのに使う。
    Contents { root: String, path: Vec<String>, inner: Vec<Src> },
}

struct Low<'a> {
    p: &'a Program,
    /// 型検査が解決したメソッド呼び出し先 (書き手の定義, 式の位置, 名前)。
    res: std::rc::Rc<HashMap<(usize, usize, String), Option<Vec<usize>>>>,
    /// 型検査が中断点と決めた呼び出し (書き手の定義, 式の位置)。
    susp: std::rc::Rc<HashSet<(usize, usize)>>,
    cur: usize,
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
    scope_stack: Vec<Vec<(String, usize)>>,
    /// 関数の仮引数。スコープ解析の初期値 (関数の間ずっと生きている)。
    scope_seed: Vec<String>,
    /// `let x = &<一時値>` / `match &<一時値> { .. }` で、一時値の寿命が束縛先のスコープまで
    /// 延びる変数名。`$temp` ルールで寿命切れと誤検出しないため。
    temp_extended: HashSet<String>,
    /// 型が決まっていて、借用を持ち得ない束縛の名前 (スカラー・所有値)。形が分からない分解で、
    /// これ以外の束縛へは右辺の借用を配る (fail-closed)。
    plain: HashSet<String>,
    /// 所有物の局所: 同じ名前の束縛のすべてが型を記録され、どれも借用を持ち得ない型の名前 (`borrowck` で求める)。
    /// 枝の値がこれの借用を運ぶと E3030 (`Ev::Escape`)。参照を通した分解 (`match &o { Some(p) => .. }`) の束縛は
    /// 型が記録されないので、同名のものがあれば入らない (黙る)。
    owned: HashSet<(usize, String)>,
    /// 型が記録されていて、参照でないと分かる束縛点。参照の変数 (または型の分からない束縛) の場所への保存は、
    /// その参照の指す先にも保存されたとみなす (`p = &mut h; p.r = s.as_str()`)。ここに入る束縛は指す先を持たない。
    nonref: HashSet<(usize, String)>,
    /// `with` の束縛点 (行, 名前) と、後始末式の (行, 束縛の名前)。診断の符号 (E3043) を決めるのに使う。
    with_sites: HashSet<(usize, String)>,
    with_closes: HashSet<(usize, String)>,
    /// 本体の値の型が借用を持ち得ない `with` の行 (値の借用は追わない)。
    with_free: HashSet<usize>,
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

    /// 場所の式が添字 (`v[i]`) を通るか。添字では要素を区別しないので、要素への書き込み・消費は、ほかの要素の
    /// 保存ローンを殺さない (どの要素かは分からない)。
    fn has_index(e: &V) -> bool {
        match nname(e) {
            "Index" => true,
            "Field" => Self::has_index(&nf(e)[0]),
            "Unary" if st(&nf(e)[0]) == "*" => Self::has_index(&nf(e)[1]),
            _ => false,
        }
    }

    /// 式 `e` の評価で一時値 (`$temp` への貸し出し) が作られるか。
    /// `match` / `for` / `if` / `while` の対象式の中の `$temp` は、その文の終わりまで
    /// 生きる (仕様 4.2)。
    fn has_temp(&self, e: &V) -> bool {
        self.loan_sources(e).iter().any(|s| matches!(s, Src::Place { root, .. } if root == "$temp"))
    }

    // ------------------------------------------------------------ メソッド・関数の署名
    /// 位置 `pos` のメソッド呼び出し `name` の候補。型検査が解決先を決めていればそれだけ、決められないときは
    /// 同名のすべて (fail-closed)。`nargs` は `self` を除く引数の個数で、合わない候補は外す。
    fn callees(&self, name: &str, pos: usize, nargs: usize) -> Vec<usize> {
        let all: &[usize] = self.p.method_idx.get(name).map_or(&[], |v| v.as_slice());
        let c: Vec<usize> = match self.res.get(&(self.cur, pos, name.to_string())) {
            Some(Some(r)) if !r.is_empty() => r.clone(),
            _ => all.to_vec(),
        };
        c.into_iter().filter(|&i| self.p.defs[i].params.len() == nargs + 1).collect()
    }

    /// 位置 `pos` のメソッド呼び出しの `self` の受け方。候補のどれかが `&mut self` なら `Mut` (fail-closed)、
    /// すべて `&self` なら `Shared`、それ以外 (値で受ける・候補なし) は `Other`。
    fn self_mode(&self, name: &str, pos: usize, nargs: usize) -> SelfMode {
        let mut mode: Option<SelfMode> = None;
        for i in self.callees(name, pos, nargs) {
            let d = &self.p.defs[i];
            let m = match d.params.first() {
                Some((_, t)) if tag(t) == "ref" => {
                    if matches!(tel(t, 1), V::B(true)) { SelfMode::Mut } else { SelfMode::Shared }
                }
                _ => SelfMode::Other,
            };
            mode = Some(match (mode, m) {
                (None, m) => m,
                (Some(SelfMode::Mut), _) | (_, SelfMode::Mut) => SelfMode::Mut,
                (Some(x), m) if x == m => m,
                _ => SelfMode::Other,
            });
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
        Self::ret_borrows(d) || Self::ret_has_type_param(d) || self.ret_struct_holds_ref(d)
    }

    /// 戻り値が、参照を欄に持つ名前付きの構造体 (`-> std.json.Value` で `doc: &'a Doc`) か。
    /// 型パラメータは含めない (`pop() -> Option<T>` を借用にすると、所有の戻り値まで誤って借りてしまう)。
    fn ret_struct_holds_ref(&self, d: &Def) -> bool {
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

    /// 型表現 `t` に現れる、`names` の型パラメータ (関数・メソッドのジェネリクス) を集める。
    /// `fn` / `dyn` は借用を隠せるので、型パラメータ同様に `*` として数える。
    fn ty_params_in(t: &V, names: &HashSet<String>, out: &mut HashSet<String>) {
        let V::L(xs) = t else { return };
        match xs.first() {
            Some(V::S(tag)) if tag == "named" => {
                if let Some(V::S(n)) = xs.get(1) {
                    if names.contains(n) {
                        out.insert(n.clone());
                    }
                }
                if let Some(V::L(args)) = xs.get(2) {
                    for a in args {
                        Self::ty_params_in(a, names, out);
                    }
                }
            }
            // 生ポインタは借用を追わない (`unsafe` の外へ出る値。`core.ptr.from_mut(&mut x)` が `x` を借り続けない)。
            Some(V::S(tag)) if tag == "ptr" => {}
            Some(V::S(tag)) if tag == "fn" || tag == "dyn" || tag == "self" => {
                out.insert("*".to_string());
            }
            // 先頭が名札でない並び (タプルの要素の並びなど) は、先頭も含めて全部見る。
            Some(V::S(_)) => {
                for x in xs.iter().skip(1) {
                    Self::ty_params_in(x, names, out);
                }
            }
            _ => {
                for x in xs.iter() {
                    Self::ty_params_in(x, names, out);
                }
            }
        }
    }

    fn type_param_names(d: &Def) -> HashSet<String> {
        d.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.clone()).collect()
    }

    /// 戻り値の型が、ジェネリクスの型パラメータ (参照を入れうる) や `fn` / `dyn` を含むか。
    /// 型パラメータに借用が入りうるので、引数の借用が戻り値へ通り抜けるとみなす (fail-closed)。
    fn ret_has_type_param(d: &Def) -> bool {
        let names = Self::type_param_names(d);
        let mut out = HashSet::new();
        Self::ty_params_in(&d.ret, &names, &mut out);
        !out.is_empty()
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

    /// 呼び出しが中断点か (5.6)。関数は解決した定義の `uses` に `suspend` があるか、メソッドは同名の候補の
    /// どれかが `suspend` を持てば中断点とみなす (fail-closed。型で候補を絞れないため、同名の別の型のメソッドも含む)。
    fn suspends(&self, di: Option<usize>, method: Option<&str>) -> bool {
        let has = |i: usize| self.p.defs[i].uses.iter().any(|u| u == "suspend");
        if let Some(i) = di {
            return has(i);
        }
        match method.and_then(|m| self.p.method_idx.get(m)) {
            Some(cands) => cands.iter().any(|&i| has(i)),
            None => false,
        }
    }

    /// 解決先の関数の定義。ネイティブ (`Target::Native`) は、同名の sig の定義で見る (借用の流れは宣言した署名で決まる)。
    fn target_def(&self, t: &Target) -> Option<usize> {
        match t {
            Target::Fn(i) | Target::Sig(i) => Some(*i),
            Target::Native(n) => self.p.sigs.get(n).copied(),
            _ => None,
        }
    }

    /// 関数呼び出し (`Path` の呼び先) が中断点か。ネイティブ (`Target::Native`) は、同名の sig の `uses` で見る。
    fn call_suspends(&self, callee: &V) -> bool {
        if nname(callee) != "Path" {
            return false;
        }
        let segs = strs(&nf(callee)[0]);
        let Some(r) = self.p.resolve(&segs, &self.file) else { return false };
        let di = match &r.target {
            Target::Fn(i) | Target::Sig(i) => Some(*i),
            Target::Native(n) => self.p.sigs.get(n).copied(),
            _ => None,
        };
        di.map_or(false, |i| self.p.defs[i].uses.iter().any(|u| u == "suspend"))
    }

    /// 名前 `name` のメソッドが、レシーバを借りた結果を返すか。(借りるなら `self` が可変か)
    /// 複数の候補 (トレイト実装など) があるとき、少なくとも 1 つが該当すれば `Some` を返す (fail-closed)。
    fn method_borrows(&self, name: &str) -> Option<bool> {
        let cands = self.p.method_idx.get(name)?;
        let mut any_mut = false;
        let mut found = false;
        for &i in cands {
            let d = &self.p.defs[i];
            let Some((_, t)) = d.params.first() else { continue };
            // 戻り値が借用を含む: 参照型・ライフタイム引数・参照を欄に持つ構造体 (`-> std.json.Value` の `doc: &'a Doc`)。
            if tag(t) == "ref" && (Self::ret_borrows(d) || self.ret_struct_holds_ref(d)) {
                found = true;
                any_mut |= matches!(tel(t, 1), V::B(true));
            }
        }
        if found { Some(any_mut) } else { None }
    }

    // ------------------------------------------------------------ 借用の出どころ
    fn loan_sources(&self, e: &V) -> Vec<Src> {
        match nname(e) {
            "Unary" => {
                let op = st(&nf(e)[0]);
                if op == "*" {
                    // 参照を外した値 (`*rr`、`*v.first().unwrap()`) は、指す先の値が持つ借用を運ぶ。
                    let inner = self.loan_sources(&nf(e)[1]);
                    return if inner.is_empty() { vec![] } else { vec![Src::Deref(inner)] };
                }
                if op != "&" && op != "&mut" {
                    return vec![];
                }
                let operand = &nf(e)[1];
                // 再借用 `&mut *r` は r の借用を引き継ぐ。`&mut *s.f` なら、s.f に保存された借用も読み戻す。
                if nname(operand) == "Unary" && st(&nf(operand)[0]) == "*" {
                    if let Some((root, path)) = self.place_of(&nf(operand)[1]) {
                        let mut out = vec![Src::Alias(root.clone())];
                        if !path.is_empty() {
                            out.push(Src::Stored { root: root.clone(), path: path.clone() });
                        }
                        // 再借用は、借りた先 `*P` の上に新しい借用を作る (`P` の参照そのものを借りるのではない)。
                        // だから `P` を読む・書く・もう一度借りると、再借用が生きている間は E3010 になる。
                        let mut deref_path = path;
                        deref_path.push("*".into());
                        out.push(Src::Place { root, path: deref_path, mutable: op == "&mut" });
                        return out;
                    }
                }
                // 場所でないものの再借用 (`&*&x`、`&*x.as_str()`、`&mut *&mut x`): 内側の借用がそのまま通り抜ける。
                if let Some(inner) = Self::reborrow_inner(operand) {
                    if self.place_of(inner).is_none() {
                        let v = self.borrow_expr_sources(inner);
                        if !v.is_empty() {
                            return v;
                        }
                    }
                }
                match self.place_of(operand) {
                    Some((root, path)) => {
                        // `&r` (r が参照を持つ変数) は、r を通して r の借用元も借りる (参照の参照の推移)。
                        // `&p.f` も同じ: `p` が参照なら `p` の借用元を通した先の借用になり、`p` に保存された借用も `p.f` の中に入る。
                        let mut out = vec![];
                        if self.locals.contains(&root) {
                            out.push(Src::Alias(root.clone()));
                            if !path.is_empty() {
                                out.push(Src::Stored { root: root.clone(), path: path.clone() });
                            }
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
                    None => {
                        // 一時値が借用を中に持つなら (`&s.as_str()`)、一時値の場所に持たせる。一時値ごとに別の道 (式の位置) を使う。
                        let inner = self.borrow_expr_sources(operand);
                        let path = if inner.is_empty() { vec![] } else { vec![format!("#t{}", operand as *const V as usize)] };
                        let mut out = vec![Src::Place { root: "$temp".into(), path: path.clone(), mutable: op == "&mut" }];
                        if !inner.is_empty() {
                            out.push(Src::Contents { root: "$temp".into(), path, inner });
                        }
                        out
                    }
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
            "Match" | "If" | "Block" | "With" => match self.value_vals.get(&(e as *const V as usize)) {
                Some(h) => vec![Src::Alias(h.clone())],
                None => vec![],
            },
            "MCall" => {
                let name = st(&nf(e)[1]);
                let recv = &nf(e)[0];
                if matches!(name.as_str(), "unwrap" | "expect" | "unwrap_or") && self.place_of(recv).is_none() {
                    return self.loan_sources(recv);
                }
                self.method_sources(recv, &name, nline(e), items(&nf(e)[2]))
            }
            "Call" => {
                if nname(&nf(e)[0]) != "Path" {
                    return vec![];
                }
                let segs = strs(&nf(&nf(e)[0])[0]);
                if segs.len() == 1 && self.locals.contains(&segs[0]) {
                    // 局所のクロージャ・関数値の呼び出し: 結果は取り込んだ借用を持ちうる (束縛の型が借用を持たないと
                    // 分かる場合は、Let で外す)。
                    return vec![Src::Alias(segs[0].clone())];
                }
                if segs.len() >= 2 && self.locals.contains(&segs[0]) {
                    // `v.first()` の呼び出し形: レシーバは segs[..-1]
                    let name = segs[segs.len() - 1].clone();
                    let recv_segs = segs[..segs.len() - 1].to_vec();
                    if matches!(name.as_str(), "unwrap" | "expect" | "unwrap_or") {
                        return self.passthrough_sources(&name, nline(e), Some((recv_segs[0].clone(), recv_segs[1..].to_vec())), None, items(&nf(e)[1]));
                    }
                    return match self.method_borrows(&name) {
                        Some(m) => {
                            let mut out = vec![
                                Src::Place { root: recv_segs[0].clone(), path: recv_segs[1..].to_vec(), mutable: m },
                                Src::Stored { root: recv_segs[0].clone(), path: recv_segs[1..].to_vec() },
                            ];
                            // レシーバの変数が持つ借用 (`let w = mk(&mut x); w.get()`) も結果へ流れる。
                            out.push(Src::Alias(recv_segs[0].clone()));
                            out.extend(self.method_arg_sources(&name, nline(e), items(&nf(e)[1])));
                            out
                        }
                        None => self.passthrough_sources(&name, nline(e), Some((recv_segs[0].clone(), recv_segs[1..].to_vec())), None, items(&nf(e)[1])),
                    };
                }
                // 関数の戻り値が借用を含むなら、借用の引数 (`&x` / `&mut x` / 参照の変数) から借りる
                let Some(r) = self.p.resolve(&segs, &self.file) else { return vec![] };
                let di = match r.target {
                    // `core.mem.replace` などは `lower_mem` が固有に扱う (中身の移動で、借用の引数そのものは返らない)。
                    Target::Native(_) if self.mem_fn(&segs).is_some() => {
                        // `replace(&mut d, v)` が返す古い値は、`d` が運んでいた借用を引き継ぐ (`swap` は返さない)。
                        if self.mem_fn(&segs) == Some("replace") {
                            if let Some((root, path)) = items(&nf(e)[1]).first().and_then(|a| self.mut_place_arg(a)) {
                                return vec![Src::Stored { root: root.clone(), path }, Src::Alias(root)];
                            }
                        }
                        return vec![];
                    }
                    Target::Fn(_) | Target::Sig(_) | Target::Native(_) => match self.target_def(&r.target) {
                        Some(i) => i,
                        None => return vec![],
                    },
                    // `Some(&x)` / `Pair(&a, &b)`: 構築した値が引数の借用を持つ。
                    Target::Variant(..) | Target::Type(_) if r.rest.is_empty() => {
                        return items(&nf(e)[1]).iter().flat_map(|a| { let mut v = self.loan_sources(a); v.extend(self.lit_sources(a)); v }).collect();
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
                    .flat_map(|(_, a)| { let mut v = self.loan_sources(a); v.extend(self.lit_sources(a)); v })
                    .collect()
            }
            "Closure" => {
                let mut out = vec![];
                for c in items(&nf(e)[2]) {
                    let kind = st(tel(c, 0));
                    let name = st(tel(c, 1));
                    if !self.locals.contains(&name) {
                        continue;
                    }
                    if kind == "&" || kind == "&mut" {
                        // 取り込んだ変数が運ぶ借用 (`v` が `m.get_mut(..)` の結果なら `m` の借用) もクロージャへ引き継ぐ。
                        out.push(Src::Alias(name.clone()));
                        out.push(Src::Stored { root: name.clone(), path: vec![] });
                        out.push(Src::Place { root: name, path: vec![], mutable: kind == "&mut" });
                    } else {
                        // `move r` は値ごと取り込む。`r` が持つ借用 (`&mut x` を持つ参照) もクロージャへ移る。
                        // 欄・要素に隠した借用 (`t.RefCell { p: x }` の欄) も保存場所から読み戻して運ぶ。
                        out.push(Src::Alias(name.clone()));
                        out.push(Src::Stored { root: name, path: vec![] });
                    }
                }
                out
            }
            _ => vec![],
        }
    }

    /// 定義 `d` の戻り値が、借用を含まない所有値 (名前付きの型で、型引数にも参照がなく、型パラメータでもない) か。
    fn def_returns_owned(&self, d: &Def) -> bool {
        let V::L(parts) = &d.ret else { return false };
        let [V::S(a), V::S(name), args] = parts.as_slice() else { return false };
        if a != "named" || d.generics.iter().any(|g| &g.1 == name) || Self::ty_contains_ref(args) {
            return false;
        }
        let segs: Vec<String> = name.split('.').map(|s| s.to_string()).collect();
        let Some(r) = self.p.resolve(&segs, &self.file) else { return false };
        matches!(r.target, Target::Type(_)) && !self.ret_contains_ref(d)
    }

    /// 戻り値が、名前付きの型の値そのもの (参照でも型パラメータそのものでもない) か。借用を持ちうる型引数 (`String<A>`) は問わない:
    /// その値を借りれば、値が一時なら借用は文の終わりで死ぬ。
    fn def_returns_value(&self, d: &Def) -> bool {
        let V::L(parts) = &d.ret else { return false };
        let [V::S(a), V::S(name), _] = parts.as_slice() else { return false };
        if a != "named" || d.generics.iter().any(|g| &g.1 == name) {
            return false;
        }
        let segs: Vec<String> = name.split('.').map(|s| s.to_string()).collect();
        let Some(r) = self.p.resolve(&segs, &self.file) else { return false };
        matches!(r.target, Target::Type(_))
    }

    fn method_returns_value(&self, name: &str) -> bool {
        let Some(cands) = self.p.method_idx.get(name) else { return false };
        !cands.is_empty()
            && cands.iter().all(|&i| {
                let d = &self.p.defs[i];
                d.params.first().map_or(false, |(n, _)| n == "self") && self.def_returns_value(d)
            })
    }

    fn method_returns_owned(&self, name: &str) -> bool {
        let Some(cands) = self.p.method_idx.get(name) else { return false };
        !cands.is_empty()
            && cands.iter().all(|&i| {
                let d = &self.p.defs[i];
                d.params.first().map_or(false, |(n, _)| n == "self") && self.def_returns_owned(d)
            })
    }

    /// 呼び出し式 `e` が、借用を含まない所有値を返すと確かに言えるか (関数は解決先、メソッドは名前の全候補で判断)。
    fn returns_owned_temp(&self, e: &V) -> bool {
        match nname(e) {
            "Call" => {
                if nname(&nf(e)[0]) != "Path" {
                    return false;
                }
                let segs = strs(&nf(&nf(e)[0])[0]);
                if segs.len() >= 2 && self.locals.contains(&segs[0]) {
                    // `w.dup()` の呼び出し形 (メソッド)
                    return self.method_returns_value(&segs[segs.len() - 1]);
                }
                let Some(r) = self.p.resolve(&segs, &self.file) else { return false };
                match self.target_def(&r.target) {
                    Some(i) if r.rest.is_empty() => self.def_returns_value(&self.p.defs[i]),
                    _ => false,
                }
            }
            "MCall" => self.method_returns_value(&st(&nf(e)[1])),
            _ => false,
        }
    }

    /// 戻り値が型パラメータを含む (`v.pop()` / `o.unwrap()`) メソッド: レシーバを新しく借りはしないが、
    /// レシーバの中の借用と、型パラメータを共有する引数の借用は戻り値へ通り抜ける (fail-closed)。
    fn passthrough_sources(&self, name: &str, pos: usize, place: Option<(String, Vec<String>)>, recv: Option<&V>, args: &[V]) -> Vec<Src> {
        let cands = self.callees(name, pos, args.len());
        if !cands.iter().any(|&i| Self::ret_has_type_param(&self.p.defs[i])) {
            return vec![];
        }
        let mut out = match (place, recv) {
            (Some((root, path)), _) => vec![Src::Stored { root: root.clone(), path }, Src::Alias(root)],
            (None, Some(r)) if matches!(nname(r), "MCall" | "Call") => self.loan_sources(r),
            _ => vec![],
        };
        out.extend(self.method_arg_sources(name, pos, args));
        out
    }

    fn method_sources(&self, recv: &V, name: &str, pos: usize, args: &[V]) -> Vec<Src> {
        let Some(m) = self.method_borrows(name) else {
            return self.passthrough_sources(name, pos, self.place_of(recv), Some(recv), args);
        };
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
                if matches!(nname(recv), "MCall" | "Call") {
                    let mut v = self.loan_sources(recv);
                    // 戻り値が所有値 (借用を含まない名前付きの型) の呼び出しの結果を借りたら、一時値を借りている
                    // (`t.mk().as_str()`)。一時値は文の終わりで死ぬので、束縛して後で使うと dangling。
                    if v.is_empty() && self.returns_owned_temp(recv) {
                        v.push(Src::Place { root: "$temp".into(), path: vec![], mutable: m });
                    }
                    v
                } else if nname(recv) == "Unary" && matches!(st(&nf(recv)[0]).as_str(), "&" | "&mut") {
                    // `(&Some(x)).as_ref()`: 借りた値 (一時値を含む) の借用が、メソッドの結果へ通り抜ける。
                    self.loan_sources(recv)
                } else {
                    vec![]
                }
            }
        };
        out.extend(self.method_arg_sources(name, pos, args));
        out
    }

    /// メソッドの `self` 以外の引数のうち、戻り値へ流れるもの (すべての候補のヘッダが一致して流すとき) の借用。
    fn method_arg_sources(&self, name: &str, pos: usize, args: &[V]) -> Vec<Src> {
        // 候補のどれかが流すなら流す (fail-closed。解決先が決まらないときは同名で個数の合うものすべて)。
        let mut flow: Vec<bool> = vec![false; args.len()];
        for i in self.callees(name, pos, args.len()) {
            let f = Self::flow_params(&self.p.defs[i]);
            for (k, slot) in flow.iter_mut().enumerate() {
                *slot |= f.as_ref().map_or(true, |f| f.get(k + 1).copied().unwrap_or(false));
            }
        }
        args.iter().zip(flow).filter(|(_, f)| *f).flat_map(|(a, _)| { let mut v = self.loan_sources(a); v.extend(self.lit_sources(a)); v }).collect()
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
        // 戻り値の参照がすべて `&'static` なら、どの引数の借用も戻り値へは流れない (型パラメータ経由は下で足す)。
        let only_static = ret_lts.is_empty() && ret.contains("&'static") && ret.matches('&').count() == ret.matches("&'static").count();
        let flows: Vec<bool> = if only_static {
            vec![false; params.len()]
        } else if !ret_lts.is_empty() {
            params.iter().map(|p| lifetimes(p).iter().any(|l| ret_lts.contains(l))).collect()
        } else if d.params.first().map_or(false, |p| p.0 == "self" && tag(&p.1) == "ref") {
            (0..params.len()).map(|k| k == 0).collect()
        } else {
            params.iter().map(|p| p.contains('&') || p.contains('\'')).collect()
        };
        // 戻り値の型パラメータ (`-> T` / `-> Option<T>`) と型パラメータを共有する引数も流れる。
        let names = Self::type_param_names(d);
        let mut ret_ps = HashSet::new();
        Self::ty_params_in(&d.ret, &names, &mut ret_ps);
        let flows: Vec<bool> = flows
            .into_iter()
            .zip(d.params.iter())
            .map(|(f, (_, pt))| {
                if f || ret_ps.is_empty() {
                    return f;
                }
                let mut ps = HashSet::new();
                Self::ty_params_in(pt, &names, &mut ps);
                ps.contains("*") || ret_ps.contains("*") || ps.iter().any(|p| ret_ps.contains(p))
            })
            .collect();
        Some(flows)
    }

    /// ヘッダの仮引数の文面 (`x: &'a T` など。`ty.dump()` にはライフタイムが含まれないのでヘッダから取る)。
    fn param_texts(d: &Def) -> Option<Vec<String>> {
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
        let ptexts = split_top(&head[i + 1..close]);
        if ptexts.len() != d.params.len() {
            return None;
        }
        Some(ptexts)
    }

    /// 関数・メソッドの定義 `d` が、ある引数 (受け手 R、`&mut` で指す先が借用を入れうる) に、別の引数 (借用元 S) の借用を
    /// 保存しうる組 (R の番号, S の番号)。ヘッダだけで決める (本体は見ない)。
    /// - 名前付きライフタイム: `fn keep['a](v: &mut Vec<&'a String>, s: &'a String)` のように `'a` が R と S の両方に現れる。
    ///   R は `&mut` で指す先がさらに参照・ライフタイムを持つもの、S はそれ以外の参照の引数。
    /// - 型パラメータ: `fn put[T](o: &mut T, x: T)` のように、R の指す先と S が型パラメータ (`fn` / `dyn` も含む) を共有する。
    ///   S が参照のときは、R の指す先も参照を持つものだけ (`x: &T` を `Vec<T>` に入れるのは複製で、`x` そのものは保存されない)。
    /// `method_form` は、メソッド形の呼び出しで戻り値に流れなくても同じ領域を共有する引数を保存しうる、の意。
    fn recv_src_pairs(d: &Def, method_form: bool) -> Vec<(usize, usize)> {
        let Some(ptexts) = Self::param_texts(d) else { return vec![] };
        let mut flow = Self::flow_params(d).unwrap_or_else(|| vec![true; d.params.len()]);
        if method_form {
            flow = vec![true; flow.len()];
        }
        let is_mut_ref = |ty: &V| tag(ty) == "ref" && matches!(tel(ty, 1), V::B(true));
        // 参照の本体の文面 (`&'a mut Vec<&'a T>` → `Vec<&'a T>`)。
        let pointee_text = |k: usize| -> String {
            let t = ptexts[k].split_once(':').map_or("", |x| x.1).trim();
            let t = t.strip_prefix('&').unwrap_or(t).trim_start();
            let t = if t.starts_with('\'') { t.split_once(' ').map_or("", |x| x.1) } else { t };
            t.trim_start().strip_prefix("mut ").unwrap_or(t).to_string()
        };
        let mut pairs: Vec<(usize, usize)> = vec![];
        let lts: Vec<String> = d.generics.iter().filter(|g| g.0 == "life").map(|g| g.1.trim_start_matches('\'').to_string()).collect();
        if !lts.is_empty() {
            let (mut receivers, mut sources) = (vec![], vec![]);
            for (k, (_, ty)) in d.params.iter().enumerate() {
                let ltys = lifetimes(&ptexts[k]);
                if !flow[k] || tag(ty) != "ref" || !lts.iter().any(|l| ltys.contains(l)) {
                    continue;
                }
                // `r: &'a mut u32` のように指す先が参照を持たない `&mut` は借用元 (保存される側)。
                let pt = pointee_text(k);
                if is_mut_ref(ty) && (pt.contains('&') || pt.contains('\'')) {
                    receivers.push(k);
                } else {
                    sources.push(k);
                }
            }
            for &r in &receivers {
                for &s in &sources {
                    pairs.push((r, s));
                }
            }
        }
        let names = Self::type_param_names(d);
        let tps = |t: &V| {
            let mut out = HashSet::new();
            Self::ty_params_in(t, &names, &mut out);
            out
        };
        for (k, (_, ty)) in d.params.iter().enumerate() {
            if !is_mut_ref(ty) {
                continue;
            }
            let rt = tps(tel(ty, 2));
            if rt.is_empty() {
                continue;
            }
            let r_holds_ref = { let t = pointee_text(k); t.contains('&') || t.contains('\'') };
            for (j, (_, sty)) in d.params.iter().enumerate() {
                if j == k || pairs.contains(&(k, j)) {
                    continue;
                }
                let st_ = tps(sty);
                if st_.is_disjoint(&rt) {
                    continue;
                }
                if tag(sty) == "ref" && !r_holds_ref {
                    continue;
                }
                pairs.push((k, j));
            }
        }
        pairs
    }

    /// `fn keep['a](v: &mut Vec<&'a String>, s: &'a String) -> &'a String` や `fn put[T](o: &mut T, x: T)` のように、
    /// ヘッダが「受け手 R の指す先に借用元 S の借用が入りうる」と言う組 (`recv_src_pairs`) では、S の借用を R の場所に
    /// 結びつけ、R の最後の使用まで生かす。`v` が生きている間は `s` を move できない (E3010)。R が参照の変数なら、
    /// 指す先にも保存される (`hold` の `Through`)。戻り値への借用追跡 (flow_params / ret_contains_ref) と同じ仕組み。
    fn bind_receiver_arg_borrow(&mut self, c: usize, di: usize, args: &[V], recv: Option<String>) {
        let d = &self.p.defs[di];
        let pairs = Self::recv_src_pairs(d, recv.is_some());
        // メソッド形 (`h.set(x)`) では、レシーバは args に入っていないので 1 つずらす。
        let shift = usize::from(recv.is_some());
        let sig: Vec<bool> = d.params.iter().map(|(_, ty)| tag(ty) == "ref" && matches!(tel(ty, 1), V::B(true))).collect();
        for (r, s) in pairs {
            let rroot = if let Some(rr) = &recv {
                if r != 0 { continue }
                rr.clone()
            } else {
                let Some(a) = args.get(r) else { continue };
                let Some((rroot, _)) = self.arg_place(a) else { continue };
                rroot
            };
            if s < shift { continue }
            let Some(b) = args.get(s - shift) else { continue };
            let mutable = sig[s];
            let srcs: Vec<Src> = match self.borrow_root(b) {
                Some(sroot) => {
                    if sroot == rroot {
                        continue;
                    }
                    vec![Src::Place { root: sroot, path: vec![], mutable }]
                }
                // `s.as_str()` のように借用を返す式: その借用元をレシーバが持つ。
                None => self.borrow_expr_sources(b).into_iter().filter(|s| !matches!(s, Src::Stored { .. })).collect(),
            };
            if srcs.is_empty() {
                continue;
            }
            self.hold(c, Holder::Var(rroot.clone()), srcs.clone());
            self.hold(c, Holder::Place { root: rroot, path: vec![] }, srcs);
        }
    }

    /// メソッド呼び出し `h.set(x)`: 名前のメソッドがただ 1 つなら、そのヘッダで `&mut self` と参照の引数が
    /// 同じライフタイムを共有するとき、引数の借用をレシーバ `h` に持たせる。
    fn bind_method_setter(&mut self, c: usize, name: &str, pos: usize, root: &str, args: &[V]) {
        // 解決先が決まらないときは、同名で個数の合う候補すべての束縛を足す (fail-closed)。
        for di in self.callees(name, pos, args.len()) {
            if self.p.defs[di].params.first().map_or(true, |p| p.0 != "self") {
                continue;
            }
            self.bind_receiver_arg_borrow(c, di, args, Some(root.to_string()));
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
                    let id = self.new_loan(root, path, mutable);
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
                Src::Contents { root, path, inner } => {
                    self.hold(n, Holder::Place { root, path }, inner);
                }
                Src::Deref(inner) => {
                    let t = format!("$d{}", self.iters);
                    self.iters += 1;
                    self.emit(n, Ev::Def(t.clone()));
                    self.hold(n, Holder::Var(t.clone()), inner);
                    self.emit(n, Ev::Deref { to: holder.clone(), from: t });
                }
            }
        }
        // 参照の変数 (または型の分からない束縛) の場所に保存したなら、その参照の指す先にも保存されている。
        if let Holder::Place { root, path } = &holder {
            if self.is_ref_root(root) {
                self.emit(n, Ev::Through { root: root.clone(), path: path.clone() });
            }
        }
    }

    /// 場所の根の変数が、参照 (または型の分からない束縛) か。型が記録されていて参照でない束縛 (構造体など) は指す先を持たない。
    fn is_ref_root(&self, root: &str) -> bool {
        match self.site_of(root) {
            Some(site) => !self.nonref.contains(&(site, root.to_string())),
            None => true,
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
        // 形が分からない分解の束縛へは、型が借用を持ち得ないと分かるもの以外に右辺の借用を配る (fail-closed)。
        if matches!(nname(pat), "PTuple" | "PStruct") {
            self.hold_whole_f(n, pat, e, true);
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
        self.hold_whole_f(n, pat, e, false)
    }

    /// `hold_whole`。`skip_plain` なら、型が借用を持ち得ないと分かっている束縛 (`plain`) には配らない。
    fn hold_whole_f(&mut self, n: usize, pat: &V, e: &V, skip_plain: bool) {
        let mut srcs = self.loan_sources(e);
        srcs.extend(self.lit_sources(e));
        let mut binds = vec![];
        pat_binds(pat, &mut binds);
        for b in binds {
            if skip_plain && self.plain.contains(&b) {
                continue;
            }
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
            // タプルの要素は内部の道 `#i` に置く (分解のとき要素ごとに読めるように)。配列リテラルも同じ (定数の添字への上書き用)。
            "TupleE" | "ArrayE" => {
                for (i, x) in items(&nf(e)[0]).iter().enumerate() {
                    let mut p = path.to_vec();
                    p.push(format!("#{}", i));
                    self.store_value(n, root, &p, x);
                }
                return;
            }
            _ => {}
        }
        // `&[&x]` (リテラルを借りる): 借りた値の中の参照 (`&x`) が、受け手へ入る (リテラルの一時の借用ではなく)。
        let lit_borrow = nname(e) == "Unary" && matches!(st(&nf(e)[0]).as_str(), "&" | "&mut") && matches!(nname(&nf(e)[1]), "ArrayE" | "TupleE" | "ArrayRep" | "StructLit");
        let mut srcs = if lit_borrow { self.lit_sources(&nf(e)[1]) } else { self.loan_sources(e) };
        if srcs.is_empty() {
            srcs = self.lit_sources(e);
        }
        // `&arr` (局所の変数を借りる): その変数が持つ借用 (`[&x]` の `x`) も受け手へ入る。
        if nname(e) == "Unary" && matches!(st(&nf(e)[0]).as_str(), "&" | "&mut") && nname(&nf(e)[1]) == "Path" {
            let segs = crate::check::strs(&nf(&nf(e)[1])[0]);
            if segs.len() == 1 {
                srcs.push(Src::Alias(segs[0].clone()));
            }
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
        self.borrow_expr_sources(tail)
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
        // `insert` は、値だけでなく引数の全部を入れる: `HashMap::insert(k, v)` / `BtreeMap::insert(k, v)` の
        // キー (0 番目) も格納される。`Vec::insert(i, v)` の添字は借用を持たないので、何も保存しない。
        // `extend` / `extend_from_slice` は、渡された列の要素 (参照を含む) を入れる。
        // 引数の中の参照を受け手が保存する (格納する) メソッド。`&mut` の引数 (`append(&mut other)`) は移すのではなく
        // 相手を借りるだけなので、格納しない。
        if matches!(name, "insert" | "extend" | "extend_from_slice" | "append") {
            for a in args {
                // `append(&mut other)`: 相手の要素 (とその借用) を受け手へ移す。相手のローンも受け手に持たせる
                // (`&mut` で貸すローンそのものは作らない)。
                if name == "append" && nname(a) == "Unary" && st(&nf(a)[0]) == "&mut" {
                    let srcs: Vec<Src> = self.loan_sources(&nf(a)[1]).into_iter().filter(|x| !matches!(x, Src::Place { .. })).collect();
                    let mut srcs = srcs;
                    if let Some((r, p)) = self.place_of(&nf(a)[1]) {
                        srcs.push(Src::Stored { root: r.clone(), path: p.clone() });
                        srcs.push(Src::Deref(vec![Src::Alias(r.clone()), Src::Stored { root: r, path: p }]));
                    }
                    if !srcs.is_empty() {
                        self.hold(n, Holder::Place { root: root.to_string(), path: path.to_vec() }, srcs);
                    }
                    continue;
                }
                if nname(a) == "StructLit" {
                    let srcs = self.lit_sources(a);
                    if !srcs.is_empty() {
                        self.hold(n, Holder::Place { root: root.to_string(), path: path.to_vec() }, srcs);
                    }
                    continue;
                }
                self.store_value(n, root, path, a);
            }
            return;
        }
        let slot = match name {
            "push" | "push_back" | "push_front" => 0,
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
        // 寿命が尽きるのは「ブロックで作った所有物」だけ。参照の別名 (`let p = s`) は
        // 元の借用の寿命を持つので、ここで DropScope しない。
        let vars = self.scope_stack.pop().unwrap_or_default();
        self.drop_owned(cur, vars);
        cur
    }

    /// 束縛点ごとに、所有物 (`owned`) だけ寿命を終わらせる。参照の別名は元の借用の寿命を持つ。
    fn drop_owned(&mut self, cur: usize, vars: Vec<(String, usize)>) {
        for (v, site) in vars {
            if self.owned.contains(&(site, v.clone())) {
                self.emit(cur, Ev::DropScope(v, site));
            }
        }
    }

    /// 名前 `name` が今指す束縛の束縛点 (内側のスコープから探す)。仮引数・不明は `None`。
    fn site_of(&self, name: &str) -> Option<usize> {
        self.scope_stack.iter().rev().flat_map(|l| l.iter().rev()).find(|(n, _)| n == name).map(|(_, s)| *s)
    }

    fn new_loan(&mut self, root: String, path: Vec<String>, mutable: bool) -> usize {
        let site = self.site_of(&root);
        self.loans.push(LoanInfo { root, path, mutable, site });
        self.loans.len() - 1
    }

    /// `break` / `continue` で抜けるブロック (スコープ深さ `depth` 以降) の局所変数の寿命を、
    /// その場で終わらせる (ブロック末尾の `DropScope` は通らないため)。
    /// `lower_block` と同じだが、ブロックの値の借用を、そのブロックの局所変数の寿命が尽きる前に合成の持ち主へ集める
    /// (集めたローンの借用元がブロックの局所なら、そのあと `DropScope` で寿命切れになる)。
    fn lower_block_value(&mut self, b: &V, mut cur: usize, holder: &str, binds: &[String], base: usize) -> usize {
        if nname(b) != "Block" {
            let end = self.lower_expr(b, cur);
            self.collect_value(end, holder, binds, b);
            let dying: Vec<(String, usize)> = self.scope_stack.iter().skip(base).flatten().cloned().collect();
            self.escape_check(end, holder, dying, b);
            return end;
        }
        self.scope_stack.push(vec![]);
        for s in items(&nf(b)[0]) {
            cur = self.lower_stmt(s, cur);
        }
        self.collect_value(cur, holder, binds, b);
        // 枝の束縛 (`match` の腕) と、ブロックの中で宣言した局所が、この枝の終わりで寿命が尽きる。
        let dying: Vec<(String, usize)> = self.scope_stack.iter().skip(base).flatten().cloned().collect();
        self.escape_check(cur, holder, dying, b);
        let vars = self.scope_stack.pop().unwrap_or_default();
        self.drop_owned(cur, vars);
        cur
    }

    /// 枝の値 (持ち主 `holder`) が、枝の終わりで寿命が尽きる所有物 `dying` の借用を運んでいないかを、
    /// `analyze` で見るために `Ev::Escape` を置く。`dying` のうち所有物の局所 (`owned`) だけを対象にする。
    fn escape_check(&mut self, n: usize, holder: &str, dying: Vec<(String, usize)>, body: &V) {
        let vars: Vec<(String, usize)> = dying.into_iter().filter(|(v, s)| self.owned.contains(&(*s, v.clone()))).collect();
        if vars.is_empty() {
            return;
        }
        let line = Self::arm_tail(body).map_or(nline(body), nline);
        self.emit(n, Ev::Escape { holder: holder.to_string(), vars, line });
    }

    fn drop_scopes_from(&mut self, cur: usize, depth: usize) {
        // `break`/`continue` で抜けるブロックの局所も、寿命が尽きるのは「所有物」だけ。
        // 参照の別名は元の借用の寿命を持つ。
        let vars: Vec<(String, usize)> = self.scope_stack.iter().skip(depth).flatten().cloned().collect();
        self.drop_owned(cur, vars);
    }

    fn lower_stmt(&mut self, s: &V, cur: usize) -> usize {
        match nname(s) {
            "Let" => {
                let f = nf(s);
                let init = &f[3];
                let c = if is_nil(init) { cur } else { self.lower_expr(init, cur) };
                let mut binds = vec![];
                pat_binds(&f[0], &mut binds);
                // `let x = &<一時値>` でも一時値は文の終端で落ちる (寿命の延長はしない。名前を付けて持つ: `let tmp = ..; let x = &tmp`)。
                // `let r2 = *rr` (`rr: &&T`) も `loan_sources` の `Deref` が運ぶ (型が借用を持ち得ないと分かる束縛 `plain` には配らない)。
                let srcs = if is_nil(init) || nname(&f[0]) != "PBind" { vec![] } else { self.loan_sources(init) };
                for (k, b) in binds.iter().enumerate() {
                    self.emit(c, Ev::ScopeAdd(b.clone()));
                    self.emit(c, Ev::Def(b.clone()));
                    if k == 0 && !srcs.is_empty() && !self.plain.contains(b) {
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
                // `let` で宣言した変数は、このブロックのスコープに入る。借用元の名前は、右辺を見終えてから (遮蔽の前の束縛を指す)。
                if let Some(top) = self.scope_stack.last_mut() {
                    for b in &binds {
                        top.push((b.clone(), nline(s)));
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
                    let srcs = if self.plain.contains(&name) { vec![] } else { self.loan_sources(&f[1]) };
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
                // `*r = v` は `*r` (根 r の道 `*`) への書き込み。再借用 `&mut *r` が生きていれば E3010。
                // `s.f = v` / `v[i] = x` は根の変数への書き込み。
                if nname(target) == "Unary" && st(&nf(target)[0]) == "*" {
                    if let Some((root, path)) = self.place_of(target) {
                        // 参照の指す先への書き込みは、参照そのもの (`h.r`) に保存されたローンを殺さない。
                        // 競合の判定は `Write` と同じ (`MutB` は可変の書き込みとして競合する) ので、殺さない種類で出す。
                        self.emit(c, Ev::Use(root.clone()));
                        self.emit(c, Ev::Access { root, path, kind: Acc::MutB, line: nline(s) });
                        return c;
                    }
                    return self.lower_expr(&nf(target)[1], c);
                }
                let c = self.lower_index_parts(target, c);
                if let Some((root, path)) = self.place_of(target) {
                    self.emit(c, Ev::Use(root.clone()));
                    // 添字を通る書き込みは、どの要素か分からないので、保存されたローンを殺さない (足すだけ)。
                    // 定数の添字 (`a[0] = ..`) だけは、配列リテラルの要素ごとの保存場所 `#0` を上書きする。
                    if Self::has_index(target) {
                        self.emit(c, Ev::Access { root: root.clone(), path: path.clone(), kind: Acc::MutKeep, line: nline(s) });
                        let mut spath = path.clone();
                        if nname(target) == "Index" && nname(&nf(target)[1]) == "Lit" && !Self::has_index(&nf(target)[0]) {
                            let k = format!("#{}", nf(&nf(target)[1])[1].dump());
                            spath.push(k);
                            self.emit(c, Ev::KillUnder { root: root.clone(), path: spath.clone() });
                        }
                        self.store_value(c, &root, &spath, &f[1]);
                        return c;
                    }
                    self.emit(c, Ev::Access { root: root.clone(), path: path.clone(), kind: Acc::Write, line: nline(s) });
                    // 借用を場所に保存する (`s.f = &mut x`)。上書きなので、
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
            res: self.res.clone(),
            susp: self.susp.clone(),
            cur: self.cur,
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
            plain: self.plain.clone(),
            owned: self.owned.clone(),
            nonref: self.nonref.clone(),
            with_sites: self.with_sites.clone(),
            with_closes: self.with_closes.clone(),
            with_free: self.with_free.clone(),
        };
        let entry = sub.new_node();
        let exit = sub.new_node();
        sub.exit = exit;
        for (k, c) in items(&f[2]).iter().enumerate() {
            let name = st(tel(c, 1));
            if st(tel(c, 0)) == "&" && self.locals.contains(&name) {
                let h = format!("$cap{}", k);
                let id = sub.loans.len();
                sub.loans.push(LoanInfo { root: name, path: vec![], mutable: false, site: None });
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
    fn recv_kind(&self, name: &str, pos: usize, nargs: usize) -> Acc {
        match self.self_mode(name, pos, nargs) {
            SelfMode::Mut if matches!(name, "push" | "push_back" | "push_front" | "insert" | "extend" | "extend_from_slice" | "append") => Acc::MutKeep,
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
                    // `&*r` / `&mut *r` は `*r` への共有・可変のアクセス (そこに新しい借用を作る)。
                    if nname(&f[1]) == "Unary" && st(&nf(&f[1])[0]) == "*" {
                        let kind = if op == "&mut" { Acc::MutB } else { Acc::Shared };
                        if self.access(c, &f[1], kind, nline(&f[1])) {
                            return c;
                        }
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
                let t_end = match &holder {
                    Some(h) => { let base = self.scope_stack.len(); self.lower_block_value(&f[1], then_n, h, &[], base) }
                    None => self.lower_block(&f[1], then_n),
                };
                self.edge(t_end, join);
                let e_end = if is_nil(&f[2]) {
                    else_n
                } else {
                    match &holder {
                        Some(h) => { let base = self.scope_stack.len(); self.lower_block_value(&f[2], else_n, h, &[], base) }
                        None => self.lower_block(&f[2], else_n),
                    }
                };
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
                // 束縛は反復ごとの層に入れ、反復の終わり (と `break` / `continue`) で寿命が尽きる。
                let depth = self.scope_stack.len();
                for b in &binds {
                    self.emit(body, Ev::Def(b.clone()));
                    self.emit(body, Ev::ScopeAdd(b.clone()));
                    let srcs = self.loan_sources(&f[1]);
                    self.hold(body, Holder::Var(b.clone()), srcs);
                }
                self.scope_stack.push(binds.iter().map(|b| (b.clone(), nline(e))).collect());
                self.loops.push((head, exit, depth));
                let end = self.lower_block(&f[2], body);
                self.loops.pop();
                let vars = self.scope_stack.pop().unwrap_or_default();
                self.drop_owned(end, vars);
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
                    // 腕の束縛は腕ごとの層に入れ、腕の終わりで寿命が尽きる (囲むブロックの終わりまで延ばさない)。
                    // 借用元の名前は、束縛を入れる前の見え方で決める (`Some(o) => ..` が被検査式 `o` を遮らない)。
                    let base = self.scope_stack.len();
                    // 分解の腕は、部分パターンごとに要素・フィールドの借用を伝える。形が分からなければ従来どおり。
                    let known = matches!(nname(&a[0]), "PTuple" | "PStruct") && {
                        for b in &binds {
                            self.emit(an, Ev::Def(b.clone()));
                            self.emit(an, Ev::ScopeAdd(b.clone()));
                        }
                        self.hold_pat(an, &a[0], &f[0])
                    };
                    if !known {
                        for b in &binds {
                            self.emit(an, Ev::Def(b.clone()));
                            self.emit(an, Ev::ScopeAdd(b.clone()));
                            // 形が分からない分解 (`(a, n) => ..` を関数呼び出しの戻り値に対して行う) は、
                            // 型が借用を持ち得ないと分かる束縛 (`plain`) 以外へ右辺の借用を配る (fail-closed)。
                            if !(matches!(nname(&a[0]), "PTuple" | "PStruct") && self.plain.contains(b)) {
                                let srcs = self.loan_sources(&f[0]);
                                self.hold(an, Holder::Var(b.clone()), srcs);
                            }
                        }
                    }
                    self.scope_stack.push(binds.iter().map(|b| (b.clone(), nline(&a[0]))).collect());
                    let end = self.lower_block_value(&a[1], an, &holder, &binds, base);
                    let vars = self.scope_stack.pop().unwrap_or_default();
                    self.drop_owned(end, vars);
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
                let base = self.scope_stack.len();
                self.lower_block_value(e, cur, &holder, &[], base)
            }
            "Unsafe" => self.lower_block(&f[2], cur),
            // `with 名前 = 初期化式 close 後始末式 { 本体 }` (仕様 4.4): 束縛を作る → 本体 (束縛のスコープの中、値は
            // ふつうのブロックの値と同じく持ち主へ集める) → 後始末式で束縛を move、と下ろす。本体の値・外への代入が束縛を
            // 借りて、後始末の move の時点でも生きているなら、競合の報告になる (`borrowck` が E3043 に直す)。
            // 本体の `return` / `?` は出口へ直接つなぐので、その経路の後始末は追わない。
            "With" => {
                let name = st(&nf(&f[0])[0]);
                let c = self.lower_expr(&f[2], cur);
                self.emit(c, Ev::ScopeAdd(name.clone()));
                self.emit(c, Ev::Def(name.clone()));
                self.with_sites.insert((nline(e), name.clone()));
                let base = self.scope_stack.len();
                self.scope_stack.push(vec![(name.clone(), nline(e))]);
                let end = if self.with_free.contains(&nline(e)) {
                    self.lower_block(&f[4], c)
                } else {
                    let holder = self.value_holder(e);
                    self.lower_block_value(&f[4], c, &holder, &[], base)
                };
                self.with_closes.insert((nline(&f[3]), name));
                let end = self.lower_expr(&f[3], end);
                // 束縛の寿命の終わり (`DropScope`) は置かない: 借用が残る誤りは E3043 で報告済みで、
                // 続く使用を E3010 の二重報告にしない。
                self.scope_stack.pop();
                end
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
                        let mut kind = self.recv_kind(&name, nline(e), args.len());
                        // `self` を値で受けるメソッド (`x.into_name()` など) はレシーバを move する:
                        // check 側が move_sites に載せたレシーバの場所なら、Read を Move に格上げする。
                        if kind == Acc::Read && self.moves.contains(&(nline(e), place_key(&root, &path))) {
                            kind = Acc::Move;
                        }
                        self.emit(c, Ev::Use(root.clone()));
                        self.emit(c, Ev::Access { root: root.clone(), path: path.clone(), kind, line: nline(e) });
                        self.store_into(c, &name, &root, &path, args);
                        if path.is_empty() {
                            self.bind_method_setter(c, &name, nline(e), &root, args);
                        }
                        self.end_temps(c, temps);
                        if self.suspends(None, Some(&name)) || self.susp.contains(&(self.cur, nline(e))) {
                            self.emit(c, Ev::Suspend { line: nline(e) });
                        }
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
                        di = self.target_def(&r.target);
                    }
                }
                let mut temps = vec![];
                let c = self.lower_args_held(args, cur, &mut temps);
                if let Some(di) = di {
                    self.bind_receiver_arg_borrow(c, di, args, None);
                }
                self.end_temps(c, temps);
                if self.call_suspends(callee) || self.susp.contains(&(self.cur, nline(e))) {
                    self.emit(c, Ev::Suspend { line: nline(e) });
                }
                // `core.mem.drop(p)`: 場所の値を消費する。そこに保存された借用はここで死ぬ。
                if nname(callee) == "Path" && self.is_drop_fn(&strs(&nf(callee)[0])) {
                    if let Some(arg) = args.first() {
                        if let Some((root, path)) = self.place_of(arg).filter(|_| !Self::has_index(arg)) {
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
                    self.access(c, recv, self.recv_kind(&name, nline(e), args.len()), nline(recv));
                    // `v.push(&x)` / `m.insert(k, &x)`: レシーバの中に借用を保存する。
                    if let Some((root, path)) = self.place_of(recv) {
                        self.store_into(c, &name, &root, &path, args);
                        if path.is_empty() {
                            self.bind_method_setter(c, &name, nline(e), &root, args);
                        }
                    }
                    self.end_temps(c, temps);
                    if self.suspends(None, Some(&name)) || self.susp.contains(&(self.cur, nline(e))) {
                        self.emit(c, Ev::Suspend { line: nline(e) });
                    }
                    return c;
                }
                let mut c = self.lower_expr(recv, cur);
                // レシーバが借用を返す呼び出しなら、その借用も引数の評価のあいだ保つ。
                if matches!(nname(recv), "MCall" | "Call") {
                    self.hold_temp(c, recv, &mut temps);
                }
                c = self.lower_args_held(args, c, &mut temps);
                self.end_temps(c, temps);
                if self.suspends(None, Some(&name)) || self.susp.contains(&(self.cur, nline(e))) {
                    self.emit(c, Ev::Suspend { line: nline(e) });
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
                    // 呼び出し先のヘッダが「受け手 R の指す先に借用元 S の借用が入りうる」と言うなら、その引数の組も保存とみなす。
                    // 省略した参照の引数を、名前付きのライフタイムで結ぶ関数へ転送する形 (`t.inner(v, x)`) も、名前の一覧に頼らず見る。
                    {
                        let (callees, args, recv): (Vec<usize>, &[V], Option<(String, Vec<String>)>) = if nname(v) == "MCall" {
                            if f.len() < 3 { (vec![], &[], None) } else {
                                let args = items(&f[2]);
                                (self.callees(&st(&f[1]), nline(v), args.len()), args, self.place_of(&f[0]))
                            }
                        } else if nname(&f[0]) != "Path" {
                            (vec![], &[], None)
                        } else {
                            let segs = strs(&nf(&f[0])[0]);
                            let args = items(&f[1]);
                            if segs.len() >= 2 && self.locals.contains(&segs[0]) {
                                let name = segs[segs.len() - 1].clone();
                                (self.callees(&name, nline(v), args.len()), args, Some((segs[0].clone(), segs[1..segs.len() - 1].to_vec())))
                            } else {
                                match self.p.resolve(&segs, &self.file) {
                                    Some(r) if r.rest.is_empty() => match self.target_def(&r.target) {
                                        Some(i) => (vec![i], args, None),
                                        None => (vec![], &[], None),
                                    },
                                    _ => (vec![], &[], None),
                                }
                            }
                        };
                        for di2 in callees {
                            let method_form = recv.is_some() || self.p.defs[di2].params.first().map_or(false, |p| p.0 == "self") && nname(v) == "MCall";
                            let shift = usize::from(method_form);
                            for (r, sidx) in Self::recv_src_pairs(&self.p.defs[di2], method_form) {
                                let place = if method_form {
                                    if r != 0 { continue }
                                    recv.clone()
                                } else {
                                    args.get(r).and_then(|a| self.arg_place(a))
                                };
                                let Some((root, path)) = place else { continue };
                                if !(path.is_empty() || path == ["*"]) {
                                    continue;
                                }
                                let Some(&p) = param_of.get(root.as_str()) else { continue };
                                if sidx < shift { continue }
                                let Some(value) = args.get(sidx - shift) else { continue };
                                check_store(p, value, line);
                            }
                        }
                    }
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
                    // `*p = v` は引数そのものの指す先への書き込み (欄ではない)。その指す先の領域は `p` の型の領域。
                    let element_lt = if path.is_empty() || path == ["*"] {
                        lt[p].clone()
                    } else {
                        self.field_lifetime(d, p, &path[0])
                    };
                    // 欄の型が参照でなければ型検査の担当なので黙る (欄の参照は省略不可のため、
                    // element_lt が空ならこの検査の対象外)。
                    if !path.is_empty() && path != ["*"] && element_lt.is_empty() {
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
            // `Some(x)` / `Ok(x)` / `Err(x)` に包んだ参照も、包まれた値の出どころ (戻り値の `ret_slots` と同じ形)。
            "Call" => {
                let f = nf(e);
                let args = items(&f[1]);
                let wrap = nname(&f[0]) == "Path" && matches!(strs(&nf(&f[0])[0]).as_slice(), [s] if matches!(s.as_str(), "Some" | "Ok" | "Err"));
                if wrap && args.len() == 1 { self.store_source(&args[0], param_of, is_ref, inits) } else { None }
            }
            // 構造体リテラルの欄に入る参照 (`t.H { r: x }`)。
            "StructLit" => items(&nf(e)[1]).iter().filter_map(|f| items(f).get(1)).find_map(|x| self.store_source(x, param_of, is_ref, inits)),
            _ => None,
        }
    }

    /// 戻り値が `&'L T` と宣言された関数の本体が、別の名前の領域 `'M` を持つ参照引数をそのまま返す
    /// (`'M: 'L` の境界もない) と、宣言より短い (または無関係な) 借用が戻り値に流れる。`'L` が `'static` なら、
    /// `'static` でない名前付きの領域の引数を返すのも同じ。読める形 (引数そのものが末尾式・`return` の値) だけを見る。
    fn check_return_lifetime(&self, d: &Def, body: &V) -> Vec<(usize, String)> {
        let Some(head) = d.header.split(" uses ").next() else { return vec![] };
        let Some(mut i) = head.find(|c| c == '(' || c == '[') else { return vec![] };
        let gen_text = if head.as_bytes()[i] == b'[' {
            let Some(j) = match_close(head, i) else { return vec![] };
            let g = head[i..=j].to_string();
            i = j + 1;
            g
        } else {
            String::new()
        };
        if head.as_bytes().get(i) != Some(&b'(') {
            return vec![];
        }
        let Some(close) = match_close(head, i) else { return vec![] };
        let ptexts = split_top(&head[i + 1..close]);
        if ptexts.len() != d.params.len() {
            return vec![];
        }
        let Some((_, ret)) = head[close + 1..].split_once("->") else { return vec![] };
        // 戻り値の型の先頭の `&'L`。
        let ref_lt = |t: &str| -> Option<String> {
            let t = t.trim().strip_prefix('&')?.trim_start();
            let t = t.strip_prefix("mut ").unwrap_or(t).trim_start();
            let t = t.strip_prefix('\'')?;
            let n: String = t.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            (!n.is_empty()).then_some(n)
        };
        let Some(want) = ref_lt(ret) else { return vec![] };
        let mut all = vec![];
        walk(body, &mut all);
        // 引数を隠す束縛があれば、名前だけでは引数と決められない。
        let mut shadow: HashSet<String> = HashSet::new();
        for n in &all {
            match nname(n) {
                "Let" | "For" | "With" => {
                    let mut b = vec![];
                    pat_binds(&nf(n)[0], &mut b);
                    shadow.extend(b);
                }
                "Match" => {
                    for arm in items(&nf(n)[1]) {
                        let mut b = vec![];
                        pat_binds(&items(arm)[0], &mut b);
                        shadow.extend(b);
                    }
                }
                _ => {}
            }
        }
        fn tails<'v>(e: &'v V, out: &mut Vec<&'v V>) {
            match nname(e) {
                "Block" => {
                    if let Some(last) = items(&nf(e)[0]).last() {
                        match nname(last) {
                            "ExprStmt" => tails(&nf(last)[0], out),
                            "Let" | "Assign" => {}
                            _ => tails(last, out),
                        }
                    }
                }
                "If" => {
                    let f = nf(e);
                    tails(&f[1], out);
                    if !is_nil(&f[2]) {
                        tails(&f[2], out);
                    }
                }
                "Match" => {
                    for arm in items(&nf(e)[1]) {
                        if let Some(b) = items(arm).last() {
                            tails(b, out);
                        }
                    }
                }
                _ => out.push(e),
            }
        }
        let mut vals: Vec<&V> = vec![];
        tails(body, &mut vals);
        for v in &all {
            if nname(v) == "Return" && !is_nil(&nf(v)[0]) {
                tails(&nf(v)[0], &mut vals);
            }
        }
        let mut out = vec![];
        for v in vals {
            if nname(v) != "Path" {
                continue;
            }
            let segs = strs(&nf(v)[0]);
            if segs.len() != 1 || shadow.contains(&segs[0]) {
                continue;
            }
            let Some(q) = d.params.iter().position(|p| p.0 == segs[0]) else { continue };
            let Some(have) = ptexts[q].split_once(':').and_then(|x| ref_lt(x.1)) else {
                // 省略した入力の参照をそのまま名前付きの戻り値の領域として返す (`x: &u32` を `&'b u32` で返す) は、
                // 省略された領域が `'b` に収まらない。
                if is_elided_ref(&ptexts[q]) {
                    out.push((
                        nlineno(v),
                        format!(
                            "{}: returns argument `{}` (`{}`), whose lifetime is elided (a new region per call), but the header declares the return type `&'{} ..`",
                            d.name, d.params[q].0, ptexts[q], want
                        ),
                    ));
                }
                continue;
            };
            if have == "static" || have == want {
                continue;
            }
            // `'b: 'a` のような境界を宣言していれば読めないので通す。
            if gen_text.contains(&format!("'{}:", have)) || gen_text.contains(&format!("'{} :", have)) {
                continue;
            }
            out.push((
                nlineno(v),
                format!(
                    "{}: returns argument `{}` (`{}`) but the header declares the return type `&'{} ..`; `'{}` is not declared to outlive `'{}`",
                    d.name, d.params[q].0, ptexts[q], want, have, want
                ),
            ));
        }
        out
    }

    /// 構造体パラメータ `p` の欄 `fname` が宣言の上で持つ名前付きの領域 (`'static` を除く)。
    /// 型の抽象木には領域が残らないので、宣言の `field_lifetimes` を見る。空なら、その欄は引数の領域を運ばない。
    fn field_named_lifetimes(&self, d: &Def, p: usize, fname: &str) -> BTreeSet<String> {
        let mut ty = &d.params[p].1;
        while tag(ty) == "ref" {
            ty = tel(ty, 2);
        }
        let V::L(parts) = ty else { return BTreeSet::new() };
        if parts.is_empty() || st(&parts[0]) != "named" || parts.len() < 2 {
            return BTreeSet::new();
        }
        let segs: Vec<String> = st(&parts[1]).split('.').map(|s| s.to_string()).collect();
        let Some(r) = self.p.resolve(&segs, &self.file) else { return BTreeSet::new() };
        let Target::Type(tdi) = r.target else { return BTreeSet::new() };
        self.p.defs[tdi]
            .field_lifetimes
            .iter()
            .filter(|(f, _)| f == fname)
            .flat_map(|(_, lts)| lts.iter().filter(|l| l.as_str() != "static").cloned())
            .collect()
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
        // 型の抽象木には領域が残らないので、宣言の `field_lifetimes` (`'static` を含む) を見る。
        self.p.defs[tdi].field_lifetimes.iter().filter(|(f, _)| f == fname).flat_map(|(_, lts)| lts.iter().cloned()).collect()
    }
}

// ---------------------------------------------------------------- 借用を生む式 (一般化)
/// 欄の道をたどる位置: 構造体・列挙 (型定義・領域の実引数) か、領域の集合だけ。
#[derive(Clone)]
enum Cur {
    Ty(usize, Vec<String>),
    Set(BTreeSet<String>),
}

/// 戻り値へ流れる借用の出どころ。
#[derive(Clone, PartialEq, Eq, Debug)]
enum Origin {
    /// 関数の中の局所変数への借用
    Local(String),
    /// 引数 (その番号) から借りたもの
    Param(usize),
    /// 引数の参照を外して読んだ、中の省略した参照 (`x: &'a &u32` の `*x`)。呼び出しごとの新しい領域を持つ。
    Inner(usize),
    /// 引数 (その番号) の欄を読んだ値。運ぶ領域はその欄の宣言の領域 (型引数を代入したもの。`'static` を含む)。
    Field(usize, BTreeSet<String>),
}

impl<'a> Low<'a> {
    /// `*e` の `e` (再借用 `&*e` / `&mut *e` の内側)。
    fn reborrow_inner(operand: &V) -> Option<&V> {
        if nname(operand) == "Unary" && st(&nf(operand)[0]) == "*" { Some(&nf(operand)[1]) } else { None }
    }

    /// 「借用を生む式」の出どころを 1 か所で求める: `&e` / `&*e` / 借用を返す呼び出し・メソッド
    /// (`loan_sources`) と、借用を含むタプル・配列・構造体リテラル (`lit_sources`)。
    /// ブロックの値・代入・`&mut` 引数・`push` の出口はこれを使う。
    fn borrow_expr_sources(&self, e: &V) -> Vec<Src> {
        let mut v = self.loan_sources(e);
        v.extend(self.lit_sources(e));
        v
    }

    /// 式 `e` の値として外へ出る葉 (`if` / `match` / ブロックの末尾を通す)。
    fn value_leaves<'v>(e: &'v V, out: &mut Vec<&'v V>) {
        match nname(e) {
            "Block" => {
                if let Some(last) = items(&nf(e)[0]).last() {
                    match nname(last) {
                        "ExprStmt" => Self::value_leaves(&nf(last)[0], out),
                        "Let" | "Assign" => {}
                        _ => Self::value_leaves(last, out),
                    }
                }
            }
            "If" => {
                let f = nf(e);
                Self::value_leaves(&f[1], out);
                if !is_nil(&f[2]) {
                    Self::value_leaves(&f[2], out);
                }
            }
            "Match" => {
                for arm in items(&nf(e)[1]) {
                    if let Some(b) = items(arm).last() {
                        Self::value_leaves(b, out);
                    }
                }
            }
            _ => out.push(e),
        }
    }

    /// 関数 `d` の `q` 番目の引数が、省略したライフタイムの参照 (`x: &u32`) か。ヘッダが読めなければ false。
    fn elided_param(d: &Def, q: usize) -> bool {
        Self::param_text(d, q).map_or(false, |t| is_elided_ref(&t))
    }

    /// 関数 `d` の `q` 番目の引数の宣言の文面 (`x: &'a &u32`)。ヘッダが読めなければ None。
    fn param_text(d: &Def, q: usize) -> Option<String> {
        let head = d.header.split(" uses ").next()?;
        let mut i = head.find(|c| c == '(' || c == '[')?;
        if head.as_bytes()[i] == b'[' {
            i = match_close(head, i)? + 1;
        }
        if head.as_bytes().get(i) != Some(&b'(') {
            return None;
        }
        let close = match_close(head, i)?;
        let ptexts = split_top(&head[i + 1..close]);
        if ptexts.len() == d.params.len() { Some(ptexts[q].clone()) } else { None }
    }

    /// 呼び出し `t.E.V(args)` が列挙子の構成なら、列挙の名前・列挙子名・引数 (プレリュードの `Some` 等は除く)。
    fn ctor_call<'v>(&self, e: &'v V) -> Option<(String, String, &'v [V])> {
        if nname(e) != "Call" {
            return None;
        }
        let f = nf(e);
        if nname(&f[0]) != "Path" {
            return None;
        }
        let segs: Vec<String> = strs(&nf(&f[0])[0]);
        let r = self.p.resolve(&segs, &self.file)?;
        let Target::Variant(en, vn) = r.target else { return None };
        if en == "Option" || en == "Result" {
            return None;
        }
        Some((en, vn, items(&f[1])))
    }

    /// 包む形 (`Some(x)`・`Ok(x)`・`Err(x)`・`(x, y)`・`t.H { r: x }`・`t.E.V(x)`) の中身。包んだ値の借用は、中身の借用と同じ。
    fn wrapped_parts<'v>(&self, e: &'v V) -> Option<Vec<&'v V>> {
        match nname(e) {
            "TupleE" => Some(items(&nf(e)[0]).iter().collect()),
            "StructLit" => Some(items(&nf(e)[1]).iter().filter_map(|f| items(f).get(1)).collect()),
            "Call" => {
                let f = nf(e);
                let prelude = nname(&f[0]) == "Path" && matches!(strs(&nf(&f[0])[0]).as_slice(), [s] if s == "Some" || s == "Ok" || s == "Err");
                if prelude && items(&f[1]).len() == 1 {
                    return Some(vec![&items(&f[1])[0]]);
                }
                let (_, _, args) = self.ctor_call(e)?;
                Some(args.iter().collect())
            }
            _ => None,
        }
    }

    /// 型 `ty` の列挙子の構成 (`t.E.V(x)`) を、戻り値の位置として分ける。期待は列挙子の payload が運ぶ領域。
    /// 列挙子の構成でなければ false (呼び出し側が葉として扱う)。
    fn ctor_slots<'v>(&self, e: &'v V, ty: &str, out: &mut Vec<(&'v V, BTreeSet<String>, bool)>) -> bool {
        let Some((en, vn, args)) = self.ctor_call(e) else { return false };
        let Some(tdi) = self.type_of_name(&en) else { return false };
        let (rname, rargs) = type_args(ty);
        if self.type_of_name(&rname) != Some(tdi) {
            return false;
        }
        let mut subst: HashMap<String, String> = HashMap::new();
        for (g, a) in self.p.defs[tdi].generics.iter().zip(&rargs) {
            if g.0 == "life" && a.starts_with('\'') {
                subst.insert(g.1.trim_start_matches('\'').to_string(), a.trim_start_matches('\'').to_string());
            }
        }
        let decl = self.p.defs[tdi].field_lifetimes.iter().find(|f| f.0 == vn).map(|f| f.1.clone()).unwrap_or_default();
        let expected: BTreeSet<String> = decl.iter().map(|l| subst.get(l).cloned().unwrap_or_else(|| l.clone())).collect();
        let explicit = ty.contains('\'') || !self.p.defs[tdi].generics.iter().any(|g| g.0 == "life");
        out.extend(args.iter().map(|x| (x, expected.clone(), explicit)));
        true
    }

    /// 戻り値の型が、領域を運ぶ名前付きの型 (欄・列挙子の payload に参照を持つ構造体・列挙) を含むか。
    fn ret_names_lifetime_type(&self, t: &V) -> bool {
        let mut names = vec![];
        named_types(t, &mut names);
        names.iter().any(|n| {
            let segs: Vec<String> = n.split('.').map(|s| s.to_string()).collect();
            match self.p.resolve(&segs, &self.file).map(|r| r.target) {
                Some(Target::Type(tdi)) => self.p.defs[tdi].field_lifetimes.iter().any(|(_, l)| !l.is_empty()),
                _ => false,
            }
        })
    }

    /// 型の文面 `ty` が、ライフタイムを持たない名前付きの型か。その欄・列挙子の領域は宣言どおりの具体的な領域
    /// (`'static` など) で、省略された出力ではない。
    fn named_without_life_generics(&self, ty: &str) -> bool {
        let (n, _) = type_args(ty);
        match self.type_of_name(&n) {
            Some(t) => !self.p.defs[t].generics.iter().any(|g| g.0 == "life"),
            None => false,
        }
    }

    /// 式 `e` が運ぶ借用の出どころ。`inits` は 1 回だけ束縛された `let` の初期化式 (変数をたどる)。
    fn origins(&self, e: &V, d: &Def, inits: &Inits, depth: usize, out: &mut Vec<Origin>) {
        if depth > 8 {
            return;
        }
        // 包んだ値 (`Some(x)` など) の借用は、中身の借用そのもの。変数に束縛されたときもそのまま運ぶ。
        if let Some(parts) = self.wrapped_parts(e) {
            for p in parts {
                self.origins(p, d, inits, depth + 1, out);
            }
            return;
        }
        let mut leaves = vec![];
        Self::value_leaves(e, &mut leaves);
        let saved = inits.pos.get();
        for leaf in leaves {
            inits.pos.set(inits.at(leaf));
            // 局所のクロージャ・関数値の呼び出し `f(x)`: 結果の借用が引数のどれから来るかはヘッダに頼れないので、
            // 全部の引数の借用が結果へ流れるとみなす (fail-closed)。
            if nname(leaf) == "Call" && nname(&nf(leaf)[0]) == "Path" {
                let segs = strs(&nf(&nf(leaf)[0])[0]);
                if segs.len() == 1 && self.locals.contains(&segs[0]) {
                    for a in items(&nf(leaf)[1]) {
                        self.origins(a, d, inits, depth + 1, out);
                    }
                }
            }
            for s in self.borrow_expr_sources(leaf) {
                self.src_origins(&s, d, inits, depth, out);
                inits.pos.set(inits.at(leaf));
            }
        }
        inits.pos.set(saved);
    }

    /// 借用の出どころ `s` から、戻り値へ流れる出どころ (局所・引数) を `out` に足す。`origins` の本体。
    fn src_origins(&self, s: &Src, d: &Def, inits: &Inits, depth: usize, out: &mut Vec<Origin>) {
        // 欄の読み戻し `p.f.g` (`Stored`) の道。省略した参照の引数では、先頭の欄の領域だけが引数の借用に当たる。
        let read_path = match s {
            Src::Stored { path, .. } => path.clone(),
            _ => vec![],
        };
        let read_field = read_path.first().cloned();
        match s {
            Src::Place { root, path: _, .. } => {
                if root == "$temp" {
                    // 一時値の借用は文の終わりで死ぬ。戻り値へ流れれば、局所の借用と同じく寿命が足りない。
                    out.push(Origin::Local(root.clone()));
                } else if let Some(q) = d.params.iter().position(|p| &p.0 == root).filter(|_| !inits.has(root)) {
                    if tag(&d.params[q].1) == "ref" || matches!(s, Src::Place { path, .. } if path.iter().any(|x| x == "*")) {
                        out.push(Origin::Param(q));
                    } else {
                        out.push(Origin::Local(root.clone()));
                    }
                } else if let Some(init) = inits.plain(root) {
                    // 借用を持つ変数 (`let y = x`) を通した借用は、その変数の出どころ。
                    let mut sub = vec![];
                    self.origins(init, d, inits, depth + 1, &mut sub);
                    if sub.is_empty() { out.push(Origin::Local(root.clone())) } else { out.extend(sub) }
                }
            }
            Src::Alias(v) | Src::Stored { root: v, .. } => {
                if let Some(q) = d.params.iter().position(|p| &p.0 == v).filter(|_| !inits.has(v)) {
                    // 省略した参照の引数の欄を読み戻した値は、欄が名前付き (`'static` でない) の領域を持つときだけ
                    // その引数の領域から来る (`self.name: &'static str` の複写は引数の借用ではない)。
                    let carries = match &read_field {
                        Some(f) if f != "*" && Self::elided_param(d, q) => !self.field_named_lifetimes(d, q, f).is_empty(),
                        _ => true,
                    };
                    let fld = match &read_field {
                        Some(f) if f != "*" && carries => self.field_region_path(d, q, &read_path),
                        _ => None,
                    };
                    if let Some(set) = fld {
                        out.push(Origin::Field(q, set));
                    } else if carries {
                        out.push(Origin::Param(q));
                    }
                } else if let Some(o) = self.bound_origin(d, inits, v) {
                    // 分解・腕のパターンで取り出した名前は、取り出し元の欄の宣言の領域を引き継ぐ。
                    out.push(o);
                } else if let Some(o) = self.alias_field_origin(d, inits, v, &read_path, 0) {
                    // `let inner = &q.p` を通した欄の読み出し `inner.y` は、`q.p.y` の宣言の領域。
                    out.push(o);
                    self.assigned_origins(v, d, inits, depth, out);
                } else if let Some(init) = inits.plain(v) {
                    self.origins(init, d, inits, depth + 1, out);
                    self.assigned_origins(v, d, inits, depth, out);
                } else if let Some(init) = inits.tilde(v) {
                    // 呼び出し・メソッド呼び出しなどを調べる `match` の腕・分解の束縛。受け手の自動借用を含むので、
                    // 引数から来る借用だけを引き継ぐ (局所の借用は E3030 の根拠にしない)。
                    let mut sub = vec![];
                    self.origins(init, d, inits, depth + 1, &mut sub);
                    out.extend(sub.into_iter().filter(|o| matches!(o, Origin::Param(_))));
                    self.assigned_origins(v, d, inits, depth, out);
                }
            }
            // 参照を外した値 (`*x`) は、指す先の値が持つ借用 (`x: & &u32` の内側の領域) を運ぶ。
            Src::Deref(inner) => {
                // 指す先の値を取り出すので、局所そのものを借りたことにはならない (引数から来る借用だけを引き継ぐ)。
                let mut sub = vec![];
                for i in inner {
                    self.src_origins(i, d, inits, depth + 1, &mut sub);
                }
                for o in sub {
                    match o {
                        Origin::Param(q) if Self::param_text(d, q).map_or(false, |t| inner_elided(&t)) => out.push(Origin::Inner(q)),
                        Origin::Param(_) | Origin::Field(..) => out.push(o),
                        _ => {}
                    }
                }
            }
            Src::Contents { .. } => {}
        }
    }

    /// 局所 `v` が引数の場所への参照 (`let v = &q.p`、またはその別名) なら、そこからの欄の読み出し `v.<read>` の
    /// 欄の宣言の領域 (`q.p.<read>`)。読み出す欄が無い・決まらないときは None (従来の判定)。
    fn alias_field_origin(&self, d: &Def, inits: &Inits, v: &str, read: &[String], depth: usize) -> Option<Origin> {
        if read.is_empty() || depth > 8 {
            return None;
        }
        let init = inits.plain(v)?;
        let (q, mut path) = match nname(init) {
            "Unary" if matches!(st(&nf(init)[0]).as_str(), "&" | "&mut") => {
                let (root, path) = self.place_of(&nf(init)[1])?;
                if inits.has(&root) {
                    return None;
                }
                (d.params.iter().position(|p| p.0 == root)?, path)
            }
            "Path" if strs(&nf(init)[0]).len() == 1 => {
                let w = &strs(&nf(init)[0])[0];
                return self.alias_field_origin(d, inits, w, read, depth + 1);
            }
            _ => return None,
        };
        path.extend(read.iter().cloned());
        let first = path.iter().find(|f| f.as_str() != "*")?;
        if Self::elided_param(d, q) && self.field_named_lifetimes(d, q, first).is_empty() {
            return None;
        }
        self.field_region_path(d, q, &path).map(|set| Origin::Field(q, set))
    }

    /// 局所の変数 `v` への代入 `v = e` の右辺が運ぶ欄の領域 (流れに依らず足す。`let mut r = self.fst; r = self.snd`)。
    fn assigned_origins(&self, v: &str, d: &Def, inits: &Inits, depth: usize, out: &mut Vec<Origin>) {
        let Some(vs) = inits.assigns.get(v) else { return };
        for a in vs {
            let mut sub = vec![];
            self.origins(a, d, inits, depth + 1, &mut sub);
            out.extend(sub.into_iter().filter(|o| matches!(o, Origin::Field(..))));
        }
    }

    /// 引数 `q` の型 (`&t.Pair<'a, 'b>`) から、欄の道をたどる出発点。型が名前付きのライフタイム引数を全部書いていない
    /// ときは None (従来の判定)。
    fn param_cursor(&self, d: &Def, q: usize) -> Option<Cur> {
        let text = Self::param_text(d, q)?;
        let mut ty = text.split_once(':').map_or(text.as_str(), |x| x.1).trim();
        while let Some(t) = ty.strip_prefix('&') {
            let t = t.trim_start();
            let t = match t.strip_prefix('\'') {
                Some(r) => r.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_').trim_start(),
                None => t,
            };
            ty = t.strip_prefix("mut ").unwrap_or(t).trim_start();
        }
        let (name, args) = type_args(ty);
        let tdi = self.type_of_name(&name)?;
        let n = self.p.defs[tdi].generics.iter().filter(|g| g.0 == "life").count();
        let mut given: Vec<String> = args.iter().filter(|a| a.starts_with('\'')).map(|a| a.trim_start_matches('\'').to_string()).collect();
        // 領域引数を 1 つも書かない (省略した) 型は、領域ごとに互いに関係の無い新しい領域として扱う (明示と同じ判定)。
        if n > 0 && given.is_empty() {
            given = (0..n).map(|k| format!("_e{}_{}", q, k)).collect();
        }
        if n == 0 || given.len() != n {
            return None;
        }
        Some(Cur::Ty(tdi, given))
    }

    /// 宣言の領域名 `flts` を、型 `tdi` の実引数 `given` (宣言の順) で置き換える。
    fn subst_lifetimes(&self, tdi: usize, given: &[String], flts: &[String]) -> Vec<String> {
        let lifes: Vec<&(String, String, V)> = self.p.defs[tdi].generics.iter().filter(|g| g.0 == "life").collect();
        flts.iter()
            .map(|l| match lifes.iter().position(|g| g.1.trim_start_matches('\'') == l.as_str()) {
                Some(k) => given[k].clone(),
                None => l.clone(),
            })
            .collect()
    }

    /// 位置 `c` の持つ領域の集合。
    fn cur_set(c: &Cur) -> BTreeSet<String> {
        match c {
            Cur::Ty(_, g) => g.iter().cloned().collect(),
            Cur::Set(s) => s.clone(),
        }
    }

    /// 欄または列挙子の payload の型 `ty` (宣言の領域 `flts` を置き換えた `sub`) の次の位置。
    /// 型が領域引数だけを取る名前付きの型ならその型の位置、そうでなければ領域の集合だけ。領域を運ばなければ None。
    fn next_cursor(&self, ty: Option<&V>, flts: &[String], sub: Vec<String>) -> Option<Cur> {
        if flts.is_empty() {
            return None;
        }
        if let Some(V::L(parts)) = ty {
            if parts.len() >= 2 && st(&parts[0]) == "named" {
                let segs: Vec<String> = st(&parts[1]).split('.').map(|s| s.to_string()).collect();
                if let Some(Target::Type(t2)) = self.p.resolve(&segs, &self.file).map(|r| r.target) {
                    if self.p.defs[t2].generics.iter().filter(|g| g.0 == "life").count() == sub.len() {
                        return Some(Cur::Ty(t2, sub));
                    }
                }
            }
        }
        Some(Cur::Set(sub.into_iter().collect()))
    }

    /// 位置 `c` の欄 `f` の位置。
    fn step_field(&self, c: &Cur, f: &str) -> Option<Cur> {
        let Cur::Ty(tdi, given) = c else { return Some(c.clone()) };
        let decl = &self.p.defs[*tdi];
        let flts = &decl.field_lifetimes.iter().find(|x| x.0 == f)?.1;
        let sub = self.subst_lifetimes(*tdi, given, flts);
        let ty = decl.fields.iter().find(|x| x.0 == f).map(|x| &x.1);
        self.next_cursor(ty, flts, sub)
    }

    /// 位置 `c` (列挙) の列挙子 `vn` の `i` 番目 (全部で `n` 個) の payload の位置。
    fn step_variant(&self, c: &Cur, vn: &str, i: usize, n: usize) -> Option<Cur> {
        let Cur::Ty(tdi, given) = c else { return Some(c.clone()) };
        let decl = &self.p.defs[*tdi];
        let flts = &decl.field_lifetimes.iter().find(|x| x.0 == vn)?.1;
        let sub = self.subst_lifetimes(*tdi, given, flts);
        let ty = if n == 1 { decl.variants.iter().find(|x| x.0 == vn).and_then(|x| x.1.get(i)) } else { None };
        self.next_cursor(ty, flts, sub)
    }

    /// 引数 `q` から欄の道 `path` をたどった位置。`*` は読み飛ばす。途中で決まらなくなったら、決まった所までの位置
    /// (欄の領域の上位集合。段が増えても今までより緩くはならない)。最初の欄が決まらなければ None。
    fn path_cursor(&self, d: &Def, q: usize, path: &[String]) -> Option<Cur> {
        let mut c = self.param_cursor(d, q)?;
        let mut any = false;
        for f in path.iter().filter(|f| f.as_str() != "*") {
            match self.step_field(&c, f) {
                Some(n) => {
                    c = n;
                    any = true;
                }
                None => {
                    if !any {
                        return None;
                    }
                    break;
                }
            }
        }
        any.then_some(c)
    }

    /// 引数 `q` の欄の道 `path` (`self.inner.snd`) の宣言の領域を、型の実引数で置き換えた集合。何段でもたどる。
    /// 決まらないとき (領域引数を全部書いていない・欄が領域を持たない) は None (従来の判定)。
    fn field_region_path(&self, d: &Def, q: usize, path: &[String]) -> Option<BTreeSet<String>> {
        let s = Self::cur_set(&self.path_cursor(d, q, path)?);
        (!s.is_empty()).then_some(s)
    }

    /// パターン `pat` の中の名前 `name` が取り出す位置。`c` はパターンが当たる値の位置。
    fn pat_cursor(&self, pat: &V, name: &str, c: &Cur) -> Option<Cur> {
        let whole = || Cur::Set(Self::cur_set(c));
        match nname(pat) {
            "PBind" => (st(&nf(pat)[0]) == name).then(|| c.clone()),
            "PRef" => self.pat_cursor(&nf(pat)[0], name, c),
            "PTuple" => items(&nf(pat)[0]).iter().find_map(|x| self.pat_cursor(x, name, &whole())),
            "PStruct" => items(&nf(pat)[1]).iter().find_map(|f| {
                let fi = items(f);
                let nc = match c {
                    Cur::Ty(..) => self.step_field(c, &st(&fi[0])).unwrap_or_else(whole),
                    Cur::Set(_) => c.clone(),
                };
                self.pat_cursor(&fi[1], name, &nc)
            }),
            "PVariant" => {
                let segs = strs(&nf(pat)[0]);
                let its = items(&nf(pat)[1]);
                let var = self.p.resolve(&segs, &self.file).and_then(|r| match r.target {
                    Target::Variant(en, vn) => Some((en, vn)),
                    _ => None,
                });
                let user = match (&var, c) {
                    (Some((en, _)), Cur::Ty(tdi, _)) => self.type_of_name(en) == Some(*tdi),
                    _ => false,
                };
                its.iter().enumerate().find_map(|(i, x)| {
                    let nc = if user { self.step_variant(c, &var.as_ref().unwrap().1, i, its.len()).unwrap_or_else(whole) } else { whole() };
                    self.pat_cursor(x, name, &nc)
                })
            }
            _ => None,
        }
    }

    /// 分解・腕のパターンで取り出した名前 `v` の出どころ。取り出し元が引数の場所 (`*self`・`self.opt`) なら、
    /// パターンが指す欄の宣言の領域だけを運ぶ。決まらなければ None (従来の判定)。
    fn bound_origin(&self, d: &Def, inits: &Inits, v: &str) -> Option<Origin> {
        let b = inits.visible(v)?;
        let (pat, init) = (b.pat?, b.init?);
        let (root, path) = self.place_of(init)?;
        let q = d.params.iter().position(|p| p.0 == root)?;
        if inits.has(&root) {
            return None;
        }
        let c = self.path_cursor(d, q, &path).or_else(|| if path.iter().all(|f| f == "*") { self.param_cursor(d, q) } else { None })?;
        let set = Self::cur_set(&self.pat_cursor(pat, v, &c)?);
        (!set.is_empty()).then_some(Origin::Field(q, set))
    }

    /// 引数の欄への代入 `p.f.g = e` が、欄の宣言の領域より短い領域の借用を入れない (E3010)。
    /// 代入先の欄の宣言の領域 (型引数を代入したもの) と、右辺が運ぶ領域 (欄の読み出し・引数) が合うかを見る。
    fn check_field_assign(&self, d: &Def, body: &V) -> Vec<(usize, String)> {
        let Some(head) = d.header.split(" uses ").next() else { return vec![] };
        let gen_text = match head.find(|c| c == '(' || c == '[') {
            Some(i) if head.as_bytes()[i] == b'[' => match match_close(head, i) {
                Some(j) => head[i..=j].to_string(),
                None => return vec![],
            },
            _ => String::new(),
        };
        let declared = |h: &String| gen_text.contains(&format!("'{}:", h)) || gen_text.contains(&format!("'{} :", h));
        let mut all = vec![];
        walk(body, &mut all);
        let inits = Inits::build(body);
        let mut out = vec![];
        for n in &all {
            if nname(n) != "Assign" || nf(n).len() < 2 {
                continue;
            }
            let (target, value) = (&nf(n)[0], &nf(n)[1]);
            let Some((root, path)) = self.place_of(target) else { continue };
            let fields: Vec<String> = path.into_iter().filter(|f| f != "*").collect();
            let Some(q) = d.params.iter().position(|p| p.0 == root) else { continue };
            inits.pos.set(inits.at(n));
            if fields.is_empty() || inits.has(&root) {
                continue;
            }
            let Some(want) = self.field_region_path(d, q, &fields) else { continue };
            let mut os = vec![];
            self.origins(value, d, &inits, 0, &mut os);
            for o in os {
                let (have, src) = match o {
                    Origin::Field(q2, have) => (have, q2),
                    Origin::Param(q2) => {
                        let Some(t) = Self::param_text(d, q2) else { continue };
                        if t.contains("'static") {
                            continue;
                        }
                        (lifetimes(&t), q2)
                    }
                    _ => continue,
                };
                if have.is_empty() || have.contains("static") || have.intersection(&want).next().is_some() || have.iter().any(|h| declared(h)) {
                    continue;
                }
                let names = |s: &BTreeSet<String>| s.iter().map(|h| format!("'{}", h)).collect::<Vec<_>>().join(", ");
                out.push((
                    nlineno(n),
                    format!(
                        "{}: stores a borrow with lifetime {} (from argument `{}`) into `{}.{}`, whose declared lifetime is {}; `{}` is not declared to outlive it",
                        d.name, names(&have), d.params[src].0, root, fields.join("."), names(&want), names(&have),
                    ),
                ));
                break;
            }
        }
        out
    }

    /// 名前 (`t.Pair`) が指す型定義。型でなければ None。
    fn type_of_name(&self, name: &str) -> Option<usize> {
        let segs: Vec<String> = name.split('.').map(|s| s.to_string()).collect();
        match self.p.resolve(&segs, &self.file)?.target {
            Target::Type(t) => Some(t),
            _ => None,
        }
    }

    /// 戻り値の式 `e` を、戻り値の型 `ty` の位置ごとに分ける。各葉に「その葉が運んでよい領域の集合」を付ける。
    /// タプルは型の成分ごとに、構造体リテラルは欄の宣言の領域を戻り値の型引数で置き換えた集合ごとに、
    /// `Some` / `Ok` / `Err` は包まれる型の位置へ送る。それ以外の葉は戻り値の型の全領域を期待とする。
    /// 形が型と合わないとき (型の名前が違う・成分の数が違う) は期待を空にする (fail-closed)。
    fn ret_slots<'v>(&self, e: &'v V, ty: &str, out: &mut Vec<(&'v V, BTreeSet<String>, bool)>) {
        // 戻り値の型の後ろの修飾 (`-> core.fmt.Formatter<'a> const`) は型ではない。
        let ty = ty.trim();
        let ty = ty.strip_suffix(" const").unwrap_or(ty).trim_end();
        // 型の文面が領域を明示しているか (`'a` / `'static`)。明示がなければ、出力の領域は省略規則 (4.3) で決まる。
        let explicit = ty.contains('\'') || self.named_without_life_generics(ty);
        // 型の引数 `T<a, b>` を (名前, 引数)。
        let generic = |t: &str| -> Option<(String, Vec<String>)> {
            let i = t.find('<')?;
            let inner = t.strip_suffix('>')?.get(i + 1..)?;
            Some((t[..i].trim().to_string(), split_top(inner)))
        };
        match nname(e) {
            "TupleE" => {
                let elems = items(&nf(e)[0]);
                let comps = ty.strip_prefix('(').and_then(|t| t.strip_suffix(')')).map(split_top);
                match comps {
                    Some(comps) if !elems.is_empty() && comps.len() == elems.len() => {
                        for (x, c) in elems.iter().zip(&comps) {
                            self.ret_slots(x, c, out);
                        }
                    }
                    _ => out.extend(elems.iter().map(|x| (x, BTreeSet::new(), explicit))),
                }
            }
            "StructLit" => {
                let Some(tdi) = self.type_of_name(&st(&nf(e)[0])) else {
                    out.extend(items(&nf(e)[1]).iter().filter_map(|f| items(f).get(1)).map(|x| (x, BTreeSet::new(), explicit)));
                    return;
                };
                let (rname, rargs) = generic(ty).unwrap_or((ty.to_string(), vec![]));
                if self.type_of_name(&rname) != Some(tdi) {
                    out.extend(items(&nf(e)[1]).iter().filter_map(|f| items(f).get(1)).map(|x| (x, BTreeSet::new(), explicit)));
                    return;
                }
                // 型の仮引数 (宣言の順) と戻り値の型引数を並べて、ライフタイムの置き換えを作る。
                let mut subst: HashMap<String, String> = HashMap::new();
                for (g, a) in self.p.defs[tdi].generics.iter().zip(&rargs) {
                    let (gn, an) = (g.1.trim_start_matches('\''), a.trim_start_matches('\''));
                    if g.0 == "life" && a.starts_with('\'') {
                        subst.insert(gn.to_string(), an.to_string());
                    }
                }
                for fld in items(&nf(e)[1]) {
                    let it = items(fld);
                    if it.len() < 2 {
                        continue;
                    }
                    let fname = st(&it[0]);
                    let flts = self.p.defs[tdi].field_lifetimes.iter().find(|f| f.0 == fname).map(|f| f.1.clone()).unwrap_or_default();
                    // 欄に名前付きの領域がなければ (型引数で決まる欄) 戻り値の全領域を期待とする。
                    let expected: BTreeSet<String> = if flts.is_empty() {
                        lifetimes_with_static(ty)
                    } else {
                        flts.iter().map(|l| subst.get(l).cloned().unwrap_or_else(|| l.clone())).collect()
                    };
                    out.push((&it[1], expected, explicit));
                }
            }
            "Call" => {
                let f = nf(e);
                let args = items(&f[1]);
                // 包む型の名前と、包まれる値が入る型引数の位置 (`Some`: `Option<T>` の 0、`Ok`: `Result<T, E>` の 0、`Err`: 1)。
                let segs = if nname(&f[0]) == "Path" { strs(&nf(&f[0])[0]) } else { vec![] };
                let ctor = match (segs.as_slice(), args.len()) {
                    ([s], 1) if s.as_str() == "Some" => Some(("Option", 0)),
                    ([s], 1) if s.as_str() == "Ok" => Some(("Result", 0)),
                    ([s], 1) if s.as_str() == "Err" => Some(("Result", 1)),
                    _ => None,
                };
                let inner = ctor.and_then(|(name, k)| {
                    let (n, a) = generic(ty)?;
                    if n == name { a.get(k).cloned() } else { None }
                });
                match inner {
                    Some(t) => self.ret_slots(&args[0], &t, out),
                    None => {
                        if !self.ctor_slots(e, ty, out) {
                            out.push((e, lifetimes_with_static(ty), explicit));
                        }
                    }
                }
            }
            _ => out.push((e, lifetimes_with_static(ty), explicit)),
        }
    }

    /// 戻り値の式が、(a) 局所変数への借用を運ぶ (E3030)、(b) ヘッダの戻り値の領域と無関係な (または
    /// `'static` でない) 引数の借用を運ぶ (E3010)。`&x`・引数そのものを返す形に加えて、再借用・関数経由・
    /// タプル・構造体・`Some(..)` に包んだ形も同じ関数 (`origins`) で求める。
    /// 組・構造体・`Some` / `Ok` / `Err` は `ret_slots` で位置ごとに照らす (`(y, x)` を `(&'a, &'b)` で返す形を通さない)。
    /// 返り値は (診断コード, 行, 文面)。
    fn check_return_flow(&self, d: &Def, body: &V) -> Vec<(&'static str, usize, String)> {
        let Some(head) = d.header.split(" uses ").next() else { return vec![] };
        let Some(mut i) = head.find(|c| c == '(' || c == '[') else { return vec![] };
        let gen_text = if head.as_bytes()[i] == b'[' {
            let Some(j) = match_close(head, i) else { return vec![] };
            let g = head[i..=j].to_string();
            i = j + 1;
            g
        } else {
            String::new()
        };
        if head.as_bytes().get(i) != Some(&b'(') {
            return vec![];
        }
        let Some(close) = match_close(head, i) else { return vec![] };
        let ptexts = split_top(&head[i + 1..close]);
        if ptexts.len() != d.params.len() {
            return vec![];
        }
        let Some((_, ret)) = head[close + 1..].split_once("->") else { return vec![] };
        let want = lifetimes(ret);
        let static_ret = ret.contains("'static");
        if want.is_empty() && !static_ret && !ret.contains('&') && !self.ret_contains_ref(d) && !self.ret_names_lifetime_type(&d.ret) {
            return vec![];
        }
        let mut all = vec![];
        walk(body, &mut all);
        // 局所の束縛は、使う位置から見える一番内側・後ろのものを束縛点ごとに選ぶ (同名の束縛・引数と同名の束縛も区別する)。
        let inits = Inits::build(body);
        let mut vals: Vec<&V> = vec![];
        Self::value_leaves(body, &mut vals);
        for v in &all {
            if nname(v) == "Return" && !is_nil(&nf(v)[0]) {
                Self::value_leaves(&nf(v)[0], &mut vals);
            }
        }
        // 葉ごとに、その葉が運んでよい領域 (戻り値の型の位置) を付ける。
        let mut slots: Vec<(&V, BTreeSet<String>, bool)> = vec![];
        for v in vals {
            self.ret_slots(v, ret, &mut slots);
        }
        // `Err(e)?` は、エラーの値 `e` を戻り値の `Result<_, E>` のエラーの位置へ返す (`?` の早期の戻り)。
        if let Some(et) = ret.trim().strip_prefix("Result<").and_then(|r| r.strip_suffix('>')).and_then(|r| split_top(r).get(1).cloned()) {
            for n in &all {
                if nname(n) == "Try" {
                    if let Some(p) = err_payload(&nf(n)[0]) {
                        self.ret_slots(p, &et, &mut slots);
                    }
                }
            }
        }
        let mut out = vec![];
        let mut seen: BTreeSet<(usize, String)> = BTreeSet::new();
        for (v, expected, explicit) in slots {
            // `&'L T` を返す形で引数そのものを返すのは check_return_lifetime が見る。
            if ret.trim().starts_with('&') && nname(v) == "Path" && strs(&nf(v)[0]).len() == 1 && d.params.iter().any(|p| p.0 == strs(&nf(v)[0])[0]) {
                continue;
            }
            let mut os = vec![];
            self.origins(v, d, &inits, 0, &mut os);
            for o in os {
                match o {
                    Origin::Local(name) => {
                        if seen.insert((nlineno(v), name.clone())) {
                            out.push((
                                "E3030",
                                nlineno(v),
                                if name == "$temp" {
                                    "returns a reference to a temporary value, which does not live long enough".to_string()
                                } else {
                                    format!("returns a reference to the local variable `{}`, which does not live long enough", name)
                                },
                            ));
                        }
                    }
                    Origin::Inner(q) => {
                        // 名前付きの外側の参照の中の省略した参照は、どの名前付きの出力の領域にも収まらない。
                        if explicit && !expected.is_empty() && seen.insert((nlineno(v), d.params[q].0.clone())) {
                            out.push((
                                "E3010",
                                nlineno(v),
                                format!(
                                    "{}: the returned value carries the inner reference of argument `{}` (`{}`), whose lifetime is elided (a new region per call); the return type `{}` names a lifetime it does not outlive",
                                    d.name, d.params[q].0, ptexts[q], ret.trim(),
                                ),
                            ));
                        }
                    }
                    Origin::Field(q, have) => {
                        // 欄の領域が `'static`、または戻り値の名前付きの領域・宣言した包含関係に収まるときだけ通す。
                        // 戻り値が領域を名指さない (省略した出力) ときは、引数全体の判定 (`Param`) と同じく省略規則に任せる。
                        if have.contains("static") || expected.is_empty() || have.intersection(&expected).next().is_some() {
                            continue;
                        }
                        if have.iter().any(|h| gen_text.contains(&format!("'{}:", h)) || gen_text.contains(&format!("'{} :", h))) {
                            continue;
                        }
                        if seen.insert((nlineno(v), d.params[q].0.clone())) {
                            out.push((
                                "E3010",
                                nlineno(v),
                                format!(
                                    "{}: the returned value carries the field borrow of argument `{}` (`{}`), whose lifetime {} is not the return type's `{}`; `{}` is not declared to outlive it",
                                    d.name,
                                    d.params[q].0,
                                    ptexts[q],
                                    have.iter().map(|h| format!("'{}", h)).collect::<Vec<_>>().join(", "),
                                    ret.trim(),
                                    have.iter().map(|h| format!("'{}", h)).collect::<Vec<_>>().join(", ")
                                ),
                            ));
                        }
                    }
                    Origin::Param(q) => {
                        let have = lifetimes(&ptexts[q]);
                        if ptexts[q].contains("'static") {
                            continue;
                        }
                        // 省略した入力の参照 (`x: &u32`) は、名前付きの出力の領域 (`'b`・`'static`) に収まらない。
                        // 省略した出力 (`-> &u32`) へ流れるのは省略規則 (4.3) の正規形なので通す。
                        if have.is_empty() {
                            if carries_elided_ref(&ptexts[q]) && explicit && !expected.is_empty() && seen.insert((nlineno(v), d.params[q].0.clone())) {
                                out.push((
                                    "E3010",
                                    nlineno(v),
                                    format!(
                                        "{}: the returned value carries a borrow from argument `{}` (`{}`), whose lifetime is elided (a new region per call); the return type `{}` names a lifetime it does not outlive",
                                        d.name,
                                        d.params[q].0,
                                        ptexts[q],
                                        ret.trim(),
                                    ),
                                ));
                            }
                            continue;
                        }
                        if have.intersection(&expected).next().is_some() {
                            continue;
                        }
                        if have.iter().any(|h| gen_text.contains(&format!("'{}:", h)) || gen_text.contains(&format!("'{} :", h))) {
                            continue;
                        }
                        if seen.insert((nlineno(v), d.params[q].0.clone())) {
                            out.push((
                                "E3010",
                                nlineno(v),
                                format!(
                                    "{}: the returned value carries a borrow from argument `{}` (`{}`), but the return type `{}` does not name its lifetime; `{}` is not declared to outlive it",
                                    d.name,
                                    d.params[q].0,
                                    ptexts[q],
                                    ret.trim(),
                                    have.iter().map(|h| format!("'{}", h)).collect::<Vec<_>>().join(", ")
                                ),
                            ));
                        }
                    }
                }
            }
        }
        out
    }
}

impl<'a> Low<'a> {
    /// 関数 `d` の本体にあるクロージャ式 (`fn(x: &u32) -> &'static u32 capture [] { .. }`) の戻り値を、
    /// 関数と同じ省略・領域の規則で検査する (E3020 / E3010)。クロージャは字面のヘッダを 5 番目の要素に持つ
    /// (`parser` の `closure`)。その字面から仮の定義を作り、`check_return_lifetime` / `check_return_flow` に通す。
    fn check_closure_returns(&self, d: &Def, body: &V) -> Vec<(&'static str, usize, String)> {
        let mut all = vec![];
        walk(body, &mut all);
        let mut out = vec![];
        for cl in all {
            if nname(cl) != "Closure" || nf(cl).len() < 5 {
                continue;
            }
            let f = nf(cl);
            let cd = Def {
                name: format!("{}.<closure>", d.name),
                file: d.file.clone(),
                header: st(&f[4]),
                params: crate::program::conv_params(&f[0]),
                ret: f[1].clone(),
                body: Some(f[3].clone()),
                ..Def::default()
            };
            // 戻り値の参照は、引数の参照がちょうど 1 つのときだけ省略できる (関数の E3020 と同じ)。
            let refs = cd.params.iter().filter(|p| p.1.dump().contains("ref")).count();
            let ret_txt = cd.header.split("->").nth(1).unwrap_or("");
            if cd.ret.dump().contains("ref") && !ret_txt.contains('\'') && refs != 1 {
                out.push((
                    "E3020",
                    nlineno(cl),
                    format!("{}: lifetime of the returned reference cannot be elided; declare lifetimes", cd.name),
                ));
            }
            for (line, msg) in self.check_return_lifetime(&cd, &f[3]) {
                out.push(("E3010", line, msg));
            }
            out.extend(self.check_return_flow(&cd, &f[3]));
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
    /// 関数 `di` の束縛のうち、型が決まっていて借用を持ち得ないものの名前。同じ名前の束縛が
    /// 1 つでも (または型が決まらない) それ以外なら入れない。
    fn plain_binds(&self, di: usize) -> HashSet<String> {
        let mut yes: HashSet<String> = HashSet::new();
        let mut no: HashSet<String> = HashSet::new();
        for ((d, _, name), ty) in self.let_tys.iter() {
            if *d != di {
                continue;
            }
            if self.ty_borrow_free(ty) {
                yes.insert(name.clone());
            } else {
                no.insert(name.clone());
            }
        }
        yes.retain(|n| !no.contains(n));
        yes
    }

    /// 型が借用を持ち得ない (参照・ポインタ・クロージャ・型パラメータ・ライフタイム付きの型を含まない)。
    fn ty_borrow_free(&self, t: &crate::types::Ty) -> bool {
        use crate::types::Ty;
        match t {
            Ty::Prim(_) | Ty::Unit | Ty::Never => true,
            Ty::Tuple(xs) => xs.iter().all(|x| self.ty_borrow_free(x)),
            Ty::Array(x, _) | Ty::Slice(x) => self.ty_borrow_free(x),
            Ty::Named(n, args) => {
                if let Some(&i) = self.p.types.get(n) {
                    if self.def(i).generics.iter().any(|g| g.0 != "type") {
                        return false;
                    }
                }
                args.iter().all(|x| self.ty_borrow_free(x))
            }
            _ => false,
        }
    }

    pub(super) fn borrowck(&mut self, di: usize) {
        let d = self.def(di);
        let Some(body) = &d.body else { return };
        // 局所変数 (引数・`let`・パターン・`for`・`match` の束縛)
        let mut locals: HashSet<String> = d.params.iter().map(|p| p.0.clone()).collect();
        let scope_seed: Vec<String> = d.params.iter().map(|p| p.0.clone()).collect();
        let mut all = vec![];
        walk(body, &mut all);
        // 束縛の総数 (仮引数を含む) を名前ごとに数える。
        let mut total: HashMap<String, usize> = scope_seed.iter().map(|p| (p.clone(), 1)).collect();
        for n in &all {
            match nname(n) {
                "Let" | "For" | "With" => {
                    let mut b = vec![];
                    pat_binds(&nf(n)[0], &mut b);
                    for x in &b {
                        *total.entry(x.clone()).or_insert(0) += 1;
                    }
                    locals.extend(b);
                }
                "Match" => {
                    for arm in items(&nf(n)[1]) {
                        let mut b = vec![];
                        pat_binds(&items(arm)[0], &mut b);
                        for x in &b {
                            *total.entry(x.clone()).or_insert(0) += 1;
                        }
                        locals.extend(b);
                    }
                }
                _ => {}
            }
        }
        // 所有物の局所: 型を記録されていて、借用を持ち得ない型の束縛点 (束縛の位置 + 名前)。名前でなく束縛点で数えるので、
        // 同じ名前の兄弟ブロックの別の束縛 (`&str` など) が、所有物の束縛の寿命の終わりを消さない。
        // 参照を通した分解 (`match &o { Some(p) => .. }`) は型を記録されないので、入らない (黙る)。
        let mut owned: HashSet<(usize, String)> = HashSet::new();
        let mut nonref: HashSet<(usize, String)> = HashSet::new();
        let mut with_free: HashSet<usize> = HashSet::new();
        for ((d, line, name), ty) in self.let_tys.iter() {
            if *d != di {
                continue;
            }
            if name == "$with_val" {
                if self.ty_borrow_free(ty) {
                    with_free.insert(*line);
                }
                continue;
            }
            if self.ty_borrow_free(ty) {
                owned.insert((*line, name.clone()));
            }
            if !matches!(ty, crate::types::Ty::Ref(..) | crate::types::Ty::Unknown | crate::types::Ty::Var(_) | crate::types::Ty::Param(_)) {
                nonref.insert((*line, name.clone()));
            }
        }
        let plain = self.plain_binds(di);
        let mut low = Low {
            p: self.p,
            res: self.call_res.clone(),
            susp: self.susp_lines.clone(),
            cur: di,
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
            plain,
            owned,
            nonref,
            with_sites: HashSet::new(),
            with_closes: HashSet::new(),
            with_free,
        };
        // 参照を別の引数の指す先 (Vec 要素・構造体欄) に保存する際の
        // ライフタイム不一致 (d22)。
        for (line, msg) in low.check_lifetime_store(di, body) {
            self.tdiag("E3010", msg, line);
        }
        // 引数の欄への代入が、欄の宣言の領域と合わない借用を入れる。
        for (line, msg) in low.check_field_assign(d, body) {
            self.tdiag("E3010", msg, line);
        }
        // 戻り値の参照が、ヘッダの宣言と違う領域の引数から来る (修正プラン A1)。
        for (line, msg) in low.check_return_lifetime(d, body) {
            self.tdiag("E3010", msg, line);
        }
        // 戻り値へ流れる借用の出どころ (局所・ヘッダと違う領域の引数) を、式の形によらず一般に求める。
        for (code, line, msg) in low.check_return_flow(d, body) {
            let ln = crate::parser::unpack_pos(if line != 0 { line } else { d.line }).0;
            let dup = self.diags.iter().any(|x| x.code == code && x.fqn == d.name && x.line == ln)
                || (code == "E3010" && self.diags.iter().any(|x| x.code == "E3010" && x.fqn == d.name && x.line == ln && x.msg.contains("returns argument")));
            if !dup {
                self.tdiag(code, msg, line);
            }
        }
        // クロージャ式の戻り値も、関数と同じ省略・領域の規則で見る (E3020 / E3010)。
        for (code, line, msg) in low.check_closure_returns(d, body) {
            self.tdiag(code, msg, line);
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
                // 中断点をまたぐ借用は E3050 (仕様 5.6)、局所の借用を外へ出すのは E3030。それ以外の借用の競合は E3010。
                let code = if msg.contains("across a suspension point") {
                    "E3050"
                } else if msg.starts_with("the `with` body") {
                    "E3043"
                } else if msg.starts_with("returns a reference to the local variable") {
                    "E3030"
                } else {
                    "E3010"
                };
                self.tdiag(code, msg, line);
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
        /// 借用元の局所変数がブロックを抜けて寿命が尽きたローン。同名の兄弟ブロックの変数が
        /// 再び `ScopeAdd` されても、別の変数なので生き返らない (借用を作り直したときだけ外す)。
        dead: BTreeSet<usize>,
    }
    let mut state_in: Vec<Option<State>> = vec![None; n];
    state_in[0] = Some(State {
        holders: BTreeMap::new(),
        scope: low.scope_seed.iter().cloned().collect(),
        dead: BTreeSet::new(),
    });
    let mut work: VecDeque<usize> = VecDeque::new();
    work.push_back(0);
    let apply = |st_: &mut State, ev: &Ev| match ev {
        Ev::Loan { holder, id } => {
            // 同じ変数・場所が複数の借用を持つこともある (`match` の被検査式など) ので、既存に足す。
            st_.dead.remove(id);
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
            let mut acc: BTreeSet<usize> = BTreeSet::new();
            for (h, ids) in st_.holders.iter() {
                if let Holder::Place { root: r, path: p } = h {
                    if r == from_root && overlaps(p, from_path) {
                        acc.extend(ids.iter().copied());
                    }
                }
            }
            st_.holders.entry(to.clone()).or_default().extend(acc);
        }
        Ev::Deref { to, from } => {
            // `from` が持つローンの指す先 (借りた場所の根の変数が持つ借用と、その場所に保存された借用) を `to` へ。
            let ids: Vec<usize> = st_.holders.get(&Holder::Var(from.clone())).map(|s| s.iter().copied().collect()).unwrap_or_default();
            let mut acc: BTreeSet<usize> = BTreeSet::new();
            for id in ids {
                let l = &low.loans[id];
                if let Some(x) = st_.holders.get(&Holder::Var(l.root.clone())) {
                    acc.extend(x.iter().copied());
                }
                for (h, x) in st_.holders.iter() {
                    if let Holder::Place { root: r, path: p } = h {
                        if *r == l.root && overlaps(p, &l.path) {
                            acc.extend(x.iter().copied());
                        }
                    }
                }
            }
            if !acc.is_empty() {
                st_.holders.entry(to.clone()).or_default().extend(acc);
            }
        }
        Ev::Through { root, path } => {
            let own: BTreeSet<usize> = st_.holders.get(&Holder::Place { root: root.clone(), path: path.clone() }).cloned().unwrap_or_default();
            if !own.is_empty() {
                let rest: Vec<String> = path.iter().skip_while(|x| *x == "*").cloned().collect();
                let ptrs: Vec<usize> = st_.holders.get(&Holder::Var(root.clone())).map(|s| s.iter().copied().collect()).unwrap_or_default();
                for id in ptrs {
                    let l = &low.loans[id];
                    if !l.mutable || l.root == "$temp" || l.root == *root {
                        continue;
                    }
                    let mut tp = l.path.clone();
                    tp.extend(rest.iter().cloned());
                    let ids: Vec<usize> = own.iter().copied().filter(|x| low.loans[*x].root != l.root).collect();
                    st_.holders.entry(Holder::Place { root: l.root.clone(), path: tp }).or_default().extend(ids);
                }
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
        Ev::KillUnder { root, path } => {
            st_.holders.retain(|h, _| !matches!(h, Holder::Place { root: r, path: p } if r == root && p.starts_with(path)));
        }
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
        Ev::DropScope(v, site) => {
            st_.scope.remove(v);
            for (id, l) in low.loans.iter().enumerate() {
                if &l.root == v && l.site == Some(*site) {
                    st_.dead.insert(id);
                }
            }
        }
        Ev::Access { root, path, kind, .. } => {
            // 場所への上書き・`&mut` で、そこに保存されたローンは死ぬ。型が分からないので広めに殺す:
            // `v.push` のような要素を残す操作でも一度殺す (保存ローンを広めに殺すのは見落としになるので、殺すのは上書き (`Write`) だけ)。
            if matches!(kind, Acc::Write) {
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
                    for x in &stt.dead {
                        ch |= old.dead.insert(*x);
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
            // 中断点をまたいで生きる借用 (中断の間に、借りた先へ他のタスクが触るかもしれない)。保守側に、
            // 中断点の後で使われる持ち主 (変数・保存場所・式の一時) のローンはすべて報告する。
            if let Ev::Suspend { line } = ev {
                for (holder, ids) in &stt.holders {
                    let live = match holder {
                        Holder::Var(v) => live_after[k].contains(v),
                        Holder::Place { root: r, .. } => live_after[k].contains(r),
                    };
                    if !live {
                        continue;
                    }
                    let who = match holder {
                        Holder::Var(v) if v.starts_with("$iter") => "the loop iterator".to_string(),
                        Holder::Var(v) if v.starts_with("$cap") => "the shared capture of the closure".to_string(),
                        Holder::Var(v) if v.starts_with("$e") => "an earlier operand of the same expression".to_string(),
                        Holder::Var(v) => format!("`{}`", v),
                        Holder::Place { root: r, path: p } => format!("`{}`", place_name(r, p)),
                    };
                    for &id in ids {
                        if stt.dead.contains(&id) {
                            continue;
                        }
                        let l = &low.loans[id];
                        out.push((*line, format!("cannot hold a borrow of `{}` across a suspension point: {} is used after the call that may suspend", l.root, who)));
                    }
                }
            }
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
                        } else if *kind == Acc::Move && low.with_closes.contains(&(*line, root.clone())) {
                            out.push((*line, format!("the `with` body value or an outer variable borrows from `{}`, which the `close` expression consumes (4.4); it is held by {}", root, who)));
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
                    // 保存場所 (`v.push(&b)` の `v`) が持つローンの借用元が、ブロックを抜けて死んでいる。
                    for (holder, ids) in &stt.holders {
                        let Holder::Place { root: r, .. } = holder else { continue };
                        if r != root {
                            continue;
                        }
                        for &id in ids {
                            if stt.dead.contains(&id) {
                                let m = format!("use of `{r}` borrows `{}` which does not live long enough", low.loans[id].root);
                                if !out.iter().any(|(l, t)| l == line && *t == m) {
                                    out.push((*line, m));
                                }
                            }
                        }
                    }
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
                            // 借用元の変数が（同名の兄弟ブロックの変数に紛れず）寿命切れ。
                            if stt.dead.contains(&id) {
                                out.push((*line, format!("use of `{root}` borrows `{}` which does not live long enough", l.root)));
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
            // 借用を持つ変数そのものを move する (`let (r, n) = pr`、`let q = pr`): 借用元がすでに寿命切れなら、
            // move した先は宙に浮いた借用を持つ (分解の束縛へは借用が伝わらないので、ここで見る)。
            if let Ev::Access { root, kind: Acc::Move, line, .. } = ev {
                if let Some(ids) = stt.holders.get(&Holder::Var(root.clone())) {
                    for &id in ids {
                        if stt.dead.contains(&id) && low.locals.contains(root) {
                            let m = format!("use of `{root}` borrows `{}` which does not live long enough", low.loans[id].root);
                            if !out.iter().any(|(l, t)| l == line && *t == m) {
                                out.push((*line, m));
                            }
                        }
                    }
                }
            }
            // 枝の値が、枝の終わりで寿命が尽きる所有物の借用を運ぶ (`match` の腕 `Ok(v) => v.as_str()` など)。
            // 値を使うかどうかに関係なく、その値は外へ出てしまうので E3030。
            if let Ev::Escape { holder, vars, line } = ev {
                if let Some(ids) = stt.holders.get(&Holder::Var(holder.clone())) {
                    for &id in ids {
                        let r = &low.loans[id].root;
                        if vars.iter().any(|(v, s)| v == r && low.loans[id].site == Some(*s)) {
                            let m = if vars.iter().any(|(v, s)| v == r && low.with_sites.contains(&(*s, v.clone()))) {
                                format!("the `with` body value borrows from `{}`, which the `close` expression consumes (4.4)", r)
                            } else {
                                format!("returns a reference to the local variable `{}`, which does not live long enough", r)
                            };
                            if !out.iter().any(|(l, t)| l == line && *t == m) {
                                out.push((*line, m));
                            }
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
pub(crate) fn match_close(s: &str, open: usize) -> Option<usize> {
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
/// 型の文面 `Name<a, b>` を (名前, 引数)。引数がなければ空。
fn type_args(t: &str) -> (String, Vec<String>) {
    let t = t.trim();
    match t.find('<') {
        Some(i) if t.ends_with('>') => (t[..i].trim().to_string(), split_top(&t[i + 1..t.len() - 1])),
        _ => (t.to_string(), vec![]),
    }
}

/// `Err(x)` の payload `x` (`?` の対象が `Err(..)` の形のとき)。
fn err_payload(op: &V) -> Option<&V> {
    if nname(op) != "Call" {
        return None;
    }
    let f = nf(op);
    if nname(&f[0]) != "Path" || strs(&nf(&f[0])[0]).as_slice() != ["Err"] {
        return None;
    }
    items(&f[1]).first()
}

/// 型の文面の中の名前付きの型 (`["named", name, ..]`) の名前を集める。
fn named_types(v: &V, out: &mut Vec<String>) {
    if let V::L(parts) = v {
        if let [V::S(a), V::S(n), ..] = parts.as_slice() {
            if a.as_str() == "named" {
                out.push(n.clone());
            }
        }
        for p in parts {
            named_types(p, out);
        }
    }
}

pub(crate) fn split_top(s: &str) -> Vec<String> {
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


/// 局所の束縛 1 つ (名前・見える範囲・初期化式)。`tilde` は分解・`match` の腕・`for` の束縛 (受け手の自動借用を含む)。
struct LocalBind<'a> {
    from: (usize, usize),
    to: (usize, usize),
    init: Option<&'a V>,
    tilde: bool,
    /// 分解・腕のパターン (`PBind` だけの束縛は None)。束縛が取り出す欄の領域を、取り出し元から引くのに使う。
    pat: Option<&'a V>,
}

/// 関数本体の局所の束縛。同名の束縛が複数あっても、使う位置から見える一番内側・後ろの束縛を選ぶ。
struct Inits<'a> {
    binds: HashMap<String, Vec<LocalBind<'a>>>,
    pos: std::cell::Cell<(usize, usize)>,
    /// 節点ごとの位置 (自分に位置が無ければ、位置を持つ一番近い祖先の位置)。
    eff: HashMap<*const V, (usize, usize)>,
    /// 局所の変数への代入 `r = e` の右辺 (変数名ごと)。位置は区別しない (流れに依らない近似)。
    assigns: HashMap<String, Vec<&'a V>>,
}

fn pos_of(v: &V) -> (usize, usize) {
    crate::parser::unpack_pos(nline(v))
}

/// 節点とその子孫の最後の位置。
fn max_pos(v: &V) -> (usize, usize) {
    let mut all = vec![];
    walk(v, &mut all);
    all.iter().map(|n| pos_of(n)).max().unwrap_or((0, 0))
}

fn fill_eff(v: &V, up: (usize, usize), out: &mut HashMap<*const V, (usize, usize)>) {
    let p = if nline(v) != 0 { pos_of(v) } else { up };
    out.insert(v as *const V, p);
    for c in children(v) {
        fill_eff(c, p, out);
    }
}

impl<'a> Inits<'a> {
    fn build(body: &'a V) -> Inits<'a> {
        let mut me = Inits { binds: HashMap::new(), pos: std::cell::Cell::new((0, 0)), eff: HashMap::new(), assigns: HashMap::new() };
        fill_eff(body, (0, 0), &mut me.eff);
        me.scan(body, max_pos(body));
        me
    }

    fn add(&mut self, pat: &'a V, from: (usize, usize), to: (usize, usize), init: Option<&'a V>, simple: bool) {
        self.add_p(pat, from, to, init, simple, true)
    }

    fn add_p(&mut self, pat: &'a V, from: (usize, usize), to: (usize, usize), init: Option<&'a V>, simple: bool, with_pat: bool) {
        let mut b = vec![];
        pat_binds(pat, &mut b);
        for x in b {
            let tilde = !simple;
            let pat = if nname(pat) == "PBind" || !with_pat { None } else { Some(pat) };
            self.binds.entry(x).or_default().push(LocalBind { from, to, init, tilde, pat });
        }
    }

    fn scan(&mut self, n: &'a V, end: (usize, usize)) {
        let mut end = end;
        match nname(n) {
            "Block" => end = max_pos(n),
            "Let" => {
                let f = nf(n);
                let init = if f.len() > 3 && !is_nil(&f[3]) { Some(&f[3]) } else { None };
                let simple = nname(&f[0]) == "PBind";
                self.add(&f[0], max_pos(n), end, init, simple);
            }
            "For" => {
                let f = nf(n);
                self.add_p(&f[0], max_pos(&f[1]), max_pos(n), Some(&f[1]), false, false);
            }
            "Assign" => {
                let f = nf(n);
                if f.len() >= 2 && nname(&f[0]) == "Path" && strs(&nf(&f[0])[0]).len() == 1 {
                    self.assigns.entry(strs(&nf(&f[0])[0])[0].clone()).or_default().push(&f[1]);
                }
            }
            "With" => {
                let f = nf(n);
                self.add(&f[0], max_pos(&f[2]), max_pos(n), None, true);
            }
            "Match" => {
                let f = nf(n);
                let scrut_is_local = nname(&f[0]) == "Path";
                for arm in items(&f[1]) {
                    // 腕は `V::L` (パターン・本体・ガード) で、`walk` が辿れないので各要素の最後の位置を取る。
                    let to = items(arm).iter().map(max_pos).max().unwrap_or((0, 0));
                    self.add(&items(arm)[0], pos_of(&items(arm)[0]), to, Some(&f[0]), scrut_is_local);
                }
            }
            _ => {}
        }
        for c in children(n) {
            self.scan(c, end);
        }
    }

    /// 今の使う位置から見える `name` の束縛 (一番後ろから始まるもの)。
    fn at(&self, v: &V) -> (usize, usize) {
        self.eff.get(&(v as *const V)).copied().unwrap_or((0, 0))
    }

    fn visible(&self, name: &str) -> Option<&LocalBind<'a>> {
        let at = self.pos.get();
        self.binds.get(name)?.iter().filter(|b| b.from < at && at <= b.to).max_by_key(|b| b.from)
    }

    fn has(&self, name: &str) -> bool {
        self.visible(name).is_some()
    }

    /// 通常の束縛 (`let y = x`・腕の局所の scrutinee) の初期化式。
    fn plain(&self, name: &str) -> Option<&'a V> {
        self.visible(name).filter(|b| !b.tilde).and_then(|b| b.init)
    }

    /// 分解・腕・`for` の束縛の初期化式。
    fn tilde(&self, name: &str) -> Option<&'a V> {
        self.visible(name).filter(|b| b.tilde).and_then(|b| b.init)
    }
}

/// 文面に現れる名前付きライフタイム (`'static` を除く)。
/// 引数の型 (`x: &u32`・`self: &Self`) が、省略したライフタイムの参照か (名前のある領域を持たない参照)。
/// 省略した入力の領域は、呼び出しごとに別の新しい領域であり (仕様 4.3)、どの名前付きの出力にも収まらない。
fn is_elided_ref(ptext: &str) -> bool {
    let ty = ptext.split_once(':').map_or(ptext, |x| x.1).trim();
    ty.starts_with('&') && !ty.contains('\'')
}

/// 名前付きの外側の参照 (`&'a ..`) の中に、省略した参照 (`&'a &u32` の内側) を持つ型か。
fn inner_elided(ptext: &str) -> bool {
    let ty = ptext.split_once(':').map_or(ptext, |x| x.1).trim();
    let Some(rest) = ty.strip_prefix('&') else { return false };
    let Some(rest) = rest.strip_prefix('\'') else { return false };
    let rest = rest.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_');
    let rest = rest.trim_start().trim_start_matches("mut").trim_start();
    let b = rest.as_bytes();
    rest.contains('&') && !rest.contains("fn(") && (0..b.len()).any(|i| b[i] == b'&' && b.get(i + 1) != Some(&b'\''))
}

/// 引数の型が、省略したライフタイムの参照をどこかに持つか (`x: &u32`・`x: Option<&u32>`・`x: & &u32`・`x: (&u32, u32)`)。
/// 名前付きのライフタイムが 1 つでも書かれている型は、領域を名前で読めるので対象外 (`is_elided_ref` と同じく省略した入力は
/// 呼び出しごとの新しい領域)。関数ポインタ型 (`fn(&u32) -> u32`) の `&` は呼ばれるときの領域なので数えない。
fn carries_elided_ref(ptext: &str) -> bool {
    let ty = ptext.split_once(':').map_or(ptext, |x| x.1).trim();
    ty.contains('&') && !ty.contains('\'') && !ty.contains("fn(")
}

/// `lifetimes` と同じ (名前付きの領域)。ただし `'static` も含める。戻り値の型が `'static` を名指すかを見るため。
fn lifetimes_with_static(s: &str) -> BTreeSet<String> {
    let mut out = lifetimes(s);
    if s.contains("'static") {
        out.insert("static".to_string());
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
        // `&(3i32 + 4i32)`: リテラルだけの算術は定数の昇格 (`'static`)。
        "Bin" => {
            let f = nf(e);
            fn lits(e: &V) -> bool {
                match nname(e) {
                    "Lit" => true,
                    "Bin" => lits(&nf(e)[1]) && lits(&nf(e)[2]),
                    "Unary" => st(&nf(e)[0]) == "-" && lits(&nf(e)[1]),
                    _ => false,
                }
            }
            lits(&f[1]) && lits(&f[2])
        }
        _ => false,
    }
}
