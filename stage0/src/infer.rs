//! 効果の推論 (仕様 0.9): `uses` / `panics` を省略した非 `pub` 関数の効果を、本体から求める。
//!
//! 呼び出し先の効果の和を取り、添字は `panics may` とする。再帰や相互再帰は不動点まで繰り返す。
//! 局所変数のメソッド呼び出しは、名前と引数の個数が合う全候補の和で近似する (保守側)。

use std::collections::BTreeSet;

use crate::check::{covers, items, nf, nname, st, strs, walk};
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
                if locals.contains(&segs[0]) {
                    if segs.len() >= 2 {
                        method(p, &segs[segs.len() - 1], items(&f[1]).len(), &mut effs, &mut may);
                    }
                    continue;
                }
                match p.resolve(&segs, &d.file) {
                    Some(r) => match r.target {
                        Target::Fn(_) | Target::Sig(_) if r.rest.is_empty() => {
                            if let Some(c) = p.fn_def(&r) {
                                add(c, &mut effs, &mut may);
                            }
                        }
                        Target::Native(_) if r.rest.is_empty() => {
                            let fq = p.expand(&segs, "").join(".");
                            if let Some(&si) = p.sigs.get(&fq) {
                                add(&p.defs[si], &mut effs, &mut may);
                            }
                        }
                        _ => {}
                    },
                    None => {}
                }
            }
            "MCall" => method(p, &st(&f[1]), items(&f[2]).len(), &mut effs, &mut may),
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
    for e in &c.uses {
        // 効果変数 (E, P など) は具体的な効果ではない。
        if e.chars().next().map_or(false, |ch| ch.is_lowercase()) {
            effs.insert(e.clone());
        }
    }
    if c.panics.as_deref() == Some("may") {
        *may = true;
    }
}

fn method(p: &Program, name: &str, nargs: usize, effs: &mut BTreeSet<String>, may: &mut bool) {
    if let Some(v) = p.method_idx.get(name) {
        for &i in v {
            let c = &p.defs[i];
            if c.params.len() == nargs + 1 {
                add(c, effs, may);
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
