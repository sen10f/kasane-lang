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
    /// 関数・クロージャの値が、効果を宣言した関数型 `fn(..) uses [..] panics ..` (これ) の位置へ入る。効果の検査 (E4001 / E4002) が使う。
    FnTo(Ty),
    /// 受け手の自動参照外し (`&&T`・`&mut &T`・`Box<T>` の受け手を `n` 段外して `self` に合わせる)。
    AutoDeref { n: u8 },
    /// `?` のエラー型の変換: `derive [From]` の `enum` のバリアントで包む (`?` の節点に付く。元のエラー型, 戻り値のエラー型, バリアント名)。
    ErrInto { from: Ty, to: Ty, variant: String },
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
    /// 【担当 C の作業用】呼び出しの引数の式 ID (`call` / `MCall` が積み、`call_def` / `call_method_in` が取る)。
    pub arg_ids: Vec<ExprId>,
    /// 【担当 C の作業用】引数の (鍵, 期待の型, 実際の型)。型が決まる関数の終わり (`flush_expr_types`) で `coercions` に変える。
    pub conv_pending: Vec<(Key, Ty, Ty)>,
    /// 【担当 C の作業用】受け手の (鍵, 受け手の型, `self` パラメータの宣言の型)。同上。
    pub recv_pending: Vec<(Key, Ty, Ty)>,
    /// 【担当 C の作業用】`?` のエラー変換 (`?` の鍵, 元のエラー型, 戻り値のエラー型, バリアント名)。同上。
    pub err_pending: Vec<(Key, Ty, Ty, String)>,
    /// 【担当 E】閉包の取り込み (節名 `FQN#連番` が鍵。`Ty::Closure { id }` で引く)。`check_def` の終わりに解決済みの型で入る。
    pub closure_caps: HashMap<String, Vec<Capture>>,
    /// 【S3A】領域の表 (設計 9 章)。集めるだけで解かない。
    pub regions: RegionTabs,
}

/// 領域の関係 `long: short` (`long` は `short` より長く生きる)。
#[derive(Clone, Debug)]
pub struct RegionCstr {
    /// 関係を生んだ節点 (引数・呼び出し・戻り値の式)。
    pub key: Key,
    pub long: crate::types::Region,
    pub short: crate::types::Region,
    /// 由来: `arg` (実引数 ⊒ 仮引数) / `recv` (受け手 ⊒ `self`) / `ret` (本体の値 ⊒ 戻り値の型) / `bound` (宣言の境界)。
    pub why: &'static str,
}

/// 関数の署名の領域 (省略規則を当てた後)。並びは型の前順 (外側の参照が先、`Named` のライフタイム引数・`dyn` の領域は自分の位置)。
#[derive(Clone, Debug, Default)]
pub struct SigRegions {
    pub params: Vec<(String, Vec<crate::types::Region>)>,
    pub ret: Vec<crate::types::Region>,
    /// 宣言の境界 `T: 'a` (型パラメータ名, 領域)。
    pub bounds: Vec<(String, crate::types::Region)>,
}

/// 領域の表。`TypeCk::side.regions`。
#[derive(Clone, Debug, Default)]
pub struct RegionTabs {
    /// `&e` / `&mut e` の式ごとの領域 (`Region::Var(式 ID)`)。鍵は借用式の節点。
    pub borrows: HashMap<Key, crate::types::Region>,
    /// 定義ごとの署名の領域。
    pub sigs: HashMap<DefId, SigRegions>,
    /// 領域どうしの関係 (集めるだけ)。
    pub cstrs: Vec<RegionCstr>,
}

/// 閉包の取り込み (`TypeCk::closure_caps` を式 ID の鍵に直した形。種別は `&` / `&mut` / `move`)。
#[derive(Clone, Debug, PartialEq)]
pub struct Capture {
    pub kind: String,
    pub name: String,
    pub ty: Ty,
    /// `&` / `&mut` の取り込みが借りる領域 (閉包の式 ID の `Region::Var`)。`move` は `Unk`。
    pub region: crate::types::Region,
}

