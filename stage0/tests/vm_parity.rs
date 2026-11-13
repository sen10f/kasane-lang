//! `ksn run --engine=vm` / `ksn test --engine=vm` が、木走査 (既定) と出力・終了コードで一致することの確認 (設計_KIR.md 5 章 段階 2)。
//!
//! - `perf/*` と、KIR に下ろせる examples (demo・native・realtime・json・more_coll・more_str) は、VM で実際に走る (退避しない) ことも確かめる。
//! - 他の examples は、退避しても出力が一致することだけを確かめる。
//! - `tests/cli.rs` の中の `fn t.main` を持つ完全なプログラム (`kir_*` 系の入力を含む) を取り出して、同じ比較をする。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

struct Res {
    code: Option<i32>,
    out: String,
    err: String,
}

/// `ksn <args>` を `cwd` で走らせる (標準入力は空、`secs` 秒で打ち切る)。
fn ksn(args: &[&str], cwd: &Path, secs: u64) -> Res {
    let mut child = Command::new(exe())
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ksn");
    // 出力が詰まらないように別スレッドで読む。
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t1 = std::thread::spawn(move || {
        let mut b = vec![];
        std::io::Read::read_to_end(&mut so, &mut b).ok();
        b
    });
    let t2 = std::thread::spawn(move || {
        let mut b = vec![];
        std::io::Read::read_to_end(&mut se, &mut b).ok();
        b
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait().expect("wait") {
            Some(s) => break Some(s),
            None if start.elapsed() > Duration::from_secs(secs) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    let (o, e) = (t1.join().unwrap(), t2.join().unwrap());
    Res {
        code: status.and_then(|s| s.code()),
        out: String::from_utf8_lossy(&o).replace("\r\n", "\n"),
        err: String::from_utf8_lossy(&e).replace("\r\n", "\n"),
    }
}

/// VM が退避した旨の行 (`vm engine skipped: ...`)。
fn skipped(r: &Res) -> Option<String> {
    r.err.lines().find(|l| l.contains("vm engine skipped")).map(|l| l.to_string())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// VM で実際に走った。
    Vm,
    /// 下ろせない・VM が扱えない (木走査へ退避)。
    Fallback,
    /// 検査 (`ksn check`) で止まった (負の事例)。
    CheckFailed,
    Timeout,
}

/// 木走査と VM の結果を比べる。違いがあれば説明を返す。`strict` なら退避を不一致とみなす。
fn compare(label: &str, args: &[&str], cwd: &Path, strict: bool, secs: u64) -> (Option<String>, Kind) {
    let mut a: Vec<&str> = args.to_vec();
    let tree = ksn(&a, cwd, secs);
    a.push("--engine=vm");
    let vm = ksn(&a, cwd, secs);
    // 検査で止まった (VM まで届かない) 場合も「VM で走っていない」に数える。
    let check_failed = regex::Regex::new(r"\bE\d{4}\b").unwrap().is_match(&format!("{}{}", tree.out, tree.err));
    let kind = if check_failed { Kind::CheckFailed } else if skipped(&vm).is_some() { Kind::Fallback } else { Kind::Vm };
    let skip = if kind == Kind::Vm { None } else { Some(skipped(&vm).unwrap_or_else(|| "検査で止まった".to_string())) };
    if tree.code.is_none() {
        return (None, Kind::Timeout); // 木走査が時間切れ (比べられない)
    }
    if vm.code != tree.code {
        return (Some(format!("{}: 終了コードが違う tree={:?} vm={:?}\n  vm stderr: {}", label, tree.code, vm.code, vm.err.lines().take(3).collect::<Vec<_>>().join(" | "))), kind);
    }
    if vm.out != tree.out {
        return (Some(format!("{}: 標準出力が違う\n--- tree\n{}\n--- vm\n{}", label, tree.out, vm.out)), kind);
    }
    if strict && skip.is_some() {
        return (Some(format!("{}: VM が退避した: {}", label, skip.unwrap())), kind);
    }
    if skip.is_none() && vm.err != tree.err {
        return (Some(format!("{}: 標準エラーが違う\n--- tree\n{}\n--- vm\n{}", label, tree.err, vm.err)), kind);
    }
    (None, kind)
}

/// `jobs` を最大 8 本の並列で処理する。
fn parallel<T: Sync>(jobs: &[T], f: impl Fn(&T) -> Option<String> + Sync) -> Vec<String> {
    let next = AtomicUsize::new(0);
    let fails = std::sync::Mutex::new(vec![]);
    std::thread::scope(|s| {
        for _ in 0..8.min(jobs.len().max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= jobs.len() {
                    break;
                }
                if let Some(m) = f(&jobs[i]) {
                    fails.lock().unwrap().push(m);
                }
            });
        }
    });
    fails.into_inner().unwrap()
}

const STRICT_PKGS: &[&str] = &[
    "perf/closure",
    "perf/fib",
    "perf/hashmap",
    "perf/loops",
    "perf/noop",
    "perf/strcat",
    "perf/structs",
    "perf/vecops",
    "examples/demo",
    "examples/native",
    "examples/realtime",
    "examples/json",
    "examples/more_coll",
    "examples/more_str",
];

const LOOSE_PKGS: &[&str] = &["examples/features", "examples/tour", "examples/kvapp", "examples/plugins", "examples/shortener", "examples/threads"];

#[test]
fn vm_run_matches_tree_walker_on_perf_and_lowerable_examples() {
    let jobs: Vec<(&str, bool)> = STRICT_PKGS.iter().map(|p| (*p, true)).chain(LOOSE_PKGS.iter().map(|p| (*p, false))).collect();
    let fails = parallel(&jobs, |(p, strict)| compare(&format!("run {}", p), &["run"], &root().join(p), *strict, 120).0);
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

#[test]
fn vm_test_matches_tree_walker_on_examples() {
    // `ksn test --engine=vm`: テストごとの `ok` / `FAILED` の行が一致する。
    let pkgs = ["examples/demo", "examples/native", "examples/realtime", "examples/json", "examples/more_coll", "examples/more_str", "examples/features", "examples/tour"];
    let fails = parallel(&pkgs, |p| compare(&format!("test {}", p), &["test"], &root().join(p), false, 120).0);
    assert!(fails.is_empty(), "{}", fails.join("\n"));
    // KIR に下ろせるテストを持つ example では、VM で実際に走る (全件が退避ではない)。
    let r = ksn(&["test", "--engine=vm"], &root().join("examples/demo"), 120);
    assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
    assert!(r.out.contains("test ") && r.out.contains("0 failed"), "{}", r.out);
}

#[test]
fn test_engine_both_compares_tree_walker_and_vm() {
    // `ksn test --engine=both`: 木走査と VM の両方で走らせて比べる。examples の全パッケージで不一致が出ない。
    let pkgs = ["examples/demo", "examples/native", "examples/realtime", "examples/json", "examples/more_coll", "examples/more_str", "examples/features", "examples/tour"];
    let fails = parallel(&pkgs, |p| {
        let r = ksn(&["test", "--engine=both"], &root().join(p), 120);
        if r.code == Some(0) && r.out.contains("compared tree walker with vm:") && !r.out.contains("disagree") {
            None
        } else {
            Some(format!("{}: {:?}\n{}{}", p, r.code, r.out, r.err))
        }
    });
    assert!(fails.is_empty(), "{}", fails.join("\n"));
    let r = ksn(&["test", "--engine=both"], &root().join("examples/demo"), 120);
    assert!(r.out.contains("compared tree walker with vm: 1 (vm skipped 0)"), "{}", r.out);
    // `ksn run` では使えない。
    let r = ksn(&["run", "--engine=both"], &root().join("examples/demo"), 120);
    assert_eq!(r.code, Some(2), "{}{}", r.out, r.err);
}

/// Rust の通常の文字列リテラルの中身 (引用符の内側) をその値にする。
fn unescape(s: &str) -> String {
    let mut o = String::new();
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] != '\\' {
            o.push(cs[i]);
            i += 1;
            continue;
        }
        i += 1;
        match cs.get(i) {
            Some('n') => o.push('\n'),
            Some('t') => o.push('\t'),
            Some('r') => o.push('\r'),
            Some('0') => o.push('\0'),
            Some('"') => o.push('"'),
            Some('\'') => o.push('\''),
            Some('\\') => o.push('\\'),
            Some('\n') => {
                // 行継続: 改行と次の行の先頭の空白を捨てる
                while matches!(cs.get(i + 1), Some(c) if c.is_whitespace()) {
                    i += 1;
                }
            }
            Some('u') => {
                let end = cs[i..].iter().position(|c| *c == '}').map(|p| i + p).unwrap_or(i);
                let hex: String = cs[i + 2..end].iter().collect();
                if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    o.push(c);
                }
                i = end;
            }
            Some('x') => {
                let hex: String = cs[i + 1..(i + 3).min(cs.len())].iter().collect();
                if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    o.push(c);
                }
                i += 2;
            }
            Some(c) => o.push(*c),
            None => {}
        }
        i += 1;
    }
    o
}

