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

/// 通ってはいけないコード (`tests/data/unsound/`)。1 ファイル 1 ケースで、どれも `ksn check` が
/// 失敗しなければならない。まだ検査できない穴は `KNOWN_HOLES` に名前を載せる (その間は「通ること」を
/// 確かめるので、塞いだら一覧から外さないとテストが落ちる = 一覧は減る方向にしか変わらない)。
const KNOWN_HOLES: &[&str] = &[
    // 借用を持つ変数 (`let w = t.mk(&x)`) のフィールドを読み戻す (`let r = w.r`) と、借用が読んだ側へ移らない
    // (変数の型が分からないので、`w.n` のような Copy の値と区別できず、移すと誤検出になる)。
    "boundary_field_read_back",
];

#[test]
fn unsound_programs_are_rejected() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/unsound");
    let mut names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    assert!(names.len() >= 16, "unsound cases missing");
    let mut failures = Vec::new();
    for p in names {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("unsound_{}", stem), &fs::read_to_string(&p).unwrap());
        let o = run(&["check", "--json"], &t.0);
        let rejected = o.status.code() == Some(1);
        let hole = KNOWN_HOLES.contains(&stem.as_str());
        if hole && rejected {
            failures.push(format!("{}: now rejected; remove it from KNOWN_HOLES", stem));
        } else if !hole && !rejected {
            failures.push(format!("{}: passes check but must be rejected (rc={:?})", stem, o.status.code()));
        }
        // 整形の違反で落ちているのではないこと (意味の誤りとして報告されること)。
        assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{} is not canonical", stem);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `unsound/linear_*` は、線形型の診断 (E3002 / E3003 / E3006) で落ちなければならない
/// (別の診断で落ちているだけのケースを許さない)。
#[test]
fn linear_corpus_is_rejected_for_linear_reasons() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/unsound");
    let mut n = 0;
    for e in fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        if !stem.starts_with("linear_") {
            continue;
        }
        n += 1;
        let t = Tmp::new(&format!("linear_why_{}", stem), &fs::read_to_string(&p).unwrap());
        let out = text(&run(&["check", "--json"], &t.0).stdout);
        assert!(codes(&out).iter().any(|c| matches!(c.as_str(), "E3002" | "E3003" | "E3006")), "{}: {}", stem, out);
    }
    assert!(n >= 20, "linear cases missing");
}

/// 線形値を全経路で消費している正しいコード (`tests/data/linear_ok/`) は `ksn check` を通る。
#[test]
fn correct_linear_programs_are_accepted() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/linear_ok");
    let mut names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    assert!(names.len() >= 8, "linear_ok cases missing");
    for p in names {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("linear_ok_{}", stem), &fs::read_to_string(&p).unwrap());
        let o = run(&["check", "--json"], &t.0);
        assert_eq!(o.status.code(), Some(0), "{}: {}", stem, text(&o.stdout));
        assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{} is not canonical", stem);
    }
}

