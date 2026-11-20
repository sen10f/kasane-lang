//! `alloc.collections` の追加分: `BtreeMap` の `iter` (鍵の順)・`range` (半開区間)・`first` / `last`、
//! `HashSet<T>` (`new` `insert` `contains` `remove` `len` `is_empty` `iter`、`String` の要素は `*_str`)。
//! 木走査 (インタプリタ) と、ネイティブ (debug と release) の出力が一致することを見る (`agree_with_interpreter`)。
//! `String` の要素を `&k` で引く形は E2054 (書き方は 1 通り)。

mod common;
use common::*;

const MK: &str = r#"fn t.mk(text: &str) -> alloc.string.String uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(text))
    s
end t.mk

"#;

fn prog(body: &str) -> String {
    format!(
        "{}fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    core.mem.drop(std.io.print_line(\"ok\"))\n    Ok(())\nend t.main\n",
        MK, body
    )
}

#[test]
fn btreemap_iter_is_in_key_order() {
    let out = agree_with_interpreter(
        "cm_bt_iter",
        &prog(
            r#"    let mut b = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)
    core.mem.drop(b.insert(30u32, 3u32))
    core.mem.drop(b.insert(10u32, 1u32))
    core.mem.drop(b.insert(20u32, 2u32))
    core.mem.drop(b.insert(50u32, 5u32))
    let mut keys = 0u32
    let mut prev = 0u32
    let mut sorted = true
    let mut count = 0usize
    for (k, v) in b.iter() {
        if *k < prev {
            sorted = false
        }
        prev = *k
        keys = keys.wrapping_mul(10u32).wrapping_add(*v)
        count = count.wrapping_add(1usize)
    }
    core.check.eq(sorted, true)
    core.check.eq(keys, 1235u32)
    core.check.eq(count, 4usize)
"#,
        ),
    );
    assert_eq!(out.trim(), "ok");
}

#[test]
fn btreemap_range_is_half_open_and_first_last() {
    let out = agree_with_interpreter(
        "cm_bt_range",
        &prog(
            r#"    let mut b = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)
    core.mem.drop(b.insert(1u32, 10u32))
    core.mem.drop(b.insert(2u32, 20u32))
    core.mem.drop(b.insert(3u32, 30u32))
    core.mem.drop(b.insert(5u32, 50u32))
    let mut s = 0u32
    for (_k, v) in b.range(&2u32, &5u32) {
        s = s.wrapping_add(*v)
    }
    core.check.eq(s, 50u32)
    let mut all = 0u32
    for (_k, v) in b.range(&0u32, &100u32) {
        all = all.wrapping_add(*v)
    }
    core.check.eq(all, 110u32)
    let mut none = 0usize
    for (_k, _v) in b.range(&6u32, &9u32) {
        none = none.wrapping_add(1usize)
    }
    core.check.eq(none, 0usize)
    match b.first() {
        Some((k, v)) => {
            core.check.eq(*k, 1u32)
            core.check.eq(*v, 10u32)
        }
        None => core.check.eq(1u32, 2u32)
    }
    match b.last() {
        Some((k, v)) => {
            core.check.eq(*k, 5u32)
            core.check.eq(*v, 50u32)
        }
        None => core.check.eq(1u32, 2u32)
    }
"#,
        ),
    );
    assert_eq!(out.trim(), "ok");
}

#[test]
fn hashset_u32_insert_contains_remove_len() {
    let out = agree_with_interpreter(
        "cm_hs_u32",
        &prog(
            r#"    let mut s = alloc.collections.HashSet.new(std.alloc.SYSTEM)
    core.check.eq(s.is_empty(), true)
    core.check.eq(s.insert(7u32), Ok(true))
    core.check.eq(s.insert(7u32), Ok(false))
    core.check.eq(s.insert(9u32), Ok(true))
    core.check.eq(s.len(), 2usize)
    core.check.eq(s.contains(&7u32), true)
    core.check.eq(s.contains(&8u32), false)
    core.check.eq(s.remove(&7u32), true)
    core.check.eq(s.remove(&7u32), false)
    core.check.eq(s.len(), 1usize)
    core.check.eq(s.is_empty(), false)
    let mut n = 0usize
    for _x in s.iter() {
        n = n.wrapping_add(1usize)
    }
    core.check.eq(n, 1usize)
"#,
        ),
    );
    assert_eq!(out.trim(), "ok");
}

#[test]
fn hashset_string_elements_use_str_forms() {
    let out = agree_with_interpreter(
        "cm_hs_str",
        &prog(
            r#"    let mut s = alloc.collections.HashSet.new(std.alloc.SYSTEM)
    core.check.eq(s.insert(t.mk("pear")), Ok(true))
    core.check.eq(s.insert(t.mk("apple")), Ok(true))
    core.check.eq(s.insert(t.mk("pear")), Ok(false))
    core.check.eq(s.len(), 2usize)
    core.check.eq(s.contains_str("apple"), true)
    core.check.eq(s.contains_str("fig"), false)
    core.check.eq(s.remove_str("pear"), true)
    core.check.eq(s.remove_str("pear"), false)
    core.check.eq(s.len(), 1usize)
    let mut n = 0usize
    for _k in s.iter() {
        n = n.wrapping_add(1usize)
    }
    core.check.eq(n, 1usize)
"#,
        ),
    );
    assert_eq!(out.trim(), "ok");
}

#[test]
fn hashset_string_contains_without_str_is_e2054() {
    let src = prog(
        r#"    let mut s = alloc.collections.HashSet.new(std.alloc.SYSTEM)
    core.check.eq(s.insert(t.mk("pear")), Ok(true))
    core.check.eq(s.contains(&t.mk("pear")), true)
"#,
    );
    let t = Tmp::new("cm_hs_e2054", &src);
    let chk = run(&["check"], &t.0);
    let out = text(&chk.stdout);
    assert!(out.contains("E2054"), "{}", out);
}
