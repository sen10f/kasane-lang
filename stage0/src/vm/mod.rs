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
fn prepare(lo: &Lowered, entry: &str) -> Result<bc::Prog, String> {
    if lo.module.find(entry).is_none() {
        return Err(root_reason(&lo, entry));
    }
    let m = lo.module.reachable_from(&[entry, kir_lower::debug::ENTRY_DEBUG]);
    tick("reachable");
    // `std.tls` (OpenSSL) は実際の記述子を要るが、VM のソケットは仮想の記述子なので走らせない。
    if m.funcs.iter().any(|f| f.link.as_deref().map_or(false, |l| l.starts_with("libssl.so") || l.starts_with("libcrypto.so"))) {
        return Err("std.tls は VM 未対応".into());
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
    if has_unsafe_block(prog) {
        return Err(UNSAFE_REASON.into());
    }
    let lo = kir_lower::lower_program_lazy(prog, user, entry);
    tick("lower");
    let bc = prepare(&lo, entry)?;
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
fn has_unsafe(v: &crate::parser::V) -> bool {
    match v {
        crate::parser::V::Node(n, fs, _, _) => (*n == "Unsafe" && { let d = v.dump(); ["core ptr null", "core ptr cast", "core ptr add"].iter().any(|k| d.contains(k)) }) || fs.iter().any(has_unsafe),
        crate::parser::V::L(xs) => xs.iter().any(has_unsafe),
        _ => false,
    }
}

/// 利用者 (依存を含む) の `unsafe` ブロックがあるか。生ポインタの境界・null の検査は木走査が持つので、そのパッケージは木走査で走らせる。
fn has_unsafe_block(prog: &Program) -> bool {
    prog.defs.iter().any(|d| !crate::project::is_stdlib_file(&d.file) && d.body.as_ref().is_some_and(has_unsafe))
}

const UNSAFE_REASON: &str = "unsafe ブロックを含むパッケージは木走査で走らせる (null・cast・add の生ポインタの検査)";

/// 全テストを 1 回だけ下ろし、(`std.tls`・複数スレッドの実行器を含まなければ) バイトコードも 1 回だけ作る。
pub struct Suite {
    unsafe_pkg: bool,
    lo: Lowered,
    shared: Option<Result<bc::Prog, String>>,
    /// コンパイル直後の確保領域 (試験ごとに戻す。`static` の状態・回収されない確保を持ち越さない)。
    snap: Option<arena::Snapshot>,
}

pub fn lower_tests(prog: &Program, user: &[usize], tests: &[usize]) -> Suite {
    let lo = kir_lower::lower_tests_lazy(prog, user, tests);
    let names: Vec<&str> = tests.iter().map(|&t| prog.defs[t].name.as_str()).filter(|n| lo.module.find(n).is_some()).collect();
    let mut roots = names.clone();
    roots.push(kir_lower::debug::ENTRY_DEBUG);
    let m = lo.module.reachable_from(&roots);
    let special = m.funcs.iter().any(|f| f.link.as_deref().map_or(false, |l| l.starts_with("libssl.so") || l.starts_with("libcrypto.so"))) || m.find("ksn.task.mt_begin").is_some() || m.funcs.iter().any(|f| f.key.starts_with("std.net.") && f.key.contains("_async"));
    let shared = if names.is_empty() || special || !crate::kir::verify_module(&m).is_empty() { None } else { Some(compile::compile(&m, &lo.lines)) };
    let snap = arena::snapshot();
    Suite { unsafe_pkg: has_unsafe_block(prog), lo, shared, snap }
}

pub fn run_test(prog: &Program, suite: &Suite, ti: usize) -> Result<Option<String>, String> {
    if suite.unsafe_pkg {
        return Err(UNSAFE_REASON.into());
    }
    let name = prog.defs[ti].name.clone();
    let lo = &suite.lo;
    let own;
    let bc = match &suite.shared {
        Some(r) if lo.module.find(&name).is_some() => match r {
            Ok(b) => b,
            Err(e) => return Err(e.clone()),
        },
        _ => {
            own = prepare(lo, &name)?;
            &own
        }
    };
    if let Some(sn) = &suite.snap {
        arena::restore(sn);
    }
    let mut vm = run::Vm::new(bc);
    vm.capture = Some(vec![]);
    let out = vm.run(&name, &[]);
    vm.flush();
    let mut captured = vm.capture.take().unwrap_or_default();
    // 木走査と同じに、試験が書いた stderr はそのまま流し、失敗の理由には最後の `panic: ` 以降だけを使う。
    let mut tail = Vec::new();
    if matches!(out, Ok(Outcome::Abort)) {
        let pat = b"panic: ";
        if let Some(i) = (0..captured.len()).rev().find(|&i| captured[i..].starts_with(pat)) {
            tail = captured.split_off(i);
        } else {
            tail = std::mem::take(&mut captured);
        }
    }
    if !captured.is_empty() {
        let _ = crate::kir_exec::write_fd(2, &captured);
    }
    let captured = tail;
    drop(vm);
    if let Some(sn) = &suite.snap {
        arena::restore(sn);
    }
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

/// `ksn debug` / `ksn test --debug` の対象。
pub enum DebugTarget {
    Entry(String),
    /// テスト (定義の添字)。
    Test(usize),
}

/// `ksn debug` (VM 版): デバッグ用にコンパイルしたバイトコードを、デバッガをつけて走らせる。
pub fn debug(prog: &Program, user: &[usize], target: DebugTarget, args: &[String], json: bool, user_files: std::collections::HashSet<String>, dap: Option<run::dbg::DapSt>) -> Result<i32, String> {
    use std::collections::{BTreeSet, HashMap, HashSet};
    if has_unsafe_block(prog) {
        return Err(UNSAFE_REASON.into());
    }
    kir_lower::set_debug_vars(true);
    compile::set_debug(true);
    let (lo, name) = match &target {
        DebugTarget::Entry(e) => (kir_lower::lower_program_lazy(prog, user, e), e.clone()),
        DebugTarget::Test(ti) => (kir_lower::lower_test_lazy(prog, user, *ti), prog.defs[*ti].name.clone()),
    };
    let r = prepare(&lo, &name);
    kir_lower::set_debug_vars(false);
    compile::set_debug(false);
    let bc = r?;
    let ret = bc.by_key.get(&name).map(|&i| bc.funcs[i as usize].ret);
    let pure: HashSet<String> = prog.defs.iter().filter(|d| d.kw == "fn" && crate::debug::pure_def(d)).map(|d| d.name.clone()).collect();
    let mut d = run::dbg::Dbg::new(json, user_files, pure);
    let is_dap = dap.is_some();
    let stop_entry = dap.as_ref().is_some_and(|x| x.stop_on_entry);
    d.dap = dap;
    let mut code_lines: HashMap<String, BTreeSet<usize>> = HashMap::new();
    for f in &bc.funcs {
        for i in &f.code {
            if let bc::Ins::Line { line, .. } = i {
                code_lines.entry(f.file.clone()).or_default().insert(*line as usize);
            }
        }
    }
    d.code_lines = code_lines;
    for def in prog.defs.iter().filter(|x| matches!(x.kw.as_str(), "fn" | "test") && x.body.is_some()) {
        d.func_at.insert(def.name.clone(), (def.file.clone(), def.line));
    }
    if is_dap {
        if !d.dap_prelude() {
            return Ok(0);
        }
        run::dbg::dap_event("process", serde_json::json!({"name": "ksn", "isLocalProcess": true, "startMethod": "launch"}));
        if stop_entry {
            d.stop_on_entry();
        }
    } else {
        let kind = if matches!(target, DebugTarget::Test(_)) { "test" } else { "entry" };
        d.emit(serde_json::json!({"event": "ready", "target": name, "kind": kind}), format!("ready: {} (commands: break run continue step next finish bt locals print quit)", name));
        match d.prompt_before_run() {
            None => return Ok(0),
            Some(false) => {
                d.emit(serde_json::json!({"event": "terminated", "reason": "quit"}), "quit".into());
                return Ok(0);
            }
            Some(true) => {}
        }
    }
    let last_err = d.last_err.clone();
    let mut vm = run::Vm::new(&bc);
    vm.args = args.to_vec();
    let mut base_hook = run::dbg::flush_json_out(json, is_dap);
    vm.out_hook = Some(Box::new(move |fd, b| {
        if fd == 2 {
            let mut e = last_err.borrow_mut();
            e.clear();
            e.extend_from_slice(b);
        }
        base_hook(fd, b)
    }));
    vm.dbg = Some(Box::new(d));
    let out = vm.run(&name, &[]);
    vm.flush();
    let mut code = 0;
    let mut panic_msg: Option<String> = None;
    let mut err_msg: Option<String> = None;
    let mut quit = false;
    match out {
        Ok(Outcome::Value(_)) => {
            if let Some(Ty::Res(_, e)) = ret {
                if vm.result.first().copied().unwrap_or(0) != 0 {
                    let n = bc::slots_of(&bc.aggs, e.ty()) as usize;
                    let payload: Vec<u64> = vm.result[1..1 + n].to_vec();
                    // `Error:` の表示 (入口の `Err` の Debug 表示) は出力の事件にする。
                    let mut buf: Vec<u8> = vec![];
                    let saved = vm.out_hook.take();
                    vm.capture = Some(vec![]);
                    let _ = vm.run(kir_lower::debug::ENTRY_DEBUG, &payload);
                    vm.flush();
                    if let Some(c) = vm.capture.take() {
                        buf.extend_from_slice(&c);
                    }
                    vm.out_hook = saved;
                    err_msg = Some(String::from_utf8_lossy(&buf).trim_end().to_string());
                    code = 1;
                }
            }
        }
        Ok(Outcome::Panic(m)) => {
            vm.dbg_panic_stop(&m);
            panic_msg = Some(m);
            code = 101;
        }
        Ok(Outcome::Abort) => {
            let m = vm.dbg.as_ref().map(|d| String::from_utf8_lossy(&d.last_err.borrow()).trim_end().to_string()).unwrap_or_default();
            let m = m.strip_prefix("panic: ").unwrap_or(&m).to_string();
            vm.dbg_panic_stop(&m);
            code = 101;
        }
        Ok(Outcome::Exit(c)) => {
            quit = vm.dbg.as_ref().map_or(false, |d| d.quit);
            code = if quit { 0 } else { c };
        }
        Ok(Outcome::StackOverflow) => {
            eprintln!("stack overflow");
            code = 102;
        }
        Err(e) => {
            eprintln!("ksn: vm: 内部エラー: {}", e);
            code = 3;
        }
    }
    let d = vm.dbg.take();
    if let Some(mut d) = d {
        if is_dap {
            d.dap_finished(code, panic_msg, err_msg, quit);
        } else if quit {
            d.emit(serde_json::json!({"event": "terminated", "reason": "quit"}), "quit".into());
        } else {
            if let Some(m) = panic_msg {
                d.emit(serde_json::json!({"event": "output", "stream": "stderr", "text": format!("panic: {}\n", m)}), format!("panic: {}", m));
            }
            if let Some(m) = err_msg {
                let m = m.trim_start_matches("Error: ").to_string();
                d.emit(serde_json::json!({"event": "output", "stream": "stderr", "text": format!("Error: {}\n", m)}), format!("Error: {}", m));
            }
            d.emit(serde_json::json!({"event": "exited", "code": code}), format!("exited with code {}", code));
        }
    }
    Ok(code)
}

/// DAP の状態 (`launch` のあとで `debug` に渡す)。
pub fn run_dap_state(lines1: bool, cols1: bool, stop_on_entry: bool) -> run::dbg::DapSt {
    run::dbg::DapSt::new(lines1, cols1, stop_on_entry)
}
