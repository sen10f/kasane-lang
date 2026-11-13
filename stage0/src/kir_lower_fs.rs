//! `std.fs` (ファイル) の KIR への下ろし (設計_KIR.md 12 章)。
//!
//! `create`/`open`/`exists`/`remove_file` をフリー関数として、`File` の
//! `close`/`flush`/`read`/`sync`/`write` をメソッドとして、ホストのファイル記述子を使う
//! 参照実行器と libc のファイル系を呼ぶ LLVM に下ろす。
//!
//! - `std.fs.create(path)` / `std.fs.open(path)`: フリー関数。`path` は fat (`&str`)。
//!   記述子は `std.fs.File` の `descriptor` フィールド (i32@0) に入れる。
//! - `std.fs.exists(path)` / `std.fs.remove_file(path)`: フリー関数。
//! - メソッド `close`/`flush`/`read`/`sync`/`write`: 受け手の `descriptor` フィールド (i32@0) から
//!   記述子を取り出し、対応する `fs.*` 命令を出す。
//!
//! `std.fs.read_to_end` は `alloc.vec.Vec` の再確保を伴うため KIR では未対応とし、木走査へ退避する。

use crate::kir::{Op, Pay, Pointee, Ty, VId};
use crate::parser::V;
use crate::program::Target as Tgt;

use super::{Lw, LR};

/// `std.io.Error` 集成体をペイロードに持つ `Result<payload, std.io.Error>`。
fn fs_err_ty(lw: &mut Lw, payload: Ty) -> Ty {
    let ioerr = lw.aggs.borrow_mut().io_error(lw.p).expect("std.io.Error の定義がない");
    Ty::Res(Pay::of(payload).expect("ペイロードがスカラーになる"), Pay::of(ioerr).expect("std.io.Error は集成体ペイロードになる"))
}

/// 受け手 (`ref<agg>` / `mutref<agg>` / `Agg`) から `descriptor` フィールド (i32@0) を読む。
fn fs_fd(lw: &mut Lw, rv: VId, rt: Ty, line: usize) -> LR<VId> {
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
        _ => return lw.err(line, format!("fs の受け手の型 {} は段階 1 の対象外", rt.name())),
    };
    Ok(fd)
}

/// `&mut [u8]` (fat) のバッファを `(ptr, len)` に分解する。
fn fs_buf_ptr_len(lw: &mut Lw, buf: VId, line: usize) -> LR<(VId, VId)> {
    let buf = match lw.vty(buf) {
        Ty::Ref(_, Pointee::Agg(ai)) if lw.aggs.borrow().defs[ai as usize].name == "fat" => {
            lw.emit(lw.fat_ty(), Op::Load(buf))
        }
        _ => buf,
    };
    if lw.vty(buf) != lw.fat_ty() {
        return lw.err(line, "fs のバッファは &[u8] ではない");
    }
    let ptr = lw.emit(Ty::Ptr, Op::Field { a: buf, off: 0 });
    let len = lw.emit(Ty::Usize, Op::Field { a: buf, off: 8 });
    Ok((ptr, len))
}

