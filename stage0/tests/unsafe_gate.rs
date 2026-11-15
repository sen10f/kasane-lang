// 3.5 / 3.8: `[unsafe] modules` の `unsafe` の許可を、テスト用パッケージ (`kasane.toml` の `[unsafe]`) で確かめる。
// 許可のないモジュールでの生ポインタは E5001。許可モジュールの `unsafe` ブロックの中の `core.ptr` は通る。
//
// 3.8: 許可モジュールの中でも `core.ptr` の関数は `unsafe` ブロックの中でしか呼べない (E5003)。
// テストは `core_ptr_call_outside_unsafe_is_e5003_in_allowed_module` が確かめる。
mod common;

use common::*;
use std::fs;

/// `[unsafe] modules` を持つ (`modules` が `Some` のとき) テスト用パッケージ。
fn pkg(name: &str, src: &str, modules: Option<&str>) -> Tmp {
    let t = Tmp::new(name, src);
    if let Some(m) = modules {
        fs::write(t.0.join("kasane.toml"), format!("{}\n[unsafe]\nmodules = [\"{}\"]\n", TOML, m)).unwrap();
    }
    t
}

fn check(t: &Tmp) -> (Option<i32>, Vec<String>, String) {
    let o = run(&["check", "--json"], &t.0);
    (o.status.code(), codes(&text(&o.stdout)), text(&o.stdout))
}

#[test]
fn core_ptr_call_inside_unsafe_is_accepted_in_allowed_module() {
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let v = 7i32
    unsafe \"read\" \"v is a live local\" {
        let p = core.ptr.from_ref(&v)
        core.mem.drop(core.ptr.read(p))
    }
    Ok(())
end t.main
";
    let t = pkg("ug_ptr_inside", src, Some("t"));
    let (rc, got, out) = check(&t);
    assert_eq!(rc, Some(0), "{}", out);
    assert!(got.is_empty(), "{}", out);
}

#[test]
fn core_ptr_call_outside_unsafe_is_rejected_in_non_allowed_module() {
    // 許可のないモジュールでは生ポインタの使用として E5001 が出る。
    let src = "fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let v = 7i32
    let p = core.ptr.from_ref(&v)
    let _x = core.ptr.read(p)
    Ok(())
end t.main
";
    let t = pkg("ug_ptr_nonallowed", src, None);
    let (rc, got, out) = check(&t);
    assert_eq!(rc, Some(1), "{}", out);
    assert!(got.iter().any(|c| c == "E5001"), "{}", out);
}

#[test]
fn core_ptr_call_outside_unsafe_is_e5003_in_allowed_module() {
    // 3.8: 許可モジュールの中でも、`core.ptr` の関数を `unsafe` ブロックの外で呼ぶと E5003 (`extern` と同じ扱い)。
    // 生成 (`from_ref`) と、関数の戻り値の位置の読み (`read`) の両方を見る。`unsafe` の中の呼びは通る。
    let src = "fn t.rd(p: *const i32) -> i32 uses [] panics never
    core.ptr.read(p)
end t.rd

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let v = 7i32
    let p = core.ptr.from_ref(&v)
    let a = t.rd(p)
    unsafe \"read\" \"v is a live local\" {
        core.mem.drop(core.ptr.read(p))
    }
    core.mem.drop(a)
    Ok(())
end t.main
";
    let t = pkg("ug_ptr_e5003_allowed", src, Some("t"));
    let (rc, got, out) = check(&t);
    assert_eq!(rc, Some(1), "{}", out);
    assert!(got.iter().any(|c| c == "E5003"), "{}", out);
    assert!(!got.iter().any(|c| c == "E5001"), "{}", out);
    // 違反は `core.ptr.read` (`t.rd` の本体) と `core.ptr.from_ref` (`t.main`) の 2 か所。
    assert_eq!(got.iter().filter(|c| *c == "E5003").count(), 2, "{}", out);
}

#[test]
fn core_ptr_value_outside_unsafe_is_e5003_in_allowed_module() {
    // `core.ptr` の関数を値として束縛・引数として渡すのも `unsafe` の外では E5003。
    let src = "fn t.rd(p: *const i32) -> i32 uses [] panics never
    let f = core.ptr.read
    let x: i32 = f(p)
    core.mem.drop(x)
end t.rd

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let v = 7i32
    let p = core.ptr.from_ref(&v)
    let a = t.rd(p)
    unsafe \"read\" \"v is a live local\" {
        core.mem.drop(core.ptr.read(p))
    }
    core.mem.drop(a)
    Ok(())