/// 明示型引数の木 (`V` は `PartialEq` を持たないので、常に等しい扱いの入れ物に包む)。
#[derive(Clone, Debug, Default)]
pub struct ETargs(pub Vec<V>);

impl PartialEq for ETargs {
    fn eq(&self, _: &ETargs) -> bool {
        true
    }
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
    /// `Call` / `MCall` だけ: 書いた明示型引数 (`f::<..>`) の数 (ライフタイムを除く)。書いていなければ `None`。
    pub tf: Option<usize>,
    /// `Call` / `MCall` だけ: 書いた明示型引数の木 (型・定数・効果・panic・ライフタイムの実引数。効果の実体化が使う)。
    pub etargs: ETargs,
    /// `For` / `With` だけ: 束縛するパターン (`Other` の子には入らない)。
    pub pats: Vec<TPat>,
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
    Closure { params: Vec<TBinding>, captures: Vec<Capture>, body: Box<TExpr>, written: Vec<(String, String)> },
    /// 構造体リテラル `T { f: e, .. }` (型の FQN の書いたまま, 欄名と値の式)。規則が欄の宣言の型と値の型を照らす。
    StructLit { name: String, fields: Vec<(String, TExpr)> },
    /// 上に無い形 (`For` `While` `Loop` `Return` `Try` `TupleE` `ArrayE` ...)。種別名と子の式。
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
    /// 節点の種別 (`PBind` `PTuple` ...)。
    pub kind: &'static str,
    pub line: usize,
    pub info: Option<PatInfo>,
    /// 子のパターン (`PTuple` / `PVariant` / `PStruct` / `PRef` の中身)。
    pub children: Vec<TPat>,
    /// `PBind` の名前。
    pub name: Option<String>,
    /// `PVariant` / `PPath` / `PStruct` の経路 (書いたまま)。
    pub path: Vec<String>,
    /// `PLit` のリテラルの種類と接尾辞 (`["int", "u64"]`、接尾辞が無ければ種類だけ)。
    /// `PStruct` だけは、`children` と同じ並びの欄の名前 (移動と線形の規則が欄の場所を引く)。
    pub lit: Vec<String>,
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
    /// 本体の全式 (呼び先の経路を除く) に確定した型が付いている。
    pub complete: bool,
}

/// プログラム全体の TAST (利用者の定義だけ)。
#[derive(Clone, Debug, Default)]
pub struct Tast {
    pub bodies: std::collections::BTreeMap<DefId, TBody>,
    pub stats: Stats,
}

/// 組んだ木の被覆の数え上げ。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// 式の節点の数 (呼び先の経路を除く) と、そのうち型表に確定した型があるもの。
    pub exprs: usize,
    pub typed: usize,
    /// 呼び先の経路 (`Call` の第 1 子) の数と、型があるもの。
    pub callees: usize,
    pub callees_typed: usize,
    /// 解決先・型引数・変換の印が付いた節点の数 (A〜C の表が埋まれば増える)。
    pub with_res: usize,
    pub with_targs: usize,
    pub with_conv: usize,
    /// パターン節点の数と、`PatInfo` が付いたもの (D)。
    pub pats: usize,
    pub pats_info: usize,
}

/// 型の中に決まっていない部分 (`Unknown` / 型変数) があれば true。
fn has_unknown(t: &Ty) -> bool {
    match t {
        Ty::Unknown | Ty::Var(_) => true,
        Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().any(has_unknown),
        Ty::Ref(_, x, _) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => has_unknown(x),
        Ty::Closure { args, ret, .. } => args.iter().any(has_unknown) || has_unknown(ret),
        _ => false,
    }
}

