//! ネイティブに下ろせなかった 5 つの形 (kir/native-gaps): それぞれネイティブ (debug/release) が
//! `ksn run` と同じ表示・終了コードを出すことを確かめる。退避 (`native backend skipped`) は許さない。

mod common;

use common::{build_native_strict, run, text, Tmp};

/// `ksn run` の出力と、ネイティブ (debug/release) の実行ファイルの出力が一致することを確かめる。
fn check_gap(name: &str, src: &str, want: &str) {
    let t = Tmp::new(name, src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "run: {}{}", text(&interp.stderr), text(&interp.stdout));
    assert_eq!(text(&interp.stdout), want, "run の表示が想定と違う");
    for release in [false, true] {
        let (bt, exe, out) = build_native_strict(&format!("{}_{}", name, release), src, release);
        let p = exe.unwrap_or_else(|| panic!("ネイティブの実行ファイルが無い (release: {}): {}", release, out));
        let o = std::process::Command::new(&p).current_dir(&bt.0).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "release {}: {}{}", release, text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), want, "native (release: {}) と run の表示が違う", release);
    }
}

/// 1. `Vec<T>` の `{:?}` / derive [Debug] (要素が String・入れ子 Vec・構造体)。
#[test]
fn vec_debug_native() {
    check_gap(
        "ngaps_vec_debug",
        r#"type t.Inner derive [Debug]
    name: alloc.string.String<std.alloc.System>
end t.Inner

type t.Cfg derive [Debug]
    title: alloc.string.String<std.alloc.System>
    tags: alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>
    nums: alloc.vec.Vec<u64, std.alloc.System>
    inner: t.Inner
end t.Cfg

fn t.mk(x: &str) -> Result<alloc.string.String<std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics never
    alloc.string.String.from_str(std.alloc.SYSTEM, x)
end t.mk

fn t.main() -> Result<(), alloc.alloc.AllocError> uses [alloc, io.console] panics may
    let tt = t.mk("T")?
    let a = t.mk("aa")?
    let b = t.mk("b\"b")?
    let tags0 = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut tags = tags0
    tags.push(a)?
    tags.push(b)?
    let mut nums = alloc.vec.Vec.new(std.alloc.SYSTEM)
    nums.push(7u64)?
    nums.push(8u64)?
    let mut inner_v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    inner_v.push(1u32)?
    let mut nested = alloc.vec.Vec.new(std.alloc.SYSTEM)
    nested.push(inner_v)?
    let n = t.mk("nn")?
    let c = t.Cfg { title: tt, tags: tags, nums: nums, inner: t.Inner { name: n } }
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    alloc.fmt.debug(&mut s, &c)?
    s.push_str("|")?
    alloc.fmt.debug(&mut s, &nested)?
    match std.io.print_line(s.as_str()) {
        Ok(_x) => Ok(())
        Err(_e) => core.panic.panic("io")
    }
end t.main
"#,
        "Cfg { title: \"T\", tags: [\"aa\", \"b\\\"b\"], nums: [7, 8], inner: Inner { name: \"nn\" } }|[[1]]\n",
    );
}

