//! `ksn run` (インタプリタ) のテスト。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use common::*;

#[test]
fn run_err_exits_1_with_debug_output() {
    let t = Tmp::new("err", "fn t.main() -> Result<(), u8> uses [] panics never\n    Err(7u8)\nend t.main\n");
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("Error: 7"));
}

#[test]
fn run_panic_exits_101() {
    let t = Tmp::new("panic", "fn t.main() -> Result<(), u8> uses [] panics may\n    core.panic.panic(\"boom\")\nend t.main\n");
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101));
    assert!(text(&o.stderr).contains("boom"));
}

#[test]
fn run_ok_exits_0_and_prints() {
    let t = Tmp::new("ok", "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    core.mem.drop(std.io.print_line(\"hi\"))\n    Ok(())\nend t.main\n");
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(text(&o.stdout).trim(), "hi");
}

#[test]
fn gap_f32_precision() {
    let b = "    let a = 16777216.0f32\n    let b = a + 1.0f32\n    core.check.eq(b, a)\n";
    let t = Tmp::new("gap_f32", &main_wrap("", b));
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
}

#[test]
fn gap_u128_max_overflow() {
    let b = "    core.check.is_true(core.num.u128.MAX.checked_add(1u128).is_none())\n";
    let t = Tmp::new("gap_u128", &main_wrap("", b));
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
}

#[test]
fn integer_overflow_panics_with_101() {
    let cases: [(&str, Vec<&str>); 5] = [
        ("ovf_add", vec!["let a = 250u8", "core.mem.drop(a + 10u8)"]),
        ("ovf_sub", vec!["let a = 1u8", "core.mem.drop(a - 2u8)"]),
        ("ovf_mul", vec!["let a = 2147483647i32", "core.mem.drop(a * 2i32)"]),
        ("ovf_div0", vec!["let a = 5u8", "let z = 0u8", "core.mem.drop(a / z)"]),
        ("ovf_neg", vec!["let a = -128i8", "core.mem.drop(-a)"]),
    ];
    for (name, lines) in cases {
        let t = Tmp::new(name, &main_wrap("", &body(&lines)));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(101), "{}: {}{}", name, text(&o.stdout), text(&o.stderr));
    }
}

// ---- 文脈キーワード (仕様 0.9): ヘッダ位置の語を識別子としても使える ----

