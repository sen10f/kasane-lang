//! B10: 相対 `link "./libX.so:..."` は実行時の CWD ではなくパッケージルートから解く。
//! インタプリタ (`ffi::open_lib` → `dlopen`) とネイティブ
//! (`kir_rt.c::ksn_ext_resolve` → `dlopen`、`ksn.ext.lib.*` 文字列) の両方が同じ絶対パスを見る。
//! 異なる CWD に同名の evil `.so` を置いても、パッケージ内の正しい `.so` がロードされることを確かめる。

#![allow(dead_code, unused_imports)]

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use common::*;

/// パッケージルートに正しい `librele.so` (`rel_path` は 777 を返す) を置き、
/// CWD として使う別ディレクトリ `evilcwd/` に同名の evil `librele.so` (`rel_path` は 888 を返す) を置く。
#[cfg(unix)]
fn relative_link_pkg() -> (Tmp, PathBuf) {
    let t = Tmp::new("ffi_relative_link", "");
    fs::write(
        t.0.join("kasane.toml"),
        TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t\"]\n\n[dependencies]", 1),
    )
    .unwrap();
    // パッケージルートに置く正しい共有ライブラリ (`rel_path` は 777 を返す)。
    let c = t.0.join("rele.c");
    fs::write(&c, "int rel_path(void) { return 777; }\n").unwrap();
    let lib = t.0.join("librele.so");
    let o = cc(&["-shared", "-fPIC", "-O1", "-o", lib.to_str().unwrap(), c.to_str().unwrap()], &t.0);
    assert!(o.status.success(), "good .so のビルドに失敗: {}", text(&o.stderr));
    // CWD として使う別ディレクトリに、同名の evil `.so` を置く (`rel_path` は 888 を返す)。
    let evil_dir = t.0.join("evilcwd");
    fs::create_dir_all(&evil_dir).unwrap();
    let evil_c = evil_dir.join("rele.c");
    fs::write(&evil_c, "int rel_path(void) { return 888; }\n").unwrap();
    let evil_lib = evil_dir.join("librele.so");
    let o = cc(
        &["-shared", "-fPIC", "-O1", "-o", evil_lib.to_str().unwrap(), evil_c.to_str().unwrap()],
        &evil_dir,
    );
    assert!(o.status.success(), "evil .so のビルドに失敗: {}", text(&o.stderr));
    let src = "extern t.rel_path() -> i32 link \"./librele.so:rel_path\"\n\nfn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    let v = unsafe \"t\" \"相対 link はパッケージルートから解く（CWD ではない）\" {\n        t.rel_path()\n    }\n    core.check.eq(v, 777i32)\n    core.mem.drop(std.io.print_line(\"relative-link ok\"))\n    Ok(())\nend t.main\n";
    fs::write(t.0.join("src").join("t.ksnl"), src).unwrap();
    t.fmt();
    (t, evil_dir)
}

#[cfg(unix)]
#[test]
fn relative_link_resolves_to_package_root_not_cwd() {
    let (t, evil_dir) = relative_link_pkg();
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "check: {}{}", text(&c.stdout), text(&c.stderr));
    // パッケージルート (`kasane.toml` のあるディレクトリ) から一つ下の `evilcwd` を CWD にして実行。
    // `find_root` は CWD から遡って `kasane.toml` を見つけるので、パッケージは正しく読める。
    // 旧実装では `dlopen("./librele.so")` が CWD の evil `.so` (888) をロードする。
    // 新実装ではパッケージルートの正しい `.so` (777) をロードする。
    let i = run(&["run"], &evil_dir);
    let out = text(&i.stdout);
    let err = text(&i.stderr);
    assert_eq!(i.status.code(), Some(0), "interpreter run from evil CWD: {}{}", out, err);
    assert!(out.contains("relative-link ok"), "interpreter output: {}{}", out, err);
    assert!(!out.contains("888"), "evil .so が CWD からロードされた: {}{}", out, err);
    // ネイティブも同様（clang があれば）。
    if clang_available() {
        let b = Command::new(exe())
            .args(["build", "--backend", "llvm"])
            .current_dir(&t.0)
            .output()
            .unwrap();
        let outb = text(&b.stdout) + &text(&b.stderr);
        assert_eq!(b.status.code(), Some(0), "build: {}", outb);
        let exe = t.0.join("target").join("debug").join("t.native");
        assert!(exe.exists(), "native executable missing: {}", outb);
        let n = Command::new(&exe).current_dir(&evil_dir).output().unwrap();
        let outn = text(&n.stdout);
        let errn = text(&n.stderr);
        assert_eq!(n.status.code(), Some(0), "native run from evil CWD: {}{}", outn, errn);
        assert!(outn.contains("relative-link ok"), "native output: {}{}", outn, errn);
        assert!(!outn.contains("888"), "evil .so がネイティブでも CWD からロードされた: {}{}", outn, errn);
    }
}
