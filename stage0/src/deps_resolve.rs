//! 依存グラフ全体の版の解決 (R2a)。
//!
//! 版 (`git` + `version`) の依存は、同じ名前を依存するすべての親の範囲の積を取り、
//! その範囲に合う最大の `vMAJOR.MINOR.PATCH` タグを選ぶ。積が空なら両方の要求元と範囲を名指しで止める。
//! ロックが manifest の要求をすべて満たすなら、ロックのコミットをそのまま使い、ネットワークに出ない。
//! 選び直しは `ksn update` (`GitMode::Fresh`) の対象と、ロックが満たさなくなった依存だけ。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::project::{load_package_src, locate_dep, GitMode, Package, ProjectError};
use crate::semver::{Range, Version};

/// 依存 1 つの解決結果。`commit` は取得するコミット、`version` は選んだタグの版。
/// レジストリ依存のとき `repository` は取得元のリポジトリ URL、`hash` は索引の分配 hash。
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    pub url: String,
    pub commit: String,
    pub version: Version,
    pub repository: Option<String>,
    pub hash: Option<String>,
}

pub type Picks = BTreeMap<String, Pick>;

/// 親 (`requirer`) が書いた版の範囲。`registry` はレジストリ依存のときその索引 URL。
struct Req {
    requirer: String,
    range: String,
    url: String,
    registry: Option<String>,
}

/// ロック (版 3) に記録された依存。`version` は版 2 には無い。
/// `repository`/`hash` はレジストリ依存だけ記録される。
struct Locked {
    url: String,
    commit: String,
    version: Option<Version>,
    repository: Option<String>,
    hash: Option<String>,
}

fn err(code: &str, msg: String) -> ProjectError {
    ProjectError { code: code.into(), msg }
}

fn read_lock(root: &Path) -> HashMap<String, Locked> {
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(root.join("kasane.lock")) else { return out };
    let Ok(v) = text.parse::<toml::Value>() else { return out };
    let Some(pk) = v.get("package").and_then(|p| p.as_array()) else { return out };
    for e in pk {
        let (Some(name), Some(src)) = (e.get("name").and_then(|n| n.as_str()), e.get("source").and_then(|s| s.as_str())) else { continue };
        let Some((url, commit)) = src.strip_prefix("git+").and_then(|s| s.rsplit_once('#'))
            .or_else(|| src.strip_prefix("registry+").and_then(|s| s.rsplit_once('#'))) else { continue };
        if commit.is_empty() {
            continue;
        }
        let version = e.get("version").and_then(|x| x.as_str()).and_then(|x| Version::parse(x).ok());
        let repository = e.get("repository").and_then(|x| x.as_str()).map(String::from);
        let hash = e.get("hash").and_then(|x| x.as_str()).map(String::from);
        out.insert(name.to_string(), Locked { url: url.to_string(), commit: commit.to_string(), version, repository, hash });
    }
    out
}

fn satisfies(v: &Version, range: &str) -> bool {
    Range::parse(range).map(|r| r.matches(v)).unwrap_or(false)
}

/// 依存グラフを歩いて、版の依存の要求を集める。`picks` が決まっている版はその先も歩く。
fn walk(
    root: &Package,
    pkg: &Package,
    mode: &dyn Fn(&str) -> GitMode,
    picks: &Picks,
    reqs: &mut BTreeMap<String, Vec<Req>>,
    seen: &mut HashSet<String>,
) -> Result<(), ProjectError> {
    let Some(deps) = pkg.manifest.get("dependencies").and_then(|d| d.as_table()) else { return Ok(()) };
    for (dn, dv) in deps {
        if *dn == root.name {
            return Err(err("E9015", format!("dependency cycle: {} depends back on the root package {}", pkg.name, root.name)));
        }
        if let (Some(range), Some(url), None) = (
            dv.get("version").and_then(|v| v.as_str()),
            dv.get("git").and_then(|g| g.as_str()),
            dv.get("rev"),
        ) {
            reqs.entry(dn.clone()).or_default().push(Req { requirer: pkg.name.clone(), range: range.to_string(), url: url.to_string(), registry: None });
            if !picks.contains_key(dn) {
                continue;
            }
        }
        if let (Some(range), Some(index_url), None) = (
            dv.get("version").and_then(|v| v.as_str()),
            dv.get("registry").and_then(|r| r.as_str()),
            dv.get("rev"),
        ) {
            reqs.entry(dn.clone()).or_default().push(Req { requirer: pkg.name.clone(), range: range.to_string(), url: index_url.to_string(), registry: Some(index_url.to_string()) });
            if !picks.contains_key(dn) {
                continue;
            }
        }
        let (source, loc) = locate_dep(root, pkg, dn, dv, mode, picks)?;
        if !seen.insert(format!("{}\n{}", dn, source)) {
            continue;
        }
        let mut dp = load_package_src(&loc)?;
        if dp.name != *dn {
            return Err(err("E9011", format!("dependency {}: package name is {}", dn, dp.name)));
        }
        dp.source = source;
        walk(root, &dp, mode, picks, reqs, seen)?;
    }
    Ok(())
}