/// 【担当 E】型表 (`TypeCk::tytab`)・`SideTabs`・`let_tys` と `V` の木から TAST を組む。
/// `V` を 1 回辿り、式の形の `match` はここにだけ置く。A〜D の表は空でも組め、埋まれば `res` `targs` `conv` `info` に入る。
/// 型が付かない節点は `ty = Ty::Unknown` (文など型表に入らない節点は `Ty::Unit`)。`TBody::complete` が本体の完全さ。
pub fn build(p: &Program, user: &[DefId], tytab: &std::collections::BTreeMap<(String, u32), crate::types::ExprTy>, side: &SideTabs, let_tys: &HashMap<Key, Ty>) -> Tast {
    let mut out = Tast::default();
    for &di in user {
        let d = &p.defs[di];
        let body = if d.kw == "fn" || d.kw == "test" {
            d.body.as_ref()
        } else if matches!(d.kw.as_str(), "const" | "static") {
            d.init.as_ref()
        } else {
            None
        };
        let Some(body) = body else { continue };
        let gn = d.gnames();
        let mut b = Builder { di, fqn: &d.name, tytab, side, let_tys, scopes: vec![HashMap::new()], gn: &gn, stats: Stats::default(), missing: false };
        let mut params = vec![];
        for (n, t) in &d.params {
            b.scopes[0].insert(n.clone(), 0);
            params.push(TBinding { name: n.clone(), ty: crate::types::from_v(t, &gn) });
        }
        let ret = crate::types::from_v(&d.ret, &gn);
        let tb = b.expr(body, false);
        let st = b.stats.clone();
        let complete = !b.missing;
        out.bodies.insert(di, TBody { def: di, params, ret, body: tb, complete });
        let s = &mut out.stats;
        s.exprs += st.exprs;
        s.typed += st.typed;
        s.callees += st.callees;
        s.callees_typed += st.callees_typed;
        s.with_res += st.with_res;
        s.with_targs += st.with_targs;
        s.with_conv += st.with_conv;
        s.pats += st.pats;
        s.pats_info += st.pats_info;
    }
    out
}

struct Builder<'a> {
    di: DefId,
    fqn: &'a str,
    tytab: &'a std::collections::BTreeMap<(String, u32), crate::types::ExprTy>,
    side: &'a SideTabs,
    let_tys: &'a HashMap<Key, Ty>,
    /// 局所の名前 → その `PBind` の ID (引数・`self` は 0)。Kasane は影を禁じるので入れ子ごとに積む。
    scopes: Vec<HashMap<String, PatId>>,
    gn: &'a std::collections::HashSet<String>,
    stats: Stats,
    missing: bool,
}

fn nodes(v: &V) -> Vec<&V> {
    match v {
        V::L(xs) => xs.iter().flat_map(nodes).collect(),
        V::Node(..) => vec![v],
        _ => vec![],
    }
}

fn nm(v: &V) -> &'static str {
    match v {
        V::Node(n, ..) => n,
        _ => "",
    }
}

fn fs(v: &V) -> &[V] {
    match v {
        V::Node(_, f, ..) => f,
        _ => &[],
    }
}

fn sv(v: &V) -> String {
    match v {
        V::S(s) | V::N(s) => s.clone(),
        V::B(b) => b.to_string(),
        _ => String::new(),
    }
}

fn lv(v: &V) -> &[V] {
    match v {
        V::L(xs) => xs,
        _ => &[],
    }
}

impl<'a> Builder<'a> {
    fn lookup(&self, n: &str) -> Option<PatId> {
        self.scopes.iter().rev().find_map(|s| s.get(n).copied())
    }

    fn bind_pat(&mut self, pat: &TPat) {
        if let (Some(n), Some(sc)) = (&pat.name, self.scopes.last_mut()) {
            sc.insert(n.clone(), pat.id);
        }
        for c in &pat.children {
            self.bind_pat(c);
        }
    }

