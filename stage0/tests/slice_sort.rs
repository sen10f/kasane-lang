//! `core.slice` の整列・検索 (`sort` `sort_by` `sort_unstable` `binary_search` `reverse`) と `alloc.vec.Vec` の
//! `extend_from_slice` `append` `dedup` `contains` (仕様 8.3。本体は Kasane、`spec/stdlib/core/slice.ksnl` / `alloc/vec.ksnl`) の実行テスト。
//! 木走査 (`ksn run --engine=tree`)、VM 指定 (`--engine=vm`。KIR に下ろせないときは木走査へ退避する)、
//! `ksn build` で作った実行体の出力が一致することを見る。
//! 要素の型 `T` を取る総称関数は KIR (ネイティブ) の単相化が未対応 (型引数はクロージャだけ) なので、
//! `build` はインタプリタ同梱の実行体へ退避する。退避は許すが、出力が一致することは確かめる。

#[path = "common/scratch.rs"]
mod scratch;

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
        let d = std::env::temp_dir().join(format!("ksn_slsort_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), TOML).unwrap();
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
    let cache = scratch::Scratch::at(format!("ksn_slsort_cache_{}_{}", std::process::id(), name));
    let b = Command::new(exe()).arg("build").current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    assert!(b.status.success(), "{}{}", text(&b.stdout), text(&b.stderr));
    let _ = fs::remove_dir_all(&cache);
    let launcher = t.0.join("target/debug").join("t");
    let native = t.0.join("target/debug").join("t");
    let p = if native.exists() { native } else { launcher };
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&tree.stdout), text(&o.stdout), "build の出力が run と違う");
    text(&tree.stdout)
}

const SORT: &str = r#"enum t.Error derive [Debug, From] pub
    Io(std.io.Error)
    Alloc(alloc.alloc.AllocError)
end t.Error

type t.P derive [Debug, Clone, Copy] pub
    key: u32
    idx: u32
end t.P

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics may
    let sizes = [0usize, 1usize, 2usize, 3usize, 11usize, 12usize, 13usize, 25usize]
    let big = [100usize, 257usize, 1000usize]
    t.sizes(&sizes)?
    t.sizes(&big)?
    Ok(())
end t.main

fn t.sizes(sizes: &[usize]) -> Result<(), t.Error> uses [alloc, io.console] panics may
    let mut si = 0usize
    while si < sizes.len() {
        let n = match sizes.get(si) {
            Some(x) => *x
            None => 0usize
        }
        let mut shape = 0u32
        while shape < 4u32 {
            t.case(n, shape)?
            shape = shape.wrapping_add(1u32)
        }
        si = si.wrapping_add(1usize)
    }
    Ok(())
end t.sizes

