//! `///` の本文 (FQN のリンク・モジュールの説明・コード例) と `ksn index` の統合テスト (仕様 3.2 / 7 章)。

mod common;
use common::*;

use std::fs;

/// リンク・モジュールの説明・コード例を含む、正しいパッケージの本体。
const DEMO: &str = "/// テスト用のモジュール。
///
/// カウンタは [`t.Counter`] を使う。短い名前 [`Counter`] も一意なら通る。

/// カウンタ。`bump` は [`t.Counter.bump`] で増やす。
type t.Counter pub
    n: u32 pub
end t.Counter

/// 1 増やす。**重要**。
///
/// ```ksn
/// let mut c = t.Counter {
///     n: 0u32
/// }
/// core.check.eq(c.bump(), 1u32)
/// ```
fn t.Counter.bump(self: &mut t.Counter) -> u32 uses [] panics never pub
    self.n = self.n.wrapping_add(1u32)
    self.n
end t.Counter.bump

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main

test t.Counter.bump.test_once
    let mut c = t.Counter {
        n: 4u32
    }
    core.check.eq(c.bump(), 5u32)
end t.Counter.bump.test_once
";

/// 生成された `target/doc` の 1 ファイルを読む。
fn doc_file(t: &Tmp, n: &str) -> String {
    fs::read_to_string(t.0.join("target").join("doc").join(n)).unwrap_or_else(|_| panic!("missing {}", n))
}

#[test]
fn doc_links_point_to_definition_anchors_and_module_doc_is_shown() {
    let t = Tmp::new_ksnh("doc_links_ok", DEMO);
    let o = run(&["check"], &t.0);
    assert!(o.status.success(), "{}", text(&o.stdout));
    let o = run(&["doc"], &t.0);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let m = doc_file(&t, "t.html");
    // FQN のリンクは定義のアンカーへ (同じモジュールなので `#` で始まる)
    assert!(m.contains("<a href=\"#t.Counter.bump\"><code>t.Counter.bump</code></a>"), "{}", m);
    // 一意な短い名前もリンクになる
    assert!(m.contains("<a href=\"#t.Counter\"><code>Counter</code></a>"), "{}", m);
    // モジュールの説明はモジュールのページの先頭に出る
    assert!(m.contains("<p>テスト用のモジュール。</p>"), "{}", m);
    // コード例は例として印が付く
    assert!(m.contains("<pre class=\"example\"><code>"), "{}", m);
    // 本文の強調・コード
    assert!(m.contains("<strong>重要</strong>") && m.contains("<code>bump</code>"), "{}", m);
}

