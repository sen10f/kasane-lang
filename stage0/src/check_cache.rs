//! `ksn check` の増分化 (第 1 段、設計_増分検査.md)。
//!
//! 保存するのは「ソースファイル 1 本の文面だけで決まる検査」の結果 (正規形の判定・lint・文法)。
//! 意味の検査 (型・効果・借用・再帰) は他の定義に依存するので毎回やり直す。
//! キーは ksn 実行体の中身・ファイルのパス・ソースの文面。キーが 1 つでも違えば使わない。
//! 破損・不一致は黙って捨てて再計算する (読めなかった = 未命中)。

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub type LintOut = Vec<(String, usize, String)>;

#[derive(Clone, Debug, PartialEq)]
pub struct FileCheck {
    /// 文法の一致 `(一致したか, メッセージ, 行)`。
    pub grammar: (bool, String, usize),
    /// `ksn fmt` で直せるので lint を報告しない。
    pub skip: bool,
    pub lint: LintOut,
}

/// `KSN_CHECK_CACHE`: `1` で有効、`0` で無効。未設定の既定は `DEFAULT_ON`。
pub fn enabled() -> bool {
    match std::env::var("KSN_CHECK_CACHE").ok().as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => DEFAULT_ON,
    }
}

const DEFAULT_ON: bool = false;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

/// ksn 実行体の中身のハッシュ (版・検査の実装が変わればキーが変わる)。
fn exe_key() -> &'static str {
    static K: OnceLock<String> = OnceLock::new();
    K.get_or_init(|| {
        let mut h = Sha256::new();
        h.update(env!("CARGO_PKG_VERSION").as_bytes());
        if let Ok(exe) = std::env::current_exe() {
            h.update(fs::read(exe).unwrap_or_default());
        }
        hex(&h.finalize())
    })
}

fn key(path: &str, rel: &str, text: &str) -> String {
    let mut h = Sha256::new();
    for part in [exe_key(), path, rel, text] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    hex(&h.finalize())
}

/// 1 ファイル 1 スロット。内容が変われば上書きするので、置き場は自然に有限になる。
fn slot(path: &str, rel: &str) -> PathBuf {
    let mut h = Sha256::new();
    h.update(path.as_bytes());
    h.update([0u8]);
    h.update(rel.as_bytes());
    crate::project::cache_dir().join("check").join(format!("{}.json", &hex(&h.finalize())[..32]))
}

fn data_of(c: &FileCheck) -> Value {
    json!({
        "grammar": [c.grammar.0, c.grammar.1, c.grammar.2],
        "skip": c.skip,
        "lint": c.lint.iter().map(|(a, b, m)| json!([a, b, m])).collect::<Vec<_>>(),
    })
}

fn sum_of(data: &Value) -> String {
    hex(&Sha256::digest(serde_json::to_string(data).unwrap_or_default().as_bytes()))
}

fn parse(v: &Value) -> Option<FileCheck> {
    let g = v.get("grammar")?.as_array()?;
    let grammar = (g.first()?.as_bool()?, g.get(1)?.as_str()?.to_string(), g.get(2)?.as_u64()? as usize);
    let skip = v.get("skip")?.as_bool()?;
    let mut lint = vec![];
    for e in v.get("lint")?.as_array()? {
        let e = e.as_array()?;
        lint.push((e.first()?.as_str()?.to_string(), e.get(1)?.as_u64()? as usize, e.get(2)?.as_str()?.to_string()));
    }
    Some(FileCheck { grammar, skip, lint })
}

pub fn load(path: &str, rel: &str, text: &str) -> Option<FileCheck> {
    let raw = fs::read_to_string(slot(path, rel)).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    if v.get("key")?.as_str()? != key(path, rel, text) {
        return None;
    }
    let data = v.get("data")?;
    if v.get("sum")?.as_str()? != sum_of(data) {
        return None;
    }
    parse(data)
}

pub fn store(path: &str, rel: &str, text: &str, c: &FileCheck) {
    let p = slot(path, rel);
    let Some(dir) = p.parent() else { return };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let data = data_of(c);
    let v = json!({ "key": key(path, rel, text), "sum": sum_of(&data), "data": data });
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    if fs::write(&tmp, serde_json::to_string(&v).unwrap_or_default()).is_ok() && fs::rename(&tmp, &p).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}
