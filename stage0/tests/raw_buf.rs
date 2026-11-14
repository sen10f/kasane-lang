//! `alloc.raw.Buf<T, A>` (仕様 8.3、修正プラン 3-3) の実行テスト。
//! 木走査 (`ksn run --engine=tree`)、VM 指定 (`--engine=vm`。KIR に下ろせないときは木走査へ退避する)、
//! `ksn build` で作った実行体の出力が一致することを見る。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str, src: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_rawbuf_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), TOML).unwrap();
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

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(exe()).args(args).current_dir(cwd).output().expect("spawn ksn")
}

/// 3 つの経路で実行し、出力が同じことを確かめて返す。
fn run_all(name: &str, src: &str) -> String {
    let t = Tmp::new(name, src);
    let chk = run(&["check"], &t.0);
    assert!(text(&chk.stdout).trim().is_empty() && chk.status.success(), "{}{}", text(&chk.stdout), text(&chk.stderr));
    let tree = run(&["run", "--engine=tree"], &t.0);
    assert_eq!(tree.status.code(), Some(0), "{}{}", text(&tree.stdout), text(&tree.stderr));
    let vm = run(&["run", "--engine=vm"], &t.0);
    assert_eq!(vm.status.code(), Some(0), "{}{}", text(&vm.stdout), text(&vm.stderr));
    assert_eq!(text(&tree.stdout), text(&vm.stdout));
    let cache = std::env::temp_dir().join(format!("ksn_rawbuf_cache_{}_{}", std::process::id(), name));
    let b = Command::new(exe()).arg("build").current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    assert!(b.status.success(), "{}{}", text(&b.stdout), text(&b.stderr));
    let _ = fs::remove_dir_all(&cache);
    let launcher = t.0.join("target/debug").join("t");
    let native = t.0.join("target/debug").join("t.native");
    let p = if native.exists() { native } else { launcher };
    let o = Command::new(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&tree.stdout), text(&o.stdout), "build の出力が run と違う");
    text(&tree.stdout)
}

const PROG: &str = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    t.run()
end t.main

fn t.show(b: &alloc.raw.Buf<u32, std.alloc.System>) -> Result<(), std.io.Error> uses [io.console] panics may
    let s = b.as_slice()
    let mut i = 0usize
    let mut sum = 0u32
    while i < s.len() {
        match s.get(i) {
            Some(x) => {
                sum = sum.wrapping_add(*x)
            }
            None => {
            }
        }
        i = i.wrapping_add(1usize)
    }
    core.check.eq(sum, sum)
    std.io.print_line("show")
end t.show

fn t.run() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut b = match alloc.raw.Buf.with_capacity(std.alloc.SYSTEM, 3usize) {
        Ok(b) => b
        Err(_e) => core.panic.panic("oom")
    }
    core.check.eq(b.len(), 0usize)
    core.check.eq(b.cap(), 3usize)
    core.check.eq(b.pop(), None)
    core.check.eq(b.push_within_cap(10u32), Ok(()))
    core.check.eq(b.push_within_cap(20u32), Ok(()))
    core.check.eq(b.push_within_cap(30u32), Ok(()))
    core.check.eq(b.push_within_cap(40u32), Err(40u32))
    core.check.eq(b.insert_within_cap(0usize, 5u32), Err(5u32))
    core.check.eq(b.grow(8usize), Ok(()))
    core.check.eq(b.cap(), 8usize)
    core.check.eq(b.insert_within_cap(9usize, 5u32), Err(5u32))
    core.check.eq(b.insert_within_cap(0usize, 5u32), Ok(()))
    core.check.eq(b.len(), 4usize)
    core.check.eq(b.get(0usize), Some(&5u32))
    core.check.eq(b.get(4usize), None)
    match b.get_mut(1usize) {
        Some(x) => {
            *x = 11u32
        }
        None => core.panic.panic("get_mut")
    }
    core.check.eq(b.get(1usize), Some(&11u32))
    core.check.eq(b.remove(0usize), Some(5u32))
    core.check.eq(b.remove(7usize), None)
    core.check.eq(b.swap_remove(0usize), Some(11u32))
    core.check.eq(b.len(), 2usize)
    core.check.eq(b.get(0usize), Some(&30u32))
    t.show(&b)?
    b.truncate(1usize)
    core.check.eq(b.len(), 1usize)
    core.check.eq(b.pop(), Some(30u32))
    core.check.eq(b.is_empty(), true)
    core.check.eq(b.cap(), 8usize)
    std.io.print_line("done")?
    Ok(())
end t.run
"#;

const BYTES: &str = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut b = match alloc.raw.Buf.with_capacity(std.alloc.SYSTEM, 8usize) {
        Ok(b) => b
        Err(_e) => core.panic.panic("oom")
    }
    core.check.eq(b.append_str_within_cap("héllo"), Ok(()))
    core.check.eq(b.len(), 6usize)
    core.check.eq(b.append_str_within_cap("xyz"), Err(()))
    core.check.eq(b.len(), 6usize)
    match b.as_str() {
        Ok(s) => std.io.print_line(s)?
        Err(_e) => core.panic.panic("utf8")
    }
    core.check.eq(b.push_within_cap(255u8), Ok(()))
    match b.as_str() {
        Ok(_s) => core.panic.panic("should be invalid")
        Err(e) => core.check.eq(e.valid_up_to, 6usize)
    }
    Ok(())
end t.main
"#;

#[test]
fn buf_operations_agree_across_run_engines_and_build() {
    let out = run_all("ops", PROG);
    assert_eq!(out, "show\ndone\n");
}

#[test]
fn buf_u8_utf8_checked_view() {
    let out = run_all("bytes", BYTES);
    assert_eq!(out, "héllo\n");
}
