//! レジスタ型バイトコード VM (設計_KIR.md 4 章、段階 2 の第 1 段)。`ksn run --engine=vm` / `ksn test --engine=vm` でだけ使う。
//!
//! - `bc.rs`: 命令とスロット、`compile.rs`: KIR → バイトコード、`run.rs`: 実行ループと明示フレーム。
//! - 入口から届く関数のどれかが KIR に下ろせない (または VM が扱えない命令を含む) ときは、理由を返して呼び出し側が木走査へ退避する。

pub mod arena;
pub mod bc;
pub mod compile;
pub mod reload;
pub mod run;

use crate::kir::Ty;
use crate::kir_exec::Outcome;
use crate::kir_lower::{self, Lowered};
use crate::program::Program;

/// `KSN_TIMING=1` のとき、直前の `tick` からの経過を stderr に出す (起動時間の内訳の計測用)。
pub fn tick(label: &str) {
    use std::sync::Mutex;
    use std::time::Instant;
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    if std::env::var_os("KSN_TIMING").is_none() {
        return;
    }
    let now = Instant::now();
    let mut g = LAST.lock().unwrap();
    if let Some(t) = *g {
        eprintln!("[timing] {:<12} {:>7.2} ms", label, (now - t).as_secs_f64() * 1000.0);
    }
    *g = Some(now);
}

/// 入口 `entry` が下ろせなかった理由 (`呼び先 X が未対応` の鎖をたどった根の理由)。
fn root_reason(lo: &Lowered, entry: &str) -> String {
    let mut key = entry.to_string();
    let mut why = String::from("理由不明");
    for _ in 0..64 {
        let Some((_, w)) = lo.unsupported.iter().find(|(k, _)| *k == key) else { break };
        why = format!("{}: {}", key, w);
        match w.strip_prefix("呼び先 ").and_then(|r| r.strip_suffix(" が未対応")) {
            Some(next) => key = next.to_string(),
            None => break,
        }
    }
    format!("{} を KIR に下ろせない ({})", entry, why)
}

/// 入口 `entry` から届く関数を KIR に下ろしてバイトコードにする。
fn prepare(lo: Lowered, entry: &str) -> Result<bc::Prog, String> {
    if lo.module.find(entry).is_none() {
        return Err(root_reason(&lo, entry));
    }
    let m = lo.module.reachable_from(&[entry, kir_lower::debug::ENTRY_DEBUG]);
    tick("reachable");
    // `std.tls` (OpenSSL) は実際の記述子を要るが、VM のソケットは仮想の記述子なので走らせない。
    if m.funcs.iter().any(|f| f.link.as_deref().map_or(false, |l| l.starts_with("libssl.so") || l.starts_with("libcrypto.so"))) {
        return Err("std.tls は VM 未対応".into());
    }
    // `std.task.block_on_threads` は複数のワーカーを前提にした意味 (取り消し・入れ子・panic の伝わり方) を持つ。VM の実行器は 1 本なので走らせない (木走査へ退避する)。
    if m.find("ksn.task.mt_begin").is_some() {
        return Err("std.task.block_on_threads は VM 未対応".into());
    }
    let errs = crate::kir::verify_module(&m);
    tick("verify");
    if let Some(e) = errs.first() {
        return Err(format!("KIR の検査に失敗: {} {}", e.0, e.1));
    }
    let r = compile::compile(&m, &lo.lines);
    tick("compile");
    // `KSN_VM_DUMP=<部分文字列>`: 鍵が一致する関数のバイトコードを stderr に出す (調査用)。
    if let (Ok(p), Ok(filter)) = (&r, std::env::var("KSN_VM_DUMP")) {
        for f in p.funcs.iter().filter(|f| f.key.contains(&filter)) {
            eprintln!("== {} (nslots {}, ret_w {})", f.key, f.nslots, f.ret_w);
            for (i, ins) in f.code.iter().enumerate() {
                eprintln!("{:4} {:?}", i, ins);
            }
        }
    }
    r
}

