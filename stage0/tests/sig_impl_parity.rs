//! `spec/stdlib/**/*.sig` の宣言と木走査 (インタプリタ) の実装の食い違いの検出 (G10)。
//! `std.io.stdin()` が `panics never` と宣言されたまま「実装がない」で落ちた事故の再発防止。許す一覧は持たない。
//!
//! - 自由関数 (`self` なし) は厳密に見る: Kasane の定義 (`.ksnl`)、組み込み表 `NATIVE_NAMES`、木走査が名前で扱う `core.ptr.*`・`extern`・
//!   数値の変換 (`wrapping_` など) のどれか。
//! - メソッドは受け手の値の種類で振り分けるため、ソース中の文字列で近似する: 名前が `stage0/src` (検査・KIR を除く) に文字列として現れること
//!   (近似なので見逃しはありうるが、誤報はしない。型名は見ない: チャネル・`Mutex` などは型名でなく値の種類で振り分ける)。

use std::fs;
use std::path::{Path, PathBuf};

fn files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let mut es: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    es.sort();
    for p in es {
        if p.is_dir() {
            files(&p, ext, out);
        } else if p.extension().is_some_and(|e| e == ext) {
            out.push(p);
        }
    }
}

struct Sig {
    name: String,
    at: String,
    method: bool,
    ext: bool,
}

fn sigs(root: &Path) -> Vec<Sig> {
    let mut fs_ = Vec::new();
    files(&root.join("spec/stdlib"), "sig", &mut fs_);
    let mut out = Vec::new();
    for f in fs_ {
        for (i, l) in fs::read_to_string(&f).unwrap().lines().enumerate() {
            let (ext, rest) = match l.strip_prefix("extern fn ") {
                Some(r) => (true, r),
                None => match l.strip_prefix("fn ") {
                    Some(r) => (false, r),
                    None => continue,
                },
            };
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '.' || *c == '_').collect();
            let after = &rest[name.len()..];
            // 型引数の `[..]` (中に `(` を含む) を括弧の深さで読み飛ばし、そのあとの `(` から引数が始まる。
            let mut depth = 0;
            let mut params = None;
            for (i, c) in after.char_indices() {
                match c {
                    '[' => depth += 1,
                    ']' => depth -= 1,
                    '(' if depth == 0 => {
                        params = Some(&after[i + 1..]);
                        break;
                    }
                    _ => {}
                }
            }
            let Some(params) = params else { continue };
            out.push(Sig {
                name,
                at: format!("{}:{}", f.strip_prefix(root).unwrap().display(), i + 1),
                method: params.starts_with("self"),
                ext,
            });
        }
    }
    out
}

fn quoted(s: &str) -> Vec<String> {
    s.split('"').skip(1).step_by(2).map(|x| x.to_string()).collect()
}

fn missing(methods: bool) -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut ks = Vec::new();
    files(&root.join("spec/stdlib"), "ksnl", &mut ks);
    let mut bodies = std::collections::HashSet::new();
    for f in ks {
        for l in fs::read_to_string(f).unwrap().lines() {
            if let Some(r) = l.strip_prefix("fn ") {
                bodies.insert(r.chars().take_while(|c| c.is_alphanumeric() || *c == '.' || *c == '_').collect::<String>());
            }
        }
    }
    let nat = fs::read_to_string(root.join("stage0/src/natives.rs")).unwrap();
    let names: std::collections::HashSet<String> = quoted(nat.split("];").next().unwrap()).into_iter().collect();
    let extra: std::collections::HashSet<String> =
        quoted(nat.split("const NUM_EXTRA").nth(1).unwrap().split("];").next().unwrap()).into_iter().collect();
    let mut src = String::new();
    let mut rs = Vec::new();
    files(&root.join("stage0/src"), "rs", &mut rs);
    for f in rs {
        let b = f.file_name().unwrap().to_string_lossy().to_string();
        if ["kir", "check", "types", "borrowck", "infer", "resolve", "lsp", "doc", "refactor", "fmt", "lint"].iter().any(|p| b.starts_with(p)) {
            continue;
        }
        src += &fs::read_to_string(f).unwrap();
    }
    let has = |w: &str| src.contains(&format!("\"{}\"", w));
    let mut out = Vec::new();
    for s in sigs(&root) {
        if s.ext || s.method != methods || bodies.contains(&s.name) || names.contains(&s.name) || s.name.starts_with("core.ptr.") {
            continue;
        }
        let parts: Vec<&str> = s.name.split('.').collect();
        let last = parts[parts.len() - 1];
        if parts.len() == 4 && parts[0] == "core" && matches!(parts[1], "num" | "bool" | "char") {
            let pre = ["wrapping_", "checked_", "saturating_", "from_", "try_from_", "trunc_to_"];
            if pre.iter().any(|p| last.starts_with(p)) || extra.contains(last) {
                continue;
            }
        }
        if methods {
            if has(last) {
                continue;
            }
        }
        out.push(format!("{} ({})", s.name, s.at));
    }
    out
}

#[test]
fn every_sig_free_function_has_a_tree_walker_body() {
    let m = missing(false);
    assert!(m.is_empty(), ".sig に宣言があるのに木走査の本体がない自由関数:\n{}", m.join("\n"));
}

#[test]
fn every_sig_method_has_a_tree_walker_body() {
    let m = missing(true);
    assert!(m.is_empty(), ".sig に宣言があるのに木走査の本体がないメソッド:\n{}", m.join("\n"));
}
