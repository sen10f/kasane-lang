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
use crate::tast::Callee;
use crate::program::{Def, Program, Target};
use crate::check::borrowck::match_close;

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Unknown,
    Never,
    Unit,
    Prim(String),
    /// 3 つ目は型実引数のライフタイム (`Foo<'a, T>` の `'a`。書いた順)。型の等しさには入れない。
    Named(String, Vec<Ty>, Rgs),
    /// 3 つ目は領域 (ライフタイム)。**型の等しさには入れない** (`Region` の `==` は常に真。厳密な比較は `Region::same`)。
    Ref(bool, Box<Ty>, Region),
    /// 生ポインタ `*const T` (false) / `*mut T` (true) (3.8)。`&T` とは別の型で、暗黙には相互変換しない
    /// (`*mut` → `*const` の縮小だけ暗黙に許す。`*const` → `*mut` は `core.ptr.as_mut` のみ)。
    Ptr(bool, Box<Ty>),
    Tuple(Vec<Ty>),
    /// 2 つ目は整数リテラルで書かれた長さ (定数名・式は `None` = 比べない)。
    Array(Box<Ty>, Option<u128>),
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
    /// 3 つ目は `dyn Tr + 'a` の `'a` (なければ `Unk`)。型の等しさには入れない。
    Dyn(String, TArgs, Region),
}

/// 参照の領域 (設計 9 章)。型の同一性には入れない (`==` は常に真)。厳密な比較は `Region::same`。
#[derive(Clone, Debug)]
pub enum Region {
    /// 分からない・まだ付けていない。
    Unk,
    /// `'static`。
    Static,
    /// 関数の型パラメータの名前付きライフタイム (`'a` の `a`)。
    Named(String),
    /// 関数の中の局所の借用ごとの領域の変数 (借用の式の ID。`SideTabs::regions` の `borrows`)。
    Var(u32),
    /// 省略規則で決まる入力側の領域 (署名の入力で、省略した領域を左から数えた番号。`&self` は 0)。
    Elided(u32),
    /// 呼び出しごとに実体化した、呼び出し先の署名の領域 (呼び出し節点の ID, 署名の中の領域 `Named` / `Elided`)。
    Inst(u32, Box<Region>),
}

/// 型実引数のライフタイムの並び。型の同一性には入れない (`==` は常に真)。
#[derive(Clone, Debug, Default)]
pub struct Rgs(pub Vec<Region>);

impl PartialEq for Rgs {
    fn eq(&self, _: &Rgs) -> bool {
        true
    }
}

impl PartialEq for Region {
    fn eq(&self, _: &Region) -> bool {
        true
    }
}

impl Region {
    /// 構文木のライフタイム (`'a` / `'static` / `'_`。なければ `Nil`) から。
    pub fn from_life(v: &V) -> Region {
        match v {
            V::S(t) if t == "'static" => Region::Static,
            V::S(t) if t == "'_" => Region::Unk,
            V::S(t) if t.starts_with('\'') => Region::Named(t[1..].to_string()),
            _ => Region::Unk,
        }
    }
    /// 領域まで含めた厳密な等しさ。
    pub fn same(&self, o: &Region) -> bool {
        match (self, o) {
            (Region::Unk, Region::Unk) | (Region::Static, Region::Static) => true,
            (Region::Named(a), Region::Named(b)) => a == b,
            (Region::Var(a), Region::Var(b)) | (Region::Elided(a), Region::Elided(b)) => a == b,
            (Region::Inst(a, x), Region::Inst(b, y)) => a == b && x.same(y),
            _ => false,
        }
    }
    pub fn show(&self) -> String {
        match self {
            Region::Unk => "'_".into(),
            Region::Static => "'static".into(),
            Region::Named(n) => format!("'{}", n),
            Region::Var(n) => format!("'?{}", n),
            Region::Elided(n) => format!("'e{}", n),
            Region::Inst(c, r) => format!("{}@{}", r.show(), c),
        }
    }
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
    /// 領域を写す (前順。`Ref` の領域・`Named` のライフタイム引数・`Dyn` の領域)。
    pub fn map_regions(&self, f: &mut dyn FnMut(&Region) -> Region) -> Ty {
        match self {
            Ty::Ref(m, x, r) => {
                let r2 = f(r);
                Ty::Ref(*m, Box::new(x.map_regions(f)), r2)
            }
            Ty::Named(n, a, rs) => {
                let rs2 = Rgs(rs.0.iter().map(|r| f(r)).collect());
                Ty::Named(n.clone(), a.iter().map(|x| x.map_regions(f)).collect(), rs2)
            }
            Ty::Dyn(n, ta, r) => Ty::Dyn(n.clone(), ta.clone(), f(r)),
            Ty::Ptr(m, x) => Ty::Ptr(*m, Box::new(x.map_regions(f))),
            Ty::Tuple(xs) => Ty::Tuple(xs.iter().map(|x| x.map_regions(f)).collect()),
            Ty::Array(x, n) => Ty::Array(Box::new(x.map_regions(f)), *n),
            Ty::Slice(x) => Ty::Slice(Box::new(x.map_regions(f))),
            Ty::Closure { args, ret, cap, id, eff } => Ty::Closure { args: args.iter().map(|x| x.map_regions(f)).collect(), ret: Box::new(ret.map_regions(f)), cap: cap.clone(), id: id.clone(), eff: eff.clone() },
            other => other.clone(),
        }
    }
    /// 型の中の領域を前順に並べる (`map_regions` と同じ順)。
    pub fn regions(&self) -> Vec<Region> {
        let mut out = vec![];
        self.map_regions(&mut |r| {
            out.push(r.clone());
            r.clone()
        });
        out
    }
    /// 参照を 1 つ以上含み、領域がすべて `'static` か (`&'static str`・`Option<&'static T>` など)。
    pub fn refs_all_static(&self) -> bool {
        let rs = self.regions();
        !rs.is_empty() && rs.iter().all(|r| matches!(r, Region::Static))
    }
    /// 一番外側の参照の領域 (`&T` / `&mut T` のとき)。
    pub fn outer_region(&self) -> Option<&Region> {
        match self {
            Ty::Ref(_, _, r) => Some(r),
            _ => None,
        }
    }
    /// 領域つきの表示 (`&'a T`。`show` は診断の文面なので領域を出さない)。
    pub fn show_r(&self) -> String {
        match self {
            Ty::Ref(m, t, r) => format!("&{} {}{}", r.show(), if *m { "mut " } else { "" }, t.show_r()),
            Ty::Named(n, a, rs) => {
                if let Some(short) = alloc_short_name(n, a) {
                    return short;
                }
                let mut xs: Vec<String> = rs.0.iter().map(|r| r.show()).collect();
                xs.extend(a.iter().map(|x| x.show_r()));
                if xs.is_empty() { n.clone() } else { format!("{}<{}>", n, xs.join(", ")) }
            }
            Ty::Dyn(t, _, r) => format!("dyn {} + {}", t, r.show()),
            Ty::Tuple(ts) => format!("({})", ts.iter().map(|x| x.show_r()).collect::<Vec<_>>().join(", ")),
            Ty::Slice(t) => format!("[{}]", t.show_r()),
            Ty::Array(t, _) => format!("[{}; _]", t.show_r()),
            Ty::Ptr(m, t) => format!("{} {}", if *m { "*mut" } else { "*const" }, t.show_r()),
            other => other.show(),
        }
    }
    pub(crate) fn strip(&self) -> &Ty {
        let mut t = self;
        while let Ty::Ref(_, inner, _) = t {
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
            Ty::Named(n, a, _) => {
                if let Some(short) = alloc_short_name(n, a) {
                    return short;
                }
                if a.is_empty() {
                    n.clone()
                } else {
                    format!("{}<{}>", n, a.iter().map(|x| x.show()).collect::<Vec<_>>().join(", "))
                }
            }
            Ty::Ref(m, t, _) => format!("&{}{}", if *m { "mut " } else { "" }, t.show()),
            Ty::Ptr(m, t) => format!("{} {}", if *m { "*mut" } else { "*const" }, t.show()),
            Ty::Tuple(ts) => format!("({})", ts.iter().map(|x| x.show()).collect::<Vec<_>>().join(", ")),
            Ty::Array(t, n) => match n { Some(n) => format!("[{}; {}]", t.show(), n), None => format!("[{}; _]", t.show()) },
            Ty::Slice(t) => format!("[{}]", t.show()),
            Ty::Param(p) => p.clone(),
            Ty::Const(c) => c.clone(),
            Ty::Closure { args, ret, .. } => {
                format!("fn({}) -> {}", args.iter().map(|x| x.show()).collect::<Vec<_>>().join(", "), ret.show())
            }
            Ty::Dyn(t, _, _) => format!("dyn {}", t),
        }
    }
    pub(crate) fn has_param(&self) -> bool {
        match self {
            Ty::Param(_) => true,
            Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().any(|x| x.has_param()),
            Ty::Ref(_, x, _) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => x.has_param(),
            Ty::Closure { args, ret, .. } => args.iter().any(|x| x.has_param()) || ret.has_param(),
            _ => false,
        }
    }
}

pub(crate) const INTS: &[&str] = &["i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize"];
const FLOATS: &[&str] = &["f32", "f64"];

/// 型の構文木 (V) を `Ty` にする。`params` に含まれる名前は型パラメータ。
pub fn from_v(v: &V, params: &HashSet<String>) -> Ty {
    crate::s5_meter::count_active("from_v");
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
        "array" => Ty::Array(Box::new(from_v(tel(v, 1), params)), array_len(tel(v, 2))),
        "slice" => Ty::Slice(Box::new(from_v(tel(v, 1), params))),
        "ref" => Ty::Ref(matches!(tel(v, 1), V::B(true)), Box::new(from_v(tel(v, 2), params)), Region::from_life(tel(v, 3))),
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
            // デフォルト確保器の短縮名 (`std.vec.Vec` など) を正式名 (`alloc.vec.Vec`) に置き換える。
            // アロケータの型実引数が省略されている (`std.string.String` のように `<>` なし) ときは、
            // アロケータ位置に `std.alloc.System` を補う。長い形式 `alloc.x.Y` はそのまま。
            let (name, mut args) = {
                let alloc = std_to_alloc(&name);
                let mut args: Vec<Ty> = items(tel(v, 2))
                    .iter()
                    .filter(|a| matches!(a, V::L(_)) && tag(a) != "life")
                    // 型実引数に書かれた効果・panic の実引数は、型の同一性に入れない (定数と同じ「どれでもよい」印)。
                    .map(|a| if matches!(tag(a).as_str(), "effects" | "panic") { Ty::Const("effect".into()) } else { const_targ(a).map_or_else(|| from_v(a, params), Ty::Const) })
                    .collect();
                if let Some(a) = alloc {
                    let (_std, _alloc, total) = ALLOC_ALIASES.iter().find(|(_, al, _)| *al == a).unwrap();
                    if args.len() == total.saturating_sub(1) {
                        // アロケータが省略済み: `std.alloc.System` を補う。
                        args.push(Ty::Named("std.alloc.System".into(), vec![], crate::types::Rgs::default()));
                    }
                    (a.to_string(), args)
                } else {
                    (name.to_string(), args)
                }
            };
            let lifes = items(tel(v, 2)).iter().filter(|a| matches!(a, V::L(_)) && tag(a) == "life").map(|a| Region::from_life(tel(a, 1))).collect();
            Ty::Named(name, args, Rgs(lifes))
        }
        // `dyn Tr` (のトレイト名)。型引数・効果・ライフタイムは解決の対象にしない (近似)。
        "dyn" => Ty::Dyn(st(tel(tel(v, 1), 0)), TArgs(items(tel(tel(v, 1), 1)).to_vec()), Region::from_life(tel(v, 2))),
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

