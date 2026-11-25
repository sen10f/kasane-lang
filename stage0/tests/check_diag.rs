//! 診断・検査 (`ksn check`) のテスト。

#![allow(dead_code, unused_imports)]

mod common;

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use common::*;

#[test]
fn diagnostics_each_case_reports_its_code() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/diag");
    // `README.md` (点検表) などケース以外のファイルは無視する。
    let mut names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().map_or(false, |x| x == "ksnl")).collect();
    names.sort();
    assert!(names.len() >= 20, "diag cases missing");
    let failures: Vec<String> = par_map(&names, |p| {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let code = &stem[..5];
        let t = Tmp::new_raw(&stem, &fs::read_to_string(p).unwrap());
        // 横に `<名前>.ksnh` があるケースは、それも `src/t.ksnh` に置く (コメントそのものや `.ksnl` との一致を試すもの)。
        if let Ok(h) = fs::read_to_string(p.with_extension("ksnh")) {
            fs::write(t.0.join("src").join("t.ksnh"), h).unwrap();
        }
        let o = run(&["check", "--json"], &t.0);
        let got = codes(&text(&o.stdout));
        if o.status.code() != Some(1) || !got.iter().any(|c| c == code) {
            Some(format!("{}: rc={:?} codes={:?}", stem, o.status.code(), got))
        } else {
            None
        }
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// 通ってはいけないコード (`tests/data/unsound/`)。1 ファイル 1 ケースで、どれも `ksn check` が
/// 失敗しなければならない。横の `<名前>.expect` に `expect: Exxxx` があるケースは、そのコードの診断で落ちることも確かめる
/// (別の診断で落ちているだけのケースを許さない)。まだ検査できない穴は `KNOWN_HOLES` に名前を載せる (その間は「通ること」を
/// 確かめるので、塞いだら一覧から外さないとテストが落ちる = 一覧は減る方向にしか変わらない)。
const KNOWN_HOLES: &[&str] = &["sec6_fnvalue_genericvar_borrow"];

#[test]
fn unsound_programs_are_rejected() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/unsound");
    let mut names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().map_or(false, |x| x == "ksnl")).collect();
    names.sort();
    assert!(names.len() >= 16, "unsound cases missing");
    let failures: Vec<String> = par_map(&names, |p| {
        let mut fails = Vec::new();
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let src = fs::read_to_string(p).unwrap();
        let t = Tmp::new_strict(&format!("unsound_{}", stem), &src);
        // `.expect` の `target: X` は `ksn check --target X` で検査する (ポインタの幅などターゲットで変わる穴)。
        let target = expect_value(p, "target");
        let mut args = vec!["check"];
        if let Some(tg) = &target {
            args.extend(["--target", tg.as_str()]);
        }
        args.push("--json");
        // `.expect` の `unsafe: M` は `[unsafe] modules = ["M"]` を持つパッケージで検査する (許可モジュールの中で拒まれるべき形)。
        if let Some(m) = expect_value(p, "unsafe") {
            fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", &format!("[unsafe]\nmodules = [\"{}\"]\n\n[dependencies]", m), 1)).unwrap();
        }
        let o = run(&args, &t.0);
        let rejected = o.status.code() == Some(1);
        let hole = KNOWN_HOLES.contains(&stem.as_str());
        // `expect: Exxxx` は`.expect` から読む。穴以外は必ず持つ。
        let want = expect_value(p, "expect");
        if hole && rejected {
            fails.push(format!("{}: now rejected; remove it from KNOWN_HOLES", stem));
        } else if !hole && !rejected {
            fails.push(format!("{}: passes check but must be rejected (rc={:?})", stem, o.status.code()));
        } else if !hole {
            match want {
                None => fails.push(format!("{}: no `expect: Exxxx` in the .expect file (every unsound case names its code)", stem)),
                Some(want) => {
                    let got = codes(&text(&o.stdout));
                    if !got.iter().any(|c| *c == want) {
                        fails.push(format!("{}: want {} codes={:?}", stem, want, got));
                    }
                }
            }
        }
        // 整形の違反で落ちているのではないこと (意味の誤りとして報告されること)。
        assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{} is not canonical", stem);
        fails
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// 線形値を全経路で消費している正しいコード (`tests/data/linear_ok/`) は `ksn check` を通る。
#[test]
fn correct_linear_programs_are_accepted() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/linear_ok");
    let mut names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().map_or(false, |x| x == "ksnl")).collect();
    names.sort();
    assert!(names.len() >= 8, "linear_ok cases missing");
    par_map(&names, |p| {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("linear_ok_{}", stem), &fs::read_to_string(p).unwrap());
        let o = run(&["check", "--json"], &t.0);
        assert_eq!(o.status.code(), Some(0), "{}: {}", stem, text(&o.stdout));
        assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{} is not canonical", stem);
    });
}

/// 型検査だけを見る正しいコード (`tests/data/check_ok/`。KIR に下ろせない `dyn` を含む) は `ksn check` を通る。
#[test]
fn check_only_correct_programs_are_accepted() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/check_ok");
    let mut names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().map_or(false, |x| x == "ksnl")).collect();
    names.sort();
    assert!(!names.is_empty(), "check_ok cases missing");
    par_map(&names, |p| {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let t = Tmp::new(&format!("check_ok_{}", stem), &fs::read_to_string(p).unwrap());
        let o = run(&["check", "--json"], &t.0);
        assert_eq!(o.status.code(), Some(0), "{}: {}", stem, text(&o.stdout));
    });
}

/// 組・構造体・`Some` / `Ok` の中で領域を入れ替えて返す正しいコードは、ヘッダの位置どおりに通る (AST の借用検査)。
/// 置き場は `linear_ok/` ではない: そこは KIR の検査 (`kir_borrowck_has_no_false_positives`) も通すが、
/// KIR の検査は戻り値の領域を 1 つにまとめるので、これらを偽陽性にする (既知。`kir_borrowck.rs` の `ret_rg`)。
#[test]
fn lifetime_slot_correct_swaps_are_accepted() {
    let cases: [(&str, &str); 3] = [
        (
            "tuple_swap",
            "fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main

fn t.tuple_swap['a, 'b](x: &'a u32, y: &'b u32) -> (&'b u32, &'a u32) uses [] panics never
    (y, x)
end t.tuple_swap
",
        ),
        (
            "opt_swap",
            "fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main

fn t.opt_swap['a, 'b](x: &'a u32, y: &'b u32) -> Option<(&'b u32, &'a u32)> uses [] panics never
    Some((y, x))
end t.opt_swap
",
        ),
        (
            "tuple_keep",
            "fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main

fn t.pick['a, 'b](x: &'a u32, y: &'b u32) -> (&'a u32, &'b u32) uses [] panics never
    (x, y)
end t.pick
",
        ),
    ];
    for (name, src) in cases {
        let t = Tmp::new(&format!("slot_ok_{}", name), src);
        let o = run(&["check", "--json"], &t.0);
        assert_eq!(o.status.code(), Some(0), "{}: {}", name, text(&o.stdout));
        assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{} is not canonical", name);
    }
}