/// 依存グラフ全体の版を決める。ロックの記録が要求をすべて満たす依存はそのまま使う (ネットワークに出ない)。
pub fn resolve_picks(root: &Package, mode: &dyn Fn(&str) -> GitMode) -> Result<Picks, ProjectError> {
    let locked = read_lock(&root.root);
    let mut picks = Picks::new();
    for (n, l) in &locked {
        if mode(n) == GitMode::Fresh {
            continue;
        }
        if let Some(v) = l.version {
            picks.insert(n.clone(), Pick { url: l.url.clone(), commit: l.commit.clone(), version: v, repository: l.repository.clone(), hash: l.hash.clone() });
        }
    }
    let mut fresh_done: HashSet<String> = HashSet::new();
    // 選び直しの連鎖 (新しい版が新しい要求を持ち込む) が収束するまで回す。
    for _ in 0..64 {
        let mut reqs: BTreeMap<String, Vec<Req>> = BTreeMap::new();
        let mut seen = HashSet::new();
        walk(root, root, mode, &picks, &mut reqs, &mut seen)?;
        let mut changed = false;
        for (name, rs) in &reqs {
            let url = &rs[0].url;
            if let Some(r) = rs.iter().find(|r| r.url != *url) {
                return Err(err("E9014", format!("package {} is required from two different sources: {} and {}", name, url, r.url)));
            }
            let m = mode(name);
            let keep = match picks.get(name) {
                Some(p) => p.url == *url && rs.iter().all(|r| satisfies(&p.version, &r.range)) && (m != GitMode::Fresh || fresh_done.contains(name)),
                None => false,
            };
            if keep {
                continue;
            }
            let describe = rs.iter().map(|r| format!("{} requires \"{}\"", r.requirer, r.range)).collect::<Vec<_>>().join(", ");
            if m == GitMode::Offline {
                return Err(err("E9010", format!("git dependency {} is not satisfied by kasane.lock ({}); run `ksn update`", name, describe)));
            }
            // 範囲の積が空なら、どの二人の要求が衝突するかを名指しする。
            let parsed: Vec<Range> = rs
                .iter()
                .map(|r| Range::parse(&r.range).map_err(|e| err("E9008", format!("dependency {}: {}", name, e))))
                .collect::<Result<_, _>>()?;
            let mut acc: Option<Range> = None;
            for (i, rr) in parsed.iter().enumerate() {
                acc = match acc {
                    None => Some(Range::parse(&rs[0].range).map_err(|e| err("E9008", e))?),
                    Some(a) => a.intersection(rr),
                };
                if acc.is_none() {
                    // 衝突する二人を名指しする: 手前の要求の中で、この範囲と積が空になるもの (無ければ先頭)。
                    let r = &rs[i];
                    let q = rs[..i].iter().find(|q| Range::parse(&q.range).ok().and_then(|qr| qr.intersection(&parsed[i])).is_none()).unwrap_or(&rs[0]);
                    return Err(err(
                        "E9023",
                        format!("version conflict on {}: {} requires \"{}\" and {} requires \"{}\"; no version satisfies both", name, q.requirer, q.range, r.requirer, r.range),
                    ));
                }
            }
            let is_registry = rs[0].registry.is_some();
            if is_registry {
                let index_url = url;
                let offline = m == GitMode::Offline;
                let pinned = locked.get(name).and_then(|l| l.version.as_ref().map(|v| v.to_string()));
                let pinned = pinned.as_deref();
                let entries = crate::registry::index_versions(index_url, name, offline, pinned)
                    .map_err(|e| err("E9010", e.msg))?;
                let mut best: Option<(Version, &crate::registry::IndexEntry)> = None;
                for e in &entries {
                    let Ok(v) = Version::parse(&e.version) else { continue };
                    if !rs.iter().all(|r| satisfies(&v, &r.range)) { continue; }
                    best = match best {
                        None => Some((v, e)),
                        Some((bv, _)) => if v > bv { Some((v, e)) } else { best },
                    };
                }
                let Some((v, e)) = best else {
                    return Err(err("E9010", format!("dependency {}: no version of {} in registry {} satisfies {}", name, name, index_url, describe)));
                };
                picks.insert(name.clone(), Pick {
                    url: index_url.clone(),
                    commit: e.commit.clone(),
                    version: v,
                    repository: Some(e.repository.clone()),
                    hash: Some(e.hash.clone()),
                });
            } else {
                let tags = crate::git_tags::list_git_tags(url).map_err(|e| err("E9010", e.msg))?;
                let best = tags
                    .iter()
                    .filter(|(v, _)| {
                        let vv = Version::new(v.major, v.minor, v.patch);
                        rs.iter().all(|r| satisfies(&vv, &r.range))
                    })
                    .max_by_key(|(v, _)| (v.major, v.minor, v.patch));
                let Some((v, c)) = best else {
                    return Err(err("E9010", format!("dependency {}: no tag of {} satisfies {}", name, url, describe)));
                };
                picks.insert(name.clone(), Pick { url: url.clone(), commit: c.clone(), version: Version::new(v.major, v.minor, v.patch), repository: None, hash: None });
            }
            if m == GitMode::Fresh {
                fresh_done.insert(name.clone());
            }
            changed = true;
        }
        if !changed {
            return Ok(picks);
        }
    }
    Err(err("E9010", "dependency resolution did not converge".into()))
}
