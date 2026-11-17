//! `ksn` コマンド (stage0)。Python 参照実装 ksn/cli.py の移植。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use sha2::{Digest, Sha256};

use crate::check::Checker;
use crate::explain::EXPLAIN;
use crate::fmt::{format_source, mechanical_collapse};
use crate::grammar::{py_repr, Grammar};
use crate::interp::Interp;
use crate::lint::Lint;
use crate::natives::NATIVE_NAMES;
use crate::program::{Def, Program};
use crate::project::*;
use crate::value::Ctl;

const VERSION: &str = "0.1.0";

fn lint_code(c: &str) -> &'static str {
    match c {
        "fqn" => "E1001",
        "name" => "E0010",
        "order" => "E0020",
        "attr" => "E0021",
        "blank" => "E0030",
        "indent" => "E0031",
        "pub" => "E1010",
        "unused" => "E1020",
        "impcollide" => "E1111",
        _ => "E0020",
    }
}

// ------------------------------------------------------------ 診断
#[derive(Clone)]
pub struct Item {
    pub code: String,
    pub fatal: bool,
    pub fqn: Option<String>,
    pub file: Option<String>,
    pub line: usize,
    pub col: usize,
    pub message: String,
    pub fix: Vec<crate::fixes::Fix>,
}

pub struct Report {
    json: bool,
    items: Vec<Item>,
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '\u{8}' => o.push_str("\\b"),
            '\u{c}' => o.push_str("\\f"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn fix_json(fixes: &[crate::fixes::Fix]) -> String {
    let parts: Vec<String> = fixes
        .iter()
        .map(|f| {
            format!(
                "{{\"file\": {}, \"line\": {}, \"col\": {}, \"end_line\": {}, \"end_col\": {}, \"replace\": {}}}",
                json_str(&f.file),
                f.line,
                f.col,
                f.end_line,
                f.end_col,
                json_str(&f.replace)
            )
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

/// 各診断に修正案を付ける (ソースは診断のファイルから読む)。
fn attach_fixes(items: &mut [Item]) {
    let mut cache: HashMap<String, Vec<String>> = HashMap::new();
    for it in items.iter_mut() {
        let Some(file) = it.file.clone() else { continue };
        let lines = cache
            .entry(file.clone())
            .or_insert_with(|| read_text(&file).unwrap_or_default().split(char::from(10u8)).map(String::from).collect());
        it.fix = crate::fixes::fixes_for(&it.code, &it.message, &file, it.line, it.col, lines);
    }
}

/// 定義ヘッダの行に付く診断のうち、メッセージが名前を示すもの (未知の型・トレイト、未使用の引数) は、
/// その名前が行の中で最初に現れる列 (1 始まり、Unicode スカラ値) を返す。
fn header_name_col(msg: &str, line: &str) -> Option<usize> {
    let name = ["unknown type ", "unknown trait ", "unused parameter "].iter().find_map(|p| msg.strip_prefix(p))?;
    let name = name.split_whitespace().next()?;
    let cs: Vec<char> = line.chars().collect();
    let ns: Vec<char> = name.chars().collect();
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    (0..cs.len()).find(|&i| {
        cs[i..].starts_with(&ns)
            && (i == 0 || !(ident(cs[i - 1]) || cs[i - 1] == '.'))
            && cs.get(i + ns.len()).map_or(true, |&c| !ident(c))
    })
    .map(|i| i + 1)
}

impl Report {
    fn new(json: bool) -> Report {
        Report { json, items: vec![] }
    }
    fn add(&mut self, code: &str, msg: &str, file: Option<&str>, line: usize, col: usize, fatal: bool) {
        self.items.push(Item {
            code: code.into(),
            fatal,
            fqn: None,
            file: file.map(String::from),
            line,
            col,
            message: msg.into(),
            fix: vec![],
        });
    }
    /// 列が分からない診断 (col == 0) に、その行の最初の非空白文字の列を補う (行が読めなければ 1)。
    fn fill_cols(&mut self) {
        let mut cache: HashMap<String, Vec<String>> = HashMap::new();
        for it in self.items.iter_mut().filter(|i| i.col == 0) {
            let lines = it.file.as_ref().map(|f| {
                cache
                    .entry(f.clone())
                    .or_insert_with(|| read_text(f).unwrap_or_default().split('\n').map(String::from).collect())
            });
            let text = lines.and_then(|ls| ls.get(it.line.wrapping_sub(1)));
            it.col = text.map_or(1, |l| {
                header_name_col(&it.message, l).unwrap_or_else(|| l.chars().take_while(|c| *c == ' ').count() + 1)
            });
        }
    }
    /// 列の補完と安定した並べ替え (出力はしない)。
    fn finish(&mut self) {
        self.fill_cols();
        self.items.sort_by(|a, b| {
            (a.file.clone().unwrap_or_default(), a.line, a.col, &a.code).cmp(&(b.file.clone().unwrap_or_default(), b.line, b.col, &b.code))
        });
    }
    fn flush(&mut self) {
        self.finish();
        for it in &self.items {
            if self.json {
                println!(
                    "{{\"code\": {}, \"fatal\": {}, \"fqn\": {}, \"file\": {}, \"line\": {}, \"col\": {}, \"message\": {}, \"fix\": {}}}",
                    json_str(&it.code),
                    it.fatal,
                    it.fqn.as_ref().map_or("null".to_string(), |x| json_str(x)),
                    it.file.as_ref().map_or("null".to_string(), |x| json_str(x)),
                    it.line,
                    it.col,
                    json_str(&it.message),
                    fix_json(&it.fix)
                );
            } else {
                let loc = match &it.file {
                    Some(f) => format!("{}:{}:{}", f, it.line, it.col),
                    None => "<project>".to_string(),
                };
                println!("{}: {}{}: {}", loc, it.code, if it.fatal { "" } else { " (non-fatal)" }, it.message);
            }
        }
    }
    fn fatal(&self) -> bool {
        self.items.iter().any(|i| i.fatal)
    }
}

pub fn rel(path: &str) -> String {
    let cwd = norm(&std::env::current_dir().unwrap_or_default());
    let p = path.replace('\\', "/");
    match p.strip_prefix(&format!("{}/", cwd)) {
        Some(r) => r.to_string(),
        None => p,
    }
}

// ------------------------------------------------------------ 読み込み
pub struct Loaded {
    pub pkg: Package,
    pub deps: Vec<Package>,
    pub std: Vec<Source>,
}

pub(crate) fn load_all(root: &Path) -> Result<Loaded, ProjectError> {
    crate::vm::tick("pre");
    let pkg = load_package(root)?;
    crate::vm::tick("load_pkg");
    let deps = load_deps(&pkg)?;
    crate::vm::tick("load_deps");
    let std = load_stdlib();
    crate::vm::tick("load_std");
    Ok(Loaded { pkg, deps, std })
}

fn empty_program() -> Program {
    let mut p = Program::default();
    for n in NATIVE_NAMES {
        p.natives.insert(n.to_string());
    }
    p
}

/// std + 依存 + 本体の順に登録する。返す添字は本体の定義。
pub(crate) fn build_program(l: &Loaded) -> (Program, Vec<usize>) {
    build_program_with(l, None)
}

/// `build_program` の、標準ライブラリの定義を複製せず `Loaded` から移すもの (起動時間の短縮)。
/// 移したあとの `l.std[*].defs` は空になるので、あとで標準ライブラリの定義を読まない呼び出しだけが使う。
pub(crate) fn build_program_take(l: &mut Loaded) -> (Program, Vec<usize>) {
    let mut std_defs: Vec<Vec<Def>> = l.std.iter_mut().map(|s| std::mem::take(&mut s.defs)).collect();
    build_program_with(l, Some(&mut std_defs))
}

fn build_program_with(l: &Loaded, taken: Option<&mut Vec<Vec<Def>>>) -> (Program, Vec<usize>) {
    let mut prog = empty_program();
    match taken {
        Some(t) => {
            for ds in t.iter_mut() {
                for d in std::mem::take(ds) {
                    prog.add_def(d);
                }
            }
        }
        None => {
            for s in &l.std {
                for d in &s.defs {
                    prog.add_def(d.clone());
                }
            }
        }
    }
    let n_std = prog.defs.len();
    for dp in &l.deps {
        for s in &dp.sources {
            for d in &s.defs {
                prog.add_def(d.clone());
            }
        }
    }
    let mut user = vec![];
    for s in &l.pkg.sources {
        for d in &s.defs {
            user.push(prog.defs.len());
            prog.add_def(d.clone());
        }
    }
    crate::derive_json::expand(&mut prog, n_std, &mut user);
    crate::infer::run(&mut prog);
    crate::infer::annotate_int_lits(&mut prog, &user);
    crate::infer::annotate_owned_drops(&mut prog, &user);
    (prog, user)
}

/// lint がモジュール名を求めるための、ソースルートからの相対パス風の名前。
/// `tests/` 配下は `<パッケージ>/tests/<名前>` にする (モジュール `<パッケージ>.tests.<名前>`)。
fn lint_rel(s: &Source, srcroot: &str) -> String {
    if s.is_test {
        format!("{}.ksnl", s.module.replace('.', "/"))
    } else {
        s.path.strip_prefix(&format!("{}/", srcroot)).unwrap_or(&s.path).to_string()
    }
}

fn check_lock(pkg: &Package, deps: &[Package], rep: &mut Report) {
    for (code, msg) in verify_lock(pkg, deps) {
        rep.add(&code, &msg, Some("kasane.lock"), 1, 1, true);
    }
}

/// 構文・正規形・意味の検査。構文エラーが無ければ Program を返す。
fn full_check(root: &Path, rep: &mut Report, run_mode: bool) -> Result<(Loaded, Option<Program>, Vec<usize>), ProjectError> {
    full_check_opts(root, rep, run_mode, false)
}

/// `ksn test` の検査: 本体に加えて、`src/` の `///` のコード例 (```` ```ksn ````) を `tests/` の統合テストとして読み込む (3.2)。
/// 例は `tests/doc_<番号>.ksnl` の仮想ファイルになり、その中の診断は元の囲みの行 (`src/...:行`) に直して出す。
fn full_check_with_examples(root: &Path, rep: &mut Report) -> Result<(Loaded, Option<Program>, Vec<usize>), ProjectError> {
    let mut l = load_all(root)?;
    let testsdir = norm(&l.pkg.root.join("tests"));
    let pkg = l.pkg.name.clone();
    // (仮想ファイル, 元のファイル, 仮想ファイルでの本文の先頭行, 元の本文の先頭行)
    let mut origins: Vec<(String, String, usize, usize)> = vec![];
    let mut extra: Vec<Source> = vec![];
    let mut k = 0;
    for s in &l.pkg.sources {
        if s.is_test || s.error.is_some() {
            continue;
        }
        for ex in crate::doclink::examples(&s.lines) {
            k += 1;
            let vpath = format!("{}/doc_{}.ksnl", testsdir, k);
            let name = format!("{}.tests.doc_{}.test_example", pkg, k);
            // 本文がモジュールの名前を使うときだけ import を付ける (使わない import は E1020)。
            let tail = s.module.rsplit('.').next().unwrap_or("").to_string();
            let uses_mod = ex.body.iter().any(|x| {
                x.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.')).any(|t| {
                    t == s.module || t == tail || t.starts_with(&format!("{}.", s.module)) || t.starts_with(&format!("{}.", tail))
                })
            });
            let mut text = String::new();
            let mut head = 2;
            if uses_mod {
                text.push_str(&format!("import {}\n\n", s.module));
                head += 2;
            }
            text.push_str(&format!("// {}:{}\n", rel(&s.path), ex.line));
            text.push_str(&format!("test {}\n", name));
            for line in &ex.body {
                if line.trim().is_empty() {
                    text.push('\n');
                } else {
                    text.push_str(&format!("    {}\n", line));
                }
            }
            text.push_str(&format!("end {}\n", name));
            origins.push((vpath.clone(), rel(&s.path), head + 1, ex.line + 1));
            extra.push(Source::from_text_test(&vpath, &testsdir, &pkg, text));
        }
    }
    l.pkg.sources.extend(extra);
    let (l, prog, user) = full_check_loaded(l, rep, false, false)?;
    for it in rep.items.iter_mut() {
        let Some(f) = it.file.clone() else { continue };
        if let Some((_, orig, vstart, ostart)) = origins.iter().find(|o| rel(&o.0) == f) {
            it.file = Some(orig.clone());
            if it.line >= *vstart {
                it.line = ostart + (it.line - vstart);
            } else {
                it.line = *ostart;
            }
            it.message = format!("in a doc example: {}", it.message);
        }
    }
    Ok((l, prog, user))
}

/// `take_std` のとき、返す `Loaded` の標準ライブラリの定義は空 (`Program` に移した)。
fn full_check_opts(root: &Path, rep: &mut Report, run_mode: bool, take_std: bool) -> Result<(Loaded, Option<Program>, Vec<usize>), ProjectError> {
    // 文法の構築 (正規表現のコンパイルが重い) は、定義単位の再利用が外れたときに初めてやる (grammar.rs)。
    let l = load_all(root)?;
    crate::vm::tick("load_all");
    full_check_loaded(l, rep, run_mode, take_std)
}

/// `full_check_opts` の、読み込み済みの `Loaded` から始めるもの (LSP の増分検査が、一部のソースだけの `Loaded` で呼ぶ)。
fn full_check_loaded(mut l: Loaded, rep: &mut Report, run_mode: bool, take_std: bool) -> Result<(Loaded, Option<Program>, Vec<usize>), ProjectError> {
    let grammar = Grammar::new();
    check_lock(&l.pkg, &l.deps, rep);
    // ``[`FQN`]`` のリンクは読み込んだ全定義に解決しなければならない (3.2、E1125)。
    {
        let known = crate::doclink::Known::of_loaded(&l);
        for s in &l.pkg.sources {
            let fl = rel(&s.path);
            for (ln, f) in crate::doclink::unresolved(&s.lines, &known) {
                rep.add("E1125", &format!("doc link to unknown definition `{}`", f), Some(&fl), ln, 1, true);
            }
        }
    }
    // 標準ライブラリを検査するときは `spec/stdlib` 自体がソースルート (モジュール名がファイルの位置から決まる)。
    let srcroot = if l.pkg.source == STDLIB_SOURCE { norm(&l.pkg.root) } else { norm(&l.pkg.root.join("src")) };
    crate::vm::tick("grammar_new");
    let use_cache = crate::check_cache::enabled();
    for s in &l.pkg.sources {
        let fl = rel(&s.path);
        if s.path.ends_with(".ksnh") {
            rep.add("E1107", "the .ksnh extension is no longer supported; rename the file to .ksnl", Some(&fl), 1, 1, true);
        }
        // 文面だけで決まる検査 (文法・正規形・lint) は、キャッシュが有効ならファイル単位で再利用する
        // (check_cache.rs、設計_増分検査.md)。無効のときは従来どおり毎回計算する。
        let relpath = lint_rel(s, &srcroot);
        let mut memo = if use_cache && s.error.is_none() { crate::check_cache::load(&s.path, &relpath) } else { Default::default() };
        let gram = if s.error.is_none() {
            Some(if use_cache { grammar.check_memo(&s.text, &mut memo.gram) } else { grammar.check(&s.text) })
        } else {
            None
        };
        // 構文エラーのとき、Rust 風の書き方をまとめて直し方つきで報告する。
        // 案内付きの行の一般的な E0002 (`expected ...`) は、同じ原因を別の言い方で重ねるだけなので出さない。
        let mut guided: Vec<usize> = vec![];
        if s.error.is_some() || !gram.as_ref().map_or(false, |g| g.0) {
            for h in crate::rustisms::scan(&s.text) {
                guided.push(h.line);
                rep.add(h.code, &h.msg, Some(&fl), h.line, h.col, true);
            }
        }
        if s.text.contains('\r') {
            rep.add("E0001", "CR (CRLF) is not allowed; use LF", Some(&fl), 1, 1, true);
        }
        if let Some(e) = &s.error {
            if !(e.code == "E0002" && guided.contains(&e.line)) {
                rep.add(&e.code, &e.msg, Some(&fl), e.line, e.col, true);
            }
            continue;
        }
        let gram = gram.unwrap();
        for (i, line) in s.lines.iter().enumerate() {
            if line.contains('\t') {
                rep.add("E0001", "tab is not allowed", Some(&fl), i + 1, line.chars().position(|c| c == '\t').map_or(1, |p| p + 1), true);
            }
            if line != line.trim_end_matches(' ') && !line.ends_with('\r') {
                rep.add("E0001", "trailing whitespace", Some(&fl), i + 1, line.trim_end_matches(' ').chars().count() + 1, true);
            }
        }
        if !s.text.ends_with('\n') || s.text.ends_with("\n\n") {
            rep.add("E0001", "file must end with exactly one LF", Some(&fl), s.lines.len(), 1, true);
        }
        let lt = s.text.replace("\r\n", "\n");
        // ファイル全体の結果は、文面と相対パスが前回と同じなら再利用する (check_cache::FileMemo::file_old)。
        let fkey = if use_cache { Some(crate::check_cache::file_key(&relpath, &s.text)) } else { None };
        let reused = match (&fkey, &memo.file_old) {
            (Some(k), Some((ok, fc))) if k == ok => Some(fc.clone()),
            _ => None,
        };
        let fc = match reused {
            Some(fc) => {
                // 定義単位の結果は前回のものをそのまま持ち越す (スロットを書き換えない)。
                memo.lint.new = memo.lint.old.clone();
                memo.gram.new.extend(memo.gram.old.iter().cloned());
                fc
            }
            None => {
                // 正規形でなくても、`ksn fmt` で正規形になり、その結果が検査を通るなら、並び順や空行は
                // エラーにしない (`fmt --check` が差分として報告する)。意味に関する誤りは従来どおり報告する。
                // 整形は安いので毎回やる。lint と文法は定義ごとの結果を再利用する (check_cache.rs)。
                let run_lint = |text: &str, memo: &mut crate::check_cache::FileMemo| {
                    if use_cache {
                        Lint::new(&relpath, text).run_memo(&mut memo.lint)
                    } else {
                        Lint::new(&relpath, text).run()
                    }
                };
                let formatted = crate::fmt::format_source(&s.text, &s.path);
                // `///` の行を落とす整形 (付く定義のない説明など) は、正規形とみなさない (E0002 のまま)。
                let docs = |t: &str| t.lines().filter(|l| l.starts_with("///")).count();
                let skip = formatted != s.text
                    && docs(&formatted) == docs(&s.text)
                    && !s.text.contains('\t')
                    && run_lint(&formatted, &mut memo).is_empty()
                    && (if use_cache { grammar.check_memo(&formatted, &mut memo.gram) } else { grammar.check(&formatted) }).0;
                let lint = if skip { vec![] } else { run_lint(&lt, &mut memo) };
                crate::check_cache::FileCheck { skip, lint }
            }
        };
        if let Some(k) = fkey {
            memo.file_new = Some((k, fc.clone()));
        }
        if use_cache {
            crate::check_cache::store(&s.path, &relpath, &memo);
        }
        if fc.skip {
            continue;
        }
        for (code, ln, msg) in fc.lint {
            // `order` (E0020: 定義順・群内順・uses/属性の並び順)は `ksn fmt` で
            // 機械的に直せる違反。check では非致命的として報告し、終了コードに
            // 影響させない (`fmt --check` が差分として検出する)。
            let fatal = !(run_mode && code == "unused") && code != "order";
            rep.add(lint_code(&code), &msg, Some(&fl), ln, 0, fatal);
        }
        let (ok, msg, ln) = gram;
        if !ok && !guided.contains(&ln) {
            rep.add("E0002", &msg, Some(&fl), ln, 0, true);
        }
    }
    if use_cache {
        // 置き場の上限を超えていたら古いものから消す (1 回だけ。ファイルごとに数えると件数の 2 乗になる)。
        crate::check_cache::trim();
    }
    crate::vm::tick("lint");
    if l.pkg.sources.iter().any(|s| s.error.is_some()) {
        return Ok((l, None, vec![]));
    }
    let (prog, user) = if take_std { build_program_take(&mut l) } else { build_program(&l) };
    crate::vm::tick("build_prog");
    let diags = {
        let mut ck = Checker::new(&prog, user.clone());
        ck.unsafe_modules = crate::project::unsafe_modules(&l.pkg.manifest);
        ck.integration_files = l.pkg.sources.iter().filter(|s| s.is_test).map(|s| s.path.clone()).collect();
        ck.run();
        crate::vm::tick("checker");
        ck.diags
    };
    // 隠しの切り替え (試験用): 借用・線形の診断 (E30xx) を外して実行する。検査器の穴を実行時の検査 (解放済みの値の読み) で確かめる。
    let diags: Vec<_> = if std::env::var_os("KSN_SKIP_BORROW_DIAGS").is_some() {
        diags.into_iter().filter(|d| !d.code.starts_with("E30")).collect()
    } else {
        diags
    };
    // 同じ呼び出しに対する E1108 は、名前だけの検査と型に基づく検査が両方出しうる。最初の 1 件だけ残す。
    let mut seen_1108: std::collections::HashSet<(String, usize, usize)> = Default::default();
    for d in diags {
        if d.code == "E1108" && !seen_1108.insert((d.file.clone(), d.line, d.col)) {
            continue;
        }
        rep.items.push(Item {
            code: d.code,
            fatal: true,
            fqn: if d.fqn == "-" { None } else { Some(d.fqn) },
            file: Some(rel(&d.file)),
            line: d.line,
            col: d.col,
            message: d.msg,
            fix: vec![],
        });
    }
    Ok((l, Some(prog), user))
}

// ------------------------------------------------------------ コマンド
fn cmd_new(name: &str) -> i32 {
    if !is_pkg_name(name) || RESERVED_NAMES.contains(&name) {
        eprintln!("ksn: invalid package name {}", py_repr(name));
        return 1;
    }
    if Path::new(name).exists() {
        eprintln!("ksn: {} already exists", name);
        return 1;
    }
    fs::create_dir_all(Path::new(name).join("src")).unwrap();
    fs::write(
        Path::new(name).join("kasane.toml"),
        format!("[package]\nname = \"{0}\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"{0}.main\"\n\n[dependencies]\n", name),
    )
    .unwrap();
    fs::write(Path::new(name).join("kasane.lock"), "version = 1\n").unwrap();
    fs::write(
        Path::new(name).join("src").join(format!("{}.ksnl", name)),
        format!(
            "fn {0}.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    std.io.print_line(\"Hello, Kasane!\")\nend {0}.main\n",
            name
        ),
    )
    .unwrap();
    0
}

fn is_pkg_name(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_lowercase()) && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `ksn lsp` 用の検査結果。`check --json` と同じ診断 (修正案つき・並べ替え済み)。
pub struct Analysis {
    pub items: Vec<Item>,
    /// 読み込みに成功したとき: 読み込んだパッケージ一式と、構文エラーが無ければ検査済みの Program。
    pub loaded: Option<(Loaded, Option<Program>)>,
}

/// パッケージ `root` を検査して診断を返す (出力はしない。ソースは `overlay_set` の上書きを優先して読む)。
pub fn analyze(root: &Path) -> Analysis {
    analyze_inc(root, None)
}

/// 前回の検査 (読み込んだ `Loaded` と診断) を渡すと、変わったソースとその依存元だけを検査し直す。
/// 結果は `analyze` (全体検査) と一致する。一致を保証できない形 (`plan_slice` が `None`) は全体を検査する。
pub fn analyze_inc(root: &Path, prev: Option<(&Loaded, &[Item])>) -> Analysis {
    let l = load_all(root);
    match (prev, l) {
        (Some((pl, pitems)), Ok(l)) => match plan_slice(&l, pl) {
            Some(plan) => {
                crate::vm::tick("plan");
                if std::env::var_os("KSN_TIMING").is_some() {
                    eprintln!("[timing] incremental: redo {} / keep {} / of {}", plan.redo.len(), plan.keep.len(), l.pkg.sources.len());
                }
                analyze_slice(l, plan, pitems)
            }
            None => {
                if std::env::var_os("KSN_TIMING").is_some() {
                    eprintln!("[timing] incremental: full");
                }
                analyze_loaded(root, Ok(l))
            }
        },
        (_, l) => analyze_loaded(root, l),
    }
}

fn analyze_loaded(_root: &Path, l: Result<Loaded, ProjectError>) -> Analysis {
    let mut rep = Report::new(false);
    let loaded = match l.and_then(|l| full_check_loaded(l, &mut rep, false, false)) {
        Ok((l, p, _)) => Some((l, p)),
        Err(e) => {
            rep.add(&e.code, &e.msg, None, 1, 1, true);
            None
        }
    };
    attach_fixes(&mut rep.items);
    rep.finish();
    Analysis { items: rep.items, loaded }
}

/// 増分検査の計画: 検査し直すソース (添字) と、検査に載せるソース (それらと、参照先の閉包)。
struct SlicePlan {
    redo: Vec<usize>,
    keep: Vec<usize>,
}

/// ソースが参照するモジュール (自分以外)。ドットでつないだ識別子の接頭辞が、パッケージのモジュール名に当たるもの。
/// `import` に限らず、FQN での参照 (import 無しでも検査は通る) と、コメント・文字列中の一致も含む (過大に数える側に倒す)。
fn referenced_modules(text: &str, mods: &HashMap<&str, usize>, me: usize) -> Vec<usize> {
    let mut out: Vec<usize> = vec![];
    let b = text.as_bytes();
    let is_id = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80;
    let mut i = 0;
    while i < b.len() {
        if !is_id(b[i]) || (i > 0 && (is_id(b[i - 1]) || b[i - 1] == b'.')) {
            i += 1;
            continue;
        }
        // 1 つのドット連鎖 `a.b.c` を読み、各接頭辞を引く。
        let start = i;
        let mut j = i;
        let mut best: Option<usize> = None;
        loop {
            while j < b.len() && is_id(b[j]) {
                j += 1;
            }
            // 最長一致だけ数える (`syn.m5.f0` は `syn` ではなく `syn.m5` への参照)。
            if let Some(&m) = mods.get(&text[start..j]) {
                best = Some(m);
            }
            if j + 1 < b.len() && b[j] == b'.' && is_id(b[j + 1]) {
                j += 1;
            } else {
                break;
            }
        }
        if let Some(m) = best {
            if m != me && !out.contains(&m) {
                out.push(m);
            }
        }
        i = j.max(i + 1);
    }
    out
}

fn def_names(s: &Source) -> Vec<&str> {
    let mut v: Vec<&str> = s.defs.iter().filter(|d| d.kw != "import").map(|d| d.name.as_str()).collect();
    v.sort_unstable();
    v
}

fn no(i: u32) -> Option<SlicePlan> {
    if std::env::var_os("KSN_TIMING").is_some() {
        eprintln!("[timing] plan_slice: 全体検査へ (理由 {})", i);
    }
    None
}

fn plan_slice(l: &Loaded, pl: &Loaded) -> Option<SlicePlan> {
    let (a, b) = (&l.pkg.sources, &pl.pkg.sources);
    if a.len() != b.len() || l.pkg.manifest != pl.pkg.manifest || l.deps.len() != pl.deps.len() {
        return no(1);
    }
    for (x, y) in l.deps.iter().zip(&pl.deps) {
        if x.name != y.name || x.sources.len() != y.sources.len() || x.sources.iter().zip(&y.sources).any(|(p, q)| p.path != q.path || p.text != q.text) {
            return no(2);
        }
    }
    // 構文エラーがあると Program が作れない。診断も古くなりうるので全体へ。
    if a.iter().chain(b.iter()).any(|s| s.error.is_some()) {
        return no(3);
    }
    let mut changed: Vec<usize> = vec![];
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        if x.path != y.path || x.module != y.module || x.is_test != y.is_test {
            return no(4);
        }
        if x.text != y.text {
            // 統合テスト・トレイト・impl はプロジェクト全体に効く。定義名の増減は重複 (E1103) などを変える。
            let global = |s: &Source| s.is_test || s.defs.iter().any(|d| matches!(d.kw.as_str(), "impl" | "trait"));
            if global(x) || global(y) || def_names(x) != def_names(y) {
                return no(5);
            }
            changed.push(i);
        }
    }
    let mut mods: HashMap<&str, usize> = HashMap::new();
    for (i, s) in a.iter().enumerate() {
        if mods.insert(s.module.as_str(), i).is_some() {
            return no(6);
        }
    }
    let refs: Vec<Vec<usize>> = a.iter().enumerate().map(|(i, s)| referenced_modules(&s.text, &mods, i)).collect();
    // 依存元 (参照している側) の推移閉包。
    let mut redo: Vec<bool> = vec![false; a.len()];
    for &c in &changed {
        redo[c] = true;
    }
    loop {
        let mut grew = false;
        for i in 0..a.len() {
            if !redo[i] && refs[i].iter().any(|&r| redo[r]) {
                redo[i] = true;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    // 検査に載せるもの: 検査し直すもの、その参照先の閉包、トレイト・impl を持つモジュール (名前で参照されずに効く)。
    let mut keep: Vec<bool> = redo.clone();
    let mut stack: Vec<usize> = (0..a.len()).filter(|&i| redo[i] || a[i].defs.iter().any(|d| matches!(d.kw.as_str(), "impl" | "trait"))).collect();
    for &i in &stack {
        keep[i] = true;
    }
    while let Some(i) = stack.pop() {
        for &r in &refs[i] {
            if !keep[r] {
                keep[r] = true;
                stack.push(r);
            }
        }
    }
    if keep.iter().all(|&k| k) {
        return no(99); // 全部載せるなら全体検査と同じ
    }
    Some(SlicePlan { redo: (0..a.len()).filter(|&i| redo[i]).collect(), keep: (0..a.len()).filter(|&i| keep[i]).collect() })
}

fn analyze_slice(mut l: Loaded, plan: SlicePlan, prev_items: &[Item]) -> Analysis {
    let n = l.pkg.sources.len();
    let all_rel: Vec<String> = l.pkg.sources.iter().map(|s| rel(&s.path)).collect();
    let redo_rel: std::collections::HashSet<&str> = plan.redo.iter().map(|&i| all_rel[i].as_str()).collect();
    // 載せるソースだけを移した `Loaded` を作って検査する。終わったら元の位置へ戻す。
    let mut slots: Vec<Option<Source>> = std::mem::take(&mut l.pkg.sources).into_iter().map(Some).collect();
    let slice_sources: Vec<Source> = plan.keep.iter().map(|&i| slots[i].take().unwrap()).collect();
    let slice = Loaded {
        pkg: Package { name: l.pkg.name.clone(), root: l.pkg.root.clone(), manifest: l.pkg.manifest.clone(), sources: slice_sources, source: l.pkg.source.clone() },
        deps: std::mem::take(&mut l.deps),
        std: std::mem::take(&mut l.std),
    };
    let mut rep = Report::new(false);
    let res = full_check_loaded(slice, &mut rep, false, true);
    let back = match res {
        Ok((sl, _, _)) => Some(sl),
        Err(_) => None,
    };
    let Some(sl) = back else {
        // ここには来ないはず (読み込みは済んでいる)。来たら全体検査へ。
        return analyze_loaded(&l.pkg.root.clone(), load_all(&l.pkg.root));
    };
    let Loaded { pkg, deps, .. } = sl;
    for (src, &i) in pkg.sources.into_iter().zip(&plan.keep) {
        slots[i] = Some(src);
    }
    l.pkg.sources = slots.into_iter().map(|s| s.unwrap()).collect();
    debug_assert_eq!(l.pkg.sources.len(), n);
    l.deps = deps;
    l.std = load_stdlib();
    // 診断: 検査し直したファイルは新しい結果、ほかのソースファイルは前回のもの、ソースでないファイル (kasane.lock ほか) は前回のもの。
    let mut items: Vec<Item> = prev_items
        .iter()
        .filter(|it| match it.file.as_deref() {
            Some(f) => !redo_rel.contains(f) && !f.ends_with("kasane.lock"),
            None => true,
        })
        .cloned()
        .collect();
    let mut fresh: Vec<Item> = rep.items.into_iter().filter(|it| it.file.as_deref().map_or(false, |f| redo_rel.contains(f))).collect();
    let mut lock = Report::new(false);
    check_lock(&l.pkg, &l.deps, &mut lock);
    fresh.extend(lock.items);
    attach_fixes(&mut fresh);
    let mut rep = Report::new(false);
    rep.items = items.drain(..).chain(fresh).collect();
    rep.finish();
    Analysis { items: rep.items, loaded: Some((l, None)) }
}

/// 隠しの切り替え `KSN_BORROWCK=kir` / `ksn check --borrowck=kir`: AST の借用検査の E3010 を、KIR の上の検査の結果に置き換える
/// (設計_KIR.md 24 章)。KIR に下ろせないときは AST の結果を残し、stderr に `kir-borrowck: unknown` を出す。
///
/// `only` (`KSN_BORROWCK=kir-only`、計測用): AST の借用・線形の診断 (E3010 / E3030 / E3002 / E3003) を外し、
/// KIR の結果だけを残す。KIR に下ろせないときは何も出さない (AST に戻さない)。既定の検査は変わらない。
fn kir_borrowck_union(rep: &mut Report, prog: Option<&Program>, user: &[usize]) {
    // 既定: AST の結果は残し、KIR の診断を足す (どちらかが拒めば拒む)。下ろせないときは AST の結果だけ。
    // すでに致命の診断があるなら足すものはない (誤りのあるプログラムは KIR に下ろす前提を満たさず、下ろす側が落ちることもある)。
    if rep.items.iter().any(|i| i.fatal) {
        return;
    }
    let Some(p) = prog else { return };
    let lo = crate::kir_lower::lower_user(p, user);
    if let crate::kir_borrowck::Outcome::Checked(ds) = crate::kir_borrowck::check(p, user, &lo, false) {
        for d in &ds {
            let line = d.line.max(1);
            if rep.items.iter().any(|i| i.code == d.code && i.file.as_deref() == Some(d.file.as_str()) && i.line == line) {
                continue;
            }
            rep.add(d.code, &format!("[kir] {}", d.msg), Some(&d.file), line, 1, true);
        }
    }
}

fn kir_borrowck_hidden(rep: &mut Report, prog: Option<&Program>, user: &[usize], own: bool, only: bool) {
    let drop_ast = |rep: &mut Report| rep.items.retain(|i| !matches!(i.code.as_str(), "E3010" | "E3030" | "E3002" | "E3003"));
    let Some(p) = prog else {
        if only {
            drop_ast(rep);
        }
        eprintln!("kir-borrowck: unknown (no program)");
        return;
    };
    let lo = crate::kir_lower::lower_user(p, user);
    match crate::kir_borrowck::check(p, user, &lo, own) {
        crate::kir_borrowck::Outcome::Unknown(why) => {
            if only {
                drop_ast(rep);
            }
            eprintln!("kir-borrowck: unknown ({})", why)
        }
        crate::kir_borrowck::Outcome::Checked(ds) => {
            if only {
                drop_ast(rep);
            }
            rep.items.retain(|i| i.code != "E3010" && !(own && (i.code == "E3002" || i.code == "E3003")));
            let codes: Vec<&str> = ds.iter().map(|d| d.code).collect();
            for d in &ds {
                rep.add(d.code, &format!("[kir] {}", d.msg), Some(&d.file), d.line.max(1), 1, true);
            }
            eprintln!("kir-borrowck: checked {}", codes.join(","));
        }
    }
}

fn cmd_check(json: bool) -> i32 {
    let mut rep = Report::new(json);
    let bc_mode = std::env::var("KSN_BORROWCK").ok().or_else(|| std::env::args().find_map(|a| a.strip_prefix("--borrowck=").map(String::from))).unwrap_or_default();
    let kir_only = bc_mode == "kir-only";
    let kir_bc = bc_mode == "kir" || bc_mode == "kir,own" || kir_only;
    let kir_own = bc_mode == "kir,own" || kir_only;
    match find_root().and_then(|r| {
        full_check(&r, &mut rep, false).map(|(_, prog, user)| {
            crate::kir_lower::dump_hidden(prog.as_ref(), &user);
            if kir_bc {
                kir_borrowck_hidden(&mut rep, prog.as_ref(), &user, kir_own, kir_only);
            } else if bc_mode != "ast" {
                kir_borrowck_union(&mut rep, prog.as_ref(), &user);
            }
        })
    }) {
        Ok(()) => {}
        Err(e) => rep.add(&e.code, &e.msg, None, 1, 1, true),
    }
    attach_fixes(&mut rep.items);
    rep.flush();
    if rep.fatal() { 1 } else { 0 }
}

/// `--target <三つ組>` の検査の幅 (ポインタの幅)。省略と `host` は今の機械 (64 ビット)。未知の三つ組は E9040。
fn target_bits_arg(args: &[String]) -> Result<u32, String> {
    let Some(name) = args.iter().position(|a| a == "--target").and_then(|i| args.get(i + 1)) else { return Ok(64) };
    if name == "host" {
        return Ok(64);
    }
    match crate::kir_llvm::target_of(name) {
        Some(t) => Ok(t.ptr_bits),
        None => {
            let names: Vec<&str> = crate::kir_llvm::TARGETS.iter().map(|t| t.name).collect();
            Err(format!("ksn: E9040: unsupported target `{}` (supported: {})", name, names.join(", ")))
        }
    }
}

/// `Package::source` の印。標準ライブラリ (`spec/stdlib`) を 1 つのパッケージとして検査するときに使う。
const STDLIB_SOURCE: &str = "stdlib";

/// 標準ライブラリの本体 (`.ksnl`) を、利用者のパッケージと同じ検査 (文法・lint・型・効果・借用など) にかける読み込み。
/// `.sig` は規範の宣言なので、検査の対象の外 (`std` 側) に置いて参照だけできるようにする。
fn stdlib_loaded() -> Loaded {
    let root = stdlib_dir();
    let (bodies, sigs): (Vec<Source>, Vec<Source>) = load_stdlib().into_iter().partition(|s| s.path.ends_with(".ksnl"));
    Loaded {
        pkg: Package {
            name: "std".to_string(),
            root,
            manifest: toml::Value::Table(Default::default()),
            sources: bodies,
            source: STDLIB_SOURCE.to_string(),
        },
        deps: vec![],
        std: sigs,
    }
}

/// `ksn check-stdlib [--json]` (隠し機能): 標準ライブラリの本体の診断を出す。標準ライブラリを書き換えたあとの回帰確認用。
fn cmd_check_stdlib(json: bool) -> i32 {
    let mut rep = Report::new(json);
    if let Err(e) = full_check_loaded(stdlib_loaded(), &mut rep, false, false) {
        rep.add(&e.code, &e.msg, None, 1, 1, true);
    }
    attach_fixes(&mut rep.items);
    rep.flush();
    if rep.fatal() { 1 } else { 0 }
}

/// `ksn kir <FQN> [--run|--native <引数>... | --llvm]` (隠し機能、設計_KIR.md 段階 1 と 4)。
/// 関数の KIR を出す。`--run` を付けると、出した KIR を読み直して参照実行器で走らせる (木走査の `ksn run` と結果を比べるため)。
/// `--llvm` は LLVM IR (`.ll`) を出し、`--native` は clang で実行ファイルを作って走らせる (表示と終了コードは `--run` と同じ)。
fn cmd_kir(rest: &[String]) -> i32 {
    let Some(fqn) = rest.first() else {
        eprintln!("usage: ksn kir <FQN> [--opt] [--run|--native <args>... | --llvm]");
        return 2;
    };
    // --opt は最適化パスを適用する。--run/--native/--llvm は実行モード。
    let mut opt = false;
    let mut mode: Option<&str> = None;
    let mut mode_pos: Option<usize> = None;
    for (i, s) in rest[1..].iter().enumerate() {
        match s.as_str() {
            "--opt" => opt = true,
            "--run" | "--native" | "--llvm" if mode.is_none() => {
                mode = Some(s.as_str());
                mode_pos = Some(1 + i);
            }
            _ => {}
        }
    }
    let run = mode == Some("--run");
    let native = mode == Some("--native");
    let llvm = mode == Some("--llvm");
    let arg_start = mode_pos.map(|p| p + 1).unwrap_or(rest.len());
    let mut rep = Report::new(false);
    let (root, prog, user) = match find_root().and_then(|r| full_check(&r, &mut rep, false).map(|x| (r, x))) {
        Ok((r, (_, Some(prog), user))) if !rep.fatal() => (r, prog, user),
        Ok(_) => {
            rep.flush();
            return 1;
        }
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 2;
        }
    };
    let lo = crate::kir_lower::lower_user(&prog, &user);
    if lo.module.find(fqn).is_none() {
        match lo.unsupported.iter().find(|(k, _)| k == fqn) {
            Some((_, why)) => eprintln!("ksn kir: {} は未対応: {}", fqn, why),
            None => eprintln!("ksn kir: {} はユーザの関数ではない", fqn),
        }
        return 1;
    }
    let mut text = crate::kir::print_module(&lo.module.reachable(fqn));
    // 出力は必ず読み直せること (往復) と検査を通ることを確かめてから使う。
    let _back = match crate::kir::parse_module(&text) {
        Ok(m) if crate::kir::print_module(&m) == text && crate::kir::verify_module(&m).is_empty() => m,
        Ok(m) => {
            eprintln!("ksn kir: 内部エラー: KIR の往復または検査が一致しない");
            for e in crate::kir::verify_module(&m).iter().take(5) {
                eprintln!("  検査: {} {}", e.0, e.1);
            }
            if crate::kir::print_module(&m) != text {
                eprintln!("  往復: 読み直して出力した形が元と違う");
            }
            return 3;
        }
        Err(e) => {
            eprintln!("ksn kir: 内部エラー: {}", e);
            return 3;
        }
    };
    // --opt: 決定的な最適化パスを適用する (意味を変えず命令数を減らす)。
    // 実行・生成には、往復を確かめた元の (extern の link を持つ) モジュールを使う。
    let mut exe = lo.module.reachable(fqn);
    if opt {
        match crate::kir_opt::optimize(&exe) {
            Ok(m) => {
                exe = m;
                text = crate::kir::print_module(&exe);
            }
            Err(e) => {
                eprintln!("ksn kir: --opt: {}", e);
                return 3;
            }
        }
    }
    if llvm {
        return match crate::kir_llvm::emit(&exe, fqn) {
            Ok(ll) => {
                print!("{}", ll);
                0
            }
            Err(e) => {
                eprintln!("ksn kir: --llvm: {}", e);
                2
            }
        };
    }
    if !run && !native {
        print!("{}", text);
        return 0;
    }
    let mut args = vec![];
    for a in &rest[arg_start..] {
        let v = match a.as_str() {
            "true" => 1,
            "false" => 0,
            s => match s.parse::<i128>() {
                Ok(v) => v,
                Err(_) => {
                    eprintln!("ksn kir: 引数 {} は整数か bool のはず", s);
                    return 2;
                }
            },
        };
        args.push(v);
    }
    let ret = exe.find(fqn).map(|f| f.ret);
    let show = |v: i128| match ret {
        Some(t) => println!("{}", crate::kir::show_value(t, v)),
        None => println!("{}", v),
    };
    if native {
        return kir_native(&root, &exe, fqn, &args, show);
    }
    match crate::kir_exec::run(&exe, fqn, &args) {
        Ok(crate::kir_exec::Outcome::Value(v)) => {
            show(v);
            0
        }
        Ok(crate::kir_exec::Outcome::Panic(m)) => {
            eprintln!("panic: {}", m);
            101
        }
        Ok(crate::kir_exec::Outcome::Abort) => 101,
        Ok(crate::kir_exec::Outcome::Exit(c)) => c,
        Ok(crate::kir_exec::Outcome::StackOverflow) => {
            eprintln!("stack overflow");
            102
        }
        Err(e) => {
            eprintln!("ksn kir: {}", e);
            2
        }
    }
}

/// `ksn kir <FQN> --native`: KIR から LLVM IR を作り、clang で `target/kir-native/` に実行ファイルを作って走らせる。
/// 実行ファイルは結果を 16 進の 2 語 (下位・上位) で出すので、ここで `show_value` の表示にする。
/// 同じ `.ll` (内容の FNV-1a で名前を付ける) の実行ファイルは作り直さない。
fn kir_native(root: &Path, m: &crate::kir::Module, fqn: &str, args: &[i128], show: impl Fn(i128)) -> i32 {
    let ll = match crate::kir_llvm::emit(m, fqn) {
        Ok(ll) => ll,
        Err(e) => {
            eprintln!("ksn kir: --native: {}", e);
            return 2;
        }
    };
    let clang = std::env::var("KSN_CLANG").unwrap_or_else(|_| "clang".to_string());
    let dir = root.join("target").join("kir-native");
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("ksn kir: --native: {} を作れない: {}", dir.display(), e);
        return 2;
    }
    let mut h: u64 = 0xcbf29ce484222325;
    let extra = clang_extra_args();
    for b in ll.bytes().chain(extra.join(" ").bytes()) {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let exe = dir.join(format!("{:016x}{}", h, if cfg!(windows) { ".exe" } else { "" }));
    if !exe.exists() {
        let src = dir.join(format!("{:016x}.ll", h));
        if let Err(e) = fs::write(&src, &ll) {
            eprintln!("ksn kir: --native: {} を書けない: {}", src.display(), e);
            return 2;
        }
        let tmp = dir.join(format!("{:016x}.{}.tmp", h, std::process::id()));
        let rt = runtime_c_args(&ll, &src);
        let o = std::process::Command::new(&clang).args(["-O1", "-Wno-override-module"]).args(&extra).args(["-x", "ir"]).arg(&src).args(&rt).arg("-o").arg(&tmp).arg("-lm").arg("-pthread").output();
        match o {
            Err(e) => {
                eprintln!("ksn kir: --native には clang が必要です ({} を起動できない: {})", clang, e);
                return 2;
            }
            Ok(o) if !o.status.success() => {
                eprintln!("ksn kir: --native: clang が失敗した ({}): {}", src.display(), String::from_utf8_lossy(&o.stderr).trim());
                let _ = fs::remove_file(&tmp);
                return 3;
            }
            Ok(_) => {}
        }
        if let Err(e) = fs::rename(&tmp, &exe) {
            eprintln!("ksn kir: --native: {}", e);
            return 2;
        }
    }
    let mut cmd = std::process::Command::new(&exe);
    for a in args {
        let u = *a as u128;
        cmd.arg(format!("{:x}", u as u64)).arg(format!("{:x}", (u >> 64) as u64));
    }
    let o = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("ksn kir: --native: 実行できない: {}", e);
            return 2;
        }
    };
    eprint!("{}", String::from_utf8_lossy(&o.stderr));
    match o.status.code() {
        Some(0) => {
            // 標準出力の最後の `\x01` から後ろが結果 (16 進の 2 語)。その前は、プログラム自身の出力 (`write`)。
            let raw = String::from_utf8_lossy(&o.stdout).to_string();
            let (prog_out, out) = match raw.rfind('\x01') {
                Some(i) => (&raw[..i], &raw[i + 1..]),
                None => ("", raw.as_str()),
            };
            print!("{}", prog_out);
            let mut it = out.split_whitespace().map(|w| u64::from_str_radix(w, 16));
            match (it.next(), it.next()) {
                (Some(Ok(lo)), Some(Ok(hi))) => {
                    show((((hi as u128) << 64) | lo as u128) as i128);
                    0
                }
                _ => {
                    eprintln!("ksn kir: --native: 実行ファイルの出力が読めない: {}", out.trim());
                    3
                }
            }
        }
        Some(c) => c,
        None => {
            eprintln!("ksn kir: --native: 実行ファイルが異常終了した ({:?})", o.status);
            3
        }
    }
}

/// `ksn todo`: `core.panic.todo()` の呼び出し (未実装の穴) を一覧する。
fn cmd_todo() -> i32 {
    let root = match find_root().and_then(|r| load_package(&r)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 2;
        }
    };
    let mut n = 0;
    for s in &root.sources {
        for (i, line) in s.lines.iter().enumerate() {
            if line.contains("core.panic.todo(") {
                println!("{}:{}: {}", rel(&s.path), i + 1, line.trim());
                n += 1;
            }
        }
    }
    println!("{} todo(s)", n);
    0
}

/// `ksn fix`: `check` の修正案をすべて適用し、`fmt` で整える。
fn cmd_fix() -> i32 {
    let mut total = 0usize;
    for _ in 0..5 {
        let mut rep = Report::new(false);
        let root = match find_root() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("ksn: {}: {}", e.code, e.msg);
                return 2;
            }
        };
        if let Err(e) = full_check(&root, &mut rep, false) {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 2;
        }
        attach_fixes(&mut rep.items);
        let mut by_file: HashMap<String, Vec<crate::fixes::Fix>> = HashMap::new();
        for it in &rep.items {
            for f in &it.fix {
                by_file.entry(f.file.clone()).or_default().push(f.clone());
            }
        }
        if by_file.is_empty() {
            break;
        }
        let mut applied = 0;
        for (file, fixes) in by_file {
            let text = fs::read_to_string(&file).unwrap_or_default();
            let (new, n) = crate::fixes::apply(&text, &fixes);
            if n > 0 && new != text {
                fs::write(&file, new).unwrap();
                applied += n;
            }
        }
        if applied == 0 {
            break;
        }
        total += applied;
    }
    println!("applied {} fix(es)", total);
    let code = cmd_fmt(&[]);
    if code != 0 {
        return code;
    }
    cmd_check(false)
}

