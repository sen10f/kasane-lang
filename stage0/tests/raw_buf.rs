//! `alloc.raw.Buf<T, A>` (仕様 8.3、修正プラン 3-3) の実行テスト。
//! 木走査 (`ksn run --engine=tree`)、VM 指定 (`--engine=vm`。KIR に下ろせないときは木走査へ退避する)、
//! `ksn build` で作った実行体の出力が一致することを見る。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str, src: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_rawbuf_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), TOML).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(d.join("src").join("t.ksnl"), src).unwrap();
        Tmp(d)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(exe()).args(args).current_dir(cwd).output().expect("spawn ksn")
}

/// 3 つの経路で実行し、出力が同じことを確かめて返す。
fn run_all(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    let chk = run(&["check"], &t.0);
    assert!(text(&chk.stdout).trim().is_empty() && chk.status.success(), "{}{}", text(&chk.stdout), text(&chk.stderr));
    let tree = run(&["run", "--engine=tree"], &t.0);
    assert_eq!(tree.status.code(), Some(0), "{}{}", text(&tree.stdout), text(&tree.stderr));
    let vm = run(&["run", "--engine=vm"], &t.0);
    assert_eq!(vm.status.code(), Some(0), "{}{}", text(&vm.stdout), text(&vm.stderr));
    assert_eq!(text(&tree.stdout), text(&vm.stdout));
    let cache = std::env::temp_dir().join(format!("ksn_rawbuf_cache_{}_{}", std::process::id(), name));
    let b = Command::new(exe()).arg("build").current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    assert!(b.status.success(), "{}{}", text(&b.stdout), text(&b.stderr));
    let _ = fs::remove_dir_all(&cache);
    let launcher = t.0.join("target/debug").join("t");
    let native = t.0.join("target/debug").join("t.native");
    let p = if native.exists() { native } else { launcher };
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&tree.stdout), text(&o.stdout), "build の出力が run と違う");
    text(&tree.stdout)
}

/// `run_all` と同じだが、ネイティブ (LLVM) の実行体ができることも要求する (バンドルへの退避は失敗)。
fn run_native(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    let chk = run(&["check"], &t.0);
    assert!(text(&chk.stdout).trim().is_empty() && chk.status.success(), "{}{}", text(&chk.stdout), text(&chk.stderr));
    let tree = run(&["run", "--engine=tree"], &t.0);
    assert_eq!(tree.status.code(), Some(0), "{}{}", text(&tree.stdout), text(&tree.stderr));
    let vm = run(&["run", "--engine=vm"], &t.0);
    assert_eq!(vm.status.code(), Some(0), "{}{}", text(&vm.stdout), text(&vm.stderr));
    assert_eq!(text(&tree.stdout), text(&vm.stdout), "VM の出力が木走査と違う");
    assert!(!text(&vm.stderr).contains("vm engine skipped"), "VM が KIR に下ろせず木走査へ退避した: {}", text(&vm.stderr));
    let cache = std::env::temp_dir().join(format!("ksn_rawbuf_cache_{}_{}", std::process::id(), name));
    let b = Command::new(exe()).arg("build").current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    let log = format!("{}{}", text(&b.stdout), text(&b.stderr));
    let native = t.0.join("target/debug/t.native");
    assert!(b.status.success() && native.exists(), "ネイティブにならない: {}", log);
    let o = Command::new(&native).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&tree.stdout), text(&o.stdout), "native の出力が run と違う");
    text(&tree.stdout)
}

const GENERIC: &str = r#"type t.Cell[T] pub
    value: T
    count: u32
end t.Cell

type t.Pair[T] pub
    a: t.Cell<T>
    b: T
end t.Pair

fn t.Cell.new[T](value: T) -> t.Cell<T> uses [] panics never
    t.Cell { value: value, count: 0u32 }
end t.Cell.new

fn t.Cell.get[T](self: &t.Cell<T>) -> &T uses [] panics never
    &self.value
end t.Cell.get

fn t.Cell.slot_mut[T](self: &mut t.Cell<T>) -> &mut T uses [] panics never
    self.count = self.count.wrapping_add(1u32)
    &mut self.value
end t.Cell.slot_mut

fn t.Cell.take[T](self: t.Cell<T>) -> T uses [] panics never
    self.value
end t.Cell.take

fn t.bump(c: &mut t.Cell<u32>) -> () uses [] panics never
    c.count = c.count.wrapping_add(1u32)
end t.bump

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut c = t.Cell.new(7u32)
    core.check.eq(*c.get(), 7u32)
    t.bump(&mut c)
    *c.slot_mut() = 9u32
    core.check.eq(*c.get(), 9u32)
    core.check.eq(c.count, 2u32)
    let p = t.Pair { a: t.Cell.new(1u64), b: 2u64 }
    core.check.eq(*p.a.get(), 1u64)
    let q = t.Cell.new(p)
    core.check.eq(q.get().b, 2u64)
    core.check.eq(c.take(), 9u32)
    std.io.print_line("ok")?
    Ok(())
