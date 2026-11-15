//! `alloc.string.String` の基本メソッドを `alloc/string.ksnl` (`Buf<u8>` の上の Kasane) で書いたことの検査 (修正プラン 3-3)。
//! 木走査 (`ksn run`) とネイティブ (`ksn build`) の出力が同じで期待値に一致すること、
//! ネイティブの KIR が組み込みではなく Kasane 版の単相インスタンス (`alloc.string.String.push_str<..>`) を呼ぶことを見る。

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";

/// 文字列の基本メソッド (`push` / `push_str` / `push_u64` / `push_i64` / `push_bytes` / `as_str` / `len` / `clear` / `is_empty`) の全部を通す。
/// 符号化は 1・2・3・4 バイトの文字すべて、整数は最大・最小・0・負の数を含む。
const PROG: &str = "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(\"héllo\"))
    core.mem.drop(s.push('€'))
    core.mem.drop(s.push('😀'))
    core.mem.drop(s.push('a'))
    core.mem.drop(s.push_u64(18446744073709551615u64))
    core.mem.drop(s.push_i64(-9223372036854775808i64))
    core.mem.drop(s.push_i64(0i64))
    core.mem.drop(s.push_i64(-42i64))
    core.mem.drop(s.push_u64(0u64))
    core.mem.drop(s.push_bytes(\"!\".as_bytes()))
    std.io.print_line(s.as_str())?
    core.check.eq(s.len(), 60usize)
    core.check.eq(s.as_str().len(), 60usize)
    s.clear()
    core.check.eq(s.is_empty(), true)
    core.mem.drop(s.push_str(\"ok\"))
    std.io.print_line(s.as_str())?
    Ok(())
end t.main
";

/// `PROG` の期待される出力 (1 行目は 60 バイト、2 行目は clear の後)。
const EXPECT: &str = "héllo€😀a18446744073709551615-92233720368547758080-420!\nok\n";

/// `n` 回足して `Buf` の拡張 (`reserve_bytes` と `grow`) の経路を通す。長さ `2 * n` を返す (KIR の検査にも使う)。
const GROW: &str = "fn t.work(n: usize) -> usize uses [alloc] panics never pub
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    let mut i = 0usize
    while i < n {
        core.mem.drop(s.push_str(\"ab\"))
        i = i.wrapping_add(1usize)
    }
    s.len()
end t.work
";

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str, entry: &str, src: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_string_kasane_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), TOML.replace("t.main", entry)).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(d.join("src").join("t.ksnl"), src).unwrap();
        Tmp(d)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str], cwd: &PathBuf) -> Output {
    Command::new(exe()).args(args).current_dir(cwd).output().expect("spawn ksn")
}

#[test]
fn string_kasane_push_family_matches_in_tree_and_native_paths() {
    let t = Tmp::new("family", "t.main", PROG);
    let chk = run(&["check"], &t.0);
    assert!(chk.status.success(), "{}{}", text(&chk.stdout), text(&chk.stderr));
    let tree = run(&["run"], &t.0);
    assert_eq!(tree.status.code(), Some(0), "{}{}", text(&tree.stdout), text(&tree.stderr));
    assert_eq!(text(&tree.stdout), EXPECT, "木走査の出力が期待値と違う");
    let cache = std::env::temp_dir().join(format!("ksn_string_kasane_cache_{}", std::process::id()));
    let b = Command::new(exe()).args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    let out = text(&b.stdout) + &text(&b.stderr);
    assert!(b.status.success(), "{}", out);
    assert!(!out.contains("backend skipped"), "ネイティブにならず退避した:\n{}", out);
    let native = t.0.join("target").join("debug").join("t.native");
    let o = Command::new(&native).output().expect("run native");
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout), EXPECT, "ネイティブの出力が期待値と違う");
}

#[test]
fn string_kasane_push_str_is_a_kasane_instance_natively() {
    let t = Tmp::new("grow", "t.work", GROW);
    let kir = run(&["kir", "t.work"], &t.0);
    let s = text(&kir.stdout);
    assert!(kir.status.success(), "{}{}", s, text(&kir.stderr));
    assert!(s.contains("call alloc.string.String.push_str<std.alloc.System>"), "push_str が Kasane 版の単相インスタンスになっていない:\n{}", s);
    assert!(s.contains("fn alloc.string.String.reserve_bytes key=alloc.string.String.reserve_bytes<std.alloc.System>"), "reserve_bytes の単相インスタンスがない:\n{}", s);
}
