//! 段階 1 の下ろしの穴 (その 5、E9040 の再調査): 空の配列リテラル、`Result.map` / `and_then` / `ok`、`String.join`、
//! `slice.sort` の `String` の要素、`enumerate` と `Vec.iter()` を元にした反復子の連なり、`Vec<Box<スカラー>>.push`、
//! `&dyn` の引数、`std.json.to_string`。
//! 木走査 (`ksn run`) と LLVM (debug / release) の出力が一致し、release がバンドルへ退避しないことを確かめる。

mod common;
use common::*;

fn assert_native_release(name: &str, src: &str) {
    let (_t, exe, out) = build_native_strict(&format!("{}_strict", name), src, true);
    assert!(exe.is_some(), "release のネイティブ生成が失敗した: {}", out);
}

fn check(name: &str, src: &str) -> String {
    let out = agree_with_interpreter(name, src);
    assert_native_release(name, src);
    out
}

/// 空の配列リテラル (`&[]` の引数・型注釈つきの `[]`・from_utf8)
#[test]
fn native_g5_empty_array() {
    let out = check("g5_empty_array", r#"fn t.total(xs: &[u32]) -> u32 uses [] panics never
    let mut s = 0u32
    for x in xs.iter() {
        s = s.wrapping_add(*x)
    }
    s
end t.total

fn t.count(xs: &[u8]) -> usize uses [] panics never
    xs.len()
end t.count

fn t.p_u64(x: u64) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &x))
    std.io.print_line(s.as_str())?
    Ok(())
end t.p_u64

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    t.p_u64(u64.from_u32(t.total(&[])))?
    t.p_u64(u64.from_u32(t.total(&[1u32, 2u32, 4000000000u32])))?
    t.p_u64(u64.from_usize(t.count(&[])))?
    let e: [u8; 0] = []
    t.p_u64(u64.from_usize(t.count(&e)))?
    match core.str.from_utf8(&[]) {
        Ok(s) => t.p_u64(u64.from_usize(s.len()))?
        Err(_) => std.io.print_line("err")?
    }
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// `Result.map` / `and_then` / `ok` (Ok と Err、局所と式の受け手、所有物のエラー)
#[test]
fn native_g5_result_map() {
    let out = check("g5_result_map", r#"fn t.half(x: u32) -> Result<u32, i64> uses [] panics may
    if x % 2u32 == 0u32 {
        Ok(x / 2u32)
    } else {
        Err(-5i64)
    }
end t.half

fn t.inc(x: u32) -> u32 uses [] panics never
    x.wrapping_add(1u32)
end t.inc

fn t.mk(x: u32) -> Result<u32, i64> uses [] panics never
    if x > 100u32 {
        Err(-1i64)
    } else {
        Ok(x)
    }
end t.mk

fn t.name(x: u32) -> Result<u32, alloc.string.String<std.alloc.System>> uses [alloc] panics never
    if x > 100u32 {
        match alloc.string.String.from_str(std.alloc.SYSTEM, "too big") {
            Ok(s) => Err(s)
            Err(_) => Ok(0u32)
        }
    } else {
        Ok(x)
    }
end t.name

fn t.p_i64(x: i64) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &x))
    std.io.print_line(s.as_str())?
    Ok(())
end t.p_i64

fn t.show(r: Result<u32, i64>) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    match r {
        Ok(v) => t.p_i64(i64.from_u32(v))?
        Err(e) => t.p_i64(e)?
    }
    Ok(())
