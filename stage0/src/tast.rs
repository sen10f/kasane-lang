//! TAST (型付き AST) の型と、推論が埋める側の表の宣言 (設計_型付き検査.md 8 章)。
//!
//! 方針 (7.5): 推論の中で木を組まず、**推論の後で、型表 (`TypeCk::tytab`) と側の表 (`SideTabs`) から木を機械的に組む**。
//! この段階 (G28) は型と表の宣言だけで、表は空のまま・`build` は空の `Tast` を返す (呼ばれない)。検査の結果は変えない。
//!
//! 鍵の決まり: 式 ID (`ExprId`、`parser::number_nodes` が本体ごとに前順で振る) は本体の中でだけ一意なので、
//! すべての表の鍵は `(DefId, ExprId)` (`Key`)。パターンも `V::Node` なので同じ番号の空間にある (`PatId = ExprId`)。
#![allow(dead_code)]

use std::collections::HashMap;

use crate::parser::V;
use crate::program::Program;
use crate::types::Ty;

/// `Program::defs` の添字。
pub type DefId = usize;
/// `V::Node` の第 4 成分 (本体ごとに 1 から。0 = 未採番)。
pub type ExprId = u32;
/// パターンの節点の式 ID (式と同じ空間)。
pub type PatId = u32;
/// 側の表の鍵: (書き手の定義, 節点の式 ID)。
pub type Key = (DefId, ExprId);

/// 呼び出しの解決先。
#[derive(Clone, Debug, PartialEq)]
pub enum Callee {
    /// 関数・メソッド・`sig` の定義 1 つ (総称なら `type_args` に実体)。
    Def(DefId),
    /// 候補が複数のメソッド (7.3 の 6.3%)。S2 で `fit` の絞り込みか、複数のまま持つかを決める。
    Methods(Vec<DefId>),
    /// 列挙子の構築 `E.V(x)` (列挙型の FQN, 変種名)。
    Variant(String, String),
    /// 局所の関数値・閉包の呼び出し (局所名)。
    Local(String),
    /// `dyn Tr` 越しのメソッド (トレイトの定義)。
    Dyn(DefId),
    /// 解決できない (検査は安全側 = 全引数の借用が戻り値へ流れる、と読む)。
    Unknown,
}

/// 暗黙の変換の印 (設計 2.3 規則 C)。式 `e` が期待される型へ入るときに推論が足した調整で、`e` の `ty` の**外側**にある。
#[derive(Clone, Debug, PartialEq)]
pub enum Coercion {
    /// 受け手の自動参照 `x.m()` → `(&x).m()` / `(&mut x).m()` (`true` = 可変)。
    AutoRef(bool),
    /// 自動の再借用 (`&mut T` を渡して `&mut *p` になる、`&mut T` → `&T`)。
    Reborrow { to_mut: bool },
    /// `&[T; N]` → `&[T]`。
    ArrayToSlice,
    /// `&T` / `Box<T>` → `&dyn Tr` / `Box<dyn Tr>` (元の型, `dyn` の型)。
    Unsize { from: Ty, to: Ty },
    /// クロージャ → 関数型 `fn(..)`。
    ClosureToFn,
}

/// 場所 (place) の表現 (設計 2.3 の述語 `place(e)`)。値の式は `None`。
#[derive(Clone, Debug, PartialEq)]
pub enum Place {
    /// 局所の束縛 (名前, その `PBind` の ID)。
    Local(String, PatId),
    /// `base.field`。
    Field(Box<Place>, String),
    /// `base[i]`。
    Index(Box<Place>),
    /// `*e`: 参照・Box の指す先。`through` は経路上の参照の型 (共有か可変かを規則 W が読む)。
    Deref(Box<Place>, Ty),
    /// 一時値 (呼び出しの結果・`match` の値など) を式 ID で指す。
    Temp(ExprId),
}

/// パターンの束縛モード (参照越しの分解の既定の束縛モード)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindMode {
    /// 値で束縛 (所有・Copy)。
    Move,
    /// `&欄の型` で束縛 (共有の参照越し)。
    Ref,
    /// `&mut 欄の型` で束縛。
    RefMut,
}

/// パターン節点の情報 (`SideTabs::pat_tys`)。
#[derive(Clone, Debug, PartialEq)]
pub struct PatInfo {
    /// パターンが照合する値の型。
    pub ty: Ty,
    /// `PBind` だけ: 束縛した名前の型 (`let_tys` と同じ値) と束縛モード。
    pub bind: Option<(String, Ty, BindMode)>,
}