/// パラメータの宣言 `F` / `&mut F` / `&F` から、Fn 境界の実体化に使う型パラメータの名前。
/// 参照は 1 段だけ透ける (貸すだけで移さない呼び出し、`core.ffi.trampoline(&mut cb)`)。
fn fn_param_name(t: &Ty) -> Option<String> {
    match t {
        Ty::Param(n) => Some(n.clone()),
        Ty::Ref(_, p, _) => match &**p {
            Ty::Param(n) => Some(n.clone()),
            _ => None,
        },
        _ => None,
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

/// 2 つの `Send` / `Sync` の判定の連言。どちらかが満たさないなら `Some(false)`、決まらないものが混じれば `None`。
fn and_marker(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

/// 型実引数が省かれた位置の既定。`alloc.string.String` と `<A>` を省いて書くと `A` は省かれるので、
/// アロケータの位置は既定の `std.alloc.SYSTEM` の型とみなす (`Allocator` は `Send` を境界で要求されるので、
/// どのアロケータでも判定は変わらない)。それ以外の位置は決められないので `None` (fail-closed)。
fn omitted_arg(g: &(String, String, V)) -> Option<Ty> {
    let allocator = items(&g.2).iter().any(|b| matches!(b, V::L(bx) if st(&bx[0]) == "alloc.alloc.Allocator"));
    allocator.then(|| Ty::Named("std.alloc.System".into(), vec![], crate::types::Rgs::default()))
}

/// デフォルト確保器 (`std.alloc.System`) を短い型名で書くためのエイリアス。
/// `std.vec.Vec<T>` は `alloc.vec.Vec<T, std.alloc.System>` と同じ型。
/// (短縮名 `std..`, 正式名 `alloc..`, 型実引数の総数。最後の型実引数がアロケータ位置)
pub(crate) const ALLOC_ALIASES: &[(&str, &str, usize)] = &[
    ("std.vec.Vec", "alloc.vec.Vec", 2),
    ("std.string.String", "alloc.string.String", 1),
    ("std.collections.HashMap", "alloc.collections.HashMap", 3),
    ("std.collections.BtreeMap", "alloc.collections.BtreeMap", 3),
    ("std.collections.HashSet", "alloc.collections.HashSet", 2),
    ("std.collections.VecDeque", "alloc.collections.VecDeque", 2),
    ("std.boxed.Box", "alloc.boxed.Box", 2),
];

/// `alloc.x.Y.<ctor>` のうちアロケータを第1パラメータに取るコンストラクタ名。
/// `spec/stdlib/alloc/**/*.sig` のコンストラクタ署名から導く（すべて第1パラメータが
/// `A: alloc.alloc.Allocator`）。`std.x.Y.<ctor>()`（アロケータ省略）を
/// `alloc.x.Y.<ctor>(std.alloc.SYSTEM, ..)` に直す際に使う。
pub(crate) const ALLOC_CTORS: &[(&str, &[&str])] = &[
    ("alloc.vec.Vec", &["new", "with_capacity"]),
    ("alloc.string.String", &["new", "from_str"]),
    ("alloc.collections.HashMap", &["new"]),
    ("alloc.collections.BtreeMap", &["new"]),
    ("alloc.collections.VecDeque", &["new"]),
    ("alloc.boxed.Box", &["new"]),
];

/// `alloc.x.Y.<ctor>` がアロケータを第1パラメータに取るコンストラクタか。
pub(crate) fn alloc_ctor_takes_allocator_first(alloc: &str, ctor: &str) -> bool {
    ALLOC_CTORS
        .iter()
        .any(|(a, ctors)| *a == alloc && ctors.contains(&ctor))
}

/// 短縮名 `std.vec.Vec` に対応する正式名 `alloc.vec.Vec`。`from_v` で短縮名を正式名に置き換えるために使う。
pub(crate) fn std_to_alloc(std: &str) -> Option<&str> {
    ALLOC_ALIASES.iter().find(|(s, _, _)| *s == std).map(|(_, a, _)| *a)
}

/// 正式名 `alloc.vec.Vec` に対応する短縮名 `std.vec.Vec`。型宣言の登録時に短縮名も型表に入れるために使う。
pub(crate) fn alloc_std_alias(alloc: &str) -> Option<&str> {
    ALLOC_ALIASES.iter().find(|(_, a, _)| *a == alloc).map(|(s, _, _)| *s)
}

/// `alloc.x.Y<実引数>` がデフォルト確保器 (`std.alloc.System`) を短縮して書ける形なら、
/// `std.x.Y<非アロケータ実引数>` の表示を返す。アロケータが明示的に `System` なら短縮する。
/// アロケータを省略した `alloc.x.Y<T>` は型引数が足りず検査エラーなので、
/// 短縮しない（診断に短い形を出すと有効に見えてしまうため）。
pub(crate) fn alloc_short_name(n: &str, args: &[Ty]) -> Option<String> {
    let Some((std, alloc, total)) = ALLOC_ALIASES.iter().find(|(_, a, _)| *a == n).copied() else { return None };
    let last_is_system = args.last().map_or(false, |t| matches!(t, Ty::Named(s, _, _) if s == "std.alloc.System"));
    if !(args.len() == total && last_is_system) {
        return None;
    }
    let non_alloc: Vec<String> = args.iter().take(total.saturating_sub(1)).map(|t| t.show()).collect();
    if non_alloc.is_empty() {
        return Some(std.to_string());
    }
    Some(format!("{}<{}>", std, non_alloc.join(", ")))
}


/// 署名の省略規則 (E3021 と同じ): 入力の省略した領域に左から `Elided(0..)` を付け、戻り値の省略した領域は
/// `&self` / `&mut self` があればその領域、なければ入力の領域がちょうど 1 つのときそれ、それ以外は `Unk` のまま。
/// `params` は (名前, 型)。
pub fn elide_sig(params: &[(String, Ty)], ret: &Ty) -> (Vec<Ty>, Ty) {
    let mut n = 0u32;
    let mut all: Vec<Region> = vec![];
    let ps: Vec<Ty> = params
        .iter()
        .map(|(_, t)| {
            t.map_regions(&mut |r| {
                let r2 = if matches!(r, Region::Unk) {
                    n += 1;
                    Region::Elided(n - 1)
                } else {
                    r.clone()
                };
                all.push(r2.clone());
                r2
            })
        })
        .collect();
    let self_r = match params.first() {
        Some((nm, Ty::Ref(_, _, _))) if nm == "self" => ps[0].outer_region().cloned(),
        _ => None,
    };
    let only = if all.len() == 1 { Some(all[0].clone()) } else { None };
    let fill = self_r.or(only);
    let r = match fill {
        Some(f) => ret.map_regions(&mut |r| if matches!(r, Region::Unk) { f.clone() } else { r.clone() }),
        None => ret.clone(),
    };
    (ps, r)
}

/// クロージャの取り込みの型 (5.5)。`&a` は `&T`、`&mut a` は `&mut T`、`move a` は `T`。
fn capture_ty(kind: &str, t: Ty) -> Ty {
    match kind {
        "&" => Ty::Ref(false, Box::new(t), crate::types::Region::Unk),
        "&mut" => Ty::Ref(true, Box::new(t), crate::types::Region::Unk),
        _ => t,
    }
}

/// 型パラメータを束縛で置き換える。束縛のないものは `Unknown`。
pub(crate) fn subst(t: &Ty, bind: &HashMap<String, Ty>) -> Ty {
    match t {
        Ty::Param(p) => bind.get(p).cloned().unwrap_or(Ty::Unknown),
        Ty::Named(n, a, rs) => Ty::Named(n.clone(), a.iter().map(|x| subst(x, bind)).collect(), rs.clone()),
        Ty::Ref(m, x, rg) => Ty::Ref(*m, Box::new(subst(x, bind)), rg.clone()),
        Ty::Ptr(m, x) => Ty::Ptr(*m, Box::new(subst(x, bind))),
        Ty::Tuple(xs) => Ty::Tuple(xs.iter().map(|x| subst(x, bind)).collect()),
        Ty::Array(x, n) => Ty::Array(Box::new(subst(x, bind)), *n),
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

/// メソッド呼び出し節点の位置の記録 (LSP の記号の特定用)。`call` は呼び出し節点の式 ID (`SideTabs::resolved_fn` の鍵の second 成分)。
/// `pos` は名前の位置: `x.m(..)` (MCall) は名前トークンの位置で `seg` は `usize::MAX`、`x.m(..)` の経路形 (Call) は
/// 経路の先頭の位置で `seg` は呼ぶ名前の添字 (経路の最後の成分)。
#[derive(Clone, Debug)]
pub struct CallSite {
    pub writer: usize,
    pub pos: usize,
    pub seg: usize,
    pub call: u32,
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
        "clone" => Some("core.ops.Clone"),
        "try_clone" => Some("alloc.alloc.TryClone"),
        "hash" => Some("core.hash.Hash"),
        _ => None,
    }
}

/// `Copy` / `Clone` の境界・`.clone()` の充足 (4.2): `&mut T` と、それを含む型 (`Option<&mut T>`・タプル・配列) は Copy でも Clone でもない。
/// 閉包は取り込みがすべて Copy / Clone のときだけ (`&x` は常に可、`&mut x` は不可、`move` は中身で決める。`FnOnce` は不可)。
/// `caps` は閉包の式 ID から取り込みの (種別, 型) の列。`leaf` は構造で決まらない型 (基本型・名前つきの型・型パラメータ) の判定。
pub(crate) fn marker_unsat(t: &Ty, caps: &dyn Fn(&str) -> Option<Vec<(String, Ty)>>, leaf: &dyn Fn(&Ty) -> Option<Ty>) -> Option<Ty> {
    match t {
        Ty::Ref(m, _, _) => m.then(|| t.clone()),
        Ty::Tuple(xs) => xs.iter().find_map(|x| marker_unsat(x, caps, leaf)),
        Ty::Array(x, _) | Ty::Slice(x) => marker_unsat(x, caps, leaf),
        Ty::Closure { cap, id, .. } => {
            if *cap == Cap::FnOnce {
                return Some(t.clone());
            }
            let cs = id.as_ref().and_then(|n| caps(n))?;
            for (k, ct) in &cs {
                match k.as_str() {
                    "&mut" => return Some(t.clone()),
                    "move" => {
                        if marker_unsat(ct, caps, leaf).is_some() {
                            return Some(t.clone());
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        Ty::Named(n, args, _) => {
            if !matches!(n.as_str(), "Option" | "Result" | "core.option.Option" | "core.result.Result") {
                if let Some(l) = leaf(t) {
                    return Some(l);
                }
            }
            args.iter().find_map(|a| marker_unsat(a, caps, leaf))
        }
        _ => leaf(t),
    }
}

/// 定義 `d` の型パラメータ `pn` が境界として `tr` を持つか (スーパートレイト `requires` を推移的に辿る。
/// `TryClone` は `Clone` から成り立つ)。`pn` が `d` の型パラメータでなければ None (分からない: 黙る)。
/// 境界に解決できないトレイト名があれば黙って Some(true) (そちらは別の診断が出る)。
pub(crate) fn param_bound_implies(p: &Program, d: &Def, pn: &str, tr: &str) -> Option<bool> {
    let g = d.generics.iter().find(|g| g.0 == "type" && g.1 == pn)?;
    let mut todo: Vec<String> = items(&g.2).iter().filter_map(|b| if let V::L(bx) = b { Some(st(&bx[0])) } else { None }).collect();
    let mut seen: HashSet<String> = HashSet::new();
    while let Some(b) = todo.pop() {
        if !seen.insert(b.clone()) {
            continue;
        }
        if b == tr || (tr == "alloc.alloc.TryClone" && b == "core.ops.Clone") {
            return Some(true);
        }
        match p.types.get(&b) {
            Some(&i) if p.defs[i].kw == "trait" => {
                for (sup, _) in &p.defs[i].requires {
                    match crate::check::resolve_trait_fqn(p, sup) {
                        Some(f) => todo.push(f),
                        None => return Some(true),
                    }
                }
            }
            _ => return Some(true),
        }
    }
    Some(false)
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
    /// D17 (E2053 冗長な注釈) を出すか。整数リテラルの接尾辞を書き戻す前の木 (`infer::annotate_int_lits`) で測る。
    /// 書き戻し後の木では `1` が `1u32` になり、注釈が冗長に見えてしまうため。
    pub redundant_on: bool,
    // 現在の関数
    cur: usize,
    /// 規則側 (TAST) から `auto_marker` を呼ぶときの、型パラメータの境界を読む定義 (None なら `cur`)。
    marker_def: std::cell::Cell<Option<usize>>,
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
    /// 鍵は (定義の添字, 束縛する `PBind` ノードの式 ID)。`with` の本体の値 (`$with_val`) だけは `With` ノードの式 ID。
    /// 式 ID は本体ごとに一意なので、同じ行の同名の束縛も別の項になる (旧: (定義, 行, 名前) で衝突して `Unknown`)。
    pub let_tys: HashMap<(usize, u32), Ty>,
    /// 旧い鍵 (定義の添字, `let` の行, 名前) の導出の表。行と名前で引く検査 (`refs_check`・`plain_binds` など、
    /// 式の木を持たない側) だけが使う。同じ行の同名で型の違う束縛は `Unknown` に潰す (旧来の挙動のまま)。
    pub let_tys_line: HashMap<(usize, usize, String), Ty>,
    /// 型検査が効果の実体化で `suspend` を含むと決めた呼び出し (書き手の定義, 式の位置)。借用検査が中断点にする (E3050)。
    pub susp_lines: HashSet<(usize, usize)>,
    let_pending: Vec<(usize, String, u32, Ty)>,
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
    /// メソッド呼び出し・フィールド参照のレシーバの型 (改名ツール用)。
    pub recv_log: Vec<RecvSite>,
    /// メソッド呼び出し節点の位置 (LSP の記号の特定が、位置から `resolved_fn` を引くのに使う)。
    pub call_sites: Vec<CallSite>,
    /// 型パラメータの境界で解決したメソッド呼び出し (書き手, 呼び出しの式 ID) → `トレイトFQN.メソッド名`。
    /// `call_res` / `resolved_fn` には載らない (候補は宣言だけで、定義を持たない)。LSP の定義・ホバーが使う。
    pub bound_sites: HashMap<(usize, u32), String>,
    /// `trait_bound_method` が見つけた境界のメソッドの名前 (`call_method` が式 ID と結び付けて取る)。
    bound_hit: Option<String>,
    /// 直近のトレイトメソッドの呼び出し (`dyn` 越し・境界越し) の実体化した仮引数の型 (`call_method` が取って側の表へ移す)。
    wants_tmp: Option<Vec<Ty>>,
    /// メソッド呼び出しごと (書き手の定義, 呼び出しノードの式 ID) の解決先。メソッド形は `MCall`、`x.m(..)` の経路形は `Call` のノード。`None` は決められない呼び出し
    /// (借用検査は安全側に倒す)。候補が複数なら引数の個数が合うものすべて。
    pub call_res: HashMap<(usize, u32), Option<Vec<usize>>>,
    last_fit: Option<Vec<usize>>,
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
    cap_refs: Vec<String>,
    nest: u32,
    /// 参照で取り込んだ Copy な名前 (閉包の本体の間だけ)。
    /// 参照を分解して束縛している最中の既定の束縛モード (`Some(true)` は `&mut`、`Some(false)` は `&`)。
    pat_mode: Option<bool>,
    /// いま束縛しているパターンの値が、型を確実に知っている出どころ (リテラル・注釈・関数の呼び出し・構造体リテラル・確かな名前) か。
    pat_exact: bool,
    /// `Box.into_dyn` の呼び出しの (元の `T`, 変換先 `D`, 行)。関数の推論が終わってから検査する。
    unsize: Vec<(Ty, Ty, usize)>,
    pat_line: usize,
    /// いま推論している式の行 (関数型への変換の効果検査の報告位置)。
    cur_line: usize,
    /// extern の引数の変換は E5005 が見るので、関数型の効果検査を止める。
    conv_quiet: bool,
    /// 入れ子のループごとの「`break` があったか」。
    loop_breaks: Vec<bool>,
    /// 文として捨てられる `else` の無い `if` の節点 (`{ if c { 値 } 次の文 }`)。値は使われないので型を検査しない。
    discard_if: Option<u32>,
    /// 名前が解決できない式 (別の診断が報告するので E2044 にしない)。
    quiet_sites: HashSet<usize>,
    /// 局所変数の名前 → 束縛の記録 (使用箇所から原因の束縛へたどる)。
    local_sites: HashMap<String, usize>,
    /// クロージャ節 → 取り込んだ名前 (取り込みの種類, 名前, 取り込んだ値の型)。spawn の `Send` 検査 (5.5) と、
    /// クロージャの自動導出 (`auto_marker`) が使う。
    closure_caps: HashMap<String, Vec<(String, String, Ty)>>,
    /// クロージャ節 → `move` で取り込んだ名前のうち、取り込み元が `'static` と分かるもの (5.5 の spawn 検査で参照を許す)。
    closure_static: HashMap<String, HashSet<String>>,
    /// `std.thread.spawn` に渡したクロージャ (節, 行)。関数の推論が終わってから取り込みの `Send` を見る。
    spawn_closures: Vec<(String, usize)>,
    /// `std.task.spawn` / `block_on_threads` に渡したクロージャ (定義, 節)。枠の `Send` (E2074) の根。
    pub task_roots: Vec<(usize, String)>,
    /// 関数を値として使った箇所の、`Send` / `Sync` の境界つきの型引数 (定義の終わりに検査する、5.5)。
    fn_value_markers: Vec<(String, bool, Ty, usize)>,
    /// 呼び出しの実引数ごとの「`'static` の値と式から分かる」印 (`call` が `call_def` へ渡す)。
    arg_static: Vec<bool>,
    /// `'static` の境界を持つ位置へ渡した値: (型, 出どころが `'static`, 行, 説明)。関数の終わりに型が決まってから見る。
    static_checks: Vec<(Ty, bool, usize, String)>,
    /// `HashMap` の `get` / `get_mut` / `contains_key` / `remove` の呼び出し (受け手の型・メソッド名・行)。定義の終わりに、
    /// キーが `String` と分かったものを E2054 にする (キーの型は後の `insert` で決まることがあるので、その場では見ない)。
    oneway_sites: Vec<(Ty, String, usize)>,
    /// 値が `'static` と分かった `match` 式の ID (全腕の値が `static_expr`)。
    static_nodes: HashSet<u32>,
    /// 推論中のクロージャ本体の効果 (`closure_nodes` と同じ深さのスタック)。型で解決できた呼び出しの
    /// 効果と添字の panic だけを足す (下からの近似。分からない呼び出しは足さない = 誤検出を出さない)。
    /// 3 つ目は「推論が確実でない」印 (推論された関数 `infer.rs` の効果を含む呼び出しがあった、
    /// または内側の確実でないクロージャを含む)。確実でない効果は境界の検査 (`check_closure_bounds`) に使わない。
    eff_acc: Vec<(BTreeSet<String>, bool, bool)>,
    /// クロージャの節 → 本体から推論した効果 (uses, panics may か, 確実でないか)。
    closure_effs: HashMap<String, (BTreeSet<String>, bool, bool)>,
    /// 型推論の被覆率の計測 (`KSN_TYPE_COVERAGE=1` のときだけ `Some`。typecov.rs)。
    pub cov: Option<crate::typecov::Cov>,
    /// 型表の未解決分: 式 ID → (種別, 行, 型)。いま推論中の関数の分で、`check_def` の終わりに解決して `tytab` へ移す。
    expr_pending: HashMap<u32, (String, usize, Ty)>,
    /// 型表: (関数の FQN, 式 ID) → 型。式 ID 0 (未採番のノード) は入れない。`expr_type` / `dump_types` で読む。
    pub tytab: BTreeMap<(String, u32), ExprTy>,
    /// TAST の側の表 (`tast.rs`)。担当 A〜D が埋める。今は空。
    pub side: crate::tast::SideTabs,
    /// 担当 B: この定義で記録した呼び出しの型引数 (呼び出しノードの ID, 型変数のまま)。`check_def` の終わりに `deep` して `side.type_args` へ。
    targs_pending: Vec<(u32, Vec<Ty>)>,
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
        TypeCk { p, owners, trait_methods, diags: vec![], redundant_on: false, cur: 0, marker_def: std::cell::Cell::new(None), scopes: vec![], vars: vec![], kinds: vec![], escaped: HashSet::new(), mutable: HashSet::new(), lits: vec![], rets: vec![], let_tys: HashMap::new(), let_tys_line: HashMap::new(), susp_lines: HashSet::new(), let_pending: vec![], edges: Default::default(), closure_nodes: vec![], closure_seq: 0, fn_closures: HashMap::new(), param_called: HashMap::new(), closure_asts: Default::default(), eff_acc: vec![], closure_effs: HashMap::new(), cov, expr_pending: HashMap::new(), tytab: BTreeMap::new(), side: Default::default(), targs_pending: vec![], recv_log: vec![], call_sites: vec![], bound_sites: HashMap::new(), bound_hit: None, wants_tmp: None, call_res: HashMap::new(), last_fit: None, recv_start: 0, path_pos: 0, strict: strict_types(), sites: vec![], site_stack: vec![], last_site: None, pat_src: None, nest: 0, cap_refs: vec![], pat_mode: None, pat_exact: false, unsize: vec![], pat_line: 0, cur_line: 0, conv_quiet: false, loop_breaks: vec![], discard_if: None, quiet_sites: HashSet::new(), local_sites: HashMap::new(), closure_caps: HashMap::new(), closure_static: HashMap::new(), spawn_closures: vec![], task_roots: vec![], fn_value_markers: vec![], arg_static: vec![], static_checks: vec![], oneway_sites: vec![], static_nodes: HashSet::new() }
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
        let mut msg = msg;
        if matches!(code, "E2023" | "E2024") && !self.cap_refs.is_empty() && msg.contains('&') {
            msg.push_str(&format!("; a name in `capture [&k]` has type `&T` in the closure body, so write `*k` to read the value (captured by reference here: {})", self.cap_refs.join(", ")));
        }
        let d = self.def();
        let (line, col) = crate::parser::unpack_pos(if line != 0 { line } else { d.line });
        self.diags.push(Diag { file: d.file.clone(), code: code.into(), fqn: d.name.clone(), line, col, msg });
    }

    /// 借用式 `&e` / `&mut e` の領域の変数を作り、側の表 (`regions.borrows`) に残す。
    fn borrow_region(&mut self, e: &V) -> Region {
        let id = crate::parser::nid(e);
        let r = Region::Var(id);
        self.side.regions.borrows.insert((self.cur, id), r.clone());
        r
    }

    /// 呼び出し先の署名 (省略規則を当てた後) の領域を、この呼び出し (`call_id`) のものに実体化する (`Named` / `Elided` → `Inst`)。
    fn inst_regions(call_id: u32, t: &Ty) -> Ty {
        t.map_regions(&mut |r| match r {
            Region::Named(_) | Region::Elided(_) => Region::Inst(call_id, Box::new(r.clone())),
            other => other.clone(),
        })
    }

    /// 実引数の型 `got` の領域が、仮引数の型 `want` (実体化済み) の領域より長く生きる、という関係を集める (解かない)。
    fn note_region_cstr(&mut self, call_id: u32, want: &Ty, got: &Ty, why: &'static str) {
        let (w, g) = (want.regions(), self.deep(got).regions());
        // 形が同じ (領域の数が等しい) ときだけ前順に対応づける。違えば一番外側の参照だけ。
        let pairs: Vec<(Region, Region)> = if w.len() == g.len() {
            g.into_iter().zip(w).collect()
        } else if let (Some(a), Some(b)) = (self.deep(got).outer_region(), want.outer_region()) {
            vec![(a.clone(), b.clone())]
        } else {
            vec![]
        };
        for (long, short) in pairs {
            if matches!(short, Region::Unk) || matches!(long, Region::Unk) || long.same(&short) {
                continue;
            }
            self.side.regions.cstrs.push(crate::tast::RegionCstr { key: (self.cur, call_id), long, short, why });
        }
    }

    /// 定義の署名の領域を、省略規則を当てた型で返す: (引数の型, 戻り値の型)。
    fn sig_elided(d: &Def, gn: &HashSet<String>) -> (Vec<Ty>, Ty) {
        let ps: Vec<(String, Ty)> = d.params.iter().map(|(n, t)| (n.clone(), from_v(t, gn))).collect();
        elide_sig(&ps, &from_v(&d.ret, gn))
    }

    pub fn check_def(&mut self, di: usize) {
        let d = &self.p.defs[di];
        // `const` / `static` は、初期化式の型を宣言の型と照らす (本体の代わりに初期化式を検査する)。
        let Some(body) = d.body.as_ref().or(if matches!(d.kw.as_str(), "const" | "static") { d.init.as_ref() } else { None }) else { return };
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
        self.closure_caps.clear();
        self.closure_static.clear();
        self.spawn_closures.clear();
        self.static_checks.clear();
        self.oneway_sites.clear();
        self.arg_static.clear();
        self.scopes = vec![HashMap::new()];
        let gn = d.gnames();
        let (sig_ps, sig_ret) = Self::sig_elided(d, &gn);
        {
            let bounds: Vec<(String, Region)> = d.generics.iter().filter(|g| g.0 == "type").flat_map(|g| items(&g.2).iter().filter_map(|b| if let V::S(t) = b { t.starts_with('\'').then(|| (g.1.clone(), Region::from_life(b))) } else { None }).collect::<Vec<_>>()).collect();
            self.side.regions.sigs.insert(di, crate::tast::SigRegions { params: d.params.iter().zip(&sig_ps).map(|((n, _), t)| (n.clone(), t.regions())).collect(), ret: sig_ret.regions(), bounds });
        }
        for ((n, _), ty) in d.params.iter().zip(sig_ps) {
            // 関数自身の型パラメータ (Ty::Param) は、単一化では「分からない型」と同じに働くが、
            // メソッド呼び出しでは境界のトレイトを引くために名前を残す。
            if matches!(ty, Ty::Prim(_)) {
                self.scopes[0].insert(exact_mark(n), Ty::Unit);
            }
            self.scopes[0].insert(n.clone(), ty);
        }
        // `&'static T` と宣言した引数は `'static` の値 (呼び出し側が `'static` を渡す義務を負う)。
        for ((n, _), st_) in d.params.iter().zip(static_ref_params(&d.header)) {
            if st_ {
                self.set_static(n, true);
            }
        }
        // `?` のエラー型の検査のため、この関数の戻り値型を覚える (クロージャはその注釈を重ねる)。
        let ret = sig_ret;
        self.rets.push(ret.clone());
        let value = self.infer(body);
        self.rets.pop();
        {
            let tail = if nname(body) == "Block" {
                match items(&nf(body)[0]).last() {
                    Some(l) if nname(l) == "ExprStmt" => nf(l)[0].clone(),
                    _ => body.clone(),
                }
            } else {
                body.clone()
            };
            self.note_conv(&tail, &ret, &value);
            self.note_region_cstr(crate::parser::nid(body), &ret, &value, "ret");
        }
        if let Err(err) = self.unify2(&ret, &value) {
            {
                let (want, got) = self.ann_conflict(&ret, &value, &err).unwrap_or_else(|| Self::raw_pair(&err));
                let line = if nname(body) == "Block" { items(&nf(body)[0]).last().map(nline).unwrap_or(0) } else { nline(body) };
                let what = if self.def().body.is_none() { "initializer" } else { "return value" };
                self.diag("E2020", format!("type mismatch in {}: expected {}, found {}{}", what, want, got, conv_hint(&want, &got)), line);
            }
        }
        // 戻り値は「受け側」の関数型が分かる: 宣言 × 宣言の効果の食い違いをここで報告する。
        self.check_fn_conv_dir(&ret, &value);
        if let Some((want, got)) = self.ref_level_conflict(&ret, &value) {
            let line = if nname(body) == "Block" { items(&nf(body)[0]).last().map(nline).unwrap_or(0) } else { nline(body) };
            self.diag("E2020", format!("type mismatch in return value: expected {}, found {}{}", want, got, self.deref_hint(&ret)), line);
        }
        self.check_dangling(body, &ret);
        self.finish_literals();
        self.check_unsize();
        self.report_undetermined();
        self.check_spawn_captures();
        self.check_fn_value_markers();
        self.check_static_bounds();
        self.check_oneway_string_keys();
        // 型変数は関数ごとの表なので、ここで解決してから外へ出す。
        for k in self.recv_start..self.recv_log.len() {
            let t = self.recv_log[k].recv.clone();
            self.recv_log[k].recv = self.deep(&t);
        }
        // 式 ID の鍵: 同じ ID が複数回推論されて型が違うなら、決められない (Unknown = 黙る)。
        // 行と名前の鍵 (導出の表): 同じ行の同じ名前の束縛 (1 行に並べた腕など) が違う型なら、同じく Unknown。
        let mut seen: HashMap<(usize, String), Ty> = HashMap::new();
        let mut by_id: HashMap<u32, Ty> = HashMap::new();
        for (line, name, id, t) in std::mem::take(&mut self.let_pending) {
            let t = self.deep(&t);
            if id != 0 {
                match by_id.get(&id) {
                    Some(old) if *old != t => {
                        self.let_tys.insert((di, id), Ty::Unknown);
                    }
                    _ => {
                        by_id.insert(id, t.clone());
                        self.let_tys.insert((di, id), t.clone());
                    }
                }
            }
            match seen.get(&(line, name.clone())) {
                Some(old) if *old != t => {
                    self.let_tys_line.insert((di, line, name), Ty::Unknown);
                }
                _ => {
                    seen.insert((line, name.clone()), t.clone());
                    self.let_tys_line.insert((di, line, name), t);
                }
            }
        }
        // 担当 D: パターンの型の型変数を、この関数の表のうちに解決する (型変数は関数ごと)。
        let keys: Vec<_> = self.side.pat_tys.keys().filter(|k| k.0 == di).copied().collect();
        for k in keys {
            let mut info = self.side.pat_tys[&k].clone();
            info.ty = self.deep(&info.ty);
            if let Some(b) = info.bind.as_mut() {
                b.1 = self.deep(&b.1);
            }
            self.side.pat_tys.insert(k, info);
        }
        // TAST (設計 8.4 担当 E): 閉包の取り込みを、型変数を解決して節名の鍵で側の表へ移す。
        for (node, caps) in std::mem::take(&mut self.closure_caps) {
            let caps_t: Vec<crate::tast::Capture> = caps.iter().map(|(k, n, t)| crate::tast::Capture { kind: k.clone(), name: n.clone(), ty: self.deep(t), region: if k == "move" { Region::Unk } else { Region::Var(self.closure_asts.get(&node).map_or(0, crate::parser::nid)) } }).collect();
            self.closure_caps.insert(node.clone(), caps);
            self.side.closure_caps.insert(node, caps_t);
        }
        self.flush_expr_types();
        self.flush_conv();
        self.flush_type_args(di);
        self.cov_flush();
    }

    /// 担当 B: 呼び出しごとに覚えた型引数を、型変数を解決して `side.type_args` に移す。
    /// 同じ呼び出しノードが再推論されて結果が違うときは、全部 `Unknown` (決められない = 黙る側)。
    fn flush_type_args(&mut self, di: usize) {
        for (id, ts) in std::mem::take(&mut self.targs_pending) {
            let ts: Vec<Ty> = ts.iter().map(|t| self.deep(t)).collect();
            match self.side.type_args.get_mut(&(di, id)) {
                Some(old) if *old != ts => *old = vec![Ty::Unknown; old.len().max(ts.len())],
                Some(_) => {}
                None => {
                    self.side.type_args.insert((di, id), ts);
                }
            }
        }
    }

    /// 担当 B: 呼び出し先 `d` の型パラメータ (型・定数) の実体を、宣言の `generics` の順に覚える。
    /// メソッドでは `impl` の型引数が先、メソッド自身の型引数が後 (`Def::generics` の並びのまま)。
    /// 引数にも戻り値にも現れず決まらないものは `Unknown`。
    fn note_type_args(&mut self, id: u32, d: &Def, map: &HashMap<String, Ty>) {
        if id == 0 {
            return;
        }
        let ts: Vec<Ty> = d
            .generics
            .iter()
            .filter(|g| g.0 == "type" || g.0 == "const")
            .map(|g| map.get(&g.1).cloned().unwrap_or(Ty::Unknown))
            .collect();
        if !ts.is_empty() {
            self.targs_pending.push((id, ts));
        }
    }

    /// `KSN_DUMP_SIDE=1` の出力 (1 行目が表の名前 `# type_args`)。(定義 FQN, 式 ID) の順。
    pub fn dump_type_args(&self) -> String {
        let mut rows: Vec<(String, u32, String)> = self
            .side
            .type_args
            .iter()
            .map(|((di, id), ts)| (self.p.defs[*di].name.clone(), *id, ts.iter().map(|t| t.show()).collect::<Vec<_>>().join(", ")))
            .collect();
        rows.sort();
        let mut out = String::from("# type_args\n");
        for (f, id, s) in rows {
            out.push_str(&format!("{}\t#{}\t[{}]\n", f, id, s));
        }
        out
    }

    /// 型の中に、決まっていない部分 (`Unknown` / 未解決の型変数) があれば true。
    /// 接尾辞のない数値リテラルの型変数は E2027 が見るので、ここでは数えない (escape されたものだけ数える)。
    fn undetermined(&self, t: &Ty) -> bool {
        match self.deep(t) {
            Ty::Unknown => true,
            Ty::Var(v) => self.kinds[v] == 0 || self.escaped.contains(&v),
            Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().any(|x| self.undetermined(x)),
            Ty::Ref(_, x, _) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => self.undetermined(&x),
            Ty::Closure { args, ret, .. } => args.iter().any(|x| self.undetermined(x)) || self.undetermined(&ret),
            _ => false,
        }
    }

    /// 型がまだ閉じていないか (未決の型変数・整数リテラルの型変数・`Unknown` を含む)。D17 の冗長な注釈の判定に使う。
    /// `undetermined` と違い、整数リテラルの型変数 (`kind != 0`) も開いているとみなす (`1` は接尾辞が無いと決まらない)。
    fn has_open(&self, t: &Ty) -> bool {
        match self.deep(t) {
            Ty::Unknown | Ty::Var(_) => true,
            Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().any(|x| self.has_open(x)),
            Ty::Ref(_, x, _) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => self.has_open(&x),
            Ty::Closure { args, ret, .. } => args.iter().any(|x| self.has_open(x)) || self.has_open(&ret),
            _ => false,
        }
    }

    /// 注釈の型が、型の木の外の情報 (借用の範囲・再借用・unsize・関数項目やクロージャの型への変換・生ポインタ) を
    /// 運び得るか。そうした注釈は、外すと同じ型でも値の扱いが変わりうるので冗長と判定しない (D17)。
    /// 参照 (`&T` / `&mut T`) は借用検査 (E3010 など) の範囲を注釈で決めるので、型が同じでも対象にしない。
    fn coerces(&self, t: &Ty) -> bool {
        match self.deep(t) {
            Ty::Ref(..) | Ty::Ptr(_, _) | Ty::Dyn(..) | Ty::Closure { .. } => true,
            Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().any(|x| self.coerces(x)),
            Ty::Array(x, _) | Ty::Slice(x) => self.coerces(&x),
            _ => false,
        }
    }

    /// 5.5: `std.thread.spawn` に渡したクロージャの取り込みが `core.marker.Send` を満たすかを見る。
    /// 取り込みは `&a` なら `&T`、`&mut a` なら `&mut T`、`move a` なら `T` として見る (仕様 5.5)。
    /// 満たさないものと、決まらないもの (`None`) は E2040 (fail-closed)。
    fn check_fn_value_markers(&mut self) {
        for (tr, _, t, line) in std::mem::take(&mut self.fn_value_markers) {
            let t = self.deep(&t);
            if self.auto_marker(&t, tr == "core.marker.Sync", &mut Vec::new()) != Some(true) {
                self.diag("E2040", format!("the trait bound `{}` of a function used as a value is not satisfied by {} (5.5)", tr, t.show()), line);
            }
        }
    }

    fn check_spawn_captures(&mut self) {
        let spawns = std::mem::take(&mut self.spawn_closures);
        for (node, line) in spawns {
            let Some(caps) = self.closure_caps.get(&node).cloned() else { continue };
            for (kind, name, t) in caps {
                let t = self.deep(&t);
                if self.auto_marker(&capture_ty(&kind, t.clone()), false, &mut Vec::new()) != Some(true) {
                    self.diag(
                        "E2040",
                        format!("spawn closure captures `{}`, which is not known to be core.marker.Send (5.5)", name),
                        line,
                    );
                }
            }
        }
    }

    /// `String` キーの `HashMap` を `&K` (`&String`) で引く `get` / `get_mut` / `contains_key` / `remove` は E2054 (書き方は 1 通り:
    /// `&str` で引く `get_str` / `get_mut_str` / `contains_key_str` / `remove_str` だけ)。キーの型が `alloc.string.String<..>` と
    /// 定義の終わりまでに決まったものだけを見る (決まらないものは E2044 の領分)。
    fn check_oneway_string_keys(&mut self) {
        for (recv, name, line) in std::mem::take(&mut self.oneway_sites) {
            let deep = self.deep(&recv);
            let Ty::Named(n, a, _) = deep.strip().clone() else { continue };
            // `HashSet` は `contains` / `remove` だけ (`insert` は値を入れるだけ)。
            let set = n == "alloc.collections.HashSet";
            if !(n == "alloc.collections.HashMap" || (set && matches!(name.as_str(), "contains" | "remove"))) {
                continue;
            }
            let Some(k) = a.first().map(|x| self.deep(x)) else { continue };
            if matches!(k.strip(), Ty::Named(kn, _, _) if kn == "alloc.string.String") {
                self.diag(
                    "E2054",
                    format!("`{0}` on a `{1}` with `String` keys: look it up by `&str` with `{0}_str` (the only form; `ksn fix` rewrites it)", name, if set { "HashSet" } else { "HashMap" }),
                    line,
                );
            }
        }
    }

    /// `'static` の境界を持つ位置 (`&'static T` の引数・`F: 'static` の型パラメータ) へ渡した値が、局所の借用を
    /// 含みうるなら E2037 (5.5)。出どころが `'static` と分かる値 (リテラル・`const`・`'static` の戻り値など) は通す。
    fn check_static_bounds(&mut self) {
        for (t, origin, line, what) in std::mem::take(&mut self.static_checks) {
            if !origin && self.holds_ref(&t, &mut Vec::new()) {
                self.diag("E2037", format!("{} must be 'static, but the value may hold a borrow of a local (5.5)", what), line);
            }
        }
    }

    /// 型が参照 (`&T` / `&mut T`、または参照を持つ名義型のフィールド) を含むか。含むなら `'static` を型からは示せない。
    /// 型パラメータは `'static` の境界が無ければ参照を持ちうる。決まらない型も持ちうる (fail-closed)。
    fn holds_ref(&self, t: &Ty, seen: &mut Vec<String>) -> bool {
        match self.deep(t) {
            Ty::Ref(..) => true,
            Ty::Tuple(xs) => xs.iter().any(|x| self.holds_ref(x, seen)),
            Ty::Array(x, _) | Ty::Slice(x) => self.holds_ref(&x, seen),
            Ty::Named(n, args, _) => {
                if args.iter().any(|x| self.holds_ref(x, seen)) {
                    return true;
                }
                if seen.iter().any(|s| *s == n) {
                    return false;
                }
                let Some(&idx) = self.p.types.get(&n) else { return false };
                let d = &self.p.defs[idx];
                if d.kw != "type" && d.kw != "enum" {
                    return false;
                }
                let gn = d.gnames();
                let tys: Vec<Ty> = if d.kw == "type" {
                    d.fields.iter().map(|f| from_v(&f.1, &gn)).collect()
                } else {
                    d.variants.iter().flat_map(|v| v.1.iter()).map(|x| from_v(x, &gn)).collect()
                };
                seen.push(n.clone());
                let r = tys.iter().any(|t| self.holds_ref(t, seen));
                seen.pop();
                r
            }
            Ty::Closure { id: Some(node), .. } => {
                let caps = self.closure_caps.get(&node).cloned().unwrap_or_default();
                let stat = self.closure_static.get(&node).cloned().unwrap_or_default();
                caps.iter().any(|(k, n, t)| k == "&" || k == "&mut" || (!stat.contains(n) && self.holds_ref(t, seen)))
            }
            // 定義の欄の中の型パラメータは、実引数 (`args`) の側で見ているので数えない。
            Ty::Param(_) if !seen.is_empty() => false,
            Ty::Param(p) => !self.def().generics.iter().any(|g| {
                g.0 == "type" && g.1 == p && items(&g.2).iter().any(|b| matches!(b, V::S(x) if x == "'static"))
            }),
            Ty::Unknown => true,
            Ty::Var(v) => self.kinds[v] == 0,
            _ => false,
        }
    }

    /// 型が `core.marker.Send` (`sync` = false) / `Sync` (true) を満たすか (5.5)。`Some(false)` は満たさない、
    /// `None` は決まらない。`&T` は `T: Sync` のとき Send、`&mut T` は `T: Send` のとき Send。生ポインタは両方とも満たさない。
    /// 手動の impl は境界 (`T: core.marker.Send` など) を実引数に当てて見る。無いものは型の定義から自動導出する
    /// (循環は成立とみなす)。型パラメータは、その境界に当該マーカーがあるときだけ満たす。
    /// 規則 (`tast_rules/bounds.rs`) から: 呼び出し元 `caller` (定義の添字) の境界の下で `t` が Send / Sync か。
    pub fn marker_in(&self, caller: usize, t: &Ty, sync: bool) -> Option<bool> {
        self.marker_def.set(Some(caller));
        let r = self.auto_marker(t, sync, &mut Vec::new());
        self.marker_def.set(None);
        r
    }

    fn auto_marker(&self, t: &Ty, sync: bool, seen: &mut Vec<String>) -> Option<bool> {
        let marker = if sync { "core.marker.Sync" } else { "core.marker.Send" };
        match self.deep(t) {
            Ty::Never | Ty::Unit | Ty::Prim(_) | Ty::Const(_) => Some(true),
            Ty::Ptr(..) => Some(false),
            Ty::Ref(m, x, _) => self.auto_marker(&x, sync || !m, seen),
            Ty::Tuple(xs) => xs.iter().fold(Some(true), |r, x| and_marker(r, self.auto_marker(x, sync, seen))),
            Ty::Array(x, _) | Ty::Slice(x) => self.auto_marker(&x, sync, seen),
            Ty::Param(p) => {
                let md = self.marker_def.get().map_or_else(|| self.def(), |i| &self.p.defs[i]);
                let bounded = md.generics.iter().any(|g| {
                    g.0 == "type" && g.1 == p && items(&g.2).iter().any(|b| matches!(b, V::L(bx) if st(&bx[0]) == marker))
                });
                if bounded { Some(true) } else { None }
            }
            Ty::Dyn(tr, _, _) => self.dyn_auto(&tr, marker),
            // クロージャは、取り込んだ実際の型がすべて満たすとき (5.5)。節が分からない (関数ポインタ) なら取り込みはない。
            Ty::Closure { id: Some(node), .. } => {
                let caps = self.closure_caps.get(&node).cloned().or_else(|| self.side.closure_caps.get(&node).map(|c| c.iter().map(|c| (c.kind.clone(), c.name.clone(), c.ty.clone())).collect())).unwrap_or_default();
                caps.into_iter().fold(Some(true), |r, (k, _, t)| and_marker(r, self.auto_marker(&capture_ty(&k, t), sync, seen)))
            }
            Ty::Closure { id: None, .. } => Some(true),
            Ty::Named(n, args, _) => self.auto_named(&n, &args, sync, seen),
            Ty::Unknown | Ty::Var(_) => None,
        }
    }

    /// 名義型 `n<args>` が `core.marker.Send` / `Sync` を満たすか。`auto_marker` の名義型の枝。
    fn auto_named(&self, n: &str, args: &[Ty], sync: bool, seen: &mut Vec<String>) -> Option<bool> {
        let marker = if sync { "core.marker.Sync" } else { "core.marker.Send" };
        // `alloc.boxed.Box<T, A>` は `T` と `A` に従う (`T` が `dyn Tr` のときは `Tr` の `requires`、`Dyn` の枝)。
        if n == "alloc.boxed.Box" {
            return args.iter().fold(Some(true), |r, x| and_marker(r, self.auto_marker(x, sync, seen)));
        }
        // 手動の impl (`impl[T: Send, A: Send] Send for Vec<T, A>` など): 境界に書かれたマーカーだけを実引数に当てる。
        if let Some(imp) = self.p.defs.iter().find(|d| d.kw == "impl" && d.impl_trait == marker && crate::check::impl_target(&d.impl_for) == n) {
            let head: Vec<&V> = items(tel(&imp.impl_for, 2)).iter().filter(|a| matches!(a, V::L(_)) && tag(a) != "life").collect();
            if args.len() > head.len() {
                return None;
            }
            let mut r = Some(true);
            for (i, a) in head.iter().enumerate() {
                // 実引数の位置の型パラメータ名 (具体型の位置は境界を持たないので見ない)。
                if tag(a) != "named" {
                    continue;
                }
                let pname = st(tel(a, 1));
                for g in imp.generics.iter().filter(|g| g.0 == "type" && g.1 == pname) {
                    let t = match args.get(i) {
                        Some(t) => t.clone(),
                        None => omitted_arg(g)?,
                    };
                    for b in items(&g.2) {
                        let V::L(bx) = b else { continue };
                        let bn = st(&bx[0]);
                        if bn == "core.marker.Send" {
                            r = and_marker(r, self.auto_marker(&t, false, seen));
                        } else if bn == "core.marker.Sync" {
                            r = and_marker(r, self.auto_marker(&t, true, seen));
                        }
                    }
                }
            }
            return r;
        }
        // 型の定義から自動導出する。定義の無い名義型は決まらない (fail-closed)。
        let &idx = self.p.types.get(n)?;
        let d = &self.p.defs[idx];
        if d.kw != "type" && d.kw != "enum" {
            return None;
        }
        let gens: Vec<&(String, String, V)> = d.generics.iter().filter(|g| g.0 != "life").collect();
        if args.len() > gens.len() {
            return None;
        }
        if seen.iter().any(|s| s == n) {
            return Some(true);
        }
        let mut bind: HashMap<String, Ty> = HashMap::new();
        for (i, g) in gens.iter().enumerate() {
            let t = match args.get(i) {
                Some(t) => t.clone(),
                None => omitted_arg(g)?,
            };
            bind.insert(g.1.clone(), t);
        }
        let gn = d.gnames();
        let tys: Vec<Ty> = if d.kw == "type" {
            d.fields.iter().map(|f| from_v(&f.1, &gn)).collect()
        } else {
            d.variants.iter().flat_map(|v| v.1.iter()).map(|x| from_v(x, &gn)).collect()
        };
        seen.push(n.to_string());
        let mut r = Some(true);
        for t in &tys {
            r = and_marker(r, self.auto_marker(&subst(t, &bind), sync, seen));
        }
        seen.pop();
        r
    }

    /// `dyn Tr` が `core.marker.Send` / `Sync` を満たすか: `Tr` の `requires` (スーパートレイトを推移的に辿る) に含むときだけ (5.5)。
    fn dyn_auto(&self, tr: &str, marker: &str) -> Option<bool> {
        let root = crate::check::resolve_trait_fqn(self.p, tr)?;
        let mut seen: HashSet<String> = HashSet::new();
        let mut stack = vec![root];
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur.clone()) {
                continue;
            }
            let Some(&i) = self.p.types.get(&cur) else { continue };
            let td = &self.p.defs[i];
            if td.kw != "trait" {
                continue;
            }
            for (sup, _) in &td.requires {
                if sup == marker {
                    return Some(true);
                }
                if let Some(fq) = crate::check::resolve_trait_fqn(self.p, sup) {
                    stack.push(fq);
                }
            }
        }
        Some(false)
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
            Ty::Array(x, _) => V::L(vec![s("array"), Self::to_v(x)?]),
            Ty::Slice(x) => V::L(vec![s("slice"), Self::to_v(x)?]),
            Ty::Ref(m, x, _) => V::L(vec![s("ref"), V::B(*m), Self::to_v(x).unwrap_or(V::Nil)]),
            Ty::Ptr(m, x) => V::L(vec![s("ptr"), V::B(*m), Self::to_v(x)?]),
            Ty::Named(n, args, _) => V::L(vec![s("named"), s(n), V::L(args.iter().map(Self::to_v).collect::<Option<Vec<V>>>()?)]),
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
        for (key, ws) in std::mem::take(&mut self.side.arg_wants_pending) {
            let ws = ws.iter().map(|w| self.deep(w)).collect();
            self.side.arg_wants.insert(key, ws);
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
            let nosuf = kind == "int?";
            let kind = if nosuf { "int" } else { kind };
            match self.deep(&ty) {
                Ty::Prim(p) if kind == "int" && INTS.contains(&p.as_str()) => {
                    if let Some(msg) = int_literal_problem(&text, &p) {
                        self.diag("E2026", msg, line);
                    }
                    if nosuf {
                        self.diag("E2028", format!("the integer literal `{}` has no suffix: append `{}` (write `{}{}`)", text, p, text, p), line);
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
    /// `if` の両枝・`match` の腕の型を合わせる。食い違いは E2020 (黙ると `if c { 1u32 } else { "x" }` が通る)。
    fn join_arm(&mut self, a: &Ty, b: &Ty, line: usize, what: &str) -> Ty {
        let unknown = |t: &Ty| matches!(t, Ty::Unknown | Ty::Param(_));
        if unknown(&self.resolve(a)) || unknown(&self.resolve(b)) {
            return a.clone();
        }
        if let Err(err) = self.unify2(a, b) {
            {
                let (want, got) = self.ann_conflict(a, b, &err).unwrap_or_else(|| Self::raw_pair(&err));
                self.diag("E2020", format!("type mismatch in {}: expected {}, found {}{}", what, want, got, conv_hint(&want, &got)), line);
            }
        }
        // 枝の参照の可変性は順序によらず一致させる (`&x` と `&mut y` の混在は、先の枝が期待になって
        // 順序で通ったり通らなかったりしていた。ネイティブの下ろしも枝の型の違いを拒む)。
        if let (Ty::Ref(m, _, _), Ty::Ref(n, _, _)) = (self.resolve(a), self.resolve(b)) {
            if m != n {
                let (w, g) = (self.deep(a).show(), self.deep(b).show());
                self.diag("E2020", format!("type mismatch in {}: the branches have different reference mutability: {} and {}; write `&*` / `&mut *` explicitly so both are the same", what, w, g), line);
            }
        }
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
            Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().for_each(|x| self.escape(x)),
            Ty::Ref(_, x, _) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => self.escape(&x),
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
                        // `r.m(..)` (`r` は取り込んだ値そのもの) で `m` が `self` を値で受けるなら消費する (線形の値も同じ規則)。
                        if segs.len() == 2 && segs[0] == name && non_copy {
                            if let Some(t) = self.lookup(name) {
                                let tn = match self.deep(&t) {
                                    Ty::Named(n, _, _) => Some(n),
                                    Ty::Prim(p) => Some(p),
                                    _ => None,
                                };
                                if let Some(tn) = tn {
                                    let fq = format!("{}.{}", tn, segs[1]);
                                    let di = self.p.funcs.get(&fq).or_else(|| self.p.sigs.get(&fq)).copied();
                                    if let Some(di) = di {
                                        if matches!(self.p.defs[di].params.first(), Some((pn, pt)) if pn == "self" && tag(pt) != "ref") {
                                            u.consumed = true;
                                        }
                                    }
                                }
                            }
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
            Ty::Ref(m, _, _) => Some(!*m),
            Ty::Array(x, _) => self.ty_copy(x),
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
            Ty::Named(n, args, _) if self.p.types.contains_key(n) => {
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
        // 参照を分解して得た名前は、欄の型への参照になる (Rust の既定の束縛モード)。
        let exact_here = self.pat_exact && self.exactly_owned(&ty);
        let ty = match self.pat_mode {
            Some(m) => Ty::Ref(m, Box::new(ty), crate::types::Region::Unk),
            None => ty,
        };
        if self.strict {
            let id = self.new_site(self.pat_line, format!("the binding `{}`", name), ty.clone());
            if let Some(src) = self.pat_src {
                self.sites[id].deps.push(src);
            }
            self.local_sites.insert(name.to_string(), id);
        }
        if let Some(s) = self.scopes.last_mut() {
            s.remove(&static_mark(name));
            s.remove(&exact_mark(name));
            if exact_here {
                s.insert(exact_mark(name), Ty::Unit);
            }
            s.insert(name.to_string(), ty);
        }
    }

    /// 見えている `name` の束縛に、`'static` と分かっている印を付ける (on) / 外す。印は束縛と同じ scope に置く。
    /// 外すのは再代入・`&mut` の借用・`&mut` の取り込みのとき (後から局所の借用を入れられるので、印を残せない)。
    /// 型が、参照の段数まで確かな所有値の型か (基本型・型引数のない名前付きの型・それらのタプル)。
    /// 総称の引数や参照越しの分解を通った型は、参照の段数が落ちていることがあるので確かではない。
    fn exactly_owned(&self, t: &Ty) -> bool {
        match self.resolve(t) {
            Ty::Prim(_) | Ty::Unit => true,
            Ty::Named(_, a, _) => a.is_empty(),
            Ty::Tuple(ts) => ts.iter().all(|x| self.exactly_owned(x)),
            _ => false,
        }
    }

    fn set_static(&mut self, name: &str, on: bool) {
        if let Some(s) = self.scopes.iter_mut().rev().find(|s| s.contains_key(name)) {
            if on {
                s.insert(static_mark(name), Ty::Unit);
            } else {
                s.remove(&static_mark(name));
            }
        }
    }

    /// 今見えている `name` の束縛が `'static` の値だと分かっているか。印は束縛と同じ scope に置く。
    fn lookup_static(&self, name: &str) -> bool {
        self.scopes.iter().rev().find(|s| s.contains_key(name)).map_or(false, |s| s.contains_key(&static_mark(name)))
    }

    /// 式の値が `'static` の参照だと型以外の根拠で分かるか (5.5: spawn の取り込みを通す根拠)。
    /// 文字列リテラル (`str` / `bstr`)、`const` / `static` の名前、`&'static` の戻り値を返す関数の呼び出し、
    /// `'static` と分かる局所変数の別名。どれにも当たらなければ false (fail-closed)。
    fn static_expr(&self, e: &V) -> bool {
        match nname(e) {
            "Lit" => matches!(st(&nf(e)[0]).as_str(), "str" | "bstr"),
            "Unary" => st(&nf(e)[0]) == "&" && matches!(nname(&nf(e)[1]), "Lit" | "Path") && self.static_expr(&nf(e)[1]),
            "Path" => {
                let segs = strs(&nf(e)[0]);
                if self.lookup(&segs[0]).is_some() {
                    return segs.len() == 1 && self.lookup_static(&segs[0]);
                }
                matches!(self.p.resolve(&segs, &self.def().file).map(|r| r.target), Some(Target::Const(_)))
            }
            // `e?`: 戻り値の参照がすべて `'static` と宣言された関数の結果は、`?` で取り出しても `'static`。
            "Try" => self.static_expr(&nf(e)[0]),
            "Match" | "If" | "Block" => self.static_nodes.contains(&crate::parser::nid(e)),
            // `'static` の値を持つ `Option` / `Result` から値を取り出す (`unwrap`・`expect`)、既定値も `'static` の `unwrap_or`。
            "MCall" => match st(&nf(e)[1]).as_str() {
                "unwrap" | "expect" => self.static_expr(&nf(e)[0]),
                "unwrap_or" => self.static_expr(&nf(e)[0]) && items(&nf(e)[2]).first().map_or(false, |a| self.static_expr(a)),
                _ => false,
            },
            "Call" => {
                let callee = &nf(e)[0];
                if nname(callee) != "Path" {
                    return false;
                }
                let segs = strs(&nf(callee)[0]);
                if segs.is_empty() || self.lookup(&segs[0]).is_some() {
                    return false;
                }
                match self.p.resolve(&segs, &self.def().file).map(|r| r.target) {
                    Some(Target::Fn(i)) | Some(Target::Sig(i)) => ret_is_static(&self.p.defs[i].header),
                    // 標準ライブラリの組み込み (`std.env.arg` など) は、`spec/stdlib` の署名で戻り値の領域を見る。
                    Some(Target::Native(ref fq)) | Some(Target::Value(ref fq)) => self.p.sigs.get(fq).map_or(false, |&i| ret_is_static(&self.p.defs[i].header)),
                    _ => false,
                }
            }
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

    /// `t` が、共有参照を一つでも通ってから `FnMut` のクロージャに届くか (`&c` / `&&c`)。
    fn shared_then_fnmut(&self, t: &Ty) -> bool {
        let mut shared = false;
        let mut cur = self.resolve(t);
        loop {
            match cur {
                Ty::Ref(m, inner, _) => {
                    shared |= !m;
                    cur = self.resolve(&inner);
                }
                Ty::Closure { cap, .. } => return shared && cap == Cap::FnMut,
                _ => return false,
            }
        }
    }

    /// 中まで解決する。
    pub fn deep(&self, t: &Ty) -> Ty {
        match self.resolve(t) {
            Ty::Named(n, a, rs) => Ty::Named(n, a.iter().map(|x| self.deep(x)).collect(), rs),
            Ty::Ref(m, x, rg) => Ty::Ref(m, Box::new(self.deep(&x)), rg),
            Ty::Ptr(m, x) => Ty::Ptr(m, Box::new(self.deep(&x))),
            Ty::Tuple(xs) => Ty::Tuple(xs.iter().map(|x| self.deep(x)).collect()),
            Ty::Array(x, n) => Ty::Array(Box::new(self.deep(&x)), n),
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
            Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().any(|x| self.occurs(v, x)),
            Ty::Ref(_, x, _) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => self.occurs(v, &x),
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
            Ty::Array(..) | Ty::Slice(_) => Some("seq"),
            Ty::Closure { .. } => Some("closure"),
            Ty::Named(n, _, _) if n != "alloc.vec.Vec" && n != "alloc.string.String" => Some("named"),
            _ => None,
        }
    }

    /// `unify2` の矛盾 (`Err`) を報告用の文字列にする。絞り込み (`filtered`) が形を知っていればその文面、
    /// 知らなければ矛盾した箇所の 2 つの型をそのまま出す。矛盾は必ず報告される (黙るのは「試すだけ」の `try_unify` だけ)。
    fn raw_pair(e: &(Ty, Ty)) -> (String, String) {
        (e.0.show(), e.1.show())
    }

    /// トレイトのメソッドの署名の型。メソッド自身の型パラメータ (`fn hash[H: Hasher](..)` の `H`) は
    /// 署名に宣言が残らず、`from_v` では点を含まない未解決の名前になる。それを型パラメータにして
    /// (`inst` が呼び出しごとの変数にする)、利用者の型と取り違えない。
    fn sig_ty(&self, v: &V, gn: &HashSet<String>) -> Ty {
        fn lift(s: &TypeCk, t: Ty) -> Ty {
            match t {
                Ty::Named(n, a, _) if a.is_empty() && !n.contains('.') && s.type_def(&n).is_none() && !matches!(n.as_str(), "Option" | "Result") => Ty::Param(n),
                Ty::Named(n, a, rs) => Ty::Named(n, a.into_iter().map(|x| lift(s, x)).collect(), rs),
                Ty::Ref(m, x, rg) => Ty::Ref(m, Box::new(lift(s, *x)), rg),
                Ty::Ptr(m, x) => Ty::Ptr(m, Box::new(lift(s, *x))),
                Ty::Tuple(a) => Ty::Tuple(a.into_iter().map(|x| lift(s, x)).collect()),
                Ty::Array(x, n) => Ty::Array(Box::new(lift(s, *x)), n),
                Ty::Slice(x) => Ty::Slice(Box::new(lift(s, *x))),
                other => other,
            }
        }
        lift(self, from_v(v, gn))
    }

    /// 構造そのものが合わない組 (`unify2` が必ず `Err` にするもの) を、報告用の文字列にする。
    /// 型パラメータどうしの取り違え、タプルの要素数、種類の違い (整数と配列、構造体とクロージャなど)、配列の長さ。
    fn shape_conflict(e: &(Ty, Ty)) -> Option<(String, String)> {
        let hit = match (&e.0, &e.1) {
            (Ty::Param(a), Ty::Param(b)) => a != b,
            (Ty::Param(_), t) | (t, Ty::Param(_)) => !matches!(t, Ty::Var(_) | Ty::Ref(..) | Ty::Unknown | Ty::Never),
            (Ty::Tuple(a), Ty::Tuple(b)) => a.len() != b.len(),
            (Ty::Array(_, Some(a)), Ty::Array(_, Some(b))) => a != b,
            (x, y) => match (Self::concrete_kind(x), Self::concrete_kind(y)) {
                (Some(p), Some(q)) => p != q,
                _ => false,
            },
        };
        hit.then(|| (e.0.show(), e.1.show()))
    }

    /// 関数・クロージャを関数型 `fn(..) uses [E] panics P` に合わせる所 (仕様 5.1) の効果の検査。
    /// 方向が分かる場所 (let の注釈・引数・戻り値・構造体の欄) で、渡し側の関数型の宣言の効果が
    /// (効果の比べは TAST の効果の規則が行う)。単一化は対称なので向きを
    /// `want`/`got` で外から渡す。入れ子 (Option<fn(..)> など) も辿る。
    fn check_fn_conv_dir(&mut self, want: &Ty, got: &Ty) {
        self.check_fn_conv_dir_walk(self.resolve(want), self.resolve(got));
    }

    /// `want`/`got` を辿り、関数型同士が対になる所を辿るための形 (効果の比べは持たない)。
    /// 構造が揃わない組は黙る (型不一致は単一化側が見る)。
    fn check_fn_conv_dir_walk(&mut self, mut want: Ty, mut got: Ty) {
        want = self.resolve(&want);
        got = self.resolve(&got);
        // 参照は透過的。両側から潜る (単一化と同じ)。
        // `&mut T` は T について不変 (部分型は `&mut` の下で使えない): 両側が `&mut` なら向きを両方で見る。
        let mut inv = false;
        loop {
            match (&want, &got) {
                (Ty::Ref(true, w, _), Ty::Ref(true, g, _)) => {
                    inv = true;
                    (want, got) = (self.resolve(w), self.resolve(g));
                }
                (Ty::Ref(_, w, _), _) => want = self.resolve(w),
                (_, Ty::Ref(_, g, _)) => got = self.resolve(g),
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
            (Ty::Closure { args: wa, ret: wr, .. }, Ty::Closure { args: ga, ret: gr, .. }) => {
                // 戻り値は共変: 同じ向きで再帰する。
                self.check_fn_conv_dir_walk(wr.as_ref().clone(), gr.as_ref().clone());
                // 引数は反変: 向きを逆にして再帰する。
                for (w, g) in wa.iter().zip(ga.iter()) {
                    self.check_fn_conv_dir_walk(g.clone(), w.clone());
                }
            }
            (Ty::Named(_, ws, _), Ty::Named(_, gs, _)) => {
                for (w, g) in ws.iter().zip(gs.iter()) {
                    self.check_fn_conv_dir_walk(w.clone(), g.clone());
                }
            }
            (Ty::Tuple(ws), Ty::Tuple(gs)) => {
                for (w, g) in ws.iter().zip(gs.iter()) {
                    self.check_fn_conv_dir_walk(w.clone(), g.clone());
                }
            }
            (Ty::Array(w, _), Ty::Array(g, _)) | (Ty::Slice(w), Ty::Slice(g)) => {
                self.check_fn_conv_dir_walk((**w).clone(), (**g).clone());
            }
            (Ty::Ptr(_, w), Ty::Ptr(_, g)) => {
                self.check_fn_conv_dir_walk((**w).clone(), (**g).clone());
            }
            // `&T` → `&dyn Tr<..>` / `Box<T>` → `Box<dyn Tr<..>>`: `T` に `Tr` の実装が無いと E2046。
            (Ty::Dyn(wt, _, _), Ty::Named(n, _, _)) => {
                let (wt, n) = (wt.clone(), n.clone());
                self.check_dyn_impl(&wt, &n);
            }
            // 型パラメータ `H` を `dyn Tr` へ渡せるのは、宣言された境界 (とそのスーパートレイト) に `Tr` があるときだけ。
            (Ty::Dyn(wt, _, _), Ty::Param(pn)) => {
                if let Some(root) = crate::check::resolve_trait_fqn(self.p, wt) {
                    if param_bound_implies(self.p, self.def(), pn, &root) == Some(false) {
                        self.diag("E2046", format!("the type parameter `{}` does not implement the trait `{}`: add the bound `{}: {}`", pn, root, pn, root), self.cur_line);
                    }
                }
            }
            _ => {}
        }
    }

    /// `dyn Tr` の受け側へ型 `ty` の値を渡す: `ty` に `Tr` の実装が無ければ E2046 (トレイトの型引数の比べは持たない)。
    fn check_dyn_impl(&mut self, tr: &str, ty: &str) {
        let Some(root) = crate::check::resolve_trait_fqn(self.p, tr) else { return };
        if self.impl_targs_of(&root, ty).is_none() && self.p.types.contains_key(ty) {
            // 実装の宣言が無い型は `dyn Tr` にできない (同名の固有メソッドでは代用しない)。
            self.diag("E2046", format!("`{}` does not implement the trait `{}`", ty, root), self.cur_line);
        }
    }

    /// 2 つの型を等しくする。矛盾したら、その箇所の 2 つの型を返す。参照は透過的。
    ///
    /// `a` は期待 (受け側)、`b` は実際 (渡し側)。変性は `unify_v` が持つ: 値の位置 (共変) でだけ
    /// `&mut T` → `&T`、`&[T; N]` → `&[T]`、`*mut T` → `*const T`、`*mut [T]` → `*mut T` を許し、
    /// `&mut` の下・生ポインタの指す先・利用者の型やコンテナの型引数では不変、関数の引数では反変。
    fn unify2(&mut self, a: &Ty, b: &Ty) -> Result<(), (Ty, Ty)> {
        self.unify_v(a, b, false)
    }

    /// 型の構造の中 (型引数・要素・参照の先) へ入って単一化する。中では参照の段数を透過しない。
    fn unify_in(&mut self, a: &Ty, b: &Ty, inv: bool) -> Result<(), (Ty, Ty)> {
        self.nest += 1;
        let r = self.unify_v(a, b, inv);
        self.nest -= 1;
        r
    }

        /// `inv` が true なら不変の位置 (参照の可変性も、配列とスライスも、一切の変換を許さない)。
    fn unify_v(&mut self, a: &Ty, b: &Ty, inv: bool) -> Result<(), (Ty, Ty)> {
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
            (Ty::Unknown, _) | (_, Ty::Unknown) | (Ty::Never, _) | (_, Ty::Never) => Ok(()),
            // 型パラメータは剛な型 (自分自身とだけ一致する)。変数・参照 (透過) は下の腕で扱う。
            (Ty::Param(p), Ty::Param(q)) => if p == q { Ok(()) } else { Err((a.clone(), b.clone())) },
            (Ty::Param(_), t) | (t, Ty::Param(_)) if !matches!(t, Ty::Var(_) | Ty::Ref(..)) => Err((a.clone(), b.clone())),
            // 型パラメータの値が要る所に参照 (`fn own[T](x: &T) -> T { x }`): 参照は所有された `T` ではない。
            (Ty::Param(_), Ty::Ref(..)) => Err((a.clone(), b.clone())),
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
                if let (Ty::Ref(_, inner, _), true) = (&t, self.kinds[*x] != 0) {
                    // 参照は透過的: リテラルの型は参照の中身に合わせる。
                    return self.unify_v(&Ty::Var(*x), inner, inv);
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
                let (x, y) = (self.resolve(x), self.resolve(y));
                // 指す先は不変。値の位置では、`*mut [T]` (スライスへのポインタ) は先頭要素へのポインタ `*mut T` として渡せる (FFI の慣習)。
                // 実際がスライス・配列で、期待がそれ以外の確定した型のときだけ (逆向きは許さない)。
                let (x, y) = match (&x, &y) {
                    (t, Ty::Slice(e) | Ty::Array(e, _)) if !inv && !matches!(t, Ty::Slice(_) | Ty::Array(..) | Ty::Var(_)) => (x.clone(), (**e).clone()),
                    _ => (x, y),
                };
                self.unify_in(&x, &y, true).map_err(|_| (a.clone(), b.clone()))?;
                // 期待が `*const`・実際が `*mut` は値の位置でだけ許す (`*mut T` → `*const T`)。逆は不一致。
                if m == n || (!inv && !m && n) { Ok(()) } else { Err((a.clone(), b.clone())) }
            }
            (Ty::Ptr(..), Ty::Prim(_) | Ty::Named(..)) | (Ty::Prim(_) | Ty::Named(..), Ty::Ptr(..)) => Err((a.clone(), b.clone())),
            // 両側が参照なら、指す先どうしを合わせる (`from_ref(&x)` の `&T` に `&i32` を渡して `T = &i32` にしない)。
            // 可変性は向きがある: 左 (期待) が `&mut` で右 (実際) が `&` は不一致 (`&mut` → `&` は許す)。
            (Ty::Ref(m, x, _), Ty::Ref(n, y, _)) => {
                let (m, n) = (*m, *n);
                let (x, y) = (self.resolve(x), self.resolve(y));
                // 可変性は向きがある: 期待が `&mut`・実際が `&` は不一致、期待が `&`・実際が `&mut` は値の位置でだけ許す。
                // 不変の位置 (`&mut` の下など) では一致させる。
                if (m && !n) || (inv && m != n) {
                    return Err((a.clone(), b.clone()));
                }
                // `&mut` の下は不変 (`&mut Option<&mut T>` を `&mut Option<&T>` にできない)。
                let inner_inv = inv || m;
                // `&str` と `&String` は別の型 (4.2: 暗黙の変換の表に無い)。
                if matches!((&x, &y), (Ty::Named(n, _, _), Ty::Prim(p)) | (Ty::Prim(p), Ty::Named(n, _, _)) if n == "alloc.string.String" && p == "str") {
                    return Err((a.clone(), b.clone()));
                }
                // `&[T; N]` → `&[T]` は参照の直下の値の位置でだけ許す (逆向き・値渡しは許さない)。
                if let (Ty::Slice(e), Ty::Array(f, _)) = (&x, &y) {
                    if inv {
                        return Err((a.clone(), b.clone()));
                    }
                    let (e, f) = ((**e).clone(), (**f).clone());
                    return self.unify_in(&e, &f, inner_inv);
                }
                self.unify_in(&x, &y, inner_inv)
            }
            // 参照は透過的。左右の順序 (期待, 実際) は診断の文面に使うので保つ。
            // 型の構造の中 (型引数・タプルの要素・参照の先など) では、参照の段数も型の一部。
            (Ty::Ref(..), _) | (_, Ty::Ref(..)) if self.nest > 0 && !matches!(a, Ty::Var(_) | Ty::Param(_)) && !matches!(b, Ty::Var(_) | Ty::Param(_)) => Err((a.clone(), b.clone())),
            (Ty::Ref(_, x, _), y) => {
                let (x, y) = (x.clone(), y.clone());
                self.unify_v(&x, &y, inv)
            }
            (x, Ty::Ref(_, y, _)) => {
                let (x, y) = (x.clone(), y.clone());
                self.unify_v(&x, &y, inv)
            }
            (Ty::Prim(p), Ty::Prim(q)) => {
                if p == q { Ok(()) } else { Err((a.clone(), b.clone())) }
            }
            // 4.3: 定数実引数は同じ定数なら同じ型、異なる定数は別単相。
            (Ty::Const(x), Ty::Const(y)) => {
                if x == y { Ok(()) } else { Err((a.clone(), b.clone())) }
            }
            (Ty::Dyn(p, _, _), Ty::Dyn(q, _, _)) => {
                if p == q { Ok(()) } else { Err((a.clone(), b.clone())) }
            }
            (Ty::Named(n, xs, _), Ty::Named(m, ys, _)) => {
                if n != m {
                    return Err((a.clone(), b.clone()));
                }
                let (xs, ys) = (xs.clone(), ys.clone());
                // `Box<dyn Tr>` と `Box<T>` は別の型 (変換は `Box.into_dyn` で明示する。5.1)。
                if n == "alloc.boxed.Box" && self.box_dyn_mismatch(&xs, &ys) {
                    return Err((a.clone(), b.clone()));
                }
                // 型引数は不変 (`Option` / `Result` は値として運ぶだけなので共変)。
                let arg_inv = inv || !matches!(n.as_str(), "Option" | "Result");
                for (x, y) in xs.iter().zip(&ys) {
                    self.unify_in(x, y, arg_inv)?;
                }
                Ok(())
            }
            (Ty::Tuple(xs), Ty::Tuple(ys)) => {
                if xs.len() != ys.len() {
                    return Err((a.clone(), b.clone()));
                }
                let (xs, ys) = (xs.clone(), ys.clone());
                for (x, y) in xs.iter().zip(&ys) {
                    self.unify_in(x, y, inv)?;
                }
                Ok(())
            }
            (Ty::Closure { args: xs, ret: rx, .. }, Ty::Closure { args: ys, ret: ry, .. }) => {
                if xs.len() != ys.len() {
                    return Err((a.clone(), b.clone()));
                }
                // 2 つの実際のクロージャを単一化する (合流・代入): 実際の効果を和にして残す。
                let merged = self.merge_closure_eff(&a, &b);
                let (xs, ys, rx, ry) = (xs.clone(), ys.clone(), (**rx).clone(), (**ry).clone());
                // 引数は反変 (期待と実際を入れ替えて比べる)、戻り値は共変。
                for (x, y) in xs.iter().zip(&ys) {
                    self.unify_in(y, x, inv).map_err(|(p, q)| (q, p))?;
                }
                self.unify_in(&rx, &ry, inv)?;
                // どちらかが型変数に束縛されていれば、束縛先も効果の和に更新する。
                for opt in [va, vb] {
                    if let Some((x, Some(Ty::Closure { .. }))) = opt {
                        self.vars[x] = Some(merged.clone());
                    }
                }
                Ok(())
            }
            (Ty::Array(_, Some(m)), Ty::Array(_, Some(n))) if m != n => Err((a.clone(), b.clone())),
            // 配列とスライスは別の型 (`&[T; N]` → `&[T]` は参照の腕だけが許す)。
            (Ty::Slice(x), Ty::Slice(y)) | (Ty::Array(x, _), Ty::Array(y, _)) => {
                let (x, y) = (x.clone(), y.clone());
                self.unify_in(&x, &y, inv)
            }
            (Ty::Slice(_), Ty::Array(..)) | (Ty::Array(..), Ty::Slice(_)) => Err((a.clone(), b.clone())),
            (Ty::Unit, Ty::Unit) => Ok(()),
            // `()` と `String` / `Vec` は決して合わない (下の除外は `str` やスライスとの近似のためで、`()` には要らない)。
            (Ty::Unit, Ty::Named(..) | Ty::Array(_, _) | Ty::Slice(_)) | (Ty::Named(..) | Ty::Array(_, _) | Ty::Slice(_), Ty::Unit) => {
                Err((a.clone(), b.clone()))
            }
            // `Vec` は配列・スライスと別の型 (4.2: 暗黙の変換は `&[T; N]` → `&[T]` のみ)。
            (Ty::Named(n, _, _), Ty::Array(_, _) | Ty::Slice(_)) | (Ty::Array(_, _) | Ty::Slice(_), Ty::Named(n, _, _)) if n == "alloc.vec.Vec" => {
                Err((a.clone(), b.clone()))
            }
            // 構造が確定していて種類が違う組 (`()` と `Result<..>`、整数とタプルなど) は矛盾 (fail-closed)。
            // `Vec` / `String` はスライス・`str` として扱われる場所があるので、ここでは対象にしない。
            (x, y) if Self::concrete_kind(x).is_some() && Self::concrete_kind(y).is_some() && Self::concrete_kind(x) != Self::concrete_kind(y) => {
                Err((a.clone(), b.clone()))
            }
            // 意図した近似 (明示): `String` は `str`、`dyn Tr` は `&T` の受け側 (境界は別の検査が見る)。
            (Ty::Named(n, _, _), Ty::Prim(p)) | (Ty::Prim(p), Ty::Named(n, _, _)) if n == "alloc.string.String" && p == "str" => Ok(()),
            (Ty::Dyn(..), _) | (_, Ty::Dyn(..)) => Ok(()),
            _ => Err((a.clone(), b.clone())),
        }
    }

    /// メソッドのレシーバの型 (`self_ty`) と、呼び出しの実際のレシーバ (`view`) を合わせる。
    /// レシーバは暗黙に借用されるので、配列はスライスのメソッドを呼べる (`&[T; N]` → `&[T]`)。
    fn unify_recv(&mut self, self_ty: &Ty, view: &Ty) -> Result<(), (Ty, Ty)> {
        let (w, g) = (self.resolve(self_ty).strip().clone(), self.resolve(view).strip().clone());
        if let (Ty::Slice(x), Ty::Array(y, _)) = (&w, &g) {
            let (x, y) = ((**x).clone(), (**y).clone());
            return self.unify2(&x, &y);
        }
        self.unify2(self_ty, view)
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
            Ty::Named(n, a, rs) => Ty::Named(n.clone(), a.iter().map(|x| self.inst(x, map)).collect(), rs.clone()),
            Ty::Ref(m, x, rg) => Ty::Ref(*m, Box::new(self.inst(x, map)), rg.clone()),
            Ty::Ptr(m, x) => Ty::Ptr(*m, Box::new(self.inst(x, map))),
            Ty::Tuple(xs) => Ty::Tuple(xs.iter().map(|x| self.inst(x, map)).collect()),
            // 関数型 `fn(A) -> R` の中にも型パラメータが現れる (`core.ffi.trampoline` の戻り値)。
            Ty::Closure { args, ret, cap, id, eff } => Ty::Closure {
                args: args.iter().map(|x| self.inst(x, map)).collect(),
                ret: Box::new(self.inst(ret, map)),
                cap: cap.clone(),
                id: id.clone(),
                eff: eff.clone(),
            },
            Ty::Array(x, n) => Ty::Array(Box::new(self.inst(x, map)), *n),
            Ty::Slice(x) => Ty::Slice(Box::new(self.inst(x, map))),
            other => other.clone(),
        }
    }

    /// 明示型引数 (`f::<..>`, 4.3) のうち型と定数の実引数を、宣言のパラメータ名に束縛する。
    /// 効果 (`uses [..]`) と panic (`never` / `may`) の実引数は効果検査 (check.rs) で扱うのでここでは無視する。
    /// 実引数の種別が宣言と並ばない (誤った) 呼び出しでは何も束縛しない (黙る)。
    /// 型の実引数は、書かれた位置 (呼び出し元) の型パラメータ名で解釈する。
    fn seed_targs(&mut self, d: &Def, targs: &[V], map: &mut HashMap<String, Ty>, _line: usize) {
        if targs.is_empty() {
            return;
        }
        // 明示型引数の数は宣言と一致しなければならない (E2023 は TAST の規則 T の `turbofish` が報告する)。
        let want = d.generics.iter().filter(|g| g.0 != "life").count();
        let got = targs.iter().filter(|a| tag(a) != "life").count();
        if want != got {
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
        if let Some(p) = Self::shape_conflict(e) {
            return Some(p);
        }
        match (&e.0, &e.1) {
            (Ty::Prim(a), Ty::Prim(b)) => Some((a.clone(), b.clone())),
            (Ty::Const(a), Ty::Const(b)) => Some((a.clone(), b.clone())),
            (Ty::Ref(true, _, _), Ty::Ref(false, _, _)) => Some((e.0.show(), e.1.show())),
            _ => None,
        }
    }

    // ------------------------------------------------------------ 型の情報
    fn type_def(&self, fqn: &str) -> Option<&'a Def> {
        self.p.types.get(fqn).map(|&i| &self.p.defs[i])
    }

    /// `let` の束縛パターンが外れうるか (反駁可能)。列挙子は、その列挙が 2 つ以上の列挙子を持つなら外れうる。
    /// 名前が解決できない形は別の診断が報告するので外れないとみなす。
    fn let_refutable(&self, p: &V) -> bool {
        match nname(p) {
            "PBind" | "PWild" => false,
            "PRef" => self.let_refutable(&nf(p)[0]),
            "PTuple" => items(&nf(p)[0]).iter().any(|x| self.let_refutable(x)),
            "PLit" => true,
            "PVariant" | "PPath" | "PStruct" => {
                let segs = strs(&nf(p)[0]);
                if segs.len() == 1 && matches!(segs[0].as_str(), "Some" | "None" | "Ok" | "Err") {
                    return true;
                }
                match self.p.resolve(&segs, &self.def().file) {
                    Some(r) if r.rest.is_empty() => match &r.target {
                        Target::Variant(en, _) => {
                            if en == "Option" || en == "Result" {
                                return true;
                            }
                            let single = self.type_def(en).map_or(false, |td| td.variants.len() == 1);
                            if !single {
                                return true;
                            }
                            match nname(p) {
                                "PVariant" => items(&nf(p)[1]).iter().any(|x| self.let_refutable(x)),
                                "PStruct" => items(&nf(p)[1]).iter().any(|f| self.let_refutable(&items(f)[1])),
                                _ => false,
                            }
                        }
                        // 構造体のパターン: 欄のパターンだけで決まる。
                        _ => match nname(p) {
                            "PStruct" => items(&nf(p)[1]).iter().any(|f| self.let_refutable(&items(f)[1])),
                            "PVariant" => items(&nf(p)[1]).iter().any(|x| self.let_refutable(x)),
                            _ => false,
                        },
                    },
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn field_ty(&self, recv: &Ty, name: &str) -> Ty {
        let r = self.deep(recv);
        let Ty::Named(fqn, args, _) = r.strip() else { return Ty::Unknown };
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
            Ty::Slice(_) | Ty::Array(_, _) => (vec!["core.slice".into(), "core.array".into()], true),
            Ty::Named(n, _, _) => match n.as_str() {
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
            (Ty::Named(n, a, _), "core.slice") if n == "alloc.vec.Vec" => {
                Ty::Slice(Box::new(a.first().cloned().unwrap_or(Ty::Unknown)))
            }
            (Ty::Named(n, _, _), "core.str") if n == "alloc.string.String" => Ty::Prim("str".into()),
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
    /// 【担当 C】メソッド呼び出しの暗黙の変換の候補を積む (型が決まる関数の終わりに `flush_conv` が表へ移す)。
    /// 候補が 1 つに決まったときだけ (`fit` が 1 件)。受け手の鍵は `MCall` の受け手の節点、`s.m()` 形 (パスの形) は呼び出しの節点。
    fn note_method_conv(&mut self, recv_id: u32, recv: &Ty, args: &[Ty], arg_ids: &[u32], fit: Option<&[usize]>) {
        let Some([di]) = fit else {
            if std::env::var("KSN_DUMP_SIDE").map_or(false, |v| v == "1") {
                let why = match fit { None => "no-fit (dyn / unknown receiver / not found)", Some([]) => "no-candidate", _ => "multiple-candidates" };
                eprintln!("coerce_none\t{}\t{}\t{}", self.def().name, recv_id, why);
            }
            return;
        };
        let d = &self.p.defs[*di];
        let gn = d.gnames();
        let Some(p0) = d.params.first() else { return };
        self.side.recv_pending.push(((self.cur, recv_id), recv.clone(), from_v(&p0.1, &gn)));
        if arg_ids.len() == args.len() {
            for (k, a) in args.iter().enumerate() {
                if let Some(p) = d.params.get(k + 1) {
                    let pt = from_v(&p.1, &gn);
                    self.side.conv_pending.push(((self.cur, arg_ids[k]), pt, a.clone()));
                }
            }
        }
    }

    /// 【担当 C】式 `e` が型 `want` の位置へ入る (代入の右辺・`let` の初期値・`return`・本体の末尾・欄・列挙子の中身)。型が決まる関数の終わりに `flush_conv` が印へ変える。
    fn note_conv(&mut self, e: &V, want: &Ty, got: &Ty) {
        self.side.conv_pending.push(((self.cur, crate::parser::nid(e)), want.clone(), got.clone()));
    }

    /// 【担当 C】受け手の変換 (`r` = 実際の受け手の型、`selfp` = `self` パラメータの宣言の型)。決まらないとき `Err(理由)`。
    fn recv_conv(r: &Ty, selfp: &Ty) -> Result<Vec<crate::tast::Coercion>, &'static str> {
        use crate::tast::Coercion;
        // 参照の層をはがす (各層の可変性を持つ)。
        fn peel(t: &Ty) -> (Vec<bool>, &Ty) {
            let mut v = vec![];
            let mut c = t;
            while let Ty::Ref(m, i, _) = c {
                v.push(*m);
                c = i;
            }
            (v, c)
        }
        fn head(t: &Ty) -> u8 {
            match t {
                Ty::Named(..) => 1,
                Ty::Prim(_) => 2,
                Ty::Slice(_) => 3,
                Ty::Array(..) => 4,
                Ty::Tuple(_) => 5,
                Ty::Dyn(..) => 6,
                Ty::Param(_) => 7,
                Ty::Closure { .. } => 8,
                _ => 0,
            }
        }
        let (rm, ri) = peel(r);
        let (sm, si) = peel(selfp);
        if matches!(ri, Ty::Var(_) | Ty::Unknown | Ty::Never) {
            return Err("recv-type-undetermined");
        }
        if matches!(ri, Ty::Param(_)) && !sm.is_empty() && rm.is_empty() {
            return Ok(vec![Coercion::AutoRef(sm[0])]);
        }
        // `Box<T>` の受け手は、`self` の型の頭に合うまで外す。
        let box_inner = |t: &Ty| -> Option<Ty> {
            match t {
                Ty::Named(n, a, _) if n == "alloc.boxed.Box" => a.first().cloned(),
                _ => None,
            }
        };
        let same_head = |a: &Ty, b: &Ty| match (a, b) {
            (Ty::Named(x, _, _), Ty::Named(y, _, _)) => x == y,
            _ => head(a) == head(b),
        };
        let mut cur = ri.clone();
        let mut boxes = 0u8;
        while !same_head(&cur, si) && !matches!(si, Ty::Param(_)) {
            match box_inner(&cur) {
                Some(x) => {
                    cur = x;
                    boxes += 1;
                }
                None => break,
            }
        }
        if !same_head(&cur, si) && !matches!(si, Ty::Param(_)) {
            return Err(if matches!(ri, Ty::Dyn(..)) { "recv-dyn" } else { "recv-no-match" });
        }
        let mut cs = vec![];
        if boxes > 0 {
            cs.push(Coercion::AutoDeref { n: rm.len() as u8 + boxes });
            if let Some(m) = sm.first() {
                cs.push(Coercion::AutoRef(*m));
            }
            return Ok(cs);
        }
        let (rd, sd) = (rm.len(), sm.len());
        if rd > sd {
            let n = rd - sd;
            cs.push(Coercion::AutoDeref { n: n as u8 });
            // 着いた層が `&mut` で `self` が `&` なら再借用。
            if sd > 0 && rm[n] && !sm[0] {
                cs.push(Coercion::Reborrow { to_mut: false });
            }
        } else if rd == sd {
            if sd > 0 && rm[0] && !sm[0] {
                cs.push(Coercion::Reborrow { to_mut: false });
            }
        } else if rd == 0 {
            cs.push(Coercion::AutoRef(sm[0]));
        }
        Ok(cs)
    }

    /// 【担当 C】型が決まった後で、積んだ候補から `SideTabs::coercions` を埋める。
    fn flush_conv(&mut self) {
        use crate::tast::Coercion;
        let fqn = self.def().name.clone();
        let dump = std::env::var("KSN_DUMP_SIDE").map_or(false, |v| v == "1");
        for (key, recv, selfp) in std::mem::take(&mut self.side.recv_pending) {
            let r = self.deep(&recv);
            let sp = self.deep(&selfp);
            match Self::recv_conv(&r, &sp) {
                Ok(cs) => self.put_conv(&fqn, key, cs),
                Err(why) => {
                    if dump {
                        eprintln!("coerce_none\t{}\t{}\t{}", fqn, key.1, why);
                    }
                }
            }
        }
        for (key, from, to, variant) in std::mem::take(&mut self.side.err_pending) {
            let (f, t) = (self.deep(&from), self.deep(&to));
            self.put_conv(&fqn, key, vec![Coercion::ErrInto { from: f, to: t, variant }]);
        }
        for (key, want, got) in std::mem::take(&mut self.side.conv_pending) {
            let (w, g) = (self.deep(&want), self.deep(&got));
            let mut cs = vec![];
            match (&w, &g) {
                (Ty::Ref(wm, wi, _), Ty::Ref(gm, gi, _)) => {
                    if *gm && !*wm {
                        cs.push(Coercion::Reborrow { to_mut: false });
                    }
                    match (&**wi, &**gi) {
                        (Ty::Slice(_), Ty::Array(..)) => cs.push(Coercion::ArrayToSlice),
                        (Ty::Dyn(..), x) if !matches!(x, Ty::Dyn(..) | Ty::Var(_) | Ty::Unknown | Ty::Param(_)) => cs.push(Coercion::Unsize { from: g.clone(), to: w.clone() }),
                        _ => {}
                    }
                }
                (Ty::Closure { id: None, .. }, Ty::Closure { id: Some(_), .. }) => cs.push(Coercion::ClosureToFn),
                (Ty::Named(wn, wa, _), Ty::Named(gn, ga, _)) if wn == "alloc.boxed.Box" && gn == wn => {
                    if let (Some(Ty::Dyn(..)), Some(x)) = (wa.first(), ga.first()) {
                        if !matches!(x, Ty::Dyn(..) | Ty::Var(_) | Ty::Unknown | Ty::Param(_)) {
                            cs.push(Coercion::Unsize { from: g.clone(), to: w.clone() });
                        }
                    }
                }
                _ => {}
            }
            if let (Ty::Closure { eff: Eff(Some(we)), .. }, Ty::Closure { eff: Eff(Some(_)), .. }) = (&w, &g) {
                if we.decl {
                    cs.push(Coercion::FnTo(w.clone()));
                }
            }
            self.put_conv(&fqn, key, cs);
        }
    }

    fn put_conv(&mut self, fqn: &str, key: crate::tast::Key, cs: Vec<crate::tast::Coercion>) {
        if cs.is_empty() {
            return;
        }
        if std::env::var("KSN_DUMP_SIDE").map_or(false, |v| v == "1") {
            eprintln!("coercions\t{}\t{}\t{:?}", fqn, key.1, cs);
        }
        let e = self.side.coercions.entry(key).or_default();
        for c in cs {
            if !e.contains(&c) {
                e.push(c);
            }
        }
    }

    fn call_method(&mut self, id: u32, recv_id: u32, recv: &Ty, base: Option<&str>, name: &str, args: &[Ty], line: usize, targs: &[V], local: bool) -> Ty {
        let saved = self.last_fit.take();
        let arg_ids = std::mem::take(&mut self.side.arg_ids);
        self.bound_hit = None;
        self.wants_tmp = None;
        let t = self.call_method_in(id, recv, base, name, args, line, targs, local);
        if let Some(ws) = self.wants_tmp.take() {
            self.side.arg_wants_pending.push(((self.cur, id), ws));
        }
        if let Some(f) = self.bound_hit.take() {
            self.bound_sites.insert((self.cur, id), f);
        }
        let fit = std::mem::replace(&mut self.last_fit, saved);
        self.note_method_conv(recv_id, recv, args, &arg_ids, fit.as_deref());
        let key = (self.cur, id);
        // 側の表 (S1A): `dyn` 越し (`Box<dyn Tr>` も) は `Callee::Dyn`、それ以外は下で `call_res` から写す。
        let dyn_tr = {
            let deep = self.deep(recv);
            match deep.strip().clone() {
                Ty::Dyn(tr, _, _) => Some(tr),
                Ty::Named(n, a, _) if n == "alloc.boxed.Box" && !matches!(name, "as_ref" | "as_mut" | "into_inner" | "drop") => match a.first().map(|x| self.deep(x)) {
                    Some(Ty::Dyn(tr, _, _)) => Some(tr),
                    _ => None,
                },
                _ => None,
            }
        };
        match (self.call_res.get_mut(&key), fit) {
            (None, f) => {
                self.call_res.insert(key, f);
            }
            (Some(Some(old)), Some(f)) => old.extend(f),
            (Some(slot), _) => *slot = None,
        }
        let dyn_fqn = dyn_tr.and_then(|tr| crate::check::resolve_trait_fqn(self.p, &tr));
        let callee = match dyn_fqn.as_ref().and_then(|r| self.p.types.get(r).copied()) {
            Some(ti) => {
                // `dyn Tr` 越しのメソッド: LSP がトレイトの宣言行を辿れるよう、トレイトメソッドの FQN を覚える
                // (`call_sites` の `bound_sites` 経由で `locate` が位置から引く。境界の総称と同じ扱い)。
                if let Some(fqn) = dyn_fqn.as_deref() {
                    self.bound_sites.insert((self.cur, id), format!("{}.{}", fqn, name));
                }
                Callee::Dyn(ti)
            }
            None => match self.call_res.get(&key) {
                Some(Some(v)) if v.len() == 1 => Callee::Def(v[0]),
                Some(Some(v)) if !v.is_empty() => Callee::Methods(v.clone()),
                _ => Callee::Unknown,
            },
        };
        self.note_callee(id, callee);
        t
    }

    /// 側の表 `resolved_fn` へ呼び出し節点の解決先を書く (式 ID 0 = 未採番は書かない。同じ鍵への再訪は後勝ち)。
    fn note_callee(&mut self, id: u32, c: Callee) {
        if id != 0 {
            self.side.resolved_fn.insert((self.cur, id), c);
        }
    }

    fn call_method_in(&mut self, call_id: u32, recv: &Ty, base: Option<&str>, name: &str, args: &[Ty], line: usize, targs: &[V], local: bool) -> Ty {
        let arg_stat = std::mem::take(&mut self.arg_static);
        // トレイトオブジェクト (5.1): `&dyn Tr` のメソッド呼び出しは、Tr とそのスーパートレイトの
        // シグネチャで検査する (Self は `dyn Tr` 自身)。
        let deep = self.deep(recv);
        if let Ty::Dyn(tr, ta, _) = deep.strip().clone() {
            return self.dyn_method(&tr, &ta, &deep, base, name, args, line);
        }
        // `Box<dyn Tr, A>` のレシーバは、`Tr` のメソッドを探す (自動参照外し。4.2 / 5.1)。`Box` 自身のメソッド
        // (`as_ref` / `as_mut` / `into_inner`) は、型の側の定義を使う。
        if let Ty::Named(n, a, _) = deep.strip().clone() {
            if n == "alloc.boxed.Box" && !matches!(name, "as_ref" | "as_mut" | "into_inner" | "drop") {
                if let Some(Ty::Dyn(tr, ta, _)) = a.first().map(|x| self.deep(x)) {
                    return self.dyn_method(&tr, &ta, &Ty::Dyn(tr.clone(), ta.clone(), crate::types::Region::Unk), base, name, args, line);
                }
            }
        }
        // `Box<T>` (`T` は具体の型) のレシーバ: `Box` 自身のメソッドが無ければ、中身 `T` のメソッドを探す (自動参照外し、4.2)。
        if let Ty::Named(n, a, _) = deep.strip().clone() {
            if n == "alloc.boxed.Box" && !self.owners.get("alloc.boxed.Box").map_or(false, |m| m.contains_key(name)) {
                if let Some(inner) = a.first().map(|x| self.deep(x)) {
                    if !matches!(inner.strip(), Ty::Var(_) | Ty::Unknown | Ty::Never) {
                        self.arg_static = arg_stat;
                        return self.call_method_in(call_id, &inner, base, name, args, line, targs, local);
                    }
                }
            }
        }
        if matches!(name, "get" | "get_mut" | "contains_key" | "contains" | "remove") && args.len() == 1 {
            self.oneway_sites.push((recv.clone(), name.to_string(), line));
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
            if let Some(t) = self.trait_bound_method(recv, base, name, args, line) {
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
            // どの候補も引数の数が合わない: 候補が複数でも (レシーバの型の持ち主が全部分かっているなら) 誤り。
            if !cands.is_empty() && complete {
                let mut wants: Vec<usize> = cands.iter().map(|(i, _)| self.p.defs[*i].params.len().saturating_sub(1)).collect();
                wants.sort();
                wants.dedup();
                let w: Vec<String> = wants.iter().map(|n| n.to_string()).collect();
                self.diag("E2022", format!("method `{}` takes {} argument(s) but {} given", name, w.join(" or "), args.len()), line);
            }
            escape_all(self);
            return Ty::Unknown;
        }
        // 効果 (4.3 / 5.1): 引数の個数が合う候補 (型で決まった所有者のメソッド) をすべて見る。
        // 検査は全候補が違反するときだけ、クロージャの効果には全候補に共通する部分だけを足す (保守側)。
        // 候補が 1 つに決まるときだけ、`Fn` 境界の具体的な効果とクロージャの効果を比べる (5.1 / 4.3)。
        // 受け手 (`self`) も引数と同じに扱う (`self: &S<F>` の `F` の `Fn` 境界)。
        // `'static` の境界 (P0-6): 関数呼び出しと同じ判定。候補が複数なら全部に当てる (fail-closed)。
        for (i, _) in &fit {
            let d = &self.p.defs[*i];
            self.note_static_args(d, 1, args, &arg_stat, line);
        }
        let with_self: Vec<Ty> = std::iter::once(recv.clone()).chain(args.iter().cloned()).collect();
        let effs: Vec<(String, Vec<String>, Option<String>, Vec<String>, Option<String>)> = fit
            .iter()
            .map(|(i, _)| {
                let c = &self.p.defs[*i];
                let (fu, fp, du, dm) = self.callee_effects(c, 0, &with_self, targs);
                let (cu, cp) = if local { (fu.clone(), fp.clone()) } else { (du, dm) };
                (c.name.clone(), fu, fp, cu, cp)
            })
            .collect();
        self.record_effects(line, effs);
        if complete {
            self.last_fit = Some(fit.iter().map(|(i, _)| *i).collect());
        }
        let (di, owner) = fit[0].clone();
        let d = &self.p.defs[di];
        let gn = d.gnames();
        self.add_edge(d.name.clone());
        if let Some(b) = base {
            if matches!(from_v(&d.params[0].1, &gn), Ty::Ref(true, _, _)) {
                self.need_mut(b, name, line);
            }
        }
        let mut map: HashMap<String, Ty> = HashMap::new();
        self.seed_targs(d, targs, &mut map, line);
        seed_effects(d, &mut map);
        let (sig_ps, sig_ret) = Self::sig_elided(d, &gn);
        let self_ty = from_v(&d.params[0].1, &gn);
        let self_ty = self.inst(&self_ty, &mut map);
        let self_inst = self.inst(&Self::inst_regions(call_id, &sig_ps[0]), &mut map);
        if matches!(self.deep(recv), Ty::Ref(..)) {
            self.note_region_cstr(call_id, &self_inst, recv, "recv");
        }
        let view = self.view_for(recv, &owner);
        let _ = self.unify_recv(&self_ty, &view);
        for (k, a) in args.iter().enumerate() {
            let pt = self.inst(&Self::inst_regions(call_id, &sig_ps[k + 1]), &mut map);
            self.note_region_cstr(call_id, &pt, a, "arg");
            let _ = self.unify2(&pt, a);
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
                self.unify_fn_bound(d, &pname, &cl, &mut map, line, &mut cap_bad);
            }
        }
        let r = self.inst(&Self::inst_regions(call_id, &sig_ret), &mut map);
        self.note_type_args(call_id, d, &map);
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
            Ty::Named(n, _, _) => self.p.types.get(n).map_or(true, |&i| self.p.defs[i].derive.iter().any(|x| x == "Ord")),
            _ => true,
        }
    }

    /// 比較のメソッド (`eq` / `cmp` / `partial_cmp`) または比較演算子 (`==` `<` など) を、受け手の利用者型・標準の型が
    /// 対応するトレイトを持たないのに使ったとき E1108 (true を返す)。宣言 (`p.types`) のある型だけを見る。
    /// 型変数・型パラメータ・宣言のない型は黙る。Vec / String は 4.2 の表どおり比較のトレイトを持たない。
    /// 二項演算子の被演算子の参照の段数 (G24, 4.2): 段数の違う組は型に依らず E2024 (`&u8 == u8`、`&Foo == Foo`、`&&str == &str`)。
    /// 同じ段数の参照どうし (`&str == &str`、`&Foo == &Foo`、スライス、`&u32 == &u32`) は通す。型が決まらないものは見ない。
    fn operator_ref_operands(&mut self, a: &Ty, b: &Ty, op: &str, line: usize) {
        fn depth(t: &Ty) -> usize {
            let mut n = 0;
            let mut t = t;
            while let Ty::Ref(_, i, _) = t {
                n += 1;
                t = i;
            }
            n
        }
        let (da, db) = (self.deep(a), self.deep(b));
        let known = |t: &Ty| !matches!(t, Ty::Unknown | Ty::Var(_) | Ty::Param(_) | Ty::Never);
        if !known(da.strip()) || !known(db.strip()) {
            return;
        }
        let (na, nb) = (depth(&da), depth(&db));
        if na == nb {
            return;
        }
        let side = if na > nb { "left" } else { "right" };
        self.diag(
            "E2024",
            format!(
                "operands of `{}` are `{}` and `{}` (the {} operand has more references): a reference is not dereferenced implicitly: write `*c` (an operator does not dereference, 4.2)",
                op,
                da.show(),
                db.show(),
                side
            ),
            line,
        );
    }

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
        // 型パラメータのレシーバは境界からのメソッド探索 (`trait_bound_method`) と `check_cmp_bound` が見る。
        // `&T` は `T` によらず Copy + Clone (4.2) なので、共有参照の `clone` / `try_clone` は見ない。
        if matches!(self.deep(recv), Ty::Ref(false, _, _)) && matches!(name, "clone" | "try_clone") {
            return false;
        }
        let whole = self.deep(recv).strip().clone();
        if matches!(whole, Ty::Param(_)) {
            return false;
        }
        let found = if matches!(tr, "core.ops.Clone" | "alloc.alloc.TryClone") {
            let caps = |n: &str| self.closure_caps.get(n).map(|c| c.iter().map(|(k, _, t)| (k.clone(), self.deep(t))).collect());
            marker_unsat(&whole, &caps, &|x: &Ty| self.unsatisfied(x.strip(), tr))
        } else {
            self.unsatisfied(&whole, tr)
        };
        let Some(leaf) = found else { return false };
        let ty = match &whole {
            Ty::Named(n, _, _) if leaf == whole => n.clone(),
            _ => whole.show(),
        };
        let hint = if leaf == whole {
            "derive it or write an impl".to_string()
        } else {
            let short = tr.rsplit('.').next().unwrap_or(tr);
            format!("`{}` must implement it too: add `derive [{}]` to `{}`", leaf.show(), short, leaf.show())
        };
        let is_method = name.chars().all(|c| c.is_alphanumeric() || c == '_');
        let msg = if is_method {
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

    fn trait_bound_method(&mut self, recv: &Ty, base: Option<&str>, name: &str, args: &[Ty], line: usize) -> Option<Ty> {
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
        // 複数のトレイト (境界とそのスーパートレイトの推移閉包) が同名のメソッドを宣言するなら曖昧 (3.2: 複数該当はエラー)。
        {
            let mut seen2: HashSet<String> = HashSet::new();
            let mut q2: Vec<String> = queue.iter().map(|x| x.0.clone()).collect();
            let mut hits: Vec<String> = vec![];
            let mut k = 0;
            while k < q2.len() {
                let tr = q2[k].clone();
                k += 1;
                if !seen2.insert(tr.clone()) {
                    continue;
                }
                let Some(&ti) = self.p.types.get(&tr) else { continue };
                let td = &self.p.defs[ti];
                if td.trait_sigs.iter().any(|s| s.0 == name) {
                    hits.push(tr.clone());
                }
                for (sup, _) in &td.requires {
                    if let Some(fq) = crate::check::resolve_trait_fqn(self.p, sup) {
                        q2.push(fq);
                    }
                }
            }
            if hits.len() > 1 {
                self.quiet_here();
                self.diag("E1108", format!("method `{}` on type parameter `{}` is ambiguous: declared by {}", name, pn, hits.join(", ")), line);
                for a in args {
                    self.escape(a);
                }
                return Some(Ty::Unknown);
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
            let found = td.trait_sigs.iter().find(|s| s.0 == name).is_some();
            if found {
                self.bound_hit = Some(format!("{}.{}", tr, name));
            }
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
            map.insert("Self".to_string(), deep.strip().clone());
            seed_effects(td, &mut map);
            if let Some(b) = base {
                if matches!(from_v(&params[0].1, &gn), Ty::Ref(true, _, _)) {
                    self.need_mut(b, name, line);
                }
            }
            // トレイトの型パラメータ (`Fn<Args, R, ..>` の `R` など) を、境界に書かれた実引数に束縛する。
            // 実引数の `Self` (スーパートレイトの `PartialEq<Self>`) は、レシーバの型。
            let tparams: Vec<&(String, String, V)> = td.generics.iter().filter(|g| g.0 != "life").collect();
            for (g, a) in tparams.iter().zip(&targs) {
                if g.0 == "type" {
                    let a = if *a == Ty::Param("Self".into()) { deep.strip().clone() } else { a.clone() };
                    map.insert(g.1.clone(), a);
                }
            }
            // 実引数との食い違い (E2023) は TAST の規則 T が `arg_wants` から報告する。ここは推論 (型変数の束縛) だけ。
            let mut wants = vec![];
            for (k, a) in args.iter().enumerate() {
                let pt = self.sig_ty(&params[k + 1].1, &gn);
                let pt = self.inst(&pt, &mut map);
                let _ = self.unify2(&pt, a);
                wants.push(pt);
            }
            self.wants_tmp = Some(wants);
            if let Some((u, p)) = eff {
                self.check_trait_effects(format!("{}.{}", tr, name), u, p, line);
            }
            let r = self.sig_ty(ret, &gn);
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
            let pt = self.sig_ty(&params[k + 1].1, &gn);
            let pt = self.inst(&pt, &mut map);
            let _ = self.unify2(&pt, a);
        }
        let r = self.sig_ty(ret, &gn);
        Some(self.inst(&r, &mut map))
    }

    /// `for x in it` の要素の型。配列・スライス・`Vec` はその要素、それ以外は `next` の戻り値 `Option<T>` の `T`。
    fn iter_elem(&mut self, it: &Ty) -> Ty {
        let deep = self.deep(it);
        match deep.strip() {
            Ty::Array(x, _) | Ty::Slice(x) => return (**x).clone(),
            Ty::Named(n, a, _) if n == "alloc.vec.Vec" => return a.first().cloned().unwrap_or(Ty::Unknown),
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
            let _ = self.unify_recv(&self_ty, &view);
            let ret = self.inst(&from_v(&d.ret, &gn), &mut map);
            if let Ty::Named(n, a, _) = self.deep(&ret) {
                if n == "Option" {
                    return a.first().cloned().unwrap_or(Ty::Unknown);
                }
            }
        }
        Ty::Unknown
    }

    /// `for x in it` が暗黙に呼ぶ `next` の効果を、明示の `it.next()` と同じに数える。型パラメータなら
    /// `Iterator` 境界の効果実引数、それ以外は `next` の宣言の効果 (呼び出し元の検査へ回す)。
    fn for_next_effects(&mut self, it: &Ty, line: usize) {
        let deep = self.deep(it);
        let mut base = deep.strip().clone();
        while let Ty::Ref(_, x, _) = base {
            base = self.deep(&x).strip().clone();
        }
        match &base {
            Ty::Array(..) | Ty::Slice(_) => return,
            Ty::Named(n, _, _) if n == "alloc.vec.Vec" => return,
            Ty::Param(pn) => {
                let d = self.def();
                let mut found = vec![];
                for g in d.generics.iter().filter(|g| g.0 == "type" && &g.1 == pn) {
                    for b in items(&g.2) {
                        if !matches!(b, V::L(bx) if st(&bx[0]) == "core.iter.Iterator") {
                            continue;
                        }
                        let ta = items(tel(b, 1));
                        let uses = ta.get(1).and_then(crate::check::targ_effs).unwrap_or_default();
                        let panics = ta.get(2).and_then(crate::check::targ_panic);
                        found.push((uses, panics));
                    }
                }
                for (uses, panics) in found {
                    self.check_trait_effects("core.iter.Iterator.next".into(), uses, panics, line);
                }
                return;
            }
            _ => {}
        }
        let (owners, _) = self.owners_for(&base);
        let mut effs = vec![];
        let mut inner: Vec<Ty> = vec![];
        for o in &owners {
            let Some(&di) = self.owners.get(o).and_then(|m| m.get("next")).and_then(|v| v.first()) else { continue };
            let c = &self.p.defs[di];
            if c.params.len() != 1 {
                continue;
            }
            // 受け手 (`self: &mut S<F..>`) の型を渡す: `F` の `Fn` 境界の効果 (`Map` の `E` / `P`) を、型の実引数の
            // クロージャから束ねる (型の効果引数は `Ty` に残らないので、`F` から再構成する)。
            let self_arg = Ty::Ref(true, Box::new(base.clone()), crate::types::Region::Unk);
            let (fu, fp, _, _) = self.callee_effects(c, 0, &[self_arg.clone()], &[]);
            // 型の効果引数 (`Enumerate<I, T, E, P>` の `E` / `P`) は型に残らないので、実体の分からない変数は
            // 明示の `it.next()` と同じく数えない (宣言の具体名と `may` / `never` だけ数える)。
            let fu: Vec<String> = fu.into_iter().filter(|e| !crate::check::is_effect_var(e)).collect();
            let fp = fp.filter(|p| !crate::check::is_panic_var(p));
            effs.push((c.name.clone(), fu.clone(), fp.clone(), fu, fp));
            // `next` が内側のイテレータ (`I: Iterator<..>` の欄) を呼ぶなら、その `next` の効果も数える (入れ子のアダプタ)。
            let gn = c.gnames();
            let iter_params: Vec<String> = c
                .generics
                .iter()
                .filter(|g| g.0 == "type" && items(&g.2).iter().any(|b| matches!(b, V::L(bx) if st(&bx[0]) == "core.iter.Iterator")))
                .map(|g| g.1.clone())
                .collect();
            if !iter_params.is_empty() {
                for (n, at) in self.param_slots(&from_v(&c.params[0].1, &gn), &self_arg) {
                    if iter_params.contains(&n) {
                        inner.push(at);
                    }
                }
            }
        }
        self.record_effects(line, effs);
        for at in inner {
            self.for_next_effects(&at, line);
        }
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
                "uses" => {
                    if let Some(set) = crate::check::targ_effs(a) {
                        bind_u.insert(g.1.clone(), set);
                    }
                }
                "panics" => {
                    if let Some(p) = crate::check::targ_panic(a) {
                        bind_p.insert(g.1.clone(), p);
                    }
                }
                _ => {}
            }
        }
        let concrete = |e: &String| e.chars().next().map_or(false, |c| c.is_lowercase());
        // 束縛された効果変数は、呼び出し元の効果変数 (大文字の名前) もそのまま残す。残った変数は
        // 呼び出し元の `uses` に同じ名前がなければ E4001 になる (`violation_effs`)。
        let mut eu: Vec<String> = vec![];
        for e in us {
            if concrete(e) {
                eu.push(e.clone());
            } else if let Some(set) = bind_u.get(e) {
                eu.extend(set.iter().cloned());
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
            let low: BTreeSet<String> = uses.iter().cloned().collect();
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
        let mut queue: Vec<(String, Vec<V>)> = vec![(root.clone(), dargs.0.clone())];
        while let Some((cur, cur_targs)) = queue.pop() {
            if !seen.insert(cur.clone()) {
                continue;
            }
            let Some(&ti) = self.p.types.get(&cur) else { continue };
            let td = &self.p.defs[ti];
            if td.kw != "trait" {
                continue;
            }
            let Some((_, params, ret)) = td.trait_sigs.iter().find(|s| s.0 == name) else {
                // スーパートレイトの型引数は、このトレイトの効果変数を `dyn` の実引数で置き換えて渡す。
                let pairs = crate::check::align_targs(&td.generics, &cur_targs).unwrap_or_default();
                for (sup, sargs) in &td.requires {
                    if let Some(fq) = crate::check::resolve_trait_fqn(self.p, sup) {
                        let sa: Vec<V> = items(sargs)
                            .iter()
                            .map(|a| {
                                let var = match (crate::check::targ_effs(a), crate::check::targ_panic(a)) {
                                    (Some(e), _) if e.len() == 1 => Some(e[0].clone()),
                                    (_, Some(p)) => Some(p),
                                    _ => None,
                                };
                                var.and_then(|v| pairs.iter().find(|(n, _, _)| *n == v.as_str()).map(|(_, _, t)| (*t).clone())).unwrap_or_else(|| a.clone())
                            })
                            .collect();
                        queue.push((fq, sa));
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
                let (u2, p2) = crate::check::inst_effects(&td.generics, &cur_targs, u, pn);
                // クロージャの本体の中なら、実体化した宣言の効果 (上限) をそのクロージャの効果に数える (5.1)。
                if !self.def().inferred {
                    if let Some(acc) = self.eff_acc.last_mut() {
                        acc.0.extend(u2.iter().cloned());
                        if p2 == "may" {
                            acc.1 = true;
                        }
                        acc.2 = true;
                    }
                }
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
                if matches!(from_v(&params[0].1, &gn), Ty::Ref(true, _, _)) {
                    self.need_mut(b, name, line);
                }
            }
            // 実引数との食い違い (E2023) は TAST の規則 T が `arg_wants` から報告する。ここは推論 (型変数の束縛) だけ。
            let mut wants = vec![];
            for (k, a) in args.iter().enumerate() {
                let pt = self.sig_ty(&params[k + 1].1, &gn);
                let pt = self.inst(&pt, &mut map);
                let _ = self.unify2(&pt, a);
                wants.push(pt);
            }
            self.wants_tmp = Some(wants);
            // 効果 (5.1): `dyn Tr` の型引数は `Ty::Dyn` が持たないので、トレイトの効果変数は束縛せず落とす (黙る)。
            // 具体名と `never` / `may` の宣言で検査する。
            let eff = Self::trait_effects_inst(td, name, &[]);
            if let Some((u, p)) = eff {
                self.check_trait_effects(format!("{}.{}", cur, name), u, p, line);
            }
            let r = self.sig_ty(ret, &gn);
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

    /// 場所式 `e` (`x` / `x.f` / `x[i]`) を可変に使う (`&mut e` / 代入) とき、根の束縛が `let mut` か `&mut` の参照でなければ E3031
    /// (メソッド呼び出しの自動の `&mut` と同じ規則)。`*r` を通す書き込みは束縛の `mut` と無関係なので見ない。
    fn need_mut_place(&mut self, e: &V, what: &str, line: usize) {
        let (root, direct) = match nname(e) {
            "Path" => {
                let segs = strs(&nf(e)[0]);
                (segs[0].clone(), segs.len() == 1)
            }
            "Field" | "Index" => {
                let mut cur = e;
                while matches!(nname(cur), "Field" | "Index") {
                    cur = &nf(cur)[0];
                }
                match nname(cur) {
                    "Path" => {
                        let segs = strs(&nf(cur)[0]);
                        (segs[0].clone(), false)
                    }
                    _ => return,
                }
            }
            _ => return,
        };
        let Some(t) = self.lookup(&root) else { return };
        if self.mutable.contains(&root) {
            return;
        }
        let t = self.resolve(&t);
        let bad = match &t {
            Ty::Unknown | Ty::Var(_) | Ty::Never => false,
            // `&mut x` で参照の束縛そのものを借りる形は、参照の段数が決まらない束縛があるので見ない。
            Ty::Ref(..) if what == "borrow" && direct => false,
            Ty::Ref(true, _, _) | Ty::Param(_) if !direct => false,
            // `&mut r.f` を `r: &T` で書く形は、下の「共有参照の欄」の検査が E3031 にする。
            Ty::Ref(false, _, _) if what == "borrow" && !direct && matches!(nname(e), "Field" | "Path") => false,
            _ => true,
        };
        if bad {
            self.diag(
                "E3031",
                format!("cannot {} `{}` as mutable: declare it with `let mut {}`", what, root, root),
                line,
            );
        }
    }

    /// `&mut self` のメソッドを呼ぶには、レシーバの局所変数が `let mut` か、`&mut` の参照でなければならない。
    fn need_mut(&mut self, base: &str, method: &str, line: usize) {
        let Some(t) = self.lookup(base) else { return };
        match self.resolve(&t) {
            Ty::Ref(false, _, _) => self.diag(
                "E3031",
                format!("cannot call the `&mut self` method `{}` through the shared reference `{}`", method, base),
                line,
            ),
            Ty::Ref(true, _, _) | Ty::Unknown | Ty::Var(_) | Ty::Param(_) | Ty::Never => {}
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
            let Ty::Dyn(tr, _, _) = &d else {
                if matches!(d, Ty::Named(..) | Ty::Prim(_) | Ty::Tuple(_)) {
                    self.diag("E2046", format!("`Box.into_dyn` must produce a `Box<dyn ..>`, found `{}`", d.show()), line);
                }
                continue;
            };
            let Some(root) = crate::check::resolve_trait_fqn(self.p, tr) else { continue };
            // 型パラメータ: 宣言された境界 (とそのスーパートレイト) が `Tr` を含むときだけ `into_dyn` できる。
            if let Ty::Param(pn) = t.strip() {
                if param_bound_implies(self.p, self.def(), pn, &root) == Some(false) {
                    self.diag("E2046", format!("the type parameter `{}` does not implement the trait `{}`: add the bound `{}: {}`", pn, root, pn, root), line);
                }
                continue;
            }
            let Ty::Named(n, _, _) = t.strip() else { continue };
            let Some(&i) = self.p.types.get(n) else { continue };
            let (tr, n2) = (tr.clone(), n.clone());
            self.cur_line = line;
            self.check_dyn_impl(&tr, &n2);
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
            Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().any(|x| self.has_var(x)),
            Ty::Ref(_, x, _) | Ty::Array(x, _) | Ty::Slice(x) => self.has_var(&x),
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
            let Ty::Named(rn, _, _) = recv.strip().clone() else { continue };
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
            Ty::Named(n, args, _) => {
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
                    // 手書き・生成の impl (`derive [ToJson]` が作る `impl[T: ToJson] .. for Box<T>` を含む) は、
                    // その impl の型パラメータの境界で決まる。境界の無い手書きの impl は満たす。
                    let imp = self.p.defs.iter().find(|d| d.kw == "impl" && d.impl_trait == tr && crate::check::impl_target(&d.impl_for) == *n);
                    let Some(imp) = imp else { return None };
                    if let Ty::Named(_, iargs, _) = from_v(&imp.impl_for, &imp.gnames()) {
                        if iargs.len() == args.len() {
                            for (ia, a) in iargs.iter().zip(args) {
                                let Ty::Param(gp) = ia else { continue };
                                for g in imp.generics.iter().filter(|g| g.0 == "type" && g.1 == *gp) {
                                    for b in items(&g.2) {
                                        let V::L(bx) = b else { continue };
                                        if let Some(leaf) = self.unsatisfied(self.deep(a).strip(), &st(&bx[0])) {
                                            return Some(leaf);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    return None;
                }
                let gs: Vec<&(String, String, V)> = self.p.defs[i].generics.iter().filter(|g| g.0 != "life").collect();
                if gs.len() != args.len() {
                    return None;
                }
                gs.iter().zip(args).filter(|(g, _)| g.0 == "type").find_map(|(_, a)| self.unsatisfied(self.deep(a).strip(), tr))
            }
            // 型パラメータは、いま検査している定義の境界が `tr` を (スーパートレイトまで辿って) 含むときだけ満たす。
            // 境界に無ければ原因になる (`fn f[T](a: T) { g(a) }` で `g` が `T: Eq` を要るとき、E2030)。
            Ty::Param(pn) => (param_bound_implies(self.p, self.def(), pn, tr) == Some(false)).then(|| t.clone()),
            // 基本型が利用者のトレイトを満たすのは、その `impl` があるとき (または全型への `impl blanket`) だけ。
            // 標準のトレイト (`Clone` `Display` など) の組み込みの実装は表に無いので、ここでは見ない。
            Ty::Prim(p) if !(tr.starts_with("core.") || tr.starts_with("alloc.") || tr.starts_with("std.")) => {
                let has = self.p.impls.contains(&(tr.to_string(), p.clone())) || self.p.impls.contains(&(tr.to_string(), "blanket".to_string()));
                (!has).then(|| t.clone())
            }
            Ty::Prim(p) if p == "f32" || p == "f64" => {
                matches!(tr, "core.cmp.Eq" | "core.cmp.Ord" | "core.hash.Hash").then(|| t.clone())
            }
            Ty::Tuple(ts) => ts.iter().find_map(|x| self.unsatisfied(self.deep(x).strip(), tr)),
            Ty::Array(x, _) | Ty::Slice(x) => self.unsatisfied(self.deep(x).strip(), tr),
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
    /// `if` / `while` の条件は `bool` でなければならない (型が決まらないときは黙る)。
    fn check_cond(&mut self, c: &V, what: &str, line: usize) {
        let t = self.infer(c);
        if self.unify2(&Ty::Prim("bool".into()), &t).is_err() {
            let got = self.deep(&t).show();
            self.diag("E2020", format!("type mismatch: the condition of {} must be `bool`, found {}", what, got), line);
        }
    }

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
                    // 途中の文の `else` の無い `if` は、値を捨てる文として置かれる (dev/plan の KIR は捨てる値を下ろす)。
                    if i + 1 < stmts.len() && nname(s) == "ExprStmt" && nname(&nf(s)[0]) == "If" && is_nil(&nf(&nf(s)[0])[2]) {
                        self.discard_if = Some(crate::parser::nid(&nf(s)[0]));
                    }
                    let t = self.stmt(s);
                    // 途中の式文のブロック・`unsafe`・`with` の値は捨てる (4.6, E2042)。
                    // 途中の式文はすべて対象 (呼び出し・メソッド・`if`/`else`・`match` を含む)。`If` の else 無しは下の `If` 側で報告済み。
                    if i + 1 < stmts.len() && nname(s) == "ExprStmt" && !(nname(&nf(s)[0]) == "If" && is_nil(&nf(&nf(s)[0])[2])) {
                    }
                    if i + 1 == stmts.len() && nname(s) == "ExprStmt" {
                        last = t;
                    }
                }
                if let Some(l) = stmts.last().filter(|s| nname(s) == "ExprStmt") {
                    if last != Ty::Never && self.static_expr(&nf(l)[0]) {
                        self.static_nodes.insert(crate::parser::nid(e));
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
                    // 接尾辞のない整数 (D12: 型が見た目で分かるよう、接尾辞を必須にする) は "int?" で印を付ける。
                    self.lits.push((if matches!(&f[2], V::S(_)) { "int" } else { "int?" }, st(&f[1]), t.clone(), nline(e)));
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
                "str" => Ty::Ref(false, Box::new(Ty::Prim("str".into())), crate::types::Region::Unk),
                "bstr" => Ty::Ref(false, Box::new(Ty::Slice(Box::new(Ty::Prim("u8".into())))), crate::types::Region::Unk),
                "unit" => Ty::Unit,
                _ => Ty::Unknown,
            },
            "Path" => {
                self.path_pos = nline(e);
                let t = self.path(&strs(&f[0]));
                self.path_pos = 0;
                t
            }
            "Call" => self.call(crate::parser::nid(e), &f[0], items(&f[1]), nline(e), items(&f[2])),
            "MCall" => {
                let recv = self.infer(&f[0]);
                self.log_recv(f, 4, false, &st(&f[1]), &recv);
                let args: Vec<Ty> = items(&f[2]).iter().map(|a| self.infer(a)).collect();
                self.side.arg_ids = items(&f[2]).iter().map(crate::parser::nid).collect();
                let base = self.base_local(&f[0]);
                self.arg_static = items(&f[2]).iter().map(|a| self.static_expr(a)).collect();
                if let Some(V::N(p)) = f.get(4) {
                    if let Ok(pos) = p.parse::<usize>() {
                        self.call_sites.push(CallSite { writer: self.cur, pos, seg: usize::MAX, call: crate::parser::nid(e) });
                    }
                }
                self.call_method(crate::parser::nid(e), crate::parser::nid(&f[0]), &recv, base.as_deref(), &st(&f[1]), &args, nline(e), items(&f[3]), false)
            }
            "Field" => {
                let r = self.infer(&f[0]);
                self.log_recv(f, 2, true, &st(&f[1]), &r);
                self.field_ty(&r, &st(&f[1]))
            }
            "Try" => {
                let t = self.infer(&f[0]);
                let inner = match self.deep(&t).strip() {
                    Ty::Named(n, a, _) if n == "Result" || n == "Option" => a.first().cloned().unwrap_or(Ty::Unknown),
                    _ => Ty::Unknown,
                };
                // 4.1: `?` はエラー型が完全一致するときだけ使える。
                self.check_try_err(&t, nline(e), crate::parser::nid(e));
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
                    {
                        let (_, got) = self.prim_conflict(&err).unwrap_or_else(|| Self::raw_pair(&err));
                        self.diag("E2025", format!("an index must be `usize`, found `{}`", got), nline(e));
                    }
                }
                match self.deep(&t).strip() {
                    Ty::Array(x, _) | Ty::Slice(x) => (**x).clone(),
                    Ty::Named(n, a, _) if n == "alloc.vec.Vec" => a.first().cloned().unwrap_or(Ty::Unknown),
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
                // 単項 `-` は符号付き整数・浮動小数だけ、`!` は `bool` と整数だけ (オペランドの型が決まっていて外れるものは型の誤り)。
                if matches!(st(&f[0]).as_str(), "-" | "!") {
                    let d0 = self.deep(&t);
                    if matches!(&d0, Ty::Ref(_, i, _) if matches!(&**i, Ty::Prim(p) if p != "str")) {
                        self.diag("E2024", format!("operand of unary `{}` is `{}`: a reference is not dereferenced implicitly: write `*c` (an operator does not dereference, 4.2)", st(&f[0]), d0.show()), nline(e));
                    }
                    let d = d0.strip().clone();
                    let neg = st(&f[0]) == "-";
                    let bad = match &d {
                        Ty::Prim(p) => {
                            if neg {
                                matches!(p.as_str(), "bool" | "char" | "str")
                            } else {
                                matches!(p.as_str(), "char" | "str" | "f32" | "f64")
                            }
                        }
                        Ty::Unit | Ty::Tuple(_) | Ty::Array(..) | Ty::Slice(_) | Ty::Ptr(..) => true,
                        Ty::Named(..) => true,
                        _ => false,
                    };
                    if bad {
                        self.diag("E2020", format!("type mismatch: unary `{}` cannot be applied to {}", st(&f[0]), d.show()), nline(e));
                    }
                }
                match st(&f[0]).as_str() {
                    "&" => {
                        let rg = self.borrow_region(e);
                        Ty::Ref(false, Box::new(t), rg)
                    }
                    "&mut" => {
                        self.need_mut_place(&f[1], "borrow", nline(e));
                        // `&mut r.f` の根 `r` が共有参照なら、共有から可変を得ることになる (E3031。`&mut self` のメソッド呼び出しと同じ)。
                        let proj = match nname(&f[1]) {
                            "Field" => true,
                            "Path" => strs(&nf(&f[1])[0]).len() >= 2,
                            _ => false,
                        };
                        if proj {
                            if let Some(root) = self.base_local(&f[1]) {
                                if let Some(rt) = self.lookup(&root) {
                                    if matches!(self.resolve(&rt), Ty::Ref(false, _, _)) {
                                        self.diag(
                                            "E3031",
                                            format!("cannot borrow a field of `{}` as mutable through the shared reference", root),
                                            nline(e),
                                        );
                                    }
                                }
                            }
                        }
                        // `&mut x` で x の値を後から差し替えられるので、x の `'static` の印を外す。
                        if nname(&f[1]) == "Path" {
                            let segs = strs(&nf(&f[1])[0]);
                            if segs.len() == 1 {
                                self.set_static(&segs[0], false);
                            }
                        }
                        let rg = self.borrow_region(e);
                        Ty::Ref(true, Box::new(t), rg)
                    }
                    // 参照は透過的なので、参照でない型の `*` も同じ型 (型が決まらないときは Unknown)。
                    "*" => match self.resolve(&t) {
                        Ty::Ref(_, x, _) => *x,
                        Ty::Unknown => Ty::Unknown,
                        // 参照でも生ポインタでもない値の `*` は型の誤り (パターンで得た名前の段数が未確定のときは Unknown になるので、ここへは来ない)。
                        other @ (Ty::Prim(_) | Ty::Unit | Ty::Tuple(_) | Ty::Array(..) | Ty::Slice(_)) => {
                            self.diag("E2020", format!("type mismatch: cannot dereference a value of type {}: `*` applies only to a reference or a raw pointer", other.show()), nline(e));
                            other
                        }
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
                    for (x, ex) in [(&a, &f[1]), (&b, &f[2])] {
                        if self.unify2(&Ty::Prim("bool".into()), x).is_err() {
                            let got = self.deep(x).show();
                            self.diag("E2020", format!("type mismatch: an operand of `{}` must be `bool`, found {}", op, got), nline(ex));
                        }
                    }
                    return Ty::Prim("bool".into());
                }
                // 演算子の被演算子は暗黙に参照を外さない (4.2: 自動参照外しはメソッドのレシーバだけ)。unify の前に、解決済みの型で見る。
                self.operator_ref_operands(&a, &b, &op, nline(e));
                if let Err(err) = self.unify2(&a, &b) {
                    {
                        let (l, r) = self.prim_conflict(&err).unwrap_or_else(|| Self::raw_pair(&err));
                        let hint = conv_hint(&l, &r);
                        self.diag("E2024", format!("operands of `{}` have different types: `{}` and `{}`{}", op, l, r, hint), nline(e));
                    }
                }
                // 算術 `+ - * / %` は整数・浮動小数だけ (`true + false`、`'a' * 2` は型の誤り)。
                // `%` は整数だけ (浮動小数は `rem` メソッド)、`& | ^` は整数と `bool` だけ (4.1)。
                if matches!(op.as_str(), "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^") {
                    if let Ty::Prim(p) = self.deep(&a).strip() {
                        let p = p.as_str();
                        let bad = match op.as_str() {
                            "%" => matches!(p, "bool" | "char" | "str" | "f32" | "f64"),
                            "&" | "|" | "^" => matches!(p, "char" | "str" | "f32" | "f64"),
                            _ => matches!(p, "bool" | "char"),
                        };
                        if bad {
                            self.diag("E2020", format!("type mismatch: operator `{}` cannot be applied to `{}`", op, p), nline(e));
                        }
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
                    return if self.p.types.contains_key(&fq) { Ty::Named(fq, vec![], crate::types::Rgs::default()) } else { Ty::Unknown };
                };
                // ジェネリクスの型パラメータを新しい型変数にして、フィールドの宣言型と値の型を単一化する。
                // 決まった型引数はリテラル式の型になり、以降のフィールド・メソッド検査に流れる。
                let order: Vec<&String> = td.generics.iter().filter(|g| g.0 == "type").map(|g| &g.1).collect();
                let args: Vec<Ty> = order.iter().map(|_| self.fresh(0)).collect();
                let gn = td.gnames();
                let bounded: HashSet<String> = td
                    .generics
                    .iter()
                    .filter(|g| g.0 == "type" && items(&g.2).iter().any(|b| matches!(b, V::S(x) if x == "'static")))
                    .map(|g| g.1.clone())
                    .collect();
                let decl_fields: Vec<(String, V)> = td.fields.iter().map(|fl| (fl.0.clone(), fl.1.clone())).collect();
                for x in items(&f[1]) {
                    let vt = self.infer(&items(x)[1]);
                    // `type T[X: 'static]` の欄へ入れる値は、`'static` の境界の位置と同じ判定 (5.5)。
                    if !bounded.is_empty() {
                        let fname = st(&items(x)[0]);
                        if let Some((_, fv)) = decl_fields.iter().find(|(n, _)| *n == fname) {
                            if mentions_param(&from_v(fv, &gn), &bounded) {
                                let origin = self.static_expr(&items(x)[1]);
                                self.static_checks.push((vt.clone(), origin, nline(e), format!("the field `{}` of `{}` (a `'static` bound)", fname, fq)));
                            }
                        }
                    }
                    let declared = self.field_ty(&Ty::Named(fq.clone(), args.clone(), crate::types::Rgs::default()), &st(&items(x)[0]));
                    self.note_conv(&items(x)[1], &declared, &vt);
                    let _ = self.unify2(&declared, &vt);
                    // 構造体の欄は「受け側」の関数型が分かる: 宣言 × 宣言の効果の食い違いをここで報告する。
                    self.check_fn_conv_dir(&declared, &vt);
                }
                // 型引数はここでは escape しない。あとから注釈や呼び出しで決まるかもしれない
                // (決まらなかったリテラルは finish_literals が E2027 にする)。
                Ty::Named(fq, args, crate::types::Rgs::default())
            }
            "TupleE" => {
                let ts: Vec<Ty> = items(&f[0]).iter().map(|x| self.infer(x)).collect();
                if ts.is_empty() { Ty::Unit } else { Ty::Tuple(ts) }
            }
            "ArrayE" => {
                let ts: Vec<Ty> = items(&f[0]).iter().map(|x| self.infer(x)).collect();
                let elem = self.fresh(0);
                for t in ts.iter() {
                    // 要素の型はすべて同じでなければならない (`[1u32, 2u8]` は誤り。E2023 は TAST の規則 T)。
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
                Ty::Array(Box::new(elem), Some(ts.len() as u128))
            }
            "If" => {
                self.check_cond(&f[0], "`if`", nline(e));
                let mut a = self.infer(&f[1]);
                let has_else = !is_nil(&f[2]);
                if !has_else {
                    // `else` を省いた `if` の型は `()` (仕様: 暗黙の空の `else` と同じ)。値を使う位置では then 枝も `()` か発散でなければならない。
                    // 途中の文として置かれた `if` は、then 枝の値を捨てる (4.6)。`()` と `!` 以外は E2042
                    // (`if c { x }` の `x` が黙って捨てられ、`return x` の書き違いが通るのを防ぐ)。
                    let discarded = self.discard_if == Some(crate::parser::nid(e));
                    if a != Ty::Never && self.unify2(&Ty::Unit, &a).is_err() {
                        let got = self.deep(&a).show();
                        if discarded {
                            self.diag("E2042", format!("a value other than () is discarded: the `then` branch of this `if` without `else` has type {}; use `return` or `core.mem.drop(x)`", got), nline(e));
                        } else {
                            self.diag("E2020", format!("type mismatch in `if` without `else`: expected (), found {}", got), nline(e));
                        }
                    }
                    return Ty::Unit;
                }
                let b = self.infer(&f[2]);
                if a == Ty::Never {
                    return b;
                }
                // 6 の 2: 両方の枝は同じ型になる。`Ok(..)` と `Err(..)` の枝の型変数は、互いに一致させて決まる。
                let (sa, sb) = (self.static_expr(&f[1]), self.static_expr(&f[2]));
                if has_else && b != Ty::Never {
                    a = self.join_arm(&a, &b, nline(e), "`if` branches");
                }
                if sa && sb {
                    self.static_nodes.insert(crate::parser::nid(e));
                }
                a
            }
            "Match" => {
                let s = self.infer(&f[0]);
                let src = self.last_site;
                let scrut_static = self.static_expr(&f[0]);
                let mut arms_static = true;
                let mut out: Option<Ty> = None;
                for arm in items(&f[1]) {
                    self.scopes.push(HashMap::new());
                    let saved_exact = self.pat_exact;
                    self.pat_exact = true;
                    self.bind_pat_from(&items(arm)[0], &s, src, nline(&items(arm)[0]));
                    self.pat_exact = saved_exact;
                    self.note_pat_binds(&items(arm)[0], nline(&items(arm)[0]));
                    // 走査の対象が `'static` の値だけを持つなら、分解で得た名前も `'static`。
                    if scrut_static {
                        let mut bs = vec![];
                        pat_binds(&items(arm)[0], &mut bs);
                        for b in bs {
                            self.set_static(&b, true);
                        }
                    }
                    let t = self.infer(&items(arm)[1]);
                    if t != Ty::Never && !self.static_expr(&items(arm)[1]) {
                        arms_static = false;
                    }
                    self.scopes.pop();
                    if t == Ty::Never {
                        continue;
                    }
                    // 合流の型は、`!` でない腕の型を順に単一化した結果 (`!` の腕は型を決めない)。
                    out = Some(match out {
                        None => t,
                        // 6 の 2: 他の腕と型を一致させる (`Some(v) => Ok(v)` と `None => Err(a)` など)。
                        Some(o) => self.join_arm(&o, &t, nline(e), "`match` arms"),
                    });
                }
                if arms_static && out.is_some() {
                    self.static_nodes.insert(crate::parser::nid(e));
                }
                // 腕がすべて発散するなら、match 自身も発散する。
                match out {
                    Some(o) => o,
                    None if !items(&f[1]).is_empty() => Ty::Never,
                    None => Ty::Unknown,
                }
            }
            "For" => {
                let it = self.infer(&f[1]);
                let src = self.last_site;
                self.scopes.push(HashMap::new());
                let mut elem = self.iter_elem(&it);
                // 参照を回す (`for x in &v` / `&mut v` / スライスの参照) と、要素は `&要素` / `&mut 要素` (Rust と同じ)。
                if let Ty::Ref(m, inner, _) = self.deep(&it) {
                    if matches!(self.deep(&inner).strip(), Ty::Array(..) | Ty::Slice(_)) || matches!(self.deep(&inner).strip(), Ty::Named(n, _, _) if n == "alloc.vec.Vec") {
                        elem = Ty::Ref(m, Box::new(elem), crate::types::Region::Unk);
                    }
                }
                self.for_next_effects(&it, nline(e));
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
                // 取り込んだ値の型も覚える (spawn の `Send` 検査 (5.5) は、推論の終わりに型を決めてから見る)。
                let mut caps: Vec<(String, String, Ty)> = vec![];
                let mut statics: HashSet<String> = HashSet::new();
                let mut cap_ref_new: Vec<String> = vec![];
                for c in items(&f[2]) {
                    let (Some(V::S(k)), Some(V::S(n))) = (items(c).first(), items(c).get(1)) else { continue };
                    let Some(t) = self.lookup(n) else { continue };
                    caps.push((k.clone(), n.clone(), t.clone()));
                    if k == "&mut" {
                        self.set_static(n, false);
                    }
                    if k == "move" {
                        if self.lookup_static(n) {
                            statics.insert(n.clone());
                        }
                        let was_static = self.lookup_static(n);
                        if let Some(s) = self.scopes.last_mut() {
                            s.insert(n.clone(), t.clone());
                            if was_static {
                                s.insert(static_mark(n), Ty::Unit);
                            }
                        }
                        continue;
                    }
                    // 仕様 4.3: `&a` / `&mut a` の取り込みは、Copy な型でも本体で `&T` / `&mut T` (値として使うには `*a`)。
                    let r = Ty::Ref(k == "&mut", Box::new(t), Region::Var(crate::parser::nid(e)));
                    cap_ref_new.push(n.clone());
                    if let Some(s) = self.scopes.last_mut() {
                        s.insert(n.clone(), r);
                    }
                }

                let cap_ref_base = self.cap_refs.len();
                self.cap_refs.extend(cap_ref_new);
                self.closure_caps.insert(node.clone(), caps);
                self.closure_static.insert(node.clone(), statics);
                for p in items(&f[0]) {
                    let ty = from_v(&items(p)[1], &gn);
                    self.bind(&st(&items(p)[0]), ty.clone());
                    args.push(ty);
                }
                let ret = from_v(&f[1], &gn);
                // クロージャの中の `?` は、クロージャの戻り値型のエラー型と比べる。
                self.rets.push(ret.clone());
                let body = self.infer(&f[3]);
                self.cap_refs.truncate(cap_ref_base);
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
                    {
                        let (want, got) = self.ann_conflict(&ret, &body, &err).unwrap_or_else(|| Self::raw_pair(&err));
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
                        self.note_conv(&f[0], &ret, &v);
                        let ret_d = self.deep(&ret);
                        self.note_region_cstr(crate::parser::nid(&f[0]), &ret_d, &v, "ret");
                        if let Err(err) = self.unify2(&ret, &v) {
                            {
                                let (want, got) = self.ann_conflict(&ret, &v, &err).unwrap_or_else(|| Self::raw_pair(&err));
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
                // 値の無い `return` は、戻り値型が `()` の関数・クロージャだけ。
                if nname(e) == "Return" && is_nil(&f[0]) {
                    if let Some(ret) = self.rets.last().cloned() {
                        if self.unify2(&ret, &Ty::Unit).is_err() {
                            let want = self.deep(&ret).show();
                            self.diag("E2020", format!("type mismatch in return value: expected {}, found (): `return` without a value in a function that returns {}", want, want), nline(e));
                        }
                    }
                }
                Ty::Never
            }
            "While" => {
                self.check_cond(&f[0], "`while`", nline(e));
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
                    self.let_pending.push((nline(e), st(&nf(&f[0])[0]), crate::parser::nid(&f[0]), t));
                }
                let body = self.infer(&f[4]);
                // 本体の値の型 (借用を持ち得ない型なら、借用検査は値の借用を追わない)。
                self.let_pending.push((nline(e), "$with_val".to_string(), crate::parser::nid(e), body.clone()));
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
                // 整数リテラルの長さだけは `usize` の幅で範囲を検査する (4.5。wasm32 は 32 ビット)。
                if let Some(msg) = array_len_problem(&f[1]) {
                    self.diag("E2026", msg, nline(e));
                }
                // 長さが名前なら、定数 (`const`) か定数ジェネリックのパラメータでなければならない (局所変数は不可)。
                if let V::L(xs) = &f[1] {
                    if xs.len() == 2 {
                        let nm = st(&xs[1]);
                        let segs: Vec<String> = nm.split('.').map(String::from).collect();
                        let is_const_param = segs.len() == 1 && self.def().generics.iter().any(|g| g.0 == "const" && g.1 == nm);
                        let is_const = self.lookup(&segs[0]).is_none()
                            && matches!(self.p.resolve(&segs, &self.def().file), Some(r) if r.rest.is_empty() && matches!(r.target, Target::Const(_)));
                        if !is_const_param && !is_const {
                            self.diag("E2026", format!("the length of `[e; N]` must be an integer literal or a `const` name, but `{}` is not a constant", nm), nline(e));
                        }
                    }
                }
                Ty::Array(Box::new(t), array_len(&f[1]))
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
                // `let` は反駁不能パターンだけ (仕様 4.1)。外れうるパターンは `match` で書かせる。
                if self.let_refutable(&f[0]) {
                    self.diag("E2021", "refutable pattern in `let`: a `let` takes only irrefutable patterns; use `match`".into(), nline(s));
                }
                let rhs = self.infer(&f[3]);
                let init_static = self.static_expr(&f[3]);
                let src = self.last_site;
                let gn = self.def().gnames();
                // D17: 注釈を外しても同じ型に決まるか。右辺だけで型が閉じているかを、単一化の前に見る
                // (後の文で決まる `Vec.new()` などは閉じていない)。
                let rhs_closed = !self.has_open(&rhs);
                let ty = if !is_nil(&f[2]) {
                    let ann = from_v(&f[2], &gn);
                    self.note_conv(&f[3], &ann, &rhs);
                    let ann_d = self.deep(&ann);
                    self.note_region_cstr(crate::parser::nid(&f[3]), &ann_d, &rhs, "let");
                    // 寿命の付いた注釈 (`&'a T`・`&'static T`) は、型の木に無い情報を運ぶので冗長ではない。
                    let life = f.len() > 4;
                    if self.redundant_on && rhs_closed && !life && !self.coerces(&ann) && self.deep(&ann) == self.deep(&rhs) {
                        let shown = self.deep(&ann).show();
                        self.diag(
                            "E2053",
                            format!("redundant type annotation `{}`: the initializer already has this type; remove the annotation", shown),
                            nline(s),
                        );
                    }
                    if let Err(err) = self.unify2(&ann, &rhs) {
                        {
                            let (want, got) = self.ann_conflict(&ann, &rhs, &err).unwrap_or_else(|| Self::raw_pair(&err));
                            self.diag("E2020", format!("type mismatch in let binding: expected {}, found {}{}", want, got, conv_hint(&want, &got)), nline(s));
                        }
                    }
                    // let の注釈は「受け側」の関数型が分かる: 宣言 × 宣言の効果の食い違いをここで報告する。
                    self.check_fn_conv_dir(&ann, &rhs);
                    if let Some((want, got)) = self.ref_level_conflict(&ann, &rhs) {
                        self.diag("E2020", format!("type mismatch in let binding: expected {}, found {}{}", want, got, self.deref_hint(&ann)), nline(s));
                    } else if self.value_typed_expr(&f[3]) {
                        // 逆向き: 値を参照の注釈へ (`let r: &u64 = 5u64`)。
                        if let Some((want, got)) = self.value_for_ref_conflict(&ann, &rhs) {
                            self.diag("E2020", format!("type mismatch in let binding: expected {}, found {}; take a reference with `&`", want, got), nline(s));
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
                let saved_exact = self.pat_exact;
                self.pat_exact = true;
                self.bind_pat_from(&f[0], &ty, src, nline(s));
                self.pat_exact = saved_exact;
                if init_static && nname(&f[0]) == "PBind" {
                    self.set_static(&st(&nf(&f[0])[0]), true);
                }
                let mut names = vec![];
                crate::check::pat_bind_nodes(&f[0], &mut names);
                for (n, id) in names {
                    if let Some(t) = self.lookup(&n) {
                        self.let_pending.push((nline(s), n, id, t));
                    }
                }
                Ty::Unit
            }
            "Assign" => {
                let target = self.infer(&f[0]);
                let value = self.infer(&f[1]);
                self.note_conv(&f[1], &target, &value);
                let target_d = self.deep(&target);
                self.note_region_cstr(crate::parser::nid(&f[1]), &target_d, &value, "assign");
                // 名前・フィールドへの代入は別の箇所が E3001 にする。添字の代入 `a[i] = ..` だけここで見る。
                match nname(&f[0]) {
                    "Index" => self.need_mut_place(&f[0], "assign to", nline(&f[0])),
                    // 共有参照 `&T` の指す先の欄への代入 (`self.n = 1` を `self: &T` で) と、`*r = ..` を `r: &T` で。
                    "Field" | "Path" | "Unary" => {
                        let (root, via_deref) = match nname(&f[0]) {
                            "Unary" if st(&nf(&f[0])[0]) == "*" && nname(&nf(&f[0])[1]) == "Path" => {
                                (Some(strs(&nf(&nf(&f[0])[1])[0])), true)
                            }
                            "Unary" => {
                                // `*e = ..` の被演算子が名前以外の式 (呼び出し・メソッド呼び出し・欄・添字):
                                // 式の型が共有の参照 `&T` なら E3031 (`*c.get() = 5` を `get -> &u64` で)。
                                if st(&nf(&f[0])[0]) == "*" {
                                    let id = crate::parser::nid(&nf(&f[0])[1]);
                                    let ot = self.expr_pending.get(&id).map(|x| x.2.clone());
                                    if let Some(ot) = ot {
                                        if matches!(self.resolve(&ot), Ty::Ref(false, _, _)) {
                                            self.diag(
                                                "E3031",
                                                "cannot assign through a shared reference returned by an expression".to_string(),
                                                nline(&f[0]),
                                            );
                                        }
                                    }
                                }
                                (None, false)
                            }
                            _ => (self.base_local_segs(&f[0]), false),
                        };
                        if let Some(segs) = root {
                            let n = &segs[0];
                            let direct = segs.len() == 1;
                            if self.lookup(n).is_some() && (via_deref || !direct) {
                                if let Some(t) = self.lookup(n) {
                                    if matches!(self.resolve(&t), Ty::Ref(false, _, _)) {
                                        self.diag(
                                            "E3031",
                                            format!("cannot assign through the shared reference `{}`", n),
                                            nline(&f[0]),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
                // 再代入された名前は、以後 `'static` と分かる印を外す (分岐の中の代入でも残さない)。
                if nname(&f[0]) == "Path" {
                    let segs = strs(&nf(&f[0])[0]);
                    if segs.len() == 1 {
                        self.set_static(&segs[0], false);
                    }
                }
                // 代入は `let` と同じ経路で報告する (変数・フィールド・添字のどれも)。
                if let Err(e) = self.unify2(&target, &value) {
                    {
                        let (want, got) = self.prim_conflict(&e).unwrap_or_else(|| Self::raw_pair(&e));
                        self.diag("E2024", format!("assignment: expected `{}`, found `{}`", want, got), nline(&f[1]));
                    }
                }
                if nname(&f[0]) == "Path" && strs(&nf(&f[0])[0]).len() == 1 {
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
    fn note_pat_binds(&mut self, pat: &V, line: usize) {
        let mut names = vec![];
        crate::check::pat_bind_nodes(pat, &mut names);
        for (n, id) in names {
            if let Some(t) = self.lookup(&n) {
                self.let_pending.push((line, n, id, t));
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
        // 分解パターン (名前を取り出す形) が参照を受けたら、参照を外して中を照合し、取り出した名前は欄の型への参照になる
        // (Rust の既定の束縛モード): `&T` は `&欄の型`、`&mut T` は `&mut 欄の型`。入れ子の参照は共有が勝つ。
        if matches!(nname(pat), "PTuple" | "PStruct" | "PVariant") {
            let full = self.deep(ty);
            if matches!(full, Ty::Ref(..)) {
                let (mut all_mut, mut cur) = (true, &full);
                while let Ty::Ref(m, inner, _) = cur {
                    all_mut &= *m;
                    cur = inner;
                }
                let base = cur.clone();
                self.note_pat_ty(pat, &full, None);
                let saved = self.pat_mode;
                self.pat_mode = Some(saved.unwrap_or(true) && all_mut);
                self.bind_pat_inner(pat, &base);
                self.pat_mode = saved;
                return;
            }
        }
        let bind = if nname(pat) == "PBind" {
            let name = st(&nf(pat)[0]);
            let mode = match self.pat_mode {
                Some(true) => crate::tast::BindMode::RefMut,
                Some(false) => crate::tast::BindMode::Ref,
                None => crate::tast::BindMode::Move,
            };
            let bty = match self.pat_mode {
                Some(m) => Ty::Ref(m, Box::new(ty.clone()), crate::types::Region::Unk),
                None => ty.clone(),
            };
            Some((name, bty, mode))
        } else {
            None
        };
        self.note_pat_ty(pat, ty, bind);
        self.bind_pat_inner(pat, ty)
    }

    /// パターン節点の型と束縛の形を `side.pat_tys` に記す (TAST の側の表、担当 D)。
    /// 同じ節点が違う型で 2 回推論されたら `Unknown` (`let_tys` と同じ扱い)。型変数は `check_def` の終わりに解決する。
    fn note_pat_ty(&mut self, pat: &V, ty: &Ty, bind: Option<(String, Ty, crate::tast::BindMode)>) {
        let id = crate::parser::nid(pat);
        if id == 0 {
            return;
        }
        let key = (self.cur, id);
        let mut info = crate::tast::PatInfo { ty: self.deep(ty), bind: bind.map(|(n, t, m)| (n, self.deep(&t), m)) };
        if let Some(old) = self.side.pat_tys.get(&key) {
            let mut old = old.clone();
            old.ty = self.deep(&old.ty);
            if let Some(b) = old.bind.as_mut() {
                b.1 = self.deep(&b.1);
            }
            if old != info {
                info.ty = Ty::Unknown;
                if let Some(b) = info.bind.as_mut() {
                    b.1 = Ty::Unknown;
                }
            }
        }
        self.side.pat_tys.insert(key, info);
    }

    fn bind_pat_inner(&mut self, pat: &V, ty: &Ty) {
        let f = nf(pat);
        match nname(pat) {
            "PBind" => self.bind(&st(&f[0]), ty.clone()),
            // `&p` パターン: 参照を一段外した型で中身を束縛する。
            "PRef" => {
                let inner = match self.resolve(ty) {
                    Ty::Ref(_, x, _) => *x,
                    other => other,
                };
                let saved = self.pat_mode.take();
                self.bind_pat(&f[0], &inner);
                self.pat_mode = saved;
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
                            let t = Ty::Named(td.name.clone(), args, crate::types::Rgs::default());
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
            // 欄のない列挙子 (`None`、`t.E.B`) や定数のパターン: 列挙子は照合する値の列挙型に属し、欄の数が 0 でなければならない。
            "PPath" => {
                let segs = strs(&f[0]);
                self.shape_for_variant(&segs, ty);
            }
            // リテラルのパターン (種類・接尾辞が値の型と合うか、の E2023) は TAST の規則 T が見る。
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
            Some(Ty::Named("Option".into(), vec![v], crate::types::Rgs::default()))
        } else if segs.len() == 1 && matches!(name.as_str(), "Ok" | "Err") {
            let (a, b) = (self.fresh(0), self.fresh(0));
            Some(Ty::Named("Result".into(), vec![a, b], crate::types::Rgs::default()))
        } else if let Some(r) = self.p.resolve(segs, &self.def().file) {
            match r.target {
                Target::Variant(en, _) if self.p.types.contains_key(&en) => {
                    let n = self.type_def(&en).map_or(0, |td| td.generics.iter().filter(|g| g.0 == "type").count());
                    let args: Vec<Ty> = (0..n).map(|_| self.fresh(0)).collect();
                    Some(Ty::Named(en, args, crate::types::Rgs::default()))
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(w) = want {
            // 列挙子は、照合する値の列挙型に属さなければならない (`Result` の値を `Some(..)` で照合するのは誤り)。
            // 値の型が具体的に決まっているときだけ比べる (参照は外して見る)。
            if let Ty::Named(en, _, _) = &w {
                let scrut = self.deep(ty);
                let mismatch = match scrut.strip() {
                    Ty::Named(n, _, _) => n != en,
                    Ty::Prim(_) | Ty::Tuple(_) | Ty::Array(..) | Ty::Slice(_) | Ty::Unit => true,
                    _ => false,
                };
                if mismatch {
                    // E2023 は TAST の規則 T が報告する。
                    return;
                }
            }
            let _ = self.unify2(&w, ty);
        }
    }

    fn variant_payload(&self, segs: &[String], ty: &Ty) -> Vec<Ty> {
        let name = segs.last().cloned().unwrap_or_default();
        let t = self.deep(ty);
        let args = match t.strip() {
            Ty::Named(_, a, _) => a.clone(),
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
                        Ty::Named("Option".into(), vec![v], crate::types::Rgs::default())
                    } else if self.p.types.contains_key(&en) {
                        self.variant_ctor(&en, &vn, &[], 0)
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
        for g in d.generics.iter().filter(|g| g.0 == "type") {
            for b in items(&g.2) {
                let V::L(bx) = b else { continue };
                let tr = st(&bx[0]);
                if tr == "core.marker.Send" || tr == "core.marker.Sync" {
                    if let Some(t) = map.get(&g.1) {
                        self.fn_value_markers.push((tr, false, t.clone(), self.cur_line));
                    }
                }
            }
        }
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
    fn variant_ctor(&mut self, en: &str, vn: &str, argt: &[Ty], line: usize) -> Ty {
        let arg_ids = std::mem::take(&mut self.side.arg_ids);
        let Some(td) = self.type_def(en) else { return Ty::Named(en.to_string(), vec![], crate::types::Rgs::default()) };
        let gn = td.gnames();
        let order: Vec<&String> = td.generics.iter().filter(|g| g.0 == "type").map(|g| &g.1).collect();
        let targs: Vec<Ty> = order.iter().map(|_| self.fresh(0)).collect();
        if let Some((_, payload)) = td.variants.iter().find(|v| v.0 == vn) {
            let mut bind = HashMap::new();
            for (n, a) in order.iter().zip(&targs) {
                bind.insert((*n).clone(), a.clone());
            }
            for (k, (pt, a)) in payload.iter().zip(argt).enumerate() {
                let raw = from_v(pt, &gn);
                let declared = subst(&raw, &bind);
                if arg_ids.len() == argt.len() {
                    self.side.conv_pending.push(((self.cur, arg_ids[k]), declared.clone(), a.clone()));
                }
                // 列挙子のペイロードの食い違いは、関数の戻り値と同じく E2020 にする (黙ると `E.A(5u8)` を通してしまう)。
                if let Err(err) = self.unify2(&declared, a) {
                    {
                        let (want, got) = self.ann_conflict(&declared, a, &err).unwrap_or_else(|| Self::raw_pair(&err));
                        self.diag("E2020", format!("type mismatch in payload of `{}`: expected {}, found {}{}", vn, want, got, conv_hint(&want, &got)), line);
                    }
                } else if self.owned_ref_clash(&declared, a) {
                    // 所有型のペイロードに参照を渡す (`A(out)`、`out: &mut String`)。Copy のスカラーの参照は従来どおり通す。
                    let (want, got) = (self.deep(&declared).show(), self.deep(a).show());
                    self.diag("E2020", format!("type mismatch in payload of `{}`: expected {}, found {}; there is no implicit dereference: pass the owned value", vn, want, got), line);
                }
                // enum のペイロードの欄は「受け側」の関数型が分かる: 宣言 × 宣言の効果の食い違いをここで報告する。
                self.check_fn_conv_dir(&declared, a);
            }
        }
        Ty::Named(en.to_string(), targs, crate::types::Rgs::default())
    }

    fn call(&mut self, call_id: u32, callee: &V, args: &[V], line: usize, targs: &[V]) -> Ty {
        let argt: Vec<Ty> = args.iter().map(|a| self.infer(a)).collect();
        self.side.arg_ids = args.iter().map(crate::parser::nid).collect();
        // 構造体リテラルの引数: 従来の検査は型引数を見ないので、呼び出し側でも型引数の矛盾を見る。
        let arg_stat: Vec<bool> = args.iter().map(|a| self.static_expr(a)).collect();
        // その位置にクロージャ式が直接書かれているか (効果検査 ② が変数経由を避けるのに使う)。
        if nname(callee) != "Path" {
            // `fn(..) {..}(x)` のように、クロージャを直接呼ぶ形。
            self.note_callee(call_id, Callee::Local(String::new()));
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
                self.note_callee(call_id, Callee::Local(segs[0].clone()));
                let mut t = self.lookup(&segs[0]).unwrap();
                // `FnMut` のクロージャは可変借用で呼ぶ (4.2)。共有参照 `&f` を一つでも通して呼ぶと、
                // 可変借用を共有参照越しに作ることになる (E3031。メソッドの `&mut self` と同じ)。
                if self.shared_then_fnmut(&t) {
                    self.diag(
                        "E3031",
                        format!("cannot call the `FnMut` closure `{}` through a shared reference: borrow it with `&mut`", segs[0]),
                        line,
                    );
                }
                // 取り込んだ `&f` / `&mut f` の呼び出しは、参照を外して呼ぶ。
                while let Ty::Ref(_, inner, _) = &t {
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
                    // 境界が宣言する効果 (`Fn<.., [io.fs], may>`) は呼び出しの効果 (トレイトの境界のメソッド・
                    // `dyn` の呼び出しと同じ扱い)。効果変数・panic 変数の境界は今の規則のまま (数えない)。
                    if let Ty::Param(pn) = self.deep(&t).strip().clone() {
                        let d = self.def();
                        if let Some((bu, bp)) = self.fn_bound_effects(d, &pn) {
                            let uses: Vec<String> =
                                bu.into_iter().filter(|u| !d.generics.iter().any(|g| g.0 == "uses" && &g.1 == u)).collect();
                            let panics = (bp == Some(true)).then(|| "may".to_string());
                            if !uses.is_empty() || panics.is_some() {
                                self.check_trait_effects(format!("the `{}` bound of `{}`", segs[0], pn), uses, panics, line);
                            }
                        }
                    }
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
            self.arg_static = arg_stat;
            // `x.m(..)` の経路形の呼び先ノードに、受け手の型を載せる (KIR が トレイト側の impl (3.2) を引くため)。
            let cid = crate::parser::nid(callee);
            if cid != 0 && (segs.len() > 2 || !self.p.trait_side.is_empty()) {
                self.expr_pending.insert(cid, ("RecvPath".to_string(), nline(callee), t.clone()));
            }
            let pos = nline(callee);
            if pos != 0 {
                self.call_sites.push(CallSite { writer: self.cur, pos, seg: segs.len() - 1, call: call_id });
            }
            return self.call_method(call_id, call_id, &t, Some(&segs[0]), &segs[segs.len() - 1], &argt, line, targs, true);
        }
        let _ = &argt;
        let Some(r) = self.p.resolve(&segs, &self.def().file) else {
            self.note_callee(call_id, Callee::Unknown);
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
                    let recv = Ty::Named(en.clone(), vec![], crate::types::Rgs::default());
                    let m = r.rest[0].clone();
                    return self.call_method(call_id, call_id, &recv, None, &m, &argt, line, targs, false);
                }
            }
        }
        if !r.rest.is_empty() {
            // `m.Enum.Variant.method(..)` / `m.CONST.method(..)`: 値に対するメソッド呼び出し。
            if r.rest.len() == 1 && matches!(r.target, Target::Variant(..) | Target::Const(_)) {
                let prefix = &segs[..segs.len() - 1];
                let recv = self.path(prefix);
                return self.call_method(call_id, call_id, &recv, None, &r.rest[0], &argt, line, targs, false);
            }
            self.note_callee(call_id, Callee::Unknown);
            argt.iter().for_each(|a| self.escape(a));
            return Ty::Unknown;
        }
        match &r.target {
            Target::Variant(en, vn) => self.note_callee(call_id, Callee::Variant(en.clone(), vn.clone())),
            Target::Fn(i) | Target::Sig(i) => self.note_callee(call_id, Callee::Def(*i)),
            Target::Native(fq) | Target::Value(fq) if self.p.sigs.contains_key(fq) => self.note_callee(call_id, Callee::Def(self.p.sigs[fq])),
            _ => self.note_callee(call_id, Callee::Unknown),
        }
        match r.target {
            Target::Variant(en, vn) => {
                // 列挙子の構築も、宣言のペイロードの数と引数の数が合わなければ E2001 (関数の呼び出しと同じ)。
                let want = match (en.as_str(), vn.as_str()) {
                    ("Option", "Some") | ("Result", "Ok" | "Err") => Some(1),
                    _ => self.type_def(&en).and_then(|td| td.variants.iter().find(|v| v.0 == vn).map(|v| v.1.len())),
                };
                if let Some(want) = want {
                    if want != argt.len() {
                        self.diag("E2001", format!("{} takes {} argument(s) but {} given", vn, want, argt.len()), line);
                    }
                }
                let a0 = argt.first().cloned().unwrap_or(Ty::Unknown);
                match (en.as_str(), vn.as_str()) {
                    ("Option", "Some") => Ty::Named("Option".into(), vec![a0], crate::types::Rgs::default()),
                    ("Result", "Ok") => {
                        let e = self.fresh(0);
                        Ty::Named("Result".into(), vec![a0, e], crate::types::Rgs::default())
                    }
                    ("Result", "Err") => {
                        let t = self.fresh(0);
                        Ty::Named("Result".into(), vec![t, a0], crate::types::Rgs::default())
                    }
                    _ if self.p.types.contains_key(&en) => self.variant_ctor(&en, &vn, &argt, line),
                    _ => Ty::Unknown,
                }
            }
            Target::Native(ref fq) | Target::Value(ref fq) if self.p.sigs.contains_key(fq) => {
                let i = self.p.sigs[fq];
                self.method_form_check(i, &segs, line);
                self.arg_static = arg_stat;
                self.call_def(call_id, i, &argt, line, targs)
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
                self.method_form_check(i, &segs, line);
                self.arg_static = arg_stat;
                self.call_def(call_id, i, &argt, line, targs)
            }
            _ => {
                argt.iter().for_each(|a| self.escape(a));
                Ty::Unknown
            }
        }
    }

    /// 受け手のあるメソッド (第 1 引数が `self`) を `T.m(&x, ..)` / `m.f(x, ..)` の経路の形で呼ぶのは誤り (D8: `x.m(..)` だけ)。
    fn method_form_check(&mut self, i: usize, segs: &[String], line: usize) {
        let d = &self.p.defs[i];
        // 受け手の型から探せる所有者 (型・基本型・`core.slice` など) の関数だけ。モジュールの関数 (`m.f(self, ..)`) は `x.f()` と書けない。
        let owner = d.name.rsplit_once('.').map(|(o, _)| o).unwrap_or("");
        let reachable = self.p.types.contains_key(owner)
            || owner.starts_with("core.num.")
            || matches!(owner, "core.str" | "core.slice" | "core.array" | "core.bool.bool" | "core.char.char" | "core.option.Option" | "core.result.Result" | "alloc.vec.Vec" | "alloc.string.String" | "alloc.boxed.Box");
        // `Box.into_dyn(b)` は期待する型から変換先 (`dyn Tr`) が決まる変換で、メソッドの形では KIR に下ろせない (段 3 では対象外)。
        if d.is_method() && reachable && d.name != "alloc.boxed.Box.into_dyn" {
            let m = segs.last().cloned().unwrap_or_default();
            self.diag("E2029", format!("`{}` takes `self` as its first argument: call it as a method, `x.{}(..)`", segs.join("."), m), line);
        }
    }

    /// 関数・関連関数の呼び出し: 型パラメータを新しい型変数にし、引数と単一化して、戻り値の型を求める。
    ///
    /// `F: core.ops.Fn<Args, R, E, P>` の境界を持つ型パラメータ (fold の `F` など) にはクロージャを渡せる。
    /// 対応する引数の型がクロージャに決まるときは、境界の引数タプル・戻り値型・capture の能力を検査する。
    /// それ以外 (クロージャでない・型変数が解決しない) のときは従来どおり、この呼び出しに渡した
    /// リテラルなどの型変数は決まらなくてもエラーにしない (`escape`)。
    ///
    fn call_def(&mut self, call_id: u32, i: usize, argt: &[Ty], line: usize, targs: &[V]) -> Ty {
        let arg_stat = std::mem::take(&mut self.arg_static);
        let arg_ids = std::mem::take(&mut self.side.arg_ids);
        let d = &self.p.defs[i];
        self.note_static_args(d, 0, argt, &arg_stat, line);
        // 5.5: spawn に渡したクロージャの取り込みは `Send` でなければならない (推論の終わりに見る)。
        if d.name == "std.thread.spawn" || d.name == "std.thread.spawn_deep" {
            if let Some(Ty::Closure { id: Some(node), .. }) = argt.get(1).map(|a| self.resolve(a)) {
                self.spawn_closures.push((node, line));
            }
        }
        // 複数スレッドの実行器 (5.6): タスクの閉包も同じ `Send` の検査を受け、枠の `Send` (E2074) の根になる。
        let task_arg = match d.name.as_str() {
            "std.task.spawn" => Some(0),
            "std.task.block_on_threads" => Some(2),
            _ => None,
        };
        if let Some(k) = task_arg {
            if let Some(Ty::Closure { id: Some(node), .. }) = argt.get(k).map(|a| self.resolve(a)) {
                self.spawn_closures.push((node.clone(), line));
                self.task_roots.push((self.cur, node));
            }
        }
        self.add_edge(d.name.clone());
        // 効果 (4.3 / 5.1): 宣言どおりの効果は従来の検査 (check.rs の visit) が見るので、ここでは
        // `Fn` 境界に渡したクロージャから推論した部分だけを検査の義務にする。クロージャの中では
        // 実体化した効果全体をそのクロージャの効果に足す。
        let (fu, fp, du, dm) = self.callee_effects(d, 0, argt, targs);
        self.record_effects(line, vec![(d.name.clone(), fu, fp, du, dm)]);
        // 4.3: 省略した `Fn*` 境界は、渡したクロージャ・関数の効果をそのまま持つ。効果を推論する (宣言のない)
        // 呼び出し先は、境界のパラメータの呼び出しを数えないので、呼び出しの所で渡した値の効果を負わせる。
        if d.inferred {
            let mut got: Vec<(Vec<String>, bool)> = vec![];
            let gn = d.gnames();
            for (k, a) in argt.iter().enumerate() {
                let Some(p) = d.params.get(k) else { break };
                for (name, at) in self.param_slots(&from_v(&p.1, &gn), a) {
                    let omitted = self.fn_bound(d, &name).iter().any(|b| items(tel(b, 1)).get(2).map_or(false, |v| tag(v) == "named" && st(tel(v, 1)).starts_with("E$")));
                    if !omitted {
                        continue;
                    }
                    if let Some((u, m)) = self.arg_fn_effects(&at) {
                        got.push((u, m.unwrap_or(false)));
                    }
                }
            }
            for (u, m) in got {
                let pn = m.then(|| "may".to_string());
                self.record_effects(line, vec![(format!("the closure passed to {}", d.name), u.clone(), pn.clone(), u, pn)]);
            }
        }
        // `I: Iterator<..>` 境界の引数は、呼び出し先が中で `next` を呼びうる。`for` と同じ求め方で、実引数の型の
        // `next` の効果を呼び出しの効果に足す (イテレータの型は効果の実引数を持たないので、宣言側の `E` / `P` だけでは漏れる)。
        let mut iter_args: Vec<Ty> = vec![];
        {
            let d = &self.p.defs[i];
            let gn = d.gnames();
            let iter_params: Vec<String> = d
                .generics
                .iter()
                .filter(|g| g.0 == "type" && items(&g.2).iter().any(|b| matches!(b, V::L(bx) if st(&bx[0]) == "core.iter.Iterator")))
                .map(|g| g.1.clone())
                .collect();
            if !iter_params.is_empty() {
                for (k, p) in d.params.iter().enumerate() {
                    let Some(a) = argt.get(k) else { continue };
                    for (n, at) in self.param_slots(&from_v(&p.1, &gn), a) {
                        if iter_params.contains(&n) {
                            iter_args.push(at);
                        }
                    }
                }
            }
        }
        for at in iter_args {
            self.for_next_effects(&at, line);
        }
        let d = &self.p.defs[i];
        let gn = d.gnames();
        // 明示型引数 (`f::<..>`, 4.3) のうち型・定数の実引数を、宣言のパラメータに束縛する。
        // 効果・panic の実引数は効果検査 (check.rs の inst_def_effects) で使う。
        // 実引数が並ばない (誤った) 呼び出しでは何も束縛せず、引数からの推論に任せる (黙る)。
        let mut map: HashMap<String, Ty> = HashMap::new();
        self.seed_targs(d, targs, &mut map, line);
        seed_effects(d, &mut map);
        // Fn 境界を受け取る引数の位置と、渡されたクロージャの型 (決まっているものだけ)。
        let mut fn_args: Vec<(usize, String)> = vec![];
        let mut closures: HashMap<usize, Ty> = HashMap::new();
        for (k, a) in argt.iter().enumerate() {
            let Some(p) = d.params.get(k) else { break };
            // `F` だけでなく `&mut F` / `&F` の形のパラメータも Fn 境界の実体化に使う
            // (`core.ffi.trampoline(&mut cb)`。貸すだけで移さない)。
            let Some(name) = fn_param_name(&from_v(&p.1, &gn)) else { continue };
            let bounds = self.fn_bound(d, &name);
            if !bounds.is_empty() {
                fn_args.push((k, name));
                let cl = self.resolve(a);
                if matches!(cl.strip(), Ty::Closure { .. }) {
                    closures.insert(k, cl.strip().clone());
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
        let (sig_ps, sig_ret) = Self::sig_elided(d, &gn);
        // Fn 境界の単一化: 境界の `Args` タプルと戻り値型を、クロージャの型と比べる。引数の照合より先に
        // 済ませる (`core.ffi.trampoline(&mut cb)` の `A` / `R` は、外の `extern` の関数型の引数との
        // 照合で決まる。照合は型パラメータを剛な型として扱うので、引数より先に決める必要がある)。
        // 参照越しの `&mut F` / `&F` (踏み台) だけをここで決める。値の `F` (`core.iter.filter` など) は
        // 下の `resolve_impl_bounds` の後に回す (境界の要素型を先に決めないと、クロージャの引数型との
        // 食い違いの E2023 が出なくなる)。
        let mut cap_bad = false;
        let by_ref = |k: usize| d.params.get(k).is_some_and(|p| matches!(from_v(&p.1, &gn), Ty::Ref(..)));
        for (k, name) in &fn_args {
            if !by_ref(*k) {
                continue;
            }
            let Some(cl) = closures.get(k).cloned() else { continue };
            self.unify_fn_bound(d, name, &cl, &mut map, line, &mut cap_bad);
        }
        for (k, a) in argt.iter().enumerate() {
            if d.params.get(k).is_none() { break; }
            let pt = self.inst(&Self::inst_regions(call_id, &sig_ps[k]), &mut map);
            self.note_region_cstr(call_id, &pt, a, "arg");
            if arg_ids.len() == argt.len() {
                self.side.conv_pending.push(((self.cur, arg_ids[k]), pt.clone(), a.clone()));
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
            }
            // 実引数の食い違い (E2023) は TAST の規則 T が報告する (推論はここで済む)。
            let _ = ur;
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
        // Fn 境界の単一化: 境界の `Args` タプルと戻り値型を、クロージャの型と比べる (値の `F`)。
        for (k, name) in &fn_args {
            if by_ref(*k) {
                continue;
            }
            let Some(cl) = closures.get(k).cloned() else { continue };
            self.unify_fn_bound(d, name, &cl, &mut map, line, &mut cap_bad);
        }
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
        let r = self.inst(&Self::inst_regions(call_id, &sig_ret), &mut map);
        // 引数にも戻り値にも現れず、明示の型引数もない型パラメータ (`core.mem.size_of()` の `T`) は、どこからも決まらない。
        for g in &d.generics {
            if g.0 == "type" && !map.contains_key(&g.1) {
                self.diag(
                    "E2044",
                    format!("cannot determine the type parameter `{}` of `{}`: add a type argument (`{}[..]`)", g.1, d.name, d.name),
                    line,
                );
            }
        }
        self.note_type_args(call_id, d, &map);
        self.box_hooks(d, &map, line);
        r
    }

    /// `Fn` 境界を持つ型パラメータ `name` に渡されたクロージャ (または関数項目) の型 `cl` を、
    /// 境界の `Args` タプル・戻り値型・capture の能力と単一化する。関数呼び出し (`call_def`) と
    /// メソッド呼び出し (`call_method`) で共通に使う。`k` は引数の添字 (0 始まり。メッセージは 1 始まり)。
    /// `cap_bad` は、この呼び出しで E2030 を出し済みかどうか (同じ呼び出しで 2 回出さない)。
    #[allow(clippy::too_many_arguments)]
    fn unify_fn_bound(&mut self, d: &Def, name: &str, cl: &Ty, map: &mut HashMap<String, Ty>, line: usize, cap_bad: &mut bool) {
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
                if args.len() == want_args.len() {
                    for (w, g) in want_args.iter().zip(&args) {
                        let _ = self.unify2(w, g);
                    }
                }
            }
            let want_ret = self.inst(&from_v(tel(&targs, 1), &gn), map);
            let _ = self.unify2(&want_ret, &ret);
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

    /// 関数の値 (クロージャの節・関数項目・関数ポインタ値) の効果 (`uses`, `may`)。分からないものは None。
    /// クロージャの節は本体から推論した効果 (確実でない分も上限として使う = fail-closed)。
    fn arg_fn_effects(&self, at: &Ty) -> Option<(Vec<String>, Option<bool>)> {
        match at {
            Ty::Closure { id: Some(node), .. } => {
                let (u, m, _unsure) = self.closure_effs.get(node)?;
                Some((u.iter().cloned().collect(), Some(*m)))
            }
            Ty::Closure { id: None, eff: Eff(Some(info)), .. } => Some((info.uses.clone(), info.may)),
            _ => None,
        }
    }

    /// 関数ポインタ型の仮引数 `pv` (`fn(..) uses [E] panics P` の V) に、実引数 `a` の効果を束縛する。
    /// 宣言の効果変数 (`uses` の大文字の名前) と panic 変数 (`P`) だけを束縛する。
    fn bind_fn_ptr_param(&self, d: &Def, pv: &V, a: &Ty, out: &mut EffBind) {
        let mut at = self.deep(a);
        while let Ty::Ref(_, x, _) = at {
            at = self.deep(&x);
        }
        let Some((uses, may)) = self.arg_fn_effects(&at) else { return };
        for e in strs(tel(pv, 3)) {
            if crate::check::is_effect_var(&e) && d.generics.iter().any(|g| g.0 == "uses" && g.1 == e) {
                out.uses.entry(e).or_default().extend(uses.iter().cloned());
            }
        }
        let pn = st(tel(pv, 4));
        if let Some(m) = may {
            if crate::check::is_panic_var(&pn) && d.generics.iter().any(|g| g.0 == "panics" && g.1 == pn) {
                *out.panics.entry(pn).or_default() |= m;
            }
        }
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
            while let Ty::Ref(_, x, _) = pt {
                pt = *x;
            }
            // 関数ポインタ型の引数 (`p: fn() uses [E] panics P`): 宣言の効果変数を、渡した値の効果で束縛する。
            if matches!(pt, Ty::Closure { .. }) {
                if tag(&p.1) == "fn" {
                    self.bind_fn_ptr_param(d, &p.1, a, &mut out);
                }
                continue;
            }
            for (name, at) in self.param_slots(&from_v(&p.1, &gn), a) {
            let bounds = self.fn_bound(d, &name);
            if bounds.is_empty() {
                continue;
            }
            let Some((uses, may)) = self.arg_fn_effects(&at) else { continue };
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
                // panic が分からない (`fn() panics P` の変数) ときは束縛しない (呼び出し元で fail-closed)。
                if let (Some(pv), Some(m)) = (var(3, "panics"), may) {
                    *out.panics.entry(pv).or_default() |= m;
                }
            }
            }
        }
        self.iter_effect_bind(d, off, argt, &mut out);
        out
    }

    /// パラメータの型 `pt` (型パラメータを含む) と実引数の型 `a` を構造的に突き合わせ、型パラメータ
    /// (`F` / `Option<F>` / `&S<F>` / `(F, u32)` の `F`) に当たる実引数の部分の型 (解決済み、参照は外す) を集める。
    fn param_slots(&self, pt: &Ty, a: &Ty) -> Vec<(String, Ty)> {
        let mut out = vec![];
        self.param_slots_walk(pt, a, &mut out);
        out
    }

    fn param_slots_walk(&self, pt: &Ty, a: &Ty, out: &mut Vec<(String, Ty)>) {
        let mut at = self.deep(a);
        while let Ty::Ref(_, x, _) = at {
            at = self.deep(&x);
        }
        let mut pt = pt;
        while let Ty::Ref(_, x, _) = pt {
            pt = x;
        }
        match (pt, &at) {
            (Ty::Param(n), _) => out.push((n.clone(), at)),
            (Ty::Named(n, pa, _), Ty::Named(m, aa, _)) if n == m && pa.len() == aa.len() => {
                for (x, y) in pa.iter().zip(aa) {
                    self.param_slots_walk(x, y, out);
                }
            }
            (Ty::Tuple(pa), Ty::Tuple(aa)) if pa.len() == aa.len() => {
                for (x, y) in pa.iter().zip(aa) {
                    self.param_slots_walk(x, y, out);
                }
            }
            (Ty::Array(x, _), Ty::Array(y, _)) | (Ty::Slice(x), Ty::Slice(y)) => self.param_slots_walk(x, y, out),
            _ => {}
        }
    }

    /// `I: core.iter.Iterator<T, E, P>` の境界を持つ引数 (`it: I`) の効果変数・panic 変数を、引数の型の
    /// `Iterator` の impl の効果実引数で束縛する (`core.iter.range` の `Range` は `[]` / `never`)。
    /// impl の実引数が具体的なものだけ束縛する。impl 側も変数 (`Map<.., E, P>` の `E`) なら分からないので
    /// 束縛しない (呼び出し元が同じ変数を持たなければ E4001 / E4002 になる = fail-closed)。
    fn iter_effect_bind(&self, d: &Def, off: usize, argt: &[Ty], out: &mut EffBind) {
        let gn = d.gnames();
        for (k, a) in argt.iter().enumerate() {
            let Some(p) = d.params.get(k + off) else { break };
            let mut pt = from_v(&p.1, &gn);
            while let Ty::Ref(_, x, _) = pt {
                pt = *x;
            }
            let Ty::Param(name) = pt else { continue };
            let mut at = self.deep(a);
            while let Ty::Ref(_, x, _) = at {
                at = self.deep(&x);
            }
            let Ty::Named(tn, _, _) = &at else { continue };
            for g in d.generics.iter().filter(|g| g.0 == "type" && g.1 == name) {
                for b in items(&g.2) {
                    let V::L(bx) = b else { continue };
                    if st(&bx[0]) != "core.iter.Iterator" {
                        continue;
                    }
                    let bt = items(tel(b, 1));
                    let Some(itargs) = self.impl_targs_of("core.iter.Iterator", tn) else { continue };
                    // 境界の実引数 (`E` / `P` の変数名) と、impl の実引数 (具体的な効果・panic) を対応させる。
                    let var = |i: usize, kind: &str| -> Option<String> {
                        let v = bt.get(i)?;
                        if tag(v) != "named" || !items(tel(v, 2)).is_empty() {
                            return None;
                        }
                        let nm = st(tel(v, 1));
                        d.generics.iter().any(|g| g.0 == kind && g.1 == nm).then_some(nm)
                    };
                    if let (Some(e), Some(ie)) = (var(1, "uses"), itargs.get(1)) {
                        if let Some(set) = crate::check::targ_effs(ie) {
                            if set.iter().all(|x| !crate::check::is_effect_var(x)) {
                                out.uses.entry(e).or_default().extend(set);
                            }
                        }
                    }
                    if let (Some(pv), Some(ip)) = (var(2, "panics"), itargs.get(2)) {
                        match crate::check::targ_panic(ip).as_deref() {
                            Some("never") => {
                                out.panics.entry(pv).or_insert(false);
                            }
                            Some("may") => {
                                out.panics.insert(pv, true);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    /// 定義 `d` の型パラメータに `core.iter.Iterator` の境界があるか (引数の型で効果が決まりうる)。
    fn has_iterator_bound(&self, d: &Def) -> bool {
        d.generics.iter().any(|g| g.0 == "type" && items(&g.2).iter().any(|b| matches!(b, V::L(bx) if st(&bx[0]) == "core.iter.Iterator")))
    }

    /// 型 `ty` (完全名) に対するトレイト `tr` の impl の実引数 (トレイトの型引数の列)。impl が無ければ None。
    fn impl_targs_of(&self, tr: &str, ty: &str) -> Option<Vec<V>> {
        self.p.defs.iter().find_map(|x| {
            if x.kw != "impl" || x.impl_trait != tr {
                return None;
            }
            let tgt = match &x.impl_for {
                V::L(xs) if matches!(xs.first(), Some(V::S(k)) if k == "named") => st(&xs[1]),
                _ => return None,
            };
            (tgt == ty).then(|| items(&x.impl_targs).to_vec())
        })
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
    /// 呼び先の宣言で `'static` の境界を持つ位置 (`&'static T` の引数・`'static` 境界の型パラメータを含む引数) に
    /// 渡した値を覚える。`&'static T` は出どころが `'static` と分かる式だけを通す (`static_expr`)。
    fn note_static_args(&mut self, d: &Def, off: usize, argt: &[Ty], arg_stat: &[bool], line: usize) {
        let gn = d.gnames();
        let bounded: HashSet<String> = d
            .generics
            .iter()
            .filter(|g| g.0 == "type" && items(&g.2).iter().any(|b| matches!(b, V::S(x) if x == "'static")))
            .map(|g| g.1.clone())
            .collect();
        let refs = static_ref_params(&d.header);
        for (k, a) in argt.iter().enumerate() {
            let Some(p) = d.params.get(k + off) else { break };
            let origin = arg_stat.get(k).copied().unwrap_or(false);
            if refs.get(k + off).copied().unwrap_or(false) {
                self.static_checks.push((Ty::Ref(false, Box::new(Ty::Unit), crate::types::Region::Unk), origin, line, format!("the argument `{}` of `{}` (`&'static`)", p.0, d.name)));
            } else if mentions_param(&from_v(&p.1, &gn), &bounded) {
                self.static_checks.push((a.clone(), origin, line, format!("the argument `{}` of `{}` (a `'static` bound)", p.0, d.name)));
            }
        }
    }

    /// 呼び出し先 `d` の効果を実体化する。明示型引数があればそれで (check.rs と同じ `inst_def_effects`)、
    /// なければクロージャから推論した束縛で置き換える。
    /// 戻り値は (実体化した uses, 実体化した panics, 束縛から来た uses, 束縛から来た panic が may か)。
    fn callee_effects(&self, d: &Def, off: usize, argt: &[Ty], targs: &[V]) -> (Vec<String>, Option<String>, Vec<String>, Option<String>) {
        if !targs.is_empty() {
            let (u, p) = crate::check::inst_def_effects(d, targs).unwrap_or_else(|| (d.uses.clone(), d.panics.clone()));
            return (u, p, vec![], None);
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
                    // 効果変数 (大文字) は具体名の宣言では覆われないので、そのまま検査する。
                    derived.extend(set.iter().filter(|x| crate::check::is_effect_var(x) || !crate::check::covers(&concrete, x)).cloned());
                }
                // 束縛のない効果変数は検査しない (実引数のない呼び出しは実体化できない。`unbound_effect_var_stays_silent`)。
                None => full.push(e.clone()),
            }
        }
        // 束縛から来た panic (`may` のとき) と、束縛されない panic 変数 (そのまま検査する) を、検査の義務にする。
        let mut derived_panic: Option<String> = None;
        let panics = d.panics.as_deref().map(|p| match bind.panics.get(p) {
            Some(&m) => {
                if m {
                    derived_panic = Some("may".to_string());
                }
                if m { "may".to_string() } else { "never".to_string() }
            }
            None => {
                // 束縛のない panic 変数は、呼び出し元が同じ変数を持たなければ E4002 (fail-closed)。ただし `Iterator`
                // 境界の引数があるときは、panic の実体は引数の型 (イテレータの型は効果実引数を持たない) で決まるので黙る。
                if crate::check::is_panic_var(p) && !self.has_iterator_bound(d) {
                    derived_panic = Some(p.to_string());
                }
                p.to_string()
            }
        });
        (full, panics, derived, derived_panic)
    }

    /// 型で解決できた呼び出しの効果を扱う。`cands` は候補ごとの (名前, 実体化した uses, 実体化した panics,
    /// 検査する uses, 検査する panics)。クロージャの本体の中なら、すべての候補に共通する効果を
    /// そのクロージャの効果に足す (下からの近似)。関数本体の直下なら、検査の義務を残す (test は検査しない)。
    fn record_effects(&mut self, line: usize, cands: Vec<(String, Vec<String>, Option<String>, Vec<String>, Option<String>)>) {
        if cands.is_empty() {
            return;
        }
        // 実体化した効果に `suspend` を含む呼び出しは中断点 (クロージャ・`Fn` 境界の呼び出し越しでも)。
        // 候補が複数あるときは 1 つでも含めば (fail-closed)。
        if cands.iter().any(|c| c.1.iter().any(|u| u == "suspend")) {
            self.susp_lines.insert((self.cur, line));
        }
        if !self.eff_acc.is_empty() {
            // クロージャの効果には、具体名と (囲む関数の) 効果変数の両方を数える。変数は呼び出しごとに
            // 呼び出し元の `uses` と比べられる (`closure_call_effects`)。
            let lower = |u: &Vec<String>| -> BTreeSet<String> { u.iter().cloned().collect() };
            // 推論された関数の効果は上からの近似 (infer.rs) なので、それが効果を足すときは確実でないとする。
            let unsure = cands.iter().any(|c| {
                self.inferred_def(&c.0) && (!lower(&c.1).is_empty() || c.2.as_deref() == Some("may"))
            });
            let mut common = lower(&cands[0].1);
            for c in &cands[1..] {
                let o = lower(&c.1);
                common.retain(|e| o.contains(e));
            }
            // 変数のままの panic (`P`) は、クロージャの効果としては上限の `may` として数える (fail-closed)。
            let all_may = cands.iter().all(|c| c.2.as_deref() == Some("may") || c.2.as_deref().map_or(false, crate::check::is_panic_var));
            if let Some(acc) = self.eff_acc.last_mut() {
                acc.0.extend(common);
                if all_may {
                    acc.1 = true;
                }
                acc.2 |= unsure;
            }
        }
    }

    /// クロージャの型での直接呼び出し (`f(x)`)。引数の個数 (E2022) と型 (E2023) を検査し、
    /// 戻り値の型を返す。`cl` がクロージャに決まらないときは None。
    ///
    /// `eff_name` が `Some` のときは、そのクロージャの本体から推論した効果を、呼び出し元の検査に足す (4.3 / 5.1)。
    fn call_closure(&mut self, cl: &Ty, argt: &[Ty], line: usize, eff_name: Option<&str>) -> Option<Ty> {
        let cl = self.deep(cl);
        let Ty::Closure { args, ret, id, eff, .. } = cl else { return None };
        // 直接呼び出し (`let g = fn..; g(x)`) は、呼び出し元からクロージャの節への辺になる (5.3)。
        // 節の本体の呼び出しは書かれた関数に帰属するが、節そのもののフレームを数えるには辺が要る。
        if let Some(node) = id.clone() {
            self.add_edge(node);
        }
        // 関数ポインタ型の値 (`f: fn() uses [E]`) の呼び出しは、宣言された効果で検査する。
        // 節 (`id` あり) は本体から推論した効果 (`closure_call_effects`) を使う。
        if id.is_none() {
            if let Eff(Some(info)) = &eff {
                if info.decl {
                    let uses = info.uses.clone();
                    let panics = (info.may == Some(true)).then(|| "may".to_string());
                    let nm = eff_name.unwrap_or("the function value").to_string();
                    self.record_effects(line, vec![(nm, uses.clone(), panics.clone(), uses, panics)]);
                }
            }
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
        for (w, g) in args.iter().zip(argt) {
            let _ = self.unify2(w, g);
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
            while let Ty::Ref(_, x, _) = t {
                t = self.deep(&x);
            }
            t
        };
        let (w, g) = (strip(want), strip(got));
        let is_vec = |t: &Ty| matches!(t, Ty::Named(n, _, _) if n == "alloc.vec.Vec");
        let is_seq = |t: &Ty| matches!(t, Ty::Array(_, _) | Ty::Slice(_));
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
            Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().any(|x| self.open(x)),
            Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => self.open(&x),
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
        if let Some(p) = Self::shape_conflict(e) {
            return Some(p);
        }
        match (&e.0, &e.1) {
            (Ty::Named(a, xs, _), Ty::Named(b, ys, _)) if a == b && a == "alloc.boxed.Box" && self.box_dyn_mismatch(xs, ys) => {
                Some((e.0.show(), e.1.show()))
            }
            (Ty::Prim(a), Ty::Prim(b)) if a != b => Some((a.clone(), b.clone())),
            (Ty::Named(a, _, _), Ty::Named(b, _, _)) if a != b => Some((a.clone(), b.clone())),
            (Ty::Const(a), Ty::Const(b)) if a != b => Some((a.clone(), b.clone())),
            (Ty::Ref(true, _, _), Ty::Ref(false, _, _)) => Some((e.0.show(), e.1.show())),
            _ => None,
        }
    }

    /// 参照の段数の食い違い (4.2: 暗黙に許すのは `&mut T` → `&T`、再借用、`&[T; N]` → `&[T]`、`&T` → `&dyn Tr` だけ)。
    /// 値 (`want`) が要る所に参照 (`got`) を渡した形 (`&u32` を `u32` として返す) を見つけて、診断用の
    /// 2 つの型の文字列を返す。型が決まっていない・型パラメータを含む場合は黙る。
    /// 逆向き (参照が要る所に値) は、パターン束縛が外側の参照を落とす近似があるので見ない。
    /// 型 `t` (参照は剥がす) が Copy か。決まらない型・型パラメータは true (従来の案内のまま)。
    pub(crate) fn ty_copy_ref(&self, t: &Ty) -> bool {
        let mut t = self.deep(t);
        while let Ty::Ref(_, inner, _) = t {
            t = *inner;
        }
        self.ty_copy_hint(&t)
    }

    fn ty_copy_hint(&self, t: &Ty) -> bool {
        match t {
            Ty::Named(nm, args, _) if nm == "Option" || nm == "Result" => args.iter().all(|x| self.ty_copy_hint(x)),
            Ty::Tuple(xs) => xs.iter().all(|x| self.ty_copy_hint(x)),
            _ => self.ty_copy(t) != Some(false),
        }
    }

    fn deref_hint(&self, want: &Ty) -> String {
        deref_advice(self.ty_copy_ref(want))
    }

    fn ref_level_conflict(&self, want: &Ty, got: &Ty) -> Option<(String, String)> {
        if let Some(p) = self.top_ref_conflict(want, got) {
            return Some(p);
        }
        // 入れ子の型引数: 所有型 (Copy でない値) を参照のまま包むと、戻り値・列挙子のペイロードの食い違い。
        // Copy のスカラーの参照 (`Option<&u32>` を `Option<u32>` とする形) は既存の扱いのまま通す。
        let (w, g) = (self.deep(want), self.deep(got));
        if !w.has_param() && !g.has_param() && self.owned_ref_clash(&w, &g) {
            return Some((w.show(), g.show()));
        }
        None
    }

    /// 所有型 (Copy でない型) か。`String`・`Vec`・所有物を持つ構造体 / 列挙は所有型、`Option` / `Result` は中身で決まる。
    /// 型定義が分からない名前 (標準ライブラリの不透明な型) は所有型とみなす。
    fn owned_ty(&self, t: &Ty) -> bool {
        match self.deep(t) {
            Ty::Named(n, args, _) if n == "Option" || n == "Result" => args.iter().any(|a| self.owned_ty(a)),
            Ty::Named(n, _, _) => self.type_def(&n).map_or(true, |d| !d.derive.iter().any(|x| x == "Copy")),
            Ty::Tuple(xs) => xs.iter().any(|x| self.owned_ty(x)),
            Ty::Array(x, _) => self.owned_ty(&x),
            _ => false,
        }
    }

    /// 入れ子の位置 (同じ名前の `Named` の型引数、同じ長さの `Tuple` の要素) で、所有型の値が参照のまま現れるか。
    /// 決まっていない型変数の所は黙る。先頭の段数は `top_ref_conflict` が見る。
    fn owned_ref_clash(&self, want: &Ty, got: &Ty) -> bool {
        let (w, g) = (self.deep(want), self.deep(got));
        match (&w, &g) {
            (Ty::Ref(_, a, _), Ty::Ref(_, b, _)) => self.owned_ref_clash(a, b),
            (x @ (Ty::Named(..) | Ty::Tuple(_) | Ty::Array(_, _)), Ty::Ref(..)) => {
                self.owned_ty(x) && !self.undetermined(x)
            }
            (Ty::Named(n, xs, _), Ty::Named(m, ys, _)) if n == m && xs.len() == ys.len() => {
                xs.iter().zip(ys.iter()).any(|(x, y)| self.owned_ref_clash(x, y))
            }
            (Ty::Tuple(xs), Ty::Tuple(ys)) if xs.len() == ys.len() => xs.iter().zip(ys.iter()).any(|(x, y)| self.owned_ref_clash(x, y)),
            _ => false,
        }
    }

    /// 式の型が「宣言どおりの値の型」と信用できるか。
    /// 分岐・ブロック・ループを経る式は信用しない。
    fn value_typed_expr(&self, e: &V) -> bool {
        match nname(e) {
            "Lit" | "Call" | "MCall" | "StructLit" | "Binary" => true,
            "Path" => true,
            _ => false,
        }
    }

    /// 参照の注釈 `&T` に、参照でない値の型が来ているか (`let r: &u64 = 5u64`)。型が決まらない・型パラメータが絡むときは黙る。
    fn value_for_ref_conflict(&self, want: &Ty, got: &Ty) -> Option<(String, String)> {
        let (w, g) = (self.deep(want), self.deep(got));
        if self.undetermined(&w) || self.undetermined(&g) || w.has_param() || g.has_param() {
            return None;
        }
        match (&w, &g) {
            (Ty::Ref(..), Ty::Prim(_) | Ty::Named(..) | Ty::Tuple(_) | Ty::Array(_, _) | Ty::Unit) => Some((w.show(), g.show())),
            _ => None,
        }
    }

    fn top_ref_conflict(&self, want: &Ty, got: &Ty) -> Option<(String, String)> {
        let w = self.deep(want);
        let g = self.deep(got);
        if self.undetermined(&w) || self.undetermined(&g) || w.has_param() || g.has_param() {
            return None;
        }
        fn value_like(t: &Ty) -> bool {
            matches!(t, Ty::Prim(_) | Ty::Named(..) | Ty::Tuple(_) | Ty::Array(_, _) | Ty::Unit)
        }
        let (mut wi, mut gi) = (&w, &g);
        // 参照の段数を揃えて比べる。
        loop {
            match (wi, gi) {
                (Ty::Ref(_, a, _), Ty::Ref(_, b, _)) => {
                    wi = a;
                    gi = b;
                }
                (x, Ty::Ref(..)) if value_like(x) => return Some((w.show(), g.show())),
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
        if let (Ty::Named(n, xs, _), Ty::Named(m, ys, _)) = (w.strip(), g.strip()) {
            if n == m && !xs.is_empty() && !ys.is_empty() && !xs.iter().any(|x| self.unresolved(x)) && !ys.iter().any(|x| self.unresolved(x)) {
                let a = w.strip().clone();
                let b = g.strip().clone();
                if a.show() != b.show() {
                    return Some((a.show(), b.show()));
                }
                return None;
            }
        }
        if let Some(p) = Self::shape_conflict(err) {
            return Some(p);
        }
        match (&err.0, &err.1) {
            (Ty::Prim(a), Ty::Prim(b)) => Some((a.clone(), b.clone())),
            (Ty::Named(a, _, _), Ty::Named(b, _, _)) if a != b => Some((a.clone(), b.clone())),
            // `()` と値の型の食い違い (`-> Result<..>` の関数が `()` で終わるなど)。
            (Ty::Ref(true, _, _), Ty::Ref(false, _, _)) => Some((err.0.show(), err.1.show())),
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
            Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().any(|x| self.unresolved(x)),
            Ty::Ref(_, x, _) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => self.unresolved(&x),
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
        let Ty::Named(wn, _, _) = want.strip() else { return None };
        if self.unresolved(got.strip()) {
            return None;
        }
        // 参照の段数も含めて比べる (`&E1` のエラーは、ペイロードが `E1` のバリアントへは包めない)。
        let key = got.show();
        if key == want.strip().show() {
            return None;
        }
        Some(self.p.from_variant(wn, &key))
    }

    /// 4.1: `?` はエラー型が完全一致するか、`derive [From]` の `enum` へ包めるときだけ使える。
    /// `?` は式を置いた関数 (クロージャ) の戻り値型へ脱糖するので、そこで受けられるのは
    /// オペランドと同じ種類の `Option` / `Result` だけ。エラー型は単一化して比べる。
    /// どちらかが分からないときは黙る (Cranelift バックエンドも同じ条件で拒否する)。
    fn check_try_err(&mut self, operand: &Ty, line: usize, id: u32) {
        let got = self.deep(operand);
        let Ty::Named(gn, ga, _) = got.strip() else { return };
        if gn != "Result" && gn != "Option" {
            return;
        }
        let Some(want) = self.rets.last().cloned() else { return };
        let want = self.deep(&want);
        let want = want.strip().clone();
        match want {
            Ty::Unknown | Ty::Param(_) | Ty::Var(_) | Ty::Never => {}
            Ty::Named(wn, wa, _) if wn == *gn => {
                // 種類は一致。`Result` のエラー型が完全一致するかだけを見る。
                if gn == "Result" && wa.len() >= 2 && ga.len() >= 2 {
                    let e_want = wa[1].clone();
                    let e_got = ga[1].clone();
                    // 4.1: 型が違っても `E2` が `derive [From]` で `E1` を唯一のペイロードに持つバリアントがあれば、包んで返す。
                    match self.from_conversion(&e_want, &e_got) {
                        Some(crate::program::FromVariant::One(v)) => {
                            self.side.err_pending.push(((self.cur, id), e_got.clone(), e_want.clone(), v));
                            return;
                        }
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
                        {
                            let (w, g) = self.type_conflict(&err).unwrap_or_else(|| Self::raw_pair(&err));
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
    /// `x` / `x.f` / `x.f.g` の根の局所変数のパス (`Field` は根まで辿る)。
    fn base_local_segs(&self, e: &V) -> Option<Vec<String>> {
        match nname(e) {
            "Path" => Some(strs(&nf(e)[0])),
            "Field" => self.base_local_segs(&nf(e)[0]).map(|mut v| {
                v.push(String::new());
                v
            }),
            _ => None,
        }
    }

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
/// 配列の長さ (`[e; N]` の `N`。`const_expr` が `V::N` の綴りで持つ) が `usize` の範囲に収まらないときの説明。
/// 長さが整数の綴りでないもの (定数名) は None (その定数の初期化式の範囲で検査される)。
/// 配列の長さが整数リテラルならその値 (定数名・式は `None`)。
fn array_len(len: &V) -> Option<u128> {
    let V::N(text) = len else { return None };
    let t = text.replace('_', "");
    let (d, r) = if let Some(x) = t.strip_prefix("0x") { (x, 16) } else if let Some(x) = t.strip_prefix("0b") { (x, 2) } else if let Some(x) = t.strip_prefix("0o") { (x, 8) } else { (t.as_str(), 10) };
    u128::from_str_radix(d, r).ok()
}

pub(crate) fn array_len_problem(len: &V) -> Option<String> {
    match len {
        V::N(text) => int_literal_problem(text, "usize"),
        _ => None,
    }
}

pub(crate) fn int_literal_problem(text: &str, ty: &str) -> Option<String> {
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
        // `usize` / `isize` はターゲットの幅 (4.5。wasm32 は 32 ビット)。
        "isize" | "usize" => crate::ptrwidth::bits(),
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

/// 関数ヘッダの戻り値の型が `'static` の参照だけか (`-> &'static str`)。参照を含まない型は false。
fn ret_is_static(header: &str) -> bool {
    let Some(head) = header.split(" uses ").next() else { return false };
    let Some(mut i) = head.find(|c| c == '(' || c == '[') else { return false };
    if head.as_bytes()[i] == b'[' {
        let Some(j) = match_close(head, i) else { return false };
        i = j + 1;
    }
    if head.as_bytes().get(i) != Some(&b'(') {
        return false;
    }
    let Some(close) = match_close(head, i) else { return false };
    let Some((_, ret)) = head[close + 1..].split_once("->") else { return false };
    let amp = ret.matches('&').count();
    amp > 0 && amp == ret.matches("&'static").count()
}

/// 関数ヘッダの各引数が `&'static ..` と宣言されているか。
fn static_ref_params(header: &str) -> Vec<bool> {
    let Some(head) = header.split(" uses ").next() else { return vec![] };
    let Some(mut i) = head.find(|c| c == '(' || c == '[') else { return vec![] };
    if head.as_bytes()[i] == b'[' {
        let Some(j) = crate::check::borrowck::match_close(head, i) else { return vec![] };
        i = j + 1;
    }
    if head.as_bytes().get(i) != Some(&b'(') {
        return vec![];
    }
    let Some(close) = crate::check::borrowck::match_close(head, i) else { return vec![] };
    crate::check::borrowck::split_top(&head[i + 1..close])
        .iter()
        .map(|t| t.split_once(':').map_or(false, |(_, ty)| ty.trim_start().starts_with("&'static")))
        .collect()
}

/// 型の中に、名前が `names` にある型パラメータがあるか。
fn mentions_param(t: &Ty, names: &HashSet<String>) -> bool {
    match t {
        Ty::Param(p) => names.contains(p),
        Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().any(|x| mentions_param(x, names)),
        Ty::Ref(_, x, _) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => mentions_param(x, names),
        Ty::Closure { args, ret, .. } => args.iter().any(|x| mentions_param(x, names)) || mentions_param(ret, names),
        _ => false,
    }
}

/// `'static` と分かる束縛に付ける印の名前 (変数名と衝突しないよう、空白と制御文字を含む)。
fn exact_mark(name: &str) -> String {
    format!("\u{1}exact:{}", name)
}

fn static_mark(name: &str) -> String {
    format!("\u{1}static {}", name)
}

/// 型 V の中に参照 (`&T` / `&mut T`) があるか ('static でないことの近似。5.1 の `Box<dyn>`)。
fn v_has_ref(v: &V) -> bool {
    match v {
        V::L(xs) => tag(v) == "ref" || xs.iter().any(v_has_ref),
        _ => false,
    }
}
