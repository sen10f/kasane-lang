//! KIR の下ろしと参照実行器のテスト (3)。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use common::*;

#[test]
fn kir_eq_operator_on_option_result_aggregates_match_tree_walker() {
    // `==` / `!=` が Option / Result / 派生の構造体・列挙型 / `&[u8]` / `&str` でも下ろせ、木走査と一致する。
    let fns = "type t.P derive [Copy, Clone, PartialEq]\n    a: u8\n    b: i32\nend t.P\n\n\
enum t.E derive [Copy, Clone, PartialEq]\n    One(u8, bool)\n    Zero\nend t.E\n\n\
fn t.opt(b: &[u8], i: usize) -> bool uses [] panics never\n    b.get(i) == Some(b']')\nend t.opt\n\n\
fn t.opt_ne(b: &[u8], i: usize) -> bool uses [] panics never\n    b.get(i) != Some(b']')\nend t.opt_ne\n\n\
fn t.res(x: u8, y: u8) -> bool uses [] panics never\n    let mut a: Result<u8, u8> = Err(x)\n    let mut b: Result<u8, u8> = Err(y)\n    if x > 5u8 {\n        a = Ok(x)\n    }\n    if y > 5u8 {\n        b = Ok(y)\n    }\n    a == b\nend t.res\n\n\
fn t.pair(x: u8, y: u8) -> bool uses [] panics never\n    t.P { a: x, b: -3i32 } == t.P { a: y, b: -3i32 }\nend t.pair\n\n\
fn t.en(x: u8, y: u8) -> bool uses [] panics never\n    t.E.One(x, true) != t.E.One(y, true)\nend t.en\n\n\
fn t.en_zero(x: u8) -> bool uses [] panics never\n    if x == 0u8 {\n        return t.E.Zero == t.E.Zero\n    }\n    t.E.Zero == t.E.One(x, false)\nend t.en_zero\n\n\
fn t.bytes(a: &str, b: &str) -> bool uses [] panics never\n    a.as_bytes() == b.as_bytes()\nend t.bytes\n";
    let cases: &[(&str, &str, &str, &str)] = &[
        ("t.opt", "\"x]\".as_bytes(), 1usize", "true", "true"),
        ("t.opt", "\"x]\".as_bytes(), 0usize", "false", "false"),
        ("t.opt", "\"x]\".as_bytes(), 7usize", "false", "false"),
        ("t.opt_ne", "\"x]\".as_bytes(), 7usize", "true", "true"),
        ("t.opt_ne", "\"x]\".as_bytes(), 1usize", "false", "false"),
        ("t.res", "9u8, 9u8", "true", "true"),
        ("t.res", "9u8, 3u8", "false", "false"),
        ("t.res", "3u8, 3u8", "true", "true"),
        ("t.pair", "4u8, 4u8", "true", "true"),
        ("t.pair", "4u8, 5u8", "false", "false"),
        ("t.en", "1u8, 1u8", "false", "false"),
        ("t.en", "1u8, 2u8", "true", "true"),
        ("t.en_zero", "0u8", "true", "true"),
        ("t.en_zero", "3u8", "false", "false"),
        ("t.bytes", "\"ab\", \"ab\"", "true", "true"),
        ("t.bytes", "\"ab\", \"ac\"", "false", "false"),
        ("t.bytes", "\"ab\", \"abc\"", "false", "false"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args);
        let src = format!(
            "{}\nfn t.go() -> bool uses [] panics never\n    {}\nend t.go\n\nfn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.eq(t.go(), {})\n    Ok(())\nend t.main\n",
            fns, call, lit
        );
        let t = Tmp::new(&format!("kir_eq_op_{}", i), &src);
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}{}", call, text(&o.stdout), text(&o.stderr));
        let k = kir_run(&["kir", "t.go", "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_eq_on_aggregates_reused_after_and_in_loop_match_tree_walker() {
    // 比較の後にも使う・ループの中で比べる。派生の比較は借用でなく値渡しだが、所有物を持たない (Copy でない) 型なので解放の問題は出ない。
    let src = "type t.P derive [Clone, PartialEq]\n    a: u8\n    b: i32\nend t.P\n\n\
fn t.go(k: u8) -> u32 uses [] panics may\n    let a = t.P { a: 1u8, b: 2i32 }\n    let b = t.P { a: k, b: 2i32 }\n    let mut c = 0u32\n    let mut i = 0u32\n    while i < 3u32 {\n        if a == b {\n            c = c + 10u32\n        }\n        if a != b {\n            c = c + 1u32\n        }\n        i = i + 1u32\n    }\n    if a == b {\n        c = c + 100u32\n    }\n    c + u32.from_u8(a.a) + u32.from_u8(b.a)\nend t.go\n\n\
fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.eq(t.go(1u8), 132u32)\n    core.check.eq(t.go(4u8), 8u32)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_eq_reuse", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker: {}{}", text(&o.stdout), text(&o.stderr));
    for (arg, want) in [("1", "132"), ("4", "8")] {
        let k = kir_run(&["kir", "t.go", "--run", arg], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}: {}", arg, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want);
    }
}

#[test]
fn kir_eq_on_aggregates_owning_heap_is_refused_with_a_reason() {
    // String を持つ型は derive できず (E2036)、手書きの eq は下ろせない。どちらにしても == / != は理由付きで拒否する (二重解放を作らない)。
    let src = "type t.P\n    name: alloc.string.String<std.alloc.System>\n    n: u32\nend t.P\n\nimpl core.cmp.PartialEq<t.P> for t.P\n\n\
fn t.P.eq(self: &t.P, other: &t.P) -> bool uses [] panics never impls core.cmp.PartialEq<t.P>\n    self.n == other.n\nend t.P.eq\n\n\
fn t.go() -> bool uses [alloc] panics never\n    let a = t.P { name: alloc.string.String.new(std.alloc.SYSTEM), n: 1u32 }\n    let b = t.P { name: alloc.string.String.new(std.alloc.SYSTEM), n: 1u32 }\n    let r = a == b\n    core.mem.drop(a)\n    core.mem.drop(b)\n    r\nend t.go\n\n\
fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    core.check.eq(t.go(), true)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_eq_owning", src);
    let k = run(&["kir", "t.go", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(1), "{}{}", text(&k.stdout), text(&k.stderr));
    assert!(text(&k.stderr).contains("比較は段階 1 の対象外"), "{}", text(&k.stderr));
}

#[test]
fn kir_get_on_vec_of_owning_elements_with_unsupported_pattern_is_refused_with_a_reason() {
    // `Option<&T>` の値の `Some(..)` の中は、名前・_・構造体のパターンだけ。リテラルなど他のパターンは理由付きで拒否する。
    let src = "type t.Item\n    name: alloc.string.String<std.alloc.System>\n    v: u32\nend t.Item\n\n\
fn t.make() -> alloc.vec.Vec<t.Item, std.alloc.System> uses [alloc] panics never\n    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    let s = alloc.string.String.new(std.alloc.SYSTEM)\n    core.mem.drop(v.push(t.Item { name: s, v: 1u32 }))\n    v\nend t.make\n\n\
fn t.pick(i: usize) -> u32 uses [alloc] panics never\n    let v = t.make()\n    let g = v.get(i)\n    match g {\n        None => 0u32\n        Some(t.Item { name: _, v: 1u32 }) => 1u32\n        Some(_) => 2u32\n    }\nend t.pick\n\n\
fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    core.check.eq(t.pick(0usize), 1u32)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_get_owning", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let k = run(&["kir", "t.pick", "--run", "0"], &t.0);
    assert_eq!(k.status.code(), Some(1), "{}", text(&k.stdout));
    assert!(text(&k.stderr).contains("構造体パターン"), "{}", text(&k.stderr));
}

#[test]
fn kir_unwrap_on_option_of_borrowed_owning_elem_is_refused_with_a_reason() {
    let src = format!("{}\nfn t.uw(n: u32) -> usize uses [alloc] panics may\n    let v = t.names(n)\n    let g = v.get(0usize)\n    g.unwrap().len()\nend t.uw\n\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    core.check.eq(t.uw(2u32), 3usize)\n    Ok(())\nend t.main\n", KIR_LETGET_FNS);
    let t = Tmp::new("kir_letget_unwrap", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let k = run(&["kir", "t.uw", "--run", "2"], &t.0);
    assert_eq!(k.status.code(), Some(1), "{}", text(&k.stdout));
    assert!(text(&k.stderr).contains("unwrap は段階 1 の対象外"), "{}", text(&k.stderr));
}

const KIR_LETGET_FNS: &str = "type t.Item
    name: alloc.string.String<std.alloc.System>
    v: u32
end t.Item

fn t.names(n: u32) -> alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        let mut s = alloc.string.String.new(std.alloc.SYSTEM)
        core.mem.drop(s.push_str(\"abc\"))
        core.mem.drop(v.push(s))
        i = i.wrapping_add(1u32)
    }
    v
end t.names

fn t.items(n: u32) -> alloc.vec.Vec<t.Item, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        let mut s = alloc.string.String.new(std.alloc.SYSTEM)
        core.mem.drop(s.push_str(\"ab\"))
        core.mem.drop(v.push(t.Item { name: s, v: i }))
        i = i.wrapping_add(1u32)
    }
    v
end t.items

fn t.let_get(n: u32, i: usize) -> usize uses [alloc] panics never
    let v = t.names(n)
    let g = v.get(i)
    match g {
        Some(s) => s.len()
        None => 99usize
    }
end t.let_get

fn t.let_is_some(n: u32, i: usize) -> u32 uses [alloc] panics never
    let v = t.names(n)
    let g = v.get(i)
    let mut r = 0u32
    if g.is_some() {
        r = r.wrapping_add(1u32)
    }
    if g.is_none() {
        r = r.wrapping_add(10u32)
    }
    r
end t.let_is_some

fn t.let_item(n: u32, i: usize) -> usize uses [alloc] panics never
    let v = t.items(n)
    let g = v.get(i)
    match g {
        Some(it) => it.name.len().wrapping_add(usize.from_u32(it.v))
        None => 99usize
    }
end t.let_item

fn t.let_struct_pat(n: u32, i: usize) -> u32 uses [alloc] panics never
    let v = t.items(n)
    let g = v.get(i)
    match g {
        Some(t.Item { name: _, v: x }) => x
        None => 99u32
    }
end t.let_struct_pat

fn t.find(v: &alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>, i: usize) -> Option<&alloc.string.String<std.alloc.System>> uses [] panics never
    v.get(i)
end t.find

fn t.find_item(v: &alloc.vec.Vec<t.Item, std.alloc.System>, i: usize) -> Option<&t.Item> uses [] panics never
    v.get(i)
end t.find_item

fn t.first(v: &alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>) -> Option<&alloc.string.String<std.alloc.System>> uses [] panics may
    if v.len() == 0usize {
        None
    } else {
        Some(&v[0usize])
    }
end t.first

fn t.len_of(g: Option<&alloc.string.String<std.alloc.System>>) -> usize uses [] panics never
    match g {
        Some(s) => s.len()
        None => 77usize
    }
end t.len_of

fn t.via_fn(n: u32, i: usize) -> usize uses [alloc] panics never
    let v = t.names(n)
    match t.find(&v, i) {
        Some(s) => s.len()
        None => 99usize
    }
end t.via_fn

fn t.via_let_fn(n: u32, i: usize) -> usize uses [alloc] panics never
    let v = t.names(n)
    let g = t.find(&v, i)
    let a = t.len_of(g)
    a.wrapping_add(t.len_of(t.find(&v, i)))
end t.via_let_fn

fn t.via_item(n: u32, i: usize) -> u32 uses [alloc] panics never
    let v = t.items(n)
    let g = t.find_item(&v, i)
    match g {
        Some(it) => it.v
        None => 99u32
    }
end t.via_item

fn t.via_first(n: u32) -> usize uses [alloc] panics may
    let v = t.names(n)
    t.len_of(t.first(&v))
end t.via_first

fn t.in_loop(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut total = 0usize
    let mut i = 0usize
    while i < 6usize {
        let g = v.get(i)
        match g {
            Some(s) => {
                total = total.wrapping_add(s.len())
            }
            None => {
                total = total.wrapping_add(100usize)
            }
        }
        i = i.wrapping_add(1usize)
    }
    total
end t.in_loop
";

#[test]
fn kir_let_get_of_owning_elems_matches_tree_walker() {
    // `let g = v.get(i)` の `Option<&T>` (T は String / String を持つ構造体) を値で持つ。`Option<&T>` を返す・引数に取る関数も下ろす。
    // 木走査と `--run` / `--native` (ASan があればそれも) が一致し、参照実行器が漏れ・二重解放を内部エラーにしない。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.let_get", &["3", "1"], "3usize", "3"),
        ("t.let_get", &["3", "7"], "99usize", "99"),
        ("t.let_get", &["0", "0"], "99usize", "99"),
        ("t.let_is_some", &["3", "1"], "1u32", "1"),
        ("t.let_is_some", &["3", "5"], "10u32", "10"),
        ("t.let_item", &["3", "2"], "4usize", "4"),
        ("t.let_item", &["3", "3"], "99usize", "99"),
        ("t.let_struct_pat", &["3", "2"], "2u32", "2"),
        ("t.let_struct_pat", &["3", "9"], "99u32", "99"),
        ("t.via_fn", &["3", "0"], "3usize", "3"),
        ("t.via_fn", &["3", "4"], "99usize", "99"),
        ("t.via_let_fn", &["3", "1"], "6usize", "6"),
        ("t.via_let_fn", &["3", "8"], "154usize", "154"),
        ("t.via_item", &["4", "3"], "3u32", "3"),
        ("t.via_item", &["4", "4"], "99u32", "99"),
        ("t.via_first", &["2"], "3usize", "3"),
        ("t.via_first", &["0"], "77usize", "77"),
        ("t.in_loop", &["4"], "212usize", "212"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let last = args.len() - 1;
        let call = format!("{}({})", f, args.iter().enumerate().map(|(k, a)| if args.len() == 2 && k == last { format!("{}usize", a) } else { format!("{}u32", a) }).collect::<Vec<_>>().join(", "));
        let body = format!("    core.check.eq({}, {})\n", call, lit);
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_letget_{}", i), &format!("{}{}", KIR_LETGET_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

const KIR_BORROW_FNS: &str = "type t.Item
    name: alloc.string.String<std.alloc.System>
    v: u32
end t.Item

fn t.word() -> alloc.string.String<std.alloc.System> uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(\"abc\"))
    s
end t.word

fn t.names(n: u32) -> alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(t.word()))
        i = i.wrapping_add(1u32)
    }
    v
end t.names

fn t.items(n: u32) -> alloc.vec.Vec<t.Item, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(t.Item { name: t.word(), v: i }))
        i = i.wrapping_add(1u32)
    }
    v
end t.items

fn t.get_str(n: u32, i: usize) -> usize uses [alloc] panics never
    let v = t.names(n)
    match v.get(i) {
        Some(s) => s.len()
        None => 99usize
    }
end t.get_str

fn t.get_item(n: u32, i: usize) -> usize uses [alloc] panics never
    let v = t.items(n)
    match v.get(i) {
        Some(it) => it.name.len().wrapping_add(usize.from_u32(it.v))
        None => 99usize
    }
end t.get_item

fn t.sum_names(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut total = 0usize
    for s in &v {
        total = total.wrapping_add(s.len())
    }
    total
end t.sum_names

fn t.sum_names_iter(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut total = 0usize
    for s in v.iter() {
        total = total.wrapping_add(s.len())
    }
    total
end t.sum_names_iter

fn t.sum_items(n: u32) -> usize uses [alloc] panics never
    let v = t.items(n)
    let mut total = 0usize
    for it in &v {
        total = total.wrapping_add(it.name.len())
    }
    total
end t.sum_items

fn t.sum_scalar(n: u32) -> u32 uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(i))
        i = i.wrapping_add(1u32)
    }
    let mut total = 0u32
    for x in &v {
        total = total.wrapping_add(*x)
    }
    total
end t.sum_scalar

fn t.name_len(s: &alloc.string.String<std.alloc.System>) -> usize uses [] panics never
    s.len()
end t.name_len

fn t.item_len(i: &t.Item) -> usize uses [] panics never
    i.name.len()
end t.item_len

fn t.idx_str(n: u32, i: usize) -> usize uses [alloc] panics may
    let v = t.names(n)
    v[i].len()
end t.idx_str

fn t.idx_item(n: u32, i: usize) -> usize uses [alloc] panics may
    let v = t.items(n)
    v[i].name.len().wrapping_add(usize.from_u32(v[i].v))
end t.idx_item

fn t.idx_ref(n: u32, i: usize) -> usize uses [alloc] panics may
    let v = t.items(n)
    let w = t.names(n)
    t.item_len(&v[i]).wrapping_add(t.name_len(&w[i]))
end t.idx_ref

fn t.get_loop(n: u32) -> usize uses [alloc] panics never
    let v = t.items(n)
    let mut total = 0usize
    let mut i = 0usize
    while i < usize.from_u32(n).wrapping_add(2usize) {
        match v.get(i) {
            Some(it) => {
                total = total.wrapping_add(it.name.len())
            }
            None => {
                total = total.wrapping_add(100usize)
            }
        }
        i = i.wrapping_add(1usize)
    }
    total
end t.get_loop

fn t.mutate_ok(n: u32) -> usize uses [alloc] panics never
    let mut v = t.names(n)
    let a = match v.get(0usize) {
        Some(s) => s.len()
        None => 0usize
    }
    core.mem.drop(v.push(t.word()))
    a.wrapping_add(v.len())
end t.mutate_ok

fn t.nested(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut total = 0usize
    for a in &v {
        for b in v.iter() {
            total = total.wrapping_add(a.len()).wrapping_add(b.len())
        }
    }
    total
end t.nested

";

#[test]
fn kir_borrow_owned_elems_match_tree_walker() {
    // 所有物を持つ要素の Vec (`Vec<String>` / String を持つ構造体の Vec) の `get` / 添字 / `for` は、要素の写しでなく番地 (借用) で下ろす。
    // 木走査と値が一致し、参照実行器が漏れ・二重解放を内部エラーにしない (`--run` / `--native`、ASan があればそれも)。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.get_str", &["3","1"], "3usize", "3"),
        ("t.get_str", &["3","7"], "99usize", "99"),
        ("t.get_str", &["0","0"], "99usize", "99"),
        ("t.get_item", &["3","2"], "5usize", "5"),
        ("t.get_item", &["3","3"], "99usize", "99"),
        ("t.get_loop", &["3"], "209usize", "209"),
        ("t.get_loop", &["0"], "200usize", "200"),
        ("t.sum_names", &["3"], "9usize", "9"),
        ("t.sum_names", &["0"], "0usize", "0"),
        ("t.sum_names_iter", &["4"], "12usize", "12"),
        ("t.sum_items", &["5"], "15usize", "15"),
        ("t.sum_scalar", &["5"], "10u32", "10"),
        ("t.idx_str", &["3","1"], "3usize", "3"),
        ("t.idx_item", &["3","1"], "4usize", "4"),
        ("t.idx_ref", &["3","1"], "6usize", "6"),
        ("t.mutate_ok", &["3"], "7usize", "7"),
        ("t.nested", &["3"], "54usize", "54"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let last = args.len() - 1;
        let call = format!("{}({})", f, args.iter().enumerate().map(|(k, a)| if args.len() == 2 && k == last { format!("{}usize", a) } else { format!("{}u32", a) }).collect::<Vec<_>>().join(", "));
        let body = format!("    core.check.eq({}, {})\n", call, lit);
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_borrow_{}", i), &format!("{}{}", KIR_BORROW_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_borrow_owned_elems_out_of_range_index_panics_like_tree_walker() {
    // 範囲外の添字 `v[i]` は、借用を作る前の境界の検査で panic する (終了コード 101)。
    let main = "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    core.check.eq(t.idx_str(3u32, 5usize), 3usize)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_borrow_oob", &format!("{}{}", KIR_BORROW_FNS, main));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101), "tree-walker: {}", text(&o.stderr));
    let k = kir_run(&["kir", "t.idx_str", "--run", "3", "5"], &t.0);
    assert_eq!(k.status.code(), Some(101), "kir: {}", text(&k.stderr));
    assert!(text(&k.stderr).contains("index out of bounds"), "{}", text(&k.stderr));
}

#[test]
fn kir_own_dump_shows_moves_as_nulling_and_flags() {
    // Vec の移動は元のヘッダの `ptr` を null にする書き込み、構造体の局所変数の移動は印 (1 バイト) の書き込みとして見える。
    let t = kir_own_pkg("kir_own_dump", "    core.check.eq(t.branch_buf(3u32, true), 3usize)\n");
    let o = run(&["kir", "t.branch"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let s = text(&o.stdout);
    assert!(s.contains("const.ptr 0") && s.contains("heap.free %") && s.contains("struct vec size=24"), "{}", s);
    let o = run(&["kir", "t.branch_buf"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let s = text(&o.stdout);
    assert!(s.contains("frame.bytes 1") && s.contains("mem.store.bool") && s.contains("mem.load.bool"), "{}", s);
}

/// `String.as_str()` / `Vec.as_slice()` はヘッダの {ptr, len} を借用の `fat` にする (所有は移さない)。
/// 木走査 (`ksn run` の `core.check.eq`) と、`--run` / `--native` の値が一致する。
#[test]
fn kir_as_str_and_as_slice_borrow_the_header() {
    let src = r#"fn t.sum_bytes(s: &str) -> u32 uses [] panics never
    let b = s.as_bytes()
    let mut acc = 0u32
    for x in b {
        acc = acc.wrapping_add(u32.from_u8(x))
    }
    acc
end t.sum_bytes

fn t.sum_slice(v: &[u16]) -> u32 uses [] panics never
    let mut acc = 0u32
    for x in v {
        acc = acc.wrapping_add(u32.from_u16(x))
    }
    acc
end t.sum_slice

fn t.go(n: u16) -> u32 uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u16
    while i < n {
        core.mem.drop(v.push(i))
        i = i.wrapping_add(1u16)
    }
    let s = alloc.string.String.new(std.alloc.SYSTEM)
    let a = t.sum_slice(v.as_slice())
    let b = t.sum_bytes(s.as_str())
    if s.as_str().len() == 0usize {
        return a.wrapping_add(b).wrapping_add(1000u32)
    }
    a.wrapping_add(b)
end t.go

fn t.main() -> () uses [alloc, io.console] panics may
    core.check.eq(t.go(10u16), 1045u32)
end t.main
"#;
    let t = Tmp::new("kir_as_str_slice", src);
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
    for (n, want) in [("10", "1045"), ("0", "1000")] {
        let o = kir_run(&["kir", "t.go", "--run", n], &t.0);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), want);
    }
}

#[test]
fn kir_closure_capture_by_mut_ref_writes_through() {
    // `capture [&mut x]` の書き込みは、取り込まれた変数に届く。木走査は `*x = v` の形を受けない
    // (取り込んだ名前を参照にしない) ので、期待値は手で求めた値 (3, 6 と加えて 11 を返し、total も 11 → 22)。
    let t = Tmp::new("kir_closure_cap_mut", &format!("{}{}", KIR_CLOSURE_FNS, main_wrap("", "    core.check.eq(t.cap_ref(1u64), 1u64)\n")));
    let k = kir_run(&["kir", "t.cap_mut", "--run", "3"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir: {}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "22");
}

#[test]
fn kir_closure_environment_is_passed_as_leading_arguments() {
    // 取り込みの値は、クロージャの関数と単相インスタンスの先頭の追加の引数になる。
    let t = Tmp::new("kir_closure_dump", &format!("{}{}", KIR_CLOSURE_FNS, main_wrap("", "    core.check.eq(t.cap_ref(4u64), 46u64)\n")));
    let o = run(&["kir", "t.cap_ref"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("fn t.cap_ref#c") && out.contains("fn t.apply key=t.apply<t.cap_ref#c"), "{}", out);
}

#[test]
fn kir_closure_reports_unsupported_captures_with_a_reason() {
    // 解放の要る値の取り込み、move で取り込んだ値の変更、反復子の未対応の元は理由付きで拒否する。
    let cases: &[(&str, &str, &str)] = &[
        ("t.cap_mutate", "    let mut n = 1usize\n    let f = fn(x: usize) -> usize capture [move n] {\n        n = n.wrapping_add(x)\n        n\n    }\n    f(1usize)\n", "を本体が変更する形"),
        ("t.take", "    core.iter.count(core.iter.take(core.iter.range(0usize, 5usize), 2usize))\n", "take"),
    ];
    for (i, (f, body, why)) in cases.iter().enumerate() {
        let src = format!(
            "fn {}() -> usize uses [alloc] panics may\n{}end {}\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics may\n    Ok(())\nend t.main\n",
            f, body, f
        );
        let t = Tmp::new(&format!("kir_closure_reason_{}", i), &src);
        let o = run(&["kir", f], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}: {}", f, text(&o.stderr));
        assert!(text(&o.stderr).contains(why), "{}: {}", f, text(&o.stderr));
    }
}

/// `Vec.get` の `Option<&スカラー>` の束縛 (`Some(c)`) を、`*c`、`&T` の引数、`c == &y` で使う。
/// 束縛は値で持つ (設計_KIR.md 10.10) ので、`*c` は値そのもの、`&T` の引数は一時に写した参照になる。
const DEREF_OPT_SRC: &str = r#"fn t.show(x: &u32) -> u32 uses [] panics never
    *x
end t.show

fn t.fill() -> Result<alloc.vec.Vec<u32>, alloc.alloc.AllocError> uses [alloc] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    v.push(3u32)?
    v.push(8u32)?
    Ok(v)
end t.fill

fn t.pick(i: usize) -> u32 uses [alloc] panics may
    let v = match t.fill() {
        Ok(v) => v
        Err(_e) => core.panic.panic("oom")
    }
    match v.get(i) {
        Some(c) => t.show(c)
        None => 9u32
    }
end t.pick

fn t.same(i: usize, x: u32) -> bool uses [alloc] panics may
    let v = match t.fill() {
        Ok(v) => v
        Err(_e) => core.panic.panic("oom")
    }
    match v.get(i) {
        Some(c) => *c == x
        None => false
    }
end t.same

fn t.eqref(i: usize, x: u32) -> bool uses [alloc] panics may
    let v = match t.fill() {
        Ok(v) => v
        Err(_e) => core.panic.panic("oom")
    }
    let y = x
    match v.get(i) {
        Some(c) => c == &y
        None => false
    }
end t.eqref

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.pick(1usize), 8u32)
    core.check.eq(t.pick(5usize), 9u32)
    core.check.eq(t.same(0usize, 3u32), true)
    core.check.eq(t.same(1usize, 3u32), false)
    core.check.eq(t.eqref(0usize, 3u32), true)
    core.check.eq(t.eqref(0usize, 4u32), false)
    Ok(())
end t.main
"#;

#[test]
fn kir_deref_of_scalar_option_ref_matches_tree_walker() {
    let t = Tmp::new("kir_deref_opt_ref", DEREF_OPT_SRC);
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0), "tree-walker main");
    let cases: &[(&str, &[&str], &str)] = &[
        ("t.pick", &["1"], "8"),
        ("t.pick", &["5"], "9"),
        ("t.same", &["0", "3"], "true"),
        ("t.same", &["1", "3"], "false"),
        ("t.eqref", &["0", "3"], "true"),
        ("t.eqref", &["0", "4"], "false"),
    ];
    for (f, args, want) in cases {
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {} {:?}\n{}", f, args, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {} {:?}", f, args);
    }
}

/// 利用者の反復子 (`core.iter.Iterator` を実装し、`&mut self` の `next` を持つ) を `for` で回す。
/// `continue` は次の `next` へ、`break` は抜ける。内側のループの反復子も、外側のループの中で作って回す。
const USER_ITER_SRC: &str = r#"type t.Upto pub
    cur: u32
    stop: u32
end t.Upto

impl core.iter.Iterator<u32, [], never> for t.Upto
fn t.Upto.next(self: &mut t.Upto) -> Option<u32> uses [] panics never impls core.iter.Iterator<u32, [], never>
    if self.cur >= self.stop {
        return None
    }
    let v = self.cur
    self.cur = self.cur.wrapping_add(1u32)
    Some(v)
end t.Upto.next

fn t.sum(a: u32, b: u32) -> u32 uses [] panics never
    let mut it = t.Upto { cur: a, stop: b }
    let mut acc = 0u32
    for n in it {
        acc = acc.wrapping_add(n)
    }
    acc
end t.sum

fn t.sum_skip_break(a: u32, b: u32, k: u32) -> u32 uses [] panics never
    let mut it = t.Upto { cur: a, stop: b }
    let mut acc = 0u32
    for n in it {
        if n == k {
            continue
        }
        if n > 20u32 {
            break
        }
        acc = acc.wrapping_add(n)
    }
    acc
end t.sum_skip_break

fn t.nested(n: u32) -> u32 uses [] panics never
    let mut it = t.Upto { cur: 0u32, stop: n }
    let mut total = 0u32
    for i in it {
        let mut jt = t.Upto { cur: 0u32, stop: i }
        for j in jt {
            total = total.wrapping_add(j)
        }
    }
    total
end t.nested

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.sum(0u32, 4u32), 6u32)
    core.check.eq(t.sum(3u32, 3u32), 0u32)
    core.check.eq(t.sum_skip_break(0u32, 100u32, 2u32), 208u32)
    core.check.eq(t.nested(4u32), 4u32)
    Ok(())
end t.main
"#;

#[test]
fn kir_user_iterator_for_matches_tree_walker() {
    let t = Tmp::new("kir_user_iter_for", USER_ITER_SRC);
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0), "tree-walker main");
    let cases: &[(&str, &[&str], &str)] = &[
        ("t.sum", &["0", "4"], "6"),
        ("t.sum", &["3", "3"], "0"),
        ("t.sum_skip_break", &["0", "100", "2"], "208"),
        ("t.nested", &["4"], "4"),
    ];
    for (f, args, want) in cases {
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {} {:?}\n{}", f, args, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {} {:?}", f, args);
    }
}