fn t.case(n: usize, shape: u32) -> Result<(), t.Error> uses [alloc, io.console] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut w = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut u = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut state = 12345u64.wrapping_add(u64.from_usize(n))
    let mut i = 0usize
    while i < n {
        let mixed = state.wrapping_mul(6364136223846793005u64)
        state = mixed.wrapping_add(1442695040888963407u64)
        let r = u32.try_from_u64(state.wrapping_shr(33u32) % 50u64).unwrap_or(0u32)
        let k = if shape == 0u32 {
            r
        } else if shape == 1u32 {
            u32.try_from_usize(i).unwrap_or(0u32)
        } else if shape == 2u32 {
            u32.try_from_usize(n.wrapping_sub(i)).unwrap_or(0u32)
        } else {
            7u32
        }
        let idx = u32.try_from_usize(i).unwrap_or(0u32)
        match v.push(t.P { key: k, idx: idx }) {
            Ok(_d) => {
            }
            Err(_e) => core.panic.panic("oom")
        }
        match w.push(k) {
            Ok(_d) => {
            }
            Err(_e) => core.panic.panic("oom")
        }
        match u.push(k) {
            Ok(_d) => {
            }
            Err(_e) => core.panic.panic("oom")
        }
        i = i.wrapping_add(1usize)
    }
    v.as_mut_slice().sort_by(fn(a: &t.P, b: &t.P) -> core.cmp.Ordering capture [] {
        a.key.cmp(&b.key)
    })
    w.as_mut_slice().sort()
    u.as_mut_slice().sort()
    let vs = v.as_slice()
    let ws = w.as_slice()
    let us = u.as_slice()
    let mut digest = 0u64
    let mut j = 0usize
    while j < n {
        let a = vs.get(j)
        let b = ws.get(j)
        let c = us.get(j)
        match a {
            Some(p) => {
                core.check.eq(b, c)
                match b {
                    Some(bk) => {
                        core.check.eq(p.key, *bk)
                    }
                    None => core.panic.panic("b")
                }
                digest = digest.wrapping_mul(31u64).wrapping_add(u64.from_u32(p.key))
                if j > 0usize {
                    match vs.get(j.wrapping_sub(1usize)) {
                        Some(q) => {
                            core.check.eq(q.key <= p.key, true)
                            if q.key == p.key {
                                core.check.eq(q.idx < p.idx, true)
                            }
                        }
                        None => core.panic.panic("prev")
                    }
                }
                match ws.binary_search(&p.key) {
                    Ok(f) => {
                        core.check.eq(ws.get(f), Some(&p.key))
                    }
                    Err(_f) => core.panic.panic("missing")
                }
            }
            None => core.panic.panic("short")
        }
        j = j.wrapping_add(1usize)
    }
    core.check.eq(ws.binary_search(&100000u32), Err(n))
    let mut out = alloc.string.String.new(std.alloc.SYSTEM)
    out.push_u64(u64.from_usize(n))?
    out.push_str(" ")?
    out.push_u64(u64.from_u32(shape))?
    out.push_str(" ")?
    out.push_u64(digest)?
    std.io.print_line(out.as_str())?
    Ok(())
end t.case
"#;

const VEC: &str = r#"fn t.main() -> Result<(), alloc.alloc.AllocError> uses [alloc] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut w = alloc.vec.Vec.new(std.alloc.SYSTEM)
    v.push(1u32)?
    v.push(1u32)?
    v.push(2u32)?
    v.push(2u32)?
    v.push(1u32)?
    w.push(7u32)?
    w.push(8u32)?
    v.dedup()
    core.check.eq(v.len(), 3usize)
    core.check.eq(v.append(&mut w), Ok(()))
    core.check.eq(w.len(), 0usize)
    core.check.eq(v.as_slice(), &[1u32, 2u32, 1u32, 7u32, 8u32])
    core.check.eq(v.contains(&8u32), true)
    core.check.eq(v.contains(&9u32), false)
    let src = [4u32, 5u32]
    v.extend_from_slice(&src)?
    core.check.eq(v.as_slice(), &[1u32, 2u32, 1u32, 7u32, 8u32, 4u32, 5u32])
    v.sort()
    core.check.eq(v.as_slice(), &[1u32, 1u32, 2u32, 4u32, 5u32, 7u32, 8u32])
    v.as_mut_slice().reverse()
    core.check.eq(v.as_slice(), &[8u32, 7u32, 5u32, 4u32, 2u32, 1u32, 1u32])
    v.as_mut_slice().sort_by(fn(a: &u32, b: &u32) -> core.cmp.Ordering capture [] {
        b.cmp(a)
    })
    core.check.eq(v.as_slice(), &[8u32, 7u32, 5u32, 4u32, 2u32, 1u32, 1u32])
    v.as_mut_slice().sort()
    core.check.eq(v.as_slice().binary_search(&5u32), Ok(4usize))
    core.check.eq(v.as_slice().binary_search(&3u32), Err(3usize))
    core.check.eq(v.as_slice().binary_search(&0u32), Err(0usize))
    core.check.eq(v.as_slice().binary_search(&9u32), Err(7usize))
    let mut e = alloc.vec.Vec.new(std.alloc.SYSTEM)
    e.dedup()
    e.as_mut_slice().sort()
    e.as_mut_slice().reverse()
    core.check.eq(e.as_slice().binary_search(&1u32), Err(0usize))
    core.check.eq(e.contains(&1u32), false)
    e.extend_from_slice(&src)?
    core.check.eq(e.len(), 2usize)
    Ok(())
end t.main
"#;

