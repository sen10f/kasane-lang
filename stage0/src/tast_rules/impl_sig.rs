//! 規則 I (impl の署名・過不足・効果) と規則 D (`&T` → `&dyn Tr` の実装の有無) (設計_型付き検査.md 2.3、S2B)。
//!
//! 旧い検査 (`check.rs` の `check_impls` / `check_impl_method_sigs` / `check_impl_method_effects`、`types.rs` の
//! `check_dyn_impl`) は V の文字列 (`dump`) の一致で比べていた。ここでは宣言の型を `Ty` にして
//! `{Self := 実装の型, トレイトの型パラメータ := impl 行の実引数, メソッド自身の型パラメータ := impl 側の同じ位置の名前}` で置き換え、
//! impl の型と `Ty` どうしで比べる。メソッド自身の型パラメータの個数・種別・境界と、ライフタイム (宣言行の文面から組み立てた領域の
//! 分け方) も同じ規則で照らす。
//!
//! 出すコード: E1112 (過不足)、E1122 (署名)、E4021 (効果)、E2046 (`&T` → `&dyn Tr` で `T` に実装が無い)。

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::check::{covers, impl_target, items, resolve_trait_fqn, st, tag, tel, Diag};
use crate::parser::V;
use crate::program::{Def, Program};
use crate::tast::{Coercion, SideTabs, TExpr, TKind, TStmt, Tast};
use crate::types::{from_v, Ty};

struct Out<'a> {
    p: &'a Program,
    diags: Vec<Diag>,
}

impl<'a> Out<'a> {
    fn diag(&mut self, code: &str, msg: String, di: usize, line: usize) {
        let d = &self.p.defs[di];
        let (ln, col) = crate::parser::unpack_pos(if line != 0 { line } else { d.line });
        self.diags.push(Diag { file: d.file.clone(), code: code.into(), fqn: d.name.clone(), line: ln, col, msg });
    }
}

/// 基本型の所有者 FQN (impl のメソッドを置く場所。3.2)。
fn prim_owner(name: &str) -> String {
    match name {
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "f32" | "f64" => format!("core.num.{}", name),
        "bool" => "core.bool.bool".into(),
        "char" => "core.char.char".into(),
        "str" => "core.str".into(),
        other => other.to_string(),
    }
}

/// 孤児規則・置き場所の違反 (E2043 は旧い検査が出す。違反の impl は過不足を見ない)。規則は `check::orphan_violation`。
fn orphan(p: &Program, user: &[usize], d: &Def, tb: &str, tgt: &str) -> bool {
    crate::check::orphan_violation(p, user, d, tb, tgt).is_some()
}

pub fn check(p: &Program, user: &[usize], tast: &Tast, side: &SideTabs) -> Vec<Diag> {
    let mut o = Out { p, diags: vec![] };
    let impl_of = impl_lines(&mut o, user);
    for &i in user {
        let d = &p.defs[i];
        if d.kw != "fn" || !d.has_impls {
            continue;
        }
        let owner = d.name.rsplit_once('.').map(|(a, _)| a.to_string()).unwrap_or_default();
        let Some(&ii) = impl_of.get(&(d.impls_trait.clone(), owner)) else { continue };
        let Some(&ti) = p.types.get(&d.impls_trait) else { continue };
        if p.defs[ti].kw != "trait" {
            continue;
        }
        sig(&mut o, i, ii, ti);
        effects(&mut o, i, ii, ti);
    }
    unsize(&mut o, tast, side);
    o.diags
}

// ---------------------------------------------------------------- E1112: メソッドの過不足

