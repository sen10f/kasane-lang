//! `ksn build` で下ろせない関数の理由を全件出すこと、`ksn defs` で `.sig` の宣言を出すことを確かめる。

mod common;
use common::*;

#[test]
fn build_shows_all_lower_reasons() {
    // `dyn` トレイトオブジェクトを引数に取る関数は KIR に下ろせない。
    // `t.call` と `t.helper` の 2 つが下ろせず、両方の理由が出ること。
    let src = "trait t.Speak[E: uses, P: panics]\n    fn speak(self: &Self) -> u32 uses [E] panics P\nend t.Speak\n\n\
type t.Dog\n    n: u32\nend t.Dog\n\n\
impl t.Speak<[], never> for t.Dog\nfn t.Dog.speak(self: &t.Dog) -> u32 uses [] panics never impls t.Speak<[], never>\n    self.n\nend t.Dog.speak\n\n\
fn t.call(s: &dyn t.Speak<[], never>) -> u32 uses [] panics never\n    s.speak()\nend t.call\n\n\
fn t.helper(s: &dyn t.Speak<[], never>) -> u32 uses [] panics never\n    s.speak()\nend t.helper\n\n\
fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    let d = t.Dog { n: 3u32 }\n    core.check.eq(t.call(&d), 3u32)\n    core.check.eq(t.helper(&d), 3u32)\n    std.io.print_line(\"x\")\nend t.main\n";
    let t = Tmp::new("build_reasons", src);
    let b = run(&["build"], &t.0);
    let out = text(&b.stdout) + &text(&b.stderr);
    assert_eq!(b.status.code(), Some(0), "{}{}", out, text(&b.stderr));
    // 2 つの下ろせない関数の理由が両方出ること。
    assert!(out.contains("t.call を KIR に下ろせない"), "{}", out);
    assert!(out.contains("t.helper を KIR に下ろせない"), "{}", out);
    // 関数ごとにファイルと行が出ること。
    assert!(out.contains("src/t.ksnl:"), "{}", out);
    // 既定の経路へ退避すること。
    assert!(out.contains("native backend skipped"), "{}", out);
    assert!(out.contains("the interpreter bundle is used"), "{}", out);
}

#[test]
fn defs_reads_sig_declarations() {
    let sig = "fn t.sig.foo(x: u32) -> u32 pub\nfn t.sig.bar(y: u32) -> u32 pub\n";
    let t = tmp_files("defs_sig", &[("t.sig", sig)]);
    let path_str = t.0.join("src/t.sig").to_string_lossy().to_string();
    let d = run(&["defs", &path_str], &t.0);
    assert_eq!(d.status.code(), Some(0), "{}", text(&d.stderr));
    let out = text(&d.stdout);
    // 宣言を定義と同じ形 (種別・FQN・行) で出すこと。
    assert!(out.contains("fn\tt.sig.foo\t1\t0"), "{}", out);
    assert!(out.contains("fn\tt.sig.bar\t2\t0"), "{}", out);
}

#[test]
fn defs_errors_on_unreadable_file() {
    let t = Tmp::new("defs_unreadable", "fn t.main() -> () uses [] panics never\nend t.main\n");
    let d = run(&["defs", "/nonexistent/nope.sig"], &t.0);
    assert_eq!(d.status.code(), Some(1));
    let err = text(&d.stderr);
    assert!(err.contains("読めない"), "{}", err);
}
