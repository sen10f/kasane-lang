//! 2026-11-20 N-gaps の続き (gap 10)。コレクションイテラの要素 (`&String` / `&Vec`) 上での
//! メソッド呼び出しをネイティブに下ろす (`HashSet<String>.iter()`, `BtreeMap<String,V>.iter()`,
//! `Vec<String>.iter()`)。それぞれネイティブ (debug/release) が `ksn run` と同じ表示・終了コードを
//! 出すことを確かめる。退避 (`native backend skipped`) は許さない。

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

/// 10-1. `HashSet<String>.iter()` の要素 `k: &String` 上で `k.len()` を呼ぶ。
#[test]
fn hashset_string_iter_len_native() {
    check_gap(
        "ngaps7_hashset_str_iter",
        r#"fn t.mk(text: &str) -> alloc.string.String uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(text))
    s
end t.mk

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut hs = alloc.collections.HashSet.new(std.alloc.SYSTEM)
    core.mem.drop(hs.insert(t.mk("aa")))
    core.mem.drop(hs.insert(t.mk("bbb")))
    let mut total = 0usize
    for k in hs.iter() {
        total = total.wrapping_add(k.len())
    }
    core.check.eq(total, 5usize)
    match std.io.print_line("ok") {
        Ok(_x) => Ok(())
        Err(_e) => core.panic.panic("io")
    }
end t.main
"#,
        "ok\n",
    );
}

/// 10-2. `BtreeMap<String,u32>.iter()` の要素 `(k, _v): (&String, &u32)` 上で `k.len()` を呼ぶ。
#[test]
fn btree_map_string_iter_len_native() {
    check_gap(
        "ngaps7_btreemap_str_iter",
        r#"fn t.mk(text: &str) -> alloc.string.String uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(text))
    s
end t.mk

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut bm = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)
    core.mem.drop(bm.insert(t.mk("x"), 1u32))
    core.mem.drop(bm.insert(t.mk("yy"), 2u32))
    let mut ktotal = 0usize
    for (k, _v) in bm.iter() {
        ktotal = ktotal.wrapping_add(k.len())
    }
    core.check.eq(ktotal, 3usize)
    match std.io.print_line("ok") {
        Ok(_x) => Ok(())
        Err(_e) => core.panic.panic("io")
    }
end t.main
"#,
        "ok\n",
    );
}

/// 10-3. `Vec<String>.iter()` の要素 `k: &String` 上で `k.len()` を呼ぶ。
#[test]
fn vec_string_iter_len_native() {
    check_gap(
        "ngaps7_vec_str_iter",
        r#"fn t.mk(text: &str) -> alloc.string.String uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(text))
    s
end t.mk

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(t.mk("foo")))
    core.mem.drop(v.push(t.mk("barbaz")))
    let mut vtotal = 0usize
    for k in v.iter() {
        vtotal = vtotal.wrapping_add(k.len())
    }
    core.check.eq(vtotal, 9usize)
    match std.io.print_line("ok") {
        Ok(_x) => Ok(())
        Err(_e) => core.panic.panic("io")
    }
end t.main
"#,
        "ok\n",
    );
}
