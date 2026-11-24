//! `std.fs.Dir` (根つきのディレクトリ) と `open_file_beneath`・`File::metadata`・`read_to_end_limited` (B6)。
//! 根の外へ出る `..`・絶対パス・どの symlink も拒むこと、読みの上限を超えたら失敗すること、
//! 情報は開いた記述子から取ること (パスを引き直さない) を、木走査 (`ksn run`) と LLVM の debug のネイティブで確かめる。
//! 一時パッケージは `std::env::temp_dir()` の下に作り、解体する (他の試験のものには触れない)。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";

/// 試験のプログラム。`root/` の中で開くものと、根の外の `outside.txt` へのリンクを試す。
/// 結果は `ラベル: 結果` の 1 行ずつ。linear の値 (File・Dir) は必ず close する。
const PROGRAM: &str = r#"fn t.say(label: &str, text: &str) -> () uses [alloc, io.console] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(label))
    core.mem.drop(s.push_str(": "))
    core.mem.drop(s.push_str(text))
    core.mem.drop(std.io.print_line(s.as_str()))
end t.say

fn t.num(n: usize) -> i64 uses [] panics never
    match i64.try_from_usize(n) {
        Ok(v) => v
        Err(_e) => -1i64
    }
end t.num

fn t.open_case(label: &str, root: &std.fs.Dir, rel: &str) -> () uses [alloc, io.console, io.fs] panics never
    match root.open_file_beneath(rel) {
        Ok(f) => {
            t.say(label, "opened")
            core.mem.drop(f.close())
        }
        Err(e) => {
            if e.kind == std.io.ErrorKind.InvalidInput {
                t.say(label, "refused: invalid")
            } else {
                t.say(label, "refused: os")
            }
        }
    }
end t.open_case

fn t.read_limit(label: &str, root: &std.fs.Dir, rel: &str, max: usize) -> () uses [alloc, io.console, io.fs] panics never
    match root.open_file_beneath(rel) {
        Ok(f) => {
            let mut file = f
            match file.read_to_end_limited(max) {
                Ok(_v) => t.say(label, "read")
                Err(_e) => t.say(label, "refused")
            }
            core.mem.drop(file.close())
        }
        Err(_e) => t.say(label, "no open")
    }
end t.read_limit

fn t.meta_case(root: &std.fs.Dir) -> () uses [alloc, io.console, io.fs] panics never
    match root.open_file_beneath("inside.txt") {
        Ok(f) => {
            match f.metadata() {
                Ok(md) => {
                    if md.size == 5u64 {
                        t.say("meta size", "5")
                    } else {
                        t.say("meta size", "other")
                    }
                    if md.kind == std.fs.FileKind.File {
                        t.say("meta kind", "file")
                    } else {
                        t.say("meta kind", "other")
                    }
                }
                Err(_e) => t.say("meta", "failed")
            }
            core.mem.drop(f.close())
        }
        Err(_e) => t.say("meta", "no open")
    }
end t.meta_case

fn t.grow_case(root: &std.fs.Dir) -> () uses [alloc, io.console, io.fs] panics never
    match root.open_file_beneath("grow.txt") {
        Ok(f) => {
            let mut file = f
            match std.fs.open_append("root/grow.txt") {
                Ok(app) => {
                    let mut w = app
                    match w.write(b"defghijk") {
                        Ok(_n) => {
                        }
                        Err(_e) => t.say("grow write", "failed")
                    }
                    core.mem.drop(w.close())
                }
                Err(_e) => t.say("grow append", "failed")
            }
            match file.read_to_end_limited(5usize) {
                Ok(_v) => t.say("grow", "read")
                Err(_e) => t.say("grow limit", "refused")
            }
            core.mem.drop(file.close())
        }
        Err(_e) => t.say("grow", "no open")
    }
end t.grow_case

fn t.run_cases(root: &std.fs.Dir) -> () uses [alloc, io.console, io.fs] panics never
    t.open_case("inside", root, "inside.txt")
    t.open_case("nested", root, "sub/deep.txt")
    t.open_case("dotdot", root, "../outside.txt")
    t.open_case("dotdot middle", root, "sub/../inside.txt")
    t.open_case("absolute", root, "/etc/passwd")
    t.open_case("symlink file out", root, "link_out")
    t.open_case("symlink file in", root, "link_in")
    t.open_case("symlink dir", root, "sub_link/deep.txt")
    t.open_case("missing", root, "nope.txt")
    t.extra_cases(root)
end t.run_cases

fn t.extra_cases(root: &std.fs.Dir) -> () uses [alloc, io.console, io.fs] panics never
    t.read_limit("read exact", root, "inside.txt", 5usize)
    t.read_limit("read short", root, "inside.txt", 4usize)
    t.read_limit("read nested", root, "sub/deep.txt", 100usize)
    t.meta_case(root)
    t.grow_case(root)
end t.extra_cases

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, io.fs] panics never
    match std.fs.Dir.open("root") {
        Ok(root) => {
            t.run_cases(&root)
            match std.fs.Dir.open("root/inside.txt") {
                Ok(d) => {
                    t.say("dir of file", "opened")
                    core.mem.drop(d.close())
                }
                Err(_e) => t.say("dir of file", "refused")
            }
            core.mem.drop(root.close())
        }
        Err(_e) => t.say("open root", "failed")
    }
    Ok(())