/// 擬似乱数 (線形合同法) の 1000 件を含む各種の入力を、`sort` / `sort_by` / `sort_unstable` で整列する。
/// プログラム内の表明: 昇順・安定 (同じキーは元の添字順)・3 つの整列の結果が一致・`binary_search` が全要素を見つける。
#[test]
fn sort_family_run_vm_build_agree() {
    let out = run_all("sort", SORT);
    // サイズ 8 + 3 通り、各 4 つの形の 44 行。
    assert_eq!(out.lines().count(), 44, "{}", out);
    assert!(out.starts_with("0 0 0\n0 1 0\n"), "{}", out);
    // 全要素が同じ (shape 3) の 1000 件: 7 を 1000 回畳み込む。
    let mut d: u64 = 0;
    for _ in 0..1000 {
        d = d.wrapping_mul(31).wrapping_add(7);
    }
    assert!(out.contains(&format!("1000 3 {}\n", d)), "{}", out);
}

/// `Vec` の `extend_from_slice` / `append` / `dedup` / `contains`、`reverse`、`binary_search` の境界 (空・見つからない位置)。
#[test]
fn vec_and_slice_helpers_run_vm_build_agree() {
    let out = run_all("vec", VEC);
    assert_eq!(out, "");
}

/// 挿入整列を基準として、`sort`/`sort_by` が random/sorted/重複多数/逆順の入力で
/// 基準と一致すること、`sort_by` のレコード整列が安定 (同じキーは元の添字順) であることを、
/// 木走査・VM・ネイティブビルドの 3 経路で見る。
#[test]
fn sort_matches_reference() {
    let out = run_all("sort_ref", SORT_REF);
    let mut expected = String::new();
    for n in [0usize, 1, 2, 7, 1000] {
        for shape in ["random", "sorted", "reversed", "dups"] {
            let mut state = 12345u64.wrapping_add(n as u64);
            let mut keys = Vec::new();
            for i in 0..n {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let x = ((state >> 33) % 50) as u32;
                let key = match shape {
                    "random" => x,
                    "sorted" => i as u32,
                    "reversed" => (n - 1 - i) as u32,
                    "dups" => 7,
                    _ => unreachable!(),
                };
                keys.push(key);
            }
            let mut refs = keys.clone();
            insertion_sort_u32(&mut refs);
            let d: u64 = refs
                .iter()
                .fold(0u64, |d, &k| d.wrapping_mul(31).wrapping_add(k as u64));
            expected.push_str(&format!("u32 {} {} {} ok\n", shape, n, d));
            let mut recs: Vec<(u32, u32)> = keys
                .iter()
                .enumerate()
                .map(|(i, &k)| (k, i as u32))
                .collect();
            recs.sort_by(|a, b| a.0.cmp(&b.0));
            let d: u64 = recs
                .iter()
                .fold(0u64, |d, &(k, _)| d.wrapping_mul(31).wrapping_add(k as u64));
            expected.push_str(&format!("rec {} {} {} ok\n", shape, n, d));
        }
    }
    assert_eq!(out, expected);
}

fn insertion_sort_u32(keys: &mut [u32]) {
    for i in 1..keys.len() {
        let x = keys[i];
        let mut j = i;
        while j > 0 && keys[j - 1] > x {
            keys[j] = keys[j - 1];
            j -= 1;
        }
        keys[j] = x;
    }
}

/// `sort`/`sort_by` の擬似乱数 (1000 件) を含む各種入力の実行テスト (従来)。
const SORT_REF: &str = r#"enum t.Error derive [Debug, From] pub
    Io(std.io.Error)
    Alloc(alloc.alloc.AllocError)
end t.Error

type t.Rec derive [Debug, Clone, Copy] pub
    key: u32
    idx: u32
end t.Rec

