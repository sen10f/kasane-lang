//! 世代付きの取っ手 `alloc.arena.Arena` / `alloc.arena.Handle` (設計_共有の取っ手.md、D1-S1/L1)。
//! `ksn run` (木走査) の表示と、ネイティブの debug / release の実行ファイルの表示が一致すること。
//! 添字と世代のやり取り (古い取っ手は `None`)、`Vec<Handle<T>>`、`Arena<String>` の値の破棄、
//! 循環 (スロットが自分や別のスロットの取っ手を持つ) を含む。

#![allow(dead_code, unused_imports)]

mod common;
use common::*;

/// 世代の食い違い・取っ手の複製・別の arena の取っ手・値の破棄までの一巡。
const ARENA: &str = r#"enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error

type t.Watcher derive [Debug, Clone, Copy] pub
    seen: u32
end t.Watcher

impl core.ops.Drop for t.Watcher
fn t.Watcher.drop(self: &mut t.Watcher) -> () uses [] panics never impls core.ops.Drop
    ()
end t.Watcher.drop

fn t.clone_handle(h: alloc.arena.Handle<u32>) -> alloc.arena.Handle<u32> uses [] panics never
    h.clone()
end t.clone_handle

fn t.drop_rounds(rounds: usize) -> () uses [alloc] panics never
    let mut r = 0usize
    while r < rounds {
        let mut words = alloc.arena.Arena.new(std.alloc.SYSTEM)
        let mut i = 0usize
        while i < 8usize {
            let made = alloc.string.String.from_str(std.alloc.SYSTEM, "arena drop round")
            match made {
                Ok(text) => match words.insert(text) {
                    Ok(_h) => {
                    }
                    Err(_e) => return
                }
                Err(_e) => return
            }
            i = i.wrapping_add(1usize)
        }
        r = r.wrapping_add(1usize)
    }
end t.drop_rounds

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics may
    let mut arena = alloc.arena.Arena.new(std.alloc.SYSTEM)
    core.check.is_true(arena.is_empty())
    core.check.eq(arena.len(), 0usize)
    let a = arena.insert(1u32)?
    let b = arena.insert(2u32)?
    let c = arena.insert(3u32)?
    core.check.eq(arena.len(), 3usize)
    core.check.is_true(arena.contains(a))
    core.check.eq(arena.get(a), Some(&1u32))
    core.check.eq(arena.get(b), Some(&2u32))
    core.check.eq(arena.get(c), Some(&3u32))
    let copied = a
    core.check.eq(arena.get(copied), Some(&1u32))
    core.check.eq(arena.get(a), Some(&1u32))
    match arena.get_mut(b) {
        Some(v) => core.mem.drop(core.mem.replace(v, 20u32))
        None => core.panic.panic("no b")
    }
    core.check.eq(arena.get(b), Some(&20u32))
    core.check.eq(arena.remove(b), Some(20u32))
    core.check.is_true(arena.get(b).is_none())
    core.check.is_true(!arena.contains(b))
    core.check.eq(arena.len(), 2usize)
    let d = arena.insert(4u32)?
    core.check.eq(arena.len(), 3usize)
    core.check.is_true(arena.contains(d))
    core.check.is_true(!arena.contains(b))
    core.check.eq(arena.get(d), Some(&4u32))
    core.check.is_true(d != b)
    core.check.is_true(t.clone_handle(d) == d)
    core.check.is_true(arena.remove(b).is_none())
    core.check.is_true(arena.remove(d).is_some())
    let e = arena.insert(5u32)?
    core.check.eq(arena.len(), 3usize)
    core.check.is_true(e != d)
    let mut others = alloc.arena.Arena.new(std.alloc.SYSTEM)
    let f = others.insert(7u32)?
    core.check.eq(others.get(f), Some(&7u32))
    core.check.is_true(others.get(e).is_none())
    core.check.eq(others.len(), 1usize)
    let mut handles = alloc.vec.Vec.new(std.alloc.SYSTEM)
    handles.push(a)?
    handles.push(c)?
    handles.push(e)?
    core.check.eq(handles.len(), 3usize)
    core.check.eq(arena.get(handles[0usize]), Some(&1u32))
    core.check.eq(arena.get(handles[2usize]), Some(&5u32))
    let mut words = alloc.arena.Arena.new(std.alloc.SYSTEM)
    let w0 = words.insert(alloc.string.String.from_str(std.alloc.SYSTEM, "zero")?)?
    let w1 = words.insert(alloc.string.String.from_str(std.alloc.SYSTEM, "one")?)?
    core.check.eq(words.len(), 2usize)
    match words.get(w1) {
        Some(text) => core.check.eq(text.as_str(), "one")
        None => core.panic.panic("no w1")
    }
    core.check.is_true(words.remove(w0).is_some())
    core.check.is_true(words.get(w0).is_none())
    core.check.is_true(words.get(w1).is_some())
    let mut watch = alloc.arena.Arena.new(std.alloc.SYSTEM)
    let t0 = watch.insert(t.Watcher { seen: 0u32 })?
    let t1 = watch.insert(t.Watcher { seen: 1u32 })?
    core.check.eq(watch.len(), 2usize)
    core.check.is_true(watch.remove(t0).is_some())
    core.check.is_true(watch.remove(t1).is_some())
    core.check.is_true(watch.is_empty())
    core.check.is_true(watch.remove(t0).is_none())
    t.drop_rounds(8usize)
    std.io.print_line("arena ok")?
    Ok(())
