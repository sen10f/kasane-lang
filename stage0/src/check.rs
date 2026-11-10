//! 意味検査 (stage0)。Python 参照実装 ksn/sema.py (Checker) と ksn/typeck.py (TypeCk) の移植。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::program::{Def, Program, Resolved, Target};
use crate::parser::V;

#[path = "borrowck.rs"]
mod borrowck;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Diag {
    pub file: String,
    pub code: String,
    pub fqn: String,
    pub line: usize,
    pub msg: String,
}

// ---------------------------------------------------------------- V の読み取り補助
pub(crate) fn nname(v: &V) -> &str {
    match v {
        V::Node(n, _, _) => n,
        _ => "",
    }
}
pub(crate) fn nf(v: &V) -> &[V] {
    match v {
        V::Node(_, f, _) => f,
        _ => &[],
    }
}
pub(crate) fn nline(v: &V) -> usize {
    match v {
        V::Node(_, _, l) => *l,
        _ => 0,
    }
}
pub(crate) fn items(v: &V) -> &[V] {
    match v {
        V::L(xs) => xs,
        _ => &[],
    }
}
pub(crate) fn st(v: &V) -> String {
    match v {
        V::S(s) | V::N(s) => s.clone(),
        V::B(b) => (if *b { "true" } else { "false" }).to_string(),
        V::Nil => String::new(),
        o => o.dump(),
    }
}
pub(crate) fn is_nil(v: &V) -> bool {
    matches!(v, V::Nil)
}
pub(crate) fn strs(v: &V) -> Vec<String> {
    items(v).iter().map(st).collect()
}

/// 型 V の先頭タグ ("prim" "ref" など)。
pub(crate) fn tag(t: &V) -> String {
    match t {
        V::L(xs) if !xs.is_empty() => st(&xs[0]),
        _ => String::new(),
    }
}
pub(crate) fn tel(t: &V, i: usize) -> &V {
    static NIL: V = V::Nil;
    match t {
        V::L(xs) if xs.len() > i => &xs[i],
        _ => &NIL,
    }
}

/// 子ノード (フィールドを平坦化して Node のみ)。Python の sema.children に相当。
pub(crate) fn children(v: &V) -> Vec<&V> {
    fn flat<'a>(v: &'a V, out: &mut Vec<&'a V>) {
        match v {
            V::Node(..) => out.push(v),
            V::L(xs) => {
                for x in xs {
                    flat(x, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for f in nf(v) {
        flat(f, &mut out);
    }
    out
}

pub(crate) fn walk<'a>(v: &'a V, out: &mut Vec<&'a V>) {
    out.push(v);
    for c in children(v) {
        walk(c, out);
    }
}

pub(crate) fn is_pat(v: &V) -> bool {
    matches!(nname(v), "PWild" | "PBind" | "PLit" | "PPath" | "PTuple" | "PVariant" | "PStruct" | "PRef")
}

pub(crate) fn pat_binds(p: &V, out: &mut Vec<String>) {
    match nname(p) {
        "PBind" => out.push(st(&nf(p)[0])),
        "PTuple" | "PVariant" => {
            let it = if nname(p) == "PTuple" { &nf(p)[0] } else { &nf(p)[1] };
            for x in items(it) {
                pat_binds(x, out);
            }
        }
        "PStruct" => {
            for f in items(&nf(p)[1]) {
                pat_binds(&items(f)[1], out);
            }
        }
        "PRef" => pat_binds(&nf(p)[0], out),
        _ => {}
    }
}

pub(crate) fn covers(caller: &[String], eff: &str) -> bool {
    if eff.chars().next().map_or(false, |c| c.is_uppercase()) {
        return true;
    }
    caller.iter().any(|c| eff == c || eff.starts_with(&format!("{}.", c)))
}

/// 式の中の明示型引数 (`f::<..>` の各実引数) の種別 (4.3)。
/// "type" は型そのもの、"effects" は `uses [..]`、"panic" は `never` / `may`、
/// "const" は定数リテラル (整数 / `bool` / `char`)、"life" はライフタイム。
pub(crate) fn targ_kind(t: &V) -> &'static str {
    if let V::L(fs) = t {
        if let Some(f0) = fs.first() {
            match st(f0).as_str() {
                "life" => return "life",
                "const" => return "const",
                "panic" => return "panic",
                "effects" => return "effects",
                _ => {}
            }
        }
    }
    "type"
}

/// 呼び出しの明示型引数を、callee のジェネリクス宣言と順に対応させる (4.3)。
/// 戻り値は (宣言の名前, 種別, 実引数)。実引数が供給されない残りの宣言
/// (引数などから推論される部分) は捨てて構わない。ライフタイムの実引数は、
/// 宣言の残りにライフタイムがあればそれに対応させ、なければ並びが崩れたとみなす。
/// `None` になるのは、実引数が宣言より多いときと、ライフタイム実引数と他の実引数の
/// 並びが崩れた (ライフタイム実引数に対応する宣言がない) とき。`type` ↔ `effects` /
/// `panic` / `const` の種別不整は `None` にならず、宣言側の種別のまま対応させて
/// 置換を試みる (結果として黙る。種別不整の呼び出しの検査はしない近似)。
pub(crate) fn align_targs<'a>(
    gens: &'a [(String, String, V)],
    targs: &'a [V],
) -> Option<Vec<(&'a str, &'a str, &'a V)>> {
    let mut out = Vec::new();
    let mut gi = 0usize;
    for t in targs {
        let k = targ_kind(t);
        while gi < gens.len() && gens[gi].0 == "life" && k != "life" {
            gi += 1;
        }
        let g = gens.get(gi)?;
        if (k == "life") != (g.0 == "life") {
            return None;
        }
        out.push((g.1.as_str(), g.0.as_str(), t));
        gi += 1;
    }
    Some(out)
}

/// callee の `uses` / `panics` を、呼び出しの明示型引数で実体化する (4.3 の単相化)。
/// 効果変数 `E` は対応する `uses [..]` 実引数の集合に、panic 変数 `P` は `never` / `may` に置き換わる。
/// 実引数がない・対応が取れない (推論できない) ときは None。呼び出し側はそのとき
/// 宣言のままの検査に落とす (未束縛の効果変数は `covers` が黙って通す)。
pub(crate) fn inst_def_effects(callee: &Def, targs: &[V]) -> Option<(Vec<String>, Option<String>)> {
    if targs.is_empty() {
        return None;
    }
    let pairs = align_targs(&callee.generics, targs)?;
    let uses: Vec<String> = callee
        .uses
        .iter()
        .flat_map(|e| match pairs.iter().find(|(n, k, _)| *k == "uses" && *n == e.as_str()) {
            // `uses [alloc, E]` は具体名と実引数の集合の和 (4.3)。
            Some((_, _, t)) => items(tel(t, 1)).iter().map(st).collect(),
            None => vec![e.clone()],
        })
        .collect();
    let panics =
        callee.panics.as_deref().map(|p| match pairs.iter().find(|(n, k, _)| *k == "panics" && *n == p) {
            Some((_, _, t)) => st(tel(t, 1)),
            None => p.to_string(),
        });
    Some((uses, panics))
}

const PRELUDE_TYPES: &[&str] = &["Option", "Result"];
/// プレリュード名のFQN形 (型・パターンに書くとE1110。メソッドFQNは除く)。
const PRELUDE_FQN: &[&str] = &[
    "core.option.Option",
    "core.option.Some",
    "core.option.None",
    "core.result.Result",
    "core.result.Ok",
    "core.result.Err",
];
pub(crate) const DERIVE_TRAITS: &[(&str, &str)] = &[
    ("Debug", "core.fmt.Debug"),
    ("Clone", "core.ops.Clone"),
    ("Copy", "core.ops.Copy"),
    ("PartialEq", "core.cmp.PartialEq"),
    ("Eq", "core.cmp.Eq"),
    ("PartialOrd", "core.cmp.PartialOrd"),
    ("Ord", "core.cmp.Ord"),
    ("Hash", "core.hash.Hash"),
];

// ---------------------------------------------------------------- Checker
pub struct Checker<'a> {
    pub p: &'a Program,
    pub user: Vec<usize>,
    pub diags: Vec<Diag>,
    cur_uses: Option<Vec<String>>,
    cur_panics: String,
    /// いま `const fn` の本体を検査中か (呼べるのは const fn と組み込みのみ、3.2)。
    in_const_fn: bool,
    pub unsafe_modules: Vec<String>,
    unsafe_ok: bool,
    // typeck の状態
    d: usize,
    names: HashSet<String>,
    /// move 済みの場所 (根の変数 + フィールド道をドットでつないだ鍵。`s`・`s.f`・`s.a.b`) → move した行。
    /// 鍵どうしは互いに重ならない (一方が他方の接頭辞なら記録しない)。E3002 (4.1・4.4)。
    moved: BTreeMap<String, usize>,
    final_env: HashMap<String, V>,
    graph: BTreeMap<String, BTreeSet<String>>,
    /// 現在の関数のファイルで `import` したモジュールの末尾名 (シャドーイング検査用)。
    import_tails: HashSet<String>,
    /// 現在の関数のファイルで `import` したモジュールの完全名 (セグメント列)。本体でのFQN参照の検出用。
    imported_modules: Vec<Vec<String>>,
}

type Env = HashMap<String, V>;

/// 場所 (根の変数 + フィールド道) を一意な鍵にする (`s`・`s.f`・`s.a.b`)。
fn place_key(root: &str, path: &[String]) -> String {
    let mut s = String::from(root);
    for p in path {
        s.push('.');
        s.push_str(p);
    }
    s
}

/// 2 つの場所の鍵が重なる (一方が他方の接頭辞) か。borrowck.rs の overlaps と同じ判定 (4.1)。
fn place_overlaps(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}.")) || b.starts_with(&format!("{a}."))
}

/// 型の中の型パラメータ (引数なしの named で束縛にある名前) を使用側の型引数で置き換える。
fn subst_ty_params(t: &V, bind: &HashMap<String, V>) -> V {
    if tag(t) == "named" {
        let nm = st(tel(t, 1));
        let args = items(tel(t, 2));
        if args.is_empty() {
            if let Some(a) = bind.get(&nm) {
                return a.clone();
            }
        }
        let new_args: Vec<V> = args.iter().map(|a| subst_ty_params(a, bind)).collect();
        return V::L(vec![V::S("named".into()), V::S(nm), V::L(new_args)]);
    }
    match tag(t).as_str() {
        "ref" | "ptr" => V::L(vec![tel(t, 0).clone(), tel(t, 1).clone(), subst_ty_params(tel(t, 2), bind)]),
        "slice" => V::L(vec![V::S("slice".into()), subst_ty_params(tel(t, 1), bind)]),
        "array" => V::L(vec![V::S("array".into()), subst_ty_params(tel(t, 1), bind), tel(t, 2).clone()]),
        "tuple" => V::L(vec![
            V::S("tuple".into()),
            V::L(items(tel(t, 1)).iter().map(|x| subst_ty_params(x, bind)).collect()),
        ]),
        _ => t.clone(),
    }
}

impl<'a> Checker<'a> {
    pub fn new(p: &'a Program, user: Vec<usize>) -> Self {
        Checker {
            p,
            user,
            diags: vec![],
            cur_uses: None,
            cur_panics: "may".into(),
            in_const_fn: false,
            unsafe_modules: vec![],
            unsafe_ok: false,
            d: 0,
            names: HashSet::new(),
            moved: BTreeMap::new(),
            final_env: HashMap::new(),
            graph: BTreeMap::new(),
            import_tails: HashSet::new(),
            imported_modules: Vec::new(),
        }
    }

