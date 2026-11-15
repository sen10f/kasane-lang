//! 意味検査 (stage0)。Python 参照実装 ksn/sema.py (Checker) と ksn/typeck.py (TypeCk) の移植。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::program::{Def, Program, Resolved, Target};
use crate::parser::{V, nid};

#[path = "borrowck.rs"]
pub(crate) mod borrowck;
#[path = "with_check.rs"]
mod with_check;
pub(crate) use with_check::cleanup_exit;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Diag {
    pub file: String,
    pub code: String,
    pub fqn: String,
    pub line: usize,
    /// 1 始まりの列 (Unicode スカラ値)。0 は「列不明」(呼び出し側で行頭の非空白に補う)。
    pub col: usize,
    pub msg: String,
}

// ---------------------------------------------------------------- V の読み取り補助
pub(crate) fn nname(v: &V) -> &str {
    match v {
        V::Node(n, _, _, _) => n,
        _ => "",
    }
}
pub(crate) fn nf(v: &V) -> &[V] {
    match v {
        V::Node(_, f, _, _) => f,
        _ => &[],
    }
}
/// ノードの位置 (行と列を符号化した値)。診断にはこれをそのまま渡す。行番号だけが要るなら `nlineno`。
pub(crate) fn nline(v: &V) -> usize {
    match v {
        V::Node(_, _, l, _) => *l,
        _ => 0,
    }
}
/// ノードの行番号 (列を除く)。
pub(crate) fn nlineno(v: &V) -> usize {
    crate::parser::unpack_pos(nline(v)).0
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

/// 取り消せないタスクを取り消したときの E4022 の文面 (木走査・ネイティブの検査で同じ文面)。
pub(crate) fn cancel_msg() -> String {
    "cannot cancel this task: its frame may hold a value that needs release (a linear or owning value, or a value whose type is not known); cancel only a handle bound by `let t = std.task.spawn_local(closure)` whose closure holds only scalar values and calls no suspending user function".to_string()
}

/// 取り消せるタスクの枠に持ってよい型 (仕様 5.6 の第 4 段): 数・`bool`・`char`・単位・参照、と `std.task.Task` (番号だけの値)。
/// 解放の要る値 (所有する `Vec`・`String`・線形の型など) と、型が決まらない値 (`Nil`) は含まない。
pub(crate) fn frame_scalar(t: &V) -> bool {
    if is_nil(t) {
        return false;
    }
    match tag(t).as_str() {
        "unit" | "ref" => true,
        // 単位型は要素の空なタプルとして表される。タプルは要素がすべて枠に置けるときだけ。
        "tuple" => items(tel(t, 1)).iter().all(frame_scalar),
        "prim" => matches!(
            st(tel(t, 1)).as_str(),
            "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "f32" | "f64" | "bool" | "char"
        ),
        "named" => st(tel(t, 1)) == "std.task.Task",
        _ => false,
    }
}

/// 型が名前の付いた型 `name` か (型引数は問わない)。
pub(crate) fn is_named_ty(t: &V, name: &str) -> bool {
    !is_nil(t) && tag(t) == "named" && st(tel(t, 1)) == name
}
/// 型の値の構造的な等しさ (`V` は `PartialEq` を持たない)。節点を含むものは等しいとみなさない (黙る方向)。
pub(crate) fn same_v(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::S(x), V::S(y)) | (V::N(x), V::N(y)) => x == y,
        (V::B(x), V::B(y)) => x == y,
        (V::L(x), V::L(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_v(p, q)),
        (V::Nil, V::Nil) => true,
        _ => false,
    }
}
/// 添字が整数リテラル (10 進) のときその値。それ以外は None。
pub(crate) fn lit_index(e: &V) -> Option<usize> {
    if nname(e) != "Lit" || st(&nf(e)[0]) != "int" {
        return None;
    }
    st(&nf(e)[1]).parse::<usize>().ok()
}

/// 診断メッセージ用にインデックス式 `a[i]` を `a[0usize]` 風にレンダリングする。
/// 解釈できない部分は `dump()` による近似で済ませる。
pub(crate) fn index_text(e: &V) -> String {
    let f = nf(e);
    let seg = |x: &V| -> String {
        if nname(x) == "Path" {
            return strs(&nf(x)[0]).join(".");
        }
        if nname(x) == "Lit" {
            let ps = nf(x);
            let mut s = st(ps.get(1).unwrap_or(&V::Nil));
            if let Some(sfx) = ps.get(2) {
                if !is_nil(sfx) {
                    s.push_str(&st(sfx));
                }
            }
            return s;
        }
        x.dump()
    };
    format!("{}[{}]", seg(&f[0]), seg(&f[1]))
}
/// enum 風の型 (`Option` / `Result` / ユーザー定義の enum) か。パターンで中身を取り出すと、その場所の全体が move される。
fn sty0_is_enumlike(t: &V) -> bool {
    tag(t) == "named"
}

