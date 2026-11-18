//! `ksn doc` の統合テスト。一時パッケージにドキュメントを生成して内容・リンク・決定性を検査する。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ksn")).args(args).current_dir(cwd).output().expect("spawn ksn")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str, src: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_doc_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), TOML).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(d.join("src").join("t.ksnl"), src).unwrap();
        // 書き方は 1 通り (E0020): 試験の入力は `ksn fmt` の出力にそろえる。
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_ksn")).arg("fmt").current_dir(&d).output();
        Tmp(d)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const SRC: &str = "\
/// 面積を持つ図形。 <b>&amp;</b> \"q\"
trait t.Area pub
    fn area(self: &Self) -> i64 uses [] panics never
end t.Area

/// 正方形 (`side` は辺の長さ)。
type t.Square pub
    side: i64
end t.Square

impl t.Area for t.Square
/// 面積を返す。
fn t.Square.area(self: &t.Square) -> i64 uses [] panics never impls t.Area
    self.side.wrapping_mul(self.side)
end t.Square.area

/// 加算 <script>alert(1)</script> & \"引用\"
///
/// - 項目 1
fn t.add(a: i32, b: i32) -> i32 uses [] panics never pub
    a.wrapping_add(b)
end t.add

/// 先頭を取る。
fn t.first(xs: &[u8]) -> u8 uses [] panics may stack 64 pub
    xs[0usize]
end t.first

fn t.secret(a: i32) -> i32
    t.add(a, 1i32)
end t.secret

test t.add.test_basic
    core.check.eq(t.add(1i32, 2i32), 3i32)
end t.add.test_basic

test t.secret.test_one
    core.check.eq(t.secret(1i32), 2i32)
end t.secret.test_one
";

fn walk(d: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = fs::read_dir(d) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}

/// 生成された (相対パス, 内容) の一覧 (パス順)。
fn files(t: &Tmp) -> Vec<(String, String)> {
    let d = t.0.join("target").join("doc");
    let mut ps = vec![];
    walk(&d, &mut ps);
    let mut v: Vec<(String, String)> = ps
        .iter()
        .map(|p| (p.strip_prefix(&d).unwrap().to_string_lossy().replace('\\', "/"), fs::read_to_string(p).unwrap()))
        .collect();
    v.sort();
    v
}

fn file(t: &Tmp, n: &str) -> String {
    fs::read_to_string(t.0.join("target").join("doc").join(n)).unwrap_or_else(|_| panic!("missing {}", n))
}

fn strip_tags(s: &str) -> String {
    let mut o = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => o.push(c),
            _ => {}
        }
    }
    o.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

fn ids_of(html: &str) -> Vec<String> {
    html.split(" id=\"").skip(1).map(|p| p.split('"').next().unwrap().to_string()).collect()
}

/// 全ファイルの `href` が、存在するファイル・アンカーを指すこと。
fn assert_links_resolve(fs_: &[(String, String)]) {
    let ids: BTreeMap<&str, Vec<String>> = fs_.iter().map(|(n, h)| (n.as_str(), ids_of(h))).collect();
    let mut checked = 0;
    for (name, html) in fs_ {
        let dir = name.rsplit_once('/').map(|(d, _)| format!("{}/", d)).unwrap_or_default();
        for part in html.split(" href=\"").skip(1) {
            let h = part.split('"').next().unwrap();
            let (f, frag) = match h.split_once('#') {
                Some((f, a)) => (f, Some(a)),
                None => (h, None),
            };
            let target = if f.is_empty() { name.clone() } else { format!("{}{}", dir, f) };
            let list = ids.get(target.as_str()).unwrap_or_else(|| panic!("{}: dangling file link {}", name, h));
            if let Some(a) = frag {
                assert!(list.iter().any(|i| i == a), "{}: dangling anchor {}", name, h);
            }
            checked += 1;
        }
    }
    assert!(checked > 0);
}

#[test]
fn doc_pages_show_contract_tests_and_escape_text() {
    let t = Tmp::new("a", SRC);
    let o = run(&["doc"], &t.0);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(text(&o.stdout).trim().ends_with("target/doc/index.html"), "{}", text(&o.stdout));
    let index = file(&t, "index.html");
    assert!(index.starts_with("<!DOCTYPE html>") && index.contains("href=\"t.html\""));
    let m = file(&t, "t.html");
    let plain = strip_tags(&m);
    assert!(plain.contains("fn t.add(a: i32, b: i32) -> i32 uses [] panics never pub"));
    assert!(m.contains("class=\"tag pure\"") && m.contains("class=\"tag never\""));
    assert!(m.contains("class=\"tag may\"") && plain.contains("panics may") && plain.contains("stack 64"));
    // 非 pub は既定では出ない
    assert!(!plain.contains("t.secret") && !plain.contains("test_one"));
    // テスト本体
    assert!(plain.contains("core.check.eq(t.add(1i32, 2i32), 3i32)"));
    // doc コメントのエスケープ
    assert!(!m.contains("<script>alert") && !m.contains("<b>&amp;</b>"));
    assert!(m.contains("&lt;script&gt;alert(1)&lt;/script&gt; &amp; &quot;引用&quot;"));
    assert!(m.contains("<li>項目 1</li>") && m.contains("<code>side</code>"));
    // 型の全体定義・impl・メソッド・トレイト側の実装一覧
    assert!(plain.contains("end t.Square"));
    assert!(plain.contains("impl t.Area for t.Square"));
    assert!(plain.contains("fn t.Square.area(") && plain.contains("impls t.Area"));
    assert!(m.contains("実装している型"));
    // --private で非 pub と推論された契約が出る
    assert!(run(&["doc", "--private"], &t.0).status.success());
    let raw = file(&t, "t.html");
    let m = strip_tags(&raw);
    assert!(m.contains("fn t.secret(a: i32) -> i32"));
    assert!(m.contains("core.check.eq(t.secret(1i32), 2i32)"));
    assert!(raw.contains("class=\"tag infer\"") && raw.contains("class=\"tag pure inf\""));
}

#[test]
fn doc_links_resolve_and_output_is_deterministic() {
    let t = Tmp::new("b", SRC);
    assert!(run(&["doc", "--private"], &t.0).status.success());
    let a = files(&t);
    assert_links_resolve(&a);
    assert!(run(&["doc", "--private"], &t.0).status.success());
    assert_eq!(a, files(&t));
}

#[test]
fn doc_std_documents_the_standard_library_and_links_into_it() {
    let t = Tmp::new("c", SRC);
    let o = run(&["doc", "--std"], &t.0);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let opt = file(&t, "std/core.option.html");
    assert!(strip_tags(&opt).contains("fn core.option.Option.map["));
    assert!(opt.contains("id=\"core.option.Option.map\""));
    // 標準ライブラリ生成後は、パッケージ側から std のページへリンクが張られて全リンクが解決する
    assert!(run(&["doc"], &t.0).status.success());
    assert!(file(&t, "index.html").contains("std/index.html"));
    let a = files(&t);
    assert_links_resolve(&a);
    assert!(run(&["doc", "--std"], &t.0).status.success());
    assert_eq!(a, files(&t));
}

#[test]
fn doc_rejects_unknown_options() {
    let t = Tmp::new("d", SRC);
    assert_eq!(run(&["doc", "--nope"], &t.0).status.code(), Some(2));
}
