//! インタプリタをネイティブの挙動に合わせる試験 (設計_インタプリタ一致.md)。
//! 段 1: `core.mem.drop` した所有する値を、借用越しに読んだら panic (終了コード 101) になる。
//! 検査器の穴 (借用検査は通ってしまう形) は、隠しの `KSN_SKIP_BORROW_DIAGS=1` で E30xx を外して走らせる。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;

fn run_skipping_borrowck(name: &str, src: &str) -> (Option<i32>, String, String) {
    let t = Tmp::new(name, src);
    let o = run_with_env(&["run"], &t.0, &[("KSN_SKIP_BORROW_DIAGS", "1")]);
    (o.status.code(), text(&o.stdout), text(&o.stderr))
}

fn assert_dead_read(name: &str, src: &str) {
    let (code, out, err) = run_skipping_borrowck(name, src);
    assert_eq!(code, Some(101), "stdout: {} stderr: {}", out, err);
    assert!(err.contains("use after free"), "{}", err);
}

const B1: &str = r#"type t.Named
    name: alloc.string.String<std.alloc.System>
end t.Named

fn t.Named.pick(self: &t.Named, other: &str) -> &str uses [] panics never
    if self.name.is_empty() {
        return other
    }
    self.name.as_str()
end t.Named.pick

fn t.hello() -> alloc.string.String<std.alloc.System> uses [alloc] panics never
    match alloc.string.String.from_str(std.alloc.SYSTEM, "hello world, this is a heap string") {
        Ok(s) => s
        Err(_e) => alloc.string.String.new(std.alloc.SYSTEM)
    }
end t.hello

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let p = t.Named { name: alloc.string.String.new(std.alloc.SYSTEM) }
    let other = t.hello()
    let r = p.pick(other.as_str())
    core.mem.drop(other)
    std.io.print_line(r)
end t.main
"#;

#[test]
fn dead_read_panics_b1() {
    // B1: self を持つ関数が別の引数の借用を返す
    assert_dead_read("par_b1", B1);
}

const B4: &str = r#"fn t.hello() -> alloc.string.String<std.alloc.System> uses [alloc] panics never
    match alloc.string.String.from_str(std.alloc.SYSTEM, "hello world, this is a heap string") {
        Ok(s) => s
        Err(_e) => alloc.string.String.new(std.alloc.SYSTEM)
    }
end t.hello

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics never
    let s = t.hello()
    match std.sync.channel(std.alloc.SYSTEM, 2usize) {
        Ok((tx, rx)) => {
            core.mem.drop(tx.send(&s))
            core.mem.drop(s)
            match rx.recv() {
                Ok(x) => std.io.print_line(x.as_str())?
                Err(_e) => std.io.print_line("closed")?
            }
        }
        Err(_e) => std.io.print_line("alloc")?
    }
    Ok(())
end t.main
"#;

#[test]
fn dead_read_panics_b4() {
    // B4: チャネルで局所の借用を送る
    assert_dead_read("par_b4", B4);
}

const H1: &str = r#"type t.Res linear
    data: alloc.vec.Vec<u64, std.alloc.System>
end t.Res

fn t.Res.shut(self: t.Res) -> () uses [] panics never
    let t.Res { data: data } = self
    core.mem.drop(data)
end t.Res.shut

fn t.mk() -> t.Res uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(41u64))
    t.Res { data: v }
end t.mk

fn t.wrap(x: &t.Res) -> &t.Res uses [] panics never
    x
end t.wrap

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let p = with r = t.mk() close r.shut() {
        t.wrap(&r)
    }
    match p.data.get(0usize) {
        Some(x) => core.check.eq(*x, 41u64)
        None => std.io.print_line("none")?
    }
    std.io.print_line("done")
end t.main
"#;

#[test]
fn dead_read_panics_h1() {
    // round3 穴 1: with の本体の値が束縛の借用を外へ出す
    assert_dead_read("par_h1", H1);
}

const H3: &str = r#"type t.P['a, 'b]
    x: &'a alloc.string.String<std.alloc.System>
    y: Option<&'b alloc.string.String<std.alloc.System>>
end t.P

fn t.f4(p: &mut t.P) -> () uses [] panics never
    match p.y {
        Some(v) => {
            p.x = v
        }
        None => ()
    }
