//! 規則 X (効果の完全一致、設計_書き方は1通り.md 12 章の D5・D13、設計_ksnhとロック.md 3.4)。
//!
//! 関数の宣言の `uses` / `panics` は、本体から求めた効果と完全に一致していなければならない (広すぎても狭すぎても誤り)。
//! 省略は誤り (D13、E4020)。トレイトの実装のメソッドだけは、本体ではなく `impl` 行で約束した効果 (トレイトの宣言を
//! impl 行の実引数で実体化したもの) と一致させる (`impl_exact`)。違反は E4025。メッセージの末尾の
//! ``write `uses [..] panics ..` `` を `ksn fix` が読んで宣言を書き直す (fixes.rs)。
//!
//! 本体の効果は TAST の解決先 (`Call`/`MCall` の `res`) で求める: 呼び出し先の宣言の効果の和 + 整数の算術・添字の `panics may`。
//! 呼び出し先が決まらない (`Methods`・`Unknown`・型が決まらない算術・効果変数を関数の値で埋める呼び出し) ときは `uncertain` とし、
//! 完全一致の検査は黙る (狭すぎる側は E4001 / E4002 が見る)。クロージャの本体の効果は、定義した関数の効果に数える
//! (`Fn` 境界の効果変数の実体化の近似。呼ばれない閉包だけ過大になる)。

use std::collections::BTreeSet;

use crate::check::{covers, is_effect_var, is_panic_var, Diag};
use crate::program::{Def, Program};
use crate::tast::{Callee, TExpr, TKind, Tast};
use crate::types::Ty;

/// 階層で包含される要素 (io と io.fs) は広い方だけを残す。
fn reduce(v: &[String]) -> Vec<String> {
    let s: BTreeSet<String> = v.iter().filter(|e| !is_effect_var(e)).cloned().collect();
    let all: Vec<String> = s.iter().cloned().collect();
    all.iter().filter(|e| !all.iter().any(|o| o != *e && covers(&[o.clone()], e))).cloned().collect()
}

pub fn header(uses: &[String], may: bool) -> String {
    format!("uses [{}] panics {}", reduce(uses).join(", "), if may { "may" } else { "never" })
}

/// 本体の効果。`uncertain` のときは `uses` / `may` は確実な分だけ。
pub struct Actual {
    pub uses: BTreeSet<String>,
    pub may: bool,
    pub uncertain: bool,
    pub why: String,
}

fn is_int(t: &Ty) -> Option<bool> {
    match t.strip() {
        Ty::Prim(n) => Some(!matches!(n.as_str(), "f32" | "f64" | "bool" | "char" | "str")),
        _ => None,
    }
}

