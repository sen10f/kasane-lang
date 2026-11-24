//! `String` をキーにした `HashMap` / `BtreeMap`、文字列リテラルの `match`、セルフホスト試作 (`selfhost/README.md` 5 章) で
//! 下りなかった形のネイティブ (KIR → LLVM) 生成。木走査 (`ksn run --engine=tree`) と `ksn build --release`
//! (退避しない。下りなければ失敗する) の出力が一致することを見る。

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

fn toml(name: &str, entry: &str) -> String {
    format!("[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"{}\"\n\n[dependencies]\n", name, entry)
}

struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str, src: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_strkeys_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), toml("t", "t.main")).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(d.join("src").join("t.ksnl"), src).unwrap();
        // 書き方は 1 通り (E0020): 試験の入力は `ksn fmt` の出力にそろえる。
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_ksn")).arg("fmt").current_dir(&d).output();
        // 効果の宣言は本体の効果と完全に一致させる (D5・D13): 固定の見出し (`main_wrap`) などを直す。
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_ksn")).arg("fix").env("KSN_FIX_ONLY", "E4020,E4025,E2055,E2057,E2058,E2059,E2060,E2061,E2062,E2063,E2064,E2065,E2066,E2067,E2068,E2069,E2070,E2071,E2072,E2073").current_dir(&d).output();
        Tmp(d)
    }
    /// `selfhost/probes/<name>` を一時ディレクトリへ複製する (元は変えない)。
    fn probe(name: &str) -> Tmp {
        let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../selfhost/probes").join(name);
        let d = std::env::temp_dir().join(format!("ksn_strkeys_probe_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        copy_dir(&from, &d);
        Tmp(d)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        scratch::remove(&self.0);
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        let q = to.join(e.file_name());
        if p.is_dir() {
            if e.file_name() != "target" {
                copy_dir(&p, &q);
            }
        } else {
            fs::copy(&p, &q).unwrap();
        }
    }
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(exe()).args(args).current_dir(cwd).output().expect("spawn ksn")
}

fn native_path(t: &Tmp) -> PathBuf {
    let dir = t.0.join("target/release");
    let name = fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).find(|n| n == "t");
    dir.join(name.expect("native executable"))
}