/// 各 `impl` 行について、`impls` を宣言したメソッドの集合がトレイトの必須メソッドと一致するか (E1112)。
/// 戻り値は (トレイト, 所有者) → その `impl` 行の添字 (署名と効果の照合に使う)。
fn impl_lines(o: &mut Out, user: &[usize]) -> HashMap<(String, String), usize> {
    let p = o.p;
    let mut declared: HashMap<(String, String), BTreeSet<String>> = HashMap::new();
    for &i in user {
        let d = &p.defs[i];
        if d.kw == "fn" && d.has_impls {
            let owner = d.name.rsplit_once('.').map(|(a, _)| a.to_string()).unwrap_or_default();
            let meth = d.name.rsplit('.').next().unwrap_or("").to_string();
            declared.entry((d.impls_trait.clone(), owner)).or_default().insert(meth);
        }
    }
    let mut impl_of: HashMap<(String, String), usize> = HashMap::new();
    for &i in user {
        let d = &p.defs[i];
        if d.kw != "impl" {
            continue;
        }
        let tb = d.impl_trait.clone();
        if matches!(tb.as_str(), "core.marker.Send" | "core.marker.Sync" | "core.ops.Fn" | "core.ops.FnMut" | "core.ops.FnOnce") {
            continue;
        }
        let Some(td) = p.defs.iter().find(|x| x.kw == "trait" && x.name == tb) else { continue };
        let tgt = impl_target(&d.impl_for);
        if orphan(p, user, d, &tb, &tgt) {
            continue;
        }
        let owner = prim_owner(&tgt);
        let alt = format!("{}.{}", tb, tgt);
        let mut have = declared.get(&(tb.clone(), owner.clone())).cloned().unwrap_or_default();
        if let Some(m) = declared.get(&(tb.clone(), alt.clone())) {
            have.extend(m.iter().cloned());
        }
        impl_of.entry((tb.clone(), owner)).or_insert(i);
        impl_of.entry((tb.clone(), alt)).or_insert(i);
        let want: BTreeSet<String> = td.methods.iter().cloned().collect();
        if want != have {
            o.diag(
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
    impl_of
}

// ---------------------------------------------------------------- E1122: 署名

fn norm(t: Ty) -> Ty {
    match t {
        Ty::Named(n, a, _) => {
            let n = match n.as_str() {
                "Option" => "core.option.Option".to_string(),
                "Result" => "core.result.Result".to_string(),
                _ => n,
            };
            Ty::Named(n, a.into_iter().map(norm).collect(), crate::types::Rgs::default())
        }
        Ty::Ref(m, x, _) => Ty::Ref(m, Box::new(norm(*x)), crate::types::Region::Unk),
        Ty::Ptr(m, x) => Ty::Ptr(m, Box::new(norm(*x))),
        Ty::Tuple(a) => Ty::Tuple(a.into_iter().map(norm).collect()),
        Ty::Array(x, n) => Ty::Array(Box::new(norm(*x)), n),
        Ty::Slice(x) => Ty::Slice(Box::new(norm(*x))),
        Ty::Closure { args, ret, cap, id, eff } => Ty::Closure { args: args.into_iter().map(norm).collect(), ret: Box::new(norm(*ret)), cap, id, eff },
        o => o,
    }
}

/// 型パラメータ (`Ty::Param`) を `m` で置き換える。`m` に無いものはそのまま。
fn sub(t: &Ty, m: &HashMap<String, Ty>) -> Ty {
    match t {
        Ty::Param(p) => m.get(p).cloned().unwrap_or_else(|| t.clone()),
        Ty::Named(n, a, _) => Ty::Named(n.clone(), a.iter().map(|x| sub(x, m)).collect(), crate::types::Rgs::default()),
        Ty::Ref(k, x, _) => Ty::Ref(*k, Box::new(sub(x, m)), crate::types::Region::Unk),
        Ty::Ptr(k, x) => Ty::Ptr(*k, Box::new(sub(x, m))),
        Ty::Tuple(a) => Ty::Tuple(a.iter().map(|x| sub(x, m)).collect()),
        Ty::Array(x, n) => Ty::Array(Box::new(sub(x, m)), *n),
        Ty::Slice(x) => Ty::Slice(Box::new(sub(x, m))),
        Ty::Closure { args, ret, cap, id, eff } => Ty::Closure { args: args.iter().map(|x| sub(x, m)).collect(), ret: Box::new(sub(ret, m)), cap: cap.clone(), id: id.clone(), eff: eff.clone() },
        _ => t.clone(),
    }
}

fn is_eff_var(n: &str) -> bool {
    n.contains('$')
}

/// メソッド自身のジェネリクス (impl 行の宣言と重なる名前・ライフタイムを除く)。
fn own_generics<'a>(d: &'a Def, idef: &Def) -> Vec<&'a (String, String, V)> {
    let outer: HashSet<&str> = idef.generics.iter().map(|g| g.1.as_str()).collect();
    d.generics.iter().filter(|g| !outer.contains(g.1.as_str())).collect()
}

/// 境界 `[(トレイト名, 型引数)]` の、トレイト名 (FQN に解決) と先頭 2 つの型引数 (`Fn*` は効果引数を除く)。
fn bounds_of(p: &Program, b: &V) -> Vec<(String, Vec<V>)> {
    items(b)
        .iter()
        .filter_map(|x| match x {
            V::L(f) if !f.is_empty() => {
                let n = st(&f[0]);
                Some((resolve_trait_fqn(p, &n).unwrap_or(n), f.get(1).map(|a| items(a).to_vec()).unwrap_or_default()))
            }
            _ => None,
        })
        .collect()
}

/// `tr` が境界の集合 `have` のどれかのスーパートレイトか (推移的に `requires` を辿る)。
fn implied(p: &Program, have: &[(String, Vec<V>)], tr: &str) -> bool {
    let mut todo: Vec<String> = have.iter().map(|b| b.0.clone()).collect();
    let mut seen: HashSet<String> = HashSet::new();
    while let Some(b) = todo.pop() {
        if b == tr {
            return true;
        }
        if !seen.insert(b.clone()) {
            continue;
        }
        match p.types.get(&b) {
            Some(&i) if p.defs[i].kw == "trait" => {
                for (sup, _) in &p.defs[i].requires {
                    match resolve_trait_fqn(p, sup) {
                        Some(f) => todo.push(f),
                        None => return true, // 解決できない名前は黙る。
                    }
                }
            }
            _ => return true,
        }
    }
    false
}

fn sig(o: &mut Out, i: usize, ii: usize, ti: usize) {
    let p = o.p;
    let (d, idef, td) = (&p.defs[i], &p.defs[ii], &p.defs[ti]);
    let meth = d.name.rsplit('.').next().unwrap_or("");
    let Some((_, tps, tret)) = td.trait_sigs.iter().find(|s| s.0 == meth) else { return };
    let tname = format!("{}.{}", td.name, meth);
    let mut msg: Option<String> = None;

    // メソッド自身のジェネリクスの個数・種別・境界。
    let own = own_generics(d, idef);
    let tgen: Option<Vec<&(String, String, V)>> = td.trait_method_generics.iter().find(|g| g.0 == meth).map(|g| g.1.iter().collect());
    let tdecl: Vec<&(String, String, V)> = tgen.clone().unwrap_or_default();
    let is_tp = |g: &&(String, String, V)| g.0 == "type" || g.0 == "const";
    let (ti_tp, ii_tp): (Vec<_>, Vec<_>) = (tdecl.iter().copied().filter(is_tp).collect(), own.iter().copied().filter(is_tp).collect());
    let eff_n = |v: &[&(String, String, V)], k: &str| v.iter().filter(|g| g.0 == k && !is_eff_var(&g.1)).count();
    let mut rename: HashMap<String, Ty> = HashMap::new();
    if tgen.is_some() {
        if ti_tp.len() != ii_tp.len() {
            msg = Some(format!("{} declares {} type parameter(s) but trait method {} declares {}", d.name, ii_tp.len(), tname, ti_tp.len()));
        } else if let Some((a, b)) = ti_tp.iter().zip(ii_tp.iter()).find(|(a, b)| a.0 != b.0) {
            msg = Some(format!("{}: type parameter `{}` is a {} parameter but trait method {} declares `{}` as a {} parameter", d.name, b.1, b.0, tname, a.1, a.0));
        } else if eff_n(&tdecl, "uses") != eff_n(&own, "uses") || eff_n(&tdecl, "panics") != eff_n(&own, "panics") {
            msg = Some(format!("{} and trait method {} declare different effect/panic parameters", d.name, tname));
        }
        for (a, b) in ti_tp.iter().zip(ii_tp.iter()) {
            if a.0 == "type" {
                rename.insert(a.1.clone(), Ty::Param(b.1.clone()));
            }
        }
    }

    // 型の置き換え: `Self` → 実装の型、トレイトの型パラメータ → impl 行の実引数、メソッド自身の型パラメータ → impl 側の同位置の名前。
    let imp_names: HashSet<String> = d.gnames().into_iter().chain(idef.gnames()).collect();
    let tr_names: HashSet<String> = td.gnames().into_iter().chain(tdecl.iter().map(|g| g.1.clone())).collect();
    let mut m: HashMap<String, Ty> = rename.clone();
    m.insert("Self".into(), from_v(&idef.impl_for, &imp_names));
    let targs = items(&idef.impl_targs).to_vec();
    for (k, g) in td.generics.iter().enumerate() {
        if g.0 == "type" {
            if let Some(a) = targs.get(k) {
                m.insert(g.1.clone(), from_v(a, &imp_names));
            }
        }
    }
    let want = |v: &V| norm(sub(&from_v(v, &tr_names), &m));
    // impl の側にも `Self` と書ける (impl の対象型)。
    let self_only: HashMap<String, Ty> = [("Self".to_string(), m["Self"].clone())].into_iter().collect();
    let have = |v: &V| norm(sub(&from_v(v, &imp_names), &self_only));

    // 境界: impl の境界はトレイトの宣言の境界 (とそのスーパートレイト) に含まれる (impl が要求するものが多ければ呼べない)。
    if msg.is_none() && tgen.is_some() {
        for (a, b) in ti_tp.iter().zip(ii_tp.iter()) {
            if a.0 != "type" {
                continue;
            }
            let (tb, ib) = (bounds_of(p, &a.2), bounds_of(p, &b.2));
            for (name, iargs) in &ib {
                let n = if matches!(name.as_str(), "core.ops.Fn" | "core.ops.FnMut" | "core.ops.FnOnce") { 2 } else { usize::MAX };
                let hit = tb.iter().find(|(t, _)| t == name);
                match hit {
                    Some((_, targs2)) => {
                        let k = n.min(iargs.len()).min(targs2.len());
                        let ok = (0..k).all(|x| want(&targs2[x]) == have(&iargs[x]));
                        if !ok || (n == usize::MAX && iargs.len() != targs2.len()) {
                            msg = Some(format!("{}: bound `{}: {}` has different type arguments than trait method {}", d.name, b.1, name, tname));
                            break;
                        }
                    }
                    None => {
                        if !implied(p, &tb, name) {
                            msg = Some(format!("{}: bound `{}: {}` is not declared by trait method {} (the impl may not require more than the declaration)", d.name, b.1, name, tname));
                            break;
                        }
                    }
                }
            }
            if msg.is_some() {
                break;
            }
        }
    }

    if msg.is_none() {
        if tps.len() != d.params.len() {
            msg = Some(format!("{} takes {} parameter(s) but trait method {} declares {}", d.name, d.params.len(), tname, tps.len()));
        } else {
            let tr_recv = tps.first().map_or(false, |x| x.0 == "self");
            let im_recv = d.params.first().map_or(false, |x| x.0 == "self");
            if tr_recv != im_recv {
                msg = Some(format!("{} and trait method {} disagree on whether there is a `self` receiver", d.name, tname));
            }
            for (k, (tp, ip)) in tps.iter().zip(d.params.iter()).enumerate() {
                if msg.is_some() {
                    break;
                }
                if want(&tp.1) != have(&ip.1) {
                    msg = Some(if k == 0 && tr_recv {
                        format!("{}: receiver type does not match trait method {} (receiver form `self` differs from the declaration)", d.name, tname)
                    } else {
                        format!("{}: parameter `{}` type does not match trait method {}", d.name, ip.0, tname)
                    });
                }
            }
            if msg.is_none() && want(tret) != have(&d.ret) {
                msg = Some(format!("{}: return type does not match trait method {}", d.name, tname));
            }
        }
    }

    // ライフタイム (型の V に残らないので、宣言行の文面から領域の分け方を組む)。
    if msg.is_none() {
        let tline = td.trait_sig_text.iter().find(|s| s.0 == meth).map(|s| s.1.as_str());
        if let (Some(tl), Some(ts), Some(is)) = (tline, tline.and_then(shape), shape(&d.header)) {
            let _ = tl;
            let outer: HashSet<String> = idef.generics.iter().filter(|g| g.0 == "life").map(|g| g.1.trim_start_matches('\'').to_string()).chain(td.generics.iter().filter(|g| g.0 == "life").map(|g| g.1.trim_start_matches('\'').to_string())).collect();
            if !ts.uses_any(&outer) && !is.uses_any(&outer) {
                if let (Some(tc), Some(ic)) = (ts.classes(), is.classes()) {
                    if tc.0.len() == ic.0.len() && tc.1 == ic.1 && !life_ok(&ic.0, &tc.0, ic.1) {
                        msg = Some(format!("{}: lifetimes do not match trait method {} (the impl may not tie or lengthen lifetimes beyond the declaration)", d.name, tname));
                    }
                }
            }
        }
    }

    if let Some(m) = msg {
        o.diag("E1122", m, i, 0);
    }
}

// ---------------------------------------------------------------- ライフタイムの形

/// 宣言行の領域の並び: 入力側と出力側の参照・ライフタイム引数の位置ごとの名前 (`None` = 省略)。
struct Shape {
    inp: Vec<Option<String>>,
    out: Vec<Option<String>>,
    declared: HashSet<String>,
    recv_ref: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cls {
    Static,
    Id(usize),
}

fn shape(header: &str) -> Option<Shape> {
    let toks = crate::lexer::lex(&format!("{}\n", header)).ok()?;
    let open = toks.iter().position(|t| t.text == "(")?;
    let declared: HashSet<String> = toks[..open].iter().filter(|t| t.kind == "life").map(|t| t.text.trim_start_matches('\'').to_string()).collect();
    let mut depth = 0i32;
    let mut close = None;
    for (k, t) in toks.iter().enumerate().skip(open) {
        match t.text.as_str() {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    close = Some(k);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let recv_ref = toks.get(open + 1).map_or(false, |t| t.text == "self") && toks.get(open + 2).map_or(false, |t| t.text == ":") && toks.get(open + 3).map_or(false, |t| t.text.starts_with('&'));
    let scan = |ts: &[crate::lexer::Tok]| -> Vec<Option<String>> {
        let mut v = vec![];
        let mut k = 0;
        while k < ts.len() {
            let t = &ts[k];
            if t.kind != "life" && t.text.starts_with('&') && t.text.chars().all(|c| c == '&') {
                let n = t.text.len();
                if ts.get(k + 1).map_or(false, |x| x.kind == "life") {
                    for _ in 1..n {
                        v.push(None);
                    }
                    v.push(Some(ts[k + 1].text.trim_start_matches('\'').to_string()));
                    k += 1;
                } else {
                    for _ in 0..n {
                        v.push(None);
                    }
                }
            } else if t.kind == "life" {
                v.push(Some(t.text.trim_start_matches('\'').to_string()));
            }
            k += 1;
        }
        v
    };
    let inp = scan(&toks[open + 1..close]);
    let arrow = toks[close..].iter().position(|t| t.text == "->").map(|x| x + close);
    let out = match arrow {
        Some(a) => {
            let end = toks[a..].iter().position(|t| t.kind == "kw" && matches!(t.text.as_str(), "uses" | "panics" | "impls" | "stack")).map_or(toks.len(), |x| x + a);
            scan(&toks[a + 1..end])
        }
        None => vec![],
    };
    Some(Shape { inp, out, declared, recv_ref })
}

impl Shape {
    /// 宣言していない名前 (外側のライフタイム) を使うか。
    fn uses_any(&self, outer: &HashSet<String>) -> bool {
        self.inp.iter().chain(self.out.iter()).any(|n| match n {
            Some(n) => n != "static" && (outer.contains(n) || !self.declared.contains(n)),
            None => false,
        })
    }
    /// 省略を解いて、位置ごとの領域の別名 (`Static` か番号) にする。戻り値は (全位置, 入力の数)。
    fn classes(&self) -> Option<(Vec<Cls>, usize)> {
        let mut names: HashMap<String, usize> = HashMap::new();
        let mut next = 0usize;
        let mut cls = |n: &Option<String>, fresh: &mut usize| -> Cls {
            match n {
                Some(s) if s == "static" => Cls::Static,
                Some(s) => Cls::Id(*names.entry(s.clone()).or_insert_with(|| {
                    *fresh += 1;
                    *fresh
                })),
                None => {
                    *fresh += 1;
                    Cls::Id(*fresh)
                }
            }
        };
        let mut all: Vec<Cls> = self.inp.iter().map(|n| cls(n, &mut next)).collect();
        let src = if self.recv_ref && !all.is_empty() {
            Some(all[0])
        } else if all.len() == 1 {
            Some(all[0])
        } else {
            None
        };
        let n_in = all.len();
        for n in &self.out {
            let c = match (n, src) {
                (None, Some(s)) => s,
                _ => cls(n, &mut next),
            };
            all.push(c);
        }
        Some((all, n_in))
    }
}

/// impl の領域の分け方 `ic` が、トレイトの宣言 `tc` より厳しくない (呼び出し側の義務が増えず、戻り値の約束を弱めない) か。
fn life_ok(ic: &[Cls], tc: &[Cls], n_in: usize) -> bool {
    for (k, (a, b)) in ic.iter().zip(tc.iter()).enumerate() {
        match (a, b) {
            (Cls::Static, Cls::Static) => {}
            (Cls::Static, _) if k >= n_in => {}
            (Cls::Static, _) => return false,
            (_, Cls::Static) if k < n_in => {}
            (_, Cls::Static) => return false,
            _ => {}
        }
    }
    // impl の同じ領域に入る位置は、トレイトでも同じ領域に入る (トレイトが静的の位置は除く)。
    let mut seen: HashMap<usize, usize> = HashMap::new();
    for (a, b) in ic.iter().zip(tc.iter()) {
        if let (Cls::Id(x), Cls::Id(y)) = (a, b) {
            if *seen.entry(*x).or_insert(*y) != *y {
                return false;
            }
        }
    }
    true
}

// ---------------------------------------------------------------- E4021: 効果

/// トレイトのメソッドの宣言された効果を impl 行の実引数で実体化する (旧: `trait_effect_bounds`)。
pub(crate) fn trait_effect_bounds(td: &Def, meth: &str, targs: &[V]) -> Option<(Option<Vec<String>>, Option<String>)> {
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

fn effects(o: &mut Out, i: usize, ii: usize, ti: usize) {
    let p = o.p;
    let (d, td) = (&p.defs[i], &p.defs[ti]);
    if d.inferred {
        return;
    }
    let meth = d.name.rsplit('.').next().unwrap_or("");
    let targs = items(&p.defs[ii].impl_targs).to_vec();
    let Some((tu, tp)) = trait_effect_bounds(td, meth, &targs) else { return };
    let (tu_all, tp_all) = (tu.clone(), tp.clone());
    let tname = format!("{}.{}", td.name, meth);
    if let Some(tu) = tu {
        for e in &d.uses {
            if e.chars().next().map_or(false, |c| c.is_lowercase()) && !covers(&tu, e) {
                o.diag("E4021", format!("{} uses effect `{}` not declared by trait method {}", d.name, e, tname), i, 0);
                break;
            }
        }
    }
    if let Some(tp) = tp {
        if d.panics.as_deref() == Some("may") && tp == "never" {
            o.diag("E4021", format!("{} may panic but trait method {} declares `panics never`", d.name, tname), i, 0);
        }
    }
    if !o.diags.iter().any(|x| x.code == "E4021" && x.fqn == d.name) {
        if let Some(m) = super::effects_exact::impl_exact(d, &tu_all, &tp_all) {
            o.diag("E4025", m, i, 0);
        }
    }
}

// ---------------------------------------------------------------- E2046: `&T` → `&dyn Tr` の実装の有無

pub(crate) fn walk(e: &TExpr, f: &mut dyn FnMut(&TExpr)) {
    f(e);
    match &e.kind {
        TKind::Call { callee, args } => {
            walk(callee, f);
            args.iter().for_each(|a| walk(a, f));
        }
        TKind::MCall { recv, args, .. } => {
            walk(recv, f);
            args.iter().for_each(|a| walk(a, f));
        }
        TKind::Field { base, .. } => walk(base, f),
        TKind::Index { base, index } => {
            walk(base, f);
            walk(index, f);
        }
        TKind::Unary { operand, .. } => walk(operand, f),
        TKind::Binary { lhs, rhs, .. } => {
            walk(lhs, f);
            walk(rhs, f);
        }
        TKind::Block(ss) => {
            for s in ss {
                match s {
                    TStmt::Let { init, .. } => {
                        if let Some(x) = init {
                            walk(x, f)
                        }
                    }
                    TStmt::Expr(x) => walk(x, f),
                    TStmt::Assign { place, value } => {
                        walk(place, f);
                        walk(value, f);
                    }
                }
            }
        }
        TKind::If { cond, then, els } => {
            walk(cond, f);
            walk(then, f);
            if let Some(x) = els {
                walk(x, f);
            }
        }
        TKind::Match { scrut, arms } => {
            walk(scrut, f);
            for a in arms {
                if let Some(g) = &a.guard {
                    walk(g, f);
                }
                walk(&a.body, f);
            }
        }
        TKind::Closure { body, .. } => walk(body, f),
        TKind::StructLit { fields, .. } => fields.iter().for_each(|(_, x)| walk(x, f)),
        TKind::Other { children, .. } => children.iter().for_each(|c| walk(c, f)),
        TKind::Lit(_) | TKind::Path(_) => {}
    }
}

/// 規則 D: `Unsize(T → dyn Tr)` の `T` に `impl Tr for T` が無ければ E2046 (同名の固有メソッドでは代用しない)。
fn unsize(o: &mut Out, tast: &Tast, _side: &SideTabs) {
    let p = o.p;
    let mut hits: Vec<(usize, usize, String, String)> = vec![];
    for (&di, b) in &tast.bodies {
        walk(&b.body, &mut |e| {
            for c in &e.conv {
                let Coercion::Unsize { from, to } = c else { continue };
                let (Ty::Dyn(tr, _, _), Ty::Named(n, _, _)) = (to.strip(), from.strip()) else { continue };
                let Some(root) = resolve_trait_fqn(p, tr) else { continue };
                if !p.types.contains_key(n) {
                    continue;
                }
                let has = p.defs.iter().any(|x| {
                    x.kw == "impl" && x.impl_trait == root && matches!(&x.impl_for, V::L(xs) if matches!(xs.first(), Some(V::S(k)) if k == "named") && st(&xs[1]) == *n)
                });
                if !has {
                    hits.push((di, e.line, root, n.clone()));
                }
            }
        });
    }
    for (di, line, root, n) in hits {
        o.diag("E2046", format!("`{}` does not implement the trait `{}`", n, root), di, line);
    }
}
