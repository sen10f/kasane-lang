//! N3 (設計_ネイティブUI.md): C から Kasane のクロージャを呼ぶコールバック。利用者のデータは
//! 捕まえた変数の環境で、C の `void (*cb)(void *user, int x)` の `user` にそのポインタが渡る
//! (処理系が踏み台を用意する)。捕まえたクロージャを C に渡せない (E5005) ので、今日は `#[ignore]`。
//! `tests/data/ffi_userdata/README.md` に書き方の候補を置いてある。実装したら `#[ignore]` を外す。
//! 名前付き関数 + user データの形 (動くもの) は `tests/data/ffi/many.ksnl` の `run_ticks`。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::*;

/// すべての検査を通ったプログラムが出す 1 行。
const WANT: &str = "userdata ok\n";

/// `tests/data/ffi_userdata/acc.c` を共有ライブラリにして、`LIB:` をそのパスに置き換えた
/// 一時パッケージ (`[unsafe] modules = ["t"]`) を作る。Kasane のソースは候補 A の形。
#[cfg(unix)]
fn userdata_pkg() -> Tmp {
    let t = Tmp::new("ffi_userdata", "");
    fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t\"]\n\n[dependencies]", 1)).unwrap();
    let c = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join("ffi_userdata").join("acc.c");
    let lib = t.0.join("libaccuser.so");
    let o = cc(&["-shared", "-fPIC", "-O1", "-o", lib.to_str().unwrap(), c.to_str().unwrap()], &t.0);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let src = include_str!("data/ffi_userdata/main.ksnl").replace("LIB:", &format!("{}:", lib.to_str().unwrap()));
    fs::write(t.0.join("src").join("t.ksnl"), src).unwrap();
    t
}

#[cfg(unix)]
#[test]
#[ignore = "捕まえたクロージャを C のコールバックに渡せない (E5005)。N3 の実掛の試験"]
fn c_calls_a_kasane_closure_that_captures_with_user_data() {
    let t = userdata_pkg();
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "{}{}", text(&c.stdout), text(&c.stderr));
    // インタプリタ (動的ロードで `LIB:` の共有ライブラリを拾う)。
    let i = run(&["run"], &t.0);
    assert_eq!(i.status.code(), Some(0), "interpreter: {}{}", text(&i.stdout), text(&i.stderr));
    assert_eq!(text(&i.stdout), WANT, "interpreter: {}", text(&i.stderr));
    // ネイティブ (LLVM)。動的ロードを持たなければ、理由を出してバンドルへ退避する。
    let (out, n) = ffi_native(&t, "ffi_userdata");
    let o = match n {
        Some(n) => n,
        None => {
            assert!(out.contains("native backend skipped: ") && out.contains("the interpreter bundle is used"), "{}", out);
            assert_native_or_reasoned_skip(&out);
            Command::new(t.0.join("target").join("debug").join("t")).current_dir(&t.0).output().unwrap()
        }
    };
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), WANT, "native: {}", text(&o.stderr));
}