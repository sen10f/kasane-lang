//! LLVM の `memory(argmem: ..)` の約束の回帰 (Mythos 3 回目の L1)。
//! 値で受けた組 `(&u64, u8)` の中の参照を辿る読みは argmem に入らない。以前は argmem: read を付けて、
//! release のネイティブが誤った値 (0) を出した。インタプリタと release のネイティブの出力が一致することを確かめる。

mod common;

use common::*;
use std::process::Command as Cmd;

#[test]
fn tuple_with_ref_param_is_not_argmem_release_matches_interp() {
    let src = std::fs::read_to_string(root().join("stage0/tests/data/llvm_attrs/argmem_tuple_ref.ksnl")).unwrap();
    let t = Tmp::new("argmem_tuple_run", &src);
    let o = run(&["run"], &t.0);
    let interp_out = text(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "interp: {}", text(&o.stderr));
    let (nt, native, out) = build_native_strict("argmem_tuple_native", &src, true);
    match native {
        Some(exe) => {
            let n = Cmd::new(&exe).current_dir(&nt.0).output().unwrap();
            assert_eq!(text(&n.stdout), interp_out, "release のネイティブがインタプリタと違う (argmem の約束が崩れている)");
        }
        None => assert_native_or_reasoned_skip(&out),
    }
}

/// `&mut Vec` / `&Vec` の引数への `noalias` (ヘッダだけ。データの確保は別) が、`push` による再確保を含む更新でも release で正しい。
#[test]
fn vec_header_noalias_release_matches_interp() {
    let src = std::fs::read_to_string(root().join("stage0/tests/data/llvm_attrs/vec_noalias.ksnl")).unwrap();
    let t = Tmp::new("vec_noalias_run", &src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "interp: {}", text(&o.stderr));
    let interp_out = text(&o.stdout);
    let (nt, native, out) = build_native_strict("vec_noalias_native", &src, true);
    match native {
        Some(exe) => {
            let n = Cmd::new(&exe).current_dir(&nt.0).output().unwrap();
            assert_eq!(text(&n.stdout), interp_out);
        }
        None => assert_native_or_reasoned_skip(&out),
    }
}