fn run_args(args: &[String]) -> (bool, bool, Vec<String>) {
    let mut json = false;
    let mut watch = false;
    let mut rest = vec![];
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "--watch" => watch = true,
            _ => rest.push(a.clone()),
        }
    }
    (json, watch, rest)
}

/// デバッガ用の読み込み: 検査して Program を作る。致命的な診断があれば、その本文を Err で返す。
pub(crate) fn load_for_debug() -> Result<crate::debug::Target, String> {
    let mut rep = Report::new(false);
    let root = find_root().map_err(|e| format!("ksn: {}: {}", e.code, e.msg))?;
    let (l, prog, user) = full_check(&root, &mut rep, true).map_err(|e| format!("ksn: {}: {}", e.code, e.msg))?;
    rep.finish();
    if rep.fatal() {
        let lines: Vec<String> = rep
            .items
            .iter()
            .filter(|it| it.fatal)
            .map(|it| match &it.file {
                Some(f) => format!("{}:{}:{}: {}: {}", f, it.line, it.col, it.code, it.message),
                None => format!("<project>: {}: {}", it.code, it.message),
            })
            .collect();
        return Err(lines.join("\n"));
    }
    let prog = Rc::new(prog.unwrap());
    let entry_name = l.pkg.entry().unwrap_or_default();
    let entry = prog.funcs.get(&entry_name).copied();
    let tests: Vec<usize> = user.iter().copied().filter(|&i| prog.defs[i].kw == "test").collect();
    let user_files = user.iter().map(|&i| prog.defs[i].file.clone()).collect();
    Ok(crate::debug::Target { prog, entry, entry_name, tests, user_files })
}

