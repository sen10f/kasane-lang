//! 規則 O: 書き方は 1 通り (設計_書き方は1通り.md 12 章、2026-11-21 の決定)。TAST の型と呼び出し先だけで決める。
//! - E2055: `Option` / `Result` の判定メソッド (`is_some` `is_none` `is_ok` `is_err`) は `式 is Some(_)` 等と書く。
//! - E2055: `Option` と `None` の比較 `x == None` / `x != None` は `x is Some(_)` と書く。
//! - E2057: `core.panic.todo()` は廃止。`core.panic.panic("..")` と書く。
//! - E2058: `sort_unstable` / `sort_unstable_by` は廃止。`sort` / `sort_by` と書く。
//! - E2059: `core.check.is_true(a == b)` は `core.check.eq(a, b)` と書く。

use crate::check::Diag;
use crate::program::Program;
use crate::tast::*;
use crate::types::Ty;

fn diag(p: &Program, di: usize, line: usize, code: &str, msg: String) -> Diag {
    Diag { file: p.defs[di].file.clone(), code: code.into(), fqn: p.defs[di].name.clone(), line, col: 0, msg }
}

fn is_named(t: &Ty, names: &[&str]) -> bool {
    matches!(t.strip(), Ty::Named(n, _, _) if names.contains(&n.as_str()))
}

/// 値の型が `Copy` に近い基本型 (整数・浮動小数・真偽・文字)。`is` や `core.check.eq` は値を取るので、移動しない型だけを直す。
fn scalar(t: &Ty) -> bool {
    matches!(t.strip(), Ty::Prim(_))
}

fn path_is(e: &TExpr, want: &[&str]) -> bool {
    matches!(&e.kind, TKind::Path(p) if p.iter().map(String::as_str).eq(want.iter().copied()))
}

fn is_none_path(e: &TExpr) -> bool {
    path_is(e, &["None"]) || path_is(e, &["core", "option", "None"])
}

/// 整数リテラル `<値>` の値 (接尾辞付きでも可)。`Lit` の文字列は `int:<値>:<接尾辞>` 形。
fn int_lit(e: &TExpr) -> Option<u128> {
    let TKind::Lit(s) = &e.kind else { return None };
    let mut it = s.split(':');
    if it.next()? != "int" {
        return None;
    }
    it.next()?.replace('_', "").parse().ok()
}

fn is_int(t: &Ty) -> bool {
    matches!(t.strip(), Ty::Prim(n) if matches!(n.as_str(), "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize"))
}

/// 型引数を単語として含むか (`V` の `dump` の文字列で見る)。
fn mentions(dump: &str, name: &str) -> bool {
    let b = dump.as_bytes();
    let n = name.len();
    let mut i = 0;
    while let Some(k) = dump[i..].find(name) {
        let at = i + k;
        let before = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_');
        let after = at + n >= b.len() || !(b[at + n].is_ascii_alphanumeric() || b[at + n] == b'_');
        if before && after {
            return true;
        }
        i = at + n;
    }
    false
}

/// 引数の式の型が、その場の注釈や他の式の推論に頼らず決まっている (整数・小数・文字列のリテラル、単純な名前、それらの `&` `&mut`)。
fn closed_arg(e: &TExpr) -> bool {
    match &e.kind {
        TKind::Lit(_) => true,
        TKind::Path(p) => p.len() == 1 && p[0] != "None",
        TKind::Unary { op, operand } if op == "&" || op == "&mut" => closed_arg(operand),
        _ => false,
    }
}

fn is_int128(t: &Ty) -> bool {
    is_int(t) || matches!(t.strip(), Ty::Prim(n) if n == "u128" || n == "i128")
}

fn flip(op: &str) -> &'static str {
    match op {
        "<" => ">=",
        "<=" => ">",
        ">" => "<=",
        _ => "<",
    }
}


/// メソッド形の呼び出しの分解: 受け手 (名前の経路 `x.f` か、式)・メソッド名・引数。`x.m(..)` は経路 `x.m` を呼ぶ `Call`、
/// 式の受け手 (`a.m().n()`) は `MCall` で表される。
enum Recv<'a> {
    Key(String),
    Inner(&'a TExpr),
}

