//! 文字列リテラル・文字リテラルとコメントの中の `let` が変数の宣言と誤読され,
//! 未使用変数 (E1020) が誤検出されないかの回帰テスト。

mod common;

use common::*;

#[test]
fn string_literal_containing_if_let_ok_does_not_report_e1020() {
    // 文字列の中に `if let ok` と `let x` を含む。これらは宣言ではなく単なる文字なので、
    // `ok` や `x` は束縛として扱われず、E1020 は出ないはず。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let s = "if let ok \"let x\""
    // let unused_in_comment
    std.io.print_line(s)?
    Ok(())
end t.main
"#;
    let t = Tmp::new("lit_if_let", src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    assert!(
        !codes(&text(&o.stdout)).iter().any(|c| c == "E1020"),
        "{}",
        text(&o.stdout)
    );
}

#[test]
fn genuinely_unused_variable_is_still_reported() {
    // 回帰防止: 文字列の中ではない本当に未使用の変数は E1020 を引き続き出す。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let s = "ok"
    let unused = 1u32
    std.io.print_line(s)?
    Ok(())
end t.main
"#;
    let t = Tmp::new("lit_unused", src);
    let o = run(&["check", "--json"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
    assert!(
        codes(&text(&o.stdout)).iter().any(|c| c == "E1020"),
        "{}",
        text(&o.stdout)
    );
}
