//! パッケージ(kasane.toml)・標準ライブラリの読み込み (stage0)。Python 参照実装 ksn/project.py の移植。

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::program::{load_file, Def};

pub const RESERVED_NAMES: &[&str] = &[
    "fn", "type", "enum", "trait", "impl", "const", "static", "extern", "test", "end", "import", "let", "mut", "if",
    "else", "match", "loop", "while", "for", "in", "break", "continue", "return", "true", "false", "self", "Self",
    "pub", "uses", "panics", "stack", "impls", "derive", "linear", "layout", "requires", "unsafe", "move", "with", "capture",
    "dyn", "blanket", "link", "never", "may", "async", "await", "where", "ref", "as", "i8", "i16", "i32", "i64",
    "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "bool", "char", "str", "core", "alloc",
    "std",
];
pub const BUNDLED: &[&str] = &["core", "alloc", "std"];

#[derive(Debug, Clone)]
pub struct ProjectError {
    pub code: String,
    pub msg: String,
}
pub fn perr<T>(code: &str, msg: impl Into<String>) -> Result<T, ProjectError> {
    Err(ProjectError { code: code.into(), msg: msg.into() })
}

#[derive(Debug, Clone)]
pub struct PErr {
    pub code: String,
    pub line: usize,
    pub col: usize,
    pub msg: String,
}

/// パース系のエラー文字列 "CODE|LINE|COL|MSG" を分解する。
pub fn decode_err(s: &str) -> PErr {
    let parts: Vec<&str> = s.splitn(4, '|').collect();
    if parts.len() == 4 && parts[0].starts_with('E') {
        if let (Ok(l), Ok(c)) = (parts[1].parse(), parts[2].parse()) {
            return PErr { code: parts[0].into(), line: l, col: c, msg: parts[3].into() };
        }
    }
    PErr { code: "E0002".into(), line: 1, col: 1, msg: s.to_string() }
}

pub fn norm(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

// ------------------------------------------------------------ 未保存バッファの上書き (ksn lsp 用)
// パスごとに「ディスクの代わりに読む本文」を持つ。空の間は従来どおりディスクだけを読む。
static OVERLAY: std::sync::Mutex<Option<std::collections::HashMap<String, String>>> = std::sync::Mutex::new(None);

fn overlay_key(path: &str) -> String {
    let p = Path::new(path);
    if p.is_absolute() {
        norm(p)
    } else {
        norm(&std::env::current_dir().unwrap_or_default().join(p))
    }
}

/// 上書き本文を設定する (`None` で解除)。
pub fn overlay_set(path: &str, text: Option<String>) {
    let mut g = OVERLAY.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(Default::default);
    match text {
        Some(t) => m.insert(overlay_key(path), t),
        None => m.remove(&overlay_key(path)),
    };
}

fn overlay_get(path: &str) -> Option<String> {
    let g = OVERLAY.lock().unwrap_or_else(|e| e.into_inner());
    g.as_ref().filter(|m| !m.is_empty()).and_then(|m| m.get(&overlay_key(path)).cloned())
}

/// 上書きがあればそれを、なければディスクの本文を返す (`overlay_hide` したパスは `None`)。
pub fn read_text(path: &str) -> Option<String> {
    match overlay_get(path) {
        Some(t) if t == HIDDEN => None,
        Some(t) => Some(t),
        None => fs::read_to_string(path).ok(),
    }
}

/// 上書き側で「存在しない」ことにするパス (改名の検証用)。空文字の本文で表す。
pub fn overlay_hide(path: &str) {
    overlay_set(path, Some(HIDDEN.to_string()));
}
const HIDDEN: &str = "\u{0}hidden\u{0}";

/// 上書きの全消去。
pub fn overlay_clear() {
    let mut g = OVERLAY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = g.as_mut() {
        m.clear();
    }
}

fn overlay_paths_under(dir: &str) -> Vec<String> {
    let g = OVERLAY.lock().unwrap_or_else(|e| e.into_inner());
    let pre = format!("{}/", dir);
    g.as_ref()
        .map(|m| m.keys().filter(|k| k.starts_with(&pre) && (k.ends_with(".ksnl") || k.ends_with(".ksnh"))).cloned().collect())
        .unwrap_or_default()
}

pub struct Source {
    pub path: String,
    #[allow(dead_code)]
    pub module: String,
    pub text: String,
    pub lines: Vec<String>,
    pub defs: Vec<Def>,
    pub error: Option<PErr>,
    /// `tests/` 配下の統合テストのファイルか (3.2)。
    pub is_test: bool,
    /// `.ksnh` から読んだソースのとき、横の `.ksnl` の本文 (無ければ `None`)。`path` は `.ksnh`。
    pub ksnl: Option<String>,
}

pub fn module_of(path: &str, root: &str) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path).trim_start_matches('/');
    let m = rel
        .trim_end_matches(".ksnh")
        .trim_end_matches(".ksnl")
        .trim_end_matches(".sig");
    m.replace('/', ".")
}

/// ソースを読む。不正な UTF-8 のときは置き換えた本文と、最初の不正なバイトの (行, 列) を返す (E0007)。
fn read_source(path: &str) -> (String, Option<(usize, usize)>) {
    if let Some(t) = overlay_get(path) {
        return (t, None);
    }
    let bytes = fs::read(path).unwrap_or_default();
    match String::from_utf8(bytes) {
        Ok(t) => (t, None),
        Err(e) => {
            let at = e.utf8_error().valid_up_to();
            let head = String::from_utf8_lossy(&e.as_bytes()[..at]).to_string();
            let line = head.matches('\n').count() + 1;
            let col = head.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            (String::from_utf8_lossy(e.as_bytes()).to_string(), Some((line, col)))
        }
    }
}