/// `ksn build --release` (退避なし) で実行体を作って走らせ、(終了コード, 標準出力) を返す。
fn native(t: &Tmp, name: &str) -> (Option<i32>, String) {
    let cache = scratch::Scratch::at(format!("ksn_strkeys_cache_{}_{}", std::process::id(), name));
    let b = Command::new(exe()).args(["build", "--release"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    assert!(b.status.success(), "build: {}{}", text(&b.stdout), text(&b.stderr));
    let p = native_path(t);
    let o = Command::new(&p).output().unwrap();
    asan(t, name, &o);
    (o.status.code(), text(&o.stdout))
}

/// clang が ASan を使えるとき (`KSN_NO_ASAN=1` で無効)、`-fsanitize=address` の実行体でも走らせ、報告 (二重解放・漏れ) がなく
/// 出力が同じであることを確かめる。
fn asan(t: &Tmp, name: &str, plain: &Output) {
    if std::env::var("KSN_NO_ASAN").is_ok_and(|v| v == "1") {
        return;
    }
    let cache = scratch::Scratch::at(format!("ksn_strkeys_asan_cache_{}_{}", std::process::id(), name));
    let args = "-fsanitize=address -fno-omit-frame-pointer -g -O0";
    let b = Command::new(exe()).args(["build", "--release"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).env("KSN_CLANG_ARGS", args).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    if !b.status.success() {
        return; // この clang は ASan を使えない
    }
    let p = native_path(t);
    let o = Command::new(&p).env("ASAN_OPTIONS", "detect_leaks=1").output().unwrap();
    assert_eq!(o.status.code(), plain.status.code(), "asan: {}{}", text(&o.stdout), text(&o.stderr));
    assert!(!text(&o.stderr).contains("ERROR: "), "asan: {}", text(&o.stderr));
    assert_eq!(text(&o.stdout), text(&plain.stdout), "asan の出力が違う");
}

/// 木走査と、ネイティブ (退避なし) の出力が同じことを確かめて、その出力を返す。
fn same(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    let chk = run(&["check"], &t.0);
    assert!(text(&chk.stdout).trim().is_empty() && chk.status.success(), "check: {}{}", text(&chk.stdout), text(&chk.stderr));
    let tree = run(&["run", "--engine=tree"], &t.0);
    assert_eq!(tree.status.code(), Some(0), "tree: {}{}", text(&tree.stdout), text(&tree.stderr));
    let (code, out) = native(&t, name);
    assert_eq!(code, Some(0), "native: {}", out);
    assert_eq!(text(&tree.stdout), out, "build の出力が run と違う");
    out
}

const MAIN: &str = "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    t.run()\nend t.main\n\n";

fn prog(body: &str) -> String {
    format!("{}{}", MAIN, body)
}

#[test]
fn hashmap_with_string_keys() {
    let out = same(
        "hm_string",
        &prog(
            r#"fn t.mk(text: &str) -> alloc.string.String uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(text))
    s
end t.mk

fn t.early(flag: bool) -> usize uses [alloc] panics never
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    core.mem.drop(m.insert(t.mk("a"), 1u64))
    if flag {
        return m.len()
    }
    core.mem.drop(m.insert(t.mk("b"), 2u64))
    m.len()
end t.early

fn t.run() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    core.mem.drop(m.insert(t.mk("fn"), 1u64))
    core.mem.drop(m.insert(t.mk("type"), 2u64))
    core.mem.drop(m.insert(t.mk("fn"), 3u64))
    let mut i = 0u64
    while i < 40u64 {
        let mut k = t.mk("key")
        core.mem.drop(k.push_u64(i))
        core.mem.drop(m.insert(k, i.wrapping_mul(7u64)))
        i = i.wrapping_add(1u64)
    }
    core.check.eq(m.len(), 42usize)
    let probe = t.mk("fn")
    core.check.eq(m.get_str(probe.as_str()), Some(&3u64))
    core.check.eq(m.contains_key_str("type"), true)
    core.check.eq(m.contains_key_str("nope"), false)
    core.check.eq(m.remove_str("type"), Some(2u64))
    core.check.eq(m.remove_str("type"), None)
    core.check.eq(m.len(), 41usize)
    let k7 = t.mk("key7")
    match m.get_str(k7.as_str()) {
        Some(v) => core.check.eq(*v, 49u64)
        None => core.check.eq(true, false)
    }
    let mut sum = 0u64
    let mut klen = 0usize
    for (k, v) in m.iter() {
        sum = sum.wrapping_add(*v)
        klen = klen.wrapping_add(k.len())
    }
    core.check.eq(sum, 5463u64)
    core.check.eq(klen, 192usize)
    core.check.eq(t.early(true), 1usize)
    core.check.eq(t.early(false), 2usize)
    m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    core.check.eq(m.len(), 0usize)
    core.mem.drop(m.insert(t.mk("again"), 1u64))
    core.check.eq(m.get_str("again"), Some(&1u64))
    core.mem.drop(std.io.print_line("sum"))
    Ok(())
end t.run
"#,
        ),
    );
    assert!(out.contains("sum"), "{}", out);
}

const MK: &str = r#"fn t.mk(text: &str) -> alloc.string.String uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(text))
    s
end t.mk

"#;

#[test]
fn btreemap_with_string_keys() {
    let body = format!(
        "{}{}",
        MK,
        r#"fn t.run() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut m = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)
    core.mem.drop(m.insert(t.mk("pear"), 4u32))
    core.mem.drop(m.insert(t.mk("apple"), 1u32))
    core.mem.drop(m.insert(t.mk("fig"), 3u32))
    core.mem.drop(m.insert(t.mk("apple"), 10u32))
    core.mem.drop(m.insert(t.mk("app"), 2u32))
    core.mem.drop(m.insert(t.mk(""), 7u32))
    let mut i = 0u64
    while i < 30u64 {
        let mut k = t.mk("k")
        core.mem.drop(k.push_u64(i.wrapping_mul(37u64) % 101u64))
        core.mem.drop(m.insert(k, 5u32))
        i = i.wrapping_add(1u64)
    }
    core.check.eq(m.len(), 35usize)
    let a = t.mk("apple")
    core.check.eq(m.get(&a), Some(&10u32))
    core.check.eq(m.get(&t.mk("app")), Some(&2u32))
    core.check.eq(m.get(&t.mk("")), Some(&7u32))
    core.check.eq(m.get(&t.mk("zebra")), None)
    core.check.eq(m.contains_key(&t.mk("fig")), true)
    core.check.eq(m.remove(&t.mk("fig")), Some(3u32))
    core.check.eq(m.remove(&t.mk("fig")), None)
    core.check.eq(m.contains_key(&t.mk("fig")), false)
    core.check.eq(m.len(), 34usize)
    core.mem.drop(std.io.print_line("btree ok"))
    Ok(())
end t.run
"#
    );
    assert_eq!(same("bm_string", &prog(&body)).trim(), "btree ok");
}

#[test]
fn btreemap_and_hashmap_with_str_keys() {
    let out = same(
        "bm_str",
        &prog(
            r#"fn t.run() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut b = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)
    core.mem.drop(b.insert("b", 2u32))
    core.mem.drop(b.insert("a", 1u32))
    core.mem.drop(b.insert("c", 3u32))
    core.check.eq(b.get(&"a"), Some(&1u32))
    core.check.eq(b.get(&"d"), None)
    core.check.eq(b.remove(&"b"), Some(2u32))
    core.check.eq(b.len(), 2usize)
    let mut h = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    core.mem.drop(h.insert("x", 9u32))
    core.check.eq(h.get(&"x"), Some(&9u32))
    core.mem.drop(std.io.print_line("str ok"))
    Ok(())
end t.run
"#,
        ),
    );
    assert_eq!(out.trim(), "str ok");
}

#[test]
fn string_order_and_cmp() {
    let body = format!(
        "{}{}",
        MK,
        r#"fn t.name(o: core.cmp.Ordering) -> &'static str uses [] panics never
    match o {
        core.cmp.Ordering.Less => "less"
        core.cmp.Ordering.Equal => "equal"
        core.cmp.Ordering.Greater => "greater"
    }
end t.name

fn t.run() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let a = t.mk("apple")
    let b = t.mk("banana")
    let a2 = t.mk("app")
    let e = t.mk("")
    core.check.eq(a < b, true)
    core.check.eq(b < a, false)
    core.check.eq(a <= a, true)
    core.check.eq(a > a2, true)
    core.check.eq(a2 >= a, false)
    core.check.eq(e < a2, true)
    core.check.eq("a" < "b", true)
    core.check.eq("b" <= "a", false)
    core.check.eq("é" > "z", true)
    core.mem.drop(std.io.print_line(t.name(a.cmp(&b))))
    core.mem.drop(std.io.print_line(t.name(b.cmp(&a))))
    core.mem.drop(std.io.print_line(t.name(a.cmp(&a))))
    core.mem.drop(std.io.print_line(t.name(a2.cmp(&a))))
    core.mem.drop(std.io.print_line(t.name("zz".cmp("z"))))
    core.mem.drop(match a.partial_cmp(&b) {
        Some(o) => std.io.print_line(t.name(o))
        None => std.io.print_line("none")
    })
    core.check.eq(a.cmp(&b), core.cmp.Ordering.Less)
    Ok(())
end t.run
"#
    );
    let out = same("str_order", &prog(&body));
    assert_eq!(out.trim(), "less\ngreater\nequal\nless\ngreater\nless");
}

/// `selfhost/probes/<name>` の複製を、木走査とネイティブ (退避なし) で走らせて出力を比べる。
fn probe_same(name: &str) -> String {
    let t = Tmp::probe(name);
    let tree = run(&["run", "--engine=tree"], &t.0);
    assert_eq!(tree.status.code(), Some(0), "tree: {}{}", text(&tree.stdout), text(&tree.stderr));
    let (code, out) = native(&t, name);
    assert_eq!(code, Some(0), "native: {}", out);
    assert_eq!(text(&tree.stdout), out, "build の出力が run と違う");
    out
}

#[test]
fn selfhost_probe_hashmap_builds_natively() {
    assert_eq!(probe_same("hashmap").trim(), "found");
}

#[test]
fn selfhost_probe_strmatch_builds_natively() {
    assert_eq!(probe_same("strmatch").trim(), "ok");
}

#[test]
fn string_literal_match() {
    let out = same(
        "str_match",
        &prog(
            r#"fn t.kw(s: &str) -> u32 uses [] panics never
    match s {
        "fn" => 1u32
        "type" => 2u32
        "" => 9u32
        "end" => 3u32
        _ => 0u32
    }
end t.kw

fn t.pick(o: Option<&str>) -> u32 uses [] panics never
    match o {
        Some("a") => 10u32
        Some("b") => 20u32
        Some(_) => 30u32
        None => 40u32
    }
end t.pick

fn t.rest(s: &str) -> u32 uses [] panics never
    match s {
        "x" => 1u32
        _other => 7u32
    }
end t.rest

fn t.run() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.kw("fn"), 1u32)
    core.check.eq(t.kw("type"), 2u32)
    core.check.eq(t.kw("end"), 3u32)
    core.check.eq(t.kw(""), 9u32)
    core.check.eq(t.kw("fnx"), 0u32)
    core.check.eq(t.kw("f"), 0u32)
    core.check.eq(t.pick(Some("a")), 10u32)
    core.check.eq(t.pick(Some("b")), 20u32)
    core.check.eq(t.pick(Some("c")), 30u32)
    core.check.eq(t.pick(None), 40u32)
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str("type"))
    core.mem.drop(match s.as_str() {
        "type" => std.io.print_line("matched type")
        _ => std.io.print_line("no match")
    })
    Ok(())
end t.run
"#,
        ),
    );
    assert_eq!(out.trim(), "matched type");
}

/// 5 章 1: `const` の `&str` への直接のメソッド呼び出し。
#[test]
fn const_str_methods() {
    let out = same(
        "const_str",
        &prog(
            r#"const t.OPS: &str = "()[]+-*/" pub
const t.SUFFIXES: &str = "u8 u16 u32" pub

fn t.is_op(c: u8) -> bool uses [] panics never
    t.OPS.as_bytes().contains(&c)
end t.is_op

fn t.count() -> usize uses [] panics never
    let mut n = 0usize
    for s in t.SUFFIXES.split(" ") {
        n = n.wrapping_add(s.len())
    }
    n
end t.count

fn t.run() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.is_op(b'+'), true)
    core.check.eq(t.is_op(b'a'), false)
    core.check.eq(t.count(), 8usize)
    core.check.eq(t.OPS.len(), 8usize)
    core.check.eq(t.OPS.starts_with("()"), true)
    core.mem.drop(std.io.print_line("const ok"))
    Ok(())
end t.run
"#,
        ),
    );
    assert_eq!(out.trim(), "const ok");
}