fn parts<'a>(e: &'a TExpr) -> Option<(Recv<'a>, &'a str, &'a [TExpr])> {
    match &e.kind {
        TKind::MCall { recv, name, args } => Some((Recv::Inner(recv), name.as_str(), args.as_slice())),
        TKind::Call { callee, args } => {
            let TKind::Path(p) = &callee.kind else { return None };
            if p.len() < 2 || matches!(p[0].as_str(), "core" | "std" | "alloc") || is_int128(&Ty::Prim(p[0].clone())) {
                return None;
            }
            if !matches!(callee.ty.strip(), Ty::Prim(_)) {
                return None;
            }
            Some((Recv::Key(p[..p.len() - 1].join(".")), p[p.len() - 1].as_str(), args.as_slice()))
        }
        _ => None,
    }
}

fn recv_key(r: &Recv) -> Option<String> {
    match r {
        Recv::Key(k) => Some(k.clone()),
        Recv::Inner(x) => path_key(x),
    }
}

fn path_key(e: &TExpr) -> Option<String> {
    match &e.kind {
        TKind::Path(p) if !p.is_empty() && p[0] != "None" => Some(p.join(".")),
        _ => None,
    }
}

fn lit_text(e: &TExpr) -> Option<&str> {
    match &e.kind {
        TKind::Lit(s) if s.starts_with("int:") => Some(s.as_str()),
        _ => None,
    }
}

fn is_max_path(e: &TExpr, t: &Ty) -> bool {
    match (&e.kind, t.strip()) {
        (TKind::Path(p), Ty::Prim(n)) => p.len() == 4 && p[0] == "core" && p[1] == "num" && &p[2] == n && p[3] == "MAX" && matches!(n.as_str(), "u8" | "u16" | "u32" | "u64" | "u128"),
        _ => false,
    }
}

fn all_ones(t: &Ty, v: u128) -> bool {
    match t.strip() {
        Ty::Prim(n) => match n.as_str() {
            "u8" => v == u8::MAX as u128,
            "u16" => v == u16::MAX as u128,
            "u32" => v == u32::MAX as u128,
            "u64" => v == u64::MAX as u128,
            "u128" => v == u128::MAX,
            _ => false,
        },
        _ => false,
    }
}

/// 何も変えない・定数になる整数の式 (E2062)。メッセージの先頭の `[タグ]` が `ksn fix` の書き換えの種類。
fn identity(e: &TExpr) -> Option<String> {
    if !is_int128(&e.ty) && !matches!(&e.kind, TKind::Binary { .. }) {
        return None;
    }
    if let TKind::Binary { op, lhs, rhs } = &e.kind {
        let int = is_int128(&lhs.ty) && is_int128(&rhs.ty);
        if !int {
            return None;
        }
        let zero = |x: &TExpr| int_lit(x) == Some(0);
        match op.as_str() {
            "|" | "^" if zero(rhs) || zero(lhs) => return Some(format!("[bit0] `x {op} 0` changes nothing: write `x`")),
            "&" if int_lit(rhs).map_or(false, |v| all_ones(&lhs.ty, v)) || int_lit(lhs).map_or(false, |v| all_ones(&rhs.ty, v)) || is_max_path(rhs, &lhs.ty) || is_max_path(lhs, &rhs.ty) => {
                return Some("[ones] `x & <all ones>` changes nothing: write `x`".into())
            }
            "==" | "!=" => {
                if let (Some(a), Some(b)) = (path_key(lhs), path_key(rhs)) {
                    if a == b {
                        return Some(format!("[same{op}] `x {op} x` on an integer is a constant: write `{}`", if op == "==" { "true" } else { "false" }));
                    }
                }
            }
            _ => {}
        }
        return None;
    }
    let (recv, name, args) = parts(e)?;
    match (name, args.len()) {
        ("wrapping_shl" | "wrapping_shr" | "shl" | "shr", 1) if int_lit(&args[0]) == Some(0) => Some(format!("[call0] `.{name}(0)` changes nothing: remove the call")),
        ("wrapping_neg", 0) => {
            if let Recv::Inner(i) = &recv {
                if let Some((_, "wrapping_neg", a)) = parts(i) {
                    if a.is_empty() {
                        return Some("[neg2] `.wrapping_neg().wrapping_neg()` changes nothing: remove both calls".into());
                    }
                }
            }
            None
        }
        ("wrapping_sub" | "wrapping_add", 1) => {
            if let (Recv::Inner(i), Some(c)) = (&recv, lit_text(&args[0])) {
                if let Some((_, other, a)) = parts(i) {
                    if a.len() == 1 && lit_text(&a[0]) == Some(c) && ((name == "wrapping_sub" && other == "wrapping_add") || (name == "wrapping_add" && other == "wrapping_sub")) {
                        return Some("[addsub] `.wrapping_add(c).wrapping_sub(c)` changes nothing: remove both calls".into());
                    }
                }
            }
            if name == "wrapping_sub" {
                if let (Some(k), Some(a)) = (recv_key(&recv), path_key(&args[0])) {
                    if k == a {
                        return Some(format!("[subself:{}] `x.wrapping_sub(x)` is `0`: write the literal", e.ty.show()));
                    }
                }
            }
            None
        }
        ("max" | "min", 1) => {
            let (k, a) = (recv_key(&recv)?, path_key(&args[0])?);
            (k == a).then(|| format!("[maxmin] `x.{name}(x)` is `x`"))
        }
        _ => None,
    }
}

