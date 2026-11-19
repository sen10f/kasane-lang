//! 段階 1 の下ろしの穴 (その 4、E9040 の再調査): `core.str.*` の完全修飾の呼び出し、`core.iter.take` と利用者の反復子の連なり、
//! `&Option<T>` / `&mut Option<T>` (連結リスト・木の付け替え)、`(*r.as_mut()).f` の場所、参照越しの構造体パターン、
//! `char` のリテラルパターン、`i128` / `u128` の定数、`capture [&x]` のスカラー。
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

/// core.str の完全修飾 find / rfind (マルチバイトのバイト位置)
#[test]
fn native_str_find_rfind_multibyte() {
    let out = check("g4_str_find_rfind_multibyte", r#"fn t.p_u64(x: u64) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &x))
    std.io.print_line(s.as_str())?
    Ok(())
end t.p_u64

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    match core.str.find("aéa", "é") {
        Some(n) => t.p_u64(u64.from_usize(n))?
        None => std.io.print_line("find none")?
    }
    match core.str.rfind("éaé", "é") {
        Some(n) => t.p_u64(u64.from_usize(n))?
        None => std.io.print_line("rfind none")?
    }
    match core.str.find("abc", "d") {
        Some(_) => std.io.print_line("find some")?
        None => std.io.print_line("find none")?
    }
    match core.str.find("é", "é") {
        Some(n) => t.p_u64(u64.from_usize(n))?
        None => std.io.print_line("find none")?
    }
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// core.str の完全修飾 eq_ignore_ascii_case
#[test]
fn native_str_eq_ignore_ascii_case() {
    let out = check("g4_str_eq_ignore_ascii_case", r#"fn t.p_bool(x: bool) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &x))
    std.io.print_line(s.as_str())?
    Ok(())
end t.p_bool

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    t.p_bool(core.str.eq_ignore_ascii_case("A", "a"))?
    t.p_bool(core.str.eq_ignore_ascii_case("abc", "ABC"))?
    t.p_bool(core.str.eq_ignore_ascii_case("É", "é"))?
    t.p_bool(core.str.eq_ignore_ascii_case("é", "É"))?
    t.p_bool(core.str.eq_ignore_ascii_case("é", "e"))?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// take の個数 (count)
#[test]
fn native_take_count() {
    let out = check("g4_take_count_04", r#"enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let n = core.iter.count(core.iter.take(core.iter.range(0u64, 100u64), 7usize))
    let m = u64.from_usize(n)
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    s.push_u64(m)?
    std.io.print_line(s.as_str())?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// map -> filter -> take -> fold
#[test]
fn native_long_chain() {
    let out = check("g4_long_chain_09", r#"enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let total = core.iter.fold(
        core.iter.take(
            core.iter.filter(
                core.iter.map(
                    core.iter.range(0i64, 20i64),
                    fn(x: i64) -> i64 capture [] {
                        x.wrapping_mul(2i64)
                    },
                ),
                fn(x: &i64) -> bool capture [] {
                    *x > 5i64 && *x < 16i64
                },
            ),
            4usize,
        ),
        0i64,
        fn(acc: i64, x: i64) -> i64 capture [] {
            acc.wrapping_add(x)
        },
    )
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    s.push_i64(total)?
    std.io.print_line(s.as_str())?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// filter -> take -> count
#[test]
fn native_filter_take_count() {
    let out = check("g4_filter_take_count_16", r#"enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let n = core.iter.count(
        core.iter.take(
            core.iter.filter(
                core.iter.range(0i64, 20i64),
                fn(x: &i64) -> bool capture [] {
                    (*x & 1i64) == 0i64
                },
            ),
            5usize,
        ),
    )
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    s.push_u64(u64.from_usize(n))?
    std.io.print_line(s.as_str())?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// filter -> map -> take -> fold
#[test]
fn native_big_chain() {
    let out = check("g4_big_chain_28", r#"enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let total = core.iter.fold(
        core.iter.take(
            core.iter.map(
                core.iter.filter(
                    core.iter.range(0i64, 6i64),
                    fn(x: &i64) -> bool capture [] {
                        (*x & 1i64) == 0i64
                    },
                ),
                fn(x: i64) -> i64 capture [] {
                    x.wrapping_mul(2i64)
                },
            ),
            2usize,
        ),
        0i64,
        fn(acc: i64, x: i64) -> i64 capture [] {
            acc.wrapping_add(x)
        },
    )
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    s.push_i64(total)?
    std.io.print_line(s.as_str())?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// 利用者の反復子を fold で消費
#[test]
fn native_custom_iter_fold() {
    let out = check("g4_custom_iter_fold_06", r#"type t.Upto2 pub
    cur: u64
    stop: u64
end t.Upto2

impl core.iter.Iterator<u64, [], never> for t.Upto2
fn t.Upto2.next(self: &mut t.Upto2) -> Option<u64> uses [] panics never impls core.iter.Iterator<u64, [], never>
    if self.cur >= self.stop {
        return None
    }
    let v = self.cur
    self.cur = self.cur.wrapping_add(1u64)
    Some(v)
end t.Upto2.next

enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let total = core.iter.fold(
        t.Upto2 { cur: 0u64, stop: 4u64 },
        0u64,
        fn(acc: u64, x: u64) -> u64 capture [] {
            acc.wrapping_add(x)
        },
    )
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    s.push_u64(total)?
    std.io.print_line(s.as_str())?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// 利用者の反復子の filter -> fold
#[test]
fn native_custom_iter_filter_fold() {
    let out = check("g4_custom_iter_filter_fold_27", r#"type t.Upto10 pub
    cur: u64
    stop: u64
end t.Upto10

impl core.iter.Iterator<u64, [], never> for t.Upto10
fn t.Upto10.next(self: &mut t.Upto10) -> Option<u64> uses [] panics never impls core.iter.Iterator<u64, [], never>
    if self.cur >= self.stop {
        return None
    }
    let v = self.cur
    self.cur = self.cur.wrapping_add(1u64)
    Some(v)
end t.Upto10.next

enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let total = core.iter.fold(
        core.iter.filter(
            t.Upto10 { cur: 0u64, stop: 4u64 },
            fn(x: &u64) -> bool capture [] {
                (*x & 1u64) == 0u64
            },
        ),
        0u64,
        fn(acc: u64, x: u64) -> u64 capture [] {
            acc.wrapping_add(x)
        },
    )
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    s.push_u64(total)?
    std.io.print_line(s.as_str())?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// 空の利用者の反復子 (fold と count)
#[test]
fn native_empty_custom_iter() {
    let out = check("g4_empty_custom_iter_31", r#"type t.Upto11 pub
    cur: u64
    stop: u64
end t.Upto11

impl core.iter.Iterator<u64, [], never> for t.Upto11
fn t.Upto11.next(self: &mut t.Upto11) -> Option<u64> uses [] panics never impls core.iter.Iterator<u64, [], never>
    if self.cur >= self.stop {
        return None
    }
    let v = self.cur
    self.cur = self.cur.wrapping_add(1u64)
    Some(v)
end t.Upto11.next

enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let total = core.iter.fold(
        t.Upto11 { cur: 5u64, stop: 5u64 },
        7u64,
        fn(acc: u64, x: u64) -> u64 capture [] {
            acc.wrapping_add(x)
        },
    )
    let n = core.iter.count(t.Upto11 { cur: 5u64, stop: 5u64 })
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    s.push_u64(total)?
    s.push_u64(u64.from_usize(n))?
    std.io.print_line(s.as_str())?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// 利用者の反復子を for と fold の両方へ
#[test]
fn native_two_instances() {
    let out = check("g4_two_instances_17", r#"type t.Upto4 pub
    cur: u64
    stop: u64
end t.Upto4

impl core.iter.Iterator<u64, [], never> for t.Upto4
fn t.Upto4.next(self: &mut t.Upto4) -> Option<u64> uses [] panics never impls core.iter.Iterator<u64, [], never>
    if self.cur >= self.stop {
        return None
    }
    let v = self.cur
    self.cur = self.cur.wrapping_add(1u64)
    Some(v)
end t.Upto4.next

enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let mut it = t.Upto4 { cur: 0u64, stop: 4u64 }
    let mut acc = 0u64
    for x in it {
        acc = acc.wrapping_add(x)
    }
    let total = core.iter.fold(
        t.Upto4 { cur: 0u64, stop: 4u64 },
        0u64,
        fn(acc2: u64, x: u64) -> u64 capture [] {
            acc2.wrapping_add(x)
        },
    )
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    s.push_u64(acc)?
    s.push_u64(total)?
    std.io.print_line(s.as_str())?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// Option<Box<_>> の連結リストを &mut Option で付け替える
#[test]
fn native_box_list_build() {
    let out = check("g4_box_list_build_03", r#"type t.ListNode derive [Drop] pub
    val: i64
    next: Option<alloc.boxed.Box<t.ListNode, std.alloc.System>>
end t.ListNode

fn t.list_push(head: &mut Option<alloc.boxed.Box<t.ListNode, std.alloc.System>>, v: i64) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never
    let old = core.mem.replace(head, None)
    let n = alloc.boxed.Box.new(std.alloc.SYSTEM, t.ListNode { val: v, next: old })?
    *head = Some(n)
    Ok(())
end t.list_push

fn t.list_sum(o: &Option<alloc.boxed.Box<t.ListNode, std.alloc.System>>) -> i64 uses [] panics never stack 4096
    match o {
        None => 0i64
        Some(b) => {
            let nb = b.as_ref()
            nb.val.wrapping_add(t.list_sum(&nb.next))
        }
    }
end t.list_sum

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let mut head: Option<alloc.boxed.Box<t.ListNode, std.alloc.System>> = None
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    match t.list_push(&mut head, 10i64) {
        Ok(()) => ()
        Err(e) => core.mem.drop(e)
    }
    match t.list_push(&mut head, 20i64) {
        Ok(()) => ()
        Err(e) => core.mem.drop(e)
    }
    match t.list_push(&mut head, 30i64) {
        Ok(()) => ()
        Err(e) => core.mem.drop(e)
    }
    s.push_str("sum=")?
    s.push_i64(t.list_sum(&head))?
    std.io.print_line(s.as_str())?
    core.mem.drop(head)
    Ok(())
end t.main

enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error
"#);
    assert!(!out.is_empty());
}

/// Box の木を &Option で読む (match o { Some(b) => .. })
#[test]
fn native_box_tree_basic() {
    let out = check("g4_box_tree_basic_01", r#"enum t.Node derive [Drop] pub
    Leaf(i64)
    Branch(i64, Option<alloc.boxed.Box<t.Node, std.alloc.System>>)
end t.Node

fn t.opt_sum(o: &Option<alloc.boxed.Box<t.Node, std.alloc.System>>) -> i64 uses [] panics never stack 4096
    match o {
        None => 0i64
        Some(b) => t.node_sum(b.as_ref())
    }
end t.opt_sum

fn t.node_sum(n: &t.Node) -> i64 uses [] panics never stack 4096
    match n {
        t.Node.Leaf(v) => *v
        t.Node.Branch(v, c) => (*v).wrapping_add(t.opt_sum(c))
    }
end t.node_sum

fn t.build(depth: u32, v: i64) -> Result<alloc.boxed.Box<t.Node, std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics never stack 4096
    if depth == 0u32 {
        return Ok(alloc.boxed.Box.new(std.alloc.SYSTEM, t.Node.Leaf(v))?)
    }
    let c = t.build(depth.wrapping_sub(1u32), v.wrapping_add(1i64))?
    Ok(alloc.boxed.Box.new(std.alloc.SYSTEM, t.Node.Branch(v, Some(c)))?)
end t.build

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    match t.build(30u32, 0i64) {
        Ok(root) => {
            let sum = t.node_sum(root.as_ref())
            s.push_str("sum=")?
            s.push_i64(sum)?
            std.io.print_line(s.as_str())?
            core.mem.drop(root)
        }
        Err(e) => core.mem.drop(e)
    }
    Ok(())
end t.main

enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error
"#);
    assert!(!out.is_empty());
}

/// 深い Box の鎖 (構造体パターンの参照越しの分解)
#[test]
fn native_box_chain_sum_deep() {
    let out = check("g4_box_chain_sum_deep_14", r#"type t.Node derive [Drop] pub
    val: i64
    child: Option<alloc.boxed.Box<t.Node, std.alloc.System>>
end t.Node

fn t.node_sum(n: &t.Node) -> i64 uses [] panics never stack 4096
    match n {
        t.Node { val: v, child: c } => (*v).wrapping_add(t.opt_sum(c))
    }
end t.node_sum

fn t.opt_sum(o: &Option<alloc.boxed.Box<t.Node, std.alloc.System>>) -> i64 uses [] panics never stack 4096
    match o {
        None => 0i64
        Some(b) => t.node_sum(b.as_ref())
    }
end t.opt_sum

fn t.build(depth: u32, v: i64) -> Result<alloc.boxed.Box<t.Node, std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics never stack 4096
    if depth == 0u32 {
        return Ok(alloc.boxed.Box.new(std.alloc.SYSTEM, t.Node { val: v, child: None })?)
    }
    let c = t.build(depth.wrapping_sub(1u32), v.wrapping_add(1i64))?
    Ok(alloc.boxed.Box.new(std.alloc.SYSTEM, t.Node { val: v, child: Some(c) })?)
end t.build

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    match t.build(200u32, 0i64) {
        Ok(root) => {
            let sum = t.node_sum(root.as_ref())
            s.push_str("sum=")?
            s.push_i64(sum)?
            std.io.print_line(s.as_str())?
            core.mem.drop(root)
        }
        Err(e) => core.mem.drop(e)
    }
    Ok(())
end t.main

enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error
"#);
    assert!(!out.is_empty());
}

/// (*r.as_mut()).child の core.mem.replace
#[test]
fn native_box_extract_detach() {
    let out = check("g4_box_extract_detach_16", r#"type t.Node derive [Drop] pub
    val: i64
    child: Option<alloc.boxed.Box<t.Node, std.alloc.System>>
end t.Node

fn t.node_sum(n: &t.Node) -> i64 uses [] panics never stack 4096
    match n {
        t.Node { val: v, child: c } => (*v).wrapping_add(t.opt_sum(c))
    }
end t.node_sum

fn t.opt_sum(o: &Option<alloc.boxed.Box<t.Node, std.alloc.System>>) -> i64 uses [] panics never stack 4096
    match o {
        None => 0i64
        Some(b) => t.node_sum(b.as_ref())
    }
end t.opt_sum

fn t.build(depth: u32, v: i64) -> Result<alloc.boxed.Box<t.Node, std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics never stack 4096
    if depth == 0u32 {
        return Ok(alloc.boxed.Box.new(std.alloc.SYSTEM, t.Node { val: v, child: None })?)
    }
    let c = t.build(depth.wrapping_sub(1u32), v.wrapping_add(1i64))?
    Ok(alloc.boxed.Box.new(std.alloc.SYSTEM, t.Node { val: v, child: Some(c) })?)
end t.build

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    match t.build(4u32, 10i64) {
        Ok(root) => {
            let sum1 = t.node_sum(root.as_ref())
            let mut r = root
            let child = core.mem.replace(&mut (*r.as_mut()).child, None)
            match child {
                None => ()
                Some(c) => {
                    s.push_str("child=")?
                    s.push_i64(t.node_sum(c.as_ref()))?
                    std.io.print_line(s.as_str())?
                    core.mem.drop(c)
                }
            }
            let sum2 = t.node_sum(r.as_ref())
            s.push_str("root=")?
            s.push_i64(sum2)?
            std.io.print_line(s.as_str())?
            core.mem.drop(r)
            s.push_str("sum1=")?
            s.push_i64(sum1)?
            std.io.print_line(s.as_str())?
        }
        Err(e) => core.mem.drop(e)
    }
    Ok(())
end t.main

enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error
"#);
    assert!(!out.is_empty());
}

/// Box の鎖を 1 つずつ外して解放する
#[test]
fn native_box_drop_walk() {
    let out = check("g4_box_drop_walk_23", r#"type t.Node derive [Drop] pub
    val: i64
    child: Option<alloc.boxed.Box<t.Node, std.alloc.System>>
end t.Node

fn t.build(depth: u32, v: i64) -> Result<alloc.boxed.Box<t.Node, std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics never stack 4096
    if depth == 0u32 {
        return Ok(alloc.boxed.Box.new(std.alloc.SYSTEM, t.Node { val: v, child: None })?)
    }
    let c = t.build(depth.wrapping_sub(1u32), v.wrapping_add(1i64))?
    Ok(alloc.boxed.Box.new(std.alloc.SYSTEM, t.Node { val: v, child: Some(c) })?)
end t.build

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    match t.build(100u32, 0i64) {
        Ok(root) => {
            let mut cur = Some(root)
            let mut count = 0u32
            loop {
                match cur {
                    None => break
                    Some(c) => {
                        let mut cc = c
                        let grand = core.mem.replace(&mut (*cc.as_mut()).child, None)
                        core.mem.drop(cc)
                        cur = grand
                        count = count.wrapping_add(1u32)
                    }
                }
            }
            s.push_str("count=")?
            s.push_u64(u64.from_u32(count))?
            std.io.print_line(s.as_str())?
        }
        Err(e) => core.mem.drop(e)
    }
    Ok(())
end t.main

enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error
"#);
    assert!(!out.is_empty());
}

/// 連結リストの後半を切り離す
#[test]
fn native_box_list_detach_half() {
    let out = check("g4_box_list_detach_half_27", r#"type t.ListNode derive [Drop] pub
    val: i64
    next: Option<alloc.boxed.Box<t.ListNode, std.alloc.System>>
end t.ListNode

fn t.list_push(head: &mut Option<alloc.boxed.Box<t.ListNode, std.alloc.System>>, v: i64) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never
    let old = core.mem.replace(head, None)
    let n = alloc.boxed.Box.new(std.alloc.SYSTEM, t.ListNode { val: v, next: old })?
    *head = Some(n)
    Ok(())
end t.list_push

fn t.list_sum(o: &Option<alloc.boxed.Box<t.ListNode, std.alloc.System>>) -> i64 uses [] panics never stack 4096
    match o {
        None => 0i64
        Some(b) => {
            let nb = b.as_ref()
            nb.val.wrapping_add(t.list_sum(&nb.next))
        }
    }
end t.list_sum

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let mut head: Option<alloc.boxed.Box<t.ListNode, std.alloc.System>> = None
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    match t.list_push(&mut head, 1i64) {
        Ok(()) => ()
        Err(e) => core.mem.drop(e)
    }
    match t.list_push(&mut head, 2i64) {
        Ok(()) => ()
        Err(e) => core.mem.drop(e)
    }
    match t.list_push(&mut head, 3i64) {
        Ok(()) => ()
        Err(e) => core.mem.drop(e)
    }
    let first = core.mem.replace(&mut head, None)
    match first {
        None => ()
        Some(h) => {
            let mut hh = h
            let tail = core.mem.replace(&mut (*hh.as_mut()).next, None)
            core.mem.drop(hh)
            let sum_tail = t.list_sum(&tail)
            s.push_str("tail=")?
            s.push_i64(sum_tail)?
            std.io.print_line(s.as_str())?
            core.mem.drop(tail)
        }
    }
    Ok(())
end t.main

enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error
"#);
    assert!(!out.is_empty());
}

/// char のリテラルパターン (マルチバイト)
#[test]
fn native_char_literal_patterns() {
    let out = check("g4_char_literal_patterns", r#"fn t.char_case(c: char) -> &'static str uses [] panics never
    match c {
        'é' => "e-acute"
        '日' => "nihon"
        '😀' => "grin"
        'a' => "ascii"
        _ => "other"
    }
end t.char_case

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never
    std.io.print_line(t.char_case('é'))?
    std.io.print_line(t.char_case('日'))?
    std.io.print_line(t.char_case('😀'))?
    std.io.print_line(t.char_case('a'))?
    std.io.print_line(t.char_case('z'))?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// chars() の中の char のリテラルパターン
#[test]
fn native_chars_char_pattern_loop() {
    let out = check("g4_chars_char_pattern_loop", r#"fn t.p_u64(x: u64) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &x))
    std.io.print_line(s.as_str())?
    Ok(())
end t.p_u64

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut n = 0u64
    for c in "é日é😀".chars() {
        match c {
            'é' => {
                n = n.wrapping_add(1u64)
            }
            _ => {
            }
        }
    }
    t.p_u64(n)?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// capture [&x] のスカラーを map のクロージャで読む
#[test]
fn native_map_capture() {
    let out = check("g4_map_capture_07", r#"enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics never
    let base = 10u64
    let total = core.iter.fold(
        core.iter.map(
            core.iter.range(0u64, 3u64),
            fn(x: u64) -> u64 capture [&base] {
                x.wrapping_add(*base)
            },
        ),
        0u64,
        fn(acc: u64, x: u64) -> u64 capture [] {
            acc.wrapping_add(x)
        },
    )
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    s.push_u64(total)?
    std.io.print_line(s.as_str())?
    Ok(())
end t.main
"#);
    assert!(!out.is_empty());
}

/// take の境界: 0 個・範囲より多い・filter の後ろ・take の入れ子 (小さい方が効く)。
#[test]
fn native_take_boundaries() {
    let out = check(
        "g4_take_bounds",
        r#"fn t.p_u64(x: u64) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &x))
    std.io.print_line(s.as_str())?
    Ok(())
end t.p_u64

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let a = core.iter.count(core.iter.take(core.iter.range(0u64, 10u64), 0usize))
    t.p_u64(u64.from_usize(a))?
    let b = core.iter.count(core.iter.take(core.iter.range(0u64, 3u64), 99usize))
    t.p_u64(u64.from_usize(b))?
    let c = core.iter.fold(
        core.iter.take(core.iter.take(core.iter.range(1u64, 100u64), 5usize), 3usize),
        0u64,
        fn(acc: u64, x: u64) -> u64 capture [] {
            acc.wrapping_add(x)
        },
    )
    t.p_u64(c)?
    let d = core.iter.fold(
        core.iter.take(
            core.iter.filter(
                core.iter.range(0u64, 100u64),
                fn(x: &u64) -> bool capture [] {
                    *x > 10u64
                },
            ),
            4usize,
        ),
        0u64,
        fn(acc: u64, x: u64) -> u64 capture [] {
            acc.wrapping_add(x)
        },
    )
    t.p_u64(d)?
    let e = core.iter.count(core.iter.take(core.iter.range(5u64, 5u64), 2usize))
    t.p_u64(u64.from_usize(e))?
    Ok(())
end t.main
"#,
    );
    assert_eq!(out, "0\n3\n6\n50\n0\n");
}

/// `&Option<スカラー>` / `&mut Option<スカラー>` / `&Option<String>` の読み書きと match (None・Some の両方、更新後の再読み)。
#[test]
fn native_option_refs_scalar_and_vec() {
    let out = check(
        "g4_opt_refs",
        r#"fn t.p_i64(x: i64) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &x))
    std.io.print_line(s.as_str())?
    Ok(())
end t.p_i64

fn t.get(o: &Option<i64>) -> i64 uses [] panics never
    match o {
        None => 0i64.wrapping_sub(1i64)
        Some(v) => *v
    }
end t.get

fn t.set(o: &mut Option<i64>, v: i64) -> () uses [] panics never
    *o = Some(v)
end t.set

fn t.take(o: &mut Option<i64>) -> Option<i64> uses [] panics never
    core.mem.replace(o, None)
end t.take

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut x: Option<i64> = None
    t.p_i64(t.get(&x))?
    t.set(&mut x, 41i64)
    t.p_i64(t.get(&x))?
    match t.take(&mut x) {
        Some(v) => t.p_i64(v.wrapping_add(1i64))?
        None => t.p_i64(0i64)?
    }
    t.p_i64(t.get(&x))?
    Ok(())
end t.main
"#,
    );
    assert_eq!(out, "-1\n41\n42\n-1\n");
}

/// `i128` / `u128` の定数 (`MIN` / `MAX`)。最小値・最大値の端を比べる (Display は使わず、bool で見る)。
#[test]
fn native_i128_u128_constants() {
    let out = check(
        "g4_int128_consts",
        r#"fn t.p_bool(x: bool) -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &x))
    std.io.print_line(s.as_str())?
    Ok(())
end t.p_bool

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    t.p_bool(core.num.i128.MIN.wrapping_sub(1i128) == core.num.i128.MAX)?
    t.p_bool(core.num.i128.MAX.wrapping_add(1i128) == core.num.i128.MIN)?
    t.p_bool(core.num.u128.MAX.wrapping_add(1u128) == core.num.u128.MIN)?
    t.p_bool(core.num.u128.MIN.wrapping_sub(1u128) == core.num.u128.MAX)?
    t.p_bool(core.num.u128.MAX == 0u128.wrapping_sub(1u128))?
    Ok(())
end t.main
"#,
    );
    assert_eq!(out, "true\ntrue\ntrue\ntrue\ntrue\n");
}
