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
}

pub fn module_of(path: &str, root: &str) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path).trim_start_matches('/');
    let m = rel
        .trim_end_matches(".ksnh")
        .trim_end_matches(".ksnl")
        .trim_end_matches(".sig");
    m.replace('/', ".")
}

impl Source {
    pub fn load(path: &str, srcdir: &str) -> Source {
        let text = match overlay_get(path) {
            Some(t) => t,
            None => String::from_utf8_lossy(&fs::read(path).unwrap_or_default()).to_string(),
        };
        Source::from_text(path, srcdir, text)
    }

    pub fn from_text(path: &str, srcdir: &str, text: String) -> Source {
        let module = module_of(path, srcdir);
        let lines = text.split('\n').map(String::from).collect();
        let (defs, error) = match load_file(path, &module, &text) {
            Ok(d) => (d, None),
            Err(e) => (vec![], Some(decode_err(&e))),
        };
        Source { path: path.to_string(), module, text, lines, defs, error }
    }
}

pub struct Package {
    pub name: String,
    pub root: PathBuf,
    pub manifest: toml::Value,
    pub sources: Vec<Source>,
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
            match dv.as_table() {
                Some(t) if t.contains_key("path") != t.contains_key("git") => {
                    if t.contains_key("git") {
                        let rev_ok = t.get("rev").and_then(|r| r.as_str()).map_or(false, |r| {
                            r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
                        });
                        if !rev_ok {
                            errs.push(e("E9008", format!("dependency {}: git requires a 40-hex rev", dn)));
                        }
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
    if let Some(t) = m.as_table() {
        for k in t.keys() {
            if k != "package" && k != "dependencies" && k != "unsafe" {
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

pub fn load_dir_sources(srcdir: &Path) -> Vec<Source> {
    let mut paths = vec![];
    walk_files(srcdir, &["ksnl", "ksnh"], &mut paths);
    let mut ps: Vec<String> = paths.iter().map(|p| norm(p)).collect();
    let sd = norm(srcdir);
    ps.extend(overlay_paths_under(&sd));
    ps.sort();
    ps.dedup();
    ps.retain(|p| overlay_get(p).map_or(true, |t| t != HIDDEN));
    ps.iter().map(|p| Source::load(p, &sd)).collect()
}

pub fn load_package(root: &Path) -> Result<Package, ProjectError> {
    let root = fs::canonicalize(root).unwrap_or(root.to_path_buf());
    let root = PathBuf::from(norm(&root).trim_start_matches("//?/"));
    let m = read_manifest(&root)?;
    let errs = validate_manifest(&m);
    if let Some((c, msg)) = errs.into_iter().next() {
        return perr(&c, msg);
    }
    let name = m["package"]["name"].as_str().unwrap().to_string();
    let sources = load_dir_sources(&root.join("src"));
    Ok(Package { name, root, manifest: m, sources })
}

pub fn load_deps(pkg: &Package) -> Result<Vec<Package>, ProjectError> {
    let mut seen: Vec<String> = vec![];
    load_deps_rec(pkg, &mut seen)
}

fn load_deps_rec(pkg: &Package, seen: &mut Vec<String>) -> Result<Vec<Package>, ProjectError> {
    let mut out = vec![];
    let Some(deps) = pkg.manifest.get("dependencies").and_then(|d| d.as_table()) else { return Ok(out) };
    for (dn, dv) in deps {
        if seen.contains(dn) {
            continue;
        }
        let droot = if let Some(p) = dv.get("path").and_then(|p| p.as_str()) {
            let joined = pkg.root.join(p);
            normalize_path(&joined)
        } else {
            let v = pkg.root.join("vendor").join(dn);
            if !v.is_dir() {
                return perr("E9010", format!("git dependency {} is not vendored (run ksn vendor)", dn));
            }
            v
        };
        let dp = load_package(&droot)?;
        if dp.name != *dn {
            return perr("E9011", format!("dependency {}: package name is {}", dn, dp.name));
        }
        seen.push(dn.clone());
        let sub = load_deps_rec(&dp, seen)?;
        out.push(dp);
        out.extend(sub);
    }
    Ok(out)
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

pub fn package_hash(root: &Path) -> String {
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
        let data = fs::read(root.join(&rel)).unwrap_or_default();
        h.update(rel.as_bytes());
        h.update([0u8]);
        h.update((data.len() as u64).to_le_bytes());
        h.update(&data);
    }
    format!("sha256:{}", h.finalize().iter().map(|b| format!("{:02x}", b)).collect::<String>())
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
    PathBuf::from("spec/stdlib")
}

pub fn load_stdlib() -> Vec<Source> {
    let root = stdlib_dir();
    let rs = norm(&root);
    let mut out = vec![];
    for ext in ["sig", "ksnl"] {
        let mut ps = vec![];
        walk_files(&root, &[ext], &mut ps);
        let mut ps: Vec<String> = ps.iter().map(|p| norm(p)).collect();
        ps.sort();
        for p in ps {
            out.push(Source::load(&p, &rs));
        }
    }
    out
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
