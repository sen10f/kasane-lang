//! `ksn check` の増分化 (第 1 段、設計_増分検査.md)。
//!
//! 保存するのは「ソースファイル 1 本の文面だけで決まる検査」の結果 (正規形の判定・lint・文法)。
//! 意味の検査 (型・効果・借用・再帰) は他の定義に依存するので毎回やり直す。
//! キーは ksn の識別子 (ビルド時に `build.rs` が実行体へ埋め込むものすべてのハッシュ。実行時に実行体を読まない)・
//! ファイルのパス・ソースの文面。キーが 1 つでも違えば使わない。
//! 破損・不一致は黙って捨てて再計算する (読めなかった = 未命中)。
//!
//! 置き場 `<KSN_CACHE_DIR>/check/` の上限は 2 つ: 使用量 (`KSN_CHECK_CACHE_MAX`、既定 64 MiB) と
//! 件数 (`KSN_CHECK_CACHE_ENTRIES`、既定 8192)。使用量は**ディスクのブロック単位** (各ファイルを 4 KiB に
//! 切り上げた合計) で数える。1 ファイルが数百バイトでも 4 KiB を占めるため、中身のバイト数では実際の量を過小に数える。
//!
//! 掃除の頻度: 置き場の大きさと件数の見積もりを `.stat.json` (記録) に持つ。書き込みのたびに増分を
//! 記録へ足し (プロセス内では `PEND_*` にためて `trim` でまとめて書く)、全体を数え直すのは次のときだけ:
//! 記録が無い・壊れている、見積もりが上限を超えた、最後に数えてから 1 時間たった。
//! 数え直したときに上限を超えていれば、古い実行体のエントリ (スロット名の接頭辞が今の実行体と違うもの) を
//! 先に、次に更新時刻の古いものから消す。消すときは上限の 8 分の 1 下まで減らす (毎回の実行で消さないため)。
//! 命中のたびに更新時刻を進めるので LRU になる。
//!
//! 複数の `ksn` が同じ置き場を同時に使っても壊れない: 書き込みは一時ファイルから `rename` で置き換え、
//! 読む側は中途半端なファイルを見ない。消えたファイルは未命中になるだけ。記録は見積もりなので、
//! 同時の書き込みで多少ずれても、1 時間ごとの数え直しで直る (結果には影響しない)。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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

/// 置き場の既定の上限 (バイト、ブロック単位)。1 ファイルは数 KB なので、64 MiB で 1 万数千ファイル分になる。
/// ビルドキャッシュの `KSN_CACHE_MAX` (既定 2 GiB) とは別の予算にする。
const DEFAULT_MAX: u64 = 64 * 1024 * 1024;

/// 置き場の既定の上限 (件数)。
const DEFAULT_MAX_ENTRIES: u64 = 8192;

/// ディスクの割り当て単位。各ファイルの大きさはこの倍数に切り上げて数える。
const BLOCK: u64 = 4096;

/// 全体を数え直す間隔 (秒)。記録の見積もりがずれても、これで必ず直る。
const SCAN_EVERY_SECS: u64 = 3600;

/// 記録のファイル名 (`.` で始まるので、スロットとしては数えない)。
const STAT_FILE: &str = ".stat.json";

/// 置き場の上限 (バイト、ブロック単位)。`KSN_CHECK_CACHE_MAX` で変える。
pub fn max_bytes() -> u64 {
    std::env::var("KSN_CHECK_CACHE_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_MAX)
}

/// 置き場の上限 (件数)。`KSN_CHECK_CACHE_ENTRIES` で変える。
pub fn max_entries() -> u64 {
    std::env::var("KSN_CHECK_CACHE_ENTRIES").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_MAX_ENTRIES)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

/// ksn 実行体の識別子 (64 桁の 16 進)。ビルド時に `build.rs` が、実行体へ埋め込むもの全部のハッシュとして作る。
/// 実行時に実行体を読んで hash すると 15 MB で約 50 ms かかり、`ksn check` の固定費になるので使わない
/// (設計_増分検査.md 9 章)。ソースや版・ツールチェーン・ビルド設定が 1 つでも変われば値が変わる。
fn exe_key() -> &'static str {
    env!("KSN_BUILD_ID")
}

