#[path = "native/lld.rs"]
mod lld;
mod debug;
mod derive_drop;
mod derive_json;
mod defs;
mod doc;
mod exhaust;
mod doclink;
mod headers;
mod lexer;
mod ksnh;
mod ksnh_sync;
mod lexrules;
mod check;
mod check_cache;
mod cli;
mod ffi;
mod cnative;
mod explain;
mod fixes;
mod infer;
mod fmath;
mod kir;
mod kir_borrowck;
mod kir_exec;
mod assets;
mod kir_cabi;
mod kir_llvm;
#[cfg(feature = "embedded-llvm")]
mod embed_llvm;
mod kir_lower;
mod kir_opt;
mod kir_suspend;
mod kir_scalar;
mod fmt;
mod canon;
mod spacing;
mod bindgen;
mod grammar;
mod lint;
mod lockapi;
mod lsp;
mod project;
mod refactor;
mod interp;
mod task;
mod natives;
mod net;
mod process;
mod ptrwidth;
mod value;
mod parser;
mod program;
mod resolve;
mod refs_check;
mod rustisms;
mod stdcache;
mod survey;
mod tast;
mod tast_check;
mod tast_rules;
mod types;
mod typecov;
mod vm;

/// 実行用の主スレッドのスタック (仮想メモリの予約。深さの上限 `kir_exec::MAX_DEPTH` の木走査を支える)。
const MAIN_STACK: usize = 4 << 30;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dev = ["lex", "defs", "bodies", "headers", "resolve", "dev-check", "dev-run", "dev-test"];
    if args.len() >= 2 && !dev.contains(&args[1].as_str()) {
        let rest: Vec<String> = args[1..].to_vec();
        let child = std::thread::Builder::new().stack_size(MAIN_STACK).spawn(move || {
            interp::set_stack_floor(MAIN_STACK);
            cli::main_cli(rest)
        }).unwrap();
        std::process::exit(child.join().unwrap_or(2));
    }
    dev_main(args);
}

fn dev_main(_args: Vec<String>) {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "lex" {
        let src = std::fs::read_to_string(&args[2]).expect("read");
        match lexer::lex(&src) {
            Ok(toks) => {
                for t in toks {
                    println!("{}\t{}\t{}\t{}", t.kind, t.line, t.col, t.text.replace('\n', "\\n"));
                }
            }
            Err(e) => {
                eprintln!("ksn: {}", e);
                std::process::exit(1);
            }
        }
    } else if args.len() >= 3 && args[1] == "defs" {
        let src = match std::fs::read_to_string(&args[2]) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("ksn: {} を読めない: {}", args[2], e);
                std::process::exit(1);
            }
        };
        match defs::scan_with(&src, args[2].ends_with(".sig")) {
            Ok(ds) => {
                for d in ds {
                    println!("{}	{}	{}	{}", d.kw, d.name, d.line, d.end_line);
                }
            }
            Err(e) => {
                eprintln!("ksn: {}", e);
                std::process::exit(1);
            }
        }
    } else if args.len() >= 3 && args[1] == "bodies" {
        let src = std::fs::read_to_string(&args[2]).expect("read");
        let lines: Vec<&str> = src.split('\n').collect();
        let ds = match defs::scan(&src) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("ksn: {}", e);
                std::process::exit(1);
            }
        };
        for d in ds {
            if d.kw != "fn" && d.kw != "test" {
                continue;
            }
            let body = lines[d.line..d.end_line - 1].join("\n") + "\n";
            let toks = match lexer::lex(&body) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("ksn: {}: {}", d.name, e);
                    std::process::exit(1);
                }
            };
            match parser::parse_body(toks, d.line) {
                Ok(v) => println!("{}	{}", d.name, v.dump()),
                Err(e) => {
                    eprintln!("ksn: {}: {}", d.name, e);
                    std::process::exit(1);
                }
            }
        }
    } else if args.len() >= 3 && args[1] == "headers" {
        let src = std::fs::read_to_string(&args[2]).expect("read");
        match headers::headers(&src, args[2].ends_with(".sig")) {
            Ok(ls) => {
                for l in ls {
                    println!("{}", l);
                }
            }
            Err(e) => {
                eprintln!("ksn: {}", e);
                std::process::exit(1);
            }
        }
    } else if args.len() >= 4 && args[1] == "resolve" {
        match resolve::run(&args[2..]) {
            Ok(ls) => {
                for l in ls {
                    println!("{}", l);
                }
            }
            Err(e) => {
                eprintln!("ksn: {}", e);
                std::process::exit(1);
            }
        }
    } else if args.len() >= 4 && args[1] == "dev-check" {
        match run_check(&args[2..]) {
            Ok(ls) => {
                for l in ls {
                    println!("{}", l);
                }
            }
            Err(e) => {
                eprintln!("ksn: {}", e);
                std::process::exit(1);
            }
        }
    } else if args.len() >= 4 && (args[1] == "dev-run" || args[1] == "dev-test") {
        let rest: Vec<String> = args[1..].to_vec();
        let child = std::thread::Builder::new()
            .stack_size(MAIN_STACK)
            .spawn(move || {
                interp::set_stack_floor(MAIN_STACK);
                run_or_test(&rest)
            })
            .unwrap();
        let code = child.join().unwrap_or(2);
        std::process::exit(code);
    } else {
        eprintln!("usage: ksn-stage0 lex|defs|bodies|headers|resolve|check|run|test <args>");
        std::process::exit(2);
    }
}

