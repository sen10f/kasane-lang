//! 標準ライブラリの欠け (G54): `String` キーの `HashMap` を `&str` で引く (`get_str` など)・
//! `String.try_clone`・`Vec<T: Debug>` の `Debug` (`derive [Debug]` の欄に置ける)。
//! 木走査 (`ksn run`) と LLVM のネイティブ (退避なし) の出力が一致することを見る。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;
use std::process::Command;

const SRC: &str = include_str!("data/stdlib_gaps.ksnl");
const WANT: &str = "1 none has-two no-tw 20 gone len=1\nRec { name: \"hi\", xs: [1, 2] } hi\n";

#[test]
fn str_lookup_try_clone_vec_debug_agree() {
    let t = Tmp::new("stdlib_gaps_interp", SRC);
    let chk = run(&["check"], &t.0);
    assert!(text(&chk.stdout).trim().is_empty() && chk.status.success(), "check: {}{}", text(&chk.stdout), text(&chk.stderr));
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    assert_eq!(text(&interp.stdout), WANT);
    for release in [false, true] {
        let (bt, exe_path, out) = build_native_strict(&format!("stdlib_gaps_{}", release), SRC, release);
        let p = exe_path.unwrap_or_else(|| panic!("ネイティブが作れない (退避しない): {}", out));
        let o = Command::new(&p).current_dir(&bt.0).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), WANT, "ネイティブと木走査の出力が違う (release: {})", release);
    }
}

/// 複製は元と別の確保: 元を書き換えても複製は変わらない。`Vec<String>` の `Debug` (木走査)。
#[test]
fn try_clone_is_independent_and_vec_of_strings_debug() {
    let src = "fn t.main() -> Result<(), alloc.alloc.AllocError> uses [alloc, io.console] panics never\n    let mut a = alloc.string.String.from_str(std.alloc.SYSTEM, \"ab\")?\n    let b = a.try_clone()?\n    a.push_str(\"c\")?\n    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    v.push(a)?\n    v.push(b)?\n    let mut out = alloc.string.String.new(std.alloc.SYSTEM)\n    out.push_debug(&v)?\n    core.mem.drop(std.io.print_line(out.as_str()))\n    Ok(())\nend t.main\n";
    let t = Tmp::new("stdlib_gaps_indep", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), "[\"abc\", \"ab\"]\n");
}

/// 列挙型のペイロードの `String` を `match` の参照で束縛し、`&String` の引数として関数へ渡す形。
/// 以前のネイティブは、ヘッダのアドレスではなく中身のバッファへの ptr を渡して壊れた (`coerce_args`)。
#[test]
fn native_ref_to_string_payload_passed_as_arg() {
    let out = agree_with_interpreter("ref_string_payload", include_str!("data/native_ref_string_payload.ksnl"));
    assert_eq!(out, "3\n");
}
