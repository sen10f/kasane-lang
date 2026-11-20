//! 最小レジストリの索引 (R3a)。索引は Git リポジトリで、`index/<name>.json` に版の昇順の配列を置く
//! (`設計_依存の版とレジストリ.md` 5.1)。取得は `<KSN_CACHE_DIR>/registry/<URL のハッシュ>` の bare クローン。
//! ここは索引の読みと cache だけを担い、解決・lock・build にはまだ接続しない (R2a の resolver が呼ぶ)。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::git_tags::SemVer;
use crate::project::{cache_dir, git_run};

static PUBLISH_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `https://` / `ssh://` 以外（ローカルパス）はネットワークを使わないレジストリとして扱い、
/// コミットまでそのまま反映する。URL のレジストリはコミットだけを作り、push は呼び出し側に任せる（ショートカット）。
pub(crate) fn is_url_registry(registry: &str) -> bool {
    registry.starts_with("https://") || registry.starts_with("ssh://")
}

/// レジストリの bare クローンを一時の非ベア作業コピーに展開する（パスを返す）。
fn publish_worktree(registry: &str) -> Result<PathBuf, RegistryError> {
    let tmp = std::env::temp_dir().join(format!(
        "ksn-publish-{}-{}",
        std::process::id(),
        PUBLISH_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&tmp);
    let tmp_s = tmp.to_string_lossy().to_string();
    git_run(&["clone", "--quiet", registry, &tmp_s])
        .map_err(|m| err(format!("cannot clone registry {registry}: {m}")))?;
    Ok(tmp)
}

/// `-C <dir>` を付けて git を実行する。
fn git_in(dir: &Path, args: &[&str]) -> Result<String, RegistryError> {
    let dir_s = dir.to_string_lossy().to_string();
    let mut all: Vec<String> = vec!["-C".to_string(), dir_s];
    all.extend(args.iter().map(|s| s.to_string()));
    let all: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
    git_run(&all).map_err(|m| err(format!("git failed: {m}")))
}

/// `index/<name>.json` を読む（ファイルが無ければ空の配列）。
fn read_index_file(worktree: &Path, name: &str) -> Result<Vec<IndexEntry>, RegistryError> {
    let p = worktree.join("index").join(format!("{name}.json"));
    let text = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(err(format!("cannot read index/{name}.json: {e}"))),
    };
    if text.trim().is_empty() {
        return Ok(vec![]);
    }
    parse_index(&text, name)
}

/// IndexEntry を索引スキーマ（設計 5.1）の 7 キーだけで JSON に戻す（キー順固定）。
fn entry_json(e: &IndexEntry) -> String {
    format!(
        "{{\"version\":\"{}\",\"tag\":\"{}\",\"repository\":\"{}\",\"commit\":\"{}\",\"hash\":\"{}\",\"owner\":\"{}\",\"yanked\":{}}}",
        e.version, e.tag, e.repository, e.commit, e.hash, e.owner, e.yanked
    )
}

/// `index/<name>.json` の全版を yank の有無にかかわらず昇順で返す。
pub(crate) fn index_all_entries(registry: &str, name: &str) -> Result<Vec<IndexEntry>, RegistryError> {
    let wt = publish_worktree(registry)?;
    let all = read_index_file(&wt, name)?;
    let _ = fs::remove_dir_all(&wt);
    Ok(all)
}

/// 1 版を索引に追加し、1 コミットしてコミットハッシュを返す。
/// 同一版の既存・非昇順は拒否する。URL レジストリは push しない（commit ハッシュだけ返す；shortcut）。
pub(crate) fn publish_entry(
    registry: &str,
    name: &str,
    entry: &IndexEntry,
) -> Result<String, RegistryError> {
    let wt = publish_worktree(registry)?;
    let mut all = read_index_file(&wt, name)?;
    if all.iter().any(|e| e.version == entry.version) {
        return Err(err(format!(
            "version {} is already published for package {name}",
            entry.version
        )));
    }
    all.push(entry.clone());
    all.sort_by(|a, b| {
        SemVer::parse(&a.version)
            .cmp(&SemVer::parse(&b.version))
    });
    let text = format!("[{}]", all.iter().map(entry_json).collect::<Vec<_>>().join(","));
    fs::create_dir_all(wt.join("index")).map_err(|e| err(format!("cannot create index dir: {e}")))?;
    fs::write(wt.join("index").join(format!("{name}.json")), &text)
        .map_err(|e| err(format!("cannot write index: {e}")))?;
    git_in(&wt, &["add", "index"])?;
    git_in(
        &wt,
        &["commit", "--quiet", "-m", &format!("publish {name} {}", entry.version)],
    )?;
    let hash = git_in(&wt, &["rev-parse", "HEAD"])?;
    if !is_url_registry(registry) {
        // ローカルレジストリへ反映（ネットワークを使わない push）
        git_in(&wt, &["push", "--quiet", "origin", "HEAD:main"])?;
    }
    let _ = fs::remove_dir_all(&wt);
    Ok(hash)
}

