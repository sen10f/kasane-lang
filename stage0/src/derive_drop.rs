//! `derive [Drop]` (仕様 4.3 / 5.3): 再帰する型の、深さに依らない破棄。
//!
//! 破棄そのものは各バックエンドが持つ (木走査・VM は Rust の値の破棄で、KIR / LLVM は `kir_lower_drop.rs` の反復する補助関数)。
//! ここは「反復で破棄できる形か」の検査だけを行う。反復の手法は、再帰の出現を `Box<Self, A>` か `Option<Box<Self, A>>` に限り、
//! 子の箱を作業リストへ移して順に破棄する (作業リストは解体済みの親の確保の中に置くので、確保も失敗もない)。

use crate::check::{items, st, tag, tel};
use crate::parser::V;
use crate::program::{Def, Program};

fn fqn(p: &Program, file: &str, name: &str) -> String {
    let segs: Vec<String> = name.split('.').map(String::from).collect();
    p.expand(&segs, file).join(".")
}

fn type_args(t: &V) -> Vec<&V> {
    items(tel(t, 2)).iter().filter(|a| matches!(a, V::L(_))).collect()
}

/// `t` の中に名義型 `name` が現れるか。
fn mentions(p: &Program, file: &str, t: &V, name: &str) -> bool {
    match tag(t).as_str() {
        "named" => {
            fqn(p, file, &st(tel(t, 1))) == name || type_args(t).iter().any(|a| mentions(p, file, a, name))
        }
        "ref" | "ptr" => mentions(p, file, tel(t, 2), name),
        "slice" | "array" => mentions(p, file, tel(t, 1), name),
        "tuple" => items(tel(t, 1)).iter().any(|x| mentions(p, file, x, name)),
        _ => false,
    }
}

/// `t` が `Box<name, A>` か。
/// 再帰の出現は `Self` そのもの (自分の型パラメータをそのまま渡した `name<T, ..>`) に限る。`name<i32>` のような多相再帰は反復で破棄できない。
fn is_box_of(p: &Program, file: &str, t: &V, name: &str, own: &[String]) -> bool {
    if tag(t) != "named" || fqn(p, file, &st(tel(t, 1))) != "alloc.boxed.Box" {
        return false;
    }
    let a = type_args(t);
    if !(a.len() == 2 && tag(a[0]) == "named" && fqn(p, file, &st(tel(a[0], 1))) == name && !mentions(p, file, a[1], name)) {
        return false;
    }
    let targs: Vec<&V> = type_args(a[0]);
    targs.len() == own.len()
        && targs.iter().zip(own).all(|(x, n)| tag(x) == "named" && st(tel(x, 1)) == *n && type_args(x).is_empty())
}

/// フィールド (ペイロード) の型 `t` の再帰の出現が、反復で破棄できる形 (`Box<Self>` / `Option<Box<Self>>`) か。
pub(crate) fn recursive_slot(p: &Program, file: &str, t: &V, name: &str, own: &[String]) -> bool {
    if is_box_of(p, file, t, name, own) {
        return true;
    }
    if tag(t) == "named" && matches!(fqn(p, file, &st(tel(t, 1))).as_str(), "Option" | "core.option.Option") {
        let a = type_args(t);
        return a.len() == 1 && is_box_of(p, file, a[0], name, own);
    }
    false
}

/// `d` の `derive [Drop]` が使えない理由 (空なら使える)。`mutual` は、`d` が他の型と相互再帰しているか。
pub(crate) fn problems(p: &Program, d: &Def, mutual: bool) -> Vec<String> {
    let mut out = vec![];
    if d.kw == "type" && d.derive.iter().any(|x| x == "Copy") || d.kw == "enum" && d.derive.iter().any(|x| x == "Copy") {
        out.push("a Copy type cannot implement Drop".to_string());
    }
    if mutual {
        out.push(
            "the type is mutually recursive with another type; derive [Drop] handles self-recursion only (write core.ops.Drop by hand, or reach the other type through an index)"
                .to_string(),
        );
    }
    let tys: Vec<&V> = if d.kw == "type" {
        d.fields.iter().map(|f| &f.1).collect()
    } else {
        d.variants.iter().flat_map(|v| v.1.iter()).collect()
    };
    let own: Vec<String> = d.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.clone()).collect();
    if !tys.iter().any(|t| mentions(p, &d.file, t, &d.name)) {
        out.push(format!("{} does not recur; derive [Drop] is for self-recursive types (a non-recursive type is dropped as usual)", d.name));
    }
    for t in tys {
        if mentions(p, &d.file, t, &d.name) && !recursive_slot(p, &d.file, t, &d.name, &own) {
            out.push(format!(
                "{} recurs through a field type other than Box<{0}> or Option<Box<{0}>> (derive [Drop] cannot bound the depth through Vec, tuples, references and the like)",
                d.name
            ));
            break;
        }
    }
    out
}
