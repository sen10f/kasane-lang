//! std.net (TCP) のインタプリタ実装。std.fs と同じく記述子表 (記述子 -> ソケット) を持つ。
//! 失敗は panic ではなく `Err(std.net.Error)` / `Err(std.io.Error)` の値で返す。

use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;

use crate::interp::Interp;
use crate::natives::{bytes_of, io_error};
use crate::value::*;

/// 記述子表。リスナとストリームは同じ番号空間 (`Interp::next_fd`) を使う。
#[derive(Default)]
pub struct NetTable {
    pub listeners: std::collections::HashMap<i128, TcpListener>,
    pub streams: std::collections::HashMap<i128, TcpStream>,
}

fn net_error(e: &std::io::Error) -> Val {
    net_error_code(e.raw_os_error().unwrap_or(-1) as i128)
}

fn net_error_code(code: i128) -> Val {
    Val::Struct(Rc::new(StructV { ty: "std.net.Error".into(), f: RefCell::new(vec![("code".into(), int(code, "i32"))]) }))
}

fn handle(ty: &str, fd: i128) -> Val {
    Val::Struct(Rc::new(StructV { ty: ty.into(), f: RefCell::new(vec![("descriptor".into(), int(fd, "i32"))]) }))
}

impl Interp {
    fn alloc_fd(&mut self) -> i128 {
        let fd = self.next_fd;
        self.next_fd += 1;
        fd
    }

    /// `std.net.connect` / `std.net.listen`。
    pub fn net_native(&mut self, name: &str, args: &[Val]) -> Res {
        let addr = str_of(&dr(args.first().unwrap_or(&Val::Unit))).unwrap_or_default();
        match name {
            "std.net.connect" => Ok(match TcpStream::connect(addr.as_str()) {
                Ok(s) => {
                    let fd = self.alloc_fd();
                    self.net.streams.insert(fd, s);
                    ok(handle("std.net.TcpStream", fd))
                }
                Err(e) => err(net_error(&e)),
            }),
            "std.net.listen" => Ok(match TcpListener::bind(addr.as_str()) {
                Ok(l) => {
                    let fd = self.alloc_fd();
                    self.net.listeners.insert(fd, l);
                    ok(handle("std.net.TcpListener", fd))
                }
                Err(e) => err(net_error(&e)),
            }),
            _ => panic(format!("unknown native {}", name)),
        }
    }

    /// `std.net.TcpListener` / `std.net.TcpStream` のメソッド。
    pub fn net_method(&mut self, s: &Rc<StructV>, name: &str, args: &[Val]) -> Res {
        let Some(Val::Int(fd, _)) = s.get("descriptor") else { return panic("invalid net handle") };
        if s.ty == "std.net.TcpListener" {
            return Ok(match name {
                "accept" => match self.net.listeners.get(&fd) {
                    Some(l) => match l.accept() {
                        Ok((st, _)) => {
                            let nfd = self.alloc_fd();
                            self.net.streams.insert(nfd, st);
                            ok(handle("std.net.TcpStream", nfd))
                        }
                        Err(e) => err(net_error(&e)),
                    },
                    None => err(net_error_code(-1)),
                },
                "local_port" => match self.net.listeners.get(&fd).and_then(|l| l.local_addr().ok()) {
                    Some(a) => ok(int(a.port() as i128, "u16")),
                    None => err(net_error_code(-1)),
                },
                "close" => {
                    self.net.listeners.remove(&fd);
                    ok(Val::Unit)
                }
                _ => return panic(format!("no method {} on TcpListener", name)),
            });
        }
        let invalid = || err(io_error(&std::io::Error::from(std::io::ErrorKind::InvalidInput)));
        match name {
            "close" => {
                // drop で FIN を送る。二重 close は線形型が防ぐので、未登録でも成功にする。
                self.net.streams.remove(&fd);
                Ok(ok(Val::Unit))
            }
            "flush" => Ok(match self.net.streams.get_mut(&fd) {
                Some(st) => match st.flush() {
                    Ok(()) => ok(Val::Unit),
                    Err(e) => err(io_error(&e)),
                },
                None => invalid(),
            }),
            "write" => {
                let data = bytes_of(args.first().unwrap_or(&Val::Unit));
                Ok(match self.net.streams.get_mut(&fd) {
                    Some(st) => match st.write_all(&data) {
                        Ok(()) => ok(int(data.len() as i128, "usize")),
                        Err(e) => err(io_error(&e)),
                    },
                    None => invalid(),
                })
            }
            "read" => {
                let target = dr(args.first().unwrap_or(&Val::Unit));
                let mut buf = vec![0u8; seq_items(&target).map_or(0, |x| x.len())];
                let n = match self.net.streams.get_mut(&fd) {
                    Some(st) => match st.read(&mut buf) {
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
            _ => panic(format!("no method {} on TcpStream", name)),
        }
    }
}
