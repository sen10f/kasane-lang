//! `std.net` の追加分 (`NetK`) の VM 実装。意味は参照実行器 (`kir_exec.rs` の `Op::NetSys`) と同じ。

use super::*;
use crate::kir::NetK;
use std::net::{Shutdown, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

impl<'a> Vm<'a> {
    /// 引数 `args` は (スロット, 語数) の列。結果は `d` へ書く。失敗は `Result` の `Err` (`UdpBind` / `AcceptTimeout` / `ConnectTimeout` だけ `std.net.Error`、ほかは `std.io.Error`)。
    pub(super) fn net_sys(&mut self, rp: *mut u64, aggs: &[crate::kir::AggDef], d: S, kind: NetK, args: &[(usize, usize)]) -> Result<(), String> {
        let rd = |k: usize| unsafe { *rp.add(args[k].0) };
        let fd = |k: usize| rd(k) as i32 as i128;
        let wr = |off: usize, v: u64| unsafe { *rp.add(d as usize + off) = v };
        let ok_unit = || wr(0, 0);
        let ok_val = |v: u64| {
            wr(0, 0);
            wr(1, v);
        };
        let net_err = |e: &std::io::Error| {
            wr(0, 1);
            wr(1, e.raw_os_error().unwrap_or(-1) as u32 as u64);
        };
        let invalid = || std::io::Error::from(std::io::ErrorKind::InvalidInput);
        // 文字列 (fat) を読む。
        let text = |k: usize| -> Result<String, String> { Ok(std::str::from_utf8(Self::fat_bytes(rp, args[k].0)?).unwrap_or("").to_string()) };
        let unit_res = |me: &Self, r: std::io::Result<()>| -> Result<(), String> {
            let _ = me;
            match r {
                Ok(()) => {
                    ok_unit();
                    Ok(())
                }
                Err(e) => fs_err(rp, aggs, d, &e),
            }
        };
        match kind {
            NetK::ReadTimeout | NetK::WriteTimeout => {
                let ms = rd(1);
                let dur = if ms == 0 { None } else { Some(Duration::from_millis(ms)) };
                let r = match self.socks.get(&fd(0)) {
                    Some(NetSock::Stream(s)) => {
                        if matches!(kind, NetK::ReadTimeout) {
                            s.set_read_timeout(dur)
                        } else {
                            s.set_write_timeout(dur)
                        }
                    }
                    _ => Err(invalid()),
                };
                unit_res(self, r)
            }
            NetK::ReadDeadline | NetK::WriteDeadline => {
                let at = Instant::now() + Duration::from_millis(rd(1));
                let f = fd(0);
                let r = if self.socks.contains_key(&f) {
                    let e = self.deadlines.entry(f).or_insert((None, None));
                    if matches!(kind, NetK::ReadDeadline) {
                        e.0 = Some(at);
                    } else {
                        e.1 = Some(at);
                    }
                    Ok(())
                } else {
                    Err(invalid())
                };
                unit_res(self, r)
            }
            NetK::ClearDeadlines => {
                self.deadlines.remove(&fd(0));
                ok_unit();
                Ok(())
            }
            NetK::SetNoDelay => {
                let r = match self.socks.get(&fd(0)) {
                    Some(NetSock::Stream(s)) => s.set_nodelay(rd(1) != 0),
                    _ => Err(invalid()),
                };
                unit_res(self, r)
            }
            NetK::Shutdown => {
                let how = match rd(1) {
                    0 => Shutdown::Read,
                    1 => Shutdown::Write,
                    _ => Shutdown::Both,
                };
                let r = match self.socks.get(&fd(0)) {
                    Some(NetSock::Stream(s)) => s.shutdown(how),
                    _ => Err(invalid()),
                };
                unit_res(self, r)
            }
            NetK::AcceptTimeout => {
                let r = match self.socks.get(&fd(0)) {
                    Some(NetSock::Listener(l)) => kir_exec::accept_until(l, rd(1)),
                    _ => Err(invalid()),
                };
                match r {
                    Ok(st) => {
                        let nfd = self.new_fd(NetSock::Stream(st));
                        ok_val(nfd as i32 as u32 as u64);
                    }
                    Err(e) => net_err(&e),
                }
                Ok(())
            }
            NetK::ConnectTimeout => {
                let a = text(0)?;
                match kir_exec::connect_until(&a, rd(1)) {
                    Ok(st) => {
                        let nfd = self.new_fd(NetSock::Stream(st));
                        ok_val(nfd as i32 as u32 as u64);
                    }
                    Err(e) => net_err(&e),
                }
                Ok(())
            }
            NetK::PeerAddr | NetK::LocalAddr => {
                let (p, cap) = (rd(1), rd(2) as usize);
                let r = match self.socks.get(&fd(0)) {
                    Some(NetSock::Stream(s)) => (if matches!(kind, NetK::PeerAddr) { s.peer_addr() } else { s.local_addr() }).map(|x| x.to_string().into_bytes()),
                    _ => Err(invalid()),
                };
                match r {
                    Ok(b) if b.len() <= cap => {
                        if !b.is_empty() {
                            unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), addr(p, b.len())?, b.len()) };
                        }
                        ok_val(b.len() as u64);
                        Ok(())
                    }
                    Ok(_) => fs_err(rp, aggs, d, &invalid()),
                    Err(e) => fs_err(rp, aggs, d, &e),
                }
            }
            NetK::Resolve => {
                let host = text(0)?;
                let (port, p, cap) = (rd(1) as u16, rd(2), rd(3) as usize);
                let r = (host.as_str(), port).to_socket_addrs().map(|it| {
                    let mut t: Vec<u8> = Vec::new();
                    for x in it {
                        t.extend_from_slice(x.to_string().as_bytes());
                        t.push(0);
                    }
                    t
                });
                match r {
                    Ok(t) if t.len() <= cap => {
                        if !t.is_empty() {
                            unsafe { std::ptr::copy_nonoverlapping(t.as_ptr(), addr(p, t.len())?, t.len()) };
                        }
                        ok_val(t.len() as u64);
                        Ok(())
                    }
                    Ok(_) => fs_err(rp, aggs, d, &invalid()),
                    // 解決の失敗は Other (番号 0)。木走査の `resolve_raw` と同じ。
                    Err(_) => fs_err(rp, aggs, d, &std::io::Error::from(std::io::ErrorKind::Other)),
                }
            }
            NetK::UdpBind => {
                let a = text(0)?;
                match UdpSocket::bind(a.as_str()) {
                    Ok(s) => {
                        let nfd = self.new_fd(NetSock::Udp(s));
                        ok_val(nfd as i32 as u32 as u64);
                    }
                    Err(e) => net_err(&e),
                }
                Ok(())
            }
            NetK::ReadTry | NetK::WriteTry => {
                use std::io::{Read, Write};
                let (p, n) = (rd(1), rd(2) as usize);
                let write = matches!(kind, NetK::WriteTry);
                let r: std::io::Result<usize> = match self.socks.get_mut(&fd(0)) {
                    Some(NetSock::Stream(s)) => {
                        let _ = s.set_nonblocking(true);
                        let r = if write {
                            let bytes: &[u8] = if n == 0 { &[] } else { unsafe { std::slice::from_raw_parts(addr(p, n)? as *const u8, n) } };
                            s.write(bytes)
                        } else {
                            let mut v = vec![0u8; n];
                            s.read(&mut v).map(|k| {
                                if k > 0 {
                                    if let Ok(dst) = addr(p, k) {
                                        unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), dst, k) };
                                    }
                                }
                                k
                            })
                        };
                        let _ = s.set_nonblocking(false);
                        r
                    }
                    _ => Err(invalid()),
                };
                match r {
                    Ok(k) => {
                        ok_val(k as u64);
                        Ok(())
                    }
                    Err(e) => fs_err(rp, aggs, d, &e),
                }
            }
            NetK::UdpSendTo => {
                let (p, n) = (rd(1), rd(2) as usize);
                let bytes: &[u8] = if n == 0 { &[] } else { unsafe { std::slice::from_raw_parts(addr(p, n)? as *const u8, n) } };
                let a = text(3)?;
                let r = match self.socks.get(&fd(0)) {
                    Some(NetSock::Udp(s)) => s.send_to(bytes, a.as_str()),
                    _ => Err(invalid()),
                };
                match r {
                    Ok(k) => {
                        ok_val(k as u64);
                        Ok(())
                    }
                    Err(e) => fs_err(rp, aggs, d, &e),
                }
            }
            NetK::UdpRecvFrom => {
                let (p, n, fp, fc) = (rd(1), rd(2) as usize, rd(3), rd(4) as usize);
                let mut v = vec![0u8; n];
                let r = match self.socks.get(&fd(0)) {
                    Some(NetSock::Udp(s)) => s.recv_from(&mut v),
                    _ => Err(invalid()),
                };
                match r {
                    Ok((nr, src)) => {
                        let mut t = src.to_string().into_bytes();
                        t.push(0);
                        if t.len() > fc {
                            return fs_err(rp, aggs, d, &invalid());
                        }
                        if nr > 0 {
                            unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), addr(p, nr)?, nr) };
                        }
                        unsafe { std::ptr::copy_nonoverlapping(t.as_ptr(), addr(fp, t.len())?, t.len()) };
                        ok_val(nr as u64);
                        Ok(())
                    }
                    Err(e) => fs_err(rp, aggs, d, &e),
                }
            }
        }
    }
}
