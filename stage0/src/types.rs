//! 型付きの式推論 (型付き IR の最初の段階)。
//!
//! 関数本体の各式に型 (`Ty`) を与える。型変数 (`Ty::Var`) と双方向の単一化で、整数・浮動小数リテラルの型と
//! コンテナの要素型 (`let mut v = Vec.new()` を `v.push(1u8)` で決める) を、関数の中で伝播する。
//! 検査: レシーバの型によるメソッドの存在 (E1108)・引数の個数 (E2022)・プリミティブ引数の型 (E2023)、
//! 演算子の両辺の型の不一致 (E2024)、添字の型 (E2025)。
//! 分からない型は `Ty::Unknown` にして、そこから先は何も言わない (誤検出より取りこぼしを選ぶ)。
//! 参照は透過的に扱う (`&T` と `T` は同じとみなす)。

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::check::{edit_distance, is_nil, items, nf, nline, nname, pat_binds, st, strs, tag, tel, Diag};
use crate::parser::V;
use crate::program::{Def, Program, Target};

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Unknown,
    Never,
    Unit,
    Prim(String),
    Named(String, Vec<Ty>),
    Ref(bool, Box<Ty>),
    Tuple(Vec<Ty>),
    Array(Box<Ty>),
    Slice(Box<Ty>),
    /// ジェネリクスの型パラメータ (シグネチャの中だけに現れる)
    Param(String),
    /// 推論中の型変数 (関数ごとの表の添字)
    Var(usize),
    /// 型実引数の定数リテラル (`app.arr.Buf<u8, 16>` の `16`、`Flag<true>` の `true` など) (4.3)。
    /// リテラルどうしの一致だけを判定する (定数名との対応は評価しない)。
    Const(String),
    /// クロージャの型。`cap` は capture 節から決めた「満たせ得る最大の `Fn` 系トレイト」の近似。
    /// `id` はクロージャ本体に対応する呼び出しグラフの節 (5.3 の間接呼び出しの候補計算に使う)。
    /// 型注釈から組み立てた型にはない (`None`)。
    Closure { args: Vec<Ty>, ret: Box<Ty>, cap: Cap, id: Option<String> },
    /// トレイトオブジェクト `dyn Tr` (5.1)。`Tr` は書かれたトレイト名で、使用箇所で trait 定義の FQN に解決する。
    /// `dyn` は `&` / `&mut` の背後にのみ現れるので、`Ref` の中身として持つ。
    Dyn(String),
}

/// クロージャの能力 (`Fn` の階順)。capture が共有参照だけなら `Fn`、`&mut` を含めば `FnMut`、
/// 値での取り込み (`run` なし) を含めば `FnOnce` に届く。
#[derive(Clone, Debug, PartialEq)]
pub enum Cap {
    Fn,
    FnMut,
    FnOnce,
}

impl Ty {
    fn strip(&self) -> &Ty {
        let mut t = self;
        while let Ty::Ref(_, inner) = t {
            t = inner;
        }
        t
    }
    fn show(&self) -> String {
        match self {
            Ty::Unknown | Ty::Var(_) => "_".into(),
            Ty::Never => "!".into(),
            Ty::Unit => "()".into(),
            Ty::Prim(p) => p.clone(),
            Ty::Named(n, a) => {
                if a.is_empty() {
                    n.clone()
                } else {
                    format!("{}<{}>", n, a.iter().map(|x| x.show()).collect::<Vec<_>>().join(", "))
                }
            }
            Ty::Ref(m, t) => format!("&{}{}", if *m { "mut " } else { "" }, t.show()),
            Ty::Tuple(ts) => format!("({})", ts.iter().map(|x| x.show()).collect::<Vec<_>>().join(", ")),
            Ty::Array(t) => format!("[{}; _]", t.show()),
            Ty::Slice(t) => format!("[{}]", t.show()),
            Ty::Param(p) => p.clone(),
            Ty::Const(c) => c.clone(),
            Ty::Closure { args, ret, .. } => {
                format!("fn({}) -> {}", args.iter().map(|x| x.show()).collect::<Vec<_>>().join(", "), ret.show())
            }
            Ty::Dyn(t) => format!("dyn {}", t),
        }
    }
    fn has_param(&self) -> bool {
        match self {
            Ty::Param(_) => true,
            Ty::Named(_, a) | Ty::Tuple(a) => a.iter().any(|x| x.has_param()),
            Ty::Ref(_, x) | Ty::Array(x) | Ty::Slice(x) => x.has_param(),
            Ty::Closure { args, ret, .. } => args.iter().any(|x| x.has_param()) || ret.has_param(),
            _ => false,
        }
    }
}

const INTS: &[&str] = &["i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize"];

/// 型の構文木 (V) を `Ty` にする。`params` に含まれる名前は型パラメータ。
pub fn from_v(v: &V, params: &HashSet<String>) -> Ty {
    if is_nil(v) {
        return Ty::Unknown;
    }
    match tag(v).as_str() {
        "prim" => Ty::Prim(st(tel(v, 1))),
        "never" => Ty::Never,
        "tuple" => {
            let xs: Vec<Ty> = items(tel(v, 1)).iter().map(|x| from_v(x, params)).collect();
            if xs.is_empty() { Ty::Unit } else { Ty::Tuple(xs) }
        }
        "array" => Ty::Array(Box::new(from_v(tel(v, 1), params))),
        "slice" => Ty::Slice(Box::new(from_v(tel(v, 1), params))),
        "ref" => Ty::Ref(matches!(tel(v, 1), V::B(true)), Box::new(from_v(tel(v, 2), params))),
        "named" => {
            let name = st(tel(v, 1));
            if params.contains(&name) {
                return Ty::Param(name);
            }
            let args: Vec<Ty> = items(tel(v, 2))
                .iter()
                .filter(|a| matches!(a, V::L(_)))
                .map(|a| const_targ(a).map_or_else(|| from_v(a, params), Ty::Const))
                .collect();
            Ty::Named(name, args)
        }
        // `dyn Tr` (のトレイト名)。型引数・効果・ライフタイムは解決の対象にしない (近似)。
        "dyn" => Ty::Dyn(st(tel(tel(v, 1), 0))),
        _ => Ty::Unknown,
    }
}

/// 型実引数が定数リテラル (`16` / `true` / `'a'`) なら、その綴りを返す (4.3 の定数パラメータ)。
/// 定数名 (`app.f.FOO`) は型と同じ構文で書かれるため区別できず、None にする (一致判定もしない)。
fn const_targ(a: &V) -> Option<String> {
    if tag(a) == "const" {
        return Some(st(tel(a, 1)));
    }
    None
}

/// 型パラメータを束縛で置き換える。束縛のないものは `Unknown`。
fn subst(t: &Ty, bind: &HashMap<String, Ty>) -> Ty {
    match t {
        Ty::Param(p) => bind.get(p).cloned().unwrap_or(Ty::Unknown),
        Ty::Named(n, a) => Ty::Named(n.clone(), a.iter().map(|x| subst(x, bind)).collect()),
        Ty::Ref(m, x) => Ty::Ref(*m, Box::new(subst(x, bind))),
        Ty::Tuple(xs) => Ty::Tuple(xs.iter().map(|x| subst(x, bind)).collect()),
        Ty::Array(x) => Ty::Array(Box::new(subst(x, bind))),
        Ty::Slice(x) => Ty::Slice(Box::new(subst(x, bind))),
        Ty::Closure { args, ret, cap, id } => Ty::Closure {
            args: args.iter().map(|x| subst(x, bind)).collect(),
            ret: Box::new(subst(ret, bind)),
            cap: cap.clone(),
            id: id.clone(),
        },
        other => other.clone(),
    }
}

const DERIVED: &[&str] = &[
    "clone", "eq", "ne", "cmp", "partial_cmp", "hash", "debug_fmt", "display_fmt", "call", "call_mut", "call_once", "drop",
    "try_clone", "to_string", "lt", "le", "gt", "ge", "min", "max",
];

/// 型で解決できた呼び出しの効果検査の義務 (5.1 / 4.3)。check.rs が、呼び出し元 `di` の
/// `uses` / `panics` と比べる。`cands` は (呼び出し先の名前, 検査する uses, 検査する panics) の列で、
/// すべての候補が違反するときだけ報告する (候補が複数あるときは保守側に黙る)。
pub struct EffCheck {
    pub di: usize,
    pub line: usize,
    pub cands: Vec<(String, Vec<String>, Option<String>)>,
}

/// 効果変数・panic 変数の束縛 (クロージャの本体から推論した効果、4.3)。
#[derive(Default)]
struct EffBind {
    uses: HashMap<String, BTreeSet<String>>,
    panics: HashMap<String, bool>,
}

pub struct TypeCk<'a> {
    p: &'a Program,
    /// 所有者 (型の FQN) → メソッド名 → 定義の添字
    owners: HashMap<String, HashMap<String, Vec<usize>>>,
    trait_methods: HashSet<String>,
    pub diags: Vec<Diag>,
    // 現在の関数
    cur: usize,
    scopes: Vec<HashMap<String, Ty>>,
    /// 型変数の束縛 (None = 未束縛) と種類 (0 = 一般、1 = 整数リテラル、2 = 浮動小数リテラル)
    vars: Vec<Option<Ty>>,
    kinds: Vec<u8>,
    /// 文脈が追えなかった (未知の型に渡された) 型変数。型が決まらなくてもエラーにしない。
    escaped: HashSet<usize>,
    /// `let mut` で宣言された局所変数 (Kasane はシャドーイング禁止なので、名前は関数内で一意)
    mutable: HashSet<String>,
    /// 出てきたリテラル: (種類 "int"/"float", 綴り, 型, 行)
    lits: Vec<(&'static str, String, Ty, usize)>,
    /// 式を置いた関数・クロージャの戻り値型の列 (`?` のエラー型の検査に使う)。
    rets: Vec<Ty>,
    /// 呼び出しの辺 (呼び出し元の関数名 -> 呼び出し先の関数名)。型で解決できたメソッド呼び出しを含む。
    pub edges: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    /// 入れ子のクロージャ本体に対応する呼び出しグラフの節 (5.3)。スタックの先頭がいま推論中のクロージャ。
    closure_nodes: Vec<String>,
    /// クロージャ節の採番 (節名は関数名に付けて一意にする)。
    closure_seq: usize,
    /// (呼び出し先の FQN, 型パラメータ名) → その引数位置に渡されたクロージャの節。
    /// `Fn` 境界のパラメータの呼び出し (5.3) の候補。分からなかったものは覚えない (黙る)。
    fn_closures: HashMap<(String, String), Vec<String>>,
    /// 関数の FQN → 本体の中で呼んでいる `Fn` 境界パラメータの局所名の集合 (5.3 の近似)。
    param_called: HashMap<String, HashSet<String>>,
    /// クロージャの節 → (パラメータの型注釈, 本体)。`stack` の見積り (E4012) で、
    /// クロージャの節のフレームを引数と `let` の型の大きさから計算するのに使う (5.3)。
    pub closure_frames: std::collections::BTreeMap<String, (Vec<V>, V)>,
    /// 推論中のクロージャ本体の効果 (`closure_nodes` と同じ深さのスタック)。型で解決できた呼び出しの
    /// 効果と添字の panic だけを足す (下からの近似。分からない呼び出しは足さない = 誤検出を出さない)。
    /// 3 つ目は「推論が確実でない」印 (推論された関数 `infer.rs` の効果を含む呼び出しがあった、
    /// または内側の確実でないクロージャを含む)。確実でない効果は境界の検査 (`check_closure_bounds`) に使わない。
    eff_acc: Vec<(BTreeSet<String>, bool, bool)>,
    /// クロージャの節 → 本体から推論した効果 (uses, panics may か, 確実でないか)。
    closure_effs: HashMap<String, (BTreeSet<String>, bool, bool)>,
    /// 型で解決できた呼び出しの効果検査の義務 (check.rs が検査する)。
    pub eff_checks: Vec<EffCheck>,
    /// 型推論の被覆率の計測 (`KSN_TYPE_COVERAGE=1` のときだけ `Some`。typecov.rs)。
    pub cov: Option<crate::typecov::Cov>,
}

impl<'a> TypeCk<'a> {
    pub fn new(p: &'a Program) -> Self {
        let mut owners: HashMap<String, HashMap<String, Vec<usize>>> = HashMap::new();
        for (i, d) in p.defs.iter().enumerate() {
            if d.kw == "fn" && d.is_method() {
                if let Some((o, m)) = d.name.rsplit_once('.') {
                    owners.entry(o.to_string()).or_default().entry(m.to_string()).or_default().push(i);
                }
            }
        }
        let mut trait_methods = HashSet::new();
        for d in &p.defs {
            if d.kw == "trait" {
                trait_methods.extend(d.methods.iter().cloned());
            }
        }
        let cov = (std::env::var("KSN_TYPE_COVERAGE").as_deref() == Ok("1")).then(Default::default);
        TypeCk { p, owners, trait_methods, diags: vec![], cur: 0, scopes: vec![], vars: vec![], kinds: vec![], escaped: HashSet::new(), mutable: HashSet::new(), lits: vec![], rets: vec![], edges: Default::default(), closure_nodes: vec![], closure_seq: 0, fn_closures: HashMap::new(), param_called: HashMap::new(), closure_frames: Default::default(), eff_acc: vec![], closure_effs: HashMap::new(), eff_checks: vec![], cov }
    }