const TOK: &str = r#"type t.Tok derive [Debug, Clone, Copy, PartialEq] pub
    code: u32 pub
    stop: usize pub
    line: usize pub
end t.Tok

type t.Ast pub
    nodes: alloc.vec.Vec<t.Tok, std.alloc.System> pub
    defs: alloc.vec.Vec<usize, std.alloc.System> pub
end t.Ast

type t.P pub
    ast: t.Ast pub
end t.P

fn t.tok(code: u32, stop: usize, line: usize) -> t.Tok uses [] panics never
    t.Tok { code: code, stop: stop, line: line }
end t.tok

"#;

/// 5 章 2: `Copy` の構造体の参照外し `*t`。
#[test]
fn deref_copy_struct() {
    let body = format!(
        "{}{}",
        TOK,
        r#"fn t.last(v: &alloc.vec.Vec<t.Tok, std.alloc.System>) -> t.Tok uses [] panics never
    match v.last() {
        Some(t) => *t
        None => t.tok(0u32, 0usize, 0usize)
    }
end t.last

fn t.first(v: &alloc.vec.Vec<t.Tok, std.alloc.System>) -> t.Tok uses [] panics never
    match v.get(0usize) {
        Some(n) => *n
        None => t.tok(0u32, 0usize, 0usize)
    }
end t.first

fn t.run() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.check.eq(t.last(&v).code, 0u32)
    core.mem.drop(v.push(t.tok(1u32, 10usize, 1usize)))
    core.mem.drop(v.push(t.tok(2u32, 20usize, 2usize)))
    let l = t.last(&v)
    core.check.eq(l.code, 2u32)
    core.check.eq(l.stop, 20usize)
    core.check.eq(t.first(&v), t.tok(1u32, 10usize, 1usize))
    let copy = *v.get(1usize).unwrap()
    core.check.eq(copy.line, 2usize)
    core.mem.drop(std.io.print_line("deref ok"))
    Ok(())
end t.run
"#
    );
    assert_eq!(same("deref_copy", &prog(&body)).trim(), "deref ok");
}