impl Source {
    /// 不正な UTF-8 だったファイルは E0007 (ほかの字句エラーより優先)。
    fn mark_bad_utf8(&mut self, bad: Option<(usize, usize)>) {
        if let Some((line, col)) = bad {
            self.error = Some(PErr { code: "E0007".into(), line, col, msg: "invalid UTF-8 byte sequence (source files must be valid UTF-8)".into() });
        }
    }

    pub fn load(path: &str, srcdir: &str) -> Source {
        let (text, bad) = read_source(path);
        let mut src = Source::from_text(path, srcdir, text);
        src.mark_bad_utf8(bad);
        src.attach_ksnl();
        src
    }

    /// `.ksnh` のソースに、横の `.ksnl` の本文を付ける。
    fn attach_ksnl(&mut self) {
        if let Some(stem) = self.path.strip_suffix(".ksnh") {
            self.ksnl = read_text(&format!("{}.ksnl", stem));
        }
    }

    pub fn from_text(path: &str, srcdir: &str, text: String) -> Source {
        let module = module_of(path, srcdir);
        Source::from_text_in(path, module, text, false)
    }

    /// `tests/` 配下のファイル。モジュール名は `<パッケージ>.tests.<ファイル>` (3.2 / 3.6)。
    pub fn load_test(path: &str, testsdir: &str, pkg: &str) -> Source {
        let (text, bad) = read_source(path);
        let module = format!("{}.tests.{}", pkg, module_of(path, testsdir));
        let mut src = Source::from_text_in(path, module, text, true);
        src.mark_bad_utf8(bad);
        src.attach_ksnl();
        src
    }

    /// テキストから `tests/` 配下のファイルを作る (`ksn test` が `///` のコード例を読み込むときに使う。3.2)。
    pub fn from_text_test(path: &str, testsdir: &str, pkg: &str, text: String) -> Source {
        let module = format!("{}.tests.{}", pkg, module_of(path, testsdir));
        Source::from_text_in(path, module, text, true)
    }

    fn from_text_in(path: &str, module: String, text: String, is_test: bool) -> Source {
        let lines = text.split('\n').map(String::from).collect();
        crate::refs_check::remember(path, &text);
        let (defs, error) = match load_file(path, &module, &text) {
            Ok(d) => (d, None),
            Err(e) => (vec![], Some(decode_err(&e))),
        };
        Source { path: path.to_string(), module, text, lines, defs, error, is_test, ksnl: None }
    }
}

pub struct Package {
    pub name: String,
    pub root: PathBuf,
    pub manifest: toml::Value,
    pub sources: Vec<Source>,
    /// 依存として読んだときの取得元 (`git+<URL>#<rev>` / `path+<ルートからの相対パス>`)。ルートは空。
    pub source: String,
}

impl Package {
    #[allow(dead_code)]
    pub fn defs(&self) -> Vec<Def> {
        self.sources.iter().flat_map(|s| s.defs.clone()).collect()
    }
    pub fn entry(&self) -> Option<String> {
        self.manifest.get("package").and_then(|p| p.get("entry")).and_then(|e| e.as_str()).map(String::from)
    }
}

pub fn read_manifest(root: &Path) -> Result<toml::Value, ProjectError> {
    let p = root.join("kasane.toml");
    let text = match read_text(&norm(&p)) {
        Some(t) => t,
        None => return perr("E9001", format!("kasane.toml not found in {}", norm(root))),
    };
    text.parse::<toml::Value>().or_else(|e| perr("E9002", format!("invalid kasane.toml: {}", e)))
}

fn is_name(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_lowercase()) && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `[native]` の C のライブラリの指定 (3.8)。`static` は静的アーカイブのパス、`dynamic` は `-l` のライブラリ名、
/// `search` はリンク時の探索先のディレクトリ。相対パスはパッケージのルート (`kasane.toml` のある場所) から。
/// 依存パッケージの `[native]` は読まず (リンクの追加は利用者の明示のままでいる)、`kasane.lock` に記録して比べる (8.1)。
#[derive(Debug, Clone, Default)]
pub struct NativeLibs {
    pub archives: Vec<String>,
    pub dynamic: Vec<String>,
    pub search: Vec<String>,
}

/// `[native]` の三つの配列 (無ければ空)。
pub fn native_libs(m: &toml::Value) -> NativeLibs {
    let get = |k: &str| -> Vec<String> {
        m.get("native")
            .and_then(|n| n.get(k))
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    };
    NativeLibs { archives: get("static"), dynamic: get("dynamic"), search: get("search") }
}

impl NativeLibs {
    pub fn is_empty(&self) -> bool {
        self.archives.is_empty() && self.dynamic.is_empty() && self.search.is_empty()
    }

    /// リンカに渡す引数 (`-L<探索先>`・アーカイブのパス・`-l<名前>`)。`root` はパッケージのルート (相対パスの起点)。
    pub fn link_args(&self, root: &Path) -> Vec<String> {
        let mut a: Vec<String> = vec![];
        for d in &self.search {
            a.push(format!("-L{}", root.join(d).display()));
        }
        for f in &self.archives {
            a.push(root.join(f).display().to_string());
        }
        for n in &self.dynamic {
            a.push(format!("-l{}", n));
        }
        a
    }
}

