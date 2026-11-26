//! 安定性レビュー S-04 / S-06 / S-11 の退行試験。

mod common;
use common::*;
use std::io::Read;
use std::process::{Command, Stdio};

const ARGS_SRC: &str = r#"fn t.main() -> () uses [alloc, env, io.console] panics may
    let mut i = 0usize
    while i < std.env.arg_count() {
        if let Some(a) = std.env.arg(i) {
            let mut l = std.string.String.new()
            l.push_str("arg: ").unwrap()
            l.push_str(a).unwrap()
            std.io.print_line(l.as_str()).unwrap()
        }
        i = i + 1usize
    }
end t.main
"#;

#[test]
fn engine_flag_after_double_dash_reaches_the_program() {
    let t = Tmp::new("stab_s04", ARGS_SRC);
    let o = run(&["run", "--", "a", "--engine=tree", "b", "--engine=x"], &t.0);
    let s = text(&o.stdout);
    assert!(s.contains("--engine=tree") && s.contains("--engine=x"), "{}\n{}", s, text(&o.stderr));
}

#[test]
fn sigterm_keeps_piped_stdout_lines() {
    let src = r#"fn t.main() -> () uses [alloc, io.console, thread, time] panics may
    std.io.print_line("started").unwrap()
    std.thread.sleep(30000u64)
end t.main
"#;
    let t = Tmp::new("stab_s11", src);
    for eng in ["--engine=tree", "--engine=vm"] {
        let mut c = Command::new(exe()).args(["run", eng]).current_dir(&t.0).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut out = c.stdout.take().unwrap();
        let mut buf = [0u8; 7];
        out.read_exact(&mut buf).expect("first line must arrive before exit");
        assert_eq!(&buf, b"started");
        let _ = Command::new("kill").arg(c.id().to_string()).status();
        let _ = c.wait();
    }
}

/// 手書きの `Drop` を持つパッケージも、VM のデバッガ (既定) では走る。`--engine=tree` のデバッガは `Drop` を走らせないので拒む。
#[test]
fn debug_runs_hand_written_drop_on_the_vm() {
    let src = r#"type t.D
    id: u64
end t.D

impl core.ops.Drop for t.D
fn t.D.drop(self: &mut t.D) -> () uses [] panics never impls core.ops.Drop
    let n = self.id
    core.mem.drop(n)
end t.D.drop

fn t.main() -> () uses [] panics never
    let a = t.D {
        id: 1u64
    }
    core.mem.drop(a.id)
end t.main
"#;
    let t = Tmp::new("stab_s06", src);
    let mut c = Command::new(exe()).args(["debug", "--json"]).current_dir(&t.0).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    use std::io::Write;
    c.stdin.take().unwrap().write_all(b"break t.D.drop\nrun\nbt\ncontinue\n").unwrap();
    let o = c.wait_with_output().unwrap();
    let out = text(&o.stdout);
    assert!(out.contains("\"reason\":\"function breakpoint\"") && out.contains("\"function\":\"t.D.drop\""), "{}\n{}", out, text(&o.stderr));
    assert_eq!(o.status.code(), Some(0), "{}", out);
    let o = run(&["debug", "--json", "--engine=tree"], &t.0);
    assert!(text(&o.stderr).contains("Drop"), "{}", text(&o.stderr));
    assert_ne!(o.status.code(), Some(0));
}
