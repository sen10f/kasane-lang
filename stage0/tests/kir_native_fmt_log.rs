//! `core.fmt.Formatter` を型表に持つ標準ライブラリ (`std.log` など) と `alloc.fmt.debug` の下ろし (G12)。
//! 木走査 (`ksn run`) と LLVM (debug / release) の標準出力が一致し、release がバンドルへ退避しないことを確かめる。
//! (`std.log` は標準エラーへ時刻つきの行を書くので、標準出力だけを比べる。)

mod common;
use common::*;

fn check(name: &str, src: &str) -> String {
    let out = agree_with_interpreter(name, src);
    let (_t, exe, why) = build_native_strict(&format!("{}_strict", name), src, true);
    assert!(exe.is_some(), "release のネイティブ生成が失敗した: {}", why);
    out
}

/// `std.log.warn` / `info` を呼ぶ (以前は「呼び先 std.log.info が未対応」で E9040)
#[test]
fn native_g12_log() {
    let out = check("g12_log", r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, time] panics never
    std.log.warn("careful", &[("k", "v")])?
    std.log.info("started", &[("port", "8080"), ("mode", "dev")])?
    std.io.print_line("done")?
    Ok(())
end t.main
"#);
    assert_eq!(out, "done\n");
}

/// `String` と `derive [Debug]` の型を `alloc.fmt.debug` で書く
#[test]
fn native_g12_debug_string_and_derive() {
    let out = check("g12_debug", r#"type t.P derive [Debug] pub
    a: u32
    b: bool
end t.P

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str("hi\n"))
    let mut out = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(alloc.fmt.debug(&mut out, &s))
    let p = t.P { a: 7u32, b: true }
    core.mem.drop(alloc.fmt.debug(&mut out, &p))
    std.io.print_line(out.as_str())?
    Ok(())
end t.main
"#);
    assert_eq!(out, "\"hi\\n\"P { a: 7, b: true }\n");
}