end t.main
"#;

/// 循環 (スロットが自分や別のスロットの取っ手を `Vec` で持つ) を作って落とす。再帰の型なので `Drop` は手書き。
const CYCLE: &str = r#"enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
end t.Error

type t.Node pub
    name: u32
    kids: alloc.vec.Vec<alloc.arena.Handle<t.Node>, std.alloc.System>
end t.Node

impl core.ops.Drop for t.Node
fn t.Node.drop(self: &mut t.Node) -> () uses [] panics never impls core.ops.Drop
    ()
end t.Node.drop

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics may
    let mut a = alloc.arena.Arena.new(std.alloc.SYSTEM)
    let n0 = a.insert(t.Node { name: 0u32, kids: alloc.vec.Vec.new(std.alloc.SYSTEM) })?
    let n1 = a.insert(t.Node { name: 1u32, kids: alloc.vec.Vec.new(std.alloc.SYSTEM) })?
    let n2 = a.insert(t.Node { name: 2u32, kids: alloc.vec.Vec.new(std.alloc.SYSTEM) })?
    core.check.eq(a.len(), 3usize)
    match a.get_mut(n0) {
        Some(v) => core.mem.drop(v.kids.push(n1))
        None => core.panic.panic("no n0")
    }
    match a.get_mut(n1) {
        Some(v) => core.mem.drop(v.kids.push(n0))
        None => core.panic.panic("no n1")
    }
    match a.get_mut(n2) {
        Some(v) => core.mem.drop(v.kids.push(n2))
        None => core.panic.panic("no n2")
    }
    match a.get(n2) {
        Some(v) => core.check.eq(v.kids.len(), 1usize)
        None => core.panic.panic("no n2")
    }
    core.mem.drop(a.remove(n0))
    core.check.eq(a.len(), 2usize)
    core.check.is_true(a.get(n0).is_none())
    core.mem.drop(a)
    std.io.print_line("cycle ok")?
    Ok(())
end t.main
"#;

fn agree(name: &str, src: &str, want: &str) {
    let t = Tmp::new(name, src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "run: {}\n{}", text(&interp.stderr), text(&interp.stdout));
    assert_eq!(text(&interp.stdout), want, "run の表示");
    for release in [false, true] {
        let (bt, exe, out) = build_native_strict(&format!("{}_{}", name, release), src, release);
        let p = exe.unwrap_or_else(|| panic!("ネイティブの実行ファイルが無い (release: {}): {}", release, out));
        let o = std::process::Command::new(&p).current_dir(&bt.0).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "release {}: {}{}", release, text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), text(&interp.stdout), "native (release: {}) と run の表示が違う", release);
    }
}

#[test]
fn arena_handles_match_native() {
    agree("arena_run", ARENA, "arena ok\n");
}

#[test]
fn arena_recursive_drop_matches_native() {
    agree("arena_cycle", CYCLE, "cycle ok\n");
}