//! 型付きの式推論 (型付き IR の最初の段階)。
//!
//! 関数本体の各式に型 (`Ty`) を与える。型変数 (`Ty::Var`) と双方向の単一化で、整数・浮動小数リテラルの型と
//! コンテナの要素型 (`let mut v = Vec.new()` を `v.push(1u8)` で決める) を、関数の中で伝播する。
//! 検査: レシーバの型によるメソッドの存在 (E1108)・引数の個数 (E2022)・プリミティブ引数の型 (E2023)、
//! 演算子の両辺の型の不一致 (E2024)、添字の型 (E2025)。
//! 方針は「分からなければ落とす」(fail-closed、実装計画 1.2 / 段階 1): 関数の推論が終わっても型が決まらない
//! 式・束縛は E2044 (型注釈を書かせる)。LLM が書いて誰も読まないので、誤検出は 1 回の書き直しで済み、
//! 取りこぼしは残り続ける。逃げ道は `--no-strict-types` / `KSN_STRICT_TYPES=0` (1 リリースだけ)。
//! 途中の `Ty::Unknown` は推論の穴で、関数の終わりに E2044 として拾われる。
//! メソッド呼び出しは、型パラメータのレシーバは境界 (とスーパートレイト) が宣言するものだけ、基本型のレシーバは
//! 規範のシグネチャにあるものだけ (どちらも E1108)。
//! 参照は単一化の中では透過的に扱う (`&T` と `T` は同じとみなす) が、参照の段数の食い違いは別に見る:
//! 値が要る所への参照 (`&u32` を `u32` として返す・`let` の注釈に入れる) は `ref_level_conflict`、
//! 呼び出しの引数は両方向を `arg_ref_conflict` が見る (4.2 が許すのは `&mut T` → `&T`・再借用・`&[T; N]` → `&[T]`・
//! `dyn` / 関数ポインタへの変換だけ)。参照越しの分解で得た名前は段数が決まらないので引数の逆向きは黙る。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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
    /// 生ポインタ `*const T` (false) / `*mut T` (true) (3.8)。`&T` とは別の型で、暗黙には相互変換しない
    /// (`*mut` → `*const` の縮小だけ暗黙に許す。`*const` → `*mut` は `core.ptr.as_mut` のみ)。
    Ptr(bool, Box<Ty>),
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
    Closure { args: Vec<Ty>, ret: Box<Ty>, cap: Cap, id: Option<String>, eff: Eff },
    /// トレイトオブジェクト `dyn Tr` (5.1)。`Tr` は書かれたトレイト名で、使用箇所で trait 定義の FQN に解決する。
    /// `dyn` は `&` / `&mut` の背後か `alloc.boxed.Box` の第1型引数にのみ現れる (`Ref` / `Box` の中身として持つ)。
    /// 2 つ目は `dyn Tr<E, P>` の型引数 (効果・panic の実引数を実体化するのに使う。型の同一性には入れない)。
    Dyn(String, TArgs),
}

/// 関数型・関数値・クロージャの効果 (仕様 5.1 の `E0 ⊆ E` / `P0 ⊑ P` の検査用)。型の同一性には入れない。
#[derive(Clone, Debug, Default)]
pub struct Eff(pub Option<EffInfo>);

#[derive(Clone, Debug)]
pub struct EffInfo {
    /// true = 型注釈 `fn(..) uses [..] panics ..` (受け側)、false = 関数・クロージャの実際の効果 (渡す側)。
    pub decl: bool,
    pub uses: Vec<String>,
    /// `None` = panic 変数 (分からない)。
    pub may: Option<bool>,
    /// 渡す側の名前 (メッセージ用)。
    pub name: String,
}

impl PartialEq for Eff {
    fn eq(&self, _: &Eff) -> bool {
        true
    }
}

/// `dyn Tr<..>` の型引数 (書かれたままの構文木)。どの値も等しいとみなす (型の比較に影響させない)。
#[derive(Clone, Debug)]
pub struct TArgs(pub Vec<V>);

impl PartialEq for TArgs {
    fn eq(&self, _: &TArgs) -> bool {
        true
    }
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
    pub(crate) fn strip(&self) -> &Ty {
        let mut t = self;
        while let Ty::Ref(_, inner) = t {
            t = inner;
        }
        t
    }
    pub(crate) fn show(&self) -> String {
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
            Ty::Ptr(m, t) => format!("{} {}", if *m { "*mut" } else { "*const" }, t.show()),
            Ty::Tuple(ts) => format!("({})", ts.iter().map(|x| x.show()).collect::<Vec<_>>().join(", ")),
            Ty::Array(t) => format!("[{}; _]", t.show()),
            Ty::Slice(t) => format!("[{}]", t.show()),
            Ty::Param(p) => p.clone(),
            Ty::Const(c) => c.clone(),
            Ty::Closure { args, ret, .. } => {
                format!("fn({}) -> {}", args.iter().map(|x| x.show()).collect::<Vec<_>>().join(", "), ret.show())
            }
            Ty::Dyn(t, _) => format!("dyn {}", t),
        }
    }
    fn has_param(&self) -> bool {
        match self {
            Ty::Param(_) => true,
            Ty::Named(_, a) | Ty::Tuple(a) => a.iter().any(|x| x.has_param()),
            Ty::Ref(_, x) | Ty::Ptr(_, x) | Ty::Array(x) | Ty::Slice(x) => x.has_param(),
            Ty::Closure { args, ret, .. } => args.iter().any(|x| x.has_param()) || ret.has_param(),
            _ => false,
        }
    }
}

pub(crate) const INTS: &[&str] = &["i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize"];
const FLOATS: &[&str] = &["f32", "f64"];

/// 型の構文木 (V) を `Ty` にする。`params` に含まれる名前は型パラメータ。
pub fn from_v(v: &V, params: &HashSet<String>) -> Ty {
    if is_nil(v) {
        return Ty::Unknown;
    }
    match tag(v).as_str() {
        "prim" => Ty::Prim(st(tel(v, 1))),
        "never" => Ty::Never,
        // 生ポインタ `*const T` / `*mut T` (FFI)。参照とは別の名義型として扱う。
        // トレイト宣言の中の `Self` (使う側で、呼び出しごとの型に置き換える)。
        "self" => Ty::Param("Self".into()),
        "tuple" => {
            let xs: Vec<Ty> = items(tel(v, 1)).iter().map(|x| from_v(x, params)).collect();
            if xs.is_empty() { Ty::Unit } else { Ty::Tuple(xs) }
        }
        "array" => Ty::Array(Box::new(from_v(tel(v, 1), params))),
        "slice" => Ty::Slice(Box::new(from_v(tel(v, 1), params))),
        "ref" => Ty::Ref(matches!(tel(v, 1), V::B(true)), Box::new(from_v(tel(v, 2), params))),
        // 関数ポインタ型 `fn(A) -> R uses [..] panics ..`。効果は型の同一性に入れない (効果検査が見る)。
        "fn" => Ty::Closure {
            args: items(tel(v, 1)).iter().map(|x| from_v(x, params)).collect(),
            ret: Box::new(from_v(tel(v, 2), params)),
            cap: Cap::Fn,
            id: None,
            eff: Eff(Some(EffInfo {
                decl: true,
                uses: strs(tel(v, 3)),
                may: match st(tel(v, 4)).as_str() {
                    "never" => Some(false),
                    "may" => Some(true),
                    _ => None,
                },
                name: String::new(),
            })),
        },
        "ptr" => Ty::Ptr(matches!(tel(v, 1), V::B(true)), Box::new(from_v(tel(v, 2), params))),
        "named" => {
            let name = st(tel(v, 1));
            if params.contains(&name) {
                return Ty::Param(name);
            }
            let args: Vec<Ty> = items(tel(v, 2))
                .iter()
                .filter(|a| matches!(a, V::L(_)) && tag(a) != "life")
                // 型実引数に書かれた効果・panic の実引数は、型の同一性に入れない (定数と同じ「どれでもよい」印)。
                .map(|a| if matches!(tag(a).as_str(), "effects" | "panic") { Ty::Const("effect".into()) } else { const_targ(a).map_or_else(|| from_v(a, params), Ty::Const) })
                .collect();
            Ty::Named(name, args)
        }
        // `dyn Tr` (のトレイト名)。型引数・効果・ライフタイムは解決の対象にしない (近似)。
        "dyn" => Ty::Dyn(st(tel(tel(v, 1), 0)), TArgs(items(tel(tel(v, 1), 1)).to_vec())),
        _ => Ty::Unknown,
    }
}

/// 効果・panic のジェネリクス (`E: uses`, `P: panics`) は型の同一性に関与しないので、どれでもよい印
/// (`Const("effect")`) に束縛しておく (未解決の型変数として残さない)。
fn seed_effects(d: &Def, map: &mut HashMap<String, Ty>) {
    for g in &d.generics {
        if g.0 == "uses" || g.0 == "panics" {
            map.entry(g.1.clone()).or_insert_with(|| Ty::Const("effect".into()));
        }
    }
}

/// `impl` の対象型 (`impl_for`) の先頭の型名。名前付きの型 (`named`) だけ返す。
fn impl_head(v: &V) -> Option<String> {
    match v {
        V::L(xs) if matches!(xs.first(), Some(V::S(k)) if k == "named") => Some(st(&xs[1])),
        _ => None,
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
        Ty::Ptr(m, x) => Ty::Ptr(*m, Box::new(subst(x, bind))),
        Ty::Tuple(xs) => Ty::Tuple(xs.iter().map(|x| subst(x, bind)).collect()),
        Ty::Array(x) => Ty::Array(Box::new(subst(x, bind))),
        Ty::Slice(x) => Ty::Slice(Box::new(subst(x, bind))),
        Ty::Closure { args, ret, cap, id, eff } => Ty::Closure {
            args: args.iter().map(|x| subst(x, bind)).collect(),
            ret: Box::new(subst(ret, bind)),
            cap: cap.clone(),
            id: id.clone(),
            eff: eff.clone(),
        },
        other => other.clone(),
    }
}

const DERIVED: &[&str] = &[
    "clone", "eq", "cmp", "partial_cmp", "hash", "debug_fmt", "display_fmt", "call", "call_mut", "call_once", "drop",
    "try_clone",
];

/// 型で一意に解決できたメソッド呼び出しの記録 (効果検査 ① 用)。
/// 解決が不確かなもの (複数候補・型不明) は記録しない (check 側は黙る)。
pub struct ResolvedCall {
    /// 呼び出しを含む定義の添字 (書き手)。
    pub writer: usize,
    /// その呼び出しが属するクロージャ節 (`None` = 定義の本体そのもの)。
    pub scope: Option<String>,
    /// 解決先の定義の添字 (メソッドの `Def`。native sig も含む)。
    pub callee: usize,
    pub line: usize,
    /// メソッド呼び出しの明示型引数 (`x.m::<..>`)。
    pub targs: Vec<V>,
    /// `dyn Tr<E, P>` 越しの呼び出しのとき: (表示名, 実体化した `uses`, 実体化した `panics`)。`callee` はトレイトの定義。
    pub dyn_eff: Option<(String, Vec<String>, String)>,
}

/// メソッド呼び出し `x.m(..)` / フィールド参照 `x.f` のレシーバの型の記録 (`ksn rename` / `ksn refs` 用)。
/// `pos` は名前トークンの位置 (パーサが MCall / Field の末尾の子に持たせる)。型は関数の推論が終わった時点で解決した形。
pub struct RecvSite {
    /// 式を含む定義の添字。
    pub writer: usize,
    /// MCall / Field では名前トークンの位置、局所変数から始まるパス (`x.f.m(..)`) ではパスの先頭の位置。
    pub pos: usize,
    /// パスの場合の、名前の添字 (MCall / Field では `usize::MAX`)。
    pub seg: usize,
    pub field: bool,
    pub recv: Ty,
    /// メソッド名 / フィールド名 (可視性の検査 (E1119 / E1122) 用)。
    pub name: String,
}

/// 比較のメソッド・演算子が要求するトレイト (4.3)。`a.eq(b)` と `a == b` は `PartialEq`、`a.partial_cmp(b)` と `<` 等は `PartialOrd`、`a.cmp(b)` は `Ord`。
fn cmp_trait(name: &str) -> Option<&'static str> {
    match name {
        "eq" | "==" | "!=" => Some("core.cmp.PartialEq"),
        "partial_cmp" | "<" | "<=" | ">" | ">=" => Some("core.cmp.PartialOrd"),
        "cmp" => Some("core.cmp.Ord"),
        _ => None,
    }
}

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

/// 式 ID の型表の 1 件 (設計_KIR.md 2.4 / 7 章 4)。関数の検査の終わりに型変数を解決した型を持つ。
#[derive(Clone, Debug)]
pub struct ExprTy {
    /// ノードの種別 (`Call` / `Lit` など)。
    pub kind: String,
    /// 式の行 (ソースの行)。
    pub line: usize,
    /// 解決後の型。未知・型変数が残りうる (分からない型は黙る方針)。
    pub ty: Ty,
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
    /// `let` の束縛の型 ((定義の添字, `let` の行, 名前) → 型)。関数の推論が終わった時点で解決した形。
    /// 所有権の検査 (check.rs の move・線形型) が、自前で型を決められない束縛に使う。
    pub let_tys: HashMap<(usize, usize, String), Ty>,
    let_pending: Vec<(usize, String, Ty)>,
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
    /// クロージャ節 (5.3 の `関数名#番号`) → そのクロージャの AST ノード。
    /// スタック上界の見積り (E4012) が、節の引数と本体の `let` からフレームを計算するために使う。
    pub closure_asts: std::collections::BTreeMap<String, V>,
    /// 型で一意に解決できたメソッド呼び出し (効果検査 ①)。
    pub resolved_calls: Vec<ResolvedCall>,
    /// メソッド呼び出し・フィールド参照のレシーバの型 (改名ツール用)。
    pub recv_log: Vec<RecvSite>,
    recv_start: usize,
    /// いま推論している `Path` ノードの位置 (`path()` が局所変数の連鎖を記録するのに使う)。
    path_pos: usize,
    /// `--strict-types` / `KSN_STRICT_TYPES=1`: 型が決まらない式・束縛を E2044 にする (実装計画 段階 1)。
    strict: bool,
    /// strict のとき、関数ごとに集める式・束縛の型の記録。
    sites: Vec<Site>,
    /// 推論中の式の記録の添字の列 (先頭が最も内側)。
    site_stack: Vec<usize>,
    /// 直前に推論し終えた式の記録 (let の右辺から束縛へ原因をたどるのに使う)。
    last_site: Option<usize>,
    /// いま束縛しているパターンの値の出どころの記録と行。
    pat_src: Option<usize>,
    /// 参照越しの分解で束縛している最中か (束縛した名前は参照の段数が決まらない)。
    pat_loose: bool,
    /// 参照の段数が決まらない局所変数の名前 (`arg_ref_conflict` の `loose`)。
    loose: HashSet<String>,
    /// 直後の呼び出しの各実引数が、参照の段数が決まらない束縛を根に持つか。
    arg_loose: Vec<bool>,
    /// `Box.into_dyn` の呼び出しの (元の `T`, 変換先 `D`, 行)。関数の推論が終わってから検査する。
    unsize: Vec<(Ty, Ty, usize)>,
    pat_line: usize,
    /// いま推論している式の行 (関数型への変換の効果検査の報告位置)。
    cur_line: usize,
    eff_seen: HashSet<(String, String, usize)>,
    /// extern の引数の変換は E5005 が見るので、関数型の効果検査を止める。
    conv_quiet: bool,
    /// 入れ子のループごとの「`break` があったか」。
    loop_breaks: Vec<bool>,
    /// 名前が解決できない式 (別の診断が報告するので E2044 にしない)。
    quiet_sites: HashSet<usize>,
    /// 局所変数の名前 → 束縛の記録 (使用箇所から原因の束縛へたどる)。
    local_sites: HashMap<String, usize>,
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
    /// 型表の未解決分: 式 ID → (種別, 行, 型)。いま推論中の関数の分で、`check_def` の終わりに解決して `tytab` へ移す。
    expr_pending: HashMap<u32, (String, usize, Ty)>,
    /// 型表: (関数の FQN, 式 ID) → 型。式 ID 0 (未採番のノード) は入れない。`expr_type` / `dump_types` で読む。
    pub tytab: BTreeMap<(String, u32), ExprTy>,
}

/// strict モードで集める、式または束縛の型の記録。
struct Site {
    line: usize,
    what: String,
    ty: Ty,
    /// 型が決まらない原因になり得る下位の記録 (子の式、束縛の右辺)。
    deps: Vec<usize>,
}