end t.main
";
    let t = pkg("ug_ptr_value_e5003", src, Some("t"));
    let (rc, got, out) = check(&t);
    assert_eq!(rc, Some(1), "{}", out);
    assert!(got.iter().any(|c| c == "E5003"), "{}", out);
    assert!(!got.iter().any(|c| c == "E5001"), "{}", out);
}

#[test]
fn extern_value_outside_unsafe_is_e5003_in_allowed_module() {
    // `extern` の関数を値として渡すのも `unsafe` の外では E5003。
    let src = "extern t.getpid() -> i32 link \"getpid\"

fn t.call0(f: fn() -> i32 uses [] panics may) -> i32 uses [] panics may
    f()
end t.call0

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let a = t.call0(t.getpid)
    core.mem.drop(a)
    Ok(())
end t.main
";
    let t = pkg("ug_ext_value_e5003", src, Some("t"));
    let (rc, got, out) = check(&t);
    assert_eq!(rc, Some(1), "{}", out);
    assert!(got.iter().any(|c| c == "E5003"), "{}", out);
    assert!(!got.iter().any(|c| c == "E5001"), "{}", out);
}

#[test]
fn core_ptr_value_inside_unsafe_is_accepted_in_allowed_module() {
    // `unsafe` の中で `core.ptr` や `extern` の関数を値として参照するのは通る。
    let src = "extern t.getpid() -> i32 link \"getpid\"

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    let v = 7i32
    unsafe \"read\" \"v is a live local\" {
        let f = core.ptr.read
        let p = core.ptr.from_ref(&v)
        let x: i32 = f(p)
        core.mem.drop(x)
        let g = t.getpid
        let y: i32 = g()
        core.mem.drop(y)
    }
    Ok(())
end t.main
";
    let t = pkg("ug_ptr_value_inside", src, Some("t"));
    let (rc, got, out) = check(&t);
    // 2026-11-15 の決定: `extern` と `core.ptr` の関数は値として扱わない。`unsafe` の中の値としての参照も
    // E5003 で拒否する (`let f = core.ptr.read` と `let g = t.getpid` の 2 か所。呼び出しの `f(p)` `g()` は違反ではない)。
    // テスト名は旧い期待 (受理) のときの名前のまま残す。入力は変えていない。
    assert_eq!(rc, Some(1), "{}", out);
    assert!(got.iter().any(|c| c == "E5003"), "{}", out);
    assert!(!got.iter().any(|c| c == "E5001"), "{}", out);
    assert_eq!(got.iter().filter(|c| *c == "E5003").count(), 2, "{}", out);
}

