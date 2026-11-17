//! KIR の下ろしと参照実行器のテスト (2)。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use common::*;

#[test]
fn kir_str_find_matches_tree_walker() {
    // `s.find(pat)` (`&str` のパターン) は `Option<usize>` の最初の一致の位置 (バイト)。
    // 空のパターンは `Some(0)`、パターンが長いと `None`、後戻り (`aab` の `ab`) と多バイト文字 (`héllo`) を見る。
    let helpers = "fn t.at(s: &str, p: &str) -> Option<usize> uses [] panics never
    s.find(p)
end t.at
";
    // (関数, 本体, 木走査の期待値の式, KIR の表示)
    let cases = [
        ("t.f_c", "\"abc\".find(\"c\")", "Some(2usize)", "Some(2)"),
        ("t.f_bc", "\"abc\".find(\"bc\")", "Some(1usize)", "Some(1)"),
        ("t.f_empty", "\"abc\".find(\"\")", "Some(0usize)", "Some(0)"),
        ("t.f_none_long", "\"abc\".find(\"abcd\")", "None", "None"),
        ("t.f_none_miss", "\"abc\".find(\"x\")", "None", "None"),
        ("t.f_backtrack", "\"aab\".find(\"ab\")", "Some(1usize)", "Some(1)"),
        ("t.f_whole", "\"abc\".find(\"abc\")", "Some(0usize)", "Some(0)"),
        ("t.f_utf8", "\"héllo\".find(\"l\")", "Some(3usize)", "Some(3)"),
        ("t.f_empty_src", "\"\".find(\"a\")", "None", "None"),
        ("t.f_at", "t.at(\"hello world\", \"wor\")", "Some(6usize)", "Some(6)"),
    ];
    let mut src = String::from(helpers);
    let mut main = String::from("\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n");
    for (f, body, want, _) in cases {
        src.push_str(&format!("\nfn {}() -> Option<usize> uses [] panics never\n    {}\nend {}\n", f, body, f));
        main.push_str(&format!("    core.check.eq({}(), {})\n", f, want));
    }
    main.push_str("    Ok(())\nend t.main\n");
    let t = Tmp::new("kir_str_find", &format!("{}{}", src, main));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    for (f, _, _, shown) in cases {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), shown, "kir value of {}", f);
    }
}

#[test]
fn kir_char_methods_match_tree_walker() {
    // `char` の `to_ascii_uppercase` / `to_ascii_lowercase` (ASCII の英字の境界だけ変える)、`to_u32`、
    // `char.try_from_u32` (0x110000 未満で、サロゲート 0xD800..=0xDFFF の外)。木走査と値が一致する (`--run` / `--native`)。
    let cases = [
        ("t.up_q", "'q'.to_ascii_uppercase() == 'Q'"),
        ("t.up_a", "'a'.to_ascii_uppercase() == 'A'"),
        ("t.up_z", "'z'.to_ascii_uppercase() == 'Z'"),
        ("t.up_backtick", "'`'.to_ascii_uppercase() == '`'"),
        ("t.up_brace", "'{'.to_ascii_uppercase() == '{'"),
        ("t.up_digit", "'5'.to_ascii_uppercase() == '5'"),
        ("t.up_eacute", "'é'.to_ascii_uppercase() == 'é'"),
        ("t.to_u32", "'q'.to_u32() == 113u32"),
        ("t.low_upper_a", "'A'.to_ascii_lowercase() == 'a'"),
        ("t.low_at", "'@'.to_ascii_lowercase() == '@'"),
        ("t.low_bracket", "'['.to_ascii_lowercase() == '['"),
        ("t.low_z", "'Z'.to_ascii_lowercase() == 'z'"),
        ("t.from_a", "char.try_from_u32(65u32) == Some('A')"),
        ("t.from_surrogate", "char.try_from_u32(55296u32) == None"),
        ("t.from_surrogate_hi", "char.try_from_u32(57343u32) == None"),
        ("t.from_after_surrogate", "char.try_from_u32(57344u32).is_some()"),
        ("t.from_too_big", "char.try_from_u32(1114112u32) == None"),
        ("t.from_max", "char.try_from_u32(1114111u32).is_some()"),
    ];
    let mut src = String::new();
    let mut main = String::from("fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n");
    for (f, expr) in cases {
        src.push_str(&format!("fn {}() -> bool uses [] panics never\n    {}\nend {}\n\n", f, expr, f));
        main.push_str(&format!("    core.check.eq({}(), true)\n", f));
    }
    main.push_str("    Ok(())\nend t.main\n");
    let t = Tmp::new("kir_char_methods", &format!("{}{}", src, main));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    for (f, _) in cases {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), "true", "kir value of {}", f);
    }
}

#[test]
fn kir_tuple_eq_matches_tree_walker() {
    // タプルの `==` / `!=` は要素を順に比べる (木走査の `v_eq` と同じ)。`Option<(&str, &str)>` (`split_once`) を含む。
    // 失敗したときの文言 (タプルの表示 `("a", "c")`) も木走査と同じ。
    let cases = [
        ("t.t_same", "(\"ab\", \"cd\") == (\"ab\", \"cd\")"),
        ("t.t_diff", "(\"ab\", \"cd\") != (\"ab\", \"ce\")"),
        ("t.t_mixed", "(1u8, \"x\", true) == (1u8, \"x\", true)"),
        ("t.t_mixed_diff", "(1u8, \"x\", true) != (1u8, \"x\", false)"),
        ("t.t_opt", "\"key=value\".split_once(\"=\") == Some((\"key\", \"value\"))"),
        ("t.t_opt_none", "\"novalue\".split_once(\"=\") == None"),
    ];
    let mut src = String::new();
    let mut main = String::from("fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n");
    for (f, body) in cases {
        src.push_str(&format!("fn {}() -> bool uses [] panics never\n    {}\nend {}\n\n", f, body, f));
        main.push_str(&format!("    core.check.eq({}(), true)\n", f));
    }
    main.push_str("    Ok(())\nend t.main\n");
    let t = Tmp::new("kir_tuple_eq", &format!("{}{}", src, main));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", format!("{}{}", text(&o.stdout), text(&o.stderr)));
    for (f, _) in cases {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), "true", "kir value of {}", f);
    }
    // 失敗の文言: 木走査と KIR の標準エラーが一致する (タプルの表示)。
    let fail = "fn t.fail() -> bool uses [io.console] panics may\n    core.check.eq((\"a\", \"b\"), (\"a\", \"c\"))\n    true\nend t.fail\n\nfn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.mem.drop(t.fail())\n    Ok(())\nend t.main\n";
    let tf = Tmp::new("kir_tuple_eq_fail", fail);
    let w = run(&["run"], &tf.0);
    let k = kir_run(&["kir", "t.fail", "--run"], &tf.0);
    assert_eq!(w.status.code(), Some(101), "tree-walker\n{}", text(&w.stderr));
    assert_eq!(k.status.code(), Some(101), "kir\n{}", text(&k.stderr));
    assert_eq!(text(&k.stderr), text(&w.stderr), "失敗の文言が違う");
}

#[test]
fn kir_array_literal_of_aggregates_matches_tree_walker() {
    // 所有物を持たない集成体 (列挙型・構造体) の配列リテラルを `for` で回す。木走査と値が一致し (`--run` / `--native`)、
    // 要素が `String` のときは、移動と解放を伴うので理由付きで止める。
    let src = "type t.P derive [Clone] pub
    x: i32
end t.P

enum t.Shape derive [Clone] pub
    Circle(i32)
    Rect(i32, i32)
    Unit
end t.Shape

fn t.shapes() -> i32 uses [] panics never
    let a = t.Shape.Circle(3i32)
    let b = t.Shape.Rect(4i32, 6i32)
    let c = t.Shape.Unit
    let mut sum = 0i32
    for s in [a, b, c] {
        sum = sum.wrapping_add(match s {
            t.Shape.Circle(r) => r.wrapping_mul(2i32)
            t.Shape.Rect(w, h) => w.wrapping_mul(h)
            t.Shape.Unit => 0i32
        })
    }
    sum
end t.shapes

fn t.pts() -> i32 uses [] panics never
    let a = t.P { x: 3i32 }
    let b = t.P { x: 4i32 }
    let mut sum = 0i32
    for p in [a, b] {
        sum = sum.wrapping_add(p.x)
    }
    sum
end t.pts

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.shapes(), 30i32)
    core.check.eq(t.pts(), 7i32)
    Ok(())
end t.main
";
    let t = Tmp::new("kir_arr_agg", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    for (f, want) in [("t.shapes", "30"), ("t.pts", "7")] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
    // 所有物の要素 (`String`) は理由付きで未対応。
    let owned = "fn t.own() -> usize uses [alloc] panics may
    let s = alloc.string.String.new(std.alloc.SYSTEM)
    let mut n = 0usize
    for x in [s] {
        n = n.wrapping_add(x.len())
    }
    n
end t.own

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.own(), 0usize)
    Ok(())
end t.main
";
    let t2 = Tmp::new("kir_arr_owned", owned);
    let k = run(&["kir", "t.own"], &t2.0);
    let msg = format!("{}{}", text(&k.stdout), text(&k.stderr));
    assert!(msg.contains("所有物を持たない集成体だけ"), "{}", msg);
}

#[test]
fn kir_btree_map_matches_tree_walker() {
    // `BtreeMap<K, V>` (整数・bool・char のキーと値)。キーの順を保つ配列 (二分探索・挿入と削除でずらす・容量を広げる)。
    // 木走査 (`natives.rs`) と値が一致する (`--run` / `--native`)。
    let cases: &[(&str, &str)] = &[
        // tour と同じ並び (挿入・置き換え・長さ・検索・削除)
        ("t.bt_basic", "    let mut m = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)
    let i1 = m.insert(3u8, 30u8)
    let i2 = m.insert(1u8, 10u8)
    let i3 = m.insert(3u8, 33u8)
    core.check.eq(i1, Ok(None))
    core.check.eq(i2, Ok(None))
    core.check.eq(i3, Ok(Some(30u8)))
    core.check.eq(m.len(), 2usize)
    core.check.eq(m.get(3u8), Some(33u8))
    core.check.eq(m.contains_key(2u8), false)
    core.check.eq(m.remove(1u8), Some(10u8))
    core.check.eq(m.remove(1u8), None)
    core.check.eq(m.is_empty(), false)
    true"),
        // 負のキーと、ソートの順 (先頭・末尾・中ほどの削除)
        ("t.bt_neg", "    let mut m = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)
    for k in [5i32, -3i32, 9i32, 0i32, -10i32, 7i32] {
        core.mem.drop(m.insert(k, k.wrapping_mul(2i32)))
    }
    core.check.eq(m.get(-3i32), Some(-6i32))
    core.check.eq(m.get(4i32), None)
    core.check.eq(m.remove(5i32), Some(10i32))
    core.check.eq(m.remove(5i32), None)
    core.check.eq(m.remove(-10i32), Some(-20i32))
    core.check.eq(m.remove(9i32), Some(18i32))
    core.check.eq(m.len(), 3usize)
    core.check.eq(m.get(7i32), Some(14i32))
    true"),
        // 容量を何度も広げる (降順に 40 個入れる)、偶数の削除、先頭の削除
        ("t.bt_grow", "    let mut m = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)
    for k in core.iter.range(1i64, 41i64) {
        core.mem.drop(m.insert(41i64 - k, k * 10i64))
    }
    core.check.eq(m.len(), 40usize)
    core.check.eq(m.get(1i64), Some(400i64))
    core.check.eq(m.get(40i64), Some(10i64))
    core.check.eq(m.get(41i64), None)
    for k in core.iter.range(1i64, 41i64) {
        if k % 2i64 == 0i64 {
            core.mem.drop(m.remove(k))
        }
    }
    core.check.eq(m.len(), 20usize)
    core.check.eq(m.get(2i64), None)
    core.check.eq(m.get(3i64), Some(380i64))
    core.check.eq(m.remove(1i64), Some(400i64))
    core.check.eq(m.len(), 19usize)
    true"),
        // bool と char のキー
        ("t.bt_char", "    let mut m = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)
    core.mem.drop(m.insert('z', true))
    core.mem.drop(m.insert('a', false))
    core.check.eq(m.get('a'), Some(false))
    core.check.eq(m.get('z'), Some(true))
    core.check.eq(m.get('m'), None)
    true"),
    ];
    let mut src = String::new();
    let mut main = String::from("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n");
    for (f, body) in cases {
        src.push_str(&format!("fn {}() -> bool uses [alloc] panics may\n{}\nend {}\n\n", f, body, f));
        main.push_str(&format!("    core.check.eq({}(), true)\n", f));
    }
    main.push_str("    Ok(())\nend t.main\n");
    let t = Tmp::new("kir_btree", &format!("{}{}", src, main));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    for (f, _) in cases {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), "true", "kir value of {}", f);
    }
}

#[test]
fn kir_push_i64_matches_tree_walker() {
    // `String.push_i64(v)`: 負なら `-` を先に書き、絶対値の桁を書く。`i64::MIN` (絶対値が i64 に入らない) も木走査と同じ。
    // 最後のケースは長い文字列の後ろに書いて、符号の 1 バイトを書くときに容量を広げる経路を通す。
    let cases: &[(&str, &str, &str)] = &[
        ("t.p_zero", "0i64", "x0"),
        ("t.p_pos", "5i64", "x5"),
        ("t.p_neg", "-42i64", "x-42"),
        ("t.p_neg_one", "-1i64", "x-1"),
        ("t.p_min", "-9223372036854775808i64", "x-9223372036854775808"),
        ("t.p_max", "9223372036854775807i64", "x9223372036854775807"),
        ("t.p_grow", "-7i64", "abcdefghijklmnopqrstuvwxyz0123456789-7"),
    ];
    let mut src = String::new();
    let mut main = String::from("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n");
    for (f, v, want) in cases {
        let prefix = if *f == "t.p_grow" { "abcdefghijklmnopqrstuvwxyz0123456789" } else { "x" };
        src.push_str(&format!(
            "fn {}() -> bool uses [alloc] panics may\n    let mut s = alloc.string.String.new(std.alloc.SYSTEM)\n    core.mem.drop(s.push_str(\"{}\"))\n    core.mem.drop(s.push_i64({}))\n    s.as_str() == \"{}\"\nend {}\n\n",
            f, prefix, v, want, f
        ));
        main.push_str(&format!("    core.check.eq({}(), true)\n", f));
    }
    main.push_str("    Ok(())\nend t.main\n");
    let t = Tmp::new("kir_push_i64", &format!("{}{}", src, main));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    for (f, _, _) in cases {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), "true", "kir value of {}", f);
    }
}

