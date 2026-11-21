//! 規則 O: 書き方は 1 通り (設計_書き方は1通り.md 12 章、2026-11-21 の決定)。TAST の型と呼び出し先だけで決める。
//! - E2055: `Option` / `Result` の判定メソッド (`is_some` `is_none` `is_ok` `is_err`) は `式 is Some(_)` 等と書く。
//! - E2055: `Option` と `None` の比較 `x == None` / `x != None` は `x is None` / `!(x is None)` と書く。
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

/// `Option<T>` / `Result<T, E>` で、すべての型の引数が基本型 (`is` の書き換えが移動を増やさない)。
fn scalar_payload(t: &Ty) -> bool {
    matches!(t.strip(), Ty::Named(_, args, _) if !args.is_empty() && args.iter().all(scalar))
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
            TKind::Call { callee, args } => {
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
                    if let Some(m) = ["is_some", "is_none", "is_ok", "is_err"].into_iter().find(|m| {
                        (name == format!("core.option.Option.{m}") || name == format!("core.result.Result.{m}")) && scalar_payload(&callee.ty)
                    }) {
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
                let opt = |x: &TExpr| is_named(&x.ty, &["core.option.Option"]) && scalar_payload(&x.ty);
                if (is_none_path(rhs) && opt(lhs)) || (is_none_path(lhs) && opt(rhs)) {
                    found.push((e.line, "E2055", format!("`{op} None` is not the canonical form: write `x is None` / `!(x is None)`")));
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
