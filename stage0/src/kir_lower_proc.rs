//! `std.process` (外部コマンドの実行) の KIR への下ろし。
//!
//! - `Command` = `{ handle: i32 }` (実行環境側の設定表のハンドル)、`Child` = `{ pid, stdin, stdout, stderr }` (4 つの i32)、
//!   `ExitStatus` = `{ code, signal }` (2 つの i32)。
//! - 設定の追加 (`arg` `env` `stdin_piped` ほか)・`spawn`・`cancel`・`wait`・`kill` は `Op::Proc` 1 つに下ろす (`ProcFn`)。
//! - パイプの読み書きは `fs.read` / `fs.write` / `fs.close` をそのまま使う (記述子は `Child` のフィールド)。
//! - `output(out, err)`: `proc.output` (標準出力・標準エラーを一時ファイルに受けて、待って、巻き戻す) のあと、
//!   2 つの記述子を `read_fd_to_end` で `Vec<u8>` に読み、閉じる。

use crate::kir::{Op, Pay, Pointee, ProcFn, Ty, VId};
use crate::parser::V;
use crate::program::Target as Tgt;

use super::fs_lower::{fs_buf_ptr_len, fs_err_ty, read_fd_to_end};
use super::{Lw, LR};

fn proc_agg(lw: &mut Lw, name: &str, line: usize) -> LR<u32> {
    if let Some(i) = lw.aggs.borrow().agg_index(name) {
        return Ok(i);
    }
    let segs: Vec<String> = name.split('.').map(|s| s.to_string()).collect();
    let Some(r) = lw.p.resolve(&segs, &lw.def.file) else {
        return lw.err(line, format!("{} の定義がない", name));
    };
    let Tgt::Type(di) = r.target else {
        return lw.err(line, format!("{} が型ではない", name));
    };
    let t = lw.aggs.borrow_mut().intern_def(lw.p, di);
    match t {
        Ok(Ty::Agg(i)) => Ok(i),
        _ => lw.err(line, format!("{} の KIR 型表登録に失敗", name)),
    }
}

fn io_pay(lw: &mut Lw) -> Pay {
    Pay::of(lw.aggs.borrow_mut().io_error(lw.p).expect("std.io.Error")).expect("ペイロード")
}

/// 型 `rt` が `std.process.Command` / `Child` (裸または参照) か。
pub(super) fn is_proc_ty(lw: &Lw, rt: Ty) -> bool {
    match rt {
        Ty::Agg(ai) | Ty::Ref(_, Pointee::Agg(ai)) => {
            matches!(lw.aggs.borrow().defs[ai as usize].name.as_str(), "std.process.Command" | "std.process.Child")
        }
        _ => false,
    }
}

/// 受け手の第 `idx` フィールド (i32、`4 * idx` バイト目) を読む。
fn field_i32(lw: &mut Lw, rv: VId, rt: Ty, idx: u32, line: usize) -> LR<VId> {
    let off = 4 * idx;
    Ok(match rt {
        Ty::Agg(_) => lw.emit(Ty::I32, Op::Field { a: rv, off }),
        Ty::Ref(m, Pointee::Agg(_)) => {
            let p = lw.emit(Ty::Ref(m, Pointee::I32), Op::FieldAddr { a: rv, off });
            lw.emit(Ty::I32, Op::Load(p))
        }
        _ => return lw.err(line, format!("process の受け手の型 {} は対象外", rt.name())),
    })
}

fn fat_arg(lw: &mut Lw, a: &V, what: &str, line: usize) -> LR<Option<VId>> {
    let Some(v) = lw.expr(a)? else { return Ok(None) };
    if lw.vty(v) != lw.fat_ty() {
        return lw.err(line, format!("{} は &str のはず", what));
    }
    Ok(Some(v))
}

/// `std.process.Command.new(program)`。
pub(super) fn proc_call(lw: &mut Lw, args: &[V], line: usize) -> LR<Option<VId>> {
    if args.len() != 1 {
        return lw.err(line, "std.process.Command.new の引数は 1 個のはず");
    }
    let cmd = proc_agg(lw, "std.process.Command", line)?;
    let Some(p) = fat_arg(lw, &args[0], "std.process.Command.new の引数", line)? else { return Ok(None) };
    let h = lw.emit(Ty::I32, Op::Proc { f: ProcFn::New, args: vec![p] });
    Ok(Some(lw.emit(Ty::Agg(cmd), Op::Make { variant: None, fields: vec![h] })))
}