    fn pat(&mut self, v: &V) -> TPat {
        let V::Node(kind, f, ln, id) = v else {
            return TPat { id: 0, kind: "", line: 0, info: None, children: vec![], name: None, path: vec![], lit: vec![] };
        };
        let mut children = vec![];
        let mut name = None;
        let mut path = vec![];
        let mut lit = vec![];
        match *kind {
            "PBind" => name = Some(sv(&f[0])),
            "PTuple" => children = lv(&f[0]).iter().map(|x| self.pat(x)).collect(),
            "PVariant" => {
                path = lv(&f[0]).iter().map(sv).collect();
                children = lv(&f[1]).iter().map(|x| self.pat(x)).collect()
            }
            "PPath" => path = lv(&f[0]).iter().map(sv).collect(),
            "PLit" => {
                let lf = fs(&f[0]);
                let k = lf.first().map(sv).unwrap_or_default();
                let sfx = match k.as_str() {
                    "int" => lf.get(2),
                    "float" => lf.get(1),
                    _ => None,
                };
                lit.push(k);
                if let Some(V::S(x)) = sfx {
                    lit.push(x.clone());
                }
                // リテラルの値 (網羅性が同じリテラルかを見る): 書いたままの木の文字列。
                path = vec![format!("{:?}", lf)];
                if let Some(V::B(b)) = lf.get(1) {
                    path.push(b.to_string());
                }
            }
            // `if let` / `is` が作る「他の全部」の腕 (`PWild [true]`)。
            "PWild" => {
                if !f.is_empty() {
                    lit.push("other".to_string());
                }
            }
            "PStruct" => {
                path = lv(&f[0]).iter().map(sv).collect();
                lit = lv(&f[1]).iter().filter(|x| lv(x).get(1).is_some()).map(|x| sv(&lv(x)[0])).collect();
                children = lv(&f[1]).iter().filter_map(|x| lv(x).get(1)).map(|x| self.pat(x)).collect()
            }
            "PRef" => children = vec![self.pat(&f[0])],
            _ => {}
        }
        let info = self.side.pat_tys.get(&(self.di, *id)).cloned();
        self.stats.pats += 1;
        if info.is_some() {
            self.stats.pats_info += 1;
        }
        TPat { id: *id, kind, line: crate::parser::unpack_pos(*ln).0, info, children, name, path, lit }
    }

    fn exprs(&mut self, v: &V) -> Vec<TExpr> {
        nodes(v).into_iter().map(|x| self.expr(x, false)).collect()
    }

    fn stmt(&mut self, v: &V) -> TStmt {
        match nm(v) {
            "Let" => {
                let f = fs(v);
                let init = if matches!(f[3], V::Node(..)) { Some(self.expr(&f[3], false)) } else { None };
                let mut pat = self.pat(&f[0]);
                // 型の注釈を書いた `let` の印 (`&mut` の再借用か移動かを、移動の規則が分ける)。
                if !matches!(f[2], V::Nil) {
                    pat.lit.push("ann".to_string());
                }
                // `let mut` の印 (パターンの全部の `PBind`)。
                if matches!(f[1], V::B(true)) {
                    mark_mut(&mut pat);
                }
                self.bind_pat(&pat);
                TStmt::Let { pat, init }
            }
            "Assign" => {
                let f = fs(v);
                let place = self.expr(&f[0], false);
                let value = self.expr(&f[1], false);
                TStmt::Assign { place, value }
            }
            "ExprStmt" => TStmt::Expr(self.expr(&fs(v)[0], false)),
            _ => TStmt::Expr(self.expr(v, false)),
        }
    }