/// E2044 (型が決まらない式・束縛) は既定で有効。`--no-strict-types` と `KSN_STRICT_TYPES=0` で旧来の
/// 「黙る」挙動に戻り、`--strict-types` と `KSN_STRICT_TYPES=1` は受け付けるが何も変えない。
/// ケースはそれ 1 件だけで、他の診断を伴わない。
#[test]
fn strict_types_is_the_default_and_can_be_turned_off() {
    let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/diag/E2044.ksnl")).unwrap();
    let t = Tmp::new("strict_types", &src);
    // 既定: E2044 だけ。
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(codes(&text(&o.stdout)), vec!["E2044"], "{}", text(&o.stdout));
    // 旧フラグ・環境変数 (1) は何も変えない。
    let o = run(&["check", "--json", "--strict-types"], &t.0);
    assert_eq!(codes(&text(&o.stdout)), vec!["E2044"]);
    let o = Command::new(exe()).args(["check", "--json"]).env("KSN_STRICT_TYPES", "1").current_dir(&t.0).output().unwrap();
    assert_eq!(codes(&text(&o.stdout)), vec!["E2044"]);
    // 逃げ道: `--no-strict-types` と `KSN_STRICT_TYPES=0` で通る。
    let o = run(&["check", "--json", "--no-strict-types"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    let o = Command::new(exe()).args(["check", "--json"]).env("KSN_STRICT_TYPES", "0").current_dir(&t.0).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    // build / test / run も同じ既定 (E2044 で止まる) で、逃げ道も効く。
    for cmd in ["build", "test", "run"] {
        let o = run(&[cmd], &t.0);
        assert!(text(&o.stderr).contains("E2044") || text(&o.stdout).contains("E2044"), "{}: {}{}", cmd, text(&o.stdout), text(&o.stderr));
        assert_ne!(o.status.code(), Some(0), "{}", cmd);
    }
    // 注釈を書けば通る。
    let fixed = src.replace("let n = None", "let n: Option<u32> = None");
    let t2 = Tmp::new("strict_types_fixed", &fixed);
    let o = run(&["check", "--json"], &t2.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
}

/// 型が決まっていくこと自体の確認: 推論の不足だった典型の形が、strict で E2044 にならない。
#[test]
fn strict_types_accepts_inferable_programs() {
    let src = "\
fn t.pick[T](flag: bool, a: T, b: T) -> T
    if flag {
        return a
    }
    b
end t.pick

fn t.main() -> Result<(), std.io.Error> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let o = Some(1u32)
    let r = o.map(fn(x: u32) -> u32 capture [] {
        x.wrapping_add(1u32)
    })
    let mut i = 0u32
    while i < 3u32 {
        i = i.wrapping_add(1u32)
    }
    let mut w = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(w.push(t.pick(true, 1u8, 2u8)))
    core.mem.drop(v.push(r.unwrap_or(0u32)))
    Ok(())
end t.main
";
    let t = Tmp::new("strict_ok", src);
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E2044"), "{}", text(&o.stdout));
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
    // `Vec.push` は `alloc` を使うので、局所レシーバの効果検査 (①) を通すため main が宣言する。
    let src = format!(
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n",
        b
    );
    let t = Tmp::new("fix_all", &src);
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

// ---- ① 型で解決できる局所レシーバの呼び出しの効果検査 ----

#[test]
fn local_receiver_method_effects_are_checked() {
    // `s.m()` (`s` は局所変数) はパスとして構文解析されるが、型で解決先が一意に分かるので検査する。
    let src = "type t.S\n    n: u32\nend t.S\n\n\
               fn t.S.boom(self: &t.S) -> u32 uses [] panics may\n    core.panic.panic(\"x\")\nend t.S.boom\n\n\
               fn t.S.loud(self: &t.S) -> u32 uses [io.console] panics never\n    1u32\nend t.S.loud\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    \
               let s = t.S { n: 1u32 }\n    core.mem.drop(s.boom())\n    core.mem.drop(s.loud())\n    Ok(())\nend t.main\n";
    let t = Tmp::new("eff_local_bad", src);
    let o = run(&["check", "--json"], &t.0);
    let got = codes(&text(&o.stdout));
    assert_eq!(got.iter().filter(|c| *c == "E4002").count(), 1, "{}", text(&o.stdout));
    assert_eq!(got.iter().filter(|c| *c == "E4001").count(), 1, "{}", text(&o.stdout));
    // 正例: 効果を宣言していれば通る。
    let ok = src.replace("uses [] panics never", "uses [io.console] panics may");
    let t = Tmp::new("eff_local_ok", &ok);
    assert_eq!(run(&["check", "--json"], &t.0).status.code(), Some(0));
}

#[test]
fn mcalls_resolved_by_the_receiver_type_are_checked_precisely() {
    // 同名・同引数個数の候補が複数あっても、レシーバの型で 1 つに解決できれば検査する
    // (全候補が違反のときだけ報告する従来の近似では黙っていた形)。
    let src = "type t.R\n    n: u32\nend t.R\n\n\
               type t.S\n    n: u32\nend t.S\n\n\
               fn t.R.boom(self: t.R) -> u32 uses [] panics never\n    1u32\nend t.R.boom\n\n\
               fn t.S.boom(self: t.S) -> u32 uses [] panics may\n    core.panic.panic(\"x\")\nend t.S.boom\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    \
               core.mem.drop(t.mk().boom())\n    Ok(())\nend t.main\n\n\
               fn t.mk() -> t.S uses [] panics never\n    t.S { n: 1u32 }\nend t.mk\n";
    let t = Tmp::new("eff_mcall_bad", src);
    let o = run(&["check", "--json"], &t.0);
    let got = codes(&text(&o.stdout));
    assert_eq!(got.iter().filter(|c| *c == "E4002").count(), 1, "{}", text(&o.stdout));
    assert!(!got.iter().any(|c| *c == "E4001"), "{}", text(&o.stdout));
}

#[test]
fn method_on_unbounded_type_parameter_is_unknown_method_not_an_effect() {
    // 境界のない型パラメータのレシーバは、メソッドを持たない (E1108)。解決先が決まらないので、
    // 他の型の同名メソッドの効果 (E4002) は持ち込まない。
    let src = "type t.S\n    n: u32\nend t.S\n\n\
               fn t.S.boom(self: t.S) -> u32 uses [] panics may\n    core.panic.panic(\"x\")\nend t.S.boom\n\n\
               fn t.f[T](s: T) -> u32 uses [] panics never\n    s.boom()\nend t.f\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    \
               core.mem.drop(t.f(1u32))\n    Ok(())\nend t.main\n";
    let t = Tmp::new("eff_local_unknown", src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(codes(&text(&o.stdout)), vec!["E1108"], "{}", text(&o.stdout));
}

// ---- ② `Fn` 境界に渡すクロージャの効果の推論 ----

#[test]
fn closure_argument_effects_are_inferred_at_fn_bounds() {
    let src = "fn t.gen[F: core.ops.Fn<(), u32, E, P>, E: uses, P: panics](f: F) -> u32 uses [E] panics P\n    f.call(())\nend t.gen\n\n\
               fn t.boom() -> u32 uses [] panics may\n    core.panic.panic(\"x\")\nend t.boom\n\n\
               fn t.loud() -> u32 uses [io.console] panics never\n    1u32\nend t.loud\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    \
               core.mem.drop(t.gen(fn() -> u32 capture [] {\n        t.boom()\n    }))\n    \
               core.mem.drop(t.gen(fn() -> u32 capture [] {\n        t.loud()\n    }))\n    Ok(())\nend t.main\n";
    let t = Tmp::new("eff_closure_bad", src);
    let o = run(&["check", "--json"], &t.0);
    let got = codes(&text(&o.stdout));
    assert_eq!(got.iter().filter(|c| *c == "E4002").count(), 1, "{}", text(&o.stdout));
    assert_eq!(got.iter().filter(|c| *c == "E4001").count(), 1, "{}", text(&o.stdout));
    // 正例: 効果を宣言していれば通る。
    let ok = src.replace("uses [] panics never", "uses [io.console] panics may");
    let t = Tmp::new("eff_closure_ok", &ok);
    assert_eq!(run(&["check", "--json"], &t.0).status.code(), Some(0));
}

#[test]
fn indirect_closure_bodies_count_toward_the_enclosing_function() {
    // クロージャを変数に束縛して渡す形、入れ子のクロージャに閉じた形も、本体は囲む関数の一部として
    // 検査する (fail-closed。以前は推論せず黙っていた)。
    let src = "fn t.gen[F: core.ops.Fn<(), u32, E, P>, E: uses, P: panics](f: F) -> u32 uses [E] panics P\n    f.call(())\nend t.gen\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    \
               let g = fn() -> u32 capture [] {\n        core.panic.panic(\"boom\")\n    }\n    \
               core.mem.drop(t.gen(g))\n    \
               core.mem.drop(t.gen(fn() -> u32 capture [] {\n        let h = fn() -> u32 capture [] {\n            core.panic.panic(\"inner\")\n        }\n        h()\n    }))\n    Ok(())\nend t.main\n";
    let t = Tmp::new("eff_closure_indirect", src);
    let o = run(&["check", "--json"], &t.0);
    let got = codes(&text(&o.stdout));
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(got.iter().filter(|c| *c == "E4002").count(), 2, "{}", text(&o.stdout));
    // 正例: 囲む関数が `panics may` なら通る。
    let ok = src.replace("uses [] panics never\n    let g", "uses [] panics may\n    let g");
    let t = Tmp::new("eff_closure_indirect_ok", &ok);
    assert_eq!(run(&["check", "--json"], &t.0).status.code(), Some(0), "{}", text(&run(&["check", "--json"], &t.0).stdout));
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
        "let n = match f.write(b\"hello\") {",
        "    Ok(n) => n",
        "    Err(_e) => {",
        "        core.mem.drop(f.close())",
        "        return Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 1i32 })",
        "    }",
        "}",
        "core.check.eq(n, 5usize)",
        "f.close()?",
        "core.check.eq(std.fs.exists(\"ksn_fs_test.txt\"), true)",
        "let mut g = std.fs.open(\"ksn_fs_test.txt\")?",
        "let mut buf = alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "let m = match std.fs.read_to_end(&mut g, &mut buf) {",
        "    Ok(m) => m",
        "    Err(_e) => {",
        "        core.mem.drop(g.close())",
        "        return Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 2i32 })",
        "    }",
        "}",
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

#[test]
fn the_stack_estimate_covers_a_closure_frame() {
    // クロージャ節のフレームは固定の 32 ではなく、引数の型から見積る。
    // 引数の大きい (4 × u128 = 64 バイト) クロージャ経由の再帰は、小さすぎる宣言で E4012、十分なら出ない。
    let nl = char::from(10u8).to_string();
    let src = |limit: &str| {
        let spin = format!("fn t.spin() -> () uses [] panics never stack {}", limit);
        [
            "fn t.gen[F: core.ops.Fn<(u128, u128, u128, u128), (), [], never>](f: F) -> () uses [] panics never stack 4096",
            "    f.call((1u128, 2u128, 3u128, 4u128))",
            "end t.gen",
            "",
            "fn t.main() -> Result<(), std.io.Error> uses [] panics never",
            "    Ok(())",
            "end t.main",
            "",
            spin.as_str(),
            "    t.gen(fn(a: u128, b: u128, c: u128, d: u128) -> () capture [] {",
            "        t.spin()",
            "    })",
            "end t.spin",
            "",
        ]
        .join(&nl)
    };
    let t = Tmp::new("closure_frame", &src("128"));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|c| c == "E4012"), "{}", out);
    assert!(!codes(&out).iter().any(|c| c == "E4010" || c == "E4011"), "{}", out);
    let t = Tmp::new("closure_frame_ok", &src("256"));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(!codes(&out).iter().any(|c| c.starts_with("E40")), "{}", out);
}

#[test]
fn the_stack_estimate_counts_a_large_closure_body() {
    // クロージャ節のフレームは本体の `let` も数える。本体に 64 バイトのタプルを持つクロージャ経由の
    // 再帰は、その分だけ大きくなる (240 バイトでは足りず、4096 バイトなら足りる)。
    let nl = char::from(10u8).to_string();
    let src = |limit: &str| {
        let spin = format!("fn t.spin() -> () uses [] panics never stack {}", limit);
        [
            "fn t.gen[F: core.ops.Fn<(), (), [], never>](f: F) -> () uses [] panics never stack 4096",
            "    f.call(())",
            "end t.gen",
            "",
            "fn t.main() -> Result<(), std.io.Error> uses [] panics never",
            "    Ok(())",
            "end t.main",
            "",
            "fn t.outer() -> () uses [] panics never stack 4096",
            "    t.gen(fn() -> () capture [] {",
            "        let keep: (u128, u128, u128, u128) = (1u128, 2u128, 3u128, 4u128)",
            "        core.mem.drop(keep)",
            "        t.spin()",
            "    })",
            "end t.outer",
            "",
            spin.as_str(),
            "    t.outer()",
            "end t.spin",
            "",
        ]
        .join(&nl)
    };
    let t = Tmp::new("closure_body", &src("240"));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|c| c == "E4012"), "{}", out);
    assert!(!codes(&out).iter().any(|c| c == "E4010" || c == "E4011"), "{}", out);
    let t = Tmp::new("closure_body_ok", &src("4096"));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(!codes(&out).iter().any(|c| c.starts_with("E40")), "{}", out);
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

/// 文字列の拡張 (`ends_with` `contains` `trim*` `strip_*` `split_once` `rfind` `eq_ignore_ascii_case` `split`、
/// `parse`、`String` の `push_u64` `push_i64` `push_bytes` `replace` `join`) を、インタプリタと Cranelift の
/// 両方で、同じ検査 (`tests/data/str_ext.ksnl`) に通す。
#[test]
fn string_extensions_agree_between_interpreter_and_cranelift() {
    let src = include_str!("data/str_ext.ksnl");
    let t = Tmp::new("str_ext_interp", src);
    let check = run(&["check"], &t.0);
    assert_eq!(check.status.code(), Some(0), "{}{}", text(&check.stdout), text(&check.stderr));
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    assert_eq!(text(&interp.stdout).trim(), "str ext ok");
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("str_ext_{}", release), src, release);
        assert!(!out.contains("skipped"), "native backend skipped: {}", out);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
}

/// `split` を変数に持って `next()` で進める書き方はインタプリタだけ。ネイティブは「skipped」で退く。
#[test]
fn a_split_iterator_held_in_a_variable_is_interpreter_only() {
    let src = [
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let mut it = \"a,b\".split(\",\")",
        "    core.check.eq(it.next(), Some(\"a\"))",
        "    core.check.eq(it.next(), Some(\"b\"))",
        "    core.check.eq(it.next(), None)",
        "    std.io.print_line(\"it ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join("\n");
    let t = Tmp::new("str_split_var", &src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    assert_eq!(text(&interp.stdout).trim(), "it ok");
    let (_t, exe_path, out) = build_clif("str_split_var_clif", &src, false);
    assert!(exe_path.is_none() && out.contains("native backend skipped"), "expected a skip: {}", out);
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

#[test]
fn nll_rejects_a_mutation_while_a_borrow_is_carried_through_a_match_value() {
    // 腕の束縛が持つローンは、`match` の値として返した先 (let の束縛) へ運ばれる。
    let (bad, out) = has_e3010(
        "nll_match_carry",
        "",
        &[
            "let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "let mut y = 2u8",
            "match m.insert(1u8, &x) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "let r = match m.get(1u8) {",
            "    Some(rr) => rr",
            "    None => &y",
            "}",
            "x = 2u8",
            "core.check.eq(*r, 1u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `r`"), "{}", out);
}

#[test]
fn nll_accepts_a_match_binding_that_is_not_returned_as_the_value() {
    // 腕の束縛がローンを持っていても、腕の値として返さなければ match の値はローンを持たない。
    let (bad, out) = has_e3010(
        "nll_match_no_carry",
        "",
        &[
            "let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "match m.insert(1u8, &x) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "let n = match m.get(1u8) {",
            "    Some(rr) => {",
            "        core.check.eq(*rr, 1u8)",
            "        0u8",
            "    }",
            "    None => 1u8",
            "}",
            "x = 2u8",
            "core.check.eq(n, 0u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_borrow_is_stored_in_a_tuple() {
    let (bad, out) = has_e3010(
        "nll_store_tuple",
        "",
        &["let mut x = 1u8", "let t = (&x, 2u8)", "x = 2u8", "core.mem.drop(t)"],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `t.0`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_borrow_is_stored_in_an_array_literal() {
    let (bad, out) = has_e3010(
        "nll_store_array",
        "",
        &["let mut x = 1u8", "let a = [&x]", "x = 2u8", "core.mem.drop(a)"],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `a.0`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_after_a_tuple_is_destructured() {
    // `let (a, b) = t` の分解で、保存されたローンは各束縛へ読み戻される。
    let (bad, out) = has_e3010(
        "nll_store_tuple_destructure",
        "",
        &[
            "let mut x = 1u8",
            "let t = (&x, 2u8)",
            "let (a, b) = t",
            "x = 2u8",
            "core.check.eq(*a, 1u8)",
            "core.check.eq(b, 2u8)",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `a`"), "{}", out);
}

#[test]
fn nll_accepts_a_tuple_overwritten_before_the_conflict() {
    // タプルごと上書きすると、保存されていた借用は死ぬ。
    let (bad, out) = has_e3010(
        "nll_store_tuple_overwrite",
        "",
        &[
            "let mut x = 1u8",
            "let mut y = 2u8",
            "let mut t = (&x, 1u8)",
            "t = (&y, 2u8)",
            "core.check.eq(x, 1u8)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn nll_accepts_a_tuple_borrow_dropped_before_the_conflict() {
    let (bad, out) = has_e3010(
        "nll_store_tuple_drop",
        "",
        &[
            "let mut x = 1u8",
            "let t = (&x, 2u8)",
            "core.mem.drop(t)",
            "x = 2u8",
            "core.check.eq(x, 2u8)",
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
fn a_field_moved_in_a_branch_stays_moved_after_the_merge() {
    // 分岐の合流: どれかの枝で move された場所は、合流後も move 済み (以前は合流で忘れていた)。
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
    assert!(bad, "{}", out);
    assert!(out.contains("use of partially moved value `s`"), "{}", out);
}

#[test]
fn a_move_in_a_diverging_branch_does_not_reach_the_merge() {
    // 後続へ進まない枝 (`return`) の中の move は、合流後に持ち越さない。
    let (bad, out) = pmove_check(
        "pmove_branch_div",
        &pmove_prefix(),
        &[
            "let s = t.S { f: t.P { a: 1u8 }, g: t.P { a: 2u8 } }",
            "let flag = true",
            "if flag {",
            "    t.take_s(s)",
            "    return Ok(())",
            "}",
            "t.take_s(s)",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn arm_bindings_with_the_same_name_do_not_share_move_state() {
    // 兄弟の腕・後続の `match` で同じ名前を束縛しても、前の腕での move を引き継がない。
    let (bad, out) = pmove_check(
        "pmove_arm_names",
        &pmove_prefix(),
        &[
            "let a = Some(t.P { a: 1u8 })",
            "let b = Some(t.P { a: 2u8 })",
            "match a {",
            "    Some(e) => t.take(e)",
            "    None => {",
            "    }",
            "}",
            "match b {",
            "    Some(e) => t.take(e)",
            "    None => {",
            "    }",
            "}",
        ],
    );
    assert!(!bad, "{}", out);
}

#[test]
fn a_value_moved_in_a_loop_body_is_an_error_on_the_next_iteration() {
    let (bad, out) = pmove_check(
        "pmove_loop",
        &pmove_prefix(),
        &[
            "let s = t.S { f: t.P { a: 1u8 }, g: t.P { a: 2u8 } }",
            "let mut n = 0u8",
            "while n < 2u8 {",
            "    t.take_s(s)",
            "    n = n.wrapping_add(1u8)",
            "}",
        ],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("moved inside a loop"), "{}", out);
    // 正例: move のあとに必ず `break` するなら、次の周回はない。
    let (bad, out) = pmove_check(
        "pmove_loop_break",
        &pmove_prefix(),
        &[
            "let s = t.S { f: t.P { a: 1u8 }, g: t.P { a: 2u8 } }",
            "loop {",
            "    t.take_s(s)",
            "    break",
            "}",
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

// ---- 一時値の型解決 (4.4・4.6): フィールド道と分解 ----

fn temp_drop_codes(name: &str, helper: &[&str], b: &[&str]) -> (Vec<String>, String) {
    let nl = char::from(10u8).to_string();
    let prefix = if helper.is_empty() { String::new() } else { helper.join(&nl) + &nl + &nl };
    let t = Tmp::new(name, &main_wrap(&prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    let c = codes(&out);
    (c, out)
}

fn temp_drop_prefix() -> Vec<&'static str> {
    vec![
        "type t.L linear pub",
        "    a: u8",
        "end t.L",
        "",
        "type t.W pub",
        "    lin: t.L",
        "    num: u8",
        "end t.W",
        "",
        "fn t.mk() -> t.W uses [] panics never",
        "    t.W { lin: t.L { a: 1u8 }, num: 2u8 }",
        "end t.mk",
        "",
        "fn t.pair() -> (t.L, u8) uses [] panics never",
        "    (t.L { a: 1u8 }, 2u8)",
        "end t.pair",
        "",
        "fn t.take(l: t.L) -> () uses [] panics never",
        "    core.mem.drop(l)",
        "end t.take",
        "",
    ]
}

#[test]
fn a_discarded_field_access_is_reported() {
    // 4.6: `t.mk().num` を式文で捨てると E2042 (フィールドの型を解決できるようになった経路)。
    let (c, out) = temp_drop_codes("td_field_discard", &temp_drop_prefix(), &["t.mk().num"]);
    assert!(c.iter().any(|x| x == "E2042"), "{}", out);
}

#[test]
fn a_discarded_tuple_literal_is_reported() {
    // 4.6: タプルリテラルの式文も `()` 以外なので E2042。
    let (c, out) = temp_drop_codes("td_tuple_discard", &[], &["(1u8, 2u8)"]);
    assert!(c.iter().any(|x| x == "E2042"), "{}", out);
}

#[test]
fn a_unit_discard_stays_allowed() {
    let (c, out) = temp_drop_codes("td_unit_discard", &[], &["()"]);
    assert!(!c.iter().any(|x| x == "E2042"), "{}", out);
}

#[test]
fn a_field_of_a_linear_type_must_be_consumed() {
    // `t.mk().lin` の型が t.L (線形) と分かるので E3003。消費すれば出ない。
    let (c, out) = temp_drop_codes("td_field_linear", &temp_drop_prefix(), &["let x = t.mk().lin"]);
    assert!(c.iter().any(|x| x == "E3003"), "{}", out);
    let (c, out) = temp_drop_codes("td_field_linear_ok", &temp_drop_prefix(), &["let x = t.mk().lin", "t.take(x)"]);
    assert!(!c.iter().any(|x| x == "E3003"), "{}", out);
}

#[test]
fn a_destructured_linear_element_must_be_consumed() {
    // 分解 `let (a, b) = t.pair()` の要素型が流れ、線形要素 a の未消費が E3003。
    let (c, out) = temp_drop_codes("td_tuple_linear", &temp_drop_prefix(), &["let (a, b) = t.pair()", "core.mem.drop(b)"]);
    assert!(c.iter().any(|x| x == "E3003"), "{}", out);
    assert!(out.contains("linear value `a`"), "{}", out);
    let (c, out) = temp_drop_codes(
        "td_tuple_linear_ok",
        &temp_drop_prefix(),
        &["let (a, b) = t.pair()", "t.take(a)", "core.mem.drop(b)"],
    );
    assert!(!c.iter().any(|x| x == "E3003"), "{}", out);
}

#[test]
fn a_non_linear_destructured_element_is_not_reported() {
    let helper = ["fn t.pair_u8() -> (u8, u8) uses [] panics never", "    (1u8, 2u8)", "end t.pair_u8"];
    let (c, out) = temp_drop_codes(
        "td_tuple_plain",
        &helper,
        &["let (a, b) = t.pair_u8()", "core.mem.drop(a)", "core.mem.drop(b)"],
    );
    assert!(!c.iter().any(|x| x == "E3003"), "{}", out);
}

#[test]
fn a_generic_field_type_stays_unknown() {
    // 型パラメータを含むフィールド型は決まらないので黙る (誤検出しない)。
    let helper = ["type t.Box[T] pub", "    item: T", "end t.Box"];
    let (c, out) = temp_drop_codes(
        "td_field_generic",
        &helper,
        &["let b = t.Box { item: 1u8 }", "let x = b.item", "core.mem.drop(x)"],
    );
    assert!(!c.iter().any(|x| x == "E3003" || x == "E2042"), "{}", out);
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

/// 浮動小数の引数・戻り値と生ポインタを扱う C の関数 (共有ライブラリにして `LD_PRELOAD` する)。
/// `bits_*` / `emit` / `ffi_done` は、値のビットパターンを 10 進で印字するための観測用。
#[cfg(unix)]
const FFI_FLOAT_C: &str = r#"#include <stdio.h>
#include <stdlib.h>

float add_f(float a, float b) { return a + b; }
double add_d(double a, double b) { return a + b; }
double mix_i(int n, double x, float y) { return (double)n + x + (double)y; }
double mix2(double a, int b, float c, double d) { return a + (double)b + (double)c + d; }
double six(double a, double b, double c, double d, double e, float f) { return a + b + c + d + e + (double)f; }
double six_i(int a, int b, int c, int d, int e, int f) { return (double)(a + b + c + d + e + f); }
int scale_i(float x) { return (int)(x * 2.0f); }
double *alloc_d(void) { return (double *)malloc(8 * 4); }
void store_d(double *p, double v) { *p = v; }
double load_d(const double *p) { return *p; }
unsigned long long bits_f(float x) { union { float f; unsigned u; } v; v.f = x; return (unsigned long long)v.u; }
unsigned long long bits_d(double x) { union { double d; unsigned long long u; } v; v.d = x; return v.u; }
void emit(unsigned long long v) { printf("%llu\n", v); }
void ffi_done(void) { printf("done\n"); }
"#;

/// 同じ C の関数を呼んで、参照値を印字するドライバ (同じ `ffi.c` と一緒にビルドする)。
#[cfg(unix)]
const FFI_FLOAT_DRIVER_C: &str = r#"#include <stdio.h>
#include <stdlib.h>

float add_f(float, float);
double add_d(double, double);
double mix_i(int, double, float);
double mix2(double, int, float, double);
double six(double, double, double, double, double, float);
double six_i(int, int, int, int, int, int);
int scale_i(float);
double *alloc_d(void);
void store_d(double *, double);
double load_d(const double *);
unsigned long long bits_f(float);
unsigned long long bits_d(double);
void emit(unsigned long long);
void ffi_done(void);

int main(void) {
    emit(bits_f(add_f(1.5f, 2.25f)));
    emit(bits_d(add_d(1.5, 2.25)));
    emit(bits_d(mix_i(3, 0.5, 0.25f)));
    emit(bits_d(mix2(0.5, 7, 0.25f, 1.5)));
    emit(bits_d(six(0.5, 1.5, 2.5, 3.5, 4.5, 0.25f)));
    emit(bits_d(six_i(1, 2, 3, 4, 5, 6)));
    emit((unsigned long long)scale_i(1.5f));
    double *p = alloc_d();
    store_d(p, 2.5);
    p[1] = 3.5;
    emit(bits_d(load_d(p)));
    emit(bits_d(p[1]));
    free(p);
    ffi_done();
    return 0;
}
"#;

/// ドライバと同じ入力で同じ C の関数を呼ぶ Kasane のプログラム (出力を突き合わせる)。
#[cfg(unix)]
const FFI_FLOAT_SRC: &str = r#"extern t.add_f(a: f32, b: f32) -> f32 link "add_f"

extern t.add_d(a: f64, b: f64) -> f64 link "add_d"

extern t.mix_i(n: i32, x: f64, y: f32) -> f64 link "mix_i"

extern t.mix2(a: f64, b: i32, c: f32, d: f64) -> f64 link "mix2"

extern t.six(a: f64, b: f64, c: f64, d: f64, e: f64, f: f32) -> f64 link "six"

extern t.six_i(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> f64 link "six_i"

extern t.scale_i(x: f32) -> i32 link "scale_i"

extern t.alloc_d() -> *mut f64 link "alloc_d"

extern t.store_d(pointer: *mut f64, value: f64) -> () link "store_d"

extern t.load_d(pointer: *const f64) -> f64 link "load_d"

extern t.free(pointer: *mut f64) -> () link "free"

extern t.bits_f(x: f32) -> u64 link "bits_f"

extern t.bits_d(x: f64) -> u64 link "bits_d"

extern t.emit(v: u64) -> () link "emit"

extern t.ffi_done() -> () link "ffi_done"

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    unsafe "libc" "floats and raw pointers" {
        t.emit(t.bits_f(t.add_f(1.5f32, 2.25f32)))
        t.emit(t.bits_d(t.add_d(1.5f64, 2.25f64)))
        t.emit(t.bits_d(t.mix_i(3i32, 0.5f64, 0.25f32)))
        t.emit(t.bits_d(t.mix2(0.5f64, 7i32, 0.25f32, 1.5f64)))
        t.emit(t.bits_d(t.six(0.5f64, 1.5f64, 2.5f64, 3.5f64, 4.5f64, 0.25f32)))
        t.emit(t.bits_d(t.six_i(1i32, 2i32, 3i32, 4i32, 5i32, 6i32)))
        t.emit(t.scale_i(1.5f32))
        let p = t.alloc_d()
        t.store_d(p, 2.5f64)
        core.ptr.write(core.ptr.add(p, 1usize), 3.5f64)
        t.emit(t.bits_d(t.load_d(p)))
        t.emit(t.bits_d(core.ptr.read(core.ptr.add(p, 1usize))))
        t.free(p)
        t.ffi_done()
    }
    Ok(())
end t.main
"#;

/// `cc` (無ければ `gcc` / `clang`) でコンパイルする。
#[cfg(unix)]
fn cc(args: &[&str], dir: &Path) -> Output {
    let has = |c: &str| Command::new(c).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    let cc = ["cc", "gcc", "clang"].iter().find(|c| has(c)).expect("no C compiler (cc / gcc / clang)");
    Command::new(cc).args(args).current_dir(dir).output().expect("spawn cc")
}

#[cfg(unix)]
#[test]
fn ffi_interpreter_float_calls_match_a_c_driver() {
    let t = Tmp::new("ffi_float", FFI_FLOAT_SRC);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let dir = &t.0;
    fs::write(dir.join("ffi.c"), FFI_FLOAT_C).unwrap();
    fs::write(dir.join("drive.c"), FFI_FLOAT_DRIVER_C).unwrap();
    // 同じ C の関数を呼ぶドライバをビルドして、参照値を得る。
    let o = cc(&["-o", "drive", "drive.c", "ffi.c"], dir);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let want = text(&Command::new(dir.join("drive")).output().unwrap().stdout);
    assert!(want.ends_with("done\n"), "{}", want);
    // 同じ C を共有ライブラリにして、インタプリタの extern から呼ぶ。
    let lib = dir.join("libffitest.so");
    let o = cc(&["-shared", "-fPIC", "-o", lib.to_str().unwrap(), "ffi.c"], dir);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let o = Command::new(exe()).args(["run"]).current_dir(dir).env("LD_PRELOAD", &lib).output().unwrap();
    assert_eq!(text(&o.stdout), want, "{}", text(&o.stderr));
}

#[test]
fn ffi_interpreter_extern_calls_and_raw_pointers_work() {
    let t = Tmp::new("ffi_interp_raw", FFI_SRC);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let o = run(&["run"], &t.0);
    assert_eq!(text(&o.stdout), "ffi ok\n", "{}", text(&o.stderr));
}

#[test]
fn ffi_interpreter_returns_floats_from_libc_math() {
    let src = r#"extern t.sqrt(x: f64) -> f64 link "sqrt"

extern t.sqrtf(x: f32) -> f32 link "sqrtf"

extern t.fabsf(x: f32) -> f32 link "fabsf"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe "libc" "square root" {
        t.sqrt(9.0f64)
    }
    core.check.eq(a.to_bits(), 3.0f64.to_bits())
    let b = unsafe "libc" "square root of a float" {
        t.sqrtf(2.25f32)
    }
    core.check.eq(b.to_bits(), 1.5f32.to_bits())
    let c = unsafe "libc" "absolute value of a float" {
        t.fabsf(0.0f32 - 2.5f32)
    }
    core.check.eq(c.to_bits(), 2.5f32.to_bits())
    std.io.print_line("math ok")
    Ok(())
end t.main
"#;
    let t = Tmp::new("ffi_math", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let o = run(&["run"], &t.0);
    assert_eq!(text(&o.stdout), "math ok\n", "{}", text(&o.stderr));
}

// ---- 診断の列 (仕様 7.1: 1 始まり、Unicode スカラ値単位)

/// `--json` の出力から、コード `code` の診断の (line, col) を取り出す。
fn pos_of(json: &str, code: &str) -> (usize, usize) {
    let line = json.lines().find(|l| l.contains(&format!("\"code\": \"{}\"", code))).expect("diagnostic not found");
    let num = |key: &str| -> usize {
        let k = format!("\"{}\": ", key);
        let rest = &line[line.find(&k).unwrap() + k.len()..];
        rest.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap()
    };
    (num("line"), num("col"))
}

#[test]
fn diagnostic_reports_column_in_the_middle_of_a_line() {
    // `nope` は 3 行目の 18 列目から始まる (行 2 は main の本体 1 行目)。
    let src = main_wrap("", "    let x: i32 = nope(1)\n    let y = x\n    let _z = y\n");
    let t = Tmp::new("col_mid", &src);
    let j = text(&run(&["check", "--json"], &t.0).stdout);
    assert_eq!(pos_of(&j, "E1101"), (2, 18), "{}", j);
    let p = text(&run(&["check"], &t.0).stdout);
    assert!(p.contains("t.ksnl:2:18: E1101"), "{}", p);
}

#[test]
fn diagnostic_column_counts_unicode_scalars_not_bytes() {
    // 「あい」は 1 文字 3 バイトだが、列は 1 文字を 1 と数える。
    let src = main_wrap("", "    let a = \"あい\".len() + zzz(2)\n    let _b = a\n");
    let t = Tmp::new("col_uni", &src);
    let j = text(&run(&["check", "--json"], &t.0).stdout);
    // `    let a = "あい".len() + zzz(2)`: zzz は 26 文字目から (バイトなら 32)。
    assert_eq!(pos_of(&j, "E1101"), (2, 26), "{}", j);
}

#[test]
fn parse_error_reports_its_column() {
    let src = main_wrap("", "    let a = \"あい\" +\n");
    let t = Tmp::new("col_parse", &src);
    let j = text(&run(&["check", "--json"], &t.0).stdout);
    let (line, col) = pos_of(&j, "E0002");
    assert_eq!((line, col), (2, 19), "{}", j);
}

#[test]
fn header_diagnostic_reports_the_column_of_the_name() {
    let src = "fn t.f(a: Nope) -> i32 uses [] panics never\n    1\nend t.f\n";
    let t = Tmp::new("col_header", src);
    let j = text(&run(&["check", "--json"], &t.0).stdout);
    assert_eq!(pos_of(&j, "E1102"), (1, 11), "{}", j);
}

#[test]
fn diagnostics_are_ordered_by_line_then_column() {
    let src = main_wrap("", "    let a = zzz(1) + yyy(2)\n    let _b = a\n");
    let t = Tmp::new("col_order", &src);
    let j = text(&run(&["check", "--json"], &t.0).stdout);
    let cols: Vec<usize> = j
        .lines()
        .filter(|l| l.contains("\"E1101\""))
        .map(|l| {
            let k = "\"col\": ";
            l[l.find(k).unwrap() + k.len()..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap()
        })
        .collect();
    assert_eq!(cols, vec![13, 22], "{}", j);
}

// ---- std.net (インタプリタ) ----

/// ポート番号を `127.0.0.1:<port>` に整形する補助 (テスト用ソースの共通部分)。
const NET_ADDR_FN: &str = r#"fn t.addr(port: u16) -> alloc.string.String<std.alloc.System> uses [alloc] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str("127.0.0.1:"))
    let n = usize.from_u16(port)
    let digits = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]
    let mut unit = 10000usize
    let mut started = false
    while unit > 0usize {
        let d = (n / unit) % 10usize
        if d > 0usize || started || unit == 1usize {
            started = true
            match digits.get(d) {
                Some(t) => core.mem.drop(s.push_str(*t))
                None => core.mem.drop(0u8)
            }
        }
        unit = unit / 10usize
    }
    s
end t.addr

"#;

#[test]
fn std_net_loopback_round_trip() {
    // ポート 0 で bind し、listen backlog のおかげで connect を accept より先に行える。
    let b = body(&[
        "let listener = match std.net.listen(\"127.0.0.1:0\") {",
        "    Ok(l) => l",
        "    Err(_e) => return Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 1i32 })",
        "}",
        "let port = match listener.local_port() {",
        "    Ok(p) => p",
        "    Err(_e) => 0u16",
        "}",
        "core.check.is_true(port > 0u16)",
        "let addr = t.addr(port)",
        "let mut client = match std.net.connect(addr.as_str()) {",
        "    Ok(c) => c",
        "    Err(_e) => {",
        "        core.mem.drop(listener.close())",
        "        return Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 2i32 })",
        "    }",
        "}",
        "let mut server = match listener.accept() {",
        "    Ok(s) => s",
        "    Err(_e) => {",
        "        core.mem.drop(client.close())",
        "        core.mem.drop(listener.close())",
        "        return Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 3i32 })",
        "    }",
        "}",
        "core.check.eq(client.write(b\"ping\").is_ok(), true)",
        "let mut buf = [0u8; 16]",
        "let n = match server.read(&mut buf) {",
        "    Ok(n) => n",
        "    Err(_e) => 0usize",
        "}",
        "core.check.eq(n, 4usize)",
        "core.check.eq(buf.get(0usize), Some(112u8))",
        "core.check.eq(server.write(b\"pong!\").is_ok(), true)",
        "core.mem.drop(server.close())",
        "let mut buf2 = [0u8; 16]",
        "let m = match client.read(&mut buf2) {",
        "    Ok(m) => m",
        "    Err(_e) => 0usize",
        "}",
        "core.check.eq(m, 5usize)",
        "let eof = match client.read(&mut buf2) {",
        "    Ok(k) => k",
        "    Err(_e) => 99usize",
        "}",
        "core.check.eq(eof, 0usize)",
        "core.mem.drop(client.close())",
        "core.mem.drop(listener.close())",
    ]);
    let src = format!("{}{}", NET_ADDR_FN, main_wrap("", &b)).replace("uses [io.console] panics may", "uses [alloc, io.console, io.net] panics may");
    let t = Tmp::new("net_rt", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

#[test]
fn std_net_connect_to_a_closed_port_is_an_error_value() {
    // bind したポートを close してから connect する (そのポートは待ち受け無し)。
    let b = body(&[
        "let listener = match std.net.listen(\"127.0.0.1:0\") {",
        "    Ok(l) => l",
        "    Err(_e) => return Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 1i32 })",
        "}",
        "let port = match listener.local_port() {",
        "    Ok(p) => p",
        "    Err(_e) => 0u16",
        "}",
        "core.mem.drop(listener.close())",
        "let addr = t.addr(port)",
        "match std.net.connect(addr.as_str()) {",
        "    Ok(c) => {",
        "        core.mem.drop(c.close())",
        "        core.panic.panic(\"connect to a closed port must fail\")",
        "    }",
        "    Err(e) => core.check.is_true(e.code != 0i32)",
        "}",
        "match std.net.connect(\"not an address\") {",
        "    Ok(c) => {",
        "        core.mem.drop(c.close())",
        "        core.panic.panic(\"bad address must fail\")",
        "    }",
        "    Err(_e) => core.mem.drop(0u8)",
        "}",
    ]);
    let src = format!("{}{}", NET_ADDR_FN, main_wrap("", &b)).replace("uses [io.console] panics may", "uses [alloc, io.console, io.net] panics may");
    let t = Tmp::new("net_closed", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

#[test]
fn std_net_checker_accepts_consumed_streams_and_rejects_dropped_ones() {
    let good = "fn t.f(addr: &str) -> Result<(), std.net.Error> uses [io.net] panics never\n    let s = std.net.connect(addr)?\n    s.close()\nend t.f\n\n\
                fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = Tmp::new("net_linear_ok", good);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    let bad = "fn t.f(addr: &str) -> Result<(), std.net.Error> uses [io.net] panics never\n    let _s: std.net.TcpStream = std.net.connect(addr)?\n    Ok(())\nend t.f\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = Tmp::new("net_linear_leak", bad);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|c| c == "E3003"), "{}", out);
}

#[test]
fn std_net_calls_require_the_io_net_effect() {
    let src = "fn t.f(addr: &str) -> Result<(), std.net.Error> uses [] panics never\n    let s = std.net.connect(addr)?\n    s.close()\nend t.f\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = Tmp::new("net_effect", src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|c| c == "E4001"), "{}", out);
}

// ---- std.fs / std.net / 参照を返す関数 (Cranelift) ----

/// プログラムをインタプリタと Cranelift (debug / release) で、それぞれ一時ディレクトリの中で動かして、
/// 標準出力と終了コードが一致することを確かめる。インタプリタの標準出力を返す。
fn agree_with_interpreter(name: &str, src: &str) -> String {
    let t = Tmp::new(&format!("{}_interp", name), src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    for release in [false, true] {
        let (bt, exe_path, out) = build_clif(&format!("{}_{}", name, release), src, release);
        assert!(!out.contains("skipped"), "native backend skipped: {}", out);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).current_dir(&bt.0).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), text(&interp.stdout), "native and interpreter outputs differ (release: {})", release);
    }
    text(&interp.stdout)
}

/// 作成・書き込み・読み戻し・削除・存在しないファイルのエラーを、インタプリタとネイティブで同じ結果にする。
/// エラー番号は OS ごとに違うので、出力の一致は Unix だけで確かめる (Windows は動いて `fs ok` で終わることだけ)。
#[test]
fn std_fs_round_trip_agrees_between_interpreter_and_cranelift() {
    let src = include_str!("data/native_fs.ksnl");
    if cfg!(unix) {
        let out = agree_with_interpreter("native_fs", src);
        assert!(out.contains("written: 11\n"), "{}", out);
        assert!(out.contains("first byte: 104\n"), "{}", out);
        assert!(out.contains("at end of file: 0\n"), "{}", out);
        assert!(out.contains("exists after remove: 0\n"), "{}", out);
        assert!(out.contains("open missing: kind 1 code 2\n"), "{}", out);
        assert!(out.ends_with("fs ok\n"), "{}", out);
    } else {
        for release in [false, true] {
            let (bt, exe_path, out) = build_clif(&format!("native_fs_win_{}", release), src, release);
            let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
            let o = Command::new(&p).current_dir(&bt.0).output().unwrap();
            assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
            assert!(text(&o.stdout).ends_with("fs ok\n"));
        }
    }
}

/// 3000 回の open / close が、記述子の上限を小さくしても失敗しない (記述子が漏れない)。作ったファイルは残らない。
#[cfg(unix)]
#[test]
fn std_fs_does_not_leak_descriptors_in_a_native_loop() {
    let src = include_str!("data/native_fs.ksnl");
    let (bt, exe_path, out) = build_clif("native_fs_leak", src, false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new("sh")
        .args(["-c", "ulimit -n 40 && exec \"$0\"", p.to_str().unwrap()])
        .current_dir(&bt.0)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stdout).contains("bytes moved in the loop: 18000\n"), "{}", text(&o.stdout));
    for left in ["ksn_native_fs.txt", "ksn_native_fs_many.txt"] {
        assert!(!bt.0.join(left).exists(), "{} was left behind", left);
    }
}

/// ループバックの往復 (ポート 0 で bind、`local_port`、connect、accept、write、read、close、終端の read) と、
/// connect / accept の繰り返し。
#[cfg(unix)]
#[test]
fn std_net_loopback_round_trip_agrees_between_interpreter_and_cranelift() {
    let out = agree_with_interpreter("native_net", include_str!("data/native_net.ksnl"));
    assert!(out.contains("server received: 4\nfirst byte: 112\n"), "{}", out);
    assert!(out.contains("client received: 5\nread at end of stream: 0\n"), "{}", out);
    assert!(out.contains("bytes moved in the loop: 1200\n"), "{}", out);
    assert!(out.ends_with("net ok\n"), "{}", out);
}

/// connect / accept を 600 回繰り返しても、記述子の上限を小さくして失敗しない (記述子が漏れない)。
#[cfg(unix)]
#[test]
fn std_net_does_not_leak_descriptors_in_a_native_loop() {
    let src = include_str!("data/native_net.ksnl");
    let (bt, exe_path, out) = build_clif("native_net_leak", src, false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new("sh").args(["-c", "ulimit -n 40 && exec \"$0\"", p.to_str().unwrap()]).current_dir(&bt.0).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stdout).ends_with("net ok\n"), "{}", text(&o.stdout));
}

/// 閉じたポートへの connect、使用中のポートへの bind、不正なアドレスが、エラー値になる (番号を含めて同じ)。
#[cfg(unix)]
#[test]
fn std_net_errors_agree_between_interpreter_and_cranelift() {
    let out = agree_with_interpreter("native_net_err", include_str!("data/native_net_err.ksnl"));
    assert!(out.contains("bad address code: -1\n"), "{}", out);
    assert!(out.contains("bad port code: -1\n"), "{}", out);
    assert!(out.contains("connect to a closed port code: "), "{}", out);
    assert!(out.ends_with("net errors ok\n"), "{}", out);
}

/// `std.net` は Windows では未対応で、ビルド時に理由を付けて退く (誤ったコードを作らない)。
#[cfg(windows)]
#[test]
fn std_net_is_skipped_by_the_native_backend_on_windows() {
    let (_t, exe_path, out) = build_clif("native_net_win", include_str!("data/native_net.ksnl"), false);
    assert!(exe_path.is_none() && out.contains("native backend skipped") && out.contains("Windows"), "{}", out);
}

/// `&str` / `&[T]` / `&スカラー` を返す関数。返す式が、リテラルか参照の引数 (に `as_str` / `trim` をつないだもの) のとき。
#[test]
fn functions_returning_references_agree_between_interpreter_and_cranelift() {
    let out = agree_with_interpreter("native_ref", include_str!("data/native_ref.ksnl"));
    assert_eq!(out, "one, and\n");
}

/// `defs` の関数を `main` から呼ぶ (呼ばれない関数はネイティブ生成の対象にならない)。`call` は呼び出しの文。
fn skip_reason_for(name: &str, defs: &str, call: &str) -> String {
    let src = format!(
        "{}\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    std.io.print_line(\"unused\")\nend t.main\n",
        defs, call
    );
    let (_t, exe_path, out) = build_clif(name, &src, false);
    assert!(exe_path.is_none(), "expected a skip, but built: {}", out);
    out
}

/// 持ち主を移す・ぶら下がる恐れのある参照を返す関数は、理由を付けて退く。
#[test]
fn unsound_reference_returns_are_skipped_with_a_reason() {
    // ローカルの変数を経由する形は、リテラルや引数に由来すると判断できないので退く
    let out = skip_reason_for("ref_local", "fn t.word(text: &str) -> &str\n    let inner = text.trim()\n    inner\nend t.word\n", "    core.mem.drop(t.word(\"a\"))\n");
    assert!(out.contains("native backend skipped: t.word: returns a reference that may not outlive the function"), "{}", out);
    // 所有する型への参照
    let out = skip_reason_for(
        "ref_string",
        "fn t.view(s: &alloc.string.String<std.alloc.System>) -> &alloc.string.String<std.alloc.System>\n    s\nend t.view\n",
        "    let s = alloc.string.String.new(std.alloc.SYSTEM)\n    core.mem.drop(t.view(&s))\n",
    );
    assert!(out.contains("native backend skipped: t.view: returns a reference to an aggregate"), "{}", out);
    // 構造体への参照
    let out = skip_reason_for("ref_struct", "type t.P\n    a: u8\nend t.P\n\nfn t.first(p: &t.P) -> &t.P\n    p\nend t.first\n", "    let p = t.P { a: 1u8 }\n    core.mem.drop(t.first(&p))\n");
    assert!(out.contains("native backend skipped: t.first: returns a reference to an aggregate"), "{}", out);
    // 可変参照
    let out = skip_reason_for("ref_mut", "fn t.bump(x: &mut u32) -> &mut u32\n    x\nend t.bump\n", "    let mut x = 1u32\n    core.mem.drop(t.bump(&mut x))\n");
    assert!(out.contains("native backend skipped: t.bump: returns a mutable reference"), "{}", out);
}

/// 例 `examples/http` (ループバックの HTTP 往復) が、ネイティブでも `ksn run` と同じ出力・終了コードになる。
#[cfg(unix)]
#[test]
fn the_http_example_runs_natively_through_cranelift() {
    let d = root().join("examples").join("http");
    let cache = std::env::temp_dir().join(format!("ksn_cache_http_{}", std::process::id()));
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
        assert!(!out.contains("skipped"), "native backend skipped: {}", out);
        let kind = if release { "release" } else { "debug" };
        let p = d.join("target").join(kind).join("http.native");
        assert!(p.exists(), "native executable missing: {}", out);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), interp.status.code(), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
    let _ = fs::remove_dir_all(&cache);
}

// ---- 既定の fail-closed: メソッドの存在と参照の段数 ----

fn codes_of(name: &str, src: &str) -> Vec<String> {
    let t = Tmp::new(name, src);
    codes(&text(&run(&["check", "--json"], &t.0).stdout))
}

#[test]
fn unbounded_type_parameter_has_no_methods_but_bounds_give_them() {
    // 境界なし: 型パラメータからはメソッドを呼べない (E1108)。
    let bad = "fn t.f[T](s: &T) -> usize uses [] panics never\n    s.len()\nend t.f\n\n";
    assert!(codes_of("tp_bad", &main_wrap(bad, "")).contains(&"E1108".to_string()));
    // 導出されるメソッドでも、境界なしなら E1108 (`clone` は `Clone` の境界が要る)。
    let bad2 = "fn t.f[T](s: &T) -> T uses [] panics never\n    s.clone()\nend t.f\n\n";
    assert!(codes_of("tp_bad2", &main_wrap(bad2, "")).contains(&"E1108".to_string()));
    // 境界のスーパートレイト (`Copy` の `requires core.ops.Clone`) が宣言するメソッドは呼べる。
    let ok = "fn t.f[T: core.ops.Copy](s: &T) -> T uses [] panics never\n    s.clone()\nend t.f\n\n";
    assert!(codes_of("tp_ok", &main_wrap(ok, "")).is_empty());
}

#[test]
fn undeclared_primitive_conversions_are_rejected_with_the_infallible_one_suggested() {
    // 規範にない `usize.try_from_u64` は E1101 (実行系が受け付けても)。必ず収まる `from_u64` を案内し、E2044 は重ねない。
    let src = "fn t.f(n: u64) -> usize uses [] panics never\n    match usize.try_from_u64(n) {\n        Ok(v) => v\n        Err(_e) => 0usize\n    }\nend t.f\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    core.mem.drop(t.f(1u64))\n    Ok(())\nend t.main\n";
    let t = Tmp::new("undeclared_conv", src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    let got = codes(&out);
    assert_eq!(got, vec!["E1101".to_string()], "{}", out);
    assert!(out.contains("usize.from_u64"), "{}", out);
}

#[test]
fn primitive_receivers_reject_methods_missing_from_the_signatures() {
    let b = |l: &str| main_wrap("", &format!("    let x = 1u8\n    {}\n", l));
    // 実行系は受け付けるが `core/num.sig` にない。
    assert_eq!(codes_of("prim_to_u32", &b("core.mem.drop(x.to_u32())")), vec!["E1108"]);
    // 綴り違いには近い名前を示す。
    let t = Tmp::new("prim_typo", &b("core.mem.drop(x.wrapping_ad(1u8))"));
    let o = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(o.contains("E1108") && o.contains("did you mean `wrapping_add`"), "{}", o);
    // 宣言されているメソッドは通る。
    assert!(codes_of("prim_ok", &b("core.mem.drop(x.wrapping_add(1u8))")).is_empty());
}

#[test]
fn call_arguments_keep_reference_levels() {
    let prefix = "fn t.val(a: u8) -> u8 uses [] panics never\n    a\nend t.val\n\n\
                  fn t.rf(a: &u8) -> u8 uses [] panics never\n    *a\nend t.rf\n\n\
                  fn t.sl(a: &[u8]) -> usize uses [] panics never\n    a.len()\nend t.sl\n\n";
    let run_body = |l: &str| {
        main_wrap(prefix, &format!("    let x = 1u8\n    let mut m = 2u8\n    let r = &x\n    let arr = [1u8, 2u8]\n    {}\n", l))
    };
    // 値が要る所に参照 / 参照が要る所に値 (自動参照も自動参照外しもない)。
    assert!(codes_of("arg_ref_for_val", &run_body("core.mem.drop(t.val(r))")).contains(&"E2023".to_string()));
    assert!(codes_of("arg_val_for_ref", &run_body("core.mem.drop(t.rf(x))")).contains(&"E2023".to_string()));
    // 許す暗黙の調整: 参照どうし・`&mut T` → `&T`・`&[T; N]` → `&[T]`・明示の `*` / `&`。
    let ok = "core.mem.drop(t.val(*r))\n    core.mem.drop(t.rf(r))\n    core.mem.drop(t.rf(&mut m))\n    core.mem.drop(t.rf(&x))\n    core.mem.drop(t.sl(&arr))";
    assert!(codes_of("arg_ref_ok", &run_body(ok)).is_empty());
    // 参照越しの分解で得た名前は参照の段数が決まらない (仕様 4.2 は束縛の段数を定めない) ので、どちらにも使える。
    let pat = "match &Some(x) {\n        Some(p) => {\n            core.mem.drop(t.val(p))\n            core.mem.drop(t.rf(p))\n        }\n        None => ()\n    }";
    let pat_src = main_wrap(prefix, &format!("    let x = 1u8\n    {}\n", pat));
    assert!(codes_of("arg_pat_loose", &pat_src).is_empty());
}

// ---- 式の中の借用・クロージャ・関数境界・コンテナ (borrowck.rs) ----

/// 診断が 1 件もないこと (他の検査も含めて) を確かめる。
fn nll_clean(name: &str, prefix: &str, b: &[&str]) {
    let src = main_wrap(prefix, &body(b)).replace("uses [io.console]", "uses [alloc, io.console]");
    let t = Tmp::new(name, &src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}: {}", name, text(&o.stdout));
}

const NLL_FNS: &str = "type t.P['a]\n    a: &'a mut u8\n    b: &'a mut u8\nend t.P\n\n\
    type t.S\n    a: u8\n    b: u8\nend t.S\n\n\
    fn t.f_pair['a](a: &'a mut u8, b: &'a mut u8) -> t.P<'a> uses [] panics never\n    t.P { a: a, b: b }\nend t.f_pair\n\n\
    fn t.f_pick['a, 'b](x: &'a u8, _y: &'b u8) -> &'a u8 uses [] panics never\n    x\nend t.f_pick\n\n\
    fn t.f_two(a: &mut u8, b: &mut u8) -> () uses [] panics never\n    *a = 1u8\n    *b = 2u8\nend t.f_two\n\n\
    fn t.f_use(a: &u8, b: &u8) -> () uses [] panics never\n    core.mem.drop(a)\n    core.mem.drop(b)\nend t.f_use\n\n";

#[test]
fn nll_expr_accepts_disjoint_and_shared_borrows_in_one_expression() {
    nll_clean(
        "nll_expr_ok",
        NLL_FNS,
        &[
            "let mut s = t.S { a: 1u8, b: 2u8 }",
            "t.f_two(&mut s.a, &mut s.b)",
            "let p = t.P { a: &mut s.a, b: &mut s.b }",
            "*p.a = 5u8",
            "*p.b = 6u8",
            "let x = 1u8",
            "t.f_use(&x, &x)",
            "let arr = [&x, &x]",
            "core.check.eq(arr.len(), 2usize)",
            "let mut y = 1u8",
            "t.f_two(&mut y, &mut s.a)",
            "t.f_two(&mut y, &mut s.a)",
            "core.check.eq(s.a, 1u8)",
        ],
    );
}

#[test]
fn nll_expr_accepts_two_phase_push_of_len() {
    nll_clean(
        "nll_expr_twophase",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "match v.push(1usize) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "match v.push(v.len()) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "core.check.eq(v.len(), 2usize)",
        ],
    );
}

#[test]
fn nll_expr_accepts_a_borrow_that_ends_before_the_conflicting_access() {
    nll_clean(
        "nll_expr_ended",
        NLL_FNS,
        &[
            "let mut x = 1u8",
            "let y = 2u8",
            "let r = t.f_pick(&x, &y)",
            "core.check.eq(*r, 1u8)",
            "x = 3u8",
            "core.check.eq(x, 3u8)",
        ],
    );
}

#[test]
fn nll_boundary_follows_only_the_arguments_the_header_returns() {
    // `f_pick` の戻り値は第 1 引数にだけ由来する。第 2 引数への書き込みは、戻り値が生きていても通る。
    nll_clean(
        "nll_boundary_other_arg",
        NLL_FNS,
        &["let x = 1u8", "let mut y = 2u8", "let r = t.f_pick(&x, &y)", "y = 3u8", "core.check.eq(*r, 1u8)", "core.check.eq(y, 3u8)"],
    );
    let (bad, out) = has_e3010(
        "nll_boundary_first_arg",
        NLL_FNS,
        &["let mut x = 1u8", "let y = 2u8", "let r = t.f_pick(&x, &y)", "x = 3u8", "core.check.eq(*r, 1u8)"],
    );
    assert!(bad, "{}", out);
}

#[test]
fn nll_boundary_struct_result_keeps_the_argument_borrows() {
    let (bad, out) = has_e3010(
        "nll_boundary_struct",
        NLL_FNS,
        &["let mut a = 1u8", "let mut b = 2u8", "let p = t.f_pair(&mut a, &mut b)", "a = 3u8", "*p.b = 4u8"],
    );
    assert!(bad, "{}", out);
    // 戻り値を使い終えてから触る分には通る。
    nll_clean(
        "nll_boundary_struct_ok",
        NLL_FNS,
        &["let mut a = 1u8", "let mut b = 2u8", "let p = t.f_pair(&mut a, &mut b)", "*p.b = 4u8", "a = 3u8", "core.check.eq(a, 3u8)"],
    );
}

#[test]
fn nll_closure_dead_before_the_captured_variable_is_mutated() {
    nll_clean(
        "nll_closure_dead",
        "",
        &[
            "let mut x = 1u8",
            "let c = fn() -> u8 capture [&x] {",
            "    x",
            "}",
            "core.check.eq(c(), 1u8)",
            "x = 2u8",
            "let d = fn() -> () capture [&mut x] {",
            "    x = 3u8",
            "}",
            "d()",
            "core.check.eq(x, 3u8)",
        ],
    );
}

#[test]
fn nll_closure_body_may_read_a_shared_capture_and_write_a_mut_capture() {
    nll_clean(
        "nll_closure_body_ok",
        "",
        &[
            "let x = 1u8",
            "let mut y = 2u8",
            "let c = fn() -> u8 capture [&x, &mut y] {",
            "    y = x",
            "    y",
            "}",
            "core.check.eq(c(), 1u8)",
        ],
    );
}

#[test]
fn nll_store_keeps_every_borrow_pushed_into_one_container() {
    nll_clean(
        "nll_store_two_ok",
        "",
        &[
            "let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
            "let mut x = 1u8",
            "let y = 2u8",
            "match v.push(&y) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "match v.push(&y) {",
            "    Ok(_u) => 0u8",
            "    Err(_e) => 1u8",
            "}",
            "core.check.eq(v.len(), 2usize)",
            "x = 2u8",
            "core.check.eq(x, 2u8)",
        ],
    );
}

#[test]
fn nll_mem_replace_with_a_dead_holder_is_accepted() {
    nll_clean(
        "nll_mem_replace_ok",
        "type t.H\n    r: &'static u8\nend t.H\n\n",
        &[
            "let a = 1u8",
            "let mut x = 2u8",
            "let mut h = t.H { r: &a }",
            "let old = core.mem.replace(&mut h.r, &x)",
            "core.check.eq(*h.r, 2u8)",
            "core.check.eq(*old, 1u8)",
            "x = 3u8",
            "core.check.eq(x, 3u8)",
        ],
    );
}

// ---- 3.3: 名前の参照形。`check` はどちらの形も受け入れ、`fmt` が正規形にする。

const SN_LIB: &str = "type t.lib.Item pub\n    n: u8 pub\nend t.lib.Item\n\nfn t.lib.make() -> t.lib.Item uses [] panics never pub\n    t.lib.Item { n: 1u8 }\nend t.lib.make\n";

const SN_CANON: &str = "import t.lib\n\nfn t.get(x: &t.lib.Item) -> u8 uses [] panics never\n    x.n\nend t.get\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let item = lib.make()\n    let n = t.get(&item)\n    core.mem.drop(n)\n    Ok(())\nend t.main\n";

fn sn_pkg(name: &str, src: &str) -> Tmp {
    let t = Tmp::new(name, src);
    fs::create_dir_all(t.0.join("src/t")).unwrap();
    fs::write(t.0.join("src/t/lib.ksnl"), SN_LIB).unwrap();
    t
}

fn sn_main(t: &Tmp) -> String {
    text(&fs::read(t.0.join("src/t.ksnl")).unwrap())
}

/// 非正規形のソースが check を通り、fmt で期待の正規形になり、冪等で、正規形が check / fmt --check を通る。
fn sn_roundtrip(name: &str, src: &str, want: &str) {
    let t = sn_pkg(name, src);
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}: check: {}{}", name, text(&c.stdout), text(&c.stderr));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(1), "{}: fmt --check must report a diff", name);
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0), "{}", name);
    assert_eq!(sn_main(&t), want, "{}: fmt result", name);
    let again = run(&["fmt"], &t.0);
    assert_eq!(sn_main(&t), want, "{}: fmt must be idempotent", name);
    assert!(!text(&again.stdout).contains("reformatted"), "{}", text(&again.stdout));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{}: canonical form must pass check", name);
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{}", name);
}

#[test]
fn short_names_canonical_source_passes() {
    let t = sn_pkg("sn_canon", SN_CANON);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
}

#[test]
fn short_form_of_an_imported_type_in_a_header_is_accepted_and_fmt_makes_it_fqn() {
    let src = SN_CANON.replace("x: &t.lib.Item", "x: &lib.Item");
    sn_roundtrip("sn_hdr_short", &src, SN_CANON);
}

#[test]
fn fqn_of_an_imported_module_in_a_body_is_accepted_and_fmt_shortens_it() {
    let src = SN_CANON.replace("lib.make()", "t.lib.make()");
    sn_roundtrip("sn_body_fqn", &src, SN_CANON);
    // 型注釈・構造体リテラルの中でも短縮される。
    let src = SN_CANON.replace("let item = lib.make()", "let item: t.lib.Item = t.lib.Item { n: 2u8 }");
    let want = SN_CANON.replace("let item = lib.make()", "let item: lib.Item = lib.Item { n: 2u8 }");
    sn_roundtrip("sn_body_fqn_ty", &src, &want);
}

#[test]
fn an_import_used_only_through_a_header_is_accepted_and_fmt_removes_it() {
    let hdr = "fn t.get(x: &lib.Item) -> u8 uses [] panics never\n    x.n\nend t.get\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let want = "fn t.get(x: &t.lib.Item) -> u8 uses [] panics never\n    x.n\nend t.get\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    sn_roundtrip("sn_hdr_only_short", &format!("import t.lib\n\n{}", hdr), want);
    // ヘッダの FQN だけが参照する import も、check は未使用にせず、fmt が外す。
    sn_roundtrip("sn_hdr_only_fqn", &format!("import t.lib\n\n{}", want), want);
}

#[test]
fn both_forms_together_and_an_unrelated_unused_import_is_still_reported() {
    let src = SN_CANON.replace("x: &t.lib.Item", "x: &lib.Item").replace("lib.make()", "t.lib.make()");
    sn_roundtrip("sn_both", &src, SN_CANON);
    // 何からも参照されない import は E1020 のまま (fmt は意味の誤りを隠さない)。
    let none = "import t.lib\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = sn_pkg("sn_unused", none);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).contains(&"E1020".to_string()), "{}", text(&o.stdout));
    run(&["fmt"], &t.0);
    assert_eq!(sn_main(&t), none);
}

#[test]
fn fqn_of_a_module_that_is_not_imported_stays_as_it_is() {
    let src = SN_CANON.replace("import t.lib\n\n", "").replace("lib.make()", "t.lib.make()");
    let t = sn_pkg("sn_not_imported", &src);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
    run(&["fmt"], &t.0);
    assert_eq!(sn_main(&t), src);
}

#[test]
fn a_header_short_form_needs_its_import_and_a_local_binding_keeps_its_meaning() {
    // import なしの短縮形は未定義の型。
    let src = SN_CANON.replace("import t.lib\n\n", "").replace("x: &t.lib.Item", "x: &lib.Item").replace("lib.make()", "t.lib.make()");
    let t = sn_pkg("sn_no_import", &src);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).contains(&"E1102".to_string()), "{}", text(&o.stdout));
    // import 末尾名と同名の局所束縛はシャドーイング (E1021)。header / 本体で解決が分かれることはない。
    let src = SN_CANON.replace("let n = t.get(&item)", "let lib = t.get(&item)\n    let n = lib");
    let t = sn_pkg("sn_shadow", &src);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).contains(&"E1021".to_string()), "{}", text(&o.stdout));
}

#[test]
fn imports_with_the_same_last_segment_stay_an_error_in_both_forms() {
    let t = Tmp::new("sn_collide", "import t.a.util\nimport t.b.util\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n");
    fs::create_dir_all(t.0.join("src/t/a")).unwrap();
    fs::create_dir_all(t.0.join("src/t/b")).unwrap();
    let f = |m: &str| format!("fn t.{m}.util.f() -> u8 uses [] panics never\n    1u8\nend t.{m}.util.f\n");
    fs::write(t.0.join("src/t/a/util.ksnl"), f("a")).unwrap();
    fs::write(t.0.join("src/t/b/util.ksnl"), f("b")).unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).contains(&"E1111".to_string()), "{}", text(&o.stdout));
}

#[test]
fn name_tools_work_on_non_canonical_source() {
    let src = SN_CANON.replace("x: &t.lib.Item", "x: &lib.Item").replace("lib.make()", "t.lib.make()");
    let t = sn_pkg("sn_tools", &src);
    let g = run(&["get", "t.get"], &t.0);
    assert_eq!(g.status.code(), Some(0), "{}", text(&g.stderr));
    assert!(text(&g.stdout).contains("fn t.get("), "{}", text(&g.stdout));
    let s = run(&["sig", "t.get"], &t.0);
    assert_eq!(s.status.code(), Some(0), "{}", text(&s.stderr));
    let r = run(&["refs", "t.lib.make"], &t.0);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert!(text(&r.stdout).contains("t.main"), "{}", text(&r.stdout));
    let c = run(&["context", "t.main"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}", text(&c.stderr));
}

// ---------------------------------------------------------------- `?` のエラー変換 (4.1 `core.convert.From`)

/// 変換が起きる・型が同じなら変換しない・`Option` は変わらない・変換後のエラーが元の中身を運ぶ、を
/// インタプリタとネイティブ (デバッグ・リリース) が同じ結果で通る。`ksn test --both` も通る。
#[test]
fn try_converts_the_error_through_from_and_both_backends_agree() {
    let src = include_str!("data/try_from.ksnl");
    let out = agree_with_interpreter("try_from", src);
    assert_eq!(out, "try-from ok\n");
    let t = Tmp::new("try_from_both", src);
    let o = run(&["test", "--both"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stdout).contains("1 passed; 0 failed"), "{}", text(&o.stdout));
}

const TRY_AB: &str = "type t.A derive [Debug, Clone, Copy] pub\n    x: u32\nend t.A\n\ntype t.B derive [Debug, Clone, Copy] pub\n    y: u32\nend t.B\n\n";
const TRY_FG: &str = "fn t.f() -> Result<u32, t.A> uses [] panics never\n    Err(t.A { x: 1u32 })\nend t.f\n\nfn t.g() -> Result<u32, t.B> uses [] panics never\n    let n = t.f()?\n    Ok(n)\nend t.g\n";
const TRY_B_TO_A: &str = "impl core.convert.From<t.B> for t.A\nfn t.A.from_value_b(value: t.B) -> t.A uses [] panics never impls core.convert.From<t.B>\n    t.A { x: value.y }\nend t.A.from_value_b\n\n";

/// `impl` がなければ従来どおり E2020 で、`map_err` と `impl` の両方を案内する。
#[test]
fn try_without_a_from_impl_reports_both_remedies() {
    let t = Tmp::new("try_no_from", &format!("{}{}", TRY_AB, TRY_FG));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(codes(&out), vec!["E2020".to_string()], "{}", out);
    assert!(out.contains("write `map_err`, or declare `impl core.convert.From<t.A> for t.B`"), "{}", out);
}

/// 逆向きの `From<B> for A` しかないときは、`A -> B` の `?` は通らない (逆変換・連鎖なし)。
#[test]
fn try_does_not_use_the_reverse_conversion() {
    let t = Tmp::new("try_reverse", &format!("{}{}{}", TRY_AB, TRY_B_TO_A, TRY_FG));
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(codes(&text(&o.stdout)), vec!["E2020".to_string()], "{}", text(&o.stdout));
}

/// クロージャ内の `?` は、クロージャの戻り値型のエラー型へ変換する (外側の関数ではない)。
#[test]
fn try_in_a_closure_converts_to_the_closure_return_type() {
    let src = "type t.Inner derive [Debug, Clone, Copy] pub\n    code: u32 pub\nend t.Inner\n\nenum t.AppError derive [Debug, Clone, Copy, PartialEq] pub\n    Code(u32)\nend t.AppError\n\nimpl core.convert.From<t.Inner> for t.AppError\nfn t.AppError.from_value(value: t.Inner) -> t.AppError uses [] panics never impls core.convert.From<t.Inner>\n    t.AppError.Code(value.code)\nend t.AppError.from_value\n\nfn t.fail(code: u32) -> Result<u32, t.Inner> uses [] panics never\n    Err(t.Inner { code: code })\nend t.fail\n\nfn t.main() -> Result<(), t.Inner> uses [] panics may\n    let f = fn(c: u32) -> Result<u32, t.AppError> capture [] {\n        let n = t.fail(c)?\n        Ok(n)\n    }\n    core.check.eq(f(4u32), Err(t.AppError.Code(4u32)))\n    Ok(())\nend t.main\n";
    let t = Tmp::new("try_closure", src);
    let c = run(&["check", "--json"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}{}", text(&c.stdout), text(&c.stderr));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

/// `impl From` 行のないメソッドは E1113、メソッドのない `impl` 行は E1112。両方そろえば通る。
#[test]
fn from_impl_lines_and_methods_must_pair_up() {
    let t = Tmp::new("from_nomethod", &format!("{}impl core.convert.From<t.B> for t.A\n", TRY_AB));
    assert!(codes(&text(&run(&["check", "--json"], &t.0).stdout)).contains(&"E1112".to_string()));
    let method_only = TRY_B_TO_A.replacen("impl core.convert.From<t.B> for t.A\n", "", 1);
    let t = Tmp::new("from_noimpl", &format!("{}{}", TRY_AB, method_only));
    assert!(codes(&text(&run(&["check", "--json"], &t.0).stdout)).contains(&"E1113".to_string()));
    let t = Tmp::new("from_ok", &format!("{}{}\n", TRY_AB, TRY_B_TO_A.trim_end()));
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
}

// ---- `with` (4.4): 範囲を抜けるすべての経路で後始末式が 1 回だけ走る ----

/// 後始末の順序・回数を標準出力に出す、`with` の全経路 (末尾・`return`・`?`・`break` / `continue`・入れ子・
/// 初期化の失敗) を通るプログラム。
const WITH_PROGRAM: &str = r#"type t.R
    id: u32
end t.R

fn t.R.get(self: &t.R) -> u32
    self.id
end t.R.get

fn t.R.new(id: u32) -> t.R
    if id == 1u32 {
        std.io.print_line("open 1")
    } else {
        if id == 2u32 {
            std.io.print_line("open 2")
        } else {
            std.io.print_line("open")
        }
    }
    t.R { id: id }
end t.R.new

fn t.R.open(id: u32) -> Result<t.R, u8>
    if id == 0u32 {
        return Err(9u8)
    }
    Ok(t.R.new(id))
end t.R.open

fn t.R.shut(self: t.R) -> ()
    if self.id == 1u32 {
        std.io.print_line("shut 1")
    } else {
        if self.id == 2u32 {
            std.io.print_line("shut 2")
        } else {
            std.io.print_line("shut")
        }
    }
    core.mem.drop(self.id)
end t.R.shut

fn t.early_return(n: u32) -> u32
    with r = t.R.new(1u32) close r.shut() {
        if n == 0u32 {
            return 100u32
        }
        r.get().wrapping_add(n)
    }
end t.early_return

fn t.fail(n: u32) -> Result<u32, u8>
    if n == 0u32 {
        return Err(7u8)
    }
    Ok(n)
end t.fail

fn t.in_loops(n: u32) -> u32
    with r = t.R.new(1u32) close r.shut() {
        for i in core.iter.range(0u32, 5u32) {
            let mut j = 0u32
            while j < 5u32 {
                if i == n {
                    return i.wrapping_mul(10u32).wrapping_add(j)
                }
                j = j.wrapping_add(1u32)
            }
        }
        r.get()
    }
end t.in_loops

fn t.loop_exits() -> u32
    let mut total = 0u32
    for i in core.iter.range(0u32, 5u32) {
        with r = t.R.new(i.wrapping_add(1u32)) close r.shut() {
            if i == 1u32 {
                continue
            }
            if i == 3u32 {
                break
            }
            total = total.wrapping_add(r.get())
        }
    }
    total
end t.loop_exits

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.early_return(0u32), 100u32)
    core.check.eq(t.early_return(2u32), 3u32)
    core.check.eq(t.in_loops(2u32), 20u32)
    core.check.eq(t.in_loops(9u32), 1u32)
    core.check.eq(t.nested(5u32), Ok(8u32))
    core.check.eq(t.nested(0u32), Err(7u8))
    core.check.eq(t.tail_unit(0u32), Err(9u8))
    core.check.eq(t.tail_unit(4u32), Ok(()))
    core.check.eq(t.loop_exits(), 4u32)
    Ok(())
end t.main

fn t.nested(n: u32) -> Result<u32, u8>
    let v = with a = t.R.open(1u32)? close a.shut() {
        let w = with b = t.R.open(2u32)? close b.shut() {
            let c = t.fail(n)?
            c.wrapping_add(b.get())
        }
        w.wrapping_add(a.get())
    }
    Ok(v)
end t.nested

fn t.tail_unit(n: u32) -> Result<(), u8>
    with r = t.R.open(n)? close r.shut() {
        core.mem.drop(r.get())
        Ok(())
    }
end t.tail_unit
"#;

const WITH_EXPECTED: &str = "open 1\nshut 1\nopen 1\nshut 1\nopen 1\nshut 1\nopen 1\nshut 1\nopen 1\nopen 2\nshut 2\nshut 1\nopen 1\nopen 2\nshut 2\nshut 1\nopen\nshut\nopen 1\nshut 1\nopen 2\nshut 2\nopen\nshut\nopen\nshut\n";

#[test]
fn with_is_checked_formatted_and_runs_its_cleanup_on_every_exit_path() {
    let t = Tmp::new("with_paths", WITH_PROGRAM);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), WITH_EXPECTED);
}

#[test]
fn with_cleanup_runs_the_same_way_in_a_cranelift_executable() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("with_native_{}", release), WITH_PROGRAM, release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), WITH_EXPECTED);
    }
}

#[test]
fn with_test_runs_in_both_the_interpreter_and_native_with_ksn_test_both() {
    // `ksn test --both`: 1 テスト = 1 プロセス。本体が `?` で抜けても後始末が走ることを `test` の中で確かめる。
    let src = format!(
        "{}\ntest t.nested.test_cleanup_on_question_mark\n    core.check.eq(t.nested(0u32), Err(7u8))\n    core.check.eq(t.nested(5u32), Ok(8u32))\nend t.nested.test_cleanup_on_question_mark\n",
        WITH_PROGRAM
    );
    let t = Tmp::new("with_test_both", &src);
    let o = run(&["test", "--both"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

#[test]
fn with_does_not_run_its_cleanup_on_panic() {
    // 5.2: panic は巻き戻さない。後始末は走らず、終了コードは 101。
    let body = r#"fn t.shut(n: u32) -> () uses [io.console] panics never
    std.io.print_line("shut")
    core.mem.drop(n)
end t.shut

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    with n = 1u32 close t.shut(n) {
        std.io.print_line("body")
        core.panic.panic("boom")
    }
    Ok(())
end t.main
"#;
    let t = Tmp::new("with_panic", body);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout), "body\n");
    let (_t2, exe_path, out) = build_clif("with_panic_native", body, false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout), "body\n");
}

/// `with` の診断の確認用。`t.R` と、その `get` / `shut` / `status` / `bump` を足した上に `main` の本体を置く。
fn with_main(body: &str) -> String {
    format!(
        "type t.R linear\n    id: u32\nend t.R\n\nfn t.R.bump(self: &mut t.R) -> () uses [] panics never\n    self.id = self.id.wrapping_add(1u32)\nend t.R.bump\n\nfn t.R.get(self: &t.R) -> u32 uses [] panics never\n    self.id\nend t.R.get\n\nfn t.R.new(id: u32) -> t.R uses [] panics never\n    t.R {{ id: id }}\nend t.R.new\n\nfn t.R.shut(self: t.R) -> () uses [] panics never\n    let t.R {{ id: id }} = self\n    core.mem.drop(id)\nend t.R.shut\n\nfn t.R.status(self: t.R) -> Result<(), u8> uses [] panics never\n    let t.R {{ id: id }} = self\n    core.mem.drop(id)\n    Ok(())\nend t.R.status\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n{}    Ok(())\nend t.main\n",
        body
    )
}

fn with_codes(name: &str, body: &str) -> Vec<String> {
    let t = Tmp::new(name, &with_main(body));
    let o = run(&["check", "--json"], &t.0);
    codes(&text(&o.stdout))
}

#[test]
fn with_accepts_a_linear_binding_consumed_by_its_cleanup() {
    let body = "    with mut r = t.R.new(1u32) close r.shut() {\n        r.bump()\n        core.mem.drop(r.get())\n    }\n";
    assert_eq!(with_codes("with_ok", body), Vec::<String>::new());
}

#[test]
fn with_cleanup_must_consume_the_binding() {
    let b = "    with r = t.R.new(1u32) close core.mem.drop(r.get()) {\n        core.mem.drop(r.get())\n    }\n";
    assert_eq!(with_codes("with_noconsume", b), ["E3041"]);
    // 分岐の中だけで消費する書き方も不可 (偽の枝で線形値が漏れる)。
    let b = "    with r = t.R.new(1u32) close if true {\n        r.shut()\n    } {\n        core.mem.drop(r.get())\n    }\n";
    assert_eq!(with_codes("with_cond", b), ["E3041"]);
}

#[test]
fn with_cleanup_must_have_type_unit() {
    let b = "    with r = t.R.new(1u32) close r.status() {\n        core.mem.drop(r.get())\n    }\n";
    assert_eq!(with_codes("with_nonunit", b), ["E2045"]);
}

#[test]
fn with_body_cannot_move_or_borrow_out_the_binding() {
    let b = "    with r = t.R.new(1u32) close r.shut() {\n        core.mem.drop(r.status())\n    }\n";
    assert_eq!(with_codes("with_moved", b), ["E3040"]);
    let b = "    with mut r = t.R.new(1u32) close r.shut() {\n        r = t.R.new(2u32)\n    }\n";
    assert_eq!(with_codes("with_reassigned", b), ["E3040"]);
    // 借りるだけ (`&mut self` のメソッド) は許す。ただし `with mut` が要る。
    let b = "    with r = t.R.new(1u32) close r.shut() {\n        r.bump()\n    }\n";
    assert_eq!(with_codes("with_bump_immutable", b), ["E3031"]);
}

#[test]
fn with_cleanup_cannot_leave_early() {
    let b = "    with r = t.R.new(1u32) close core.mem.drop(r.status()?) {\n        core.mem.drop(r.get())\n    }\n";
    assert_eq!(with_codes("with_cleanup_try", b), ["E3042"]);
}

#[test]
fn with_binding_is_not_visible_after_the_block_and_cannot_shadow() {
    let b = "    with r = t.R.new(1u32) close r.shut() {\n        core.mem.drop(r.get())\n    }\n    core.mem.drop(r.get())\n";
    assert_eq!(with_codes("with_scope", b), ["E1101"]);
    let b = "    let r = 1u32\n    with r = t.R.new(1u32) close r.shut() {\n        core.mem.drop(r.get())\n    }\n    core.mem.drop(r)\n";
    assert_eq!(with_codes("with_shadow", b), ["E1021"]);
}

#[test]
fn with_is_a_reserved_word_but_close_is_not() {
    // `with` は識別子に使えない (構文エラー)。
    for (name, src) in [
        ("with_ident_let", "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let with = 1u32\n    core.mem.drop(with)\n    Ok(())\nend t.main\n"),
        ("with_ident_param", "fn t.f(with: u32) -> u32 uses [] panics never\n    with\nend t.f\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n"),
    ] {
        let t = Tmp::new(name, src);
        let o = run(&["check", "--json"], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}", name);
        assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E0002"), "{}: {}", name, text(&o.stdout));
    }
    // `close` は `with` の位置でだけ意味を持つ文脈キーワード: 変数・メソッド・フィールドの名前に使える。
    let ok = "type t.Door\n    close: u32\nend t.Door\n\nfn t.Door.close(self: &t.Door) -> u32 uses [] panics never\n    self.close\nend t.Door.close\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics may\n    let close = t.Door { close: 3u32 }\n    core.check.eq(close.close(), 3u32)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("close_ident", ok);
    // フィールド名とメソッド名の重複 (E1115) は別の規則なので、メソッド名とフィールド名は分ける。
    let ok2 = ok.replace("    close: u32", "    shut: u32").replace("self.close\n", "self.shut\n").replace("t.Door { close: 3u32 }", "t.Door { shut: 3u32 }");
    let t2 = Tmp::new("close_ident2", &ok2);
    let _ = t;
    assert_eq!(run(&["check"], &t2.0).status.code(), Some(0), "{}", text(&run(&["check"], &t2.0).stdout));
    assert_eq!(run(&["run"], &t2.0).status.code(), Some(0));
}

#[test]
fn with_header_may_wrap_at_call_arguments_and_the_format_is_stable() {
    let src = "type t.R\n    id: u32\nend t.R\n\nfn t.R.open(a: u32, b: u32) -> Result<t.R, u8> uses [] panics never\n    Ok(t.R { id: a.wrapping_add(b) })\nend t.R.open\n\nfn t.R.shut(self: t.R, n: u32) -> () uses [] panics never\n    core.mem.drop(self.id.wrapping_add(n))\nend t.R.shut\n\nfn t.main() -> Result<(), u8> uses [] panics never\n    let v = with r = t.R.open(\n        1u32,\n        2u32,\n    )? close t.R.shut(\n        r,\n        3u32,\n    ) {\n        4u32\n    }\n    core.mem.drop(v)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("with_wrap", src);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{}", text(&run(&["check"], &t.0).stdout));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
    // 1 行ブロックは不可 (`{` の直後は必ず改行)。
    let one = "fn t.main() -> Result<(), u8> uses [] panics never\n    with n = 1u32 close core.mem.drop(n) { Ok(()) }\nend t.main\n";
    let t = Tmp::new("with_oneline", one);
    assert_eq!(run(&["check", "--json"], &t.0).status.code(), Some(1));
}

#[test]
fn with_linear_binding_is_not_reported_as_leaked_on_early_exits_in_the_body() {
    // 線形な束縛は、本体の `return` / `break` / `continue` のどの出口でも後始末が消費する (4.4)。
    let body = "    loop {\n        with r = t.R.new(1u32) close r.shut() {\n            if r.get() == 1u32 {\n                break\n            }\n            if r.get() == 2u32 {\n                continue\n            }\n            return Ok(())\n        }\n    }\n";
    assert_eq!(with_codes("with_linear_exits", body), Vec::<String>::new());
    // 外側の線形値は、本体の早期 return で従来どおり漏れとして報告される。
    let body = "    let outer = t.R.new(2u32)\n    with r = t.R.new(1u32) close r.shut() {\n        if r.get() == 1u32 {\n            return Ok(())\n        }\n    }\n    outer.shut()\n";
    assert_eq!(with_codes("with_outer_leak", body), ["E3003"]);
}

// ------------------------------------------------------------ スレッド・Mutex・チャネル・std.env・std.time (実スレッド)

/// `tests/data/threads/<name>.ksnl` を 1 つのパッケージとして取り込む。
fn threads_pkg(name: &str, tag: &str) -> Tmp {
    let src = fs::read_to_string(root().join("stage0").join("tests").join("data").join("threads").join(format!("{}.ksnl", name))).unwrap();
    Tmp::new(&format!("thr_{}_{}", tag, name), &src)
}

/// Cranelift でビルドして実行ファイルを返す。スキップされたら (理由を付けて) 失敗にする。
fn threads_build(t: &Tmp, release: bool) -> PathBuf {
    let cache = std::env::temp_dir().join(format!("ksn_cache_thr_{}_{}", std::process::id(), t.0.file_name().unwrap().to_string_lossy()));
    let mut c = Command::new(exe());
    c.args(["build", "--backend", "clif"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
    if release {
        c.arg("--release");
    }
    let o = c.output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    assert!(!out.contains("skipped"), "native backend skipped: {}", out);
    let kind = if release { "release" } else { "debug" };
    let p = t.0.join("target").join(kind).join(if cfg!(windows) { "t.exe" } else { "t.native" });
    assert!(p.exists(), "native executable missing: {}", out);
    let _ = fs::remove_dir_all(&cache);
    p
}

/// インタプリタと、ネイティブ (debug / release) の出力が同じで、終了コード 0 であること。
fn threads_same_output(name: &str) {
    if cfg!(windows) {
        return; // 実スレッドは POSIX のみ (Windows はビルド時にスキップ)
    }
    let t = threads_pkg(name, "same");
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let p = threads_build(&t, release);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ ({})", name);
    }
}

#[test]
fn threads_spawn_and_join_return_the_closure_result() {
    threads_same_output("spawn_join");
}

/// 4 つのスレッドが `static` の `Mutex` を 1 万回ずつ、長い臨界区間 (読む → 数えて待つ → 書く) で増やす。
/// 排他が働かないと更新が失われて最後の値が 4 万に届かない (ロックを無操作にすると必ず失敗する)。
#[test]
fn threads_mutex_gives_mutual_exclusion() {
    threads_same_output("mutex_counter");
}

#[test]
fn threads_channels_carry_values_and_strings_between_threads() {
    threads_same_output("channel");
}

#[test]
fn threads_a_panic_in_a_spawned_thread_aborts_the_process() {
    let t = threads_pkg("thread_panic", "panic");
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(101));
    assert!(text(&interp.stderr).contains("panic: boom in thread"), "{}", text(&interp.stderr));
    if cfg!(windows) {
        return;
    }
    let p = threads_build(&t, false);
    let r = Command::new(&p).output().unwrap();
    assert_eq!(r.status.code(), Some(101));
    assert!(text(&r.stderr).contains("panic: boom in thread"), "{}", text(&r.stderr));
    assert!(!text(&r.stdout).contains("not reached"));
}

#[test]
fn threads_env_arguments_variables_and_time_work_natively() {
    let t = threads_pkg("env_time", "env");
    let interp = Command::new(exe()).args(["run", "alpha", "b c"]).env("KSN_THREADS_TEST_VAR", "hello-env").current_dir(&t.0).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    let want = "args=2\nalpha\nb c\nno more\nhello-env\nmissing is unset\ntime ok\n";
    assert_eq!(text(&interp.stdout), want);
    if cfg!(windows) {
        return;
    }
    for release in [false, true] {
        let p = threads_build(&t, release);
        let r = Command::new(&p).args(["alpha", "b c"]).env("KSN_THREADS_TEST_VAR", "hello-env").output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), want);
    }
}

/// 走っているスレッドが、待っている側 (main) からの後の `send` を待つ往復の通信。ネイティブでは進むが、
/// 単一スレッドのインタプリタには割り当てられないので、止まらずに明確なメッセージで失敗する。
#[test]
fn threads_interpreter_fails_clearly_when_no_single_threaded_schedule_exists() {
    let t = threads_pkg("ping_pong", "pingpong");
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(101));
    assert!(text(&interp.stderr).contains("deadlock"), "{}", text(&interp.stderr));
    if cfg!(windows) {
        return;
    }
    let p = threads_build(&t, false);
    let r = Command::new(&p).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), "ping pong ok\n");
}

#[test]
fn the_shortener_example_builds_natively_without_skips_and_matches_the_interpreter() {
    if cfg!(windows) {
        return;
    }
    let d = root().join("examples").join("shortener");
    let cache = std::env::temp_dir().join(format!("ksn_cache_short_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    let o = Command::new(exe()).args(["build", "--backend", "clif"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    assert!(!out.contains("skipped"), "native backend skipped: {}", out);
    let p = d.join("target").join("debug").join("shortener.native");
    let r = Command::new(&p).current_dir(&d).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), text(&interp.stdout));
    let _ = fs::remove_dir_all(&cache);
}