pub fn actual(p: &Program, d: &Def, body: &TExpr) -> Actual {
    let mut a = Actual { uses: BTreeSet::new(), may: false, uncertain: false, why: String::new() };
    super::impl_sig::walk(body, &mut |e| match &e.kind {
        TKind::Index { .. } => a.may = true,
        // `for x in it`: ユーザーの型の反復子は `next` の効果を持つ (暗黙の呼び出し)。型が決まらなければ決まらない。
        TKind::Other { name: "For", children } => match children.first().map(|c| c.ty.strip().clone()) {
            Some(Ty::Named(n, _)) if p.types.contains_key(&n) && matches!(p.defs[p.types[&n]].kw.as_str(), "type" | "enum") => {
                let nx = format!("{}.next", n);
                let mut found = false;
                for c in p.defs.iter().filter(|c| c.kw == "fn" && c.has_impls && c.name == nx) {
                    found = true;
                    if c.uses.iter().any(|u| is_effect_var(u)) || c.panics.as_deref().map_or(false, is_panic_var) {
                        a.uncertain = true;
                    }
                    a.uses.extend(c.uses.iter().filter(|u| !is_effect_var(u)).cloned());
                    if c.panics.as_deref() == Some("may") {
                        a.may = true;
                    }
                }
                if !found {
                    a.uncertain = true;
                }
            }
            Some(Ty::Named(..)) | Some(Ty::Array(..)) | Some(Ty::Slice(..)) | Some(Ty::Ref(..)) => {}
            _ => {
                a.uncertain = true;
                a.why = format!("for iterable type line {}", e.line);
            }
        },
        TKind::Binary { op, lhs, .. } if matches!(op.as_str(), "+" | "-" | "*" | "/" | "%") => match is_int(&lhs.ty) {
            Some(true) => a.may = true,
            Some(false) => {}
            None => { a.uncertain = true; a.why = format!("binop-type {} line {}", lhs.ty.show(), e.line); }
        },
        TKind::Unary { op, operand } if op == "-" => match is_int(&operand.ty) {
            Some(true) => a.may = true,
            Some(false) => {}
            None => a.uncertain = true,
        },
        TKind::Call { args, .. } | TKind::MCall { args, .. } => {
            let name = if let TKind::MCall { name, .. } = &e.kind { name.as_str() } else { "" };
            let closure_arg = |args: &[TExpr]| args.iter().any(|x| matches!(x.ty.strip(), Ty::Closure { .. }) && !matches!(x.kind, TKind::Closure { .. }));
            match &e.res {
                Some(Callee::Def(ci)) => {
                    let c = &p.defs[*ci];
                    // `extern` (C の関数) の効果は C 側にあり、本体からは分からない。それを呼ぶ関数 (FFI の包み) の宣言は表明として信じる。
                    if c.kw == "extern" {
                        a.uncertain = true;
                        a.why = format!("extern {} line {}", c.name, e.line);
                    }
                    let var = c.uses.iter().any(|u| is_effect_var(u)) || c.panics.as_deref().map_or(false, is_panic_var);
                    if var && closure_arg(args) {
                        a.uncertain = true;
                    }
                    // 効果変数を持つ呼び出し先で、閉包の本体 (数えている) 以外から実体が決まる形 (明示型引数・既定) は決まらない。
                    if var && e.tf.is_some() {
                        a.uncertain = true;
                        a.why = format!("explicit targs line {}", e.line);
                    }
                    if var && !args.iter().any(|x| matches!(x.kind, TKind::Closure { .. })) {
                        a.uncertain = true;
                        a.why = format!("effect var callee {} line {}", c.name, e.line);
                    }
                    for u in &c.uses {
                        if !is_effect_var(u) {
                            a.uses.insert(u.clone());
                        }
                    }
                    if c.panics.as_deref() == Some("may") {
                        a.may = true;
                    }
                    // 総称の効果変数の実体が閉包以外 (型パラメータの値) で決まるときは、本体の外の効果。
                    if var && args.iter().any(|x| matches!(x.ty.strip(), Ty::Param(_))) {
                        a.uncertain = true;
                    }
                }
                Some(Callee::Methods(v)) if !v.is_empty() => {
                    // 候補の効果がすべて同じなら (`len` など) 一意に決まったのと同じ。違えば決まらない。
                    let eff = |c: &Def| {
                        let u: Vec<String> = c.uses.iter().filter(|x| !is_effect_var(x)).cloned().collect();
                        (reduce(&u), c.panics.as_deref() == Some("may"), c.uses.iter().any(|x| is_effect_var(x)) || c.panics.as_deref().map_or(false, is_panic_var))
                    };
                    let first = eff(&p.defs[v[0]]);
                    if first.2 || v.iter().any(|i| eff(&p.defs[*i]) != first) {
                        a.uncertain = true;
                        a.why = format!("ambiguous {} line {}", v.iter().map(|i| p.defs[*i].name.clone()).collect::<Vec<_>>().join("|"), e.line);
                    } else {
                        a.uses.extend(first.0);
                        if first.1 {
                            a.may = true;
                        }
                    }
                }
                Some(Callee::Variant(..)) => {}
                Some(Callee::Local(n)) => {
                    if let Some((u, pn)) = crate::infer::fn_param_effects(d, n) {
                        for x in u {
                            if !is_effect_var(&x) {
                                a.uses.insert(x);
                            }
                        }
                        if pn == "may" {
                            a.may = true;
                        }
                    } else if let Some((u, pn)) = param_bound_effects(d, n) {
                        // `F: core.ops.Fn<.., E, P>` 境界の値の呼び出し: 境界の効果。変数のままの `E` / `P` は
                        // 宣言の側の変数 (宣言が変数を含むときは一致を検査しない)。
                        for x in u {
                            if !is_effect_var(&x) {
                                a.uses.insert(x);
                            }
                        }
                        if pn == Some(true) {
                            a.may = true;
                        }
                    } else if n.is_empty() {
                        a.uncertain = true;
                        a.why = format!("callee value line {}", e.line);
                    }
                }
                Some(Callee::Dyn(ti)) => {
                    let td = &p.defs[*ti];
                    match td.trait_effs.iter().find(|x| x.0 == name) {
                        Some((_, u, pn)) => {
                            for x in u {
                                if is_effect_var(x) {
                                    a.uncertain = true;
                                } else {
                                    a.uses.insert(x.clone());
                                }
                            }
                            if pn == "may" {
                                a.may = true;
                            } else if is_panic_var(pn) {
                                a.uncertain = true;
                            }
                        }
                        None => a.uncertain = true,
                    }
                }
                other => {
                    // `Call` の Path の経路や、解決先が表に無い呼び出し。
                    a.uncertain = true;
                    a.why = format!("callee {:?} line {}", other.as_ref().map(|c| match c { Callee::Methods(v) => format!("Methods{}", v.iter().map(|i| p.defs[*i].name.clone()).collect::<Vec<_>>().join("|")), Callee::Unknown => "Unknown".into(), _ => "other".into() }), e.line);
                }
            }
        }
        _ => {}
    });
    a
}

