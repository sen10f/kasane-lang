//! `ksn sync`: LLM が変えた `.ksnl` に、人の `.ksnh` のコメントを付け直す (設計_ksnhとロック.md 2 章)。

mod common;
use common::*;
use std::fs;

fn f(name: &str, body: &[&str]) -> String {
    let b: String = body.iter().map(|l| format!("    {}\n", l)).collect();
    format!("fn t.{n}() -> () uses [] panics never\n{b}end t.{n}\n", n = name, b = b)
}

const HUMAN: &str = "/// module doc

// a doc
fn t.a() -> () uses [] panics never
    // first
    core.mem.drop(1u8)
    // second
    core.mem.drop(2u8)
    // third
    core.mem.drop(3u8)
end t.a

// b doc
fn t.b() -> () uses [] panics never
    core.mem.drop(9u8)
end t.b

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main
";

fn setup(name: &str) -> Tmp {
    let t = Tmp::new_ksnh(name, HUMAN);
    assert_eq!(run(&["strip"], &t.0).status.code(), Some(0));
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0), "{}", text(&run(&["check"], &t.0).stdout));
    t
}

fn rd(t: &Tmp, n: &str) -> String {
    fs::read_to_string(t.0.join("src").join(n)).unwrap()
}

fn llm(t: &Tmp, from: &str, to: &str) {
    let l = rd(t, "t.ksnl");
    assert!(l.contains(from), "{}", from);
    fs::write(t.0.join("src/t.ksnl"), l.replacen(from, to, 1)).unwrap();
}

fn sync(t: &Tmp, args: &[&str]) -> (i32, String) {
    let mut a = vec!["sync"];
    a.extend_from_slice(args);
    let o = run(&a, &t.0);
    (o.status.code().unwrap(), format!("{}{}", text(&o.stdout), text(&o.stderr)))
}

fn assert_consistent(t: &Tmp) {
    let o = text(&run(&["check"], &t.0).stdout);
    assert!(!o.contains("E9050") && !o.contains("E0020"), "{}", o);
}

#[test]
fn unchanged_is_up_to_date() {
    let t = setup("sync_nochange");
    let (rc, out) = sync(&t, &[]);
    assert_eq!(rc, 0, "{}", out);
    assert!(out.contains("up to date"), "{}", out);
}

#[test]
fn insert_statement_keeps_comments_on_their_statements() {
    let t = setup("sync_insert");
    llm(&t, "    core.mem.drop(2u8)\n", "    core.mem.drop(7u8)\n    core.mem.drop(2u8)\n");
    // E9050 の文面に sync が出る。
    assert!(text(&run(&["check"], &t.0).stdout).contains("ksn sync"));
    let (rc, out) = sync(&t, &[]);
    assert_eq!(rc, 0, "{}", out);
    let h = rd(&t, "t.ksnh");
    assert!(h.contains("    // second\n    core.mem.drop(2u8)\n"), "{}", h);
    assert!(h.contains("    // first\n    core.mem.drop(1u8)\n    core.mem.drop(7u8)\n    // second\n"), "{}", h);
    assert!(h.starts_with("/// module doc\n\n// a doc\nfn t.a()"), "{}", h);
    assert!(h.contains("// b doc\nfn t.b()"));
    assert_consistent(&t);
}

#[test]
fn delete_statement_reports_lost_and_writes_nothing_by_default() {
    let t = setup("sync_lost");
    llm(&t, "    core.mem.drop(2u8)\n", "");
    let before = rd(&t, "t.ksnh");
    let (rc, out) = sync(&t, &[]);
    assert_eq!(rc, 1, "{}", out);
    assert!(out.contains("lost comment: t.a") && out.contains("second") && out.contains("文が消えた"), "{}", out);
    assert_eq!(rd(&t, "t.ksnh"), before);
    assert!(t.0.join("target/sync/lost/t.json").exists());
    // --drop-lost: コメントを捨てて進める。
    let (rc, out) = sync(&t, &["--drop-lost"]);
    assert_eq!(rc, 0, "{}", out);
    let h = rd(&t, "t.ksnh");
    assert!(!h.contains("second") && h.contains("// first") && h.contains("// third\n    core.mem.drop(3u8)"), "{}", h);
    assert_consistent(&t);
}

