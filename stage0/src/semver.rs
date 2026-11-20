//! 依存の版と範囲の厳格な parser (R1a)。

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

/// `MAJOR.MINOR.PATCH` の版。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    pub fn parse(s: &str) -> Result<Version, String> {
        parse_version(s)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for Version {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_version(s)
    }
}

/// `MAJOR.MINOR.PATCH` を解析する。pre-release・build metadata・0 以外の先頭 0 は拒否する。
pub fn parse_version(s: &str) -> Result<Version, String> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return Err("version must be MAJOR.MINOR.PATCH".into());
    }
    Ok(Version {
        major: parse_component(parts[0], "major")?,
        minor: parse_component(parts[1], "minor")?,
        patch: parse_component(parts[2], "patch")?,
    })
}

fn parse_component(s: &str, name: &str) -> Result<u64, String> {
    if s.is_empty() {
        return Err(format!("{} must not be empty", name));
    }
    if !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{} must be a decimal integer", name));
    }
    if s.len() > 1 && s.starts_with('0') {
        return Err(format!("{} must not have a leading zero", name));
    }
    s.parse::<u64>()
        .map_err(|_| format!("{} is too large", name))
}

/// 範囲の一端。`inclusive` が false なら境界そのものは含まない。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Bound {
    version: Version,
    inclusive: bool,
}

/// 範囲の制約。比較を足しただけの形を保ち、上限・下限の最強の制約だけを持つ。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Constraints {
    exact: Option<Version>,
    lower: Option<Bound>,
    upper: Option<Bound>,
}

impl Constraints {
    fn add_exact(&mut self, version: Version) -> Result<(), String> {
        if let Some(old) = self.exact {
            if old != version {
                return Err("range constraints have no common version".into());
            }
        }
        self.exact = Some(version);
        Ok(())
    }

    fn add_lower(&mut self, bound: Bound) {
        self.lower = Some(match self.lower {
            None => bound,
            Some(old) if bound.version > old.version => bound,
            Some(old) if bound.version == old.version => Bound {
                version: old.version,
                inclusive: old.inclusive && bound.inclusive,
            },
            Some(old) => old,
        });
    }

    fn add_upper(&mut self, bound: Bound) {
        self.upper = Some(match self.upper {
            None => bound,
            Some(old) if bound.version < old.version => bound,
            Some(old) if bound.version == old.version => Bound {
                version: old.version,
                inclusive: old.inclusive && bound.inclusive,
            },
            Some(old) => old,
        });
    }

    fn valid(&self) -> bool {
        if let Some(v) = self.exact {
            return self
                .lower
                .map_or(true, |b| b.version < v || (b.version == v && b.inclusive))
                && self
                    .upper
                    .map_or(true, |b| b.version > v || (b.version == v && b.inclusive));
        }
        match (self.lower, self.upper) {
            (Some(lo), Some(hi)) => {
                lo.version < hi.version
                    || (lo.version == hi.version && lo.inclusive && hi.inclusive)
            }
            _ => true,
        }
    }

    fn matches(&self, version: &Version) -> bool {
        if let Some(exact) = self.exact {
            if exact != *version {
                return false;
            }
        }
        if let Some(lo) = self.lower {
            if version < &lo.version || (version == &lo.version && !lo.inclusive) {
                return false;
            }
        }
        if let Some(hi) = self.upper {
            if version > &hi.version || (version == &hi.version && !hi.inclusive) {
                return false;
            }
        }
        true
    }
}

/// 比較演算子を連結した版の範囲。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Range {
    constraints: Constraints,
}

impl Range {
    pub fn parse(s: &str) -> Result<Range, String> {
        parse_range(s)
    }

    pub fn matches(&self, version: &Version) -> bool {
        self.constraints.matches(version)
    }

    /// 2 つの範囲の積。共通の版が若无ければ `None`。
    pub fn intersection(&self, other: &Range) -> Option<Range> {
        let mut c = self.constraints;
        if let Some(v) = other.constraints.exact {
            c.add_exact(v).ok()?;
        }
        if let Some(b) = other.constraints.lower {
            c.add_lower(b);
        }
        if let Some(b) = other.constraints.upper {
            c.add_upper(b);
        }
        c.valid().then_some(Range { constraints: c })
    }

    /// 条件を満たす候補の最大の版。
    pub fn max_satisfying<I, T>(&self, candidates: I) -> Option<Version>
    where
        I: IntoIterator<Item = T>,
        T: Borrow<Version>,
    {
        candidates
            .into_iter()
            .filter(|v| self.matches(v.borrow()))
            .map(|v| *v.borrow())
            .max()
    }
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(v) = self.constraints.exact {
            return write!(f, "={}", v);
        }
        let mut first = true;
        if let Some(b) = self.constraints.lower {
            write!(f, "{}{}", if b.inclusive { ">=" } else { ">" }, b.version)?;
            first = false;
        }
        if let Some(b) = self.constraints.upper {
            if !first {
                write!(f, ", ")?;
            }
            write!(f, "{}{}", if b.inclusive { "<=" } else { "<" }, b.version)?;
        }
        Ok(())
    }
}

