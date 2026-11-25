//! `BtreeMap` (アリーナの上の treap、O(log n)) を、整列した `Vec` の参照と乱数の操作で突き合わせる。
//! 木走査・VM・ネイティブ (debug と release) の出力が一致することを見る。

mod common;
use common::*;

const SRC: &str = r#"fn t.same(b: &alloc.collections.BtreeMap<u64, u64, std.alloc.System>, r: &alloc.vec.Vec<u64, std.alloc.System>) -> bool uses [] panics never
    if b.len() != r.len() {
        return false
    }
    let mut i = 0usize
    for (k, v) in b.iter() {
        match r.get(i) {
            Some(x) => {
                if *x != *k || *v != x.wrapping_mul(3u64) {
                    return false
                }
            }
            None => return false
        }
        i = i.wrapping_add(1usize)
    }
    i == r.len()
end t.same

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut b = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)
    let mut r: alloc.vec.Vec<u64, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut s = 88172645463325252u64
    let mut bad = 0u32
    let mut n = 0u32
    while n < 6000u32 {
        s = s ^ s.wrapping_shl(13u32)
        s = s ^ s.wrapping_shr(7u32)
        s = s ^ s.wrapping_shl(17u32)
        let k = s % 300u64
        let op = s.wrapping_shr(20u32) % 3u64
        let mut pos = 0usize
        let mut eq = false
        while pos < r.len() {
            match r.get(pos) {
                Some(x) => {
                    if *x < k {
                        pos = pos.wrapping_add(1usize)
                    } else {
                        if *x == k {
                            eq = true
                        }
                        break
                    }
                }
                None => break
            }
        }
        if op < 2u64 {
            match b.insert(k, k.wrapping_mul(3u64)) {
                Ok(old) => {
                    if (old is Some(_)) != eq {
                        bad = bad.wrapping_add(1u32)
                    }
                }
                Err(_) => {
                    bad = bad.wrapping_add(1u32)
                }
            }
            if !eq {
                core.mem.drop(r.insert(pos, k))
            }
        } else {
            let got = b.remove(&k)
            if (got is Some(_)) != eq {
                bad = bad.wrapping_add(1u32)
            }
            if eq {
                core.mem.drop(r.remove(pos))
            }
        }
        if n % 25u32 == 0u32 && !t.same(&b, &r) {
            bad = bad.wrapping_add(1u32)
        }
        n = n.wrapping_add(1u32)
    }
    if !t.same(&b, &r) {
        bad = bad.wrapping_add(1u32)
    }
    let mut inside = 0usize
    for (_k, _v) in b.range(&100u64, &200u64) {
        inside = inside.wrapping_add(1usize)
    }
    let mut expect = 0usize
    let mut j = 0usize
    while j < r.len() {
        match r.get(j) {
            Some(x) => {
                if *x >= 100u64 && *x < 200u64 {
                    expect = expect.wrapping_add(1usize)
                }
            }
            None => {
            }
        }
        j = j.wrapping_add(1usize)
    }
    core.check.eq(inside, expect)
    core.check.eq(bad, 0u32)
    core.check.eq(b.len() > 50usize, true)
    core.mem.drop(std.io.print_line("ok"))
    Ok(())
end t.main
"#;

#[test]
fn btreemap_matches_sorted_vec_reference() {
    let out = agree_with_interpreter("bt_treap", SRC);
    assert_eq!(out.trim(), "ok");
}

#[test]
fn btreemap_treap_tree_engine() {
    let t = Tmp::new("bt_treap_tree", SRC);
    let o = run(&["run", "--engine=tree"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "ok");
}
