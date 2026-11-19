//! 8.2 の依存方向 (層規則、E1118) と、それに伴う参照検査の統合テスト。
//! `spec/tools/xref.py` の dep / ref の移植分。ユーザーパッケージが `src/core/` 配下に
//! 層の名前空間のモジュールを置く形を含む (diag ケースは単一ファイルしか置けないため)。

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(exe()).args(args).current_dir(cwd).output().expect("spawn ksn")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

/// 複数ファイルのパッケージ `t` を作る。files は (src からの相対パス, 内容)。
struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str, files: &[(&str, &str)]) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_test_layer_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        for (rel, src) in files {
            let p = d.join("src").join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, src).unwrap();
        }
        fs::write(d.join("kasane.toml"), TOML).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        Tmp(d)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        scratch::remove(&self.0);
    }
}

fn codes(json: &str) -> Vec<String> {
    json.lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| {
            let i = l.find("\"code\": \"")? + 9;
            Some(l[i..i + 5].to_string())
        })
        .collect()
}

const MAIN: &str = "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";

#[test]
fn lower_layer_must_not_reference_higher_layer() {
    // core が std / alloc の型を参照したら E1118 (8.2。xref.py の dep の移植)。
    let core_to_std = Tmp::new(
        "core_to_std",
        &[
            ("t.ksnl", MAIN),
            (
                "core/dep_x.ksnl",
                "fn core.dep_x.f(e: std.io.Error) -> i32 uses [] panics never pub\n    core.mem.drop(e)\n    1i32\nend core.dep_x.f\n",
            ),
        ],
    );
    let o = run(&["check", "--json"], &core_to_std.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(codes(&text(&o.stdout)).contains(&"E1118".to_string()));

    // alloc が std の型を参照しても E1118。
    let alloc_to_std = Tmp::new(
        "alloc_to_std",
        &[
            ("t.ksnl", MAIN),
            (
                "alloc/x.ksnl",
                "fn alloc.x.g(e: std.io.Error) -> i32 uses [] panics never pub\n    core.mem.drop(e)\n    1i32\nend alloc.x.g\n",
            ),
        ],
    );
    let o = run(&["check", "--json"], &alloc_to_std.0);
    assert_eq!(o.status.code(), Some(1));
    assert!(codes(&text(&o.stdout)).contains(&"E1118".to_string()));
}

#[test]
fn layer_modules_may_reference_their_own_and_lower_layers() {
    // core は core のみ、alloc は core と alloc、std は3層すべて参照できる (8.1, 8.2)。
    let ok = Tmp::new(
        "layers_ok",
        &[
            ("t.ksnl", MAIN),
            (
                "core/dep_x.ksnl",
                "fn core.dep_x.f(o: core.cmp.Ordering) -> i32 uses [] panics never pub\n    core.mem.drop(o)\n    1i32\nend core.dep_x.f\n",
            ),
            (
                "alloc/x.ksnl",
                "fn alloc.x.g(l: alloc.alloc.Layout) -> i32 uses [] panics never pub\n    core.mem.drop(l)\n    1i32\nend alloc.x.g\n",
            ),
            (
                "std/x.ksnl",
                "fn std.x.h(e: std.io.Error) -> usize uses [] panics never pub\n    core.mem.drop(e)\n    0usize\nend std.x.h\n",
            ),
        ],
    );
    let o = run(&["check"], &ok.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
}

#[test]
fn user_modules_may_reference_all_layers() {
    // ユーザーモジュール (パッケージ名の最上位成分) は層を持たないので、
    // core / alloc / std のどれを参照しても E1118 にならない (8.1)。
    let ok = Tmp::new(
        "user_all_layers",
        &[(
            "t.ksnl",
            "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let v: alloc.vec.Vec<i32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)\n    core.mem.drop(v)\n    Ok(())\nend t.main\n\nfn t.mix(v: alloc.vec.Vec<i32, std.alloc.System>, e: core.cmp.Ordering) -> usize uses [] panics never pub\n    core.mem.drop(e)\n    v.len()\nend t.mix\n",
        )],
    );
    let o = run(&["check"], &ok.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
}

#[test]
fn fn_header_impls_trait_is_layer_checked() {
    // fn ヘッダの `impls <トレイト>` も層規則の参照 (xref.py は定義テキスト全体を
    // TYPEREF で見るので、ヘッダに書かれた impls も対象)。シグネチャ・本体には
    // 上位層の参照を置かず、impls だけが違反になる形にする。
    let bad = Tmp::new(
        "core_impls_std",
        &[
            ("t.ksnl", MAIN),
            (
                "core/dep_x.ksnl",
                "type core.dep_x.R\n    got: usize\nend core.dep_x.R\n\n\
                 impl std.io.Read<[]> for core.dep_x.R\n\
                 fn core.dep_x.read(self: &mut core.dep_x.R, _buffer: &mut [u8]) -> usize uses [] panics never impls std.io.Read<[]>\n    \
                 self.got\nend core.dep_x.read\n",
            ),
        ],
    );
    let o = run(&["check", "--json"], &bad.0);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
    let got = codes(&text(&o.stdout));
    assert!(got.contains(&"E1118".to_string()), "{}", text(&o.stdout));
    // この形では impls 以外の参照は層違反にならない。
    assert_eq!(got.iter().filter(|c| **c == "E1118").count(), 1, "{}", text(&o.stdout));
}

#[test]
fn user_root_impls_trait_is_not_layer_checked() {
    // ユーザー定義は層を持たないので、上位層のトレイトを impls しても E1118 にならない (8.1)。
    let ok = Tmp::new(
        "user_impls_ok",
        &[(
            "t.ksnl",
            "type t.R\n    got: usize\nend t.R\n\n\
             impl std.io.Read<[]> for t.R\n\
             fn t.R.read(self: &mut t.R, _buffer: &mut [u8]) -> Result<usize, std.io.Error> uses [] panics never impls std.io.Read<[]>\n    \
             Ok(self.got)\nend t.R.read\n\n\
             fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n",
        )],
    );
    let o = run(&["check"], &ok.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    assert!(!codes(&text(&o.stdout)).contains(&"E1118".to_string()), "{}", text(&o.stdout));
}
