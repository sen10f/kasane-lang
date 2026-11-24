//! `std.process` (外部コマンドの実行) の試験。`ksn run` (木走査) と `ksn build` (LLVM ネイティブ) の出力が一致すること。
//! `/bin/echo`・`/bin/cat`・`/bin/sh` を使う (POSIX のみ)。

#![cfg(unix)]

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

struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str, src: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_test_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), TOML).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(d.join("src").join("t.ksnl"), src).unwrap();
        // 書き方は 1 通り (E0020): 試験の入力は `ksn fmt` の出力にそろえる。
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_ksn")).arg("fmt").current_dir(&d).output();
        // 効果の宣言は本体の効果と完全に一致させる (D5・D13): 固定の見出し (`main_wrap`) などを直す。
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_ksn")).arg("fix").env("KSN_FIX_ONLY", "E4020,E4025,E2055,E2057,E2058,E2059,E2060,E2061,E2062,E2063,E2064,E2065,E2066,E2067,E2068,E2069,E2070,E2071,E2072,E2073").current_dir(&d).output();
        Tmp(d)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        scratch::remove(&self.0);
    }
}

/// `src` を `ksn run` と、`ksn build` したネイティブで走らせ、標準出力が `want` と一致すること。
/// ネイティブが作れず理由付きでバンドルに退避したときは、バンドルの起動スクリプトで確かめる。
fn both(name: &str, src: &str, want: &str) {
    let t = Tmp::new(name, src);
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{name} run: {}\n{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), want, "{name} run output");
    let b = run(&["build", "--backend", "llvm"], &t.0);
    let bout = text(&b.stdout) + &text(&b.stderr);
    assert_eq!(b.status.code(), Some(0), "{name} build: {bout}");
    let native = t.0.join("target").join("debug").join("t");
    let prog = if native.exists() {
        native
    } else {
        assert!(bout.contains("skipped"), "{name}: ネイティブもバンドルの退避の表示もない: {bout}");
        t.0.join("target").join("debug").join("t")
    };
    let n = Command::new(&prog).current_dir(&t.0).output().expect("spawn native");
    assert_eq!(n.status.code(), Some(0), "{name} native: {}\n{}", text(&n.stdout), text(&n.stderr));
    assert_eq!(text(&n.stdout), want, "{name} native output");
}

#[test]
fn echo_output_is_captured() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, process] panics may
    let mut c = std.process.Command.new("/bin/echo")
    c.arg("hello")
    c.arg("world")
    let mut out = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut er = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let st = c.output(&mut out, &mut er)?
    core.check.eq(st.code, 0i32)
    core.check.eq(st.signal, 0i32)
    core.check.eq(out.len(), 12usize)
    core.check.eq(er.len(), 0usize)
    core.mem.drop(std.io.print_line("echo ok"))
    Ok(())
end t.main
"#;
    both("proc_echo", src, "echo ok\n");
}

#[test]
fn shell_exit_code_stderr_env_and_cwd() {
    let src = r#"fn t.run_sh(script: &str, key: &str, value: &str, dir: &str) -> Result<(i32, usize, usize), std.io.Error> uses [alloc, process] panics never
    let mut c = std.process.Command.new("/bin/sh")
    c.arg("-c")
    c.arg(script)
    c.env(key, value)
    c.env("KSN_REMOVED", "x")
    c.env_remove("KSN_REMOVED")
    c.current_dir(dir)
    let mut out = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut er = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let st = c.output(&mut out, &mut er)?
    Ok((st.code, out.len(), er.len()))
end t.run_sh

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, process] panics may
    let (a, ao, ae) = t.run_sh("printf abc; printf xy >&2; exit 3", "KSN_A", "1", "/")?
    core.check.eq(a, 3i32)
    core.check.eq(ao, 3usize)
    core.check.eq(ae, 2usize)
    let (b, bo, be) = t.run_sh("printf \"$KSN_A\"; printf \"[$KSN_REMOVED]\"; pwd", "KSN_A", "valuevalue", "/")?
    core.check.eq(b, 0i32)
    core.check.eq(bo, 14usize)
    core.check.eq(be, 0usize)
    core.mem.drop(std.io.print_line("sh ok"))
    Ok(())
end t.main
"#;
    // 2 回目の標準出力: "valuevalue" (10) + "[]" (2) + "/\n" (2) = 14
    both("proc_sh", src, "sh ok\n");
}

#[test]
fn pipes_feed_cat_through_stdin_and_stdout() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console, process] panics may
    let mut c = std.process.Command.new("/bin/cat")
    c.stdin_piped()
    c.stdout_piped()
    let mut child = c.spawn()?
    let n = match child.write_stdin(b"hello, pipe") {
        Ok(n) => n
        Err(e) => {
            core.mem.drop(child.wait())
            return Err(e)
        }
    }
    core.check.eq(n, 11usize)
    match child.close_stdin() {
        Ok(()) => {
        }
        Err(e) => {
            core.mem.drop(child.wait())
            return Err(e)
        }
    }
    let mut total = 0usize
    let mut buf = [0u8; 4]
    loop {
        match child.read_stdout(&mut buf) {
            Ok(k) => {
                if k == 0usize {
                    break
                }
                total = total + k
            }
            Err(e) => {
                core.mem.drop(child.wait())
                return Err(e)
            }
        }
    }
    core.check.eq(total, 11usize)
    let st = child.wait()?
    core.check.eq(st.code, 0i32)
    core.mem.drop(std.io.print_line("pipes ok"))
    Ok(())