end t.main
"#;

const REF_FIELD: &str = r#"type t.Cu pub
    value: u32
    count: u32
end t.Cu

fn t.Cu.slot_mut(self: &mut t.Cu) -> &mut u32 uses [] panics never
    &mut self.value
end t.Cu.slot_mut

fn t.set(c: &mut t.Cu) -> () uses [] panics never
    let r = c.slot_mut()
    *r = 9u32
end t.set

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut d = t.Cu { value: 7u32, count: 0u32 }
    t.set(&mut d)
    core.check.eq(d.value, 9u32)
    std.io.print_line("ok")?
    Ok(())
end t.main
"#;

const PROG: &str = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    t.run()
end t.main

fn t.show(b: &alloc.raw.Buf<u32, std.alloc.System>) -> Result<(), std.io.Error> uses [io.console] panics may
    let s = b.as_slice()
    let mut i = 0usize
    let mut sum = 0u32
    while i < s.len() {
        match s.get(i) {
            Some(x) => {
                sum = sum.wrapping_add(*x)
            }
            None => {
            }
        }
        i = i.wrapping_add(1usize)
    }
    core.check.eq(sum, sum)
    std.io.print_line("show")
end t.show

fn t.run() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut b = match alloc.raw.Buf.with_capacity(std.alloc.SYSTEM, 3usize) {
        Ok(b) => b
        Err(_e) => core.panic.panic("oom")
    }
    core.check.eq(b.len(), 0usize)
    core.check.eq(b.cap(), 3usize)
    core.check.eq(b.pop(), None)
    core.check.eq(b.push_within_cap(10u32), Ok(()))
    core.check.eq(b.push_within_cap(20u32), Ok(()))
    core.check.eq(b.push_within_cap(30u32), Ok(()))
    core.check.eq(b.push_within_cap(40u32), Err(40u32))
    core.check.eq(b.insert_within_cap(0usize, 5u32), Err(5u32))
    core.check.eq(b.grow(8usize), Ok(()))
    core.check.eq(b.cap(), 8usize)
    core.check.eq(b.insert_within_cap(9usize, 5u32), Err(5u32))
    core.check.eq(b.insert_within_cap(0usize, 5u32), Ok(()))
    core.check.eq(b.len(), 4usize)
    core.check.eq(b.get(0usize), Some(&5u32))
    core.check.eq(b.get(4usize), None)
    match b.get_mut(1usize) {
        Some(x) => {
            *x = 11u32
        }
        None => core.panic.panic("get_mut")
    }
    core.check.eq(b.get(1usize), Some(&11u32))
    core.check.eq(b.remove(0usize), Some(5u32))
    core.check.eq(b.remove(7usize), None)
    core.check.eq(b.swap_remove(0usize), Some(11u32))
    core.check.eq(b.len(), 2usize)
    core.check.eq(b.get(0usize), Some(&30u32))
    t.show(&b)?
    b.truncate(1usize)
    core.check.eq(b.len(), 1usize)
    core.check.eq(b.pop(), Some(30u32))
    core.check.eq(b.is_empty(), true)
    core.check.eq(b.cap(), 8usize)
    std.io.print_line("done")?
    Ok(())
end t.run
"#;

const BYTES: &str = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut b = match alloc.raw.Buf.with_capacity(std.alloc.SYSTEM, 8usize) {
        Ok(b) => b
        Err(_e) => core.panic.panic("oom")
    }
    core.check.eq(b.append_str_within_cap("héllo"), Ok(()))
    core.check.eq(b.len(), 6usize)
    core.check.eq(b.append_str_within_cap("xyz"), Err(()))
    core.check.eq(b.len(), 6usize)
    match b.as_str() {
        Ok(s) => std.io.print_line(s)?
        Err(_e) => core.panic.panic("utf8")
    }
    core.check.eq(b.push_within_cap(255u8), Ok(()))
    match b.as_str() {
        Ok(_s) => core.panic.panic("should be invalid")
        Err(e) => core.check.eq(e.valid_up_to, 6usize)
    }
    Ok(())
end t.main
"#;

#[test]
fn buf_operations_agree_across_run_engines_and_build() {
    let out = run_native("ops", PROG);
    assert_eq!(out, "show\ndone\n");
}

#[test]
fn buf_u8_utf8_checked_view() {
    let out = run_native("bytes", BYTES);
    assert_eq!(out, "héllo\n");
}

#[test]
fn generic_structs_and_methods_lower_to_native() {
    assert_eq!(run_native("generic", GENERIC), "ok\n");
}

#[test]
fn address_of_a_field_through_a_reference_points_at_the_field() {
    // `&mut self.value` は、フィールドそのもののアドレス (以前は読んだ値の一時の写しを指していた)。
    assert_eq!(run_native("ref_field", REF_FIELD), "ok\n");
}

const OWNED: &str = r#"type t.Item pub
    name: alloc.string.String<std.alloc.System>
    n: u32
end t.Item

