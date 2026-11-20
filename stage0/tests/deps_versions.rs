//! 依存の版と取得元の manifest 検査 (R0・R1a)。

mod common;

use common::*;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

const SRC: &str =
    "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn manifest(version: &str, deps: &str) -> Tmp {
    let name = format!("deps_versions_{}", COUNTER.fetch_add(1, Ordering::Relaxed));
    let t = Tmp::new_raw(&name, SRC);
    let mut m = format!(
        "[package]\nname = \"t\"\nversion = \"{}\"\nedition = 1\nentry = \"t.main\"\n",
        version
    );
    if !deps.is_empty() {
        m.push_str("\n[dependencies]\n");
        m.push_str(deps);
    }
    fs::write(t.0.join("kasane.toml"), m).unwrap();
    t
}

fn check_codes(t: &Tmp) -> (i32, Vec<String>, String) {
    let o = run(&["check", "--json"], &t.0);
    let out = text(&o.stdout);
    (
        o.status.code().unwrap_or(-1),
        codes(&out),
        format!("{}{}", out, text(&o.stderr)),
    )
}

fn assert_diagnostic(version: &str, deps: &str, code: &str) {
    let t = manifest(version, deps);
    let (rc, cs, out) = check_codes(&t);
    assert_eq!(rc, 1, "{}", out);
    assert!(cs.iter().any(|c| c == code), "expected {}: {:?}", code, cs);
}

#[test]
fn valid_versioned_manifest_is_accepted() {
    let deps = concat!(
        "exact = { git = \"https://example.org/exact.git\", version = \"=1.2.3\" }\n",
        "local = { path = \"../local\" }\n",
        "pinned = { git = \"https://example.org/pinned.git\", rev = \"0123456789abcdef0123456789abcdef01234567\" }\n",
        "ranged = { git = \"https://example.org/ranged.git\", version = \">=1.2.0, <2.0.0\" }\n",
        "registry_pkg = { registry = \"https://example.org/kasane-registry.git\", version = \">=1.0.0, <2.0.0\" }\n",
    );
    let t = manifest("1.2.3", deps);
    let o = run(&["fmt"], &t.0);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}{}",
        text(&o.stdout),
        text(&o.stderr)
    );
    let o = run(&["fmt", "--check"], &t.0);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}{}",
        text(&o.stdout),
        text(&o.stderr)
    );
}

#[test]
fn package_version_must_be_strict_semver() {
    for version in ["1.2", "01.2.3", "1.2.3-alpha", "1.2.3+build"] {
        assert_diagnostic(version, "", "E9005");
    }
}

#[test]
fn dependency_sources_and_types_are_checked() {
    let cases = [
        ("dep = \"1.2.3\"\n", "E9008"),
        ("dep = { version = \"=1.2.3\" }\n", "E9008"),
        ("dep = { path = \"../local\", git = \"https://example.org/x.git\" }\n", "E9008"),
        ("dep = { git = \"https://example.org/x.git\" }\n", "E9008"),
        ("dep = { git = \"https://example.org/x.git\", rev = \"0123456789abcdef0123456789abcdef01234567\", version = \"=1.2.3\" }\n", "E9008"),
        ("dep = { registry = \"https://example.org/registry.git\" }\n", "E9008"),
        ("dep = { path = \"../local\", rev = \"0123456789abcdef0123456789abcdef01234567\" }\n", "E9008"),
        ("dep = { registry = \"https://example.org/registry.git\", version = \"=1.2.3\", rev = \"0123456789abcdef0123456789abcdef01234567\" }\n", "E9008"),
    ];
    for (deps, code) in cases {
        assert_diagnostic("1.2.3", deps, code);
    }
}

#[test]
fn dependency_urls_revs_tags_and_ranges_are_checked() {
    let cases = [
        ("dep = { git = \"http://example.org/x.git\", rev = \"0123456789abcdef0123456789abcdef01234567\" }\n", "E9008"),
        ("dep = { registry = \"file://example.org/registry.git\", version = \"=1.2.3\" }\n", "E9008"),
        ("dep = { git = \"https://example.org/x.git\", rev = \"main\" }\n", "E9008"),
        ("dep = { git = \"https://example.org/x.git\", rev = \"0123456789ABCDEF0123456789abcdef01234567\" }\n", "E9008"),
        ("dep = { git = \"https://example.org/x.git\", tag = \"v1.2.3\" }\n", "E9008"),
        ("dep = { git = \"https://example.org/x.git\", version = \"^1.2.3\" }\n", "E9008"),
        ("dep = { git = \"https://example.org/x.git\", version = \">= 1.2.3\" }\n", "E9008"),
        ("dep = { git = \"https://example.org/x.git\", version = \">2.0.0, <1.0.0\" }\n", "E9008"),
    ];
    for (deps, code) in cases {
        assert_diagnostic("1.2.3", deps, code);
    }
}

#[test]
fn unknown_dependency_key_is_e9007() {
    assert_diagnostic(
        "1.2.3",
        "dep = { path = \"../local\", branch = \"main\" }\n",
        "E9007",
    );
}

#[test]
fn fmt_normalizes_dependency_ranges() {
    let t = manifest(
        "1.2.3",
        "ranged = { git = \"https://example.org/ranged.git\", version = \"<2.0.0, >=1.2.0\" }\n",
    );
    let o = run(&["fmt"], &t.0);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}{}",
        text(&o.stdout),
        text(&o.stderr)
    );
    let m = fs::read_to_string(t.0.join("kasane.toml")).unwrap();
    assert!(m.contains("version = \">=1.2.0, <2.0.0\""), "{}", m);
}

#[test]
fn version_resolution_is_explicitly_not_implemented() {
    assert_diagnostic(
        "1.2.3",
        "dep = { git = \"https://example.org/x.git\", version = \"=1.2.3\" }\n",
        "E9040",
    );
    assert_diagnostic(
        "1.2.3",
        "dep = { registry = \"https://example.org/registry.git\", version = \"=1.2.3\" }\n",
        "E9040",
    );
}