/// 5 章 3: フィールド経由の構造体の `Vec` の `for`。
#[test]
fn for_over_a_struct_vec_through_a_field() {
    let body = format!(
        "{}{}",
        TOK,
        r#"fn t.sum(p: &t.P) -> usize uses [] panics never
    let mut total = 0usize
    for n in p.ast.nodes.as_slice().iter() {
        total = total.wrapping_add(n.stop)
    }
    for d in p.ast.defs.as_slice().iter() {
        total = total.wrapping_add(*d)
    }
    total
end t.sum

fn t.run() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let nodes = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let defs = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let ast = t.Ast { nodes: nodes, defs: defs }
    let mut p = t.P { ast: ast }
    core.mem.drop(p.ast.nodes.push(t.tok(1u32, 10usize, 1usize)))
    core.mem.drop(p.ast.nodes.push(t.tok(2u32, 20usize, 2usize)))
    core.mem.drop(p.ast.defs.push(5usize))
    core.check.eq(t.sum(&p), 35usize)
    let mut n = 0u32
    for x in p.ast.nodes.as_slice().iter() {
        n = n.wrapping_add(x.code)
    }
    core.check.eq(n, 3u32)
    core.mem.drop(std.io.print_line("for ok"))
    Ok(())
end t.run
"#
    );
    assert_eq!(same("for_field", &prog(&body)).trim(), "for ok");
}

