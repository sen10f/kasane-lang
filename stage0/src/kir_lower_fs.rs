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
//! `std.fs.read_to_end(&mut file, &mut v)` (`v: Vec<u8>`) は、新しい命令を作らず `fs.read` を繰り返して
//! ヘッダの末尾へ足す (`read_to_end`)。Vec の再確保は `kir_lower_own` の `reserve` (`push` と同じ規則) を使う。

use crate::kir::{Aop, Cc, FsK, Mode, Op, Pay, Pointee, Ty, VId};
use crate::parser::V;
use crate::program::Target as Tgt;

use super::{Lw, LR};

/// `std.io.Error` 集成体をペイロードに持つ `Result<payload, std.io.Error>`。
pub(super) fn fs_err_ty(lw: &mut Lw, payload: Ty) -> Ty {
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
pub(super) fn fs_buf_ptr_len(lw: &mut Lw, buf: VId, line: usize) -> LR<(VId, VId)> {
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

/// `std.fs.read_to_end(&mut file, &mut v)` の下ろし (木走査の `Read::read_to_end` と同じ値)。
///
/// 1 周: `reserve` で残りを 4096 バイト以上にし、残りの容量すべてを `fs.read` で読む。0 バイトなら終わりで `Ok(総数)`。
/// 失敗したら、その周までに足したバイトを捨てて (長さを元に戻す。木走査は失敗時に `out` を変えない) `Err` をそのまま返す。
/// 成功時は読んだ総数を `Ok` で返し、`v` には元の末尾の後ろへ足したバイトが残る。
fn read_to_end(lw: &mut Lw, args: &[V], line: usize) -> LR<Option<VId>> {
    if args.len() != 2 {
        return lw.err(line, "std.fs.read_to_end の引数は 2 個のはず");
    }
    let Some(fv) = lw.expr(&args[0])? else { return Ok(None) };
    let ft = lw.vty(fv);
    let fd = fs_fd(lw, fv, ft, line)?;
    let Some(h) = lw.expr(&args[1])? else { return Ok(None) };
    if lw.vty(h) != Ty::Ptr {
        return lw.err(line, "std.fs.read_to_end の出力がヘッダのアドレスでない (Vec<u8> の &mut のはず)");
    }
    Ok(Some(read_fd_to_end(lw, fd, h)))
}

/// 記述子 `fd` を終端まで読んで、ヘッダ `h` の `Vec<u8>` の末尾へ足す (`read_to_end` の本体。`std.process.Command.output` も使う)。
/// 結果は `Result<usize, std.io.Error>` の値。
pub(super) fn read_fd_to_end(lw: &mut Lw, fd: VId, h: VId) -> VId {
    let res_ty = fs_err_ty(lw, Ty::Usize);
    let orig = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let zero = lw.emit(Ty::Usize, Op::Const(0));
    let (hd, hp) = lw.new_block(&[Ty::Usize]);
    let (exit, xp) = lw.new_block(&[res_ty]);
    lw.goto(hd, vec![zero]);
    lw.cur = Some(hd);
    let total = hp[0];
    let chunk = lw.emit(Ty::Usize, Op::Const(4096));
    super::own::reserve(lw, h, chunk);
    let base = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let cap = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 });
    let dst = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off: len });
    let room = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: cap, b: len });
    let r = lw.emit(res_ty, Op::FsRead { fd, buf: dst, len: room });
    let is_err = lw.emit(Ty::Bool, Op::Tag(r));
    let (on_err, _) = lw.new_block(&[]);
    let (on_ok, _) = lw.new_block(&[]);
    lw.branch(is_err, (on_err, vec![]), (on_ok, vec![]));
    // 失敗: この呼び出しで足したバイトを捨てて、読み出しの Err をそのまま返す。
    lw.cur = Some(on_err);
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: orig });
    lw.goto(exit, vec![r]);
    // 成功: 0 バイトなら終わり。そうでなければ長さを進めて次の周へ。
    lw.cur = Some(on_ok);
    let n = lw.emit(Ty::Usize, Op::Payload(r));
    let zn = lw.emit(Ty::Usize, Op::Const(0));
    let at_end = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: n, b: zn });
    let (done, _) = lw.new_block(&[]);
    let (more, _) = lw.new_block(&[]);
    lw.branch(at_end, (done, vec![]), (more, vec![]));
    lw.cur = Some(done);
    let ok = lw.emit(res_ty, Op::Ok(total));
    lw.goto(exit, vec![ok]);
    lw.cur = Some(more);
    let nl = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: len, b: n });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: nl });
    let nt = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: total, b: n });
    lw.goto(hd, vec![nt]);
    lw.cur = Some(exit);
    xp[0]
}