/// `std.fs.create` / `std.fs.open` / `std.fs.exists` / `std.fs.remove_file` / `std.fs.read_to_end` (フリー関数)。
pub(super) fn fs_call(lw: &mut Lw, _e: &V, name: &str, args: &[V], line: usize) -> LR<Option<VId>> {
    let segs: Vec<String> = name.split('.').map(|s| s.to_string()).collect();
    let Some(r) = lw.p.resolve(&segs, &lw.def.file) else {
        return lw.err(line, "std.fs.create が見つからない");
    };
    if !matches!(r.target, Tgt::Fn(_) | Tgt::Sig(_) | Tgt::Native(_)) {
        return lw.err(line, "std.fs.create が関数として解決できない");
    };
    // `std.fs.File` を KIR の型表に登録し、その集成体添字を取得する。
    let file = if let Some(i) = {
        let aggs = lw.aggs.borrow();
        aggs.agg_index("std.fs.File")
    } {
        i
    } else {
        let segs: Vec<String> = ["std", "fs", "File"].iter().map(|s| s.to_string()).collect();
        let Some(r) = lw.p.resolve(&segs, &lw.def.file) else {
            return lw.err(line, "std.fs.File の定義がない");
        };
        let Tgt::Type(di) = r.target else {
            return lw.err(line, "std.fs.File が型ではない");
        };
        match lw.aggs.borrow_mut().intern_def(lw.p, di) {
            Ok(Ty::Agg(i)) => i,
            _ => return lw.err(line, "std.fs.File の KIR 型表登録に失敗"),
        }
    };
    let ioerr = Pay::of(lw.aggs.borrow_mut().io_error(lw.p).expect("std.io.Error")).expect("ペイロード");
    let (op, res_ty) = match name {
        "std.fs.create" => {
            if args.len() != 1 {
                return lw.err(line, "std.fs.create の引数は 1 個のはず");
            }
            let Some(path) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(path) != lw.fat_ty() {
                return lw.err(line, "std.fs.create のパスは &str ではない");
            }
            (Op::FsCreate { path }, Ty::Res(Pay::Agg(file), ioerr))
        }
        "std.fs.open" => {
            if args.len() != 1 {
                return lw.err(line, "std.fs.open の引数は 1 個のはず");
            }
            let Some(path) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(path) != lw.fat_ty() {
                return lw.err(line, "std.fs.open のパスは &str ではない");
            }
            (Op::FsOpen { path }, Ty::Res(Pay::Agg(file), ioerr))
        }
        "std.fs.exists" => {
            if args.len() != 1 {
                return lw.err(line, "std.fs.exists の引数は 1 個のはず");
            }
            let Some(path) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(path) != lw.fat_ty() {
                return lw.err(line, "std.fs.exists のパスは &str ではない");
            }
            (Op::FsExists { path }, Ty::Bool)
        }
        "std.fs.remove_file" => {
            if args.len() != 1 {
                return lw.err(line, "std.fs.remove_file の引数は 1 個のはず");
            }
            let Some(path) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(path) != lw.fat_ty() {
                return lw.err(line, "std.fs.remove_file のパスは &str ではない");
            }
            (Op::FsRemove { path }, fs_err_ty(lw, Ty::Unit))
        }
        "std.fs.read_to_end" => {
            // `alloc.vec.Vec` の再確保を伴うため KIR では未対応。木走査へ退避。
            return Ok(None);
        }
        _ => return Ok(None),
    };
    Ok(Some(lw.emit(res_ty, op)))
}

/// `std.fs.File` のメソッド `close`/`flush`/`read`/`sync`/`write`。
pub(super) fn fs_method(lw: &mut Lw, _e: &V, name: &str, recv: VId, rt: Ty, args: &[V], line: usize) -> LR<Option<VId>> {
    let fd = fs_fd(lw, recv, rt, line)?;
    let (op, res_ty) = match name {
        "close" => {
            if !args.is_empty() {
                return lw.err(line, "close の引数は 0 個のはず");
            }
            (Op::FsClose { fd }, fs_err_ty(lw, Ty::Unit))
        }
        "flush" => {
            if !args.is_empty() {
                return lw.err(line, "flush の引数は 0 個のはず");
            }
            (Op::FsFlush { fd }, fs_err_ty(lw, Ty::Unit))
        }
        "read" => {
            if args.len() != 1 {
                return lw.err(line, "read の引数は 1 個のはず");
            }
            let Some(buf) = lw.expr(&args[0])? else { return Ok(None) };
            let (ptr, len) = fs_buf_ptr_len(lw, buf, line)?;
            (Op::FsRead { fd, buf: ptr, len }, fs_err_ty(lw, Ty::Usize))
        }
        "sync" => {
            if !args.is_empty() {
                return lw.err(line, "sync の引数は 0 個のはず");
            }
            (Op::FsSync { fd }, fs_err_ty(lw, Ty::Unit))
        }
        "write" => {
            if args.len() != 1 {
                return lw.err(line, "write の引数は 1 個のはず");
            }
            let Some(buf) = lw.expr(&args[0])? else { return Ok(None) };
            let (ptr, len) = fs_buf_ptr_len(lw, buf, line)?;
            (Op::FsWrite { fd, buf: ptr, len }, fs_err_ty(lw, Ty::Usize))
        }
        _ => return Ok(None),
    };
    Ok(Some(lw.emit(res_ty, op)))
}

/// 型 `rt` が `std.fs.File` (裸または参照) か。
pub(super) fn is_fs_ty(lw: &Lw, rt: Ty) -> bool {
    match rt {
        Ty::Agg(ai) | Ty::Ref(_, Pointee::Agg(ai)) => {
            matches!(
                lw.aggs.borrow().defs[ai as usize].name.as_str(),
                "std.fs.File"
            )
        }
        _ => false,
    }
}