/// 5 章 4: `match` の束縛 `String` への `as_str`。
#[test]
fn as_str_on_a_matched_string() {
    let body = format!(
        "{}{}",
        MK,
        r#"fn t.run() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let key = alloc.string.String.from_str(std.alloc.SYSTEM, "hello world")
    match key {
        Ok(text) => {
            core.check.eq(text.as_str().starts_with("hello"), true)
            core.check.eq(text.as_str().len(), 11usize)
            core.check.eq(text.as_str() == "hello world", true)
        }
        Err(_e) => core.check.eq(true, false)
    }
    core.mem.drop(std.io.print_line("as_str ok"))
    Ok(())
end t.run
"#
    );
    assert_eq!(same("as_str_match", &prog(&body)).trim(), "as_str ok");
}

/// 5 章 4 (本物の形): `Ok(E.V(text))` で束縛した `String` への `as_str`。関数と `test` の両方。`test --both` は LLVM で比べる。
#[test]
fn as_str_on_a_string_bound_in_a_nested_pattern() {
    let src = r#"enum t.Outcome pub
    Done(alloc.string.String<std.alloc.System>)
    Bad(usize)
end t.Outcome

fn t.check(ok: bool) -> usize uses [alloc] panics may
    match t.make(ok) {
        Ok(t.Outcome.Done(text)) => {
            core.check.eq(text.as_str().starts_with("== t\na"), true)
            text.as_str().len()
        }
        Ok(t.Outcome.Bad(n)) => n
        Err(_e) => 0usize
    }
end t.check

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.check(true), 8usize)
    core.check.eq(t.check(false), 3usize)
    std.io.print_line("nested ok")
