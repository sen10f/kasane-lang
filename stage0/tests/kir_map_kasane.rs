//! `HashMap` / `BtreeMap` の Kasane 版 (`spec/stdlib/alloc/collections.ksnl`、修正プラン 3-4 段 2) のネイティブ生成。
//! 型ごとの下ろし (`kir_lower_map.rs`) をやめて、利用者の集成体と同じ経路で下ろす。確かめるのは次の形:
//! 値が構造体・`String` のマップを、構造体のフィールド越しに `self.map.insert` / `get` / `get_mut` で使う (記号表の必須形)、
//! `for (k, v) in m.iter()` の挿入順、`get_mut` で書いた値が残ること。木走査 (`ksn run --engine=tree`) と
//! `ksn build --release` (退避なし) の出力が一致することを見る。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str, src: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_mapk2_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        let toml = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";
        fs::write(d.join("kasane.toml"), toml).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(d.join("src").join("t.ksnl"), src).unwrap();
        // 書き方は 1 通り (E0020): 試験の入力は `ksn fmt` の出力にそろえる。
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_ksn")).arg("fmt").current_dir(&d).output();
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

/// 木走査の出力と、ネイティブ (退避なし) の出力が同じことを確かめて、その出力を返す。
fn same(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    let chk = run(&["check"], &t.0);
    assert!(chk.status.success() && text(&chk.stdout).trim().is_empty(), "check: {}{}", text(&chk.stdout), text(&chk.stderr));
    let tree = run(&["run", "--engine=tree"], &t.0);
    assert_eq!(tree.status.code(), Some(0), "tree: {}{}", text(&tree.stdout), text(&tree.stderr));
    let cache = std::env::temp_dir().join(format!("ksn_mapk2_cache_{}_{}", std::process::id(), name));
    let b = Command::new(exe()).args(["build", "--release"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    assert!(b.status.success(), "ネイティブにならない (退避なし): {}{}", text(&b.stdout), text(&b.stderr));
    let p = t.0.join("target/release").join("t.native");
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), tree.status.code(), "終了コード: {}", text(&o.stderr));
    assert_eq!(text(&o.stdout), text(&tree.stdout), "ネイティブの出力が木走査と違う");
    text(&tree.stdout)
}

#[test]
fn struct_field_hashmap_of_structs_with_string_keys_runs_natively() {
    let out = same(
        "field_map",
        r#"type t.Item pub
    count: u32
    name: alloc.string.String<std.alloc.System>
end t.Item

type t.Store pub
    map: alloc.collections.HashMap<alloc.string.String<std.alloc.System>, t.Item, std.alloc.System>
    hits: u32
end t.Store

fn t.Store.new() -> t.Store uses [alloc] panics never pub
    let m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    t.Store { map: m, hits: 0u32 }
end t.Store.new

fn t.Store.bump(self: &mut t.Store, key: &str) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never pub
    let k = alloc.string.String.from_str(std.alloc.SYSTEM, key)?
    match self.map.get_mut(&k) {
        Some(item) => {
            item.count = item.count.wrapping_add(1u32)
        }
        None => {
            let name = alloc.string.String.from_str(std.alloc.SYSTEM, key)?
            match self.map.insert(k, t.Item { count: 1u32, name: name }) {
                Ok(_old) => {
                }
                Err(error) => {
                    return Err(error)
                }
            }
        }
    }
    self.hits = self.hits.wrapping_add(1u32)
    Ok(())
end t.Store.bump

fn t.Store.count(self: &t.Store, key: &str) -> u32 uses [alloc] panics never pub
    match alloc.string.String.from_str(std.alloc.SYSTEM, key) {
        Ok(kk) => {
            match self.map.get(&kk) {
                Some(item) => {
                    item.count
                }
                None => {
                    0u32
                }
            }
        }
        Err(_e) => {
            0u32
        }
    }
end t.Store.count

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut s = t.Store.new()
    let words = ["a", "b", "a", "c", "a"]
    let mut i = 0usize
    while i < 5usize {
        match s.bump(words[i]) {
            Ok(_d) => {
            }
            Err(_e) => {
                core.panic.panic("alloc")
            }
        }
        i = i.wrapping_add(1usize)
    }
    core.check.eq(s.count("a"), 3u32)
    core.check.eq(s.count("b"), 1u32)
    core.check.eq(s.count("z"), 0u32)
    core.check.eq(s.hits, 5u32)
    core.mem.drop(std.io.print_line("field map ok"))
    Ok(())
end t.main
"#,
    );
    assert_eq!(out, "field map ok\n");
}

#[test]
fn get_mut_writes_back_and_iteration_keeps_insertion_order() {
    let out = same(
        "order",
        r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut k = 0u32
    while k < 12u32 {
        match m.insert(k.wrapping_mul(5u32) % 13u32, k) {
            Ok(_o) => {
            }
            Err(_e) => {
                core.panic.panic("alloc")
            }
        }
        k = k.wrapping_add(1u32)
    }
    k = 0u32
    while k < 12u32 {
        core.mem.drop(m.remove(&k))
        k = k.wrapping_add(2u32)
    }
    match m.get_mut(&1u32) {
        Some(v) => {
            *v = 100u32
        }
        None => {
            core.check.eq(true, false)
        }
    }
    core.check.eq(m.len(), 7usize)
    core.check.eq(m.get(&1u32), Some(&100u32))
    // 挿入の順 (削除した組の後ろの組は詰まる): 5 7 12 9 1 11 3
    let want = [5u32, 7u32, 12u32, 9u32, 1u32, 11u32, 3u32]
    let vals = [1u32, 4u32, 5u32, 7u32, 100u32, 10u32, 11u32]
    let mut idx = 0usize
    for (key, val) in m.iter() {
        core.check.eq(*key, want[idx])
        core.check.eq(*val, vals[idx])
        idx = idx.wrapping_add(1usize)
    }
    core.check.eq(idx, 7usize)
    core.mem.drop(std.io.print_line("order ok"))
    Ok(())
end t.main
"#,
    );
    assert_eq!(out, "order ok\n");
}

#[test]
fn btreemap_of_integers_insert_get_remove_runs_natively() {
    let out = same(
        "btree",
        r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut b = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)
    let mut k = 0u32
    while k < 20u32 {
        match b.insert(k.wrapping_mul(7u32) % 23u32, k) {
            Ok(_o) => {
            }
            Err(_e) => {
                core.panic.panic("alloc")
            }
        }
        k = k.wrapping_add(1u32)
    }
    core.check.eq(b.len(), 20usize)
    core.check.eq(b.get(&0u32), Some(&0u32))
    core.check.eq(b.contains_key(&23u32), false)
    core.check.eq(b.remove(&0u32), Some(0u32))
    core.check.eq(b.len(), 19usize)
    core.mem.drop(std.io.print_line("btree ok"))
    Ok(())
end t.main
"#,
    );
    assert_eq!(out, "btree ok\n");
}