/// `--engine=vm|tree|both` を取り除く。1 つ目は VM を使うか (`ksn run` / `ksn test`。既定は木走査)、
/// 2 つ目は `both` (`ksn test` だけ: 木走査と VM の両方で走らせて結果を比べる)。
fn take_engine(args: &[String]) -> Result<(bool, bool, Vec<String>), String> {
    let mut vm = false;
    let mut cmp = false;
    let mut rest = vec![];
    for a in args {
        match a.strip_prefix("--engine=") {
            Some("vm") => vm = true,
            Some("tree") => vm = false,
            Some("both") => {
                vm = true;
                cmp = true;
            }
            Some(o) => return Err(format!("--engine は vm か tree (指定: {})", o)),
            None => rest.push(a.clone()),
        }
    }
    Ok((vm, cmp, rest))
}

fn cmd_run(args: &[String]) -> i32 {
    let (use_vm, cmp, args) = match take_engine(args) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("ksn: {}", e);
            return 2;
        }
    };
    if cmp {
        eprintln!("ksn: --engine=both は ksn test だけ (木走査と VM の結果を比べる)");
        return 2;
    }
    let args = &args[..];
    let (json, watch, rest) = run_args(args);
    let mut rep = Report::new(json);
    crate::vm::tick("start");
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            rep.add(&e.code, &e.msg, None, 1, 1, true);
            rep.flush();
            return 1;
        }
    };
    let (l, prog, user) = match full_check_opts(&root, &mut rep, true, !watch) {
        Ok(x) => x,
        Err(e) => {
            rep.add(&e.code, &e.msg, None, 1, 1, true);
            rep.flush();
            return 1;
        }
    };
    rep.flush();
    crate::vm::tick("full_check");
    if rep.fatal() {
        return 1;
    }
    let Some(entry) = l.pkg.entry() else {
        eprintln!("ksn: E9030: this package is a library (no `entry` in kasane.toml)");
        return 1;
    };
    let prog = prog.unwrap();
    let Some(&di) = prog.funcs.get(&entry) else {
        eprintln!("ksn: E9030: entry function {} not found", entry);
        return 1;
    };
    if use_vm {
        let mk: Option<Box<dyn FnOnce() -> crate::vm::reload::Reload + '_>> = if watch {
            let (l, entry) = (&l, entry.clone());
            Some(Box::new(move || {
                let (tx, rx) = std::sync::mpsc::channel::<(Program, Vec<usize>)>();
                let flag: &'static std::sync::atomic::AtomicBool = Box::leak(Box::new(std::sync::atomic::AtomicBool::new(false)));
                spawn_watcher_with(l, true, move |p, u| {
                    let ok = tx.send((p, u)).is_ok();
                    flag.store(true, std::sync::atomic::Ordering::Release);
                    ok
                });
                crate::vm::reload::Reload { rx, flag, entry }
            }))
        } else {
            None
        };
        match crate::vm::run_entry(&prog, &user, &entry, &rest, mk) {
            Ok(code) => {
                // 終了時の解放 (標準ライブラリの定義表など。約 1 ms) を省く。このあとはプロセスが終わるだけ。
                std::mem::forget(prog);
                std::mem::forget(l);
                return code;
            }
            Err(why) => eprintln!("vm engine skipped: {}; falling back to the tree walker", first_line(&why)),
        }
    }
    let prog = Rc::new(prog);
    let mut it = Interp::new(prog);
    it.args = rest;
    if watch {
        it.reload = Some(spawn_watcher(&l));
    }
    let child_code = it.run_entry(di);
    child_code
}

