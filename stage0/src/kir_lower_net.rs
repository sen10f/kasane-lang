//! std.net (TCP) の KIR への下ろし (設計_KIR.md 12 章)。
//!
//! `listen`/`connect`/`accept`/`read`/`write`/`flush`/`close` を、ホストのソケットを使う
//! 参照実行器と libc の socket 系を呼ぶ LLVM に下ろす。
//!
//! - `std.net.listen(addr)` / `std.net.connect(addr)`: フリー関数。`addr` は fat (`&str`)。
//! - メソッド `accept`/`read`/`write`/`flush`/`close`: 受け手の `descriptor` フィールド (i32@0) から
//!   記述子を取り出し、対応する `net.*` 命令を出す。

use crate::check::{items, nf};
use crate::kir::{NetK, Op, Pay, Pointee, Ty, VId};
use crate::parser::V;

use super::{Lw, LR};

/// `std.io.Error` 集成体をペイロードに持つ `Result<i32, std.io.Error>` / `Result<usize, ...>` / `Result<(), ...>`。
/// `read` / `write` / `flush` の失敗は `std.io.Error` (設計_KIR.md 12 章、`spec/stdlib/std/net.sig`)。
fn net_err_ty(lw: &mut Lw, payload: Ty) -> Ty {
    let ioerr = lw.aggs.borrow_mut().io_error(lw.p).expect("std.io.Error の定義がない");
    Ty::Res(Pay::of(payload).expect("ペイロードがスカラーになる"), Pay::of(ioerr).expect("std.io.Error は集成体ペイロードになる"))
}

/// `std.net.Error` (`{ code: i32 }`) をペイロードに持つ `Result`。`listen` / `connect` / `accept` / `local_port` / `close` の失敗。
/// 失敗のペイロードは errno (下位 32 ビット) で、`kir_llvm` の `net_result` と `kir_exec` の `net_error_agg` がこの形に直す。
fn net_sock_err_ty(lw: &mut Lw, payload: Pay) -> Ty {
    let neterr = lw.aggs.borrow().agg_index("std.net.Error").expect("std.net.Error が型表にない");
    Ty::Res(payload, Pay::Agg(neterr))
}

/// 受け手 (`ref<agg>` / `mutref<agg>` / `Agg`) から `descriptor` フィールド (i32@0) を読む。
fn sock_fd(lw: &mut Lw, rv: VId, rt: Ty, line: usize) -> LR<VId> {
    let fd = match rt {
        Ty::Agg(_) => lw.emit(Ty::I32, Op::Field { a: rv, off: 0 }),
        Ty::Ref(true, Pointee::Agg(_)) => {
            let fdptr = lw.emit(Ty::Ref(true, Pointee::I32), Op::FieldAddr { a: rv, off: 0 });
            lw.emit(Ty::I32, Op::Load(fdptr))
        }
        Ty::Ref(false, Pointee::Agg(_)) => {
            let fdptr = lw.emit(Ty::Ref(false, Pointee::I32), Op::FieldAddr { a: rv, off: 0 });
            lw.emit(Ty::I32, Op::Load(fdptr))
        }
        _ => return lw.err(line, format!("net の受け手の型 {} は段階 1 の対象外", rt.name())),
    };
    Ok(fd)
}

/// `&mut [u8]` (fat) のバッファを `(ptr, len)` に分解する。
/// `&mut [u8]` / `&[u8]` (fat) のバッファを `(ptr, len)` に分解する。
/// `&mut 配列` は `mutref<fat>` で渡されるので、`load` して fat を取り出す。
fn buf_ptr_len(lw: &mut Lw, buf: VId, line: usize) -> LR<(VId, VId)> {
    let buf = match lw.vty(buf) {
        Ty::Ref(_, Pointee::Agg(ai)) if lw.aggs.borrow().defs[ai as usize].name == "fat" => {
            lw.emit(lw.fat_ty(), Op::Load(buf))
        }
        _ => buf,
    };
    if lw.vty(buf) != lw.fat_ty() {
        return lw.err(line, "net のバッファは &[u8] ではない");
    }
    let ptr = lw.emit(Ty::Ptr, Op::Field { a: buf, off: 0 });
    let len = lw.emit(Ty::Usize, Op::Field { a: buf, off: 8 });
    Ok((ptr, len))
}