pub fn validate_manifest(m: &toml::Value) -> Vec<(String, String)> {
    let mut errs: Vec<(String, String)> = vec![];
    let e = |c: &str, m: String| (c.to_string(), m);
    let empty = toml::map::Map::new();
    let pk = m.get("package").and_then(|p| p.as_table()).unwrap_or(&empty);
    let name = pk.get("name").and_then(|n| n.as_str());
    match name {
        Some(n) if is_name(n) => {
            if RESERVED_NAMES.contains(&n) {
                errs.push(e("E9004", format!("package name '{}' is reserved", n)));
            }
        }
        _ => errs.push(e("E9003", "package.name must match [a-z][a-z0-9_]*".into())),
    }
    let ver_ok = pk.get("version").and_then(|v| v.as_str()).map_or(false, |v| {
        let p: Vec<&str> = v.split('.').collect();
        p.len() == 3 && p.iter().all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()))
    });
    if !ver_ok {
        errs.push(e("E9005", "package.version must be MAJOR.MINOR.PATCH".into()));
    }
    if !pk.get("edition").map_or(false, |v| v.is_integer()) {
        errs.push(e("E9006", "package.edition must be an integer".into()));
    }
    // T17: entry が文字列でないと黙ってライブラリ扱いになるので誤りにする (設計_tomlとlockの書き方.md 7 章)。
    if let Some(ev) = pk.get("entry") {
        if ev.as_str().is_none() {
            errs.push(e("E9007", "package.entry must be a string".into()));
        }
    }
    for k in pk.keys() {
        if !["name", "version", "edition", "entry"].contains(&k.as_str()) {
            errs.push(e("E9007", format!("unknown key package.{}", k)));
        }
    }
    if let Some(deps) = m.get("dependencies").and_then(|d| d.as_table()) {
        for (dn, dv) in deps {
            if BUNDLED.contains(&dn.as_str()) {
                errs.push(e("E9004", format!("dependency name '{}' is reserved", dn)));
            }
            if !is_name(dn) {
                errs.push(e("E9003", format!("dependency name '{}' must match [a-z][a-z0-9_]*", dn)));
            }
            if name == Some(dn.as_str()) {
                errs.push(e("E9011", format!("package {} depends on itself", dn)));
            }
            match dv.as_table() {
                Some(t) if t.contains_key("path") != t.contains_key("git") => {
                    if let Some(p) = t.get("path") {
                        if p.as_str().map_or(true, |x| x.is_empty()) {
                            errs.push(e("E9008", format!("dependency {}: path must be a non-empty string", dn)));
                        }
                    }
                    if t.contains_key("git") {
                        // URL は `git clone` の引数になるので、空やオプション風 (`-` 始まり) は受けない。
                        if t.get("git").and_then(|g| g.as_str()).map_or(true, |u| u.is_empty() || u.starts_with('-')) {
                            errs.push(e("E9008", format!("dependency {}: git must be a URL string", dn)));
                        }
                        let rev_ok = t.get("rev").and_then(|r| r.as_str()).map_or(false, |r| {
                            r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
                        });
                        if !rev_ok {
                            errs.push(e("E9008", format!("dependency {}: git requires a 40-hex rev", dn)));
                        }
                    } else if t.contains_key("rev") {
                        errs.push(e("E9007", format!("unknown key dependencies.{}.rev (path dependencies take no rev)", dn)));
                    }
                    for k in t.keys() {
                        if !["path", "git", "rev"].contains(&k.as_str()) {
                            errs.push(e("E9007", format!("unknown key dependencies.{}.{}", dn, k)));
                        }
                    }
                }
                Some(t) => {
                    errs.push(e("E9008", format!("dependency {}: exactly one of path / git required", dn)));
                    for k in t.keys() {
                        if !["path", "git", "rev"].contains(&k.as_str()) {
                            errs.push(e("E9007", format!("unknown key dependencies.{}.{}", dn, k)));
                        }
                    }
                }
                None => errs.push(e("E9008", format!("dependency {}: exactly one of path / git required", dn))),
            }
        }
    }
    // [unsafe] modules = ["app.sys"]: unsafe / extern / 生ポインタを許すモジュール (3.5)。
    if let Some(u) = m.get("unsafe") {
        match u.as_table() {
            None => errs.push(e("E9012", "[unsafe] must be a table".into())),
            Some(t) => {
                for (k, v) in t {
                    if k != "modules" {
                        errs.push(e("E9007", format!("unknown key unsafe.{}", k)));
                        continue;
                    }
                    let Some(arr) = v.as_array() else {
                        errs.push(e("E9012", "unsafe.modules must be an array of module names".into()));
                        continue;
                    };
                    for it in arr {
                        let Some(mn) = it.as_str() else {
                            errs.push(e("E9012", "unsafe.modules must contain strings".into()));
                            continue;
                        };
                        let comps: Vec<&str> = mn.split('.').collect();
                        if !comps.iter().all(|c| is_name(c)) {
                            errs.push(e("E9012", format!("unsafe.modules: invalid module name '{}'", mn)));
                        } else if name.map_or(true, |n| comps[0] != n) {
                            errs.push(e("E9012", format!("unsafe.modules: '{}' is not a module of this package", mn)));
                        }
                    }
                }
            }
        }
    }
    // [native] static / dynamic / search: C のライブラリを実行ファイルにリンクする (3.8)。
    if let Some(n) = m.get("native") {
        match n.as_table() {
            None => errs.push(e("E9012", "[native] must be a table".into())),
            Some(t) => {
                for (k, v) in t {
                    if !["static", "dynamic", "search"].contains(&k.as_str()) {
                        errs.push(e("E9007", format!("unknown key native.{}", k)));
                        continue;
                    }
                    let Some(arr) = v.as_array() else {
                        errs.push(e("E9012", format!("native.{} must be an array of strings", k)));
                        continue;
                    };
                    for it in arr {
                        let Some(s) = it.as_str() else {
                            errs.push(e("E9012", format!("native.{} must contain strings", k)));
                            continue;
                        };
                        // リンカの引数になるので、空や `-` 始まり (オプションと誤解される) は受けない。
                        if s.is_empty() || s.starts_with('-') {
                            errs.push(e("E9012", format!("native.{}: `{}` is not a library name or path", k, s)));
                        }
                        // `dynamic` は `-l<名前>` の名前なので、`lib:名前` の形・パス区切り・拡張子は書けない。
                        if k == "dynamic" && (s.contains(':') || s.contains('/') || s.contains('\\') || s.contains('.')) {
                            errs.push(e("E9012", format!("native.dynamic: `{}` is a library name (no `:`, no path separator, no extension)", s)));
                        }
                    }
                }
            }
        }
    }
    if let Some(t) = m.as_table() {
        for k in t.keys() {
            if k != "package" && k != "dependencies" && k != "unsafe" && k != "native" {
                errs.push(e("E9007", format!("unknown table {}", k)));
            }
        }
    }
    errs
}