/// ホットリロード: 外部面が不変の関数本体の変更だけを反映する。
fn spawn_watcher(l: &Loaded) -> std::sync::mpsc::Receiver<Program> {
    let (tx, rx) = std::sync::mpsc::channel::<Program>();
    spawn_watcher_with(l, false, move |p, _| tx.send(p).is_ok());
    rx
}

/// 監視スレッドを起こす。反映できる変更があれば、再構築した `Program` と利用者の定義の添字を `send` に渡す (false が返ったら終わる)。
/// `allow_add` (VM) のときは、新しい `fn` の追加 (名前が増えるだけで、ほかの定義の集合は同じ) も即時に反映する。
fn spawn_watcher_with(l: &Loaded, allow_add: bool, send: impl Fn(Program, Vec<usize>) -> bool + Send + 'static) {
    let mut std_defs: Vec<Def> = vec![];
    for s in &l.std {
        std_defs.extend(s.defs.clone());
    }
    let mut dep_defs: Vec<Def> = vec![];
    for dp in &l.deps {
        for s in &dp.sources {
            dep_defs.extend(s.defs.clone());
        }
    }
    let srcroot = norm(&l.pkg.root.join("src"));
    struct W {
        path: String,
        lines: Vec<String>,
        defs: Vec<Def>,
        mtime: Option<std::time::SystemTime>,
    }
    let mut ws: Vec<W> = l
        .pkg
        .sources
        .iter()
        .filter(|s| !s.is_test)
        .map(|s| W {
            path: s.path.clone(),
            lines: s.lines.clone(),
            defs: s.defs.clone(),
            // 基準の時刻は持たずに始める。読み込みから監視を始めるまでの間に書き換わった変更も、
            // 最初の見回りで中身を比べて拾う (中身が同じなら何もしない)。
            mtime: None,
        })
        .collect();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut changed = false;
        let mut restart = false;
        for w in ws.iter_mut() {
            let m = fs::metadata(&w.path).and_then(|m| m.modified()).ok();
            if m == w.mtime {
                continue;
            }
            w.mtime = m;
            let new = Source::load(&w.path, &srcroot);
            if new.error.is_none() && new.lines == w.lines {
                continue;
            }
            if let Some(e) = &new.error {
                eprintln!("hot reload: {}: {} (kept old code)", rel(&w.path), e.msg);
                continue;
            }
            let old: HashMap<&str, &Def> = w.defs.iter().filter(|d| d.kw != "import").map(|d| (d.name.as_str(), d)).collect();
            let cur: HashMap<&str, &Def> = new.defs.iter().filter(|d| d.kw != "import").map(|d| (d.name.as_str(), d)).collect();
            let mut same_names: Vec<&&str> = old.keys().collect();
            same_names.sort();
            let mut cur_names: Vec<&&str> = cur.keys().collect();
            cur_names.sort();
            let mut added = false;
            if same_names != cur_names {
                let only_fn_added = allow_add
                    && old.keys().all(|k| cur.contains_key(k))
                    && cur.iter().filter(|(k, _)| !old.contains_key(*k)).all(|(_, d)| d.kw == "fn" && d.generics.is_empty() && d.body.is_some());
                if !only_fn_added {
                    restart = true;
                    continue;
                }
                added = true;
            }
            let mut ok = true;
            for (n, d) in &cur {
                let Some(o) = old.get(n) else { continue };
                if d.kw == "fn" {
                    if d.header != o.header || !d.generics.is_empty() {
                        ok = false;
                    } else if d.body.is_some() {
                        changed = true;
                    }
                } else if matches!(d.kw.as_str(), "type" | "enum" | "trait" | "const" | "static" | "impl") {
                    let a = &new.lines[d.line.min(new.lines.len())..d.end_line.max(d.line).min(new.lines.len())];
                    let b = &w.lines[o.line.min(w.lines.len())..o.end_line.max(o.line).min(w.lines.len())];
                    if d.header != o.header || a != b {
                        ok = false;
                    }
                }
            }
            if !ok {
                restart = true;
                continue;
            }
            if added {
                changed = true;
            }
            w.lines = new.lines.clone();
            w.defs = new.defs.clone();
        }
        if restart {
            eprintln!("hot reload: header/type change, restart required");
        }
        if changed {
            let mut prog = empty_program();
            for d in &std_defs {
                prog.add_def(d.clone());
            }
            let n_std = prog.defs.len();
            for d in &dep_defs {
                prog.add_def(d.clone());
            }
            let mut user = vec![];
            for w in &ws {
                for d in &w.defs {
                    user.push(prog.defs.len());
                    prog.add_def(d.clone());
                }
            }
            crate::derive_json::expand(&mut prog, n_std, &mut user);
            crate::infer::run(&mut prog);
            crate::infer::annotate_int_lits(&mut prog, &user);
            crate::infer::annotate_owned_drops(&mut prog, &user);
            if !send(prog, user) {
                return;
            }
        }
    });
}

fn cmd_test(args: &[String]) -> i32 {
    // `ksn test --debug [--json] <テスト名>`: そのテスト 1 つをデバッガの下で走らせる (debug.rs)
    if args.iter().any(|a| a == "--debug") {
        let name = args.iter().find(|a| !a.starts_with("--"));
        let Some(name) = name else {
            eprintln!("usage: ksn test --debug [--json] <test FQN>");
            return 2;
        };
        let mut dargs: Vec<String> = args.iter().filter(|a| *a == "--json").cloned().collect();
        dargs.extend(["--test".to_string(), name.clone()]);
        return crate::debug::cmd_debug(&dargs);
    }
    let (use_vm, cmp, args) = match take_engine(args) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("ksn: {}", e);
            return 2;
        }
    };
    let mut both = false;
    let mut doc_only = false;
    let mut prefix: Option<String> = None;
    for a in &args {
        if a == "--both" {
            both = true;
        } else if a == "--doc" {
            doc_only = true;
        } else {
            prefix = Some(a.clone());
        }
    }
    let mut rep = Report::new(false);
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            rep.add(&e.code, &e.msg, None, 1, 1, true);
            rep.flush();
            return 1;
        }
    };
    let (l, prog, user) = match full_check_with_examples(&root, &mut rep) {
        Ok(x) => x,
        Err(e) => {
            rep.add(&e.code, &e.msg, None, 1, 1, true);
            rep.flush();
            return 1;
        }
    };
    if rep.fatal() {
        rep.flush();
        return 1;
    }
    rep.flush();
    if doc_only {
        // `--doc`: `src/` の `///` のコード例 (tests/ の仮想ファイル doc_<番号>.ksnl) だけを走らせる (3.2)。
        prefix = Some(format!("{}.tests.doc_", l.pkg.name));
    }
    let prog = Rc::new(prog.unwrap());
    let mut tests: Vec<usize> = user
        .iter()
        .copied()
        .filter(|&i| prog.defs[i].kw == "test" && prefix.as_ref().map_or(true, |p| prog.defs[i].name.starts_with(p.as_str())))
        .collect();
    tests.sort_by(|&a, &b| prog.defs[a].name.cmp(&prog.defs[b].name));
    let mut failed = 0;
    let (mut n_llvm, mut n_bundle) = (0, 0);
    let (mut n_vm, mut n_vm_skipped) = (0, 0);
    for ti in &tests {
        let name = prog.defs[*ti].name.clone();
        let mut results: Vec<Option<String>> = vec![];
        let mut names: Vec<&str> = vec![];
        let mut vm_note = String::new();
        let vm_result = if use_vm {
            match crate::vm::run_test(&prog, &user, *ti) {
                Ok(r) => Some(r),
                Err(why) => {
                    eprintln!("test {}: vm engine skipped: {}; falling back to the tree walker", name, first_line(&why));
                    vm_note = format!("vm skipped: {}", first_line(&why));
                    None
                }
            }
        } else {
            None
        };
        // 木走査は、VM を使わない・VM が退避した・`--engine=both` のときに走らせる (比べるときは木走査が先)。
        if vm_result.is_none() || cmp {
            let mut it = Interp::new(prog.clone());
            let r = it.call_body(*ti);
            use std::io::Write;
            let _ = it.out.flush();
            results.push(match r {
                Err(Ctl::Panic(m)) => Some(m),
                Err(Ctl::Exit(c)) => Some(format!("exit({})", c)),
                _ => None,
            });
            names.push("tree");
        }
        if let Some(r) = vm_result {
            results.push(r);
            names.push("vm");
            if cmp {
                n_vm += 1;
            }
        } else if cmp {
            n_vm_skipped += 1;
        }
        // `--both` の相手は、KIR に下ろせればその LLVM の実行ファイル。下ろせない・clang が無いときは、
        // 理由を出してバンドル (インタプリタ同梱) で走らせる。
        let mut other = "";
        let mut label = String::new();
        if both {
            match build_llvm_test(&root, &prog, &user, *ti) {
                Ok(exe) => {
                    results.push(run_native_test(&root, &exe));
                    other = "llvm";
                    label = "both: llvm".into();
                }
                Err(why_llvm) => {
                    results.push(run_in_bundle(&root, &name, &l));
                    other = "bundle";
                    label = format!("both: bundle (llvm skipped: {})", first_line(&why_llvm));
                }
            }
        }
        if !other.is_empty() {
            names.push(other);
        }
        if !vm_note.is_empty() && cmp {
            label = if label.is_empty() { vm_note.clone() } else { format!("{}; {}", label, vm_note) };
        }
        let suffix = if label.is_empty() { String::new() } else { format!(" ({})", label) };
        match other {
            "llvm" => n_llvm += 1,
            "bundle" => n_bundle += 1,
            _ => {}
        }
        let disagree = results.len() > 1 && results[1..].iter().any(|r| backends_disagree(&results[0], r));
        if disagree {
            failed += 1;
            let who = if cmp { names.join(", ") } else { format!("interpreter and {}", other) };
            println!("test {} ... FAILED: {} disagree: {}{}", name, who, py_opt_list(&results), suffix);
        } else if results.iter().all(|r| r.is_none()) {
            println!("test {} ... ok{}", name, suffix);
        } else {
            failed += 1;
            println!("test {} ... FAILED: {}{}", name, results.iter().flatten().next().unwrap(), suffix);
        }
    }
    println!("\n{} passed; {} failed", tests.len() - failed, failed);
    if both {
        println!("compared with: llvm {}, bundle fallback {}", n_llvm, n_bundle);
    }
    if cmp {
        println!("compared tree walker with vm: {} (vm skipped {})", n_vm, n_vm_skipped);
    }
    if failed > 0 { 1 } else { 0 }
}

/// 理由の最初の行 (前後の空白を除く)。
fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

/// 成否が違う、または両方失敗でも理由の分類が違えば不一致。
fn backends_disagree(a: &Option<String>, b: &Option<String>) -> bool {
    match (a, b) {
        (None, None) => false,
        (Some(a), Some(b)) => fail_class(a) != fail_class(b),
        _ => true,
    }
}

/// 失敗理由を、バックエンドの文言の違いによらない分類にする。
/// インタプリタは `attempt to add with overflow` / `assertion failed: left: .., right: ..`、機械語は
/// `arithmetic overflow` / `assertion failed` と書く。文言を揃えずに分類で比べる (組み込みの文言を各所で変えるより小さい)。
fn fail_class(msg: &str) -> String {
    let m = msg.trim().trim_start_matches("panic: ");
    if let Some(c) = m.strip_prefix("exit(").and_then(|r| r.strip_suffix(')')).or_else(|| m.strip_prefix("exit ")) {
        return format!("exit {}", c);
    }
    let kind = if m.contains("overflow") {
        "overflow"
    } else if m.contains("assertion failed") {
        "assert"
    } else if m.contains("unwrap") || m.contains("expect") {
        "unwrap"
    } else if m.contains("by zero") {
        "divide"
    } else if m.contains("out of bounds") {
        "index"
    } else {
        "other"
    };
    kind.to_string()
}

fn py_opt_list(rs: &[Option<String>]) -> String {
    format!(
        "[{}]",
        rs.iter().map(|r| r.as_ref().map_or("None".to_string(), |s| py_repr(s))).collect::<Vec<_>>().join(", ")
    )
}

fn exe_name() -> &'static str {
    if cfg!(windows) { "ksn.exe" } else { "ksn" }
}

fn run_in_bundle(root: &Path, fqn: &str, l: &Loaded) -> Option<String> {
    let bdir = root.join("target").join("debug");
    // 既存のバンドルは、今のキー (ksn 本体・ソース) で作られたものだけ使う。
    // 存在だけで判定すると、ソースや ksn を変えても古いバンドルで走ってしまう。
    let want = build_key(l, false, "host", "c");
    let fresh = fs::read_to_string(bdir.join(BUILD_STAMP)).map_or(false, |s| s.trim() == want);
    if !(fresh && bdir.join(exe_name()).exists()) && cmd_build(&[], Some(l)) != 0 {
        return Some("build failed".into());
    }
    let r = std::process::Command::new(bdir.join(exe_name()))
        .args(["_exec", "--test", fqn])
        .env("KSN_STDLIB", bdir.join("stdlib"))
        .current_dir(bdir.join("pkg"))
        .output();
    match r {
        Ok(o) if o.status.success() => None,
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
            Some(if e.is_empty() { format!("exit {}", o.status.code().unwrap_or(-1)) } else { e })
        }
        Err(e) => Some(e.to_string()),
    }
}

/// 機械語のテストを 1 プロセスで走らせる。インタプリタと同じ形 (成功は None、失敗は理由) で返す。
fn run_native_test(root: &Path, exe: &Path) -> Option<String> {
    match std::process::Command::new(exe).current_dir(root).output() {
        Ok(o) if o.status.success() => None,
        // 102 は stack 超過 (機械語は stderr に `stack overflow` と書く)。木走査は `Exit(102)` なので、終了の種類で比べる。
        Ok(o) if o.status.code() == Some(102) => Some("exit(102)".into()),
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
            Some(if e.is_empty() { format!("exit {}", o.status.code().unwrap_or(-1)) } else { e })
        }
        Err(e) => Some(e.to_string()),
    }
}

// ------------------------------------------------------------ get / sig / refs / rename
struct Everything {
    l: Loaded,
}

impl Everything {
    fn sources(&self) -> Vec<&Source> {
        let mut out: Vec<&Source> = self.l.pkg.sources.iter().collect();
        for dp in &self.l.deps {
            out.extend(dp.sources.iter());
        }
        out.extend(self.l.std.iter());
        out
    }
}