/// 2. derive [ToJson, FromJson] の構造体に `Vec<String>` と入れ子の構造体がある場合。
#[test]
fn derive_json_vec_string_native() {
    check_gap(
        "ngaps_json",
        r#"type t.Inner derive [Debug, ToJson, FromJson]
    name: alloc.string.String<std.alloc.System>
end t.Inner

type t.Cfg derive [Debug, ToJson, FromJson]
    title: alloc.string.String<std.alloc.System>
    tags: alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>
    inner: t.Inner
end t.Cfg

fn t.mk(x: &str) -> Result<alloc.string.String<std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics never
    alloc.string.String.from_str(std.alloc.SYSTEM, x)
end t.mk

fn t.tags1(x: alloc.string.String<std.alloc.System>) -> Result<alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    v.push(x)?
    Ok(v)
end t.tags1

fn t.main() -> Result<(), alloc.alloc.AllocError> uses [alloc, io.console] panics may
    let tt = t.mk("t")?
    let x = t.mk("x")?
    let n = t.mk("n")?
    let tags = t.tags1(x)?
    let c = t.Cfg { title: tt, tags: tags, inner: t.Inner { name: n } }
    let mut out = alloc.string.String.new(std.alloc.SYSTEM)
    c.to_json(&mut out)?
    let j = t.mk("{\"title\":\"t\",\"tags\":[\"x\"],\"inner\":{\"name\":\"n\"}}")?
    let doc = match std.json.parse(j.as_str()) {
        Ok(d) => d
        Err(_e) => core.panic.panic("json")
    }
    let c2 = match t.Cfg.from_json(&doc, doc.root()) {
        Ok(v) => v
        Err(_e) => core.panic.panic("json")
    }
    let mut out2 = alloc.string.String.new(std.alloc.SYSTEM)
    c2.to_json(&mut out2)?
    match std.io.print_line(out.as_str()) {
        Ok(_x) => match std.io.print_line(out2.as_str()) {
            Ok(_y) => Ok(())
            Err(_e) => core.panic.panic("io")
        }
        Err(_e) => core.panic.panic("io")
    }
end t.main
"#,
        "{\"title\":\"t\",\"tags\":[\"x\"],\"inner\":{\"name\":\"n\"}}\n{\"title\":\"t\",\"tags\":[\"x\"],\"inner\":{\"name\":\"n\"}}\n",
    );
}

/// 3. `Vec<T>.try_clone()` (スカラー要素と String 要素)。
#[test]
fn vec_try_clone_native() {
    check_gap(
        "ngaps_try_clone",
        r#"fn t.mk(x: &str) -> Result<alloc.string.String<std.alloc.System>, alloc.alloc.AllocError> uses [alloc] panics never
    alloc.string.String.from_str(std.alloc.SYSTEM, x)
end t.mk

fn t.main() -> Result<(), alloc.alloc.AllocError> uses [alloc, io.console] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    v.push(7u64)?
    v.push(8u64)?
    let c = match v.try_clone() {
        Ok(x) => x
        Err(_e) => core.panic.panic("clone")
    }
    let mut vs = alloc.vec.Vec.new(std.alloc.SYSTEM)
    vs.push(t.mk("aa")?)?
    vs.push(t.mk("b\"b")?)?
    let cs = match vs.try_clone() {
        Ok(x) => x
        Err(_e) => core.panic.panic("clone")
    }
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    alloc.fmt.debug(&mut s, &c)?
    s.push_str("|")?
    alloc.fmt.debug(&mut s, &cs)?
    match std.io.print_line(s.as_str()) {
        Ok(_x) => Ok(())
        Err(_e) => core.panic.panic("io")
    }
end t.main
"#,
        "[7, 8]|[\"aa\", \"b\\\"b\"]\n",
    );
}

/// 4. 利用者が書いた `impl core.fmt.Debug` / `Display` (`push_debug` / `push_display` の受け皿)。
#[test]
fn user_fmt_impl_native() {
    check_gap(
        "ngaps_user_fmt",
        r#"type t.P
    x: u64
end t.P

impl core.fmt.Debug for t.P
fn t.P.debug_fmt(self: &t.P, f: &mut core.fmt.Formatter) -> Result<(), core.fmt.Error> uses [] panics never impls core.fmt.Debug
    if self.x == 42u64 {
        f.write_str("P42")?
    } else {
        f.write_str("P?")?
    }
    Ok(())
end t.P.debug_fmt

impl core.fmt.Display for t.P
fn t.P.display_fmt(self: &t.P, f: &mut core.fmt.Formatter) -> Result<(), core.fmt.Error> uses [] panics never impls core.fmt.Display
    f.write_str("disp")?
    Ok(())
end t.P.display_fmt

fn t.main() -> Result<(), alloc.alloc.AllocError> uses [alloc, io.console] panics may
    let p = t.P { x: 42u64 }
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    alloc.fmt.debug(&mut s, &p)?
    s.push_str("|")?
    s.push_display(&p)?
    match std.io.print_line(s.as_str()) {
        Ok(_x) => Ok(())
        Err(_e) => core.panic.panic("io")
    }
end t.main
"#,
        "P42|disp\n",
    );
}

