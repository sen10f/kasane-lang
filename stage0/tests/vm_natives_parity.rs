//! S6-2 (設計_S5S6_型付きIRの一本化.md 7 章): 組み込み (`natives.rs`) の相当品が、木走査・VM・ネイティブの 3 つで同じ出力になることを確かめる。
//!
//! - 木走査 (`ksn run`) は参照実装。VM (`ksn run --engine=vm`) は KIR を走らせる (`vm/mod.rs`)。ネイティブは同じ KIR を LLVM に下ろしたもの。
//! - 3 つの標準出力と終了コードが一致すること、VM が木走査へ退避していないこと (`vm engine skipped` が出ない) を確かめる。
//! - 対象: 文字列 (`str` の検索・削除・分割と `String::new`)、整数の `wrapping_*` / `checked_*` / `saturating_*` / bit 演算と `min`、
//!   浮動小数の基本、`Vec` の `push` / `pop` / `len`、`HashMap` の `insert` / `get` / `contains_key` / `len`。
//! - 既知の食い違い (`u128` のリテラルが `i128::MAX` を超えると KIR に下ろせず、VM が木走査へ退避する) は、ここでは踏まない
//!   入力を使う。その穴は `実装計画.md` の S6-2 の節に記録する。

#[allow(dead_code, unused_imports)]
mod common;

use common::{build_native_strict, run, text, Tmp};
use std::process::Command;

/// 木走査 (`ksn run`)・VM (`ksn run --engine=vm`)・ネイティブ (LLVM、release) の出力を比べ、木走査の標準出力を返す。
fn three_way(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    let tree = run(&["run"], &t.0);
    assert_eq!(tree.status.code(), Some(0), "木走査: {}{}", text(&tree.stdout), text(&tree.stderr));
    let vm = run(&["run", "--engine=vm"], &t.0);
    let vm_err = text(&vm.stderr);
    assert!(!vm_err.contains("vm engine skipped"), "VM が木走査へ退避した: {}", vm_err);
    assert_eq!(vm.status.code(), Some(0), "VM: {}{}", text(&vm.stdout), vm_err);
    let (bt, exe, out) = build_native_strict(&format!("{}_native", name), src, true);
    let exe = exe.unwrap_or_else(|| panic!("ネイティブの実行ファイルがない: {}", out));
    let nat = Command::new(&exe).current_dir(&bt.0).output().unwrap();
    assert_eq!(nat.status.code(), Some(0), "ネイティブ: {}{}", text(&nat.stdout), text(&nat.stderr));
    let tree_out = text(&tree.stdout);
    assert_eq!(text(&vm.stdout), tree_out, "{}: VM と木走査の出力が違う", name);
    assert_eq!(text(&nat.stdout), tree_out, "{}: ネイティブと木走査の出力が違う", name);
    tree_out
}

#[test]
fn string_helpers_agree_across_tree_vm_and_native() {
    let out = three_way("vm_natives_str", STRINGS);
    assert!(out.ends_with("end\n"), "{}", out);
}

#[test]
fn numeric_helpers_agree_across_tree_vm_and_native() {
    let out = three_way("vm_natives_num", NUMBERS);
    assert!(out.ends_with("end\n"), "{}", out);
}

