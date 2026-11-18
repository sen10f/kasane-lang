//! `derive [Drop]` (仕様 4.3 / 5.3): 再帰する型の、深さに依らない破棄。木走査・VM・KIR / LLVM のネイティブで動くこと、
//! 深さ 100 万の連結リスト / 木をスタックを小さくしても破棄できること、診断 (E2051 / E2032 / E4013)、整形、`get --derived`。
//! 補助 (`Tmp` ほか) は `cli.rs` と同じものを最小限だけ複製している。

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

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// 一時パッケージ `t` (`src/t.ksnl`)。
struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str, src: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_dd_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), TOML).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(d.join("src").join("t.ksnl"), src).unwrap();
        // 書き方は 1 通り (E0020): 試験の入力は `ksn fmt` の出力にそろえる。
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_ksn")).arg("fmt").current_dir(&d).output();
        Tmp(d)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `ksn check --json` の診断コード (1 診断 1 行の JSON から取り出す)。
fn codes(t: &Tmp) -> Vec<String> {
    let o = run(&["check", "--json"], &t.0);
    let mut v: Vec<String> = text(&o.stdout)
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| {
            let i = l.find("\"code\": \"")? + 9;
            Some(l[i..i + 5].to_string())
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// 3 つの形 (文字列を持つ鎖・2 分木・片側に偏った 2 分木) を作って捨てる。`n` は鎖と偏った木の深さ、`rounds` は繰り返しの回数。
fn program(n: u64, rounds: u64) -> String {
    format!(
        r#"enum t.T derive [Drop] pub
    Leaf
    Name(alloc.string.String<std.alloc.System>, alloc.boxed.Box<t.T, std.alloc.System>)
    Two(alloc.boxed.Box<t.T, std.alloc.System>, i64, alloc.boxed.Box<t.T, std.alloc.System>)
end t.T

fn t.chain(n: i64) -> Result<t.T, alloc.alloc.AllocError> uses [alloc] panics never
    let mut x = t.T.Leaf
    let mut i = 0i64
    while i < n {{
        let b = alloc.boxed.Box.new(std.alloc.SYSTEM, x)?
        let s = alloc.string.String.new(std.alloc.SYSTEM)
        x = t.T.Name(s, b)
        i = i.wrapping_add(1i64)
    }}
    Ok(x)
end t.chain

fn t.comb(n: i64) -> Result<t.T, alloc.alloc.AllocError> uses [alloc] panics never
    let mut x = t.T.Leaf
    let mut i = 0i64
    while i < n {{
        let l = alloc.boxed.Box.new(std.alloc.SYSTEM, x)?
        let r = alloc.boxed.Box.new(std.alloc.SYSTEM, t.T.Leaf)?
        x = t.T.Two(l, i, r)
        i = i.wrapping_add(1i64)
    }}
    Ok(x)
end t.comb

fn t.full(depth: i64) -> Result<t.T, alloc.alloc.AllocError> uses [alloc] panics never stack 4096
    if depth == 0i64 {{
        return Ok(t.T.Leaf)
    }}
    let l = alloc.boxed.Box.new(std.alloc.SYSTEM, t.full(depth.wrapping_sub(1i64))?)?
    let r = alloc.boxed.Box.new(std.alloc.SYSTEM, t.full(depth.wrapping_sub(1i64))?)?
    Ok(t.T.Two(l, depth, r))
end t.full

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut k = 0i64
    while k < {rounds}i64 {{
        match t.chain({n}i64) {{
            Ok(x) => core.mem.drop(x)
            Err(_) => core.panic.panic("oom")
        }}
        match t.comb({n}i64) {{
            Ok(x) => core.mem.drop(x)
            Err(_) => core.panic.panic("oom")
        }}
        match t.full(10i64) {{
            Ok(x) => core.mem.drop(x)
            Err(_) => core.panic.panic("oom")
        }}
        k = k.wrapping_add(1i64)
    }}
    std.io.print_line("done")
end t.main
"#,
        n = n,
        rounds = rounds
    )
}

#[test]
fn derive_drop_runs_in_the_tree_walker_and_the_vm() {
    let t = Tmp::new("engines", &program(2000, 3));
    let c = run(&["check"], &t.0);
    assert_eq!(c.status.code(), Some(0), "check: {}{}", text(&c.stdout), text(&c.stderr));
    for engine in ["--engine=tree", "--engine=vm"] {
        let o = run(&["run", engine], &t.0);
        assert_eq!(o.status.code(), Some(0), "{}: {}{}", engine, text(&o.stdout), text(&o.stderr));
        assert_eq!(text(&o.stdout), "done\n", "{}", engine);
    }
}

/// ネイティブ (LLVM): 深さ 100 万の鎖と偏った木を、スタック 512 KiB でも捨てられる (再帰する破棄ならあふれる深さ)。
/// 繰り返して確保を返し忘れないことは、仮想メモリの上限で確かめる (漏らせば確保が失敗して panic (終了コード 101) になる)。
#[test]
fn derive_drop_frees_a_million_deep_chain_and_tree_natively_with_a_small_stack() {
    if cfg!(windows) {
        return;
    }
    let t = Tmp::new("native", &program(1_000_000, 12));
    let cache = std::env::temp_dir().join(format!("ksn_cache_dd_{}", std::process::id()));
    let o = Command::new(exe()).args(["build"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    let _ = fs::remove_dir_all(&cache);
    let native = t.0.join("target").join("debug").join("t.native");
    assert!(native.exists(), "derive [Drop] の型はネイティブに下ろせるはず: {}", out);
    // 1 回の繰り返しで鎖 (約 100 万 × 40 バイト) と木が生きる。12 回漏らすと 1 GB を超える。
    let o = Command::new("sh")
        .arg("-c")
        .arg(format!("ulimit -s 512; ulimit -v 700000; exec {}", native.display()))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    assert_eq!(text(&o.stdout), "done\n");
}

/// 診断: 手書きの `Drop` なしの再帰型は E4013、`derive [Drop]` があれば通る。
#[test]
fn derive_drop_satisfies_e4013() {
    let plain = "enum t.L pub\n    Nil\n    Cons(i64, alloc.boxed.Box<t.L, std.alloc.System>)\nend t.L\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    assert_eq!(codes(&Tmp::new("e4013_plain", plain)), vec!["E4013".to_string()]);
    let derived = plain.replace("enum t.L pub", "enum t.L derive [Drop] pub");
    assert!(codes(&Tmp::new("e4013_derived", &derived)).is_empty());
    // 型パラメータを持つ木も書ける。
    let generic = "enum t.L[T] derive [Drop] pub\n    Nil\n    Cons(T, alloc.boxed.Box<t.L<T>, std.alloc.System>)\nend t.L\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    assert!(codes(&Tmp::new("e4013_generic", generic)).is_empty(), "{:?}", codes(&Tmp::new("e4013_generic2", generic)));
    // `Option<Box<Self>>` の欄を持つ構造体。
    let opt = "type t.N derive [Drop] pub\n    v: i64\n    next: Option<alloc.boxed.Box<t.N, std.alloc.System>>\nend t.N\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    assert!(codes(&Tmp::new("e4013_opt", opt)).is_empty(), "{:?}", codes(&Tmp::new("e4013_opt2", opt)));
}

#[test]
fn derive_drop_rejects_shapes_it_cannot_drop_in_bounded_depth() {
    let main = "\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    // Vec を経由する再帰・タプルを経由する再帰・Box 以外の再帰。
    let vec = format!("enum t.Tree derive [Drop] pub\n    Leaf\n    Node(alloc.vec.Vec<t.Tree, std.alloc.System>)\nend t.Tree\n{}", main);
    assert_eq!(codes(&Tmp::new("e2051_vec", &vec)), vec!["E2051".to_string()]);
    let tup = format!("enum t.Tree derive [Drop] pub\n    Leaf\n    Node((i64, alloc.boxed.Box<t.Tree, std.alloc.System>))\nend t.Tree\n{}", main);
    assert_eq!(codes(&Tmp::new("e2051_tuple", &tup)), vec!["E2051".to_string()]);
    // 相互再帰。
    let mutual = format!(
        "enum t.A derive [Drop] pub\n    Leaf\n    Node(alloc.boxed.Box<t.B, std.alloc.System>)\nend t.A\n\nenum t.B derive [Drop] pub\n    Leaf\n    Node(alloc.boxed.Box<t.A, std.alloc.System>)\nend t.B\n{}",
        main
    );
    assert_eq!(codes(&Tmp::new("e2051_mutual", &mutual)), vec!["E2051".to_string()]);
    // 手書きの Drop との併用。
    let both = format!(
        "enum t.L derive [Drop] pub\n    Nil\n    Cons(i64, alloc.boxed.Box<t.L, std.alloc.System>)\nend t.L\n\nimpl core.ops.Drop for t.L\n\nfn t.L.drop(self: &mut t.L) -> () uses [] panics never impls core.ops.Drop\n    ()\nend t.L.drop\n{}",
        main
    );
    assert_eq!(codes(&Tmp::new("e2051_both", &both)), vec!["E2051".to_string()]);
    // Copy との併用 (Copy は Clone を要するので Clone も derive する)。
    let copy = format!("enum t.C derive [Clone, Copy, Drop] pub\n    A\n    B(i64)\nend t.C\n{}", main);
    assert!(codes(&Tmp::new("e2051_copy", &copy)).contains(&"E2051".to_string()));
    // linear 型は Drop を持てない。
    let lin = format!("enum t.Lin derive [Drop] linear pub\n    A\n    B(i64)\nend t.Lin\n{}", main);
    assert!(codes(&Tmp::new("e2032_linear", &lin)).contains(&"E2032".to_string()));
}

#[test]
fn derive_drop_is_documented_in_explain_and_listed_by_get_derived() {
    let t = Tmp::new("explain", "fn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n");
    let o = run(&["explain", "E4013"], &t.0);
    assert!(text(&o.stdout).contains("derive [Drop]"), "{}", text(&o.stdout));
    let o = run(&["explain", "E2051"], &t.0);
    assert!(text(&o.stdout).contains("derive [Drop]"), "{}", text(&o.stdout));
    let src = "enum t.L derive [Drop] pub\n    Nil\n    Cons(i64, alloc.boxed.Box<t.L, std.alloc.System>)\nend t.L\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    Ok(())\nend t.main\n";
    let t = Tmp::new("derived", src);
    let o = run(&["get", "--derived", "t.L"], &t.0);
    let s = text(&o.stdout);
    assert!(s.contains("impl core.ops.Drop for t.L") && s.contains("iterative"), "{}", s);
}

/// `derive` の並び (4.3): `Drop` は `TryClone` の後ろ、`PartialEq` の前。`ksn fmt` が並べ替える。
#[test]
fn derive_drop_has_a_place_in_the_derive_order() {
    let src = "enum t.L derive [PartialEq, Drop, Clone] pub\n    Nil\n    Cons(i64, alloc.boxed.Box<t.L, std.alloc.System>)\nend t.L\n";
    let t = Tmp::new("order", src);
    // `Tmp::new` は `ksn fmt` を通すので、並び違いの入力を書き戻す (E0020 の確認)。
    fs::write(t.0.join("src").join("t.ksnl"), src).unwrap();
    assert_ne!(run(&["check"], &t.0).status.code(), Some(0));
    assert_ne!(run(&["fmt", "--check"], &t.0).status.code(), Some(0));
    run(&["fmt"], &t.0);
    let s = fs::read_to_string(t.0.join("src").join("t.ksnl")).unwrap();
    assert!(s.starts_with("enum t.L derive [Clone, Drop, PartialEq] pub\n"), "{}", s);
}

/// セルフホストの試作 `selfhost/probes/boxtree` に `derive [Drop]` を足した版が、ネイティブで動く (`selfhost/` は書き換えない)。
#[test]
fn the_selfhost_boxtree_probe_builds_natively_with_derive_drop() {
    if cfg!(windows) {
        return;
    }
    let src = root().join("selfhost").join("probes").join("boxtree");
    if !src.exists() {
        return;
    }
    let d = std::env::temp_dir().join(format!("ksn_dd_{}_boxtree", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("src")).unwrap();
    for f in ["kasane.toml", "kasane.lock"] {
        fs::copy(src.join(f), d.join(f)).unwrap();
    }
    let mut n = 0;
    for e in fs::read_dir(src.join("src")).unwrap() {
        let p = e.unwrap().path();
        let body = fs::read_to_string(&p).unwrap();
        fs::write(d.join("src").join(p.file_name().unwrap()), &body).unwrap();
        n += 1;
    }
    assert!(n > 0);
    // 元のままでは E4013。
    let o = run(&["check"], &d);
    assert!(text(&o.stdout).contains("E4013") || text(&o.stderr).contains("E4013"), "{}{}", text(&o.stdout), text(&o.stderr));
    // `derive [Drop]` を足す。
    let f = d.join("src").join("boxtree.ksnl");
    let body = fs::read_to_string(&f).unwrap();
    assert!(body.contains("enum boxtree.Expr pub"));
    fs::write(&f, body.replace("enum boxtree.Expr pub", "enum boxtree.Expr derive [Drop] pub")).unwrap();
    let o = run(&["check"], &d);
    assert!(text(&o.stdout).trim().is_empty() && text(&o.stderr).trim().is_empty(), "{}{}", text(&o.stdout), text(&o.stderr));
    let o = run(&["fmt", "--check"], &d);
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let cache = std::env::temp_dir().join(format!("ksn_cache_dd_bt_{}", std::process::id()));
    let o = Command::new(exe()).args(["build"]).current_dir(&d).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    let _ = fs::remove_dir_all(&cache);
    let native = d.join("target").join("debug").join("boxtree.native");
    assert!(native.exists(), "{}", out);
    let o = Command::new(&native).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(text(&o.stdout), "x\n");
    let _ = fs::remove_dir_all(&d);
}