    /// 式の節点を組む。`callee` は `Call` の第 1 子 (被覆の数え方だけが違う)。
    fn expr(&mut self, v: &V, callee: bool) -> TExpr {
        let V::Node(kind, f, ln, id) = v else {
            return TExpr { id: 0, line: 0, kind: TKind::Other { name: "?", children: vec![] }, ty: Ty::Unknown, place: None, conv: vec![], res: None, targs: vec![], tf: None, etargs: ETargs::default(), pats: vec![] };
        };
        let key = (self.di, *id);
        let mut place = None;
        let mut pats = vec![];
        let tkind = match *kind {
            "Lit" => TKind::Lit(f.iter().map(sv).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(":")),
            "Path" => {
                let segs: Vec<String> = lv(&f[0]).iter().map(sv).collect();
                // `p.x.y` は局所 `p` から始まる経路でも欄の連なり (呼び先の `v.len` は欄ではないので除く)。
                if !callee {
                    if let Some(pid) = self.lookup(&segs[0]) {
                        let mut pl = Place::Local(segs[0].clone(), pid);
                        for fld in &segs[1..] {
                            pl = Place::Field(Box::new(pl), fld.clone());
                        }
                        place = Some(pl);
                    }
                }
                TKind::Path(segs)
            }
            "Call" => {
                let c = self.expr(&f[0], true);
                let args = self.exprs(&f[1]);
                TKind::Call { callee: Box::new(c), args }
            }
            "MCall" => {
                let recv = self.expr(&f[0], false);
                let args = self.exprs(&f[2]);
                TKind::MCall { recv: Box::new(recv), name: sv(&f[1]), args }
            }
            "Field" => {
                let base = self.expr(&f[0], false);
                place = Some(Place::Field(Box::new(base_place(&base)), sv(&f[1])));
                TKind::Field { base: Box::new(base), name: sv(&f[1]) }
            }
            "Index" => {
                let base = self.expr(&f[0], false);
                let index = self.expr(&f[1], false);
                place = Some(Place::Index(Box::new(base_place(&base))));
                TKind::Index { base: Box::new(base), index: Box::new(index) }
            }
            "Unary" => {
                let operand = self.expr(&f[1], false);
                let op = sv(&f[0]);
                if op == "*" {
                    place = Some(Place::Deref(Box::new(base_place(&operand)), operand.ty.clone()));
                }
                TKind::Unary { op, operand: Box::new(operand) }
            }
            "Bin" => {
                let lhs = self.expr(&f[1], false);
                let rhs = self.expr(&f[2], false);
                TKind::Binary { op: sv(&f[0]), lhs: Box::new(lhs), rhs: Box::new(rhs) }
            }
            "StructLit" => {
                let fields = lv(&f[1]).iter().map(|x| (sv(&lv(x)[0]), self.expr(&lv(x)[1], false))).collect();
                TKind::StructLit { name: sv(&f[0]), fields }
            }
            "Block" => {
                self.scopes.push(HashMap::new());
                let stmts = lv(&f[0]).iter().map(|s| self.stmt(s)).collect();
                self.scopes.pop();
                TKind::Block(stmts)
            }
            "If" => {
                let cond = self.expr(&f[0], false);
                let then = self.expr(&f[1], false);
                let els = if matches!(f[2], V::Node(..)) { Some(Box::new(self.expr(&f[2], false))) } else { None };
                TKind::If { cond: Box::new(cond), then: Box::new(then), els }
            }
            "Match" => {
                let scrut = self.expr(&f[0], false);
                let mut arms = vec![];
                for a in lv(&f[1]) {
                    let a = lv(a);
                    self.scopes.push(HashMap::new());
                    let pat = self.pat(&a[0]);
                    self.bind_pat(&pat);
                    let (guard, bi) = if a.len() > 2 { (Some(self.expr(&a[1], false)), 2) } else { (None, 1) };
                    let body = self.expr(&a[bi], false);
                    self.scopes.pop();
                    arms.push(TArm { pat, guard, body });
                }
                TKind::Match { scrut: Box::new(scrut), arms }
            }
            "Closure" => {
                self.scopes.push(HashMap::new());
                let mut params = vec![];
                for q in lv(&f[0]) {
                    let q = lv(q);
                    let name = sv(&q[0]);
                    self.scopes.last_mut().unwrap().insert(name.clone(), 0);
                    params.push(TBinding { name, ty: crate::types::from_v(&q[1], self.gn) });
                }
                let body = self.expr(&f[3], false);
                self.scopes.pop();
                let captures = match self.tytab.get(&(self.fqn.to_string(), *id)).map(|e| &e.ty) {
                    Some(Ty::Closure { id: Some(node), .. }) => self.side.closure_caps.get(node).cloned().unwrap_or_default(),
                    _ => vec![],
                };
                let written = lv(&f[2]).iter().filter_map(|c| match (lv(c).first(), lv(c).get(1)) { (Some(k), Some(n)) => Some((sv(k), sv(n))), _ => None }).collect();
                TKind::Closure { params, captures, body: Box::new(body), written }
            }
            "For" => {
                let it = self.expr(&f[1], false);
                self.scopes.push(HashMap::new());
                let pat = self.pat(&f[0]);
                self.bind_pat(&pat);
                let body = self.expr(&f[2], false);
                self.scopes.pop();
                pats.push(pat);
                TKind::Other { name: "For", children: vec![it, body] }
            }
            "With" => {
                let init = self.expr(&f[2], false);
                self.scopes.push(HashMap::new());
                let mut pat = self.pat(&f[0]);
                if matches!(f[1], V::B(true)) {
                    mark_mut(&mut pat);
                }
                self.bind_pat(&pat);
                let mut children = vec![init];
                if matches!(f[3], V::Node(..)) {
                    children.push(self.expr(&f[3], false));
                }
                children.push(self.expr(&f[4], false));
                self.scopes.pop();
                pats.push(pat);
                TKind::Other { name: "With", children }
            }
            // 上に無い形 (`While` `Loop` `Return` `Try` `StructLit` `TupleE` `ArrayE` `ArrayRep` `Unsafe` `Break` `Continue` ...):
            // 子の式を順に集める (構造体リテラルの欄の組・文字列などの葉は節点でないので落ちる)。
            other => {
                let children = f.iter().flat_map(nodes).map(|x| self.expr(x, false)).collect();
                TKind::Other { name: other, children }
            }
        };
        let et = self.tytab.get(&(self.fqn.to_string(), *id));
        let ty = match et {
            Some(e) => e.ty.clone(),
            None => Ty::Unknown,
        };
        let known = et.is_some() && !has_unknown(&ty);
        if callee {
            self.stats.callees += 1;
            if known {
                self.stats.callees_typed += 1;
            }
        } else {
            self.stats.exprs += 1;
            if known {
                self.stats.typed += 1;
            } else {
                self.missing = true;
            }
        }
        let _ = self.let_tys;
        let res = self.side.resolved_fn.get(&key).cloned();
        let targs = self.side.type_args.get(&key).cloned().unwrap_or_default();
        let conv = self.side.coercions.get(&key).cloned().unwrap_or_default();
        self.stats.with_res += res.is_some() as usize;
        self.stats.with_targs += !targs.is_empty() as usize;
        self.stats.with_conv += !conv.is_empty() as usize;
        let tf = match *kind {
            "Call" => f.get(2),
            "MCall" => f.get(3),
            _ => None,
        }
        .and_then(|t| match t {
            V::L(xs) => Some(xs.iter().filter(|a| !matches!(a, V::L(y) if matches!(y.first(), Some(V::S(s)) if s == "life"))).count()),
            _ => None,
        });
        let etargs = ETargs(match *kind {
            "Call" => f.get(2),
            "MCall" => f.get(3),
            _ => None,
        }
        .map(|t| match t {
            V::L(xs) => xs.clone(),
            _ => vec![],
        })
        .unwrap_or_default());
        TExpr { id: *id, line: crate::parser::unpack_pos(*ln).0, kind: tkind, ty, place, conv, res, targs, tf, etargs, pats }
    }
}

