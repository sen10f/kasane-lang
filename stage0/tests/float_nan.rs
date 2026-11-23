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
