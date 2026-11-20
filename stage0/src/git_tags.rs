//! git 依存の `vMAJOR.MINOR.PATCH` タグを `git ls-remote --tags` から列挙する (R1b)。
//! 解決・lock への接続はまだ行わない (R2a で行う)。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::project::git_run;

/// 1 つの semver 版 (`MAJOR.MINOR.PATCH`、十進整数、先頭 0 は "0" のみ)。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SemVer {
    pub(crate) major: u64,
    pub(crate) minor: u64,
    pub(crate) patch: u64,
}

// shortcut: 独自の小さな版型。R1a の semver.rs がマージされたら、そちらの型を
// pub(crate) で再利用してこちらを削除する (パース・比較・表示の意味は同じにする)。

impl SemVer {
    /// `MAJOR.MINOR.PATCH` のみを受け付ける (`v` 前置・先頭 0・pre-release・build metadata は受け付けない)。
    pub(crate) fn parse(s: &str) -> Option<SemVer> {
        let p: Vec<&str> = s.split('.').collect();
        if p.len() != 3 {
            return None;
        }
        let mut v = SemVer { major: 0, minor: 0, patch: 0 };
        for (i, x) in p.iter().enumerate() {
            if x.is_empty() || !x.as_bytes().iter().all(|&c| c.is_ascii_digit()) {
                return None;
            }
            if x.len() > 1 && x.starts_with('0') {
                return None;
            }
            let n: u64 = match x.parse() {
                Ok(n) => n,
                Err(_) => return None,
            };
            match i {
                0 => v.major = n,
                1 => v.minor = n,
                _ => v.patch = n,
            }
        }
        Some(v)
    }
}

impl fmt::Display for SemVer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// `git ls-remote --tags <url>` の出力から、列挙されたタグを `(版, コミット)` で返す。
// annotated tag は `refs/tags/<名前>^{}` の行でコミットへ peel する。軽量タグは
// `refs/tags/<名前>` の行のハッシュがそのままコミット。同じ版のコミットが 2 つ異なれば
// 重複としてエラーにする。出力は版順にソート済み。
pub(crate) fn parse_ls_remote_output(
    output: &str,
) -> Result<Vec<(SemVer, String)>, GitTagsError> {
    let mut direct: BTreeMap<String, String> = BTreeMap::new(); // ref 名 -> ハッシュ (^ {} なし)
    let mut peeled: BTreeMap<String, String> = BTreeMap::new(); // ref 名 (^ {} を除く) -> コミットハッシュ
    for line in output.lines() {
        let Some((hash, rest)) = line.split_once('\t') else { continue };
        if let Some(name) = rest.strip_prefix("refs/tags/") {
            if let Some(base) = name.strip_suffix("^{}") {
                peeled.insert(base.to_string(), hash.to_string());
            } else if let Some(dup) = direct.insert(name.to_string(), hash.to_string()) {
                if dup != hash {
                    return Err(GitTagsError {
                        msg: format!(
                            "重複したタグ: '{}' が 2 つのコミット {} と {} を指している",
                            name, dup, hash
                        ),
                    });
                }
            }
        }
    }
    let mut names: BTreeSet<String> = BTreeSet::new();
    names.extend(direct.keys().cloned());
    names.extend(peeled.keys().cloned());
    let mut tags: BTreeMap<SemVer, String> = BTreeMap::new();
    for name in names {
        let Some(ver_str) = name.strip_prefix('v') else { continue };
        let Some(version) = SemVer::parse(ver_str) else { continue };
        let commit = match peeled.get(&name) {
            Some(c) => c.clone(),
            None => direct.get(&name).cloned().unwrap(), // names の和集合なので direct に必ずある
        };
        match tags.entry(version.clone()) {
            std::collections::btree_map::Entry::Occupied(mut e) => {
                if e.get() != &commit {
                    return Err(GitTagsError {
                        msg: format!(
                            "同じ版 {} に 2 つのコミット {} と {} が該当する",
                            version, e.get(), commit
                        ),
                    });
                }
            }
            std::collections::btree_map::Entry::Vacant(e) => {
                e.insert(commit);
            }
        }
    }
    Ok(tags.into_iter().collect())
}

/// `git ls-remote --tags <url>` から `vMAJOR.MINOR.PATCH` タグを列挙する (R1b)。
pub(crate) fn list_git_tags(url: &str) -> Result<Vec<(SemVer, String)>, GitTagsError> {
    let output = git_run(&["ls-remote", "--tags", url]).map_err(|m| {
        GitTagsError {
            msg: format!("git タグの列挙に失敗しました ({url}): {m}"),
        }
    })?;
    parse_ls_remote_output(&output)
}

#[derive(Debug)]
pub(crate) struct GitTagsError {
    pub(crate) msg: String,
}

