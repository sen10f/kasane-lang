//! std.net (TCP / UDP) のインタプリタ実装。std.fs と同じく記述子表 (記述子 -> ソケット) を持つ。
//! 失敗は panic ではなく `Err(std.net.Error)` / `Err(std.io.Error)` の値で返す。

use std::cell::RefCell;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::rc::Rc;
use std::time::Duration;

use crate::interp::Interp;
use crate::natives::{bytes_of, io_error};
use crate::value::*;

/// 記述子表。リスナ・ストリーム・UDP ソケットは同じ番号空間 (`Interp::next_fd`) を使う。
#[derive(Default)]
pub struct NetTable {
    pub listeners: std::collections::HashMap<i128, TcpListener>,
    pub streams: std::collections::HashMap<i128, TcpStream>,
    pub udps: std::collections::HashMap<i128, UdpSocket>,
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

/// `std.net.Shutdown` の変種を読む (`Read` / `Write` / `Both`)。
fn shutdown_of(v: &Val) -> Option<Shutdown> {
    match dr(v) {
        Val::Enum(e) => match e.var.as_str() {
            "Read" => Some(Shutdown::Read),
            "Write" => Some(Shutdown::Write),
            "Both" => Some(Shutdown::Both),
            _ => None,
        },
        _ => None,
    }
}

/// 出力バッファ (`&mut [u8]`) へ `data` を書く。入りきらなければ InvalidInput (何も書かない)。成功は書いたバイト数。
fn put(target: &Val, data: &[u8]) -> Val {
    let cap = seq_items(target).map_or(0, |x| x.len());
    if data.len() > cap {
        return err(io_error_kind_invalid());
    }
    fill(target, data);
    ok(int(data.len() as i128, "usize"))
}

/// バッファへ書く (容量の検査は呼び出し側)。
pub(crate) fn fill(target: &Val, data: &[u8]) {
    match &crate::value::as_raw(target) {
        Val::View(l, a, _) => {
            let mut l = l.borrow_mut();
            for (i, b) in data.iter().enumerate() {
                l[*a + i] = int(*b as i128, "u8");
            }
        }
        Val::Arr(l) | Val::Vec(l) => {
            let mut l = l.borrow_mut();
            for (i, b) in data.iter().enumerate() {
                l[i] = int(*b as i128, "u8");
            }
        }
        _ => {}
    }
}

fn io_error_kind_invalid() -> Val {
    Val::Struct(Rc::new(StructV {
        ty: "std.io.Error".into(),
        f: RefCell::new(vec![("kind".into(), mk_enum("std.io.ErrorKind", "InvalidInput", vec![])), ("code".into(), int(0, "i32"))]),
    }))
}

impl Interp {
    #[cfg_attr(unix, allow(dead_code))]
    fn alloc_fd(&mut self) -> i128 {
        let fd = self.next_fd;
        self.next_fd += 1;
        fd
    }

    /// ソケットの記述子の番号。Unix では OS の fd (ネイティブ生成と同じ番号)。`std.tls` は `descriptor` を
    /// `SSL_set_fd` に渡すので、仮想の番号では別のファイルを指してしまう (3.8 の FFI と 8.3 の `std.tls`)。
    #[cfg(unix)]
    fn sock_fd<T: std::os::unix::io::AsRawFd>(&mut self, s: &T) -> i128 {
        s.as_raw_fd() as i128
    }

    #[cfg(not(unix))]
    fn sock_fd<T>(&mut self, _s: &T) -> i128 {
        self.alloc_fd()
    }

    /// 自由関数: `std.net.connect` / `std.net.listen` / `std.net.UdpSocket.bind` / `std.net.resolve_raw`。
    pub fn net_native(&mut self, name: &str, args: &[Val]) -> Res {
        let first = dr(args.first().unwrap_or(&Val::Unit));
        let addr = str_of(&first).unwrap_or_default();
        match name {
            "std.net.connect" => Ok(match TcpStream::connect(addr.as_str()) {
                Ok(s) => {
                    let fd = self.sock_fd(&s);
                    self.net.streams.insert(fd, s);
                    ok(handle("std.net.TcpStream", fd))
                }
                Err(e) => err(net_error(&e)),
            }),
            "std.net.listen" => Ok(match TcpListener::bind(addr.as_str()) {
                Ok(l) => {
                    let fd = self.sock_fd(&l);
                    self.net.listeners.insert(fd, l);
                    ok(handle("std.net.TcpListener", fd))
                }
                Err(e) => err(net_error(&e)),
            }),
            "std.net.UdpSocket.bind" => Ok(match UdpSocket::bind(addr.as_str()) {
                Ok(s) => {
                    let fd = self.sock_fd(&s);
                    self.net.udps.insert(fd, s);
                    ok(handle("std.net.UdpSocket", fd))
                }
                Err(e) => err(net_error(&e)),
            }),
            "std.net.resolve_raw" => {
                // 解決の結果は `アドレス\0` の並び。失敗 (解決できない) は Other で、OS の番号は持たない (code 0)。
                let port = match args.get(1).map(dr) {
                    Some(Val::Int(p, _)) => p as u16,
                    _ => 0,
                };
                let out = dr(args.get(2).unwrap_or(&Val::Unit));
                Ok(match (addr.as_str(), port).to_socket_addrs() {
                    Ok(it) => {
                        let mut text: Vec<u8> = Vec::new();
                        for a in it {
                            text.extend_from_slice(a.to_string().as_bytes());
                            text.push(0);
                        }
                        put(&out, &text)
                    }
                    Err(_) => err(io_error_other()),
                })
            }
            _ => panic(format!("unknown native {}", name)),
        }
    }