fn is_option(t: &Ty) -> bool {
    matches!(t.strip(), Ty::Named(n, _, _) if n == "Option" || n == "core.option.Option")
}

/// `i < b` の `i` の名前 (`while` の条件)。
fn cond_names(c: &TExpr) -> Option<String> {
    match &c.kind {
        TKind::Binary { op, lhs, .. } if op == "<" => match &lhs.kind {
            TKind::Path(p) if p.len() == 1 => Some(p[0].clone()),
            _ => None,
        },
        _ => None,
    }
}

fn root_name(e: &TExpr) -> Option<&str> {
    match &e.kind {
        TKind::Path(p) => p.first().map(String::as_str),
        TKind::Field { base, .. } | TKind::Index { base, .. } => root_name(base),
        TKind::Unary { operand, .. } => root_name(operand),
        _ => None,
    }
}

/// `e` の中に、`name` を書き換える (代入・`&mut`) 所か `continue` があるか。
fn writes_or_continues(e: &TExpr, name: &str, continues: bool) -> bool {
    let mut hit = false;
    super::impl_sig::walk(e, &mut |x| match &x.kind {
        TKind::Block(ss) => {
            for st in ss {
                if let TStmt::Assign { place, .. } = st {
                    if root_name(place) == Some(name) {
                        hit = true;
                    }
                }
            }
        }
        TKind::Unary { op, operand } if op == "&mut" && root_name(operand) == Some(name) => hit = true,
        TKind::Other { name: "Continue", .. } if continues => hit = true,
        _ => {}
    });
    hit
}

fn mentions_var(e: &TExpr, name: &str) -> bool {
    let mut hit = false;
    super::impl_sig::walk(e, &mut |x| {
        if let TKind::Path(p) = &x.kind {
            if p.first().map(String::as_str) == Some(name) {
                hit = true;
            }
        }
    });
    hit
}

fn stmt_mentions(s: &TStmt, name: &str) -> bool {
    match s {
        TStmt::Let { init, .. } => init.as_ref().map_or(false, |x| mentions_var(x, name)),
        TStmt::Expr(x) => mentions_var(x, name),
        TStmt::Assign { place, value } => mentions_var(place, name) || mentions_var(value, name),
    }
}

/// `name` を書き換える (または書き換えうる) 使い方があるか。保守的: 代入・`&mut`・メソッドの受け手 (`x.m()`)・関数値の呼び出し・
/// 閉包の中の言及は、どれも「書き換えうる」。
fn may_mutate(p: &Program, e: &TExpr, name: &str) -> bool {
    let mut hit = false;
    super::impl_sig::walk(e, &mut |x| match &x.kind {
        TKind::Block(ss) => {
            for st in ss {
                if let TStmt::Assign { place, .. } = st {
                    if root_name(place) == Some(name) {
                        hit = true;
                    }
                }
            }
        }
        TKind::Unary { op, operand } if op == "&mut" && root_name(operand) == Some(name) => hit = true,
        TKind::MCall { recv, .. } if root_name(recv) == Some(name) && takes_mut_self(p, x) => hit = true,
        TKind::Call { callee, .. } => {
            if let TKind::Path(pth) = &callee.kind {
                if pth.first().map(String::as_str) == Some(name) && (pth.len() != 2 || takes_mut_self(p, x)) {
                    hit = true;
                }
            }
        }
        TKind::Closure { .. } if mentions_var(x, name) => hit = true,
        TKind::Other { name: "With", .. } | TKind::Other { name: "For", .. } if false => {}
        _ => {}
    });
    hit
}