end t.f4

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let a = alloc.string.String.new(std.alloc.SYSTEM)
    let mut p = t.P { x: &a, y: None }
    let s = match alloc.string.String.from_str(std.alloc.SYSTEM, "hello") {
        Ok(s) => s
        Err(_e) => return Ok(())
    }
    p.y = Some(&s)
    t.f4(&mut p)
    p.y = None
    core.mem.drop(s)
    let xr = p.x
    if xr.as_str() == "hello" {
        std.io.print_line(xr.as_str())?
    }
    std.io.print_line("done")
end t.main
"#;

#[test]
fn dead_read_panics_h3() {
    // round3 穴 3: 省略したライフタイムの欄へ別の領域の借用を入れる
    assert_dead_read("par_h3", H3);
}

#[test]
fn dead_marks_do_not_touch_live_values() {
    // 正しいプログラム: drop したあとに同じ値を読まない。複製・別の String・Vec の drop は、他の値を巻き込まない。
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let a = match alloc.string.String.from_str(std.alloc.SYSTEM, "abc") {
        Ok(s) => s
        Err(_e) => return Ok(())
    }
    let b = match alloc.string.String.from_str(std.alloc.SYSTEM, "abc") {
        Ok(s) => s
        Err(_e) => return Ok(())
    }
    let r = a.as_str()
    std.io.print_line(r)?
    core.mem.drop(a)
    std.io.print_line(b.as_str())?
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(1u32))
    core.mem.drop(v)
    std.io.print_line("ok")
end t.main
"#;
    let t = Tmp::new("par_live", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout), "abc\nabc\nok\n");
}

const REFBIND_IFLET_MUT_REF_BIND_MISMATCH: &str = r#"fn t.bump(opt: &mut Option<i32>, delta: i32) -> bool uses [] panics never
    if let Some(v) = opt {
        *v = v.wrapping_add(delta)
        true
    } else {
        false
    }
end t.bump

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut opt = Some(5i32)
    core.check.eq(t.bump(&mut opt, 3i32), true)
    std.io.print_line("iflet_mut ok")
end t.main
"#;

#[test]
fn ref_bind_writes_through_iflet_mut_ref_bind_mismatch() {
    // 段 2: 参照越しの分解の束縛は欄への参照 (ネイティブと同じ。`*v = ..` が元の値へ書ける)
    let t = Tmp::new("par_iflet_mut_ref_bind_mismatch", REFBIND_IFLET_MUT_REF_BIND_MISMATCH);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "iflet_mut ok");
}

const REFBIND_FP_MATCH_ASSIGN_PROBE: &str = r#"fn t.bump_match(opt: &mut Option<i32>, delta: i32) -> bool uses [] panics never
    match opt {
        Some(v) => {
            *v = v.wrapping_add(delta)
            true
        }
        None => {
            false
        }
    }
end t.bump_match

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut opt = Some(5i32)
    core.check.eq(t.bump_match(&mut opt, 3i32), true)
    std.io.print_line("match_assign ok")
end t.main
"#;

#[test]
fn ref_bind_writes_through_fp_match_assign_probe() {
    // 段 2: 参照越しの分解の束縛は欄への参照 (ネイティブと同じ。`*v = ..` が元の値へ書ける)
    let t = Tmp::new("par_fp_match_assign_probe", REFBIND_FP_MATCH_ASSIGN_PROBE);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "match_assign ok");
}

const REFBIND_FP_ENUM_VARIANT_LIKE: &str = r#"enum t.Kind pub
    A(i32)
    B(i32)
end t.Kind

fn t.bump_kind(k: &mut t.Kind, d: i32) -> () uses [] panics never
    match k {
        t.Kind.A(v) => {
            *v = v.wrapping_add(d)
        }
        t.Kind.B(v) => {
            *v = v.wrapping_add(d)
        }
    }
end t.bump_kind

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut k = t.Kind.A(5i32)
    t.bump_kind(&mut k, 3i32)
    match k {
        t.Kind.A(v) => {
            core.check.eq(v, 8i32)
        }
        t.Kind.B(_v) => {
            core.panic.panic("wrong variant")
        }
    }
    std.io.print_line("enum_variant ok")
end t.main
"#;

#[test]
fn ref_bind_writes_through_fp_enum_variant_like() {
    // 段 2: 参照越しの分解の束縛は欄への参照 (ネイティブと同じ。`*v = ..` が元の値へ書ける)
    let t = Tmp::new("par_fp_enum_variant_like", REFBIND_FP_ENUM_VARIANT_LIKE);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "enum_variant ok");
}

