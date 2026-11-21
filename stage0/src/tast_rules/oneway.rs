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

/// `Some(_)` / `None` の `is` が値を使い切らない型 (線形の値を含まない)。線形の値 (`Option<ハンドル>` など) は `is` が E3003 になるので、
/// `is_some()` 等 (借用) のままにする。判定できない型 (型引数・未知・`&mut`) は `false` (直さない)。
fn plain(p: &Program, t: &Ty, seen: &mut Vec<String>) -> bool {
    match t.strip() {
        Ty::Prim(_) | Ty::Unit | Ty::Never => true,
        Ty::Ref(false, _, _) => true,
        Ty::Tuple(xs) => xs.iter().all(|x| plain(p, x, seen)),
        Ty::Array(x, _) | Ty::Slice(x) => plain(p, x, seen),
        Ty::Named(n, args, _) => {
            if !args.iter().all(|a| plain(p, a, seen)) {
                return false;
            }
            if matches!(n.as_str(), "core.option.Option" | "core.result.Result" | "Option" | "Result") || seen.contains(n) {
                return true;
            }
            let Some(&i) = p.types.get(n) else { return false };
            let d = &p.defs[i];
            if d.linear {
                return false;
            }
            seen.push(n.clone());
            let gn = d.gnames();
            let ok = d.fields.iter().map(|f| crate::types::from_v(&f.1, &gn)).chain(d.variants.iter().flat_map(|v| v.1.iter().map(|x| crate::types::from_v(x, &gn)))).all(|x| plain(p, &x, seen));
            seen.pop();
            ok
        }
        _ => false,
    }
}

fn plain_ty(p: &Program, t: &Ty) -> bool {
    plain(p, t, &mut vec![])
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

pub fn check(p: &Program, user: &[usize], tast: &Tast) -> Vec<Diag> {
    let mut out: Vec<Diag> = vec![];
    for &di in user {
        let d = &p.defs[di];
        if d.kw != "fn" && d.kw != "test" {
            continue;
        }
        let Some(b) = tast.bodies.get(&di) else { continue };
        let mut found: Vec<(usize, &'static str, String)> = vec![];
        super::impl_sig::walk(&b.body, &mut |e| match &e.kind {
            TKind::MCall { recv, name, args } => {
                if args.is_empty() && matches!(name.as_str(), "is_some" | "is_none" | "is_ok" | "is_err") && is_named(&recv.ty, &["core.option.Option", "core.result.Result"]) && plain_ty(p, &recv.ty) {
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
                    if n > 0 && all_type && n == tys.len() && !def.is_method() && args.len() == def.params.len() {
                        let mut ok = true;
                        for t in &tys {
                            let mut seen = false;
                            for (i, (_, pty)) in def.params.iter().enumerate() {
                                if mentions(&pty.dump(), t) {
                                    seen = true;
                                    if !closed_arg(&args[i]) {
                                        ok = false;
                                    }
                                }
                            }
                            ok &= seen;
                        }
                        if ok {
                            found.push((e.line, "E2067", "the type argument `::<..>` is decided by the arguments: remove it".into()));
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
                        .find(|m| (name == format!("core.option.Option.{m}") || name == format!("core.result.Result.{m}")) && plain_ty(p, &callee.ty))
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
                let opt = |x: &TExpr| is_named(&x.ty, &["core.option.Option"]) && plain_ty(p, &x.ty);
                if (is_none_path(rhs) && opt(lhs)) || (is_none_path(lhs) && opt(rhs)) {
                    found.push((e.line, "E2055", format!("`{op} None` is not the canonical form: write `x is None` / `x is Some(_)`")));
                }
            }
            _ => {}
        });
        found.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        found.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        for (line, code, msg) in found {
            out.push(diag(p, di, line, code, msg));
        }
    }
    out
}
