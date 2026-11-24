//! `std.rand` のネイティブ下ろし (`from_entropy` / `fill_secure`、設計_KIR.md 26 章) と、
//! `Option<Vec<..>>` / `Option<String>` の構造体のフィールド・ペイロードの下ろし (同 26 章)。
//! 木走査 (`ksn run`) と LLVM (debug / release) の出力が一致することを、`agree_with_interpreter` で確かめる。
//! 定義は `main` を先頭に、残りを名前順に並べる (`E0020` の順序検査を通すため。`uses` は名前順)。

mod common;
#[path = "common/scratch.rs"]
mod scratch;

use common::*;

use std::fs;
use std::process::{Command, Output};

/// `u64` を 10 進の文字列にする補助 (`alloc.fmt.write` の `Display`)。
const SHOW: &str = "fn t.show(v: u64) -> alloc.string.String<std.alloc.System> uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.write(&mut s, &v))
    s
end t.show
";

/// `from_seed` の列は木走査とネイティブで同じで、標準の splitmix64 の値に一致する (参照値は独立に計算した列)。
/// 局所の `Rng` を `while` の中で進める形も含む (以前はループの受け渡しから漏れて、同じ値を返していた)。
#[test]
fn rand_from_seed_sequence_matches_tree_walker_and_native_and_splitmix64() {
    let src = format!(
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut r = std.rand.Rng.from_seed(42u64)
    let mut i = 0u32
    while i < 4u32 {{
        let x = r.next_u64()
        let s = t.show(x)
        std.io.print_line(s.as_str())?
        i = i.wrapping_add(1u32)
    }}
    let mut z = std.rand.Rng.from_seed(0u64)
    let first = z.next_u64()
    std.io.print_line(t.show(first).as_str())?
    Ok(())
end t.main

{}",
        SHOW
    );
    let out = agree_with_interpreter("rand_seq", &src);
    assert_eq!(
        out,
        "13679457532755275413\n2949826092126892291\n5139283748462763858\n6349198060258255764\n16294208416658607535\n",
        "{}",
        out
    );
}

/// 局所の `Rng` を `loop` の中で進めると、状態は次の周に残る。木走査とネイティブの結果が一致し、連続する値が同じにならない。
#[test]
fn rand_state_advances_across_loop_iterations_in_native() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut r = std.rand.Rng.from_seed(7u64)
    let a = r.next_u64()
    let mut i = 0u32
    let mut last = 0u64
    let mut same = 0u32
    loop {
        if i == 3u32 {
            break
        }
        let x = r.next_u64()
        if x == last {
            same = same.wrapping_add(1u32)
        }
        last = x
        i = i.wrapping_add(1u32)
    }
    core.check.eq(same, 0u32)
    core.check.eq(a == last, false)
    std.io.print_line(\"loop ok\")?
    Ok(())
end t.main
";
    let out = agree_with_interpreter("rand_loop", src);
    assert_eq!(out, "loop ok\n");
}

/// `fill_secure` と `from_entropy` は呼ぶたびに違う値を返す。2 回の `fill_secure` の先頭 8 バイトは違い、
/// 2 つの `from_entropy` の最初の値も違う。
#[test]
fn rand_entropy_and_fill_secure_differ_between_calls() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console, rand] panics may
    let mut a = [0u8; 16]
    let mut b = [0u8; 16]
    let r1 = std.rand.fill_secure(&mut a)
    let r2 = std.rand.fill_secure(&mut b)
    core.check.is_true(r1 is Ok(_))
    core.check.is_true(r2 is Ok(_))
    let same = a[0] == b[0] && a[1] == b[1]
    let same2 = a[2] == b[2] && a[3] == b[3]
    let also = a[4] == b[4] && a[5] == b[5]
    let also2 = a[6] == b[6] && a[7] == b[7]
    core.check.eq(same && same2 && also && also2, false)
    let mut x = std.rand.Rng.from_entropy()
    let mut y = std.rand.Rng.from_entropy()
    let u = x.next_u64()
    let v = y.next_u64()
    core.check.eq(u == v, false)
    std.io.print_line(\"entropy ok\")?
    Ok(())
end t.main
";
    let out = agree_with_interpreter("rand_entropy", src);
    assert_eq!(out, "entropy ok\n");
}