/// `std.process.Command` / `Child` のメソッド。
pub(super) fn proc_method(lw: &mut Lw, name: &str, recv: VId, rt: Ty, args: &[V], line: usize) -> LR<Option<VId>> {
    let is_cmd = match rt {
        Ty::Agg(ai) | Ty::Ref(_, Pointee::Agg(ai)) => lw.aggs.borrow().defs[ai as usize].name == "std.process.Command",
        _ => false,
    };
    let want = |lw: &mut Lw, n: usize| -> LR<()> {
        if args.len() != n {
            return lw.err(line, format!("{} の引数は {} 個のはず", name, n));
        }
        Ok(())
    };
    if is_cmd {
        let h = field_i32(lw, recv, rt, 0, line)?;
        let simple = match name {
            "arg" => Some((ProcFn::Arg, 1)),
            "env_remove" => Some((ProcFn::EnvRemove, 1)),
            "current_dir" => Some((ProcFn::Dir, 1)),
            "env" => Some((ProcFn::Env, 2)),
            "env_clear" => Some((ProcFn::EnvClear, 0)),
            "stdin_piped" => Some((ProcFn::InPiped, 0)),
            "stdin_null" => Some((ProcFn::InNull, 0)),
            "stdout_piped" => Some((ProcFn::OutPiped, 0)),
            "stdout_null" => Some((ProcFn::OutNull, 0)),
            "stderr_piped" => Some((ProcFn::ErrPiped, 0)),
            "stderr_null" => Some((ProcFn::ErrNull, 0)),
            "cancel" => Some((ProcFn::Cancel, 0)),
            _ => None,
        };
        if let Some((f, n)) = simple {
            want(lw, n)?;
            let mut a = vec![h];
            for x in args {
                let Some(v) = fat_arg(lw, x, "引数", line)? else { return Ok(None) };
                a.push(v);
            }
            return Ok(Some(lw.emit(Ty::Unit, Op::Proc { f, args: a })));
        }
        let child = proc_agg(lw, "std.process.Child", line)?;
        let io = io_pay(lw);
        match name {
            "spawn" => {
                want(lw, 0)?;
                return Ok(Some(lw.emit(Ty::Res(Pay::Agg(child), io), Op::Proc { f: ProcFn::Spawn, args: vec![h] })));
            }
            "output" => {
                want(lw, 2)?;
                return output(lw, h, child, io, args, line);
            }
            _ => return Ok(None),
        }
    }
    // Child
    let io_unit = fs_err_ty(lw, Ty::Unit);
    match name {
        "id" => {
            want(lw, 0)?;
            Ok(Some(field_i32(lw, recv, rt, 0, line)?))
        }
        "kill" => {
            want(lw, 0)?;
            let pid = field_i32(lw, recv, rt, 0, line)?;
            Ok(Some(lw.emit(io_unit, Op::Proc { f: ProcFn::Kill, args: vec![pid] })))
        }
        "close_stdin" => {
            want(lw, 0)?;
            let fd = field_i32(lw, recv, rt, 1, line)?;
            let r = lw.emit(io_unit, Op::FsClose { fd });
            // 二重に閉じない (記述子の番号は再利用されうる): フィールドを -1 にする。
            if let Ty::Ref(true, Pointee::Agg(_)) = rt {
                let p = lw.emit(Ty::Ref(true, Pointee::I32), Op::FieldAddr { a: recv, off: 4 });
                let m1 = lw.emit(Ty::I32, Op::Const(-1));
                lw.emit(Ty::Unit, Op::Store { ptr: p, val: m1 });
            }
            Ok(Some(r))
        }
        "write_stdin" | "read_stdout" | "read_stderr" => {
            want(lw, 1)?;
            let idx = match name {
                "write_stdin" => 1,
                "read_stdout" => 2,
                _ => 3,
            };
            let fd = field_i32(lw, recv, rt, idx, line)?;
            let Some(buf) = lw.expr(&args[0])? else { return Ok(None) };
            let (ptr, len) = fs_buf_ptr_len(lw, buf, line)?;
            let res = fs_err_ty(lw, Ty::Usize);
            let op = if name == "write_stdin" { Op::FsWrite { fd, buf: ptr, len } } else { Op::FsRead { fd, buf: ptr, len } };
            Ok(Some(lw.emit(res, op)))
        }
        "wait" => {
            want(lw, 0)?;
            let status = proc_agg(lw, "std.process.ExitStatus", line)?;
            let io = io_pay(lw);
            let pid = field_i32(lw, recv, rt, 0, line)?;
            // 標準入力を閉じてから待つ (子が EOF を待っていても終われる)。残りは待ったあとに閉じる。
            let fin = field_i32(lw, recv, rt, 1, line)?;
            lw.emit(io_unit, Op::FsClose { fd: fin });
            let r = lw.emit(Ty::Res(Pay::Agg(status), io), Op::Proc { f: ProcFn::Wait, args: vec![pid] });
            for idx in [2, 3] {
                let fd = field_i32(lw, recv, rt, idx, line)?;
                lw.emit(io_unit, Op::FsClose { fd });
            }
            Ok(Some(r))
        }
        _ => Ok(None),
    }
}

