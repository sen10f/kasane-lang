//! 効果の推論 (仕様 0.9): `uses` / `panics` を省略した非 `pub` 関数の効果を、本体から求める。
//!
//! 呼び出し先の効果の和を取り、添字は `panics may` とする。再帰や相互再帰は不動点まで繰り返す。
//! 局所変数のメソッド呼び出しは、名前と引数の個数が合う全候補の和で近似する (保守側)。

use std::collections::BTreeSet;

use crate::check::{covers, inst_def_effects, items, nf, nname, st, strs, walk};
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
    let mut effs: BTreeSet<String> = BTreeSet::new();
    let mut may = false;
    let Some(body) = &d.body else { return (vec![], false) };
    let locals = local_names(d, body);
    let mut nodes: Vec<&V> = vec![];
    walk(body, &mut nodes);
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
                    if segs.len() >= 2 {
                        method(p, &segs[segs.len() - 1], items(&f[1]).len(), targs, &mut effs, &mut may);
                    }
                    continue;
                }
                match p.resolve(&segs, &d.file) {
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
            "MCall" => method(p, &st(&f[1]), items(&f[2]).len(), items(&f[3]), &mut effs, &mut may),
            _ => {}
        }
    }
    // 階層で包含される要素 (io と io.fs) は、広い方だけを残す。
    let all: Vec<String> = effs.iter().cloned().collect();
    let uses: Vec<String> =
        all.iter().filter(|e| !all.iter().any(|o| o != *e && covers(&[o.clone()], e))).cloned().collect();
    (uses, may)
}

fn add(c: &Def, effs: &mut BTreeSet<String>, may: &mut bool) {
    add_effs(&c.uses, c.panics.as_deref(), effs, may);
}

/// 効果の集合と panic を推論結果に足す。効果変数 (E, P など) は具体的な効果ではないので無視する。
fn add_effs(uses: &[String], panics: Option<&str>, effs: &mut BTreeSet<String>, may: &mut bool) {
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

fn local_names(d: &Def, body: &V) -> BTreeSet<String> {
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
            "For" => {
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

/// 型表で整数型に決まった接尾辞なしの整数リテラルへ、その型を接尾辞として書き戻す。
/// 木走査のインタプリタは型表を見ないので、これがないと `let mut c = 0` のような式が型なしで計算され、
/// 溢れ検査や wrapping が KIR・機械語と食い違う。型が整数に決まらないものは触らない (黙る)。
/// 対象は `user` の添字の fn / test だけ (std と依存は触らない)。
pub fn annotate_int_lits(p: &mut Program, user: &[usize]) {
    use std::collections::HashMap;
    let mut tk = crate::types::TypeCk::new(p);
    for &i in user {
        let d = &p.defs[i];
        if matches!(d.kw.as_str(), "fn" | "test") && d.body.is_some() {
            tk.check_def(i);
        }
    }
    let mut want: HashMap<&str, HashMap<u32, String>> = HashMap::new();
    for ((fqn, id), e) in &tk.tytab {
        if e.kind == "Lit" {
            if let crate::types::Ty::Prim(s) = &e.ty {
                if matches!(s.as_str(), "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128" | "usize") {
                    want.entry(fqn.as_str()).or_default().insert(*id, s.clone());
                }
            }
        }
    }
    let todo: Vec<(usize, HashMap<u32, String>)> =
        user.iter().filter_map(|&i| want.get(p.defs[i].name.as_str()).map(|m| (i, m.clone()))).collect();
    drop(want);
    drop(tk);
    for (i, m) in todo {
        if let Some(b) = p.defs[i].body.as_mut() {
            fill(b, &m);
        }
    }
}

fn fill(v: &mut V, m: &std::collections::HashMap<u32, String>) {
    match v {
        V::Node(kind, fs, _, id) => {
            if *kind == "Lit" && fs.len() > 2 && matches!(&fs[0], V::S(k) if k == "int") && matches!(fs[2], V::Nil) {
                if let Some(t) = m.get(id) {
                    fs[2] = V::S(t.clone());
                }
            }
            for c in fs.iter_mut() {
                fill(c, m);
            }
        }
        V::L(xs) => xs.iter_mut().for_each(|c| fill(c, m)),
        _ => {}
    }
}