end t.main
"#;

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(exe()).args(args).current_dir(cwd).output().expect("spawn ksn")
}

/// 一時パッケージ。`root/` (根) と、根の外の `outside.txt`、根の中の通常ファイル・深いファイル・
/// 根の外へのリンク・根の中のファイルへのリンク・根の中のディレクトリへのリンクを作る。
struct Pkg(PathBuf);

impl Pkg {
    fn new(name: &str, program: &str) -> Pkg {
        let d = std::env::temp_dir().join(format!("ksn_fsrooted_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), TOML).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(d.join("src").join("t.ksnl"), program).unwrap();
        let _ = run(&["fmt"], &d);
        fs::write(d.join("outside.txt"), "secret").unwrap();
        fs::create_dir_all(d.join("root").join("sub")).unwrap();
        fs::write(d.join("root").join("inside.txt"), "hello").unwrap();
        fs::write(d.join("root").join("sub").join("deep.txt"), "deep").unwrap();
        fs::write(d.join("root").join("grow.txt"), "xyz").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink("../outside.txt", d.join("root").join("link_out")).unwrap();
            symlink("inside.txt", d.join("root").join("link_in")).unwrap();
            symlink("sub", d.join("root").join("sub_link")).unwrap();
        }
        Pkg(d)
    }
}

impl Drop for Pkg {
    fn drop(&mut self) {
        if !std::thread::panicking() && std::env::var("KSN_KEEP_TMP").map(|v| v != "1").unwrap_or(true) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

/// 木走査 (`ksn run`) の出力。
fn interp_output(p: &Pkg) -> String {
    let o = run(&["run"], &p.0);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    text(&o.stdout)
}

/// LLVM の debug のネイティブの出力。退避 (バンドル) は失敗とみなす。
fn native_output(p: &Pkg) -> String {
    let cache = std::env::temp_dir().join(format!("ksn_fsrooted_cache_{}", std::process::id()));
    let o = Command::new(exe()).arg("build").current_dir(&p.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let log = text(&o.stdout) + &text(&o.stderr);
    let _ = fs::remove_dir_all(&cache);
    let bin = p.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t" });
    assert!(std::fs::read_to_string(&bin.parent().unwrap().join(".ksn-artifact")).map_or(false, |m| m.trim() == "native"), "ネイティブが作れなかった (退避は許さない): {}", log);
    let o = Command::new(&bin).current_dir(&p.0).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    text(&o.stdout)
}

/// 木走査だけで試す全部 (読みの上限・情報・伸びるファイルを含む)。
#[test]
fn fs_rooted_refuses_escapes_and_symlinks_and_checks_limits_in_the_interpreter() {
    if !cfg!(unix) {
        return;
    }
    let p = Pkg::new("interp", PROGRAM);
    let interp = interp_output(&p);
    let has = |l: &str| assert!(interp.lines().any(|x| x == l), "出力に `{}` がない:\n{}", l, interp);
    // 根の中の通常のファイルと入れ子は開ける
    has("inside: opened");
    has("nested: opened");
    // `..`・途中の `..`・絶対パスは形で拒む (InvalidInput)
    has("dotdot: refused: invalid");
    has("dotdot middle: refused: invalid");
    has("absolute: refused: invalid");
    // symlink は、根の外へ向くものも、根の中のファイルへ向くものも、ディレクトリ経由も拒む (ELOOP)
    has("symlink file out: refused: os");
    has("symlink file in: refused: os");
    has("symlink dir: refused: os");
    has("missing: refused: os");
    // ディレクトリでないものを Dir にしない
    has("dir of file: refused");
    // 読みの上限: ちょうど 5 バイトは読める、4 バイトの上限は超える、入れ子は読める
    has("read exact: read");
    has("read short: refused");
    has("read nested: read");
    // 情報は開いた記述子から
    has("meta size: 5");
    has("meta kind: file");
    // 開いた後に伸びたファイルは上限で止まる (3 + 8 = 11 バイト > 5)
    has("grow limit: refused");
}

/// 木走査と LLVM の debug のネイティブが、全件 (読みの上限・情報を含む) で同じ出力になること。
#[test]
fn fs_rooted_open_cases_agree_with_the_native_build() {
    if !cfg!(unix) {
        return;
    }
    let p = Pkg::new("native", PROGRAM);
    let interp = interp_output(&p);
    let native = native_output(&p);
    assert_eq!(native, interp, "debug のネイティブと木走査の出力が違う");
    let has = |l: &str| assert!(interp.lines().any(|x| x == l), "出力に `{}` がない:\n{}", l, interp);
    has("inside: opened");
    has("nested: opened");
    has("dotdot: refused: invalid");
    has("absolute: refused: invalid");
    has("symlink file out: refused: os");
    has("symlink file in: refused: os");
    has("symlink dir: refused: os");
    has("dir of file: refused");
    has("read exact: read");
    has("read short: refused");
    has("meta size: 5");
    has("meta kind: file");
    has("grow limit: refused");
    let c = Command::new(exe()).args(["check", "--native"]).current_dir(&p.0).output().unwrap();
    let log = text(&c.stdout) + &text(&c.stderr);
    assert!(!log.contains("W9040"), "ネイティブの警告: {}", log);
}