#[test]
fn keep_lost_collects_comments_before_the_definition() {
    let t = setup("sync_keep");
    llm(&t, "    core.mem.drop(2u8)\n", "");
    let (rc, out) = sync(&t, &["--keep-lost"]);
    assert_eq!(rc, 0, "{}", out);
    let h = rd(&t, "t.ksnh");
    assert!(h.contains("// LOST (消えた t.a): second\nfn t.a()"), "{}", h);
    assert_consistent(&t);
}

#[test]
fn deleted_definition_loses_its_comments() {
    let t = setup("sync_deldef");
    let l = rd(&t, "t.ksnl").replace(&f("b", &["core.mem.drop(9u8)"]), "").replace("\n\n\n", "\n\n");
    fs::write(t.0.join("src/t.ksnl"), l).unwrap();
    let (rc, out) = sync(&t, &[]);
    assert_eq!(rc, 1, "{}", out);
    assert!(out.contains("lost comment: t.b") && out.contains("b doc") && out.contains("定義が消えた"), "{}", out);
    let (rc, out) = sync(&t, &["--drop-lost"]);
    assert_eq!(rc, 0, "{}", out);
    assert!(!rd(&t, "t.ksnh").contains("t.b"));
    assert_consistent(&t);
}

#[test]
fn add_definitions_keeps_comments_on_the_others() {
    // 定義の並びは `ksn fmt` が決める (FQN のバイト順) ので、LLM は正規の順に足す。
    let t = setup("sync_add");
    let a = f("a", &["core.mem.drop(1u8)", "core.mem.drop(2u8)", "core.mem.drop(3u8)"]);
    let b = f("b", &["core.mem.drop(9u8)"]);
    let l = rd(&t, "t.ksnl");
    let l = l.replace(&format!("{}\n{}", a, b), &format!("{}\n{}\n{}\n{}", f("0z", &["core.mem.drop(4u8)"]), a, f("ab", &["core.mem.drop(5u8)"]), b));
    fs::write(t.0.join("src/t.ksnl"), l).unwrap();
    let (rc, out) = sync(&t, &[]);
    assert_eq!(rc, 0, "{}", out);
    let h = rd(&t, "t.ksnh");
    assert!(h.contains("// b doc\nfn t.b()") && h.contains("// a doc\nfn t.a()") && h.contains("    // third\n"), "{}", h);
    assert!(h.find("fn t.a(").unwrap() < h.find("fn t.ab(").unwrap() && h.find("fn t.ab(").unwrap() < h.find("fn t.b(").unwrap(), "{}", h);
    assert!(!h.contains("// LOST"), "{}", h);
    assert_consistent(&t);
}

#[test]
fn renamed_definition_moves_comments() {
    let t = setup("sync_rename");
    let l = rd(&t, "t.ksnl").replace("fn t.b()", "fn t.c()").replace("end t.b", "end t.c");
    fs::write(t.0.join("src/t.ksnl"), l).unwrap();
    let (rc, out) = sync(&t, &[]);
    assert_eq!(rc, 0, "{}", out);
    assert!(out.contains("renamed t.b -> t.c"), "{}", out);
    assert!(rd(&t, "t.ksnh").contains("// b doc\nfn t.c()"));
    assert_consistent(&t);
}

#[test]
fn nested_blocks_keep_comments_when_the_header_changes() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never
    // setup
    let x = 1u32
    if x == 1u32 {
        // inner
        core.mem.drop(1u8)
    }
    Ok(())
end t.main
";
    let t = Tmp::new_ksnh("sync_nested", src);
    assert_eq!(run(&["strip"], &t.0).status.code(), Some(0));
    llm(&t, "x == 1u32", "x == 2u32");
    let (rc, out) = sync(&t, &[]);
    assert_eq!(rc, 0, "{}", out);
    let h = rd(&t, "t.ksnh");
    assert!(h.contains("    if x == 2u32 {\n        // inner\n        core.mem.drop(1u8)\n"), "{}", h);
    assert!(h.contains("    // setup\n    let x = 1u32\n"), "{}", h);
}

