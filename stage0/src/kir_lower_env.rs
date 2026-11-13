//! `std.env` (引数・環境変数) の KIR への下ろし。
//!
//! - `std.env.arg_count()`: 引数の個数。結果は `usize`。
//! - `std.env.arg(i)`: 第 `i` 個目の引数。結果は `Option<&'static str>` (`Option<fat>`)。
//! - `std.env.var(name)`: 環境変数の値。結果は `Option<&'static str>` (`Option<fat>`)。

use crate::kir::{Op, Pay, Ty, VId};
use crate::parser::V;

use super::{Lw, LR};

/// `std.env.arg_count` / `std.env.arg` / `std.env.var` (フリー関数)。
pub(super) fn env_call(lw: &mut Lw, _e: &V, name: &str, args: &[V], line: usize) -> LR<Option<VId>> {
    let fat = lw.fat_ty();
    let opt_fat = Ty::Opt(Pay::Agg(match fat {
        Ty::Agg(a) => a,
        _ => return lw.err(line, "fat 型が見つからない"),
    }));
    let op = match name {
        "std.env.arg_count" => {
            if !args.is_empty() {
                return lw.err(line, "std.env.arg_count の引数は 0 個のはず");
            }
            Op::EnvArgCount
        }
        "std.env.arg" => {
            if args.len() != 1 {
                return lw.err(line, "std.env.arg の引数は 1 個のはず");
            }
            let Some(index) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(index) != Ty::Usize {
                return lw.err(line, "std.env.arg の引数は usize ではない");
            }
            Op::EnvArg { index }
        }
        "std.env.var" => {
            if args.len() != 1 {
                return lw.err(line, "std.env.var の引数は 1 個のはず");
            }
            let Some(name_v) = lw.expr(&args[0])? else { return Ok(None) };
            if lw.vty(name_v) != fat {
                return lw.err(line, "std.env.var の引数は &str ではない");
            }
            Op::EnvVar { name: name_v }
        }
        _ => return Ok(None),
    };
    let res_ty = match name {
        "std.env.arg_count" => Ty::Usize,
        "std.env.arg" | "std.env.var" => opt_fat,
        _ => return Ok(None),
    };
    Ok(Some(lw.emit(res_ty, op)))
}
