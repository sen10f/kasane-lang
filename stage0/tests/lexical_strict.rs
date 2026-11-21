//! 字句の「書き方は 1 通り」の規則 (G38: 設計_書き方は1通り.md の L1〜L4・L6・L7・L9・L16)。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;
use std::fs;

fn prog(body: &str) -> String {
    format!("fn t.main() -> () uses [] panics never\n{}end t.main\n", body)
}

fn check_codes_of(name: &str, src: &str) -> Vec<String> {
    // コメントは `.ksnh` が持つ (`.ksnl` にコメントは置けない)。字句の規則はコメントにもかかる。
    let t = Tmp::new_ksnh(name, src);
    codes(&text(&run(&["check", "--json"], &t.0).stdout))
}

#[test]
fn forbidden_chars_are_rejected_everywhere() {
    // 文字列・文字・コメント・`///` のどこでも、書けない文字は E0004 (表示の向き・見えない文字・制御・BOM・NBSP)。
    for (k, c) in ['\u{202e}', '\u{2066}', '\u{200f}', '\u{061c}', '\u{200b}', '\u{1b}', '\u{7f}', '\u{85}', '\u{2028}', '\u{a0}', '\u{feff}', '\u{e000}', '\u{ad}']
        .iter()
        .enumerate()
    {
        let in_str = prog(&format!("    let s = \"a{}b\"\n", c));
        assert!(check_codes_of(&format!("lx_str{}", k), &in_str).contains(&"E0004".to_string()), "string U+{:04X}", *c as u32);
        let in_cmt = prog(&format!("    // a{}b\n", c));
        assert!(check_codes_of(&format!("lx_cmt{}", k), &in_cmt).contains(&"E0004".to_string()), "comment U+{:04X}", *c as u32);
        let in_doc = format!("/// a{}b\n{}", c, prog(""));
        assert!(check_codes_of(&format!("lx_doc{}", k), &in_doc).contains(&"E0004".to_string()), "doc U+{:04X}", *c as u32);
    }
    let in_chr = prog("    let c = '\u{202e}'\n");
    assert!(check_codes_of("lx_chr", &in_chr).contains(&"E0004".to_string()));
}

#[test]
fn bom_is_rejected_with_its_own_reason() {
    let t = Tmp::new("lx_bom", &format!("\u{feff}{}", prog("")));
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1));
    assert!(out.contains("E0004") && out.contains("BOM"), "{}", out);
}

#[test]
fn escapes_have_one_spelling() {
    // 正しい書き方 (名前のある・構文上要る・書けない文字の `\u{..}`) は通る。
    let ok = prog("    let s = \"a\\n\\t\\r\\0\\\\\\\"\\u{202e}\\u{1b}\\u{7f}\\u{a0}b\"\n    let c = '\\''\n    let b = b\"A\\x01\\xff\"\n");
    let c = check_codes_of("lx_esc_ok", &ok);
    assert!(!c.iter().any(|x| x.starts_with("E000")), "{:?}", c);
    // 誤り: 印字できる文字のエスケープ、`\x`、先頭の 0・大文字の 16 進、名前のあるものの `\u`、不要な `\"` / `\'`。
    for (k, bad) in [
        "\"\\x41\"",
        "\"\\u{41}\"",
        "\"\\u{3042}\"",
        "\"\\u{0041}\"",
        "\"\\u{1B}\"",
        "\"\\u{01b}\"",
        "\"\\u{a}\"",
        "\"\\u{0}\"",
        "\"\\u{22}\"",
        "\"\\'\"",
        "'\\\"'",
        "'\\u{41}'",
        "'\\x41'",
        "b\"\\x41\"",
        "b\"\\x0a\"",
        "b\"\\xFF\"",
    ]
    .iter()
    .enumerate()
    {
        let src = prog(&format!("    let s = {}\n", bad));
        let c = check_codes_of(&format!("lx_esc{}", k), &src);
        assert!(c.contains(&"E0005".to_string()), "{} -> {:?}", bad, c);
    }
}