/// `std.net.listen` / `std.net.connect` (フリー関数)。`is_listen` でどちらかを選ぶ。
pub(super) fn net_call(lw: &mut Lw, e: &V, is_listen: bool, line: usize) -> LR<Option<VId>> {
    let f = nf(e);
    let args = items(&f[1]);
    if args.len() != 1 {
        return lw.err(line, "net 関数の引数は 1 個のはず");
    }
    let Some(addr) = lw.expr(&args[0])? else { return Ok(None) };
    let fat = lw.fat_ty();
    if lw.vty(addr) != fat {
        return lw.err(line, "net のアドレスは &str ではない");
    }
    let res_ty = if is_listen {
        let agg = lw.aggs.borrow().agg_index("std.net.TcpListener").expect("std.net.TcpListener が型表にない");
        net_sock_err_ty(lw, Pay::Agg(agg))
    } else {
        let agg = lw.aggs.borrow().agg_index("std.net.TcpStream").expect("std.net.TcpStream が型表にない");
        net_sock_err_ty(lw, Pay::Agg(agg))
    };
    let op = if is_listen {
        Op::NetListen { addr }
    } else {
        Op::NetConnect { addr }
    };
    Ok(Some(lw.emit(res_ty, op)))
}

/// `TcpListener` / `TcpStream` のメソッド `accept`/`read`/`write`/`flush`/`close`。
pub(super) fn net_method(lw: &mut Lw, _e: &V, name: &str, recv: VId, rt: Ty, args: &[V], line: usize) -> LR<Option<VId>> {
    let fd = sock_fd(lw, recv, rt, line)?;
    let op = match name {
        "accept" => {
            if !args.is_empty() {
                return lw.err(line, "accept の引数は 0 個のはず");
            }
            Op::NetAccept { sock: fd }
        }
        "read" => {
            if args.len() != 1 {
                return lw.err(line, "read の引数は 1 個のはず");
            }
            let Some(buf) = lw.expr(&args[0])? else { return Ok(None) };
            let (ptr, len) = buf_ptr_len(lw, buf, line)?;
            Op::NetRead { sock: fd, buf: ptr, len }
        }
        "write" => {
            if args.len() != 1 {
                return lw.err(line, "write の引数は 1 個のはず");
            }
            let Some(buf) = lw.expr(&args[0])? else { return Ok(None) };
            let (ptr, len) = buf_ptr_len(lw, buf, line)?;
            Op::NetWrite { sock: fd, buf: ptr, len }
        }
        "flush" => {
            if !args.is_empty() {
                return lw.err(line, "flush の引数は 0 個のはず");
            }
            Op::NetFlush { sock: fd }
        }
        "close" => {
            if !args.is_empty() {
                return lw.err(line, "close の引数は 0 個のはず");
            }
            Op::NetClose { sock: fd }
        }
        "fd" => {
            if !args.is_empty() {
                return lw.err(line, "fd の引数は 0 個のはず");
            }
            return Ok(Some(fd));
        }
        "local_port" => {
            if !args.is_empty() {
                return lw.err(line, "local_port の引数は 0 個のはず");
            }
            Op::NetLocalPort { sock: fd }
        }
        // 追加分 (`NetK`): タイムアウト・shutdown・アドレス・UDP の送受信。
        "set_read_timeout" | "set_write_timeout" => {
            if args.len() != 1 {
                return lw.err(line, format!("{} の引数は 1 個のはず", name));
            }
            let Some(ms) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(ms) != Ty::U64 {
                return lw.err(line, format!("{} の引数は u64 のはず", name));
            }
            let kind = if name == "set_read_timeout" { NetK::ReadTimeout } else { NetK::WriteTimeout };
            let res_ty = net_err_ty(lw, Ty::Unit);
            return Ok(Some(lw.emit(res_ty, Op::NetSys { kind, args: vec![fd, ms] })));
        }
        // 期限つき (`ReadDeadline` など): 引数は ms (u64) か、nodelay の bool。
        "set_read_deadline" | "set_write_deadline" => {
            if args.len() != 1 {
                return lw.err(line, format!("{} の引数は 1 個のはず", name));
            }
            let Some(ms) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(ms) != Ty::U64 {
                return lw.err(line, format!("{} の引数は u64 のはず", name));
            }
            let kind = if name == "set_read_deadline" { NetK::ReadDeadline } else { NetK::WriteDeadline };
            let res_ty = net_err_ty(lw, Ty::Unit);
            return Ok(Some(lw.emit(res_ty, Op::NetSys { kind, args: vec![fd, ms] })));
        }
        "clear_deadlines" => {
            if !args.is_empty() {
                return lw.err(line, "clear_deadlines の引数は 0 個のはず");
            }
            let res_ty = net_err_ty(lw, Ty::Unit);
            return Ok(Some(lw.emit(res_ty, Op::NetSys { kind: NetK::ClearDeadlines, args: vec![fd] })));
        }
        "set_nodelay" => {
            if args.len() != 1 {
                return lw.err(line, "set_nodelay の引数は 1 個のはず");
            }
            let Some(on) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(on) != Ty::Bool {
                return lw.err(line, "set_nodelay の引数は bool のはず");
            }
            let res_ty = net_err_ty(lw, Ty::Unit);
            return Ok(Some(lw.emit(res_ty, Op::NetSys { kind: NetK::SetNoDelay, args: vec![fd, on] })));
        }
        "accept_timeout" => {
            if args.len() != 1 {
                return lw.err(line, "accept_timeout の引数は 1 個のはず");
            }
            let Some(ms) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(ms) != Ty::U64 {
                return lw.err(line, "accept_timeout の引数は u64 のはず");
            }
            let agg = lw.aggs.borrow().agg_index("std.net.TcpStream").expect("std.net.TcpStream が型表にない");
            let res_ty = net_sock_err_ty(lw, Pay::Agg(agg));
            return Ok(Some(lw.emit(res_ty, Op::NetSys { kind: NetK::AcceptTimeout, args: vec![fd, ms] })));
        }
        "shutdown" => {
            if args.len() != 1 {
                return lw.err(line, "shutdown の引数は 1 個のはず");
            }
            let Some(how) = lw.expr(&args[0])? else { return Ok(None) };
            // `std.net.Shutdown` の変種の添字 (Read 0 / Write 1 / Both 2)。
            let tag = lw.emit(Ty::I32, Op::Tag(how));
            let res_ty = net_err_ty(lw, Ty::Unit);
            return Ok(Some(lw.emit(res_ty, Op::NetSys { kind: NetK::Shutdown, args: vec![fd, tag] })));
        }
        "peer_addr_raw" | "local_addr_raw" => {
            if args.len() != 1 {
                return lw.err(line, format!("{} の引数は 1 個のはず", name));
            }
            let Some(buf) = lw.expr(&args[0])? else { return Ok(None) };
            let (p, n) = buf_ptr_len(lw, buf, line)?;
            let kind = if name == "peer_addr_raw" { NetK::PeerAddr } else { NetK::LocalAddr };
            let res_ty = net_err_ty(lw, Ty::Usize);
            return Ok(Some(lw.emit(res_ty, Op::NetSys { kind, args: vec![fd, p, n] })));
        }
        "send_to" => {
            if args.len() != 2 {
                return lw.err(line, "send_to の引数は 2 個のはず");
            }
            let Some(buf) = lw.expr(&args[0])? else { return Ok(None) };
            let (p, n) = buf_ptr_len(lw, buf, line)?;
            let Some(addr) = lw.expr(&args[1])? else { return Ok(None) };
            if lw.vty(addr) != lw.fat_ty() {
                return lw.err(line, "send_to のアドレスは &str ではない");
            }
            let res_ty = net_err_ty(lw, Ty::Usize);
            return Ok(Some(lw.emit(res_ty, Op::NetSys { kind: NetK::UdpSendTo, args: vec![fd, p, n, addr] })));
        }
        "recv_from_raw" => {
            if args.len() != 2 {
                return lw.err(line, "recv_from_raw の引数は 2 個のはず");
            }
            let Some(buf) = lw.expr(&args[0])? else { return Ok(None) };
            let (p, n) = buf_ptr_len(lw, buf, line)?;
            let Some(from) = lw.expr(&args[1])? else { return Ok(None) };
            let (fp, fnn) = buf_ptr_len(lw, from, line)?;
            let res_ty = net_err_ty(lw, Ty::Usize);
            return Ok(Some(lw.emit(res_ty, Op::NetSys { kind: NetK::UdpRecvFrom, args: vec![fd, p, n, fp, fnn] })));
        }
        _ => return Ok(None),
    };
    let res_ty = match name {
        "accept" => {
            let stream = lw.aggs.borrow().agg_index("std.net.TcpStream").expect("std.net.TcpStream が型表にない");
            net_sock_err_ty(lw, Pay::Agg(stream))
        }
        "read" | "write" => net_err_ty(lw, Ty::Usize),
        "local_port" => net_sock_err_ty(lw, Pay::U16),
        // `flush` は `std.io.Error`、`close` は `std.net.Error` (spec の型どおり)。
        "close" => net_sock_err_ty(lw, Pay::Unit),
        _ => net_err_ty(lw, Ty::Unit),
    };
    Ok(Some(lw.emit(res_ty, op)))
}

