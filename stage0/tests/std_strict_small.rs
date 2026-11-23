//! 小さな標準ライブラリの厳密化: `core.slice.ct_eq` (定数時間の比較) と `<int>.parse_decimal` (厳密な 10 進)。
//! 木走査 (`ksn run`) とネイティブ (`ksn build`) の出力が一致することを確かめる。
//! `std.http` の制御バイト・`+` の拒否・`header_unique` は `http_tls.rs` の `parse.ksnl` の試験で確かめる。

mod common;

use common::*;

const SRC: &str = include_str!("data/std_strict_small/main.ksnl");

/// `main.ksnl` の期待出力 (入力の文字列と結果の対)。`parse_decimal` は符号・先頭の 0・空白・非 ASCII の数字を拒み、範囲外は Err。
/// 符号つきの型は `-` を受け、`-0`・`-00` は拒む。
const OUT: &str = "eq-abc
ne
ne
eq-empty
0
ok
7
ok
00
err
007
err
255
ok
256
err
+1
err
-0
err

err
 1
err
1 
err
٣
err
12a
err
-128
ok
-129
err
127
ok
128
err
-0
err
-00
err
-0x1
err
-1
ok
+1
err
18446744073709551615
ok
18446744073709551616
err
0
ok
-9223372036854775808
ok
9223372036854775808
err
-9223372036854775809
err
123
ok
-1
err
";

fn interp_stdout(name: &str, src: &str) -> String {
    let t = common::Tmp::new(name, src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    text(&o.stdout)
}

fn native_stdout(name: &str, src: &str) -> String {
    let (t, found, out) = build_native_strict(name, src, true);
    let Some(p) = found else { panic!("ネイティブの実行ファイルがない (退避は許さない): {}", out) };
    assert!(!out.contains("skipped"), "ネイティブに下ろせず退避した: {}", out);
    let o = std::process::Command::new(&p).current_dir(&t.0).output().expect("run native");
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    text(&o.stdout)
}

#[test]
fn std_strict_small_interpreter_and_native_agree() {
    assert_eq!(interp_stdout("strict_small_i", SRC), OUT, "木走査の出力");
    assert_eq!(native_stdout("strict_small_n", SRC), OUT, "ネイティブの出力");
}