end t.main

fn t.make(ok: bool) -> Result<t.Outcome, alloc.alloc.AllocError> uses [alloc] panics never
    if ok {
        let s = alloc.string.String.from_str(std.alloc.SYSTEM, "== t\nabc")?
        return Ok(t.Outcome.Done(s))
    }
    Ok(t.Outcome.Bad(3usize))
end t.make

test t.make.test_done
    match t.make(true) {
        Ok(t.Outcome.Done(text)) => {
            core.check.eq(text.as_str().starts_with("== t\na"), true)
        }
        Ok(t.Outcome.Bad(_a)) => core.check.eq(1u8, 2u8)
        Err(_e) => core.check.eq(1u8, 2u8)
    }
end t.make.test_done
"#;
    assert_eq!(same("nested_as_str", src).trim(), "nested ok");
    let t = Tmp::new("nested_as_str_test", src);
    let o = run(&["test", "--both"], &t.0);
    let out = format!("{}{}", text(&o.stdout), text(&o.stderr));
    assert!(o.status.success(), "{}", out);
    assert!(out.contains("(both: llvm)") && !out.contains("bundle ("), "{}", out);
}

#[test]
fn hashmap_remove_tombstones_keep_insertion_order() {
    let out = same(
        "hm_tomb",
        &prog(
            r#"fn t.order(m: &alloc.collections.HashMap<u64, u64, std.alloc.System>) -> u64 uses [] panics never
    let mut acc = 0u64
    for (k, v) in m.iter() {
        acc = acc.wrapping_mul(1_000_003u64).wrapping_add(*k).wrapping_mul(31u64).wrapping_add(*v)
    }
    acc
end t.order

fn t.show(n: u64) -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_u64(n))
    std.io.print_line(s.as_str())
end t.show

fn t.run() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut i = 0u64
    while i < 5000u64 {
        core.mem.drop(m.insert(i, i.wrapping_mul(3u64)))
        i = i.wrapping_add(1u64)
    }
    core.check.eq(m.len(), 5000usize)
    i = 0u64
    while i < 5000u64 {
        if i % 3u64 != 0u64 {
            core.check.eq(m.remove(&i), Some(i.wrapping_mul(3u64)))
        }
        i = i.wrapping_add(1u64)
    }
    core.check.eq(m.len(), 1667usize)
    core.check.eq(m.remove(&1u64), None)
    core.check.eq(m.get(&3u64), Some(&9u64))
    core.check.eq(m.get(&4u64), None)
    core.mem.drop(t.show(t.order(&m)))
    i = 0u64
    while i < 3000u64 {
        core.mem.drop(m.insert(i, i))
        i = i.wrapping_add(1u64)
    }
    core.mem.drop(t.show(t.order(&m)))
    i = 0u64
    while i < 20000u64 {
        core.mem.drop(m.insert(i.wrapping_add(10_000u64), i))
        if i % 2u64 == 0u64 {
            core.mem.drop(m.remove(&i))
        }
        i = i.wrapping_add(1u64)
    }
    core.mem.drop(t.show(t.order(&m)))
    core.mem.drop(t.show(u64.from_usize(m.len())))
    let mut c = 0u64
    while c < 100u64 {
        core.mem.drop(m.remove(&c))
        core.mem.drop(m.insert(c, c))
        c = c.wrapping_add(1u64)
    }
    core.mem.drop(t.show(t.order(&m)))
    Ok(())
end t.run
"#,
        ),
    );
    assert!(out.lines().count() >= 4, "{}", out);
}