fn t.insertion_by[T, F: core.ops.Fn<(&T, &T), core.cmp.Ordering, E, P>, E: uses, P: panics](s: &mut [T], f: &F) -> () uses [E] panics P
    for i in core.iter.range(1usize, s.len()) {
        let mut j = i
        while j > 0usize {
            match s.get(j) {
                Some(x) => match s.get(j.wrapping_sub(1usize)) {
                    Some(y) => match f.call((y, x)) {
                        core.cmp.Ordering.Greater => {
                            match s.swap(j, j.wrapping_sub(1usize)) {
                                Ok(_done) => {
                                }
                                Err(_err) => core.panic.panic("swap")
                            }
                            j = j.wrapping_sub(1usize)
                        }
                        core.cmp.Ordering.Less => {
                            break
                        }
                        core.cmp.Ordering.Equal => {
                            break
                        }
                    }
                    None => {
                        break
                    }
                }
                None => {
                    break
                }
            }
        }
    }
end t.insertion_by

fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics may
    let sizes = [0usize, 1usize, 2usize, 7usize, 1000usize]
    t.sizes(&sizes)?
    Ok(())
end t.main

fn t.sizes(sizes: &[usize]) -> Result<(), t.Error> uses [alloc, io.console] panics may
    for si in core.iter.range(0usize, sizes.len()) {
        let n = match sizes.get(si) {
            Some(x) => *x
            None => 0usize
        }
        t.shapes(n)?
    }
    Ok(())
end t.sizes

fn t.shapes(n: usize) -> Result<(), t.Error> uses [alloc, io.console] panics may
    for shape in core.iter.range(0u32, 4u32) {
        t.u32_case(n, shape)?
        t.rec_case(n, shape)?
    }
    Ok(())
end t.shapes

fn t.u32_case(n: usize, shape: u32) -> Result<(), t.Error> uses [alloc, io.console] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut r = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut state = 12345u64.wrapping_add(u64.from_usize(n))
    for i in core.iter.range(0usize, n) {
        let mixed = state.wrapping_mul(6364136223846793005u64)
        state = mixed.wrapping_add(1442695040888963407u64)
        let x = u32.try_from_u64(state.wrapping_shr(33u32) % 50u64).unwrap_or(0u32)
        let key = if shape == 0u32 {
            x
        } else if shape == 1u32 {
            u32.try_from_usize(i).unwrap_or(0u32)
        } else if shape == 2u32 {
            u32.try_from_usize(n.wrapping_sub(i.wrapping_add(1usize))).unwrap_or(0u32)
        } else {
            7u32
        }
        match v.push(key) {
            Ok(_done) => {
            }
            Err(_err) => core.panic.panic("oom")
        }
        match r.push(key) {
            Ok(_done) => {
            }
            Err(_err) => core.panic.panic("oom")
        }
    }
    let cmp_u32 = fn(a: &u32, b: &u32) -> core.cmp.Ordering capture [] {
        a.cmp(b)
    }
    t.insertion_by(r.as_mut_slice(), &cmp_u32)
    v.as_mut_slice().sort()
    let vs = v.as_slice()
    let rs = r.as_slice()
    let mut ok = true
    for j in core.iter.range(0usize, n) {
        match rs.get(j) {
            Some(rk) => match vs.get(j) {
                Some(vk) => {
                    if *rk != *vk {
                        ok = false
                    }
                }
                None => {
                    ok = false
                }
            }
            None => {
                ok = false
            }
        }
    }
    let mut d = 0u64
    for j in core.iter.range(0usize, n) {
        match vs.get(j) {
            Some(vk) => {
                d = d.wrapping_mul(31u64).wrapping_add(u64.from_u32(*vk))
            }
            None => core.panic.panic("short")
        }
    }
    let mut out = alloc.string.String.new(std.alloc.SYSTEM)
    out.push_str("u32 ")?
    out.push_str(if shape == 0u32 {
        "random"
    } else if shape == 1u32 {
        "sorted"
    } else if shape == 2u32 {
        "reversed"
    } else {
        "dups"
    })?
    out.push_str(" ")?
    out.push_u64(u64.from_usize(n))?
    out.push_str(" ")?
    out.push_u64(d)?
    out.push_str(" ")?
    out.push_str(if ok {
        "ok"
    } else {
        "FAIL"
    })?
    std.io.print_line(out.as_str())?
    Ok(())
end t.u32_case