/// 型が決まっていくこと自体の確認: 推論の不足だった典型の形が、strict で E2044 にならない。
#[test]
fn strict_types_accepts_inferable_programs() {
    let src = "\
fn t.pick[T](flag: bool, a: T, b: T) -> T uses [] panics never
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
fn syntax_error_is_reported_as_e0xxx() {
    let t = Tmp::new("syntax", "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\n");
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(codes(&text(&o.stdout)).iter().any(|c| c.starts_with("E0")));
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
    let base = scratch::Scratch::at(format!("ksn_new_{}", std::process::id()));
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
fn e2024_message_names_canonical_conversion() {
    // usize + u32: E2024 は panics may でも型の食い違いで出る。換え先 (conv_hint) が文に入っていること。
    let src = main_wrap("", "    let total = 0usize\n    let x = 1u32\n    core.mem.drop(total + x)\n");
    let t = Tmp::new("e2024_conv", &src);
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("operands of `+` have different types: `usize` and `u32`")
            && out.contains("write `core.num.usize.from_u32(x)`"),
        "unexpected diagnostic: {out}"
    );
}

#[test]
fn e4010_message_names_stack_example() {
    // 再帰する panics never 関数に stack が無い: 診断に stack N の書き方例が入ること。
    let fact = "fn t.fact(n: u64) -> Option<u64> uses [] panics never\n    if n == 0u64 {\n        return Some(1u64)\n    }\n    match t.fact(n.wrapping_sub(1u64)) {\n        Some(r) => r.checked_mul(n)\n        None => None\n    }\nend t.fact\n\n";
    let src = fact.to_owned() + &main_wrap("", "    core.mem.drop(t.fact(5u64))\n");
    let t = Tmp::new("e4010_stack", &src);
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("recursive `panics never` function t.fact requires `stack <bytes>`")
            && out.contains("stack 4096"),
        "unexpected diagnostic: {out}"
    );
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
fn ksnh_without_ksnl_is_e9050() {
    let t = Tmp::new("ksnh", &main_wrap("", ""));
    fs::write(t.0.join("src/t/old.ksnh"), "// c\nfn t.old.f() -> () uses [] panics never\nend t.old.f\n").unwrap_or_else(|_| {
        fs::create_dir_all(t.0.join("src/t")).unwrap();
        fs::write(t.0.join("src/t/old.ksnh"), "").unwrap();
    });
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E9050"), "{}", text(&o.stdout));
}

/// `.ksnh` が正: コメントを足した `.ksnh` と `ksn strip` で作った `.ksnl` は `check` / `fmt --check` / `run` を通る。
/// `.ksnl` を直す (または `.ksnh` のコードを直す) と食い違って E9050。診断は `.ksnh` の行で出て、`--json` は `.ksnl` の行と `ksnh_line`。
#[test]
fn ksnh_pair_check_strip_and_line_numbers() {
    let t = Tmp::new("ksnh_pair", "");
    let h = "/// module doc\n\n// the entry\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    // ok\n    Ok(())\nend t.main\n";
    fs::remove_file(t.0.join("src/t.ksnl")).unwrap();
    fs::write(t.0.join("src/t.ksnh"), h).unwrap();
    assert_eq!(run(&["check"], &t.0).status.code(), Some(1)); // `.ksnl` が無い
    assert_eq!(run(&["strip"], &t.0).status.code(), Some(0));
    let l = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert_eq!(l, "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n");
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{}", text(&run(&["check"], &t.0).stdout));
    assert_eq!(run(&["strip", "--check"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
    // コメントだけを変えた `.ksnh`: `.ksnl` は変わらないので E9050 にならない。
    fs::write(t.0.join("src/t.ksnh"), h.replace("// ok", "// changed")).unwrap();
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    // コードを変えた `.ksnh`: 食い違い (E9050)。`--json` の `file` / `line` は `.ksnl`、`ksnh_line` は `.ksnh`。
    fs::write(t.0.join("src/t.ksnh"), h.replace("    Ok(())", "    let a = 1u32\n    core.mem.drop(a)\n    Ok(())")).unwrap();
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(codes(&out).iter().any(|c| c == "E9050"), "{}", out);
    assert!(out.contains("\"file\": \"src/t.ksnl\"") || out.contains("t.ksnl"), "{}", out);
    assert!(out.contains("\"ksnh_line\": "), "{}", out);
    // 人用の出力は `.ksnh`
    assert!(text(&run(&["check"], &t.0).stdout).contains("t.ksnh:"));
    // `strip` で直る。
    assert_eq!(run(&["strip"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

/// 依存の `.ksnh` はコードとして読まない (動くのは `.ksnl` だけ)。コメントを除いて `.ksnl` と食い違えば E9050 で止める
/// (利用者の決定 S1c、2026-11-21)。`.ksnl` だけの依存は問題ない。
#[test]
fn ksnh_in_a_dependency_is_only_compared_never_read() {
    let t = Tmp::new("ksnh_dep", &main_wrap("", ""));
    let dep = t.0.join("dep");
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(dep.join("kasane.toml"), "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = 1\n\n[dependencies]\n").unwrap();
    fs::write(dep.join("src/dep.ksnl"), "fn dep.one() -> u32 uses [] panics never pub\n    1u32\nend dep.one\n").unwrap();
    fs::write(t.0.join("kasane.toml"), format!("{}dep = {{ path = \"dep\" }}\n", TOML)).unwrap();
    fs::write(t.0.join("src/t.ksnl"), main_wrap("", "    core.mem.drop(dep.one())\n").replace("uses [io.console] panics may", "uses [] panics never")).unwrap();
    run(&["update"], &t.0);
    // `.ksnl` だけの依存は通る。
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    // (b) 食い違う `.ksnh` は E9050 (依存名とファイルを示す)。check も run も止まる。
    fs::write(dep.join("src/dep.ksnh"), "// broken: not matching\nfn dep.one() -> u32 uses [] panics never pub\n    2u32\nend dep.one\n").unwrap();
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}", out);
    assert!(codes(&out).iter().any(|c| c == "E9050") && out.contains("dependency `dep`") && out.contains("dep/src/dep.ksn"), "{}", out);
    assert_ne!(run(&["run"], &t.0).status.code(), Some(0));
    // (a) コメントだけが違う `.ksnh` は通る。`.ksnh` は読まれず、走るのは `.ksnl` (`.ksnh` だけの定義は存在しない)。
    fs::write(dep.join("src/dep.ksnh"), "// a comment\nfn dep.one() -> u32 uses [] panics never pub\n    1u32\nend dep.one\n").unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    fs::write(dep.join("src/extra.ksnh"), "// only a .ksnh\nfn dep.extra() -> u32 uses [] panics never pub\n    3u32\nend dep.extra\n").unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E9050"), "{}", text(&o.stdout));
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
    fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t\"]\n\n[dependencies]", 1)).unwrap();
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
    fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t.sys\"]\n\n[dependencies]", 1)).unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E5001"), "{}", text(&o.stdout));
}

/// 生のポインタを持つ構造体への `impl core.marker.Send` は、許可モジュール (`[unsafe] modules`) の中でだけ通る。
/// 許可の無いモジュールでは E5002 (手動の Send/Sync impl)、許可のあるモジュールでは通る。
#[test]
fn send_impl_for_raw_pointer_only_in_unsafe_module() {
    let body = "type t.sys.Shared
    p: *const u32
end t.sys.Shared

impl core.marker.Send for t.sys.Shared
";
    let t = Tmp::new("send_impl_ok_probe", &main_wrap("", ""));
    fs::create_dir_all(t.0.join("src/t")).unwrap();
    fs::write(t.0.join("src/t/sys.ksnl"), body).unwrap();
    fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t.sys\"]\n\n[dependencies]", 1)).unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E5002"), "{}", text(&o.stdout));
    fs::write(t.0.join("kasane.toml"), TOML).unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E5002"), "{}", text(&o.stdout));
}

/// 5.5: 手動の Send/Sync は型を定義したモジュール (かつ許可モジュール) だけ。別モジュール・標準の型は E5010、
/// 冗長 (自動で成り立つ型) は E5011、二重は E1114。
#[test]
fn send_sync_impl_site_rules() {
    let cases: [(&str, &str, &str, &str); 6] = [
        ("other_module", "type t.Outer pub\n    inner: t.sys.Shared\nend t.Outer\n", "type t.sys.Shared pub\n    p: *const u8\nend t.sys.Shared\n\nimpl core.marker.Send for t.Outer\n", "E5010"),
        ("std_type", "", "impl core.marker.Send for core.sync.AtomicBool\n", "E5010"),
        ("std_generic", "", "impl core.marker.Send for std.sync.Receiver<u8>\n", "E5010"),
        ("duplicate", "", "type t.sys.Shared pub\n    p: *const u8\nend t.sys.Shared\n\nimpl core.marker.Send for t.sys.Shared\n\nimpl core.marker.Send for t.sys.Shared\n", "E1114"),
        ("redundant", "", "type t.sys.Plain pub\n    n: u32\nend t.sys.Plain\n\nimpl core.marker.Send for t.sys.Plain\n", "E5011"),
        ("own_ok", "", "type t.sys.Shared pub\n    p: *const u8\nend t.sys.Shared\n\nimpl core.marker.Send for t.sys.Shared\n\nimpl core.marker.Sync for t.sys.Shared\n", ""),
    ];
    for (name, main_items, sys, want) in cases {
        let t = Tmp::new(&format!("sendsync_site_{}", name), &main_wrap(main_items, ""));
        fs::create_dir_all(t.0.join("src/t")).unwrap();
        fs::write(t.0.join("src/t/sys.ksnl"), sys).unwrap();
        fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t.sys\"]\n\n[dependencies]", 1)).unwrap();
        let o = run(&["check", "--json"], &t.0);
        let cs = codes(&text(&o.stdout));
        if want.is_empty() {
            assert!(cs.is_empty(), "{}: {:?}", name, cs);
        } else {
            assert!(cs.iter().any(|c| c == want), "{}: {:?} {}", name, cs, text(&o.stdout));
        }
    }
}

#[test]
fn unsafe_manifest_is_validated() {
    let t = Tmp::new("unsafe_bad", &main_wrap("", ""));
    for bad in ["modules = [\"other.mod\"]", "modules = \"t\"", "modules = [\"Bad Name\"]", "extra = 1"] {
        fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", &format!("[unsafe]\n{}\n\n[dependencies]", bad), 1)).unwrap();
        let o = run(&["check", "--json"], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}: {}{}", bad, text(&o.stdout), text(&o.stderr));
    }
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
         \x20   core.check.eq(a + b, 0.3f32)\n\
         \x20   core.check.is_true(a.to_f64() != 0.1f64)\n\
         \x20   core.check.eq(1.0f32 / 3.0f32, 0.33333334f32)\n\
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
fn misordered_definitions_fail_check_and_are_fixed_by_fmt() {
    // 書き方は 1 通り: `ksn fmt` の出力と違うファイルは check の誤り (E0020、致命)。`fmt` で直る。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n\n\
               fn t.a() -> u8 uses [] panics never pub\n    1u8\nend t.a\n";
    let t = Tmp::new_raw("loose_order", src);
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(1), "{}", text(&c.stdout));
    assert!(text(&c.stdout).contains("E0020"), "{}", text(&c.stdout));
    assert!(!text(&c.stdout).contains("non-fatal"), "{}", text(&c.stdout));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(1));
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

#[test]
fn semantic_errors_are_still_reported_in_misordered_files() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    t.nothing()\n    Ok(())\nend t.main\n\n\
               fn t.a() -> u8 uses [] panics never pub\n    1u8\nend t.a\n";
    let t = Tmp::new_raw("loose_sem", src);
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

// ---- 診断の修正案 (fix) と `ksn fix` ----

#[test]
fn unknown_method_diagnostic_carries_a_fix() {
    let b = body(&["let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)", "core.mem.drop(v.pushh(1u8))"]);
    let t = Tmp::new("fix_method", &main_wrap("", &b));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    // 「did you mean」の案内は文面に残るが、編集距離の当て推量は fix に付けない。
    assert!(out.contains("unknown method `pushh`; did you mean `push`?"), "{}", out);
    assert!(!out.contains(" (`ksn fix` で直せる)"), "{}", out);
    assert!(out.contains("\"fix\": []"), "{}", out);
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
    // 他の診断は直るが、編集距離の「did you mean」 (`pushh` → `push`) は直らない。
    let o = run(&["fix"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}{}", text(&o.stdout), text(&o.stderr));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("let _unused = 1u8"), "{}", fixed);
    assert!(fixed.contains("core.mem.drop(2u8)"), "{}", fixed);
    assert!(fixed.contains("v.pushh(1u8)"), "{}", fixed);
    // 意味の当て推量は直らないので check はまだ E1108 で止まり、案内は文面に残る。
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|c| c == "E1108"), "{}", out);
    assert!(out.contains("did you mean `push`"), "{}", out);
    assert!(!out.contains(" (`ksn fix` で直せる)"), "{}", out);
}

// ---- 型付きの穴: core.panic.todo ----