static STRICT_OFF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--no-strict-types` から旧来の挙動に戻す。
pub fn set_strict_types(on: bool) {
    STRICT_OFF.store(!on, std::sync::atomic::Ordering::Relaxed);
}

/// strict モードか。既定で有効。`--no-strict-types` / `KSN_STRICT_TYPES=0` で旧来の「黙る」挙動に戻せる
/// (移行用の逃げ道で、1 リリースのあいだだけ残す)。`--strict-types` / `KSN_STRICT_TYPES=1` は受け付けるが何も変えない。
pub fn strict_types() -> bool {
    !STRICT_OFF.load(std::sync::atomic::Ordering::Relaxed) && std::env::var("KSN_STRICT_TYPES").map_or(true, |v| v != "0")
}

impl<'a> TypeCk<'a> {
    pub fn new(p: &'a Program) -> Self {
        let mut owners: HashMap<String, HashMap<String, Vec<usize>>> = HashMap::new();
        for (i, d) in p.defs.iter().enumerate() {
            if d.kw == "fn" && d.is_method() {
                if let Some((o, m)) = d.name.rsplit_once('.') {
                    owners.entry(o.to_string()).or_default().entry(m.to_string()).or_default().push(i);
                    // トレイト側に置いた impl (3.2) のメソッドは、対象の型の所有者としても引ける。
                    if let Some(x) = d.trait_side_owner() {
                        for k in impl_owner_keys(&x) {
                            if k != o {
                                owners.entry(k).or_default().entry(m.to_string()).or_default().push(i);
                            }
                        }
                    }
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
        TypeCk { p, owners, trait_methods, diags: vec![], cur: 0, scopes: vec![], vars: vec![], kinds: vec![], escaped: HashSet::new(), mutable: HashSet::new(), lits: vec![], rets: vec![], let_tys: HashMap::new(), let_pending: vec![], edges: Default::default(), closure_nodes: vec![], closure_seq: 0, fn_closures: HashMap::new(), param_called: HashMap::new(), closure_asts: Default::default(), eff_acc: vec![], closure_effs: HashMap::new(), eff_checks: vec![], cov, expr_pending: HashMap::new(), tytab: BTreeMap::new(), resolved_calls: vec![], recv_log: vec![], recv_start: 0, path_pos: 0, strict: strict_types(), sites: vec![], site_stack: vec![], last_site: None, pat_src: None, pat_loose: false, loose: HashSet::new(), arg_loose: vec![], unsize: vec![], pat_line: 0, cur_line: 0, eff_seen: HashSet::new(), conv_quiet: false, loop_breaks: vec![], quiet_sites: HashSet::new(), local_sites: HashMap::new() }
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
        let (line, col) = crate::parser::unpack_pos(if line != 0 { line } else { d.line });
        self.diags.push(Diag { file: d.file.clone(), code: code.into(), fqn: d.name.clone(), line, col, msg });
    }

    pub fn check_def(&mut self, di: usize) {
        let d = &self.p.defs[di];
        let Some(body) = &d.body else { return };
        self.cur = di;
        self.recv_start = self.recv_log.len();
        self.expr_pending.clear();
        self.vars.clear();
        self.kinds.clear();
        self.escaped.clear();
        self.mutable.clear();
        self.lits.clear();
        self.sites.clear();
        self.site_stack.clear();
        self.last_site = None;
        self.pat_src = None;
        self.pat_line = 0;
        self.loop_breaks.clear();
        self.quiet_sites.clear();
        self.local_sites.clear();
        self.scopes = vec![HashMap::new()];
        let gn = d.gnames();
        for (n, t) in &d.params {
            let ty = from_v(t, &gn);
            // 関数自身の型パラメータ (Ty::Param) は、単一化では「分からない型」と同じに働くが、
            // メソッド呼び出しでは境界のトレイトを引くために名前を残す。
            self.scopes[0].insert(n.clone(), ty);
        }
        // `?` のエラー型の検査のため、この関数の戻り値型を覚える (クロージャはその注釈を重ねる)。
        let ret = from_v(&d.ret, &gn);
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
        // 戻り値は「受け側」の関数型が分かる: 宣言 × 宣言の効果の食い違いをここで報告する。
        self.check_fn_conv_dir(&ret, &value);
        if let Some((want, got)) = self.ref_level_conflict(&ret, &value) {
            let stmts = items(&nf(body)[0]);
            let line = stmts.last().map(nline).unwrap_or(0);
            self.diag("E2020", format!("type mismatch in return value: expected {}, found {}{}", want, got, self.deref_hint(&ret)), line);
        }
        self.check_dangling(body, &ret);
        self.finish_literals();
        self.check_unsize();
        self.report_undetermined();
        // 型変数は関数ごとの表なので、ここで解決してから外へ出す。
        for k in self.recv_start..self.recv_log.len() {
            let t = self.recv_log[k].recv.clone();
            self.recv_log[k].recv = self.deep(&t);
        }
        // 同じ行の同じ名前の束縛 (1 行に並べた腕など) が違う型なら、決められない (Unknown = 黙る)。
        let mut seen: HashMap<(usize, String), Ty> = HashMap::new();
        for (line, name, t) in std::mem::take(&mut self.let_pending) {
            let t = self.deep(&t);
            match seen.get(&(line, name.clone())) {
                Some(old) if *old != t => {
                    self.let_tys.insert((di, line, name), Ty::Unknown);
                }
                _ => {
                    seen.insert((line, name.clone()), t.clone());
                    self.let_tys.insert((di, line, name), t);
                }
            }
        }
        self.flush_expr_types();
        self.cov_flush();
    }

    /// 型の中に、決まっていない部分 (`Unknown` / 未解決の型変数) があれば true。
    /// 接尾辞のない数値リテラルの型変数は E2027 が見るので、ここでは数えない (escape されたものだけ数える)。
    fn undetermined(&self, t: &Ty) -> bool {
        match self.deep(t) {
            Ty::Unknown => true,
            Ty::Var(v) => self.kinds[v] == 0 || self.escaped.contains(&v),
            Ty::Named(_, a) | Ty::Tuple(a) => a.iter().any(|x| self.undetermined(x)),
            Ty::Ref(_, x) | Ty::Ptr(_, x) | Ty::Array(x) | Ty::Slice(x) => self.undetermined(&x),
            Ty::Closure { args, ret, .. } => args.iter().any(|x| self.undetermined(x)) || self.undetermined(&ret),
            _ => false,
        }
    }

    /// strict: 関数の推論が終わった時点で型が決まっていない式・束縛を E2044 にする。
    /// 原因の連鎖 (下位の式が決まらないので上位も決まらない) は根の 1 件だけを報告する。
    fn report_undetermined(&mut self) {
        let sites = std::mem::take(&mut self.sites);
        if !self.strict {
            return;
        }
        let und: Vec<bool> = sites.iter().map(|s| self.undetermined(&s.ty)).collect();
        let mut seen: HashSet<(usize, String)> = HashSet::new();
        for (i, s) in sites.iter().enumerate() {
            if und[i] && !self.quiet_sites.contains(&i) && !s.deps.iter().any(|&d| und[d]) && seen.insert((s.line, s.what.clone())) {
                let shown = self.deep(&s.ty).show();
                self.diag(
                    "E2044",
                    format!("cannot determine the type of {} (inferred so far: `{}`): add a type annotation", s.what, shown),
                    s.line,
                );
            }
        }
    }

    /// いま推論中の式の型は局所変数 `name` の型に依存する (決まらない原因は束縛の側にある)。
    fn dep_on_local(&mut self, name: &str) {
        if let (Some(&top), Some(&b)) = (self.site_stack.last(), self.local_sites.get(name)) {
            self.sites[top].deps.push(b);
        }
    }

    /// いま推論中の式を、E2044 の対象から外す (名前が解決できないなど、別の診断が報告する誤り)。
    fn quiet_here(&mut self) {
        if let Some(&i) = self.site_stack.last() {
            self.quiet_sites.insert(i);
        }
    }

    fn new_site(&mut self, line: usize, what: String, ty: Ty) -> usize {
        let id = self.sites.len();
        self.sites.push(Site { line, what, ty, deps: vec![] });
        if let Some(&parent) = self.site_stack.last() {
            self.sites[parent].deps.push(id);
        }
        id
    }

    /// 式の説明 (E2044 の文面と、原因別の集計に使う)。
    fn describe(&self, e: &V) -> String {
        let f = nf(e);
        match nname(e) {
            "Path" => format!("the path `{}`", strs(&f[0]).join(".")),
            "Call" => match nname(&f[0]) {
                "Path" => format!("the call to `{}`", strs(&nf(&f[0])[0]).join(".")),
                _ => "the call of an expression".to_string(),
            },
            "MCall" => format!("the method call `.{}(..)`", st(&f[1])),
            "Field" => format!("the field access `.{}`", st(&f[1])),
            "Lit" => format!("the {} literal", st(&f[0])),
            "Bin" => format!("the `{}` expression", st(&f[0])),
            "Unary" => format!("the `{}` expression", st(&f[0])),
            "" => "a value".to_string(),
            n => format!("the `{}` expression", n),
        }
    }

    /// 解決済みの型を、型注釈と同じ AST の形に戻す。未決定の部分を含むときは None。
    pub fn to_v(t: &Ty) -> Option<V> {
        let s = |x: &str| V::S(x.to_string());
        Some(match t {
            Ty::Prim(p) => V::L(vec![s("prim"), s(p)]),
            Ty::Unit => V::L(vec![s("tuple"), V::L(vec![])]),
            Ty::Tuple(xs) => V::L(vec![s("tuple"), V::L(xs.iter().map(Self::to_v).collect::<Option<Vec<V>>>()?)]),
            Ty::Array(x) => V::L(vec![s("array"), Self::to_v(x)?]),
            Ty::Slice(x) => V::L(vec![s("slice"), Self::to_v(x)?]),
            Ty::Ref(m, x) => V::L(vec![s("ref"), V::B(*m), Self::to_v(x).unwrap_or(V::Nil)]),
            Ty::Ptr(m, x) => V::L(vec![s("ptr"), V::B(*m), Self::to_v(x)?]),
            Ty::Named(n, args) => V::L(vec![s("named"), s(n), V::L(args.iter().map(Self::to_v).collect::<Option<Vec<V>>>()?)]),
            _ => return None,
        })
    }

    /// 型表: この関数で推論した式の型を、型変数を解決して `tytab` に移す (同じ式 ID は最後の推論の結果)。
    fn flush_expr_types(&mut self) {
        let fqn = self.def().name.clone();
        for (id, (kind, line, t)) in std::mem::take(&mut self.expr_pending) {
            let ty = self.deep(&t);
            self.tytab.insert((fqn.clone(), id), ExprTy { kind, line, ty });
        }
    }

    /// 型表から、関数 `fqn` の式 ID `id` の型を引く。段階 1 (KIR の降格) の入口で使う。
    #[allow(dead_code)]
    pub fn expr_type(&self, fqn: &str, id: u32) -> Option<&ExprTy> {
        self.tytab.get(&(fqn.to_string(), id))
    }

    /// 型表を 1 行 1 式で文字列にする (`KSN_DUMP_TYPES=1 ksn check` が stderr に出す)。
    /// 並びは (FQN, 式 ID) の順なので、同じソースからは同じ出力になる。
    pub fn dump_types(&self) -> String {
        let mut out = String::new();
        for ((fqn, id), e) in &self.tytab {
            out.push_str(&format!("{}\t#{}\t{}\tline {}\t{}\n", fqn, id, e.kind, e.line, e.ty.show()));
        }
        out
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
            key: crate::parser::nid(e) as usize,
            kind,
            detail,
            ty: t.clone(),
            subject: subject.map(|s| crate::parser::nid(s) as usize),
            local,
            loc: format!("{}:{}", self.def().file, nline(e)),
        };
        if let Some(c) = self.cov.as_mut() {
            c.record(rec);
        }
    }

    /// 戻り値が参照のとき、局所変数 (引数以外) への参照を返していないか。
    fn check_dangling(&mut self, body: &V, _ret: &Ty) {
        let d = self.def();
        let params: HashSet<&String> = d.params.iter().map(|p| &p.0).collect();
        let mut nodes: Vec<&V> = vec![];
        crate::check::walk(body, &mut nodes);
        let mut locals: HashSet<String> = HashSet::new();
        // 局所変数への参照を持つ束縛 (`let r = &x`、`let r2 = r`)。
        let mut ref_locals: HashMap<String, String> = HashMap::new();
        for n in &nodes {
            if nname(n) == "Let" {
                let mut b = vec![];
                pat_binds(&nf(n)[0], &mut b);
                if nname(&nf(n)[0]) == "PBind" {
                    // 初期化式が局所変数への参照そのもの、またはそれを中に持つ値
                    // (構造体リテラルのフィールド・タプル・バリアントの引数) のとき。
                    let mut leaves: Vec<&V> = vec![];
                    Self::escaping(&nf(n)[3], &mut leaves);
                    if let Some(src) = leaves.iter().find_map(|l| Self::local_ref_source(l, &locals, &params, &ref_locals)) {
                        ref_locals.insert(b[0].clone(), src);
                    }
                }
                locals.extend(b);
            }
        }
        // 戻り値になる式 (本体の末尾と `return`)。`if` / `match` / ブロックの末尾、構造体リテラルの
        // フィールド、タプル、バリアントの引数 (`Some(..)` など) を通して、外へ出る葉の式を集める。
        let mut leaves: Vec<&V> = vec![];
        if let Some(last) = items(&nf(body)[0]).last() {
            if nname(last) == "ExprStmt" {
                Self::escaping(&nf(last)[0], &mut leaves);
            }
        }
        for n in &nodes {
            if nname(n) == "Return" && !is_nil(&nf(n)[0]) {
                Self::escaping(&nf(n)[0], &mut leaves);
            }
        }
        let mut reported: HashSet<(String, usize)> = HashSet::new();
        for r in leaves {
            if let Some(src) = Self::local_ref_source(r, &locals, &params, &ref_locals) {
                if reported.insert((src.clone(), nline(r))) {
                    self.diag(
                        "E3030",
                        format!("returns a reference to the local variable `{}`, which does not live long enough", src),
                        nline(r),
                    );
                }
            }
        }
    }

    /// 式が局所変数への参照 (`&x` / `&mut x`、またはそれを持つ束縛) なら、指している局所変数の名前。
    fn local_ref_source(e: &V, locals: &HashSet<String>, params: &HashSet<&String>, ref_locals: &HashMap<String, String>) -> Option<String> {
        let f = nf(e);
        match nname(e) {
            "Unary" if matches!(st(&f[0]).as_str(), "&" | "&mut") && nname(&f[1]) == "Path" => {
                let segs = strs(&nf(&f[1])[0]);
                (segs.len() == 1 && locals.contains(&segs[0]) && !params.contains(&segs[0])).then(|| segs[0].clone())
            }
            "Path" => {
                let segs = strs(&f[0]);
                if segs.len() == 1 { ref_locals.get(&segs[0]).cloned() } else { None }
            }
            _ => None,
        }
    }

    /// 値として外へ出る葉の式を集める。
    fn escaping<'v>(e: &'v V, out: &mut Vec<&'v V>) {
        let f = nf(e);
        match nname(e) {
            "Block" => {
                if let Some(last) = items(&f[0]).last() {
                    if nname(last) == "ExprStmt" {
                        Self::escaping(&nf(last)[0], out);
                    }
                }
            }
            "If" => {
                Self::escaping(&f[1], out);
                if !is_nil(&f[2]) {
                    Self::escaping(&f[2], out);
                }
            }
            "Match" => {
                for arm in items(&f[1]) {
                    Self::escaping(&items(arm)[1], out);
                }
            }
            "StructLit" => {
                for x in items(&f[1]) {
                    Self::escaping(&items(x)[1], out);
                }
            }
            "TupleE" => {
                for x in items(&f[0]) {
                    Self::escaping(x, out);
                }
            }
            // バリアントの構築 (`Some(x)` / `Ok(x)` / `m.E.V(x)`): 末尾の名前が大文字で始まる呼び出し。
            "Call" if nname(&f[0]) == "Path" => {
                let segs = strs(&nf(&f[0])[0]);
                if segs.last().and_then(|s| s.chars().next()).map_or(false, |c| c.is_uppercase()) {
                    for a in items(&f[1]) {
                        Self::escaping(a, out);
                    }
                }
            }
            _ => out.push(e),
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
    /// 同じ型になるべき 2 つの式 (`if` の両枝、`match` の腕) の型を一致させる。
    /// 結果は報告しない (不一致は別の検査が見る)。分からない型 (未知・型パラメータ)
    /// とは一致させない。2 つのクロージャが合流するときは、実際の効果 (`eff`) を和に
    /// して残す (先に入った方だけが残る穴を塞ぐ)。
    fn join_arm(&mut self, a: &Ty, b: &Ty) -> Ty {
        let unknown = |t: &Ty| matches!(t, Ty::Unknown | Ty::Param(_));
        if unknown(&self.resolve(a)) || unknown(&self.resolve(b)) {
            return a.clone();
        }
        let _ = self.unify2(a, b);
        self.merge_closure_eff(a, b)
    }

    /// 実際の効果を和にする (`uses` の和、`may` はどちらか `may` なら `may`)。
    fn eff_union(&self, ea: &Eff, eb: &Eff) -> Eff {
        let Eff(Some(a)) = ea else { return eb.clone() };
        let Eff(Some(b)) = eb else { return ea.clone() };
        let mut uses = a.uses.clone();
        for e in &b.uses {
            if !uses.contains(e) {
                uses.push(e.clone());
            }
        }
        let may = match (a.may, b.may) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        };
        Eff(Some(EffInfo {
            decl: a.decl,
            uses,
            may,
            name: a.name.clone(),
        }))
    }

    /// 2 つのクロージャを合流させた型 (実際の効果の和を残す)。クロージャでなければ a。
    fn merge_closure_eff(&self, a: &Ty, b: &Ty) -> Ty {
        let (ca, cb) = (self.resolve(a), self.resolve(b));
        let (Ty::Closure { args, ret, cap, id, eff: ea }, Ty::Closure { eff: eb, .. }) =
            (&ca, &cb)
        else {
            return ca;
        };
        Ty::Closure {
            args: args.clone(),
            ret: ret.clone(),
            cap: cap.clone(),
            id: id.clone(),
            eff: self.eff_union(ea, eb),
        }
    }

    fn escape(&mut self, t: &Ty) {
        match self.resolve(t) {
            Ty::Var(v) => {
                self.escaped.insert(v);
            }
            Ty::Named(_, a) | Ty::Tuple(a) => a.iter().for_each(|x| self.escape(x)),
            Ty::Ref(_, x) | Ty::Ptr(_, x) | Ty::Array(x) | Ty::Slice(x) => self.escape(&x),
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

    /// capture 節と本体から、満たせ得る最大の能力を求める (4.2: 捕捉の使い方で決まる)。
    /// `&x` は共有参照なので `Fn`。`&mut x` は従来どおり `FnMut`。`move x` は、本体が `x` を変更すれば
    /// `FnMut`、型が `Copy` でない `x` を値のまま消費すれば `FnOnce`、読むだけなら `Fn` のまま。
    /// 型や使い方が分からないときは消費・変更しないとみなす (黙る)。
    fn closure_cap(&self, caps: &V, body: &V) -> Cap {
        let mut cap = Cap::Fn;
        for c in items(caps) {
            let (Some(V::S(k)), Some(V::S(n))) = (items(c).first(), items(c).get(1)) else { continue };
            match k.as_str() {
                "move" => {
                    let u = self.capture_use(body, n);
                    if u.consumed {
                        cap = Cap::FnOnce;
                    } else if u.mutated && cap == Cap::Fn {
                        cap = Cap::FnMut;
                    }
                }
                "&mut" if cap == Cap::Fn => cap = Cap::FnMut,
                _ => {}
            }
        }
        cap
    }

    /// 本体の中での捕捉された局所 `name` の使い方。型は外側の束縛から引く (本体より外の scope にある)。
    fn capture_use(&self, body: &V, name: &str) -> CapUse {
        let non_copy = self.lookup(name).map(|t| {
            self.ty_copy(&self.deep(&t)) == Some(false)
                // `F: core.ops.FnOnce` の型パラメータを `move` で取り込み、本体で呼ぶと、
                // そのクロージャも `FnOnce` になる (e15)。
                || self.is_fnonce_param(name)
        }).unwrap_or(false);
        let mut u = CapUse::default();
        self.scan_capture(body, name, non_copy, false, &mut u);
        u
    }

    /// 型パラメータ `name` の束縛が `F: core.ops.FnOnce` のように `FnOnce` を境界に持つか。
    /// その値を本体で呼ぶと消費される (一度きり)。
    fn is_fnonce_param(&self, name: &str) -> bool {
        let Some(Ty::Param(p)) = self.lookup(name) else { return false };
        let d = self.def();
        d.generics.iter().filter(|g| g.0 == "type" && g.1 == p).any(|g| {
            items(&g.2).iter().any(|b| {
                let bn = match b {
                    V::L(bx) if !bx.is_empty() => st(&bx[0]),
                    V::S(x) => x.clone(),
                    _ => return true,
                };
                bn == "FnOnce" || bn.ends_with(".FnOnce")
            })
        })
    }

    /// `e` を走査して `name` の使い方を `u` に記録する。`borrowed` は `e` が借用・読取りの位置にあるか
    /// (その位置の `name` は消費しない)。借用・読取りの位置は、単項演算・フィールド・添字の根・
    /// メソッドのレシーバ・二項演算の項・代入の左辺。それ以外の位置での `name` は値として消費する。
    fn scan_capture(&self, e: &V, name: &str, non_copy: bool, borrowed: bool, u: &mut CapUse) {
        let f = nf(e);
        match nname(e) {
            "Path" => {
                if !borrowed && non_copy && place_root_is(e, name) {
                    u.consumed = true;
                }
            }
            "Assign" => {
                if place_root_is(&f[0], name) {
                    u.mutated = true;
                }
                self.scan_capture(&f[0], name, non_copy, true, u);
                self.scan_capture(&f[1], name, non_copy, false, u);
            }
            "Unary" => {
                if st(&f[0]) == "&mut" && place_root_is(&f[1], name) {
                    u.mutated = true;
                }
                self.scan_capture(&f[1], name, non_copy, true, u);
            }
            "Field" => self.scan_capture(&f[0], name, non_copy, true, u),
            "Index" => {
                self.scan_capture(&f[0], name, non_copy, true, u);
                self.scan_capture(&f[1], name, non_copy, false, u);
            }
            "MCall" => {
                self.scan_capture(&f[0], name, non_copy, true, u);
                // `F: core.ops.FnOnce` の型パラメータをレシーバに呼ぶと消費される (e15)。
                if non_copy && self.is_fnonce_param(name) && place_root_is(&f[0], name) {
                    u.consumed = true;
                }
                for a in items(&f[2]) {
                    self.scan_capture(a, name, non_copy, false, u);
                }
            }
            "Call" => {
                // `f()` で `f` が `F: FnOnce` の型パラメータなら消費される (e15)。`f.call_once(())` のように
                // 修飾パスで呼んでも、根が `f` なら同様に消費する。
                if let Some(callee) = f.first() {
                    if nname(callee) == "Path" {
                        let segs = strs(&nf(callee)[0]);
                        if !segs.is_empty() && segs[0] == name
                            && non_copy
                            && self.is_fnonce_param(name)
                        {
                            u.consumed = true;
                        }
                    }
                }
                for c in crate::check::children(e) {
                    self.scan_capture(c, name, non_copy, false, u);
                }
            }
            "Bin" => {
                self.scan_capture(&f[1], name, non_copy, true, u);
                self.scan_capture(&f[2], name, non_copy, true, u);
            }
            // 内側のクロージャは捕捉の節だけを見る。`move` で捕まえれば値を移すので消費、`&mut` なら変更。
            "Closure" => {
                for c in items(&f[2]) {
                    let (Some(V::S(k)), Some(V::S(n))) = (items(c).first(), items(c).get(1)) else { continue };
                    if n != name {
                        continue;
                    }
                    match k.as_str() {
                        "move" if non_copy => u.consumed = true,
                        "&mut" => u.mutated = true,
                        _ => {}
                    }
                }
            }
            _ => {
                for c in crate::check::children(e) {
                    self.scan_capture(c, name, non_copy, false, u);
                }
            }
        }
    }

    /// 型が `Copy` か。`Some(false)` は確実に `Copy` でない (引数なしのユーザー型で `Copy` を実装しない)。
    /// 分からない型 (型引数つき・型変数・型パラメータ・スライスなど) は `None`。
    fn ty_copy(&self, t: &Ty) -> Option<bool> {
        match t {
            Ty::Prim(_) | Ty::Unit | Ty::Never => Some(true),
            Ty::Ref(m, _) => Some(!*m),
            Ty::Array(x) => self.ty_copy(x),
            Ty::Tuple(xs) => {
                let parts: Vec<Option<bool>> = xs.iter().map(|x| self.ty_copy(x)).collect();
                if parts.contains(&Some(false)) {
                    Some(false)
                } else if parts.iter().all(|p| *p == Some(true)) {
                    Some(true)
                } else {
                    None
                }
            }
            // `Copy` を実装しない型は、型引数があっても `Copy` でない。`Copy` を持つ型は、型引数つきなら
            // 条件つき (`T: Copy`) かもしれないので分からない。
            Ty::Named(n, args) if self.p.types.contains_key(n) => {
                if !self.implements(n, "core.ops.Copy") {
                    Some(false)
                } else if args.is_empty() {
                    Some(true)
                } else {
                    None
                }
            }
            // `FnOnce` クロージャは一度しか呼べないので `Copy` ではない。外側のクロージャが
            // 本体でそれを呼ぶと、外側も `FnOnce` になる (cap 推論で伝える)。
            Ty::Closure { cap, .. } if *cap == crate::types::Cap::FnOnce => Some(false),
            _ => None,
        }
    }

    fn bind(&mut self, name: &str, ty: Ty) {
        if self.strict {
            let id = self.new_site(self.pat_line, format!("the binding `{}`", name), ty.clone());
            if let Some(src) = self.pat_src {
                self.sites[id].deps.push(src);
            }
            self.local_sites.insert(name.to_string(), id);
        }
        if self.pat_loose {
            self.loose.insert(name.to_string());
        } else {
            self.loose.remove(name);
        }
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string(), ty);
        }
    }

    /// 実引数の式の根が、参照の段数が決まらない局所変数 (フィールド・添字・`*` を通してよい) か。
    fn root_loose(&self, e: &V) -> bool {
        match nname(e) {
            "Path" => {
                let segs = strs(&nf(e)[0]);
                self.loose.contains(&segs[0]) && self.lookup(&segs[0]).is_some()
            }
            "Field" | "Index" => self.root_loose(&nf(e)[0]),
            "Unary" => self.root_loose(&nf(e)[1]),
            _ => false,
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
            Ty::Ptr(m, x) => Ty::Ptr(m, Box::new(self.deep(&x))),
            Ty::Tuple(xs) => Ty::Tuple(xs.iter().map(|x| self.deep(x)).collect()),
            Ty::Array(x) => Ty::Array(Box::new(self.deep(&x))),
            Ty::Slice(x) => Ty::Slice(Box::new(self.deep(&x))),
            Ty::Closure { args, ret, cap, id, eff } => Ty::Closure {
                args: args.iter().map(|x| self.deep(x)).collect(),
                ret: Box::new(self.deep(&*ret)),
                cap,
                id,
                eff,
            },
            other => other,
        }
    }

    fn occurs(&self, v: usize, t: &Ty) -> bool {
        match self.resolve(t) {
            Ty::Var(i) => i == v,
            Ty::Named(_, a) | Ty::Tuple(a) => a.iter().any(|x| self.occurs(v, x)),
            Ty::Ref(_, x) | Ty::Ptr(_, x) | Ty::Array(x) | Ty::Slice(x) => self.occurs(v, &x),
            Ty::Closure { args, ret, .. } => args.iter().any(|x| self.occurs(v, x)) || self.occurs(v, &*ret),
            _ => false,
        }
    }

    /// 構造の種類 (単一化で矛盾を判定できるもの)。分からない・変換の余地があるものは None。
    fn concrete_kind(t: &Ty) -> Option<&'static str> {
        match t {
            Ty::Unit => Some("unit"),
            Ty::Prim(_) => Some("prim"),
            Ty::Tuple(_) => Some("tuple"),
            Ty::Named(n, _) if n != "alloc.vec.Vec" && n != "alloc.string.String" => Some("named"),
            _ => None,
        }
    }

    /// 関数・クロージャを関数型 `fn(..) uses [E] panics P` に合わせる所 (仕様 5.1) の効果の検査。
    /// `want` (受け側: 注釈) の効果に `got` (渡し側: 実際の効果) の効果が含まれていなければ E4001 / E4002。
    /// 効果変数・panic 変数の側は黙る。単一化は対称なので向きの分からない組はここでは見ない
    /// (向きが分かる場所は `check_fn_conv_dir` で見る)。
    fn check_fn_conv(&mut self, ea: &Eff, eb: &Eff) {
        if self.conv_quiet {
            return;
        }
        let (Some(x), Some(y)) = (&ea.0, &eb.0) else { return };
        let (want, got) = match (x.decl, y.decl) {
            (true, false) => (x, y),
            (false, true) => (y, x),
            _ => return,
        };
        self.check_fn_eff(want, got, self.cur_line);
    }

    /// `want` (受け側の宣言) の効果に `got` (渡し側の宣言) の効果が含まれていなければ E4001 / E4002 を報告する。
    /// 向きは呼び出し側が決める (let の注釈・引数・戻り値・構造体の欄)。宣言 × 実際と宣言 × 宣言の両方を見る。
    /// 受け側に効果変数・panic 変数があると何が求められているか分からないので黙る (呼ぶ側で実体化を検査するため)。
    /// 渡し側に効果変数・panic 変数があり、受け側が具体的なら、その変数がその場で同じ変数に
    /// 実体化されるとは限らないので fail-closed で報告する。
    fn check_fn_eff(&mut self, want: &EffInfo, got: &EffInfo, line: usize) {
        if want.uses.iter().any(|e| e.chars().next().map_or(false, |c| c.is_uppercase()))
            || want.may == None
        {
            return;
        }
        let mut out: Vec<(&str, String)> = vec![];
        for e in &got.uses {
            match e.chars().next() {
                Some(c) if c.is_lowercase() => {
                    if !crate::check::covers(&want.uses, e) {
                        out.push((
                            "E4001",
                            format!("{} uses effect `{}` but the function type declares `uses [{}]`", got.name, e, want.uses.join(", ")),
                        ));
                    }
                }
                Some(c) if c.is_uppercase() => {
                    // 渡し側に効果変数があり、受け側は具体的: どの効果に束縛されるか分からないので fail-closed。
                    out.push((
                        "E4001",
                        format!(
                            "{} uses effect variable `{}` but the function type declares `uses [{}]`",
                            if got.name.is_empty() { "the passed function type" } else { &got.name },
                            e,
                            want.uses.join(", ")
                        ),
                    ));
                }
                _ => {}
            }
        }
        if want.may == Some(false) && got.may != Some(false) {
            let g = if got.name.is_empty() { "the passed function type" } else { &got.name };
            out.push(("E4002", format!("{} may panic but the function type declares `panics never`", g)));
        }
        for (c, m) in out {
            if self.eff_seen.insert((c.to_string(), m.clone(), line)) {
                self.diag(c, m, line);
            }
        }
    }

    /// 方向が分かる場所 (let の注釈・引数・戻り値・構造体の欄) で、渡し側の関数型の宣言の効果が
    /// 受け側の宣言に含まれていなければ E4001 / E4002 を報告する。単一化は対称なので向きを
    /// `want`/`got` で外から渡す。入れ子 (Option<fn(..)> など) も辿る。
    fn check_fn_conv_dir(&mut self, want: &Ty, got: &Ty) {
        self.check_fn_conv_dir_walk(self.resolve(want), self.resolve(got));
    }

    /// `want`/`got` を辿り、関数型同士が対になる所だけ `check_fn_eff` する。
    /// 構造が揃わない組は黙る (型不一致は単一化側が見る)。
    fn check_fn_conv_dir_walk(&mut self, mut want: Ty, mut got: Ty) {
        want = self.resolve(&want);
        got = self.resolve(&got);
        // 参照は透過的。両側から潜る (単一化と同じ)。
        // `&mut T` は T について不変 (部分型は `&mut` の下で使えない): 両側が `&mut` なら向きを両方で見る。
        let mut inv = false;
        loop {
            match (&want, &got) {
                (Ty::Ref(true, w), Ty::Ref(true, g)) => {
                    inv = true;
                    (want, got) = (self.resolve(w), self.resolve(g));
                }
                (Ty::Ref(_, w), _) => want = self.resolve(w),
                (_, Ty::Ref(_, g)) => got = self.resolve(g),
                _ => break,
            }
        }
        if inv {
            self.check_fn_conv_dir_walk_one(want.clone(), got.clone());
            self.check_fn_conv_dir_walk_one(got, want);
            return;
        }
        self.check_fn_conv_dir_walk_one(want, got);
    }

    fn check_fn_conv_dir_walk_one(&mut self, want: Ty, got: Ty) {
        match (&want, &got) {
            (Ty::Closure { eff: ew, args: wa, ret: wr, .. }, Ty::Closure { eff: eg, args: ga, ret: gr, .. }) => {
                if let (Some(ew), Some(eg)) = (&ew.0, &eg.0) {
                    self.check_fn_eff(ew, eg, self.cur_line);
                }
                // 戻り値は共変: 同じ向きで再帰する。
                self.check_fn_conv_dir_walk(wr.as_ref().clone(), gr.as_ref().clone());
                // 引数は反変: 向きを逆にして再帰する。
                for (w, g) in wa.iter().zip(ga.iter()) {
                    self.check_fn_conv_dir_walk(g.clone(), w.clone());
                }
            }
            (Ty::Named(_, ws), Ty::Named(_, gs)) => {
                for (w, g) in ws.iter().zip(gs.iter()) {
                    self.check_fn_conv_dir_walk(w.clone(), g.clone());
                }
            }
            (Ty::Tuple(ws), Ty::Tuple(gs)) => {
                for (w, g) in ws.iter().zip(gs.iter()) {
                    self.check_fn_conv_dir_walk(w.clone(), g.clone());
                }
            }
            (Ty::Array(w), Ty::Array(g)) | (Ty::Slice(w), Ty::Slice(g)) => {
                self.check_fn_conv_dir_walk((**w).clone(), (**g).clone());
            }
            (Ty::Ptr(_, w), Ty::Ptr(_, g)) => {
                self.check_fn_conv_dir_walk((**w).clone(), (**g).clone());
            }
            _ => {}
        }
    }

    /// 2 つの型を等しくする。矛盾したら、その箇所の 2 つの型を返す。参照は透過的。
    fn unify2(&mut self, a: &Ty, b: &Ty) -> Result<(), (Ty, Ty)> {
        // 単一化前に、どちらかが型変数に束縛されていればその変数を記録する
        // (resolve で束縛先が展開されてしまうため)。
        let (va, vb) = (
            match a {
                Ty::Var(x) => Some((*x, self.vars[*x].clone())),
                _ => None,
            },
            match b {
                Ty::Var(x) => Some((*x, self.vars[*x].clone())),
                _ => None,
            },
        );
        let (a, b) = (self.resolve(a), self.resolve(b));
        // 型パラメータ (本体の中では「どれかの型」) と一般の型変数は、変数を型パラメータに束縛して決める。
        // リテラルの型変数 (整数・浮動小数) は従来どおり決めない。
        if let (Ty::Var(x), Ty::Param(_)) | (Ty::Param(_), Ty::Var(x)) = (&a, &b) {
            if self.kinds[*x] == 0 {
                let t = if matches!(a, Ty::Param(_)) { a.clone() } else { b.clone() };
                self.vars[*x] = Some(t);
                return Ok(());
            }
        }
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
                if let (Ty::Ref(_, inner), true) = (&t, self.kinds[*x] != 0) {
                    // 参照は透過的: リテラルの型は参照の中身に合わせる。
                    return self.unify2(&Ty::Var(*x), inner);
                }
                if self.occurs(*x, &t) {
                    return Ok(());
                }
                self.vars[*x] = Some(t);
                Ok(())
            }
            // 生ポインタと参照は暗黙に変換しない。ただし参照は透過的に扱う方針なので、ここでは黙る。
            (Ty::Ptr(..), Ty::Ref(..)) | (Ty::Ref(..), Ty::Ptr(..)) => Ok(()),
            // 要素型は不変。可変性は向きがあるので、ここでは違いを矛盾として返し、
            // 呼び出し側 (`ptr_mismatch`) が `*mut` → `*const` の許される向きかどうかを決める。
            (Ty::Ptr(m, x), Ty::Ptr(n, y)) => {
                let (m, n) = (*m, *n);
                let (x, y) = (x.clone(), y.clone());
                self.unify2(&x, &y).map_err(|_| (a.clone(), b.clone()))?;
                if m == n { Ok(()) } else { Err((a.clone(), b.clone())) }
            }
            (Ty::Ptr(..), Ty::Prim(_) | Ty::Named(..)) | (Ty::Prim(_) | Ty::Named(..), Ty::Ptr(..)) => Err((a.clone(), b.clone())),
            // 両側が参照なら、指す先どうしを合わせる (`from_ref(&x)` の `&T` に `&i32` を渡して `T = &i32` にしない)。
            (Ty::Ref(_, x), Ty::Ref(_, y)) => {
                let (x, y) = (x.clone(), y.clone());
                self.unify2(&x, &y)
            }
            // 参照は透過的。左右の順序 (期待, 実際) は診断の文面に使うので保つ。
            (Ty::Ref(_, x), y) => {
                let (x, y) = (x.clone(), y.clone());
                self.unify2(&x, &y)
            }
            (x, Ty::Ref(_, y)) => {
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
            (Ty::Dyn(p, _), Ty::Dyn(q, _)) => {
                if p == q { Ok(()) } else { Err((a.clone(), b.clone())) }
            }
            (Ty::Named(n, xs), Ty::Named(m, ys)) => {
                if n != m {
                    return Err((a.clone(), b.clone()));
                }
                let (xs, ys) = (xs.clone(), ys.clone());
                // `Box<dyn Tr>` と `Box<T>` は別の型 (変換は `Box.into_dyn` で明示する。5.1)。
                if n == "alloc.boxed.Box" && self.box_dyn_mismatch(&xs, &ys) {
                    return Err((a.clone(), b.clone()));
                }
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
            (Ty::Closure { args: xs, ret: rx, eff: ea, .. }, Ty::Closure { args: ys, ret: ry, eff: eb, .. }) => {
                if xs.len() != ys.len() {
                    return Err((a.clone(), b.clone()));
                }
                self.check_fn_conv(ea, eb);
                // 2 つの実際のクロージャを単一化する (合流・代入): 実際の効果を和にして残す。
                let merged = self.merge_closure_eff(&a, &b);
                let (xs, ys, rx, ry) = (xs.clone(), ys.clone(), (**rx).clone(), (**ry).clone());
                for (x, y) in xs.iter().zip(&ys) {
                    self.unify2(x, y)?;
                }
                self.unify2(&rx, &ry)?;
                // どちらかが型変数に束縛されていれば、束縛先も効果の和に更新する。
                for opt in [va, vb] {
                    if let Some((x, Some(Ty::Closure { .. }))) = opt {
                        self.vars[x] = Some(merged.clone());
                    }
                }
                Ok(())
            }
            (Ty::Slice(x), Ty::Slice(y)) | (Ty::Slice(x), Ty::Array(y)) | (Ty::Array(x), Ty::Slice(y)) | (Ty::Array(x), Ty::Array(y)) => {
                let (x, y) = (x.clone(), y.clone());
                self.unify2(&x, &y)
            }
            (Ty::Unit, Ty::Unit) => Ok(()),
            // `Vec` は配列・スライスと別の型 (4.2: 暗黙の変換は `&[T; N]` → `&[T]` のみ)。
            (Ty::Named(n, _), Ty::Array(_) | Ty::Slice(_)) | (Ty::Array(_) | Ty::Slice(_), Ty::Named(n, _)) if n == "alloc.vec.Vec" => {
                Err((a.clone(), b.clone()))
            }
            // 構造が確定していて種類が違う組 (`()` と `Result<..>`、整数とタプルなど) は矛盾 (fail-closed)。
            // `Vec` / `String` はスライス・`str` として扱われる場所があるので、ここでは対象にしない。
            (x, y) if Self::concrete_kind(x).is_some() && Self::concrete_kind(y).is_some() && Self::concrete_kind(x) != Self::concrete_kind(y) => {
                Err((a.clone(), b.clone()))
            }
            _ => Ok(()), // それ以外の構造の違いは、近似の限界かもしれないので何も言わない
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
            Ty::Ptr(m, x) => Ty::Ptr(*m, Box::new(self.inst(x, map))),
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
            // 基本型のメソッド表は規範のシグネチャ (`spec/stdlib/core/{num,bool,char}.sig`) が全て。
            // 実行系が受け付けても、シグネチャにないメソッドは E1108 (正は `.sig`)。
            Ty::Prim(p) if p == "bool" => (vec!["core.bool.bool".into()], true),
            Ty::Prim(p) if p == "char" => (vec!["core.char.char".into()], true),
            Ty::Prim(p) => (vec![format!("core.num.{}", p)], true),
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
    fn log_step(&mut self, pos: usize, seg: usize, field: bool, name: &str, recv: &Ty) {
        if pos != 0 {
            self.recv_log.push(RecvSite { writer: self.cur, pos, seg, field, recv: recv.clone(), name: name.to_string() });
        }
    }

    fn log_recv(&mut self, f: &[V], at: usize, field: bool, name: &str, recv: &Ty) {
        if let Some(V::N(p)) = f.get(at) {
            if let Ok(pos) = p.parse::<usize>() {
                self.recv_log.push(RecvSite { writer: self.cur, pos, seg: usize::MAX, field, recv: recv.clone(), name: name.to_string() });
            }
        }
    }

    /// `local` は局所変数から始まるパスの呼び出し (`s.m()` / `s.f.m()`) か。その形は従来の効果検査を
    /// 通らない (パスとして構文解析される) ので、型で解決できたときに実体化した効果全体を検査する。
    /// それ以外 (`x.f().m()` などの MCall) は従来の名前ベースの検査が宣言どおりの効果を見るので、
    /// クロージャから推論した部分だけを検査する。
    fn call_method(&mut self, recv: &Ty, base: Option<&str>, name: &str, args: &[Ty], line: usize, targs: &[V], local: bool) -> Ty {
        let loose = std::mem::take(&mut self.arg_loose);
        // トレイトオブジェクト (5.1): `&dyn Tr` のメソッド呼び出しは、Tr とそのスーパートレイトの
        // シグネチャで検査する (Self は `dyn Tr` 自身)。
        let deep = self.deep(recv);
        if let Ty::Dyn(tr, ta) = deep.strip().clone() {
            return self.dyn_method(&tr, &ta, &deep, base, name, args, line);
        }
        // `Box<dyn Tr, A>` のレシーバは、`Tr` のメソッドを探す (自動参照外し。4.2 / 5.1)。`Box` 自身のメソッド
        // (`as_ref` / `as_mut` / `into_inner`) は、型の側の定義を使う。
        if let Ty::Named(n, a) = deep.strip().clone() {
            if n == "alloc.boxed.Box" && !matches!(name, "as_ref" | "as_mut" | "into_inner" | "drop") {
                if let Some(Ty::Dyn(tr, ta)) = a.first().map(|x| self.deep(x)) {
                    return self.dyn_method(&tr, &ta, &Ty::Dyn(tr.clone(), ta.clone()), base, name, args, line);
                }
            }
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
            // 整数の `to_<整数型>` は廃止された変換名 (4.5)。整数どうしの変換は `from_*` / `try_from_*` で書く。
            if let Ty::Prim(p) = deep.strip() {
                if let Some(msg) = self.int_to_hint(p, name) {
                    self.quiet_here();
                    self.diag("E1108", msg, line);
                    escape_all(self);
                    return Ty::Unknown;
                }
            }
            if self.cmp_trait_missing(recv, name, line) {
                escape_all(self);
                return Ty::Unknown;
            }
            if let Some(t) = self.trait_bound_method(recv, name, args, line) {
                return t;
            }
            // 型パラメータのレシーバ: 境界 (とそのスーパートレイト) が宣言するメソッドだけを呼べる。
            // 境界がなければ、何の型か分からないので呼べない (導出されたメソッドも境界が要る)。
            if let Ty::Param(pn) = deep.strip() {
                if pn != "Self" {
                    let mut tr: Vec<String> = self
                        .p
                        .types
                        .iter()
                        .filter(|(_, &i)| self.p.defs[i].kw == "trait" && self.p.defs[i].trait_sigs.iter().any(|s| s.0 == name))
                        .map(|(k, _)| k.clone())
                        .collect();
                    tr.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
                    let hint = match tr.first() {
                        Some(t) => format!("; add a bound that declares it, for example `{}: {}`", pn, t),
                        None => "; no trait declares a method with that name".to_string(),
                    };
                    self.quiet_here();
                    self.diag("E1108", format!("type parameter `{}` has no method `{}` through its bounds{}", pn, name, hint), line);
                    escape_all(self);
                    return Ty::Unknown;
                }
            }
            if let Some(t) = self.derived_method(recv, name, args) {
                return t;
            }
            let is_prim = matches!(deep.strip(), Ty::Prim(_));
            let exists_elsewhere = self.p.method_idx.contains_key(name) || is_prim;
            if complete && exists_elsewhere && !DERIVED.contains(&name) && !self.trait_methods.contains(name) {
                let mut names: Vec<&String> = owners.iter().filter_map(|o| self.owners.get(o)).flat_map(|m| m.keys()).collect();
                names.sort();
                names.dedup();
                let best = names
                    .iter()
                    .map(|n| (edit_distance(name, n), *n))
                    .filter(|(d, n)| *d <= name.len().max(n.len()) / 3 || n.contains(name) || name.contains(n.as_str()))
                    .min_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(b.1)));
                let hint = best.map(|(_, n)| format!("; did you mean `{}`?", n)).unwrap_or_default();
                let shown = self.deep(recv).strip().show();
                self.quiet_here();
                self.diag("E1108", format!("type `{}` has no method `{}`{}", shown, name, hint), line);
            }
            // 型が決まらない数値リテラルのメソッド呼び出し (`let a = 9.0; a.sqrt()`)。レシーバの型変数は escape
            // しないので、呼び出しの後で型が決まればそれに従い、決まらなければ finish_literals が E2027 にする。
            if matches!(deep.strip(), Ty::Var(v) if self.kinds[*v] != 0) {
                for a in args {
                    self.escape(a);
                }
                return Ty::Unknown;
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
        // 効果検査 (check.rs) のため、型で一意に解決できたメソッド呼び出しを記録する。
        // メソッド表が完全で、引数の個数が合う候補がちょうど 1 つのときだけ (不確かなときは黙る)。
        if fit.len() == 1 && complete {
            self.resolved_calls.push(ResolvedCall {
                writer: self.cur,
                scope: self.closure_nodes.last().cloned(),
                callee: di,
                line,
                targs: targs.to_vec(),
                dyn_eff: None,
            });
        }
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
        seed_effects(d, &mut map);
        let self_ty = from_v(&d.params[0].1, &gn);
        let self_ty = self.inst(&self_ty, &mut map);
        let view = self.view_for(recv, &owner);
        let _ = self.unify2(&self_ty, &view);
        for (k, a) in args.iter().enumerate() {
            let raw_pt = from_v(&d.params[k + 1].1, &gn);
            let pt = self.inst(&raw_pt, &mut map);
            if let Some((want, got)) = self.arg_ref_conflict(&raw_pt, a, loose.get(k).copied().unwrap_or(false)) {
                self.diag("E2023", format!("argument {} of `{}`: expected `{}`, found `{}`{}", k + 1, name, want, got, ref_hint(&want, &got, self.ty_copy_ref(&raw_pt))), line);
            }
            if let Err(e) = self.unify2(&pt, a) {
                if fit.len() == 1 {
                    if let Some((want, got)) = self.prim_conflict(&e).or_else(|| self.ptr_mismatch(&pt, a)) {
                        self.diag(
                            "E2023",
                            format!("argument {} of `{}`: expected `{}`, found `{}`", k + 1, name, want, got),
                            line,
                        );
                    }
                }
            }
        }
        self.resolve_impl_bounds(d, &mut map);
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
        let r = self.inst(&ret, &mut map);
        self.box_hooks(d, &map, line);
        r
    }

    /// 整数型・浮動小数型 `recv` の `to_<整数型>` 呼び出しへの E1108 の文面。対象が整数型名のときだけ Some。
    /// 整数どうしは、幅が広がって失われない変換 `u32.from_u8(x)` (sig にあれば) か、それ以外 `u8.try_from_u32(x)` (Result) を示す。
    /// 浮動小数は切り捨てて範囲外を None にする `x.trunc_to_u32()` (Option) を示す。機械適用はしない (型が Option に変わる)。
    /// 同じ型への `to_` は変換が要らない。文面の形は fixes.rs が機械的に読むので変えるときは合わせる。
    fn int_to_hint(&self, recv: &str, name: &str) -> Option<String> {
        let target = name.strip_prefix("to_")?;
        if !INTS.contains(&target) {
            return None;
        }
        if FLOATS.contains(&recv) {
            return Some(format!(
                "unknown method `{}` on `{}`; use `x.trunc_to_{}()` (returns Option: truncates toward zero, None if out of range or NaN)",
                name, recv, target
            ));
        }
        if !INTS.contains(&recv) {
            return None;
        }
        if target == recv {
            return Some(format!("unknown method `{}` on `{}`; the value is already `{}`, so no conversion is needed", name, recv, recv));
        }
        let from = format!("core.num.{}.from_{}", target, recv);
        if self.p.funcs.contains_key(&from) || self.p.sigs.contains_key(&from) {
            Some(format!("unknown method `{}` on `{}`; use `{}.from_{}(x)` (lossless)", name, recv, target, recv))
        } else {
            Some(format!("unknown method `{}` on `{}`; use `{}.try_from_{}(x)` (returns Result)", name, recv, target, recv))
        }
    }

    /// `Ord` の提供メソッド (`min` / `max`) を持つか。基本型と未確定の型は黙って通す (保守側)。
    /// 利用者の型・標準の型は `derive` に `Ord` があるときだけ持つ。
    fn ord_provides(&self, t: &Ty) -> bool {
        match t {
            Ty::Named(n, _) => self.p.types.get(n).map_or(true, |&i| self.p.defs[i].derive.iter().any(|x| x == "Ord")),
            _ => true,
        }
    }

    /// 比較のメソッド (`eq` / `cmp` / `partial_cmp`) または比較演算子 (`==` `<` など) を、受け手の利用者型・標準の型が
    /// 対応するトレイトを持たないのに使ったとき E1108 (true を返す)。宣言 (`p.types`) のある型だけを見る。
    /// 型変数・型パラメータ・宣言のない型は黙る。Vec / String は 4.2 の表どおり比較のトレイトを持たない。
    fn cmp_trait_missing(&mut self, recv: &Ty, name: &str, line: usize) -> bool {
        // 浮動小数は `Partial*` だけを実装し、`Eq` / `Ord` / `Hash` は実装しない (4.3)。
        if let Ty::Prim(p) = self.deep(recv).strip() {
            if matches!(p.as_str(), "f32" | "f64") && matches!(name, "cmp" | "hash") {
                let tr = if name == "cmp" { "core.cmp.Ord" } else { "core.hash.Hash" };
                let msg = format!("unknown method `{}` on `{}`: floating-point types do not implement `{}` (use `partial_cmp`)", name, p, tr);
                self.diag("E1108", msg, line);
                return true;
            }
            return false;
        }
        let Some(tr) = cmp_trait(name) else { return false };
        let whole = self.deep(recv).strip().clone();
        let Some(leaf) = self.unsatisfied(&whole, tr) else { return false };
        let ty = match &whole {
            Ty::Named(n, _) if leaf == whole => n.clone(),
            _ => whole.show(),
        };
        let hint = if leaf == whole {
            "derive it or write an impl".to_string()
        } else {
            let short = tr.rsplit('.').next().unwrap_or(tr);
            format!("`{}` must implement it too: add `derive [{}]` to `{}`", leaf.show(), short, leaf.show())
        };
        let msg = if matches!(name, "eq" | "cmp" | "partial_cmp") {
            format!("unknown method `{}` on `{}`: the type does not implement `{}` ({})", name, ty, tr, hint)
        } else {
            format!("operator `{}` on `{}`: the type does not implement `{}` ({})", name, ty, tr, hint)
        };
        self.quiet_here();
        self.diag("E1108", msg, line);
        true
    }

    /// レシーバが型パラメータ `T` で、境界のトレイトがそのメソッドを宣言しているとき、トレイトのシグネチャで呼び出しを検査する。
    /// 比較演算子の被演算子が型パラメータのとき、境界が比較のトレイトを含むこと (4.3)。
    /// `==` `!=` は `core.cmp.PartialEq`、`<` などは `core.cmp.PartialOrd` (それらを `requires` する
    /// トレイトでもよい)。境界のトレイトが見つからない・`requires` を持つユーザー定義のときは黙る。
    fn check_cmp_bound(&mut self, operand: &Ty, op: &str, line: usize) {
        let deep = self.deep(operand);
        let Ty::Param(pn) = deep.strip().clone() else { return };
        let d = self.def();
        let Some(g) = d.generics.iter().find(|g| g.0 == "type" && g.1 == pn) else { return };
        let bounds: Vec<String> = items(&g.2).iter().filter_map(|b| if let V::L(bx) = b { Some(st(&bx[0])) } else { None }).collect();
        let eq = op == "==" || op == "!=";
        let need = if eq { "core.cmp.PartialEq" } else { "core.cmp.PartialOrd" };
        let mut seen: HashSet<String> = HashSet::new();
        let mut todo = bounds;
        while let Some(tr) = todo.pop() {
            if !seen.insert(tr.clone()) {
                continue;
            }
            if tr == need {
                return;
            }
            match self.p.types.get(&tr) {
                Some(&ti) => todo.extend(self.p.defs[ti].requires.iter().map(|r| r.0.clone())),
                // 解決できないトレイト名: 分からないので黙る。
                None => return,
            }
        }
        self.diag(
            "E2030",
            format!("`{}` on values of type parameter `{}` requires the bound `{}: {}<{}>`", op, pn, pn, need, pn),
            line,
        );
    }

    fn trait_bound_method(&mut self, recv: &Ty, name: &str, args: &[Ty], line: usize) -> Option<Ty> {
        let deep = self.deep(recv);
        let Ty::Param(pn) = deep.strip().clone() else { return None };
        let d = self.def();
        // 境界 (トレイト名, 型実引数) の列。スーパートレイト (`requires`) も推移的に辿る。
        let gn_caller = d.gnames();
        let mut queue: Vec<(String, Vec<Ty>, Vec<V>)> = vec![];
        for g in d.generics.iter().filter(|g| g.0 == "type" && g.1 == pn) {
            for b in items(&g.2) {
                if let V::L(bx) = b {
                    let targs: Vec<Ty> = items(tel(b, 1)).iter().filter(|a| matches!(a, V::L(_)) && tag(a) != "life").map(|a| bound_targ(a, &gn_caller)).collect();
                    queue.push((st(&bx[0]), targs, items(tel(b, 1)).to_vec()));
                }
            }
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut qi = 0;
        while qi < queue.len() {
            let (tr, targs, bargs) = queue[qi].clone();
            qi += 1;
            if !seen.insert(tr.clone()) {
                continue;
            }
            let Some(&ti) = self.p.types.get(&tr) else { continue };
            let td = &self.p.defs[ti];
            let Some((_, params, ret)) = td.trait_sigs.iter().find(|s| s.0 == name) else {
                for (sup, sargs) in &td.requires {
                    let Some(fq) = crate::check::resolve_trait_fqn(self.p, sup) else { continue };
                    let tgn = td.gnames();
                    let sa: Vec<Ty> = items(sargs).iter().filter(|a| matches!(a, V::L(_)) && tag(a) != "life").map(|a| bound_targ(a, &tgn)).collect();
                    queue.push((fq, sa, items(sargs).to_vec()));
                }
                continue;
            };
            let eff = Self::trait_effects_inst(td, name, &bargs);
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
            seed_effects(td, &mut map);
            // トレイトの型パラメータ (`Fn<Args, R, ..>` の `R` など) を、境界に書かれた実引数に束縛する。
            // 実引数の `Self` (スーパートレイトの `PartialEq<Self>`) は、レシーバの型。
            let tparams: Vec<&(String, String, V)> = td.generics.iter().filter(|g| g.0 != "life").collect();
            for (g, a) in tparams.iter().zip(&targs) {
                if g.0 == "type" {
                    let a = if *a == Ty::Param("Self".into()) { deep.clone() } else { a.clone() };
                    map.insert(g.1.clone(), a);
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
            if let Some((u, p)) = eff {
                self.check_trait_effects(format!("{}.{}", tr, name), u, p, line);
            }
            let r = from_v(ret, &gn);
            return Some(self.inst(&r, &mut map));
        }
        None
    }

    /// トレイトを実装する型 (またはその導出) のメソッドで、型の側に定義がないもの
    /// (`clone` / `eq` / `cmp` / `to_string` など) を、トレイトのシグネチャで解く。
    /// `Self` はレシーバの参照を外した型。
    fn derived_method(&mut self, recv: &Ty, name: &str, args: &[Ty]) -> Option<Ty> {
        // `min` / `max` は `Ord` の提供メソッド (仕様 4.3): `Self` を返し、引数は `Self` と単一化する。
        // `Ord` を持たない利用者の型には無い (E1108 は型検査が出す)。
        if matches!(name, "min" | "max") {
            let self_ty = self.deep(recv).strip().clone();
            if !self.ord_provides(&self_ty) {
                return None;
            }
            for a in args {
                let _ = self.unify2(&self_ty, a);
            }
            return Some(self_ty);
        }
        if !DERIVED.contains(&name) || matches!(name, "call" | "call_mut" | "call_once") {
            return None;
        }
        let deep = self.deep(recv);
        let self_ty = deep.strip().clone();
        if matches!(self_ty, Ty::Unknown | Ty::Var(_)) {
            return None;
        }
        let mut found: Vec<&'a Def> = vec![];
        let mut keys: Vec<&String> = self.p.types.keys().collect();
        keys.sort();
        for k in keys {
            let td = &self.p.defs[self.p.types[k]];
            if td.kw == "trait" && td.trait_sigs.iter().any(|s| s.0 == name && s.1.len() == args.len() + 1) {
                found.push(td);
            }
        }
        let td = *found.first()?;
        let (_, params, ret) = td.trait_sigs.iter().find(|s| s.0 == name)?;
        let mut gn = td.gnames();
        gn.insert("Self".to_string());
        let mut map: HashMap<String, Ty> = HashMap::new();
        map.insert("Self".to_string(), self_ty.clone());
        seed_effects(td, &mut map);
        // 比較系のトレイトの `Rhs` は、既定では `Self`。
        for g in td.generics.iter().filter(|g| g.0 == "type") {
            map.entry(g.1.clone()).or_insert_with(|| self_ty.clone());
        }
        for (k, a) in args.iter().enumerate() {
            let pt = from_v(&params[k + 1].1, &gn);
            let pt = self.inst(&pt, &mut map);
            let _ = self.unify2(&pt, a);
        }
        let r = from_v(ret, &gn);
        Some(self.inst(&r, &mut map))
    }

    /// `for x in it` の要素の型。配列・スライス・`Vec` はその要素、それ以外は `next` の戻り値 `Option<T>` の `T`。
    fn iter_elem(&mut self, it: &Ty) -> Ty {
        let deep = self.deep(it);
        match deep.strip() {
            Ty::Array(x) | Ty::Slice(x) => return (**x).clone(),
            Ty::Named(n, a) if n == "alloc.vec.Vec" => return a.first().cloned().unwrap_or(Ty::Unknown),
            _ => {}
        }
        if let Ty::Param(pn) = deep.strip() {
            let d = self.def();
            let gn = d.gnames();
            for g in d.generics.iter().filter(|g| g.0 == "type" && &g.1 == pn) {
                for b in items(&g.2) {
                    if matches!(b, V::L(bx) if st(&bx[0]) == "core.iter.Iterator") {
                        if let Some(a0) = items(tel(b, 1)).first() {
                            return bound_targ(a0, &gn);
                        }
                    }
                }
            }
        }
        let (owners, _) = self.owners_for(&deep);
        for o in &owners {
            let Some(&di) = self.owners.get(o).and_then(|m| m.get("next")).and_then(|v| v.first()) else { continue };
            let d = &self.p.defs[di];
            if d.params.len() != 1 {
                continue;
            }
            let gn = d.gnames();
            let mut map: HashMap<String, Ty> = HashMap::new();
            seed_effects(d, &mut map);
            let self_ty = self.inst(&from_v(&d.params[0].1, &gn), &mut map);
            let view = self.view_for(&deep, o);
            let _ = self.unify2(&self_ty, &view);
            let ret = self.inst(&from_v(&d.ret, &gn), &mut map);
            if let Ty::Named(n, a) = self.deep(&ret) {
                if n == "Option" {
                    return a.first().cloned().unwrap_or(Ty::Unknown);
                }
            }
        }
        Ty::Unknown
    }

    /// トレイトのメソッド `name` の宣言された効果 (`uses` / `panics`) を、境界の実引数 `bargs` で実体化する
    /// (4.3 / 5.1)。トレイトの効果変数 (`E: uses`) は、対応する実引数が具体的な `uses [..]` / `never` / `may` の
    /// ときだけ束縛し、束縛が分からない効果変数は落とす（黙る）。戻り値は具体名だけの uses と、確定した panics。
    /// 宣言の効果が無い (`uses` を書いていない) メソッドは `None`。
    fn trait_effects_inst(td: &Def, name: &str, bargs: &[V]) -> Option<(Vec<String>, Option<String>)> {
        let (_, us, pv) = td.trait_effs.iter().find(|e| e.0 == name)?;
        let mut bind_u: HashMap<String, Vec<String>> = HashMap::new();
        let mut bind_p: HashMap<String, String> = HashMap::new();
        for (k, g) in td.generics.iter().enumerate() {
            let Some(a) = bargs.get(k) else { continue };
            match g.0.as_str() {
                "uses" if tag(a) == "effects" => {
                    bind_u.insert(g.1.clone(), items(tel(a, 1)).iter().map(st).collect());
                }
                "panics" if tag(a) == "panic" => {
                    bind_p.insert(g.1.clone(), st(tel(a, 1)));
                }
                _ => {}
            }
        }
        let concrete = |e: &String| e.chars().next().map_or(false, |c| c.is_lowercase());
        let mut eu: Vec<String> = vec![];
        for e in us {
            if concrete(e) {
                eu.push(e.clone());
            } else if let Some(set) = bind_u.get(e) {
                eu.extend(set.iter().filter(|x| concrete(x)).cloned());
            }
        }
        let ep = match pv.as_str() {
            "never" | "may" => Some(pv.clone()),
            v => bind_p.get(v).cloned(),
        };
        Some((eu, ep))
    }

    /// トレイト経由の呼び出し (境界・`dyn`) の効果を、呼び出し元の宣言と比べる。型の決まらない・推論された
    /// 関数の中・クロージャの本体の中では黙る（クロージャの効果は確実に含まれる効果だけを足すため、
    /// トレイトの宣言の効果 (上限) は足さない）。
    fn check_trait_effects(&mut self, name: String, uses: Vec<String>, panics: Option<String>, line: usize) {
        if self.def().inferred {
            return;
        }
        if !self.eff_acc.is_empty() {
            // クロージャの本体の中: 境界・`dyn` 越しの呼び出しは、宣言の効果 (上限) をそのクロージャの効果に数える
            // (5.1 未宣言の副作用はエラー。確実でないので `unsure`)。効果変数は実体化されるまで数えない。
            let low: BTreeSet<String> =
                uses.iter().filter(|e| e.chars().next().map_or(false, |c| c.is_lowercase())).cloned().collect();
            if let Some(acc) = self.eff_acc.last_mut() {
                acc.0.extend(low);
                if panics.as_deref() == Some("may") {
                    acc.1 = true;
                }
                acc.2 = true;
            }
            return;
        }
        self.record_effects(line, vec![(name, uses.clone(), panics.clone(), uses, panics)]);
    }

    /// `dyn Tr` のメソッド呼び出しを、トレイト (とそのスーパートレイト) のシグネチャで検査する (4.3 / 5.1)。
    /// 引数の個数 (E2022) と型 (E2023) を検査し、戻り値の型を式へ流す。`Self` は `dyn Tr` 自身。
    /// トレイトに無いメソッド名では黙る (名前の存在は名前ベースの従来の検査が見る)。
    fn dyn_method(&mut self, tr: &str, dargs: &TArgs, recv: &Ty, base: Option<&str>, name: &str, args: &[Ty], line: usize) -> Ty {
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
            // 5.1: `dyn Tr<E, P>` 越しの呼び出しの効果は、メソッドの宣言 `uses [E] panics P` を型引数で実体化したもの。
            // 型引数のないトレイトの具体的な効果 (`uses [io.console]`) はそのまま使う。実引数が分からなければ変数のまま
            // (効果検査は未束縛の変数を黙って通す)。
            if let Some((_, u, pn)) = td.trait_effs.iter().find(|e| e.0 == name) {
                let (u2, p2) = crate::check::inst_effects(&td.generics, if cur == root { &dargs.0 } else { &[] }, u, pn);
                // クロージャの本体の中なら、実体化した宣言の効果 (上限) をそのクロージャの効果に数える (5.1)。
                if !self.def().inferred {
                    if let Some(acc) = self.eff_acc.last_mut() {
                        acc.0.extend(u2.iter().filter(|e| e.chars().next().map_or(false, |c| c.is_lowercase())).cloned());
                        if p2 == "may" {
                            acc.1 = true;
                        }
                        acc.2 = true;
                    }
                }
                self.resolved_calls.push(ResolvedCall {
                    writer: self.cur,
                    scope: self.closure_nodes.last().cloned(),
                    callee: ti,
                    line,
                    targs: vec![],
                    dyn_eff: Some((format!("{}.{} (through dyn)", cur, name), u2, p2)),
                });
            }
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
            seed_effects(td, &mut map);
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
            // 効果 (5.1): `dyn Tr` の型引数は `Ty::Dyn` が持たないので、トレイトの効果変数は束縛せず落とす (黙る)。
            // 具体名と `never` / `may` の宣言で検査する。
            let eff = Self::trait_effects_inst(td, name, &[]);
            if let Some((u, p)) = eff {
                self.check_trait_effects(format!("{}.{}", cur, name), u, p, line);
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
                if let Some(leaf) = self.unsatisfied(t, &tr) {
                    let mut msg = format!("the trait bound `{}: {}` of {} is not satisfied by {}", g.1, tr, d.name, t.show());
                    if leaf != *t {
                        let short = tr.rsplit('.').next().unwrap_or(&tr);
                        msg.push_str(&format!(
                            ": `{}` is not `{}` (the type arguments of a derived impl must satisfy it too); add `derive [{}]` to `{}`",
                            leaf.show(),
                            short,
                            short,
                            leaf.show()
                        ));
                    }
                    self.diag("E2030", msg, line);
                }
            }
        }
    }

    /// `alloc.boxed.Box.into_dyn` (変換先と `T: Tr` は関数の推論が終わってから検査する) と、
    /// `dyn` の `into_inner` (E2046) の印を付ける (5.1)。
    fn box_hooks(&mut self, d: &Def, map: &HashMap<String, Ty>, line: usize) {
        match d.name.as_str() {
            "alloc.boxed.Box.into_dyn" => {
                if let (Some(t), Some(dd)) = (map.get("T"), map.get("D")) {
                    self.unsize.push((t.clone(), dd.clone(), line));
                }
            }
            "alloc.boxed.Box.into_inner" => {
                if let Some(t) = map.get("T") {
                    if matches!(self.deep(t), Ty::Dyn(..)) {
                        self.diag("E2046", "`into_inner` cannot move a `dyn` value out of a `Box`".into(), line);
                    }
                }
            }
            _ => {}
        }
    }

    /// `Box.into_dyn` の変換先と元の型を検査する (関数の推論が終わってから)。
    fn check_unsize(&mut self) {
        for (t, d, line) in std::mem::take(&mut self.unsize) {
            let (t, d) = (self.deep(&t), self.deep(&d));
            let Ty::Dyn(tr, _) = &d else {
                if matches!(d, Ty::Named(..) | Ty::Prim(_) | Ty::Tuple(_)) {
                    self.diag("E2046", format!("`Box.into_dyn` must produce a `Box<dyn ..>`, found `{}`", d.show()), line);
                }
                continue;
            };
            let Some(root) = crate::check::resolve_trait_fqn(self.p, tr) else { continue };
            let Ty::Named(n, _) = t.strip() else { continue };
            let Some(&i) = self.p.types.get(n) else { continue };
            if !self.p.impls.iter().any(|(tb, tgt)| tgt == n && crate::check::resolve_trait_fqn(self.p, tb).as_deref() == Some(root.as_str())) {
                self.diag("E2046", format!("`{}` does not implement the trait `{}`", n, root), line);
            }
            let def = &self.p.defs[i];
            let has_ref = def.fields.iter().any(|f| v_has_ref(&f.1)) || def.variants.iter().any(|v| v.1.iter().any(v_has_ref));
            if has_ref {
                self.diag("E2046", format!("`{}` holds a reference, so it cannot be boxed as `dyn {} + 'static`", n, root), line);
            }
        }
    }

    /// 型の中に未解決の型変数があるか。
    fn has_var(&self, t: &Ty) -> bool {
        match self.deep(t) {
            Ty::Var(_) => true,
            Ty::Named(_, a) | Ty::Tuple(a) => a.iter().any(|x| self.has_var(x)),
            Ty::Ref(_, x) | Ty::Array(x) | Ty::Slice(x) => self.has_var(&x),
            Ty::Closure { args, ret, .. } => args.iter().any(|x| self.has_var(x)) || self.has_var(&ret),
            _ => false,
        }
    }

    /// 型パラメータの境界 `T: Tr<A..>` (`Fn` 系と marker を除く) の型引数が、まだ未解決の型変数を含むとき、
    /// `T` の具体型 (名前付きの型) が実装する `impl Tr<B..> for T'` の `B..` と `A..` を単一化する。
    /// 例: `core.iter.enumerate(it)` の `T` は `I: Iterator<T, E, P>` の `I` (`Range<i32>`) の impl
    /// (`Iterator<T, [], never> for Range<T>`) から決まる。候補が 1 つでないときや単一化の失敗は黙る。
    fn resolve_impl_bounds(&mut self, d: &Def, map: &mut HashMap<String, Ty>) {
        let p: &'a Program = self.p;
        let gn = d.gnames();
        for g in &d.generics {
            if g.0 != "type" {
                continue;
            }
            let Some(recv) = map.get(&g.1).cloned() else { continue };
            let recv = self.deep(&recv);
            let Ty::Named(rn, _) = recv.strip().clone() else { continue };
            for b in items(&g.2) {
                let V::L(bx) = b else { continue };
                let tr = st(&bx[0]);
                if tr.starts_with("core.ops.Fn") || tr == "core.marker.Send" || tr == "core.marker.Sync" {
                    continue;
                }
                let targs = items(tel(b, 1));
                if targs.is_empty() {
                    continue;
                }
                let mut want = vec![];
                for a in targs {
                    let t = from_v(a, &gn);
                    want.push(self.inst(&t, map));
                }
                if !want.iter().any(|t| self.has_var(t)) {
                    continue;
                }
                let cands: Vec<usize> = p
                    .defs
                    .iter()
                    .enumerate()
                    .filter(|(_, x)| x.kw == "impl" && x.impl_trait == tr && impl_head(&x.impl_for).as_deref() == Some(rn.as_str()))
                    .map(|(i, _)| i)
                    .collect();
                if cands.len() != 1 {
                    continue;
                }
                let imp = &p.defs[cands[0]];
                let ign = imp.gnames();
                let mut im: HashMap<String, Ty> = HashMap::new();
                seed_effects(imp, &mut im);
                let for_t = from_v(&imp.impl_for, &ign);
                let for_t = self.inst(&for_t, &mut im);
                if self.unify2(&for_t, &recv).is_err() {
                    continue;
                }
                for (w, a) in want.iter().zip(items(&imp.impl_targs)) {
                    let it = from_v(a, &ign);
                    let it = self.inst(&it, &mut im);
                    let _ = self.unify2(w, &it);
                }
            }
        }
    }

    /// 型 `t` がトレイト `tr` を満たさないとき、満たさない原因の型を返す。`derive` で得た実装は
    /// 型引数 (`T: Tr`) の成り立ちに還元する (`Option<T>: PartialEq` ⇔ `T: PartialEq`)。タプル・配列・
    /// スライス・参照は中身に還元する。型パラメータ・未確定の型・手書きの `impl` は黙って通す。
    fn unsatisfied(&self, t: &Ty, tr: &str) -> Option<Ty> {
        match t {
            Ty::Named(n, args) => {
                let n = &match n.as_str() {
                    "Option" => "core.option.Option".to_string(),
                    "Result" => "core.result.Result".to_string(),
                    _ => n.clone(),
                };
                let &i = self.p.types.get(n)?;
                if !self.implements(n, tr) {
                    return Some(t.clone());
                }
                if self.p.impls.contains(&(tr.to_string(), n.clone())) {
                    return None;
                }
                let gs: Vec<&(String, String, V)> = self.p.defs[i].generics.iter().filter(|g| g.0 != "life").collect();
                if gs.len() != args.len() {
                    return None;
                }
                gs.iter().zip(args).filter(|(g, _)| g.0 == "type").find_map(|(_, a)| self.unsatisfied(self.deep(a).strip(), tr))
            }
            Ty::Prim(p) if p == "f32" || p == "f64" => {
                matches!(tr, "core.cmp.Eq" | "core.cmp.Ord" | "core.hash.Hash").then(|| t.clone())
            }
            Ty::Tuple(ts) => ts.iter().find_map(|x| self.unsatisfied(self.deep(x).strip(), tr)),
            Ty::Array(x) | Ty::Slice(x) => self.unsatisfied(self.deep(x).strip(), tr),
            _ => None,
        }
    }

    fn implements(&self, ty: &str, tr: &str) -> bool {
        let Some(&i) = self.p.types.get(ty) else { return true };
        if self.p.impls.contains(&(tr.to_string(), ty.to_string())) {
            return true;
        }
        // `Clone` を実装する型は `TryClone` も持つ (確保なしの複製は確保ありの複製でもある)。
        if tr == "alloc.alloc.TryClone" && self.implements(ty, "core.ops.Clone") {
            return true;
        }
        self.p.defs[i].derive.iter().any(|x| crate::check::DERIVE_TRAITS.iter().any(|(k, v)| k == x && *v == tr))
    }

    // ------------------------------------------------------------ 式
    /// 式を推論する。計測が有効なら結果を記録する (`cov_record`)。本体は `infer_node`。
    fn infer(&mut self, e: &V) -> Ty {
        let saved = self.cur_line;
        if nline(e) != 0 {
            self.cur_line = nline(e);
        }
        let t = self.infer_site(e);
        self.cur_line = saved;
        t
    }

    fn infer_site(&mut self, e: &V) -> Ty {
        if !self.strict {
            return self.infer_traced(e);
        }
        let id = self.new_site(nline(e), self.describe(e), Ty::Unknown);
        self.site_stack.push(id);
        let t = self.infer_traced(e);
        self.site_stack.pop();
        self.sites[id].ty = t.clone();
        self.last_site = Some(id);
        t
    }

    fn infer_traced(&mut self, e: &V) -> Ty {
        let t = self.infer_inner(e);
        let id = crate::parser::nid(e);
        if id != 0 {
            self.expr_pending.insert(id, (nname(e).to_string(), nline(e), t.clone()));
        }
        if self.cov.is_some() {
            self.cov_record(e, &t);
        }
        t
    }

    fn infer_inner(&mut self, e: &V) -> Ty {
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
            "Path" => {
                self.path_pos = nline(e);
                let t = self.path(&strs(&f[0]));
                self.path_pos = 0;
                t
            }
            "Call" => self.call(&f[0], items(&f[1]), nline(e), items(&f[2])),
            "MCall" => {
                let recv = self.infer(&f[0]);
                self.log_recv(f, 4, false, &st(&f[1]), &recv);
                let args: Vec<Ty> = items(&f[2]).iter().map(|a| self.infer(a)).collect();
                let base = self.base_local(&f[0]);
                self.arg_loose = items(&f[2]).iter().map(|a| self.root_loose(a)).collect();
                self.call_method(&recv, base.as_deref(), &st(&f[1]), &args, nline(e), items(&f[3]), false)
            }
            "Field" => {
                let r = self.infer(&f[0]);
                self.log_recv(f, 2, true, &st(&f[1]), &r);
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
                    Ty::Named(n, a) if n == "alloc.vec.Vec" => a.first().cloned().unwrap_or(Ty::Unknown),
                    _ => Ty::Unknown,
                }
            }
            "Unary" => {
                let t = self.infer(&f[1]);
                if st(&f[0]) == "-" && self.is_int_prim(&t) {
                    if let Some(acc) = self.eff_acc.last_mut() {
                        acc.1 = true;
                    }
                }
                match st(&f[0]).as_str() {
                    "&" => Ty::Ref(false, Box::new(t)),
                    "&mut" => Ty::Ref(true, Box::new(t)),
                    // 参照は透過的なので、参照でない型の `*` も同じ型 (型が決まらないときは Unknown)。
                    "*" => match self.resolve(&t) {
                        Ty::Ref(_, x) => *x,
                        Ty::Unknown => Ty::Unknown,
                        other => other,
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
                // 整数の算術はオーバーフロー・0 除算で panic しうる (クロージャの本体では `panics may` の根拠にする)。
                // 型が整数と決まっているときだけ数える (浮動小数・型が分からないものは数えない)。
                if matches!(op.as_str(), "+" | "-" | "*" | "/" | "%") && (self.is_int_prim(&a) || self.is_int_prim(&b)) {
                    if let Some(acc) = self.eff_acc.last_mut() {
                        acc.1 = true;
                    }
                }
                // 比較演算子は PartialEq / PartialOrd に対応する (3.2 / 4.3)。受け手の型が持たなければ E1108 (メソッド形と揃える)。
                self.cmp_trait_missing(&a, &op, nline(e));
                match op.as_str() {
                    "==" | "!=" | "<" | "<=" | ">" | ">=" => {
                        self.check_cmp_bound(&a, &op, nline(e));
                        Ty::Prim("bool".into())
                    }
                    // 算術演算の結果は値 (参照は透過的に扱うので、左辺の参照を外す)。
                    _ => self.deep(&a).strip().clone(),
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
                        if let Some((want, got)) = self.type_conflict(&err).or_else(|| self.vec_seq_mismatch(&declared, &vt)) {
                            self.diag(
                                "E2023",
                                format!("field `{}` of `{}`: expected `{}`, found `{}`", st(&items(x)[0]), fq, want, got),
                                nline(e),
                            );
                        }
                    }
                    // 構造体の欄は「受け側」の関数型が分かる: 宣言 × 宣言の効果の食い違いをここで報告する。
                    self.check_fn_conv_dir(&declared, &vt);
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
                // 配列リテラルでクロージャが合流する: 実際の効果を和にして要素型に残す。
                if let Ty::Var(v) = &elem {
                    let mut merged: Option<Ty> = None;
                    for t in &ts {
                        merged = Some(match merged {
                            None => t.clone(),
                            Some(m) => self.merge_closure_eff(&m, t),
                        });
                    }
                    if let Some(m) = merged {
                        self.vars[*v] = Some(m);
                    }
                }
                Ty::Array(Box::new(elem))
            }
            "If" => {
                self.infer(&f[0]);
                let mut a = self.infer(&f[1]);
                let has_else = !is_nil(&f[2]);
                let b = if has_else { self.infer(&f[2]) } else { Ty::Unit };
                if a == Ty::Never {
                    return b;
                }
                // 6 の 2: 両方の枝は同じ型になる。`Ok(..)` と `Err(..)` の枝の型変数は、互いに一致させて決まる。
                if has_else && b != Ty::Never {
                    a = self.join_arm(&a, &b);
                }
                a
            }
            "Match" => {
                let s = self.infer(&f[0]);
                let src = self.last_site;
                let mut out = Ty::Unknown;
                for arm in items(&f[1]) {
                    self.scopes.push(HashMap::new());
                    self.bind_pat_from(&items(arm)[0], &s, src, nline(&items(arm)[0]));
                    self.note_pat_binds(&items(arm)[0], nline(&items(arm)[0]));
                    let t = self.infer(&items(arm)[1]);
                    self.scopes.pop();
                    if t == Ty::Never {
                        continue;
                    }
                    if out == Ty::Unknown {
                        out = t;
                    } else {
                        // 6 の 2: 他の腕と型を一致させる (`Some(v) => Ok(v)` と `None => Err(a)` など)。
                        out = self.join_arm(&out, &t);
                    }
                }
                // 腕がすべて発散するなら、match 自身も発散する。
                if out == Ty::Unknown && !items(&f[1]).is_empty() { Ty::Never } else { out }
            }
            "For" => {
                let it = self.infer(&f[1]);
                let src = self.last_site;
                self.scopes.push(HashMap::new());
                let elem = self.iter_elem(&it);
                self.bind_pat_from(&f[0], &elem, src, nline(e));
                self.note_pat_binds(&f[0], nline(e));
                self.loop_breaks.push(false);
                self.infer(&f[2]);
                self.loop_breaks.pop();
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
                // 節名 → クロージャ AST を覚える (E4012 が引数と本体の `let` からフレームを見積る)。
                self.closure_asts.insert(node.clone(), e.clone());
                self.closure_nodes.push(node.clone());
                self.eff_acc.push(Default::default());
                self.scopes.push(HashMap::new());
                // `&a` / `&mut a` で取り込んだ名前は、本体では `&T` / `&mut T` (move は `T` のまま)。
                for c in items(&f[2]) {
                    let (Some(V::S(k)), Some(V::S(n))) = (items(c).first(), items(c).get(1)) else { continue };
                    if k == "move" {
                        continue;
                    }
                    if let Some(t) = self.lookup(n) {
                        // Copy な型 (整数など) は、読むだけなら値のままで害がない。移せる (Copy でない) 型だけ参照にする。
                        if self.ty_copy(&t) == Some(false) {
                            let r = Ty::Ref(k == "&mut", Box::new(t));
                            if let Some(s) = self.scopes.last_mut() {
                                s.insert(n.clone(), r);
                            }
                        }
                    }
                }
                for p in items(&f[0]) {
                    let ty = from_v(&items(p)[1], &gn);
                    self.bind(&st(&items(p)[0]), ty.clone());
                    args.push(ty);
                }
                let ret = from_v(&f[1], &gn);
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
                let ceff = Eff(Some(EffInfo {
                    decl: false,
                    uses: effs.0.iter().cloned().collect(),
                    may: Some(effs.1),
                    name: "the closure".into(),
                }));
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
                let cap = self.closure_cap(&f[2], &f[3]);
                Ty::Closure { args, ret: Box::new(ret), cap, id: Some(node), eff: ceff }
            }
            "Return" | "Break" | "Continue" => {
                if nname(e) == "Break" {
                    if let Some(b) = self.loop_breaks.last_mut() {
                        *b = true;
                    }
                }
                if nname(e) == "Return" && !is_nil(&f[0]) {
                    let v = self.infer(&f[0]);
                    // 4.3: `return` の値は、いま囲んでいる関数・クロージャの戻り値型と比べる (末尾の値と同じ規則)。
                    // `rets` は関数・クロージャの入れ子に合わせて積まれている。
                    if let Some(ret) = self.rets.last().cloned() {
                        if let Err(err) = self.unify2(&ret, &v) {
                            if let Some((want, got)) = self.ann_conflict(&ret, &v, &err) {
                                self.diag(
                                    "E2020",
                                    format!("type mismatch in return value: expected {}, found {}{}", want, got, conv_hint(&want, &got)),
                                    nline(e),
                                );
                            }
                        }
                        if let Some((want, got)) = self.ref_level_conflict(&ret, &v) {
                            self.diag("E2020", format!("type mismatch in return value: expected {}, found {}{}", want, got, self.deref_hint(&ret)), nline(e));
                        }
                    }
                }
                Ty::Never
            }
            "While" => {
                self.infer(&f[0]);
                self.loop_breaks.push(false);
                self.infer(&f[1]);
                self.loop_breaks.pop();
                Ty::Unit
            }
            "Loop" => {
                self.loop_breaks.push(false);
                self.infer(&f[0]);
                // `break` のない `loop` は戻らない。
                if self.loop_breaks.pop() == Some(true) { Ty::Unit } else { Ty::Never }
            }
            "Unsafe" => self.infer(&f[2]),
            // `with 名前 = 初期化式 close 後始末式 { 本体 }` (4.4): 値は本体の値。後始末式は `()` でなければならない。
            "With" => {
                let init = self.infer(&f[2]);
                let src = self.last_site;
                self.scopes.push(HashMap::new());
                if matches!(f[1], V::B(true)) {
                    self.mutable.insert(st(&nf(&f[0])[0]));
                }
                self.bind_pat_from(&f[0], &init, src, nline(e));
                if let Some(t) = self.lookup(&st(&nf(&f[0])[0])) {
                    self.let_pending.push((nline(e), st(&nf(&f[0])[0]), t));
                }
                let body = self.infer(&f[4]);
                let cl = self.infer(&f[3]);
                if let Ty::Named(..) | Ty::Prim(_) | Ty::Tuple(_) | Ty::Ref(..) = self.deep(&cl) {
                    self.diag("E2045", format!("the `close` expression of `with` must have type (), found {}", self.deep(&cl).show()), nline(&f[3]));
                }
                self.scopes.pop();
                body
            }
            "ArrayRep" => {
                let t = self.infer(&f[0]);
                // 長さは `const_expr` (整数・定数名・括弧) で式ではないので、推論にかけない。
                Ty::Array(Box::new(t))
            }
            _ => {
                // その他: 子の式をたどる (パターンは除く)。
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
                let src = self.last_site;
                let gn = self.def().gnames();
                let ty = if !is_nil(&f[2]) {
                    let ann = from_v(&f[2], &gn);
                    if let Err(err) = self.unify2(&ann, &rhs) {
                        if let Some((want, got)) = self.ann_conflict(&ann, &rhs, &err) {
                            self.diag("E2020", format!("type mismatch in let binding: expected {}, found {}{}", want, got, conv_hint(&want, &got)), nline(s));
                        }
                    }
                    // let の注釈は「受け側」の関数型が分かる: 宣言 × 宣言の効果の食い違いをここで報告する。
                    self.check_fn_conv_dir(&ann, &rhs);
                    if let Some((want, got)) = self.ref_level_conflict(&ann, &rhs) {
                        self.diag("E2020", format!("type mismatch in let binding: expected {}, found {}{}", want, got, self.deref_hint(&ann)), nline(s));
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
                self.bind_pat_from(&f[0], &ty, src, nline(s));
                let mut names = vec![];
                pat_binds(&f[0], &mut names);
                for n in names {
                    if let Some(t) = self.lookup(&n) {
                        self.let_pending.push((nline(s), n, t));
                    }
                }
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
                    // 再代入でクロージャが合流する: 実際の効果を和にして束縛を更新する。
                    if matches!(self.resolve(&target), Ty::Closure { .. })
                        && matches!(self.resolve(&value), Ty::Closure { .. })
                    {
                        let merged = self.merge_closure_eff(&target, &value);
                        if let Some(name) = strs(&nf(&f[0])[0]).first().cloned() {
                            for s in self.scopes.iter_mut().rev() {
                                if s.contains_key(&name) {
                                    s.insert(name, merged);
                                    break;
                                }
                            }
                        }
                    }
                }
                Ty::Unit
            }
            _ => self.infer(&f[0]),
        }
    }

    /// `match` の腕・`for` のパターンの束縛の型を `let_tys` に記録する (move の追跡 E3002 が使う)。
    /// 参照の段数が決まらない束縛 (`loose`) は記録しない (黙る)。
    fn note_pat_binds(&mut self, pat: &V, line: usize) {
        let mut names = vec![];
        pat_binds(pat, &mut names);
        for n in names {
            if self.loose.contains(&n) {
                continue;
            }
            if let Some(t) = self.lookup(&n) {
                self.let_pending.push((line, n, t));
            }
        }
    }

    /// 値の出どころの記録 (`src`) と行を覚えてから、パターンを束縛する。
    fn bind_pat_from(&mut self, pat: &V, ty: &Ty, src: Option<usize>, line: usize) {
        let saved = (self.pat_src, self.pat_line);
        self.pat_src = src;
        self.pat_line = line;
        self.bind_pat(pat, ty);
        (self.pat_src, self.pat_line) = saved;
    }

    fn bind_pat(&mut self, pat: &V, ty: &Ty) {
        // 分解パターンが参照を受けたら、中の束縛は外側の参照を落とした型になる (参照の段数は決まらない)。
        if matches!(nname(pat), "PTuple" | "PStruct" | "PVariant") && matches!(self.resolve(ty), Ty::Ref(..)) && !self.pat_loose {
            self.pat_loose = true;
            self.bind_pat_inner(pat, ty);
            self.pat_loose = false;
            return;
        }
        self.bind_pat_inner(pat, ty)
    }

    fn bind_pat_inner(&mut self, pat: &V, ty: &Ty) {
        let f = nf(pat);
        match nname(pat) {
            "PBind" => self.bind(&st(&f[0]), ty.clone()),
            // `&p` パターン: 参照を一段外した型で中身を束縛する。
            "PRef" => {
                let inner = match self.resolve(ty) {
                    Ty::Ref(_, x) => *x,
                    other => other,
                };
                self.bind_pat(&f[0], &inner)
            }
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
            "PStruct" => {
                // `t.Point { x: a, y: b }`: 構造体の宣言から、各フィールドの型を流す。
                let segs = strs(&f[0]);
                let mut fty: Option<(String, Ty)> = None;
                if let Some(r) = self.p.resolve(&segs, &self.def().file) {
                    if let (Target::Type(i), true) = (&r.target, r.rest.is_empty()) {
                        let td = &self.p.defs[*i];
                        if td.kw == "type" {
                            let n = td.generics.iter().filter(|g| g.0 == "type").count();
                            let args: Vec<Ty> = (0..n).map(|_| self.fresh(0)).collect();
                            let t = Ty::Named(td.name.clone(), args);
                            let _ = self.unify2(ty, &t);
                            fty = Some((td.name.clone(), t));
                        }
                    }
                }
                for fp in items(&f[1]) {
                    let fname = st(&items(fp)[0]);
                    let t = match &fty {
                        Some((_, st_ty)) => {
                            let scrut = self.deep(ty);
                            let base = if matches!(scrut.strip(), Ty::Named(..)) { scrut } else { st_ty.clone() };
                            self.field_ty(&base, &fname)
                        }
                        None => Ty::Unknown,
                    };
                    self.bind_pat(&items(fp)[1], &t);
                }
            }
            "PVariant" => {
                let segs = strs(&f[0]);
                self.shape_for_variant(&segs, ty);
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

    /// パターンの列挙子が決める型の形 (`Some(_)` なら `Option<_>`) に、値の型を合わせる。
    /// 値の型がまだ決まっていない (型変数の) ときに、ペイロードの型を取り出せるようにする。
    fn shape_for_variant(&mut self, segs: &[String], ty: &Ty) {
        let name = segs.last().cloned().unwrap_or_default();
        let want = if segs.len() == 1 && matches!(name.as_str(), "Some" | "None") {
            let v = self.fresh(0);
            Some(Ty::Named("Option".into(), vec![v]))
        } else if segs.len() == 1 && matches!(name.as_str(), "Ok" | "Err") {
            let (a, b) = (self.fresh(0), self.fresh(0));
            Some(Ty::Named("Result".into(), vec![a, b]))
        } else if let Some(r) = self.p.resolve(segs, &self.def().file) {
            match r.target {
                Target::Variant(en, _) if self.p.types.contains_key(&en) => {
                    let n = self.type_def(&en).map_or(0, |td| td.generics.iter().filter(|g| g.0 == "type").count());
                    let args: Vec<Ty> = (0..n).map(|_| self.fresh(0)).collect();
                    Some(Ty::Named(en, args))
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(w) = want {
            let _ = self.unify2(&w, ty);
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
            self.dep_on_local(&segs[0]);
            let pp = std::mem::take(&mut self.path_pos);
            for (k, s) in segs[1..].iter().enumerate() {
                self.log_step(pp, k + 1, true, s, &t);
                t = self.field_ty(&t, s);
            }
            return t;
        }
        match self.p.resolve(segs, &self.def().file) {
            None => {
                self.quiet_here();
                Ty::Unknown
            }
            Some(r) if r.rest.is_empty() => match r.target {
                Target::Const(i) => from_v(&self.p.defs[i].ret, &HashSet::new()),
                Target::Fn(i) | Target::Sig(i) => self.fn_item_ty(i),
                Target::Variant(en, vn) => {
                    if en == "Option" && vn == "None" {
                        let v = self.fresh(0);
                        Ty::Named("Option".into(), vec![v])
                    } else if self.p.types.contains_key(&en) {
                        self.variant_ctor(&en, &vn, &[])
                    } else {
                        Ty::Unknown
                    }
                }
                Target::Native(ref fq) | Target::Value(ref fq) if self.p.sigs.contains_key(fq) => {
                    let i = self.p.sigs[fq];
                    self.fn_item_ty(i)
                }
                _ => Ty::Unknown,
            },
            _ => Ty::Unknown,
        }
    }

    /// 関数定義 `i` を値として使うときの型 (型パラメータは新しい型変数にする)。
    fn fn_item_ty(&mut self, i: usize) -> Ty {
        let d = &self.p.defs[i];
        let gn = d.gnames();
        let mut map: HashMap<String, Ty> = HashMap::new();
        seed_effects(d, &mut map);
        let args: Vec<Ty> = d.params.iter().map(|p| from_v(&p.1, &gn)).collect::<Vec<_>>().iter().map(|t| self.inst(t, &mut map)).collect();
        let ret = self.inst(&from_v(&d.ret, &gn), &mut map);
        let eff = Eff(Some(EffInfo {
            decl: false,
            uses: d.uses.clone(),
            may: match d.panics.as_deref() {
                Some("never") => Some(false),
                Some("may") => Some(true),
                _ => None,
            },
            name: format!("`{}`", d.name),
        }));
        Ty::Closure { args, ret: Box::new(ret), cap: Cap::Fn, id: None, eff }
    }

    /// ユーザー定義の enum の列挙子 `en.vn(args)` の型。型パラメータは新しい型変数にし、
    /// ペイロードの宣言型と実引数を単一化して決める。
    fn variant_ctor(&mut self, en: &str, vn: &str, argt: &[Ty]) -> Ty {
        let Some(td) = self.type_def(en) else { return Ty::Named(en.to_string(), vec![]) };
        let gn = td.gnames();
        let order: Vec<&String> = td.generics.iter().filter(|g| g.0 == "type").map(|g| &g.1).collect();
        let targs: Vec<Ty> = order.iter().map(|_| self.fresh(0)).collect();
        if let Some((_, payload)) = td.variants.iter().find(|v| v.0 == vn) {
            let mut bind = HashMap::new();
            for (n, a) in order.iter().zip(&targs) {
                bind.insert((*n).clone(), a.clone());
            }
            for (pt, a) in payload.iter().zip(argt) {
                let declared = subst(&from_v(pt, &gn), &bind);
                let _ = self.unify2(&declared, a);
                // enum のペイロードの欄は「受け側」の関数型が分かる: 宣言 × 宣言の効果の食い違いをここで報告する。
                self.check_fn_conv_dir(&declared, a);
            }
        }
        Ty::Named(en.to_string(), targs)
    }

    fn call(&mut self, callee: &V, args: &[V], line: usize, targs: &[V]) -> Ty {
        let argt: Vec<Ty> = args.iter().map(|a| self.infer(a)).collect();
        let loose_args: Vec<bool> = args.iter().map(|a| self.root_loose(a)).collect();
        // 構造体リテラルの引数: 従来の検査は型引数を見ないので、呼び出し側でも型引数の矛盾を見る。
        let sl: Vec<bool> = args.iter().map(|a| nname(a) == "StructLit").collect();
        // その位置にクロージャ式が直接書かれているか (効果検査 ② が変数経由を避けるのに使う)。
        if nname(callee) != "Path" {
            // `fn(..) {..}(x)` のように、クロージャを直接呼ぶ形。
            let t = self.infer(callee);
            if let Some(r) = self.call_closure(&t, &argt, line, Some("closure")) {
                return r;
            }
            argt.iter().for_each(|a| self.escape(a));
            return Ty::Unknown;
        }
        let segs = strs(&nf(callee)[0]);
        // `v.items.push(x)` のように、局所変数から始まるパスはメソッド呼び出し。
        if self.lookup(&segs[0]).is_some() {
            self.dep_on_local(&segs[0]);
            if segs.len() < 2 {
                // `f(x)`: クロージャを持つ局所変数の直接呼び出し。
                let mut t = self.lookup(&segs[0]).unwrap();
                // 取り込んだ `&f` / `&mut f` の呼び出しは、参照を外して呼ぶ。
                while let Ty::Ref(_, inner) = &t {
                    let inner = (**inner).clone();
                    t = inner;
                }
                // 再代入されうる `let mut` の局所は、束縛時のクロージャの効果を使わない (黙る)。
                let eff_name = (!self.mutable.contains(&segs[0])).then(|| segs[0].as_str());
                if let Some(r) = self.call_closure(&t, &argt, line, eff_name) {
                    return r;
                }
                // `F: core.ops.Fn<(A,), R, E, P>` の境界を持つ型パラメータの値の呼び出し。
                if let Some(cl) = self.fn_bound_closure(&t) {
                    if let Some(r) = self.call_closure(&cl, &argt, line, None) {
                        return r;
                    }
                }
                return Ty::Unknown;
            }
            let mut t = self.lookup(&segs[0]).unwrap();
            for (k, s) in segs[1..segs.len() - 1].iter().enumerate() {
                self.log_step(nline(callee), k + 1, true, s, &t);
                t = self.field_ty(&t, s);
            }
            self.log_step(nline(callee), segs.len() - 1, false, &segs[segs.len() - 1], &t);
            self.arg_loose = loose_args;
            // `x.m(..)` の経路形の呼び先ノードに、受け手の型を載せる (KIR が トレイト側の impl (3.2) を引くため)。
            let cid = crate::parser::nid(callee);
            if segs.len() == 2 && cid != 0 && !self.p.trait_side.is_empty() {
                self.expr_pending.insert(cid, ("RecvPath".to_string(), nline(callee), t.clone()));
            }
            return self.call_method(&t, Some(&segs[0]), &segs[segs.len() - 1], &argt, line, targs, true);
        }
        let _ = &argt;
        let Some(r) = self.p.resolve(&segs, &self.def().file) else {
            self.quiet_here();
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
            // `m.Enum.Variant.method(..)` / `m.CONST.method(..)`: 値に対するメソッド呼び出し。
            if r.rest.len() == 1 && matches!(r.target, Target::Variant(..) | Target::Const(_)) {
                let prefix = &segs[..segs.len() - 1];
                let recv = self.path(prefix);
                return self.call_method(&recv, None, &r.rest[0], &argt, line, targs, false);
            }
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
                    _ if self.p.types.contains_key(&en) => self.variant_ctor(&en, &vn, &argt),
                    _ => Ty::Unknown,
                }
            }
            Target::Native(ref fq) | Target::Value(ref fq) if self.p.sigs.contains_key(fq) => {
                let i = self.p.sigs[fq];
                self.arg_loose = loose_args;
                self.call_def(i, &argt, line, &sl, targs)
            }
            // 規範のシグネチャにない基本型の関連関数 (`usize.try_from_u64` など)。実行系は整数の変換を一般の規則で
            // 受け付けるが、規範はシグネチャなので拒否する (メソッドの E1108 と同じ方針)。近い名前を提案する。
            Target::Native(ref fq)
                if fq.split_once('.').map_or(false, |(t, _)| INTS.contains(&t) || matches!(t, "f32" | "f64" | "bool" | "char")) =>
            {
                let (owner, name) = fq.split_once('.').unwrap();
                let prefix = format!("core.num.{}.", owner);
                let mut best: Option<(usize, String)> = None;
                for k in self.p.sigs.keys().filter(|k| k.starts_with(&prefix)) {
                    let cand = &k[prefix.len()..];
                    let d = crate::check::edit_distance(name, cand);
                    if best.as_ref().map_or(true, |(bd, bn)| d < *bd || (d == *bd && cand < bn.as_str())) {
                        best = Some((d, cand.to_string()));
                    }
                }
                // `try_from_X` が無くて `from_X` がある (必ず収まる変換) なら、そちらを第一に示す。
                let infallible = name.strip_prefix("try_").filter(|f| self.p.sigs.contains_key(&format!("{}{}", prefix, f)));
                let hint = match (infallible, best) {
                    (Some(f), _) => format!("; `{o}.{f}` always succeeds, so use it (it returns `{o}`, not a Result)", o = owner, f = f),
                    (None, Some((d, n))) if d <= (name.len() / 3).max(2) => format!("; did you mean `{}.{}`?", owner, n),
                    _ => String::new(),
                };
                self.diag("E1101", format!("`{}` is not declared in the standard library signatures{}", fq, hint), line);
                self.quiet_here();
                argt.iter().for_each(|a| self.escape(a));
                Ty::Unknown
            }
            Target::Fn(i) | Target::Sig(i) => {
                self.arg_loose = loose_args;
                self.call_def(i, &argt, line, &sl, targs)
            }
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
        let loose = std::mem::take(&mut self.arg_loose);
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
        seed_effects(d, &mut map);
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
            if let Some((want, got)) = self.arg_ref_conflict(&raw, a, loose.get(k).copied().unwrap_or(false)) {
                self.diag("E2023", format!("argument {} of `{}`: expected `{}`, found `{}`{}", k + 1, d.name, want, got, ref_hint(&want, &got, self.ty_copy_ref(&raw))), line);
            }
            let is_extern = d.kw == "extern";
            self.conv_quiet = is_extern;
            let ur = self.unify2(&pt, a);
            self.conv_quiet = false;
            // 引数は「受け側」のパラメータ型が分かる: 宣言 × 宣言の効果の食い違いをここで報告する。
            // `extern` の呼び出しは従来どおりここで黙る (E5004/E5005 が境界で見る)。
            if !is_extern {
                self.check_fn_conv_dir(&pt, a);
                // `F: Fn<.., E, P>` 型の値を、`Fn<.., [], never>` のように具体的な効果を持つ
                // パラメータに渡す。受け側の境界が具体的で渡し側に効果変数・panic 変数があり、
                // 実体化が分からないなら fail-closed で E4001 / E4002 にする。
                self.check_fn_param_bound_effects(d, &raw, &self.resolve(a), line);
            }
            if let Err(e) = ur {
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
                // 生ポインタの取り違え (`*const` を `*mut` の引数へ、ポインタと値など) は宣言に型パラメータがなくても報告する。
                let ptr = self.ptr_mismatch(&pt, &av);
                if raw.has_param() || const_mismatch || ptr.is_some() || (sl.get(k).copied().unwrap_or(false) && same_named) {
                    if let Some((want, got)) = self.prim_conflict(&e).or(ptr) {
                        self.diag(
                            "E2023",
                            format!("argument {} of `{}`: expected `{}`, found `{}`", k + 1, d.name, want, got),
                            line,
                        );
                    }
                }
            }
        }
        // `I: core.iter.Iterator<T, ..>` の境界: 引数 `I` の型が分かれば、その要素の型を `T` に結びつける。
        for g in &d.generics {
            if g.0 != "type" {
                continue;
            }
            for b in items(&g.2) {
                let V::L(bx) = b else { continue };
                if st(&bx[0]) != "core.iter.Iterator" {
                    continue;
                }
                let Some(first) = items(tel(b, 1)).first() else { continue };
                let (Some(it), true) = (map.get(&g.1).cloned(), true) else { continue };
                let want = self.inst(&from_v(first, &gn), &mut map);
                let elem = self.iter_elem(&it);
                let _ = self.unify2(&want, &elem);
            }
        }
        // 境界の型引数 (`I: Iterator<T, ..>` の `T`) を、受け取った引数の型の impl から決める。
        // Fn 境界より先にやり、クロージャの引数型との食い違いは Fn 境界の検査 (E2023) が報告する。
        self.resolve_impl_bounds(d, &mut map);
        // Fn 境界の単一化: 境界の `Args` タプルと戻り値型を、クロージャの型と比べる。
        let mut cap_bad = false;
        for (k, name) in &fn_args {
            let Some(cl) = closures.get(k).cloned() else { continue };
            self.unify_fn_bound(d, *k, name, &cl, &mut map, line, &mut cap_bad);
        }
        self.check_bounds(d, &map, line, true);
        // 決まらなかった型パラメータは escape する (分からない型と同じ扱い)。ただし数値リテラル由来の
        // 型変数 (kind != 0) は escape しない。`t.id(5)` の `5` は、後から決まればそれに従い、最後まで
        // 決まらなければ finish_literals が E2027 にする。
        for t in map.values() {
            if let Ty::Var(v) = self.deep(t) {
                if self.kinds[v] == 0 {
                    self.escape(t);
                }
            }
        }
        let ret = from_v(&d.ret, &gn);
        let r = self.inst(&ret, &mut map);
        self.box_hooks(d, &map, line);
        r
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

    /// `F: core.ops.Fn<Args, R, E, P>` の境界を持つ型パラメータ `name` (定義 `d` の generics に属する)
    /// の効果実引数を `(uses, panics)` で返す。効果変数・panic 変数はそのまま (大文字の名前 /
    /// `None`) で返す。`Fn` 境界がなければ None。
    fn fn_bound_effects(&self, d: &Def, name: &str) -> Option<(Vec<String>, Option<bool>)> {
        for b in self.fn_bound(d, name).iter() {
            let V::L(bx) = b else { continue };
            if !st(&bx[0]).starts_with("core.ops.Fn") {
                continue;
            }
            let targs = items(tel(b, 1));
            let uses = match targs.get(2) {
                Some(v) if tag(v) == "effects" => items(tel(v, 1)).iter().map(|x| st(x)).collect(),
                Some(v) if tag(v) == "named" => {
                    let nm = st(tel(v, 1));
                    if d.generics.iter().any(|g| g.0 == "uses" && g.1 == nm) {
                        vec![nm]
                    } else {
                        continue;
                    }
                }
                _ => continue,
            };
            let may = match targs.get(3) {
                Some(v) if tag(v) == "panic" => match st(tel(v, 1)).as_str() {
                    "never" => Some(false),
                    "may" => Some(true),
                    _ => None,
                },
                Some(v) if tag(v) == "named" => {
                    let nm = st(tel(v, 1));
                    if d.generics.iter().any(|g| g.0 == "panics" && g.1 == nm) {
                        None
                    } else {
                        continue;
                    }
                }
                _ => continue,
            };
            return Some((uses, may));
        }
        None
    }

    /// 型パラメータの `Fn` 境界を受け取るパラメータに型パラメータの値を渡す場所で効果を検査する。
    /// 受け側の境界が具体的で渡し側に効果変数・panic 変数があり、実体化が分からないなら
    /// fail-closed で E4001 / E4002 を報告する。
    fn check_fn_param_bound_effects(&mut self, callee: &Def, raw: &Ty, got: &Ty, line: usize) {
        // 参照は透過的: `&F` / `&mut F` も `F` と同じ境界を持つ。
        let mut raw = raw;
        while let Ty::Ref(_, x) = raw {
            raw = x;
        }
        let mut got = got.clone();
        while let Ty::Ref(_, x) = got {
            got = self.resolve(&x);
        }
        let Ty::Param(wname) = raw else { return };
        let Some((wu, wp)) = self.fn_bound_effects(callee, wname) else { return };
        let Ty::Param(gname) = &got else { return };
        let d = self.def();
        let Some((gu, gp)) = self.fn_bound_effects(d, gname) else { return };
        let want = EffInfo {
            decl: true,
            uses: wu,
            may: wp,
            name: format!("parameter `{}` of `{}`", wname, callee.name),
        };
        let got = EffInfo {
            decl: false,
            uses: gu,
            may: gp,
            name: format!("type parameter `{}` of `{}`", gname, d.name),
        };
        self.check_fn_eff(&want, &got, line);
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
            // 参照は透過的: `&F` / `&mut F` も `F` と同じ境界を持ち、`&g` を渡しても `g` のクロージャを見る。
            let mut pt = from_v(&p.1, &gn);
            while let Ty::Ref(_, x) = pt {
                pt = *x;
            }
            let Ty::Param(name) = pt else { continue };
            let bounds = self.fn_bound(d, &name);
            if bounds.is_empty() {
                continue;
            }
            let mut at = self.deep(a);
            while let Ty::Ref(_, x) = at {
                at = self.deep(&x);
            }
            let Ty::Closure { id: Some(node), .. } = at else { continue };
            let Some((uses, may, unsure)) = self.closure_effs.get(&node) else { continue };
            // 確実でない効果 (推論された関数・`dyn` / 境界越しの上限) も、5.1 に合わせて上限として束縛に使う (fail-closed)。
            let _ = unsure;
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

    /// 型が整数のプリミティブと決まっているか (参照は外して見る)。
    fn is_int_prim(&self, t: &Ty) -> bool {
        matches!(self.deep(t).strip(), Ty::Prim(n) if crate::check::INT_PRIMS.contains(&n.as_str()))
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
            // 参照は透過的: `&F` / `&mut F` も `F` と同じ境界を持ち、`&g` を渡しても `g` のクロージャを見る。
            let mut pt = from_v(&p.1, &gn);
            while let Ty::Ref(_, x) = pt {
                pt = *x;
            }
            let Ty::Param(name) = pt else { continue };
            let bounds = self.fn_bound(d, &name);
            if bounds.is_empty() {
                continue;
            }
            let mut at = self.deep(a);
            while let Ty::Ref(_, x) = at {
                at = self.deep(&x);
            }
            let Ty::Closure { id: Some(node), .. } = at else { continue };
            let Some((uses, may, unsure)) = self.closure_effs.get(&node) else { continue };
            // 境界の具体名との比べは、確実な効果だけで行う (推論された関数・`dyn` 越しの上限は、
            // 囲む関数の宣言との比べ (`closure_effect_bind` / `closure_call_effects`) が見る)。
            // 確実でない効果 (推論された関数・`dyn` 越しの上限) も上限として比べる (fail-closed)。
            let _ = unsure;
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
    ///
    /// `eff_name` が `Some` のときは、そのクロージャの本体から推論した効果を、呼び出し元の検査に足す (4.3 / 5.1)。
    fn call_closure(&mut self, cl: &Ty, argt: &[Ty], line: usize, eff_name: Option<&str>) -> Option<Ty> {
        let cl = self.deep(cl);
        let Ty::Closure { args, ret, id, .. } = cl else { return None };
        // 直接呼び出し (`let g = fn..; g(x)`) は、呼び出し元からクロージャの節への辺になる (5.3)。
        // 節の本体の呼び出しは書かれた関数に帰属するが、節そのもののフレームを数えるには辺が要る。
        if let Some(node) = id.clone() {
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
        if let (Some(name), Some(node)) = (eff_name, id.as_deref()) {
            self.closure_call_effects(node, name, line);
        }
        Some((*ret).clone())
    }

    /// 型パラメータ `F` に `core.ops.Fn*<(A..), R, ..>` の境界があるとき、その引数・戻り値の型でできたクロージャ型。
    fn fn_bound_closure(&self, t: &Ty) -> Option<Ty> {
        let Ty::Param(pn) = self.deep(t).strip().clone() else { return None };
        let d = self.def();
        let gn = d.gnames();
        for g in d.generics.iter().filter(|g| g.0 == "type" && g.1 == pn) {
            for b in items(&g.2) {
                let V::L(bx) = b else { continue };
                if !st(&bx[0]).starts_with("core.ops.Fn") {
                    continue;
                }
                let ta = items(tel(b, 1));
                let (Some(a0), Some(r)) = (ta.first(), ta.get(1)) else { continue };
                let Ty::Tuple(args) = bound_targ(a0, &gn) else {
                    if bound_targ(a0, &gn) == Ty::Unit {
                        return Some(Ty::Closure { args: vec![], ret: Box::new(bound_targ(r, &gn)), cap: Cap::Fn, id: None, eff: Eff::default() });
                    }
                    continue;
                };
                return Some(Ty::Closure { args, ret: Box::new(bound_targ(r, &gn)), cap: Cap::Fn, id: None, eff: Eff::default() });
            }
        }
        None
    }

    /// クロージャの直接呼び出しの効果 (4.3 / 5.1): 本体から推論した効果 (確実に含まれる部分だけ) を、
    /// 呼び出し元の宣言と比べる (`record_effects`)。確実でない推論の効果は使わない (黙る)。その
    /// 呼び出しを含む外側のクロージャも確実でないとする (`eff_acc` の 3 つ目)。
    fn closure_call_effects(&mut self, node: &str, name: &str, line: usize) {
        let Some((uses, may, unsure)) = self.closure_effs.get(node).cloned() else { return };
        // 確実でなくても上限として数える。呼び出しを含む外側のクロージャも確実でないとする。
        if unsure {
            if let Some(acc) = self.eff_acc.last_mut() {
                acc.2 = true;
            }
        }
        let uses: Vec<String> = uses.into_iter().collect();
        let panics = may.then(|| "may".to_string());
        self.record_effects(line, vec![(name.to_string(), uses.clone(), panics.clone(), uses, panics)]);
    }

    /// 生ポインタが絡む矛盾 (`want` = 注釈・宣言の型、`got` = 実際の型) の 2 つの型の文字列。
    /// 報告するのは、`*const` を `*mut` に渡す (`as_mut` を通していない)、要素型が確定して食い違う、
    /// ポインタと確定したプリミティブ・構造体の型を取り違える、の 3 つだけ。型が途中で決まらないもの、
    /// 参照との組み合わせ、型パラメータや未知の型を含むものは黙る。
    fn ptr_mismatch(&self, want: &Ty, got: &Ty) -> Option<(String, String)> {
        let w = self.deep(want);
        let g = self.deep(got);
        match (&w, &g) {
            (Ty::Ptr(wm, wx), Ty::Ptr(gm, gx)) => {
                if *wm && !*gm {
                    return Some((w.show(), g.show()));
                }
                let (wx, gx) = (self.deep(wx), self.deep(gx));
                if self.open(&wx) || self.open(&gx) {
                    return None;
                }
                if wx != gx { Some((w.show(), g.show())) } else { None }
            }
            (Ty::Ptr(..), o) => self.plain_show(o).map(|s| (w.show(), s)),
            (o, Ty::Ptr(..)) => self.plain_show(o).map(|s| (s, g.show())),
            _ => self.vec_seq_mismatch(&w, &g),
        }
    }

    /// `Vec` と配列・スライスの取り違え (4.2: 暗黙の変換は `&[T; N]` → `&[T]` だけ)。参照は剥がして比べる。
    fn vec_seq_mismatch(&self, want: &Ty, got: &Ty) -> Option<(String, String)> {
        let strip = |t: &Ty| {
            let mut t = self.deep(t);
            while let Ty::Ref(_, x) = t {
                t = self.deep(&x);
            }
            t
        };
        let (w, g) = (strip(want), strip(got));
        let is_vec = |t: &Ty| matches!(t, Ty::Named(n, _) if n == "alloc.vec.Vec");
        let is_seq = |t: &Ty| matches!(t, Ty::Array(_) | Ty::Slice(_));
        if (is_vec(&w) && is_seq(&g)) || (is_seq(&w) && is_vec(&g)) {
            Some((w.show(), g.show()))
        } else {
            None
        }
    }

    /// 型の中に、まだ決まっていない・分からない部分 (型変数・未知・型パラメータ・参照・クロージャ・`dyn`) があれば true。
    fn open(&self, t: &Ty) -> bool {
        match self.deep(t) {
            Ty::Unknown | Ty::Var(_) | Ty::Param(_) | Ty::Ref(..) | Ty::Closure { .. } | Ty::Dyn(..) | Ty::Never => true,
            Ty::Named(_, a) | Ty::Tuple(a) => a.iter().any(|x| self.open(x)),
            Ty::Ptr(_, x) | Ty::Array(x) | Ty::Slice(x) => self.open(&x),
            _ => false,
        }
    }

    /// ポインタとの取り違えを報告する、確定したプリミティブ・構造体・リテラルの型の文字列。
    fn plain_show(&self, t: &Ty) -> Option<String> {
        let d = self.deep(t);
        match &d {
            Ty::Prim(p) => Some(p.clone()),
            Ty::Named(..) => Some(d.show()),
            Ty::Var(v) => match self.kinds[*v] {
                1 => Some("{integer}".into()),
                2 => Some("{float}".into()),
                _ => None,
            },
            _ => None,
        }
    }

    /// 矛盾した 2 つの型が「プリミティブ同士の違い」か「Named 同士の名前違い」なら、その文字列を返す。
    /// それ以外は誤検出のおそれがあるので None (何も言わない)。
    /// `Box<X, ..>` と `Box<Y, ..>` の第1型引数の一方だけが `dyn` か (どちらも決まっているとき)。
    fn box_dyn_mismatch(&self, xs: &[Ty], ys: &[Ty]) -> bool {
        let (Some(x), Some(y)) = (xs.first(), ys.first()) else { return false };
        let (x, y) = (self.deep(x), self.deep(y));
        let known = |t: &Ty| !matches!(t, Ty::Unknown | Ty::Var(_) | Ty::Param(_) | Ty::Never);
        known(&x) && known(&y) && matches!(x, Ty::Dyn(..)) != matches!(y, Ty::Dyn(..))
    }

    fn type_conflict(&self, e: &(Ty, Ty)) -> Option<(String, String)> {
        match (&e.0, &e.1) {
            (Ty::Named(a, xs), Ty::Named(b, ys)) if a == b && a == "alloc.boxed.Box" && self.box_dyn_mismatch(xs, ys) => {
                Some((e.0.show(), e.1.show()))
            }
            (Ty::Prim(a), Ty::Prim(b)) if a != b => Some((a.clone(), b.clone())),
            (Ty::Named(a, _), Ty::Named(b, _)) if a != b => Some((a.clone(), b.clone())),
            (Ty::Const(a), Ty::Const(b)) if a != b => Some((a.clone(), b.clone())),
            _ => None,
        }
    }

    /// 参照の段数の食い違い (4.2: 暗黙に許すのは `&mut T` → `&T`、再借用、`&[T; N]` → `&[T]`、`&T` → `&dyn Tr` だけ)。
    /// 値 (`want`) が要る所に参照 (`got`) を渡した形 (`&u32` を `u32` として返す) を見つけて、診断用の
    /// 2 つの型の文字列を返す。型が決まっていない・型パラメータを含む場合は黙る。
    /// 逆向き (参照が要る所に値) は、パターン束縛が外側の参照を落とす近似があるので見ない。
    /// 型 `t` (参照は剥がす) が Copy か。決まらない型・型パラメータは true (従来の案内のまま)。
    fn ty_copy_ref(&self, t: &Ty) -> bool {
        let mut t = self.deep(t);
        while let Ty::Ref(_, inner) = t {
            t = *inner;
        }
        self.ty_copy_hint(&t)
    }

    fn ty_copy_hint(&self, t: &Ty) -> bool {
        match t {
            Ty::Named(nm, args) if nm == "Option" || nm == "Result" => args.iter().all(|x| self.ty_copy_hint(x)),
            Ty::Tuple(xs) => xs.iter().all(|x| self.ty_copy_hint(x)),
            _ => self.ty_copy(t) != Some(false),
        }
    }

    fn deref_hint(&self, want: &Ty) -> String {
        deref_advice(self.ty_copy_ref(want))
    }

    fn ref_level_conflict(&self, want: &Ty, got: &Ty) -> Option<(String, String)> {
        let w = self.deep(want);
        let g = self.deep(got);
        if self.undetermined(&w) || self.undetermined(&g) || w.has_param() || g.has_param() {
            return None;
        }
        fn value_like(t: &Ty) -> bool {
            matches!(t, Ty::Prim(_) | Ty::Named(..) | Ty::Tuple(_) | Ty::Array(_) | Ty::Unit)
        }
        let (mut wi, mut gi) = (&w, &g);
        // 参照の段数を揃えて比べる。
        loop {
            match (wi, gi) {
                (Ty::Ref(_, a), Ty::Ref(_, b)) => {
                    wi = a;
                    gi = b;
                }
                (x, Ty::Ref(..)) if value_like(x) => return Some((w.show(), g.show())),
                _ => return None,
            }
        }
    }

    /// 呼び出しの引数の参照の段数 (4.2: 引数には暗黙の自動参照も自動参照外しもない)。
    /// `want` は宣言の引数型 (型パラメータを含んでよい)、`got` は実引数の型。参照の段を左右そろえて
    /// 外し、片方だけが参照のまま値に当たったら食い違い。許すのは両側が参照 (`&mut T` → `&T`、
    /// `&[T; N]` → `&[T]`、`&T` → `&dyn Tr` を含む) の組だけ。決まっていない・型パラメータ・クロージャ・`dyn`・
    /// `!` が絡むときは黙る。`loose` は実引数が「参照の段数が決まらない束縛」(参照越しの分解で得た名前) の
    /// とき。その束縛は外側の参照を落とした型を持つので、値が要る所に参照が来る向きだけを見る
    /// (落としているので参照が現れることはなく、逆向きだけが誤検出になり得る)。
    fn arg_ref_conflict(&self, want: &Ty, got: &Ty, loose: bool) -> Option<(String, String)> {
        let g0 = self.deep(got);
        fn plain(t: &Ty) -> bool {
            matches!(t, Ty::Prim(_) | Ty::Named(..) | Ty::Tuple(_) | Ty::Array(_) | Ty::Unit)
        }
        let (mut wi, mut gi) = (want.clone(), g0.clone());
        loop {
            match (&wi, &gi) {
                (Ty::Ref(_, a), Ty::Ref(_, b)) => {
                    let (a, b) = ((**a).clone(), (**b).clone());
                    wi = a;
                    gi = b;
                }
                (w, Ty::Ref(..)) if plain(w) && !w.has_param() => return Some((want.show(), g0.show())),
                (Ty::Ref(_, a), g) if !loose && plain(g) && !self.undetermined(g) && !matches!(**a, Ty::Param(_) | Ty::Unknown) && !self.undetermined(&g0) => {
                    return Some((want.show(), g0.show()));
                }
                _ => return None,
            }
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
        if let Some(p) = self.ptr_mismatch(want, got) {
            return Some(p);
        }
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
            // `()` と値の型の食い違い (`-> Result<..>` の関数が `()` で終わるなど)。
            (Ty::Unit, x @ (Ty::Prim(_) | Ty::Named(..) | Ty::Tuple(_))) => Some(("()".into(), x.show())),
            (x @ (Ty::Prim(_) | Ty::Named(..) | Ty::Tuple(_)), Ty::Unit) => Some((x.show(), "()".into())),
            _ => None,
        }
    }

    /// 型の中に、まだ決まっていない型変数があれば true。推論が途切れた部分を含む型を
    /// 全体の形として示しても役に立たないので、そのときは矛盾した箇所どうしを見せる。
    fn unresolved(&self, t: &Ty) -> bool {
        match self.deep(t) {
            Ty::Var(_) => true,
            Ty::Named(_, a) | Ty::Tuple(a) => a.iter().any(|x| self.unresolved(x)),
            Ty::Ref(_, x) | Ty::Ptr(_, x) | Ty::Array(x) | Ty::Slice(x) => self.unresolved(&x),
            Ty::Closure { args, ret, .. } => args.iter().any(|x| self.unresolved(x)) || self.unresolved(&ret),
            _ => false,
        }
    }

    /// `?` のエラー変換 (4.1): 関数 (クロージャ) のエラー型 `want` が `derive [From]` の `enum` で、
    /// `got` を唯一のペイロードに持つバリアントを引く。両方の型が確定していて同じ型でないときだけ引く
    /// (型が分からないときは None で、呼び出し側は従来どおり単一化に任せる)。
    fn from_conversion(&mut self, want: &Ty, got: &Ty) -> Option<crate::program::FromVariant> {
        let want = self.deep(want);
        let got = self.deep(got);
        let Ty::Named(wn, _) = want.strip() else { return None };
        if self.unresolved(got.strip()) {
            return None;
        }
        let key = got.strip().show();
        if key == want.strip().show() {
            return None;
        }
        Some(self.p.from_variant(wn, &key))
    }

    /// 4.1: `?` はエラー型が完全一致するか、`derive [From]` の `enum` へ包めるときだけ使える。
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
                    // 4.1: 型が違っても `E2` が `derive [From]` で `E1` を唯一のペイロードに持つバリアントがあれば、包んで返す。
                    match self.from_conversion(&e_want, &e_got) {
                        Some(crate::program::FromVariant::One(_)) => return,
                        Some(crate::program::FromVariant::Many(vs)) => {
                            let (w, g) = (self.deep(&e_want).strip().show(), self.deep(&e_got).strip().show());
                            self.diag(
                                "E2049",
                                format!(
                                    "ambiguous `?` conversion: variants {} of `{w}` all carry `{g}`; write `map_err` to choose one",
                                    vs.join(", "),
                                ),
                                line,
                            );
                            return;
                        }
                        _ => {}
                    }
                    if let Err(err) = self.unify2(&e_want, &e_got) {
                        if let Some((w, g)) = self.type_conflict(&err) {
                            self.diag(
                                "E2020",
                                format!(
                                    "type mismatch in `?`: expected error type `{w}`, found `{g}`; add a variant `V({g})` to `{w}` (an enum that declares `derive [From]`), or write `map_err`",
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

/// トレイト境界の型実引数 (`core.ops.Fn<(&T,), bool, E, P>` の各項) を型にする。
/// `Self` は `Param("Self")`、効果・panic の実引数は「どれでもよい」印。
fn bound_targ(a: &V, params: &HashSet<String>) -> Ty {
    match tag(a).as_str() {
        "effects" | "panic" => Ty::Const("effect".into()),
        "named" if st(tel(a, 1)) == "Self" => Ty::Param("Self".into()),
        _ => const_targ(a).map_or_else(|| from_v(a, params), Ty::Const),
    }
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

/// 本体の中で、ある捕捉された局所がどう使われるかの近似 (`TypeCk::closure_cap` 用)。
#[derive(Default)]
struct CapUse {
    /// 代入・`&mut` などで変更される (FnMut)。
    mutated: bool,
    /// 型が `Copy` でない値として消費される (FnOnce)。
    consumed: bool,
}

/// 式の根が局所 `name` そのもの (`name` / `name.f` / `name[i]`) か。
fn place_root_is(e: &V, name: &str) -> bool {
    match nname(e) {
        "Path" => strs(&nf(e)[0]) == [name.to_string()],
        "Field" | "Index" => place_root_is(&nf(e)[0], name),
        _ => false,
    }
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

/// 値が要る所に参照が来たときの案内。Copy の型は `*` で読めるが、Copy でない型は参照の先から move できない (4.1)。
pub(crate) fn deref_advice(copy: bool) -> String {
    if copy {
        "; there is no implicit dereference: write `*` to read the value".to_string()
    } else {
        "; there is no implicit dereference, and a value that is not Copy cannot be moved out of a reference: pass the owned value, or call `.clone()` to copy it".to_string()
    }
}

/// 参照の段数の食い違いに付ける案内 (引数には自動参照も自動参照外しもない)。
pub(crate) fn ref_hint(want: &str, got: &str, copy: bool) -> String {
    if got.starts_with('&') && !want.starts_with('&') {
        deref_advice(copy)
    } else if want.starts_with('&') && !got.starts_with('&') {
        "; there is no implicit reference: write `&` (or `&mut`) to borrow the value".to_string()
    } else {
        String::new()
    }
}

/// 整数型どうしの食い違いに付ける、変換の仕方の案内 (暗黙の変換も `as` もない)。
pub(crate) fn conv_hint(want: &str, got: &str) -> String {
    let int = |t: &str| INTS.contains(&t);
    if want == got || !int(want) || !int(got) {
        return String::new();
    }
    // (符号つきか, ビット幅)。`usize` / `isize` は 64 ビット (仕様 4.5)。
    let shape = |t: &str| -> (bool, u32) {
        let signed = t.starts_with('i');
        let bits = match &t[1..] {
            "size" => 64,
            n => n.parse().unwrap_or(64),
        };
        (signed, bits)
    };
    let ((ws, wb), (gs, gb)) = (shape(want), shape(got));
    // 値が必ず収まる向き: 同じ符号で幅が同じか広い、または符号なしからより広い符号つきへ。
    let widening = (ws == gs && wb >= gb) || (ws && !gs && wb > gb);
    if widening {
        format!("; there is no implicit conversion and no `as`: write `core.num.{w}.from_{g}(x)` ({g} always fits in {w})", w = want, g = got)
    } else {
        format!(
            "; there is no implicit conversion and no `as`: {g} does not always fit in {w}, so write `core.num.{w}.try_from_{g}(x)` (returns Result)",
            w = want,
            g = got
        )
    }
}

/// 型 V の中に参照 (`&T` / `&mut T`) があるか ('static でないことの近似。5.1 の `Box<dyn>`)。
fn v_has_ref(v: &V) -> bool {
    match v {
        V::L(xs) => tag(v) == "ref" || xs.iter().any(v_has_ref),
        _ => false,
    }
}
