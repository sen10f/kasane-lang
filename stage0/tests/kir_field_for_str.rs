//! KIR → LLVM の下ろしの試験: フィールド経由の `for` (設計_KIR.md 28 章) と文字列 API (同 29 章)。
//!
//! どの試験も、`ksn run` (木走査) の標準出力と、`ksn build --release` で作ったネイティブの実行ファイルの
//! 標準出力と終了コードが一致することを確かめる。出力は `名前` と `ok` / `NG` の組で、`NG` が出たら失敗。

#![allow(dead_code, unused_imports)]

mod common;

use std::process::Command;

use common::*;

/// `ksn run` の出力と、ネイティブの実行ファイルの出力 (標準出力) を返す。どちらも終了コード 0 でなければ失敗。
fn run_and_native(name: &str, src: &str) -> (String, String) {
    let (t, found, out) = build_native_strict(name, src, true);
    let Some(p) = found else {
        panic!("ネイティブの実行ファイルが作れない (下ろせない形がある): {}", out);
    };
    let r = run(&["run"], &t.0);
    assert_eq!(r.status.code(), Some(0), "ksn run: {}{}", text(&r.stdout), text(&r.stderr));
    let n = Command::new(&p).current_dir(&t.0).output().unwrap();
    assert_eq!(n.status.code(), Some(0), "ネイティブ: {}{}", text(&n.stdout), text(&n.stderr));
    (text(&r.stdout), text(&n.stdout))
}

/// `run` と native の出力が同じで、`NG` が無く、`ok` が `want` 件あること。
fn assert_same_and_ok(name: &str, src: &str, want: usize) {
    let (r, n) = run_and_native(name, src);
    assert_eq!(r, n, "run と native の出力が違う");
    assert!(!n.lines().any(|l| l == "NG"), "NG が出た:\n{}", n);
    assert_eq!(n.lines().filter(|l| *l == "ok").count(), want, "ok の件数:\n{}", n);
}

// ---------------- フィールド経由の for ----------------

/// 構造体 (`t.Bag`) の Vec のフィールドを、`for` で `&self` / `&mut self` / 局所の参照から回す。
/// 要素は構造体 (`t.Item` は `String` を持つ)・`String`・`Vec`・`Copy` の構造体・`usize`。
const FIELD_SRC: &str = "type t.Bag
    items: alloc.vec.Vec<t.Item, std.alloc.System>
    names: alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>
    subs: alloc.vec.Vec<alloc.vec.Vec<u32, std.alloc.System>, std.alloc.System>
    pts: alloc.vec.Vec<t.Point, std.alloc.System>
    nums: alloc.vec.Vec<usize, std.alloc.System>
    inner: t.Inner
    tot: u32
end t.Bag

type t.Inner
    names: alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>
end t.Inner

type t.Item
    name: alloc.string.String<std.alloc.System>
    n: u32
end t.Item

type t.Point derive [Copy, Clone]
    x: u32
    y: u32
end t.Point