/// 実行体の識別子の先頭 16 桁。スロット名の接頭辞になり、掃除が「今の実行体のものか」を名前で判定する。
fn exe_prefix() -> &'static str {
    &exe_key()[..16]
}

fn key(path: &str, rel: &str, text: &str) -> String {
    key_with(exe_key(), path, rel, text)
}

/// 鍵 (実行体の識別子を引数にする。テストで識別子を変えて確かめるため分けてある)。
fn key_with(exe: &str, path: &str, rel: &str, text: &str) -> String {
    let mut h = Sha256::new();
    for part in [exe, path, rel, text] {
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
/// 名前は `<実行体の接頭辞>-<パスの hash>.json`。総量は `trim` が上限に収める。
fn slot(path: &str, rel: &str) -> PathBuf {
    let mut h = Sha256::new();
    h.update(path.as_bytes());
    h.update([0u8]);
    h.update(rel.as_bytes());
    dir().join(format!("{}-{}.json", exe_prefix(), &hex(&h.finalize())[..32]))
}

/// ディスク上の大きさ (ブロックに切り上げた量)。
fn disk_size(len: u64) -> u64 {
    len.div_ceil(BLOCK) * BLOCK
}

/// 置き場の見積もりから記録へ足す増分。`store` / `load` がプロセス内でためて、`trim` が書く。
static PEND_BYTES: AtomicI64 = AtomicI64::new(0);
static PEND_ENTRIES: AtomicI64 = AtomicI64::new(0);

fn add(a: u64, d: i64) -> u64 {
    if d >= 0 {
        a.saturating_add(d as u64)
    } else {
        a.saturating_sub(d.unsigned_abs())
    }
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
            let old = fs::metadata(&p).ok().filter(|m| m.is_file()).map(|m| disk_size(m.len()));
            if fs::remove_file(&p).is_ok() {
                if let Some(sz) = old {
                    PEND_BYTES.fetch_sub(sz as i64, Ordering::Relaxed);
                    PEND_ENTRIES.fetch_sub(1, Ordering::Relaxed);
                }
            }
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
    let body = serde_json::to_string(&v).unwrap_or_default();
    // 上書きなら古い大きさを引く (見積もりを実際に近く保つ)。
    let old = fs::metadata(&p).ok().filter(|m| m.is_file()).map(|m| disk_size(m.len()));
    // 一時ファイルは同じ置き場に作り、rename で置き換える (同時に書いても読む側は完全なものだけを見る)。
    let tmp = p.with_extension(format!("tmp{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    let ok = fs::write(&tmp, &body).is_ok() && fs::rename(&tmp, &p).is_ok();
    if !ok {
        let _ = fs::remove_file(&tmp);
        return;
    }
    PEND_BYTES.fetch_add(disk_size(body.len() as u64) as i64 - old.unwrap_or(0) as i64, Ordering::Relaxed);
    if old.is_none() {
        PEND_ENTRIES.fetch_add(1, Ordering::Relaxed);
    }
}

/// 置き場の上限 (使用量と件数)。
#[derive(Clone, Copy, Debug)]
struct Limits {
    bytes: u64,
    entries: u64,
}

/// 記録 (`.stat.json`)。見積もりの大きさ・件数と、全体を最後に数えた時刻 (秒)。
#[derive(Clone, Copy, Debug, PartialEq)]
struct Stat {
    bytes: u64,
    entries: u64,
    scanned: u64,
}

fn read_stat(dir: &Path) -> Option<Stat> {
    let raw = fs::read_to_string(dir.join(STAT_FILE)).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    Some(Stat { bytes: v.get("bytes")?.as_u64()?, entries: v.get("entries")?.as_u64()?, scanned: v.get("scanned")?.as_u64()? })
}

/// 記録は一時ファイルから `rename` で置く。失敗しても次の数え直しで直るので、結果は変えない。
fn write_stat(dir: &Path, s: Stat) {
    let v = json!({ "bytes": s.bytes, "entries": s.entries, "scanned": s.scanned });
    let tmp = dir.join(format!(".stat.tmp{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    let ok = fs::write(&tmp, v.to_string()).is_ok() && fs::rename(&tmp, dir.join(STAT_FILE)).is_ok();
    if !ok {
        let _ = fs::remove_file(&tmp);
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 置き場の 1 ファイル (スロットと一時ファイル。記録は含まない)。
struct Ent {
    mtime: SystemTime,
    path: PathBuf,
    size: u64,
    /// 今の実行体のものでない (名前の接頭辞が `cur` と違う、または接頭辞が無い)。
    stale: bool,
}

/// `dir` の全ファイルを数える (記録と `.` で始まるものは除く)。ここが置き場の大きさに比例する費用。
fn scan(dir: &Path, cur: &str) -> Vec<Ent> {
    let Ok(rd) = fs::read_dir(dir) else { return vec![] };
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                return None;
            }
            let m = e.metadata().ok()?;
            if !m.is_file() {
                return None;
            }
            let stale = name.split_once('-').map_or(true, |(p, _)| p != cur);
            Some(Ent { mtime: m.modified().unwrap_or(UNIX_EPOCH), path: e.path(), size: disk_size(m.len()), stale })
        })
        .collect()
}

/// 置き場を上限以内に保つ。`ksn check` の終わりに 1 回呼ぶ。
/// プロセス内でためた増分 (`store` / `load` の分) を記録へ足し、全体を数え直すかを決める。数え直したら `true`。
pub fn trim() -> bool {
    let pend = (PEND_BYTES.swap(0, Ordering::Relaxed), PEND_ENTRIES.swap(0, Ordering::Relaxed));
    trim_in(&dir(), Limits { bytes: max_bytes(), entries: max_entries() }, exe_prefix(), now_secs(), pend)
}

/// `trim` の本体 (置き場・上限・今の実行体の接頭辞・現在時刻を引数にして、テストできる形にする)。
fn trim_in(dir: &Path, lim: Limits, cur: &str, now: u64, pend: (i64, i64)) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let (pb, pe) = pend;
    let rec = read_stat(dir);
    let due = match rec {
        None => true,
        Some(s) => {
            now < s.scanned
                || now - s.scanned >= SCAN_EVERY_SECS
                || add(s.bytes, pb) > lim.bytes
                || add(s.entries, pe) > lim.entries
        }
    };
    if !due {
        // 数え直さない。増分だけ記録に足す (増分がなければ書かない)。
        if let (Some(s), true) = (rec, pb != 0 || pe != 0) {
            write_stat(dir, Stat { bytes: add(s.bytes, pb), entries: add(s.entries, pe), scanned: s.scanned });
        }
        return false;
    }
    let mut ents = scan(dir, cur);
    let mut bytes: u64 = ents.iter().map(|e| e.size).sum();
    let mut n = ents.len() as u64;
    if bytes > lim.bytes || n > lim.entries {
        // 消すときは上限の 8 分の 1 下まで減らす (次の実行でまた超えて数え直すのを避ける)。
        let (tb, tn) = (lim.bytes - lim.bytes / 8, lim.entries - lim.entries / 8);
        // 今の実行体でないもの (先に消す)、次に更新時刻の古いもの。
        ents.sort_by(|a, b| (!a.stale, a.mtime, &a.path).cmp(&(!b.stale, b.mtime, &b.path)));
        for e in ents {
            if bytes <= tb && n <= tn {
                break;
            }
            match fs::remove_file(&e.path) {
                Ok(()) => {
                    bytes -= e.size;
                    n -= 1;
                }
                // 他のプロセスが先に消した。合計からは引く。
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    bytes -= e.size;
                    n -= 1;
                }
                // 消せなかったもの (権限など) は合計から引かない。
                Err(_) => {}
            }
        }
    }
    write_stat(dir, Stat { bytes, entries: n, scanned: now });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const LIM: Limits = Limits { bytes: 3 * BLOCK, entries: 100 };

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ksn_cc_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// `name` を 100 バイトで作り、更新時刻を `base + secs` にする (1 ファイル 4 KiB のブロック)。
    fn mk(d: &Path, name: &str, secs: u64) {
        let p = d.join(name);
        fs::write(&p, vec![b'x'; 100]).unwrap();
        let base = UNIX_EPOCH + Duration::from_secs(1_000_000);
        fs::OpenOptions::new().write(true).open(&p).unwrap().set_modified(base + Duration::from_secs(secs)).unwrap();
    }

    fn names(d: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).filter(|n| !n.starts_with('.')).collect();
        v.sort();
        v
    }

    /// 実行体の識別子が変われば鍵が変わる (古い実行体のエントリは当たらない)。
    /// 鍵の残りの部分 (パス・相対パス・文面) が同じでも、識別子だけで別の鍵になることを確かめる。
    #[test]
    fn key_changes_with_build_identity() {
        let a = key_with("aa", "/p/x.ksnl", "src/x.ksnl", "fn f() end f\n");
        assert_eq!(a, key_with("aa", "/p/x.ksnl", "src/x.ksnl", "fn f() end f\n"));
        assert_ne!(a, key_with("ab", "/p/x.ksnl", "src/x.ksnl", "fn f() end f\n"));
        // 連結のずれで同じ鍵にならないこと (長さの前置)。
        assert_ne!(key_with("a", "bc", "", ""), key_with("ab", "c", "", ""));
    }

    /// 識別子は 64 桁の 16 進で、スロット名の接頭辞は先頭 16 桁。
    #[test]
    fn build_id_is_a_hex_fingerprint() {
        let id = exe_key();
        assert_eq!(id.len(), 64);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(exe_prefix(), &id[..16]);
    }

    #[test]
    fn disk_size_rounds_up_to_blocks() {
        assert_eq!(disk_size(0), 0);
        assert_eq!(disk_size(1), BLOCK);
        assert_eq!(disk_size(BLOCK), BLOCK);
        assert_eq!(disk_size(BLOCK + 1), 2 * BLOCK);
    }

    #[test]
    fn trim_removes_oldest_first_until_under_limit() {
        let d = tmpdir("oldest");
        for i in 0..5u64 {
            mk(&d, &format!("cur-e{}.json", i), i * 10);
        }
        // 5 本 × 4 KiB = 20 KiB > 上限 12 KiB。下限は 12 KiB - 12 KiB/8 = 10.5 KiB なので、古い 3 本を消して 2 本残す。
        assert!(trim_in(&d, LIM, "cur", 2_000_000_000, (0, 0)));
        assert_eq!(names(&d), vec!["cur-e3.json", "cur-e4.json"]);
        // 上限 0 なら全部消えるが、置き場のディレクトリは残る。
        trim_in(&d, Limits { bytes: 0, entries: 0 }, "cur", 2_000_000_000, (0, 0));
        assert!(names(&d).is_empty());
        assert!(d.is_dir());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn trim_removes_other_executables_entries_first() {
        let d = tmpdir("stale");
        // 古い実行体のもの (`old-...`) は最も新しいが、先に消える。その次に今の実行体の古いものから。
        mk(&d, "cur-a.json", 0);
        mk(&d, "cur-b.json", 10);
        mk(&d, "cur-c.json", 20);
        mk(&d, "old-x.json", 30);
        // 4 本 × 4 KiB = 16 KiB > 12 KiB。先に old-x (4 KiB) を消すと 12 KiB で、下限 10.5 KiB を超えるので cur-a も消す。
        trim_in(&d, LIM, "cur", 2_000_000_000, (0, 0));
        assert_eq!(names(&d), vec!["cur-b.json", "cur-c.json"]);
        // 接頭辞の無い古い形式のものも古い実行体扱い。
        let d2 = tmpdir("stale_noprefix");
        mk(&d2, "0123abcd.json", 99);
        mk(&d2, "cur-a.json", 0);
        mk(&d2, "cur-b.json", 1);
        mk(&d2, "cur-c.json", 2);
        trim_in(&d2, LIM, "cur", 2_000_000_000, (0, 0));
        assert_eq!(names(&d2), vec!["cur-b.json", "cur-c.json"]);
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&d2);
    }

    #[test]
    fn trim_counts_only_when_due() {
        let d = tmpdir("due");
        mk(&d, "cur-a.json", 0);
        // 記録が無い: 数える。
        assert!(trim_in(&d, LIM, "cur", 1000, (0, 0)));
        let s = read_stat(&d).unwrap();
        assert_eq!((s.bytes, s.entries, s.scanned), (BLOCK, 1, 1000));
        // 記録が新しく、上限も超えない: 数えない。増分だけ足す。
        assert!(!trim_in(&d, LIM, "cur", 1500, (BLOCK as i64, 1)));
        let s = read_stat(&d).unwrap();
        assert_eq!((s.bytes, s.entries, s.scanned), (2 * BLOCK, 2, 1000));
        // 増分がなければ記録も書かない (時刻を見るだけ)。
        assert!(!trim_in(&d, LIM, "cur", 1600, (0, 0)));
        assert_eq!(read_stat(&d).unwrap().scanned, 1000);
        // 見積もりが上限を超えたら数える。
        assert!(trim_in(&d, LIM, "cur", 1700, (10 * BLOCK as i64, 0)));
        // 1 時間たったら数える (見積もりがずれていても直る)。
        assert!(trim_in(&d, LIM, "cur", 1700 + SCAN_EVERY_SECS, (0, 0)));
        // 記録が壊れていたら作り直す。
        fs::write(d.join(STAT_FILE), "garbage").unwrap();
        assert!(trim_in(&d, LIM, "cur", 5000, (0, 0)));
        assert_eq!(read_stat(&d).unwrap().entries, 1);
        // 時計が戻ったら (記録の時刻より前) も数える。
        assert!(trim_in(&d, LIM, "cur", 100, (0, 0)));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn trim_on_missing_dir_is_a_no_op() {
        let d = std::env::temp_dir().join(format!("ksn_cc_{}_missing", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        assert!(!trim_in(&d, LIM, "cur", 1, (5, 5)));
        assert!(!d.exists());
    }

    /// 66,457 件 (作業の計測の規模) の置き場で、数え直しの費用と、記録を見るだけの費用を比べる。
    /// 通常の実行には入れない: `cargo test --release --lib check_cache::tests::bench_ -- --ignored --nocapture`。
    #[test]
    #[ignore]
    fn bench_trim_66k_entries() {
        let d = tmpdir("bench66k");
        for i in 0..66_457u64 {
            fs::write(d.join(format!("oldexe0123456789-{:032x}.json", i)), vec![b'x'; 400]).unwrap();
        }
        let t = std::time::Instant::now();
        let scanned = scan(&d, "cur").len();
        let full = t.elapsed();
        let t = std::time::Instant::now();
        trim_in(&d, Limits { bytes: 1 << 40, entries: 1 << 40 }, "cur", 1000, (0, 0));
        let first = t.elapsed();
        let t = std::time::Instant::now();
        let again = trim_in(&d, Limits { bytes: 1 << 40, entries: 1 << 40 }, "cur", 1001, (0, 0));
        let fast = t.elapsed();
        eprintln!("entries={} full_count={:?} trim_first={:?} trim_fast_path={:?} rescanned={}", scanned, full, first, fast, again);
        let _ = fs::remove_dir_all(&d);
    }
}