/// `Command.output(out, err)`: 一時ファイルに受けた標準出力・標準エラーを 2 つの `Vec<u8>` へ読む。
fn output(lw: &mut Lw, h: VId, child: u32, io: Pay, args: &[V], line: usize) -> LR<Option<VId>> {
    let status = proc_agg(lw, "std.process.ExitStatus", line)?;
    let Some(out) = lw.expr(&args[0])? else { return Ok(None) };
    let Some(er) = lw.expr(&args[1])? else { return Ok(None) };
    if lw.vty(out) != Ty::Ptr || lw.vty(er) != Ty::Ptr {
        return lw.err(line, "std.process.Command.output の出力がヘッダのアドレスでない (Vec<u8> の &mut のはず)");
    }
    let res_ty = Ty::Res(Pay::Agg(status), io);
    let ioty = io.ty();
    let io_unit = fs_err_ty(lw, Ty::Unit);
    let r = lw.emit(Ty::Res(Pay::Agg(child), io), Op::Proc { f: ProcFn::Output, args: vec![h] });
    let (exit, xp) = lw.new_block(&[res_ty]);
    let is_err = lw.emit(Ty::Bool, Op::Tag(r));
    let (on_err, _) = lw.new_block(&[]);
    let (on_ok, _) = lw.new_block(&[]);
    lw.branch(is_err, (on_err, vec![]), (on_ok, vec![]));
    lw.cur = Some(on_err);
    let e = lw.emit(ioty, Op::Payload(r));
    let er_res = lw.emit(res_ty, Op::Err(e));
    lw.goto(exit, vec![er_res]);
    lw.cur = Some(on_ok);
    let c = lw.emit(Ty::Agg(child), Op::Payload(r));
    let code = lw.emit(Ty::I32, Op::Field { a: c, off: 0 });
    let sig = lw.emit(Ty::I32, Op::Field { a: c, off: 4 });
    let ofd = lw.emit(Ty::I32, Op::Field { a: c, off: 8 });
    let efd = lw.emit(Ty::I32, Op::Field { a: c, off: 12 });
    let r1 = read_fd_to_end(lw, ofd, out);
    let r2 = read_fd_to_end(lw, efd, er);
    lw.emit(io_unit, Op::FsClose { fd: ofd });
    lw.emit(io_unit, Op::FsClose { fd: efd });
    let t1 = lw.emit(Ty::Bool, Op::Tag(r1));
    let (bad1, _) = lw.new_block(&[]);
    let (next, _) = lw.new_block(&[]);
    lw.branch(t1, (bad1, vec![]), (next, vec![]));
    lw.cur = Some(bad1);
    let e1 = lw.emit(ioty, Op::Payload(r1));
    let x1 = lw.emit(res_ty, Op::Err(e1));
    lw.goto(exit, vec![x1]);
    lw.cur = Some(next);
    let t2 = lw.emit(Ty::Bool, Op::Tag(r2));
    let (bad2, _) = lw.new_block(&[]);
    let (fine, _) = lw.new_block(&[]);
    lw.branch(t2, (bad2, vec![]), (fine, vec![]));
    lw.cur = Some(bad2);
    let e2 = lw.emit(ioty, Op::Payload(r2));
    let x2 = lw.emit(res_ty, Op::Err(e2));
    lw.goto(exit, vec![x2]);
    lw.cur = Some(fine);
    let st = lw.emit(Ty::Agg(status), Op::Make { variant: None, fields: vec![code, sig] });
    let okv = lw.emit(res_ty, Op::Ok(st));
    lw.goto(exit, vec![okv]);
    lw.cur = Some(exit);
    Ok(Some(xp[0]))
}
