//! `ksn check` の増分化 (第 1 段、設計_増分検査.md)。
//!
//! 保存するのは「ソースファイル 1 本の文面だけで決まる検査」の結果 (正規形の判定・lint・文法)。
//! 意味の検査 (型・効果・借用・再帰) は他の定義に依存するので毎回やり直す。
//! キーは ksn 実行体の中身・ファイルのパス・ソースの文面。キーが 1 つでも違えば使わない。
//! 破損・不一致は黙って捨てて再計算する (読めなかった = 未命中)。
//!
//! 置き場 `<KSN_CACHE_DIR>/check/` は上限 (`KSN_CHECK_CACHE_MAX`、既定 64 MiB) を超えたら、
//! 更新時刻の古いものから消す (`trim`)。命中のたびに更新時刻を進めるので LRU になる。
//! 複数の `ksn` が同じ置き場を同時に使っても壊れない: 書き込みは一時ファイルから `rename` で置き換え、
//! 読む側は中途半端なファイルを見ない。消えたファイルは未命中になるだけ。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::SystemTime;

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

/// 既定は有効。`KSN_CHECK_CACHE=0` で無効、`1` で有効。
/// 置き場の上限と古いものの削除 (`trim`) を入れたので、既定で有効にできる (設計_増分検査.md 4 章)。
pub fn enabled() -> bool {
    match std::env::var("KSN_CHECK_CACHE").ok().as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => DEFAULT_ON,
    }
}

const DEFAULT_ON: bool = true;

/// 置き場の既定の上限 (バイト)。1 ファイルは数 KB なので、64 MiB で数万ファイル分になる。
/// ビルドキャッシュの `KSN_CACHE_MAX` (既定 2 GiB) とは別の予算にする。
const DEFAULT_MAX: u64 = 64 * 1024 * 1024;

/// 置き場の上限 (バイト)。`KSN_CHECK_CACHE_MAX` で変える。
pub fn max_bytes() -> u64 {
    std::env::var("KSN_CHECK_CACHE_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_MAX)
}

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

/// 置き場のディレクトリ (`<KSN_CACHE_DIR>/check`)。
fn dir() -> PathBuf {
    crate::project::cache_dir().join("check")
}

/// 1 ファイル 1 スロット。内容が変われば上書きするので、スロット数はパスの数で決まる。
/// 総量は `trim` が上限に収める。
fn slot(path: &str, rel: &str) -> PathBuf {
    let mut h = Sha256::new();
    h.update(path.as_bytes());
    h.update([0u8]);
    h.update(rel.as_bytes());
    dir().join(format!("{}.json", &hex(&h.finalize())[..32]))
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

/// 中身を検証して読む。キー・`sum`・形のどれかが合わなければ `None`。
fn parse_entry(raw: &str, path: &str, rel: &str, text: &str) -> Option<FileCheck> {
    let v: Value = serde_json::from_str(raw).ok()?;
    if v.get("key")?.as_str()? != key(path, rel, text) {
        return None;
    }
    let data = v.get("data")?;
    if v.get("sum")?.as_str()? != sum_of(data) {
        return None;
    }
    parse(data)
}

/// 命中したスロットの更新時刻を今にする (LRU の「最近使った」)。失敗しても結果は変わらない。
fn touch(p: &Path) {
    if let Ok(f) = fs::OpenOptions::new().write(true).open(p) {
        let _ = f.set_modified(SystemTime::now());
    }
}

pub fn load(path: &str, rel: &str, text: &str) -> Option<FileCheck> {
    let p = slot(path, rel);
    // 読めない (無い・ディレクトリ) は未命中。消すものがない。
    let raw = fs::read_to_string(&p).ok()?;
    match parse_entry(&raw, path, rel, text) {
        Some(c) => {
            touch(&p);
            Some(c)
        }
        None => {
            // 壊れている、または別の文面・版のもの。捨てる (呼び出し側がこのあと上書きする)。
            let _ = fs::remove_file(&p);
            None
        }
    }
}

static SEQ: AtomicU64 = AtomicU64::new(0);

pub fn store(path: &str, rel: &str, text: &str, c: &FileCheck) {
    let p = slot(path, rel);
    let Some(d) = p.parent() else { return };
    if fs::create_dir_all(d).is_err() {
        return;
    }
    let data = data_of(c);
    let v = json!({ "key": key(path, rel, text), "sum": sum_of(&data), "data": data });
    // 一時ファイルは同じ置き場に作り、rename で置き換える (同時に書いても読む側は完全なものだけを見る)。
    let tmp = p.with_extension(format!("tmp{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    let ok = fs::write(&tmp, serde_json::to_string(&v).unwrap_or_default()).is_ok() && fs::rename(&tmp, &p).is_ok();
    if !ok {
        let _ = fs::remove_file(&tmp);
    }
}

/// 置き場を上限 (`max_bytes`) まで縮める。`ksn check` の終わりに 1 回呼ぶ。
pub fn trim() {
    trim_dir(&dir(), max_bytes());
}

/// `dir` 直下のファイル (一時ファイルを含む) の合計を `max` 以下にする。更新時刻の古いものから消す。
/// ディレクトリは数えず消さない。他のプロセスが先に消した場合は無視する。
fn trim_dir(dir: &Path, max: u64) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut ents: Vec<(SystemTime, PathBuf, u64)> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            if !m.is_file() {
                return None;
            }
            Some((m.modified().unwrap_or(SystemTime::UNIX_EPOCH), e.path(), m.len()))
        })
        .collect();
    let mut total: u64 = ents.iter().map(|e| e.2).sum();
    if total <= max {
        return;
    }
    ents.sort();
    for (_, p, len) in ents {
        if total <= max {
            break;
        }
        // 消せなかったもの (権限など) は合計から引かない。
        if fs::remove_file(&p).is_ok() {
            total -= len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn trim_removes_oldest_first_until_under_limit() {
        let d = std::env::temp_dir().join(format!("ksn_cc_trim_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        for i in 0..5u64 {
            let p = d.join(format!("e{}.json", i));
            fs::write(&p, vec![b'x'; 100]).unwrap();
            fs::OpenOptions::new().write(true).open(&p).unwrap().set_modified(base + Duration::from_secs(i * 10)).unwrap();
        }
        // 5 本 × 100 バイト、上限 250: 古い 3 本が消え、新しい 2 本が残る。
        trim_dir(&d, 250);
        let names = |d: &Path| {
            let mut v: Vec<String> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
            v.sort();
            v
        };
        assert_eq!(names(&d), vec!["e3.json", "e4.json"]);
        // 上限 0 なら全部消えるが、置き場のディレクトリは残る。
        trim_dir(&d, 0);
        assert!(names(&d).is_empty());
        assert!(d.is_dir());
        let _ = fs::remove_dir_all(&d);
    }
}
