//! `alloc.collections` の `HashMap` / `BtreeMap` を Kasane で書いたもの (修正プラン 3-4、`spec/stdlib/alloc/collections.ksnl`) の検査。
//! 規範の定義 (`collections.sig` の型) と本体 (`collections.ksnl`) を、名前だけ `scr.` に写した利用者のパッケージに入れ、
//! 木走査で、素朴な配列のモデル (線形探索) と毎回比べる。`HashMap` は挿入順の反復も確かめる (仕様 8.3)。
//! 組み込みの `Val::Map` は使わない (ここでは Kasane の本体そのものを動かす)。

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOML: &str = "[package]\nname = \"scr\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"scr.main\"\n\n[dependencies]\n";

/// 規範の型定義のうち `head` で始まる行から、対応する `end` の行までを取り出す。
fn block(sig: &str, head: &str, end: &str) -> String {
    let start = sig.find(head).unwrap_or_else(|| panic!("sig に {} がない", head));
    let rest = &sig[start..];
    let stop = rest.find(end).unwrap_or_else(|| panic!("sig に {} がない", end)) + end.len();
    rest[..stop].to_string()
}

/// `from` で始まる箇所から `to` の終わりまでを取り除く。
fn drop_from(src: &str, from: &str, to: &str) -> String {
    let a = src.find(from).unwrap_or_else(|| panic!("{} がない", from));
    let b = a + src[a..].find(to).unwrap_or_else(|| panic!("{} がない", to)) + to.len();
    format!("{}{}", &src[..a], &src[b..])
}

fn scratch_source() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("spec").join("stdlib").join("alloc");
    let sig = fs::read_to_string(root.join("collections.sig")).unwrap();
    let ksnl = fs::read_to_string(root.join("collections.ksnl")).unwrap();
    let types = [
        block(&sig, "type alloc.collections.BtreeMap", "end alloc.collections.BtreeMap"),
        block(&sig, "type alloc.collections.BtreeNode", "end alloc.collections.BtreeNode"),
        block(&sig, "type alloc.collections.MapEntry", "end alloc.collections.MapEntry"),
        block(&sig, "type alloc.collections.HashMap[", "end alloc.collections.HashMap"),
        block(&sig, "enum alloc.collections.HashCell", "end alloc.collections.HashCell"),
    ]
    .join("\n\n");
    // 規範の FQN を `scr.` に写す (利用者のパッケージの中で同じ型・同じ関数になる)。
    // 参照は二級の値 (E3060): 利用者のパッケージは参照を運ぶ型 (`HashMapIter['a, K, V]`) を宣言できないので、
    // 反復子 (`HashMap.iter` と `HashMapIter` の `Iterator` 実装) は写さない。反復の順序は標準ライブラリ本体の試験が見る。
    // `BtreeMap` / `HashSet` の反復子も参照を運ぶ型 (`BtreeMapIter['a, K, V]` / `HashSetIter['a, T]`) なので写さない。
    let ksnl = drop_from(&ksnl, "fn alloc.collections.BtreeMap.iter[", "end alloc.collections.BtreeMap.iter");
    let ksnl = drop_from(&ksnl, "fn alloc.collections.BtreeMap.range[", "end alloc.collections.BtreeMap.range");
    let ksnl = drop_from(&ksnl, "impl['a, K, V] core.iter.Iterator<(&'a K, &'a V), [], never> for alloc.collections.BtreeMapIter", "end alloc.collections.BtreeMapIter.next");
    // `HashSet` (`HashMap` の上の薄い包み) はこの試験の対象外なので、本体ごと写さない。
    let ksnl = drop_from(&ksnl, "fn alloc.collections.HashSet.contains[", "end alloc.collections.HashSetIter.next");
    let ksnl = drop_from(&ksnl, "fn alloc.collections.HashMap.iter[", "end alloc.collections.HashMap.iter");
    let ksnl = drop_from(&ksnl, "impl['a, K, V] core.iter.Iterator<(&'a K, &'a V)", "end alloc.collections.HashMapIter.next");
    let main = drop_from(MAIN, "    // 反復は挿入順", "    core.check.eq(j, keys.len())\n");
    format!("{}\n\n{}\n\n{}", types, ksnl, main).replace("alloc.collections.", "scr.")
}

const MAIN: &str = r#"
fn scr.unwrap_alloc[T](r: Result<T, alloc.alloc.AllocError>) -> T uses [] panics may
    match r {
        Ok(x) => {
            x
        }
        Err(_e) => {
            core.panic.panic("alloc")
        }
    }
end scr.unwrap_alloc

fn scr.model_find(keys: &alloc.vec.Vec<u32, std.alloc.System>, key: u32) -> Option<usize> uses [] panics never
    let mut i = 0usize
    while i < keys.len() {
        match keys.as_slice().get(i) {
            Some(k) => {
                if *k == key {
                    return Some(i)
                }
            }
            None => {
            }
        }
        i = i.wrapping_add(1usize)
    }
    None
end scr.model_find