fn walk_files(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            walk_files(&p, exts, out);
        } else if p.extension().and_then(|e| e.to_str()).map_or(false, |e| exts.contains(&e)) {
            out.push(p);
        }
    }
}

/// 読むファイルの一覧。`use_ksnh` のとき、`.ksnh` があるモジュールは `.ksnh` だけを (正として) 読み、横の `.ksnl` は
/// `Source::ksnl` に持つ。依存は `.ksnh` を読まない (`use_ksnh` が偽。`.ksnl` だけ)。
fn pick_sources(mut ps: Vec<String>, use_ksnh: bool) -> Vec<String> {
    ps.sort();
    ps.dedup();
    ps.retain(|p| overlay_get(p).map_or(true, |t| t != HIDDEN));
    if !use_ksnh {
        ps.retain(|p| p.ends_with(".ksnl"));
        return ps;
    }
    let hs: std::collections::HashSet<String> = ps.iter().filter(|p| p.ends_with(".ksnh")).map(|p| p.trim_end_matches(".ksnh").to_string()).collect();
    ps.retain(|p| !p.ends_with(".ksnl") || !hs.contains(p.trim_end_matches(".ksnl")));
    ps
}

pub fn load_dir_sources(srcdir: &Path, use_ksnh: bool) -> Vec<Source> {
    let mut paths = vec![];
    walk_files(srcdir, &["ksnl", "ksnh"], &mut paths);
    let mut ps: Vec<String> = paths.iter().map(|p| norm(p)).collect();
    let sd = norm(srcdir);
    ps.extend(overlay_paths_under(&sd));
    pick_sources(ps, use_ksnh).iter().map(|p| Source::load(p, &sd)).collect()
}

/// `<root>/tests/` 配下の統合テスト (3.2)。依存パッケージの `tests/` は読まない。
pub fn load_test_sources(root: &Path, pkg: &str) -> Vec<Source> {
    let td = root.join("tests");
    let mut paths = vec![];
    walk_files(&td, &["ksnl", "ksnh"], &mut paths);
    let mut ps: Vec<String> = paths.iter().map(|p| norm(p)).collect();
    let tds = norm(&td);
    ps.extend(overlay_paths_under(&tds));
    pick_sources(ps, true).iter().map(|p| Source::load_test(p, &tds, pkg)).collect()
}

pub fn load_package(root: &Path) -> Result<Package, ProjectError> {
    let mut p = load_package_src_opt(root, true)?;
    let t = load_test_sources(&p.root, &p.name);
    p.sources.extend(t);
    Ok(p)
}

/// `src/` だけを読む (依存パッケージ用)。
pub fn load_package_src(root: &Path) -> Result<Package, ProjectError> {
    load_package_src_opt(root, false)
}

fn load_package_src_opt(root: &Path, use_ksnh: bool) -> Result<Package, ProjectError> {
    let root = fs::canonicalize(root).unwrap_or(root.to_path_buf());
    let root = PathBuf::from(norm(&root).trim_start_matches("//?/"));
    let m = read_manifest(&root)?;
    let errs = validate_manifest(&m);
    if let Some((c, msg)) = errs.into_iter().next() {
        return perr(&c, msg);
    }
    let name = m["package"]["name"].as_str().unwrap().to_string();
    let sources = load_dir_sources(&root.join("src"), use_ksnh);
    Ok(Package { name, root, manifest: m, sources, source: String::new() })
}

// ------------------------------------------------------------ 依存の解決 (8.1)

/// ビルドキャッシュのルート (`KSN_CACHE_DIR`、既定は `~/.ksn/cache`)。
pub fn cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("KSN_CACHE_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".ksn").join("cache")
}

/// 取得済みの git 依存の置き場 (`<キャッシュ>/git/<URL と rev のハッシュ>`)。内容は `rev` で決まる。
pub fn git_cache_path(url: &str, rev: &str) -> PathBuf {
    let mut h = Sha256::new();
    h.update(url.as_bytes());
    h.update([0u8]);
    h.update(rev.as_bytes());
    let hex: String = h.finalize().iter().map(|b| format!("{:02x}", b)).collect();
    cache_dir().join("git").join(&hex[..32])
}

fn git_cmd() -> std::process::Command {
    let mut c = std::process::Command::new("git");
    // 認証を尋ねて止まらない。外部コマンドを起動する転送 (`ext::`) は許さない。
    c.env("GIT_TERMINAL_PROMPT", "0").env("GIT_ALLOW_PROTOCOL", "file:https:http:ssh:git").stdin(std::process::Stdio::null());
    c
}

