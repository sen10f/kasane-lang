//! バンドル (`target/<kind>/pkg/`) に、パッケージ内の `target/` やキャッシュ
//! ディレクトリ (`KSN_CACHE_DIR` がパッケージ内を指すとき) をコピーしないこと、
//! そしてバンドルが動くことを確かめる。

mod common;
use common::*;

use std::fs;
use std::process::Command;

const PROG: &str = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never
    core.mem.drop(std.io.print_line(\"bundle ok\"))
    Ok(())
end t.main
";

fn make_pkg() -> Tmp {
    let t = Tmp::new("bundle_copy", PROG);
    let cache = t.0.join(".cache");
    fs::create_dir_all(&cache).unwrap();
    fs::write(cache.join("dummy"), "cached bytes").unwrap();
    // ビルド残骸が `target/` にあってもバンドルに混ぜない。
    fs::create_dir_all(t.0.join("target").join("debug")).unwrap();
    fs::write(t.0.join("target").join("debug").join("stale.bin"), "stale").unwrap();
    t
}

/// `KSN_CACHE_DIR` がパッケージの中の `.` で始まらない名前 (`cache/` など) を指すとき、
/// その最上位の名前もバンドルから除いて入れ子に膨らませない。
fn make_pkg_nondot_cache() -> Tmp {
    let t = Tmp::new("bundle_copy_nondot", PROG);
    let cache = t.0.join("cache");
    fs::create_dir_all(&cache).unwrap();
    // キャッシュの置き場は自分だけが書ける (0700) ことが求められる。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(cache.join("dummy"), "cached bytes").unwrap();
    fs::create_dir_all(t.0.join("target").join("debug")).unwrap();
    fs::write(t.0.join("target").join("debug").join("stale.bin"), "stale").unwrap();
    t
}

#[test]
fn bundle_does_not_copy_cache_or_target() {
    let t = make_pkg();
    let cache = t.0.join(".cache");
    // 退避する (デバッグビルドは常にインタプリタ同梱のバンドルへ退避する) プログラムを
    // 2 回ビルドしても、入れ子に膨らませない。
    for _ in 0..2usize {
        let o = run_with_env(&["build"], &t.0, &[("KSN_CACHE_DIR", cache.to_str().unwrap())]);
        let out = text(&o.stdout) + &text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "build must succeed: {out}");
    }
    let pkg = t.0.join("target").join("debug").join("pkg");
    assert!(!pkg.join(".cache").exists(), "キャッシュがバンドルに含まれている: {pkg:?}");
    assert!(!pkg.join("target").exists(), "target がバンドルに含まれている: {pkg:?}");
    // バンドルが動く。
    let launcher =
        t.0.join("target").join("debug").join(if cfg!(windows) { "t.cmd" } else { "t" });
    assert!(launcher.exists(), "バンドルの起動スクリプトがない: {launcher:?}");
    let o = Command::new(&launcher).output().expect("run bundle");
    assert_eq!(o.status.code(), Some(0), "bundle must run: {}", text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "bundle ok");
}

#[test]
fn bundle_does_not_copy_nondot_cache_dir() {
    let t = make_pkg_nondot_cache();
    let cache = t.0.join("cache");
    for _ in 0..2usize {
        let o = run_with_env(&["build"], &t.0, &[("KSN_CACHE_DIR", cache.to_str().unwrap())]);
        let out = text(&o.stdout) + &text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "build must succeed: {out}");
    }
    let pkg = t.0.join("target").join("debug").join("pkg");
    assert!(!pkg.join("cache").exists(), "キャッシュがバンドルに含まれている: {pkg:?}");
    assert!(!pkg.join("target").exists(), "target がバンドルに含まれている: {pkg:?}");
    // バンドルが動く。
    let launcher =
        t.0.join("target").join("debug").join(if cfg!(windows) { "t.cmd" } else { "t" });
    assert!(launcher.exists(), "バンドルの起動スクリプトがない: {launcher:?}");
    let o = Command::new(&launcher).output().expect("run bundle");
    assert_eq!(o.status.code(), Some(0), "bundle must run: {}", text(&o.stderr));
    assert_eq!(text(&o.stdout).trim(), "bundle ok");
}
