//! 2026-11-25 m4 下ろしの穴 (2)。`ksn check` を通るのに KIR に下ろせなかった形が、木走査 (tree)・VM・
//! release のネイティブで同じ表示になることを確かめる。VM は退避 (`vm engine skipped`) を許さない。

mod common;

use common::{build_native_strict, run, text, Tmp};

fn check_form(name: &str, src: &str, want: &str) {
    check_form_on(name, src, want, true)
}

/// `vm` が偽: VM は `Vec<Option<_>>` の格納 (`mem.store`) をまだ書けないので、tree と native だけ比べる。
fn check_form_on(name: &str, src: &str, want: &str, vm: bool) {
    let src = &src
        .replace("fn t.main() -> () uses [io.console] panics may", "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may")
        .replace("    std.io.print_line(\"ok\")\n", "    match std.io.print_line(\"ok\") {\n        Ok(_x) => Ok(())\n        Err(_e) => core.panic.panic(\"io\")\n    }\n");
    let t = Tmp::new(name, src);
    let tree = run(&["run", "--engine=tree"], &t.0);
    assert_eq!(tree.status.code(), Some(0), "tree: {}{}", text(&tree.stderr), text(&tree.stdout));
    assert_eq!(text(&tree.stdout), want, "tree の表示が想定と違う");
    if vm {
        let vm = run(&["run", "--engine=vm"], &t.0);
        assert!(!text(&vm.stderr).contains("vm engine skipped"), "VM に下ろせない: {}", text(&vm.stderr));
        assert_eq!(vm.status.code(), Some(0), "vm: {}", text(&vm.stderr));
        assert_eq!(text(&vm.stdout), want, "vm の表示が tree と違う");
    }
    let (bt, exe, out) = build_native_strict(&format!("{}_rel", name), src, true);
    let p = exe.unwrap_or_else(|| panic!("ネイティブの実行ファイルが無い: {}", out));
    let o = std::process::Command::new(&p).current_dir(&bt.0).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "native: {}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), want, "native の表示が tree と違う");
}

/// 1. `Option.ok_or`。
#[test]
fn option_ok_or() {
    check_form(
        "m4l_ok_or",
        r#"fn t.pick(v: Option<u32>) -> Result<u32, u8> uses [] panics never
    v.ok_or(7u8)
end t.pick

fn t.main() -> () uses [io.console] panics may
    let a = t.pick(Some(5u32))
    let b = t.pick(None)
    core.check.eq(a, Ok(5u32))
    core.check.eq(b, Err(7u8))
    std.io.print_line("ok")
end t.main
"#,
        "ok\n",
    );
}

/// 2. `bs[i].v = bs[i].v + d` (Vec/配列の添字越しの構造体欄への代入)。
#[test]
fn struct_field_assign_through_index() {
    check_form(
        "m4l_idx_field",
        r#"type t.Body derive [Debug, Clone, Copy] pub
    x: f64
    v: f64
end t.Body

fn t.step(bs: &mut [t.Body]) -> () uses [] panics may
    let n = bs.len()
    for i in core.iter.range(0usize, n) {
        for j in core.iter.range(i + 1usize, n) {
            let d = bs[j].x - bs[i].x
            bs[i].v = bs[i].v + d
            bs[j].v = bs[j].v - d
        }
    }
end t.step

fn t.main() -> () uses [io.console] panics may
    let mut bs = [t.Body {
        x: 0.0f64
        v: 0.0f64
    }, t.Body {
        x: 1.0f64
        v: 0.0f64
    }]
    t.step(&mut bs)
    core.check.eq(bs[0usize].v, 1.0f64)
    core.check.eq(bs[1usize].v, -1.0f64)
    std.io.print_line("ok")
end t.main
"#,
        "ok\n",
    );
}

/// 6. `for x in v.iter() { match x { Some(n) => .. } }` (`x: &Option<u32>`)。
#[test]
fn match_on_ref_option_items() {
    check_form_on(
        "m4l_ref_option",
        r#"fn t.sum(v: &std.vec.Vec<Option<u32>>) -> u32 uses [] panics never
    let mut total = 0u32
    for x in v.iter() {
        match x {
            Some(n) => {
                total = total.wrapping_add(*n)
            }
            None => {
                total = total.wrapping_add(100u32)
            }
        }
    }
    total
end t.sum

fn t.main() -> () uses [io.console] panics may
    let mut v = std.vec.Vec.new()
    core.mem.drop(v.push(Some(5u32)))
    core.mem.drop(v.push(None))
    core.check.eq(t.sum(&v), 105u32)
    std.io.print_line("ok")
end t.main
"#,
        "ok\n",
        false,
    );
}

/// 4. 入れ子の配列 `&[[u8; 3]; 3]` の `get`。
#[test]
fn get_on_nested_array() {
    check_form(
        "m4l_nested_get",
        r#"fn t.find(grid: &[[u8; 3]; 3], want: u8) -> Option<(usize, usize)> uses [] panics never
    for i in core.iter.range(0usize, 3usize) {
        if let Some(row) = grid.get(i) {
            for j in core.iter.range(0usize, 3usize) {
                if let Some(c) = row.get(j) {
                    if *c == want {
                        return Some((i, j))
                    }
                }
            }
        }
    }
    None
end t.find

fn t.main() -> () uses [io.console] panics may
    let g = [[1u8, 2u8, 3u8], [4u8, 5u8, 6u8], [7u8, 8u8, 9u8]]
    core.check.eq(t.find(&g, 6u8), Some((1usize, 2usize)))
    core.check.eq(t.find(&g, 0u8), None)
    std.io.print_line("ok")
end t.main
"#,
        "ok\n",
    );
}

/// 7a. `for (i, x) in core.iter.enumerate(v.iter())` (配列・Vec)。
#[test]
fn for_enumerate_destructuring() {
    check_form(
        "m4l_enumerate",
        r#"fn t.weighted(v: &std.vec.Vec<u32>) -> usize uses [] panics never
    let mut total = 0usize
    for (i, x) in core.iter.enumerate(v.iter()) {
        total = total.wrapping_add(i.wrapping_mul(usize.from_u32(*x)))
    }
    total
end t.weighted

fn t.main() -> () uses [io.console] panics may
    let a = [10u32, 20u32, 30u32]
    let mut total = 0usize
    for (i, x) in core.iter.enumerate(a.iter()) {
        total = total.wrapping_add(i.wrapping_mul(usize.from_u32(*x)))
    }
    core.check.eq(total, 80usize)
    let mut v = std.vec.Vec.new()
    core.mem.drop(v.push(1u32))
    core.mem.drop(v.push(5u32))
    core.check.eq(t.weighted(&v), 5usize)
    std.io.print_line("ok")
end t.main
"#,
        "ok\n",
    );
}
