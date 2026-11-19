//! `kasane.toml` / `kasane.lock` の字面の規則 (設計_tomlとlockの書き方.md 7 章)。
//!
//! - 字句は `.ksnl` と同じ (lexrules.rs を呼ぶ。同じ規則を 2 か所に書かない): 書けない文字 E0004、
//!   NFC でない E0006 (BOM も E0004)。CR (CRLF) は E0001。
//! - 整形は `ksn fmt` が正規形を作り、`ksn check` は出力と違えば E0020 (`.ksnl` と同じ扱い)。
//!   エスケープ (`\uXXXX`)・整数の別の書き方 (`0x1`・`+1`・`1_0`)・単一引用符・複数行文字列・
//!   点付きキー・引用符付きキー・インラインの表・`[dependencies.x]`・`package = [...]` は、
//!   正規形への書き換えで E0020 が拾う (意味が同じでも文面が違えば誤り)。
//! - コメント `#` は許す (字句の規則は同じ)。`ksn fmt` はコメントを消さない: コメントはその行の
//!   キー (または末尾) に属するとみなし、正規形ではそのキーの行の直前に置く。
//! - 整形できない誤り (意味の誤り・ロックの構造の誤り L1〜L6) があるときは整形せず、その誤りを
//!   報告する (整形でデータを落とさないため)。

use std::collections::{HashMap, HashSet};

use toml::Value;
use unicode_normalization::UnicodeNormalization;

use crate::lexrules::Finding;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Manifest,
    Lock,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Manifest => "kasane.toml",
            Kind::Lock => "kasane.lock",
        }
    }
}

pub struct Analyzed {
    pub findings: Vec<Finding>,
    /// 正規形の本文。`None` は整形で直せない誤りがあるということ。
    pub canon: Option<String>,
}

/// `ksn check` と `ksn fmt` の共通の本体。字句の検査と、正規形との比較 (E0020)。
pub fn analyze(kind: Kind, text: &str) -> Analyzed {
    let mut findings: Vec<Finding> = vec![];
    for f in crate::lexrules::scan(text) {
        findings.push(f);
    }
    if text.contains('\r') {
        findings.push(Finding { code: "E0001", line: 1, col: 1, msg: "CR (CRLF) is not allowed; use LF".into() });
    }
    let v: Value = match text.parse() {
        Ok(v) => v,
        Err(e) => {
            findings.push(Finding { code: "E9002", line: 1, col: 1, msg: format!("invalid {}: {}", kind.name(), e) });
            return Analyzed { findings, canon: None };
        }
    };
    let canon = match kind {
        // 意味の誤り (未知のキー・型の外れ) は読み込み側 (project.rs) が報告する。ここでは整形しない。
        Kind::Manifest => {
            if crate::project::validate_manifest(&v).is_empty() {
                Some(emit_manifest(&v, &anchors_manifest(&v, text)))
            } else {
                None
            }
        }
        Kind::Lock => {
            let before = findings.len();
            let a = anchors_lock(&v, text, &mut findings);
            // 構造の誤り (L1〜L6) があるときは整形しない。version が 1 でないときも書き換えない。
            let shape_ok = findings.len() == before && v.get("version").and_then(|x| x.as_integer()) == Some(1);
            shape_ok.then(|| emit_lock(&v, &a))
        }
    };
    e0020(text, canon, &mut findings)
}

fn e0020(text: &str, canon: Option<String>, findings: &mut Vec<Finding>) -> Analyzed {
    if let Some(c) = &canon {
        if c != text {
            let (x, y): (Vec<&str>, Vec<&str>) = (text.lines().collect(), c.lines().collect());
            let n = (0..x.len().max(y.len())).filter(|&i| x.get(i) != y.get(i)).count();
            let first = (0..x.len().max(y.len())).find(|&i| x.get(i) != y.get(i)).unwrap_or(0);
            findings.push(Finding {
                code: "E0020",
                line: first + 1,
                col: 1,
                msg: format!("the file differs from the `ksn fmt` output ({} line(s) differ, first at line {}): run `ksn fmt`", n, first + 1),
            });
        }
    }
    Analyzed { findings: std::mem::take(findings), canon }
}