fn load_everything() -> Result<Everything, ProjectError> {
    let root = find_root()?;
    let l = load_all(&root)?;
    Ok(Everything { l })
}

pub(crate) enum Found<'a> {
    Def(&'a Def, &'a Source),
    #[allow(dead_code)]
    Line(&'a Source, usize),
}

pub(crate) fn find_def<'a>(srcs: &[&'a Source], fqn: &str) -> Option<Found<'a>> {
    for s in srcs {
        for d in &s.defs {
            if d.name == fqn && matches!(d.kw.as_str(), "fn" | "type" | "enum" | "trait" | "test" | "const" | "static" | "extern") {
                return Some(Found::Def(d, s));
            }
        }
    }
    let (head, last) = fqn.rsplit_once('.')?;
    for s in srcs {
        for d in &s.defs {
            if d.name == head {
                match d.kw.as_str() {
                    "type" => {
                        if let Some(k) = d.fields.iter().position(|f| f.0 == last) {
                            return Some(Found::Line(s, d.line + 1 + k));
                        }
                    }
                    "enum" => {
                        if let Some(k) = d.variants.iter().position(|f| f.0 == last) {
                            return Some(Found::Line(s, d.line + 1 + k));
                        }
                    }
                    "trait" => {
                        if let Some(k) = d.methods.iter().position(|m| m == last) {
                            return Some(Found::Line(s, d.line + 1 + k));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    None
}

pub(crate) fn def_start(s: &Source, d: &Def) -> usize {
    let mut i = d.line - 1;
    while i > 0 && s.lines[i - 1].starts_with("//") {
        i -= 1;
    }
    i
}

fn derived_impls(d: &Def) -> Option<String> {
    if !matches!(d.kw.as_str(), "type" | "enum") || d.derive.is_empty() {
        return None;
    }
    let n = &d.name;
    let mut out: Vec<String> = vec![];
    for tr in &d.derive {
        match tr.as_str() {
            "Copy" => out.push(format!("impl core.ops.Copy for {}", n)),
            "Eq" => out.push(format!("impl core.cmp.Eq for {}", n)),
            "Clone" => {
                out.push(format!(
                    "impl core.ops.Clone for {0}\nfn {0}.clone(self: &{0}) -> {0} uses [] panics never impls core.ops.Clone",
                    n
                ));
                if d.kw == "type" {
                    out.push(format!(
                        "    {} {{ {} }}",
                        n,
                        d.fields.iter().map(|f| format!("{0}: self.{0}.clone()", f.0)).collect::<Vec<_>>().join(", ")
                    ));
                } else {
                    out.push("    // clone each variant payload".into());
                }
                out.push(format!("end {}.clone", n));
            }
            "TryClone" => {
                out.push(format!(
                    "impl alloc.alloc.TryClone for {0}\nfn {0}.try_clone(self: &{0}) -> Result<{0}, alloc.alloc.AllocError> uses [alloc] panics never impls alloc.alloc.TryClone",
                    n
                ));
                if d.kw == "type" {
                    out.push(format!(
                        "    Ok({} {{ {} }})",
                        n,
                        d.fields.iter().map(|f| format!("{0}: self.{0}.try_clone()?", f.0)).collect::<Vec<_>>().join(", ")
                    ));
                } else {
                    out.push("    // try_clone each variant payload, propagating the error with `?`".into());
                }
                out.push(format!("end {}.try_clone", n));
            }
            "PartialEq" => {
                out.push(format!(
                    "impl core.cmp.PartialEq<{0}> for {0}\nfn {0}.eq(self: &{0}, other: &{0}) -> bool uses [] panics never impls core.cmp.PartialEq<{0}>",
                    n
                ));
                if d.kw == "type" {
                    out.push(format!(
                        "    {}",
                        d.fields.iter().map(|f| format!("self.{0} == other.{0}", f.0)).collect::<Vec<_>>().join(" && ")
                    ));
                } else {
                    out.push("    // equal variants with equal payloads".into());
                }
                out.push(format!("end {}.eq", n));
            }
            "From" => {
                // `?` が包むバリアントを、ペイロードの型ごとに 1 行で示す (4.1)。同じ型を持つバリアントが複数あると曖昧 (E2049)。
                out.push(format!("// derive From for {}: `e?` with `e: Result<_, E1>` wraps the error as the variant", n));
                let keyed: Vec<(String, &String)> = d
                    .variants
                    .iter()
                    .filter(|(_, pl)| pl.len() == 1)
                    .map(|(vn, pl)| (crate::types::from_v(&pl[0], &std::collections::HashSet::new()).show(), vn))
                    .collect();
                for (ty, vn) in &keyed {
                    let same: Vec<&str> = keyed.iter().filter(|(t, _)| t == ty).map(|(_, v)| v.as_str()).collect();
                    if same.len() > 1 {
                        out.push(format!("//     E1 = {}  =>  ambiguous ({}): `?` is an error, use map_err", ty, same.join(", ")));
                    } else {
                        out.push(format!("//     E1 = {}  =>  {}.{}(error)", ty, n, vn));
                    }
                }
            }
            "Drop" => {
                out.push(format!(
                    "impl core.ops.Drop for {0}\nfn {0}.drop(self: &mut {0}) -> () uses [] panics never impls core.ops.Drop",
                    n
                ));
                out.push("    // built in: an iterative drop whose depth does not depend on the shape of the value".into());
                out.push("    // (the Box<Self> children go onto a work list kept in the freed parents; no allocation, no recursion)".into());
                out.push(format!("end {}.drop", n));
            }
            "ToJson" | "FromJson" => out.push(format!("// {}: expanded to ordinary Kasane code (see below)", tr)),
            other => out.push(format!("// {}: built-in derived implementation for {}", other, n)),
        }
        out.push(String::new());
    }
    Some(out.join("\n").trim_end().to_string())
}

fn cmd_get(args: &[String]) -> i32 {
    let derived = args.iter().any(|a| a == "--derived");
    let Some(fqn) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("ksn: usage: ksn get [--derived] <FQN>");
        return 2;
    };
    let ev = match load_everything() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let srcs = ev.sources();
    match find_def(&srcs, fqn) {
        None => {
            eprintln!("ksn: {} not found", fqn);
            1
        }
        Some(Found::Line(s, ln)) => {
            println!("{}", s.lines[ln - 1]);
            0
        }
        Some(Found::Def(d, s)) => {
            if derived {
                match derived_impls(d) {
                    Some(o) => {
                        println!("{}", o);
                        if d.derive.iter().any(|x| x == "ToJson" || x == "FromJson") {
                            let (prog, _) = build_program(&ev.l);
                            match crate::derive_json::text_for(&prog, d) {
                                Ok(t) => println!("\n{}", t.trim_end()),
                                Err(m) => println!("\n// cannot expand: {}", m),
                            }
                        }
                    }
                    None => {
                        eprintln!("ksn: {} has no derive", fqn);
                        return 1;
                    }
                }
                return 0;
            }
            let start = def_start(s, d);
            let end = if d.end_line > 0 { d.end_line } else { d.line };
            println!("{}", s.lines[start..end].join("\n"));
            0
        }
    }
}

/// `ksn context <FQN>`: 定義全体と、そこから参照される定義の署名 (型は全体) を出力する。
/// 小さな文脈で 1 つの定義を読み書きするための表示。
fn cmd_context(args: &[String]) -> i32 {
    let Some(fqn) = args.first() else {
        eprintln!("ksn: usage: ksn context <FQN>");
        return 2;
    };
    let ev = match load_everything() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let srcs = ev.sources();
    let Some(Found::Def(d, s)) = find_def(&srcs, fqn) else {
        eprintln!("ksn: {} not found", fqn);
        return 1;
    };
    let start = def_start(s, d);
    let end = if d.end_line > 0 { d.end_line } else { d.line };
    let text = s.lines[start..end].join("
");
    println!("// target: {}", d.name);
    println!("{}", text);
    let mut refs: Vec<(String, String)> = vec![];
    for s2 in &srcs {
        for d2 in &s2.defs {
            if d2.name == d.name || !matches!(d2.kw.as_str(), "fn" | "type" | "enum" | "trait" | "const" | "static") {
                continue;
            }
            if d2.kw == "fn" && d2.name.contains('.') && !fqn_occurs(&text, &d2.name, false) {
                continue;
            }
            if d2.kw != "fn" && !fqn_occurs(&text, &d2.name, false) {
                continue;
            }
            let shown = if d2.kw == "fn" || d2.kw == "const" || d2.kw == "static" {
                d2.header.clone()
            } else {
                let st = def_start(s2, d2);
                let en = if d2.end_line > 0 { d2.end_line } else { d2.line };
                s2.lines[st..en].join("
")
            };
            refs.push((d2.name.clone(), shown));
        }
    }
    refs.sort();
    refs.dedup();
    if !refs.is_empty() {
        println!();
        println!("// referenced definitions (signatures)");
        for (_, shown) in refs {
            println!("{}", shown);
        }
    }
    0
}

fn cmd_sig(args: &[String]) -> i32 {
    let Some(fqn) = args.first() else {
        eprintln!("ksn: usage: ksn sig <FQN>");
        return 2;
    };
    let ev = match load_everything() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    match find_def(&ev.sources(), fqn) {
        None => {
            eprintln!("ksn: {} not found", fqn);
            1
        }
        Some(Found::Line(s, ln)) => {
            println!("{}", s.lines[ln - 1]);
            0
        }
        Some(Found::Def(d, _)) => {
            println!("{}", d.header);
            0
        }
    }
}

/// `build_program` が定義を登録した順 (標準ライブラリ → 依存 → 本体) に、文書化する定義を対応づける。
/// 戻り値は (標準ライブラリの定義, 本体の定義)。
fn doc_items<'a>(l: &'a Loaded, prog: &'a Program) -> (Vec<crate::doc::Item<'a>>, Vec<crate::doc::Item<'a>>) {
    let mut std_items = vec![];
    let mut pkg_items = vec![];
    let mut k = 0;
    for s in &l.std {
        for _ in &s.defs {
            std_items.push(crate::doc::Item { d: &prog.defs[k], lines: &s.lines });
            k += 1;
        }
    }
    for dp in &l.deps {
        for s in &dp.sources {
            k += s.defs.len();
        }
    }
    for s in &l.pkg.sources {
        for _ in &s.defs {
            pkg_items.push(crate::doc::Item { d: &prog.defs[k], lines: &s.lines });
            k += 1;
        }
    }
    (std_items, pkg_items)
}

/// `ksn index [--std]`: LLM 向けの索引。1 定義 1 行の JSON (FQN のバイト順)。仕様 7 章。
fn cmd_index(args: &[String]) -> i32 {
    for a in args {
        if a != "--std" {
            eprintln!("ksn: usage: ksn index [--std]");
            return 2;
        }
    }
    let with_std = args.iter().any(|a| a == "--std");
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let l = match load_all(&root) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let (prog, _) = build_program(&l);
    let (std_items, pkg_items) = doc_items(&l, &prog);
    let mut module_docs = std::collections::BTreeMap::new();
    let mut srcs: Vec<&crate::project::Source> = l.pkg.sources.iter().collect();
    if with_std {
        srcs.extend(l.std.iter());
    }
    for s in srcs {
        if let Some(md) = crate::doclink::module_doc(&s.lines) {
            module_docs.insert(s.module.clone(), (rel(&s.path), s.lines.len(), md));
        }
    }
    let mut items = pkg_items;
    if with_std {
        items.extend(std_items);
    }
    for line in crate::doc::index(&items, &module_docs) {
        println!("{}", line);
    }
    0
}

/// `ksn doc [--std] [--private]`: 静的 HTML の API ドキュメントを `target/doc/` に生成する (生成器は doc.rs)。
fn cmd_doc(args: &[String]) -> i32 {
    for a in args {
        if a != "--std" && a != "--private" {
            eprintln!("ksn: usage: ksn doc [--std] [--private]");
            return 2;
        }
    }
    let std_only = args.iter().any(|a| a == "--std");
    let private = args.iter().any(|a| a == "--private");
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let l = match load_all(&root) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let (prog, _) = build_program(&l);
    let (std_items, pkg_items) = doc_items(&l, &prog);
    // ``[`FQN`]`` は読み込んだ全定義に対して解決する。解決しないものはページを書かずに失敗する (E1125、3.2)。
    let known = crate::doclink::Known::of_loaded(&l);
    let doc_sources: Vec<&crate::project::Source> = if std_only { l.std.iter().collect() } else { l.pkg.sources.iter().collect() };
    let mut bad = 0;
    for s in &doc_sources {
        for (ln, f) in crate::doclink::unresolved(&s.lines, &known) {
            eprintln!("{}:{}:1: E1125: doc link to unknown definition `{}`", rel(&s.path), ln, f);
            bad += 1;
        }
    }
    if bad > 0 {
        return 1;
    }
    let mut module_docs = std::collections::BTreeMap::new();
    for s in &doc_sources {
        if let Some(md) = crate::doclink::module_doc(&s.lines) {
            module_docs.insert(s.module.clone(), md);
        }
    }
    let out = root.join("target").join("doc");
    let std_dir = out.join("std");
    let (dir, pages) = if std_only {
        let cfg = crate::doc::Config { title: "標準ライブラリ", is_std: true, private, std_link: None, known: &known, module_docs: &module_docs };
        (std_dir, crate::doc::generate(&std_items, &cfg))
    } else {
        let name = l.pkg.name.clone();
        let link = if std_dir.join("index.html").exists() { Some(&std_items[..]) } else { None };
        let cfg = crate::doc::Config { title: &name, is_std: false, private, std_link: link, known: &known, module_docs: &module_docs };
        (out, crate::doc::generate(&pkg_items, &cfg))
    };
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("ksn: cannot create {}: {}", norm(&dir), e);
        return 1;
    }
    // 古いページ (消えたモジュール) を残さない。
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            if e.path().extension().map_or(false, |x| x == "html") {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    for (f, html) in &pages {
        if let Err(e) = fs::write(dir.join(f), html) {
            eprintln!("ksn: cannot write {}: {}", f, e);
            return 1;
        }
    }
    println!("{}", norm(&dir.join("index.html")));
    0
}

/// `(?<![\w.])OLD(?![\w])(?!\.[A-Za-z_])` に一致するか。
pub(crate) fn fqn_occurs(text: &str, target: &str, tail_dot_ok: bool) -> bool {
    text.match_indices(target).any(|(i, _)| fqn_boundary(text, i, target.len(), tail_dot_ok))
}
fn fqn_boundary(text: &str, i: usize, len: usize, tail_dot_ok: bool) -> bool {
    let w = |c: char| c.is_alphanumeric() || c == '_';
    let before = text[..i].chars().next_back();
    let rest = &text[i + len..];
    let after = rest.chars().next();
    let mut second = rest.chars();
    second.next();
    before.map_or(true, |c| !(w(c) || c == '.'))
        && after.map_or(true, |c| !w(c))
        && (tail_dot_ok || !(after == Some('.') && second.next().map_or(false, |c| c.is_ascii_alphabetic() || c == '_')))
}

fn cmd_refs(args: &[String]) -> i32 {
    let Some(target) = args.first() else {
        eprintln!("ksn: usage: ksn refs <FQN>");
        return 2;
    };
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let l = match load_all(&root) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    match crate::refactor::refs(&l, target) {
        Ok(sites) => {
            for s in sites {
                println!("{}:{}:{}: {} [{}]", rel(&s.file), s.line, s.col, s.owner, s.kind);
            }
            0
        }
        Err(m) => {
            eprintln!("ksn: {}", m);
            1
        }
    }
}

/// `ksn check` と同じ検査を走らせ、致命的な診断の一覧 ("file:line: code: message") を返す。
/// 改名の検証 (改名の前後で診断が増えないこと) に使う。
pub(crate) fn check_error_lines(root: &Path) -> Vec<String> {
    let mut rep = Report::new(false);
    if let Err(e) = full_check(root, &mut rep, false) {
        return vec![format!("{}: {}", e.code, e.msg)];
    }
    rep.items
        .iter()
        .filter(|i| i.fatal)
        .map(|i| format!("{}:{}: {}: {}", i.file.clone().unwrap_or_default(), i.line, i.code, i.message))
        .collect()
}

fn cmd_rename(args: &[String]) -> i32 {
    let dry = args.iter().any(|a| a == "--dry-run");
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if pos.len() < 2 {
        eprintln!("ksn: usage: ksn rename [--dry-run] <old> <new>");
        return 2;
    }
    let (old, new) = (pos[0], pos[1]);
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let l = match load_all(&root) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let plan = match crate::refactor::plan_rename(&l, old, new) {
        Ok(p) => p,
        Err(m) => {
            eprintln!("ksn: {}", m);
            return 1;
        }
    };
    if let Err(m) = crate::refactor::verify(&root, &plan) {
        eprintln!("ksn: {}", m);
        return 1;
    }
    for c in &plan.changes {
        match &c.old_path {
            Some(o) if *o != c.new_path => println!("move {} -> {}", rel(o), rel(&c.new_path)),
            _ => println!("update {}", rel(&c.new_path)),
        }
    }
    if let Some((p, _, _)) = &plan.manifest {
        println!("update {}", rel(p));
    }
    for n in &plan.notes {
        eprintln!("ksn: note: {}", n);
    }
    if dry {
        for c in &plan.changes {
            let diff = similar::TextDiff::from_lines(c.old_text.as_str(), c.new_text.as_str());
            let oldh = c.old_path.as_deref().map(rel).unwrap_or_default();
            print!("{}", diff.unified_diff().header(&oldh, &rel(&c.new_path)));
        }
        if let Some((p, o, n)) = &plan.manifest {
            let diff = similar::TextDiff::from_lines(o.as_str(), n.as_str());
            print!("{}", diff.unified_diff().header(&rel(p), &rel(p)));
        }
        return 0;
    }
    if let Err(m) = crate::refactor::apply(&plan) {
        eprintln!("ksn: {}", m);
        return 1;
    }
    0
}

fn cmd_fmt(args: &[String]) -> i32 {
    let check = args.iter().any(|a| a == "--check");
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let pkg = match load_package(&root) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let srcroot = norm(&pkg.root.join("src"));
    let grammar = Grammar::new();
    let mut diff = false;
    let mut changed = false;
    for s in &pkg.sources {
        let nt = if s.error.is_none() { format_source(&s.text, &s.path) } else { mechanical_collapse(&s.text) };
        let mut text = s.text.clone();
        if nt != s.text {
            changed = true;
            println!("{} {}", if check { "would reformat" } else { "reformatted" }, rel(&s.path));
            if !check {
                fs::write(&s.path, &nt).unwrap();
                text = nt;
            }
        }
        let relpath = lint_rel(s, &srcroot);
        for (code, l, m) in Lint::new(&relpath, &text.replace("\r\n", "\n")).run() {
            if code != "unused" {
                diff = true;
                println!("{}:{}: {}: {} (not auto-fixable)", rel(&s.path), l, lint_code(&code), m);
            }
        }
        let (ok, msg, ln) = grammar.check(&text);
        if !ok {
            diff = true;
            println!("{}:{}: E0002: {}", rel(&s.path), ln, msg);
        }
    }
    if diff || (changed && check) { 1 } else { 0 }
}

fn cmd_explain(args: &[String]) -> i32 {
    let code = args.first().map(|s| s.as_str()).unwrap_or("");
    match EXPLAIN.iter().find(|(k, _)| *k == code) {
        Some((k, v)) => {
            println!("{}: {}", k, v);
            0
        }
        None => {
            eprintln!("ksn: unknown code {}", code);
            1
        }
    }
}

fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// 既存の `kasane.lock` の `[[package]]` を名前で引く。
fn old_lock(root: &Path) -> std::collections::HashMap<String, toml::Value> {
    let v: toml::Value = fs::read_to_string(root.join("kasane.lock")).unwrap_or_default().parse().unwrap_or(toml::Value::Table(Default::default()));
    v.get("package")
        .and_then(|p| p.as_array())
        .map(|a| a.iter().filter_map(|e| Some((e.get("name")?.as_str()?.to_string(), e.clone()))).collect())
        .unwrap_or_default()
}

/// `ksn update [<名前>]`: 依存を再解決して `kasane.lock` を書く。git 依存は取得元から取り直してハッシュを計算する
/// (`<名前>` を指定したときは、その依存だけを取り直し、ほかの依存は既存のロックの記録を保つ)。
fn cmd_update(args: &[String]) -> i32 {
    let only = args.first().cloned();
    let fail = |e: ProjectError| {
        eprintln!("ksn: {}: {}", e.code, e.msg);
        2
    };
    let (root, pkg) = match find_root().and_then(|r| load_package(&r).map(|p| (r, p))) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    let old = old_lock(&root);
    let mode = |n: &str| match &only {
        None => GitMode::Fresh,
        Some(o) if o == n => GitMode::Fresh,
        Some(_) => GitMode::IfMissing,
    };
    let deps = match load_deps_with(&pkg, &mode) {
        Ok(d) => d,
        Err(e) => return fail(e),
    };
    if let Some(o) = &only {
        if !deps.iter().any(|d| d.name == *o) {
            return fail(ProjectError { code: "E9017".into(), msg: format!("{} is not a dependency of {}", o, pkg.name) });
        }
    }
    let mut lines = vec!["version = 1".to_string(), String::new()];
    let mut sorted: Vec<&Package> = deps.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for dp in sorted {
        lines.push("[[package]]".into());
        lines.push(format!("name = {}", toml_str(&dp.name)));
        lines.push(format!("source = {}", toml_str(&dp.source)));
        if dp.source.starts_with("git+") {
            let kept = only.as_ref().filter(|o| **o != dp.name).and_then(|_| old.get(&dp.name)).and_then(|e| e.get("hash")).and_then(|h| h.as_str()).map(String::from);
            let hash = match kept {
                Some(h) => h,
                None => match try_package_hash(&dp.root) {
                    Ok(h) => h,
                    Err(e) => return fail(e),
                },
            };
            lines.push(format!("hash = {}", toml_str(&hash)));
        }
        let mut dn: Vec<String> =
            dp.manifest.get("dependencies").and_then(|d| d.as_table()).map(|t| t.keys().cloned().collect()).unwrap_or_default();
        dn.sort();
        lines.push(format!("deps = [{}]", dn.iter().map(|x| toml_str(x)).collect::<Vec<_>>().join(", ")));
        lines.push(String::new());
    }
    let text = lines.join("\n").trim_end().to_string() + "\n";
    fs::write(root.join("kasane.lock"), text).unwrap();
    0
}

/// `ksn vendor`: git 依存 (推移的なものを含む) を取得元から取り直し、`vendor/<名前>/` に複製する。
/// `kasane.lock` に記録があれば、取得した内容がそのハッシュと一致することを確かめてから複製する。
fn cmd_vendor() -> i32 {
    let fail = |e: ProjectError| {
        eprintln!("ksn: {}: {}", e.code, e.msg);
        2
    };
    let (root, pkg) = match find_root().and_then(|r| load_package(&r).map(|p| (r, p))) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    let deps = match load_deps_with(&pkg, &|_| GitMode::Fresh) {
        Ok(d) => d,
        Err(e) => return fail(e),
    };
    let old = old_lock(&root);
    for dp in deps.iter().filter(|d| d.source.starts_with("git+")) {
        let actual = match try_package_hash(&dp.root) {
            Ok(h) => h,
            Err(e) => return fail(e),
        };
        if let Some(h) = old.get(&dp.name).and_then(|e| e.get("hash")).and_then(|h| h.as_str()) {
            if h != actual {
                return fail(ProjectError {
                    code: "E9022".into(),
                    msg: format!("hash mismatch for {}: kasane.lock says {} but the fetched source has {}; not vendored", dp.name, h, actual),
                });
            }
        }
        let dest = root.join("vendor").join(&dp.name);
        let _ = fs::remove_dir_all(&dest);
        copy_tree(&dp.root, &dest, &[]);
        let _ = fs::write(dest.join(".ksn-source"), format!("{}\n", dp.source));
    }
    0
}

// ------------------------------------------------------------ build
fn dir_size(p: &Path) -> u64 {
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            let q = e.path();
            n += if q.is_dir() { dir_size(&q) } else { e.metadata().map(|m| m.len()).unwrap_or(0) };
        }
    }
    n
}

fn copy_tree(src: &Path, dst: &Path, skip: &[&str]) {
    let _ = fs::create_dir_all(dst);
    if let Ok(rd) = fs::read_dir(src) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if skip.contains(&name.as_str()) {
                continue;
            }
            let s = e.path();
            // キャッシュディレクトリや `.git` などの隠しディレクトリはバンドルに含めない。
            // 入れ子膨張 (`pkg/.cache/.../pkg/.cache/…`) の元になる。
            if s.is_dir() && name.starts_with('.') {
                continue;
            }
            let d = dst.join(&name);
            if s.is_dir() {
                copy_tree(&s, &d, skip);
            } else {
                let _ = fs::copy(&s, &d);
            }
        }
    }
}

/// キャッシュ entry (ksn・stdlib・pkg) を `target/<kind>` へ複製する。
/// 直下の `.used` と生成物ディレクトリは除く (`copy_tree` の skip は再帰するので使わない)。
fn copy_entry(src: &Path, dst: &Path) {
    let _ = fs::create_dir_all(dst);
    let Ok(rd) = fs::read_dir(src) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name == ".used" || name == ARTIFACT_DIR {
            continue;
        }
        let (s, d) = (e.path(), dst.join(&name));
        if s.is_dir() {
            copy_tree(&s, &d, &[]);
        } else {
            let _ = fs::copy(&s, &d);
        }
    }
}