    fn def(&self, i: usize) -> &'a Def {
        &self.p.defs[i]
    }

    fn diag(&mut self, code: &str, msg: String, di: usize, line: usize) {
        let d = self.def(di);
        let fqn = if d.kw == "import" { "-".to_string() } else { d.name.clone() };
        self.diags.push(Diag {
            file: d.file.clone(),
            code: code.into(),
            fqn,
            line: if line != 0 { line } else { d.line },
            msg,
        });
    }

    pub fn run(&mut self) {
        let user = self.user.clone();
        let mut seen: HashSet<String> = HashSet::new();
        for &i in &user {
            let d = self.def(i);
            if matches!(d.kw.as_str(), "fn" | "type" | "enum" | "trait" | "const" | "static" | "extern" | "test") {
                if !seen.insert(d.name.clone()) {
                    self.diag("E1103", format!("duplicate definition of {}", d.name), i, 0);
                }
            }
        }
        self.check_declarations();
        self.check_impls();
        self.check_field_method();
        self.check_statics();
        self.check_tests();
        self.check_linear();
        self.check_derive();
        for &i in &user {
            let d = self.def(i);
            match d.kw.as_str() {
                "fn" if d.body.is_some() => self.check_fn_sema(i, false),
                "test" => self.check_fn_sema(i, true),
                "const" | "static" => {
                    self.check_header_types(i, &[d.ret.clone()]);
                    if let Some(init) = &d.init {
                        self.cur_uses = self.cur_uses.take();
                        self.visit(init, &mut HashSet::new(), i, true);
                    }
                }
                "type" => {
                    let tys: Vec<V> = d.fields.iter().map(|f| f.1.clone()).collect();
                    self.check_header_types(i, &tys);
                }
                "enum" => {
                    let tys: Vec<V> = d.variants.iter().flat_map(|v| v.1.clone()).collect();
                    self.check_header_types(i, &tys);
                }
                "impl" => {
                    let tr = d.impl_trait.clone();
                    if !self.p.types.contains_key(&tr) && tr != "core.marker.Send" && tr != "core.marker.Sync" {
                        self.diag("E1104", format!("unknown trait {}", tr), i, 0);
                    }
                }
                _ => {}
            }
        }
        self.check_cycles();
        self.typeck();
    }

    /// 3.5: `[unsafe] modules` の完全一致または接頭辞一致。core / alloc / std は暗黙に許可。
    fn module_may_use_unsafe(&self, module: &str) -> bool {
        let top = module.split('.').next().unwrap_or("");
        if matches!(top, "core" | "alloc" | "std") {
            return true;
        }
        self.unsafe_modules.iter().any(|m| module == m || module.starts_with(&format!("{}.", m)))
    }

    // ---- 宣言レベルの規則 (3.5 / 5.3 / 8.2)
    fn check_declarations(&mut self) {
        let user = self.user.clone();
        // 5.3: 再帰を含む型 (自己参照・相互再帰の両方) の集合。
        let recursive = self.recursive_types();
        for &i in &user {
            let d = self.def(i);
            // 効果の推論 (非 pub の関数のみ): pub / impls の関数は宣言が必要。
            if d.kw == "fn" && d.inferred && (d.header.trim_end().ends_with(" pub") || d.header.contains(" impls ")) {
                self.diag("E4020", format!("{}: pub / impls functions must declare `uses` and `panics`", d.name), i, 0);
            }
            // 8.2: ユーザーは ksn_rt_ を link に使えない。
            if d.kw == "extern" && d.header.contains("link \"ksn_rt_") {
                let own = d.name.starts_with("core.") || d.name.starts_with("alloc.") || d.name.starts_with("std.");
                if !own {
                    self.diag("E1030", "`ksn_rt_` link names are reserved for the runtime".into(), i, 0);
                }
            }
            // 5.3: 再帰を含む型 (自己参照・相互再帰) は Drop の明示実装が必要。
            if matches!(d.kw.as_str(), "type" | "enum") && recursive.contains(&d.name) {
                let has_drop = user.iter().any(|&j| {
                    let x = self.def(j);
                    x.kw == "impl" && x.impl_trait == "core.ops.Drop" && ty_mentions(&x.impl_for, &d.name)
                });
                if !has_drop {
                    let msg = format!("recursive type {} requires an explicit core.ops.Drop impl", d.name);
                    self.diag("E4013", msg, i, 0);
                }
            }
            // 3.2: トレイトの必須メソッドのレシーバは `self: Self` / `&Self` / `&mut Self` のいずれか。
            if d.kw == "trait" {
                for (mname, params, _ret) in &d.trait_sigs {
                    match params.first() {
                        Some((pname, pty)) if pname == "self" => {
                            if !is_self_ty(pty) {
                                self.diag(
                                    "E2039",
                                    format!("trait method {} must take `self: Self`, `&Self`, or `&mut Self` (3.2)", mname),
                                    i,
                                    0,
                                );
                            }
                        }
                        _ => self.diag(
                            "E2039",
                            format!("trait method {} must take `self` as its first parameter (3.2)", mname),
                            i,
                            0,
                        ),
                    }
                }
            }
        }
    }

    /// 3.2: `impl Tr for X` と `impls Tr` のメソッドの整合 (過不足)、`impl` 行の重複を検査する
    /// (`spec/tools/xref.py` の impl / dup の移植)。未知トレイトは E1104 が報告する。
    fn check_impls(&mut self) {
        // トレイトの必須メソッド (標準ライブラリを含む全定義から)。
        let mut req: HashMap<String, BTreeSet<String>> = HashMap::new();
        for d in &self.p.defs {
            if d.kw == "trait" {
                req.insert(d.name.clone(), d.methods.iter().cloned().collect());
            }
        }
        // (トレイト, owner) -> `impls` を宣言したメソッド名の集合。
        let mut declared: HashMap<(String, String), BTreeSet<String>> = HashMap::new();
        let mut declared_at: HashMap<(String, String), usize> = HashMap::new();
        for &i in &self.user {
            let d = self.def(i);
            if d.kw == "fn" && d.has_impls {
                let owner = d.name.rsplit_once('.').map(|(o, _)| o.to_string()).unwrap_or_default();
                let key = (d.impls_trait.clone(), owner);
                let meth = d.name.rsplit('.').next().unwrap_or("").to_string();
                declared.entry(key.clone()).or_default().insert(meth);
                declared_at.entry(key).or_insert(i);
            }
        }
        let impl_lines: Vec<usize> = self.user.iter().copied().filter(|&i| self.def(i).kw == "impl").collect();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut covered: HashSet<(String, String)> = HashSet::new();
        for &i in &impl_lines {
            let d = self.def(i);
            let tb = d.impl_trait.clone();
            // 5.5: 手動の `Send` / `Sync` impl (内部可変性の表明) は unsafe 許可モジュールのみ。
            if matches!(tb.as_str(), "core.marker.Send" | "core.marker.Sync") {
                if !self.module_may_use_unsafe(&d.module) {
                    self.diag(
                        "E5002",
                        format!("manual impl of {} is only allowed in an unsafe-permitted module (5.5)", tb),
                        i,
                        0,
                    );
                }
                continue;
            }
            // 4.3: `Fn` / `FnMut` / `FnOnce` は自動実装のみ。手動 impl はできない。
            if matches!(tb.as_str(), "core.ops.Fn" | "core.ops.FnMut" | "core.ops.FnOnce") {
                self.diag(
                    "E2034",
                    format!("manual impl of {} is not allowed; Fn* is implemented automatically (4.3)", tb),
                    i,
                    0,
                );
                continue;
            }
            if !req.contains_key(&tb) {
                continue;
            }
            let tgt = impl_target(&d.impl_for);
            // 4.3 (3.2): 孤児規則 — `impl Tr for T` は、Tr と T のどちらかがこのパッケージ内で
            // 定義されたもののときだけ置ける。impl の置き場所自体が不正なときは、ここで報告して
            // メソッドの過不足 (E1112) は見ない (メソッドを置ける場所がそもそも無いので E1112 では直せない)。
            if self.orphan_violation(d, &tb, &tgt) {
                self.diag(
                    "E2043",
                    format!(
                        "impl {} for {} violates the orphan rule: neither the trait nor the type is defined in this package (3.2)",
                        tb, tgt
                    ),
                    i,
                    0,
                );
                continue;
            }
            let owner = prim_owner(&tgt);
            let alt_owner = format!("{}.{}", tb, tgt.rsplit('.').next().unwrap_or(""));
            if !seen.insert((tb.clone(), tgt.clone())) {
                self.diag("E1114", format!("duplicate impl of {} for {}", tb, tgt), i, 0);
            }
            let want = req.get(&tb).cloned().unwrap_or_default();
            let mut have = declared.get(&(tb.clone(), owner.clone())).cloned().unwrap_or_default();
            if let Some(m) = declared.get(&(tb.clone(), alt_owner.clone())) {
                have.extend(m.iter().cloned());
            }
            covered.insert((tb.clone(), owner));
            covered.insert((tb.clone(), alt_owner));
            if want != have {
                self.diag(
                    "E1112",
                    format!(
                        "impl {} for {}: methods [{}] != required [{}]",
                        tb,
                        tgt,
                        have.iter().cloned().collect::<Vec<_>>().join(", "),
                        want.iter().cloned().collect::<Vec<_>>().join(", ")
                    ),
                    i,
                    0,
                );
            }
        }
        // 逆方向: `impls` を宣言したメソッドがあるのに対応する `impl` 行が無い。
        for (key, ms) in &declared {
            if !covered.contains(key) {
                let at = declared_at.get(key).copied().unwrap_or(0);
                self.diag(
                    "E1113",
                    format!(
                        "methods [{}] impl {} on {} but no impl line exists",
                        ms.iter().cloned().collect::<Vec<_>>().join(", "),
                        key.0,
                        key.1
                    ),
                    at,
                    0,
                );
            }
        }
    }

    /// 4.3 (3.2): 孤児規則。`impl Tr for T` は、Tr と T のどちらか一方がこのパッケージ内で
    /// 定義されたもののときだけ許される。対象が型パラメータ (blanket impl) のとき、名前が
    /// 未解決のとき、名義型・基本型以外の対象 (参照・スライス・タプル・fn 型) のときは黙る (誤検出厳禁)。
    fn orphan_violation(&self, d: &Def, tb: &str, tgt: &str) -> bool {
        let Some(&ti) = self.p.types.get(tb) else { return false };
        let td = self.def(ti);
        if td.kw != "trait" {
            return false; // トレイトでない名前はここでは扱わない。
        }
        if self.user.contains(&ti) {
            return false; // このパッケージのトレイトなら常に許す。
        }
        // 対象が型パラメータ (blanket impl) なら黙る。
        if d.gnames().contains(tgt) {
            return false;
        }
        // 対象の形: 名義型と基本型だけが impl の対象。それ以外の形は黙る。
        match tag(&d.impl_for).as_str() {
            "prim" => return true, // 基本型はユーザーパッケージでは定義できない
            "named" => {}
            _ => return false,
        }
        match self.p.types.get(tgt) {
            // このパッケージの名義型なら許す。
            Some(&j) if self.user.contains(&j) && matches!(self.def(j).kw.as_str(), "type" | "enum") => false,
            // 部外の型 (標準ライブラリ・依存パッケージ)。トレイトも部外なので違反。
            Some(_) => true,
            // 未解決の名前。
            None => false,
        }
    }

    /// 4.3: `derive` は生成するトレイトの `requires` が揃うときだけ可能 (E2035)。
    /// `Copy` は全フィールドが `Copy` でなければならない (E2036)。
    fn check_derive(&mut self) {
        let short_to_fqn: HashMap<&str, &str> = DERIVE_TRAITS.iter().copied().collect();
        let fqn_to_short: HashMap<&str, &str> = DERIVE_TRAITS.iter().map(|(s, f)| (*f, *s)).collect();
        let user_types: HashSet<String> = self
            .user
            .iter()
            .map(|&i| self.def(i))
            .filter(|d| matches!(d.kw.as_str(), "type" | "enum"))
            .map(|d| d.name.clone())
            .collect();
        let idxs: Vec<usize> = self.user.clone();
        for i in idxs {
            let d = self.def(i);
            if !matches!(d.kw.as_str(), "type" | "enum") || d.derive.is_empty() {
                continue;
            }
            let derived: HashSet<String> = d.derive.clone().into_iter().collect();
            for dv in d.derive.clone() {
                let Some(&fqn) = short_to_fqn.get(dv.as_str()) else { continue };
                let Some(&ti) = self.p.types.get(fqn) else { continue };
                for (rb, _) in self.def(ti).requires.clone() {
                    if let Some(&rs) = fqn_to_short.get(rb.as_str()) {
                        if !derived.contains(rs) && !self.has_manual_marker(&d.name, &rb) {
                            self.diag(
                                "E2035",
                                format!("derive [{}] on {} also requires `{}`", dv, d.name, rs),
                                i,
                                0,
                            );
                        }
                    }
                }
            }
            if derived.contains("Copy") {
                let tys: Vec<V> = if d.kw == "type" {
                    d.fields.iter().map(|f| f.1.clone()).collect()
                } else {
                    d.variants.iter().flat_map(|v| v.1.clone()).collect()
                };
                if tys.iter().any(|t| self.is_copy_v(t, &user_types) == Some(false)) {
                    self.diag("E2036", format!("derive [Copy] on {}: every field must be Copy", d.name), i, 0);
                }
            }
        }
    }

    /// 型が `Copy` か。`Some(false)` は明確に `Copy` でない (Copy を derive していないユーザー型など)。
    fn is_copy_v(&self, t: &V, user_types: &HashSet<String>) -> Option<bool> {
        match tag(t).as_str() {
            "prim" | "never" | "ptr" => Some(true),
            // `&T` は Copy だが `&mut T` は Copy でも Clone でもない (4.2)。
            "ref" => Some(!matches!(tel(t, 1), V::B(true))),
            "array" | "slice" => self.is_copy_v(tel(t, 1), user_types),
            "tuple" => {
                let mut res = Some(true);
                for x in items(tel(t, 1)) {
                    res = and3(res, self.is_copy_v(x, user_types));
                }
                res
            }
            "named" => {
                let n = st(tel(t, 1));
                if self.has_manual_marker(&n, "core.ops.Copy") {
                    return Some(true);
                }
                if !user_types.contains(&n) {
                    return None;
                }
                let is_copy = self.p.types.get(&n).map(|&i| self.def(i).derive.iter().any(|x| x == "Copy")).unwrap_or(false);
                Some(is_copy)
            }
            _ => None,
        }
    }

    /// 4.4: `linear` 型には `Clone` / `Copy` を derive できず、`core.ops.Drop` も実装できない (E2032)。
    fn check_linear(&mut self) {
        let idxs: Vec<usize> = self.user.clone();
        let linear_types: HashSet<String> = idxs
            .iter()
            .map(|&i| self.def(i))
            .filter(|d| matches!(d.kw.as_str(), "type" | "enum") && d.linear)
            .map(|d| d.name.clone())
            .collect();
        if linear_types.is_empty() {
            return;
        }
        for i in idxs {
            let d = self.def(i);
            if matches!(d.kw.as_str(), "type" | "enum") && d.linear {
                for dv in d.derive.clone() {
                    if dv == "Clone" || dv == "Copy" {
                        self.diag("E2032", format!("linear type {} must not derive {}", d.name, dv), i, 0);
                    }
                }
            }
            if d.kw == "impl" && d.impl_trait == "core.ops.Drop" {
                let tgt = impl_target(&d.impl_for);
                if linear_types.contains(&tgt) {
                    self.diag("E2032", format!("linear type {} must not implement core.ops.Drop (4.4)", tgt), i, 0);
                }
            }
        }
    }

    /// 3.2: `test <対象FQN>.test_<名前>` の対象FQNが存在し、テストと同じファイルにあること (E1116)。
    fn check_tests(&mut self) {
        let tests: Vec<usize> = self.user.iter().copied().filter(|&i| self.def(i).kw == "test").collect();
        for i in tests {
            let d = self.def(i);
            let Some((target, _)) = d.name.rsplit_once('.') else { continue };
            let file = d.file.clone();
            let name = d.name.clone();
            let target = target.to_string();
            let found = self
                .p
                .funcs
                .get(&target)
                .or_else(|| self.p.types.get(&target))
                .or_else(|| self.p.consts.get(&target))
                .or_else(|| self.p.sigs.get(&target))
                .copied();
            match found {
                None => self.diag("E1116", format!("test {} has no target definition {}", name, target), i, 0),
                Some(j) => {
                    if self.def(j).file != file {
                        self.diag("E1116", format!("test {} must be in the same file as {}", name, target), i, 0);
                    }
                }
            }
        }
    }

    /// 3.1: `static` の型は `core.marker.Sync` かつ非線形でなければならない (E2031)。
    /// 分からない型 (標準ライブラリの型・型パラメータなど) では何も言わない。
    fn check_statics(&mut self) {
        let statics: Vec<usize> = self.user.iter().copied().filter(|&i| self.def(i).kw == "static").collect();
        if statics.is_empty() {
            return;
        }
        let user_types: HashSet<String> = self
            .user
            .iter()
            .map(|&i| self.def(i))
            .filter(|d| matches!(d.kw.as_str(), "type" | "enum"))
            .map(|d| d.name.clone())
            .collect();
        for i in statics {
            let d = self.def(i);
            let t = d.ret.clone();
            let gn = d.gnames();
            let name = d.name.clone();
            if self.sync_v(&t, &gn, &user_types, &mut HashSet::new()) == Some(false) {
                self.diag("E2031", format!("static {} must have a core.marker.Sync type (3.1)", name), i, 0);
            }
            if self.linear_v(&t, &gn, &user_types, &mut HashSet::new()) == Some(true) {
                self.diag("E2031", format!("static {} must not have a linear type (3.1)", name), i, 0);
            }
        }
    }

    /// 型が `core.marker.Sync` を満たすか。`Some(false)` は明確に満たさない (生ポインタを含むなど)。
    fn sync_v(
        &self,
        t: &V,
        gn: &HashSet<String>,
        user_types: &HashSet<String>,
        seen: &mut HashSet<String>,
    ) -> Option<bool> {
        match tag(t).as_str() {
            "prim" | "never" => Some(true),
            "tuple" => {
                let mut res = Some(true);
                for x in items(tel(t, 1)) {
                    res = and3(res, self.sync_v(x, gn, user_types, seen));
                }
                res
            }
            "ref" => self.sync_v(tel(t, 2), gn, user_types, seen),
            // 生ポインタは Send でも Sync でもない (5.5)。
            "ptr" => Some(false),
            "slice" | "array" => self.sync_v(tel(t, 1), gn, user_types, seen),
            "named" => {
                let n = st(tel(t, 1));
                if gn.contains(&n) {
                    return None;
                }
                if self.has_manual_marker(&n, "core.marker.Sync") {
                    return Some(true);
                }
                if !user_types.contains(&n) {
                    return None;
                }
                if !seen.insert(n.clone()) {
                    return Some(true);
                }
                let res = self
                    .p
                    .types
                    .get(&n)
                    .map(|&i| self.def(i))
                    .map(|d| {
                        let tys: Vec<&V> = if d.kw == "type" {
                            d.fields.iter().map(|f| &f.1).collect()
                        } else {
                            d.variants.iter().flat_map(|v| v.1.iter()).collect()
                        };
                        let mut r = Some(true);
                        for x in tys {
                            r = and3(r, self.sync_v(x, gn, user_types, seen));
                        }
                        r
                    })
                    .unwrap_or(None);
                seen.remove(&n);
                res
            }
            _ => None,
        }
    }

    /// 型が `core.marker.Send` を満たすか。`Some(false)` は明確に満たさない (5.5)。
    /// `&T` は `T: Sync` のとき Send、`&mut T` は `T: Send` のとき Send。
    fn send_v(
        &self,
        t: &V,
        gn: &HashSet<String>,
        user_types: &HashSet<String>,
        seen: &mut HashSet<String>,
    ) -> Option<bool> {
        match tag(t).as_str() {
            "prim" | "never" => Some(true),
            "tuple" => {
                let mut res = Some(true);
                for x in items(tel(t, 1)) {
                    res = and3(res, self.send_v(x, gn, user_types, seen));
                }
                res
            }
            "ref" => {
                if matches!(tel(t, 1), V::B(true)) {
                    self.send_v(tel(t, 2), gn, user_types, seen)
                } else {
                    self.sync_v(tel(t, 2), gn, user_types, seen)
                }
            }
            "ptr" => Some(false),
            "slice" | "array" => self.send_v(tel(t, 1), gn, user_types, seen),
            "named" => {
                let n = st(tel(t, 1));
                if gn.contains(&n) {
                    return None;
                }
                if self.has_manual_marker(&n, "core.marker.Send") {
                    return Some(true);
                }
                if !user_types.contains(&n) {
                    return None;
                }
                if !seen.insert(n.clone()) {
                    return Some(true);
                }
                let res = self
                    .p
                    .types
                    .get(&n)
                    .map(|&i| self.def(i))
                    .map(|d| {
                        let tys: Vec<&V> = if d.kw == "type" {
                            d.fields.iter().map(|f| &f.1).collect()
                        } else {
                            d.variants.iter().flat_map(|v| v.1.iter()).collect()
                        };
                        let mut r = Some(true);
                        for x in tys {
                            r = and3(r, self.send_v(x, gn, user_types, seen));
                        }
                        r
                    })
                    .unwrap_or(None);
                seen.remove(&n);
                res
            }
            _ => None,
        }
    }

    /// 型が線形な値を含むか。`Some(true)` は明確に線形。
    fn linear_v(
        &self,
        t: &V,
        gn: &HashSet<String>,
        user_types: &HashSet<String>,
        seen: &mut HashSet<String>,
    ) -> Option<bool> {
        match tag(t).as_str() {
            "prim" | "never" | "ptr" | "ref" => Some(false),
            "tuple" => {
                let mut res = Some(false);
                for x in items(tel(t, 1)) {
                    res = or3(res, self.linear_v(x, gn, user_types, seen));
                }
                res
            }
            "slice" | "array" => self.linear_v(tel(t, 1), gn, user_types, seen),
            "named" => {
                let n = st(tel(t, 1));
                if gn.contains(&n) || !user_types.contains(&n) {
                    return None;
                }
                if !seen.insert(n.clone()) {
                    return Some(false);
                }
                let res = self
                    .p
                    .types
                    .get(&n)
                    .map(|&i| self.def(i))
                    .map(|d| {
                        let tys: Vec<&V> = if d.kw == "type" {
                            d.fields.iter().map(|f| &f.1).collect()
                        } else {
                            d.variants.iter().flat_map(|v| v.1.iter()).collect()
                        };
                        let mut r = if d.linear { Some(true) } else { Some(false) };
                        for x in tys {
                            r = or3(r, self.linear_v(x, gn, user_types, seen));
                        }
                        r
                    })
                    .unwrap_or(None);
                seen.remove(&n);
                res
            }
            _ => None,
        }
    }

    /// このファイルで `import` したモジュールをFQNで参照していれば、そのモジュール名を返す (3.3)。
    fn imported_fqn(&self, segs: &[String]) -> Option<String> {
        self.imported_modules
            .iter()
            .find(|m| segs.len() >= m.len() && segs[..m.len()] == m[..])
            .map(|m| m.join("."))
    }

    /// `core.ops.Drop` の `drop` メソッドが定義されているか (直接呼び出しの検出用)。
    fn has_drop_trait_method(&self) -> bool {
        self.p
            .method_idx
            .get("drop")
            .map_or(false, |v| v.iter().any(|&i| self.def(i).impls_trait == "core.ops.Drop"))
    }

    /// 手動のマーカー impl (`impl core.marker.Sync for X`) があるか。
    fn has_manual_marker(&self, ty: &str, marker: &str) -> bool {
        self.p.defs.iter().any(|d| d.kw == "impl" && d.impl_trait == marker && impl_target(&d.impl_for) == ty)
    }

    /// 3.2: 型のフィールド名とメソッド名は重複できない (`ksn get <型FQN>.<名前>` を一意にするため)。
    fn check_field_method(&mut self) {
        let mut fields: HashMap<String, HashSet<String>> = HashMap::new();
        for &i in &self.user {
            let d = self.def(i);
            if d.kw == "type" {
                fields.insert(d.name.clone(), d.fields.iter().map(|f| f.0.clone()).collect());
            }
        }
        if fields.is_empty() {
            return;
        }
        let methods: Vec<usize> =
            self.user.iter().copied().filter(|&i| self.def(i).kw == "fn" && self.def(i).is_method()).collect();
        for i in methods {
            let d = self.def(i);
            let owner = d.name.rsplit_once('.').map(|(o, _)| o).unwrap_or("");
            let meth = d.name.rsplit('.').next().unwrap_or("");
            if fields.get(owner).map_or(false, |fs| fs.contains(meth)) {
                self.diag("E1115", format!("method {} collides with a field of {}", d.name, owner), i, 0);
            }
        }
    }

    /// 型の辺 (フィールド・バリアント・タプル・配列・スライス・型引数・参照・生ポインタ) をたどって
    /// 自分自身に戻れる型 (自己参照・相互再帰) の名前を集める (5.3)。
    fn recursive_types(&self) -> HashSet<String> {
        let defs: Vec<&Def> = self
            .user
            .iter()
            .map(|&j| self.def(j))
            .filter(|d| matches!(d.kw.as_str(), "type" | "enum"))
            .collect();
        let idx: HashMap<&str, usize> = defs.iter().enumerate().map(|(k, d)| (d.name.as_str(), k)).collect();
        let n = defs.len();
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (k, d) in defs.iter().enumerate() {
            let tys: Vec<&V> = if d.kw == "type" {
                d.fields.iter().map(|f| &f.1).collect()
            } else {
                d.variants.iter().flat_map(|v| v.1.iter()).collect()
            };
            let mut names = HashSet::new();
            for t in tys {
                collect_ty_names(t, &mut names);
            }
            for nm in names {
                if let Some(&m) = idx.get(nm.as_str()) {
                    if !adj[k].contains(&m) {
                        adj[k].push(m);
                    }
                }
            }
        }
        let mut out = HashSet::new();
        for k in 0..n {
            let mut seen = vec![false; n];
            let mut stack = adj[k].clone();
            while let Some(m) = stack.pop() {
                if m == k {
                    out.insert(defs[k].name.clone());
                    break;
                }
                if !seen[m] {
                    seen[m] = true;
                    stack.extend(adj[m].iter().copied());
                }
            }
        }
        out
    }

    // ---- 型の参照
    fn check_header_types(&mut self, di: usize, tys: &[V]) {
        let names = self.def(di).gnames();
        for t in tys {
            self.ty_names(t, di, &names);
        }
    }

    fn ty_names(&mut self, t: &V, di: usize, names: &HashSet<String>) {
        match tag(t).as_str() {
            "named" => {
                let nm = st(tel(t, 1));
                if PRELUDE_FQN.contains(&nm.as_str()) {
                    let bare = nm.rsplit('.').next().unwrap();
                    self.diag("E1110", format!("write the prelude name `{}` without its module path", bare), di, 0);
                }
                if !nm.contains('.') {
                    if !names.contains(&nm) && !PRELUDE_TYPES.contains(&nm.as_str()) {
                        self.diag("E1102", format!("unknown type {}", nm), di, 0);
                    }
                } else if !self.p.types.contains_key(&nm) {
                    self.diag("E1102", format!("unknown type {}", nm), di, 0);
                }
                for a in items(tel(t, 2)) {
                    if matches!(tag(a).as_str(), "named" | "ref" | "tuple" | "array" | "slice" | "prim") {
                        self.ty_names(a, di, names);
                    }
                }
            }
            "dyn" => self.check_dyn_types(tel(t, 1), di),
            "ref" | "ptr" => self.ty_names(tel(t, 2), di, names),
            "slice" | "array" => self.ty_names(tel(t, 1), di, names),
            "tuple" => {
                for x in items(tel(t, 1)) {
                    self.ty_names(x, di, names);
                }
            }
            "fn" => {
                for x in items(tel(t, 1)) {
                    self.ty_names(x, di, names);
                }
                self.ty_names(tel(t, 2), di, names);
            }
            _ => {}
        }
    }

    // ---- dyn 安全性 (5.1)
    fn trait_def(&self, name: &str) -> Option<&'a Def> {
        let &i = self.p.types.get(name)?;
        let d = &self.p.defs[i];
        if d.kw == "trait" {
            Some(d)
        } else {
            None
        }
    }

    /// `dyn Tr` (の使用箇所) を検査する (xref.py の dyn_problems の移植)。
    fn check_dyn_types(&mut self, trait_ref: &V, di: usize) {
        let f = items(trait_ref);
        if f.is_empty() {
            return;
        }
        let nm = st(&f[0]);
        let Some(base) = resolve_trait_fqn(self.p, &nm) else { return };
        let mut problems = Vec::new();
        let mut seen = HashSet::new();
        self.dyn_problems(&base, &mut seen, &mut problems);
        for p in problems {
            self.diag("E2033", format!("dyn {} is not dyn-safe: {}", nm, p), di, 0);
        }
    }

    /// トレイト `tr` とその推移的スーパートレイトが dyn 安全かを調べ、違反を out に足す。
    fn dyn_problems(&self, tr: &str, seen: &mut HashSet<String>, out: &mut Vec<String>) {
        if !seen.insert(tr.to_string()) {
            return;
        }
        let Some(td) = self.trait_def(tr) else { return };
        for meth in &td.methods {
            if td.generic_methods.iter().any(|m| m == meth) {
                out.push(format!("{}.{} is generic", tr, meth));
                continue;
            }
            let Some((_, params, ret)) = td.trait_sigs.iter().find(|s| &s.0 == meth) else { continue };
            let has_receiver = params.first().map_or(false, |p| p.0 == "self");
            if has_receiver && tag(&params[0].1) == "self" {
                out.push(format!("{}.{} takes self by value", tr, meth));
            }
            for p in params.iter().skip(if has_receiver { 1 } else { 0 }) {
                if ty_has_self(&p.1) {
                    out.push(format!("{}.{} uses Self in an argument", tr, meth));
                    break;
                }
            }
            if ty_has_self(ret) {
                out.push(format!("{}.{} uses Self in the return type", tr, meth));
            }
        }
        for (base, args) in &td.requires {
            if items(args).iter().any(ty_has_self) {
                out.push(format!("{} requires Self in a supertrait argument", tr));
            }
            if let Some(sup) = resolve_trait_fqn(self.p, base) {
                self.dyn_problems(&sup, seen, out);
            }
        }
    }

    fn check_cycles(&mut self) {
        let mut mods: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut names: BTreeSet<String> = BTreeSet::new();
        for &i in &self.user {
            let d = self.def(i);
            names.insert(d.module.clone());
            if d.kw == "import" {
                mods.entry(d.module.clone()).or_default().insert(d.name.clone());
            }
        }
        let mut color: HashMap<String, u8> = HashMap::new();
        fn dfs(
            m: &str,
            path: &mut Vec<String>,
            mods: &BTreeMap<String, BTreeSet<String>>,
            names: &BTreeSet<String>,
            color: &mut HashMap<String, u8>,
        ) -> Option<Vec<String>> {
            color.insert(m.to_string(), 1);
            if let Some(ns) = mods.get(m) {
                for n in ns {
                    if !names.contains(n) {
                        continue;
                    }
                    if color.get(n) == Some(&1) {
                        let mut r = path.clone();
                        r.push(m.to_string());
                        r.push(n.clone());
                        return Some(r);
                    }
                    if !color.contains_key(n) {
                        path.push(m.to_string());
                        let r = dfs(n, path, mods, names, color);
                        path.pop();
                        if r.is_some() {
                            return r;
                        }
                    }
                }
            }
            color.insert(m.to_string(), 2);
            None
        }
        for m in names.clone() {
            if !color.contains_key(&m) {
                if let Some(r) = dfs(&m, &mut vec![], &mods, &names, &mut color) {
                    let di = *self.user.iter().find(|&&i| self.def(i).module == r[0]).unwrap();
                    self.diag("E1105", format!("module cycle: {}", r.join(" -> ")), di, 0);
                    return;
                }
            }
        }
    }

    // ---- fn 本体
    fn check_fn_sema(&mut self, di: usize, test: bool) {
        let d = self.def(di);
        let mut tys: Vec<V> = d.params.iter().map(|p| p.1.clone()).collect();
        tys.push(d.ret.clone());
        self.check_header_types(di, &tys);
        let mut locals: HashSet<String> = d.params.iter().map(|p| p.0.clone()).collect();
        // 4.1: `import` したモジュールの末尾名も可視なので、同名の束縛はシャドーイングになる。
        // 変数の解決には使わない (別集合)。
        self.import_tails = self.p.imports.get(&d.file).map(|m| m.keys().cloned().collect()).unwrap_or_default();
        self.imported_modules = self
            .p
            .imports
            .get(&d.file)
            .map(|m| m.values().map(|v| v.split('.').map(String::from).collect()).collect())
            .unwrap_or_default();
        self.cur_uses = if test { None } else { Some(d.uses.clone()) };
        self.cur_panics = if test { "may".into() } else { d.panics.clone().unwrap_or("may".into()) };
        self.in_const_fn = !test && d.is_const;
        self.unsafe_ok = self.module_may_use_unsafe(&d.module);
        let body = d.body.as_ref().unwrap();
        self.visit(body, &mut locals, di, test);
    }

    fn visit(&mut self, e: &V, locals: &mut HashSet<String>, di: usize, free: bool) {
        let f = nf(e);
        match nname(e) {
            "Block" => {
                let mut inner = locals.clone();
                for s in items(&f[0]) {
                    self.visit(s, &mut inner, di, free);
                }
            }
            "Let" => {
                self.visit(&f[3], locals, di, free);
                if nname(&f[0]) == "PWild" {
                    self.diag("E2041", "`let _ = e` is not allowed; use core.mem.drop(e)".into(), di, nline(e));
                }
                let mut b = vec![];
                pat_binds(&f[0], &mut b);
                self.shadow_check(&b, locals, di, nline(e));
                for n in b {
                    locals.insert(n);
                }
                self.visit_pat(&f[0], di);
            }
            "Match" => {
                self.visit(&f[0], locals, di, free);
                for arm in items(&f[1]) {
                    let pat = &items(arm)[0];
                    let body = &items(arm)[1];
                    let mut inner = locals.clone();
                    let mut b = vec![];
                    pat_binds(pat, &mut b);
                    self.shadow_check(&b, locals, di, nline(e));
                    inner.extend(b);
                    self.visit_pat(pat, di);
                    self.visit(body, &mut inner, di, free);
                }
            }
            "For" => {
                self.visit(&f[1], locals, di, free);
                let mut inner = locals.clone();
                let mut b = vec![];
                pat_binds(&f[0], &mut b);
                self.shadow_check(&b, locals, di, nline(e));
                inner.extend(b);
                self.visit(&f[2], &mut inner, di, free);
            }
            "Closure" => {
                let mut inner = locals.clone();
                let names: Vec<String> = items(&f[0]).iter().map(|p| st(&items(p)[0])).collect();
                self.shadow_check(&names, locals, di, nline(e));
                inner.extend(names);
                self.visit(&f[3], &mut inner, di, true);
            }
            "Path" => {
                let segs = strs(&f[0]);
                if !locals.contains(&segs[0]) {
                    if let Some(m) = self.imported_fqn(&segs) {
                        self.diag(
                            "E1117",
                            format!("module {} is imported; use its short name instead of the FQN (3.3)", m),
                            di,
                            nline(e),
                        );
                    }
                    if let Some(r) = self.resolve_path(&segs, di, nline(e)) {
                        // 型の名前を値として使っている (`std.alloc.System` ではなく `std.alloc.SYSTEM`)。
                        if matches!(r.target, Target::Type(_)) && r.rest.is_empty() {
                            let name = segs.join(".");
                            let want = name.to_lowercase();
                            let hint = self
                                .p
                                .consts
                                .keys()
                                .find(|k| k.to_lowercase() == want)
                                .map(|k| format!("; did you mean the constant {}?", k))
                                .unwrap_or_default();
                            self.diag("E1109", format!("{} is a type, not a value{}", name, hint), di, nline(e));
                        }
                    }
                }
            }
            "Call" => {
                for a in items(&f[1]) {
                    self.visit(a, locals, di, free);
                }
                let fnn = &f[0];
                if nname(fnn) == "Path" {
                    let segs = strs(&nf(fnn)[0]);
                    // 4.4: `core.ops.Drop.drop` は直接呼べない (二重 drop を防ぐ)。
                    if segs.join(".") == "core.ops.Drop.drop" {
                        self.diag(
                            "E3004",
                            "core.ops.Drop.drop cannot be called directly; use core.mem.drop(x)".into(),
                            di,
                            nline(fnn),
                        );
                    }
                    if locals.contains(&segs[0]) {
                        // `v.push(x)` は局所変数のメソッド呼び出し (パスとして構文解析される)。
                        if segs.len() >= 2 {
                            let name = segs[segs.len() - 1].clone();
                            if name == "drop" && self.has_drop_trait_method() {
                                self.diag(
                                    "E3004",
                                    "core.ops.Drop.drop cannot be called directly; use core.mem.drop(x)".into(),
                                    di,
                                    nline(fnn),
                                );
                            }
                            self.check_method_exists(&name, di, nline(fnn));
                        }
                        return;
                    }
                    if let Some(m) = self.imported_fqn(&segs) {
                        self.diag(
                            "E1117",
                            format!("module {} is imported; use its short name instead of the FQN (3.3)", m),
                            di,
                            nline(fnn),
                        );
                    }
                    // 5.5: spawn に渡すクロージャは 'static。局所束縛への参照キャプチャは不可。
                    if self.p.expand(&segs, &self.def(di).file).join(".") == "std.thread.spawn" {
                        if let Some(cl) = items(&f[1]).get(1) {
                            if nname(cl) == "Closure" {
                                for c in items(&nf(cl)[2]) {
                                    let parts = items(c);
                                    if parts.len() == 2 {
                                        let kind = st(&parts[0]);
                                        if kind == "&" || kind == "&mut" {
                                            self.diag(
                                                "E2037",
                                                format!(
                                                    "spawn closure must be 'static; cannot capture `{}` by reference",
                                                    st(&parts[1])
                                                ),
                                                di,
                                                nline(c),
                                            );
                                        }
                                    }
                                }
                                // 5.5: spawn の結果型 T も `Send` でなければならない。
                                let user_types: HashSet<String> = self
                                    .user
                                    .iter()
                                    .map(|&i| self.def(i))
                                    .filter(|d| matches!(d.kw.as_str(), "type" | "enum"))
                                    .map(|d| d.name.clone())
                                    .collect();
                                if self.send_v(&nf(cl)[1], &HashSet::new(), &user_types, &mut HashSet::new())
                                    == Some(false)
                                {
                                    self.diag("E2040", "spawn result type must be core.marker.Send (5.5)".into(), di, nline(cl));
                                }
                            }
                        }
                    }
                    let r = self.resolve_path(&segs, di, nline(fnn));
                    if let Some(r) = r {
                        if matches!(r.target, Target::Fn(_) | Target::Sig(_)) && r.rest.is_empty() {
                            let callee = self.p.fn_def(&r).unwrap();
                            // 3.2: `const fn` の本体から呼べるのは `const fn` と組み込みのみ。
                            if self.in_const_fn && !callee.is_const {
                                self.diag(
                                    "E2038",
                                    format!("`const fn` cannot call non-const fn {}", callee.name),
                                    di,
                                    nline(e),
                                );
                            }
                            let n = items(&f[1]).len();
                            if callee.params.len() != n {
                                self.diag(
                                    "E2001",
                                    format!("{} takes {} argument(s) but {} given", callee.name, callee.params.len(), n),
                                    di,
                                    nline(e),
                                );
                            }
                            // 4.3: 明示型引数 (`f::<[io.fs], never>`) があれば効果変数を実体化する。
                            self.effects(callee, di, nline(e), free, items(&f[2]));
                        } else if matches!(r.target, Target::Native(_)) && r.rest.is_empty() {
                            let fq = self.p.expand(&segs, "").join(".");
                            if let Some(&si) = self.p.sigs.get(&fq) {
                                self.effects(self.def(si), di, nline(e), free, items(&f[2]));
                            }
                        }
                    }
                } else {
                    self.visit(fnn, locals, di, free);
                }
            }
            "MCall" => {
                self.visit(&f[0], locals, di, free);
                for a in items(&f[2]) {
                    self.visit(a, locals, di, free);
                }
                let name = st(&f[1]);
                self.check_method_exists(&name, di, nline(e));
                // 4.4: `core.ops.Drop.drop` (トレイトの drop) のメソッド呼び出しは直接呼べない。
                if name == "drop" {
                    if self.has_drop_trait_method() {
                        self.diag(
                            "E3004",
                            "core.ops.Drop.drop cannot be called directly; use core.mem.drop(x)".into(),
                            di,
                            nline(e),
                        );
                    }
                }
                let n = items(&f[2]).len();
                let cands: Vec<&Def> = self
                    .p
                    .method_idx
                    .get(&name)
                    .map(|v| v.iter().map(|&i| self.def(i)).filter(|c| c.params.len() == n + 1).collect())
                    .unwrap_or_default();
                if !cands.is_empty() {
                    // 4.3: メソッド呼び出しの明示型引数 (`x.m::<..>`) があれば効果変数を実体化する。
                    self.effects_all(&cands, di, nline(e), free, items(&f[3]));
                }
            }
            "Index" => {
                self.visit(&f[0], locals, di, free);
                self.visit(&f[1], locals, di, free);
                if !free && self.cur_panics == "never" {
                    self.diag(
                        "E4003",
                        "indexing may panic; use .get() in a `panics never` function".into(),
                        di,
                        nline(e),
                    );
                }
            }
            "Unsafe" => {
                if !self.unsafe_ok {
                    self.diag("E5001", "unsafe is only allowed in modules listed in [unsafe] modules".into(), di, nline(e));
                }
                self.visit(&f[2], locals, di, free);
            }
            "StructLit" => {
                let name = st(&f[0]);
                let segs: Vec<String> = name.split('.').map(String::from).collect();
                let fq = self.p.expand(&segs, &self.def(di).file).join(".");
                match self.p.types.get(&fq) {
                    None => self.diag("E1101", format!("unknown struct {}", name), di, nline(e)),
                    Some(&ti) => {
                        let td = self.def(ti);
                        if td.kw == "type" {
                            let known: Vec<String> = td.fields.iter().map(|x| x.0.clone()).collect();
                            let given: Vec<String> = items(&f[1]).iter().map(|x| st(&items(x)[0])).collect();
                            for g in &given {
                                if !known.contains(g) {
                                    self.diag("E2010", format!("{} has no field {}", fq, g), di, nline(e));
                                }
                            }
                            for k in &known {
                                if !given.contains(k) {
                                    self.diag("E2011", format!("missing field {} in {} literal", k, fq), di, nline(e));
                                }
                            }
                        }
                    }
                }
                for x in items(&f[1]) {
                    self.visit(&items(x)[1], locals, di, free);
                }
            }
            _ => {
                for c in children(e) {
                    if !is_pat(c) {
                        self.visit(c, locals, di, free);
                    }
                }
            }
        }
    }

    /// 呼ばれたメソッド名が、どこにも定義・宣言されていなければ E1108 (近い名前を提案する)。
    fn check_method_exists(&mut self, name: &str, di: usize, line: usize) {
        if self.method_known(name) {
            return;
        }
        let msg = match self.nearest_method(name) {
            Some(n) => format!("unknown method `{}`; did you mean `{}`?", name, n),
            None => format!("unknown method `{}`", name),
        };
        self.diag("E1108", msg, di, line);
    }

    fn method_known(&self, name: &str) -> bool {
        const DERIVED: &[&str] = &[
            "clone", "eq", "ne", "cmp", "partial_cmp", "hash", "debug_fmt", "display_fmt", "call", "call_mut",
            "call_once", "drop", "try_clone", "to_string",
        ];
        const WRAPPING: &[&str] = &["add", "sub", "mul", "neg", "shl", "shr"];
        const CHECKED: &[&str] = &["add", "sub", "mul", "div", "rem", "neg", "shl", "shr", "abs"];
        const SATURATING: &[&str] = &["add", "sub", "mul", "neg"];
        for (prefix, ops) in [("wrapping_", WRAPPING), ("checked_", CHECKED), ("saturating_", SATURATING)] {
            if let Some(op) = name.strip_prefix(prefix) {
                return ops.contains(&op);
            }
        }
        self.p.method_idx.contains_key(name)
            || crate::natives::known_num(name)
            || DERIVED.contains(&name)
            || self.p.types.values().any(|&i| {
                let d = &self.p.defs[i];
                d.kw == "trait" && d.methods.iter().any(|m| m == name)
            })
    }

    fn nearest_method(&self, name: &str) -> Option<String> {
        // `try_push` のように、既存のメソッド名を含む (または含まれる) 名前を先に提案する。
        let mut contained: Vec<&String> = self
            .p
            .method_idx
            .keys()
            .filter(|c| c.len() >= 3 && c.as_str() != name && (name.contains(c.as_str()) || c.contains(name)))
            .collect();
        contained.sort_by_key(|c| (c.len().abs_diff(name.len()), c.as_str()));
        if let Some(c) = contained.first() {
            return Some((*c).clone());
        }
        let mut best: Option<(usize, String)> = None;
        for cand in self.p.method_idx.keys() {
            let d = edit_distance(name, cand);
            let limit = (name.len().max(cand.len()) / 3).max(1);
            if d <= limit && best.as_ref().map_or(true, |(bd, bn)| d < *bd || (d == *bd && cand < bn)) {
                best = Some((d, cand.clone()));
            }
        }
        best.map(|(_, n)| n)
    }

    /// 4.1: 可視な束縛と同名の宣言はシャドーイングでエラー。
    fn shadow_check(&mut self, binds: &[String], locals: &HashSet<String>, di: usize, line: usize) {
        for n in binds {
            let shadows = locals.contains(n) || self.import_tails.contains(n);
            if shadows {
                self.diag("E1021", format!("`{}` shadows an existing binding", n), di, line);
            }
        }
    }

    fn visit_pat(&mut self, p: &V, di: usize) {
        let n = nname(p);
        let f = nf(p);
        if matches!(n, "PPath" | "PVariant" | "PStruct") {
            let segs = strs(&f[0]);
            if segs.len() == 1 && matches!(segs[0].as_str(), "None" | "Some" | "Ok" | "Err") {
                return;
            }
            if PRELUDE_FQN.contains(&segs.join(".").as_str()) {
                let bare = segs.last().unwrap();
                self.diag("E1110", format!("write the prelude name `{}` without its module path", bare), di, 0);
            }
            let r = self.p.resolve(&segs, &self.def(di).file);
            let ok = matches!(r, Some(ref x) if matches!(x.target, Target::Variant(..) | Target::Const(_) | Target::Type(_)));
            if !ok {
                self.diag("E1101", format!("unresolved pattern {}", segs.join(".")), di, 0);
            }
        }
        match n {
            "PTuple" => {
                for x in items(&f[0]) {
                    self.visit_pat(x, di);
                }
            }
            "PVariant" => {
                for x in items(&f[1]) {
                    self.visit_pat(x, di);
                }
            }
            "PStruct" => {
                for x in items(&f[1]) {
                    self.visit_pat(&items(x)[1], di);
                }
            }
            "PRef" => self.visit_pat(&f[0], di),
            _ => {}
        }
    }

    fn resolve_path(&mut self, segs: &[String], di: usize, line: usize) -> Option<Resolved> {
        let r = self.p.resolve(segs, &self.def(di).file);
        if r.is_none() {
            let bad = segs.join(".");
            let msg = match self.nearest_name(segs) {
                Some(n) => format!("unresolved name {}; did you mean {}?", bad, n),
                None => format!("unresolved name {}", bad),
            };
            self.diag("E1101", msg, di, line);
        }
        r
    }

    /// 解決できない名前に近い定義名を探す: 末尾 2 成分が同じものを先に、次に編集距離で。
    fn nearest_name(&self, segs: &[String]) -> Option<String> {
        let bad = segs.join(".");
        let mut all: Vec<&String> = vec![];
        all.extend(self.p.funcs.keys());
        all.extend(self.p.sigs.keys());
        all.extend(self.p.types.keys());
        all.extend(self.p.consts.keys());
        all.extend(self.p.natives.iter());
        if segs.len() >= 2 {
            let tail = format!(".{}", segs[segs.len() - 2..].join("."));
            let mut hits: Vec<&&String> = all.iter().filter(|n| n.ends_with(&tail)).collect();
            hits.sort();
            if let Some(h) = hits.first() {
                return Some((**h).clone());
            }
        }
        let mut best: Option<(usize, String)> = None;
        for n in all {
            let d = edit_distance(&bad, n);
            let limit = (bad.len().max(n.len()) / 4).max(2);
            if d <= limit && best.as_ref().map_or(true, |(bd, bn)| d < *bd || (d == *bd && n < bn)) {
                best = Some((d, n.clone()));
            }
        }
        best.map(|(_, n)| n)
    }

    fn violation(&self, callee: &Def) -> Option<(String, String)> {
        self.violation_effs(&callee.name, &callee.uses, callee.panics.as_deref())
    }

    /// 呼び出し側の `uses` / `panics` が、callee の効果 (5.1 の階層の部分型) を覆うか。
    fn violation_effs(&self, name: &str, uses: &[String], panics: Option<&str>) -> Option<(String, String)> {
        let cu = self.cur_uses.as_ref()?;
        for eff in uses {
            if !covers(cu, eff) {
                return Some(("E4001".into(), format!("{} uses effect `{}` not declared by the caller", name, eff)));
            }
        }
        if panics == Some("may") && self.cur_panics == "never" {
            return Some((
                "E4002".into(),
                format!("{} may panic but the caller declares `panics never`", name),
            ));
        }
        None
    }

    /// 呼び出しの明示型引数 (`f::<..>`) で効果変数・panic 変数を実体化してから検査する (4.3)。
    /// 呼び出しごとに独立なので、1 つ目の実体化が 2 つ目に漏れることはない。
    /// 実体化できない (実引数がない・名前が並ばない) ときは、実体化せずに宣言のまま検査する
    /// (効果変数は `covers` が黙って通す = 誤検出を出さない)。
    fn violation_targs(&self, callee: &Def, targs: &[V]) -> Option<(String, String)> {
        match inst_def_effects(callee, targs) {
            Some((uses, panics)) => self.violation_effs(&callee.name, &uses, panics.as_deref()),
            None => self.violation(callee),
        }
    }

    fn effects(&mut self, callee: &Def, di: usize, line: usize, free: bool, targs: &[V]) {
        if free || self.cur_uses.is_none() {
            return;
        }
        if let Some((c, m)) = self.violation_targs(callee, targs) {
            self.diag(&c, m, di, line);
        }
    }

    fn effects_all(&mut self, cands: &[&Def], di: usize, line: usize, free: bool, targs: &[V]) {
        if free || self.cur_uses.is_none() {
            return;
        }
        let bad: Vec<Option<(String, String)>> = cands.iter().map(|c| self.violation_targs(c, targs)).collect();
        if bad.iter().all(|b| b.is_some()) {
            let (c, m) = bad[0].clone().unwrap();
            self.diag(&c, m, di, line);
        }
    }

    // ================================================================ TypeCk
    fn typeck(&mut self) {
        let user = self.user.clone();
        for &i in &user {
            if self.def(i).kw == "fn" {
                self.check_elision(i);
            }
        }
        for &i in &user {
            let d = self.def(i);
            if d.kw == "fn" && d.body.is_some() {
                self.tc_fn(i);
            }
        }
        // 型付きの式推論に基づく検査 (レシーバの型によるメソッドの存在・引数の個数と型)。
        let mut tk = crate::types::TypeCk::new(self.p);
        for &i in &user {
            let d = self.def(i);
            if (d.kw == "fn" && d.body.is_some()) || d.kw == "test" {
                tk.check_def(i);
            }
        }
        self.diags.extend(tk.diags);
        // 型で解決できた呼び出し (メソッドを含む) の辺を、再帰・スタックの検査に加える。
        for (from, tos) in tk.edges {
            self.graph.entry(from).or_default().extend(tos);
        }
        // 従来の検査と新しい型推論が同じ誤りを同じ文面で報告したときは、1 件にまとめる。
        let mut seen = HashSet::new();
        self.diags.retain(|d| seen.insert((d.file.clone(), d.code.clone(), d.fqn.clone(), d.line, d.msg.clone())));
        self.check_recursion();
    }

    fn check_elision(&mut self, di: usize) {
        let d = self.def(di);
        let head = d.header.split(" uses ").next().unwrap_or("");
        if head.contains("['") {
            // 4.1: 省略が正規形のときのライフタイム宣言はエラー。
            let ret_txt = head.split("->").nth(1).unwrap_or("");
            let refs = d.params.iter().filter(|p| p.1.dump().contains("ref")).count();
            let redundant = if !ret_txt.contains('\'') { true } else { d.ret.dump().contains("ref") && refs == 1 };
            if redundant {
                let msg = format!("{}: lifetimes can be elided here; remove the explicit declaration", d.name);
                self.diag("E3021", msg, di, 0);
            }
            return;
        }
        if head.contains('\'') {
            return;
        }
        if !d.ret.dump().contains("ref") {
            return;
        }
        let refs = d.params.iter().filter(|p| p.1.dump().contains("ref")).count();
        let selfref = d.params.first().map_or(false, |p| p.0 == "self" && tag(&p.1) == "ref");
        if !selfref && refs != 1 {
            self.diag(
                "E3020",
                format!("{}: lifetime of the returned reference cannot be elided; declare lifetimes", d.name),
                di,
                0,
            );
        }
    }

    fn strip<'b>(&self, mut t: &'b V) -> &'b V {
        while tag(t) == "ref" {
            t = tel(t, 2);
        }
        t
    }

    fn is_generic(&self, t: &V, names: &HashSet<String>) -> bool {
        if is_nil(t) {
            return true;
        }
        match tag(t).as_str() {
            "named" => {
                names.contains(&st(tel(t, 1)))
                    || items(tel(t, 2)).iter().any(|a| matches!(a, V::L(_)) && self.is_generic(a, names))
            }
            "self" | "dyn" | "fn" | "never" => true,
            "ref" | "ptr" => self.is_generic(tel(t, 2), names),
            "slice" | "array" => self.is_generic(tel(t, 1), names),
            "tuple" => items(tel(t, 1)).iter().any(|x| self.is_generic(x, names)),
            _ => false,
        }
    }

    fn is_copy(&self, t: &V) -> bool {
        if is_nil(t) {
            return true;
        }
        match tag(t).as_str() {
            "ref" => !matches!(tel(t, 1), V::B(true)),
            "prim" | "never" => true,
            "tuple" => items(tel(t, 1)).iter().all(|x| self.is_copy(x)),
            "named" => {
                let nm = st(tel(t, 1));
                if nm == "Option" || nm == "Result" {
                    return items(tel(t, 2)).iter().all(|a| self.is_copy(a));
                }
                self.p.types.get(&nm).map_or(false, |&i| self.def(i).derive.iter().any(|x| x == "Copy"))
            }
            _ => true,
        }
    }

    /// 整数の算術演算子はオーバーフローで panic する。`panics never` の関数では使えない (4.1)。
    fn int_op_may_panic(&mut self, what: &str, line: usize) {
        if self.def(self.d).panics.as_deref() == Some("never") {
            let name = match what {
                s if s.contains("`+`") => "add",
                s if s.contains("`-`") && s.starts_with("unary") => "neg",
                s if s.contains("`-`") => "sub",
                s if s.contains("`*`") => "mul",
                s if s.contains("`/`") => "div",
                _ => "rem",
            };
            let how = if name == "neg" {
                "write `x.checked_neg()` (returns Option) or `x.wrapping_neg()`".to_string()
            } else {
                format!(
                    "write `a.checked_{n}(b)` (returns Option: use `match` to handle None) or `a.wrapping_{n}(b)` / `a.saturating_{n}(b)` (return the integer)",
                    n = name
                )
            };
            let msg = format!("integer {} may panic on overflow; {}", what, how);
            self.tdiag("E4004", msg, line);
        }
    }

    fn is_int_ty(&self, t: &V) -> bool {
        let s = self.strip(t);
        tag(s) == "prim" && INT_PRIMS.contains(&st(tel(s, 1)).as_str())
    }

    /// 4.6: `()` と `!` 以外の値を捨てる式文はエラー。
    fn discard_check(&mut self, stmts: &[V], i: usize, env: &Env) {
        let s = &stmts[i];
        if i + 1 == stmts.len() || nname(s) != "ExprStmt" {
            return;
        }
        let ty = self.ty_of(&nf(s)[0], env);
        if is_nil(&ty) {
            return;
        }
        let is_unit = tag(&ty) == "tuple" && items(tel(&ty, 1)).is_empty();
        if !is_unit && tag(&ty) != "never" {
            self.tdiag("E2042", "a value other than () is discarded; use core.mem.drop(x)".into(), nline(s));
        }
    }

    fn is_linear(&self, t: &V) -> bool {
        tag(t) == "named" && self.p.types.get(&st(tel(t, 1))).map_or(false, |&i| self.def(i).linear)
    }

    fn tdiag(&mut self, code: &str, msg: String, line: usize) {
        let d = self.d;
        self.diag(code, msg, d, line);
    }

    fn mismatch(&mut self, want: &V, got: &V, line: usize, what: &str) {
        if is_nil(want) || is_nil(got) {
            return;
        }
        let names = self.names.clone();
        if self.is_generic(want, &names) || self.is_generic(got, &names) {
            return;
        }
        let w = self.strip(want).clone();
        let g = self.strip(got).clone();
        let (wt, gt) = (tag(&w), tag(&g));
        if wt == "never" || gt == "never" {
            return;
        }
        let mut msg = format!("type mismatch in {}: expected {}, found {}", what, st(tel(&w, 1)), st(tel(&g, 1)));
        if wt == "prim" && gt == "prim" {
            msg += &crate::types::conv_hint(&st(tel(&w, 1)), &st(tel(&g, 1)));
        }
        if wt == "prim" && gt == "prim" && st(tel(&w, 1)) != st(tel(&g, 1)) {
            self.tdiag("E2020", msg, line);
        } else if wt == "named" && gt == "named" && st(tel(&w, 1)) != st(tel(&g, 1)) {
            let (a, b) = (st(tel(&w, 1)), st(tel(&g, 1)));
            if a != "Option" && a != "Result" && b != "Option" && b != "Result" {
                self.tdiag("E2020", msg, line);
            }
        } else if wt == "prim" && gt == "named" {
            self.tdiag("E2020", msg, line);
        }
    }

    fn ty_of(&self, e: &V, env: &Env) -> V {
        let f = nf(e);
        match nname(e) {
            "Lit" => match st(&f[0]).as_str() {
                "int" => {
                    if is_nil(&f[2]) { V::Nil } else { V::L(vec![V::S("prim".into()), f[2].clone()]) }
                }
                "float" => {
                    if is_nil(&f[1]) { V::Nil } else { V::L(vec![V::S("prim".into()), f[1].clone()]) }
                }
                "bool" => V::L(vec![V::S("prim".into()), V::S("bool".into())]),
                "char" => V::L(vec![V::S("prim".into()), V::S("char".into())]),
                "str" => V::L(vec![
                    V::S("ref".into()),
                    V::B(false),
                    V::L(vec![V::S("prim".into()), V::S("str".into())]),
                ]),
                _ => V::Nil,
            },
            "Path" => {
                let segs = items(&f[0]);
                if segs.len() == 1 { env.get(&st(&segs[0])).cloned().unwrap_or(V::Nil) } else { V::Nil }
            }
            "StructLit" => {
                let name = st(&f[0]);
                let segs: Vec<String> = name.split('.').map(String::from).collect();
                let fq = self.p.expand(&segs, &self.def(self.d).file).join(".");
                if self.p.types.contains_key(&fq) {
                    V::L(vec![V::S("named".into()), V::S(fq), V::L(vec![])])
                } else {
                    V::Nil
                }
            }
            "Bin" => {
                if matches!(st(&f[0]).as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=" | "&&" | "||") {
                    V::L(vec![V::S("prim".into()), V::S("bool".into())])
                } else {
                    self.ty_of(&f[1], env)
                }
            }
            "Unary" => {
                let op = st(&f[0]);
                match op.as_str() {
                    "!" => self.ty_of(&f[1], env),
                    "&" | "&mut" => {
                        let inner = self.ty_of(&f[1], env);
                        if is_nil(&inner) { V::Nil } else { V::L(vec![V::S("ref".into()), V::B(op == "&mut"), inner]) }
                    }
                    "*" => {
                        let x = self.ty_of(&f[1], env);
                        if tag(&x) == "ref" { tel(&x, 2).clone() } else { V::Nil }
                    }
                    _ => V::Nil,
                }
            }
            "Call" => {
                let fnn = &f[0];
                if nname(fnn) == "Path" {
                    let segs = strs(&nf(fnn)[0]);
                    if !env.contains_key(&segs[0]) {
                        if let Some(r) = self.p.resolve(&segs, &self.def(self.d).file) {
                            if let Target::Variant(en, _) = &r.target {
                                if self.p.types.contains_key(en) {
                                    return V::L(vec![V::S("named".into()), V::S(en.clone()), V::L(vec![])]);
                                }
                            }
                            if let Target::Fn(i) = r.target {
                                if r.rest.is_empty() {
                                    let callee = self.def(i);
                                    if !self.is_generic(&callee.ret, &callee.gnames()) {
                                        return callee.ret.clone();
                                    }
                                }
                            }
                        }
                    }
                }
                V::Nil
            }
            _ => V::Nil,
        }
    }

    fn tc_fn(&mut self, di: usize) {
        let d = self.def(di);
        self.d = di;
        self.names = d.gnames();
        let mut env: Env = d.params.iter().cloned().collect();
        self.graph.entry(d.name.clone()).or_default();
        self.moved.clear();
        let body = d.body.as_ref().unwrap();
        self.tc_block(body, &env, &BTreeSet::new());
        self.check_extern_calls(di);
        // NLL の借用検査 (制御フローと、借用を持つ変数の生存に基づく)
        self.borrowck(di);
        env = self.final_env.clone();
        let stmts = items(&nf(body)[0]);
        if let Some(last) = stmts.last() {
            if nname(last) == "ExprStmt" {
                let names = self.names.clone();
                if !is_nil(&d.ret) && !self.is_generic(&d.ret, &names) {
                    let got = self.ty_of(&nf(last)[0], &env);
                    self.mismatch(&d.ret, &got, nline(last), "return value");
                }
            }
        }
    }

    /// `extern` の関数は `unsafe` ブロックの中でしか呼べない (3.8, E5003)。
    fn check_extern_calls(&mut self, di: usize) {
        let d = self.def(di);
        let Some(body) = &d.body else { return };
        fn scan<'b>(ck: &Checker, v: &'b V, in_unsafe: bool, file: &str, out: &mut Vec<(usize, String)>) {
            if nname(v) == "Call" && nname(&nf(v)[0]) == "Path" && !in_unsafe {
                let segs = strs(&nf(&nf(v)[0])[0]);
                if let Some(r) = ck.p.resolve(&segs, file) {
                    if let Target::Sig(i) = r.target {
                        if r.rest.is_empty() && ck.p.defs[i].kw == "extern" {
                            out.push((nline(v), ck.p.defs[i].name.clone()));
                        }
                    }
                }
            }
            let iu = in_unsafe || nname(v) == "Unsafe";
            for c in children(v) {
                scan(ck, c, iu, file, out);
            }
        }
        let mut found = vec![];
        scan(self, body, false, &d.file, &mut found);
        for (line, name) in found {
            self.diag(
                "E5003",
                format!("the external function `{}` can only be called inside an `unsafe` block", name),
                di,
                line,
            );
        }
    }

    fn tc_block(&mut self, b: &V, env: &Env, muts: &BTreeSet<String>) {
        let mut env = env.clone();
        let mut muts = muts.clone();
        for (i, s) in items(&nf(b)[0]).iter().enumerate() {
            self.tc_stmt(s, &mut env, &mut muts);
            self.discard_check(items(&nf(b)[0]), i, &env);
        }
        self.final_env = env;
    }

    fn tc_stmt(&mut self, s: &V, env: &mut Env, muts: &mut BTreeSet<String>) {
        let f = nf(s);
        match nname(s) {
            "Let" => {
                self.tc_expr(&f[3], env, muts, true);
                let got = self.ty_of(&f[3], env);
                if !is_nil(&f[2]) {
                    self.mismatch(&f[2], &got, nline(s), "let binding");
                }
                let ty = if !is_nil(&f[2]) { f[2].clone() } else { got };
                let mut binds = vec![];
                pat_binds(&f[0], &mut binds);
                for n in binds {
                    env.insert(n.clone(), if nname(&f[0]) == "PBind" { ty.clone() } else { V::Nil });
                    self.revive_place(std::slice::from_ref(&n));
                    if matches!(f[1], V::B(true)) {
                        muts.insert(n);
                    } else {
                        muts.remove(&n);
                    }
                }
                if nname(&f[0]) == "PBind" {
                    self.note_linear(&st(&nf(&f[0])[0]), &ty, nline(s));
                }
            }
            "Assign" => {
                self.tc_expr(&f[1], env, muts, true);
                let tg = &f[0];
                if nname(tg) == "Path" {
                    let segs = strs(&nf(tg)[0]);
                    if env.contains_key(&segs[0]) {
                        let root = &segs[0];
                        let rt = env.get(root).cloned().unwrap_or(V::Nil);
                        if !muts.contains(root) && root != "self" && !(!is_nil(&rt) && tag(&rt) == "ref") {
                            self.tdiag(
                                "E3001",
                                format!("cannot assign to `{}`: `{}` is not declared `mut`", segs.join("."), root),
                                nline(s),
                            );
                        }
                        // 代入で、その場所とその下の move 済みの部分は復活する (4.1)。
                        self.revive_place(&segs);
                        if segs.len() == 1 {
                            let got = self.ty_of(&f[1], env);
                            self.mismatch(&rt, &got, nline(s), "assignment");
                        }
                    } else {
                        // 3.1: `static` / `const` は不変で、代入できない。
                        let ex = self.p.expand(&segs, &self.def(self.d).file);
                        let is_const = (1..=ex.len()).rev().any(|k| self.p.consts.contains_key(&ex[..k].join(".")));
                        if is_const {
                            self.tdiag("E3005", format!("cannot assign to `{}`: static/const is immutable (3.1)", ex.join(".")), nline(s));
                        }
                    }
                }
            }
            _ => self.tc_expr(&f[0], env, muts, false),
        }
    }

    /// 場所 (根の変数 + フィールド道) の使用を検査する (4.1。部分 move を含む)。
    /// move 済みの場所と重なる (一方が他方の接頭辞) ときは E3002。
    /// 重ならなければ、consume のときに型を解決して move を記録する (Copy は move にならない)。
    fn use_place(&mut self, root: &str, path: &[String], env: &Env, line: usize, consume: bool) {
        if let Some((mk, ml)) = self.moved_overlap(root, path) {
            let key = place_key(root, path);
            let msg = if mk == key {
                format!("use of moved value `{key}` (moved at line {ml})")
            } else if mk.len() < key.len() {
                // 使用した場所の先祖 (全体) が move 済み
                format!("use of moved value `{key}` (moved `{mk}` at line {ml})")
            } else {
                // 使用した場所の一部 (下の道) が move 済み
                format!("use of partially moved value `{key}` (moved `{mk}` at line {ml})")
            };
            self.tdiag("E3002", msg, line);
            return;
        }
        if consume {
            self.record_move(root, path, env, line);
        }
    }

    /// 使用した場所と重なる move 済みの場所を探す。moved の鍵は互いに重ならないので、
    /// 見つかれば高々 1 つ (鍵の順で最初のもの。並び順は安定)。
    fn moved_overlap(&self, root: &str, path: &[String]) -> Option<(String, usize)> {
        let key = place_key(root, path);
        self.moved.iter().find(|(k, _)| place_overlaps(k, &key)).map(|(k, &ml)| (k.clone(), ml))
    }

    /// 消費 (consume) の対象の場所を move 済みとして記録する。道の型を解決できて
    /// Copy でなく参照越しでないときだけ記録する (型が分からない所では黙る = 記録しない)。
    fn record_move(&mut self, root: &str, path: &[String], env: &Env, line: usize) {
        let Some(ty) = self.place_ty(root, path, env) else { return };
        if self.is_copy(&ty) {
            return;
        }
        self.moved.insert(place_key(root, path), line);
    }

    /// 場所 (根の変数 + フィールド道) の型を env の根の型から解決する。
    /// 分からない (型未決定・未解決の道・参照越し) ときは None (黙る)。
    fn place_ty(&self, root: &str, path: &[String], env: &Env) -> Option<V> {
        let t = env.get(root)?;
        if is_nil(t) || tag(t) == "ref" {
            return None;
        }
        let mut cur = t.clone();
        for seg in path {
            cur = self.field_ty(&cur, seg)?;
            if tag(&cur) == "ref" {
                return None;
            }
        }
        Some(cur)
    }

    /// 構造体 (named) のフィールドの型。ジェネリクス型の型パラメータは使用側の型引数で置き換える。
    /// 構造体でない型 (enum・Option など)・未解決の道は None (黙る)。
    fn field_ty(&self, t: &V, field: &str) -> Option<V> {
        if tag(t) != "named" {
            return None;
        }
        let nm = st(tel(t, 1));
        let &ti = self.p.types.get(&nm)?;
        let d = self.def(ti);
        if d.kw != "type" {
            return None;
        }
        let ty = &d.fields.iter().find(|(n, _, _)| n == field)?.1;
        let tparams: Vec<String> = d.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.clone()).collect();
        if tparams.iter().any(|p| ty_mentions(ty, p)) {
            // 型パラメータを含むときは、使用側の型引数の並びと合うときだけ置き換える。
            // 合わなければ (効果などの引数が混ざるときは) 黙る。
            let args = items(tel(t, 2));
            if tparams.len() != args.len() {
                return None;
            }
            let bind: HashMap<String, V> = tparams.into_iter().zip(args.iter().cloned()).collect();
            return Some(subst_ty_params(ty, &bind));
        }
        Some(ty.clone())
    }

    /// 代入・再束縛の対象の場所とその下の move 済みの記録を消す (再代入で復活、4.1)。
    /// `s = ..` は `s`・`s.f`・`s.f.g` を、`s.f = ..` は `s.f`・`s.f.g` を消す (兄弟は残す)。
    fn revive_place(&mut self, segs: &[String]) {
        let key = place_key(&segs[0], &segs[1..]);
        let dead: Vec<String> = self
            .moved
            .keys()
            .filter(|k| **k == key || k.starts_with(&format!("{key}.")))
            .cloned()
            .collect();
        for k in dead {
            self.moved.remove(&k);
        }
    }

    fn tc_expr(&mut self, e: &V, env: &Env, muts: &BTreeSet<String>, consume: bool) {
        let f = nf(e);
        match nname(e) {
            "Path" => {
                let segs = strs(&f[0]);
                if env.contains_key(&segs[0]) {
                    self.use_place(&segs[0], &segs[1..], env, nline(e), consume);
                }
            }
            "Block" => {
                let mut inner = env.clone();
                let mut im = muts.clone();
                for (i, s) in items(&f[0]).iter().enumerate() {
                    self.tc_stmt(s, &mut inner, &mut im);
                    self.discard_check(items(&f[0]), i, &inner);
                }
            }
            "If" | "Match" | "Loop" | "While" | "For" => self.branchy(e, env, muts),
            "Closure" => {
                let mut sub = env.clone();
                for p in items(&f[0]) {
                    sub.insert(st(&items(p)[0]), items(p)[1].clone());
                }
                let saved = self.moved.clone();
                let fe = self.final_env.clone();
                self.tc_block(&f[3], &sub, muts);
                self.final_env = fe;
                self.moved = saved;
            }
            "Call" => {
                let mut callee: Option<&Def> = None;
                let fnn = &f[0];
                if nname(fnn) == "Path" {
                    let segs = strs(&nf(fnn)[0]);
                    if !env.contains_key(&segs[0]) {
                        if let Some(r) = self.p.resolve(&segs, &self.def(self.d).file) {
                            if let Target::Fn(i) = r.target {
                                if r.rest.is_empty() {
                                    callee = Some(self.def(i));
                                    let cn = self.def(i).name.clone();
                                    let cur = self.def(self.d).name.clone();
                                    self.graph.entry(cur).or_default().insert(cn);
                                }
                            }
                        }
                    } else {
                        // 呼び出し先が局所変数のとき。`v.push(x)` のようなメソッド呼び出し
                        // (segs の最後はメソッド名) は、レシーバの場所を使うだけで move はしない (4.2)。
                        let recv: &[String] = if segs.len() >= 2 { &segs[1..segs.len() - 1] } else { &[] };
                        self.use_place(&segs[0], recv, env, nline(e), false);
                    }
                } else {
                    self.tc_expr(fnn, env, muts, false);
                }
                for (i, a) in items(&f[1]).iter().enumerate() {
                    self.tc_expr(a, env, muts, true);
                    if let Some(c) = callee {
                        if i < c.params.len() {
                            let pt = &c.params[i].1;
                            if !self.is_generic(pt, &c.gnames()) {
                                let got = self.ty_of(a, env);
                                self.mismatch(pt, &got, nline(e), &format!("argument {} of {}", i + 1, c.name));
                            }
                        }
                    }
                }
                if let Some(c) = callee {
                    self.check_bounds(c, items(&f[1]), env, nline(e));
                }
            }
            "MCall" => {
                self.tc_expr(&f[0], env, muts, false);
                for a in items(&f[2]) {
                    self.tc_expr(a, env, muts, true);
                }
            }
            "Unary" if matches!(st(&f[0]).as_str(), "&" | "&mut") => self.tc_expr(&f[1], env, muts, false),
            "Unary" if st(&f[0]) == "-" => {
                if self.is_int_ty(&self.ty_of(&f[1], env)) {
                    self.int_op_may_panic("unary `-`", nline(e));
                }
                self.tc_expr(&f[1], env, muts, false);
            }
            "Bin" if matches!(st(&f[0]).as_str(), "+" | "-" | "*" | "/" | "%") => {
                if self.is_int_ty(&self.ty_of(&f[1], env)) || self.is_int_ty(&self.ty_of(&f[2], env)) {
                    let what = format!("operator `{}`", st(&f[0]));
                    self.int_op_may_panic(&what, nline(e));
                }
                self.tc_expr(&f[1], env, muts, false);
                self.tc_expr(&f[2], env, muts, false);
            }
            "StructLit" => {
                for x in items(&f[1]) {
                    self.tc_expr(&items(x)[1], env, muts, true);
                }
            }
            "TupleE" | "ArrayE" => {
                for x in items(&f[0]) {
                    self.tc_expr(x, env, muts, true);
                }
            }
            "Return" => {
                if !is_nil(&f[0]) {
                    self.tc_expr(&f[0], env, muts, true);
                }
            }
            _ => {
                for c in children(e) {
                    if !is_pat(c) {
                        self.tc_expr(c, env, muts, false);
                    }
                }
            }
        }
    }

    fn branchy(&mut self, e: &V, env: &Env, muts: &BTreeSet<String>) {
        let f = nf(e);
        let saved = self.moved.clone();
        match nname(e) {
            "If" => {
                self.tc_expr(&f[0], env, muts, false);
                self.tc_expr(&f[1], env, muts, false);
                let a = self.moved.clone();
                self.moved = saved.clone();
                if !is_nil(&f[2]) {
                    self.tc_expr(&f[2], env, muts, false);
                }
                let mut m = saved.clone();
                for (k, v) in a {
                    if saved.contains_key(&k) {
                        m.insert(k, v);
                    }
                }
                self.moved = m;
            }
            "Match" => {
                self.tc_expr(&f[0], env, muts, false);
                let base = self.moved.clone();
                for arm in items(&f[1]) {
                    self.moved = base.clone();
                    let mut inner = env.clone();
                    let mut b = vec![];
                    pat_binds(&items(arm)[0], &mut b);
                    for n in b {
                        inner.insert(n, V::Nil);
                    }
                    self.tc_expr(&items(arm)[1], &inner, muts, false);
                }
                self.moved = base;
                self.exhaustive(e, env);
            }
            "For" => {
                self.tc_expr(&f[1], env, muts, true);
                let mut inner = env.clone();
                let mut b = vec![];
                pat_binds(&f[0], &mut b);
                for n in b {
                    inner.insert(n, V::Nil);
                }
                self.tc_expr(&f[2], &inner, muts, false);
                self.moved = saved;
            }
            n => {
                let body = if n == "While" {
                    self.tc_expr(&f[0], env, muts, false);
                    &f[1]
                } else {
                    &f[0]
                };
                self.tc_expr(body, env, muts, false);
                self.moved = saved;
            }
        }
    }

    fn exhaustive(&mut self, m: &V, env: &Env) {
        let f = nf(m);
        let pats: Vec<&V> = items(&f[1]).iter().map(|a| &items(a)[0]).collect();
        if pats.iter().any(|p| matches!(nname(p), "PWild" | "PBind")) {
            let got = self.ty_of(&f[0], env);
            let s = self.strip(&got).clone();
            let by_arm = pats.iter().any(|p| match nname(p) {
                "PVariant" | "PPath" => {
                    let segs = strs(&nf(p)[0]);
                    (segs.len() == 1 && matches!(segs[0].as_str(), "None" | "Some" | "Ok" | "Err"))
                        || matches!(
                            self.p.resolve(&segs, &self.def(self.d).file),
                            Some(Resolved { target: Target::Variant(..), .. })
                        )
                }
                "PLit" => st(&nf(&nf(p)[0])[0]) == "bool",
                _ => false,
            });
            if by_arm {
                self.tdiag("E2021", "`_` / binding patterns cannot match an enum or bool; list all cases".into(), nline(m));
                return;
            }
            if tag(&s) == "named" {
                if let Some(&ti) = self.p.types.get(&st(tel(&s, 1))) {
                    if self.def(ti).kw == "enum" {
                        self.tdiag(
                            "E2021",
                            "`_` / binding patterns cannot match an enum; list all variants".into(),
                            nline(m),
                        );
                    }
                }
            }
            return;
        }
        let mut names: Vec<(String, String)> = vec![];
        for p in &pats {
            match nname(p) {
                "PVariant" | "PPath" => {
                    let segs = strs(&nf(p)[0]);
                    if let Some(r) = self.p.resolve(&segs, &self.def(self.d).file) {
                        if let Target::Variant(en, vn) = r.target {
                            let key = (en, vn);
                            if !names.contains(&key) {
                                names.push(key);
                            }
                        }
                    }
                }
                "PLit" => {
                    let lit = &nf(p)[0];
                    if st(&nf(lit)[0]) == "bool" {
                        let key = ("bool".to_string(), if matches!(nf(lit)[1], V::B(true)) { "True" } else { "False" }.to_string());
                        if !names.contains(&key) {
                            names.push(key);
                        }
                    }
                }
                _ => {}
            }
        }
        if names.is_empty() {
            return;
        }
        let en = names[0].0.clone();
        let want: Vec<String> = match en.as_str() {
            "Option" => vec!["Some".into(), "None".into()],
            "Result" => vec!["Ok".into(), "Err".into()],
            "bool" => vec!["True".into(), "False".into()],
            _ => match self.p.types.get(&en) {
                Some(&ti) => self.def(ti).variants.iter().map(|v| v.0.clone()).collect(),
                None => return,
            },
        };
        let have: HashSet<&String> = names.iter().filter(|x| x.0 == en).map(|x| &x.1).collect();
        let mut missing: Vec<String> = want.iter().filter(|w| !have.contains(w)).cloned().collect();
        if !missing.is_empty() {
            missing.sort();
            let list = missing.iter().map(|x| format!("'{}'", x)).collect::<Vec<_>>().join(", ");
            self.tdiag("E2021", format!("non-exhaustive match: missing [{}]", list), nline(m));
        }
    }

    fn note_linear(&mut self, name: &str, ty: &V, line: usize) {
        if !self.is_linear(ty) {
            return;
        }
        let body = self.def(self.d).body.as_ref().unwrap();
        let mut all = vec![];
        walk(body, &mut all);
        if !all.iter().any(|x| nname(x) == "Path" && st(&items(&nf(x)[0])[0]) == name) {
            self.tdiag("E3003", format!("linear value `{}` is never consumed", name), line);
        }
    }

    fn implements(&self, ty: &str, tr: &str) -> bool {
        let Some(&i) = self.p.types.get(ty) else { return true };
        if self.p.impls.contains(&(tr.to_string(), ty.to_string())) {
            return true;
        }
        self.def(i).derive.iter().any(|x| DERIVE_TRAITS.iter().any(|(k, v)| k == x && *v == tr))
    }

    fn check_bounds(&mut self, callee: &Def, args: &[V], env: &Env, line: usize) {
        let mut bounds: HashMap<String, Vec<V>> = HashMap::new();
        for g in &callee.generics {
            if g.0 == "type" && !items(&g.2).is_empty() {
                bounds.insert(g.1.clone(), items(&g.2).to_vec());
            }
        }
        for ((_, pt), a) in callee.params.iter().zip(args) {
            let base = self.strip(pt);
            if tag(base) == "named" && bounds.contains_key(&st(tel(base, 1))) {
                let got = self.ty_of(a, env);
                let at = self.strip(&got).clone();
                if tag(&at) != "named" {
                    continue;
                }
                let bname = st(tel(base, 1));
                for b in &bounds[&bname] {
                    let V::L(bx) = b else { continue };
                    let tr = st(&bx[0]);
                    let known = self.p.types.get(&tr).map_or(false, |&i| self.def(i).kw == "trait");
                    let derive_tr = DERIVE_TRAITS.iter().any(|(_, v)| *v == tr);
                    if (known || derive_tr)
                        && tr != "core.marker.Send"
                        && tr != "core.marker.Sync"
                        && !tr.starts_with("core.ops.Fn")
                        && !self.implements(&st(tel(&at, 1)), &tr)
                    {
                        self.tdiag(
                            "E2030",
                            format!(
                                "the trait bound `{}: {}` of {} is not satisfied by {}",
                                bname,
                                tr,
                                callee.name,
                                st(tel(&at, 1))
                            ),
                            line,
                        );
                    }
                }
            }
        }
    }

    fn check_recursion(&mut self) {
        let user = self.user.clone();
        let fns: Vec<usize> = user
            .iter()
            .copied()
            .filter(|&i| self.def(i).kw == "fn" && self.def(i).body.is_some())
            .collect();
        let mut by_name: HashMap<String, usize> = HashMap::new();
        for &i in &fns {
            by_name.insert(self.def(i).name.clone(), i);
        }
        for &i in &fns {
            let d = self.def(i);
            let n = d.name.clone();
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut stk: Vec<String> = self.graph.get(&n).map(|s| s.iter().cloned().collect()).unwrap_or_default();
            while let Some(x) = stk.pop() {
                if seen.insert(x.clone()) {
                    if let Some(s) = self.graph.get(&x) {
                        stk.extend(s.iter().cloned());
                    }
                }
            }
            let rec = seen.contains(&n);
            let never = d.panics.as_deref() == Some("never");
            self.d = i;
            if rec && never && d.stack.is_none() {
                self.tdiag("E4010", format!("recursive `panics never` function {} requires `stack <bytes>`", n), 0);
            }
            if d.stack.is_some() && !(rec && never) {
                self.tdiag("E4011", format!("`stack` is only allowed on recursive `panics never` functions ({})", n), 0);
            }
            if let Some(declared) = d.stack {
                let mut memo: HashMap<String, u64> = HashMap::new();
                let need = self.go(&n, &BTreeSet::new(), &by_name, &mut memo);
                if need > declared {
                    self.tdiag("E4012", format!("{}: estimated stack use {} exceeds declared {}", n, need, declared), 0);
                }
            }
        }
    }

    /// 型 `t` の大きさ (バイト) の見積り。再帰する型や未知の型は 1 語から数語とする。
    fn ty_size(&self, t: &V, depth: u32) -> u64 {
        if is_nil(t) || depth > 6 {
            return 8;
        }
        match tag(t).as_str() {
            "prim" => match st(tel(t, 1)).as_str() {
                "bool" | "i8" | "u8" => 1,
                "i16" | "u16" => 2,
                "i32" | "u32" | "f32" | "char" => 4,
                "i128" | "u128" | "str" => 16,
                _ => 8,
            },
            "ref" => match tag(tel(t, 2)).as_str() {
                "slice" => 16,
                "prim" if st(tel(tel(t, 2), 1)) == "str" => 16,
                _ => 8,
            },
            "tuple" => items(tel(t, 1)).iter().map(|x| round8(self.ty_size(x, depth + 1))).sum(),
            "slice" => 16,
            "array" => {
                let n = tel(t, 2);
                let len = if nname(n) == "Int" { st(&nf(n)[1]).replace('_', "").parse::<u64>().unwrap_or(16) } else { 16 };
                len.saturating_mul(self.ty_size(tel(t, 1), depth + 1).max(1))
            }
            "named" => {
                let nm = st(tel(t, 1));
                let args: Vec<&V> = items(tel(t, 2)).iter().filter(|a| matches!(a, V::L(_))).collect();
                match nm.as_str() {
                    "Option" | "Result" => 8 + args.iter().map(|a| self.ty_size(a, depth + 1)).max().unwrap_or(0),
                    "alloc.vec.Vec" | "alloc.string.String" => 24,
                    "alloc.boxed.Box" => 8,
                    _ => match self.p.types.get(&nm) {
                        Some(&i) => {
                            let td = self.def(i);
                            if td.kw == "enum" {
                                8 + td.variants.iter().map(|v| v.1.iter().map(|x| round8(self.ty_size(x, depth + 1))).sum::<u64>()).max().unwrap_or(0)
                            } else if td.kw == "type" {
                                td.fields.iter().map(|f| round8(self.ty_size(&f.1, depth + 1))).sum()
                            } else {
                                8
                            }
                        }
                        None => 16,
                    },
                }
            }
            _ => 8,
        }
    }

    fn go(&self, n: &str, path: &BTreeSet<String>, names: &HashMap<String, usize>, memo: &mut HashMap<String, u64>) -> u64 {
        if path.contains(n) {
            return 0;
        }
        if let Some(&v) = memo.get(n) {
            return v;
        }
        let frame = match names.get(n) {
            None => 32,
            Some(&i) => {
                let d = self.def(i);
                let mut all = vec![];
                walk(d.body.as_ref().unwrap(), &mut all);
                // 戻りアドレスと保存したフレームポインタ (16) + 引数と局所変数の大きさ + 一時値の余裕 (16)。
                // 型の注釈がない局所変数は 1 語 (8) とみなす。
                let mut bytes: u64 = 16 + 16;
                for p in &d.params {
                    bytes += round8(self.ty_size(&p.1, 0));
                }
                for l in all.iter().filter(|x| nname(x) == "Let") {
                    let ann = &nf(l)[2];
                    bytes += if is_nil(ann) { 8 } else { round8(self.ty_size(ann, 0)) };
                }
                bytes
            }
        };
        let mut np = path.clone();
        np.insert(n.to_string());
        let callees: Vec<String> = self.graph.get(n).map(|s| s.iter().cloned().collect()).unwrap_or_default();
        let m = callees.iter().map(|c| self.go(c, &np, names, memo)).max().unwrap_or(0);
        let r = frame + m;
        memo.insert(n.to_string(), r);
        r
    }
}