fn git_run(args: &[&str]) -> Result<String, String> {
    match git_cmd().args(args).output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(format!("cannot run git: {}", e)),
    }
}

/// `url` の `rev` を `dest` に取り出す (`.git` は残さない。内容は `rev` のコミットのファイルだけ)。
pub fn git_materialize(url: &str, rev: &str, dest: &Path) -> Result<(), ProjectError> {
    let fail = |m: String| -> Result<(), ProjectError> { perr("E9018", format!("cannot fetch git dependency {}#{}: {}", url, rev, m)) };
    let tmp = PathBuf::from(format!("{}.tmp{}", norm(dest), std::process::id()));
    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::remove_dir_all(&tmp);
    let t = norm(&tmp);
    // `--` を挟み、URL がオプションとして解釈されないようにする。
    if let Err(m) = git_run(&["clone", "--quiet", "--no-checkout", "--", url, &t]) {
        let _ = fs::remove_dir_all(&tmp);
        return fail(m);
    }
    let r = git_run(&["-C", &t, "-c", "advice.detachedHead=false", "checkout", "--quiet", rev, "--"]).and_then(|_| {
        let head = git_run(&["-C", &t, "rev-parse", "HEAD"])?;
        if head == rev { Ok(()) } else { Err(format!("checked out {} instead of {}", head, rev)) }
    });
    if let Err(m) = r {
        let _ = fs::remove_dir_all(&tmp);
        return fail(m);
    }
    let _ = fs::remove_dir_all(tmp.join(".git"));
    let _ = fs::remove_dir_all(dest);
    if let Err(e) = fs::rename(&tmp, dest) {
        let _ = fs::remove_dir_all(&tmp);
        return fail(e.to_string());
    }
    Ok(())
}

/// git 依存をどう取得するか。
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GitMode {
    /// `vendor/<名前>` かキャッシュだけを使う (`check` / `build` / `test` / `run`。ネットワークに出ない)。
    Offline,
    /// 無いときだけ取得する。
    IfMissing,
    /// 必ず取得し直す (`update` / `vendor`。取得元が正で、古いキャッシュを信用しない)。
    Fresh,
}

pub fn load_deps(pkg: &Package) -> Result<Vec<Package>, ProjectError> {
    load_deps_with(pkg, &|_| GitMode::Offline)
}

/// 推移的な依存をすべて読む (ルート自身は含まない)。`mode` は依存名ごとの git の取得方法。
pub fn load_deps_with(pkg: &Package, mode: &dyn Fn(&str) -> GitMode) -> Result<Vec<Package>, ProjectError> {
    let mut seen: Vec<(String, String)> = vec![];
    let mut out = vec![];
    resolve_rec(pkg, pkg, mode, &mut seen, &mut out)?;
    Ok(out)
}

/// 依存 `dn` の (取得元文字列, 置き場所)。
fn locate_dep(root: &Package, pkg: &Package, dn: &str, dv: &toml::Value, mode: &dyn Fn(&str) -> GitMode) -> Result<(String, PathBuf), ProjectError> {
    if let Some(p) = dv.get("path").and_then(|p| p.as_str()) {
        // 8.1: git 依存先は git 依存だけを持てる。
        if pkg.source.starts_with("git+") {
            return perr("E9013", format!("git dependency {} has a path dependency {}; a git dependency may only depend on git dependencies", pkg.name, dn));
        }
        let d = normalize_path(&pkg.root.join(p));
        let source = format!("path+{}", relative_path(&norm(&d), &norm(&root.root)));
        // ビルド成果物 (`target/*/pkg`) には元の `path` が無いので、複製された `vendor/<名前>` を使う。
        let vend = root.root.join("vendor").join(dn);
        let loc = if !d.is_dir() && vend.is_dir() { vend } else { d };
        return Ok((source, loc));
    }
    let url = dv.get("git").and_then(|g| g.as_str()).unwrap_or("");
    let rev = dv.get("rev").and_then(|g| g.as_str()).unwrap_or("");
    let source = format!("git+{}#{}", url, rev);
    let cache = git_cache_path(url, rev);
    let m = mode(dn);
    if m == GitMode::Fresh {
        git_materialize(url, rev, &cache)?;
        return Ok((source, cache));
    }
    let v = root.root.join("vendor").join(dn);
    if v.is_dir() {
        if let Ok(mark) = fs::read_to_string(v.join(".ksn-source")) {
            if mark.trim() != source {
                return perr("E9019", format!("vendor/{} was vendored from {} but kasane.toml says {}; run `ksn vendor`", dn, mark.trim(), source));
            }
        }
        return Ok((source, v));
    }
    if cache.is_dir() {
        return Ok((source, cache));
    }
    if m == GitMode::IfMissing {
        git_materialize(url, rev, &cache)?;
        return Ok((source, cache));
    }
    perr("E9010", format!("git dependency {} is not vendored or cached (run `ksn vendor` or `ksn update`)", dn))
}