#[test]
fn kir_push_bytes_matches_tree_walker() {
    // `String.push_bytes(bytes)`: 正しい UTF-8・空・誤った UTF-8 (何も足さない) の 3 ケース。
    // `--run` (KIR インタプリタ) と `--native` (clang/LLVM) が `ksn run` と一致すること。
    let cases: &[(&str, &str, &str)] = &[
        ("t.p_ok", "core.mem.drop(b.push_bytes(\"héy\".as_bytes()))\n    b.as_str() == \"héy\"", "true"),
        ("t.p_emp", "core.mem.drop(b.push_bytes(\"\".as_bytes()))\n    b.as_str() == \"\"", "true"),
        ("t.p_bad", "core.mem.drop(b.push_bytes(b\"\\xff\"))\n    b.push_bytes(b\"\\xff\").is_ok()", "false"),
    ];
    let mut src = String::new();
    let mut main = String::from("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n");
    for (f, body, want) in cases {
        src.push_str(&format!(
            "fn {}() -> bool uses [alloc] panics may\n    let mut b = alloc.string.String.new(std.alloc.SYSTEM)\n    {}\nend {}\n\n",
            f, body, f
        ));
        main.push_str(&format!("    core.check.eq({}(), {})\n", f, want));
    }
    main.push_str("    Ok(())\nend t.main\n");
    let t = Tmp::new("kir_push_bytes", &format!("{}{}", src, main));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    for engine in ["--run", "--native"] {
        for (f, _, want) in cases {
            let k = kir_run(&["kir", f, engine], &t.0);
            let got = text(&k.stdout).trim().to_string();
            assert_eq!(k.status.code(), Some(0), "kir {} {}\n{}", f, engine, text(&k.stderr));
            assert_eq!(got, *want, "kir value of {} {}", f, engine);
        }
    }
}

#[test]
fn kir_parse_matches_tree_walker() {
    // `i64.parse(text)` は基数 10 の `from_str_radix` (木走査と同じ経路)。符号・溢れ・形の誤りを見る。
    let cases = [
        ("t.p_plain", "match i64.parse(\"12\") {\n        Ok(n) => n == 12i64\n        Err(_) => false\n    }"),
        ("t.p_neg", "match i64.parse(\"-3\") {\n        Ok(n) => n == -3i64\n        Err(_) => false\n    }"),
        ("t.p_plus", "match i64.parse(\"+5\") {\n        Ok(n) => n == 5i64\n        Err(_) => false\n    }"),
        ("t.p_letter", "match i64.parse(\"x\") {\n        Ok(_) => false\n        Err(_) => true\n    }"),
        ("t.p_empty", "match i64.parse(\"\") {\n        Ok(_) => false\n        Err(_) => true\n    }"),
        ("t.p_u8_overflow", "match u8.parse(\"300\") {\n        Ok(_) => false\n        Err(_) => true\n    }"),
        ("t.p_u8_max", "match u8.parse(\"255\") {\n        Ok(n) => n == 255u8\n        Err(_) => false\n    }"),
        ("t.p_u8_neg", "match u8.parse(\"-1\") {\n        Ok(_) => false\n        Err(_) => true\n    }"),
    ];
    let mut src = String::new();
    let mut main = String::from("fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n");
    for (f, body) in cases {
        src.push_str(&format!("fn {}() -> bool uses [] panics never\n    {}\nend {}\n\n", f, body, f));
        main.push_str(&format!("    core.check.eq({}(), true)\n", f));
    }
    main.push_str("    Ok(())\nend t.main\n");
    let t = Tmp::new("kir_parse", &format!("{}{}", src, main));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    for (f, _) in cases {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), "true", "kir value of {}", f);
    }
}

#[test]
fn kir_optres_slice_literal_eq_matches_tree_walker() {
    // `Option<&[T]>` (`sub` の結果) と `Some(&[..])` (配列リテラルの参照) の `==` / `!=`。
    // `Some(&[..])` は長さ N を持つ参照だが、Option のペイロードでは `fat` (`Option<&[T]>` と同じ値) で持つ。
    // 失敗の文言は `Option<&[u8]>` の表示 (`Some([2, 3])`) で、木走査と同じ。
    let src = "fn t.same(a: usize, b: usize) -> bool uses [] panics never
    let arr = [1u8, 2u8, 3u8, 4u8, 5u8]
    let mid = arr.sub(a, b)
    mid == Some(&[2u8, 3u8, 4u8])
end t.same

fn t.differs(a: usize, b: usize) -> bool uses [] panics never
    let arr = [1u8, 2u8, 3u8, 4u8, 5u8]
    let mid = arr.sub(a, b)
    mid != Some(&[2u8, 3u8, 4u8])
end t.differs

fn t.p1() -> bool uses [] panics never
    t.same(1usize, 4usize)
end t.p1

fn t.p2() -> bool uses [] panics never
    t.same(0usize, 3usize)
end t.p2

fn t.p3() -> bool uses [] panics never
    t.same(9usize, 9usize)
end t.p3

fn t.p4() -> bool uses [] panics never
    t.differs(0usize, 3usize)
end t.p4

fn t.p5() -> bool uses [] panics never
    t.differs(1usize, 4usize)
end t.p5

fn t.show() -> () uses [io.console] panics may
    let arr = [1u8, 2u8, 3u8, 4u8, 5u8]
    let mid = arr.sub(1usize, 3usize)
    core.check.eq(mid, Some(&[9u8, 3u8]))
end t.show

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.p1(), true)
    core.check.eq(t.p2(), false)
    core.check.eq(t.p3(), false)
    core.check.eq(t.p4(), true)
    core.check.eq(t.p5(), false)
    Ok(())
end t.main
";
    let t = Tmp::new("kir_optres_slice_lit", src);
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0), "tree-walker\n{}", text(&run(&["run"], &t.0).stderr));
    for (f, want) in [("t.p1", "true"), ("t.p2", "false"), ("t.p3", "false"), ("t.p4", "true"), ("t.p5", "false")] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
    // 失敗の文言: 木走査と同じ表示 (`Some([2, 3])` / `Some([9, 3])`)。native / ASan との比較も `kir_run` が行う。
    let k = kir_run(&["kir", "t.show", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(101), "kir t.show\n{}", text(&k.stderr));
    assert_eq!(
        text(&k.stderr),
        "panic: assertion failed: left: Some([2, 3]), right: Some([9, 3])\n",
        "kir t.show の失敗の文言"
    );
}

// ---- 段階 1: ペイロードが所有物 (String / Vec) の Option / Result (E は整数/unit) ----
// `Some`/`Ok`/`Err` で作り、`match` でペイロードを取り出し、`?` で同じ E を伝え、
// スコープの終わりで解放する。観察値はスカラーに落とす (--run の入口は集成体を引数・戻り値に持てないため)。
// 参照実行器・機械語・ASan は kir_run が比べる (解放の漏れ・二重解放も ASan で検出)。
#[test]
fn kir_result_heap_owned_payloads_match_tree_walker() {
    // 段階 1: `Option<String>` / `Result<String, u8>` / `Result<Vec<u32>, u8>` を `Some`/`Ok`/`Err` で作り、
    // `match` でペイロードを取り出し、`?` で同じ E を伝え、スコープの終わりで解放する。
    // 観察値はスカラーに落とす (--run の入口は集成体を引数・戻り値に持てないため)。
    let src = "fn t.mk_s() -> Option<alloc.string.String<std.alloc.System>> uses [alloc] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(\"banana\"))
    Some(s)
end t.mk_s

fn t.mk_n() -> Option<alloc.string.String<std.alloc.System>> uses [alloc] panics may
    None
end t.mk_n

fn t.opt_len() -> usize uses [alloc] panics may
    match t.mk_s() {
        Some(x) => x.len()
        None => 99usize
    }
end t.opt_len

fn t.opt_none() -> usize uses [alloc] panics may
    match t.mk_n() {
        Some(_x) => 99usize
        None => 0usize
    }
end t.opt_none

fn t.mk_ok() -> Result<alloc.string.String<std.alloc.System>, u8> uses [alloc] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(\"ok\"))
    Ok(s)
end t.mk_ok

fn t.mk_err() -> Result<alloc.string.String<std.alloc.System>, u8> uses [] panics never
    Err(3u8)
end t.mk_err

fn t.res_match_ok() -> usize uses [alloc] panics may
    match t.mk_ok() {
        Ok(x) => x.len()
        Err(_e) => 99usize
    }
end t.res_match_ok

fn t.res_match_err() -> usize uses [] panics never
    match t.mk_err() {
        Ok(_x) => 99usize
        Err(_e) => 0usize
    }
end t.res_match_err

fn t.q() -> Result<alloc.string.String<std.alloc.System>, u8> uses [alloc] panics may
    Ok(t.mk_ok()?)
end t.q

fn t.q_len() -> usize uses [alloc] panics may
    match t.q() {
        Ok(x) => x.len()
        Err(_e) => 99usize
    }
end t.q_len

fn t.mk_v() -> Result<alloc.vec.Vec<u32, std.alloc.System>, u8> uses [alloc] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(1u32))
    core.mem.drop(v.push(2u32))
    Ok(v)
end t.mk_v

fn t.mk_ve() -> Result<alloc.vec.Vec<u32, std.alloc.System>, u8> uses [alloc] panics may
    Err(9u8)
end t.mk_ve

fn t.vlen() -> usize uses [alloc] panics may
    match t.mk_v() {
        Ok(v) => v.len()
        Err(_e) => 99usize
    }
end t.vlen

fn t.velen() -> usize uses [alloc] panics may
    match t.mk_ve() {
        Ok(_v) => 99usize
        Err(_e) => 0usize
    }
end t.velen

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.opt_len(), 6usize)
    core.check.eq(t.opt_none(), 0usize)
    core.check.eq(t.res_match_ok(), 2usize)
    core.check.eq(t.res_match_err(), 0usize)
    core.check.eq(t.q_len(), 2usize)
    core.check.eq(t.vlen(), 2usize)
    core.check.eq(t.velen(), 0usize)
    Ok(())
end t.main
";
    let t = Tmp::new("kir_result_heap_owned", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    for (f, want) in [
        ("t.opt_len", "6"),
        ("t.opt_none", "0"),
        ("t.res_match_ok", "2"),
        ("t.res_match_err", "0"),
        ("t.q_len", "2"),
        ("t.vlen", "2"),
        ("t.velen", "0"),
    ] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
}