/// `tests/cli.rs` から取り出した文字列リテラル: (位置, 値)。通常の文字列と raw 文字列の両方。
fn cli_literals(src: &str) -> Vec<(usize, String)> {
    let b = src.as_bytes();
    let mut lits: Vec<(usize, String)> = vec![];
    let mut i = 0;
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'r' if (i == 0 || !ident(b[i - 1])) && matches!(b.get(i + 1), Some(b'"') | Some(b'#')) => {
                let mut j = i + 1;
                let mut hashes = 0;
                while b.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if b.get(j) != Some(&b'"') {
                    i += 1;
                    continue;
                }
                let start = i;
                let body = j + 1;
                let close = format!("\"{}", "#".repeat(hashes));
                let end = src[body..].find(&close).map(|p| body + p).expect("raw 文字列が閉じない");
                lits.push((start, src[body..end].to_string()));
                i = end + close.len();
            }
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                lits.push((start, unescape(&src[start + 1..i])));
                i += 1;
            }
            b'\'' => {
                // 文字リテラル (`'"'` など) は読み飛ばす。ライフタイムは 1 文字進めるだけ。
                if b.get(i + 1) == Some(&b'\\') && b.get(i + 3) == Some(&b'\'') {
                    i += 4;
                } else if b.get(i + 2) == Some(&b'\'') {
                    i += 3;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    lits
}

/// 一時パッケージ用に `main` を足す (`tests/cli.rs` の `main_wrap` と同じ)。
fn main_wrap(prefix: &str, body: &str) -> String {
    format!("{}fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n{}    Ok(())\nend t.main\n", prefix, body)
}

/// `tests/cli.rs` から取り出したプログラム (テスト関数の名前, ソース)。
/// (a) `fn t.main` を持つ完全なプログラムのリテラル。
/// (b) `kir_*` のテストの表 `("t.f", &["引数"..], "期待値の式", "表示")` / `("t.f", &["引数"..], "panic の文言")` を、
///     そのテストが使う関数の定義 (`const ..: &str`) と組み合わせた `core.check.eq(f(args), lit)` / `let _x = f(args)` の main。
/// 同じ内容は 1 本にまとめる。
fn cli_programs() -> Vec<(String, String)> {
    let src = include_str!("cli.rs");
    let lits = cli_literals(src);
    // トップレベルの `fn name(` の位置。
    let mut fns: Vec<(usize, String)> = vec![];
    let mut off = 0;
    for line in src.split_inclusive('\n') {
        if let Some(r) = line.strip_prefix("fn ") {
            if let Some(n) = r.split('(').next() {
                fns.push((off, n.to_string()));
            }
        }
        off += line.len();
    }
    let fn_name = |pos: usize| fns.iter().rev().find(|(p, _)| *p <= pos).map(|(_, n)| n.clone()).unwrap_or_default();
    // 関数定義のライブラリ (`const NAME: &str = ..`、`fn t.` を持ち `main` を持たない)。
    let mut libs: Vec<(String, String)> = vec![];
    for (pos, text) in &lits {
        let line_start = src[..*pos].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let line = &src[line_start..*pos];
        if let Some(r) = line.strip_prefix("const ") {
            if line.trim_end().ends_with("&str =") && text.contains("fn t.") && !text.contains("t.main") {
                libs.push((r.split(':').next().unwrap_or("").to_string(), text.clone()));
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<(String, String)> = vec![];
    for (pos, text) in &lits {
        // `format!` の雛形 (`{}` や `{w}` だけの行を持つもの) は完全なプログラムではない。
        let template = regex::Regex::new(r"(^|\n)\{[a-z]*\}").unwrap().is_match(text);
        if !template && text.contains("fn t.main") && text.contains("end t.main") && seen.insert(text.clone()) {
            out.push((fn_name(*pos), text.clone()));
        }
    }
    // (b) 表の事例。テスト関数ごとの本文を切り出す。
    let re = regex::Regex::new(r#"\(\s*"(t\.[A-Za-z0-9_.]+)",\s*&\[([^\]]*)\],\s*"([^"]*)"(,\s*"[^"]*")?\s*\)"#).unwrap();
    for (k, (start, name)) in fns.iter().enumerate() {
        if !name.starts_with("kir_") {
            continue;
        }
        let end = fns.get(k + 1).map(|f| f.0).unwrap_or(src.len());
        let body = &src[*start..end];
        let uses_kir_pkg = body.contains("kir_pkg(");
        for c in re.captures_iter(body) {
            let (f, args, third, fourth) = (&c[1], &c[2], &c[3], c.get(4));
            let args: Vec<String> = args.split(',').map(|a| a.trim().trim_matches('"').to_string()).filter(|a| !a.is_empty()).collect();
            let call = format!("{}({})", f, args.join(", "));
            let defines = |lib: &str| lib.contains(&format!("fn {}(", f)) || lib.contains(&format!("fn {}[", f));
            let lib = libs.iter().find(|(n, t)| (body.contains(n.as_str()) || (uses_kir_pkg && n == "KIR_FNS")) && defines(t));
            let Some((_, lib)) = lib else { continue };
            let main = match fourth {
                Some(_) => format!("    core.check.eq({}, {})\n", call, third),
                None => format!("    let _x = {}\n", call),
            };
            let prog = main_wrap(lib, &main);
            if seen.insert(prog.clone()) {
                out.push((name.clone(), prog));
            }
        }
    }
    out
}

#[test]
fn vm_run_matches_tree_walker_on_programs_from_cli_tests() {
    let progs = cli_programs();
    let n_kir = progs.iter().filter(|(n, _)| n.starts_with("kir_")).count();
    assert!(progs.len() >= 150 && n_kir >= 60, "取り出せたプログラムが少ない: 全体 {} / kir_ 系 {}", progs.len(), n_kir);
    let pid = std::process::id();
    let jobs: Vec<(usize, &(String, String))> = progs.iter().enumerate().collect();
    let counts = [AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0)];
    let fails = parallel(&jobs, |(i, (name, text))| {
        let d = std::env::temp_dir().join(format!("ksn_vmp_{}_{}", pid, i));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n").unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(d.join("src").join("t.ksnl"), text).unwrap();
        let (r, kind) = compare(&format!("{} #{}", name, i), &["run"], &d, false, 60);
        counts[kind as usize].fetch_add(1, Ordering::SeqCst);
        if kind == Kind::CheckFailed && std::env::var("VM_PARITY_SHOW").is_ok() {
            eprintln!("--- 検査で止まった {} #{}\n{}", name, i, text);
        }
        let _ = fs::remove_dir_all(&d);
        r
    });
    let c: Vec<usize> = counts.iter().map(|a| a.load(Ordering::SeqCst)).collect();
    eprintln!("vm_parity: cli.rs から {} 本 (kir_ 系 {} 本): VM で走った {} / 退避 {} / 検査で止まった {} / 時間切れ {}", progs.len(), n_kir, c[0], c[1], c[2], c[3]);
    assert!(fails.is_empty(), "{}", fails.join("\n"));
    assert!(c[0] >= 100, "VM で走ったプログラムが少なすぎる");
}