fn resolve_rec(
    root: &Package,
    pkg: &Package,
    mode: &dyn Fn(&str) -> GitMode,
    seen: &mut Vec<(String, String)>,
    out: &mut Vec<Package>,
) -> Result<(), ProjectError> {
    let Some(deps) = pkg.manifest.get("dependencies").and_then(|d| d.as_table()) else { return Ok(()) };
    for (dn, dv) in deps {
        if *dn == root.name {
            return perr("E9015", format!("dependency cycle: {} depends back on the root package {}", pkg.name, root.name));
        }
        let (source, loc) = locate_dep(root, pkg, dn, dv, mode)?;
        if let Some((_, prev)) = seen.iter().find(|(n, _)| n == dn) {
            // 菱形依存: 同じ取得元なら1つとして扱い、違う取得元はエラー (8.1。版の自動調停はしない)。
            if *prev != source {
                return perr("E9014", format!("package {} is required from two different sources: {} and {}", dn, prev, source));
            }
            continue;
        }
        let mut dp = load_package_src(&loc)?;
        if dp.name != *dn {
            return perr("E9011", format!("dependency {}: package name is {}", dn, dp.name));
        }
        dp.source = source.clone();
        seen.push((dn.clone(), source));
        let mut sub = vec![];
        resolve_rec(root, &dp, mode, seen, &mut sub)?;
        out.push(dp);
        out.extend(sub);
    }
    Ok(())
}

pub fn normalize_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `base` から `target` への `/` 区切りの相対パス (どちらも正規化済みの絶対パス)。
pub fn relative_path(target: &str, base: &str) -> String {
    let t: Vec<&str> = target.split('/').collect();
    let b: Vec<&str> = base.split('/').collect();
    let mut i = 0;
    while i < t.len() && i < b.len() && t[i] == b[i] {
        i += 1;
    }
    let mut out: Vec<String> = vec!["..".to_string(); b.len() - i];
    out.extend(t[i..].iter().map(|x| x.to_string()));
    if out.is_empty() { ".".into() } else { out.join("/") }
}

/// 8.1 のパッケージハッシュ。対象は `kasane.toml` と `src/` 配下の `*.ksnl`。
/// パスはルートからの相対 (`/` 区切り) でバイト列順に並べ、各ファイルを
/// 「パス, 0x00, 内容長 (u64 LE), 生バイト列」で連結して SHA-256 を取る。
pub fn package_hash(root: &Path) -> String {
    try_package_hash(root).unwrap_or_default()
}

/// `package_hash` に、対象にシンボリックリンクがあれば E9016 を返す検査を足したもの。
pub fn try_package_hash(root: &Path) -> Result<String, ProjectError> {
    let mut files: Vec<String> = vec!["kasane.toml".into()];
    let mut ps = vec![];
    walk_files(&root.join("src"), &["ksnl"], &mut ps);
    let base = norm(root);
    for p in ps {
        files.push(norm(&p).strip_prefix(&format!("{}/", base)).unwrap_or("").to_string());
    }
    files.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    files.dedup();
    let mut h = Sha256::new();
    for rel in files {
        // ファイルそのものも、途中のディレクトリもシンボリックリンクであってはならない。
        let mut cur = root.to_path_buf();
        for c in rel.split('/') {
            cur = cur.join(c);
            if fs::symlink_metadata(&cur).map_or(false, |m| m.file_type().is_symlink()) {
                return perr("E9016", format!("symbolic links are not allowed in a hashed package: {}", rel));
            }
        }
        let data = fs::read(root.join(&rel)).unwrap_or_default();
        h.update(rel.as_bytes());
        h.update([0u8]);
        h.update((data.len() as u64).to_le_bytes());
        h.update(&data);
    }
    Ok(format!("sha256:{}", h.finalize().iter().map(|b| format!("{:02x}", b)).collect::<String>()))
}