fn t.item(text: &str, n: u32) -> t.Item uses [alloc] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.check.eq(s.push_str(text), Ok(()))
    t.Item { name: s, n: n }
end t.item

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut b = match alloc.raw.Buf.with_capacity(std.alloc.SYSTEM, 4usize) {
        Ok(b) => b
        Err(_e) => core.panic.panic("oom")
    }
    core.check.eq(b.push_within_cap(t.item("a", 1u32)).is_ok(), true)
    core.check.eq(b.push_within_cap(t.item("b", 2u32)).is_ok(), true)
    core.check.eq(b.push_within_cap(t.item("c", 3u32)).is_ok(), true)
    core.check.eq(b.insert_within_cap(1usize, t.item("x", 9u32)).is_ok(), true)
    core.check.eq(b.len(), 4usize)
    let full = b.push_within_cap(t.item("y", 10u32))
    core.check.eq(full.is_err(), true)
    match b.swap_remove(0usize) {
        Some(it) => core.check.eq(it.n, 1u32)
        None => core.panic.panic("swap_remove")
    }
    core.check.eq(b.len(), 3usize)
    b.truncate(1usize)
    core.check.eq(b.len(), 1usize)
    match b.pop() {
        Some(it) => core.check.eq(it.n, 3u32)
        None => core.panic.panic("pop")
    }
    std.io.print_line("owned ok")?
    Ok(())
end t.main
"#;

#[test]
fn buf_of_owning_elements_lowers_to_native() {
    assert_eq!(run_native("owned", OWNED), "owned ok\n");
}

#[test]
fn a_kasane_vec_over_buf_lowers_to_native_with_owning_elements() {
    // ジェネリックな構造体 + `Buf` フィールド (暗黙の drop) + メソッド + 所有物の要素 (`String`・入れ子の Vec) を、
    // 組み込みの `Vec` を使わず Kasane だけで書いた `Vec` (設計_stdlib_kasane.md)。
    let src = include_str!("data/raw_buf/kvec.ksnl");
    assert_eq!(run_native("kvec", src), "ok\n");
}

const VEC_KASANE: &str = r#"fn t.name(text: &str) -> alloc.string.String<std.alloc.System> uses [alloc] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.check.eq(s.push_str(text), Ok(()))
    s
end t.name

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < 20u32 {
        core.check.eq(v.push(i), Ok(()))
        i = i.wrapping_add(1u32)
    }
    core.check.eq(v.len(), 20usize)
    core.check.eq(v.pop(), Some(19u32))
    core.check.eq(v.insert(0usize, 100u32).is_ok(), true)
    core.check.eq(v.insert(99usize, 1u32).is_err(), true)
    core.check.eq(v.remove(0usize), Some(100u32))
    core.check.eq(v.remove(50usize), None)
    core.check.eq(v.reserve(100usize), Ok(()))
    core.check.eq(v.capacity() >= v.len(), true)
    core.check.eq(v.as_slice().get(5usize), Some(&5u32))
    v.truncate(3usize)
    core.check.eq(v.len(), 3usize)
    v.clear()
    core.check.eq(v.is_empty(), true)
    let mut names = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.check.eq(names.push(t.name("ann")), Ok(()))
    core.check.eq(names.push(t.name("bob")), Ok(()))
    core.check.eq(names.insert(1usize, t.name("cy")).is_ok(), true)
    match names.remove(0usize) {
        Some(s) => core.check.eq(s.len(), 3usize)
        None => core.panic.panic("remove")
    }
    names.truncate(1usize)
    core.check.eq(names.len(), 1usize)
    names.clear()
    std.io.print_line("vec ok")?
    Ok(())
end t.main
"#;

#[test]
fn vec_methods_written_in_kasane_match_the_builtin_ones_natively() {
    // KIR は `Vec` の基本メソッドを `alloc/vec.ksnl` (Kasane。`Buf` の上) の単相インスタンスで下ろす。
    // 木走査 (組み込み) と、Kasane 版のネイティブの出力が一致する。
    let t = Tmp::new("vec_kasane", VEC_KASANE);
    let tree = run(&["run", "--engine=tree"], &t.0);
    assert_eq!(tree.status.code(), Some(0), "{}{}", text(&tree.stdout), text(&tree.stderr));
    let cache = std::env::temp_dir().join(format!("ksn_rawbuf_cache_{}_vk", std::process::id()));
    let b = Command::new(exe()).arg("build").current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    let native = t.0.join("target/debug/t.native");
    assert!(b.status.success() && native.exists(), "ネイティブにならない: {}{}", text(&b.stdout), text(&b.stderr));
    let o = Command::new(&native).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&tree.stdout), text(&o.stdout));
    // Kasane 版が使われている (`alloc.vec.Vec.push<..>` の関数が KIR にある)。
    let k = Command::new(exe()).args(["kir", "t.main"]).current_dir(&t.0).output().unwrap();
    assert!(text(&k.stdout).contains("fn alloc.vec.Vec.push key=alloc.vec.Vec.push<"), "{}", text(&k.stdout));
}
