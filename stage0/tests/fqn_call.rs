//! FQN 呼び出しが局所変数によってシャドーイングされる場合の E1108 ヒント診断 (spec 3.2)。

mod common;
use common::*;

/// `src/t.ksnl` に `src` を書いて `ksn check --json` を実行し、(終了コード, エラーコード一覧) を返す。
fn check_src(name: &str, src: &str) -> (Option<i32>, Vec<String>) {
    let t = Tmp::new(name, src);
    let o = run(&["check", "--json"], &t.0);
    (o.status.code(), codes(&text(&o.stdout)))
}

#[test]
fn fqn_call_shadowed_by_local_emits_hint() {
    // 局所変数 `todo` がモジュールパス `todo.model.push_quoted` をシャドーイングする。
    let src = "fn todo.model.push_quoted(x: u32) -> u32 uses [] panics never\n    x\nend todo.model.push_quoted\n\nfn t.main() -> u32 uses [] panics never\n    let todo: u32 = 0u32\n    todo.model.push_quoted(1u32)\nend t.main\n";
    let (rc, got) = check_src("shadow_hint", src);
    assert_eq!(rc, Some(1));
    assert!(got.iter().any(|c| c == "E1108"), "{:?}", got);

    // ヒントメッセージの内容を確認
    let t = Tmp::new("shadow_hint_msg", src);
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert!(
        out.contains("is a local variable, which shadows the module path (3.2)"),
        "{:?}",
        out
    );
    assert!(
        out.contains("rename the local to call `todo.model.push_quoted`"),
        "{:?}",
        out
    );
}

#[test]
fn fqn_call_normal_no_shadow() {
    // 衝突しない局所名: メソッド呼び出しは正常に通る。
    let src = "fn t.main() -> u32 uses [] panics never\n    let v = 1u32\n    v.wrapping_add(2u32)\nend t.main\n";
    let (rc, _) = check_src("normal", src);
    assert_eq!(rc, Some(0));
}