fn scr.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let cells0 = alloc.raw.Buf.new(std.alloc.SYSTEM)
    let slots0 = 0usize
    let mut m = scr.HashMap { cells: cells0, slots: slots0, dead: 0usize }
    let mut b = scr.BtreeMap.new_empty(std.alloc.SYSTEM)
    let mut keys: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut vals: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut seed = 7u32
    let mut step = 0u32
    while step < 1200u32 {
        seed = seed.wrapping_mul(1103515245u32).wrapping_add(12345u32)
        let key = seed.wrapping_shr(16u32) & 255u32
        let op = seed.wrapping_shr(8u32) & 3u32
        let value = seed.wrapping_shr(4u32)
        if op == 0u32 {
            let got = m.remove(&key)
            let gotb = b.remove(&key)
            match scr.model_find(&keys, key) {
                Some(i) => {
                    core.check.is_true(got is Some(_))
                    core.check.is_true(gotb is Some(_))
                    core.mem.drop(keys.remove(i))
                    core.mem.drop(vals.remove(i))
                }
                None => {
                    core.check.is_true(got is None)
                    core.check.is_true(gotb is None)
                }
            }
        } else {
            let old = scr.unwrap_alloc(m.insert(key, value))
            let oldb = scr.unwrap_alloc(b.insert(key, value))
            match scr.model_find(&keys, key) {
                Some(i) => {
                    core.check.is_true(old is Some(_))
                    core.check.is_true(oldb is Some(_))
                    match vals.as_mut_slice().get_mut(i) {
                        Some(slot) => {
                            *slot = value
                        }
                        None => {
                        }
                    }
                }
                None => {
                    core.check.is_true(old is None)
                    core.check.is_true(oldb is None)
                    core.mem.drop(scr.unwrap_alloc(keys.push(key)))
                    core.mem.drop(scr.unwrap_alloc(vals.push(value)))
                }
            }
        }
        scr.check_get(&m, &keys, &vals, key)
        scr.check_get_b(&b, &keys, &vals, key)
        core.check.eq(m.len(), keys.len())
        core.check.eq(b.len(), keys.len())
        step = step.wrapping_add(1u32)
    }
    // 反復は挿入順 (削除後も残りの相対順を保つ)。
    let mut it = m.iter()
    let mut j = 0usize
    loop {
        match it.next() {
            Some((k, v)) => {
                match keys.as_slice().get(j) {
                    Some(x) => {
                        core.check.eq(*k, *x)
                    }
                    None => {
                        core.panic.panic("too many entries")
                    }
                }
                match vals.as_slice().get(j) {
                    Some(y) => {
                        core.check.eq(*v, *y)
                    }
                    None => {
                        core.panic.panic("too many entries")
                    }
                }
                j = j.wrapping_add(1usize)
            }
            None => {
                break
            }
        }
    }
    core.check.eq(j, keys.len())
    core.mem.drop(std.io.print_line("ok"))
    Ok(())
end scr.main

fn scr.check_get(m: &scr.HashMap<u32, u32, std.alloc.System>, keys: &alloc.vec.Vec<u32, std.alloc.System>, vals: &alloc.vec.Vec<u32, std.alloc.System>, key: u32) -> () uses [] panics may
    match m.get(&key) {
        Some(v) => {
            match scr.model_find(keys, key) {
                Some(i) => {
                    match vals.as_slice().get(i) {
                        Some(w) => {
                            core.check.eq(*v, *w)
                        }
                        None => {
                        }
                    }
                }
                None => {
                    core.panic.panic("found but absent")
                }
            }
        }
        None => {
            match scr.model_find(keys, key) {
                Some(_i) => {
                    core.panic.panic("absent but in model")
                }
                None => {
                }
            }
        }
    }
end scr.check_get

fn scr.check_get_b(m: &scr.BtreeMap<u32, u32, std.alloc.System>, keys: &alloc.vec.Vec<u32, std.alloc.System>, vals: &alloc.vec.Vec<u32, std.alloc.System>, key: u32) -> () uses [] panics may
    match m.get(&key) {
        Some(v) => {
            match scr.model_find(keys, key) {
                Some(i) => {
                    match vals.as_slice().get(i) {
                        Some(w) => {
                            core.check.eq(*v, *w)
                        }
                        None => {
                        }
                    }
                }
                None => {
                    core.panic.panic("found but absent")
                }
            }
        }
        None => {
            match scr.model_find(keys, key) {
                Some(_i) => {
                    core.panic.panic("absent but in model")
                }
                None => {
                }
            }
        }
    }
end scr.check_get_b
"#;

struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str, src: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_mapk_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), TOML).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(d.join("src").join("scr.ksnl"), src).unwrap();
        // 書き方は 1 通り (E0020): 標準の本体を写して組み立てた入力は `ksn fmt` の出力にそろえる。
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
    Command::new(env!("CARGO_BIN_EXE_ksn")).args(args).current_dir(cwd).output().expect("spawn ksn")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

#[test]
fn kasane_hashmap_and_btreemap_match_a_model_in_tree_walker() {
    let t = Tmp::new("model", &scratch_source());
    let chk = run(&["check"], &t.0);
    assert!(chk.status.success() && text(&chk.stdout).trim().is_empty(), "{}{}", text(&chk.stdout), text(&chk.stderr));
    let r = run(&["run", "--engine=tree"], &t.0);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "ok\n");
}
