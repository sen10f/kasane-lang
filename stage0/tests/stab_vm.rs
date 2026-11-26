//! VM・木走査の安定性の回帰 (opus-stab の S-01 / S-07 / S-08 / S-09)。入力は `tests/data/stab_vm/`。

#[allow(dead_code, unused_imports)]
mod common;

use std::path::PathBuf;

fn pkg(name: &str, unsafe_mod: bool) -> PathBuf {
    let d = std::env::temp_dir().join(format!("stab_vm_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("src")).unwrap();
    let mut toml = String::from("[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n");
    if unsafe_mod {
        toml.push_str("[unsafe]\nmodules = [\"t\"]\n\n");
    }
    toml.push_str("[dependencies]\n");
    std::fs::write(d.join("kasane.toml"), toml).unwrap();
    std::fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
    let src = common::root().join("stage0/tests/data/stab_vm").join(format!("{}.ksnl", name));
    std::fs::copy(src, d.join("src/t.ksnl")).unwrap();
    d
}

fn run_engine(d: &PathBuf, engine: &str, extra: &[&str]) -> (Option<i32>, String, String) {
    let mut args = vec!["run".to_string(), format!("--engine={}", engine)];
    if !extra.is_empty() {
        args.push("--".into());
        args.extend(extra.iter().map(|s| s.to_string()));
    }
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let o = common::run_with_env(&a, d, &[("KSN_CACHE_DIR", d.join("cache").to_str().unwrap())]);
    (o.status.code(), common::text(&o.stdout), common::text(&o.stderr))
}

#[test]
fn s08_many_callbacks_in_vm() {
    let d = pkg("S-08", true);
    let (c, out, err) = run_engine(&d, "vm", &[]);
    assert_eq!(c, Some(0), "{}{}", out, err);
    assert!(out.contains("sort 19 first=9"), "{}", out);
    assert!(!err.contains("internal error") && !err.contains("内部エラー"), "{}", err);
}

#[test]
fn s09_char_debug_same_in_all_engines() {
    let d = pkg("S-09", false);
    let (c1, o1, e1) = run_engine(&d, "tree", &[]);
    let (c2, o2, e2) = run_engine(&d, "vm", &[]);
    assert_eq!((c1, c2), (Some(0), Some(0)), "{}{}{}{}", o1, e1, o2, e2);
    assert_eq!(o1, o2);
    assert!(o1.contains("U+0080 '\\u{80}'") && o1.contains("U+200B '\\u{200b}'"), "{}", o1);
}

#[test]
fn s01_huge_array_is_a_panic_not_an_abort() {
    let d = pkg("S-01", false);
    for e in ["tree", "vm"] {
        let (c, _o, err) = run_engine(&d, e, &[]);
        assert_eq!(c, Some(101), "{}: {}", e, err);
        assert!(err.contains("memory allocation failed"), "{}: {}", e, err);
    }
}

#[test]
fn s07_vm_big_allocation_matches_tree() {
    let d = pkg("S-07", false);
    for e in ["tree", "vm"] {
        let (c, o, err) = run_engine(&d, e, &["9000000000"]);
        assert_eq!(c, Some(0), "{}: {}{}", e, o, err);
        assert!(o.contains("ok cap>=9000000000"), "{}: {}", e, o);
    }
}

#[test]
fn s14_reads_big_file_under_ulimit_v() {
    if !std::path::Path::new("/bin/sh").exists() {
        return;
    }
    let d = pkg("S-14", false);
    let big = d.join("big.bin");
    std::fs::write(&big, vec![0u8; 300_000_000]).unwrap();
    for (lim, must_succeed) in [(16_000_000, true), (5_000_000, false)] {
        let cmd = format!(
            "ulimit -v {} 2>/dev/null || exit 77; KSN_CACHE_DIR={} {} run -- big.bin",
            lim,
            d.join("cache").display(),
            common::exe()
        );
        let o = std::process::Command::new("/bin/sh").arg("-c").arg(cmd).current_dir(&d).output().unwrap();
        if o.status.code() == Some(77) {
            break;
        }
        let (out, err) = (common::text(&o.stdout), common::text(&o.stderr));
        // SIGABRT (134) や信号死は不可。
        assert!(o.status.code().is_some_and(|c| c < 128), "limit {}: {:?} {}{}", lim, o.status, out, err);
        if must_succeed {
            assert!(o.status.code() == Some(0) && out.contains("len=300000000"), "{:?} {}{}", o.status, out, err);
        }
    }
    let _ = std::fs::remove_file(&big);
}
