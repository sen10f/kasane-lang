//! `std.io.stdin()` / `stdout()` / `stderr()` と `Stdin.read` / `Stdout.write` / `Stdout.flush` / `Stderr.write` (G10)。
//! 木走査 (`ksn run`) と LLVM のネイティブで、標準入力を管で渡したときの出力・終了コードが一致することを確かめる。

mod common;
use common::*;
use std::io::Write;
use std::process::{Command, Output, Stdio};

const SRC: &str = "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never
    let mut input = std.io.stdin()
    let mut out = std.io.stdout()
    let mut err = std.io.stderr()
    let mut buf = [0u8; 8]
    let mut total = 0usize
    loop {
        let n = input.read(&mut buf)?
        if n == 0usize {
            break
        }
        total = total.wrapping_add(n)
        let head = out.write(&buf)?
        core.mem.drop(head)
    }
    out.flush()?
    let w = err.write(\"seen\".as_bytes())?
    core.mem.drop(w)
    err.flush()?
    if total == 20usize {
        std.io.print_line(\"|twenty\")?
    } else {
        std.io.print_line(\"|other\")?
    }
    Ok(())
end t.main
";

fn with_stdin(mut c: Command, input: &[u8]) -> Output {
    let mut ch = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn");
    ch.stdin.take().unwrap().write_all(input).unwrap();
    ch.wait_with_output().unwrap()
}

#[test]
fn stdin_stdout_stderr_interp_and_native_agree() {
    let t = Tmp::new("g10_streams", SRC);
    let chk = run(&["check"], &t.0);
    assert_eq!(chk.status.code(), Some(0), "{}{}", text(&chk.stdout), text(&chk.stderr));
    let input = b"0123456789abcdefghij";
    let mut c = Command::new(exe());
    c.arg("run").current_dir(&t.0);
    let r = with_stdin(c, input);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(text(&r.stderr), "seen");
    let o = text(&r.stdout);
    assert!(o.ends_with("|twenty\n") && o.starts_with("01234567"), "{}", o);

    let b = run(&["build", "--backend", "llvm"], &t.0);
    let bo = text(&b.stdout);
    assert_eq!(b.status.code(), Some(0), "{}{}", bo, text(&b.stderr));
    if !clang_available() {
        return;
    }
    assert!(!bo.contains("backend skipped"), "{}", bo);
    let exe_path = t.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t" });
    assert!(exe_path.exists(), "{}", bo);
    let mut c = Command::new(&exe_path);
    c.current_dir(&t.0);
    let n = with_stdin(c, input);
    same_output(&r, &n);
}