#[test]
fn header_words_are_usable_as_identifiers() {
    let f = "fn t.a(stack: u8, link: u8) -> u8 uses [] panics never pub\n    let derive = stack\n    let layout = link\n    \
             derive.max(layout)\nend t.a\n\n";
    let b = body(&["core.check.eq(t.a(3u8, 9u8), 9u8)"]);
    let t = Tmp::new("ctx_kw", &main_wrap(f, &b));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

// ---- std.fs (インタプリタ) ----

#[test]
fn std_fs_writes_reads_and_removes_a_file() {
    let b = body(&[
        "let mut f = std.fs.create(\"ksn_fs_test.txt\")?",
        "let n = match f.write(b\"hello\") {",
        "    Ok(n) => n",
        "    Err(_e) => {",
        "        core.mem.drop(f.close())",
        "        return Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 1i32 })",
        "    }",
        "}",
        "core.check.eq(n, 5usize)",
        "f.close()?",
        "core.check.eq(std.fs.exists(\"ksn_fs_test.txt\"), true)",
        "let mut g = std.fs.open(\"ksn_fs_test.txt\")?",
        "let mut buf = alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "let m = match std.fs.read_to_end(&mut g, &mut buf) {",
        "    Ok(m) => m",
        "    Err(_e) => {",
        "        core.mem.drop(g.close())",
        "        return Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 2i32 })",
        "    }",
        "}",
        "core.check.eq(m, 5usize)",
        "core.check.eq(buf.len(), 5usize)",
        "g.close()?",
        "std.fs.remove_file(\"ksn_fs_test.txt\")?",
        "core.check.eq(std.fs.exists(\"ksn_fs_test.txt\"), false)",
    ]);
    let src = main_wrap("", &b).replace("uses [io.console] panics may", "uses [alloc, io.fs] panics may");
    let t = Tmp::new("fs_rw", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(!t.0.join("ksn_fs_test.txt").exists());
}

#[test]
fn std_fs_reports_a_missing_file_as_an_error_value() {
    let b = body(&[
        "match std.fs.open(\"no_such_file_ksn.txt\") {",
        "    Ok(f) => {",
        "        core.mem.drop(f.close())",
        "    }",
        "    Err(e) => {",
        "        core.check.eq(e.kind, std.io.ErrorKind.NotFound)",
        "    }",
        "}",
    ]);
    let src = main_wrap("", &b).replace("uses [io.console] panics may", "uses [io.fs] panics may");
    let t = Tmp::new("fs_missing", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

#[test]
fn trait_side_impl_rename_of_the_trait_method_follows_call_sites() {
    let t = tmp_files("trait_side_rename", &[("t.ksnl", TRAIT_SIDE_MAIN), ("t/tr.ksnl", TRAIT_SIDE_TR), ("t/ty.ksnl", TRAIT_SIDE_TY)]);
    let r = run(&["rename", "t.tr.Show.show", "t.tr.Show.shown"], &t.0);
    assert_eq!(r.status.code(), Some(0), "{}{}", text(&r.stdout), text(&r.stderr));
    assert_eq!(text(&run(&["run"], &t.0).stdout), "ok\n");
}

// ---- std.net (インタプリタ) ----

/// ポート番号を `127.0.0.1:<port>` に整形する補助 (テスト用ソースの共通部分)。
const NET_ADDR_FN: &str = r#"fn t.addr(port: u16) -> alloc.string.String<std.alloc.System> uses [alloc] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str("127.0.0.1:"))
    let n = usize.from_u16(port)
    let digits = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]
    let mut unit = 10000usize
    let mut started = false
    while unit > 0usize {
        let d = (n / unit) % 10usize
        if d > 0usize || started || unit == 1usize {
            started = true
            match digits.get(d) {
                Some(t) => core.mem.drop(s.push_str(*t))
                None => core.mem.drop(0u8)
            }
        }
        unit = unit / 10usize
    }
    s
end t.addr

"#;

#[test]
fn std_net_loopback_round_trip() {
    // ポート 0 で bind し、listen backlog のおかげで connect を accept より先に行える。
    let b = body(&[
        "let listener = match std.net.listen(\"127.0.0.1:0\") {",
        "    Ok(l) => l",
        "    Err(_e) => return Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 1i32 })",
        "}",
        "let port = match listener.local_port() {",
        "    Ok(p) => p",
        "    Err(_e) => 0u16",
        "}",
        "core.check.is_true(port > 0u16)",
        "let addr = t.addr(port)",
        "let mut client = match std.net.connect(addr.as_str()) {",
        "    Ok(c) => c",
        "    Err(_e) => {",
        "        core.mem.drop(listener.close())",
        "        return Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 2i32 })",
        "    }",
        "}",
        "let mut server = match listener.accept() {",
        "    Ok(s) => s",
        "    Err(_e) => {",
        "        core.mem.drop(client.close())",
        "        core.mem.drop(listener.close())",
        "        return Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 3i32 })",
        "    }",
        "}",
        "core.check.eq(client.write(b\"ping\").is_ok(), true)",
        "let mut buf = [0u8; 16]",
        "let n = match server.read(&mut buf) {",
        "    Ok(n) => n",
        "    Err(_e) => 0usize",
        "}",
        "core.check.eq(n, 4usize)",
        "core.check.eq(buf.get(0usize), Some(&112u8))",
        "core.check.eq(server.write(b\"pong!\").is_ok(), true)",
        "core.mem.drop(server.close())",
        "let mut buf2 = [0u8; 16]",
        "let m = match client.read(&mut buf2) {",
        "    Ok(m) => m",
        "    Err(_e) => 0usize",
        "}",
        "core.check.eq(m, 5usize)",
        "let eof = match client.read(&mut buf2) {",
        "    Ok(k) => k",
        "    Err(_e) => 99usize",
        "}",
        "core.check.eq(eof, 0usize)",
        "core.mem.drop(client.close())",
        "core.mem.drop(listener.close())",
    ]);
    let src = format!("{}{}", NET_ADDR_FN, main_wrap("", &b)).replace("uses [io.console] panics may", "uses [alloc, io.console, io.net] panics may");
    let t = Tmp::new("net_rt", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

#[test]
fn std_net_connect_to_a_closed_port_is_an_error_value() {
    // bind したポートを close してから connect する (そのポートは待ち受け無し)。
    let b = body(&[
        "let listener = match std.net.listen(\"127.0.0.1:0\") {",
        "    Ok(l) => l",
        "    Err(_e) => return Err(std.io.Error { kind: std.io.ErrorKind.Other, code: 1i32 })",
        "}",
        "let port = match listener.local_port() {",
        "    Ok(p) => p",
        "    Err(_e) => 0u16",
        "}",
        "core.mem.drop(listener.close())",
        "let addr = t.addr(port)",
        "match std.net.connect(addr.as_str()) {",
        "    Ok(c) => {",
        "        core.mem.drop(c.close())",
        "        core.panic.panic(\"connect to a closed port must fail\")",
        "    }",
        "    Err(e) => core.check.is_true(e.code != 0i32)",
        "}",
        "match std.net.connect(\"not an address\") {",
        "    Ok(c) => {",
        "        core.mem.drop(c.close())",
        "        core.panic.panic(\"bad address must fail\")",
        "    }",
        "    Err(_e) => core.mem.drop(0u8)",
        "}",
    ]);
    let src = format!("{}{}", NET_ADDR_FN, main_wrap("", &b)).replace("uses [io.console] panics may", "uses [alloc, io.console, io.net] panics may");
    let t = Tmp::new("net_closed", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

/// `Display` の char は文字そのもの。`print_line` と `to_string` の出力を確かめる。
#[test]
fn char_display_prints_the_character_itself() {
    let body = "    let c = 'x'\n    let mut f = core.fmt.Formatter.new(8usize)\n    core.mem.drop(c.display_fmt(&mut f))\n    match core.str.from_utf8(f.written()) {\n        Ok(s) => std.io.print_line(s)?\n        Err(_e) => std.io.print_line(\"bad\")?\n    }\n    let n = 7u8\n    let mut f2 = core.fmt.Formatter.new(4usize)\n    core.mem.drop(n.display_fmt(&mut f2))\n    match core.str.from_utf8(f2.written()) {\n        Ok(s) => std.io.print_line(s)?\n        Err(_e) => std.io.print_line(\"bad\")?\n    }\n";
    let t = Tmp::new("char_display", &main_wrap("", body));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), "x\n7\n");
}

/// 整数の幅変換は `from_*` / `try_from_*` (仕様 4.5)。`to_<整数型>` は持たない。
#[test]
fn integer_width_conversion_uses_from_not_to() {
    let t = Tmp::new("int_from", &main_wrap("", "    core.check.eq(u32.from_u8(200u8), 200u32)\n"));
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
}

/// 派生の順序 (`<` / `partial_cmp`) は、構造体のフィールドを型定義の宣言順に辞書式で比べる (4.3)。構造体リテラルに
/// 書いた順では比べない。入れ子 (`Option` の中) でも同じ。以前は格納順 (リテラルの順) で比べ、`x < y` を取り違えていた。
#[test]
fn derived_order_compares_fields_in_declaration_order() {
    let src = r#"type t.P derive [Clone, Copy, PartialEq, PartialOrd] pub
    a: u8
    b: u8
end t.P

fn t.main() -> () uses [] panics may
    let x = t.P { b: 1u8, a: 2u8 }
    let y = t.P { a: 1u8, b: 9u8 }
    core.check.eq(x < y, false)
    core.check.eq(x > y, true)
    core.check.eq(x.partial_cmp(&y) == Some(core.cmp.Ordering.Greater), true)
    let ox = Some(x)
    let oy = Some(y)
    core.check.eq(ox.partial_cmp(&oy) == Some(core.cmp.Ordering.Greater), true)
end t.main
"#;
    let t = Tmp::new("derived_order_decl", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
}

/// 派生の `cmp` / `partial_cmp` の入口は、利用者の型の最上位で手書きのメソッドを引かない (4.3)。手書きの `partial_cmp` が
/// 派生の `cmp` を呼んでも無限に再帰しない (以前は `stack overflow`)。逆向き (手書きの `cmp` → 派生の `partial_cmp`) も、
/// 派生の辞書式 (`partial_cmp` と `<`) で比べ、手書きの `cmp` は `cmp` の呼び出しでだけ使う。
#[test]
fn derived_cmp_entry_does_not_recurse_into_handwritten_methods() {
    let src = r#"type t.K derive [PartialEq, Eq, Ord] pub
    v: u8
end t.K

impl core.cmp.PartialOrd<t.K> for t.K
fn t.K.partial_cmp(self: &t.K, other: &t.K) -> Option<core.cmp.Ordering> uses [] panics never impls core.cmp.PartialOrd<t.K>
    Some(self.cmp(other))
end t.K.partial_cmp

type t.R derive [PartialEq, Eq, PartialOrd] pub
    v: u8
end t.R

impl core.cmp.Ord for t.R
fn t.R.cmp(self: &t.R, other: &t.R) -> core.cmp.Ordering uses [] panics never impls core.cmp.Ord
    if self.v > other.v {
        core.cmp.Ordering.Less
    } else if self.v < other.v {
        core.cmp.Ordering.Greater
    } else {
        core.cmp.Ordering.Equal
    }
end t.R.cmp

fn t.main() -> () uses [] panics may
    let a = t.K { v: 1u8 }
    let b = t.K { v: 2u8 }
    core.check.eq(a.cmp(&b) == core.cmp.Ordering.Less, true)
    core.check.eq(a < b, true)
    let c = t.R { v: 1u8 }
    let d = t.R { v: 2u8 }
    core.check.eq(c.partial_cmp(&d) == Some(core.cmp.Ordering.Less), true)
    core.check.eq(c < d, true)
    core.check.eq(c.cmp(&d) == core.cmp.Ordering.Greater, true)
end t.main
"#;
    let t = Tmp::new("derived_cmp_entry", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert!(!text(&o.stderr).contains("stack overflow"), "{}", text(&o.stderr));
}

/// 整数・`bool` だけの構造体の `derive [ToJson]` は、ネイティブ (LLVM) まで下ろせて、`run` と出力が一致する。
#[test]
fn derive_to_json_of_a_scalar_struct_builds_native() {
    let src = "type t.P derive [ToJson] pub\n    ok: bool\n    x: u32\n    y: i64\nend t.P\n\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never\n    let p = t.P { ok: true, x: 1u32, y: -2i64 }\n    match std.json.to_string(&p) {\n        Ok(s) => std.io.print_line(s.as_str())?\n        Err(_e) => std.io.print_line(\"err\")?\n    }\n    Ok(())\nend t.main\n";
    let t = Tmp::new("derive_json_native", src);
    let r = run(&["run"], &t.0);
    assert_eq!(text(&r.stdout), "{\"ok\":true,\"x\":1,\"y\":-2}\n", "{}", text(&r.stderr));
    if let Some((r, n)) = llvm_build_and_run(&t) {
        same_output(&r, &n);
    }
}

/// `alloc.arena.Arena` の世代付きの取っ手 (設計_共有の取っ手.md、D1-S1): `remove` 済みの取っ手は弱くなり
/// `get` が `None`、空きスロットを再利用しても世代が 1 つ上がるので古い取っ手は当たらない。`remove` は
/// 外した値を返す。別の arena の同じ型の取っ手が当たることは防げない (添字と世代だけを持つので)。
/// 型の違う取っ手は型検査 (E2023) で拒む。
#[test]
fn arena_stale_handles_return_none() {
    let src = "enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub\n    Alloc(alloc.alloc.AllocError)\n    Io(std.io.Error)\nend t.Error\n\n\
fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics may\n    \
let mut a = alloc.arena.Arena.new(std.alloc.SYSTEM)\n    \
let h0 = a.insert(1u32)?\n    \
let h1 = a.insert(2u32)?\n    \
core.check.eq(a.len(), 2usize)\n    \
core.check.eq(a.get(h0), Some(&1u32))\n    \
core.check.eq(a.remove(h1), Some(2u32))\n    \
core.check.is_true(a.get(h1).is_none())\n    \
core.check.is_true(!a.contains(h1))\n    \
let h2 = a.insert(3u32)?\n    \
core.check.eq(a.len(), 2usize)\n    \
core.check.eq(a.get(h2), Some(&3u32))\n    \
core.check.is_true(h2 != h1)\n    \
core.check.is_true(a.remove(h1).is_none())\n    \
core.check.is_true(a.remove(h2).is_some())\n    \
let h3 = a.insert(4u32)?\n    \
core.check.eq(a.len(), 2usize)\n    \
core.check.is_true(a.get(h3).is_some())\n    \
core.check.is_true(a.get(h2).is_none())\n    \
std.io.print_line(\"arena handle ok\")?\n    \
Ok(())\nend t.main\n";
    let t = Tmp::new("arena_handle", src);
    let r = run(&["run"], &t.0);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), "arena handle ok\n", "{}", text(&r.stdout));
}

/// `Arena<String>` と `core.ops.Drop` を手書きした要素の破棄 (設計_共有の取っ手.md、D1-S1): `remove` で外した
/// 値はその場で、arena を落とすときは残った値が落ちる。`Drop` の本体は効果の規則 (uses [] panics never) で
/// 表示できないので、二重解放・過剰解放で落ちないこと (終了コード 0 と表示) で確かめる。
#[test]
fn arena_drops_owned_values_without_crashing() {
    let src = "enum t.Error derive [Debug, Clone, Copy, PartialEq, From] pub\n    Alloc(alloc.alloc.AllocError)\n    Io(std.io.Error)\nend t.Error\n\n\
type t.Watch derive [Debug, Clone, Copy] pub\n    n: u32\nend t.Watch\n\n\
impl core.ops.Drop for t.Watch\nfn t.Watch.drop(self: &mut t.Watch) -> () uses [] panics never impls core.ops.Drop\n    ()\nend t.Watch.drop\n\n\
fn t.rounds(n: usize) -> Result<(), t.Error> uses [alloc] panics may\n    \
let mut r = 0usize\n    \
while r < n {\n        \
let mut a = alloc.arena.Arena.new(std.alloc.SYSTEM)\n        \
let s0 = a.insert(alloc.string.String.from_str(std.alloc.SYSTEM, \"keep\")?)?\n        \
let s1 = a.insert(alloc.string.String.from_str(std.alloc.SYSTEM, \"drop\")?)?\n        \
let mut w = alloc.arena.Arena.new(std.alloc.SYSTEM)\n        \
let _w0 = w.insert(t.Watch { n: 0u32 })?\n        \
let w1 = w.insert(t.Watch { n: 1u32 })?\n        \
let w2 = w.insert(t.Watch { n: 2u32 })?\n        \
core.check.eq(w.len(), 3usize)\n        \
core.check.is_true(w.remove(w1).is_some())\n        \
core.check.is_true(w.remove(w2).is_some())\n        \
core.check.is_true(w.get(w1).is_none())\n        \
core.check.eq(w.len(), 1usize)\n        \
core.check.is_true(a.remove(s0).is_some())\n        \
core.check.is_true(a.get(s1).is_some())\n        \
r = r.wrapping_add(1usize)\n    \
}\n    \
Ok(())\nend t.rounds\n\n\
fn t.main() -> Result<(), t.Error> uses [alloc, io.console] panics may\n    \
t.rounds(20usize)?\n    \
std.io.print_line(\"arena drop ok\")?\n    \
Ok(())\nend t.main\n";
    let t = Tmp::new("arena_drop", src);
    let r = run(&["run"], &t.0);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), "arena drop ok\n", "{}", text(&r.stdout));
}