/// 5. `core.sync.AtomicBool` / `AtomicU64` / `AtomicUsize` (LLVM では実アトミック命令)。
#[test]
fn atomics_native() {
    check_gap(
        "ngaps_atomics",
        r#"fn t.main() -> Result<(), alloc.alloc.AllocError> uses [alloc, io.console] panics may
    let a = core.sync.AtomicU64.new(7u64)
    a.store(9u64, core.sync.Ordering.SeqCst)
    let old = a.swap(11u64, core.sync.Ordering.SeqCst)
    let got = a.load(core.sync.Ordering.SeqCst)
    let sum = a.fetch_add(5u64, core.sync.Ordering.SeqCst)
    let cas = match a.compare_exchange(16u64, 20u64, core.sync.Ordering.SeqCst) {
        Ok(v) => v
        Err(_e) => 999u64
    }
    let b = core.sync.AtomicBool.new(true)
    let bold = b.swap(false, core.sync.Ordering.SeqCst)
    let n = core.sync.AtomicUsize.new(3usize)
    let nold = n.fetch_add(1usize, core.sync.Ordering.SeqCst)
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    alloc.fmt.debug(&mut s, &got)?
    s.push_str("/")?
    alloc.fmt.debug(&mut s, &old)?
    s.push_str("/")?
    alloc.fmt.debug(&mut s, &sum)?
    s.push_str("/")?
    alloc.fmt.debug(&mut s, &cas)?
    s.push_str("/")?
    alloc.fmt.debug(&mut s, &bold)?
    s.push_str("/")?
    alloc.fmt.debug(&mut s, &nold)?
    match std.io.print_line(s.as_str()) {
        Ok(_x) => Ok(())
        Err(_e) => core.panic.panic("io")
    }
end t.main
"#,
        "11/9/11/16/true/3\n",
    );
}

/// 6. `core.check.eq` で浮動小数 (f32/f64) を含む構造体 — 失敗の文言 (最短往復形・nan/inf/-0.0) も一致。
#[test]
fn float_check_eq_native() {
    // 失敗する検査: 文言の一致 (stderr) と終了コードも見るため run とネイティブの両方を走らせて比べる。
    let src = r#"type t.Vec2 derive [PartialEq, Debug]
    x: f64
    y: f64
end t.Vec2

fn t.main() -> () uses [] panics may
    let nan = 0.0f64 / 0.0f64
    let inf = 1.0f64 / 0.0f64
    let a = t.Vec2 { x: nan, y: inf }
    let b = t.Vec2 { x: -0.0f64, y: 1e-300f64 }
    core.check.eq(a, b)
end t.main
"#;
    let t = Tmp::new("ngaps_float_eq", src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(101), "run: {}{}", text(&interp.stderr), text(&interp.stdout));
    let want = text(&interp.stderr);
    assert!(want.contains("nan") && want.contains("inf") && want.contains("-0.0"), "文言: {}", want);
    for release in [false, true] {
        let (bt, exe, out) = build_native_strict(&format!("ngaps_float_eq_{}", release), src, release);
        let p = exe.unwrap_or_else(|| panic!("ネイティブの実行ファイルが無い (release: {}): {}", release, out));
        let o = std::process::Command::new(&p).current_dir(&bt.0).output().unwrap();
        assert_eq!(o.status.code(), Some(101), "release {}: {}{}", release, text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stderr), want, "native (release: {}) と run の失敗文言が違う", release);
    }
    // 等しいとみなされる組 (-0.0 == 0.0、NaN は不一致) の通過側。
    check_gap(
        "ngaps_float_eq_ok",
        r#"type t.Vec2 derive [PartialEq, Debug]
    x: f32
    y: f32
end t.Vec2

fn t.main() -> Result<(), alloc.alloc.AllocError> uses [alloc, io.console] panics may
    let a = t.Vec2 { x: -0.0f32, y: 2.5f32 }
    let b = t.Vec2 { x: 0.0f32, y: 2.5f32 }
    core.check.eq(a, b)
    match std.io.print_line("ok") {
        Ok(_x) => Ok(())
        Err(_e) => core.panic.panic("io")
    }
end t.main
"#,
        "ok\n",
    );
}