    /// 関数 `d` の本体が、局所名 `param` の (`Fn` 境界の) パラメータを呼んでいるか (5.3)。
    /// `f(..)` / `f.call(..)` / `f.call_mut(..)` / `f.call_once(..)` の形の呼び出しを探す近似。
    /// 名前で見るので、`let g = f` としたあとの `g.call(..)` は見つけられない (黙る)。
    fn callee_calls_param(&mut self, d: &Def, param: &str) -> bool {
        if !self.param_called.contains_key(&d.name) {
            let mut called: HashSet<String> = HashSet::new();
            if let Some(b) = &d.body {
                let mut all: Vec<&V> = vec![];
                crate::check::walk(b, &mut all);
                for n in &all {
                    // `f.call(..)` は、呼び出し先が `f.call` というパスの `Call` (局所変数から始まる
                    // パスはメソッド呼び出しに解決される)。
                    if nname(n) != "Call" {
                        continue;
                    }
                    let f = nf(n);
                    if nname(&f[0]) != "Path" {
                        continue;
                    }
                    let segs = strs(&nf(&f[0])[0]);
                    // `f(x)` (局所名の直接呼び出し) と `f.call(..)` の両方を、パラメータの呼び出しとして見る。
                    // 前者は `Fn` 境界の値の普通の呼び出しで、後者は 5.3 の検査 (E4010c) が使う形。
                    let direct = segs.len() == 1;
                    if direct || (segs.len() == 2 && matches!(segs[1].as_str(), "call" | "call_mut" | "call_once")) {
                        called.insert(segs[0].clone());
                    }
                }
            }
            self.param_called.insert(d.name.clone(), called);
        }
        self.param_called.get(&d.name).map_or(false, |s| s.contains(param))
    }

    /// 呼び出しの辺を足す。クロージャの本体の中では、外側の関数 (従来どおりの近似) と
    /// クロージャの節の両方に入る。クロージャの節は、`Fn` 境界のパラメータの呼び出し (5.3) の
    /// 候補として使う (外側の関数だけだと、呼び出し先が分からない呼び出しの候補を表せない)。
    fn add_edge(&mut self, to: String) {
        let caller = self.def().name.clone();
        self.edges.entry(caller).or_default().insert(to.clone());
        if let Some(node) = self.closure_nodes.last() {
            let node = node.clone();
            self.edges.entry(node).or_default().insert(to);
        }
    }

