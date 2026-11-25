//! リテラルの静的な昇格 (`let r: &'static u32 = &10u32` を返す): 木走査・VM・ネイティブ (release) で、
//! 返した後も参照が有効なこと (実体は関数の枠でなく静的な領域)。

mod common;
use common::*;
use std::process::Command;

const SRC: &str = "fn t.clobber(n: u32) -> u32 uses [] panics never\n    let a = [n; 64usize]\n    match a.get(3usize) {\n        Some(x) => *x\n        None => 0u32\n    }\nend t.clobber\n\nfn t.k() -> &'static u32 uses [] panics never\n    let r: &'static u32 = &10u32\n    r\nend t.k\n\nfn t.main() -> () uses [process] panics never\n    let p = t.k()\n    let c = t.clobber(99u32)\n    if *p == 10u32 && c == 99u32 {\n        std.process.exit(0i32)\n    } else {\n        std.process.exit(1i32)\n    }\nend t.main\n";

#[test]
fn promoted_literal_reference_outlives_the_frame_in_every_engine() {
    let t = Tmp::new("promoted_static", SRC);
    assert_eq!(run(&["check"], &t.0).status.code(), Some(0));
    for args in [&["run", "--engine=tree"][..], &["run"][..]] {
        assert_eq!(run(args, &t.0).status.code(), Some(0), "{:?}", args);
    }
    let (_t2, bin, out) = build_native_strict("promoted_static_nat", SRC, true);
    let bin = bin.unwrap_or_else(|| panic!("native build failed: {}", out));
    assert_eq!(Command::new(bin).status().unwrap().code(), Some(0));
}
