//! `ksn unsafe [--json]` (設計_SendSyncとunsafe.md 第 3 波、H7): パッケージと `path` 依存の unsafe の使用の一覧。

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::path::Path;
use std::process::Command;

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

const LIB: &str = "type pl.Cell
    p: *mut u32
end pl.Cell

impl core.marker.Send for pl.Cell

extern pl.getpid() -> i32 link \"getpid\"

fn pl.pid() -> i32 uses [] panics never pub
    unsafe \"pid\" \"getpid has no preconditions\" {
        pl.getpid()
    }
end pl.pid
";

const APP: &str = "fn app.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end app.main
";

fn setup(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ksn_unsafe_report_{}_{}", std::process::id(), name));
    scratch::remove(&dir);
    write(&dir.join("lib/kasane.toml"), "[package]\nname = \"pl\"\nversion = \"0.1.0\"\nedition = 1\n\n[unsafe]\nmodules = [\"pl\"]\n\n[dependencies]\n");
    write(&dir.join("lib/kasane.lock"), "version = 1\n");
    write(&dir.join("lib/src/pl.ksnl"), LIB);
    write(&dir.join("app/kasane.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"app.main\"\n\n[dependencies]\npl = { path = \"../lib\" }\n");
    write(&dir.join("app/kasane.lock"), "version = 1\n");
    write(&dir.join("app/src/app.ksnl"), APP);
    dir
}

#[test]
fn text_report_lists_dependency_unsafe_items_in_order() {
    let dir = setup("text");
    let o = Command::new(exe()).arg("unsafe").current_dir(dir.join("app")).env("KSN_CACHE_DIR", dir.join("cache")).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert_eq!(o.status.code(), Some(0), "{}{}", out, String::from_utf8_lossy(&o.stderr));
    let want = [
        "package app (root)",
        "unsafe modules: (none)",
        "package pl (dependency)",
        "unsafe modules: pl",
        "raw-pointer type pl.Cell",
        "manual impl core.marker.Send for pl.Cell",
        "extern pl.getpid link \"getpid\"",
        "unsafe block \"pid\": getpid has no preconditions",
    ];
    let mut at = 0;
    for w in want {
        let i = out[at..].find(w).unwrap_or_else(|| panic!("missing {:?} in order:\n{}", w, out));
        at += i + w.len();
    }
    scratch::remove(&dir);
}

#[test]
fn json_report_has_kinds_and_is_stable() {
    let dir = setup("json");
    let run = || Command::new(exe()).args(["unsafe", "--json"]).current_dir(dir.join("app")).env("KSN_CACHE_DIR", dir.join("cache")).output().unwrap();
    let a = run();
    let b = run();
    assert_eq!(a.stdout, b.stdout);
    let v: serde_json::Value = serde_json::from_slice(&a.stdout).unwrap();
    let pk = v["packages"].as_array().unwrap();
    assert_eq!(pk[0]["name"], "app");
    assert_eq!(pk[1]["name"], "pl");
    assert_eq!(pk[1]["unsafe_modules"][0], "pl");
    let kinds: Vec<&str> = pk[1]["items"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["raw_pointer_type", "manual_impl", "extern", "unsafe_block"]);
    scratch::remove(&dir);
}

#[test]
fn bad_argument_is_usage_error() {
    let o = Command::new(exe()).args(["unsafe", "--bogus"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}
