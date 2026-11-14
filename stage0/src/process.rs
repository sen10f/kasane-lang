//! std.process (外部コマンドの実行) のインタプリタ実装。
//!
//! - `Command` は `{ handle: i32 }`。設定 (プログラム名・引数・環境・作業ディレクトリ・標準入出力) は `ProcTable::cmds` が持つ
//!   (KIR / LLVM のランタイム `sysrt.c` の表と同じ作り)。`spawn` / `output` / `cancel` が消費して表から外す。
//! - `Child` は `{ pid, stdin, stdout, stderr }` (パイプの記述子。ないものは -1)。パイプは `Interp::files` の記述子に入れ、
//!   読み書き・閉じるは `std.fs.File` と同じ表を使う。`wait` が子を回収する。
//! - 失敗は `Err(std.io.Error)` の値で返す (panic しない)。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::interp::Interp;
use crate::natives::{bytes_of, io_error};
use crate::value::*;

#[derive(Clone, Copy, PartialEq)]
pub enum Io {
    Inherit,
    Null,
    Piped,
}

pub struct Spec {
    program: String,
    args: Vec<String>,
    /// 環境の編集 (`env` = Some(値)、`env_remove` = None)。`clear` の後に順に適用する。
    env: Vec<(String, Option<String>)>,
    clear: bool,
    dir: Option<String>,
    io: [Io; 3],
}

#[derive(Default)]
pub struct ProcTable {
    cmds: HashMap<i128, Spec>,
    next: i128,
    children: HashMap<i128, std::process::Child>,
}

fn child_val(pid: i128, fds: [i128; 3]) -> Val {
    Val::Struct(Rc::new(StructV {
        ty: "std.process.Child".into(),
        f: RefCell::new(vec![
            ("pid".into(), int(pid, "i32")),
            ("stdin".into(), int(fds[0], "i32")),
            ("stdout".into(), int(fds[1], "i32")),
            ("stderr".into(), int(fds[2], "i32")),
        ]),
    }))
}

fn status_val(st: std::process::ExitStatus) -> Val {
    use std::os::unix::process::ExitStatusExt;
    let (code, sig) = match (st.code(), st.signal()) {
        (Some(c), _) => (c, 0),
        (None, s) => (-1, s.unwrap_or(0)),
    };
    Val::Struct(Rc::new(StructV {
        ty: "std.process.ExitStatus".into(),
        f: RefCell::new(vec![("code".into(), int(code as i128, "i32")), ("signal".into(), int(sig as i128, "i32"))]),
    }))
}

fn invalid() -> Val {
    err(io_error(&std::io::Error::from(std::io::ErrorKind::InvalidInput)))
}

fn field_int(s: &StructV, name: &str) -> i128 {
    if let Some(Val::Int(i, _)) = s.get(name) {
        i
    } else {
        -1
    }
}

impl Interp {
    /// `std.process.Command.new`。
    pub fn process_native(&mut self, name: &str, args: &[Val]) -> Res {
        match name {
            "std.process.Command.new" => {
                let program = str_of(&dr(args.first().unwrap_or(&Val::Unit))).unwrap_or_default();
                let id = self.procs.next;
                self.procs.next += 1;
                self.procs.cmds.insert(id, Spec { program, args: vec![], env: vec![], clear: false, dir: None, io: [Io::Inherit; 3] });
                Ok(Val::Struct(Rc::new(StructV { ty: "std.process.Command".into(), f: RefCell::new(vec![("handle".into(), int(id, "i32"))]) })))
            }
            _ => panic(format!("unknown native {}", name)),
        }
    }

    fn build(spec: &Spec) -> std::process::Command {
        let mut c = std::process::Command::new(&spec.program);
        c.args(&spec.args);
        if spec.clear {
            c.env_clear();
        }
        for (k, v) in &spec.env {
            match v {
                Some(v) => c.env(k, v),
                None => c.env_remove(k),
            };
        }
        if let Some(d) = &spec.dir {
            c.current_dir(d);
        }
        c
    }

    fn stdio(m: Io) -> std::process::Stdio {
        match m {
            Io::Inherit => std::process::Stdio::inherit(),
            Io::Null => std::process::Stdio::null(),
            Io::Piped => std::process::Stdio::piped(),
        }
    }

    fn add_file(&mut self, f: std::fs::File) -> i128 {
        let fd = self.next_fd;
        self.next_fd += 1;
        self.files.insert(fd, f);
        fd
    }