/// `std.fs.read_dir_raw(path, &mut v)` (`v: Vec<u8>`) の下ろし。`fs.read_dir` を、必要な容量が足りるまで `reserve` しながら呼び直す。
/// 失敗したら `v` を変えずに `Err` を返す。成功したら `v` の末尾へ `[種別][名前][0]` の並びを足し、足したバイト数を `Ok` で返す。
fn read_dir_raw(lw: &mut Lw, args: &[V], line: usize) -> LR<Option<VId>> {
    if args.len() != 2 {
        return lw.err(line, "std.fs.read_dir_raw の引数は 2 個のはず");
    }
    let Some(path) = lw.expr(&args[0])? else { return Ok(None) };
    if lw.vty(path) != lw.fat_ty() {
        return lw.err(line, "std.fs.read_dir_raw のパスは &str ではない");
    }
    let Some(h) = lw.expr(&args[1])? else { return Ok(None) };
    if lw.vty(h) != Ty::Ptr {
        return lw.err(line, "std.fs.read_dir_raw の出力がヘッダのアドレスでない (Vec<u8> の &mut のはず)");
    }
    let res_ty = fs_err_ty(lw, Ty::Usize);
    let first = lw.emit(Ty::Usize, Op::Const(4096));
    let (hd, hp) = lw.new_block(&[Ty::Usize]);
    let (exit, xp) = lw.new_block(&[res_ty]);
    lw.goto(hd, vec![first]);
    lw.cur = Some(hd);
    let need = hp[0];
    super::own::reserve(lw, h, need);
    let base = lw.emit(Ty::Ptr, Op::MemLoad { p: h, off: 0 });
    let len = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 8 });
    let cap = lw.emit(Ty::Usize, Op::MemLoad { p: h, off: 16 });
    let dst = lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off: len });
    let room = lw.emit(Ty::Usize, Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: cap, b: len });
    let r = lw.emit(res_ty, Op::FsSys { kind: FsK::ReadDir, args: vec![path, dst, room] });
    let is_err = lw.emit(Ty::Bool, Op::Tag(r));
    let (on_err, _) = lw.new_block(&[]);
    let (on_ok, _) = lw.new_block(&[]);
    lw.branch(is_err, (on_err, vec![]), (on_ok, vec![]));
    lw.cur = Some(on_err);
    lw.goto(exit, vec![r]);
    lw.cur = Some(on_ok);
    let n = lw.emit(Ty::Usize, Op::Payload(r));
    let fits = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Le, ty: Ty::Usize, a: n, b: room });
    let (done, _) = lw.new_block(&[]);
    let (again, _) = lw.new_block(&[]);
    lw.branch(fits, (done, vec![]), (again, vec![]));
    lw.cur = Some(done);
    let nl = lw.emit(Ty::Usize, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: len, b: n });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: h, off: 8, v: nl });
    let ok = lw.emit(res_ty, Op::Ok(n));
    lw.goto(exit, vec![ok]);
    lw.cur = Some(again);
    lw.goto(hd, vec![n]);
    lw.cur = Some(exit);
    Ok(Some(xp[0]))
}