// ------------------------------------------------------------ コメントの割り当て (生の文面 → キー)

type Anchors = HashMap<String, Vec<String>>;

fn flush(a: &mut Anchors, pending: &mut Vec<String>, path: &str) {
    if !pending.is_empty() {
        a.entry(path.to_string()).or_default().append(pending);
    }
}

/// 行頭の裸のキー (`key = ...` の `key`)。点付き・引用符付きのキーは正規形でないので `None`
/// (その行のコメントは次のキーに回す)。
fn bare_key(t: &str) -> Option<&str> {
    let (k, _) = t.split_once('=')?;
    let k = k.trim();
    (!k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(k)
}

/// 行の内容の後のコメント (`# ...`)。文字列の中の `#` は数えない。
fn trailing_comment(t: &str) -> Option<&str> {
    let (mut in_str, mut esc) = (false, false);
    for (i, c) in t.char_indices() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
        } else if c == '#' {
            return Some(t[i..].trim());
        }
    }
    None
}

/// 正規形の本文に行を置く。
fn put(lines: &mut Vec<String>, a: &Anchors, path: &str) {
    if let Some(cs) = a.get(path) {
        lines.extend(cs.iter().cloned());
    }
}

/// 表のヘッダの前の空行 (正規形: 表の間は 1 つの空行。ファイルの先頭には付けない)。
fn blank(lines: &mut Vec<String>) {
    if !lines.is_empty() {
        lines.push(String::new());
    }
}