#[test]
fn doc_link_to_unknown_definition_fails_check_and_doc() {
    let src = format!("{}\n/// 壊れた [`t.Missing`] と [`no.such`]。\nfn t.broken() -> u8 uses [] panics never pub\n    1u8\nend t.broken\n", DEMO);
    let t = Tmp::new_ksnh("doc_links_bad", &src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
    let out = text(&o.stdout);
    assert_eq!(codes(&out).iter().filter(|c| *c == "E1125").count(), 2, "{}", out);
    assert!(out.contains("`t.Missing`") && out.contains("`no.such`"), "{}", out);
    // `ksn doc` は失敗し、ページを書かない
    let o = run(&["doc"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("E1125"), "{}", text(&o.stderr));
    assert!(!t.0.join("target").join("doc").join("index.html").exists());
}

#[test]
fn doc_link_to_a_field_is_rejected() {
    // フィールドは FQN のリンクの先にならない (3.2)
    let src = format!("{}\n/// [`t.Counter.n`]\nfn t.f() -> u8 uses [] panics never pub\n    1u8\nend t.f\n", DEMO);
    let t = Tmp::new_ksnh("doc_links_field", &src);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
    assert!(text(&o.stdout).contains("E1125"), "{}", text(&o.stdout));
}

#[test]
fn index_is_one_json_line_per_definition_in_fqn_order_and_is_stable() {
    let t = Tmp::new_ksnh("doc_index", DEMO);
    let o = run(&["index"], &t.0);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let a = text(&o.stdout);
    let lines: Vec<&str> = a.lines().collect();
    assert!(lines.iter().all(|l| l.starts_with('{') && l.ends_with('}')), "{}", a);
    // FQN の順 (バイト順) と安定性
    let fqns: Vec<String> = lines
        .iter()
        .map(|l| {
            let rest = l.split("\"fqn\":\"").nth(1).unwrap();
            rest.split('"').next().unwrap().to_string()
        })
        .collect();
    let mut sorted = fqns.clone();
    sorted.sort();
    assert_eq!(fqns, sorted);
    assert_eq!(a, text(&run(&["index"], &t.0).stdout));
    // 定義の欄: 種別・ファイル・行・ヘッダ・本文・契約・テスト・公開
    let bump = lines.iter().find(|l| l.contains("\"fqn\":\"t.Counter.bump\"")).expect("bump");
    assert!(bump.contains("\"kind\":\"fn\""), "{}", bump);
    assert!(bump.contains("\"file\":\"src/t.ksnh\""), "{}", bump);
    assert!(bump.contains("\"uses\":[]") && bump.contains("\"panics\":\"never\""), "{}", bump);
    assert!(bump.contains("\"tests\":[\"t.Counter.bump.test_once\"]"), "{}", bump);
    assert!(bump.contains("\"pub\":true"), "{}", bump);
    assert!(bump.contains("\"doc\":\"1 増やす。**重要**。"), "{}", bump);
    // モジュールの説明のある `t` は種別 module の行
    assert!(lines.iter().any(|l| l.contains("\"kind\":\"module\"") && l.contains("\"fqn\":\"t\"")), "{}", a);
    // テストは索引に定義として出ない
    assert!(!lines.iter().any(|l| l.contains("test_once\",\"kind\"")), "{}", a);
}

#[test]
fn index_lists_private_definitions_with_pub_false() {
    let src = format!("{}\nfn t.secret() -> u8 uses [] panics never\n    1u8\nend t.secret\n", DEMO);
    let t = Tmp::new_ksnh("doc_index_private", &src);
    let a = text(&run(&["index"], &t.0).stdout);
    let line = a.lines().find(|l| l.contains("\"fqn\":\"t.secret\"")).expect("secret");
    assert!(line.contains("\"pub\":false"), "{}", line);
}

#[test]
fn examples_run_under_ksn_test_and_report_their_source_line() {
    // 正しい例は通る (`--doc` は例だけ)
    let t = Tmp::new_ksnh("doc_ex_ok", DEMO);
    let o = run(&["test", "--doc"], &t.0);
    assert!(o.status.success(), "{}{}", text(&o.stdout), text(&o.stderr));
    // 誤った例 (型の誤り) は、元の囲みの行 (src/t.ksnh の行) で報告される
    let bad = DEMO.replace("core.check.eq(c.bump(), 1u32)", "core.check.eq(c.bump(), true)");
    let t = Tmp::new_ksnh("doc_ex_bad", &bad);
    let o = run(&["test", "--doc"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    let out = text(&o.stdout) + &text(&o.stderr);
    let line = bad.lines().position(|l| l.contains("core.check.eq(c.bump(), true)")).unwrap() + 1;
    let fence = bad.lines().position(|l| l.starts_with("/// ```ksn")).unwrap() + 1;
    assert!(out.contains(&format!("src/t.ksnh:{}", fence)) || out.contains(&format!("src/t.ksnh:{}", line)), "{}", out);
    assert!(out.contains("in a doc example"), "{}", out);
}

#[test]
fn doc_example_failing_at_run_time_fails_its_test() {
    let bad = DEMO.replace("core.check.eq(c.bump(), 1u32)", "core.check.eq(c.bump(), 2u32)");
    let t = Tmp::new_ksnh("doc_ex_run", &bad);
    let o = run(&["test", "--doc"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}{}", text(&o.stdout), text(&o.stderr));
    let out = text(&o.stdout) + &text(&o.stderr);
    assert!(out.contains("t.tests.doc_1.test_example"), "{}", out);
}

#[test]
fn module_doc_survives_fmt_and_a_doc_before_import_is_an_error() {
    let t = Tmp::new_ksnh("doc_fmt", DEMO);
    let o = run(&["fmt", "--check"], &t.0);
    assert!(o.status.success(), "{}{}", text(&o.stdout), text(&o.stderr));
    let t = Tmp::new_ksnh("doc_adjacent", "/// 直後に import\nimport core.option\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n");
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
    assert!(text(&o.stdout).contains("E0002"), "{}", text(&o.stdout));
}
