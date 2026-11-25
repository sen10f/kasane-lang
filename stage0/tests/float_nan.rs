//! 浮動小数の NaN: ビットが見える所 (`to_bits`・メモリ・構造体・書式) でだけ正規化する (設計_KIR.md 17.8)。インタプリタとネイティブが一致する。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;
use std::process::Command;

const SRC: &str = "type t.P
    x: f64
    y: f32
end t.P

fn t.show(s: &mut std.string.String, tag: &str, v: u64) -> () uses [alloc] panics may
    s.push_str(tag).unwrap()
    s.push_str(\"=\").unwrap()
    s.push_u64(v).unwrap()
    s.push_str(\"\\n\").unwrap()
end t.show

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut s = std.string.String.new()
    let sn = f64.from_bits(0x7FF0000000000001u64)
    let neg = f64.from_bits(0xFFF8000000000123u64)
    let z = 0.0f64
    t.show(&mut s, \"div\", (z / z).to_bits())
    t.show(&mut s, \"add\", (sn + 1.0f64).to_bits())
    t.show(&mut s, \"mul\", (neg * 2.0f64).to_bits())
    t.show(&mut s, \"sqrt\", (-1.0f64).sqrt().to_bits())
    t.show(&mut s, \"cast\", u64.from_u32(f32.from_bits(0x7F800001u32).to_bits()))
    t.show(&mut s, \"f32mul\", u64.from_u32((f32.from_bits(0xFFC00042u32) * 3.0f32).to_bits()))
    let p = t.P { x: z / z, y: f32.from_bits(0x7F800001u32) + 1.0f32 }
    t.show(&mut s, \"px\", p.x.to_bits())
    t.show(&mut s, \"py\", u64.from_u32(p.y.to_bits()))
    let mut v = std.vec.Vec.new()
    v.push(sn * 1.0f64).unwrap()
    match v.get(0usize) {
        Some(e) => t.show(&mut s, \"vec\", e.to_bits())
        None => core.mem.drop(0u8)
    }
    std.io.print_line(s.as_str())
end t.main
";

#[test]
fn nan_bits_match_between_interpreter_and_native() {
    let (t, exe_path, out) = build_native_strict("float_nan", SRC, true);
    let Some(exe_path) = exe_path else {
        assert!(out.contains("native backend skipped"), "{}", out);
        return;
    };
    let native = Command::new(&exe_path).output().unwrap();
    let interp = run(&["run"], &t.0);
    assert_eq!(text(&interp.status.code().map(|c| c.to_string()).unwrap_or_default().into_bytes()), "0", "{}", text(&interp.stderr));
    assert_eq!(text(&native.stdout), text(&interp.stdout), "{}", text(&native.stderr));
    // 見える所はどれも正の quiet NaN。
    let o = text(&native.stdout);
    for (tag, bits) in [("div", 0x7FF8000000000000u64), ("add", 0x7FF8000000000000), ("mul", 0x7FF8000000000000), ("sqrt", 0x7FF8000000000000), ("px", 0x7FF8000000000000), ("vec", 0x7FF8000000000000)] {
        assert!(o.contains(&format!("{}={}\n", tag, bits)), "{}: {}", tag, o);
    }
    assert!(o.contains(&format!("py={}\n", 0x7FC00000u32)) && o.contains(&format!("cast={}\n", 0x7FC00000u32)), "{}", o);
}

/// NaN を extern の C 関数 (libm の `fabs`・`sqrt`) へ渡す・C から受け取る。どちらも `to_bits` で正の quiet NaN にそろう。
/// (標準ライブラリの `core.hash` は浮動小数をハッシュしない (`spec/stdlib/core/hash.sig` に f32 / f64 が無い) ので、ハッシュの試験は無い。)
#[test]
fn nan_through_extern_c_matches_between_interpreter_and_native() {
    let src = "extern t.fabs(x: f64) -> f64 link \"fabs\"

extern t.sqrt(x: f64) -> f64 link \"sqrt\"

fn t.main() -> Result<(), std.io.Error> uses [io.console, alloc] panics may
    let mut s = std.string.String.new()
    let a = unsafe \"libc\" \"fabs of a NaN\" {
        t.fabs(f64.from_bits(0xFFF0000000000001u64))
    }
    let b = unsafe \"libc\" \"sqrt of a negative\" {
        t.sqrt(-1.0f64)
    }
    s.push_str(\"fabs=\").unwrap()
    s.push_u64(a.to_bits()).unwrap()
    s.push_str(\" sqrt=\").unwrap()
    s.push_u64(b.to_bits()).unwrap()
    core.mem.drop(std.io.print_line(s.as_str()))
    Ok(())
end t.main
";
    let t = Tmp::new("float_nan_ffi", src);
    let manifest = t.0.join("kasane.toml");
    let toml = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, toml.replacen("[dependencies]", "[unsafe]\nmodules = [\"t\"]\n\n[dependencies]", 1)).unwrap();
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    let want = "fabs=9221120237041090560 sqrt=9221120237041090560\n";
    assert_eq!(text(&interp.stdout), want);
    let b = run(&["build", "--release", "--allow-bundle"], &t.0);
    let p = t.0.join("target/release/t");
    if crate::common::artifact_is_native(&p) {
        let n = Command::new(&p).output().unwrap();
        assert_eq!(text(&n.stdout), want, "{}{}", text(&n.stderr), text(&b.stderr));
    }
}

/// VM (`--engine=vm`) も、木走査・ネイティブと同じく `to_bits`・構造体の欄・`Vec` の要素で NaN を正規化する。
#[test]
fn nan_bits_match_on_the_vm_engine() {
    let t = Tmp::new("float_nan_vm", SRC);
    let tree = run(&["run"], &t.0);
    let vm = run(&["run", "--engine=vm"], &t.0);
    assert_eq!(vm.status.code(), Some(0), "{}", text(&vm.stderr));
    assert_eq!(text(&vm.stdout), text(&tree.stdout), "{}", text(&vm.stderr));
    assert!(text(&vm.stdout).contains("div=9221120237041090560\n"), "{}", text(&vm.stdout));
}

const STORE_SRC: &str = "type t.P
    x: f64
    y: f32
end t.P

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut s = std.string.String.new()
    let p = t.P {
        x: f64.from_bits(0xFFF8000000000123u64)
        y: f32.from_bits(0xFFC00042u32)
    }
    let mut v = std.vec.Vec.new()
    v.push(f64.from_bits(0x7FF0000000000001u64)).unwrap()
    s.push_u64(p.x.to_bits()).unwrap()
    s.push_str(\" \").unwrap()
    s.push_u64(u64.from_u32(p.y.to_bits())).unwrap()
    s.push_str(\" \").unwrap()
    match v.get(0usize) {
        Some(e) => s.push_u64(e.to_bits()).unwrap()
        None => core.mem.drop(0u8)
    }
    std.io.print_line(s.as_str())
end t.main
";

/// 非正規のペイロードの NaN をそのまま `Vec` と構造体の欄へ入れて読み戻しても、`to_bits` は正の quiet NaN (メモリへの書き出しでは正規化しない)。
/// 木走査・VM・ネイティブ (release) で同じ。
#[test]
fn stored_noncanonical_nan_reads_back_canonical_via_to_bits() {
    let want = format!("{} {} {}\n", 0x7FF8000000000000u64, 0x7FC00000u32, 0x7FF8000000000000u64);
    let t = Tmp::new("float_nan_store", STORE_SRC);
    let tree = run(&["run"], &t.0);
    assert_eq!(text(&tree.stdout), want, "{}", text(&tree.stderr));
    let vm = run(&["run", "--engine=vm"], &t.0);
    assert_eq!(text(&vm.stdout), want, "{}", text(&vm.stderr));
    let (_t2, exe_path, out) = build_native_strict("float_nan_store_n", STORE_SRC, true);
    let Some(exe_path) = exe_path else {
        assert!(out.contains("native backend skipped"), "{}", out);
        return;
    };
    let n = Command::new(&exe_path).output().unwrap();
    assert_eq!(text(&n.stdout), want, "{}", text(&n.stderr));
}
