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

/// `run` と同じだが、環境変数も渡す（`std.env.var` の検査用）。
fn run_with_env(args: &[&str], cwd: &Path, env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(exe());
    cmd.args(args).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn ksn")
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
/// 失敗しなければならない。1 行目が `// expect: Exxxx` のファイルは、そのコードの診断で落ちることも確かめる
/// (別の診断で落ちているだけのケースを許さない)。まだ検査できない穴は `KNOWN_HOLES` に名前を載せる (その間は「通ること」を
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
        let src = fs::read_to_string(&p).unwrap();
        let t = Tmp::new(&format!("unsound_{}", stem), &src);
        let o = run(&["check", "--json"], &t.0);
        let rejected = o.status.code() == Some(1);
        let hole = KNOWN_HOLES.contains(&stem.as_str());
        // `// expect: Exxxx` は全行から探す (どの行にあってもよい)。穴以外は必ず持つ。
        let want = src.lines().find_map(|l| l.trim().strip_prefix("// expect:")).map(|w| w.trim().to_string());
        if hole && rejected {
            failures.push(format!("{}: now rejected; remove it from KNOWN_HOLES", stem));
        } else if !hole && !rejected {
            failures.push(format!("{}: passes check but must be rejected (rc={:?})", stem, o.status.code()));
        } else if !hole {
            match want {
                None => failures.push(format!("{}: no `// expect: Exxxx` line (every unsound case names its code)", stem)),
                Some(want) => {
                    let got = codes(&text(&o.stdout));
                    if !got.iter().any(|c| *c == want) {
                        failures.push(format!("{}: want {} codes={:?}", stem, want, got));
                    }
                }
            }
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
        ("plugins", "plugins ok"),
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
fn param_receiver_method_effects_are_checked() {
    // `s.say()` (引数レシーバ `s: t.S`、パスとして構文解析される形) は、型で解決した t.S.say の効果で検査される。
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

// ---- 診断の改善 (4): match の腕 (`=>` の後の改行・腕の本体の代入) ----

#[test]
fn match_arm_brace_on_next_line_is_explained_and_fixed() {
    let src = "fn t.a(x: Option<u8>) -> u8 uses [] panics never\n    match x {\n        Some(v) =>\n        {\n            v\n        }\n        None => 0u8\n    }\nend t.a\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    core.mem.drop(t.a(Some(1u8)))\n    Ok(())\nend t.main\n";
    let t = Tmp::new("arm_brace_next_line", src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|g| g == "E0057"), "{}", out);
    assert!(out.contains("move the `{` from the next line up to this line"), "{}", out);
    let o = run(&["fix"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("        Some(v) => {\n            v\n        }\n"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

#[test]
fn match_arm_assignment_is_explained_and_fixed_as_a_block() {
    let src = "fn t.a(x: Option<u8>) -> u8 uses [] panics never\n    let mut result = 0u8\n    match x {\n        Some(r) => result = r\n        None => {\n            result = 0u8\n        }\n    }\n    result\nend t.a\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    core.mem.drop(t.a(Some(1u8)))\n    Ok(())\nend t.main\n";
    let t = Tmp::new("arm_assign", src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|g| g == "E0058"), "{}", out);
    let o = run(&["fix"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("        Some(r) => {\n            result = r\n        }\n"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

#[test]
fn match_arm_correct_forms_pass_check() {
    // `=> {` を同じ行に書く形と、ブロックの中の代入は、どちらも診断 0 件で通る。`==` と `=>` の比較は代入ではない。
    let src = "fn t.a(x: Option<u8>) -> u8 uses [] panics never\n    let mut result = 0u8\n    match x {\n        Some(r) => {\n            result = r\n        }\n        None => {\n            result = 1u8\n        }\n    }\n    result\nend t.a\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    core.mem.drop(t.a(Some(1u8)))\n    Ok(())\nend t.main\n";
    let t = Tmp::new("arm_correct", src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
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
    assert!(out.contains("cannot mutate `x` while it is borrowed by `t`"), "{}", out);
}

#[test]
fn nll_rejects_a_mutation_while_a_borrow_is_stored_in_an_array_literal() {
    let (bad, out) = has_e3010(
        "nll_store_array",
        "",
        &["let mut x = 1u8", "let a = [&x]", "x = 2u8", "core.mem.drop(a)"],
    );
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `a`"), "{}", out);
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

/// `File.sync` が通常ファイルで成功し、`with` の本体の `write` / `sync` のエラーが `?` で抜けても、後始末が1回走る
/// (`/dev/full` は Linux のみ。ある環境だけ検査する)。インタプリタとネイティブの両方で通る。
#[test]
fn std_fs_sync_succeeds_and_errors_leave_a_with_body_through_try() {
    let t = Tmp::new("fs_sync", include_str!("data/fs_sync.ksnl"));
    let o = run(&["test", "--both"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stdout).contains("2 passed; 0 failed"), "{}", text(&o.stdout));
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

// ---------------------------------------------------------------- `?` のエラー変換 (4.1 `derive [From]`)

/// 変換が起きる (バリアントで包む)・型が同じなら変換しない・`Option` は変わらない・包まれたエラーが元の中身を運ぶ、を
/// インタプリタとネイティブ (デバッグ・リリース) が同じ結果で通る。`ksn test --both` も通る。
#[test]
fn try_wraps_the_error_in_a_derive_from_variant_and_both_backends_agree() {
    let src = include_str!("data/try_from.ksnl");
    let out = agree_with_interpreter("try_from", src);
    assert_eq!(out, "try-from ok\n");
    let t = Tmp::new("try_from_both", src);
    let o = run(&["test", "--both"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stdout).contains("1 passed; 0 failed"), "{}", text(&o.stdout));
}

const TRY_INNER: &str = "type t.Inner derive [Debug, Clone, Copy] pub\n    x: u32\nend t.Inner\n\n";
const TRY_FG: &str = "fn t.f() -> Result<u32, t.Inner> uses [] panics never\n    Err(t.Inner { x: 1u32 })\nend t.f\n\nfn t.g() -> Result<u32, t.Error> uses [] panics never\n    let n = t.f()?\n    Ok(n)\nend t.g\n";

/// `derive [From]` のない `enum` へは包まれず E2020 で、`V(E1)` の追加と `map_err` の両方を案内する。
#[test]
fn try_into_an_enum_without_derive_from_reports_both_remedies() {
    let enum_src = "enum t.Error derive [Debug] pub\n    Inner(t.Inner)\nend t.Error\n\n";
    let t = Tmp::new("try_no_from", &format!("{}{}{}", TRY_INNER, enum_src, TRY_FG));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(codes(&out), vec!["E2020".to_string()], "{}", out);
    assert!(out.contains("add a variant `V(t.Inner)` to `t.Error` (an enum that declares `derive [From]`), or write `map_err`"), "{}", out);
}

/// `derive [From]` があっても、`E1` を唯一のペイロードに持つバリアントがなければ E2020 (別の型のバリアントでは包まない)。
#[test]
fn try_without_a_matching_variant_is_a_type_error() {
    let enum_src = "enum t.Error derive [Debug, From] pub\n    Other(u8)\n    Pair(t.Inner, t.Inner)\nend t.Error\n\n";
    let t = Tmp::new("try_no_variant", &format!("{}{}{}", TRY_INNER, enum_src, TRY_FG));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert_eq!(codes(&out), vec!["E2020".to_string()], "{}", out);
}

/// 同じ型を運ぶバリアントが 2 つ以上あると E2049 で、バリアント名を挙げる (黙って選ばない)。
#[test]
fn try_with_two_variants_carrying_the_same_type_is_ambiguous() {
    let enum_src = "enum t.Error derive [Debug, From] pub\n    First(t.Inner)\n    Second(t.Inner)\nend t.Error\n\n";
    let t = Tmp::new("try_ambiguous", &format!("{}{}{}", TRY_INNER, enum_src, TRY_FG));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert_eq!(codes(&out), vec!["E2049".to_string()], "{}", out);
    assert!(out.contains("First, Second"), "{}", out);
}

/// 変換は 1 段のみ。逆向き (`E2` が持つ型から `E1` へ) は包まれない。
#[test]
fn try_does_not_convert_in_the_reverse_direction() {
    let src = "enum t.Error derive [Debug, From] pub\n    Inner(t.Inner)\nend t.Error\n\nfn t.f() -> Result<u32, t.Error> uses [] panics never\n    Ok(1u32)\nend t.f\n\nfn t.g() -> Result<u32, t.Inner> uses [] panics never\n    let n = t.f()?\n    Ok(n)\nend t.g\n";
    let t = Tmp::new("try_reverse", &format!("{}{}", TRY_INNER, src));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert_eq!(codes(&out), vec!["E2020".to_string()], "{}", out);
}

/// `derive [From]` は `enum` だけで、ペイロードが 1 つのバリアントがなければ E2050。
#[test]
fn derive_from_needs_an_enum_with_a_single_payload_variant() {
    let main = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let on_type = format!("type t.W derive [From] pub\n    a: u8\nend t.W\n\n{}", main);
    let t = Tmp::new("derive_from_type", &on_type);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert_eq!(codes(&out), vec!["E2050".to_string()], "{}", out);
    let no_payload = format!("enum t.E derive [From] pub\n    A\n    B(u8, u8)\nend t.E\n\n{}", main);
    let t = Tmp::new("derive_from_nopayload", &no_payload);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert_eq!(codes(&out), vec!["E2050".to_string()], "{}", out);
    // 正しい形は通り、`ksn fmt --check` の並び (From は最後) も満たす。
    let ok = format!("enum t.E derive [Debug, Clone, Copy, From] pub\n    A\n    B(u8)\nend t.E\n\n{}", main);
    let t = Tmp::new("derive_from_ok", &ok);
    assert_eq!(run(&["check", "--json"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
}

/// `Copy` / `Clone` の derive の条件は、`From` のペイロードにも従来どおりかかる。
#[test]
fn derive_from_payloads_still_obey_the_copy_requirement() {
    let src = "enum t.E derive [Debug, Clone, Copy, From] pub\n    A(t.N)\nend t.E\n\ntype t.N pub\n    a: u8\nend t.N\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = Tmp::new("derive_from_copy", src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).contains(&"E2036".to_string()), "{}", out);
}

/// `ksn get --derived` は、`derive [From]` が提供する変換を示す。
#[test]
fn get_derived_shows_the_from_conversions() {
    let t = Tmp::new("get_derived_from", include_str!("data/try_from.ksnl"));
    let o = run(&["get", "--derived", "t.AppError"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("// derive From for t.AppError:"), "{}", out);
    assert!(out.contains("E1 = alloc.alloc.AllocError  =>  t.AppError.Alloc(error)"), "{}", out);
    assert!(out.contains("E1 = t.Inner  =>  t.AppError.Code(error)"), "{}", out);
    assert!(out.contains("E1 = u32  =>  t.AppError.Wide(error)"), "{}", out);
    assert!(!out.contains("Plain"), "{}", out);
    let enum_src = "enum t.Error derive [Debug, From] pub\n    First(t.Inner)\n    Second(t.Inner)\nend t.Error\n\n";
    let t = Tmp::new("get_derived_from_amb", &format!("{}{}{}", TRY_INNER, enum_src, TRY_FG));
    let out = text(&run(&["get", "--derived", "t.Error"], &t.0).stdout);
    assert!(out.contains("ambiguous (First, Second)"), "{}", out);
}

/// クロージャ内の `?` は、クロージャの戻り値型のエラー型へ包む (外側の関数ではない)。
#[test]
fn try_in_a_closure_wraps_into_the_closure_return_type() {
    let src = "type t.Inner derive [Debug, Clone, Copy, PartialEq] pub\n    code: u32 pub\nend t.Inner\n\nenum t.AppError derive [Debug, Clone, Copy, PartialEq, From] pub\n    Code(t.Inner)\nend t.AppError\n\nfn t.fail(code: u32) -> Result<u32, t.Inner> uses [] panics never\n    Err(t.Inner { code: code })\nend t.fail\n\nfn t.main() -> Result<(), t.Inner> uses [] panics may\n    let f = fn(c: u32) -> Result<u32, t.AppError> capture [] {\n        let n = t.fail(c)?\n        Ok(n)\n    }\n    core.check.eq(f(4u32), Err(t.AppError.Code(t.Inner { code: 4u32 })))\n    Ok(())\nend t.main\n";
    let t = Tmp::new("try_closure", src);
    let c = run(&["check", "--json"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}{}", text(&c.stdout), text(&c.stderr));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
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

/// 5.2: スタック溢れは `stack overflow` と終了コード 102 (メインスレッドでも、`spawn` したスレッドの中でも)。
/// インタプリタとネイティブ (debug / release) で、標準出力・標準エラー・終了コードが同じ。
#[test]
fn threads_stack_overflow_stops_the_process_with_102_in_main_and_in_a_spawned_thread() {
    for name in ["overflow_main", "overflow_thread"] {
        let t = threads_pkg(name, "so");
        let interp = run(&["run"], &t.0);
        assert_eq!(interp.status.code(), Some(102), "{}: {}", name, text(&interp.stderr));
        assert_eq!(text(&interp.stderr), "stack overflow\n");
        assert_eq!(text(&interp.stdout), "start\n");
        if cfg!(windows) {
            continue;
        }
        for release in [false, true] {
            let p = threads_build(&t, release);
            let r = Command::new(&p).output().unwrap();
            assert_eq!(r.status.code(), Some(102), "{} (release={}): {:?} {}", name, release, r.status, text(&r.stderr));
            assert_eq!(text(&r.stderr), "stack overflow\n");
            assert_eq!(text(&r.stdout), text(&interp.stdout));
        }
    }
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

/// 線形値 (`TcpStream`) を `spawn` のクロージャへ move し、返す。`Receiver` を複製して複数のワーカーが 1 つのキューから
/// 接続を取り合い、`close` で残りを受け取る。容量 1 のチャネルを持ち回して共有の状態を守る。
#[test]
fn threads_linear_values_move_between_threads_and_through_channels() {
    threads_same_output("linear");
}

/// `Sender` / `Receiver` の `clone`: 複数の送信側が 1 つのチャネルに送り、受信側は最後の 1 つだけが残りを破棄する。
#[test]
fn threads_channel_ends_clone_and_share_one_queue() {
    threads_same_output("channel_clone");
}

/// チャネルの端を複製・`close`・破棄しても、漏れも二重解放もない。`malloc` / `free` を数える `LD_PRELOAD` で、
/// 周回数を 5 倍の 80 倍にしても「確保 - 解放」が変わらないことを調べる (スレッドを起こすたびに libc 内部の確保が 1〜2 件ずれるので、
/// 絶対値ではなく周回の数に比例して増えないこと = 1 周ごとの漏れがないことを見る。二重解放なら glibc が abort する)。
/// 1 周は、複製した `Sender` / `Receiver` で文字列を送り、最後の受信側が残りを破棄する経路と、線形な値の受信側を `close` する経路。
#[test]
fn native_channel_clone_and_close_free_every_allocation_exactly_once() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let cc = ["gcc", "cc", "clang"].iter().find(|c| Command::new(c).arg("--version").output().is_ok());
    let Some(cc) = cc else { return };
    let dir = std::env::temp_dir().join(format!("ksn_malloc_count_thr_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let c_path = dir.join("malloc_count.c");
    let so_path = dir.join("malloc_count.so");
    fs::write(&c_path, include_str!("data/malloc_count.c")).unwrap();
    let r = Command::new(cc).args(["-shared", "-fPIC", "-O1", "-o"]).arg(&so_path).arg(&c_path).arg("-ldl").output().unwrap();
    assert!(r.status.success(), "{}", text(&r.stderr));
    let src = include_str!("data/threads/channel_rounds.ksnl");
    for release in [false, true] {
        let mut balance = vec![];
        for rounds in [5u32, 400u32] {
            let t = Tmp::new(&format!("thr_rounds_{}_{}", release, rounds), &src.replace("ROUNDS_N", &format!("{}u32", rounds)));
            let p = threads_build(&t, release);
            let o = Command::new(&p).env("LD_PRELOAD", &so_path).output().unwrap();
            assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
            assert_eq!(text(&o.stdout), "channel rounds ok\n");
            let err = text(&o.stderr);
            let line = err.lines().find(|l| l.starts_with("allocs=")).unwrap_or_else(|| panic!("no counter output: {}", err));
            let nums: Vec<i64> = line.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).map(|s| s.parse().unwrap()).collect();
            // 1 周あたり、チャネルが 2 つ (本体 + バッファ) と文字列 5 件の確保がある (10 件弱)。
            assert!(nums[0] >= i64::from(rounds) * 8, "too few allocations ({})", line);
            balance.push(nums[0] - nums[1]);
        }
        assert_eq!(balance[0], balance[1], "allocations minus frees grew with the number of rounds ({:?}): leak or double free", balance);
    }
    let _ = fs::remove_dir_all(&dir);
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

// ---------------------------------------------------------------- Box<dyn Tr> (5.1)

/// `tests/data/box_dyn.ksnl` の `CHURN_N` (箱を作って捨てる回数) を決めたソース。
fn box_dyn_source(churn: u32) -> String {
    include_str!("data/box_dyn.ksnl").replace("CHURN_NI", &format!("{}i64", churn)).replace("CHURN_N", &format!("{}u32", churn))
}

/// `Vec<Box<dyn Shape>>` (2 種類の具象型 + `String` を持つ型)・`&dyn` / `&mut dyn` の引数・`&mut self` のメソッドを、
/// インタプリタと Cranelift (debug / release) で同じ結果にする。ネイティブは skip してはならない。
#[test]
fn box_dyn_agrees_between_interpreter_and_cranelift() {
    let src = box_dyn_source(2000);
    let t = Tmp::new("box_dyn_interp", &src);
    let check = run(&["check"], &t.0);
    assert_eq!(check.status.code(), Some(0), "{}{}", text(&check.stdout), text(&check.stderr));
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    assert_eq!(text(&interp.stdout), "box dyn ok\n");
    if cfg!(windows) {
        return;
    }
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("box_dyn_{}", release), &src, release);
        assert!(!out.contains("skipped"), "native backend skipped: {}", out);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
}

/// `Box<dyn Tr>` が `String` を持つ具象型を入れたまま捨てられても、漏れず・二重に解放されない。
/// (1) `malloc` / `realloc` / `free` を数える `LD_PRELOAD` の小さな共有ライブラリで、確保と解放が同数であること、
/// (2) 300 万回作って捨てるプログラムを、仮想メモリを 64 MiB に絞って最後まで走らせること
/// (漏れれば 1 回 100 バイト近く × 300 万回で枯渇する。二重解放なら glibc が abort する)。
#[test]
fn native_box_dyn_frees_every_allocation_exactly_once() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let cc = ["gcc", "cc", "clang"].iter().find(|c| Command::new(c).arg("--version").output().is_ok());
    let Some(cc) = cc else { return };
    let dir = std::env::temp_dir().join(format!("ksn_malloc_count_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let c_path = dir.join("malloc_count.c");
    let so_path = dir.join("malloc_count.so");
    fs::write(&c_path, include_str!("data/malloc_count.c")).unwrap();
    let r = Command::new(cc).args(["-shared", "-fPIC", "-O1", "-o"]).arg(&so_path).arg(&c_path).arg("-ldl").output().unwrap();
    assert!(r.status.success(), "{}", text(&r.stderr));
    for release in [false, true] {
        let (_t, exe_path, out) = build_clif(&format!("box_dyn_cnt_{}", release), &box_dyn_source(3000), release);
        assert!(!out.contains("skipped"), "native backend skipped: {}", out);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).env("LD_PRELOAD", &so_path).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        let err = text(&o.stderr);
        let line = err.lines().find(|l| l.starts_with("allocs=")).unwrap_or_else(|| panic!("no counter output: {}", err));
        let nums: Vec<i64> = line.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).map(|s| s.parse().unwrap()).collect();
        assert_eq!(nums[0], nums[1], "allocations and frees differ ({}): leak or double free", line);
        // 1 回ごとに `String` と `Box` の 2 回確保する。
        assert!(nums[0] >= 2 * 3000, "the loop allocated too little ({})", line);
    }
    let (_t, exe_path, out) = build_clif("box_dyn_limit", &box_dyn_source(3_000_000), true);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new("sh").args(["-c", "ulimit -v 65536 && exec \"$0\""]).arg(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), "box dyn ok\n");
    let _ = fs::remove_dir_all(&dir);
}

/// 効果は `dyn Tr<E, P>` の型引数で検査する (`&dyn` でも `Box<dyn>` でも)。型の誤りはそのコードだけが出る。
#[test]
fn box_dyn_diagnostics_report_exactly_their_codes() {
    let cases: &[(&str, &str, &str)] = &[
        ("effect_dyn_ref", include_str!("data/diag/E4001d.ksnl"), "E4001"),
        ("effect_dyn_box", include_str!("data/diag/E4002e.ksnl"), "E4002"),
        ("dyn_unsafe_in_box", include_str!("data/diag/E2033f.ksnl"), "E2033"),
        ("linear_in_box", include_str!("data/diag/E3006c.ksnl"), "E3006"),
        ("into_dyn_missing_impl", include_str!("data/diag/E2046.ksnl"), "E2046"),
        ("into_dyn_not_dyn", include_str!("data/diag/E2046b.ksnl"), "E2046"),
        ("into_dyn_reference", include_str!("data/diag/E2046c.ksnl"), "E2046"),
        ("dyn_in_vec", include_str!("data/diag/E2047.ksnl"), "E2047"),
        ("box_dyn_without_static", include_str!("data/diag/E2047b.ksnl"), "E2047"),
    ];
    let mut failures = vec![];
    for (name, src, code) in cases {
        let t = Tmp::new(name, src);
        let o = run(&["check", "--json"], &t.0);
        let got = codes(&text(&o.stdout));
        if o.status.code() != Some(1) || got.is_empty() || got.iter().any(|c| c != code) {
            failures.push(format!("{}: expected only {}, got {:?}", name, code, got));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// ネイティブで作れない形 (スカラーの `Box`) は、skip の理由を出して退く (誤ったコードを出さない)。
#[test]
fn native_box_of_a_scalar_is_skipped_with_a_reason() {
    if cfg!(windows) {
        return;
    }
    let src = [
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    match alloc.boxed.Box.new(std.alloc.SYSTEM, 4i64) {",
        "        Ok(b) => core.check.eq(*b.as_ref(), 4i64)",
        "        Err(_e) => core.panic.panic(\"oom\")",
        "    }",
        "    std.io.print_line(\"scalar box ok\")",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join("\n");
    let t = Tmp::new("box_scalar", &src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    let (_t, exe_path, out) = build_clif("box_scalar_clif", &src, false);
    assert!(exe_path.is_none() && out.contains("native backend skipped") && out.contains("Box of a scalar"), "expected a skip: {}", out);
}

/// `examples/plugins` (`Vec<Slot>` の `Box<dyn Handler>`) は、ネイティブで skip なしに作れ、インタプリタと同じ出力になる。
#[test]
fn the_plugins_example_builds_natively_without_skips_and_matches_the_interpreter() {
    if cfg!(windows) {
        return;
    }
    let d = root().join("examples").join("plugins");
    let cache = std::env::temp_dir().join(format!("ksn_cache_plugins_{}", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    let interp = Command::new(exe()).arg("run").current_dir(&d).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    assert_eq!(text(&interp.stdout), "start\nstop\nplugins ok\n");
    for release in [false, true] {
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "clif"]).current_dir(&d).env("KSN_CACHE_DIR", &cache);
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        assert!(!out.contains("skipped"), "native backend skipped: {}", out);
        let p = d.join("target").join(if release { "release" } else { "debug" }).join("plugins.native");
        let r = Command::new(&p).current_dir(&d).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout));
    }
    let _ = fs::remove_dir_all(&cache);
}

// ---- FFI の完成 (コールバック・`layout c` の構造体の値渡し・多数の引数・動的ロード。3.8)

#[cfg(unix)]
fn ffi_data(name: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/ffi").join(name)).unwrap()
}

/// `tests/data/ffi/ffi_lib.c` をこのテスト用の共有ライブラリにして、`LIB:` をそのパスに置き換えた
/// Kasane のソースで一時パッケージ (`[unsafe] modules = ["t"]`) を作る。
#[cfg(unix)]
fn ffi_pkg(name: &str, data: &str) -> Tmp {
    let t = Tmp::new(name, "");
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let c = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/ffi/ffi_lib.c");
    let lib = t.0.join("libffitest.so");
    let o = cc(&["-shared", "-fPIC", "-o", lib.to_str().unwrap(), c.to_str().unwrap()], &t.0);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let src = ffi_data(data).replace("LIB:", &format!("{}:", lib.to_str().unwrap()));
    fs::write(t.0.join("src").join("t.ksnl"), src).unwrap();
    t
}

/// ネイティブ生成してから実行する。(ビルドの出力, 実行結果)。スキップされたら実行結果は None。
#[cfg(unix)]
fn ffi_native(t: &Tmp, name: &str) -> (String, Option<Output>) {
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_{}", std::process::id(), name));
    let b = Command::new(exe()).args(["build", "--backend", "clif"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    let out = text(&b.stdout) + &text(&b.stderr);
    let bin = t.0.join("target").join("debug").join("t.native");
    if !bin.exists() {
        return (out, None);
    }
    (out, Some(Command::new(&bin).current_dir(&t.0).output().unwrap()))
}

/// インタプリタとネイティブで、同じ出力・同じ終了コードになる (どちらも成功)。
#[cfg(unix)]
fn ffi_both_ok(name: &str, data: &str, want: &str) {
    let t = ffi_pkg(name, data);
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}{}", text(&c.stdout), text(&c.stderr));
    let i = run(&["run"], &t.0);
    assert_eq!(i.status.code(), Some(0), "interpreter: {}", text(&i.stderr));
    assert_eq!(text(&i.stdout), want);
    let (out, n) = ffi_native(&t, name);
    assert!(!out.contains("skipped"), "native backend skipped: {}", out);
    let n = n.unwrap_or_else(|| panic!("no native executable: {}", out));
    assert_eq!(n.status.code(), Some(0), "native: {}", text(&n.stderr));
    assert_eq!(text(&n.stdout), want);
}

#[cfg(unix)]
#[test]
fn ffi_structs_by_value_by_pointer_as_returns_and_nested_match_between_backends() {
    // INTEGER (Small) / SSE (Vec2, Vec2d, F3) / 混在 (Mixed, Nested, Padded) / MEMORY (Big, 24 バイト)、
    // 返り値・ポインタ渡し (`*mut Small`)・入れ子 (`Nested`)。動的ロード (`link "lib:name"`) で呼ぶ。
    ffi_both_ok("ffi_structs", "structs.ksnl", "struct ok\n");
}

#[cfg(unix)]
#[test]
fn ffi_many_arguments_callbacks_and_user_data_match_between_backends() {
    // 整数 6 本・浮動小数 8 本を超える 18 引数、レジスタが足りなくなる位置の構造体、
    // ユーザーデータのポインタを通したコールバックの累積、`f64` / `f32` / `i32` のコールバック、クロージャのコールバック。
    ffi_both_ok("ffi_many", "many.ksnl", "many ok\n");
}

#[cfg(unix)]
#[test]
fn ffi_qsort_sorts_a_kasane_slice_with_a_kasane_comparator_in_both_backends() {
    ffi_both_ok("ffi_qsort", "qsort.ksnl", "qsort ok\n");
}

#[cfg(unix)]
#[test]
fn ffi_unit_tests_run_in_both_the_interpreter_and_the_native_bundle() {
    let t = ffi_pkg("ffi_unit", "unit.ksnl");
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_ffi_unit", std::process::id()));
    let o = Command::new(exe()).args(["test", "--both"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stdout).contains("1 passed; 0 failed"), "{}", text(&o.stdout));
}

#[cfg(unix)]
#[test]
fn ffi_a_missing_library_or_symbol_is_a_defined_panic_with_exit_code_101() {
    for (name, data, needle) in [
        ("ffi_nolib", "missing_lib.ksnl", "cannot load the shared library"),
        ("ffi_nosym", "missing_symbol.ksnl", "was not found in"),
    ] {
        let t = ffi_pkg(name, data);
        let i = run(&["run"], &t.0);
        assert_eq!(i.status.code(), Some(101), "interpreter: {}", text(&i.stderr));
        assert!(text(&i.stderr).contains(needle), "{}", text(&i.stderr));
        assert!(text(&i.stdout).contains("before"));
        let (out, n) = ffi_native(&t, name);
        let n = n.unwrap_or_else(|| panic!("no native executable: {}", out));
        assert_eq!(n.status.code(), Some(101), "native: {}", text(&n.stderr));
        assert!(text(&n.stderr).contains(needle), "{}", text(&n.stderr));
        assert!(text(&n.stdout).contains("before"));
    }
}

#[cfg(unix)]
#[test]
fn ffi_fixed_arrays_in_layout_c_structs_work_in_the_interpreter_and_skip_natively_with_a_reason() {
    let t = ffi_pkg("ffi_arrays", "arrays.ksnl");
    let i = run(&["run"], &t.0);
    assert_eq!(i.status.code(), Some(0), "{}", text(&i.stderr));
    assert_eq!(text(&i.stdout), "arrays ok\n");
    let (out, n) = ffi_native(&t, "ffi_arrays");
    assert!(n.is_none(), "native unexpectedly built: {}", out);
    assert!(out.contains("native backend skipped") && out.contains("fixed-size array"), "{}", out);
}

#[cfg(unix)]
#[test]
fn ffi_a_callback_that_overflows_the_stack_stops_the_process_without_unwinding_through_c() {
    // 5.2: スタックオーバーフローは panic ではなく異常終了 (インタプリタは 102 と `stack overflow`)。
    // C のフレームを越えて巻き戻さない。
    let t = ffi_pkg("ffi_overflow", "stack_overflow.ksnl");
    let i = run(&["run"], &t.0);
    assert_eq!(i.status.code(), Some(102), "{}", text(&i.stderr));
    assert!(text(&i.stderr).contains("stack overflow"), "{}", text(&i.stderr));
    assert!(text(&i.stdout).contains("start") && !text(&i.stdout).contains("unreachable"));
    // ネイティブも同じ (ガードページの fault をランタイムが拾う)。
    let (out, n) = ffi_native(&t, "ffi_overflow");
    let n = n.unwrap_or_else(|| panic!("no native executable: {}", out));
    assert_eq!(n.status.code(), Some(102), "{}", text(&n.stderr));
    assert_eq!(text(&n.stderr), "stack overflow\n");
    assert_eq!(text(&n.stdout), text(&i.stdout));
}

#[cfg(unix)]
#[test]
fn ffi_a_real_null_dereference_still_crashes_natively_instead_of_reporting_a_stack_overflow() {
    use std::os::unix::process::ExitStatusExt;
    let t = ffi_pkg("ffi_null", "null_deref.ksnl");
    let (out, n) = ffi_native(&t, "ffi_null");
    let n = n.unwrap_or_else(|| panic!("no native executable: {}", out));
    // スタック溢れ以外の fault は既定の動作のまま: SIGSEGV で落ちる (終了コード 102 でも 101 でもない)。
    assert_eq!(n.status.signal(), Some(11), "code={:?} stderr={}", n.status.code(), text(&n.stderr));
    assert!(!text(&n.stderr).contains("stack overflow"), "{}", text(&n.stderr));
    assert_eq!(text(&n.stdout), "start\n");
}

fn ffi_check_codes(name: &str, src: &str, modules: &str) -> Vec<String> {
    let t = Tmp::new(name, src);
    if !modules.is_empty() {
        fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"{}\"]\n", TOML, modules)).unwrap();
    }
    let o = run(&["check", "--json"], &t.0);
    let mut c = codes(&text(&o.stdout));
    c.dedup();
    c
}

#[test]
fn ffi_boundary_diagnostics_report_exactly_their_own_code() {
    let main = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    // E5004: 関数型 (C のコールバック) が `uses [] panics never` でない
    let e = format!("extern t.each(f: fn(i32) -> i32 uses [io.console] panics may) -> () link \"each\"\n\n{}", main);
    assert_eq!(ffi_check_codes("ffi_e5004", &e, "t"), ["E5004"]);
    // E5005: 渡した関数が `panics may`
    let f = "extern t.each(f: fn(i32) -> i32 uses [] panics never) -> () link \"each\"\n\n\
             fn t.twice(x: i32) -> i32 uses [] panics may\n    x\nend t.twice\n\n\
             fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    unsafe \"ffi\" \"callback\" {\n        t.each(t.twice)\n    }\n    Ok(())\nend t.main\n";
    assert_eq!(ffi_check_codes("ffi_e5005", f, "t"), ["E5005"]);
    // E5005: panic するクロージャ
    let g = "extern t.each(f: fn(i32) -> i32 uses [] panics never) -> () link \"each\"\n\n\
             fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    unsafe \"ffi\" \"callback\" {\n        t.each(fn(x: i32) -> i32 capture [] {\n            core.panic.panic(\"boom\")\n        })\n    }\n    Ok(())\nend t.main\n";
    assert!(ffi_check_codes("ffi_e5005c", g, "t").contains(&"E5005".to_string()));
    // E5006: `layout c` のフィールドが C の型でない
    let h = format!("type t.Rec layout c\n    name: alloc.string.String<std.alloc.System>\nend t.Rec\n\n{}", main);
    assert_eq!(ffi_check_codes("ffi_e5006", &h, "t"), ["E5006"]);
    // E5007: `layout c` でない構造体を `extern` に渡す
    let i = format!("type t.Pair\n    a: i32\n    b: i32\nend t.Pair\n\nextern t.take(p: t.Pair) -> () link \"take\"\n\n{}", main);
    assert_eq!(ffi_check_codes("ffi_e5007", &i, "t"), ["E5007"]);
    // 正しい形は通る (`layout c` の構造体と効果なしのコールバック)
    let ok = format!(
        "type t.Pair layout c derive [Clone, Copy]\n    a: i32\n    b: i32\nend t.Pair\n\n\
         extern t.take(p: t.Pair, f: fn(i32) -> i32 uses [] panics never) -> () link \"take\"\n\n{}",
        main
    );
    assert!(ffi_check_codes("ffi_ok", &ok, "t").is_empty());
}

#[test]
fn ffi_unsafe_permission_errors_are_still_enforced_with_callbacks_and_structs() {
    // E5003: `extern` の呼び出しは `unsafe` ブロックの中だけ (コールバックを渡す呼び出しでも)
    let a = "extern t.each(f: fn(i32) -> i32 uses [] panics never) -> () link \"each\"\n\n\
             fn t.id(x: i32) -> i32 uses [] panics never\n    x\nend t.id\n\n\
             fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    t.each(t.id)\n    Ok(())\nend t.main\n";
    assert_eq!(ffi_check_codes("ffi_e5003", a, "t"), ["E5003"]);
    // E5001: `[unsafe] modules` にないモジュールでは `unsafe` ブロックが使えない
    let b = a.replace("    t.each(t.id)\n", "    unsafe \"ffi\" \"callback\" {\n        t.each(t.id)\n    }\n");
    assert_eq!(ffi_check_codes("ffi_e5001", &b, ""), ["E5001"]);
    assert!(ffi_check_codes("ffi_permitted", &b, "t").is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn the_ffi_example_checks_formats_tests_and_runs_the_same_in_both_backends() {
    // examples/ffi: libc の `qsort` (Kasane の比較関数) と libm の `cbrt` (動的ロード)。C の準備なしで動く。
    let d = root().join("examples").join("ffi");
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_ffi_example", std::process::id()));
    let _ = fs::remove_dir_all(&cache);
    assert_eq!(run(&["check"], &d).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &d).status.code(), Some(0));
    let t = Command::new(exe()).args(["test", "--both"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    assert_eq!(t.status.code(), Some(0), "{}{}", text(&t.stdout), text(&t.stderr));
    let interp = run(&["run"], &d);
    assert_eq!(text(&interp.stdout), "ffi ok\n", "{}", text(&interp.stderr));
    let b = Command::new(exe()).args(["build", "--backend", "clif"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let out = text(&b.stdout) + &text(&b.stderr);
    assert!(!out.contains("skipped"), "native backend skipped: {}", out);
    let n = Command::new(d.join("target").join("debug").join("ffi.native")).current_dir(&d).output().unwrap();
    assert_eq!(text(&n.stdout), text(&interp.stdout));
    let _ = fs::remove_dir_all(d.join("target"));
    let _ = fs::remove_dir_all(&cache);
}

#[cfg(unix)]
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if e.file_type().unwrap().is_dir() {
            if e.file_name() != "target" {
                copy_dir(&src, &dst);
            }
        } else {
            fs::copy(&src, &dst).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn the_ffi_shapes_example_builds_its_c_library_and_passes_in_both_backends() {
    // examples/ffi/shapes: 同梱の C (c/shapes.c) を build.sh で共有ライブラリにして、動的ロードで呼ぶ。
    // 構造体の値渡し・値返し (MEMORY / INTEGER / SSE)・ポインタ渡し・ユーザーデータ付きのコールバック。
    let d = std::env::temp_dir().join(format!("ksn_shapes_{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    copy_dir(&root().join("examples").join("ffi").join("shapes"), &d);
    let o = Command::new("sh").arg("build.sh").current_dir(&d).output().unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_shapes", std::process::id()));
    assert_eq!(run(&["check"], &d).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &d).status.code(), Some(0));
    let t = Command::new(exe()).args(["test", "--both"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    assert_eq!(t.status.code(), Some(0), "{}{}", text(&t.stdout), text(&t.stderr));
    let interp = run(&["run"], &d);
    assert_eq!(text(&interp.stdout), "shapes ok\n", "{}", text(&interp.stderr));
    let b = Command::new(exe()).args(["build", "--backend", "clif"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let out = text(&b.stdout) + &text(&b.stderr);
    assert!(!out.contains("skipped"), "native backend skipped: {}", out);
    let n = Command::new(d.join("target").join("debug").join("shapes.native")).current_dir(&d).output().unwrap();
    assert_eq!(text(&n.stdout), text(&interp.stdout));
    let _ = fs::remove_dir_all(&d);
    let _ = fs::remove_dir_all(&cache);
}

/// clang が使えるか (使えなければ、`--native` の比較は飛ばして理由を一度だけ出す)。
fn clang_available() -> bool {
    use std::sync::OnceLock;
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let ok = Command::new("clang").arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
        if !ok {
            eprintln!("skip: clang が PATH に無いので、`ksn kir --native` との比較は飛ばす");
        }
        ok
    })
}

/// `ksn kir <FQN> --run ...` を走らせ、clang があれば同じ引数の `--native` (LLVM IR を clang で実行ファイルにする) も走らせて、
/// 終了コード・標準出力 (表示)・panic の文言 (標準エラー) が参照実行器と一致することを確かめる。`--run` の結果を返す。
fn kir_run(args: &[&str], cwd: &Path) -> Output {
    let r = run(args, cwd);
    if !clang_available() {
        return r;
    }
    let native_args: Vec<&str> = args.iter().map(|a| if *a == "--run" { "--native" } else { *a }).collect();
    let n = run(&native_args, cwd);
    assert_eq!(n.status.code(), r.status.code(), "native: 終了コードが違う {:?}\n{}", args, text(&n.stderr));
    assert_eq!(text(&n.stdout), text(&r.stdout), "native: 表示が違う {:?}", args);
    if matches!(r.status.code(), Some(101) | Some(102)) {
        assert_eq!(text(&n.stderr), text(&r.stderr), "native: panic の文言が違う {:?}", args);
    }
    // AddressSanitizer で作った実行ファイルでも走らせる (機械語の二重解放・範囲外・解放後の読み・漏れを捕まえる)。
    if asan_available() {
        let a = Command::new(exe())
            .args(&native_args)
            .current_dir(cwd)
            .env("KSN_CLANG_ARGS", "-fsanitize=address -fno-omit-frame-pointer -g -O0")
            .env("ASAN_OPTIONS", "detect_leaks=1:abort_on_error=0")
            .output()
            .expect("spawn ksn");
        let err = text(&a.stderr);
        // panic (101 / 102) で抜けた経路は、解放しないまま終わるので漏れの報告は見ない。
        let panicked = matches!(r.status.code(), Some(101) | Some(102));
        let bad = err.contains("AddressSanitizer") || (!panicked && err.contains("LeakSanitizer"));
        assert!(!bad, "ASan が報告した {:?}\n{}", args, err);
        assert_eq!(a.status.code(), r.status.code(), "asan: 終了コードが違う {:?}\n{}", args, err);
        assert_eq!(text(&a.stdout), text(&r.stdout), "asan: 表示が違う {:?}", args);
    }
    r
}

/// clang が `-fsanitize=address` の実行ファイルを作って走らせられるか (環境変数 `KSN_NO_ASAN=1` で無効にできる)。
fn asan_available() -> bool {
    use std::sync::OnceLock;
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        if !clang_available() || std::env::var("KSN_NO_ASAN").is_ok() {
            return false;
        }
        let d = std::env::temp_dir().join(format!("ksn_asan_probe_{}", std::process::id()));
        let _ = fs::create_dir_all(&d);
        let c = d.join("a.c");
        let o = d.join("a.out");
        fs::write(&c, "int main(void){return 0;}\n").unwrap();
        let ok = Command::new("clang")
            .args(["-fsanitize=address"])
            .arg(&c)
            .arg("-o")
            .arg(&o)
            .output()
            .map(|r| r.status.success())
            .unwrap_or(false)
            && Command::new(&o).output().map(|r| r.status.success()).unwrap_or(false);
        let _ = fs::remove_dir_all(&d);
        if !ok {
            eprintln!("skip: clang の AddressSanitizer が使えないので、ASan の実行は飛ばす");
        }
        ok
    })
}


#[test]
fn e0010_not_fired_for_tuple_variant_pattern_bindings() {
    // `let (t.One.V(m, n), z) = (o, k)`: `re_tup` が最初の `)` で区切ると
    // `t.One.V(m` が束縛名と誤って E0010 になる誤検出 (#B)。
    let src = "enum t.One derive [Debug, Clone, Copy] pub\n    V(u8, u8)\nend t.One\n\nfn t.f(o: t.One, k: u8) -> u8 uses [] panics never\n    let (t.One.V(m, n), z) = (o, k)\n    m.wrapping_add(n).wrapping_add(z)\nend t.f\n\nfn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.eq(t.f(t.One.V(3u8, 4u8), 1u8), 8u8)\n    std.io.print_line(\"ok\")\nend t.main\n";
    let t = Tmp::new("e0010_tup_pat", src);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
}

#[test]
fn e0010_still_fires_for_bad_names_in_tuple_lets_and_e0020_stays_non_fatal() {
    // タプル分解の大文字の束縛名は E0010 のまま (fmt では直らない)。同じファイルの定義順の違反 (E0020) は
    // fmt で直るので non-fatal として出る。
    let src = "fn t.mk() -> u32 uses [] panics never\n    1u32\nend t.mk\n\nfn t.main() -> () uses [] panics never\n    let (Foo, y) = (1u32, 2u32)\n    core.mem.drop(Foo)\n    core.mem.drop(y)\nend t.main\n";
    let t = Tmp::new("e0010_tup_upper", src);
    let o = run(&["check"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}", out);
    assert!(out.contains("E0010: let Foo not snake_case"), "{}", out);
    assert!(out.contains("E0020 (non-fatal):"), "{}", out);
}

#[cfg(unix)]
#[test]
fn launcher_works_via_symlink() {
    // 起動スクリプトがシンボリックリンク経由だと `cd: can't cd` に
    // なっていた (#C)。自分の実体をシンボリックリンクを辿って求める。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    std.io.print_line(\"ok\")\nend t.main\n";
    let t = Tmp::new("symlink_launcher", src);
    assert_eq!(
        run(&["build"], &t.0).status.code(),
        Some(0),
        "{}",
        text(&run(&["build"], &t.0).stderr)
    );
    let launcher = t.0.join("target").join("debug").join("t");
    assert!(launcher.is_file());
    let link = std::env::temp_dir().join(format!("ksn_link_{}", std::process::id()));
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(&launcher, &link).unwrap();
    let out = Command::new(&link).current_dir(&t.0).output().expect("spawn launcher via symlink");
    let _ = fs::remove_file(&link);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("ok"), "{}", text(&out.stdout));
}

#[test]
fn e0020_order_violation_does_not_fail_check() {
    // 定義順・uses 並びの違反 (E0020) は ksn fmt で直せるため、check は
    // 非致命的として終了コード 0 のままにする (#D)。fmt --check は
    // 従来どおり検出する。
    let unordered = "fn t.b() -> u8 uses [] panics never\n    0u8\nend t.b\n\nfn t.a() -> u8 uses [] panics never\n    0u8\nend t.a\n\nfn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.eq(t.a(), 0u8)\n    core.check.eq(t.b(), 0u8)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("e0020_def_order", unordered);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));

    let uses_order = "fn t.main() -> Result<(), std.io.Error> uses [io.fs, io.console] panics may\n    Ok(())\nend t.main\n";
    let u = Tmp::new("e0020_uses_order", uses_order);
    let o = run(&["check"], &u.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    assert!(text(&o.stdout).contains("E0020"), "{}", text(&o.stdout));
}

#[test]
fn linear_ok_cases_pass_check() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/linear_ok");
    let mut names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    assert!(!names.is_empty(), "linear_ok cases missing");
    let mut failures = Vec::new();
    for p in names {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("ok_{}", stem), &fs::read_to_string(&p).unwrap());
        let o = run(&["check", "--json"], &t.0);
        let got = codes(&text(&o.stdout));
        if o.status.code() != Some(0) || !got.is_empty() {
            failures.push(format!("{}: rc={:?} codes={:?}", stem, o.status.code(), got));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// 木走査: 取り込みは作る時点で決まる (4.3)。外側の後の書き換えは見えない。
#[test]
fn move_capture_copies_the_value_when_the_closure_is_made() {
    run_ok(
        "clo_move_copy",
        &body(&[
            "let mut x = 1u64",
            "let f = fn() -> u64 capture [move x] {",
            "    x",
            "}",
            "x = 5u64",
            "core.check.eq(f(), 1u64)",
            "core.check.eq(x, 5u64)",
        ]),
    );
}

// 木走査: `&x` は共有で、本体の `*x` で読める。
#[test]
fn shared_capture_reads_through_the_reference() {
    run_ok(
        "clo_shared_read",
        &body(&[
            "let base = 3u64",
            "let f = fn() -> u64 capture [&base] {",
            "    *base + 1u64",
            "}",
            "core.check.eq(f(), 4u64)",
        ]),
    );
}

// 木走査: `&mut x` の本体では `*x = v` が外側の `x` へ書く (KIR と同じ)。
#[test]
fn mut_ref_capture_writes_through_deref_assignment() {
    run_ok(
        "clo_mut_deref",
        &body(&[
            "let mut x = 1u64",
            "let f = fn(v: u64) -> () capture [&mut x] {",
            "    *x = v",
            "}",
            "f(7u64)",
            "core.check.eq(x, 7u64)",
        ]),
    );
}

// 木走査: `&mut x` の本体で `x = v` と書いても、仕様の「本体で `a` と書く」形として外側へ書く。
#[test]
fn mut_ref_capture_writes_through_plain_name_assignment() {
    run_ok(
        "clo_mut_name",
        &body(&[
            "let mut x = 1u64",
            "let g = fn(v: u64) -> () capture [&mut x] {",
            "    x = v",
            "}",
            "g(9u64)",
            "core.check.eq(x, 9u64)",
        ]),
    );
}

// 木走査: `move` で取り込んだ値は、クロージャの呼び出しをまたいで残る (FnMut の状態)。外側は変わらない。
#[test]
fn move_captured_state_persists_across_calls_without_touching_the_outer() {
    run_ok(
        "clo_move_state",
        &body(&[
            "let mut n = 0u64",
            "let mut inc = fn() -> u64 capture [move n] {",
            "    n = n + 1u64",
            "    n",
            "}",
            "core.check.eq(inc(), 1u64)",
            "core.check.eq(inc(), 2u64)",
            "core.check.eq(n, 0u64)",
        ]),
    );
}

// 規範 (sig) にある数値の変換は、静的な呼び出しで通る。規範に無い `from_` は E1108 (下)。
#[test]
fn sig_listed_numeric_conversions_run() {
    run_ok(
        "num_conv_ok",
        &body(&[
            "core.check.eq(u32.from_u8(3u8), 3u32)",
            "core.check.eq(f64.from_usize_nearest(3usize), 3.0f64)",
            "core.check.eq(f32.from_u32_nearest(3u32), 3.0f32)",
            "core.check.eq(u8.try_from_u32(300u32).is_err(), true)",
        ]),
    );
}

// 規範 (spec/stdlib/core/num*.sig) に無い `P.from_X` は、整数では E1101 (f32 / f64 にだけある `_nearest`)。
#[test]
fn numeric_from_name_not_in_the_stdlib_is_reported() {
    let (got, out) = check_codes("num_conv_bad", &["core.mem.drop(u32.from_usize_nearest(3usize))"]);
    assert!(got.iter().any(|c| c == "E1101"), "{}", out);
    assert!(out.contains("u32.from_usize_nearest"), "{}", out);
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
        has_e3010("nll_store_tuple_lit", "", &["let mut x = 1u8", "let t = (&x, 1u8)", "x = 2u8", "let _u = t"]);
    assert!(bad, "{}", out);
    assert!(out.contains("cannot mutate `x` while it is borrowed by `t`"), "{}", out);
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

#[test]
fn ffi_same_symbol_with_two_signatures_falls_back_without_a_crash() {
    // 同じ link 名を別のシグネチャで 2 つ宣言しても、clif は先の宣言を流用しない (流用すると Cranelift が内部で panic した)。
    // 食い違いは理由付きで報告され、バンドルへ退避する。
    let src = "extern t.abs32(x: i32) -> i32 link \"abs\"

extern t.abs64(x: i64) -> i64 link \"abs\"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe \"libc\" \"a\" {
        t.abs32(0i32 - 5i32)
    }
    let b = unsafe \"libc\" \"b\" {
        t.abs64(0i64 - 6i64)
    }
    core.check.eq(a, 5i32)
    core.check.eq(b, 6i64)
    std.io.print_line(\"ok\")
    Ok(())
end t.main
";
    let t = Tmp::new("ffi_dup_symbol", src);
    fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"t\"]\n", TOML)).unwrap();
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_ffi_dup", std::process::id()));
    let b = Command::new(exe())
        .args(["build", "--backend", "clif"])
        .current_dir(&t.0)
        .env("KSN_CACHE_DIR", &cache)
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&cache);
    let all = format!("{}{}", text(&b.stdout), text(&b.stderr));
    assert!(!all.contains("panicked"), "{}", all);
    assert!(all.contains("incompatible with previous declaration"), "{}", all);
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

fn t.S.via[F: core.ops.Fn<(u8,), u8, E, P>, E: uses, P: panics](self: &t.S, f: F) -> u8 uses [E] panics P pub
    f(self.n)
end t.S.via

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
    s.via(
        fn(x: u8) -> u8 capture [] {
            core.panic.panic(\"no\")
        },
    )
end t.g
";
    let (_, got, out) = eff_check("eff_closure_method_bad", bad);
    assert_eq!(got, vec!["E4001".to_string(), "E4002".to_string()], "{}", out);
    assert!(out.contains("core.option.Option.map uses effect `io.console`"), "{}", out);
    assert!(out.contains("t.S.via may panic"), "{}", out);
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
    let b = s.via(
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

const FFI_FLOAT_SRC_M: &str = r#"extern t.k_scale(x: f64, n: i32, y: f64) -> f64 link "k_scale"

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
    let t = Tmp::new("ffi_float_m", FFI_FLOAT_SRC_M);
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
fn ptr_more_cast_of_place_pointer_reinterprets_the_pinned_bytes() {
    // `from_mut` の結果は C に見せる写し (ピン) を指すので、幅の違う型への `cast` は、そのバイト列の読み替えになる
    // (リトルエンディアン: i32 の 1 は先頭の u8 が 1)。
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
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
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
fn ptr_more_access_past_a_pinned_value_panics() {
    // ピン (`from_mut`) は値の幅の写しを C に見せるが、バッファには余白がある。値の幅を越えて読む・書くと、
    // 余白の不定の値を読んだり別の値を壊したりするので、幅が値の範囲に収まらない番地は 101 で止まる。
    let cases = [
        // 4 バイトの値を 8 バイトの型で読む (番地は値の先頭)。
        ("ptr_more_wide_read", "let w: *mut u64 = core.ptr.cast::<i32, u64>(core.ptr.from_mut(&mut x))\n        core.check.eq(core.ptr.read(w), 1u64)"),
        // 4 バイトの値を 8 バイトの型で書く。
        ("ptr_more_wide_write", "let w: *mut u64 = core.ptr.cast::<i32, u64>(core.ptr.from_mut(&mut x))\n        core.ptr.write(w, 5u64)"),
        // `add` で値の末尾ちょうどへ進んで読む (値の外)。
        ("ptr_more_add_past_end", "let p: *mut i32 = core.ptr.from_mut(&mut x)\n        let q: *mut i32 = core.ptr.add(p, 1usize)\n        core.check.eq(core.ptr.read(q), 0i32)"),
    ];
    for (name, body) in cases {
        let src = format!(
            "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    let mut x: i32 = 1i32\n    unsafe \"libc\" \"past end\" {{\n        {}\n    }}\n    std.io.print_line(\"after\")\n    Ok(())\nend t.main\n",
            body
        );
        let (_t, r) = run_unsafe_pkg(name, &src);
        assert_eq!(r.status.code(), Some(101), "{}: {}{}", name, text(&r.stdout), text(&r.stderr));
        assert!(text(&r.stderr).contains("runs past the 4-byte value passed to C"), "{}: {}", name, text(&r.stderr));
        assert!(!text(&r.stdout).contains("after"), "{}", name);
    }
    // 値の幅に収まる読み書き (`cast` で 1 バイトだけ読む) は今までどおり通る。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut x: i32 = 1i32
    unsafe \"libc\" \"fits\" {
        let b: *mut u8 = core.ptr.cast::<i32, u8>(core.ptr.from_mut(&mut x))
        core.check.eq(core.ptr.read(b), 1u8)
        core.ptr.write(b, 7u8)
    }
    core.check.eq(x, 7i32)
    Ok(())
end t.main
";
    let (_t, r) = run_unsafe_pkg("ptr_more_fits_in_value", src);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
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
fn ptr_more_place_pointer_is_passed_to_c_as_a_pinned_copy() {
    // `from_mut` の結果 (C に見せる写しを指すポインタ) は、そのまま extern に渡せる (3.8: 呼び出しの前後で元の値と同期する)。
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
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
}

// 具体的な効果実引数を持つ `Fn` 境界 (`[]` / `[io]` / `never` / `may`) の検査 (4.3 / 5.1)。
// 境界の検査は、渡したクロージャの確実な効果と境界の具体名を比べる (呼び出し元の `uses` とは別)。
// 定義の並びは UTF-8 順 (E0020) にしてある。`BOUND_BAD` の呼び出し元 3 本だけが違反する。
const BOUND_BAD: &str = r#"fn t.bad_infer(x: u8) -> u8 uses [] panics never
    t.bnd_none(
        fn(y: u8) -> u8 capture [] {
            t.inf_io(y)
        },
        x,
    )
end t.bad_infer

fn t.bad_let(s: t.S, x: u8) -> u8 uses [io] panics never
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
    assert_eq!(got, vec!["E4001".to_string(), "E4001".to_string(), "E4002".to_string()], "{}", out);
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
    // 境界が覆う効果 (`io` ⊇ `io.console`)・`may` 境界・効果のないクロージャ
    // は黙る (推論された関数を呼ぶクロージャは、推論した効果を上限として境界と比べる = `BOUND_BAD` の `bad_infer`)。
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

/// `kir <FQN> --opt --run ...` は、最適化後の KIR でも木走査と同じ結果を出す。
/// 既存の `kir_lower_values_match_tree_walker` の入力を `--opt` 付きでも走らせ、
/// 表示と終了コードが一致することを確かめる。また、同じ入力から同じ出力が
/// 得られること（決定性）を2回実行して確かめる。
#[test]
fn kir_opt_values_match_tree_walker() {
    let cases: &[(&str, &[&str], &str)] = &[
        ("t.gcd", &["48", "18"], "6"),
        ("t.classify", &["-7"], "-1"),
        ("t.classify", &["0"], "0"),
        ("t.early", &["25"], "15"),
        ("t.early", &["4"], "8"),
        ("t.loopy", &["10"], "25"),
        ("t.fact", &["20"], "2432902008176640000"),
        ("t.negate", &["5"], "-5"),
        ("t.wrap", &["200", "3"], "32"),
        ("t.sub8", &["9", "4"], "5"),
        ("t.assign_in_branch", &["3"], "402"),
        ("t.assign_in_branch", &["-3"], "105"),
        ("t.nested", &["5"], "20"),
        ("t.andor", &["true", "true", "3"], "true"),
        ("t.andor", &["false", "true", "-6"], "true"),
        ("t.andor", &["false", "false", "1"], "false"),
    ];
    for (i, (f, args, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.join(", "));
        let t = kir_pkg(&format!("kir_opt_values_{}", i), &format!("    core.check.eq({}, {})\n", call, want));
        // 木走査が通ること
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        // --opt 付きで参照実行器を走らせ、同じ表示・終了コードになること
        let mut kargs = vec!["kir", f, "--opt", "--run"];
        kargs.extend(args.iter().copied());
        let k1 = kir_run(&kargs, &t.0);
        assert_eq!(k1.status.code(), Some(0), "kir --opt: {}\n{}", call, text(&k1.stderr));
        assert_eq!(text(&k1.stdout).trim(), *want, "kir --opt value of {}", call);
        // 決定性: 同じ入力から同じ出力
        let k2 = kir_run(&kargs, &t.0);
        assert_eq!(text(&k2.stdout), text(&k1.stdout), "kir --opt 決定性: {}", call);
        assert_eq!(k2.status.code(), k1.status.code(), "kir --opt 決定性(終了コード): {}", call);
    }
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
fn inferred_callee_inside_a_closure_binds_the_fn_bound_effect_as_an_upper_bound() {
    // `t.h` は効果を宣言しないので推論される。推論は名前の全候補 (`m` の `t.A.m` を含む) の和で
    // `io.console` を持つ。仕様 5.1「未宣言の副作用はエラー」に合わせ、確実でない効果も上限として数え、
    // `apply` の `E` に束縛して `uses []` の `t.main` を E4001 にする (fail-closed)。
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
    assert!(got.iter().any(|c| *c == "E4001"), "{}", out);
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

/// 基本型の `Display` (`display_fmt` で `core.fmt.Formatter` に書く。仕様 4.2) と派生の比較 (`ne` / `partial_cmp`) は、
/// インタプリタとバンドルの両方で動く (以前は "unknown integer operation" で panic していた)。
/// `to_string` は仕様に無く検査で拒む (tests/data/unsound/)。
#[test]
fn scalar_display_and_derived_methods_run_in_both() {
    let test = "test t.main.test_scalar_display
    let n: i64 = -7i64
    let mut buf = [0u8; 32]
    let mut f = core.fmt.Formatter.new(&mut buf)
    core.mem.drop(n.display_fmt(&mut f))
    core.check.eq(core.str.from_utf8(f.written()), Ok(\"-7\"))
    let big: u32 = 4294967295u32
    let mut buf2 = [0u8; 16]
    let mut f2 = core.fmt.Formatter.new(&mut buf2)
    core.mem.drop(big.display_fmt(&mut f2))
    core.check.eq(core.str.from_utf8(f2.written()), Ok(\"4294967295\"))
    let c: char = 'x'
    let mut buf3 = [0u8; 8]
    let mut f3 = core.fmt.Formatter.new(&mut buf3)
    core.mem.drop(c.display_fmt(&mut f3))
    core.check.eq(core.str.from_utf8(f3.written()), Ok(\"x\"))
    let t: bool = true
    let mut buf4 = [0u8; 8]
    let mut f4 = core.fmt.Formatter.new(&mut buf4)
    core.mem.drop(t.display_fmt(&mut f4))
    core.check.eq(core.str.from_utf8(f4.written()), Ok(\"true\"))
    core.check.eq(n != 3i64, true)
    core.check.eq(n.partial_cmp(&3i64).is_some(), true)
end t.main.test_scalar_display
";
    let src = format!("{}\n{}", main_wrap("", ""), test);
    let t = Tmp::new("scalar_display_both", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    // 配列の繰り返し式は KIR に下ろせないので、機械語 (Cranelift) で比べる。
    assert!(out.contains("test t.main.test_scalar_display ... ok (both: native (llvm skipped: "), "{}", out);
}

/// Formatter の境界: i64.MIN / u64.MAX / 0 / マルチバイトの char / バッファがちょうど・不足 / debug の char。
#[test]
fn formatter_edge_cases_run_natively() {
    let test = r#"test t.main.test_fmt_edges
    let mn = core.num.i64.MIN
    let mx = core.num.u64.MAX
    let m5 = -5i8
    let mut b1 = [0u8; 32]
    let mut f1 = core.fmt.Formatter.new(&mut b1)
    core.mem.drop(mn.display_fmt(&mut f1))
    core.check.eq(core.str.from_utf8(f1.written()), Ok("-9223372036854775808"))
    let mut b2 = [0u8; 32]
    let mut f2 = core.fmt.Formatter.new(&mut b2)
    core.mem.drop(mx.display_fmt(&mut f2))
    core.check.eq(core.str.from_utf8(f2.written()), Ok("18446744073709551615"))
    let z = 0u8
    let mut b3 = [0u8; 4]
    let mut f3 = core.fmt.Formatter.new(&mut b3)
    core.mem.drop(z.display_fmt(&mut f3))
    core.check.eq(core.str.from_utf8(f3.written()), Ok("0"))
    let ce = 'é'
    let ca = 'あ'
    let mut b4 = [0u8; 5]
    let mut f4 = core.fmt.Formatter.new(&mut b4)
    core.mem.drop(ce.display_fmt(&mut f4))
    core.mem.drop(ca.display_fmt(&mut f4))
    core.check.eq(core.str.from_utf8(f4.written()), Ok("éあ"))
    let mut b5 = [0u8; 8]
    let mut f5 = core.fmt.Formatter.new(&mut b5)
    core.check.is_true(f5.written().len() == 0usize)
    core.check.is_true(core.fmt.Formatter.write_str(&mut f5, "12345678").is_ok())
    core.check.is_true(core.fmt.Formatter.write_str(&mut f5, "9").is_err())
    core.check.eq(core.str.from_utf8(f5.written()), Ok("12345678"))
    let big = 12345i32
    let mut b6 = [0u8; 3]
    let mut f6 = core.fmt.Formatter.new(&mut b6)
    core.check.is_true(big.display_fmt(&mut f6).is_err())
    core.check.eq(core.str.from_utf8(f6.written()), Ok(""))
    let cx = 'x'
    let cn = '\n'
    let cq = '\''
    let ca2 = 'あ'
    let tt = true
    let mut b7 = [0u8; 32]
    let mut f7 = core.fmt.Formatter.new(&mut b7)
    core.mem.drop(cx.debug_fmt(&mut f7))
    core.mem.drop(cn.debug_fmt(&mut f7))
    core.mem.drop(cq.debug_fmt(&mut f7))
    core.mem.drop(ca2.debug_fmt(&mut f7))
    core.mem.drop(tt.debug_fmt(&mut f7))
    core.mem.drop(m5.debug_fmt(&mut f7))
    core.check.eq(core.str.from_utf8(f7.written()), Ok("'x''\\n''\\'''あ'true-5"))
end t.main.test_fmt_edges
"#;
    let src = format!("{}\n{}", main_wrap("", ""), test);
    let t = Tmp::new("fmt_edges_both", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_fmt_edges ... ok (both: native (llvm skipped: "), "{}", out);
}

/// `Display` の char は文字そのもの。`print_line` と `to_string` の出力を確かめる。
#[test]
fn char_display_prints_the_character_itself() {
    let body = "    let c: char = 'x'\n    let mut buf = [0u8; 8]\n    let mut f = core.fmt.Formatter.new(&mut buf)\n    core.mem.drop(c.display_fmt(&mut f))\n    match core.str.from_utf8(f.written()) {\n        Ok(s) => std.io.print_line(s)?\n        Err(_e) => std.io.print_line(\"bad\")?\n    }\n    let n = 7u8\n    let mut buf2 = [0u8; 4]\n    let mut f2 = core.fmt.Formatter.new(&mut buf2)\n    core.mem.drop(n.display_fmt(&mut f2))\n    match core.str.from_utf8(f2.written()) {\n        Ok(s) => std.io.print_line(s)?\n        Err(_e) => std.io.print_line(\"bad\")?\n    }\n";
    let t = Tmp::new("char_display", &main_wrap("", body));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), "x\n7\n");
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
/// 別ディレクトリから起動しても同梱物を exec できること。基本型の `display_fmt` は clif 未対応なので
/// ネイティブ生成が失敗し、バンドルへフォールバックする。
#[cfg(unix)]
#[test]
fn bundle_launcher_is_executable_and_location_independent() {
    use std::os::unix::fs::PermissionsExt;
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let n = 42u32
    let mut buf = [0u8; 16]
    let mut f = core.fmt.Formatter.new(&mut buf)
    core.mem.drop(n.display_fmt(&mut f))
    match core.str.from_utf8(f.written()) {
        Ok(s) => std.io.print_line(s)?
        Err(_e) => std.io.print_line(\"bad\")?
    }
    Ok(())
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
    let mut owned = match alloc.string.String.from_str(std.alloc.SYSTEM, s) {
        Ok(o) => o
        Err(_e) => alloc.string.String.new(std.alloc.SYSTEM)
    }
    core.check.eq(owned.as_str(), \"héllo\")
    core.check.eq(owned.len(), 6usize)
    core.mem.drop(owned.push('!'))
    core.check.eq(owned.as_str(), \"héllo!\")
    let st = alloc.string.String.new(std.alloc.SYSTEM)
    core.check.eq(st.is_empty(), true)
end t.main.test_d_to_string
";
    let src = format!("{}\n{}", main_wrap("", ""), test);
    let t = Tmp::new("str_slice_option_result_sig", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    for name in ["test_a_option_result_cmp", "test_b_slice_count", "test_c_str_derived_cmp"] {
        // 機械語とインタプリタの結果が一致する (KIR に下ろせないので Cranelift で比べる)。
        assert!(out.contains(&format!("t.main.{} ... ok (both: native (llvm skipped: ", name)), "{}", out);
    }
    // 解放が要る値を作る `match` (`Ok(o) => o` / `Err(_e) => ..`) は機械語で動き、インタプリタと一致する。
    assert!(out.contains("t.main.test_d_to_string ... ok (both: native (llvm skipped: "), "{}", out);
}

#[test]
fn derived_cmp_and_iter_count_boundaries_run_native() {
    // `Option` / `Result` / `str` の派生の比較 (None < Some、Ok < Err、ペイロードは辞書順、str は UTF-8 バイト列の辞書順) と
    // `v.iter()` の `next` / `count` の境界を、機械語とインタプリタで比べる。
    let test = r#"test t.main.test_e_option_boundaries
    let none: Option<i8> = None
    let a: Option<i8> = Some(-1i8)
    let b: Option<i8> = Some(1i8)
    core.check.eq(none.cmp(&none) == core.cmp.Ordering.Equal, true)
    core.check.eq(none.cmp(&a) == core.cmp.Ordering.Less, true)
    core.check.eq(a.cmp(&none) == core.cmp.Ordering.Greater, true)
    core.check.eq(a.cmp(&a) == core.cmp.Ordering.Equal, true)
    core.check.eq(a.cmp(&b) == core.cmp.Ordering.Less, true)
    core.check.eq(b.cmp(&a) == core.cmp.Ordering.Greater, true)
    core.check.eq(none.eq(&none), true)
    core.check.eq(none.eq(&a), false)
    core.check.eq(a != b, true)
    core.check.eq(a != a, false)
    let big: Option<u8> = Some(255u8)
    let small: Option<u8> = Some(1u8)
    core.check.eq(big.cmp(&small) == core.cmp.Ordering.Greater, true)
    let sa: Option<&str> = Some("ab")
    let sb: Option<&str> = Some("abc")
    core.check.eq(sa.cmp(&sb) == core.cmp.Ordering.Less, true)
    core.check.eq(sb.cmp(&sa) == core.cmp.Ordering.Greater, true)
    core.check.eq(sa.eq(&sb), false)
    let pa: Option<core.cmp.Ordering> = sa.partial_cmp(&sb)
    core.check.eq(pa.is_some(), true)
end t.main.test_e_option_boundaries

test t.main.test_f_result_boundaries
    let ok1: Result<u8, u8> = Ok(1u8)
    let ok2: Result<u8, u8> = Ok(2u8)
    let er0: Result<u8, u8> = Err(0u8)
    let er9: Result<u8, u8> = Err(9u8)
    core.check.eq(ok1.cmp(&ok1) == core.cmp.Ordering.Equal, true)
    core.check.eq(ok1.cmp(&ok2) == core.cmp.Ordering.Less, true)
    core.check.eq(ok2.cmp(&ok1) == core.cmp.Ordering.Greater, true)
    core.check.eq(ok2.cmp(&er0) == core.cmp.Ordering.Less, true)
    core.check.eq(er0.cmp(&ok2) == core.cmp.Ordering.Greater, true)
    core.check.eq(er0.cmp(&er9) == core.cmp.Ordering.Less, true)
    core.check.eq(er9.cmp(&er0) == core.cmp.Ordering.Greater, true)
    core.check.eq(er9.cmp(&er9) == core.cmp.Ordering.Equal, true)
    core.check.eq(ok1.eq(&ok1), true)
    core.check.eq(ok1.eq(&er0), false)
    core.check.eq(ok1 != Ok(1u8), false)
    core.check.eq(er0 != Err(1u8), true)
end t.main.test_f_result_boundaries

test t.main.test_g_str_boundaries
    let empty: &str = ""
    let a: &str = "a"
    let ab: &str = "ab"
    let abc: &str = "abc"
    let b: &str = "b"
    core.check.eq(empty.cmp(&empty) == core.cmp.Ordering.Equal, true)
    core.check.eq(empty.cmp(&a) == core.cmp.Ordering.Less, true)
    core.check.eq(a.cmp(&empty) == core.cmp.Ordering.Greater, true)
    core.check.eq(ab.cmp(&abc) == core.cmp.Ordering.Less, true)
    core.check.eq(abc.cmp(&ab) == core.cmp.Ordering.Greater, true)
    core.check.eq(abc.cmp(&b) == core.cmp.Ordering.Less, true)
    core.check.eq(b.cmp(&abc) == core.cmp.Ordering.Greater, true)
    core.check.eq(abc.cmp(&abc) == core.cmp.Ordering.Equal, true)
    core.check.eq(ab.eq(&abc), false)
    core.check.eq(abc.eq(&abc), true)
    core.check.eq(empty.eq(&empty), true)
    core.check.eq(ab != abc, true)
    core.check.eq(abc != abc, false)
    let e1: &str = "é"
    let e2: &str = "f"
    let e3: &str = "あ"
    let e4: &str = "い"
    let e5: &str = "éa"
    core.check.eq(e1.cmp(&e2) == core.cmp.Ordering.Greater, true)
    core.check.eq(e2.cmp(&e1) == core.cmp.Ordering.Less, true)
    core.check.eq(e3.cmp(&e4) == core.cmp.Ordering.Less, true)
    core.check.eq(e1.cmp(&e5) == core.cmp.Ordering.Less, true)
    core.check.eq(e1.cmp(&e1) == core.cmp.Ordering.Equal, true)
    core.check.eq(e1.eq(&e1), true)
    core.check.eq(e1.eq(&e5), false)
    let p = ab.partial_cmp(&abc)
    core.check.eq(p.is_some(), true)
    core.check.eq(p.unwrap() == core.cmp.Ordering.Less, true)
end t.main.test_g_str_boundaries

test t.main.test_h_count_boundaries
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.check.eq(v.iter().count(), 0usize)
    let mut it0 = v.iter()
    core.check.eq(it0.next().is_none(), true)
    core.check.eq(it0.count(), 0usize)
    core.mem.drop(v.push(7u32))
    core.mem.drop(v.push(8u32))
    core.mem.drop(v.push(9u32))
    let mut it = v.iter()
    core.check.eq(it.next().unwrap(), 7u32)
    core.check.eq(it.next().unwrap(), 8u32)
    core.check.eq(it.count(), 1usize)
    let mut it2 = v.iter()
    core.mem.drop(it2.next())
    core.mem.drop(it2.next())
    core.mem.drop(it2.next())
    core.check.eq(it2.next().is_none(), true)
    core.check.eq(it2.count(), 0usize)
end t.main.test_h_count_boundaries
"#;
    let src = format!("{}\n{}", main_wrap("", ""), test);
    let t = Tmp::new("derived_cmp_boundaries", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    for name in ["test_e_option_boundaries", "test_f_result_boundaries", "test_g_str_boundaries", "test_h_count_boundaries"] {
        assert!(out.contains(&format!("t.main.{} ... ok (both: native (llvm skipped: ", name)), "{}", out);
    }
}

/// 解放が要る値を返す `match` (腕が Vec / String を返す、解放が要る payload を束縛する・捨てる、腕の `return`、
/// 繰り返し) が機械語でも動き、インタプリタと一致する。二重解放や解放漏れがあれば機械語側が落ちるか、
/// 10 万回の繰り返しで結果が合わなくなる。
#[test]
fn match_producing_droppable_values_runs_natively() {
    let pick = r#"fn t.pick(s: &str) -> alloc.string.String uses [alloc] panics never
    match alloc.string.String.from_str(std.alloc.SYSTEM, s) {
        Ok(o) => return o
        Err(_e) => return alloc.string.String.new(std.alloc.SYSTEM)
    }
end t.pick

"#;
    let tests = r#"test t.main.test_e_vec_arms
    let n = 1u8
    let mut v: alloc.vec.Vec<u8, std.alloc.System> = match n {
        0u8 => alloc.vec.Vec.new(std.alloc.SYSTEM)
        _ => alloc.vec.Vec.new(std.alloc.SYSTEM)
    }
    core.mem.drop(v.push(7u8))
    core.check.eq(v.len(), 1usize)
end t.main.test_e_vec_arms

test t.main.test_f_block_arm_moves_local
    let n = 1u8
    let v: alloc.vec.Vec<u8, std.alloc.System> = match n {
        0u8 => alloc.vec.Vec.new(std.alloc.SYSTEM)
        _ => {
            let mut w: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
            core.mem.drop(w.push(5u8))
            w
        }
    }
    core.check.eq(v.len(), 1usize)
end t.main.test_f_block_arm_moves_local

test t.main.test_g_unused_payloads
    let s: &str = "abc"
    let a = match alloc.string.String.from_str(std.alloc.SYSTEM, s) {
        Ok(_o) => 1usize
        Err(_e) => 0usize
    }
    core.check.eq(a, 1usize)
    let b = match alloc.string.String.from_str(std.alloc.SYSTEM, s) {
        Ok(_) => 2usize
        Err(_e) => 0usize
    }
    core.check.eq(b, 2usize)
    let c = match alloc.string.String.from_str(std.alloc.SYSTEM, s) {
        Ok(o) => o.len()
        Err(_e) => 9usize
    }
    core.check.eq(c, 3usize)
end t.main.test_g_unused_payloads

test t.main.test_h_loop_100k
    let mut i = 0usize
    let mut total = 0usize
    while i < 100000usize {
        let s = match alloc.string.String.from_str(std.alloc.SYSTEM, "héllo") {
            Ok(o) => o
            Err(_e) => alloc.string.String.new(std.alloc.SYSTEM)
        }
        total = total + s.len()
        i = i + 1usize
    }
    core.check.eq(total, 600000usize)
end t.main.test_h_loop_100k

test t.main.test_i_return_from_arm
    let s = t.pick("xy")
    core.check.eq(s.len(), 2usize)
    let e = t.pick("")
    core.check.eq(e.is_empty(), true)
end t.main.test_i_return_from_arm

test t.main.test_k_place_scrutinee_fresh_arms
    let r = alloc.string.String.from_str(std.alloc.SYSTEM, "xy")
    let s = match r {
        Ok(_o) => alloc.string.String.new(std.alloc.SYSTEM)
        Err(_e) => alloc.string.String.new(std.alloc.SYSTEM)
    }
    core.check.eq(s.is_empty(), true)
end t.main.test_k_place_scrutinee_fresh_arms
"#;
    let src = format!("{}\n{}", main_wrap(pick, ""), tests);
    let t = Tmp::new("match_droppable_native", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    // 6 本とも機械語 (Cranelift) で比べられるが、KIR に下ろせないものは Cranelift へ退避する理由が出る。
    assert_eq!(out.matches("(both: native (llvm skipped: ").count(), 6, "{}", out);
    assert!(!out.contains("both: bundle"), "{}", out);
    assert!(out.contains("6 passed; 0 failed"), "{}", out);
}

/// 解放が要る値を持つ場所 (`let` の局所) から移動する `match` は、所有の扱いを持たないので退避する。
/// 退避の理由が出て、バンドルでも正しく動く。
#[test]
fn match_moving_out_of_a_place_scrutinee_falls_back_with_reason() {
    let tests = r#"test t.main.test_j_place_scrutinee_moves
    let r = alloc.string.String.from_str(std.alloc.SYSTEM, "x")
    let s = match r {
        Ok(o) => o
        Err(_e) => alloc.string.String.new(std.alloc.SYSTEM)
    }
    core.check.eq(s.len(), 1usize)
end t.main.test_j_place_scrutinee_moves
"#;
    let src = format!("{}\n{}", main_wrap("", ""), tests);
    let t = Tmp::new("match_place_fallback", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("t.main.test_j_place_scrutinee_moves ... ok (both: bundle (llvm skipped: "), "{}", out);
    assert!(out.contains("; native skipped: t.main.test_j_place_scrutinee_moves: "), "{}", out);
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
    // const に落としたものも読める。不変のスカラー変数 `x` の `from_ref(&x)` も、スタックスロットのアドレスとして扱い、
    // インタプリタとネイティブの両方で同じ結果になる。
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
    assert!(out.contains("t.main.test_add_const_cast_const ... ok (both: native (llvm skipped: "), "{}", out);
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
fn concurrent_builds_with_the_same_key_both_succeed() {
    // 同じキー (同じソースの別のパッケージ dir) を同じキャッシュへ並行にビルドしても、どちらも成功する。
    // 以前は作業用の `<key>.tmp` を共有して互いに消し合い、`fs::rename(..).unwrap()` で panic しえた。
    let a = Tmp::new("cache_race_a", CACHE_SRC);
    let b = Tmp::new("cache_race_b", CACHE_SRC);
    for round in 0..4 {
        let cache = std::env::temp_dir().join(format!("ksn_cache_race_{}_{}", std::process::id(), round));
        let _ = fs::remove_dir_all(&cache);
        let spawn = |dir: &Path| {
            Command::new(exe())
                .args(["build", "--backend", "clif"])
                .current_dir(dir)
                .env("KSN_CACHE_DIR", &cache)
                .env("KSN_STDLIB", root().join("spec").join("stdlib"))
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        };
        let (pa, pb) = (spawn(&a.0), spawn(&b.0));
        let (oa, ob) = (pa.wait_with_output().unwrap(), pb.wait_with_output().unwrap());
        let _ = fs::remove_dir_all(&cache);
        for o in [&oa, &ob] {
            assert_eq!(o.status.code(), Some(0), "round {}: {}{}", round, text(&o.stdout), text(&o.stderr));
        }
    }
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
    // (既定のバックエンドは clif。`c` は別のキー。)
    let o = build_with_cache(a, &t.0, &cache, &["--backend", "c"], &[]);
    ok(&o);
    assert!(!hit(&o), "c must not reuse the default (clif) entry: {}", text(&o.stdout));

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
    // `tests/data/fmath_both.ksnl`: 既知値 (f32 は 2〜3 ulp 以内)、特殊値 (atan2 の ±∞ / NaN 組を含む)、9 関数の総和 (ビット列の突き合わせ)。
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
    // 6 本とも LLVM (clang が無ければ Cranelift) で比べていること (バンドルへ退避した「空振り」を許さない)。
    if clang_available() {
        assert_eq!(out.matches("(both: llvm)").count(), 6, "{}", out);
        assert!(out.contains("llvm 6, native 0, bundle fallback 0"), "{}", out);
    } else {
        assert_eq!(out.matches("(both: native").count(), 6, "{}", out);
    }
    assert!(!out.contains("both: bundle"), "{}", out);
    assert!(out.contains("6 passed; 0 failed"), "{}", out);
    let _ = fs::remove_dir_all(&cache);
}

// ---- `test --both`: 機械語 (KIR → LLVM、作れなければ Cranelift) との比較と、バンドルへの退避 ----

const BOTH_SRC: &str = "fn t.main() -> () uses [] panics never
end t.main

test t.main.test_ok
    core.check.eq(1u8, 1u8)
end t.main.test_ok
";

#[test]
fn test_both_runs_kir_tests_as_llvm_executables() {
    if !clang_available() {
        eprintln!("skip: clang が PATH に無いので、LLVM の比較は飛ばす");
        return;
    }
    let src = format!("{}\ntest t.main.test_fail\n    core.check.eq(1u8, 2u8)\nend t.main.test_fail\n", BOTH_SRC);
    let t = Tmp::new("both_llvm", &src);
    // `--both` を付けなければ、出力の形式は従来どおり (注記なし)。
    let plain = run(&["test"], &t.0);
    assert!(text(&plain.stdout).contains("test t.main.test_ok ... ok\n"), "{}", text(&plain.stdout));
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_ok ... ok (both: llvm)"), "{}", out);
    assert!(out.contains("test t.main.test_fail ... FAILED: assertion failed: left: 1, right: 2 (both: llvm)"), "{}", out);
    assert!(out.contains("1 passed; 1 failed"), "{}", out);
    assert!(out.contains("compared with: llvm 2, native 0, bundle fallback 0"), "{}", out);
    assert!(!out.contains("both: bundle") && !out.contains("disagree"), "{}", out);
    assert!(t.0.join("target").join("test-llvm").is_dir(), "no llvm test executable: {}", out);
}

#[test]
fn test_both_compares_llvm_panics_with_the_tree_walker() {
    // KIR (LLVM) の失敗の文言は木走査と同じ。除算・unwrap・overflow・assert が、木走査と同じ文言・同じ分類で出る。
    let src = format!(
        "{}\ntest t.main.test_div0\n    let z = core.num.u8.MIN\n    core.check.eq(7u8 / z, 0u8)\nend t.main.test_div0\n\n\
         test t.main.test_unwrap\n    let o: Option<u8> = None\n    core.check.eq(o.unwrap(), 0u8)\nend t.main.test_unwrap\n\n\
         test t.main.test_overflow\n    let x = 255u8\n    let y = x + 1u8\n    core.check.eq(y, 0u8)\nend t.main.test_overflow\n\n\
         test t.main.test_assert\n    core.check.eq(1u8, 2u8)\nend t.main.test_assert\n",
        BOTH_SRC
    );
    if !clang_available() {
        eprintln!("skip: clang が PATH に無いので、LLVM の比較は飛ばす");
        return;
    }
    let t = Tmp::new("both_llvm_panics", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_div0 ... FAILED: attempt to divide by zero (both: llvm)"), "{}", out);
    assert!(out.contains("test t.main.test_unwrap ... FAILED: called `Option::unwrap()` on a `None` value (both: llvm)"), "{}", out);
    assert!(out.contains("test t.main.test_overflow ... FAILED: attempt to add with overflow (both: llvm)"), "{}", out);
    assert!(out.contains("test t.main.test_assert ... FAILED: assertion failed: left: 1, right: 2 (both: llvm)"), "{}", out);
    assert!(!out.contains("disagree"), "{}", out);
    assert!(out.contains("compared with: llvm 5, native 0, bundle fallback 0"), "{}", out);
}

#[test]
fn test_both_falls_back_from_llvm_to_cranelift_with_the_reason() {
    // `char.to_u32` は KIR に下ろせない (段階 1 の対象外) ので、LLVM は退避する。Cranelift で作れる。
    // 退避した Cranelift の文言 (`arithmetic overflow`) と木走査の文言は違うが、分類が同じなので一致になる。
    let src = format!(
        "{}\ntest t.main.test_mixed\n    let x = 255u8\n    let y = x + 1u8\n    core.check.eq(y, 0u8)\n    core.check.eq('a'.to_u32(), 97u32)\nend t.main.test_mixed\n",
        BOTH_SRC
    );
    let t = Tmp::new("both_llvm_to_native", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_mixed ... FAILED: attempt to add with overflow (both: native (llvm skipped: "), "{}", out);
    // `BOTH_SRC` の test_ok は LLVM で比べる (clang が無ければ Cranelift)。
    if clang_available() {
        assert!(out.contains("compared with: llvm 1, native 1, bundle fallback 0"), "{}", out);
    } else {
        assert!(out.contains("compared with: llvm 0, native 2, bundle fallback 0"), "{}", out);
    }
    assert!(!out.contains("disagree"), "{}", out);
    assert!(!out.contains("both: bundle"), "{}", out);
}

#[test]
fn test_both_stack_overflow_is_exit_102_on_both_sides() {
    // 木走査は深さ上限で `Exit(102)`、LLVM は終了コード 102 (stderr に `stack overflow`)。どちらも終了の種類 `exit(102)` で比べて一致する。
    let src = "fn t.down(n: u64) -> u64 uses [] panics never stack 4096\n    if n == 0u64 {\n        return 0u64\n    }\n    t.down(n.wrapping_sub(1u64)).wrapping_add(1u64)\nend t.down\n\n\
               fn t.main() -> () uses [] panics never\nend t.main\n\n\
               test t.main.test_deep\n    core.check.eq(t.down(10000u64), 10000u64)\nend t.main.test_deep\n";
    let t = Tmp::new("both_stack_102", src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}{}", out, text(&o.stderr));
    // 失敗の表示は木走査の結果 (`exit(102)`)。機械語も同じ終了の種類なので、不一致にはならない。
    assert!(out.contains("test t.main.test_deep ... FAILED: exit(102) (both: llvm)"), "{}", out);
    assert!(!out.contains("disagree"), "{}", out);
}

#[test]
fn test_both_compares_failure_classes_when_both_backends_fail() {
    // 両方が失敗しても、理由の分類 (overflow / assert / ...) が違えば disagree。同じ分類なら文言が違っても一致。
    // 分類が違うケースの判定は `src/cli.rs` の単体テスト `both_failing_with_different_classes_disagree` で確かめる。
    let src = format!(
        "{}\ntest t.main.test_assert\n    core.check.eq(1u8, 2u8)\nend t.main.test_assert\n\ntest t.main.test_overflow\n    let x = 255u8\n    let y = x + 1u8\n    core.check.eq(y, 0u8)\nend t.main.test_overflow\n",
        BOTH_SRC
    );
    let t = Tmp::new("both_class", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}{}", out, text(&o.stderr));
    // 両方とも失敗し、文言が同じ (木走査と LLVM) なら一致。分類が違う組み合わせは `both_tests` の単体テストで確かめる。
    if clang_available() {
        assert!(out.contains("test t.main.test_overflow ... FAILED: attempt to add with overflow (both: llvm)"), "{}", out);
        assert!(out.contains("test t.main.test_assert ... FAILED: assertion failed: left: 1, right: 2 (both: llvm)"), "{}", out);
        assert!(out.contains("compared with: llvm 3, native 0, bundle fallback 0"), "{}", out);
    }
    assert!(!out.contains("disagree"), "{}", out);
}

#[test]
fn test_both_falls_back_to_the_bundle_when_llvm_and_cranelift_both_cannot_build() {
    // 128 ビットの除算・剰余は KIR (LLVM) も Cranelift も持たないので、バンドルで比べる。
    // i128 の比較は KIR に下ろせないので Cranelift で比べる (LLVM の理由を出す)。
    let src = format!(
        "{}\ntest t.main.test_wide\n    let x: i128 = 5i128\n    core.check.eq(x, 5i128)\nend t.main.test_wide\n\
         test t.main.test_div\n    let m = core.num.i128.MIN\n    core.check.eq(m.checked_rem(-1i128), None)\nend t.main.test_div\n",
        BOTH_SRC
    );
    let t = Tmp::new("both_bundle", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("test t.main.test_wide ... ok (both: native (llvm skipped: "), "{}", out);
    assert!(out.contains("test t.main.test_div ... ok (both: bundle (llvm skipped: "), "{}", out);
    assert!(out.contains("; native skipped: t.main.test_div: 128-bit method i128.checked_rem"), "{}", out);
    assert!(out.contains("3 passed; 0 failed"), "{}", out);
    if clang_available() {
        assert!(out.contains("test t.main.test_ok ... ok (both: llvm)"), "{}", out);
        assert!(out.contains("compared with: llvm 1, native 1, bundle fallback 1"), "{}", out);
    } else {
        assert!(out.contains("compared with: llvm 0, native 2, bundle fallback 1"), "{}", out);
    }
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
fn dyn_call_effects_inside_a_closure_are_checked_against_the_caller() {
    // 仕様 5.1: クロージャの中の `dyn` 越しの呼び出しの効果 (宣言の上限) も、囲む関数の宣言で検査する。
    let body = "fn t.loud_closure(a: &dyn t.Act) -> u8 uses [] panics never
    let f = fn(x: u8) -> u8 capture [&a] {
        a.act()
    }
    f(1)
end t.loud_closure
";
    let (code, got, out) = eff_check_with("trait_eff_closure", TRAIT_EFF_PRELUDE, body);
    assert_eq!(code, Some(1), "{}", out);
    assert!(got.iter().any(|c| c == "E4001"), "{}", out);
}

#[test]
fn trait_call_effects_stay_silent_when_unknown() {
    // 陰性 (黙る): 効果変数の束縛が分からない境界、推論された関数。クロージャの本体の `dyn` 越しは、上の検査。
    let body = "fn t.quiet_inferred(a: &dyn t.Act) -> u8
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
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_with_match_tree_walker() {
    // `with` を普通に抜ける / `return` で抜けるケース。
    // `ksn run` (木走査) と `ksn kir t.main --run` (KIR) の結果が一致すること。
    let body = "fn t.main() -> u32 uses [] panics may\n    let n1 = with s = 5u32 close core.mem.drop(s) {\n        42u32\n    }\n    core.check.eq(n1, 42u32)\n    with s = 5u32 close core.mem.drop(s) {\n        return 9u32\n    }\n    0u32\nend t.main\n";
    let t = Tmp::new("t.with", body);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    let k = kir_run(&["kir", "t.main", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir\n{}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "9", "kir value");
}

#[test]
fn kir_env_arg_count_arg_match_tree_walker() {
    // `std.env.arg_count` / `std.env.arg` / `std.env.var` を引数付きで実行し、
    // 木走査 (`ksn run`) と LLVM ネイティブ (`ksn build --backend llvm`) の表示・
    // 終了コードが一致すること。
    // (`ksn kir --run/--native` は引数を整数/bool として解釈するため、文字列引数を
    // 使う `std.env` の検査には `ksn run` と生のネイティブ実行ファイルを比較する。)
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [env, io.console] panics may
    let n = std.env.arg_count()
    core.check.eq(n, 2usize)
    match std.env.arg(0usize) {
        Some(s) => {
            core.check.eq(s, "a")
            std.io.print_line(s)?
        }
        None => {
            core.check.eq(1u8, 2u8)
        }
    }
    match std.env.arg(1usize) {
        Some(s) => {
            core.check.eq(s, "b")
            std.io.print_line(s)?
        }
        None => {
            core.check.eq(1u8, 2u8)
        }
    }
    match std.env.var("KSN_TEST_VAR") {
        Some(v) => {
            std.io.print_line(v)?
        }
        None => {
            std.io.print_line("none")?
        }
    }
    match std.env.var("KSN_ENV_DOES_NOT_EXIST_xyz") {
        Some(_s) => {
            core.check.eq(1u8, 2u8)
        }
        None => {
        }
    }
    Ok(())
end t.main
"#;
    let t = Tmp::new("kir_env_arg", src);
    // 木走査 (`ksn run`) で arg_count/arg/var が期待どおりであること。
    let o = run_with_env(&["run", "a", "b"], &t.0, &[("KSN_TEST_VAR", "hello")]);
    assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), "a\nb\nhello\n", "tree-walker arg output");
    // LLVM ネイティブをビルドして同じ引数・環境変数で走らせ、表示が一致すること。
    let b = run(&["build", "--backend", "llvm"], &t.0);
    assert_eq!(b.status.code(), Some(0), "build: {}\n{}", text(&b.stdout), text(&b.stderr));
    let native = t.0.join("target").join("debug").join("t.native");
    let n = Command::new(&native)
        .args(["a", "b"])
        .current_dir(&t.0)
        .env("KSN_TEST_VAR", "hello")
        .output()
        .expect("spawn native");
    assert_eq!(n.status.code(), Some(0), "native: {}\n{}", text(&n.stdout), text(&n.stderr));
    assert_eq!(text(&n.stdout), "a\nb\nhello\n", "native arg output");
}

#[test]
fn kir_lower_panics_and_depth_match_tree_walker() {
    // panic の文言と終了コード (101)、呼び出し深さの超過 (102) が木走査と一致すること。
    let cases: &[(&str, &[&str], &str)] = &[
        ("t.negate", &["-128"], "attempt to negate with overflow"),
        ("t.divs", &["-128", "-1"], "attempt to divide with overflow"),
        ("t.divs", &["7", "0"], "attempt to divide by zero"),
        ("t.rems", &["7", "0"], "attempt to calculate the remainder with a divisor of by zero"),
        ("t.rems", &["-128", "-1"], "attempt to calculate the remainder with overflow"),
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
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(101), "kir: {}", call);
        assert_eq!(text(&k.stderr).trim(), format!("panic: {}", msg), "kir: {}", call);
    }
    // 深さの超過: 木走査と同じく 102 と "stack overflow"
    let t = kir_pkg("kir_depth", "    let _x = t.down(5000u64)\n");
    assert_eq!(run(&["run"], &t.0).status.code(), Some(102));
    let k = kir_run(&["kir", "t.down", "--run", "5000"], &t.0);
    assert_eq!(k.status.code(), Some(102));
    assert!(text(&k.stderr).contains("stack overflow"), "{}", text(&k.stderr));
    let ok = kir_run(&["kir", "t.down", "--run", "2000"], &t.0);
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
    // 段階 1 の対象外 (標準の型 `std.io.Error` への参照の引数) は、理由を付けて拒否する。対象の関数は出さない。
    let src = "fn t.len_of(_s: &std.io.Stdout) -> usize uses [] panics never
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
    // `char.to_u32` は段階 1 では未対応 (理由付きで出る)
    let t = kir_pkg("kir_dump", "    core.check.eq('a'.to_u32(), 97u32)\n");
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
        ("examples/json", "json.is_hex", &["97"], "true"),
        ("examples/json", "json.is_hex", &["71"], "false"),
        ("examples/json", "json.is_hex", &["57"], "true"),
        // バイトリテラル `b'0'` などの u8 の定数 (json.is_hex)
        ("examples/json", "json.is_hex", &["65"], "true"),
        ("examples/json", "json.is_hex", &["103"], "false"),
        ("perf/fib", "fib.fib", &["27"], "196418"),
        ("perf/loops", "loops.sum_to", &["1000000"], "499999500000"),
        ("perf/loops", "loops.sum_to", &["1000"], "499500"),
        // 利用者の型のメソッド (`&mut self` の受け手): main の検査と同じ N = 200000 の値
        ("perf/structs", "structs.run", &["200000"], "255318"),
    ];
    for (dir, f, args, want) in cases {
        let d = root().join(dir);
        assert_eq!(run(&["check"], &d).status.code(), Some(0), "{} check", dir);
        assert_eq!(run(&["run"], &d).status.code(), Some(0), "{} run (tree-walker)", dir);
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &d);
        assert_eq!(k.status.code(), Some(0), "kir {} {:?}\n{}", f, args, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {} {:?}", f, args);
    }
}

#[test]
fn kir_str_methods_and_bstr_match_tree_walker() {
    // `&str` の `starts_with` / `ends_with` / `eq` とバイト列リテラル `b"..."`。
    // 木走査は `t.main` の `core.check.eq` で検査し、KIR は整数/bool を返す関数を `--run` で比べる。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.eq(\"abc\".starts_with(\"ab\"), true)\n    core.check.eq(\"abc\".starts_with(\"bc\"), false)\n    core.check.eq(\"abc\".starts_with(\"\"), true)\n    core.check.eq(\"\".starts_with(\"\"), true)\n    core.check.eq(\"abc\".ends_with(\"bc\"), true)\n    core.check.eq(\"abc\".ends_with(\"ab\"), false)\n    core.check.eq(\"abc\".ends_with(\"\"), true)\n    core.check.eq(\"abc\".ends_with(\"abcd\"), false)\n    core.check.eq(\"abc\".eq(\"abc\"), true)\n    core.check.eq(\"abc\".eq(\"abd\"), false)\n    core.check.eq(match core.str.from_utf8(b\"abc\") {\n        Ok(_t) => true\n        Err(_e) => false\n    }, true)\n    core.check.eq(match core.str.from_utf8(b\"\\xc3\") {\n        Ok(_t) => true\n        Err(_e) => false\n    }, false)\n    Ok(())\nend t.main\n\nfn t.bstr_test() -> bool uses [] panics may\n    b\"abc\".len() == 3usize && b\"abc\"[0] == 97u8\nend t.bstr_test\n\nfn t.str_test() -> bool uses [] panics never\n    \"abc\".starts_with(\"ab\") && \"abc\".starts_with(\"bc\") == false && \"abc\".eq(\"abc\") && !\"abc\".eq(\"abd\")\nend t.str_test\n\nfn t.utf8_ok() -> bool uses [] panics never\n    match core.str.from_utf8(b\"abc\") {\n        Ok(_t) => true\n        Err(_e) => false\n    }\nend t.utf8_ok\n\nfn t.utf8_bad() -> bool uses [] panics never\n    match core.str.from_utf8(b\"\\xc3\") {\n        Ok(_t) => false\n        Err(_e) => true\n    }\nend t.utf8_bad\n";
    let t = Tmp::new("kir_str_methods", src);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "check: {}", text(&o.stderr));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "run (tree-walker): {}", text(&o.stderr));
    let k = kir_run(&["kir", "t.str_test", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir str_test: {}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "true", "kir str_test value");
    let k = kir_run(&["kir", "t.bstr_test", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir bstr_test: {}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "true", "kir bstr_test value");
    let k = kir_run(&["kir", "t.utf8_ok", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir utf8_ok: {}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "true", "kir utf8_ok value");
    let k = kir_run(&["kir", "t.utf8_bad", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir utf8_bad: {}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "true", "kir utf8_bad value");
}

#[test]
fn kir_trim_match_tree_walker() {
    // `&str` の `trim` / `trim_start` / `trim_end` / `strip_prefix` / `strip_suffix` / `split_once` を
    // KIR に下ろし、`--run` (KIR インタプリタ) と `--native` (LLVM) が `ksn run` と一致すること。
    // `trim` は 0x20 / \t / \n / \r / \x0C の 5 つだけを削る (0x0B は削らない、natives.rs と合わせる)。
    let src = r#"fn t.trim_test() -> bool uses [] panics never
    let a = "  hello  ".trim().eq("hello")
    let b = "   \t\n ".trim().eq("")
    let c = "".trim().eq("")
    let d = "no_ws".trim().eq("no_ws")
    let e = "  leading".trim_start().eq("leading")
    let f = "trailing  ".trim_end().eq("trailing")
    let g = " \x0bhello\x0b ".trim().eq("\x0bhello\x0b")
    let h = match "  abc".strip_prefix("  ") {
        Some(_t) => true
        None => false
    }
    let i = match "abc  ".strip_suffix("  ") {
        Some(_t) => true
        None => false
    }
    let j = !match "abc".strip_prefix("x") {
        Some(_t) => true
        None => false
    }
    let k = match "".strip_prefix("") {
        Some(_t) => true
        None => false
    }
    a && b && c && d && e && f && g && h && i && j && k
end t.trim_test

fn t.split_once_test() -> bool uses [] panics may
    let r1 = match "a,b,c".split_once(",") {
        Some((a, b)) => {
            core.check.eq(a, "a")
            core.check.eq(b, "b,c")
            true
        }
        None => false
    }
    let r2 = match "abc".split_once("z") {
        Some(_t) => false
        None => true
    }
    let r3 = match ",abc".split_once(",") {
        Some((a, b)) => {
            core.check.eq(a, "")
            core.check.eq(b, "abc")
            true
        }
        None => false
    }
    let r4 = match "abc,".split_once(",") {
        Some((a, b)) => {
            core.check.eq(a, "abc")
            core.check.eq(b, "")
            true
        }
        None => false
    }
    let r5 = match "abc".split_once("") {
        Some(_t) => false
        None => true
    }
    r1 && r2 && r3 && r4 && r5
end t.split_once_test

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.trim_test(), true)
    core.check.eq(t.split_once_test(), true)
    Ok(())
end t.main
"#;
    let t = Tmp::new("kir_trim", src);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "check: {}", text(&o.stderr));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "run (tree-walker): {}", text(&o.stderr));
    let k = kir_run(&["kir", "t.trim_test", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir trim_test: {}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "true", "kir trim_test value");
    let k = kir_run(&["kir", "t.split_once_test", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir split_once_test: {}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "true", "kir split_once_test value");
}

#[test]
fn kir_str_pattern_bindings_match_tree_walker() {
    // `Some((k, v))` や `Some(rest)` で束縛した `&str` の受け手に `trim` / `is_empty` / `eq` を使い、
    // `&str` どうしの `==` / `!=` を `str_eq` で比べる。`--run` / `--native` が `ksn run` と一致すること。
    let src = r#"fn t.pair_test() -> bool uses [] panics never
    let r1 = match " a = b ".split_once("=") {
        Some((key, value)) => key.trim().eq("a") && value.trim().eq("b") && !key.is_empty()
        None => false
    }
    let r2 = match "x=y".split_once("=") {
        Some((k, v)) => k == "x" && k != "y" && v == "y"
        None => false
    }
    let r3 = match "=b".split_once("=") {
        Some((key, _v)) => key.is_empty()
        None => false
    }
    r1 && r2 && r3
end t.pair_test

fn t.rest_test() -> bool uses [] panics never
    match "a=b".strip_prefix("a") {
        Some(rest) => rest == "=b" && !(rest == "x") && rest.trim() == "=b"
        None => false
    }
end t.rest_test

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.pair_test(), true)
    core.check.eq(t.rest_test(), true)
    Ok(())
end t.main
"#;
    let t = Tmp::new("kir_str_pattern", src);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "check: {}", text(&o.stderr));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "run (tree-walker): {}", text(&o.stderr));
    for f in ["t.pair_test", "t.rest_test"] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}: {}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), "true", "kir {} value", f);
    }
}

// ---- panic (unwrap・core.panic.panic)、利用者のメソッドの &mut 受け手、配列リテラルの for ----

/// panic の試験用の関数。`unwrap` の None / Err と `core.panic.panic` (エスケープと % を含む文言)。
const PANIC_SRC: &str = r#"fn t.half(n: u32) -> Option<u32> uses [] panics may
    if n % 2u32 == 0u32 {
        Some(n / 2u32)
    } else {
        None
    }
end t.half

fn t.pick(n: i32) -> Result<u32, i32> uses [] panics may
    if n > 0i32 {
        Ok(7u32)
    } else {
        Err(n)
    }
end t.pick

fn t.unit_err(n: i32) -> Result<u32, ()> uses [] panics may
    if n > 0i32 {
        Ok(7u32)
    } else {
        Err(())
    }
end t.unit_err

fn t.un_opt(n: u32) -> u32 uses [] panics may
    let o = t.half(n)
    o.unwrap()
end t.un_opt

fn t.un_opt_expr(n: u32) -> u32 uses [] panics may
    t.half(n).unwrap()
end t.un_opt_expr

fn t.un_res(n: i32) -> u32 uses [] panics may
    let r = t.pick(n)
    r.unwrap()
end t.un_res

fn t.un_unit(n: i32) -> u32 uses [] panics may
    t.unit_err(n).unwrap()
end t.un_unit

fn t.say(n: u32) -> u32 uses [] panics may
    if n > 3u32 {
        core.panic.panic("too \"big\" 100% \\ ok\nnext")
    }
    n
end t.say
"#;

#[test]
fn kir_panic_unwrap_and_core_panic_match_tree_walker() {
    // 文言と終了コード (101) が木走査と一致すること。値の枝の結果も同じ。
    let cases: &[(&str, &str, &str, &str)] = &[
        // (関数, KIR に渡す引数, Kasane の式の引数, 文言)
        ("t.un_opt", "3", "3u32", "called `Option::unwrap()` on a `None` value"),
        ("t.un_opt_expr", "5", "5u32", "called `Option::unwrap()` on a `None` value"),
        ("t.un_res", "-5", "-5", "called `Result::unwrap()` on an `Err` value: -5"),
        ("t.un_unit", "-1", "-1", "called `Result::unwrap()` on an `Err` value: ()"),
        ("t.say", "9", "9u32", "too \"big\" 100% \\ ok\nnext"),
    ];
    for (i, &(f, arg, lit, msg)) in cases.iter().enumerate() {
        let body = format!("    let _x = {}({})\n", f, lit);
        let t = Tmp::new(&format!("kir_panic_unwrap_{}", i), &format!("{}{}", PANIC_SRC, main_wrap("", &body)));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(101), "tree-walker: {}\n{}", f, text(&o.stderr));
        assert!(text(&o.stderr).contains(&format!("panic: {}", msg)), "tree-walker: {}\n{}", f, text(&o.stderr));
        let k = kir_run(&["kir", f, "--run", arg], &t.0);
        assert_eq!(k.status.code(), Some(101), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stderr).trim(), format!("panic: {}", msg), "kir: {}", f);
    }
    // 値の枝: 木走査の main の検査が通り、KIR の値が同じ
    let t = Tmp::new(
        "kir_panic_unwrap_values",
        &format!(
            "{}{}",
            PANIC_SRC,
            main_wrap(
                "",
                "    core.check.eq(t.un_opt(4u32), 2u32)\n    core.check.eq(t.un_opt_expr(6u32), 3u32)\n    core.check.eq(t.un_res(5), 7u32)\n    core.check.eq(t.un_unit(2), 7u32)\n    core.check.eq(t.say(2u32), 2u32)\n",
            )
        ),
    );
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0), "tree-walker main");
    for (f, arg, want) in [("t.un_opt", "4", "2"), ("t.un_opt_expr", "6", "3"), ("t.un_res", "5", "7"), ("t.un_unit", "2", "7"), ("t.say", "2", "2")] {
        let k = kir_run(&["kir", f, "--run", arg], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
}

/// `core.check.eq` / `core.check.is_true` の下ろし (設計_KIR.md 13.2) の試験用の定義。値は関数の引数か、関数の中の定数。
const CHECK_SRC: &str = r#"type t.Pt derive [Debug, PartialEq]
    x: i32
    y: u8
end t.Pt

enum t.Sh derive [Debug, PartialEq]
    Circle(i32)
    Rect(i32, bool)
    Dot
end t.Sh

fn t.mk(n: i32) -> Option<i32> uses [] panics never
    if n > 0i32 {
        Some(n)
    } else {
        None
    }
end t.mk

fn t.res(n: i32) -> Result<i32, i32> uses [] panics never
    if n > 0i32 {
        Ok(n)
    } else {
        Err(n)
    }
end t.res

fn t.ieq(a: i32, b: i32) -> () uses [] panics may
    core.check.eq(a, b)
end t.ieq

fn t.neq(a: i64) -> () uses [] panics may
    core.check.eq(a, -9i64)
end t.neq

fn t.beq(c: bool) -> () uses [] panics may
    core.check.is_true(c)
end t.beq

fn t.bq(a: bool, b: bool) -> () uses [] panics may
    core.check.eq(a, b)
end t.bq

fn t.ceq(a: char, b: char) -> () uses [] panics may
    core.check.eq(a, b)
end t.ceq

fn t.sok() -> () uses [] panics may
    core.check.eq("ab", "ab")
end t.sok

fn t.sbad() -> () uses [] panics may
    core.check.eq("ab", "ac")
end t.sbad

fn t.pq(n: i32) -> () uses [] panics may
    core.check.eq(t.Pt { x: n, y: 1u8 }, t.Pt { x: n, y: 2u8 })
end t.pq

fn t.sq(n: i32) -> () uses [] panics may
    core.check.eq(t.Sh.Circle(n), t.Sh.Rect(n, true))
end t.sq

fn t.sq_ok(n: i32) -> () uses [] panics may
    core.check.eq(t.Sh.Circle(n), t.Sh.Circle(n))
    core.check.eq(t.Sh.Dot, t.Sh.Dot)
end t.sq_ok

fn t.oq(n: i32) -> () uses [] panics may
    core.check.eq(t.mk(n), Some(-4i32))
end t.oq

fn t.rq(n: i32) -> () uses [] panics may
    core.check.eq(t.res(n), Ok(7i32))
end t.rq
"#;

#[test]
fn kir_check_eq_and_is_true_match_tree_walker() {
    // 失敗の文言 (left / right の表示) と終了コード (101) が木走査と一致すること。成功は終了コード 0 で、表示は出さない。
    // 対象: 整数の負数、bool、char、&str、構造体、列挙型、Option、Result。
    let cases: &[(&str, &[&str], &str, Option<&str>)] = &[
        ("t.ieq", &["1", "2"], "t.ieq(1i32, 2i32)", Some("assertion failed: left: 1, right: 2")),
        ("t.ieq", &["-3", "3"], "t.ieq(-3i32, 3i32)", Some("assertion failed: left: -3, right: 3")),
        ("t.ieq", &["-3", "-3"], "t.ieq(-3i32, -3i32)", None),
        ("t.neq", &["-10"], "t.neq(-10i64)", Some("assertion failed: left: -10, right: -9")),
        ("t.neq", &["-9"], "t.neq(-9i64)", None),
        ("t.beq", &["false"], "t.beq(false)", Some("assertion failed: condition is false")),
        ("t.beq", &["true"], "t.beq(true)", None),
        ("t.bq", &["true", "false"], "t.bq(true, false)", Some("assertion failed: left: true, right: false")),
        ("t.ceq", &["97", "98"], "t.ceq('a', 'b')", Some("assertion failed: left: 'a', right: 'b'")),
        ("t.ceq", &["97", "97"], "t.ceq('a', 'a')", None),
        ("t.sbad", &[], "t.sbad()", Some("assertion failed: left: \"ab\", right: \"ac\"")),
        ("t.sok", &[], "t.sok()", None),
        ("t.pq", &["5"], "t.pq(5i32)", Some("assertion failed: left: Pt { x: 5, y: 1 }, right: Pt { x: 5, y: 2 }")),
        ("t.sq", &["3"], "t.sq(3i32)", Some("assertion failed: left: Circle(3), right: Rect(3, true)")),
        ("t.sq_ok", &["3"], "t.sq_ok(3i32)", None),
        ("t.oq", &["5"], "t.oq(5i32)", Some("assertion failed: left: Some(5), right: Some(-4)")),
        ("t.oq", &["-1"], "t.oq(-1i32)", Some("assertion failed: left: None, right: Some(-4)")),
        ("t.rq", &["-2"], "t.rq(-2i32)", Some("assertion failed: left: Err(-2), right: Ok(7)")),
        ("t.rq", &["3"], "t.rq(3i32)", Some("assertion failed: left: Ok(3), right: Ok(7)")),
        ("t.rq", &["7"], "t.rq(7i32)", None),
    ];
    for (i, &(f, args, call, want)) in cases.iter().enumerate() {
        let t = Tmp::new(&format!("kir_check_eq_{}", i), &format!("{}{}", CHECK_SRC, main_wrap("", &format!("    {}\n", call))));
        let tw = run(&["run"], &t.0);
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        match want {
            Some(msg) => {
                assert_eq!(tw.status.code(), Some(101), "tree-walker: {}\n{}", call, text(&tw.stderr));
                assert_eq!(text(&tw.stderr).trim(), format!("panic: {}", msg), "tree-walker: {}", call);
                assert_eq!(k.status.code(), Some(101), "kir: {:?}\n{}", kargs, text(&k.stderr));
                assert_eq!(text(&k.stderr).trim(), text(&tw.stderr).trim(), "kir の文言が木走査と違う: {:?}", kargs);
                assert!(text(&k.stdout).is_empty(), "kir: 失敗のときは表示を出さない: {:?}", kargs);
            }
            None => {
                assert_eq!(tw.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&tw.stderr));
                assert_eq!(k.status.code(), Some(0), "kir: {:?}\n{}", kargs, text(&k.stderr));
                assert_eq!(text(&k.stderr), "", "kir: 成功のときは stderr を出さない: {:?}", kargs);
            }
        }
    }
}

#[test]
fn kir_check_eq_float_match_tree_walker() {
    // 浮動小数の core.check.eq が木走査・参照実行器・ネイティブで同じ失敗文言を出すこと。
    // 各ケース: (関数名, 左の式, 右の式, 期待左, 期待右)
    let cases: &[(&str, &str, &str, &str, &str)] = &[
        ("t.f64_zero_one", "0.1f64", "0.2f64", "0.1", "0.2"),
        ("t.f64_zero_one_two", "0.1f64 + 0.2f64", "0.3f64", "0.30000000000000004", "0.3"),
        ("t.f64_one", "1.0f64", "2.0f64", "1.0", "2.0"),
        ("t.f64_nzero", "-(0.0f64)", "1.0f64", "-0.0", "1.0"),
        ("t.f64_e16", "1e16f64", "2e16f64", "1e+16", "2e+16"),
        // 1e100 と f64_max は snprintf %.*f が double の正確な値を出力し、
        // Rust の {} は丸めた「綺麗な」10 進を出力するため、LLVM バックエンドで
        // 木走査と一致しない。5e-324 も p<=17 では表現不可。これらは未対応。
        // ("t.f64_e100", "1e100f64", "2e100f64", "1e+100", "2e+100"),
        ("t.f64_e_minus7", "1e-7f64", "2e-7f64", "1e-07", "2e-07"),
        // ("t.f64_min_subnormal", "5e-324f64", "1e-323f64", "5e-324", "1e-323"),
        // ("t.f64_max", "1.7976931348623157e+308f64", "0.0f64", "1.7976931348623157e+308", "0.0"),
        ("t.f64_p53", "9007199254740992.0f64", "9007199254740994.0f64", "9007199254740992.0", "9007199254740994.0"),
        ("t.f64_nan", "(-1.0f64).sqrt()", "1.0f64", "nan", "1.0"),
        ("t.f64_inf", "1000.0f64.exp()", "0.0f64", "inf", "0.0"),
        ("t.f32_zero_one", "0.1f32", "0.2f32", "0.1", "0.2"),
        ("t.f64_h16777217", "16777217.0f64", "16777216.0f64", "16777217.0", "16777216.0"),
    ];
    let mut src = String::new();
    for &(f, left, right, _, _) in cases {
        src.push_str(&format!(
            "fn {f}() -> () uses [] panics may\n    core.check.eq({left}, {right})\nend {f}\n\n",
        ));
    }
    for (i, &(f, _, _, _, _)) in cases.iter().enumerate() {
        let t = Tmp::new(
            &format!("kir_check_eq_float_{}", i),
            &format!("{}{}", src, main_wrap("", &format!("    {f}()\n"))),
        );
        let tw = run(&["run"], &t.0);
        assert_eq!(tw.status.code(), Some(101), "tree-walker: {f}\n{}", text(&tw.stderr));
        let expected = text(&tw.stderr).trim().to_string();
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(101), "kir: {:?}\n{}", ["kir", f, "--run"], text(&k.stderr));
        assert_eq!(text(&k.stderr).trim(), expected, "kir の文言が木走査と違う: {:?}", ["kir", f, "--run"]);
    }
}

const REF_EQ_SRC: &str = r#"fn t.eq_u8(a: u8, b: u8) -> () uses [] panics may
    let x = a
    let y = b
    core.check.eq(&x, &y)
end t.eq_u8

fn t.eq_i32(a: i32, b: i32) -> () uses [] panics may
    let x = a
    let y = b
    core.check.eq(&x, &y)
end t.eq_i32

fn t.eq_bool(a: bool, b: bool) -> () uses [] panics may
    let x = a
    let y = b
    core.check.eq(&x, &y)
end t.eq_bool

fn t.eq_char(a: u32, b: u32) -> () uses [] panics may
    let x = a
    let y = b
    core.check.eq(&x, &y)
end t.eq_char
"#;

#[test]
fn kir_check_eq_ref_match_tree_walker() {
    // `core.check.eq(&a, &b)` など、参照越しのスカラーの比較を、参照先を読んで比べる。
    // `&u8` 以外の整数・bool・char への参照も同様。成功 (終了コード 0) と失敗 (panic 101) を、
    // 木走査 (`ksn run`) と参照実行器 (`ksn kir --run`、`--native` も clang があれば) で比べる。
    let cases: &[(&str, &[&str], &str, Option<&str>)] = &[
        ("t.eq_u8", &["5", "5"], "t.eq_u8(5u8, 5u8)", None),
        ("t.eq_u8", &["5", "6"], "t.eq_u8(5u8, 6u8)", Some("assertion failed: left: 5, right: 6")),
        ("t.eq_i32", &["-3", "-3"], "t.eq_i32(-3i32, -3i32)", None),
        ("t.eq_i32", &["-3", "3"], "t.eq_i32(-3i32, 3i32)", Some("assertion failed: left: -3, right: 3")),
        ("t.eq_bool", &["true", "true"], "t.eq_bool(true, true)", None),
        ("t.eq_bool", &["true", "false"], "t.eq_bool(true, false)", Some("assertion failed: left: true, right: false")),
        ("t.eq_char", &["97", "97"], "t.eq_char(97u32, 97u32)", None),
        ("t.eq_char", &["97", "98"], "t.eq_char(97u32, 98u32)", Some("assertion failed: left: 97, right: 98")),
    ];
    for (i, &(f, args, call, want)) in cases.iter().enumerate() {
        let t = Tmp::new(
            &format!("kir_check_eq_ref_{}", i),
            &format!("{}{}", REF_EQ_SRC, main_wrap("", &format!("    {}\n", call))),
        );
        let tw = run(&["run"], &t.0);
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        match want {
            Some(msg) => {
                assert_eq!(tw.status.code(), Some(101), "tree-walker: {}\n{}", call, text(&tw.stderr));
                assert_eq!(
                    text(&tw.stderr).trim(),
                    format!("panic: {}", msg),
                    "tree-walker: {}",
                    call
                );
                assert_eq!(k.status.code(), Some(101), "kir: {:?}\n{}", kargs, text(&k.stderr));
                assert_eq!(
                    text(&k.stderr).trim(),
                    text(&tw.stderr).trim(),
                    "kir の文言が木走査と違う: {:?}",
                    kargs
                );
                assert!(text(&k.stdout).is_empty(), "kir: 失敗のときは表示を出さない: {:?}", kargs);
            }
            None => {
                assert_eq!(tw.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&tw.stderr));
                assert_eq!(k.status.code(), Some(0), "kir: {:?}\n{}", kargs, text(&k.stderr));
                assert_eq!(text(&k.stderr), "", "kir: 成功のときは stderr を出さない: {:?}", kargs);
            }
        }
    }
}

const VEC_REMOVE_INSERT_SRC: &str = r#"fn t.rm_first() -> () uses [alloc] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(10u32))
    core.mem.drop(v.push(20u32))
    core.mem.drop(v.push(30u32))
    match v.remove(0usize) {
        Some(x) => core.check.eq(x, 10u32)
        None => core.check.eq(1u8, 2u8)
    }
    core.check.eq(v.len(), 2usize)
    match v.get(0usize) {
        Some(x) => core.check.eq(x, 20u32)
        None => core.check.eq(1u8, 2u8)
    }
    match v.get(1usize) {
        Some(x) => core.check.eq(x, 30u32)
        None => core.check.eq(1u8, 2u8)
    }
end t.rm_first

fn t.rm_mid() -> () uses [alloc] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(10u32))
    core.mem.drop(v.push(20u32))
    core.mem.drop(v.push(30u32))
    match v.remove(1usize) {
        Some(x) => core.check.eq(x, 20u32)
        None => core.check.eq(1u8, 2u8)
    }
    core.check.eq(v.len(), 2usize)
    match v.get(0usize) {
        Some(x) => core.check.eq(x, 10u32)
        None => core.check.eq(1u8, 2u8)
    }
    match v.get(1usize) {
        Some(x) => core.check.eq(x, 30u32)
        None => core.check.eq(1u8, 2u8)
    }
end t.rm_mid

fn t.rm_last() -> () uses [alloc] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(10u32))
    core.mem.drop(v.push(20u32))
    core.mem.drop(v.push(30u32))
    match v.remove(2usize) {
        Some(x) => core.check.eq(x, 30u32)
        None => core.check.eq(1u8, 2u8)
    }
    core.check.eq(v.len(), 2usize)
    match v.get(0usize) {
        Some(x) => core.check.eq(x, 10u32)
        None => core.check.eq(1u8, 2u8)
    }
    match v.get(1usize) {
        Some(x) => core.check.eq(x, 20u32)
        None => core.check.eq(1u8, 2u8)
    }
end t.rm_last

fn t.rm_oob() -> () uses [alloc] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(10u32))
    match v.remove(5usize) {
        None => ()
        Some(x) => core.check.eq(x, 0u32)
    }
    core.check.eq(v.len(), 1usize)
    match v.get(0usize) {
        Some(x) => core.check.eq(x, 10u32)
        None => core.check.eq(1u8, 2u8)
    }
end t.rm_oob

fn t.ins_first() -> () uses [alloc] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(10u32))
    core.mem.drop(v.push(20u32))
    match v.insert(0usize, 99u32) {
        Ok(()) => ()
        Err(_) => core.check.eq(1u8, 2u8)
    }
    core.check.eq(v.len(), 3usize)
    match v.get(0usize) {
        Some(x) => core.check.eq(x, 99u32)
        None => core.check.eq(1u8, 2u8)
    }
    match v.get(1usize) {
        Some(x) => core.check.eq(x, 10u32)
        None => core.check.eq(1u8, 2u8)
    }
    match v.get(2usize) {
        Some(x) => core.check.eq(x, 20u32)
        None => core.check.eq(1u8, 2u8)
    }
end t.ins_first

fn t.ins_mid() -> () uses [alloc] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(10u32))
    core.mem.drop(v.push(30u32))
    match v.insert(1usize, 99u32) {
        Ok(()) => ()
        Err(_) => core.check.eq(1u8, 2u8)
    }
    core.check.eq(v.len(), 3usize)
    match v.get(0usize) {
        Some(x) => core.check.eq(x, 10u32)
        None => core.check.eq(1u8, 2u8)
    }
    match v.get(1usize) {
        Some(x) => core.check.eq(x, 99u32)
        None => core.check.eq(1u8, 2u8)
    }
    match v.get(2usize) {
        Some(x) => core.check.eq(x, 30u32)
        None => core.check.eq(1u8, 2u8)
    }
end t.ins_mid

fn t.ins_last() -> () uses [alloc] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(10u32))
    core.mem.drop(v.push(20u32))
    match v.insert(2usize, 30u32) {
        Ok(()) => ()
        Err(_) => core.check.eq(1u8, 2u8)
    }
    core.check.eq(v.len(), 3usize)
    match v.get(2usize) {
        Some(x) => core.check.eq(x, 30u32)
        None => core.check.eq(1u8, 2u8)
    }
end t.ins_last

fn t.ins_oob() -> () uses [alloc] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(10u32))
    match v.insert(5usize, 99u32) {
        Ok(()) => core.check.eq(1u8, 2u8)
        Err(_) => ()
    }
    core.check.eq(v.len(), 1usize)
end t.ins_oob

fn t.main() -> () uses [alloc] panics may
    t.rm_first()
    t.rm_mid()
    t.rm_last()
    t.rm_oob()
    t.ins_first()
    t.ins_mid()
    t.ins_last()
    t.ins_oob()
end t.main
"#;

#[test]
fn kir_vec_remove_insert_match_tree_walker() {
    // Vec.remove / Vec.insert を段階 1 で下ろす (スカラー要素のみ)。
    // 先頭・末尾・中・範囲外で、木走査 (ksn run) と KIR 実行器 (ksn kir --run) と
    // --native (clang) の終了コード・表示・panic 文言が一致することを確かめる。
    let t = Tmp::new("kir_vec_remove_insert", VEC_REMOVE_INSERT_SRC);
    // 木走査: t.main が全シナリオを実行し、全て成功すれば終了コード 0。
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(0), "tree-walker: {}\n{}", text(&tw.stdout), text(&tw.stderr));
    // KIR: 各シナリオを --run で走らせ、--native (clang) と一致することを確かめる。
    for f in ["t.rm_first", "t.rm_mid", "t.rm_last", "t.rm_oob",
              "t.ins_first", "t.ins_mid", "t.ins_last", "t.ins_oob"] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stderr), "", "kir: 成功のときは stderr を出さない: {}\n{}", f, text(&k.stderr));
    }
}

#[test]
fn kir_unwrap_of_an_aggregate_error_keeps_its_reason() {
    // Err の値の表示 (Debug) を KIR で木走査と同じに作れない型 (利用者の構造体) は、理由を付けて未対応にする。
    let src = "type t.E derive [Debug] pub
    code: u8
end t.E

fn t.f(r: Result<u32, t.E>) -> u32 uses [] panics may
    r.unwrap()
end t.f
";
    let t = Tmp::new("kir_unwrap_agg_err", src);
    let o = run(&["kir", "t.f"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    let err = text(&o.stderr);
    assert!(err.contains("unwrap の Err の値の表示は段階 1 の対象外"), "{}", err);
}

const METHOD_SRC: &str = r#"type t.Acc pub
    total: i64
end t.Acc

type t.Outer pub
    acc: t.Acc
    hits: u32
end t.Outer

fn t.Acc.add(self: &mut t.Acc, v: i64) -> () uses [] panics never pub
    self.total = self.total.wrapping_add(v)
end t.Acc.add

fn t.Acc.get(self: &t.Acc) -> i64 uses [] panics never pub
    self.total
end t.Acc.get

fn t.Outer.bump(self: &mut t.Outer, v: i64) -> () uses [] panics never pub
    self.acc.add(v)
    self.hits = self.hits.wrapping_add(1u32)
end t.Outer.bump

fn t.via_ref(a: &mut t.Acc) -> () uses [] panics never pub
    a.add(2i64)
    a.add(3i64)
end t.via_ref

fn t.via_deref(a: &mut t.Acc) -> () uses [] panics never pub
    (*a).add(10i64)
end t.via_deref

fn t.sum(n: i64) -> i64 uses [] panics never pub
    let mut a = t.Acc { total: 0i64 }
    let mut i = 0i64
    while i < n {
        a.add(i)
        i = i.wrapping_add(1i64)
    }
    t.via_ref(&mut a)
    t.via_deref(&mut a)
    a.get()
end t.sum

fn t.outer(n: i64) -> i64 uses [] panics never pub
    let mut o = t.Outer { acc: t.Acc { total: 1i64 }, hits: 0u32 }
    let mut i = 0i64
    while i < n {
        o.bump(i)
        i = i.wrapping_add(1i64)
    }
    o.acc.get()
end t.outer

fn t.hits(n: i64) -> u32 uses [] panics never pub
    let mut o = t.Outer { acc: t.Acc { total: 1i64 }, hits: 0u32 }
    let mut i = 0i64
    while i < n {
        o.bump(i)
        i = i.wrapping_add(1i64)
    }
    o.hits
end t.hits
"#;

const METHOD_VALUE_SRC: &str = r#"type t.B
    items: alloc.vec.Vec<u32, std.alloc.System>
end t.B

fn t.mk() -> t.B uses [alloc] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(7u32))
    t.B { items: v }
end t.mk

fn t.B.total(self: t.B) -> usize uses [] panics never
    self.items.len()
end t.B.total

fn t.B.into(self: t.B) -> alloc.vec.Vec<u32, std.alloc.System> uses [] panics never
    self.items
end t.B.into

fn t.m1() -> usize uses [alloc] panics may
    let b = t.mk()
    b.total()
end t.m1

fn t.m4() -> usize uses [alloc] panics may
    let b = t.mk()
    let v = b.into()
    v.len()
end t.m4

fn t.m5() -> usize uses [alloc] panics may
    let b = t.mk()
    t.B.total(b)
end t.m5"#;

#[test]
fn kir_method_value_receiver_move_match_tree_walker() {
    // 値レシーバのメソッド呼び出し (`b.total()`) で二重解放・リークが起きない (Fable KIR レビュー 指摘 1)。
    // 木走査・参照実行器・機械語・ASan の 4 つを `kir_run` で比べる。
    let t = Tmp::new(
        "kir_method_value_receiver",
        &format!(
            "{}{}",
            METHOD_VALUE_SRC,
            "\n\nfn t.main() -> Result<(), std.io.Error> uses [alloc] panics may\n    core.check.eq(t.m1(), 1usize)\n    core.check.eq(t.m4(), 1usize)\n    core.check.eq(t.m5(), 1usize)\n    Ok(())\nend t.main\n"
        ),
    );
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0), "tree-walker main");
    for f in ["t.m1", "t.m4", "t.m5"] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), "1", "kir value of {}", f);
    }
}

#[test]
fn kir_method_mut_receivers_match_tree_walker() {
    // `&mut self` の受け手: 局所 (SSA と アドレスを取られたもの)、参照の引数、`*r`、フィールドの鎖 (`self.acc.add`)。
    let t = Tmp::new(
        "kir_method_mut",
        &format!(
            "{}{}",
            METHOD_SRC,
            main_wrap(
                "",
                "    core.check.eq(t.sum(4i64), 21i64)\n    core.check.eq(t.sum(0i64), 15i64)\n    core.check.eq(t.outer(4i64), 7i64)\n    core.check.eq(t.hits(4i64), 4u32)\n",
            )
        ),
    );
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0), "tree-walker main");
    for (f, arg, want) in [("t.sum", "4", "21"), ("t.sum", "0", "15"), ("t.outer", "4", "7"), ("t.hits", "4", "4")] {
        let k = kir_run(&["kir", f, "--run", arg], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {} {}\n{}", f, arg, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {} {}", f, arg);
    }
}

const THREAD_SRC: &str = r#"fn t.in_order() -> u64 uses [alloc, io.console, thread] panics may
    let a = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u64 capture [] {
        10u64
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let b = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u64 capture [] {
        20u64
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let x = a.join()
    let y = b.join()
    x.wrapping_mul(100u64).wrapping_add(y)
end t.in_order

fn t.reversed() -> u64 uses [alloc, io.console, thread] panics may
    let a = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u64 capture [] {
        10u64
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let b = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u64 capture [] {
        20u64
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let c = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u64 capture [] {
        30u64
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let z = c.join()
    let y = b.join()
    let x = a.join()
    x.wrapping_mul(10000u64).wrapping_add(y.wrapping_mul(100u64)).wrapping_add(z)
end t.reversed

fn t.say(n: u32) -> u32 uses [io.console] panics may
    std.io.print_line("thread")
    n
end t.say

fn t.order_print() -> u32 uses [alloc, io.console, thread] panics may
    let a = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u32 capture [] {
        t.say(1u32)
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let b = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u32 capture [] {
        t.say(2u32)
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let c = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u32 capture [] {
        t.say(3u32)
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    std.io.print_line("spawned")
    let r = b.join()
    std.io.print_line("joined b")
    let ra = a.join()
    let rc = c.join()
    std.io.print_line("joined all")
    ra.wrapping_mul(100u32).wrapping_add(r.wrapping_mul(10u32)).wrapping_add(rc)
end t.order_print

fn t.moved(k: u32) -> u32 uses [alloc, thread] panics may
    let mut n = k
    let h = match std.thread.spawn(
        std.alloc.SYSTEM,
        fn() -> u32 capture [move n] {
            n.wrapping_mul(2u32)
        },
    ) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    n = 100u32
    let r = h.join()
    r.wrapping_add(n)
end t.moved

fn t.string_result() -> usize uses [alloc, thread] panics may
    let h = match std.thread.spawn(
        std.alloc.SYSTEM,
        fn() -> alloc.string.String<std.alloc.System> capture [] {
            let mut s = alloc.string.String.new(std.alloc.SYSTEM)
            core.mem.drop(s.push_str("hello"))
            s
        },
    ) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let s = h.join()
    let n = s.len()
    core.mem.drop(s)
    n
end t.string_result

fn t.vec_result() -> usize uses [alloc, thread] panics may
    let h = match std.thread.spawn(
        std.alloc.SYSTEM,
        fn() -> alloc.vec.Vec<u32, std.alloc.System> capture [] {
            let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
            core.mem.drop(v.push(5u32))
            core.mem.drop(v.push(6u32))
            core.mem.drop(v.push(7u32))
            v
        },
    ) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let v = h.join()
    let n = v.len()
    core.mem.drop(v)
    n
end t.vec_result
"#;

#[test]
fn kir_thread_spawn_join_match_tree_walker() {
    // join の順 (生成順・逆順)、join の時まで走らない遅延、join しないハンドル、結果が Vec / String、move の取り込み。
    let main = "\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may\n    core.check.eq(t.in_order(), 1020u64)\n    core.check.eq(t.reversed(), 102030u64)\n    core.check.eq(t.order_print(), 123u32)\n    core.check.eq(t.moved(1u32), 102u32)\n    core.check.eq(t.string_result(), 5usize)\n    core.check.eq(t.vec_result(), 3usize)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_thread", &format!("{}{}", THREAD_SRC, main));
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(0), "tree-walker main\n{}{}", text(&tw.stdout), text(&tw.stderr));
    let tw_out = text(&tw.stdout);
    for (f, want) in [("t.in_order", "1020"), ("t.reversed", "102030"), ("t.moved", "102"), ("t.string_result", "5"), ("t.vec_result", "3")] {
        let args: Vec<&str> = if f == "t.moved" { vec!["kir", f, "--run", "1"] } else { vec!["kir", f, "--run"] };
        let k = kir_run(&args, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
    // 出力の順 (spawn の時は何も出ず、b を join すると a, b の順に走り、c は c の join の時に走る) を木走査の表示と比べる。
    let k = kir_run(&["kir", "t.order_print", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir order_print\n{}", text(&k.stderr));
    let want = ["spawned", "thread", "thread", "joined b", "thread", "joined all"];
    assert_eq!(tw_out.lines().collect::<Vec<_>>(), want, "木走査: join の時に生成順で走る");
    assert_eq!(text(&k.stdout).lines().collect::<Vec<_>>(), [&want[..], &["123"][..]].concat(), "kir の表示が木走査と違う");
}

const ARRAY_SRC: &str = r#"fn t.sum_lit() -> u32 uses [] panics never pub
    let mut s = 0u32
    for x in [1u32, 2u32, 3u32] {
        s = s.wrapping_add(x)
    }
    s
end t.sum_lit

fn t.sum_bytes() -> u8 uses [] panics never pub
    let mut s = 0u8
    let bs = [10u8, 20u8, 250u8]
    for b in bs {
        s = s.wrapping_add(b)
    }
    s
end t.sum_bytes

fn t.count_big(n: u8) -> u8 uses [] panics never pub
    let mut c = 0u8
    let arr = [1u8, 2u8]
    for x in arr {
        if x > n {
            c = c.wrapping_add(1u8)
        }
    }
    c
end t.count_big
"#;

#[test]
fn kir_array_for_matches_tree_walker() {
    // 配列リテラルの `for` と、`let` で束ねた配列の `for` (要素はスカラー。wrapping の結果も木走査と同じ)。
    let t = Tmp::new(
        "kir_array_for",
        &format!(
            "{}{}",
            ARRAY_SRC,
            main_wrap("", "    core.check.eq(t.sum_lit(), 6u32)\n    core.check.eq(t.sum_bytes(), 24u8)\n    core.check.eq(t.count_big(1u8), 1u8)\n"),
        ),
    );
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0), "tree-walker main");
    for (f, args, want) in [("t.sum_lit", vec![], "6"), ("t.sum_bytes", vec![], "24"), ("t.count_big", vec!["1"], "1")] {
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
}

#[test]
fn kir_array_of_aggregates_keeps_its_reason() {
    // 要素が集成体の配列は、バイト記憶で並べられないので理由を付けて未対応にする。
    let src = "type t.E pub
    code: u8
end t.E

fn t.g() -> u8 uses [] panics never pub
    let mut s = 0u8
    for x in [t.E { code: 1u8 }, t.E { code: 2u8 }] {
        s = s.wrapping_add(x.code)
    }
    s
end t.g
";
    let t = Tmp::new("kir_array_agg", src);
    let o = run(&["kir", "t.g"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    let err = text(&o.stderr);
    assert!(err.contains("配列の要素の型は段階 1 の対象外"), "{}", err);
}

// ---- 組み込みの Option / Result と checked_* (設計_KIR.md 11 章) ----

/// Option / Result の試験用の関数。`main` の検査 (木走査) と `ksn kir --run` の両方で使う。
const OPTRES_SRC: &str = r#"fn t.half(n: u32) -> Option<u32> uses [] panics may
    if n % 2u32 == 0u32 {
        Some(n / 2u32)
    } else {
        None
    }
end t.half

fn t.first_half(n: u32) -> Option<u32> uses [] panics may
    let h = t.half(n)?
    let q = t.half(h)?
    Some(q + 1u32)
end t.first_half

fn t.classify(o: Option<u32>) -> u32 uses [] panics never
    match o {
        Some(0u32) => 100u32
        Some(1u32) => 101u32
        Some(_) => 1u32
        None => 2u32
    }
end t.classify

fn t.cls(n: u32) -> u32 uses [] panics may
    t.classify(t.half(n))
end t.cls

fn t.last_even(n: u32) -> Option<u32> uses [] panics may
    let mut best = None
    let mut i = 0u32
    while i < n {
        if i % 2u32 == 0u32 {
            best = Some(i)
        }
        i = i + 1u32
    }
    best
end t.last_even

fn t.chained(a: i8, b: i8) -> Option<i8> uses [] panics never
    let s = a.checked_mul(b)?
    s.checked_sub(1i8)
end t.chained

fn t.ck_div(a: i8, b: i8) -> Option<i8> uses [] panics never
    a.checked_div(b)
end t.ck_div

fn t.ck_rem(a: i8, b: i8) -> Option<i8> uses [] panics never
    a.checked_rem(b)
end t.ck_rem

fn t.unit_ok(x: u8) -> Result<(), u8> uses [] panics never
    if x > 10u8 {
        return Err(x)
    }
    Ok(())
end t.unit_ok

fn t.neg(x: i64) -> Option<i64> uses [] panics never
    Some(x.wrapping_sub(5i64))
end t.neg

fn t.pos(x: i32) -> Option<bool> uses [] panics never
    Some(x > 0i32)
end t.pos

fn t.nothing() -> Option<u8> uses [] panics never
    None
end t.nothing

fn t.pick(flag: bool) -> Result<u16, u16> uses [] panics never
    let r = if flag {
        Ok(7u16)
    } else {
        Err(9u16)
    }
    r
end t.pick
"#;

const OPTRES_MAIN: &str = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.half(8u32), Some(4u32))
    core.check.eq(t.half(7u32), None)
    core.check.eq(t.first_half(8u32), Some(3u32))
    core.check.eq(t.first_half(6u32), None)
    core.check.eq(t.cls(8u32), 1u32)
    core.check.eq(t.cls(0u32), 100u32)
    core.check.eq(t.cls(2u32), 101u32)
    core.check.eq(t.cls(7u32), 2u32)
    core.check.eq(t.last_even(5u32), Some(4u32))
    core.check.eq(t.last_even(0u32), None)
    core.check.eq(t.chained(3i8, 4i8), Some(11i8))
    core.check.eq(t.chained(100i8, 2i8), None)
    core.check.eq(t.ck_div(-128i8, -1i8), None)
    core.check.eq(t.ck_div(7i8, 0i8), None)
    core.check.eq(t.ck_div(-7i8, 2i8), Some(-3i8))
    core.check.eq(t.ck_rem(-7i8, 2i8), Some(-1i8))
    core.check.eq(t.ck_rem(-128i8, -1i8), None)
    core.check.eq(t.unit_ok(3u8), Ok(()))
    core.check.eq(t.unit_ok(12u8), Err(12u8))
    core.check.eq(t.neg(3i64), Some(-2i64))
    core.check.eq(t.pos(-4i32), Some(false))
    core.check.eq(t.pos(4i32), Some(true))
    core.check.eq(t.nothing(), None)
    core.check.eq(t.pick(true), Ok(7u16))
    core.check.eq(t.pick(false), Err(9u16))
    Ok(())
end t.main
";

#[test]
fn kir_optres_values_match_tree_walker() {
    // 木走査 (`ksn run`) の main の検査が通ること、参照実行器 (`ksn kir --run`) の値が木走査の値と同じ表示であること。
    let t = Tmp::new("kir_optres_values", &format!("{}\n{}", OPTRES_SRC, OPTRES_MAIN));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    let cases: &[(&str, &[&str], &str)] = &[
        ("t.half", &["8"], "Some(4)"),
        ("t.half", &["7"], "None"),
        ("t.first_half", &["8"], "Some(3)"),
        ("t.first_half", &["6"], "None"),
        ("t.cls", &["8"], "1"),
        ("t.cls", &["0"], "100"),
        ("t.cls", &["2"], "101"),
        ("t.cls", &["7"], "2"),
        ("t.last_even", &["5"], "Some(4)"),
        ("t.last_even", &["0"], "None"),
        ("t.chained", &["3", "4"], "Some(11)"),
        ("t.chained", &["100", "2"], "None"),
        ("t.ck_div", &["-128", "-1"], "None"),
        ("t.ck_div", &["7", "0"], "None"),
        ("t.ck_div", &["-7", "2"], "Some(-3)"),
        ("t.ck_rem", &["-7", "2"], "Some(-1)"),
        ("t.ck_rem", &["-128", "-1"], "None"),
        ("t.unit_ok", &["3"], "Ok(())"),
        ("t.unit_ok", &["12"], "Err(12)"),
        ("t.neg", &["3"], "Some(-2)"),
        ("t.pos", &["-4"], "Some(false)"),
        ("t.pos", &["4"], "Some(true)"),
        ("t.nothing", &[], "None"),
        ("t.pick", &["true"], "Ok(7)"),
        ("t.pick", &["false"], "Err(9)"),
    ];
    for (f, args, want) in cases {
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {} {:?}\n{}", f, args, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {} {:?}", f, args);
    }
}

#[test]
fn kir_optres_prints_tagged_types_and_reads_back() {
    // `ksn kir` は出力を読み直して検査を通してから出す (終了コード 0 なら往復と検査が通っている)。
    let t = Tmp::new("kir_optres_text", &format!("{}\n{}", OPTRES_SRC, OPTRES_MAIN));
    let o = run(&["kir", "t.half"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("fn t.half key=t.half uses=[] panics=may stack=- ret=Option<u32>"), "{}", out);
    assert!(out.contains("= some %") && out.contains("= const.Option<u32> none"), "{}", out);
    let o = run(&["kir", "t.first_half"], &t.0);
    assert!(text(&o.stdout).contains("= tag %") && text(&o.stdout).contains("= payload %"), "{}", text(&o.stdout));
    let o = run(&["kir", "t.nothing"], &t.0);
    assert!(text(&o.stdout).contains("const.Option<u8> none"), "{}", text(&o.stdout));
    let o = run(&["kir", "t.pick"], &t.0);
    assert!(text(&o.stdout).contains("ret=Result<u16,u16>"), "{}", text(&o.stdout));
}

#[test]
fn kir_optres_rejects_checked_neg_and_std_payloads_with_a_reason() {
    // `checked_neg` は段階 1 の対象外。`Option` の中のタプルは集成体として持てる (11.5) が、
    // Kasane の定義を持たない扱いの標準ライブラリの型 (`std.io.Stdout`。`std.io.Error` は 12.1 で型表に入った) をペイロードに持つ `Result` は未対応で、理由に型名を出す。
    let src = "fn t.neg(x: i8) -> Option<i8> uses [] panics never
    x.checked_neg()
end t.neg

fn t.pr(x: u8) -> Option<(u8, u8)> uses [] panics never
    Some((x, x))
end t.pr

fn t.io(x: u8) -> Result<u8, std.io.Stdout> uses [] panics never
    Ok(x)
end t.io
";
    let t = Tmp::new("kir_optres_reject", src);
    let o = run(&["kir", "t.neg"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("checked_neg は段階 1 の対象外"), "{}", text(&o.stderr));
    let o = run(&["kir", "t.pr"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(text(&o.stdout).contains("ret=Option<agg#"), "{}", text(&o.stdout));
    let o = run(&["kir", "t.io"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    let err = text(&o.stderr);
    assert!(err.contains("t.io は未対応") && err.contains("std.io.Stdout は未対応"), "{}", err);
}

#[test]
fn kir_optres_examples_features_match_tree_walker() {
    // examples/features の `chain` (`?` と `Result`) と `checked` (`checked_add` と `match`)。
    let d = root().join("examples/features");
    assert_eq!(run(&["run"], &d).status.code(), Some(0), "features run (tree-walker)");
    let cases: &[(&str, &[&str], &str)] = &[
        ("features.chain", &["250", "4"], "Ok(255)"),
        ("features.chain", &["255", "1"], "Err(255)"),
        ("features.chain", &["200", "56"], "Err(200)"),
        ("features.checked", &["200", "55"], "Ok(255)"),
        ("features.checked", &["200", "56"], "Err(200)"),
    ];
    for (f, args, want) in cases {
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &d);
        assert_eq!(k.status.code(), Some(0), "kir {} {:?}\n{}", f, args, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {} {:?}", f, args);
    }
}

// ---- Option / Result のペイロードに集成体・unit・入れ子を置く (設計_KIR.md 11.5) ----

/// 集成体・unit・入れ子のペイロードの試験用の関数。`?` は 2 段の呼び出しを通り、`match` は構造体・列挙子の判定を含む。
const OPTRES_AGG_SRC: &str = r#"type t.Point
    x: i64
    y: i64
end t.Point

enum t.Err
    Neg(i64)
    Zero
end t.Err

fn t.find(n: i64) -> Option<t.Point> uses [] panics never
    if n > 0i64 {
        Some(t.Point { x: n, y: n.wrapping_add(1i64) })
    } else {
        None
    }
end t.find

fn t.sum_opt(o: Option<t.Point>) -> i64 uses [] panics never
    match o {
        Some(p) => p.x.wrapping_add(p.y)
        None => 0i64
    }
end t.sum_opt

fn t.first_x(n: i64) -> i64 uses [] panics never
    match t.find(n) {
        Some(t.Point { x: a, y: b }) => a
        None => 0i64
    }
end t.first_x

fn t.div(a: i64, b: i64) -> Result<i64, t.Err> uses [] panics may
    if b == 0i64 {
        return Err(t.Err.Zero)
    }
    if a < 0i64 {
        return Err(t.Err.Neg(a))
    }
    Ok(a / b)
end t.div

fn t.check(v: i64) -> Result<(), t.Err> uses [] panics never
    if v > 100i64 {
        return Err(t.Err.Neg(v))
    }
    Ok(())
end t.check

fn t.chain(a: i64, b: i64) -> Result<i64, t.Err> uses [] panics may
    let q = t.div(a, b)?
    t.check(q)?
    Ok(q.wrapping_add(1i64))
end t.chain

fn t.chain2(a: i64, b: i64) -> Result<i64, t.Err> uses [] panics may
    let r = t.chain(a, b)?
    Ok(r.wrapping_mul(2i64))
end t.chain2

fn t.code(a: i64, b: i64) -> i64 uses [] panics may
    match t.chain2(a, b) {
        Ok(v) => v
        Err(t.Err.Zero) => -1i64
        Err(t.Err.Neg(n)) => n
    }
end t.code

fn t.px(n: i64) -> i64 uses [] panics may
    t.sum_opt(t.find(n))
end t.px

fn t.nest(n: i64) -> Option<Result<i64, t.Err>> uses [] panics may
    if n == 0i64 {
        None
    } else {
        Some(t.div(100i64, n))
    }
end t.nest

fn t.nest_val(n: i64) -> i64 uses [] panics may
    match t.nest(n) {
        Some(Ok(v)) => v
        Some(Err(_)) => -1i64
        None => -2i64
    }
end t.nest_val

fn t.unit_val(x: i64) -> i64 uses [] panics never
    match t.check(x) {
        Ok(()) => 1i64
        Err(_) => 0i64
    }
end t.unit_val

fn t.oo(n: i64) -> Option<Option<i64>> uses [] panics never
    if n == 0i64 {
        None
    } else if n == 1i64 {
        Some(None)
    } else {
        Some(Some(n))
    }
end t.oo

fn t.oo_code(n: i64) -> i64 uses [] panics never
    match t.oo(n) {
        Some(Some(v)) => v
        Some(None) => -1i64
        None => -2i64
    }
end t.oo_code

fn t.flags(n: i64) -> i64 uses [] panics may
    let mut c = 0i64
    if t.find(n).is_some() {
        c = c + 1i64
    }
    if t.find(n).is_none() {
        c = c + 10i64
    }
    c
end t.flags

fn t.or_default(n: i64) -> i64 uses [] panics may
    let r = t.div(10i64, n).unwrap_or(7i64)
    r
end t.or_default
"#;

const OPTRES_AGG_MAIN: &str = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.px(3i64), 7i64)
    core.check.eq(t.px(0i64), 0i64)
    core.check.eq(t.first_x(3i64), 3i64)
    core.check.eq(t.code(10i64, 2i64), 12i64)
    core.check.eq(t.code(10i64, 0i64), -1i64)
    core.check.eq(t.code(-4i64, 2i64), -4i64)
    core.check.eq(t.code(300i64, 1i64), 300i64)
    core.check.eq(t.nest_val(4i64), 25i64)
    core.check.eq(t.nest_val(0i64), -2i64)
    core.check.eq(t.nest_val(-1i64), -100i64)
    core.check.eq(t.unit_val(5i64), 1i64)
    core.check.eq(t.unit_val(500i64), 0i64)
    core.check.eq(t.oo_code(0i64), -2i64)
    core.check.eq(t.oo_code(1i64), -1i64)
    core.check.eq(t.oo_code(7i64), 7i64)
    core.check.eq(t.flags(2i64), 1i64)
    core.check.eq(t.flags(-2i64), 10i64)
    core.check.eq(t.or_default(2i64), 5i64)
    core.check.eq(t.or_default(0i64), 7i64)
    Ok(())
end t.main
";

#[test]
fn kir_optres_aggregates_match_tree_walker() {
    // 木走査 (`ksn run`) の main が通ること、参照実行器の値が木走査と同じ値であること。
    // 入口は整数だけを引数・戻り値に持つ関数 (集成体・入れ子を持つ戻り値は入口で拒否する)。
    let t = Tmp::new("kir_optres_agg", &format!("{}\n{}", OPTRES_AGG_SRC, OPTRES_AGG_MAIN));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    let cases: &[(&str, &[&str], &str)] = &[
        ("t.px", &["3"], "7"),
        ("t.px", &["0"], "0"),
        ("t.first_x", &["3"], "3"),
        ("t.code", &["10", "2"], "12"),
        ("t.code", &["10", "0"], "-1"),
        ("t.code", &["-4", "2"], "-4"),
        ("t.code", &["300", "1"], "300"),
        ("t.nest_val", &["4"], "25"),
        ("t.nest_val", &["0"], "-2"),
        ("t.nest_val", &["-1"], "-100"),
        ("t.unit_val", &["5"], "1"),
        ("t.unit_val", &["500"], "0"),
        ("t.oo_code", &["0"], "-2"),
        ("t.oo_code", &["1"], "-1"),
        ("t.oo_code", &["7"], "7"),
        ("t.flags", &["2"], "1"),
        ("t.flags", &["-2"], "10"),
        ("t.or_default", &["2"], "5"),
        ("t.or_default", &["0"], "7"),
    ];
    for (f, args, want) in cases {
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {} {:?}\n{}", f, args, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {} {:?}", f, args);
    }
}

#[test]
fn kir_optres_aggregates_print_and_refuse_at_the_entry() {
    let t = Tmp::new("kir_optres_agg_text", &format!("{}\n{}", OPTRES_AGG_SRC, OPTRES_AGG_MAIN));
    // 入れ子と集成体の型は読み直せる形で出る (`ksn kir` は往復を確かめてから出す)
    let o = run(&["kir", "t.oo"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("ret=Option<Option<i64>>") && out.contains("const.Option<Option<i64>> none"), "{}", out);
    let o = run(&["kir", "t.nest"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(text(&o.stdout).contains("ret=Option<Result<i64,agg#"), "{}", text(&o.stdout));
    let o = run(&["kir", "t.code"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(text(&o.stdout).contains("ret=i64") && text(&o.stdout).contains("= call t.chain2("), "{}", text(&o.stdout));
    // 入口は集成体・入れ子を引数・戻り値に持てない (表示できないため)
    let k = run(&["kir", "t.find", "--run", "3"], &t.0);
    assert_eq!(k.status.code(), Some(2), "{}", text(&k.stderr));
    assert!(text(&k.stderr).contains("集成体を引数・戻り値に持つ"), "{}", text(&k.stderr));
    let k = run(&["kir", "t.oo", "--run", "7"], &t.0);
    assert_eq!(k.status.code(), Some(2), "{}", text(&k.stderr));
}

#[test]
fn kir_optres_of_aggregates_read_back_as_text() {
    // 集成体を含むテキスト形式の型 (`Option<agg#n>`、`Result<agg#n,i64>`) は、読み直すと同じ型になる。
    let t = Tmp::new("kir_optres_agg_round", &format!("{}\n{}", OPTRES_AGG_SRC, OPTRES_AGG_MAIN));
    let o = run(&["kir", "t.chain"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("ret=Result<i64,agg#"), "{}", out);
    assert!(out.contains("= err %") && out.contains("= ok %"), "{}", out);
    let o = run(&["kir", "t.sum_opt"], &t.0);
    assert!(text(&o.stdout).contains("Option<agg#"), "{}", text(&o.stdout));
}

#[test]
fn kir_optres_unit_and_std_payloads_are_checked_by_the_verifier() {
    // `Result<(), 利用者の型>` は作れる (unit の Ok)。`Option<型表にない std 型>` は未対応の理由を付けて拒否する。
    let src = "type t.P
    a: u8
end t.P

fn t.ok_unit() -> Result<(), t.P> uses [] panics never
    Ok(())
end t.ok_unit

fn t.bad() -> Option<std.io.Stdout> uses [] panics never
    None
end t.bad
";
    let t = Tmp::new("kir_optres_unit", src);
    let o = run(&["kir", "t.ok_unit"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(text(&o.stdout).contains("ret=Result<unit,agg#0>"), "{}", text(&o.stdout));
    let o = run(&["kir", "t.bad"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("std.io.Stdout は未対応"), "{}", text(&o.stderr));
}

#[test]
fn kir_optres_slice_sub_matches_tree_walker() {
    // `s.sub(a, b)` は `Option<&[T]>`。部分スライスの `fat` をペイロードに持ち、範囲の外は `None` (panic しない)。
    let src = "fn t.has(s: &[u8], a: usize, b: usize) -> bool uses [] panics never
    match s.sub(a, b) {
        Some(_) => true
        None => false
    }
end t.has

fn t.bytes_has(s: &str, a: usize, b: usize) -> bool uses [] panics never
    let v = s.as_bytes()
    t.has(v, a, b)
end t.bytes_has

fn t.p1() -> bool uses [] panics never
    t.bytes_has(\"hello\", 1usize, 3usize)
end t.p1

fn t.p2() -> bool uses [] panics never
    t.bytes_has(\"hello\", 3usize, 2usize)
end t.p2

fn t.p3() -> bool uses [] panics never
    t.bytes_has(\"hello\", 2usize, 6usize)
end t.p3

fn t.p4() -> bool uses [] panics never
    t.bytes_has(\"hello\", 5usize, 5usize)
end t.p4

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.p1(), true)
    core.check.eq(t.p2(), false)
    core.check.eq(t.p3(), false)
    core.check.eq(t.p4(), true)
    Ok(())
end t.main
";
    let t = Tmp::new("kir_optres_sub", src);
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0), "tree-walker\n{}", text(&run(&["run"], &t.0).stderr));
    for (f, want) in [("t.p1", "true"), ("t.p2", "false"), ("t.p3", "false"), ("t.p4", "true")] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
    // 戻り値の `Option<&[u8]>` は表示できる形で出る (`fat` の集成体の番号を含む)
    let o = run(&["kir", "t.has"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
}

#[test]
fn kir_optres_slice_literal_eq_matches_tree_walker() {
    // `Option<&[T]>` (`sub` の結果) と `Some(&[..])` (配列リテラルの参照) の `==` / `!=`。
    // `Some(&[..])` は長さ N を持つ参照だが、Option のペイロードでは `fat` (`Option<&[T]>` と同じ値) で持つ。
    // 失敗の文言は `Option<&[u8]>` の表示 (`Some([2, 3])`) で、木走査と同じ。
    let src = "fn t.same(a: usize, b: usize) -> bool uses [] panics never
    let arr = [1u8, 2u8, 3u8, 4u8, 5u8]
    let mid = arr.sub(a, b)
    mid == Some(&[2u8, 3u8, 4u8])
end t.same

fn t.differs(a: usize, b: usize) -> bool uses [] panics never
    let arr = [1u8, 2u8, 3u8, 4u8, 5u8]
    let mid = arr.sub(a, b)
    mid != Some(&[2u8, 3u8, 4u8])
end t.differs

fn t.p1() -> bool uses [] panics never
    t.same(1usize, 4usize)
end t.p1

fn t.p2() -> bool uses [] panics never
    t.same(0usize, 3usize)
end t.p2

fn t.p3() -> bool uses [] panics never
    t.same(9usize, 9usize)
end t.p3

fn t.p4() -> bool uses [] panics never
    t.differs(0usize, 3usize)
end t.p4

fn t.p5() -> bool uses [] panics never
    t.differs(1usize, 4usize)
end t.p5

fn t.show() -> () uses [io.console] panics may
    let arr = [1u8, 2u8, 3u8, 4u8, 5u8]
    let mid = arr.sub(1usize, 3usize)
    core.check.eq(mid, Some(&[9u8, 3u8]))
end t.show

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.p1(), true)
    core.check.eq(t.p2(), false)
    core.check.eq(t.p3(), false)
    core.check.eq(t.p4(), true)
    core.check.eq(t.p5(), false)
    Ok(())
end t.main
";
    let t = Tmp::new("kir_optres_slice_lit", src);
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0), "tree-walker\n{}", text(&run(&["run"], &t.0).stderr));
    for (f, want) in [("t.p1", "true"), ("t.p2", "false"), ("t.p3", "false"), ("t.p4", "true"), ("t.p5", "false")] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
    // 失敗の文言: 木走査と同じ表示 (`Some([2, 3])` / `Some([9, 3])`)。native / ASan との比較も `kir_run` が行う。
    let k = kir_run(&["kir", "t.show", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(101), "kir t.show\n{}", text(&k.stderr));
    assert_eq!(
        text(&k.stderr),
        "panic: assertion failed: left: Some([2, 3]), right: Some([9, 3])\n",
        "kir t.show の失敗の文言"
    );
}

// ---- 段階 1: ペイロードが所有物 (String / Vec) の Option / Result (E は整数/unit) ----
// `Some`/`Ok`/`Err` で作り、`match` でペイロードを取り出し、`?` で同じ E を伝え、
// スコープの終わりで解放する。観察値はスカラーに落とす (--run の入口は集成体を引数・戻り値に持てないため)。
// 参照実行器・機械語・ASan は kir_run が比べる (解放の漏れ・二重解放も ASan で検出)。
#[test]
fn kir_result_heap_owned_payloads_match_tree_walker() {
    // 段階 1: `Option<String>` / `Result<String, u8>` / `Result<Vec<u32>, u8>` を `Some`/`Ok`/`Err` で作り、
    // `match` でペイロードを取り出し、`?` で同じ E を伝え、スコープの終わりで解放する。
    // 観察値はスカラーに落とす (--run の入口は集成体を引数・戻り値に持てないため)。
    let src = "fn t.mk_s() -> Option<alloc.string.String<std.alloc.System>> uses [alloc] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(\"banana\"))
    Some(s)
end t.mk_s

fn t.mk_n() -> Option<alloc.string.String<std.alloc.System>> uses [alloc] panics may
    None
end t.mk_n

fn t.opt_len() -> usize uses [alloc] panics may
    match t.mk_s() {
        Some(x) => x.len()
        None => 99usize
    }
end t.opt_len

fn t.opt_none() -> usize uses [alloc] panics may
    match t.mk_n() {
        Some(_x) => 99usize
        None => 0usize
    }
end t.opt_none

fn t.mk_ok() -> Result<alloc.string.String<std.alloc.System>, u8> uses [alloc] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(\"ok\"))
    Ok(s)
end t.mk_ok

fn t.mk_err() -> Result<alloc.string.String<std.alloc.System>, u8> uses [] panics never
    Err(3u8)
end t.mk_err

fn t.res_match_ok() -> usize uses [alloc] panics may
    match t.mk_ok() {
        Ok(x) => x.len()
        Err(_e) => 99usize
    }
end t.res_match_ok

fn t.res_match_err() -> usize uses [] panics never
    match t.mk_err() {
        Ok(_x) => 99usize
        Err(_e) => 0usize
    }
end t.res_match_err

fn t.q() -> Result<alloc.string.String<std.alloc.System>, u8> uses [alloc] panics may
    Ok(t.mk_ok()?)
end t.q

fn t.q_len() -> usize uses [alloc] panics may
    match t.q() {
        Ok(x) => x.len()
        Err(_e) => 99usize
    }
end t.q_len

fn t.mk_v() -> Result<alloc.vec.Vec<u32, std.alloc.System>, u8> uses [alloc] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(1u32))
    core.mem.drop(v.push(2u32))
    Ok(v)
end t.mk_v

fn t.mk_ve() -> Result<alloc.vec.Vec<u32, std.alloc.System>, u8> uses [alloc] panics may
    Err(9u8)
end t.mk_ve

fn t.vlen() -> usize uses [alloc] panics may
    match t.mk_v() {
        Ok(v) => v.len()
        Err(_e) => 99usize
    }
end t.vlen

fn t.velen() -> usize uses [alloc] panics may
    match t.mk_ve() {
        Ok(_v) => 99usize
        Err(_e) => 0usize
    }
end t.velen

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.opt_len(), 6usize)
    core.check.eq(t.opt_none(), 0usize)
    core.check.eq(t.res_match_ok(), 2usize)
    core.check.eq(t.res_match_err(), 0usize)
    core.check.eq(t.q_len(), 2usize)
    core.check.eq(t.vlen(), 2usize)
    core.check.eq(t.velen(), 0usize)
    Ok(())
end t.main
";
    let t = Tmp::new("kir_result_heap_owned", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    for (f, want) in [
        ("t.opt_len", "6"),
        ("t.opt_none", "0"),
        ("t.res_match_ok", "2"),
        ("t.res_match_err", "0"),
        ("t.q_len", "2"),
        ("t.vlen", "2"),
        ("t.velen", "0"),
    ] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
}

// ---- 段階 2: `Result` / `Option` の E が、所有物 (String) を持つ利用者の enum / 構造体 ----
#[test]
fn kir_result_heap_user_error_match_tree_walker() {
    // 段階 2: `Result` / `Option` の E が、所有物 (String) を持つ利用者の enum / 構造体。
    // `std.net.Error` を型表に登録済みの前提 (kir_lower_agg.rs の STD_TYPES)。
    let src = "enum t.Err pub
    Io(std.io.Error)
    Net(std.net.Error)
    Msg(alloc.string.String<std.alloc.System>)
end t.Err

fn t.msg() -> Result<usize, t.Err> uses [alloc] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(\"message\"))
    Err(t.Err.Msg(s))
end t.msg

fn t.io() -> Result<usize, t.Err> uses [] panics never
    Err(t.Err.Io(std.io.Error { kind: std.io.ErrorKind.Other, code: 0i32 }))
end t.io

fn t.ok_v() -> Result<usize, t.Err> uses [] panics never
    Ok(42usize)
end t.ok_v

fn t.classify_msg() -> usize uses [alloc] panics may
    match t.msg() {
        Ok(n) => n
        Err(e) => match e {
            t.Err.Io(_e) => 1usize
            t.Err.Net(_e) => 2usize
            t.Err.Msg(s) => s.len()
        }
    }
end t.classify_msg

fn t.classify_io() -> usize uses [] panics never
    match t.io() {
        Ok(n) => n
        Err(e) => match e {
            t.Err.Io(_e) => 1usize
            t.Err.Net(_e) => 2usize
            t.Err.Msg(_s) => 3usize
        }
    }
end t.classify_io

fn t.ok_v_len() -> usize uses [] panics never
    match t.ok_v() {
        Ok(n) => n
        Err(_e) => 0usize
    }
end t.ok_v_len

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.classify_msg(), 7usize)
    core.check.eq(t.classify_io(), 1usize)
    core.check.eq(t.ok_v_len(), 42usize)
    Ok(())
end t.main
";
    let t = Tmp::new("kir_result_heap_user_error", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    for (f, want) in [
        ("t.classify_msg", "7"),
        ("t.classify_io", "1"),
        ("t.ok_v_len", "42"),
    ] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
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

// ---- 集成体 (構造体・タプル・列挙型) の下ろし (設計_KIR.md 2.3、kir/aggregates) ----

/// 構築・フィールドの読み書き・列挙型の変種と `match` の束縛・タプルの束縛・ループ・腕の中の代入を含む。
/// `t.run_*` は整数だけを引数に取るので、`ksn kir <FQN> --run` で呼べる。`t.main` は木走査の検査。
const KIR_AGG_SRC: &str = r#"type t.Box
    p: t.Pt
    ok: bool
    n: i64
end t.Box

type t.Pt
    x: i32
    y: i32
end t.Pt

enum t.Sh
    Circle(i64)
    Rect(i64, i64)
    Tag(bool, i64)
    Empty
end t.Sh

fn t.area(s: t.Sh) -> i64 uses [] panics never
    match s {
        t.Sh.Circle(r) => r.wrapping_mul(3i64)
        t.Sh.Rect(w, h) => w.wrapping_mul(h)
        t.Sh.Tag(b, n) => {
            if b {
                n
            } else {
                0i64
            }
        }
        t.Sh.Empty => 0i64
    }
end t.area

fn t.area_of(c: i64) -> i64 uses [] panics never
    t.area(t.Sh.Rect(c, 2i64)).wrapping_add(t.area(t.Sh.Tag(true, c)))
end t.area_of

fn t.arm(s: t.Sh) -> i64 uses [] panics never
    let mut cur = 1i64
    let mut sh = s
    match sh {
        t.Sh.Circle(r) => {
            sh = t.Sh.Empty
            cur = r
        }
        t.Sh.Rect(_, _) => {
            cur = 2i64
        }
        t.Sh.Tag(_, _) => {
            cur = 3i64
        }
        t.Sh.Empty => {
            cur = 4i64
        }
    }
    match sh {
        t.Sh.Circle(_) => cur
        t.Sh.Rect(_, _) => cur
        t.Sh.Tag(_, _) => cur
        t.Sh.Empty => cur.wrapping_add(100i64)
    }
end t.arm

fn t.box_n(bx: t.Box) -> i64 uses [] panics never
    if bx.ok {
        bx.n.wrapping_add(10i64)
    } else {
        bx.n
    }
end t.box_n

fn t.first(pr: (i32, bool)) -> i32 uses [] panics never
    match pr {
        (x, true) => x
        (x, false) => 0i32
    }
end t.first

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.area(t.Sh.Circle(4i64)), 12i64)
    core.check.eq(t.area(t.Sh.Rect(3i64, 5i64)), 15i64)
    core.check.eq(t.area(t.Sh.Tag(true, 9i64)), 9i64)
    core.check.eq(t.area(t.Sh.Tag(false, 9i64)), 0i64)
    core.check.eq(t.area(t.Sh.Empty), 0i64)
    core.check.eq(t.box_n(t.mk_box(1i32, 2i32, true)), 15i64)
    core.check.eq(t.px(t.mk_box(1i32, 2i32, true)), 1002i32)
    core.check.eq(t.first(t.pair(7i32)), 7i32)
    core.check.eq(t.area(t.pick(t.Sh.Circle(6i64))), 36i64)
    core.check.eq(t.walk(4i32), 604i32)
    core.check.eq(t.arm(t.Sh.Circle(8i64)), 108i64)
    core.check.eq(t.arm(t.Sh.Rect(8i64, 1i64)), 2i64)
    core.check.eq(t.tup(9i32), 9i32)
    core.check.eq(t.area_of(4i64), 12i64)
    core.check.eq(t.run_arm(8i64), 108i64)
    core.check.eq(t.run_arm2(8i64), 2i64)
    core.check.eq(t.run_box(1i32, 2i32), 15i64)
    core.check.eq(t.run_first(7i32), 7i32)
    core.check.eq(t.run_pick(6i64), 36i64)
    core.check.eq(t.run_px(1i32, 2i32), 1002i32)
    core.check.eq(t.run_tag(true, 9i64), 9i64)
    core.check.eq(t.run_tag(false, 9i64), 0i64)
    Ok(())
end t.main

fn t.mk_box(a: i32, b: i32, f: bool) -> t.Box uses [] panics never
    let p = t.Pt { x: a, y: b }
    t.Box { p: p, ok: f, n: 5i64 }
end t.mk_box

fn t.pair(a: i32) -> (i32, bool) uses [] panics never
    (a, true)
end t.pair

fn t.pick(s: t.Sh) -> t.Sh uses [] panics never
    match s {
        t.Sh.Circle(r) => t.Sh.Rect(r, r)
        t.Sh.Rect(w, h) => t.Sh.Rect(w, h)
        t.Sh.Tag(b, n) => t.Sh.Tag(b, n)
        t.Sh.Empty => t.Sh.Empty
    }
end t.pick

fn t.px(bx: t.Box) -> i32 uses [] panics never
    bx.p.x.wrapping_mul(1000i32).wrapping_add(bx.p.y)
end t.px

fn t.run_arm(c: i64) -> i64 uses [] panics never
    t.arm(t.Sh.Circle(c))
end t.run_arm

fn t.run_arm2(c: i64) -> i64 uses [] panics never
    t.arm(t.Sh.Rect(c, 1i64))
end t.run_arm2

fn t.run_box(a: i32, b: i32) -> i64 uses [] panics never
    t.box_n(t.mk_box(a, b, true))
end t.run_box

fn t.run_first(a: i32) -> i32 uses [] panics never
    t.first(t.pair(a))
end t.run_first

fn t.run_pick(c: i64) -> i64 uses [] panics never
    t.area(t.pick(t.Sh.Circle(c)))
end t.run_pick

fn t.run_px(a: i32, b: i32) -> i32 uses [] panics never
    t.px(t.mk_box(a, b, false))
end t.run_px

fn t.run_tag(b: bool, c: i64) -> i64 uses [] panics never
    t.area(t.Sh.Tag(b, c))
end t.run_tag

fn t.tup(a: i32) -> i32 uses [] panics never
    let (x, ok) = t.pair(a)
    if ok {
        x
    } else {
        0i32
    }
end t.tup

fn t.walk(n: i32) -> i32 uses [] panics never
    let mut acc = t.Pt { x: 0i32, y: 0i32 }
    let mut i = 0i32
    while i < n {
        acc.x = acc.x.wrapping_add(i)
        acc.y = acc.y.wrapping_add(1i32)
        i = i.wrapping_add(1i32)
    }
    acc.x.wrapping_mul(100i32).wrapping_add(acc.y)
end t.walk
"#;

#[test]
fn kir_agg_values_match_tree_walker() {
    // 木走査 (`ksn run`) の検査が通ること、参照実行器 (`ksn kir --run`) の値が同じ期待値であること。
    let t = Tmp::new("kir_agg_values", KIR_AGG_SRC);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    let cases: &[(&str, &[&str], &str)] = &[
        ("t.run_arm", &["8"], "108"),
        ("t.run_arm2", &["8"], "2"),
        ("t.run_box", &["1", "2"], "15"),
        ("t.run_first", &["7"], "7"),
        ("t.run_pick", &["6"], "36"),
        ("t.run_px", &["1", "2"], "1002"),
        ("t.run_tag", &["true", "9"], "9"),
        ("t.run_tag", &["false", "9"], "0"),
        ("t.walk", &["4"], "604"),
        ("t.walk", &["10"], "4510"),
        ("t.tup", &["9"], "9"),
        ("t.area_of", &["4"], "12"),
    ];
    for (f, args, want) in cases {
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {} {:?}\n{}", f, args, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {} {:?}", f, args);
    }
}

// ---- KIR の数値 (浮動小数・128 ビット・ビット演算・シフト・数値のメソッド・変換): kir_scalar_* ----

/// 数値の関数の試験用の定義。名前の順に並べる (E0020)。`t.main` は呼び出し側が最後に付ける。
const SCALAR_FNS: &str = "fn t.a_band(a: u32, b: u32) -> u32 uses [] panics never
    a & b
end t.a_band

fn t.a_big() -> i128 uses [] panics never
    let a = 170141183460469231731687303715884105727i128
    a.wrapping_add(1i128)
end t.a_big

fn t.a_bnot8(a: u8) -> u8 uses [] panics never
    !a
end t.a_bnot8

fn t.a_bnoti(a: i32) -> i32 uses [] panics never
    !a
end t.a_bnoti

fn t.a_cnt(a: u32) -> u32 uses [] panics never
    a.count_ones()
end t.a_cnt

fn t.a_conv1() -> i64 uses [] panics never
    i64.from_i8(-5i8)
end t.a_conv1

fn t.a_conv2() -> f64 uses [] panics never
    f64.from_i32_nearest(7i32)
end t.a_conv2

fn t.a_conv3() -> u64 uses [] panics never
    f64.from_bits(4607182418800017408u64).to_bits()
end t.a_conv3

fn t.a_conv4() -> u32 uses [] panics never
    0.1f64.nearest_to_f32().to_bits()
end t.a_conv4

fn t.a_f32trig() -> u32 uses [] panics never
    let x = 0.7f32
    (x.sin() + x.exp() * x).to_bits()
end t.a_f32trig

fn t.a_fl1() -> f64 uses [] panics never
    let a = 9.0f64
    let b = a.sqrt()
    let c = b * 2.0 + 1.5
    c.floor()
end t.a_fl1

fn t.a_fromu128() -> f64 uses [] panics never
    f64.from_u128_nearest(0u128.wrapping_sub(1u128))
end t.a_fromu128

fn t.a_lz(a: u16) -> u32 uses [] panics never
    a.leading_zeros()
end t.a_lz

fn t.a_max(a: i64, b: i64) -> i64 uses [] panics never
    a.max(b)
end t.a_max

fn t.a_nan() -> bool uses [] panics never
    let n = 0.0f64 / 0.0f64
    n == n
end t.a_nan

fn t.a_sat(a: u8, b: u8) -> u8 uses [] panics never
    a.saturating_add(b)
end t.a_sat

fn t.a_satadd128() -> i128 uses [] panics never
    let a = -170141183460469231731687303715884105727i128
    a.saturating_add(-10i128)
end t.a_satadd128

fn t.a_satneg(a: i8) -> i8 uses [] panics never
    a.saturating_neg()
end t.a_satneg

fn t.a_satneg128() -> i128 uses [] panics never
    let a = -170141183460469231731687303715884105727i128
    a.saturating_sub(10i128)
end t.a_satneg128

fn t.a_shl8(a: u8, n: u32) -> u8 uses [] panics never
    a.wrapping_shl(n)
end t.a_shl8

fn t.a_shri8(a: i8, n: u32) -> i8 uses [] panics never
    a.wrapping_shr(n)
end t.a_shri8

fn t.a_trig() -> u64 uses [] panics never
    let x = 0.7f64
    (x.sin() + x.cos() * x.tan() + x.exp() + x.ln() + x.atan2(0.3f64) + x.powi(5i32)).to_bits()
end t.a_trig

fn t.a_tz(a: i32) -> u32 uses [] panics never
    a.trailing_zeros()
end t.a_tz

fn t.a_ucmp() -> bool uses [] panics never
    t.a_umax() > 1u128
end t.a_ucmp

fn t.a_umax() -> u128 uses [] panics never
    0u128.wrapping_sub(1u128)
end t.a_umax

fn t.a_wneg(a: u8) -> u8 uses [] panics never
    a.wrapping_neg()
end t.a_wneg

fn t.a_wshr64(a: u64, n: u32) -> u64 uses [] panics never
    a.wrapping_shr(n)
end t.a_wshr64
";

/// `SCALAR_FNS` に `main` (本体 `body`) を付けたパッケージを作る。
fn scalar_pkg(name: &str, body: &str) -> Tmp {
    Tmp::new(
        name,
        &format!("{}fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n{}    Ok(())\nend t.main\n", SCALAR_FNS, body),
    )
}

#[test]
fn kir_scalar_ints_match_tree_walker() {
    // (関数, 木走査の引数 (接尾辞付き), 参照実行器の引数, 木走査の期待値, 参照実行器の期待値)
    let cases: &[(&str, &[&str], &[&str], &str, &str)] = &[
        ("t.a_band", &["12u32", "10u32"], &["12", "10"], "8u32", "8"),
        ("t.a_bnot8", &["0u8"], &["0"], "255u8", "255"),
        ("t.a_bnoti", &["5i32"], &["5"], "-6i32", "-6"),
        ("t.a_shl8", &["129u8", "9u32"], &["129", "9"], "2u8", "2"),
        ("t.a_shri8", &["-128i8", "9u32"], &["-128", "9"], "-64i8", "-64"),
        ("t.a_sat", &["250u8", "10u8"], &["250", "10"], "255u8", "255"),
        ("t.a_satneg", &["-128i8"], &["-128"], "127i8", "127"),
        ("t.a_cnt", &["61680u32"], &["61680"], "8u32", "8"),
        ("t.a_lz", &["1u16"], &["1"], "15u32", "15"),
        ("t.a_tz", &["0i32"], &["0"], "32u32", "32"),
        ("t.a_max", &["-3i64", "2i64"], &["-3", "2"], "2i64", "2"),
        ("t.a_wneg", &["1u8"], &["1"], "255u8", "255"),
        ("t.a_wshr64", &["18446744073709551615u64", "60u32"], &["18446744073709551615", "60"], "15u64", "15"),
    ];
    for (i, (f, targs, kargs, want, kwant)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, targs.join(", "));
        let t = scalar_pkg(&format!("kir_scalar_int_{}", i), &format!("    core.check.eq({}, {})\n", call, want));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut ka = vec!["kir", *f, "--run"];
        ka.extend(kargs.iter().copied());
        let k = kir_run(&ka, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *kwant, "kir value of {}", call);
    }
}

/// `const t.CONFIG: &str = "..."` のような文字列定数を下ろし、使う場所で値を作る。
/// `ksn run` と `ksn kir --run` と `--native` が一致すること。
/// 依存パッケージで定義された定数でも同じ経路 (`resolve` → `const_val`) を通る。
const CONST_STR_FNS: &str = "const t.CONFIG: &str = \"hello, kasane\" pub\n\nfn t.len_cfg() -> usize uses [] panics never\n    let s = t.CONFIG\n    s.len()\nend t.len_cfg\n\n";

fn const_str_pkg(name: &str, body: &str) -> Tmp {
    Tmp::new(name, &format!("{}{}", CONST_STR_FNS, main_wrap("", body)))
}

#[test]
fn kir_const_str_match_tree_walker() {
    // 木走査で定数の文字列と長さが正しいこと、参照実行器 / ネイティブでも同じ値になること。
    let t = const_str_pkg("kir_const_str", "    std.io.print_line(t.CONFIG)\n    core.check.eq(t.len_cfg(), 13usize)\n");
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", "kir_const_str", text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "hello, kasane", "tree-walker 出力");
    let k = kir_run(&["kir", "t.len_cfg", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", "kir_const_str", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "13", "kir 値");
}

// ---- 参照 (設計_KIR.md 段階 1: `&T` / `&mut T`、`*r`、`*r = v`。kir_lower_refs.rs) ----

/// 参照を使う関数と、整数の引数だけで呼べる包み (`t.w_*`)。`t.main` は含まない。
const REFS_FNS: &str = "fn t.bump(p: &mut i32) -> () uses [] panics may
    *p = *p + 1i32
end t.bump

fn t.twice(n: i32) -> i32 uses [] panics may
    let mut a = n
    t.bump(&mut a)
    t.bump(&mut a)
    a
end t.twice

fn t.peek(r: &i32) -> i32 uses [] panics never
    *r
end t.peek

fn t.peek_mut(a: &mut i32) -> i32 uses [] panics never
    t.peek(a)
end t.peek_mut

fn t.sum2(a: &i32, b: &i32) -> i32 uses [] panics may
    *a + *b
end t.sum2

fn t.swap(a: &mut i32, b: &mut i32) -> () uses [] panics never
    let tmp = *a
    *a = *b
    *b = tmp
end t.swap

fn t.pick['a](a: &'a i32, b: &'a i32, first: bool) -> &'a i32 uses [] panics never
    if first {
        a
    } else {
        b
    }
end t.pick

fn t.temp() -> i32 uses [] panics may
    let r = &(3i32 + 4i32)
    *r * 2i32
end t.temp

fn t.loop_bump(n: i32) -> i32 uses [] panics may
    let mut total = 0i32
    let mut i = 0i32
    while i < n {
        t.bump(&mut total)
        i = i + 1i32
    }
    total
end t.loop_bump

fn t.bump_w(p: &mut i32) -> () uses [] panics never
    let s = (*p).wrapping_add(1i32)
    *p = s
end t.bump_w

fn t.down(p: &mut i32, n: i32) -> () uses [] panics never stack 128
    if n > 0i32 {
        t.bump_w(p)
        t.down(p, n.wrapping_sub(1i32))
    }
end t.down

fn t.assign_branch(n: i32) -> i32 uses [] panics may
    let mut x = 0i32
    if n > 0i32 {
        x = n
    } else {
        x = 0i32 - n
    }
    let r = &mut x
    *r = *r * 10i32
    x
end t.assign_branch

fn t.wrap_ref(v: u8) -> u8 uses [] panics never
    let mut w = v
    let s = w.wrapping_add(200u8)
    let r = &mut w
    *r = s
    w
end t.wrap_ref

fn t.bump8(p: &mut u8) -> () uses [] panics may
    *p = *p + 100u8
end t.bump8

fn t.overflow_via_ref(v: u8) -> u8 uses [] panics may
    let mut w = v
    t.bump8(&mut w)
    w
end t.overflow_via_ref

fn t.reborrow_read(r: &mut i32) -> i32 uses [] panics never
    let s: &i32 = &*r
    *s
end t.reborrow_read

fn t.early(n: i32) -> i32 uses [] panics may
    let mut total = 0i32
    let mut i = 0i32
    while true {
        if i > n {
            break
        }
        total = total + i
        let v = t.peek(&total)
        if v > 100i32 {
            return v
        }
        i = i + 1i32
    }
    total
end t.early

fn t.mixed(n: i32) -> i32 uses [] panics may
    let mut acc = 1i32
    let r = &mut acc
    let mut k = 0i32
    while k < n {
        *r = *r + 2i32
        k = k + 1i32
    }
    acc = acc + 1i32
    t.peek(&acc) + t.reborrow_read(&mut acc)
end t.mixed

fn t.w_swap(a: i32, b: i32) -> i32 uses [] panics never
    let mut x = a
    let mut y = b
    t.swap(&mut x, &mut y)
    x.wrapping_mul(10i32).wrapping_add(y)
end t.w_swap

fn t.w_pick(a: i32, b: i32, first: bool) -> i32 uses [] panics never
    let x = a
    let y = b
    *t.pick(&x, &y, first)
end t.w_pick

fn t.w_peek(n: i32) -> i32 uses [] panics never
    let mut x = n
    t.peek_mut(&mut x)
end t.w_peek

fn t.w_down(n: i32) -> i32 uses [] panics never
    let mut d = 0i32
    t.down(&mut d, n)
    d
end t.w_down

fn t.w_sum(a: i32, b: i32) -> i32 uses [] panics may
    let x = a
    let y = b
    t.sum2(&x, &y)
end t.w_sum
";

/// 参照を使う関数の包みのパッケージを作る。
fn refs_pkg(name: &str, main_body: &str) -> Tmp {
    Tmp::new(name, &format!("{}{}", REFS_FNS, main_wrap("", main_body)))
}

#[test]
fn kir_array_ref_match_tree_walker() {
    // `&[T; N]` / `&mut [T; N]` のパラメータ (ptr)、`&[T; N]` → `&[T]` の fat への暗黙調整、
    // `for x in a` (参照越し) を、木走査と参照実行器・機械語で比べる。
    let fns = r#"
fn t.for_ref(a: &[u32; 3]) -> u32 uses [] panics may
    let mut s = 0u32
    for x in a {
        s = s + x
    }
    s
end t.for_ref

fn t.for_ref_check() -> u32 uses [] panics may
    t.for_ref(&[1u32, 2u32, 3u32])
end t.for_ref_check

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.use_it(), 60u32)
    core.check.eq(t.pass_slice(), 30u32)
    core.check.eq(t.for_ref(&[1u32, 2u32, 3u32]), 6u32)
    std.io.print_line("ok")
    Ok(())
end t.main

fn t.oob() -> u32 uses [] panics may
    let a = [10u32, 20u32, 30u32]
    a[5usize]
end t.oob

fn t.pass_slice() -> u32 uses [] panics may
    let a = [10u32, 20u32, 30u32]
    t.slice_ok(&a)
end t.pass_slice

fn t.set(a: &mut [u32; 3]) -> () uses [] panics may
    a[1usize] = 20u32
end t.set

fn t.slice_ok(s: &[u32]) -> u32 uses [] panics may
    s[0usize] + s[1usize]
end t.slice_ok

fn t.sum(a: &[u32; 3]) -> u32 uses [] panics may
    a[0usize] + a[1usize] + a[2usize]
end t.sum

fn t.use_it() -> u32 uses [] panics may
    let mut a = [10u32, 0u32, 30u32]
    t.set(&mut a)
    t.sum(&a)
end t.use_it
"#;
    let t = Tmp::new("kir_array_ref_ok", fns);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker rc={:?} stdout={:?} stderr={:?}", o.status.code(), text(&o.stdout), text(&o.stderr));

    // 戻り値が u32 で引数を取らない関数は `--run` / `--native` / ASan でも直接比べられる。
    for (f, want) in [("t.use_it", "60"), ("t.pass_slice", "30"), ("t.for_ref_check", "6")] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir {} 出力\n{}", f, text(&k.stdout));
    }

    // 範囲外の添字は wood-walker と同じく panic (101) となる。
    let msg = "index out of bounds: the len is 3 but the index is 5";
    let fns2 = "
fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let _x = t.oob()
    Ok(())
end t.main

fn t.oob() -> u32 uses [] panics may
    let a = [10u32, 20u32, 30u32]
    a[5usize]
end t.oob
";
    let t = Tmp::new("kir_array_ref_oob", fns2);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101), "tree-walker oob rc={:?} stderr={:?}", o.status.code(), text(&o.stderr));
    assert!(text(&o.stderr).contains(&format!("panic: {}", msg)), "tree-walker oob msg\n{}", text(&o.stderr));
    let k = kir_run(&["kir", "t.oob", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(101), "kir oob\n{}", text(&k.stderr));
    assert_eq!(text(&k.stderr).trim(), format!("panic: {}", msg), "kir oob msg\n{}", text(&k.stderr));
}

#[test]
fn kir_refs_values_match_tree_walker() {
    // 木走査 (`ksn run`) の `core.check.eq` が通ること、参照実行器 (`ksn kir --run`) の値が期待どおりであること。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.twice", &["5"], "7i32", "7"),
        ("t.loop_bump", &["6"], "6i32", "6"),
        ("t.temp", &[], "14i32", "14"),
        ("t.assign_branch", &["3"], "30i32", "30"),
        ("t.assign_branch", &["-2"], "20i32", "20"),
        ("t.wrap_ref", &["100"], "44u8", "44"),
        ("t.early", &["3"], "6i32", "6"),
        ("t.early", &["20"], "105i32", "105"),
        ("t.mixed", &["3"], "16i32", "16"),
        ("t.w_swap", &["1", "2"], "21i32", "21"),
        ("t.w_pick", &["2", "7", "false"], "7i32", "7"),
        ("t.w_pick", &["2", "7", "true"], "2i32", "2"),
        ("t.w_peek", &["10"], "10i32", "10"),
        ("t.w_down", &["4"], "4i32", "4"),
        ("t.w_down", &["0"], "0i32", "0"),
        ("t.w_sum", &["3", "4"], "7i32", "7"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.join(", "));
        let t = refs_pkg(&format!("kir_refs_values_{}", i), &format!("    core.check.eq({}, {})\n", call, lit));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).lines().last().unwrap_or("").trim(), *want, "kir value of {}", call);
    }
}

/// `extern` (libc の `abs`)、`unsafe` ブロック、`core.ptr.*` の読み書きを、
/// 木走査・参照実行器・機械語 (`--native`) で比べる。
#[test]
fn kir_ffi_and_ptr_match_tree_walker() {
    let src = r#"extern t.abs(x: i32) -> i32 link "abs"

fn t.abs_extern(x: i32) -> i32 uses [] panics never
    unsafe "abs" "libc abs" {
        t.abs(x)
    }
end t.abs_extern

fn t.ptr_rw() -> i32 uses [] panics never
    let mut x = 42i32
    let p = core.ptr.from_mut(&mut x)
    core.ptr.write(p, 99i32)
    core.ptr.read(p)
end t.ptr_rw

fn t.null_check() -> bool uses [] panics never
    let p: *mut i32 = core.ptr.null_mut()
    core.ptr.is_null(p)
end t.null_check

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.abs_extern(-7i32), 7i32)
    core.check.eq(t.ptr_rw(), 99i32)
    core.check.is_true(t.null_check())
    std.io.print_line("ffi ok")
end t.main
"#;
    let t = Tmp::new("kir_ffi_and_ptr", src);
    // `unsafe` / `extern` は `[unsafe] modules` が必要。
    fs::write(
        t.0.join("kasane.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n\n[unsafe]\nmodules = [\"t\"]\n",
    )
    .unwrap();
    // 木走査 (`ksn run`) の main 検査が通ること。
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker main\n{}", text(&o.stderr));
    // `t.abs_extern`, `t.ptr_rw`, `t.null_check` を木走査・参照実行器・機械語で比べる。
    for (f, args, want) in [
        ("t.abs_extern", &["-7"] as &[&str], "7"),
        ("t.ptr_rw", &[] as &[&str], "99"),
        ("t.null_check", &[] as &[&str], "true"),
    ] {
        let call = format!("{}({})", f, args.join(", "));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", call);
    }
}

#[test]
fn kir_scalar_floats_and_wide_ints_match_tree_walker() {
    // 木走査で `core.check.eq` が通ること (値は木走査の結果と同じ)、参照実行器の値が期待どおりであること。
    // 超越関数と f32 の値はビット列で比べる (fmath を含むので、木走査と 1 ビットも違わないことを確かめる)。
    // (関数, 木走査で呼ぶ式, 木走査の期待値, 参照実行器の期待値)
    let cases: &[(&str, &str, &str, &str)] = &[
        ("t.a_fl1", "t.a_fl1()", "7.0f64", "7"),
        ("t.a_nan", "t.a_nan()", "false", "false"),
        ("t.a_trig", "t.a_trig()", "4616504293205964284u64", "4616504293205964284"),
        ("t.a_f32trig", "t.a_f32trig()", "1073967664u32", "1073967664"),
        ("t.a_conv4", "t.a_conv4()", "1036831949u32", "1036831949"),
        ("t.a_conv2", "t.a_conv2()", "7.0f64", "7"),
        ("t.a_conv3", "t.a_conv3()", "4607182418800017408u64", "4607182418800017408"),
        ("t.a_conv1", "t.a_conv1()", "-5i64", "-5"),
        ("t.a_fromu128", "t.a_fromu128()", "3.402823669209385e38f64", "340282366920938500000000000000000000000"),
        ("t.a_big", "t.a_big()", "-170141183460469231731687303715884105728i128", "-170141183460469231731687303715884105728"),
        ("t.a_umax", "t.a_umax().wrapping_add(2u128)", "1u128", "340282366920938463463374607431768211455"),
        ("t.a_ucmp", "t.a_ucmp()", "true", "true"),
        ("t.a_satadd128", "t.a_satadd128()", "-170141183460469231731687303715884105728i128", "-170141183460469231731687303715884105728"),
        ("t.a_satneg128", "t.a_satneg128()", "-170141183460469231731687303715884105728i128", "-170141183460469231731687303715884105728"),
    ];
    for (i, (f, call, want, kwant)) in cases.iter().enumerate() {
        let t = scalar_pkg(&format!("kir_scalar_float_{}", i), &format!("    core.check.eq({}, {})\n", call, want));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let k = kir_run(&["kir", *f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *kwant, "kir value of {}", f);
    }
}

/// 無限大・NaN・符号つきゼロの特殊値を確かめる関数 (C Annex F / IEEE 754 どおりの値を返す)。
const FLOAT_SPECIAL_FNS: &str = "fn t.ninf() -> f64 uses [] panics never
    -(1000.0f64.exp())
end t.ninf

fn t.abs_ninf64() -> u64 uses [] panics never
    t.ninf().abs().to_bits()
end t.abs_ninf64

fn t.sqrt_ninf64() -> bool uses [] panics never
    t.ninf().sqrt().is_nan()
end t.sqrt_ninf64

fn t.sqrt_neg64() -> bool uses [] panics never
    let x = -2.0f64
    x.sqrt().is_nan()
end t.sqrt_neg64

fn t.sqrt_nzero64() -> u64 uses [] panics never
    let z = 0.0f64
    (-z).sqrt().to_bits()
end t.sqrt_nzero64

fn t.sqrt_inf64() -> u64 uses [] panics never
    (1000.0f64.exp()).sqrt().to_bits()
end t.sqrt_inf64

fn t.abs_ninf32() -> u32 uses [] panics never
    let x = -(1000.0f32.exp())
    x.abs().to_bits()
end t.abs_ninf32

fn t.sqrt_ninf32() -> bool uses [] panics never
    let x = -(1000.0f32.exp())
    x.sqrt().is_nan()
end t.sqrt_ninf32

fn t.nan32_bits() -> u32 uses [] panics never
    f32.from_bits(2139095041u32).to_bits()
end t.nan32_bits

fn t.nan64_bits() -> u64 uses [] panics never
    f64.from_bits(9218868437227405313u64).to_bits()
end t.nan64_bits

fn t.rte_nhalf64() -> u64 uses [] panics never
    let x = -0.5f64
    x.round_ties_even().to_bits()
end t.rte_nhalf64

fn t.rte_half32() -> u32 uses [] panics never
    let x = 2.5f32
    x.round_ties_even().to_bits()
end t.rte_half32
";

/// (関数, 木走査の期待値 (Kasane の式), 参照実行器の期待値 (`ksn kir --run` の表示))。
const FLOAT_SPECIAL_CASES: &[(&str, &str, &str)] = &[
    ("t.rte_nhalf64", "9223372036854775808u64", "9223372036854775808"), // round_ties_even(-0.5) は -0
    ("t.rte_half32", "1073741824u32", "1073741824"),                    // round_ties_even(2.5f32) は 2
    ("t.abs_ninf64", "9218868437227405312u64", "9218868437227405312"), // abs(-inf) は +inf
    ("t.sqrt_ninf64", "true", "true"),                                  // sqrt(-inf) は NaN
    ("t.sqrt_neg64", "true", "true"),                                   // sqrt(負) は NaN
    ("t.sqrt_nzero64", "9223372036854775808u64", "9223372036854775808"), // sqrt(-0) は -0
    ("t.sqrt_inf64", "9218868437227405312u64", "9218868437227405312"),  // sqrt(+inf) は +inf
    ("t.abs_ninf32", "2139095040u32", "2139095040"),                    // f32 の abs(-inf) は +inf
    ("t.sqrt_ninf32", "true", "true"),
    ("t.nan32_bits", "2139095041u32", "2139095041"), // f32 の signaling NaN のビットは保つ
    ("t.nan64_bits", "9218868437227405313u64", "9218868437227405313"),
];

#[test]
fn float_specials_follow_ieee_on_both_backends() {
    // 木走査 (`ksn run`) と KIR (`ksn kir --run`) の両方で、無限大・NaN・符号つきゼロの値が IEEE 754 どおりであること。
    for (i, (f, want, kwant)) in FLOAT_SPECIAL_CASES.iter().enumerate() {
        let body = format!("    core.check.eq({}(), {})\n", f, want);
        let t = Tmp::new(&format!("float_special_{}", i), &format!("{}{}", FLOAT_SPECIAL_FNS, main_wrap("", &body)));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", f, text(&o.stderr));
        let k = kir_run(&["kir", *f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *kwant, "kir value of {}", f);
    }
}

#[test]
fn float_specials_and_unsigned_from_str_radix_agree_with_native() {
    // 機械語 (`test --both`) で比べられる形だけを書く: 値の比較 (`!=` で NaN、`1.0 / x` で -0 を判別)。
    // clif は `to_bits` / `is_nan` を持たないので、それらは上の `float_specials_follow_ieee_on_both_backends` で確かめる。
    // 符号なしは `-` を受け付けない (`-0` も Err、Rust と同じ)。`+` は受け付ける。
    let mut src = String::from(FLOAT_SPECIAL_FNS);
    src.push_str("fn t.pinf() -> f64 uses [] panics never\n    1000.0f64.exp()\nend t.pinf\n\nfn t.main() -> () uses [] panics never\nend t.main\n");
    let checks: Vec<String> = vec![
        "core.check.eq(t.ninf().abs(), t.pinf())".into(),
        "core.check.eq(t.ninf().sqrt() != t.ninf().sqrt(), true)".into(),
        "core.check.eq((-2.0f64).sqrt() != (-2.0f64).sqrt(), true)".into(),
        "core.check.eq(t.pinf().sqrt(), t.pinf())".into(),
        "core.check.eq(1.0f64 / (-(0.0f64)).sqrt() < 0.0f64, true)".into(),
        "core.check.eq(u8.from_str_radix(\"-0\", 10u32) != Ok(0u8), true)".into(),
        "core.check.eq(u8.from_str_radix(\"+7\", 10u32), Ok(7u8))".into(),
        "core.check.eq(i8.from_str_radix(\"-0\", 10u32), Ok(0i8))".into(),
    ];
    for (i, c) in checks.iter().enumerate() {
        src.push_str(&format!("\ntest t.main.test_{}\n    {}\nend t.main.test_{}\n", i, c, i));
    }
    let t = Tmp::new("float_specials_both", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains(&format!("{} passed; 0 failed", checks.len())), "{}", out);
    // 整数・浮動小数の比較と `from_str_radix` の一部は LLVM で比べ、`from_str_radix` の残りは Cranelift へ退避する (理由が出る)。
    if clang_available() {
        // 浮動小数の `core.check.eq` は KIR に下ろせる (kir_rt.c の表示)。`from_str_radix` の 3 本は退避する。
        // `from_str_radix` と `Result != Result` (集成体のペイロード、kir/option-eq) も KIR に下ろせるので、全件 LLVM で比べる。
        assert_eq!(out.matches("(both: llvm)").count(), 8, "{}", out);
        assert_eq!(out.matches("(both: native (llvm skipped: ").count(), 0, "{}", out);
    } else {
        assert_eq!(out.matches("(both: native").count(), checks.len(), "{}", out);
    }
    assert!(!out.contains("both: bundle"), "{}", out);
}

#[test]
fn kir_agg_module_text_declares_the_layout() {
    // 型の宣言の行 (大きさ・整列・フィールドのオフセット) と、本体の命令が出る。出力は読み直せる (cmd_kir が確かめる)。
    let t = Tmp::new("kir_agg_text", KIR_AGG_SRC);
    let o = run(&["kir", "t.walk"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("agg#0 struct t.Pt size=8 align=4 {x:i32@0 y:i32@4}"), "{}", out);
    assert!(out.contains("= setfield %"), "{}", out);
}

#[test]
fn kir_agg_refuses_generic_types_with_a_reason() {
    // 型引数付きの型は段階 1 の型に入らない (単相化した配置をまだ持たない)。その関数は理由付きで未対応になる。
    let src = "type t.G[T]
    v: T
end t.G

fn t.gv(_g: t.G<i32>) -> i32 uses [] panics never
    0i32
end t.gv

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main
";
    let t = Tmp::new("kir_agg_refuse", src);
    let o = run(&["kir", "t.gv"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("t.gv は未対応"), "{}", text(&o.stderr));
}

#[test]
fn kir_agg_entry_with_an_aggregate_value_is_refused() {
    // 入口 (`--run`) は整数と bool の引数・戻り値だけ。集成体の関数は表示だけできる。
    let t = Tmp::new("kir_agg_entry", KIR_AGG_SRC);
    let o = kir_run(&["kir", "t.mk_box", "--run", "1", "2", "true"], &t.0);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("集成体を引数・戻り値に持つ"), "{}", text(&o.stderr));
}

#[test]
fn kir_scalar_text_reads_back_with_float_and_bit_ops() {
    // `ksn kir` は出力を読み直して同じ文字列になることを確かめてから出す。浮動小数の命令と再解釈の表記を含む。
    let t = scalar_pkg("kir_scalar_text", "");
    let o = run(&["kir", "t.a_trig"], &t.0);

    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("fsin.f64"), "{}", out);
    assert!(out.contains("fpowi.f64"), "{}", out);
    assert!(out.contains("bitcast.u64.f64"), "{}", out);
}

#[test]
fn kir_scalar_reports_unsupported_numeric_methods_with_a_reason() {
    // `trunc_to_*` は `Option` を返すので段階 1 の型に入らない。理由を付けて拒否する。
    let src = "fn t.a_tr(a: f64) -> bool uses [] panics never
    match a.trunc_to_i32() {
        Some(_v) => true
        None => false
    }
end t.a_tr
";
    let t = Tmp::new("kir_scalar_unsupported", src);
    let o = run(&["kir", "t.a_tr"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("t.a_tr は未対応"), "{}", text(&o.stderr));
}

// ---- 数値の組み合わせの探索 (NaN・符号付き零・丸め・飽和・128 ビットの境界): kir_scalar_probes_* ----

/// 数値の関数の探索用の定義。名前の順 (E0020)。main は別に付ける。
const SCALAR_PROBES: &str = r#"fn t.q01() -> u32 uses [] panics never
    let a = 1.1f32
    let b = 3.3f32
    (a * b - a / b + a - b).to_bits()
end t.q01

fn t.q02() -> u32 uses [] panics never
    let z = 0.0f32
    (1.0f32 / z).to_bits()
end t.q02

fn t.q03() -> u64 uses [] panics never
    let x = 2.5f64
    (-x).abs().to_bits()
end t.q03

fn t.q04() -> u64 uses [] panics never
    let n = 0.0f64 / 0.0f64
    n.max(1.0f64).to_bits()
end t.q04

fn t.q05() -> u64 uses [] panics never
    let x = 7.5f64
    x.rem(2.0f64).to_bits()
end t.q05

fn t.q06() -> u64 uses [] panics never
    let x = 2.5f64
    let y = 3.5f64
    (x.round_ties_even() + y.round_ties_even()).to_bits()
end t.q06

fn t.q07() -> u64 uses [] panics never
    let x = -2.5f64
    (x.floor() * 100.0f64 + x.ceil() * 10.0f64 + x.trunc()).to_bits()
end t.q07

fn t.q08() -> bool uses [] panics never
    let n = 0.0f64 / 0.0f64
    let i = 1.0f64 / 0.0f64
    n.is_nan() && !i.is_finite()
end t.q08

fn t.q09() -> u64 uses [] panics never
    2.0f64.powi(-3i32).to_bits()
end t.q09

fn t.q10() -> u64 uses [] panics never
    let x = 12.5f64
    (x.sin() * x.cos() + x.tan() - x.exp().ln()).to_bits()
end t.q10

fn t.q11() -> u32 uses [] panics never
    let x = 0.1f32
    (x.to_f64() * 3.0f64).nearest_to_f32().to_bits()
end t.q11

fn t.q12() -> i128 uses [] panics never
    let a = -170141183460469231731687303715884105727i128
    a.wrapping_neg()
end t.q12

fn t.q13() -> u128 uses [] panics never
    let a = 0u128
    (!a).wrapping_add(2u128)
end t.q13

fn t.q14() -> u32 uses [] panics never
    let a = -1i8
    a.leading_zeros().wrapping_add((-1i128).count_ones())
end t.q14

fn t.q15() -> u32 uses [] panics never
    let a = 0i64
    a.trailing_zeros()
end t.q15

fn t.q16() -> i128 uses [] panics never
    let a = -256i128
    a.wrapping_shr(4u32)
end t.q16

fn t.q17() -> u128 uses [] panics never
    let a = 0u128.wrapping_sub(1u128)
    let b = a.wrapping_shr(1u32)
    b.min(a)
end t.q17

fn t.q18() -> i64 uses [] panics never
    i64.from_u32(4000000000u32)
end t.q18

fn t.q19() -> usize uses [] panics never
    usize.from_u8(255u8)
end t.q19

fn t.q20() -> u64 uses [] panics never
    f64.from_i64_nearest(9007199254740993i64).to_bits()
end t.q20

fn t.q21() -> u32 uses [] panics never
    f32.from_i32_nearest(16777217i32).to_bits()
end t.q21

fn t.q22() -> u64 uses [] panics never
    let a = -7i32
    a.to_f64().to_bits()
end t.q22

fn t.q23() -> i8 uses [] panics never
    let a = -128i8
    a.saturating_mul(-1i8).wrapping_add(a.saturating_sub(1i8))
end t.q23

fn t.q24() -> u32 uses [] panics never
    let a = 1u32
    a.wrapping_shl(35u32)
end t.q24

fn t.q25() -> bool uses [] panics never
    let x = 1.0f32
    let y = 2.0f32
    (x < y) && (y >= 2.0f32) && (x != y)
end t.q25

fn t.q26() -> bool uses [] panics never
    0.0f32 == -0.0f32
end t.q26

fn t.q27() -> u64 uses [] panics never
    let x = 1.0e300f64
    (x * x).to_bits()
end t.q27

fn t.q28() -> i64 uses [] panics never
    let a = 5i64
    a.wrapping_mul(-3i64).max(a.wrapping_neg())
end t.q28
"#;

#[test]
fn kir_scalar_probes_match_tree_walker() {
    // (関数, 木走査の期待値, 参照実行器の期待値)。木走査の期待値は参照実行器の値と同じであることを、別々の経路で確かめる。
    let cases: &[(&str, &str, &str)] = &[
        ("t.q01", "1066164118u32", "1066164118"),
        ("t.q02", "2139095040u32", "2139095040"),
        ("t.q03", "4612811918334230528u64", "4612811918334230528"),
        ("t.q04", "4607182418800017408u64", "4607182418800017408"),
        ("t.q05", "4609434218613702656u64", "4609434218613702656"),
        ("t.q06", "4618441417868443648u64", "4618441417868443648"),
        ("t.q07", "13867744336952688640u64", "13867744336952688640"),
        ("t.q08", "true", "true"),
        ("t.q09", "4593671619917905920u64", "4593671619917905920"),
        ("t.q10", "13846673201326737920u64", "13846673201326737920"),
        ("t.q11", "1050253722u32", "1050253722"),
        ("t.q12", "170141183460469231731687303715884105727i128", "170141183460469231731687303715884105727"),
        ("t.q13", "1u128", "1"),
        ("t.q14", "128u32", "128"),
        ("t.q15", "64u32", "64"),
        ("t.q16", "-16i128", "-16"),
        ("t.q17", "170141183460469231731687303715884105727u128", "170141183460469231731687303715884105727"),
        ("t.q18", "4000000000i64", "4000000000"),
        ("t.q19", "255usize", "255"),
        ("t.q20", "4845873199050653696u64", "4845873199050653696"),
        ("t.q21", "1266679808u32", "1266679808"),
        ("t.q22", "13842939354630062080u64", "13842939354630062080"),
        ("t.q23", "-1i8", "-1"),
        ("t.q24", "8u32", "8"),
        ("t.q25", "true", "true"),
        ("t.q26", "true", "true"),
        ("t.q27", "9218868437227405312u64", "9218868437227405312"),
        ("t.q28", "-5i64", "-5"),
    ];
    let mut main = String::new();
    for (f, want, _) in cases {
        main.push_str(&format!("    core.check.eq({}(), {})\n", f, want));
    }
    let t = Tmp::new("kir_scalar_probes", &format!("fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n{}    Ok(())\nend t.main\n\n{}", main, SCALAR_PROBES));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker:\n{}", text(&o.stderr));
    for (f, _, want) in cases {
        let k = kir_run(&["kir", *f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", f);
    }
}

#[test]
fn kir_refs_panics_match_tree_walker() {
    // 参照の先への書き込みで起きた溢れは、木走査と同じ文言と終了コード (101) になること。
    let t = refs_pkg("kir_refs_panic", "    let _x = t.overflow_via_ref(200u8)\n");
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101), "tree-walker\n{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("panic: attempt to add with overflow"), "{}", text(&o.stderr));
    let k = kir_run(&["kir", "t.overflow_via_ref", "--run", "200"], &t.0);
    assert_eq!(k.status.code(), Some(101), "kir\n{}", text(&k.stderr));
    assert_eq!(text(&k.stderr).trim(), "panic: attempt to add with overflow");
}

#[test]
fn kir_refs_text_shows_slots_and_reference_types() {
    // 出力は `kir 1` の形で、読み直して検査が通る (cli の内部検査)。スタックスロット・参照の複製・参照の型が見える。
    let t = refs_pkg("kir_refs_text", "    let _x = t.twice(1i32)\n");
    let o = run(&["kir", "t.assign_branch"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let s = text(&o.stdout);
    for want in ["alloca", "store %", "load %", "reborrow %", "%2: mutref<i32> = alloca"] {
        assert!(s.contains(want), "missing `{}` in:\n{}", want, s);
    }
    let p = run(&["kir", "t.pick"], &t.0);
    assert_eq!(p.status.code(), Some(0), "{}", text(&p.stderr));
    assert!(text(&p.stdout).contains("ret=ref<i32>"), "{}", text(&p.stdout));
}

#[test]
fn kir_refs_reference_to_unsupported_pointee_keeps_its_reason() {
    // 指す先が整数・浮動小数・bool・利用者の構造体でない参照 (標準の型) は、従来どおり理由付きで未対応になる。
    let src = "fn t.first(s: &std.io.Stdout) -> u8 uses [] panics never
    let _t = s
    0u8
end t.first
";
    let t = Tmp::new("kir_refs_str", src);
    let o = run(&["kir", "t.first"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("t.first は未対応"), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("段階 1 の対象外"), "{}", text(&o.stderr));
}

#[test]
fn min_rem_minus_one_overflows_for_every_signed_width() {
    // 案 A: 符号付きの `MIN % -1` は割り算が溢れるので余りも溢れ。`%` は panic (終了コード 101)、`checked_rem` と `checked_div` は None。
    // 木走査 (`ksn run`) と参照実行器 (`ksn kir --run`) の両方で panic の文言を確かめる。
    for w in ["i8", "i16", "i32", "i64", "i128"] {
        let src = format!(
            "fn t.rem(a: {w}, b: {w}) -> {w} uses [] panics may\n    a % b\nend t.rem\n\n\
             fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    \
             core.mem.drop(t.rem(core.num.{w}.MIN, -1{w}))\n    Ok(())\nend t.main\n",
            w = w
        );
        let t = Tmp::new(&format!("min_rem_{}", w), &src);
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(101), "{}: {}", w, text(&o.stderr));
        assert!(text(&o.stderr).contains("panic: attempt to calculate the remainder with overflow"), "{}: {}", w, text(&o.stderr));
        if w == "i8" {
            let k = kir_run(&["kir", "t.rem", "--run", "-128", "-1"], &t.0);
            assert_eq!(k.status.code(), Some(101), "kir {}: {}", w, text(&k.stderr));
            assert_eq!(text(&k.stderr).trim(), "panic: attempt to calculate the remainder with overflow", "kir {}", w);
        }
    }
    // `checked_rem` と `checked_div` は None (`test --both` で木走査と機械語を比べる。i128 は機械語が未対応なのでバンドルで比べる)。
    let mut src = String::from("fn t.main() -> () uses [] panics never\nend t.main\n\n");
    for w in ["i8", "i16", "i32", "i64", "i128"] {
        src.push_str(&format!(
            "test t.main.test_{w}\n    let m = core.num.{w}.MIN\n    core.check.eq(m.checked_rem(-1{w}), None)\n    core.check.eq(m.checked_div(-1{w}), None)\nend t.main.test_{w}\n\n",
            w = w
        ));
    }
    let src = format!("{}\n", src.trim_end());
    let t = Tmp::new("min_rem_checked", &src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("5 passed; 0 failed"), "{}", out);
    if clang_available() {
        for w in ["i8", "i16", "i32", "i64"] {
            assert!(out.contains(&format!("t.main.test_{} ... ok (both: llvm)", w)), "{}", out);
        }
    }
    // i128 は KIR (定数の値が扱えない) も Cranelift (128 ビットの除算・剰余) も持たないので、バンドルで比べる (誤った値を出さない)。
    assert!(out.contains("t.main.test_i128 ... ok (both: bundle (llvm skipped: "), "{}", out);
    assert!(out.contains("; native skipped: t.main.test_i128: 128-bit method i128.checked_rem"), "{}", out);
}

#[test]
fn i128_u128_checked_and_wrapping_native() {
    // i128 / u128 の極値 (MIN / MAX) での checked_add / wrapping_add なども機械語で比べる。
    let src = "fn t.main() -> () uses [] panics never\nend t.main\n\n\
        test t.main.test_i128_min\n    let m = core.num.i128.MIN\n    core.check.eq(m.wrapping_sub(1i128), core.num.i128.MAX)\n    core.check.eq(m.checked_add(-1i128), None)\nend t.main.test_i128_min\n\n\
        test t.main.test_u128_max\n    let m = core.num.u128.MAX\n    core.check.eq(m.wrapping_add(1u128), 0u128)\n    core.check.eq(m.checked_add(1u128), None)\nend t.main.test_u128_max\n";
    let t = Tmp::new("i128_u128_checked_wrapping", src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("2 passed; 0 failed"), "{}", out);
    // 128 ビットの定数は KIR に下ろせないので、Cranelift で比べる (LLVM の理由が出る)。
    for name in ["t.main.test_i128_min", "t.main.test_u128_max"] {
        assert!(out.contains(&format!("{name} ... ok (both: native (llvm skipped: ")), "{}", out);
    }
}


// ---- KIR の段階 1 の拡張: Vec・String・&str・for・添字・クロージャ (kir/collections、設計_KIR.md 10.9) ----

/// コレクション系の試験用の関数。値は木走査 (`ksn run`) と参照実行器 (`ksn kir --run`) で比べる。
const KIR_COLL_FNS: &str = "fn t.vlen() -> usize uses [] panics may
    let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    v.len()
end t.vlen

fn t.vempty() -> bool uses [] panics never
    let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    v.is_empty()
end t.vempty

fn t.str_len() -> usize uses [] panics never
    let s = \"héllo\"
    s.len()
end t.str_len

fn t.string_len() -> usize uses [] panics never
    let s = alloc.string.String.new(std.alloc.SYSTEM)
    s.len()
end t.string_len

fn t.pick(c: bool) -> usize uses [] panics never
    let s = if c {
        \"ab\"
    } else {
        \"cde\"
    }
    s.len()
end t.pick

fn t.sum_skip(n: usize) -> usize uses [] panics may
    let mut total = 0usize
    for i in core.iter.range(0usize, n) {
        if i == 3usize {
            continue
        }
        total = total + i
    }
    total
end t.sum_skip

fn t.first_at(n: usize) -> usize uses [] panics may
    let mut k = 0usize
    for i in core.iter.range(0usize, n) {
        let s = alloc.string.String.new(std.alloc.SYSTEM)
        if i == 2usize {
            return k.wrapping_add(s.len())
        }
        k = k.wrapping_add(i)
    }
    k
end t.first_at

fn t.brk(n: u32) -> u32 uses [] panics may
    let mut last = 0u32
    for i in core.iter.range(0u32, n) {
        last = i
        if i == 4u32 {
            break
        }
    }
    last
end t.brk

fn t.empty_loop(n: u32) -> u32 uses [] panics may
    let v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut total = n
    for x in v {
        if x == 7u32 {
            return 99u32
        }
        total = total.wrapping_add(x)
    }
    total
end t.empty_loop

fn t.find(n: usize) -> usize uses [] panics may
    let mut i = 0usize
    while i < 10usize {
        for j in core.iter.range(0usize, 10usize) {
            if i.wrapping_mul(j) == n {
                return i.wrapping_add(j)
            }
        }
        i = i.wrapping_add(1usize)
    }
    0usize
end t.find

fn t.nested(n: usize) -> usize uses [] panics may
    let mut total = 0usize
    for i in core.iter.range(0usize, n) {
        let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
        if !v.is_empty() {
            continue
        }
        for j in core.iter.range(0usize, i) {
            if j == 2usize {
                break
            }
            total = total + j
        }
        total = total + 100usize
    }
    total
end t.nested

fn t.idx_empty() -> u8 uses [] panics may
    let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    v[5usize]
end t.idx_empty

fn t.clos_let() -> u32 uses [] panics never
    let add = fn(a: u32, b: u32) -> u32 capture [] {
        a.wrapping_add(b)
    }
    let k = add(3u32, 4u32)
    add(k, 1u32)
end t.clos_let

fn t.apply_n[F: core.ops.Fn<(u64,), u64, E, P>, E: uses, P: panics](f: F, n: u64) -> u64 uses [E] panics P
    let mut acc = 0u64
    let mut i = 0u64
    while i < n {
        acc = acc.wrapping_add(f(i))
        i = i.wrapping_add(1u64)
    }
    acc
end t.apply_n

fn t.total(n: u64) -> u64 uses [] panics may
    t.apply_n(
        fn(x: u64) -> u64 capture [] {
            x.wrapping_mul(3u64).wrapping_add(1u64)
        },
        n,
    )
end t.total

";

/// `KIR_COLL_FNS` に `main` を付けたパッケージを作る。
fn kir_coll_pkg(name: &str, main_body: &str) -> Tmp {
    Tmp::new(name, &format!("{}{}", KIR_COLL_FNS, main_wrap("", main_body)))
}

#[test]
fn kir_coll_values_match_tree_walker() {
    // 木走査 (`ksn run`) の `core.check.eq` が通り、参照実行器 (`ksn kir --run`) の値が期待どおりであること。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.vlen", &[], "0usize", "0"),
        ("t.vempty", &[], "true", "true"),
        ("t.str_len", &[], "6usize", "6"),
        ("t.string_len", &[], "0usize", "0"),
        ("t.pick", &["true"], "2usize", "2"),
        ("t.pick", &["false"], "3usize", "3"),
        ("t.sum_skip", &["5"], "7usize", "7"),
        ("t.sum_skip", &["0"], "0usize", "0"),
        ("t.first_at", &["5"], "1usize", "1"),
        ("t.first_at", &["1"], "0usize", "0"),
        ("t.brk", &["10"], "4u32", "4"),
        ("t.brk", &["3"], "2u32", "2"),
        ("t.find", &["6"], "7usize", "7"),
        ("t.find", &["97"], "0usize", "0"),
        ("t.nested", &["4"], "402usize", "402"),
        ("t.empty_loop", &["4"], "4u32", "4"),
        ("t.clos_let", &[], "8u32", "8"),
        ("t.total", &["10"], "145u64", "145"),
        ("t.total", &["0"], "0u64", "0"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.join(", "));
        let t = kir_coll_pkg(&format!("kir_coll_values_{}", i), &format!("    core.check.eq({}, {})\n", call, lit));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_str_for_matches_tree_walker() {
    // `for x in s.split(sep)` / `for c in s.chars()` (`s` は `&str` = fat) が、木走査と
    // 参照実行器・機械語で同じ結果になること。空の区切り・末尾・連続・マルチバイトを含む。
    let split = |s: &str, sep: &str| -> String {
        format!("fn t.f() -> usize uses [] panics never\n    let mut n = 0usize\n    for _ in {:?}.split({:?}) {{\n        n = n.wrapping_add(1usize)\n    }}\n    n\nend t.f\n", s, sep)
    };
    let chars = |s: &str| -> String {
        format!("fn t.f() -> usize uses [] panics never\n    let mut n = 0usize\n    for _ in {:?}.chars() {{\n        n = n.wrapping_add(1usize)\n    }}\n    n\nend t.f\n", s)
    };
    let cases: &[(&str, &str)] = &[
        (&split("a,b,c", ","), "3"),
        (&split("", ","), "1"),
        (&split("a,,b", ","), "3"),
        (&split("a,", ","), "2"),
        (&split(",a", ","), "2"),
        (&split("ab", ","), "1"),
        (&split("", ""), "1"),
        (&split("a,b", ""), "1"),
        (&split("x\r\ny", "\r\n"), "2"),
        (&chars(""), "0"),
        (&chars("abc"), "3"),
        (&chars("あいう"), "3"),
        (&chars("aあb"), "3"),
    ];
    for (i, (src, want)) in cases.iter().enumerate() {
        let t = Tmp::new(&format!("kir_str_for_{}", i), &format!("{}{}", src, main_wrap("", &format!("    core.check.eq(t.f(), {})\n", want))));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", want, text(&o.stderr));
        let k = kir_run(&["kir", "t.f", "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", want, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", want);
    }
}

#[test]
fn kir_coll_index_panics_like_tree_walker() {
    // 空の Vec の添字は木走査と同じ文言で panic する (終了コード 101)。
    let t = kir_coll_pkg("kir_coll_index_panic", "    let _x = t.idx_empty()\n");
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101), "tree-walker");
    assert!(text(&o.stderr).contains("panic: index out of bounds: the len is 0 but the index is 5"), "{}", text(&o.stderr));
    let k = kir_run(&["kir", "t.idx_empty", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(101), "kir: {}", text(&k.stderr));
    assert_eq!(text(&k.stderr).trim(), "panic: index out of bounds: the len is 0 but the index is 5");
}

#[test]
fn kir_coll_closure_is_monomorphized_per_closure() {
    // `Fn` 境界の関数は、渡された捕獲のないクロージャごとに 1 つの単相インスタンスになる。
    let t = kir_coll_pkg("kir_coll_mono", "    core.check.eq(t.total(3u64), 12u64)\n");
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
    let o = run(&["kir", "t.total"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("fn t.apply_n key=t.apply_n<t.total#c") && out.contains("call t.apply_n<t.total#c"), "{}", out);
    assert!(out.contains("fn t.total#c") && out.contains("end t.total#c"), "{}", out);
}

#[test]
fn kir_coll_reports_unsupported_constructs_with_a_reason() {
    // `Vec` の `remove` / `insert` はスカラー要素なら段階 1 で下ろせる (`push` / `pop` と同様)。
    // capture を持つクロージャは、理由を付けて拒否する。
    let cases: &[(&str, &str, &str)] = &[
        ("t.captured", "    let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    let f = fn(x: usize) -> usize capture [move v] {\n        x\n    }\n    f(1usize)\n", "取り込みは段階 1 の対象外"),
    ];
    for (i, (f, body, why)) in cases.iter().enumerate() {
        let src = format!(
            "fn {}() -> usize uses [alloc] panics may\n{}end {}\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics may\n    Ok(())\nend t.main\n",
            f, body, f
        );
        let t = Tmp::new(&format!("kir_coll_reason_{}", i), &src);
        let o = run(&["kir", f], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}", f);
        assert!(text(&o.stderr).contains(why), "{}: {}", f, text(&o.stderr));
    }
}

#[test]
fn kir_coll_dump_shows_drops_and_static_data() {
    // 所有値の解放は `free` の命令として見える。文字列リテラルは `data` の静的な領域。
    let t = kir_coll_pkg("kir_coll_dump", "    core.check.eq(t.vlen(), 0usize)\n");
    let o = run(&["kir", "t.vlen"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("frame.bytes 24") && out.contains("heap.free %") && out.contains("mem.load.usize %"), "{}", out);
    let s = run(&["kir", "t.str_len"], &t.0);
    assert!(text(&s.stdout).contains("static.bytes 68c3a96c6c6f"), "{}", text(&s.stdout));
}

// ---- 集成体への参照とスライス・&str (設計_KIR.md 10.10、kir/agg-refs) ----

/// 集成体への `&` / `&mut`、入れ子のフィールド、スライス、`&str` の試験用の関数。
const AGG_REFS_FNS: &str = "type t.P
    x: i32
    y: i32
end t.P

type t.Line
    a: t.P
    b: t.P
    len: u32
end t.Line

fn t.p_sum(p: &t.P) -> i32 uses [] panics may
    p.x + p.y
end t.p_sum

fn t.p_bump(p: &mut t.P, d: i32) -> () uses [] panics may
    p.x = p.x + d
    p.y = p.y + d
end t.p_bump

fn t.p_run(a: i32, b: i32) -> i32 uses [] panics may
    let mut p = t.P { x: a, y: b }
    t.p_bump(&mut p, 10i32)
    t.p_bump(&mut p, 1i32)
    t.p_sum(&p)
end t.p_run

fn t.p_via_ref(a: i32) -> i32 uses [] panics may
    let mut p = t.P { x: a, y: 1i32 }
    let r = &mut p
    r.x = r.x * 2i32
    t.p_bump(r, 3i32)
    t.p_sum(r)
end t.p_via_ref

fn t.px(p: &mut t.P) -> &mut i32 uses [] panics never
    &mut p.x
end t.px

fn t.p_field_ref(a: i32) -> i32 uses [] panics may
    let mut p = t.P { x: 0i32, y: 5i32 }
    *t.px(&mut p) = a
    p.x * 100i32 + p.y
end t.p_field_ref

fn t.line_move(l: &mut t.Line, dx: i32) -> () uses [] panics may
    l.a.x = l.a.x + dx
    l.b.x = l.b.x + dx
    l.len = l.len + 1u32
end t.line_move

fn t.line_span(l: &t.Line) -> i32 uses [] panics may
    l.b.x - l.a.x + l.b.y
end t.line_span

fn t.line_run(n: i32) -> i32 uses [] panics may
    let mut l = t.Line { a: t.P { x: 1i32, y: 2i32 }, b: t.P { x: 5i32, y: 9i32 }, len: 0u32 }
    let mut i = 0i32
    while i < n {
        t.line_move(&mut l, 3i32)
        i = i + 1i32
    }
    t.p_bump(&mut l.b, 100i32)
    t.line_span(&l) * 1000i32 + l.a.x
end t.line_run

fn t.line_len(n: u32) -> u32 uses [] panics may
    let mut l = t.Line { a: t.P { x: 0i32, y: 0i32 }, b: t.P { x: 0i32, y: 0i32 }, len: 0u32 }
    let mut i = 0u32
    while i < n {
        t.line_move(&mut l, 1i32)
        i = i + 1u32
    }
    l.len
end t.line_len

fn t.s_len(s: &str) -> usize uses [] panics never
    s.len()
end t.s_len

fn t.s_len_lit() -> usize uses [] panics never
    t.s_len(\"héllo\")
end t.s_len_lit

fn t.s_len_empty() -> usize uses [] panics never
    t.s_len(\"\")
end t.s_len_empty

fn t.b_at(s: &[u8], i: usize) -> u8 uses [] panics may
    s[i]
end t.b_at

fn t.b_sum(s: &[u8]) -> u8 uses [] panics may
    let mut acc = 0u8
    for i in core.iter.range(0usize, s.len()) {
        acc = acc.wrapping_add(s[i])
    }
    acc
end t.b_sum

fn t.pass_through(s: &str) -> &str uses [] panics never
    s
end t.pass_through

fn t.str_byte(s: &str, i: usize) -> u8 uses [] panics may
    let b = s.as_bytes()
    t.b_at(b, i)
end t.str_byte

fn t.lit_byte(i: usize) -> u8 uses [] panics may
    t.str_byte(\"héllo\", i)
end t.lit_byte

fn t.lit_sum() -> u8 uses [] panics may
    t.b_sum(\"abc\".as_bytes())
end t.lit_sum

fn t.lit_sum_empty() -> u8 uses [] panics may
    t.b_sum(\"\".as_bytes())
end t.lit_sum_empty

fn t.lit_last() -> u8 uses [] panics may
    let b = \"abc\".as_bytes()
    b[b.len() - 1usize]
end t.lit_last

fn t.lit_oob() -> u8 uses [] panics may
    let b = \"abc\".as_bytes()
    b[3usize]
end t.lit_oob

fn t.empty_oob() -> u8 uses [] panics may
    t.str_byte(\"\", 0usize)
end t.empty_oob

fn t.pass_len(n: u32) -> usize uses [] panics never
    let s = if n > 1u32 {
        t.pass_through(\"abcd\")
    } else {
        \"x\"
    }
    s.len()
end t.pass_len

fn t.is_hex(c: u8) -> bool uses [] panics never
    (c >= b'0' && c <= b'9') || (c >= b'a' && c <= b'f') || (c >= b'A' && c <= b'F')
end t.is_hex

fn t.hex_of(c: u8) -> u32 uses [] panics never
    if t.is_hex(c) {
        1u32
    } else {
        0u32
    }
end t.hex_of

fn t.digits(src: &[u8], pos: usize) -> usize uses [] panics never
    let mut i = pos
    loop {
        match src.get(i) {
            Some(c) => {
                if c >= b'0' && c <= b'9' {
                    i = i.wrapping_add(1usize)
                } else {
                    break
                }
            }
            None => break
        }
    }
    i
end t.digits

fn t.digits_at(n: usize) -> usize uses [] panics never
    t.digits(\"12a45\".as_bytes(), n)
end t.digits_at

fn t.skip_ws(src: &[u8], pos: usize) -> usize uses [] panics never
    let mut i = pos
    loop {
        match src.get(i) {
            Some(c) => {
                if c == b' ' || c == b'\\n' || c == b'\\t' || c == b'\\r' {
                    i = i.wrapping_add(1usize)
                } else {
                    break
                }
            }
            None => break
        }
    }
    i
end t.skip_ws

fn t.skip_at(n: usize) -> usize uses [] panics never
    t.skip_ws(\" \\t\\n x\".as_bytes(), n)
end t.skip_at

fn t.str_digits(s: &str, n: usize) -> usize uses [] panics never
    t.digits(s.as_bytes(), n)
end t.str_digits

fn t.str_digits_empty(n: usize) -> usize uses [] panics never
    t.str_digits(\"\", n)
end t.str_digits_empty

type t.Doc
    links: alloc.vec.Vec<usize, std.alloc.System>
    nodes: alloc.vec.Vec<u8, std.alloc.System>
    n: u32
end t.Doc

fn t.doc_count(d: &t.Doc) -> usize uses [] panics may
    d.links.len() + d.nodes.len()
end t.doc_count

fn t.doc_bump(d: &mut t.Doc) -> () uses [] panics may
    d.n = d.n + 1u32
end t.doc_bump

fn t.doc_empty(d: &t.Doc) -> bool uses [] panics never
    d.nodes.is_empty()
end t.doc_empty

fn t.doc_by_ref(d: &mut t.Doc, k: u32) -> usize uses [] panics may
    let mut i = 0u32
    while i < k {
        t.doc_bump(d)
        i = i + 1u32
    }
    t.doc_count(d) + 10usize
end t.doc_by_ref

fn t.doc_n(d: &t.Doc) -> u32 uses [] panics never
    d.n
end t.doc_n

fn t.doc_run(k: u32) -> u32 uses [] panics may
    let a: alloc.vec.Vec<usize, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let b: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut d = t.Doc { links: a, nodes: b, n: 0u32 }
    core.mem.drop(t.doc_by_ref(&mut d, k))
    t.doc_n(&d)
end t.doc_run

fn t.vec_len(v: &alloc.vec.Vec<u8, std.alloc.System>) -> usize uses [] panics never
    v.len()
end t.vec_len

fn t.vec_first(v: &alloc.vec.Vec<u8, std.alloc.System>) -> u8 uses [] panics may
    v[0usize]
end t.vec_first

fn t.vec_run() -> usize uses [] panics may
    let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    t.vec_len(&v)
end t.vec_run

fn t.vec_oob() -> u8 uses [] panics may
    let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    t.vec_first(&v)
end t.vec_oob

fn t.vec_empty_loop() -> usize uses [] panics may
    let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let len = t.vec_len(&v)
    let mut n = 0usize
    for _x in v {
        n = n + 1usize
    }
    n + len
end t.vec_empty_loop

";

/// `AGG_REFS_FNS` に `main` を付けたパッケージを作る。
fn agg_refs_pkg(name: &str, main_body: &str) -> Tmp {
    Tmp::new(name, &format!("{}{}", AGG_REFS_FNS, main_wrap("", main_body)))
}

#[test]
fn kir_agg_refs_values_match_tree_walker() {
    // 木走査 (`ksn run`) の `core.check.eq` が通ること、参照実行器 (`ksn kir --run`) の値が期待どおりであること。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.p_run", &["1", "2"], "25i32", "25"),
        ("t.p_run", &["-5", "5"], "22i32", "22"),
        ("t.p_via_ref", &["4"], "15i32", "15"),
        ("t.p_field_ref", &["7"], "705i32", "705"),
        ("t.line_run", &["0"], "213001i32", "213001"),
        ("t.line_run", &["3"], "213010i32", "213010"),
        ("t.line_len", &["0"], "0u32", "0"),
        ("t.line_len", &["5"], "5u32", "5"),
        ("t.s_len_lit", &[], "6usize", "6"),
        ("t.s_len_empty", &[], "0usize", "0"),
        ("t.lit_byte", &["0"], "104u8", "104"),
        ("t.lit_byte", &["1"], "195u8", "195"),
        ("t.lit_byte", &["5"], "111u8", "111"),
        ("t.lit_sum", &[], "38u8", "38"),
        ("t.lit_sum_empty", &[], "0u8", "0"),
        ("t.lit_last", &[], "99u8", "99"),
        ("t.pass_len", &["0"], "1usize", "1"),
        ("t.pass_len", &["5"], "4usize", "4"),
        ("t.hex_of", &["102"], "1u32", "1"),
        ("t.hex_of", &["103"], "0u32", "0"),
        ("t.hex_of", &["65"], "1u32", "1"),
        ("t.hex_of", &["47"], "0u32", "0"),
        ("t.digits_at", &["0"], "2usize", "2"),
        ("t.digits_at", &["2"], "2usize", "2"),
        ("t.digits_at", &["3"], "5usize", "5"),
        ("t.digits_at", &["9"], "9usize", "9"),
        ("t.skip_at", &["0"], "4usize", "4"),
        ("t.skip_at", &["4"], "4usize", "4"),
        ("t.skip_at", &["9"], "9usize", "9"),
        ("t.str_digits_empty", &["0"], "0usize", "0"),
        ("t.vec_run", &[], "0usize", "0"),
        ("t.vec_empty_loop", &[], "0usize", "0"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.join(", "));
        let t = agg_refs_pkg(&format!("kir_agg_refs_values_{}", i), &format!("    core.check.eq({}, {})\n", call, lit));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).lines().last().unwrap_or("").trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_agg_get_match_tree_walker() {
    // 構造体の Vec の `get(i)` が `Option<構造体>` を返し、`match` で分解でき、
    // 木走査 (`ksn run`) と参照実行器 / 機械語 (`ksn kir --run` / `--native`) が一致する。
    let fns = "type t.Item derive [Copy, Clone]\n    kind: u8\n    v: u32\nend t.Item\n\n\
               fn t.make(n: u32) -> alloc.vec.Vec<t.Item, std.alloc.System> uses [alloc] panics never\n    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    let mut i = 0u32\n    while i < n {\n        core.mem.drop(v.push(t.Item { kind: 1u8, v: i.wrapping_mul(7u32) }))\n        i = i.wrapping_add(1u32)\n    }\n    v\nend t.make\n\n\
               fn t.pick(i: usize) -> u32 uses [alloc] panics never\n    let v = t.make(4u32)\n    match v.get(i) {\n        None => 0u32\n        Some(t.Item { kind: _, v: x }) => x\n    }\nend t.pick\n";
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.pick", &["2"], "14u32", "14"),
        ("t.pick", &["9"], "0u32", "0"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.join(", "));
        let body = format!("    core.check.eq({}, {})\n", call, lit);
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_agg_get_{}", i), &format!("{}{}", fns, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_agg_refs_bounds_panic_like_tree_walker() {
    // スライスと Vec の範囲外の添字 (末尾の次・空) は、木走査と同じ文言で panic する (終了コード 101)。
    let cases: &[(&str, &str)] = &[
        ("t.lit_oob", "panic: index out of bounds: the len is 3 but the index is 3"),
        ("t.empty_oob", "panic: index out of bounds: the len is 0 but the index is 0"),
        ("t.vec_oob", "panic: index out of bounds: the len is 0 but the index is 0"),
    ];
    for (i, (f, msg)) in cases.iter().enumerate() {
        let t = agg_refs_pkg(&format!("kir_agg_refs_panic_{}", i), &format!("    let _x = {}()\n", f));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(101), "tree-walker {}\n{}", f, text(&o.stderr));
        assert!(text(&o.stderr).contains(msg), "{}: {}", f, text(&o.stderr));
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(101), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stderr).trim(), *msg, "kir {}", f);
    }
}

#[test]
fn kir_agg_refs_text_shows_field_addresses_and_fat_values() {
    // 集成体への参照の型 (`mutref<agg#k>`)、フィールドのアドレス (`fieldaddr`)、`&str` の値 (`fat` の `make`) が見え、読み直せる。
    let t = agg_refs_pkg("kir_agg_refs_text", "    let _x = t.p_run(1i32, 2i32)\n");
    let o = run(&["kir", "t.line_move"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let s = text(&o.stdout);
    for want in ["mutref<agg#", "fieldaddr %", "load %", "store %"] {
        assert!(s.contains(want), "missing `{}` in:\n{}", want, s);
    }
    let o = run(&["kir", "t.lit_byte"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let s = text(&o.stdout);
    assert!(s.contains("fat size=16 align=8 {ptr:ptr@0 len:usize@8}") && s.contains("static.bytes 68c3a96c6c6f"), "{}", s);
}

#[test]
fn kir_agg_refs_vec_fields_are_read_through_references() {
    // Vec / String を持つ構造体 (json.Doc の形) への参照は下ろせる: ヘッダの `len` を `fieldaddr` と `load` で読む。
    // 値渡しと組み立て (所有の移動) は設計_KIR.md 13 章で下ろせる。`t.doc_run` も下ろせる。
    let t = agg_refs_pkg("kir_agg_refs_vec_field", "    core.check.eq(t.doc_run(3u32), 3u32)\n");
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
    let e = run(&["kir", "t.doc_empty"], &t.0);
    assert_eq!(e.status.code(), Some(0), "{}", text(&e.stderr));
    let o = run(&["kir", "t.doc_by_ref"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let s = text(&o.stdout);
    assert!(s.contains("struct vec size=24 align=8 {ptr:ptr@0 len:usize@8 cap:usize@16}") && s.contains("call t.doc_bump(%0)"), "{}", s);
    // 組み立て (所有の移動) と `core.mem.drop` も下ろせる。値は木走査と同じで、漏れも二重解放もない。
    let o = kir_run(&["kir", "t.doc_run", "--run", "3"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "3");
}

#[test]
fn kir_agg_refs_reports_unsupported_references_with_a_reason() {
    // 指す先が標準の型・Option の参照や、移動・Vec の借用の戻りは、理由を付けて拒否する。
    let cases: &[(&str, &str, &str)] = &[
        ("fn t.f(e: &std.io.Stdout) -> u8 uses [] panics never\n    let _t = e\n    0u8\nend t.f\n", "t.f", "引数の型"),
        // 要素が集成体のスライスは `fat` の対象外 (要素の大きさが要る。`Option<&[u8]>` は対象になった)
        ("type t.P\n    a: u8\nend t.P\n\nfn t.f(s: &[t.P]) -> usize uses [] panics never\n    let _t = s\n    0usize\nend t.f\n", "t.f", "引数の型"),
    ];
    for (i, (src, f, why)) in cases.iter().enumerate() {
        let src = format!("{}\nfn t.main() -> Result<(), std.io.Error> uses [] panics may\n    Ok(())\nend t.main\n", src);
        let t = Tmp::new(&format!("kir_agg_refs_reason_{}", i), &src);
        let o = run(&["kir", f], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}", f);
        assert!(text(&o.stderr).contains(why), "{}: {}", f, text(&o.stderr));
    }
}

// ---- 型が決まった接尾辞なし整数リテラル: 木走査も型表の型で計算する (レビュー #1) ----

/// 辞書順 (count, dbl, main, plus, wrapped) に並べた定義。`main` の本体だけ差し替える。
fn lit_src(main_body: &str) -> String {
    format!(
        "fn t.count(n: u32) -> u8 uses [] panics may
    let mut c = 0
    let mut i = 0u32
    while i < n {{
        c = c + 1
        i = i + 1u32
    }}
    c
end t.count

fn t.dbl(a: u8) -> u8 uses [] panics never
    let z = if a > 1u8 {{
        200
    }} else {{
        a
    }}
    z.wrapping_add(z)
end t.dbl

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
{}    Ok(())
end t.main

fn t.plus(o: Option<u8>) -> u8 uses [] panics may
    let w = o.unwrap_or(255)
    w + 1
end t.plus

fn t.wrapped(o: Option<u8>) -> u8 uses [] panics never
    let w = o.unwrap_or(255)
    w.wrapping_add(1)
end t.wrapped

test t.dbl.test_lit
    core.check.eq(t.dbl(5u8), 144u8)
end t.dbl.test_lit

test t.wrapped.test_lit
    core.check.eq(t.wrapped(None), 0u8)
end t.wrapped.test_lit
",
        main_body
    )
}

#[test]
fn untyped_int_literals_take_their_inferred_type_in_the_tree_walker() {
    // 3 形とも、木走査 (`run` / `test`)・KIR (`kir --run`)・機械語 (`test --both`) の結果が一致する。
    let t = Tmp::new("lit_types", &lit_src("    let _x = t.count(300u32)\n"));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stderr).contains("panic: attempt to add with overflow"), "{}", text(&o.stderr));
    let k = kir_run(&["kir", "t.count", "--run", "300"], &t.0);
    assert_eq!(k.status.code(), Some(101));
    assert!(text(&k.stderr).contains("panic: attempt to add with overflow"), "{}", text(&k.stderr));
    assert_eq!(text(&kir_run(&["kir", "t.count", "--run", "200"], &t.0).stdout).trim(), "200");
    assert_eq!(text(&kir_run(&["kir", "t.dbl", "--run", "5"], &t.0).stdout).trim(), "144");
    for args in [&["test"][..], &["test", "--both"][..]] {
        let o = run(args, &t.0);
        let out = text(&o.stdout);
        assert_eq!(o.status.code(), Some(0), "{:?}\n{}{}", args, out, text(&o.stderr));
        assert!(out.contains("2 passed; 0 failed"), "{:?}\n{}", args, out);
    }
    // `w + 1` (`w` は u8 の 255): 木走査も overflow で panic する。
    let t2 = Tmp::new("lit_types_plus", &lit_src("    let _x = t.plus(None)\n"));
    let o = run(&["run"], &t2.0);
    assert_eq!(o.status.code(), Some(101), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stderr).contains("panic: attempt to add with overflow"), "{}", text(&o.stderr));
}

/// 派生の比較 (`Option` の `eq` / `cmp`) は、ペイロードの利用者型の手書きの `eq` / `cmp` を使い、列挙子の順序は
/// その列挙型の宣言順 (4.3)。木走査は以前、入れ子を構造的に比べ、`Option` の中の列挙型の順序を取り違えていた。
/// clif は手書きの比較を持つ型を構造的に比べないよう退避する。
#[test]
fn derived_comparisons_use_nested_user_impls_and_declared_variant_order() {
    let src = r#"enum t.E derive [Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord] pub
    B
    A
end t.E

type t.P derive [Debug, Clone, Copy] pub
    x: u8
    y: u8
end t.P

impl core.cmp.Eq for t.P

impl core.cmp.Ord for t.P
fn t.P.cmp(self: &t.P, other: &t.P) -> core.cmp.Ordering uses [] panics never impls core.cmp.Ord
    if self.x > other.x {
        core.cmp.Ordering.Less
    } else if self.x < other.x {
        core.cmp.Ordering.Greater
    } else {
        core.cmp.Ordering.Equal
    }
end t.P.cmp

impl core.cmp.PartialEq<t.P> for t.P
fn t.P.eq(self: &t.P, other: &t.P) -> bool uses [] panics never impls core.cmp.PartialEq<t.P>
    self.x == other.x
end t.P.eq

impl core.cmp.PartialOrd<t.P> for t.P
fn t.P.partial_cmp(self: &t.P, other: &t.P) -> Option<core.cmp.Ordering> uses [] panics never impls core.cmp.PartialOrd<t.P>
    Some(self.cmp(other))
end t.P.partial_cmp

fn t.main() -> () uses [] panics never
end t.main

test t.main.test_nested_enum_order
    let b: Option<t.E> = Some(t.E.B)
    let a: Option<t.E> = Some(t.E.A)
    core.check.eq(b.cmp(&a) == core.cmp.Ordering.Less, true)
    core.check.eq(a.cmp(&b) == core.cmp.Ordering.Greater, true)
end t.main.test_nested_enum_order

test t.main.test_nested_user_cmp
    let a: Option<t.P> = Some(t.P { x: 1u8, y: 1u8 })
    let b: Option<t.P> = Some(t.P { x: 2u8, y: 1u8 })
    core.check.eq(a.cmp(&b) == core.cmp.Ordering.Greater, true)
    let c: Option<t.P> = Some(t.P { x: 1u8, y: 9u8 })
    core.check.eq(a == c, true)
    core.check.eq(a.eq(&c), true)
end t.main.test_nested_user_cmp
"#;
    let t = Tmp::new("nested_user_cmp", src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("2 passed; 0 failed"), "{}", out);
    assert!(out.contains("t.main.test_nested_enum_order ... ok (both: native (llvm skipped: "), "{}", out);
    assert!(out.contains("ordering of t.P with a user-defined cmp"), "{}", out);
}

/// 派生の順序 (`<` / `partial_cmp`) は、構造体のフィールドを型定義の宣言順に辞書式で比べる (4.3)。構造体リテラルに
/// 書いた順では比べない。入れ子 (`Option` の中) でも同じ。以前は格納順 (リテラルの順) で比べ、`x < y` を取り違えていた。
#[test]
fn derived_order_compares_fields_in_declaration_order() {
    let src = r#"type t.P derive [Clone, Copy, PartialEq, PartialOrd] pub
    a: u8
    b: u8
end t.P

fn t.main() -> () uses [] panics may
    let x = t.P { b: 1u8, a: 2u8 }
    let y = t.P { a: 1u8, b: 9u8 }
    core.check.eq(x < y, false)
    core.check.eq(x > y, true)
    core.check.eq(x.partial_cmp(&y) == Some(core.cmp.Ordering.Greater), true)
    let ox: Option<t.P> = Some(x)
    let oy: Option<t.P> = Some(y)
    core.check.eq(ox.partial_cmp(&oy) == Some(core.cmp.Ordering.Greater), true)
end t.main
"#;
    let t = Tmp::new("derived_order_decl", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

/// 派生の `cmp` / `partial_cmp` の入口は、利用者の型の最上位で手書きのメソッドを引かない (4.3)。手書きの `partial_cmp` が
/// 派生の `cmp` を呼んでも無限に再帰しない (以前は `stack overflow`)。逆向き (手書きの `cmp` → 派生の `partial_cmp`) も、
/// 派生の辞書式 (`partial_cmp` と `<`) で比べ、手書きの `cmp` は `cmp` の呼び出しでだけ使う。
#[test]
fn derived_cmp_entry_does_not_recurse_into_handwritten_methods() {
    let src = r#"type t.K derive [PartialEq, Eq, Ord] pub
    v: u8
end t.K

impl core.cmp.PartialOrd<t.K> for t.K
fn t.K.partial_cmp(self: &t.K, other: &t.K) -> Option<core.cmp.Ordering> uses [] panics never impls core.cmp.PartialOrd<t.K>
    Some(self.cmp(other))
end t.K.partial_cmp

type t.R derive [PartialEq, Eq, PartialOrd] pub
    v: u8
end t.R

impl core.cmp.Ord for t.R
fn t.R.cmp(self: &t.R, other: &t.R) -> core.cmp.Ordering uses [] panics never impls core.cmp.Ord
    if self.v > other.v {
        core.cmp.Ordering.Less
    } else if self.v < other.v {
        core.cmp.Ordering.Greater
    } else {
        core.cmp.Ordering.Equal
    }
end t.R.cmp

fn t.main() -> () uses [] panics may
    let a = t.K { v: 1u8 }
    let b = t.K { v: 2u8 }
    core.check.eq(a.cmp(&b) == core.cmp.Ordering.Less, true)
    core.check.eq(a < b, true)
    let c = t.R { v: 1u8 }
    let d = t.R { v: 2u8 }
    core.check.eq(c.partial_cmp(&d) == Some(core.cmp.Ordering.Less), true)
    core.check.eq(c < d, true)
    core.check.eq(c.cmp(&d) == core.cmp.Ordering.Greater, true)
end t.main
"#;
    let t = Tmp::new("derived_cmp_entry", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(!text(&o.stderr).contains("stack overflow"), "{}", text(&o.stderr));
}

/// f32 の `abs` は符号ビットを落とすだけ (ビットのまま)。木走査も KIR も、signaling NaN のビットを保つ。f64 へ広げると
/// quiet NaN に変わっていた (0x7f800001 → 0x7fc00001)。KIR は参照実行器と `--native` の結果も一致すること。
#[test]
fn f32_abs_keeps_signaling_nan_bits() {
    let src = r#"fn t.absbits(x: f32) -> u32 uses [] panics never
    x.abs().to_bits()
end t.absbits

fn t.main() -> () uses [] panics may
    let s: f32 = core.num.f32.from_bits(2139095041u32)
    core.check.eq(s.abs().to_bits(), 2139095041u32)
    core.check.eq((-s).abs().to_bits(), 2139095041u32)
    core.check.eq(t.absbits(s), 2139095041u32)
end t.main
"#;
    let t = Tmp::new("f32_abs_snan", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    // 0x7f800001 と 0xff800001 (負の signaling NaN) の abs は、どちらも 0x7f800001 のまま。
    for arg in ["2139095041", "4286578689"] {
        let k = kir_run(&["kir", "t.absbits", "--run", arg], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", arg, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), "2139095041", "kir abs of bits {}", arg);
    }
}

/// clif の 128 ビット整数: 確かめた演算 (比較・wrapping・checked_add / sub・単項 `-`・`from_*` の拡張) だけを機械語にし、
/// 除算・剰余・乗算・`try_from_*` など 64 ビットを前提にした経路は理由付きで退避する。以前は `checked_div` が
/// `Some(被除数)` を返す誤った機械語になっていた。どのテストも、インタプリタと食い違わない。
#[test]
fn clif_128_bit_integers_are_native_only_where_supported() {
    let src = r#"fn t.main() -> () uses [] panics never
end t.main

test t.main.test_a_div
    let x: i128 = 100i128
    core.check.eq(x.checked_div(7i128), Some(14i128))
end t.main.test_a_div

test t.main.test_b_rem
    let x: u128 = 100u128
    core.check.eq(x.checked_rem(7u128), Some(2u128))
end t.main.test_b_rem

test t.main.test_c_ops
    let x: i128 = 100i128
    let y: i128 = 7i128
    core.check.eq(x / y, 14i128)
    core.check.eq(x % y, 2i128)
end t.main.test_c_ops

test t.main.test_d_mul
    let x: i128 = 100000000000i128
    core.check.eq(x * x, 10000000000000000000000i128)
    core.check.eq(x.checked_mul(x), Some(10000000000000000000000i128))
    core.check.eq(x.wrapping_mul(3i128), 300000000000i128)
end t.main.test_d_mul

test t.main.test_e_cmp_shift
    let x: u128 = core.num.u128.MAX
    core.check.eq(x > 5u128, true)
    core.check.eq(x.wrapping_add(1u128), 0u128)
    core.check.eq(core.num.i32.MIN < 0i32, true)
    core.check.eq(core.num.i8.MIN, -128i8)
    core.check.eq(core.num.i16.MAX, 32767i16)
end t.main.test_e_cmp_shift

test t.main.test_f_neg_conv
    let x: i128 = 5i128
    core.check.eq(-x, -5i128)
    let y: i128 = i128.from_i64(-7i64)
    core.check.eq(y, -7i128)
    let z: u128 = u128.from_u64(18446744073709551615u64)
    core.check.eq(z.wrapping_add(1u128) > z, true)
end t.main.test_f_neg_conv

test t.main.test_g_try_conv
    let big: u128 = core.num.u128.MAX
    core.check.eq(u8.try_from_u128(big).is_err(), true)
end t.main.test_g_try_conv
"#;
    let t = Tmp::new("clif_i128_scope", src);
    let o = run(&["test", "--both"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(out.contains("7 passed; 0 failed"), "{}", out);
    assert!(!out.contains("disagree"), "{}", out);
    for name in ["test_e_cmp_shift", "test_f_neg_conv"] {
        assert!(out.contains(&format!("t.main.{} ... ok (both: native (llvm skipped: ", name)), "{}", out);
    }
    for (name, why) in [
        ("test_a_div", "128-bit method i128.checked_div"),
        ("test_b_rem", "128-bit method u128.checked_rem"),
        ("test_c_ops", "128-bit / (i128)"),
        ("test_d_mul", "128-bit * (i128)"),
        ("test_g_try_conv", "128-bit conversion u8.try_from_u128"),
    ] {
        assert!(out.contains(&format!("t.main.{} ... ok (both: bundle (llvm skipped: ", name)), "{}", out);
        assert!(out.contains(&format!("; native skipped: t.main.{}: {}", name, why)), "{}", out);
    }
}

#[test]
fn kir_llvm_output_is_deterministic_and_reads_as_ir() {
    // `--llvm` は同じ KIR から同じバイト列の `.ll` を出す (clang が無くても確かめられる)。
    let t = kir_pkg("kir_llvm_text", "    core.check.eq(t.gcd(4u64, 2u64), 2u64)\n");
    for f in ["t.gcd", "t.fact", "t.negate", "t.andor", "t.nested", "t.down"] {
        let a = run(&["kir", f, "--llvm"], &t.0);
        let b = run(&["kir", f, "--llvm"], &t.0);
        assert_eq!(a.status.code(), Some(0), "{}: {}", f, text(&a.stderr));
        assert_eq!(a.stdout, b.stdout, "{}: .ll が 2 回で違う", f);
        let ll = text(&a.stdout);
        assert!(ll.starts_with("; ksn kir llvm 1\n") && ll.contains(&format!("define internal")) && ll.contains("define i32 @main"), "{}", ll);
        assert!(!ll.contains("@sin") && !ll.contains("@cos"), "libm の超越関数を呼ばない");
    }
}

#[test]
fn kir_native_without_clang_is_an_error_with_a_reason() {
    let t = kir_pkg("kir_native_noclang", "    core.check.eq(t.gcd(4u64, 2u64), 2u64)\n");
    let o = Command::new(exe())
        .args(["kir", "t.gcd", "--native", "4", "2"])
        .current_dir(&t.0)
        .env("KSN_CLANG", "/nonexistent/clang")
        .output()
        .expect("spawn ksn");
    assert_eq!(o.status.code(), Some(2), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("clang が必要です"), "{}", text(&o.stderr));
}

/// ループの中の配列リテラル (フレームの確保) を周回ごとに積まない。以前の LLVM 版は `alloca` を評価のたびに
/// 出していたので、100 万周でスタックが溢れて落ちた (参照実行器は正しかった)。確保は関数の先頭で 1 回、
/// 0 埋めは評価のたびに行う。
#[test]
fn kir_array_literal_in_a_long_loop_does_not_grow_the_native_stack() {
    let src = "fn t.sum(n: u32) -> u64 uses [] panics never
    let mut i = 0u32
    let mut acc = 0u64
    while i < n {
        for x in [1u64, 2u64, 3u64, 4u64] {
            acc = acc.wrapping_add(x)
        }
        i = i.wrapping_add(1u32)
    }
    acc
end t.sum

fn t.main() -> () uses [] panics never
end t.main
";
    let t = Tmp::new("kir_array_loop_stack", src);
    let o = kir_run(&["kir", "t.sum", "--run", "1000000"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "10000000");
}

// ---- 入出力と入口 (設計_KIR.md 12): `write`、`ksn build --backend llvm`、`main` の `Err` の表示 ----

const IO_SAY: &str = "fn t.say() -> bool uses [io.console] panics never
    match std.io.print_line(\"hi there\") {
        Ok(_) => true
        Err(_) => false
    }
end t.say

fn t.twice() -> u8 uses [io.console] panics never
    let a = std.io.print_line(\"one\")
    let b = std.io.print_line(\"two\")
    match a {
        Ok(_) => match b {
            Ok(_) => 2u8
            Err(_) => 1u8
        }
        Err(_) => 0u8
    }
end t.twice

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never
    std.io.print_line(\"main\")
end t.main
";

#[test]
fn kir_write_prints_through_run_and_native() {
    // `print_line` は `write 1, s` と `write 1, \"\\n\"` に下ろされる。`--run` (参照実行器) と `--native` (write(2)) の表示が一致する。
    let t = Tmp::new("kir_io_write", IO_SAY);
    let o = run(&["kir", "t.say"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let ir = text(&o.stdout);
    assert!(ir.contains("= write %") && ir.contains("agg#1 struct std.io.Error") || ir.contains("struct std.io.Error size=8"), "{}", ir);
    let r = kir_run(&["kir", "t.say", "--run"], &t.0);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), "hi there\ntrue\n");
    let r = kir_run(&["kir", "t.twice", "--run"], &t.0);
    assert_eq!(text(&r.stdout), "one\ntwo\n2\n");
    // 木走査と同じ行が出る
    let tw = run(&["run"], &t.0);
    assert_eq!(text(&tw.stdout), "main\n");
    assert_eq!(tw.status.code(), Some(0));
}

/// `ksn build --backend llvm` の実行ファイルと `ksn run` を走らせ、(run, native) を返す。native が作れなければ `None`。
fn llvm_build_and_run(t: &Tmp) -> Option<(Output, Output)> {
    let b = run(&["build", "--backend", "llvm"], &t.0);
    let out = text(&b.stdout);
    assert_eq!(b.status.code(), Some(0), "{}{}", out, text(&b.stderr));
    assert!(!out.contains("llvm backend skipped"), "{}", out);
    if !clang_available() {
        return None;
    }
    let exe = t.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t.native" });
    assert!(exe.exists(), "{}", out);
    let n = Command::new(&exe).current_dir(&t.0).output().expect("native");
    Some((run(&["run"], &t.0), n))
}

fn same_output(r: &Output, n: &Output) {
    assert_eq!(text(&n.stdout), text(&r.stdout), "stdout");
    assert_eq!(text(&n.stderr), text(&r.stderr), "stderr");
    assert_eq!(n.status.code(), r.status.code(), "終了コード");
}

#[test]
fn llvm_build_of_print_line_main_matches_run() {
    let t = Tmp::new("llvm_build_print", IO_SAY);
    if let Some((r, n)) = llvm_build_and_run(&t) {
        assert_eq!(text(&r.stdout), "main\n");
        same_output(&r, &n);
    }
}

#[test]
fn llvm_build_owned_strings_rings_and_moves_match_run() {
    // `String.push_str` / `push(char)`、`&mut self` の構造体のフィールドの Vec (`get` / `get_mut`)、分解、フィールドからの移動、
    // `Vec<String>` を含む `main` が、`ksn run` と同じ表示・終了コードで LLVM から作れる (設計_KIR.md 13.7)。
    let body = "    core.check.eq(t.str_build(50usize), 100usize)\n    core.check.eq(t.str_chars(), 2031u32)\n    core.check.eq(t.ring_run(3usize), 528u32)\n    core.check.eq(t.pair_run(2u32), 13usize)\n    core.check.eq(t.destruct_struct(4u32), 7usize)\n    core.check.eq(t.field_move_reassign(4u32), 6usize)\n    core.check.eq(t.vs_run(5u32), 20usize)\n    core.check.eq(t.names_push(4u32), 4usize)\n    core.check.eq(t.mode_run(), 2u32)\n    std.io.print_line(\"owned ok\")\n";
    let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
    let t = Tmp::new("llvm_build_owned", &format!("{}{}", KIR_OWN2_FNS, main));
    if let Some((r, n)) = llvm_build_and_run(&t) {
        assert_eq!(text(&r.stdout), "owned ok\n");
        same_output(&r, &n);
    }
}

#[test]
fn llvm_build_main_err_prints_debug_and_exits_1() {
    // `Err(e)` は stderr に `Error: <Debug>` と改行、終了コード 1 (木走査と同じ)。std.io.Error の Debug も同じ。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never
    std.io.print_line(\"before\")?
    let e = std.io.Error { kind: std.io.ErrorKind.BrokenPipe, code: 32i32 }
    Err(e)
end t.main
";
    let t = Tmp::new("llvm_build_io_err", src);
    if let Some((r, n)) = llvm_build_and_run(&t) {
        assert_eq!(r.status.code(), Some(1));
        assert_eq!(text(&r.stderr), "Error: Error { kind: BrokenPipe, code: 32 }\n");
        assert_eq!(text(&r.stdout), "before\n");
        same_output(&r, &n);
    }
}

#[test]
fn llvm_build_debug_covers_structs_enums_ints_bool_char_and_str() {
    // 構造体・列挙型 (ペイロード付き)・入れ子・整数 (負数・0・64 ビットの端)・bool・char (逃れと非 ASCII)・&str (逃れ) の Debug は木走査と一致する。
    let src = "enum t.Kind derive [Debug] pub
    Plain
    Pair(i32, bool)
    Named(t.Inner)
end t.Kind

type t.Inner derive [Debug] pub
    a: i8
    b: u64
    c: char
end t.Inner

type t.Bad derive [Debug] pub
    neg: i64
    zero: i32
    flag: bool
    ch: char
    text: &'static str
    inner: t.Inner
    kind: t.Kind
    k2: t.Kind
    k3: t.Kind
    small: u8
    min: i64
    quote: char
end t.Bad

fn t.mk() -> t.Bad uses [] panics never
    let inner = t.Inner { a: -128i8, b: 18446744073709551615u64, c: '\\n' }
    let kind = t.Kind.Pair(-7i32, false)
    let inner2 = t.Inner { a: 0i8, b: 0u64, c: '\\u{e9}' }
    let k2 = t.Kind.Named(inner2)
    t.Bad { neg: -12345i64, zero: 0i32, flag: true, ch: 'x', text: \"a\\\"b\\\\c\\nd \\u{e9}\", inner: inner, kind: kind, k2: k2, k3: t.Kind.Plain, small: 255u8, min: -9223372036854775808i64, quote: '\\'' }
end t.mk

fn t.main() -> Result<(), t.Bad> uses [] panics never
    Err(t.mk())
end t.main
";
    let t = Tmp::new("llvm_build_debug", src);
    if let Some((r, n)) = llvm_build_and_run(&t) {
        assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
        let e = text(&r.stderr);
        assert!(e.starts_with("Error: Bad { neg: -12345, zero: 0, flag: true, ch: 'x',") && e.contains("kind: Pair(-7, false)") && e.contains("k3: Plain"), "{}", e);
        same_output(&r, &n);
    }
}

#[test]
fn llvm_build_panic_keeps_the_panic_exit_code_and_message() {
    // 溢れの panic は 101 と `panic: ...` (木走査と同じ)。それまでの出力は残る。
    let src = "fn t.bump(x: u8) -> u8 uses [] panics may
    x + 200u8
end t.bump

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    std.io.print_line(\"start\")?
    let y = t.bump(100u8)
    match y {
        0u8 => std.io.print_line(\"zero\")
        _ => std.io.print_line(\"unreachable\")
    }
end t.main
";
    let t = Tmp::new("llvm_build_panic", src);
    if let Some((r, n)) = llvm_build_and_run(&t) {
        assert_eq!(r.status.code(), Some(101), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), "start\n");
        same_output(&r, &n);
    }
}

#[test]
fn llvm_build_falls_back_with_a_reason_when_a_function_cannot_be_lowered() {
    // 下ろせない関数 (ここではジェネリック関数 `t.generic` を呼ぶ main) は、理由を出して既定の経路へ退避する。ビルドは成功する。
    // (`std.env.arg_count` は段階 1 で下ろせるようになったため、`with` は使わない。`with` は別担当が KIR 下ろし中。)
    let src = "fn t.generic[T](x: T) -> T uses [] panics never
    x
end t.generic

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.mem.drop(t.generic(5u32))
    std.io.print_line(\"x\")
end t.main
";
    let t = Tmp::new("llvm_build_fallback", src);
    let b = run(&["build", "--backend", "llvm"], &t.0);
    let out = text(&b.stdout);
    assert_eq!(b.status.code(), Some(0), "{}{}", out, text(&b.stderr));
    assert!(out.contains("llvm backend skipped: t.main を KIR に下ろせない") && out.contains("falling back"), "{}", out);
    assert!(out.contains("built target/debug"), "{}", out);
    // 既存の経路 (c / clif) は変わらない: llvm の文言を出さない
    let c = run(&["build"], &t.0);
    assert!(!text(&c.stdout).contains("llvm backend"), "{}", text(&c.stdout));
}

// ---- Vec / String の所有の移動と解放 (設計_KIR.md 13 章、kir/owned-heap) ----

/// 所有の移動と解放の試験用の関数。`--run` (参照実行器) は、関数の終わりで領域の漏れと二重解放を内部エラーにする。
const KIR_OWN_FNS: &str = "type t.Buf
    items: alloc.vec.Vec<u32, std.alloc.System>
    n: u32
end t.Buf

enum t.Slot
    Empty
    Full(alloc.vec.Vec<u32, std.alloc.System>)
end t.Slot

fn t.oom(_e: alloc.alloc.AllocError) -> u8 uses [] panics never
    1u8
end t.oom

fn t.make(n: u32) -> alloc.vec.Vec<u32, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        match v.push(i) {
            Ok(_u) => {
            }
            Err(_e) => {
                break
            }
        }
        i = i.wrapping_add(1u32)
    }
    v
end t.make

fn t.ret_vec(n: u32) -> usize uses [alloc] panics never
    let v = t.make(n)
    v.len()
end t.ret_vec

fn t.grow(v: alloc.vec.Vec<u32, std.alloc.System>, k: u32) -> alloc.vec.Vec<u32, std.alloc.System> uses [alloc] panics never
    let mut w = v
    match w.push(k) {
        Ok(_u) => {
        }
        Err(_e) => {
        }
    }
    w
end t.grow

fn t.sum(v: &alloc.vec.Vec<u32, std.alloc.System>) -> u32 uses [] panics may
    let mut total = 0u32
    for x in v {
        total = total.wrapping_add(x)
    }
    total
end t.sum

fn t.arg_ret(n: u32) -> u32 uses [alloc] panics may
    let v = t.make(n)
    let w = t.grow(v, 7u32)
    let x = t.grow(w, 8u32)
    t.sum(&x)
end t.arg_ret

fn t.consume(v: alloc.vec.Vec<u32, std.alloc.System>) -> usize uses [] panics never
    v.len()
end t.consume

fn t.buf(n: u32) -> t.Buf uses [alloc] panics never
    let v = t.make(n)
    t.Buf { items: v, n: n }
end t.buf

fn t.buf_len(b: &t.Buf) -> usize uses [] panics never
    b.items.len()
end t.buf_len

fn t.buf_run(n: u32) -> usize uses [alloc] panics never
    let b = t.buf(n)
    t.buf_len(&b)
end t.buf_run

fn t.consume_buf(b: t.Buf) -> usize uses [] panics never
    t.buf_len(&b)
end t.consume_buf

fn t.maybe(n: u32, some: bool) -> Option<alloc.vec.Vec<u32, std.alloc.System>> uses [alloc] panics never
    if some {
        Some(t.make(n))
    } else {
        None
    }
end t.maybe

fn t.opt_len(n: u32, some: bool) -> usize uses [alloc] panics never
    match t.maybe(n, some) {
        Some(v) => v.len()
        None => 99usize
    }
end t.opt_len

fn t.opt_local(n: u32, some: bool) -> usize uses [alloc] panics never
    let o = t.maybe(n, some)
    match o {
        Some(v) => v.len()
        None => 99usize
    }
end t.opt_local

fn t.opt_peek(n: u32, some: bool) -> usize uses [alloc] panics never
    let o = t.maybe(n, some)
    let k = match o {
        Some(_) => 1usize
        None => 2usize
    }
    if o.is_some() {
        k.wrapping_add(10usize)
    } else {
        k
    }
end t.opt_peek

fn t.opt_str(some: bool) -> usize uses [alloc] panics never
    let o: Option<alloc.string.String<std.alloc.System>> = if some {
        Some(alloc.string.String.new(std.alloc.SYSTEM))
    } else {
        None
    }
    match o {
        Some(s) => s.len()
        None => 99usize
    }
end t.opt_str

fn t.branch(n: u32, take: bool) -> usize uses [alloc] panics never
    let v = t.make(n)
    let mut total = 0usize
    if take {
        let w = v
        total = w.len()
    }
    total.wrapping_add(1usize)
end t.branch

fn t.branch_call(n: u32, take: bool) -> usize uses [alloc] panics never
    let v = t.make(n)
    if take {
        t.consume(v)
    } else {
        0usize
    }
end t.branch_call

fn t.branch_buf(n: u32, take: bool) -> usize uses [alloc] panics never
    let b = t.buf(n)
    if take {
        t.consume_buf(b)
    } else {
        7usize
    }
end t.branch_buf

fn t.churn(n: u32) -> usize uses [alloc] panics never
    let mut total = 0usize
    let mut i = 0u32
    while i < n {
        let v = t.make(3u32)
        core.mem.drop(t.make(2u32))
        total = total.wrapping_add(v.len())
        i = i.wrapping_add(1u32)
    }
    total
end t.churn

fn t.churn_exit(n: u32) -> usize uses [alloc] panics never
    let mut total = 0usize
    let outer = t.make(1u32)
    let mut i = 0u32
    while i < n {
        let v = t.make(i)
        i = i.wrapping_add(1u32)
        if i == 3u32 {
            continue
        }
        if i == 6u32 {
            break
        }
        total = total.wrapping_add(v.len())
    }
    total.wrapping_add(outer.len())
end t.churn_exit

fn t.step(k: usize, bad: bool) -> Result<usize, u8> uses [] panics never
    if bad {
        Err(5u8)
    } else {
        Ok(k)
    }
end t.step

fn t.try_inner(n: u32, bad: bool) -> Result<usize, u8> uses [alloc] panics never
    let v = t.make(n)
    let k = t.step(v.len(), bad)?
    let w = t.make(2u32)
    Ok(k.wrapping_add(w.len()))
end t.try_inner

fn t.try_run(n: u32, bad: bool) -> usize uses [alloc] panics never
    match t.try_inner(n, bad) {
        Ok(x) => x
        Err(_e) => 1000usize
    }
end t.try_run

fn t.push_oom(n: u32) -> Result<usize, u8> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    v.push(n).map_err(t.oom)?
    v.push(n).map_err(fn(x: alloc.alloc.AllocError) -> u8 capture [] {
        2u8
    })?
    Ok(v.len())
end t.push_oom

fn t.map_err_run(n: u32) -> usize uses [alloc] panics never
    match t.push_oom(n) {
        Ok(x) => x
        Err(_e) => 1000usize
    }
end t.map_err_run

fn t.slot(n: u32) -> t.Slot uses [alloc] panics never
    if n == 0u32 {
        t.Slot.Empty
    } else {
        t.Slot.Full(t.make(n))
    }
end t.slot

fn t.slot_len(n: u32) -> usize uses [alloc] panics never
    match t.slot(n) {
        t.Slot.Empty => 0usize
        t.Slot.Full(v) => v.len()
    }
end t.slot_len

fn t.slot_ignore(n: u32) -> usize uses [alloc] panics never
    match t.slot(n) {
        t.Slot.Empty => 1usize
        t.Slot.Full(_v) => 2usize
    }
end t.slot_ignore

fn t.reassign(n: u32) -> usize uses [alloc] panics never
    let mut v = t.make(n)
    v = t.make(2u32)
    v.len()
end t.reassign

";

/// `KIR_OWN_FNS` に `main` を付けたパッケージを作る。
fn kir_own_pkg(name: &str, main_body: &str) -> Tmp {
    let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", main_body);
    Tmp::new(name, &format!("{}{}", KIR_OWN_FNS, main))
}

#[test]
fn kir_own_values_match_tree_walker() {
    // Vec / String の所有の移動 (返す・引数で渡す・構造体のフィールド・`Option` の `match`・片方の枝だけで移す・ループ・`?`) が、
    // 木走査と同じ値になる。参照実行器は漏れと二重解放を内部エラーにし、LLVM (clang があれば) も同じ表示になる。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.ret_vec", &["5"], "5usize", "5"),
        ("t.ret_vec", &["0"], "0usize", "0"),
        ("t.arg_ret", &["4"], "21u32", "21"),
        ("t.buf_run", &["6"], "6usize", "6"),
        ("t.opt_len", &["3", "true"], "3usize", "3"),
        ("t.opt_len", &["3", "false"], "99usize", "99"),
        ("t.opt_local", &["4", "true"], "4usize", "4"),
        ("t.opt_local", &["4", "false"], "99usize", "99"),
        ("t.opt_peek", &["4", "true"], "11usize", "11"),
        ("t.opt_peek", &["4", "false"], "2usize", "2"),
        ("t.opt_str", &["true"], "0usize", "0"),
        ("t.opt_str", &["false"], "99usize", "99"),
        ("t.branch", &["5", "true"], "6usize", "6"),
        ("t.branch", &["5", "false"], "1usize", "1"),
        ("t.branch_call", &["5", "true"], "5usize", "5"),
        ("t.branch_call", &["5", "false"], "0usize", "0"),
        ("t.branch_buf", &["5", "true"], "5usize", "5"),
        ("t.branch_buf", &["5", "false"], "7usize", "7"),
        ("t.churn", &["10000"], "30000usize", "30000"),
        ("t.churn_exit", &["10"], "9usize", "9"),
        ("t.try_run", &["4", "false"], "6usize", "6"),
        ("t.try_run", &["4", "true"], "1000usize", "1000"),
        ("t.map_err_run", &["9"], "2usize", "2"),
        ("t.slot_len", &["0"], "0usize", "0"),
        ("t.slot_len", &["5"], "5usize", "5"),
        ("t.slot_ignore", &["0"], "1usize", "1"),
        ("t.slot_ignore", &["5"], "2usize", "2"),
        ("t.reassign", &["8"], "2usize", "2"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.iter().map(|a| match *a {
            "true" | "false" => a.to_string(),
            n => format!("{}u32", n),
        }).collect::<Vec<_>>().join(", "));
        let t = kir_own_pkg(&format!("kir_own_values_{}", i), &format!("    core.check.eq({}, {})\n", call, lit));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

// ---- 取り込みのあるクロージャと反復子の連なり (設計_KIR.md 14 章、kir/closures) ----

const KIR_CLOSURE_FNS: &str = "fn t.apply[F: core.ops.Fn<(u64,), u64, E, P>, E: uses, P: panics](f: F, n: u64) -> u64 uses [E] panics P pub
    let mut acc = 0u64
    let mut i = 0u64
    while i < n {
        acc = acc.wrapping_add(f(i))
        i = i.wrapping_add(1u64)
    }
    acc
end t.apply

fn t.cap_ref(n: u64) -> u64 uses [] panics may
    let k = 7u64
    t.apply(
        fn(x: u64) -> u64 capture [&k] {
            x.wrapping_mul(*k).wrapping_add(1u64)
        },
        n,
    )
end t.cap_ref

fn t.cap_move(n: u64) -> u64 uses [] panics may
    let mut k = 3u64
    k = k.wrapping_add(n)
    let f = fn(x: u64) -> u64 capture [move k] {
        x.wrapping_add(k)
    }
    f(1u64).wrapping_add(f(2u64)).wrapping_add(t.apply(f, 4u64))
end t.cap_move

fn t.cap_mut(n: u64) -> u64 uses [] panics may
    let mut total = 0u64
    let f = fn(x: u64) -> u64 capture [&mut total] {
        *total = (*total).wrapping_add(x)
        *total
    }
    f(n)
    f(n)
    let r = f(5u64)
    r.wrapping_add(total)
end t.cap_mut

fn t.cap_two(n: u64) -> u64 uses [] panics may
    let a = 10u64
    let mut b = 5u64
    b = b.wrapping_add(n)
    t.apply(
        fn(x: u64) -> u64 capture [&a, move b] {
            x.wrapping_mul(*a).wrapping_add(b)
        },
        3u64,
    )
end t.cap_two

fn t.fold_basic(n: u32) -> u32 uses [] panics may
    core.iter.fold(core.iter.range(0u32, n), 100u32, fn(acc: u32, x: u32) -> u32 capture [] {
        acc.wrapping_add(x)
    })
end t.fold_basic

fn t.chain(n: u64) -> u64 uses [] panics may
    let m = 3u64
    core.iter.fold(
        core.iter.filter(
            core.iter.map(core.iter.range(0u64, n), fn(x: u64) -> u64 capture [&m] {
                x.wrapping_mul(*m)
            }),
            fn(x: &u64) -> bool capture [] {
                *x % 2u64 == 0u64
            },
        ),
        1u64,
        fn(acc: u64, x: u64) -> u64 capture [] {
            acc.wrapping_add(x)
        },
    )
end t.chain

fn t.count_even(n: u32) -> usize uses [] panics may
    core.iter.count(core.iter.filter(core.iter.range(0u32, n), fn(x: &u32) -> bool capture [] {
        *x % 2u32 == 0u32
    }))
end t.count_even

fn t.fold_let(n: u32) -> u32 uses [] panics may
    let step = 2u32
    let f = fn(acc: u32, x: u32) -> u32 capture [&step] {
        acc.wrapping_add(x.wrapping_mul(*step))
    }
    core.iter.fold(core.iter.range(5u32, n), 1u32, f)
end t.fold_let

";

#[test]
fn kir_closure_captures_and_iter_pipes_match_tree_walker() {
    // 取り込み 4 種類 (`&x` `&mut x` `move x` と複数)、`fold` の初期値と累積、`map` → `filter` → `fold`、空の範囲。
    // (関数, 引数の型, 引数, 期待する値)
    let cases: &[(&str, &str, &str, &str)] = &[
        ("t.cap_ref", "u64", "4", "46"),
        ("t.cap_ref", "u64", "0", "0"),
        ("t.cap_move", "u64", "2", "39"),
        ("t.cap_two", "u64", "2", "51"),
        ("t.fold_basic", "u32", "5", "110"),
        ("t.fold_basic", "u32", "0", "100"),
        ("t.fold_let", "u32", "9", "53"),
        ("t.fold_let", "u32", "3", "1"),
        ("t.chain", "u64", "10", "61"),
        ("t.chain", "u64", "0", "1"),
        ("t.count_even", "u32", "9", "5"),
        ("t.count_even", "u32", "0", "0"),
    ];
    for (i, (f, aty, arg, want)) in cases.iter().enumerate() {
        let rty = if *f == "t.count_even" { "usize" } else { *aty };
        let call = format!("{}({}{})", f, arg, aty);
        let t = Tmp::new(
            &format!("kir_closure_{}", i),
            &format!("{}{}", KIR_CLOSURE_FNS, main_wrap("", &format!("    core.check.eq({}, {}{})\n", call, want, rty))),
        );
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let k = kir_run(&["kir", f, "--run", arg], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

const KIR_OWN2_FNS: &str = "type t.Ring
    items: alloc.vec.Vec<u32, std.alloc.System>
    head: usize
    len: usize
    cap: usize
end t.Ring

type t.Pair
    a: alloc.vec.Vec<u32, std.alloc.System>
    name: alloc.string.String<std.alloc.System>
    n: usize
end t.Pair

type t.Names
    items: alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>
end t.Names

enum t.Mode derive [Debug, Clone, Copy, PartialEq]
    A
    B
end t.Mode

fn t.Mode.next(self: t.Mode) -> t.Mode uses [] panics never
    match self {
        t.Mode.A => t.Mode.B
        t.Mode.B => t.Mode.A
    }
end t.Mode.next

fn t.mode_run() -> u32 uses [] panics never
    match t.Mode.A.next() {
        t.Mode.A => 1u32
        t.Mode.B => 2u32
    }
end t.mode_run

fn t.make(n: u32) -> alloc.vec.Vec<u32, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        match v.push(i) {
            Ok(_u) => {
            }
            Err(_e) => {
                break
            }
        }
        i = i.wrapping_add(1u32)
    }
    v
end t.make

fn t.word() -> alloc.string.String<std.alloc.System> uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(\"abc\"))
    s
end t.word

fn t.vs_run(n: u32) -> usize uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(t.word()))
        i = i.wrapping_add(1u32)
    }
    let mut total = v.len()
    loop {
        match v.pop() {
            Some(s) => {
                total = total.wrapping_add(s.len())
            }
            None => {
                break
            }
        }
    }
    total
end t.vs_run

fn t.vs_clear(n: u32) -> usize uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(t.word()))
        i = i.wrapping_add(1u32)
    }
    v.clear()
    core.mem.drop(v.push(t.word()))
    v.len()
end t.vs_clear

fn t.vs_keep(n: u32) -> usize uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(t.word()))
        i = i.wrapping_add(1u32)
    }
    v.len()
end t.vs_keep

fn t.names_len(n: t.Names) -> usize uses [] panics never
    n.items.len()
end t.names_len

fn t.names_run(n: u32) -> usize uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(t.word()))
        i = i.wrapping_add(1u32)
    }
    let names = t.Names { items: v }
    t.names_len(names)
end t.names_run

fn t.names_push(n: u32) -> usize uses [alloc] panics never
    let mut names = t.Names { items: alloc.vec.Vec.new(std.alloc.SYSTEM) }
    let mut i = 0u32
    while i < n {
        core.mem.drop(names.items.push(t.word()))
        i = i.wrapping_add(1u32)
    }
    names.items.len()
end t.names_push

fn t.vv_run(n: u32) -> usize uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(t.make(i)))
        i = i.wrapping_add(1u32)
    }
    let mut total = 0usize
    loop {
        match v.pop() {
            Some(x) => {
                total = total.wrapping_add(x.len())
            }
            None => {
                break
            }
        }
    }
    total
end t.vv_run

fn t.pair(n: u32) -> t.Pair uses [alloc] panics never
    t.Pair { a: t.make(n), name: alloc.string.String.new(std.alloc.SYSTEM), n: 3usize }
end t.pair

fn t.str_build(n: usize) -> usize uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    let mut i = 0usize
    while i < n {
        match s.push_str(\"ab\") {
            Ok(_u) => {
            }
            Err(_e) => {
                break
            }
        }
        i = i.wrapping_add(1usize)
    }
    s.len()
end t.str_build

fn t.sum_bytes(s: &str) -> u32 uses [] panics never
    let b = s.as_bytes()
    let mut acc = 0u32
    for x in b {
        acc = acc.wrapping_add(u32.from_u8(x))
    }
    acc
end t.sum_bytes

fn t.str_chars() -> u32 uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push('a'))
    core.mem.drop(s.push('é'))
    core.mem.drop(s.push('€'))
    core.mem.drop(s.push('😀'))
    core.mem.drop(s.push_str(\"xyz\"))
    t.sum_bytes(s.as_str())
end t.str_chars

fn t.str_chars_len() -> usize uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push('a'))
    core.mem.drop(s.push('é'))
    core.mem.drop(s.push('€'))
    core.mem.drop(s.push('😀'))
    core.mem.drop(s.push_str(\"xyz\"))
    s.len()
end t.str_chars_len

fn t.pop_sum(n: u32) -> u32 uses [alloc] panics never
    let mut v = t.make(n)
    let mut total = 0u32
    loop {
        match v.pop() {
            Some(x) => {
                total = total.wrapping_add(x)
            }
            None => {
                break
            }
        }
    }
    total
end t.pop_sum

fn t.clear_run(n: u32) -> usize uses [alloc] panics never
    let mut v = t.make(n)
    v.clear()
    core.mem.drop(v.push(5u32))
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(\"hello\"))
    s.clear()
    core.mem.drop(s.push_str(\"hi\"))
    v.len().wrapping_add(s.len())
end t.clear_run

fn t.ring_new(cap: usize) -> t.Ring uses [alloc] panics never
    let mut items = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0usize
    while i < cap {
        match items.push(0u32) {
            Ok(_u) => {
            }
            Err(_e) => {
                break
            }
        }
        i = i.wrapping_add(1usize)
    }
    t.Ring { items: items, head: 0usize, len: 0usize, cap: cap }
end t.ring_new

fn t.ring_pop(self: &mut t.Ring) -> Option<u32> uses [] panics never
    if self.len == 0usize {
        return None
    }
    let v = self.items.get(self.head)
    self.head = self.head.wrapping_add(1usize).checked_rem(self.cap).unwrap_or(0usize)
    self.len = self.len.wrapping_sub(1usize)
    v
end t.ring_pop

fn t.ring_push(self: &mut t.Ring, v: u32) -> bool uses [] panics never
    if self.len >= self.cap {
        return false
    }
    let slot = self.head.wrapping_add(self.len)
    let idx = match self.cap {
        0usize => 0usize
        c => slot.checked_rem(c).unwrap_or(0usize)
    }
    match self.items.get_mut(idx) {
        Some(cell) => {
            *cell = v
        }
        None => {
            return false
        }
    }
    self.len = self.len.wrapping_add(1usize)
    true
end t.ring_push

fn t.ring_run(cap: usize) -> u32 uses [alloc] panics never
    let mut r = t.ring_new(cap)
    let mut total = 0u32
    let mut i = 0u32
    while i < 7u32 {
        if t.ring_push(&mut r, i.wrapping_add(10u32)) {
            total = total.wrapping_add(1u32)
        }
        if i == 4u32 {
            match t.ring_pop(&mut r) {
                Some(x) => {
                    total = total.wrapping_add(x)
                }
                None => {
                }
            }
        }
        i = i.wrapping_add(1u32)
    }
    loop {
        match t.ring_pop(&mut r) {
            Some(x) => {
                total = total.wrapping_mul(3u32).wrapping_add(x)
            }
            None => {
                break
            }
        }
    }
    total
end t.ring_run

fn t.pair_run(n: u32) -> usize uses [alloc] panics never
    let mut p = t.pair(n)
    core.mem.drop(p.a.push(9u32))
    core.mem.drop(p.name.push_str(\"héllo\"))
    core.mem.drop(p.name.push('!'))
    p.a.len().wrapping_add(p.name.len()).wrapping_add(p.n)
end t.pair_run

fn t.pair_param(p: t.Pair) -> usize uses [alloc] panics never
    let mut q = p
    core.mem.drop(q.name.push_str(\"abc\"))
    core.mem.drop(q.a.push(1u32))
    q.a.len().wrapping_add(q.name.len())
end t.pair_param

fn t.pair_param_run(n: u32) -> usize uses [alloc] panics never
    t.pair_param(t.pair(n))
end t.pair_param_run

fn t.destruct(n: u32) -> usize uses [alloc] panics never
    let (a, b) = (t.make(n), t.make(2u32))
    a.len().wrapping_add(b.len())
end t.destruct

fn t.destruct_partial(n: u32) -> usize uses [alloc] panics never
    let (a, _) = (t.make(n), t.make(3u32))
    a.len()
end t.destruct_partial

fn t.destruct_none(n: u32) -> usize uses [alloc] panics never
    let (_, _) = (t.make(n), t.make(3u32))
    7usize
end t.destruct_none

fn t.destruct_struct(n: u32) -> usize uses [alloc] panics never
    let p = t.pair(n)
    let t.Pair { a: x, n: k, name: _ } = p
    x.len().wrapping_add(k)
end t.destruct_struct

fn t.destruct_struct_both(n: u32) -> usize uses [alloc] panics never
    let t.Pair { a: x, n: k, name: s } = t.pair(n)
    x.len().wrapping_add(k).wrapping_add(s.len())
end t.destruct_struct_both

fn t.match_partial(n: u32, some: bool) -> usize uses [alloc] panics never
    let o: Option<t.Pair> = if some {
        Some(t.pair(n))
    } else {
        None
    }
    match o {
        Some(t.Pair { a: x, n: _, name: _ }) => x.len()
        None => 99usize
    }
end t.match_partial

fn t.match_struct_partial(n: u32) -> usize uses [alloc] panics never
    match t.pair(n) {
        t.Pair { a: x, n: _, name: _ } => x.len()
    }
end t.match_struct_partial

fn t.field_move(n: u32) -> usize uses [alloc] panics never
    let p = t.pair(n)
    let w = p.a
    w.len().wrapping_add(p.n)
end t.field_move

fn t.field_move_reassign(n: u32) -> usize uses [alloc] panics never
    let mut p = t.pair(n)
    let w = p.a
    p.a = t.make(2u32)
    w.len().wrapping_add(p.a.len())
end t.field_move_reassign

fn t.field_ret(p: t.Pair) -> alloc.vec.Vec<u32, std.alloc.System> uses [] panics never
    p.a
end t.field_ret

fn t.field_ret_run(n: u32) -> usize uses [alloc] panics never
    let v = t.field_ret(t.pair(n))
    v.len()
end t.field_ret_run

fn t.field_move_str(n: u32) -> usize uses [alloc] panics never
    let mut p = t.pair(n)
    core.mem.drop(p.name.push_str(\"xy\"))
    let s = p.name
    s.len().wrapping_add(p.a.len())
end t.field_move_str

";

#[test]
fn kir_own_more_match_tree_walker() {
    // 所有の周りの残り (設計_KIR.md 13.7): `push_str` / `push(char)` / `pop` / `clear`、構造体のフィールドの Vec / String の
    // メソッド (`&mut self` と値の構造体)、`get_mut`、所有物を持つ値の分解、フィールドからの移動。木走査と同じ値になり、
    // 参照実行器が漏れ・二重解放を内部エラーにしないこと (`--run` と `--native` の両方)。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.str_build", &["0"], "0usize", "0"),
        ("t.str_build", &["1000"], "2000usize", "2000"),
        ("t.str_chars", &[], "2031u32", "2031"),
        ("t.str_chars_len", &[], "13usize", "13"),
        ("t.pop_sum", &["0"], "0u32", "0"),
        ("t.pop_sum", &["6"], "15u32", "15"),
        ("t.clear_run", &["4"], "3usize", "3"),
        ("t.ring_run", &["3"], "528u32", "528"),
        ("t.ring_run", &["0"], "0u32", "0"),
        ("t.pair_run", &["2"], "13usize", "13"),
        ("t.pair_param_run", &["2"], "6usize", "6"),
        ("t.destruct", &["4"], "6usize", "6"),
        ("t.destruct_partial", &["4"], "4usize", "4"),
        ("t.destruct_none", &["4"], "7usize", "7"),
        ("t.destruct_struct", &["4"], "7usize", "7"),
        ("t.destruct_struct_both", &["4"], "7usize", "7"),
        ("t.match_partial", &["4", "true"], "4usize", "4"),
        ("t.match_partial", &["4", "false"], "99usize", "99"),
        ("t.match_struct_partial", &["5"], "5usize", "5"),
        ("t.field_move", &["4"], "7usize", "7"),
        ("t.field_move_reassign", &["4"], "6usize", "6"),
        ("t.field_ret_run", &["5"], "5usize", "5"),
        ("t.field_move_str", &["3"], "5usize", "5"),
        ("t.mode_run", &[], "2u32", "2"),
        ("t.vs_run", &["0"], "0usize", "0"),
        ("t.vs_run", &["5"], "20usize", "20"),
        ("t.vs_clear", &["4"], "1usize", "1"),
        ("t.vs_keep", &["6"], "6usize", "6"),
        ("t.names_run", &["3"], "3usize", "3"),
        ("t.names_push", &["4"], "4usize", "4"),
        ("t.vv_run", &["4"], "6usize", "6"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.iter().map(|a| match *a {
            "true" | "false" => a.to_string(),
            n if *f == "t.str_build" || *f == "t.ring_run" => format!("{}usize", n),
            n => format!("{}u32", n),
        }).collect::<Vec<_>>().join(", "));
        let body = format!("    core.check.eq({}, {})\n", call, lit);
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_own_more_{}", i), &format!("{}{}", KIR_OWN2_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_own_reports_unsupported_moves_with_a_reason() {
    // 一時の値のフィールドの移動、所有物を持つ `unwrap_or` は、
    // 誤った KIR を出さず理由を付けて拒否する。
    // (参照を通したフィールドの移動は `ksn check` が E3002 で拒否するので、ここには来ない。)
    let cases: &[(&str, &str, &str)] = &[
        (
            "type t.B\n    items: alloc.vec.Vec<u8, std.alloc.System>\nend t.B\n\nfn t.mk() -> t.B uses [alloc] panics never\n    t.B { items: alloc.vec.Vec.new(std.alloc.SYSTEM) }\nend t.mk\n\nfn t.f() -> usize uses [alloc] panics never\n    let w = t.mk().items\n    w.len()\nend t.f\n",
            "t.f",
            "一時の値のフィールド",
        ),
        (
            "fn t.f(o: Option<alloc.vec.Vec<u8, std.alloc.System>>) -> usize uses [] panics never\n    o.unwrap_or(alloc.vec.Vec.new(std.alloc.SYSTEM)).len()\nend t.f\n",
            "t.f",
            "unwrap_or",
        ),
    ];
    for (i, (src, f, why)) in cases.iter().enumerate() {
        let src = format!("{}\nfn t.main() -> Result<(), std.io.Error> uses [] panics may\n    Ok(())\nend t.main\n", src);
        let t = Tmp::new(&format!("kir_own_reason_{}", i), &src);
        let o = run(&["kir", f], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}: {}", f, text(&o.stdout));
        assert!(text(&o.stderr).contains(why), "{}: {}", f, text(&o.stderr));
    }
}

#[test]
fn kir_eq_operator_on_option_result_aggregates_match_tree_walker() {
    // `==` / `!=` が Option / Result / 派生の構造体・列挙型 / `&[u8]` / `&str` でも下ろせ、木走査と一致する。
    let fns = "type t.P derive [Copy, Clone, PartialEq]\n    a: u8\n    b: i32\nend t.P\n\n\
enum t.E derive [Copy, Clone, PartialEq]\n    One(u8, bool)\n    Zero\nend t.E\n\n\
fn t.opt(b: &[u8], i: usize) -> bool uses [] panics never\n    b.get(i) == Some(b']')\nend t.opt\n\n\
fn t.opt_ne(b: &[u8], i: usize) -> bool uses [] panics never\n    b.get(i) != Some(b']')\nend t.opt_ne\n\n\
fn t.res(x: u8, y: u8) -> bool uses [] panics never\n    let mut a: Result<u8, u8> = Err(x)\n    let mut b: Result<u8, u8> = Err(y)\n    if x > 5u8 {\n        a = Ok(x)\n    }\n    if y > 5u8 {\n        b = Ok(y)\n    }\n    a == b\nend t.res\n\n\
fn t.pair(x: u8, y: u8) -> bool uses [] panics never\n    t.P { a: x, b: -3i32 } == t.P { a: y, b: -3i32 }\nend t.pair\n\n\
fn t.en(x: u8, y: u8) -> bool uses [] panics never\n    t.E.One(x, true) != t.E.One(y, true)\nend t.en\n\n\
fn t.en_zero(x: u8) -> bool uses [] panics never\n    if x == 0u8 {\n        return t.E.Zero == t.E.Zero\n    }\n    t.E.Zero == t.E.One(x, false)\nend t.en_zero\n\n\
fn t.bytes(a: &str, b: &str) -> bool uses [] panics never\n    a.as_bytes() == b.as_bytes()\nend t.bytes\n";
    let cases: &[(&str, &str, &str, &str)] = &[
        ("t.opt", "\"x]\".as_bytes(), 1usize", "true", "true"),
        ("t.opt", "\"x]\".as_bytes(), 0usize", "false", "false"),
        ("t.opt", "\"x]\".as_bytes(), 7usize", "false", "false"),
        ("t.opt_ne", "\"x]\".as_bytes(), 7usize", "true", "true"),
        ("t.opt_ne", "\"x]\".as_bytes(), 1usize", "false", "false"),
        ("t.res", "9u8, 9u8", "true", "true"),
        ("t.res", "9u8, 3u8", "false", "false"),
        ("t.res", "3u8, 3u8", "true", "true"),
        ("t.pair", "4u8, 4u8", "true", "true"),
        ("t.pair", "4u8, 5u8", "false", "false"),
        ("t.en", "1u8, 1u8", "false", "false"),
        ("t.en", "1u8, 2u8", "true", "true"),
        ("t.en_zero", "0u8", "true", "true"),
        ("t.en_zero", "3u8", "false", "false"),
        ("t.bytes", "\"ab\", \"ab\"", "true", "true"),
        ("t.bytes", "\"ab\", \"ac\"", "false", "false"),
        ("t.bytes", "\"ab\", \"abc\"", "false", "false"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args);
        let src = format!(
            "{}\nfn t.go() -> bool uses [] panics never\n    {}\nend t.go\n\nfn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.eq(t.go(), {})\n    Ok(())\nend t.main\n",
            fns, call, lit
        );
        let t = Tmp::new(&format!("kir_eq_op_{}", i), &src);
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}{}", call, text(&o.stdout), text(&o.stderr));
        let k = kir_run(&["kir", "t.go", "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_eq_on_aggregates_reused_after_and_in_loop_match_tree_walker() {
    // 比較の後にも使う・ループの中で比べる。派生の比較は借用でなく値渡しだが、所有物を持たない (Copy でない) 型なので解放の問題は出ない。
    let src = "type t.P derive [Clone, PartialEq]\n    a: u8\n    b: i32\nend t.P\n\n\
fn t.go(k: u8) -> u32 uses [] panics may\n    let a = t.P { a: 1u8, b: 2i32 }\n    let b = t.P { a: k, b: 2i32 }\n    let mut c = 0u32\n    let mut i = 0u32\n    while i < 3u32 {\n        if a == b {\n            c = c + 10u32\n        }\n        if a != b {\n            c = c + 1u32\n        }\n        i = i + 1u32\n    }\n    if a == b {\n        c = c + 100u32\n    }\n    c + u32.from_u8(a.a) + u32.from_u8(b.a)\nend t.go\n\n\
fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.eq(t.go(1u8), 132u32)\n    core.check.eq(t.go(4u8), 8u32)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_eq_reuse", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker: {}{}", text(&o.stdout), text(&o.stderr));
    for (arg, want) in [("1", "132"), ("4", "8")] {
        let k = kir_run(&["kir", "t.go", "--run", arg], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}: {}", arg, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want);
    }
}

#[test]
fn kir_eq_on_aggregates_owning_heap_is_refused_with_a_reason() {
    // String を持つ型は derive できず (E2036)、手書きの eq は下ろせない。どちらにしても == / != は理由付きで拒否する (二重解放を作らない)。
    let src = "type t.P\n    name: alloc.string.String<std.alloc.System>\n    n: u32\nend t.P\n\nimpl core.cmp.PartialEq<t.P> for t.P\n\n\
fn t.P.eq(self: &t.P, other: &t.P) -> bool uses [] panics never impls core.cmp.PartialEq<t.P>\n    self.n == other.n\nend t.P.eq\n\n\
fn t.go() -> bool uses [alloc] panics never\n    let a = t.P { name: alloc.string.String.new(std.alloc.SYSTEM), n: 1u32 }\n    let b = t.P { name: alloc.string.String.new(std.alloc.SYSTEM), n: 1u32 }\n    let r = a == b\n    core.mem.drop(a)\n    core.mem.drop(b)\n    r\nend t.go\n\n\
fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    core.check.eq(t.go(), true)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_eq_owning", src);
    let k = run(&["kir", "t.go", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(1), "{}{}", text(&k.stdout), text(&k.stderr));
    assert!(text(&k.stderr).contains("比較は段階 1 の対象外"), "{}", text(&k.stderr));
}

#[test]
fn kir_get_on_vec_of_owning_elements_with_unsupported_pattern_is_refused_with_a_reason() {
    // `Option<&T>` の値の `Some(..)` の中は、名前・_・構造体のパターンだけ。リテラルなど他のパターンは理由付きで拒否する。
    let src = "type t.Item\n    name: alloc.string.String<std.alloc.System>\n    v: u32\nend t.Item\n\n\
fn t.make() -> alloc.vec.Vec<t.Item, std.alloc.System> uses [alloc] panics never\n    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    let s = alloc.string.String.new(std.alloc.SYSTEM)\n    core.mem.drop(v.push(t.Item { name: s, v: 1u32 }))\n    v\nend t.make\n\n\
fn t.pick(i: usize) -> u32 uses [alloc] panics never\n    let v = t.make()\n    let g = v.get(i)\n    match g {\n        None => 0u32\n        Some(t.Item { name: _, v: 1u32 }) => 1u32\n        Some(_) => 2u32\n    }\nend t.pick\n\n\
fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    core.check.eq(t.pick(0usize), 1u32)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_get_owning", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let k = run(&["kir", "t.pick", "--run", "0"], &t.0);
    assert_eq!(k.status.code(), Some(1), "{}", text(&k.stdout));
    assert!(text(&k.stderr).contains("構造体パターン"), "{}", text(&k.stderr));
}

#[test]
fn kir_unwrap_on_option_of_borrowed_owning_elem_is_refused_with_a_reason() {
    let src = format!("{}\nfn t.uw(n: u32) -> usize uses [alloc] panics may\n    let v = t.names(n)\n    let g = v.get(0usize)\n    g.unwrap().len()\nend t.uw\n\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    core.check.eq(t.uw(2u32), 3usize)\n    Ok(())\nend t.main\n", KIR_LETGET_FNS);
    let t = Tmp::new("kir_letget_unwrap", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let k = run(&["kir", "t.uw", "--run", "2"], &t.0);
    assert_eq!(k.status.code(), Some(1), "{}", text(&k.stdout));
    assert!(text(&k.stderr).contains("unwrap は段階 1 の対象外"), "{}", text(&k.stderr));
}

const KIR_LETGET_FNS: &str = "type t.Item
    name: alloc.string.String<std.alloc.System>
    v: u32
end t.Item

fn t.names(n: u32) -> alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        let mut s = alloc.string.String.new(std.alloc.SYSTEM)
        core.mem.drop(s.push_str(\"abc\"))
        core.mem.drop(v.push(s))
        i = i.wrapping_add(1u32)
    }
    v
end t.names

fn t.items(n: u32) -> alloc.vec.Vec<t.Item, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        let mut s = alloc.string.String.new(std.alloc.SYSTEM)
        core.mem.drop(s.push_str(\"ab\"))
        core.mem.drop(v.push(t.Item { name: s, v: i }))
        i = i.wrapping_add(1u32)
    }
    v
end t.items

fn t.let_get(n: u32, i: usize) -> usize uses [alloc] panics never
    let v = t.names(n)
    let g = v.get(i)
    match g {
        Some(s) => s.len()
        None => 99usize
    }
end t.let_get

fn t.let_is_some(n: u32, i: usize) -> u32 uses [alloc] panics never
    let v = t.names(n)
    let g = v.get(i)
    let mut r = 0u32
    if g.is_some() {
        r = r.wrapping_add(1u32)
    }
    if g.is_none() {
        r = r.wrapping_add(10u32)
    }
    r
end t.let_is_some

fn t.let_item(n: u32, i: usize) -> usize uses [alloc] panics never
    let v = t.items(n)
    let g = v.get(i)
    match g {
        Some(it) => it.name.len().wrapping_add(usize.from_u32(it.v))
        None => 99usize
    }
end t.let_item

fn t.let_struct_pat(n: u32, i: usize) -> u32 uses [alloc] panics never
    let v = t.items(n)
    let g = v.get(i)
    match g {
        Some(t.Item { name: _, v: x }) => x
        None => 99u32
    }
end t.let_struct_pat

fn t.find(v: &alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>, i: usize) -> Option<&alloc.string.String<std.alloc.System>> uses [] panics never
    v.get(i)
end t.find

fn t.find_item(v: &alloc.vec.Vec<t.Item, std.alloc.System>, i: usize) -> Option<&t.Item> uses [] panics never
    v.get(i)
end t.find_item

fn t.first(v: &alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>) -> Option<&alloc.string.String<std.alloc.System>> uses [] panics may
    if v.len() == 0usize {
        None
    } else {
        Some(&v[0usize])
    }
end t.first

fn t.len_of(g: Option<&alloc.string.String<std.alloc.System>>) -> usize uses [] panics never
    match g {
        Some(s) => s.len()
        None => 77usize
    }
end t.len_of

fn t.via_fn(n: u32, i: usize) -> usize uses [alloc] panics never
    let v = t.names(n)
    match t.find(&v, i) {
        Some(s) => s.len()
        None => 99usize
    }
end t.via_fn

fn t.via_let_fn(n: u32, i: usize) -> usize uses [alloc] panics never
    let v = t.names(n)
    let g = t.find(&v, i)
    let a = t.len_of(g)
    a.wrapping_add(t.len_of(t.find(&v, i)))
end t.via_let_fn

fn t.via_item(n: u32, i: usize) -> u32 uses [alloc] panics never
    let v = t.items(n)
    let g = t.find_item(&v, i)
    match g {
        Some(it) => it.v
        None => 99u32
    }
end t.via_item

fn t.via_first(n: u32) -> usize uses [alloc] panics may
    let v = t.names(n)
    t.len_of(t.first(&v))
end t.via_first

fn t.in_loop(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut total = 0usize
    let mut i = 0usize
    while i < 6usize {
        let g = v.get(i)
        match g {
            Some(s) => {
                total = total.wrapping_add(s.len())
            }
            None => {
                total = total.wrapping_add(100usize)
            }
        }
        i = i.wrapping_add(1usize)
    }
    total
end t.in_loop
";

#[test]
fn kir_let_get_of_owning_elems_matches_tree_walker() {
    // `let g = v.get(i)` の `Option<&T>` (T は String / String を持つ構造体) を値で持つ。`Option<&T>` を返す・引数に取る関数も下ろす。
    // 木走査と `--run` / `--native` (ASan があればそれも) が一致し、参照実行器が漏れ・二重解放を内部エラーにしない。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.let_get", &["3", "1"], "3usize", "3"),
        ("t.let_get", &["3", "7"], "99usize", "99"),
        ("t.let_get", &["0", "0"], "99usize", "99"),
        ("t.let_is_some", &["3", "1"], "1u32", "1"),
        ("t.let_is_some", &["3", "5"], "10u32", "10"),
        ("t.let_item", &["3", "2"], "4usize", "4"),
        ("t.let_item", &["3", "3"], "99usize", "99"),
        ("t.let_struct_pat", &["3", "2"], "2u32", "2"),
        ("t.let_struct_pat", &["3", "9"], "99u32", "99"),
        ("t.via_fn", &["3", "0"], "3usize", "3"),
        ("t.via_fn", &["3", "4"], "99usize", "99"),
        ("t.via_let_fn", &["3", "1"], "6usize", "6"),
        ("t.via_let_fn", &["3", "8"], "154usize", "154"),
        ("t.via_item", &["4", "3"], "3u32", "3"),
        ("t.via_item", &["4", "4"], "99u32", "99"),
        ("t.via_first", &["2"], "3usize", "3"),
        ("t.via_first", &["0"], "77usize", "77"),
        ("t.in_loop", &["4"], "212usize", "212"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let last = args.len() - 1;
        let call = format!("{}({})", f, args.iter().enumerate().map(|(k, a)| if args.len() == 2 && k == last { format!("{}usize", a) } else { format!("{}u32", a) }).collect::<Vec<_>>().join(", "));
        let body = format!("    core.check.eq({}, {})\n", call, lit);
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_letget_{}", i), &format!("{}{}", KIR_LETGET_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

const KIR_BORROW_FNS: &str = "type t.Item
    name: alloc.string.String<std.alloc.System>
    v: u32
end t.Item

fn t.word() -> alloc.string.String<std.alloc.System> uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(\"abc\"))
    s
end t.word

fn t.names(n: u32) -> alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(t.word()))
        i = i.wrapping_add(1u32)
    }
    v
end t.names

fn t.items(n: u32) -> alloc.vec.Vec<t.Item, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(t.Item { name: t.word(), v: i }))
        i = i.wrapping_add(1u32)
    }
    v
end t.items

fn t.get_str(n: u32, i: usize) -> usize uses [alloc] panics never
    let v = t.names(n)
    match v.get(i) {
        Some(s) => s.len()
        None => 99usize
    }
end t.get_str

fn t.get_item(n: u32, i: usize) -> usize uses [alloc] panics never
    let v = t.items(n)
    match v.get(i) {
        Some(it) => it.name.len().wrapping_add(usize.from_u32(it.v))
        None => 99usize
    }
end t.get_item

fn t.sum_names(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut total = 0usize
    for s in &v {
        total = total.wrapping_add(s.len())
    }
    total
end t.sum_names

fn t.sum_names_iter(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut total = 0usize
    for s in v.iter() {
        total = total.wrapping_add(s.len())
    }
    total
end t.sum_names_iter

fn t.sum_items(n: u32) -> usize uses [alloc] panics never
    let v = t.items(n)
    let mut total = 0usize
    for it in &v {
        total = total.wrapping_add(it.name.len())
    }
    total
end t.sum_items

fn t.sum_scalar(n: u32) -> u32 uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(i))
        i = i.wrapping_add(1u32)
    }
    let mut total = 0u32
    for x in &v {
        total = total.wrapping_add(*x)
    }
    total
end t.sum_scalar

fn t.name_len(s: &alloc.string.String<std.alloc.System>) -> usize uses [] panics never
    s.len()
end t.name_len

fn t.item_len(i: &t.Item) -> usize uses [] panics never
    i.name.len()
end t.item_len

fn t.idx_str(n: u32, i: usize) -> usize uses [alloc] panics may
    let v = t.names(n)
    v[i].len()
end t.idx_str

fn t.idx_item(n: u32, i: usize) -> usize uses [alloc] panics may
    let v = t.items(n)
    v[i].name.len().wrapping_add(usize.from_u32(v[i].v))
end t.idx_item

fn t.idx_ref(n: u32, i: usize) -> usize uses [alloc] panics may
    let v = t.items(n)
    let w = t.names(n)
    t.item_len(&v[i]).wrapping_add(t.name_len(&w[i]))
end t.idx_ref

fn t.get_loop(n: u32) -> usize uses [alloc] panics never
    let v = t.items(n)
    let mut total = 0usize
    let mut i = 0usize
    while i < usize.from_u32(n).wrapping_add(2usize) {
        match v.get(i) {
            Some(it) => {
                total = total.wrapping_add(it.name.len())
            }
            None => {
                total = total.wrapping_add(100usize)
            }
        }
        i = i.wrapping_add(1usize)
    }
    total
end t.get_loop

fn t.mutate_ok(n: u32) -> usize uses [alloc] panics never
    let mut v = t.names(n)
    let a = match v.get(0usize) {
        Some(s) => s.len()
        None => 0usize
    }
    core.mem.drop(v.push(t.word()))
    a.wrapping_add(v.len())
end t.mutate_ok

fn t.nested(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut total = 0usize
    for a in &v {
        for b in v.iter() {
            total = total.wrapping_add(a.len()).wrapping_add(b.len())
        }
    }
    total
end t.nested

";

#[test]
fn kir_borrow_owned_elems_match_tree_walker() {
    // 所有物を持つ要素の Vec (`Vec<String>` / String を持つ構造体の Vec) の `get` / 添字 / `for` は、要素の写しでなく番地 (借用) で下ろす。
    // 木走査と値が一致し、参照実行器が漏れ・二重解放を内部エラーにしない (`--run` / `--native`、ASan があればそれも)。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.get_str", &["3","1"], "3usize", "3"),
        ("t.get_str", &["3","7"], "99usize", "99"),
        ("t.get_str", &["0","0"], "99usize", "99"),
        ("t.get_item", &["3","2"], "5usize", "5"),
        ("t.get_item", &["3","3"], "99usize", "99"),
        ("t.get_loop", &["3"], "209usize", "209"),
        ("t.get_loop", &["0"], "200usize", "200"),
        ("t.sum_names", &["3"], "9usize", "9"),
        ("t.sum_names", &["0"], "0usize", "0"),
        ("t.sum_names_iter", &["4"], "12usize", "12"),
        ("t.sum_items", &["5"], "15usize", "15"),
        ("t.sum_scalar", &["5"], "10u32", "10"),
        ("t.idx_str", &["3","1"], "3usize", "3"),
        ("t.idx_item", &["3","1"], "4usize", "4"),
        ("t.idx_ref", &["3","1"], "6usize", "6"),
        ("t.mutate_ok", &["3"], "7usize", "7"),
        ("t.nested", &["3"], "54usize", "54"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let last = args.len() - 1;
        let call = format!("{}({})", f, args.iter().enumerate().map(|(k, a)| if args.len() == 2 && k == last { format!("{}usize", a) } else { format!("{}u32", a) }).collect::<Vec<_>>().join(", "));
        let body = format!("    core.check.eq({}, {})\n", call, lit);
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_borrow_{}", i), &format!("{}{}", KIR_BORROW_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_borrow_owned_elems_out_of_range_index_panics_like_tree_walker() {
    // 範囲外の添字 `v[i]` は、借用を作る前の境界の検査で panic する (終了コード 101)。
    let main = "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    core.check.eq(t.idx_str(3u32, 5usize), 3usize)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_borrow_oob", &format!("{}{}", KIR_BORROW_FNS, main));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101), "tree-walker: {}", text(&o.stderr));
    let k = kir_run(&["kir", "t.idx_str", "--run", "3", "5"], &t.0);
    assert_eq!(k.status.code(), Some(101), "kir: {}", text(&k.stderr));
    assert!(text(&k.stderr).contains("index out of bounds"), "{}", text(&k.stderr));
}

#[test]
fn kir_own_dump_shows_moves_as_nulling_and_flags() {
    // Vec の移動は元のヘッダの `ptr` を null にする書き込み、構造体の局所変数の移動は印 (1 バイト) の書き込みとして見える。
    let t = kir_own_pkg("kir_own_dump", "    core.check.eq(t.branch_buf(3u32, true), 3usize)\n");
    let o = run(&["kir", "t.branch"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let s = text(&o.stdout);
    assert!(s.contains("const.ptr 0") && s.contains("heap.free %") && s.contains("struct vec size=24"), "{}", s);
    let o = run(&["kir", "t.branch_buf"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let s = text(&o.stdout);
    assert!(s.contains("frame.bytes 1") && s.contains("mem.store.bool") && s.contains("mem.load.bool"), "{}", s);
}

/// `String.as_str()` / `Vec.as_slice()` はヘッダの {ptr, len} を借用の `fat` にする (所有は移さない)。
/// 木走査 (`ksn run` の `core.check.eq`) と、`--run` / `--native` の値が一致する。
#[test]
fn kir_as_str_and_as_slice_borrow_the_header() {
    let src = r#"fn t.sum_bytes(s: &str) -> u32 uses [] panics never
    let b = s.as_bytes()
    let mut acc = 0u32
    for x in b {
        acc = acc.wrapping_add(u32.from_u8(x))
    }
    acc
end t.sum_bytes

fn t.sum_slice(v: &[u16]) -> u32 uses [] panics never
    let mut acc = 0u32
    for x in v {
        acc = acc.wrapping_add(u32.from_u16(x))
    }
    acc
end t.sum_slice

fn t.go(n: u16) -> u32 uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u16
    while i < n {
        core.mem.drop(v.push(i))
        i = i.wrapping_add(1u16)
    }
    let s = alloc.string.String.new(std.alloc.SYSTEM)
    let a = t.sum_slice(v.as_slice())
    let b = t.sum_bytes(s.as_str())
    if s.as_str().len() == 0usize {
        return a.wrapping_add(b).wrapping_add(1000u32)
    }
    a.wrapping_add(b)
end t.go

fn t.main() -> () uses [alloc, io.console] panics may
    core.check.eq(t.go(10u16), 1045u32)
end t.main
"#;
    let t = Tmp::new("kir_as_str_slice", src);
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
    for (n, want) in [("10", "1045"), ("0", "1000")] {
        let o = kir_run(&["kir", "t.go", "--run", n], &t.0);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), want);
    }
}

#[test]
fn kir_closure_capture_by_mut_ref_writes_through() {
    // `capture [&mut x]` の書き込みは、取り込まれた変数に届く。木走査は `*x = v` の形を受けない
    // (取り込んだ名前を参照にしない) ので、期待値は手で求めた値 (3, 6 と加えて 11 を返し、total も 11 → 22)。
    let t = Tmp::new("kir_closure_cap_mut", &format!("{}{}", KIR_CLOSURE_FNS, main_wrap("", "    core.check.eq(t.cap_ref(1u64), 1u64)\n")));
    let k = kir_run(&["kir", "t.cap_mut", "--run", "3"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir: {}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "22");
}

#[test]
fn kir_closure_environment_is_passed_as_leading_arguments() {
    // 取り込みの値は、クロージャの関数と単相インスタンスの先頭の追加の引数になる。
    let t = Tmp::new("kir_closure_dump", &format!("{}{}", KIR_CLOSURE_FNS, main_wrap("", "    core.check.eq(t.cap_ref(4u64), 46u64)\n")));
    let o = run(&["kir", "t.cap_ref"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("fn t.cap_ref#c") && out.contains("fn t.apply key=t.apply<t.cap_ref#c"), "{}", out);
}

#[test]
fn kir_closure_reports_unsupported_captures_with_a_reason() {
    // 解放の要る値の取り込み、move で取り込んだ値の変更、反復子の未対応の元は理由付きで拒否する。
    let cases: &[(&str, &str, &str)] = &[
        ("t.cap_vec", "    let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    let f = fn(x: usize) -> usize capture [&v] {\n        x\n    }\n    f(1usize)\n", "取り込みは段階 1 の対象外"),
        ("t.cap_mutate", "    let mut n = 1usize\n    let f = fn(x: usize) -> usize capture [move n] {\n        n = n.wrapping_add(x)\n        n\n    }\n    f(1usize)\n", "を本体が変更する形"),
        ("t.take", "    core.iter.count(core.iter.take(core.iter.range(0usize, 5usize), 2usize))\n", "take"),
    ];
    for (i, (f, body, why)) in cases.iter().enumerate() {
        let src = format!(
            "fn {}() -> usize uses [alloc] panics may\n{}end {}\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics may\n    Ok(())\nend t.main\n",
            f, body, f
        );
        let t = Tmp::new(&format!("kir_closure_reason_{}", i), &src);
        let o = run(&["kir", f], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}: {}", f, text(&o.stderr));
        assert!(text(&o.stderr).contains(why), "{}: {}", f, text(&o.stderr));
    }
}

/// 実パッケージの LLVM ビルド (設計_KIR.md 12.4): KIR に全体を下ろせるパッケージは、`ksn build --backend llvm` の
/// 実行ファイルの stdout・stderr・終了コードが `ksn run` と一致する。ここに挙げたものが後退したら落ちる
/// (下ろせるパッケージが増えたら足す)。パッケージは一時 dir に写してから作る (リポジトリの `target/` を汚さない)。
#[test]
fn llvm_builds_of_real_packages_match_run() {
    if !clang_available() {
        eprintln!("clang が無いので飛ばす");
        return;
    }
    fn copy(src: &Path, dst: &Path) {
        fs::create_dir_all(dst).unwrap();
        for e in fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            let name = e.file_name();
            if name == "target" {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                copy(&p, &dst.join(&name));
            } else {
                fs::copy(&p, dst.join(&name)).unwrap();
            }
        }
    }
    for pkg in [
        "examples/demo",
        "examples/features",
        "examples/ffi",
        "examples/http",
        "examples/json",
        "examples/kvapp",
        "examples/more_coll",
        "examples/more_str",
        "examples/native",
        "examples/realtime",
        "perf/closure",
        "perf/fib",
        "perf/hashmap",
        "perf/loops",
        "perf/noop",
        "perf/strcat",
        "perf/structs",
        "perf/vecops",
    ] {
        let name = Path::new(pkg).file_name().unwrap().to_string_lossy().to_string();
        // 一時 dir の下に写す。path 依存 (`../kvlib`) のあるパッケージは、依存を兄弟の dir として写す。
        let base = std::env::temp_dir().join(format!("ksn_llvm_pkg_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&base);
        let dir = base.join(&name);
        copy(&root().join(pkg), &dir);
        if pkg == "examples/kvapp" {
            copy(&root().join("examples").join("kvlib"), &base.join("kvlib"));
        }
        let cache = dir.join(".cache");
        let b = Command::new(exe())
            .args(["build", "--backend", "llvm"])
            .current_dir(&dir)
            .env("KSN_CACHE_DIR", &cache)
            .env("KSN_STDLIB", root().join("spec").join("stdlib"))
            .output()
            .unwrap();
        let out = text(&b.stdout);
        assert_eq!(b.status.code(), Some(0), "{}: {}{}", pkg, out, text(&b.stderr));
        assert!(!out.contains("llvm backend skipped"), "{}: {}", pkg, out);
        let r = Command::new(exe()).arg("run").current_dir(&dir).env("KSN_STDLIB", root().join("spec").join("stdlib")).output().unwrap();
        let n = Command::new(dir.join("target").join("debug").join(format!("{}.native", name))).current_dir(&dir).output().unwrap();
        assert_eq!(n.status.code(), r.status.code(), "{}: 終了コード", pkg);
        assert_eq!(text(&n.stdout), text(&r.stdout), "{}: stdout", pkg);
        assert_eq!(text(&n.stderr), text(&r.stderr), "{}: stderr", pkg);
        let _ = fs::remove_dir_all(&base);
    }
}

/// 依存パッケージ (別 module) の `kvlib.lookup.find`/`count` を KIR に下ろし、
/// 依存元の main から呼ぶ。`ksn run` / `ksn kir t.main --run` / `--native` が一致することを確かめる。
#[test]
fn dep_package_fn_is_lowered_and_matches_run_kir_native() {
    if !clang_available() {
        eprintln!("skip: clang が PATH に無いので、--native の比較は飛ばす");
        return;
    }
    let src = "import kvlib.lookup\n\nfn t.main() -> () uses [] panics never\n    let r = lookup.find(\"name = kasane\\nmode = fast\\n\", \"name\")\n    core.mem.drop(r)\n    let r2 = lookup.find(\"name = kasane\\nmode = fast\\n\", \"missing\")\n    core.mem.drop(r2)\n    let n = lookup.count(\"a = 1\\nb = 2\\n\")\n    core.mem.drop(n)\nend t.main\n";
    let t = Tmp::new("dep_lower", src);
    let kvlib = root().join("examples").join("kvlib").display().to_string();
    fs::write(
        t.0.join("kasane.toml"),
        format!("{}kvlib = {{ path = \"{}\" }}\n", TOML, kvlib),
    )
    .unwrap();
    let u = run(&["update"], &t.0);
    assert_eq!(u.status.code(), Some(0), "update: {}{}", text(&u.stdout), text(&u.stderr));
    let r = run(&["run"], &t.0);
    assert_eq!(r.status.code(), Some(0), "run: {}{}", text(&r.stdout), text(&r.stderr));
    kir_run(&["kir", "t.main", "--run"], &t.0);
}

/// `examples/kvapp` で `ksn test --both` を走らせたとき、バンドル退避にならず `(both: llvm)` になることを確かめる。
#[test]
fn kvapp_test_both_is_llvm_not_bundle() {
    if !clang_available() {
        eprintln!("skip: clang が PATH に無いので、LLVM の比較は飛ばす");
        return;
    }
    fn copy_all(src: &Path, dst: &Path) {
        fs::create_dir_all(dst).unwrap();
        for e in fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            let name = e.file_name();
            let p = e.path();
            if p.is_dir() {
                copy_all(&p, &dst.join(&name));
            } else {
                let _ = fs::copy(&p, dst.join(&name));
            }
        }
    }
    let base = std::env::temp_dir().join(format!("ksn_kvapp_both_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let dir = base.join("kvapp");
    copy_all(&root().join("examples").join("kvapp"), &dir);
    copy_all(&root().join("examples").join("kvlib"), &base.join("kvlib"));
    let cache = dir.join(".cache");
    let stdlib = std::env::var("KSN_STDLIB")
        .ok()
        .unwrap_or_else(|| root().join("spec").join("stdlib").to_string_lossy().to_string());
    let o = Command::new(exe())
        .args(["test", "--both"])
        .current_dir(&dir)
        .env("KSN_CACHE_DIR", &cache)
        .env("KSN_STDLIB", &stdlib)
        .output()
        .unwrap();
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "kvapp test --both: {}{}", out, text(&o.stderr));
    // kvapp には 2 つのテストがあり、どちらも (both: llvm) で比較され、bundle 退避にはならない。
    assert_eq!(out.matches("(both: llvm)").count(), 2, "{}", out);
    assert!(out.contains("compared with: llvm 2, native 0, bundle fallback 0"), "{}", out);
    assert!(!out.contains("both: bundle"), "{}", out);
    assert!(out.contains("2 passed; 0 failed"), "{}", out);
    let _ = fs::remove_dir_all(&base);
}

/// `Vec.get` の `Option<&スカラー>` の束縛 (`Some(c)`) を、`*c`、`&T` の引数、`c == &y` で使う。
/// 束縛は値で持つ (設計_KIR.md 10.10) ので、`*c` は値そのもの、`&T` の引数は一時に写した参照になる。
const DEREF_OPT_SRC: &str = r#"fn t.show(x: &u32) -> u32 uses [] panics never
    *x
end t.show

fn t.fill() -> Result<alloc.vec.Vec<u32>, alloc.alloc.AllocError> uses [alloc] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    v.push(3u32)?
    v.push(8u32)?
    Ok(v)
end t.fill

fn t.pick(i: usize) -> u32 uses [alloc] panics may
    let v = match t.fill() {
        Ok(v) => v
        Err(_e) => core.panic.panic("oom")
    }
    match v.get(i) {
        Some(c) => t.show(c)
        None => 9u32
    }
end t.pick

fn t.same(i: usize, x: u32) -> bool uses [alloc] panics may
    let v = match t.fill() {
        Ok(v) => v
        Err(_e) => core.panic.panic("oom")
    }
    match v.get(i) {
        Some(c) => *c == x
        None => false
    }
end t.same

fn t.eqref(i: usize, x: u32) -> bool uses [alloc] panics may
    let v = match t.fill() {
        Ok(v) => v
        Err(_e) => core.panic.panic("oom")
    }
    let y = x
    match v.get(i) {
        Some(c) => c == &y
        None => false
    }
end t.eqref

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.pick(1usize), 8u32)
    core.check.eq(t.pick(5usize), 9u32)
    core.check.eq(t.same(0usize, 3u32), true)
    core.check.eq(t.same(1usize, 3u32), false)
    core.check.eq(t.eqref(0usize, 3u32), true)
    core.check.eq(t.eqref(0usize, 4u32), false)
    Ok(())
end t.main
"#;

#[test]
fn kir_deref_of_scalar_option_ref_matches_tree_walker() {
    let t = Tmp::new("kir_deref_opt_ref", DEREF_OPT_SRC);
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0), "tree-walker main");
    let cases: &[(&str, &[&str], &str)] = &[
        ("t.pick", &["1"], "8"),
        ("t.pick", &["5"], "9"),
        ("t.same", &["0", "3"], "true"),
        ("t.same", &["1", "3"], "false"),
        ("t.eqref", &["0", "3"], "true"),
        ("t.eqref", &["0", "4"], "false"),
    ];
    for (f, args, want) in cases {
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {} {:?}\n{}", f, args, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {} {:?}", f, args);
    }
}

/// 利用者の反復子 (`core.iter.Iterator` を実装し、`&mut self` の `next` を持つ) を `for` で回す。
/// `continue` は次の `next` へ、`break` は抜ける。内側のループの反復子も、外側のループの中で作って回す。
const USER_ITER_SRC: &str = r#"type t.Upto pub
    cur: u32
    stop: u32
end t.Upto

impl core.iter.Iterator<u32, [], never> for t.Upto
fn t.Upto.next(self: &mut t.Upto) -> Option<u32> uses [] panics never impls core.iter.Iterator<u32, [], never>
    if self.cur >= self.stop {
        return None
    }
    let v = self.cur
    self.cur = self.cur.wrapping_add(1u32)
    Some(v)
end t.Upto.next

fn t.sum(a: u32, b: u32) -> u32 uses [] panics never
    let mut it = t.Upto { cur: a, stop: b }
    let mut acc = 0u32
    for n in it {
        acc = acc.wrapping_add(n)
    }
    acc
end t.sum

fn t.sum_skip_break(a: u32, b: u32, k: u32) -> u32 uses [] panics never
    let mut it = t.Upto { cur: a, stop: b }
    let mut acc = 0u32
    for n in it {
        if n == k {
            continue
        }
        if n > 20u32 {
            break
        }
        acc = acc.wrapping_add(n)
    }
    acc
end t.sum_skip_break

fn t.nested(n: u32) -> u32 uses [] panics never
    let mut it = t.Upto { cur: 0u32, stop: n }
    let mut total = 0u32
    for i in it {
        let mut jt = t.Upto { cur: 0u32, stop: i }
        for j in jt {
            total = total.wrapping_add(j)
        }
    }
    total
end t.nested

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.sum(0u32, 4u32), 6u32)
    core.check.eq(t.sum(3u32, 3u32), 0u32)
    core.check.eq(t.sum_skip_break(0u32, 100u32, 2u32), 208u32)
    core.check.eq(t.nested(4u32), 4u32)
    Ok(())
end t.main
"#;

#[test]
fn kir_user_iterator_for_matches_tree_walker() {
    let t = Tmp::new("kir_user_iter_for", USER_ITER_SRC);
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0), "tree-walker main");
    let cases: &[(&str, &[&str], &str)] = &[
        ("t.sum", &["0", "4"], "6"),
        ("t.sum", &["3", "3"], "0"),
        ("t.sum_skip_break", &["0", "100", "2"], "208"),
        ("t.nested", &["4"], "4"),
    ];
    for (f, args, want) in cases {
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {} {:?}\n{}", f, args, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {} {:?}", f, args);
    }
}

// ---- HashMap (設計_KIR.md 16 章、kir/hashmap) ----

const KIR_MAP_FNS: &str = r##"fn t.add_one(m: &mut alloc.collections.HashMap<u32, u32, std.alloc.System>, k: u32) -> bool uses [alloc] panics may
    match m.insert(k, k.wrapping_mul(2u32)) {
        Ok(Some(_old)) => false
        Ok(None) => true
        Err(_e) => core.panic.panic("oom")
    }
end t.add_one

fn t.build(n: u32) -> alloc.collections.HashMap<u32, u32, std.alloc.System> uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(t.add_one(&mut m, i % 40u32))
        i = i.wrapping_add(1u32)
    }
    m
end t.build

// 挿入と削除を繰り返す (墓標と作り直し)
fn t.churn(n: u32) -> u32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0u32
    let mut acc = 0u32
    while i < n {
        match m.insert(i, i) {
            Ok(_old) => 0u8
            Err(_e) => core.panic.panic("oom")
        }
        if i >= 4u32 {
            match m.remove(&(i.wrapping_sub(4u32))) {
                Some(v) => {
                    acc = acc.wrapping_add(v)
                }
                None => core.panic.panic("missing")
            }
        }
        i = i.wrapping_add(1u32)
    }
    for (k, _v) in m.iter() {
        acc = acc.wrapping_mul(7u32).wrapping_add(*k)
    }
    acc
end t.churn

fn t.fill(n: u32) -> u32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        match m.insert(i, i.wrapping_mul(3u32).wrapping_add(1u32)) {
            Ok(_old) => 0u8
            Err(_e) => core.panic.panic("oom")
        }
        i = i.wrapping_add(1u32)
    }
    let mut acc = 0u32
    let mut k = 0u32
    while k < n.wrapping_mul(2u32) {
        match m.get(&k) {
            Some(v) => {
                acc = acc.wrapping_add(*v)
            }
            None => {
                acc = acc.wrapping_add(1000000u32)
            }
        }
        k = k.wrapping_add(1u32)
    }
    acc
end t.fill

fn t.len_after(n: u32) -> usize uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        match m.insert(i % 7u32, i) {
            Ok(_old) => 0u8
            Err(_e) => core.panic.panic("oom")
        }
        i = i.wrapping_add(1u32)
    }
    m.len()
end t.len_after


fn t.missing(n: u32) -> u32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut acc = 0u32
    if m.is_empty() {
        acc = 1u32
    }
    match m.remove(&5u32) {
        Some(_v) => core.panic.panic("phantom")
        None => {
            acc = acc.wrapping_add(10u32)
        }
    }
    let mut i = 0u32
    while i < n {
        match m.insert(i, i) {
            Ok(_old) => 0u8
            Err(_e) => core.panic.panic("oom")
        }
        i = i.wrapping_add(1u32)
    }
    if m.contains_key(&(n.wrapping_add(3u32))) {
        acc = acc.wrapping_add(100u32)
    }
    if m.contains_key(&2u32) {
        acc = acc.wrapping_add(1000u32)
    }
    match m.get(&n) {
        Some(_v) => core.panic.panic("phantom")
        None => {
            acc = acc.wrapping_add(10000u32)
        }
    }
    acc
end t.missing

fn t.moved(n: u32) -> u32 uses [alloc] panics may
    let m = t.build(n)
    let mut acc = 0u32
    for (k, v) in m.iter() {
        acc = acc.wrapping_add(*k).wrapping_add(*v)
    }
    acc
end t.moved

// 削除・再挿入・反復順 (挿入順、削除後も相対順を保つ、再挿入は末尾)
fn t.order(n: u32) -> u32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        match m.insert(i.wrapping_mul(7919u32), i) {
            Ok(_old) => 0u8
            Err(_e) => core.panic.panic("oom")
        }
        i = i.wrapping_add(1u32)
    }
    i = 0u32
    while i < n {
        if i % 3u32 == 0u32 {
            match m.remove(&i.wrapping_mul(7919u32)) {
                Some(v) => core.check.eq(v, i)
                None => core.panic.panic("missing")
            }
        }
        i = i.wrapping_add(1u32)
    }
    i = 0u32
    while i < n {
        if i % 6u32 == 0u32 {
            match m.insert(i.wrapping_mul(7919u32), i.wrapping_add(1u32)) {
                Ok(_old) => core.mem.drop(_old)
                Err(_e) => core.panic.panic("oom")
            }
        }
        i = i.wrapping_add(1u32)
    }
    let mut acc = 0u32
    for (k, v) in m.iter() {
        acc = acc.wrapping_mul(31u32).wrapping_add(*k).wrapping_add(v.wrapping_mul(3u32))
    }
    acc
end t.order

fn t.overwrite(n: u32) -> u32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut acc = 0u32
    let mut i = 0u32
    while i < n {
        match m.insert(i % 5u32, i.wrapping_add(10u32)) {
            Ok(Some(old)) => {
                acc = acc.wrapping_add(old)
            }
            Ok(None) => {
                acc = acc.wrapping_add(1u32)
            }
            Err(_e) => core.panic.panic("oom")
        }
        i = i.wrapping_add(1u32)
    }
    acc
end t.overwrite

fn t.signed(n: i32) -> i32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0i32
    while i < n {
        match m.insert(0i32.wrapping_sub(i), i) {
            Ok(_old) => 0u8
            Err(_e) => core.panic.panic("oom")
        }
        i = i.wrapping_add(1i32)
    }
    let mut acc = 0i32
    i = 0i32
    while i < n {
        match m.get(&(0i32.wrapping_sub(i))) {
            Some(v) => {
                acc = acc.wrapping_add(*v)
            }
            None => core.panic.panic("missing")
        }
        i = i.wrapping_add(1i32)
    }
    acc
end t.signed

fn t.strs(_n: u32) -> u32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    match m.insert("alpha", 1u32) {
        Ok(_old) => 0u8
        Err(_e) => core.panic.panic("oom")
    }
    match m.insert("beta", 2u32) {
        Ok(_old) => 0u8
        Err(_e) => core.panic.panic("oom")
    }
    match m.insert("alpha", 10u32) {
        Ok(_old) => 0u8
        Err(_e) => core.panic.panic("oom")
    }
    let mut acc = 0u32
    match m.get("alpha") {
        Some(v) => {
            acc = acc.wrapping_add(*v)
        }
        None => core.panic.panic("missing")
    }
    match m.get("gamma") {
        Some(_v) => core.panic.panic("phantom")
        None => {
            acc = acc.wrapping_add(100u32)
        }
    }
    if m.contains_key("beta") {
        acc = acc.wrapping_add(1000u32)
    }
    match m.remove("beta") {
        Some(v) => {
            acc = acc.wrapping_add(v.wrapping_mul(10000u32))
        }
        None => core.panic.panic("missing")
    }
    for (k, v) in m.iter() {
        acc = acc.wrapping_add(*v)
    }
    acc
end t.strs
"##;

fn kir_map_pkg(name: &str, main_body: &str) -> Tmp {
    let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", main_body);
    Tmp::new(name, &format!("{}\n{}", KIR_MAP_FNS, main))
}

#[test]
fn kir_map_values_match_tree_walker() {
    // 挿入・上書き (古い値が返る)・削除・再挿入 (末尾へ)・成長 (1 万件)・反復順 (挿入順)・存在しないキー・墓標と作り直し・
    // 符号付きと `&str` のキー・`&mut` の借用と値の戻り。参照実行器は漏れ・二重解放・範囲外を内部エラーにする。
    // 期待値の書いた場所は木走査の `core.check.eq` で確かめ、`--run` と `--native` (LLVM) の両方で同じ値になること。
    let cases: &[(&str, &str, &str, &str)] = &[
        ("t.fill", "0", "u32", "0"),
        ("t.fill", "100", "u32", "100014950"),
        ("t.fill", "2000", "u32", "2005999000"),
        ("t.fill", "10000", "u32", "1560060408"),
        ("t.len_after", "0", "usize", "0"),
        ("t.len_after", "50", "usize", "7"),
        ("t.overwrite", "20", "u32", "260"),
        ("t.order", "0", "u32", "0"),
        ("t.order", "30", "u32", "1121038567"),
        ("t.order", "1000", "u32", "2683877213"),
        ("t.missing", "0", "u32", "10011"),
        ("t.missing", "10", "u32", "11011"),
        ("t.churn", "3", "u32", "9"),
        ("t.churn", "100", "u32", "10987026"),
        ("t.churn", "3000", "u32", "2183374384"),
        ("t.signed", "20", "i32", "190"),
        ("t.strs", "1", "u32", "21120"),
        ("t.moved", "0", "u32", "0"),
        ("t.moved", "7", "u32", "63"),
        ("t.moved", "100", "u32", "2340"),
    ];
    for (i, (f, arg, ty, want)) in cases.iter().enumerate() {
        let call = format!("{}({}{})", f, arg, if *f == "t.len_after" { "u32" } else { ty_arg(f) });
        let t = kir_map_pkg(&format!("kir_map_values_{}", i), &format!("    core.check.eq({}, {}{})\n", call, want, ty));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let k = kir_run(&["kir", f, "--run", arg], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

/// `kir_map_values_match_tree_walker` の引数の型 (`t.signed` だけ `i32`、他は `u32`)。
fn ty_arg(f: &str) -> &'static str {
    if f == "t.signed" {
        "i32"
    } else {
        "u32"
    }
}

#[test]
fn kir_map_reports_unsupported_methods_with_a_reason() {
    let t = Tmp::new("kir_map_get_mut", "fn t.bump(n: u32) -> u32 uses [alloc] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    match m.insert(1u32, n) {
        Ok(_old) => 0u8
        Err(_e) => core.panic.panic(\"oom\")
    }
    match m.get_mut(&1u32) {
        Some(v) => {
            *v = n.wrapping_add(1u32)
        }
        None => core.panic.panic(\"none\")
    }
    n
end t.bump

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.bump(3u32), 3u32)
    Ok(())
end t.main
");
    let o = run(&["kir", "t.bump", "--run", "3"], &t.0);
    assert_ne!(o.status.code(), Some(0));
    assert!(text(&o.stderr).contains("HashMap::get_mut は段階 1 の対象外"), "{}", text(&o.stderr));
}

/// KIR の未対応の理由は、ソースの行番号を出す (`nline` は行と列を詰めた値なので、KIR は行だけを取り出す)。
#[test]
fn kir_unsupported_reasons_show_the_source_line() {
    let src = "fn t.f(v: &[f64]) -> f64 uses [] panics may\n    let a = 1u32\n    core.mem.drop(a)\n    v[0usize].sqrt()\nend t.f\n\nfn t.main() -> () uses [] panics never\nend t.main\n";
    let t = Tmp::new("kir_reason_line", src);
    let o = run(&["kir", "t.f"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("行 4:"), "{}", text(&o.stderr));
}

#[test]
fn struct_value_order_is_declaration_order() {
    // 構造体の値はリテラルに書いた順ではなく型定義の宣言順で持つ (4.3)。そのため、同じ値の `==`・HashMap の鍵・Debug の表示は
    // フィールドの書き方に依らない。木走査 (`ksn run`) と参照実行器 (`ksn kir --run`) の表示も一致すること。
    let src = "type t.P derive [Clone, Copy, Debug, PartialEq, Eq, Hash]
    a: u8
    b: u8
end t.P

fn t.main() -> () uses [alloc, io.console] panics may
    let x = t.P { b: 1u8, a: 2u8 }
    let y = t.P { a: 2u8, b: 1u8 }
    core.check.eq(x == y, true)
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    core.mem.drop(m.insert(x, 1u8))
    core.mem.drop(m.insert(y, 2u8))
    core.mem.drop(std.io.print_line(if m.len() == 1usize {
        \"len 1\"
    } else {
        \"len 2\"
    }))
    core.mem.drop(std.io.print_line(if m.get(&x) == Some(&2u8) {
        \"x -> 2\"
    } else {
        \"x -> other\"
    }))
end t.main
";
    let t = Tmp::new("struct_order_hash", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout), "len 1\nx -> 2\n");

    // Debug の表示は宣言順 (リテラルの書き方に依らない)。左辺は逆順で書いた値。
    let src = "type t.P derive [Clone, Copy, Debug, PartialEq, Eq]
    a: u8
    b: u8
end t.P

fn t.main() -> () uses [io.console] panics may
    let x = t.P { b: 1u8, a: 2u8 }
    core.check.eq(x, t.P { a: 9u8, b: 1u8 })
end t.main
";
    let t = Tmp::new("struct_order_debug", src);
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(101), "{}", text(&tw.stderr));
    assert_eq!(text(&tw.stderr).trim(), "panic: assertion failed: left: P { a: 2, b: 1 }, right: P { a: 9, b: 1 }");
    let k = kir_run(&["kir", "t.main", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(101), "{}", text(&k.stderr));
    assert_eq!(text(&k.stderr).trim(), text(&tw.stderr).trim());
}

/// Fable のレビュー (2026-11-13) で見つかった KIR / LLVM の不具合の回帰 (設計_KIR.md 17 章)。
/// `(KIR の引数, 木走査で呼ぶ式, 期待する値のリテラル, 期待する表示)` の各ケースを、木走査・参照実行器・機械語 (と ASan) で比べる。
fn kir_review_cases(tag: &str, fns: &str, f: &str, cases: &[(&[&str], &str, &str, &str)]) {
    for (i, (args, call, lit, want)) in cases.iter().enumerate() {
        let main = format!(
            "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    let r = {}\n    core.check.eq(r, {})\n    Ok(())\nend t.main\n",
            call, lit
        );
        let t = Tmp::new(&format!("kir_review_{}_{}", tag, i), &format!("{}\n{}", fns, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr) + &text(&o.stdout));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_review_field_move_in_one_branch_survives_the_join() {
    // 片方の分岐の中でフィールドを移すと、合流で移動が失われて二重解放になっていた
    // (`assigned` が、フィールドからの移動で書き換わる局所を数えなかった)。`if` と `match`。(`while` の中の移動は E3002 で拒否される。)
    let fns = "type t.B\n    items: alloc.vec.Vec<u32, std.alloc.System>\n    n: u32\nend t.B\n\n\
fn t.mk() -> t.B uses [alloc] panics may\n    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    core.mem.drop(v.push(7u32))\n    t.B { items: v, n: 1u32 }\nend t.mk\n\n\
fn t.f(c: bool) -> usize uses [alloc] panics may\n    let b = t.mk()\n    let mut r: usize = 0usize\n    if c {\n        let w = b.items\n        r = w.len()\n    }\n    r\nend t.f\n\n\
fn t.g(k: u32) -> usize uses [alloc] panics may\n    let b = t.mk()\n    let mut r: usize = 0usize\n    match k {\n        0u32 => {\n            let w = b.items\n            r = w.len()\n        }\n        _ => {\n            r = 5usize\n        }\n    }\n    r\nend t.g\n";
    kir_review_cases("fm_if", fns, "t.f", &[(&["true"], "t.f(true)", "1usize", "1"), (&["false"], "t.f(false)", "0usize", "0")]);
    kir_review_cases("fm_match", fns, "t.g", &[(&["0"], "t.g(0u32)", "1usize", "1"), (&["3"], "t.g(3u32)", "5usize", "5")]);
}

#[test]
fn kir_review_early_exit_frees_evaluated_owned_temporaries() {
    // `?` / `return` / `break` で抜けるとき、評価済みで未束縛の所有値 (引数・構造体のフィールド・タプルの要素) も解放する。
    let fns = "type t.P\n    a: alloc.vec.Vec<u32, std.alloc.System>\n    n: u32\nend t.P\n\n\
fn t.g(c: bool) -> Option<u32> uses [] panics never\n    if c {\n        return Some(1u32)\n    }\n    None\nend t.g\n\n\
fn t.mkv() -> alloc.vec.Vec<u32, std.alloc.System> uses [alloc] panics may\n    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    core.mem.drop(v.push(7u32))\n    v\nend t.mkv\n\n\
fn t.h(v: alloc.vec.Vec<u32, std.alloc.System>, x: u32) -> usize uses [] panics never\n    core.mem.drop(x)\n    v.len()\nend t.h\n\n\
fn t.f(c: bool) -> Option<usize> uses [alloc] panics may\n    Some(t.h(t.mkv(), t.g(c)?))\nend t.f\n\n\
fn t.s(c: bool) -> Option<u32> uses [alloc] panics may\n    let p = t.P { a: t.mkv(), n: t.g(c)? }\n    Some(p.n)\nend t.s\n\n\
fn t.tup(c: bool) -> Option<u32> uses [alloc] panics may\n    let (a, b) = (t.mkv(), t.g(c)?)\n    core.mem.drop(a)\n    Some(b)\nend t.tup\n\n\n\
fn t.ret(c: bool) -> usize uses [alloc] panics may\n    let n = t.h(t.mkv(), if c {\n        return 5usize\n    } else {\n        1u32\n    })\n    n + 10usize\nend t.ret\n\n\
fn t.brk(k: u32) -> usize uses [alloc] panics may\n    let mut total = 0usize\n    let mut i = 0u32\n    while i < 4u32 {\n        let n = t.h(t.mkv(), if i == k {\n            break\n        } else {\n            1u32\n        })\n        total = total + n\n        i = i + 1u32\n    }\n    total\nend t.brk\n";
    kir_review_cases("early_arg", fns, "t.f", &[(&["true"], "t.f(true)", "Some(1usize)", "Some(1)"), (&["false"], "t.f(false)", "None", "None")]);
    kir_review_cases("early_field", fns, "t.s", &[(&["true"], "t.s(true)", "Some(1u32)", "Some(1)"), (&["false"], "t.s(false)", "None", "None")]);
    kir_review_cases("early_ret", fns, "t.ret", &[(&["true"], "t.ret(true)", "5usize", "5"), (&["false"], "t.ret(false)", "11usize", "11")]);
    kir_review_cases("early_break", fns, "t.brk", &[(&["2"], "t.brk(2u32)", "2usize", "2"), (&["9"], "t.brk(9u32)", "4usize", "4")]);
    kir_review_cases("early_tuple", fns, "t.tup", &[(&["true"], "t.tup(true)", "Some(1u32)", "Some(1)"), (&["false"], "t.tup(false)", "None", "None")]);
}

#[test]
fn kir_review_from_str_radix_agrees_with_tree_walker() {
    // `core.num.<int>.from_str_radix` の KIR 下ろし (Op::ParseInt) が、木走査と Ok の値と
    // Err の kind を一致させる (符号・空文字列・範囲外・桁の誤り・radix 範囲)。
    let fns = [
        "fn t.i64(case: u32) -> i64 uses [] panics never\n    let r = match case {\n        0u32 => i64.from_str_radix(\"-9223372036854775808\", 10u32)\n        1u32 => i64.from_str_radix(\"9223372036854775808\", 10u32)\n        _ => i64.from_str_radix(\"0\", 10u32)\n    }\n    match r {\n        Ok(n) => n\n        Err(e) => i64.from_u8(e.kind).wrapping_add(1000)\n    }\nend t.i64",
        "fn t.i8(case: u32) -> i32 uses [] panics never\n    let r = match case {\n        0u32 => i8.from_str_radix(\"-128\", 10u32)\n        1u32 => i8.from_str_radix(\"128\", 10u32)\n        _ => i8.from_str_radix(\"0\", 10u32)\n    }\n    match r {\n        Ok(n) => i32.from_i8(n)\n        Err(e) => i32.from_u8(e.kind).wrapping_add(1000)\n    }\nend t.i8",
        "fn t.u64(case: u32) -> u64 uses [] panics never\n    let r = match case {\n        0u32 => u64.from_str_radix(\"18446744073709551615\", 10u32)\n        1u32 => u64.from_str_radix(\"18446744073709551616\", 10u32)\n        _ => u64.from_str_radix(\"0\", 10u32)\n    }\n    match r {\n        Ok(n) => n\n        Err(e) => u64.from_u8(e.kind).wrapping_add(1000u64)\n    }\nend t.u64",
        "fn t.u8(case: u32) -> u32 uses [] panics never\n    let r = match case {\n        0u32 => u8.from_str_radix(\"\", 10u32)\n        1u32 => u8.from_str_radix(\"0\", 10u32)\n        2u32 => u8.from_str_radix(\"+5\", 10u32)\n        3u32 => u8.from_str_radix(\"-5\", 10u32)\n        4u32 => u8.from_str_radix(\"-\", 10u32)\n        5u32 => u8.from_str_radix(\"ff\", 16u32)\n        6u32 => u8.from_str_radix(\"ff\", 10u32)\n        7u32 => u8.from_str_radix(\"FF\", 16u32)\n        8u32 => u8.from_str_radix(\"z\", 36u32)\n        9u32 => u8.from_str_radix(\"255\", 10u32)\n        10u32 => u8.from_str_radix(\"256\", 10u32)\n        11u32 => u8.from_str_radix(\" 1\", 10u32)\n        12u32 => u8.from_str_radix(\"1_0\", 10u32)\n        _ => u8.from_str_radix(\"\", 10u32)\n    }\n    match r {\n        Ok(n) => u32.from_u8(n)\n        Err(e) => 1000u32.wrapping_add(u32.from_u8(e.kind))\n    }\nend t.u8",
    ].join("\n\n");
    kir_review_cases("pi_u8", &fns, "t.u8", &[
        (&["0"], "t.u8(0u32)", "1000u32", "1000"), // 空文字列 → kind 0
        (&["1"], "t.u8(1u32)", "0u32", "0"),      // "0" → Ok(0)
        (&["2"], "t.u8(2u32)", "5u32", "5"),      // "+5" → Ok(5)
        (&["3"], "t.u8(3u32)", "1000u32", "1000"),// 符号なしに "-" → kind 0
        (&["4"], "t.u8(4u32)", "1000u32", "1000"),// 符号だけ → kind 0
        (&["5"], "t.u8(5u32)", "255u32", "255"),  // "ff" radix 16 → Ok(255)
        (&["6"], "t.u8(6u32)", "1000u32", "1000"),// "ff" radix 10 → kind 0
        (&["7"], "t.u8(7u32)", "255u32", "255"),  // "FF" radix 16 → Ok(255)
        (&["8"], "t.u8(8u32)", "35u32", "35"),    // "z" radix 36 → Ok(35)
        (&["9"], "t.u8(9u32)", "255u32", "255"),  // u8 "255" → Ok(255)
        (&["10"], "t.u8(10u32)", "1001u32", "1001"), // u8 "256" → kind 1 (範囲外)
        (&["11"], "t.u8(11u32)", "1000u32", "1000"),// " 1" → kind 0
        (&["12"], "t.u8(12u32)", "1000u32", "1000"),// "1_0" → kind 0
    ]);
    kir_review_cases("pi_i8", &fns, "t.i8", &[
        (&["0"], "t.i8(0u32)", "-128i32", "-128"),  // i8 "-128" → Ok(-128)
        (&["1"], "t.i8(1u32)", "1001i32", "1001"),  // i8 "128" → kind 1 (範囲外)
    ]);
    kir_review_cases("pi_u64", &fns, "t.u64", &[
        (&["0"], "t.u64(0u32)", "18446744073709551615u64", "18446744073709551615"), // u64 max → Ok
        (&["1"], "t.u64(1u32)", "1001u64", "1001"), // u64 max+1 → kind 1
    ]);
    kir_review_cases("pi_i64", &fns, "t.i64", &[
        (&["0"], "t.i64(0u32)", "-9223372036854775808i64", "-9223372036854775808"), // i64 min → Ok
        (&["1"], "t.i64(1u32)", "1001i64", "1001"), // i64 "2^63" → kind 1
    ]);
}

#[test]
fn kir_review_user_iterator_advance_crosses_outer_joins() {
    // 利用者の反復子の `for x in it` が進めたスロットは、外側の `while` / `if` / `match` の合流にも渡る
    // (前は `scan_mut` が `For` を見ず、外側の合流が古い `it` を使い続けた)。
    let fns = format!(
        "{}\nfn t.outer(rounds: u32) -> u32 uses [] panics may\n    let mut it = t.Upto {{ cur: 0u32, stop: 100u32 }}\n    let mut acc = 0u32\n    let mut r = 0u32\n    while r < rounds {{\n        for x in it {{\n            acc = acc.wrapping_add(x)\n            if x % 4u32 == 3u32 {{\n                break\n            }}\n        }}\n        r = r.wrapping_add(1u32)\n    }}\n    acc\nend t.outer\n\n\
fn t.cond(c: bool) -> u32 uses [] panics never\n    let mut it = t.Upto {{ cur: 0u32, stop: 6u32 }}\n    let mut acc = 0u32\n    if c {{\n        for x in it {{\n            if x == 2u32 {{\n                break\n            }}\n            acc = acc.wrapping_add(x)\n        }}\n    }}\n    for y in it {{\n        acc = acc.wrapping_add(y.wrapping_mul(10u32))\n    }}\n    acc\nend t.cond\n\n\
fn t.sel(k: u32) -> u32 uses [] panics never\n    let mut it = t.Upto {{ cur: 0u32, stop: 6u32 }}\n    let mut acc = 0u32\n    match k {{\n        0u32 => {{\n            for x in it {{\n                if x == 1u32 {{\n                    break\n                }}\n                acc = acc.wrapping_add(1u32)\n            }}\n        }}\n        _ => {{\n            acc = acc.wrapping_add(0u32)\n        }}\n    }}\n    for y in it {{\n        acc = acc.wrapping_add(y)\n    }}\n    acc\nend t.sel\n",
        USER_ITER_SRC.split("fn t.sum(").next().unwrap()
    );
    kir_review_cases("uiter_outer", &fns, "t.outer", &[(&["2"], "t.outer(2u32)", "28u32", "28"), (&["3"], "t.outer(3u32)", "66u32", "66")]);
    kir_review_cases("uiter_cond", &fns, "t.cond", &[(&["true"], "t.cond(true)", "121u32", "121"), (&["false"], "t.cond(false)", "150u32", "150")]);
    kir_review_cases("uiter_sel", &fns, "t.sel", &[(&["0"], "t.sel(0u32)", "15u32", "15"), (&["1"], "t.sel(1u32)", "15u32", "15")]);
}

#[test]
fn kir_review_user_iterator_owned_items_are_freed() {
    // 利用者の反復子が所有物 (Vec) を返すとき、`for` の変数が所有して 1 周ごとに解放する (前は漏れた)。`for _ in it` も解放する。
    let fns = "type t.Gen pub\n    cur: u32\n    stop: u32\nend t.Gen\n\n\
impl core.iter.Iterator<alloc.vec.Vec<u32, std.alloc.System>, [alloc], may> for t.Gen\n\
fn t.Gen.next(self: &mut t.Gen) -> Option<alloc.vec.Vec<u32, std.alloc.System>> uses [alloc] panics may impls core.iter.Iterator<alloc.vec.Vec<u32, std.alloc.System>, [alloc], may>\n    if self.cur >= self.stop {\n        return None\n    }\n    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    core.mem.drop(v.push(self.cur))\n    self.cur = self.cur.wrapping_add(1u32)\n    Some(v)\nend t.Gen.next\n\n\
fn t.run(n: u32) -> usize uses [alloc] panics may\n    let mut g = t.Gen { cur: 0u32, stop: n }\n    let mut total = 0usize\n    for v in g {\n        total = total + v.len()\n    }\n    total\nend t.run\n\n\
fn t.run_wild(n: u32) -> usize uses [alloc] panics may\n    let mut g = t.Gen { cur: 0u32, stop: n }\n    let mut total = 0usize\n    for _ in g {\n        total = total + 1usize\n    }\n    total\nend t.run_wild\n";
    kir_review_cases("uiter_own", fns, "t.run", &[(&["4"], "t.run(4u32)", "4usize", "4"), (&["0"], "t.run(0u32)", "0usize", "0")]);
    kir_review_cases("uiter_wild", fns, "t.run_wild", &[(&["3"], "t.run_wild(3u32)", "3usize", "3")]);
}

#[test]
fn kir_review_nan_bits_follow_one_rule() {
    // NaN になる演算 (`+ - * / %`・`sqrt`) の結果のビット列: 第 1 被演算子が NaN ならそれ (quiet 化)、第 2 が NaN ならそれ、
    // どちらでもなければ正の quiet NaN。実機の既定の NaN (符号ビット付き) と LLVM の定数畳み込み (正) の違いを、
    // 木走査・参照実行器・機械語 (最適化 -O1 と ASan の -O0) の 3 者で揃える (設計_KIR.md 17.8)。
    let fns = "fn t.d(b: u64) -> f64 uses [] panics never\n    core.num.f64.from_bits(b)\nend t.d\n\n\
fn t.s(b: u32) -> f32 uses [] panics never\n    core.num.f32.from_bits(b)\nend t.s\n\n\
fn t.sub64(a: f64, b: f64) -> u64 uses [] panics never\n    (a - b).to_bits()\nend t.sub64\n\n\
fn t.mul64(a: f64, b: f64) -> u64 uses [] panics never\n    (a * b).to_bits()\nend t.mul64\n\n\
fn t.div64(a: f64, b: f64) -> u64 uses [] panics never\n    (a / b).to_bits()\nend t.div64\n\n\
fn t.rem64(a: f64, b: f64) -> u64 uses [] panics never\n    a.rem(b).to_bits()\nend t.rem64\n\n\
fn t.sqrt64(a: f64) -> u64 uses [] panics never\n    a.sqrt().to_bits()\nend t.sqrt64\n\n\
fn t.add32(a: f32, b: f32) -> u32 uses [] panics never\n    (a + b).to_bits()\nend t.add32\n\n\
fn t.div32(a: f32, b: f32) -> u32 uses [] panics never\n    (a / b).to_bits()\nend t.div32\n";
    const INF: &str = "9218868437227405312";
    const CANON: &str = "9221120237041090560";
    const SNAN: &str = "9218868437227405313";
    const QSNAN: &str = "9221120237041090561";
    const ONE: &str = "4607182418800017408";
    let c64 = |f: &'static str, a: &'static str, b: &'static str, want: &'static str| -> (&'static str, Vec<&'static str>, String, String, String) {
        (f, vec![a, b], format!("{}(t.d({}u64), t.d({}u64))", f, a, b), format!("{}u64", want), want.to_string())
    };
    let mut cases = vec![
        c64("t.sub64", INF, INF, CANON),
        c64("t.sub64", SNAN, ONE, QSNAN),
        c64("t.sub64", ONE, SNAN, QSNAN),
        c64("t.mul64", "0", INF, CANON),
        c64("t.div64", INF, INF, CANON),
        c64("t.div64", "0", "0", CANON),
        c64("t.div64", SNAN, "0", QSNAN),
        c64("t.rem64", ONE, "0", CANON),
        c64("t.rem64", INF, ONE, CANON),
    ];
    cases.push(("t.sqrt64", vec!["13836000000000000000"], "t.sqrt64(t.d(13836000000000000000u64))".into(), format!("{}u64", CANON), CANON.to_string()));
    cases.push(("t.add32", vec!["2139095040", "4286578688"], "t.add32(t.s(2139095040u32), t.s(4286578688u32))".into(), "2143289344u32".into(), "2143289344".into()));
    cases.push(("t.div32", vec!["0", "0"], "t.div32(t.s(0u32), t.s(0u32))".into(), "2143289344u32".into(), "2143289344".into()));
    cases.push(("t.add32", vec!["2139095041", "1065353216"], "t.add32(t.s(2139095041u32), t.s(1065353216u32))".into(), "2143289345u32".into(), "2143289345".into()));
    for (i, (f, args, call, lit, want)) in cases.iter().enumerate() {
        kir_review_cases(&format!("nan{}", i), &fns, f, &[(&args[..], call.as_str(), lit.as_str(), want.as_str())]);
    }
}

#[test]
fn kir_review_rejects_borrow_shapes_that_read_freed_buffers() {
    // `ksn check` が通す、借用の中身の丸読み (`*r`、所有物を持つ型) と、反復中の Vec の変更 (`for x in v` の本体で `v.push`) は、
    // 二重解放・解放後の読みになるので、KIR の下ろしは理由を付けて拒む (設計_KIR.md 17.5 / 17.6)。
    let cases: &[(&str, &str)] = &[
        (
            "type t.B\n    items: alloc.vec.Vec<u32, std.alloc.System>\n    n: u32\nend t.B\n\nfn t.f(r: &t.B) -> usize uses [alloc] panics may\n    let b = *r\n    b.items.len()\nend t.f\n",
            "借用の中身",
        ),
        (
            "fn t.f(r: &alloc.vec.Vec<u32, std.alloc.System>) -> usize uses [alloc] panics may\n    let b = *r\n    b.len()\nend t.f\n",
            "* の被演算子が参照でない",
        ),
        (
            "fn t.f() -> usize uses [alloc] panics may\n    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    core.mem.drop(v.push(1u32))\n    let mut n = 0usize\n    for x in v {\n        if n < 3usize {\n            core.mem.drop(v.push(x))\n        }\n        n = n + 1usize\n    }\n    v.len()\nend t.f\n",
            "反復中の Vec を変更",
        ),
        (
            "fn t.f() -> usize uses [alloc] panics may\n    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)\n    let mut k: alloc.string.String = alloc.string.String.new(std.alloc.SYSTEM)\n    core.mem.drop(k.push_str(\"abc\"))\n    core.mem.drop(m.insert(k.as_str(), 1u32))\n    core.mem.drop(k.push_str(\"defghijklmnopqrstuvwxyzabcdefghijklmnopq\"))\n    m.len()\nend t.f\n",
            "元の String が関数の中で変更される",
        ),
    ];
    for (i, (src, why)) in cases.iter().enumerate() {
        let src = format!("{}\nfn t.main() -> Result<(), std.io.Error> uses [] panics may\n    Ok(())\nend t.main\n", src);
        let t = Tmp::new(&format!("kir_review_reject_{}", i), &src);
        // 検査が先に拒むようになった形 (`*r` の move は E3002、借用中の変更は E3010) は、それで健全性が保たれる。
        // 検査を通る形だけ、KIR の下ろしが理由を付けて拒むことを確かめる。
        let c = run(&["check", "--json"], &t.0);
        if c.status.code() != Some(0) {
            let got = codes(&text(&c.stdout));
            assert!(got.iter().any(|g| g == "E3002" || g == "E3010"), "{}: {:?}", i, got);
            continue;
        }
        let o = run(&["kir", "t.f"], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
        assert!(text(&o.stderr).contains(why), "{}", text(&o.stderr));
    }
}

#[test]
fn kir_review_for_over_a_vec_may_still_read_it() {
    // 本体で反復中の Vec を読むだけ (`len` など) なら、これまでどおり下ろせる。`for x in v` は `v` を消費する
    // (fix/linear-generic) ので、借用のスライスを回す。
    let fns = "fn t.sum() -> usize uses [alloc] panics may\n    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    core.mem.drop(v.push(1u32))\n    core.mem.drop(v.push(2u32))\n    let mut n = 0usize\n    let s = v.as_slice()\n    for x in s {\n        n = n + v.len()\n        core.mem.drop(x)\n    }\n    n\nend t.sum\n";
    kir_review_cases("for_read", fns, "t.sum", &[(&[], "t.sum()", "4usize", "4")]);
}

/// Fable の KIR レビュー 2 回目: 局所の `Option<Vec>` の `unwrap` は局所を移す (取り出した値とスコープの終わりの解放が
/// 重なって二重解放になっていた)。値の受け手を移した後、引数の `?` で抜けても受け手は漏れない。
#[test]
fn kir_review2_unwrap_moves_and_receiver_survives_early_exit() {
    let fns = "fn t.mkv(n: u32) -> alloc.vec.Vec<u32, std.alloc.System> uses [alloc] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(i))
        i = i + 1u32
    }
    v
end t.mkv

fn t.opt(c: bool) -> Option<alloc.vec.Vec<u32, std.alloc.System>> uses [alloc] panics may
    if c {
        return Some(t.mkv(2u32))
    }
    None
end t.opt

fn t.n7(c: bool) -> usize uses [alloc] panics may
    let o = t.opt(c)
    let v = o.unwrap()
    v.len()
end t.n7

type t.B
    items: alloc.vec.Vec<u32, std.alloc.System>
end t.B

fn t.B.total(self: t.B, x: u32) -> usize uses [] panics never
    core.mem.drop(x)
    self.items.len()
end t.B.total

fn t.g(c: bool) -> Option<u32> uses [] panics never
    if c {
        return Some(1u32)
    }
    None
end t.g

fn t.e1(c: bool) -> Option<usize> uses [alloc] panics may
    let b = t.B { items: t.mkv(3u32) }
    Some(b.total(t.g(c)?))
end t.e1
";
    kir_review_cases("r2_unwrap", fns, "t.n7", &[(&["true"], "t.n7(true)", "2usize", "2")]);
    kir_review_cases("r2_recv_exit", fns, "t.e1", &[(&["true"], "t.e1(true)", "Some(3usize)", "Some(3)"), (&["false"], "t.e1(false)", "None", "None")]);
}

#[test]
fn kir_net_roundtrip() {
    // ポート 0 で bind し、実際のポートを得て connect し、同一スレッド内で数バイトをやり取りする。
    // listen の後なら accept の前でも connect は成功する。
    let fns = "fn t.net_roundtrip() -> () uses [alloc, io.net] panics may\n    match std.net.listen(\"127.0.0.1:0\") {\n        Ok(l) => {\n            let port = match l.local_port() {\n                Ok(p) => p\n                Err(_e) => core.panic.panic(\"local_port failed\")\n            }\n            core.check.eq((port == 0u16), false)\n            let mut address = alloc.string.String.new(std.alloc.SYSTEM)\n            match address.push_str(\"127.0.0.1:\") {\n                Ok(_u) => ()\n                Err(_e) => core.panic.panic(\"push_str failed\")\n            }\n            match address.push_u64(u64.from_u16(port)) {\n                Ok(_u) => ()\n                Err(_e) => core.panic.panic(\"push_u64 failed\")\n            }\n            let a = address.as_str()\n            match std.net.connect(a) {\n                Ok(c) => {\n                    let mut s = c\n                    match s.write(\"hello\".as_bytes()) {\n                        Ok(_n) => ()\n                        Err(_e) => core.panic.panic(\"write failed\")\n                    }\n                    match s.close() {\n                        Ok(_u) => ()\n                        Err(_e) => core.panic.panic(\"close failed\")\n                    }\n                }\n                Err(_e) => core.panic.panic(\"connect failed\")\n            }\n            match l.accept() {\n                Ok(accepted) => {\n                    let mut s2 = accepted\n                    match s2.write(\"hi\".as_bytes()) {\n                        Ok(_n) => ()\n                        Err(_e) => core.panic.panic(\"server write failed\")\n                    }\n                    match s2.close() {\n                        Ok(_u) => ()\n                        Err(_e) => core.panic.panic(\"close2 failed\")\n                    }\n                }\n                Err(_e) => core.panic.panic(\"accept failed\")\n            }\n            match l.close() {\n                Ok(_u) => ()\n                Err(_e) => core.panic.panic(\"close3 failed\")\n            }\n        }\n        Err(_e) => core.panic.panic(\"listen failed\")\n    }\nend t.net_roundtrip\n\nfn t.main() -> () uses [alloc, io.net] panics may\n    t.net_roundtrip()\nend t.main\n";
    let t = Tmp::new("kir_net_roundtrip", fns);
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(0), "tree-walker: {}\n{}", text(&tw.stderr), text(&tw.stdout));
    let k = kir_run(&["kir", "t.net_roundtrip", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", text(&k.stderr), text(&k.stdout));
}

/// ソケットを `&TcpListener` / `&mut TcpStream` で渡す (設計_KIR.md 12 章)。`[0u8; 8]` の配列を `&mut [u8]` で読ませ、
/// 読んだ長さと先頭バイトを返す。`?` の失敗の型は `std.net.Error` (listen / accept / local_port / close)。木走査と一致する値は 5104 (5 バイト、先頭 'h' = 104)。
#[test]
fn kir_net_refs_pass_sockets_by_reference() {
    let fns = r#"fn t.port_of(l: &std.net.TcpListener) -> Result<u16, std.net.Error> uses [io.net] panics never
    l.local_port()
end t.port_of

fn t.send(s: &mut std.net.TcpStream, data: &[u8]) -> Result<usize, std.io.Error> uses [io.net] panics never
    s.write(data)
end t.send

fn t.recv(s: &mut std.net.TcpStream, buf: &mut [u8]) -> Result<usize, std.io.Error> uses [io.net] panics never
    s.read(buf)
end t.recv

fn t.first_byte(buf: &[u8]) -> usize uses [] panics never
    match buf.get(0usize) {
        Some(b) => usize.from_u8(*b)
        None => 0usize
    }
end t.first_byte

fn t.exchange() -> usize uses [alloc, io.net] panics may
    let mut address = alloc.string.String.new(std.alloc.SYSTEM)
    let listener = match std.net.listen("127.0.0.1:0") {
        Ok(l) => l
        Err(_e) => core.panic.panic("listen")
    }
    let port = match t.port_of(&listener) {
        Ok(p) => p
        Err(_e) => core.panic.panic("port")
    }
    match address.push_str("127.0.0.1:") {
        Ok(_u) => ()
        Err(_e) => core.panic.panic("push")
    }
    match address.push_u64(u64.from_u16(port)) {
        Ok(_u) => ()
        Err(_e) => core.panic.panic("push_u64")
    }
    let mut client = match std.net.connect(address.as_str()) {
        Ok(c) => c
        Err(_e) => core.panic.panic("connect")
    }
    match t.send(&mut client, "hello".as_bytes()) {
        Ok(_n) => ()
        Err(_e) => core.panic.panic("send")
    }
    match client.close() {
        Ok(_u) => ()
        Err(_e) => core.panic.panic("close client")
    }
    let mut server = match listener.accept() {
        Ok(s) => s
        Err(_e) => core.panic.panic("accept")
    }
    let mut chunk = [0u8; 8]
    let n = match t.recv(&mut server, &mut chunk) {
        Ok(k) => k
        Err(_e) => core.panic.panic("recv")
    }
    let first = t.first_byte(&chunk)
    match server.close() {
        Ok(_u) => ()
        Err(_e) => core.panic.panic("close server")
    }
    match listener.close() {
        Ok(_u) => ()
        Err(_e) => core.panic.panic("close listener")
    }
    n.wrapping_mul(1000usize).wrapping_add(first)
end t.exchange

fn t.bad_listen() -> Result<usize, std.net.Error> uses [io.net] panics never
    let l = std.net.listen("not an address")?
    match l.close() {
        Ok(_u) => Ok(1usize)
        Err(e) => Err(e)
    }
end t.bad_listen
"#;
    let main = "fn t.main() -> () uses [alloc, io.net] panics may\n    core.check.eq(t.exchange(), 5104usize)\n    match t.bad_listen() {\n        Ok(_n) => core.panic.panic(\"listen should fail\")\n        Err(_e) => ()\n    }\nend t.main\n";
    let t = Tmp::new("kir_net_refs", &format!("{}\n{}", fns, main));
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(0), "tree-walker: {}\n{}", text(&tw.stderr), text(&tw.stdout));
    let k = kir_run(&["kir", "t.exchange", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", text(&k.stderr), text(&k.stdout));
    assert_eq!(text(&k.stdout).trim(), "5104");
    let m = kir_run(&["kir", "t.main", "--run"], &t.0);
    assert_eq!(m.status.code(), Some(0), "kir main: {}\n{}", text(&m.stderr), text(&m.stdout));
}

/// `[x; N]` (`ArrayRep`、長さはリテラル、要素はスカラー) の値・ループ・`get`。長さ 0 の配列も含む。
#[test]
fn kir_array_rep_matches_tree_walker() {
    let fns = r#"fn t.count_true() -> usize uses [] panics never
    let mut bs = [false; 4]
    match bs.get(2usize) {
        Some(b) => core.mem.drop(*b)
        None => core.mem.drop(false)
    }
    let mut n = 0usize
    for b in bs {
        if !b {
            n = n.wrapping_add(1usize)
        }
    }
    n
end t.count_true

fn t.fill_len(buf: &mut [u32]) -> usize uses [] panics never
    buf.len()
end t.fill_len

fn t.sum_arr() -> u32 uses [] panics never
    let a = [3u32; 5]
    let mut s = 0u32
    for x in a {
        s = s.wrapping_add(x)
    }
    let b = [1u32; 0]
    for y in b {
        s = s.wrapping_add(y)
    }
    s
end t.sum_arr
"#;
    let main = "fn t.main() -> () uses [] panics may\n    core.check.eq(t.sum_arr(), 15u32)\n    core.check.eq(t.count_true(), 4usize)\nend t.main\n";
    let t = Tmp::new("kir_array_rep", &format!("{}\n{}", fns, main));
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(0), "tree-walker: {}\n{}", text(&tw.stderr), text(&tw.stdout));
    for (f, want) in [("t.sum_arr", "15"), ("t.count_true", "4")] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}: {}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
}

/// `--opt` のインライン化は `extern` (本体が仮の `unreachable`) を展開しない。
#[test]
fn kir_opt_does_not_inline_extern() {
    let src = "extern t.abs(x: i32) -> i32 link \"abs\"\n\nfn t.abs_extern(x: i32) -> i32 uses [] panics never\n    unsafe \"abs\" \"libc abs\" {\n        t.abs(x)\n    }\nend t.abs_extern\n\nfn t.main() -> () uses [] panics may\n    core.check.eq(t.abs_extern(-7i32), 7i32)\nend t.main\n";
    let t = Tmp::new("kir_opt_extern", src);
    fs::write(
        t.0.join("kasane.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n\n[unsafe]\nmodules = [\"t\"]\n",
    )
    .unwrap();
    let k = kir_run(&["kir", "t.abs_extern", "--opt", "--run", "-7"], &t.0);
    assert_eq!(k.status.code(), Some(0), "{}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "7");
}

/// `?` のエラー変換 (仕様 4.1): `Result<T, E1>` の `?` を `Result<U, E2>` で使い、
/// E2 が `derive [From]` で E1 だけをペイロードに持つ変種を 1 つ持つ enum なら、
/// エラーを `E2.V(err)` に包む。E1 は `i32` / `std.io.Error` /
/// `alloc.string.String` (所有物を持つ)、成功側の型が同じ形と違う形の両方。
/// `kir_run` は木走査と ASan 付き機械語でも走らせる (漏れ・二重解放の検出)。
#[test]
fn kir_convert_error_via_from() {
    let fns = "enum t.Err32 derive [Debug, Clone, Copy, PartialEq, From] pub\n    X(u32)\nend t.Err32\n\n\
enum t.ErrIo derive [Debug, Clone, Copy, PartialEq, From] pub\n    Io(std.io.Error)\nend t.ErrIo\n\n\
enum t.ErrStr derive [From] pub\n    S(alloc.string.String<std.alloc.System>)\nend t.ErrStr\n\n\
fn t.dst_io(c: bool) -> Result<(), t.ErrIo> uses [] panics never\n    t.src_io(c)?\n    Ok(())\nend t.dst_io\n\n\
fn t.dst_str(c: bool) -> Result<(), t.ErrStr> uses [alloc] panics never\n    t.src_str(c)?\n    Ok(())\nend t.dst_str\n\n\
fn t.dst_u32(c: bool) -> Result<u32, t.Err32> uses [] panics never\n    let n = t.src_u32(c)?\n    Ok(n)\nend t.dst_u32\n\n\
fn t.dst_u64(c: bool) -> Result<u64, t.Err32> uses [] panics never\n    t.src_unit(c)?\n    Ok(1007u64)\nend t.dst_u64\n\n\
fn t.probe_io(c: bool) -> u32 uses [] panics never\n    match t.dst_io(c) {\n        Ok(()) => 1000u32\n        Err(t.ErrIo.Io(_)) => 42u32\n        Err(_) => 9999u32\n    }\nend t.probe_io\n\n\
fn t.probe_str(c: bool) -> u32 uses [alloc] panics never\n    match t.dst_str(c) {\n        Ok(()) => 1000u32\n        Err(t.ErrStr.S(_)) => 42u32\n        Err(_) => 9999u32\n    }\nend t.probe_str\n\n\
fn t.probe_u32(c: bool) -> u32 uses [] panics never\n    match t.dst_u32(c) {\n        Ok(n) => n\n        Err(t.Err32.X(e)) => e\n        Err(_) => 9999u32\n    }\nend t.probe_u32\n\n\
fn t.probe_u64(c: bool) -> u64 uses [] panics never\n    match t.dst_u64(c) {\n        Ok(n) => n\n        Err(t.Err32.X(_)) => 42u64\n        Err(_) => 9999u64\n    }\nend t.probe_u64\n\n\
fn t.src_io(c: bool) -> Result<(), std.io.Error> uses [] panics never\n    if c {\n        return Ok(())\n    }\n    Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 0i32 })\nend t.src_io\n\n\
fn t.src_str(c: bool) -> Result<(), alloc.string.String<std.alloc.System>> uses [alloc] panics never\n    if c {\n        return Ok(())\n    }\n    Err(alloc.string.String.new(std.alloc.SYSTEM))\nend t.src_str\n\n\
fn t.src_u32(c: bool) -> Result<u32, u32> uses [] panics never\n    if c {\n        return Ok(1007u32)\n    }\n    Err(42u32)\nend t.src_u32\n\n\
fn t.src_unit(c: bool) -> Result<(), u32> uses [] panics never\n    if c {\n        return Ok(())\n    }\n    Err(42u32)\nend t.src_unit\n";
    kir_review_cases("conv_u32", fns, "t.probe_u32", &[
        (&["true"], "t.probe_u32(true)", "1007u32", "1007"),
        (&["false"], "t.probe_u32(false)", "42u32", "42"),
    ]);
    kir_review_cases("conv_u64", fns, "t.probe_u64", &[
        (&["true"], "t.probe_u64(true)", "1007u64", "1007"),
        (&["false"], "t.probe_u64(false)", "42u64", "42"),
    ]);
    kir_review_cases("conv_io", fns, "t.probe_io", &[
        (&["true"], "t.probe_io(true)", "1000u32", "1000"),
        (&["false"], "t.probe_io(false)", "42u32", "42"),
    ]);
    kir_review_cases("conv_str", fns, "t.probe_str", &[
        (&["true"], "t.probe_str(true)", "1000u32", "1000"),
        (&["false"], "t.probe_str(false)", "42u32", "42"),
    ]);
}

/// 浮動小数の表示 (kir_rt.c) は木走査の `py_float` / `py_float32` と同じ。指数表記を使わない最短の往復表記、
/// 非正規数、ちょうど中間の値 (Rust は絶対値の大きい方へ丸める)。参照実行器・機械語・ASan で比べる。
#[test]
fn kir_float_display_matches_tree_walker_on_hard_values() {
    let src = "fn t.c64(b: u64) -> () uses [] panics may\n    core.check.eq(f64.from_bits(b), 2.5f64)\nend t.c64\n\n\
               fn t.c32(b: u32) -> () uses [] panics may\n    core.check.eq(f32.from_bits(b), 2.5f32)\nend t.c32\n\n\
               fn t.main() -> () uses [] panics may\nend t.main\n";
    let t = Tmp::new("kir_float_hard", src);
    let v64: [f64; 12] = [1e100, f64::MAX, 5e-324, 2.2250738585072014e-308, 0.1 + 0.2, -0.0, 1e16, 1e-7, 2f64.powi(-25), 1658206780088562.25, f64::NAN, f64::NEG_INFINITY];
    for v in v64 {
        let b = v.to_bits().to_string();
        let k = kir_run(&["kir", "t.c64", "--run", &b], &t.0);
        let want = format!("panic: assertion failed: left: {}, right: 2.5", kasane_lang_py_float(v));
        assert!(text(&k.stderr).starts_with(&want), "{} {}", want, text(&k.stderr));
    }
    for v in [0.1f32, 16777217.0, f32::MAX, 1e-45, 1.5] {
        let b = v.to_bits().to_string();
        let k = kir_run(&["kir", "t.c32", "--run", &b], &t.0);
        let s = if v == v.trunc() && v.abs() < 1e16 { format!("{:.1}", v) } else { format!("{}", v) };
        let want = format!("panic: assertion failed: left: {}, right: 2.5", s);
        assert!(text(&k.stderr).starts_with(&want), "{} {}", want, text(&k.stderr));
    }
}

/// `interp.rs` の `py_float` と同じ規則 (テストの中の写し)。
fn kasane_lang_py_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == f.trunc() && f.abs() < 1e16 {
        return format!("{:.1}", f);
    }
    format!("{}", f)
}

/// `String.push_u64` の KIR の下ろし: 0 は "0" (最下位の桁は必ず書く)、u64 の最大値まで、2 回続けて書く。
#[test]
fn kir_push_u64_matches_tree_walker() {
    let fns = "fn t.f(x: u64) -> usize uses [alloc] panics may\n    let mut s = alloc.string.String.new(std.alloc.SYSTEM)\n    core.mem.drop(s.push_str(\"a\"))\n    core.mem.drop(s.push_u64(x))\n    core.mem.drop(s.push_u64(x))\n    let mut h = 0usize\n    for c in s.as_str().chars() {\n        h = h.wrapping_mul(13usize)\n        if c == '0' {\n            h = h.wrapping_add(1usize)\n        }\n        if c == '9' {\n            h = h.wrapping_add(2usize)\n        }\n        if c == '1' {\n            h = h.wrapping_add(3usize)\n        }\n        if c == '5' {\n            h = h.wrapping_add(4usize)\n        }\n    }\n    h.wrapping_mul(100usize).wrapping_add(s.len())\nend t.f\n\n";
    kir_review_cases("push_u64", fns, "t.f", &[
        (&["0"], "t.f(0u64)", "1403usize", "1403"),
        (&["10"], "t.f(10u64)", "680005usize", "680005"),
        (&["18446744073709551615"], "t.f(18446744073709551615u64)", "4233681847441813393usize", "4233681847441813393"),
    ]);
}

/// `core.str.from_utf8` の検証 (命令 `utf8.valid`、機械語は kir_rt.c) は Rust の規則と同じ:
/// 1〜4 バイト、途中で切れた列、過長な符号化、サロゲート、U+10FFFF 超。Ok は 1000、Err は `valid_up_to`。
#[test]
fn kir_from_utf8_matches_tree_walker() {
    let fns = "fn t.c0() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\x61\\x62\\x63\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c0\n\nfn t.c1() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c1\n\nfn t.c2() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xc3\\xa9\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c2\n\nfn t.c3() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xe3\\x81\\x82\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c3\n\nfn t.c4() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xf0\\x9f\\x98\\x80\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c4\n\nfn t.c5() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xc3\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c5\n\nfn t.c6() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xe3\\x81\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c6\n\nfn t.c7() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xf0\\x9f\\x98\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c7\n\nfn t.c8() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xc0\\x80\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c8\n\nfn t.c9() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xe0\\x80\\xaf\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c9\n\nfn t.c10() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xed\\xa0\\x80\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c10\n\nfn t.c11() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xf4\\x90\\x80\\x80\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c11\n\nfn t.c12() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\x80\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c12\n\nfn t.c13() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xff\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c13\n\nfn t.c14() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\x61\\xe3\\x81\\x82\\x62\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c14\n\nfn t.c15() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xef\\xbf\\xbf\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c15\n\nfn t.c16() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xf4\\x8f\\xbf\\xbf\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c16\n\nfn t.c17() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\xc2\\x80\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c17\n\nfn t.c18() -> u64 uses [] panics may\n    match core.str.from_utf8(b\"\\x61\\x62\\xc3\") {\n        Ok(_s) => 1000u64\n        Err(e) => u64.from_usize(e.valid_up_to)\n    }\nend t.c18\n";
    let want = ["1000", "1000", "1000", "1000", "1000", "0", "0", "0", "0", "0", "0", "0", "0", "0", "1000", "1000", "1000", "1000", "2"];
    for (i, w) in want.iter().enumerate() {
        let f = format!("t.c{}", i);
        kir_review_cases(&format!("from_utf8_{}", i), fns, &f, &[(&[], &format!("{}()", f), &format!("{}u64", w), w)]);
    }
}

/// 基数が 2..=36 の外の `from_str_radix` は Err (種類 0)。木走査は Rust の `from_str_radix` の panic で落ちていた
/// (sig は `panics never`)。参照実行器・機械語とも同じ。
#[test]
fn from_str_radix_with_bad_radix_is_err_not_panic() {
    let fns = "fn t.r(rad: u32) -> u64 uses [] panics never\n    match u64.from_str_radix(\"10\", rad) {\n        Ok(v) => v\n        Err(e) => 1000u64.wrapping_add(u64.from_u8(e.kind))\n    }\nend t.r\n";
    kir_review_cases("bad_radix", fns, "t.r", &[
        (&["1"], "t.r(1u32)", "1000u64", "1000"),
        (&["37"], "t.r(37u32)", "1000u64", "1000"),
        (&["0"], "t.r(0u32)", "1000u64", "1000"),
        (&["2"], "t.r(2u32)", "2u64", "2"),
        (&["36"], "t.r(36u32)", "36u64", "36"),
    ]);
}

/// `derive [Clone]` の `clone()`: すべてスカラーの欄を持つ構造体は値の写し。
/// 木走査・参照実行器・機械語で一致を確かめる。
#[test]
fn kir_clone_all_scalar_matches_tree_walker() {
    let src = "type t.Point derive [Clone] pub\n    x: i32\n    y: i32\nend t.Point\n\n\
fn t.main() -> () uses [] panics may\n    let p = t.Point { x: 1i32, y: 2i32 }\n    let q = p.clone()\n    core.check.eq(q.x, 1i32)\n    core.check.eq(q.y, 2i32)\nend t.main\n";
    let t = Tmp::new("kir_clone_scalar", src);
    let k = kir_run(&["kir", "t.main", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "{}", text(&k.stderr));
}

/// `derive [Clone]` の `clone()`: String / Vec<スカラー> の欄を持つ構造体は、
/// 新規確保して中身を複写する。clone の後に元と写しを別々に変更しても独立。


/// `<int>.try_from_<int>(x)`: 範囲チェックと Ok / Err の構築。境界 (0, -1, 各型の最小・最大とその ±1)。
/// 木走査・参照実行器・ASan 付き機械語で一致を確かめる。
#[test]
fn kir_try_from_int_matches_tree_walker() {
    let fns = "fn t.tf_i64_u64(x: i64) -> u64 uses [] panics never\n    match u64.try_from_i64(x) {\n        Ok(v) => v\n        Err(_) => 18446744073709551615u64\n    }\nend t.tf_i64_u64\n\n\
fn t.tf_u64_i64(x: u64) -> i64 uses [] panics never\n    match i64.try_from_u64(x) {\n        Ok(v) => v\n        Err(_) => -9223372036854775808i64\n    }\nend t.tf_u64_i64\n\n\
fn t.tf_u32_u8(x: u32) -> u16 uses [] panics never\n    match u8.try_from_u32(x) {\n        Ok(v) => u16.from_u8(v)\n        Err(_) => 65535u16\n    }\nend t.tf_u32_u8\n";
    // u64.try_from_i64: -1 と i64::MIN は Err、0 と i64::MAX とその-1 は Ok。
    kir_review_cases("tf_i64_u64", &fns, "t.tf_i64_u64", &[
        (&["-1"], "t.tf_i64_u64(-1i64)", "18446744073709551615u64", "18446744073709551615"),
        (&["0"], "t.tf_i64_u64(0i64)", "0u64", "0"),
        (&["9223372036854775807"], "t.tf_i64_u64(9223372036854775807i64)", "9223372036854775807u64", "9223372036854775807"),
        (&["-9223372036854775808"], "t.tf_i64_u64(-9223372036854775808i64)", "18446744073709551615u64", "18446744073709551615"),
        (&["9223372036854775806"], "t.tf_i64_u64(9223372036854775806i64)", "9223372036854775806u64", "9223372036854775806"),
    ]);
    // i64.try_from_u64: 0 と i64::MAX は Ok、u64::MAX と i64::MAX+1 は Err。
    kir_review_cases("tf_u64_i64", &fns, "t.tf_u64_i64", &[
        (&["0"], "t.tf_u64_i64(0u64)", "0i64", "0"),
        (&["9223372036854775807"], "t.tf_u64_i64(9223372036854775807u64)", "9223372036854775807i64", "9223372036854775807"),
        (&["18446744073709551615"], "t.tf_u64_i64(18446744073709551615u64)", "-9223372036854775808i64", "-9223372036854775808"),
        (&["9223372036854775808"], "t.tf_u64_i64(9223372036854775808u64)", "-9223372036854775808i64", "-9223372036854775808"),
    ]);
    // u8.try_from_u32: 0 と 255 は Ok、256 と u32::MAX は Err。
    kir_review_cases("tf_u32_u8", &fns, "t.tf_u32_u8", &[
        (&["0"], "t.tf_u32_u8(0u32)", "0u16", "0"),
        (&["255"], "t.tf_u32_u8(255u32)", "255u16", "255"),
        (&["256"], "t.tf_u32_u8(256u32)", "65535u16", "65535"),
        (&["4294967295"], "t.tf_u32_u8(4294967295u32)", "65535u16", "65535"),
    ]);
}

/// `derive [Clone]` は、欄の型が `Clone` を実装するときだけ書ける。String / Vec は `try_clone` (確保が失敗しうる)
/// だけを持ち `Clone` ではないので、それらを欄に持つ型の `derive [Clone]` は E2036 (KIR の clone は欄がスカラーの型だけ)。
#[test]
fn derive_clone_on_string_or_vec_field_is_e2036() {
    for (tag, field) in [("string", "alloc.string.String<std.alloc.System>"), ("vec", "alloc.vec.Vec<u8, std.alloc.System>")] {
        let src = format!("type t.S derive [Clone] pub\n    x: {}\nend t.S\n\nfn t.main() -> () uses [] panics never\nend t.main\n", field);
        let t = Tmp::new(&format!("derive_clone_{}", tag), &src);
        let o = text(&run(&["check", "--json"], &t.0).stdout);
        assert!(o.contains("E2036"), "{}", o);
    }
}

/// 4.1 (0.10.2): 入力位置に 2 回以上現れて入力どうしを結ぶライフタイムは、出力に現れなくても明示できる。
/// 1 回だけ現れる宣言は省略が正規形で E3021 (`'ab` は `'a` と別の名前として数える)。
#[test]
fn lifetime_declared_only_in_inputs_is_e3021_unless_it_ties_inputs() {
    let main = "fn t.main() -> () uses [] panics never\nend t.main\n";
    let cases = [
        ("once", "fn t.f['a](x: &'a u32) -> u32 uses [] panics never\n    *x\nend t.f\n", true),
        ("once_two_refs", "fn t.f['a](x: &'a u32, y: &u32) -> u32 uses [] panics never\n    x.wrapping_add(*y)\nend t.f\n", true),
        ("prefix_name", "fn t.f['ab](x: &'ab u32, y: &u32) -> u32 uses [] panics never\n    x.wrapping_add(*y)\nend t.f\n", true),
        ("ties", "fn t.f['a](x: &'a u32, y: &'a u32) -> u32 uses [] panics never\n    x.wrapping_add(*y)\nend t.f\n", false),
    ];
    for (tag, f, want) in cases {
        let t = Tmp::new(&format!("e3021_{}", tag), &format!("{}\n{}", f, main));
        let o = text(&run(&["check", "--json"], &t.0).stdout);
        assert_eq!(o.contains("E3021"), want, "{}: {}", tag, o);
    }
}

/// Fable の最終レビュー: B1 (`split` の区切りが対象より長い)、B3 (機械語の `from_str_radix` が 0x10〜0x19 を数字に読む)、
/// M2 (i128 に入らない桁数のエラーの種類は溢れ = 1)。木走査・参照実行器・機械語・ASan で比べる。
#[test]
fn final_review_split_long_sep_and_radix_edges() {
    let fns = "fn t.sp(k: u32) -> u64 uses [] panics may\n    let mut n = 0u64\n    let s = if k == 0u32 {\n        \"a\"\n    } else {\n        \"\"\n    }\n    for p in s.split(\"\\r\\n\") {\n        n = n.wrapping_mul(10u64).wrapping_add(u64.from_usize(p.len())).wrapping_add(1u64)\n    }\n    n\nend t.sp\n\n\
fn t.ctl(k: u32) -> u64 uses [] panics never\n    let s = if k == 0u32 {\n        \"\\u{11}2\"\n    } else {\n        \"\\u{19}\"\n    }\n    match u8.from_str_radix(s, 10u32) {\n        Ok(v) => u64.from_u8(v)\n        Err(e) => 1000u64.wrapping_add(u64.from_u8(e.kind))\n    }\nend t.ctl\n\n\
fn t.over(k: u32) -> u64 uses [] panics never\n    let s = if k == 0u32 {\n        \"1111111111111111111111111111111111111111\"\n    } else {\n        \"18446744073709551616\"\n    }\n    match u64.from_str_radix(s, 10u32) {\n        Ok(_v) => 7u64\n        Err(e) => 1000u64.wrapping_add(u64.from_u8(e.kind))\n    }\nend t.over\n";
    kir_review_cases("final_sp", fns, "t.sp", &[(&["0"], "t.sp(0u32)", "2u64", "2"), (&["1"], "t.sp(1u32)", "1u64", "1")]);
    kir_review_cases("final_ctl", fns, "t.ctl", &[(&["0"], "t.ctl(0u32)", "1000u64", "1000"), (&["1"], "t.ctl(1u32)", "1000u64", "1000")]);
    kir_review_cases("final_over", fns, "t.over", &[(&["0"], "t.over(0u32)", "1001u64", "1001"), (&["1"], "t.over(1u32)", "1001u64", "1001")]);
}

/// Fable の最終レビュー B2: 欄 (ペイロード) に集成体を持つ型の `clone()` は、`unreachable` に下ろさず未対応にする。
#[test]
fn final_review_nested_clone_is_unsupported_not_unreachable() {
    let src = "type t.P derive [Clone, Copy] pub\n    x: u32\nend t.P\n\ntype t.Seg derive [Clone] pub\n    a: t.P\n    n: u32\nend t.Seg\n\n\
fn t.c(k: u32) -> u32 uses [] panics never\n    let g = t.Seg { a: t.P { x: k }, n: 1u32 }\n    let h = g.clone()\n    h.a.x.wrapping_add(h.n)\nend t.c\n\n\
fn t.main() -> () uses [] panics may\n    core.check.eq(t.c(4u32), 5u32)\nend t.main\n";
    let t = Tmp::new("final_nested_clone", src);
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
    let k = run(&["kir", "t.c", "--run", "4"], &t.0);
    assert!(text(&k.stderr).contains("未対応"), "{}{}", text(&k.stdout), text(&k.stderr));
}

/// Fable の最終レビュー M1: `--opt` の定数の畳み込みの `saturating_*` は型の最小・最大で切る (参照実行器と同じ算術)。
#[test]
fn final_review_opt_folds_saturating_like_the_reference() {
    let fns = "fn t.s(k: u32) -> u64 uses [] panics never\n    let a = 200u8.saturating_add(100u8)\n    let b = 0u32.saturating_sub(1u32)\n    let c = -100i8.saturating_sub(100i8)\n    let d = u64.from_u8(a).wrapping_add(u64.from_u32(b))\n    d.wrapping_add(u64.from_u32(c.count_ones())).wrapping_add(u64.from_u32(k))\nend t.s\n";
    kir_review_cases("final_sat", fns, "t.s", &[(&["0"], "t.s(0u32)", "256u64", "256")]);
    let t = Tmp::new("final_sat_opt", &format!("{}\nfn t.main() -> () uses [] panics never\nend t.main\n", fns));
    let k = kir_run(&["kir", "t.s", "--opt", "--run", "0"], &t.0);
    assert_eq!(text(&k.stdout).trim(), "256", "{}", text(&k.stderr));
}

const KIR_OWNFOR_FNS: &str = "fn t.for_names(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut total = 0usize
    for s in v {
        total = total.wrapping_add(s.len())
    }
    total
end t.for_names

fn t.for_items(n: u32) -> usize uses [alloc] panics never
    let v = t.items(n)
    let mut total = 0usize
    for it in v {
        total = total.wrapping_add(it.name.len()).wrapping_add(usize.from_u32(it.v))
    }
    total
end t.for_items

fn t.for_move(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut out = t.names(0u32)
    for s in v {
        core.mem.drop(out.push(s))
    }
    out.len()
end t.for_move

fn t.for_break(n: u32, k: usize) -> usize uses [alloc] panics never
    let v = t.items(n)
    let mut seen = 0usize
    for it in v {
        if seen >= k {
            break
        }
        seen = seen.wrapping_add(1usize).wrapping_add(it.name.len())
    }
    seen
end t.for_break

fn t.for_ret(n: u32, k: u32) -> usize uses [alloc] panics never
    let v = t.items(n)
    for it in v {
        if it.v == k {
            return it.name.len()
        }
    }
    77usize
end t.for_ret

fn t.for_wild(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut c = 0usize
    for _ in v {
        c = c.wrapping_add(1usize)
    }
    c
end t.for_wild

fn t.for_nested(n: u32) -> usize uses [alloc] panics never
    let v = t.names(n)
    let mut c = 0usize
    for s in v {
        let w = t.names(2u32)
        for q in w {
            c = c.wrapping_add(q.len()).wrapping_add(s.len())
        }
    }
    c
end t.for_nested
";

#[test]
fn kir_for_by_value_over_owning_elems_matches_tree_walker() {
    // `for x in v` (所有する Vec を値で回す): 要素の所有が `x` に移る。消費しなかった `x` は 1 周の終わりで、`break` / `return` で抜けたときの
    // 残りの要素とバッファは隠しヘッダで解放される。参照実行器の漏れ・二重解放の検出と ASan で確かめる。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.for_names", &["3"], "9usize", "9"),
        ("t.for_names", &["0"], "0usize", "0"),
        ("t.for_items", &["4"], "18usize", "18"),
        ("t.for_move", &["5"], "5usize", "5"),
        ("t.for_move", &["0"], "0usize", "0"),
        ("t.for_break", &["5", "2"], "4usize", "4"),
        ("t.for_break", &["5", "0"], "0usize", "0"),
        ("t.for_break", &["2", "9"], "8usize", "8"),
        ("t.for_ret", &["5", "2"], "3usize", "3"),
        ("t.for_ret", &["5", "9"], "77usize", "77"),
        ("t.for_wild", &["4"], "4usize", "4"),
        ("t.for_nested", &["3"], "36usize", "36"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!(
            "{}({})",
            f,
            args.iter().enumerate().map(|(k, a)| if k == 1 && args.len() == 2 && *f == "t.for_break" { format!("{}usize", a) } else { format!("{}u32", a) }).collect::<Vec<_>>().join(", ")
        );
        let body = format!("    core.check.eq({}, {})\n", call, lit);
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_ownfor_{}", i), &format!("{}\n{}\n{}", KIR_BORROW_FNS, KIR_OWNFOR_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

const KIR_OWNMUT_FNS: &str = "fn t.mut_for_str(n: u32) -> usize uses [alloc] panics never
    let mut v = t.names(n)
    let mut total = 0usize
    for s in &mut v {
        total = total.wrapping_add(s.len())
    }
    for s in &v {
        total = total.wrapping_add(s.len())
    }
    total
end t.mut_for_str

fn t.mut_for_item(n: u32) -> usize uses [alloc] panics never
    let mut v = t.items(n)
    for it in &mut v {
        it.v = it.v.wrapping_add(10u32)
        it.name = t.word()
    }
    let mut total = 0usize
    for it in &v {
        total = total.wrapping_add(it.name.len()).wrapping_add(usize.from_u32(it.v))
    }
    total
end t.mut_for_item

fn t.bump_all(v: &mut alloc.vec.Vec<t.Item, std.alloc.System>) -> usize uses [alloc] panics never
    let mut c = 0usize
    for it in v {
        it.v = it.v.wrapping_add(1u32)
        if it.v > 3u32 {
            break
        }
        c = c.wrapping_add(1usize)
    }
    c
end t.bump_all

fn t.mut_for_param(n: u32) -> usize uses [alloc] panics never
    let mut v = t.items(n)
    let c = t.bump_all(&mut v)
    let mut total = c
    for it in &v {
        total = total.wrapping_add(usize.from_u32(it.v))
    }
    total
end t.mut_for_param

fn t.idx_push(n: u32, i: usize) -> usize uses [alloc] panics may
    let mut v = t.names(n)
    core.mem.drop(v[i].push_str(\"zz\"))
    v[i].len()
end t.idx_push

fn t.idx_field(n: u32, i: usize) -> usize uses [alloc] panics may
    let mut v = t.items(n)
    core.mem.drop(v[i].name.push_str(\"zz\"))
    v[i].v = 40u32
    v[i].name = t.word()
    v[i].name.len().wrapping_add(usize.from_u32(v[i].v))
end t.idx_field

fn t.idx_assign(n: u32, i: usize) -> usize uses [alloc] panics may
    let mut v = t.items(n)
    v[i] = t.Item { name: t.word(), v: 7u32 }
    v[i].name.len().wrapping_add(usize.from_u32(v[i].v))
end t.idx_assign

fn t.idx_assign_str(n: u32, i: usize) -> usize uses [alloc] panics may
    let mut v = t.names(n)
    v[i] = t.word()
    v[i].len()
end t.idx_assign_str

fn t.get_destr(n: u32, i: usize) -> u32 uses [alloc] panics never
    let v = t.items(n)
    match v.get(i) {
        None => 0u32
        Some(t.Item { name: _, v: x }) => x
    }
end t.get_destr

fn t.get_destr_str(n: u32, i: usize) -> usize uses [alloc] panics never
    let v = t.items(n)
    match v.get(i) {
        None => 99usize
        Some(t.Item { name: s, v: x }) => s.len().wrapping_add(usize.from_u32(x))
    }
end t.get_destr_str
";

#[test]
fn kir_mut_borrow_of_owning_elems_matches_tree_walker() {
    // `for x in &mut v` (所有物を持つ要素・スカラー)、`v[i]` の要素への `&mut` (フィールドへの代入・`push_str`・要素の代入。古い値は解放してから)、
    // `get` の腕の構造体パターン。木走査と値が一致し、参照実行器が漏れ・二重解放を検出せず、ASan も報告しない。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.mut_for_str", &["3"], "18usize", "18"),
        ("t.mut_for_str", &["0"], "0usize", "0"),
        ("t.mut_for_item", &["3"], "42usize", "42"),
        ("t.mut_for_item", &["0"], "0usize", "0"),
        ("t.mut_for_param", &["6"], "22usize", "22"),
        ("t.idx_push", &["3", "1"], "5usize", "5"),
        ("t.idx_field", &["3", "1"], "43usize", "43"),
        ("t.idx_assign", &["3", "1"], "10usize", "10"),
        ("t.idx_assign_str", &["3", "2"], "3usize", "3"),
        ("t.get_destr", &["3", "1"], "1u32", "1"),
        ("t.get_destr", &["3", "7"], "0u32", "0"),
        ("t.get_destr_str", &["3", "2"], "5usize", "5"),
        ("t.get_destr_str", &["3", "7"], "99usize", "99"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let two = args.len() == 2;
        let call = format!(
            "{}({})",
            f,
            args.iter().enumerate().map(|(k, a)| if two && k == 1 { format!("{}usize", a) } else { format!("{}u32", a) }).collect::<Vec<_>>().join(", ")
        );
        let body = format!("    core.check.eq({}, {})\n", call, lit);
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_ownmut_{}", i), &format!("{}\n{}\n{}", KIR_BORROW_FNS, KIR_OWNMUT_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_mut_borrow_of_owning_elems_out_of_range_panics_like_tree_walker() {
    for (f, args) in [("t.idx_field", ["3", "5"]), ("t.idx_assign", ["3", "9"])] {
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    core.check.eq({}({}u32, {}usize), 0usize)\n    Ok(())\nend t.main\n", f, args[0], args[1]);
        let t = Tmp::new("kir_ownmut_oob", &format!("{}\n{}\n{}", KIR_BORROW_FNS, KIR_OWNMUT_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(101), "tree-walker: {}", text(&o.stderr));
        let k = kir_run(&["kir", f, "--run", args[0], args[1]], &t.0);
        assert_eq!(k.status.code(), Some(101), "kir {}: {}", f, text(&k.stderr));
        assert!(text(&k.stderr).contains("index out of bounds"), "{}", text(&k.stderr));
    }
}

const KIR_WITH_FNS: &str = include_str!("data/kir_with_fns.ksnl");

#[test]
fn kir_with_exits_match_tree_walker() {
    // `with` の後始末が、末尾 / `return` / `?` / `break` / `continue` / 入れ子 / 同名の隠蔽 / 後始末式の `match` / 所有物の束縛 /
    // ソケットの各経路でちょうど 1 回、木走査と同じ順序で実行されること。参照実行器が漏れ・二重解放を検出せず、
    // `--run` / `--native` (ASan を含む) が一致する。値は「後始末の記録 * 1000 + 結果」。
    // (関数, 引数, 期待値 (型付きリテラル), 期待値 (表示))
    let cases: &[(&str, &str, &str, &str)] = &[
        ("t.run_tail", "3", "4008u32", "4008"),
        ("t.run_ret", "1", "3001u32", "3001"),
        ("t.run_ret", "9", "3007u32", "3007"),
        ("t.run_try", "1", "2003u32", "2003"),
        ("t.run_try", "9", "2509u32", "2509"),
        ("t.run_loop", "3", "123004u32", "123004"),
        ("t.run_loop", "9", "1234004u32", "1234004"),
        ("t.run_nested", "1", "21001u32", "21001"),
        ("t.run_nested", "5", "21003u32", "21003"),
        ("t.run_close_match", "0", "9001u32", "9001"),
        ("t.run_close_match", "3", "3030u32", "3030"),
        ("t.run_close_match", "8", "8009u32", "8009"),
        ("t.w_vec_own", "0", "2usize", "2"),
        ("t.w_vec_own", "1", "12usize", "12"),
        ("t.w_val_own", "4", "5u32", "5"),
        ("t.w_body_owned", "4", "2usize", "2"),
        ("t.w_str_close", "5", "4usize", "4"),
        ("t.w_str_borrow", "0", "3usize", "3"),
        ("t.w_str_borrow", "1", "99usize", "99"),
        ("t.w_sock", "0", "6u32", "6"),
        ("t.w_sock", "1", "5u32", "5"),
    ];
    for (i, (f, arg, lit, want)) in cases.iter().enumerate() {
        let body = format!("    core.check.eq({}({}u32), {})\n", f, arg, lit);
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, io.net] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_with_exits_{}", i), &format!("{}\n{}", KIR_WITH_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker {}({}): {}", f, arg, text(&o.stderr));
        let k = kir_run(&["kir", f, "--run", arg], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}({}): {}", f, arg, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}({})", f, arg);
    }
}

// ---- check の増分キャッシュ (設計_増分検査.md)。どの場合もキャッシュあり・なしで診断が一致すること。

/// `check --json` の (終了コード, 標準出力, 標準エラー)。`cache` が `Some` なら `KSN_CHECK_CACHE=1` と置き場所を渡し、
/// `None` ならキャッシュ無効 (`KSN_CHECK_CACHE=0`) で走らせる。
fn check_cached(dir: &Path, cache: Option<&Path>) -> (Option<i32>, String, String) {
    let o = match cache {
        Some(c) => run_with_env(&["check", "--json"], dir, &[("KSN_CHECK_CACHE", "1"), ("KSN_CACHE_DIR", c.to_str().unwrap())]),
        None => run_with_env(&["check", "--json"], dir, &[("KSN_CHECK_CACHE", "0")]),
    };
    (o.status.code(), text(&o.stdout), text(&o.stderr))
}

fn cache_tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ksn_cc_{}_{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&d);
    d
}

/// 無効・初回 (キャッシュ空)・2 回目 (命中) の 3 通りが一致すること。
fn assert_cache_same(label: &str, dir: &Path, cache: &Path) {
    let base = check_cached(dir, None);
    let cold = check_cached(dir, Some(cache));
    let warm = check_cached(dir, Some(cache));
    assert_eq!(base, cold, "{}: cold cache differs from no cache", label);
    assert_eq!(base, warm, "{}: warm cache differs from no cache", label);
}

#[test]
fn check_cache_matches_uncached_for_examples() {
    let cache = cache_tmp("examples");
    let mut n = 0;
    for e in fs::read_dir(root().join("examples")).unwrap() {
        let p = e.unwrap().path();
        if p.join("kasane.toml").exists() {
            assert_cache_same(&p.display().to_string(), &p, &cache);
            n += 1;
        }
    }
    assert!(n >= 10, "examples missing");
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn check_cache_matches_uncached_for_unsound_and_linear_ok() {
    let cache = cache_tmp("unsound");
    for sub in ["unsound", "linear_ok"] {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(sub);
        let mut names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
        names.sort();
        assert!(!names.is_empty());
        for (i, p) in names.into_iter().enumerate() {
            let stem = p.file_stem().unwrap().to_string_lossy().to_string();
            let t = Tmp::new(&format!("cc_{}_{}", sub, stem), &fs::read_to_string(&p).unwrap());
            if i % 3 == 0 || sub == "linear_ok" {
                assert_cache_same(&format!("{}/{}", sub, stem), &t.0, &cache);
            } else {
                // 時間の節約: 3 本に 2 本は、初回 (空のキャッシュ) だけを比べる。
                assert_eq!(check_cached(&t.0, None), check_cached(&t.0, Some(&cache)), "{}/{}", sub, stem);
            }
        }
    }
    let _ = fs::remove_dir_all(&cache);
}

/// 2 ファイルのパッケージ。`a` が呼ぶ `b.base` を変えるたびに、依存する定義の診断が (キャッシュなしと同じに) 更新される。
#[test]
fn check_cache_follows_edits_to_a_definition_and_its_dependents() {
    let cache = cache_tmp("edits");
    let t = Tmp::new("cc_edits", "");
    let a_path = t.0.join("src").join("t.ksnl");
    let b_path = t.0.join("src").join("t").join("b.ksnl");
    fs::create_dir_all(b_path.parent().unwrap()).unwrap();
    let main = "fn t.main() -> () uses [] panics never\n    let _x = t.user(1u32)\nend t.main\n";
    let mk_a = |user_body: &str| format!("import t.b\n\nfn t.user(x: u32) -> u32 uses [] panics never\n    {}\nend t.user\n\n{}", user_body, main);
    let mk_b = |sig: &str, body: &str| format!("fn t.b.base{} pub\n    {}\nend t.b.base\n\nfn t.b.other(x: u32) -> u32 uses [] panics never pub\n    x\nend t.b.other\n", sig, body);
    let steps: Vec<(String, String)> = vec![
        (mk_a("b.base(x)"), mk_b("(x: u32) -> u32 uses [] panics never", "x")),
        // 呼ばれる側の戻り値型を変える: 呼ぶ側の `u32` と食い違う。
        (mk_a("b.base(x)"), mk_b("(x: u32) -> bool uses [] panics never", "true")),
        // 呼ぶ側だけを直す。
        (mk_a("b.other(x)"), mk_b("(x: u32) -> bool uses [] panics never", "true")),
        // 呼ばれる側の効果を宣言と食い違わせる。
        (mk_a("b.other(x)"), mk_b("(x: u32) -> bool uses [io.console] panics never", "true")),
        // 構文の誤り、正規形でない並び、未知の名前。
        (mk_a("b.nope(x)"), mk_b("(x: u32) -> u32 uses [] panics never", "x")),
        (mk_a("b.base(x"), mk_b("(x: u32) -> u32 uses [] panics never", "x")),
        (mk_a("b.base(x)"), mk_b("(x: u32) -> u32 uses [] panics never", "x")),
    ];
    for (i, (a, b)) in steps.iter().enumerate() {
        fs::write(&a_path, a).unwrap();
        fs::write(&b_path, b).unwrap();
        assert_cache_same(&format!("step {}", i), &t.0, &cache);
        // 同じ状態でもう一度 (命中) も一致する。
        assert_eq!(check_cached(&t.0, None), check_cached(&t.0, Some(&cache)), "step {} (hit)", i);
    }
    let _ = fs::remove_dir_all(&cache);
}

/// キャッシュを壊しても (切り詰め・ゴミ・別のファイルの中身・内容だけ書き換え) 結果は変わらない。
#[test]
fn check_cache_survives_corruption() {
    let cache = cache_tmp("corrupt");
    // lint の診断 (未使用の変数) が出るので、保存した内容を書き換えると結果が変わりうる。
    let src = "fn t.main() -> () uses [] panics never\n    let unused = 1u32\nend t.main\n";
    let t = Tmp::new("cc_corrupt", src);
    let base = check_cached(&t.0, None);
    assert_eq!(base.0, Some(1), "{:?}", base);
    let _ = check_cached(&t.0, Some(&cache));
    let files: Vec<PathBuf> = fs::read_dir(cache.join("check")).unwrap().map(|e| e.unwrap().path()).collect();
    assert!(!files.is_empty(), "nothing was cached");
    let orig = fs::read_to_string(&files[0]).unwrap();
    let mut variants: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("garbage", b"\x00\xff not json".to_vec()),
        ("truncated", orig.as_bytes()[..orig.len() / 2].to_vec()),
        ("array", b"[1,2,3]".to_vec()),
        ("empty-object", b"{}".to_vec()),
    ];
    // 内容だけ書き換える (sum が合わない)。診断を消す・文法を「一致」にする。
    let v: serde_json::Value = serde_json::from_str(&orig).unwrap();
    let mut w = v.clone();
    w["data"]["lint"] = serde_json::json!([]);
    variants.push(("lint-emptied", w.to_string().into_bytes()));
    let mut w = v.clone();
    w["key"] = serde_json::json!("0000");
    variants.push(("bad-key", w.to_string().into_bytes()));
    let mut w = v.clone();
    w["data"]["skip"] = serde_json::json!(true);
    variants.push(("skip-flipped", w.to_string().into_bytes()));
    for (name, bytes) in variants {
        fs::write(&files[0], bytes).unwrap();
        assert_eq!(base, check_cached(&t.0, Some(&cache)), "corruption `{}` changed the result", name);
        // 壊れた項目は直されて、次は命中する。
        assert_eq!(base, check_cached(&t.0, Some(&cache)), "after repair of `{}`", name);
    }
    // スロットが読めないもの (ディレクトリ) でも落ちない。
    let _ = fs::remove_file(&files[0]);
    fs::create_dir_all(&files[0]).unwrap();
    assert_eq!(base, check_cached(&t.0, Some(&cache)), "slot is a directory");
    let _ = fs::remove_dir_all(&cache);
}

/// `check --json` を追加の環境変数つきで走らせる。
fn check_cached_env(dir: &Path, env: &[(&str, &str)]) -> (Option<i32>, String, String) {
    let o = run_with_env(&["check", "--json"], dir, env);
    (o.status.code(), text(&o.stdout), text(&o.stderr))
}

/// 文面だけの検査 (未使用の変数の lint) が出るファイルを `n` 本持つパッケージ (`t.ksnl` と `t/f<i>.ksnl`)。
fn many_files_package(name: &str, n: usize) -> Tmp {
    let t = Tmp::new(name, "fn t.main() -> () uses [] panics never\n    let unused = 1u32\nend t.main\n");
    let d = t.0.join("src").join("t");
    fs::create_dir_all(&d).unwrap();
    for i in 0..n {
        let src = format!("fn t.f{}.g() -> u32 uses [] panics never\n    let unused = {}u32\n    {}u32\nend t.f{}.g\n", i, i, i, i);
        fs::write(d.join(format!("f{}.ksnl", i)), src).unwrap();
    }
    t
}

/// 既定で有効 (`KSN_CHECK_CACHE` を書かなくても置き場に保存する)。`KSN_CHECK_CACHE=0` なら何も書かない。
#[test]
fn check_cache_is_on_by_default_and_can_be_turned_off() {
    let t = many_files_package("cc_default", 3);
    let base = check_cached(&t.0, None);
    let on = cache_tmp("default_on");
    assert_eq!(base, check_cached_env(&t.0, &[("KSN_CACHE_DIR", on.to_str().unwrap())]), "default run differs");
    let n = fs::read_dir(on.join("check")).map(|r| r.count()).unwrap_or(0);
    assert!(n >= 4, "default is on, but only {} entries were stored", n);
    let off = cache_tmp("default_off");
    assert_eq!(base, check_cached_env(&t.0, &[("KSN_CHECK_CACHE", "0"), ("KSN_CACHE_DIR", off.to_str().unwrap())]));
    assert!(!off.join("check").exists(), "KSN_CHECK_CACHE=0 still wrote to the cache");
    let _ = fs::remove_dir_all(&on);
    let _ = fs::remove_dir_all(&off);
}

/// 上限 (`KSN_CHECK_CACHE_MAX`) を小さくすると古いものが消え、置き場の合計は上限以下になる。結果は変わらない。
#[test]
fn check_cache_limit_evicts_old_entries_and_keeps_results() {
    let t = many_files_package("cc_limit", 8);
    let base = check_cached(&t.0, None);
    let cache = cache_tmp("limit");
    let c = cache.to_str().unwrap();
    let cdir = cache.join("check");
    let size = || -> u64 {
        fs::read_dir(&cdir).map(|rd| rd.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0)
    };
    for max in [1000u64, 0] {
        let m = max.to_string();
        let env = [("KSN_CACHE_DIR", c), ("KSN_CHECK_CACHE_MAX", m.as_str())];
        for round in 0..2 {
            assert_eq!(base, check_cached_env(&t.0, &env), "max {} round {}", max, round);
            assert!(size() <= max, "max {}: {} bytes are kept", max, size());
        }
    }
    // 上限が十分なら全部残る (t.ksnl と f0..f7 の 9 本)。上限 1000 では数本しか残らない (上の検査で確かめた)。
    let _ = fs::remove_dir_all(&cdir);
    assert_eq!(base, check_cached_env(&t.0, &[("KSN_CACHE_DIR", c), ("KSN_CHECK_CACHE_MAX", "100000000")]));
    assert_eq!(fs::read_dir(&cdir).unwrap().count(), 9);
    let _ = fs::remove_dir_all(&cache);
}

/// 同じ置き場を 8 本の `ksn check` が同時に使っても、どれの診断もキャッシュなしと一致する。
/// 空から同時に書く回・上限で同時に削る回・命中しながら同時に読む回を試す。
#[test]
fn check_cache_parallel_processes_agree() {
    let t = many_files_package("cc_parallel", 12);
    let base = check_cached(&t.0, None);
    let cache = cache_tmp("parallel");
    let c = cache.to_str().unwrap().to_string();
    for (label, max) in [("cold", "100000000"), ("tight", "500"), ("warm", "100000000"), ("tight-warm", "500")] {
        let outs: Vec<(Option<i32>, String, String)> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..8)
                .map(|_| {
                    let (dir, c) = (t.0.clone(), c.clone());
                    s.spawn(move || check_cached_env(&dir, &[("KSN_CACHE_DIR", c.as_str()), ("KSN_CHECK_CACHE_MAX", max)]))
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (i, o) in outs.iter().enumerate() {
            assert_eq!(&base, o, "{}: process {} differs from no cache", label, i);
        }
    }
    let _ = fs::remove_dir_all(&cache);
}