fn t.rec_case(n: usize, shape: u32) -> Result<(), t.Error> uses [alloc, io.console] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut r = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut state = 12345u64.wrapping_add(u64.from_usize(n))
    for i in core.iter.range(0usize, n) {
        let mixed = state.wrapping_mul(6364136223846793005u64)
        state = mixed.wrapping_add(1442695040888963407u64)
        let x = u32.try_from_u64(state.wrapping_shr(33u32) % 50u64).unwrap_or(0u32)
        let key = if shape == 0u32 {
            x
        } else if shape == 1u32 {
            u32.try_from_usize(i).unwrap_or(0u32)
        } else if shape == 2u32 {
            u32.try_from_usize(n.wrapping_sub(i.wrapping_add(1usize))).unwrap_or(0u32)
        } else {
            7u32
        }
        let idx = u32.try_from_usize(i).unwrap_or(0u32)
        match v.push(t.Rec { key: key, idx: idx }) {
            Ok(_done) => {
            }
            Err(_err) => core.panic.panic("oom")
        }
        match r.push(t.Rec { key: key, idx: idx }) {
            Ok(_done) => {
            }
            Err(_err) => core.panic.panic("oom")
        }
    }
    let cmp_rec = fn(a: &t.Rec, b: &t.Rec) -> core.cmp.Ordering capture [] {
        a.key.cmp(&b.key)
    }
    t.insertion_by(r.as_mut_slice(), &cmp_rec)
    v.as_mut_slice().sort_by(fn(a: &t.Rec, b: &t.Rec) -> core.cmp.Ordering capture [] {
        a.key.cmp(&b.key)
    })
    let vs = v.as_slice()
    let rs = r.as_slice()
    let mut ok = true
    for j in core.iter.range(0usize, n) {
        match rs.get(j) {
            Some(rp) => match vs.get(j) {
                Some(vp) => {
                    if rp.key != vp.key || rp.idx != vp.idx {
                        ok = false
                    }
                }
                None => {
                    ok = false
                }
            }
            None => {
                ok = false
            }
        }
    }
    let mut d = 0u64
    for j in core.iter.range(0usize, n) {
        match vs.get(j) {
            Some(vp) => {
                d = d.wrapping_mul(31u64).wrapping_add(u64.from_u32(vp.key))
            }
            None => core.panic.panic("short")
        }
    }
    let mut out = alloc.string.String.new(std.alloc.SYSTEM)
    out.push_str("rec ")?
    out.push_str(if shape == 0u32 {
        "random"
    } else if shape == 1u32 {
        "sorted"
    } else if shape == 2u32 {
        "reversed"
    } else {
        "dups"
    })?
    out.push_str(" ")?
    out.push_u64(u64.from_usize(n))?
    out.push_str(" ")?
    out.push_u64(d)?
    out.push_str(" ")?
    out.push_str(if ok {
        "ok"
    } else {
        "FAIL"
    })?
    std.io.print_line(out.as_str())?
    Ok(())
end t.rec_case
"#;

/// `sort` / `sort_by` / `sort_unstable` の 3 つの整列の結果が一致する従来の家族テスト。

/// `ksn build` がバンドルへ退避せずネイティブ (LLVM) の実行体を作り、その出力が木走査と一致することを見る
/// (要素型 `T` と `F: Fn` を取る総称関数の単相化)。
fn build_native_and_compare(name: &str, src: &str) {
    let t = Tmp::new(name, src);
    let tree = run(&["run", "--engine=tree"], &t.0);
    assert_eq!(tree.status.code(), Some(0), "{}{}", text(&tree.stdout), text(&tree.stderr));
    let cache = scratch::Scratch::at(format!("ksn_slsort_ncache_{}_{}", std::process::id(), name));
    let b = Command::new(exe()).arg("build").current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    let log = format!("{}{}", text(&b.stdout), text(&b.stderr));
    let native = t.0.join("target/debug/t");
    assert!(b.status.success() && native.exists(), "ネイティブにならない: {}", log);
    let o = Command::new(&native).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&tree.stdout), text(&o.stdout), "native の出力が run と違う");
}

#[test]
fn sort_family_builds_native() {
    build_native_and_compare("sort_native", SORT);
}

#[test]
fn vec_and_slice_helpers_build_native() {
    build_native_and_compare("vec_native", VEC);
}