/// 自由関数 `std.net.resolve_raw(host, port, out)` / `std.net.UdpSocket.bind(addr)` の下ろし。
pub(super) fn net_free_call(lw: &mut Lw, e: &V, name: &str, line: usize) -> LR<Option<VId>> {
    let f = nf(e);
    let args = items(&f[1]);
    if name == "std.net.connect_timeout" {
        if args.len() != 2 {
            return lw.err(line, "std.net.connect_timeout の引数は 2 個のはず");
        }
        let Some(addr) = lw.expr(&args[0])? else { return Ok(None) };
        if lw.vty(addr) != lw.fat_ty() {
            return lw.err(line, "std.net.connect_timeout のアドレスは &str ではない");
        }
        let Some(ms) = lw.expr(&args[1])? else { return Ok(None) };
        if lw.vty(ms) != Ty::U64 {
            return lw.err(line, "std.net.connect_timeout の期限は u64 ではない");
        }
        let agg = lw.aggs.borrow().agg_index("std.net.TcpStream").expect("std.net.TcpStream が型表にない");
        let res_ty = net_sock_err_ty(lw, Pay::Agg(agg));
        return Ok(Some(lw.emit(res_ty, Op::NetSys { kind: NetK::ConnectTimeout, args: vec![addr, ms] })));
    }
    if name == "std.net.UdpSocket.bind" {
        if args.len() != 1 {
            return lw.err(line, "std.net.UdpSocket.bind の引数は 1 個のはず");
        }
        let Some(addr) = lw.expr(&args[0])? else { return Ok(None) };
        if lw.vty(addr) != lw.fat_ty() {
            return lw.err(line, "std.net.UdpSocket.bind のアドレスは &str ではない");
        }
        let agg = lw.aggs.borrow().agg_index("std.net.UdpSocket").expect("std.net.UdpSocket が型表にない");
        let res_ty = net_sock_err_ty(lw, Pay::Agg(agg));
        return Ok(Some(lw.emit(res_ty, Op::NetSys { kind: NetK::UdpBind, args: vec![addr] })));
    }
    if args.len() != 3 {
        return lw.err(line, "std.net.resolve_raw の引数は 3 個のはず");
    }
    let Some(host) = lw.expr(&args[0])? else { return Ok(None) };
    if lw.vty(host) != lw.fat_ty() {
        return lw.err(line, "std.net.resolve_raw のホストは &str ではない");
    }
    let Some(port) = lw.expr(&args[1])? else { return Ok(None) };
    if lw.vty(port) != Ty::U16 {
        return lw.err(line, "std.net.resolve_raw のポートは u16 ではない");
    }
    let Some(buf) = lw.expr(&args[2])? else { return Ok(None) };
    let (p, n) = buf_ptr_len(lw, buf, line)?;
    let res_ty = net_err_ty(lw, Ty::Usize);
    Ok(Some(lw.emit(res_ty, Op::NetSys { kind: NetK::Resolve, args: vec![host, port, p, n] })))
}

/// 型 `rt` が `std.net.TcpListener` / `TcpStream` / `UdpSocket` (裸または参照) か。
pub(super) fn is_net_ty(lw: &Lw, rt: Ty) -> bool {
    match rt {
        Ty::Agg(ai) | Ty::Ref(_, Pointee::Agg(ai)) => {
            matches!(
                lw.aggs.borrow().defs[ai as usize].name.as_str(),
                "std.net.TcpListener" | "std.net.TcpStream" | "std.net.UdpSocket"
            )
        }
        _ => false,
    }
}