/// `let mut` / `with mut` のパターンの `PBind` に印 `"mut"` を付ける。
fn mark_mut(p: &mut TPat) {
    if p.kind == "PBind" {
        p.lit.push("mut".to_string());
    }
    for c in &mut p.children {
        mark_mut(c);
    }
}

/// 場所でない式 (呼び出しの結果など) は一時値の場所で指す。
fn base_place(e: &TExpr) -> Place {
    e.place.clone().unwrap_or(Place::Temp(e.id))
}

/// `KSN_DUMP_TAST=1` の出力: 1 節点 1 行、字下げは木の深さ。並びは (DefId, 木の順) で安定。
pub fn dump(p: &Program, t: &Tast) -> String {
    let mut o = String::new();
    for (di, b) in &t.bodies {
        o.push_str(&format!("body {} complete={}\n", p.defs[*di].name, b.complete));
        for q in &b.params {
            o.push_str(&format!("  param {}: {}\n", q.name, q.ty.show()));
        }
        dump_expr(&b.body, 1, &mut o);
    }
    let s = &t.stats;
    o.push_str(&format!(
        "stats exprs={} typed={} callees={} callees_typed={} res={} targs={} conv={} pats={} pats_info={}\n",
        s.exprs, s.typed, s.callees, s.callees_typed, s.with_res, s.with_targs, s.with_conv, s.pats, s.pats_info
    ));
    o
}

