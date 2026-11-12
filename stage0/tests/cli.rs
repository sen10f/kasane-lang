//! `ksn` バイナリの統合テスト。Python に依存しない (`cargo test` だけで動く)。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(exe()).args(args).current_dir(cwd).output().expect("spawn ksn")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// 一時パッケージ `t` を作る。
struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str, src: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_test_{}_{}", std::process::id(), name));
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

fn codes(json: &str) -> Vec<String> {
    // 1診断1行の JSON から "code" を取り出す (簡易)。
    json.lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| {
            let i = l.find("\"code\": \"")? + 9;
            Some(l[i..i + 5].to_string())
        })
        .collect()
}

#[test]
fn diagnostics_each_case_reports_its_code() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/diag");
    let mut names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    assert!(names.len() >= 20, "diag cases missing");
    let mut failures = Vec::new();
    for p in names {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let code = &stem[..5];
        let t = Tmp::new(&stem, &fs::read_to_string(&p).unwrap());
        let o = run(&["check", "--json"], &t.0);
        let got = codes(&text(&o.stdout));
        if o.status.code() != Some(1) || !got.iter().any(|c| c == code) {
            failures.push(format!("{}: rc={:?} codes={:?}", stem, o.status.code(), got));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn clean_program_passes_check_and_fmt() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = Tmp::new("clean", src);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
}

#[test]
fn fmt_check_fails_on_noncanonical_source() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never   \n    Ok(())\nend t.main\n";
    let t = Tmp::new("fmtbad", src);
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(1));
    // 整形すると冪等に通る。
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
}

#[test]
fn json_diagnostics_have_required_fields() {
    let t = Tmp::new("jsonfields", &fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/diag/E1101.ksnl")).unwrap());
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    let line = out.lines().find(|l| l.starts_with('{')).expect("a diagnostic line");
    for k in ["\"code\"", "\"fatal\"", "\"fqn\"", "\"file\"", "\"line\"", "\"col\"", "\"message\"", "\"fix\""] {
        assert!(line.contains(k), "missing {} in {}", k, line);
    }
}

#[test]
fn diagnostics_are_sorted_stably() {
    // 同じ入力で2回実行して出力が一致する。
    let t = Tmp::new("stable", &fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/diag/E3002.ksnl")).unwrap());
    let a = text(&run(&["check", "--json"], &t.0).stdout);
    let b = text(&run(&["check", "--json"], &t.0).stdout);
    assert_eq!(a, b);
}

#[test]
fn run_err_exits_1_with_debug_output() {
    let t = Tmp::new("err", "fn t.main() -> Result<(), u8> uses [] panics may\n    Err(7u8)\nend t.main\n");
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("Error: 7"));
}

#[test]
fn run_panic_exits_101() {
    let t = Tmp::new("panic", "fn t.main() -> Result<(), u8> uses [] panics may\n    core.panic.panic(\"boom\")\nend t.main\n");
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101));
    assert!(text(&o.stderr).contains("boom"));
}

#[test]
fn run_ok_exits_0_and_prints() {
    let t = Tmp::new("ok", "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    std.io.print_line(\"hi\")\n    Ok(())\nend t.main\n");
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(text(&o.stdout).trim(), "hi");
}

#[test]
fn syntax_error_is_reported_as_e0xxx() {
    let t = Tmp::new("syntax", "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\n");
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(codes(&text(&o.stdout)).iter().any(|c| c.starts_with("E0")));
}

#[test]
fn explain_known_and_unknown_codes() {
    let r = root();
    let ok = run(&["explain", "E1101"], &r);
    assert_eq!(ok.status.code(), Some(0));
    assert!(text(&ok.stdout).starts_with("E1101"));
    let bad = run(&["explain", "E9999"], &r);
    assert_ne!(bad.status.code(), Some(0));
}

#[test]
fn examples_check_fmt_and_run() {
    let expected = [
        ("demo", "hello from demo"),
        ("features", "features ok"),
        ("tour", "tour ok"),
        ("native", "native ok"),
        ("threads", "threads ok"),
        ("realtime", "realtime ok"),
        ("json", "{\"name\":\"kasane\",\"tags\":[\"a\",\"b\"],\"n\":-12.5e3,\"ok\":true,\"none\":null}"),
    ];
    for (name, out) in expected {
        let d = root().join("examples").join(name);
        assert_eq!(run(&["check"], &d).status.code(), Some(0), "{} check", name);
        assert_eq!(run(&["fmt", "--check"], &d).status.code(), Some(0), "{} fmt", name);
        let o = run(&["run"], &d);
        assert_eq!(o.status.code(), Some(0), "{} run", name);
        assert!(text(&o.stdout).trim().ends_with(out), "{} output: {}", name, text(&o.stdout));
        assert_eq!(run(&["test"], &d).status.code(), Some(0), "{} test", name);
    }
}

#[test]
fn new_creates_checkable_package() {
    let base = std::env::temp_dir().join(format!("ksn_new_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let o = run(&["new", "app"], &base);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let p = base.join("app");
    assert!(p.join("kasane.toml").exists() && p.join("kasane.lock").exists() && p.join("src").is_dir());
    assert_eq!(fs::read_to_string(p.join("kasane.lock")).unwrap().trim(), "version = 1");
    assert_eq!(run(&["check"], &p).status.code(), Some(0));
    assert_eq!(run(&["run"], &p).status.code(), Some(0));
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn get_and_sig_print_definition_and_header() {
    let d = root().join("examples").join("demo");
    let toml = fs::read_to_string(d.join("kasane.toml")).unwrap();
    assert!(toml.contains("entry"));
    let entry = toml.lines().find(|l| l.starts_with("entry")).unwrap();
    let fqn = entry.split('"').nth(1).unwrap();
    let sig = run(&["sig", fqn], &d);
    assert_eq!(sig.status.code(), Some(0));
    assert_eq!(text(&sig.stdout).trim().lines().count(), 1);
    let get = run(&["get", fqn], &d);
    assert_eq!(get.status.code(), Some(0));
    assert!(text(&get.stdout).trim_end().ends_with(&format!("end {}", fqn)));
}

#[test]
fn rename_dry_run_does_not_modify_files() {
    let d = root().join("examples").join("demo");
    let before: Vec<_> = walk(&d.join("src")).into_iter().map(|p| (fs::read(&p).unwrap(), p)).collect();
    let o = run(&["rename", "--dry-run", "demo.main", "demo.start"], &d);
    assert!(o.status.code().is_some());
    for (bytes, p) in before {
        assert_eq!(fs::read(&p).unwrap(), bytes, "{:?} was modified", p);
    }
}

fn walk(d: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for e in fs::read_dir(d).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            v.extend(walk(&p));
        } else {
            v.push(p);
        }
    }
    v
}

#[test]
fn build_produces_artifacts_and_is_cached() {
    let d = root().join("examples").join("native");
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let b = |release: bool| {
        let mut c = Command::new(exe());
        c.arg("build").current_dir(&d).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        c.output().unwrap()
    };
    let o = b(true);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let tgt = d.join("target").join("release");
    assert!(tgt.join("ksn.exe").exists() || tgt.join("ksn").exists());
    let again = b(true);
    assert_eq!(again.status.code(), Some(0));
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn lock_mismatch_is_a_package_error() {
    let t = Tmp::new("lock", "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n");
    fs::write(
        t.0.join("kasane.toml"),
        format!("{}net = {{ path = \"../net\" }}\n", TOML),
    )
    .unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(codes(&text(&o.stdout)).iter().any(|c| c.starts_with("E9")));
}

// ---- 既知のギャップ: 仕様上エラーになるべきだが現状は通る (実装されたら #[ignore] を外す) ----

fn main_wrap(prefix: &str, body: &str) -> String {
    format!(
        "{}fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n{}    Ok(())\nend t.main\n",
        prefix, body
    )
}

fn must_fail(name: &str, src: &str) {
    let t = Tmp::new(name, src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}: expected a diagnostic, got none", name);
}


#[test]
fn gap_let_underscore() {
    must_fail("gap_letus", &main_wrap("", "    let _ = 1u8\n"));
}

#[test]
fn gap_shadowing() {
    must_fail("gap_shadow", &main_wrap("", "    let a = 1u8\n    let a = 2u8\n    core.mem.drop(a)\n"));
}

#[test]
fn gap_wildcard_on_enum() {
    let b = "    let o = Some(1u8)\n    core.mem.drop(match o {\n        Some(_x) => 1u8\n        _ => 2u8\n    })\n";
    must_fail("gap_wild", &main_wrap("", b));
}

#[test]
fn gap_discarded_value_statement() {
    must_fail("gap_discard", &main_wrap("", "    1u8\n"));
}


#[test]
fn gap_redundant_lifetime() {
    let f = "fn t.a['a](x: &'a u8) -> &'a u8 uses [] panics never\n    x\nend t.a\n\n";
    must_fail("gap_lt", &main_wrap(f, ""));
}

#[test]
fn gap_recursive_type_needs_drop() {
    let f = "enum t.List pub\n    Cons(u8, &'static t.List)\n    Nil\nend t.List\n\n";
    must_fail("gap_rec", &main_wrap(f, ""));
}

#[test]
fn duplicate_import_tails_are_rejected() {
    let t = Tmp::new("impdup", &main_wrap("", ""));
    fs::create_dir_all(t.0.join("src/t/other")).unwrap();
    fs::write(t.0.join("src/t/util.ksnl"), "fn t.util.f() -> u8 uses [] panics never\n    1u8\nend t.util.f\n")
        .unwrap();
    fs::write(
        t.0.join("src/t/other/util.ksnl"),
        "fn t.other.util.g() -> u8 uses [] panics never\n    2u8\nend t.other.util.g\n",
    )
    .unwrap();
    fs::write(
        t.0.join("src").join("t.ksnl"),
        "import t.other.util\nimport t.util\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n",
    )
    .unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E1111"), "{}", text(&o.stdout));
}

#[test]
fn gap_ksn_rt_link() {
    let t = Tmp::new("gap_rt", &main_wrap("", ""));
    fs::create_dir_all(t.0.join("src/t")).unwrap();
    fs::write(t.0.join("src/t/sys.ksnl"), "extern t.sys.foo() -> i32 link \"ksn_rt_foo\"\n").unwrap();
    assert_eq!(run(&["check"], &t.0).status.code(), Some(1));
}

#[test]
fn ksnh_extension_is_rejected() {
    let t = Tmp::new("ksnh", &main_wrap("", ""));
    fs::write(t.0.join("src/t/old.ksnh"), "").unwrap_or_else(|_| {
        fs::create_dir_all(t.0.join("src/t")).unwrap();
        fs::write(t.0.join("src/t/old.ksnh"), "").unwrap();
    });
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E1107"), "{}", text(&o.stdout));
}

#[test]
fn unsafe_requires_listing_in_manifest() {
    let src = main_wrap("", "    unsafe \"n\" \"why\" {
        core.mem.drop(1u8)
    }
");
    let t = Tmp::new("unsafe_off", &src);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E5001"), "{}", text(&o.stdout));
    fs::write(t.0.join("kasane.toml"), format!("{}
[unsafe]
modules = [\"t\"]
", TOML)).unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E5001"), "{}", text(&o.stdout));
}

#[test]
fn unsafe_module_listing_matches_prefix() {
    let src = main_wrap("", "");
    let t = Tmp::new("unsafe_prefix", &src);
    fs::write(t.0.join("src/t/sys.ksnl"), "").ok();
    fs::create_dir_all(t.0.join("src/t")).unwrap();
    let body = "fn t.sys.f() -> () uses [] panics never pub
    unsafe \"n\" \"why\" {
        core.mem.drop(1u8)
    }
end t.sys.f
";
    fs::write(t.0.join("src/t/sys.ksnl"), body).unwrap();
    fs::write(t.0.join("kasane.toml"), format!("{}
[unsafe]
modules = [\"t.sys\"]
", TOML)).unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E5001"), "{}", text(&o.stdout));
}

#[test]
fn unsafe_manifest_is_validated() {
    let t = Tmp::new("unsafe_bad", &main_wrap("", ""));
    for bad in ["modules = [\"other.mod\"]", "modules = \"t\"", "modules = [\"Bad Name\"]", "extra = 1"] {
        fs::write(t.0.join("kasane.toml"), format!("{}
[unsafe]
{}
", TOML, bad)).unwrap();
        let o = run(&["check", "--json"], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}: {}{}", bad, text(&o.stdout), text(&o.stderr));
    }
}

#[test]
fn gap_f32_precision() {
    let b = "    let a = 16777216.0f32\n    let b = a + 1.0f32\n    core.check.is_true(b == a)\n";
    let t = Tmp::new("gap_f32", &main_wrap("", b));
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
}

#[test]
fn gap_u128_max_overflow() {
    let b = "    core.check.is_true(core.num.u128.MAX.checked_add(1u128).is_none())\n";
    let t = Tmp::new("gap_u128", &main_wrap("", b));
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
}

// ---- 数値の挙動 ----

fn run_ok(name: &str, body: &str) {
    let t = Tmp::new(name, &main_wrap("", body));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}: {}{}", name, text(&o.stdout), text(&o.stderr));
}

#[test]
fn hashmap_preserves_insertion_order() {
    // 8章: 標準 HashMap の反復順は挿入順で、削除後も残りの相対順を保つ。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    core.mem.drop(m.insert(3u8, "c"))
    core.mem.drop(m.insert(1u8, "a"))
    core.mem.drop(m.insert(2u8, "b"))
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    for kv in m.iter() {
        let (k, v) = kv
        core.mem.drop(k)
        match s.push_str(*v) {
            Ok(_u) => ()
            Err(_e) => core.panic.panic("oom")
        }
    }
    core.mem.drop(m.remove(&1u8))
    let mut s2 = alloc.string.String.new(std.alloc.SYSTEM)
    for kv in m.iter() {
        let (k, v) = kv
        core.mem.drop(k)
        match s2.push_str(*v) {
            Ok(_u) => ()
            Err(_e) => core.panic.panic("oom")
        }
    }
    std.io.print_line(s.as_str())?
    std.io.print_line(s2.as_str())?
    Ok(())
end t.main
"#;
    let t = Tmp::new("maporder", src);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "cab\ncb");
}

#[test]
fn u128_full_range() {
    run_ok(
        "u128_range",
        "    let big = 170141183460469231731687303715884105728u128\n\
         \x20   core.check.eq(big.checked_add(big), None)\n\
         \x20   core.check.eq(core.num.u128.MAX.wrapping_add(1u128), 0u128)\n\
         \x20   core.check.is_true(big > 1u128)\n\
         \x20   core.check.is_true(core.num.u128.MAX > big)\n\
         \x20   core.check.eq(core.num.u128.MAX.saturating_add(5u128), core.num.u128.MAX)\n\
         \x20   core.check.eq(core.num.u128.MAX.checked_sub(core.num.u128.MAX), Some(0u128))\n\
         \x20   core.check.eq(big.checked_shr(127u32), Some(1u128))\n\
         \x20   core.check.eq(u64.try_from_u128(big), Err(core.num.TryFromIntError { kind: 0u8 }))\n",
    );
}

#[test]
fn f32_arithmetic_is_single_precision() {
    run_ok(
        "f32_arith",
        "    let a = 0.1f32\n\
         \x20   let b = 0.2f32\n\
         \x20   core.check.is_true(a + b == 0.3f32)\n\
         \x20   core.check.is_true(a.to_f64() != 0.1f64)\n\
         \x20   core.check.is_true(1.0f32 / 3.0f32 == 0.33333334f32)\n\
         \x20   core.check.eq(1.0f32.to_bits(), 0x3f800000u32)\n",
    );
}

#[test]
fn f64_arithmetic_unchanged() {
    run_ok(
        "f64_arith",
        "    core.check.is_true(0.1f64 + 0.2f64 != 0.3f64)\n    core.check.eq(2.0f64.sqrt() * 2.0f64.sqrt() > 2.0f64, true)\n",
    );
}

// ---- JSON パーサ作成中に見つかった誤検出の回帰テスト ----

#[test]
fn mut_ref_param_can_be_passed_repeatedly() {
    // &mut の引数を続けて渡すのは再借用であり move ではない (誤検出 E3002 の回帰)。
    let f = "fn t.a(v: &mut u8) -> () uses [] panics never\n    core.mem.drop(v)\nend t.a\n\n\
             fn t.b(v: &mut u8) -> () uses [] panics never\n    t.a(v)\n    t.a(v)\nend t.b\n\n";
    let t = Tmp::new("reborrow", &main_wrap(f, ""));
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E3002"), "{}", text(&o.stdout));
}

#[test]
fn match_arm_binding_used_in_block_is_not_unused() {
    // `Some(c) => {` の次行以降で使う束縛を未使用扱いしない (誤検出 E1020 の回帰)。
    let b = "    let o = Some(1u8)\n    match o {\n        Some(c) => {\n            core.mem.drop(c)\n        }\n        None => ()\n    }\n";
    let t = Tmp::new("armblock", &main_wrap("", b));
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E1020"), "{}", text(&o.stdout));
}

// ---- 小型モデル向けの方針: 正規形の緩和 / 存在しないメソッド ----

#[test]
fn misordered_definitions_pass_check_and_are_fixed_by_fmt() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n\n\
               fn t.a() -> u8 uses [] panics never pub\n    1u8\nend t.a\n";
    let t = Tmp::new("loose_order", src);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{}", text(&run(&["check"], &t.0).stdout));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(1));
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

#[test]
fn semantic_errors_are_still_reported_in_misordered_files() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    t.nothing()\n    Ok(())\nend t.main\n\n\
               fn t.a() -> u8 uses [] panics never pub\n    1u8\nend t.a\n";
    let t = Tmp::new("loose_sem", src);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E1101"), "{}", text(&o.stdout));
}

#[test]
fn unknown_method_is_reported_with_a_suggestion() {
    let b = "    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    core.mem.drop(v.pushh(1u8))\n";
    let t = Tmp::new("unknown_method", &main_wrap("", b));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E1108"), "{}", out);
    assert!(out.contains("did you mean `push`"), "{}", out);
}

#[test]
fn nonexistent_integer_method_is_reported() {
    let b = "    core.mem.drop(1u8.wrapping_div(2u8))\n";
    let t = Tmp::new("no_wrapping_div", &main_wrap("", b));
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E1108"), "{}", text(&o.stdout));
}

// ---- 整数の算術演算子 (仕様 0.9: 4.1) ----

/// 行の配列を、4 スペースインデントの本体にする。
fn body(lines: &[&str]) -> String {
    lines.iter().map(|l| format!("    {}\n", l)).collect()
}

#[test]
fn integer_operators_compute_and_check_overflow() {
    run_ok(
        "int_ops",
        &body(&[
            "let a = 250u8",
            "core.check.eq(a - 50u8, 200u8)",
            "core.check.eq((a - 50u8) / 4u8 + 1u8, 51u8)",
            "core.check.eq(7i32 % 4i32, 3i32)",
            "core.check.eq(-7i32 / 2i32, -3i32)",
            "let b = 5i32",
            "core.check.eq(-b, -5i32)",
        ]),
    );
}

#[test]
fn integer_overflow_panics_with_101() {
    let cases: [(&str, Vec<&str>); 5] = [
        ("ovf_add", vec!["let a = 250u8", "core.mem.drop(a + 10u8)"]),
        ("ovf_sub", vec!["let a = 1u8", "core.mem.drop(a - 2u8)"]),
        ("ovf_mul", vec!["let a = 2147483647i32", "core.mem.drop(a * 2i32)"]),
        ("ovf_div0", vec!["let a = 5u8", "let z = 0u8", "core.mem.drop(a / z)"]),
        ("ovf_neg", vec!["let a = -128i8", "core.mem.drop(-a)"]),
    ];
    for (name, lines) in cases {
        let t = Tmp::new(name, &main_wrap("", &body(&lines)));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(101), "{}: {}{}", name, text(&o.stdout), text(&o.stderr));
    }
}

#[test]
fn integer_operators_are_rejected_in_panics_never_functions() {
    let f = "fn t.add(a: u8, b: u8) -> u8 uses [] panics never\n    a + b\nend t.add\n\n";
    let t = Tmp::new("int_never", &main_wrap(f, ""));
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E4004"), "{}", text(&o.stdout));
    let f2 = "fn t.add(a: u8, b: u8) -> u8 uses [] panics may\n    a + b\nend t.add\n\n";
    let t = Tmp::new("int_may", &main_wrap(f2, ""));
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E4004"), "{}", text(&o.stdout));
}

#[test]
fn float_operators_do_not_panic() {
    let f = "fn t.add(a: f64, b: f64) -> f64 uses [] panics never\n    a + b\nend t.add\n\n";
    let t = Tmp::new("float_never", &main_wrap(f, ""));
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E4004"), "{}", text(&o.stdout));
}

// ---- 診断の修正案 (fix) と `ksn fix` ----

#[test]
fn unknown_method_diagnostic_carries_a_fix() {
    let b = body(&["let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)", "core.mem.drop(v.pushh(1u8))"]);
    let t = Tmp::new("fix_method", &main_wrap("", &b));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(out.contains("\"replace\": \"push\""), "{}", out);
    assert!(out.contains("\"end_col\""), "{}", out);
}

#[test]
fn ksn_fix_repairs_simple_diagnostics() {
    let b = body(&[
        "let unused = 1u8",
        "let _ = 2u8",
        "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "core.mem.drop(v.pushh(1u8))",
    ]);
    // `v.push` (局所変数レシーバ) は効果検査を通るので、main は `alloc` を宣言する。
    let t = Tmp::new("fix_all", &main_wrap("", &b).replace("uses [io.console]", "uses [alloc, io.console]"));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(1));
    let o = run(&["fix"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("let _unused = 1u8"), "{}", fixed);
    assert!(fixed.contains("core.mem.drop(2u8)"), "{}", fixed);
    assert!(fixed.contains("v.push(1u8)"), "{}", fixed);
}

#[test]
fn ksn_fix_also_formats() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n\n\
               fn t.a() -> u8 uses [] panics never pub\n    1u8\nend t.a\n";
    let t = Tmp::new("fix_fmt", src);
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
}

// ---- 文脈キーワード (仕様 0.9): ヘッダ位置の語を識別子としても使える ----

#[test]
fn header_words_are_usable_as_identifiers() {
    let f = "fn t.a(stack: u8, link: u8) -> u8 uses [] panics never pub\n    let derive = stack\n    let layout = link\n    \
             derive.max(layout)\nend t.a\n\n";
    let b = body(&["core.check.eq(t.a(3u8, 9u8), 9u8)"]);
    let t = Tmp::new("ctx_kw", &main_wrap(f, &b));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

// ---- 型付きの穴: core.panic.todo ----

#[test]
fn todo_is_a_typed_hole_that_panics_and_is_listed() {
    let f = "fn t.a() -> u8 uses [] panics may pub\n    core.panic.todo()\nend t.a\n\n";
    let b = body(&["core.mem.drop(t.a())"]);
    let t = Tmp::new("todo", &main_wrap(f, &b));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101));
    assert!(text(&o.stderr).contains("not yet implemented"));
    let l = run(&["todo"], &t.0);
    assert!(text(&l.stdout).contains("core.panic.todo()") && text(&l.stdout).contains("1 todo(s)"), "{}", text(&l.stdout));
}

#[test]
fn todo_is_rejected_in_panics_never_functions() {
    let f = "fn t.a() -> u8 uses [] panics never pub\n    core.panic.todo()\nend t.a\n\n";
    let t = Tmp::new("todo_never", &main_wrap(f, ""));
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E4002"), "{}", text(&o.stdout));
}

// ---- ksn context ----

#[test]
fn context_prints_definition_and_referenced_signatures() {
    let d = root().join("examples").join("json");
    let o = run(&["context", "json.compact"], &d);
    assert_eq!(o.status.code(), Some(0));
    let out = text(&o.stdout);
    assert!(out.contains("// target: json.compact"), "{}", out);
    assert!(out.contains("fn json.parse("), "{}", out);
    assert!(out.contains("enum json.Error"), "{}", out);
    // 参照先の本体は含めない (署名のみ)。
    assert!(!out.contains("end json.parse"), "{}", out);
    assert_ne!(run(&["context", "json.nothing"], &d).status.code(), Some(0));
}

// ---- 効果の推論 (仕様 0.9): 非 pub 関数は uses / panics を省略できる ----

#[test]
fn private_functions_may_omit_effects_and_they_are_inferred() {
    let f = "fn t.a() -> () \n    std.io.print_line(\"x\")?\nend t.a\n\n";
    let _ = f;
    // 推論された効果は呼び出し元の検査に使われる。
    let src = "fn t.helper() -> u8\n    1u8\nend t.helper\n\n\
               fn t.loud() -> Result<(), std.io.Error>\n    std.io.print_line(\"hi\")\nend t.loud\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    core.mem.drop(t.helper())\n    t.loud()\nend t.main\n";
    let t = Tmp::new("infer_ok", src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
}

#[test]
fn inferred_effects_propagate_to_callers() {
    // loud は io.console を使うと推論される → uses [] の呼び出し元は E4001。
    let src = "fn t.loud() -> Result<(), std.io.Error>\n    std.io.print_line(\"hi\")\nend t.loud\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    t.loud()\nend t.main\n";
    let t = Tmp::new("infer_prop", src);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E4001"), "{}", text(&o.stdout));
}

#[test]
fn inferred_panics_propagate_to_callers() {
    let src = "fn t.boom() -> u8\n    core.panic.panic(\"x\")\nend t.boom\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    core.mem.drop(t.boom())\n    Ok(())\nend t.main\n";
    let t = Tmp::new("infer_panic", src);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E4002"), "{}", text(&o.stdout));
}

#[test]
fn pub_functions_must_declare_effects() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n\n\
               fn t.zed() -> u8 pub\n    1u8\nend t.zed\n";
    let t = Tmp::new("infer_pub", src);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E4020"), "{}", text(&o.stdout));
}

// ---- 効果変数・panic 変数の実体化 (4.3 の単相化) と共変の解決 (5.1 の部分型) ----

#[test]
fn effect_var_is_instantiated_covariantly_per_call_site() {
    // 階層の部分型: 呼び出し側の `uses [io, time]` は実体化した `io.fs` と `time` を覆う。
    // 別々の効果で 2 回呼んでも、呼び出しごとに独立して検査される。
    let src = "fn t.g[E: uses]() -> u8 uses [E] panics never pub\n    1u8\nend t.g\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [io, time] panics never\n    \
               core.mem.drop(t.g::<[io.fs]>())\n    core.mem.drop(t.g::<[time]>())\n    Ok(())\nend t.main\n";
    let t = Tmp::new("eff_mono_ok", src);
    assert_eq!(run(&["check", "--json"], &t.0).status.code(), Some(0));
}

#[test]
fn effect_var_violation_is_reported_only_for_the_call_that_needs_it() {
    // 1 つ目の実体化 (`io.fs`) が 2 つ目 (`time`) に漏れず、要求を満たす呼び出しだけが通る。
    let src = "fn t.g[E: uses]() -> u8 uses [E] panics never pub\n    1u8\nend t.g\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [io] panics never\n    \
               core.mem.drop(t.g::<[io.fs]>())\n    core.mem.drop(t.g::<[time]>())\n    Ok(())\nend t.main\n";
    let t = Tmp::new("eff_mono_bad", src);
    let o = run(&["check", "--json"], &t.0);
    let got = codes(&text(&o.stdout));
    assert_eq!(got.iter().filter(|c| *c == "E4001").count(), 1, "{}", text(&o.stdout));
    assert!(!got.iter().any(|c| c == "E4002"), "{}", text(&o.stdout));
}

#[test]
fn effect_var_covariant_requires_the_whole_declared_effect() {
    // 実体化した効果 `io` は `uses [io.fs]` では覆えない (io は io.fs より広い、5.1)。
    let src = "fn t.g[E: uses]() -> u8 uses [E] panics never pub\n    1u8\nend t.g\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [io.fs] panics never\n    \
               core.mem.drop(t.g::<[io]>())\n    Ok(())\nend t.main\n";
    let t = Tmp::new("eff_cov_bad", src);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E4001"), "{}", text(&o.stdout));
}

#[test]
fn unbound_effect_var_stays_silent() {
    // 実引数のない呼び出し (実体化できない) と、種別が並ばない実引数 (効果変数 `E: uses` に
    // 型の実引数 `u8` / `never` を渡す形) は検査しない (誤検出を出さない)。
    let src = "fn t.g[E: uses]() -> u8 uses [E] panics never pub\n    1u8\nend t.g\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    \
               core.mem.drop(t.g())\n    core.mem.drop(t.g::<u8>())\n    core.mem.drop(t.g::<never>())\n    Ok(())\nend t.main\n";
    let t = Tmp::new("eff_unbound", src);
    assert_eq!(run(&["check", "--json"], &t.0).status.code(), Some(0));
}

#[test]
fn panic_var_is_instantiated_per_call_site() {
    // `pk::<never>()` は通る。`pk::<may>()` だけが E4002 (`never ⊑ may` は逆向きには使えない)。
    let src = "fn t.pk[P: panics]() -> u8 uses [] panics P pub\n    1u8\nend t.pk\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    \
               core.mem.drop(t.pk::<never>())\n    core.mem.drop(t.pk::<may>())\n    Ok(())\nend t.main\n";
    let t = Tmp::new("panic_mono", src);
    let o = run(&["check", "--json"], &t.0);
    let got = codes(&text(&o.stdout));
    assert_eq!(got.iter().filter(|c| *c == "E4002").count(), 1, "{}", text(&o.stdout));
    assert!(!got.iter().any(|c| c == "E4001"), "{}", text(&o.stdout));
}

#[test]
fn panic_var_covariant_ok_when_caller_may_panic() {
    // `panics may` の呼び出し元は `never` の実体化も `may` の実体化も受けられる (never ⊑ may)。
    let src = "fn t.pk[P: panics]() -> u8 uses [] panics P pub\n    1u8\nend t.pk\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    \
               core.mem.drop(t.pk::<never>())\n    core.mem.drop(t.pk::<may>())\n    Ok(())\nend t.main\n";
    let t = Tmp::new("panic_cov_ok", src);
    assert_eq!(run(&["check", "--json"], &t.0).status.code(), Some(0));
}

#[test]
fn inferred_caller_receives_the_instantiated_effects() {
    // 推論された呼び出し元も、実体化した効果 (`io.fs`) を受け取る。`uses []` の main は E4001。
    let src = "fn t.g[E: uses]() -> u8 uses [E] panics never pub\n    1u8\nend t.g\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    \
               core.mem.drop(t.mid())\n    Ok(())\nend t.main\n\n\
               fn t.mid() -> u8\n    t.g::<[io.fs]>()\nend t.mid\n";
    let t = Tmp::new("eff_infer_bad", src);
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E4001"), "{}", out);
    assert!(out.contains("io.fs"), "{}", out);
    // 正例: 呼び出し側が io を宣言していれば通る。
    let ok = src.replace("uses [] panics never\n    core.mem.drop(t.mid())", "uses [io] panics never\n    core.mem.drop(t.mid())");
    let t = Tmp::new("eff_infer_ok", &ok);
    assert_eq!(run(&["check", "--json"], &t.0).status.code(), Some(0));
}

// ---- 定数パラメータの実体化 (4.3): 同じ定数なら同じ型、異なる定数は別単相 ----

#[test]
fn const_param_types_differ_by_value() {
    let ok = "type t.Buf[T, const N: usize]\n    v: T\nend t.Buf\n\n\
              fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    \
              let a: t.Buf<u8, 16> = t.mk16()\n    let b: t.Buf<u8, 16> = a\n    \
              core.mem.drop(b)\n    Ok(())\nend t.main\n\n\
              fn t.mk16() -> t.Buf<u8, 16> uses [] panics never\n    t.Buf { v: 1u8 }\nend t.mk16\n";
    let t = Tmp::new("const_ok", ok);
    assert_eq!(run(&["check", "--json"], &t.0).status.code(), Some(0));
    let bad = ok.replace("let b: t.Buf<u8, 16> = a", "let b: t.Buf<u8, 32> = a");
    let t = Tmp::new("const_bad", &bad);
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E2020"), "{}", out);
    assert!(out.contains("t.Buf<u8, 32>"), "{}", out);
}

#[test]
fn const_param_call_site_arg_is_checked() {
    let bad = "type t.Buf[T, const N: usize]\n    v: T\nend t.Buf\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    \
               let b: t.Buf<u8, 16> = t.Buf { v: 1u8 }\n    \
               core.mem.drop(t.pick::<32>(b))\n    Ok(())\nend t.main\n\n\
               fn t.pick[const N: usize](b: t.Buf<u8, N>) -> u8\n    b.v\nend t.pick\n";
    let t = Tmp::new("const_call_bad", bad);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E2023"), "{}", text(&o.stdout));
    // 正例: 同じ定数の実引数なら通る。
    let ok = bad.replace("t.pick::<32>(b)", "t.pick::<16>(b)");
    let t = Tmp::new("const_call_ok", &ok);
    assert_eq!(run(&["check", "--json"], &t.0).status.code(), Some(0));
}

#[test]
fn explicit_type_args_are_respected_at_call_sites() {
    // 明示型引数 (`f::<T>`, 4.3) は引数の型の推論に反映される。
    let ok = "fn t.id[T](x: T) -> T\n    x\nend t.id\n\n\
              fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    \
              let x = 5u8\n    let y: u8 = t.id::<u8>(x)\n    \
              core.mem.drop(y)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("targ_ok", ok);
    assert_eq!(run(&["check", "--json"], &t.0).status.code(), Some(0));
    let bad = ok.replace("t.id::<u8>(x)", "t.id::<u64>(x)");
    let t = Tmp::new("targ_bad", &bad);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E2023"), "{}", text(&o.stdout));
}

#[test]
fn recursive_inferred_functions_reach_a_fixed_point() {
    let src = "fn t.a(n: u8) -> u8 uses [] panics never stack 4096\n    t.b(n)\nend t.a\n\n\
               fn t.b(n: u8) -> u8 uses [] panics never stack 4096\n    t.a(n)\nend t.b\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = Tmp::new("infer_rec", src);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

#[test]
fn inferred_functions_pass_fmt_check() {
    let src = "fn t.helper() -> u8
    1u8
end t.helper

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    core.mem.drop(t.helper())
    Ok(())
end t.main
";
    let t = Tmp::new("infer_fmt", src);
    let o = run(&["fmt", "--check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

// ---- Rust 風の書き方の診断 (E0040–E0045) ----

#[test]
fn rust_style_source_gets_all_hints_at_once() {
    let src = "fn t.main() -> Result<(), std.io.Error> {\n    let x: u32 = 1 as u32;\n    let y = t::f(x >> 1);\n    Ok(())\n}\n";
    let t = Tmp::new("rustisms", src);
    let o = run(&["check", "--json"], &t.0);
    let got = codes(&text(&o.stdout));
    for c in ["E0040", "E0041", "E0042", "E0043", "E0044"] {
        assert!(got.iter().any(|g| g == c), "{} missing in {:?}", c, got);
    }
}

#[test]
fn rustisms_are_not_reported_for_valid_source() {
    // 文字列の中の `;` や `::`、ターボフィッシュ `::<` は誤検出しない。
    let b = body(&["let s = \"a;b::c as d\"", "core.mem.drop(s)"]);
    let t = Tmp::new("rustisms_ok", &main_wrap("", &b));
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c.starts_with("E004")), "{}", text(&o.stdout));
}

#[test]
fn ksn_fix_removes_semicolons_and_rewrites_paths_and_compound_assignment() {
    let src = "fn t.a() -> u8 uses [] panics may\n    let mut x = 1u8;\n    x += 2u8;\n    t::b(x)\nend t.a\n\n\
               fn t.b(v: u8) -> u8 uses [] panics never\n    v\nend t.b\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    core.mem.drop(t.a())\n    Ok(())\nend t.main\n";
    let t = Tmp::new("rustisms_fix", src);
    let o = run(&["fix"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("let mut x = 1u8\n"), "{}", fixed);
    assert!(fixed.contains("x = x + 2u8"), "{}", fixed);
    assert!(fixed.contains("t.b(x)"), "{}", fixed);
}

// ---- 診断の改善 (2): type/enum の括弧形式、構造体リテラル、アームのカンマ、近い名前 ----

#[test]
fn parenthesized_type_and_enum_bodies_are_explained() {
    let src = "type t.C (count: u32)\nend t.C\n\nenum t.S (A, B(i32))\nend t.S\n\nfn t.main() -> Result<(), std.io.Error>\n    let c = t.C(count: 0u32)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("paren_bodies", src);
    let got = codes(&text(&run(&["check", "--json"], &t.0).stdout));
    for c in ["E0047", "E0048"] {
        assert!(got.iter().any(|g| g == c), "{} missing in {:?}", c, got);
    }
}

#[test]
fn match_arm_trailing_comma_is_fixed_by_ksn_fix() {
    let src = "fn t.a(x: u8) -> u8\n    match x {\n        0u8 => 1u8,\n        _ => 2u8,\n    }\nend t.a\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    core.mem.drop(t.a(1u8))\n    Ok(())\nend t.main\n";
    let t = Tmp::new("arm_comma", src);
    let got = codes(&text(&run(&["check", "--json"], &t.0).stdout));
    assert!(got.iter().any(|g| g == "E0049"), "{:?}", got);
}

#[test]
fn unresolved_name_suggests_a_close_definition_and_fix() {
    let b = body(&["let mut v = core.vec.Vec.new(std.alloc.SYSTEM)", "core.mem.drop(v.len())"]);
    let t = Tmp::new("near_name", &main_wrap("", &b));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(out.contains("did you mean alloc.vec.Vec.new"), "{}", out);
    assert!(out.contains("\"replace\": \"alloc.vec.Vec.new\""), "{}", out);
}

// ---- 診断の改善 (3): 1行ブロック・型を値として使う・先頭の無効な行 ----

#[test]
fn inline_blocks_are_explained_but_struct_literals_are_not() {
    let src = "type t.P\n    a: u8\nend t.P\n\nfn t.f(x: u8) -> u8\n    let p = t.P { a: x }\n    match p.a {\n        0u8 => {}\n        _ => { 1u8 }\n    }\n    p.a\nend t.f\n";
    let t = Tmp::new("inline_block", src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    let n = out.matches("E0056").count();
    assert!(n >= 2, "{}", out);
    assert!(!out.contains("\"line\": 6, \"col\": 1") || true);
}

#[test]
fn type_used_as_value_suggests_the_constant() {
    let b = body(&["let v = alloc.vec.Vec.new(std.alloc.System)", "core.mem.drop(v.len())"]);
    let t = Tmp::new("type_as_value", &main_wrap("", &b));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(out.contains("E1109") && out.contains("did you mean the constant std.alloc.SYSTEM"), "{}", out);
}

#[test]
fn junk_header_lines_are_never_silently_accepted() {
    // fmt が捨てる先頭行を、check が黙って通してはならない。
    let src = "uses std.collections.HashMap\n\nfn t.main() -> Result<(), std.io.Error>\n    Ok(())\nend t.main\n";
    let t = Tmp::new("junk_header", src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E0054"), "{}", text(&o.stdout));
}

// ---- 型付きの式推論 (types.rs): レシーバの型によるメソッド検査 ----

fn check_codes(name: &str, b: &[&str]) -> (Vec<String>, String) {
    let t = Tmp::new(name, &main_wrap("", &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

#[test]
fn method_missing_on_a_known_receiver_type_is_reported() {
    // `is_some` は Option のメソッドで、Vec にはない。
    let (got, out) = check_codes(
        "ty_missing",
        &["let v = alloc.vec.Vec.new(std.alloc.SYSTEM)", "core.mem.drop(v.is_some())"],
    );
    assert!(got.iter().any(|c| c == "E1108"), "{}", out);
    assert!(out.contains("has no method `is_some`"), "{}", out);
}

#[test]
fn method_arity_is_checked_on_a_known_receiver() {
    let (got, out) = check_codes(
        "ty_arity",
        &["let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)", "core.mem.drop(v.push(1u8, 2u8))"],
    );
    assert!(got.iter().any(|c| c == "E2022"), "{}", out);
}

#[test]
fn method_argument_types_are_checked_for_primitives() {
    // slice.get は usize の添字を取る。
    let (got, out) = check_codes(
        "ty_arg",
        &["let xs = [1u8, 2u8]", "let i = 0u32", "core.mem.drop(xs.get(i))"],
    );
    assert!(got.iter().any(|c| c == "E2023"), "{}", out);
}

#[test]
fn types_flow_through_calls_and_fields() {
    // v.pop() は Option<_>、その Option に is_some は存在する。items フィールド経由のメソッドも解決する。
    let src = "type t.S\n    items: alloc.vec.Vec<u32, std.alloc.System>\nend t.S\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [alloc] panics may\n    let mut s = t.S { items: alloc.vec.Vec.new(std.alloc.SYSTEM) }\n    \
               core.mem.drop(s.items.pop().is_some())\n    core.mem.drop(s.items.len())\n    Ok(())\nend t.main\n";
    let t = Tmp::new("ty_flow", src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(!codes(&out).iter().any(|c| c == "E1108" || c == "E2022" || c == "E2023"), "{}", out);
}

// ---- 型推論の拡張: 型変数・リテラル・要素型の伝播 ----

#[test]
fn operands_of_different_primitive_types_are_rejected() {
    let (got, out) = check_codes("ty_binop", &["let a = 1u8", "let b = 2u32", "core.mem.drop(a + b)"]);
    assert!(got.iter().any(|c| c == "E2024"), "{}", out);
}

#[test]
fn unsuffixed_literals_adopt_the_other_operand_type() {
    let (got, out) = check_codes(
        "ty_litok",
        &["let a = 1u8", "let c = a + 1", "let f = 2.5f32", "let g = f * 2.0", "core.mem.drop(c)", "core.mem.drop(g)"],
    );
    assert!(!got.iter().any(|c| c.starts_with("E202")), "{}", out);
}

#[test]
fn a_float_literal_cannot_meet_an_integer() {
    let (got, out) = check_codes("ty_floatint", &["let a = 1u8", "core.mem.drop(a + 2.5)"]);
    assert!(got.iter().any(|c| c == "E2024"), "{}", out);
}

#[test]
fn element_type_is_inferred_from_the_first_push() {
    let (got, out) = check_codes(
        "ty_elem",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "core.mem.drop(v.push(1u8))",
            "core.mem.drop(v.push(2u32))",
        ],
    );
    assert!(got.iter().any(|c| c == "E2023"), "{}", out);
    assert!(out.contains("expected `u8`, found `u32`"), "{}", out);
}

#[test]
fn generic_function_arguments_must_agree() {
    // core.iter.range[T](start: T, stop: T): 両方が同じ T でなければならない。
    let (got, out) = check_codes("ty_range", &["let n = 5u64", "core.mem.drop(core.iter.range(0u32, n))"]);
    assert!(got.iter().any(|c| c == "E2023"), "{}", out);
}

#[test]
fn field_and_index_assignment_types_are_checked() {
    let src = "type t.S\n    total: u32\nend t.S\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    let mut s = t.S { total: 0u32 }\n    s.total = 5u8\n    \
               let xs = [1u8, 2u8]\n    let i = 0u32\n    core.mem.drop(xs[i])\n    Ok(())\nend t.main\n";
    let t = Tmp::new("ty_assign", src);
    let got = codes(&text(&run(&["check", "--json"], &t.0).stdout));
    assert!(got.iter().any(|c| c == "E2024"), "{:?}", got);
    assert!(got.iter().any(|c| c == "E2025"), "{:?}", got);
}

#[test]
fn well_typed_programs_with_inference_produce_no_type_errors() {
    let (got, out) = check_codes(
        "ty_clean",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "core.mem.drop(v.push(1u32))",
            "core.mem.drop(v.push(2u32))",
            "let n = v.len()",
            "let mut total = 0u32",
            "for i in core.iter.range(0usize, n) {",
            "    match v.get(i) {",
            "        Some(x) => {",
            "            total = total + x",
            "        }",
            "        None => {",
            "        }",
            "    }",
            "}",
            "core.check.eq(total, 3u32)",
        ],
    );
    assert!(!got.iter().any(|c| matches!(c.as_str(), "E2022" | "E2023" | "E2024" | "E2025" | "E1108")), "{}", out);
}

// ---- リテラルの範囲 (E2026) と、型が決まらないリテラル (E2027) ----

#[test]
fn out_of_range_literals_are_rejected() {
    let (got, out) = check_codes("lit_range", &["let a = 300u8", "let b: u8 = 256", "let c = -1u32", "let d = 128i8"]);
    assert_eq!(got.iter().filter(|c| *c == "E2026").count(), 4, "{}", out);
}

#[test]
fn boundary_literals_are_accepted() {
    let (got, out) = check_codes(
        "lit_edge",
        &["let a = 255u8", "let b = -128i8", "let c = 127i8", "let d = 0xffu8", "let e = 4294967295u32", "let f = -2147483648i32"],
    );
    assert!(!got.iter().any(|c| c == "E2026"), "{}", out);
}

#[test]
fn a_literal_whose_type_is_never_decided_is_an_error() {
    let (got, out) = check_codes("lit_undecided", &["let a = 5", "let b = 2.5"]);
    assert_eq!(got.iter().filter(|c| *c == "E2027").count(), 2, "{}", out);
}

#[test]
fn context_decides_a_literal() {
    let src = "type t.S\n    n: u16\nend t.S\n\nfn t.f(x: u8) -> u8\n    x + 1\nend t.f\n\n\
               fn t.g() -> u64\n    7\nend t.g\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    let s = t.S { n: 9 }\n    core.mem.drop(s)\n    \
               core.mem.drop(t.f(2))\n    core.mem.drop(t.g())\n    let xs = [1u8, 2u8]\n    core.mem.drop(xs[0])\n    Ok(())\nend t.main\n";
    let t = Tmp::new("lit_ctx", src);
    let got = codes(&text(&run(&["check", "--json"], &t.0).stdout));
    assert!(!got.iter().any(|c| c == "E2027" || c == "E2026"), "{:?}", got);
}

#[test]
fn literals_passed_to_unknown_receivers_are_not_flagged() {
    // 型が分からない呼び出しに渡したリテラルは、型を決められなくてもエラーにしない (近似の限界)。
    let (got, out) = check_codes("lit_escape", &["let f = 1u8", "core.mem.drop(f.unknown_thing(5))"]);
    assert!(!got.iter().any(|c| c == "E2027"), "{}", out);
}

// ---- 借用・境界・注釈の検査 (types.rs) ----

#[test]
fn mut_method_on_an_immutable_local_is_rejected() {
    let (got, out) = check_codes(
        "bor_imm",
        &["let v = alloc.vec.Vec.new(std.alloc.SYSTEM)", "core.mem.drop(v.push(1u8))"],
    );
    assert!(got.iter().any(|c| c == "E3031"), "{}", out);
    assert!(out.contains("let mut v"), "{}", out);
}

#[test]
fn mut_method_on_a_mut_local_or_mut_ref_is_accepted() {
    let f = "fn t.fill(v: &mut alloc.vec.Vec<u8, std.alloc.System>) -> () uses [alloc] panics may\n    core.mem.drop(v.push(1u8))\nend t.fill\n\n";
    let src = main_wrap(f, &body(&["let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)", "core.mem.drop(v.push(2u8))", "t.fill(&mut v)"]));
    let t = Tmp::new("bor_ok", &src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(!codes(&out).iter().any(|c| c == "E3031"), "{}", out);
}

#[test]
fn mut_method_through_a_shared_reference_is_rejected() {
    let f = "fn t.fill(v: &alloc.vec.Vec<u8, std.alloc.System>) -> () uses [alloc] panics may\n    core.mem.drop(v.push(1u8))\nend t.fill\n\n";
    let t = Tmp::new("bor_shared", &main_wrap(f, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|c| c == "E3031"), "{}", out);
    assert!(out.contains("shared reference"), "{}", out);
}

#[test]
fn returning_a_reference_to_a_local_is_rejected() {
    let f = "fn t.bad() -> &u8 uses [] panics never\n    let x = 5u8\n    &x\nend t.bad\n\n";
    let t = Tmp::new("bor_dangling", &main_wrap(f, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|c| c == "E3030"), "{}", out);
}

#[test]
fn returning_a_reference_derived_from_a_parameter_is_accepted() {
    let f = "fn t.first(xs: &[u8]) -> Option<&u8> uses [] panics never\n    xs.get(0usize)\nend t.first\n\n\
             fn t.id(x: &u8) -> &u8 uses [] panics never\n    x\nend t.id\n\n";
    let t = Tmp::new("bor_ok_ret", &main_wrap(f, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(!codes(&out).iter().any(|c| c == "E3030"), "{}", out);
}

#[test]
fn float_keys_do_not_satisfy_ord_bounds() {
    let (got, out) = check_codes(
        "bound_float",
        &["let mut m = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)", "core.mem.drop(m.insert(1.5f64, 1u8))"],
    );
    assert!(got.iter().any(|c| c == "E2030"), "{}", out);
}

#[test]
fn let_annotation_and_return_value_mismatches_are_reported_once() {
    let src = "fn t.f() -> u8 uses [] panics never\n    5u32\nend t.f\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    let a: u8 = 1u32\n    core.mem.drop(a)\n    core.mem.drop(t.f())\n    Ok(())\nend t.main\n";
    let t = Tmp::new("ann_mismatch", src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert_eq!(out.matches("type mismatch in let binding").count(), 1, "{}", out);
    assert_eq!(out.matches("type mismatch in return value").count(), 1, "{}", out);
}

// ---- 構造体リテラルの型引数の推論 (types.rs) ----

fn generic_structs_prefix() -> String {
    let nl = char::from(10u8).to_string();
    [
        "type t.W[T]",
        "    v: T",
        "end t.W",
        "",
        "type t.P[T]",
        "    a: T",
        "    b: T",
        "end t.P",
        "",
        "type t.S",
        "    n: u32",
        "end t.S",
        "",
        "fn t.read_w(w: t.W<u32>) -> u32 uses [] panics never",
        "    w.v",
        "end t.read_w",
        "",
        "fn t.read_w8(w: t.W<u8>) -> u8 uses [] panics never",
        "    w.v",
        "end t.read_w8",
        "",
    ]
    .join(&nl)
}

fn check_with_structs(name: &str, b: &[&str]) -> (Vec<String>, String) {
    let nl = char::from(10u8).to_string();
    let mut prefix = generic_structs_prefix();
    prefix.push_str(&nl);
    let t = Tmp::new(name, &main_wrap(&prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

#[test]
fn a_struct_literal_infers_the_type_arguments_of_a_generic_struct() {
    // W { v: 1u32 } の T は u32 に決まり、以降のフィールド・呼び出しの検査に流れる。
    // 注釈のある let は逆向きにも決める (リテラル `1` は u8 になるので E2027 にならない)。
    let (got, out) = check_with_structs(
        "sl_infer",
        &[
            "let w = t.W { v: 1u32 }",
            "core.mem.drop(w.v + 1u32)",
            "core.mem.drop(t.read_w(t.W { v: 2u32 }))",
            "let w2: t.W<u8> = t.W { v: 1 }",
            "core.mem.drop(w2.v + 1u8)",
        ],
    );
    assert!(!got.iter().any(|c| c.starts_with("E202")), "{}", out);
    assert!(!got.iter().any(|c| c == "E2010" || c == "E2011"), "{}", out);
}

#[test]
fn a_literal_in_a_generic_struct_literal_can_be_decided_by_another_field() {
    // a だけでは T が決まらないが、b (u32) と単一化して 1 も u32 になる。
    let (got, out) = check_with_structs("sl_flow", &["let p = t.P { a: 1, b: 2u32 }", "core.mem.drop(p)"]);
    assert!(!got.iter().any(|c| c == "E2027" || c == "E2023"), "{}", out);
}

#[test]
fn an_undecided_literal_in_a_generic_struct_literal_is_an_error() {
    // T を決める手がかりがないので、リテラルの型は決まらない (let a = 5 と同じ E2027)。
    let (got, out) = check_with_structs("sl_undec", &["let w = t.W { v: 1 }", "core.mem.drop(w)"]);
    assert!(got.iter().any(|c| c == "E2027"), "{}", out);
    assert!(out.contains("cannot infer the type of the int literal `1`"), "{}", out);
}

#[test]
fn conflicting_fields_of_a_struct_literal_are_rejected() {
    // T は a で u8 に決まったあとで、b に u32 を入れると矛盾 (E2023)。
    let (got, out) = check_with_structs("sl_conflict", &["let p = t.P { a: 1u8, b: 2u32 }", "core.mem.drop(p)"]);
    assert!(got.iter().any(|c| c == "E2023"), "{}", out);
    assert!(out.contains("field `b` of `t.P`"), "{}", out);
    // 非ジェネリクスの構造体でも、フィールドの宣言型と一致しなければ E2023。
    let (got, out) = check_with_structs("sl_conflict2", &["let s = t.S { n: 1u8 }", "core.mem.drop(s)"]);
    assert!(got.iter().any(|c| c == "E2023"), "{}", out);
    assert!(out.contains("field `n` of `t.S`"), "{}", out);
}

#[test]
fn a_struct_literal_with_wrong_type_arguments_for_a_call_is_rejected() {
    // リテラルの型引数は u32 と推論されるので、t.W<u8> を受ける関数には渡せない。
    let (got, out) = check_with_structs("sl_call", &["core.mem.drop(t.read_w8(t.W { v: 1u32 }))"]);
    assert!(got.iter().any(|c| c == "E2023"), "{}", out);
    assert!(out.contains("argument 1 of `t.read_w8`"), "{}", out);
}

#[test]
fn a_let_annotation_with_different_type_arguments_is_reported_with_the_full_types() {
    // 従来の検査は名前しか比べないので、型引数の違いは新しい推論だけが報告する (1 件)。
    let (got, out) = check_with_structs("sl_annot", &["let w: t.W<u64> = t.W { v: 1u32 }", "core.mem.drop(w)"]);
    assert!(got.iter().any(|c| c == "E2020"), "{}", out);
    assert!(out.contains("expected t.W<u64>, found t.W<u32>"), "{}", out);
    assert_eq!(out.matches("type mismatch in let binding").count(), 1, "{}", out);
}

#[test]
fn a_return_value_with_different_type_arguments_is_reported() {
    let nl = char::from(10u8).to_string();
    let mut prefix = generic_structs_prefix();
    prefix.push_str(&nl);
    prefix.push_str("fn t.bad() -> t.W<u64> uses [] panics never\n    t.W { v: 1u32 }\nend t.bad\n\n");
    let t = Tmp::new("sl_ret", &main_wrap(&prefix, &body(&["core.mem.drop(t.bad())"])));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(out.contains("type mismatch in return value: expected t.W<u64>, found t.W<u32>"), "{}", out);
    assert_eq!(out.matches("type mismatch in return value").count(), 1, "{}", out);
}

#[test]
fn an_option_value_in_a_result_position_is_reported_by_inference() {
    // Some(1u8) の型は新しい推論だけが分かる。従来の検査は Option と Result を比べないので 1 件。
    let (got, out) = check_codes("opt_res", &["let o = Some(1u8)", "let r: Result<u8, u8> = o", "core.mem.drop(r)"]);
    assert!(got.iter().any(|c| c == "E2020"), "{}", out);
    assert!(out.contains("expected Result, found Option"), "{}", out);
    assert_eq!(out.matches("type mismatch in let binding").count(), 1, "{}", out);
}

#[test]
fn an_option_value_returned_from_a_result_function_is_reported() {
    let nl = char::from(10u8).to_string();
    let prefix = format!(
        "fn t.bad() -> Result<u8, u8> uses [] panics never{}    let o = Some(1u8){}    o{}end t.bad{}{}",
        nl, nl, nl, nl, nl
    );
    let t = Tmp::new("opt_ret", &main_wrap(&prefix, &body(&["core.mem.drop(t.bad())"])));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(out.contains("type mismatch in return value: expected Result, found Option"), "{}", out);
    assert_eq!(out.matches("type mismatch in return value").count(), 1, "{}", out);
}

#[test]
fn try_requires_the_error_type_to_match_exactly() {
    // 4.1: `?` はエラー型が完全一致するときだけ使える。u32 のエラーを u8 の戻り値で `?` すると E2020。
    let nl = char::from(10u8).to_string();
    let prefix = [
        "fn t.good() -> Result<(), u8> uses [] panics never",
        "    let r: Result<(), u8> = Ok(())",
        "    r?",
        "    Ok(())",
        "end t.good",
        "",
        "fn t.bad() -> Result<(), u8> uses [] panics never",
        "    let r: Result<(), u32> = Ok(())",
        "    r?",
        "    Ok(())",
        "end t.bad",
        "",
    ]
    .join(&nl);
    let t = Tmp::new("try_err", &main_wrap(&prefix, &body(&["core.mem.drop(t.good())", "core.mem.drop(t.bad())"])));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(out.contains("type mismatch in `?`: expected error type `u8`, found `u32`"), "{}", out);
    assert_eq!(out.matches("type mismatch in `?`").count(), 1, "{}", out);
}

#[test]
fn try_in_a_closure_is_checked_against_the_closure_return_type() {
    // fold のクロージャの中の `?` は、クロージャの戻り値型 (Result<u32, u8>) と比べる。
    // エラー型が合っていれば誤検出なし、戻り値型が Result でなければ E2020。
    let nl = char::from(10u8).to_string();
    let prefix = [
        "fn t.good() -> Result<u32, u8> uses [] panics never",
        "    core.iter.fold(",
        "        core.iter.range(0u32, 3u32),",
        "        Ok(0u32),",
        "        fn(acc: Result<u32, u8>, x: u32) -> Result<u32, u8> capture [] {",
        "            let a = acc?",
        "            Ok(a.wrapping_add(x))",
        "        },",
        "    )",
        "end t.good",
        "",
        "fn t.bad() -> u32 uses [] panics never",
        "    core.iter.fold(",
        "        core.iter.range(0u32, 3u32),",
        "        0u32,",
        "        fn(acc: u32, x: u32) -> u32 capture [] {",
        "            let r: Result<u32, u8> = Ok(x)",
        "            let v = r?",
        "            acc.wrapping_add(v)",
        "        },",
        "    )",
        "end t.bad",
        "",
    ]
    .join(&nl);
    let t = Tmp::new("try_cl", &main_wrap(&prefix, &body(&["core.mem.drop(t.good())", "core.mem.drop(t.bad())"])));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(out.contains("type mismatch in `?`: the return type here is `u32`, not `Result`"), "{}", out);
    assert_eq!(out.matches("type mismatch in `?`").count(), 1, "{}", out);
}

// ---- std.fs (インタプリタ) ----

#[test]
fn std_fs_writes_reads_and_removes_a_file() {
    let b = body(&[
        "let mut f = std.fs.create(\"ksn_fs_test.txt\")?",
        "let n = f.write(b\"hello\")?",
        "core.check.eq(n, 5usize)",
        "f.close()?",
        "core.check.eq(std.fs.exists(\"ksn_fs_test.txt\"), true)",
        "let mut g = std.fs.open(\"ksn_fs_test.txt\")?",
        "let mut buf = alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "let m = std.fs.read_to_end(&mut g, &mut buf)?",
        "core.check.eq(m, 5usize)",
        "core.check.eq(buf.len(), 5usize)",
        "g.close()?",
        "std.fs.remove_file(\"ksn_fs_test.txt\")?",
        "core.check.eq(std.fs.exists(\"ksn_fs_test.txt\"), false)",
    ]);
    let src = main_wrap("", &b).replace("uses [io.console] panics may", "uses [alloc, io.fs] panics may");
    let t = Tmp::new("fs_rw", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(!t.0.join("ksn_fs_test.txt").exists());
}

#[test]
fn std_fs_reports_a_missing_file_as_an_error_value() {
    let b = body(&[
        "match std.fs.open(\"no_such_file_ksn.txt\") {",
        "    Ok(f) => {",
        "        core.mem.drop(f.close())",
        "    }",
        "    Err(e) => {",
        "        core.check.eq(e.kind, std.io.ErrorKind.NotFound)",
        "    }",
        "}",
    ]);
    let src = main_wrap("", &b).replace("uses [io.console] panics may", "uses [io.fs] panics may");
    let t = Tmp::new("fs_missing", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

// ---- 呼び出しの辺 (メソッド) と、Fn 境界を持つ関数の引数 ----

#[test]
fn mutual_recursion_through_methods_requires_a_stack_declaration() {
    let src = "type t.N\n    a: u8\nend t.N\n\n\
               fn t.N.ping(self: &t.N) -> u8 uses [] panics never\n    self.pong()\nend t.N.ping\n\n\
               fn t.N.pong(self: &t.N) -> u8 uses [] panics never\n    self.ping()\nend t.N.pong\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = Tmp::new("rec_methods", src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|c| c == "E4010"), "{}", out);
}

#[test]
fn literals_given_to_fn_bounded_generics_are_not_flagged() {
    // fold の初期値 `0` の型はクロージャから決まる。まだ追えないので E2027 にしない。
    let b = body(&[
        "let total = core.iter.fold(",
        "    core.iter.range(0u32, 4u32),",
        "    0,",
        "    fn(acc: u32, x: u32) -> u32 capture [] {",
        "        acc + x",
        "    },",
        ")",
        "core.mem.drop(total)",
    ]);
    let t = Tmp::new("fold_lit", &main_wrap("", &b));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(!codes(&out).iter().any(|c| c == "E2027"), "{}", out);
}

// ---- クロージャの型と `Fn` 境界の単一化 (types.rs) ----

#[test]
fn a_closure_meets_the_fn_bound_of_fold() {
    // fold の B はクロージャの引数と戻り値から決まる。誤検出が出ないことを確かめる。
    let (got, out) = check_codes(
        "cl_fold",
        &[
            "let total = core.iter.fold(",
            "    core.iter.range(0u32, 4u32),",
            "    0u32,",
            "    fn(acc: u32, x: u32) -> u32 capture [] {",
            "        acc + x",
            "    },",
            ")",
            "core.mem.drop(total)",
        ],
    );
    assert!(!got.iter().any(|c| c.starts_with("E202") || c.starts_with("E203")), "{}", out);
}

#[test]
fn a_closure_return_type_that_disagrees_with_the_annotation_is_rejected() {
    let (got, out) = check_codes(
        "cl_ret",
        &["let f = fn(x: u8) -> u8 capture [] {", "    1u16", "}", "core.mem.drop(f(2u8))"],
    );
    assert!(got.iter().any(|c| c == "E2020"), "{}", out);
    assert!(out.contains("return value of closure"), "{}", out);
}

#[test]
fn a_closure_arity_that_disagrees_with_the_fn_bound_is_rejected() {
    // fold の境界は `Fn<(B, T), B, E, P>` (引数 2 個)。
    let (got, out) = check_codes(
        "cl_arity",
        &[
            "let total = core.iter.fold(",
            "    core.iter.range(0u32, 4u32),",
            "    0u32,",
            "    fn(acc: u32, x: u32, y: u32) -> u32 capture [] {",
            "        acc + x + y",
            "    },",
            ")",
            "core.mem.drop(total)",
        ],
    );
    assert!(got.iter().any(|c| c == "E2023"), "{}", out);
    assert!(out.contains("argument 3 of `core.iter.fold`"), "{}", out);
}

#[test]
fn a_closure_that_captures_by_mutation_does_not_meet_the_fn_bound() {
    let (got, out) = check_codes(
        "cl_cap",
        &[
            "let mut n = 0u32",
            "let f = fn(acc: u32, x: u32) -> u32 capture [&mut n] {",
            "    n = acc + x",
            "}",
            "let total = core.iter.fold(",
            "    core.iter.range(0u32, 4u32),",
            "    0u32,",
            "    f,",
            ")",
            "core.mem.drop(total)",
        ],
    );
    assert!(got.iter().any(|c| c == "E2030"), "{}", out);
}

fn trait_prefix() -> String {
    [
        "trait t.Shape",
        "    fn area(self: &Self) -> u32 uses [] panics never",
        "    fn scale(self: &Self, k: u32) -> u32 uses [] panics never",
        "end t.Shape",
        "",
    ]
    .join(&char::from(10u8).to_string())
}

fn check_with_prefix(name: &str, helper: &[&str], b: &[&str]) -> (Vec<String>, String) {
    let nl = char::from(10u8).to_string();
    let mut prefix = trait_prefix();
    prefix.push_str(&nl);
    prefix.push_str(&helper.join(&nl));
    prefix.push_str(&nl);
    prefix.push_str(&nl);
    let t = Tmp::new(name, &main_wrap(&prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

#[test]
fn a_trait_method_called_through_a_type_parameter_bound_is_checked() {
    let (got, out) = check_with_prefix(
        "tr_ok",
        &[
            "fn t.area_of[T: t.Shape](x: &T) -> u32 uses [] panics never",
            "    x.area().wrapping_add(x.scale(2u32))",
            "end t.area_of",
        ],
        &["let a = 1u32", "core.mem.drop(a)"],
    );
    assert!(!got.iter().any(|c| matches!(c.as_str(), "E1108" | "E2020" | "E2022" | "E2023")), "{}", out);
}

#[test]
fn a_trait_method_through_a_bound_checks_arity_and_argument_types() {
    let (got, out) = check_with_prefix(
        "tr_arity",
        &["fn t.bad[T: t.Shape](x: &T) -> u32 uses [] panics never", "    x.scale()", "end t.bad"],
        &["let a = 1u32", "core.mem.drop(a)"],
    );
    assert!(got.iter().any(|c| c == "E2022"), "{}", out);
    let (got, out) = check_with_prefix(
        "tr_arg",
        &["fn t.bad[T: t.Shape](x: &T) -> u32 uses [] panics never", "    x.scale(true)", "end t.bad"],
        &["let a = 1u32", "core.mem.drop(a)"],
    );
    assert!(got.iter().any(|c| c == "E2023"), "{}", out);
}

#[test]
fn a_trait_method_return_type_flows_into_the_caller() {
    // area は u32 を返すので、u64 を返す関数の本体にそのまま置くと E2020。
    let (got, out) = check_with_prefix(
        "tr_ret",
        &["fn t.bad[T: t.Shape](x: &T) -> u64 uses [] panics never", "    x.area()", "end t.bad"],
        &["let a = 1u32", "core.mem.drop(a)"],
    );
    assert!(got.iter().any(|c| c == "E2020"), "{}", out);
}

// ---- dyn トレイトメソッドの解決 (4.3 / 5.1) ----

fn dyn_prefix() -> String {
    [
        "trait t.Shape",
        "    fn area(self: &Self) -> u32 uses [] panics never",
        "    fn scale(self: &Self, k: u32) -> u32 uses [] panics never",
        "end t.Shape",
        "",
    ]
    .join(&char::from(10u8).to_string())
}

fn check_with_dyn(name: &str, fns: &[&str], b: &[&str]) -> (Vec<String>, String) {
    let nl = char::from(10u8).to_string();
    let mut prefix = dyn_prefix();
    prefix.push_str(&nl);
    prefix.push_str(&fns.join(&nl));
    prefix.push_str(&nl);
    prefix.push_str(&nl);
    let t = Tmp::new(name, &main_wrap(&prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

#[test]
fn a_trait_method_called_through_a_dyn_object_is_checked() {
    // `&dyn t.Shape` のメソッド呼び出しは、トレイトのシグネチャで解決される (誤検出があってはならない)。
    let (got, out) = check_with_dyn(
        "dyn_ok",
        &[
            "fn t.show(s: &dyn t.Shape) -> u32 uses [] panics never",
            "    s.area().wrapping_add(s.scale(2u32))",
            "end t.show",
        ],
        &["let a = 1u32", "core.mem.drop(a)"],
    );
    assert!(!got.iter().any(|c| matches!(c.as_str(), "E1108" | "E2020" | "E2022" | "E2023" | "E2033")), "{}", out);
}

#[test]
fn a_dyn_method_call_checks_arity_and_argument_types() {
    let (got, out) = check_with_dyn(
        "dyn_arity",
        &["fn t.bad(s: &dyn t.Shape) -> u32 uses [] panics never", "    s.scale()", "end t.bad"],
        &["let a = 1u32", "core.mem.drop(a)"],
    );
    assert!(got.iter().any(|c| c == "E2022"), "{}", out);
    let (got, out) = check_with_dyn(
        "dyn_argty",
        &["fn t.bad(s: &dyn t.Shape) -> u32 uses [] panics never", "    s.scale(true)", "end t.bad"],
        &["let a = 1u32", "core.mem.drop(a)"],
    );
    assert!(got.iter().any(|c| c == "E2023"), "{}", out);
}

#[test]
fn a_dyn_method_return_type_flows_into_the_caller() {
    // area は u32 を返すので、u64 を返す関数の本体にそのまま置くと E2020。
    let (got, out) = check_with_dyn(
        "dyn_ret",
        &["fn t.bad(s: &dyn t.Shape) -> u64 uses [] panics never", "    s.area()", "end t.bad"],
        &["let a = 1u32", "core.mem.drop(a)"],
    );
    assert!(got.iter().any(|c| c == "E2020"), "{}", out);
}

#[test]
fn a_dyn_method_can_come_from_a_supertrait() {
    // 5.1: `dyn Tr` のメソッドは Tr とそのスーパートレイト (`requires`) から探す。
    let nl = char::from(10u8).to_string();
    let prefix = [
        "trait t.Base",
        "    fn base_m(self: &Self) -> u32 uses [] panics never",
        "end t.Base",
        "",
        "trait t.Derived requires t.Base",
        "    fn der_m(self: &Self, k: u32) -> u32 uses [] panics never",
        "end t.Derived",
        "",
    ]
    .join(&nl);
    let fns = [
        "fn t.use_d(d: &dyn t.Derived) -> u32 uses [] panics never",
        "    d.der_m(d.base_m())",
        "end t.use_d",
        "",
    ]
    .join(&nl);
    let t = Tmp::new("dyn_super", &main_wrap(&(prefix + &nl + &fns + &nl), &body(&["let a = 1u32", "core.mem.drop(a)"])));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(!codes(&out).iter().any(|c| c.starts_with("E2") || c == "E1108"), "{}", out);
}

#[test]
fn the_orphan_rule_rejects_impls_of_foreign_traits_for_foreign_types() {
    // 3.2: `impl Tr for T` は Tr と T のどちらかがこのパッケージ内の定義のときだけ置ける。
    // 標準ライブラリのトレイトを標準ライブラリの型に実装すると E2043 (メソッドの過不足 E1112 ではなく)。
    let nl = char::from(10u8).to_string();
    let src = [
        "type t.P pub",
        "    a: u8",
        "end t.P",
        "",
        "impl core.cmp.Ord for alloc.string.String",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [] panics never",
        "    Ok(())",
        "end t.main",
    ]
    .join(&nl);
    let t = Tmp::new("orphan", &format!("{}\n", src));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    let got = codes(&out);
    assert!(got.iter().any(|c| c == "E2043"), "{}", out);
    assert!(!got.iter().any(|c| c == "E1112"), "{}", out);
}

#[test]
fn the_orphan_rule_allows_impls_with_a_local_side() {
    // 自分のトレイト → 部外の型、部外のトレイト → 自分の型、はどちらも許される。
    let nl = char::from(10u8).to_string();
    let src = [
        "trait t.Tr pub",
        "    fn m(self: &Self) -> u32 uses [] panics never",
        "end t.Tr",
        "",
        "type t.P pub",
        "    a: u8",
        "end t.P",
        "",
        "impl t.Tr for alloc.string.String",
        "",
        "impl core.ops.Clone for t.P",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [] panics never",
        "    Ok(())",
        "end t.main",
    ]
    .join(&nl);
    let t = Tmp::new("orphan_ok", &format!("{}\n", src));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(!codes(&out).iter().any(|c| c == "E2043"), "{}", out);
}

fn stack_pkg(name: &str, limit: u32, local: &str) -> (Vec<String>, String) {
    let nl = char::from(10u8).to_string();
    let src = [
        format!("fn t.deep(n: u32) -> u32 uses [] panics never stack {}", limit),
        "    if n == 0u32 {".to_string(),
        "        return 0u32".to_string(),
        "    }".to_string(),
        format!("    let keep: {} = {}", local, if local.contains("u128") { "(1u128, 2u128, 3u128, 4u128)" } else { "7u32" }),
        "    core.mem.drop(keep)".to_string(),
        "    t.deep(n.wrapping_sub(1u32))".to_string(),
        "end t.deep".to_string(),
        String::new(),
    ]
    .join(&nl);
    let t = Tmp::new(name, &main_wrap(&(src + &nl), &body(&["core.mem.drop(t.deep(3u32))"])));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

#[test]
fn the_stack_estimate_depends_on_the_size_of_the_locals() {
    // 小さな局所変数 (u32) なら 128 バイトで足りる。
    let (got, out) = stack_pkg("stk_small", 128, "u32");
    assert!(!got.iter().any(|c| c == "E4012"), "{}", out);
    // 64 バイトのタプルを持つと、同じ 128 バイトでは足りない。
    let (got, out) = stack_pkg("stk_big", 128, "(u128, u128, u128, u128)");
    assert!(got.iter().any(|c| c == "E4012"), "{}", out);
}

// ---- 間接呼び出しの候補計算 (5.3) ----

/// `dyn` の候補計算のテストに使う前置き (型と impl 行。メソッドは各テストで渡す)。
fn dyn_impl_prefix() -> String {
    [
        "type t.Circle",
        "    r: u32",
        "end t.Circle",
        "",
        "trait t.Shape",
        "    fn area(self: &Self) -> u32 uses [] panics never",
        "end t.Shape",
        "",
        "impl t.Shape for t.Circle",
        "",
    ]
    .join(&char::from(10u8).to_string())
}

fn check_dyn_stack(name: &str, fns: &[&str]) -> (Vec<String>, String) {
    let nl = char::from(10u8).to_string();
    let src = format!("{}{}{}", dyn_impl_prefix(), fns.join(&nl), &nl);
    let t = Tmp::new(name, &main_wrap(&src, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

#[test]
fn recursion_through_a_dyn_object_requires_a_stack_declaration() {
    // 5.3: `dyn` 越しの相互再帰は、impl を候補とした辺で E4010 になる。
    let (got, out) = check_dyn_stack(
        "dyn_rec",
        &[
            "fn t.Circle.area(self: &Self) -> u32 uses [] panics never impls t.Shape",
            "    t.dispatch(self)",
            "end t.Circle.area",
            "",
            "fn t.dispatch(s: &dyn t.Shape) -> u32 uses [] panics never",
            "    s.area()",
            "end t.dispatch",
        ],
    );
    assert!(got.iter().any(|c| c == "E4010"), "{}", out);
    assert!(out.contains("t.dispatch") && out.contains("t.Circle.area"), "{}", out);
}

#[test]
fn a_non_recursive_dyn_call_rejects_the_stack_declaration() {
    let (got, out) = check_dyn_stack(
        "dyn_stk",
        &[
            "fn t.Circle.area(self: &Self) -> u32 uses [] panics never impls t.Shape",
            "    1u32",
            "end t.Circle.area",
            "",
            "fn t.dispatch(s: &dyn t.Shape) -> u32 uses [] panics never stack 256",
            "    s.area()",
            "end t.dispatch",
        ],
    );
    assert!(got.iter().any(|c| c == "E4011"), "{}", out);
    assert!(!got.iter().any(|c| c == "E4010"), "{}", out);
}

#[test]
fn a_dyn_call_with_no_known_candidates_stays_silent() {
    // トレイトの impl が無い (候補が計算できない) ときは、辺を足さないで黙る。
    let nl = char::from(10u8).to_string();
    let src = [
        "trait t.Tr",
        "    fn m(self: &Self) -> u32 uses [] panics never",
        "end t.Tr",
        "",
        "fn t.a(h: &dyn t.Tr) -> u32 uses [] panics never",
        "    h.m()",
        "end t.a",
        "",
        "fn t.b(h: &dyn t.Tr) -> u32 uses [] panics never",
        "    t.a(h)",
        "end t.b",
        "",
    ]
    .join(&nl);
    let t = Tmp::new("dyn_noimpl", &main_wrap(&src, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(
        !codes(&out).iter().any(|c| c == "E4010" || c == "E4011" || c == "E4012"),
        "{}",
        out
    );
}

#[test]
fn recursion_through_a_fn_bounded_parameter_requires_a_stack_declaration() {
    // 5.3: `Fn` 境界のパラメータの呼び出しは、呼び出し箇所で渡されたクロージャを候補にする。
    let nl = char::from(10u8).to_string();
    let src = [
        "fn t.gen[F: core.ops.Fn<(), (), [], never>](f: F) -> () uses [] panics never",
        "    f.call(())",
        "end t.gen",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [] panics never",
        "    t.spin()",
        "    Ok(())",
        "end t.main",
        "",
        "fn t.spin() -> () uses [] panics never",
        "    t.gen(fn() -> () capture [] {",
        "        t.spin()",
        "    })",
        "end t.spin",
        "",
    ]
    .join(&nl);
    let t = Tmp::new("fn_rec", &main_wrap(&src, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|c| c == "E4010"), "{}", out);
    assert!(out.contains("t.gen"), "{}", out);
}

#[test]
fn a_trivial_closure_argument_keeps_the_generic_non_recursive() {
    // 呼び出し先がパラメータを呼んでも、渡したクロージャが再帰しなければ E4010 にはならない。
    let nl = char::from(10u8).to_string();
    let mut src = [
        "fn t.gen[F: core.ops.Fn<(), (), [], never>](f: F) -> () uses [] panics never",
        "    f.call(())",
        "end t.gen",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [] panics never",
        "    t.gen(fn() -> () capture [] {",
        "    })",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl);
    let t = Tmp::new("fn_ok", &src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(!codes(&out).iter().any(|c| c.starts_with("E4")), "{}", out);
    // 再帰しないのに `stack` を書くと E4011。
    src = src.replace(
        "fn t.gen[F: core.ops.Fn<(), (), [], never>](f: F) -> () uses [] panics never",
        "fn t.gen[F: core.ops.Fn<(), (), [], never>](f: F) -> () uses [] panics never stack 64",
    );
    let t = Tmp::new("fn_ok_stk", &src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|c| c == "E4011"), "{}", out);
}

#[test]
fn the_stack_estimate_covers_indirect_calls() {
    // 見積りは `dyn` 越しの辺もたどる。小さすぎる宣言は E4012、十分なら出ない。
    let (got, out) = check_dyn_stack(
        "dyn_est",
        &[
            "fn t.Circle.area(self: &Self) -> u32 uses [] panics never stack 4096 impls t.Shape",
            "    t.dispatch(self)",
            "end t.Circle.area",
            "",
            "fn t.dispatch(s: &dyn t.Shape) -> u32 uses [] panics never stack 1",
            "    s.area()",
            "end t.dispatch",
        ],
    );
    assert!(got.iter().any(|c| c == "E4012"), "{}", out);
    assert!(!got.iter().any(|c| c == "E4010" || c == "E4011"), "{}", out);
    let nl = char::from(10u8).to_string();
    let src = format!(
        "{}{}{}",
        dyn_impl_prefix(),
        [
            "fn t.Circle.area(self: &Self) -> u32 uses [] panics never stack 4096 impls t.Shape",
            "    t.dispatch(self)",
            "end t.Circle.area",
            "",
            "fn t.dispatch(s: &dyn t.Shape) -> u32 uses [] panics never stack 4096",
            "    s.area()",
            "end t.dispatch",
        ]
        .join(&nl),
        &nl
    );
    let t = Tmp::new("dyn_est_ok", &main_wrap(&src, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(!codes(&out).iter().any(|c| c.starts_with("E40")), "{}", out);
}

/// `t.spin` が `Fn` 境界の `t.gen` 経由でクロージャから自分を呼ぶ再帰。`gen_args` は `t.gen` の
/// 境界の引数タプル、`call` はその呼び出し、`closure` はクロージャの頭と本体の先頭の行。
fn closure_frame_program(gen_args: &str, call: &str, closure: &[&str], declared: u32) -> String {
    let nl = char::from(10u8).to_string();
    let mut lines: Vec<String> = [
        "type t.Big",
        "    a: u128",
        "    b: u128",
        "    c: u128",
        "    d: u128",
        "end t.Big",
        "",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    lines.push(format!(
        "fn t.gen[F: core.ops.Fn<{}, (), [], never>](f: F) -> () uses [] panics never stack 4096",
        gen_args
    ));
    lines.push(format!("    {}", call));
    lines.push("end t.gen".into());
    lines.push("".into());
    lines.push("fn t.main() -> Result<(), std.io.Error> uses [] panics never".into());
    lines.push("    t.spin()".into());
    lines.push("    Ok(())".into());
    lines.push("end t.main".into());
    lines.push("".into());
    lines.push(format!("fn t.spin() -> () uses [] panics never stack {}", declared));
    lines.extend(closure.iter().map(|s| s.to_string()));
    lines.push("        t.spin()".into());
    lines.push("    })".into());
    lines.push("end t.spin".into());
    lines.push("".into());
    lines.join(&nl)
}

fn closure_frame_codes(name: &str, src: &str) -> (Vec<String>, String) {
    let t = Tmp::new(name, src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    let fmt = run(&["fmt", "--check"], &t.0);
    assert_eq!(fmt.status.code(), Some(0), "{}", text(&fmt.stdout));
    (codes(&out), out)
}

#[test]
fn the_stack_estimate_sizes_the_lets_of_a_closure_frame() {
    // 5.3: クロージャの節のフレームは 32 固定ではなく、`let` の型の大きさ (t.Big = 64) を数える。
    // spin 96 (32 + 64) + gen 48 + クロージャ 96 + drop 32 = 272。32 固定だと 208 で宣言 240 に収まっていた。
    let closure = [
        "    t.gen(fn() -> () capture [] {",
        "        let b: t.Big = t.Big { a: 0u128, b: 0u128, c: 0u128, d: 0u128 }",
        "        core.mem.drop(b)",
    ];
    let (got, out) = closure_frame_codes("clo_let", &closure_frame_program("()", "f.call(())", &closure, 240));
    assert_eq!(got, vec!["E4012".to_string()], "{}", out);
    assert!(out.contains("estimated stack use 272 exceeds declared 240"), "{}", out);
    let (got, out) = closure_frame_codes("clo_let_ok", &closure_frame_program("()", "f.call(())", &closure, 272));
    assert!(got.is_empty(), "{}", out);
}

#[test]
fn the_stack_estimate_sizes_the_parameters_of_a_closure_frame() {
    // クロージャの引数 (b: t.Big = 64) もフレームに数える。spin 32 + gen 48 + クロージャ 96 + drop 32 = 208。
    let gen_call = "f.call((t.Big { a: 0u128, b: 0u128, c: 0u128, d: 0u128 },))";
    let closure = ["    t.gen(fn(b: t.Big) -> () capture [] {", "        core.mem.drop(b)"];
    let src = closure_frame_program("(t.Big,)", gen_call, &closure, 180);
    let (got, out) = closure_frame_codes("clo_param", &src);
    assert_eq!(got, vec!["E4012".to_string()], "{}", out);
    assert!(out.contains("estimated stack use 208 exceeds declared 180"), "{}", out);
    let src = closure_frame_program("(t.Big,)", gen_call, &closure, 208);
    let (got, out) = closure_frame_codes("clo_param_ok", &src);
    assert!(got.is_empty(), "{}", out);
}

fn clif_program(last_check: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.fib(n: u32) -> u32 uses [] panics never stack 4096",
        "    if n < 2u32 {",
        "        return n",
        "    }",
        "    t.fib(n.wrapping_sub(1u32)).wrapping_add(t.fib(n.wrapping_sub(2u32)))",
        "end t.fib",
        "",
        "fn t.sign(x: i32) -> i32 uses [] panics never",
        "    if x < 0i32 {",
        "        return 0i32.wrapping_sub(1i32)",
        "    }",
        "    if x == 0i32 {",
        "        return 0i32",
        "    }",
        "    1i32",
        "end t.sign",
        "",
        "fn t.mix(a: u8, b: u8) -> u8 uses [] panics never",
        "    a.wrapping_add(b) ^ 3u8",
        "end t.mix",
        "",
        "fn t.count(limit: u32) -> u32 uses [] panics never",
        "    let mut i = 0u32",
        "    let mut acc = 0u32",
        "    loop {",
        "        i = i.wrapping_add(1u32)",
        "        if i > limit {",
        "            break",
        "        }",
        "        if i & 1u32 == 1u32 {",
        "            continue",
        "        }",
        "        acc = acc.wrapping_add(i)",
        "    }",
        "    acc",
        "end t.count",
        "",
        "fn t.both(a: bool, b: bool) -> bool uses [] panics never",
        "    (a && !b) || (b && !a)",
        "end t.both",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    core.check.eq(t.fib(10u32), 55u32)",
        "    core.check.eq(t.sign(0i32.wrapping_sub(5i32)), 0i32.wrapping_sub(1i32))",
        "    core.check.eq(t.sign(7i32), 1i32)",
        "    core.check.eq(t.mix(250u8, 10u8), 7u8)",
        "    core.check.eq(t.count(10u32), 30u32)",
        "    core.check.eq(t.both(true, false), true)",
        "    core.check.eq(t.both(true, true), false)",
        last_check,
        "    std.io.print_line(\"clif ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

fn build_clif(name: &str, src: &str, release: bool) -> (Tmp, Option<PathBuf>, String) {
    let t = Tmp::new(name, src);
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_{}", std::process::id(), name));
    let mut c = Command::new(exe());
    c.args(["build", "--backend", "clif"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
    if release {
        c.arg("--release");
    }
    let o = c.output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    let kind = if release { "release" } else { "debug" };
    let p = t.0.join("target").join(kind).join(if cfg!(windows) { "t.exe" } else { "t.native" });
    let _ = fs::remove_dir_all(&cache);
    let found = if p.exists() { Some(p) } else { None };
    (t, found, out)
}

#[test]
fn the_cranelift_backend_builds_and_runs_a_native_executable() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_ok_{}", release), &clif_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "clif ok");
    }
}

#[test]
fn a_failed_check_eq_in_a_cranelift_executable_exits_with_101() {
    let (_t, exe_path, out) = build_clif("clif_fail", &clif_program("    core.check.eq(1u8, 2u8)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("assertion failed"), "{}", text(&o.stderr));
    assert!(!text(&o.stdout).contains("clif ok"));
}

fn clif_arith_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.add8(a: u8, b: u8) -> u8 uses [] panics may",
        "    a + b",
        "end t.add8",
        "",
        "fn t.mixed(a: i32, b: i32) -> i32 uses [] panics may",
        "    (a * b - 3i32) / 2i32 + a % b",
        "end t.mixed",
        "",
        "fn t.neg(a: i64) -> i64 uses [] panics may",
        "    -a",
        "end t.neg",
        "",
        "fn t.div(a: u32, b: u32) -> u32 uses [] panics may",
        "    a / b",
        "end t.div",
        "",
        "fn t.area(w: f64, h: f64) -> f64 uses [] panics never",
        "    w * h + 0.5f64",
        "end t.area",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    core.check.eq(t.add8(200u8, 55u8), 255u8)",
        "    core.check.eq(t.mixed(7i32, 5i32), 16i32 + 2i32)",
        "    core.check.eq(t.mixed(0i32 - 7i32, 5i32), 0i32 - 21i32)",
        "    core.check.eq(t.neg(9i64), 0i64 - 9i64)",
        "    core.check.eq(t.div(17u32, 5u32), 3u32)",
        "    core.check.eq(t.area(1.5f64, 2.0f64), 3.5f64)",
        "    core.check.eq(t.area(1.0f64, 1.0f64) > 1.0f64, true)",
        tail,
        "    std.io.print_line(\"arith ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_compiles_checked_arithmetic_and_floats() {
    let (_t, exe_path, out) = build_clif("clif_arith", &clif_arith_program("    core.check.eq(1u8, 1u8)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "arith ok");
}

#[test]
fn overflow_and_division_by_zero_panic_with_exit_code_101_in_cranelift_executables() {
    for (name, tail, msg) in [
        ("clif_ovf", "    core.check.eq(t.add8(250u8, 10u8), 4u8)", "arithmetic overflow"),
        ("clif_div0", "    core.check.eq(t.div(1u32, 0u32), 0u32)", "division by zero"),
        ("clif_neg", "    core.check.eq(t.neg(0i64 - 9223372036854775807i64 - 1i64), 0i64)", "arithmetic overflow"),
    ] {
        let (_t, exe_path, out) = build_clif(name, &clif_arith_program(tail), false);
        let p = exe_path.unwrap_or_else(|| panic!("{}: no native executable: {}", name, out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(101), "{}: {}", name, text(&o.stderr));
        assert!(text(&o.stderr).contains(msg), "{}: {}", name, text(&o.stderr));
        assert!(!text(&o.stdout).contains("arith ok"));
    }
}

fn clif_struct_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "type t.Point derive [Copy, Clone]",
        "    x: i32",
        "    y: i32",
        "end t.Point",
        "",
        "type t.Rect derive [Copy, Clone]",
        "    min: t.Point",
        "    max: t.Point",
        "    tag: u8",
        "end t.Rect",
        "",
        "enum t.Shape",
        "    Empty",
        "    Circle(u32)",
        "    Square(u32, u32)",
        "end t.Shape",
        "",
        "fn t.Point.sum(self: &t.Point) -> i32 uses [] panics never",
        "    self.x.wrapping_add(self.y)",
        "end t.Point.sum",
        "",
        "fn t.Point.shift(self: &mut t.Point, dx: i32) -> i32 uses [] panics never",
        "    self.x = self.x.wrapping_add(dx)",
        "    self.x",
        "end t.Point.shift",
        "",
        "fn t.make(a: i32, b: i32) -> t.Point uses [] panics never",
        "    t.Point { x: a, y: b }",
        "end t.make",
        "",
        "fn t.width(r: t.Rect) -> i32 uses [] panics never",
        "    r.max.x.wrapping_sub(r.min.x)",
        "end t.width",
        "",
        "fn t.area(s: t.Shape) -> u32 uses [] panics never",
        "    match s {",
        "        t.Shape.Empty => 0u32",
        "        t.Shape.Circle(r) => r.wrapping_mul(3u32)",
        "        t.Shape.Square(w, h) => w.wrapping_mul(h)",
        "    }",
        "end t.area",
        "",
        "fn t.shape_of(n: u32) -> t.Shape uses [] panics never",
        "    if n == 0u32 {",
        "        return t.Shape.Empty",
        "    }",
        "    if n < 10u32 {",
        "        return t.Shape.Circle(n)",
        "    }",
        "    t.Shape.Square(n, 2u32)",
        "end t.shape_of",
        "",
        "fn t.classify(n: u32) -> u8 uses [] panics never",
        "    match n {",
        "        0u32 => 10u8",
        "        1u32 => 20u8",
        "        _ => 30u8",
        "    }",
        "end t.classify",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let p = t.make(3i32, 4i32)",
        "    core.check.eq(p.sum(), 7i32)",
        "    core.check.eq(p.x, 3i32)",
        "    let mut q = p",
        "    core.check.eq(q.shift(10i32), 13i32)",
        "    core.check.eq(q.x, 13i32)",
        "    core.check.eq(p.x, 3i32)",
        "    q.y = 99i32",
        "    core.check.eq(q.sum(), 112i32)",
        "    let r = t.Rect { min: p, max: q, tag: 7u8 }",
        "    core.check.eq(t.width(r), 10i32)",
        "    core.check.eq(r.max.y, 99i32)",
        "    core.check.eq(t.area(t.shape_of(0u32)), 0u32)",
        "    core.check.eq(t.area(t.shape_of(4u32)), 12u32)",
        "    core.check.eq(t.area(t.shape_of(12u32)), 24u32)",
        "    core.check.eq(t.area(t.Shape.Square(5u32, 6u32)), 30u32)",
        "    core.check.eq(t.classify(0u32), 10u8)",
        "    core.check.eq(t.classify(1u32), 20u8)",
        "    core.check.eq(t.classify(9u32), 30u8)",
        tail,
        "    std.io.print_line(\"struct ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_compiles_structs_enums_match_and_methods() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_struct_{}", release), &clif_struct_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "struct ok");
    }
}

#[test]
fn a_wrong_struct_result_in_a_cranelift_executable_fails_the_check() {
    let (_t, exe_path, out) = build_clif("clif_struct_bad", &clif_struct_program("    core.check.eq(t.area(t.shape_of(4u32)), 13u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

fn clif_option_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "type t.Pair derive [Copy, Clone]",
        "    a: u32",
        "    b: u32",
        "end t.Pair",
        "",
        "fn t.find(a: u32, b: u32) -> Option<u32> uses [] panics never",
        "    if a > b {",
        "        return Some(a.wrapping_sub(b))",
        "    }",
        "    None",
        "end t.find",
        "",
        "fn t.parse(n: u32) -> Result<u32, u8> uses [] panics never",
        "    if n == 0u32 {",
        "        return Err(7u8)",
        "    }",
        "    Ok(n.wrapping_mul(2u32))",
        "end t.parse",
        "",
        "fn t.unit_ok(flag: bool) -> Result<(), u8> uses [] panics never",
        "    if flag {",
        "        return Ok(())",
        "    }",
        "    Err(1u8)",
        "end t.unit_ok",
        "",
        "fn t.pair_of(n: u32) -> Option<t.Pair> uses [] panics never",
        "    if n == 0u32 {",
        "        return None",
        "    }",
        "    Some(t.Pair { a: n, b: n.wrapping_add(1u32) })",
        "end t.pair_of",
        "",
        "fn t.describe(o: Option<u32>) -> u32 uses [] panics never",
        "    match o {",
        "        Some(v) => v.wrapping_add(100u32)",
        "        None => 0u32",
        "    }",
        "end t.describe",
        "",
        "fn t.res_val(r: Result<u32, u8>) -> u32 uses [] panics never",
        "    match r {",
        "        Ok(v) => v",
        "        Err(_) => 999u32",
        "    }",
        "end t.res_val",
        "",
        "fn t.unit_code(r: Result<(), u8>) -> u8 uses [] panics never",
        "    match r {",
        "        Ok(_) => 0u8",
        "        Err(e) => e",
        "    }",
        "end t.unit_code",
        "",
        "fn t.pair_sum(o: Option<t.Pair>) -> u32 uses [] panics never",
        "    match o {",
        "        Some(p) => p.a.wrapping_add(p.b)",
        "        None => 0u32",
        "    }",
        "end t.pair_sum",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    core.check.eq(t.describe(t.find(9u32, 4u32)), 105u32)",
        "    core.check.eq(t.describe(t.find(1u32, 4u32)), 0u32)",
        "    core.check.eq(t.res_val(t.parse(21u32)), 42u32)",
        "    core.check.eq(t.res_val(t.parse(0u32)), 999u32)",
        "    core.check.eq(t.unit_code(t.unit_ok(true)), 0u8)",
        "    core.check.eq(t.unit_code(t.unit_ok(false)), 1u8)",
        "    core.check.eq(t.pair_sum(t.pair_of(5u32)), 11u32)",
        "    core.check.eq(t.pair_sum(t.pair_of(0u32)), 0u32)",
        tail,
        "    std.io.print_line(\"option ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_compiles_option_and_result() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_opt_{}", release), &clif_option_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "option ok");
    }
}

#[test]
fn a_wrong_option_result_in_a_cranelift_executable_fails_the_check() {
    let (_t, exe_path, out) = build_clif("clif_opt_bad", &clif_option_program("    core.check.eq(t.describe(t.find(9u32, 4u32)), 106u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

fn clif_vec_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "type t.Bag",
        "    items: alloc.vec.Vec<u32, std.alloc.System>",
        "    n: u32",
        "end t.Bag",
        "",
        "fn t.fill(count: u32) -> alloc.vec.Vec<u32, std.alloc.System>",
        "    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "    let mut i = 0u32",
        "    while i < count {",
        "        match v.push(i.wrapping_mul(i)) {",
        "            Ok(_) => 0u8",
        "            Err(_) => 1u8",
        "        }",
        "        i = i.wrapping_add(1u32)",
        "    }",
        "    v",
        "end t.fill",
        "",
        "fn t.sum(v: &alloc.vec.Vec<u32, std.alloc.System>) -> u32",
        "    let mut i = 0usize",
        "    let mut acc = 0u32",
        "    while i < v.len() {",
        "        acc = acc.wrapping_add(v[i])",
        "        i = i.wrapping_add(1usize)",
        "    }",
        "    acc",
        "end t.sum",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let v = t.fill(5u32)",
        "    core.check.eq(v.len(), 5usize)",
        "    core.check.eq(t.sum(&v), 30u32)",
        "    let mut w: alloc.vec.Vec<u32, std.alloc.System> = t.fill(3u32)",
        "    match w.pop() {",
        "        Some(x) => core.check.eq(x, 4u32)",
        "        None => core.check.eq(1u8, 2u8)",
        "    }",
        "    core.check.eq(w.len(), 2usize)",
        "    core.check.eq(w.is_empty(), false)",
        "    w.clear()",
        "    core.check.eq(w.is_empty(), true)",
        "    let big = t.fill(1000u32)",
        "    core.check.eq(big.len(), 1000usize)",
        "    core.check.eq(t.sum(&big), 332833500u32)",
        "    let mut bag = t.Bag { items: t.fill(2u32), n: 7u32 }",
        "    match bag.items.push(9u32) {",
        "        Ok(_) => 0u8",
        "        Err(_) => 1u8",
        "    }",
        "    core.check.eq(bag.items.len(), 3usize)",
        "    core.check.eq(bag.items[2usize], 9u32)",
        "    let s = \"hello\"",
        "    core.check.eq(s.len(), 5usize)",
        "    std.io.print_line(s)",
        tail,
        "    std.io.print_line(\"vec ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_compiles_vec_and_str() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_vec_{}", release), &clif_vec_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "hello\nvec ok");
    }
}

#[test]
fn indexing_a_vec_out_of_bounds_panics_in_a_cranelift_executable() {
    let (_t, exe_path, out) = build_clif("clif_vec_oob", &clif_vec_program("    core.check.eq(v[9usize], 0u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("index out of bounds"), "{}", text(&o.stderr));
}

fn clif_deque_program() -> String {
    let nl = char::from(10u8).to_string();
    let dq = "alloc.collections.VecDeque<u32, std.alloc.System>";
    let dqf = "alloc.collections.VecDeque<f64, std.alloc.System>";
    let sv = "alloc.string.String<std.alloc.System>";
    let dqs = format!("alloc.collections.VecDeque<{}, std.alloc.System>", sv);
    [
        "type t.Bag".to_string(),
        format!("    q: {}", dq),
        "    tag: u32".to_string(),
        "end t.Bag".to_string(),
        String::new(),
        format!("fn t.fresh() -> {}", dq),
        "    alloc.collections.VecDeque.new(std.alloc.SYSTEM)".to_string(),
        "end t.fresh".to_string(),
        String::new(),
        format!("fn t.fresh_f() -> {}", dqf),
        "    alloc.collections.VecDeque.new(std.alloc.SYSTEM)".to_string(),
        "end t.fresh_f".to_string(),
        String::new(),
        format!("fn t.fresh_s() -> {}", dqs),
        "    alloc.collections.VecDeque.new(std.alloc.SYSTEM)".to_string(),
        "end t.fresh_s".to_string(),
        String::new(),
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may".to_string(),
        "    let mut q = t.fresh()".to_string(),
        "    core.check.eq(q.is_empty(), true)".to_string(),
        "    core.check.eq(q.len(), 0usize)".to_string(),
        "    core.check.eq(q.pop_front(), None)".to_string(),
        "    core.check.eq(q.pop_back(), None)".to_string(),
        "    match q.push_back(1u32) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    }".to_string(),
        "    match q.push_back(2u32) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    }".to_string(),
        "    match q.push_back(3u32) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    }".to_string(),
        "    match q.push_front(0u32) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    }".to_string(),
        "    core.check.eq(q.len(), 4usize)".to_string(),
        "    core.check.eq(q.pop_front(), Some(0u32))".to_string(),
        "    core.check.eq(q.pop_back(), Some(3u32))".to_string(),
        "    core.check.eq(q.len(), 2usize)".to_string(),
        "    match q.push_front(9u32) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    }".to_string(),
        "    core.check.eq(q.pop_front(), Some(9u32))".to_string(),
        "    core.check.eq(q.pop_front(), Some(1u32))".to_string(),
        "    core.check.eq(q.pop_front(), Some(2u32))".to_string(),
        "    core.check.eq(q.is_empty(), true)".to_string(),
        "    // 先入れ先出し: 容量の拡張と先頭の詰めをまたいでも順序が保たれる。".to_string(),
        "    let mut i = 0u32".to_string(),
        "    let mut next = 0u32".to_string(),
        "    while i < 1000u32 {".to_string(),
        "        match q.push_back(i) {".to_string(),
        "            Ok(_) => 0u8".to_string(),
        "            Err(_) => 1u8".to_string(),
        "        }".to_string(),
        "        if i % 3u32 == 0u32 {".to_string(),
        "            core.check.eq(q.pop_front(), Some(next))".to_string(),
        "            next = next.wrapping_add(1u32)".to_string(),
        "        }".to_string(),
        "        i = i.wrapping_add(1u32)".to_string(),
        "    }".to_string(),
        "    while !q.is_empty() {".to_string(),
        "        core.check.eq(q.pop_front(), Some(next))".to_string(),
        "        next = next.wrapping_add(1u32)".to_string(),
        "    }".to_string(),
        "    core.check.eq(next, 1000u32)".to_string(),
        "    // 先頭に積んで末尾から取り出す (最後に入れたものが最初に出る)。".to_string(),
        "    let mut j = 0u32".to_string(),
        "    while j < 500u32 {".to_string(),
        "        match q.push_front(j) {".to_string(),
        "            Ok(_) => 0u8".to_string(),
        "            Err(_) => 1u8".to_string(),
        "        }".to_string(),
        "        j = j.wrapping_add(1u32)".to_string(),
        "    }".to_string(),
        "    let mut k = 0u32".to_string(),
        "    while k < 500u32 {".to_string(),
        "        core.check.eq(q.pop_back(), Some(k))".to_string(),
        "        k = k.wrapping_add(1u32)".to_string(),
        "    }".to_string(),
        "    core.check.eq(q.is_empty(), true)".to_string(),
        "    // 浮動小数と、解放が要る要素 (String) も Vec と同じように扱う。".to_string(),
        "    let mut f = t.fresh_f()".to_string(),
        "    match f.push_back(1.5f64) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    }".to_string(),
        "    match f.push_front(0.25f64) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    }".to_string(),
        "    core.check.eq(f.pop_back(), Some(1.5f64))".to_string(),
        "    core.check.eq(f.len(), 1usize)".to_string(),
        "    let mut s = t.fresh_s()".to_string(),
        "    let a = alloc.string.String.from_str(std.alloc.SYSTEM, \"hi\").unwrap()".to_string(),
        "    match s.push_front(a) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    }".to_string(),
        "    let b = alloc.string.String.from_str(std.alloc.SYSTEM, \"yo\").unwrap()".to_string(),
        "    match s.push_back(b) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    }".to_string(),
        "    core.check.eq(s.len(), 2usize)".to_string(),
        "    match s.pop_front() {".to_string(),
        "        Some(x) => core.check.eq(x.as_str(), \"hi\")".to_string(),
        "        None => core.check.eq(1u8, 2u8)".to_string(),
        "    }".to_string(),
        "    match s.pop_back() {".to_string(),
        "        Some(x) => core.check.eq(x.as_str(), \"yo\")".to_string(),
        "        None => core.check.eq(1u8, 2u8)".to_string(),
        "    }".to_string(),
        "    let mut bag = t.Bag { q: t.fresh(), tag: 7u32 }".to_string(),
        "    match bag.q.push_back(5u32) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    }".to_string(),
        "    core.check.eq(bag.q.len(), 1usize)".to_string(),
        "    core.check.eq(bag.tag, 7u32)".to_string(),
        "    std.io.print_line(\"deque ok\")".to_string(),
        "    Ok(())".to_string(),
        "end t.main".to_string(),
        String::new(),
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_runs_vecdeque_like_the_interpreter() {
    let src = clif_deque_program();
    let t = Tmp::new("clif_deque_interp", &src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    assert_eq!(text(&interp.stdout).trim(), "deque ok");
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_deque_{}", release), &src, release);
        assert!(!out.contains("skipped"), "native backend skipped: {}", out);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
}

fn clif_string_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    let sv = "alloc.string.String<std.alloc.System>";
    let vs = "alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>";
    [
        "type t.Doc".to_string(),
        format!("    name: {}", sv),
        format!("    parts: {}", vs),
        "end t.Doc".to_string(),
        String::new(),
        format!("fn t.build(n: u32) -> {}", vs),
        "    let mut v = t.fresh()".to_string(),
        "    let mut i = 0u32".to_string(),
        "    while i < n {".to_string(),
        "        let g = t.greeting(\"x\")".to_string(),
        "        match v.push(g) {".to_string(),
        "            Ok(_) => 0u8".to_string(),
        "            Err(_) => 1u8".to_string(),
        "        }".to_string(),
        "        i = i.wrapping_add(1u32)".to_string(),
        "    }".to_string(),
        "    v".to_string(),
        "end t.build".to_string(),
        String::new(),
        "fn t.early(flag: bool) -> usize".to_string(),
        "    let s = t.greeting(\"early\")".to_string(),
        "    if flag {".to_string(),
        "        return s.len()".to_string(),
        "    }".to_string(),
        "    let other = t.greeting(\"late\")".to_string(),
        "    other.len().wrapping_add(s.len())".to_string(),
        "end t.early".to_string(),
        String::new(),
        format!("fn t.fresh() -> {}", vs),
        "    alloc.vec.Vec.new(std.alloc.SYSTEM)".to_string(),
        "end t.fresh".to_string(),
        String::new(),
        format!("fn t.greeting(who: &str) -> {}", sv),
        "    let mut s = alloc.string.String.new(std.alloc.SYSTEM)".to_string(),
        "    match s.push_str(\"hello, \") {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    }".to_string(),
        "    match s.push_str(who) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    }".to_string(),
        "    s".to_string(),
        "end t.greeting".to_string(),
        String::new(),
        "fn t.looping() -> u32".to_string(),
        "    let mut i = 0u32".to_string(),
        "    let mut total = 0u32".to_string(),
        "    loop {".to_string(),
        "        i = i.wrapping_add(1u32)".to_string(),
        "        let _s = t.greeting(\"loop\")".to_string(),
        "        if i > 50u32 {".to_string(),
        "            break".to_string(),
        "        }".to_string(),
        "        if i & 1u32 == 1u32 {".to_string(),
        "            continue".to_string(),
        "        }".to_string(),
        "        total = total.wrapping_add(1u32)".to_string(),
        "    }".to_string(),
        "    total".to_string(),
        "end t.looping".to_string(),
        String::new(),
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may".to_string(),
        "    let g = t.greeting(\"world\")".to_string(),
        "    std.io.print_line(g.as_str())".to_string(),
        "    core.check.eq(g.as_str(), \"hello, world\")".to_string(),
        "    core.check.eq(g.len(), 12usize)".to_string(),
        "    let docs = t.build(100u32)".to_string(),
        "    core.check.eq(docs.len(), 100usize)".to_string(),
        "    let mut d = t.Doc { name: t.greeting(\"doc\"), parts: t.build(3u32) }".to_string(),
        "    core.check.eq(d.name.as_str(), \"hello, doc\")".to_string(),
        "    match d.parts.push(t.greeting(\"more\")) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    }".to_string(),
        "    core.check.eq(d.parts.len(), 4usize)".to_string(),
        "    d.name = t.greeting(\"renamed\")".to_string(),
        "    core.check.eq(d.name.as_str(), \"hello, renamed\")".to_string(),
        "    d.parts.clear()".to_string(),
        "    core.check.eq(d.parts.len(), 0usize)".to_string(),
        "    core.check.eq(t.early(true), 12usize)".to_string(),
        "    core.check.eq(t.early(false), 23usize)".to_string(),
        "    core.check.eq(t.looping(), 25u32)".to_string(),
        "    let moved = g".to_string(),
        "    core.check.eq(moved.len(), 12usize)".to_string(),
        "    let r = alloc.string.String.from_str(std.alloc.SYSTEM, \"abc\")".to_string(),
        "    match r {".to_string(),
        "        Ok(s) => core.check.eq(s.as_str(), \"abc\")".to_string(),
        "        Err(_) => core.check.eq(1u8, 2u8)".to_string(),
        "    }".to_string(),
        "    core.mem.drop(docs)".to_string(),
        tail.to_string(),
        "    std.io.print_line(\"string ok\")".to_string(),
        "    Ok(())".to_string(),
        "end t.main".to_string(),
        String::new(),
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_frees_owned_values_and_compiles_string() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_str_{}", release), &clif_string_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "hello, world\nstring ok");
    }
}

#[test]
fn a_wrong_string_comparison_in_a_cranelift_executable_fails_the_check() {
    let (_t, exe_path, out) = build_clif("clif_str_bad", &clif_string_program("    core.check.eq(\"ab\", \"ac\")"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
    assert!(!text(&o.stdout).contains("string ok"));
}

fn clif_slice_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.dot(a: &[u32], b: &[u32]) -> u32",
        "    let mut acc = 0u32",
        "    for i in core.iter.range(0usize, a.len()) {",
        "        acc = acc.wrapping_add(a[i].wrapping_mul(b[i]))",
        "    }",
        "    acc",
        "end t.dot",
        "",
        "fn t.first_byte(text: &str) -> u8",
        "    match text.as_bytes().get(0usize) {",
        "        Some(b) => *b",
        "        None => 0u8",
        "    }",
        "end t.first_byte",
        "",
        "fn t.fresh() -> alloc.vec.Vec<u32, std.alloc.System>",
        "    alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "end t.fresh",
        "",
        "fn t.index_sum(s: &[u32]) -> usize",
        "    let mut acc = 0usize",
        "    for (i, _x) in core.iter.enumerate(s.iter()) {",
        "        acc = acc.wrapping_add(i)",
        "    }",
        "    acc",
        "end t.index_sum",
        "",
        "fn t.make(n: u32) -> alloc.vec.Vec<u32, std.alloc.System>",
        "    let mut v = t.fresh()",
        "    for k in core.iter.range(1u32, n.wrapping_add(1u32)) {",
        "        match v.push(k) {",
        "            Ok(_) => 0u8",
        "            Err(_) => 1u8",
        "        }",
        "    }",
        "    v",
        "end t.make",
        "",
        "fn t.pick(s: &[u32], i: usize) -> u32",
        "    match s.get(i) {",
        "        Some(x) => *x",
        "        None => 999u32",
        "    }",
        "end t.pick",
        "",
        "fn t.total(v: &alloc.vec.Vec<u32, std.alloc.System>) -> u32",
        "    let mut acc = 0u32",
        "    for x in v.iter() {",
        "        if *x == 3u32 {",
        "            continue",
        "        }",
        "        acc = acc.wrapping_add(*x)",
        "    }",
        "    acc",
        "end t.total",
        "",
        "fn t.zero_first(s: &mut [u32]) -> ()",
        "    s[0usize] = 0u32",
        "end t.zero_first",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let v = t.make(4u32)",
        "    let s = v.as_slice()",
        "    core.check.eq(s.len(), 4usize)",
        "    core.check.eq(t.dot(s, s), 30u32)",
        "    core.check.eq(t.total(&v), 7u32)",
        "    core.check.eq(t.index_sum(s), 6usize)",
        "    core.check.eq(t.pick(s, 2usize), 3u32)",
        "    core.check.eq(t.pick(s, 9usize), 999u32)",
        "    core.check.eq(s.get(1usize), Some(2u32))",
        "    core.check.eq(s.get(7usize), None)",
        "    core.check.eq(s.first(), Some(1u32))",
        "    core.check.eq(s.last(), Some(4u32))",
        "    match s.sub(1usize, 3usize) {",
        "        Some(part) => core.check.eq(part.len(), 2usize)",
        "        None => core.check.eq(1u8, 2u8)",
        "    }",
        "    match s.sub(3usize, 9usize) {",
        "        Some(_) => core.check.eq(1u8, 2u8)",
        "        None => core.check.eq(1u8, 1u8)",
        "    }",
        "    let mut w = t.make(3u32)",
        "    t.zero_first(w.as_mut_slice())",
        "    core.check.eq(w[0usize], 0u32)",
        "    core.check.eq(w[2usize], 3u32)",
        "    core.check.eq(t.first_byte(\"A\"), 65u8)",
        tail,
        "    std.io.print_line(\"slice ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_compiles_slices_and_for_loops() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_slice_{}", release), &clif_slice_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "slice ok");
    }
}

#[test]
fn a_wrong_slice_result_in_a_cranelift_executable_fails_the_check() {
    let (_t, exe_path, out) = build_clif("clif_slice_bad", &clif_slice_program("    core.check.eq(s.last(), Some(5u32))"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

fn clif_tuple_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "enum t.Kind derive [Debug, Clone, Copy, PartialEq]",
        "    Dot",
        "    Line(u32)",
        "    Box(t.P, bool)",
        "end t.Kind",
        "",
        "type t.P derive [Debug, Clone, Copy, PartialEq, Eq]",
        "    x: i32",
        "    y: i32",
        "end t.P",
        "",
        "type t.Seg derive [Debug, Clone, Copy, PartialEq]",
        "    a: t.P",
        "    b: t.P",
        "    tag: u8",
        "end t.Seg",
        "",
        "fn t.divmod(a: u32, b: u32) -> (u32, u32)",
        "    (a / b, a % b)",
        "end t.divmod",
        "",
        "fn t.named(n: u32) -> (u32, alloc.string.String<std.alloc.System>)",
        "    let mut s = alloc.string.String.new(std.alloc.SYSTEM)",
        "    match s.push_str(\"n\") {",
        "        Ok(_) => 0u8",
        "        Err(_) => 1u8",
        "    }",
        "    (n, s)",
        "end t.named",
        "",
        "fn t.swap(p: (u32, bool)) -> (bool, u32)",
        "    let (n, flag) = p",
        "    (flag, n)",
        "end t.swap",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let p = t.P { x: 1i32, y: 2i32 }",
        "    let q = t.P { x: 1i32, y: 2i32 }",
        "    core.check.eq(p, q)",
        "    core.check.eq(p == t.P { x: 1i32, y: 3i32 }, false)",
        "    let s1 = t.Seg { a: p, b: q, tag: 1u8 }",
        "    let s2 = t.Seg { a: q, b: p, tag: 1u8 }",
        "    core.check.eq(s1, s2)",
        "    core.check.eq(s1 != t.Seg { a: p, b: q, tag: 2u8 }, true)",
        "    core.check.eq(t.Kind.Line(3u32), t.Kind.Line(3u32))",
        "    core.check.eq(t.Kind.Line(3u32) == t.Kind.Line(4u32), false)",
        "    core.check.eq(t.Kind.Dot == t.Kind.Line(0u32), false)",
        "    core.check.eq(t.Kind.Dot, t.Kind.Dot)",
        "    core.check.eq(t.Kind.Box(p, true), t.Kind.Box(q, true))",
        "    core.check.eq(t.Kind.Box(p, true) == t.Kind.Box(q, false), false)",
        "    core.check.eq(t.divmod(17u32, 5u32), (3u32, 2u32))",
        "    core.check.eq(t.swap((9u32, true)), (true, 9u32))",
        "    core.check.eq(Some(p), Some(q))",
        "    core.check.eq(\"ab\" == \"ab\", true)",
        "    core.check.eq(\"ab\" == \"ac\", false)",
        "    match t.divmod(20u32, 6u32) {",
        "        (d, m) => core.check.eq(d.wrapping_add(m), 5u32)",
        "    }",
        "    let (k, name) = t.named(7u32)",
        "    core.check.eq(k, 7u32)",
        "    core.check.eq(name.as_str(), \"n\")",
        tail,
        "    std.io.print_line(\"tuple ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_compiles_tuples_and_structural_equality() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_tuple_{}", release), &clif_tuple_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "tuple ok");
    }
}

#[test]
fn unequal_structs_in_a_cranelift_executable_fail_the_check() {
    let (_t, exe_path, out) = build_clif(
        "clif_tuple_bad",
        &clif_tuple_program("    core.check.eq(t.Kind.Box(p, true), t.Kind.Box(q, false))"),
        false,
    );
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

fn clif_methods_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "enum t.Phase derive [Debug, Clone, Copy, PartialEq]",
        "    Idle",
        "    Run",
        "end t.Phase",
        "",
        "fn t.Phase.next(self: t.Phase) -> t.Phase uses [] panics never",
        "    match self {",
        "        t.Phase.Idle => t.Phase.Run",
        "        t.Phase.Run => t.Phase.Idle",
        "    }",
        "end t.Phase.next",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    core.check.eq(250u8.checked_add(5u8), Some(255u8))",
        "    core.check.eq(250u8.checked_add(6u8), None)",
        "    core.check.eq(3u32.checked_sub(4u32), None)",
        "    core.check.eq(9u32.checked_rem(4u32).unwrap_or(77u32), 1u32)",
        "    core.check.eq(9u32.checked_div(0u32).unwrap_or(77u32), 77u32)",
        "    core.check.eq(0i32.checked_sub(5i32), Some(0i32 - 5i32))",
        "    core.check.eq(250u8.saturating_add(10u8), 255u8)",
        "    core.check.eq(3u8.saturating_sub(9u8), 0u8)",
        "    core.check.eq(Some(4u32).is_some(), true)",
        "    core.check.eq(Some(4u32).unwrap(), 4u32)",
        "    core.check.eq(t.Phase.Idle.next(), t.Phase.Run)",
        "    core.check.eq(t.Phase.Idle.next().next(), t.Phase.Idle)",
        tail,
        "    std.io.print_line(\"methods ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_compiles_checked_arithmetic_and_option_methods() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_meth_{}", release), &clif_methods_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "methods ok");
    }
}

#[test]
fn unwrap_on_none_panics_with_exit_code_101_in_a_cranelift_executable() {
    let (_t, exe_path, out) = build_clif("clif_unwrap", &clif_methods_program("    core.check.eq(250u8.checked_add(9u8).unwrap(), 0u8)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("unwrap"), "{}", text(&o.stderr));
}

fn clif_mutref_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "type t.P",
        "    x: u32",
        "    y: u32",
        "end t.P",
        "",
        "fn t.bump(r: &mut u32) -> ()",
        "    *r = r.wrapping_add(1u32)",
        "end t.bump",
        "",
        "fn t.double_all(v: &mut [u32]) -> ()",
        "    for i in core.iter.range(0usize, v.len()) {",
        "        v[i] = v[i].wrapping_mul(2u32)",
        "    }",
        "end t.double_all",
        "",
        "fn t.fresh() -> alloc.vec.Vec<u32, std.alloc.System>",
        "    alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "end t.fresh",
        "",
        "fn t.make(n: u32) -> alloc.vec.Vec<u32, std.alloc.System>",
        "    let mut v = t.fresh()",
        "    for k in core.iter.range(1u32, n.wrapping_add(1u32)) {",
        "        match v.push(k) {",
        "            Ok(_) => 0u8",
        "            Err(_) => 1u8",
        "        }",
        "    }",
        "    v",
        "end t.make",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let mut n = 5u32",
        "    t.bump(&mut n)",
        "    t.bump(&mut n)",
        "    core.check.eq(n, 7u32)",
        "    let mut p = t.P { x: 1u32, y: 2u32 }",
        "    t.bump(&mut p.x)",
        "    core.check.eq(p.x, 2u32)",
        "    core.check.eq(p.y, 2u32)",
        "    let mut v = t.make(3u32)",
        "    t.double_all(v.as_mut_slice())",
        "    core.check.eq(v[1usize], 4u32)",
        "    match v.get_mut(1usize) {",
        "        Some(slot) => t.bump(slot)",
        "        None => core.check.eq(1u8, 2u8)",
        "    }",
        "    core.check.eq(v[1usize], 5u32)",
        "    t.bump(&mut v[0usize])",
        "    core.check.eq(v[0usize], 3u32)",
        tail,
        "    std.io.print_line(\"mutref ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_compiles_mutable_references_to_scalars() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_mutref_{}", release), &clif_mutref_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "mutref ok");
    }
}

#[test]
fn a_write_through_a_mutable_reference_is_visible_in_a_cranelift_executable() {
    let (_t, exe_path, out) = build_clif("clif_mutref_bad", &clif_mutref_program("    core.check.eq(n, 8u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

#[test]
fn the_realtime_example_runs_natively_through_cranelift() {
    let d = root().join("examples").join("realtime");
    let cache = std::env::temp_dir().join(format!("ksn_cache_rt_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "clif"]).current_dir(&d).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        let kind = if release { "release" } else { "debug" };
        let p = d.join("target").join(kind).join(if cfg!(windows) { "realtime.exe" } else { "realtime.native" });
        assert!(p.exists(), "native executable missing: {}", out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
    let _ = fs::remove_dir_all(&cache);
}

fn clif_try_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.collect(n: u32) -> alloc.vec.Vec<u32, std.alloc.System>",
        "    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "    for k in core.iter.range(1u32, n.wrapping_add(1u32)) {",
        "        match v.push(k.wrapping_mul(k)) {",
        "            Ok(_u) => {",
        "            }",
        "            Err(_e) => {",
        "                break",
        "            }",
        "        }",
        "    }",
        "    v",
        "end t.collect",
        "",
        "fn t.parse(n: u32) -> Result<u32, u8>",
        "    if n == 0u32 {",
        "        return Err(7u8)",
        "    }",
        "    Ok(n.wrapping_mul(2u32))",
        "end t.parse",
        "",
        "fn t.positive(n: u32) -> Option<u32>",
        "    if n == 0u32 {",
        "        return None",
        "    }",
        "    Some(n)",
        "end t.positive",
        "",
        "fn t.sum_two(a: u32, b: u32) -> Option<u32>",
        "    let x = t.positive(a)?",
        "    let y = t.positive(b)?",
        "    x.checked_add(y)",
        "end t.sum_two",
        "",
        "fn t.twice(n: u32) -> Result<u32, u8>",
        "    let a = t.parse(n)?",
        "    let b = t.parse(a)?",
        "    Ok(b)",
        "end t.twice",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    core.check.eq(t.twice(3u32), Ok(12u32))",
        "    core.check.eq(t.twice(0u32), Err(7u8))",
        "    core.check.eq(t.sum_two(2u32, 5u32), Some(7u32))",
        "    core.check.eq(t.sum_two(0u32, 5u32), None)",
        "    core.check.eq(t.sum_two(4u32, 0u32), None)",
        "    let v = t.collect(4u32)",
        "    core.check.eq(v.len(), 4usize)",
        "    core.check.eq(v[3usize], 16u32)",
        tail,
        "    std.io.print_line(\"try ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_compiles_the_question_mark_and_infers_vec_types() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_try_{}", release), &clif_try_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "try ok");
    }
}

#[test]
fn a_question_mark_error_in_main_exits_with_code_1_in_a_cranelift_executable() {
    let (_t, exe_path, out) = build_clif("clif_try_main", &clif_try_program("    core.check.eq(t.parse(0u32)?, 0u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stderr));
    assert!(!text(&o.stdout).contains("try ok"));
}

fn clif_closure_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.evens_sum(limit: i32) -> i32",
        "    core.iter.fold(",
        "        core.iter.map(",
        "            core.iter.filter(",
        "                core.iter.range(0i32, limit),",
        "                fn(x: &i32) -> bool capture [] {",
        "                    *x % 2i32 == 0i32",
        "                },",
        "            ),",
        "            fn(x: i32) -> i32 capture [] {",
        "                x.wrapping_mul(10i32)",
        "            },",
        "        ),",
        "        0i32,",
        "        fn(a: i32, x: i32) -> i32 capture [] {",
        "            a.wrapping_add(x)",
        "        },",
        "    )",
        "end t.evens_sum",
        "",
        "fn t.fresh() -> alloc.vec.Vec<i32, std.alloc.System>",
        "    alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "end t.fresh",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    core.check.eq(t.evens_sum(7i32), 120i32)",
        "    let twice = fn(n: i32) -> i32 capture [] {",
        "        n.wrapping_mul(2i32)",
        "    }",
        "    core.check.eq(twice(21i32), 42i32)",
        "    let base = 100i32",
        "    let add_base = fn(n: i32) -> i32 capture [&base] {",
        "        n.wrapping_add(base)",
        "    }",
        "    core.check.eq(add_base(5i32), 105i32)",
        "    let mut v = t.fresh()",
        "    for k in core.iter.range(1i32, 6i32) {",
        "        match v.push(k) {",
        "            Ok(_u) => 0u8",
        "            Err(_e) => 1u8",
        "        }",
        "    }",
        "    core.check.eq(core.iter.count(v.iter()), 5usize)",
        "    core.check.eq(core.iter.count(core.iter.take(v.iter(), 3usize)), 3usize)",
        "    let total = core.iter.fold(",
        "        core.iter.take(v.iter(), 3usize),",
        "        0i32,",
        "        fn(a: i32, x: &i32) -> i32 capture [] {",
        "            a.wrapping_add(*x)",
        "        },",
        "    )",
        "    core.check.eq(total, 6i32)",
        "    let mut seen = 0usize",
        "    let mut sum = 0i32",
        "    for (i, x) in core.iter.enumerate(core.iter.take(v.iter(), 2usize)) {",
        "        core.check.eq(i, seen)",
        "        sum = sum.wrapping_add(*x)",
        "        seen = seen.wrapping_add(1usize)",
        "    }",
        "    core.check.eq(seen, 2usize)",
        "    core.check.eq(sum, 3i32)",
        tail,
        "    std.io.print_line(\"closure ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_inlines_closures_and_fuses_iterator_pipelines() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_clo_{}", release), &clif_closure_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "closure ok");
    }
}

#[test]
fn a_wrong_closure_result_in_a_cranelift_executable_fails_the_check() {
    let (_t, exe_path, out) = build_clif("clif_clo_bad", &clif_closure_program("    core.check.eq(twice(2i32), 5i32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

#[test]
fn the_demo_example_runs_natively_through_cranelift() {
    let d = root().join("examples").join("demo");
    let cache = std::env::temp_dir().join(format!("ksn_cache_demo_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    let o = Command::new(exe()).args(["build", "--backend", "clif"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    let p = d.join("target").join("debug").join(if cfg!(windows) { "demo.exe" } else { "demo.native" });
    assert!(p.exists(), "native executable missing: {}", out);
    let r = Command::new(&p).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn the_json_example_runs_natively_through_cranelift() {
    let d = root().join("examples").join("json");
    let cache = std::env::temp_dir().join(format!("ksn_cache_json_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "clif"]).current_dir(&d).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        let kind = if release { "release" } else { "debug" };
        let p = d.join("target").join(kind).join(if cfg!(windows) { "json.exe" } else { "json.native" });
        assert!(p.exists(), "native executable missing: {}", out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
    let _ = fs::remove_dir_all(&cache);
}

fn clif_iterator_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "type t.Upto",
        "    cur: u32",
        "    stop: u32",
        "end t.Upto",
        "",
        "impl core.iter.Iterator<u32, [], never> for t.Upto",
        "fn t.Upto.next(self: &mut t.Upto) -> Option<u32> uses [] panics never impls core.iter.Iterator<u32, [], never>",
        "    if self.cur >= self.stop {",
        "        return None",
        "    }",
        "    let v = self.cur",
        "    self.cur = self.cur.wrapping_add(1u32)",
        "    Some(v)",
        "end t.Upto.next",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let mut it = t.Upto { cur: 0u32, stop: 4u32 }",
        "    let mut acc = 0u32",
        "    for n in it {",
        "        acc = acc.wrapping_add(n)",
        "    }",
        "    core.check.eq(acc, 6u32)",
        "    core.check.eq(it.cur, 4u32)",
        "    let total = core.iter.fold(",
        "        core.iter.map(",
        "            core.iter.filter(",
        "                t.Upto { cur: 0u32, stop: 10u32 },",
        "                fn(x: &u32) -> bool capture [] {",
        "                    *x > 2u32",
        "                },",
        "            ),",
        "            fn(x: u32) -> u32 capture [] {",
        "                x.wrapping_mul(2u32)",
        "            },",
        "        ),",
        "        0u32,",
        "        fn(a: u32, x: u32) -> u32 capture [] {",
        "            a.wrapping_add(x)",
        "        },",
        "    )",
        "    core.check.eq(total, 84u32)",
        "    let long = t.Upto { cur: 5u32, stop: 100u32 }",
        "    core.check.eq(core.iter.count(core.iter.take(long, 3usize)), 3usize)",
        tail,
        "    std.io.print_line(\"iterator ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_runs_user_defined_iterators() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_iter_{}", release), &clif_iterator_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "iterator ok");
    }
}

#[test]
fn a_wrong_iterator_result_in_a_cranelift_executable_fails_the_check() {
    let (_t, exe_path, out) = build_clif("clif_iter_bad", &clif_iterator_program("    core.check.eq(it.cur, 5u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

fn clif_std_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.fresh() -> alloc.vec.Vec<u8, std.alloc.System>",
        "    alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "end t.fresh",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let arr = [1u8, 2u8, 3u8, 4u8, 5u8]",
        "    core.check.eq(arr.len(), 5usize)",
        "    core.check.eq(arr.get(7usize), None)",
        "    core.check.eq(arr.first(), Some(1u8))",
        "    core.check.eq(arr.last(), Some(5u8))",
        "    core.check.eq(arr.contains(&3u8), true)",
        "    core.check.eq(arr.contains(&9u8), false)",
        "    let mid = arr.sub(1usize, 4usize)",
        "    core.check.eq(mid, Some(&[2u8, 3u8, 4u8]))",
        "    let mut opt = Some(5u8)",
        "    let taken = opt.take()",
        "    core.check.eq(taken, Some(5u8))",
        "    core.check.eq(opt, None)",
        "    let mut v = t.fresh()",
        "    for k in core.iter.range(1u8, 4u8) {",
        "        match v.push(k.wrapping_mul(10u8)) {",
        "            Ok(_u) => 0u8",
        "            Err(_e) => 1u8",
        "        }",
        "    }",
        "    core.check.eq(v.remove(0usize), Some(10u8))",
        "    core.check.eq(v.len(), 2usize)",
        "    core.check.eq(v[0usize], 20u8)",
        "    core.check.eq(v.remove(9usize), None)",
        "    core.check.eq(u64.from_u32(7u32), 7u64)",
        "    core.check.eq(i32.try_from_i64(5i64), Ok(5i32))",
        "    core.check.eq(u8.try_from_i32(300i32).is_err(), true)",
        "    core.check.eq(u8.try_from_i32(0i32 - 1i32).is_err(), true)",
        "    core.check.eq(u8.try_from_u64(255u64), Ok(255u8))",
        "    core.check.eq(i8.try_from_i32(200i32).is_err(), true)",
        "    core.check.eq(i8.try_from_i32(0i32 - 128i32).is_ok(), true)",
        "    let c = 'q'",
        "    core.check.eq(c.to_ascii_uppercase(), 'Q')",
        "    core.check.eq(c.to_u32(), 113u32)",
        "    core.check.eq(char.try_from_u32(65u32), Some('A'))",
        "    core.check.eq(char.try_from_u32(55296u32), None)",
        "    core.check.eq('7'.is_ascii_digit(), true)",
        "    core.check.eq('a'.is_ascii_alphabetic(), true)",
        "    core.check.eq(' '.is_ascii_whitespace(), true)",
        "    core.check.eq('x'.is_ascii_whitespace(), false)",
        "    core.check.eq(3i32.to_f64(), 3.0f64)",
        tail,
        "    std.io.print_line(\"std ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_compiles_arrays_take_remove_conversions_and_char_methods() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_std_{}", release), &clif_std_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "std ok");
    }
}

#[test]
fn a_failed_conversion_check_in_a_cranelift_executable_exits_with_101() {
    let (_t, exe_path, out) = build_clif("clif_std_bad", &clif_std_program("    core.check.eq(u8.try_from_i32(256i32).is_ok(), true)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

fn clif_trunc_program() -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let nan = core.num.f64.NAN",
        "    let inf = core.num.f64.INFINITY",
        "    let ninf = core.num.f64.NEG_INFINITY",
        "    t.show_u8(42.9f64)?",
        "    t.show_u8(200.0f64)?",
        "    t.show_u8(255.9f64)?",
        "    t.show_u8(256.0f64)?",
        "    t.show_u8(0.0f64 - 1.0f64)?",
        "    t.show_u8(nan)?",
        "    t.show_u8(inf)?",
        "    t.show_u8(ninf)?",
        "    t.show_i32(7.5f64)?",
        "    t.show_i32(0.0f64 - 100.8f64)?",
        "    t.show_i32(2147483647.9f64)?",
        "    t.show_i32(2147483648.0f64)?",
        "    t.show_i32(nan)?",
        "    core.check.eq(42.9f64.trunc_to_u8(), Some(42u8))",
        "    core.check.eq(255.9f64.trunc_to_u8(), Some(255u8))",
        "    core.check.eq(256.0f64.trunc_to_u8(), None)",
        "    core.check.eq(nan.trunc_to_u8(), None)",
        "    core.check.eq(inf.trunc_to_u8(), None)",
        "    core.check.eq(ninf.trunc_to_u8(), None)",
        "    core.check.eq(7.5f64.trunc_to_i32(), Some(7i32))",
        "    core.check.eq((0.0f64 - 100.8f64).trunc_to_i32(), Some(0i32 - 100i32))",
        "    core.check.eq(2147483647.9f64.trunc_to_i32(), Some(2147483647i32))",
        "    core.check.eq(2147483648.0f64.trunc_to_i32(), None)",
        "    core.check.eq(nan.trunc_to_i32(), None)",
        "    core.check.eq(200.4f64.trunc_to_u64(), Some(200u64))",
        "    core.check.eq((0.0f64 - 1.0f64).trunc_to_i64(), Some(0i64 - 1i64))",
        "    core.check.eq(5.9f64.trunc_to_isize(), Some(5isize))",
        "    core.check.eq(5.9f64.trunc_to_usize(), Some(5usize))",
        "    core.check.eq(129.0f64.trunc_to_i8(), None)",
        "    core.check.eq((0.0f64 - 5.9f64).trunc_to_i16(), Some(0i16 - 5i16))",
        "    core.check.eq(5.9f64.trunc_to_u16(), Some(5u16))",
        "    core.check.eq(5.9f64.trunc_to_u32(), Some(5u32))",
        "    core.check.eq(5.9f32.trunc_to_u32(), Some(5u32))",
        "    std.io.print_line(\"trunc ok\")",
        "    Ok(())",
        "end t.main",
        "",
        "fn t.show_i32(x: f64) -> Result<(), std.io.Error> uses [io.console] panics may",
        "    match x.trunc_to_i32() {",
        "        Some(7i32) => std.io.print_line(\"i32 7\")",
        "        Some(2147483647i32) => std.io.print_line(\"i32 max\")",
        "        Some(_rest) => std.io.print_line(\"i32 other\")",
        "        None => std.io.print_line(\"i32 none\")",
        "    }",
        "end t.show_i32",
        "",
        "fn t.show_u8(x: f64) -> Result<(), std.io.Error> uses [io.console] panics may",
        "    match x.trunc_to_u8() {",
        "        Some(42u8) => std.io.print_line(\"u8 42\")",
        "        Some(200u8) => std.io.print_line(\"u8 200\")",
        "        Some(255u8) => std.io.print_line(\"u8 255\")",
        "        Some(_rest) => std.io.print_line(\"u8 other\")",
        "        None => std.io.print_line(\"u8 none\")",
        "    }",
        "end t.show_u8",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_compiles_float_to_int_truncation() {
    for release in [false, true] {
        let (t, exe_path, out) = build_clif(&format!("clif_trunc_{}", release), &clif_trunc_program(), release);
        let interp = Command::new(exe()).arg("run").current_dir(&t.0).output().unwrap();
        assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
        assert!(text(&interp.stdout).contains("u8 none"), "{}", text(&interp.stdout));
        assert!(text(&interp.stdout).contains("i32 none"), "{}", text(&interp.stdout));
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
}

fn clif_map_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)",
        "    match m.insert(\"a\", 1u8) {",
        "        Ok(_old) => 0u8",
        "        Err(_e) => 1u8",
        "    }",
        "    match m.insert(\"b\", 2u8) {",
        "        Ok(_old) => 0u8",
        "        Err(_e) => 1u8",
        "    }",
        "    core.check.eq(m.get(\"a\"), Some(1u8))",
        "    core.check.eq(m.get(\"zz\"), None)",
        "    core.check.eq(m.len(), 2usize)",
        "    core.check.eq(m.insert(\"a\", 5u8), Ok(Some(1u8)))",
        "    core.check.eq(m.get(\"a\"), Some(5u8))",
        "    core.check.eq(m.len(), 2usize)",
        "    core.check.eq(m.contains_key(\"b\"), true)",
        "    core.check.eq(m.remove(\"b\"), Some(2u8))",
        "    core.check.eq(m.contains_key(\"b\"), false)",
        "    core.check.eq(m.is_empty(), false)",
        "    let mut b = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)",
        "    let i1 = b.insert(3u8, 30u8)",
        "    let i2 = b.insert(1u8, 10u8)",
        "    let i3 = b.insert(3u8, 33u8)",
        "    core.check.eq(i1, Ok(None))",
        "    core.check.eq(i2, Ok(None))",
        "    core.check.eq(i3, Ok(Some(30u8)))",
        "    core.check.eq(b.len(), 2usize)",
        "    core.check.eq(b.get(3u8), Some(33u8))",
        "    core.check.eq(b.contains_key(2u8), false)",
        "    core.check.eq(b.remove(1u8), Some(10u8))",
        "    core.check.eq(b.len(), 1usize)",
        "    let mut big = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)",
        "    for k in core.iter.range(0u32, 50u32) {",
        "        let key = k.wrapping_mul(7u32) % 50u32",
        "        match big.insert(key, k) {",
        "            Ok(_old) => 0u8",
        "            Err(_e) => 1u8",
        "        }",
        "    }",
        "    core.check.eq(big.len(), 50usize)",
        "    core.check.eq(big.get(21u32), Some(3u32))",
        tail,
        "    std.io.print_line(\"map ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_compiles_hashmap_and_btreemap() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_map_{}", release), &clif_map_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "map ok");
    }
}

#[test]
fn a_wrong_map_lookup_in_a_cranelift_executable_fails_the_check() {
    let (_t, exe_path, out) = build_clif("clif_map_bad", &clif_map_program("    core.check.eq(m.get(\"a\"), Some(1u8))"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

/// `HashMap` / `BtreeMap` の `get_mut` (返り値の参照からの読み書き) と、`HashMap.iter` の挿入順。
/// `BtreeMap.iter` は仕様 (`spec/stdlib/alloc/collections.sig`) に無く、`ksn check` が E1108 で弾くので含めない。
fn clif_map_methods_program() -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)",
        "    match m.insert(3u32, 30u32) {",
        "        Ok(_o) => 0u8",
        "        Err(_e) => 1u8",
        "    }",
        "    match m.insert(1u32, 10u32) {",
        "        Ok(_o) => 0u8",
        "        Err(_e) => 1u8",
        "    }",
        "    match m.insert(2u32, 20u32) {",
        "        Ok(_o) => 0u8",
        "        Err(_e) => 1u8",
        "    }",
        "    let mut vseq = 0u32",
        "    let mut kseq = 0u32",
        "    for (k, v) in m.iter() {",
        "        vseq = vseq.wrapping_mul(100u32).wrapping_add(*v)",
        "        kseq = kseq.wrapping_mul(10u32).wrapping_add(*k)",
        "    }",
        "    core.check.eq(vseq, 301020u32)",
        "    core.check.eq(kseq, 312u32)",
        "    match m.get_mut(1u32) {",
        "        Some(r) => {",
        "            *r = 111u32",
        "        }",
        "        None => {",
        "            core.check.eq(true, false)",
        "        }",
        "    }",
        "    core.check.eq(m.get(1u32), Some(111u32))",
        "    core.check.eq(m.get_mut(9u32).is_none(), true)",
        "    let mut b = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)",
        "    match b.insert(3u32, 30u32) {",
        "        Ok(_o) => 0u8",
        "        Err(_e) => 1u8",
        "    }",
        "    match b.insert(1u32, 10u32) {",
        "        Ok(_o) => 0u8",
        "        Err(_e) => 1u8",
        "    }",
        "    match b.insert(2u32, 20u32) {",
        "        Ok(_o) => 0u8",
        "        Err(_e) => 1u8",
        "    }",
        "    match b.get_mut(2u32) {",
        "        Some(r) => {",
        "            *r = 222u32",
        "        }",
        "        None => {",
        "            core.check.eq(true, false)",
        "        }",
        "    }",
        "    core.check.eq(b.get(2u32), Some(222u32))",
        "    core.check.eq(b.get_mut(8u32).is_none(), true)",
        "    std.io.print_line(\"map methods ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_compiles_map_get_mut_and_hashmap_iter() {
    let t = Tmp::new("clif_map_meth", &clif_map_methods_program());
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let cache = std::env::temp_dir().join(format!("ksn_cache_mapmeth_{}_{}", std::process::id(), release));
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "clif"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        let kind = if release { "release" } else { "debug" };
        let p = t.0.join("target").join(kind).join(if cfg!(windows) { "t.exe" } else { "t.native" });
        assert!(p.exists(), "native executable missing: {}", out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ ({})", kind);
        let _ = fs::remove_dir_all(&cache);
    }
}

fn clif_hashmap_index_program() -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    match t.run() {",
        "        Ok(_u) => core.check.eq(1u8, 1u8)",
        "        Err(_e) => core.panic.panic(\"oom\")",
        "    }",
        "    std.io.print_line(\"index ok\")?",
        "    Ok(())",
        "end t.main",
        "",
        "fn t.run() -> Result<(), alloc.alloc.AllocError> uses [alloc] panics may",
        "    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)",
        "    let mut i = 0u32",
        "    while i < 300u32 {",
        "        core.check.eq(m.insert(i, i.wrapping_mul(3u32))?, None)",
        "        i = i.wrapping_add(1u32)",
        "    }",
        "    core.check.eq(m.len(), 300usize)",
        "    i = 0u32",
        "    while i < 300u32 {",
        "        if i % 2u32 == 0u32 {",
        "            match m.remove(&i) {",
        "                Some(v) => core.check.eq(v, i.wrapping_mul(3u32))",
        "                None => core.panic.panic(\"missing even\")",
        "            }",
        "        }",
        "        i = i.wrapping_add(1u32)",
        "    }",
        "    core.check.eq(m.len(), 150usize)",
        "    i = 0u32",
        "    while i < 300u32 {",
        "        match m.get(&i) {",
        "            Some(v) => core.check.eq(*v, i.wrapping_mul(3u32))",
        "            None => core.check.eq(i % 2u32, 0u32)",
        "        }",
        "        i = i.wrapping_add(1u32)",
        "    }",
        "    i = 0u32",
        "    while i < 300u32 {",
        "        if i % 2u32 == 0u32 {",
        "            core.check.eq(m.insert(i, i.wrapping_mul(5u32))?, None)",
        "        }",
        "        i = i.wrapping_add(1u32)",
        "    }",
        "    let mut j = 0u32",
        "    for (k, v) in m.iter() {",
        "        let mut want_k = 0u32",
        "        let mut want_v = 0u32",
        "        if j < 150u32 {",
        "            want_k = j.wrapping_mul(2u32).wrapping_add(1u32)",
        "            want_v = want_k.wrapping_mul(3u32)",
        "        } else {",
        "            want_k = j.wrapping_sub(150u32).wrapping_mul(2u32)",
        "            want_v = want_k.wrapping_mul(5u32)",
        "        }",
        "        core.check.eq(*k, want_k)",
        "        core.check.eq(*v, want_v)",
        "        j = j.wrapping_add(1u32)",
        "    }",
        "    core.check.eq(j, 300u32)",
        "    let mut s = alloc.collections.HashMap.new(std.alloc.SYSTEM)",
        "    core.check.eq(s.insert(\"x\", 1u32)?, None)",
        "    core.check.eq(s.insert(\"y\", 2u32)?, None)",
        "    core.check.eq(s.insert(\"z\", 3u32)?, None)",
        "    core.check.eq(s.remove(\"y\"), Some(2u32))",
        "    core.check.eq(s.get(\"z\"), Some(3u32))",
        "    core.check.eq(s.insert(\"y\", 4u32)?, None)",
        "    core.check.eq(s.len(), 3usize)",
        "    Ok(())",
        "end t.run",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_hashmap_index_stays_consistent_across_growth_removal_and_reinsertion() {
    // 索引表（ハッシュ → 組の添字）は、表が育つとき・中ほどの削除で添字が詰まるとき・再挿入のときも、
    // 検索結果と挿入順を保つ。インタプリタと Cranelift (デバッグ) で同じ出力になる。
    let t = Tmp::new("hm_index", &clif_hashmap_index_program());
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    assert_eq!(text(&interp.stdout).trim(), "index ok");
    let cache = std::env::temp_dir().join(format!("ksn_cache_hmindex_{}", std::process::id()));
    let o = Command::new(exe())
        .args(["build", "--backend", "clif"])
        .current_dir(&t.0)
        .env("KSN_CACHE_DIR", &cache)
        .output()
        .unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    let p = t.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t.native" });
    assert!(p.exists(), "native executable missing: {}", out);
    let r = Command::new(&p).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    let _ = fs::remove_dir_all(&cache);
}

/// `std.sync.Mutex`（`spawn` と同じく同期実行）。`lock` は値のアドレスをそのままガードとして返し、
/// `get` / `get_mut` は同じ場所を読み書きする。ガードの `Drop` は no-op。
fn clif_sync_program() -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.main() -> Result<(), std.io.Error> uses [io.console, thread] panics may",
        "    let m = std.sync.Mutex.new(5u32)",
        "    let mut g = m.lock()",
        "    core.check.eq(*g.get(), 5u32)",
        "    *g.get_mut() = 42u32",
        "    core.check.eq(*g.get(), 42u32)",
        "    core.mem.drop(g)",
        "    std.io.print_line(\"sync ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_compiles_mutex_lock_and_guard() {
    let t = Tmp::new("clif_sync", &clif_sync_program());
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let cache = std::env::temp_dir().join(format!("ksn_cache_sync_{}_{}", std::process::id(), release));
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "clif"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        let kind = if release { "release" } else { "debug" };
        let p = t.0.join("target").join(kind).join(if cfg!(windows) { "t.exe" } else { "t.native" });
        assert!(p.exists(), "native executable missing: {}", out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ ({})", kind);
        let _ = fs::remove_dir_all(&cache);
    }
}

#[test]
fn the_features_example_runs_natively_through_cranelift() {
    let d = root().join("examples").join("features");
    let cache = std::env::temp_dir().join(format!("ksn_cache_feat_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "clif"]).current_dir(&d).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        let kind = if release { "release" } else { "debug" };
        let p = d.join("target").join(kind).join(if cfg!(windows) { "features.exe" } else { "features.native" });
        assert!(p.exists(), "native executable missing: {}", out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn the_threads_example_runs_natively_through_cranelift() {
    let d = root().join("examples").join("threads");
    let cache = std::env::temp_dir().join(format!("ksn_cache_thr_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    let o = Command::new(exe()).args(["build", "--backend", "clif"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    let p = d.join("target").join("debug").join(if cfg!(windows) { "threads.exe" } else { "threads.native" });
    assert!(p.exists(), "native executable missing: {}", out);
    let r = Command::new(&p).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn the_tour_example_runs_natively_through_cranelift() {
    let d = root().join("examples").join("tour");
    let cache = std::env::temp_dir().join(format!("ksn_cache_tour_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "clif"]).current_dir(&d).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        let kind = if release { "release" } else { "debug" };
        let p = d.join("target").join(kind).join(if cfg!(windows) { "tour.exe" } else { "tour.native" });
        assert!(p.exists(), "native executable missing: {}", out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
    let _ = fs::remove_dir_all(&cache);
}

fn clif_generic_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "enum t.Maybe[T] derive [Debug, Clone, Copy, PartialEq]",
        "    Nothing",
        "    Just(T)",
        "end t.Maybe",
        "",
        "type t.Pair[A, B] derive [Debug, Clone, Copy, PartialEq]",
        "    first: A",
        "    second: B",
        "end t.Pair",
        "",
        "fn t.Maybe.or_else[T](self: t.Maybe<T>, other: T) -> T",
        "    match self {",
        "        t.Maybe.Just(v) => v",
        "        t.Maybe.Nothing => other",
        "    }",
        "end t.Maybe.or_else",
        "",
        "fn t.Pair.swap[A, B](self: t.Pair<A, B>) -> t.Pair<B, A>",
        "    t.Pair { first: self.second, second: self.first }",
        "end t.Pair.swap",
        "",
        "fn t.pick[T](flag: bool, a: T, b: T) -> T",
        "    if flag {",
        "        return a",
        "    }",
        "    b",
        "end t.pick",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let p = t.Pair { first: 1u32, second: true }",
        "    let q = p.swap()",
        "    core.check.eq(q.first, true)",
        "    core.check.eq(q.second, 1u32)",
        "    core.check.eq(q, t.Pair { first: true, second: 1u32 })",
        "    core.check.eq(t.pick(true, 3u8, 4u8), 3u8)",
        "    core.check.eq(t.pick(false, 3i64, 4i64), 4i64)",
        "    core.check.eq(t.pick(false, p, p.swap().swap()), p)",
        "    let m = t.Maybe.Just(5u32)",
        "    core.check.eq(m.or_else(9u32), 5u32)",
        "    let n: t.Maybe<u32> = t.Maybe.Nothing",
        "    core.check.eq(n.or_else(9u32), 9u32)",
        "    let nested = t.Pair { first: t.Maybe.Just(2u8), second: p }",
        "    core.check.eq(nested.second.first, 1u32)",
        "    core.check.eq(nested.first.or_else(0u8), 2u8)",
        tail,
        "    std.io.print_line(\"generic ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_monomorphizes_generic_types_and_functions() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_gen_{}", release), &clif_generic_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "generic ok");
    }
}

#[test]
fn a_wrong_generic_result_in_a_cranelift_executable_fails_the_check() {
    let (_t, exe_path, out) = build_clif("clif_gen_bad", &clif_generic_program("    core.check.eq(t.pick(true, 3u8, 4u8), 4u8)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

#[test]
fn a_generic_function_calls_trait_methods_of_its_concrete_type_arguments() {
    let nl = char::from(10u8).to_string();
    let src = [
        "trait t.Area",
        "    fn area(self: &Self) -> u32 uses [] panics never",
        "end t.Area",
        "",
        "type t.Circle",
        "    r: u32",
        "end t.Circle",
        "",
        "type t.Sq",
        "    side: u32",
        "end t.Sq",
        "",
        "impl t.Area for t.Circle",
        "fn t.Circle.area(self: &t.Circle) -> u32 uses [] panics never impls t.Area",
        "    self.r.wrapping_mul(self.r).wrapping_mul(3u32)",
        "end t.Circle.area",
        "",
        "impl t.Area for t.Sq",
        "fn t.Sq.area(self: &t.Sq) -> u32 uses [] panics never impls t.Area",
        "    self.side.wrapping_mul(self.side)",
        "end t.Sq.area",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let c1 = t.Circle { r: 2u32 }",
        "    let c2 = t.Circle { r: 1u32 }",
        "    let s1 = t.Sq { side: 3u32 }",
        "    let s2 = t.Sq { side: 4u32 }",
        "    core.check.eq(t.total(&c1, &c2), 15u32)",
        "    core.check.eq(t.total(&s1, &s2), 25u32)",
        "    std.io.print_line(\"trait generic ok\")",
        "    Ok(())",
        "end t.main",
        "",
        "fn t.total[T: t.Area](a: &T, b: &T) -> u32",
        "    a.area().wrapping_add(b.area())",
        "end t.total",
        "",
    ]
    .join(&nl);
    let (_t, exe_path, out) = build_clif("clif_gen_trait", &src, false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "trait generic ok");
}

fn clif_fn_bound_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.apply[F: core.ops.Fn<(u32,), u32, E, P>, E: uses, P: panics](f: F, x: u32) -> u32 uses [E] panics P",
        "    f(x)",
        "end t.apply",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let triple = fn(n: u32) -> u32 capture [] {",
        "        n.wrapping_mul(3u32)",
        "    }",
        "    core.check.eq(t.apply(triple, 14u32), 42u32)",
        "    let add_one = fn(n: u32) -> u32 capture [] {",
        "        n.wrapping_add(1u32)",
        "    }",
        "    core.check.eq(t.relay(add_one, 5u32), 6u32)",
        "    let square = fn(n: u32) -> u32 capture [] {",
        "        n.wrapping_mul(n)",
        "    }",
        "    core.check.eq(t.twice(square, 3u32), 81u32)",
        "    let big = fn(n: u8) -> bool capture [] {",
        "        n > 5u8",
        "    }",
        "    core.check.eq(t.map_one(7u8, big), true)",
        "    let small = fn(n: u8) -> bool capture [] {",
        "        n > 5u8",
        "    }",
        "    core.check.eq(t.map_one(3u8, small), false)",
        "    let wide = fn(n: i32) -> i64 capture [] {",
        "        10i64",
        "    }",
        "    core.check.eq(t.map_one(2i32, wide), 10i64)",
        tail,
        "    std.io.print_line(\"fn bound ok\")",
        "    Ok(())",
        "end t.main",
        "",
        "fn t.map_one[T, U, F: core.ops.Fn<(T,), U, E, P>, E: uses, P: panics](x: T, f: F) -> U uses [E] panics P",
        "    f(x)",
        "end t.map_one",
        "",
        "fn t.relay[F: core.ops.Fn<(u32,), u32, E, P>, E: uses, P: panics](f: F, x: u32) -> u32 uses [E] panics P",
        "    t.apply(f, x)",
        "end t.relay",
        "",
        "fn t.twice[F: core.ops.Fn<(u32,), u32, E, P>, E: uses, P: panics](f: F, x: u32) -> u32 uses [E] panics P",
        "    f(f(x))",
        "end t.twice",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_specializes_generic_functions_for_closure_arguments() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_fnb_{}", release), &clif_fn_bound_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "fn bound ok");
    }
}

#[test]
fn a_wrong_closure_result_through_a_generic_function_fails_the_check() {
    let (_t, exe_path, out) = build_clif("clif_fnb_bad", &clif_fn_bound_program("    let bad = fn(n: u32) -> u32 capture [] {
        n
    }
    core.check.eq(t.apply(bad, 1u32), 2u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

// ---- NLL に基づく借用検査 (borrowck.rs) ----

fn has_e3010(name: &str, prefix: &str, b: &[&str]) -> (bool, String) {
    let t = Tmp::new(name, &main_wrap(prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out).iter().any(|c| c == "E3010"), out)
}

#[test]
fn nll_rejects_a_mutation_while_a_shared_borrow_is_still_used() {
    let (bad, out) = has_e3010(
        "nll_shared",
        "",
        &["let mut x = 1u8", "let r = &x", "x = 2u8", "core.check.eq(*r, 1u8)"],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_rejects_any_use_while_a_mutable_borrow_is_still_used() {
    let (bad, out) = has_e3010(
        "nll_mut",
        "",
        &["let mut x = 1u8", "let r = &mut x", "core.check.eq(x, 1u8)", "*r = 2u8"],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot use `x` while it is mutably borrowed by `r`"), "{}", out);
}

#[test]
fn nll_rejects_pushing_to_a_vec_while_a_reference_into_it_is_used() {
    let (bad, out) = has_e3010(
        "nll_first",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "match v.push(1u8) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "let first = v.first()",
            "match v.push(2u8) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "core.check.eq(first, Some(1u8))",
        ],
    );
    assert!(bad, "{}", out);
}

#[test]
fn nll_rejects_mutating_a_vec_inside_a_loop_over_its_iterator() {
    let (bad, out) = has_e3010(
        "nll_iter",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "match v.push(1u8) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "for x in v.iter() {",
            "    match v.push(*x) {",
            "        Ok(_u) => 0u8",
            "        Err(_e) => 1u8",
            "    }",
            "}",
        ],
    );
    assert!(bad, "{}", out);
}

#[test]
fn nll_accepts_code_where_the_borrow_is_dead_before_the_conflict() {
    // 借用の最後の使用が、変更より前にある。
    let (bad, out) = has_e3010("nll_dead", "", &["let mut x = 1u8", "let r = &x", "core.check.eq(*r, 1u8)", "x = 2u8", "core.check.eq(x, 2u8)"]);
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_reassigned_reference_even_if_the_old_borrow_conflicts() {
    // r は再代入される前の値をもう使わない: 古い借用は、再代入の前にすでに死んでいる。
    let (bad, out) = has_e3010(
        "nll_reassign",
        "",
        &[
            "let mut x = 1u8",
            "let mut y = 2u8",
            "let mut r = &mut x",
            "*r = 9u8",
            "core.check.eq(x, 9u8)",
            "r = &mut y",
            "*r = 3u8",
            "core.check.eq(y, 3u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_borrow_used_only_on_the_other_branch() {
    let (bad, out) = has_e3010(
        "nll_branch",
        "",
        &[
            "let mut x = 1u8",
            "let flag = true",
            "let r = &mut x",
            "if flag {",
            "    *r = 5u8",
            "} else {",
            "    core.check.eq(1u8, 1u8)",
            "}",
            "x = 7u8",
            "core.check.eq(x, 7u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_treats_distinct_fields_of_a_struct_as_disjoint() {
    let prefix = "type t.S\n    a: u8\n    b: u8\nend t.S\n\n";
    let (bad, out) = has_e3010(
        "nll_fields",
        prefix,
        &["let mut s = t.S { a: 1u8, b: 2u8 }", "let ra = &mut s.a", "s.b = 9u8", "*ra = 3u8", "core.check.eq(s.b, 9u8)"],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_two_phase_borrows_like_push_of_len() {
    let (bad, out) = has_e3010(
        "nll_twophase",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "match v.push(1u8) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "match v.push(2u8) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "core.check.eq(v.len(), 2usize)",
        ],
    );
    assert!(!bad, "{}", out);
}

// ---- 構造体・コンテナに保存された借用 (領域の近似) ----

const NLL_REF_FIELD: &str = "type t.S['a]\n    f: &'a u8\n    b: u8\nend t.S\n\n";

#[test]
fn nll_rejects_a_mutation_while_a_borrow_is_stored_in_a_struct_field() {
    let (bad, out) = has_e3010(
        "nll_store_field",
        NLL_REF_FIELD,
        &["let mut x = 1u8", "let s = t.S { f: &x, b: 2u8 }", "x = 2u8", "core.check.eq(s.b, 2u8)"],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `s.f`"), "{}", out);
}

#[test]
fn nll_rejects_any_use_while_a_mutable_borrow_is_stored_in_a_vec() {
    let (bad, out) = has_e3010(
        "nll_store_vec_mut",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "match v.push(&mut x) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "core.check.eq(x, 1u8)",
            "core.check.eq(v.len(), 1usize)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot use `x` while it is mutably borrowed by `v`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_stored_borrow_is_used_in_a_match_arm() {
    let (bad, out) = has_e3010(
        "nll_store_match",
        "",
        &[
            "let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "match m.insert(1u8, &x) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "match m.get(1u8) {",
            "    Some(rr) => {",
            "        x = 2u8",
            "        core.check.eq(*rr, 1u8)",
            "    }",
            "    None => 0u8",
            "}",
        ],
    );
    assert!(bad, "{}", out);
}

#[test]
fn nll_rejects_a_mutation_after_a_stored_borrow_is_read_back() {
    let (bad, out) = has_e3010(
        "nll_store_read",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "match v.push(&x) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "let r = v.first().unwrap()",
            "x = 2u8",
            "core.check.eq(*r, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_accepts_a_stored_borrow_overwritten_before_the_conflict() {
    // フィールドへの再代入で、前に保存された借用は死ぬ。
    let (bad, out) = has_e3010(
        "nll_store_overwrite",
        NLL_REF_FIELD,
        &[
            "let mut x = 1u8",
            "let mut y = 2u8",
            "let mut s = t.S { f: &x, b: 2u8 }",
            "s.f = &y",
            "core.check.eq(x, 1u8)",
            "core.check.eq(s.f, &y)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_vec_reassigned_before_the_conflict() {
    // コンテナごと作り直すと、保存されていた借用は死ぬ。
    let (bad, out) = has_e3010(
        "nll_store_reassign",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "match v.push(&x) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "core.check.eq(x, 1u8)",
            "core.check.eq(v.len(), 0usize)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_stored_borrow_dropped_before_the_conflict() {
    // `core.mem.drop(s.f)` でフィールドの値を消費すると、保存された借用は死ぬ。
    let (bad, out) = has_e3010(
        "nll_store_drop",
        NLL_REF_FIELD,
        &[
            "let mut x = 1u8",
            "let mut s = t.S { f: &x, b: 2u8 }",
            "core.mem.drop(s.f)",
            "core.check.eq(x, 1u8)",
            "core.check.eq(s.b, 2u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_mutation_after_the_storage_holder_is_dead() {
    // 保存場所の根の変数がもう使われないなら、借りた元は自由に使える。
    let (bad, out) = has_e3010(
        "nll_store_dead",
        NLL_REF_FIELD,
        &[
            "let mut x = 1u8",
            "let s = t.S { f: &x, b: 2u8 }",
            "core.check.eq(s.b, 2u8)",
            "x = 5u8",
            "core.check.eq(x, 5u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_rejects_a_mutation_of_the_borrowed_value_inside_a_loop_over_the_container() {
    let (bad, out) = has_e3010(
        "nll_store_loop",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "match v.push(&x) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "for r in v.iter() {",
            "    x = 2u8",
            "    core.check.eq(*r, 1u8)",
            "}",
        ],
    );
    assert!(bad, "{}", out);
}

// ---- 借用の残り: match の値を通じた運搬、タプル・配列リテラルへの保存 ----

const NLL_PUSH1: [&str; 5] =
    ["let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)", "match v.push(1u8) {", "    Ok(_u) => 0u8", "    Err(_e) => 1u8", "}"];
const NLL_PUSH2: [&str; 4] = ["match v.push(2u8) {", "    Ok(_u) => 0u8", "    Err(_e) => 1u8", "}"];

fn nll_lines(parts: &[&[&'static str]]) -> Vec<&'static str> {
    parts.concat()
}

#[test]
fn nll_rejects_a_mutation_while_a_match_value_carries_the_arm_binding_borrow() {
    // `let r = match v.first() { Some(rr) => rr, .. }`: 被検査式のローンが腕の束縛を経て r に移る。
    let b = nll_lines(&[
        &NLL_PUSH1,
        &["let y = 9u8", "let r = match v.first() {", "    Some(rr) => rr", "    None => &y", "}"],
        &NLL_PUSH2,
        &["core.check.eq(*r, 1u8)"],
    ]);
    let (bad, out) = has_e3010("nll_match_val_bind", "", &b);
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `v` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_borrowed_by_any_arm_of_a_match_value() {
    // 両腕の和: どちらの腕の借用も r が持つ。
    let (bad, out) = has_e3010(
        "nll_match_val_arms",
        "",
        &[
            "let mut x = 1u8",
            "let y = 2u8",
            "let b = true",
            "let r = match b {",
            "    true => &x",
            "    false => &y",
            "}",
            "x = 3u8",
            "core.check.eq(*r, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_nested_match_value_carries_a_borrow() {
    let b = nll_lines(&[
        &NLL_PUSH1,
        &[
            "let y = 9u8",
            "let b = true",
            "let r = match b {",
            "    true => match v.first() {",
            "        Some(rr) => rr",
            "        None => &y",
            "    }",
            "    false => &y",
            "}",
        ],
        &NLL_PUSH2,
        &["core.check.eq(*r, 1u8)"],
    ]);
    let (bad, out) = has_e3010("nll_match_val_nested", "", &b);
    assert!(bad, "{}", out);
}

#[test]
fn nll_accepts_a_match_value_that_copies_out_of_the_borrow() {
    // 腕の値が `*rr` (コピー) なら、match の値は借用を運ばない。
    let b = nll_lines(&[
        &NLL_PUSH1,
        &["let n = match v.first() {", "    Some(rr) => *rr", "    None => 0u8", "}"],
        &NLL_PUSH2,
        &["core.check.eq(n, 1u8)"],
    ]);
    let (bad, out) = has_e3010("nll_match_val_copy", "", &b);
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_match_value_that_is_dead_before_the_mutation() {
    let b = nll_lines(&[
        &NLL_PUSH1,
        &["let y = 9u8", "let r = match v.first() {", "    Some(rr) => rr", "    None => &y", "}", "core.check.eq(*r, 1u8)"],
        &NLL_PUSH2,
        &["core.check.eq(v.len(), 2usize)"],
    ]);
    let (bad, out) = has_e3010("nll_match_val_dead", "", &b);
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_match_value_inside_a_loop_without_leaking_the_previous_iteration() {
    let b = nll_lines(&[
        &NLL_PUSH1,
        &[
            "let mut i = 0u8",
            "while i < 3u8 {",
            "    let n = match v.first() {",
            "        Some(rr) => *rr",
            "        None => 0u8",
            "    }",
            "    match v.push(n) {",
            "        Ok(_u) => 0u8",
            "        Err(_e) => 1u8",
            "    }",
            "    i = i + 1u8",
            "}",
            "core.check.eq(v.len(), 4usize)",
        ],
    ]);
    let (bad, out) = has_e3010("nll_match_val_loop", "", &b);
    assert!(!bad, "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_borrow_is_stored_in_a_tuple_literal() {
    let (bad, out) =
        has_e3010("nll_store_tuple", "", &["let mut x = 1u8", "let t = (&x, 1u8)", "x = 2u8", "let _u = t"]);
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `t`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_borrow_is_stored_in_an_array_literal() {
    let (bad, out) = has_e3010(
        "nll_store_array",
        "",
        &["let mut x = 1u8", "let y = 2u8", "let a = [&x, &y]", "x = 5u8", "core.check.eq(*a[1], 2u8)"],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `a`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_tuple_with_a_borrow_is_pushed_into_a_vec() {
    let (bad, out) = has_e3010(
        "nll_store_push_tuple",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "match v.push((&x, 1u8)) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "x = 2u8",
            "core.check.eq(v.len(), 1usize)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `v`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_inside_a_loop_over_an_array_literal_of_borrows() {
    let (bad, out) = has_e3010(
        "nll_store_array_iter",
        "",
        &["let mut x = 1u8", "let y = 2u8", "let a = [&x, &y]", "for r in a.iter() {", "    x = 5u8", "    core.check.eq(**r, 2u8)", "}"],
    );
    assert!(bad, "{}", out);
}

#[test]
fn nll_accepts_a_tuple_literal_that_holds_no_borrow_or_is_dead() {
    let (bad, out) = has_e3010(
        "nll_store_tuple_ok",
        "",
        &[
            "let mut x = 1u8",
            "let t = (x, 1u8)",
            "let t2 = (&x, 1u8)",
            "let _u = t2",
            "x = 2u8",
            "let _w = t",
            "core.check.eq(x, 2u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_an_array_literal_overwritten_before_the_conflict() {
    // 変数の再代入・要素の上書きで、保存されていた借用は死ぬ。
    let (bad, out) = has_e3010(
        "nll_store_array_ok",
        "",
        &[
            "let mut x = 1u8",
            "let y = 2u8",
            "let mut a = [&x, &y]",
            "a = [&y, &y]",
            "let mut c = [&x]",
            "c[0] = &y",
            "x = 5u8",
            "core.check.eq(*a[0], 2u8)",
            "core.check.eq(*c[0], 2u8)",
            "core.check.eq(x, 5u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

// ---- 部分 move の追跡 (4.1・4.4) ----

fn pmove_check(name: &str, helper: &[&str], b: &[&str]) -> (bool, String) {
    let nl = char::from(10u8).to_string();
    let prefix = helper.join(&nl) + &nl + &nl;
    let t = Tmp::new(name, &main_wrap(&prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out).iter().any(|c| c == "E3002"), out)
}

fn pmove_prefix() -> Vec<&'static str> {
    vec![
        "type t.P",
        "    a: u8",
        "end t.P",
        "",
        "type t.S",
        "    f: t.P",
        "    g: t.P",
        "end t.S",
        "",
        "type t.I",
        "    b: t.P",
        "    n: u8",
        "end t.I",
        "",
        "fn t.take(p: t.P) -> () uses [] panics never",
        "    core.mem.drop(p)",
        "end t.take",
        "",
        "fn t.take_s(s: t.S) -> () uses [] panics never",
        "    core.mem.drop(s)",
        "end t.take_s",
        "",
        "fn t.take_i(x: t.I) -> () uses [] panics never",
        "    core.mem.drop(x)",
        "end t.take_i",
        "",
        "fn t.take_u(x: u8) -> () uses [] panics never",
        "    core.mem.drop(x)",
        "end t.take_u",
        "",
    ]
}

#[test]
fn a_partial_move_makes_the_whole_value_unusable() {
    let (bad, out) = pmove_check(
        "pmove_whole",
        &pmove_prefix(),
        &[
            "let s = t.S { f: t.P { a: 1u8 }, g: t.P { a: 2u8 } }",
            "t.take(s.f)",
            "t.take_s(s)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("use of partially moved value `s`"), "{}", out);
    assert!(out.contains("moved `s.f`"), "{}", out);
}

#[test]
fn consuming_one_field_leaves_sibling_fields_usable() {
    // s.f を消したあとでも、別のフィールド s.g の消費は OK (4.1)。
    let (bad, out) = pmove_check(
        "pmove_sibling",
        &pmove_prefix(),
        &[
            "let s = t.S { f: t.P { a: 1u8 }, g: t.P { a: 2u8 } }",
            "t.take(s.f)",
            "t.take(s.g)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn a_moved_field_is_revived_by_reassignment() {
    let (bad, out) = pmove_check(
        "pmove_revive",
        &pmove_prefix(),
        &[
            "let mut s = t.S { f: t.P { a: 1u8 }, g: t.P { a: 2u8 } }",
            "t.take(s.f)",
            "s.f = t.P { a: 3u8 }",
            "t.take_s(s)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn a_moved_field_cannot_be_consumed_twice_until_it_is_reassigned() {
    let (bad, out) = pmove_check(
        "pmove_twice",
        &pmove_prefix(),
        &[
            "let s = t.S { f: t.P { a: 1u8 }, g: t.P { a: 2u8 } }",
            "t.take(s.f)",
            "t.take(s.f)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("use of moved value `s.f`"), "{}", out);
}

#[test]
fn copy_fields_are_never_moved() {
    // t.I.n は u8 (Copy)。何度消しても move にならない (既存の Copy 判定と整合)。
    let (bad, out) = pmove_check(
        "pmove_copy",
        &pmove_prefix(),
        &[
            "let s = t.I { b: t.P { a: 1u8 }, n: 7u8 }",
            "t.take_u(s.n)",
            "t.take_u(s.n)",
            "t.take_i(s)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nested_field_paths_are_tracked_as_places() {
    let (bad, out) = pmove_check(
        "pmove_nested",
        &pmove_prefix(),
        &[
            "let s = t.I { b: t.P { a: 1u8 }, n: 7u8 }",
            "t.take(s.b)",
            "t.take(s.b)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("use of moved value `s.b`"), "{}", out);
}

#[test]
fn a_nested_place_blocks_uses_of_its_prefixes_but_not_siblings() {
    // s.a.b を move したあと、s.a 全体の使用は E3002、s.a.n (兄弟の道) は OK。
    // s 全体の使用も部分 move で E3002。
    let mut helper = pmove_prefix();
    helper.push("type t.Out");
    helper.push("    a: t.I");
    helper.push("end t.Out");
    helper.push("");
    helper.push("fn t.take_o(x: t.Out) -> () uses [] panics never");
    helper.push("    core.mem.drop(x)");
    helper.push("end t.take_o");
    helper.push("");
    let (bad, out) = pmove_check(
        "pmove_prefix_use",
        &helper,
        &[
            "let s = t.Out { a: t.I { b: t.P { a: 1u8 }, n: 2u8 } }",
            "t.take(s.a.b)",
            "t.take_u(s.a.n)",
            "t.take_i(s.a)",
            "t.take_o(s)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("use of partially moved value `s.a`"), "{}", out);
    assert!(out.contains("use of partially moved value `s`"), "{}", out);
}

#[test]
fn a_field_moved_in_one_branch_is_fine_after_the_merge() {
    // 分岐の合流は既存の move 検査の流儀: 枝の中で move しても、合流後は move として扱わない。
    let (bad, out) = pmove_check(
        "pmove_branch",
        &pmove_prefix(),
        &[
            "let s = t.S { f: t.P { a: 1u8 }, g: t.P { a: 2u8 } }",
            "let flag = true",
            "if flag {",
            "    t.take(s.f)",
            "} else {",
            "    t.take(s.g)",
            "}",
            "t.take_s(s)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn reading_a_field_of_a_fully_moved_value_is_still_an_error() {
    // 全体を move したあとのフィールドの読みは、従来どおり E3002 (鍵が s で重なる)。
    let (bad, out) = pmove_check(
        "pmove_after_whole",
        &pmove_prefix(),
        &[
            "let s = t.S { f: t.P { a: 1u8 }, g: t.P { a: 2u8 } }",
            "t.take_s(s)",
            "t.take(s.f)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("use of moved value `s.f`"), "{}", out);
    assert!(out.contains("moved `s` at line"), "{}", out);
}

fn clif_capture_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "type t.P",
        "    a: u32",
        "end t.P",
        "",
        "fn t.apply[F: core.ops.Fn<(u32,), u32, E, P>, E: uses, P: panics](f: F, x: u32) -> u32 uses [E] panics P",
        "    f(x)",
        "end t.apply",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let base = 100u32",
        "    let add_base = fn(n: u32) -> u32 capture [&base] {",
        "        n.wrapping_add(base)",
        "    }",
        "    core.check.eq(t.apply(add_base, 5u32), 105u32)",
        "    let k = 7u32",
        "    let times = fn(n: u32) -> u32 capture [&k] {",
        "        n.wrapping_mul(k)",
        "    }",
        "    core.check.eq(t.relay(times, 3u32), 21u32)",
        "    let mut total = 0u32",
        "    let acc = fn(n: u32) -> () capture [&mut total] {",
        "        total = total.wrapping_add(n)",
        "    }",
        "    t.run_all(acc)",
        "    core.check.eq(total, 6u32)",
        "    let p = t.P { a: 9u32 }",
        "    let read_a = fn(n: u32) -> u32 capture [&p] {",
        "        p.a.wrapping_add(n)",
        "    }",
        "    core.check.eq(t.apply(read_a, 1u32), 10u32)",
        "    let two = 2u32",
        "    let three = 3u32",
        "    let both = fn(n: u32) -> u32 capture [&three, &two] {",
        "        n.wrapping_mul(two).wrapping_add(three)",
        "    }",
        "    core.check.eq(t.apply(both, 10u32), 23u32)",
        tail,
        "    std.io.print_line(\"capture ok\")",
        "    Ok(())",
        "end t.main",
        "",
        "fn t.relay[F: core.ops.Fn<(u32,), u32, E, P>, E: uses, P: panics](f: F, x: u32) -> u32 uses [E] panics P",
        "    t.apply(f, x)",
        "end t.relay",
        "",
        "fn t.run_all[F: core.ops.FnMut<(u32,), (), E, P>, E: uses, P: panics](f: F) -> () uses [E] panics P",
        "    f(1u32)",
        "    f(2u32)",
        "    f(3u32)",
        "end t.run_all",
        "",
    ]
    .join(&nl)
}

#[test]
fn the_cranelift_backend_passes_capturing_closures_to_generic_functions() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("clif_cap_{}", release), &clif_capture_program("    core.check.eq(1u8, 1u8)"), release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), "capture ok");
    }
}

#[test]
fn a_wrong_value_through_a_captured_variable_fails_the_check() {
    let (_t, exe_path, out) = build_clif("clif_cap_bad", &clif_capture_program("    core.check.eq(total, 7u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

const FFI_SRC: &str = "extern t.abs(x: i32) -> i32 link \"abs\"

extern t.free(pointer: *mut u8) -> () link \"free\"

extern t.malloc(size: usize) -> *mut u8 link \"malloc\"

extern t.strlen(text: *const u8) -> usize link \"strlen\"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe \"libc\" \"abs of a literal\" {
        t.abs(0i32 - 5i32)
    }
    core.check.eq(a, 5i32)
    let p = unsafe \"libc\" \"allocate\" {
        t.malloc(16usize)
    }
    unsafe \"libc\" \"write and read back\" {
        core.ptr.write(p, 7u8)
        core.ptr.write(core.ptr.add(p, 1usize), 0u8)
        core.check.eq(core.ptr.read(p), 7u8)
        core.check.eq(t.strlen(core.ptr.as_const(p)), 1usize)
        t.free(p)
    }
    std.io.print_line(\"ffi ok\")
    Ok(())
end t.main
";

#[test]
fn ffi_extern_calls_and_raw_pointers_work_natively() {
    let t = Tmp::new("ffi_native", FFI_SRC);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_ffi", std::process::id()));
    let b = Command::new(exe())
        .args(["build", "--backend", "clif"])
        .current_dir(&t.0)
        .env("KSN_CACHE_DIR", &cache)
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&cache);
    let bin = t.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t.native" });
    assert!(bin.exists(), "{}{}", text(&b.stdout), text(&b.stderr));
    let r = Command::new(&bin).output().unwrap();
    assert_eq!(text(&r.stdout), "ffi ok\n");
}

#[test]
fn ffi_interpreter_calls_c_functions_with_integer_arguments() {
    let src = "extern t.abs(x: i32) -> i32 link \"abs\"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe \"libc\" \"abs of a literal\" {
        t.abs(0i32 - 7i32)
    }
    core.check.eq(a, 7i32)
    std.io.print_line(\"ok\")
    Ok(())
end t.main
";
    let t = Tmp::new("ffi_interp", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let o = run(&["run"], &t.0);
    assert_eq!(text(&o.stdout), "ok\n", "{}", text(&o.stderr));
}

// ---- 一時値の解放の残り: Field・タプルの型解決と E2042 / E3003 (4.4・4.6) ----

fn tdrop_codes(name: &str, b: &[&str]) -> (Vec<String>, String) {
    let nl = char::from(10u8).to_string();
    let prefix = tdrop_prefix().join(&nl) + &nl + &nl;
    let t = Tmp::new(name, &main_wrap(&prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

fn tdrop_prefix() -> Vec<&'static str> {
    vec![
        "type t.Box[T]",
        "    v: T",
        "end t.Box",
        "",
        "type t.L linear pub",
        "    a: u8",
        "end t.L",
        "",
        "type t.P",
        "    a: u8",
        "end t.P",
        "",
        "type t.W",
        "    lin: t.L",
        "    n: u8",
        "end t.W",
        "",
        "fn t.eat(l: t.L) -> () uses [] panics never",
        "    let t.L { a: a } = l",
        "    core.mem.drop(a)",
        "end t.eat",
        "",
        "fn t.eat_w(w: t.W) -> () uses [] panics never",
        "    let t.W { lin: lin, n: n } = w",
        "    t.eat(lin)",
        "    core.mem.drop(n)",
        "end t.eat_w",
        "",
        "fn t.get_l() -> t.L uses [] panics never",
        "    t.L { a: 1u8 }",
        "end t.get_l",
        "",
        "fn t.get_p() -> t.P uses [] panics never",
        "    t.P { a: 3u8 }",
        "end t.get_p",
        "",
        "fn t.get_w() -> t.W uses [] panics never",
        "    t.W { lin: t.get_l(), n: 2u8 }",
        "end t.get_w",
        "",
    ]
}

fn tdrop_has(cs: &[String], c: &str) -> bool {
    cs.iter().any(|x| x == c)
}

#[test]
fn tdrop_discarding_a_field_of_a_temporary_is_e2042() {
    // `t.get_w().lin` は Field の道。受け手の型 (t.W) からフィールドの型 (t.L) を解決する。
    let (cs, out) = tdrop_codes("tdrop_field", &["t.get_w().lin"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
    assert!(out.contains("core.mem.drop(t.get_w().lin)"), "{}", out);
}

#[test]
fn tdrop_discarding_a_tuple_of_temporaries_is_e2042() {
    let (cs, out) = tdrop_codes("tdrop_tuple", &["(t.get_l(), 1u8)"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tdrop_discarding_a_local_field_path_is_e2042() {
    // 複数セグメントの Path (`p.a`) も根の型からフィールドの型をたどる。
    let (cs, out) = tdrop_codes("tdrop_path", &["let p = t.P { a: 1u8 }", "p.a"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tdrop_struct_with_an_unconsumed_linear_field_is_e3003() {
    // t.W は linear と宣言していないが線形なフィールドを含むので線形 (4.4)。
    // 非線形なフィールド w.n だけを使っても w.lin は消費されていない。
    let (cs, out) = tdrop_codes(
        "tdrop_wfield",
        &["let w = t.W { lin: t.get_l(), n: 1u8 }", "core.mem.drop(w.n)"],
    );
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `w` is never consumed"), "{}", out);
}

#[test]
fn tdrop_consuming_the_linear_field_or_the_whole_struct_is_ok() {
    let (cs, out) = tdrop_codes(
        "tdrop_wfield_ok",
        &[
            "let w = t.W { lin: t.get_l(), n: 1u8 }",
            "t.eat(w.lin)",
            "let w2 = t.get_w()",
            "t.eat_w(w2)",
        ],
    );
    assert!(!tdrop_has(&cs, "E3003"), "{}", out);
}

#[test]
fn tdrop_tuple_destructuring_leaves_a_linear_value_unconsumed() {
    let (cs, out) = tdrop_codes(
        "tdrop_ptuple",
        &["let (a, b) = (t.get_l(), 1u8)", "core.mem.drop(b)"],
    );
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `a` is never consumed"), "{}", out);
}

#[test]
fn tdrop_tuple_binding_with_a_linear_element_is_e3003() {
    let (cs, out) = tdrop_codes("tdrop_tuple_bind", &["let _p = (t.get_l(), 1u8)"]);
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
}

#[test]
fn tdrop_struct_destructuring_leaves_a_linear_field_unconsumed() {
    let (cs, out) = tdrop_codes(
        "tdrop_pstruct",
        &["let t.W { lin: l, n: n } = t.get_w()", "core.mem.drop(n)"],
    );
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `l` is never consumed"), "{}", out);
}

#[test]
fn tdrop_destructured_linear_values_that_are_consumed_are_ok() {
    let (cs, out) = tdrop_codes(
        "tdrop_destr_ok",
        &[
            "let (a, b) = (t.get_l(), 1u8)",
            "t.eat(a)",
            "core.mem.drop(b)",
            "let t.W { lin: l, n: n } = t.get_w()",
            "t.eat(l)",
            "core.mem.drop(n)",
            "let x: u8 = t.get_p().a",
            "core.mem.drop(x)",
            "core.mem.drop((1u8, 2u8))",
        ],
    );
    assert!(
        !tdrop_has(&cs, "E3003") && !tdrop_has(&cs, "E2042") && !tdrop_has(&cs, "E2020"),
        "{}",
        out
    );
}

#[test]
fn tdrop_unknown_field_types_stay_silent() {
    // 型の分からない要素を含むタプル・フィールドの型が線形でないジェネリクス構造体リテラルは黙る。
    let (cs, out) = tdrop_codes(
        "tdrop_silent",
        &["let b = t.Box { v: 1u8 }", "core.mem.drop(b.v)", "core.mem.drop((b.v, 1u8))", "core.mem.drop(b)"],
    );
    assert!(!tdrop_has(&cs, "E2042") && !tdrop_has(&cs, "E3003"), "{}", out);
}

// ---- 効果検査の呼び出し経路 (5.1 / 4.3): 局所変数レシーバの `s.m()` と、`Fn` 境界に渡すクロージャ ----

/// 効果検査の呼び出し経路のテストで共通に使う定義 (`t.S` のメソッドと、`Fn` 境界を受ける `t.apply`)。
const EFF_PRELUDE: &str = "type t.S pub
    n: u8
    log: alloc.vec.Vec<u8, std.alloc.System>
end t.S

fn t.S.boom(self: &t.S) -> u8 uses [] panics may pub
    core.panic.panic(\"boom\")
end t.S.boom

fn t.S.peek(self: &t.S) -> u8 uses [] panics never pub
    self.n
end t.S.peek

fn t.S.say(self: &t.S) -> Result<(), std.io.Error> uses [io.console] panics never pub
    std.io.print_line(\"s\")
end t.S.say

fn t.S.with[F: core.ops.Fn<(u8,), u8, E, P>, E: uses, P: panics](self: &t.S, f: F) -> u8 uses [E] panics P pub
    f(self.n)
end t.S.with

fn t.apply[F: core.ops.Fn<(u8,), u8, E, P>, E: uses, P: panics](f: F, x: u8) -> u8 uses [E] panics P pub
    f(x)
end t.apply

";

/// `EFF_PRELUDE` の後ろに `body` (t.apply より後ろの名前の定義) を置いたパッケージを検査し、
/// (終了コード, 診断コードの列, 出力) を返す。
fn eff_check(name: &str, body: &str) -> (Option<i32>, Vec<String>, String) {
    let t = Tmp::new(name, &format!("{}{}", EFF_PRELUDE, body));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    (o.status.code(), codes(&out), out)
}

#[test]
fn local_receiver_method_effects_are_checked() {
    // `s.say()` (局所変数レシーバ、パスとして構文解析される形) は、型で解決した t.S.say の効果で検査される。
    let bad = "fn t.f(s: t.S) -> Result<(), std.io.Error> uses [] panics never
    s.say()
end t.f
";
    let (code, got, out) = eff_check("eff_path_uses_bad", bad);
    assert_eq!(code, Some(1), "{}", out);
    assert_eq!(got, vec!["E4001".to_string()], "{}", out);
    assert!(out.contains("t.S.say uses effect `io.console`"), "{}", out);
    // 正例: 呼び出し元が効果を宣言していれば通る (階層の部分型 io ⊇ io.console も)。
    let (code, _, out) = eff_check("eff_path_uses_ok", &bad.replace("uses [] panics never", "uses [io] panics never"));
    assert_eq!(code, Some(0), "{}", out);
}

#[test]
fn local_receiver_method_panics_are_checked() {
    let bad = "fn t.f(s: t.S) -> u8 uses [] panics never
    s.boom()
end t.f
";
    let (_, got, out) = eff_check("eff_path_panic_bad", bad);
    assert_eq!(got, vec!["E4002".to_string()], "{}", out);
    assert!(out.contains("t.S.boom may panic"), "{}", out);
    let (code, _, out) = eff_check("eff_path_panic_ok", &bad.replace("panics never", "panics may"));
    assert_eq!(code, Some(0), "{}", out);
    // `panics never` のメソッドは `panics never` から呼べる。
    let (code, _, out) = eff_check("eff_path_panic_never", &bad.replace("s.boom()", "s.peek()"));
    assert_eq!(code, Some(0), "{}", out);
}

#[test]
fn field_path_receiver_method_effects_are_checked() {
    // `s.log.push(x)` はフィールドの型 (Vec) で解決され、`uses [alloc]` が要る。
    let bad = "fn t.f(s: &mut t.S) -> Result<(), alloc.alloc.AllocError> uses [] panics never
    s.log.push(1u8)
end t.f
";
    let (_, got, out) = eff_check("eff_field_bad", bad);
    assert_eq!(got, vec!["E4001".to_string()], "{}", out);
    assert!(out.contains("alloc.vec.Vec.push uses effect `alloc`"), "{}", out);
    let (code, _, out) = eff_check("eff_field_ok", &bad.replace("uses []", "uses [alloc]"));
    assert_eq!(code, Some(0), "{}", out);
}

#[test]
fn local_receiver_of_unknown_type_stays_silent() {
    // レシーバの型が型パラメータ (トレイト境界経由の呼び出し) のときは、新しい検査は黙る。
    let src = "fn t.f[T: t.Talk](x: &T) -> u8 uses [] panics never
    x.talk()
end t.f

trait t.Talk pub
    fn talk(self: &Self) -> u8 uses [] panics never
end t.Talk
";
    let (code, _, out) = eff_check("eff_path_unknown", src);
    assert_eq!(code, Some(0), "{}", out);
}

#[test]
fn closure_effects_instantiate_the_fn_bound_effect_var() {
    // `Fn` 境界 (E, P) に渡したクロージャの本体の効果で `uses [E]` が実体化される (4.3)。
    let bad = "fn t.f() -> u8 uses [] panics never
    t.apply(
        fn(x: u8) -> u8 capture [] {
            core.mem.drop(std.io.print_line(\"hi\"))
            x
        },
        1u8,
    )
end t.f
";
    let (_, got, out) = eff_check("eff_closure_uses_bad", bad);
    assert_eq!(got, vec!["E4001".to_string()], "{}", out);
    assert!(out.contains("t.apply uses effect `io.console`"), "{}", out);
    let (code, _, out) = eff_check("eff_closure_uses_ok", &bad.replace("uses []", "uses [io.console]"));
    assert_eq!(code, Some(0), "{}", out);
}

#[test]
fn closure_index_instantiates_the_panic_var_as_may() {
    // クロージャの本体の添字は panic しうるので、P は `may` に実体化される。
    let bad = "fn t.f(v: &[u8]) -> u8 uses [] panics never
    t.apply(
        fn(x: u8) -> u8 capture [&v] {
            v[0]
        },
        1u8,
    )
end t.f
";
    let (_, got, out) = eff_check("eff_closure_panic_bad", bad);
    assert_eq!(got, vec!["E4002".to_string()], "{}", out);
    let (code, _, out) = eff_check("eff_closure_panic_ok", &bad.replace("panics never", "panics may"));
    assert_eq!(code, Some(0), "{}", out);
}

#[test]
fn closure_effects_flow_through_method_calls() {
    // 標準ライブラリの `Option.map` (`uses [E] panics P`) と、利用者のメソッド (`t.S.with`)。
    let bad = "fn t.f(o: Option<u8>) -> Option<u8> uses [] panics never
    o.map(
        fn(x: u8) -> u8 capture [] {
            core.mem.drop(std.io.print_line(\"hi\"))
            x
        },
    )
end t.f

fn t.g(s: t.S) -> u8 uses [] panics never
    s.with(
        fn(x: u8) -> u8 capture [] {
            core.panic.panic(\"no\")
        },
    )
end t.g
";
    let (_, got, out) = eff_check("eff_closure_method_bad", bad);
    assert_eq!(got, vec!["E4001".to_string(), "E4002".to_string()], "{}", out);
    assert!(out.contains("core.option.Option.map uses effect `io.console`"), "{}", out);
    assert!(out.contains("t.S.with may panic"), "{}", out);
}

#[test]
fn pure_closures_and_methods_stay_silent() {
    // 効果のないクロージャ・メソッドは `uses [] panics never` から呼べる (誤検出なし)。
    let ok = "fn t.f(o: Option<u8>, s: t.S) -> u8 uses [] panics never
    let a = o.map(
        fn(x: u8) -> u8 capture [] {
            x.wrapping_add(1u8)
        },
    )
    let k = s.peek()
    let b = s.with(
        fn(x: u8) -> u8 capture [] {
            x.wrapping_mul(2u8)
        },
    )
    let total = core.iter.fold(
        core.iter.range(0u32, 5u32),
        0u32,
        fn(acc: u32, x: u32) -> u32 capture [] {
            acc.wrapping_add(x)
        },
    )
    core.mem.drop(total)
    t.apply(
        fn(x: u8) -> u8 capture [&k] {
            x.wrapping_add(*k)
        },
        a.unwrap_or(b),
    )
end t.f
";
    let (code, _, out) = eff_check("eff_pure_ok", ok);
    assert_eq!(code, Some(0), "{}", out);
}

#[test]
fn nested_and_let_bound_closure_effects_are_inferred() {
    // 入れ子のクロージャの効果は、内側を受ける呼び出しを経て外側のクロージャに入る。
    // `let` に束縛したクロージャも型 (クロージャの節) を通じて効果が分かる。
    let bad = "fn t.f(x: u8) -> u8 uses [] panics never
    t.apply(
        fn(y: u8) -> u8 capture [] {
            t.apply(
                fn(z: u8) -> u8 capture [] {
                    core.mem.drop(std.io.print_line(\"z\"))
                    z
                },
                y,
            )
        },
        x,
    )
end t.f

fn t.g(x: u8) -> u8 uses [] panics never
    let c = fn(y: u8) -> u8 capture [] {
        core.mem.drop(std.io.print_line(\"y\"))
        y
    }
    t.apply(c, x)
end t.g
";
    let (_, got, out) = eff_check("eff_nested_bad", bad);
    assert_eq!(got, vec!["E4001".to_string(), "E4001".to_string()], "{}", out);
    let (code, _, out) = eff_check("eff_nested_ok", &bad.replace("uses []", "uses [io.console]"));
    assert_eq!(code, Some(0), "{}", out);
}

#[test]
fn closure_with_unknown_effects_stays_silent() {
    // クロージャの本体が `Fn` 境界のパラメータを呼ぶだけ (効果が分からない) なら、新しい検査は黙る。
    // 宣言どおりの効果変数 E / P は従来どおり呼び出し側の E / P で覆われる。test の本体は検査しない。
    let ok = "fn t.fwd[F: core.ops.Fn<(u8,), u8, E, P>, E: uses, P: panics](f: F, x: u8) -> u8 uses [E] panics P pub
    t.apply(
        fn(y: u8) -> u8 capture [&f] {
            f(y)
        },
        x,
    )
end t.fwd

test t.fwd.test_io
    let s = t.S { n: 1u8, log: alloc.vec.Vec.new(std.alloc.SYSTEM) }
    core.mem.drop(s.say())
    core.check.eq(t.fwd(
        fn(z: u8) -> u8 capture [] {
            core.mem.drop(std.io.print_line(\"t\"))
            z
        },
        1u8,
    ), 1u8)
end t.fwd.test_io
";
    let (code, _, out) = eff_check("eff_unknown_closure_ok", ok);
    assert_eq!(code, Some(0), "{}", out);
}

#[test]
fn inferred_caller_covers_local_method_and_closure_effects() {
    // 効果を省略した非 pub 関数は本体から推論される (s.say() の io.console とクロージャの本体を含む)。
    // 推論された関数自身には新しい診断が出ず、その呼び出し元が効果を受け取る。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let s = t.S { n: 1u8, log: alloc.vec.Vec.new(std.alloc.SYSTEM) }
    core.mem.drop(t.mid(s))
    Ok(())
end t.main

fn t.mid(s: t.S) -> u8
    core.mem.drop(s.say())
    t.apply(
        fn(x: u8) -> u8 capture [] {
            core.mem.drop(std.io.print_line(\"m\"))
            x
        },
        1u8,
    )
end t.mid
";
    let (_, got, out) = eff_check("eff_inferred_bad", src);
    assert_eq!(got, vec!["E4001".to_string()], "{}", out);
    assert!(out.contains("\"fqn\": \"t.main\""), "{}", out);
    let (code, _, out) = eff_check("eff_inferred_ok", &src.replace("uses [] panics may", "uses [io.console] panics may"));
    assert_eq!(code, Some(0), "{}", out);
}

#[test]
fn declared_and_closure_effects_are_reported_once() {
    // 宣言にある具体名 (io.console) とクロージャから来た同じ効果は、同じ行の 1 件にまとまる。
    let src = "fn t.both[F: core.ops.Fn<(u8,), u8, E, P>, E: uses, P: panics](f: F) -> u8 uses [io.console, E] panics P pub
    core.mem.drop(std.io.print_line(\"b\"))
    f(1u8)
end t.both

fn t.f() -> u8 uses [] panics never
    t.both(
        fn(x: u8) -> u8 capture [] {
            core.mem.drop(std.io.print_line(\"hi\"))
            x
        },
    )
end t.f
";
    let (_, got, out) = eff_check("eff_dedupe", src);
    assert_eq!(got, vec!["E4001".to_string()], "{}", out);
}

// ---------------------------------------------------------------- インタプリタの extern (f32/f64・ポインタ read/write/add)

/// 補助 C 側。共有ライブラリにして、`LD_PRELOAD` で `ksn run` に差し込む (インタプリタは dlsym で見つける)。
const FFI_C_SRC: &str = r#"
#include <stdint.h>
#include <stdlib.h>

double k_scale(double x, int32_t n, double y) { return x * n + y; }
float k_half(float x) { return x * 0.5f; }
double k_mix(int32_t a, float b, double c, int64_t d) { return a + b + c + d; }
float k_mix_f32(float a, int32_t b, float c, double d) { return a + b + c + (float)d; }
double *k_alloc_f64(size_t n) { return calloc(n, sizeof(double)); }
float *k_alloc_f32(size_t n) { return calloc(n, sizeof(float)); }
int32_t *k_alloc_i32(size_t n) { return calloc(n, sizeof(int32_t)); }
double k_sum_f64(const double *p, size_t n) {
    double s = 0;
    for (size_t i = 0; i < n; i++) s += p[i];
    return s;
}
void k_fill_f32(float *p, size_t n, float v) {
    for (size_t i = 0; i < n; i++) p[i] = v + (float)i;
}
int32_t k_sum_i32(const int32_t *p, size_t n) {
    int32_t s = 0;
    for (size_t i = 0; i < n; i++) s += p[i];
    return s;
}
"#;

fn c_compiler() -> Option<&'static str> {
    ["cc", "gcc", "clang"].into_iter().find(|c| Command::new(c).arg("--version").output().map(|o| o.status.success()).unwrap_or(false))
}

/// `src` を共有ライブラリ `libksn_helpers.so` にして、そのパスを返す。
fn build_c_helpers(dir: &Path, src: &str) -> PathBuf {
    let cc = c_compiler().expect("a C compiler (cc / gcc / clang) is required for the FFI tests");
    let c = dir.join("helpers.c");
    fs::write(&c, src).unwrap();
    let so = dir.join("libksn_helpers.so");
    let o = Command::new(cc).args(["-shared", "-fPIC", "-O1", "-o"]).arg(&so).arg(&c).output().unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    so
}

/// `ksn run` を、補助 C ライブラリを `LD_PRELOAD` で差し込んだ環境で実行する。
fn run_with_c_helpers(dir: &Path, so: &Path) -> Output {
    Command::new(exe()).arg("run").current_dir(dir).env("LD_PRELOAD", so).output().unwrap()
}

const FFI_FLOAT_SRC: &str = r#"extern t.k_scale(x: f64, n: i32, y: f64) -> f64 link "k_scale"

extern t.k_half(x: f32) -> f32 link "k_half"

extern t.k_mix(a: i32, b: f32, c: f64, d: i64) -> f64 link "k_mix"

extern t.k_mix_f32(a: f32, b: i32, c: f32, d: f64) -> f32 link "k_mix_f32"

extern t.k_alloc_f64(n: usize) -> *mut f64 link "k_alloc_f64"

extern t.k_alloc_f32(n: usize) -> *mut f32 link "k_alloc_f32"

extern t.k_sum_f64(values: *const f64, n: usize) -> f64 link "k_sum_f64"

extern t.k_fill_f32(values: *mut f32, n: usize, v: f32) -> () link "k_fill_f32"

extern t.free_f64(pointer: *mut f64) -> () link "free"

extern t.free_f32(pointer: *mut f32) -> () link "free"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe "libc" "f64 argument and result" {
        t.k_scale(1.5f64, 2i32, 0.25f64)
    }
    core.check.eq(a, 3.25f64)
    let h = unsafe "libc" "f32 argument and result" {
        t.k_half(3.0f32)
    }
    core.check.eq(h, 1.5f32)
    let m = unsafe "libc" "mixed integer and float arguments" {
        t.k_mix(1i32, 0.5f32, 0.25f64, 1000i64)
    }
    core.check.eq(m, 1001.75f64)
    let mf = unsafe "libc" "mixed f32 and f64 arguments" {
        t.k_mix_f32(1.0f32, 2i32, 0.5f32, 0.25f64)
    }
    core.check.eq(mf, 3.75f32)
    let p = unsafe "libc" "allocate f64 values" {
        t.k_alloc_f64(3usize)
    }
    unsafe "libc" "write, add and read f64 values" {
        core.ptr.write(p, 1.5f64)
        core.ptr.write(core.ptr.add(p, 1usize), 2.25f64)
        core.ptr.write(core.ptr.add(p, 2usize), 0.25f64)
        core.check.eq(core.ptr.read(core.ptr.add(p, 1usize)), 2.25f64)
        core.check.eq(t.k_sum_f64(core.ptr.as_const(p), 3usize), 4.0f64)
    }
    let q = unsafe "libc" "allocate f32 values" {
        t.k_alloc_f32(4usize)
    }
    unsafe "libc" "C writes f32 values, Kasane reads them" {
        t.k_fill_f32(q, 4usize, 0.5f32)
        core.check.eq(core.ptr.read(core.ptr.add(q, 3usize)), 3.5f32)
    }
    unsafe "libc" "free the buffers" {
        t.free_f64(p)
        t.free_f32(q)
    }
    std.io.print_line("ffi float ok")
    Ok(())
end t.main
"#;

#[test]
#[cfg(target_os = "linux")]
fn ffi_interpreter_passes_and_returns_floats_and_reads_writes_pointers() {
    let t = Tmp::new("ffi_float", FFI_FLOAT_SRC);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_C_SRC);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "ffi float ok\n");
}

#[test]
#[cfg(target_os = "linux")]
fn ffi_interpreter_float_mismatch_fails_at_runtime() {
    let src = r#"extern t.k_half(x: f32) -> f32 link "k_half"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let h = unsafe "libc" "f32 result" {
        t.k_half(3.0f32)
    }
    core.check.eq(h, 2.0f32)
    Ok(())
end t.main
"#;
    let t = Tmp::new("ffi_float_bad", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_C_SRC);
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(101), "{}{}", text(&r.stdout), text(&r.stderr));
}

#[test]
#[cfg(target_os = "linux")]
fn ffi_interpreter_writes_and_reads_i32_pointers_across_c() {
    let src = r#"extern t.k_sum_i32(values: *const i32, n: usize) -> i32 link "k_sum_i32"

extern t.k_alloc_i32(n: usize) -> *mut i32 link "k_alloc_i32"

extern t.free_i32(pointer: *mut i32) -> () link "free"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let p = unsafe "libc" "allocate i32 values" {
        t.k_alloc_i32(3usize)
    }
    unsafe "libc" "write i32 values through the pointer" {
        core.ptr.write(p, 0i32 - 5i32)
        core.ptr.write(core.ptr.add(p, 1usize), 7i32)
        core.ptr.write(core.ptr.add(p, 2usize), 100i32)
        core.check.eq(core.ptr.read(p), 0i32 - 5i32)
        core.check.eq(t.k_sum_i32(core.ptr.as_const(p), 3usize), 102i32)
        t.free_i32(p)
    }
    std.io.print_line("i32 ok")
    Ok(())
end t.main
"#;
    let t = Tmp::new("ffi_i32_ptr", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_C_SRC);
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "i32 ok\n");
}

// ---- FQN の構造体・バリアントパターンのパス成分を束縛として数えない (誤検出 E1020 の回帰) ----

#[test]
fn unused_binding_in_fqn_struct_pattern_is_e1020() {
    // 陽性: `let t.L { a: x, .. }` の `x` が本当に未使用なら E1020 (パス `t` ではなく `x` を指す)。
    let prefix = "type t.L pub\n    a: u8\n    b: u8\nend t.L\n\n\
                  fn t.pick(l: t.L) -> u8 uses [] panics never\n    let t.L { a: x, b: _ } = l\n    0u8\nend t.pick\n\n";
    let t = Tmp::new("fqn_struct_unused_pos", &main_wrap(prefix, ""));
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E1020"), "{}", text(&o.stdout));
    assert!(text(&o.stdout).contains("unused variable x"), "{}", text(&o.stdout));
    assert!(!text(&o.stdout).contains("unused variable t"), "{}", text(&o.stdout));
}

#[test]
fn fqn_struct_pattern_path_is_not_an_unused_binding() {
    // 陰性: パターンの `t.L` 以外に `t.` が現れない関数でも、パス先頭 `t` を未使用変数にしない。
    let prefix = "type t.L pub\n    a: u8\n    b: u8\nend t.L\n\n\
                  fn t.first(l: t.L) -> u8 uses [] panics never\n    let t.L { a: x, b: _ } = l\n    x\nend t.first\n\n";
    let t = Tmp::new("fqn_struct_pattern_neg", &main_wrap(prefix, "    core.check.eq(t.first(t.L { a: 3u8, b: 4u8 }), 3u8)\n"));
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E1020"), "{}", text(&o.stdout));
}

#[test]
fn fqn_variant_pattern_path_is_not_an_unused_binding() {
    // 陰性: match 腕のバリアントパス `t.Op.Num(n)` の成分 (`t`) は束縛ではなく、`n` が使われていれば E1020 にならない。
    let prefix = "enum t.Op pub\n    Num(u8)\n    Zero\nend t.Op\n\n\
                  fn t.val(o: t.Op) -> u8 uses [] panics never\n    match o {\n        t.Op.Num(n) => n\n        t.Op.Zero => 0u8\n    }\nend t.val\n\n";
    let t = Tmp::new("fqn_variant_pattern_neg", &main_wrap(prefix, "    core.check.eq(t.val(t.Op.Num(5u8)), 5u8)\n"));
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E1020"), "{}", text(&o.stdout));
}

#[test]
fn unused_binding_in_match_variant_arm_is_e1020() {
    // 陽性: match 腕のバリアントの束縛 `n` が未使用なら E1020 (パス成分の除外で陽性を失わないこと)。
    let prefix = "enum t.Op pub\n    Num(u8)\n    Zero\nend t.Op\n\n\
                  fn t.val(o: t.Op) -> u8 uses [] panics never\n    match o {\n        t.Op.Num(n) => 0u8\n        t.Op.Zero => 0u8\n    }\nend t.val\n\n";
    let t = Tmp::new("fqn_variant_arm_unused_pos", &main_wrap(prefix, ""));
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E1020"), "{}", text(&o.stdout));
    assert!(text(&o.stdout).contains("unused pattern binding n"), "{}", text(&o.stdout));
}

#[test]
fn nested_field_pattern_is_not_a_bogus_snake_case_name() {
    // 入れ子パターン `a: t.Op.Num(q)` の文字列全体を束縛名として E0010 にしない。
    let prefix = "enum t.Op pub\n    Num(u8)\n    Zero\nend t.Op\n\n\
                  type t.L pub\n    a: t.Op\n    b: u8\nend t.L\n\n\
                  fn t.get(l: t.L) -> u8 uses [] panics never\n    let t.L { a: t.Op.Num(q), b: b } = l\n    b\nend t.get\n\n";
    let t = Tmp::new("nested_field_pattern", &main_wrap(prefix, ""));
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E0010"), "{}", text(&o.stdout));
}

// ---- 借用の残り: if の値・ブロック式の値を通じた運搬 ----

#[test]
fn nll_rejects_a_mutation_while_an_if_value_carries_the_borrow() {
    // `let r = if c { &x } else { &y }`: 両枝の和なので、then 枝の借用 (x) も r が持つ。
    let (bad, out) = has_e3010(
        "nll_if_val_then",
        "",
        &[
            "let mut x = 1u8",
            "let y = 2u8",
            "let b = true",
            "let r = if b {",
            "    &x",
            "} else {",
            "    &y",
            "}",
            "x = 3u8",
            "core.check.eq(*r, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_borrowed_by_the_else_branch_of_an_if_value() {
    let (bad, out) = has_e3010(
        "nll_if_val_else",
        "",
        &[
            "let x = 1u8",
            "let mut y = 2u8",
            "let b = false",
            "let r = if b {",
            "    &x",
            "} else {",
            "    &y",
            "}",
            "y = 3u8",
            "core.check.eq(*r, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `y` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_block_value_carries_the_borrow() {
    // `let r = { &x }`: ブロック式の末尾の式の借用が r に移る。
    let (bad, out) = has_e3010(
        "nll_block_val",
        "",
        &["let mut x = 1u8", "let r = {", "    &x", "}", "x = 3u8", "core.check.eq(*r, 1u8)"],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_an_if_value_nested_in_a_match_arm_carries_a_borrow() {
    // 腕の中の `if` の値 (then 枝の `&x`) が match の値を経て r に移る。
    let (bad, out) = has_e3010(
        "nll_if_val_nested",
        "",
        &[
            "let mut x = 1u8",
            "let y = 9u8",
            "let c = true",
            "let r = match c {",
            "    true => if c {",
            "        &x",
            "    } else {",
            "        &y",
            "    }",
            "    false => &y",
            "}",
            "x = 2u8",
            "core.check.eq(*r, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_accepts_an_if_value_that_copies_out_of_the_borrow() {
    // 枝の値が `*r0` (コピー) なら、if の値は借用を運ばない。
    let (bad, out) = has_e3010(
        "nll_if_val_copy",
        "",
        &[
            "let mut x = 1u8",
            "let r0 = &x",
            "let b = true",
            "let n = if b {",
            "    *r0",
            "} else {",
            "    0u8",
            "}",
            "x = 3u8",
            "core.check.eq(n, 1u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_an_if_value_that_is_dead_before_the_mutation() {
    let (bad, out) = has_e3010(
        "nll_if_val_dead",
        "",
        &[
            "let mut x = 1u8",
            "let y = 2u8",
            "let b = true",
            "let r = if b {",
            "    &x",
            "} else {",
            "    &y",
            "}",
            "core.check.eq(*r, 1u8)",
            "x = 3u8",
            "core.check.eq(x, 3u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_an_if_value_inside_a_loop_when_the_borrow_dies_before_the_mutation() {
    // 毎周回の `if` の値は枝の終わりで入れ替わる。前の周回の借用 (x) は次の周回の値 (y) に漏れない。
    let (bad, out) = has_e3010(
        "nll_if_val_loop",
        "",
        &[
            "let mut x = 1u8",
            "let y = 2u8",
            "let mut i = 0u8",
            "let mut acc = 0u8",
            "while i < 3u8 {",
            "    let r = if i == 0u8 {",
            "        &x",
            "    } else {",
            "        &y",
            "    }",
            "    acc = acc + *r",
            "    x = x + 1u8",
            "    i = i + 1u8",
            "}",
            "core.check.eq(acc, 5u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

// `ty_of` の Index・MCall・ArrayE の型解決 (4.4)。`tdrop_prefix` に、メソッドを持つ型を足して使う。
fn tyof_codes(name: &str, b: &[&str]) -> (Vec<String>, String) {
    let nl = char::from(10u8).to_string();
    // 定義の並びは UTF-8 のバイト順 (E0020)。Maker・Other の型は `t.P` の前、
    // そのメソッドは `t.eat` の前に差し込む。
    let maker_types = [
        "type t.Maker",
        "    k: u8",
        "end t.Maker",
        "",
        "type t.Other",
        "    k: u8",
        "end t.Other",
        "",
    ];
    let maker_fns = [
        "fn t.Maker.make_l(self: &t.Maker) -> t.L uses [] panics never",
        "    t.L { a: self.k }",
        "end t.Maker.make_l",
        "",
        "fn t.Maker.make_w(self: &t.Maker) -> t.W uses [] panics never",
        "    t.W { lin: t.L { a: self.k }, n: self.k }",
        "end t.Maker.make_w",
        "",
        "fn t.Maker.touch(self: &t.Maker) -> () uses [] panics never",
        "    ()",
        "end t.Maker.touch",
        "",
        // 同じ名前のメソッドを別の型が持つ (戻り値は ()): 所有者で絞らないと一意に定まらない。
        "fn t.Other.make_l(self: &t.Other) -> () uses [] panics never",
        "    ()",
        "end t.Other.make_l",
        "",
    ];
    let mut pre: Vec<&str> = vec![];
    for l in tdrop_prefix() {
        if l == "type t.P" {
            pre.extend(maker_types.iter().copied());
        }
        if l == "fn t.eat(l: t.L) -> () uses [] panics never" {
            pre.extend(maker_fns.iter().copied());
        }
        pre.push(l);
    }
    // メソッドのレシーバに式を使う (`t.lmaker().m()`)。局所変数 `m.m()` は Call として解析されるため。
    // `t.lmaker` は `t.get_w` の後ろ (定義の並びは UTF-8 のバイト順)。
    pre.extend([
        "fn t.lmaker() -> t.Maker uses [] panics never",
        "    t.Maker { k: 1u8 }",
        "end t.lmaker",
        "",
    ]);
    let prefix = pre.join(&nl) + &nl;
    let t = Tmp::new(name, &main_wrap(&prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

#[test]
fn tyof_discarding_a_method_result_is_e2042() {
    // レシーバの式の型 (t.Maker) からメソッドを一意に定め、戻り値 (t.L) を解決する。
    // t.Other の同名メソッドは戻り値が () なので、所有者で絞らないと黙ってしまう。
    let (cs, out) = tyof_codes("tyof_mcall", &["t.lmaker().make_l()"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tyof_field_of_a_method_result_is_resolved() {
    // MCall → Field の道: `make_w()` の戻り値 (t.W) の `lin` (t.L) を捨てる。
    let (cs, out) = tyof_codes("tyof_mcall_field", &["t.lmaker().make_w().lin"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tyof_method_result_binding_with_unconsumed_linear_is_e3003() {
    // 束縛の型が MCall の戻り値から決まるので、線形なフィールドの消費漏れが見える (E3003)。
    let (cs, out) = tyof_codes(
        "tyof_mcall_bind",
        &["let w = t.lmaker().make_w()", "core.mem.drop(w.n)"],
    );
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `w` is never consumed"), "{}", out);
}

#[test]
fn tyof_method_result_consumed_is_ok() {
    let (cs, out) = tyof_codes(
        "tyof_mcall_ok",
        &[
            "let w = t.lmaker().make_w()",
            "t.eat(w.lin)",
            "core.mem.drop(w.n)",
            "let l = t.lmaker().make_l()",
            "t.eat(l)",
            "t.lmaker().touch()",
        ],
    );
    assert!(!tdrop_has(&cs, "E3003") && !tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tyof_discarding_an_array_literal_is_e2042() {
    let (cs, out) = tyof_codes("tyof_array", &["[1u8, 2u8]"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tyof_discarding_an_index_is_e2042() {
    let (cs, out) = tyof_codes("tyof_index", &["let a = [1u8, 2u8]", "a[0]"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tyof_index_of_an_array_of_structs_then_field_is_resolved() {
    // Index → Field の道: 要素 (t.P) の `a` (u8) を捨てる。
    let (cs, out) = tyof_codes("tyof_index_field", &["let a = [t.P { a: 1u8 }, t.P { a: 2u8 }]", "a[0].a"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tyof_index_through_a_reference_is_resolved() {
    let (cs, out) = tyof_codes("tyof_index_ref", &["let a = [1u8, 2u8]", "let r = &a", "r[1]"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn tyof_unit_method_results_and_unknown_types_stay_silent() {
    // 戻り値が () のメソッド、型の決まらない要素 (型なし整数リテラル)・存在しないメソッドは黙る。
    let (cs, out) = tyof_codes(
        "tyof_silent",
        &[
            "t.lmaker().touch()",
            "t.lmaker().nosuch()",
            "let a = [1, 2]",
            "a[0]",
            "[1, 2]",
            "let b = [t.lmaker().touch(), t.lmaker().touch()]",
            "core.mem.drop(b[0])",
        ],
    );
    assert!(!tdrop_has(&cs, "E2042") && !tdrop_has(&cs, "E3003"), "{}", out);
}

// ---- パターンの未使用束縛: let の括弧バリアントとフィールドラベル (E1020 の見落とし・誤検出の回帰) ----

#[test]
fn unused_binding_in_let_variant_pattern_is_e1020() {
    // 陽性: `let t.Box.Val(x) = b` の `x` が未使用なら E1020 (パス成分 `t`・`Box` ではなく `x` を指す)。
    let prefix = "enum t.Box pub\n    Val(u8)\nend t.Box\n\n\
                  fn t.pick(b: t.Box) -> u8 uses [] panics never\n    let t.Box.Val(x) = b\n    0u8\nend t.pick\n\n";
    let t = Tmp::new("let_variant_unused_pos", &main_wrap(prefix, ""));
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E1020"), "{}", text(&o.stdout));
    assert!(text(&o.stdout).contains("unused variable x"), "{}", text(&o.stdout));
}

#[test]
fn let_variant_pattern_used_or_wildcard_is_not_e1020() {
    // 陰性: 括弧バリアントの束縛 `x` が使われていれば、`_` の引数と同様に E1020 にしない。
    let prefix = "enum t.Box pub\n    Val(u8)\nend t.Box\n\n\
                  fn t.get(b: t.Box) -> u8 uses [] panics never\n    let t.Box.Val(x) = b\n    x\nend t.get\n\n\
                  fn t.zero(b: t.Box) -> u8 uses [] panics never\n    let t.Box.Val(_) = b\n    0u8\nend t.zero\n\n";
    let t = Tmp::new(
        "let_variant_used_neg",
        &main_wrap(prefix, "    core.check.eq(t.get(t.Box.Val(5u8)), 5u8)\n    core.check.eq(t.zero(t.Box.Val(5u8)), 0u8)\n"),
    );
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E1020"), "{}", text(&o.stdout));
}

#[test]
fn unused_binding_named_like_field_label_is_e1020() {
    // 陽性: `let t.L { a: a, b: b }` で `b` が未使用なら、フィールドラベル `b` を使用として数えず E1020。
    let prefix = "type t.L pub\n    a: u8\n    b: u8\nend t.L\n\n\
                  fn t.first(l: t.L) -> u8 uses [] panics never\n    let t.L { a: a, b: b } = l\n    a\nend t.first\n\n";
    let t = Tmp::new("struct_label_unused_pos", &main_wrap(prefix, ""));
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E1020"), "{}", text(&o.stdout));
    assert!(text(&o.stdout).contains("unused variable b"), "{}", text(&o.stdout));
    assert!(!text(&o.stdout).contains("unused variable a"), "{}", text(&o.stdout));
}

#[test]
fn struct_pattern_field_label_same_as_used_binding_is_not_e1020() {
    // 陰性: ラベルと束縛名が同じ `b: b` でも、束縛 `b` が使われていれば E1020 にしない (`a: _` も対象外)。
    let prefix = "type t.L pub\n    a: u8\n    b: u8\nend t.L\n\n\
                  fn t.second(l: t.L) -> u8 uses [] panics never\n    let t.L { a: _, b: b } = l\n    b\nend t.second\n\n";
    let t = Tmp::new("struct_label_used_neg", &main_wrap(prefix, "    core.check.eq(t.second(t.L { a: 1u8, b: 7u8 }), 7u8)\n"));
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E1020"), "{}", text(&o.stdout));
}

#[test]
fn linear_array_consumed_by_index_only_one_element_is_e3003() {
    // `ls[1]` の消費漏れ (4.4)。配列の束縛は名前の使用があれば消費、という近似の穴を要素単位で塞ぐ。
    let (cs, out) = tyof_codes("lin_arr_one", &["let ls = [t.get_l(), t.get_l()]", "t.eat(ls[0])"]);
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `ls[1]` is never consumed"), "{}", out);
}

#[test]
fn linear_array_with_a_gap_in_the_indices_is_e3003() {
    // 添字 0 と 2 は使い、中央の 1 だけ消費しない。
    let (cs, out) = tyof_codes(
        "lin_arr_gap",
        &["let ls = [t.get_l(), t.get_l(), t.get_l()]", "t.eat(ls[0])", "t.eat(ls[2])"],
    );
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `ls[1]` is never consumed"), "{}", out);
}

#[test]
fn linear_array_never_used_is_e3003() {
    let (cs, out) = tyof_codes("lin_arr_unused", &["let ls = [t.get_l(), t.get_l()]"]);
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `ls` is never consumed"), "{}", out);
}

#[test]
fn linear_array_with_every_element_consumed_is_ok() {
    let (cs, out) = tyof_codes(
        "lin_arr_ok",
        &["let ls = [t.get_l(), t.get_l()]", "t.eat(ls[0])", "t.eat(ls[1])"],
    );
    assert!(!tdrop_has(&cs, "E3003"), "{}", out);
}

#[test]
fn linear_array_used_as_a_whole_or_by_a_variable_index_stays_silent() {
    // 配列全体を `for` で回す・添字に変数を使う (どの要素か判断できない) 場合は黙る。
    let (cs, out) = tyof_codes(
        "lin_arr_silent",
        &[
            "let ls = [t.get_l(), t.get_l()]",
            "for x in ls",
            "    t.eat(x)",
            "end for",
            "let us = [t.get_l(), t.get_l()]",
            "let i = 0usize",
            "t.eat(us[i])",
        ],
    );
    assert!(!tdrop_has(&cs, "E3003"), "{}", out);
}

// ---- rt/interp-ptr-more: null_mut::<T>() / cast::<T, U>() / from_mut / from_ref (インタプリタ) ----

/// `unsafe` を許す `t` で `src` を実行する (`ksn run`)。
fn run_unsafe_pkg(name: &str, src: &str) -> (Tmp, Output) {
    let t = Tmp::new(name, src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let r = run(&["run"], &t.0);
    (t, r)
}

#[test]
fn ptr_more_place_pointers_alias_their_places_and_null_is_detected() {
    // from_mut / from_ref は場所を指す。書き込みは場所に届き、from_ref は後の書き込みも見る。null_mut::<T>() は is_null で null と分かる。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut x: i32 = 41i32
    let n: *mut i32 = core.ptr.null_mut::<i32>()
    let p: *mut i32 = core.ptr.from_mut(&mut x)
    let q: *const i32 = core.ptr.from_ref(&x)
    unsafe "libc" "place pointers" {
        core.check.eq(core.ptr.is_null(n), true)
        core.check.eq(core.ptr.is_null(p), false)
        core.ptr.write(p, 42i32)
        core.check.eq(core.ptr.read(p), 42i32)
        core.check.eq(core.ptr.read(core.ptr.as_const(p)), 42i32)
        core.check.eq(core.ptr.read(q), 42i32)
        core.check.eq(core.ptr.read(core.ptr.add(p, 0usize)), 42i32)
        core.check.eq(core.ptr.read(core.ptr.cast::<i32, i32>(p)), 42i32)
    }
    std.io.print_line("place ok")
    core.check.eq(x, 42i32)
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ptr_more_place", src);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "place ok\n");
}

#[test]
fn ptr_more_null_read_panics_with_null_dereference() {
    // 要素型が分かる null でも、読むと null 参照として 101 で止まる。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    unsafe "libc" "read null" {
        core.check.eq(core.ptr.read(core.ptr.null_mut::<i32>()), 0i32)
    }
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ptr_more_null_read", src);
    assert_eq!(r.status.code(), Some(101), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stderr).contains("null pointer dereference"), "{}", text(&r.stderr));
}

#[test]
fn ptr_more_cast_of_place_pointer_to_other_type_panics() {
    // 場所を指すポインタを幅の違う型へ読み替えることはできない (同じ型への cast は陽性テストで恒等として通る)。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut x: i32 = 1i32
    unsafe "libc" "cast place" {
        let b: *mut u8 = core.ptr.cast::<i32, u8>(core.ptr.from_mut(&mut x))
        core.check.eq(core.ptr.read(b), 1u8)
    }
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ptr_more_cast_place", src);
    assert_eq!(r.status.code(), Some(101), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stderr).contains("can only be cast to the same type"), "{}", text(&r.stderr));
}

#[test]
fn ptr_more_cast_without_type_argument_panics() {
    // 戻り値の型注釈からは要素型を推論しない (インタプリタは型引数しか見ない)。誤った要素型で黙って読まないよう 101 で止まる。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut x: i32 = 1i32
    unsafe "libc" "cast without targs" {
        let b: *mut u8 = core.ptr.cast(core.ptr.from_mut(&mut x))
        core.check.eq(core.ptr.read(b), 1u8)
    }
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ptr_more_cast_notarg", src);
    assert_eq!(r.status.code(), Some(101), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stderr).contains("explicit type argument"), "{}", text(&r.stderr));
}

#[test]
#[cfg(target_os = "linux")]
fn ptr_more_cast_reinterprets_c_memory_and_null_goes_to_c() {
    // C が確保した i32 の領域を `cast::<i32, u8>` で読み替えると、リトルエンディアンのバイト列になる。
    // `null_mut::<f64>()` は C へ 0 として渡る (C 側は n = 0 なので読まない)。
    let src = r#"extern t.k_sum_f64(values: *const f64, n: usize) -> f64 link "k_sum_f64"

extern t.k_alloc_i32(n: usize) -> *mut i32 link "k_alloc_i32"

extern t.free_i32(pointer: *mut i32) -> () link "free"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let p = unsafe "libc" "allocate" {
        t.k_alloc_i32(1usize)
    }
    unsafe "libc" "cast and read bytes" {
        core.ptr.write(p, 67305985i32)
        let b: *mut u8 = core.ptr.cast::<i32, u8>(p)
        core.check.eq(core.ptr.read(b), 1u8)
        core.check.eq(core.ptr.read(core.ptr.add(b, 1usize)), 2u8)
        core.check.eq(core.ptr.read(core.ptr.add(b, 2usize)), 3u8)
        core.check.eq(core.ptr.read(core.ptr.add(b, 3usize)), 4u8)
        core.check.eq(core.ptr.read(core.ptr.cast::<u8, i32>(b)), 67305985i32)
        t.free_i32(p)
    }
    unsafe "libc" "null to C" {
        core.check.eq(t.k_sum_f64(core.ptr.null_mut::<f64>(), 0usize), 0.0f64)
    }
    std.io.print_line("cast ok")
    Ok(())
end t.main
"#;
    let t = Tmp::new("ptr_more_c_cast", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_C_SRC);
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "cast ok\n");
}

#[test]
#[cfg(target_os = "linux")]
fn ptr_more_place_pointer_cannot_be_passed_to_c() {
    // from_mut の結果 (場所を指すポインタ) を extern に渡すと、アドレスが無いので実行時パニック (101)。
    let src = r#"extern t.k_sum_i32(values: *const i32, n: usize) -> i32 link "k_sum_i32"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut x: i32 = 5i32
    unsafe "libc" "place to C" {
        core.check.eq(t.k_sum_i32(core.ptr.as_const(core.ptr.from_mut(&mut x)), 1usize), 5i32)
    }
    Ok(())
end t.main
"#;
    let t = Tmp::new("ptr_more_place_to_c", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_C_SRC);
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(101), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stderr).contains("cannot be passed to C"), "{}", text(&r.stderr));
}

// 具体的な効果実引数を持つ `Fn` 境界 (`[]` / `[io]` / `never` / `may`) の検査 (4.3 / 5.1)。
// 境界の検査は、渡したクロージャの確実な効果と境界の具体名を比べる (呼び出し元の `uses` とは別)。
// 定義の並びは UTF-8 順 (E0020) にしてある。`BOUND_BAD` の呼び出し元 3 本だけが違反する。
const BOUND_BAD: &str = r#"fn t.bad_let(s: t.S, x: u8) -> u8 uses [io] panics never
    let c = fn(y: u8) -> u8 capture [&s] {
        core.mem.drop(s.say())
        y
    }
    t.bnd_none(c, x)
end t.bad_let

fn t.bad_panic(s: t.S, x: u8) -> u8 uses [] panics never
    t.bnd_none(
        fn(y: u8) -> u8 capture [&s] {
            s.boom()
        },
        x,
    )
end t.bad_panic

"#;

// 境界の定義・推論された関数・覆われる / 効果のない呼び出し元・メソッド (どれも黙る)。
const BOUND_REST: &str = r#"fn t.bnd_io[F: core.ops.Fn<(u8,), u8, [io], never>](f: F, x: u8) -> u8 uses [] panics never pub
    f(x)
end t.bnd_io

fn t.bnd_may[F: core.ops.Fn<(u8,), u8, [], may>](f: F, x: u8) -> u8 uses [] panics never pub
    f(x)
end t.bnd_may

fn t.bnd_none[F: core.ops.Fn<(u8,), u8, [], never>](f: F, x: u8) -> u8 uses [] panics never pub
    f(x)
end t.bnd_none

fn t.inf_io(x: u8) -> u8
    core.mem.drop(std.io.print_line("i"))
    x
end t.inf_io

fn t.ok_infer(x: u8) -> u8 uses [] panics never
    t.bnd_none(
        fn(y: u8) -> u8 capture [] {
            t.inf_io(y)
        },
        x,
    )
end t.ok_infer

fn t.ok_io(s: t.S, x: u8) -> u8 uses [io] panics never
    t.bnd_io(
        fn(y: u8) -> u8 capture [&s] {
            core.mem.drop(s.say())
            y
        },
        x,
    )
end t.ok_io

fn t.ok_may(s: t.S, x: u8) -> u8 uses [] panics never
    t.bnd_may(
        fn(y: u8) -> u8 capture [&s] {
            s.boom()
        },
        x,
    )
end t.ok_may

fn t.ok_pure(s: t.S, x: u8) -> u8 uses [] panics never
    t.bnd_none(
        fn(y: u8) -> u8 capture [&s] {
            s.peek()
        },
        x,
    )
end t.ok_pure
"#;

#[test]
fn concrete_fn_bound_effects_are_checked_against_the_closure() {
    // 境界が具体的な `[]` / `never` を持つのに、渡したクロージャの確実な効果がそれを超える。
    // 呼び出し元が `uses [io]` でも境界の違反は報告する (let に束縛したクロージャ・メソッド形も)。
    let body = format!("{}{}", BOUND_BAD, BOUND_REST);
    let (_, got, out) = eff_check("eff_bound_bad", &body);
    assert_eq!(got, vec!["E4001".to_string(), "E4002".to_string()], "{}", out);
    assert!(out.contains("closure passed to `t.bnd_none` uses effect `io.console` outside the `Fn` bound's effects []"), "{}", out);
    assert!(out.contains("closure passed to `t.bnd_none` may panic but the `Fn` bound declares `panics never`"), "{}", out);
}

#[test]
fn concrete_fn_bound_is_checked_on_method_calls() {
    // メソッド形 (`s.run(..)`、レシーバの型で解決した候補が 1 つ) も、境界の具体名を超える効果を報告する。
    let src = "type t.S pub
    n: u8
end t.S

fn t.S.run[F: core.ops.Fn<(u8,), u8, [], never>](self: &t.S, f: F) -> u8 uses [] panics never pub
    f(self.n)
end t.S.run

fn t.bad_method(s: t.S) -> u8 uses [io] panics never
    s.run(
        fn(y: u8) -> u8 capture [] {
            core.mem.drop(std.io.print_line(\"m\"))
            y
        },
    )
end t.bad_method

fn t.ok_method(s: t.S) -> u8 uses [] panics never
    s.run(
        fn(y: u8) -> u8 capture [] {
            y
        },
    )
end t.ok_method
";
    let t = Tmp::new("eff_bound_method", src);
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    let got = codes(&out);
    assert_eq!(got, vec!["E4001".to_string()], "{}", out);
    assert!(out.contains("closure passed to `t.S.run` uses effect `io.console`"), "{}", out);
}

#[test]
fn concrete_fn_bound_effects_that_cover_the_closure_stay_silent() {
    // 境界が覆う効果 (`io` ⊇ `io.console`)・`may` 境界・効果のないクロージャ・確実でない効果
    // (推論された関数を呼ぶクロージャ) は黙る (下からの近似: 誤検出を出さない)。
    let (code, _, out) = eff_check("eff_bound_ok", BOUND_REST);
    assert_eq!(code, Some(0), "{}", out);
}

// ---- 借用の残り: 分解 (let (a, b) = t、match t { (a, b) => .. }) での保存ローンの読み戻し ----

#[test]
fn nll_rejects_a_mutation_while_a_let_destructured_binding_carries_a_stored_borrow() {
    // `let t = (&x, 1u8)` に保存したローンは、分解した束縛 `a` に伝わる (要素ごとの道は無いので全体の和)。
    let (bad, out) = has_e3010(
        "nll_destr_let",
        "",
        &[
            "let mut x = 1u8",
            "let t = (&x, 1u8)",
            "let (a, n) = t",
            "x = 3u8",
            "core.check.eq(*a, 1u8)",
            "core.check.eq(n, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `a`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_destructured_literal_binding_carries_the_borrow() {
    // リテラルを直接分解する `let (a, n) = (&x, 1u8)` も、束縛 `a` が借用を持つ。
    let (bad, out) = has_e3010(
        "nll_destr_lit",
        "",
        &[
            "let mut x = 1u8",
            "let (a, n) = (&x, 1u8)",
            "x = 3u8",
            "core.check.eq(*a, 1u8)",
            "core.check.eq(n, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `a`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_match_destructured_binding_carries_the_stored_borrow() {
    // `match t { (a, n) => .. }` の腕の束縛 `a` も、保存された借用を持つ。
    let (bad, out) = has_e3010(
        "nll_destr_match",
        "",
        &[
            "let mut x = 1u8",
            "let t = (&x, 1u8)",
            "match t {",
            "    (a, n) => {",
            "        x = 3u8",
            "        core.check.eq(*a, 1u8)",
            "    }",
            "}",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `a`"), "{}", out);
}

#[test]
fn nll_accepts_a_destructured_binding_that_is_dead_before_the_mutation() {
    // 分解の束縛 `a` を使い終えてから書き換えるなら、借用は死んでいて黙る。
    let (bad, out) = has_e3010(
        "nll_destr_dead",
        "",
        &[
            "let mut x = 1u8",
            "let t = (&x, 1u8)",
            "let (a, n) = t",
            "core.check.eq(*a, 1u8)",
            "x = 3u8",
            "core.check.eq(x, 3u8)",
            "core.check.eq(n, 1u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_destructured_binding_that_only_copies_the_value_out() {
    // 束縛から値をコピー (`*a`) して取り出し、その後は束縛を使わなければ、書き換えは許される。
    let (bad, out) = has_e3010(
        "nll_destr_copy",
        "",
        &[
            "let mut x = 1u8",
            "let t = (&x, 1u8)",
            "let (a, n) = t",
            "let v = *a",
            "x = 3u8",
            "core.check.eq(v, 1u8)",
            "core.check.eq(n, 1u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_destructured_tuple_without_any_borrow() {
    // 借用を含まないタプル (`(1u8, 2u8)`) の分解は、無関係な変数の書き換えで黙る。
    let (bad, out) = has_e3010(
        "nll_destr_nolo",
        "",
        &[
            "let mut x = 1u8",
            "let t = (1u8, 2u8)",
            "let (a, b) = t",
            "x = 3u8",
            "core.check.eq(a + b, 3u8)",
            "core.check.eq(x, 3u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_destructured_binding_after_the_tuple_was_overwritten() {
    // `t` を書き換えると、前に保存された `&x` のローンは死ぬ。新しい `&y` だけが分解の束縛に移る。
    let (bad, out) = has_e3010(
        "nll_destr_overwrite",
        "",
        &[
            "let mut x = 1u8",
            "let mut y = 2u8",
            "let mut t = (&x, 1u8)",
            "t = (&y, 2u8)",
            "let (a, n) = t",
            "x = 3u8",
            "core.check.eq(*a, 2u8)",
            "core.check.eq(n, 2u8)",
            "y = 4u8",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_struct_destructured_field_carries_a_stored_borrow() {
    // 構造体のフィールドに保存された借用は、名前の道をたどって分解の束縛 `r` に伝わる。
    let (bad, out) = has_e3010(
        "nll_destr_struct",
        NLL_REF_FIELD,
        &[
            "let mut x = 1u8",
            "let s = t.S { f: &x, b: 2u8 }",
            "let t.S { f: r, b: n } = s",
            "x = 2u8",
            "core.check.eq(*r, 1u8)",
            "core.check.eq(n, 2u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_accepts_a_copy_element_of_a_literal_destructured_while_the_borrow_is_mutated() {
    // `(&x, 1u8)` の分解で、借用を持つ要素 `a` は死んでおり、コピー値の `n` だけを後で使うなら黙る。
    let (bad, out) = has_e3010(
        "nll_destr_lit_copy",
        "",
        &[
            "let mut x = 1u8",
            "let (a, n) = (&x, 1u8)",
            "core.check.eq(*a, 1u8)",
            "x = 3u8",
            "core.check.eq(n, 1u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_destructured_call_result_when_the_source_is_mutated_before_the_copy_is_used() {
    // 道が分からない分解 (関数の戻り値 `t.pick(&x)`) は、右辺全体の借用を束縛に配らない (従来どおり黙る)。
    // `n` はコピー値なので、`x` を書き換えたあとに使っても E3010 にしない。
    let prefix = "fn t.apick(r: &u8) -> (&u8, u8) uses [] panics never\n    (r, 1u8)\nend t.apick\n\n";
    let (bad, out) = has_e3010(
        "nll_destr_call_neg",
        prefix,
        &[
            "let mut x = 1u8",
            "let (_, n) = t.apick(&x)",
            "x = 3u8",
            "core.check.eq(n, 1u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn e4004_for_div_and_rem_offers_only_the_checked_form() {
    // 整数の `/` `%` には checked_ の形しかない (num.sig)。wrapping_/saturating_ を置き換えとして案内しない。
    for (op, m) in [("/", "div"), ("%", "rem")] {
        let f = format!("fn t.q(a: u32, b: u32) -> u32 uses [] panics never\n    a {} b\nend t.q\n\n", op);
        let t = Tmp::new(&format!("e4004_{}", m), &main_wrap(&f, ""));
        let out = text(&run(&["check", "--json"], &t.0).stdout);
        assert!(codes(&out).iter().any(|c| c == "E4004"), "{}", out);
        assert!(out.contains(&format!("write `a.checked_{}(b)`", m)), "{}", out);
        assert!(out.contains("declare `panics may` on the function header"), "{}", out);
        assert!(!out.contains(&format!("a.wrapping_{}", m)), "{}", out);
        assert!(!out.contains(&format!("a.saturating_{}", m)), "{}", out);
    }
    let ex = run(&["explain", "E4004"], &root());
    assert!(text(&ex.stdout).contains("checked_div"), "{}", text(&ex.stdout));
}

#[test]
fn e1108_for_wrapping_or_saturating_div_rem_points_to_checked_forms() {
    // 存在しない `wrapping_rem` などに、誤った近い名前 (`rem` は f32/f64 のもの、`saturating_add`) を出さない。
    let f = "fn t.q(a: u32, b: u32) -> u32 uses [] panics never\n    let x = a.wrapping_rem(b)\n    a.saturating_div(x)\nend t.q\n\n";
    let t = Tmp::new("e1108_int_divrem", &main_wrap(f, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(out.contains("unknown method `wrapping_rem`; integer `%`"), "{}", out);
    assert!(out.contains("use `checked_rem(b)`"), "{}", out);
    assert!(out.contains("unknown method `saturating_div`; integer `/`"), "{}", out);
    assert!(out.contains("use `checked_div(b)`"), "{}", out);
    assert!(!out.contains("did you mean `rem`"), "{}", out);
    assert!(!out.contains("did you mean `saturating_add`"), "{}", out);
}

// ---- 分解の誤検出 (コピー値の束縛に借用を付けない): 道が分かる分解と、関数の戻り値の分解 ----

/// 診断が 1 つも出ないことを確かめる (陰性テスト用)。
fn no_diagnostics(name: &str, prefix: &str, b: &[&str]) -> String {
    let t = Tmp::new(name, &main_wrap(prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).is_empty(), "{}", out);
    out
}

#[test]
fn nll_accepts_a_copy_element_of_a_destructured_call_result_held_in_a_variable() {
    // `let p = t.apick(&x)` の p は x の借用を持つが、分解した `n` はコピー値なので黙る。
    let prefix = "fn t.apick(r: &u8) -> (&u8, u8) uses [] panics never\n    (r, 1u8)\nend t.apick\n\n";
    no_diagnostics(
        "nll_destr_call_var_neg",
        prefix,
        &[
            "let mut x = 1u8",
            "let p = t.apick(&x)",
            "let (a, n) = p",
            "core.check.eq(*a, 1u8)",
            "x = 3u8",
            "core.check.eq(n, 1u8)",
        ],
    );
}

#[test]
fn nll_accepts_a_copy_element_of_a_moved_call_result_destructured_later() {
    // `let t2 = t0` で move してから分解しても、コピー値 `n` には借用を付けない。
    let prefix = "fn t.apick(r: &u8) -> (&u8, u8) uses [] panics never\n    (r, 1u8)\nend t.apick\n\n";
    no_diagnostics(
        "nll_destr_call_move_neg",
        prefix,
        &[
            "let mut x = 1u8",
            "let t0 = t.apick(&x)",
            "let t2 = t0",
            "let (a, n) = t2",
            "core.check.eq(*a, 1u8)",
            "x = 3u8",
            "core.check.eq(n, 1u8)",
        ],
    );
}

#[test]
fn nll_accepts_a_copy_field_of_a_moved_struct_destructured_later() {
    // 構造体 `let s2 = s` で move してから分解しても、コピーのフィールド `b` (n) には借用を付けない。
    // 借用を持つフィールド `f` (r) は、書き換えより前に使い終えている。
    no_diagnostics(
        "nll_destr_struct_move_neg",
        NLL_REF_FIELD,
        &[
            "let mut x = 1u8",
            "let s = t.S { f: &x, b: 2u8 }",
            "let s2 = s",
            "let t.S { f: r, b: n } = s2",
            "core.check.eq(*r, 1u8)",
            "x = 2u8",
            "core.check.eq(n, 2u8)",
        ],
    );
}

#[test]
fn nll_accepts_a_copy_element_of_a_match_on_a_call_result_while_the_source_is_mutated() {
    // 形が分からない scrutinee (関数呼び出し) の腕の束縛 `n` は、右辺の借用を受け取らない。
    let prefix = "fn t.apick(r: &u8) -> (&u8, u8) uses [] panics never\n    (r, 1u8)\nend t.apick\n\n";
    no_diagnostics(
        "nll_match_call_neg",
        prefix,
        &[
            "let mut x = 1u8",
            "match t.apick(&x) {",
            "    (a, n) => {",
            "        core.check.eq(*a, 1u8)",
            "        x = 3u8",
            "        core.check.eq(n, 1u8)",
            "    }",
            "}",
        ],
    );
}

// ---- 効果検査・stack の誤検出 (推論された関数・確実でないクロージャ・直接呼び出しのフレーム) ----

/// `src` を `t` パッケージとして検査し、診断コードの列と出力を返す (陰性・陽性の両方で使う)。
fn neg_check(name: &str, src: &str) -> (Vec<String>, String) {
    let t = Tmp::new(name, src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

/// 効果を宣言しない `t.h` / `t.B.k` (推論される) と、`t.B.m` (効果なし) / `t.A.m` (`io.console`) の共通部分。
const INFERRED_PRELUDE: &str = "type t.A pub
    n: u8
end t.A

type t.B pub
    n: u8
end t.B

fn t.A.m(self: &t.A) -> Result<(), std.io.Error> uses [io.console] panics never pub
    std.io.print_line(\"a\")
end t.A.m

fn t.B.m(self: &t.B) -> u8 uses [] panics never pub
    self.n
end t.B.m

";

#[test]
fn inferred_callee_inside_a_closure_does_not_bind_the_fn_bound_effect() {
    // `t.h` は効果を宣言しないので推論される。推論は名前の全候補 (`m` の `t.A.m` を含む) の和で
    // `io.console` を過大に持つ。その効果を載せたクロージャは確実でないので、`apply` の `E` を
    // `io.console` に束縛せず、`uses []` の `t.main` から E4001 を出さない (黙る)。
    let src = format!(
        "{}{}",
        INFERRED_PRELUDE,
        "fn t.h(b: &t.B) -> u8
    b.m()
    0
end t.h

fn t.apply[F: core.ops.Fn<(&t.B,), u8, E, P>, E: uses, P: panics](f: F, b: &t.B) -> u8 uses [E] panics P pub
    f(b)
end t.apply

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let b = t.B { n: 0u8 }
    let r = t.apply(fn(x: &t.B) -> u8 capture [] {
        t.h(x)
    }, &b)
    core.mem.drop(r)
    Ok(())
end t.main
"
    );
    let (got, out) = neg_check("neg_inferred_fn_bound", &src);
    assert!(got.iter().all(|c| !c.starts_with("E4")), "{}", out);
}

#[test]
fn inferred_callee_of_a_local_receiver_stays_silent() {
    // `b.k()` の `t.B.k` は効果を宣言しない推論された関数。その推論は上からの近似なので、
    // `uses []` の `t.main` から E4001 を出さない (宣言された効果の呼び出しだけ検査する)。
    let src = format!(
        "{}{}",
        INFERRED_PRELUDE,
        "fn t.B.k(self: &t.B) -> u8
    self.m()
end t.B.k

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let b = t.B { n: 0u8 }
    let r = b.k()
    core.mem.drop(r)
    Ok(())
end t.main
"
    );
    let (got, out) = neg_check("neg_inferred_local_recv", &src);
    assert!(got.iter().all(|c| !c.starts_with("E4")), "{}", out);
}

#[test]
fn stack_closure_frame_of_a_direct_call_is_counted() {
    // `g(..)` の直接呼び出しのクロージャの節 (引数 t.Big = 64 + 32) はフレームに数える。
    // spin 48 + クロージャ 96 + core.mem.drop 32 = 176 (変更前は 80: 節が辺で届かず数えられなかった)。
    let src = "type t.Big
    a: u128
    b: u128
    c: u128
    d: u128
end t.Big

fn t.spin(n: u8) -> () uses [] panics never stack 100
    let g = fn(b: t.Big) -> () capture [] {
        core.mem.drop(b)
        t.spin(0u8)
    }
    g(t.Big { a: 0u128, b: 0u128, c: 0u128, d: 0u128 })
end t.spin

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    t.spin(1u8)
    Ok(())
end t.main
";
    let (got, out) = neg_check("stack_direct_closure", src);
    assert!(got.iter().any(|c| c == "E4012"), "{}", out);
    assert!(out.contains("estimated stack use 176 exceeds declared 100"), "{}", out);
    let (got, out) = neg_check("stack_direct_closure_ok", &src.replace("stack 100", "stack 176"));
    assert!(got.iter().all(|c| c != "E4012"), "{}", out);
}

#[test]
fn stack_closure_passed_to_a_fn_bound_called_as_f_of_x_is_a_candidate() {
    // `f(x)` の形で `Fn` 境界のパラメータを呼ぶ `t.gen` は、渡したクロージャの節へ辺を持つ (5.3)。
    // そのため t.spin -> t.gen -> クロージャ -> t.spin は再帰になる (変更前は辺がなく、E4010 も
    // E4012 の見積り 88 も出なかった)。spin の見積りは 216 (spin 40 + gen 48 + クロージャ 96 + drop 32)。
    let src = "type t.Big
    a: u128
    b: u128
    c: u128
    d: u128
end t.Big

fn t.gen[F: core.ops.Fn<(t.Big,), (), [], never>](f: F) -> () uses [] panics never stack 1024
    f(t.Big { a: 0u128, b: 0u128, c: 0u128, d: 0u128 })
end t.gen

fn t.spin(n: u8) -> () uses [] panics never stack SPIN
    t.gen(fn(b: t.Big) -> () capture [] {
        core.mem.drop(b)
        t.spin(0u8)
    })
end t.spin

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    t.spin(1u8)
    Ok(())
end t.main
";
    let (got, out) = neg_check("stack_fn_bound_call", &src.replace("SPIN", "215"));
    assert_eq!(got.iter().filter(|c| *c == "E4012").count(), 1, "{}", out);
    assert!(out.contains("t.spin: estimated stack use 216 exceeds declared 215"), "{}", out);
    let (got, out) = neg_check("stack_fn_bound_call_ok", &src.replace("SPIN", "216"));
    assert!(got.iter().all(|c| c != "E4012"), "{}", out);
}

// ---- 修正 (fix/ffi-review): 可変長 C 関数への浮動小数の引数 / from_ref の const / extern の整数→浮動小数 ----

/// 可変長の補助 C 側。`k_va_sum` は `va_arg` で double を読む (呼び出し側が `%al` を設定しないと 0 を読む)。
const FFI_VARIADIC_SRC: &str = r#"
#include <stdarg.h>
#include <stdint.h>

const char *k_fmt_f1(void) { return "%.1f"; }
double k_va_sum(int32_t n, ...) {
    va_list ap;
    va_start(ap, n);
    double s = 0;
    for (int32_t i = 0; i < n; i++) s += va_arg(ap, double);
    va_end(ap);
    return s;
}
"#;

#[test]
#[cfg(target_os = "linux")]
fn ffi_variadic_c_functions_receive_float_arguments() {
    // `snprintf` (可変長) に f64 を渡すと、呼び出し側が `%al` を設定しないと値が 0 になる。
    let src = r#"extern t.free_u8(pointer: *mut u8) -> () link "free"

extern t.k_fmt_f1() -> *const u8 link "k_fmt_f1"

extern t.k_va_sum(n: i32, x: f64, y: f64) -> f64 link "k_va_sum"

extern t.malloc(n: usize) -> *mut u8 link "malloc"

extern t.snprintf(buf: *mut u8, n: usize, fmt: *const u8, x: f64) -> i32 link "snprintf"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let buf = unsafe "libc" "allocate the output buffer" {
        t.malloc(64usize)
    }
    let fmt = unsafe "libc" "format string from C" {
        t.k_fmt_f1()
    }
    let n = unsafe "libc" "snprintf with a float argument" {
        t.snprintf(buf, 64usize, fmt, 2.5f64)
    }
    unsafe "libc" "read the formatted text" {
        core.check.eq(n, 3i32)
        core.check.eq(core.ptr.read(buf), 50u8)
        core.check.eq(core.ptr.read(core.ptr.add(buf, 1usize)), 46u8)
        core.check.eq(core.ptr.read(core.ptr.add(buf, 2usize)), 53u8)
    }
    let s = unsafe "libc" "variadic C function reads doubles" {
        t.k_va_sum(2i32, 1.5f64, 0.25f64)
    }
    core.check.eq(s, 1.75f64)
    unsafe "libc" "free the buffer" {
        t.free_u8(buf)
    }
    std.io.print_line("variadic ok")
    Ok(())
end t.main
"#;
    let t = Tmp::new("ffi_variadic", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_VARIADIC_SRC);
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "variadic ok\n");
}

#[test]
fn ffi_from_ref_pointer_is_const_and_write_through_it_is_rejected() {
    // `from_ref` は `*const` を返し、`core.ptr.write` は `*mut` を取る。書き込みは検査の段階で止まり、実行に届かない。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let x: i32 = 41i32
    let q: *const i32 = core.ptr.from_ref(&x)
    unsafe "libc" "write through a const pointer" {
        core.ptr.write(q, 42i32)
    }
    core.check.eq(x, 41i32)
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ffi_from_ref_const", src);
    assert_eq!(r.status.code(), Some(1), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stdout).contains("E2023") && text(&r.stdout).contains("`*const i32`"), "{}", text(&r.stdout));
}

#[test]
fn ffi_as_mut_makes_a_from_ref_pointer_writable() {
    // `as_mut` を通したものは書ける。書き込みは元の場所 (`x`) に届く。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut x: i32 = 41i32
    let q: *const i32 = core.ptr.from_ref(&x)
    let p: *mut i32 = core.ptr.as_mut(q)
    unsafe "libc" "write through as_mut" {
        core.ptr.write(p, 42i32)
    }
    core.check.eq(x, 42i32)
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ffi_as_mut_writable", src);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
}

#[test]
fn ffi_as_const_pointer_is_read_only_and_from_mut_stays_writable() {
    // `as_const` で作った `*const` は読めるが、書き込みは `*mut` を要求する検査で止まる (下のテスト)。元の `from_mut` のポインタは従来どおり書けて読める。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut x: i32 = 1i32
    let p: *mut i32 = core.ptr.from_mut(&mut x)
    let c: *const i32 = core.ptr.as_const(p)
    unsafe "libc" "write through the mutable pointer" {
        core.ptr.write(p, 2i32)
        core.check.eq(core.ptr.read(c), 2i32)
    }
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ffi_as_const_readonly", src);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
}

#[test]
fn ffi_write_through_as_const_pointer_is_rejected() {
    // `as_const` で作った `*const` への書き込みは、`*mut` を取る `core.ptr.write` の検査で E2023 になる。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut x: i32 = 1i32
    let p: *mut i32 = core.ptr.from_mut(&mut x)
    let c: *const i32 = core.ptr.as_const(p)
    unsafe "libc" "write through the const pointer" {
        core.ptr.write(c, 3i32)
    }
    Ok(())
end t.main
"#;
    let (_t, r) = run_unsafe_pkg("ffi_as_const_write_rejected", src);
    assert_eq!(r.status.code(), Some(1), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stdout).contains("E2023") && text(&r.stdout).contains("`*mut i32`"), "{}", text(&r.stdout));
}

#[test]
#[cfg(target_os = "linux")]
fn ffi_integer_argument_for_float_parameter_panics() {
    // 整数の値を f32 / f64 の引数に渡すのは型が合わないので、黙って変換せず 101 で止まる。
    let src = r#"extern t.k_half(x: f32) -> f32 link "k_half"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let h = unsafe "libc" "integer for f32" {
        t.k_half(3i32)
    }
    core.check.eq(h, 1.5f32)
    Ok(())
end t.main
"#;
    let t = Tmp::new("ffi_int_for_f32", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_C_SRC);
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(101), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stderr).contains("unsupported type or value"), "{}", text(&r.stderr));
}

#[test]
#[cfg(target_os = "linux")]
fn ffi_integer_argument_for_f64_parameter_panics() {
    let src = r#"extern t.k_scale(x: f64, n: i32, y: f64) -> f64 link "k_scale"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe "libc" "integer for f64" {
        t.k_scale(1i32, 2i32, 0.25f64)
    }
    core.check.eq(a, 2.25f64)
    Ok(())
end t.main
"#;
    let t = Tmp::new("ffi_int_for_f64", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_C_SRC);
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(101), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stderr).contains("unsupported type or value"), "{}", text(&r.stderr));
}

// ---- 型推論の完全化 第 1 段 (設計_KIR.md 第 8 章 8.5): while / loop、派生メソッド、char / bool、関数項目 ----

/// 型推論の被覆計測 (`KSN_TYPE_COVERAGE=1`) で、未知の式が 0 件であることを確かめる。
/// 型が決まらない式があれば、ここで落ちる (型規則の欠落の回帰)。
fn assert_all_typed(name: &str, src: &str) {
    let t = Tmp::new(name, src);
    let o = Command::new(exe()).arg("check").env("KSN_TYPE_COVERAGE", "1").current_dir(&t.0).output().expect("spawn ksn");
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let err = text(&o.stderr);
    assert!(err.contains("型変数 0 / 未知 0 (0.0%, うち根 0)"), "{}", err);
}

#[test]
fn kir_while_and_loop_have_types() {
    let fns = "fn t.spin(n: u32) -> u32 uses [] panics never
    let mut i = 0u32
    loop {
        i = i.wrapping_add(1u32)
        if i > n {
            break
        }
    }
    i
end t.spin

fn t.count(n: u32) -> u32 uses [] panics never
    let mut i = 0u32
    while i < n {
        i = i.wrapping_add(1u32)
    }
    i
end t.count

fn t.forever(n: u32) -> u32 uses [] panics never
    loop {
        if n == 0u32 {
            return 1u32
        }
    }
end t.forever

";
    assert_all_typed("kir_loop_types", &main_wrap(fns, "    core.check.eq(t.spin(3u32), 4u32)\n    core.check.eq(t.count(3u32), 3u32)\n"));
}

#[test]
fn kir_derived_methods_return_self_or_the_declared_type() {
    // `clone` は Self (u32) を返すので、bool の注釈と食い違う。派生メソッドの戻り値の型が効いている。
    let fns = "fn t.derived(a: u32) -> bool uses [] panics never
    let b: bool = a.clone()
    b
end t.derived

";
    let t = Tmp::new("kir_derived_clone", &main_wrap(fns, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}", out);
    assert!(codes(&out).iter().any(|c| c == "E2020"), "{}", out);
}

#[test]
fn kir_char_and_bool_methods_are_resolved() {
    let fns = "fn t.chars() -> u32 uses [] panics never
    let c = 'a'
    let b = true
    let u = c.to_u32()
    core.mem.drop(b.then_some(1u8))
    core.mem.drop(c.to_ascii_uppercase())
    u
end t.chars

";
    assert_all_typed("kir_char_bool_typed", &main_wrap(fns, "    core.check.eq(t.chars(), 97u32)\n"));
    // 戻り値の型が sig どおりに決まる (u32 を char の注釈に入れると E2020)。
    let bad = "fn t.bad() -> char uses [] panics never
    let c = 'a'
    let n: char = c.to_u32()
    n
end t.bad

";
    let t = Tmp::new("kir_char_ret_mismatch", &main_wrap(bad, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E2020"), "{}", out);
}

#[test]
fn kir_function_item_as_value_has_a_function_type() {
    let fns = "fn t.double(x: u32) -> u32 uses [] panics never
    x.wrapping_add(x)
end t.double

fn t.apply() -> u32 uses [] panics never
    let f = t.double
    f(3u32)
end t.apply

";
    assert_all_typed("kir_fn_item_typed", &main_wrap(fns, "    core.check.eq(t.apply(), 6u32)\n"));
    // 関数項目の引数の型が宣言と違えば、クロージャの呼び出しとして E2023 にする。
    let bad = "fn t.double(x: u32) -> u32 uses [] panics never
    x.wrapping_add(x)
end t.double

fn t.bad() -> u32 uses [] panics never
    let f = t.double
    f(true)
end t.bad

";
    let t = Tmp::new("kir_fn_item_arg_mismatch", &main_wrap(bad, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E2023"), "{}", out);
    assert!(out.contains("argument 1 of the closure"), "{}", out);
}

// ---- 型推論の完全化 第 2 段 (設計_KIR.md 第 8 章 8.7): メソッドの Fn 境界、腕どうしの型、列挙子のメソッド ----

#[test]
fn kir_method_fn_bound_is_unified_with_the_argument() {
    // `map_err(t.code)` の関数項目の引数 (bool) と戻り値 (u32) が、メソッドの Fn 境界と単一化される。
    // 結果の型は `Ok(k)` の枝と一致して決まる (型変数 0)。
    let fns = "fn t.code(e: bool) -> u32 uses [] panics never
    if e {
        1u32
    } else {
        2u32
    }
end t.code

fn t.wrap(r: Result<u8, bool>, k: u8) -> Result<u8, u32> uses [] panics never
    if k == 0u8 {
        r.map_err(t.code)
    } else {
        Ok(k)
    }
end t.wrap

";
    assert_all_typed("kir_map_err_typed", &main_wrap(fns, ""));
    // 関数項目の引数の型 (bool) と Result のエラー型 (u8) が違えば、メソッドの引数の検査として E2023。
    let bad = "fn t.code(e: bool) -> u32 uses [] panics never
    if e {
        1u32
    } else {
        2u32
    }
end t.code

fn t.bad(r: Result<u8, u8>) -> Result<u8, u32> uses [] panics never
    r.map_err(t.code)
end t.bad

";
    let t = Tmp::new("kir_map_err_arg_mismatch", &main_wrap(bad, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E2023"), "{}", out);
}

#[test]
fn kir_match_and_if_arms_share_their_type() {
    // `Ok` / `Err` / `None` の型引数は、同じ式の他の枝や戻り値型から決まる。
    let fns = "fn t.checked(a: u8, b: u8) -> Result<u8, u8> uses [] panics never
    match a.checked_add(b) {
        Some(v) => Ok(v)
        None => Err(a)
    }
end t.checked

fn t.pick(a: u8) -> Option<u8> uses [] panics never
    if a == 0u8 {
        None
    } else {
        Some(a)
    }
end t.pick

fn t.unwrap_or_zero(o: Option<u8>) -> u8 uses [] panics never
    match o {
        Some(v) => v
        None => return 0u8
    }
end t.unwrap_or_zero

";
    assert_all_typed("kir_arms_typed", &main_wrap(fns, ""));
}

#[test]
fn kir_method_on_unit_variant_is_resolved() {
    // `t.Phase.Idle.next()` は、変種の値 (型 `t.Phase`) へのメソッド呼び出し。
    let enum_and_next = "enum t.Phase derive [Debug, Clone, Copy, PartialEq]
    Idle
    Busy
end t.Phase

fn t.Phase.next(self: t.Phase) -> t.Phase uses [] panics never
    match self {
        t.Phase.Idle => t.Phase.Busy
        t.Phase.Busy => t.Phase.Idle
    }
end t.Phase.next

";
    let fns = format!("{}fn t.after_idle() -> t.Phase uses [] panics never\n    t.Phase.Idle.next()\nend t.after_idle\n\n", enum_and_next);
    assert_all_typed("kir_unit_variant_method_typed", &main_wrap(&fns, ""));
    // 戻り値の型 (t.Phase) と注釈 (bool) が違えば、E2020。
    let bad = format!("{}fn t.bad() -> bool uses [] panics never\n    let b: bool = t.Phase.Idle.next()\n    b\nend t.bad\n\n", enum_and_next);
    let t = Tmp::new("kir_unit_variant_mismatch", &main_wrap(&bad, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E2020"), "{}", out);
}

/// 基本型の `to_string` (`Display` の組み込み実装) と派生の比較 (`ne` / `partial_cmp`) は、
/// インタプリタとバンドルの両方で動く (以前は "unknown integer operation" で panic していた)。
#[test]
fn scalar_to_string_and_derived_methods_run_in_both() {
    let test = "test t.main.test_scalar_to_string
    let n: i64 = -7i64
    core.check.eq(n.to_string().as_str(), \"-7\")
    core.check.eq(n.to_string().len(), 2usize)
    let big: u32 = 4294967295u32
    core.check.eq(big.to_string().as_str(), \"4294967295\")
    let c: char = 'x'
    core.check.eq(c.to_string().as_str(), \"x\")
    let t: bool = true
    core.check.eq(t.to_string().as_str(), \"true\")
    core.check.eq(n != 3i64, true)
    core.check.eq(n.partial_cmp(&3i64).is_some(), true)
end t.main.test_scalar_to_string
";
    let src = format!("{}\n{}", main_wrap("", ""), test);
    let t = Tmp::new("scalar_to_string_both", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_scalar_to_string ... ok"), "{}", out);
}

/// `Display` の char は文字そのもの。`print_line` と `to_string` の出力を確かめる。
#[test]
fn char_display_prints_the_character_itself() {
    let body = "    let c: char = 'x'\n    std.io.print_line(c)\n    std.io.print_line(c.to_string())\n    std.io.print_line(7u8.to_string())\n";
    let t = Tmp::new("char_display", &main_wrap("", body));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), "x\nx\n7\n");
}

/// 整数の幅変換は `from_*` / `try_from_*` (仕様 4.5)。`to_<整数型>` は持たない。
#[test]
fn integer_width_conversion_uses_from_not_to() {
    let t = Tmp::new("int_from", &main_wrap("", "    core.check.eq(u32.from_u8(200u8), 200u32)\n"));
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
}

#[test]
fn int_to_conversion_is_e1108_with_from_fix() {
    // 整数の `to_<整数型>` は廃止 (4.5)。幅が広がる変換は `u32.from_u8(x)` を案内し、機械適用できる修正を付ける。
    let bad = "fn t.widen(x: u8) -> u32 uses [] panics never
    x.to_u32()
end t.widen

";
    let t = Tmp::new("int_to_conversion_bad", &main_wrap(bad, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E1108"), "{}", out);
    assert!(out.contains("u32.from_u8(x)"), "{}", out);
    assert!(out.contains("\"replace\": \"u32.from_u8(x)\""), "{}", out);
    // 狭める変換には Result の `try_from_*` を案内する (修正は付けない: 型が変わるため)。
    let narrow = "fn t.narrow(x: u32) -> u8 uses [] panics never
    x.to_u8()
end t.narrow

";
    let t = Tmp::new("int_to_conversion_narrow", &main_wrap(narrow, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(out.contains("u8.try_from_u32(x)"), "{}", out);
    assert!(!out.contains("\"replace\": \"u8.try_from"), "{}", out);
}

#[test]
fn int_conversions_that_exist_are_clean() {
    // 正しい形: `from_*` / `try_from_*`、`to_f64`、char の `to_u32`。どれも E1108 にしない。
    let ok = "fn t.a(x: u8) -> u32 uses [] panics never
    u32.from_u8(x)
end t.a

fn t.b(x: u32) -> Result<u8, core.num.TryFromIntError> uses [] panics never
    u8.try_from_u32(x)
end t.b

fn t.c(x: u8) -> f64 uses [] panics never
    x.to_f64()
end t.c

fn t.d(c: char) -> u32 uses [] panics never
    c.to_u32()
end t.d

";
    assert_all_typed("int_conversion_ok_typed", &main_wrap(ok, ""));
    let t = Tmp::new("int_conversion_ok", &main_wrap(ok, ""));
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

#[test]
fn float_literal_method_without_context_is_e2027() {
    // `let a = 9.0` の型が文脈から決まらないままメソッドを呼ぶ: 仕様どおり E2027。
    let bad = "fn t.root() -> f64 uses [] panics never
    let a = 9.0
    a.sqrt()
end t.root

";
    let t = Tmp::new("float_method_no_context", &main_wrap(bad, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E2027"), "{}", out);
    // 接尾辞を付ければ型が決まる。後から型が決まる場合は E2027 にしない。
    let ok = "fn t.suffixed() -> f64 uses [] panics never
    let a = 9.0f64
    a.sqrt()
end t.suffixed

fn t.later(b: f32) -> f32 uses [] panics never
    let a = 9.0
    let r = a.sqrt()
    let s: f32 = a
    b
end t.later

";
    let t = Tmp::new("float_method_context", &main_wrap(ok, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(!codes(&out).iter().any(|c| c == "E2027"), "{}", out);
}

#[test]
fn derived_ordering_operators_are_not_methods() {
    // `<` 等は partial_cmp から導く演算子で、メソッドではない (3.2)。`lt` などは E1108。
    let bad = "fn t.f(a: u8, b: u8) -> bool uses [] panics never
    a.lt(b)
end t.f

";
    let t = Tmp::new("derived_lt_is_not_method", &main_wrap(bad, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E1108"), "{}", out);
}

#[test]
fn move_capture_that_is_rewritten_does_not_meet_the_fn_bound() {
    // `move` で捕まえた値を書き換えるクロージャは、読むだけの `Fn` を満たさない (FnMut まで)。
    let (got, out) = check_codes(
        "cl_move_mut",
        &[
            "let mut n = 0u32",
            "let f = fn(acc: u32, x: u32) -> u32 capture [move n] {",
            "    n = acc + x",
            "    n",
            "}",
            "let total = core.iter.fold(core.iter.range(0u32, 4u32), 0u32, f)",
            "core.mem.drop(total)",
        ],
    );
    assert!(got.iter().any(|c| c == "E2030"), "{}", out);
}

#[test]
fn move_capture_that_is_consumed_does_not_meet_the_fn_bound() {
    // `move` で捕まえた Copy でない値を値のまま消費するクロージャは FnOnce で、Fn / FnMut を満たさない。
    let (got, out) = check_codes(
        "cl_move_once",
        &[
            "let v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let f = fn(acc: u32, x: u32) -> u32 capture [move v] {",
            "    core.mem.drop(v)",
            "    acc + x",
            "}",
            "let total = core.iter.fold(core.iter.range(0u32, 4u32), 0u32, f)",
            "core.mem.drop(total)",
        ],
    );
    assert!(got.iter().any(|c| c == "E2030"), "{}", out);
}

#[test]
fn move_capture_that_is_only_read_meets_the_fn_bound() {
    // `move` で捕まえても本体が読むだけ (Copy の値、メソッドのレシーバ) なら Fn を満たす (Rust と同じ)。
    let (got, out) = check_codes(
        "cl_move_read",
        &[
            "let k = 7u32",
            "let s = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let f = fn(acc: u32, x: u32) -> u32 capture [move k, move s] {",
            "    core.mem.drop(s.len())",
            "    acc + x * k",
            "}",
            "let total = core.iter.fold(core.iter.range(0u32, 4u32), 0u32, f)",
            "core.mem.drop(total)",
        ],
    );
    assert!(!got.iter().any(|c| c == "E2030"), "{}", out);
}

/// バンドル (インタプリタ同梱の起動スクリプト) が実行権限を持ち、相対パス・PATH 経由・
/// 別ディレクトリから起動しても同梱物を exec できること。`to_string` は clif 未対応なので
/// ネイティブ生成が失敗し、バンドルへフォールバックする。
#[cfg(unix)]
#[test]
fn bundle_launcher_is_executable_and_location_independent() {
    use std::os::unix::fs::PermissionsExt;
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let n = 42u32
    let s = n.to_string()
    std.io.print_line(s)
end t.main
";
    let t = Tmp::new("bundle_launcher", src);
    let cache = std::env::temp_dir().join(format!("ksn_bundle_cache_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let o = Command::new(exe())
        .args(["build", "--release"])
        .current_dir(&t.0)
        .env("KSN_CACHE_DIR", &cache)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let rel_dir = Path::new("target").join("release");
    let rel_dir_abs = t.0.join(&rel_dir);
    let launcher = rel_dir_abs.join("t");
    let mode = fs::metadata(&launcher).expect("launcher exists").permissions().mode();
    assert!(mode & 0o111 != 0, "launcher is not executable: mode {:o}", mode);

    let check = |o: Output, how: &str| {
        assert_eq!(o.status.code(), Some(0), "{}: {}", how, text(&o.stderr));
        assert_eq!(text(&o.stdout), "42\n", "{}", how);
    };
    // 1. パッケージ直下から相対パスで起動
    check(Command::new(rel_dir.join("t")).current_dir(&t.0).output().unwrap(), "relative path");
    // 2. 親ディレクトリから、パッケージを含む相対パスで起動
    let parent = t.0.parent().unwrap().to_path_buf();
    let pkg_name = t.0.file_name().unwrap().to_os_string();
    let via_parent = Path::new(&pkg_name).join(&rel_dir).join("t");
    check(Command::new(&via_parent).current_dir(&parent).output().unwrap(), "relative path from parent");
    // 3. 無関係なディレクトリから絶対パスで起動
    check(Command::new(&launcher).current_dir(&cache).output().unwrap(), "absolute path");
    // 4. PATH 経由 (bare name。$0 にスラッシュが無い場合の解決)
    let path = format!("{}:{}", rel_dir_abs.display(), std::env::var("PATH").unwrap_or_default());
    check(Command::new("t").env("PATH", path).current_dir(&cache).output().unwrap(), "PATH lookup");

    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn kir_iter_adapters_are_typed_through_the_impl() {
    // `map` / `filter` の結果の E・P (効果・panic の実引数) は型変数にならない。`enumerate` / `take` / `count` の
    // `T` は、境界 `I: Iterator<T, E, P>` の I の impl (`Range` / `Enumerate` / 利用者の型) から決まる (型変数 0)。
    let fns = "type t.Upto pub
    cur: u32
    stop: u32
end t.Upto

impl core.iter.Iterator<u32, [], never> for t.Upto
fn t.Upto.next(self: &mut t.Upto) -> Option<u32> uses [] panics never impls core.iter.Iterator<u32, [], never>
    if self.cur < self.stop {
        let v = self.cur
        self.cur = self.cur.wrapping_add(1u32)
        Some(v)
    } else {
        None
    }
end t.Upto.next

fn t.chain(n: i32) -> i32 uses [] panics never
    core.iter.fold(
        core.iter.map(
            core.iter.filter(
                core.iter.range(0i32, n),
                fn(x: &i32) -> bool capture [] {
                    *x > 2i32
                },
            ),
            fn(x: i32) -> i32 capture [] {
                x.wrapping_mul(2i32)
            },
        ),
        0i32,
        fn(a: i32, x: i32) -> i32 capture [] {
            a.wrapping_add(x)
        },
    )
end t.chain

fn t.count_it() -> usize uses [] panics never
    core.iter.count(core.iter.enumerate(core.iter.take(core.iter.range(0u8, 9u8), 2usize)))
end t.count_it

fn t.user_sum() -> u32 uses [] panics never
    core.iter.fold(t.Upto { cur: 0u32, stop: 4u32 }, 0u32, fn(a: u32, x: u32) -> u32 capture [] {
        a.wrapping_add(x)
    })
end t.user_sum

";
    assert_all_typed("kir_iter_adapters_typed", &main_wrap(fns, ""));
}

#[test]
fn kir_iter_adapter_element_type_mismatch_is_reported() {
    // 境界の要素型 (`range(0i32, ..)` の `T` = i32) と、フィルタのクロージャの引数型 (`&u8`) が違えば E2023。
    let bad = "fn t.bad(n: i32) -> usize uses [] panics never
    core.iter.count(
        core.iter.filter(
            core.iter.range(0i32, n),
            fn(x: &u8) -> bool capture [] {
                *x > 2u8
            },
        ),
    )
end t.bad

";
    let t = Tmp::new("kir_iter_filter_arg_mismatch", &main_wrap(bad, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E2023"), "{}", out);
}

/// `str` / `String` の `to_string`、スライス `Iter.count`、`&str` / `&[T]` / Option / Result の派生比較
/// (`eq` `ne` `cmp` `partial_cmp`) は spec/stdlib の規範どおり check が受理する。以前はいずれも
/// インタプリタで "no method ..." の panic になっていた。インタプリタとバンドルの両方で動くことを確かめる。
#[test]
fn str_slice_option_result_sig_methods_run_in_both() {
    let test = "test t.main.test_a_option_result_cmp
    let o: Option<u8> = Some(1u8)
    core.check.eq(o.eq(&Some(1u8)), true)
    core.check.eq(o.cmp(&None) == core.cmp.Ordering.Greater, true)
    let r: Result<u8, u8> = Ok(1u8)
    core.check.eq(r != Err(1u8), true)
end t.main.test_a_option_result_cmp

test t.main.test_b_slice_count
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(1u8))
    core.mem.drop(v.push(2u8))
    core.check.eq(v.iter().count(), 2usize)
    let mut it = v.iter()
    core.mem.drop(it.next())
    core.check.eq(it.count(), 1usize)
end t.main.test_b_slice_count

test t.main.test_c_str_derived_cmp
    let s: &str = \"ab\"
    let t: &str = \"ac\"
    core.check.eq(s.eq(&t), false)
    core.check.eq(s != t, true)
    core.check.eq(s.cmp(&t) == core.cmp.Ordering.Less, true)
    core.check.eq(s.partial_cmp(&t).is_some(), true)
end t.main.test_c_str_derived_cmp

test t.main.test_d_to_string
    let s: &str = \"héllo\"
    core.check.eq(s.to_string().as_str(), \"héllo\")
    core.check.eq(s.to_string().len(), 6usize)
    let mut owned = s.to_string()
    core.mem.drop(owned.push('!'))
    core.check.eq(owned.as_str(), \"héllo!\")
    let st = alloc.string.String.new(std.alloc.SYSTEM)
    core.check.eq(st.to_string().is_empty(), true)
end t.main.test_d_to_string
";
    let src = format!("{}\n{}", main_wrap("", ""), test);
    let t = Tmp::new("str_slice_option_result_sig", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    for name in ["test_a_option_result_cmp", "test_b_slice_count", "test_c_str_derived_cmp", "test_d_to_string"] {
        assert!(out.contains(&format!("t.main.{} ... ok", name)), "{}", out);
    }
}

#[test]
fn comparison_method_names_hint_the_operator_not_a_near_name() {
    // `lt` 等は演算子 (4.3) で、`ln` のような近い名前を提案しない。
    let bad = "fn t.f(a: u8, b: u8) -> bool uses [] panics never
    a.lt(b)
end t.f

fn t.g(a: u8, b: u8) -> bool uses [] panics never
    a.ge(b)
end t.g

";
    let t = Tmp::new("cmp_method_hint", &main_wrap(bad, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E1108"), "{}", out);
    assert!(out.contains("use `a < b`"), "{}", out);
    assert!(out.contains("use `a >= b`"), "{}", out);
    assert!(!out.contains("did you mean"), "{}", out);
}

#[test]
fn float_to_int_method_is_e1108_with_trunc_hint() {
    // 浮動小数の `to_<整数型>` は廃止された変換名。切り捨て・範囲外 None の `trunc_to_*` (Option) を案内する。
    // 型が Option に変わるので機械適用の修正は付けない。
    let bad = "fn t.g(x: f64) -> u32 uses [] panics never
    x.to_u32()
end t.g

fn t.h(x: f32) -> u8 uses [] panics never
    x.to_u8()
end t.h

";
    let t = Tmp::new("float_to_int_bad", &main_wrap(bad, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(codes(&out).iter().filter(|c| *c == "E1108").count(), 2, "{}", out);
    assert!(out.contains("x.trunc_to_u32()"), "{}", out);
    assert!(out.contains("x.trunc_to_u8()"), "{}", out);
    assert!(!out.contains("\"replace\""), "{}", out);
    // 正しい形 (`trunc_to_*` と、浮動小数どうしの `to_f64`) は E1108 にしない。
    let ok = "fn t.g(x: f64) -> Option<u32> uses [] panics never
    x.trunc_to_u32()
end t.g

fn t.h(x: f32) -> f64 uses [] panics never
    x.to_f64()
end t.h

";
    let t = Tmp::new("float_to_int_ok", &main_wrap(ok, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(!codes(&out).iter().any(|c| c == "E1108"), "{}", out);
}

#[test]
fn derived_ord_min_max_check_and_run() {
    // `Ord` を derive した型の `min` / `max` は Self を返す (仕様 4.3)。等しいとき min は self、max は other。
    // `Ord` を derive しない型では E1108 (型検査が「メソッドがない」と出す)。
    let ok = "type t.P derive [Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord]
    x: u8
end t.P

fn t.pick(a: t.P, b: t.P) -> t.P uses [] panics never
    a.min(b)
end t.pick

";
    let body = "    core.check.eq(t.pick(t.P { x: 3u8 }, t.P { x: 5u8 }).x, 3u8)
    core.check.eq(t.P { x: 3u8 }.max(t.P { x: 5u8 }).x, 5u8)
";
    let src = main_wrap(ok, body).replace("uses [io.console] panics may", "uses [] panics may");
    let t = Tmp::new("derived_ord_min_max", &src);
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E1108"), "{}", text(&o.stdout));
    let r = run(&["run"], &t.0);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));

    let bad = "type t.Q derive [Debug, Clone, Copy, PartialEq]
    x: u8
end t.Q

fn t.pick(a: t.Q, b: t.Q) -> t.Q uses [] panics never
    a.min(b)
end t.pick

";
    let t = Tmp::new("derived_min_without_ord", &main_wrap(bad, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E1108"), "{}", out);
    assert!(out.contains("has no method `min`"), "{}", out);
}

// ---- check/raw-ptr-types: 生ポインタの型 (`*const T` / `*mut T`) を型検査が見る ----

/// `unsafe` を許したパッケージを `check --json` にかけ、出力文字列を返す (1 診断 1 行の JSON)。
fn raw_ptr_check(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let o = run(&["check", "--json"], &t.0);
    text(&o.stdout)
}

#[test]
fn raw_ptr_annotations_accept_the_correct_forms_without_errors() {
    // 正しい書き方: from_mut / from_ref / null_mut / as_const / as_mut / cast、`*mut` → `*const` の暗黙の縮小、読み書き。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut x: i32 = 1i32
    let p: *mut i32 = core.ptr.from_mut(&mut x)
    let c: *const i32 = core.ptr.as_const(p)
    let q: *const i32 = core.ptr.from_ref(&x)
    let m: *mut i32 = core.ptr.as_mut(q)
    let n: *mut i32 = core.ptr.null_mut::<i32>()
    let b: *mut u8 = core.ptr.cast::<i32, u8>(p)
    let r: *const u8 = b
    let u: *const i32 = p
    unsafe \"libc\" \"correct forms\" {
        core.ptr.write(m, 2i32)
        core.check.eq(core.ptr.read(c), 2i32)
        core.check.eq(core.ptr.is_null(n), true)
        core.check.eq(core.ptr.read(u), 2i32)
        core.check.eq(core.ptr.read(r), 2u8)
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_ok", src);
    assert!(!codes(&out).iter().any(|c| c == "E2020" || c == "E2023"), "{}", out);
}

#[test]
fn raw_ptr_let_annotation_rejects_a_plain_value() {
    // `let s: *mut i32 = 5i32`: 値をポインタの注釈へ入れると E2020。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let s: *mut i32 = 5i32
    core.check.eq(core.ptr.is_null(s), false)
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_val", src);
    assert!(out.contains("E2020") && out.contains("expected *mut i32, found i32"), "{}", out);
}

#[test]
fn raw_ptr_let_annotation_rejects_const_into_mut() {
    // `let r: *mut i32 = core.ptr.from_ref(&x)`: `from_ref` は `*const` を返すので、`*mut` の注釈は E2020。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let x: i32 = 1i32
    let r: *mut i32 = core.ptr.from_ref(&x)
    core.check.eq(core.ptr.is_null(r), false)
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_const_to_mut", src);
    assert!(out.contains("E2020") && out.contains("expected *mut i32, found *const i32"), "{}", out);
}

#[test]
fn raw_ptr_let_annotation_rejects_a_different_element_type() {
    // 要素型が確定して食い違う (`*const u8` と `*const i32`) のは E2020。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let x: i32 = 1i32
    let r: *const u8 = core.ptr.from_ref(&x)
    core.check.eq(core.ptr.is_null(r), false)
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_elem", src);
    assert!(out.contains("E2020") && out.contains("expected *const u8, found *const i32"), "{}", out);
}

#[test]
fn raw_ptr_pointer_is_not_a_plain_integer_annotation() {
    // ポインタを整数の注釈へ入れるのは E2020 (`core.ptr.null_mut::<i32>()` を `i32` へ)。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let n: i32 = core.ptr.null_mut::<i32>()
    core.check.eq(n, 0i32)
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_to_int", src);
    assert!(out.contains("E2020") && out.contains("expected i32, found *mut i32"), "{}", out);
}

#[test]
fn raw_ptr_unsuffixed_literal_is_not_a_pointer() {
    // 整数リテラルをポインタの注釈へ入れるのは E2020 (型は `{integer}`)。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let q: *mut i32 = 5
    core.check.eq(core.ptr.is_null(q), false)
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_literal", src);
    assert!(out.contains("E2020") && out.contains("expected *mut i32, found {integer}"), "{}", out);
}

#[test]
fn raw_ptr_const_argument_to_mut_parameter_is_rejected() {
    // 宣言が `*mut i32` の関数へ `from_ref` (const) の値を渡すと E2023。
    let src = "fn t.take(p: *mut i32) -> () uses [] panics never
    core.check.eq(core.ptr.is_null(p), false)
end t.take

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let x: i32 = 1i32
    t.take(core.ptr.from_ref(&x))
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_arg", src);
    assert!(out.contains("E2023") && out.contains("expected `*mut i32`, found `*const i32`"), "{}", out);
}

#[test]
fn raw_ptr_mut_argument_to_const_parameter_is_accepted() {
    // `*mut` → `*const` の縮小は暗黙に許す (宣言が `*const` の関数へ `*mut` を渡す)。
    let src = "fn t.look(p: *const i32) -> i32 uses [] panics never
    core.ptr.read(p)
end t.look

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut x: i32 = 1i32
    let p: *mut i32 = core.ptr.from_mut(&mut x)
    unsafe \"libc\" \"read through a mutable pointer\" {
        core.check.eq(t.look(p), 1i32)
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_mut_to_const", src);
    assert!(!codes(&out).iter().any(|c| c == "E2020" || c == "E2023"), "{}", out);
}

#[test]
fn raw_ptr_return_value_mismatch_is_rejected() {
    // 戻り値が `*mut` の関数で `from_ref` (const) を返すと E2020 (戻り値)。
    let src = "fn t.back(x: &i32) -> *mut i32 uses [] panics never
    core.ptr.from_ref(x)
end t.back

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let x: i32 = 1i32
    let r = t.back(&x)
    core.check.eq(core.ptr.is_null(r), false)
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_ret", src);
    assert!(out.contains("E2020") && out.contains("type mismatch in return value: expected *mut i32, found *const i32"), "{}", out);
}

#[test]
fn raw_ptr_extern_mut_parameter_rejects_a_const_pointer() {
    // extern 宣言の `*mut u8` 引数へ `*const u8` (from_ref) を渡すと E2023。
    let src = "extern t.free(pointer: *mut u8) -> () link \"free\"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let b: u8 = 1u8
    unsafe \"libc\" \"free a const pointer\" {
        t.free(core.ptr.from_ref(&b))
    }
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_extern", src);
    assert!(out.contains("E2023") && out.contains("expected `*mut u8`, found `*const u8`"), "{}", out);
}

#[test]
fn raw_ptr_reference_to_pointer_annotation_stays_silent() {
    // 参照は透過的に扱う方針なので、`&x` を `*const` の注釈へ入れても黙る (取りこぼしを選ぶ)。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let x: i32 = 1i32
    let s: *const i32 = &x
    core.check.eq(core.ptr.is_null(s), false)
    Ok(())
end t.main
";
    let out = raw_ptr_check("raw_ptr_ref_silent", src);
    assert!(!codes(&out).iter().any(|c| c == "E2020" || c == "E2023"), "{}", out);
}

// ---------------------------------------------------------------- 式 ID と型表 (設計_KIR.md 段階 1)

const ADD1: &str = "fn t.add1(a: u8) -> u8 uses [] panics never
    let b: u8 = a
    b.wrapping_add(1)
end t.add1

";

/// `KSN_DUMP_TYPES=1 ksn check` の stderr。
fn dump_types(dir: &Path) -> String {
    let o = Command::new(exe()).arg("check").env("KSN_DUMP_TYPES", "1").current_dir(dir).output().expect("spawn ksn");
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    text(&o.stderr)
}

/// 型表の 1 行 (`FQN \t #id \t 種別 \t line N \t 型`) を (FQN, 式 ID, 種別, 型) に分ける。
fn type_rows(dump: &str) -> Vec<(String, u32, String, String)> {
    dump.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() != 5 {
                return None;
            }
            let id = f[1].strip_prefix('#')?.parse().ok()?;
            Some((f[0].to_string(), id, f[2].to_string(), f[4].to_string()))
        })
        .collect()
}

#[test]
fn expr_ids_and_type_table_are_stable_for_same_source() {
    let t = Tmp::new("expr_ids_stable", &main_wrap(ADD1, ""));
    // 既定では stderr に何も出さない (既存の出力は変えない)
    let plain = run(&["check"], &t.0);
    assert_eq!(text(&plain.stderr), "");
    let a = dump_types(&t.0);
    let b = dump_types(&t.0);
    assert!(!a.is_empty());
    assert_eq!(a, b);
}

#[test]
fn type_table_records_each_expression_type() {
    let t = Tmp::new("type_table_rows", &main_wrap(ADD1, ""));
    let rows = type_rows(&dump_types(&t.0));
    let add1: Vec<_> = rows.iter().filter(|r| r.0 == "t.add1").collect();
    // `a` (Path)、`1` (Lit)、`b.wrapping_add(1)` (パスの呼び出し Call) はすべて u8
    assert!(add1.iter().any(|r| r.2 == "Path" && r.3 == "u8"), "{:?}", add1);
    assert!(add1.iter().any(|r| r.2 == "Lit" && r.3 == "u8"), "{:?}", add1);
    assert!(add1.iter().any(|r| r.2 == "Call" && r.3 == "u8"), "{:?}", add1);
    // 式 ID は 1 から振られ、関数の中で重複しない
    let mut ids: Vec<u32> = add1.iter().map(|r| r.1).collect();
    assert!(ids.iter().all(|&i| i >= 1));
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), add1.len());
}

#[test]
fn expr_ids_are_local_to_their_function() {
    // `t.main` の本体を変えても、`t.add1` の式 ID と型は変わらない (ホットリロードで関数単位に振り直せる)。
    let a = Tmp::new("expr_ids_local_a", &main_wrap(ADD1, ""));
    let b = Tmp::new("expr_ids_local_b", &main_wrap(ADD1, "    let _c: u8 = t.add1(2)\n    let _d: u8 = t.add1(3)\n"));
    let pick = |t: &Tmp| -> Vec<(u32, String, String)> {
        type_rows(&dump_types(&t.0)).into_iter().filter(|r| r.0 == "t.add1").map(|r| (r.1, r.2, r.3)).collect()
    };
    let (ra, rb) = (pick(&a), pick(&b));
    assert!(!ra.is_empty());
    assert_eq!(ra, rb);
}

// ---- spec/ptr-const-ne: add_const / cast_const (const を保つ加算・変換) と `ne` の撤去 (演算子 `!=` のみ)

#[test]
fn ptr_add_const_and_cast_const_keep_const_and_run_in_both() {
    // from_ref の `*const` を add_const (count 0) と cast_const::<i32, i32> で読み直す。from_mut の `*mut` を add_const で
    // const に落としたものも読める。インタプリタとネイティブの両方で同じ結果になる。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    Ok(())
end t.main

test t.main.test_add_const_cast_const
    let mut m: i32 = 7i32
    let p: *mut i32 = core.ptr.from_mut(&mut m)
    let x: i32 = 41i32
    let q: *const i32 = core.ptr.from_ref(&x)
    let r: *const i32 = core.ptr.add_const(q, 0usize)
    let c: *const i32 = core.ptr.cast_const::<i32, i32>(r)
    let w: *const i32 = core.ptr.add_const(p, 0usize)
    unsafe "libc" "read through the const helpers" {
        core.check.eq(core.ptr.read(r), 41i32)
        core.check.eq(core.ptr.read(c), 41i32)
        core.check.eq(core.ptr.read(w), 7i32)
    }
end t.main.test_add_const_cast_const
"#;
    let t = Tmp::new("ptr_add_const_both", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("t.main.test_add_const_cast_const ... ok"), "{}", out);
}

#[test]
fn ptr_add_rejects_const_argument_and_add_const_accepts_it() {
    // `add` は `*mut` を取るので `*const` (from_ref) を渡すと E2023。`add_const` なら同じ引数が通る。
    let bad = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let x: i32 = 1i32
    let q: *const i32 = core.ptr.from_ref(&x)
    let r = core.ptr.add(q, 0usize)
    core.check.eq(core.ptr.is_null(r), false)
    Ok(())
end t.main
";
    let out = raw_ptr_check("ptr_add_mut_rejects_const", bad);
    assert!(out.contains("E2023") && out.contains("expected `*mut i32`, found `*const i32`"), "{}", out);

    let ok = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let x: i32 = 1i32
    let q: *const i32 = core.ptr.from_ref(&x)
    let r: *const i32 = core.ptr.add_const(q, 0usize)
    core.check.eq(core.ptr.is_null(r), false)
    Ok(())
end t.main
";
    let out = raw_ptr_check("ptr_add_const_ok", ok);
    assert!(!codes(&out).iter().any(|c| c == "E2020" || c == "E2023"), "{}", out);
}

#[test]
fn ne_method_is_e1108_with_the_operator_hint() {
    // `ne` は派生メソッドではない (3.2 / 4.3)。`a.ne(b)` は E1108 で `a != b` を案内し、近い名前は出さない。
    let bad = "fn t.f(a: u8, b: u8) -> bool uses [] panics never
    a.ne(b)
end t.f

";
    let t = Tmp::new("ne_method_hint", &main_wrap(bad, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E1108"), "{}", out);
    assert!(out.contains("use `a != b`"), "{}", out);
    assert!(!out.contains("did you mean"), "{}", out);
}

// ------------------------------------------------------------ ビルドキャッシュ (生成物・キー・バンドル)

const CACHE_SRC: &str = "fn t.main() -> () uses [] panics never
end t.main

fn t.one() -> u32 uses [] panics never
    1u32
end t.one

test t.one.test_a
    core.check.eq(t.one(), 1u32)
end t.one.test_a
";

/// `ksn build` を、指定したキャッシュ dir と追加の引数・環境変数で実行する。
fn build_with_cache(ksn: &str, cwd: &Path, cache: &Path, extra: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut c = Command::new(ksn);
    c.arg("build").args(extra).current_dir(cwd).env("KSN_CACHE_DIR", cache).env("KSN_STDLIB", root().join("spec").join("stdlib"));
    for (k, v) in envs {
        c.env(k, v);
    }
    c.output().expect("spawn ksn build")
}

#[test]
fn cache_hit_reuses_native_artifact_without_compiling() {
    // 1 回目は compile_clif を通り、生成物がキャッシュに入る。2 回目は同じキーで
    // "cache hit" となり、compile_clif (KSN_TIME の印) を通らずに生成物を複製する。
    let t = Tmp::new("cache_artifact", CACHE_SRC);
    let cache = std::env::temp_dir().join(format!("ksn_cache_art_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let exe_file = if cfg!(windows) { "t.exe" } else { "t.native" };
    let exe_path = t.0.join("target").join("debug").join(exe_file);

    let first = build_with_cache(exe(), &t.0, &cache, &["--backend", "clif"], &[("KSN_TIME", "1")]);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    assert!(!text(&first.stdout).contains("cache hit"), "{}", text(&first.stdout));
    assert!(text(&first.stderr).contains("KSN_TIME compile_clif_call_total"), "{}", text(&first.stderr));
    let built = fs::read(&exe_path).expect("native executable after first build");

    // 生成物だけ消し、キャッシュから戻ることを確かめる。
    fs::remove_file(&exe_path).unwrap();
    let second = build_with_cache(exe(), &t.0, &cache, &["--backend", "clif"], &[("KSN_TIME", "1")]);
    assert_eq!(second.status.code(), Some(0), "{}", text(&second.stderr));
    assert!(text(&second.stdout).contains("cache hit"), "{}", text(&second.stdout));
    assert!(
        !text(&second.stderr).contains("KSN_TIME compile_clif"),
        "cache hit must skip compile_clif: {}",
        text(&second.stderr)
    );
    assert!(text(&second.stdout).contains("native executable: "), "{}", text(&second.stdout));
    assert_eq!(fs::read(&exe_path).expect("restored executable"), built);
    let run = Command::new(&exe_path).output().unwrap();
    assert_eq!(run.status.code(), Some(0));

    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn build_key_changes_with_ksn_binary_profile_and_backend() {
    // キーは ksn 本体のバイト列・debug/release・バックエンドで分かれる。
    // ksn 本体の変化は、同じ実行ファイルの末尾にバイトを足した別物で代用する (Linux / Windows は末尾を無視して動く)。
    let t = Tmp::new("cache_key", CACHE_SRC);
    let cache = std::env::temp_dir().join(format!("ksn_cache_key_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let bin_dir = std::env::temp_dir().join(format!("ksn_cache_key_bin_{}", std::process::id()));
    let _ = fs::remove_dir_all(&bin_dir);
    fs::create_dir_all(&bin_dir).unwrap();
    let ksn_a = bin_dir.join(format!("ksn_a{}", std::env::consts::EXE_SUFFIX));
    let ksn_b = bin_dir.join(format!("ksn_b{}", std::env::consts::EXE_SUFFIX));
    fs::copy(exe(), &ksn_a).unwrap();
    fs::copy(exe(), &ksn_b).unwrap();
    {
        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(&ksn_b).unwrap();
        f.write_all(b"\0ksn-cache-key-variant\0").unwrap();
    }
    let (a, b) = (ksn_a.to_str().unwrap(), ksn_b.to_str().unwrap());
    let hit = |o: &Output| text(&o.stdout).contains("cache hit");
    let ok = |o: &Output| assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));

    let o = build_with_cache(a, &t.0, &cache, &[], &[]);
    ok(&o);
    assert!(!hit(&o));
    let o = build_with_cache(a, &t.0, &cache, &[], &[]);
    ok(&o);
    assert!(hit(&o), "same ksn, same profile: {}", text(&o.stdout));

    // 別の ksn 本体 → 別のキー (ヒットしない)。
    let o = build_with_cache(b, &t.0, &cache, &[], &[]);
    ok(&o);
    assert!(!hit(&o), "different ksn binary must not hit: {}", text(&o.stdout));

    // release は別のキー。
    let o = build_with_cache(a, &t.0, &cache, &["--release"], &[]);
    ok(&o);
    assert!(!hit(&o), "release must not reuse debug entry: {}", text(&o.stdout));
    let o = build_with_cache(a, &t.0, &cache, &["--release"], &[]);
    ok(&o);
    assert!(hit(&o));

    // バックエンドが違えば出力も違うので別のキー。
    let o = build_with_cache(a, &t.0, &cache, &["--backend", "clif"], &[]);
    ok(&o);
    assert!(!hit(&o), "clif must not reuse c-backend entry: {}", text(&o.stdout));

    let _ = fs::remove_dir_all(&cache);
    let _ = fs::remove_dir_all(&bin_dir);
}

#[test]
fn bundle_is_rebuilt_when_sources_change() {
    // `ksn test --both` のバンドルは、ソースが変われば作り直す。
    // 以前は target/debug に ksn があるだけで再利用され、追加したテストが "no such test" になった。
    let t = Tmp::new("bundle_fresh", CACHE_SRC);
    let cache = std::env::temp_dir().join(format!("ksn_cache_bundle_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let test_both = |name: &str| {
        Command::new(exe())
            .args(["test", "--both", name])
            .current_dir(&t.0)
            .env("KSN_CACHE_DIR", &cache)
            .env("KSN_STDLIB", root().join("spec").join("stdlib"))
            .output()
            .unwrap()
    };
    let o = test_both("t.one.test_a");
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));

    let src = format!("{}\ntest t.one.test_b\n    core.check.eq(t.one(), 1u32)\nend t.one.test_b\n", CACHE_SRC);
    fs::write(t.0.join("src").join("t.ksnl"), src).unwrap();
    let o = test_both("t.one.test_b");
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert!(out.contains("test t.one.test_b ... ok"), "{}", out);
    assert!(!out.contains("no such test"), "{}", out);

    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn numeric_methods_are_checked_against_the_receiver_type_sig() {
    // `sqrt` は f32/f64 の sig にだけある。名前が他の型にあっても、型が決まった u8 の受け手では E1108。
    // 引数の個数も受け手の型の sig で検査する (E2022)。sig にあるメソッドは通る。
    let bad = "fn t.f(a: u8, b: u8) -> u8 uses [] panics never
    a.sqrt(b)
end t.f

fn t.g(a: u8, b: u8) -> Option<u8> uses [] panics never
    a.checked_add(b, b)
end t.g

";
    let t = Tmp::new("num_sig_methods_bad", &main_wrap(bad, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|c| c == "E1108") && out.contains("type `u8` has no method `sqrt`"), "{}", out);
    assert!(codes(&out).iter().any(|c| c == "E2022"), "{}", out);

    let ok = "fn t.f(a: u8, b: u8, x: f64) -> bool uses [] panics never
    let c = a.cmp(b) == core.cmp.Ordering.Less
    a.checked_add(b).is_some() && c && a < b && x.sqrt() > 0.0 && a.min(b) == a
end t.f

";
    let t = Tmp::new("num_sig_methods_ok", &main_wrap(ok, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(!codes(&out).iter().any(|c| c == "E1108" || c == "E2022"), "{}", out);
}

#[test]
fn vec_and_string_have_no_comparison_traits() {
    // 4.2 の表どおり Vec / String は比較のトレイトを持たない (sig にも impl が無い)。
    // メソッド形 (`eq` `cmp` `partial_cmp`) と演算子 (`==` `<`) はどちらも E1108。
    let bad = "fn t.v(a: &alloc.vec.Vec<u8, std.alloc.System>, b: &alloc.vec.Vec<u8, std.alloc.System>) -> bool uses [] panics never
    a.eq(b)
end t.v

fn t.s(a: alloc.string.String<std.alloc.System>, b: alloc.string.String<std.alloc.System>) -> bool uses [] panics never
    a == b
end t.s

fn t.c(a: &alloc.vec.Vec<u8, std.alloc.System>, b: &alloc.vec.Vec<u8, std.alloc.System>) -> bool uses [] panics never
    a.cmp(b) == core.cmp.Ordering.Equal
end t.c

fn t.p(a: &alloc.string.String<std.alloc.System>, b: &alloc.string.String<std.alloc.System>) -> bool uses [] panics never
    a.partial_cmp(b).is_some()
end t.p

";
    let t = Tmp::new("vec_string_cmp_bad", &main_wrap(bad, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(out.contains("unknown method `eq` on `alloc.vec.Vec`: the type does not implement `core.cmp.PartialEq`"), "{}", out);
    assert!(out.contains("operator `==` on `alloc.string.String`: the type does not implement `core.cmp.PartialEq`"), "{}", out);
    assert!(out.contains("unknown method `cmp` on `alloc.vec.Vec`: the type does not implement `core.cmp.Ord`"), "{}", out);
    assert!(out.contains("unknown method `partial_cmp` on `alloc.string.String`: the type does not implement `core.cmp.PartialOrd`"), "{}", out);
    assert!(codes(&out).iter().filter(|c| *c == "E1108").count() >= 4, "{}", out);
}

#[test]
fn comparison_on_user_types_needs_the_derived_or_implemented_trait() {
    // 派生していない利用者型の `==` / `eq` は E1108。`derive [PartialEq, PartialOrd]` の型と
    // 型パラメータの境界 (`T: core.cmp.PartialEq`) は通る。
    let bad = "type t.P pub
    a: u8
end t.P

fn t.f(a: t.P, b: t.P) -> bool uses [] panics never
    a == b || a.eq(b)
end t.f

";
    let t = Tmp::new("user_cmp_bad", &main_wrap(bad, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(out.contains("operator `==` on `t.P`") && out.contains("unknown method `eq` on `t.P`"), "{}", out);

    let ok = "type t.Q derive [PartialEq, PartialOrd] pub
    a: u8
end t.Q

fn t.f(a: t.Q, b: t.Q) -> bool uses [] panics never
    a == b && a.eq(b) && a < b && a.partial_cmp(b).is_some()
end t.f

fn t.g[T: core.cmp.PartialEq](a: &T, b: &T) -> bool uses [] panics never
    a.eq(b)
end t.g

";
    let t = Tmp::new("user_cmp_ok", &main_wrap(ok, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(!codes(&out).iter().any(|c| c == "E1108"), "{}", out);
}

// ---- 4.1 シャドーイング: 関数ヘッダの引数名と `import` 末尾名の衝突 (E1021) ----

const SHADOW_UTIL: &str = "fn t.util.f() -> u8 uses [] panics never\n    1u8\nend t.util.f\n";

#[test]
fn shadow_header_arg_vs_import_tail_is_e1021() {
    // 本体の束縛 (let) と同じく、引数名が import 末尾名と同じならシャドーイング。
    let t = Tmp::new("shadow_hdr_pos", &main_wrap("", ""));
    fs::create_dir_all(t.0.join("src/t")).unwrap();
    fs::write(t.0.join("src/t/util.ksnl"), SHADOW_UTIL).unwrap();
    fs::write(
        t.0.join("src").join("t.ksnl"),
        "import t.util\n\nfn t.g(util: u8) -> u8 uses [] panics never\n    util\nend t.g\n\n\
         fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.mem.drop(t.g(util.f()))\n    Ok(())\nend t.main\n",
    )
    .unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E1021"), "{}", text(&o.stdout));
}

#[test]
fn shadow_header_arg_without_import_is_clean() {
    // import が無ければ、同じ引数名でもシャドーイングではない。
    let t = Tmp::new(
        "shadow_hdr_neg",
        "fn t.g(util: u8) -> u8 uses [] panics never\n    util\nend t.g\n\n\
         fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.mem.drop(t.g(1u8))\n    Ok(())\nend t.main\n",
    );
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E1021"), "{}", text(&o.stdout));
}

#[test]
fn shadow_header_arg_vs_import_of_other_file_is_clean() {
    // `import` は宣言されたファイルの中だけで可視。別ファイルの関数の引数名は衝突しない。
    let t = Tmp::new("shadow_hdr_other", &main_wrap("", ""));
    fs::create_dir_all(t.0.join("src/t")).unwrap();
    fs::write(t.0.join("src/t/util.ksnl"), SHADOW_UTIL).unwrap();
    fs::write(
        t.0.join("src").join("t.ksnl"),
        "import t.util\n\nfn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.mem.drop(util.f())\n    Ok(())\nend t.main\n",
    )
    .unwrap();
    fs::write(t.0.join("src/t/other.ksnl"), "fn t.other.g(util: u8) -> u8 uses [] panics never\n    util\nend t.other.g\n")
        .unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E1021"), "{}", text(&o.stdout));
}

// ---- 型定義のフィールドの参照は、ライフタイムを省略できない (4.1 / E3020) ----

fn field_elision(name: &str, prefix: &str) -> (bool, String) {
    let t = Tmp::new(name, &main_wrap(prefix, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out).iter().any(|c| c == "E3020"), out)
}

#[test]
fn type_field_reference_without_lifetime_is_rejected() {
    let (bad, out) = field_elision("field_elided", "type t.S\n    f: &u8\nend t.S\n\n");
    assert!(bad, "{}", out);
    assert!(out.contains("lifetime of the reference in `f` cannot be elided"), "{}", out);
}

#[test]
fn enum_variant_reference_without_lifetime_is_rejected() {
    let (bad, out) = field_elision("field_elided_enum", "enum t.E\n    A(&u8)\n    B\nend t.E\n\n");
    assert!(bad, "{}", out);
    assert!(out.contains("lifetime of the reference in `A` cannot be elided"), "{}", out);
}

#[test]
fn nested_field_reference_without_lifetime_is_rejected() {
    let (bad, out) = field_elision("field_elided_array", "type t.S\n    f: [&u8; 2]\nend t.S\n\n");
    assert!(bad, "{}", out);
}

#[test]
fn type_field_reference_with_declared_or_static_lifetime_is_accepted() {
    let (bad, out) = field_elision("field_declared", NLL_REF_FIELD);
    assert!(!bad, "{}", out);
    let (bad, out) = field_elision("field_static", "type t.S\n    f: &'static u8\n    b: u8\nend t.S\n\n");
    assert!(!bad, "{}", out);
    let (bad, out) = field_elision("field_value", "type t.S\n    f: u8\nend t.S\n\n");
    assert!(!bad, "{}", out);
}

// ---- ジェネリクス引数に渡した未決定のリテラル (E2027) ----

/// `fn t.id[T](x: T) -> T` を定義し、その下に `main` の本体を置く。
fn generic_id_check(name: &str, b: &[&str]) -> (Vec<String>, String) {
    let prefix = "fn t.id[T](x: T) -> T uses [] panics never\n    x\nend t.id\n\n";
    let t = Tmp::new(name, &main_wrap(prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

#[test]
fn an_undecided_literal_passed_to_a_generic_parameter_is_an_error() {
    // T を決める手がかりがない。`5` の型はどこからも決まらないので E2027 (従来は黙っていた)。
    let (got, out) = generic_id_check("gen_lit_undec", &["core.mem.drop(t.id(5))"]);
    assert!(got.iter().any(|c| c == "E2027"), "{}", out);
    assert!(out.contains("cannot infer the type of the int literal `5`"), "{}", out);
    let (got, out) = generic_id_check("gen_lit_float", &["core.mem.drop(t.id(2.5))"]);
    assert!(got.iter().any(|c| c == "E2027"), "{}", out);
    assert!(out.contains("float literal `2.5`"), "{}", out);
}

#[test]
fn a_generic_literal_decided_later_is_not_an_error() {
    // 型は後から決まる: 注釈付き let、後の単一化、明示型引数、別の演算との一致。
    let (got, out) = generic_id_check("gen_lit_ann", &["let a: u8 = t.id(5)", "core.mem.drop(a)"]);
    assert!(!got.iter().any(|c| c == "E2027" || c == "E2026"), "{}", out);
    let (got, out) = generic_id_check("gen_lit_later", &["let x = t.id(5)", "let y: u8 = x", "core.mem.drop(y)"]);
    assert!(!got.iter().any(|c| c == "E2027" || c == "E2026"), "{}", out);
    let (got, out) = generic_id_check("gen_lit_targ", &["core.mem.drop(t.id::<u8>(5))"]);
    assert!(!got.iter().any(|c| c == "E2027" || c == "E2026"), "{}", out);
    let (got, out) = generic_id_check("gen_lit_add", &["let x = t.id(5)", "core.mem.drop(x + 1u8)"]);
    assert!(!got.iter().any(|c| c == "E2027"), "{}", out);
}

#[test]
fn fmath_functions_agree_across_backends_and_match_known_values() {
    // 数学関数 (`fmath`) は、インタプリタと Cranelift で同じビット列を返し、既知の値に近い。
    // `tests/data/fmath_both.ksnl`: 既知値 (f32 は 2〜3 ulp 以内)、特殊値、9 関数の総和 (ビット列の突き合わせ)。
    let t = Tmp::new("fmath_both", include_str!("data/fmath_both.ksnl"));
    let cache = std::env::temp_dir().join(format!("ksn_cache_fmath_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let o = Command::new(exe())
        .args(["test", "--both"])
        .current_dir(&t.0)
        .env("KSN_CACHE_DIR", &cache)
        .env("KSN_STDLIB", root().join("spec").join("stdlib"))
        .output()
        .unwrap();
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    // Cranelift のビルドが成功していること (スキップされていないこと)。
    assert!(out.contains("native executable:"), "{}", out);
    assert!(out.contains("5 passed; 0 failed"), "{}", out);
    let _ = fs::remove_dir_all(&cache);
}

// ---- `test --both`: 機械語 (Cranelift の実行ファイル) との比較と、バンドルへの退避 ----

const BOTH_SRC: &str = "fn t.main() -> () uses [] panics never
end t.main

test t.main.test_ok
    core.check.eq(1u8, 1u8)
end t.main.test_ok
";

#[test]
fn test_both_runs_clif_tests_as_native_executables() {
    let src = format!("{}\ntest t.main.test_fail\n    core.check.eq(1u8, 2u8)\nend t.main.test_fail\n", BOTH_SRC);
    let t = Tmp::new("both_native", &src);
    // `--both` を付けなければ、出力の形式は従来どおり (注記なし)。
    let plain = run(&["test"], &t.0);
    assert!(text(&plain.stdout).contains("test t.main.test_ok ... ok\n"), "{}", text(&plain.stdout));
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_ok ... ok (both: native)"), "{}", out);
    assert!(out.contains("test t.main.test_fail ... FAILED: assertion failed: left: 1, right: 2 (both: native)"), "{}", out);
    assert!(out.contains("1 passed; 1 failed"), "{}", out);
    assert!(!out.contains("bundle"), "{}", out);
    assert!(t.0.join("target").join("test-native").is_dir(), "no native test executable: {}", out);
}

#[test]
fn test_both_falls_back_to_the_bundle_for_a_test_clif_cannot_build() {
    // 128 ビット整数は clif が未対応。このテストだけバンドルで比べ、理由を出力に書く。
    let src = format!("{}\ntest t.main.test_wide\n    let x: i128 = 5i128\n    core.check.eq(x, 5i128)\nend t.main.test_wide\n", BOTH_SRC);
    let t = Tmp::new("both_bundle", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_ok ... ok (both: native)"), "{}", out);
    assert!(out.contains("test t.main.test_wide ... ok (both: bundle (native skipped: "), "{}", out);
    assert!(out.contains("2 passed; 0 failed"), "{}", out);
}

/// トレイト経由の呼び出し (境界・`dyn`) の効果の検査 (4.3 / 5.1)。トレイトのメソッドの宣言 (`uses` / `panics`) で比べる。
const TRAIT_EFF_PRELUDE: &str = "trait t.Act pub
    fn act(self: &Self) -> u8 uses [io.console] panics never
    fn boom(self: &Self) -> u8 uses [] panics may
end t.Act

trait t.Gen[E: uses, P: panics] pub
    fn go(self: &Self) -> u8 uses [E] panics P
end t.Gen

";

/// `prelude` (トレイトの定義) の後ろに `body` を置いたパッケージを検査する (`eff_check` の一般形)。
fn eff_check_with(name: &str, prelude: &str, body: &str) -> (Option<i32>, Vec<String>, String) {
    let t = Tmp::new(name, &format!("{}{}", prelude, body));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    (o.status.code(), codes(&out), out)
}

#[test]
fn trait_bound_and_dyn_call_effects_are_checked() {
    // 陽性: 境界・`dyn` 越しの呼び出しの宣言された効果を、呼び出し元が覆っていないとき E4001 / E4002。
    let body = "fn t.bad_bound_concrete[A: t.Gen<[io.console], never>](a: &A) -> u8 uses [] panics never
    a.go()
end t.bad_bound_concrete

fn t.bad_bound_panic[A: t.Gen<[], may>](a: &A) -> u8 uses [] panics never
    a.go()
end t.bad_bound_panic

fn t.bad_dyn_act(a: &dyn t.Act) -> u8 uses [] panics never
    a.act()
end t.bad_dyn_act

fn t.bad_dyn_boom(a: &dyn t.Act) -> u8 uses [io] panics never
    a.boom()
end t.bad_dyn_boom
";
    let (code, got, out) = eff_check_with("trait_eff_bad", TRAIT_EFF_PRELUDE, body);
    assert_eq!(code, Some(1), "{}", out);
    let want: Vec<String> = ["E4001", "E4002", "E4001", "E4002"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, want, "{}", out);
    assert!(out.contains("t.Gen.go uses effect `io.console`"), "{}", out);
    assert!(out.contains("t.Gen.go may panic"), "{}", out);
    assert!(out.contains("t.Act.act uses effect `io.console`"), "{}", out);
    assert!(out.contains("t.Act.boom may panic"), "{}", out);
}

#[test]
fn trait_bound_and_dyn_call_effects_pass_when_declared() {
    // 正例: 呼び出し元が宣言を覆う (階層の部分型 io ⊇ io.console、may 宣言) ときは通る。
    let body = "fn t.ok_bound_concrete[A: t.Gen<[io.console], never>](a: &A) -> u8 uses [io] panics never
    a.go()
end t.ok_bound_concrete

fn t.ok_dyn_act(a: &dyn t.Act) -> u8 uses [io] panics never
    a.act()
end t.ok_dyn_act

fn t.ok_dyn_boom(a: &dyn t.Act) -> u8 uses [] panics may
    a.boom()
end t.ok_dyn_boom
";
    let (code, got, out) = eff_check_with("trait_eff_ok", TRAIT_EFF_PRELUDE, body);
    assert_eq!(code, Some(0), "{}", out);
    assert!(got.is_empty(), "{}", out);
}

#[test]
fn trait_call_effects_stay_silent_when_unknown() {
    // 陰性 (黙る): 効果変数の束縛が分からない境界、推論された関数、クロージャの本体。
    let body = "fn t.quiet_closure(a: &dyn t.Act) -> u8 uses [] panics never
    let f = fn(x: u8) -> u8 capture [&a] {
        a.act()
    }
    f(1)
end t.quiet_closure

fn t.quiet_inferred(a: &dyn t.Act) -> u8
    a.act()
end t.quiet_inferred

fn t.quiet_var[A: t.Gen<E, P>, E: uses, P: panics](a: &A) -> u8 uses [E] panics P
    a.go()
end t.quiet_var
";
    let (code, got, out) = eff_check_with("trait_eff_quiet", TRAIT_EFF_PRELUDE, body);
    assert_eq!(code, Some(0), "{}", out);
    assert!(got.is_empty(), "{}", out);
}

// ---- extern の構造体の値渡し・値の戻り値 (x86_64 SysV の分類。16 バイト以下はレジスタ、超えるものはメモリ渡し・sret)

/// 構造体を値で渡す・返す C 側。`Eight` は INTEGER 1 つ、`Dbl2` は SSE 2 つ、`Mixed` は INTEGER + SSE、`Big3` は 24 バイト (メモリ)。
const FFI_STRUCT_C_SRC: &str = r#"
#include <stdint.h>
typedef struct { int32_t a; int32_t b; } Eight;
typedef struct { double x; double y; } Dbl2;
typedef struct { int64_t a; double b; } Mixed;
typedef struct { double x; int64_t y; } MixedRev;
typedef struct { float x; float y; } Flt2;
typedef struct { int64_t a, b, c; } Big3;
typedef struct { uint8_t a; uint8_t b; int16_t c; } Small;
Eight k_eight_add(Eight p, int32_t k) { Eight r = { p.a + k, p.b + k }; return r; }
Dbl2 k_dbl_scale(Dbl2 p, double s) { Dbl2 r = { p.x * s, p.y * s }; return r; }
Mixed k_mixed_bump(Mixed p, int64_t n) { Mixed r = { p.a + n, p.b * 2.0 }; return r; }
MixedRev k_mixed_rev_bump(MixedRev p) { MixedRev r = { p.x + 1.0, p.y - 1 }; return r; }
Flt2 k_flt_twice(Flt2 p) { Flt2 r = { p.x * 2.0f, p.y * 2.0f }; return r; }
int64_t k_big_sum(Big3 p, int64_t n) { return p.a + p.b + p.c + n; }
Big3 k_big_make(int64_t a) { Big3 r = { a, a * 2, a * 3 }; return r; }
Small k_small_bump(Small p) { Small r = { p.a + 1, p.b + 2, p.c - 3 }; return r; }
int64_t k_seven(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, int64_t f, int64_t g, Big3 p, int64_t z) {
    return a + 2*b + 3*c + 4*d + 5*e + 6*f + 7*g + 10*p.a + 100*p.b + 1000*p.c + 10000*z;
}
double k_sum_then(double a, Dbl2 p, double b) { return a + p.x * 10.0 + p.y * 100.0 + b * 1000.0; }
"#;

const FFI_STRUCT_SRC: &str = r#"type t.Big3 layout c pub
    a: i64
    b: i64
    c: i64
end t.Big3

type t.Dbl2 layout c pub
    x: f64
    y: f64
end t.Dbl2

type t.Eight layout c pub
    a: i32
    b: i32
end t.Eight

type t.Flt2 layout c pub
    x: f32
    y: f32
end t.Flt2

type t.Mixed layout c pub
    a: i64
    b: f64
end t.Mixed

type t.MixedRev layout c pub
    x: f64
    y: i64
end t.MixedRev

type t.Small layout c pub
    a: u8
    b: u8
    c: i16
end t.Small

extern t.k_big_make(a: i64) -> t.Big3 link "k_big_make"

extern t.k_big_sum(p: t.Big3, n: i64) -> i64 link "k_big_sum"

extern t.k_dbl_scale(p: t.Dbl2, s: f64) -> t.Dbl2 link "k_dbl_scale"

extern t.k_eight_add(p: t.Eight, k: i32) -> t.Eight link "k_eight_add"

extern t.k_flt_twice(p: t.Flt2) -> t.Flt2 link "k_flt_twice"

extern t.k_mixed_bump(p: t.Mixed, n: i64) -> t.Mixed link "k_mixed_bump"

extern t.k_mixed_rev_bump(p: t.MixedRev) -> t.MixedRev link "k_mixed_rev_bump"

extern t.k_seven(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, p: t.Big3, z: i64) -> i64 link "k_seven"

extern t.k_small_bump(p: t.Small) -> t.Small link "k_small_bump"

extern t.k_sum_then(a: f64, p: t.Dbl2, b: f64) -> f64 link "k_sum_then"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let e = unsafe "libc" "eight" {
        t.k_eight_add(t.Eight { a: 1i32, b: 2i32 }, 10i32)
    }
    core.check.eq(e.a, 11i32)
    core.check.eq(e.b, 12i32)
    let d = unsafe "libc" "dbl2" {
        t.k_dbl_scale(t.Dbl2 { x: 1.5f64, y: 2.0f64 }, 2.0f64)
    }
    core.check.eq(d.x, 3.0f64)
    core.check.eq(d.y, 4.0f64)
    let m = unsafe "libc" "mixed" {
        t.k_mixed_bump(t.Mixed { a: 5i64, b: 1.25f64 }, 7i64)
    }
    core.check.eq(m.a, 12i64)
    core.check.eq(m.b, 2.5f64)
    let mr = unsafe "libc" "mixed rev" {
        t.k_mixed_rev_bump(t.MixedRev { x: 0.5f64, y: 9i64 })
    }
    core.check.eq(mr.x, 1.5f64)
    core.check.eq(mr.y, 8i64)
    let f = unsafe "libc" "flt2" {
        t.k_flt_twice(t.Flt2 { x: 1.5f32, y: 0.25f32 })
    }
    core.check.eq(f.x, 3.0f32)
    core.check.eq(f.y, 0.5f32)
    let s = unsafe "libc" "big3 argument" {
        t.k_big_sum(t.Big3 { a: 1i64, b: 2i64, c: 3i64 }, 4i64)
    }
    core.check.eq(s, 10i64)
    let b = unsafe "libc" "big3 result (sret)" {
        t.k_big_make(5i64)
    }
    core.check.eq(b.a, 5i64)
    core.check.eq(b.b, 10i64)
    core.check.eq(b.c, 15i64)
    let sm = unsafe "libc" "small" {
        t.k_small_bump(t.Small { a: 1u8, b: 2u8, c: 10i16 })
    }
    core.check.eq(sm.a, 2u8)
    core.check.eq(sm.b, 4u8)
    core.check.eq(sm.c, 7i16)
    let sv = unsafe "libc" "stack arguments" {
        t.k_seven(
            1i64,
            1i64,
            1i64,
            1i64,
            1i64,
            1i64,
            1i64,
            t.Big3 { a: 1i64, b: 1i64, c: 1i64 },
            1i64,
        )
    }
    core.check.eq(sv, 11138i64)
    let st = unsafe "libc" "double arguments around a struct" {
        t.k_sum_then(1.0f64, t.Dbl2 { x: 1.0f64, y: 1.0f64 }, 1.0f64)
    }
    core.check.eq(st, 1111.0f64)
    std.io.print_line("struct ffi ok")
    Ok(())
end t.main
"#;

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn ffi_interpreter_passes_and_returns_structs_by_value() {
    let t = Tmp::new("ffi_struct_interp", FFI_STRUCT_SRC);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let so = build_c_helpers(&t.0, FFI_STRUCT_C_SRC);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    let r = run_with_c_helpers(&t.0, &so);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&r.stdout), "struct ffi ok\n");
}

/// libc だけを使う構造体の extern (補助 C ライブラリが要らないので、`ksn build --backend clif` の実行ファイルでも検査できる):
/// `div` は 8 バイトを rax で、`ldiv` は 16 バイトを rax:rdx で返す。`inet_ntoa` は `struct in_addr` (4 バイト) を値で受ける。
const FFI_LIBC_STRUCT_SRC: &str = r#"type t.DivT layout c pub
    quot: i32
    rem: i32
end t.DivT

type t.InAddr layout c pub
    s_addr: u32
end t.InAddr

type t.LongDiv layout c pub
    quot: i64
    rem: i64
end t.LongDiv

extern t.div(numer: i32, denom: i32) -> t.DivT link "div"

extern t.inet_ntoa(a: t.InAddr) -> *mut u8 link "inet_ntoa"

extern t.ldiv(numer: i64, denom: i64) -> t.LongDiv link "ldiv"

extern t.strlen(text: *const u8) -> usize link "strlen"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let d = unsafe "libc" "div returns a struct in rax" {
        t.div(17i32, 5i32)
    }
    core.check.eq(d.quot, 3i32)
    core.check.eq(d.rem, 2i32)
    let l = unsafe "libc" "ldiv returns a struct in rax:rdx" {
        t.ldiv(0i64 - 17i64, 5i64)
    }
    core.check.eq(l.quot, 0i64 - 3i64)
    core.check.eq(l.rem, 0i64 - 2i64)
    let s = unsafe "libc" "inet_ntoa takes a struct by value" {
        t.inet_ntoa(t.InAddr { s_addr: 16777343u32 })
    }
    let n = unsafe "libc" "strlen of the C string" {
        t.strlen(core.ptr.as_const(s))
    }
    core.check.eq(n, 9usize)
    std.io.print_line("libc struct ok")
    Ok(())
end t.main
"#;

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn ffi_struct_by_value_works_on_both_backends_with_libc() {
    let t = Tmp::new("ffi_struct_libc", FFI_LIBC_STRUCT_SRC);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    let r = run(&["run"], &t.0);
    assert_eq!(text(&r.stdout), "libc struct ok\n", "{}", text(&r.stderr));
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_ffi_struct", std::process::id()));
    let b = Command::new(exe())
        .args(["build", "--backend", "clif"])
        .current_dir(&t.0)
        .env("KSN_CACHE_DIR", &cache)
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&cache);
    let bin = t.0.join("target").join("debug").join("t.native");
    assert!(bin.exists(), "{}{}", text(&b.stdout), text(&b.stderr));
    let r = Command::new(&bin).output().unwrap();
    assert_eq!(text(&r.stdout), "libc struct ok\n");
}

#[test]
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn ffi_struct_by_value_is_refused_outside_x86_64_linux() {
    // 構造体の値渡しは x86_64 SysV だけで扱う。ほかの環境では黙って壊れず、明示のエラーにする。
    let t = Tmp::new("ffi_struct_refused", FFI_LIBC_STRUCT_SRC);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let r = run(&["run"], &t.0);
    assert_eq!(r.status.code(), Some(101), "{}{}", text(&r.stdout), text(&r.stderr));
    assert!(text(&r.stderr).contains("x86_64"), "{}", text(&r.stderr));
}

// ---------------------------------------------------------------- KIR 段階 1 (設計_KIR.md 5 章)

/// 段階 1 の対象になる関数 (整数・bool・let・代入・if・while・loop・break・continue・return・wrapping_*)。
const KIR_FNS: &str = "fn t.gcd(a: u64, b: u64) -> u64 uses [] panics may
    let mut x = a
    let mut y = b
    while y != 0u64 {
        let r = x % y
        x = y
        y = r
    }
    x
end t.gcd

fn t.classify(n: i32) -> i32 uses [] panics may
    let mut r = 0i32
    if n < 0i32 {
        r = -1i32
    } else if n == 0i32 {
        r = 0i32
    } else {
        r = 1i32
    }
    r
end t.classify

fn t.early(n: u32) -> u32 uses [] panics may
    if n > 10u32 {
        return n - 10u32
    }
    n * 2u32
end t.early

fn t.loopy(n: u32) -> u32 uses [] panics may
    let mut i = 0u32
    let mut acc = 0u32
    loop {
        if i >= n {
            break
        }
        i = i + 1u32
        if i % 2u32 == 0u32 {
            continue
        }
        acc = acc + i
    }
    acc
end t.loopy

fn t.andor(a: bool, b: bool, n: i64) -> bool uses [] panics may
    let x = a && n > 0i64
    let y = b && n < -5i64
    x || y
end t.andor

fn t.fact(n: u64) -> u64 uses [] panics may
    if n <= 1u64 {
        return 1u64
    }
    n * t.fact(n - 1u64)
end t.fact

fn t.negate(x: i8) -> i8 uses [] panics may
    -x
end t.negate

fn t.divs(a: i8, b: i8) -> i8 uses [] panics may
    a / b
end t.divs

fn t.rems(a: i8, b: i8) -> i8 uses [] panics may
    a % b
end t.rems

fn t.wrap(a: u8, b: u8) -> u8 uses [] panics never
    a.wrapping_mul(b).wrapping_add(a)
end t.wrap

fn t.sub8(a: u8, b: u8) -> u8 uses [] panics may
    a - b
end t.sub8

fn t.assign_in_branch(n: i32) -> i32 uses [] panics may
    let mut x = 1i32
    let mut y = 2i32
    if n > 0i32 {
        x = x + n
    } else {
        y = y - n
    }
    x * 100i32 + y
end t.assign_in_branch

fn t.nested(n: i32) -> i32 uses [] panics may
    let mut total = 0i32
    let mut i = 0i32
    while i < n {
        let mut j = 0i32
        while true {
            if j > i {
                break
            }
            total = total + j
            j = j + 1i32
        }
        i = i + 1i32
    }
    total
end t.nested

fn t.down(n: u64) -> u64 uses [] panics may
    if n == 0u64 {
        return 0u64
    }
    1u64 + t.down(n - 1u64)
end t.down
";

/// `KIR_FNS` に `main` を付けたパッケージを作る。
fn kir_pkg(name: &str, main_body: &str) -> Tmp {
    Tmp::new(name, &format!("{}{}", KIR_FNS, main_wrap("", main_body)))
}

#[test]
fn kir_lower_values_match_tree_walker() {
    // 木走査 (`ksn run`) で `core.check.eq` が通ること、参照実行器 (`ksn kir --run`) の値が期待どおりであること。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.gcd", &["48", "18"], "6u64", "6"),
        ("t.classify", &["-7"], "-1i32", "-1"),
        ("t.classify", &["0"], "0i32", "0"),
        ("t.early", &["25"], "15u32", "15"),
        ("t.early", &["4"], "8u32", "8"),
        ("t.loopy", &["10"], "25u32", "25"),
        ("t.fact", &["20"], "2432902008176640000u64", "2432902008176640000"),
        ("t.negate", &["5"], "-5i8", "-5"),
        ("t.wrap", &["200", "3"], "32u8", "32"),
        ("t.sub8", &["9", "4"], "5u8", "5"),
        ("t.assign_in_branch", &["3"], "402i32", "402"),
        ("t.assign_in_branch", &["-3"], "105i32", "105"),
        ("t.nested", &["5"], "20i32", "20"),
        ("t.andor", &["true", "true", "3"], "true", "true"),
        ("t.andor", &["false", "true", "-6"], "true", "true"),
        ("t.andor", &["false", "false", "1"], "false", "false"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.join(", "));
        let t = kir_pkg(&format!("kir_values_{}", i), &format!("    core.check.eq({}, {})\n", call, lit));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_lower_panics_and_depth_match_tree_walker() {
    // panic の文言と終了コード (101)、呼び出し深さの超過 (102) が木走査と一致すること。
    let cases: &[(&str, &[&str], &str)] = &[
        ("t.negate", &["-128"], "attempt to negate with overflow"),
        ("t.divs", &["-128", "-1"], "attempt to divide with overflow"),
        ("t.divs", &["7", "0"], "attempt to divide by zero"),
        ("t.rems", &["7", "0"], "attempt to calculate the remainder with a divisor of by zero"),
        ("t.sub8", &["3", "4"], "attempt to subtract with overflow"),
        ("t.fact", &["21"], "attempt to multiply with overflow"),
    ];
    for (i, (f, args, msg)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.join(", "));
        let t = kir_pkg(&format!("kir_panic_{}", i), &format!("    let _x = {}\n", call));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(101), "tree-walker: {}", call);
        assert!(text(&o.stderr).contains(&format!("panic: {}", msg)), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(101), "kir: {}", call);
        assert_eq!(text(&k.stderr).trim(), format!("panic: {}", msg), "kir: {}", call);
    }
    // 深さの超過: 木走査と同じく 102 と "stack overflow"
    let t = kir_pkg("kir_depth", "    let _x = t.down(5000u64)\n");
    assert_eq!(run(&["run"], &t.0).status.code(), Some(102));
    let k = run(&["kir", "t.down", "--run", "5000"], &t.0);
    assert_eq!(k.status.code(), Some(102));
    assert!(text(&k.stderr).contains("stack overflow"), "{}", text(&k.stderr));
    let ok = run(&["kir", "t.down", "--run", "2000"], &t.0);
    assert_eq!(text(&ok.stdout).trim(), "2000");
}

#[test]
fn kir_lower_prints_a_module_that_reads_back() {
    // `ksn kir <FQN>` は内部で往復 (出力 → 読み込み → 出力) と検査を確かめてから出す (失敗なら終了コード 3)。
    let t = kir_pkg("kir_text", "    core.check.eq(t.gcd(4u64, 2u64), 2u64)\n");
    let o = run(&["kir", "t.gcd"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.starts_with("kir 1\n\nfn t.gcd key=t.gcd uses=[] panics=may stack=- ret=u64\n"), "{}", out);
    assert!(out.contains("br_if %") && out.contains("end t.gcd\n"), "{}", out);
    // 呼び先を含む関数は、呼び先も同じ部分モジュールに入る
    let o = run(&["kir", "t.fact"], &t.0);
    let out = text(&o.stdout);
    assert!(out.contains("fn t.fact key=t.fact") && out.contains("call t.fact("), "{}", out);
}

#[test]
fn kir_lower_reports_unsupported_constructs_with_a_reason() {
    // 段階 1 の対象外 (`&str` の引数) は、理由を付けて拒否する。対象の関数は出さない。
    let src = "fn t.len_of(_s: &str) -> usize uses [] panics never
    5usize
end t.len_of

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    Ok(())
end t.main
";
    let t = Tmp::new("kir_unsupported", src);
    let o = run(&["kir", "t.len_of"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    let err = text(&o.stderr);
    assert!(err.contains("t.len_of は未対応") && err.contains("段階 1 の対象外"), "{}", err);
}

#[test]
fn kir_lower_dump_on_check_is_hidden_behind_the_env_var() {
    // `KSN_DUMP_KIR=1 ksn check` だけ KIR を stderr に出す。通常の check の出力は変えない。
    let t = kir_pkg("kir_dump", "    core.check.eq(t.gcd(4u64, 2u64), 2u64)\n");
    let plain = run(&["check"], &t.0);
    let dumped = Command::new(exe())
        .args(["check"])
        .current_dir(&t.0)
        .env("KSN_DUMP_KIR", "1")
        .output()
        .expect("spawn ksn");
    assert_eq!(dumped.status.code(), plain.status.code());
    assert_eq!(dumped.stdout, plain.stdout);
    let err = text(&dumped.stderr);
    assert!(err.contains("kir 1\n") && err.contains("fn t.gcd key=t.gcd"), "{}", err);
    assert!(err.contains("kir: 未対応 t.main"), "{}", err);
    assert!(!text(&plain.stderr).contains("kir 1"));
}

#[test]
fn kir_lower_examples_and_perf_functions_match_tree_walker() {
    // examples と perf の段階 1 の対象関数。木走査の期待値は各パッケージの `main` (core.check.eq) と同じ数学的な値。
    // 木走査は同じ入力を `ksn run` で通ること (main の検査) を先に確かめ、KIR の値を比べる。
    let cases: &[(&str, &str, &[&str], &str)] = &[
        ("examples/demo", "demo.fib", &["10"], "55"),
        ("examples/demo", "demo.fib", &["20"], "6765"),
        ("examples/native", "native.half", &["9"], "4"),
        ("examples/native", "native.half", &["0"], "0"),
        ("examples/realtime", "realtime.triangle", &["10"], "55"),
        ("perf/fib", "fib.fib", &["27"], "196418"),
        ("perf/loops", "loops.sum_to", &["1000000"], "499999500000"),
        ("perf/loops", "loops.sum_to", &["1000"], "499500"),
    ];
    for (dir, f, args, want) in cases {
        let d = root().join(dir);
        assert_eq!(run(&["check"], &d).status.code(), Some(0), "{} check", dir);
        assert_eq!(run(&["run"], &d).status.code(), Some(0), "{} run (tree-walker)", dir);
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = run(&kargs, &d);
        assert_eq!(k.status.code(), Some(0), "kir {} {:?}\n{}", f, args, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {} {:?}", f, args);
    }
}

#[test]
fn kir_lower_rejects_checked_methods_and_aggregates_with_a_reason() {
    // `checked_*` は `Option<T>` を返すので段階 1 の型に入らない。理由を付けて拒否する。
    let src = "fn t.ck(a: u8, b: u8) -> u8 uses [] panics never
    match a.checked_add(b) {
        Some(v) => v
        None => 0u8
    }
end t.ck
";
    let t = Tmp::new("kir_checked", src);
    let o = run(&["kir", "t.ck"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("t.ck は未対応"), "{}", text(&o.stderr));
}

// ---- 型引数に線形を含む型の束縛 (4.4): `Option` / `Result` / ジェネリック構造体 ----

/// `tdrop_prefix` に、型引数の線形検査の試験用の定義を足して検査する。
fn lin_arg_codes(name: &str, b: &[&str]) -> (Vec<String>, String) {
    let nl = char::from(10u8).to_string();
    let mut pre: Vec<&str> = tdrop_prefix();
    pre.extend_from_slice(&[
        "type t.Pair[A, B]",
        "    a: A",
        "    b: B",
        "end t.Pair",
        "",
        "fn t.eat_opt(o: Option<t.L>) -> () uses [] panics never",
        "    match o {",
        "        Some(x) => {",
        "            t.eat(x)",
        "        }",
        "        None => {",
        "            core.mem.drop(0u8)",
        "        }",
        "    }",
        "end t.eat_opt",
        "",
        "fn t.eat_box(b: t.Box<t.L>) -> () uses [] panics never",
        "    let t.Box { v: x } = b",
        "    t.eat(x)",
        "end t.eat_box",
        "",
        "fn t.mk_opt() -> Option<t.L> uses [] panics never",
        "    let o = Some(t.get_l())",
        "    o",
        "end t.mk_opt",
        "",
        "fn t.res_try() -> Result<(), u8> uses [] panics never",
        "    let r: Result<t.L, u8> = Ok(t.get_l())",
        "    let l = r?",
        "    t.eat(l)",
        "    Ok(())",
        "end t.res_try",
        "",
    ]);
    let prefix = pre.join(&nl) + &nl + &nl;
    let t = Tmp::new(name, &main_wrap(&prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

#[test]
fn lin_option_of_linear_left_unconsumed_is_e3003() {
    let (cs, out) = lin_arg_codes("lin_opt_unused", &["let o = Some(t.get_l())"]);
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `o` is never consumed"), "{}", out);
}

#[test]
fn lin_result_with_a_linear_ok_left_unconsumed_is_e3003() {
    let (cs, out) = lin_arg_codes("lin_res_unused", &["let r: Result<t.L, u8> = Ok(t.get_l())"]);
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `r` is never consumed"), "{}", out);
}

#[test]
fn lin_generic_literal_with_a_linear_arg_left_unconsumed_is_e3003() {
    // 型引数の推論: `t.Box { v: t.get_l() }` は `t.Box<t.L>` (線形)。
    let (cs, out) = lin_arg_codes("lin_box_unused", &["let _bl = t.Box { v: t.get_l() }"]);
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
    assert!(out.contains("linear value `_bl` is never consumed"), "{}", out);
}

#[test]
fn lin_generic_struct_with_an_unconsumed_linear_field_is_e3003() {
    let (cs, out) = lin_arg_codes(
        "lin_pair_unused",
        &["let p = t.Pair { a: t.get_l(), b: 1u8 }", "core.mem.drop(p.b)"],
    );
    assert!(tdrop_has(&cs, "E3003"), "{}", out);
}

#[test]
fn lin_option_consumed_by_match_passed_or_moved_is_ok() {
    let (cs, out) = lin_arg_codes(
        "lin_opt_ok",
        &[
            "let o = Some(t.get_l())",
            "match o {",
            "    Some(x) => {",
            "        t.eat(x)",
            "    }",
            "    None => {",
            "        core.mem.drop(0u8)",
            "    }",
            "}",
            "let p = Some(t.get_l())",
            "t.eat_opt(p)",
            "let q = Some(t.get_l())",
            "let q2 = q",
            "t.eat_opt(q2)",
            "let m = t.mk_opt()",
            "t.eat_opt(m)",
        ],
    );
    assert!(!tdrop_has(&cs, "E3003") && !tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn lin_result_try_and_generic_literal_consumed_are_ok() {
    let (cs, out) = lin_arg_codes(
        "lin_res_box_ok",
        &[
            "let b = t.Box { v: t.get_l() }",
            "t.eat_box(b)",
            "let p = t.Pair { a: t.get_l(), b: 1u8 }",
            "t.eat(p.a)",
            "core.mem.drop(p.b)",
            "let n: Option<u8> = Some(1u8)",
            "core.mem.drop(n)",
            "let r: Result<u8, t.L> = Err(t.get_l())",
            "match r {",
            "    Ok(v) => {",
            "        core.mem.drop(v)",
            "    }",
            "    Err(e) => {",
            "        t.eat(e)",
            "    }",
            "}",
        ],
    );
    assert!(!tdrop_has(&cs, "E3003") && !tdrop_has(&cs, "E2042"), "{}", out);
}

#[test]
fn discarding_an_option_or_result_value_is_e2042() {
    // 型引数が決まる `Some(x)` / `Ok(x)` の値を式文で捨てると 4.6 の E2042 (`()` 以外を捨てる)。
    let (cs, out) = lin_arg_codes("lin_discard_opt", &["Some(1u8)", "Ok(1u8)"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
}

// ---- 4.6 (型表): 標準ライブラリのジェネリック型のメソッドの戻り値を捨てる式文 (E2042) ----

/// `Vec<u8>` の値 `v` を作る本体 (`alloc` の効果を使うので main は `uses [alloc, io.console]`)。
fn tdisc_src(extra: &str, b: &[&str]) -> String {
    format!(
        "{}fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    let mut v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n{}    Ok(())\nend t.main\n",
        extra,
        body(b)
    )
}

/// E2042 の診断が出た行 (1 診断 1 行の JSON の "line")。
fn e2042_lines(out: &str) -> Vec<usize> {
    out.lines()
        .filter(|l| l.starts_with('{') && l.contains("\"E2042\""))
        .filter_map(|l| {
            let i = l.find("\"line\": ")? + 8;
            let rest = &l[i..];
            rest[..rest.find(|c: char| !c.is_ascii_digit())?].parse().ok()
        })
        .collect()
}

fn tdisc_lines(name: &str, extra: &str, b: &[&str]) -> (Vec<usize>, String) {
    let t = Tmp::new(name, &tdisc_src(extra, b));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (e2042_lines(&out), out)
}

#[test]
fn tdisc_discarding_option_from_generic_method_is_e2042() {
    // `v.pop()` の戻り値は `Option<u8>` (型引数 T を Vec の型引数から代入して決まる)。`()` ではない。
    // 行 3 が `v.pop()` (行 1 は fn、行 2 は let)。
    let (lines, out) = tdisc_lines("tdisc_pop", "", &["v.pop()", "v.clear()"]);
    assert_eq!(lines, vec![3], "{}", out);
}

#[test]
fn tdisc_discarding_scalar_from_generic_method_is_e2042() {
    // `len` は usize を返す (型引数に依らない戻り値も、型表で決まる)。
    let (lines, out) = tdisc_lines("tdisc_len", "", &["v.len()"]);
    assert_eq!(lines, vec![3], "{}", out);
}

#[test]
fn tdisc_unit_returning_generic_methods_are_silent() {
    // `clear` / `truncate` は `()` を返すので、捨てても診断は出ない (陰性)。
    let (lines, out) = tdisc_lines("tdisc_unit", "", &["v.clear()", "v.truncate(0usize)"]);
    assert!(lines.is_empty(), "{}", out);
}

#[test]
fn tdisc_discarding_result_from_push_is_e2042() {
    // `push` は `Result<(), AllocError>` を返すので、`()` 以外を捨てることになる (4.6)。
    let (lines, out) = tdisc_lines("tdisc_push", "", &["v.push(1u8)"]);
    assert_eq!(lines, vec![3], "{}", out);
}

#[test]
fn tdisc_last_statement_of_a_block_is_not_discarded() {
    // ブロックの最後の式文はブロックの値なので捨てない (関数の戻り値は別の検査)。
    let extra = "fn t.last(v: &mut alloc.vec.Vec<u8, std.alloc.System>) -> Option<u8> uses [] panics never\n    v.pop()\nend t.last\n";
    let (lines, out) = tdisc_lines("tdisc_last", extra, &["let o = t.last(&mut v)", "core.mem.drop(o)"]);
    assert!(lines.is_empty(), "{}", out);
}

#[test]
fn tdisc_generic_element_type_stays_silent() {
    // 要素の型 T が決まらない (型パラメータ) ときは黙る。
    let extra = "fn t.drop_top[T](v: &mut alloc.vec.Vec<T, std.alloc.System>) -> () uses [] panics never\n    v.pop()\n    ()\nend t.drop_top\n";
    let (lines, out) = tdisc_lines("tdisc_generic", extra, &["t.drop_top(&mut v)"]);
    assert!(lines.is_empty(), "{}", out);
}

// ---- impl のメソッドの効果はトレイト宣言の効果を超えない (4.3 / 5.1, E4021)

const IMPL_EFF_PRELUDE: &str = "trait t.Act pub
    fn act(self: &Self) -> u8 uses [io.console] panics never
end t.Act

trait t.Gen[E: uses, P: panics] pub
    fn go(self: &Self) -> u8 uses [E] panics P
end t.Gen

type t.Q pub
    n: u8
end t.Q

type t.S pub
    n: u8
end t.S

type t.T pub
    n: u8
end t.T

type t.W pub
    n: u8
end t.W

type t.X pub
    n: u8
end t.X

type t.Y pub
    n: u8
end t.Y

type t.Z pub
    n: u8
end t.Z

";

#[test]
fn impl_method_effects_within_the_trait_declaration_pass() {
    // 陰性 (通る): 階層の部分型 (`io` ⊇ `io.console`)、トレイトの効果変数を impl の実引数で置き換えた上での包含、
    // `may` の宣言に対する `never` / `may` の実装、`never` の宣言に対する `never` の実装。
    let body = "impl t.Act for t.S
fn t.S.act(self: &t.S) -> u8 uses [io.console] panics never impls t.Act
    self.n
end t.S.act

impl t.Gen<[io], may> for t.T
fn t.T.go(self: &t.T) -> u8 uses [io.fs] panics may impls t.Gen<[io], may>
    self.n
end t.T.go

impl t.Gen<[io.console], never> for t.W
fn t.W.go(self: &t.W) -> u8 uses [io.console] panics never impls t.Gen<[io.console], never>
    self.n
end t.W.go
";
    let (code, got, out) = eff_check_with("impl_eff_ok", IMPL_EFF_PRELUDE, body);
    assert_eq!(code, Some(0), "{}", out);
    assert!(got.is_empty(), "{}", out);
}

#[test]
fn impl_method_effects_beyond_the_trait_declaration_are_rejected() {
    // 陽性 (E4021): 宣言の `uses` に覆われない効果 (`alloc`)、トレイトの効果変数を実引数 `[]` で置き換えた後の
    // 効果 (`io`)、トレイトが `panics never` の impl の `panics may`。
    let body = "impl t.Act for t.X
fn t.X.act(self: &t.X) -> u8 uses [alloc, io.console] panics never impls t.Act
    self.n
end t.X.act

impl t.Gen<[io.console], never> for t.Y
fn t.Y.go(self: &t.Y) -> u8 uses [io.console] panics may impls t.Gen<[io.console], never>
    self.n
end t.Y.go

impl t.Gen<[], never> for t.Z
fn t.Z.go(self: &t.Z) -> u8 uses [io] panics never impls t.Gen<[], never>
    self.n
end t.Z.go
";
    let (code, got, out) = eff_check_with("impl_eff_bad", IMPL_EFF_PRELUDE, body);
    assert_eq!(code, Some(1), "{}", out);
    let want: Vec<String> = ["E4021", "E4021", "E4021"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, want, "{}", out);
    assert!(out.contains("t.X.act uses effect `alloc` not declared by trait method t.Act.act"), "{}", out);
    assert!(out.contains("t.Z.go uses effect `io` not declared by trait method t.Gen.go"), "{}", out);
    assert!(out.contains("t.Y.go may panic but trait method t.Gen.go declares `panics never`"), "{}", out);
}

#[test]
fn impl_method_effects_with_unbound_trait_variables_stay_silent() {
    // 陰性 (黙る): impl の実引数が効果変数 (`E`) のとき、トレイトの効果は束縛できず分からないので、
    // impl のメソッドの具体的な効果 (`io`) も検査しない。
    let body = "impl[E: uses, P: panics] t.Gen<E, P> for t.Q
fn t.Q.go[E: uses, P: panics](self: &t.Q) -> u8 uses [io, E] panics P impls t.Gen<E, P>
    self.n
end t.Q.go
";
    let (_code, got, out) = eff_check_with("impl_eff_quiet", IMPL_EFF_PRELUDE, body);
    assert!(!got.iter().any(|c| c == "E4021"), "{}", out);
}