/// 宣言の `uses` の具体名 (効果変数を除く) を縮約したものと、実際の縮約を比べる。
fn same_uses(decl: &[String], act: &BTreeSet<String>) -> bool {
    let a: Vec<String> = act.iter().cloned().collect();
    reduce(decl) == reduce(&a)
}

pub fn check(p: &Program, user: &[usize], tast: &Tast) -> Vec<Diag> {
    let mut out = vec![];
    for &di in user {
        let d = &p.defs[di];
        if d.kw != "fn" || d.body.is_none() {
            continue;
        }
        // `derive [ToJson, FromJson]` が作る関数の見出しは派生の契約 (`derive_json.rs`) で、本体の効果の検査の対象外。
        if let Some((owner, meth)) = d.name.rsplit_once('.') {
            if p.derived_json.contains_key(owner) && (meth.starts_with("from_json") || meth.starts_with("to_json")) {
                continue;
            }
        }
        let Some(b) = tast.bodies.get(&di) else { continue };
        let a = actual(p, d, &b.body);
        let mk = |code: &str, msg: String| {
            let (ln, col) = crate::parser::unpack_pos(d.line);
            Diag { file: d.file.clone(), code: code.into(), fqn: d.name.clone(), line: ln, col, msg }
        };
        if d.inferred {
            // 省略は誤り (D13)。推論した値を示す (決まらないときは、従来の推論の値)。
            let (u, m) = if a.uncertain { (d.uses.clone(), d.panics.as_deref() == Some("may")) } else { (a.uses.iter().cloned().collect(), a.may) };
            out.push(mk("E4020", format!("{}: functions must declare `uses` and `panics`; write `{}`", d.name, header(&u, m))));
            continue;
        }
        if a.uncertain && std::env::var_os("KSN_EXP").is_some() {
            eprintln!("EXPU {} {} {}", d.file, d.name, a.why);
        }
        if d.has_impls || a.uncertain {
            continue;
        }
        let dvars = d.uses.iter().any(|u| is_effect_var(u)) || d.panics.as_deref().map_or(false, is_panic_var);
        if dvars {
            continue;
        }
        let dmay = d.panics.as_deref() == Some("may");
        if same_uses(&d.uses, &a.uses) && dmay == a.may {
            continue;
        }
        // 狭すぎる (本体の効果を宣言が覆わない・`never` なのに panic しうる) 側は、E4001 / E4002 / E4003 / E4004 が見る。
        // ここは、宣言が本体の効果をすべて覆ったうえで広すぎる場合 (他の診断が出ない形) だけを報告する。
        let narrow_uses = a.uses.iter().any(|e| !covers(&d.uses, e));
        let narrow = narrow_uses || (a.may && !dmay);
        let want = header(&a.uses.iter().cloned().collect::<Vec<_>>(), a.may);
        if narrow {
            if std::env::var_os("KSN_EXP").is_some() {
                eprintln!("EXPN {} {} decl={}", d.file, d.name, header(&d.uses, dmay));
            }
            continue;
        }
        let have = header(&d.uses, dmay);
        let why = if !same_uses(&d.uses, &a.uses) && dmay != a.may {
            "`uses` and `panics`"
        } else if dmay != a.may {
            "`panics`"
        } else {
            "`uses`"
        };
        out.push(mk("E4025", format!("{}: declared `{}` but the body has `{}` ({} must be exactly the body's effects, not wider); write `{}`", d.name, have, want, why, want)));
    }
    out
}