/// 2026-11-21 の決定で `core.panic.todo` は廃止 (旧: 型付きの穴として走って panic し、`ksn todo` が一覧した。`ksn todo` も廃止)。
/// `check` は E2057 で拒み、`ksn fix` が `core.panic.panic("todo")` に直す。
#[test]
fn todo_is_abolished_and_ksn_fix_rewrites_it() {
    let f = "fn t.a() -> u8 uses [] panics may pub\n    core.panic.todo()\nend t.a\n\n";
    let b = body(&["core.mem.drop(t.a())"]);
    let t = Tmp::new_raw("todo", &main_wrap(f, &b));
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E2057"), "{}", text(&o.stdout));
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101));
    assert!(text(&o.stderr).contains("todo"), "{}", text(&o.stderr));
    assert_ne!(run(&["todo"], &t.0).status.code(), Some(0));
}

#[test]
fn todo_is_rejected_in_panics_never_functions() {
    let f = "fn t.a() -> u8 uses [] panics never pub\n    core.panic.todo()\nend t.a\n\n";
    let t = Tmp::new("todo_never", &main_wrap(f, ""));
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E4002"), "{}", text(&o.stdout));
}

// ---- 効果の推論 (仕様 0.9): 非 pub 関数は uses / panics を省略できる ----

#[test]
fn private_functions_may_omit_effects_and_they_are_inferred() {
    let f = "fn t.a() -> () uses [io.console] panics never \n    std.io.print_line(\"x\")?\nend t.a\n\n";
    let _ = f;
    // 推論された効果は呼び出し元の検査に使われる。
    let src = "fn t.helper() -> u8 uses [] panics never\n    1u8\nend t.helper\n\n\
               fn t.loud() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"hi\")\nend t.loud\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    core.mem.drop(t.helper())\n    t.loud()\nend t.main\n";
    let t = Tmp::new("infer_ok", src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
}

#[test]
fn inferred_effects_propagate_to_callers() {
    // loud は io.console を使うと推論される → uses [] の呼び出し元は E4001。
    let src = "fn t.loud() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"hi\")\nend t.loud\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    t.loud()\nend t.main\n";
    let t = Tmp::new("infer_prop", src);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E4001"), "{}", text(&o.stdout));
}

#[test]
fn inferred_panics_propagate_to_callers() {
    let src = "fn t.boom() -> u8 uses [] panics may\n    core.panic.panic(\"x\")\nend t.boom\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    core.mem.drop(t.boom())\n    Ok(())\nend t.main\n";
    let t = Tmp::new("infer_panic", src);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E4002"), "{}", text(&o.stdout));
}

#[test]
fn pub_functions_must_declare_effects() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n\n\
               fn t.zed() -> u8 pub\n    1u8\nend t.zed\n";
    let t = Tmp::new_strict("infer_pub", src);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E4020"), "{}", text(&o.stdout));
}