/// 生成したネイティブ実行ファイルをキャッシュ entry に保存する。
/// entry が消えた後に半端なディレクトリを作らないよう `create_dir` (非再帰) を使う。
fn store_artifact(cdir: &Path, key: &str, exe: &Path) {
    let cached = cdir.join(key);
    let Some(fname) = exe.file_name() else { return };
    let adir = cached.join(ARTIFACT_DIR);
    if fs::create_dir(&adir).is_err() && !adir.is_dir() {
        return;
    }
    let tmp = adir.join(format!("{}.tmp", fname.to_string_lossy()));
    if fs::copy(exe, &tmp).is_ok() {
        let _ = fs::rename(&tmp, adir.join(fname));
    }
    evict(cdir, key);
}

fn evict(cdir: &Path, keep: &str) {
    let limit: u64 = std::env::var("KSN_CACHE_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(2 * 1024 * 1024 * 1024);
    let Ok(rd) = fs::read_dir(cdir) else { return };
    let mut ents: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy() != keep && e.file_name().to_string_lossy() != "git" && e.file_name().to_string_lossy() != "check")
        .map(|e| {
            let used = e.path().join(".used");
            let t = fs::metadata(&used).or_else(|_| e.metadata()).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            (t, e.path())
        })
        .collect();
    ents.sort();
    let mut total: u64 = ents.iter().map(|(_, p)| dir_size(p)).sum();
    while !ents.is_empty() && total > limit {
        let (_, p) = ents.remove(0);
        total -= dir_size(&p);
        let _ = fs::remove_dir_all(&p);
    }
}

/// キャッシュ entry 内で生成物 (ネイティブ実行ファイル) を置く子ディレクトリ名。
const ARTIFACT_DIR: &str = "artifact";
/// `target/<kind>` に置く、そのビルドのキー。バンドルが今の ksn・ソースに対応するかの判定に使う。
const BUILD_STAMP: &str = ".build-key";

/// ビルドキーの内訳: 生成条件 (ksn 本体・edition・debug/release・ターゲット・バックエンド・clang とそのバージョン・clang の追加引数)、
/// パッケージと依存のソース、stdlib、ksn 本体のバイト列。
/// バックエンドは出力が違うので (llvm と c) キーに入れる。clang を差し替えたり引数を変えたりしても前の成果物が `cache hit` で出ないように、
/// clang 自体もキーに含める。
fn build_key(l: &Loaded, release: bool, target: &str, backend: &str) -> String {
    let mut h = Sha256::new();
    let edition = l.pkg.manifest["package"].get("edition").map(|e| e.to_string()).unwrap_or_default();
    let clang = std::env::var("KSN_CLANG").unwrap_or_else(|_| "clang".to_string());
    let clang_args = clang_extra_args().join(" ");
    h.update(format!("{}|{}|{}|{}|{}|{}|{}|{}", VERSION, edition, if release { "release" } else { "debug" }, target, backend, clang, clang_version_line(&clang), clang_args).as_bytes());
    h.update(package_hash(&l.pkg.root).as_bytes());
    for dp in &l.deps {
        h.update(package_hash(&dp.root).as_bytes());
    }
    // デバッグビルドの同梱物には `tests/` (統合テスト、3.2) が入る。リリースには入れない。
    if !release {
        for s in l.pkg.sources.iter().filter(|s| s.is_test) {
            h.update(s.path.as_bytes());
            h.update(s.text.as_bytes());
        }
    }
    for s in &l.std {
        h.update(s.text.as_bytes());
    }
    if let Ok(exe) = std::env::current_exe() {
        h.update(fs::read(exe).unwrap_or_default());
    }
    h.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

/// `clang --version` の 1 行目 (clang が起動できなければ空)。キャッシュキーに clang の世代を入れるために使う。
fn clang_version_line(clang: &str) -> String {
    match std::process::Command::new(clang).arg("--version").output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string(),
        Err(_) => String::new(),
    }
}