/// `std.fs.create` / `std.fs.open` / `std.fs.exists` / `std.fs.remove_file` / `std.fs.read_to_end` (フリー関数)。
pub(super) fn fs_call(lw: &mut Lw, _e: &V, name: &str, args: &[V], line: usize) -> LR<Option<VId>> {
    let segs: Vec<String> = name.split('.').map(|s| s.to_string()).collect();
    let Some(r) = lw.p.lookup_fqn(&segs.join(".")) else {
        return lw.err(line, "std.fs.create が見つからない");
    };
    if !matches!(r, Tgt::Fn(_) | Tgt::Sig(_) | Tgt::Native(_)) {
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
        let Some(r) = lw.p.lookup_fqn(&segs.join(".")) else {
            return lw.err(line, "std.fs.File の定義がない");
        };
        let Tgt::Type(di) = r else {
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
        "std.fs.read_dir_raw" => return read_dir_raw(lw, args, line),
        "std.fs.rename" | "std.fs.create_dir" | "std.fs.remove_dir" | "std.fs.open_append" | "std.fs.stat_raw" => {
            let kind = match name {
                "std.fs.rename" => FsK::Rename,
                "std.fs.create_dir" => FsK::CreateDir,
                "std.fs.remove_dir" => FsK::RemoveDir,
                "std.fs.open_append" => FsK::OpenAppend,
                _ => FsK::Stat,
            };
            if args.len() != kind.arity() {
                return lw.err(line, format!("{} の引数は {} 個のはず", name, kind.arity()));
            }
            let mut vs = Vec::new();
            for a in args {
                let Some(v) = lw.expr(a)? else { return Ok(None) };
                vs.push(v);
            }
            let npaths = if matches!(kind, FsK::Rename) { 2 } else { 1 };
            for v in &vs[..npaths] {
                if lw.vty(*v) != lw.fat_ty() {
                    return lw.err(line, format!("{} のパスは &str ではない", name));
                }
            }
            let rt = match kind {
                FsK::OpenAppend => Ty::Res(Pay::Agg(file), ioerr),
                FsK::Stat => fs_err_ty(lw, Ty::U64),
                _ => fs_err_ty(lw, Ty::Unit),
            };
            (Op::FsSys { kind, args: vs }, rt)
        }
        "std.fs.read_to_end" => return read_to_end(lw, args, line),
        "std.fs.open_dir_raw" | "std.fs.Dir.open" => {
            if args.len() != 1 {
                return lw.err(line, "std.fs.open_dir_raw の引数は 1 個のはず");
            }
            let dir = fs_agg_index(lw, "std.fs.Dir", line)?;
            let Some(path) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(path) != lw.fat_ty() {
                return lw.err(line, "std.fs.open_dir_raw のパスは &str ではない");
            }
            (Op::FsSys { kind: FsK::OpenDir, args: vec![path] }, Ty::Res(Pay::Agg(dir), ioerr))
        }
        "std.fs.open_beneath_raw" => {
            if args.len() != 2 {
                return lw.err(line, "std.fs.open_beneath_raw の引数は 2 個のはず");
            }
            let Some(dv) = lw.expr(&args[0])? else { return Ok(None) };
            let dt = lw.vty(dv);
            if !is_dir_ty(lw, dt) {
                return lw.err(line, "std.fs.open_beneath_raw の受け手は &std.fs.Dir のはず");
            }
            let dfd = fs_fd(lw, dv, dt, line)?;
            let Some(rel) = lw.expr(&args[1])? else { return Ok(None) };
            if lw.vty(rel) != lw.fat_ty() {
                return lw.err(line, "std.fs.open_beneath_raw の相対パスは &str ではない");
            }
            (Op::FsSys { kind: FsK::OpenBeneath, args: vec![dfd, rel] }, Ty::Res(Pay::Agg(file), ioerr))
        }
        "std.fs.stat_fd_raw" => {
            if args.len() != 2 {
                return lw.err(line, "std.fs.stat_fd_raw の引数は 2 個のはず");
            }
            let Some(fv) = lw.expr(&args[0])? else { return Ok(None) };
            let ft = lw.vty(fv);
            let ffd = fs_fd(lw, fv, ft, line)?;
            let Some(field) = lw.expr(&args[1])? else { return Ok(None) };
            if lw.vty(field) != Ty::U32 {
                return lw.err(line, "std.fs.stat_fd_raw の field は u32 のはず");
            }
            (Op::FsSys { kind: FsK::FStat, args: vec![ffd, field] }, fs_err_ty(lw, Ty::U64))
        }
        _ => return Ok(None),
    };
    Ok(Some(lw.emit(res_ty, op)))
}

/// `std.fs.Dir` (ディレクトリの取っ手) の KIR 集成体添字。型表にまだ無ければ登録する。
fn fs_agg_index(lw: &mut Lw, name: &str, line: usize) -> LR<u32> {
    if let Some(i) = lw.aggs.borrow().agg_index(name) {
        return Ok(i);
    }
    let segs: Vec<String> = name.split('.').map(|s| s.to_string()).collect();
    let Some(r) = lw.p.lookup_fqn(&segs.join(".")) else {
        return lw.err(line, format!("{} の定義がない", name));
    };
    let Tgt::Type(di) = r else {
        return lw.err(line, format!("{} が型ではない", name));
    };
    match lw.aggs.borrow_mut().intern_def(lw.p, di) {
        Ok(Ty::Agg(i)) => Ok(i),
        _ => lw.err(line, format!("{} の KIR 型表登録に失敗", name)),
    }
}

/// 型 `rt` が `std.fs.Dir` (裸または参照) か。
fn is_dir_ty(lw: &Lw, rt: Ty) -> bool {
    match rt {
        Ty::Agg(ai) | Ty::Ref(_, Pointee::Agg(ai)) => lw.aggs.borrow().defs[ai as usize].name == "std.fs.Dir",
        _ => false,
    }
}

/// `std.fs.File` のメソッド `close`/`flush`/`read`/`sync`/`write`。
pub(super) fn fs_method(lw: &mut Lw, _e: &V, name: &str, recv: VId, rt: Ty, args: &[V], line: usize) -> LR<Option<VId>> {
    let fd = fs_fd(lw, recv, rt, line)?;
    // 標準入出力は、ネイティブでは利用者側の溜めを持たない。`flush` は何もしない (`fsync` は管や端末で失敗する)。
    let is_std = match rt {
        Ty::Agg(ai) | Ty::Ref(_, Pointee::Agg(ai)) => lw.aggs.borrow().defs[ai as usize].name.starts_with("std.io."),
        _ => false,
    };
    if is_std && name == "flush" {
        let u = lw.emit(Ty::Unit, Op::Const(0));
        let res_ty = fs_err_ty(lw, Ty::Unit);
        return Ok(Some(lw.emit(res_ty, Op::Ok(u))));
    }
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
        "open_file_beneath" => {
            if args.len() != 1 {
                return lw.err(line, "open_file_beneath の引数は 1 個のはず");
            }
            let file = fs_agg_index(lw, "std.fs.File", line)?;
            let Some(rel) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(rel) != lw.fat_ty() {
                return lw.err(line, "open_file_beneath の相対パスは &str ではない");
            }
            let ioerr = Pay::of(lw.aggs.borrow_mut().io_error(lw.p).expect("std.io.Error")).expect("ペイロード");
            (Op::FsSys { kind: FsK::OpenBeneath, args: vec![fd, rel] }, Ty::Res(Pay::Agg(file), ioerr))
        }
        // 木走査だけの対応 (ネイティブの下ろしは未実装。`ksn build` は退避か、release では失敗する)。
        "metadata" | "read_to_end_limited" => {
            return lw.err(line, format!("{} はネイティブでは未対応 (木走査 `ksn run` だけ)", name));
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
                "std.fs.File" | "std.fs.Dir" | "std.io.Stdin" | "std.io.Stdout" | "std.io.Stderr"
            )
        }
        _ => false,
    }
}