#[test]
fn escape_errors_are_fixed_by_ksn_fix() {
    let src = prog("    let s = \"a\\x41\\u{3042}\\u{0041}\\'\"\n    let c = '\\u{41}'\n    let d = '\\\"'\n    let n = \"e\u{301}\"\n");
    let t = Tmp::new("lx_fix", &src);
    let o = run(&["fix"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    let fixed = fs::read_to_string(t.0.join("src/t.ksnl")).unwrap();
    assert!(fixed.contains("= \"aAあA'\""), "{}", fixed);
    assert!(fixed.contains("= 'A'") && fixed.contains("= '\"'"), "{}", fixed);
    assert!(fixed.contains("= \"\u{e9}\""), "{}", fixed);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
}

#[test]
fn non_nfc_text_is_rejected() {
    let in_str = prog("    let s = \"e\u{301}\"\n");
    assert!(check_codes_of("lx_nfc_s", &in_str).contains(&"E0006".to_string()));
    let in_cmt = prog("    // e\u{301}\n");
    assert!(check_codes_of("lx_nfc_c", &in_cmt).contains(&"E0006".to_string()));
    let ok = prog("    let s = \"\u{e9}\u{3042}\"\n    // \u{e9}\n");
    let c = check_codes_of("lx_nfc_ok", &ok);
    assert!(!c.iter().any(|x| x.starts_with("E000")), "{:?}", c);
}

#[test]
fn invalid_utf8_is_rejected() {
    let t = Tmp::new("lx_utf8", "");
    let mut bytes = prog("    let s = \"ok\"\n    let t = \"a").into_bytes();
    bytes.push(0xff);
    bytes.extend_from_slice(b"b\"\n");
    bytes.extend_from_slice(b"end t.main\n");
    // 末尾の `end t.main` が重なるので作り直す。
    let mut b2 = b"fn t.main() -> () uses [] panics never\n    let s = \"a".to_vec();
    b2.push(0xff);
    b2.extend_from_slice(b"b\"\nend t.main\n");
    fs::write(t.0.join("src/t.ksnl"), b2).unwrap();
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(1));
    assert!(codes(&out).contains(&"E0007".to_string()), "{}", out);
    assert!(out.contains("\"line\": 2"), "{}", out);
}

#[test]
fn lexical_error_positions_are_the_real_ones() {
    // 識別子の中の非 ASCII (キリル文字の `а`) の位置が、ファイルの行・列で出る。
    let src = prog("    let x = 1u32\n    let \u{430}b = 1u32\n    let y = 2u32\n");
    let t = Tmp::new("lx_pos", &src);
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    assert!(out.contains("\"code\": \"E0001\"") && out.contains("\"line\": 3, \"col\": 9"), "{}", out);
}

#[test]
fn fmt_keeps_a_lone_cr_in_a_string() {
    let t = Tmp::new("lx_cr", "");
    fs::write(t.0.join("src/t.ksnl"), "fn t.main() -> () uses [] panics never\n    let s = \"a\rb\"\nend t.main\n").unwrap();
    let before = fs::read(t.0.join("src/t.ksnl")).unwrap();
    let _ = run(&["fmt"], &t.0);
    assert_eq!(fs::read(t.0.join("src/t.ksnl")).unwrap(), before, "fmt must not rewrite a lone CR");
    assert_eq!(run(&["check"], &t.0).status.code(), Some(1));
}

#[test]
fn fmt_check_agrees_with_check_on_trailing_nbsp() {
    let src = prog("    let s = 1u32\u{a0}\n");
    let t = Tmp::new("lx_nbsp", &src);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(1));
    assert_eq!(run(&["fmt", "--check"], &t.0).status.code(), Some(1));
}

#[test]
fn get_and_doc_show_forbidden_chars_visibly() {
    // `ksn get` は書けない文字が (他の経路で入っても) 見える形で出る。ここでは `visible` を通る文字列だけ確かめる。
    let src = prog("    let s = \"\\u{202e}\"\n");
    let t = Tmp::new("lx_vis", &src);
    let o = run(&["get", "t.main"], &t.0);
    let out = text(&o.stdout);
    assert!(!out.contains('\u{202e}'), "{}", out);
}

#[test]
fn invisible_fillers_and_variation_selectors_are_rejected_in_literals() {
    // 見えない詰め物・異体字選択子は文字列・文字に生で書けない (E0004)。絵文字の提示選択子 U+FE0F は書いてよい。
    for (k, c) in ['\u{115f}', '\u{1160}', '\u{3164}', '\u{ffa0}', '\u{2800}', '\u{34f}', '\u{17b4}', '\u{180b}', '\u{180e}', '\u{fe00}', '\u{e0100}']
        .iter()
        .enumerate()
    {
        let in_str = prog(&format!("    let s = \"a{}b\"\n", c));
        assert!(check_codes_of(&format!("lx_fill{}", k), &in_str).contains(&"E0004".to_string()), "string U+{:04X}", *c as u32);
    }
    let ok = prog("    let s = \"a\u{2764}\u{fe0f}b\"\n");
    assert!(!check_codes_of("lx_fill_ok", &ok).contains(&"E0004".to_string()));
}
