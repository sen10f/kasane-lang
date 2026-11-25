//! 手書きの `impl core.ops.Drop` の実行 (仕様 4.2 / 4.4、`実装計画.d/user-drop.md`)。
//! 入力は `tests/data/user_drop/*.ksnl`: `Drop.drop` は `static` のアトミックに履歴 (`log * 10 + id`) を書き、
//! `main` が `core.check.eq` で期待の履歴を確かめて `ok` を出す。`ksn run` (VM = KIR の実行)・LLVM の debug / release・
//! ASan (二重解放・解放後の使用がない) の 4 つで同じ結果になること。

#![allow(dead_code, unused_imports)]

mod common;

#[path = "common/scratch.rs"]
mod scratch;

use common::*;
use std::fs;
use std::process::Command;

fn source(name: &str) -> String {
    fs::read_to_string(root().join("stage0").join("tests").join("data").join("user_drop").join(format!("{}.ksnl", name))).unwrap()
}

fn native_out(t: &Tmp, name: &str, release: bool, asan: bool) -> Option<std::process::Output> {
    let cache = scratch::Scratch::at(format!("ksn_cache_user_drop_{}_{}_{}_{}", std::process::id(), name, release, asan));
    let mut c = Command::new(exe());
    c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
    if asan {
        c.env("KSN_CLANG_ARGS", "-fsanitize=address -fno-omit-frame-pointer -g -O1");
    }
    if release {
        c.arg("--release");
    }
    let o = c.output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{} (release={}, asan={}): {}", name, release, asan, out);
    assert!(!out.contains("skipped"), "{}: ネイティブに下りない: {}", name, out);
    let kind = if release { "release" } else { "debug" };
    let p = t.0.join("target").join(kind).join("t");
    assert!(p.exists(), "{}: ネイティブの実行ファイルがない: {}", name, out);
    let mut r = Command::new(&p);
    if asan {
        r.env("ASAN_OPTIONS", "detect_leaks=1");
    }
    let r = r.output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    Some(r)
}

fn all_engines(name: &str) {
    if cfg!(windows) {
        return;
    }
    let t = Tmp::new(&format!("user_drop_{}", name), &source(name));
    let vm = run(&["run"], &t.0);
    assert_eq!(vm.status.code(), Some(0), "{}: {}", name, text(&vm.stderr));
    assert_eq!(text(&vm.stdout), "ok\n", "{}: {}", name, text(&vm.stderr));
    for release in [false, true] {
        let r = native_out(&t, name, release, false).unwrap();
        assert_eq!(r.status.code(), Some(0), "{} (release={}): {}", name, release, text(&r.stderr));
        assert_eq!(text(&r.stdout), "ok\n", "{} (release={})", name, release);
    }
    if asan_available() {
        let r = native_out(&t, name, false, true).unwrap();
        assert_eq!(r.status.code(), Some(0), "{} (asan): {}", name, text(&r.stderr));
        assert_eq!(text(&r.stdout), "ok\n", "{} (asan)", name);
    }
}

/// スコープの終わり (宣言の逆順)・再代入・入れ子の構造体 (外側の `drop` のあと欄は宣言順)・`Vec<D>` の要素 (昇順)・
/// move 済みは呼ばない・条件付きの move (実行時の印)・早期 return・`break`・`Option`・一時値。
#[test]
fn user_drop_scope_reassign_nested_vec_option_temp() {
    all_engines("scope");
}

/// 欄の部分的な move (残りの欄だけ破棄)・欄への代入 (古い値を破棄)・`?`・`match` の束縛・`Box<D>`・`with`・`swap` / `replace`・`Vec` の `pop` / `clear`。
#[test]
fn user_drop_partial_move_field_assign_try_match_box_with_replace_vec_ops() {
    all_engines("moves");
}

/// `spawn` のクロージャに move した値はスレッドの中で破棄される。
#[test]
fn user_drop_value_moved_into_a_thread_is_dropped_there() {
    all_engines("thread");
}

/// 列挙型の `Drop`・引数の受け渡し (呼ばれた側が破棄)・ループの再代入・`continue`・タプル・欄に `Vec<D>` と `Option<D>` を持つ構造体・分解。
#[test]
fn user_drop_enum_args_loops_tuple_struct_with_vec_destructure() {
    all_engines("misc");
}

/// `ksn test` も同じ (VM で走る)。
#[test]
fn user_drop_ksn_test_runs_the_drop() {
    let src = source("scope").replace(
        "fn t.main()",
        "test t.s_reassign.test_drop
    t.s_reassign()
    core.check.eq(t.take(), 12u64)
end t.s_reassign.test_drop

fn t.main()",
    );
    let t = Tmp::new("user_drop_test_cmd", &src);
    let o = run(&["test"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert!(out.contains("test_drop ... ok") || out.contains("1 passed"), "{}", out);
}

/// 木走査は所有を追跡しないので、KIR に下ろせない手書き Drop のプログラムは、退避して黙って Drop を飛ばさずに理由付きで失敗する。
#[test]
fn user_drop_program_that_does_not_lower_fails_instead_of_skipping_drops() {
    if cfg!(windows) {
        return;
    }
    // `Fn` のクロージャが Drop 型を move で取り込む形は (まだ) KIR に下ろせない。
    let src = source("scope").replace(
        "fn t.main()",
        "fn t.s_closure() -> () uses [] panics never
    let d = t.mk(1u64)
    let f = fn() -> u64 capture [move d] {
        d.id
    }
    core.mem.drop(f())
end t.s_closure

fn t.main()",
    ).replace("    t.s_reassign()\n", "    t.s_closure()\n    t.s_reassign()\n");
    let t = Tmp::new("user_drop_no_lower", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("hand-written `core.ops.Drop`"), "{}", text(&o.stderr));
}