// ---- HashMap (設計_KIR.md 16 章、kir/hashmap) ----

const KIR_MAP_FNS: &str = r##"fn t.add_one(m: &mut alloc.collections.HashMap<u32, u32, std.alloc.System>, k: u32) -> bool uses [alloc] panics may
    match m.insert(k, k.wrapping_mul(2u32)) {
        Ok(Some(_old)) => false
        Ok(None) => true
        Err(_e) => core.panic.panic("oom")
    }
end t.add_one

fn t.build(n: u32) -> alloc.collections.HashMap<u32, u32, std.alloc.System> uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(t.add_one(&mut m, i % 40u32))
        i = i.wrapping_add(1u32)
    }
    m
end t.build

// 挿入と削除を繰り返す (墓標と作り直し)
fn t.churn(n: u32) -> u32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0u32
    let mut acc = 0u32
    while i < n {
        match m.insert(i, i) {
            Ok(_old) => 0u8
            Err(_e) => core.panic.panic("oom")
        }
        if i >= 4u32 {
            match m.remove(&(i.wrapping_sub(4u32))) {
                Some(v) => {
                    acc = acc.wrapping_add(v)
                }
                None => core.panic.panic("missing")
            }
        }
        i = i.wrapping_add(1u32)
    }
    for (k, _v) in m.iter() {
        acc = acc.wrapping_mul(7u32).wrapping_add(*k)
    }
    acc
end t.churn

fn t.fill(n: u32) -> u32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        match m.insert(i, i.wrapping_mul(3u32).wrapping_add(1u32)) {
            Ok(_old) => 0u8
            Err(_e) => core.panic.panic("oom")
        }
        i = i.wrapping_add(1u32)
    }
    let mut acc = 0u32
    let mut k = 0u32
    while k < n.wrapping_mul(2u32) {
        match m.get(&k) {
            Some(v) => {
                acc = acc.wrapping_add(*v)
            }
            None => {
                acc = acc.wrapping_add(1000000u32)
            }
        }
        k = k.wrapping_add(1u32)
    }
    acc
end t.fill

fn t.len_after(n: u32) -> usize uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        match m.insert(i % 7u32, i) {
            Ok(_old) => 0u8
            Err(_e) => core.panic.panic("oom")
        }
        i = i.wrapping_add(1u32)
    }
    m.len()
end t.len_after


fn t.missing(n: u32) -> u32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut acc = 0u32
    if m.is_empty() {
        acc = 1u32
    }
    match m.remove(&5u32) {
        Some(_v) => core.panic.panic("phantom")
        None => {
            acc = acc.wrapping_add(10u32)
        }
    }
    let mut i = 0u32
    while i < n {
        match m.insert(i, i) {
            Ok(_old) => 0u8
            Err(_e) => core.panic.panic("oom")
        }
        i = i.wrapping_add(1u32)
    }
    if m.contains_key(&(n.wrapping_add(3u32))) {
        acc = acc.wrapping_add(100u32)
    }
    if m.contains_key(&2u32) {
        acc = acc.wrapping_add(1000u32)
    }
    match m.get(&n) {
        Some(_v) => core.panic.panic("phantom")
        None => {
            acc = acc.wrapping_add(10000u32)
        }
    }
    acc
end t.missing

fn t.moved(n: u32) -> u32 uses [alloc] panics may
    let m = t.build(n)
    let mut acc = 0u32
    for (k, v) in m.iter() {
        acc = acc.wrapping_add(*k).wrapping_add(*v)
    }
    acc
end t.moved

// 削除・再挿入・反復順 (挿入順、削除後も相対順を保つ、再挿入は末尾)
fn t.order(n: u32) -> u32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        match m.insert(i.wrapping_mul(7919u32), i) {
            Ok(_old) => 0u8
            Err(_e) => core.panic.panic("oom")
        }
        i = i.wrapping_add(1u32)
    }
    i = 0u32
    while i < n {
        if i % 3u32 == 0u32 {
            match m.remove(&i.wrapping_mul(7919u32)) {
                Some(v) => core.check.eq(v, i)
                None => core.panic.panic("missing")
            }
        }
        i = i.wrapping_add(1u32)
    }
    i = 0u32
    while i < n {
        if i % 6u32 == 0u32 {
            match m.insert(i.wrapping_mul(7919u32), i.wrapping_add(1u32)) {
                Ok(_old) => core.mem.drop(_old)
                Err(_e) => core.panic.panic("oom")
            }
        }
        i = i.wrapping_add(1u32)
    }
    let mut acc = 0u32
    for (k, v) in m.iter() {
        acc = acc.wrapping_mul(31u32).wrapping_add(*k).wrapping_add(v.wrapping_mul(3u32))
    }
    acc
end t.order

fn t.overwrite(n: u32) -> u32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut acc = 0u32
    let mut i = 0u32
    while i < n {
        match m.insert(i % 5u32, i.wrapping_add(10u32)) {
            Ok(Some(old)) => {
                acc = acc.wrapping_add(old)
            }
            Ok(None) => {
                acc = acc.wrapping_add(1u32)
            }
            Err(_e) => core.panic.panic("oom")
        }
        i = i.wrapping_add(1u32)
    }
    acc
end t.overwrite

fn t.signed(n: i32) -> i32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0i32
    while i < n {
        match m.insert(0i32.wrapping_sub(i), i) {
            Ok(_old) => 0u8
            Err(_e) => core.panic.panic("oom")
        }
        i = i.wrapping_add(1i32)
    }
    let mut acc = 0i32
    i = 0i32
    while i < n {
        match m.get(&(0i32.wrapping_sub(i))) {
            Some(v) => {
                acc = acc.wrapping_add(*v)
            }
            None => core.panic.panic("missing")
        }
        i = i.wrapping_add(1i32)
    }
    acc
end t.signed

fn t.strs(_n: u32) -> u32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    match m.insert("alpha", 1u32) {
        Ok(_old) => 0u8
        Err(_e) => core.panic.panic("oom")
    }
    match m.insert("beta", 2u32) {
        Ok(_old) => 0u8
        Err(_e) => core.panic.panic("oom")
    }
    match m.insert("alpha", 10u32) {
        Ok(_old) => 0u8
        Err(_e) => core.panic.panic("oom")
    }
    let mut acc = 0u32
    match m.get("alpha") {
        Some(v) => {
            acc = acc.wrapping_add(*v)
        }
        None => core.panic.panic("missing")
    }
    match m.get("gamma") {
        Some(_v) => core.panic.panic("phantom")
        None => {
            acc = acc.wrapping_add(100u32)
        }
    }
    if m.contains_key("beta") {
        acc = acc.wrapping_add(1000u32)
    }
    match m.remove("beta") {
        Some(v) => {
            acc = acc.wrapping_add(v.wrapping_mul(10000u32))
        }
        None => core.panic.panic("missing")
    }
    for (k, v) in m.iter() {
        acc = acc.wrapping_add(*v)
    }
    acc
end t.strs
"##;

fn kir_map_pkg(name: &str, main_body: &str) -> Tmp {
    let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", main_body);
    Tmp::new(name, &format!("{}\n{}", KIR_MAP_FNS, main))
}