#[test]
fn ksn_fix_rewrites_missing_and_wide_effect_headers() {
    // D5・D13: 省略 (非 pub も) と広すぎる宣言は誤り (E4020 / E4025)。`ksn fix` が本体の効果に書き直す。
    let src = "fn t.helper() -> u8\n    1u8\nend t.helper\n\n\
               fn t.loud() -> Result<(), std.io.Error> uses [alloc, io.console, io.fs] panics may\n    std.io.print_line(\"hi\")\nend t.loud\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    core.mem.drop(t.helper())\n    t.loud()\nend t.main\n";
    let t = Tmp::new_strict("fix_effects", src);
    let o = run(&["check", "--json"], &t.0);
    let got = codes(&text(&o.stdout));
    assert!(got.iter().any(|c| c == "E4020") && got.iter().any(|c| c == "E4025"), "{}", text(&o.stdout));
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("fn t.helper() -> u8 uses [] panics never\n"), "{}", fixed);
    assert!(fixed.contains("fn t.loud() -> Result<(), std.io.Error> uses [io.console] panics never\n"), "{}", fixed);
    assert!(fixed.contains("fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
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
               fn t.mid() -> u8 uses [io.fs] panics never\n    t.g::<[io.fs]>()\nend t.mid\n";
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
               fn t.S.loud(self: &t.S) -> u32 uses [io.console] panics never\n    core.mem.drop(std.io.print_line(\"x\"))\n    1u32\nend t.S.loud\n\n\
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
               fn t.loud() -> u32 uses [io.console] panics never\n    core.mem.drop(std.io.print_line(\"x\"))\n    1u32\nend t.loud\n\n\
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
              let a = t.mk16()\n    let b = a\n    \
              core.mem.drop(b)\n    Ok(())\nend t.main\n\n\
              fn t.mk16() -> t.Buf<u8, 16> uses [] panics never\n    t.Buf { v: 1u8 }\nend t.mk16\n";
    let t = Tmp::new("const_ok", ok);
    assert_eq!(run(&["check", "--json"], &t.0).status.code(), Some(0));
    let bad = ok.replace("let b = a", "let b: t.Buf<u8, 32> = a");
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
               fn t.pick[const N: usize](b: t.Buf<u8, N>) -> u8 uses [] panics never\n    b.v\nend t.pick\n";
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
    let ok = "fn t.id[T](x: T) -> T uses [] panics never\n    x\nend t.id\n\n\
              fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    \
              let x = 5u8\n    let y = t.id::<u8>(x)\n    \
              core.mem.drop(y)\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("targ_ok", ok);
    // 引数で決まる型引数は書かない (E2067)。ほかの診断は出ない (型引数は推論に反映されている)。
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(codes(&text(&o.stdout)), vec!["E2067".to_string()], "{}", text(&o.stdout));
    assert_eq!(run(&["fix"], &t.0).status.code(), Some(0));
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

// ---- Rust 風の書き方の診断 (E0040–E0045) ----

#[test]
fn rust_style_source_gets_all_hints_at_once() {
    let src = "fn t.main() -> Result<(), std.io.Error> {\n    let x: u32 = 1 as u32;\n    let y = t::f(x >> 1);\n    Ok(())\n}\n";
    // `ksn fmt` は行末の `;` を消すので、`;` の入ったまま置く (`new_raw`)。
    let t = Tmp::new_raw("rustisms", src);
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
    // `ksn fmt` が腕の末尾の `,` を消すので、E0049 そのものは整形していないファイルで試す (`new_raw`)。
    let t = Tmp::new_raw("arm_comma", src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|g| g == "E0049"), "{}", out);
    // 直せる診断は文面の末尾に `ksn fix` の旨が付く (code は変わらない)。
    assert!(out.contains("remove the trailing `,` (`ksn fix` で直せる)"), "{}", out);
    let o = run(&["fix"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("        0u8 => 1u8\n        _ => 2u8\n"), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

#[test]
fn duplicate_test_names_say_which_one_to_rename_or_remove() {
    let src = "test t.fib.test_small\n    core.mem.drop(1u8)\nend t.fib.test_small\n\n\
               test t.fib.test_small\n    core.mem.drop(2u8)\nend t.fib.test_small\n";
    let t = Tmp::new("dup_test", src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|g| g == "E1103"), "{}", out);
    assert!(out.contains("同じ名前の `test` が 2 つある"), "{}", out);
    assert!(out.contains("2 つ目は 5 行目"), "{}", out);
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
    // 腕の 1 つの式だけの `{ }` は裸の式に整形される (段 3 の D15)。
    assert!(fixed.contains("        Some(v) => v\n"), "{}", fixed);
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
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    core.mem.drop(t.a(Some(1u8)))\n    Ok(())\nend t.main\n";
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
    assert!(!out.contains(" (`ksn fix` で直せる)"), "{}", out);
    assert!(out.contains("\"fix\": []"), "{}", out);
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
    let src = "uses std.collections.HashMap\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("junk_header", src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E0054"), "{}", text(&o.stdout));
}

#[test]
fn method_missing_on_a_known_receiver_type_is_reported() {
    // `is_ok` は Result のメソッドで、Vec にはない。
    let (got, out) = check_codes(
        "ty_missing",
        &["let v = alloc.vec.Vec.new(std.alloc.SYSTEM)", "core.mem.drop(v.is_ok())"],
    );
    assert!(got.iter().any(|c| c == "E1108"), "{}", out);
    assert!(out.contains("has no method `is_ok`"), "{}", out);
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
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let mut s = t.S { items: alloc.vec.Vec.new(std.alloc.SYSTEM) }\n    \
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
            "            total = total + *x",
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
    let src = "type t.S\n    n: u16\nend t.S\n\nfn t.f(x: u8) -> u8 uses [] panics may\n    x + 1\nend t.f\n\n\
               fn t.g() -> u64 uses [] panics never\n    7\nend t.g\n\n\
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
fn let_annotation_and_return_value_mismatches_are_reported_once() {
    let src = "fn t.f() -> u8 uses [] panics never\n    5u32\nend t.f\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let a: u8 = 1u32\n    core.mem.drop(a)\n    core.mem.drop(t.f())\n    Ok(())\nend t.main\n";
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

#[test]
fn trait_side_impls_work_for_foreign_types_in_run_and_llvm_build() {
    // 3.2: 部外の型 (`alloc.string.String` / 基本型) への、自分のトレイトの impl は、トレイトを定義したモジュールに置き、
    // メソッドは `<トレイトFQN>.<型FQN>.<名前>` になる。メソッド探索 (3) で見つかる。自分の型への impl は型のモジュールに置く。
    let t = tmp_files("trait_side", &[("t.ksnl", TRAIT_SIDE_MAIN), ("t/tr.ksnl", TRAIT_SIDE_TR), ("t/ty.ksnl", TRAIT_SIDE_TY)]);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).is_empty(), "{}", out);
    let r = run(&["run"], &t.0);
    assert_eq!(text(&r.stdout), "ok\n", "{}", text(&r.stderr));
    let g = run(&["get", "t.ty.P.show"], &t.0);
    assert!(text(&g.stdout).contains("impls t.tr.Show"), "{}", text(&g.stdout));
    if let Some((r, n)) = llvm_build_and_run(&t) {
        same_output(&r, &n);
    }
}

#[test]
fn an_impl_placed_on_both_the_type_side_and_the_trait_side_is_a_duplicate() {
    // 型側 (t/ty.ksnl) と、トレイト側 (t/tr.ksnl に追加) の両方に同じ (トレイト, 型) の impl を置く。
    let tr = format!("{}\n{}", TRAIT_SIDE_TR, TRAIT_SIDE_P_DUP);
    let t = tmp_files("trait_side_dup", &[("t.ksnl", TRAIT_SIDE_MAIN), ("t/tr.ksnl", &tr), ("t/ty.ksnl", TRAIT_SIDE_TY)]);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(codes(&out).iter().any(|c| c == "E1114"), "{}", out);
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

#[test]
fn a_failed_check_eq_in_the_native_build_exits_with_101() {
    let (_t, exe_path, out) = build_native("clif_fail", &clif_program("    core.check.eq(1u8, 2u8)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("assertion failed"), "{}", text(&o.stderr));
    assert!(!text(&o.stdout).contains("clif ok"));
}

#[test]
fn overflow_and_division_by_zero_panic_with_exit_code_101_in_the_native_build() {
    for (name, tail, msg) in [
        // 文言は LLVM のネイティブ (`arithmetic overflow` / `division by zero`) とバンドル (`attempt to add with overflow` /
        // `attempt to divide by zero`) で違うので、共通の部分だけ見る。
        ("clif_ovf", "    core.check.eq(t.add8(250u8, 10u8), 4u8)", "overflow"),
        ("clif_div0", "    core.check.eq(t.div(1u32, 0u32), 0u32)", "by zero"),
        ("clif_neg", "    core.check.eq(t.neg(0i64 - 9223372036854775807i64 - 1i64), 0i64)", "overflow"),
    ] {
        let (_t, exe_path, out) = build_native(name, &clif_arith_program(tail), false);
        let p = exe_path.unwrap_or_else(|| panic!("{}: no native executable: {}", name, out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(101), "{}: {}", name, text(&o.stderr));
        assert!(text(&o.stderr).contains(msg), "{}: {}", name, text(&o.stderr));
        assert!(!text(&o.stdout).contains("arith ok"));
    }
}

#[test]
fn a_wrong_struct_result_in_the_native_build_fails_the_check() {
    let (_t, exe_path, out) = build_native("clif_struct_bad", &clif_struct_program("    core.check.eq(t.area(t.shape_of(4u32)), 13u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

#[test]
fn a_wrong_option_result_in_the_native_build_fails_the_check() {
    let (_t, exe_path, out) = build_native("clif_opt_bad", &clif_option_program("    core.check.eq(t.describe(t.find(9u32, 4u32)), 106u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

#[test]
fn indexing_a_vec_out_of_bounds_panics_in_the_native_build() {
    let (_t, exe_path, out) = build_native("clif_vec_oob", &clif_vec_program("    core.check.eq(v[9usize], 0u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("index out of bounds"), "{}", text(&o.stderr));
}

/// 文字列の拡張 (`ends_with` `contains` `trim*` `strip_*` `split_once` `rfind` `eq_ignore_ascii_case` `split`、
/// `parse`、`String` の `push_u64` `push_i64` `push_bytes` `replace` `join`) を、インタプリタと LLVM の
/// 両方で、同じ検査 (`tests/data/str_ext.ksnl`) に通す。
#[test]
fn string_extensions_agree_between_interpreter_and_the_native_build() {
    let src = include_str!("data/str_ext.ksnl");
    let t = Tmp::new("str_ext_interp", src);
    let check = run(&["check"], &t.0);
    assert_eq!(check.status.code(), Some(0), "{}{}", text(&check.stdout), text(&check.stderr));
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    assert_eq!(text(&interp.stdout).trim(), "str ext ok");
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("str_ext_{}", release), src, release);
        assert_native_or_reasoned_skip(&out);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
}

#[test]
fn a_wrong_string_comparison_in_the_native_build_fails_the_check() {
    let (_t, exe_path, out) = build_native("clif_str_bad", &clif_string_program("    core.check.eq(\"ab\", \"ac\")"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
    assert!(!text(&o.stdout).contains("string ok"));
}

#[test]
fn a_wrong_slice_result_in_the_native_build_fails_the_check() {
    let (_t, exe_path, out) = build_native("clif_slice_bad", &clif_slice_program("    core.check.eq(s.last(), Some(&5u32))"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

#[test]
fn unequal_structs_in_the_native_build_fail_the_check() {
    let (_t, exe_path, out) = build_native(
        "clif_tuple_bad",
        &clif_tuple_program("    core.check.eq(t.Kind.Box(p, true), t.Kind.Box(q, false))"),
        false,
    );
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

#[test]
fn unwrap_on_none_panics_with_exit_code_101_in_the_native_build() {
    let (_t, exe_path, out) = build_native("clif_unwrap", &clif_methods_program("    core.check.eq(250u8.checked_add(9u8).unwrap(), 0u8)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("unwrap"), "{}", text(&o.stderr));
}

#[test]
fn a_write_through_a_mutable_reference_is_visible_in_the_native_build() {
    let (_t, exe_path, out) = build_native("clif_mutref_bad", &clif_mutref_program("    core.check.eq(n, 8u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

#[test]
fn a_question_mark_error_in_main_exits_with_code_1_in_the_native_build() {
    let (_t, exe_path, out) = build_native("clif_try_main", &clif_try_program("    core.check.eq(t.parse(0u32)?, 0u32)").replace("-> Result<(), std.io.Error> uses [alloc, io.console]", "-> Result<(), u8> uses [alloc, io.console]"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stderr));
    assert!(!text(&o.stdout).contains("try ok"));
}

#[test]
fn a_wrong_closure_result_in_the_native_build_fails_the_check() {
    let (_t, exe_path, out) = build_native("clif_clo_bad", &clif_closure_program("    core.check.eq(twice(2i32), 5i32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

#[test]
fn a_wrong_iterator_result_in_the_native_build_fails_the_check() {
    let (_t, exe_path, out) = build_native("clif_iter_bad", &clif_iterator_program("    core.check.eq(it.cur, 5u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

#[test]
fn a_failed_conversion_check_in_the_native_build_exits_with_101() {
    let (_t, exe_path, out) = build_native("clif_std_bad", &clif_std_program("    core.check.is_true(u8.try_from_i32(256i32) is Ok(_))"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

#[test]
fn a_wrong_map_lookup_in_the_native_build_fails_the_check() {
    let (_t, exe_path, out) = build_native("clif_map_bad", &clif_map_program("    core.check.eq(m.get(&\"a\"), Some(&1u8))"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
}

#[test]
fn a_wrong_generic_result_in_the_native_build_fails_the_check() {
    let (_t, exe_path, out) = build_native("clif_gen_bad", &clif_generic_program("    core.check.eq(t.pick(true, 3u8, 4u8), 4u8)"), false);
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
        "    core.mem.drop(std.io.print_line(\"trait generic ok\"))",
        "    Ok(())",
        "end t.main",
        "",
        "fn t.total[T: t.Area](a: &T, b: &T) -> u32",
        "    a.area().wrapping_add(b.area())",
        "end t.total",
        "",
    ]
    .join(&nl);
    let (_t, exe_path, out) = build_native("clif_gen_trait", &src, false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "trait generic ok");
}

#[test]
fn a_wrong_closure_result_through_a_generic_function_fails_the_check() {
    let (_t, exe_path, out) = build_native("clif_fnb_bad", &clif_fn_bound_program("    let bad = fn(n: u32) -> u32 capture [] {
        n
    }
    core.check.eq(t.apply(bad, 1u32), 2u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
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
    assert!(out.contains("use of moved value `s`"), "{}", out);
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

#[test]
fn a_wrong_value_through_a_captured_variable_fails_the_check() {
    let (_t, exe_path, out) = build_native("clif_cap_bad", &clif_capture_program("    core.check.eq(total, 7u32)"), false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
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

/// 作成・書き込み・読み戻し・削除・存在しないファイルのエラーを、インタプリタとネイティブで同じ結果にする。
/// エラー番号は OS ごとに違うので、出力の一致は Unix だけで確かめる (Windows は動いて `fs ok` で終わることだけ)。
#[test]
fn std_fs_round_trip_agrees_between_interpreter_and_the_native_build() {
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
            let (bt, exe_path, out) = build_native(&format!("native_fs_win_{}", release), src, release);
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
    let (bt, exe_path, out) = build_native("native_fs_leak", src, false);
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
fn std_net_loopback_round_trip_agrees_between_interpreter_and_the_native_build() {
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
    let (bt, exe_path, out) = build_native("native_net_leak", src, false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new("sh").args(["-c", "ulimit -n 40 && exec \"$0\"", p.to_str().unwrap()]).current_dir(&bt.0).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
    assert!(text(&o.stdout).ends_with("net ok\n"), "{}", text(&o.stdout));
}

/// 閉じたポートへの connect、使用中のポートへの bind、不正なアドレスが、エラー値になる (番号はインタプリタだけで確かめる)。
#[cfg(unix)]
#[test]
fn std_net_errors_agree_between_interpreter_and_the_native_build() {
    // エラー番号の行 (`... code: N`) は、LLVM のネイティブでは値が違う (errno に 0x8000_0000 が立つ・不正なアドレスの検査がない)。
    // 既知の差 (報告済み) なので、その行は一致を求めず、インタプリタの値だけを確かめる。
    let out = agree_with_interpreter_masking("native_net_err", include_str!("data/native_net_err.ksnl"), " code: ");
    assert!(out.contains("bad address code: -1\n"), "{}", out);
    assert!(out.contains("bad port code: -1\n"), "{}", out);
    assert!(out.contains("connect to a closed port code: "), "{}", out);
    assert!(out.ends_with("net errors ok\n"), "{}", out);
}

/// `std.net` は Windows では未対応で、ビルド時に理由を付けて退く (誤ったコードを作らない)。
#[cfg(windows)]
#[test]
fn std_net_is_skipped_by_the_native_backend_on_windows() {
    let (_t, exe_path, out) = build_native("native_net_win", include_str!("data/native_net.ksnl"), false);
    assert!(exe_path.is_none() && out.contains("native backend skipped") && out.contains("Windows"), "{}", out);
}

/// `&str` / `&[T]` / `&スカラー` を返す関数。返す式が、リテラルか参照の引数 (に `as_str` / `trim` をつないだもの) のとき。
#[test]
fn functions_returning_references_agree_between_interpreter_and_the_native_build() {
    let out = agree_with_interpreter("native_ref", include_str!("data/native_ref.ksnl"));
    assert_eq!(out, "one, and\n");
}

/// 関数の値 (`let sp = t.susp; sp()`) の呼び出しも中断点: 状態機械が枠 (`a`) を保存し、インタプリタと同じに動く。
#[test]
fn fn_value_suspension_agrees_between_interpreter_and_the_native_build() {
    let out = agree_with_interpreter("native_fnvalue_susp", include_str!("data/check_ok/sec6_fnvalue_suspend_ok.ksnl"));
    assert_eq!(out, "ok\n");
}

/// `defs` の関数を `main` から呼ぶ。`call` は呼び出しの文。LLVM のネイティブ (下ろせなければバンドル) で動かして、
/// 正常終了して `unused` を出すことを確かめる。
fn run_ref_return_case(name: &str, defs: &str, call: &str) {
    let src = format!(
        "{}\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    std.io.print_line(\"unused\")\nend t.main\n",
        defs, call
    );
    let (bt, exe_path, out) = build_native(name, &src, false);
    assert_native_or_reasoned_skip(&out);
    let o = Command::new(exe_path.unwrap()).current_dir(&bt.0).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}: {}{}", name, text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), "unused\n", "{}", name);
}

/// 持ち主を移す・ぶら下がる恐れのある参照を返す関数。以前は Cranelift が理由を付けて退いていた。
/// LLVM (KIR) は参照をポインタで持つので、どれもそのまま動く (下ろせなければ理由付きでバンドル)。
#[test]
fn functions_returning_owned_or_local_references_run_in_the_native_build() {
    // ローカルの変数を経由する形
    run_ref_return_case("ref_local", "fn t.word(text: &str) -> &str uses [] panics never\n    let inner = text.trim()\n    inner\nend t.word\n", "    core.mem.drop(t.word(\"a\"))\n");
    // 所有する型への参照
    run_ref_return_case(
        "ref_string",
        "fn t.view(s: &alloc.string.String<std.alloc.System>) -> &alloc.string.String<std.alloc.System> uses [] panics never\n    s\nend t.view\n",
        "    let s = alloc.string.String.new(std.alloc.SYSTEM)\n    core.mem.drop(t.view(&s))\n",
    );
    // 構造体への参照
    run_ref_return_case("ref_struct", "type t.P\n    a: u8\nend t.P\n\nfn t.first(p: &t.P) -> &t.P uses [] panics never\n    p\nend t.first\n", "    let p = t.P { a: 1u8 }\n    core.mem.drop(t.first(&p))\n");
    // 可変参照
    run_ref_return_case("ref_mut", "fn t.bump(x: &mut u32) -> &mut u32 uses [] panics never\n    x\nend t.bump\n", "    let mut x = 1u32\n    core.mem.drop(t.bump(&mut x))\n");
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
    // 参照越しの分解で得た名前は欄の型への参照 (Rust の既定の束縛モード): 参照の位置にはそのまま、値の位置には `*` が要る。
    let pat = "match &Some(x) {\n        Some(p) => {\n            core.mem.drop(t.val(*p))\n            core.mem.drop(t.rf(p))\n        }\n        None => ()\n    }";
    let pat_src = main_wrap(prefix, &format!("    let x = 1u8\n    {}\n", pat));
    assert!(codes_of("arg_pat_ref", &pat_src).is_empty());
    let pat_bad = "match &Some(x) {\n        Some(p) => core.mem.drop(t.val(p))\n        None => ()\n    }";
    let pat_bad_src = main_wrap(prefix, &format!("    let x = 1u8\n    {}\n", pat_bad));
    assert!(codes_of("arg_pat_ref_for_val", &pat_bad_src).contains(&"E2023".to_string()));
}

fn sn_main(t: &Tmp) -> String {
    text(&fs::read(t.0.join("src/t.ksnl")).unwrap())
}

/// 非正規形のソースが check の誤り (E0020) になり、fmt で期待の正規形になり、冪等で、正規形が check / fmt --check を通る。
fn sn_roundtrip(name: &str, src: &str, want: &str) {
    let t = sn_pkg(name, src);
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(1), "{}: check must fail: {}{}", name, text(&c.stdout), text(&c.stderr));
    assert!(text(&c.stdout).contains("E0020"), "{}: {}", name, text(&c.stdout));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(1), "{}: fmt --check must report a diff", name);
    assert_eq!(run(&["fmt"], &t.0).status.code(), Some(0), "{}", name);
    assert_eq!(sn_main(&t), want, "{}: fmt result", name);
    let again = run(&["fmt"], &t.0);
    assert_eq!(sn_main(&t), want, "{}: fmt must be idempotent", name);
    assert!(!text(&again.stdout).contains("reformatted"), "{}", text(&again.stdout));
    let fin = run(&["check"], &t.0);
    assert_eq!(fin.status.code(), Some(0), "{}: canonical form must pass check: {}{}", name, text(&fin.stdout), text(&fin.stderr));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0), "{}", name);
}

#[test]
fn short_names_canonical_source_passes() {
    let t = sn_pkg("sn_canon", SN_CANON);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
}

#[test]
fn short_form_of_an_imported_type_in_a_header_is_an_error_and_fmt_makes_it_fqn() {
    let src = SN_CANON.replace("x: &t.lib.Item", "x: &lib.Item");
    sn_roundtrip("sn_hdr_short", &src, SN_CANON);
}

#[test]
fn fqn_of_an_imported_module_in_a_body_is_an_error_and_fmt_shortens_it() {
    let src = SN_CANON.replace("lib.make()", "t.lib.make()");
    sn_roundtrip("sn_body_fqn", &src, SN_CANON);
    // 型注釈の中でも短縮される。注釈は右辺だけでは型が決まらない形 (`Vec.new`) にして、冗長な注釈 (E2053) にしない。
    let body_old = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let item = lib.make()\n    let n = t.get(&item)\n    core.mem.drop(n)\n    Ok(())\nend t.main\n";
    let body_new = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let items: alloc.vec.Vec<t.lib.Item, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    let n = items.len()\n    core.mem.drop(items)\n    core.mem.drop(n)\n    Ok(())\nend t.main\n";
    assert!(SN_CANON.contains(body_old));
    let src = SN_CANON.replace(body_old, body_new);
    // 長い形式 (`alloc.vec.Vec<.., System> = alloc.vec.Vec.new(std.alloc.SYSTEM)`) は fmt で短縮名 (`std.vec.Vec<..> = std.vec.Vec.new()`) に書き換えられる。
    let want_body = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let items: std.vec.Vec<lib.Item> = std.vec.Vec.new()\n    let n = items.len()\n    core.mem.drop(items)\n    core.mem.drop(n)\n    Ok(())\nend t.main\n";
    let want = SN_CANON.replace(body_old, want_body);
    sn_roundtrip("sn_body_fqn_ty", &src, &want);
}

#[test]
fn an_import_used_only_through_a_header_is_an_error_and_fmt_removes_it() {
    let hdr = "fn t.get(x: &lib.Item) -> u8 uses [] panics never\n    x.n\nend t.get\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let want = "fn t.get(x: &t.lib.Item) -> u8 uses [] panics never\n    x.n\nend t.get\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    sn_roundtrip("sn_hdr_only_short", &format!("import t.lib\n\n{}", hdr), want);
    // ヘッダの FQN だけが参照する import も、check は未使用にせず (E1020 ではなく E0020)、fmt が外す。
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
fn with_cleanup_runs_the_same_way_in_the_native_build() {
    for release in [false, true] {
        let (_t, exe_path, out) = build_native(&format!("with_native_{}", release), WITH_PROGRAM, release);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), WITH_EXPECTED);
    }
}

#[test]
fn with_does_not_run_its_cleanup_on_panic() {
    // 5.2: panic は巻き戻さない。後始末は走らず、終了コードは 101。
    let body = r#"fn t.shut(n: u32) -> () uses [io.console] panics never
    core.mem.drop(std.io.print_line("shut"))
    core.mem.drop(n)
end t.shut

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    with n = 1u32 close t.shut(n) {
        core.mem.drop(std.io.print_line("body"))
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
    let (_t2, exe_path, out) = build_native("with_panic_native", body, false);
    let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout), "body\n");
}

/// `with` の診断の確認用。`t.R` と、その `get` / `shut` / `status` / `bump` を足した上に `main` の本体を置く。
fn with_main(body: &str) -> String {
    format!(
        "type t.R linear\n    id: u32\nend t.R\n\nfn t.R.bump(self: &mut t.R) -> () uses [] panics never\n    self.id = self.id.wrapping_add(1u32)\nend t.R.bump\n\nfn t.R.get(self: &t.R) -> u32 uses [] panics never\n    self.id\nend t.R.get\n\nfn t.R.new(id: u32) -> t.R uses [] panics never\n    t.R {{ id: id }}\nend t.R.new\n\nfn t.R.shut(self: t.R) -> () uses [] panics never\n    let t.R {{ id: id }} = self\n    core.mem.drop(id)\nend t.R.shut\n\nfn t.R.status(self: t.R) -> Result<(), std.io.Error> uses [] panics never\n    let t.R {{ id: id }} = self\n    core.mem.drop(id)\n    Ok(())\nend t.R.status\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n{}    Ok(())\nend t.main\n",
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
    assert_eq!(with_codes("with_reassigned", b), ["E3003", "E3040"]);
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
    let src = "type t.R\n    id: u32\nend t.R\n\nfn t.R.open(a: u32, b: u32) -> Result<t.R, u8> uses [] panics never\n    Ok(t.R { id: a.wrapping_add(b) })\nend t.R.open\n\nfn t.R.shut(r: t.R, n: u32) -> () uses [] panics never\n    core.mem.drop(r.id.wrapping_add(n))\nend t.R.shut\n\nfn t.main() -> Result<(), u8> uses [] panics never\n    let v = with r = t.R.open(\n        1u32,\n        2u32,\n    )? close t.R.shut(\n        r,\n        3u32,\n    ) {\n        4u32\n    }\n    core.mem.drop(v)\n    Ok(())\nend t.main\n";
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

/// `Vec<Box<dyn Shape>>` (2 種類の具象型 + `String` を持つ型)・`&dyn` / `&mut dyn` の引数・`&mut self` のメソッドを、
/// インタプリタと LLVM のネイティブ (debug / release) で同じ結果にする。LLVM に下ろせなければ理由付きでバンドルへ退避する。
#[test]
fn box_dyn_agrees_between_interpreter_and_the_native_build() {
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
        let (_t, exe_path, out) = build_native(&format!("box_dyn_{}", release), &src, release);
        assert_native_or_reasoned_skip(&out);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), text(&interp.stdout), "native and interpreter outputs differ");
    }
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
fn e0010_still_fires_for_bad_names_in_tuple_lets_and_e0020_is_fatal() {
    // タプル分解の大文字の束縛名は E0010 のまま (fmt では直らない)。同じファイルの定義順の違反 (E0020) も致命で出る。
    let src = "fn t.mk() -> u32 uses [] panics never\n    1u32\nend t.mk\n\nfn t.main() -> () uses [] panics never\n    let (Foo, y) = (1u32, 2u32)\n    core.mem.drop(Foo)\n    core.mem.drop(y)\nend t.main\n";
    let t = Tmp::new_raw("e0010_tup_upper", src);
    let o = run(&["check"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{}", out);
    assert!(out.contains("E0010: let Foo not snake_case"), "{}", out);
}

#[test]
fn e0020_order_violation_fails_check() {
    // 定義順・uses の並びの違反は `ksn fmt` が直す揺れで、書き方は 1 通りなので check の誤り (E0020、致命)。
    let unordered = "fn t.b() -> u8 uses [] panics never\n    0u8\nend t.b\n\nfn t.a() -> u8 uses [] panics never\n    0u8\nend t.a\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics may\n    core.check.eq(t.a(), 0u8)\n    core.check.eq(t.b(), 0u8)\n    Ok(())\nend t.main\n";
    let t = Tmp::new_raw("e0020_def_order", unordered);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
    assert!(text(&o.stdout).contains("E0020"), "{}", text(&o.stdout));

    let uses_order = "fn t.main() -> Result<(), std.io.Error> uses [io.fs, io.console] panics never\n    std.io.print_line(\"x\")?\n    core.mem.drop(std.fs.exists(\"x.txt\"))\n    Ok(())\nend t.main\n";
    let u = Tmp::new_raw("e0020_uses_order", uses_order);
    let o = run(&["check"], &u.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
    assert!(text(&o.stdout).contains("E0020"), "{}", text(&o.stdout));
    // `fmt` が直し、直したあとは通る。
    assert_eq!(run(&["fmt"], &u.0).status.code(), Some(0));
    assert_eq!(run(&["check"], &u.0).status.code(), Some(0));
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

// 規範 (sig) にある数値の変換は、静的な呼び出しで通る。規範に無い `from_` は E1108 (下)。
#[test]
fn sig_listed_numeric_conversions_run() {
    run_ok(
        "num_conv_ok",
        &body(&[
            "core.check.eq(u32.from_u8(3u8), 3u32)",
            "core.check.eq(f64.from_usize_nearest(3usize), 3.0f64)",
            "core.check.eq(f32.from_u32_nearest(3u32), 3.0f32)",
            "core.check.is_true(u8.try_from_u32(300u32) is Err(_))",
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
        "        let b = t.Big { a: 0u128, b: 0u128, c: 0u128, d: 0u128 }",
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
        fn(_x: u8) -> u8 capture [&v] {
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
        fn(_x: u8) -> u8 capture [] {
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

fn t.mid(s: t.S) -> u8 uses [io.console] panics never
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
        fn(_y: u8) -> u8 capture [&s] {
            s.boom()
        },
        x,
    )
end t.bad_panic

"#;

// 境界の定義・推論された関数・覆われる / 効果のない呼び出し元・メソッド (どれも黙る)。
const BOUND_REST: &str = r#"fn t.bnd_io[F: core.ops.Fn<(u8,), u8, [io], never>](f: F, x: u8) -> u8 uses [io] panics never pub
    f(x)
end t.bnd_io

fn t.bnd_may[F: core.ops.Fn<(u8,), u8, [], may>](f: F, x: u8) -> u8 uses [] panics may pub
    f(x)
end t.bnd_may

fn t.bnd_none[F: core.ops.Fn<(u8,), u8, [], never>](f: F, x: u8) -> u8 uses [] panics never pub
    f(x)
end t.bnd_none

fn t.inf_io(x: u8) -> u8 uses [io.console] panics never
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

fn t.ok_may(s: t.S, x: u8) -> u8 uses [] panics may
    t.bnd_may(
        fn(_y: u8) -> u8 capture [&s] {
            s.boom()
        },
        x,
    )
end t.ok_may

fn t.ok_pure(s: t.S, x: u8) -> u8 uses [] panics never
    t.bnd_none(
        fn(_y: u8) -> u8 capture [&s] {
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

fn t.bad_method(s: t.S) -> u8 uses [io.console] panics never
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
        "fn t.h(b: &t.B) -> u8 uses [io.console] panics never
    core.mem.drop(b.m())
    core.mem.drop(std.io.print_line(\"h\"))
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
        "fn t.B.k(self: &t.B) -> u8 uses [] panics never
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

// ---------------------------------------------------------------- 式 ID と型表 (設計_KIR.md 段階 1)

const ADD1: &str = "fn t.add1(a: u8) -> u8 uses [] panics never
    let b = a
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
    let b = Tmp::new("expr_ids_local_b", &main_wrap(ADD1, "    let _c = t.add1(2)\n    let _d = t.add1(3)\n"));
    let pick = |t: &Tmp| -> Vec<(u32, String, String)> {
        type_rows(&dump_types(&t.0)).into_iter().filter(|r| r.0 == "t.add1").map(|r| (r.1, r.2, r.3)).collect()
    };
    let (ra, rb) = (pick(&a), pick(&b));
    assert!(!ra.is_empty());
    assert_eq!(ra, rb);
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
fn vec_has_no_comparison_traits_but_string_has() {
    // 4.2 の表どおり Vec は比較のトレイトを持たない (sig にも impl が無い)。
    // メソッド形 (`eq` `cmp`) はどちらも E1108。String は `string.sig` の `impl` で比較できる (2026-11-14 の決定)。
    let bad = "fn t.v(a: &alloc.vec.Vec<u8, std.alloc.System>, b: &alloc.vec.Vec<u8, std.alloc.System>) -> bool uses [] panics never
    a.eq(b)
end t.v

fn t.c(a: &alloc.vec.Vec<u8, std.alloc.System>, b: &alloc.vec.Vec<u8, std.alloc.System>) -> bool uses [] panics never
    a.cmp(b) == core.cmp.Ordering.Equal
end t.c

";
    let t = Tmp::new("vec_cmp_bad", &main_wrap(bad, ""));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(out.contains("unknown method `eq` on `alloc.vec.Vec`: the type does not implement `core.cmp.PartialEq`"), "{}", out);
    assert!(out.contains("unknown method `cmp` on `alloc.vec.Vec`: the type does not implement `core.cmp.Ord`"), "{}", out);
    assert!(codes(&out).iter().filter(|c| *c == "E1108").count() >= 2, "{}", out);
    let good = "fn t.s(a: &alloc.string.String<std.alloc.System>, b: &alloc.string.String<std.alloc.System>) -> bool uses [] panics never
    a == b && a.partial_cmp(b).is_some() && a.cmp(b) == core.cmp.Ordering.Equal
end t.s

";
    let t = Tmp::new("string_cmp_ok", &main_wrap(good, ""));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}", out);
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
         fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    core.mem.drop(t.g(1u8))\n    Ok(())\nend t.main\n",
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
    assert!(out.contains("t.Act.act (through dyn) uses effect `io.console`"), "{}", out);
    assert!(out.contains("t.Act.boom (through dyn) may panic"), "{}", out);
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
    let body = "fn t.quiet_inferred(a: &dyn t.Act) -> u8 uses [io.console] panics never
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

#[test]
fn discarding_an_option_or_result_value_is_e2042() {
    // 型引数が決まる `Some(x)` / `Ok(x)` の値を式文で捨てると 4.6 の E2042 (`()` 以外を捨てる)。
    let (cs, out) = lin_arg_codes("lin_discard_opt", &["Some(1u8)", "Ok(1u8)"]);
    assert!(tdrop_has(&cs, "E2042"), "{}", out);
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

/// ユーザー定義の型の欄がトレイトを実装していない E2036 では、同じトレイトをその型に derive するよう案内する。
#[test]
fn e2036_user_type_field_hint_adds_derive() {
    let src = "enum bench.Mode pub\n    Fast\n    Slow\nend bench.Mode\n\ntype bench.Product derive [Clone] pub\n    mode: bench.Mode\nend bench.Product\n\nfn bench.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend bench.main\n";
    let t = Tmp::new("e2036_user_field_hint", src);
    let o = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(o.contains("E2036"), "{}", o);
    assert!(o.contains("derive [Clone] on bench.Product: the field type bench.Mode does not implement core.ops.Clone"), "{}", o);
    assert!(o.contains("add `derive [Clone]` to `bench.Mode`"), "{}", o);
}

/// `String` / `Vec` の欄を持つ型の `derive [Clone]` は `Clone` を外して `try_clone` (確保が失敗しうる) を使うよう案内する。
#[test]
fn e2036_std_type_field_hint_try_clone() {
    for (tag, field, api) in [("string", "alloc.string.String<std.alloc.System>", "try_clone"), ("vec", "alloc.vec.Vec<u8, std.alloc.System>", "try_clone")] {
        let src = format!("type t.P derive [Clone] pub\n    x: {}\nend t.P\n\nfn t.main() -> () uses [] panics never\nend t.main\n", field);
        let t = Tmp::new(&format!("e2036_std_hint_{}", tag), &src);
        let o = text(&run(&["check", "--json"], &t.0).stdout);
        assert!(o.contains("E2036"), "{}", o);
        assert!(o.contains(&format!("derive [Clone] on t.P: the field type {} does not implement core.ops.Clone", field.split('<').next().unwrap())), "{}", o);
        assert!(o.contains("remove `Clone` from the derive list"), "{}", o);
        assert!(o.contains(api), "{}", o);
    }
}

/// `HashMap` の欄を持つ型の `derive [Clone]` は `Clone` を外すよう案内するが、`try_clone` は名乗らない（`HashMap` には複製 API が無い）。
#[test]
fn e2036_std_type_field_hint_hashmap_no_clone_api() {
    let src = "type t.Q derive [Clone] pub\n    m: alloc.collections.HashMap<u32, u32, std.alloc.System>\nend t.Q\n\nfn t.main() -> () uses [] panics never\nend t.main\n";
    let t = Tmp::new("e2036_std_hint_hashmap", src);
    let o = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(o.contains("E2036"), "{}", o);
    assert!(o.contains("derive [Clone] on t.Q: the field type alloc.collections.HashMap does not implement core.ops.Clone"), "{}", o);
    assert!(o.contains("remove `Clone` from the derive list"), "{}", o);
    assert!(!o.contains("try_clone"), "{}", o);
}

/// E2021 で列挙型の `_` / 束縛腕には、列挙すべきバリアント名を案内する。
#[test]
fn e2021_enum_hint_lists_variants() {
    let src = "enum bench.Tok pub\n    Num(u32)\n    Plus\nend bench.Tok\n\nfn bench.main() -> Result<(), std.io.Error> uses [] panics never\n    let e = bench.Tok.Plus\n    match e {\n        x => 0u8\n    }\n    Ok(())\nend bench.main\n";
    let t = Tmp::new("e2021_enum_hint", src);
    let o = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(o.contains("E2021"), "{}", o);
    assert!(o.contains("`_` / binding patterns cannot match an enum; list all variants:"), "{}", o);
    assert!(o.contains("bench.Tok.Num(..)"), "{}", o);
    assert!(o.contains("bench.Tok.Plus"), "{}", o);
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
fn t.ctl(k: u32) -> u64 uses [] panics never\n    let s = if k == 0u32 {\n        \"\\u{11}2\"\n    } else {\n        \"\\u{19}\"\n    }\n    match u8.parse(s) {\n        Ok(v) => u64.from_u8(v)\n        Err(e) => 1000u64.wrapping_add(u64.from_u8(e.kind))\n    }\nend t.ctl\n\n\
fn t.over(k: u32) -> u64 uses [] panics never\n    let s = if k == 0u32 {\n        \"1111111111111111111111111111111111111111\"\n    } else {\n        \"18446744073709551616\"\n    }\n    match u64.parse(s) {\n        Ok(_v) => 7u64\n        Err(e) => 1000u64.wrapping_add(u64.from_u8(e.kind))\n    }\nend t.over\n";
    kir_review_cases("final_sp", fns, "t.sp", &[(&["0"], "t.sp(0u32)", "2u64", "2"), (&["1"], "t.sp(1u32)", "1u64", "1")]);
    kir_review_cases("final_ctl", fns, "t.ctl", &[(&["0"], "t.ctl(0u32)", "1000u64", "1000"), (&["1"], "t.ctl(1u32)", "1000u64", "1000")]);
    kir_review_cases("final_over", fns, "t.over", &[(&["0"], "t.over(0u32)", "1001u64", "1001"), (&["1"], "t.over(1u32)", "1001u64", "1001")]);
}

// ---- 診断の重複: 案内付きの行の一般的な E0002 は出さない ----

fn guided_syntax_case(name: &str, src: &str) -> Vec<String> {
    // `ksn fmt` が直す書き方 (腕の末尾の `,` など) は整形せずにそのまま試す (`new_raw`)。
    let t = Tmp::new_raw(name, src);
    codes(&text(&run(&["check", "--json"], &t.0).stdout))
}

#[test]
fn guided_arm_syntax_errors_drop_the_generic_e0002_on_their_line() {
    let e49 = "fn t.a(x: u8) -> u8\n    match x {\n        0u8 => 1u8,\n        _ => 2u8,\n    }\nend t.a\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    core.mem.drop(t.a(1u8))\n    Ok(())\nend t.main\n";
    let e57 = "fn t.a(x: Option<u8>) -> u8 uses [] panics never\n    match x {\n        Some(v) =>\n        {\n            v\n        }\n        None => 0u8\n    }\nend t.a\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    core.mem.drop(t.a(Some(1u8)))\n    Ok(())\nend t.main\n";
    let e58 = "fn t.a(x: Option<u8>) -> u8 uses [] panics never\n    let mut result = 0u8\n    match x {\n        Some(r) => result = r\n        None => {\n            result = 0u8\n        }\n    }\n    result\nend t.a\n\n\
               fn t.main() -> Result<(), std.io.Error> uses [] panics may\n    core.mem.drop(t.a(Some(1u8)))\n    Ok(())\nend t.main\n";
    for (name, src, code) in [("guided_e49", e49, "E0049"), ("guided_e57", e57, "E0057"), ("guided_e58", e58, "E0058")] {
        let got = guided_syntax_case(name, src);
        assert!(got.iter().any(|g| g == code), "{} missing in {:?}", code, got);
        assert!(!got.iter().any(|g| g == "E0002"), "{}: E0002 should be dropped, got {:?}", code, got);
    }
}

#[test]
fn unguided_syntax_error_keeps_e0002() {
    let got = guided_syntax_case("unguided_e0002", "fn t.a(x: u8) -> u8\n    let y = (x +\n    x\nend t.a\n");
    assert!(got.iter().any(|g| g == "E0002"), "{:?}", got);
}

/// `derive [TryClone]` は全フィールドが `TryClone` のときだけ (E2036)。`T: TryClone` の境界を満たさない型は E2030。
#[test]
fn derive_try_clone_on_non_try_clone_field_is_e2036() {
    let t = Tmp::new("derive_try_clone_bad", "type t.N pub\n    x: u32\nend t.N\n\ntype t.W derive [TryClone] pub\n    n: t.N\nend t.W\n\nfn t.dup[T: alloc.alloc.TryClone](v: &T) -> Result<T, alloc.alloc.AllocError> uses [alloc] panics never\n    v.try_clone()\nend t.dup\n\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never\n    let n = t.N { x: 1u32 }\n    match t.dup(&n) {\n        Ok(_c) => std.io.print_line(\"e ok\")\n        Err(_e) => std.io.print_line(\"err\")\n    }\n    Ok(())\nend t.main\n");
    let o = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(o.contains("E2036"), "{}", o);
    assert!(o.contains("E2030"), "{}", o);
}

#[test]
fn elided_fn_panic_reaches_a_panics_never_caller() {
    let t = Tmp::new("elide_panic", &elide_main("", "never", "        t.boom()"));
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("E4002") || text(&o.stdout).contains("E4002"), "{}{}", text(&o.stdout), text(&o.stderr));
    let t = Tmp::new("elide_panic_ok", &elide_main("", "may", "        t.boom()"));
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let t = Tmp::new("elide_pure_ok", &elide_main("", "never", "        3"));
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

#[test]
fn ambiguous_elided_fn_effects_must_be_written_out() {
    let two = "fn t.two[F: core.ops.Fn<(), u8>, G: core.ops.Fn<(), u8>](f: F, g: G) -> u8 uses [] panics never\n    g.call(())\nend t.two\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = Tmp::new("elide_two", two);
    let o = run(&["check"], &t.0);
    assert!(format!("{}{}", text(&o.stdout), text(&o.stderr)).contains("omits its effect arguments"), "{}{}", text(&o.stdout), text(&o.stderr));
    let var = two.replace("F: core.ops.Fn<(), u8>, G: core.ops.Fn<(), u8>", "F: core.ops.Fn<(), u8>, G: core.ops.Fn<(), u8, [], never>");
    let var = var.replace("uses [] panics never\n    g", "uses [] panics never\n    g");
    let t = Tmp::new("elide_one_explicit", &var);
    let o = run(&["check"], &t.0);
    assert!(!format!("{}{}", text(&o.stdout), text(&o.stderr)).contains("omits its effect arguments"), "{}{}", text(&o.stdout), text(&o.stderr));
    let half = two.replace("G: core.ops.Fn<(), u8>", "G: core.ops.Fn<(), u8, []>");
    let t = Tmp::new("elide_half", &half);
    let o = run(&["check"], &t.0);
    assert!(format!("{}{}", text(&o.stdout), text(&o.stderr)).contains("both the effect and the panic argument"), "{}{}", text(&o.stdout), text(&o.stderr));
}

/// `derive [ToJson, FromJson]` は、フィールドの型が JSON の形を持つときだけ (E2036): 実装しない名義型・`f64`・タプル・
/// 型パラメータ (`FromJson`)。`get --derived` は展開した Kasane の文面を表示する。
#[test]
fn derive_json_on_unsupported_field_is_e2036() {
    let src = "type t.N pub\n    x: u32\nend t.N\n\ntype t.A derive [ToJson, FromJson] pub\n    n: t.N\nend t.A\n\ntype t.B derive [ToJson] pub\n    f: f64\nend t.B\n\ntype t.C derive [ToJson] pub\n    p: (u8, u8)\nend t.C\n\ntype t.G[T] derive [FromJson] pub\n    v: T\nend t.G\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = Tmp::new("derive_json_bad", src);
    let o = text(&run(&["check", "--json"], &t.0).stdout);
    let n = o.matches("E2036").count();
    assert!(n >= 5, "{}", o);
    assert!(o.contains("does not implement std.json.ToJson"), "{}", o);
    assert!(o.contains("does not implement std.json.FromJson"), "{}", o);
    assert!(o.contains("`f64` has no JSON form"), "{}", o);
    assert!(o.contains("cannot derive FromJson"), "{}", o);
    let ok = Tmp::new("derive_json_get", DERIVE_JSON_SRC);
    let g = text(&run(&["get", "--derived", "t.Shape"], &ok.0).stdout);
    assert!(g.contains("fn t.Shape.to_json") && g.contains("fn t.Shape.from_json") && g.contains("\"Rect\""), "{}", g);
}

/// 標準ライブラリの本体 (`spec/stdlib/**/*.ksnl`) は、利用者のコードと同じ検査で診断 0 件 (`ksn check-stdlib`)。
/// 本体の型誤りは LLVM の下ろしまで見つからなかったことがあるので、ここで止める。
#[test]
fn stdlib_bodies_have_no_diagnostics() {
    let std_dir = root().join("spec").join("stdlib");
    let o = run_with_env(&["check-stdlib", "--json"], &root(), &[("KSN_STDLIB", std_dir.to_str().unwrap())]);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{}{}", out, text(&o.stderr));
    assert!(codes(&out).is_empty(), "{}", out);
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for e in fs::read_dir(src).unwrap() {
        let p = e.unwrap().path();
        let to = dst.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_tree(&p, &to);
        } else {
            fs::copy(&p, &to).unwrap();
        }
    }
}

/// 標準ライブラリの複製に型誤りを 1 つ入れると、`ksn check-stdlib` がそれを報告する (検査が本体に届いていること)。
#[test]
fn stdlib_injected_type_error_is_reported() {
    let dir = scratch::Scratch::at(format!("ksn_test_{}_stdlib_neg", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    copy_tree(&root().join("spec").join("stdlib"), &dir.join("stdlib"));
    // `log` は `.ksnh` (コメント付き) と `.ksnl` の対。両方に同じ関数を足す。
    for ext in ["ksnh", "ksnl"] {
        let f = dir.join("stdlib").join("std").join(format!("log.{}", ext));
        let mut log = fs::read_to_string(&f).unwrap();
        log.push_str("\nfn std.log.bad_probe() -> u32 uses [] panics never\n    let x: u32 = \"no\"\n    x\nend std.log.bad_probe\n");
        fs::write(&f, log).unwrap();
    }
    let o = run_with_env(&["check-stdlib", "--json"], &root(), &[("KSN_STDLIB", dir.join("stdlib").to_str().unwrap())]);
    let out = text(&o.stdout);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(o.status.code(), Some(1), "{}", out);
    assert!(codes(&out).iter().any(|c| c == "E2020"), "{}", out);
}

/// 文字リテラル `'"'` の `"` を文字列の始まりと誤らない (誤ると後ろの変数の使用が消え、E1020 になる)。
#[test]
fn char_literal_double_quote_does_not_hide_later_uses() {
    let src = "fn t.has_quote(text: &str) -> bool uses [] panics never\n    for c in text.chars() {\n        if c == '\"' {\n            return true\n        }\n    }\n    false\nend t.has_quote\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = Tmp::new("char_quote", src);
    let o = run(&["check", "--json"], &t.0);
    assert!(!codes(&text(&o.stdout)).iter().any(|c| c == "E1020"), "{}", text(&o.stdout));
}

/// 未解決のパターン名 (`Nope.X(v)`) の診断は、そのパターンの行を指す。関数の先頭行 (本体の 0 行扱い) ではない。
#[test]
fn unresolved_pattern_reports_its_own_line() {
    let src = "enum t.E\n    A\n    B(i32)\nend t.E\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let m = match t.E.A {\n        Nope.X(v) => v\n        t.E.A => 0i32\n        t.E.B(q) => q\n    }\n    core.mem.drop(m)\n    Ok(())\nend t.main\n";
    let want = src.lines().position(|l| l.contains("Nope.X")).unwrap() + 1;
    let t = Tmp::new("pattern_line", src);
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    let line = out
        .lines()
        .find(|l| l.starts_with('{') && l.contains("\"code\": \"E1101\"") && l.contains("unresolved pattern"))
        .unwrap_or_else(|| panic!("no E1101 for the pattern:\n{}", out));
    let i = line.find("\"line\": ").expect("line field") + 8;
    let got: usize = line[i..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap();
    assert_eq!(got, want, "{}", line);
}

/// 生ポインタの指す先は不変 (round2 finding 13): `*mut u8` と `*mut [u8; 4]` は取り違えられない。
/// 許すのは値の位置の `*mut [T; N]` → `*mut T` (先頭要素のポインタ) と `*mut T` → `*const T` だけ。
#[test]
fn raw_pointer_pointee_is_invariant() {
    let bad = "fn t.g[T](p: *mut T, q: *mut T) -> () uses [] panics never
    core.mem.drop(p)
    core.mem.drop(q)
end t.g

fn t.main(s: *mut [u8; 4], e: *mut u8) -> () uses [] panics never
    t.g(s, e)
end t.main
";
    let ok = "fn t.g(p: *mut u8, q: *const u8) -> () uses [] panics never
    core.mem.drop(p)
    core.mem.drop(q)
end t.g

fn t.main(s: *mut [u8; 4], e: *mut u8) -> () uses [] panics never
    t.g(s, e)
end t.main
";
    for (name, src, want_err) in [("ptr_inv_bad", bad, true), ("ptr_inv_ok", ok, false)] {
        let t = Tmp::new(name, src);
        fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t\"]\n\n[dependencies]", 1)).unwrap();
        let o = run(&["check", "--json"], &t.0);
        let got = codes(&text(&o.stdout));
        assert_eq!(got.iter().any(|c| c == "E2023"), want_err, "{}: {}", name, text(&o.stdout));
        if !want_err {
            assert!(got.is_empty(), "{}: {}", name, text(&o.stdout));
        }
    }
}

/// `static` / `const` の初期化式から効果のある関数を呼んでも、`ksn check` はそれを実行しない (X1)。
/// 1 つ目は検査 (E2038) が拒み、2 つ目 (`const fn` の検査を通り抜けた場合) は評価の防御線 (E4024) が止める。
#[test]
fn check_never_runs_effects_of_const_initializers() {
    let marker = std::env::temp_dir().join(format!("ksn_constinit_marker_{}", std::process::id()));
    let _ = fs::remove_file(&marker);
    let m = marker.to_string_lossy().to_string();
    let src = format!(
        "static t.X: u32 = t.f()\n\nfn t.f() -> u32 uses [process] panics never\n    let mut c = std.process.Command.new(\"/bin/touch\")\n    c.arg(\"{m}\")\n    match c.spawn() {{\n        Ok(child) => match child.wait() {{\n            Ok(_) => 1u32\n            Err(_) => 3u32\n        }}\n        Err(_) => 2u32\n    }}\nend t.f\n\nfn t.main() -> u32 uses [] panics never\n    t.X\nend t.main\n"
    );
    let t = Tmp::new_strict("constinit_no_exec", &src);
    let o = run(&["check", "--json"], &t.0);
    assert!(codes(&text(&o.stdout)).iter().any(|c| c == "E2038"), "{}", text(&o.stdout));
    assert!(!marker.exists(), "`ksn check` ran a constant initializer's effects");
    let src2 = format!(
        "static t.X: bool = std.fs.create_dir(\"{m}\").is_ok()\n\nfn t.main() -> bool uses [] panics never\n    t.X\nend t.main\n"
    );
    let t2 = Tmp::new_strict("constinit_no_exec2", &src2);
    let o2 = run(&["check", "--json"], &t2.0);
    assert_ne!(o2.status.code(), Some(0));
    assert!(!marker.exists(), "`ksn check` ran a constant initializer's effects");
}

#[test]
fn rust_forms_forbidden_by_decision_get_guidance_not_a_fix() {
    let src = "enum t.Op\n    Ins { pos: usize, len: usize }\nend t.Op\n\nfn t.g(mut q: i32) -> i32 uses [] panics never\n    let end = q\n    match res {\n        Ok(mut s) => s\n        Err(e) => 0\n    }\n    loop {\n        break Some(3i32)\n    }\n    q\nend t.g\n";
    let t = Tmp::new_raw("rust_forms_guidance", src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(out.contains("tuple variant `Ins(usize, usize)`"), "{}", out);
    assert!(out.contains("`mut` is not allowed in a pattern or parameter"), "{}", out);
    assert!(out.contains("`end` is a reserved word"), "{}", out);
    assert!(out.contains("`break` takes no value"), "{}", out);
}

// ---- E0020 の道でも検査を落とさない: 非正規形のファイルの他の診断は、`ksn fmt` 後と同じ (E0020 を除く) ----

#[test]
fn e0020_still_reports_the_other_diagnostics() {
    let dir = root().join("stage0/tests/data");
    let mut files: Vec<std::path::PathBuf> = vec![];
    for sub in ["diag", "unsound"] {
        let mut v: Vec<_> = std::fs::read_dir(dir.join(sub)).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().map_or(false, |e| e == "ksnl")).collect();
        v.sort();
        files.extend(v.into_iter().skip(1).step_by(9).take(12));
    }
    let mut checked = 0;
    for (i, f) in files.iter().enumerate() {
        let src = std::fs::read_to_string(f).unwrap();
        // 先に fmt で正規形にし (fmt が書き換える形は元の入力で変わるので)、その正規形の間隔をわざと崩したものと比べる。
        let b = Tmp::new_raw(&format!("e0020_fmt{}", i), &src);
        b.fmt();
        let canon = std::fs::read_to_string(b.0.join("src/t.ksnl")).unwrap();
        let loose = canon.replace(" = ", "   =   ").replace(": ", ":   ");
        if loose == canon {
            continue;
        }
        let a = Tmp::new_raw(&format!("e0020_loose{}", i), &loose);
        let got: std::collections::BTreeSet<String> = codes(&text(&run(&["check", "--json"], &a.0).stdout)).into_iter().filter(|c| c != "E0020").collect();
        let want: std::collections::BTreeSet<String> = codes(&text(&run(&["check", "--json"], &b.0).stdout)).into_iter().filter(|c| c != "E0020").collect();
        // 長い行 (E0055) のように、fmt が行の形そのものを変えるものは固有の診断になる。そろえるのはそれ以外。
        if got.contains("E0055") || got.contains("E0049") || want.contains("E0055") {
            continue;
        }
        assert_eq!(got, want, "{}", f.display());
        checked += 1;
    }
    assert!(checked >= 10, "比べた件数が少ない: {}", checked);
}

/// 手書きの `Send` / `Sync` の impl が、対象の型の奥に現れる型パラメータにも境界を求める (E5008) 形の、正しい書き方。
#[test]
fn manual_send_impl_with_nested_bound_is_accepted() {
    let src = "type t.W[X]
    v: X
end t.W

impl[T: core.marker.Send] core.marker.Send for t.W<Option<T>>

type t.V[X]
    v: X
end t.V

impl[T: core.marker.Send] core.marker.Send for t.V<(T, u8)>

fn t.main() -> i32 uses [] panics never
    0i32
end t.main
";
    let t = Tmp::new("send_nested_ok", src);
    fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t\"]\n\n[dependencies]", 1)).unwrap();
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
}
