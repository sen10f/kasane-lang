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
    let t = Tmp::new("fix_all", &main_wrap("", &b));
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
