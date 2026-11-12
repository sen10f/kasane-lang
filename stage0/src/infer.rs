//! 効果の推論 (仕様 0.9): `uses` / `panics` を省略した非 `pub` 関数の効果を、本体から求める。
//!
//! 呼び出し先の効果の和を取り、添字は `panics may` とする。再帰や相互再帰は不動点まで繰り返す。
//! 局所変数のメソッド呼び出しは、名前と引数の個数が合う全候補の和で近似する (保守側)。

use std::collections::BTreeSet;

use crate::check::{children, covers, inst_def_effects, items, nf, nname, st, strs, walk};
use crate::parser::V;
use crate::program::{Def, Program, Target};

/// 推論対象の関数の `uses` / `panics` を埋める。
pub fn run(p: &mut Program) {
    let targets: Vec<usize> = (0..p.defs.len()).filter(|&i| p.defs[i].kw == "fn" && p.defs[i].inferred).collect();
    if targets.is_empty() {
        return;
    }
    for &i in &targets {
        p.defs[i].panics = Some("never".into());
    }
    loop {
        let mut changed = false;
        for &i in &targets {
            let (uses, may) = analyze(p, i);
            let d = &mut p.defs[i];
            let new_panics = if may { "may" } else { "never" };
            if d.uses != uses || d.panics.as_deref() != Some(new_panics) {
                d.uses = uses;
                d.panics = Some(new_panics.into());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

fn analyze(p: &Program, di: usize) -> (Vec<String>, bool) {
    let d = &p.defs[di];
    let Some(body) = &d.body else { return (vec![], false) };
    let locals = local_names(d, body);
    let (uses, mut may) = body_effects(p, &d.file, &locals, body, false);
    // 関数ポインタ型の引数 (`f: fn(..) -> .. uses [..] panics ..`) の呼び出しは、その型の効果を持つ。
    let mut effs: BTreeSet<String> = uses.into_iter().collect();
    let mut nodes: Vec<&V> = vec![];
    collect_nodes(body, &mut nodes, false);
    for n in nodes {
        if nname(n) != "Call" || nname(&nf(n)[0]) != "Path" {
            continue;
        }
        let segs = strs(&nf(&nf(n)[0])[0]);
        if segs.len() == 1 {
            if let Some((u, pn)) = fn_param_effects(d, &segs[0]) {
                add_effs(&u, Some(pn.as_str()), &mut effs, &mut may);
            }
        }
    }
    (effs.into_iter().collect(), may)
}

/// 引数 `name` が関数ポインタ型なら、その型が宣言する (`uses`, `panics`) を返す。
/// `panics` が panic 変数 (`P`) のときは、実体が分からないので保守的に `may` とする。
pub(crate) fn fn_param_effects(d: &Def, name: &str) -> Option<(Vec<String>, String)> {
    let (_, t) = d.params.iter().find(|(n, _)| n == name)?;
    let mut t = t;
    while crate::check::tag(t) == "ref" {
        t = crate::check::tel(t, 2);
    }
    if crate::check::tag(t) != "fn" {
        return None;
    }
    let uses = strs(crate::check::tel(t, 3));
    let pn = crate::check::st(crate::check::tel(t, 4));
    Some((uses, if pn == "never" { "never".into() } else { "may".into() }))
}

/// 本体 AST と局所名の集合から、その本体の効果 (`uses` と `panics may`) を求める純粋関数。
/// 関数の効果推論 (`analyze`) と、`Fn` 境界に渡すクロージャ本体の推論 (check 側) が共用する。
/// `for_closure` のときは、型で解決できないメソッド呼び出しを効果に数えず (名前と引数個数の和で
/// 過大評価しない)、入れ子のクロージャ本体も数えない (どちらも呼び出し側の誤検出を避けるため)。
pub(crate) fn body_effects(
    p: &Program,
    file: &str,
    locals: &BTreeSet<String>,
    body: &V,
    for_closure: bool,
) -> (Vec<String>, bool) {
    let mut effs: BTreeSet<String> = BTreeSet::new();
    let mut may = false;
    let mut nodes: Vec<&V> = vec![];
    collect_nodes(body, &mut nodes, for_closure);
    for n in nodes {
        let f = nf(n);
        match nname(n) {
            "Index" => may = true,
            // 整数の算術はオーバーフローで panic しうる。型は分からないので、浮動小数リテラルが
            // 見えるとき以外は保守的に `may` とする。
            "Bin" if matches!(st(&f[0]).as_str(), "+" | "-" | "*" | "/" | "%") => {
                if !is_float_lit(&f[1]) && !is_float_lit(&f[2]) {
                    may = true;
                }
            }
            "Unary" if st(&f[0]) == "-" && !is_float_lit(&f[1]) => may = true,
            "Call" => {
                let fnn = &f[0];
                if nname(fnn) != "Path" {
                    continue;
                }
                let segs = strs(&nf(fnn)[0]);
                // 4.3: 明示型引数 (`f::<[io.fs], never>`) があれば効果変数を実体化した和を取る。
                let targs: &[V] = items(&f[2]);
                if locals.contains(&segs[0]) {
                    if !for_closure && segs.len() >= 2 {
                        method(p, &segs[segs.len() - 1], items(&f[1]).len(), targs, &mut effs, &mut may);
                    }
                    continue;
                }
                match p.resolve(&segs, file) {
                    Some(r) => match r.target {
                        Target::Fn(_) | Target::Sig(_) if r.rest.is_empty() => {
                            if let Some(c) = p.fn_def(&r) {
                                add_callee(c, targs, &mut effs, &mut may);
                            }
                        }
                        Target::Native(_) if r.rest.is_empty() => {
                            let fq = p.expand(&segs, "").join(".");
                            if let Some(&si) = p.sigs.get(&fq) {
                                add_callee(&p.defs[si], targs, &mut effs, &mut may);
                            }
                        }
                        _ => {}
                    },
                    None => {}
                }
            }
            "MCall" => {
                if !for_closure {
                    method(p, &st(&f[1]), items(&f[2]).len(), items(&f[3]), &mut effs, &mut may);
                }
            }
            _ => {}
        }
    }
    // 階層で包含される要素 (io と io.fs) は、広い方だけを残す。
    let all: Vec<String> = effs.iter().cloned().collect();
    let uses: Vec<String> =
        all.iter().filter(|e| !all.iter().any(|o| o != *e && covers(&[o.clone()], e))).cloned().collect();
    (uses, may)
}

/// `body` の節を集める。`skip_closures` のときは入れ子のクロージャ本体に降りない
/// (その中で起きる効果は、定義しただけでは呼び出し側の効果にならない)。
fn collect_nodes<'a>(v: &'a V, out: &mut Vec<&'a V>, skip_closures: bool) {
    out.push(v);
    if skip_closures && nname(v) == "Closure" {
        return;
    }
    for c in children(v) {
        collect_nodes(c, out, skip_closures);
    }
}

fn add(c: &Def, effs: &mut BTreeSet<String>, may: &mut bool) {
    add_effs(&c.uses, c.panics.as_deref(), effs, may);
}

/// 効果の集合と panic を推論結果に足す。効果変数 (E, P など) は具体的な効果ではないので無視する。
pub(crate) fn add_effs(uses: &[String], panics: Option<&str>, effs: &mut BTreeSet<String>, may: &mut bool) {
    for e in uses {
        // 効果変数 (E, P など) は具体的な効果ではない。
        if e.chars().next().map_or(false, |ch| ch.is_lowercase()) {
            effs.insert(e.clone());
        }
    }
    if panics == Some("may") {
        *may = true;
    }
}

/// 呼び出し先の効果を、明示型引数で効果変数を実体化してから足す (4.3 の単相化)。
/// 実体化できない (実引数がない・名前が並ばない) ときは宣言のまま足す。
fn add_callee(c: &Def, targs: &[V], effs: &mut BTreeSet<String>, may: &mut bool) {
    match inst_def_effects(c, targs) {
        Some((uses, panics)) => add_effs(&uses, panics.as_deref(), effs, may),
        None => add(c, effs, may),
    }
}

fn method(p: &Program, name: &str, nargs: usize, targs: &[V], effs: &mut BTreeSet<String>, may: &mut bool) {
    if let Some(v) = p.method_idx.get(name) {
        for &i in v {
            let c = &p.defs[i];
            if c.params.len() == nargs + 1 {
                add_callee(c, targs, effs, may);
            }
        }
    }
}

/// 本体の中で束縛される名前 (引数・`let`・`for`・`match`・クロージャの引数) の集合。
/// 呼び出しの解決 (FQN か局所変数レシーバか) を名前で近似するために使う (過大評価 = 取りこぼし側)。
pub(crate) fn local_names(d: &Def, body: &V) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = d.params.iter().map(|p| p.0.clone()).collect();
    let mut nodes: Vec<&V> = vec![];
    walk(body, &mut nodes);
    for n in nodes {
        let f = nf(n);
        match nname(n) {
            "Let" => {
                let mut b = vec![];
                crate::check::pat_binds(&f[0], &mut b);
                out.extend(b);
            }
            "For" | "With" => {
                let mut b = vec![];
                crate::check::pat_binds(&f[0], &mut b);
                out.extend(b);
            }
            "Match" => {
                for arm in items(&f[1]) {
                    let mut b = vec![];
                    crate::check::pat_binds(&items(arm)[0], &mut b);
                    out.extend(b);
                }
            }
            "Closure" => {
                for prm in items(&f[0]) {
                    out.insert(st(&items(prm)[0]));
                }
            }
            _ => {}
        }
    }
    out
}

fn is_float_lit(e: &V) -> bool {
    nname(e) == "Lit" && st(&nf(e)[0]) == "float"
}