fn ind(n: usize, o: &mut String) {
    for _ in 0..n {
        o.push_str("  ");
    }
}

fn dump_pat(q: &TPat, n: usize, o: &mut String) {
    ind(n, o);
    o.push_str(&format!("pat#{} {} L{}", q.id, q.kind, q.line));
    if let Some(nm) = &q.name {
        o.push_str(&format!(" name={}", nm));
    }
    if let Some(i) = &q.info {
        o.push_str(&format!(" ty={}", i.ty.show()));
    }
    o.push('\n');
    for c in &q.children {
        dump_pat(c, n + 1, o);
    }
}

fn dump_expr(e: &TExpr, n: usize, o: &mut String) {
    ind(n, o);
    let (label, subs): (String, Vec<&TExpr>) = match &e.kind {
        TKind::Lit(s) => (format!("Lit {}", s), vec![]),
        TKind::Path(s) => (format!("Path {}", s.join(".")), vec![]),
        TKind::Call { callee, args } => ("Call".into(), std::iter::once(&**callee).chain(args.iter()).collect()),
        TKind::MCall { recv, name, args } => (format!("MCall .{}", name), std::iter::once(&**recv).chain(args.iter()).collect()),
        TKind::Field { base, name } => (format!("Field .{}", name), vec![&**base]),
        TKind::Index { base, index } => ("Index".into(), vec![&**base, &**index]),
        TKind::Unary { op, operand } => (format!("Unary {}", op), vec![&**operand]),
        TKind::Binary { op, lhs, rhs } => (format!("Binary {}", op), vec![&**lhs, &**rhs]),
        TKind::Block(_) => ("Block".into(), vec![]),
        TKind::If { cond, then, els } => ("If".into(), [Some(&**cond), Some(&**then), els.as_deref()].into_iter().flatten().collect()),
        TKind::Match { scrut, .. } => ("Match".into(), vec![&**scrut]),
        TKind::Closure { captures, body, .. } => (format!("Closure caps=[{}]", captures.iter().map(|c| format!("{} {}: {}", c.kind, c.name, c.ty.show())).collect::<Vec<_>>().join(", ")), vec![&**body]),
        TKind::StructLit { fields, .. } => ("StructLit".into(), fields.iter().map(|(_, x)| x).collect()),
        TKind::Other { name, children } => (name.to_string(), children.iter().collect()),
    };
    o.push_str(&format!("#{} {} L{} : {}", e.id, label, e.line, if matches!(e.ty, Ty::Unknown) { "?".to_string() } else { e.ty.show() }));
    if let Some(pl) = &e.place {
        o.push_str(&format!(" place={}", show_place(pl)));
    }
    if let Some(r) = &e.res {
        o.push_str(&format!(" res={:?}", r));
    }
    if !e.targs.is_empty() {
        o.push_str(&format!(" targs=[{}]", e.targs.iter().map(|t| t.show()).collect::<Vec<_>>().join(", ")));
    }
    if !e.conv.is_empty() {
        o.push_str(&format!(" conv={:?}", e.conv));
    }
    o.push('\n');
    for x in subs {
        dump_expr(x, n + 1, o);
    }
    match &e.kind {
        TKind::Block(stmts) => {
            for s in stmts {
                match s {
                    TStmt::Let { pat, init } => {
                        ind(n + 1, o);
                        o.push_str("Let\n");
                        dump_pat(pat, n + 2, o);
                        if let Some(i) = init {
                            dump_expr(i, n + 2, o);
                        }
                    }
                    TStmt::Expr(x) => dump_expr(x, n + 1, o),
                    TStmt::Assign { place, value } => {
                        ind(n + 1, o);
                        o.push_str("Assign\n");
                        dump_expr(place, n + 2, o);
                        dump_expr(value, n + 2, o);
                    }
                }
            }
        }
        TKind::Match { arms, .. } => {
            for a in arms {
                ind(n + 1, o);
                o.push_str("Arm\n");
                dump_pat(&a.pat, n + 2, o);
                if let Some(g) = &a.guard {
                    dump_expr(g, n + 2, o);
                }
                dump_expr(&a.body, n + 2, o);
            }
        }
        _ => {}
    }
}

