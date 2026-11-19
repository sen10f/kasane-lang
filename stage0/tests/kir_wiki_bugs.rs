//! `apps/wiki` の作業で報告された 3 件の回帰テスト (`apps/wiki/FRICTION.md` の 19〜21 行)。
//!  1. `std.rand.fill_secure` の後に文字列リテラルを関数の引数へ渡す (データ領域の破壊の報告)。
//!  2. 空のスライスに `core.slice.sort` を呼ぶ (ネイティブで segfault の報告)。
//!  3. 受け手が組み込みの Vec で、Kasane 側に本体のあるメソッド (`dedup` / `append` / `extend_from_slice`) と、
//!     利用者のトレイト実装 (`impl ... for alloc.vec.Vec<u8, ..>`)。`try_mcall` の `method_on` が `Err` を返すと、
//!     `user_mcall` へ回らない、という報告。
//!
//! 2026-11-15 の時点で、報告の手順 (と、その周辺の形: 配列版、`for` 版、空の `Vec` の `truncate` / `reserve` 後、
//! 関数引数、`sort_unstable` / `reverse` / `binary_search`) は、どれも再現しなかった。ここでは、現在の処理系で
//! run と build (debug / release) の出力が一致すること、ASan の報告がないことを固定する。
//! 木走査 (`ksn run`) と LLVM (debug / release) の一致は `agree_with_interpreter` で確かめる。

mod common;
use common::*;

use std::fs;
use std::process::{Command, Output};

/// ASan の実行体を作って走らせる (clang が使えないとき、または `KSN_NO_ASAN=1` のときは `None`)。
fn asan_run(t: &Tmp, name: &str) -> Option<Output> {
    if !asan_available() {
        return None;
    }
    let cache = std::env::temp_dir().join(format!("ksn_wiki_bugs_asan_cache_{}_{}", std::process::id(), name));
    let args = "-fsanitize=address -fno-omit-frame-pointer -g -O0";
    let b = Command::new(exe()).args(["build", "--release"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).env("KSN_CLANG_ARGS", args).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    if !b.status.success() {
        return None;
    }
    let dir = t.0.join("target/release");
    let p = dir.join(fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).find(|n| n.to_string_lossy().ends_with(".native")).expect("native executable"));
    Some(Command::new(&p).env("ASAN_OPTIONS", "detect_leaks=1").output().unwrap())
}

/// 報告 1: `fill_secure` を何度も回し、そのたびに文字列リテラルを作って関数へ渡す。ASan でも報告がない。
#[test]
fn fill_secure_then_literal_arguments_are_intact() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, rand] panics may
    let mut i = 0usize
    while i < 60usize {
        let n = i % 40usize + 1usize
        let mut buf = alloc.vec.Vec.new(std.alloc.SYSTEM)
        let mut k = 0usize
        while k < n {
            core.mem.drop(buf.push(0u8))
            k = k + 1usize
        }
        std.rand.fill_secure(buf.as_mut_slice())?
        let s = match alloc.string.String.from_str(std.alloc.SYSTEM, \"literal\") {
            Ok(v) => v
            Err(_) => core.panic.panic(\"alloc\")
        }
        core.check.eq(s.len(), 7usize)
        i = i + 1usize
    }
    t.show(\"literal\")?
    t.show(\"after\")?
    Ok(())
end t.main

fn t.show(s: &str) -> Result<(), std.io.Error> uses [io.console] panics never
    std.io.print_line(s)?
    Ok(())
end t.show
";
    let out = agree_with_interpreter("wiki_bug_fill_secure", src);
    assert_eq!(out, "literal\nafter\n");
    let t = Tmp::new("wiki_bug_fill_secure_asan", src);
    if let Some(a) = asan_run(&t, "fill") {
        let err = text(&a.stderr);
        assert!(!err.contains("AddressSanitizer") && !err.contains("LeakSanitizer"), "ASan が報告した\n{}", err);
        assert_eq!(a.status.code(), Some(0), "{}", err);
        assert_eq!(text(&a.stdout), out);
    }
}

/// 報告 2: 空のスライスに `core.slice.sort` を呼ぶ。空の `Vec` (押していない・`truncate(0)`・`reserve` だけ)、
/// 空の配列、関数の引数として受けた空のスライス。どれも run と build が一致し、クラッシュしない。
#[test]
fn sort_on_empty_slices_does_not_crash() {
    let cases: &[(&str, &str)] = &[
        (
            "wiki_bug_sort_empty_vec",
            "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.slice.sort(v.as_mut_slice())
    std.io.print_line(\"sorted\")?
    Ok(())
end t.main
",
        ),
        (
            "wiki_bug_sort_empty_array",
            "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never
    let mut a = [0u32; 0]
    core.slice.sort(&mut a)
    std.io.print_line(\"sorted\")?
    Ok(())
end t.main
",
        ),
        (
            "wiki_bug_sort_truncated_and_reserved",
            "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(5u32))
    core.mem.drop(v.truncate(0usize))
    core.mem.drop(v.reserve(16usize))
    core.slice.sort(v.as_mut_slice())
    core.slice.sort_unstable(v.as_mut_slice())
    core.slice.reverse(v.as_mut_slice())
    let r = core.slice.binary_search(v.as_slice(), &7u32)
    core.check.eq(r.is_err(), true)
    std.io.print_line(\"sorted\")?
    Ok(())
end t.main
",
        ),
        (
            "wiki_bug_sort_empty_arg",
            "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    t.srt(v.as_mut_slice())
    std.io.print_line(\"sorted\")?
    Ok(())
end t.main

fn t.srt(s: &mut [u32]) -> () uses [] panics never
    core.slice.sort(s)
end t.srt
",
        ),
    ];
    for (name, src) in cases {
        let out = agree_with_interpreter(name, src);
        assert_eq!(out, "sorted\n", "{}", name);
    }
}

/// 報告 3: Kasane 側に本体のあるメソッド (`dedup` / `append` / `extend_from_slice`) と、
/// 利用者のトレイト実装 (`Vec<u8>` の `show`) を、受け手が組み込みの Vec のまま呼ぶ。`user_mcall` と同じ結果になる。
#[test]
fn kasane_side_vec_methods_and_trait_impls_lower_natively() {
    let src = "trait t.Show pub
    fn show(self: &Self) -> u32 uses [] panics never
end t.Show

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(1u8))
    let n = v.show()
    core.check.eq(n, 3u32)
    let mut w: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(w.push(4u32))
    core.mem.drop(w.push(4u32))
    core.mem.drop(w.dedup())
    let mut x: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(x.push(9u32))
    core.mem.drop(w.append(&mut x))
    core.mem.drop(w.extend_from_slice(x.as_slice()))
    core.check.eq(w.len(), 2usize)
    std.io.print_line(\"kasane-side ok\")?
    Ok(())
end t.main

impl t.Show for alloc.vec.Vec<u8, std.alloc.System>
fn t.Show.alloc.vec.Vec.show(self: &alloc.vec.Vec<u8, std.alloc.System>) -> u32 uses [] panics never impls t.Show
    if self.is_empty() {
        return 100u32
    }
    3u32
end t.Show.alloc.vec.Vec.show
";
    let out = agree_with_interpreter("wiki_bug_kasane_side_mcall", src);
    assert_eq!(out, "kasane-side ok\n");
}