/// トレイトの実装のメソッド: `uses` / `panics` は impl 行で約束した効果 (`tu` / `tp`) と一致させる。
pub fn impl_exact(d: &Def, tu: &Option<Vec<String>>, tp: &Option<String>) -> Option<String> {
    let mut bad = false;
    if let Some(tu) = tu {
        let dec: Vec<String> = d.uses.iter().filter(|u| !is_effect_var(u)).cloned().collect();
        if reduce(&dec) != reduce(tu) {
            bad = true;
        }
    }
    if let Some(tp) = tp {
        if !is_panic_var(tp) && d.panics.as_deref().map_or(false, |x| !is_panic_var(x)) && d.panics.as_deref() != Some(tp.as_str()) {
            bad = true;
        }
    }
    if !bad {
        return None;
    }
    let u = tu.clone().unwrap_or_else(|| d.uses.clone());
    let may = match tp {
        Some(t) if !is_panic_var(t) => t == "may",
        _ => d.panics.as_deref() == Some("may"),
    };
    Some(format!("{}: an `impls` method must declare the effects promised by the `impl` line (`{}`); write `{}`", d.name, header(&u, may), header(&u, may)))
}

/// 引数 `name` の型が `F: core.ops.Fn<Args, R, E, P>` 境界を持つ型パラメータなら、境界の効果実引数 (`uses`、`panics` の `may` か `None` = 変数)。
fn param_bound_effects(d: &Def, name: &str) -> Option<(Vec<String>, Option<bool>)> {
    use crate::check::{items, st, tag, tel};
    let (_, t) = d.params.iter().find(|(n, _)| n == name)?;
    let mut t = t;
    while tag(t) == "ref" {
        t = tel(t, 2);
    }
    if tag(t) != "named" {
        return None;
    }
    let gname = st(tel(t, 1));
    for g in d.generics.iter().filter(|g| g.0 == "type" && g.1 == gname) {
        for b in items(&g.2) {
            let crate::parser::V::L(bx) = b else { continue };
            if !st(&bx[0]).starts_with("core.ops.Fn") {
                continue;
            }
            let targs = items(tel(b, 1));
            let uses: Vec<String> = match targs.get(2) {
                Some(v) if tag(v) == "effects" => items(tel(v, 1)).iter().map(|x| st(x)).collect(),
                Some(v) if tag(v) == "named" => vec![st(tel(v, 1))],
                _ => vec![],
            };
            let may = match targs.get(3) {
                Some(v) if tag(v) == "panic" => Some(st(tel(v, 1)) == "may"),
                _ => None,
            };
            return Some((uses, may));
        }
    }
    None
}