// ---- 段階 2: `Result` / `Option` の E が、所有物 (String) を持つ利用者の enum / 構造体 ----
#[test]
fn kir_result_heap_user_error_match_tree_walker() {
    // 段階 2: `Result` / `Option` の E が、所有物 (String) を持つ利用者の enum / 構造体。
    // `std.net.Error` を型表に登録済みの前提 (kir_lower_agg.rs の STD_TYPES)。
    let src = "enum t.Err pub
    Io(std.io.Error)
    Net(std.net.Error)
    Msg(alloc.string.String<std.alloc.System>)
end t.Err

fn t.msg() -> Result<usize, t.Err> uses [alloc] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(\"message\"))
    Err(t.Err.Msg(s))
end t.msg

fn t.io() -> Result<usize, t.Err> uses [] panics never
    Err(t.Err.Io(std.io.Error { kind: std.io.ErrorKind.Other, code: 0i32 }))
end t.io

fn t.ok_v() -> Result<usize, t.Err> uses [] panics never
    Ok(42usize)
end t.ok_v

fn t.classify_msg() -> usize uses [alloc] panics may
    match t.msg() {
        Ok(n) => n
        Err(e) => match e {
            t.Err.Io(_e) => 1usize
            t.Err.Net(_e) => 2usize
            t.Err.Msg(s) => s.len()
        }
    }
end t.classify_msg

fn t.classify_io() -> usize uses [] panics never
    match t.io() {
        Ok(n) => n
        Err(e) => match e {
            t.Err.Io(_e) => 1usize
            t.Err.Net(_e) => 2usize
            t.Err.Msg(_s) => 3usize
        }
    }
end t.classify_io

fn t.ok_v_len() -> usize uses [] panics never
    match t.ok_v() {
        Ok(n) => n
        Err(_e) => 0usize
    }
end t.ok_v_len

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    core.check.eq(t.classify_msg(), 7usize)
    core.check.eq(t.classify_io(), 1usize)
    core.check.eq(t.ok_v_len(), 42usize)
    Ok(())
end t.main
";
    let t = Tmp::new("kir_result_heap_user_error", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    for (f, want) in [
        ("t.classify_msg", "7"),
        ("t.classify_io", "1"),
        ("t.ok_v_len", "42"),
    ] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
}

// ---- 集成体 (構造体・タプル・列挙型) の下ろし (設計_KIR.md 2.3、kir/aggregates) ----

/// 構築・フィールドの読み書き・列挙型の変種と `match` の束縛・タプルの束縛・ループ・腕の中の代入を含む。
/// `t.run_*` は整数だけを引数に取るので、`ksn kir <FQN> --run` で呼べる。`t.main` は木走査の検査。
const KIR_AGG_SRC: &str = r#"type t.Box
    p: t.Pt
    ok: bool
    n: i64
end t.Box

type t.Pt
    x: i32
    y: i32
end t.Pt

enum t.Sh
    Circle(i64)
    Rect(i64, i64)
    Tag(bool, i64)
    Empty
end t.Sh

fn t.area(s: t.Sh) -> i64 uses [] panics never
    match s {
        t.Sh.Circle(r) => r.wrapping_mul(3i64)
        t.Sh.Rect(w, h) => w.wrapping_mul(h)
        t.Sh.Tag(b, n) => {
            if b {
                n
            } else {
                0i64
            }
        }
        t.Sh.Empty => 0i64
    }
end t.area

fn t.area_of(c: i64) -> i64 uses [] panics never
    t.area(t.Sh.Rect(c, 2i64)).wrapping_add(t.area(t.Sh.Tag(true, c)))
end t.area_of

fn t.arm(s: t.Sh) -> i64 uses [] panics never
    let mut cur = 1i64
    let mut sh = s
    match sh {
        t.Sh.Circle(r) => {
            sh = t.Sh.Empty
            cur = r
        }
        t.Sh.Rect(_, _) => {
            cur = 2i64
        }
        t.Sh.Tag(_, _) => {
            cur = 3i64
        }
        t.Sh.Empty => {
            cur = 4i64
        }
    }
    match sh {
        t.Sh.Circle(_) => cur
        t.Sh.Rect(_, _) => cur
        t.Sh.Tag(_, _) => cur
        t.Sh.Empty => cur.wrapping_add(100i64)
    }
end t.arm

fn t.box_n(bx: t.Box) -> i64 uses [] panics never
    if bx.ok {
        bx.n.wrapping_add(10i64)
    } else {
        bx.n
    }
end t.box_n

fn t.first(pr: (i32, bool)) -> i32 uses [] panics never
    match pr {
        (x, true) => x
        (x, false) => 0i32
    }
end t.first

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.area(t.Sh.Circle(4i64)), 12i64)
    core.check.eq(t.area(t.Sh.Rect(3i64, 5i64)), 15i64)
    core.check.eq(t.area(t.Sh.Tag(true, 9i64)), 9i64)
    core.check.eq(t.area(t.Sh.Tag(false, 9i64)), 0i64)
    core.check.eq(t.area(t.Sh.Empty), 0i64)
    core.check.eq(t.box_n(t.mk_box(1i32, 2i32, true)), 15i64)
    core.check.eq(t.px(t.mk_box(1i32, 2i32, true)), 1002i32)
    core.check.eq(t.first(t.pair(7i32)), 7i32)
    core.check.eq(t.area(t.pick(t.Sh.Circle(6i64))), 36i64)
    core.check.eq(t.walk(4i32), 604i32)
    core.check.eq(t.arm(t.Sh.Circle(8i64)), 108i64)
    core.check.eq(t.arm(t.Sh.Rect(8i64, 1i64)), 2i64)
    core.check.eq(t.tup(9i32), 9i32)
    core.check.eq(t.area_of(4i64), 12i64)
    core.check.eq(t.run_arm(8i64), 108i64)
    core.check.eq(t.run_arm2(8i64), 2i64)
    core.check.eq(t.run_box(1i32, 2i32), 15i64)
    core.check.eq(t.run_first(7i32), 7i32)
    core.check.eq(t.run_pick(6i64), 36i64)
    core.check.eq(t.run_px(1i32, 2i32), 1002i32)
    core.check.eq(t.run_tag(true, 9i64), 9i64)
    core.check.eq(t.run_tag(false, 9i64), 0i64)
    Ok(())
end t.main

fn t.mk_box(a: i32, b: i32, f: bool) -> t.Box uses [] panics never
    let p = t.Pt { x: a, y: b }
    t.Box { p: p, ok: f, n: 5i64 }
end t.mk_box

fn t.pair(a: i32) -> (i32, bool) uses [] panics never
    (a, true)
end t.pair

fn t.pick(s: t.Sh) -> t.Sh uses [] panics never
    match s {
        t.Sh.Circle(r) => t.Sh.Rect(r, r)
        t.Sh.Rect(w, h) => t.Sh.Rect(w, h)
        t.Sh.Tag(b, n) => t.Sh.Tag(b, n)
        t.Sh.Empty => t.Sh.Empty
    }
end t.pick

fn t.px(bx: t.Box) -> i32 uses [] panics never
    bx.p.x.wrapping_mul(1000i32).wrapping_add(bx.p.y)
end t.px

fn t.run_arm(c: i64) -> i64 uses [] panics never
    t.arm(t.Sh.Circle(c))
end t.run_arm

fn t.run_arm2(c: i64) -> i64 uses [] panics never
    t.arm(t.Sh.Rect(c, 1i64))
end t.run_arm2

fn t.run_box(a: i32, b: i32) -> i64 uses [] panics never
    t.box_n(t.mk_box(a, b, true))
end t.run_box

fn t.run_first(a: i32) -> i32 uses [] panics never
    t.first(t.pair(a))
end t.run_first

fn t.run_pick(c: i64) -> i64 uses [] panics never
    t.area(t.pick(t.Sh.Circle(c)))
end t.run_pick

fn t.run_px(a: i32, b: i32) -> i32 uses [] panics never
    t.px(t.mk_box(a, b, false))
end t.run_px

fn t.run_tag(b: bool, c: i64) -> i64 uses [] panics never
    t.area(t.Sh.Tag(b, c))
end t.run_tag

fn t.tup(a: i32) -> i32 uses [] panics never
    let (x, ok) = t.pair(a)
    if ok {
        x
    } else {
        0i32
    }
end t.tup

fn t.walk(n: i32) -> i32 uses [] panics never
    let mut acc = t.Pt { x: 0i32, y: 0i32 }
    let mut i = 0i32
    while i < n {
        acc.x = acc.x.wrapping_add(i)
        acc.y = acc.y.wrapping_add(1i32)
        i = i.wrapping_add(1i32)
    }
    acc.x.wrapping_mul(100i32).wrapping_add(acc.y)
end t.walk
"#;

#[test]
fn kir_agg_values_match_tree_walker() {
    // 木走査 (`ksn run`) の検査が通ること、参照実行器 (`ksn kir --run`) の値が同じ期待値であること。
    let t = Tmp::new("kir_agg_values", KIR_AGG_SRC);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker\n{}", text(&o.stderr));
    let cases: &[(&str, &[&str], &str)] = &[
        ("t.run_arm", &["8"], "108"),
        ("t.run_arm2", &["8"], "2"),
        ("t.run_box", &["1", "2"], "15"),
        ("t.run_first", &["7"], "7"),
        ("t.run_pick", &["6"], "36"),
        ("t.run_px", &["1", "2"], "1002"),
        ("t.run_tag", &["true", "9"], "9"),
        ("t.run_tag", &["false", "9"], "0"),
        ("t.walk", &["4"], "604"),
        ("t.walk", &["10"], "4510"),
        ("t.tup", &["9"], "9"),
        ("t.area_of", &["4"], "12"),
    ];
    for (f, args, want) in cases {
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {} {:?}\n{}", f, args, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {} {:?}", f, args);
    }
}

// ---- KIR の数値 (浮動小数・128 ビット・ビット演算・シフト・数値のメソッド・変換): kir_scalar_* ----

/// 数値の関数の試験用の定義。名前の順に並べる (E0020)。`t.main` は呼び出し側が最後に付ける。
const SCALAR_FNS: &str = "fn t.a_band(a: u32, b: u32) -> u32 uses [] panics never
    a & b
end t.a_band

fn t.a_big() -> i128 uses [] panics never
    let a = 170141183460469231731687303715884105727i128
    a.wrapping_add(1i128)
end t.a_big

fn t.a_bnot8(a: u8) -> u8 uses [] panics never
    !a
end t.a_bnot8

fn t.a_bnoti(a: i32) -> i32 uses [] panics never
    !a
end t.a_bnoti

fn t.a_cnt(a: u32) -> u32 uses [] panics never
    a.count_ones()
end t.a_cnt

fn t.a_conv1() -> i64 uses [] panics never
    i64.from_i8(-5i8)
end t.a_conv1

fn t.a_conv2() -> f64 uses [] panics never
    f64.from_i32_nearest(7i32)
end t.a_conv2

fn t.a_conv3() -> u64 uses [] panics never
    f64.from_bits(4607182418800017408u64).to_bits()
end t.a_conv3

fn t.a_conv4() -> u32 uses [] panics never
    0.1f64.nearest_to_f32().to_bits()
end t.a_conv4

fn t.a_f32trig() -> u32 uses [] panics never
    let x = 0.7f32
    (x.sin() + x.exp() * x).to_bits()
end t.a_f32trig

fn t.a_fl1() -> f64 uses [] panics never
    let a = 9.0f64
    let b = a.sqrt()
    let c = b * 2.0 + 1.5
    c.floor()
end t.a_fl1

fn t.a_fromu128() -> f64 uses [] panics never
    f64.from_u128_nearest(0u128.wrapping_sub(1u128))
end t.a_fromu128

fn t.a_lz(a: u16) -> u32 uses [] panics never
    a.leading_zeros()
end t.a_lz

fn t.a_max(a: i64, b: i64) -> i64 uses [] panics never
    a.max(b)
end t.a_max

fn t.a_nan() -> bool uses [] panics never
    let n = 0.0f64 / 0.0f64
    n == n
end t.a_nan

fn t.a_sat(a: u8, b: u8) -> u8 uses [] panics never
    a.saturating_add(b)
end t.a_sat

fn t.a_satadd128() -> i128 uses [] panics never
    let a = -170141183460469231731687303715884105727i128
    a.saturating_add(-10i128)
end t.a_satadd128

fn t.a_satneg(a: i8) -> i8 uses [] panics never
    a.saturating_neg()
end t.a_satneg

fn t.a_satneg128() -> i128 uses [] panics never
    let a = -170141183460469231731687303715884105727i128
    a.saturating_sub(10i128)
end t.a_satneg128

fn t.a_shl8(a: u8, n: u32) -> u8 uses [] panics never
    a.wrapping_shl(n)
end t.a_shl8

fn t.a_shri8(a: i8, n: u32) -> i8 uses [] panics never
    a.wrapping_shr(n)
end t.a_shri8

fn t.a_trig() -> u64 uses [] panics never
    let x = 0.7f64
    (x.sin() + x.cos() * x.tan() + x.exp() + x.ln() + x.atan2(0.3f64) + x.powi(5i32)).to_bits()
end t.a_trig

fn t.a_tz(a: i32) -> u32 uses [] panics never
    a.trailing_zeros()
end t.a_tz

fn t.a_ucmp() -> bool uses [] panics never
    t.a_umax() > 1u128
end t.a_ucmp

fn t.a_umax() -> u128 uses [] panics never
    0u128.wrapping_sub(1u128)
end t.a_umax

fn t.a_wneg(a: u8) -> u8 uses [] panics never
    a.wrapping_neg()
end t.a_wneg

fn t.a_wshr64(a: u64, n: u32) -> u64 uses [] panics never
    a.wrapping_shr(n)
end t.a_wshr64
";

/// `SCALAR_FNS` に `main` (本体 `body`) を付けたパッケージを作る。
fn scalar_pkg(name: &str, body: &str) -> Tmp {
    Tmp::new(
        name,
        &format!("{}fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n{}    Ok(())\nend t.main\n", SCALAR_FNS, body),
    )
}