fn round8(n: u64) -> u64 {
    (n + 7) / 8 * 8
}

pub fn format_diag(d: &Diag) -> String {
    format!("{}\t{}\t{}\t{}", d.code, d.fqn, d.line, d.msg)
}

/// 型 `t` が名前 `name` の型を (参照・引数・タプル・配列を通して) 含むか。
/// `Some(false)` を優先する 3 値の論理積。
fn and3(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

/// `Some(true)` を優先する 3 値の論理和。
fn or3(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

/// トレイトのレシーバ型が `Self` / `&Self` / `&mut Self` か (3.2)。
/// `Self` はパーサではタグ `"self"` になる。
fn is_self_ty(t: &V) -> bool {
    match tag(t).as_str() {
        "self" => true,
        "ref" => tag(tel(t, 2)) == "self",
        _ => false,
    }
}

/// トレイト名 (`dyn Tr` の `Tr`、`requires` のスーパートレイト名など) を trait 定義の FQN に解決する。
/// 曖昧・未定義なら None (黙る)。
pub(crate) fn resolve_trait_fqn(p: &Program, nm: &str) -> Option<String> {
    if let Some(&i) = p.types.get(nm) {
        let d = &p.defs[i];
        return if d.kw == "trait" { Some(d.name.clone()) } else { None };
    }
    if !nm.contains('.') {
        let mut found: Option<String> = None;
        for d in &p.defs {
            if d.kw == "trait" && (d.name == nm || d.name.ends_with(&format!(".{}", nm))) {
                if found.is_some() {
                    return None; // 曖昧なので黙る。
                }
                found = Some(d.name.clone());
            }
        }
        return found;
    }
    None
}

/// `impl .. for X` の対象型の基底名 (型引数を落とす)。
fn impl_target(v: &V) -> String {
    match tag(v).as_str() {
        "named" | "prim" => st(tel(v, 1)),
        "ref" | "ptr" => impl_target(tel(v, 2)),
        _ => st(tel(v, 1)),
    }
}

/// 基本型の所有者 FQN (impl のメソッドを置く場所。3.2 の孤児規則)。
fn prim_owner(name: &str) -> String {
    match name {
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128" | "usize"
        | "f32" | "f64" => format!("core.num.{}", name),
        "bool" => "core.bool.bool".into(),
        "char" => "core.char.char".into(),
        "str" => "core.str".into(),
        other => other.to_string(),
    }
}

/// 型の中に現れる名前つき型を集める (`ty_mentions` と同じ辺をたどる)。
fn collect_ty_names(t: &V, out: &mut HashSet<String>) {
    match tag(t).as_str() {
        "named" => {
            out.insert(st(tel(t, 1)));
            for a in items(tel(t, 2)) {
                if matches!(a, V::L(_)) {
                    collect_ty_names(a, out);
                }
            }
        }
        "ref" | "ptr" => collect_ty_names(tel(t, 2), out),
        "slice" | "array" => collect_ty_names(tel(t, 1), out),
        "tuple" => {
            for x in items(tel(t, 1)) {
                collect_ty_names(x, out);
            }
        }
        _ => {}
    }
}

/// 型 V の中に `Self` (tag "self") が現れるか (5.1 の dyn 安全性)。
fn ty_has_self(t: &V) -> bool {
    match t {
        V::L(xs) => xs.first().map_or(false, |x| st(x) == "self") || xs.iter().any(ty_has_self),
        V::Node(_, fs, _) => fs.iter().any(ty_has_self),
        _ => false,
    }
}

fn ty_mentions(t: &V, name: &str) -> bool {
    match tag(t).as_str() {
        "named" => {
            st(tel(t, 1)) == name || items(tel(t, 2)).iter().any(|a| matches!(a, V::L(_)) && ty_mentions(a, name))
        }
        "ref" | "ptr" => ty_mentions(tel(t, 2), name),
        "slice" | "array" => ty_mentions(tel(t, 1), name),
        "tuple" => items(tel(t, 1)).iter().any(|x| ty_mentions(x, name)),
        _ => false,
    }
}

const INT_PRIMS: &[&str] = &["i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize"];

/// レーベンシュタイン距離。
pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i];
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur.push((prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost));
        }
        prev = cur;
    }
    prev[b.len()]
}