/// メソッド呼び出し `e` の解決先が `self` を `&mut` で受けるか (解決できなければ受けるとみなす)。
fn takes_mut_self(p: &Program, e: &TExpr) -> bool {
    let Some(Callee::Def(d)) = &e.res else { return true };
    match p.defs[*d].params.first() {
        Some((n, t)) if n == "self" => t.dump().starts_with("[ref true"),
        _ => true,
    }
}

fn stmt_may_mutate(p: &Program, s: &TStmt, name: &str) -> bool {
    match s {
        TStmt::Let { init, .. } => init.as_ref().map_or(false, |x| may_mutate(p, x, name)),
        TStmt::Expr(x) => may_mutate(p, x, name),
        TStmt::Assign { place, value } => root_name(place) == Some(name) || may_mutate(p, place, name) || may_mutate(p, value, name),
    }
}

/// 複数行の書き換えが要る形 (E2068 bool の `match`・E2069 1 腕の `match`・E2070 数え上げの `while`・E2071 `== Some(リテラル)`)。
fn shapes(p: &Program, e: &TExpr, found: &mut Vec<(usize, &'static str, String)>) {
    match &e.kind {
        TKind::Match { scrut, arms } if arms.len() == 2 => {
            if matches!(scrut.ty.strip(), Ty::Prim(n) if n == "bool") && arms.iter().all(|a| a.pat.kind == "PLit" && a.guard.is_none()) {
                found.push((e.line, "E2068", "`match` on a `bool` is `if` / `else`".into()));
            }
            if is_option(&scrut.ty) {
                let some = arms.iter().find(|a| a.pat.kind == "PVariant" && a.pat.path.last().map(String::as_str) == Some("Some"));
                let none = arms.iter().find(|a| a.pat.kind == "PPath" && a.pat.path.last().map(String::as_str) == Some("None"));
                if let (Some(sm), Some(nn)) = (some, none) {
                    let unit_none = matches!(&nn.body.kind, TKind::Lit(l) if l == "unit") || matches!(&nn.body.kind, TKind::Block(b) if b.is_empty());
                    if unit_none && nn.guard.is_none() && sm.guard.is_none() && matches!(sm.body.ty.strip(), Ty::Unit) {
                        found.push((e.line, "E2069", "a `match` on an `Option` whose `None` arm is `()` is `if let Some(..) = x`".into()));
                    }
                }
            }
        }
        TKind::Binary { op, lhs, rhs } if op == "==" || op == "!=" => {
            if let TKind::Call { callee, args } = &rhs.kind {
                if path_is(callee, &["Some"]) && args.len() == 1 && matches!(&args[0].kind, TKind::Lit(l) if l.starts_with("int:") || l.starts_with("bool")) && is_option(&lhs.ty) {
                    found.push((e.line, "E2071", format!("`x {op} Some(lit)` is `x is Some(lit)`")));
                }
            }
        }
        TKind::Closure { params, captures, body, written } if captures.is_empty() && written.is_empty() && !params.is_empty() => {
            let call = match &body.kind {
                TKind::Block(ss) if ss.len() == 1 => match &ss[0] {
                    TStmt::Expr(x) => Some(x),
                    _ => None,
                },
                TKind::Call { .. } => Some(body.as_ref()),
                _ => None,
            };
            if let Some(c) = call {
                if let (TKind::Call { callee, args }, Some(Callee::Def(d))) = (&c.kind, &c.res) {
                    let def = &p.defs[*d];
                    let TKind::Path(cp) = &callee.kind else { return };
                    let in_order = args.len() == params.len() && args.iter().zip(params.iter()).all(|(a, q)| path_is(a, &[q.name.as_str()]));
                    let same_sig = def.params.len() == params.len() && def.params.iter().zip(params.iter()).all(|((_, pv), q)| crate::types::from_v(pv, &Default::default()).show() == q.ty.show());
                    if def.kw == "fn" && def.generics.is_empty() && !def.is_method() && in_order && same_sig && !params.iter().any(|q| cp.first() == Some(&q.name)) && cp.join(".") == def.name {
                        found.push((e.line, "E2073", format!("this closure only calls `{}` with its own parameters: write the function `{}` itself", def.name, def.name)));
                    }
                }
            }
        }
        TKind::Block(ss) => {
            // E2072: 書き換えない `let mut x` は `let x`。
            for (j, st) in ss.iter().enumerate() {
                if let TStmt::Let { pat, init: Some(_) } = st {
                    if pat.kind == "PBind" && pat.lit.iter().any(|l| l == "mut") {
                        if let Some(n) = pat.name.as_deref() {
                            if !n.starts_with('_') && ss[j + 1..].iter().any(|s| stmt_mentions(s, n)) && !ss[j + 1..].iter().any(|s| stmt_may_mutate(p, s, n)) {
                                found.push((pat.line, "E2072", format!("`let mut {n}` is never mutated: write `let {n}`")));
                            }
                        }
                    }
                }
            }
            for k in 1..ss.len() {
                let TStmt::Expr(w) = &ss[k] else { continue };
                let TKind::Other { name: "While", children } = &w.kind else { continue };
                let (Some(cond), Some(body)) = (children.first(), children.get(1)) else { continue };
                let Some(j) = (0..k).rev().find(|&j| matches!(&ss[j], TStmt::Let { pat, .. } if pat.name.is_some() && pat.kind == "PBind" && matches!(&ss[k - 1], _) && cond_names(cond).as_deref() == pat.name.as_deref())) else { continue };
                let TStmt::Let { pat, init: Some(init) } = &ss[j] else { continue };
                // 間の文が `i` に触れない。`let` を `for` の所へ動かすので、離れているときは初期値がリテラルのときだけ。
                if ss[j + 1..k].iter().any(|s| stmt_mentions(s, pat.name.as_deref().unwrap_or(""))) || (j + 1 < k && !matches!(&init.kind, TKind::Lit(_))) {
                    continue;
                }
                if pat.kind != "PBind" || pat.lit.iter().any(|l| l == "ann") {
                    continue;
                }
                let Some(i) = pat.name.as_deref() else { continue };
                let TKind::Binary { op, lhs, rhs } = &cond.kind else { continue };
                if op != "<" || !path_is(lhs, &[i]) || !is_int128(&lhs.ty) || !is_int128(&rhs.ty) || init.ty.show() != lhs.ty.show() {
                    continue;
                }
                let bname = match &rhs.kind {
                    TKind::Lit(l) if l.starts_with("int:") => None,
                    TKind::Path(pth) if pth.len() == 1 => Some(pth[0].as_str()),
                    _ => continue,
                };
                let TKind::Block(bs) = &body.kind else { continue };
                if bs.len() < 2 {
                    continue;
                }
                let Some(TStmt::Assign { place, value }) = bs.last() else { continue };
                let step = matches!(&value.kind, TKind::Call { callee, args } if matches!(&callee.kind, TKind::Path(q) if q.len() == 2 && q[0] == i && q[1] == "wrapping_add") && args.len() == 1 && int_lit(&args[0]) == Some(1));
                if !path_is(place, &[i]) || !step {
                    continue;
                }
                let inner = TExpr { kind: TKind::Block(bs[..bs.len() - 1].to_vec()), ..body.clone() };
                if writes_or_continues(&inner, i, true) || bname.map_or(false, |b| writes_or_continues(&inner, b, false)) {
                    continue;
                }
                if ss[k + 1..].iter().any(|s| stmt_mentions(s, i)) {
                    continue;
                }
                found.push((w.line, "E2070", format!("a counting `while` over `{i}` is `for {i} in core.iter.range(a, b)` (the `let mut {i}` is at line {})", pat.line)));
            }
        }
        _ => {}
    }
}

pub fn check(p: &Program, user: &[usize], tast: &Tast) -> Vec<Diag> {
    let mut out: Vec<Diag> = vec![];
    for &di in user {
        let d = &p.defs[di];
        if d.kw != "fn" && d.kw != "test" {
            continue;
        }
        let Some(b) = tast.bodies.get(&di) else { continue };
        let mut found: Vec<(usize, &'static str, String)> = vec![];
        super::impl_sig::walk(&b.body, &mut |e| {
            if let Some(m) = identity(e) {
                found.push((e.line, "E2062", m));
            }
            shapes(p, e, &mut found);
            match &e.kind {
            TKind::MCall { recv, name, args } => {
                if args.is_empty() && matches!(name.as_str(), "is_some" | "is_none" | "is_ok" | "is_err") && is_named(&recv.ty, &["core.option.Option", "core.result.Result"]) {
                    found.push((e.line, "E2055", format!("`.{name}()` is not the canonical form: write `x is Some(_)` (or `if let` when the value is used)")));
                }
                // `s.trim_start().trim_end()` (両順) は `s.trim()`。
                if let (Some(Callee::Def(outer)), TKind::Call { .. }) = (&e.res, &recv.kind) {
                    if let Some(Callee::Def(inner)) = &recv.res {
                        let (o, i) = (p.defs[*outer].name.as_str(), p.defs[*inner].name.as_str());
                        if (o == "core.str.trim_end" && i == "core.str.trim_start") || (o == "core.str.trim_start" && i == "core.str.trim_end") {
                            found.push((e.line, "E2060", "`trim_start().trim_end()` (either order) is `trim()`".into()));
                        }
                    }
                }
                if matches!(name.as_str(), "sort_unstable" | "sort_unstable_by") {
                    found.push((e.line, "E2058", format!("`{name}` is removed: use `{}`", name.replacen("sort_unstable", "sort", 1))));
                }
            }
            TKind::Unary { op, operand } if op == "!" => {
                // 整数の `!(a < b)` は `a >= b` (小数は NaN で意味が変わるので対象外)。
                if let TKind::Binary { op: c, lhs, rhs } = &operand.kind {
                    if matches!(c.as_str(), "<" | "<=" | ">" | ">=") && is_int128(&lhs.ty) && is_int128(&rhs.ty) {
                        found.push((e.line, "E2066", format!("`!(a {c} b)` on integers is `a {} b`", flip(c))));
                    }
                }
            }
            TKind::Call { callee, args } => {
                // `x.clone()` (基本型) は `x`。受け手は呼び出しの経路に入る。
                if args.is_empty() {
                    if let TKind::Path(pth) = &callee.kind {
                        if pth.len() >= 2 && pth.last().map(String::as_str) == Some("clone") && matches!(callee.ty.strip(), Ty::Prim(n) if n != "str") && matches!(e.ty.strip(), Ty::Prim(_)) {
                            found.push((e.line, "E2065", "`.clone()` on a `Copy` type is the value itself: remove the call".into()));
                        }
                    }
                }
                // 明示の型引数 `f::<T>(..)` が引数で決まるなら書かない。
                if let (Some(n), Some(Callee::Def(d))) = (e.tf, &e.res) {
                    let def = &p.defs[*d];
                    let tys: Vec<&str> = def.generics.iter().filter(|g| g.0 != "life").map(|g| g.1.as_str()).collect();
                    let all_type = def.generics.iter().all(|g| g.0 == "type" || g.0 == "life");
                    // 書いた型引数を各引数の宣言の型に入れた型が、引数の式の型と一致するときだけ (食い違いは別の診断が出る。直して通してはならない)。
                    let gn = def.gnames();
                    let written: Vec<Ty> = e.etargs.0.iter().filter(|v| !matches!(v, crate::parser::V::L(y) if matches!(y.first(), Some(crate::parser::V::S(s)) if s == "life"))).map(|v| crate::types::from_v(v, &p.defs[di].gnames())).collect();
                    let bind: std::collections::HashMap<String, Ty> = tys.iter().map(|s| s.to_string()).zip(written.iter().cloned()).collect();
                    let mut ok = n > 0 && all_type && n == tys.len() && !def.is_method() && args.len() == def.params.len() && written.len() == n;
                    if ok {
                        for t in &tys {
                            let mut seen = false;
                            for (i, (_, pty)) in def.params.iter().enumerate() {
                                if mentions(&pty.dump(), t) {
                                    seen = true;
                                    let want = crate::types::subst(&crate::types::from_v(pty, &gn), &bind);
                                    if !closed_arg(&args[i]) || want.show() != args[i].ty.show() {
                                        ok = false;
                                    }
                                }
                            }
                            ok &= seen;
                        }
                    }
                    {
                        if ok {
                            found.push((e.line, "E2067", "the type argument `::<..>` is decided by the arguments: remove it".into()));
                        }
                    }
                }
                // `alloc.fmt.write(&mut s, &"lit")` は `s.push_str("lit")`。
                if let Some(Callee::Def(d)) = &e.res {
                    if p.defs[*d].name == "alloc.fmt.write" && args.len() == 2 {
                        if let TKind::Unary { op, operand } = &args[1].kind {
                            if op == "&" && matches!(operand.kind, TKind::Lit(_)) && matches!(operand.ty.strip(), Ty::Prim(n) if n == "str") {
                                found.push((e.line, "E2061", "`alloc.fmt.write(&mut s, &\"..\")` of a string literal is `s.push_str(\"..\")`".into()));
                            }
                        }
                    }
                }
                // `s.push_display(&"lit")` は `s.push_str("lit")`。
                if let Some(Callee::Def(d)) = &e.res {
                    if p.defs[*d].name == "alloc.string.String.push_display" && args.len() == 1 {
                        if let TKind::Unary { op, operand } = &args[0].kind {
                            if op == "&" && matches!(operand.kind, TKind::Lit(_)) && matches!(operand.ty.strip(), Ty::Prim(n) if n == "str") {
                                found.push((e.line, "E2061", "`push_display(&\"..\")` of a string literal is `push_str(\"..\")`".into()));
                            }
                        }
                    }
                }
                // `x.is_some()` 等は解決先の定義で見る (受け手は呼び出しの経路に入り、引数には並ばない)。
                if let Some(Callee::Def(id)) = &e.res {
                    let name = p.defs[*id].name.as_str();
                    if let Some(m) = ["is_some", "is_none", "is_ok", "is_err"]
                        .into_iter()
                        .find(|m| (name == format!("core.option.Option.{m}") || name == format!("core.result.Result.{m}")))
                    {
                        let pat = match m {
                            "is_some" => "Some(_)",
                            "is_none" => "None",
                            "is_ok" => "Ok(_)",
                            _ => "Err(_)",
                        };
                        found.push((e.line, "E2055", format!("`.{m}()` is not the canonical form: write `x is {pat}` (or `if let` when the value is used)")));
                    }
                }
                if let Some(Callee::Def(d)) = &e.res {
                    let n = p.defs[*d].name.as_str();
                    let last = n.rsplit('.').next().unwrap_or("");
                    if args.len() == 1 && n.starts_with("core.num.") {
                        let v = int_lit(&args[0]);
                        if matches!((last, v), ("wrapping_add" | "wrapping_sub", Some(0)) | ("wrapping_mul", Some(1))) {
                            found.push((e.line, "E2062", format!("`.{last}(..)` with this literal changes nothing: remove the call")));
                        }
                    }
                    if n == "core.bool.bool.then_some" {
                        found.push((e.line, "E2063", "`then_some` is removed: write `if c { Some(v) } else { None }`".into()));
                    }
                }
                // `u32.from_str_radix(s, 10)` は `u32.parse(s)`。
                if let Some(Callee::Def(d)) = &e.res {
                    let n = p.defs[*d].name.as_str();
                    if n.starts_with("core.num.") && n.ends_with(".from_str_radix") && args.len() == 2 && int_lit(&args[1]) == Some(10) {
                        found.push((e.line, "E2064", "`from_str_radix(s, 10)` is `parse(s)`".into()));
                    }
                }
                if let TKind::Path(pth) = &callee.kind {
                    let s: Vec<&str> = pth.iter().map(String::as_str).collect();
                    if s == ["core", "panic", "todo"] && args.is_empty() {
                        found.push((e.line, "E2057", "`core.panic.todo` is removed: write `core.panic.panic(\"...\")`".into()));
                    }
                    if matches!(s.as_slice(), [.., "sort_unstable"] | [.., "sort_unstable_by"]) {
                        let last = *s.last().unwrap();
                        found.push((e.line, "E2058", format!("`{last}` is removed: use `{}`", last.replacen("sort_unstable", "sort", 1))));
                    }
                    if s == ["core", "check", "is_true"] && args.len() == 1 {
                        if let TKind::Binary { op, lhs, rhs } = &args[0].kind {
                            if op == "==" && scalar(&lhs.ty) && scalar(&rhs.ty) {
                                found.push((e.line, "E2059", "`core.check.is_true(a == b)` is not the canonical form: write `core.check.eq(a, b)`".into()));
                            }
                        }
                    }
                }
            }
            TKind::Binary { op, lhs, rhs } if op == "==" || op == "!=" => {
                let opt = |x: &TExpr| is_named(&x.ty, &["core.option.Option"]);
                if (is_none_path(rhs) && opt(lhs)) || (is_none_path(lhs) && opt(rhs)) {
                    found.push((e.line, "E2055", format!("`{op} None` is not the canonical form: write `x is None` / `x is Some(_)`")));
                }
            }
            _ => {}
            }
        });
        found.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        found.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        for (line, code, msg) in found {
            out.push(diag(p, di, line, code, msg));
        }
    }
    out
}