/// 診断の文面に出す式の短い表記 (書かれたとおりでなくてよい)。
fn short_text(e: &V) -> String {
    let f = nf(e);
    match nname(e) {
        "Path" => strs(&f[0]).join("."),
        "Field" if nname(&f[0]) == "Unary" => format!("({}).{}", short_text(&f[0]), st(&f[1])),
        "Field" => format!("{}.{}", short_text(&f[0]), st(&f[1])),
        "Unary" => format!("{}{}", st(&f[0]), short_text(&f[1])),
        "Index" => index_text(e),
        "Call" | "MCall" => "(call result)".to_string(),
        _ => "(expression)".to_string(),
    }
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

/// 生ポインタ型 `*const T` / `*mut T` (parser が `V::L [ptr, mut?, T]` で作る形)。
/// 変数名の `ptr` (パスの `V::L` の先頭が文字列) とは、2 番目が真偽値であることで区別する。
fn is_ptr_ty(v: &V) -> bool {
    matches!(v, V::L(xs) if xs.len() == 3 && matches!(xs[1], V::B(_)) && st(&xs[0]) == "ptr")
}

/// 型・式・パターンのどこであれ、生ポインタ型を含む最初の位置 (近い節点の行)。型は `V::L` で、
/// `children` は `V::L` の中の節点しか辿らないので、型の中まで見るにはこちらを使う。
pub(crate) fn find_ptr_ty(v: &V, line: usize) -> Option<usize> {
    if is_ptr_ty(v) {
        return Some(line);
    }
    match v {
        V::L(xs) => xs.iter().find_map(|x| find_ptr_ty(x, line)),
        V::Node(_, fs, l, _) => fs.iter().find_map(|x| find_ptr_ty(x, *l)),
        _ => None,
    }
}

/// 定義のシグネチャ (引数・戻り値) に生ポインタ型があるか。
pub(crate) fn sig_has_ptr(d: &Def) -> bool {
    d.params.iter().any(|p| find_ptr_ty(&p.1, 0).is_some()) || find_ptr_ty(&d.ret, 0).is_some()
}

/// 型表 (`types.rs` の `tytab`) から、メソッド呼び出し (`v.pop();`) の式文で、戻り値が `()` / `!` 以外に
/// 決まる行を集める (4.6, E2042)。`ty_of` は標準ライブラリのジェネリック型のメソッドの戻り値を決められないので、
/// その戻り値 (レシーバの型引数を sig の戻り値型に代入した型) を型表から取る。
/// 関数呼び出し・字段の捨ては対象外 (`discard_check` の担当で、従来どおり黙る形もある)。
/// ブロックの最後の文 (ブロックの値) は捨てない。型に未知・型変数・型パラメータ・クロージャが
/// 残るものは黙る (誤検出を出さない)。
fn discarded_by_type(tk: &crate::types::TypeCk, d: &Def) -> Vec<usize> {
    let Some(body) = &d.body else { return vec![] };
    let mut all = vec![];
    walk(body, &mut all);
    // 局所名 (引数と `PBind`)。`v.pop()` は `Call` の経路 `["v", "pop"]` として parse され、先頭が
    // 局所名のものだけがレシーバのあるメソッド呼び出し (型検査器の `call` と同じ判定)。
    let mut locals: std::collections::HashSet<String> = d.params.iter().map(|p| p.0.clone()).collect();
    for v in &all {
        if nname(v) == "PBind" {
            locals.insert(st(&nf(v)[0]));
        }
    }
    let is_method_call = |e: &V| match nname(e) {
        "MCall" => true,
        "Call" => {
            let callee = &nf(e)[0];
            nname(callee) == "Path" && {
                let segs = strs(&nf(callee)[0]);
                segs.len() >= 2 && locals.contains(&segs[0])
            }
        }
        _ => false,
    };
    let mut lines = vec![];
    for b in all.iter().filter(|v| nname(v) == "Block") {
        let stmts = items(&nf(b)[0]);
        for s in stmts.iter().take(stmts.len().saturating_sub(1)) {
            if nname(s) != "ExprStmt" || !is_method_call(&nf(s)[0]) {
                continue;
            }
            let Some(et) = tk.expr_type(&d.name, crate::parser::nid(&nf(s)[0])) else { continue };
            let unit = matches!(et.ty, crate::types::Ty::Unit | crate::types::Ty::Never)
                || matches!(&et.ty, crate::types::Ty::Tuple(xs) if xs.is_empty());
            if !unit && ty_fully_known(&et.ty) {
                lines.push(nline(s));
            }
        }
    }
    lines
}

/// 型が具体的に決まっているか (未知・型変数・型パラメータ・クロージャを含まない)。
fn ty_fully_known(t: &crate::types::Ty) -> bool {
    use crate::types::Ty;
    match t {
        Ty::Unknown | Ty::Var(_) | Ty::Param(_) | Ty::Closure { .. } => false,
        Ty::Never | Ty::Unit | Ty::Prim(_) | Ty::Const(_) | Ty::Dyn(..) => true,
        Ty::Named(_, xs) | Ty::Tuple(xs) => xs.iter().all(ty_fully_known),
        Ty::Ref(_, x) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => ty_fully_known(x),
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

/// 効果変数・panic 変数の名前か (大文字で始まる。具体名 `io.console` や `never` / `may` とは違う)。
pub(crate) fn is_effect_var(eff: &str) -> bool {
    eff.chars().next().map_or(false, |c| c.is_uppercase())
}

pub(crate) fn is_panic_var(p: &str) -> bool {
    is_effect_var(p)
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

/// 効果変数の実引数 (`uses [..]` の集合、または呼び出し元の効果変数の名前 `E`) の要素。
/// 形が分からないものは None。`dyn Tr<E>` / `f::<E>` のように、呼び出し元の効果変数をそのまま渡す形も含む。
pub(crate) fn targ_effs(t: &V) -> Option<Vec<String>> {
    match tag(t).as_str() {
        "effects" => Some(items(tel(t, 1)).iter().map(st).collect()),
        "named" if items(tel(t, 2)).is_empty() && is_effect_var(&st(tel(t, 1))) => Some(vec![st(tel(t, 1))]),
        _ => None,
    }
}

/// panic 変数の実引数 (`never` / `may`、または呼び出し元の panic 変数の名前 `P`)。形が分からないものは None。
pub(crate) fn targ_panic(t: &V) -> Option<String> {
    match tag(t).as_str() {
        "panic" => Some(st(tel(t, 1))),
        "named" if items(tel(t, 2)).is_empty() && is_panic_var(&st(tel(t, 1))) => Some(st(tel(t, 1))),
        _ => None,
    }
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
    // 種別の並ばない実引数 (効果変数に型を渡す形) は、実体化できないものとして None (黙る。`violation` が見る)。
    let mut uses: Vec<String> = vec![];
    for e in &callee.uses {
        match pairs.iter().find(|(n, k, _)| *k == "uses" && *n == e.as_str()) {
            // `uses [alloc, E]` は具体名と実引数の集合の和 (4.3)。
            Some((_, _, t)) => uses.extend(targ_effs(t)?),
            None => uses.push(e.clone()),
        }
    }
    let panics = match callee.panics.as_deref() {
        None => None,
        Some(p) => match pairs.iter().find(|(n, k, _)| *k == "panics" && *n == p) {
            Some((_, _, t)) => Some(targ_panic(t)?),
            None => Some(p.to_string()),
        },
    };
    Some((uses, panics))
}

/// トレイトのメソッドの宣言した効果 (`uses` の要素と `panics`) を、`dyn Tr<E, P>` の型引数で実体化する (5.1)。
/// 型引数に対応のない効果変数は、変数のまま残す (`covers` が黙って通す)。
pub(crate) fn inst_effects(gens: &[(String, String, V)], targs: &[V], uses: &[String], panics: &str) -> (Vec<String>, String) {
    let pairs = align_targs(gens, targs).unwrap_or_default();
    let u: Vec<String> = uses
        .iter()
        .flat_map(|e| match pairs.iter().find(|(n, k, _)| *k == "uses" && *n == e.as_str()) {
            Some((_, _, t)) => targ_effs(t).unwrap_or_else(|| vec![e.clone()]),
            None => vec![e.clone()],
        })
        .collect();
    let p = match pairs.iter().find(|(n, k, _)| *k == "panics" && *n == panics) {
        Some((_, _, t)) => targ_panic(t).unwrap_or_else(|| panics.to_string()),
        None => panics.to_string(),
    };
    (u, p)
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
    ("TryClone", "alloc.alloc.TryClone"),
    ("Drop", "core.ops.Drop"),
    ("PartialEq", "core.cmp.PartialEq"),
    ("Eq", "core.cmp.Eq"),
    ("PartialOrd", "core.cmp.PartialOrd"),
    ("Ord", "core.cmp.Ord"),
    ("Hash", "core.hash.Hash"),
    ("ToJson", "std.json.ToJson"),
    ("FromJson", "std.json.FromJson"),
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
    /// `tests/` 配下 (統合テスト、3.2) のファイルパス。ここに属する定義には別の規則を当てる。
    pub integration_files: HashSet<String>,
    unsafe_ok: bool,
    // typeck の状態
    d: usize,
    names: HashSet<String>,
    /// move 済みの場所 (根の変数 + フィールド道をドットでつないだ鍵。`s`・`s.f`・`s.a.b`) → move した行。
    /// 鍵どうしは互いに重ならない (一方が他方の接頭辞なら記録しない)。E3002 (4.1・4.4)。
    moved: BTreeMap<String, usize>,
    /// 関数の本体で move した場所 (式の位置, 場所の鍵)。借用検査が、借用中の move を E3010 にするのに使う。
    move_sites: HashSet<(usize, String)>,
    /// 囲むループごとの (`break` 時点の move, `continue` 時点の move)。
    loop_exits: Vec<(BTreeMap<String, usize>, BTreeMap<String, usize>)>,
    /// 線形値の消費義務 (4.4)。束縛を宣言順に積む (線形でない束縛もシャドーイングの目印として積む)。
    lin: Vec<Lin>,
    /// 現在の関数 / クロージャが見る `lin` の下限 (クロージャの外の束縛は、本体の `return` で漏れとしない)。
    lin_floor: usize,
    lin_in_closure: bool,
    /// 直前のクロージャ式が、未消費の線形値を `move` で取り込んだか (その `let` の束縛が線形になる)。
    lin_closure_took: bool,
    /// 囲むループごとの、`break` / `continue` 時点の義務の状態。
    lin_loops: Vec<LinLoop>,
    /// 現在の関数の型パラメータのうち `linear` と宣言されたもの。その型 `T` の値は線形として扱う (4.4)。
    lin_gnames: HashSet<String>,
    /// 型推論 (types.rs) が決めた `let` の束縛の型。自前の `ty_of` で決まらない束縛に使う。
    let_tys: HashMap<(usize, usize, String), crate::types::Ty>,
    /// `a[i]` で要素を move した (consume された) 添字式の (関数名, 式 ID)。
    /// `tc_expr` 時点では Vec 等の基の型が `Nil` なので、型表 (`tytab`) が確定した
    /// `typeck()` の後処理で要素型を引いて E3002 を出す (4.1)。
    index_consumes: Vec<(String, u32, String, usize)>,
    /// 値を消費するクロージャ (`FnOnce`) を束縛した局所の名前。呼ぶと move 済みになる (4.1)。
    fnonce_locals: HashSet<String>,
    /// Copy でないクロージャの値 (`move` で Copy でない値・`&mut` を取り込んだ、または `FnOnce` の) を束縛した局所の名前。
    /// 値として渡す・別の名前へ移すと、そのクロージャ自身が move される (4.1)。
    noncopy_closures: HashSet<String>,
    /// 取り消し (`Task<T>.cancel`、仕様 5.6 の第 4 段) の検査: `let t = spawn_local(閉包)` の閉包を調べている間の入れ子の
    /// 印。閉包の中で解放の要る値・解決できない値・ユーザの関数の呼び出しが出たら、外側のすべての印を立てる。
    spawn_scan: Vec<bool>,
    /// `let t = spawn_local(閉包)` で束縛した局所 `(定義の番号, 名前)` → 閉包が取り消せるか (`cancel` の検査)。
    task_safe: HashMap<(usize, String), bool>,
    /// 参照越しの `match` の腕で、`&T` として束縛した名前 (Copy でない中身)。値を要る所へ渡すと move になってしまう。
    ref_binds: HashSet<String>,
    /// `let r = &x` で、`x` を共有参照で借りた束縛 (欄への代入を E3001 にする)。関数ごと。
    shared_lets: HashSet<String>,
    /// `let` で束縛した名前 (仮引数と区別する。関数ごと)。
    let_names: HashSet<String>,
    /// `a[i].f` (添字の先のフィールド) を値として使った (関数名, Field 式の ID, 文言, 位置)。
    /// フィールドの型を型表で引き、Copy でなければ E3002 (配列・Vec の要素の先からは move できない、4.1)。
    field_consumes: Vec<(String, u32, String, usize)>,
    /// 型表 (`TypeCk::tytab`) の写し。`tc_fn` より前に作り、注釈のない `let` の型を引くのに使う (4.1)。
    mv_types: BTreeMap<(String, u32), crate::types::ExprTy>,
    /// 型検査が解決したメソッド呼び出し先 (`TypeCk::call_res`)。借用検査が名前でなく解決先で引く。
    call_res: std::rc::Rc<HashMap<(usize, usize, String), Option<Vec<usize>>>>,
    final_env: HashMap<String, V>,
    graph: BTreeMap<String, BTreeSet<String>>,
    /// クロージャ節 (5.3 の `関数名#番号`) → そのクロージャの AST ノード。
    /// スタック上界の見積り (E4012) が、節のフレームを関数と同じ式で計算するために使う。
    closure_asts: BTreeMap<String, V>,
    /// 現在の関数のファイルで `import` したモジュールの末尾名 (シャドーイング検査用)。
    import_tails: HashSet<String>,
}

type Env = HashMap<String, V>;

/// 線形値の消費義務の 1 件 (4.4)。`live` は「線形で、まだ消費されていない」。
#[derive(Clone)]
struct Lin {
    name: String,
    /// 宣言の位置 (パック済み)
    line: usize,
    linear: bool,
    live: bool,
    /// 線形な欄を持つ構造体の根 (義務は欄ごとの後続の項目 `name.欄` が持つ)。根自身は義務を持たない。
    parts: bool,
}

/// 義務の名前 (`p` または欄の `p.a`) を、診断の文に出す形へ分ける: 根の名前と、欄の注記 (` (field `a`)`)。
fn lin_label(name: &str) -> (String, String) {
    match name.split_once('.') {
        Some((root, field)) => (root.to_string(), format!(" (field `{field}`)")),
        None => (name.to_string(), String::new()),
    }
}

/// ループ 1 つぶんの、`break` / `continue` 時点の義務の状態 (説明, 状態)。
struct LinLoop {
    base: usize,
    breaks: Vec<(String, Vec<Lin>)>,
    conts: Vec<(String, Vec<Lin>)>,
}

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
            integration_files: HashSet::new(),
            unsafe_ok: false,
            d: 0,
            names: HashSet::new(),
            moved: BTreeMap::new(),
            move_sites: HashSet::new(),
            loop_exits: vec![],
            lin: vec![],
            lin_floor: 0,
            lin_in_closure: false,
            lin_closure_took: false,
            lin_loops: vec![],
            lin_gnames: HashSet::new(),
            let_tys: HashMap::new(),
            fnonce_locals: HashSet::new(),
            noncopy_closures: HashSet::new(),
            spawn_scan: Vec::new(),
            task_safe: HashMap::new(),
            field_consumes: vec![],
            index_consumes: Vec::new(),
            ref_binds: HashSet::new(),
            shared_lets: HashSet::new(),
            let_names: HashSet::new(),
            mv_types: BTreeMap::new(),
            call_res: Default::default(),
            final_env: HashMap::new(),
            graph: BTreeMap::new(),
            closure_asts: BTreeMap::new(),
            import_tails: HashSet::new(),
        }
    }

    fn def(&self, i: usize) -> &'a Def {
        &self.p.defs[i]
    }

    fn diag(&mut self, code: &str, msg: String, di: usize, line: usize) {
        let d = self.def(di);
        let fqn = if d.kw == "import" { "-".to_string() } else { d.name.clone() };
        let (ln, col) = crate::parser::unpack_pos(if line != 0 { line } else { d.line });
        self.diags.push(Diag {
            file: d.file.clone(),
            code: code.into(),
            fqn,
            line: ln,
            col,
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
        self.check_layer_directions();
        self.check_impls();
        self.check_field_method();
        self.check_statics();
        self.check_tests();
        self.check_integration_files();
        self.check_visibility();
        self.check_linear();
        self.check_linear_type_args();
        self.check_derive();
        self.check_impl_requires();
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
                        self.check_const_range(init, &d.ret, i);
                    }
                }
                "type" => {
                    let tys: Vec<V> = d.fields.iter().map(|f| f.1.clone()).collect();
                    self.check_header_types(i, &tys);
                    self.check_layout_c(i);
                }
                "enum" => {
                    let tys: Vec<V> = d.variants.iter().flat_map(|v| v.1.clone()).collect();
                    self.check_header_types(i, &tys);
                    self.check_layout_c(i);
                }
                // xref.py の ref (未知の型・トレイト参照) のうち、今まで見ていなかったヘッダ位置。
                "extern" => {
                    let mut tys: Vec<V> = d.params.iter().map(|p| p.1.clone()).collect();
                    tys.push(d.ret.clone());
                    self.check_header_types(i, &tys);
                    self.check_extern_sig(i);
                }
                "trait" => self.check_trait_refs(i),
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
            if !self.module_may_use_unsafe(&self.def(i).module) {
                self.check_unsafe_surface(i);
            }
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
                let has_drop = d.derive.iter().any(|x| x == "Drop") || user.iter().any(|&j| {
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
            // 3.2: ジェネリクスのトレイト境界の名前が解決できるか (xref.py の ref の移植の一部)。
            self.check_generic_bounds(i);
        }
    }

    /// ジェネリクスのトレイト境界 (`[H: core.hash.Hasher]` など) のトレイト名が解決できるか
    /// (xref.py の ref の移植の一部)。裸の名前は曖昧の可能性があるので黙る。
    fn check_generic_bounds(&mut self, di: usize) {
        let gs = self.def(di).generics.clone();
        for (kind, _, bound) in &gs {
            if kind != "type" {
                continue;
            }
            for b in items(bound) {
                let f = items(b);
                if f.is_empty() {
                    continue;
                }
                let base = st(&f[0]);
                if base.contains('.') && !self.p.types.contains_key(&base) {
                    self.diag("E1104", format!("unknown trait {}", base), di, 0);
                }
                if f.len() > 1 {
                    self.check_dotted_types(di, &f[1]);
                }
            }
        }
    }

    /// トレイトのヘッダの参照を検査する (xref.py の ref のうち、これまで ksn check が
    /// 見ていなかった部分): `requires` のスーパートレイトとその型引数、トレイトメソッドの
    /// シグネチャの型。メソッドのジェネリクス (裸の名前) は Def に残らないため、
    /// ドット付きの名前だけを検査する。
    fn check_trait_refs(&mut self, di: usize) {
        let req = self.def(di).requires.clone();
        for (base, args) in &req {
            if base.contains('.') && !self.p.types.contains_key(base) {
                self.diag("E1104", format!("unknown trait {}", base), di, 0);
            }
            self.check_dotted_types(di, args);
        }
        let sigs = self.def(di).trait_sigs.clone();
        for (_, params, ret) in &sigs {
            for (_, ty) in params {
                self.check_dotted_types(di, ty);
            }
            self.check_dotted_types(di, ret);
        }
    }

    /// 型 V の中のドット付き名前付き型が定義されているか (E1102)、`dyn` のトレイトが
    /// 未定義なら E1104。裸の名前は (ジェネリクス引数などの可能性があるので) 検査しない。
    fn check_dotted_types(&mut self, di: usize, t: &V) {
        match tag(t).as_str() {
            "named" => {
                let nm = st(tel(t, 1));
                if nm.contains('.') && !self.p.types.contains_key(&nm) {
                    self.diag("E1102", format!("unknown type {}", nm), di, 0);
                }
                for a in items(tel(t, 2)) {
                    if matches!(a, V::L(_)) {
                        self.check_dotted_types(di, a);
                    }
                }
            }
            "dyn" => {
                let f = items(tel(t, 1));
                if !f.is_empty() {
                    let base = st(&f[0]);
                    if base.contains('.') && !self.p.types.contains_key(&base) {
                        self.diag("E1104", format!("unknown trait {}", base), di, 0);
                    }
                    if f.len() > 1 {
                        self.check_dotted_types(di, &f[1]);
                    }
                }
            }
            "ref" | "ptr" => self.check_dotted_types(di, tel(t, 2)),
            "slice" | "array" => self.check_dotted_types(di, tel(t, 1)),
            "tuple" => {
                for x in items(tel(t, 1)) {
                    self.check_dotted_types(di, x);
                }
            }
            "fn" => {
                for x in items(tel(t, 1)) {
                    self.check_dotted_types(di, x);
                }
                self.check_dotted_types(di, tel(t, 2));
            }
            _ => {}
        }
    }

    /// 8.2 の依存方向 (層規則) を検査する (`spec/tools/xref.py` の dep の移植)。
    /// `core` / `alloc` / `std` を最上位成分に持つ定義が、より上位の層の型・トレイトを
    /// 参照していれば E1118。方向は `std` → `alloc` → `core`。本体に取り込んだ標準
    /// ライブラリの定義と、層の名前空間で定義されたモジュール (ユーザーパッケージの
    /// `src/core/` 配下など) の両方が対象。ユーザー定義は層を持たないので、それ以外の
    /// 最上位成分のときは黙る (8.1。ユーザーは3層すべてを参照できる)。
    fn check_layer_directions(&mut self) {
        let p = self.p;
        for i in 0..p.defs.len() {
            let d = &p.defs[i];
            // `impl` の名前はヘッダ行そのもの、`import` は xref.py も検査外 (3.5)。
            if matches!(d.kw.as_str(), "import" | "impl") {
                continue;
            }
            let own_root = d.name.split('.').next().unwrap_or("");
            let Some(own) = layer_rank(own_root) else { continue };
            let mut refs: BTreeSet<String> = BTreeSet::new();
            for (kind, _, bound) in &d.generics {
                if kind != "type" {
                    continue;
                }
                for b in items(bound) {
                    if matches!(b, V::L(_)) {
                        collect_ref_fqns(b, &mut refs);
                    }
                }
            }
            for (_, t) in &d.params {
                collect_ref_fqns(t, &mut refs);
            }
            collect_ref_fqns(&d.ret, &mut refs);
            for (_, t, _) in &d.fields {
                collect_ref_fqns(t, &mut refs);
            }
            for (_, ts) in &d.variants {
                for t in ts {
                    collect_ref_fqns(t, &mut refs);
                }
            }
            for (base, args) in &d.requires {
                if base.contains('.') {
                    refs.insert(base.clone());
                }
                collect_ref_fqns(args, &mut refs);
            }
            // fn ヘッダの `impls <トレイト>` も層の参照。xref.py は定義テキスト全体を
            // TYPEREF で見るので、ヘッダに書かれた impls も依存方向の対象になる (8.2)。
            if d.impls_trait.contains('.') {
                refs.insert(d.impls_trait.clone());
            }
            if let Some(init) = &d.init {
                collect_ref_fqns(init, &mut refs);
            }
            if let Some(body) = &d.body {
                collect_ref_fqns(body, &mut refs);
            }
            for r in &refs {
                let Some(rr) = r.split('.').next() else { continue };
                let Some(rank) = layer_rank(rr) else { continue };
                // xref.py と同じく、解決できる参照だけが依存の証拠。未解決の参照は黙る。
                if rank > own && p.types.contains_key(r) {
                    self.diag(
                        "E1118",
                        format!(
                            "{} must not depend on {} ({}); the dependency direction is std -> alloc -> core (8.2)",
                            own_root, rr, r
                        ),
                        i,
                        0,
                    );
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
        // (トレイト, owner) -> その impl 行の def 番号。メソッドの効果を impl の実引数で実体化する (E4021)。
        let mut impl_of: HashMap<(String, String), usize> = HashMap::new();
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
            // トレイト側に置いたメソッドの所有者は `<トレイトFQN>.<型FQN>` (基本型は型名。3.2)。
            let alt_owner = format!("{}.{}", tb, tgt);
            // 同じ (トレイト, 型) は型引数が違っても 1 つ (3.2)。
            let tkey = tb.clone();
            if !seen.insert((tkey.clone(), tgt.clone())) {
                self.diag("E1114", format!("duplicate impl of {} for {}", tkey, tgt), i, 0);
            }
            let want = req.get(&tb).cloned().unwrap_or_default();
            let mut have = declared.get(&(tkey.clone(), owner.clone())).cloned().unwrap_or_default();
            if let Some(m) = declared.get(&(tkey.clone(), alt_owner.clone())) {
                // 型側とトレイト側の両方に同じ (トレイト, 型) の impl のメソッドがある (3.2)。
                if !have.is_empty() && owner != alt_owner {
                    self.diag(
                        "E1114",
                        format!("duplicate impl of {} for {}: methods are placed both on the type and on the trait side", tkey, tgt),
                        i,
                        0,
                    );
                }
                have.extend(m.iter().cloned());
            }
            impl_of.entry((tkey.clone(), owner.clone())).or_insert(i);
            impl_of.entry((tkey.clone(), alt_owner.clone())).or_insert(i);
            covered.insert((tkey.clone(), owner));
            covered.insert((tkey, alt_owner));
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
        self.check_impl_method_effects(&impl_of);
    }

    /// 4.3 / 5.1: impl のメソッドの効果はトレイト宣言の効果を超えない (E4021)。
    /// `uses`: impl の具体的な効果は、トレイトの宣言の `uses` のいずれかに覆われる (階層の部分型)。
    /// `panics`: トレイトが `panics never` なら impl も `never` (`never ⊑ may`)。
    /// トレイトの効果変数は impl 行の実引数で置き換えて比べる。束縛できない変数が残るとき、
    /// 効果変数を含む impl の効果、宣言の無いトレイトのメソッドでは黙る (誤検出を出さない)。
    /// 推論された関数は効果が宣言されていないので対象外。トレイトにメソッド単位の `stack` は無いので、
    /// stack の上界の規則は設けない。
    fn check_impl_method_effects(&mut self, impl_of: &HashMap<(String, String), usize>) {
        let mut found: Vec<(usize, String)> = vec![];
        for &i in &self.user {
            let d = self.def(i);
            if d.kw != "fn" || !d.has_impls || d.inferred {
                continue;
            }
            let owner = d.name.rsplit_once('.').map(|(o, _)| o.to_string()).unwrap_or_default();
            let Some(&ii) = impl_of.get(&(d.impls_trait.clone(), owner)) else { continue };
            let Some(&ti) = self.p.types.get(&d.impls_trait) else { continue };
            let td = self.def(ti);
            if td.kw != "trait" {
                continue;
            }
            let meth = d.name.rsplit('.').next().unwrap_or("");
            let targs = items(&self.def(ii).impl_targs).to_vec();
            let Some((tu, tp)) = trait_effect_bounds(td, meth, &targs) else { continue };
            let tname = format!("{}.{}", td.name, meth);
            if let Some(tu) = tu {
                for e in &d.uses {
                    if e.chars().next().map_or(false, |c| c.is_lowercase()) && !covers(&tu, e) {
                        found.push((
                            i,
                            format!(
                                "{} uses effect `{}` not declared by trait method {}",
                                d.name, e, tname
                            ),
                        ));
                        break;
                    }
                }
            }
            if let Some(tp) = tp {
                if d.panics.as_deref() == Some("may") && tp == "never" {
                    found.push((
                        i,
                        format!("{} may panic but trait method {} declares `panics never`", d.name, tname),
                    ));
                }
            }
        }
        for (i, msg) in found {
            self.diag("E4021", msg, i, 0);
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
            // 4.3: `From` は `enum` だけで、型パラメータを持たず、ペイロードがちょうど 1 つのバリアントが要る (E2050)。
            if derived.contains("From") {
                let ok = d.kw == "enum"
                    && !d.generics.iter().any(|g| g.0 == "type")
                    && d.variants.iter().any(|v| v.1.len() == 1);
                if !ok {
                    let why = if d.kw != "enum" {
                        "only an enum can derive From"
                    } else if d.generics.iter().any(|g| g.0 == "type") {
                        "an enum with type parameters cannot derive From"
                    } else {
                        "the enum has no variant with exactly one payload field"
                    };
                    self.diag("E2050", format!("derive [From] on {}: {}", d.name, why), i, 0);
                }
            }
            // 4.3 / 5.3: `Drop` は反復で破棄できる形の型にだけ (E2051)。`linear` 型・手書きの `Drop` との併用も不可。
            if derived.contains("Drop") {
                let mutual = user_types.iter().any(|m| *m != d.name && self.type_reaches(&d.name, m) && self.type_reaches(m, &d.name));
                let mut why = crate::derive_drop::problems(self.p, &d, mutual);
                let manual = self.user.iter().any(|&j| {
                    let x = self.def(j);
                    x.kw == "impl" && x.impl_trait == "core.ops.Drop" && ty_mentions(&x.impl_for, &d.name)
                });
                if manual {
                    why.push("the type also has an explicit core.ops.Drop impl (a type has one Drop)".to_string());
                }
                for w in why {
                    self.diag("E2051", format!("derive [Drop] on {}: {}", d.name, w), i, 0);
                }
            }
            // 4.3: derive した各トレイト (Copy 以外) は、フィールド・ペイロードの名義型もそのトレイトを実装していること
            // (基本型・参照・タプル・配列は組み込み実装、型パラメータは境界が担うので見ない)。
            {
                let tys: Vec<V> = if d.kw == "type" {
                    d.fields.iter().map(|f| f.1.clone()).collect()
                } else {
                    d.variants.iter().flat_map(|v| v.1.clone()).collect()
                };
                let params: HashSet<String> = d.generics.iter().map(|g| g.1.clone()).collect();
                for dv in d.derive.clone() {
                    if dv == "Copy" || dv == "From" || dv == "Drop" {
                        continue;
                    }
                    let Some(&tr) = short_to_fqn.get(dv.as_str()) else { continue };
                    let mut missing: Vec<String> = vec![];
                    for t in &tys {
                        let mut named = vec![];
                        Self::named_in(t, &mut named, tr == "core.ops.Clone" || tr == "alloc.alloc.TryClone");
                        for n in named {
                            // 型引数のアロケータ (`Vec<T, std.alloc.System>` の `System`) は値の部品ではないので見ない。
                            let is_allocator = self.p.impls.contains(&("alloc.alloc.Allocator".to_string(), n.clone()));
                            if params.contains(&n) || n == d.name || n == "Option" || n == "Result" || is_allocator {
                                continue;
                            }
                            if self.p.types.contains_key(&n) && !self.implements(&n, tr) && !missing.contains(&n) {
                                missing.push(n);
                            }
                        }
                    }
                    for n in missing {
                        self.diag("E2036", format!("derive [{}] on {}: the field type {} does not implement {}", dv, d.name, n, tr), i, 0);
                    }
                }
            }
            // 4.3: `ToJson` / `FromJson` は、フィールドの型が JSON の形を持つときだけ (E2036)。
            for (tr, why) in crate::derive_json::problems(self.p, &d) {
                self.diag("E2036", format!("derive [{}] on {}: {}", tr, d.name, why), i, 0);
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

    /// 3.2 / 4.3: `impl Tr for X` は、`Tr` の `requires` (スーパートレイト) を `X` が満たすこと (E2035)。
    /// `derive` の `requires` と同じ規則 (`Eq` の `impl` には `PartialEq<Self>` が要る)。手書きの `impl` は
    /// 対象型が宣言 (`p.types`) を持つときだけ見る (組み込みの型への `impl` は組み込みの実装が担う)。
    fn check_impl_requires(&mut self) {
        for i in self.user.clone() {
            let d = self.def(i);
            if d.kw != "impl" {
                continue;
            }
            let target = impl_target(&d.impl_for);
            if !self.p.types.contains_key(&target) {
                continue;
            }
            let Some(&ti) = self.p.types.get(&d.impl_trait) else { continue };
            for (sup, _) in self.p.defs[ti].requires.clone() {
                let Some(sf) = resolve_trait_fqn(self.p, &sup) else { continue };
                if !self.implements(&target, &sf) {
                    let short = sf.rsplit('.').next().unwrap_or(&sf).to_string();
                    let msg = format!(
                        "impl {} for {} requires `{}`: {} must implement it too (derive [{}] or write `impl {} for {}`)",
                        d.impl_trait, target, sf, target, short, sf, target
                    );
                    self.diag("E2035", msg, i, 0);
                }
            }
        }
    }

    /// 利用者の型 `from` のフィールドをたどって型 `to` に届くか (`from` 自身から始め、1 歩以上)。
    fn type_reaches(&self, from: &str, to: &str) -> bool {
        let mut seen: HashSet<String> = HashSet::new();
        let mut stack = vec![from.to_string()];
        while let Some(n) = stack.pop() {
            let Some(&i) = self.p.types.get(&n) else { continue };
            if !self.user.contains(&i) {
                continue;
            }
            let d = self.def(i);
            let tys: Vec<&V> = if d.kw == "type" {
                d.fields.iter().map(|f| &f.1).collect()
            } else {
                d.variants.iter().flat_map(|v| v.1.iter()).collect()
            };
            let mut names = HashSet::new();
            for t in tys {
                collect_ty_names(t, &mut names);
            }
            for m in names {
                if m == to {
                    return true;
                }
                if seen.insert(m.clone()) {
                    stack.push(m);
                }
            }
        }
        false
    }

    /// 型の中に現れる名義型の FQN (参照・タプル・配列・スライス・型引数を通して)。
    /// `shared_ref_is_leaf` が真なら、`&T` (共有参照) は中を見ない (`&T` は常に `Clone` / `Copy`。4.2)。
    fn named_in(t: &V, out: &mut Vec<String>, shared_ref_is_leaf: bool) {
        match tag(t).as_str() {
            "named" => {
                out.push(st(tel(t, 1)));
                for a in items(tel(t, 2)) {
                    if matches!(a, V::L(_)) {
                        Self::named_in(a, out, shared_ref_is_leaf);
                    }
                }
            }
            "ref" => {
                if !(shared_ref_is_leaf && matches!(tel(t, 1), V::B(false))) {
                    Self::named_in(tel(t, 2), out, shared_ref_is_leaf)
                }
            }
            "array" | "slice" => Self::named_in(tel(t, 1), out, shared_ref_is_leaf),
            "tuple" => items(tel(t, 1)).iter().for_each(|x| Self::named_in(x, out, shared_ref_is_leaf)),
            _ => {}
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

    /// 4.4・8.3: 定義の型 (フィールド・バリアントのペイロード・仮引数・戻り値) に書かれた型に、
    /// 線形な型を `linear` でない型パラメータへの実引数として渡しているものがないか (`Vec<TcpStream, A>` など)。
    fn check_linear_type_args(&mut self) {
        for i in self.user.clone() {
            let d = self.def(i);
            let mut tys: Vec<V> = d.fields.iter().map(|f| f.1.clone()).collect();
            tys.extend(d.variants.iter().flat_map(|v| v.1.iter().cloned()));
            tys.extend(d.params.iter().map(|p| p.1.clone()));
            tys.push(d.ret.clone());
            let gn = d.gnames();
            for t in tys {
                if !is_nil(&t) && !self.is_generic(&t, &gn) {
                    self.check_ty_linear_args(&t, i, 0);
                }
            }
        }
    }

    /// 型 `t` の中の、非線形の型パラメータに線形な型を渡している適用を E3006 にする。
    fn check_ty_linear_args(&mut self, t: &V, di: usize, line: usize) {
        match tag(t).as_str() {
            "named" => {
                let n = st(tel(t, 1));
                let args: Vec<V> = items(tel(t, 2)).iter().filter(|a| matches!(a, V::L(_))).cloned().collect();
                if let Some(&ti) = self.p.types.get(&n) {
                    let d = self.def(ti);
                    let params: Vec<&(String, String, V)> = d.generics.iter().filter(|g| g.0 == "type").collect();
                    if params.len() == args.len() {
                        for (g, a) in params.iter().zip(&args) {
                            let declared_linear = items(&g.2).iter().any(|b| matches!(b, V::S(x) if x == "linear"));
                            if !declared_linear && self.is_linear(a) {
                                self.diag(
                                    "E3006",
                                    format!(
                                        "linear type {} is given for the type parameter `{}` of {}, which is not declared `linear` (4.4)",
                                        if tag(a) == "named" { st(tel(a, 1)) } else { "(tuple)".to_string() },
                                        g.1,
                                        n
                                    ),
                                    di,
                                    line,
                                );
                            }
                        }
                    }
                }
                for a in &args {
                    self.check_ty_linear_args(a, di, line);
                }
            }
            "ref" | "ptr" => self.check_ty_linear_args(tel(t, 2), di, line),
            "slice" | "array" => self.check_ty_linear_args(tel(t, 1), di, line),
            "tuple" => {
                for x in items(tel(t, 1)) {
                    self.check_ty_linear_args(x, di, line);
                }
            }
            _ => {}
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
                    if dv == "Clone" || dv == "Copy" || dv == "Drop" {
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

    /// 3.2 / 3.6: `tests/` の統合テストの規則 (E1120) と、`src/` が予約名前空間
    /// `<パッケージ>.tests` を使っていないこと (E1121)。
    fn check_integration_files(&mut self) {
        let mut reported: BTreeSet<String> = BTreeSet::new();
        for &i in &self.user.clone() {
            let d = self.def(i);
            if self.integration_files.contains(&d.file) {
                if d.kw == "extern" {
                    self.diag("E1120", format!("`extern` is not allowed in tests/ ({})", d.name), i, 0);
                } else if !matches!(d.kw.as_str(), "import" | "impl" | "test") && header_is_pub(&d.header) {
                    self.diag("E1120", format!("{} in tests/ must not be `pub` (3.2)", d.name), i, 0);
                }
            } else if d.kw != "import" && d.module.split('.').nth(1) == Some("tests") && reported.insert(d.module.clone()) {
                self.diag(
                    "E1121",
                    format!("module {} is reserved for integration tests (tests/); rename it (3.6)", d.module),
                    i,
                    0,
                );
            }
        }
    }

    /// 3.1: 可視性は2段階 (省略 = 同一モジュール内、`pub` = 全パッケージ)。別モジュールの
    /// 非 `pub` な定義を FQN (または `import` 済みの短縮形) で参照すると E1119。
    /// 名前ベースの近似: 解決できる fn / 型 / const / extern の参照だけを見る。メソッド呼び出し
    /// (`x.m()`) とフィールドは型が要るので見ない (黙る)。標準ライブラリは対象外。
    fn check_visibility(&mut self) {
        let p = self.p;
        let mut tails: HashMap<&str, HashMap<String, String>> = HashMap::new();
        for d in &p.defs {
            if d.kw == "import" {
                if let Some(t) = d.name.rsplit('.').next() {
                    tails.entry(d.file.as_str()).or_default().insert(t.to_string(), d.name.clone());
                }
            }
        }
        for &i in &self.user.clone() {
            let d = self.def(i);
            if matches!(d.kw.as_str(), "import" | "impl") {
                continue;
            }
            let own_root = d.module.split('.').next().unwrap_or("");
            let in_std_layer = matches!(own_root, "core" | "alloc" | "std");
            // 3.1: `impls` メソッドの可視性はトレイトと型の小さい方に自動で従うので、`pub` は書けない。
            if d.kw == "fn" && d.has_impls && header_is_pub(&d.header) {
                self.diag("E1123", format!("{} is an `impls` method: its visibility follows the trait and the type; remove `pub` (3.1)", d.name), i, 0);
            }
            let mut refs: Vec<(String, usize)> = vec![];
            let mut hdr: BTreeSet<String> = BTreeSet::new();
            for (kind, _, bound) in &d.generics {
                if kind == "type" {
                    for b in items(bound) {
                        if matches!(b, V::L(_)) {
                            collect_ref_fqns(b, &mut hdr);
                        }
                    }
                }
            }
            for (_, t) in &d.params {
                collect_ref_fqns(t, &mut hdr);
            }
            collect_ref_fqns(&d.ret, &mut hdr);
            for (_, t, _) in &d.fields {
                collect_ref_fqns(t, &mut hdr);
            }
            for (_, ts) in &d.variants {
                for t in ts {
                    collect_ref_fqns(t, &mut hdr);
                }
            }
            for (base, args) in &d.requires {
                hdr.insert(base.clone());
                collect_ref_fqns(args, &mut hdr);
            }
            if d.has_impls {
                hdr.insert(d.impls_trait.clone());
            }
            refs.extend(hdr.into_iter().map(|r| (r, 0)));
            if let Some(init) = &d.init {
                collect_vis_refs(init, &mut refs);
            }
            if let Some(body) = &d.body {
                collect_vis_refs(body, &mut refs);
            }
            let imp = tails.get(d.file.as_str());
            let mut seen: BTreeSet<(String, usize)> = BTreeSet::new();
            for (r, line) in refs {
                let segs: Vec<&str> = r.split('.').collect();
                let mut chains = vec![segs.iter().map(|x| x.to_string()).collect::<Vec<_>>()];
                if let Some(full) = imp.and_then(|m| m.get(segs[0])) {
                    let mut c: Vec<String> = full.split('.').map(String::from).collect();
                    c.extend(segs[1..].iter().map(|x| x.to_string()));
                    chains.push(c);
                }
                for chain in chains {
                    // 8.2: `std.sys.*` は `core` / `alloc` / `std` の外から参照できない。
                    if !in_std_layer && chain.len() >= 2 && chain[0] == "std" && chain[1] == "sys" {
                        if seen.insert((chain.join("."), line)) {
                            self.diag("E1124", format!("{} is internal to the standard library (std.sys.* may only be referenced from core / alloc / std, 8.2)", chain.join(".")), i, line);
                        }
                        break;
                    }
                    let hit = (1..=chain.len()).rev().find_map(|k| {
                        let n = chain[..k].join(".");
                        let j = p.funcs.get(&n).or_else(|| p.types.get(&n)).or_else(|| p.consts.get(&n)).or_else(|| p.sigs.get(&n))?;
                        Some((n, *j))
                    });
                    let Some((n, j)) = hit else { continue };
                    let t = &p.defs[j];
                    if t.has_impls
                        || matches!(t.module.split('.').next(), Some("core" | "alloc" | "std"))
                        || header_is_pub(&t.header)
                    {
                        break;
                    }
                    // 同一モジュール内の参照は可視。`pub` な定義のヘッダが非公開の型を挙げる誤りは lint の E1010 が見る。
                    if t.module == d.module {
                        break;
                    }
                    if seen.insert((n.clone(), line)) {
                        self.diag("E1119", format!("{} is private to module {} (add `pub` to use it from {})", n, t.module, d.module), i, line);
                    }
                    break;
                }
            }
        }
        // `import std.sys.*` も同じ (3.5 / 8.2)。
        for &i in &self.user.clone() {
            let d = self.def(i);
            if d.kw == "import" && d.name.starts_with("std.sys.") && !matches!(d.module.split('.').next(), Some("core" | "alloc" | "std")) {
                self.diag("E1124", format!("{} is internal to the standard library (std.sys.* may only be referenced from core / alloc / std, 8.2)", d.name), i, 0);
            }
        }
    }

    /// 3.1: メソッド呼び出し `x.m()` とフィールド参照 `x.f` の可視性 (型推論が記録したレシーバの型を使う)。
    /// 別モジュールの型の非 `pub` なメソッドは E1119、非 `pub` なフィールドは E1122。構造体リテラルと
    /// 構造体パターンのフィールドも E1122 (名前から型を決める)。レシーバの型が決まらない所は E2044 が別に報告する。
    fn check_visibility_members(&mut self, log: &[crate::types::RecvSite]) {
        let p = self.p;
        let mut seen: BTreeSet<(usize, usize, String)> = BTreeSet::new();
        for r in log {
            if !self.user.contains(&r.writer) {
                continue;
            }
            let w = self.def(r.writer);
            let crate::types::Ty::Named(n, _) = r.recv.strip() else { continue };
            // 標準ライブラリの型のメソッドは飛ばす (可視性は規範の `.sig` が決める)。フィールドは標準の型も検査する:
            // `alloc.raw.Buf` の `initialized` のような内部の欄を利用者が外から書けてはならない (E1122)。
            if !r.field && matches!(n.split('.').next(), Some("core" | "alloc" | "std")) {
                continue;
            }
            let wmod = w.module.clone();
            let msg;
            let code;
            if r.field {
                let Some(&ti) = p.types.get(n.as_str()) else { continue };
                let td = &p.defs[ti];
                if td.kw != "type" || td.module == wmod {
                    continue;
                }
                match td.fields.iter().find(|f| f.0 == r.name) {
                    Some((_, _, false)) => {}
                    _ => continue,
                }
                code = "E1122";
                msg = format!("field {} of {} is private to module {} (add `pub` to the field to use it from {})", r.name, n, td.module, wmod);
            } else {
                let Some(&mi) = p.funcs.get(&format!("{}.{}", n, r.name)) else { continue };
                let md = &p.defs[mi];
                if md.module == wmod {
                    continue;
                }
                let visible = if md.has_impls {
                    // `impls` メソッド: トレイトと型の小さい方。
                    let tr_pub = p.types.get(&md.impls_trait).map_or(true, |&t| {
                        matches!(p.defs[t].module.split('.').next(), Some("core" | "alloc" | "std")) || header_is_pub(&p.defs[t].header)
                    });
                    let ty_pub = p.types.get(n.as_str()).map_or(true, |&t| header_is_pub(&p.defs[t].header));
                    tr_pub && ty_pub
                } else {
                    header_is_pub(&md.header)
                };
                if visible {
                    continue;
                }
                code = "E1119";
                msg = format!("method {} is private to module {} (add `pub` to use it from {})", md.name, md.module, wmod);
            }
            if seen.insert((r.writer, r.pos, r.name.clone())) {
                self.diag(code, msg, r.writer, r.pos);
            }
        }
        // 構造体リテラル `t.P { f: .. }` と構造体パターン `t.P { f: a }` のフィールド。
        for &i in &self.user.clone() {
            let d = self.def(i);
            let mut sites: Vec<(Vec<String>, String, usize)> = vec![];
            if let Some(b) = &d.body {
                collect_struct_fields(b, &mut sites);
            }
            if let Some(b) = &d.init {
                collect_struct_fields(b, &mut sites);
            }
            for (segs, field, pos) in sites {
                let fq = p.expand(&segs, &d.file).join(".");
                let Some(&ti) = p.types.get(&fq) else { continue };
                let td = &p.defs[ti];
                if td.kw != "type" || td.module == d.module {
                    continue;
                }
                if let Some((_, _, false)) = td.fields.iter().find(|f| f.0 == field) {
                    if seen.insert((i, pos, field.clone())) {
                        let msg = format!("field {} of {} is private to module {} (add `pub` to the field to use it from {})", field, fq, td.module, d.module);
                        self.diag("E1122", msg, i, pos);
                    }
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
            if self.integration_files.contains(&file) {
                // 統合テスト (3.2): 対象は定義ではなくテストファイル自身のモジュール。
                if target != d.module {
                    self.diag(
                        "E1116",
                        format!("integration test {} must be named {}.test_<name> (3.2)", name, d.module),
                        i,
                        0,
                    );
                }
                continue;
            }
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
            "dyn" => self.dyn_marker(tel(t, 1), "core.marker.Sync"),
            "ptr" => Some(false),
            "slice" | "array" => self.sync_v(tel(t, 1), gn, user_types, seen),
            "named" => {
                let n = st(tel(t, 1));
                if gn.contains(&n) {
                    return None;
                }
                // `alloc.boxed.Box<T, A>` は `T` と `A` に従う (`T` が `dyn Tr` のときは `Tr` の `requires`。5.5)。
                if n == "alloc.boxed.Box" {
                    let mut r = Some(true);
                    for x in items(tel(t, 2)).iter().filter(|x| matches!(x, V::L(_)) && tag(x) != "life") {
                        r = and3(r, self.sync_v(x, gn, user_types, seen));
                    }
                    return r;
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

    /// `dyn Tr` が `core.marker.Send` / `Sync` を満たすか: `Tr` の `requires` (スーパートレイトを推移的に辿る) に含むときだけ (5.5)。
    fn dyn_marker(&self, trait_ref: &V, marker: &str) -> Option<bool> {
        let f = items(trait_ref);
        let root = resolve_trait_fqn(self.p, &st(f.first()?))?;
        let mut seen = HashSet::new();
        let mut stack = vec![root];
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur.clone()) {
                continue;
            }
            let Some(td) = self.trait_def(&cur) else { continue };
            for (sup, _) in &td.requires {
                if sup == marker {
                    return Some(true);
                }
                if let Some(fq) = resolve_trait_fqn(self.p, sup) {
                    stack.push(fq);
                }
            }
        }
        Some(false)
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
            "dyn" => self.dyn_marker(tel(t, 1), "core.marker.Send"),
            "ptr" => Some(false),
            "slice" | "array" => self.send_v(tel(t, 1), gn, user_types, seen),
            "named" => {
                let n = st(tel(t, 1));
                if gn.contains(&n) {
                    return None;
                }
                // `alloc.boxed.Box<T, A>` は `T` と `A` に従う (`T` が `dyn Tr` のときは `Tr` の `requires`。5.5)。
                if n == "alloc.boxed.Box" {
                    let mut r = Some(true);
                    for x in items(tel(t, 2)).iter().filter(|x| matches!(x, V::L(_)) && tag(x) != "life") {
                        r = and3(r, self.send_v(x, gn, user_types, seen));
                    }
                    return r;
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
                // 型引数に線形を含む型も線形 (4.4): `Option<線形>`・`Result<線形, _>`・`Box<線形>` など。
                // 型引数が判断できないもの (Nil・型パラメータ) は None で、他に線形がなければ黙る。
                let arg_res = items(tel(t, 2))
                    .iter()
                    .fold(Some(false), |r, a| or3(r, self.linear_v(a, gn, user_types, seen)));
                if arg_res == Some(true) {
                    return Some(true);
                }
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
                or3(res, arg_res)
            }
            _ => None,
        }
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
            self.ty_names(t, di, &names, 0);
        }
    }

    fn ty_names(&mut self, t: &V, di: usize, names: &HashSet<String>, line: usize) {
        match tag(t).as_str() {
            "named" => {
                let nm = st(tel(t, 1));
                if PRELUDE_FQN.contains(&nm.as_str()) {
                    let bare = nm.rsplit('.').next().unwrap();
                    self.diag("E1110", format!("write the prelude name `{}` without its module path", bare), di, line);
                }
                if !nm.contains('.') {
                    if !names.contains(&nm) && !PRELUDE_TYPES.contains(&nm.as_str()) {
                        self.diag("E1102", format!("unknown type {}", nm), di, line);
                    }
                } else if !self.p.types.contains_key(&nm) {
                    self.diag("E1102", format!("unknown type {}", nm), di, line);
                }
                for (k, a) in items(tel(t, 2)).iter().enumerate() {
                    if tag(a) == "dyn" {
                        // `dyn` は `alloc.boxed.Box` の第1型引数にだけ置ける。ライフタイム境界は `'static` のみ (5.1)。
                        if nm == "alloc.boxed.Box" && k == 0 {
                            if st(tel(a, 2)) != "'static" {
                                self.diag("E2047", "`Box<dyn ..>` must be written `dyn Tr + 'static`".into(), di, line);
                            }
                            self.check_dyn_types(tel(a, 1), di);
                        } else {
                            self.diag("E2047", "`dyn` may appear only behind `&` / `&mut` or as the first type argument of `alloc.boxed.Box`".into(), di, line);
                            self.check_dyn_types(tel(a, 1), di);
                        }
                    } else if matches!(tag(a).as_str(), "named" | "ref" | "tuple" | "array" | "slice" | "prim") {
                        self.ty_names(a, di, names, line);
                    }
                }
            }
            "dyn" => {
                self.diag("E2047", "`dyn` may appear only behind `&` / `&mut` or as the first type argument of `alloc.boxed.Box`".into(), di, line);
                self.check_dyn_types(tel(t, 1), di);
            }
            "ref" if tag(tel(t, 2)) == "dyn" => self.check_dyn_types(tel(tel(t, 2), 1), di),
            "ref" | "ptr" => self.ty_names(tel(t, 2), di, names, line),
            "slice" | "array" => self.ty_names(tel(t, 1), di, names, line),
            "tuple" => {
                for x in items(tel(t, 1)) {
                    self.ty_names(x, di, names, line);
                }
            }
            "fn" => {
                for x in items(tel(t, 1)) {
                    self.ty_names(x, di, names, line);
                }
                self.ty_names(tel(t, 2), di, names, line);
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
        // xref.py の ref の移植: `dyn` のトレイトが未定義なら E1104。裸の名前は曖昧の
        // 可能性があるので黙る (resolve_trait_fqn の規約)。
        let Some(base) = resolve_trait_fqn(self.p, &nm) else {
            if nm.contains('.') && !self.p.types.contains_key(&nm) {
                self.diag("E1104", format!("unknown trait {}", nm), di, 0);
            }
            return;
        };
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
        // 関数ヘッダの引数名も、`import` の末尾名と同名ならシャドーイング (E1021)。本体の束縛と同じ扱い。
        let params: Vec<String> = d.params.iter().map(|p| p.0.clone()).collect();
        self.shadow_check(&params, &HashSet::new(), di, 0);
        self.cur_uses = if test { None } else { Some(d.uses.clone()) };
        self.cur_panics = if test { "may".into() } else { d.panics.clone().unwrap_or("may".into()) };
        self.in_const_fn = !test && d.is_const;
        self.unsafe_ok = self.module_may_use_unsafe(&d.module);
        let body = d.body.as_ref().unwrap();
        let names = self.def(di).gnames();
        self.check_body_types(body, di, &names);
        self.visit(body, &mut locals, di, test);
    }

    /// 本体の中の型 (`let x: T` の注釈、クロージャの引数と戻り値、`f::<T>` / `x.m::<T>` の型引数) の名前を
    /// ヘッダと同じ規則で確かめる (E1102・E1110・E2047)。木を前順で全部辿る。
    fn check_body_types(&mut self, e: &V, di: usize, names: &HashSet<String>) {
        match nname(e) {
            "Let" => {
                if let Some(t) = nf(e).get(2) {
                    if !matches!(t, V::Nil) {
                        self.ty_names(t, di, names, nline(e));
                    }
                }
            }
            "Closure" => {
                if let Some(ps) = nf(e).first() {
                    for p in items(ps) {
                        if let Some(t) = items(p).get(1) {
                            self.ty_names(t, di, names, nline(e));
                        }
                    }
                }
                if let Some(t) = nf(e).get(1) {
                    self.ty_names(t, di, names, nline(e));
                }
            }
            // 呼び出しの型引数は `Call` の中の `Path` と同じ V を持つので、`Path` と `MCall` だけ見る。
            "Path" | "MCall" => {
                let k = if nname(e) == "Path" { 1 } else { 3 };
                if let Some(ta) = nf(e).get(k) {
                    for a in items(ta) {
                        self.ty_names(a, di, names, nline(e));
                    }
                }
            }
            _ => {}
        }
        match e {
            V::Node(_, fs, _, _) => {
                for f in fs {
                    self.check_body_types(f, di, names);
                }
            }
            V::L(xs) => {
                for x in xs {
                    self.check_body_types(x, di, names);
                }
            }
            _ => {}
        }
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
                // 取り込み (`capture [&a, move b]`) は外側の局所でなければならない (仕様 4.4)。
                for c in items(&f[2]) {
                    let parts = items(c);
                    if parts.len() == 2 {
                        let name = st(&parts[1]);
                        if !locals.contains(&name) {
                            self.diag("E1101", format!("unresolved name {} in the capture list", name), di, nline(e));
                        }
                    }
                }
                let mut inner = locals.clone();
                let names: Vec<String> = items(&f[0]).iter().map(|p| st(&items(p)[0])).collect();
                self.shadow_check(&names, locals, di, nline(e));
                inner.extend(names);
                self.visit(&f[3], &mut inner, di, true);
            }
            "Path" => {
                let segs = strs(&f[0]);
                if !locals.contains(&segs[0]) {
                    if let Some(r) = self.resolve_path(&segs, di, nline(e)) {
                        // 列挙型の後ろに無い列挙子 (`E.Nope`) は未解決の名前 (E1101)。型の一部として解けない。
                        if self.is_enum_member_miss(&r) {
                            self.diag("E1101", format!("unresolved name {}: `{}` has no such variant", segs.join("."), r.name), di, nline(e));
                        }
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
                            let fqn = segs.join(".");
                            let shadow = self.p.resolve(&segs, &self.def(di).file).map(|_| fqn.as_str());
                            self.check_method_exists(&name, di, nline(fnn), shadow);
                        } else if !free && self.cur_uses.is_some() {
                            // 関数ポインタ型の引数の呼び出し: 型が宣言する効果を呼び出し側が覆うこと (5.1)。
                            if let Some((uses, pn)) = crate::infer::fn_param_effects(self.def(di), &segs[0]) {
                                let concrete: Vec<String> =
                                    uses.into_iter().filter(|u| u.chars().next().map_or(false, |c| c.is_lowercase())).collect();
                                let what = format!("the function pointer `{}`", segs[0]);
                                if let Some((c, m)) = self.violation_effs(&what, &concrete, Some(pn.as_str())) {
                                    self.diag(&c, m, di, nline(e));
                                }
                            }
                        }
                        return;
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
                self.check_method_exists(&name, di, nline(e), None);
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
                // 3.5: 同名の候補に生ポインタを持つもの (例: `alloc.alloc.Allocator` の `alloc`) があれば、
                // どの候補かは型が決まってから分かるので、名前で見て許可のないモジュールでは E5001。
                if !self.unsafe_ok && cands.iter().any(|c| sig_has_ptr(c)) {
                    self.diag(
                        "E5001",
                        format!("method `{}` takes or returns raw pointers; only allowed in modules listed in [unsafe] modules", name),
                        di,
                        nline(e),
                    );
                }
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
            "With" => self.visit_with(e, locals, di, free),
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
    /// `shadow_fqn` が `Some` のときは、レシーバ側の先頭セグメントが局所変数で、かつ
    /// `<局所名>.<残り>` がパッケージの関数 FQN として実在する場合で、その旨をメッセージに足す (3.2)。
    fn check_method_exists(&mut self, name: &str, di: usize, line: usize, shadow_fqn: Option<&str>) {
        if self.method_known(name) {
            return;
        }
        // 整数の `/` `%` に wrapping_ / saturating_ の形は無い。近い名前 (`rem` は f32/f64 のもの) を出すと誤誘導する。
        // `<` 等は partial_cmp から導く演算子で、メソッドではない (4.3)。`lt` を近い名前 (`ln`) に誤誘導しない。
        // `ne` も同じく演算子 (`!=`、`eq` の否定) で、メソッドではない (4.3)。
        let op = match name {
            "lt" => Some("<"),
            "le" => Some("<="),
            "gt" => Some(">"),
            "ge" => Some(">="),
            _ => None,
        };
        let hint: Option<String> = match name {
            "wrapping_div" | "saturating_div" => Some("integer `/` has no wrapping_ or saturating_ form; use `checked_div(b)` (Option: handle None with `match`)".into()),
            "wrapping_rem" | "saturating_rem" => Some("integer `%` has no wrapping_ or saturating_ form; use `checked_rem(b)` (Option: handle None with `match`)".into()),
            "ne" => Some("comparison is an operator, not a method; use `a != b` (the negation of `eq`)".into()),
            _ => op.map(|o| format!("comparison is an operator, not a method; use `a {} b` (PartialOrd, derived from partial_cmp)", o)),
        };
        let base_msg: String = if let Some(h) = hint {
            format!("unknown method `{}`; {}", name, h)
        } else {
            match self.nearest_method(name) {
                Some(n) => format!("unknown method `{}`; did you mean `{}`?", name, n),
                None => format!("unknown method `{}`", name),
            }
        };
        let msg = match shadow_fqn {
            Some(fqn) => {
                let local = fqn.split('.').next().unwrap_or(name);
                format!(
                    "{}; `{}` is a local variable, which shadows the module path (3.2); rename the local to call `{}`",
                    base_msg, local, fqn
                )
            }
            None => base_msg,
        };
        self.diag("E1108", msg, di, line);
    }

    fn method_known(&self, name: &str) -> bool {
        const DERIVED: &[&str] = &[
            "clone", "eq", "cmp", "partial_cmp", "hash", "debug_fmt", "display_fmt", "call", "call_mut",
            "call_once", "drop", "try_clone",
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
            // 廃止された `to_<整数型>` (4.5) は型検査 (types.rs `int_to_hint`) が書き換え先を案内するので、ここでは黙らせない。
            || name.strip_prefix("to_").is_some_and(|t| crate::types::INTS.contains(&t))
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
            // 短い名前は編集距離 1 の候補を出さない (`lt` → `ln` のような無関係な誤提案を防ぐ)。
            let limit = name.len().max(cand.len()) / 3;
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

    /// 列挙型の名前の後ろに、その型の列挙子でない成分が続く (`E.Nope`)。`resolve` は先頭の型まで解けて
    /// 残りを `rest` に返すので、型の後ろが列挙子でなければ未解決。
    fn is_enum_member_miss(&self, r: &Resolved) -> bool {
        match r.target {
            Target::Type(i) => !r.rest.is_empty() && self.def(i).kw == "enum",
            _ => false,
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
                self.diag("E1110", format!("write the prelude name `{}` without its module path", bare), di, nline(p));
            }
            let r = self.p.resolve(&segs, &self.def(di).file);
            // 型の名前だけを指すパターン (残りなし) か、列挙子・定数。型の後ろに続く成分は見ない (`E.Nope` は未解決)。
            let ok = matches!(r, Some(ref x) if matches!(x.target, Target::Variant(..) | Target::Const(_)) || (matches!(x.target, Target::Type(_)) && x.rest.is_empty()));
            if !ok {
                self.diag("E1101", format!("unresolved pattern {}", segs.join(".")), di, nline(p));
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

    /// 明示型引数がない呼び出しの、宣言のままの検査。効果変数・panic 変数は呼び出し元の束縛
    /// (`Fn` 境界のクロージャ・`Iterator` の impl・関数項目) で決まるので、ここでは見ない (型の検査が見る)。
    /// 束縛が分からない変数は、型の検査 (`callee_effects` の未束縛) が呼び出し元と比べる。
    fn violation(&self, callee: &Def) -> Option<(String, String)> {
        let uses: Vec<String> = callee.uses.iter().filter(|e| !is_effect_var(e)).cloned().collect();
        let panics = callee.panics.as_deref().filter(|p| !is_panic_var(p));
        self.violation_effs(&callee.name, &uses, panics)
    }

    /// 呼び出し側の `uses` / `panics` が、callee の効果 (5.1 の階層の部分型) を覆うか。
    fn violation_effs(&self, name: &str, uses: &[String], panics: Option<&str>) -> Option<(String, String)> {
        let cu = self.cur_uses.as_ref()?;
        for eff in uses {
            // 効果変数 (大文字) は、呼び出し先の実体化が分からないまま残ったもの。呼び出し元が同じ変数を
            // `uses` に持つときだけ覆う (`covers` は具体名だけを見る)。それ以外は fail-closed で E4001。
            let covered = if is_effect_var(eff) { cu.iter().any(|c| c == eff) } else { covers(cu, eff) };
            if !covered {
                return Some(("E4001".into(), format!("{} uses effect `{}` not declared by the caller", name, eff)));
            }
        }
        match panics {
            // 残った panic 変数 `P` は、呼び出し元が同じ `P: panics` を持つときだけ許す。
            Some(p) if is_panic_var(p) && self.cur_panics != p => Some((
                "E4002".into(),
                format!("{} may panic (panic variable `{}`) but the caller does not declare `panics {}`", name, p, p),
            )),
            Some("may") if self.cur_panics == "never" => Some((
                "E4002".into(),
                format!("{} may panic but the caller declares `panics never`", name),
            )),
            _ => None,
        }
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

    /// ① 型で一意に解決できたメソッド呼び出し (`s.m(..)`) の効果を、書き手 (呼び出しを含む関数) の
    /// 宣言と比べる。書き手が `test` のときは検査しない (既存の `effects` と同じ条件)。
    /// クロージャの中の呼び出しは ② が本体ごと見るので、ここでは扱わない。
    /// `MCall` 形の粗い近似 (`effects_all`) が同じ呼び出し箇所を既に報告していれば重複させない。
    fn check_resolved_methods(&mut self, calls: &[crate::types::ResolvedCall]) {
        for c in calls {
            if c.scope.is_some() {
                continue;
            }
            let w = self.def(c.writer);
            // 効果を推論する関数 (宣言のない `inferred`) が書き手でも解決先でも、宣言の効果ではないので検査しない (黙る)。
            if w.kw != "fn" || w.inferred || self.def(c.callee).inferred {
                continue;
            }
            let (uses, panics) = (w.uses.clone(), w.panics.clone().unwrap_or_else(|| "may".into()));
            // 呼び出し箇所の書き手の効果で検査する (検査のあと、元の状態に戻す)。
            let saved_u = self.cur_uses.replace(uses);
            let saved_p = std::mem::replace(&mut self.cur_panics, panics);
            let viol = match &c.dyn_eff {
                Some((name, u, pn)) => self.violation_effs(name, u, Some(pn.as_str())),
                None => self.violation_targs(self.def(c.callee), &c.targs),
            };
            self.cur_uses = saved_u;
            self.cur_panics = saved_p;
            let Some((code, msg)) = viol else { continue };
            let w = self.def(c.writer);
            let dup = self.diags.iter().any(|x| x.code == code && x.file == w.file && x.fqn == w.name && x.line == crate::parser::unpack_pos(c.line).0);
            if !dup {
                self.diag(&code, msg, c.writer, c.line);
            }
        }
    }

    /// 型推論が残した効果検査の義務 (`types::EffCheck`) を、呼び出し元の `uses` / `panics` で検査する。
    /// 従来の `effects` と同じ文面なので、同じ違反を両方が見つけたときは重複除去で 1 件になる。
    fn effects_checked(&mut self, ec: &crate::types::EffCheck) {
        let d = self.def(ec.di);
        if d.kw != "fn" {
            return;
        }
        self.cur_uses = Some(d.uses.clone());
        self.cur_panics = d.panics.clone().unwrap_or("may".into());
        let bad: Vec<Option<(String, String)>> =
            ec.cands.iter().map(|(n, u, p)| self.violation_effs(n, u, p.as_deref())).collect();
        if !bad.is_empty() && bad.iter().all(|b| b.is_some()) {
            let (c, m) = bad[0].clone().unwrap();
            self.diag(&c, m, ec.di, ec.line);
        }
    }

    // ================================================================ TypeCk
    fn typeck(&mut self) {
        let user = self.user.clone();
        for &i in &user {
            if self.def(i).kw == "trait" {
                self.check_trait_lifetimes(i);
            }
            if self.def(i).kw == "fn" {
                self.check_elision(i);
            }
            if matches!(self.def(i).kw.as_str(), "type" | "enum") {
                self.check_field_elision(i);
            }
        }
        // 型付きの式推論に基づく検査 (レシーバの型によるメソッドの存在・引数の個数と型)。
        // 型表は move の追跡 (`tc_fn`) も使うので、先に作る。
        let mut tk = crate::types::TypeCk::new(self.p);
        for &i in &user {
            let d = self.def(i);
            if (d.kw == "fn" && d.body.is_some()) || d.kw == "test" {
                tk.check_def(i);
            }
        }
        self.let_tys = std::mem::take(&mut tk.let_tys);
        self.mv_types = std::mem::take(&mut tk.tytab);
        self.call_res = std::rc::Rc::new(std::mem::take(&mut tk.call_res));
        for &i in &user {
            let d = self.def(i);
            if (d.kw == "fn" || d.kw == "test") && d.body.is_some() {
                self.tc_fn(i);
            }
        }
        let vis_log = std::mem::take(&mut tk.recv_log);
        self.check_visibility_members(&vis_log);
        tk.tytab = std::mem::take(&mut self.mv_types);
        // `a[i]` で Copy でない要素を値として取り出した (consume した) 場所を、型表で
        // 要素型を引いて E3002 にする (4.1)。型が分からない (Vec 等の基が Nil、型引数依存等)
        // ときは黙る。
        for (fqn, nid, text, line) in self.index_consumes.clone() {
            let Some(et) = tk.tytab.get(&(fqn.clone(), nid)) else { continue };
            // 基が参照 (`&[T]` / `&Vec<T>`) なら剥がして要素型を見る。
            let mut base = &et.ty;
            while let crate::types::Ty::Ref(_, inner) = base {
                base = inner.as_ref();
            }
            let el = match base {
                crate::types::Ty::Array(el, _) | crate::types::Ty::Slice(el) => Some(el.as_ref()),
                crate::types::Ty::Named(_, args) => args.first(),
                _ => None,
            };
            let Some(el) = el else { continue };
            if !self.is_copy_ty(el) {
                self.tdiag(
                    "E3002",
                    format!(
                        "cannot move out of `{}`: the element is not Copy; borrow it with `&{}` or use `get`",
                        &text, &text
                    ),
                    line,
                );
            }
        }
        for (fqn, nid_, text, line) in self.field_consumes.clone() {
            let Some(et) = tk.tytab.get(&(fqn.clone(), nid_)) else { continue };
            if !self.is_copy_ty(&et.ty) {
                if let Some(&di) = self.user.iter().find(|&&i| self.def(i).name == fqn) {
                    self.d = di;
                }
                self.tdiag(
                    "E3002",
                    format!("cannot move out of `{text}`: the element is behind an index and its field is not Copy; borrow it with `&{text}`"),
                    line,
                );
            }
        }
        // 計測 (隠し機能): `KSN_TYPE_COVERAGE=1` のときだけ stderr に被覆率を出す (typecov.rs)。
        if let Some(c) = tk.cov.take() {
            let top = std::env::var("KSN_TYPE_COVERAGE_TOP").ok().and_then(|v| v.parse().ok()).unwrap_or(15);
            crate::typecov::report(&c, top);
        }
        // 確認 (隠し機能): `KSN_DUMP_TYPES=1` のとき式 ID ごとの型表を stderr に出す (types.rs `dump_types`)。
        if std::env::var_os("KSN_DUMP_TYPES").is_some() {
            eprint!("{}", tk.dump_types());
        }
        // 4.6 (型表): 型推論が型を決めた式文の捨て。`ty_of` では決まらない形 (標準ライブラリの
        // ジェネリック型のメソッドの戻り値、`v.pop();` の `Option` など) をここで拾う。
        for &i in &user {
            let d = self.def(i);
            if (d.kw == "fn" && d.body.is_some()) || d.kw == "test" {
                self.d = i;
                for line in discarded_by_type(&tk, d) {
                    self.tdiag("E2042", "a value other than () is discarded; use core.mem.drop(x)".into(), line);
                }
            }
        }
        self.diags.extend(tk.diags);
        // 型で解決できた呼び出しの効果 (5.1 / 4.3): 局所変数レシーバの `s.m()` と、`Fn` 境界に渡した
        // クロージャから推論した効果変数・panic 変数。呼び出し元の宣言と比べ、全候補が違反するときだけ報告する。
        for ec in std::mem::take(&mut tk.eff_checks) {
            self.effects_checked(&ec);
        }
        // 型で解決できた呼び出し (メソッドを含む) の辺を、再帰・スタックの検査に加える。
        for (from, tos) in tk.edges {
            self.graph.entry(from).or_default().extend(tos);
        }
        // クロージャ節のフレーム見積り (E4012) のため、節名 → クロージャ AST の対応を引き継ぐ。
        self.closure_asts = tk.closure_asts;
        // 型で解決できた局所レシーバのメソッド呼び出し・`dyn` 越しの呼び出しの効果 (クロージャの効果は `eff_checks`)。
        self.check_resolved_methods(&tk.resolved_calls);
        // 従来の検査と新しい型推論が同じ誤りを同じ文面で報告したときは、1 件にまとめる。
        let mut seen = HashSet::new();
        self.diags.retain(|d| seen.insert((d.file.clone(), d.code.clone(), d.fqn.clone(), d.line, d.msg.clone())));
        self.check_recursion();
    }

    fn check_elision(&mut self, di: usize) {
        /// ヘッダの型パラメータ列 (`fn x.f['a, 'b, T](..)`) で宣言したライフタイムの名前 (`'a`)。
        fn lifetime_names(head: &str) -> Vec<String> {
            let Some(open) = head.find('[') else { return vec![] };
            let Some(close) = head[open..].find(']') else { return vec![] };
            head[open + 1..open + close].split(',').map(|x| x.trim()).filter(|x| x.starts_with('\'')).map(|x| x.to_string()).collect()
        }
        /// `txt` の中で、ライフタイム `lt` (`'a`) が識別子として現れる回数 (`'ab` は数えない)。
        fn count_lifetime(txt: &str, lt: &str) -> usize {
            let b = txt.as_bytes();
            txt.match_indices(lt)
                .filter(|(i, _)| b.get(i + lt.len()).map_or(true, |c| !(c.is_ascii_alphanumeric() || *c == b'_')))
                .count()
        }
        let d = self.def(di);
        let head = d.header.split(" uses ").next().unwrap_or("");
        if head.contains("['") {
            // 4.1: 省略が正規形のときのライフタイム宣言はエラー。
            let ret_txt = head.split("->").nth(1).unwrap_or("");
            let refs = d.params.iter().filter(|p| p.1.dump().contains("ref")).count();
            // 入力の位置に 2 回以上現れるライフタイム (2 つの入力を結ぶ: `v: &mut Vec<&'a T>, s: &'a T`) は、
            // 戻り値に現れなくても明示が要る (省略すると別々のライフタイムになる)。1 回だけなら省略が正規形。
            let params_txt = head.split("->").next().unwrap_or("");
            let params_txt = params_txt.split_once('(').map(|x| x.1).unwrap_or("");
            let ties_inputs = lifetime_names(head).iter().any(|lt| count_lifetime(params_txt, lt) >= 2);
            let redundant = if !ret_txt.contains('\'') { !ties_inputs } else { d.ret.dump().contains("ref") && refs == 1 };
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

    /// トレイトのメソッドの宣言 (本体なし) が、宣言していないライフタイム (`'a`) を使う。本体の流れは見られないので、
    /// 宣言の有無だけを見る。`program.rs` がメソッドごとの未宣言の名前を `field_lifetimes` に入れている。
    fn check_trait_lifetimes(&mut self, di: usize) {
        let d = self.def(di);
        let names: Vec<String> = d.field_lifetimes.iter().filter(|(_, l)| !l.is_empty()).map(|(m, l)| format!("{}: '{}", m, l.join(", '"))).collect();
        if names.is_empty() {
            return;
        }
        let msg = format!(
            "{}: lifetime of the returned reference is not declared (`{}`); declare it in the method's generics (for example `fn m['a](...)`)",
            d.name,
            names.join("`, `")
        );
        self.diag("E3020", msg, di, 0);
    }

    /// 4.1: `type` / `enum` のフィールド・バリアントの参照は、ライフタイムを省略できない (`'static` も明示)。
    fn check_field_elision(&mut self, di: usize) {
        let d = self.def(di);
        if d.elided_refs.is_empty() {
            return;
        }
        let msg = format!(
            "{}: lifetime of the reference in `{}` cannot be elided; declare it (for example `&'a T` with `['a]`)",
            d.name,
            d.elided_refs.join("`, `")
        );
        self.diag("E3020", msg, di, 0);
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
            // 配列は要素が Copy のときだけ Copy (`for x in a` で所有する配列を消費するため、4.1)。
            "array" => self.is_copy(tel(t, 1)),
            "named" => {
                let nm = st(tel(t, 1));
                if nm == "Option" || nm == "Result" {
                    return items(tel(t, 2)).iter().all(|a| self.is_copy(a));
                }
                // 型パラメータ: 境界に `core.ops.Copy` があれば Copy。ユーザー定義のトレイトの境界は
                // 上位トレイトに Copy を持ちうるので黙る (Copy とみなす)。
                if items(tel(t, 2)).is_empty() && self.names.contains(&nm) {
                    return self.generic_may_be_copy(&nm);
                }
                // 派生の `Copy` は型引数が `Copy` のときだけ (`impl[T: Copy] Copy for Box<T>`)。
                self.p.types.get(&nm).map_or(false, |&i| self.def(i).derive.iter().any(|x| x == "Copy"))
                    && items(tel(t, 2)).iter().all(|a| self.is_copy(a))
            }
            _ => true,
        }
    }

    /// 現在の定義の型パラメータ `nm` が Copy でありうるか (境界に Copy がある / 境界が分からない)。
    fn generic_may_be_copy(&self, nm: &str) -> bool {
        let d = self.def(self.d);
        for g in d.generics.iter().filter(|g| g.0 == "type" && g.1 == nm) {
            for b in items(&g.2) {
                let bn = match b {
                    V::L(bx) if !bx.is_empty() => st(&bx[0]),
                    V::S(x) => x.clone(),
                    _ => return true,
                };
                if bn == "Copy" || bn.ends_with(".Copy") {
                    return true;
                }
                // 標準ライブラリのトレイト以外 (ユーザー定義) は上位トレイトまで追わない: 黙る。
                if !bn.starts_with("core.") && !bn.starts_with("std.") && !bn.starts_with("alloc.") {
                    return true;
                }
            }
        }
        // `impl` の型パラメータなど、この定義の generics に無い名前は分からないので黙る。
        !d.generics.iter().any(|g| g.0 == "type" && g.1 == nm)
    }

    /// `Ty` が Copy か。`is_copy` の `Ty` 版。未知の型・型パラメータは黙る (true)。
    fn is_copy_ty(&self, t: &crate::types::Ty) -> bool {
        match t {
            crate::types::Ty::Unknown | crate::types::Ty::Var(_) | crate::types::Ty::Param(_) => true,
            crate::types::Ty::Never | crate::types::Ty::Unit | crate::types::Ty::Prim(_) => true,
            crate::types::Ty::Ref(mut_, _) => !mut_,
            crate::types::Ty::Tuple(xs) => xs.iter().all(|x| self.is_copy_ty(x)),
            crate::types::Ty::Named(nm, args) => {
                if nm == "Option" || nm == "Result" {
                    return args.iter().all(|a| self.is_copy_ty(a));
                }
                // 派生の `Copy` は型引数が `Copy` のときだけ (`impl[T: Copy] Copy for Box<T>`)。
                self.p
                    .types
                    .get(nm)
                    .map_or(false, |&i| self.def(i).derive.iter().any(|x| x == "Copy"))
                    && args.iter().all(|a| self.is_copy_ty(a))
            }
            crate::types::Ty::Array(el, _) | crate::types::Ty::Slice(el) => self.is_copy_ty(el),
            crate::types::Ty::Ptr(_, _) | crate::types::Ty::Const(_) | crate::types::Ty::Dyn(..) => true,
            crate::types::Ty::Closure { .. } => true,
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
            // `/` と `%` は 0 で割ると panic し、wrapping_/saturating_ の形は無い (num.sig)。
            // 具体的な置き換えと、関数ヘッダを `panics may` にする選択肢の両方を示す。
            let (cause, how, note) = match name {
                "neg" => (
                    "on overflow",
                    "write `x.checked_neg()` (Option: handle None with `match`) or `x.wrapping_neg()` (returns the integer)".to_string(),
                    String::new(),
                ),
                "div" | "rem" => (
                    "(division by zero or overflow)",
                    format!("write `a.checked_{n}(b)` (Option: handle None with `match`)", n = name),
                    format!(" (`wrapping_{n}` and `saturating_{n}` do not exist)", n = name),
                ),
                _ => (
                    "on overflow",
                    format!(
                        "write `a.checked_{n}(b)` (Option: handle None with `match`), `a.wrapping_{n}(b)` or `a.saturating_{n}(b)` (return the integer)",
                        n = name
                    ),
                    String::new(),
                ),
            };
            let msg = format!(
                "integer {} may panic {}; {}, or declare `panics may` on the function header{}",
                what, cause, how, note
            );
            self.tdiag("E4004", msg, line);
        }
    }

    /// `const` / `static` の初期化式に含まれる整数リテラル・定数式が、
    /// 型の範囲に収まるか検査する。範囲外は E2026 を報告する (仕様 3.7, 4.5)。
    /// 初期化式の中の配列の長さ (`[e; N]`) も、`usize` の幅で検査する (4.5)。
    fn check_const_range(&mut self, init: &V, declared: &V, di: usize) {
        // 宣言の型が整数の基本型なら、接尾辞のない整数リテラルはその型と見る (`const N: usize = 4294967296`)。
        let expect = if tag(declared) == "prim" {
            let name = st(tel(declared, 1));
            INT_PRIMS.contains(&name.as_str()).then_some(name)
        } else {
            None
        };
        let _ = self.const_eval(init, expect.as_deref(), di);
        self.check_array_lens(init, di);
    }

    /// `[e; N]` の長さ `N` (整数リテラル) が `usize` の範囲に収まるか検査する (E2026)。
    fn check_array_lens(&mut self, e: &V, di: usize) {
        if nname(e) == "ArrayRep" {
            if let Some(msg) = crate::types::array_len_problem(&nf(e)[1]) {
                self.diag("E2026", msg, di, nline(e));
            }
        }
        for c in children(e) {
            self.check_array_lens(c, di);
        }
    }

    /// 定数式を評価し、型の範囲を検査する。`expect` は接尾辞のない整数リテラルの型 (宣言の型)。
    /// 返り値: `Some((i128 の値, 型名))`、評価不能の場合は `None`。
    fn const_eval(&mut self, e: &V, expect: Option<&str>, di: usize) -> Option<(i128, String)> {
        let f = nf(e);
        match nname(e) {
            "Lit" => {
                if st(&f[0]) != "int" {
                    return None;
                }
                let text = st(&f[1]);
                let ty = if is_nil(&f[2]) {
                    expect?.to_string()
                } else {
                    st(&f[2])
                };
                // リテラルの範囲検査 (E2026)
                if let Some(msg) = crate::types::int_literal_problem(&text, &ty) {
                    self.diag("E2026", msg, di, nline(e));
                }
                let mag = text.trim_start_matches('-').replace('_', "");
                let digits: String = mag.chars().take_while(|c| c.is_ascii_digit()).collect();
                let value: i128 = digits.parse().ok()?;
                let neg = text.starts_with('-');
                Some((if neg { -value } else { value }, ty))
            }
            "Bin" => {
                let op = st(&f[0]);
                if !matches!(op.as_str(), "+" | "-" | "*" | "/" | "%") {
                    return None;
                }
                let (lv, lt) = self.const_eval(&f[1], expect, di)?;
                let (rv, _rt) = self.const_eval(&f[2], Some(lt.as_str()), di)?;
                let result = match op.as_str() {
                    "+" => lv.checked_add(rv)?,
                    "-" => lv.checked_sub(rv)?,
                    "*" => lv.checked_mul(rv)?,
                    "/" => {
                        if rv == 0 {
                            return None;
                        }
                        lv.checked_div(rv)?
                    }
                    "%" => {
                        if rv == 0 {
                            return None;
                        }
                        lv.checked_rem(rv)?
                    }
                    _ => return None,
                };
                // 結果の型は左辺の型
                if !self.value_in_range(result, &lt) {
                    self.diag(
                        "E2026",
                        format!("integer literal `{}` is out of range for `{}`", result, lt),
                        di,
                        nline(e),
                    );
                }
                Some((result, lt))
            }
            "Unary" => {
                if st(&f[0]) != "-" {
                    return None;
                }
                let (v, ty) = self.const_eval(&f[1], expect, di)?;
                Some((-v, ty))
            }
            _ => None,
        }
    }

    /// 値 `value` が型 `ty` の範囲に収まるか。`usize`/`isize` はターゲットの幅で見る (4.5)。
    fn value_in_range(&self, value: i128, ty: &str) -> bool {
        let bits: u32 = match ty {
            "i8" | "u8" => 8,
            "i16" | "u16" => 16,
            "i32" | "u32" => 32,
            "i128" | "u128" => 128,
            "isize" | "usize" => crate::ptrwidth::bits(),
            _ => return true,
        };
        let signed = ty.starts_with('i');
        if signed {
            value >= -(1i128 << (bits - 1)) && value <= (1i128 << (bits - 1)) - 1
        } else {
            let max = if bits == 128 { u128::MAX } else { (1u128 << bits) - 1 };
            value >= 0 && value <= max as i128
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

    /// 型が線形か (4.4): `linear` と宣言された型、または線形な値を含む型
    /// (フィールド・バリアントのペイロード・タプル要素・配列要素・型引数)。
    fn is_linear(&self, t: &V) -> bool {
        self.is_linear_in(t, &mut HashSet::new())
    }

    /// 分解の束縛へ流してよい型か: 汎用でない型、または線形と分かる汎用の型 (`T: linear` の `T` など)。
    /// 線形でない汎用の型は、従来どおり決まらない (Nil) として黙る。
    fn keeps_ty(&self, t: &V) -> bool {
        !self.is_generic(t, &self.names) || self.is_linear(t)
    }

    fn is_linear_in(&self, t: &V, seen: &mut HashSet<String>) -> bool {
        match tag(t).as_str() {
            "tuple" => items(tel(t, 1)).iter().any(|x| self.is_linear_in(x, seen)),
            "array" => self.is_linear_in(tel(t, 1), seen),
            "named" => {
                let name = st(tel(t, 1));
                // `Sender<T>` は `T` が線形でも線形にならない (落としてよい。キューの値の持ち主は受信側。5.5)。
                if name == "std.sync.Sender" {
                    return false;
                }
                // `T: linear` と宣言された型パラメータ (現在の関数の分) は線形 (4.4)。
                if items(tel(t, 2)).is_empty() && self.lin_gnames.contains(&name) && !self.p.types.contains_key(&name) {
                    return true;
                }
                if items(tel(t, 2)).iter().any(|a| matches!(a, V::L(_)) && self.is_linear_in(a, seen)) {
                    return true;
                }
                let Some(&i) = self.p.types.get(&name) else { return false };
                let d = self.def(i);
                if d.linear {
                    return true;
                }
                if !seen.insert(name) {
                    return false;
                }
                d.fields.iter().any(|f| self.is_linear_in(&f.1, seen))
                    || d.variants.iter().any(|v| v.1.iter().any(|x| self.is_linear_in(x, seen)))
            }
            _ => false,
        }
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
                let segs = strs(&f[0]);
                let Some(root) = env.get(&segs[0]) else { return V::Nil };
                // `s.f.g` (根が局所変数のフィールド道) は構造体定義のフィールドをたどる (4.4)。
                // 参照越しは自動参照外しでたどる。途中で分からなければ黙る (Nil)。
                let mut cur = root.clone();
                for seg in &segs[1..] {
                    if is_nil(&cur) {
                        return V::Nil;
                    }
                    let base = self.strip(&cur).clone();
                    cur = self.field_ty(&base, seg).unwrap_or(V::Nil);
                }
                cur
            }
            "Field" => {
                // 一時値のフィールド (`t.make().lin`、4.4)。受け手の型が決まり、
                // 構造体のフィールドとして解決できるときだけ型を返す (分からなければ黙る)。
                let recv = self.ty_of(&f[0], env);
                if is_nil(&recv) {
                    return V::Nil;
                }
                let base = self.strip(&recv).clone();
                match self.field_ty(&base, &st(&f[1])) {
                    Some(ft) if !self.is_generic(&ft, &self.names) => ft,
                    _ => V::Nil,
                }
            }
            "TupleE" => {
                // タプル式は要素の型がすべて決まるときだけ型を返す (4.4)。
                let mut els = vec![];
                for x in items(&f[0]) {
                    let t = self.ty_of(x, env);
                    if is_nil(&t) {
                        return V::Nil;
                    }
                    els.push(t);
                }
                V::L(vec![V::S("tuple".into()), V::L(els)])
            }
            "ArrayE" => {
                // 配列式は要素の型がすべて同じ型に決まるときだけ型を返す (4.4)。
                // 長さは型に入れない (型の長さは `ty_size` が既定値を使うので Nil のままでよい)。
                let xs = items(&f[0]);
                let Some(first) = xs.first() else { return V::Nil };
                let el = self.ty_of(first, env);
                if is_nil(&el) {
                    return V::Nil;
                }
                for x in &xs[1..] {
                    if !same_v(&self.ty_of(x, env), &el) {
                        return V::Nil;
                    }
                }
                V::L(vec![V::S("array".into()), el, V::Nil])
            }
            "Index" => {
                // `a[i]` は配列・スライスの要素の型 (4.4)。`types.rs` の Index と同じく、
                // 添字の型は見ない。`Vec` など別の型や、要素の型が決まらない / 型引数に依る
                // ときは黙る (Nil)。
                let base = self.ty_of(&f[0], env);
                if is_nil(&base) {
                    return V::Nil;
                }
                let b = self.strip(&base).clone();
                if !matches!(tag(&b).as_str(), "array" | "slice") {
                    return V::Nil;
                }
                let el = tel(&b, 1).clone();
                if is_nil(&el) || self.is_generic(&el, &self.names) {
                    return V::Nil;
                }
                el
            }
            "MCall" => {
                // `x.m(..)` の戻り値 (4.4)。レシーバの型が型引数なしのユーザー型に決まり、
                // その型のメソッドが一意に定まるときだけ戻り値の型を返す。
                // 戻り値が型引数・`Self` を含むときは黙る。
                let recv = self.ty_of(&f[0], env);
                if is_nil(&recv) {
                    return V::Nil;
                }
                let th = self.thread_method_ty(&recv, &st(&f[1]));
                if !is_nil(&th) {
                    return th;
                }
                let base = self.strip(&recv).clone();
                if tag(&base) != "named" || !items(tel(&base, 2)).is_empty() {
                    return V::Nil;
                }
                let owner = st(tel(&base, 1));
                if !self.p.types.contains_key(&owner) {
                    return V::Nil;
                }
                let name = st(&f[1]);
                let n = items(&f[2]).len();
                let cands: Vec<&Def> = self
                    .p
                    .method_idx
                    .get(&name)
                    .map(|v| {
                        v.iter()
                            .map(|&i| self.def(i))
                            .filter(|c| {
                                c.params.len() == n + 1 && c.name.rsplit_once('.').map_or(false, |(o, _)| o == owner)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                match cands.as_slice() {
                    [callee] if !self.is_generic(&callee.ret, &callee.gnames()) => callee.ret.clone(),
                    _ => V::Nil,
                }
            }
            "StructLit" => {
                let name = st(&f[0]);
                let segs: Vec<String> = name.split('.').map(String::from).collect();
                let fq = self.p.expand(&segs, &self.def(self.d).file).join(".");
                let Some(&ti) = self.p.types.get(&fq) else { return V::Nil };
                // ジェネリクスの型引数は、型がそのまま型パラメータの宣言のフィールドの値から推論する
                // (`t.Box { v: t.mk() }` の `T`)。推論できない引数は Nil (黙る)。
                let d = self.def(ti);
                let tparams: Vec<String> = d.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.clone()).collect();
                let mut args = vec![V::Nil; tparams.len()];
                if d.kw == "type" && !tparams.is_empty() {
                    for fl in items(&f[1]) {
                        let fx = items(fl);
                        let Some((_, decl, _)) = d.fields.iter().find(|x| x.0 == st(&fx[0])) else { continue };
                        if tag(decl) != "named" || !items(tel(decl, 2)).is_empty() {
                            continue;
                        }
                        let Some(i) = tparams.iter().position(|p| *p == st(tel(decl, 1))) else { continue };
                        if is_nil(&args[i]) {
                            args[i] = self.ty_of(&fx[1], env);
                        }
                    }
                }
                V::L(vec![V::S("named".into()), V::S(fq), V::L(args)])
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
                    // `rx.recv()` のような局所変数のメソッド呼び出し (パスの形で表れる)
                    if segs.len() == 2 {
                        if let Some(t) = env.get(&segs[0]) {
                            return self.thread_method_ty(t, &segs[1]);
                        }
                    }
                    if !env.contains_key(&segs[0]) {
                        if let Some(r) = self.p.resolve(&segs, &self.def(self.d).file) {
                            if let Target::Variant(en, vn) = &r.target {
                                // プレリュードの `Option` / `Result` は `p.types` に無いので別に通す。
                                if self.p.types.contains_key(en) || en == "Option" || en == "Result" {
                                    // `Some(x)`・`Ok(x)`・`Err(x)` は引数の型を型引数に入れる (4.4)。
                                    // 分からない型引数は Nil (黙る)。
                                    let payload = items(&nf(e)[1]).first().map(|a| self.ty_of(a, env));
                                    let args = match (en.as_str(), vn.as_str(), payload) {
                                        ("Option", "Some", Some(p)) => vec![p],
                                        ("Result", "Ok", Some(p)) => vec![p, V::Nil],
                                        ("Result", "Err", Some(p)) => vec![V::Nil, p],
                                        _ => vec![],
                                    };
                                    return V::L(vec![V::S("named".into()), V::S(en.clone()), V::L(args)]);
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

    /// スレッド間でやり取りする線形値の型 (5.5): `recv` / `close` / `send` / `clone` / `join` の結果型。
    /// 受け取った線形値のパターン束縛 (`Ok(r) => ..`) が消費の義務を負うために必要。分からなければ `V::Nil`。
    fn thread_method_ty(&self, recv: &V, name: &str) -> V {
        if is_nil(recv) {
            return V::Nil;
        }
        let r = self.strip(recv).clone();
        if tag(&r) != "named" {
            return V::Nil;
        }
        let targs: Vec<V> = items(tel(&r, 2)).iter().filter(|a| matches!(a, V::L(_))).cloned().collect();
        let named = |n: &str, a: Vec<V>| V::L(vec![V::S("named".into()), V::S(n.into()), V::L(a)]);
        let unit = V::L(vec![V::S("tuple".into()), V::L(vec![])]);
        match (st(tel(&r, 1)).as_str(), name, targs.first()) {
            ("std.sync.Receiver", "recv", Some(t)) => named("Result", vec![t.clone(), named("std.sync.RecvError", vec![])]),
            ("std.sync.Receiver", "close", Some(_)) => named("Result", vec![unit, r.clone()]),
            ("std.sync.Receiver" | "std.sync.Sender", "clone", Some(_)) => r.clone(),
            ("std.sync.Sender", "send", Some(t)) => named("Result", vec![unit, named("std.sync.SendError", vec![t.clone()])]),
            ("std.thread.JoinHandle", "join", Some(t)) => t.clone(),
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
        self.move_sites.clear();
        self.fnonce_locals.clear();
        self.noncopy_closures.clear();
        self.ref_binds.clear();
        self.shared_lets.clear();
        self.let_names.clear();
        // `F: core.ops.FnOnce<..>` の型パラメータの引数は、呼ぶと消費される (一度きり)。
        for (pn, pt) in &d.params {
            if tag(pt) == "named" && items(tel(pt, 2)).is_empty() {
                let tn = st(tel(pt, 1));
                let once = d.generics.iter().filter(|g| g.0 == "type" && g.1 == tn).any(|g| {
                    items(&g.2).iter().any(|b| {
                        let bn = match b {
                            V::L(bx) if !bx.is_empty() => st(&bx[0]),
                            V::S(x) => x.clone(),
                            _ => String::new(),
                        };
                        bn == "FnOnce" || bn.ends_with(".FnOnce")
                    })
                });
                if once {
                    self.fnonce_locals.insert(pn.clone());
                    self.noncopy_closures.insert(pn.clone());
                }
            }
        }
        self.lin.clear();
        self.lin_floor = 0;
        self.lin_in_closure = false;
        self.lin_loops.clear();
        // `linear` と宣言された型パラメータ (`T: linear`) の値は線形。仮引数もそうなら消費の義務を負う (4.4)。
        self.lin_gnames = d
            .generics
            .iter()
            .filter(|g| g.0 == "type" && items(&g.2).iter().any(|b| matches!(b, V::S(x) if x == "linear")))
            .map(|g| g.1.clone())
            .collect();
        for (pn, pt) in &d.params {
            // 線形と分かる型 (`T: linear` や、それを含む `Option<T>`) は、汎用でも義務を負う。
            // 宣言行は仮引数の行 (関数ヘッダ行) で、0 にすると診断の `declared at line 0` が誤る (g25)。
            self.lin_declare(pn, pt, d.line);
        }
        let body = d.body.as_ref().unwrap();
        self.tc_block(body, &env, &BTreeSet::new());
        self.lin_scope_end(0, "when the function ends");
        self.lin_gnames.clear();
        self.check_extern_calls(di);
        self.check_callback_args(di);
        // NLL の借用検査 (制御フローと、借用を持つ変数の生存に基づく)
        self.borrowck(di);
        env = self.final_env.clone();
        let stmts = items(&nf(body)[0]);
        if let Some(last) = stmts.last() {
            // 関数の末尾の値 `a[i]` は、要素を呼び出し元へ move する (4.1)。`return a[i]` と同じ扱い。
            if nname(last) == "ExprStmt" && nname(&nf(last)[0]) == "Index" && !is_nil(&d.ret) && tag(&d.ret) != "unit" {
                let e = &nf(last)[0];
                self.index_consumes.push((d.name.clone(), nid(&nf(e)[0]), index_text(e), nline(e)));
            }
            if nname(last) == "ExprStmt" {
                let names = self.names.clone();
                if !is_nil(&d.ret) && !self.is_generic(&d.ret, &names) {
                    let got = self.ty_of(&nf(last)[0], &env);
                    self.mismatch(&d.ret, &got, nline(last), "return value");
                }
            }
        }
    }

    /// 型 `t` を C の型として使えない理由 (使えるなら None)。`array_ok` は、配列を値として許すか
    /// (`layout c` の構造体のフィールドは可、`extern` の引数・戻り値は不可: C では配列は渡せず、ポインタになる)。
    /// 型が決まらない (名前を解決できない) ときは黙る。
    fn c_type_problem(&self, t: &V, file: &str, array_ok: bool) -> Option<String> {
        match tag(t).as_str() {
            "prim" => {
                let p = st(tel(t, 1));
                if matches!(p.as_str(), "str" | "i128" | "u128") {
                    Some(format!("`{}` has no C counterpart", p))
                } else {
                    None
                }
            }
            "ptr" | "fn" | "never" => None,
            "tuple" if items(tel(t, 1)).is_empty() => None,
            "tuple" => Some("a tuple is not a C type".into()),
            "array" if array_ok => self.c_type_problem(tel(t, 1), file, true),
            "array" => Some("an array is not passed by value in C (pass a pointer)".into()),
            "ref" | "slice" => Some("a reference or slice is not a C type (use a raw pointer)".into()),
            "named" => {
                let name = st(tel(t, 1));
                let segs: Vec<String> = name.split('.').map(String::from).collect();
                let full = self.p.expand(&segs, file).join(".");
                match self.p.types.get(&full) {
                    Some(&i) => {
                        let d = &self.p.defs[i];
                        if d.kw == "type" && d.layout_c {
                            None
                        } else {
                            Some(format!("`{}` is not a `layout c` struct", full))
                        }
                    }
                    None if matches!(name.as_str(), "Option" | "Result") => Some(format!("`{}` is not a C type", name)),
                    None => None,
                }
            }
            _ => None,
        }
    }

    /// 3.8: `extern` の引数・戻り値の型が C の型であること (E5007)、関数型 (C のコールバック) が
    /// `uses [] panics never` であること (E5004)。
    fn check_extern_sig(&mut self, di: usize) {
        let d = self.def(di);
        let mut tys: Vec<V> = d.params.iter().map(|p| p.1.clone()).collect();
        tys.push(d.ret.clone());
        for t in &tys {
            if tag(t) == "fn" {
                let (uses, pn) = (strs(tel(t, 3)), st(tel(t, 4)));
                if !uses.is_empty() || pn != "never" {
                    self.diag(
                        "E5004",
                        format!(
                            "{}: a function type that crosses the C boundary must be `uses [] panics never` (C cannot carry effects or panics)",
                            d.name
                        ),
                        di,
                        0,
                    );
                }
                for x in items(tel(t, 1)).iter().chain(std::iter::once(tel(t, 2))) {
                    if let Some(why) = self.c_type_problem(x, &d.file, false) {
                        self.diag("E5007", format!("{}: the callback signature is not C-compatible: {}", d.name, why), di, 0);
                    }
                }
            } else if let Some(why) = self.c_type_problem(t, &d.file, false) {
                self.diag("E5007", format!("{}: the signature is not C-compatible: {}", d.name, why), di, 0);
            }
        }
    }

    /// 3.8: `layout c` の型は、フィールドがすべて C の型 (整数・浮動小数・`bool`・`char`・生ポインタ・関数型・
    /// 配列・入れ子の `layout c` の構造体) の、ジェネリクスなしの構造体 (E5006)。
    fn check_layout_c(&mut self, di: usize) {
        let d = self.def(di);
        if !d.layout_c {
            return;
        }
        if d.kw != "type" {
            self.diag("E5006", format!("{}: `layout c` applies to a struct (`type`), not to an enum", d.name), di, 0);
            return;
        }
        if d.generics.iter().any(|g| g.0 != "life") {
            self.diag("E5006", format!("{}: a `layout c` struct cannot be generic", d.name), di, 0);
        }
        if d.fields.is_empty() {
            self.diag("E5006", format!("{}: a `layout c` struct needs at least one field (C has no empty structs)", d.name), di, 0);
        }
        for (n, t, _) in &d.fields {
            if let Some(why) = self.c_type_problem(t, &d.file, true) {
                self.diag("E5006", format!("{}: the field `{}` is not a C type: {}", d.name, n, why), di, 0);
            }
        }
    }

    /// 3.8: C に渡す関数型の引数 (関数・クロージャ) は `uses [] panics never` を満たすこと (E5005)。
    /// 関数への道は宣言 (推論した効果を含む) を、クロージャ式は本体の効果を見る。キャプチャを持つクロージャも不可 (E5005)。
    fn check_callback_args(&mut self, di: usize) {
        let d = self.def(di);
        let Some(body) = &d.body else { return };
        let mut nodes: Vec<&V> = vec![];
        walk(body, &mut nodes);
        let mut found: Vec<(usize, String)> = vec![];
        for n in nodes {
            if nname(n) != "Call" || nname(&nf(n)[0]) != "Path" {
                continue;
            }
            let segs = strs(&nf(&nf(n)[0])[0]);
            let Some(r) = self.p.resolve(&segs, &d.file) else { continue };
            let Target::Sig(i) = r.target else { continue };
            let ext = &self.p.defs[i];
            if !r.rest.is_empty() || ext.kw != "extern" {
                continue;
            }
            for (a, (_, pt)) in items(&nf(n)[1]).iter().zip(ext.params.iter()) {
                if tag(pt) != "fn" {
                    continue;
                }
                match nname(a) {
                    "Path" => {
                        let asegs = strs(&nf(a)[0]);
                        if let Some(ar) = self.p.resolve(&asegs, &d.file) {
                            if let (Target::Fn(fi), true) = (&ar.target, ar.rest.is_empty()) {
                                let cd = &self.p.defs[*fi];
                                if !cd.uses.is_empty() || cd.panics.as_deref() != Some("never") {
                                    found.push((nline(a), format!("the callback `{}` must be declared `uses [] panics never`", cd.name)));
                                }
                            }
                        }
                    }
                    "Closure" => {
                        if !items(&nf(a)[2]).is_empty() {
                            found.push((nline(a), "a closure passed to C cannot capture variables (write `capture []`)".into()));
                        }
                        let locals = crate::infer::local_names(d, body);
                        let (uses, may) = crate::infer::body_effects(self.p, &d.file, &locals, &nf(a)[3], true);
                        if !uses.is_empty() || may {
                            found.push((nline(a), "the closure passed to C must be `uses [] panics never`".into()));
                        }
                    }
                    _ => {}
                }
            }
        }
        for (line, msg) in found {
            self.diag("E5005", msg, di, line);
        }
    }

    /// `extern` の関数と `core.ptr` の関数 (生ポインタの生成・変換・読み書き) は `unsafe` ブロックの中でしか呼べない (3.8, E5003)。
    /// 呼び出しの関数位置 (`t.getpid()`) にしか書けず、値としての参照 (`let f = t.getpid`、
    /// フィールドへの代入、引数、戻り値、クロージャの捕捉) は `unsafe` の中でも外でも E5003
    /// (2026-11-15 の決定: `extern` と `core.ptr` の関数は値として扱わない。C へ渡すコールバックは Kasane の関数を渡すので対象外)。
    fn check_extern_calls(&mut self, di: usize) {
        let d = self.def(di);
        let Some(body) = &d.body else { return };
        fn scan<'b>(
            ck: &Checker,
            v: &'b V,
            in_unsafe: bool,
            in_call_pos: bool,
            file: &str,
            out: &mut Vec<(usize, String, bool)>,
        ) {
            match nname(v) {
                "Call" => {
                    // 呼び出しの関数位置 (callee) にある extern/core.ptr 関数は「呼び出し」として検査する。
                    // callee は関数位置、それ以降の子は値位置。
                    if !in_unsafe {
                        if let Some(callee) = nf(v).first() {
                            if nname(callee) == "Path" {
                                let segs = strs(&nf(callee)[0]);
                                if let Some(r) = ck.p.resolve(&segs, file) {
                                    if let Target::Sig(i) = r.target {
                                        let t = &ck.p.defs[i];
                                        if r.rest.is_empty() && (t.kw == "extern" || t.name.starts_with("core.ptr.")) {
                                            out.push((nline(v), t.name.clone(), false));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    let f = nf(v);
                    if !f.is_empty() {
                        scan(ck, &f[0], in_unsafe, true, file, out);
                        for c in &f[1..] {
                            scan(ck, c, in_unsafe, false, file, out);
                        }
                    }
                    return;
                }
                "Unsafe" => {
                    let f = nf(v);
                    for c in f {
                        scan(ck, c, true, false, file, out);
                    }
                    return;
                }
                "Path" => {
                    // 呼び出しの関数位置以外に名前が現れる = 関数を値として参照。
                    // 値としての参照は `unsafe` の中でも拒否する (`extern` と `core.ptr` の関数は値にならない)。
                    if !in_call_pos {
                        let segs = strs(&nf(v)[0]);
                        if let Some(r) = ck.p.resolve(&segs, file) {
                            if let Target::Sig(i) = r.target {
                                let t = &ck.p.defs[i];
                                if r.rest.is_empty() && (t.kw == "extern" || t.name.starts_with("core.ptr.")) {
                                    out.push((nline(v), t.name.clone(), true));
                                }
                            }
                        }
                    }
                    return;
                }
                _ => {}
            }
            for c in children(v) {
                scan(ck, c, in_unsafe, false, file, out);
            }
            // `children` は Node のみを辿るため、`V::L` (引数リストなど) が直接渡されたときは
            // その要素も辿る。
            if let V::L(xs) = v {
                for c in xs {
                    scan(ck, c, in_unsafe, false, file, out);
                }
            }
        }
        let mut found = vec![];
        scan(self, body, false, false, &d.file, &mut found);
        for (line, name, as_value) in found {
            let msg = if as_value {
                format!("the external or raw-pointer function `{}` cannot be used as a value (it can only be called directly, e.g. `{}(..)`)", name, name)
            } else {
                format!("the external or raw-pointer function `{}` can only be called inside an `unsafe` block", name)
            };
            self.diag("E5003", msg, di, line);
        }
    }

    /// 3.5 / 3.8: `[unsafe] modules` に列挙されていないモジュールのユーザー定義は、`extern` の宣言、
    /// 生ポインタ型 (注釈・引数・戻り値・フィールド・本体の型)、`core.ptr` の関数、生ポインタを
    /// シグネチャに持つ関数・定数の参照を使えない (E5001)。`unsafe` ブロックだけを見ていた穴を、
    /// 生ポインタと FFI の入口の全部で塞ぐ。判断できない参照は通さない (メソッドは名前で候補を見る)。
    fn check_unsafe_surface(&mut self, di: usize) {
        let mut found: Vec<(usize, String)> = vec![];
        {
            let d = self.def(di);
            if d.kw == "extern" {
                found.push((0, format!("extern function `{}` (FFI) is only allowed in modules listed in [unsafe] modules", d.name)));
            }
            let mut tys: Vec<&V> = d.params.iter().map(|p| &p.1).collect();
            tys.push(&d.ret);
            tys.extend(d.fields.iter().map(|f| &f.1));
            tys.extend(d.variants.iter().flat_map(|v| v.1.iter()));
            tys.extend(d.generics.iter().map(|g| &g.2));
            if let Some(b) = &d.body {
                tys.push(b);
            }
            for t in tys {
                if let Some(l) = find_ptr_ty(t, 0) {
                    found.push((l, "raw pointer type `*const T` / `*mut T` is only allowed in modules listed in [unsafe] modules".into()));
                }
            }
            if let Some(b) = &d.body {
                let mut nodes = vec![];
                walk(b, &mut nodes);
                for n in nodes {
                    if nname(n) != "Path" {
                        continue;
                    }
                    let segs = strs(&nf(n)[0]);
                    let Some(r) = self.p.resolve(&segs, &d.file) else { continue };
                    let target = match r.target {
                        Target::Fn(i) | Target::Sig(i) | Target::Const(i) => i,
                        _ => continue,
                    };
                    let Some(t) = self.p.defs.get(target) else { continue };
                    if t.name.starts_with("core.ptr.") || sig_has_ptr(t) {
                        found.push((nline(n), format!("`{}` uses raw pointers and is only allowed in modules listed in [unsafe] modules", t.name)));
                    }
                }
            }
        }
        for (line, msg) in found {
            self.diag("E5001", msg, di, line);
        }
    }

    /// 関数 / クロージャの本体のブロック。末尾の式は値として返る (消費)。
    fn tc_block(&mut self, b: &V, env: &Env, muts: &BTreeSet<String>) {
        self.final_env = self.tc_stmts(b, env, muts, true);
    }

    /// 文の並びを検査する。`consume` は、ブロックの値 (末尾の式) が値として渡る位置か。
    /// ブロックの中で宣言した線形値は、ブロックの終わりまでに消費されていなければならない (4.4)。
    fn tc_stmts(&mut self, b: &V, env: &Env, muts: &BTreeSet<String>, consume: bool) -> Env {
        let mut env = env.clone();
        let mut muts = muts.clone();
        let base = self.lin.len();
        let stmts = items(&nf(b)[0]);
        for (i, s) in stmts.iter().enumerate() {
            self.tc_stmt(s, &mut env, &mut muts, consume && i + 1 == stmts.len());
            self.discard_check(stmts, i, &env);
        }
        if stmts.iter().any(|s| self.diverges(s)) {
            // 後続へ進まないブロック: 漏れは `return` / `break` / `continue` の地点で報告済み。
            self.lin.truncate(base);
        } else {
            self.lin_scope_end(base, "when its scope ends");
        }
        env
    }

    fn tc_stmt(&mut self, s: &V, env: &mut Env, muts: &mut BTreeSet<String>, tail: bool) {
        let f = nf(s);
        match nname(s) {
            "Let" => {
                self.lin_closure_took = false;
                let spawn_began = self.spawn_begin(&f[3], env);
                self.tc_expr(&f[3], env, muts, true);
                let spawn_safe = if spawn_began {
                    let unsafe_ = self.spawn_scan.pop().unwrap_or(true);
                    if unsafe_ {
                        self.spawn_unsafe();
                    }
                    Some(!unsafe_)
                } else {
                    None
                };
                let took = nname(&f[3]) == "Closure" && self.lin_closure_took;
                self.lin_closure_took = false;
                let got = self.ty_of(&f[3], env);
                if !is_nil(&f[2]) {
                    self.mismatch(&f[2], &got, nline(s), "let binding");
                    if !self.is_generic(&f[2], &self.names) {
                        let t = f[2].clone();
                        self.check_ty_linear_args(&t, self.d, nline(s));
                    }
                }
                let ty = if !is_nil(&f[2]) { f[2].clone() } else { got.clone() };
                // パターンに沿って束縛名の型を決める (PBind と、分解 `let (a, b) = ..`)。
                // 決まらない束縛は下のパスで V::Nil になり、検査は黙る。
                // 参照越しの分解 `let p = x` (`x: &T`): 束縛は `&T` の欄。所有値として消費させない (E3002)。
                let src_is_ref = self.ety_is_ref(&f[3]) || tag(&got) == "ref" || tag(&ty) == "ref";
                let shared_src = tag(&ty) == "ref" && matches!(tel(&ty, 1), V::B(false));
                let pat_ty = if src_is_ref && tag(&ty) == "ref" { tel(&ty, 2).clone() } else { ty };
                let mut typed: Vec<(String, V)> = vec![];
                self.bind_pat_types(&f[0], &pat_ty, &mut typed);
                self.lin_wild(&typed, nline(s));
                let mut binds = vec![];
                pat_binds(&f[0], &mut binds);
                // `let r = &x` (共有参照): 束縛名を覚える。下の包み直しは `&mut` 由来の束縛も共有に見せるので、ここで元の向きを見る。
                for n in binds {
                    self.let_names.insert(n.clone());
                    if shared_src && nname(&f[0]) == "PBind" {
                        self.shared_lets.insert(n.clone());
                    } else {
                        self.shared_lets.remove(&n);
                    }
                    let mut t = typed.iter().find(|(m, _)| *m == n).map(|(_, t)| t.clone()).unwrap_or(V::Nil);
                    if is_nil(&t) {
                        // 自前で決まらない束縛は、型推論の結果を使う (未決定の部分を含む型は使わない)。
                        if let Some(ty) = self.let_tys.get(&(self.d, nline(s), n.clone())) {
                            t = crate::types::TypeCk::to_v(ty).unwrap_or(V::Nil);
                        }
                    }
                    if src_is_ref && !is_nil(&t) && tag(&t) != "ref" && !self.is_copy(&t) {
                        // 型パラメータへの共有参照 (`&T`) も Copy: 束縛を `&T` のままにして move 記録しない。
                        t = V::L(vec![V::S("ref".into()), V::B(false), t]);
                        self.ref_binds.insert(n.clone());
                    } else {
                        self.ref_binds.remove(&n);
                    }
                    env.insert(n.clone(), t);
                    // `let z = s` で `s` が参照越しの `match` 束縛 (ref_binds) なら、`z` も同じ参照として束縛する。
                    // `*z` や `z` を値レシーバに渡すと E3002 (f23)。
                    if nname(&f[0]) == "PBind" && nname(&f[3]) == "Path" {
                        let segs = strs(&nf(&f[3])[0]);
                        if segs.len() == 1 && self.ref_binds.contains(&segs[0]) {
                            if let Some(st) = env.get(&segs[0]) {
                                t = st.clone();
                                self.ref_binds.insert(n.clone());
                                env.insert(n.clone(), t.clone());
                            }
                        }
                    }
                    self.revive_place(std::slice::from_ref(&n));
                    let once = matches!(
                        self.let_tys.get(&(self.d, nline(s), n.clone())),
                        Some(crate::types::Ty::Closure { cap: crate::types::Cap::FnOnce, .. })
                    );
                    if once {
                        self.fnonce_locals.insert(n.clone());
                    } else {
                        self.fnonce_locals.remove(&n);
                    }
                    if nname(&f[0]) == "PBind" && (once || self.closure_value_non_copy(&f[3], env)) {
                        self.noncopy_closures.insert(n.clone());
                    } else {
                        self.noncopy_closures.remove(&n);
                    }
                    if matches!(f[1], V::B(true)) {
                        muts.insert(n);
                    } else {
                        muts.remove(&n);
                    }
                }
                // 線形な束縛は消費の義務を負う (4.4)。型が決まらない束縛は義務なし (黙る)。
                let mut names = vec![];
                pat_binds(&f[0], &mut names);
                for n in names {
                    let t = env.get(&n).cloned().unwrap_or(V::Nil);
                    // 線形な要素を持つ配列リテラルの束縛は、要素を添字ごとに調べる (`note_linear_array`)。
                    // 束縛そのものは線形としない (添字での取り出しを `ls` の消費漏れにしない)。
                    if nname(&f[0]) == "PBind" && self.note_linear_array(&n, &f[3], &t, nline(s)) {
                        self.lin_declare(&n, &V::Nil, nline(s));
                        continue;
                    }
                    self.lin_declare(&n, &t, nline(s));
                    // 線形値を `move` で取り込んだクロージャは、それ自体が線形 (4.4)。呼ぶと消費される。
                    if took && nname(&f[0]) == "PBind" {
                        if let Some(l) = self.lin.last_mut() {
                            l.linear = true;
                            l.live = true;
                        }
                    }
                }
                // 取り消しの検査: `let t = spawn_local(..)` の束縛 `t` を、閉包が取り消せるかの表に載せる (上の束縛で外れた分を戻す)。
                if let Some(safe) = spawn_safe {
                    let mut ns = vec![];
                    pat_binds(&f[0], &mut ns);
                    if nname(&f[0]) == "PBind" && ns.len() == 1 {
                        self.task_safe.insert((self.d, ns[0].clone()), safe);
                    }
                }
            }
            "Assign" => {
                self.tc_expr(&f[1], env, muts, true);
                let tg = &f[0];
                if nname(tg) == "Path" {
                    let segs = strs(&nf(tg)[0]);
                    if segs.len() == 1 {
                        self.task_safe.remove(&(self.d, segs[0].clone()));
                    }
                }
                // 参照越しの代入 (`*slot = n`、`p.a = n` で `p` が参照): 古い値は消費されずに消えるので、
                // 代入先の型が線形なら E3003 (4.4)。
                if self.assign_through_ref(tg, env) && self.is_linear(&self.lin_ty(tg, env)) {
                    self.tdiag(
                        "E3003",
                        "a linear value behind a reference is overwritten by the assignment without being consumed".into(),
                        nline(s),
                    );
                }
                // `a[i] = ..` で線形な要素を上書きすると、添字演算は基を借用して要素を move しないので、
                // 上書き前の値が消費されずに捨てられる (4.4)。
                if nname(tg) == "Index" {
                    let et = self.ty_of(tg, env);
                    if !is_nil(&et) && self.is_linear(&et) {
                        self.tdiag(
                            "E3003",
                            "a linear array element is overwritten without being consumed first".into(),
                            nline(s),
                        );
                    }
                }
                if nname(tg) == "Path" {
                    let segs = strs(&nf(tg)[0]);
                    if env.contains_key(&segs[0]) {
                        let root = &segs[0];
                        let rt = env.get(root).cloned().unwrap_or(V::Nil);
                        // 型の分からない束縛 (match の腕などで rt == Nil) や &mut 束縛
                        // (tag == "ref") への代入は E3001 にしない: 「分からなければ黙る」
                        // が既存の作法。`let x = S{..}` のような値束縛 (rt が named など)
                        // は従来どおりエラーにする。
                        // 共有参照 `&T` 越しの欄への代入 (`r.v = ..`、`let r = &h` や `p: &T` のとき): 参照の中身は書けない。
                        // 内部の可変性は `core.sync` のアトミック型で表すので、欄への代入は常に誤り。
                        // let で束縛した名前は、`&mut` 由来の束縛も共有の形に包まれるので、`shared_lets` だけを見る。
                        let shared_ref = self.shared_lets.contains(root)
                            || (tag(&rt) == "ref" && matches!(tel(&rt, 1), V::B(false)) && !self.let_names.contains(root));
                        if segs.len() >= 2 && root != "self" && shared_ref {
                            self.tdiag(
                                "E3001",
                                format!("cannot assign to `{}` through the shared reference `{}`: write through `&mut`", segs.join("."), root),
                                nline(s),
                            );
                        }
                        if !muts.contains(root)
                            && root != "self"
                            && !is_nil(&rt)
                            && tag(&rt) != "ref"
                        {
                            self.tdiag(
                                "E3001",
                                format!("cannot assign to `{}`: `{}` is not declared `mut`", segs.join("."), root),
                                nline(s),
                            );
                        }
                        // 代入で、その場所とその下の move 済みの部分は復活する (4.1)。
                        self.revive_place(&segs);
                        // 線形な欄の義務 (`p.a = ..`) も、古い値の未消費は漏れ、新しい値の義務を負う。
                        self.lin_assign_place(&segs, nline(s));
                        if segs.len() == 1 {
                            // 線形な変数への再代入: 古い値が残っていれば漏れ。新しい値の義務を負う。
                            if let Some(i) = self.lin_find(root) {
                                if self.lin[i].linear {
                                    if self.lin[i].live {
                                        let (ln, dl) = (nlineno(s), crate::parser::unpack_pos(self.lin[i].line).0);
                                        self.tdiag(
                                            "E3003",
                                            format!("linear value `{root}` (declared at line {dl}) is not consumed before the assignment at line {ln} overwrites it"),
                                            nline(s),
                                        );
                                    }
                                    self.lin[i].live = true;
                                }
                            }
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
            _ => self.tc_expr(&f[0], env, muts, tail),
        }
    }

    /// 代入の対象が参照の先か (`*r`、参照の根からの欄 `p.a`)。根の束縛の代入 (`s = ..`) は含まない。
    fn assign_through_ref(&self, tg: &V, env: &Env) -> bool {
        let root_is_ref = |segs: &[String]| env.get(&segs[0]).is_some_and(|t| tag(t) == "ref");
        match nname(tg) {
            "Unary" => st(&nf(tg)[0]) == "*",
            "Path" => {
                let segs = strs(&nf(tg)[0]);
                segs.len() >= 2 && root_is_ref(&segs)
            }
            "Field" => {
                let mut b = &nf(tg)[0];
                while nname(b) == "Field" {
                    b = &nf(b)[0];
                }
                match nname(b) {
                    "Path" => root_is_ref(&strs(&nf(b)[0])),
                    "Unary" => st(&nf(b)[0]) == "*",
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// 式が一時値か (場所 `x` / `x.f` / `a[i]` / `*r` でなく、値を作る式)。一時値は束縛も消費もされなければ漏れる。
    fn is_temp_value(e: &V) -> bool {
        match nname(e) {
            "Path" | "Field" | "Index" => false,
            "Unary" => st(&nf(e)[0]) != "*",
            _ => true,
        }
    }

    /// 線形な一時値が、束縛も消費もされない位置 (`&` の対象、欄の読み出しの基) に現れたら E3003 (4.4)。
    /// 消費の位置で線形な欄を取り出す (`t.mk().inner` を渡す) ときの判定は、呼び出し側 (`Field`) が決める。
    fn lin_temp_unused(&mut self, e: &V, env: &Env, line: usize) {
        if !Self::is_temp_value(e) {
            return;
        }
        let t = self.lin_ty(e, env);
        if self.is_linear(&t) {
            self.tdiag(
                "E3003",
                "a linear temporary is borrowed or read through a field and is never consumed (bind it, or consume it)".into(),
                line,
            );
        }
    }

    /// 場所 (根の変数 + フィールド道) の使用を検査する (4.1。部分 move を含む)。
    /// move 済みの場所と重なる (一方が他方の接頭辞) ときは E3002。
    /// 重ならなければ、consume のときに型を解決して move を記録する (Copy は move にならない)。
    fn use_place(&mut self, root: &str, path: &[String], env: &Env, line: usize, consume: bool) {
        if let Some((mk, ml)) = self.moved_overlap(root, path) {
            let ml = crate::parser::unpack_pos(ml).0;
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
            if !path.is_empty() && self.move_out_of_ref(root, path, env, line) {
                return;
            }
            if path.is_empty() && self.noncopy_closures.contains(root) {
                self.moved.insert(root.to_string(), line);
            }
            self.record_move(root, path, env, line);
            self.lin_consume_place(root, path, env);
        }
    }

    /// 参照 (`&T` / `&mut T`) の先のフィールドを、Copy でない型のまま値として使った (move した) なら E3002 (4.1)。
    /// 根の型が参照で、道の型を最後まで解決でき、Copy でないときだけ報告する (型が分からなければ黙る)。
    fn move_out_of_ref(&mut self, root: &str, path: &[String], env: &Env, line: usize) -> bool {
        let Some(t) = env.get(root) else { return false };
        if is_nil(t) || tag(t) != "ref" {
            return false;
        }
        let mut cur = tel(t, 2).clone();
        for seg in path {
            if is_nil(&cur) || tag(&cur) == "ref" {
                return false;
            }
            match self.field_ty(&cur, seg) {
                Some(n) => cur = n,
                None => return false,
            }
        }
        if is_nil(&cur) || self.is_generic(&cur, &self.names) || self.is_copy(&cur) {
            return false;
        }
        let key = place_key(root, path);
        self.tdiag(
            "E3002",
            format!("cannot move out of `{key}`: `{root}` is a reference, so its field cannot be moved; borrow it with `&{key}` or copy it"),
            line,
        );
        true
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
        self.move_sites.insert((line, place_key(root, path)));
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

    /// パターン `p` を型 `ty` に照らして、束縛名とその型の組を集める (4.4 の分解)。
    /// PBind はその型、PTuple は `ty` がタプルで要素数が一致するときだけ要素型を流す。
    /// それ以外 (PVariant・PStruct・PRef・PLit や、型が決まらないとき) は何も入れず、
    /// 呼び出し側の既定 (V::Nil) に任せる (型が決まらない所では黙る)。
    fn bind_pat_types(&self, p: &V, ty: &V, out: &mut Vec<(String, V)>) {
        match nname(p) {
            "PBind" => out.push((st(&nf(p)[0]), ty.clone())),
            "PTuple" => {
                let pats = items(&nf(p)[0]);
                let elems = if tag(ty) == "tuple" { items(tel(ty, 1)) } else { &[] };
                let known = tag(ty) == "tuple" && elems.len() == pats.len();
                for (i, sub) in pats.iter().enumerate() {
                    // 要素型が決まり、ジェネリクスを含まないときだけ流す。
                    let et = if known && self.keeps_ty(&elems[i]) {
                        elems[i].clone()
                    } else {
                        V::Nil
                    };
                    if nname(sub) == "PWild" && !is_nil(&et) {
                        out.push(("_".to_string(), et.clone()));
                    }
                    self.bind_pat_types(sub, &et, out);
                }
            }
            "PVariant" => {
                // `Some(p)` / `Ok(p)` / `Err(p)` と、ジェネリクスでないユーザー定義 enum のバリアント。
                // 参照越しの照合 (スクラティニーが `&T`) は所有しないので流さない。
                let f = nf(p);
                let segs = strs(&f[0]);
                let subs = items(&f[1]);
                let Some(vname) = segs.last() else { return };
                if tag(ty) != "named" {
                    return;
                }
                let tname = st(tel(ty, 1));
                let targs: Vec<&V> = items(tel(ty, 2)).iter().filter(|a| matches!(a, V::L(_))).collect();
                let payload: Vec<V> = match (tname.as_str(), vname.as_str()) {
                    ("Option", "Some") => targs.first().map(|t| vec![(*t).clone()]).unwrap_or_default(),
                    ("Result", "Ok") => targs.first().map(|t| vec![(*t).clone()]).unwrap_or_default(),
                    ("Result", "Err") => targs.get(1).map(|t| vec![(*t).clone()]).unwrap_or_default(),
                    // ジェネリックな enum は、使用側の型引数を代入した payload の型を流す (個数が合わなければ黙る)。
                    _ => match self.p.types.get(&tname) {
                        Some(&i) => {
                            let d = self.def(i);
                            let tps: Vec<String> = d.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.clone()).collect();
                            let args = items(tel(ty, 2));
                            if tps.len() != args.len() {
                                vec![]
                            } else {
                                let bind: HashMap<String, V> = tps.into_iter().zip(args.iter().cloned()).collect();
                                d.variants
                                    .iter()
                                    .find(|v| &v.0 == vname)
                                    .map(|v| v.1.iter().map(|t| subst_ty_params(t, &bind)).collect())
                                    .unwrap_or_default()
                            }
                        }
                        _ => vec![],
                    },
                };
                if payload.len() != subs.len() {
                    return;
                }
                for (sub, pt) in subs.iter().zip(payload.iter()) {
                    let pt = if self.keeps_ty(pt) { pt.clone() } else { V::Nil };
                    if nname(sub) == "PWild" && !is_nil(&pt) {
                        out.push(("_".to_string(), pt.clone()));
                    }
                    self.bind_pat_types(sub, &pt, out);
                }
            }
            "PStruct" => {
                // 構造体のパターン `T { f: p, .. }`: 欄の型 (ジェネリクスは使用側の型引数で置き換える) を、欄の部分パターンへ流す。
                if tag(ty) != "named" {
                    return;
                }
                for x in items(&nf(p)[1]) {
                    let Some(ft) = self.field_ty(ty, &st(&items(x)[0])) else { continue };
                    let ft = if self.keeps_ty(&ft) { ft } else { V::Nil };
                    let sub = &items(x)[1];
                    if nname(sub) == "PWild" && !is_nil(&ft) {
                        out.push(("_".to_string(), ft.clone()));
                    }
                    self.bind_pat_types(sub, &ft, out);
                }
            }
            _ => {}
        }
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
                self.tc_stmts(e, env, muts, consume);
            }
            "If" | "Match" | "Loop" | "While" | "For" => self.branchy(e, env, muts, consume),
            "With" => self.tc_with(e, env, muts, consume),
            "Break" | "Continue" => {
                // ループを抜ける / 次の周回へ入る時点の move を、囲むループに渡す。
                let cur = self.moved.clone();
                if let Some((breaks, continues)) = self.loop_exits.last_mut() {
                    Self::union_moves(if nname(e) == "Break" { breaks } else { continues }, &cur);
                }
                // ループの中で宣言した線形値が未消費なら、この経路で漏れる (4.4)。
                if let Some(base) = self.lin_loops.last().map(|l| l.base) {
                    let is_break = nname(e) == "Break";
                    let ln = nlineno(e);
                    let how = if is_break {
                        format!("when the `break` at line {ln} leaves the loop")
                    } else {
                        format!("when the `continue` at line {ln} starts the next iteration")
                    };
                    self.lin_leaks(base, nline(e), &how);
                    let snap = self.lin[..base.min(self.lin.len())].to_vec();
                    let desc = format!("the `{}` at line {ln}", if is_break { "break" } else { "continue" });
                    if let Some(lp) = self.lin_loops.last_mut() {
                        if is_break { &mut lp.breaks } else { &mut lp.conts }.push((desc, snap));
                    }
                }
            }
            "Closure" => {
                let mut sub = env.clone();
                // `move` キャプチャした線形値の義務は、クロージャに移る (4.4)。
                let mut taken: Vec<(String, usize)> = vec![];
                for c in items(&f[2]) {
                    if items(c).first().map(st).as_deref() == Some("move") && self.lin_consume_name(&st(&items(c)[1])) {
                        self.lin_closure_took = true;
                        taken.push((st(&items(c)[1]), nline(c)));
                    }
                }
                // `&f` / `&mut f` で取り込んだ `FnOnce` を本体で呼ぶのは、参照の先から move すること。
                for c in items(&f[2]) {
                    let name = st(tel(c, 1));
                    if st(tel(c, 0)) != "move" && self.fnonce_locals.contains(&name) {
                        let mut nodes: Vec<&V> = vec![];
                        walk(&f[3], &mut nodes);
                        let called = nodes.iter().any(|n| {
                            nname(n) == "Call" && nname(&nf(n)[0]) == "Path" && {
                                let sg = strs(&nf(&nf(n)[0])[0]);
                                sg[0] == name && (sg.len() == 1 || (sg.len() == 2 && sg[1] == "call_once"))
                            }
                        });
                        if called {
                            self.tdiag(
                                "E3002",
                                format!("cannot move out of `{name}`: it is an `FnOnce` captured by reference, and calling it moves it; capture it with `move {name}`"),
                                nline(e),
                            );
                        }
                    }
                }
                let saved_lin = (self.lin_floor, self.lin_in_closure, std::mem::take(&mut self.lin_loops));
                self.lin_floor = self.lin.len();
                self.lin_in_closure = true;
                // 移った義務は本体が負う: 取り込んだ名前を本体の線形な束縛として宣言し、全経路で消費しているかを検査する。
                for (name, line) in taken {
                    self.lin.push(Lin { name, line, linear: true, live: true, parts: false });
                }
                // `&a` / `&mut a` で取り込んだ名前は、本体では `&T` / `&mut T` (move は `T` のまま)。
                for c in items(&f[2]) {
                    let k = st(tel(c, 0));
                    let n = st(tel(c, 1));
                    if k == "move" {
                        continue;
                    }
                    if let Some(t) = env.get(&n) {
                        if !is_nil(t) && !self.is_generic(t, &self.names) && !self.is_copy(t) {
                            sub.insert(n, V::L(vec![V::S("ref".into()), V::B(k == "&mut"), t.clone()]));
                        }
                    }
                }
                for p in items(&f[0]) {
                    let (pn, pt) = (st(&items(p)[0]), items(p)[1].clone());
                    sub.insert(pn.clone(), pt.clone());
                    self.lin_declare(&pn, &pt, nline(e));
                }
                let saved = self.moved.clone();
                for p in items(&f[0]) {
                    self.revive_place(&[st(&items(p)[0])]);
                }
                let fe = self.final_env.clone();
                self.tc_block(&f[3], &sub, muts);
                let fl = self.lin_floor;
                self.lin_scope_end(fl, "when the closure ends");
                self.final_env = fe;
                self.moved = saved;
                // 取り込みは作った時点で行われる: `move x` は x を move し (Copy は除く)、`&x` / `&mut x` は使うだけ。
                for c in items(&f[2]) {
                    let name = st(tel(c, 1));
                    if env.contains_key(&name) {
                        self.use_place(&name, &[], env, nline(e), st(tel(c, 0)) == "move");
                    }
                }
                self.lin_floor = saved_lin.0;
                self.lin_in_closure = saved_lin.1;
                self.lin_loops = saved_lin.2;
            }
            "Try" => {
                self.tc_expr(&f[0], env, muts, true);
                // `?` はエラー側で関数 (クロージャ) から早期 return する。
                let fl = self.lin_floor;
                self.lin_leaks(fl, nline(e), &format!("when `?` returns early at line {}", nlineno(e)));
            }
            "Call" => {
                let mut callee: Option<&Def> = None;
                let mut lin_callee: Option<(&Def, usize)> = None;
                let fnn = &f[0];
                // 局所変数のメソッド呼び出し `t.m()` は `Path` の形で来る (`v.push(x)` と同じ)。レシーバの型で検査する。
                let local_method = nname(fnn) == "Path" && {
                    let segs = strs(&nf(fnn)[0]);
                    segs.len() == 2 && env.contains_key(&segs[0])
                };
                // 取り消しの検査 (仕様 5.6 の第 4 段): 閉包の中では、中断しうる関数・解放の要る結果を持つ呼び出しを禁じる。
                if !self.spawn_scan.is_empty() {
                    let ok = if local_method {
                        let segs = strs(&nf(fnn)[0]);
                        let rt = env.get(&segs[0]).cloned().unwrap_or(V::Nil);
                        is_named_ty(&rt, "std.task.Task") || tag(&rt) == "prim"
                    } else {
                        matches!(self.callee_target(fnn, env), Some((_, false)))
                    };
                    if !ok || !frame_scalar(&self.lin_ty(e, env)) {
                        self.spawn_unsafe();
                    }
                }
                // `t.cancel()` (局所のメソッドの形): 閉包が取り消せると分かった `let t = spawn_local(..)` の束縛だけ通す (E4022)。
                if local_method {
                    let segs = strs(&nf(fnn)[0]);
                    let rt = env.get(&segs[0]).cloned().unwrap_or(V::Nil);
                    if segs[1] == "cancel" && is_named_ty(&rt, "std.task.Task") {
                        if self.task_safe.get(&(self.d, segs[0].clone())) != Some(&true) {
                            self.tdiag("E4022", cancel_msg(), nline(e));
                        }
                    }
                }
                // `std.task.Task.cancel(t)` の形は扱わない (メソッドの形 `t.cancel()` だけ。E4022)。
                if matches!(self.callee_target(fnn, env), Some((ref n, _)) if n == "std.task.Task.cancel") {
                    self.tdiag("E4022", cancel_msg(), nline(e));
                }
                if nname(fnn) == "Path" {
                    let segs = strs(&nf(fnn)[0]);
                    if !env.contains_key(&segs[0]) {
                        if let Some(r) = self.p.resolve(&segs, &self.def(self.d).file) {
                            if let Target::Fn(i) = r.target {
                                if r.rest.is_empty() {
                                    callee = Some(self.def(i));
                                    lin_callee = Some((self.def(i), 0));
                                    let cn = self.def(i).name.clone();
                                    let cur = self.def(self.d).name.clone();
                                    self.graph.entry(cur).or_default().insert(cn);
                                }
                            } else if r.rest.is_empty() {
                                // 標準ライブラリの関数 (`core.mem.drop`、`Box.new` など。組み込みを含む):
                                // 線形な引数の検査だけ行う。
                                let sig = match &r.target {
                                    Target::Sig(i) => Some(*i),
                                    Target::Native(n) => self.p.sigs.get(n).copied(),
                                    _ => None,
                                };
                                if let Some(i) = sig.filter(|&i| self.def(i).kw == "fn") {
                                    lin_callee = Some((self.def(i), 0));
                                }
                            }
                        }
                    } else {
                        if segs.len() == 1 {
                            // 局所変数の関数値を呼ぶ。線形なクロージャは呼ぶと消費される (一度きりの呼び出し)。
                            self.lin_consume_name(&segs[0]);
                        }
                        // 呼び出し先が局所変数のとき。`v.push(x)` のようなメソッド呼び出し
                        // (segs の最後はメソッド名) は、レシーバの場所を使うだけで move はしない (4.2)。
                        // ただし `self` を値で受けるメソッド (`self: T`) はレシーバを消費する (4.4)。
                        // レシーバの型とメソッドが一意に分かるときだけ消費として扱う。
                        let recv: &[String] = if segs.len() >= 2 { &segs[1..segs.len() - 1] } else { &[] };
                        let by_value = segs.len() >= 2 && self.method_takes_self_by_value(&segs[0], recv, &segs[segs.len() - 1], env);
                        self.use_place(&segs[0], recv, env, nline(e), by_value);
                        // 参照の先の値を `self` で受けるメソッド (`s: &S` の `s.into_name()`) は、参照の先を move する。
                        if segs.len() >= 2 && !by_value {
                            if let Some((ty, true)) = self.recv_ty_through_ref(&segs[0], recv, env) {
                                let name = &segs[segs.len() - 1];
                                if self.method_of_named(&ty, name).is_some_and(|m| self.def_takes_self_by_value(m)) && !self.is_generic(&ty, &self.names) && !self.is_copy(&ty) {
                                    let place = segs[..segs.len() - 1].join(".");
                                    self.tdiag(
                                        "E3002",
                                        format!("cannot move out of `{place}`: `{name}` takes `self` by value, but `{place}` is behind a reference; call it on an owned value or clone first"),
                                        nline(e),
                                    );
                                }
                            }
                        }
                        // `FnOnce` のクロージャを呼ぶと、そのクロージャ自身が move される (2 回目の呼び出しは E3002)。
                        let call_once = segs.len() == 2 && segs[1] == "call_once";
                        if (segs.len() == 1 || call_once) && self.fnonce_locals.contains(&segs[0]) && self.moved_overlap(&segs[0], &[]).is_none() {
                            self.moved.insert(segs[0].clone(), nline(e));
                        }
                        if segs.len() >= 2 {
                            if let Some(md) = self.method_def(&segs[0], recv, &segs[segs.len() - 1], env) {
                                if md.is_method() {
                                    lin_callee = Some((md, 1));
                                }
                            }
                        }
                    }
                } else {
                    self.tc_expr(fnn, env, muts, false);
                }
                for (i, a) in items(&f[1]).iter().enumerate() {
                    self.tc_expr(a, env, muts, true);
                    if let Some(c) = callee {
                        if i < c.params.len() {
                            let pt = &c.params[i].1;
                            // 参照越しの `match` で `&T` として束縛した名前を、値 `T` の引数に渡す (= 参照の先を move する)。
                            if nname(a) == "Path"
                                && strs(&nf(a)[0]).len() == 1
                                && self.ref_binds.contains(&strs(&nf(a)[0])[0])
                                && tag(pt) != "ref"
                                && !self.is_generic(pt, &c.gnames())
                                && !self.is_copy(pt)
                            {
                                let n = &strs(&nf(a)[0])[0];
                                self.tdiag(
                                    "E3002",
                                    format!("cannot move out of `{n}`: it was bound by matching through a reference, so it is a reference; pass `{n}.clone()` or take the value out of the owner"),
                                    nline(a),
                                );
                            }
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
                if let Some((c, off)) = lin_callee {
                    self.check_linear_args(c, off, items(&f[1]), env, nline(e));
                }
            }
            "MCall" => {
                // 取り消しの検査 (仕様 5.6 の第 4 段): 閉包の中のメソッド呼び出しは、`Task` と数などの型に限る (ユーザのメソッドは中断しうる)。
                if !self.spawn_scan.is_empty() {
                    let rt = self.lin_ty(&f[0], env);
                    let rt_ok = is_named_ty(&rt, "std.task.Task") || matches!(tag(&rt).as_str(), "prim");
                    if !rt_ok || !frame_scalar(&self.lin_ty(e, env)) {
                        self.spawn_unsafe();
                    }
                }
                // `t.cancel()` は、`let t = spawn_local(閉包)` で束縛した局所で、閉包が取り消せるときだけ通す (E4022)。
                if st(&f[1]) == "cancel" && is_named_ty(&self.lin_ty(&f[0], env), "std.task.Task") {
                    let local = nname(&f[0]) == "Path" && {
                        let segs = strs(&nf(&f[0])[0]);
                        segs.len() == 1 && self.task_safe.get(&(self.d, segs[0].clone())) == Some(&true)
                    };
                    if !local {
                        self.tdiag("E4022", cancel_msg(), nline(e));
                    }
                }
                // 一時値の線形値に `&self` / `&mut self` のメソッドを呼ぶと、値は束縛も消費もされず漏れる (4.4)。
                if Self::is_temp_value(&f[0]) {
                    let t = self.lin_ty(&f[0], env);
                    if self.is_linear(&t) && self.method_borrows_self(&t, &st(&f[1]), items(&f[2]).len()) {
                        self.tdiag(
                            "E3003",
                            format!("a linear temporary is borrowed by `{}` and never consumed (bind it, or consume it)", st(&f[1])),
                            nline(e),
                        );
                    }
                }
                self.mcall_moves_out_of_ref(e, env);
                self.tc_expr(&f[0], env, muts, false);
                // `self` を値で受けるメソッド (`x.into_name()` など) はレシーバを move する (4.4)。
                // レシーバが局所の場所 (`x` や `s.f`) のときだけ move_sites に載せる
                // (型が解決できない場所や Copy の型では黙る)。
                if let Some((root, path)) = self.mcall_recv_place(&f[0]) {
                    if self.method_takes_self_by_value(&root, &path, &st(&f[1]), env) {
                        self.use_place(&root, &path, env, nline(&f[0]), true);
                    }
                }
                for a in items(&f[2]) {
                    self.tc_expr(a, env, muts, true);
                }
            }
            "Unary" if matches!(st(&f[0]).as_str(), "&" | "&mut") => {
                // 一時値の借用 (`&t.open(1)`): 借りた先は束縛も消費もされないので、線形なら漏れる (4.4)。
                self.lin_temp_unused(&f[1], env, nline(e));
                self.tc_expr(&f[1], env, muts, false)
            }
            "Unary" if st(&f[0]) == "*" => {
                // `*r` を値として使う (move) のは、参照の先が Copy のときだけ (4.1)。
                // `let z = s` で `s` が参照越しの `match` 束縛 (ref_binds) なら `*z` も同様に E3002 (f23)。
                let operand = &f[1];
                let deref_non_copy = self.ety_is_ref(operand) && self.ety_non_copy(e)
                    || {
                        nname(operand) == "Path"
                            && {
                                let segs = strs(&nf(operand)[0]);
                                segs.len() == 1
                                    && self.ref_binds.contains(&segs[0])
                                    && env.get(&segs[0]).is_some_and(|t| {
                                        let mut cur = t.clone();
                                        if tag(&cur) == "ref" {
                                            cur = self.strip(&cur).clone();
                                        }
                                        !is_nil(&cur) && !self.is_generic(&cur, &self.names) && !self.is_copy(&cur)
                                    })
                            }
                    };
                if consume && deref_non_copy {
                    let text = format!("*{}", short_text(operand));
                    self.tdiag(
                        "E3002",
                        format!("cannot move out of `{text}`: it is behind a reference and its type is not Copy; borrow it with `&{text}` or call `.clone()`"),
                        nline(e),
                    );
                }
                self.tc_expr(operand, env, muts, false);
            }
            // `self.f` (`self` は `Path` の 1 要素で、`x.f` のように 1 つのパスにまとまらない) と、
            // 根が局所変数の `(x).f` は、場所 (根 + フィールド道) として使う。
            "Field" if self.field_place(e).is_some_and(|(r, _)| env.contains_key(&r)) => {
                let (root, path) = self.field_place(e).unwrap_or_default();
                self.use_place(&root, &path, env, nline(e), consume);
            }
            // 参照の先・関数の戻り値の参照の先のフィールド (`(*r).f`、`t.id(s).f`) を、Copy でない型のまま値として使う。
            "Field" if consume && self.field_behind_ref(e) && self.ety_non_copy(e) => {
                let mut b = &f[0];
                while nname(b) == "Field" {
                    b = &nf(b)[0];
                }
                let text = format!("{}.{}", short_text(b), st(&f[1]));
                self.tdiag(
                    "E3002",
                    format!("cannot move out of `{text}`: it is behind a reference and its type is not Copy; borrow it with `&{text}` or call `.clone()`"),
                    nline(e),
                );
                self.tc_expr(&f[0], env, muts, false);
            }
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
                let fl = self.lin_floor;
                let what = if self.lin_in_closure { "the closure" } else { "the function" };
                self.lin_leaks(fl, nline(e), &format!("when {what} returns early at line {}", nlineno(e)));
            }
            "Field" if consume && {
                let mut b = &f[0];
                while nname(b) == "Field" {
                    b = &nf(b)[0];
                }
                nname(b) == "Index"
            } =>
            {
                let mut b = &f[0];
                while nname(b) == "Field" {
                    b = &nf(b)[0];
                }
                let text = format!("{}.{}", index_text(b), st(&f[1]));
                self.field_consumes.push((self.def(self.d).name.clone(), nid(e), text, nline(e)));
                self.tc_expr(&f[0], env, muts, false);
            }
            "Field" => {
                // 一時値の欄の読み出し (`t.open(1).id`): 基の線形な値は束縛も消費もされず漏れる。
                // ただし消費の位置で線形な欄を取り出すなら、その欄が義務を引き継ぐ (`t.mk().inner` を渡す)。
                // 一時値の線形な欄の一部だけを消費して一時値を捨てると、他の線形な欄が漏れる
                // (`t.mkpair().a` は `a` だけ消費して `b` を漏らす)。
                let fld_linear = self.is_linear(&self.lin_ty(e, env));
                if consume && fld_linear && Self::is_temp_value(&f[0]) {
                    let tt = self.lin_ty(&f[0], env);
                    if self.is_linear(&tt) {
                        let consumed = st(&f[1]).to_string();
                        let has_other = self
                            .split_fields(&tt)
                            .iter()
                            .any(|(n, _)| n != &consumed);
                        if has_other {
                            self.tdiag(
                                "E3003",
                                format!("a linear temporary's field `{consumed}` is consumed but the temporary is not fully consumed (other linear fields are leaked)"),
                                nline(e),
                            );
                        }
                    }
                } else {
                    self.lin_temp_unused(&f[0], env, nline(e));
                }
                for c in children(e) {
                    if !is_pat(c) {
                        self.tc_expr(c, env, muts, false);
                    }
                }
            }
            "Index" => {
                // `a[i]` で Copy でない要素を値として取り出す (move) は 4.1 に反する (E3002)。
                // `&a[i]`/`&mut a[i]` は消費 (consume=false) にならないので対象外。
                // 要素の型は型表 (`tytab`) で後から引く (tc_expr 時点では Vec 等が解決しないため)。
                // 即時には記録だけしておく。
                if consume {
                    let base_nid = nid(&f[0]);
                    self.index_consumes.push((
                        self.def(self.d).name.clone(),
                        base_nid,
                        index_text(e),
                        nline(e),
                    ));
                }
                // 添字演算は基の場所を move しない (参照として扱う)。
                self.tc_expr(&f[0], env, muts, false);
                self.tc_expr(&f[1], env, muts, false);
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

    /// 型 `t` のメソッド `name` (引数 `n` 個) が、`self` を参照で受けるか。
    /// `funcs`/`sigs` (stdlib も含む) を FQN (`型.メソッド`) で引く。`Option`/`Result` は
    /// 標準ライブラリの FQN に置き換え、配列・スライスは `core.array`/`core.slice` を引く。
    /// 分からなければ false (黙る)。
    fn method_borrows_self(&self, t: &V, name: &str, n: usize) -> bool {
        let tname: String = match tag(t).as_str() {
            "named" => match st(tel(t, 1)).as_str() {
                "Option" => "core.option.Option".to_string(),
                "Result" => "core.result.Result".to_string(),
                nm => nm.to_string(),
            },
            "array" => "core.array".to_string(),
            "slice" => "core.slice".to_string(),
            _ => return false,
        };
        let fq = format!("{tname}.{name}");
        let Some(&i) = self.p.funcs.get(&fq).or_else(|| self.p.sigs.get(&fq)) else {
            return false;
        };
        let c = self.def(i);
        c.params.len() == n + 1 && c.params.first().is_some_and(|(pn, pt)| pn == "self" && tag(pt) == "ref")
    }

    /// 型推論 (types.rs) が決めた、式 `e` の型 (今の関数の式 ID から引く)。
    fn ety(&self, e: &V) -> Option<&crate::types::Ty> {
        let id = nid(e);
        if id == 0 {
            return None;
        }
        self.mv_types.get(&(self.def(self.d).name.clone(), id)).map(|x| &x.ty)
    }

    fn ety_is_ref(&self, e: &V) -> bool {
        matches!(self.ety(e), Some(crate::types::Ty::Ref(..)))
    }

    /// 式 `e` の型が、確実に Copy でない型か (決まらない・型パラメータは false = 黙る)。
    fn ety_non_copy(&self, e: &V) -> bool {
        self.ety(e).is_some_and(|t| self.ty_certainly_not_copy(t))
    }

    fn ty_certainly_not_copy(&self, t: &crate::types::Ty) -> bool {
        use crate::types::Ty;
        match t {
            Ty::Unknown | Ty::Var(_) => false,
            // 型パラメータに Copy の境界が無ければ、`*r` で値を取り出すのは不正 (e11)。
            Ty::Param(p) => !self.generic_may_be_copy(p),
            Ty::Tuple(xs) => xs.iter().any(|x| self.ty_certainly_not_copy(x)),
            Ty::Array(el, _) => self.ty_certainly_not_copy(el),
            Ty::Named(nm, args) if nm == "Option" || nm == "Result" => args.iter().any(|a| self.ty_certainly_not_copy(a)),
            _ => !self.is_copy_ty(t),
        }
    }

    /// `Field` の鎖 `a.f.g` の根が局所の単一の名前 (`self` など。`Path` が 1 要素) のとき、(根, 道)。
    fn field_place(&self, e: &V) -> Option<(String, Vec<String>)> {
        let mut path = vec![];
        let mut b = e;
        while nname(b) == "Field" {
            path.push(st(&nf(b)[1]));
            b = &nf(b)[0];
        }
        if nname(b) != "Path" {
            return None;
        }
        let segs = strs(&nf(b)[0]);
        if segs.len() != 1 {
            return None;
        }
        path.reverse();
        Some((segs[0].clone(), path))
    }

    /// `Field` の鎖の根が、参照を返す式 (`*r`、参照を返す呼び出し) か。そこからフィールドを move することはできない。
    fn field_behind_ref(&self, e: &V) -> bool {
        let mut b = e;
        while nname(b) == "Field" {
            b = &nf(b)[0];
        }
        match nname(b) {
            "Unary" => st(&nf(b)[0]) == "*" && self.ety_is_ref(&nf(b)[1]),
            "Call" | "MCall" => self.ety_is_ref(b),
            _ => false,
        }
    }

    /// `let f = <rhs>` の右辺が、Copy でないクロージャの値か。クロージャ式で、`move` で Copy でない値 (や Copy でない
    /// クロージャ) を取り込む・`&mut` で取り込むとき。別の Copy でないクロージャの名前 (`let g = f`) もそう。型が決まらない取り込みは黙る。
    fn closure_value_non_copy(&self, rhs: &V, env: &Env) -> bool {
        match nname(rhs) {
            "Closure" => items(&nf(rhs)[2]).iter().any(|c| {
                let name = st(tel(c, 1));
                match st(tel(c, 0)).as_str() {
                    "move" => {
                        self.noncopy_closures.contains(&name)
                            || env.get(&name).is_some_and(|t| !is_nil(t) && !self.is_generic(t, &self.names) && !self.is_copy(t))
                    }
                    "&mut" => true,
                    _ => false,
                }
            }),
            "Path" => {
                let segs = strs(&nf(rhs)[0]);
                segs.len() == 1 && self.noncopy_closures.contains(&segs[0])
            }
            _ => false,
        }
    }

    /// `match` の腕のパターン `pat` の束縛名と、決まる型 (パターンからの型、無ければ型推論の `let` の表)。
    fn arm_bindings(&self, pat: &V, sty: &V, line: usize) -> Vec<(String, V)> {
        let mut typed: Vec<(String, V)> = vec![];
        if !is_nil(sty) {
            self.bind_pat_types(pat, sty, &mut typed);
        }
        let mut names = vec![];
        pat_binds(pat, &mut names);
        names
            .into_iter()
            .map(|n| {
                let mut t = typed.iter().find(|(m, _)| *m == n).map(|(_, t)| t.clone()).unwrap_or(V::Nil);
                if is_nil(&t) {
                    if let Some(ty) = self.let_tys.get(&(self.d, line, n.clone())) {
                        t = crate::types::TypeCk::to_v(ty).unwrap_or(V::Nil);
                    }
                }
                (n, t)
            })
            .collect()
    }

    /// スクラティニー `e` が、move できない場所か: `*r`・参照越しのフィールド・添字の先・参照を返す式のフィールド。
    fn scrut_behind_ref(&self, e: &V, env: &Env) -> bool {
        match nname(e) {
            "Unary" => st(&nf(e)[0]) == "*" && self.ety_is_ref(&nf(e)[1]),
            "Index" => true,
            "Field" => {
                let mut b = e;
                while nname(b) == "Field" {
                    b = &nf(b)[0];
                }
                nname(b) == "Index" || self.field_behind_ref(e) || self.field_place(e).is_some_and(|(r, p)| self.path_through_ref(&r, &p, env))
            }
            "Path" => {
                let segs = strs(&nf(e)[0]);
                segs.len() >= 2 && self.path_through_ref(&segs[0], &segs[1..], env)
            }
            _ => false,
        }
    }

    /// 場所 `root.path` の道の途中に参照があるか (根が参照のときを含む)。型が分からなければ false。
    fn path_through_ref(&self, root: &str, path: &[String], env: &Env) -> bool {
        let Some(t) = env.get(root) else { return false };
        let mut cur = t.clone();
        let mut through = false;
        for seg in path {
            if is_nil(&cur) {
                return false;
            }
            if tag(&cur) == "ref" {
                through = true;
                cur = self.strip(&cur).clone();
            }
            match self.field_ty(&cur, seg) {
                Some(n) => cur = n,
                None => return false,
            }
        }
        through
    }

    /// スクラティニー `e` が局所の場所 (変数・フィールドの道) ならその (根, 道)。
    fn scrut_place(&self, e: &V, env: &Env) -> Option<(String, Vec<String>)> {
        let (root, path) = match nname(e) {
            "Path" => {
                let segs = strs(&nf(e)[0]);
                (segs[0].clone(), segs[1..].to_vec())
            }
            "Field" => self.field_place(e)?,
            _ => return None,
        };
        env.contains_key(&root).then_some((root, path))
    }

    /// 場所 `root.path` の型のメソッド `name` が、`self` を値で受けるか。
    /// レシーバの型が分からない・メソッドが見つからないときは `false` (消費として扱わない)。
    fn method_takes_self_by_value(&self, root: &str, path: &[String], name: &str, env: &Env) -> bool {
        match self.method_def(root, path, name, env).and_then(|d| d.params.first()) {
            Some((pn, pt)) => pn == "self" && tag(pt) != "ref",
            None => false,
        }
    }

    /// メソッド呼び出し `e.m(..)` のレシーバ `e` が局所の場所 (変数・フィールドの道) ならその (根, 道)。
    /// 呼び出し式など場所でないレシーバなら None (move の元を特定できず、黙る)。
    fn mcall_recv_place(&self, e: &V) -> Option<(String, Vec<String>)> {
        match nname(e) {
            "Path" => {
                let segs = strs(&nf(e)[0]);
                let root = segs.first()?;
                Some((root.clone(), segs[1..].to_vec()))
            }
            "Field" => self.field_place(e),
            _ => None,
        }
    }

    /// 場所 `root.path` の型 (参照は剥がす)。道の途中か最後に参照があれば true を添える。分からなければ None。
    fn recv_ty_through_ref(&self, root: &str, path: &[String], env: &Env) -> Option<(V, bool)> {
        let mut cur = env.get(root)?.clone();
        let mut through = false;
        for seg in path {
            if is_nil(&cur) {
                return None;
            }
            if tag(&cur) == "ref" {
                through = true;
                cur = self.strip(&cur).clone();
            }
            cur = self.field_ty(&cur, seg)?;
        }
        if is_nil(&cur) {
            return None;
        }
        if tag(&cur) == "ref" {
            through = true;
            cur = self.strip(&cur).clone();
        }
        Some((cur, through))
    }

    /// 型 `ty` (`named`) のメソッド `name` の定義。
    fn method_of_named(&self, ty: &V, name: &str) -> Option<&'a Def> {
        if tag(ty) != "named" {
            return None;
        }
        let tname = match st(tel(ty, 1)).as_str() {
            "Option" => "core.option.Option".to_string(),
            "Result" => "core.result.Result".to_string(),
            n => n.to_string(),
        };
        let fq = format!("{}.{}", tname, name);
        self.p.funcs.get(&fq).or_else(|| self.p.sigs.get(&fq)).map(|&i| self.def(i))
    }

    fn def_takes_self_by_value(&self, d: &Def) -> bool {
        matches!(d.params.first(), Some((pn, pt)) if pn == "self" && tag(pt) != "ref")
    }

    /// `recv.m(..)` の `m` が `self` を値で受け、レシーバが参照の先・添字の先の場所 (または参照そのもの) で、
    /// その型が Copy でないなら E3002 (4.1: 参照の先からは move できない)。型は型表から引く。
    /// `recv.m(..)` の `m` が `self` を値で受け、レシーバが参照の先・添字の先の場所 (または参照そのもの) で、
    /// その型が Copy でないなら E3002 (4.1: 参照の先からは move できない)。
    /// `ety` はフィールド越しの型 (`self.opt` where `self: &t.O`) を持たないことがあるので、
    /// その場合は根の型から `recv_ty_through_ref` で辿る。
    fn mcall_moves_out_of_ref(&mut self, e: &V, env: &Env) {
        let f = nf(e);
        let recv = &f[0];
        let name = st(&f[1]);
        let (pointee, through) = match nname(recv) {
            "Unary" if st(&nf(recv)[0]) == "*" => {
                let Some(rt) = self.ety(&nf(recv)[1]).cloned() else { return };
                match rt {
                    crate::types::Ty::Ref(_, inner) => (crate::types::TypeCk::to_v(&*inner).unwrap_or(V::Nil), true),
                    _ => return,
                }
            }
            "Field" => {
                let Some((root, path)) = self.field_place(recv) else { return };
                self.recv_ty_through_ref(&root, &path, env).unwrap_or((V::Nil, false))
            }
            "Index" => {
                let Some(rt) = self.ety(recv).cloned() else { return };
                (crate::types::TypeCk::to_v(&rt).unwrap_or(V::Nil), true)
            }
            _ => {
                let Some(rt) = self.ety(recv).cloned() else { return };
                match rt {
                    crate::types::Ty::Ref(_, inner) => (crate::types::TypeCk::to_v(&*inner).unwrap_or(V::Nil), true),
                    _ => return,
                }
            }
        };
        if !through || is_nil(&pointee) || self.is_copy(&pointee) {
            return;
        }
        if self.method_of_named(&pointee, &name).is_some_and(|m| self.def_takes_self_by_value(m)) {
            self.tdiag(
                "E3002",
                format!(
                    "cannot move out of `{}`: `{name}` takes `self` by value, but the receiver is behind a reference or an index; call it on an owned value or clone first",
                    short_text(recv)
                ),
                nline(e),
            );
        }
    }

    /// 場所 `root.path` の型のメソッド `name` の定義。レシーバの型・メソッドが分からなければ None。
    fn method_def(&self, root: &str, path: &[String], name: &str, env: &Env) -> Option<&'a Def> {
        let ty = self.place_ty(root, path, env)?;
        if tag(&ty) != "named" {
            return None;
        }
        let tname = match st(tel(&ty, 1)).as_str() {
            "Option" => "core.option.Option".to_string(),
            "Result" => "core.result.Result".to_string(),
            n => n.to_string(),
        };
        let fq = format!("{}.{}", tname, name);
        self.p.funcs.get(&fq).or_else(|| self.p.sigs.get(&fq)).map(|&i| self.def(i))
    }

    /// 式を評価し終えたあとに、後続の文へ制御が進まないか (`return` / `break` / `continue` / panic)。
    /// 分からない形は `false` (進むとみなす = move を合流に残す側)。
    fn diverges(&self, e: &V) -> bool {
        let f = nf(e);
        match nname(e) {
            "Return" | "Break" | "Continue" => true,
            "ExprStmt" => self.diverges(&f[0]),
            "Block" => items(&f[0]).iter().any(|s| self.diverges(s)),
            "If" => !is_nil(&f[2]) && self.diverges(&f[1]) && self.diverges(&f[2]),
            "Match" => {
                let arms = items(&f[1]);
                !arms.is_empty() && arms.iter().all(|a| self.diverges(&items(a)[1]))
            }
            "Call" if nname(&f[0]) == "Path" => {
                let fq = self.p.expand(&strs(&nf(&f[0])[0]), &self.def(self.d).file).join(".");
                fq == "core.panic.panic" || fq == "core.panic.todo"
            }
            _ => false,
        }
    }

    /// `for` の本体の中に、このループを抜ける `break` または `return` があるか。
    /// `break` はネストしたループ (`loop`/`while`/`for`) の中ならそちらを抜けるだけなので、
    /// その内部の `break` は無視する。`return` は関数を抜けるのでどこでも早期抜けとする。
    fn for_body_may_exit(&self, body: &V) -> bool {
        self.for_body_may_exit_at(body, 0)
    }

    fn for_body_may_exit_at(&self, e: &V, depth: usize) -> bool {
        match nname(e) {
            "Return" => true,
            "Break" => depth == 0,
            "Loop" | "While" | "For" => {
                // ネストしたループの中は深さを増す (そこの `break` は外の `for` を抜けない)。
                for c in children(e) {
                    if self.for_body_may_exit_at(c, depth + 1) {
                        return true;
                    }
                }
                false
            }
            _ => {
                for c in children(e) {
                    if self.for_body_may_exit_at(c, depth) {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// move の集合の和 (同じ場所は先に記録した行を残す)。
    fn union_moves(into: &mut BTreeMap<String, usize>, from: &BTreeMap<String, usize>) {
        for (k, v) in from {
            into.entry(k.clone()).or_insert(*v);
        }
    }

    /// 分岐とループでの move の合流 (4.1)。どれかの枝で move された場所は、合流後は move 済みとして
    /// 扱う (後続へ進まない枝は除く)。ループの外で宣言した値を本体で move し、そのまま次の周回に
    /// 入る経路があれば E3002 (2 周目で move 済みの値を使うことになる)。
    fn branchy(&mut self, e: &V, env: &Env, muts: &BTreeSet<String>, consume: bool) {
        let f = nf(e);
        match nname(e) {
            "If" => {
                self.tc_expr(&f[0], env, muts, false);
                let base = self.moved.clone();
                let (lbase, lstate) = (self.lin.len(), self.lin.clone());
                let ln = nlineno(e);
                self.tc_expr(&f[1], env, muts, consume);
                let a = self.moved.clone();
                let a_div = self.diverges(&f[1]);
                self.lin.truncate(lbase);
                let la = std::mem::replace(&mut self.lin, lstate.clone());
                self.moved = base.clone();
                let mut b_div = false;
                if !is_nil(&f[2]) {
                    self.tc_expr(&f[2], env, muts, consume);
                    b_div = self.diverges(&f[2]);
                }
                self.lin.truncate(lbase);
                let mut inputs = vec![];
                if !a_div {
                    inputs.push((format!("the `if` at line {ln} is true"), la));
                }
                if !b_div {
                    inputs.push((format!("the `if` at line {ln} is false"), self.lin.clone()));
                }
                self.lin = lstate;
                self.lin_merge(inputs, lbase, nline(e));
                let b = self.moved.clone();
                let mut m = base;
                if !a_div {
                    Self::union_moves(&mut m, &a);
                }
                if !b_div {
                    Self::union_moves(&mut m, &b);
                }
                // 枝の中で宣言した束縛は、合流後には見えない (同じ名前の別の束縛と混同しない)。
                m.retain(|k, _| env.contains_key(k.split('.').next().unwrap_or(k)));
                self.moved = m;
            }
            "Match" => {
                self.tc_expr(&f[0], env, muts, false);
                let sty0 = self.ty_of(&f[0], env);
                let sref = self.ety_is_ref(&f[0]) || tag(&sty0) == "ref";
                let smut = matches!(self.ety(&f[0]), Some(crate::types::Ty::Ref(true, _)))
                    || (tag(&sty0) == "ref" && matches!(tel(&sty0, 1), V::B(true)));
                // 腕が Copy でない値を束縛して取り出すなら、スクラティニーの場所は move される (4.1)。
                // 参照の先・添字の先からは move できない (E3002)。スクラティニーが参照そのものなら、束縛は `&T`。
                let moving = !sref
                    && items(&f[1]).iter().any(|a| {
                        self.arm_bindings(&items(a)[0], &sty0, nline(&items(a)[0]))
                            .iter()
                            .any(|(n, t)| n != "_" && !is_nil(t) && !self.is_generic(t, &self.names) && !self.is_copy(t))
                    });
                if moving {
                    if self.scrut_behind_ref(&f[0], env) {
                        self.tdiag(
                            "E3002",
                            format!(
                                "cannot move out of `{}` in a `match`: it is behind a reference or an index and the pattern binds a value that is not Copy; match on `&{}` instead",
                                short_text(&f[0]),
                                short_text(&f[0])
                            ),
                            nline(e),
                        );
                    } else if let Some((root, path)) = self.scrut_place(&f[0], env) {
                        if !path.is_empty() || sty0_is_enumlike(&sty0) {
                            self.record_move(&root, &path, env, nline(e));
                        }
                    }
                }
                let base = self.moved.clone();
                // 腕の終わりの move の和。各腕は `base` から始まるので、全腕が同じ場所を復活させれば (`rx = back`) 復活が残る。
                let mut merged: Option<BTreeMap<String, usize>> = None;
                // スクラティニーの型が分かれば、腕の束縛に型を流す (線形値の未消費 E3003 のため)。
                let sty = self.ty_of(&f[0], env);
                // 線形値を `match` する (パターンで分解する) と、その値は消費される。
                if nname(&f[0]) == "Path" {
                    let segs = strs(&nf(&f[0])[0]);
                    if env.contains_key(&segs[0]) {
                        self.lin_consume_place(&segs[0], &segs[1..], env);
                    }
                }
                let (lbase, lstate) = (self.lin.len(), self.lin.clone());
                let mut inputs = vec![];
                for arm in items(&f[1]) {
                    self.lin = lstate.clone();
                    self.moved = base.clone();
                    let mut inner = env.clone();
                    let mut b = vec![];
                    pat_binds(&items(arm)[0], &mut b);
                    let mut typed: Vec<(String, V)> = vec![];
                    if !is_nil(&sty) {
                        self.bind_pat_types(&items(arm)[0], &sty, &mut typed);
                        self.lin_wild(&typed, nline(&items(arm)[0]));
                        self.lin_wild_top(&items(arm)[0], &sty, nline(&items(arm)[0]));
                    }
                    for n in b {
                        let mut t = typed.iter().find(|(m, _)| *m == n).map(|(_, t)| t.clone()).unwrap_or(V::Nil);
                        if is_nil(&t) {
                            // 自前で決まらない腕の束縛は、型推論の結果を使う (4.1)。
                            if let Some(ty) = self.let_tys.get(&(self.d, nline(&items(arm)[0]), n.clone())) {
                                t = crate::types::TypeCk::to_v(ty).unwrap_or(V::Nil);
                            }
                        }
                        if sref && is_nil(&t) && !is_nil(&sty0) {
                            let inner_ty = self.strip(&sty0).clone();
                            t = self
                                .arm_bindings(&items(arm)[0], &inner_ty, nline(&items(arm)[0]))
                                .into_iter()
                                .find(|(m, _)| *m == n)
                                .map(|(_, t)| t)
                                .unwrap_or(V::Nil);
                        }
                        // 参照越しの照合で得た Copy でない値は、参照 (`&T` / `&mut T`) として束縛される。
                        // 参照越しの照合で得た Copy でない値は、参照 (`&T` / `&mut T`) として束縛される。
                        if sref && !is_nil(&t) && tag(&t) != "ref" && !self.is_generic(&t, &self.names) && !self.is_copy(&t) {
                            t = V::L(vec![V::S("ref".into()), V::B(smut), t]);
                            self.ref_binds.insert(n.clone());
                        } else {
                            self.ref_binds.remove(&n);
                        }
                        // 腕の束縛は新しい値。以前の同名の束縛の move 記録を引き継がない。
                        self.revive_place(std::slice::from_ref(&n));
                        self.lin_declare(&n, &t, nline(arm));
                        inner.insert(n, t);
                    }
                    self.tc_expr(&items(arm)[1], &inner, muts, consume);
                    let div = self.diverges(&items(arm)[1]);
                    if div {
                        self.lin.truncate(lbase);
                    } else {
                        self.lin_scope_end(lbase, "when the `match` arm ends");
                        inputs.push((format!("the `match` arm at line {} is taken", nlineno(&items(arm)[0])), self.lin.clone()));
                        let end = self.moved.clone();
                        Self::union_moves(merged.get_or_insert_with(BTreeMap::new), &end);
                    }
                }
                let mut merged = merged.unwrap_or(base);
                self.lin = lstate;
                self.lin_merge(inputs, lbase, nline(e));
                // 腕の束縛は、合流後には見えない。
                merged.retain(|k, _| env.contains_key(k.split('.').next().unwrap_or(k)));
                self.moved = merged;
                self.exhaustive(e, env);
            }
            n => {
                let mut inner = env.clone();
                let mut loop_binds = false;
                // `for` の束縛とその型。線形な要素は、ループの本体の中で消費の義務を負う (4.4)。
                let mut loop_tys: Vec<(String, V)> = vec![];
                // `while` の条件は毎周評価される: 条件の中の move も 2 周目の検査に入れるため、条件より前の状態を基にする。
                let mut pre_cond: Option<BTreeMap<String, usize>> = None;
                // 条件を評価した直後の move (本体へ入る前)。条件が偽でループを出る経路は、これを持つ。
                let mut after_cond: Option<BTreeMap<String, usize>> = None;
                let mut linear_owned = false;
                let body = match n {
                    "For" => {
                        // 所有する `Vec` や配列を `for x in v` で回すと、`v` は move 済みになる (Rust と同じ、4.1)。
                        // `v.iter()` や `&v` は借用で、消費にならない。それ以外の反復子 (利用者の `Iterator`) は
                        // 従来どおり、`for` が `it` をその場で進める (仕様が未規定の所)。
                        let it_ty = self.lin_ty(&f[1], env);
                        let owned = tag(&it_ty) == "array" || (tag(&it_ty) == "named" && st(tel(&it_ty, 1)) == "alloc.vec.Vec");
                        let before = self.moved.clone();
                        self.tc_expr(&f[1], env, muts, true);
                        if !owned {
                            self.moved = before;
                        }
                        // 要素の型: 所有する反復子の要素なら、線形な要素の束縛は義務を負う (`for _r in a` は E3003)。
                        // `break`/`return` で抜けると残りの要素が漏れる (4.4)。
                        let el = self.elem_ty(&it_ty);
                        linear_owned = owned && !is_nil(&el) && self.is_linear(&el);
                        let mut typed: Vec<(String, V)> = vec![];
                        self.bind_pat_types(&f[0], &el, &mut typed);
                        self.lin_wild(&typed, nline(e));
                        self.lin_wild_top(&f[0], &el, nline(e));
                        let mut b = vec![];
                        pat_binds(&f[0], &mut b);
                        for x in b {
                            let mut t = self
                                .let_tys
                                .get(&(self.d, nline(e), x.clone()))
                                .and_then(crate::types::TypeCk::to_v)
                                .unwrap_or(V::Nil);
                            if is_nil(&t) {
                                t = typed.iter().find(|(m, _)| *m == x).map(|(_, t)| t.clone()).unwrap_or(V::Nil);
                            }
                            // 参照 (`&Vec<T>` / `&[T]`) の反復の束縛は `&T`。所有値として消費させない。
                            let src_ref = self.ety_is_ref(&f[1]) || tag(&it_ty) == "ref";
                            if src_ref && !is_nil(&t) && tag(&t) != "ref" && !self.is_generic(&t, &self.names) && !self.is_copy(&t) {
                                let smut = matches!(self.ety(&f[1]), Some(crate::types::Ty::Ref(true, _)))
                                    || (tag(&it_ty) == "ref" && matches!(tel(&it_ty, 1), V::B(true)));
                                t = V::L(vec![V::S("ref".into()), V::B(smut), t]);
                                self.ref_binds.insert(x.clone());
                            } else {
                                self.ref_binds.remove(&x);
                            }
                            inner.insert(x.clone(), t.clone());
                            loop_tys.push((x, t));
                        }
                        loop_binds = true;
                        &f[2]
                    }
                    "While" => {
                        pre_cond = Some(self.moved.clone());
                        // 条件の中の消費 (`while t.eat(s) { .. }`) は move になる。
                        self.tc_expr(&f[0], env, muts, true);
                        after_cond = Some(self.moved.clone());
                        &f[1]
                    }
                    _ => &f[0],
                };
                let base = pre_cond.unwrap_or_else(|| self.moved.clone());
                let (lbase, lstate) = (self.lin.len(), self.lin.clone());
                self.lin_loops.push(LinLoop { base: lbase, breaks: vec![], conts: vec![] });
                if loop_binds {
                    for (x, t) in &loop_tys {
                        self.lin_declare(x, t, nline(e));
                    }
                }
                self.loop_exits.push((BTreeMap::new(), BTreeMap::new()));
                self.tc_expr(body, &inner, muts, false);
                if linear_owned && self.for_body_may_exit(body) {
                    let text = match nname(&f[1]) {
                        "Path" => {
                            let segs = strs(&nf(&f[1])[0]);
                            segs.first().cloned().unwrap_or_else(|| "the iterator".to_string())
                        }
                        _ => "the iterator".to_string(),
                    };
                    self.tdiag(
                        "E3003",
                        format!("linear value `{text}` may not have all elements consumed when the loop exits early (break or `return`)"),
                        nline(e),
                    );
                }
                let body_div = self.diverges(body);
                if body_div {
                    self.lin.truncate(lbase);
                } else {
                    self.lin_scope_end(lbase, "when the loop body ends");
                }
                let lp = self.lin_loops.pop().unwrap();
                let ln = nlineno(e);
                let mut inputs = vec![];
                if n != "Loop" {
                    inputs.push((format!("the loop at line {ln} runs zero times"), lstate.clone()));
                    if !body_div {
                        inputs.push((format!("the loop at line {ln} finishes an iteration"), self.lin.clone()));
                    }
                    inputs.extend(lp.conts);
                }
                inputs.extend(lp.breaks);
                self.lin = lstate;
                if inputs.is_empty() {
                    // `break` のない `loop`: 後ろへは進まない。
                    for x in self.lin.iter_mut() {
                        x.live = false;
                    }
                } else {
                    self.lin_merge(inputs, lbase, nline(e));
                }
                let (breaks, continues) = self.loop_exits.pop().unwrap_or_default();
                let end = self.moved.clone();
                // 次の周回へ入る経路: 本体の末尾 (後続へ進むとき) と `continue`。
                let mut back = continues;
                if !self.diverges(body) {
                    Self::union_moves(&mut back, &end);
                }
                for (k, line) in &back {
                    if base.contains_key(k) {
                        continue;
                    }
                    let root = k.split('.').next().unwrap_or(k);
                    if env.contains_key(root) {
                        self.tdiag(
                            "E3002",
                            format!("value `{k}` is moved inside a loop (at line {}) and would be used again in the next iteration", crate::parser::unpack_pos(*line).0),
                            *line,
                        );
                    }
                }
                // ループを抜ける経路: `break`、および条件・反復子による終了 (`loop` にはない)。
                let mut out = base;
                Self::union_moves(&mut out, &breaks);
                if n != "Loop" {
                    Self::union_moves(&mut out, &back);
                }
                // `while` は、条件を評価した直後 (本体へ入る前) の move の状態でも出る。本体で再代入しても、
                // 条件で move した値は、条件が偽になって出た経路では復活していない。
                if let Some(ac) = &after_cond {
                    Self::union_moves(&mut out, ac);
                }
                // 本体の中で宣言した束縛は、ループの外では見えない。
                out.retain(|k, _| env.contains_key(k.split('.').next().unwrap_or(k)));
                self.moved = out;
            }
        }
    }

    fn exhaustive(&mut self, m: &V, env: &Env) {
        let f = nf(m);
        let pats: Vec<&V> = items(&f[1]).iter().map(|a| &items(a)[0]).collect();
        // `if let` / `is` が作る「他の全部」の腕 (`PWild [true]`) は、全列挙の強制の対象外。
        if pats.iter().any(|p| nname(p) == "PWild" && !nf(p).is_empty()) {
            return;
        }
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

    /// 式の型。`ty_of` で決まらなければ型表 (`mv_types`) から引く (汎用関数の戻り値など、4.4 の検査が見る型)。
    fn lin_ty(&self, e: &V, env: &Env) -> V {
        let t = self.ty_of(e, env);
        if !is_nil(&t) {
            return t;
        }
        match self.mv_types.get(&(self.def(self.d).name.clone(), nid(e))) {
            Some(et) => crate::types::TypeCk::to_v(&et.ty).unwrap_or(V::Nil),
            None => V::Nil,
        }
    }

    /// 所有する反復子 (配列・スライス・`Vec`) の要素の型。借用 (`&v`) や他の型は Nil (黙る)。
    fn elem_ty(&self, it: &V) -> V {
        match tag(it).as_str() {
            "array" | "slice" => tel(it, 1).clone(),
            "named" if st(tel(it, 1)) == "alloc.vec.Vec" => items(tel(it, 2)).first().cloned().unwrap_or(V::Nil),
            _ => V::Nil,
        }
    }

    /// 束縛を宣言する。線形な型なら消費の義務を負う (4.4)。線形でない束縛 (型が決まらないものを含む) も、
    /// 同名の外側の束縛を隠す目印として積む。
    /// 取り消しの検査の印を、開いているすべての `spawn_local` の閉包について立てる (解放の要る値などを見つけたとき)。
    fn spawn_unsafe(&mut self) {
        for u in self.spawn_scan.iter_mut() {
            *u = true;
        }
    }

    /// 呼び出しの解決先 `(FQN, 中断しうるか)`。中断しうるのは `uses` に `suspend` を持つ関数だけ (中断しない関数は
    /// 呼び出しの間しか生きず、枠の値にならない)。ローカル変数の呼び出し・解決できない呼び出しは `None`。
    fn callee_target(&self, fnn: &V, env: &Env) -> Option<(String, bool)> {
        if nname(fnn) != "Path" {
            return None;
        }
        let segs = strs(&nf(fnn)[0]);
        if env.contains_key(&segs[0]) {
            return None;
        }
        let r = self.p.resolve(&segs, &self.def(self.d).file)?;
        if !r.rest.is_empty() {
            return None;
        }
        // `std.task.yield_now` は葉の中断点 (枠を作らない)。取り消しの検査では、枠を作る呼び出しだけを数える。
        let susp = |i: usize| self.def(i).uses.iter().any(|u| u == "suspend") && !matches!(self.def(i).name.as_str(), "std.task.yield_now");
        match r.target {
            Target::Fn(i) | Target::Sig(i) => Some((self.def(i).name.clone(), susp(i))),
            Target::Native(n) => Some((n, false)),
            _ => None,
        }
    }

    /// `let t = std.task.spawn_local(閉包)` の初期化なら、閉包を調べる印を開いて `true` を返す。
    /// 取り消せる条件: 閉包の結果 (`Task<T>` の `T`) と、`move` で取り込んだ局所が、解放の要らない型であること。
    fn spawn_begin(&mut self, init: &V, env: &Env) -> bool {
        if nname(init) != "Call" {
            return false;
        }
        let args = items(&nf(init)[1]);
        let is_spawn = matches!(self.callee_target(&nf(init)[0], env), Some((n, _)) if n == "std.task.spawn_local");
        if !is_spawn || args.len() != 1 || nname(&args[0]) != "Closure" {
            return false;
        }
        let cl = &args[0];
        self.spawn_scan.push(false);
        // 結果 (`T`) は閉包の宣言の戻り値の型 (`fn() -> T`)。中断の間は枠の中にあるので、解放の要らない型に限る。
        if !frame_scalar(&nf(cl)[1]) {
            self.spawn_unsafe();
        }
        for c in items(&nf(cl)[2]) {
            let name = st(&items(c)[1]);
            if let Some(t) = env.get(&name) {
                if !frame_scalar(t) {
                    self.spawn_unsafe();
                }
            }
        }
        true
    }

    fn lin_declare(&mut self, name: &str, ty: &V, line: usize) {
        // 取り消しの検査: 閉包の中の束縛が解放の要る型なら印を立てる。同じ名前の取り消し可能な印は消す (隠す束縛)。
        if !self.spawn_scan.is_empty() && !frame_scalar(ty) {
            self.spawn_unsafe();
        }
        self.task_safe.remove(&(self.d, name.to_string()));
        // 線形な欄を持つ、線形でない構造体は、欄ごとに義務を負う。根の束縛は義務を持たず、
        // 欄の部分 move で根全体を消費済みにしない (`p.a` を渡しても `p.b` の漏れを見る)。
        let split = self.split_fields(ty);
        if !split.is_empty() {
            self.lin.push(Lin { name: name.to_string(), line, linear: false, live: false, parts: true });
            for (f, ft) in split {
                self.lin_declare(&format!("{name}.{f}"), &ft, line);
            }
            return;
        }
        let linear = !is_nil(ty) && self.is_linear(ty);
        self.lin.push(Lin { name: name.to_string(), line, linear, live: linear, parts: false });
    }

    /// 線形な欄を持つ、線形でない構造体の欄 (名前と型) の一覧。欄ごとに義務を負う対象。
    /// 構造体でない型・線形な型自身・型が決まらない型は空 (従来どおり根で扱う)。
    fn split_fields(&self, t: &V) -> Vec<(String, V)> {
        if is_nil(t) || tag(t) != "named" {
            return vec![];
        }
        let Some(&i) = self.p.types.get(&st(tel(t, 1))) else { return vec![] };
        let d = self.def(i);
        if d.kw != "type" || d.linear {
            return vec![];
        }
        d.fields
            .iter()
            .filter_map(|(n, _, _)| self.field_ty(t, n).map(|ft| (n.clone(), ft)))
            .filter(|(_, ft)| !self.is_generic(ft, &self.names) && self.is_linear(ft))
            .collect()
    }

    /// パターンの `_` が線形な値を受けている (捨てている) か。`bind_pat_types` は、入れ子の `_` を
    /// 名前 `_` の組として返す。
    fn lin_wild(&mut self, typed: &[(String, V)], line: usize) {
        for (n, t) in typed {
            if n == "_" && self.is_linear(t) {
                self.tdiag("E3003", "a linear value matched by the pattern `_` is discarded without being consumed".into(), line);
            }
        }
    }

    /// 腕の最上位のパターンが `_` で、スクラティニーが線形な値なら、その値を捨てている (4.4)。
    fn lin_wild_top(&mut self, pat: &V, ty: &V, line: usize) {
        if nname(pat) == "PWild" && self.is_linear(ty) {
            self.tdiag("E3003", "a linear value matched by the pattern `_` is discarded without being consumed".into(), line);
        }
    }

    /// 名前が指す (最も内側の) 束縛。現在の関数 / クロージャの範囲だけを見る。
    fn lin_find(&self, name: &str) -> Option<usize> {
        (self.lin_floor..self.lin.len()).rev().find(|&i| self.lin[i].name == name)
    }

    /// 名前の束縛を消費済みにする。未消費の線形値だったら true。
    fn lin_consume_name(&mut self, name: &str) -> bool {
        match self.lin_find(name) {
            Some(i) => self.lin_consume_at(i),
            None => false,
        }
    }

    /// 位置 `i` の束縛と、その下の欄の義務 (`名前.` で始まる後続の項目) を消費済みにする。
    /// 未消費の義務が 1 つでもあったら true。
    fn lin_consume_at(&mut self, i: usize) -> bool {
        let prefix = format!("{}.", self.lin[i].name);
        let mut was = false;
        for j in i..self.lin.len() {
            if (j == i || self.lin[j].name.starts_with(&prefix)) && self.lin[j].linear {
                was |= self.lin[j].live;
                self.lin[j].live = false;
            }
        }
        was
    }

    /// 場所の値を値として渡した (move した)。線形な欄の義務 (`parts` の根) は、道に当たる欄だけを消費する。
    /// それ以外は場所の型が線形なら根の束縛の義務が移る。線形でない部分 (`s.len` のような読み出し) は消費にならない。
    fn lin_consume_place(&mut self, root: &str, path: &[String], env: &Env) {
        if path.is_empty() {
            self.lin_consume_name(root);
            return;
        }
        if let Some(i) = self.lin_find(root) {
            if self.lin[i].parts {
                // 道を長い方から、最初に見つかった欄の義務を探す。見つからなければ線形でない欄の読み出し。
                for k in (1..=path.len()).rev() {
                    let key = format!("{}.{}", root, path[..k].join("."));
                    let Some(j) = (i + 1..self.lin.len()).rev().find(|&j| self.lin[j].name == key) else { continue };
                    // 道の途中で親の義務 (線形な欄・分解された欄) に当たったときは、道の型で決める。
                    let whole = k == path.len() || self.place_ty(root, path, env).is_some_and(|t| self.is_linear(&t));
                    if self.lin[j].parts {
                        if k == path.len() {
                            self.lin_consume_at(j);
                        }
                    } else if whole {
                        self.lin_consume_at(j);
                    }
                    return;
                }
                return;
            }
        }
        if let Some(t) = self.place_ty(root, path, env) {
            if self.is_linear(&t) {
                self.lin_consume_name(root);
            }
        }
    }

    /// 代入 `segs = ..` で、場所とその下の欄の義務を新しい値へ移す。古い値の未消費の義務は漏れ (E3003)、
    /// 新しい値の義務は負う (生き返らせる)。根が `parts` でなければ、根の束縛は呼び出し側が扱う。
    fn lin_assign_place(&mut self, segs: &[String], line: usize) {
        let Some(i) = self.lin_find(&segs[0]) else { return };
        let key = segs.join(".");
        let prefix = format!("{key}.");
        for j in i + 1..self.lin.len() {
            let n = self.lin[j].name.clone();
            if !(n == key || n.starts_with(&prefix)) || !self.lin[j].linear {
                continue;
            }
            if self.lin[j].live {
                let dl = crate::parser::unpack_pos(self.lin[j].line).0;
                let ln = crate::parser::unpack_pos(line).0;
                let (root, note) = lin_label(&n);
                self.tdiag(
                    "E3003",
                    format!("linear value `{root}` (declared at line {dl}) is not consumed before the assignment at line {ln} overwrites it{note}"),
                    line,
                );
            }
            self.lin[j].live = true;
        }
    }

    /// `from` 以降の束縛のうち未消費の線形値を、この経路で漏れるものとして報告する (束縛は残す)。
    fn lin_leaks(&mut self, from: usize, line: usize, how: &str) {
        let from = from.max(self.lin_floor);
        let leaks: Vec<(String, usize)> =
            self.lin.iter().skip(from).filter(|x| x.live).map(|x| (x.name.clone(), x.line)).collect();
        for (name, decl) in leaks {
            let dl = crate::parser::unpack_pos(decl).0;
            let (root, note) = lin_label(&name);
            self.tdiag("E3003", format!("linear value `{root}` (declared at line {dl}) is not consumed {how}{note}"), line);
        }
    }

    /// 線形な要素を持つ配列リテラルの束縛 (4.4)。要素を整数リテラルの添字 (`ls[0]`) でしか
    /// 使わないとき、使われていない添字の要素を E3003 とする。配列全体の使用 (渡す・返す・
    /// `for` で回す・添字に変数を使う等) や、判断できない添字があれば黙る。
    /// この関数で扱ったとき true (呼び出し側は束縛を線形にしない)。
    fn note_linear_array(&mut self, name: &str, init: &V, ty: &V, line: usize) -> bool {
        if tag(ty) != "array" || nname(init) != "ArrayE" {
            return false;
        }
        let el = tel(ty, 1);
        if is_nil(el) || !self.is_linear(el) {
            return false;
        }
        let n = items(&nf(init)[0]).len();
        let body = self.def(self.d).body.as_ref().unwrap();
        let mut all = vec![];
        walk(body, &mut all);
        let total = all
            .iter()
            .filter(|x| nname(x) == "Path" && strs(&nf(x)[0]).first().map(|s| s.as_str()) == Some(name))
            .count();
        let mut used: HashSet<usize> = HashSet::new();
        let mut index_uses = 0;
        let mut unknown = false;
        for x in all.iter().filter(|x| nname(x) == "Index") {
            let b = &nf(x)[0];
            let base_is_name = nname(b) == "Path" && {
                let segs = strs(&nf(b)[0]);
                segs.len() == 1 && segs[0] == name
            };
            if !base_is_name {
                continue;
            }
            index_uses += 1;
            match lit_index(&nf(x)[1]) {
                Some(i) if i < n => {
                    used.insert(i);
                }
                _ => unknown = true,
            }
        }
        // 添字が判断できないときは黙る。配列全体の使用 (`index_uses != total`) があっても、
        // `.len()` / `.iter()` のような借用だけなら配列自体は消費されないので、義務を残す
        // (スコープ末で未消費を報告)。`for x in a` (所有) や丸ごと渡しは `use_place(consume)`
        // で義務を消費する。
        if unknown {
            return true;
        }
        if index_uses != total {
            return false;
        }
        if total == 0 {
            self.tdiag("E3003", format!("linear value `{}` is never consumed", name), line);
        } else if let Some(i) = (0..n).find(|i| !used.contains(i)) {
            self.tdiag("E3003", format!("linear value `{}[{}]` is never consumed", name, i), line);
        }
        true
    }

    /// スコープの終わり: `base` 以降の束縛を取り除き、未消費の線形値を報告する (宣言の位置で)。
    fn lin_scope_end(&mut self, base: usize, how: &str) {
        let base = base.min(self.lin.len());
        let gone = self.lin.split_off(base);
        for x in gone.into_iter().filter(|x| x.live) {
            let (root, note) = lin_label(&x.name);
            self.tdiag("E3003", format!("linear value `{root}` is never consumed {how}{note}"), x.line);
        }
    }

    /// 分岐の合流。外側の束縛 (先頭 `base` 件) が、ある経路では消費済みで別の経路では未消費なら、
    /// 未消費の経路ごとに E3003。報告した束縛は消費済みとして続ける。
    fn lin_merge(&mut self, inputs: Vec<(String, Vec<Lin>)>, base: usize, line: usize) {
        if inputs.is_empty() {
            return;
        }
        let mut res: Vec<Lin> = inputs[0].1[..base].to_vec();
        for i in 0..base {
            let lives: Vec<bool> = inputs.iter().map(|x| x.1[i].live).collect();
            let all = lives.iter().all(|&b| b);
            if !all && lives.iter().any(|&b| b) {
                let (name, decl) = (res[i].name.clone(), crate::parser::unpack_pos(res[i].line).0);
                let (root, note) = lin_label(&name);
                for (k, (desc, _)) in inputs.iter().enumerate() {
                    if lives[k] {
                        self.tdiag(
                            "E3003",
                            format!("linear value `{root}` (declared at line {decl}) is not consumed on the path where {desc}{note}"),
                            line,
                        );
                    }
                }
            }
            res[i].live = all;
        }
        self.lin = res;
    }

    /// 型パラメータが `linear` と宣言されていない関数に、線形な値を渡していないか (4.4・8.3)。
    /// `off` は、引数列に対する仮引数列のずれ (`v.push(x)` の `self` で 1)。仮引数の型と実引数の型を
    /// 構造で突き合わせ、線形でない型パラメータに線形な型が束縛されるとき E3006。
    fn check_linear_args(&mut self, c: &Def, off: usize, args: &[V], env: &Env, line: usize) {
        let nonlin: HashSet<String> = c
            .generics
            .iter()
            .filter(|g| g.0 == "type" && !items(&g.2).iter().any(|b| matches!(b, V::S(x) if x == "linear")))
            .map(|g| g.1.clone())
            .collect();
        if nonlin.is_empty() {
            return;
        }
        for (i, a) in args.iter().enumerate() {
            let Some((_, pt)) = c.params.get(i + off) else { break };
            let at = self.ty_of(a, env);
            if is_nil(&at) {
                // 型が付かない線形な値 (線形な値を `move` で取り込んだクロージャを束縛した名前) も、
                // 線形でない型パラメータには渡せない (`core.mem.drop(f)`)。
                if nname(a) == "Path" && tag(pt) == "named" && items(tel(pt, 2)).is_empty() && nonlin.contains(&st(tel(pt, 1))) {
                    let segs = strs(&nf(a)[0]);
                    if segs.len() == 1 && self.lin_find(&segs[0]).is_some_and(|k| self.lin[k].linear) {
                        self.tdiag(
                            "E3006",
                            format!(
                                "linear closure `{}` is passed for the type parameter `{}` of {}, which is not declared `linear` (4.4)",
                                segs[0],
                                st(tel(pt, 1)),
                                c.name
                            ),
                            line,
                        );
                    }
                }
                continue;
            }
            let mut hit = vec![];
            self.lin_unify(pt, &at, &nonlin, &mut hit);
            if let Some((p, t)) = hit.first() {
                self.tdiag(
                    "E3006",
                    format!(
                        "linear value of type {t} is passed for the type parameter `{p}` of {}, which is not declared `linear` (4.4)",
                        c.name
                    ),
                    line,
                );
            }
        }
    }

    /// 仮引数の型 `pt` と実引数の型 `at` を構造で突き合わせ、線形でない型パラメータ (`nonlin`) に
    /// 線形な型が当たった組 (パラメータ名, 型の表示) を `out` に集める。
    fn lin_unify(&self, pt: &V, at: &V, nonlin: &HashSet<String>, out: &mut Vec<(String, String)>) {
        match tag(pt).as_str() {
            "named" => {
                let n = st(tel(pt, 1));
                let pargs: Vec<&V> = items(tel(pt, 2)).iter().filter(|a| matches!(a, V::L(_))).collect();
                if pargs.is_empty() && nonlin.contains(&n) {
                    if self.is_linear(at) {
                        let shown = if tag(at) == "named" { st(tel(at, 1)) } else { "(tuple)".to_string() };
                        out.push((n, shown));
                    }
                    return;
                }
                if tag(at) == "named" && st(tel(at, 1)) == n {
                    let aargs: Vec<&V> = items(tel(at, 2)).iter().filter(|a| matches!(a, V::L(_))).collect();
                    if aargs.len() == pargs.len() {
                        for (p, a) in pargs.iter().zip(aargs) {
                            self.lin_unify(p, a, nonlin, out);
                        }
                    }
                }
            }
            "ref" => {
                let inner = if tag(at) == "ref" { tel(at, 2) } else { at };
                self.lin_unify(tel(pt, 2), inner, nonlin, out);
            }
            "tuple" if tag(at) == "tuple" && items(tel(pt, 1)).len() == items(tel(at, 1)).len() => {
                for (p, a) in items(tel(pt, 1)).iter().zip(items(tel(at, 1))) {
                    self.lin_unify(p, a, nonlin, out);
                }
            }
            "array" | "slice" if tag(at) == tag(pt) => self.lin_unify(tel(pt, 1), tel(at, 1), nonlin, out),
            _ => {}
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
        let cyclic = cyclic_nodes(&self.graph);
        for &i in &fns {
            let d = self.def(i);
            let n = d.name.clone();
            let rec = cyclic.contains(&n);
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

    /// 節のフレーム (バイト) の見積り: 戻りアドレスと保存したフレームポインタ (16)
    /// + 引数と局所変数の大きさ + 一時値の余裕 (16)。`anns` は引数の型注釈、`body` は本体。
    /// 型の注釈がない引数・局所変数は 1 語 (8) とみなす。本体の `let` は `walk` で拾うので、
    /// 入れ子のクロージャの中の `let` も数に入る (呼び出し元の関数のフレームとの二重計上は、
    /// 従来の保守的な近似の延長として許容する)。書かれた関数とクロージャ節で共通に使う。
    fn frame_of(&self, anns: &[&V], body: &V) -> u64 {
        let mut all = vec![];
        walk(body, &mut all);
        let mut bytes: u64 = 16 + 16;
        for a in anns {
            bytes += round8(self.ty_size(a, 0));
        }
        for l in all.iter().filter(|x| nname(x) == "Let") {
            let ann = &nf(l)[2];
            bytes += if is_nil(ann) { 8 } else { round8(self.ty_size(ann, 0)) };
        }
        bytes
    }

    fn go(&self, n: &str, path: &BTreeSet<String>, names: &HashMap<String, usize>, memo: &mut HashMap<String, u64>) -> u64 {
        if path.contains(n) {
            return 0;
        }
        if let Some(&v) = memo.get(n) {
            return v;
        }
        let frame = match names.get(n) {
            Some(&i) => {
                let d = self.def(i);
                let params: Vec<&V> = d.params.iter().map(|p| &p.1).collect();
                self.frame_of(&params, d.body.as_ref().unwrap())
            }
            // `names` にあるのは書かれた関数だけ。それ以外の節はクロージャ節 (5.3) かもしれないので、
            // 対応表にあれば関数と同じ式で見積る。対応が取れない節 (クロージャ以外や、AST を
            // 覚えていないもの) は従来どおり 32 にフォールバックする (近似・黙る)。
            None => match self.closure_asts.get(n) {
                Some(c) if nname(c) == "Closure" && nf(c).len() >= 4 => {
                    let f = nf(c);
                    let params: Vec<&V> = items(&f[0]).iter().filter_map(|p| items(p).get(1)).collect();
                    self.frame_of(&params, &f[3])
                }
                _ => 32,
            },
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

/// 呼び出しグラフの上で、自分自身へ戻ってこられる頂点 (自己ループか大きさ 2 以上の強連結成分) の集合。
/// 頂点ごとに到達集合を作る旧実装 (関数数の 2 乗) と同じ結果を、Tarjan 法 (反復版) で線形に求める。
fn cyclic_nodes(g: &BTreeMap<String, BTreeSet<String>>) -> HashSet<String> {
    let mut ids: HashMap<&str, usize> = HashMap::new();
    let mut names: Vec<&str> = vec![];
    for (k, vs) in g {
        for n in std::iter::once(k).chain(vs.iter()) {
            if !ids.contains_key(n.as_str()) {
                ids.insert(n.as_str(), names.len());
                names.push(n.as_str());
            }
        }
    }
    let n = names.len();
    let adj: Vec<Vec<usize>> = names
        .iter()
        .map(|nm| g.get(*nm).map(|vs| vs.iter().map(|v| ids[v.as_str()]).collect()).unwrap_or_default())
        .collect();
    const NONE: usize = usize::MAX;
    let mut index = vec![NONE; n];
    let mut low = vec![0usize; n];
    let mut on = vec![false; n];
    let mut stack: Vec<usize> = vec![];
    let mut out: HashSet<String> = HashSet::new();
    let mut next = 0usize;
    for root in 0..n {
        if index[root] != NONE {
            continue;
        }
        let mut call: Vec<(usize, usize)> = vec![(root, 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on[root] = true;
        while let Some(&(v, ei)) = call.last() {
            if ei < adj[v].len() {
                call.last_mut().unwrap().1 += 1;
                let w = adj[v][ei];
                if index[w] == NONE {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on[w] = true;
                    call.push((w, 0));
                } else if on[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                call.pop();
                if let Some(&(u, _)) = call.last() {
                    low[u] = low[u].min(low[v]);
                }
                if low[v] == index[v] {
                    let mut comp = vec![];
                    while let Some(w) = stack.pop() {
                        on[w] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    if comp.len() > 1 || adj[v].contains(&v) {
                        out.extend(comp.into_iter().map(|w| names[w].to_string()));
                    }
                }
            }
        }
    }
    out
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
pub(crate) fn impl_target(v: &V) -> String {
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

/// トレイトのメソッド `meth` の宣言された効果を、impl 行の実引数 `targs` (トレイトの型パラメータ列の順) で実体化する
/// (4.3 / 5.1)。戻り値は (uses の具体名、panics)。束縛できない効果変数が残る要素は `uses` 全体を `None` にする
/// (分からないので黙る)。宣言の `uses` / `panics` を書いていないメソッドは外側の `None`。
fn trait_effect_bounds(td: &Def, meth: &str, targs: &[V]) -> Option<(Option<Vec<String>>, Option<String>)> {
    let (_, us, pv) = td.trait_effs.iter().find(|e| e.0 == meth)?;
    let mut bind_u: HashMap<String, Vec<String>> = HashMap::new();
    let mut bind_p: HashMap<String, String> = HashMap::new();
    for (k, g) in td.generics.iter().enumerate() {
        let Some(a) = targs.get(k) else { continue };
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
    let mut unknown = false;
    for e in us {
        if concrete(e) {
            eu.push(e.clone());
        } else {
            match bind_u.get(e) {
                Some(set) if set.iter().all(concrete) => eu.extend(set.iter().cloned()),
                _ => unknown = true,
            }
        }
    }
    let ep = match pv.as_str() {
        "never" | "may" => Some(pv.clone()),
        v => bind_p.get(v).cloned(),
    };
    Some((if unknown { None } else { Some(eu) }, ep))
}

/// 8.2 の層ランク。依存方向は `std` → `alloc` → `core` なので、ランクの小さい層は
/// ランクの大きい層を参照できない。それ以外の最上位成分 (ユーザーパッケージ) は層を持たない。
fn layer_rank(root: &str) -> Option<u8> {
    match root {
        "core" => Some(0),
        "alloc" => Some(1),
        "std" => Some(2),
        _ => None,
    }
}

/// 型 V・trait_ref V・本体の AST に現れるドット付き FQN (named の型、`dyn` / 境界 /
/// `requires` のトレイト名) を集める。裸の名前 (基本型・型パラメータ) は集めない。
fn collect_ref_fqns(t: &V, out: &mut BTreeSet<String>) {
    match t {
        V::L(xs) => {
            if let Some(V::S(s)) = xs.first() {
                if s.contains('.') {
                    // trait_ref の先頭 (`requires` / 境界 / `dyn` のトレイト名)。
                    out.insert(s.clone());
                } else if s == "named" {
                    if let Some(n) = xs.get(1) {
                        let nm = st(n);
                        if nm.contains('.') {
                            out.insert(nm);
                        }
                    }
                }
            }
            for x in xs {
                if matches!(x, V::L(_) | V::Node(..)) {
                    collect_ref_fqns(x, out);
                }
            }
        }
        V::Node(_, fs, _, _) => {
            for f in fs {
                collect_ref_fqns(f, out);
            }
        }
        _ => {}
    }
}

/// 定義ヘッダの末尾が `pub` か。
fn header_is_pub(h: &str) -> bool {
    h.split_whitespace().last() == Some("pub")
}

/// 本体・初期化式の AST から、可視性の検査 (E1119) が見る名前参照 (パス・構造体リテラル・
/// バリアントパターン・型注釈の名前つき型) を (名前, 位置) で集める。
fn collect_vis_refs(t: &V, out: &mut Vec<(String, usize)>) {
    match t {
        V::Node(n, fs, pos, _) => {
            match *n {
                "Path" | "PVariant" => {
                    if let Some(V::L(segs)) = fs.first() {
                        let v: Vec<String> = segs.iter().map(st).collect();
                        if v.len() > 1 {
                            out.push((v.join("."), *pos));
                        }
                    }
                }
                "StructLit" => {
                    if let Some(V::S(nm)) = fs.first() {
                        if nm.contains('.') {
                            out.push((nm.clone(), *pos));
                        }
                    }
                }
                _ => {}
            }
            for f in fs {
                collect_vis_refs(f, out);
            }
        }
        V::L(xs) => {
            let mut hdr: BTreeSet<String> = BTreeSet::new();
            if matches!(xs.first(), Some(V::S(s)) if s == "named" || s.contains('.')) {
                collect_ref_fqns(t, &mut hdr);
                out.extend(hdr.into_iter().map(|r| (r, 0)));
                return;
            }
            for x in xs {
                if matches!(x, V::L(_) | V::Node(..)) {
                    collect_vis_refs(x, out);
                }
            }
        }
        _ => {}
    }
}

/// 本体の AST から構造体リテラル・構造体パターンの (型の名前の成分, フィールド名, 位置) を集める。
fn collect_struct_fields(t: &V, out: &mut Vec<(Vec<String>, String, usize)>) {
    match t {
        V::Node(n, fs, pos, _) => {
            match *n {
                "StructLit" => {
                    if let (Some(V::S(nm)), Some(V::L(fl))) = (fs.first(), fs.get(1)) {
                        for x in fl {
                            if let V::L(xs) | V::Node(_, xs, _, _) = x {
                                if let Some(V::S(f)) = xs.first() {
                                    out.push((nm.split('.').map(String::from).collect(), f.clone(), *pos));
                                }
                            }
                        }
                    }
                }
                "PStruct" => {
                    if let (Some(V::L(segs)), Some(V::L(fl))) = (fs.first(), fs.get(1)) {
                        let v: Vec<String> = segs.iter().map(st).collect();
                        for x in fl {
                            if let V::L(xs) | V::Node(_, xs, _, _) = x {
                                if let Some(V::S(f)) = xs.first() {
                                    out.push((v.clone(), f.clone(), *pos));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            for f in fs {
                collect_struct_fields(f, out);
            }
        }
        V::L(xs) => {
            for x in xs {
                collect_struct_fields(x, out);
            }
        }
        _ => {}
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
        V::Node(_, fs, _, _) => fs.iter().any(ty_has_self),
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

pub(crate) const INT_PRIMS: &[&str] = &["i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize"];

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
