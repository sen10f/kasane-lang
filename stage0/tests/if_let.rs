//! `if let` と `is` (1 つの形だけを見る構文) の統合テスト。`match` の 1 腕 + 構文由来の「他の全部」の腕に
//! 下ろされる。実行 (インタプリタ) とネイティブ (LLVM) の出力一致、線形値の漏れの検査、`ksn fmt` の往復を見る。

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";
const PROG: &str = include_str!("data/if_let_prog.ksnl");

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(exe()).args(args).current_dir(cwd).output().expect("spawn ksn")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str, src: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_iflet_{}_{}", std::process::id(), name));
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

fn codes(json: &str) -> Vec<String> {
    json.lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| {
            let i = l.find("\"code\": \"")? + 9;
            Some(l[i..i + 5].to_string())
        })
        .collect()
}

fn check_codes(name: &str, src: &str) -> (Option<i32>, Vec<String>) {
    let t = Tmp::new(name, src);
    let o = run(&["check", "--json"], &t.0);
    (o.status.code(), codes(&text(&o.stdout)))
}

#[test]
fn if_let_and_is_run_and_build_to_the_same_output() {
    let t = Tmp::new("prog_run", PROG);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "iflet ok");

    let t = Tmp::new("prog_build", PROG);
    let cache = scratch::Scratch::at(format!("ksn_iflet_cache_{}", std::process::id()));
    let b = Command::new(exe()).args(["build"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let out = text(&b.stdout) + &text(&b.stderr);
    let _ = fs::remove_dir_all(&cache);
    let native = t.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t" });
    assert!(native.exists(), "if let / is must lower to the native (LLVM) build: {}", out);
    let n = Command::new(&native).output().unwrap();
    assert_eq!(n.status.code(), Some(0), "{} {}", text(&n.stdout), text(&n.stderr));
    assert_eq!(text(&n.stdout), text(&o.stdout));
}

#[test]
fn unsound_if_let_cases_are_rejected_for_leaking_linear_values() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/unsound");
    for stem in ["if_let_linear_else_leak", "if_let_linear_no_else", "is_linear_payload_discarded"] {
        let src = fs::read_to_string(dir.join(format!("{}.ksnl", stem))).unwrap();
        let (rc, got) = check_codes(stem, &src);
        assert_eq!(rc, Some(1), "{}", stem);
        assert!(got.iter().any(|c| c == "E3003"), "{}: {:?}", stem, got);
        let t = Tmp::new(&format!("{}_fmt", stem), &src);
        assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{} is not canonical", stem);
    }
}

#[test]
fn match_still_forbids_wildcards_on_enums_but_if_let_does_not() {
    let wild = PROG.replace(
        "    if let t.Kind.Ident(n) = k {\n        n\n    } else if let t.Kind.Pair(a, b) = k {\n        a.wrapping_add(b)\n    } else if let t.Kind.Eof = k {\n        1000u32\n    } else {\n        2000u32\n    }",
        "    match k {\n        t.Kind.Ident(n) => n\n        _ => 2000u32\n    }",
    );
    assert_ne!(wild, PROG);
    let (rc, got) = check_codes("wild_match", &wild);
    assert_eq!(rc, Some(1));
    assert!(got.iter().any(|c| c == "E2021"), "{:?}", got);
    let (rc, got) = check_codes("if_let_ok", PROG);
    assert_eq!(rc, Some(0), "{:?}", got);
}

#[test]
fn a_pattern_after_is_cannot_bind_a_name() {
    let src = PROG.replace("k is t.Kind.Num(_)", "k is t.Kind.Num(v)");
    let (rc, got) = check_codes("is_binds", &src);
    assert_eq!(rc, Some(1));
    assert!(got.iter().any(|c| c == "E0002"), "{:?}", got);
}

#[test]
fn if_let_bindings_are_scoped_to_the_then_branch() {
    let src = PROG.replace("        n = x\n    }\n    core.check.eq(n, 5u32)\n    if let Some(x) = None", "        n = x\n    }\n    n = x\n    core.check.eq(n, 5u32)\n    if let Some(x) = None");
    assert_ne!(src, PROG);
    let (rc, _) = check_codes("scope", &src);
    assert_eq!(rc, Some(1));
}

#[test]
fn fmt_round_trips_if_let_and_is() {
    let t = Tmp::new("fmt_ok", PROG);
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{}", text(&run(&["fmt", "--check"], &t.0).stdout));
    // 行末の空白を足した入力は fmt で元に戻り、構文は保たれる。
    let messy = PROG.replace("    if let Some(x) = Some(5u32) {", "    if let Some(x) = Some(5u32) {   ");
    let t = Tmp::new("fmt_messy", &messy);
    let _ = run(&["fmt"], &t.0);
    let after = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert_eq!(after, PROG);
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
}