fn t.check(name: &str, ok: bool) -> Result<(), std.io.Error> uses [io.console] panics may
    std.io.print_line(name)?
    if ok {
        std.io.print_line(\"ok\")?
    } else {
        std.io.print_line(\"NG\")?
    }
    Ok(())
end t.check

fn t.mk_name(s: &str) -> alloc.string.String<std.alloc.System> uses [alloc] panics may
    alloc.string.String.from_str(std.alloc.SYSTEM, s).unwrap()
end t.mk_name

fn t.Bag.bump(self: &mut t.Bag) -> u32 uses [] panics never
    for it in &mut self.items {
        it.n = it.n.wrapping_add(1u32)
    }
    self.sum()
end t.Bag.bump

fn t.Bag.count_tot(self: &mut t.Bag) -> u32 uses [] panics never
    for it in self.items.as_slice().iter() {
        self.tot = self.tot.wrapping_add(it.n)
    }
    self.tot
end t.Bag.count_tot

fn t.Bag.inner_len(self: &t.Bag) -> usize uses [] panics never
    let mut acc = 0usize
    for s in self.inner.names.iter() {
        acc = acc.wrapping_add(s.len())
    }
    acc
end t.Bag.inner_len

fn t.Bag.lens(self: &t.Bag) -> usize uses [] panics never
    let mut acc = 0usize
    for s in self.names.iter() {
        acc = acc.wrapping_add(s.len())
    }
    acc
end t.Bag.lens

fn t.Bag.nums_sum(self: &t.Bag) -> usize uses [] panics never
    let mut acc = 0usize
    for n in self.nums.as_slice().iter() {
        acc = acc.wrapping_add(*n)
    }
    acc
end t.Bag.nums_sum

fn t.Bag.px(self: &t.Bag) -> u32 uses [] panics never
    let mut acc = 0u32
    for d in self.pts.iter() {
        let q = *d
        acc = acc.wrapping_add(q.x)
    }
    acc
end t.Bag.px

fn t.Bag.sub_len(self: &t.Bag) -> usize uses [] panics never
    let mut acc = 0usize
    for v in &self.subs {
        acc = acc.wrapping_add(v.len())
    }
    acc
end t.Bag.sub_len

fn t.Bag.sum(self: &t.Bag) -> u32 uses [] panics never
    let mut acc = 0u32
    for it in self.items.as_slice().iter() {
        acc = acc.wrapping_add(it.n)
    }
    acc
end t.Bag.sum

fn t.Bag.ys(self: &t.Bag) -> u32 uses [] panics never
    let mut acc = 0u32
    for d in self.pts.iter() {
        acc = acc.wrapping_add(d.y)
    }
    acc
end t.Bag.ys

fn t.bag() -> t.Bag uses [alloc] panics may
    let i0 = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let n0 = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let s0 = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let p0 = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let u0 = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let in0 = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let inn = t.Inner { names: in0 }
    let z = 0u32
    let mut b = t.Bag { items: i0, names: n0, subs: s0, pts: p0, nums: u0, inner: inn, tot: z }
    core.mem.drop(b.items.push(t.Item { name: t.mk_name(\"a\"), n: 3u32 }))
    core.mem.drop(b.names.push(t.mk_name(\"abcd\")))
    core.mem.drop(b.pts.push(t.Point { x: 5u32, y: 6u32 }))
    core.mem.drop(b.nums.push(7usize))
    core.mem.drop(b.inner.names.push(t.mk_name(\"xy\")))
    let mut sub = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(sub.push(1u32))
    core.mem.drop(sub.push(2u32))
    core.mem.drop(b.subs.push(sub))
    b
end t.bag

fn t.total_len(b: &t.Bag) -> usize uses [] panics never
    let mut acc = 0usize
    for s in b.names.iter() {
        acc = acc.wrapping_add(s.len())
    }
    acc
end t.total_len

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut b = t.bag()
    t.check(\"sum\", b.sum() == 3u32)?
    t.check(\"lens\", b.lens() == 4usize)?
    t.check(\"sub_len\", b.sub_len() == 2usize)?
    t.check(\"px\", b.px() == 5u32)?
    t.check(\"ys\", b.ys() == 6u32)?
    t.check(\"nums_sum\", b.nums_sum() == 7usize)?
    t.check(\"inner_len\", b.inner_len() == 2usize)?
    t.check(\"total_len\", t.total_len(&b) == 4usize)?
    t.check(\"bump\", b.bump() == 4u32)?
    t.check(\"sum_after_bump\", b.sum() == 4u32)?
    t.check(\"count_tot\", b.count_tot() == 4u32)?
    Ok(())
end t.main
";

#[test]
fn field_for_struct_string_vec_and_copy_elements_native_matches_run() {
    // 上の検査は 11 件。
    assert_same_and_ok("kff_field", FIELD_SRC, 11);
}

/// 反復中に同じ Vec を変える形 (`push`) は、ネイティブの実行ファイルを作らない (黙って作らない)。
/// 今は借用の検査 (E3010) が先に止める。下ろしの側の拒否 (E9040) も残っていれば、どちらかで止まればよい。
#[test]
fn field_for_body_that_mutates_the_iterated_vec_is_not_native() {
    let grow = "fn t.Bag.grow(self: &mut t.Bag) -> usize uses [alloc] panics may
    for _ in self.names.iter() {
        core.mem.drop(self.names.push(t.mk_name(\"z\")))
    }
    self.names.len()
end t.Bag.grow

fn t.total_len(b: &t.Bag)";
    let src = FIELD_SRC
        .replace("fn t.total_len(b: &t.Bag)", grow)
        .replace("    t.check(\"sum\", b.sum() == 3u32)?", "    core.check.eq(b.grow(), 0usize)\n    t.check(\"sum\", b.sum() == 3u32)?");
    let (_t, found, out) = build_native_strict("kff_grow", &src, true);
    assert!(found.is_none(), "反復中の変更がネイティブになった: {}", out);
    assert!(out.contains("E3010") || out.contains("E9040"), "止めた理由が出ていない: {}", out);
}

// ---------------- 文字列 API ----------------

/// 文字列 API の境界値。`rfind`、`split` / `chars` の `for` の形 (反復の列を `for` で数える)、既存の API の代表。
/// 値の反復子 (`let mut it = s.split(p)`) は下ろさない (`kir_native.rs` の試験が、インタプリタだけと固定している)。
const STR_SRC: &str = "fn t.check(name: &str, ok: bool) -> Result<(), std.io.Error> uses [io.console] panics may
    std.io.print_line(name)?
    if ok {
        std.io.print_line(\"ok\")?
    } else {
        std.io.print_line(\"NG\")?
    }
    Ok(())
end t.check

fn t.chars_count(s: &str) -> usize uses [] panics never
    let mut n = 0usize
    for _ in s.chars() {
        n = n.wrapping_add(1usize)
    }
    n
end t.chars_count

fn t.chars_sum(s: &str) -> u32 uses [] panics never
    let mut acc = 0u32
    for c in s.chars() {
        acc = acc.wrapping_add(c.to_u32())
    }
    acc
end t.chars_sum

fn t.chars_for_count(s: &str) -> usize uses [] panics never
    let mut n = 0usize
    for _ in s.chars() {
        n = n.wrapping_add(1usize)
    }
    n
end t.chars_for_count

fn t.rf(s: &str, p: &str) -> usize uses [] panics never
    match s.rfind(p) {
        Some(i) => i
        None => 99usize
    }
end t.rf

fn t.fnd(s: &str, p: &str) -> usize uses [] panics never
    match s.find(p) {
        Some(i) => i
        None => 99usize
    }
end t.fnd

fn t.split_count(s: &str, sep: &str) -> usize uses [] panics never
    let mut n = 0usize
    for _ in s.split(sep) {
        n = n.wrapping_add(1usize)
    }
    n
end t.split_count

fn t.split_for_count(s: &str, sep: &str) -> usize uses [] panics never
    let mut n = 0usize
    for _ in s.split(sep) {
        n = n.wrapping_add(1usize)
    }
    n
end t.split_for_count

fn t.split_lens(s: &str, sep: &str) -> usize uses [] panics never
    let mut n = 0usize
    for p in s.split(sep) {
        n = n.wrapping_add(p.len())
    }
    n
end t.split_lens

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    t.check(\"rfind empty pattern\", t.rf(\"abc\", \"\") == 3usize)?
    t.check(\"rfind last of several\", t.rf(\"abcabc\", \"bc\") == 4usize)?
    t.check(\"rfind overlapping\", t.rf(\"aaa\", \"aa\") == 1usize)?
    t.check(\"rfind at start\", t.rf(\"abc\", \"a\") == 0usize)?
    t.check(\"rfind whole\", t.rf(\"abc\", \"abc\") == 0usize)?
    t.check(\"rfind absent\", t.rf(\"abc\", \"x\") == 99usize)?
    t.check(\"rfind longer pattern\", t.rf(\"abc\", \"abcd\") == 99usize)?
    t.check(\"rfind empty in empty\", t.rf(\"\", \"\") == 0usize)?
    t.check(\"split trailing sep\", t.split_count(\"a,b,\", \",\") == 3usize)?
    t.check(\"split leading sep\", t.split_count(\",a\", \",\") == 2usize)?
    t.check(\"split consecutive\", t.split_count(\",,\", \",\") == 3usize)?
    t.check(\"split empty string\", t.split_count(\"\", \",\") == 1usize)?
    t.check(\"split empty sep\", t.split_count(\"abc\", \"\") == 1usize)?
    t.check(\"split longer sep\", t.split_count(\"abc\", \"abcd\") == 1usize)?
    t.check(\"split two-char sep\", t.split_count(\"a::b\", \"::\") == 2usize)?
    t.check(\"split piece lengths\", t.split_lens(\"a,bc,d\", \",\") == 4usize)?
    t.check(\"split for trailing\", t.split_for_count(\"a,b,\", \",\") == 3usize)?
    t.check(\"chars count utf8\", t.chars_count(\"aé日😀\") == 4usize)?
    t.check(\"chars code sum\", t.chars_sum(\"aé日😀\") == 154927u32)?
    t.check(\"chars empty\", t.chars_count(\"\") == 0usize)?
    t.check(\"chars ascii\", t.chars_sum(\"ab\") == 195u32)?
    t.check(\"chars for count\", t.chars_for_count(\"aé日😀\") == 4usize)?
    t.check(\"contains\", \"a,bc\".contains(\"bc\"))?
    t.check(\"find\", t.fnd(\"a,bc,d\", \",\") == 1usize)?
    Ok(())
end t.main
";

#[test]
fn str_api_native_matches_run() {
    // 上の検査は 24 件。
    assert_same_and_ok("kff_str", STR_SRC, 24);
}