#[test]
fn kir_scalar_ints_match_tree_walker() {
    // (関数, 木走査の引数 (接尾辞付き), 参照実行器の引数, 木走査の期待値, 参照実行器の期待値)
    let cases: &[(&str, &[&str], &[&str], &str, &str)] = &[
        ("t.a_band", &["12u32", "10u32"], &["12", "10"], "8u32", "8"),
        ("t.a_bnot8", &["0u8"], &["0"], "255u8", "255"),
        ("t.a_bnoti", &["5i32"], &["5"], "-6i32", "-6"),
        ("t.a_shl8", &["129u8", "9u32"], &["129", "9"], "2u8", "2"),
        ("t.a_shri8", &["-128i8", "9u32"], &["-128", "9"], "-64i8", "-64"),
        ("t.a_sat", &["250u8", "10u8"], &["250", "10"], "255u8", "255"),
        ("t.a_satneg", &["-128i8"], &["-128"], "127i8", "127"),
        ("t.a_cnt", &["61680u32"], &["61680"], "8u32", "8"),
        ("t.a_lz", &["1u16"], &["1"], "15u32", "15"),
        ("t.a_tz", &["0i32"], &["0"], "32u32", "32"),
        ("t.a_max", &["-3i64", "2i64"], &["-3", "2"], "2i64", "2"),
        ("t.a_wneg", &["1u8"], &["1"], "255u8", "255"),
        ("t.a_wshr64", &["18446744073709551615u64", "60u32"], &["18446744073709551615", "60"], "15u64", "15"),
    ];
    for (i, (f, targs, kargs, want, kwant)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, targs.join(", "));
        let t = scalar_pkg(&format!("kir_scalar_int_{}", i), &format!("    core.check.eq({}, {})\n", call, want));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut ka = vec!["kir", *f, "--run"];
        ka.extend(kargs.iter().copied());
        let k = kir_run(&ka, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *kwant, "kir value of {}", call);
    }
}

/// `const t.CONFIG: &str = "..."` のような文字列定数を下ろし、使う場所で値を作る。
/// `ksn run` と `ksn kir --run` と `--native` が一致すること。
/// 依存パッケージで定義された定数でも同じ経路 (`resolve` → `const_val`) を通る。
const CONST_STR_FNS: &str = "const t.CONFIG: &str = \"hello, kasane\" pub\n\nfn t.len_cfg() -> usize uses [] panics never\n    let s = t.CONFIG\n    s.len()\nend t.len_cfg\n\n";

fn const_str_pkg(name: &str, body: &str) -> Tmp {
    Tmp::new(name, &format!("{}{}", CONST_STR_FNS, main_wrap("", body)))
}

#[test]
fn kir_const_str_match_tree_walker() {
    // 木走査で定数の文字列と長さが正しいこと、参照実行器 / ネイティブでも同じ値になること。
    let t = const_str_pkg("kir_const_str", "    core.mem.drop(std.io.print_line(t.CONFIG))\n    core.check.eq(t.len_cfg(), 13usize)\n");
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", "kir_const_str", text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "hello, kasane", "tree-walker 出力");
    let k = kir_run(&["kir", "t.len_cfg", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", "kir_const_str", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "13", "kir 値");
}

// ---- 参照 (設計_KIR.md 段階 1: `&T` / `&mut T`、`*r`、`*r = v`。kir_lower_refs.rs) ----

/// 参照を使う関数と、整数の引数だけで呼べる包み (`t.w_*`)。`t.main` は含まない。
const REFS_FNS: &str = "fn t.bump(p: &mut i32) -> () uses [] panics may
    *p = *p + 1i32
end t.bump

fn t.twice(n: i32) -> i32 uses [] panics may
    let mut a = n
    t.bump(&mut a)
    t.bump(&mut a)
    a
end t.twice

fn t.peek(r: &i32) -> i32 uses [] panics never
    *r
end t.peek

fn t.peek_mut(a: &mut i32) -> i32 uses [] panics never
    t.peek(a)
end t.peek_mut

fn t.sum2(a: &i32, b: &i32) -> i32 uses [] panics may
    *a + *b
end t.sum2

fn t.swap(a: &mut i32, b: &mut i32) -> () uses [] panics never
    let tmp = *a
    *a = *b
    *b = tmp
end t.swap

fn t.pick['a](a: &'a i32, b: &'a i32, first: bool) -> &'a i32 uses [] panics never
    if first {
        a
    } else {
        b
    }
end t.pick

fn t.temp() -> i32 uses [] panics may
    let r = &(3i32 + 4i32)
    *r * 2i32
end t.temp

fn t.loop_bump(n: i32) -> i32 uses [] panics may
    let mut total = 0i32
    let mut i = 0i32
    while i < n {
        t.bump(&mut total)
        i = i + 1i32
    }
    total
end t.loop_bump

fn t.bump_w(p: &mut i32) -> () uses [] panics never
    let s = (*p).wrapping_add(1i32)
    *p = s
end t.bump_w

fn t.down(p: &mut i32, n: i32) -> () uses [] panics never stack 128
    if n > 0i32 {
        t.bump_w(p)
        t.down(p, n.wrapping_sub(1i32))
    }
end t.down

fn t.assign_branch(n: i32) -> i32 uses [] panics may
    let mut x = 0i32
    if n > 0i32 {
        x = n
    } else {
        x = 0i32 - n
    }
    let r = &mut x
    *r = *r * 10i32
    x
end t.assign_branch

fn t.wrap_ref(v: u8) -> u8 uses [] panics never
    let mut w = v
    let s = w.wrapping_add(200u8)
    let r = &mut w
    *r = s
    w
end t.wrap_ref

fn t.bump8(p: &mut u8) -> () uses [] panics may
    *p = *p + 100u8
end t.bump8

fn t.overflow_via_ref(v: u8) -> u8 uses [] panics may
    let mut w = v
    t.bump8(&mut w)
    w
end t.overflow_via_ref

fn t.reborrow_read(r: &mut i32) -> i32 uses [] panics never
    let s: &i32 = &*r
    *s
end t.reborrow_read

fn t.early(n: i32) -> i32 uses [] panics may
    let mut total = 0i32
    let mut i = 0i32
    while true {
        if i > n {
            break
        }
        total = total + i
        let v = t.peek(&total)
        if v > 100i32 {
            return v
        }
        i = i + 1i32
    }
    total
end t.early

fn t.mixed(n: i32) -> i32 uses [] panics may
    let mut acc = 1i32
    let r = &mut acc
    let mut k = 0i32
    while k < n {
        *r = *r + 2i32
        k = k + 1i32
    }
    acc = acc + 1i32
    t.peek(&acc) + t.reborrow_read(&mut acc)
end t.mixed

fn t.w_swap(a: i32, b: i32) -> i32 uses [] panics never
    let mut x = a
    let mut y = b
    t.swap(&mut x, &mut y)
    x.wrapping_mul(10i32).wrapping_add(y)
end t.w_swap

fn t.w_pick(a: i32, b: i32, first: bool) -> i32 uses [] panics never
    let x = a
    let y = b
    *t.pick(&x, &y, first)
end t.w_pick

fn t.w_peek(n: i32) -> i32 uses [] panics never
    let mut x = n
    t.peek_mut(&mut x)
end t.w_peek

fn t.w_down(n: i32) -> i32 uses [] panics never
    let mut d = 0i32
    t.down(&mut d, n)
    d
end t.w_down

fn t.w_sum(a: i32, b: i32) -> i32 uses [] panics may
    let x = a
    let y = b
    t.sum2(&x, &y)
end t.w_sum
";

/// 参照を使う関数の包みのパッケージを作る。
fn refs_pkg(name: &str, main_body: &str) -> Tmp {
    Tmp::new(name, &format!("{}{}", REFS_FNS, main_wrap("", main_body)))
}

#[test]
fn kir_array_ref_match_tree_walker() {
    // `&[T; N]` / `&mut [T; N]` のパラメータ (ptr)、`&[T; N]` → `&[T]` の fat への暗黙調整、
    // `for x in a` (参照越し) を、木走査と参照実行器・機械語で比べる。
    let fns = r#"
fn t.for_ref(a: &[u32; 3]) -> u32 uses [] panics may
    let mut s = 0u32
    for x in a {
        s = s + x
    }
    s
end t.for_ref

fn t.for_ref_check() -> u32 uses [] panics may
    t.for_ref(&[1u32, 2u32, 3u32])
end t.for_ref_check

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.use_it(), 60u32)
    core.check.eq(t.pass_slice(), 30u32)
    core.check.eq(t.for_ref(&[1u32, 2u32, 3u32]), 6u32)
    core.mem.drop(std.io.print_line("ok"))
    Ok(())
end t.main

fn t.oob() -> u32 uses [] panics may
    let a = [10u32, 20u32, 30u32]
    a[5usize]
end t.oob

fn t.pass_slice() -> u32 uses [] panics may
    let a = [10u32, 20u32, 30u32]
    t.slice_ok(&a)
end t.pass_slice

fn t.set(a: &mut [u32; 3]) -> () uses [] panics may
    a[1usize] = 20u32
end t.set

fn t.slice_ok(s: &[u32]) -> u32 uses [] panics may
    s[0usize] + s[1usize]
end t.slice_ok

fn t.sum(a: &[u32; 3]) -> u32 uses [] panics may
    a[0usize] + a[1usize] + a[2usize]
end t.sum

fn t.use_it() -> u32 uses [] panics may
    let mut a = [10u32, 0u32, 30u32]
    t.set(&mut a)
    t.sum(&a)
end t.use_it
"#;
    let t = Tmp::new("kir_array_ref_ok", fns);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker rc={:?} stdout={:?} stderr={:?}", o.status.code(), text(&o.stdout), text(&o.stderr));

    // 戻り値が u32 で引数を取らない関数は `--run` / `--native` / ASan でも直接比べられる。
    for (f, want) in [("t.use_it", "60"), ("t.pass_slice", "30"), ("t.for_ref_check", "6")] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir {} 出力\n{}", f, text(&k.stdout));
    }

    // 範囲外の添字は wood-walker と同じく panic (101) となる。
    let msg = "index out of bounds: the len is 3 but the index is 5";
    let fns2 = "
fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let _x = t.oob()
    Ok(())
end t.main

fn t.oob() -> u32 uses [] panics may
    let a = [10u32, 20u32, 30u32]
    a[5usize]
end t.oob
";
    let t = Tmp::new("kir_array_ref_oob", fns2);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101), "tree-walker oob rc={:?} stderr={:?}", o.status.code(), text(&o.stderr));
    assert!(text(&o.stderr).contains(&format!("panic: {}", msg)), "tree-walker oob msg\n{}", text(&o.stderr));
    let k = kir_run(&["kir", "t.oob", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(101), "kir oob\n{}", text(&k.stderr));
    assert_eq!(text(&k.stderr).trim(), format!("panic: {}", msg), "kir oob msg\n{}", text(&k.stderr));
}

#[test]
fn kir_refs_values_match_tree_walker() {
    // 木走査 (`ksn run`) の `core.check.eq` が通ること、参照実行器 (`ksn kir --run`) の値が期待どおりであること。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.twice", &["5"], "7i32", "7"),
        ("t.loop_bump", &["6"], "6i32", "6"),
        ("t.temp", &[], "14i32", "14"),
        ("t.assign_branch", &["3"], "30i32", "30"),
        ("t.assign_branch", &["-2"], "20i32", "20"),
        ("t.wrap_ref", &["100"], "44u8", "44"),
        ("t.early", &["3"], "6i32", "6"),
        ("t.early", &["20"], "105i32", "105"),
        ("t.mixed", &["3"], "16i32", "16"),
        ("t.w_swap", &["1", "2"], "21i32", "21"),
        ("t.w_pick", &["2", "7", "false"], "7i32", "7"),
        ("t.w_pick", &["2", "7", "true"], "2i32", "2"),
        ("t.w_peek", &["10"], "10i32", "10"),
        ("t.w_down", &["4"], "4i32", "4"),
        ("t.w_down", &["0"], "0i32", "0"),
        ("t.w_sum", &["3", "4"], "7i32", "7"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.join(", "));
        let t = refs_pkg(&format!("kir_refs_values_{}", i), &format!("    core.check.eq({}, {})\n", call, lit));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).lines().last().unwrap_or("").trim(), *want, "kir value of {}", call);
    }
}

/// `extern` (libc の `abs`)、`unsafe` ブロック、`core.ptr.*` の読み書きを、
/// 木走査・参照実行器・機械語 (`--native`) で比べる。
#[test]
fn kir_ffi_and_ptr_match_tree_walker() {
    let src = r#"extern t.abs(x: i32) -> i32 link "abs"

fn t.abs_extern(x: i32) -> i32 uses [] panics never
    unsafe "abs" "libc abs" {
        t.abs(x)
    }
end t.abs_extern

fn t.ptr_rw() -> i32 uses [] panics never
    let mut x = 42i32
    let p = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.from_mut(&mut x)
    }
    unsafe "ptr" "read or write through a raw pointer" {
        core.ptr.write(p, 99i32)
    }
    unsafe "ptr" "read or write through a raw pointer" {
        core.ptr.read(p)
    }
end t.ptr_rw

fn t.null_check() -> bool uses [] panics never
    let p: *mut i32 = unsafe "ptr" "make or convert a raw pointer" {
        core.ptr.null_mut()
    }
    unsafe "ptr" "null check" {
        core.ptr.is_null(p)
    }
end t.null_check

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.abs_extern(-7i32), 7i32)
    core.check.eq(t.ptr_rw(), 99i32)
    core.check.is_true(t.null_check())
    std.io.print_line("ffi ok")