#[test]
fn extern_and_ptr_functions_as_values_are_e5003_even_inside_unsafe() {
    // 2026-11-15 (a25 の穴探しが見つけた 9 件): `unsafe` の中で `extern` の関数を値として束縛・代入・捕捉・返却し、
    // `unsafe` の外で普通の関数として呼ぶ。値としての参照は `unsafe` の中でも E5003 で拒否する。
    // 9 件はすべて `[unsafe] modules = ["t"]` のパッケージで、E5001 ではなく E5003 だけが出ること。
    let cases: &[(&str, &str)] = &[
        ("ug_a25_bind_then_call", r#"extern t.getpid() -> i32 link "getpid"

fn t.dummy() -> i32 uses [] panics may
    0i32
end t.dummy

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut f = t.dummy
    unsafe "getpid" "bind" {
        f = t.getpid
    }
    core.mem.drop(f())
    Ok(())
end t.main
"#),
        ("ug_a25_closure_bind", r#"extern t.getpid() -> i32 link "getpid"

fn t.dummy() -> i32 uses [] panics may
    0i32
end t.dummy

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut f = t.dummy
    unsafe "getpid" "getpid を束縛" {
        f = t.getpid
    }
    let c = fn() -> i32 capture [&f] {
        f()
    }
    let x = c()
    core.mem.drop(x)
    Ok(())
end t.main
"#),
        ("ug_a25_double_indirect", r#"extern t.getpid() -> i32 link "getpid"

fn t.dummy() -> i32 uses [] panics may
    0i32
end t.dummy

fn t.first(f: fn() -> i32 uses [] panics may) -> fn() -> i32 uses [] panics may
    f
end t.first

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut f = t.dummy
    unsafe "getpid" "getpid を束縛" {
        f = t.getpid
    }
    let g = t.first(f)
    let x = t.second(g)
    core.mem.drop(x)
    Ok(())
end t.main

fn t.second(f: fn() -> i32 uses [] panics may) -> i32 uses [] panics may
    f()
end t.second
"#),
        ("ug_a25_env_bind", r#"extern t.dom_create(tag_ptr: *const u8, tag_len: usize) -> u32 link "env:ksn_dom_create"

fn t.dummy() -> u32 uses [] panics may
    0u32
end t.dummy

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut f = t.dummy
    unsafe "env" "env 関数を束縛" {
        f = t.dom_create
    }
    let x = f()
    core.mem.drop(x)
    Ok(())
end t.main
"#),
        ("ug_a25_if_let", r#"extern t.getpid() -> i32 link "getpid"

fn t.dummy() -> i32 uses [] panics may
    0i32
end t.dummy

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut f = t.dummy
    unsafe "getpid" "getpid を束縛" {
        f = t.getpid
    }
    if let g = f {
        let x = g()
        core.mem.drop(x)
    }
    Ok(())
end t.main
"#),
        ("ug_a25_method_call", r#"type t.Runner
    f: fn() -> i32 uses [] panics may
end t.Runner

extern t.getpid() -> i32 link "getpid"

fn t.Runner.run(self: &t.Runner) -> i32 uses [] panics may
    (self.f)()
end t.Runner.run

fn t.dummy() -> i32 uses [] panics may
    0i32
end t.dummy

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut r = t.Runner { f: t.dummy }
    unsafe "getpid" "getpid をフィールドに代入" {
        r.f = t.getpid
    }
    let x = r.run()
    core.mem.drop(x)
    Ok(())
end t.main
"#),
        ("ug_a25_return_holder", r#"type t.Holder
    f: fn() -> i32 uses [] panics may
end t.Holder

extern t.getpid() -> i32 link "getpid"

fn t.dummy() -> i32 uses [] panics may
    0i32
end t.dummy

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let h = t.make_holder()
    let g = h.f
    let x = g()
    core.mem.drop(x)
    Ok(())
end t.main

fn t.make_holder() -> t.Holder uses [] panics may
    unsafe "getpid" "getpid を構造体に入れる" {
        t.Holder { f: t.getpid }
    }
end t.make_holder
"#),
        ("ug_a25_runner", r#"extern t.getpid() -> i32 link "getpid"

fn t.dummy() -> i32 uses [] panics may
    0i32
end t.dummy

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut f = t.dummy
    unsafe "getpid" "getpid を束縛" {
        f = t.getpid
    }
    let x = t.runner(f)
    core.mem.drop(x)
    Ok(())
end t.main

fn t.runner(f: fn() -> i32 uses [] panics may) -> i32 uses [] panics may
    f()
end t.runner
"#),
        ("ug_a25_struct_field", r#"type t.Holder
    f: fn() -> i32 uses [] panics may
end t.Holder

extern t.getpid() -> i32 link "getpid"

fn t.dummy() -> i32 uses [] panics may
    0i32
end t.dummy

fn t.main() -> Result<(), std.io.Error> uses [] panics may
    let mut h = t.Holder { f: t.dummy }
    unsafe "getpid" "getpid をフィールドに代入" {
        h.f = t.getpid
    }
    let g = h.f
    let x = g()
    core.mem.drop(x)
    Ok(())
end t.main
"#),
    ];
    assert_eq!(cases.len(), 9);
    for (name, src) in cases {
        let t = pkg(name, src, Some("t"));
        let (rc, got, out) = check(&t);
        assert_eq!(rc, Some(1), "{}: {}", name, out);
        assert!(got.iter().any(|c| c == "E5003"), "{}: {}", name, out);
        assert!(!got.iter().any(|c| c == "E5001"), "{}: {}", name, out);
    }
}