impl GitTagsError {
    pub(crate) fn new(msg: impl Into<String>) -> Self {
        Self { msg: msg.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(hash: &str, r: &str) -> String {
        format!("{}\t{}", hash, r)
    }

    #[test]
    fn semver_parse() {
        assert_eq!(
            SemVer::parse("1.2.3"),
            Some(SemVer { major: 1, minor: 2, patch: 3 })
        );
        assert_eq!(SemVer::parse("0.0.0"), Some(SemVer { major: 0, minor: 0, patch: 0 }));
        assert_eq!(SemVer::parse("2.10.0"), Some(SemVer { major: 2, minor: 10, patch: 0 }));
        for bad in [
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "v1.2.3",
            "1.2",
            "1.2.3.4",
            "1.2.3-rc1",
            "1.2.3+build",
            "abc",
            "",
            "1.2. ",
        ] {
            assert!(SemVer::parse(bad).is_none(), "'{bad}' は版として受け付けない");
        }
    }

    #[test]
    fn semver_order() {
        let a = SemVer::parse("1.2.3").unwrap();
        let b = SemVer::parse("1.2.4").unwrap();
        let c = SemVer::parse("1.3.0").unwrap();
        let d = SemVer::parse("2.0.0").unwrap();
        assert!(a < b && b < c && c < d);
    }

    #[test]
    fn lightweight_tag_is_used_directly() {
        let out = format!("{}\n", line("aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111", "refs/tags/v1.0.0"));
        let tags = parse_ls_remote_output(&out).unwrap();
        assert_eq!(
            tags,
            vec![(
                SemVer::parse("1.0.0").unwrap(),
                "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111".to_string()
            )]
        );
    }

    #[test]
    fn annotated_tag_peels_to_commit() {
        let out = format!(
            "{}\n{}\n",
            line("tttt0000tttt0000tttt0000tttt0000tttt0000", "refs/tags/v1.0.0"),
            line("cccc0000cccc0000cccc0000cccc0000cccc0000", "refs/tags/v1.0.0^{}")
        );
        let tags = parse_ls_remote_output(&out).unwrap();
        assert_eq!(
            tags,
            vec![(
                SemVer::parse("1.0.0").unwrap(),
                "cccc0000cccc0000cccc0000cccc0000cccc0000".to_string()
            )]
        );
    }

    #[test]
    fn non_semver_tags_are_ignored() {
        let out = format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
            line("a", "refs/tags/v1.2"),
            line("b", "refs/tags/release-1"),
            line("c", "refs/tags/v01.2.3"),
            line("d", "refs/tags/1.2.3"),
            line("e", "refs/tags/v1.2.3.4"),
            line("f", "refs/tags/v1.2.3-rc1"),
            line("g", "refs/tags/v2.0.0")
        );
        let tags = parse_ls_remote_output(&out).unwrap();
        assert_eq!(
            tags,
            vec![(SemVer::parse("2.0.0").unwrap(), "g".to_string())]
        );
    }

    #[test]
    fn same_tag_direct_and_peeled_is_not_a_duplicate() {
        // annotated tag の 2 行 (tag オブジェクトへの直接行と peel 行) は同じタグとして扱う。
        let out = format!(
            "{}\n{}\n",
            line("tttt0000tttt0000tttt0000tttt0000tttt0000", "refs/tags/v1.0.0"),
            line("cccc0000cccc0000cccc0000cccc0000cccc0000", "refs/tags/v1.0.0^{}")
        );
        assert!(parse_ls_remote_output(&out).is_ok());
    }

    #[test]
    fn duplicate_version_is_rejected() {
        // 実の git レポジトリでは同じ ref 名は 1 つしか作れないが、ls-remote の出力として
        // 同じ版の 2 つのコミットが来たら重複エラーにする (設計 R1b)。
        let out = format!(
            "{}\n{}\n",
            line("aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111", "refs/tags/v1.0.0"),
            line("bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222", "refs/tags/v1.0.0")
        );
        let err = parse_ls_remote_output(&out).unwrap_err();
        assert!(err.msg.contains("1.0.0"));
    }

    #[test]
    fn sorted_by_version() {
        let out = format!(
            "{}\n{}\n{}\n",
            line("z", "refs/tags/v2.0.0"),
            line("a", "refs/tags/v1.0.0"),
            line("m", "refs/tags/v1.1.0")
        );
        let tags = parse_ls_remote_output(&out).unwrap();
        let vs: Vec<SemVer> = tags.into_iter().map(|(v, _)| v).collect();
        assert_eq!(
            vs,
            vec![
                SemVer::parse("1.0.0").unwrap(),
                SemVer::parse("1.1.0").unwrap(),
                SemVer::parse("2.0.0").unwrap()
            ]
        );
    }
}