fn finish(lines: Vec<String>) -> String {
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

// ------------------------------------------------------------ kasane.toml

/// コメントの割り当て。既知のキーの行だけを拾い、ほかの行のコメントは次のキー (または末尾) に回す。
fn anchors_manifest(v: &Value, raw: &str) -> Anchors {
    let deps: HashSet<&str> =
        v.get("dependencies").and_then(|d| d.as_table()).map(|t| t.keys().map(|k| k.as_str()).collect()).unwrap_or_default();
    let mut a: Anchors = HashMap::new();
    let mut pending: Vec<String> = vec![];
    let mut section = "";
    for line in raw.split('\n') {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        match if t.starts_with('#') { Some(t) } else { trailing_comment(t) } {
            Some(c) => pending.push(c.to_string()),
            None => {}
        }
        if t.starts_with('[') {
            let name = t.trim_matches(|c| c == '[' || c == ']').trim();
            if matches!(name, "package" | "unsafe" | "dependencies") {
                section = name;
                flush(&mut a, &mut pending, name);
            }
            continue;
        }
        if let Some(k) = bare_key(t) {
            let known = match section {
                "package" => matches!(k, "name" | "version" | "edition" | "entry"),
                "unsafe" => k == "modules",
                "dependencies" => deps.contains(k),
                _ => false,
            };
            if known {
                flush(&mut a, &mut pending, &format!("{}.{}", section, k));
            }
        }
    }
    flush(&mut a, &mut pending, "");
    a
}

fn emit_manifest(v: &Value, a: &Anchors) -> String {
    let mut lines: Vec<String> = vec![];
    let empty = toml::map::Map::new();
    let pk = v.get("package").and_then(|p| p.as_table()).unwrap_or(&empty);
    put(&mut lines, a, "package");
    lines.push("[package]".into());
    for k in ["name", "version", "edition", "entry"] {
        if let Some(val) = pk.get(k) {
            put(&mut lines, a, &format!("package.{}", k));
            lines.push(format!("{} = {}", k, emit_value(val)));
        }
    }
    if let Some(u) = v.get("unsafe") {
        blank(&mut lines);
        put(&mut lines, a, "unsafe");
        lines.push("[unsafe]".into());
        if let Some(m) = u.get("modules") {
            put(&mut lines, a, "unsafe.modules");
            lines.push(format!("modules = {}", emit_array(m)));
        }
    }
    if let Some(d) = v.get("dependencies").and_then(|x| x.as_table()) {
        blank(&mut lines);
        put(&mut lines, a, "dependencies");
        lines.push("[dependencies]".into());
        for (dn, dv) in d {
            put(&mut lines, a, &format!("dependencies.{}", dn));
            lines.push(format!("{} = {}", dn, emit_inline_table(dv)));
        }
    }
    put(&mut lines, a, "");
    finish(lines)
}

fn emit_value(v: &Value) -> String {
    match v {
        Value::String(s) => canon_str(s),
        Value::Integer(i) => i.to_string(),
        // 検査が通る形には現れない (読み込み側が弾く)。
        other => other.to_string(),
    }
}

fn emit_array(v: &Value) -> String {
    let items: Vec<String> = v.as_array().map(|a| a.iter().map(emit_value).collect()).unwrap_or_default();
    format!("[{}]", items.join(", "))
}

fn emit_inline_table(v: &Value) -> String {
    match v.as_table() {
        Some(t) => {
            let items: Vec<String> = t.iter().map(|(k, val)| format!("{} = {}", k, emit_value(val))).collect();
            format!("{{ {} }}", items.join(", "))
        }
        None => emit_value(v),
    }
}

/// 文字列の正規形。エスケープは `.ksnl` と同じ規則 (lexrules.rs): 構文上要るもの (`\\` と `\"`)、
/// 名前のあるもの (`\n` `\t` `\r`)、そのまま書けない文字の `\uXXXX` / `\UXXXXXXXX`。NFC に直す。
fn canon_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.nfc() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\u0000"),
            c if crate::lexrules::needs_u_escape(c) => {
                if c as u32 <= 0xFFFF {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                } else {
                    out.push_str(&format!("\\U{:08x}", c as u32));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ------------------------------------------------------------ kasane.lock

/// ロックの構造の検査 (L1〜L6) とコメントの割り当て。
/// `entries` は生の文面の `[[package]]` の順と同じ (TOML の配列は順を保つ)。
fn anchors_lock(v: &Value, raw: &str, findings: &mut Vec<Finding>) -> Anchors {
    let entries: Vec<&Value> = v.get("package").and_then(|p| p.as_array()).map(|a| a.iter().collect()).unwrap_or_default();
    let name_of = |i: usize| -> String {
        entries.get(i).and_then(|e| e.get("name")).and_then(|n| n.as_str()).unwrap_or("~").to_string()
    };
    let mut a: Anchors = HashMap::new();
    let mut pending: Vec<String> = vec![];
    let mut idx = 0usize; // 現在の [[package]] の (生の文面での) 番号
    let mut in_pkg = false;
    let mut prev: Option<(String, usize, bool)> = None; // (名前, ヘッダの行, deps があったか)。L6 の検査用
    let mut dep_line: HashMap<String, usize> = HashMap::new();
    let mut hash_line: HashMap<String, usize> = HashMap::new();
    for (i, line) in raw.split('\n').enumerate() {
        let ln = i + 1;
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        match if t.starts_with('#') { Some(t) } else { trailing_comment(t) } {
            Some(c) => pending.push(c.to_string()),
            None => {}
        }
        if t.starts_with("[[") {
            if let Some((n, h, sd)) = prev.take() {
                close_entry(findings, n, h, sd);
            }
            in_pkg = true;
            let n = name_of(idx);
            idx += 1;
            flush(&mut a, &mut pending, &format!("package:{}", n));
            prev = Some((n, ln, false));
            continue;
        }
        if t.starts_with('[') {
            findings.push(Finding { code: "E9007", line: ln, col: 1, msg: format!("unknown table {} in kasane.lock", t) });
            continue;
        }
        let Some(k) = bare_key(t) else { continue };
        if !in_pkg {
            if k == "version" {
                flush(&mut a, &mut pending, "version");
            } else if k == "package" {
                // L5: インラインの配列表。
                findings.push(Finding {
                    code: "E9007",
                    line: ln,
                    col: 1,
                    msg: "inline `package = [...]` is not allowed in kasane.lock; write [[package]] tables".into(),
                });
            } else {
                findings.push(Finding { code: "E9007", line: ln, col: 1, msg: format!("unknown key {} in kasane.lock", k) });
            }
            continue;
        }
        let n = name_of(idx.wrapping_sub(1));
        match k {
            "name" | "source" | "hash" | "deps" => {
                if k == "deps" {
                    if let Some(p) = prev.as_mut() {
                        p.2 = true;
                    }
                    dep_line.insert(n.clone(), ln);
                }
                if k == "hash" {
                    hash_line.insert(n.clone(), ln);
                }
                flush(&mut a, &mut pending, &format!("package:{}.{}", n, k));
            }
            _ => {
                // L1: [[package]] の中の知らないキー。
                findings.push(Finding {
                    code: "E9007",
                    line: ln,
                    col: 1,
                    msg: format!("unknown key {} in [[package]] {} of kasane.lock", k, n),
                });
            }
        }
    }
    if let Some((n, h, sd)) = prev.take() {
        close_entry(findings, n, h, sd);
    }
    // L2 / L3: deps の中身と、path 依存の hash。位置は生の文面の行。
    for e in &entries {
        let Some(n) = e.get("name").and_then(|x| x.as_str()) else { continue };
        match e.get("deps").map(|d| d.as_array()) {
            None => {}
            Some(None) => findings.push(Finding {
                code: "E9021",
                line: *dep_line.get(n).unwrap_or(&1),
                col: 1,
                msg: format!("deps of {} must be an array of strings", n),
            }),
            Some(Some(arr)) => {
                if arr.iter().any(|x| x.as_str().is_none()) {
                    findings.push(Finding {
                        code: "E9021",
                        line: *dep_line.get(n).unwrap_or(&1),
                        col: 1,
                        msg: format!("deps of {} must contain only strings", n),
                    });
                }
            }
        }
        if e.get("hash").is_some() && !e.get("source").and_then(|s| s.as_str()).unwrap_or("").starts_with("git+") {
            findings.push(Finding {
                code: "E9021",
                line: *hash_line.get(n).unwrap_or(&1),
                col: 1,
                msg: format!("hash of {} is recorded but {} is not a git dependency", n, n),
            });
        }
    }
    flush(&mut a, &mut pending, "");
    a
}

/// L6: `[[package]]` に `deps` がない。
fn close_entry(findings: &mut Vec<Finding>, n: String, header_line: usize, saw_deps: bool) {
    if !saw_deps {
        findings.push(Finding {
            code: "E9021",
            line: header_line,
            col: 1,
            msg: format!("deps of {} are missing; run `ksn update`", n),
        });
    }
}

fn emit_lock(v: &Value, a: &Anchors) -> String {
    let mut lines: Vec<String> = vec![];
    put(&mut lines, a, "version");
    lines.push("version = 1".into());
    let mut entries: Vec<&Value> = v.get("package").and_then(|p| p.as_array()).map(|x| x.iter().collect()).unwrap_or_default();
    entries.sort_by_key(|e| e.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string());
    for e in entries {
        let label = format!("package:{}", e.get("name").and_then(|n| n.as_str()).unwrap_or("~"));
        blank(&mut lines);
        put(&mut lines, a, &label);
        lines.push("[[package]]".into());
        for k in ["name", "source", "hash", "deps"] {
            if let Some(val) = e.get(k) {
                put(&mut lines, a, &format!("{}.{}", label, k));
                let s = if k == "deps" { emit_array(val) } else { emit_value(val) };
                lines.push(format!("{} = {}", k, s));
            }
        }
    }
    put(&mut lines, a, "");
    finish(lines)
}

// ------------------------------------------------------------ 試験

#[cfg(test)]
mod tests {
    use super::*;

    const TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";

    fn find<'a>(an: &'a Analyzed, code: &str) -> bool {
        an.findings.iter().any(|f| f.code == code)
    }

    #[test]
    fn canonical_toml_passes_and_is_a_fixpoint() {
        let an = analyze(Kind::Manifest, TOML);
        assert!(an.findings.is_empty(), "{:?}", an.findings.iter().map(|f| (&f.code, &f.msg)).collect::<Vec<_>>());
        assert_eq!(an.canon.as_deref(), Some(TOML));
    }

    #[test]
    fn canonical_lock_passes_and_matches_ksn_update() {
        let lock = "version = 1\n\n[[package]]\nname = \"lib\"\nsource = \"path+../lib\"\ndeps = []\n";
        let an = analyze(Kind::Lock, lock);
        assert!(an.findings.is_empty(), "{:?}", an.findings.iter().map(|f| (&f.code, &f.msg)).collect::<Vec<_>>());
        assert_eq!(an.canon.as_deref(), Some(lock));
    }

    #[test]
    fn deviations_become_e0020() {
        for bad in [
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 0x1\nentry = \"t.main\"\n",
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = 't.main'\n",
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"\"\"t.main\"\"\"\n",
            "package = { name = \"t\", version = \"0.1.0\", edition = 1 }\n",
            "[package]\n\"entry\" = \"t.main\"\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\n",
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies.x]\npath = \"..\"\n",
        ] {
            let an = analyze(Kind::Manifest, bad);
            assert!(find(&an, "E0020"), "{}: {:?}", bad, an.findings.iter().map(|f| (&f.code, &f.msg)).collect::<Vec<_>>());
        }
        for bad in ["version = 0x1\n", "version = 1\n\npackage = [{name = \"x\"}]\n"] {
            let an = analyze(Kind::Lock, bad);
            assert!(find(&an, "E0020") || find(&an, "E9007"), "{}: {:?}", bad, an.findings);
        }
    }

    #[test]
    fn lexical_rules_apply() {
        let an = analyze(Kind::Manifest, "\u{feff}[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n");
        assert!(find(&an, "E0004"));
        let an = analyze(Kind::Manifest, "# caf\u{65}\u{301}\n[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n");
        assert!(find(&an, "E0006"));
        let an = analyze(Kind::Lock, "version = 1\r\n");
        assert!(find(&an, "E0001"));
    }

    #[test]
    fn comments_survive_fmt_and_are_a_fixpoint() {
        let src = "# the package\n[package]\nname = \"t\" # main package\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n# deps below\n[dependencies]\nk = { path = \"../k\" } # local\n# trailing\n";
        let an = analyze(Kind::Manifest, src);
        let canon = an.canon.expect("canon");
        for c in ["# the package", "# main package", "# deps below", "# local", "# trailing"] {
            assert!(canon.contains(c), "{} missing in {}", c, canon);
        }
        let again = analyze(Kind::Manifest, &canon);
        assert!(!find(&again, "E0020"), "{:?}", again.findings.iter().map(|f| (&f.code, &f.msg)).collect::<Vec<_>>());
        assert_eq!(again.canon.as_deref(), Some(canon.as_str()));
    }

    #[test]
    fn lock_structure_errors_block_fmt() {
        // 知らないキー・deps の省略・非文字列・path 依存の hash。
        let an = analyze(Kind::Lock, "version = 1\nextra = 2\n");
        assert!(find(&an, "E9007") && an.canon.is_none());
        let an = analyze(Kind::Lock, "version = 1\n\n[[package]]\nname = \"x\"\nsource = \"path+../x\"\n");
        assert!(find(&an, "E9021") && an.canon.is_none());
        let an = analyze(Kind::Lock, "version = 1\n\n[[package]]\nname = \"x\"\nsource = \"path+../x\"\ndeps = [1]\n");
        assert!(find(&an, "E9021") && an.canon.is_none());
        let an = analyze(
            Kind::Lock,
            "version = 1\n\n[[package]]\nname = \"x\"\nsource = \"path+../x\"\nhash = \"sha256:00\"\ndeps = []\n",
        );
        assert!(find(&an, "E9021") && an.canon.is_none());
    }
}