/// 推論が埋める側の表。担当 (A〜D) が、doc コメントの関数で埋める。空のままなら何も変わらない。
#[derive(Clone, Debug, Default)]
pub struct SideTabs {
    /// 【担当 A】`Call` / `MCall` ごとの解決先 (自由関数・列挙子・メソッド・局所の閉包)。
    /// 埋める所: `types.rs` の `call` / `call_method` / `variant_ctor` (今は `p.resolve` を引いて捨てている)。
    /// メソッドは既存の `TypeCk::call_res` から写せる (`Methods` / `Def`)。
    pub resolved_fn: HashMap<Key, Callee>,
    /// 【担当 B】総称の呼び出しの型引数の実体 (宣言の型パラメータの順)。呼び出しの節点の鍵。
    /// 埋める所: `call_def` で新しい型変数を作る所。`check_def` の終わりに `deep` で解決して `type_args` へ移す。
    pub type_args: HashMap<Key, Vec<Ty>>,
    /// 【担当 C】式ごとの暗黙の変換の印 (適用順)。変換される式 (引数・受け手・代入の右辺など) の節点の鍵。
    /// 埋める所: `call_method_in` (自動参照)、`arg_ref_conflict` 系 (再借用)、`unsize` を積む所 (`check_dyn_impl`)。
    pub coercions: HashMap<Key, Vec<Coercion>>,
    /// 【担当 D】パターンの節点ごとの型と束縛モード。
    /// 埋める所: `bind_pat` / `bind_pat_from` (`pat_mode` を読んでいる所)。`PBind` の型は `let_tys` と一致させる。
    pub pat_tys: HashMap<Key, PatInfo>,
}

/// 閉包の取り込み (`TypeCk::closure_caps` を式 ID の鍵に直した形。種別は `&` / `&mut` / `move`)。
#[derive(Clone, Debug, PartialEq)]
pub struct Capture {
    pub kind: String,
    pub name: String,
    pub ty: Ty,
}

/// 型付きの節点が持つ共通の情報。
#[derive(Clone, Debug, PartialEq)]
pub struct TExpr {
    pub id: ExprId,
    pub line: usize,
    pub kind: TKind,
    /// 推論済みの型 (`tytab`)。型表に入らない節点 (文など) は `Ty::Unit`。
    pub ty: Ty,
    /// 場所の式なら場所。
    pub place: Option<Place>,
    /// 暗黙の変換の印 (`SideTabs::coercions`)。
    pub conv: Vec<Coercion>,
    /// 呼び出し節点の解決先 (`SideTabs::resolved_fn`)。
    pub res: Option<Callee>,
    /// 呼び出し節点の型引数の実体 (`SideTabs::type_args`)。
    pub targs: Vec<Ty>,
}