end t.main
"#;
    let t = Tmp::new("kir_ffi_and_ptr", src);
    // `unsafe` / `extern` は `[unsafe] modules` が必要。
    fs::write(
        t.0.join("kasane.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n\n[unsafe]\nmodules = [\"t\"]\n",
    )
    .unwrap();
    // 木走査 (`ksn run`) の main 検査が通ること。
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker main\n{}", text(&o.stderr));
    // `t.abs_extern`, `t.ptr_rw`, `t.null_check` を木走査・参照実行器・機械語で比べる。
    for (f, args, want) in [
        ("t.abs_extern", &["-7"] as &[&str], "7"),
        ("t.ptr_rw", &[] as &[&str], "99"),
        ("t.null_check", &[] as &[&str], "true"),
    ] {
        let call = format!("{}({})", f, args.join(", "));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", call);
    }
}

#[test]
fn kir_scalar_floats_and_wide_ints_match_tree_walker() {
    // 木走査で `core.check.eq` が通ること (値は木走査の結果と同じ)、参照実行器の値が期待どおりであること。
    // 超越関数と f32 の値はビット列で比べる (fmath を含むので、木走査と 1 ビットも違わないことを確かめる)。
    // (関数, 木走査で呼ぶ式, 木走査の期待値, 参照実行器の期待値)
    let cases: &[(&str, &str, &str, &str)] = &[
        ("t.a_fl1", "t.a_fl1()", "7.0f64", "7"),
        ("t.a_nan", "t.a_nan()", "false", "false"),
        ("t.a_trig", "t.a_trig()", "4616504293205964284u64", "4616504293205964284"),
        ("t.a_f32trig", "t.a_f32trig()", "1073967664u32", "1073967664"),
        ("t.a_conv4", "t.a_conv4()", "1036831949u32", "1036831949"),
        ("t.a_conv2", "t.a_conv2()", "7.0f64", "7"),
        ("t.a_conv3", "t.a_conv3()", "4607182418800017408u64", "4607182418800017408"),
        ("t.a_conv1", "t.a_conv1()", "-5i64", "-5"),
        ("t.a_fromu128", "t.a_fromu128()", "3.402823669209385e38f64", "340282366920938500000000000000000000000"),
        ("t.a_big", "t.a_big()", "-170141183460469231731687303715884105728i128", "-170141183460469231731687303715884105728"),
        ("t.a_umax", "t.a_umax().wrapping_add(2u128)", "1u128", "340282366920938463463374607431768211455"),
        ("t.a_ucmp", "t.a_ucmp()", "true", "true"),
        ("t.a_satadd128", "t.a_satadd128()", "-170141183460469231731687303715884105728i128", "-170141183460469231731687303715884105728"),
        ("t.a_satneg128", "t.a_satneg128()", "-170141183460469231731687303715884105728i128", "-170141183460469231731687303715884105728"),
    ];
    for (i, (f, call, want, kwant)) in cases.iter().enumerate() {
        let t = scalar_pkg(&format!("kir_scalar_float_{}", i), &format!("    core.check.eq({}, {})\n", call, want));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let k = kir_run(&["kir", *f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *kwant, "kir value of {}", f);
    }
}

#[test]
fn kir_agg_module_text_declares_the_layout() {
    // 型の宣言の行 (大きさ・整列・フィールドのオフセット) と、本体の命令が出る。出力は読み直せる (cmd_kir が確かめる)。
    let t = Tmp::new("kir_agg_text", KIR_AGG_SRC);
    let o = run(&["kir", "t.walk"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("agg#0 struct t.Pt size=8 align=4 {x:i32@0 y:i32@4}"), "{}", out);
    assert!(out.contains("= setfield %"), "{}", out);
}

#[test]
fn kir_agg_refuses_generic_types_with_a_reason() {
    // 型引数付きの構造体は単相化して下ろせる (raw_buf.rs)。型引数が段階 1 の型に入らないもの (標準ライブラリの未対応の型) は、その関数が理由付きで未対応になる。
    let src = "type t.G[T]
    v: T
end t.G

fn t.gv(_g: t.G<std.sync.Mutex<i32>>) -> i32 uses [] panics never
    0i32
end t.gv

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main
";
    let t = Tmp::new("kir_agg_refuse", src);
    let o = run(&["kir", "t.gv"], &t.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("t.gv は未対応"), "{}", text(&o.stderr));
}

#[test]
fn kir_agg_entry_with_an_aggregate_value_is_refused() {
    // 入口 (`--run`) は整数と bool の引数・戻り値だけ。集成体の関数は表示だけできる。
    let t = Tmp::new("kir_agg_entry", KIR_AGG_SRC);
    let o = kir_run(&["kir", "t.mk_box", "--run", "1", "2", "true"], &t.0);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("集成体を引数・戻り値に持つ"), "{}", text(&o.stderr));
}

#[test]
fn kir_scalar_text_reads_back_with_float_and_bit_ops() {
    // `ksn kir` は出力を読み直して同じ文字列になることを確かめてから出す。浮動小数の命令と再解釈の表記を含む。
    let t = scalar_pkg("kir_scalar_text", "");
    let o = run(&["kir", "t.a_trig"], &t.0);

    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("fsin.f64"), "{}", out);
    assert!(out.contains("fpowi.f64"), "{}", out);
    assert!(out.contains("bitcast.u64.f64"), "{}", out);
}