/// 実行権限を付ける (Unix のみ。Windows は拡張子 .exe / .cmd で起動されるので不要)。
#[cfg(unix)]
fn set_exec(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn set_exec(_p: &Path) {}

fn cmd_build(args: &[String], preloaded: Option<&Loaded>) -> i32 {
    let release = args.iter().any(|a| a == "--release");
    // 3-2: リリースのネイティブ生成が失敗したとき、インタプリタ同梱のバンドルへ退避するのを明示的に許す。
    // デバッグ (`--release` なし) は常に退避する (理由を出す) ので、このフラグは効かない。
    let allow_bundle = args.iter().any(|a| a == "--allow-bundle");
    let backend = args.iter().position(|a| a == "--backend").and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| "llvm".into());
    let target_arg = args.iter().position(|a| a == "--target").and_then(|i| args.get(i + 1)).cloned();
    // `--emit-llvm <ファイル>`: ネイティブを作らず、clang に渡す前の LLVM IR (`.ll`) を書き出す。
    let emit_llvm = match args.iter().position(|a| a == "--emit-llvm") {
        Some(i) => match args.get(i + 1) {
            Some(v) => Some(PathBuf::from(v)),
            None => {
                eprintln!("ksn: E9040: `--emit-llvm <ファイル>`: 出力ファイルを指定してください");
                return 1;
            }
        },
        None => None,
    };
    if backend == "clif" {
        eprintln!("ksn: E9040: `--backend clif` was removed: the Cranelift backend is gone and native generation goes through LLVM (KIR). Use `--backend llvm` (the default), or `--backend c`");
        return 1;
    }
    if backend != "llvm" && backend != "c" {
        eprintln!("ksn: E9040: unknown backend `{}` (expected llvm or c)", backend);
        return 1;
    }
    // `--target` (省略か `host` は今の機械)。対応は x86_64-linux-gnu と aarch64-linux-gnu (`kir_llvm::TARGETS`)。
    let explicit = target_arg.as_deref().is_some_and(|t| t != "host");
    let target = target_arg.clone().unwrap_or_else(|| "host".into());
    let tgt = if explicit {
        match crate::kir_llvm::target_of(&target) {
            Some(t) => Some(t),
            None => {
                let names: Vec<&str> = crate::kir_llvm::TARGETS.iter().map(|t| t.name).collect();
                eprintln!("ksn: E9040: unsupported target `{}` (supported: {})", target, names.join(", "));
                return 1;
            }
        }
    } else {
        crate::kir_llvm::host_target()
    };
    // `usize` / `isize` / ポインタの幅を対象から決める (検査のリテラル範囲と KIR の大きさの両方が読む)。
    let _width = crate::ptrwidth::Guard::set(tgt.map_or(64, |t| t.ptr_bits));
    if explicit && backend != "llvm" {
        eprintln!("ksn: E9040: `--target {}` needs `--backend llvm` (the C backend builds only for this machine)", target);
        return 1;
    }
    if backend == "c" && emit_llvm.is_some() {
        eprintln!("ksn: E9040: `--emit-llvm` needs `--backend llvm` (the C backend does not produce LLVM IR)");
        return 1;
    }
    // 対象が今の機械なら、インタプリタ同梱のバンドル (ホストの ksn のコピー) もそのまま動く。違えば作らない。
    let runs_here = !explicit || tgt == crate::kir_llvm::host_target();
    let mut rep = Report::new(false);
    let root = match find_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 1;
        }
    };
    let (l, prog, user) = match full_check(&root, &mut rep, false) {
        Ok(x) => x,
        Err(e) => {
            rep.add(&e.code, &e.msg, None, 1, 1, true);
            rep.flush();
            return 1;
        }
    };
    let _ = preloaded;
    rep.flush();
    if rep.fatal() {
        return 1;
    }
    // `--emit-llvm <ファイル>`: ネイティブを作らず、clang に渡す前の LLVM IR (`.ll`) を書き出して終了する。
    // キャッシュ・バンドル・インタプリタ退避は行わない。
    if let Some(emit_path) = &emit_llvm {
        let entry = match l.pkg.entry() {
            Some(e) => e,
            None => {
                eprintln!("ksn: E9040: `--emit-llvm` needs an entry point (`entry` in kasane.toml)");
                return 1;
            }
        };
        let p = prog.as_ref().expect("entry is set");
        let test_files: std::collections::HashSet<&str> =
            l.pkg.sources.iter().filter(|s| s.is_test).map(|s| s.path.as_str()).collect();
        let mut user_ll: Vec<usize> = user.iter().copied().filter(|&i| !test_files.contains(p.defs[i].file.as_str())).collect();
        let dep_files: std::collections::HashSet<&str> =
            l.deps.iter().flat_map(|dp| dp.sources.iter().map(|s| s.path.as_str())).collect();
        user_ll.extend((0..p.defs.len()).filter(|&i| dep_files.contains(p.defs[i].file.as_str())));
        let lo = crate::kir_lower::lower_program(p, &user_ll, Some(&entry));
        if lo.module.find(&entry).is_none() {
            eprintln!("ksn: E9040: `--emit-llvm`: entry `{}` cannot be lowered to LLVM IR", entry);
            return 1;
        }
        let roots: Vec<&str> = vec![&entry, crate::kir_lower::debug::ENTRY_DEBUG];
        let m = lo.module.reachable_from(&roots);
        let ll = match crate::kir_llvm::emit_program(&m, &entry, tgt) {
            Ok(ll) => ll,
            Err(e) => {
                eprintln!("ksn: E9040: `--emit-llvm`: {}", e);
                return 1;
            }
        };
        if let Err(e) = fs::write(emit_path, &ll) {
            eprintln!("ksn: {} を書けない: {}", emit_path.display(), e);
            return 1;
        }
        println!("wrote {}", emit_path.display());
        return 0;
    }
    let kind = if release { "release" } else { "debug" };
    let key = build_key(&l, release, &target, &backend);
    let cdir = cache_dir();
    let cached = cdir.join(&key);
    if cached.is_dir() {
        let _ = fs::write(cached.join(".used"), "");
        println!("cache hit");
    } else {
        // 一時ディレクトリはプロセスごと (同じキーの並行ビルドが互いの作業中のものを消さない)。
        let tmp = cdir.join(format!("{}.tmp{}", key, std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        if let Ok(exe) = std::env::current_exe() {
            let _ = fs::copy(exe, tmp.join(exe_name()));
        }
        copy_tree(&stdlib_dir(), &tmp.join("stdlib"), &[]);
        let pk = tmp.join("pkg");
        let mut skip: Vec<&str> = vec!["target", "__pycache__", ".git"];
        // KSN_CACHE_DIR がパッケージの中を指すと、バンドル時にパッケージと一緒にコピーされて
        // `pkg/<キャッシュ>/…/pkg/…` と入れ子に膨らむ。最上位の名前を除く。
        let cdir = cache_dir();
        if let Ok(rel) = cdir.strip_prefix(&l.pkg.root) {
            if let Some(top) = rel.iter().next().and_then(|n| n.to_str()) {
                if !top.starts_with('.') && !skip.contains(&top) {
                    skip.push(top);
                }
            }
        }
        copy_tree(&l.pkg.root, &pk, &skip);
        if release {
            // 3.2: テストはリリースビルドから除外する (統合テストのファイルも同梱しない)。
            let _ = fs::remove_dir_all(pk.join("tests"));
        }
        for dp in &l.deps {
            copy_tree(&dp.root, &pk.join("vendor").join(&dp.name), &["target", "__pycache__", ".git"]);
        }
        let _ = fs::write(tmp.join(".used"), "");
        if let Err(e) = fs::rename(&tmp, &cached) {
            // 同じキーを並行にビルドした別のプロセスが先に置いた。中身は同じなので、それを使う。
            let _ = fs::remove_dir_all(&tmp);
            if !cached.is_dir() {
                eprintln!("ksn: cannot store the build in the cache {}: {}", cached.display(), e);
                return 1;
            }
        }
        evict(&cdir, &key);
    }
    // 既定 (`--target` なし) は従来どおり `target/<kind>`。`--target` を付けたときは `target/<三つ組>/<kind>`。
    let out = if explicit { l.pkg.root.join("target").join(&target).join(kind) } else { l.pkg.root.join("target").join(kind) };
    let _ = fs::remove_dir_all(&out);
    let name = l.pkg.name.clone();
    if runs_here {
        copy_entry(&cached, &out);
        let _ = fs::write(out.join(BUILD_STAMP), &key);
        // %~dp0 は常に絶対パス。sh 版は $0 (相対パス・PATH 経由の名前) を絶対パスに直してから
        // pkg へ cd する (cd 後に相対パスの $d/ksn を exec すると失敗するため)。
        let _ = fs::write(out.join(format!("{}.cmd", name)), "@echo off\r\nset KSN_STDLIB=%~dp0stdlib\r\ncd /d \"%~dp0pkg\"\r\n\"%~dp0ksn.exe\" _exec %*\r\n");
        let launcher = out.join(&name);
        let _ = fs::write(
            &launcher,
            r#"#!/bin/sh
case "$0" in
  */*) p="$0" ;;
  *) p="$(command -v "$0")" ;;
esac
# $0 がシンボリックリンクなら実体を辿ってから場所を特定する。
# `readlink -f` は macOS などに無いので、`ls -l` のリンク先を辿るループで POSIX 的に解決する。
while [ -L "$p" ]; do
  target="$(ls -l "$p")"
  target="${target#* -> }"
  case "$target" in
    /*) p="$target" ;;
    *) p="$(dirname "$p")/$target" ;;
  esac
done
d="$(cd "$(dirname "$p")" && pwd)"
KSN_STDLIB="$d/stdlib"
export KSN_STDLIB
cd "$d/pkg" && exec "$d/ksn" _exec "$@"
"#,
        );
        set_exec(&launcher);
    } else {
        let _ = fs::create_dir_all(&out);
    }
    let prog = prog.unwrap();
    let entry = l.pkg.entry();
    let exe = out.join(if cfg!(windows) {
        format!("{}.exe", name)
    } else if crate::kir_llvm::is_wasm(tgt) {
        format!("{}.wasm", name)
    } else {
        format!("{}.native", name)
    });
    // キャッシュに同じキーの生成物があれば複製し、コンパイルとリンクを省く。
    let stored = cached.join(ARTIFACT_DIR).join(exe.file_name().expect("exe has a file name"));
    let reused = entry.is_some() && fs::copy(&stored, &exe).is_ok();
    let native: Result<(), String> = match &entry {
        _ if reused => Ok(()),
        None => Err("library package".into()),
        Some(e) => {
            let test_files: std::collections::HashSet<&str> =
                l.pkg.sources.iter().filter(|s| s.is_test).map(|s| s.path.as_str()).collect();
            let mut user: Vec<usize> = user.iter().copied().filter(|&i| !test_files.contains(prog.defs[i].file.as_str())).collect();
            // 依存パッケージの関数・型も機械語にする (到達できるものだけが出力される)。
            let dep_files: std::collections::HashSet<&str> =
                l.deps.iter().flat_map(|dp| dp.sources.iter().map(|s| s.path.as_str())).collect();
            user.extend((0..prog.defs.len()).filter(|&i| dep_files.contains(prog.defs[i].file.as_str())));
            let mut funcs: HashMap<String, &Def> = HashMap::new();
            for &i in &user {
                let d = &prog.defs[i];
                if d.kw == "fn" && d.body.is_some() {
                    funcs.insert(d.name.clone(), d);
                }
            }
            if backend == "llvm" {
                // 下ろせない・clang が無いなどなら、理由を出してインタプリタ同梱のバンドルだけにする。
                let t_cc = std::time::Instant::now();
                let r = build_llvm(&prog, &user, e, &exe, release, Tgt { ir: tgt, clang: if explicit { tgt } else { None } });
                if std::env::var_os("KSN_TIME").is_some() {
                    eprintln!("KSN_TIME compile_llvm_total={:.2} [ms]", t_cc.elapsed().as_secs_f64() * 1e3);
                }
                r
            } else {
                crate::cnative::compile_native(&funcs, e, &norm(&exe), release)
            }
        }
    };
    if native.is_ok() && !reused {
        store_artifact(&cdir, &key, &exe);
    }
    if !runs_here {
        // 対象が今の機械でないときは、バンドル (ホストの ksn) で退避できない。作れなければ理由を出して失敗する。
        return match native {
            Ok(()) => {
                println!("native executable: {}", rel(&norm(&exe)));
                println!("built {} ({}, target {}; cross-compiled, not run here)", rel(&norm(&out)), kind, target);
                0
            }
            Err(why) => {
                let _ = fs::remove_dir_all(&out);
                eprintln!("ksn: E9040: cannot make a native executable for target {}: {}", target, why);
                1
            }
        };
    }
    if let Err(why) = &native {
        // ライブラリ (entry なし) は実行ファイルを作らないものなので、退避の対象ではなく従来どおり扱う。
        if release && !allow_bundle && entry.is_some() {
            // 3-2: リリースは退避しない。作りかけの target/release を残さず、理由と逃げ道を出して失敗する。
            let _ = fs::remove_dir_all(&out);
            eprintln!(
                "ksn: E9040: `ksn build --release` cannot make a native executable: {}. Release builds do not fall back to the interpreter bundle. Fix the cause, build without `--release` (the bundle is then made), or pass `--allow-bundle` to accept the bundle",
                why
            );
            return 1;
        }
    }
    match native {
        Ok(()) => println!("native executable: {}", rel(&norm(&exe))),
        Err(e) => println!("native backend skipped: {}; the interpreter bundle is used", e),
    }
    println!("built {} ({}, target {}; portable interpreter bundle)", rel(&norm(&out)), kind, target);
    0
}

/// `ksn build --backend llvm`: 入口の関数から届く関数すべてを KIR に下ろし、LLVM IR を作って clang で実行ファイルにする (設計_KIR.md 12.3)。
/// 下ろせない関数が 1 つでもあれば、理由を `Err` で返す (呼び出し側が既定の経路へ退避する)。
fn build_llvm(prog: &Program, user: &[usize], entry: &str, exe: &Path, release: bool, tg: Tgt) -> Result<(), String> {
    let lo = crate::kir_lower::lower_program(prog, user, Some(entry));
    match llvm_exe(lo, entry, exe, if release { "-O2" } else { "-O1" }, tg) {
        Ok(()) => Ok(()),
        Err(BuildFail::Unsupported(unsupported)) => Err(format_reasons(prog, &unsupported, entry, if release { Some(20) } else { None })),
        Err(BuildFail::Tool(s)) => Err(s),
    }
}

/// LLVM IR の対象 (`ir`: triple と datalayout を書く) と、clang に渡す対象 (`clang`: `--target` を付けるのは明示された対象だけ)。
#[derive(Clone, Copy)]
struct Tgt {
    ir: Option<crate::kir_llvm::Target>,
    clang: Option<crate::kir_llvm::Target>,
}


/// `ksn test --both` の LLVM 側: テスト `ti` を入口にして KIR → LLVM → clang の実行ファイルを作る (`target/test-llvm/`)。
/// 下ろせない・clang が無いなどで作れなければ理由を返す (呼び出し側がバンドルへ退避する)。
fn build_llvm_test(root: &Path, prog: &Program, user: &[usize], ti: usize) -> Result<PathBuf, String> {
    let d = &prog.defs[ti];
    let lo = crate::kir_lower::lower_test(prog, user, ti);
    let dir = root.join("target").join("test-llvm");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let exe = dir.join(format!("{}_{}{}", ti, test_stem(&d.name), if cfg!(windows) { ".exe" } else { ".native" }));
    let _ = fs::remove_file(&exe);
    match llvm_exe(lo, &d.name, &exe, "-O1", Tgt { ir: crate::kir_llvm::host_target(), clang: None }) {
        Ok(()) => Ok(exe),
        Err(BuildFail::Unsupported(unsupported)) => Err(format_reasons(prog, &unsupported, &d.name, None)),
        Err(BuildFail::Tool(s)) => Err(s),
    }
}

/// テストの FQN を、実行ファイル名に使える形にする。
fn test_stem(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '.' { c } else { '_' }).collect()
}

/// 下ろしの失敗原因。`Unsupported` は下ろせない関数の一覧（呼び出し側でファイル・行を付けて出力する）、
/// `Tool` は clang / IR 出力などツール側の生のエラー文字列。
enum BuildFail {
    Unsupported(Vec<(String, String)>),
    Tool(String),
}

/// 下ろし済みの `lo` から、入口 `entry` の実行ファイル `exe` を作る。入口が下ろせれば `Ok(())` を返す。
/// 入口が下ろせなければ、`lo.unsupported` にある下ろせない関数の理由を全件 `Err` で返す
/// (呼び出し側でファイル・行を付けて出力する)。
fn llvm_exe(lo: crate::kir_lower::Lowered, entry: &str, exe: &Path, opt: &str, tg: Tgt) -> Result<(), BuildFail> {
    if lo.module.find(entry).is_none() {
        return Err(BuildFail::Unsupported(lo.unsupported.clone()));
    }
    if std::env::var_os("KSN_DUMP_KIR").is_some() {
        for (f, why) in &lo.unsupported {
            eprintln!("kir: 未対応 {}: {}", f, why);
        }
    }
    // wasm32 は `ksn_` の関数 (JS から呼ぶ export、WASM ABI) も根にする。ほかの三つ組は出力を変えない。
    let mut roots: Vec<String> = vec![entry.to_string(), crate::kir_lower::debug::ENTRY_DEBUG.to_string()];
    if crate::kir_llvm::is_wasm(tg.ir) {
        roots.extend(lo.module.funcs.iter().filter(|f| crate::kir_llvm::wasm_export_name(&f.key).is_some()).map(|f| f.key.clone()));
        // JS の完了を待つ入口 (`await_js`): `ksn_resume` が使う関数 (結果の置き場・再開・待ちの有無) を根にする。
        if crate::kir_llvm::await_mode(&lo.module) {
            roots.extend(["ksn.js.put".to_string(), format!("{}.resume", entry), format!("{}.pending", entry)]);
        }
    }
    let refs: Vec<&str> = roots.iter().map(|s| s.as_str()).collect();
    let m = lo.module.reachable_from(&refs);
    let ll = crate::kir_llvm::emit_program(&m, entry, tg.ir).map_err(|e| BuildFail::Tool(e))?;
    clang_ll(&ll, exe, opt, tg.clang).map_err(|e| BuildFail::Tool(e))
}

/// `lo.unsupported` の `(関数FQN, 理由)` を、プログラムの定義からファイル・行を付けて
/// 「関数ごとに 1 行」のメッセージにする。`entry` が指定されるとその理由を先頭に出す
/// (ユーザが下ろそうとした入口が最も目立つように)。`max` が `Some(n)` のときは先頭 `n` 件だけ出し、
/// 超えた分は「... 他 N 件」と件数を出す。
fn format_reasons(prog: &Program, unsupported: &[(String, String)], entry: &str, max: Option<usize>) -> String {
    let limit = max.unwrap_or(usize::MAX);
    // 入口の理由を先頭に、それ以外を元の順序でその後ろに。
    let entry_pos = unsupported.iter().position(|(fq, _)| fq == entry);
    let mut ordered: Vec<&(String, String)> = Vec::new();
    if let Some(pos) = entry_pos {
        ordered.push(&unsupported[pos]);
    }
    for (i, item) in unsupported.iter().enumerate() {
        if entry_pos.map_or(false, |p| i == p) { continue; }
        ordered.push(item);
    }
    let mut lines: Vec<String> = Vec::new();
    for (fq, why) in ordered.iter().take(limit) {
        let loc = prog
            .defs
            .iter()
            .find(|d| d.name == **fq)
            .map(|d| format!("{}:{}", d.file, d.line))
            .unwrap_or_else(|| fq.clone());
        lines.push(format!("{} を KIR に下ろせない ({}): {}", fq, loc, why));
    }
    if let Some(max) = max {
        if unsupported.len() > max {
            lines.push(format!("... 他 {} 件が下ろせない", unsupported.len() - max));
        }
    }
    lines.join("\n")
}

/// クロスコンパイルの clang の引数 (対象を明示したときだけ)。`--target` と、ターゲットの C の開発ファイル・libc の探索先。
/// sysroot は使わない: aarch64 の `libc.so` / `libm.so` はリンカスクリプトで絶対パスを書いており、`--sysroot` を付けると
/// GNU ld / lld がそれに sysroot を二重に前置して見つからなくなる。代わりに `-isystem` と `-L` で同じ場所を探させる。
/// 探索先は `KSN_SYSROOT` で上書きできる (空なら対象の既定)。
fn clang_target_args(t: Option<crate::kir_llvm::Target>) -> Vec<String> {
    let Some(t) = t else { return vec![] };
    let mut a = vec![format!("--target={}", t.name)];
    let root = std::env::var("KSN_SYSROOT").ok().filter(|s| !s.is_empty()).or_else(|| (!t.sysroot.is_empty()).then(|| t.sysroot.to_string()));
    if let Some(r) = root {
        a.push("-isystem".into());
        a.push(format!("{}/include", r));
        a.push(format!("-L{}/lib", r));
        // crt とコンパイラの実行時 (libgcc) は、GCC の構成の下にある。Debian の `gcc-<arch>` の場所を使う。
        if let Some(g) = gcc_install_dir(t.name) {
            a.push(format!("--gcc-install-dir={}", g));
        }
    }
    a
}

/// `/usr/lib/gcc-cross/<三つ組>/<版>` のうち版が最も新しいもの (クロス GCC の構成の場所)。無ければ `None`。
fn gcc_install_dir(triple: &str) -> Option<String> {
    let base = Path::new("/usr/lib/gcc-cross").join(triple);
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for e in fs::read_dir(&base).ok()?.flatten() {
        let p = e.path();
        let ver: Option<Vec<u32>> = e.file_name().to_string_lossy().split('.').map(|x| x.parse().ok()).collect();
        if let Some(v) = ver.filter(|_| p.is_dir()) {
            if best.as_ref().is_none_or(|(b, _)| v > *b) {
                best = Some((v, p));
            }
        }
    }
    best.map(|(_, p)| p.display().to_string())
}

/// 環境変数 `KSN_CLANG_ARGS` (空白区切り) の clang への追加の引数。試験が `-fsanitize=address -g` などを渡すための隠し機能。
/// 未設定なら空 (既定の挙動は変わらない)。
/// IR が C の実行時補助 (`kir_rt.c`) や OS ランタイム (`sysrt.c`) を要するなら、
/// それらを `src` の隣に書いて、clang に足す引数 (`-x c <path>` の繰り返し) を返す。
fn runtime_c_args(ll: &str, src: &Path) -> Vec<String> {
    if !crate::kir_llvm::needs_runtime(ll) {
        return vec![];
    }
    let mut args = Vec::new();
    let c = src.with_extension("rt.c");
    if fs::write(&c, crate::kir_llvm::RUNTIME_C).is_ok() {
        args.push("-x".into());
        args.push("c".into());
        args.push(c.display().to_string());
    }
    if ll.contains("@ksn_rt_sys_") {
        let c = src.with_extension("sys.c");
        if fs::write(&c, include_str!("sysrt.c")).is_ok() {
            args.push("-x".into());
            args.push("c".into());
            args.push(c.display().to_string());
        }
    }
    args
}

/// wasm32-unknown-unknown の実行時 (C) と、libc の見出しの最小 (`src/wasm/`)。`wasm_link` が一時の場所へ書く。
const WASM_RT: &str = include_str!("wasm/rt.c");
const WASM_KIR_RT: &str = include_str!("kir_rt.c");
/// wasm32-wasi だけ: WASI の入口 (`_start`)、`ksn_write` / `ksn_exit` / `getenv`、`ksn_rt_sys_*` (std.fs・時刻・乱数・環境)。
const WASM_WASI_RT: &str = include_str!("wasm/wasi.c");
const WASM_HEADERS: [(&str, &str); 4] = [
    ("stdio.h", include_str!("wasm/include/stdio.h")),
    ("stdlib.h", include_str!("wasm/include/stdlib.h")),
    ("string.h", include_str!("wasm/include/string.h")),
    ("math.h", include_str!("wasm/include/math.h")),
];

/// LLVM IR を `wasm32-unknown-unknown` の素の `.wasm` にする (修正プラン 5-3)。`wasm32-wasi` なら WASI preview 1 の `.wasm` にする。
/// clang で IR と実行時 (`rt.c`、`kir_rt.c`、WASI なら `wasi.c`) をそれぞれ wasm32 の目的ファイルにし、`wasm-ld` でつなぐ。
/// 未定義の記号は `ksn_write` と `ksn_exit` (素の wasm) と、IR が `wasm-import-name` で宣言した JS の import (`link "env:名前"`) だけ import として許す (それ以外は、リンクが失敗して理由が出る)。
/// WASI の import (`wasi_snapshot_preview1`) は `wasi.c` が import 属性つきで宣言するので、許可の一覧には要らない。
/// 道具は `KSN_CLANG` / `KSN_WASM_LD` で上書きできる (既定は `clang` と `wasm-ld-20`、無ければ `wasm-ld`)。
fn wasm_link(ll: &str, exe: &Path, opt: &str, target: Option<crate::kir_llvm::Target>) -> Result<(), String> {
    let wasi = crate::kir_llvm::is_wasi(target);
    let clang = std::env::var("KSN_CLANG").unwrap_or_else(|_| "clang".to_string());
    let ld = std::env::var("KSN_WASM_LD").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "wasm-ld-20".to_string());
    let dir = exe.with_extension("wasm.d");
    let _ = fs::remove_dir_all(&dir);
    let inc = dir.join("include");
    fs::create_dir_all(&inc).map_err(|e| format!("{} を作れない: {}", inc.display(), e))?;
    let put = |p: PathBuf, s: &str| fs::write(&p, s).map_err(|e| format!("{} を書けない: {}", p.display(), e));
    for (name, text) in WASM_HEADERS {
        put(inc.join(name), text)?;
    }
    let (ir, rt, krt, allow) = (dir.join("ir.ll"), dir.join("rt.c"), dir.join("kir_rt.c"), dir.join("allow.txt"));
    put(ir.clone(), ll)?;
    put(rt.clone(), WASM_RT)?;
    put(krt.clone(), WASM_KIR_RT)?;
    let wasi_c = dir.join("wasi.c");
    if wasi {
        put(wasi_c.clone(), WASM_WASI_RT)?;
    }
    // 未定義の記号として許すのは、`ksn_write` `ksn_exit` と、IR が `wasm-import-name` で宣言した JS の import (`link "env:名前"`) だけ。
    let mut names: Vec<&str> = vec!["ksn_write", "ksn_exit"];
    names.extend(ll.split("\"wasm-import-name\"=\"").skip(1).filter_map(|p| p.split('"').next()));
    put(allow.clone(), &(names.join("\n") + "\n"))?;
    let (oir, ort, okrt, owasi) = (dir.join("ir.o"), dir.join("rt.o"), dir.join("kir_rt.o"), dir.join("wasi.o"));
    // 三つ組の綴り (素の wasm は `wasm32-unknown-unknown`、WASI は `wasm32-unknown-wasi`)。
    let triple = format!("--target={}", target.map_or("wasm32-unknown-unknown", |t| t.triple));
    // WASI のときは `KSN_WASI` を立てる (rt.c は `write` / `exit` / `getenv` を wasi.c に任せ、kir_rt.c は乱数の stub を外す)。
    let defs: Vec<String> = if wasi { vec!["-DKSN_WASI=1".to_string()] } else { vec![] };
    let inc_arg = format!("-isystem{}", inc.display());
    let run = |cmd: &str, args: Vec<String>, what: &str| -> Result<(), String> {
        let o = std::process::Command::new(cmd).args(&args).output().map_err(|e| format!("{} を起動できない ({}: {})", what, cmd, e))?;
        if o.status.success() {
            Ok(())
        } else {
            Err(format!("{} が失敗した: {}", what, String::from_utf8_lossy(&o.stderr).trim()))
        }
    };
    let s = |x: &str| x.to_string();
    let p = |x: &Path| x.display().to_string();
    let c_args = |src: &Path, obj: &Path| -> Vec<String> {
        [vec![s("-O1"), triple.clone(), s("-ffreestanding"), s("-fno-builtin")], defs.clone(), vec![s(&inc_arg), s("-c"), s("-x"), s("c"), p(src), s("-o"), p(obj)]].concat()
    };
    let r1 = run(&clang, [vec![s(opt), s("-Wno-override-module"), triple.clone(), s("-c"), s("-x"), s("ir"), p(&ir), s("-o"), p(&oir)]].concat(), "clang (IR → wasm32)");
    let r2 = r1.and_then(|_| run(&clang, c_args(&rt, &ort), "clang (rt.c)"));
    let r3 = r2.and_then(|_| run(&clang, c_args(&krt, &okrt), "clang (kir_rt.c)"));
    let r3 = r3.and_then(|_| if wasi { run(&clang, c_args(&wasi_c, &owasi), "clang (wasi.c)") } else { Ok(()) });
    let r4 = r3.and_then(|_| {
        let mut args = vec![p(&oir), p(&ort), p(&okrt)];
        if wasi {
            args.push(p(&owasi));
        }
        args.extend([s("--no-entry"), s("--export=ksn_main")]);
        if wasi {
            args.push(s("--export=_start"));
        }
        args.extend([s("--gc-sections"), format!("--allow-undefined-file={}", p(&allow)), s("-o"), p(exe)]);
        run(&ld, args, "wasm-ld")
    });
    let _ = fs::remove_dir_all(&dir);
    if r4.is_err() {
        let _ = fs::remove_file(exe);
    }
    r4
}

fn clang_extra_args() -> Vec<String> {
    std::env::var("KSN_CLANG_ARGS").map(|s| s.split_whitespace().map(|x| x.to_string()).collect()).unwrap_or_default()
}

/// LLVM IR (`.ll` のテキスト) を clang で実行ファイル `exe` にする。
fn clang_ll(ll: &str, exe: &Path, opt: &str, target: Option<crate::kir_llvm::Target>) -> Result<(), String> {
    if crate::kir_llvm::is_wasm(target) {
        return wasm_link(ll, exe, opt, target);
    }
    let clang = std::env::var("KSN_CLANG").unwrap_or_else(|_| "clang".to_string());
    let src = exe.with_extension("ll.tmp");
    fs::write(&src, ll).map_err(|e| format!("{} を書けない: {}", src.display(), e))?;
    let rt = runtime_c_args(ll, &src);
    let o = std::process::Command::new(&clang)
        .args([opt, "-Wno-override-module"])
        .args(clang_target_args(target))
        .args(clang_extra_args())
        .args(["-x", "ir"])
        .arg(&src)
        .args(&rt)
        .arg("-o")
        .arg(exe)
        .arg("-lm")
        .arg("-pthread")
        .output();
    let _ = fs::remove_file(&src);
    // 一時的な C ファイルを削除する (引数は "-x c <path>" の繰り返し)。
    for i in (2..rt.len()).step_by(3) {
        let _ = fs::remove_file(&rt[i]);
    }
    match o {
        Err(e) => Err(format!("clang を起動できない ({}: {})", clang, e)),
        Ok(o) if !o.status.success() => Err(format!("clang が失敗した: {}", String::from_utf8_lossy(&o.stderr).trim())),
        Ok(_) => Ok(()),
    }
}

/// ビルド成果物の実行入口 (内部コマンド)。
fn cmd_exec(args: &[String]) -> i32 {
    let root = std::env::current_dir().unwrap_or_default();
    let l = match load_all(&root) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ksn: {}: {}", e.code, e.msg);
            return 2;
        }
    };
    let (prog, user) = build_program(&l);
    let prog = Rc::new(prog);
    if args.first().map(|s| s.as_str()) == Some("--test") {
        let name = args.get(1).cloned().unwrap_or_default();
        let Some(&ti) = user.iter().find(|&&i| prog.defs[i].kw == "test" && prog.defs[i].name == name) else {
            eprintln!("ksn: no such test {}", name);
            return 2;
        };
        let mut it = Interp::new(prog.clone());
        let r = it.call_body(ti);
        use std::io::Write;
        let _ = it.out.flush();
        return match r {
            Err(Ctl::Panic(m)) => {
                eprintln!("panic: {}", m);
                101
            }
            _ => 0,
        };
    }
    let Some(entry) = l.pkg.entry() else { return 2 };
    let Some(&di) = prog.funcs.get(&entry) else { return 2 };
    let mut it = Interp::new(prog);
    it.args = args.to_vec();
    it.run_entry(di)
}

// ------------------------------------------------------------ エントリ
pub fn main_cli(args: Vec<String>) -> i32 {
    // E2044 (型が決まらない) は既定で有効。`--no-strict-types` で旧来の挙動に戻す (移行用)。
    // `--strict-types` は受け付けるだけで何も変えない。どちらも全コマンド共通 (check / build / test / run)。
    let mut rest: Vec<String> = args[1..].to_vec();
    // `--` より後ろはプログラムへの引数なので触らない。
    let cut = rest.iter().position(|a| a == "--").unwrap_or(rest.len());
    if rest[..cut].iter().any(|a| a == "--no-strict-types") {
        crate::types::set_strict_types(false);
    }
    let tail = rest.split_off(cut);
    rest.retain(|a| a != "--no-strict-types" && a != "--strict-types");
    rest.extend(tail);
    match args[0].as_str() {
        "new" => match rest.first() {
            Some(n) => cmd_new(n),
            None => {
                eprintln!("usage: ksn new <name>");
                2
            }
        },
        "check" => match target_bits_arg(&rest) {
            // `--target wasm32-unknown-unknown` は `usize` / `isize` の幅を 32 ビットにして検査する (リテラルの範囲、修正プラン 4-2)。
            Ok(bits) => {
                let _width = crate::ptrwidth::Guard::set(bits);
                cmd_check(rest.iter().any(|a| a == "--json"))
            }
            Err(why) => {
                eprintln!("{}", why);
                1
            }
        },
        "check-stdlib" => cmd_check_stdlib(rest.iter().any(|a| a == "--json")),
        "run" => cmd_run(&rest),
        "test" => cmd_test(&rest),
        "get" => cmd_get(&rest),
        "sig" => cmd_sig(&rest),
        "doc" => cmd_doc(&rest),
        "index" => cmd_index(&rest),
        "refs" => cmd_refs(&rest),
        "rename" => cmd_rename(&rest),
        "fmt" => cmd_fmt(&rest),
        "fix" => cmd_fix(),
        "todo" => cmd_todo(),
        "context" => cmd_context(&rest),
        "explain" => cmd_explain(&rest),
        "bindgen" => crate::bindgen::cmd_bindgen(&rest),
        "lsp" => crate::lsp::run(),
        "debug" => crate::debug::cmd_debug(&rest),
        "dap" => crate::debug::cmd_dap(),
        "kir" => cmd_kir(&rest),
        "build" => cmd_build(&rest, None),
        "update" => cmd_update(&rest),
        "vendor" => cmd_vendor(),
        "_exec" => cmd_exec(&rest),
        "--version" | "-V" => {
            println!("ksn {}", VERSION);
            0
        }
        _ => {
            eprintln!(
                "usage: ksn <new|check|fix|todo|context|fmt|run|test|debug|dap|build|get|sig|doc|index|refs|rename|explain|lsp|update|vendor> [args]"
            );
            2
        }
    }
}

/// 試験用: パッケージ `root` を検査し、(プログラム, ユーザの定義の添字) を返す。検査が失敗すれば `None`。
#[cfg(test)]
pub(crate) fn checked_program_for_test(root: &Path) -> Option<(Program, Vec<usize>)> {
    let mut rep = Report::new(false);
    match full_check(root, &mut rep, false) {
        Ok((_, Some(prog), user)) if !rep.fatal() => Some((prog, user)),
        _ => None,
    }
}

#[cfg(test)]
mod both_tests {
    use super::*;
    fn s(x: &str) -> Option<String> {
        Some(x.to_string())
    }
    #[test]
    fn both_failing_with_different_classes_disagree() {
        // 文言が違っても分類が同じなら一致。
        assert!(!backends_disagree(&s("attempt to add with overflow"), &s("panic: arithmetic overflow")));
        assert!(!backends_disagree(&s("assertion failed: left: 1, right: 2"), &s("panic: assertion failed")));
        assert!(!backends_disagree(&s("called `Option::unwrap()` on a `None` value"), &s("panic: called unwrap on a None or Err value")));
        assert!(!backends_disagree(&s("attempt to divide by zero"), &s("panic: division by zero")));
        // 両方失敗でも、分類が違えば不一致。
        assert!(backends_disagree(&s("attempt to add with overflow"), &s("panic: assertion failed")));
        assert!(backends_disagree(&s("assertion failed: left: 1, right: 2"), &s("panic: arithmetic overflow")));
        assert!(backends_disagree(&s("called `Option::unwrap()` on a `None` value"), &s("panic: index out of bounds")));
        assert!(backends_disagree(&s("exit(3)"), &s("exit 4")));
        assert!(!backends_disagree(&s("exit(3)"), &s("exit 3")));
        // 片方だけ失敗。
        assert!(backends_disagree(&None, &s("panic: assertion failed")));
        assert!(!backends_disagree(&None, &None));
    }

    #[test]
    fn llvm_panic_texts_classify_like_the_tree_walker() {
        // KIR の LLVM 出力 (`ksn.panic.*` と `eprint` の文言) と、木走査の文言が同じ分類になること。
        // `kir_llvm::PANICS` と `interp.rs` の文言を並べる (片方を変えたらここが落ちる)。
        let pairs = [
            ("panic: attempt to add with overflow", "attempt to add with overflow", "overflow"),
            ("panic: attempt to subtract with overflow", "attempt to subtract with overflow", "overflow"),
            ("panic: attempt to multiply with overflow", "attempt to multiply with overflow", "overflow"),
            ("panic: attempt to divide with overflow", "attempt to divide with overflow", "overflow"),
            ("panic: attempt to calculate the remainder with overflow", "attempt to calculate the remainder with overflow", "overflow"),
            ("panic: attempt to negate with overflow", "attempt to negate with overflow", "overflow"),
            ("panic: attempt to divide by zero", "attempt to divide by zero", "divide"),
            (
                "panic: attempt to calculate the remainder with a divisor of by zero",
                "attempt to calculate the remainder with a divisor of by zero",
                "divide",
            ),
            (
                "panic: index out of bounds: the len is 3 but the index is 5",
                "index out of bounds: the len is 3 but the index is 5",
                "index",
            ),
            (
                "panic: called `Option::unwrap()` on a `None` value",
                "called `Option::unwrap()` on a `None` value",
                "unwrap",
            ),
            (
                "panic: called `Result::unwrap()` on an `Err` value: 5",
                "called `Result::unwrap()` on an `Err` value: 5",
                "unwrap",
            ),
            ("panic: assertion failed: left: 1, right: 2", "assertion failed: left: 1, right: 2", "assert"),
            ("panic: assertion failed: condition is false", "assertion failed: condition is false", "assert"),
        ];
        for (llvm, tree, class) in pairs {
            assert_eq!(fail_class(llvm), class, "{}", llvm);
            assert_eq!(fail_class(tree), class, "{}", tree);
            assert!(!backends_disagree(&s(llvm), &s(tree)), "{} / {}", llvm, tree);
        }
        // 終了の種類 (`exit(102)` は stack 超過、木走査の `Exit(102)` と同じ)。
        assert_eq!(fail_class("exit(102)"), "exit 102");
        assert!(!backends_disagree(&s("exit(102)"), &s("exit(102)")));
        assert!(backends_disagree(&s("exit(102)"), &s("panic: attempt to add with overflow")));
    }
}
