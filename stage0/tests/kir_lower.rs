//! KIR の下ろしと参照実行器のテスト。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use common::*;

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
    let native = native_or_launcher(t.0.join("target").join("debug").join("t.native"), &(text(&b.stdout) + &text(&b.stderr)));
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
fn kir_fs_create_open_read_remove_match_tree_walker() {
    // `std.fs.create` / `open` / `close` / `sync` / `flush` / `remove_file` / `exists` を
    // 一時ディレクトリ上で使い、`ksn run` と `ksn kir --run` と `--native` の結果が一致すること。
    let tmp = Tmp::new("kir_fs_tmp", "fn t.main() -> u32 uses [] panics may\n    0u32\nend t.main\n");
    let path_str = tmp.0.join("fs_test.bin").to_string_lossy().replace('\\', "/");
    let src = format!(r#"
fn t.main() -> u32 uses [io.fs] panics may
    match std.fs.exists("{path_str}") {{
        true => {{
            core.check.eq(1u8, 2u8)
        }}
        false => {{
        }}
    }}
    with mut file = match std.fs.create("{path_str}") {{
        Ok(f) => f
        Err(_) => return 1u32
    }} close core.mem.drop(file.close()) {{
        match file.write("hi".as_bytes()) {{
            Ok(n) => {{
                core.check.eq(n, 2usize)
            }}
            Err(_) => {{
                return 1u32
            }}
        }}
        match file.sync() {{
            Ok(()) => {{
            }}
            Err(_) => {{
                return 1u32
            }}
        }}
        match file.flush() {{
            Ok(()) => {{
            }}
            Err(_) => {{
                return 1u32
            }}
        }}
    }}
    with mut file = match std.fs.open("{path_str}") {{
        Ok(f) => f
        Err(_) => return 1u32
    }} close core.mem.drop(file.close()) {{
    }}
    match std.fs.remove_file("{path_str}") {{
        Ok(()) => {{
        }}
        Err(_) => {{
            return 1u32
        }}
    }}
    match std.fs.exists("{path_str}") {{
        true => {{
            core.check.eq(1u8, 2u8)
        }}
        false => {{
        }}
    }}
    0u32
end t.main
"#);
    let t = Tmp::new("kir_fs_create_open_read_remove", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", text(&o.stdout), text(&o.stderr));
    let k = kir_run(&["kir", "t.main", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", text(&k.stdout), text(&k.stderr));
}

#[test]
fn kir_fs_read_to_end_matches_tree_walker() {
    // `std.fs.read_to_end` を KIR に下ろす (`fs.read` の繰り返しと、Vec<u8> の再確保 `reserve`)。
    // 既存の要素 (3 バイト) の後ろへ足す。空・小さい・初期の伸び (4096) ちょうど・再確保が何度も起きる大きさで、
    // 木走査・`--run`・`--native` (ASan 込み) が同じ総数と同じバイト列 (重み付きの合計) を返すこと。
    // 読めない (ディレクトリ) ときは Err を返し、この呼び出しで足したバイトは残さないこと。
    let tmp = Tmp::new("kir_read_to_end_tmp", "fn t.main() -> u32 uses [] panics may\n    0u32\nend t.main\n");
    for (label, n) in [("empty", 0usize), ("small", 5), ("chunk", 4096), ("grow", 10_000), ("big", 300_000)] {
        let file = tmp.0.join(format!("read_{label}.bin"));
        let bytes: Vec<u8> = (0..n).map(|i| ((i * 37 + 11) % 251) as u8).collect();
        fs::write(&file, &bytes).unwrap();
        let path_str = file.to_string_lossy().replace('\\', "/");
        let mut seq: Vec<u8> = vec![1, 2, 3];
        seq.extend_from_slice(&bytes);
        let want = seq.iter().fold(0u8, |acc, b| acc.wrapping_mul(31).wrapping_add(*b));
        let total = n + 3;
        let src = format!(
            r#"
fn t.main() -> u32 uses [alloc, io.fs] panics may
    let mut data = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(data.push(1u8))
    core.mem.drop(data.push(2u8))
    core.mem.drop(data.push(3u8))
    with mut file = match std.fs.open("{path_str}") {{
        Ok(f) => f
        Err(_) => return 1u32
    }} close core.mem.drop(file.close()) {{
        match std.fs.read_to_end(&mut file, &mut data) {{
            Ok(k) => {{
                core.check.eq(k, {n}usize)
            }}
            Err(_) => {{
                return 2u32
            }}
        }}
    }}
    core.check.eq(data.len(), {total}usize)
    let mut acc = 0u8
    for b in &data {{
        acc = acc.wrapping_mul(31u8).wrapping_add(*b)
    }}
    core.check.eq(acc, {want}u8)
    0u32
end t.main
"#
        );
        let t = Tmp::new(&format!("kir_read_to_end_{label}"), &src);
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "{label}: tree-walker: {}\n{}", text(&o.stdout), text(&o.stderr));
        let k = kir_run(&["kir", "t.main", "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "{label}: kir: {}\n{}", text(&k.stdout), text(&k.stderr));
    }
    // 読めない: ディレクトリは開けても読めない (EISDIR)。Err を返し、足したバイトは残らない。
    let dir_str = tmp.0.to_string_lossy().replace('\\', "/");
    let src = format!(
        r#"
fn t.main() -> u32 uses [alloc, io.fs] panics may
    let mut data = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(data.push(1u8))
    core.mem.drop(data.push(2u8))
    core.mem.drop(data.push(3u8))
    with mut file = match std.fs.open("{dir_str}") {{
        Ok(f) => f
        Err(_) => return 1u32
    }} close core.mem.drop(file.close()) {{
        match std.fs.read_to_end(&mut file, &mut data) {{
            Ok(_) => {{
                return 4u32
            }}
            Err(_) => {{
            }}
        }}
    }}
    core.check.eq(data.len(), 3usize)
    0u32
end t.main
"#
    );
    let t = Tmp::new("kir_read_to_end_dir", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "dir: tree-walker: {}\n{}", text(&o.stdout), text(&o.stderr));
    let k = kir_run(&["kir", "t.main", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "dir: kir: {}\n{}", text(&k.stdout), text(&k.stderr));
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
    // `i32.successor` は段階 1 では未対応 (理由付きで出る)。以前の例 `char.is_ascii_digit` は kir/native-gaps-3 で下りるようになった
    let t = kir_pkg("kir_dump", "    core.check.eq(i32.successor(&1i32), Some(2i32))\n");
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

#[test]
fn kir_read_only_field_method_in_short_circuit_rhs_does_not_write_back() {
    // `reply.location.is_empty()` を `||` の右辺に置いても、読むだけのメソッドはフィールドを書き戻さない
    // (条件付きの位置の `setfield` が後の使用を支配しなかった。examples/shortener の `step` で見つかった)。
    let t = Tmp::new("kir_ro_field_sc", r#"type t.Reply
    body: alloc.string.String<std.alloc.System>
    location: alloc.string.String<std.alloc.System>
    status: u16
end t.Reply

fn t.parse(n: usize) -> Result<t.Reply, alloc.alloc.AllocError> uses [alloc] panics never
    let mut body = alloc.string.String.new(std.alloc.SYSTEM)
    body.push_str("hello")?
    let mut location = alloc.string.String.new(std.alloc.SYSTEM)
    if n == 1usize {
        location.push_str("http://x")?
    }
    Ok(t.Reply { body: body, location: location, status: 200u16 })
end t.parse

fn t.step(method: &str, path: &str, n: usize, status: u16, expected: &str) -> Result<usize, alloc.alloc.AllocError> uses [alloc] panics never
    let reply = t.parse(n)?
    if reply.status != status {
        return Ok(0usize)
    }
    if reply.body.as_str() != expected {
        return Ok(1usize)
    }
    if !(status != 302u16 || !reply.location.is_empty()) {
        return Ok(2usize)
    }
    let mut line = alloc.string.String.new(std.alloc.SYSTEM)
    line.push_str(method)?
    line.push_str(" ")?
    line.push_str(path)?
    line.push_str(" -> ")?
    line.push_u64(u64.from_u16(reply.status))?
    line.push_str(" ")?
    line.push_str(reply.body.as_str())?
    if !reply.location.is_empty() {
        line.push_str(" Location: ")?
        line.push_str(reply.location.as_str())?
    }
    Ok(line.len())
end t.step

fn t.go(n: usize) -> usize uses [alloc] panics never
    match t.step("GET", "/stats", n, 200u16, "hello") {
        Ok(k) => k
        Err(_e) => 999usize
    }
end t.go

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.go(0usize), 23usize)
    Ok(())
end t.main
"#);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "木走査: {}", text(&o.stderr));
    for (a, want) in [("0", "23"), ("1", "42")] {
        let k = kir_run(&["kir", "t.go", "--run", a], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}: {}", a, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want);
    }
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
    // 要素が所有物 (String) の配列は、移動と解放を扱わないので理由を付けて未対応にする
    // (所有物を持たない集成体の配列は、設計_KIR.md 17.21 で下ろした)。
    let src = "fn t.g() -> usize uses [alloc] panics may pub
    let a = alloc.string.String.new(std.alloc.SYSTEM)
    let b = alloc.string.String.new(std.alloc.SYSTEM)
    let mut s = 0usize
    for x in [a, b] {
        s = s.wrapping_add(x.len())
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
    // 128 ビットの `checked_neg` (Option のペイロードが 128 ビット) は段階 1 の対象外。(`checked_neg` の 64 ビット以下は `kir_native_gaps2.rs`)。`Option` の中のタプルは集成体として持てる (11.5) が、
    // Kasane の定義を持たない扱いの標準ライブラリの型 (`std.io.Stdout`。`std.io.Error` は 12.1 で型表に入った) をペイロードに持つ `Result` は未対応で、理由に型名を出す。
    let src = "fn t.neg(x: i128) -> Option<i128> uses [] panics never
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
    assert!(text(&o.stderr).contains("型 i128 は Option / Result のペイロードに使えない"), "{}", text(&o.stderr));
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
        Some(t.Point { x: a, y: _b }) => a
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
fn kir_first_last_contains_match_tree_walker() {
    // `first()` / `last()` は `Option<&T>` (スカラーは値で持つ)、`contains(&x)` は先頭からの `==` の走査。
    // 配列・スライス・Vec の局所で、空と境界の両側を見る。木走査と値が一致し (`--run` / `--native`、ASan があればそれも)。
    let helpers = "fn t.first_is(a: &[u8], want: u8) -> bool uses [] panics never
    match a.first() {
        Some(x) => *x == want
        None => false
    }
end t.first_is

fn t.last_is(a: &[u8], want: u8) -> bool uses [] panics never
    match a.last() {
        Some(x) => *x == want
        None => false
    }
end t.last_is

fn t.has(a: &[u8], x: u8) -> bool uses [] panics never
    a.contains(&x)
end t.has

fn t.vfirst_is(v: &alloc.vec.Vec<u32, std.alloc.System>, want: u32) -> bool uses [] panics never
    match v.first() {
        Some(x) => *x == want
        None => false
    }
end t.vfirst_is

fn t.vlast_is(v: &alloc.vec.Vec<u32, std.alloc.System>, want: u32) -> bool uses [] panics never
    match v.last() {
        Some(x) => *x == want
        None => false
    }
end t.vlast_is

fn t.vhas(v: &alloc.vec.Vec<u32, std.alloc.System>, x: u32) -> bool uses [] panics never
    v.contains(&x)
end t.vhas

fn t.arr_first_yes() -> bool uses [] panics never
    let arr = [3u8, 1u8, 4u8]
    t.first_is(&arr, 3u8)
end t.arr_first_yes

fn t.arr_first_no() -> bool uses [] panics never
    let arr = [3u8, 1u8, 4u8]
    t.first_is(&arr, 1u8)
end t.arr_first_no

fn t.arr_last_yes() -> bool uses [] panics never
    let arr = [3u8, 1u8, 4u8]
    t.last_is(&arr, 4u8)
end t.arr_last_yes

fn t.arr_has_yes() -> bool uses [] panics never
    let arr = [3u8, 1u8, 4u8]
    t.has(&arr, 1u8)
end t.arr_has_yes

fn t.arr_has_no() -> bool uses [] panics never
    let arr = [3u8, 1u8, 4u8]
    t.has(&arr, 9u8)
end t.arr_has_no

fn t.sub_last_yes() -> bool uses [] panics never
    let arr = [3u8, 1u8, 4u8, 1u8, 5u8]
    let mid = arr.sub(1usize, 3usize)
    match mid {
        Some(s) => t.last_is(s, 4u8)
        None => false
    }
end t.sub_last_yes

fn t.vec_empty() -> bool uses [alloc] panics may
    let v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    t.vfirst_is(&v, 0u32)
end t.vec_empty

fn t.vec_first_yes() -> bool uses [alloc] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(5u32))
    core.mem.drop(v.push(6u32))
    t.vfirst_is(&v, 5u32)
end t.vec_first_yes

fn t.vec_last_yes() -> bool uses [alloc] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(5u32))
    core.mem.drop(v.push(6u32))
    t.vlast_is(&v, 6u32)
end t.vec_last_yes

fn t.vec_has_yes() -> bool uses [alloc] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(5u32))
    core.mem.drop(v.push(6u32))
    t.vhas(&v, 6u32)
end t.vec_has_yes

fn t.vec_has_no() -> bool uses [alloc] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(5u32))
    core.mem.drop(v.push(6u32))
    t.vhas(&v, 7u32)
end t.vec_has_no

fn t.str_first_len(v: &alloc.vec.Vec<alloc.string.String, std.alloc.System>) -> usize uses [] panics never
    let g = v.first()
    match g {
        Some(x) => x.len()
        None => 0usize
    }
end t.str_first_len

fn t.str_last_len(v: &alloc.vec.Vec<alloc.string.String, std.alloc.System>) -> usize uses [] panics never
    match v.last() {
        Some(x) => x.len()
        None => 0usize
    }
end t.str_last_len

fn t.str_mk(n: usize) -> usize uses [alloc] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(\"ab\"))
    core.mem.drop(v.push(s))
    let mut t2 = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(t2.push_str(\"xyz\"))
    core.mem.drop(v.push(t2))
    t.str_first_len(&v) + t.str_last_len(&v) + n
end t.str_mk
";
    // (関数, 期待値)。木走査は `core.check.eq` で確かめ、KIR は `--run` の表示 (`true` / `false`) で確かめる。
    let cases = [
        ("t.arr_first_yes", "true"),
        ("t.arr_first_no", "false"),
        ("t.arr_last_yes", "true"),
        ("t.arr_has_yes", "true"),
        ("t.arr_has_no", "false"),
        ("t.sub_last_yes", "true"),
        ("t.vec_empty", "false"),
        ("t.vec_first_yes", "true"),
        ("t.vec_last_yes", "true"),
        ("t.vec_has_yes", "true"),
        ("t.vec_has_no", "false"),
    ];
    let mut main = String::from("\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n");
    for (f, want) in cases {
        main.push_str(&format!("    core.check.eq({}(), {})\n", f, want));
    }
    // 所有物の要素 (`Vec<String>`): `let g = v.first()` と `match v.last()` は `bref` (借用) で持つ。
    main.push_str("    core.check.eq(t.str_mk(0usize), 5usize)\n    Ok(())\nend t.main\n");
    let t = Tmp::new("kir_first_last", &format!("{}{}", helpers, main));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    for (f, want) in cases {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
    let k = kir_run(&["kir", "t.str_mk", "--run", "0"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir t.str_mk\n{}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "5", "kir value of t.str_mk");
}