/// `from_entropy` の種は OS の乱数から来る (固定の定数ではない)。2 回の実行で同じ値になることはまず起きない。
#[test]
fn rand_from_entropy_seeds_are_not_a_fixed_constant() {
    let src = format!(
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, rand] panics may
    let mut x = std.rand.Rng.from_entropy()
    let s = t.show(x.next_u64())
    std.io.print_line(s.as_str())?
    Ok(())
end t.main

{}",
        SHOW
    );
    let t = Tmp::new("rand_fixed_const", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let first = text(&o.stdout);
    assert_ne!(first.trim(), "11400714819323198485", "まだ固定の種を使っている");
    let o2 = run(&["run"], &t.0);
    assert_ne!(text(&o2.stdout), first, "2 回の実行で同じ値 (OS の乱数になっていない)");
}

/// `ksn kir <FQN> --run` (参照実行器) が、乱数を使う関数の値を木走査と同じ値で返す。
#[test]
fn rand_reference_executor_agrees_with_tree_walker_for_a_seeded_function() {
    let src = format!(
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    std.io.print_line(t.show(t.seq(42u64)).as_str())?
    Ok(())
end t.main

fn t.seq(seed: u64) -> u64 uses [] panics never
    let mut r = std.rand.Rng.from_seed(seed)
    let a = r.next_u64()
    let b = r.next_u64()
    let c = r.next_u64()
    a ^ b ^ c
end t.seq

{}",
        SHOW
    );
    let t = Tmp::new("rand_kir_run", &src);
    let tree = run(&["run"], &t.0);
    assert_eq!(tree.status.code(), Some(0), "{}", text(&tree.stderr));
    let k = run(&["kir", "t.seq", "--run", "42"], &t.0);
    assert_eq!(k.status.code(), Some(0), "{}{}", text(&k.stdout), text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), text(&tree.stdout).trim(), "{}", text(&tree.stderr));
}

/// `Option<Vec<u32>>` と `Option<String>` の構造体のフィールド: 作成・代入 (古い値の解放)・`let` の移動・
/// 値の `match` での取り出し (束縛する腕と束縛しない腕)・関数への値渡しを、木走査とネイティブで同じ出力にする。
const OPT_FIELD_SRC: &str = "type t.Store pub
    tok: Option<alloc.vec.Vec<u32, std.alloc.System>> pub
    name: Option<alloc.string.String<std.alloc.System>> pub
    n: u32 pub
end t.Store

fn t.fresh(n: u32) -> t.Store uses [] panics never
    let nm = alloc.string.String.new(std.alloc.SYSTEM)
    t.Store { tok: None, name: Some(nm), n: n }
end t.fresh

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut s = t.fresh(3u32)
    core.check.eq(t.take_len(t.fresh(1u32)), 0usize)
    let mut s2 = t.Store { tok: Some(t.mk(10u32)), name: None, n: 4u32 }
    s2.tok = Some(t.mk(20u32))
    s.tok = Some(t.mk(30u32))
    core.check.eq(s.n, 3u32)
    core.check.eq(s2.n, 4u32)
    let moved = s2.tok
    match moved {
        Some(w) => core.check.eq(w.len(), 2usize)
        None => core.check.eq(1usize, 0usize)
    }
    let nm = s.name
    match nm {
        Some(x) => core.check.eq(x.len(), 0usize)
        None => core.check.eq(1usize, 0usize)
    }
    core.check.eq(t.take_len(t.Store { tok: Some(t.mk(1u32)), name: None, n: 0u32 }), 2usize)
    std.io.print_line(\"option fields ok\")?
    Ok(())
end t.main

fn t.mk(k: u32) -> alloc.vec.Vec<u32, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(k))
    core.mem.drop(v.push(k.wrapping_add(1u32)))
    v
end t.mk

fn t.take_len(s: t.Store) -> usize uses [] panics never
    match s.tok {
        Some(w) => w.len()
        None => 0usize
    }
end t.take_len
";

#[test]
fn option_vec_and_string_fields_match_tree_walker_and_native() {
    let out = agree_with_interpreter("opt_fields", OPT_FIELD_SRC);
    assert_eq!(out, "option fields ok\n");
}