/// `kasane.lock` の検査 (8.1)。`deps` は `load_deps` の結果。見つけた違反を (コード, メッセージ) で返す。
pub fn verify_lock(pkg: &Package, deps: &[Package]) -> Vec<(String, String)> {
    let mut errs: Vec<(String, String)> = vec![];
    let lock = pkg.root.join("kasane.lock");
    if !deps.is_empty() && !lock.exists() {
        errs.push(("E9020".into(), "kasane.lock is missing; run `ksn update`".into()));
        return errs;
    }
    if !lock.exists() {
        return errs;
    }
    let bad = |m: String| ("E9021".to_string(), format!("kasane.toml and kasane.lock disagree: {}; run `ksn update`", m));
    let lock_text = fs::read_to_string(&lock).unwrap_or_default();
    let v: toml::Value = match lock_text.parse() {
        Ok(v) => v,
        Err(e) => return vec![bad(format!("invalid kasane.lock ({})", e))],
    };
    let version = v.get("version").and_then(|x| x.as_integer());
    if version != Some(1) && version != Some(2) && version != Some(3) {
        return vec![bad("lock version must be 1, 2 or 3".into())];
    }
    let entries: Vec<&toml::Value> = v.get("package").and_then(|p| p.as_array()).map(|a| a.iter().collect()).unwrap_or_default();
    let mut by_name: std::collections::BTreeMap<String, &toml::Value> = Default::default();
    for e in &entries {
        let Some(n) = e.get("name").and_then(|n| n.as_str()) else {
            errs.push(bad("a [[package]] has no name".into()));
            continue;
        };
        if by_name.insert(n.to_string(), e).is_some() {
            errs.push(bad(format!("package {} is listed twice", n)));
        }
    }
    for dp in deps {
        if !by_name.contains_key(&dp.name) {
            errs.push(bad(format!("package {} is missing from the lock", dp.name)));
        }
    }
    for n in by_name.keys() {
        if !deps.iter().any(|d| d.name == *n) {
            errs.push(bad(format!("package {} is in the lock but not a dependency", n)));
        }
    }
    for dp in deps {
        let Some(e) = by_name.get(&dp.name) else { continue };
        let lock_source = e.get("source").and_then(|s| s.as_str()).unwrap_or("(none)");
        if lock_source != dp.source {
            errs.push(bad(format!("source of {} is {} in kasane.toml but {} in the lock", dp.name, dp.source, lock_source)));
        }
        let mut want: Vec<String> =
            dp.manifest.get("dependencies").and_then(|d| d.as_table()).map(|t| t.keys().cloned().collect()).unwrap_or_default();
        want.sort();
        let have: Vec<String> = e
            .get("deps")
            .and_then(|d| d.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if want != have {
            errs.push(bad(format!("deps of {} are {:?} but the lock says {:?}", dp.name, want, have)));
        }
        if dp.source.starts_with("git+") {
            match try_package_hash(&dp.root) {
                Err(e) => errs.push((e.code, e.msg)),
                Ok(actual) => match e.get("hash").and_then(|h| h.as_str()) {
                    None => errs.push(bad(format!("git dependency {} has no hash", dp.name))),
                    Some(h) if h != actual => errs.push((
                        "E9022".into(),
                        format!(
                            "hash mismatch for {}: kasane.lock says {} but {} has {}; the vendored copy or cache was modified (run `ksn vendor` to restore it)",
                            dp.name,
                            h,
                            norm(&dp.root),
                            actual
                        ),
                    )),
                    _ => {}
                },
            }
            // 版 3: `version` は manifest の `package.version` と一致する必要がある。
            if version == Some(3) {
                let lock_ver = e.get("version").and_then(|v| v.as_str()).unwrap_or("");
                let actual_ver = dp
                    .manifest
                    .get("package")
                    .and_then(|p| p.get("version"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if lock_ver != actual_ver {
                    errs.push(bad(format!(
                        "version of {} is {} in kasane.toml but {} in the lock",
                        dp.name, actual_ver, lock_ver
                    )));
                }
            }
            // 版 2・3: 公開の署名と効果の記録と比べる (`path` 依存は記録しない)。
            if version == Some(2) || version == Some(3) {
                match crate::lockapi::read_api(e) {
                    None => errs.push(bad(format!("git dependency {} has no recorded api; run `ksn lock --accept {}`", dp.name, dp.name))),
                    Some((old, old_uns, old_nat)) => {
                        errs.extend(crate::lockapi::compare(
                            &dp.name,
                            &old,
                            &old_uns,
                            &crate::lockapi::api_of(dp),
                            &crate::lockapi::unsafe_of(dp),
                            &old_nat,
                            &crate::lockapi::native_of(dp),
                        ));
                    }
                }
            }
        }
    }
    errs
}

pub fn stdlib_dir() -> PathBuf {
    if let Ok(d) = std::env::var("KSN_STDLIB") {
        return PathBuf::from(d);
    }
    let mut starts: Vec<PathBuf> = vec![];
    if let Ok(e) = std::env::current_exe() {
        starts.push(e.parent().unwrap().to_path_buf());
    }
    if let Ok(c) = std::env::current_dir() {
        starts.push(c);
    }
    for s in starts {
        let mut d = Some(s.as_path());
        while let Some(x) = d {
            let c = x.join("spec").join("stdlib");
            if c.is_dir() {
                return c;
            }
            let c2 = x.join("stdlib");
            if c2.is_dir() {
                return c2;
            }
            d = x.parent();
        }
    }
    let rel = PathBuf::from("spec/stdlib");
    if rel.is_dir() {
        return rel;
    }
    embedded_stdlib_dir().unwrap_or(rel)
}

/// 実行体に埋め込んだ標準ライブラリの宣言 (`assets::STDLIB`) を、キャッシュの場所 (`KSN_CACHE_DIR` > `~/.cache/ksn` > 一時) の
/// `stdlib-<実行体の識別子>/` に展開してその場所を返す (1 度だけ。展開済みなら再利用する)。ディスクに `spec/stdlib` があるときは使われない。
fn embedded_stdlib_dir() -> Option<PathBuf> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let base = std::env::var_os("KSN_CACHE_DIR")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache").join("ksn")))
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("stdlib-{}", env!("KSN_BUILD_ID")));
        if dir.join(".done").exists() {
            return Some(dir);
        }
        let tmp = base.join(format!("stdlib-{}.{}.tmp", env!("KSN_BUILD_ID"), std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let mut b = crate::assets::STDLIB;
        while b.len() >= 4 {
            let n = u32::from_le_bytes(b[..4].try_into().ok()?) as usize;
            let rel = std::str::from_utf8(b.get(4..4 + n)?).ok()?;
            let m = u64::from_le_bytes(b.get(4 + n..12 + n)?.try_into().ok()?) as usize;
            let data = b.get(12 + n..12 + n + m)?;
            let p = tmp.join(rel);
            fs::create_dir_all(p.parent()?).ok()?;
            fs::write(&p, data).ok()?;
            b = &b[12 + n + m..];
        }
        fs::write(tmp.join(".done"), "").ok()?;
        if fs::rename(&tmp, &dir).is_err() {
            // 別のプロセスが先に展開した。
            let _ = fs::remove_dir_all(&tmp);
            if !dir.join(".done").exists() {
                return None;
            }
        }
        Some(dir)
    })
    .clone()
}

/// 32 ビットのターゲット (wasm32) の `core/num.sig`: `usize` / `isize` の定数を 32 ビットの値にし、
/// 32 ビットでは値を失いうる変換 (`usize.from_u64`・`isize.from_i64`・`isize.from_u32`) を `try_from_` にする。
/// 64 ビット用に生成した文面 (`spec/tools/gen_num_sig.py`) を、その行だけ書き換える。
fn num_sig_32(text: &str) -> String {
    let fixed = [
        ("const core.num.usize.BITS:", "const core.num.usize.BITS: u32 = 32u32 pub"),
        ("const core.num.usize.MAX:", "const core.num.usize.MAX: usize = 4294967295usize pub"),
        ("const core.num.isize.BITS:", "const core.num.isize.BITS: u32 = 32u32 pub"),
        ("const core.num.isize.MAX:", "const core.num.isize.MAX: isize = 2147483647isize pub"),
        ("const core.num.isize.MIN:", "const core.num.isize.MIN: isize = -2147483648isize pub"),
    ];
    let lossy = [("usize", "u64"), ("isize", "i64"), ("isize", "u32")];
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let mut l = line.to_string();
        if let Some((_, new)) = fixed.iter().find(|(p, _)| line.starts_with(p)) {
            l = new.to_string();
        }
        for (t, m) in lossy {
            if line.starts_with(&format!("fn core.num.{t}.from_{m}(")) {
                l = format!("fn core.num.{t}.try_from_{m}(x: {m}) -> Result<{t}, core.num.TryFromIntError> uses [] panics never pub");
            }
        }
        out.push_str(&l);
        out.push('\n');
    }
    out
}