/// `std.fs` (作成・書き込み・読み出し・メタデータ・ディレクトリの列挙・改名・削除) を、3 つの入口で比べる。
/// 作業は一時ディレクトリの中だけで行い、終わったら消す。
#[test]
fn fs_helpers_agree_across_tree_vm_and_native() {
    let dir = std::env::temp_dir().join(format!("vm_natives_fs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let d = dir.to_string_lossy().replace('\\', "/");
    let src = FS.replace("@DIR@", &d);
    let out = three_way("vm_natives_fs", &src);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.ends_with("end\n"), "{}", out);
}

#[test]
fn collection_helpers_agree_across_tree_vm_and_native() {
    let out = three_way("vm_natives_coll", COLLECTIONS);
    assert!(out.ends_with("end\n"), "{}", out);
}

const STRINGS: &str = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let s = "  hello, world  "
    std.io.print_line(s.trim())?
    std.io.print_line(s.trim_start())?
    std.io.print_line(s.trim_end())?
    core.check.eq(s.trim().strip_prefix("hello"), Some(", world"))
    core.check.eq(s.trim().strip_suffix("world"), Some("hello, "))
    core.check.eq(s.trim().strip_suffix("zzz"), None)
    core.check.eq(s.trim().starts_with("hel"), true)
    core.check.eq(s.trim().ends_with("rld"), true)
    core.check.eq(s.trim().contains("o, w"), true)
    core.check.eq(s.trim().find("o"), Some(4usize))
    core.check.eq(s.trim().rfind("o"), Some(8usize))
    core.check.eq(s.trim().len(), 12usize)
    core.check.eq("".is_empty(), true)
    core.check.eq("ABC".eq_ignore_ascii_case("abc"), true)
    core.check.eq("k=v".split_once("="), Some(("k", "v")))
    core.check.eq(s.trim().as_bytes().len(), 12usize)
    let owned = alloc.string.String.new(std.alloc.SYSTEM)
    core.check.eq(owned.is_empty(), true)
    std.io.print_line("end")?
    Ok(())
end t.main
"#;

const NUMBERS: &str = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = 200u8
    core.check.eq(a.wrapping_add(100u8), 44u8)
    core.check.eq(a.checked_add(100u8), None)
    core.check.eq(a.checked_add(10u8), Some(210u8))
    core.check.eq(a.saturating_add(100u8), 255u8)
    core.check.eq(a.count_ones(), 3u32)
    core.check.eq(a.leading_zeros(), 0u32)
    core.check.eq(a.trailing_zeros(), 3u32)
    core.check.eq(7i32.min(3i32), 3i32)
    core.check.eq(7i64.wrapping_mul(3i64), 21i64)
    let big = 170_141_183_460_469_231_731_687_303_715_884_105_727u128
    core.check.eq(big.wrapping_add(2u128), 170_141_183_460_469_231_731_687_303_715_884_105_729u128)
    core.check.eq(big.wrapping_mul(2u128).wrapping_add(2u128), 0u128)
    let lo = -170_141_183_460_469_231_731_687_303_715_884_105_727i128
    core.check.eq(lo - 1i128, -170_141_183_460_469_231_731_687_303_715_884_105_728i128)
    core.check.eq(3u8.successor(), Some(4u8))
    core.check.eq(255u8.successor(), None)
    core.check.eq((-2i32).successor(), Some(-1i32))
    core.check.eq(9223372036854775807i64.successor(), None)
    core.check.eq(big.wrapping_add(2u128) == 0u128, false)
    core.check.eq(1.5f64.floor(), 1.0f64)
    core.check.eq(2.0f64.sqrt() * 2.0f64.sqrt() > 1.99f64, true)
    core.check.eq(0.0f64.is_nan(), false)
    core.check.eq(1.5f64.max(2.5f64), 2.5f64)
    std.io.print_line("end")?
    Ok(())
end t.main
"#;

const COLLECTIONS: &str = r#"fn t.count_bytes(text: &str) -> alloc.collections.HashMap<u8, u32, std.alloc.System> uses [alloc] panics never
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let bytes = text.as_bytes()
    let mut i = 0usize
    while i < bytes.len() {
        match bytes.get(i) {
            Some(c) => {
                match m.insert(*c, 1u32) {
                    Ok(None) => ()
                    Ok(Some(old)) => match m.insert(*c, old.wrapping_add(1u32)) {
                        Ok(None) => ()
                        Ok(Some(_old2)) => ()
                        Err(_e) => ()
                    }
                    Err(_e) => ()
                }
                i = i.wrapping_add(1usize)
            }
            None => break
        }
    }
    m
end t.count_bytes

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    match v.push(3u32) {
        Ok(_) => ()
        Err(_e) => core.panic.panic("oom")
    }
    match v.push(1u32) {
        Ok(_) => ()
        Err(_e) => core.panic.panic("oom")
    }
    core.check.eq(v.len(), 2usize)
    core.check.eq(v.is_empty(), false)
    core.check.eq(v.pop(), Some(1u32))
    core.check.eq(v.len(), 1usize)
    let m = t.count_bytes("hello")
    match m.get(&108u8) {
        Some(c) => core.check.eq(*c, 2u32)
        None => core.check.eq(0usize, 1usize)
    }
    core.check.eq(m.contains_key(&104u8), true)
    core.check.eq(m.contains_key(&122u8), false)
    core.check.eq(m.len(), 4usize)
    std.io.print_line("end")?
    Ok(())
end t.main
"#;

const FS: &str = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.fs, io.console] panics may
    let dir = "@DIR@"
    std.fs.create_dir_all(dir)?
    let p = "@DIR@/a.txt"
    std.fs.write_atomic(p, b"hello\n")?
    core.check.eq(std.fs.metadata(p)?.size, 6u64)
    core.check.eq(std.fs.exists(p), true)
    let mut data = alloc.vec.Vec.new(std.alloc.SYSTEM)
    with mut file = std.fs.open(p)? close core.mem.drop(file.close()) {
        core.mem.drop(std.fs.read_to_end(&mut file, &mut data)?)
    }
    core.check.eq(data.len(), 6usize)
    let ents = std.fs.read_dir(dir)?
    core.check.eq(ents.len(), 1usize)
    core.check.eq(std.fs.exists("@DIR@/nope"), false)
    std.fs.remove_file(p)?
    core.check.eq(std.fs.exists(p), false)
    std.io.print_line("end")?
    Ok(())
end t.main
"#;