#[test]
fn kir_map_values_match_tree_walker() {
    // 挿入・上書き (古い値が返る)・削除・再挿入 (末尾へ)・成長 (1 万件)・反復順 (挿入順)・存在しないキー・墓標と作り直し・
    // 符号付きと `&str` のキー・`&mut` の借用と値の戻り。参照実行器は漏れ・二重解放・範囲外を内部エラーにする。
    // 期待値の書いた場所は木走査の `core.check.eq` で確かめ、`--run` と `--native` (LLVM) の両方で同じ値になること。
    let cases: &[(&str, &str, &str, &str)] = &[
        ("t.fill", "0", "u32", "0"),
        ("t.fill", "100", "u32", "100014950"),
        ("t.fill", "2000", "u32", "2005999000"),
        ("t.fill", "10000", "u32", "1560060408"),
        ("t.len_after", "0", "usize", "0"),
        ("t.len_after", "50", "usize", "7"),
        ("t.overwrite", "20", "u32", "260"),
        ("t.order", "0", "u32", "0"),
        ("t.order", "30", "u32", "1121038567"),
        ("t.order", "1000", "u32", "2683877213"),
        ("t.missing", "0", "u32", "10011"),
        ("t.missing", "10", "u32", "11011"),
        ("t.churn", "3", "u32", "9"),
        ("t.churn", "100", "u32", "10987026"),
        ("t.churn", "3000", "u32", "2183374384"),
        ("t.signed", "20", "i32", "190"),
        ("t.strs", "1", "u32", "21120"),
        ("t.moved", "0", "u32", "0"),
        ("t.moved", "7", "u32", "63"),
        ("t.moved", "100", "u32", "2340"),
    ];
    for (i, (f, arg, ty, want)) in cases.iter().enumerate() {
        let call = format!("{}({}{})", f, arg, if *f == "t.len_after" { "u32" } else { ty_arg(f) });
        let t = kir_map_pkg(&format!("kir_map_values_{}", i), &format!("    core.check.eq({}, {}{})\n", call, want, ty));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let k = kir_run(&["kir", f, "--run", arg], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_map_get_mut_in_a_match_lowers_and_writes_through() {
    // 旧: `HashMap::get_mut は段階 1 の対象外` (型ごとの下ろし)。段 2 (Kasane 版の `HashMap`) で `get_mut` は下りる。
    // `match m.get_mut(k)` の `Some(v)` が指す番地に書いた値が、マップに残ること (木走査と同じ) を確かめる。
    let t = Tmp::new("kir_map_get_mut", "fn t.bump(n: u32) -> u32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    match m.insert(1u32, n) {
        Ok(_old) => 0u8
        Err(_e) => core.panic.panic(\"oom\")
    }
    match m.get_mut(&1u32) {
        Some(v) => {
            *v = n.wrapping_add(1u32)
        }
        None => core.panic.panic(\"none\")
    }
    match m.get(&1u32) {
        Some(v) => *v
        None => 0u32
    }
end t.bump

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.bump(3u32), 4u32)
    Ok(())
end t.main
");
    let o = run(&["run", "--engine=tree"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let k = run(&["kir", "t.bump", "--run", "3"], &t.0);
    assert_eq!(k.status.code(), Some(0), "{}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "4");
}

/// KIR の未対応の理由は、ソースの行番号を出す (`nline` は行と列を詰めた値なので、KIR は行だけを取り出す)。
#[test]
fn kir_unsupported_reasons_show_the_source_line() {
    let src = "fn t.f(v: &[f64]) -> f64 uses [] panics may\n    let a = 1u32\n    core.mem.drop(a)\n    v[0usize].sqrt()\nend t.f\n\nfn t.main() -> () uses [] panics never\nend t.main\n";
    let t = Tmp::new("kir_reason_line", src);
    let o = run(&["kir", "t.f"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("行 4:"), "{}", text(&o.stderr));
}

#[test]
fn kir_review_field_move_in_one_branch_survives_the_join() {
    // 片方の分岐の中でフィールドを移すと、合流で移動が失われて二重解放になっていた
    // (`assigned` が、フィールドからの移動で書き換わる局所を数えなかった)。`if` と `match`。(`while` の中の移動は E3002 で拒否される。)
    let fns = "type t.B\n    items: alloc.vec.Vec<u32, std.alloc.System>\n    n: u32\nend t.B\n\n\
fn t.mk() -> t.B uses [alloc] panics may\n    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    core.mem.drop(v.push(7u32))\n    t.B { items: v, n: 1u32 }\nend t.mk\n\n\
fn t.f(c: bool) -> usize uses [alloc] panics may\n    let b = t.mk()\n    let mut r: usize = 0usize\n    if c {\n        let w = b.items\n        r = w.len()\n    }\n    r\nend t.f\n\n\
fn t.g(k: u32) -> usize uses [alloc] panics may\n    let b = t.mk()\n    let mut r: usize = 0usize\n    match k {\n        0u32 => {\n            let w = b.items\n            r = w.len()\n        }\n        _ => {\n            r = 5usize\n        }\n    }\n    r\nend t.g\n";
    kir_review_cases("fm_if", fns, "t.f", &[(&["true"], "t.f(true)", "1usize", "1"), (&["false"], "t.f(false)", "0usize", "0")]);
    kir_review_cases("fm_match", fns, "t.g", &[(&["0"], "t.g(0u32)", "1usize", "1"), (&["3"], "t.g(3u32)", "5usize", "5")]);
}

#[test]
fn kir_review_early_exit_frees_evaluated_owned_temporaries() {
    // `?` / `return` / `break` で抜けるとき、評価済みで未束縛の所有値 (引数・構造体のフィールド・タプルの要素) も解放する。
    let fns = "type t.P\n    a: alloc.vec.Vec<u32, std.alloc.System>\n    n: u32\nend t.P\n\n\
fn t.g(c: bool) -> Option<u32> uses [] panics never\n    if c {\n        return Some(1u32)\n    }\n    None\nend t.g\n\n\
fn t.mkv() -> alloc.vec.Vec<u32, std.alloc.System> uses [alloc] panics may\n    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    core.mem.drop(v.push(7u32))\n    v\nend t.mkv\n\n\
fn t.h(v: alloc.vec.Vec<u32, std.alloc.System>, x: u32) -> usize uses [] panics never\n    core.mem.drop(x)\n    v.len()\nend t.h\n\n\
fn t.f(c: bool) -> Option<usize> uses [alloc] panics may\n    Some(t.h(t.mkv(), t.g(c)?))\nend t.f\n\n\
fn t.s(c: bool) -> Option<u32> uses [alloc] panics may\n    let p = t.P { a: t.mkv(), n: t.g(c)? }\n    Some(p.n)\nend t.s\n\n\
fn t.tup(c: bool) -> Option<u32> uses [alloc] panics may\n    let (a, b) = (t.mkv(), t.g(c)?)\n    core.mem.drop(a)\n    Some(b)\nend t.tup\n\n\n\
fn t.ret(c: bool) -> usize uses [alloc] panics may\n    let n = t.h(t.mkv(), if c {\n        return 5usize\n    } else {\n        1u32\n    })\n    n + 10usize\nend t.ret\n\n\
fn t.brk(k: u32) -> usize uses [alloc] panics may\n    let mut total = 0usize\n    let mut i = 0u32\n    while i < 4u32 {\n        let n = t.h(t.mkv(), if i == k {\n            break\n        } else {\n            1u32\n        })\n        total = total + n\n        i = i + 1u32\n    }\n    total\nend t.brk\n";
    kir_review_cases("early_arg", fns, "t.f", &[(&["true"], "t.f(true)", "Some(1usize)", "Some(1)"), (&["false"], "t.f(false)", "None", "None")]);
    kir_review_cases("early_field", fns, "t.s", &[(&["true"], "t.s(true)", "Some(1u32)", "Some(1)"), (&["false"], "t.s(false)", "None", "None")]);
    kir_review_cases("early_ret", fns, "t.ret", &[(&["true"], "t.ret(true)", "5usize", "5"), (&["false"], "t.ret(false)", "11usize", "11")]);
    kir_review_cases("early_break", fns, "t.brk", &[(&["2"], "t.brk(2u32)", "2usize", "2"), (&["9"], "t.brk(9u32)", "4usize", "4")]);
    kir_review_cases("early_tuple", fns, "t.tup", &[(&["true"], "t.tup(true)", "Some(1u32)", "Some(1)"), (&["false"], "t.tup(false)", "None", "None")]);
}

#[test]
fn kir_review_from_str_radix_agrees_with_tree_walker() {
    // `core.num.<int>.from_str_radix` の KIR 下ろし (Op::ParseInt) が、木走査と Ok の値と
    // Err の kind を一致させる (符号・空文字列・範囲外・桁の誤り・radix 範囲)。
    let fns = [
        "fn t.i64(case: u32) -> i64 uses [] panics never\n    let r = match case {\n        0u32 => i64.from_str_radix(\"-9223372036854775808\", 10u32)\n        1u32 => i64.from_str_radix(\"9223372036854775808\", 10u32)\n        _ => i64.from_str_radix(\"0\", 10u32)\n    }\n    match r {\n        Ok(n) => n\n        Err(e) => i64.from_u8(e.kind).wrapping_add(1000)\n    }\nend t.i64",
        "fn t.i8(case: u32) -> i32 uses [] panics never\n    let r = match case {\n        0u32 => i8.from_str_radix(\"-128\", 10u32)\n        1u32 => i8.from_str_radix(\"128\", 10u32)\n        _ => i8.from_str_radix(\"0\", 10u32)\n    }\n    match r {\n        Ok(n) => i32.from_i8(n)\n        Err(e) => i32.from_u8(e.kind).wrapping_add(1000)\n    }\nend t.i8",
        "fn t.u64(case: u32) -> u64 uses [] panics never\n    let r = match case {\n        0u32 => u64.from_str_radix(\"18446744073709551615\", 10u32)\n        1u32 => u64.from_str_radix(\"18446744073709551616\", 10u32)\n        _ => u64.from_str_radix(\"0\", 10u32)\n    }\n    match r {\n        Ok(n) => n\n        Err(e) => u64.from_u8(e.kind).wrapping_add(1000u64)\n    }\nend t.u64",
        "fn t.u8(case: u32) -> u32 uses [] panics never\n    let r = match case {\n        0u32 => u8.from_str_radix(\"\", 10u32)\n        1u32 => u8.from_str_radix(\"0\", 10u32)\n        2u32 => u8.from_str_radix(\"+5\", 10u32)\n        3u32 => u8.from_str_radix(\"-5\", 10u32)\n        4u32 => u8.from_str_radix(\"-\", 10u32)\n        5u32 => u8.from_str_radix(\"ff\", 16u32)\n        6u32 => u8.from_str_radix(\"ff\", 10u32)\n        7u32 => u8.from_str_radix(\"FF\", 16u32)\n        8u32 => u8.from_str_radix(\"z\", 36u32)\n        9u32 => u8.from_str_radix(\"255\", 10u32)\n        10u32 => u8.from_str_radix(\"256\", 10u32)\n        11u32 => u8.from_str_radix(\" 1\", 10u32)\n        12u32 => u8.from_str_radix(\"1_0\", 10u32)\n        _ => u8.from_str_radix(\"\", 10u32)\n    }\n    match r {\n        Ok(n) => u32.from_u8(n)\n        Err(e) => 1000u32.wrapping_add(u32.from_u8(e.kind))\n    }\nend t.u8",
    ].join("\n\n");
    kir_review_cases("pi_u8", &fns, "t.u8", &[
        (&["0"], "t.u8(0u32)", "1000u32", "1000"), // 空文字列 → kind 0
        (&["1"], "t.u8(1u32)", "0u32", "0"),      // "0" → Ok(0)
        (&["2"], "t.u8(2u32)", "5u32", "5"),      // "+5" → Ok(5)
        (&["3"], "t.u8(3u32)", "1000u32", "1000"),// 符号なしに "-" → kind 0
        (&["4"], "t.u8(4u32)", "1000u32", "1000"),// 符号だけ → kind 0
        (&["5"], "t.u8(5u32)", "255u32", "255"),  // "ff" radix 16 → Ok(255)
        (&["6"], "t.u8(6u32)", "1000u32", "1000"),// "ff" radix 10 → kind 0
        (&["7"], "t.u8(7u32)", "255u32", "255"),  // "FF" radix 16 → Ok(255)
        (&["8"], "t.u8(8u32)", "35u32", "35"),    // "z" radix 36 → Ok(35)
        (&["9"], "t.u8(9u32)", "255u32", "255"),  // u8 "255" → Ok(255)
        (&["10"], "t.u8(10u32)", "1001u32", "1001"), // u8 "256" → kind 1 (範囲外)
        (&["11"], "t.u8(11u32)", "1000u32", "1000"),// " 1" → kind 0
        (&["12"], "t.u8(12u32)", "1000u32", "1000"),// "1_0" → kind 0
    ]);
    kir_review_cases("pi_i8", &fns, "t.i8", &[
        (&["0"], "t.i8(0u32)", "-128i32", "-128"),  // i8 "-128" → Ok(-128)
        (&["1"], "t.i8(1u32)", "1001i32", "1001"),  // i8 "128" → kind 1 (範囲外)
    ]);
    kir_review_cases("pi_u64", &fns, "t.u64", &[
        (&["0"], "t.u64(0u32)", "18446744073709551615u64", "18446744073709551615"), // u64 max → Ok
        (&["1"], "t.u64(1u32)", "1001u64", "1001"), // u64 max+1 → kind 1
    ]);
    kir_review_cases("pi_i64", &fns, "t.i64", &[
        (&["0"], "t.i64(0u32)", "-9223372036854775808i64", "-9223372036854775808"), // i64 min → Ok
        (&["1"], "t.i64(1u32)", "1001i64", "1001"), // i64 "2^63" → kind 1
    ]);
}

#[test]
fn kir_review_user_iterator_advance_crosses_outer_joins() {
    // 利用者の反復子の `for x in it` が進めたスロットは、外側の `while` / `if` / `match` の合流にも渡る
    // (前は `scan_mut` が `For` を見ず、外側の合流が古い `it` を使い続けた)。
    let fns = format!(
        "{}\nfn t.outer(rounds: u32) -> u32 uses [] panics may\n    let mut it = t.Upto {{ cur: 0u32, stop: 100u32 }}\n    let mut acc = 0u32\n    let mut r = 0u32\n    while r < rounds {{\n        for x in it {{\n            acc = acc.wrapping_add(x)\n            if x % 4u32 == 3u32 {{\n                break\n            }}\n        }}\n        r = r.wrapping_add(1u32)\n    }}\n    acc\nend t.outer\n\n\
fn t.cond(c: bool) -> u32 uses [] panics never\n    let mut it = t.Upto {{ cur: 0u32, stop: 6u32 }}\n    let mut acc = 0u32\n    if c {{\n        for x in it {{\n            if x == 2u32 {{\n                break\n            }}\n            acc = acc.wrapping_add(x)\n        }}\n    }}\n    for y in it {{\n        acc = acc.wrapping_add(y.wrapping_mul(10u32))\n    }}\n    acc\nend t.cond\n\n\
fn t.sel(k: u32) -> u32 uses [] panics never\n    let mut it = t.Upto {{ cur: 0u32, stop: 6u32 }}\n    let mut acc = 0u32\n    match k {{\n        0u32 => {{\n            for x in it {{\n                if x == 1u32 {{\n                    break\n                }}\n                acc = acc.wrapping_add(1u32)\n            }}\n        }}\n        _ => {{\n            acc = acc.wrapping_add(0u32)\n        }}\n    }}\n    for y in it {{\n        acc = acc.wrapping_add(y)\n    }}\n    acc\nend t.sel\n",
        USER_ITER_SRC.split("fn t.sum(").next().unwrap()
    );
    kir_review_cases("uiter_outer", &fns, "t.outer", &[(&["2"], "t.outer(2u32)", "28u32", "28"), (&["3"], "t.outer(3u32)", "66u32", "66")]);
    kir_review_cases("uiter_cond", &fns, "t.cond", &[(&["true"], "t.cond(true)", "121u32", "121"), (&["false"], "t.cond(false)", "150u32", "150")]);
    kir_review_cases("uiter_sel", &fns, "t.sel", &[(&["0"], "t.sel(0u32)", "15u32", "15"), (&["1"], "t.sel(1u32)", "15u32", "15")]);
}

#[test]
fn kir_review_user_iterator_owned_items_are_freed() {
    // 利用者の反復子が所有物 (Vec) を返すとき、`for` の変数が所有して 1 周ごとに解放する (前は漏れた)。`for _ in it` も解放する。
    let fns = "type t.Gen pub\n    cur: u32\n    stop: u32\nend t.Gen\n\n\
impl core.iter.Iterator<alloc.vec.Vec<u32, std.alloc.System>, [alloc], may> for t.Gen\n\
fn t.Gen.next(self: &mut t.Gen) -> Option<alloc.vec.Vec<u32, std.alloc.System>> uses [alloc] panics may impls core.iter.Iterator<alloc.vec.Vec<u32, std.alloc.System>, [alloc], may>\n    if self.cur >= self.stop {\n        return None\n    }\n    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    core.mem.drop(v.push(self.cur))\n    self.cur = self.cur.wrapping_add(1u32)\n    Some(v)\nend t.Gen.next\n\n\
fn t.run(n: u32) -> usize uses [alloc] panics may\n    let mut g = t.Gen { cur: 0u32, stop: n }\n    let mut total = 0usize\n    for v in g {\n        total = total + v.len()\n    }\n    total\nend t.run\n\n\
fn t.run_wild(n: u32) -> usize uses [alloc] panics may\n    let mut g = t.Gen { cur: 0u32, stop: n }\n    let mut total = 0usize\n    for _ in g {\n        total = total + 1usize\n    }\n    total\nend t.run_wild\n";
    kir_review_cases("uiter_own", fns, "t.run", &[(&["4"], "t.run(4u32)", "4usize", "4"), (&["0"], "t.run(0u32)", "0usize", "0")]);
    kir_review_cases("uiter_wild", fns, "t.run_wild", &[(&["3"], "t.run_wild(3u32)", "3usize", "3")]);
}

#[test]
fn kir_review_nan_bits_follow_one_rule() {
    // NaN になる演算 (`+ - * / %`・`sqrt`) の結果のビット列: 第 1 被演算子が NaN ならそれ (quiet 化)、第 2 が NaN ならそれ、
    // どちらでもなければ正の quiet NaN。実機の既定の NaN (符号ビット付き) と LLVM の定数畳み込み (正) の違いを、
    // 木走査・参照実行器・機械語 (最適化 -O1 と ASan の -O0) の 3 者で揃える (設計_KIR.md 17.8)。
    let fns = "fn t.d(b: u64) -> f64 uses [] panics never\n    core.num.f64.from_bits(b)\nend t.d\n\n\
fn t.s(b: u32) -> f32 uses [] panics never\n    core.num.f32.from_bits(b)\nend t.s\n\n\
fn t.sub64(a: f64, b: f64) -> u64 uses [] panics never\n    (a - b).to_bits()\nend t.sub64\n\n\
fn t.mul64(a: f64, b: f64) -> u64 uses [] panics never\n    (a * b).to_bits()\nend t.mul64\n\n\
fn t.div64(a: f64, b: f64) -> u64 uses [] panics never\n    (a / b).to_bits()\nend t.div64\n\n\
fn t.rem64(a: f64, b: f64) -> u64 uses [] panics never\n    a.rem(b).to_bits()\nend t.rem64\n\n\
fn t.sqrt64(a: f64) -> u64 uses [] panics never\n    a.sqrt().to_bits()\nend t.sqrt64\n\n\
fn t.add32(a: f32, b: f32) -> u32 uses [] panics never\n    (a + b).to_bits()\nend t.add32\n\n\
fn t.div32(a: f32, b: f32) -> u32 uses [] panics never\n    (a / b).to_bits()\nend t.div32\n";
    const INF: &str = "9218868437227405312";
    const CANON: &str = "9221120237041090560";
    const SNAN: &str = "9218868437227405313";
    const QSNAN: &str = "9221120237041090561";
    const ONE: &str = "4607182418800017408";
    let c64 = |f: &'static str, a: &'static str, b: &'static str, want: &'static str| -> (&'static str, Vec<&'static str>, String, String, String) {
        (f, vec![a, b], format!("{}(t.d({}u64), t.d({}u64))", f, a, b), format!("{}u64", want), want.to_string())
    };
    let mut cases = vec![
        c64("t.sub64", INF, INF, CANON),
        c64("t.sub64", SNAN, ONE, QSNAN),
        c64("t.sub64", ONE, SNAN, QSNAN),
        c64("t.mul64", "0", INF, CANON),
        c64("t.div64", INF, INF, CANON),
        c64("t.div64", "0", "0", CANON),
        c64("t.div64", SNAN, "0", QSNAN),
        c64("t.rem64", ONE, "0", CANON),
        c64("t.rem64", INF, ONE, CANON),
    ];
    cases.push(("t.sqrt64", vec!["13836000000000000000"], "t.sqrt64(t.d(13836000000000000000u64))".into(), format!("{}u64", CANON), CANON.to_string()));
    cases.push(("t.add32", vec!["2139095040", "4286578688"], "t.add32(t.s(2139095040u32), t.s(4286578688u32))".into(), "2143289344u32".into(), "2143289344".into()));
    cases.push(("t.div32", vec!["0", "0"], "t.div32(t.s(0u32), t.s(0u32))".into(), "2143289344u32".into(), "2143289344".into()));
    cases.push(("t.add32", vec!["2139095041", "1065353216"], "t.add32(t.s(2139095041u32), t.s(1065353216u32))".into(), "2143289345u32".into(), "2143289345".into()));
    for (i, (f, args, call, lit, want)) in cases.iter().enumerate() {
        kir_review_cases(&format!("nan{}", i), &fns, f, &[(&args[..], call.as_str(), lit.as_str(), want.as_str())]);
    }
}

#[test]
fn kir_review_rejects_borrow_shapes_that_read_freed_buffers() {
    // `ksn check` が通す、借用の中身の丸読み (`*r`、所有物を持つ型) と、反復中の Vec の変更 (`for x in v` の本体で `v.push`) は、
    // 二重解放・解放後の読みになるので、KIR の下ろしは理由を付けて拒む (設計_KIR.md 17.5 / 17.6)。
    let cases: &[(&str, &str)] = &[
        (
            "type t.B\n    items: alloc.vec.Vec<u32, std.alloc.System>\n    n: u32\nend t.B\n\nfn t.f(r: &t.B) -> usize uses [alloc] panics may\n    let b = *r\n    b.items.len()\nend t.f\n",
            "借用の中身",
        ),
        (
            "fn t.f(r: &alloc.vec.Vec<u32, std.alloc.System>) -> usize uses [alloc] panics may\n    let b = *r\n    b.len()\nend t.f\n",
            "* の被演算子が参照でない",
        ),
        (
            "fn t.f() -> usize uses [alloc] panics may\n    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    core.mem.drop(v.push(1u32))\n    let mut n = 0usize\n    for x in v {\n        if n < 3usize {\n            core.mem.drop(v.push(x))\n        }\n        n = n + 1usize\n    }\n    v.len()\nend t.f\n",
            "反復中の Vec を変更",
        ),
        (
            "fn t.f() -> usize uses [alloc] panics may\n    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)\n    let mut k: alloc.string.String = alloc.string.String.new(std.alloc.SYSTEM)\n    core.mem.drop(k.push_str(\"abc\"))\n    core.mem.drop(m.insert(k.as_str(), 1u32))\n    core.mem.drop(k.push_str(\"defghijklmnopqrstuvwxyzabcdefghijklmnopq\"))\n    m.len()\nend t.f\n",
            "元の String が関数の中で変更される",
        ),
    ];
    for (i, (src, why)) in cases.iter().enumerate() {
        let src = format!("{}\nfn t.main() -> Result<(), std.io.Error> uses [] panics may\n    Ok(())\nend t.main\n", src);
        let t = Tmp::new(&format!("kir_review_reject_{}", i), &src);
        // 検査が先に拒むようになった形 (`*r` の move は E3002、借用中の変更は E3010) は、それで健全性が保たれる。
        // 検査を通る形だけ、KIR の下ろしが理由を付けて拒むことを確かめる。
        let c = run(&["check", "--json"], &t.0);
        if c.status.code() != Some(0) {
            let got = codes(&text(&c.stdout));
            assert!(got.iter().any(|g| g == "E3002" || g == "E3010"), "{}: {:?}", i, got);
            continue;
        }
        let o = run(&["kir", "t.f"], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
        assert!(text(&o.stderr).contains(why), "{}", text(&o.stderr));
    }
}

#[test]
fn kir_review_for_over_a_vec_may_still_read_it() {
    // 本体で反復中の Vec を読むだけ (`len` など) なら、これまでどおり下ろせる。`for x in v` は `v` を消費する
    // (fix/linear-generic) ので、借用のスライスを回す。
    let fns = "fn t.sum() -> usize uses [alloc] panics may\n    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    core.mem.drop(v.push(1u32))\n    core.mem.drop(v.push(2u32))\n    let mut n = 0usize\n    let s = v.as_slice()\n    for x in s {\n        n = n + v.len()\n        core.mem.drop(x)\n    }\n    n\nend t.sum\n";
    kir_review_cases("for_read", fns, "t.sum", &[(&[], "t.sum()", "4usize", "4")]);
}

/// Fable の KIR レビュー 2 回目: 局所の `Option<Vec>` の `unwrap` は局所を移す (取り出した値とスコープの終わりの解放が
/// 重なって二重解放になっていた)。値の受け手を移した後、引数の `?` で抜けても受け手は漏れない。
#[test]
fn kir_review2_unwrap_moves_and_receiver_survives_early_exit() {
    let fns = "fn t.mkv(n: u32) -> alloc.vec.Vec<u32, std.alloc.System> uses [alloc] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(i))
        i = i + 1u32
    }
    v
end t.mkv

fn t.opt(c: bool) -> Option<alloc.vec.Vec<u32, std.alloc.System>> uses [alloc] panics may
    if c {
        return Some(t.mkv(2u32))
    }
    None
end t.opt

fn t.n7(c: bool) -> usize uses [alloc] panics may
    let o = t.opt(c)
    let v = o.unwrap()
    v.len()
end t.n7

type t.B
    items: alloc.vec.Vec<u32, std.alloc.System>
end t.B

fn t.B.total(self: t.B, x: u32) -> usize uses [] panics never
    core.mem.drop(x)
    self.items.len()
end t.B.total

fn t.g(c: bool) -> Option<u32> uses [] panics never
    if c {
        return Some(1u32)
    }
    None
end t.g

fn t.e1(c: bool) -> Option<usize> uses [alloc] panics may
    let b = t.B { items: t.mkv(3u32) }
    Some(b.total(t.g(c)?))
end t.e1
";
    kir_review_cases("r2_unwrap", fns, "t.n7", &[(&["true"], "t.n7(true)", "2usize", "2")]);
    kir_review_cases("r2_recv_exit", fns, "t.e1", &[(&["true"], "t.e1(true)", "Some(3usize)", "Some(3)"), (&["false"], "t.e1(false)", "None", "None")]);
}

#[test]
fn kir_net_roundtrip() {
    // ポート 0 で bind し、実際のポートを得て connect し、同一スレッド内で数バイトをやり取りする。
    // listen の後なら accept の前でも connect は成功する。
    let fns = "fn t.net_roundtrip() -> () uses [alloc, io.net] panics may\n    match std.net.listen(\"127.0.0.1:0\") {\n        Ok(l) => {\n            let port = match l.local_port() {\n                Ok(p) => p\n                Err(_e) => core.panic.panic(\"local_port failed\")\n            }\n            core.check.eq((port == 0u16), false)\n            let mut address = alloc.string.String.new(std.alloc.SYSTEM)\n            match address.push_str(\"127.0.0.1:\") {\n                Ok(_u) => ()\n                Err(_e) => core.panic.panic(\"push_str failed\")\n            }\n            match address.push_u64(u64.from_u16(port)) {\n                Ok(_u) => ()\n                Err(_e) => core.panic.panic(\"push_u64 failed\")\n            }\n            let a = address.as_str()\n            match std.net.connect(a) {\n                Ok(c) => {\n                    let mut s = c\n                    match s.write(\"hello\".as_bytes()) {\n                        Ok(_n) => ()\n                        Err(_e) => core.panic.panic(\"write failed\")\n                    }\n                    match s.close() {\n                        Ok(_u) => ()\n                        Err(_e) => core.panic.panic(\"close failed\")\n                    }\n                }\n                Err(_e) => core.panic.panic(\"connect failed\")\n            }\n            match l.accept() {\n                Ok(accepted) => {\n                    let mut s2 = accepted\n                    match s2.write(\"hi\".as_bytes()) {\n                        Ok(_n) => ()\n                        Err(_e) => core.panic.panic(\"server write failed\")\n                    }\n                    match s2.close() {\n                        Ok(_u) => ()\n                        Err(_e) => core.panic.panic(\"close2 failed\")\n                    }\n                }\n                Err(_e) => core.panic.panic(\"accept failed\")\n            }\n            match l.close() {\n                Ok(_u) => ()\n                Err(_e) => core.panic.panic(\"close3 failed\")\n            }\n        }\n        Err(_e) => core.panic.panic(\"listen failed\")\n    }\nend t.net_roundtrip\n\nfn t.main() -> () uses [alloc, io.net] panics may\n    t.net_roundtrip()\nend t.main\n";
    let t = Tmp::new("kir_net_roundtrip", fns);
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(0), "tree-walker: {}\n{}", text(&tw.stderr), text(&tw.stdout));
    let k = kir_run(&["kir", "t.net_roundtrip", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", text(&k.stderr), text(&k.stdout));
}

/// ソケットを `&TcpListener` / `&mut TcpStream` で渡す (設計_KIR.md 12 章)。`[0u8; 8]` の配列を `&mut [u8]` で読ませ、
/// 読んだ長さと先頭バイトを返す。`?` の失敗の型は `std.net.Error` (listen / accept / local_port / close)。木走査と一致する値は 5104 (5 バイト、先頭 'h' = 104)。
#[test]
fn kir_net_refs_pass_sockets_by_reference() {
    let fns = r#"fn t.port_of(l: &std.net.TcpListener) -> Result<u16, std.net.Error> uses [io.net] panics never
    l.local_port()
end t.port_of

fn t.send(s: &mut std.net.TcpStream, data: &[u8]) -> Result<usize, std.io.Error> uses [io.net] panics never
    s.write(data)
end t.send

fn t.recv(s: &mut std.net.TcpStream, buf: &mut [u8]) -> Result<usize, std.io.Error> uses [io.net] panics never
    s.read(buf)
end t.recv

fn t.first_byte(buf: &[u8]) -> usize uses [] panics never
    match buf.get(0usize) {
        Some(b) => usize.from_u8(*b)
        None => 0usize
    }
end t.first_byte

fn t.exchange() -> usize uses [alloc, io.net] panics may
    let mut address = alloc.string.String.new(std.alloc.SYSTEM)
    let listener = match std.net.listen("127.0.0.1:0") {
        Ok(l) => l
        Err(_e) => core.panic.panic("listen")
    }
    let port = match t.port_of(&listener) {
        Ok(p) => p
        Err(_e) => core.panic.panic("port")
    }
    match address.push_str("127.0.0.1:") {
        Ok(_u) => ()
        Err(_e) => core.panic.panic("push")
    }
    match address.push_u64(u64.from_u16(port)) {
        Ok(_u) => ()
        Err(_e) => core.panic.panic("push_u64")
    }
    let mut client = match std.net.connect(address.as_str()) {
        Ok(c) => c
        Err(_e) => core.panic.panic("connect")
    }
    match t.send(&mut client, "hello".as_bytes()) {
        Ok(_n) => ()
        Err(_e) => core.panic.panic("send")
    }
    match client.close() {
        Ok(_u) => ()
        Err(_e) => core.panic.panic("close client")
    }
    let mut server = match listener.accept() {
        Ok(s) => s
        Err(_e) => core.panic.panic("accept")
    }
    let mut chunk = [0u8; 8]
    let n = match t.recv(&mut server, &mut chunk) {
        Ok(k) => k
        Err(_e) => core.panic.panic("recv")
    }
    let first = t.first_byte(&chunk)
    match server.close() {
        Ok(_u) => ()
        Err(_e) => core.panic.panic("close server")
    }
    match listener.close() {
        Ok(_u) => ()
        Err(_e) => core.panic.panic("close listener")
    }
    n.wrapping_mul(1000usize).wrapping_add(first)
end t.exchange

fn t.bad_listen() -> Result<usize, std.net.Error> uses [io.net] panics never
    let l = std.net.listen("not an address")?
    match l.close() {
        Ok(_u) => Ok(1usize)
        Err(e) => Err(e)
    }
end t.bad_listen
"#;
    let main = "fn t.main() -> () uses [alloc, io.net] panics may\n    core.check.eq(t.exchange(), 5104usize)\n    match t.bad_listen() {\n        Ok(_n) => core.panic.panic(\"listen should fail\")\n        Err(_e) => ()\n    }\nend t.main\n";
    let t = Tmp::new("kir_net_refs", &format!("{}\n{}", fns, main));
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(0), "tree-walker: {}\n{}", text(&tw.stderr), text(&tw.stdout));
    let k = kir_run(&["kir", "t.exchange", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", text(&k.stderr), text(&k.stdout));
    assert_eq!(text(&k.stdout).trim(), "5104");
    let m = kir_run(&["kir", "t.main", "--run"], &t.0);
    assert_eq!(m.status.code(), Some(0), "kir main: {}\n{}", text(&m.stderr), text(&m.stdout));
}

/// `[x; N]` (`ArrayRep`、長さはリテラル、要素はスカラー) の値・ループ・`get`。長さ 0 の配列も含む。
#[test]
fn kir_array_rep_matches_tree_walker() {
    let fns = r#"fn t.count_true() -> usize uses [] panics never
    let mut bs = [false; 4]
    match bs.get(2usize) {
        Some(b) => core.mem.drop(*b)
        None => core.mem.drop(false)
    }
    let mut n = 0usize
    for b in bs {
        if !b {
            n = n.wrapping_add(1usize)
        }
    }
    n
end t.count_true

fn t.fill_len(buf: &mut [u32]) -> usize uses [] panics never
    buf.len()
end t.fill_len

fn t.sum_arr() -> u32 uses [] panics never
    let a = [3u32; 5]
    let mut s = 0u32
    for x in a {
        s = s.wrapping_add(x)
    }
    let b = [1u32; 0]
    for y in b {
        s = s.wrapping_add(y)
    }
    s
end t.sum_arr
"#;
    let main = "fn t.main() -> () uses [] panics may\n    core.check.eq(t.sum_arr(), 15u32)\n    core.check.eq(t.count_true(), 4usize)\nend t.main\n";
    let t = Tmp::new("kir_array_rep", &format!("{}\n{}", fns, main));
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(0), "tree-walker: {}\n{}", text(&tw.stderr), text(&tw.stdout));
    for (f, want) in [("t.sum_arr", "15"), ("t.count_true", "4")] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}: {}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
}

/// `--opt` のインライン化は `extern` (本体が仮の `unreachable`) を展開しない。
#[test]
fn kir_opt_does_not_inline_extern() {
    let src = "extern t.abs(x: i32) -> i32 link \"abs\"\n\nfn t.abs_extern(x: i32) -> i32 uses [] panics never\n    unsafe \"abs\" \"libc abs\" {\n        t.abs(x)\n    }\nend t.abs_extern\n\nfn t.main() -> () uses [] panics may\n    core.check.eq(t.abs_extern(-7i32), 7i32)\nend t.main\n";
    let t = Tmp::new("kir_opt_extern", src);
    fs::write(
        t.0.join("kasane.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n\n[unsafe]\nmodules = [\"t\"]\n",
    )
    .unwrap();
    let k = kir_run(&["kir", "t.abs_extern", "--opt", "--run", "-7"], &t.0);
    assert_eq!(k.status.code(), Some(0), "{}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "7");
}

/// `?` のエラー変換 (仕様 4.1): `Result<T, E1>` の `?` を `Result<U, E2>` で使い、
/// E2 が `derive [From]` で E1 だけをペイロードに持つ変種を 1 つ持つ enum なら、
/// エラーを `E2.V(err)` に包む。E1 は `i32` / `std.io.Error` /
/// `alloc.string.String` (所有物を持つ)、成功側の型が同じ形と違う形の両方。
/// `kir_run` は木走査と ASan 付き機械語でも走らせる (漏れ・二重解放の検出)。
#[test]
fn kir_convert_error_via_from() {
    let fns = "enum t.Err32 derive [Debug, Clone, Copy, PartialEq, From] pub\n    X(u32)\nend t.Err32\n\n\
enum t.ErrIo derive [Debug, Clone, Copy, PartialEq, From] pub\n    Io(std.io.Error)\nend t.ErrIo\n\n\
enum t.ErrStr derive [From] pub\n    S(alloc.string.String<std.alloc.System>)\nend t.ErrStr\n\n\
fn t.dst_io(c: bool) -> Result<(), t.ErrIo> uses [] panics never\n    t.src_io(c)?\n    Ok(())\nend t.dst_io\n\n\
fn t.dst_str(c: bool) -> Result<(), t.ErrStr> uses [alloc] panics never\n    t.src_str(c)?\n    Ok(())\nend t.dst_str\n\n\
fn t.dst_u32(c: bool) -> Result<u32, t.Err32> uses [] panics never\n    let n = t.src_u32(c)?\n    Ok(n)\nend t.dst_u32\n\n\
fn t.dst_u64(c: bool) -> Result<u64, t.Err32> uses [] panics never\n    t.src_unit(c)?\n    Ok(1007u64)\nend t.dst_u64\n\n\
fn t.probe_io(c: bool) -> u32 uses [] panics never\n    match t.dst_io(c) {\n        Ok(()) => 1000u32\n        Err(t.ErrIo.Io(_)) => 42u32\n        Err(_) => 9999u32\n    }\nend t.probe_io\n\n\
fn t.probe_str(c: bool) -> u32 uses [alloc] panics never\n    match t.dst_str(c) {\n        Ok(()) => 1000u32\n        Err(t.ErrStr.S(_)) => 42u32\n        Err(_) => 9999u32\n    }\nend t.probe_str\n\n\
fn t.probe_u32(c: bool) -> u32 uses [] panics never\n    match t.dst_u32(c) {\n        Ok(n) => n\n        Err(t.Err32.X(e)) => e\n        Err(_) => 9999u32\n    }\nend t.probe_u32\n\n\
fn t.probe_u64(c: bool) -> u64 uses [] panics never\n    match t.dst_u64(c) {\n        Ok(n) => n\n        Err(t.Err32.X(_)) => 42u64\n        Err(_) => 9999u64\n    }\nend t.probe_u64\n\n\
fn t.src_io(c: bool) -> Result<(), std.io.Error> uses [] panics never\n    if c {\n        return Ok(())\n    }\n    Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 0i32 })\nend t.src_io\n\n\
fn t.src_str(c: bool) -> Result<(), alloc.string.String<std.alloc.System>> uses [alloc] panics never\n    if c {\n        return Ok(())\n    }\n    Err(alloc.string.String.new(std.alloc.SYSTEM))\nend t.src_str\n\n\
fn t.src_u32(c: bool) -> Result<u32, u32> uses [] panics never\n    if c {\n        return Ok(1007u32)\n    }\n    Err(42u32)\nend t.src_u32\n\n\
fn t.src_unit(c: bool) -> Result<(), u32> uses [] panics never\n    if c {\n        return Ok(())\n    }\n    Err(42u32)\nend t.src_unit\n";
    kir_review_cases("conv_u32", fns, "t.probe_u32", &[
        (&["true"], "t.probe_u32(true)", "1007u32", "1007"),
        (&["false"], "t.probe_u32(false)", "42u32", "42"),
    ]);
    kir_review_cases("conv_u64", fns, "t.probe_u64", &[
        (&["true"], "t.probe_u64(true)", "1007u64", "1007"),
        (&["false"], "t.probe_u64(false)", "42u64", "42"),
    ]);
    kir_review_cases("conv_io", fns, "t.probe_io", &[
        (&["true"], "t.probe_io(true)", "1000u32", "1000"),
        (&["false"], "t.probe_io(false)", "42u32", "42"),
    ]);
    kir_review_cases("conv_str", fns, "t.probe_str", &[
        (&["true"], "t.probe_str(true)", "1000u32", "1000"),
        (&["false"], "t.probe_str(false)", "42u32", "42"),
    ]);
}

/// 浮動小数の表示 (kir_rt.c) は木走査の `py_float` / `py_float32` と同じ。指数表記を使わない最短の往復表記、
/// 非正規数、ちょうど中間の値 (Rust は絶対値の大きい方へ丸める)。参照実行器・機械語・ASan で比べる。
#[test]
fn kir_float_display_matches_tree_walker_on_hard_values() {
    let src = "fn t.c64(b: u64) -> () uses [] panics may\n    core.check.eq(f64.from_bits(b), 2.5f64)\nend t.c64\n\n\
               fn t.c32(b: u32) -> () uses [] panics may\n    core.check.eq(f32.from_bits(b), 2.5f32)\nend t.c32\n\n\
               fn t.main() -> () uses [] panics may\nend t.main\n";
    let t = Tmp::new("kir_float_hard", src);
    let v64: [f64; 12] = [1e100, f64::MAX, 5e-324, 2.2250738585072014e-308, 0.1 + 0.2, -0.0, 1e16, 1e-7, 2f64.powi(-25), 1658206780088562.25, f64::NAN, f64::NEG_INFINITY];
    for v in v64 {
        let b = v.to_bits().to_string();
        let k = kir_run(&["kir", "t.c64", "--run", &b], &t.0);
        let want = format!("panic: assertion failed: left: {}, right: 2.5", kasane_lang_py_float(v));
        assert!(text(&k.stderr).starts_with(&want), "{} {}", want, text(&k.stderr));
    }
    for v in [0.1f32, 16777217.0, f32::MAX, 1e-45, 1.5] {
        let b = v.to_bits().to_string();
        let k = kir_run(&["kir", "t.c32", "--run", &b], &t.0);
        let s = if v == v.trunc() && v.abs() < 1e16 { format!("{:.1}", v) } else { format!("{}", v) };
        let want = format!("panic: assertion failed: left: {}, right: 2.5", s);
        assert!(text(&k.stderr).starts_with(&want), "{} {}", want, text(&k.stderr));
    }
}

/// `interp.rs` の `py_float` と同じ規則 (テストの中の写し)。
fn kasane_lang_py_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == f.trunc() && f.abs() < 1e16 {
        return format!("{:.1}", f);
    }
    format!("{}", f)
}

/// `String.push_u64` の KIR の下ろし: 0 は "0" (最下位の桁は必ず書く)、u64 の最大値まで、2 回続けて書く。
#[test]
fn kir_push_u64_matches_tree_walker() {
    let fns = "fn t.f(x: u64) -> usize uses [alloc] panics may\n    let mut s = alloc.string.String.new(std.alloc.SYSTEM)\n    core.mem.drop(s.push_str(\"a\"))\n    core.mem.drop(s.push_u64(x))\n    core.mem.drop(s.push_u64(x))\n    let mut h = 0usize\n    for c in s.as_str().chars() {\n        h = h.wrapping_mul(13usize)\n        if c == '0' {\n            h = h.wrapping_add(1usize)\n        }\n        if c == '9' {\n            h = h.wrapping_add(2usize)\n        }\n        if c == '1' {\n            h = h.wrapping_add(3usize)\n        }\n        if c == '5' {\n            h = h.wrapping_add(4usize)\n        }\n    }\n    h.wrapping_mul(100usize).wrapping_add(s.len())\nend t.f\n\n";
    kir_review_cases("push_u64", fns, "t.f", &[
        (&["0"], "t.f(0u64)", "1403usize", "1403"),
        (&["10"], "t.f(10u64)", "680005usize", "680005"),
        (&["18446744073709551615"], "t.f(18446744073709551615u64)", "4233681847441813393usize", "4233681847441813393"),
    ]);
}

/// `panics never` の関数での `push_u64` / `push_i64`。下ろしが作る `/ 10` `% 10` は除数が正の定数で trap しないので、
/// KIR の検証 (`panics never` に trap 付きの算術を許さない) を通り、木走査と同じ値になる。
#[test]
fn kir_push_u64_in_a_panics_never_function_matches_tree_walker() {
    let fns = "fn t.f(x: u64) -> usize uses [alloc] panics never\n    let mut s = alloc.string.String.new(std.alloc.SYSTEM)\n    match s.push_u64(x) {\n        Ok(_u) => s.len()\n        Err(_e) => 0usize\n    }\nend t.f\n\nfn t.g(x: i64) -> usize uses [alloc] panics never\n    let mut s = alloc.string.String.new(std.alloc.SYSTEM)\n    match s.push_i64(x) {\n        Ok(_u) => s.len()\n        Err(_e) => 0usize\n    }\nend t.g\n\n";
    kir_review_cases("push_never_u", fns, "t.f", &[
        (&["0"], "t.f(0u64)", "1usize", "1"),
        (&["18446744073709551615"], "t.f(18446744073709551615u64)", "20usize", "20"),
    ]);
    kir_review_cases("push_never_i", fns, "t.g", &[
        (&["-5"], "t.g(-5i64)", "2usize", "2"),
    ]);
}

/// `core.str.from_utf8` の検証 (命令 `utf8.valid`、機械語は kir_rt.c) は Rust の規則と同じ:
/// 1〜4 バイト、途中で切れた列、過長な符号化、サロゲート、U+10FFFF 超。Ok は 1000、Err は `valid_up_to`。
#[test]
fn kir_from_utf8_matches_tree_walker() {
    let fns = "fn t.c0() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\x61\\x62\\x63\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c0\n\nfn t.c1() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c1\n\nfn t.c2() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xc3\\xa9\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c2\n\nfn t.c3() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xe3\\x81\\x82\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c3\n\nfn t.c4() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xf0\\x9f\\x98\\x80\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c4\n\nfn t.c5() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xc3\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c5\n\nfn t.c6() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xe3\\x81\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c6\n\nfn t.c7() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xf0\\x9f\\x98\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c7\n\nfn t.c8() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xc0\\x80\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c8\n\nfn t.c9() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xe0\\x80\\xaf\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c9\n\nfn t.c10() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xed\\xa0\\x80\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c10\n\nfn t.c11() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xf4\\x90\\x80\\x80\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c11\n\nfn t.c12() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\x80\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c12\n\nfn t.c13() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xff\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c13\n\nfn t.c14() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\x61\\xe3\\x81\\x82\\x62\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c14\n\nfn t.c15() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xef\\xbf\\xbf\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c15\n\nfn t.c16() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xf4\\x8f\\xbf\\xbf\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c16\n\nfn t.c17() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xc2\\x80\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c17\n\nfn t.c18() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\x61\\x62\\xc3\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c18\n";
    let want = ["1000", "1000", "1000", "1000", "1000", "0", "0", "0", "0", "0", "0", "0", "0", "0", "1000", "1000", "1000", "1000", "2"];
    for (i, w) in want.iter().enumerate() {
        let f = format!("t.c{}", i);
        kir_review_cases(&format!("from_utf8_{}", i), fns, &f, &[(&[], &format!("{}()", f), &format!("{}u64", w), w)]);
    }
}

/// `derive [Clone]` の `clone()`: すべてスカラーの欄を持つ構造体は値の写し。
/// 木走査・参照実行器・機械語で一致を確かめる。
#[test]
fn kir_clone_all_scalar_matches_tree_walker() {
    let src = "type t.Point derive [Clone] pub\n    x: i32\n    y: i32\nend t.Point\n\n\
fn t.main() -> () uses [] panics may\n    let p = t.Point { x: 1i32, y: 2i32 }\n    let q = p.clone()\n    core.check.eq(q.x, 1i32)\n    core.check.eq(q.y, 2i32)\nend t.main\n";
    let t = Tmp::new("kir_clone_scalar", src);
    let k = kir_run(&["kir", "t.main", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "{}", text(&k.stderr));
}

/// `derive [Clone]` の `clone()`: String / Vec<スカラー> の欄を持つ構造体は、
/// 新規確保して中身を複写する。clone の後に元と写しを別々に変更しても独立。


/// `<int>.try_from_<int>(x)`: 範囲チェックと Ok / Err の構築。境界 (0, -1, 各型の最小・最大とその ±1)。
/// 木走査・参照実行器・ASan 付き機械語で一致を確かめる。
#[test]
fn kir_try_from_int_matches_tree_walker() {
    let fns = "fn t.tf_i64_u64(x: i64) -> u64 uses [] panics never\n    match u64.try_from_i64(x) {\n        Ok(v) => v\n        Err(_) => 18446744073709551615u64\n    }\nend t.tf_i64_u64\n\n\
fn t.tf_u64_i64(x: u64) -> i64 uses [] panics never\n    match i64.try_from_u64(x) {\n        Ok(v) => v\n        Err(_) => -9223372036854775808i64\n    }\nend t.tf_u64_i64\n\n\
fn t.tf_u32_u8(x: u32) -> u16 uses [] panics never\n    match u8.try_from_u32(x) {\n        Ok(v) => u16.from_u8(v)\n        Err(_) => 65535u16\n    }\nend t.tf_u32_u8\n";
    // u64.try_from_i64: -1 と i64::MIN は Err、0 と i64::MAX とその-1 は Ok。
    kir_review_cases("tf_i64_u64", &fns, "t.tf_i64_u64", &[
        (&["-1"], "t.tf_i64_u64(-1i64)", "18446744073709551615u64", "18446744073709551615"),
        (&["0"], "t.tf_i64_u64(0i64)", "0u64", "0"),
        (&["9223372036854775807"], "t.tf_i64_u64(9223372036854775807i64)", "9223372036854775807u64", "9223372036854775807"),
        (&["-9223372036854775808"], "t.tf_i64_u64(-9223372036854775808i64)", "18446744073709551615u64", "18446744073709551615"),
        (&["9223372036854775806"], "t.tf_i64_u64(9223372036854775806i64)", "9223372036854775806u64", "9223372036854775806"),
    ]);
    // i64.try_from_u64: 0 と i64::MAX は Ok、u64::MAX と i64::MAX+1 は Err。
    kir_review_cases("tf_u64_i64", &fns, "t.tf_u64_i64", &[
        (&["0"], "t.tf_u64_i64(0u64)", "0i64", "0"),
        (&["9223372036854775807"], "t.tf_u64_i64(9223372036854775807u64)", "9223372036854775807i64", "9223372036854775807"),
        (&["18446744073709551615"], "t.tf_u64_i64(18446744073709551615u64)", "-9223372036854775808i64", "-9223372036854775808"),
        (&["9223372036854775808"], "t.tf_u64_i64(9223372036854775808u64)", "-9223372036854775808i64", "-9223372036854775808"),
    ]);
    // u8.try_from_u32: 0 と 255 は Ok、256 と u32::MAX は Err。
    kir_review_cases("tf_u32_u8", &fns, "t.tf_u32_u8", &[
        (&["0"], "t.tf_u32_u8(0u32)", "0u16", "0"),
        (&["255"], "t.tf_u32_u8(255u32)", "255u16", "255"),
        (&["256"], "t.tf_u32_u8(256u32)", "65535u16", "65535"),
        (&["4294967295"], "t.tf_u32_u8(4294967295u32)", "65535u16", "65535"),
    ]);
}

const KIR_OWNFOR_FNS: &str = "fn t.for_names(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut total = 0usize
    for s in v {
        total = total.wrapping_add(s.len())
    }
    total
end t.for_names

fn t.for_items(n: u32) -> usize uses [alloc] panics never
    let v = t.items(n)
    let mut total = 0usize
    for it in v {
        total = total.wrapping_add(it.name.len()).wrapping_add(usize.from_u32(it.v))
    }
    total
end t.for_items

fn t.for_move(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut out = t.names(0u32)
    for s in v {
        core.mem.drop(out.push(s))
    }
    out.len()
end t.for_move

fn t.for_break(n: u32, k: usize) -> usize uses [alloc] panics never
    let v = t.items(n)
    let mut seen = 0usize
    for it in v {
        if seen >= k {
            break
        }
        seen = seen.wrapping_add(1usize).wrapping_add(it.name.len())
    }
    seen
end t.for_break

fn t.for_ret(n: u32, k: u32) -> usize uses [alloc] panics never
    let v = t.items(n)
    for it in v {
        if it.v == k {
            return it.name.len()
        }
    }
    77usize
end t.for_ret

fn t.for_wild(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut c = 0usize
    for _ in v {
        c = c.wrapping_add(1usize)
    }
    c
end t.for_wild

fn t.for_nested(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut c = 0usize
    for s in v {
        let w = t.names(2u32)
        for q in w {
            c = c.wrapping_add(q.len()).wrapping_add(s.len())
        }
    }
    c
end t.for_nested
";

#[test]
fn kir_for_by_value_over_owning_elems_matches_tree_walker() {
    // `for x in v` (所有する Vec を値で回す): 要素の所有が `x` に移る。消費しなかった `x` は 1 周の終わりで、`break` / `return` で抜けたときの
    // 残りの要素とバッファは隠しヘッダで解放される。参照実行器の漏れ・二重解放の検出と ASan で確かめる。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.for_names", &["3"], "9usize", "9"),
        ("t.for_names", &["0"], "0usize", "0"),
        ("t.for_items", &["4"], "18usize", "18"),
        ("t.for_move", &["5"], "5usize", "5"),
        ("t.for_move", &["0"], "0usize", "0"),
        ("t.for_break", &["5", "2"], "4usize", "4"),
        ("t.for_break", &["5", "0"], "0usize", "0"),
        ("t.for_break", &["2", "9"], "8usize", "8"),
        ("t.for_ret", &["5", "2"], "3usize", "3"),
        ("t.for_ret", &["5", "9"], "77usize", "77"),
        ("t.for_wild", &["4"], "4usize", "4"),
        ("t.for_nested", &["3"], "36usize", "36"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!(
            "{}({})",
            f,
            args.iter().enumerate().map(|(k, a)| if k == 1 && args.len() == 2 && *f == "t.for_break" { format!("{}usize", a) } else { format!("{}u32", a) }).collect::<Vec<_>>().join(", ")
        );
        let body = format!("    core.check.eq({}, {})\n", call, lit);
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_ownfor_{}", i), &format!("{}\n{}\n{}", KIR_BORROW_FNS, KIR_OWNFOR_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

const KIR_OWNMUT_FNS: &str = "fn t.mut_for_str(n: u32) -> usize uses [alloc] panics never
    let mut v = t.names(n)
    let mut total = 0usize
    for s in &mut v {
        total = total.wrapping_add(s.len())
    }
    for s in &v {
        total = total.wrapping_add(s.len())
    }
    total
end t.mut_for_str

fn t.mut_for_item(n: u32) -> usize uses [alloc] panics never
    let mut v = t.items(n)
    for it in &mut v {
        it.v = it.v.wrapping_add(10u32)
        it.name = t.word()
    }
    let mut total = 0usize
    for it in &v {
        total = total.wrapping_add(it.name.len()).wrapping_add(usize.from_u32(it.v))
    }
    total
end t.mut_for_item

fn t.bump_all(v: &mut alloc.vec.Vec<t.Item, std.alloc.System>) -> usize uses [alloc] panics never
    let mut c = 0usize
    for it in v {
        it.v = it.v.wrapping_add(1u32)
        if it.v > 3u32 {
            break
        }
        c = c.wrapping_add(1usize)
    }
    c
end t.bump_all

fn t.mut_for_param(n: u32) -> usize uses [alloc] panics never
    let mut v = t.items(n)
    let c = t.bump_all(&mut v)
    let mut total = c
    for it in &v {
        total = total.wrapping_add(usize.from_u32(it.v))
    }
    total
end t.mut_for_param

fn t.idx_push(n: u32, i: usize) -> usize uses [alloc] panics may
    let mut v = t.names(n)
    core.mem.drop(v[i].push_str(\"zz\"))
    v[i].len()
end t.idx_push

fn t.idx_field(n: u32, i: usize) -> usize uses [alloc] panics may
    let mut v = t.items(n)
    core.mem.drop(v[i].name.push_str(\"zz\"))
    v[i].v = 40u32
    v[i].name = t.word()
    v[i].name.len().wrapping_add(usize.from_u32(v[i].v))
end t.idx_field

fn t.idx_assign(n: u32, i: usize) -> usize uses [alloc] panics may
    let mut v = t.items(n)
    v[i] = t.Item { name: t.word(), v: 7u32 }
    v[i].name.len().wrapping_add(usize.from_u32(v[i].v))
end t.idx_assign

fn t.idx_assign_str(n: u32, i: usize) -> usize uses [alloc] panics may
    let mut v = t.names(n)
    v[i] = t.word()
    v[i].len()
end t.idx_assign_str

fn t.get_destr(n: u32, i: usize) -> u32 uses [alloc] panics never
    let v = t.items(n)
    match v.get(i) {
        None => 0u32
        Some(t.Item { name: _, v: x }) => x
    }
end t.get_destr

fn t.get_destr_str(n: u32, i: usize) -> usize uses [alloc] panics never
    let v = t.items(n)
    match v.get(i) {
        None => 99usize
        Some(t.Item { name: s, v: x }) => s.len().wrapping_add(usize.from_u32(x))
    }
end t.get_destr_str
";

#[test]
fn kir_mut_borrow_of_owning_elems_matches_tree_walker() {
    // `for x in &mut v` (所有物を持つ要素・スカラー)、`v[i]` の要素への `&mut` (フィールドへの代入・`push_str`・要素の代入。古い値は解放してから)、
    // `get` の腕の構造体パターン。木走査と値が一致し、参照実行器が漏れ・二重解放を検出せず、ASan も報告しない。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.mut_for_str", &["3"], "18usize", "18"),
        ("t.mut_for_str", &["0"], "0usize", "0"),
        ("t.mut_for_item", &["3"], "42usize", "42"),
        ("t.mut_for_item", &["0"], "0usize", "0"),
        ("t.mut_for_param", &["6"], "22usize", "22"),
        ("t.idx_push", &["3", "1"], "5usize", "5"),
        ("t.idx_field", &["3", "1"], "43usize", "43"),
        ("t.idx_assign", &["3", "1"], "10usize", "10"),
        ("t.idx_assign_str", &["3", "2"], "3usize", "3"),
        ("t.get_destr", &["3", "1"], "1u32", "1"),
        ("t.get_destr", &["3", "7"], "0u32", "0"),
        ("t.get_destr_str", &["3", "2"], "5usize", "5"),
        ("t.get_destr_str", &["3", "7"], "99usize", "99"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let two = args.len() == 2;
        let call = format!(
            "{}({})",
            f,
            args.iter().enumerate().map(|(k, a)| if two && k == 1 { format!("{}usize", a) } else { format!("{}u32", a) }).collect::<Vec<_>>().join(", ")
        );
        let body = format!("    core.check.eq({}, {})\n", call, lit);
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_ownmut_{}", i), &format!("{}\n{}\n{}", KIR_BORROW_FNS, KIR_OWNMUT_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_mut_borrow_of_owning_elems_out_of_range_panics_like_tree_walker() {
    for (f, args) in [("t.idx_field", ["3", "5"]), ("t.idx_assign", ["3", "9"])] {
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    core.check.eq({}({}u32, {}usize), 0usize)\n    Ok(())\nend t.main\n", f, args[0], args[1]);
        let t = Tmp::new("kir_ownmut_oob", &format!("{}\n{}\n{}", KIR_BORROW_FNS, KIR_OWNMUT_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(101), "tree-walker: {}", text(&o.stderr));
        let k = kir_run(&["kir", f, "--run", args[0], args[1]], &t.0);
        assert_eq!(k.status.code(), Some(101), "kir {}: {}", f, text(&k.stderr));
        assert!(text(&k.stderr).contains("index out of bounds"), "{}", text(&k.stderr));
    }
}

const KIR_WITH_FNS: &str = include_str!("data/kir_with_fns.ksnl");

#[test]
fn kir_with_exits_match_tree_walker() {
    // `with` の後始末が、末尾 / `return` / `?` / `break` / `continue` / 入れ子 / 同名の隠蔽 / 後始末式の `match` / 所有物の束縛 /
    // ソケットの各経路でちょうど 1 回、木走査と同じ順序で実行されること。参照実行器が漏れ・二重解放を検出せず、
    // `--run` / `--native` (ASan を含む) が一致する。値は「後始末の記録 * 1000 + 結果」。
    // (関数, 引数, 期待値 (型付きリテラル), 期待値 (表示))
    let cases: &[(&str, &str, &str, &str)] = &[
        ("t.run_tail", "3", "4008u32", "4008"),
        ("t.run_ret", "1", "3001u32", "3001"),
        ("t.run_ret", "9", "3007u32", "3007"),
        ("t.run_try", "1", "2003u32", "2003"),
        ("t.run_try", "9", "2509u32", "2509"),
        ("t.run_loop", "3", "123004u32", "123004"),
        ("t.run_loop", "9", "1234004u32", "1234004"),
        ("t.run_nested", "1", "21001u32", "21001"),
        ("t.run_nested", "5", "21003u32", "21003"),
        ("t.run_close_match", "0", "9001u32", "9001"),
        ("t.run_close_match", "3", "3030u32", "3030"),
        ("t.run_close_match", "8", "8009u32", "8009"),
        ("t.w_vec_own", "0", "2usize", "2"),
        ("t.w_vec_own", "1", "12usize", "12"),
        ("t.w_val_own", "4", "5u32", "5"),
        ("t.w_body_owned", "4", "2usize", "2"),
        ("t.w_str_close", "5", "4usize", "4"),
        ("t.w_str_borrow", "0", "3usize", "3"),
        ("t.w_str_borrow", "1", "99usize", "99"),
        ("t.w_sock", "0", "6u32", "6"),
        ("t.w_sock", "1", "5u32", "5"),
    ];
    for (i, (f, arg, lit, want)) in cases.iter().enumerate() {
        let body = format!("    core.check.eq({}({}u32), {})\n", f, arg, lit);
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, io.net] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_with_exits_{}", i), &format!("{}\n{}", KIR_WITH_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker {}({}): {}", f, arg, text(&o.stderr));
        let k = kir_run(&["kir", f, "--run", arg], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}({}): {}", f, arg, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}({})", f, arg);
    }
}

// ---- 型引数付きの関数・メソッドの単相化 (設計_KIR.md 20 章、kir/generic-method) ----

/// 型引数付きの関数・メソッドを、2 つ以上の型で呼ぶ。推論と `::<..>` の明示の両方。
const MONO_FNS: &str = "type t.Acc
    total: u64
end t.Acc

type t.Pair
    a: u32
    b: u32
end t.Pair

fn t.Acc.put[T](self: &mut t.Acc, v: T) -> T uses [] panics never
    self.total = self.total.wrapping_add(1u64)
    v
end t.Acc.put

fn t.ann[T](x: T) -> T uses [] panics never
    let y: T = x
    y
end t.ann

fn t.first[A, B](a: A, b: B) -> A uses [] panics never
    let _unused = b
    a
end t.first

fn t.id[T](x: T) -> T uses [] panics never
    x
end t.id

fn t.make[T](x: T) -> T uses [] panics never
    t.id(x)
end t.make

fn t.pair2[T](a: T, b: T) -> (T, T) uses [] panics never
    (a, b)
end t.pair2

fn t.w() -> u32 uses [] panics may
    core.check.eq(t.id(5u32), 5u32)
    core.check.eq(t.id(7u8), 7u8)
    core.check.eq(t.id::<i64>(9i64), 9i64)
    core.check.eq(t.first(1u32, 2u8), 1u32)
    core.check.eq(t.first(3u8, true), 3u8)
    core.check.eq(t.make(11u16), 11u16)
    core.check.eq(t.ann(5u8), 5u8)
    let p = t.id(t.Pair { a: 1u32, b: 2u32 })
    core.check.eq(p.b, 2u32)
    let (x, y) = t.pair2(1u32, 2u32)
    let mut acc = t.Acc { total: 0u64 }
    core.check.eq(acc.put(4u32), 4u32)
    core.check.eq(acc.put(5u8), 5u8)
    core.check.eq(acc.put::<i16>(6i16), 6i16)
    core.check.eq(acc.total, 3u64)
    t.id(x.wrapping_add(y).wrapping_add(39u32))
end t.w

";

#[test]
fn kir_generic_functions_and_methods_are_monomorphized() {
    let t = Tmp::new("kir_mono", &format!("{}{}", MONO_FNS, main_wrap("", "    core.check.eq(t.w(), 42u32)\n")));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker: {}", text(&o.stderr));
    let k = kir_run(&["kir", "t.w", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir: {}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "42");
    // 型引数ごとに別の関数 (名前に型引数が入る)。
    let d = run(&["kir", "t.w"], &t.0);
    let out = format!("{}{}", text(&d.stdout), text(&d.stderr));
    for key in ["t.id<u32>", "t.id<u8>", "t.id<i64>", "t.id<t.Pair>", "t.first<u32,u8>", "t.first<u8,bool>", "t.Acc.put<u32>", "t.Acc.put<u8>", "t.Acc.put<i16>"] {
        assert!(out.contains(key), "{} が KIR にない:\n{}", key, out);
    }
}

#[test]
fn kir_generic_with_effect_parameter_is_rejected_with_a_reason() {
    let src = "fn t.eff[E: uses](x: u32) -> u32 uses [E] panics never
    x
end t.eff

fn t.w() -> u32 uses [] panics never
    t.eff::<[]>(3u32)
end t.w
";
    let t = Tmp::new("kir_mono_eff", src);
    let k = run(&["kir", "t.w", "--run"], &t.0);
    assert_ne!(k.status.code(), Some(0));
    assert!(text(&k.stderr).contains("uses のパラメータ (E) を持つので、単相化は対象外"), "{}", text(&k.stderr));
}


// ---- Box<T> と Box<dyn Tr> (設計_KIR.md 21 章、kir/box-dyn) ----

/// `Box` / `dyn` の試験用の関数。
const KIR_DYN_FNS: &str = r#"import alloc.boxed
import alloc.vec

trait t.Shape[E: uses, P: panics] pub
    fn area(self: &Self) -> u32 uses [E] panics P
    fn grow(self: &mut Self, by: u32) -> u32 uses [E] panics P
end t.Shape

type t.Sq
    s: u32
end t.Sq

type t.Rect
    w: u32
    h: u32
    tag: alloc.string.String
end t.Rect

type t.Slot
    h: alloc.boxed.Box<dyn t.Shape<[], never> + 'static, std.alloc.System>
end t.Slot

impl t.Shape<[], never> for t.Sq
fn t.Sq.area(self: &t.Sq) -> u32 uses [] panics never impls t.Shape<[], never>
    self.s.wrapping_mul(self.s)
end t.Sq.area
fn t.Sq.grow(self: &mut t.Sq, by: u32) -> u32 uses [] panics never impls t.Shape<[], never>
    self.s = self.s.wrapping_add(by)
    self.s
end t.Sq.grow

impl t.Shape<[], never> for t.Rect
fn t.Rect.area(self: &t.Rect) -> u32 uses [] panics never impls t.Shape<[], never>
    self.w.wrapping_mul(self.h)
end t.Rect.area
fn t.Rect.grow(self: &mut t.Rect, by: u32) -> u32 uses [] panics never impls t.Shape<[], never>
    self.w = self.w.wrapping_add(by)
    self.w
end t.Rect.grow

fn t.box_u32(n: u32) -> u32 uses [alloc] panics may
    match boxed.Box.new(std.alloc.SYSTEM, n) {
        Ok(bx) => {
            let r = *bx.as_ref()
            r.wrapping_add(1u32)
        }
        Err(_e) => 0u32
    }
end t.box_u32

fn t.box_inner(n: u32) -> u32 uses [alloc] panics may
    match boxed.Box.new(std.alloc.SYSTEM, n) {
        Ok(bx) => bx.into_inner()
        Err(_e) => 0u32
    }
end t.box_inner

fn t.box_mut_r(n: u32) -> Result<u32, alloc.alloc.AllocError> uses [alloc] panics never
    let mut bx = boxed.Box.new(std.alloc.SYSTEM, n)?
    let r = bx.as_mut()
    *r = r.wrapping_add(10u32)
    Ok(*bx.as_ref())
end t.box_mut_r

fn t.box_mut(n: u32) -> u32 uses [alloc] panics may
    match t.box_mut_r(n) {
        Ok(v) => v
        Err(_e) => 0u32
    }
end t.box_mut

fn t.box_string_r(n: usize) -> Result<usize, alloc.alloc.AllocError> uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    let mut i = 0usize
    while i < n {
        s.push_str("ab")?
        i = i.wrapping_add(1usize)
    }
    let bx = boxed.Box.new(std.alloc.SYSTEM, s)?
    Ok(bx.as_ref().len())
end t.box_string_r

fn t.box_string(n: usize) -> usize uses [alloc] panics may
    match t.box_string_r(n) {
        Ok(v) => v
        Err(_e) => 0usize
    }
end t.box_string

fn t.slot_sq(s: u32) -> Result<t.Slot, alloc.alloc.AllocError> uses [alloc] panics never
    let b = boxed.Box.new(std.alloc.SYSTEM, t.Sq { s: s })?
    Ok(t.Slot { h: boxed.Box.into_dyn(b) })
end t.slot_sq

fn t.slot_rect(w: u32) -> Result<t.Slot, alloc.alloc.AllocError> uses [alloc] panics never
    let tag = alloc.string.String.new(std.alloc.SYSTEM)
    let b = boxed.Box.new(std.alloc.SYSTEM, t.Rect { w: w, h: 2u32, tag: tag })?
    Ok(t.Slot { h: boxed.Box.into_dyn(b) })
end t.slot_rect

fn t.dyn_one_r(pick: bool) -> Result<u32, alloc.alloc.AllocError> uses [alloc] panics never
    let mut total = 0u32
    if pick {
        let mut s = t.slot_sq(3u32)?
        total = total.wrapping_add(s.h.grow(1u32))
        total = total.wrapping_add(s.h.area())
    } else {
        let s = t.slot_rect(5u32)?
        total = total.wrapping_add(s.h.area())
    }
    Ok(total)
end t.dyn_one_r

fn t.dyn_one(pick: bool) -> u32 uses [alloc] panics may
    match t.dyn_one_r(pick) {
        Ok(v) => v
        Err(_e) => 0u32
    }
end t.dyn_one

fn t.dyn_vec_r(n: u32) -> Result<u32, alloc.alloc.AllocError> uses [alloc] panics may
    let mut v = vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        if i % 2u32 == 0u32 {
            v.push(t.slot_sq(i)?)?
        } else {
            v.push(t.slot_rect(i)?)?
        }
        i = i.wrapping_add(1u32)
    }
    let mut total = 0u32
    let mut j = 0usize
    while j < v.len() {
        total = total.wrapping_add(v[j].h.area())
        j = j.wrapping_add(1usize)
    }
    Ok(total)
end t.dyn_vec_r

fn t.dyn_vec(n: u32) -> u32 uses [alloc] panics may
    match t.dyn_vec_r(n) {
        Ok(v) => v
        Err(_e) => 0u32
    }
end t.dyn_vec

fn t.slot_rect_tag(w: u32, n: usize) -> Result<t.Slot, alloc.alloc.AllocError> uses [alloc] panics never
    let mut tag = alloc.string.String.new(std.alloc.SYSTEM)
    let mut i = 0usize
    while i < n {
        tag.push_str("xy")?
        i = i.wrapping_add(1usize)
    }
    let b = boxed.Box.new(std.alloc.SYSTEM, t.Rect { w: w, h: 3u32, tag: tag })?
    Ok(t.Slot { h: boxed.Box.into_dyn(b) })
end t.slot_rect_tag

fn t.rect_str_r(w: u32, n: usize) -> Result<u32, alloc.alloc.AllocError> uses [alloc] panics never
    let mut s = t.slot_rect_tag(w, n)?
    let g = s.h.grow(2u32)
    Ok(g.wrapping_add(s.h.area()))
end t.rect_str_r

fn t.rect_str(w: u32, n: usize) -> u32 uses [alloc] panics may
    match t.rect_str_r(w, n) {
        Ok(v) => v
        Err(_e) => 0u32
    }
end t.rect_str

fn t.consume(b: alloc.boxed.Box<u32, std.alloc.System>) -> u32 uses [] panics never
    *b.as_ref()
end t.consume

fn t.make_box(n: u32) -> Result<alloc.boxed.Box<u32, std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics never
    let b = boxed.Box.new(std.alloc.SYSTEM, n)?
    Ok(b)
end t.make_box

fn t.box_move_r(n: u32) -> Result<u32, alloc.alloc.AllocError> uses [alloc] panics never
    let b = t.make_box(n)?
    let c = t.make_box(n.wrapping_add(1u32))?
    let first = t.consume(b)
    Ok(first.wrapping_add(*c.as_ref()))
end t.box_move_r

fn t.box_move(n: u32) -> u32 uses [alloc] panics may
    match t.box_move_r(n) {
        Ok(v) => v
        Err(_e) => 0u32
    }
end t.box_move

fn t.box_struct_r(s: u32) -> Result<u32, alloc.alloc.AllocError> uses [alloc] panics never
    let b = boxed.Box.new(std.alloc.SYSTEM, t.Sq { s: s })?
    let sq = b.into_inner()
    Ok(sq.s)
end t.box_struct_r

fn t.box_struct(s: u32) -> u32 uses [alloc] panics may
    match t.box_struct_r(s) {
        Ok(v) => v
        Err(_e) => 0u32
    }
end t.box_struct

fn t.box_rect_r(w: u32) -> Result<u32, alloc.alloc.AllocError> uses [alloc] panics never
    let mut tag = alloc.string.String.new(std.alloc.SYSTEM)
    tag.push_str("hello")?
    let b = boxed.Box.new(std.alloc.SYSTEM, t.Rect { w: w, h: 2u32, tag: tag })?
    Ok(b.as_ref().w)
end t.box_rect_r

fn t.box_rect(w: u32) -> u32 uses [alloc] panics may
    match t.box_rect_r(w) {
        Ok(v) => v
        Err(_e) => 0u32
    }
end t.box_rect

"#;

#[test]
fn kir_box_dyn_match_tree_walker() {
    // `Box<u32>` / `Box<String>` / `Box<構造体>`、`Box<dyn Tr>` の 2 つの実装の呼び分け、`Vec<Box<dyn Tr>>`、移動と解放。
    // 木走査と同じ値になり、参照実行器が漏れ・二重解放を内部エラーにせず、`--native` と ASan も何も出さないこと。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.box_u32", &["4u32"], "5u32", "5"),
        ("t.box_inner", &["7u32"], "7u32", "7"),
        ("t.box_mut", &["5u32"], "15u32", "15"),
        ("t.box_string", &["0usize"], "0usize", "0"),
        ("t.box_string", &["3usize"], "6usize", "6"),
        ("t.box_struct", &["9u32"], "9u32", "9"),
        ("t.box_rect", &["7u32"], "7u32", "7"),
        ("t.box_move", &["6u32"], "13u32", "13"),
        ("t.dyn_one", &["true"], "20u32", "20"),
        ("t.dyn_one", &["false"], "10u32", "10"),
        ("t.rect_str", &["4u32", "3usize"], "24u32", "24"),
        ("t.rect_str", &["4u32", "0usize"], "24u32", "24"),
        ("t.dyn_vec", &["0u32"], "0u32", "0"),
        ("t.dyn_vec", &["5u32"], "28u32", "28"),
        ("t.dyn_vec", &["40u32"], "10680u32", "10680"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.join(", "));
        let body = format!("    core.check.eq({}, {})\n", call, lit);
        let main = format!("\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_box_dyn_{}", i), &format!("{}{}", KIR_DYN_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}{}", call, text(&o.stdout), text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        // 入口の引数は接尾辞なしの数 (`4u32` → `4`)
        let plain: Vec<String> = args
            .iter()
            .map(|a| if *a == "true" || *a == "false" { a.to_string() } else { a.split(|c: char| c.is_alphabetic()).next().unwrap().to_string() })
            .collect();
        kargs.extend(plain.iter().map(|s| s.as_str()));
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_box_dyn_dispatch_uses_a_per_trait_helper_without_new_ops() {
    // `dyn` のメソッド呼び出しは vtable の番号で分岐する補助関数 `dyn.call.<Tr>.<m>`、解放は `dyn.drop`。
    let main = "\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    core.check.eq(t.dyn_vec(5u32), 28u32)\n    std.io.print_line(\"ok\")\nend t.main\n";
    let t = Tmp::new("kir_box_dyn_dump", &format!("{}{}", KIR_DYN_FNS, main));
    let o = run(&["kir", "t.dyn_vec"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let d = text(&o.stdout);
    assert!(d.contains("call dyn.call.t.Shape.area("), "{}", d);
    assert!(d.contains("call dyn.drop("), "{}", d);
    assert!(d.contains("dynbox<t.Shape>"), "{}", d);
    let b = run(&["build", "--backend", "llvm"], &t.0);
    assert_eq!(b.status.code(), Some(0), "{}{}", text(&b.stdout), text(&b.stderr));
    assert!(!text(&b.stdout).contains("backend skipped"), "{}", text(&b.stdout));
}

#[test]
fn kir_box_dyn_reports_unsupported_with_a_reason() {
    // `Box<dyn>` から `&dyn` を作る `as_ref` は対象外。誤った KIR を出さず理由を付けて拒否する。
    let tail = "
fn t.via_ref_r() -> Result<u32, alloc.alloc.AllocError> uses [alloc] panics never
    let s = t.slot_sq(3u32)?
    let r = s.h.as_ref()
    Ok(r.area())
end t.via_ref_r

fn t.via_ref() -> u32 uses [alloc] panics never
    match t.via_ref_r() {
        Ok(v) => v
        Err(_e) => 0u32
    }
end t.via_ref

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.via_ref(), 9u32)
    std.io.print_line(\"ok\")
end t.main
";
    let t = Tmp::new("kir_box_dyn_reject", &format!("{}{}", KIR_DYN_FNS, tail));
    let r = run(&["run"], &t.0);
    assert_eq!(r.status.code(), Some(0), "tree-walker: {}{}", text(&r.stdout), text(&r.stderr));
    let k = run(&["kir", "t.via_ref_r"], &t.0);
    assert_ne!(k.status.code(), Some(0), "{}", text(&k.stdout));
    assert!(text(&k.stderr).contains("Box<dyn> の as_ref"), "{}", text(&k.stderr));
}

/// フィールドの `Vec` に対する `as_slice().get(i)` が、要素がスカラー (`u8`) と
/// 所有物 (`String`) の両方で、木走査・KIR 実行器 (`--run`)・機械語 (`--native`) で一致すること。
#[test]
fn kir_as_slice_get_on_field_matches_run_kir_native() {
    let src = r#"type t.Store pub
    urls: alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>
    data: alloc.vec.Vec<u8, std.alloc.System>
end t.Store

fn t.Store.new() -> t.Store uses [alloc] panics may pub
    let mut urls = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let first = match alloc.string.String.from_str(std.alloc.SYSTEM, "first") {
        Ok(s) => s
        Err(_e) => core.panic.panic("from_str failed")
    }
    core.mem.drop(urls.push(first))
    let second = match alloc.string.String.from_str(std.alloc.SYSTEM, "second") {
        Ok(s) => s
        Err(_e) => core.panic.panic("from_str failed")
    }
    core.mem.drop(urls.push(second))
    let third = match alloc.string.String.from_str(std.alloc.SYSTEM, "third") {
        Ok(s) => s
        Err(_e) => core.panic.panic("from_str failed")
    }
    core.mem.drop(urls.push(third))
    let mut data = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(data.push(10u8))
    core.mem.drop(data.push(20u8))
    core.mem.drop(data.push(30u8))
    t.Store { urls: urls, data: data }
end t.Store.new

fn t.Store.lookup_str(self: &t.Store, i: usize) -> Option<&str> uses [] panics never pub
    match self.urls.as_slice().get(i) {
        Some(url) => Some(url.as_str())
        None => None
    }
end t.Store.lookup_str

fn t.Store.lookup_u8(self: &t.Store, i: usize) -> u8 uses [] panics never pub
    match self.data.as_slice().get(i) {
        Some(x) => *x
        None => 0u8
    }
end t.Store.lookup_u8

fn t.main() -> Result<(), std.io.Error> uses [alloc] panics may
    let s = t.Store.new()
    core.check.eq(s.lookup_str(0usize), Some("first"))
    core.check.eq(s.lookup_str(1usize), Some("second"))
    core.check.eq(s.lookup_str(2usize), Some("third"))
    core.check.eq(s.lookup_str(3usize), None)
    core.check.eq(s.lookup_str(100usize), None)
    core.check.eq(s.lookup_u8(0usize), 10u8)
    core.check.eq(s.lookup_u8(1usize), 20u8)
    core.check.eq(s.lookup_u8(2usize), 30u8)
    core.check.eq(s.lookup_u8(3usize), 0u8)
    Ok(())
end t.main

fn t.lookup_u8_at(i: usize) -> u8 uses [alloc] panics may pub
    let s = t.Store.new()
    s.lookup_u8(i)
end t.lookup_u8_at

fn t.check_url_at(i: usize) -> bool uses [alloc] panics may pub
    let s = t.Store.new()
    match s.urls.as_slice().get(i) {
        Some(url) => url.as_str() == "first"
        None => false
    }
end t.check_url_at
"#;
    let t = Tmp::new("kir_as_slice_get_on_field", src);
    // 木走査: t.main が全チェックを通過すれば終了コード 0。
    let r = run(&["run"], &t.0);
    assert_eq!(r.status.code(), Some(0), "tree-walker: {}{}", text(&r.stdout), text(&r.stderr));
    // KIR: 要素がスカラー (u8) の as_slice().get(i)。
    for (i, want) in [(0usize, "10"), (1usize, "20"), (2usize, "30"), (3usize, "0"), (100usize, "0")] {
        let arg = i.to_string();
        let k = kir_run(&["kir", "t.lookup_u8_at", "--run", arg.as_str()], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir lookup_u8_at {}: {}{}", i, text(&k.stdout), text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir lookup_u8_at {} value", i);
    }
    // KIR: 要素が所有物 (String) の as_slice().get(i) + as_str() + == 。
    for (i, want) in [(0usize, "true"), (1usize, "false"), (2usize, "false"), (3usize, "false"), (100usize, "false")] {
        let arg = i.to_string();
        let k = kir_run(&["kir", "t.check_url_at", "--run", arg.as_str()], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir check_url_at {}: {}{}", i, text(&k.stdout), text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir check_url_at {} value", i);
    }
}

/// `&str` / スライスの `split_at`、`eq_ignore_ascii_case`、`contains`、`String` の `contains` / `starts_with` /
/// `ends_with` / `as_bytes`、一時の `String` の `as_str`、`split_once` の結果の `let` 分解 (設計_KIR.md 23 章)。
/// 木走査と `--run` / `--native` の値が一致する。各関数は `bool` を返し、木走査の `main` の検査が通る。
#[test]
fn kir_str_rest_matches_tree_walker() {
    let helpers = r#"fn t.mk_s() -> Result<alloc.string.String<std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics may
    alloc.string.String.from_str(std.alloc.SYSTEM, "xy")
end t.mk_s

"#;
    // (関数名, 本体の行 (4 字下げ), 真になるべき)
    let cases: &[(&str, &str)] = &[
        ("t.sa_mid", "    \"hello\".split_at(2usize) == Some((\"he\", \"llo\"))"),
        ("t.sa_zero", "    \"hello\".split_at(0usize) == Some((\"\", \"hello\"))"),
        ("t.sa_len", "    \"hello\".split_at(5usize) == Some((\"hello\", \"\"))"),
        ("t.sa_past", "    \"hello\".split_at(6usize) == None"),
        ("t.sa_utf8_mid", "    \"héllo\".split_at(2usize) == None"),
        ("t.sa_utf8", "    \"héllo\".split_at(3usize) == Some((\"hé\", \"llo\"))"),
        ("t.sb_mid", "    b\"abc\".split_at(1usize) == Some((b\"a\", b\"bc\"))"),
        ("t.sb_end", "    b\"abc\".split_at(3usize) == Some((b\"abc\", b\"\"))"),
        ("t.sb_past", "    b\"abc\".split_at(4usize) == None"),
        ("t.ei_yes", "    \"HeLLo\".eq_ignore_ascii_case(\"hello\")"),
        ("t.ei_header", "    \"Content-Length\".eq_ignore_ascii_case(\"content-LENGTH\")"),
        ("t.ei_punct", "    \"[]@`{\".eq_ignore_ascii_case(\"[]@`{\")"),
        ("t.ei_no_len", "    \"abc\".eq_ignore_ascii_case(\"abcd\") == false"),
        ("t.ei_no_char", "    \"abc\".eq_ignore_ascii_case(\"abd\") == false"),
        ("t.ei_nonascii", "    \"É\".eq_ignore_ascii_case(\"é\") == false"),
        ("t.ei_empty", "    \"\".eq_ignore_ascii_case(\"\")"),
        ("t.ct_mid", "    \"hello world\".contains(\"o w\")"),
        ("t.ct_empty", "    \"hello\".contains(\"\")"),
        ("t.ct_no", "    \"hello\".contains(\"x\") == false"),
        ("t.ct_long", "    \"hello\".contains(\"hello!\") == false"),
        (
            "t.sx_contains",
            "    let s = alloc.string.String.from_str(std.alloc.SYSTEM, \"hello world\").unwrap()\n    s.contains(\"lo w\") && s.contains(\"xyz\") == false",
        ),
        (
            "t.sx_prefix",
            "    let s = alloc.string.String.from_str(std.alloc.SYSTEM, \"hello world\").unwrap()\n    s.starts_with(\"hel\") && s.ends_with(\"rld\") && s.starts_with(\"el\") == false",
        ),
        (
            "t.sx_bytes",
            "    let s = alloc.string.String.from_str(std.alloc.SYSTEM, \"ab\").unwrap()\n    s.as_bytes() == b\"ab\"",
        ),
        ("t.sx_tmp", "    t.mk_s().unwrap().as_str() == \"xy\""),
        ("t.sx_tmp_len", "    t.mk_s().unwrap().as_bytes().len() == 2usize"),
        (
            "t.sl_let",
            "    let (line, rest) = match \"GET /x HTTP\".split_once(\" \") {\n        Some(pair) => pair\n        None => return false\n    }\n    line == \"GET\" && rest == \"/x HTTP\"",
        ),
    ];
    let mut src = String::from(helpers);
    let mut main = String::from("\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n");
    for (f, body) in cases {
        src.push_str(&format!("fn {}() -> bool uses [alloc] panics may\n{}\nend {}\n\n", f, body, f));
        main.push_str(&format!("    core.check.eq({}(), true)\n", f));
    }
    main.push_str("    Ok(())\nend t.main\n");
    let t = Tmp::new("kir_str_rest", &format!("{}{}", src, main));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    for (f, _) in cases {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), "true", "kir value of {}", f);
    }
}

/// `unwrap` の `Err` の表示: 派生の `Debug` を持つ列挙型の `Err` は、木走査と同じ `Bad(3)` / `Neg` の文言で止まる。
#[test]
fn kir_unwrap_err_enum_debug_matches_tree_walker() {
    let src = r#"enum t.Er derive [Debug, Clone, PartialEq]
    Bad(usize)
    Neg
end t.Er

fn t.pick(n: i32) -> Result<u32, t.Er> uses [] panics may
    if n > 0i32 {
        Ok(7u32)
    } else {
        if n == 0i32 {
            Err(t.Er.Bad(3usize))
        } else {
            Err(t.Er.Neg)
        }
    }
end t.pick

fn t.un(n: i32) -> u32 uses [] panics may
    t.pick(n).unwrap()
end t.un
"#;
    let cases = [(0, "Bad(3)"), (-1, "Neg")];
    for (i, (arg, shown)) in cases.iter().enumerate() {
        let msg = format!("called `Result::unwrap()` on an `Err` value: {}", shown);
        let t = Tmp::new(&format!("kir_unwrap_enum_{}", i), &format!("{}{}", src, main_wrap("", &format!("    let _x = t.un({})\n", arg))));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(101), "tree-walker\n{}", text(&o.stderr));
        assert!(text(&o.stderr).contains(&format!("panic: {}", msg)), "tree-walker\n{}", text(&o.stderr));
        let k = kir_run(&["kir", "t.un", "--run", &arg.to_string()], &t.0);
        assert_eq!(k.status.code(), Some(101), "kir\n{}", text(&k.stderr));
        assert_eq!(text(&k.stderr).trim(), format!("panic: {}", msg), "kir");
    }
    let t = Tmp::new("kir_unwrap_enum_ok", &format!("{}{}", src, main_wrap("", "    core.check.eq(t.un(5), 7u32)\n")));
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0), "tree-walker main");
    let k = kir_run(&["kir", "t.un", "--run", "5"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir\n{}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "7", "kir value");
}

/// `core.check.eq` で `String` の中身を `as_str()` の `&str` として比べる (`String` 自体は `PartialEq` / `Debug`
/// を持たないので `core.check.eq` に渡せない。E2030)。等しいときは木走査と同じく通り、違うときは
/// 木走査と同じ文言 (`"ab"`) で 101 になる。
#[test]
fn kir_check_eq_on_string_matches_tree_walker() {
    let src = r#"fn t.mk_s(s: &str) -> Result<alloc.string.String<std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics never
    alloc.string.String.from_str(std.alloc.SYSTEM, s)
end t.mk_s

fn t.cmp(a: &str, b: &str) -> () uses [alloc] panics may
    let x = t.mk_s(a).unwrap()
    let y = t.mk_s(b).unwrap()
    core.check.eq(x.as_str(), y.as_str())
    core.mem.drop(x)
    core.mem.drop(y)
end t.cmp

fn t.same() -> () uses [alloc] panics may
    t.cmp("ab", "ab")
    t.cmp("", "")
end t.same

fn t.differs() -> () uses [alloc] panics may
    t.cmp("ab", "ac")
end t.differs

"#;
    let main = "fn t.main() -> Result<(), std.io.Error> uses [io.console, alloc] panics may\n    t.same()\n    t.differs()\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_check_eq_string", &format!("{}{}", src, main));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101), "tree-walker\n{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("panic: assertion failed: left: \"ab\", right: \"ac\""), "tree-walker\n{}", text(&o.stderr));
    let k = kir_run(&["kir", "t.same", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir same\n{}", text(&k.stderr));
    let k = kir_run(&["kir", "t.differs", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(101), "kir differs\n{}", text(&k.stderr));
    assert_eq!(text(&k.stderr).trim(), "panic: assertion failed: left: \"ab\", right: \"ac\"", "kir differs");
}

/// コピーできる集成体の要素を添字で読む形 (`tests/data/linear_ok/index_field_copy.ksnl` の `a[0usize].x`)。
/// 要素の値を読んでフィールドを取る・値として束縛する・関数の引数に渡す・入れ子のフィールド・`&Vec` 越し・ローカルの `Vec`・`for` の中で読む。
const KIR_COPY_ELEM_FNS: &str = r#"type t.Point derive [Debug, Clone, Copy] pub
    x: u32
    y: u32
end t.Point

type t.Seg derive [Debug, Clone, Copy] pub
    p: t.Point
    n: u32
end t.Seg

fn t.area(p: t.Point) -> u32 uses [] panics may
    p.x * p.y
end t.area

fn t.field_at(n: usize) -> u32 uses [] panics may
    let a = [t.Point { x: 1u32, y: 2u32 }, t.Point { x: 10u32, y: 20u32 }]
    a[n].x + a[n].y
end t.field_at

fn t.whole_at(n: usize) -> u32 uses [] panics may
    let a = [t.Point { x: 1u32, y: 2u32 }, t.Point { x: 10u32, y: 20u32 }]
    let b = a[n]
    b.x * b.y
end t.whole_at

fn t.nested_at(n: usize) -> u32 uses [] panics may
    let s = [
        t.Seg { p: t.Point { x: 3u32, y: 4u32 }, n: 5u32 },
        t.Seg { p: t.Point { x: 6u32, y: 7u32 }, n: 8u32 },
    ]
    s[n].p.y + s[n].n
end t.nested_at

fn t.pass_at(n: usize) -> u32 uses [] panics may
    let a = [t.Point { x: 2u32, y: 3u32 }, t.Point { x: 4u32, y: 5u32 }]
    t.area(a[n])
end t.pass_at

fn t.vec_at(n: usize) -> u32 uses [alloc] panics may
    let mut v: alloc.vec.Vec<t.Point, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let p = t.Point { x: 5u32, y: 6u32 }
    core.mem.drop(v.push(p))
    let q = t.Point { x: 7u32, y: 8u32 }
    core.mem.drop(v.push(q))
    v[n].x + v[n].y
end t.vec_at

fn t.sum_ref(v: &alloc.vec.Vec<t.Point, std.alloc.System>, n: usize) -> u32 uses [] panics may
    v[n].x + v[n].y
end t.sum_ref

fn t.ref_at(n: usize) -> u32 uses [alloc] panics may
    let mut v: alloc.vec.Vec<t.Point, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let p = t.Point { x: 5u32, y: 6u32 }
    core.mem.drop(v.push(p))
    let q = t.Point { x: 7u32, y: 8u32 }
    core.mem.drop(v.push(q))
    t.sum_ref(&v, n)
end t.ref_at

fn t.loop_sum(n: usize) -> u32 uses [] panics may
    let a = [t.Point { x: 1u32, y: 2u32 }, t.Point { x: 10u32, y: 20u32 }]
    let mut s = 0u32
    for i in core.iter.range(0usize, n) {
        s = s.wrapping_add(a[i].x)
    }
    s
end t.loop_sum

"#;

#[test]
fn kir_copy_elem_fields_match_tree_walker() {
    // コピーできる集成体の要素の読み (`a[n].x` / `let b = a[n]` / `a[n].p.y` / 引数に渡す) は、木走査と同じ値になる。
    // 以前は要素を `MemLoad` (スカラーだけ読める) で読み、下ろしの検査が `load は ptr から整数・...` で落ちた (設計_KIR.md 17.22)。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.field_at", &["0"], "3u32", "3"),
        ("t.field_at", &["1"], "30u32", "30"),
        ("t.whole_at", &["0"], "2u32", "2"),
        ("t.whole_at", &["1"], "200u32", "200"),
        ("t.nested_at", &["0"], "9u32", "9"),
        ("t.nested_at", &["1"], "15u32", "15"),
        ("t.pass_at", &["0"], "6u32", "6"),
        ("t.pass_at", &["1"], "20u32", "20"),
        ("t.vec_at", &["0"], "11u32", "11"),
        ("t.vec_at", &["1"], "15u32", "15"),
        ("t.ref_at", &["0"], "11u32", "11"),
        ("t.ref_at", &["1"], "15u32", "15"),
        ("t.loop_sum", &["0"], "0u32", "0"),
        ("t.loop_sum", &["2"], "11u32", "11"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.iter().map(|a| format!("{}usize", a)).collect::<Vec<_>>().join(", "));
        let main = format!(
            "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    core.check.eq({}, {})\n    Ok(())\nend t.main\n",
            call, lit
        );
        let t = Tmp::new(&format!("kir_copy_elem_{}", i), &format!("{}{}", KIR_COPY_ELEM_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}


// ---- KIR の上の借用検査 (隠しの切り替え `KSN_BORROWCK=kir`。設計_KIR.md 24 章) ----

/// `KSN_BORROWCK=kir ksn check` の結果: (KIR の検査の状態, KIR の診断の符号の列)。
/// 状態は `Some(codes)` (検査した。codes は KIR が出した E3010 / E3030 の列) か `None` (KIR に下ろせず判断できない)。
fn kir_bc(dir: &Path) -> Option<Vec<String>> {
    let cache = std::env::temp_dir().join(format!("ksn_kirbc_cache_{}", std::process::id()));
    let o = run_with_env(&["check", "--json"], dir, &[("KSN_BORROWCK", "kir"), ("KSN_CACHE_DIR", cache.to_str().unwrap())]);
    let err = text(&o.stderr);
    err.lines().find_map(|l| l.strip_prefix("kir-borrowck: checked").map(|r| r.split_whitespace().flat_map(|w| w.split(',')).filter(|c| !c.is_empty()).map(String::from).collect()))
}

fn ast_borrow_rejects(dir: &Path) -> bool {
    codes(&text(&run(&["check", "--json"], dir).stdout)).iter().any(|c| c == "E3010" || c == "E3030")
}

fn sorted_ksnl(sub: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(sub)).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "ksnl")).collect();
    v.sort();
    v
}

/// KIR の検査は、正しいコード (`linear_ok/`、`examples/`、参照解) に誤検出を出さない。KIR に下ろせないものは除く (その数を出す)。
#[test]
fn kir_borrowck_has_no_false_positives() {
    let mut fps = vec![];
    let (mut checked, mut unknown) = (0, 0);
    for p in sorted_ksnl("linear_ok") {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("kirbc_ok_{}", stem), &fs::read_to_string(&p).unwrap());
        match kir_bc(&t.0) {
            Some(c) if !c.is_empty() => fps.push(format!("linear_ok/{}: {:?}", stem, c)),
            Some(_) => checked += 1,
            None => unknown += 1,
        }
    }
    let bench = root().join("bench").join("tasks");
    let mut refs: Vec<PathBuf> = fs::read_dir(&bench).unwrap().map(|e| e.unwrap().path().join("reference.ksnl")).filter(|p| p.exists()).collect();
    refs.sort();
    for p in refs {
        let name = p.parent().unwrap().file_name().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("kirbc_ref_{}", name), &fs::read_to_string(&p).unwrap());
        match kir_bc(&t.0) {
            Some(c) if !c.is_empty() => fps.push(format!("reference {}: {:?}", name, c)),
            Some(_) => checked += 1,
            None => unknown += 1,
        }
    }
    let mut ex: Vec<PathBuf> = fs::read_dir(root().join("examples")).unwrap().map(|e| e.unwrap().path()).filter(|p| p.join("kasane.toml").exists()).collect();
    ex.sort();
    for d in ex {
        let name = d.file_name().unwrap().to_string_lossy().to_string();
        match kir_bc(&d) {
            Some(c) if !c.is_empty() => fps.push(format!("examples/{}: {:?}", name, c)),
            Some(_) => checked += 1,
            None => unknown += 1,
        }
    }
    eprintln!("kir_borrowck (correct code): checked {} unknown {} false positives {}", checked, unknown, fps.len());
    assert!(fps.is_empty(), "KIR borrowck false positives:\n{}", fps.join("\n"));
    // 下ろせる数が減らないこと (現在値 125 を下限にする。下ろしの対応が増えたら上げる)。
    assert!(checked >= 125, "only {} correct programs were checked on KIR", checked);
}

/// `unsound/` の全件で、AST の借用検査 (E3010 / E3030) と KIR の検査の結果を並べる。
/// 検査できた (KIR に下ろせた) もののうち、一致した数が下限を下回らないこと。header_lifetime_* のうち下ろせるものは KIR が止める。
#[test]
fn kir_borrowck_agreement_on_unsound_does_not_drop() {
    let (mut checked, mut agree, mut kir_reject, mut unknown) = (0, 0, 0, 0);
    let mut mism = vec![];
    for p in sorted_ksnl("unsound") {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("kirbc_{}", stem), &fs::read_to_string(&p).unwrap());
        match kir_bc(&t.0) {
            None => unknown += 1,
            Some(c) => {
                checked += 1;
                let kir = !c.is_empty();
                let ast = ast_borrow_rejects(&t.0);
                if kir {
                    kir_reject += 1;
                }
                if kir == ast {
                    agree += 1;
                } else {
                    mism.push(format!("{} (ast={} kir={})", stem, ast, kir));
                }
                if stem == "header_lifetime_return_other" || stem == "header_lifetime_return_static" {
                    assert!(kir, "{} must be rejected by the KIR borrowck", stem);
                }
            }
        }
    }
    eprintln!("kir_borrowck (unsound): checked {} agree {} kir_reject {} unknown {}; mismatches: {}", checked, agree, kir_reject, unknown, mism.join(", "));
    assert!(checked >= 197, "KIR checked only {} unsound programs", checked);
    assert!(agree >= 190, "agreement dropped to {} (mismatches: {})", agree, mism.join(", "));
    assert!(kir_reject >= 58, "KIR rejects only {}", kir_reject);
}

/// 借用検査の注釈 (`borrow.*` / `scope.end`。設計_KIR.md 24 章) は実行に影響しない: KIR のテキストに出て、
/// 参照実行器・最適化後・LLVM のネイティブの結果が同じ (往復と検査は `ksn kir` が毎回行う)。
#[test]
fn kir_borrow_annotations_do_not_change_execution() {
    let src = "fn t.f(n: u32) -> usize uses [alloc] panics never\n    let mut s = alloc.string.String.new(std.alloc.SYSTEM)\n    core.mem.drop(s.push_str(\"hello\"))\n    let r = &s\n    let k = t.peek(r)\n    let mut x = n\n    {\n        let y = &mut x\n        *y = 7u32\n    }\n    k.wrapping_add(t.peek(r))\nend t.f\n\nfn t.peek(s: &alloc.string.String) -> usize uses [] panics never\n    s.len()\nend t.peek\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_annot", src);
    let txt = text(&run(&["kir", "t.f"], &t.0).stdout);
    assert!(txt.contains("borrow.shared") && txt.contains("scope.end"), "annotations missing:\n{}", txt);
    for extra in [&["--run"][..], &["--opt", "--run"][..], &["--native"][..]] {
        let mut args = vec!["kir", "t.f"];
        args.extend_from_slice(extra);
        args.push("3");
        let o = run(&args, &t.0);
        assert_eq!(text(&o.stdout).trim(), "10", "{:?}: {}", extra, text(&o.stderr));
    }
    // 借用検査は、この正しいコードを通す。
    assert_eq!(kir_bc(&t.0), Some(vec![]));
}

/// 第 2 段で解けるようになった不一致 (move 中の借用・ブロックを出る借用・文の終わりの一時値) は KIR の検査が止める。
#[test]
fn kir_borrowck_stage2_catches_scope_and_move_cases() {
    for stem in ["borrow_then_consume_use_later", "borrow_then_branch_consume", "d05_block_value_borrow_escape", "temp_as_str_bound", "temp_method_owned_as_str", "d06_temporary_borrow", "d08_move_receiver_while_field_borrowed"] {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/unsound").join(format!("{}.ksnl", stem));
        let t = Tmp::new(&format!("kirbc2_{}", stem), &fs::read_to_string(&p).unwrap());
        let c = kir_bc(&t.0).unwrap_or_else(|| panic!("{} must be lowerable", stem));
        assert!(!c.is_empty(), "{} must be rejected by the KIR borrowck", stem);
    }
}

/// 第 3 段 (実験): 所有の注釈 (`own.*`) の上の線形性・move の検査 (E3002 / E3003)。隠しの切り替え `KSN_BORROWCK=kir,own` のときだけ動く
/// (既定の `kir` は借用だけ)。AST の結果 (E3002 / E3003) と並べ、一致数の下限と、正しいコードの誤検出 0 を固定する。
/// 注釈: 値の受け手 (`self` で受けるメソッド・`close` / `join`)、`with` の後始末 (束縛の消費)、`match` の腕の束縛、字段の受け手 (部分的な move)、
/// クロージャの `capture [move]`、`chan` の端の `close`、`Option` / `Result` の `map_err` / `unwrap`。
#[test]
fn kir_own_linearity_agreement_floor() {
    fn own_codes(dir: &Path) -> Option<Vec<String>> {
        let cache = std::env::temp_dir().join(format!("ksn_kirown_cache_{}", std::process::id()));
        let o = run_with_env(&["check", "--json"], dir, &[("KSN_BORROWCK", "kir,own"), ("KSN_CACHE_DIR", cache.to_str().unwrap())]);
        text(&o.stderr).lines().find_map(|l| l.strip_prefix("kir-borrowck: checked").map(|r| r.split_whitespace().flat_map(|w| w.split(',')).filter(|c| !c.is_empty()).map(String::from).collect()))
    }
    let lin = |cs: &[String]| cs.iter().any(|c| c == "E3002" || c == "E3003");
    // 既定の `kir` は線形性を出さない。
    let t = Tmp::new("kirown_default", &fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/unsound/move_in_loop.ksnl")).unwrap());
    assert!(!kir_bc(&t.0).unwrap_or_default().iter().any(|c| c == "E3002" || c == "E3003"));
    let (mut checked, mut agree, mut kir_reject) = (0, 0, 0);
    for p in sorted_ksnl("unsound") {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("kirown_{}", stem), &fs::read_to_string(&p).unwrap());
        let Some(k) = own_codes(&t.0) else { continue };
        checked += 1;
        let ast = codes(&text(&run(&["check", "--json"], &t.0).stdout)).iter().any(|c| c == "E3002" || c == "E3003");
        if lin(&k) {
            kir_reject += 1;
        }
        if lin(&k) == ast {
            agree += 1;
        }
    }
    // 正しいコードの誤検出は 0 件 (`linear_ok/`、参照解、`examples/` の全パッケージ)。
    let mut fps = vec![];
    let mut ok_checked = 0;
    for p in sorted_ksnl("linear_ok") {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("kirown_ok_{}", stem), &fs::read_to_string(&p).unwrap());
        match own_codes(&t.0) {
            Some(k) if lin(&k) => fps.push(format!("linear_ok/{}: {:?}", stem, k)),
            Some(_) => ok_checked += 1,
            None => {}
        }
    }
    let mut refs: Vec<PathBuf> = fs::read_dir(root().join("bench").join("tasks")).unwrap().map(|e| e.unwrap().path().join("reference.ksnl")).filter(|p| p.exists()).collect();
    refs.sort();
    for p in refs {
        let name = p.parent().unwrap().file_name().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("kirown_ref_{}", name), &fs::read_to_string(&p).unwrap());
        match own_codes(&t.0) {
            Some(k) if lin(&k) => fps.push(format!("reference {}: {:?}", name, k)),
            Some(_) => ok_checked += 1,
            None => {}
        }
    }
    let mut ex: Vec<PathBuf> = fs::read_dir(root().join("examples")).unwrap().map(|e| e.unwrap().path()).filter(|p| p.join("kasane.toml").exists()).collect();
    ex.sort();
    for d in ex {
        let name = d.file_name().unwrap().to_string_lossy().to_string();
        match own_codes(&d) {
            Some(k) if lin(&k) => fps.push(format!("examples/{}: {:?}", name, k)),
            Some(_) => ok_checked += 1,
            None => {}
        }
    }
    eprintln!("kir own (unsound): checked {} agree {} kir_reject {}; correct code checked {} false positives {:?}", checked, agree, kir_reject, ok_checked, fps);
    assert!(fps.is_empty(), "KIR linearity false positives on correct code:\n{}", fps.join("\n"));
    // 下限 (現在値。下げない): 検査できた不正・一致・KIR の拒否・正しいコードの検査数。
    assert!(checked >= 197, "KIR (own) checked only {} unsound programs", checked);
    assert!(agree >= 155, "agreement dropped to {}", agree);
    assert!(kir_reject >= 59, "KIR (own) rejects only {}", kir_reject);
    assert!(ok_checked >= 125, "only {} correct programs were checked by the KIR linearity", ok_checked);
}

/// 第 5 段 (計測): AST の借用・線形の検査を外し、KIR の検査だけ (隠しの切り替え `KSN_BORROWCK=kir-only`) で見る。
/// `// expect:` が E3010 / E3030 / E3002 / E3003 の `unsound/` (借用・線形の不正) のうち拒否できた数の下限と、
/// 正しいコード (`linear_ok/`、参照解、`examples/`) の誤拒否 0 を固定する。足りない件数と理由は設計_KIR.md 24.10。
#[test]
fn kir_only_rejects_floor() {
    fn only_rejects(dir: &Path) -> bool {
        let cache = std::env::temp_dir().join(format!("ksn_kironly_cache_{}", std::process::id()));
        let o = run_with_env(&["check", "--json"], dir, &[("KSN_BORROWCK", "kir-only"), ("KSN_CACHE_DIR", cache.to_str().unwrap())]);
        codes(&text(&o.stdout)).iter().any(|c| matches!(c.as_str(), "E3010" | "E3030" | "E3002" | "E3003"))
    }
    let (mut expected, mut rejected) = (0, 0);
    for p in sorted_ksnl("unsound") {
        let src = fs::read_to_string(&p).unwrap();
        let bl = src.lines().any(|l| l.starts_with("// expect:") && ["E3010", "E3030", "E3002", "E3003"].iter().any(|c| l.contains(c)));
        if !bl {
            continue;
        }
        expected += 1;
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("kironly_{}", stem), &src);
        if only_rejects(&t.0) {
            rejected += 1;
        }
    }
    let mut fps = vec![];
    for p in sorted_ksnl("linear_ok") {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("kironly_ok_{}", stem), &fs::read_to_string(&p).unwrap());
        if only_rejects(&t.0) {
            fps.push(format!("linear_ok/{}", stem));
        }
    }
    let mut refs: Vec<PathBuf> = fs::read_dir(root().join("bench").join("tasks")).unwrap().map(|e| e.unwrap().path().join("reference.ksnl")).filter(|p| p.exists()).collect();
    refs.sort();
    for p in refs {
        let name = p.parent().unwrap().file_name().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("kironly_ref_{}", name), &fs::read_to_string(&p).unwrap());
        if only_rejects(&t.0) {
            fps.push(format!("reference {}", name));
        }
    }
    let mut ex: Vec<PathBuf> = fs::read_dir(root().join("examples")).unwrap().map(|e| e.unwrap().path()).filter(|p| p.join("kasane.toml").exists()).collect();
    ex.sort();
    for d in ex {
        if only_rejects(&d) {
            fps.push(format!("examples/{}", d.file_name().unwrap().to_string_lossy()));
        }
    }
    eprintln!("kir-only: borrow/linear expected {} rejected {}; correct code rejected {:?}", expected, rejected, fps);
    assert!(fps.is_empty(), "kir-only rejects correct code:\n{}", fps.join("\n"));
    // 下限 (現在値。下げない)。
    assert!(expected >= 240, "only {} borrow/linear programs in unsound/", expected);
    assert!(rejected >= 112, "kir-only rejects only {} of {}", rejected, expected);
}