    /// `std.process.Command` / `Child` のメソッド。
    pub fn process_method(&mut self, s: &Rc<StructV>, name: &str, args: &[Val]) -> Res {
        use std::io::{Read, Write};
        if s.ty == "std.process.Command" {
            let id = field_int(s, "handle");
            let sarg = |i: usize| str_of(&dr(args.get(i).unwrap_or(&Val::Unit))).unwrap_or_default();
            match name {
                "spawn" | "output" => {
                    let Some(spec) = self.procs.cmds.remove(&id) else { return Ok(invalid()) };
                    let _ = std::io::Write::flush(&mut self.out);
                    let mut c = Self::build(&spec);
                    if name == "output" {
                        c.stdin(std::process::Stdio::null());
                        c.stdout(std::process::Stdio::piped());
                        c.stderr(std::process::Stdio::piped());
                        return Ok(match c.output() {
                            Ok(o) => {
                                for (i, data) in [&o.stdout, &o.stderr].into_iter().enumerate() {
                                    if let Some(target) = args.get(i) {
                                        if let Some(l) = crate::natives::item_list_pub(&dr(target)) {
                                            l.borrow_mut().extend(data.iter().map(|b| int(*b as i128, "u8")));
                                        }
                                    }
                                }
                                ok(status_val(o.status))
                            }
                            Err(e) => err(io_error(&e)),
                        });
                    }
                    c.stdin(Self::stdio(spec.io[0]));
                    c.stdout(Self::stdio(spec.io[1]));
                    c.stderr(Self::stdio(spec.io[2]));
                    return Ok(match c.spawn() {
                        Ok(mut ch) => {
                            let pid = ch.id() as i128;
                            let mut fds = [-1i128; 3];
                            if let Some(p) = ch.stdin.take() {
                                fds[0] = self.add_file(std::fs::File::from(std::os::fd::OwnedFd::from(p)));
                            }
                            if let Some(p) = ch.stdout.take() {
                                fds[1] = self.add_file(std::fs::File::from(std::os::fd::OwnedFd::from(p)));
                            }
                            if let Some(p) = ch.stderr.take() {
                                fds[2] = self.add_file(std::fs::File::from(std::os::fd::OwnedFd::from(p)));
                            }
                            self.procs.children.insert(pid, ch);
                            ok(child_val(pid, fds))
                        }
                        Err(e) => err(io_error(&e)),
                    });
                }
                "cancel" => {
                    self.procs.cmds.remove(&id);
                    return Ok(Val::Unit);
                }
                _ => {}
            }
            let Some(spec) = self.procs.cmds.get_mut(&id) else { return Ok(Val::Unit) };
            match name {
                "arg" => spec.args.push(sarg(0)),
                "env" => spec.env.push((sarg(0), Some(sarg(1)))),
                "env_remove" => spec.env.push((sarg(0), None)),
                "env_clear" => {
                    spec.clear = true;
                    spec.env.clear();
                }
                "current_dir" => spec.dir = Some(sarg(0)),
                "stdin_piped" => spec.io[0] = Io::Piped,
                "stdin_null" => spec.io[0] = Io::Null,
                "stdout_piped" => spec.io[1] = Io::Piped,
                "stdout_null" => spec.io[1] = Io::Null,
                "stderr_piped" => spec.io[2] = Io::Piped,
                "stderr_null" => spec.io[2] = Io::Null,
                _ => return panic(format!("no method {} on Command", name)),
            }
            return Ok(Val::Unit);
        }
        // Child
        let pid = field_int(s, "pid");
        let fd_of = |n: &str| field_int(s, n);
        match name {
            "id" => Ok(int(pid, "i32")),
            "kill" => Ok(match self.procs.children.get_mut(&pid) {
                Some(c) => match c.kill() {
                    Ok(()) => ok(Val::Unit),
                    Err(e) => err(io_error(&e)),
                },
                None => invalid(),
            }),
            "close_stdin" => {
                self.files.remove(&fd_of("stdin"));
                s.f.borrow_mut().iter_mut().for_each(|(k, v)| {
                    if k == "stdin" {
                        *v = int(-1, "i32");
                    }
                });
                Ok(ok(Val::Unit))
            }
            "write_stdin" => {
                let data = bytes_of(args.first().unwrap_or(&Val::Unit));
                Ok(match self.files.get_mut(&fd_of("stdin")) {
                    Some(f) => match f.write_all(&data) {
                        Ok(()) => ok(int(data.len() as i128, "usize")),
                        Err(e) => err(io_error(&e)),
                    },
                    None => invalid(),
                })
            }
            "read_stdout" | "read_stderr" => {
                let fd = fd_of(if name == "read_stdout" { "stdout" } else { "stderr" });
                let target = dr(args.first().unwrap_or(&Val::Unit));
                let mut buf = vec![0u8; seq_items(&target).map_or(0, |x| x.len())];
                let n = match self.files.get_mut(&fd) {
                    Some(f) => match f.read(&mut buf) {
                        Ok(n) => n,
                        Err(e) => return Ok(err(io_error(&e))),
                    },
                    None => return Ok(invalid()),
                };
                match &target {
                    Val::View(l, a, _) => {
                        let mut l = l.borrow_mut();
                        for (i, b) in buf[..n].iter().enumerate() {
                            l[*a + i] = int(*b as i128, "u8");
                        }
                    }
                    Val::Arr(l) | Val::Vec(l) => {
                        let mut l = l.borrow_mut();
                        for (i, b) in buf[..n].iter().enumerate() {
                            l[i] = int(*b as i128, "u8");
                        }
                    }
                    _ => {}
                }
                Ok(ok(int(n as i128, "usize")))
            }
            "wait" => {
                // 標準入力を閉じてから待つ (子が EOF を待っていても終われる)。残りのパイプは待った後に閉じる。
                self.files.remove(&fd_of("stdin"));
                let r = match self.procs.children.remove(&pid) {
                    Some(mut c) => match c.wait() {
                        Ok(st) => ok(status_val(st)),
                        Err(e) => err(io_error(&e)),
                    },
                    None => invalid(),
                };
                self.files.remove(&fd_of("stdout"));
                self.files.remove(&fd_of("stderr"));
                Ok(r)
            }
            _ => panic(format!("no method {} on Child", name)),
        }
    }
}