#[cfg(test)]
pub fn walk_files_for_test(root: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    walk_files(root, exts, out)
}

pub fn load_stdlib() -> Vec<Source> {
    let root = stdlib_dir();
    let rs = norm(&root);
    let mut ps: Vec<(usize, String)> = vec![];
    for (k, ext) in ["sig", "ksnl"].iter().enumerate() {
        let mut fs = vec![];
        walk_files(&root, &[*ext, "ksnh"], &mut fs);
        let mut fs: Vec<String> = fs.iter().map(|p| norm(p)).filter(|p| p.ends_with(&format!(".{}", ext)) || (k == 1 && p.ends_with(".ksnh"))).collect();
        if k == 1 {
            fs = pick_sources(fs, true);
        }
        fs.sort();
        ps.extend(fs.into_iter().map(|p| (k, p)));
    }
    // 文面は先にまとめて読む (読み込み用の overlay (`--watch` / LSP) があればそちらが優先。`Source::load` と同じ)。
    let mut texts: Vec<(String, String)> = ps
        .iter()
        .map(|(_, p)| {
            let t = match overlay_get(p) {
                Some(t) => t,
                None => match String::from_utf8(fs::read(p).unwrap_or_default()) {
                    Ok(t) => t,
                    Err(e) => String::from_utf8_lossy(e.as_bytes()).to_string(),
                },
            };
            (p.clone(), t)
        })
        .collect();
    if crate::ptrwidth::bits() == 32 {
        for (p, t) in texts.iter_mut().filter(|(p, _)| p.ends_with("core/num.sig")) {
            *t = num_sig_32(t);
        }
    }
    // 構文解析の結果は実行体の識別子と全文面を鍵にして保存してある (stdcache.rs)。命中すれば構文解析しない。
    let use_cache = crate::check_cache::enabled();
    let key = if use_cache { Some(crate::stdcache::key(&texts)) } else { None };
    let mods: Vec<String> = texts.iter().map(|(p, _)| module_of(p, &rs)).collect();
    if let Some(k) = &key {
        let files: Vec<(&str, &str)> = texts.iter().zip(&mods).map(|((p, _), m)| (p.as_str(), m.as_str())).collect();
        let ld = crate::stdcache::load(k, &files);
        if let Some(defs) = ld {
            return texts
                .into_iter()
                .zip(mods)
                .zip(defs)
                .map(|(((path, text), module), defs)| {
                    let lines = text.split('\n').map(String::from).collect();
                    Source { path, module, text, lines, defs, error: None, is_test: false, ksnl: None }
                })
                .collect();
        }
    }
    let out = parse_stdlib(texts, &rs);
    if let Some(k) = &key {
        if out.iter().all(|s| s.error.is_none()) {
            let ds: Vec<&[Def]> = out.iter().map(|s| s.defs.as_slice()).collect();
            crate::stdcache::store(k, &ds);
        }
    }
    out
}

fn parse_stdlib(texts: Vec<(String, String)>, rs: &str) -> Vec<Source> {
    // 構文解析は互いに独立なので、ファイルごとにスレッドで並列化する。順序は保つ。
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8);
    if n == 1 || texts.len() < 4 {
        return texts.into_iter().map(|(p, t)| Source::from_text(&p, rs, t)).collect();
    }
    // 大きいファイルから始める (最後に残った 1 本が全体を待たせないように)。
    let mut order: Vec<usize> = (0..texts.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(texts[i].1.len()));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<Source>>> = texts.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|sc| {
        for _ in 0..n {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= texts.len() {
                    break;
                }
                let i = order[i];
                *slots[i].lock().unwrap() = Some(Source::from_text(&texts[i].0, rs, texts[i].1.clone()));
            });
        }
    });
    slots.into_iter().map(|m| m.into_inner().unwrap().expect("stdlib 読み込み")).collect()
}

pub fn find_root() -> Result<PathBuf, ProjectError> {
    let mut d = std::env::current_dir().unwrap_or_default();
    loop {
        if d.join("kasane.toml").exists() {
            return Ok(d);
        }
        if !d.pop() {
            return perr("E9001", "kasane.toml not found (run inside a Kasane package)");
        }
    }
}

/// `[unsafe] modules` に列挙されたモジュール名 (完全一致または接頭辞一致の対象)。
pub fn unsafe_modules(m: &toml::Value) -> Vec<String> {
    m.get("unsafe")
        .and_then(|u| u.get("modules"))
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}