/// `std.io.stdin()` / `stdout()` / `stderr()`: 記述子 0 / 1 / 2 だけを持つ構造体。
pub(super) fn std_stream_call(lw: &mut Lw, name: &str, args: &[V], line: usize) -> LR<Option<VId>> {
    if !args.is_empty() {
        return lw.err(line, format!("{} の引数は 0 個のはず", name));
    }
    let (ty, fd) = match name {
        "std.io.stdin" => ("std.io.Stdin", 0),
        "std.io.stdout" => ("std.io.Stdout", 1),
        _ => ("std.io.Stderr", 2),
    };
    let segs: Vec<String> = ty.split('.').map(|s| s.to_string()).collect();
    let Some(r) = lw.p.lookup_fqn(&segs.join(".")) else {
        return lw.err(line, format!("{} の定義がない", ty));
    };
    let Tgt::Type(di) = r else {
        return lw.err(line, format!("{} が型ではない", ty));
    };
    let agg = match lw.aggs.borrow_mut().intern_def(lw.p, di) {
        Ok(Ty::Agg(i)) => i,
        _ => return lw.err(line, format!("{} の KIR 型表登録に失敗", ty)),
    };
    let f = lw.emit(Ty::I32, Op::Const(fd));
    Ok(Some(lw.emit(Ty::Agg(agg), Op::Make { variant: None, fields: vec![f] })))
}
