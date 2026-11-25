//! `HashMap` の境界の振る舞い (成長・削除して再挿入・削除後の反復順・衝突の多い表) を木走査とネイティブで比べる (std/hashmap-onealloc)。
//! 木走査 (`ksn run --engine=tree`) と `ksn build --release` (退避なし) の出力が一致することを見る。

#[path = "common/scratch.rs"]
mod scratch;

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
        let d = std::env::temp_dir().join(format!("ksn_mapedge_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        let toml = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";
        fs::write(d.join("kasane.toml"), toml).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(d.join("src").join("t.ksnl"), src).unwrap();
        // 書き方は 1 通り (E0020): 試験の入力は `ksn fmt` の出力にそろえる。
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_ksn")).arg("fmt").current_dir(&d).output();
        // 効果の宣言は本体の効果と完全に一致させる (D5・D13): 固定の見出し (`main_wrap`) などを直す。
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_ksn")).arg("fix").env("KSN_FIX_ONLY", "E4020,E4025,E2055,E2057,E2058,E2059,E2060,E2061,E2062,E2063,E2064,E2065,E2066,E2067,E2068,E2069,E2070,E2071,E2072,E2073").current_dir(&d).output();
        Tmp(d)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        scratch::remove(&self.0);
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
    let cache = scratch::Scratch::at(format!("ksn_mapedge_cache_{}_{}", std::process::id(), name));
    let b = Command::new(exe()).args(["build", "--release"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    assert!(b.status.success(), "ネイティブにならない (退避なし): {}{}", text(&b.stdout), text(&b.stderr));
    let p = t.0.join("target/release").join("t");
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), tree.status.code(), "終了コード: {}", text(&o.stderr));
    assert_eq!(text(&o.stdout), text(&tree.stdout), "ネイティブの出力が木走査と違う");
    text(&tree.stdout)
}

const PRE: &str = r#"fn t.put(m: &mut alloc.collections.HashMap<u64, u64, std.alloc.System>, k: u64, v: u64) -> () uses [alloc] panics may
    match m.insert(k, v) {
        Ok(_o) => {
        }
        Err(_e) => {
            core.panic.panic("alloc")
        }
    }
end t.put

fn t.key(i: u64) -> u64 uses [] panics never
    i.wrapping_mul(11_400_714_819_323_198_485u64)
end t.key

"#;

fn prog(main: &str) -> String {
    format!("{}{}", PRE, main)
}

#[test]
fn growth_across_many_resizes_keeps_every_entry_and_insertion_order() {
    let out = same(
        "growth",
        &prog(r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0u64
    while i < 40_000u64 {
        t.put(&mut m, t.key(i), i)
        i = i.wrapping_add(1u64)
    }
    core.check.eq(m.len(), 40_000usize)
    i = 0u64
    while i < 40_000u64 {
        let k = t.key(i)
        core.check.eq(m.get(&k), Some(&i))
        let miss = t.key(i.wrapping_add(40_000u64))
        core.check.eq(m.contains_key(&miss), false)
        i = i.wrapping_add(1u64)
    }
    let mut idx = 0u64
    for (key, val) in m.iter() {
        core.check.eq(*key, t.key(idx))
        core.check.eq(*val, idx)
        idx = idx.wrapping_add(1u64)
    }
    core.check.eq(idx, 40_000u64)
    core.mem.drop(std.io.print_line("growth ok"))
    Ok(())
end t.main
"#),
    );
    assert_eq!(out, "growth ok\n");
}

#[test]
fn remove_then_reinsert_moves_the_entry_to_the_end() {
    let out = same(
        "reinsert",
        &prog(r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0u64
    while i < 5_000u64 {
        t.put(&mut m, i, i)
        i = i.wrapping_add(1u64)
    }
    i = 0u64
    while i < 5_000u64 {
        core.check.eq(m.remove(&i), Some(i))
        i = i.wrapping_add(2u64)
    }
    core.check.eq(m.len(), 2_500usize)
    i = 0u64
    while i < 5_000u64 {
        core.check.eq(m.contains_key(&i), i & 1u64 == 1u64)
        i = i.wrapping_add(1u64)
    }
    core.check.eq(m.remove(&0u64), None)
    i = 0u64
    while i < 5_000u64 {
        t.put(&mut m, i, i.wrapping_add(1_000_000u64))
        i = i.wrapping_add(2u64)
    }
    core.check.eq(m.len(), 5_000usize)
    let mut pos = 0u64
    for (key, val) in m.iter() {
        if pos < 2_500u64 {
            let want = pos.wrapping_mul(2u64).wrapping_add(1u64)
            core.check.eq(*key, want)
            core.check.eq(*val, want)
        } else {
            let want = pos.wrapping_sub(2_500u64).wrapping_mul(2u64)
            core.check.eq(*key, want)
            core.check.eq(*val, want.wrapping_add(1_000_000u64))
        }
        pos = pos.wrapping_add(1u64)
    }
    core.check.eq(pos, 5_000u64)
    core.mem.drop(std.io.print_line("reinsert ok"))
    Ok(())
end t.main
"#),
    );
    assert_eq!(out, "reinsert ok\n");
}

#[test]
fn iteration_order_after_removals_skips_tombstones_and_survives_growth() {
    let out = same(
        "removals",
        &prog(r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0u64
    while i < 3_000u64 {
        t.put(&mut m, t.key(i), i)
        if i % 3u64 == 1u64 {
            let old = t.key(i.wrapping_sub(1u64))
            core.mem.drop(m.remove(&old))
        }
        i = i.wrapping_add(1u64)
    }
    let mut want = 0u64
    let mut seen = 0usize
    for (key, val) in m.iter() {
        while want % 3u64 == 0u64 && want < 3_000u64 {
            want = want.wrapping_add(1u64)
        }
        core.check.eq(*val, want)
        core.check.eq(*key, t.key(want))
        want = want.wrapping_add(1u64)
        seen = seen.wrapping_add(1usize)
    }
    core.check.eq(seen, m.len())
    core.check.eq(seen, 2_000usize)
    core.mem.drop(std.io.print_line("removals ok"))
    Ok(())
end t.main
"#),
    );
    assert_eq!(out, "removals ok\n");
}

#[test]
fn random_churn_in_a_dense_table_matches_a_model() {
    let out = same(
        "churn",
        &prog(r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut model = [0u64; 1024]
    let mut x = 12_345u64
    let mut stamp = 0u64
    let mut step = 0u32
    while step < 60_000u32 {
        x = x.wrapping_mul(6_364_136_223_846_793_005u64).wrapping_add(1_442_695_040_888_963_407u64)
        let key = x.wrapping_shr(40u32) & 1_023u64
        let slot = usize.from_u64(key)
        let op = x.wrapping_shr(20u32) & 3u64
        if op < 2u64 {
            if model[slot] != 0u64 {
                core.check.eq(m.remove(&key), Some(model[slot]))
            }
            stamp = stamp.wrapping_add(1u64)
            t.put(&mut m, key, stamp)
            model[slot] = stamp
        } else if op == 2u64 {
            if model[slot] != 0u64 {
                core.check.eq(m.remove(&key), Some(model[slot]))
                model[slot] = 0u64
            } else {
                core.check.eq(m.remove(&key), None)
            }
        } else {
            if model[slot] != 0u64 {
                core.check.eq(m.get(&key), Some(&model[slot]))
            } else {
                core.check.eq(m.contains_key(&key), false)
            }
        }
        step = step.wrapping_add(1u32)
    }
    let mut live = 0usize
    let mut last = 0u64
    for (key, val) in m.iter() {
        core.check.eq(model[usize.from_u64(*key)], *val)
        core.check.eq(*val > last, true)
        last = *val
        live = live.wrapping_add(1usize)
    }
    let mut count = 0usize
    for v in model.iter() {
        if *v != 0u64 {
            count = count.wrapping_add(1usize)
        }
    }
    core.check.eq(live, count)
    core.check.eq(m.len(), count)
    core.mem.drop(std.io.print_line("churn ok"))
    Ok(())
end t.main
"#),
    );
    assert_eq!(out, "churn ok\n");
}