const REFBIND_REFBIND: &str = r#"type t.S
    a: i32
    b: i32
end t.S

fn t.tup(p: &mut (i32, i32)) -> () uses [] panics never
    let (x, y) = p
    *x = x.wrapping_add(1i32)
    *y = y.wrapping_add(10i32)
end t.tup

fn t.st(s: &mut t.S) -> () uses [] panics never
    let t.S { a: a, b: b } = s
    *a = a.wrapping_add(5i32)
    *b = b.wrapping_add(7i32)
end t.st

fn t.nest(o: &mut Option<(i32, i32)>) -> () uses [] panics never
    match o {
        Some((x, _y)) => {
            *x = x.wrapping_add(100i32)
        }
        None => ()
    }
end t.nest

fn t.rd(o: &Option<i32>) -> i32 uses [] panics never
    match o {
        Some(v) => *v
        None => 0i32
    }
end t.rd

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut p = (1i32, 2i32)
    t.tup(&mut p)
    let (x, y) = p
    core.check.eq(x, 2i32)
    core.check.eq(y, 12i32)
    let mut s = t.S { a: 1i32, b: 2i32 }
    t.st(&mut s)
    core.check.eq(s.a, 6i32)
    core.check.eq(s.b, 9i32)
    let mut o = Some((1i32, 2i32))
    t.nest(&mut o)
    match o {
        Some((q, _w)) => core.check.eq(q, 101i32)
        None => core.panic.panic("none")
    }
    core.check.eq(t.rd(&Some(4i32)), 4i32)
    std.io.print_line("refbind ok")
end t.main
"#;

#[test]
fn ref_bind_writes_through_refbind() {
    // 段 2: 参照越しの分解の束縛は欄への参照 (ネイティブと同じ。`*v = ..` が元の値へ書ける)
    let t = Tmp::new("par_refbind", REFBIND_REFBIND);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "refbind ok");
}

// 段 2: drop を伴わない死 (スコープの終わり・`for` の束縛・`match` の腕の束縛・`Vec` の再確保)。unsound/ の再現を検査なしで走らせる。
macro_rules! dead_case {
    ($test:ident, $file:literal) => {
        #[test]
        fn $test() {
            assert_dead_read(concat!("par_", $file), include_str!(concat!("data/unsound/", $file, ".ksnl")));
        }
    };
}
dead_case!(scope_end_block_local, "flow_store_through_mut_alias");
dead_case!(scope_end_block_value, "header_lifetime_return_other");
dead_case!(for_binding_dies_each_iteration, "flow_for_binding_outlives");
dead_case!(match_arm_binding_dies_with_the_arm, "flow_match_arm_binding_outlives");
dead_case!(match_temp_scrutinee_binding_dies, "match_arm_owned_as_str_escape");
dead_case!(vec_growth_invalidates_element_borrows, "fqn_same_name_shared_push");
dead_case!(vec_growth_invalidates_field_vec_borrows, "flow_ref_field_borrow_inherits_loan");

#[test]
fn scope_end_marks_do_not_touch_live_values() {
    // 正しいプログラム: ブロックの局所の String を借用して使い切る、値を返す・move する、`for` で要素を使う、
    // `match` の腕で値を使う、`push` を繰り返して読む。どれも出力は変わらない。
    let src = r#"fn t.mk(s: &str) -> alloc.string.String<std.alloc.System> uses [alloc] panics never
    match alloc.string.String.from_str(std.alloc.SYSTEM, s) {
        Ok(v) => v
        Err(_e) => alloc.string.String.new(std.alloc.SYSTEM)
    }
end t.mk

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let n = {
        let s = t.mk("hello")
        s.len()
    }
    core.check.eq(n, 5usize)
    let kept = {
        let s = t.mk("kept")
        s
    }
    std.io.print_line(kept.as_str())?
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < 100u32 {
        core.mem.drop(v.push(t.mk("item")))
        i = i + 1u32
    }
    let mut total = 0usize
    for s in v {
        total = total + s.len()
    }
    core.check.eq(total, 400usize)
    match Some(t.mk("arm")) {
        Some(s) => std.io.print_line(s.as_str())?
        None => ()
    }
    let w = match Some(t.mk("moved")) {
        Some(s) => s
        None => t.mk("none")
    }
    std.io.print_line(w.as_str())?
    std.io.print_line("ok")
end t.main
"#;
    let t = Tmp::new("par_scope_live", src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout), "kept\narm\nmoved\nok\n");
}