#[test]
fn kir_scalar_reports_unsupported_numeric_methods_with_a_reason() {
    // `trunc_to_i128` は 128 ビットの `Option` なので段階 1 の型に入らない (64 ビット以下の `trunc_to_*` は `kir_native_gaps.rs`)。理由を付けて拒否する。
    let src = "fn t.a_tr(a: f64) -> bool uses [] panics never
    match a.trunc_to_i128() {
        Some(_v) => true
        None => false
    }
end t.a_tr
";
    let t = Tmp::new("kir_scalar_unsupported", src);
    let o = run(&["kir", "t.a_tr"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("t.a_tr は未対応"), "{}", text(&o.stderr));
}

// ---- 数値の組み合わせの探索 (NaN・符号付き零・丸め・飽和・128 ビットの境界): kir_scalar_probes_* ----

/// 数値の関数の探索用の定義。名前の順 (E0020)。main は別に付ける。
const SCALAR_PROBES: &str = r#"fn t.q01() -> u32 uses [] panics never
    let a = 1.1f32
    let b = 3.3f32
    (a * b - a / b + a - b).to_bits()
end t.q01

fn t.q02() -> u32 uses [] panics never
    let z = 0.0f32
    (1.0f32 / z).to_bits()
end t.q02

fn t.q03() -> u64 uses [] panics never
    let x = 2.5f64
    (-x).abs().to_bits()
end t.q03

fn t.q04() -> u64 uses [] panics never
    let n = 0.0f64 / 0.0f64
    n.max(1.0f64).to_bits()
end t.q04

fn t.q05() -> u64 uses [] panics never
    let x = 7.5f64
    x.rem(2.0f64).to_bits()
end t.q05

fn t.q06() -> u64 uses [] panics never
    let x = 2.5f64
    let y = 3.5f64
    (x.round_ties_even() + y.round_ties_even()).to_bits()
end t.q06

fn t.q07() -> u64 uses [] panics never
    let x = -2.5f64
    (x.floor() * 100.0f64 + x.ceil() * 10.0f64 + x.trunc()).to_bits()
end t.q07

fn t.q08() -> bool uses [] panics never
    let n = 0.0f64 / 0.0f64
    let i = 1.0f64 / 0.0f64
    n.is_nan() && !i.is_finite()
end t.q08

fn t.q09() -> u64 uses [] panics never
    2.0f64.powi(-3i32).to_bits()
end t.q09

fn t.q10() -> u64 uses [] panics never
    let x = 12.5f64
    (x.sin() * x.cos() + x.tan() - x.exp().ln()).to_bits()
end t.q10

fn t.q11() -> u32 uses [] panics never
    let x = 0.1f32
    (x.to_f64() * 3.0f64).nearest_to_f32().to_bits()
end t.q11

fn t.q12() -> i128 uses [] panics never
    let a = -170141183460469231731687303715884105727i128
    a.wrapping_neg()
end t.q12

fn t.q13() -> u128 uses [] panics never
    let a = 0u128
    (!a).wrapping_add(2u128)
end t.q13

fn t.q14() -> u32 uses [] panics never
    let a = -1i8
    a.leading_zeros().wrapping_add((-1i128).count_ones())
end t.q14

fn t.q15() -> u32 uses [] panics never
    let a = 0i64
    a.trailing_zeros()
end t.q15

fn t.q16() -> i128 uses [] panics never
    let a = -256i128
    a.wrapping_shr(4u32)
end t.q16

fn t.q17() -> u128 uses [] panics never
    let a = 0u128.wrapping_sub(1u128)
    let b = a.wrapping_shr(1u32)
    b.min(a)
end t.q17

fn t.q18() -> i64 uses [] panics never
    i64.from_u32(4000000000u32)
end t.q18

fn t.q19() -> usize uses [] panics never
    usize.from_u8(255u8)
end t.q19

fn t.q20() -> u64 uses [] panics never
    f64.from_i64_nearest(9007199254740993i64).to_bits()
end t.q20

fn t.q21() -> u32 uses [] panics never
    f32.from_i32_nearest(16777217i32).to_bits()
end t.q21

fn t.q22() -> u64 uses [] panics never
    let a = -7i32
    a.to_f64().to_bits()
end t.q22

fn t.q23() -> i8 uses [] panics never
    let a = -128i8
    a.saturating_mul(-1i8).wrapping_add(a.saturating_sub(1i8))
end t.q23

fn t.q24() -> u32 uses [] panics never
    let a = 1u32
    a.wrapping_shl(35u32)
end t.q24

fn t.q25() -> bool uses [] panics never
    let x = 1.0f32
    let y = 2.0f32
    (x < y) && (y >= 2.0f32) && (x != y)
end t.q25

fn t.q26() -> bool uses [] panics never
    0.0f32 == -0.0f32
end t.q26

fn t.q27() -> u64 uses [] panics never
    let x = 1.0e300f64
    (x * x).to_bits()
end t.q27

fn t.q28() -> i64 uses [] panics never
    let a = 5i64
    a.wrapping_mul(-3i64).max(a.wrapping_neg())
end t.q28
"#;

#[test]
fn kir_scalar_probes_match_tree_walker() {
    // (関数, 木走査の期待値, 参照実行器の期待値)。木走査の期待値は参照実行器の値と同じであることを、別々の経路で確かめる。
    let cases: &[(&str, &str, &str)] = &[
        ("t.q01", "1066164118u32", "1066164118"),
        ("t.q02", "2139095040u32", "2139095040"),
        ("t.q03", "4612811918334230528u64", "4612811918334230528"),
        ("t.q04", "4607182418800017408u64", "4607182418800017408"),
        ("t.q05", "4609434218613702656u64", "4609434218613702656"),
        ("t.q06", "4618441417868443648u64", "4618441417868443648"),
        ("t.q07", "13867744336952688640u64", "13867744336952688640"),
        ("t.q08", "true", "true"),
        ("t.q09", "4593671619917905920u64", "4593671619917905920"),
        ("t.q10", "13846673201326737920u64", "13846673201326737920"),
        ("t.q11", "1050253722u32", "1050253722"),
        ("t.q12", "170141183460469231731687303715884105727i128", "170141183460469231731687303715884105727"),
        ("t.q13", "1u128", "1"),
        ("t.q14", "128u32", "128"),
        ("t.q15", "64u32", "64"),
        ("t.q16", "-16i128", "-16"),
        ("t.q17", "170141183460469231731687303715884105727u128", "170141183460469231731687303715884105727"),
        ("t.q18", "4000000000i64", "4000000000"),
        ("t.q19", "255usize", "255"),
        ("t.q20", "4845873199050653696u64", "4845873199050653696"),
        ("t.q21", "1266679808u32", "1266679808"),
        ("t.q22", "13842939354630062080u64", "13842939354630062080"),
        ("t.q23", "-1i8", "-1"),
        ("t.q24", "8u32", "8"),
        ("t.q25", "true", "true"),
        ("t.q26", "true", "true"),
        ("t.q27", "9218868437227405312u64", "9218868437227405312"),
        ("t.q28", "-5i64", "-5"),
    ];
    let mut main = String::new();
    for (f, want, _) in cases {
        main.push_str(&format!("    core.check.eq({}(), {})\n", f, want));
    }
    let t = Tmp::new("kir_scalar_probes", &format!("fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n{}    Ok(())\nend t.main\n\n{}", main, SCALAR_PROBES));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "tree-walker:\n{}", text(&o.stderr));
    for (f, _, want) in cases {
        let k = kir_run(&["kir", *f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", f);
    }
}

#[test]
fn kir_refs_panics_match_tree_walker() {
    // 参照の先への書き込みで起きた溢れは、木走査と同じ文言と終了コード (101) になること。
    let t = refs_pkg("kir_refs_panic", "    let _x = t.overflow_via_ref(200u8)\n");
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101), "tree-walker\n{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("panic: attempt to add with overflow"), "{}", text(&o.stderr));
    let k = kir_run(&["kir", "t.overflow_via_ref", "--run", "200"], &t.0);
    assert_eq!(k.status.code(), Some(101), "kir\n{}", text(&k.stderr));
    assert_eq!(text(&k.stderr).trim(), "panic: attempt to add with overflow");
}

#[test]
fn kir_refs_text_shows_slots_and_reference_types() {
    // 出力は `kir 1` の形で、読み直して検査が通る (cli の内部検査)。スタックスロット・参照の複製・参照の型が見える。
    let t = refs_pkg("kir_refs_text", "    let _x = t.twice(1i32)\n");
    let o = run(&["kir", "t.assign_branch"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let s = text(&o.stdout);
    for want in ["alloca", "store %", "load %", "reborrow %", "%2: mutref<i32> = alloca"] {
        assert!(s.contains(want), "missing `{}` in:\n{}", want, s);
    }
    let p = run(&["kir", "t.pick"], &t.0);
    assert_eq!(p.status.code(), Some(0), "{}", text(&p.stderr));
    assert!(text(&p.stdout).contains("ret=ref<i32>"), "{}", text(&p.stdout));
}

#[test]
fn kir_refs_reference_to_unsupported_pointee_keeps_its_reason() {
    // 指す先が整数・浮動小数・bool・利用者の構造体でない参照 (標準の型) は、従来どおり理由付きで未対応になる。
    let src = "fn t.first(s: &std.io.Stdout) -> u8 uses [] panics never
    let _t = s
    0u8
end t.first
";
    let t = Tmp::new("kir_refs_str", src);
    let o = run(&["kir", "t.first"], &t.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("t.first は未対応"), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("段階 1 の対象外"), "{}", text(&o.stderr));
}


// ---- KIR の段階 1 の拡張: Vec・String・&str・for・添字・クロージャ (kir/collections、設計_KIR.md 10.9) ----

/// コレクション系の試験用の関数。値は木走査 (`ksn run`) と参照実行器 (`ksn kir --run`) で比べる。
const KIR_COLL_FNS: &str = "fn t.vlen() -> usize uses [] panics may
    let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    v.len()
end t.vlen

fn t.vempty() -> bool uses [] panics never
    let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    v.is_empty()
end t.vempty

fn t.str_len() -> usize uses [] panics never
    let s = \"héllo\"
    s.len()
end t.str_len

fn t.string_len() -> usize uses [] panics never
    let s = alloc.string.String.new(std.alloc.SYSTEM)
    s.len()
end t.string_len

fn t.pick(c: bool) -> usize uses [] panics never
    let s = if c {
        \"ab\"
    } else {
        \"cde\"
    }
    s.len()
end t.pick

fn t.sum_skip(n: usize) -> usize uses [] panics may
    let mut total = 0usize
    for i in core.iter.range(0usize, n) {
        if i == 3usize {
            continue
        }
        total = total + i
    }
    total
end t.sum_skip

fn t.first_at(n: usize) -> usize uses [] panics may
    let mut k = 0usize
    for i in core.iter.range(0usize, n) {
        let s = alloc.string.String.new(std.alloc.SYSTEM)
        if i == 2usize {
            return k.wrapping_add(s.len())
        }
        k = k.wrapping_add(i)
    }
    k
end t.first_at

fn t.brk(n: u32) -> u32 uses [] panics may
    let mut last = 0u32
    for i in core.iter.range(0u32, n) {
        last = i
        if i == 4u32 {
            break
        }
    }
    last
end t.brk

fn t.empty_loop(n: u32) -> u32 uses [] panics may
    let v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut total = n
    for x in v {
        if x == 7u32 {
            return 99u32
        }
        total = total.wrapping_add(x)
    }
    total
end t.empty_loop

fn t.find(n: usize) -> usize uses [] panics may
    let mut i = 0usize
    while i < 10usize {
        for j in core.iter.range(0usize, 10usize) {
            if i.wrapping_mul(j) == n {
                return i.wrapping_add(j)
            }
        }
        i = i.wrapping_add(1usize)
    }
    0usize
end t.find

fn t.nested(n: usize) -> usize uses [] panics may
    let mut total = 0usize
    for i in core.iter.range(0usize, n) {
        let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
        if !v.is_empty() {
            continue
        }
        for j in core.iter.range(0usize, i) {
            if j == 2usize {
                break
            }
            total = total + j
        }
        total = total + 100usize
    }
    total
end t.nested

fn t.idx_empty() -> u8 uses [] panics may
    let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    v[5usize]
end t.idx_empty

fn t.clos_let() -> u32 uses [] panics never
    let add = fn(a: u32, b: u32) -> u32 capture [] {
        a.wrapping_add(b)
    }
    let k = add(3u32, 4u32)
    add(k, 1u32)
end t.clos_let

fn t.apply_n[F: core.ops.Fn<(u64,), u64, E, P>, E: uses, P: panics](f: F, n: u64) -> u64 uses [E] panics P
    let mut acc = 0u64
    let mut i = 0u64
    while i < n {
        acc = acc.wrapping_add(f(i))
        i = i.wrapping_add(1u64)
    }
    acc
end t.apply_n

fn t.total(n: u64) -> u64 uses [] panics may
    t.apply_n(
        fn(x: u64) -> u64 capture [] {
            x.wrapping_mul(3u64).wrapping_add(1u64)
        },
        n,
    )
end t.total

";

/// `KIR_COLL_FNS` に `main` を付けたパッケージを作る。
fn kir_coll_pkg(name: &str, main_body: &str) -> Tmp {
    Tmp::new(name, &format!("{}{}", KIR_COLL_FNS, main_wrap("", main_body)))
}

#[test]
fn kir_coll_values_match_tree_walker() {
    // 木走査 (`ksn run`) の `core.check.eq` が通り、参照実行器 (`ksn kir --run`) の値が期待どおりであること。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.vlen", &[], "0usize", "0"),
        ("t.vempty", &[], "true", "true"),
        ("t.str_len", &[], "6usize", "6"),
        ("t.string_len", &[], "0usize", "0"),
        ("t.pick", &["true"], "2usize", "2"),
        ("t.pick", &["false"], "3usize", "3"),
        ("t.sum_skip", &["5"], "7usize", "7"),
        ("t.sum_skip", &["0"], "0usize", "0"),
        ("t.first_at", &["5"], "1usize", "1"),
        ("t.first_at", &["1"], "0usize", "0"),
        ("t.brk", &["10"], "4u32", "4"),
        ("t.brk", &["3"], "2u32", "2"),
        ("t.find", &["6"], "7usize", "7"),
        ("t.find", &["97"], "0usize", "0"),
        ("t.nested", &["4"], "402usize", "402"),
        ("t.empty_loop", &["4"], "4u32", "4"),
        ("t.clos_let", &[], "8u32", "8"),
        ("t.total", &["10"], "145u64", "145"),
        ("t.total", &["0"], "0u64", "0"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.join(", "));
        let t = kir_coll_pkg(&format!("kir_coll_values_{}", i), &format!("    core.check.eq({}, {})\n", call, lit));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_str_for_matches_tree_walker() {
    // `for x in s.split(sep)` / `for c in s.chars()` (`s` は `&str` = fat) が、木走査と
    // 参照実行器・機械語で同じ結果になること。空の区切り・末尾・連続・マルチバイトを含む。
    let split = |s: &str, sep: &str| -> String {
        format!("fn t.f() -> usize uses [] panics never\n    let mut n = 0usize\n    for _ in {:?}.split({:?}) {{\n        n = n.wrapping_add(1usize)\n    }}\n    n\nend t.f\n", s, sep)
    };
    let chars = |s: &str| -> String {
        format!("fn t.f() -> usize uses [] panics never\n    let mut n = 0usize\n    for _ in {:?}.chars() {{\n        n = n.wrapping_add(1usize)\n    }}\n    n\nend t.f\n", s)
    };
    let cases: &[(&str, &str)] = &[
        (&split("a,b,c", ","), "3"),
        (&split("", ","), "1"),
        (&split("a,,b", ","), "3"),
        (&split("a,", ","), "2"),
        (&split(",a", ","), "2"),
        (&split("ab", ","), "1"),
        (&split("", ""), "1"),
        (&split("a,b", ""), "1"),
        (&split("x\r\ny", "\r\n"), "2"),
        (&chars(""), "0"),
        (&chars("abc"), "3"),
        (&chars("あいう"), "3"),
        (&chars("aあb"), "3"),
    ];
    for (i, (src, want)) in cases.iter().enumerate() {
        let t = Tmp::new(&format!("kir_str_for_{}", i), &format!("{}{}", src, main_wrap("", &format!("    core.check.eq(t.f(), {})\n", want))));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", want, text(&o.stderr));
        let k = kir_run(&["kir", "t.f", "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", want, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", want);
    }
}

#[test]
fn kir_coll_index_panics_like_tree_walker() {
    // 空の Vec の添字は木走査と同じ文言で panic する (終了コード 101)。
    let t = kir_coll_pkg("kir_coll_index_panic", "    let _x = t.idx_empty()\n");
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(101), "tree-walker");
    assert!(text(&o.stderr).contains("panic: index out of bounds: the len is 0 but the index is 5"), "{}", text(&o.stderr));
    let k = kir_run(&["kir", "t.idx_empty", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(101), "kir: {}", text(&k.stderr));
    assert_eq!(text(&k.stderr).trim(), "panic: index out of bounds: the len is 0 but the index is 5");
}

#[test]
fn kir_coll_closure_is_monomorphized_per_closure() {
    // `Fn` 境界の関数は、渡された捕獲のないクロージャごとに 1 つの単相インスタンスになる。
    let t = kir_coll_pkg("kir_coll_mono", "    core.check.eq(t.total(3u64), 12u64)\n");
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
    let o = run(&["kir", "t.total"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("fn t.apply_n key=t.apply_n<t.total#c") && out.contains("call t.apply_n<t.total#c"), "{}", out);
    assert!(out.contains("fn t.total#c") && out.contains("end t.total#c"), "{}", out);
}

#[test]
fn kir_coll_reports_unsupported_constructs_with_a_reason() {
    // `Vec` の `remove` / `insert` はスカラー要素なら段階 1 で下ろせる (`push` / `pop` と同様)。
    // capture を持つクロージャは、理由を付けて拒否する。
    let cases: &[(&str, &str, &str)] = &[
        ("t.captured", "    let v: Option<alloc.vec.Vec<u8, std.alloc.System>> = Some(alloc.vec.Vec.new(std.alloc.SYSTEM))\n    let f = fn(x: usize) -> usize capture [move v] {\n        x\n    }\n    f(1usize)\n", "解放の要る値を持つ"),
    ];
    for (i, (f, body, why)) in cases.iter().enumerate() {
        let src = format!(
            "fn {}() -> usize uses [alloc] panics may\n{}end {}\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics may\n    Ok(())\nend t.main\n",
            f, body, f
        );
        let t = Tmp::new(&format!("kir_coll_reason_{}", i), &src);
        let o = run(&["kir", f], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}", f);
        assert!(text(&o.stderr).contains(why), "{}: {}", f, text(&o.stderr));
    }
}

#[test]
fn kir_coll_dump_shows_drops_and_static_data() {
    // 所有値の解放は `free` の命令として見える。文字列リテラルは `data` の静的な領域。
    let t = kir_coll_pkg("kir_coll_dump", "    core.check.eq(t.vlen(), 0usize)\n");
    let o = run(&["kir", "t.vlen"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("frame.bytes 24") && out.contains("heap.free %") && out.contains("mem.load.usize %"), "{}", out);
    let s = run(&["kir", "t.str_len"], &t.0);
    assert!(text(&s.stdout).contains("static.bytes 68c3a96c6c6f"), "{}", text(&s.stdout));
}

// ---- 集成体への参照とスライス・&str (設計_KIR.md 10.10、kir/agg-refs) ----

/// 集成体への `&` / `&mut`、入れ子のフィールド、スライス、`&str` の試験用の関数。
const AGG_REFS_FNS: &str = "type t.P
    x: i32
    y: i32
end t.P

type t.Line
    a: t.P
    b: t.P
    len: u32
end t.Line

fn t.p_sum(p: &t.P) -> i32 uses [] panics may
    p.x + p.y
end t.p_sum

fn t.p_bump(p: &mut t.P, d: i32) -> () uses [] panics may
    p.x = p.x + d
    p.y = p.y + d
end t.p_bump

fn t.p_run(a: i32, b: i32) -> i32 uses [] panics may
    let mut p = t.P { x: a, y: b }
    t.p_bump(&mut p, 10i32)
    t.p_bump(&mut p, 1i32)
    t.p_sum(&p)
end t.p_run

fn t.p_via_ref(a: i32) -> i32 uses [] panics may
    let mut p = t.P { x: a, y: 1i32 }
    let r = &mut p
    r.x = r.x * 2i32
    t.p_bump(r, 3i32)
    t.p_sum(r)
end t.p_via_ref

fn t.px(p: &mut t.P) -> &mut i32 uses [] panics never
    &mut p.x
end t.px

fn t.p_field_ref(a: i32) -> i32 uses [] panics may
    let mut p = t.P { x: 0i32, y: 5i32 }
    *t.px(&mut p) = a
    p.x * 100i32 + p.y
end t.p_field_ref

fn t.line_move(l: &mut t.Line, dx: i32) -> () uses [] panics may
    l.a.x = l.a.x + dx
    l.b.x = l.b.x + dx
    l.len = l.len + 1u32
end t.line_move

fn t.line_span(l: &t.Line) -> i32 uses [] panics may
    l.b.x - l.a.x + l.b.y
end t.line_span

fn t.line_run(n: i32) -> i32 uses [] panics may
    let mut l = t.Line { a: t.P { x: 1i32, y: 2i32 }, b: t.P { x: 5i32, y: 9i32 }, len: 0u32 }
    let mut i = 0i32
    while i < n {
        t.line_move(&mut l, 3i32)
        i = i + 1i32
    }
    t.p_bump(&mut l.b, 100i32)
    t.line_span(&l) * 1000i32 + l.a.x
end t.line_run

fn t.line_len(n: u32) -> u32 uses [] panics may
    let mut l = t.Line { a: t.P { x: 0i32, y: 0i32 }, b: t.P { x: 0i32, y: 0i32 }, len: 0u32 }
    let mut i = 0u32
    while i < n {
        t.line_move(&mut l, 1i32)
        i = i + 1u32
    }
    l.len
end t.line_len

fn t.s_len(s: &str) -> usize uses [] panics never
    s.len()
end t.s_len

fn t.s_len_lit() -> usize uses [] panics never
    t.s_len(\"héllo\")
end t.s_len_lit

fn t.s_len_empty() -> usize uses [] panics never
    t.s_len(\"\")
end t.s_len_empty

fn t.b_at(s: &[u8], i: usize) -> u8 uses [] panics may
    s[i]
end t.b_at

fn t.b_sum(s: &[u8]) -> u8 uses [] panics may
    let mut acc = 0u8
    for i in core.iter.range(0usize, s.len()) {
        acc = acc.wrapping_add(s[i])
    }
    acc
end t.b_sum

fn t.pass_through(s: &str) -> &str uses [] panics never
    s
end t.pass_through

fn t.str_byte(s: &str, i: usize) -> u8 uses [] panics may
    let b = s.as_bytes()
    t.b_at(b, i)
end t.str_byte

fn t.lit_byte(i: usize) -> u8 uses [] panics may
    t.str_byte(\"héllo\", i)
end t.lit_byte

fn t.lit_sum() -> u8 uses [] panics may
    t.b_sum(\"abc\".as_bytes())
end t.lit_sum

fn t.lit_sum_empty() -> u8 uses [] panics may
    t.b_sum(\"\".as_bytes())
end t.lit_sum_empty

fn t.lit_last() -> u8 uses [] panics may
    let b = \"abc\".as_bytes()
    b[b.len() - 1usize]
end t.lit_last

fn t.lit_oob() -> u8 uses [] panics may
    let b = \"abc\".as_bytes()
    b[3usize]
end t.lit_oob

fn t.empty_oob() -> u8 uses [] panics may
    t.str_byte(\"\", 0usize)
end t.empty_oob

fn t.pass_len(n: u32) -> usize uses [] panics never
    let s = if n > 1u32 {
        t.pass_through(\"abcd\")
    } else {
        \"x\"
    }
    s.len()
end t.pass_len

fn t.is_hex(c: u8) -> bool uses [] panics never
    (c >= b'0' && c <= b'9') || (c >= b'a' && c <= b'f') || (c >= b'A' && c <= b'F')
end t.is_hex

fn t.hex_of(c: u8) -> u32 uses [] panics never
    if t.is_hex(c) {
        1u32
    } else {
        0u32
    }
end t.hex_of

fn t.digits(src: &[u8], pos: usize) -> usize uses [] panics never
    let mut i = pos
    loop {
        match src.get(i) {
            Some(c) => {
                if c >= b'0' && c <= b'9' {
                    i = i.wrapping_add(1usize)
                } else {
                    break
                }
            }
            None => break
        }
    }
    i
end t.digits

fn t.digits_at(n: usize) -> usize uses [] panics never
    t.digits(\"12a45\".as_bytes(), n)
end t.digits_at

fn t.skip_ws(src: &[u8], pos: usize) -> usize uses [] panics never
    let mut i = pos
    loop {
        match src.get(i) {
            Some(c) => {
                if c == b' ' || c == b'\\n' || c == b'\\t' || c == b'\\r' {
                    i = i.wrapping_add(1usize)
                } else {
                    break
                }
            }
            None => break
        }
    }
    i
end t.skip_ws

fn t.skip_at(n: usize) -> usize uses [] panics never
    t.skip_ws(\" \\t\\n x\".as_bytes(), n)
end t.skip_at

fn t.str_digits(s: &str, n: usize) -> usize uses [] panics never
    t.digits(s.as_bytes(), n)
end t.str_digits

fn t.str_digits_empty(n: usize) -> usize uses [] panics never
    t.str_digits(\"\", n)
end t.str_digits_empty

type t.Doc
    links: alloc.vec.Vec<usize, std.alloc.System>
    nodes: alloc.vec.Vec<u8, std.alloc.System>
    n: u32
end t.Doc

fn t.doc_count(d: &t.Doc) -> usize uses [] panics may
    d.links.len() + d.nodes.len()
end t.doc_count

fn t.doc_bump(d: &mut t.Doc) -> () uses [] panics may
    d.n = d.n + 1u32
end t.doc_bump

fn t.doc_empty(d: &t.Doc) -> bool uses [] panics never
    d.nodes.is_empty()
end t.doc_empty

fn t.doc_by_ref(d: &mut t.Doc, k: u32) -> usize uses [] panics may
    let mut i = 0u32
    while i < k {
        t.doc_bump(d)
        i = i + 1u32
    }
    t.doc_count(d) + 10usize
end t.doc_by_ref

fn t.doc_n(d: &t.Doc) -> u32 uses [] panics never
    d.n
end t.doc_n

fn t.doc_run(k: u32) -> u32 uses [] panics may
    let a: alloc.vec.Vec<usize, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let b: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut d = t.Doc { links: a, nodes: b, n: 0u32 }
    core.mem.drop(t.doc_by_ref(&mut d, k))
    t.doc_n(&d)
end t.doc_run

fn t.vec_len(v: &alloc.vec.Vec<u8, std.alloc.System>) -> usize uses [] panics never
    v.len()
end t.vec_len

fn t.vec_first(v: &alloc.vec.Vec<u8, std.alloc.System>) -> u8 uses [] panics may
    v[0usize]
end t.vec_first

fn t.vec_run() -> usize uses [] panics may
    let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    t.vec_len(&v)
end t.vec_run

fn t.vec_oob() -> u8 uses [] panics may
    let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    t.vec_first(&v)
end t.vec_oob

fn t.vec_empty_loop() -> usize uses [] panics may
    let v: alloc.vec.Vec<u8, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let len = t.vec_len(&v)
    let mut n = 0usize
    for _x in v {
        n = n + 1usize
    }
    n + len
end t.vec_empty_loop

";

/// `AGG_REFS_FNS` に `main` を付けたパッケージを作る。
fn agg_refs_pkg(name: &str, main_body: &str) -> Tmp {
    Tmp::new(name, &format!("{}{}", AGG_REFS_FNS, main_wrap("", main_body)))
}

#[test]
fn kir_agg_refs_values_match_tree_walker() {
    // 木走査 (`ksn run`) の `core.check.eq` が通ること、参照実行器 (`ksn kir --run`) の値が期待どおりであること。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.p_run", &["1", "2"], "25i32", "25"),
        ("t.p_run", &["-5", "5"], "22i32", "22"),
        ("t.p_via_ref", &["4"], "15i32", "15"),
        ("t.p_field_ref", &["7"], "705i32", "705"),
        ("t.line_run", &["0"], "213001i32", "213001"),
        ("t.line_run", &["3"], "213010i32", "213010"),
        ("t.line_len", &["0"], "0u32", "0"),
        ("t.line_len", &["5"], "5u32", "5"),
        ("t.s_len_lit", &[], "6usize", "6"),
        ("t.s_len_empty", &[], "0usize", "0"),
        ("t.lit_byte", &["0"], "104u8", "104"),
        ("t.lit_byte", &["1"], "195u8", "195"),
        ("t.lit_byte", &["5"], "111u8", "111"),
        ("t.lit_sum", &[], "38u8", "38"),
        ("t.lit_sum_empty", &[], "0u8", "0"),
        ("t.lit_last", &[], "99u8", "99"),
        ("t.pass_len", &["0"], "1usize", "1"),
        ("t.pass_len", &["5"], "4usize", "4"),
        ("t.hex_of", &["102"], "1u32", "1"),
        ("t.hex_of", &["103"], "0u32", "0"),
        ("t.hex_of", &["65"], "1u32", "1"),
        ("t.hex_of", &["47"], "0u32", "0"),
        ("t.digits_at", &["0"], "2usize", "2"),
        ("t.digits_at", &["2"], "2usize", "2"),
        ("t.digits_at", &["3"], "5usize", "5"),
        ("t.digits_at", &["9"], "9usize", "9"),
        ("t.skip_at", &["0"], "4usize", "4"),
        ("t.skip_at", &["4"], "4usize", "4"),
        ("t.skip_at", &["9"], "9usize", "9"),
        ("t.str_digits_empty", &["0"], "0usize", "0"),
        ("t.vec_run", &[], "0usize", "0"),
        ("t.vec_empty_loop", &[], "0usize", "0"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.join(", "));
        let t = agg_refs_pkg(&format!("kir_agg_refs_values_{}", i), &format!("    core.check.eq({}, {})\n", call, lit));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).lines().last().unwrap_or("").trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_agg_get_match_tree_walker() {
    // 構造体の Vec の `get(i)` が `Option<構造体>` を返し、`match` で分解でき、
    // 木走査 (`ksn run`) と参照実行器 / 機械語 (`ksn kir --run` / `--native`) が一致する。
    let fns = "type t.Item derive [Copy, Clone]\n    kind: u8\n    v: u32\nend t.Item\n\n\
               fn t.make(n: u32) -> alloc.vec.Vec<t.Item, std.alloc.System> uses [alloc] panics never\n    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    let mut i = 0u32\n    while i < n {\n        core.mem.drop(v.push(t.Item { kind: 1u8, v: i.wrapping_mul(7u32) }))\n        i = i.wrapping_add(1u32)\n    }\n    v\nend t.make\n\n\
               fn t.pick(i: usize) -> u32 uses [alloc] panics never\n    let v = t.make(4u32)\n    match v.get(i) {\n        None => 0u32\n        Some(t.Item { kind: _, v: x }) => x\n    }\nend t.pick\n";
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.pick", &["2"], "14u32", "14"),
        ("t.pick", &["9"], "0u32", "0"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.join(", "));
        let body = format!("    core.check.eq({}, {})\n", call, lit);
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_agg_get_{}", i), &format!("{}{}", fns, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_agg_refs_bounds_panic_like_tree_walker() {
    // スライスと Vec の範囲外の添字 (末尾の次・空) は、木走査と同じ文言で panic する (終了コード 101)。
    let cases: &[(&str, &str)] = &[
        ("t.lit_oob", "panic: index out of bounds: the len is 3 but the index is 3"),
        ("t.empty_oob", "panic: index out of bounds: the len is 0 but the index is 0"),
        ("t.vec_oob", "panic: index out of bounds: the len is 0 but the index is 0"),
    ];
    for (i, (f, msg)) in cases.iter().enumerate() {
        let t = agg_refs_pkg(&format!("kir_agg_refs_panic_{}", i), &format!("    let _x = {}()\n", f));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(101), "tree-walker {}\n{}", f, text(&o.stderr));
        assert!(text(&o.stderr).contains(msg), "{}: {}", f, text(&o.stderr));
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(101), "kir {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stderr).trim(), *msg, "kir {}", f);
    }
}

#[test]
fn kir_agg_refs_text_shows_field_addresses_and_fat_values() {
    // 集成体への参照の型 (`mutref<agg#k>`)、フィールドのアドレス (`fieldaddr`)、`&str` の値 (`fat` の `make`) が見え、読み直せる。
    let t = agg_refs_pkg("kir_agg_refs_text", "    let _x = t.p_run(1i32, 2i32)\n");
    let o = run(&["kir", "t.line_move"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let s = text(&o.stdout);
    for want in ["mutref<agg#", "fieldaddr %", "load %", "store %"] {
        assert!(s.contains(want), "missing `{}` in:\n{}", want, s);
    }
    let o = run(&["kir", "t.lit_byte"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let s = text(&o.stdout);
    assert!(s.contains("fat size=16 align=8 {ptr:ptr@0 len:usize@8}") && s.contains("static.bytes 68c3a96c6c6f"), "{}", s);
}

#[test]
fn kir_agg_refs_vec_fields_are_read_through_references() {
    // Vec / String を持つ構造体 (json.Doc の形) への参照は下ろせる: ヘッダの `len` を `fieldaddr` と `load` で読む。
    // 値渡しと組み立て (所有の移動) は設計_KIR.md 13 章で下ろせる。`t.doc_run` も下ろせる。
    let t = agg_refs_pkg("kir_agg_refs_vec_field", "    core.check.eq(t.doc_run(3u32), 3u32)\n");
    assert_eq!(run(&["run"], &t.0).status.code(), Some(0));
    let e = run(&["kir", "t.doc_empty"], &t.0);
    assert_eq!(e.status.code(), Some(0), "{}", text(&e.stderr));
    let o = run(&["kir", "t.doc_by_ref"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let s = text(&o.stdout);
    assert!(s.contains("struct vec size=24 align=8 {ptr:ptr@0 len:usize@8 cap:usize@16}") && s.contains("call t.doc_bump(%0)"), "{}", s);
    // 組み立て (所有の移動) と `core.mem.drop` も下ろせる。値は木走査と同じで、漏れも二重解放もない。
    let o = kir_run(&["kir", "t.doc_run", "--run", "3"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "3");
}

#[test]
fn kir_agg_refs_reports_unsupported_references_with_a_reason() {
    // 指す先が標準の型の参照は、理由を付けて拒否する (要素が集成体のスライス `&[t.P]` は下ろせる)。
    let cases: &[(&str, &str, &str)] = &[
        ("fn t.f(e: &std.io.Stdout) -> u8 uses [] panics never\n    let _t = e\n    0u8\nend t.f\n", "t.f", "引数の型"),
    ];
    for (i, (src, f, why)) in cases.iter().enumerate() {
        let src = format!("{}\nfn t.main() -> Result<(), std.io.Error> uses [] panics may\n    Ok(())\nend t.main\n", src);
        let t = Tmp::new(&format!("kir_agg_refs_reason_{}", i), &src);
        let o = run(&["kir", f], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}", f);
        assert!(text(&o.stderr).contains(why), "{}: {}", f, text(&o.stderr));
    }
}

#[test]
fn kir_llvm_output_is_deterministic_and_reads_as_ir() {
    // `--llvm` は同じ KIR から同じバイト列の `.ll` を出す (clang が無くても確かめられる)。
    let t = kir_pkg("kir_llvm_text", "    core.check.eq(t.gcd(4u64, 2u64), 2u64)\n");
    for f in ["t.gcd", "t.fact", "t.negate", "t.andor", "t.nested", "t.down"] {
        let a = run(&["kir", f, "--llvm"], &t.0);
        let b = run(&["kir", f, "--llvm"], &t.0);
        assert_eq!(a.status.code(), Some(0), "{}: {}", f, text(&a.stderr));
        assert_eq!(a.stdout, b.stdout, "{}: .ll が 2 回で違う", f);
        let ll = text(&a.stdout);
        assert!(ll.starts_with("; ksn kir llvm 1\n") && ll.contains(&format!("define internal")) && ll.contains("define i32 @main"), "{}", ll);
        assert!(!ll.contains("@sin") && !ll.contains("@cos"), "libm の超越関数を呼ばない");
    }
}

#[test]
fn kir_native_without_clang_is_an_error_with_a_reason() {
    let t = kir_pkg("kir_native_noclang", "    core.check.eq(t.gcd(4u64, 2u64), 2u64)\n");
    let o = Command::new(exe())
        .args(["kir", "t.gcd", "--native", "4", "2"])
        .current_dir(&t.0)
        .env("KSN_CLANG", "/nonexistent/clang")
        .output()
        .expect("spawn ksn");
    assert_eq!(o.status.code(), Some(2), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("clang が必要です"), "{}", text(&o.stderr));
}

/// ループの中の配列リテラル (フレームの確保) を周回ごとに積まない。以前の LLVM 版は `alloca` を評価のたびに
/// 出していたので、100 万周でスタックが溢れて落ちた (参照実行器は正しかった)。確保は関数の先頭で 1 回、
/// 0 埋めは評価のたびに行う。
#[test]
fn kir_array_literal_in_a_long_loop_does_not_grow_the_native_stack() {
    let src = "fn t.sum(n: u32) -> u64 uses [] panics never
    let mut i = 0u32
    let mut acc = 0u64
    while i < n {
        for x in [1u64, 2u64, 3u64, 4u64] {
            acc = acc.wrapping_add(x)
        }
        i = i.wrapping_add(1u32)
    }
    acc
end t.sum

fn t.main() -> () uses [] panics never
end t.main
";
    let t = Tmp::new("kir_array_loop_stack", src);
    let o = kir_run(&["kir", "t.sum", "--run", "1000000"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "10000000");
}

#[test]
fn kir_write_prints_through_run_and_native() {
    // `print_line` は `write 1, s` と `write 1, \"\\n\"` に下ろされる。`--run` (参照実行器) と `--native` (write(2)) の表示が一致する。
    let t = Tmp::new("kir_io_write", IO_SAY);
    let o = run(&["kir", "t.say"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let ir = text(&o.stdout);
    assert!(ir.contains("= write %") && ir.contains("agg#1 struct std.io.Error") || ir.contains("struct std.io.Error size=8"), "{}", ir);
    let r = kir_run(&["kir", "t.say", "--run"], &t.0);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), "hi there\ntrue\n");
    let r = kir_run(&["kir", "t.twice", "--run"], &t.0);
    assert_eq!(text(&r.stdout), "one\ntwo\n2\n");
    // 木走査と同じ行が出る
    let tw = run(&["run"], &t.0);
    assert_eq!(text(&tw.stdout), "main\n");
    assert_eq!(tw.status.code(), Some(0));
}

#[test]
fn kir_own_values_match_tree_walker() {
    // Vec / String の所有の移動 (返す・引数で渡す・構造体のフィールド・`Option` の `match`・片方の枝だけで移す・ループ・`?`) が、
    // 木走査と同じ値になる。参照実行器は漏れと二重解放を内部エラーにし、LLVM (clang があれば) も同じ表示になる。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.ret_vec", &["5"], "5usize", "5"),
        ("t.ret_vec", &["0"], "0usize", "0"),
        ("t.arg_ret", &["4"], "21u32", "21"),
        ("t.buf_run", &["6"], "6usize", "6"),
        ("t.opt_len", &["3", "true"], "3usize", "3"),
        ("t.opt_len", &["3", "false"], "99usize", "99"),
        ("t.opt_local", &["4", "true"], "4usize", "4"),
        ("t.opt_local", &["4", "false"], "99usize", "99"),
        ("t.opt_peek", &["4", "true"], "11usize", "11"),
        ("t.opt_peek", &["4", "false"], "2usize", "2"),
        ("t.opt_str", &["true"], "0usize", "0"),
        ("t.opt_str", &["false"], "99usize", "99"),
        ("t.branch", &["5", "true"], "6usize", "6"),
        ("t.branch", &["5", "false"], "1usize", "1"),
        ("t.branch_call", &["5", "true"], "5usize", "5"),
        ("t.branch_call", &["5", "false"], "0usize", "0"),
        ("t.branch_buf", &["5", "true"], "5usize", "5"),
        ("t.branch_buf", &["5", "false"], "7usize", "7"),
        ("t.churn", &["10000"], "30000usize", "30000"),
        ("t.churn_exit", &["10"], "9usize", "9"),
        ("t.try_run", &["4", "false"], "6usize", "6"),
        ("t.try_run", &["4", "true"], "1000usize", "1000"),
        ("t.map_err_run", &["9"], "2usize", "2"),
        ("t.slot_len", &["0"], "0usize", "0"),
        ("t.slot_len", &["5"], "5usize", "5"),
        ("t.slot_ignore", &["0"], "1usize", "1"),
        ("t.slot_ignore", &["5"], "2usize", "2"),
        ("t.reassign", &["8"], "2usize", "2"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.iter().map(|a| match *a {
            "true" | "false" => a.to_string(),
            n => format!("{}u32", n),
        }).collect::<Vec<_>>().join(", "));
        let t = kir_own_pkg(&format!("kir_own_values_{}", i), &format!("    core.check.eq({}, {})\n", call, lit));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_closure_captures_and_iter_pipes_match_tree_walker() {
    // 取り込み 4 種類 (`&x` `&mut x` `move x` と複数)、`fold` の初期値と累積、`map` → `filter` → `fold`、空の範囲。
    // (関数, 引数の型, 引数, 期待する値)
    let cases: &[(&str, &str, &str, &str)] = &[
        ("t.cap_ref", "u64", "4", "46"),
        ("t.cap_ref", "u64", "0", "0"),
        ("t.cap_move", "u64", "2", "39"),
        ("t.cap_two", "u64", "2", "51"),
        ("t.fold_basic", "u32", "5", "110"),
        ("t.fold_basic", "u32", "0", "100"),
        ("t.fold_let", "u32", "9", "53"),
        ("t.fold_let", "u32", "3", "1"),
        ("t.chain", "u64", "10", "61"),
        ("t.chain", "u64", "0", "1"),
        ("t.count_even", "u32", "9", "5"),
        ("t.count_even", "u32", "0", "0"),
    ];
    for (i, (f, aty, arg, want)) in cases.iter().enumerate() {
        let rty = if *f == "t.count_even" { "usize" } else { *aty };
        let call = format!("{}({}{})", f, arg, aty);
        let t = Tmp::new(
            &format!("kir_closure_{}", i),
            &format!("{}{}", KIR_CLOSURE_FNS, main_wrap("", &format!("    core.check.eq({}, {}{})\n", call, want, rty))),
        );
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let k = kir_run(&["kir", f, "--run", arg], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_own_more_match_tree_walker() {
    // 所有の周りの残り (設計_KIR.md 13.7): `push_str` / `push(char)` / `pop` / `clear`、構造体のフィールドの Vec / String の
    // メソッド (`&mut self` と値の構造体)、`get_mut`、所有物を持つ値の分解、フィールドからの移動。木走査と同じ値になり、
    // 参照実行器が漏れ・二重解放を内部エラーにしないこと (`--run` と `--native` の両方)。
    let cases: &[(&str, &[&str], &str, &str)] = &[
        ("t.str_build", &["0"], "0usize", "0"),
        ("t.str_build", &["1000"], "2000usize", "2000"),
        ("t.str_chars", &[], "2031u32", "2031"),
        ("t.str_chars_len", &[], "13usize", "13"),
        ("t.pop_sum", &["0"], "0u32", "0"),
        ("t.pop_sum", &["6"], "15u32", "15"),
        ("t.clear_run", &["4"], "3usize", "3"),
        ("t.ring_run", &["3"], "528u32", "528"),
        ("t.ring_run", &["0"], "0u32", "0"),
        ("t.pair_run", &["2"], "13usize", "13"),
        ("t.pair_param_run", &["2"], "6usize", "6"),
        ("t.destruct", &["4"], "6usize", "6"),
        ("t.destruct_partial", &["4"], "4usize", "4"),
        ("t.destruct_none", &["4"], "7usize", "7"),
        ("t.destruct_struct", &["4"], "7usize", "7"),
        ("t.destruct_struct_both", &["4"], "7usize", "7"),
        ("t.match_partial", &["4", "true"], "4usize", "4"),
        ("t.match_partial", &["4", "false"], "99usize", "99"),
        ("t.match_struct_partial", &["5"], "5usize", "5"),
        ("t.field_move", &["4"], "7usize", "7"),
        ("t.field_move_reassign", &["4"], "6usize", "6"),
        ("t.field_ret_run", &["5"], "5usize", "5"),
        ("t.field_move_str", &["3"], "5usize", "5"),
        ("t.mode_run", &[], "2u32", "2"),
        ("t.vs_run", &["0"], "0usize", "0"),
        ("t.vs_run", &["5"], "20usize", "20"),
        ("t.vs_clear", &["4"], "1usize", "1"),
        ("t.vs_keep", &["6"], "6usize", "6"),
        ("t.names_run", &["3"], "3usize", "3"),
        ("t.names_push", &["4"], "4usize", "4"),
        ("t.vv_run", &["4"], "6usize", "6"),
    ];
    for (i, (f, args, lit, want)) in cases.iter().enumerate() {
        let call = format!("{}({})", f, args.iter().map(|a| match *a {
            "true" | "false" => a.to_string(),
            n if *f == "t.str_build" || *f == "t.ring_run" => format!("{}usize", n),
            n => format!("{}u32", n),
        }).collect::<Vec<_>>().join(", "));
        let body = format!("    core.check.eq({}, {})\n", call, lit);
        let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", body);
        let t = Tmp::new(&format!("kir_own_more_{}", i), &format!("{}{}", KIR_OWN2_FNS, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    }
}

#[test]
fn kir_own_reports_unsupported_moves_with_a_reason() {
    // 一時の値のフィールドの移動、所有物を持つ `unwrap_or` は、
    // 誤った KIR を出さず理由を付けて拒否する。
    // (参照を通したフィールドの移動は `ksn check` が E3002 で拒否するので、ここには来ない。)
    let cases: &[(&str, &str, &str)] = &[
        (
            "type t.B\n    items: alloc.vec.Vec<u8, std.alloc.System>\nend t.B\n\nfn t.mk() -> t.B uses [alloc] panics never\n    t.B { items: alloc.vec.Vec.new(std.alloc.SYSTEM) }\nend t.mk\n\nfn t.f() -> usize uses [alloc] panics never\n    let w = t.mk().items\n    w.len()\nend t.f\n",
            "t.f",
            "一時の値のフィールド",
        ),
        (
            "fn t.f(o: Option<alloc.vec.Vec<u8, std.alloc.System>>) -> usize uses [] panics never\n    o.unwrap_or(alloc.vec.Vec.new(std.alloc.SYSTEM)).len()\nend t.f\n",
            "t.f",
            "unwrap_or",
        ),
    ];
    for (i, (src, f, why)) in cases.iter().enumerate() {
        let src = format!("{}\nfn t.main() -> Result<(), std.io.Error> uses [] panics may\n    Ok(())\nend t.main\n", src);
        let t = Tmp::new(&format!("kir_own_reason_{}", i), &src);
        let o = run(&["kir", f], &t.0);
        assert_eq!(o.status.code(), Some(1), "{}: {}", f, text(&o.stdout));
        assert!(text(&o.stderr).contains(why), "{}: {}", f, text(&o.stderr));
    }
}