impl FromStr for Range {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_range(s)
    }
}

/// 唯一の範囲文法を解析する。区切りは `, ` だけ、比較子と版の間は空白なし。
pub fn parse_range(s: &str) -> Result<Range, String> {
    if s.is_empty() {
        return Err("version range must not be empty".into());
    }
    let mut c = Constraints::default();
    for part in s.split(", ") {
        if let Some(v) = part.strip_prefix(">=") {
            c.add_lower(Bound {
                version: parse_version(v)?,
                inclusive: true,
            });
        } else if let Some(v) = part.strip_prefix("<=") {
            c.add_upper(Bound {
                version: parse_version(v)?,
                inclusive: true,
            });
        } else if let Some(v) = part.strip_prefix('>') {
            c.add_lower(Bound {
                version: parse_version(v)?,
                inclusive: false,
            });
        } else if let Some(v) = part.strip_prefix('<') {
            c.add_upper(Bound {
                version: parse_version(v)?,
                inclusive: false,
            });
        } else if let Some(v) = part.strip_prefix('=') {
            c.add_exact(parse_version(v)?)?;
        } else {
            return Err("range comparator must be =, >=, <=, >, or <".into());
        }
    }
    if !c.valid() {
        return Err("range constraints have no common version".into());
    }
    Ok(Range { constraints: c })
}

/// Git の版候補タグ `vMAJOR.MINOR.PATCH` を解析する。
pub fn parse_git_tag(s: &str) -> Result<Version, String> {
    let version = s
        .strip_prefix('v')
        .ok_or_else(|| "git tag must be vMAJOR.MINOR.PATCH".to_string())?;
    parse_version(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_strict_versions_and_tags() {
        assert_eq!(parse_version("0.1.2"), Ok(Version::new(0, 1, 2)));
        assert_eq!(parse_version("10.20.30").unwrap().to_string(), "10.20.30");
        assert_eq!(parse_git_tag("v1.2.3"), Ok(Version::new(1, 2, 3)));
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.x",
            "1.2.3-alpha",
            "1.2.3+build",
            " 1.2.3",
            "1.2.3 ",
        ] {
            assert!(parse_version(bad).is_err(), "accepted version {:?}", bad);
        }
        for bad in ["1.2.3", "v1.2", "v01.2.3", "release-1.2.3", "v1.2.3-rc"] {
            assert!(parse_git_tag(bad).is_err(), "accepted tag {:?}", bad);
        }
    }

    #[test]
    fn parses_only_the_strict_range_grammar() {
        let r = parse_range(">=1.2.0, <2.0.0").unwrap();
        assert_eq!(r.to_string(), ">=1.2.0, <2.0.0");
        assert!(r.matches(&Version::new(1, 2, 0)));
        assert!(r.matches(&Version::new(1, 9, 9)));
        assert!(!r.matches(&Version::new(2, 0, 0)));
        assert_eq!(
            parse_range("<2.0.0, >=1.2.0").unwrap().to_string(),
            ">=1.2.0, <2.0.0"
        );
        assert_eq!(parse_range("=1.2.3").unwrap().to_string(), "=1.2.3");
        for bad in [
            "",
            "1.2.3",
            ">= 1.2.3",
            ">=1.2.3,<2.0.0",
            ">=1.2.3 , <2.0.0",
            ">=1.2.3,  <2.0.0",
            ">=1.2.3,",
            "^1.2.3",
            "~1.2.3",
            "*",
            "1.x",
            "1.2.3 - 2.0.0",
            ">=1.2.3 || <2.0.0",
            ">=1.2.3-alpha",
            ">=1.2.3+build",
            ">=01.2.3",
            ">=1.2",
            ">2.0.0, <1.0.0",
        ] {
            assert!(parse_range(bad).is_err(), "accepted range {:?}", bad);
        }
    }

    #[test]
    fn intersects_constraints() {
        let a = parse_range(">=1.0.0, <2.0.0").unwrap();
        let b = parse_range(">=1.5.0").unwrap();
        assert_eq!(a.intersection(&b).unwrap().to_string(), ">=1.5.0, <2.0.0");
        let c = parse_range("<1.0.0").unwrap();
        assert!(a.intersection(&c).is_none());
        let exact = parse_range("=1.5.0").unwrap();
        assert_eq!(b.intersection(&exact).unwrap().to_string(), "=1.5.0");
    }

    #[test]
    fn selects_the_maximum_matching_version() {
        let r = parse_range(">=1.0.0, <2.0.0").unwrap();
        let candidates = vec![
            Version::new(1, 0, 0),
            Version::new(1, 9, 0),
            Version::new(2, 0, 0),
            Version::new(1, 4, 0),
        ];
        assert_eq!(
            r.max_satisfying(candidates.iter()),
            Some(Version::new(1, 9, 0))
        );
        assert_eq!(
            parse_range(">=3.0.0")
                .unwrap()
                .max_satisfying(candidates.iter()),
            None
        );
        assert_eq!(r.max_satisfying(candidates), Some(Version::new(1, 9, 0)));
    }
}