    /// `std.net.TcpListener` / `std.net.TcpStream` / `std.net.UdpSocket` のメソッド。
    pub fn net_method(&mut self, s: &Rc<StructV>, name: &str, args: &[Val]) -> Res {
        let Some(Val::Int(fd, _)) = s.get("descriptor") else { return panic("invalid net handle") };
        if s.ty == "std.net.TcpListener" {
            return Ok(match name {
                "accept" => match self.net.listeners.get(&fd) {
                    Some(l) => match l.accept() {
                        Ok((st, _)) => {
                            let nfd = self.sock_fd(&st);
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
        if s.ty == "std.net.UdpSocket" {
            return self.udp_method(fd, name, args);
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
                fill(&target, &buf[..n]);
                Ok(ok(int(n as i128, "usize")))
            }
            "set_read_timeout" | "set_write_timeout" => {
                let ms = match args.first().map(dr) {
                    Some(Val::Int(v, _)) => v as u64,
                    _ => 0,
                };
                // 0 = タイムアウトなし (`None`)。
                let dur = if ms == 0 { None } else { Some(Duration::from_millis(ms)) };
                Ok(match self.net.streams.get(&fd) {
                    Some(st) => {
                        let r = if name == "set_read_timeout" { st.set_read_timeout(dur) } else { st.set_write_timeout(dur) };
                        match r {
                            Ok(()) => ok(Val::Unit),
                            Err(e) => err(io_error(&e)),
                        }
                    }
                    None => invalid(),
                })
            }
            "shutdown" => {
                let Some(how) = args.first().and_then(shutdown_of) else { return panic("shutdown の引数は std.net.Shutdown のはず") };
                Ok(match self.net.streams.get(&fd) {
                    Some(st) => match st.shutdown(how) {
                        Ok(()) => ok(Val::Unit),
                        Err(e) => err(io_error(&e)),
                    },
                    None => invalid(),
                })
            }
            "peer_addr_raw" | "local_addr_raw" => {
                let target = dr(args.first().unwrap_or(&Val::Unit));
                let addr = match self.net.streams.get(&fd) {
                    Some(st) => {
                        if name == "peer_addr_raw" {
                            st.peer_addr()
                        } else {
                            st.local_addr()
                        }
                    }
                    None => return Ok(invalid()),
                };
                Ok(match addr {
                    Ok(a) => put(&target, a.to_string().as_bytes()),
                    Err(e) => err(io_error(&e)),
                })
            }
            _ => panic(format!("no method {} on TcpStream", name)),
        }
    }

    /// `std.net.UdpSocket` のメソッド。`close` の記述子は表から外す (drop で閉じる)。
    fn udp_method(&mut self, fd: i128, name: &str, args: &[Val]) -> Res {
        let invalid = || err(io_error(&std::io::Error::from(std::io::ErrorKind::InvalidInput)));
        Ok(match name {
            "close" => {
                self.net.udps.remove(&fd);
                ok(Val::Unit)
            }
            "local_port" => match self.net.udps.get(&fd).and_then(|s| s.local_addr().ok()) {
                Some(a) => ok(int(a.port() as i128, "u16")),
                None => err(net_error_code(-1)),
            },
            "send_to" => {
                let data = bytes_of(args.first().unwrap_or(&Val::Unit));
                let addr = str_of(&dr(args.get(1).unwrap_or(&Val::Unit))).unwrap_or_default();
                match self.net.udps.get(&fd) {
                    Some(s) => match s.send_to(&data, addr.as_str()) {
                        Ok(n) => ok(int(n as i128, "usize")),
                        Err(e) => err(io_error(&e)),
                    },
                    None => invalid(),
                }
            }
            "recv_from_raw" => {
                let target = dr(args.first().unwrap_or(&Val::Unit));
                let from = dr(args.get(1).unwrap_or(&Val::Unit));
                let mut buf = vec![0u8; seq_items(&target).map_or(0, |x| x.len())];
                let (n, src) = match self.net.udps.get(&fd) {
                    Some(s) => match s.recv_from(&mut buf) {
                        Ok(v) => v,
                        Err(e) => return Ok(err(io_error(&e))),
                    },
                    None => return Ok(invalid()),
                };
                // 送り元の文字列は NUL 終端で書く。入りきらなければ InvalidInput (データグラムは捨てる)。
                let mut text = src.to_string().into_bytes();
                text.push(0);
                let cap = seq_items(&from).map_or(0, |x| x.len());
                if text.len() > cap {
                    return Ok(err(io_error_kind_invalid()));
                }
                fill(&target, &buf[..n]);
                fill(&from, &text);
                ok(int(n as i128, "usize"))
            }
            _ => return panic(format!("no method {} on UdpSocket", name)),
        })
    }
}

/// `io::Error` を持たない失敗 (名前解決の失敗など) の `std.io.Error` (Other、番号 0)。
fn io_error_other() -> Val {
    Val::Struct(Rc::new(StructV {
        ty: "std.io.Error".into(),
        f: RefCell::new(vec![("kind".into(), mk_enum("std.io.ErrorKind", "Other", vec![])), ("code".into(), int(0, "i32"))]),
    }))
}