/// `check <natives> <nlib> <files...>`: 先頭 nlib 個はライブラリ、残りは "path::module" 形式の検査対象。
fn run_check(args: &[String]) -> Result<Vec<String>, String> {
    let natives = std::fs::read_to_string(&args[0]).map_err(|e| e.to_string())?;
    let nlib: usize = args[1].parse().map_err(|_| "bad lib count".to_string())?;
    let mut prog = program::Program::default();
    for n in natives.lines() {
        prog.natives.insert(n.to_string());
    }
    let mut user = Vec::new();
    for (i, spec) in args[2..].iter().enumerate() {
        let (path, module) = match spec.split_once("::") {
            Some((p, m)) => (p.to_string(), m.to_string()),
            None => (spec.clone(), String::new()),
        };
        let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {}", path, e))?;
        for d in program::load_file(&path, &module, &src)? {
            if i >= nlib {
                user.push(prog.defs.len());
            }
            prog.add_def(d);
        }
    }
    let mut ck = check::Checker::new(&prog, user);
    ck.run();
    Ok(ck.diags.iter().map(check::format_diag).collect())
}

fn load_program(nlib: usize, files: &[String]) -> Result<program::Program, String> {
    let mut prog = program::Program::default();
    for n in natives::NATIVE_NAMES {
        prog.natives.insert(n.to_string());
    }
    for (i, spec) in files.iter().enumerate() {
        let (path, module) = match spec.split_once("::") {
            Some((p, m)) => (p.to_string(), m.to_string()),
            None => (spec.clone(), String::new()),
        };
        let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {}", path, e))?;
        let _ = i < nlib;
        for d in program::load_file(&path, &module, &src)? {
            prog.add_def(d);
        }
    }
    Ok(prog)
}

/// `run <entry> <nlib> files...` / `test <nlib> files...`
fn run_or_test(args: &[String]) -> i32 {
    use std::io::Write;
    use std::rc::Rc;
    let is_run = args[0] == "dev-run";
    let (entry, nlib_idx) = if is_run { (args[1].clone(), 2) } else { (String::new(), 1) };
    let nlib: usize = args[nlib_idx].parse().unwrap_or(0);
    let files = &args[nlib_idx + 1..];
    let prog = match load_program(nlib, files) {
        Ok(p) => Rc::new(p),
        Err(e) => {
            eprintln!("ksn: {}", e);
            return 2;
        }
    };
    if is_run {
        let Some(&di) = prog.funcs.get(&entry) else {
            eprintln!("ksn: E9030: entry function {} not found", entry);
            return 1;
        };
        let mut it = interp::Interp::new(prog.clone());
        return it.run_entry(di);
    }
    // test: 検査対象 (nlib 以降のファイル) の test 定義を名前順に実行
    let user_files: std::collections::HashSet<&str> = files[nlib..]
        .iter()
        .map(|s| s.split_once("::").map_or(s.as_str(), |x| x.0))
        .collect();
    let mut tests: Vec<usize> = (0..prog.defs.len())
        .filter(|&i| prog.defs[i].kw == "test" && user_files.contains(prog.defs[i].file.as_str()))
        .collect();
    tests.sort_by(|&a, &b| prog.defs[a].name.cmp(&prog.defs[b].name));
    let mut failed = 0;
    for &ti in &tests {
        let mut it = interp::Interp::new(prog.clone());
        let r = it.call_body(ti);
        let _ = it.out.flush();
        match r {
            Ok(_) | Err(value::Ctl::Return(_)) => println!("test {} ... ok", prog.defs[ti].name),
            Err(value::Ctl::Panic(m)) => {
                failed += 1;
                println!("test {} ... FAILED: {}", prog.defs[ti].name, m);
            }
            Err(value::Ctl::Exit(c)) => {
                failed += 1;
                println!("test {} ... FAILED: exit({})", prog.defs[ti].name, c);
            }
            Err(_) => println!("test {} ... ok", prog.defs[ti].name),
        }
    }
    println!("
{} passed; {} failed", tests.len() - failed, failed);
    if failed > 0 { 1 } else { 0 }
}