/// `ksn run --engine=vm`: 入口を VM で実行して終了コードを返す。実行を始める前に分かる理由 (下ろせない・VM が扱えない) は `Err`。
pub fn run_entry(prog: &Program, user: &[usize], entry: &str, args: &[String], watch: Option<Box<dyn FnOnce() -> reload::Reload + '_>>) -> Result<i32, String> {
    let lo = kir_lower::lower_program_lazy(prog, user, entry);
    tick("lower");
    let bc = prepare(lo, entry)?;
    // 戻り値の型は付け替え後の型表 (`bc.aggs`) の添字で読む。`lo.module` の型は型表を縮める前の添字なので使えない。
    let ret = bc.by_key.get(entry).map(|&i| bc.funcs[i as usize].ret);
    let mut vm = run::Vm::new(&bc);
    vm.args = args.to_vec();
    vm.hook = watch.map(|mk| mk());
    tick("vm-start");
    let out = vm.run(entry, &[]);
    tick("vm-run");
    Ok(match out {
        Ok(Outcome::Value(_)) => match ret {
            // 入口が `Err(e)` を返したら、stderr に `Error: <e の Debug 表示>` を出して終了コード 1 (木走査の `run_entry` と同じ)。
            Some(Ty::Res(_, e)) if vm.result.first().copied().unwrap_or(0) != 0 => {
                vm.err_write(b"Error: ");
                let n = bc::slots_of(&bc.aggs, e.ty()) as usize;
                let payload: Vec<u64> = vm.result[1..1 + n].to_vec();
                match vm.run(kir_lower::debug::ENTRY_DEBUG, &payload) {
                    Ok(Outcome::Value(_)) => {}
                    other => {
                        vm.flush();
                        eprintln!("ksn: vm: 入口の Err の表示に失敗した: {:?}", other);
                    }
                }
                vm.err_write(b"\n");
                vm.flush();
                1
            }
            _ => 0,
        },
        Ok(Outcome::Panic(m)) => {
            vm.flush();
            eprintln!("panic: {}", m);
            // `KSN_PANIC_LOC=1`: panic の位置を、デバッガ (`ksn debug` の `stopped: panic`) と同じ形で添える。
            if std::env::var_os("KSN_PANIC_LOC").is_some() {
                if let Some(loc) = vm.panic_location() {
                    eprintln!("  {}", loc);
                }
            }
            101
        }
        Ok(Outcome::Abort) => {
            vm.flush();
            if std::env::var_os("KSN_PANIC_LOC").is_some() {
                if let Some(loc) = vm.panic_location() {
                    eprintln!("  {}", loc);
                }
            }
            101
        }
        Ok(Outcome::Exit(c)) => {
            vm.flush();
            c
        }
        Ok(Outcome::StackOverflow) => {
            vm.flush();
            eprintln!("stack overflow");
            102
        }
        Err(e) => {
            vm.flush();
            eprintln!("ksn: vm: 内部エラー: {}", e);
            3
        }
    })
}

/// `ksn test --engine=vm`: テスト `ti` を VM で実行する。結果は木走査と同じ形 (`None` = 成功、`Some(文言)` = 失敗)。
/// 下ろせない・VM が扱えないときは `Err(理由)` (呼び出し側が木走査で実行する)。
pub fn run_test(prog: &Program, user: &[usize], ti: usize) -> Result<Option<String>, String> {
    let name = prog.defs[ti].name.clone();
    let lo = kir_lower::lower_test_lazy(prog, user, ti);
    let bc = prepare(lo, &name)?;
    let mut vm = run::Vm::new(&bc);
    vm.capture = Some(vec![]);
    let out = vm.run(&name, &[]);
    vm.flush();
    let captured = vm.capture.take().unwrap_or_default();
    Ok(match out {
        Ok(Outcome::Value(_)) => None,
        Ok(Outcome::Panic(m)) => Some(m),
        // `core.check.eq` などは文言を `eprint` で書いてから終わる。書かれた文言 (`panic: ` と末尾の改行を除く) が失敗の理由。
        Ok(Outcome::Abort) => {
            let s = String::from_utf8_lossy(&captured).to_string();
            let s = s.trim_end_matches('\n');
            Some(s.strip_prefix("panic: ").unwrap_or(s).to_string())
        }
        Ok(Outcome::Exit(c)) => Some(format!("exit({})", c)),
        Ok(Outcome::StackOverflow) => {
            eprintln!("stack overflow");
            Some("exit(102)".to_string())
        }
        Err(e) => return Err(format!("実行中の内部エラー: {}", e)),
    })
}