end t.main
"#;
    both("proc_pipes", src, "pipes ok\n");
}

#[test]
fn wait_reports_exit_code_and_signal() {
    let src = r#"fn t.status(script: &str) -> Result<std.process.ExitStatus, std.io.Error> uses [process] panics never
    let mut c = std.process.Command.new("/bin/sh")
    c.arg("-c")
    c.arg(script)
    c.stdout_null()
    c.stderr_null()
    let child = c.spawn()?
    child.wait()
end t.status

fn t.main() -> Result<(), std.io.Error> uses [io.console, process] panics may
    let a = t.status("exit 7")?
    core.check.eq(a.code, 7i32)
    core.check.eq(a.signal, 0i32)
    let b = t.status("kill -9 $$")?
    core.check.eq(b.code, -1i32)
    core.check.eq(b.signal, 9i32)
    core.mem.drop(std.io.print_line("status ok"))
    Ok(())
end t.main
"#;
    both("proc_status", src, "status ok\n");
}

#[test]
fn missing_program_is_an_error_value() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, process] panics may
    let c = std.process.Command.new("/no/such/ksn-program")
    core.mem.drop(match c.spawn() {
        Ok(child) => {
            core.mem.drop(child.wait())
            std.io.print_line("spawned")
        }
        Err(e) => {
            core.check.eq(e.kind, std.io.ErrorKind.NotFound)
            std.io.print_line("not found")
        }
    })
    let mut d = std.process.Command.new("/no/such/ksn-program")
    let mut out = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut er = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(match d.output(&mut out, &mut er) {
        Ok(_st) => std.io.print_line("ran")
        Err(e) => {
            core.check.eq(e.kind, std.io.ErrorKind.NotFound)
            std.io.print_line("output: not found")
        }
    })
    let mut w = std.process.Command.new("/bin/echo")
    w.current_dir("/no/such/ksn-dir")
    core.mem.drop(match w.spawn() {
        Ok(child) => {
            core.mem.drop(child.wait())
            std.io.print_line("spawned")
        }
        Err(e) => {
            core.check.eq(e.kind, std.io.ErrorKind.NotFound)
            std.io.print_line("dir: not found")
        }
    })
    Ok(())
end t.main
"#;
    both("proc_missing", src, "not found\noutput: not found\ndir: not found\n");
}

#[test]
fn inherited_stdout_reaches_the_parent_in_order() {
    let src = r#"fn t.main() -> Result<(), std.io.Error> uses [io.console, process] panics may
    core.mem.drop(std.io.print_line("before"))
    let mut c = std.process.Command.new("/bin/echo")
    c.arg("child")
    let child = c.spawn()?
    let st = child.wait()?
    core.check.eq(st.code, 0i32)
    core.mem.drop(std.io.print_line("after"))
    Ok(())
end t.main
"#;
    both("proc_inherit", src, "before\nchild\nafter\n");
}


#[test]
fn cancel_discards_a_command_and_threads_spawn_concurrently() {
    // 実スレッド (ネイティブ) と併用しても、設定表と起動が安全なこと。4 本のスレッドがそれぞれ 20 回 `/bin/echo` を起動して出力を数える。
    let src = r#"fn t.job(n: u32) -> u64 uses [alloc, process] panics may
    let mut sum = 0u64
    let mut i = 0u32
    while i < n {
        let mut d = std.process.Command.new("/bin/echo")
        d.arg("xy")
        let mut out = alloc.vec.Vec.new(std.alloc.SYSTEM)
        let mut er = alloc.vec.Vec.new(std.alloc.SYSTEM)
        core.mem.drop(match d.output(&mut out, &mut er) {
            Ok(st) => {
                if st.code == 0i32 {
                    sum = sum + 1u64
                }
            }
            Err(_e) => {
            }
        })
        if out.len() == 3usize {
            sum = sum + 100u64
        }
        i = i + 1u32
    }
    sum
end t.job

fn t.spawn_job() -> std.thread.JoinHandle<u64, std.alloc.System> uses [alloc, process, thread] panics may
    match std.thread.spawn(std.alloc.SYSTEM, fn() -> u64 capture [] {
        t.job(20u32)
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
end t.spawn_job

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, process, thread] panics may
    let mut c = std.process.Command.new("/bin/echo")
    c.arg("never runs")
    c.cancel()
    let a = t.spawn_job()
    let b = t.spawn_job()
    let c2 = t.spawn_job()
    let d = t.spawn_job()
    let total = a.join() + b.join() + c2.join() + d.join()
    core.check.eq(total, 8080u64)
    core.mem.drop(std.io.print_line("threads ok"))
    Ok(())
end t.main
"#;
    both("proc_threads", src, "threads ok\n");
}