/// `version` の yanked フラグを `yanked` に切り替え、1 コミットしてコミットハッシュを返す。
/// `--undo`（false）との区別は caller が行う。URL レジストリは push しない（shortcut）。
pub(crate) fn yank_entry(
    registry: &str,
    name: &str,
    version: &str,
    yanked: bool,
) -> Result<String, RegistryError> {
    let wt = publish_worktree(registry)?;
    let mut all = read_index_file(&wt, name)?;
    let entry = all
        .iter_mut()
        .find(|e| e.version == version)
        .ok_or_else(|| err(format!("version {version} is not in package {name}")))?;
    if entry.yanked == yanked {
        return Err(err(format!(
            "version {version} of {name} is already {}",
            if yanked { "yanked" } else { "not yanked" }
        )));
    }
    entry.yanked = yanked;
    all.sort_by(|a, b| {
        SemVer::parse(&a.version)
            .cmp(&SemVer::parse(&b.version))
    });
    let text = format!("[{}]", all.iter().map(entry_json).collect::<Vec<_>>().join(","));
    fs::create_dir_all(wt.join("index")).map_err(|e| err(format!("cannot create index dir: {e}")))?;
    fs::write(wt.join("index").join(format!("{name}.json")), &text)
        .map_err(|e| err(format!("cannot write index: {e}")))?;
    git_in(&wt, &["add", "index"])?;
    let msg = if yanked {
        format!("yank {name} {version}")
    } else {
        format!("unyank {name} {version}")
    };
    git_in(&wt, &["commit", "--quiet", "-m", &msg])?;
    let hash = git_in(&wt, &["rev-parse", "HEAD"])?;
    if !is_url_registry(registry) {
        git_in(&wt, &["push", "--quiet", "origin", "HEAD:main"])?;
    }
    let _ = fs::remove_dir_all(&wt);
    Ok(hash)
}

/// 索引の 1 版 (`index/<name>.json` の 1 要素)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexEntry {
    pub(crate) version: String,
    pub(crate) tag: String,
    pub(crate) repository: String,
    pub(crate) commit: String,
    pub(crate) hash: String,
    pub(crate) owner: String,
    pub(crate) yanked: bool,
}

#[derive(Debug)]
pub(crate) struct RegistryError {
    /// 診断コード。登録の取得・索引の検証は既存の E9040 (レジストリ依存は未対応) を使う。
    pub(crate) code: &'static str,
    pub(crate) msg: String,
}

fn err(msg: String) -> RegistryError {
    RegistryError { code: "E9040", msg }
}

const ENTRY_KEYS: [&str; 7] = ["version", "tag", "repository", "commit", "hash", "owner", "yanked"];

/// パッケージ名は索引のファイル名になるので、パスの区切りや `..` を通さない。
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
        && name.as_bytes()[0].is_ascii_lowercase()
}

fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// 索引の 1 版を strict に読む。未知のキー・欠けたキー・型の誤りは拒否する。
fn parse_entry(v: &Value, at: &str) -> Result<IndexEntry, RegistryError> {
    let obj = v.as_object().ok_or_else(|| err(format!("{at}: object expected")))?;
    for k in obj.keys() {
        if !ENTRY_KEYS.contains(&k.as_str()) {
            return Err(err(format!("{at}: unknown key {k}")));
        }
    }
    let s = |k: &str| -> Result<String, RegistryError> {
        obj.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| err(format!("{at}: {k} must be a string")))
    };
    let version = s("version")?;
    if SemVer::parse(&version).is_none() {
        return Err(err(format!("{at}: version {version} is not MAJOR.MINOR.PATCH")));
    }
    let tag = s("tag")?;
    if tag != format!("v{version}") {
        return Err(err(format!("{at}: tag {tag} does not match version {version}")));
    }
    let repository = s("repository")?;
    let commit = s("commit")?;
    if !is_hex(&commit, 40) {
        return Err(err(format!("{at}: commit must be 40 lowercase hex digits")));
    }
    let hash = s("hash")?;
    match hash.strip_prefix("sha256:") {
        Some(h) if is_hex(h, 64) => {}
        _ => return Err(err(format!("{at}: hash must be sha256:<64 lowercase hex>"))),
    }
    let owner = s("owner")?;
    let yanked = obj.get("yanked").and_then(Value::as_bool).ok_or_else(|| err(format!("{at}: yanked must be a bool")))?;
    Ok(IndexEntry { version, tag, repository, commit, hash, owner, yanked })
}

/// 索引ファイル (`index/<name>.json`) の本文を読む。版は昇順・重複なしを要求する。
pub(crate) fn parse_index(text: &str, name: &str) -> Result<Vec<IndexEntry>, RegistryError> {
    let v: Value = serde_json::from_str(text).map_err(|e| err(format!("index/{name}.json: {e}")))?;
    let arr = v.as_array().ok_or_else(|| err(format!("index/{name}.json: top level must be an array")))?;
    let mut out: Vec<IndexEntry> = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        let e = parse_entry(item, &format!("index/{name}.json[{i}]"))?;
        if let Some(prev) = out.last() {
            if SemVer::parse(&prev.version) >= SemVer::parse(&e.version) {
                return Err(err(format!("index/{name}.json[{i}]: versions must be strictly ascending")));
            }
        }
        out.push(e);
    }
    Ok(out)
}

/// 索引の cache の置き場 (`<KSN_CACHE_DIR>/registry/<URL のハッシュ>`)。
pub(crate) fn registry_cache_path(registry: &str) -> PathBuf {
    let hex = hex_of(&Sha256::digest(registry.as_bytes()));
    cache_dir().join("registry").join(&hex[..32])
}

/// 索引の bare クローンを用意する。`offline` なら既存の cache だけを使い、無ければ明確なエラーにする。
/// オンラインで cache があれば fetch して最新の HEAD を読む。
pub(crate) fn sync_registry(registry: &str, offline: bool) -> Result<PathBuf, RegistryError> {
    let dir = registry_cache_path(registry);
    let dir_s = dir.to_string_lossy().to_string();
    let exists = dir.join("HEAD").is_file();
    if offline {
        if exists {
            return Ok(dir);
        }
        return Err(err(format!("registry {registry} is not cached; run once online or drop --offline")));
    }
    if exists {
        git_run(&["-C", &dir_s, "fetch", "--quiet", "origin", "+refs/heads/*:refs/heads/*"])
            .map_err(|m| err(format!("cannot update registry {registry}: {m}")))?;
    } else {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| err(format!("cannot create cache: {e}")))?;
        }
        git_run(&["clone", "--bare", "--quiet", registry, &dir_s])
            .map_err(|m| err(format!("cannot clone registry {registry}: {m}")))?;
    }
    Ok(dir)
}

/// `name` の版の一覧を昇順で返す。yanked は除外する。ただし `pinned` (lock が既に固定している版) は残す。
/// shortcut: 索引の版と取得 commit の `package.version` の照合、分配 hash の再計算は R2a の取得時に行う。
/// shortcut: R2a の resolver はこの関数を呼ぶ。今は build に接続せず、`ksn registry index` だけが使う。
pub(crate) fn index_versions(
    registry: &str,
    name: &str,
    offline: bool,
    pinned: Option<&str>,
) -> Result<Vec<IndexEntry>, RegistryError> {
    if !valid_name(name) {
        return Err(err(format!("invalid registry package name {name:?}")));
    }
    let dir = sync_registry(registry, offline)?;
    let dir_s = dir.to_string_lossy().to_string();
    let spec = format!("HEAD:index/{name}.json");
    let text = match git_run(&["-C", &dir_s, "show", &spec]) {
        Ok(t) => t,
        Err(_) => return Err(err(format!("package {name} is not in registry {registry}"))),
    };
    let all = parse_index(&text, name)?;
    Ok(all.into_iter().filter(|e| !e.yanked || pinned == Some(e.version.as_str())).collect())
}
