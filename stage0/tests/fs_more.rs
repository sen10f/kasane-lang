//! `std.fs` の追加分 (追記・`rename`・`write_atomic`・ディレクトリ・`read_dir`・`metadata`) と `std.path` の検査。
//! 木走査 (`ksn run`) と、LLVM のネイティブ (`ksn build` の debug / release) の出力が一致することを確かめる。
//! 実行は一時パッケージの中 (相対パス `d/...`) で行うので、ほかのテストと干渉しない。

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(exe()).args(args).current_dir(cwd).output().expect("spawn ksn")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

/// 一時パッケージ `t` (解体時に消す)。
struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str, src: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_fsmore_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), TOML).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(d.join("src").join("t.ksnl"), src).unwrap();
        // 書き方は 1 通り (E0020): 試験の入力は `ksn fmt` の出力にそろえる。
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_ksn")).arg("fmt").current_dir(&d).output();
        // 効果の宣言は本体の効果と完全に一致させる (D5・D13): 固定の見出し (`main_wrap`) などを直す。
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_ksn")).arg("fix").env("KSN_FIX_ONLY", "E4020,E4025").current_dir(&d).output();
        Tmp(d)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        scratch::remove(&self.0);
    }
}

/// `ksn build` (debug か release) でネイティブを作って走らせ、標準出力を返す。バンドルへの退避は失敗とみなす。
fn native_output(name: &str, src: &str, release: bool) -> String {
    let t = Tmp::new(name, src);
    let cache = scratch::Scratch::at(format!("ksn_cache_fsmore_{}_{}", std::process::id(), name));
    let mut c = Command::new(exe());
    c.arg("build").current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
    if release {
        c.arg("--release");
    }
    let o = c.output().unwrap();
    let log = text(&o.stdout) + &text(&o.stderr);
    let _ = fs::remove_dir_all(&cache);
    let kind = if release { "release" } else { "debug" };
    let p = t.0.join("target").join(kind).join(if cfg!(windows) { "t.exe" } else { "t.native" });
    assert!(p.exists(), "ネイティブが作れなかった (退避は許さない): {}", log);
    let o = Command::new(&p).current_dir(&t.0).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    text(&o.stdout)
}

fn interp_output(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    text(&o.stdout)
}

/// 全関数を 1 本のプログラムで走らせ、木走査と LLVM (debug / release) の出力が同じで、期待どおりであることを確かめる。
#[test]
fn std_fs_more_agrees_between_interpreter_and_the_native_build() {
    if !cfg!(unix) {
        return;
    }
    let src = include_str!("data/fs_more.ksnl");
    let interp = interp_output("fsm_interp", src);
    assert_eq!(native_output("fsm_debug", src, false), interp, "debug のネイティブと木走査の出力が違う");
    assert_eq!(native_output("fsm_release", src, true), interp, "release のネイティブと木走査の出力が違う");
    let has = |l: &str| assert!(interp.lines().any(|x| x == l), "出力に `{}` がない:\n{}", l, interp);
    // create_dir / create_dir_all / remove_dir (EEXIST = 17, ENOENT = 2, ENOTEMPTY = 39)
    has("create_dir twice: kind 3 code 17");
    has("create_dir without parent: kind 1 code 2");
    has("deep dir exists: 1");
    has("remove_dir not empty: kind 9 code 39");
    has("remove_dir missing: kind 1 code 2");
    // open_append
    has("append 1: 2");
    has("log length: 4");
    has("log first: 97");
    has("log last: 100");
    // write_atomic (一時ファイルは残らない)
    has("atomic length 1: 5");
    has("atomic length 2: 3");
    has("atomic first: 98");
    has("temporary file left behind: 0");
    has("write_atomic into a missing directory: kind 1 code 2");
    // rename
    has("old name exists: 0");
    has("new name exists: 1");
    has("rename missing: kind 1 code 2");
    // metadata (File = 1, Dir = 2)
    has("moved size: 3");
    has("moved kind: 1");
    has("moved modified: 1");
    has("dir kind: 2");
    has("metadata missing: kind 1 code 2");
    // read_dir は名前のバイト順 (大文字が先)
    let order: Vec<&str> = interp.lines().filter(|l| l.starts_with("entry ")).collect();
    assert_eq!(order, ["entry Zed kind 1", "entry a kind 2", "entry abc kind 1", "entry log.txt kind 1", "entry moved.txt kind 1"]);
    has("entries: 5");
    has("empty dir entries: 0");
    has("read_dir missing: kind 1 code 2");
    has("read_dir of a file: kind 9 code 20");
    // 4096 バイトを超える列挙 (容量を増やして呼び直す)
    has("big entries: 400");
    has("big first: file_with_a_longer_name_0");
    has("big last: file_with_a_longer_name_99");
    // std.path
    has("parent a/b/c: a/b");
    has("parent /a: /");
    has("parent /: <none>");
    has("parent a: ");
    has("parent a//b/: a");
    has("parent empty: <none>");
    has("name a/b.tar.gz: b.tar.gz");
    has("name a/b/: b");
    has("name /: <none>");
    has("name ..: <none>");
    has("ext a/b.tar.gz: gz");
    has("ext .bashrc: <none>");
    has("ext noext: <none>");
    has("ext trailing dot: ");
    has("stem a/b.tar.gz: b.tar");
    has("stem .bashrc: .bashrc");
    has("absolute /x: 1");
    has("absolute x: 0");
    has("join a b: a/b");
    has("join a/ b: a/b");
    has("join a /b: /b");
    has("join empty b: b");
    assert!(interp.ends_with("fs more ok\n"), "{}", interp);
}

/// 新しい命令は KIR に下りて検証を通る (LLVM への下ろしは上のネイティブのビルドが確かめる)。
#[test]
fn std_fs_more_lowers_to_kir() {
    let src = include_str!("data/fs_more.ksnl");
    let t = Tmp::new("fsm_kir", src);
    let mut all = String::new();
    for f in ["t.files", "t.dirs", "std.fs.read_dir"] {
        let o = run(&["kir", f], &t.0);
        let out = text(&o.stdout) + &text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{}: {}", f, out);
        all.push_str(&out);
    }
    for op in ["fs.rename", "fs.create_dir", "fs.remove_dir", "fs.open_append", "fs.stat", "fs.read_dir"] {
        assert!(all.contains(op), "KIR に {} がない", op);
    }
}
