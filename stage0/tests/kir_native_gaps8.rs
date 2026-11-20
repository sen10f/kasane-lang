//! 2026-11-20 S5 計器の続き (gap 8)。定数の型の拡張 (浮動小数・タプル・非整数欄の構造体の定数を
//! 使う場所で下ろす) と、固定長配列の `as_mut_slice` / `as_slice` をネイティブに下ろす。
//! ネイティブ (debug/release) が `ksn run` と同じ表示を出すことを確かめる。退避は許さない。

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

/// 8-1. 浮動小数・タプルの定数を使う。
#[test]
fn const_float_and_tuple_native() {
    check_gap(
        "ngaps8_const_float_tuple",
        r#"const t.H: f32 = 36.0f32
const t.P: (u8, i32) = (1u8, 2i32)

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let (a, b) = t.P
    let ok = t.H > 1.0f32 && a == 1u8 && b == 2i32
    if ok {
        match std.io.print_line("ok") {
            Ok(_x) => Ok(())
            Err(_e) => core.panic.panic("io")
        }
    } else {
        core.panic.panic("const")
    }
end t.main
"#,
        "ok\n",
    );
}
