//! `core.mem.size_of::<T>()` のインタプリタ結果が、ネイティブ生成の `kir::size_of` と
//! 一致することを確かめる (stage0/src/natives.rs の `size_of_ty` と stage0/src/kir.rs の
//! `size_of` のパリティ)。

mod common;

use common::{build_native_strict, run, text, Tmp};

/// `ksn run` の出力と、ネイティブ (debug/release) の実行ファイルの出力が一致することを確かめる。
fn check_parity(name: &str, src: &str, want: &str) {
    let t = Tmp::new(name, src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "run: {}{}", text(&interp.stderr), text(&interp.stdout));
    assert_eq!(text(&interp.stdout), want, "run の表示が想定と違う");
    for release in [false, true] {
        let (bt, exe, out) = build_native_strict(&format!("{}_r{}", name, release), src, release);
        let p = exe.unwrap_or_else(|| panic!("ネイティブの実行ファイルが無い (release: {}): {}", release, out));
        let o = std::process::Command::new(&p).current_dir(&bt.0).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "release {}: {}{}", release, text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), want, "native (release: {}) と run の表示が違う", release);
    }
}

/// プリミティブ・参照・ユニット・タプル・`layout c` 構造体の大きさ。
/// 生ポインタ (`*mut T`/`*const T`) は `[unsafe]` モジュールが必要なので省略。配列型
/// (`[T; N]`) は型実引数として `targ()` が `[` をエフェクト集合と誤って扱うため、
/// `size_of::<[T; N]>()` は構文エラーになる。
#[test]
fn size_of_parity() {
    check_parity(
        "size_of_parity",
        r#"type t.Ev layout c derive [Clone, Copy]
    kind: i32
    x: f32
    n: u64
end t.Ev

fn t.main() -> () uses [alloc, io.console] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    let a8 = core.mem.size_of::<u8>()
    let a16 = core.mem.size_of::<u16>()
    let a32 = core.mem.size_of::<u32>()
    let a64 = core.mem.size_of::<u64>()
    let az = core.mem.size_of::<usize>()
    let ar = core.mem.size_of::<&u8>()
    let au = core.mem.size_of::<()>()
    let ac = core.mem.size_of::<(u8, u16)>()
    let ad = core.mem.size_of::<(u64, u8)>()
    let ae = core.mem.size_of::<t.Ev>()
    match alloc.fmt.debug(&mut s, &a8) {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match s.push_str("|") {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match alloc.fmt.debug(&mut s, &a16) {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match s.push_str("|") {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match alloc.fmt.debug(&mut s, &a32) {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match s.push_str("|") {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match alloc.fmt.debug(&mut s, &a64) {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match s.push_str("|") {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match alloc.fmt.debug(&mut s, &az) {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match s.push_str("|") {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match alloc.fmt.debug(&mut s, &ar) {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match s.push_str("|") {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match alloc.fmt.debug(&mut s, &au) {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match s.push_str("|") {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match alloc.fmt.debug(&mut s, &ac) {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match s.push_str("|") {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match alloc.fmt.debug(&mut s, &ad) {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match s.push_str("|") {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match alloc.fmt.debug(&mut s, &ae) {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
    match std.io.print_line(s.as_str()) {
        Ok(_) => ()
        Err(_) => core.panic.panic("io")
    }
end t.main
"#,
        "1|2|4|8|8|8|0|4|16|16\n",
    );
}