    fn def(&self) -> &'a Def {
        &self.p.defs[self.cur]
    }

    fn diag(&mut self, code: &str, msg: String, line: usize) {
        let d = self.def();
        self.diags.push(Diag { file: d.file.clone(), code: code.into(), fqn: d.name.clone(), line: if line != 0 { line } else { d.line }, msg });
    }

    pub fn check_def(&mut self, di: usize) {
        let d = &self.p.defs[di];
        let Some(body) = &d.body else { return };
        self.cur = di;
        self.vars.clear();
        self.kinds.clear();
        self.escaped.clear();
        self.mutable.clear();
        self.lits.clear();
        self.scopes = vec![HashMap::new()];
        let gn = d.gnames();
        for (n, t) in &d.params {
            let ty = from_v(t, &gn);
            // 関数自身の型パラメータ (Ty::Param) は、単一化では「分からない型」と同じに働くが、
            // メソッド呼び出しでは境界のトレイトを引くために名前を残す。
            self.scopes[0].insert(n.clone(), ty);
        }
        // `?` のエラー型の検査のため、この関数の戻り値型を覚える (クロージャはその注釈を重ねる)。
        let ret = subst_params_unknown(&from_v(&d.ret, &gn));
        self.rets.push(ret.clone());
        let value = self.infer(body);
        self.rets.pop();
        if let Err(err) = self.unify2(&ret, &value) {
            if let Some((want, got)) = self.ann_conflict(&ret, &value, &err) {
                let stmts = items(&nf(body)[0]);
                let line = stmts.last().map(nline).unwrap_or(0);
                self.diag("E2020", format!("type mismatch in return value: expected {}, found {}{}", want, got, conv_hint(&want, &got)), line);
            }
        }
        self.check_dangling(body, &ret);
        self.finish_literals();
        self.cov_flush();
    }

    /// 計測: この関数で記録した式の型を、型変数を解決してから分類し、集計に入れる (typecov.rs)。
    /// 未知の根は、子 (レシーバなど) が既知なのに未知になったもの。
    fn cov_flush(&mut self) {
        let Some(mut cov) = self.cov.take() else { return };
        let recs = cov.take_pending();
        let cls: HashMap<usize, crate::typecov::Cls> = recs.iter().map(|r| (r.key, crate::typecov::classify(&self.deep(&r.ty)))).collect();
        for r in &recs {
            let c = cls[&r.key];
            let unk = |s: &crate::typecov::Cls| matches!(s, crate::typecov::Cls::Unknown | crate::typecov::Cls::Var);
            let local_unk = r.local.as_ref().map_or(false, |t| unk(&crate::typecov::classify(&self.deep(t))));
            let propagated = c == crate::typecov::Cls::Unknown && (local_unk || r.subject.and_then(|k| cls.get(&k)).map_or(false, unk));
            cov.add(r, c, !propagated);
        }
        self.cov = Some(cov);
    }

    /// 計測: `infer_node` の結果を記録する。種別と原因の手掛かりは AST から取る。
    fn cov_record(&mut self, e: &V, t: &Ty) {
        let kind = nname(e).to_string();
        let f = nf(e);
        let mut detail = String::new();
        let mut subject: Option<&V> = None;
        let mut local: Option<Ty> = None;
        if !crate::typecov::HANDLED.contains(&kind.as_str()) {
            detail = "規則なし".into();
        }
        match kind.as_str() {
            "Path" => {
                // 先頭が局所変数なら、残りは `path()` が型からのフィールドとしてたどる (型は変数から来る)。
                let segs = strs(&f[0]);
                local = self.lookup(&segs[0]);
                detail = segs.join(".");
            }
            "Lit" => detail = st(&f[0]),
            "Call" => {
                if nname(&f[0]) == "Path" {
                    let segs = strs(&nf(&f[0])[0]);
                    detail = segs.join(".");
                    local = self.lookup(&segs[0]);
                } else {
                    detail = "(式の呼び出し)".into();
                }
            }
            "MCall" | "Field" => {
                detail = format!(".{}", st(&f[1]));
                subject = Some(&f[0]);
            }
            "Try" | "Index" => subject = Some(&f[0]),
            "Unary" => {
                detail = st(&f[0]);
                subject = Some(&f[1]);
            }
            "Bin" => {
                detail = st(&f[0]);
                subject = Some(&f[1]);
            }
            "Block" => {
                subject = items(&f[0]).last().filter(|s| nname(s) == "ExprStmt").map(|s| &nf(s)[0]);
            }
            "If" => subject = Some(&f[1]),
            _ => {}
        }
        let rec = crate::typecov::Rec {
            key: e as *const V as usize,
            kind,
            detail,
            ty: t.clone(),
            subject: subject.map(|s| s as *const V as usize),
            local,
            loc: format!("{}:{}", self.def().file, nline(e)),
        };
        if let Some(c) = self.cov.as_mut() {
            c.record(rec);
        }
    }

    /// 戻り値が参照のとき、局所変数 (引数以外) への参照を返していないか。
    fn check_dangling(&mut self, body: &V, ret: &Ty) {
        if !matches!(ret, Ty::Ref(..)) {
            return;
        }
        let d = self.def();
        let params: HashSet<&String> = d.params.iter().map(|p| &p.0).collect();
        let mut nodes: Vec<&V> = vec![];
        crate::check::walk(body, &mut nodes);
        let mut locals: HashSet<String> = HashSet::new();
        for n in &nodes {
            if nname(n) == "Let" {
                let mut b = vec![];
                pat_binds(&nf(n)[0], &mut b);
                locals.extend(b);
            }
        }
        let mut returned: Vec<&V> = vec![];
        if let Some(last) = items(&nf(body)[0]).last() {
            if nname(last) == "ExprStmt" {
                returned.push(&nf(last)[0]);
            }
        }
        for n in &nodes {
            if nname(n) == "Return" && !is_nil(&nf(n)[0]) {
                returned.push(&nf(n)[0]);
            }
        }
        for r in returned {
            if nname(r) == "Unary" && matches!(st(&nf(r)[0]).as_str(), "&" | "&mut") && nname(&nf(r)[1]) == "Path" {
                let segs = strs(&nf(&nf(r)[1])[0]);
                if segs.len() == 1 && locals.contains(&segs[0]) && !params.contains(&segs[0]) {
                    self.diag(
                        "E3030",
                        format!("returns a reference to the local variable `{}`, which does not live long enough", segs[0]),
                        nline(r),
                    );
                }
            }
        }
    }

    /// 関数の最後に、リテラルの範囲と、型が決まらなかったリテラルを検査する。
    fn finish_literals(&mut self) {
        let lits = std::mem::take(&mut self.lits);
        for (kind, text, ty, line) in lits {
            match self.deep(&ty) {
                Ty::Prim(p) if kind == "int" && INTS.contains(&p.as_str()) => {
                    if let Some(msg) = int_literal_problem(&text, &p) {
                        self.diag("E2026", msg, line);
                    }
                }
                Ty::Var(v) => {
                    if !self.escaped.contains(&v) {
                        self.diag(
                            "E2027",
                            format!(
                                "cannot infer the type of the {} literal `{}`: add a suffix such as `{}{}`",
                                kind,
                                text,
                                text,
                                if kind == "int" { "u32" } else { "f64" }
                            ),
                            line,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    /// 同じ型になるべき 2 つの式 (`if` の両枝、`match` の腕) の型を一致させる。結果は報告しない
    /// (不一致は別の検査が見る)。分からない型 (未知・型パラメータ) とは一致させない。そうしないと
    /// 型変数が escape され、本来決まるはずの型変数の診断が消えるため。
    fn join_arm(&mut self, a: &Ty, b: &Ty) {
        let unknown = |t: &Ty| matches!(t, Ty::Unknown | Ty::Param(_));
        if unknown(&self.resolve(a)) || unknown(&self.resolve(b)) {
            return;
        }
        let _ = self.unify2(a, b);
    }

    fn escape(&mut self, t: &Ty) {
        match self.resolve(t) {
            Ty::Var(v) => {
                self.escaped.insert(v);
            }
            Ty::Named(_, a) | Ty::Tuple(a) => a.iter().for_each(|x| self.escape(x)),
            Ty::Ref(_, x) | Ty::Array(x) | Ty::Slice(x) => self.escape(&x),
            Ty::Closure { args, ret, .. } => {
                args.iter().for_each(|x| self.escape(x));
                self.escape(&*ret);
            }
            _ => {}
        }
    }

    fn lookup(&self, name: &str) -> Option<Ty> {
        self.scopes.iter().rev().find_map(|s| s.get(name).cloned())
    }

    fn bind(&mut self, name: &str, ty: Ty) {
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string(), ty);
        }
    }

    // ------------------------------------------------------------ 型変数と単一化
    fn fresh(&mut self, kind: u8) -> Ty {
        self.vars.push(None);
        self.kinds.push(kind);
        Ty::Var(self.vars.len() - 1)
    }

    /// 先頭だけ解決する。
    fn resolve(&self, t: &Ty) -> Ty {
        let mut t = t.clone();
        while let Ty::Var(i) = t {
            match &self.vars[i] {
                Some(b) => t = b.clone(),
                None => break,
            }
        }
        t
    }

    /// 中まで解決する。
    fn deep(&self, t: &Ty) -> Ty {
        match self.resolve(t) {
            Ty::Named(n, a) => Ty::Named(n, a.iter().map(|x| self.deep(x)).collect()),
            Ty::Ref(m, x) => Ty::Ref(m, Box::new(self.deep(&x))),
            Ty::Tuple(xs) => Ty::Tuple(xs.iter().map(|x| self.deep(x)).collect()),
            Ty::Array(x) => Ty::Array(Box::new(self.deep(&x))),
            Ty::Slice(x) => Ty::Slice(Box::new(self.deep(&x))),
            Ty::Closure { args, ret, cap, id } => Ty::Closure {
                args: args.iter().map(|x| self.deep(x)).collect(),
                ret: Box::new(self.deep(&*ret)),
                cap,
                id,
            },
            other => other,
        }
    }

    fn occurs(&self, v: usize, t: &Ty) -> bool {
        match self.resolve(t) {
            Ty::Var(i) => i == v,
            Ty::Named(_, a) | Ty::Tuple(a) => a.iter().any(|x| self.occurs(v, x)),
            Ty::Ref(_, x) | Ty::Array(x) | Ty::Slice(x) => self.occurs(v, &x),
            Ty::Closure { args, ret, .. } => args.iter().any(|x| self.occurs(v, x)) || self.occurs(v, &*ret),
            _ => false,
        }
    }

    /// 2 つの型を等しくする。矛盾したら、その箇所の 2 つの型を返す。参照は透過的。
    fn unify2(&mut self, a: &Ty, b: &Ty) -> Result<(), (Ty, Ty)> {
        let (a, b) = (self.resolve(a), self.resolve(b));
        if matches!(a, Ty::Unknown | Ty::Param(_)) {
            self.escape(&b);
        }
        if matches!(b, Ty::Unknown | Ty::Param(_)) {
            self.escape(&a);
        }
        match (&a, &b) {
            (Ty::Unknown, _) | (_, Ty::Unknown) | (Ty::Never, _) | (_, Ty::Never) | (Ty::Param(_), _) | (_, Ty::Param(_)) => Ok(()),
            (Ty::Var(x), Ty::Var(y)) if x == y => Ok(()),
            (Ty::Var(x), Ty::Var(y)) => {
                let k = self.kinds[*x].max(self.kinds[*y]);
                self.kinds[*y] = k;
                self.vars[*x] = Some(Ty::Var(*y));
                Ok(())
            }
            (Ty::Var(x), t) | (t, Ty::Var(x)) => {
                let t = t.clone();
                match self.kinds[*x] {
                    1 => {
                        if let Ty::Prim(p) = &t {
                            if !INTS.contains(&p.as_str()) {
                                return Err((Ty::Prim("{integer}".into()), t));
                            }
                        } else if !matches!(t, Ty::Ref(..)) {
                            return Err((Ty::Prim("{integer}".into()), t));
                        }
                    }
                    2 => {
                        if let Ty::Prim(p) = &t {
                            if p != "f32" && p != "f64" {
                                return Err((Ty::Prim("{float}".into()), t));
                            }
                        } else if !matches!(t, Ty::Ref(..)) {
                            return Err((Ty::Prim("{float}".into()), t));
                        }
                    }
                    _ => {}
                }
                if let Ty::Ref(_, inner) = &t {
                    // 参照は透過的: リテラルの型は参照の中身に合わせる。
                    return self.unify2(&Ty::Var(*x), inner);
                }
                if self.occurs(*x, &t) {
                    return Ok(());
                }
                self.vars[*x] = Some(t);
                Ok(())
            }
            (Ty::Ref(_, x), y) | (y, Ty::Ref(_, x)) => {
                let (x, y) = (x.clone(), y.clone());
                self.unify2(&x, &y)
            }
            (Ty::Prim(p), Ty::Prim(q)) => {
                if p == q { Ok(()) } else { Err((a.clone(), b.clone())) }
            }
            // 4.3: 定数実引数は同じ定数なら同じ型、異なる定数は別単相。
            (Ty::Const(x), Ty::Const(y)) => {
                if x == y { Ok(()) } else { Err((a.clone(), b.clone())) }
            }
            (Ty::Dyn(p), Ty::Dyn(q)) => {
                if p == q { Ok(()) } else { Err((a.clone(), b.clone())) }
            }
            (Ty::Named(n, xs), Ty::Named(m, ys)) => {
                if n != m {
                    return Err((a.clone(), b.clone()));
                }
                let (xs, ys) = (xs.clone(), ys.clone());
                for (x, y) in xs.iter().zip(&ys) {
                    self.unify2(x, y)?;
                }
                Ok(())
            }
            (Ty::Tuple(xs), Ty::Tuple(ys)) => {
                let (xs, ys) = (xs.clone(), ys.clone());
                for (x, y) in xs.iter().zip(&ys) {
                    self.unify2(x, y)?;
                }
                Ok(())
            }
            (Ty::Closure { args: xs, ret: rx, .. }, Ty::Closure { args: ys, ret: ry, .. }) => {
                if xs.len() != ys.len() {
                    return Err((a.clone(), b.clone()));
                }
                let (xs, ys, rx, ry) = (xs.clone(), ys.clone(), (**rx).clone(), (**ry).clone());
                for (x, y) in xs.iter().zip(&ys) {
                    self.unify2(x, y)?;
                }
                self.unify2(&rx, &ry)
            }
            (Ty::Slice(x), Ty::Slice(y)) | (Ty::Slice(x), Ty::Array(y)) | (Ty::Array(x), Ty::Slice(y)) | (Ty::Array(x), Ty::Array(y)) => {
                let (x, y) = (x.clone(), y.clone());
                self.unify2(&x, &y)
            }
            (Ty::Unit, Ty::Unit) => Ok(()),
            _ => Ok(()), // 構造が違うものは、近似の限界かもしれないので何も言わない
        }
    }

    /// シグネチャの型パラメータを、呼び出しごとの新しい型変数に置き換える。
    fn inst(&mut self, t: &Ty, map: &mut HashMap<String, Ty>) -> Ty {
        match t {
            Ty::Param(p) => {
                if let Some(v) = map.get(p) {
                    return v.clone();
                }
                let v = self.fresh(0);
                map.insert(p.clone(), v.clone());
                v
            }
            Ty::Named(n, a) => Ty::Named(n.clone(), a.iter().map(|x| self.inst(x, map)).collect()),
            Ty::Ref(m, x) => Ty::Ref(*m, Box::new(self.inst(x, map))),
            Ty::Tuple(xs) => Ty::Tuple(xs.iter().map(|x| self.inst(x, map)).collect()),
            Ty::Array(x) => Ty::Array(Box::new(self.inst(x, map))),
            Ty::Slice(x) => Ty::Slice(Box::new(self.inst(x, map))),
            other => other.clone(),
        }
    }

    /// 明示型引数 (`f::<..>`, 4.3) のうち型と定数の実引数を、宣言のパラメータ名に束縛する。
    /// 効果 (`uses [..]`) と panic (`never` / `may`) の実引数は効果検査 (check.rs) で扱うのでここでは無視する。
    /// 実引数の種別が宣言と並ばない (誤った) 呼び出しでは何も束縛しない (黙る)。
    /// 型の実引数は、書かれた位置 (呼び出し元) の型パラメータ名で解釈する。
    fn seed_targs(&mut self, d: &Def, targs: &[V], map: &mut HashMap<String, Ty>) {
        if targs.is_empty() {
            return;
        }
        let Some(pairs) = crate::check::align_targs(&d.generics, targs) else { return };
        let scope = self.def().gnames();
        for (name, kind, t) in pairs {
            match kind {
                "const" => {
                    if let Some(c) = const_targ(t) {
                        map.insert(name.to_string(), Ty::Const(c));
                    }
                }
                "type" => {
                    map.insert(name.to_string(), from_v(t, &scope));
                }
                _ => {}
            }
        }
    }

    /// 矛盾した 2 つが両方プリミティブ (リテラル型を含む)、または定数実引数どうしの違い
    /// (異なる定数は別単相、4.3) なら、診断のための文字列を返す。
    fn prim_conflict(&self, e: &(Ty, Ty)) -> Option<(String, String)> {
        match (&e.0, &e.1) {
            (Ty::Prim(a), Ty::Prim(b)) => Some((a.clone(), b.clone())),
            (Ty::Const(a), Ty::Const(b)) => Some((a.clone(), b.clone())),
            _ => None,
        }
    }

    // ------------------------------------------------------------ 型の情報
    fn type_def(&self, fqn: &str) -> Option<&'a Def> {
        self.p.types.get(fqn).map(|&i| &self.p.defs[i])
    }

    fn field_ty(&self, recv: &Ty, name: &str) -> Ty {
        let r = self.deep(recv);
        let Ty::Named(fqn, args) = r.strip() else { return Ty::Unknown };
        let Some(td) = self.type_def(fqn) else { return Ty::Unknown };
        if td.kw != "type" {
            return Ty::Unknown;
        }
        let gn = td.gnames();
        let Some(f) = td.fields.iter().find(|f| f.0 == name) else { return Ty::Unknown };
        let t = from_v(&f.1, &gn);
        let order: Vec<&String> = td.generics.iter().filter(|g| g.0 == "type").map(|g| &g.1).collect();
        let mut bind = HashMap::new();
        for (n, a) in order.iter().zip(args) {
            bind.insert((*n).clone(), a.clone());
        }
        subst(&t, &bind)
    }

    /// レシーバの型から、メソッドを探す所有者の列と「その型のメソッド表が完全か」を返す。
    fn owners_for(&self, t: &Ty) -> (Vec<String>, bool) {
        let t = self.deep(t);
        match t.strip() {
            Ty::Prim(p) if p == "str" => (vec!["core.str".into()], true),
            // sig の所有者は `core.bool.bool` / `core.char.char` (型名を末尾に重ねた FQN。`core.num.u8` と違う形)。
            Ty::Prim(p) if p == "bool" => (vec!["core.bool.bool".into()], false),
            Ty::Prim(p) if p == "char" => (vec!["core.char.char".into()], false),
            Ty::Prim(p) => (vec![format!("core.num.{}", p)], false),
            Ty::Slice(_) | Ty::Array(_) => (vec!["core.slice".into(), "core.array".into()], true),
            Ty::Named(n, _) => match n.as_str() {
                "Option" => (vec!["core.option.Option".into()], true),
                "Result" => (vec!["core.result.Result".into()], true),
                "alloc.vec.Vec" => (vec!["alloc.vec.Vec".into(), "core.slice".into()], true),
                "alloc.string.String" => (vec!["alloc.string.String".into(), "core.str".into()], true),
                "alloc.boxed.Box" => (vec![n.clone()], false),
                _ => (vec![n.clone()], self.p.types.contains_key(n)),
            },
            _ => (vec![], false),
        }
    }

    /// 所有者 `owner` のメソッドの `self` に合わせたレシーバの見え方 (Vec → スライス、String → str)。
    fn view_for(&self, recv: &Ty, owner: &str) -> Ty {
        let r = self.deep(recv);
        match (r.strip(), owner) {
            (Ty::Named(n, a), "core.slice") if n == "alloc.vec.Vec" => {
                Ty::Slice(Box::new(a.first().cloned().unwrap_or(Ty::Unknown)))
            }
            (Ty::Named(n, _), "core.str") if n == "alloc.string.String" => Ty::Prim("str".into()),
            _ => r,
        }
    }

    // ------------------------------------------------------------ メソッド呼び出し
    /// `local` は局所変数から始まるパスの呼び出し (`s.m()` / `s.f.m()`) か。その形は従来の効果検査を
    /// 通らない (パスとして構文解析される) ので、型で解決できたときに実体化した効果全体を検査する。
    /// それ以外 (`x.f().m()` などの MCall) は従来の名前ベースの検査が宣言どおりの効果を見るので、
    /// クロージャから推論した部分だけを検査する。
    fn call_method(&mut self, recv: &Ty, base: Option<&str>, name: &str, args: &[Ty], line: usize, targs: &[V], local: bool) -> Ty {
        // トレイトオブジェクト (5.1): `&dyn Tr` のメソッド呼び出しは、Tr とそのスーパートレイトの
        // シグネチャで検査する (Self は `dyn Tr` 自身)。
        let deep = self.deep(recv);
        if let Ty::Dyn(tr) = deep.strip().clone() {
            return self.dyn_method(&tr, &deep, base, name, args, line);
        }
        let (owners, complete) = self.owners_for(recv);
        let mut cands: Vec<(usize, String)> = vec![];
        let escape_all = |this: &mut Self| {
            for a in args {
                this.escape(a);
            }
            this.escape(recv);
        };
        for o in &owners {
            if let Some(m) = self.owners.get(o).and_then(|m| m.get(name)) {
                cands.extend(m.iter().map(|&i| (i, o.clone())));
            }
        }
        if cands.is_empty() {
            if let Some(t) = self.trait_bound_method(recv, name, args, line) {
                return t;
            }
            if let Some(t) = self.derived_method(recv, name, args) {
                return t;
            }
            let exists_elsewhere = self.p.method_idx.contains_key(name);
            if complete && exists_elsewhere && !DERIVED.contains(&name) && !self.trait_methods.contains(name) {
                let mut names: Vec<&String> = owners.iter().filter_map(|o| self.owners.get(o)).flat_map(|m| m.keys()).collect();
                names.sort();
                names.dedup();
                let best = names
                    .iter()
                    .map(|n| (edit_distance(name, n), *n))
                    .filter(|(d, n)| *d <= (name.len().max(n.len()) / 3).max(1) || n.contains(name) || name.contains(n.as_str()))
                    .min_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(b.1)));
                let hint = best.map(|(_, n)| format!("; did you mean `{}`?", n)).unwrap_or_default();
                let shown = self.deep(recv).strip().show();
                self.diag("E1108", format!("type `{}` has no method `{}`{}", shown, name, hint), line);
            }
            escape_all(self);
            return Ty::Unknown;
        }
        let fit: Vec<(usize, String)> = cands.iter().cloned().filter(|(i, _)| self.p.defs[*i].params.len() == args.len() + 1).collect();
        if fit.is_empty() {
            if cands.len() == 1 && complete {
                let d = &self.p.defs[cands[0].0];
                self.diag(
                    "E2022",
                    format!("method `{}` takes {} argument(s) but {} given", name, d.params.len() - 1, args.len()),
                    line,
                );
            }
            escape_all(self);
            return Ty::Unknown;
        }
        // 効果 (4.3 / 5.1): 引数の個数が合う候補 (型で決まった所有者のメソッド) をすべて見る。
        // 検査は全候補が違反するときだけ、クロージャの効果には全候補に共通する部分だけを足す (保守側)。
        // 候補が 1 つに決まるときだけ、`Fn` 境界の具体的な効果とクロージャの効果を比べる (5.1 / 4.3)。
        if let [(di, _)] = fit.as_slice() {
            self.check_closure_bounds(&self.p.defs[*di], 1, args, line);
        }
        let effs: Vec<(String, Vec<String>, Option<String>, Vec<String>, Option<String>)> = fit
            .iter()
            .map(|(i, _)| {
                let c = &self.p.defs[*i];
                let (fu, fp, du, dm) = self.callee_effects(c, 1, args, targs);
                let (cu, cp) = if local { (fu.clone(), fp.clone()) } else { (du, dm.then(|| "may".to_string())) };
                (c.name.clone(), fu, fp, cu, cp)
            })
            .collect();
        self.record_effects(line, effs);
        let (di, owner) = fit[0].clone();
        let d = &self.p.defs[di];
        let gn = d.gnames();
        self.add_edge(d.name.clone());
        if let Some(b) = base {
            if matches!(from_v(&d.params[0].1, &gn), Ty::Ref(true, _)) {
                self.need_mut(b, name, line);
            }
        }
        let mut map: HashMap<String, Ty> = HashMap::new();
        self.seed_targs(d, targs, &mut map);
        let self_ty = from_v(&d.params[0].1, &gn);
        let self_ty = self.inst(&self_ty, &mut map);
        let view = self.view_for(recv, &owner);
        let _ = self.unify2(&self_ty, &view);
        for (k, a) in args.iter().enumerate() {
            let pt = from_v(&d.params[k + 1].1, &gn);
            let pt = self.inst(&pt, &mut map);
            if let Err(e) = self.unify2(&pt, a) {
                if fit.len() == 1 {
                    if let Some((want, got)) = self.prim_conflict(&e) {
                        self.diag(
                            "E2023",
                            format!("argument {} of `{}`: expected `{}`, found `{}`", k + 1, name, want, got),
                            line,
                        );
                    }
                }
            }
        }
        // Fn 境界 (6 の 1): `map_err(f)` などの引数のクロージャ・関数項目を、境界の引数・戻り値と単一化する
        // (`call_def` と同じ検査)。宣言の添字は `self` の分だけずれる。
        let mut cap_bad = false;
        for (k, a) in args.iter().enumerate() {
            let Some(p) = d.params.get(k + 1) else { break };
            let Ty::Param(pname) = from_v(&p.1, &gn) else { continue };
            if self.fn_bound(d, &pname).is_empty() {
                continue;
            }
            if let cl @ Ty::Closure { .. } = self.resolve(a) {
                self.unify_fn_bound(d, k, &pname, &cl, &mut map, line, &mut cap_bad);
            }
        }
        self.check_bounds(d, &map, line, false);
        let ret = from_v(&d.ret, &gn);
        self.inst(&ret, &mut map)
    }

    /// 派生トレイト (`DERIVED`) のメソッドの戻り値。所有者の sig にメソッドがないときだけ使う。
    /// `Self` はレシーバの型 (参照は透過)。戻り値が分からない名前 (`call` など) は None (黙る)。
    /// `min` / `max` の引数は `Self` と単一化し、それ以外の引数は従来どおり escape する。
    fn derived_method(&mut self, recv: &Ty, name: &str, args: &[Ty]) -> Option<Ty> {
        let self_ty = self.deep(recv).strip().clone();
        let ret = match name {
            "clone" => self_ty,
            "min" | "max" => {
                for a in args {
                    let _ = self.unify2(&self_ty, a);
                }
                return Some(self_ty);
            }
            "eq" | "ne" | "lt" | "le" | "gt" | "ge" => Ty::Prim("bool".into()),
            "hash" => Ty::Unit,
            "to_string" => Ty::Named("alloc.string.String".into(), vec![]),
            "cmp" => Ty::Named("core.cmp.Ordering".into(), vec![]),
            "partial_cmp" => Ty::Named("Option".into(), vec![Ty::Named("core.cmp.Ordering".into(), vec![])]),
            _ => return None,
        };
        for a in args {
            self.escape(a);
        }
        Some(ret)
    }

    /// レシーバが型パラメータ `T` で、境界のトレイトがそのメソッドを宣言しているとき、トレイトのシグネチャで呼び出しを検査する。
    fn trait_bound_method(&mut self, recv: &Ty, name: &str, args: &[Ty], line: usize) -> Option<Ty> {
        let deep = self.deep(recv);
        let Ty::Param(pn) = deep.strip().clone() else { return None };
        let d = self.def();
        let bounds: Vec<String> = d
            .generics
            .iter()
            .filter(|g| g.0 == "type" && g.1 == pn)
            .flat_map(|g| items(&g.2).iter().filter_map(|b| if let V::L(bx) = b { Some(st(&bx[0])) } else { None }).collect::<Vec<_>>())
            .collect();
        for tr in bounds {
            let Some(&ti) = self.p.types.get(&tr) else { continue };
            let td = &self.p.defs[ti];
            let Some((_, params, ret)) = td.trait_sigs.iter().find(|s| s.0 == name) else { continue };
            // 5.3: `core.ops.Fn` 系の境界を持つパラメータの呼び出しは間接呼び出し。この関数の
            // 呼び出し箇所で渡されたクロージャ (単一化で分かったもの) を候補の辺にする。
            // 渡されたクロージャが分からなければ、辺を足さない (黙る)。
            if tr.starts_with("core.ops.Fn") {
                let key = (self.def().name.clone(), pn.clone());
                if let Some(nodes) = self.fn_closures.get(&key) {
                    for n in nodes.clone() {
                        self.add_edge(n);
                    }
                }
            }
            if params.len() != args.len() + 1 {
                self.diag(
                    "E2022",
                    format!("method `{}` of trait {} takes {} argument(s) but {} given", name, tr, params.len().saturating_sub(1), args.len()),
                    line,
                );
                return Some(Ty::Unknown);
            }
            let mut gn = td.gnames();
            gn.insert("Self".to_string());
            let mut map: HashMap<String, Ty> = HashMap::new();
            map.insert("Self".to_string(), deep.clone());
            for (k, a) in args.iter().enumerate() {
                let pt = from_v(&params[k + 1].1, &gn);
                let pt = self.inst(&pt, &mut map);
                if let Err(e) = self.unify2(&pt, a) {
                    if let Some((want, got)) = self.prim_conflict(&e) {
                        self.diag(
                            "E2023",
                            format!("argument {} of `{}`: expected `{}`, found `{}`", k + 1, name, want, got),
                            line,
                        );
                    }
                }
            }
            let r = from_v(ret, &gn);
            return Some(self.inst(&r, &mut map));
        }
        None
    }

    /// `dyn Tr` のメソッド呼び出しを、トレイト (とそのスーパートレイト) のシグネチャで検査する (4.3 / 5.1)。
    /// 引数の個数 (E2022) と型 (E2023) を検査し、戻り値の型を式へ流す。`Self` は `dyn Tr` 自身。
    /// トレイトに無いメソッド名では黙る (名前の存在は名前ベースの従来の検査が見る)。
    fn dyn_method(&mut self, tr: &str, recv: &Ty, base: Option<&str>, name: &str, args: &[Ty], line: usize) -> Ty {
        let escape_all = |this: &mut Self| {
            for a in args {
                this.escape(a);
            }
        };
        let Some(root) = crate::check::resolve_trait_fqn(self.p, tr) else {
            escape_all(self);
            return Ty::Unknown;
        };
        // トレイト自身に無ければスーパートレイト (`requires`) を推移的に辿る (5.1)。
        // `root` は 5.3 の候補計算 (impl の探索) にも使うので残しておく。
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue = vec![root.clone()];
        while let Some(cur) = queue.pop() {
            if !seen.insert(cur.clone()) {
                continue;
            }
            let Some(&ti) = self.p.types.get(&cur) else { continue };
            let td = &self.p.defs[ti];
            if td.kw != "trait" {
                continue;
            }
            let Some((_, params, ret)) = td.trait_sigs.iter().find(|s| s.0 == name) else {
                for (sup, _) in &td.requires {
                    if let Some(fq) = crate::check::resolve_trait_fqn(self.p, sup) {
                        queue.push(fq);
                    }
                }
                continue;
            };
            if params.is_empty() {
                // `self` の無いシグネチャは不正 (E2039 がトレイト側で報告する)。ここでは黙る。
                escape_all(self);
                return Ty::Unknown;
            }
            // 5.3: `dyn` 越しの呼び出しの候補は、`dyn Tr` の `Tr` (`root`) の impl を持つ型の
            // メソッドすべて。impl が無ければ候補は分からないので、辺を足さない (黙る)。
            self.add_dyn_candidates(&root, name);
            if params.len() != args.len() + 1 {
                self.diag(
                    "E2022",
                    format!(
                        "method `{}` of trait {} takes {} argument(s) but {} given",
                        name,
                        cur,
                        params.len().saturating_sub(1),
                        args.len()
                    ),
                    line,
                );
                escape_all(self);
                return Ty::Unknown;
            }
            let mut gn = td.gnames();
            gn.insert("Self".to_string());
            let mut map: HashMap<String, Ty> = HashMap::new();
            map.insert("Self".to_string(), recv.clone());
            if let Some(b) = base {
                if matches!(from_v(&params[0].1, &gn), Ty::Ref(true, _)) {
                    self.need_mut(b, name, line);
                }
            }
            for (k, a) in args.iter().enumerate() {
                let pt = from_v(&params[k + 1].1, &gn);
                let pt = self.inst(&pt, &mut map);
                if let Err(e) = self.unify2(&pt, a) {
                    if let Some((want, got)) = self.prim_conflict(&e) {
                        self.diag(
                            "E2023",
                            format!("argument {} of `{}`: expected `{}`, found `{}`", k + 1, name, want, got),
                            line,
                        );
                    }
                }
            }
            let r = from_v(ret, &gn);
            return self.inst(&r, &mut map);
        }
        escape_all(self);
        Ty::Unknown
    }

    /// `dyn Tr` のメソッド呼び出し (5.3) の候補: `impl Tr for T` のメソッド `name` をすべて辺にする。
    /// トレイトの impl が無い (候補が計算できない) ときは、何も辺にしない (黙る)。
    /// トレイト名が解決できない呼び出しは、この関数に来る前に黙って帰る。
    fn add_dyn_candidates(&mut self, root: &str, name: &str) {
        let mut cands: Vec<String> = vec![];
        for (tb, tgt) in &self.p.impls {
            if crate::check::resolve_trait_fqn(self.p, tb).as_deref() != Some(root) {
                continue;
            }
            // 基本型への impl のメソッドは `core.num.u32` などが所有者になる (3.2)。
            for owner in impl_owner_keys(tgt) {
                if let Some(ms) = self.owners.get(&owner) {
                    if let Some(idx) = ms.get(name) {
                        for &i in idx {
                            cands.push(self.p.defs[i].name.clone());
                        }
                    }
                }
            }
        }
        for c in cands {
            self.add_edge(c);
        }
    }

    /// `&mut self` のメソッドを呼ぶには、レシーバの局所変数が `let mut` か、`&mut` の参照でなければならない。
    fn need_mut(&mut self, base: &str, method: &str, line: usize) {
        let Some(t) = self.lookup(base) else { return };
        match self.resolve(&t) {
            Ty::Ref(false, _) => self.diag(
                "E3031",
                format!("cannot call the `&mut self` method `{}` through the shared reference `{}`", method, base),
                line,
            ),
            Ty::Ref(true, _) | Ty::Unknown | Ty::Var(_) | Ty::Param(_) | Ty::Never => {}
            _ => {
                if !self.mutable.contains(base) {
                    self.diag(
                        "E3031",
                        format!("cannot borrow `{}` as mutable to call `{}`: declare it with `let mut {}`", base, method, base),
                        line,
                    );
                }
            }
        }
    }

    /// 型パラメータの境界 (`T: core.cmp.Ord` など) を、解決された型が満たすか。
    fn check_bounds(&mut self, d: &Def, map: &HashMap<String, Ty>, line: usize, direct_only_skip: bool) {
        let _ = direct_only_skip;
        for g in &d.generics {
            if g.0 != "type" || items(&g.2).is_empty() {
                continue;
            }
            let Some(t) = map.get(&g.1) else { continue };
            let t = self.deep(t);
            let t = t.strip();
            for b in items(&g.2) {
                let V::L(bx) = b else { continue };
                let tr = st(&bx[0]);
                if tr == "core.marker.Send" || tr == "core.marker.Sync" || tr.starts_with("core.ops.Fn") {
                    continue;
                }
                let known = self.p.types.get(&tr).map_or(false, |&i| self.p.defs[i].kw == "trait")
                    || crate::check::DERIVE_TRAITS.iter().any(|(_, v)| *v == tr);
                if !known {
                    continue;
                }
                let bad = match t {
                    Ty::Named(n, _) => self.p.types.contains_key(n) && !self.implements(n, &tr),
                    Ty::Prim(p) if p == "f32" || p == "f64" => {
                        matches!(tr.as_str(), "core.cmp.Eq" | "core.cmp.Ord" | "core.hash.Hash")
                    }
                    _ => false,
                };
                if bad {
                    self.diag(
                        "E2030",
                        format!("the trait bound `{}: {}` of {} is not satisfied by {}", g.1, tr, d.name, t.show()),
                        line,
                    );
                }
            }
        }
    }

    fn implements(&self, ty: &str, tr: &str) -> bool {
        let Some(&i) = self.p.types.get(ty) else { return true };
        if self.p.impls.contains(&(tr.to_string(), ty.to_string())) {
            return true;
        }
        self.p.defs[i].derive.iter().any(|x| crate::check::DERIVE_TRAITS.iter().any(|(k, v)| k == x && *v == tr))
    }

    // ------------------------------------------------------------ 式
    /// 式を推論する。計測が有効なら結果を記録する (`cov_record`)。本体は `infer_node`。
    fn infer(&mut self, e: &V) -> Ty {
        let t = self.infer_node(e);
        if self.cov.is_some() {
            self.cov_record(e, &t);
        }
        t
    }

    fn infer_node(&mut self, e: &V) -> Ty {
        let f = nf(e);
        match nname(e) {
            "Block" => {
                self.scopes.push(HashMap::new());
                let mut last = Ty::Unit;
                let stmts = items(&f[0]);
                for (i, s) in stmts.iter().enumerate() {
                    let t = self.stmt(s);
                    if i + 1 == stmts.len() && nname(s) == "ExprStmt" {
                        last = t;
                    }
                }
                self.scopes.pop();
                last
            }
            "Lit" => match st(&f[0]).as_str() {
                "int" => {
                    let t = match &f[2] {
                        V::S(s) => Ty::Prim(s.clone()),
                        _ => self.fresh(1),
                    };
                    self.lits.push(("int", st(&f[1]), t.clone(), nline(e)));
                    t
                }
                "float" => {
                    let t = match &f[1] {
                        V::S(s) => Ty::Prim(s.clone()),
                        _ => self.fresh(2),
                    };
                    self.lits.push(("float", st(&f[2]), t.clone(), nline(e)));
                    t
                }
                "bool" => Ty::Prim("bool".into()),
                "char" => Ty::Prim("char".into()),
                "byte" => Ty::Prim("u8".into()),
                "str" => Ty::Ref(false, Box::new(Ty::Prim("str".into()))),
                "bstr" => Ty::Ref(false, Box::new(Ty::Slice(Box::new(Ty::Prim("u8".into()))))),
                "unit" => Ty::Unit,
                _ => Ty::Unknown,
            },
            "Path" => self.path(&strs(&f[0])),
            "Call" => self.call(&f[0], items(&f[1]), nline(e), items(&f[2])),
            "MCall" => {
                let recv = self.infer(&f[0]);
                let args: Vec<Ty> = items(&f[2]).iter().map(|a| self.infer(a)).collect();
                let base = self.base_local(&f[0]);
                self.call_method(&recv, base.as_deref(), &st(&f[1]), &args, nline(e), items(&f[3]), false)
            }
            "Field" => {
                let r = self.infer(&f[0]);
                self.field_ty(&r, &st(&f[1]))
            }
            "Try" => {
                let t = self.infer(&f[0]);
                let inner = match self.deep(&t).strip() {
                    Ty::Named(n, a) if n == "Result" || n == "Option" => a.first().cloned().unwrap_or(Ty::Unknown),
                    _ => Ty::Unknown,
                };
                // 4.1: `?` はエラー型が完全一致するときだけ使える。
                self.check_try_err(&t, nline(e));
                inner
            }
            "Index" => {
                // 添字は panic しうる (クロージャの本体では、そのクロージャの `panics may` の根拠にする)。
                if let Some(acc) = self.eff_acc.last_mut() {
                    acc.1 = true;
                }
                let t = self.infer(&f[0]);
                let idx = self.infer(&f[1]);
                let want = Ty::Prim("usize".into());
                if let Err(err) = self.unify2(&want, &idx) {
                    if let Some((_, got)) = self.prim_conflict(&err) {
                        self.diag("E2025", format!("an index must be `usize`, found `{}`", got), nline(e));
                    }
                }
                match self.deep(&t).strip() {
                    Ty::Array(x) | Ty::Slice(x) => (**x).clone(),
                    _ => Ty::Unknown,
                }
            }
            "Unary" => {
                let t = self.infer(&f[1]);
                match st(&f[0]).as_str() {
                    "&" => Ty::Ref(false, Box::new(t)),
                    "&mut" => Ty::Ref(true, Box::new(t)),
                    "*" => match self.resolve(&t) {
                        Ty::Ref(_, x) => *x,
                        _ => Ty::Unknown,
                    },
                    _ => t,
                }
            }
            "Bin" => {
                let a = self.infer(&f[1]);
                let b = self.infer(&f[2]);
                let op = st(&f[0]);
                if op == "&&" || op == "||" {
                    return Ty::Prim("bool".into());
                }
                if let Err(err) = self.unify2(&a, &b) {
                    if let Some((l, r)) = self.prim_conflict(&err) {
                        self.diag("E2024", format!("operands of `{}` have different types: `{}` and `{}`", op, l, r), nline(e));
                    }
                }
                match op.as_str() {
                    "==" | "!=" | "<" | "<=" | ">" | ">=" => Ty::Prim("bool".into()),
                    _ => a,
                }
            }
            "StructLit" => {
                let segs: Vec<String> = st(&f[0]).split('.').map(String::from).collect();
                let fq = self.p.expand(&segs, &self.def().file).join(".");
                let td = self.type_def(&fq).filter(|td| td.kw == "type");
                let Some(td) = td else {
                    // 構造体が分からない (または type でない): フィールドの値の型は決められないので escape する。
                    for x in items(&f[1]) {
                        let vt = self.infer(&items(x)[1]);
                        self.escape(&vt);
                    }
                    return if self.p.types.contains_key(&fq) { Ty::Named(fq, vec![]) } else { Ty::Unknown };
                };
                // ジェネリクスの型パラメータを新しい型変数にして、フィールドの宣言型と値の型を単一化する。
                // 決まった型引数はリテラル式の型になり、以降のフィールド・メソッド検査に流れる。
                let order: Vec<&String> = td.generics.iter().filter(|g| g.0 == "type").map(|g| &g.1).collect();
                let args: Vec<Ty> = order.iter().map(|_| self.fresh(0)).collect();
                for x in items(&f[1]) {
                    let vt = self.infer(&items(x)[1]);
                    let declared = self.field_ty(&Ty::Named(fq.clone(), args.clone()), &st(&items(x)[0]));
                    if let Err(err) = self.unify2(&declared, &vt) {
                        if let Some((want, got)) = self.type_conflict(&err) {
                            self.diag(
                                "E2023",
                                format!("field `{}` of `{}`: expected `{}`, found `{}`", st(&items(x)[0]), fq, want, got),
                                nline(e),
                            );
                        }
                    }
                }
                // 型引数はここでは escape しない。あとから注釈や呼び出しで決まるかもしれない
                // (決まらなかったリテラルは finish_literals が E2027 にする)。
                Ty::Named(fq, args)
            }
            "TupleE" => {
                let ts: Vec<Ty> = items(&f[0]).iter().map(|x| self.infer(x)).collect();
                if ts.is_empty() { Ty::Unit } else { Ty::Tuple(ts) }
            }
            "ArrayE" => {
                let ts: Vec<Ty> = items(&f[0]).iter().map(|x| self.infer(x)).collect();
                let elem = self.fresh(0);
                for t in &ts {
                    let _ = self.unify2(&elem, t);
                }
                Ty::Array(Box::new(elem))
            }
            "If" => {
                self.infer(&f[0]);
                let a = self.infer(&f[1]);
                let has_else = !is_nil(&f[2]);
                let b = if has_else { self.infer(&f[2]) } else { Ty::Unit };
                if a == Ty::Never {
                    return b;
                }
                // 6 の 2: 両方の枝は同じ型になる。`Ok(..)` と `Err(..)` の枝の型変数は、互いに一致させて決まる。
                if has_else && b != Ty::Never {
                    self.join_arm(&a, &b);
                }
                a
            }
            "Match" => {
                let s = self.infer(&f[0]);
                let mut out = Ty::Unknown;
                for arm in items(&f[1]) {
                    self.scopes.push(HashMap::new());
                    self.bind_pat(&items(arm)[0], &s);
                    let t = self.infer(&items(arm)[1]);
                    self.scopes.pop();
                    if t == Ty::Never {
                        continue;
                    }
                    if out == Ty::Unknown {
                        out = t;
                    } else {
                        // 6 の 2: 他の腕と型を一致させる (`Some(v) => Ok(v)` と `None => Err(a)` など)。
                        self.join_arm(&out, &t);
                    }
                }
                out
            }
            "For" => {
                let it = self.infer(&f[1]);
                self.scopes.push(HashMap::new());
                let elem = match self.deep(&it).strip() {
                    Ty::Named(n, a) if n == "core.iter.Range" => a.first().cloned().unwrap_or(Ty::Unknown),
                    _ => Ty::Unknown,
                };
                self.bind_pat(&f[0], &elem);
                self.infer(&f[2]);
                self.scopes.pop();
                Ty::Unit
            }
            "Closure" => {
                // パラメータと戻り値の型は注釈が必須なので分かる (関数自身の型パラメータは Unknown)。
                let gn = self.def().gnames();
                let mut args: Vec<Ty> = vec![];
                // クロージャの本体は、呼び出しグラフの独立した節にする (5.3)。外側の関数に
                // 帰属させる従来の辺は `add_edge` が維持するので、既存の検査は変わらない。
                let node = format!("{}#{}", self.def().name.clone(), self.closure_seq);
                self.closure_seq += 1;
                self.closure_nodes.push(node.clone());
                let ptys: Vec<V> = items(&f[0]).iter().map(|p| items(p).get(1).cloned().unwrap_or(V::Nil)).collect();
                self.closure_frames.insert(node.clone(), (ptys, f[3].clone()));
                self.eff_acc.push(Default::default());
                self.scopes.push(HashMap::new());
                for p in items(&f[0]) {
                    let ty = subst_params_unknown(&from_v(&items(p)[1], &gn));
                    self.bind(&st(&items(p)[0]), ty.clone());
                    args.push(ty);
                }
                let ret = subst_params_unknown(&from_v(&f[1], &gn));
                // クロージャの中の `?` は、クロージャの戻り値型のエラー型と比べる。
                self.rets.push(ret.clone());
                let body = self.infer(&f[3]);
                self.rets.pop();
                self.scopes.pop();
                self.closure_nodes.pop();
                // クロージャの `uses` / `panics` は本体から推論する (4.3)。型で解決できた呼び出しと添字だけ。
                let effs = self.eff_acc.pop().unwrap_or_default();
                // 内側のクロージャが確実でないなら、それを含む外側のクロージャも確実でない。
                if effs.2 {
                    if let Some(outer) = self.eff_acc.last_mut() {
                        outer.2 = true;
                    }
                }
                self.closure_effs.insert(node.clone(), effs);
                if let Err(err) = self.unify2(&ret, &body) {
                    if let Some((want, got)) = self.ann_conflict(&ret, &body, &err) {
                        self.diag(
                            "E2020",
                            format!("type mismatch in return value of closure: expected {}, found {}{}", want, got, conv_hint(&want, &got)),
                            nline(e),
                        );
                    }
                }
                Ty::Closure { args, ret: Box::new(ret), cap: closure_cap(&f[2]), id: Some(node) }
            }
            // `while` は値を持たない (unit)。条件と本体は従来どおり推論する。
            "While" => {
                self.infer(&f[0]);
                self.infer(&f[1]);
                Ty::Unit
            }
            // `loop` の値は `break` の値で決まる。ただし文法の `break` は値を取らない (`Break` は子を持たない)
            // ので、この loop を抜ける `break` があれば unit、なければ発散 (`!`) とする。
            "Loop" => {
                self.infer(&f[0]);
                if has_break_out(&f[0]) { Ty::Unit } else { Ty::Never }
            }
            "Return" | "Break" | "Continue" => {
                if nname(e) == "Return" && !is_nil(&f[0]) {
                    let v = self.infer(&f[0]);
                    let d = self.def();
                    let gn = d.gnames();
                    let ret = subst_params_unknown(&from_v(&d.ret, &gn));
                    let _ = self.unify2(&ret, &v);
                }
                Ty::Never
            }
            _ => {
                // While / Loop / Unsafe / その他: 子の式をたどる (パターンは除く)。
                for c in crate::check::children(e) {
                    if !crate::check::is_pat(c) {
                        self.infer(c);
                    }
                }
                Ty::Unknown
            }
        }
    }

    fn stmt(&mut self, s: &V) -> Ty {
        let f = nf(s);
        match nname(s) {
            "Let" => {
                let rhs = self.infer(&f[3]);
                let gn = self.def().gnames();
                let ty = if !is_nil(&f[2]) {
                    let ann = subst_params_unknown(&from_v(&f[2], &gn));
                    if let Err(err) = self.unify2(&ann, &rhs) {
                        if let Some((want, got)) = self.ann_conflict(&ann, &rhs, &err) {
                            self.diag("E2020", format!("type mismatch in let binding: expected {}, found {}{}", want, got, conv_hint(&want, &got)), nline(s));
                        }
                    }
                    ann
                } else {
                    rhs
                };
                if matches!(f[1], V::B(true)) {
                    let mut b = vec![];
                    pat_binds(&f[0], &mut b);
                    self.mutable.extend(b);
                }
                self.bind_pat(&f[0], &ty);
                Ty::Unit
            }
            "Assign" => {
                let target = self.infer(&f[0]);
                let value = self.infer(&f[1]);
                // 変数への代入は従来の検査 (E2020) が見る。フィールド・添字への代入をここで見る。
                if nname(&f[0]) != "Path" || strs(&nf(&f[0])[0]).len() > 1 {
                    if let Err(e) = self.unify2(&target, &value) {
                        if let Some((want, got)) = self.prim_conflict(&e) {
                            self.diag("E2024", format!("assignment: expected `{}`, found `{}`", want, got), nline(&f[1]));
                        }
                    }
                } else {
                    let _ = self.unify2(&target, &value);
                }
                Ty::Unit
            }
            _ => self.infer(&f[0]),
        }
    }

    fn bind_pat(&mut self, pat: &V, ty: &Ty) {
        let f = nf(pat);
        match nname(pat) {
            "PBind" => self.bind(&st(&f[0]), ty.clone()),
            "PRef" => self.bind_pat(&f[0], ty),
            "PTuple" => {
                let ts: Vec<Ty> = match self.deep(ty).strip() {
                    Ty::Tuple(ts) => ts.clone(),
                    _ => vec![],
                };
                for (i, p) in items(&f[0]).iter().enumerate() {
                    let t = ts.get(i).cloned().unwrap_or(Ty::Unknown);
                    self.bind_pat(p, &t);
                }
            }
            "PVariant" => {
                let segs = strs(&f[0]);
                let payload: Vec<Ty> = self.variant_payload(&segs, ty);
                for (i, p) in items(&f[1]).iter().enumerate() {
                    let t = payload.get(i).cloned().unwrap_or(Ty::Unknown);
                    self.bind_pat(p, &t);
                }
            }
            _ => {
                let mut b = vec![];
                pat_binds(pat, &mut b);
                for n in b {
                    self.bind(&n, Ty::Unknown);
                }
            }
        }
    }

    fn variant_payload(&self, segs: &[String], ty: &Ty) -> Vec<Ty> {
        let name = segs.last().cloned().unwrap_or_default();
        let t = self.deep(ty);
        let args = match t.strip() {
            Ty::Named(_, a) => a.clone(),
            _ => vec![],
        };
        if segs.len() == 1 {
            match name.as_str() {
                "Some" | "Ok" => return vec![args.first().cloned().unwrap_or(Ty::Unknown)],
                "Err" => return vec![args.get(1).cloned().unwrap_or(Ty::Unknown)],
                _ => {}
            }
        }
        if let Some(r) = self.p.resolve(segs, &self.def().file) {
            if let Target::Variant(en, vn) = &r.target {
                if let Some(td) = self.type_def(en) {
                    if let Some((_, payload)) = td.variants.iter().find(|v| &v.0 == vn) {
                        let gn = td.gnames();
                        let order: Vec<&String> = td.generics.iter().filter(|g| g.0 == "type").map(|g| &g.1).collect();
                        let mut bind = HashMap::new();
                        for (n, a) in order.iter().zip(&args) {
                            bind.insert((*n).clone(), a.clone());
                        }
                        return payload.iter().map(|t| subst(&from_v(t, &gn), &bind)).collect();
                    }
                }
            }
        }
        vec![]
    }

    /// パス式の型。局所変数 (+ フィールド連鎖) か、定数、列挙子。
    fn path(&mut self, segs: &[String]) -> Ty {
        if let Some(mut t) = self.lookup(&segs[0]) {
            for s in &segs[1..] {
                t = self.field_ty(&t, s);
            }
            return t;
        }
        match self.p.resolve(segs, &self.def().file) {
            Some(r) if r.rest.is_empty() => match r.target {
                Target::Const(i) => from_v(&self.p.defs[i].ret, &HashSet::new()),
                Target::Fn(i) | Target::Sig(i) => self.fn_item_ty(i),
                Target::Variant(en, vn) => {
                    if en == "Option" && vn == "None" {
                        let v = self.fresh(0);
                        Ty::Named("Option".into(), vec![v])
                    } else if self.p.types.contains_key(&en) {
                        Ty::Named(en, vec![])
                    } else {
                        Ty::Unknown
                    }
                }
                _ => Ty::Unknown,
            },
            _ => Ty::Unknown,
        }
    }

    /// 関数項目を値として使う (`map_err(json.oom)`) ときの関数型。引数と戻り値は宣言の型から取り、
    /// 型パラメータを含むものは Unknown にする (そこから先は黙る)。`id` を持たないので、クロージャの
    /// 本体に基づく効果の検査 (`closure_effect_bind` / `check_closure_bounds`) の対象にはならない。
    /// メソッド (`self` を持つ定義) は、`Self` の解決が要るので Unknown にする。
    fn fn_item_ty(&self, i: usize) -> Ty {
        let d = &self.p.defs[i];
        if d.is_method() {
            return Ty::Unknown;
        }
        let gn = d.gnames();
        let args = d.params.iter().map(|p| subst_params_unknown(&from_v(&p.1, &gn))).collect();
        let ret = subst_params_unknown(&from_v(&d.ret, &gn));
        Ty::Closure { args, ret: Box::new(ret), cap: Cap::Fn, id: None }
    }

    fn call(&mut self, callee: &V, args: &[V], line: usize, targs: &[V]) -> Ty {
        let argt: Vec<Ty> = args.iter().map(|a| self.infer(a)).collect();
        // 構造体リテラルの引数: 従来の検査は型引数を見ないので、呼び出し側でも型引数の矛盾を見る。
        let sl: Vec<bool> = args.iter().map(|a| nname(a) == "StructLit").collect();
        if nname(callee) != "Path" {
            // `fn(..) {..}(x)` のように、クロージャを直接呼ぶ形。
            let t = self.infer(callee);
            if let Some(r) = self.call_closure(&t, &argt, line) {
                return r;
            }
            argt.iter().for_each(|a| self.escape(a));
            return Ty::Unknown;
        }
        let segs = strs(&nf(callee)[0]);
        // `v.items.push(x)` のように、局所変数から始まるパスはメソッド呼び出し。
        if self.lookup(&segs[0]).is_some() {
            if segs.len() < 2 {
                // `f(x)`: クロージャを持つ局所変数の直接呼び出し。
                let t = self.lookup(&segs[0]).unwrap();
                if let Some(r) = self.call_closure(&t, &argt, line) {
                    return r;
                }
                return Ty::Unknown;
            }
            let mut t = self.lookup(&segs[0]).unwrap();
            for s in &segs[1..segs.len() - 1] {
                t = self.field_ty(&t, s);
            }
            return self.call_method(&t, Some(&segs[0]), &segs[segs.len() - 1], &argt, line, targs, true);
        }
        let _ = &argt;
        let Some(r) = self.p.resolve(&segs, &self.def().file) else {
            argt.iter().for_each(|a| self.escape(a));
            return Ty::Unknown;
        };
        // 6 の 3: `Enum.Variant.m()` は、ペイロードのない変種の値 (型 `Enum`) へのメソッド呼び出し。
        // 型引数のない列挙型の変種だけを扱う (それ以外は従来どおり黙る)。
        if r.rest.len() == 1 {
            if let Target::Variant(en, vn) = &r.target {
                let unit_variant = self.type_def(en).is_some_and(|td| {
                    td.generics.iter().all(|g| g.0 != "type") && td.variants.iter().any(|v| &v.0 == vn && v.1.is_empty())
                });
                if unit_variant && self.p.types.contains_key(en) {
                    let recv = Ty::Named(en.clone(), vec![]);
                    let m = r.rest[0].clone();
                    return self.call_method(&recv, None, &m, &argt, line, targs, false);
                }
            }
        }
        if !r.rest.is_empty() {
            argt.iter().for_each(|a| self.escape(a));
            return Ty::Unknown;
        }
        match r.target {
            Target::Variant(en, vn) => {
                let a0 = argt.first().cloned().unwrap_or(Ty::Unknown);
                match (en.as_str(), vn.as_str()) {
                    ("Option", "Some") => Ty::Named("Option".into(), vec![a0]),
                    ("Result", "Ok") => {
                        let e = self.fresh(0);
                        Ty::Named("Result".into(), vec![a0, e])
                    }
                    ("Result", "Err") => {
                        let t = self.fresh(0);
                        Ty::Named("Result".into(), vec![t, a0])
                    }
                    _ if self.p.types.contains_key(&en) => Ty::Named(en, vec![]),
                    _ => Ty::Unknown,
                }
            }
            Target::Native(ref fq) | Target::Value(ref fq) if self.p.sigs.contains_key(fq) => {
                let i = self.p.sigs[fq];
                self.call_def(i, &argt, line, &sl, targs)
            }
            Target::Fn(i) | Target::Sig(i) => self.call_def(i, &argt, line, &sl, targs),
            _ => {
                argt.iter().for_each(|a| self.escape(a));
                Ty::Unknown
            }
        }
    }

    /// 関数・関連関数の呼び出し: 型パラメータを新しい型変数にし、引数と単一化して、戻り値の型を求める。
    ///
    /// `F: core.ops.Fn<Args, R, E, P>` の境界を持つ型パラメータ (fold の `F` など) にはクロージャを渡せる。
    /// 対応する引数の型がクロージャに決まるときは、境界の引数タプル・戻り値型・capture の能力を検査する。
    /// それ以外 (クロージャでない・型変数が解決しない) のときは従来どおり、この呼び出しに渡した
    /// リテラルなどの型変数は決まらなくてもエラーにしない (`escape`)。
    ///
    /// `sl` は、各引数の式が構造体リテラルかどうか。従来の検査は構造体リテラルの型引数を見ない
    /// (名前しか比べない) ので、同じ名前どうしの型引数の矛盾はここで報告する。
    fn call_def(&mut self, i: usize, argt: &[Ty], line: usize, sl: &[bool], targs: &[V]) -> Ty {
        let d = &self.p.defs[i];
        self.add_edge(d.name.clone());
        self.check_closure_bounds(d, 0, argt, line);
        // 効果 (4.3 / 5.1): 宣言どおりの効果は従来の検査 (check.rs の visit) が見るので、ここでは
        // `Fn` 境界に渡したクロージャから推論した部分だけを検査の義務にする。クロージャの中では
        // 実体化した効果全体をそのクロージャの効果に足す。
        let (fu, fp, du, dm) = self.callee_effects(d, 0, argt, targs);
        self.record_effects(line, vec![(d.name.clone(), fu, fp, du, dm.then(|| "may".to_string()))]);
        let gn = d.gnames();
        // 明示型引数 (`f::<..>`, 4.3) のうち型・定数の実引数を、宣言のパラメータに束縛する。
        // 効果・panic の実引数は効果検査 (check.rs の inst_def_effects) で使う。
        // 実引数が並ばない (誤った) 呼び出しでは何も束縛せず、引数からの推論に任せる (黙る)。
        let mut map: HashMap<String, Ty> = HashMap::new();
        self.seed_targs(d, targs, &mut map);
        // Fn 境界を受け取る引数の位置と、渡されたクロージャの型 (決まっているものだけ)。
        let mut fn_args: Vec<(usize, String)> = vec![];
        let mut closures: HashMap<usize, Ty> = HashMap::new();
        for (k, a) in argt.iter().enumerate() {
            let Some(p) = d.params.get(k) else { break };
            if let Ty::Param(name) = from_v(&p.1, &gn) {
                let bounds = self.fn_bound(d, &name);
                if !bounds.is_empty() {
                    fn_args.push((k, name));
                    if let t @ Ty::Closure { .. } = self.resolve(a) {
                        closures.insert(k, t);
                    }
                }
            }
        }
        if fn_args.len() != closures.len() {
            // クロージャの型が追えない呼び出し: 引数の型変数は決まらなくてもエラーにしない。
            argt.iter().for_each(|a| self.escape(a));
        }
        // 5.3: `Fn` 境界のパラメータへの引数は、呼び出し先の本体がそのパラメータを呼ぶときの
        // 間接呼び出しの候補になる。渡されたクロージャが分かって (`id`)、呼び出し先がパラメータを
        // 呼んでいる (本体の走査の近似) ときだけ、呼び出し先からクロージャの節へ辺を足す。
        // どちらかが分からなければ黙る (辺を足さない)。渡されたクロージャは、呼び出し先の本体の
        // 中のパラメータ呼び出し (型に基づく解決) の候補としても覚える。
        for (k, name) in &fn_args {
            let Some(Ty::Closure { id: Some(node), .. }) = closures.get(k) else { continue };
            self.fn_closures.entry((d.name.clone(), name.clone())).or_default().push(node.clone());
            let local = d.params[*k].0.clone();
            if self.callee_calls_param(d, &local) {
                let callee = d.name.clone();
                self.edges.entry(callee).or_default().insert(node.clone());
            }
        }
        for (k, a) in argt.iter().enumerate() {
            let Some(p) = d.params.get(k) else { break };
            let raw = from_v(&p.1, &gn);
            let pt = self.inst(&raw, &mut map);
            if let Err(e) = self.unify2(&pt, a) {
                // 型パラメータを含まない引数の型は従来の検査 (E2020) が見る。ここでは重複を避ける。
                // ただし構造体リテラルの引数で、宣言と同じ型名どうしの矛盾 (型引数の違い) は、
                // 従来の検査が名前しか比べないので黙る。ここでだけ報告する。
                // 定数実引数どうしの違い (異なる定数は別単相、4.3) も従来の検査は見ないので、ここで報告する。
                let pt = self.deep(&pt);
                let av = self.deep(a);
                let same_named = match (pt.strip(), av.strip()) {
                    (Ty::Named(x, _), Ty::Named(y, _)) => x == y,
                    _ => false,
                };
                let const_mismatch = matches!((&e.0, &e.1), (Ty::Const(_), Ty::Const(_)));
                if raw.has_param() || const_mismatch || (sl.get(k).copied().unwrap_or(false) && same_named) {
                    if let Some((want, got)) = self.prim_conflict(&e) {
                        self.diag(
                            "E2023",
                            format!("argument {} of `{}`: expected `{}`, found `{}`", k + 1, d.name, want, got),
                            line,
                        );
                    }
                }
            }
        }
        // Fn 境界の単一化: 境界の `Args` タプルと戻り値型を、クロージャの型と比べる。
        let mut cap_bad = false;
        for (k, name) in &fn_args {
            let Some(cl) = closures.get(k).cloned() else { continue };
            self.unify_fn_bound(d, *k, name, &cl, &mut map, line, &mut cap_bad);
        }
        self.check_bounds(d, &map, line, true);
        // 決まらなかった型パラメータは、従来の escape-all と同じ扱いにする (リテラルの E2027 を出さない)。
        for t in map.values() {
            if let Ty::Var(_) = self.deep(t) {
                self.escape(t);
            }
        }
        let ret = from_v(&d.ret, &gn);
        self.inst(&ret, &mut map)
    }

    /// `Fn` 境界を持つ型パラメータ `name` に渡されたクロージャ (または関数項目) の型 `cl` を、
    /// 境界の `Args` タプル・戻り値型・capture の能力と単一化する。関数呼び出し (`call_def`) と
    /// メソッド呼び出し (`call_method`) で共通に使う。`k` は引数の添字 (0 始まり。メッセージは 1 始まり)。
    /// `cap_bad` は、この呼び出しで E2030 を出し済みかどうか (同じ呼び出しで 2 回出さない)。
    #[allow(clippy::too_many_arguments)]
    fn unify_fn_bound(&mut self, d: &Def, k: usize, name: &str, cl: &Ty, map: &mut HashMap<String, Ty>, line: usize, cap_bad: &mut bool) {
        let gn = d.gnames();
        let Ty::Closure { args, ret, cap, .. } = self.deep(cl) else { return };
        let bounds = self.fn_bound(d, name);
        for b in &bounds {
            let V::L(bx) = b else { continue };
            let tr = st(&bx[0]);
            if !tr.starts_with("core.ops.Fn") {
                continue;
            }
            let targs = tel(b, 1);
            if let Ty::Tuple(want_args) = self.inst(&from_v(tel(&targs, 0), &gn), map) {
                if args.len() != want_args.len() {
                    self.diag(
                        "E2023",
                        format!(
                            "argument {} of `{}`: expected `{}`, found `{}`",
                            k + 1,
                            d.name,
                            Ty::Tuple(want_args.clone()).show(),
                            cl.show()
                        ),
                        line,
                    );
                } else {
                    for (w, g) in want_args.iter().zip(&args) {
                        if let Err(e) = self.unify2(w, g) {
                            if let Some((want, got)) = self.type_conflict(&e) {
                                self.diag(
                                    "E2023",
                                    format!("argument {} of `{}`: expected `{}`, found `{}`", k + 1, d.name, want, got),
                                    line,
                                );
                            }
                        }
                    }
                }
            }
            let want_ret = self.inst(&from_v(tel(&targs, 1), &gn), map);
            if let Err(e) = self.unify2(&want_ret, &ret) {
                if let Some((want, got)) = self.type_conflict(&e) {
                    self.diag("E2023", format!("argument {} of `{}`: expected `{}`, found `{}`", k + 1, d.name, want, got), line);
                }
            }
            // capture の能力: `Fn` は共有参照だけ、`FnMut` は値での取り込み以外まで、`FnOnce` は無条件。
            let ok = match tr.as_str() {
                "core.ops.Fn" => cap == Cap::Fn,
                "core.ops.FnMut" => cap != Cap::FnOnce,
                _ => true,
            };
            if !ok && !*cap_bad {
                *cap_bad = true;
                self.diag("E2030", format!("the trait bound `{}: {}` of {} is not satisfied by {}", name, tr, d.name, cl.show()), line);
            }
        }
    }

    /// ジェネリクス `name` の境界のうち、`core.ops.Fn*` を含む項目の列。
    fn fn_bound(&self, d: &Def, name: &str) -> Vec<V> {
        let mut out = vec![];
        for g in &d.generics {
            if g.0 == "type" && g.1 == name {
                for b in items(&g.2) {
                    if matches!(b, V::L(bx) if st(&bx[0]).starts_with("core.ops.Fn")) {
                        out.push(b.clone());
                    }
                }
            }
        }
        out
    }

    /// `Fn` 境界 (`F: core.ops.Fn<Args, R, E, P>`) のパラメータに渡されたクロージャの本体の効果で、
    /// 境界の効果変数 `E`・panic 変数 `P` を束縛する (4.3 の引数からの効果推論)。
    /// `off` は引数と宣言の引数の添字のずれ (メソッドの `self` で 1)。同じ `E` / `P` が複数の境界に
    /// 現れるときは各実引数の和 (`P` は `may` 優先)。クロージャの型・本体の効果が分からない引数、
    /// 境界の効果実引数が宣言の効果変数でない (`[io]` などの具体名) ものは束縛しない (黙る)。
    fn closure_effect_bind(&self, d: &Def, off: usize, argt: &[Ty]) -> EffBind {
        let mut out = EffBind::default();
        let gn = d.gnames();
        for (k, a) in argt.iter().enumerate() {
            let Some(p) = d.params.get(k + off) else { break };
            let Ty::Param(name) = from_v(&p.1, &gn) else { continue };
            let bounds = self.fn_bound(d, &name);
            if bounds.is_empty() {
                continue;
            }
            let Ty::Closure { id: Some(node), .. } = self.deep(a) else { continue };
            let Some((uses, may, unsure)) = self.closure_effs.get(&node) else { continue };
            // 確実でない効果 (推論された関数の上からの近似を含む) は、効果変数の束縛に使わない (黙る)。
            if *unsure {
                continue;
            }
            for b in &bounds {
                let bt = items(tel(b, 1));
                let var = |i: usize, kind: &str| -> Option<String> {
                    let v = bt.get(i)?;
                    if tag(v) != "named" || !items(tel(v, 2)).is_empty() {
                        return None;
                    }
                    let nm = st(tel(v, 1));
                    d.generics.iter().any(|g| g.0 == kind && g.1 == nm).then_some(nm)
                };
                if let Some(e) = var(2, "uses") {
                    out.uses.entry(e).or_default().extend(uses.iter().cloned());
                }
                if let Some(pv) = var(3, "panics") {
                    *out.panics.entry(pv).or_default() |= *may;
                }
            }
        }
        out
    }

    /// 推論された関数 (`infer.rs` の上からの近似) の名前か。
    fn inferred_def(&self, name: &str) -> bool {
        self.p.defs.iter().any(|d| d.inferred && d.name == name)
    }

    /// `Fn` 境界の効果実引数が具体的な名前 (`[]` / `[io.console]`) や `never` のとき、渡したクロージャの
    /// 確実な効果がそれを超えないかを検査する (4.3)。クロージャの効果は下からの近似 (確実に含まれる効果
    /// だけ) なので、超えていれば境界の違反、分からなければ黙る。境界の効果変数 (`E` / `P`) は
    /// `closure_effect_bind` が扱うので、ここでは具体名の部分だけを見る。
    fn check_closure_bounds(&mut self, d: &Def, off: usize, argt: &[Ty], line: usize) {
        let gn = d.gnames();
        let mut out: Vec<(&'static str, String)> = vec![];
        for (k, a) in argt.iter().enumerate() {
            let Some(p) = d.params.get(k + off) else { break };
            let Ty::Param(name) = from_v(&p.1, &gn) else { continue };
            let bounds = self.fn_bound(d, &name);
            if bounds.is_empty() {
                continue;
            }
            let Ty::Closure { id: Some(node), .. } = self.deep(a) else { continue };
            let Some((uses, may, unsure)) = self.closure_effs.get(&node) else { continue };
            if *unsure {
                continue;
            }
            for b in &bounds {
                let bt = items(tel(b, 1));
                if let Some(v) = bt.get(2).filter(|v| tag(v) == "effects") {
                    let bl: Vec<String> = items(tel(v, 1)).iter().map(st).collect();
                    // 効果変数 (大文字) を含む境界は、ここでは検査しない。
                    if bl.iter().all(|e| e.chars().next().map_or(false, |c| c.is_lowercase())) {
                        for e in uses.iter().filter(|e| !crate::check::covers(&bl, e)) {
                            out.push((
                                "E4001",
                                format!(
                                    "closure passed to `{}` uses effect `{}` outside the `Fn` bound's effects [{}]",
                                    d.name,
                                    e,
                                    bl.join(", ")
                                ),
                            ));
                        }
                    }
                }
                if let Some(v) = bt.get(3).filter(|v| tag(v) == "panic") {
                    if st(tel(v, 1)) == "never" && *may {
                        out.push((
                            "E4002",
                            format!("closure passed to `{}` may panic but the `Fn` bound declares `panics never`", d.name),
                        ));
                    }
                }
            }
        }
        for (c, m) in out {
            self.diag(c, m, line);
        }
    }

    /// 呼び出し先 `d` の効果を実体化する。明示型引数があればそれで (check.rs と同じ `inst_def_effects`)、
    /// なければクロージャから推論した束縛で置き換える。
    /// 戻り値は (実体化した uses, 実体化した panics, 束縛から来た uses, 束縛から来た panic が may か)。
    fn callee_effects(&self, d: &Def, off: usize, argt: &[Ty], targs: &[V]) -> (Vec<String>, Option<String>, Vec<String>, bool) {
        if !targs.is_empty() {
            let (u, p) = crate::check::inst_def_effects(d, targs).unwrap_or_else(|| (d.uses.clone(), d.panics.clone()));
            return (u, p, vec![], false);
        }
        let bind = self.closure_effect_bind(d, off, argt);
        // 宣言にある具体名 (`uses [io.console, E]` の io.console) で覆われる効果は、従来の検査が
        // 宣言どおりに見るので、束縛から来た部分 (derived) からは除く (同じ違反の二重報告を避ける)。
        let concrete: Vec<String> =
            d.uses.iter().filter(|e| e.chars().next().map_or(false, |c| c.is_lowercase())).cloned().collect();
        let mut full = vec![];
        let mut derived = vec![];
        for e in &d.uses {
            match bind.uses.get(e) {
                Some(set) => {
                    full.extend(set.iter().cloned());
                    derived.extend(set.iter().filter(|x| !crate::check::covers(&concrete, x)).cloned());
                }
                None => full.push(e.clone()),
            }
        }
        let mut derived_may = false;
        let panics = d.panics.as_deref().map(|p| match bind.panics.get(p) {
            Some(&m) => {
                derived_may = m;
                if m { "may".to_string() } else { "never".to_string() }
            }
            None => p.to_string(),
        });
        (full, panics, derived, derived_may)
    }

    /// 型で解決できた呼び出しの効果を扱う。`cands` は候補ごとの (名前, 実体化した uses, 実体化した panics,
    /// 検査する uses, 検査する panics)。クロージャの本体の中なら、すべての候補に共通する効果を
    /// そのクロージャの効果に足す (下からの近似)。関数本体の直下なら、検査の義務を残す (test は検査しない)。
    fn record_effects(&mut self, line: usize, cands: Vec<(String, Vec<String>, Option<String>, Vec<String>, Option<String>)>) {
        if cands.is_empty() {
            return;
        }
        if !self.eff_acc.is_empty() {
            let lower = |u: &Vec<String>| -> BTreeSet<String> {
                u.iter().filter(|e| e.chars().next().map_or(false, |c| c.is_lowercase())).cloned().collect()
            };
            // 推論された関数の効果は上からの近似 (infer.rs) なので、それが効果を足すときは確実でないとする。
            let unsure = cands.iter().any(|c| {
                self.inferred_def(&c.0) && (!lower(&c.1).is_empty() || c.2.as_deref() == Some("may"))
            });
            let mut common = lower(&cands[0].1);
            for c in &cands[1..] {
                let o = lower(&c.1);
                common.retain(|e| o.contains(e));
            }
            let all_may = cands.iter().all(|c| c.2.as_deref() == Some("may"));
            if let Some(acc) = self.eff_acc.last_mut() {
                acc.0.extend(common);
                if all_may {
                    acc.1 = true;
                }
                acc.2 |= unsure;
            }
            return;
        }
        if self.def().kw == "test" {
            return;
        }
        // 推論された関数 (infer.rs) の効果は上からの近似 (名前の全候補の和を含みうる) なので、
        // 宣言された効果と同じ確かさでは検査しない (黙る)。
        if cands.iter().any(|c| self.inferred_def(&c.0)) {
            return;
        }
        let checks: Vec<(String, Vec<String>, Option<String>)> =
            cands.into_iter().map(|(n, _, _, u, p)| (n, u, p)).collect();
        if checks.iter().all(|c| c.1.is_empty() && c.2.as_deref() != Some("may")) {
            return;
        }
        self.eff_checks.push(EffCheck { di: self.cur, line, cands: checks });
    }

    /// クロージャの型での直接呼び出し (`f(x)`)。引数の個数 (E2022) と型 (E2023) を検査し、
    /// 戻り値の型を返す。`cl` がクロージャに決まらないときは None。
    fn call_closure(&mut self, cl: &Ty, argt: &[Ty], line: usize) -> Option<Ty> {
        let cl = self.deep(cl);
        let Ty::Closure { args, ret, id, .. } = cl else { return None };
        // 直接呼び出し (`let g = fn..; g(x)`) は、呼び出し元からクロージャの節への辺になる (5.3)。
        // 節の本体の呼び出しは書かれた関数に帰属するが、節そのもののフレームを数えるには辺が要る。
        if let Some(node) = id {
            self.add_edge(node);
        }
        if args.len() != argt.len() {
            self.diag(
                "E2022",
                format!("the closure takes {} argument(s) but {} given", args.len(), argt.len()),
                line,
            );
            argt.iter().for_each(|a| self.escape(a));
            return Some(Ty::Unknown);
        }
        for (i, (w, g)) in args.iter().zip(argt).enumerate() {
            if let Err(e) = self.unify2(w, g) {
                if let Some((want, got)) = self.type_conflict(&e) {
                    self.diag(
                        "E2023",
                        format!("argument {} of the closure: expected `{}`, found `{}`", i + 1, want, got),
                        line,
                    );
                }
            }
        }
        Some((*ret).clone())
    }

    /// 矛盾した 2 つの型が「プリミティブ同士の違い」か「Named 同士の名前違い」なら、その文字列を返す。
    /// それ以外は誤検出のおそれがあるので None (何も言わない)。
    fn type_conflict(&self, e: &(Ty, Ty)) -> Option<(String, String)> {
        match (&e.0, &e.1) {
            (Ty::Prim(a), Ty::Prim(b)) if a != b => Some((a.clone(), b.clone())),
            (Ty::Named(a, _), Ty::Named(b, _)) if a != b => Some((a.clone(), b.clone())),
            (Ty::Const(a), Ty::Const(b)) if a != b => Some((a.clone(), b.clone())),
            _ => None,
        }
    }

    /// `let` の注釈・戻り値との矛盾を報告するときの 2 つの型の文字列。
    ///
    /// 従来の検査 (check.rs の `mismatch`) も同じ誤りを同じ文面で報告することがある。そのときは
    /// check.rs の重複除去 (同じ file + code + fqn + 行 + 文面) で 1 件にまとめられるので、
    /// ここでは文面を崩さない (プリミティブはそのまま、Named は名前だけ)。
    /// ただし同じ型で型引数だけが違うとき (例: `W<u64>` と `W<u32>`) は従来の検査が名前しか
    /// 見ないので報告されない。ここでだけ全体の形を示す。
    fn ann_conflict(&self, want: &Ty, got: &Ty, err: &(Ty, Ty)) -> Option<(String, String)> {
        let w = self.deep(want);
        let g = self.deep(got);
        if let (Ty::Named(n, xs), Ty::Named(m, ys)) = (w.strip(), g.strip()) {
            if n == m && !xs.is_empty() && !ys.is_empty() && !xs.iter().any(|x| self.unresolved(x)) && !ys.iter().any(|x| self.unresolved(x)) {
                let a = w.strip().clone();
                let b = g.strip().clone();
                if a.show() != b.show() {
                    return Some((a.show(), b.show()));
                }
                return None;
            }
        }
        match (&err.0, &err.1) {
            (Ty::Prim(a), Ty::Prim(b)) => Some((a.clone(), b.clone())),
            (Ty::Named(a, _), Ty::Named(b, _)) if a != b => Some((a.clone(), b.clone())),
            _ => None,
        }
    }

    /// 型の中に、まだ決まっていない型変数があれば true。推論が途切れた部分を含む型を
    /// 全体の形として示しても役に立たないので、そのときは矛盾した箇所どうしを見せる。
    fn unresolved(&self, t: &Ty) -> bool {
        match self.deep(t) {
            Ty::Var(_) => true,
            Ty::Named(_, a) | Ty::Tuple(a) => a.iter().any(|x| self.unresolved(x)),
            Ty::Ref(_, x) | Ty::Array(x) | Ty::Slice(x) => self.unresolved(&x),
            Ty::Closure { args, ret, .. } => args.iter().any(|x| self.unresolved(x)) || self.unresolved(&ret),
            _ => false,
        }
    }

    /// 4.1: `?` はエラー型が完全一致するときだけ使える (変換は `map_err` 必須)。
    /// `?` は式を置いた関数 (クロージャ) の戻り値型へ脱糖するので、そこで受けられるのは
    /// オペランドと同じ種類の `Option` / `Result` だけ。エラー型は単一化して比べる。
    /// どちらかが分からないときは黙る (Cranelift バックエンドも同じ条件で拒否する)。
    fn check_try_err(&mut self, operand: &Ty, line: usize) {
        let got = self.deep(operand);
        let Ty::Named(gn, ga) = got.strip() else { return };
        if gn != "Result" && gn != "Option" {
            return;
        }
        let Some(want) = self.rets.last().cloned() else { return };
        let want = self.deep(&want);
        let want = want.strip().clone();
        match want {
            Ty::Unknown | Ty::Param(_) | Ty::Var(_) | Ty::Never => {}
            Ty::Named(wn, wa) if wn == *gn => {
                // 種類は一致。`Result` のエラー型が完全一致するかだけを見る。
                if gn == "Result" && wa.len() >= 2 && ga.len() >= 2 {
                    let e_want = wa[1].clone();
                    let e_got = ga[1].clone();
                    if let Err(err) = self.unify2(&e_want, &e_got) {
                        if let Some((w, g)) = self.type_conflict(&err) {
                            self.diag(
                                "E2020",
                                format!(
                                    "type mismatch in `?`: expected error type `{}`, found `{}`; the error type must match exactly, use `map_err`",
                                    w, g
                                ),
                                line,
                            );
                        }
                    }
                }
            }
            other => {
                self.diag(
                    "E2020",
                    format!("type mismatch in `?`: the return type here is `{}`, not `{}`", other.show(), gn),
                    line,
                );
            }
        }
    }

    /// 式の根にある局所変数の名前 (`v` / `s.items` → `s`)。
    fn base_local(&self, e: &V) -> Option<String> {
        match nname(e) {
            "Path" => {
                let segs = strs(&nf(e)[0]);
                if self.lookup(&segs[0]).is_some() { Some(segs[0].clone()) } else { None }
            }
            "Field" => self.base_local(&nf(e)[0]),
            _ => None,
        }
    }
}

/// 関数自身の型パラメータは、本体の中では分からない型として扱う。
fn subst_params_unknown(t: &Ty) -> Ty {
    subst(t, &HashMap::new())
}

/// `e` (`loop` の本体) の中に、その `loop` を抜ける `break` があるか。入れ子の `loop` と `for` / `while` の
/// 本体、クロージャの中の `break` は別の loop (または不正な位置) のものなので数えない。
/// `while` の条件と `for` の反復子は、外側の loop の中にあるので数える。
fn has_break_out(e: &V) -> bool {
    match nname(e) {
        "Break" => return true,
        "Loop" | "Closure" => return false,
        "While" => return has_break_out(&nf(e)[0]),
        "For" => return has_break_out(&nf(e)[1]),
        _ => {}
    }
    crate::check::children(e).into_iter().any(has_break_out)
}

/// impl の対象型 `tgt` について、メソッドの所有者になり得る名前 (型そのものと、
/// 基本型ならその所有者 FQN。3.2 の impl の置き場所)。
fn impl_owner_keys(tgt: &str) -> Vec<String> {
    let mut v = vec![tgt.to_string()];
    let owner = match tgt {
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128" | "usize"
        | "f32" | "f64" => format!("core.num.{}", tgt),
        "bool" => "core.bool.bool".into(),
        "char" => "core.char.char".into(),
        "str" => "core.str".into(),
        _ => String::new(),
    };
    if !owner.is_empty() {
        v.push(owner);
    }
    v
}

/// capture 節の AST (`move x` / `&x` / `&mut x` の並び) から、満たせ得る最大の能力を求める。
/// 共有参照だけ (または空) なら `Fn`、`&mut` を含めば `FnMut`、値での取り込みを含めば `FnOnce`。
fn closure_cap(caps: &V) -> Cap {
    let mut cap = Cap::Fn;
    for c in items(caps) {
        let Some(V::S(k)) = items(c).first() else { continue };
        match k.as_str() {
            "move" => cap = Cap::FnOnce,
            "&mut" if cap == Cap::Fn => cap = Cap::FnMut,
            _ => {}
        }
    }
    cap
}

/// 整数リテラルの綴り `text` が型 `ty` に収まらなければ、その説明を返す。
fn int_literal_problem(text: &str, ty: &str) -> Option<String> {
    let neg = text.starts_with('-');
    let t = text.trim_start_matches('-').replace('_', "");
    let (digits, radix) = if let Some(r) = t.strip_prefix("0x") {
        (r.to_string(), 16)
    } else if let Some(r) = t.strip_prefix("0b") {
        (r.to_string(), 2)
    } else if let Some(r) = t.strip_prefix("0o") {
        (r.to_string(), 8)
    } else {
        (t.clone(), 10)
    };
    // 接尾辞は字句解析の段階で分けてある (綴りには含まれない) が、念のため取り除く。
    let digits: String = digits.chars().take_while(|c| c.is_digit(radix)).collect();
    let mag = u128::from_str_radix(&digits, radix).ok();
    let bits: u32 = match ty {
        "i8" | "u8" => 8,
        "i16" | "u16" => 16,
        "i32" | "u32" => 32,
        "i128" | "u128" => 128,
        _ => 64,
    };
    let signed = ty.starts_with('i');
    let Some(mag) = mag else { return Some(format!("integer literal `{}` is too large for `{}`", text, ty)) };
    if !signed {
        if neg && mag != 0 {
            return Some(format!("a negative literal `{}` cannot have the unsigned type `{}`", text, ty));
        }
        let max = if bits == 128 { u128::MAX } else { (1u128 << bits) - 1 };
        if mag > max {
            return Some(format!("integer literal `{}` is out of range for `{}` (max {})", text, ty, max));
        }
    } else {
        let limit: u128 = 1u128 << (bits - 1);
        if (!neg && mag >= limit) || (neg && mag > limit) {
            return Some(format!("integer literal `{}` is out of range for `{}`", text, ty));
        }
    }
    None
}

/// 整数型どうしの食い違いに付ける、変換の仕方の案内 (暗黙の変換も `as` もない)。
pub(crate) fn conv_hint(want: &str, got: &str) -> String {
    let int = |t: &str| INTS.contains(&t);
    if want != got && int(want) && int(got) {
        format!(
            "; there is no implicit conversion and no `as`: write `core.num.{w}.from_{g}(x)` if {g} always fits in {w}, otherwise `core.num.{w}.try_from_{g}(x)` (returns Result)",
            w = want,
            g = got
        )
    } else {
        String::new()
    }
}