/// 参照実行器 (`ksn kir --run`) も、フィールドの `Option<Vec>` を作って、代入・読み出しができる (スカラーの値で確かめる)。
#[test]
fn option_vec_field_reference_executor_reads_back_the_payload() {
    let src = "fn t.count(k: u32) -> usize uses [alloc] panics never
    let mut s = t.Store { tok: Some(t.mk(k)), n: 4u32 }
    s.tok = None
    let back = s.tok
    match back {
        Some(w) => w.len()
        None => 5usize
    }
end t.count

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.count(3u32), 5usize)
    std.io.print_line(\"ok\")?
    Ok(())
end t.main

type t.Store pub
    tok: Option<alloc.vec.Vec<u32, std.alloc.System>> pub
    n: u32 pub
end t.Store

fn t.mk(k: u32) -> alloc.vec.Vec<u32, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(k))
    core.mem.drop(v.push(k.wrapping_add(1u32)))
    v
end t.mk
";
    let t = Tmp::new("opt_field_kir", src);
    let k = run(&["kir", "t.count", "--run", "3"], &t.0);
    assert_eq!(k.status.code(), Some(0), "{}{}", text(&k.stdout), text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "5");
    let o = run(&["run"], &t.0);
    assert_eq!(text(&o.stdout), "ok\n", "{}", text(&o.stderr));
}

/// `ASan` の実行体を作って走らせる (clang が使えないときと `KSN_NO_ASAN=1` のときは `None`)。`kir_string_keys.rs` と同じ作法。
fn asan_run(t: &Tmp, name: &str) -> Option<Output> {
    if std::env::var("KSN_NO_ASAN").is_ok_and(|v| v == "1") {
        return None;
    }
    let cache = scratch::Scratch::at(format!("ksn_rand_asan_cache_{}_{}", std::process::id(), name));
    let args = "-fsanitize=address -fno-omit-frame-pointer -g -O0";
    let b = Command::new(exe()).args(["build", "--release"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).env("KSN_CLANG_ARGS", args).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    if !b.status.success() {
        return None;
    }
    let dir = t.0.join("target/release");
    let p = dir.join(fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).find(|n| n.to_string_lossy() == "t").expect("native executable"));
    Some(Command::new(&p).env("ASAN_OPTIONS", "detect_leaks=1").output().unwrap())
}

/// `Option<Vec>` / `Option<String>` のフィールドを、作って・代入し直して・移して・値の `match` で取り出して捨てる。
/// ループで 2000 回回しても、ASan の報告 (二重解放・漏れ) がなく、出力は木走査と同じ。
#[test]
fn option_vec_and_string_fields_are_freed_under_asan() {
    let src = "fn t.consume(s: t.Store) -> bool uses [] panics never
    match s.tok {
        Some(w) => w.is_empty()
        None => true
    }
end t.consume

fn t.fresh(k: u32) -> t.Store uses [alloc] panics never
    let mut nm = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(nm.push_str(\"name\"))
    t.Store { tok: Some(t.mk(k)), name: Some(nm), n: k }
end t.fresh

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut i = 0u32
    while i < 2000u32 {
        let mut s = t.fresh(i)
        s.tok = None
        s.name = Some(alloc.string.String.new(std.alloc.SYSTEM))
        s.tok = Some(t.mk(i))
        core.check.eq(t.consume(s), false)
        i = i.wrapping_add(1u32)
    }
    std.io.print_line(\"freed\")?
    Ok(())
end t.main

fn t.mk(k: u32) -> alloc.vec.Vec<u32, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(k))
    core.mem.drop(v.push(k.wrapping_add(1u32)))
    v
end t.mk

type t.Store pub
    tok: Option<alloc.vec.Vec<u32, std.alloc.System>> pub
    name: Option<alloc.string.String<std.alloc.System>> pub
    n: u32 pub
end t.Store
";
    let t = Tmp::new("opt_field_asan", src);
    let b = Command::new(exe()).args(["build", "--release"]).current_dir(&t.0).output().unwrap();
    assert!(b.status.success(), "{}{}", text(&b.stdout), text(&b.stderr));
    let interp = run(&["run"], &t.0);
    assert_eq!(text(&interp.stdout), "freed\n", "{}", text(&interp.stderr));
    if let Some(o) = asan_run(&t, "opt_field") {
        assert_eq!(o.status.code(), Some(0), "asan: {}{}", text(&o.stdout), text(&o.stderr));
        assert!(!text(&o.stderr).contains("ERROR: "), "asan の報告: {}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "freed\n");
    }
}
