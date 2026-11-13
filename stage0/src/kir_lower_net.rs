//! std.net (TCP) の KIR への下ろし (設計_KIR.md 12 章)。
//!
//! `listen`/`connect`/`accept`/`read`/`write`/`flush`/`close` を、ホストのソケットを使う
//! 参照実行器と libc の socket 系を呼ぶ LLVM に下ろす。
//!
//! - `std.net.listen(addr)` / `std.net.connect(addr)`: フリー関数。`addr` は fat (`&str`)。
//! - メソッド `accept`/`read`/`write`/`flush`/`close`: 受け手の `descriptor` フィールド (i32@0) から
//!   記述子を取り出し、対応する `net.*` 命令を出す。

use crate::check::{items, nf};
use crate::kir::{Op, Pay, Pointee, Ty, VId};
use crate::parser::V;

use super::{Lw, LR};

/// `std.io.Error` 集成体をペイロードに持つ `Result<i32, std.io.Error>` / `Result<usize, ...>` / `Result<(), ...>`。
fn net_err_ty(lw: &mut Lw, payload: Ty) -> Ty {
    let ioerr = lw.aggs.borrow_mut().io_error(lw.p).expect("std.io.Error の定義がない");
    Ty::Res(Pay::of(payload).expect("ペイロードがスカラーになる"), Pay::of(ioerr).expect("std.io.Error は集成体ペイロードになる"))
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
    let ioerr = Pay::of(lw.aggs.borrow_mut().io_error(lw.p).expect("std.io.Error")).expect("ペイロード");
    let res_ty = if is_listen {
        let agg = lw.aggs.borrow().agg_index("std.net.TcpListener").expect("std.net.TcpListener が型表にない");
        Ty::Res(Pay::Agg(agg), ioerr)
    } else {
        let agg = lw.aggs.borrow().agg_index("std.net.TcpStream").expect("std.net.TcpStream が型表にない");
        Ty::Res(Pay::Agg(agg), ioerr)
    };
    let op = if is_listen {
        Op::NetListen { addr }
    } else {
        Op::NetConnect { addr }
    };
    Ok(Some(lw.emit(res_ty, op)))
}

/// `TcpListener` / `TcpStream` のメソッド `accept`/`read`/`write`/`flush`/`close`。
pub(super) fn net_method(lw: &mut Lw, e: &V, name: &str, recv: VId, rt: Ty, args: &[V], line: usize) -> LR<Option<VId>> {
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
        "local_port" => {
            if !args.is_empty() {
                return lw.err(line, "local_port の引数は 0 個のはず");
            }
            Op::NetLocalPort { sock: fd }
        }
        _ => return Ok(None),
    };
    let res_ty = match name {
        "accept" => {
            let stream = lw.aggs.borrow().agg_index("std.net.TcpStream").expect("std.net.TcpStream が型表にない");
            Ty::Res(Pay::Agg(stream), Pay::of(lw.aggs.borrow_mut().io_error(lw.p).expect("std.io.Error")).expect("ペイロード"))
        }
        "read" | "write" => net_err_ty(lw, Ty::Usize),
        "local_port" => net_err_ty(lw, Ty::U16),
        _ => net_err_ty(lw, Ty::Unit),
    };
    Ok(Some(lw.emit(res_ty, op)))
}

/// 型 `rt` が `std.net.TcpListener` / `TcpStream` (裸または参照) か。
pub(super) fn is_net_ty(lw: &Lw, rt: Ty) -> bool {
    match rt {
        Ty::Agg(ai) | Ty::Ref(_, Pointee::Agg(ai)) => {
            matches!(
                lw.aggs.borrow().defs[ai as usize].name.as_str(),
                "std.net.TcpListener" | "std.net.TcpStream"
            )
        }
        _ => false,
    }
}