#[test]
fn both_changed_the_same_definition_is_a_conflict() {
    let t = setup("sync_conflict");
    // 人: a の本体を .ksnh で変える。LLM: 同じ a を .ksnl で変える。b は LLM だけが変える。
    let h = rd(&t, "t.ksnh").replacen("core.mem.drop(1u8)", "core.mem.drop(11u8)", 1);
    fs::write(t.0.join("src/t.ksnh"), &h).unwrap();
    llm(&t, "core.mem.drop(3u8)", "core.mem.drop(33u8)");
    llm(&t, "core.mem.drop(9u8)", "core.mem.drop(99u8)");
    let before = (rd(&t, "t.ksnh"), rd(&t, "t.ksnl"));
    let (rc, out) = sync(&t, &[]);
    assert_eq!(rc, 1, "{}", out);
    assert!(out.contains("conflict: t.a"), "{}", out);
    assert_eq!((rd(&t, "t.ksnh"), rd(&t, "t.ksnl")), before);
    // 人を採る: .ksnl を .ksnh から作り直す。
    let (rc, out) = sync(&t, &["--take", "ksnh"]);
    assert_eq!(rc, 0, "{}", out);
    assert!(rd(&t, "t.ksnl").contains("drop(11u8)") && !rd(&t, "t.ksnl").contains("drop(33u8)"));
}

#[test]
fn take_ksnl_resolves_a_conflict_and_different_definitions_merge() {
    let t = setup("sync_merge");
    // 人は a、LLM は b を変える: 衝突しない。人の a の変更は残り、LLM の b の変更が入る。
    let h = rd(&t, "t.ksnh").replacen("core.mem.drop(1u8)", "core.mem.drop(11u8)", 1);
    fs::write(t.0.join("src/t.ksnh"), &h).unwrap();
    llm(&t, "core.mem.drop(9u8)", "core.mem.drop(99u8)");
    let (rc, out) = sync(&t, &[]);
    assert_eq!(rc, 0, "{}", out);
    let (nh, nl) = (rd(&t, "t.ksnh"), rd(&t, "t.ksnl"));
    assert!(nh.contains("    // first\n    core.mem.drop(11u8)") && nh.contains("// b doc\nfn t.b()") && nh.contains("drop(99u8)"), "{}", nh);
    assert!(nl.contains("drop(11u8)") && nl.contains("drop(99u8)"));
    assert_consistent(&t);
    // 同じ定義の衝突は --take ksnl で LLM を採る (コメントは付け直す)。
    let h = rd(&t, "t.ksnh").replacen("core.mem.drop(2u8)", "core.mem.drop(22u8)", 1);
    fs::write(t.0.join("src/t.ksnh"), &h).unwrap();
    llm(&t, "core.mem.drop(3u8)", "core.mem.drop(33u8)");
    assert_eq!(sync(&t, &[]).0, 1);
    let (rc, out) = sync(&t, &["--take", "ksnl"]);
    assert_eq!(rc, 1, "{}", out); // 人の drop(22u8) の行は LLM に無いので second のコメントは失われる
    assert!(out.contains("second"), "{}", out);
    let (rc, out) = sync(&t, &["--take", "ksnl", "--drop-lost"]);
    assert_eq!(rc, 0, "{}", out);
    let h = rd(&t, "t.ksnh");
    assert!(h.contains("    // first\n    core.mem.drop(11u8)") && h.contains("core.mem.drop(33u8)") && !h.contains("second"), "{}", h);
    assert_consistent(&t);
}

#[test]
fn no_base_and_different_files_is_a_conflict() {
    let t = setup("sync_nobase");
    let _ = fs::remove_dir_all(t.0.join("target/sync"));
    llm(&t, "core.mem.drop(9u8)", "core.mem.drop(99u8)");
    let (rc, out) = sync(&t, &[]);
    assert_eq!(rc, 1, "{}", out);
    assert!(out.contains("conflict"), "{}", out);
}