end t.show

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let r: Result<u32, i64> = Ok(7u32)
    t.show(r.map(t.inc))?
    let r2: Result<u32, i64> = Err(-3i64)
    t.show(r2.map(fn(x: u32) -> u32 capture [] {
        x.wrapping_add(10u32)
    }))?
    t.show(t.mk(8u32).and_then(t.half))?
    t.show(t.mk(9u32).and_then(t.half))?
    t.show(t.mk(500u32).and_then(t.half))?
    t.show(t.mk(4294967295u32).map(t.inc))?
    match t.mk(3u32).ok() {
        Some(v) => t.p_i64(i64.from_u32(v))?
        None => std.io.print_line("none")?
    }
    match t.mk(300u32).ok() {
        Some(v) => t.p_i64(i64.from_u32(v))?
        None => std.io.print_line("none")?
    }
    let d = t.mk(4u32)
    match d.ok() {
        Some(v) => t.p_i64(i64.from_u32(v))?
        None => std.io.print_line("none")?
    }
    match t.name(5u32).map(t.inc) {
        Ok(v) => t.p_i64(i64.from_u32(v))?
        Err(e) => std.io.print_line(e.as_str())?
    }
    match t.name(500u32).map(t.inc) {
        Ok(v) => t.p_i64(i64.from_u32(v))?
        Err(e) => std.io.print_line(e.as_str())?
    }
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// `String.join` (複数・1 つ・空・マルチバイト・長い連結)
#[test]
fn native_g5_string_join() {
    let out = check("g5_string_join", r#"fn t.show(r: Result<alloc.string.String<std.alloc.System>, alloc.alloc.AllocError>) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    match r {
        Ok(s) => {
            std.io.print_line(s.as_str())?
            let mut n = alloc.string.String.new(std.alloc.SYSTEM)
            core.mem.drop(n.push_u64(u64.from_usize(s.len())))
            std.io.print_line(n.as_str())?
        }
        Err(_) => std.io.print_line("err")?
    }
    Ok(())
end t.show

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let parts = ["one", "two", "three"]
    t.show(alloc.string.String.join(std.alloc.SYSTEM, &parts, ", "))?
    let one = ["x"]
    t.show(alloc.string.String.join(std.alloc.SYSTEM, &one, ", "))?
    let none: [&str; 0] = []
    t.show(alloc.string.String.join(std.alloc.SYSTEM, &none, ","))?
    let mp = ["é", "日", "本"]
    t.show(alloc.string.String.join(std.alloc.SYSTEM, &mp, "・"))?
    t.show(alloc.string.String.join(std.alloc.SYSTEM, &parts, ""))?
    let many = ["abcdefghij", "klmnopqrst", "uvwxyz0123", "4567890abc", "defghijklm", "nopqrstuvw"]
    t.show(alloc.string.String.join(std.alloc.SYSTEM, &many, "--"))?
    let blanks = ["", "", ""]
    t.show(alloc.string.String.join(std.alloc.SYSTEM, &blanks, "|"))?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// `slice.sort` / `sort_by` を `String` の要素で (13 個以上で併合の経路も通る)
#[test]
fn native_g5_sort_strings() {
    let out = check("g5_sort_strings", r#"fn t.add(v: &mut alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>, text: &str) -> () uses [alloc] panics never
    match alloc.string.String.from_str(std.alloc.SYSTEM, text) {
        Ok(s) => core.mem.drop(v.push(s))
        Err(e) => core.mem.drop(e)
    }
end t.add

fn t.by_len(a: &alloc.string.String<std.alloc.System>, b: &alloc.string.String<std.alloc.System>) -> core.cmp.Ordering uses [] panics never
    a.len().cmp(&b.len())
end t.by_len

fn t.dump(v: &alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut out = alloc.string.String.new(std.alloc.SYSTEM)
    for s in v.iter() {
        core.mem.drop(out.push_str(s.as_str()))
        core.mem.drop(out.push(','))
    }
    std.io.print_line(out.as_str())?
    Ok(())
end t.dump

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    t.add(&mut v, "pear")
    t.add(&mut v, "apple")
    t.add(&mut v, "")
    t.add(&mut v, "fig")
    t.add(&mut v, "apple")
    t.add(&mut v, "zebra")
    t.add(&mut v, "banana")
    t.add(&mut v, "Apple")
    t.add(&mut v, "cherry")
    t.add(&mut v, "date")
    t.add(&mut v, "elder")
    t.add(&mut v, "grape")
    t.add(&mut v, "kiwi")
    t.add(&mut v, "lemon")
    t.add(&mut v, "mango")
    t.add(&mut v, "apple pie")
    v.as_mut_slice().sort()
    t.dump(&v)?
    v.as_mut_slice().sort_by(t.by_len)
    t.dump(&v)?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// `enumerate` と `Vec.iter()` を元にした `fold` / `count`、組の累積
#[test]
fn native_g5_iter_forms() {
    let out = check("g5_iter_forms", r#"fn t.p_u64(x: u64) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &x))
    std.io.print_line(s.as_str())?
    Ok(())
end t.p_u64

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut nums = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(nums.push(5u64))
    core.mem.drop(nums.push(9u64))
    core.mem.drop(nums.push(2u64))
    core.mem.drop(nums.push(7u64))
    let a = core.iter.fold(nums.iter(), 0u64, fn(acc: u64, x: &u64) -> u64 capture [] {
        acc.wrapping_add(*x)
    })
    t.p_u64(a)?
    let b = core.iter.fold(
        core.iter.enumerate(nums.iter()),
        0u64,
        fn(acc: u64, p: (usize, &u64)) -> u64 capture [] {
            let (i, v) = p
            acc.wrapping_add(u64.from_usize(i).wrapping_mul(*v))
        },
    )
    t.p_u64(b)?
    let c = core.iter.count(core.iter.enumerate(core.iter.range(10u64, 20u64)))
    t.p_u64(u64.from_usize(c))?
    let d = core.iter.fold(
        core.iter.filter(
            core.iter.enumerate(core.iter.range(0u64, 10u64)),
            fn(p: &(usize, u64)) -> bool capture [] {
                let (i, _) = *p
                i % 3usize == 0usize
            },
        ),
        0u64,
        fn(acc: u64, p: (usize, u64)) -> u64 capture [] {
            let (_, v) = p
            acc.wrapping_add(v)
        },
    )
    t.p_u64(d)?
    let e = core.iter.fold(
        core.iter.take(core.iter.enumerate(core.iter.range(100u64, 200u64)), 4usize),
        0u64,
        fn(acc: u64, p: (usize, u64)) -> u64 capture [] {
            let (i, v) = p
            acc.wrapping_add(v).wrapping_add(u64.from_usize(i))
        },
    )
    t.p_u64(e)?
    let f = core.iter.fold(
        core.iter.range(0u64, 5u64),
        (0u64, 1u64),
        fn(acc: (u64, u64), x: u64) -> (u64, u64) capture [] {
            let (s, m) = acc
            (s.wrapping_add(x), m.wrapping_mul(x.wrapping_add(1u64)))
        },
    )
    let (f0, f1) = f
    t.p_u64(f0)?
    t.p_u64(f1)?
    let empty: alloc.vec.Vec<u64, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    t.p_u64(u64.from_usize(core.iter.count(empty.iter())))?
    let sl = nums.as_slice()
    t.p_u64(core.iter.fold(sl.iter(), 1u64, fn(acc: u64, x: &u64) -> u64 capture [] {
        acc.wrapping_mul(*x)
    }))?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// `Vec<Box<u32>>.push` と `Vec<Box<u8>>`
#[test]
fn native_g5_vec_box() {
    let out = check("g5_vec_box", r#"fn t.fill(v: &mut alloc.vec.Vec<alloc.boxed.Box<u32, std.alloc.System>, std.alloc.System>, n: u32) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never
    let mut i = 0u32
    while i < n {
        let b = alloc.boxed.Box.new(std.alloc.SYSTEM, i.wrapping_mul(7u32))?
        v.push(b)?
        i = i.wrapping_add(1u32)
    }
    Ok(())
end t.fill

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    match t.fill(&mut v, 20u32) {
        Ok(()) => {
        }
        Err(e) => core.mem.drop(e)
    }
    let mut sum = 0u32
    for b in v.iter() {
        sum = sum.wrapping_add(*b.as_ref())
    }
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    match s.push_u64(u64.from_u32(sum)) {
        Ok(_) => {
        }
        Err(e) => core.mem.drop(e)
    }
    std.io.print_line(s.as_str())?
    let mut w = alloc.vec.Vec.new(std.alloc.SYSTEM)
    match alloc.boxed.Box.new(std.alloc.SYSTEM, 5u8) {
        Ok(bx) => match w.push(bx) {
            Ok(()) => {
            }
            Err(e) => core.mem.drop(e)
        }
        Err(e) => core.mem.drop(e)
    }
    std.io.print_line("vec_box_scalar ok")?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// `&dyn Tr` / `&mut dyn Tr` を引数に取る関数 (2 つの実装・let の型注釈・Box<dyn> の as_ref)
#[test]
fn native_g5_dyn_ref() {
    let out = check("g5_dyn_ref", r#"trait t.Tr[E: uses, P: panics] pub
    fn m(self: &Self) -> u32 uses [E] panics P
end t.Tr

trait t.Bump pub
    fn bump(self: &mut Self) -> u32 uses [] panics never
end t.Bump

type t.A pub
    n: u32
end t.A

type t.B pub
    k: u32
    j: u32
end t.B

impl t.Tr<[], never> for t.A
fn t.A.m(self: &t.A) -> u32 uses [] panics never impls t.Tr<[], never>
    self.n
end t.A.m

impl t.Tr<[], never> for t.B
fn t.B.m(self: &t.B) -> u32 uses [] panics never impls t.Tr<[], never>
    self.k.wrapping_mul(self.j)
end t.B.m

impl t.Bump for t.A
fn t.A.bump(self: &mut t.A) -> u32 uses [] panics never impls t.Bump
    self.n = self.n.wrapping_add(1u32)
    self.n
end t.A.bump

fn t.use_ref(h: &dyn t.Tr<[], never>) -> u32 uses [] panics never
    h.m()
end t.use_ref

fn t.twice(h: &dyn t.Tr<[], never>) -> u32 uses [] panics never
    t.use_ref(h).wrapping_add(t.use_ref(h))
end t.twice

fn t.bump_all(b: &mut dyn t.Bump) -> u32 uses [] panics never
    let x = b.bump()
    x.wrapping_add(b.bump())
end t.bump_all

fn t.p_u64(x: u64) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &x))
    std.io.print_line(s.as_str())?
    Ok(())
end t.p_u64

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let a = t.A { n: 9u32 }
    let b = t.B { k: 6u32, j: 7u32 }
    let h: &dyn t.Tr<[], never> = &a
    t.p_u64(u64.from_u32(t.use_ref(h)))?
    t.p_u64(u64.from_u32(t.use_ref(&b)))?
    t.p_u64(u64.from_u32(t.twice(&a)))?
    t.p_u64(u64.from_u32(t.twice(&b)))?
    let mut c = t.A { n: 100u32 }
    t.p_u64(u64.from_u32(t.bump_all(&mut c)))?
    t.p_u64(u64.from_u32(c.n))?
    match alloc.boxed.Box.new(std.alloc.SYSTEM, t.B { k: 3u32, j: 5u32 }) {
        Ok(bx) => match alloc.boxed.Box.into_dyn(bx) {
            bd => {
                t.p_u64(u64.from_u32(t.use_ref(bd.as_ref())))?
            }
        }
        Err(_) => std.io.print_line("alloc err")?
    }
    core.mem.drop(a)
    core.mem.drop(b)
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// `std.json.to_string` (`derive [ToJson]` の構造体・入れ子・列挙・Vec)
#[test]
fn native_g5_json_to_string() {
    let out = check("g5_json_to_string", r#"type t.Inner derive [ToJson] pub
    id: u32
    ok: bool
end t.Inner

enum t.Kind derive [ToJson] pub
    Plain
    Num(u32)
end t.Kind

type t.Item derive [ToJson] pub
    name: alloc.string.String<std.alloc.System>
    value: u32
    inner: t.Inner
    kind: t.Kind
    tags: alloc.vec.Vec<u32, std.alloc.System>
end t.Item

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut tags = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(tags.push(1u32))
    core.mem.drop(tags.push(2u32))
    match alloc.string.String.from_str(std.alloc.SYSTEM, "it\"em") {
        Ok(name) => {
            let inner = t.Inner { id: 4000000000u32, ok: true }
            let kind = t.Kind.Num(3u32)
            let it = t.Item { name: name, value: 7u32, inner: inner, kind: kind, tags: tags }
            match std.json.to_string(&it) {
                Ok(s) => std.io.print_line(s.as_str())?
                Err(_) => std.io.print_line("err")?
            }
        }
        Err(_) => std.io.print_line("alloc err")?
    }
    let k = t.Kind.Plain
    match std.json.to_string(&k) {
        Ok(s) => std.io.print_line(s.as_str())?
        Err(_) => std.io.print_line("err")?
    }
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// 参照解 `bench/tasks/fsm/reference.ksnl` (空の配列リテラルを使う) がネイティブになる (名前の接頭辞だけ `t.` にする)
#[test]
fn native_g5_bench_fsm_reference() {
    let src = include_str!("../../bench/tasks/fsm/reference.ksnl").replace("bench.", "t.");
    let out = check("g5_fsm_reference", &src);
    assert!(out.contains("fsm ok"), "{}", out);
}

/// 空の配列の長さ 0 の読み: 要素を読む `get(0)` は `None`
#[test]
fn native_g5_empty_array_get() {
    let out = check("g5_empty_array_get", r#"fn t.first(xs: &[u8]) -> u8 uses [] panics never
    match xs.get(0usize) {
        Some(b) => *b
        None => 255u8
    }
end t.first

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &t.first(&[])))
    core.mem.drop(alloc.fmt.write(&mut s, &t.first(&[7u8])))
    std.io.print_line(s.as_str())?
    Ok(())
end t.main
"#);
    assert_eq!(out.trim(), "2557");
}