fn show_place(p: &Place) -> String {
    match p {
        Place::Local(n, id) => format!("{}@{}", n, id),
        Place::Field(b, f) => format!("{}.{}", show_place(b), f),
        Place::Index(b) => format!("{}[]", show_place(b)),
        Place::Deref(b, _) => format!("*{}", show_place(b)),
        Place::Temp(id) => format!("tmp#{}", id),
    }
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

/// `KSN_DUMP_SIDE=1` の領域の表 (1 行目が `# regions`)。借用・署名・関係を、定義名と式 ID の順に出す。
pub fn dump_regions(p: &Program, side: &SideTabs) -> String {
    let rs = &side.regions;
    let show = |v: &[crate::types::Region]| v.iter().map(|r| r.show()).collect::<Vec<_>>().join(" ");
    let mut rows: Vec<String> = vec![];
    for (k, r) in &rs.borrows {
        rows.push(format!("borrow\t{}\t{}\t{}", p.defs[k.0].name, k.1, r.show()));
    }
    for (di, s) in &rs.sigs {
        let ps = s.params.iter().map(|(n, r)| format!("{}:[{}]", n, show(r))).collect::<Vec<_>>().join(" ");
        let bs = s.bounds.iter().map(|(n, r)| format!("{}:{}", n, r.show())).collect::<Vec<_>>().join(" ");
        rows.push(format!("sig\t{}\t0\tparams {} | ret [{}] | bounds {}", p.defs[*di].name, ps, show(&s.ret), bs));
    }
    // 束縛の型 (領域つき)。`static=true` は型の領域がすべて `'static` (G40 の `static_expr` を型で言い換えたもの)。
    for (k, i) in &side.pat_tys {
        if let Some((n, t, _)) = &i.bind {
            rows.push(format!("bind\t{}\t{}\t{} : {} static={}", p.defs[k.0].name, k.1, n, t.show_r(), t.refs_all_static()));
        }
    }
    for c in &rs.cstrs {
        rows.push(format!("cstr\t{}\t{}\t{} : {} ({})", p.defs[c.key.0].name, c.key.1, c.long.show(), c.short.show(), c.why));
    }
    rows.sort();
    let mut o = String::from("# regions\n");
    for r in rows {
        o.push_str(&r);
        o.push('\n');
    }
    o
}
