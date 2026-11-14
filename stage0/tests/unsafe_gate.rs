// 3.5 / 3.8: `[unsafe] modules` の `unsafe` の許可を、テスト用パッケージ (`kasane.toml` の `[unsafe]`) で確かめる。
// 許可のないモジュールでの生ポインタは E5001。許可モジュールの `unsafe` ブロックの中の `core.ptr` は通る。
//
// 塞いでいない穴: 許可モジュールの中でも `core.ptr` の関数を `unsafe` の外で呼ぶと通る (3.8 違反)。
// 塞ぐと `examples/ffi` と既存のテスト (`cache_incremental` `cross_target` `kir_*` `ptr_*` `ffi_*` など) が
// `unsafe` の外の `core.ptr` で落ちるので、それらを書き換えるまで保留している。
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