/// 節点の形。式の形ごとの場合分けは、ここ (木を組む側) に 1 回だけ置く (設計 2.3 の末尾)。
#[derive(Clone, Debug, PartialEq)]
pub enum TKind {
    Lit(String),
    /// 経路 `a.b.c` (束縛・定数・関数・列挙子)。
    Path(Vec<String>),
    Call { callee: Box<TExpr>, args: Vec<TExpr> },
    MCall { recv: Box<TExpr>, name: String, args: Vec<TExpr> },
    Field { base: Box<TExpr>, name: String },
    Index { base: Box<TExpr>, index: Box<TExpr> },
    /// 単項 (`&` `&mut` `*` `-` `!`) と二項演算。
    Unary { op: String, operand: Box<TExpr> },
    Binary { op: String, lhs: Box<TExpr>, rhs: Box<TExpr> },
    Block(Vec<TStmt>),
    If { cond: Box<TExpr>, then: Box<TExpr>, els: Option<Box<TExpr>> },
    Match { scrut: Box<TExpr>, arms: Vec<TArm> },
    /// 閉包: 引数の束縛・取り込み (`closure_caps`)・本体。
    Closure { params: Vec<TBinding>, captures: Vec<Capture>, body: Box<TExpr> },
    /// 上に無い形 (`For` `While` `Loop` `Return` `Try` `StructLit` `TupleE` `ArrayE` ...)。種別名と子の式。
    Other { name: &'static str, children: Vec<TExpr> },
}

/// 文。
#[derive(Clone, Debug, PartialEq)]
pub enum TStmt {
    Let { pat: TPat, init: Option<TExpr> },
    Expr(TExpr),
    Assign { place: TExpr, value: TExpr },
}

/// `match` の腕。
#[derive(Clone, Debug, PartialEq)]
pub struct TArm {
    pub pat: TPat,
    pub guard: Option<TExpr>,
    pub body: TExpr,
}

/// パターン節点。
#[derive(Clone, Debug, PartialEq)]
pub struct TPat {
    pub id: PatId,
    pub line: usize,
    pub info: Option<PatInfo>,
    /// 子のパターン (`PTuple` / `PVariant` / `PStruct` / `PRef` の中身)。
    pub children: Vec<TPat>,
    /// `PBind` の名前。
    pub name: Option<String>,
}

/// 関数・閉包の引数の束縛 (宣言の型から `from_v` で引き直した型)。
#[derive(Clone, Debug, PartialEq)]
pub struct TBinding {
    pub name: String,
    pub ty: Ty,
}

/// 1 つの定義の本体。
#[derive(Clone, Debug, PartialEq)]
pub struct TBody {
    pub def: DefId,
    pub params: Vec<TBinding>,
    pub ret: Ty,
    pub body: TExpr,
}

/// プログラム全体の TAST (利用者の定義だけ)。
#[derive(Clone, Debug, Default)]
pub struct Tast {
    pub bodies: std::collections::BTreeMap<DefId, TBody>,
}

/// 【担当 E】型表 (`TypeCk::tytab`)・`SideTabs`・`let_tys`・`closure_caps` と `V` の木から TAST を組む。
/// `V` を 1 回辿り、式の形の `match` はここにだけ置く。未確定の型が 1 つでもある本体は組まない (E2044 が先に止める)。
/// 今は空の TAST を返す (どこからも呼ばれない)。
pub fn build(_p: &Program, _user: &[DefId], _tytab: &std::collections::BTreeMap<(String, u32), crate::types::ExprTy>, _side: &SideTabs, _let_tys: &HashMap<Key, Ty>) -> Tast {
    let _ = std::marker::PhantomData::<V>;
    Tast::default()
}

/// `KSN_DUMP_SIDE=1`: `resolved_fn` を (定義名, 式 ID) 順に出す。1 行目は `side resolved_fn`。
pub fn dump_resolved_fn(p: &Program, side: &SideTabs) -> String {
    let mut keys: Vec<&Key> = side.resolved_fn.keys().collect();
    keys.sort_by(|a, b| (&p.defs[a.0].name, a.1).cmp(&(&p.defs[b.0].name, b.1)));
    let mut o = String::from("side resolved_fn\n");
    for k in keys {
        let c = match &side.resolved_fn[k] {
            Callee::Def(i) => format!("Def {}", p.defs[*i].name),
            Callee::Methods(v) => format!("Methods {}", v.iter().map(|i| p.defs[*i].name.as_str()).collect::<Vec<_>>().join(" | ")),
            Callee::Variant(e, v) => format!("Variant {}.{}", e, v),
            Callee::Local(n) => format!("Local {}", n),
            Callee::Dyn(i) => format!("Dyn {}", p.defs[*i].name),
            Callee::Unknown => "Unknown".to_string(),
        };
        o.push_str(&format!("{}\t{}\t{}\n", p.defs[k.0].name, k.1, c));
    }
    o
}

/// `KSN_TYPE_SURVEY=1`: `resolved_fn` の種類別の件数 (stderr の末尾に出す)。
pub fn survey_resolved_fn(side: &SideTabs) -> String {
    let mut n = [0usize; 6];
    for c in side.resolved_fn.values() {
        n[match c {
            Callee::Def(_) => 0,
            Callee::Methods(_) => 1,
            Callee::Variant(..) => 2,
            Callee::Local(_) => 3,
            Callee::Dyn(_) => 4,
            Callee::Unknown => 5,
        }] += 1;
    }
    let tot: usize = n.iter().sum();
    format!(
        "resolved_fn\ttotal={}\tDef={}\tMethods={}\tVariant={}\tLocal={}\tDyn={}\tUnknown={}\n",
        tot, n[0], n[1], n[2], n[3], n[4], n[5]
    )
}
