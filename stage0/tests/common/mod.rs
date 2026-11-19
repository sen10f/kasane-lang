//! 複数のテストファイルで使う補助 (`Tmp`・`run`・`text` など)。

#![allow(dead_code, unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub mod scratch;
pub use scratch::Scratch;

pub const TOML: &str = "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n";

pub fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

pub fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(exe()).args(args).current_dir(cwd).output().expect("spawn ksn")
}

/// `run` と同じだが、環境変数も渡す（`std.env.var` の検査用）。
pub fn run_with_env(args: &[&str], cwd: &Path, env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(exe());
    cmd.args(args).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn ksn")
}

pub fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// 一時パッケージ `t` を作る。
pub struct Tmp(pub PathBuf);
impl Tmp {
    /// 一時パッケージ `t`。`.ksnl` にコメントは置けない (E0059) ので、試験の入力に説明のコメントの行があれば外し (`strip_ksnh`)、
    /// `ksn fmt` の出力にそろえる (E0020)。コメントそのものを試験するときは `new_raw` (そのまま置く) か `new_ksnh` (`.ksnh` を正にする)。
    pub fn new(name: &str, src: &str) -> Tmp {
        let t = Tmp::new_strict(name, src);
        t.effects();
        t
    }

    /// `new` から、`uses` / `panics` を本体の効果にそろえる `ksn fix` (E4020 / E4025 だけ) を除いたもの。
    /// 試験の入力の `main_wrap` などの固定の見出しを、本体の効果と完全に一致させる (D5・D13) ための後始末を省く。
    /// 効果の完全一致そのものを試すときはこちら (または `new_raw`)。
    pub fn new_strict(name: &str, src: &str) -> Tmp {
        let t = Tmp::new_raw(name, &strip_ksnh(src));
        t.fmt();
        t
    }

    /// 効果の宣言 (`uses` / `panics`) を本体の効果にそろえる (`ksn fix` の E4020 / E4025 だけ)。
    pub fn effects(&self) {
        let _ = Command::new(exe()).arg("fix").env("KSN_FIX_ONLY", "E4020,E4025").current_dir(&self.0).output();
    }

    /// 書き方は 1 通り (E0020) なので、試験の入力は `ksn fmt` の出力にそろえる。`fmt` を通さずに試すときは `Tmp::new_raw`。
    pub fn fmt(&self) {
        let _ = run(&["fmt"], &self.0);
        one_way_fix(&self.0);
    }

    /// `ksn fmt` を通さない版 (E0020 などの整形の誤りそのものを試すとき)。
    pub fn new_raw(name: &str, src: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("ksn_test_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("kasane.toml"), TOML).unwrap();
        fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
        fs::write(d.join("src").join("t.ksnl"), src).unwrap();
        Tmp(d)
    }
}
impl Tmp {
    /// `.ksnh` (コメント付き) を正にした一時パッケージ: `src/t.ksnh` と、その `strip` の `src/t.ksnl` を置く。
    pub fn new_ksnh(name: &str, ksnh: &str) -> Tmp {
        let t = Tmp::new_raw(name, &strip_ksnh(ksnh));
        fs::write(t.0.join("src").join("t.ksnh"), ksnh).unwrap();
        t.fmt();
        t
    }
}

/// `ksn strip` と同じ変換 (先頭の非空白が `//` の行を消す。ファイル先頭のコメントの塊の直後の空行も消す)。
pub fn strip_ksnh(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let is_c = |l: &str| l.trim_start_matches(' ').starts_with("//");
    let mut keep: Vec<bool> = lines.iter().map(|l| !is_c(l)).collect();
    let head = lines.iter().take_while(|l| is_c(l)).count();
    if head > 0 && head + 1 < lines.len() && lines[head].is_empty() {
        keep[head] = false;
    }
    lines.iter().zip(&keep).filter(|(_, k)| **k).map(|(l, _)| *l).collect::<Vec<_>>().join("\n")
}

impl Drop for Tmp {
    fn drop(&mut self) {
        scratch::remove(&self.0);
    }
}

pub fn codes(json: &str) -> Vec<String> {
    // 1診断1行の JSON から "code" を取り出す (簡易)。
    json.lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| {
            let i = l.find("\"code\": \"")? + 9;
            Some(l[i..i + 5].to_string())
        })
        .collect()
}

// ---- 既知のギャップ: 仕様上エラーになるべきだが現状は通る (実装されたら #[ignore] を外す) ----

pub fn main_wrap(prefix: &str, body: &str) -> String {
    format!(
        "{}fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may\n{}    Ok(())\nend t.main\n",
        prefix, body
    )
}

// ---- 数値の挙動 ----

pub fn run_ok(name: &str, body: &str) {
    let t = Tmp::new(name, &main_wrap("", body));
    let o = run(&["run"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}: {}{}", name, text(&o.stdout), text(&o.stderr));
}

// ---- 整数の算術演算子 (仕様 0.9: 4.1) ----

/// 行の配列を、4 スペースインデントの本体にする。
pub fn body(lines: &[&str]) -> String {
    lines.iter().map(|l| format!("    {}\n", l)).collect()
}

// ---- 型付きの式推論 (types.rs): レシーバの型によるメソッド検査 ----

pub fn check_codes(name: &str, b: &[&str]) -> (Vec<String>, String) {
    let t = Tmp::new(name, &main_wrap("", &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

/// 複数ファイルのパッケージ (`files` は `src/` からの相対パスと中身)。
pub fn tmp_files(name: &str, files: &[(&str, &str)]) -> Tmp {
    let t = Tmp::new(name, "");
    for (rel, body) in files {
        let p = t.0.join("src").join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }
    t.fmt();
    t
}

pub const TRAIT_SIDE_TR: &str = "trait t.tr.Show pub
    fn show(self: &Self) -> u32 uses [] panics never
end t.tr.Show
";
pub const TRAIT_SIDE_TY: &str = "type t.ty.P pub
    a: u32 pub
end t.ty.P
";
pub const TRAIT_SIDE_MAIN: &str = "impl t.tr.Show for alloc.string.String
fn t.tr.Show.alloc.string.String.show(self: &alloc.string.String) -> u32 uses [] panics never impls t.tr.Show
    if self.is_empty() {
        return 100u32
    }
    3u32
end t.tr.Show.alloc.string.String.show

impl t.tr.Show for t.ty.P
fn t.tr.Show.t.ty.P.show(self: &t.ty.P) -> u32 uses [] panics never impls t.tr.Show
    self.a.wrapping_add(1u32)
end t.tr.Show.t.ty.P.show

impl t.tr.Show for u32
fn t.tr.Show.u32.show(self: &u32) -> u32 uses [] panics never impls t.tr.Show
    let v = *self
    v.wrapping_add(10u32)
end t.tr.Show.u32.show

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let s = alloc.string.String.from_str(std.alloc.SYSTEM, \"abc\").unwrap()
    let p = t.ty.P { a: 4u32 }
    let n = 7u32
    core.check.eq(s.show(), 3u32)
    core.check.eq(p.show(), 5u32)
    core.check.eq(n.show(), 17u32)
    std.io.print_line(\"ok\")?
    Ok(())
end t.main
";

pub fn clif_program(last_check: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.fib(n: u32) -> u32 uses [] panics never stack 4096",
        "    if n < 2u32 {",
        "        return n",
        "    }",
        "    t.fib(n.wrapping_sub(1u32)).wrapping_add(t.fib(n.wrapping_sub(2u32)))",
        "end t.fib",
        "",
        "fn t.sign(x: i32) -> i32 uses [] panics never",
        "    if x < 0i32 {",
        "        return 0i32.wrapping_sub(1i32)",
        "    }",
        "    if x == 0i32 {",
        "        return 0i32",
        "    }",
        "    1i32",
        "end t.sign",
        "",
        "fn t.mix(a: u8, b: u8) -> u8 uses [] panics never",
        "    a.wrapping_add(b) ^ 3u8",
        "end t.mix",
        "",
        "fn t.count(limit: u32) -> u32 uses [] panics never",
        "    let mut i = 0u32",
        "    let mut acc = 0u32",
        "    loop {",
        "        i = i.wrapping_add(1u32)",
        "        if i > limit {",
        "            break",
        "        }",
        "        if i & 1u32 == 1u32 {",
        "            continue",
        "        }",
        "        acc = acc.wrapping_add(i)",
        "    }",
        "    acc",
        "end t.count",
        "",
        "fn t.both(a: bool, b: bool) -> bool uses [] panics never",
        "    (a && !b) || (b && !a)",
        "end t.both",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    core.check.eq(t.fib(10u32), 55u32)",
        "    core.check.eq(t.sign(0i32.wrapping_sub(5i32)), 0i32.wrapping_sub(1i32))",
        "    core.check.eq(t.sign(7i32), 1i32)",
        "    core.check.eq(t.mix(250u8, 10u8), 7u8)",
        "    core.check.eq(t.count(10u32), 30u32)",
        "    core.check.eq(t.both(true, false), true)",
        "    core.check.eq(t.both(true, true), false)",
        last_check,
        "    core.mem.drop(std.io.print_line(\"clif ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

/// ネイティブ (LLVM) が作れたか、作れなければ理由 (KIR に下ろせない・clang の失敗) とバンドルへの退避が出ていること。
/// 以前は Cranelift が「退避なしで作れる」ことを確かめていた試験の置き換え。LLVM が対応を広げたら、
/// 退避の許容をやめて `assert!(!out.contains("skipped"))` に戻せる。
pub fn assert_native_or_reasoned_skip(out: &str) {
    if out.contains("skipped") {
        assert!(
            out.contains("native backend skipped: ")
                && out.contains("the interpreter bundle is used")
                && (out.contains("KIR に下ろせない") || out.contains("clang が失敗した")),
            "退避には理由とバンドルへの退避が要る: {}",
            out
        );
    }
}

/// ネイティブの実行ファイル `p` があればそれ、無ければ (理由付きの退避を確かめたうえで) 同じ名前のバンドルの起動スクリプト。
pub fn native_or_launcher(p: PathBuf, out: &str) -> PathBuf {
    if p.exists() {
        return p;
    }
    assert!(out.contains("skipped"), "ネイティブもバンドルの退避の表示もない: {}", out);
    assert_native_or_reasoned_skip(out);
    p.with_extension(if cfg!(windows) { "cmd" } else { "" })
}

/// `ksn build` (既定の LLVM) で作る。ネイティブの実行ファイルが作れなければ、`ksn build` が理由を出して
/// インタプリタ同梱のバンドルだけにする (`native backend skipped: ...; the interpreter bundle is used`)。
/// そのときは、理由が出ていることを確かめたうえで、バンドルの起動スクリプトを返す (同じ性質をバンドルで確かめる)。
/// ネイティブだけを確かめたい試験は `build_native_strict` を使う。
pub fn build_native(name: &str, src: &str, release: bool) -> (Tmp, Option<PathBuf>, String) {
    let (t, found, out) = build_native_strict(name, src, release);
    if found.is_some() {
        return (t, found, out);
    }
    assert!(
        out.contains("native backend skipped: ") && out.contains("the interpreter bundle is used"),
        "ネイティブが作れないときは理由とバンドルへの退避を出す: {}",
        out
    );
    let kind = if release { "release" } else { "debug" };
    let launcher = t.0.join("target").join(kind).join(if cfg!(windows) { "t.cmd" } else { "t" });
    assert!(launcher.exists(), "バンドルの起動スクリプトがない: {}", out);
    (t, Some(launcher), out)
}

pub fn build_native_strict(name: &str, src: &str, release: bool) -> (Tmp, Option<PathBuf>, String) {
    let t = Tmp::new(name, src);
    let cache = Scratch::at(format!("ksn_cache_{}_{}", std::process::id(), name));
    let mut c = Command::new(exe());
    c.args(["build"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
    if release {
        c.args(["--release", "--allow-bundle"]);
    }
    let o = c.output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    let kind = if release { "release" } else { "debug" };
    let p = t.0.join("target").join(kind).join(if cfg!(windows) { "t.exe" } else { "t.native" });
    drop(cache);
    let found = if p.exists() { Some(p) } else { None };
    (t, found, out)
}

pub fn clif_arith_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.add8(a: u8, b: u8) -> u8 uses [] panics may",
        "    a + b",
        "end t.add8",
        "",
        "fn t.mixed(a: i32, b: i32) -> i32 uses [] panics may",
        "    (a * b - 3i32) / 2i32 + a % b",
        "end t.mixed",
        "",
        "fn t.neg(a: i64) -> i64 uses [] panics may",
        "    -a",
        "end t.neg",
        "",
        "fn t.div(a: u32, b: u32) -> u32 uses [] panics may",
        "    a / b",
        "end t.div",
        "",
        "fn t.area(w: f64, h: f64) -> f64 uses [] panics never",
        "    w * h + 0.5f64",
        "end t.area",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    core.check.eq(t.add8(200u8, 55u8), 255u8)",
        "    core.check.eq(t.mixed(7i32, 5i32), 16i32 + 2i32)",
        "    core.check.eq(t.mixed(0i32 - 7i32, 5i32), 0i32 - 21i32)",
        "    core.check.eq(t.neg(9i64), 0i64 - 9i64)",
        "    core.check.eq(t.div(17u32, 5u32), 3u32)",
        "    core.check.eq(t.area(1.5f64, 2.0f64), 3.5f64)",
        "    core.check.eq(t.area(1.0f64, 1.0f64) > 1.0f64, true)",
        tail,
        "    core.mem.drop(std.io.print_line(\"arith ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

pub fn clif_struct_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "type t.Point derive [Copy, Clone]",
        "    x: i32",
        "    y: i32",
        "end t.Point",
        "",
        "type t.Rect derive [Copy, Clone]",
        "    min: t.Point",
        "    max: t.Point",
        "    tag: u8",
        "end t.Rect",
        "",
        "enum t.Shape",
        "    Empty",
        "    Circle(u32)",
        "    Square(u32, u32)",
        "end t.Shape",
        "",
        "fn t.Point.sum(self: &t.Point) -> i32 uses [] panics never",
        "    self.x.wrapping_add(self.y)",
        "end t.Point.sum",
        "",
        "fn t.Point.shift(self: &mut t.Point, dx: i32) -> i32 uses [] panics never",
        "    self.x = self.x.wrapping_add(dx)",
        "    self.x",
        "end t.Point.shift",
        "",
        "fn t.make(a: i32, b: i32) -> t.Point uses [] panics never",
        "    t.Point { x: a, y: b }",
        "end t.make",
        "",
        "fn t.width(r: t.Rect) -> i32 uses [] panics never",
        "    r.max.x.wrapping_sub(r.min.x)",
        "end t.width",
        "",
        "fn t.area(s: t.Shape) -> u32 uses [] panics never",
        "    match s {",
        "        t.Shape.Empty => 0u32",
        "        t.Shape.Circle(r) => r.wrapping_mul(3u32)",
        "        t.Shape.Square(w, h) => w.wrapping_mul(h)",
        "    }",
        "end t.area",
        "",
        "fn t.shape_of(n: u32) -> t.Shape uses [] panics never",
        "    if n == 0u32 {",
        "        return t.Shape.Empty",
        "    }",
        "    if n < 10u32 {",
        "        return t.Shape.Circle(n)",
        "    }",
        "    t.Shape.Square(n, 2u32)",
        "end t.shape_of",
        "",
        "fn t.classify(n: u32) -> u8 uses [] panics never",
        "    match n {",
        "        0u32 => 10u8",
        "        1u32 => 20u8",
        "        _ => 30u8",
        "    }",
        "end t.classify",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let p = t.make(3i32, 4i32)",
        "    core.check.eq(p.sum(), 7i32)",
        "    core.check.eq(p.x, 3i32)",
        "    let mut q = p",
        "    core.check.eq(q.shift(10i32), 13i32)",
        "    core.check.eq(q.x, 13i32)",
        "    core.check.eq(p.x, 3i32)",
        "    q.y = 99i32",
        "    core.check.eq(q.sum(), 112i32)",
        "    let r = t.Rect { min: p, max: q, tag: 7u8 }",
        "    core.check.eq(t.width(r), 10i32)",
        "    core.check.eq(r.max.y, 99i32)",
        "    core.check.eq(t.area(t.shape_of(0u32)), 0u32)",
        "    core.check.eq(t.area(t.shape_of(4u32)), 12u32)",
        "    core.check.eq(t.area(t.shape_of(12u32)), 24u32)",
        "    core.check.eq(t.area(t.Shape.Square(5u32, 6u32)), 30u32)",
        "    core.check.eq(t.classify(0u32), 10u8)",
        "    core.check.eq(t.classify(1u32), 20u8)",
        "    core.check.eq(t.classify(9u32), 30u8)",
        tail,
        "    core.mem.drop(std.io.print_line(\"struct ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

pub fn clif_option_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "type t.Pair derive [Copy, Clone]",
        "    a: u32",
        "    b: u32",
        "end t.Pair",
        "",
        "fn t.find(a: u32, b: u32) -> Option<u32> uses [] panics never",
        "    if a > b {",
        "        return Some(a.wrapping_sub(b))",
        "    }",
        "    None",
        "end t.find",
        "",
        "fn t.parse(n: u32) -> Result<u32, u8> uses [] panics never",
        "    if n == 0u32 {",
        "        return Err(7u8)",
        "    }",
        "    Ok(n.wrapping_mul(2u32))",
        "end t.parse",
        "",
        "fn t.unit_ok(flag: bool) -> Result<(), u8> uses [] panics never",
        "    if flag {",
        "        return Ok(())",
        "    }",
        "    Err(1u8)",
        "end t.unit_ok",
        "",
        "fn t.pair_of(n: u32) -> Option<t.Pair> uses [] panics never",
        "    if n == 0u32 {",
        "        return None",
        "    }",
        "    Some(t.Pair { a: n, b: n.wrapping_add(1u32) })",
        "end t.pair_of",
        "",
        "fn t.describe(o: Option<u32>) -> u32 uses [] panics never",
        "    match o {",
        "        Some(v) => v.wrapping_add(100u32)",
        "        None => 0u32",
        "    }",
        "end t.describe",
        "",
        "fn t.res_val(r: Result<u32, u8>) -> u32 uses [] panics never",
        "    match r {",
        "        Ok(v) => v",
        "        Err(_) => 999u32",
        "    }",
        "end t.res_val",
        "",
        "fn t.unit_code(r: Result<(), u8>) -> u8 uses [] panics never",
        "    match r {",
        "        Ok(_) => 0u8",
        "        Err(e) => e",
        "    }",
        "end t.unit_code",
        "",
        "fn t.pair_sum(o: Option<t.Pair>) -> u32 uses [] panics never",
        "    match o {",
        "        Some(p) => p.a.wrapping_add(p.b)",
        "        None => 0u32",
        "    }",
        "end t.pair_sum",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    core.check.eq(t.describe(t.find(9u32, 4u32)), 105u32)",
        "    core.check.eq(t.describe(t.find(1u32, 4u32)), 0u32)",
        "    core.check.eq(t.res_val(t.parse(21u32)), 42u32)",
        "    core.check.eq(t.res_val(t.parse(0u32)), 999u32)",
        "    core.check.eq(t.unit_code(t.unit_ok(true)), 0u8)",
        "    core.check.eq(t.unit_code(t.unit_ok(false)), 1u8)",
        "    core.check.eq(t.pair_sum(t.pair_of(5u32)), 11u32)",
        "    core.check.eq(t.pair_sum(t.pair_of(0u32)), 0u32)",
        tail,
        "    core.mem.drop(std.io.print_line(\"option ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

pub fn clif_vec_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "type t.Bag",
        "    items: alloc.vec.Vec<u32, std.alloc.System>",
        "    n: u32",
        "end t.Bag",
        "",
        "fn t.fill(count: u32) -> alloc.vec.Vec<u32, std.alloc.System>",
        "    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "    let mut i = 0u32",
        "    while i < count {",
        "        core.mem.drop(match v.push(i.wrapping_mul(i)) {",
        "            Ok(_) => 0u8",
        "            Err(_) => 1u8",
        "        })",
        "        i = i.wrapping_add(1u32)",
        "    }",
        "    v",
        "end t.fill",
        "",
        "fn t.sum(v: &alloc.vec.Vec<u32, std.alloc.System>) -> u32",
        "    let mut i = 0usize",
        "    let mut acc = 0u32",
        "    while i < v.len() {",
        "        acc = acc.wrapping_add(v[i])",
        "        i = i.wrapping_add(1usize)",
        "    }",
        "    acc",
        "end t.sum",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let v = t.fill(5u32)",
        "    core.check.eq(v.len(), 5usize)",
        "    core.check.eq(t.sum(&v), 30u32)",
        "    let mut w = t.fill(3u32)",
        "    match w.pop() {",
        "        Some(x) => core.check.eq(x, 4u32)",
        "        None => core.check.eq(1u8, 2u8)",
        "    }",
        "    core.check.eq(w.len(), 2usize)",
        "    core.check.eq(w.is_empty(), false)",
        "    w.clear()",
        "    core.check.eq(w.is_empty(), true)",
        "    let big = t.fill(1000u32)",
        "    core.check.eq(big.len(), 1000usize)",
        "    core.check.eq(t.sum(&big), 332833500u32)",
        "    let mut bag = t.Bag { items: t.fill(2u32), n: 7u32 }",
        "    core.mem.drop(match bag.items.push(9u32) {",
        "        Ok(_) => 0u8",
        "        Err(_) => 1u8",
        "    })",
        "    core.check.eq(bag.items.len(), 3usize)",
        "    core.check.eq(bag.items[2usize], 9u32)",
        "    let s = \"hello\"",
        "    core.check.eq(s.len(), 5usize)",
        "    core.mem.drop(std.io.print_line(s))",
        tail,
        "    core.mem.drop(std.io.print_line(\"vec ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

pub fn clif_string_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    let sv = "alloc.string.String<std.alloc.System>";
    let vs = "alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>";
    [
        "type t.Doc".to_string(),
        format!("    name: {}", sv),
        format!("    parts: {}", vs),
        "end t.Doc".to_string(),
        String::new(),
        format!("fn t.build(n: u32) -> {}", vs),
        "    let mut v = t.fresh()".to_string(),
        "    let mut i = 0u32".to_string(),
        "    while i < n {".to_string(),
        "        let g = t.greeting(\"x\")".to_string(),
        "        core.mem.drop(match v.push(g) {".to_string(),
        "            Ok(_) => 0u8".to_string(),
        "            Err(_) => 1u8".to_string(),
        "        })".to_string(),
        "        i = i.wrapping_add(1u32)".to_string(),
        "    }".to_string(),
        "    v".to_string(),
        "end t.build".to_string(),
        String::new(),
        "fn t.early(flag: bool) -> usize".to_string(),
        "    let s = t.greeting(\"early\")".to_string(),
        "    if flag {".to_string(),
        "        return s.len()".to_string(),
        "    }".to_string(),
        "    let other = t.greeting(\"late\")".to_string(),
        "    other.len().wrapping_add(s.len())".to_string(),
        "end t.early".to_string(),
        String::new(),
        format!("fn t.fresh() -> {}", vs),
        "    alloc.vec.Vec.new(std.alloc.SYSTEM)".to_string(),
        "end t.fresh".to_string(),
        String::new(),
        format!("fn t.greeting(who: &str) -> {}", sv),
        "    let mut s = alloc.string.String.new(std.alloc.SYSTEM)".to_string(),
        "    core.mem.drop(match s.push_str(\"hello, \") {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    })".to_string(),
        "    core.mem.drop(match s.push_str(who) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    })".to_string(),
        "    s".to_string(),
        "end t.greeting".to_string(),
        String::new(),
        "fn t.looping() -> u32".to_string(),
        "    let mut i = 0u32".to_string(),
        "    let mut total = 0u32".to_string(),
        "    loop {".to_string(),
        "        i = i.wrapping_add(1u32)".to_string(),
        "        let _s = t.greeting(\"loop\")".to_string(),
        "        if i > 50u32 {".to_string(),
        "            break".to_string(),
        "        }".to_string(),
        "        if i & 1u32 == 1u32 {".to_string(),
        "            continue".to_string(),
        "        }".to_string(),
        "        total = total.wrapping_add(1u32)".to_string(),
        "    }".to_string(),
        "    total".to_string(),
        "end t.looping".to_string(),
        String::new(),
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may".to_string(),
        "    let g = t.greeting(\"world\")".to_string(),
        "    core.mem.drop(std.io.print_line(g.as_str()))".to_string(),
        "    core.check.eq(g.as_str(), \"hello, world\")".to_string(),
        "    core.check.eq(g.len(), 12usize)".to_string(),
        "    let docs = t.build(100u32)".to_string(),
        "    core.check.eq(docs.len(), 100usize)".to_string(),
        "    let mut d = t.Doc { name: t.greeting(\"doc\"), parts: t.build(3u32) }".to_string(),
        "    core.check.eq(d.name.as_str(), \"hello, doc\")".to_string(),
        "    core.mem.drop(match d.parts.push(t.greeting(\"more\")) {".to_string(),
        "        Ok(_) => 0u8".to_string(),
        "        Err(_) => 1u8".to_string(),
        "    })".to_string(),
        "    core.check.eq(d.parts.len(), 4usize)".to_string(),
        "    d.name = t.greeting(\"renamed\")".to_string(),
        "    core.check.eq(d.name.as_str(), \"hello, renamed\")".to_string(),
        "    d.parts.clear()".to_string(),
        "    core.check.eq(d.parts.len(), 0usize)".to_string(),
        "    core.check.eq(t.early(true), 12usize)".to_string(),
        "    core.check.eq(t.early(false), 23usize)".to_string(),
        "    core.check.eq(t.looping(), 25u32)".to_string(),
        "    let moved = g".to_string(),
        "    core.check.eq(moved.len(), 12usize)".to_string(),
        "    let r = alloc.string.String.from_str(std.alloc.SYSTEM, \"abc\")".to_string(),
        "    match r {".to_string(),
        "        Ok(s) => core.check.eq(s.as_str(), \"abc\")".to_string(),
        "        Err(_) => core.check.eq(1u8, 2u8)".to_string(),
        "    }".to_string(),
        "    core.mem.drop(docs)".to_string(),
        tail.to_string(),
        "    core.mem.drop(std.io.print_line(\"string ok\"))".to_string(),
        "    Ok(())".to_string(),
        "end t.main".to_string(),
        String::new(),
    ]
    .join(&nl)
}

pub fn clif_slice_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.dot(a: &[u32], b: &[u32]) -> u32",
        "    let mut acc = 0u32",
        "    for i in core.iter.range(0usize, a.len()) {",
        "        acc = acc.wrapping_add(a[i].wrapping_mul(b[i]))",
        "    }",
        "    acc",
        "end t.dot",
        "",
        "fn t.first_byte(text: &str) -> u8",
        "    match text.as_bytes().get(0usize) {",
        "        Some(b) => *b",
        "        None => 0u8",
        "    }",
        "end t.first_byte",
        "",
        "fn t.fresh() -> alloc.vec.Vec<u32, std.alloc.System>",
        "    alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "end t.fresh",
        "",
        "fn t.index_sum(s: &[u32]) -> usize",
        "    let mut acc = 0usize",
        "    for (i, _x) in core.iter.enumerate(s.iter()) {",
        "        acc = acc.wrapping_add(i)",
        "    }",
        "    acc",
        "end t.index_sum",
        "",
        "fn t.make(n: u32) -> alloc.vec.Vec<u32, std.alloc.System>",
        "    let mut v = t.fresh()",
        "    for k in core.iter.range(1u32, n.wrapping_add(1u32)) {",
        "        core.mem.drop(match v.push(k) {",
        "            Ok(_) => 0u8",
        "            Err(_) => 1u8",
        "        })",
        "    }",
        "    v",
        "end t.make",
        "",
        "fn t.pick(s: &[u32], i: usize) -> u32",
        "    match s.get(i) {",
        "        Some(x) => *x",
        "        None => 999u32",
        "    }",
        "end t.pick",
        "",
        "fn t.total(v: &alloc.vec.Vec<u32, std.alloc.System>) -> u32",
        "    let mut acc = 0u32",
        "    for x in v.iter() {",
        "        if *x == 3u32 {",
        "            continue",
        "        }",
        "        acc = acc.wrapping_add(*x)",
        "    }",
        "    acc",
        "end t.total",
        "",
        "fn t.zero_first(s: &mut [u32]) -> ()",
        "    s[0usize] = 0u32",
        "end t.zero_first",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let v = t.make(4u32)",
        "    let s = v.as_slice()",
        "    core.check.eq(s.len(), 4usize)",
        "    core.check.eq(t.dot(s, s), 30u32)",
        "    core.check.eq(t.total(&v), 7u32)",
        "    core.check.eq(t.index_sum(s), 6usize)",
        "    core.check.eq(t.pick(s, 2usize), 3u32)",
        "    core.check.eq(t.pick(s, 9usize), 999u32)",
        "    core.check.eq(s.get(1usize), Some(&2u32))",
        "    core.check.eq(s.get(7usize), None)",
        "    core.check.eq(s.first(), Some(&1u32))",
        "    core.check.eq(s.last(), Some(&4u32))",
        "    match s.sub(1usize, 3usize) {",
        "        Some(part) => core.check.eq(part.len(), 2usize)",
        "        None => core.check.eq(1u8, 2u8)",
        "    }",
        "    match s.sub(3usize, 9usize) {",
        "        Some(_) => core.check.eq(1u8, 2u8)",
        "        None => core.check.eq(1u8, 1u8)",
        "    }",
        "    let mut w = t.make(3u32)",
        "    t.zero_first(w.as_mut_slice())",
        "    core.check.eq(w[0usize], 0u32)",
        "    core.check.eq(w[2usize], 3u32)",
        "    core.check.eq(t.first_byte(\"A\"), 65u8)",
        tail,
        "    core.mem.drop(std.io.print_line(\"slice ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

pub fn clif_tuple_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "enum t.Kind derive [Debug, Clone, Copy, PartialEq]",
        "    Dot",
        "    Line(u32)",
        "    Box(t.P, bool)",
        "end t.Kind",
        "",
        "type t.P derive [Debug, Clone, Copy, PartialEq, Eq]",
        "    x: i32",
        "    y: i32",
        "end t.P",
        "",
        "type t.Seg derive [Debug, Clone, Copy, PartialEq]",
        "    a: t.P",
        "    b: t.P",
        "    tag: u8",
        "end t.Seg",
        "",
        "fn t.divmod(a: u32, b: u32) -> (u32, u32)",
        "    (a / b, a % b)",
        "end t.divmod",
        "",
        "fn t.named(n: u32) -> (u32, alloc.string.String<std.alloc.System>)",
        "    let mut s = alloc.string.String.new(std.alloc.SYSTEM)",
        "    core.mem.drop(match s.push_str(\"n\") {",
        "        Ok(_) => 0u8",
        "        Err(_) => 1u8",
        "    })",
        "    (n, s)",
        "end t.named",
        "",
        "fn t.swap(p: (u32, bool)) -> (bool, u32)",
        "    let (n, flag) = p",
        "    (flag, n)",
        "end t.swap",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let p = t.P { x: 1i32, y: 2i32 }",
        "    let q = t.P { x: 1i32, y: 2i32 }",
        "    core.check.eq(p, q)",
        "    core.check.eq(p == t.P { x: 1i32, y: 3i32 }, false)",
        "    let s1 = t.Seg { a: p, b: q, tag: 1u8 }",
        "    let s2 = t.Seg { a: q, b: p, tag: 1u8 }",
        "    core.check.eq(s1, s2)",
        "    core.check.eq(s1 != t.Seg { a: p, b: q, tag: 2u8 }, true)",
        "    core.check.eq(t.Kind.Line(3u32), t.Kind.Line(3u32))",
        "    core.check.eq(t.Kind.Line(3u32) == t.Kind.Line(4u32), false)",
        "    core.check.eq(t.Kind.Dot == t.Kind.Line(0u32), false)",
        "    core.check.eq(t.Kind.Dot, t.Kind.Dot)",
        "    core.check.eq(t.Kind.Box(p, true), t.Kind.Box(q, true))",
        "    core.check.eq(t.Kind.Box(p, true) == t.Kind.Box(q, false), false)",
        "    core.check.eq(t.divmod(17u32, 5u32), (3u32, 2u32))",
        "    core.check.eq(t.swap((9u32, true)), (true, 9u32))",
        "    core.check.eq(Some(p), Some(q))",
        "    core.check.eq(\"ab\" == \"ab\", true)",
        "    core.check.eq(\"ab\" == \"ac\", false)",
        "    match t.divmod(20u32, 6u32) {",
        "        (d, m) => core.check.eq(d.wrapping_add(m), 5u32)",
        "    }",
        "    let (k, name) = t.named(7u32)",
        "    core.check.eq(k, 7u32)",
        "    core.check.eq(name.as_str(), \"n\")",
        tail,
        "    core.mem.drop(std.io.print_line(\"tuple ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

pub fn clif_methods_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "enum t.Phase derive [Debug, Clone, Copy, PartialEq]",
        "    Idle",
        "    Run",
        "end t.Phase",
        "",
        "fn t.Phase.next(self: t.Phase) -> t.Phase uses [] panics never",
        "    match self {",
        "        t.Phase.Idle => t.Phase.Run",
        "        t.Phase.Run => t.Phase.Idle",
        "    }",
        "end t.Phase.next",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    core.check.eq(250u8.checked_add(5u8), Some(255u8))",
        "    core.check.eq(250u8.checked_add(6u8), None)",
        "    core.check.eq(3u32.checked_sub(4u32), None)",
        "    core.check.eq(9u32.checked_rem(4u32).unwrap_or(77u32), 1u32)",
        "    core.check.eq(9u32.checked_div(0u32).unwrap_or(77u32), 77u32)",
        "    core.check.eq(0i32.checked_sub(5i32), Some(0i32 - 5i32))",
        "    core.check.eq(250u8.saturating_add(10u8), 255u8)",
        "    core.check.eq(3u8.saturating_sub(9u8), 0u8)",
        "    core.check.eq(Some(4u32).is_some(), true)",
        "    core.check.eq(Some(4u32).unwrap(), 4u32)",
        "    core.check.eq(t.Phase.Idle.next(), t.Phase.Run)",
        "    core.check.eq(t.Phase.Idle.next().next(), t.Phase.Idle)",
        tail,
        "    core.mem.drop(std.io.print_line(\"methods ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

pub fn clif_mutref_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "type t.P",
        "    x: u32",
        "    y: u32",
        "end t.P",
        "",
        "fn t.bump(r: &mut u32) -> ()",
        "    *r = r.wrapping_add(1u32)",
        "end t.bump",
        "",
        "fn t.double_all(v: &mut [u32]) -> ()",
        "    for i in core.iter.range(0usize, v.len()) {",
        "        v[i] = v[i].wrapping_mul(2u32)",
        "    }",
        "end t.double_all",
        "",
        "fn t.fresh() -> alloc.vec.Vec<u32, std.alloc.System>",
        "    alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "end t.fresh",
        "",
        "fn t.make(n: u32) -> alloc.vec.Vec<u32, std.alloc.System>",
        "    let mut v = t.fresh()",
        "    for k in core.iter.range(1u32, n.wrapping_add(1u32)) {",
        "        core.mem.drop(match v.push(k) {",
        "            Ok(_) => 0u8",
        "            Err(_) => 1u8",
        "        })",
        "    }",
        "    v",
        "end t.make",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let mut n = 5u32",
        "    t.bump(&mut n)",
        "    t.bump(&mut n)",
        "    core.check.eq(n, 7u32)",
        "    let mut p = t.P { x: 1u32, y: 2u32 }",
        "    t.bump(&mut p.x)",
        "    core.check.eq(p.x, 2u32)",
        "    core.check.eq(p.y, 2u32)",
        "    let mut v = t.make(3u32)",
        "    t.double_all(v.as_mut_slice())",
        "    core.check.eq(v[1usize], 4u32)",
        "    match v.get_mut(1usize) {",
        "        Some(slot) => t.bump(slot)",
        "        None => core.check.eq(1u8, 2u8)",
        "    }",
        "    core.check.eq(v[1usize], 5u32)",
        "    t.bump(&mut v[0usize])",
        "    core.check.eq(v[0usize], 3u32)",
        tail,
        "    core.mem.drop(std.io.print_line(\"mutref ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

pub fn clif_try_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.collect(n: u32) -> alloc.vec.Vec<u32, std.alloc.System>",
        "    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "    for k in core.iter.range(1u32, n.wrapping_add(1u32)) {",
        "        match v.push(k.wrapping_mul(k)) {",
        "            Ok(_u) => {",
        "            }",
        "            Err(_e) => {",
        "                break",
        "            }",
        "        }",
        "    }",
        "    v",
        "end t.collect",
        "",
        "fn t.parse(n: u32) -> Result<u32, u8>",
        "    if n == 0u32 {",
        "        return Err(7u8)",
        "    }",
        "    Ok(n.wrapping_mul(2u32))",
        "end t.parse",
        "",
        "fn t.positive(n: u32) -> Option<u32>",
        "    if n == 0u32 {",
        "        return None",
        "    }",
        "    Some(n)",
        "end t.positive",
        "",
        "fn t.sum_two(a: u32, b: u32) -> Option<u32>",
        "    let x = t.positive(a)?",
        "    let y = t.positive(b)?",
        "    x.checked_add(y)",
        "end t.sum_two",
        "",
        "fn t.twice(n: u32) -> Result<u32, u8>",
        "    let a = t.parse(n)?",
        "    let b = t.parse(a)?",
        "    Ok(b)",
        "end t.twice",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    core.check.eq(t.twice(3u32), Ok(12u32))",
        "    core.check.eq(t.twice(0u32), Err(7u8))",
        "    core.check.eq(t.sum_two(2u32, 5u32), Some(7u32))",
        "    core.check.eq(t.sum_two(0u32, 5u32), None)",
        "    core.check.eq(t.sum_two(4u32, 0u32), None)",
        "    let v = t.collect(4u32)",
        "    core.check.eq(v.len(), 4usize)",
        "    core.check.eq(v[3usize], 16u32)",
        tail,
        "    core.mem.drop(std.io.print_line(\"try ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

pub fn clif_closure_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.evens_sum(limit: i32) -> i32",
        "    core.iter.fold(",
        "        core.iter.map(",
        "            core.iter.filter(",
        "                core.iter.range(0i32, limit),",
        "                fn(x: &i32) -> bool capture [] {",
        "                    *x % 2i32 == 0i32",
        "                },",
        "            ),",
        "            fn(x: i32) -> i32 capture [] {",
        "                x.wrapping_mul(10i32)",
        "            },",
        "        ),",
        "        0i32,",
        "        fn(a: i32, x: i32) -> i32 capture [] {",
        "            a.wrapping_add(x)",
        "        },",
        "    )",
        "end t.evens_sum",
        "",
        "fn t.fresh() -> alloc.vec.Vec<i32, std.alloc.System>",
        "    alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "end t.fresh",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    core.check.eq(t.evens_sum(7i32), 120i32)",
        "    let twice = fn(n: i32) -> i32 capture [] {",
        "        n.wrapping_mul(2i32)",
        "    }",
        "    core.check.eq(twice(21i32), 42i32)",
        "    let base = 100i32",
        "    let add_base = fn(n: i32) -> i32 capture [&base] {",
        "        n.wrapping_add(*base)",
        "    }",
        "    core.check.eq(add_base(5i32), 105i32)",
        "    let mut v = t.fresh()",
        "    for k in core.iter.range(1i32, 6i32) {",
        "        core.mem.drop(match v.push(k) {",
        "            Ok(_u) => 0u8",
        "            Err(_e) => 1u8",
        "        })",
        "    }",
        "    core.check.eq(core.iter.count(v.iter()), 5usize)",
        "    core.check.eq(core.iter.count(core.iter.take(v.iter(), 3usize)), 3usize)",
        "    let total = core.iter.fold(",
        "        core.iter.take(v.iter(), 3usize),",
        "        0i32,",
        "        fn(a: i32, x: &i32) -> i32 capture [] {",
        "            a.wrapping_add(*x)",
        "        },",
        "    )",
        "    core.check.eq(total, 6i32)",
        "    let mut seen = 0usize",
        "    let mut sum = 0i32",
        "    for (i, x) in core.iter.enumerate(core.iter.take(v.iter(), 2usize)) {",
        "        core.check.eq(i, seen)",
        "        sum = sum.wrapping_add(*x)",
        "        seen = seen.wrapping_add(1usize)",
        "    }",
        "    core.check.eq(seen, 2usize)",
        "    core.check.eq(sum, 3i32)",
        tail,
        "    core.mem.drop(std.io.print_line(\"closure ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

pub fn clif_iterator_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "type t.Upto",
        "    cur: u32",
        "    stop: u32",
        "end t.Upto",
        "",
        "impl core.iter.Iterator<u32, [], never> for t.Upto",
        "fn t.Upto.next(self: &mut t.Upto) -> Option<u32> uses [] panics never impls core.iter.Iterator<u32, [], never>",
        "    if self.cur >= self.stop {",
        "        return None",
        "    }",
        "    let v = self.cur",
        "    self.cur = self.cur.wrapping_add(1u32)",
        "    Some(v)",
        "end t.Upto.next",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let mut it = t.Upto { cur: 0u32, stop: 4u32 }",
        "    let mut acc = 0u32",
        "    for n in it {",
        "        acc = acc.wrapping_add(n)",
        "    }",
        "    core.check.eq(acc, 6u32)",
        "    core.check.eq(it.cur, 4u32)",
        "    let total = core.iter.fold(",
        "        core.iter.map(",
        "            core.iter.filter(",
        "                t.Upto { cur: 0u32, stop: 10u32 },",
        "                fn(x: &u32) -> bool capture [] {",
        "                    *x > 2u32",
        "                },",
        "            ),",
        "            fn(x: u32) -> u32 capture [] {",
        "                x.wrapping_mul(2u32)",
        "            },",
        "        ),",
        "        0u32,",
        "        fn(a: u32, x: u32) -> u32 capture [] {",
        "            a.wrapping_add(x)",
        "        },",
        "    )",
        "    core.check.eq(total, 84u32)",
        "    let long = t.Upto { cur: 5u32, stop: 100u32 }",
        "    core.check.eq(core.iter.count(core.iter.take(long, 3usize)), 3usize)",
        tail,
        "    core.mem.drop(std.io.print_line(\"iterator ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

pub fn clif_std_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.fresh() -> alloc.vec.Vec<u8, std.alloc.System>",
        "    alloc.vec.Vec.new(std.alloc.SYSTEM)",
        "end t.fresh",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let arr = [1u8, 2u8, 3u8, 4u8, 5u8]",
        "    core.check.eq(arr.len(), 5usize)",
        "    core.check.eq(arr.get(7usize), None)",
        "    core.check.eq(arr.first(), Some(&1u8))",
        "    core.check.eq(arr.last(), Some(&5u8))",
        "    core.check.eq(arr.contains(&3u8), true)",
        "    core.check.eq(arr.contains(&9u8), false)",
        "    let mid = arr.sub(1usize, 4usize)",
        "    core.check.eq(mid, Some(&[2u8, 3u8, 4u8]))",
        "    let mut opt = Some(5u8)",
        "    let taken = opt.take()",
        "    core.check.eq(taken, Some(5u8))",
        "    core.check.eq(opt, None)",
        "    let mut v = t.fresh()",
        "    for k in core.iter.range(1u8, 4u8) {",
        "        core.mem.drop(match v.push(k.wrapping_mul(10u8)) {",
        "            Ok(_u) => 0u8",
        "            Err(_e) => 1u8",
        "        })",
        "    }",
        "    core.check.eq(v.remove(0usize), Some(10u8))",
        "    core.check.eq(v.len(), 2usize)",
        "    core.check.eq(v[0usize], 20u8)",
        "    core.check.eq(v.remove(9usize), None)",
        "    core.check.eq(u64.from_u32(7u32), 7u64)",
        "    core.check.eq(i32.try_from_i64(5i64), Ok(5i32))",
        "    core.check.eq(u8.try_from_i32(300i32).is_err(), true)",
        "    core.check.eq(u8.try_from_i32(0i32 - 1i32).is_err(), true)",
        "    core.check.eq(u8.try_from_u64(255u64), Ok(255u8))",
        "    core.check.eq(i8.try_from_i32(200i32).is_err(), true)",
        "    core.check.eq(i8.try_from_i32(0i32 - 128i32).is_ok(), true)",
        "    let c = 'q'",
        "    core.check.eq(c.to_ascii_uppercase(), 'Q')",
        "    core.check.eq(c.to_u32(), 113u32)",
        "    core.check.eq(char.try_from_u32(65u32), Some('A'))",
        "    core.check.eq(char.try_from_u32(55296u32), None)",
        "    core.check.eq('7'.is_ascii_digit(), true)",
        "    core.check.eq('a'.is_ascii_alphabetic(), true)",
        "    core.check.eq(' '.is_ascii_whitespace(), true)",
        "    core.check.eq('x'.is_ascii_whitespace(), false)",
        "    core.check.eq(3i32.to_f64(), 3.0f64)",
        tail,
        "    core.mem.drop(std.io.print_line(\"std ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

pub fn clif_map_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may",
        "    let mut m = alloc.collections.HashMap.new(std.alloc.SYSTEM)",
        "    core.mem.drop(match m.insert(\"a\", 1u8) {",
        "        Ok(_old) => 0u8",
        "        Err(_e) => 1u8",
        "    })",
        "    core.mem.drop(match m.insert(\"b\", 2u8) {",
        "        Ok(_old) => 0u8",
        "        Err(_e) => 1u8",
        "    })",
        "    core.check.eq(m.get(&\"a\"), Some(&1u8))",
        "    core.check.eq(m.get(&\"zz\"), None)",
        "    core.check.eq(m.len(), 2usize)",
        "    core.check.eq(m.insert(\"a\", 5u8), Ok(Some(1u8)))",
        "    core.check.eq(m.get(&\"a\"), Some(&5u8))",
        "    core.check.eq(m.len(), 2usize)",
        "    core.check.eq(m.contains_key(&\"b\"), true)",
        "    core.check.eq(m.remove(&\"b\"), Some(2u8))",
        "    core.check.eq(m.contains_key(&\"b\"), false)",
        "    core.check.eq(m.is_empty(), false)",
        "    let mut b = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)",
        "    let i1 = b.insert(3u8, 30u8)",
        "    let i2 = b.insert(1u8, 10u8)",
        "    let i3 = b.insert(3u8, 33u8)",
        "    core.check.eq(i1, Ok(None))",
        "    core.check.eq(i2, Ok(None))",
        "    core.check.eq(i3, Ok(Some(30u8)))",
        "    core.check.eq(b.len(), 2usize)",
        "    core.check.eq(b.get(&3u8), Some(&33u8))",
        "    core.check.eq(b.contains_key(&2u8), false)",
        "    core.check.eq(b.remove(&1u8), Some(10u8))",
        "    core.check.eq(b.len(), 1usize)",
        "    let mut big = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)",
        "    for k in core.iter.range(0u32, 50u32) {",
        "        let key = k.wrapping_mul(7u32) % 50u32",
        "        core.mem.drop(match big.insert(key, k) {",
        "            Ok(_old) => 0u8",
        "            Err(_e) => 1u8",
        "        })",
        "    }",
        "    core.check.eq(big.len(), 50usize)",
        "    core.check.eq(big.get(&21u32), Some(&3u32))",
        tail,
        "    core.mem.drop(std.io.print_line(\"map ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

pub fn clif_generic_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "enum t.Maybe[T] derive [Debug, Clone, Copy, PartialEq]",
        "    Nothing",
        "    Just(T)",
        "end t.Maybe",
        "",
        "type t.Pair[A, B] derive [Debug, Clone, Copy, PartialEq]",
        "    first: A",
        "    second: B",
        "end t.Pair",
        "",
        "fn t.Maybe.or_else[T](self: t.Maybe<T>, other: T) -> T",
        "    match self {",
        "        t.Maybe.Just(v) => v",
        "        t.Maybe.Nothing => other",
        "    }",
        "end t.Maybe.or_else",
        "",
        "fn t.Pair.swap[A, B](self: t.Pair<A, B>) -> t.Pair<B, A>",
        "    t.Pair { first: self.second, second: self.first }",
        "end t.Pair.swap",
        "",
        "fn t.pick[T](flag: bool, a: T, b: T) -> T",
        "    if flag {",
        "        return a",
        "    }",
        "    b",
        "end t.pick",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let p = t.Pair { first: 1u32, second: true }",
        "    let q = p.swap()",
        "    core.check.eq(q.first, true)",
        "    core.check.eq(q.second, 1u32)",
        "    core.check.eq(q, t.Pair { first: true, second: 1u32 })",
        "    core.check.eq(t.pick(true, 3u8, 4u8), 3u8)",
        "    core.check.eq(t.pick(false, 3i64, 4i64), 4i64)",
        "    core.check.eq(t.pick(false, p, p.swap().swap()), p)",
        "    let m = t.Maybe.Just(5u32)",
        "    core.check.eq(m.or_else(9u32), 5u32)",
        "    let n: t.Maybe<u32> = t.Maybe.Nothing",
        "    core.check.eq(n.or_else(9u32), 9u32)",
        "    let nested = t.Pair { first: t.Maybe.Just(2u8), second: p }",
        "    core.check.eq(nested.second.first, 1u32)",
        "    core.check.eq(nested.first.or_else(0u8), 2u8)",
        tail,
        "    core.mem.drop(std.io.print_line(\"generic ok\"))",
        "    Ok(())",
        "end t.main",
        "",
    ]
    .join(&nl)
}

pub fn clif_fn_bound_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "fn t.apply[F: core.ops.Fn<(u32,), u32, E, P>, E: uses, P: panics](f: F, x: u32) -> u32 uses [E] panics P",
        "    f(x)",
        "end t.apply",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let triple = fn(n: u32) -> u32 capture [] {",
        "        n.wrapping_mul(3u32)",
        "    }",
        "    core.check.eq(t.apply(triple, 14u32), 42u32)",
        "    let add_one = fn(n: u32) -> u32 capture [] {",
        "        n.wrapping_add(1u32)",
        "    }",
        "    core.check.eq(t.relay(add_one, 5u32), 6u32)",
        "    let square = fn(n: u32) -> u32 capture [] {",
        "        n.wrapping_mul(n)",
        "    }",
        "    core.check.eq(t.twice(square, 3u32), 81u32)",
        "    let big = fn(n: u8) -> bool capture [] {",
        "        n > 5u8",
        "    }",
        "    core.check.eq(t.map_one(7u8, big), true)",
        "    let small = fn(n: u8) -> bool capture [] {",
        "        n > 5u8",
        "    }",
        "    core.check.eq(t.map_one(3u8, small), false)",
        "    let wide = fn(n: i32) -> i64 capture [] {",
        "        10i64",
        "    }",
        "    core.check.eq(t.map_one(2i32, wide), 10i64)",
        tail,
        "    core.mem.drop(std.io.print_line(\"fn bound ok\"))",
        "    Ok(())",
        "end t.main",
        "",
        "fn t.map_one[T, U, F: core.ops.Fn<(T,), U, E, P>, E: uses, P: panics](x: T, f: F) -> U uses [E] panics P",
        "    f(x)",
        "end t.map_one",
        "",
        "fn t.relay[F: core.ops.Fn<(u32,), u32, E, P>, E: uses, P: panics](f: F, x: u32) -> u32 uses [E] panics P",
        "    t.apply(f, x)",
        "end t.relay",
        "",
        "fn t.twice[F: core.ops.Fn<(u32,), u32, E, P>, E: uses, P: panics](f: F, x: u32) -> u32 uses [E] panics P",
        "    f(f(x))",
        "end t.twice",
        "",
    ]
    .join(&nl)
}

// ---- 構造体・コンテナに保存された借用 (領域の近似) ----

pub const NLL_REF_FIELD: &str = "type t.S['a]\n    f: &'a u8\n    b: u8\nend t.S\n\n";

pub fn clif_capture_program(tail: &str) -> String {
    let nl = char::from(10u8).to_string();
    [
        "type t.P",
        "    a: u32",
        "end t.P",
        "",
        "fn t.apply[F: core.ops.Fn<(u32,), u32, E, P>, E: uses, P: panics](f: F, x: u32) -> u32 uses [E] panics P",
        "    f(x)",
        "end t.apply",
        "",
        "fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may",
        "    let base = 100u32",
        "    let add_base = fn(n: u32) -> u32 capture [&base] {",
        "        n.wrapping_add(*base)",
        "    }",
        "    core.check.eq(t.apply(add_base, 5u32), 105u32)",
        "    let k = 7u32",
        "    let times = fn(n: u32) -> u32 capture [&k] {",
        "        n.wrapping_mul(*k)",
        "    }",
        "    core.check.eq(t.relay(times, 3u32), 21u32)",
        "    let mut total = 0u32",
        "    let acc = fn(n: u32) -> () capture [&mut total] {",
        "        *total = (*total).wrapping_add(n)",
        "    }",
        "    t.run_all(acc)",
        "    core.check.eq(total, 6u32)",
        "    let p = t.P { a: 9u32 }",
        "    let read_a = fn(n: u32) -> u32 capture [&p] {",
        "        (*p).a.wrapping_add(n)",
        "    }",
        "    core.check.eq(t.apply(read_a, 1u32), 10u32)",
        "    let two = 2u32",
        "    let three = 3u32",
        "    let both = fn(n: u32) -> u32 capture [&three, &two] {",
        "        n.wrapping_mul(*two).wrapping_add(*three)",
        "    }",
        "    core.check.eq(t.apply(both, 10u32), 23u32)",
        tail,
        "    core.mem.drop(std.io.print_line(\"capture ok\"))",
        "    Ok(())",
        "end t.main",
        "",
        "fn t.relay[F: core.ops.Fn<(u32,), u32, E, P>, E: uses, P: panics](f: F, x: u32) -> u32 uses [E] panics P",
        "    t.apply(f, x)",
        "end t.relay",
        "",
        "fn t.run_all[F: core.ops.FnMut<(u32,), (), E, P>, E: uses, P: panics](f: F) -> () uses [E] panics P",
        "    f(1u32)",
        "    f(2u32)",
        "    f(3u32)",
        "end t.run_all",
        "",
    ]
    .join(&nl)
}

pub const FFI_SRC: &str = "extern t.abs(x: i32) -> i32 link \"abs\"

extern t.free(pointer: *mut u8) -> () link \"free\"

extern t.malloc(size: usize) -> *mut u8 link \"malloc\"

extern t.strlen(text: *const u8) -> usize link \"strlen\"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let a = unsafe \"libc\" \"abs of a literal\" {
        t.abs(0i32 - 5i32)
    }
    core.check.eq(a, 5i32)
    let p = unsafe \"libc\" \"allocate\" {
        t.malloc(16usize)
    }
    unsafe \"libc\" \"write and read back\" {
        core.ptr.write(p, 7u8)
        core.ptr.write(core.ptr.add(p, 1usize), 0u8)
        core.check.eq(core.ptr.read(p), 7u8)
        core.check.eq(t.strlen(core.ptr.as_const(p)), 1usize)
        t.free(p)
    }
    core.mem.drop(std.io.print_line(\"ffi ok\"))
    Ok(())
end t.main
";

/// `cc` (無ければ `gcc` / `clang`) でコンパイルする。
#[cfg(unix)]
pub fn cc(args: &[&str], dir: &Path) -> Output {
    let has = |c: &str| Command::new(c).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    let cc = ["cc", "gcc", "clang"].iter().find(|c| has(c)).expect("no C compiler (cc / gcc / clang)");
    Command::new(cc).args(args).current_dir(dir).output().expect("spawn cc")
}

// ---- std.fs / std.net / 参照を返す関数 (LLVM) ----

/// プログラムをインタプリタと LLVM (debug / release) で、それぞれ一時ディレクトリの中で動かして、
/// 標準出力と終了コードが一致することを確かめる。インタプリタの標準出力を返す。
/// LLVM に下ろせなければ理由付きでバンドルへ退避するので、そのときはバンドルの出力と比べる。
pub fn agree_with_interpreter(name: &str, src: &str) -> String {
    agree_with_interpreter_masking(name, src, "")
}

/// `agree_with_interpreter` のうち、`mask` を含む行は一致を求めない (空文字列なら全行を比べる)。
pub fn agree_with_interpreter_masking(name: &str, src: &str, mask: &str) -> String {
    let t = Tmp::new(&format!("{}_interp", name), src);
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}{}", text(&interp.stdout), text(&interp.stderr));
    let keep = |s: String| -> String {
        s.lines().filter(|l| mask.is_empty() || !l.contains(mask)).map(|l| format!("{}\n", l)).collect()
    };
    for release in [false, true] {
        let (bt, exe_path, out) = build_native(&format!("{}_{}", name, release), src, release);
        assert_native_or_reasoned_skip(&out);
        let p = exe_path.unwrap_or_else(|| panic!("no native executable: {}", out));
        let o = Command::new(&p).current_dir(&bt.0).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
        assert_eq!(keep(text(&o.stdout)), keep(text(&interp.stdout)), "native and interpreter outputs differ (release: {})", release);
    }
    text(&interp.stdout)
}

// ---- 3.3: 名前の参照形。`check` はどちらの形も受け入れ、`fmt` が正規形にする。

pub const SN_LIB: &str = "type t.lib.Item pub\n    n: u8 pub\nend t.lib.Item\n\nfn t.lib.make() -> t.lib.Item uses [] panics never pub\n    t.lib.Item { n: 1u8 }\nend t.lib.make\n";

pub const SN_CANON: &str = "import t.lib\n\nfn t.get(x: &t.lib.Item) -> u8 uses [] panics never\n    x.n\nend t.get\n\nfn t.main() -> Result<(), std.io.Error> uses [] panics never\n    let item = lib.make()\n    let n = t.get(&item)\n    core.mem.drop(n)\n    Ok(())\nend t.main\n";

pub fn sn_pkg(name: &str, src: &str) -> Tmp {
    // 名前の参照形 (3.3) の試験: `fmt` を通さない入力を使う。
    let t = Tmp::new_raw(name, src);
    fs::create_dir_all(t.0.join("src/t")).unwrap();
    fs::write(t.0.join("src/t/lib.ksnl"), SN_LIB).unwrap();
    t
}

// ---- `with` (4.4): 範囲を抜けるすべての経路で後始末式が 1 回だけ走る ----

/// 後始末の順序・回数を標準出力に出す、`with` の全経路 (末尾・`return`・`?`・`break` / `continue`・入れ子・
/// 初期化の失敗) を通るプログラム。
pub const WITH_PROGRAM: &str = r#"type t.R
    id: u32
end t.R

fn t.R.get(self: &t.R) -> u32 uses [] panics never
    self.id
end t.R.get

fn t.R.new(id: u32) -> t.R uses [io.console] panics never
    core.mem.drop(if id == 1u32 {
        std.io.print_line("open 1")
    } else {
        if id == 2u32 {
            std.io.print_line("open 2")
        } else {
            std.io.print_line("open")
        }
    })
    t.R { id: id }
end t.R.new

fn t.R.open(id: u32) -> Result<t.R, u8> uses [io.console] panics never
    if id == 0u32 {
        return Err(9u8)
    }
    Ok(t.R.new(id))
end t.R.open

fn t.R.shut(self: t.R) -> () uses [io.console] panics never
    core.mem.drop(if self.id == 1u32 {
        std.io.print_line("shut 1")
    } else {
        if self.id == 2u32 {
            std.io.print_line("shut 2")
        } else {
            std.io.print_line("shut")
        }
    })
    core.mem.drop(self.id)
end t.R.shut

fn t.early_return(n: u32) -> u32 uses [io.console] panics never
    with r = t.R.new(1u32) close r.shut() {
        if n == 0u32 {
            return 100u32
        }
        r.get().wrapping_add(n)
    }
end t.early_return

fn t.fail(n: u32) -> Result<u32, u8> uses [] panics never
    if n == 0u32 {
        return Err(7u8)
    }
    Ok(n)
end t.fail

fn t.in_loops(n: u32) -> u32 uses [io.console] panics never
    with r = t.R.new(1u32) close r.shut() {
        for i in core.iter.range(0u32, 5u32) {
            let mut j = 0u32
            while j < 5u32 {
                if i == n {
                    return i.wrapping_mul(10u32).wrapping_add(j)
                }
                j = j.wrapping_add(1u32)
            }
        }
        r.get()
    }
end t.in_loops

fn t.loop_exits() -> u32 uses [io.console] panics never
    let mut total = 0u32
    for i in core.iter.range(0u32, 5u32) {
        with r = t.R.new(i.wrapping_add(1u32)) close r.shut() {
            if i == 1u32 {
                continue
            }
            if i == 3u32 {
                break
            }
            total = total.wrapping_add(r.get())
        }
    }
    total
end t.loop_exits

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    core.check.eq(t.early_return(0u32), 100u32)
    core.check.eq(t.early_return(2u32), 3u32)
    core.check.eq(t.in_loops(2u32), 20u32)
    core.check.eq(t.in_loops(9u32), 1u32)
    core.check.eq(t.nested(5u32), Ok(8u32))
    core.check.eq(t.nested(0u32), Err(7u8))
    core.check.eq(t.tail_unit(0u32), Err(9u8))
    core.check.eq(t.tail_unit(4u32), Ok(()))
    core.check.eq(t.loop_exits(), 4u32)
    Ok(())
end t.main

fn t.nested(n: u32) -> Result<u32, u8> uses [io.console] panics never
    let v = with a = t.R.open(1u32)? close a.shut() {
        let w = with b = t.R.open(2u32)? close b.shut() {
            let c = t.fail(n)?
            c.wrapping_add(b.get())
        }
        w.wrapping_add(a.get())
    }
    Ok(v)
end t.nested

fn t.tail_unit(n: u32) -> Result<(), u8> uses [io.console] panics never
    with r = t.R.open(n)? close r.shut() {
        core.mem.drop(r.get())
        Ok(())
    }
end t.tail_unit
"#;

// ---------------------------------------------------------------- Box<dyn Tr> (5.1)

/// `tests/data/box_dyn.ksnl` の `CHURN_N` (箱を作って捨てる回数) を決めたソース。
pub fn box_dyn_source(churn: u32) -> String {
    include_str!("../data/box_dyn.ksnl").replace("CHURN_NI", &format!("{}i64", churn)).replace("CHURN_N", &format!("{}u32", churn))
}

// ---- FFI の完成 (コールバック・`layout c` の構造体の値渡し・多数の引数・動的ロード。3.8)

#[cfg(unix)]
pub fn ffi_data(name: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/ffi").join(name)).unwrap()
}

/// `tests/data/ffi/ffi_lib.c` をこのテスト用の共有ライブラリにして、`LIB:` をそのパスに置き換えた
/// Kasane のソースで一時パッケージ (`[unsafe] modules = ["t"]`) を作る。
#[cfg(unix)]
pub fn ffi_pkg(name: &str, data: &str) -> Tmp {
    let t = Tmp::new(name, "");
    fs::write(t.0.join("kasane.toml"), TOML.replacen("[dependencies]", "[unsafe]\nmodules = [\"t\"]\n\n[dependencies]", 1)).unwrap();
    let c = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/ffi/ffi_lib.c");
    let lib = t.0.join("libffitest.so");
    let o = cc(&["-shared", "-fPIC", "-o", lib.to_str().unwrap(), c.to_str().unwrap()], &t.0);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let src = ffi_data(data).replace("LIB:", &format!("{}:", lib.to_str().unwrap()));
    fs::write(t.0.join("src").join("t.ksnl"), src).unwrap();
    t
}

/// ネイティブ生成してから実行する。(ビルドの出力, 実行結果)。スキップされたら実行結果は None。
#[cfg(unix)]
pub fn ffi_native(t: &Tmp, name: &str) -> (String, Option<Output>) {
    let cache = std::env::temp_dir().join(format!("ksn_cache_{}_{}", std::process::id(), name));
    let b = Command::new(exe()).args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).output().unwrap();
    let _ = fs::remove_dir_all(&cache);
    let out = text(&b.stdout) + &text(&b.stderr);
    let bin = t.0.join("target").join("debug").join("t.native");
    if !bin.exists() {
        return (out, None);
    }
    (out, Some(Command::new(&bin).current_dir(&t.0).output().unwrap()))
}

/// clang が使えるか (使えなければ、`--native` の比較は飛ばして理由を一度だけ出す)。
pub fn clang_available() -> bool {
    use std::sync::OnceLock;
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let ok = Command::new("clang").arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
        if !ok {
            eprintln!("skip: clang が PATH に無いので、`ksn kir --native` との比較は飛ばす");
        }
        ok
    })
}

/// `ksn kir <FQN> --run ...` を走らせ、clang があれば同じ引数の `--native` (LLVM IR を clang で実行ファイルにする) も走らせて、
/// 終了コード・標準出力 (表示)・panic の文言 (標準エラー) が参照実行器と一致することを確かめる。`--run` の結果を返す。
pub fn kir_run(args: &[&str], cwd: &Path) -> Output {
    let r = run(args, cwd);
    if !clang_available() {
        return r;
    }
    let native_args: Vec<&str> = args.iter().map(|a| if *a == "--run" { "--native" } else { *a }).collect();
    let n = run(&native_args, cwd);
    assert_eq!(n.status.code(), r.status.code(), "native: 終了コードが違う {:?}\n{}", args, text(&n.stderr));
    assert_eq!(text(&n.stdout), text(&r.stdout), "native: 表示が違う {:?}", args);
    if matches!(r.status.code(), Some(101) | Some(102)) {
        assert_eq!(text(&n.stderr), text(&r.stderr), "native: panic の文言が違う {:?}", args);
    }
    // AddressSanitizer で作った実行ファイルでも走らせる (機械語の二重解放・範囲外・解放後の読み・漏れを捕まえる)。
    if asan_available() {
        let a = Command::new(exe())
            .args(&native_args)
            .current_dir(cwd)
            .env("KSN_CLANG_ARGS", "-fsanitize=address -fno-omit-frame-pointer -g -O0")
            .env("ASAN_OPTIONS", "detect_leaks=1:abort_on_error=0")
            .output()
            .expect("spawn ksn");
        let err = text(&a.stderr);
        // panic (101 / 102) で抜けた経路は、解放しないまま終わるので漏れの報告は見ない。
        let panicked = matches!(r.status.code(), Some(101) | Some(102));
        let bad = err.contains("AddressSanitizer") || (!panicked && err.contains("LeakSanitizer"));
        assert!(!bad, "ASan が報告した {:?}\n{}", args, err);
        assert_eq!(a.status.code(), r.status.code(), "asan: 終了コードが違う {:?}\n{}", args, err);
        assert_eq!(text(&a.stdout), text(&r.stdout), "asan: 表示が違う {:?}", args);
    }
    r
}

/// clang が `-fsanitize=address` の実行ファイルを作って走らせられるか (環境変数 `KSN_NO_ASAN=1` で無効にできる)。
pub fn asan_available() -> bool {
    use std::sync::OnceLock;
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        if !clang_available() || std::env::var("KSN_NO_ASAN").is_ok() {
            return false;
        }
        let d = std::env::temp_dir().join(format!("ksn_asan_probe_{}", std::process::id()));
        let _ = fs::create_dir_all(&d);
        let c = d.join("a.c");
        let o = d.join("a.out");
        fs::write(&c, "int main(void){return 0;}\n").unwrap();
        let ok = Command::new("clang")
            .args(["-fsanitize=address"])
            .arg(&c)
            .arg("-o")
            .arg(&o)
            .output()
            .map(|r| r.status.success())
            .unwrap_or(false)
            && Command::new(&o).output().map(|r| r.status.success()).unwrap_or(false);
        let _ = fs::remove_dir_all(&d);
        if !ok {
            eprintln!("skip: clang の AddressSanitizer が使えないので、ASan の実行は飛ばす");
        }
        ok
    })
}

pub fn tdrop_prefix() -> Vec<&'static str> {
    vec![
        "type t.Box[T]",
        "    v: T",
        "end t.Box",
        "",
        "type t.L linear pub",
        "    a: u8",
        "end t.L",
        "",
        "type t.P",
        "    a: u8",
        "end t.P",
        "",
        "type t.W",
        "    lin: t.L",
        "    n: u8",
        "end t.W",
        "",
        "fn t.eat(l: t.L) -> () uses [] panics never",
        "    let t.L { a: a } = l",
        "    core.mem.drop(a)",
        "end t.eat",
        "",
        "fn t.eat_w(w: t.W) -> () uses [] panics never",
        "    let t.W { lin: lin, n: n } = w",
        "    t.eat(lin)",
        "    core.mem.drop(n)",
        "end t.eat_w",
        "",
        "fn t.get_l() -> t.L uses [] panics never",
        "    t.L { a: 1u8 }",
        "end t.get_l",
        "",
        "fn t.get_p() -> t.P uses [] panics never",
        "    t.P { a: 3u8 }",
        "end t.get_p",
        "",
        "fn t.get_w() -> t.W uses [] panics never",
        "    t.W { lin: t.get_l(), n: 2u8 }",
        "end t.get_w",
        "",
    ]
}

pub fn tdrop_has(cs: &[String], c: &str) -> bool {
    cs.iter().any(|x| x == c)
}

// ---- 型推論の完全化 第 1 段 (設計_KIR.md 第 8 章 8.5): while / loop、派生メソッド、char / bool、関数項目 ----

/// 型推論の被覆計測 (`KSN_TYPE_COVERAGE=1`) で、未知の式が 0 件であることを確かめる。
/// 型が決まらない式があれば、ここで落ちる (型規則の欠落の回帰)。
pub fn assert_all_typed(name: &str, src: &str) {
    let t = Tmp::new(name, src);
    let o = Command::new(exe()).arg("check").env("KSN_TYPE_COVERAGE", "1").current_dir(&t.0).output().expect("spawn ksn");
    assert_eq!(o.status.code(), Some(0), "{}{}", text(&o.stdout), text(&o.stderr));
    let err = text(&o.stderr);
    assert!(err.contains("型変数 0 / 未知 0 (0.0%, うち根 0)"), "{}", err);
}

/// libc だけを使う構造体の extern (補助 C ライブラリが要らないので、`ksn build` の実行ファイル (LLVM、下ろせなければバンドル) でも検査できる):
/// `div` は 8 バイトを rax で、`ldiv` は 16 バイトを rax:rdx で返す。`inet_ntoa` は `struct in_addr` (4 バイト) を値で受ける。
pub const FFI_LIBC_STRUCT_SRC: &str = r#"type t.DivT layout c pub
    quot: i32
    rem: i32
end t.DivT

type t.InAddr layout c pub
    s_addr: u32
end t.InAddr

type t.LongDiv layout c pub
    quot: i64
    rem: i64
end t.LongDiv

extern t.div(numer: i32, denom: i32) -> t.DivT link "div"

extern t.inet_ntoa(a: t.InAddr) -> *mut u8 link "inet_ntoa"

extern t.ldiv(numer: i64, denom: i64) -> t.LongDiv link "ldiv"

extern t.strlen(text: *const u8) -> usize link "strlen"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let d = unsafe "libc" "div returns a struct in rax" {
        t.div(17i32, 5i32)
    }
    core.check.eq(d.quot, 3i32)
    core.check.eq(d.rem, 2i32)
    let l = unsafe "libc" "ldiv returns a struct in rax:rdx" {
        t.ldiv(0i64 - 17i64, 5i64)
    }
    core.check.eq(l.quot, 0i64 - 3i64)
    core.check.eq(l.rem, 0i64 - 2i64)
    let s = unsafe "libc" "inet_ntoa takes a struct by value" {
        t.inet_ntoa(t.InAddr { s_addr: 16777343u32 })
    }
    let n = unsafe "libc" "strlen of the C string" {
        t.strlen(core.ptr.as_const(s))
    }
    core.check.eq(n, 9usize)
    core.mem.drop(std.io.print_line("libc struct ok"))
    Ok(())
end t.main
"#;

// ---------------------------------------------------------------- KIR 段階 1 (設計_KIR.md 5 章)

/// 段階 1 の対象になる関数 (整数・bool・let・代入・if・while・loop・break・continue・return・wrapping_*)。
pub const KIR_FNS: &str = "fn t.gcd(a: u64, b: u64) -> u64 uses [] panics may
    let mut x = a
    let mut y = b
    while y != 0u64 {
        let r = x % y
        x = y
        y = r
    }
    x
end t.gcd

fn t.classify(n: i32) -> i32 uses [] panics never
    let mut r = 0i32
    if n < 0i32 {
        r = -1i32
    } else if n == 0i32 {
        r = 0i32
    } else {
        r = 1i32
    }
    r
end t.classify

fn t.early(n: u32) -> u32 uses [] panics may
    if n > 10u32 {
        return n - 10u32
    }
    n * 2u32
end t.early

fn t.loopy(n: u32) -> u32 uses [] panics may
    let mut i = 0u32
    let mut acc = 0u32
    loop {
        if i >= n {
            break
        }
        i = i + 1u32
        if i % 2u32 == 0u32 {
            continue
        }
        acc = acc + i
    }
    acc
end t.loopy

fn t.andor(a: bool, b: bool, n: i64) -> bool uses [] panics never
    let x = a && n > 0i64
    let y = b && n < -5i64
    x || y
end t.andor

fn t.fact(n: u64) -> u64 uses [] panics may
    if n <= 1u64 {
        return 1u64
    }
    n * t.fact(n - 1u64)
end t.fact

fn t.negate(x: i8) -> i8 uses [] panics may
    -x
end t.negate

fn t.divs(a: i8, b: i8) -> i8 uses [] panics may
    a / b
end t.divs

fn t.rems(a: i8, b: i8) -> i8 uses [] panics may
    a % b
end t.rems

fn t.wrap(a: u8, b: u8) -> u8 uses [] panics never
    a.wrapping_mul(b).wrapping_add(a)
end t.wrap

fn t.sub8(a: u8, b: u8) -> u8 uses [] panics may
    a - b
end t.sub8

fn t.assign_in_branch(n: i32) -> i32 uses [] panics may
    let mut x = 1i32
    let mut y = 2i32
    if n > 0i32 {
        x = x + n
    } else {
        y = y - n
    }
    x * 100i32 + y
end t.assign_in_branch

fn t.nested(n: i32) -> i32 uses [] panics may
    let mut total = 0i32
    let mut i = 0i32
    while i < n {
        let mut j = 0i32
        while true {
            if j > i {
                break
            }
            total = total + j
            j = j + 1i32
        }
        i = i + 1i32
    }
    total
end t.nested

fn t.down(n: u64) -> u64 uses [] panics may
    if n == 0u64 {
        return 0u64
    }
    1u64 + t.down(n - 1u64)
end t.down
";

/// `KIR_FNS` に `main` を付けたパッケージを作る。
pub fn kir_pkg(name: &str, main_body: &str) -> Tmp {
    Tmp::new(name, &format!("{}{}", KIR_FNS, main_wrap("", main_body)))
}

// ---- 型引数に線形を含む型の束縛 (4.4): `Option` / `Result` / ジェネリック構造体 ----

/// `tdrop_prefix` に、型引数の線形検査の試験用の定義を足して検査する。
pub fn lin_arg_codes(name: &str, b: &[&str]) -> (Vec<String>, String) {
    let nl = char::from(10u8).to_string();
    let mut pre: Vec<&str> = tdrop_prefix();
    pre.extend_from_slice(&[
        "type t.Pair[A, B]",
        "    a: A",
        "    b: B",
        "end t.Pair",
        "",
        "fn t.eat_opt(o: Option<t.L>) -> () uses [] panics never",
        "    match o {",
        "        Some(x) => {",
        "            t.eat(x)",
        "        }",
        "        None => {",
        "            core.mem.drop(0u8)",
        "        }",
        "    }",
        "end t.eat_opt",
        "",
        "fn t.eat_box(b: t.Box<t.L>) -> () uses [] panics never",
        "    let t.Box { v: x } = b",
        "    t.eat(x)",
        "end t.eat_box",
        "",
        "fn t.mk_opt() -> Option<t.L> uses [] panics never",
        "    let o = Some(t.get_l())",
        "    o",
        "end t.mk_opt",
        "",
        "fn t.res_try() -> Result<(), u8> uses [] panics never",
        "    let r: Result<t.L, u8> = Ok(t.get_l())",
        "    let l = r?",
        "    t.eat(l)",
        "    Ok(())",
        "end t.res_try",
        "",
    ]);
    let prefix = pre.join(&nl) + &nl + &nl;
    let t = Tmp::new(name, &main_wrap(&prefix, &body(b)));
    let out = text(&run(&["check", "--json"], &t.0).stdout);
    (codes(&out), out)
}

/// 無限大・NaN・符号つきゼロの特殊値を確かめる関数 (C Annex F / IEEE 754 どおりの値を返す)。
pub const FLOAT_SPECIAL_FNS: &str = "fn t.ninf() -> f64 uses [] panics never
    -(1000.0f64.exp())
end t.ninf

fn t.abs_ninf64() -> u64 uses [] panics never
    t.ninf().abs().to_bits()
end t.abs_ninf64

fn t.sqrt_ninf64() -> bool uses [] panics never
    t.ninf().sqrt().is_nan()
end t.sqrt_ninf64

fn t.sqrt_neg64() -> bool uses [] panics never
    let x = -2.0f64
    x.sqrt().is_nan()
end t.sqrt_neg64

fn t.sqrt_nzero64() -> u64 uses [] panics never
    let z = 0.0f64
    (-z).sqrt().to_bits()
end t.sqrt_nzero64

fn t.sqrt_inf64() -> u64 uses [] panics never
    (1000.0f64.exp()).sqrt().to_bits()
end t.sqrt_inf64

fn t.abs_ninf32() -> u32 uses [] panics never
    let x = -(1000.0f32.exp())
    x.abs().to_bits()
end t.abs_ninf32

fn t.sqrt_ninf32() -> bool uses [] panics never
    let x = -(1000.0f32.exp())
    x.sqrt().is_nan()
end t.sqrt_ninf32

fn t.nan32_bits() -> u32 uses [] panics never
    f32.from_bits(2139095041u32).to_bits()
end t.nan32_bits

fn t.nan64_bits() -> u64 uses [] panics never
    f64.from_bits(9218868437227405313u64).to_bits()
end t.nan64_bits

fn t.rte_nhalf64() -> u64 uses [] panics never
    let x = -0.5f64
    x.round_ties_even().to_bits()
end t.rte_nhalf64

fn t.rte_half32() -> u32 uses [] panics never
    let x = 2.5f32
    x.round_ties_even().to_bits()
end t.rte_half32
";

// ---- 入出力と入口 (設計_KIR.md 12): `write`、`ksn build --backend llvm`、`main` の `Err` の表示 ----

pub const IO_SAY: &str = "fn t.say() -> bool uses [io.console] panics never
    match std.io.print_line(\"hi there\") {
        Ok(_) => true
        Err(_) => false
    }
end t.say

fn t.twice() -> u8 uses [io.console] panics never
    let a = std.io.print_line(\"one\")
    let b = std.io.print_line(\"two\")
    match a {
        Ok(_) => match b {
            Ok(_) => 2u8
            Err(_) => 1u8
        }
        Err(_) => 0u8
    }
end t.twice

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics never
    std.io.print_line(\"main\")
end t.main
";

/// `ksn build --backend llvm` の実行ファイルと `ksn run` を走らせ、(run, native) を返す。native が作れなければ `None`。
pub fn llvm_build_and_run(t: &Tmp) -> Option<(Output, Output)> {
    let b = run(&["build", "--backend", "llvm"], &t.0);
    let out = text(&b.stdout);
    assert_eq!(b.status.code(), Some(0), "{}{}", out, text(&b.stderr));
    assert!(!out.contains("backend skipped"), "{}", out);
    if !clang_available() {
        return None;
    }
    let exe = t.0.join("target").join("debug").join(if cfg!(windows) { "t.exe" } else { "t.native" });
    assert!(exe.exists(), "{}", out);
    let n = Command::new(&exe).current_dir(&t.0).output().expect("native");
    Some((run(&["run"], &t.0), n))
}

pub fn same_output(r: &Output, n: &Output) {
    assert_eq!(text(&n.stdout), text(&r.stdout), "stdout");
    assert_eq!(text(&n.stderr), text(&r.stderr), "stderr");
    assert_eq!(n.status.code(), r.status.code(), "終了コード");
}

// ---- Vec / String の所有の移動と解放 (設計_KIR.md 13 章、kir/owned-heap) ----

/// 所有の移動と解放の試験用の関数。`--run` (参照実行器) は、関数の終わりで領域の漏れと二重解放を内部エラーにする。
pub const KIR_OWN_FNS: &str = "type t.Buf
    items: alloc.vec.Vec<u32, std.alloc.System>
    n: u32
end t.Buf

enum t.Slot
    Empty
    Full(alloc.vec.Vec<u32, std.alloc.System>)
end t.Slot

fn t.oom(_e: alloc.alloc.AllocError) -> u8 uses [] panics never
    1u8
end t.oom

fn t.make(n: u32) -> alloc.vec.Vec<u32, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        match v.push(i) {
            Ok(_u) => {
            }
            Err(_e) => {
                break
            }
        }
        i = i.wrapping_add(1u32)
    }
    v
end t.make

fn t.ret_vec(n: u32) -> usize uses [alloc] panics never
    let v = t.make(n)
    v.len()
end t.ret_vec

fn t.grow(v: alloc.vec.Vec<u32, std.alloc.System>, k: u32) -> alloc.vec.Vec<u32, std.alloc.System> uses [alloc] panics never
    let mut w = v
    match w.push(k) {
        Ok(_u) => {
        }
        Err(_e) => {
        }
    }
    w
end t.grow

fn t.sum(v: &alloc.vec.Vec<u32, std.alloc.System>) -> u32 uses [] panics never
    let mut total = 0u32
    for x in v {
        total = total.wrapping_add(*x)
    }
    total
end t.sum

fn t.arg_ret(n: u32) -> u32 uses [alloc] panics never
    let v = t.make(n)
    let w = t.grow(v, 7u32)
    let x = t.grow(w, 8u32)
    t.sum(&x)
end t.arg_ret

fn t.consume(v: alloc.vec.Vec<u32, std.alloc.System>) -> usize uses [] panics never
    v.len()
end t.consume

fn t.buf(n: u32) -> t.Buf uses [alloc] panics never
    let v = t.make(n)
    t.Buf { items: v, n: n }
end t.buf

fn t.buf_len(b: &t.Buf) -> usize uses [] panics never
    b.items.len()
end t.buf_len

fn t.buf_run(n: u32) -> usize uses [alloc] panics never
    let b = t.buf(n)
    t.buf_len(&b)
end t.buf_run

fn t.consume_buf(b: t.Buf) -> usize uses [] panics never
    t.buf_len(&b)
end t.consume_buf

fn t.maybe(n: u32, some: bool) -> Option<alloc.vec.Vec<u32, std.alloc.System>> uses [alloc] panics never
    if some {
        Some(t.make(n))
    } else {
        None
    }
end t.maybe

fn t.opt_len(n: u32, some: bool) -> usize uses [alloc] panics never
    match t.maybe(n, some) {
        Some(v) => v.len()
        None => 99usize
    }
end t.opt_len

fn t.opt_local(n: u32, some: bool) -> usize uses [alloc] panics never
    let o = t.maybe(n, some)
    match o {
        Some(v) => v.len()
        None => 99usize
    }
end t.opt_local

fn t.opt_peek(n: u32, some: bool) -> usize uses [alloc] panics never
    let o = t.maybe(n, some)
    let k = match o {
        Some(_) => 1usize
        None => 2usize
    }
    if o.is_some() {
        k.wrapping_add(10usize)
    } else {
        k
    }
end t.opt_peek

fn t.opt_str(some: bool) -> usize uses [] panics never
    let o = if some {
        Some(alloc.string.String.new(std.alloc.SYSTEM))
    } else {
        None
    }
    match o {
        Some(s) => s.len()
        None => 99usize
    }
end t.opt_str

fn t.branch(n: u32, take: bool) -> usize uses [alloc] panics never
    let v = t.make(n)
    let mut total = 0usize
    if take {
        let w = v
        total = w.len()
    }
    total.wrapping_add(1usize)
end t.branch

fn t.branch_call(n: u32, take: bool) -> usize uses [alloc] panics never
    let v = t.make(n)
    if take {
        t.consume(v)
    } else {
        0usize
    }
end t.branch_call

fn t.branch_buf(n: u32, take: bool) -> usize uses [alloc] panics never
    let b = t.buf(n)
    if take {
        t.consume_buf(b)
    } else {
        7usize
    }
end t.branch_buf

fn t.churn(n: u32) -> usize uses [alloc] panics never
    let mut total = 0usize
    let mut i = 0u32
    while i < n {
        let v = t.make(3u32)
        core.mem.drop(t.make(2u32))
        total = total.wrapping_add(v.len())
        i = i.wrapping_add(1u32)
    }
    total
end t.churn

fn t.churn_exit(n: u32) -> usize uses [alloc] panics never
    let mut total = 0usize
    let outer = t.make(1u32)
    let mut i = 0u32
    while i < n {
        let v = t.make(i)
        i = i.wrapping_add(1u32)
        if i == 3u32 {
            continue
        }
        if i == 6u32 {
            break
        }
        total = total.wrapping_add(v.len())
    }
    total.wrapping_add(outer.len())
end t.churn_exit

fn t.step(k: usize, bad: bool) -> Result<usize, u8> uses [] panics never
    if bad {
        Err(5u8)
    } else {
        Ok(k)
    }
end t.step

fn t.try_inner(n: u32, bad: bool) -> Result<usize, u8> uses [alloc] panics never
    let v = t.make(n)
    let k = t.step(v.len(), bad)?
    let w = t.make(2u32)
    Ok(k.wrapping_add(w.len()))
end t.try_inner

fn t.try_run(n: u32, bad: bool) -> usize uses [alloc] panics never
    match t.try_inner(n, bad) {
        Ok(x) => x
        Err(_e) => 1000usize
    }
end t.try_run

fn t.push_oom(n: u32) -> Result<usize, u8> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    v.push(n).map_err(t.oom)?
    v.push(n).map_err(fn(_x: alloc.alloc.AllocError) -> u8 capture [] {
        2u8
    })?
    Ok(v.len())
end t.push_oom

fn t.map_err_run(n: u32) -> usize uses [alloc] panics never
    match t.push_oom(n) {
        Ok(x) => x
        Err(_e) => 1000usize
    }
end t.map_err_run

fn t.slot(n: u32) -> t.Slot uses [alloc] panics never
    if n == 0u32 {
        t.Slot.Empty
    } else {
        t.Slot.Full(t.make(n))
    }
end t.slot

fn t.slot_len(n: u32) -> usize uses [alloc] panics never
    match t.slot(n) {
        t.Slot.Empty => 0usize
        t.Slot.Full(v) => v.len()
    }
end t.slot_len

fn t.slot_ignore(n: u32) -> usize uses [alloc] panics never
    match t.slot(n) {
        t.Slot.Empty => 1usize
        t.Slot.Full(_v) => 2usize
    }
end t.slot_ignore

fn t.reassign(n: u32) -> usize uses [alloc] panics never
    let mut v = t.make(n)
    v = t.make(2u32)
    v.len()
end t.reassign

";

/// `KIR_OWN_FNS` に `main` を付けたパッケージを作る。
pub fn kir_own_pkg(name: &str, main_body: &str) -> Tmp {
    let main = format!("fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n{}    Ok(())\nend t.main\n", main_body);
    Tmp::new(name, &format!("{}{}", KIR_OWN_FNS, main))
}

// ---- 取り込みのあるクロージャと反復子の連なり (設計_KIR.md 14 章、kir/closures) ----

pub const KIR_CLOSURE_FNS: &str = "fn t.apply[F: core.ops.Fn<(u64,), u64, E, P>, E: uses, P: panics](f: F, n: u64) -> u64 uses [E] panics P pub
    let mut acc = 0u64
    let mut i = 0u64
    while i < n {
        acc = acc.wrapping_add(f(i))
        i = i.wrapping_add(1u64)
    }
    acc
end t.apply

fn t.cap_ref(n: u64) -> u64 uses [] panics never
    let k = 7u64
    t.apply(
        fn(x: u64) -> u64 capture [&k] {
            x.wrapping_mul(*k).wrapping_add(1u64)
        },
        n,
    )
end t.cap_ref

fn t.cap_move(n: u64) -> u64 uses [] panics may
    let mut k = 3u64
    k = k.wrapping_add(n)
    let f = fn(x: u64) -> u64 capture [move k] {
        x.wrapping_add(k)
    }
    f(1u64).wrapping_add(f(2u64)).wrapping_add(t.apply(f, 4u64))
end t.cap_move

fn t.cap_mut(n: u64) -> u64 uses [] panics never
    let mut total = 0u64
    let f = fn(x: u64) -> u64 capture [&mut total] {
        *total = (*total).wrapping_add(x)
        *total
    }
    core.mem.drop(f(n))
    core.mem.drop(f(n))
    let r = f(5u64)
    r.wrapping_add(total)
end t.cap_mut

fn t.cap_two(n: u64) -> u64 uses [] panics never
    let a = 10u64
    let mut b = 5u64
    b = b.wrapping_add(n)
    t.apply(
        fn(x: u64) -> u64 capture [&a, move b] {
            x.wrapping_mul(*a).wrapping_add(b)
        },
        3u64,
    )
end t.cap_two

fn t.fold_basic(n: u32) -> u32 uses [] panics never
    core.iter.fold(core.iter.range(0u32, n), 100u32, fn(acc: u32, x: u32) -> u32 capture [] {
        acc.wrapping_add(x)
    })
end t.fold_basic

fn t.chain(n: u64) -> u64 uses [] panics may
    let m = 3u64
    core.iter.fold(
        core.iter.filter(
            core.iter.map(core.iter.range(0u64, n), fn(x: u64) -> u64 capture [&m] {
                x.wrapping_mul(*m)
            }),
            fn(x: &u64) -> bool capture [] {
                *x % 2u64 == 0u64
            },
        ),
        1u64,
        fn(acc: u64, x: u64) -> u64 capture [] {
            acc.wrapping_add(x)
        },
    )
end t.chain

fn t.count_even(n: u32) -> usize uses [] panics may
    core.iter.count(core.iter.filter(core.iter.range(0u32, n), fn(x: &u32) -> bool capture [] {
        *x % 2u32 == 0u32
    }))
end t.count_even

fn t.fold_let(n: u32) -> u32 uses [] panics may
    let step = 2u32
    let f = fn(acc: u32, x: u32) -> u32 capture [&step] {
        acc.wrapping_add(x.wrapping_mul(*step))
    }
    core.iter.fold(core.iter.range(5u32, n), 1u32, f)
end t.fold_let

";

pub const KIR_OWN2_FNS: &str = "type t.Ring
    items: alloc.vec.Vec<u32, std.alloc.System>
    head: usize
    len: usize
    cap: usize
end t.Ring

type t.Pair
    a: alloc.vec.Vec<u32, std.alloc.System>
    name: alloc.string.String<std.alloc.System>
    n: usize
end t.Pair

type t.Names
    items: alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>
end t.Names

enum t.Mode derive [Debug, Clone, Copy, PartialEq]
    A
    B
end t.Mode

fn t.Mode.next(self: t.Mode) -> t.Mode uses [] panics never
    match self {
        t.Mode.A => t.Mode.B
        t.Mode.B => t.Mode.A
    }
end t.Mode.next

fn t.mode_run() -> u32 uses [] panics never
    match t.Mode.A.next() {
        t.Mode.A => 1u32
        t.Mode.B => 2u32
    }
end t.mode_run

fn t.make(n: u32) -> alloc.vec.Vec<u32, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        match v.push(i) {
            Ok(_u) => {
            }
            Err(_e) => {
                break
            }
        }
        i = i.wrapping_add(1u32)
    }
    v
end t.make

fn t.word() -> alloc.string.String<std.alloc.System> uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(\"abc\"))
    s
end t.word

fn t.vs_run(n: u32) -> usize uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(t.word()))
        i = i.wrapping_add(1u32)
    }
    let mut total = v.len()
    loop {
        match v.pop() {
            Some(s) => {
                total = total.wrapping_add(s.len())
            }
            None => {
                break
            }
        }
    }
    total
end t.vs_run

fn t.vs_clear(n: u32) -> usize uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(t.word()))
        i = i.wrapping_add(1u32)
    }
    v.clear()
    core.mem.drop(v.push(t.word()))
    v.len()
end t.vs_clear

fn t.vs_keep(n: u32) -> usize uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(t.word()))
        i = i.wrapping_add(1u32)
    }
    v.len()
end t.vs_keep

fn t.names_len(n: t.Names) -> usize uses [] panics never
    n.items.len()
end t.names_len

fn t.names_run(n: u32) -> usize uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(t.word()))
        i = i.wrapping_add(1u32)
    }
    let names = t.Names { items: v }
    t.names_len(names)
end t.names_run

fn t.names_push(n: u32) -> usize uses [alloc] panics never
    let mut names = t.Names { items: alloc.vec.Vec.new(std.alloc.SYSTEM) }
    let mut i = 0u32
    while i < n {
        core.mem.drop(names.items.push(t.word()))
        i = i.wrapping_add(1u32)
    }
    names.items.len()
end t.names_push

fn t.vv_run(n: u32) -> usize uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(t.make(i)))
        i = i.wrapping_add(1u32)
    }
    let mut total = 0usize
    loop {
        match v.pop() {
            Some(x) => {
                total = total.wrapping_add(x.len())
            }
            None => {
                break
            }
        }
    }
    total
end t.vv_run

fn t.pair(n: u32) -> t.Pair uses [alloc] panics never
    t.Pair { a: t.make(n), name: alloc.string.String.new(std.alloc.SYSTEM), n: 3usize }
end t.pair

fn t.str_build(n: usize) -> usize uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    let mut i = 0usize
    while i < n {
        match s.push_str(\"ab\") {
            Ok(_u) => {
            }
            Err(_e) => {
                break
            }
        }
        i = i.wrapping_add(1usize)
    }
    s.len()
end t.str_build

fn t.sum_bytes(s: &str) -> u32 uses [] panics never
    let b = s.as_bytes()
    let mut acc = 0u32
    for x in b {
        acc = acc.wrapping_add(u32.from_u8(*x))
    }
    acc
end t.sum_bytes

fn t.str_chars() -> u32 uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push('a'))
    core.mem.drop(s.push('é'))
    core.mem.drop(s.push('€'))
    core.mem.drop(s.push('😀'))
    core.mem.drop(s.push_str(\"xyz\"))
    t.sum_bytes(s.as_str())
end t.str_chars

fn t.str_chars_len() -> usize uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push('a'))
    core.mem.drop(s.push('é'))
    core.mem.drop(s.push('€'))
    core.mem.drop(s.push('😀'))
    core.mem.drop(s.push_str(\"xyz\"))
    s.len()
end t.str_chars_len

fn t.pop_sum(n: u32) -> u32 uses [alloc] panics never
    let mut v = t.make(n)
    let mut total = 0u32
    loop {
        match v.pop() {
            Some(x) => {
                total = total.wrapping_add(x)
            }
            None => {
                break
            }
        }
    }
    total
end t.pop_sum

fn t.clear_run(n: u32) -> usize uses [alloc] panics never
    let mut v = t.make(n)
    v.clear()
    core.mem.drop(v.push(5u32))
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    core.mem.drop(s.push_str(\"hello\"))
    s.clear()
    core.mem.drop(s.push_str(\"hi\"))
    v.len().wrapping_add(s.len())
end t.clear_run

fn t.ring_new(cap: usize) -> t.Ring uses [alloc] panics never
    let mut items = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0usize
    while i < cap {
        match items.push(0u32) {
            Ok(_u) => {
            }
            Err(_e) => {
                break
            }
        }
        i = i.wrapping_add(1usize)
    }
    t.Ring { items: items, head: 0usize, len: 0usize, cap: cap }
end t.ring_new

fn t.ring_pop(self: &mut t.Ring) -> Option<u32> uses [] panics never
    if self.len == 0usize {
        return None
    }
    let v = match self.items.get(self.head) {
        Some(x) => Some(*x)
        None => None
    }
    self.head = self.head.wrapping_add(1usize).checked_rem(self.cap).unwrap_or(0usize)
    self.len = self.len.wrapping_sub(1usize)
    v
end t.ring_pop

fn t.ring_push(self: &mut t.Ring, v: u32) -> bool uses [] panics never
    if self.len >= self.cap {
        return false
    }
    let slot = self.head.wrapping_add(self.len)
    let idx = match self.cap {
        0usize => 0usize
        c => slot.checked_rem(c).unwrap_or(0usize)
    }
    match self.items.get_mut(idx) {
        Some(cell) => {
            *cell = v
        }
        None => {
            return false
        }
    }
    self.len = self.len.wrapping_add(1usize)
    true
end t.ring_push

fn t.ring_run(cap: usize) -> u32 uses [alloc] panics never
    let mut r = t.ring_new(cap)
    let mut total = 0u32
    let mut i = 0u32
    while i < 7u32 {
        if t.ring_push(&mut r, i.wrapping_add(10u32)) {
            total = total.wrapping_add(1u32)
        }
        if i == 4u32 {
            match t.ring_pop(&mut r) {
                Some(x) => {
                    total = total.wrapping_add(x)
                }
                None => {
                }
            }
        }
        i = i.wrapping_add(1u32)
    }
    loop {
        match t.ring_pop(&mut r) {
            Some(x) => {
                total = total.wrapping_mul(3u32).wrapping_add(x)
            }
            None => {
                break
            }
        }
    }
    total
end t.ring_run

fn t.pair_run(n: u32) -> usize uses [alloc] panics never
    let mut p = t.pair(n)
    core.mem.drop(p.a.push(9u32))
    core.mem.drop(p.name.push_str(\"héllo\"))
    core.mem.drop(p.name.push('!'))
    p.a.len().wrapping_add(p.name.len()).wrapping_add(p.n)
end t.pair_run

fn t.pair_param(p: t.Pair) -> usize uses [alloc] panics never
    let mut q = p
    core.mem.drop(q.name.push_str(\"abc\"))
    core.mem.drop(q.a.push(1u32))
    q.a.len().wrapping_add(q.name.len())
end t.pair_param

fn t.pair_param_run(n: u32) -> usize uses [alloc] panics never
    t.pair_param(t.pair(n))
end t.pair_param_run

fn t.destruct(n: u32) -> usize uses [alloc] panics never
    let (a, b) = (t.make(n), t.make(2u32))
    a.len().wrapping_add(b.len())
end t.destruct

fn t.destruct_partial(n: u32) -> usize uses [alloc] panics never
    let (a, _) = (t.make(n), t.make(3u32))
    a.len()
end t.destruct_partial

fn t.destruct_none(n: u32) -> usize uses [alloc] panics never
    let (_, _) = (t.make(n), t.make(3u32))
    7usize
end t.destruct_none

fn t.destruct_struct(n: u32) -> usize uses [alloc] panics never
    let p = t.pair(n)
    let t.Pair { a: x, n: k, name: _ } = p
    x.len().wrapping_add(k)
end t.destruct_struct

fn t.destruct_struct_both(n: u32) -> usize uses [alloc] panics never
    let t.Pair { a: x, n: k, name: s } = t.pair(n)
    x.len().wrapping_add(k).wrapping_add(s.len())
end t.destruct_struct_both

fn t.match_partial(n: u32, some: bool) -> usize uses [alloc] panics never
    let o = if some {
        Some(t.pair(n))
    } else {
        None
    }
    match o {
        Some(t.Pair { a: x, n: _, name: _ }) => x.len()
        None => 99usize
    }
end t.match_partial

fn t.match_struct_partial(n: u32) -> usize uses [alloc] panics never
    match t.pair(n) {
        t.Pair { a: x, n: _, name: _ } => x.len()
    }
end t.match_struct_partial

fn t.field_move(n: u32) -> usize uses [alloc] panics never
    let p = t.pair(n)
    let w = p.a
    w.len().wrapping_add(p.n)
end t.field_move

fn t.field_move_reassign(n: u32) -> usize uses [alloc] panics never
    let mut p = t.pair(n)
    let w = p.a
    p.a = t.make(2u32)
    w.len().wrapping_add(p.a.len())
end t.field_move_reassign

fn t.field_ret(p: t.Pair) -> alloc.vec.Vec<u32, std.alloc.System> uses [] panics never
    p.a
end t.field_ret

fn t.field_ret_run(n: u32) -> usize uses [alloc] panics never
    let v = t.field_ret(t.pair(n))
    v.len()
end t.field_ret_run

fn t.field_move_str(n: u32) -> usize uses [alloc] panics never
    let mut p = t.pair(n)
    core.mem.drop(p.name.push_str(\"xy\"))
    let s = p.name
    s.len().wrapping_add(p.a.len())
end t.field_move_str

";

/// `kir_map_values_match_tree_walker` の引数の型 (`t.signed` だけ `i32`、他は `u32`)。
pub fn ty_arg(f: &str) -> &'static str {
    if f == "t.signed" {
        "i32"
    } else {
        "u32"
    }
}

/// Fable のレビュー (2026-11-13) で見つかった KIR / LLVM の不具合の回帰 (設計_KIR.md 17 章)。
/// `(KIR の引数, 木走査で呼ぶ式, 期待する値のリテラル, 期待する表示)` の各ケースを、木走査・参照実行器・機械語 (と ASan) で比べる。
pub fn kir_review_cases(tag: &str, fns: &str, f: &str, cases: &[(&[&str], &str, &str, &str)]) {
    let items: Vec<(usize, &(&[&str], &str, &str, &str))> = cases.iter().enumerate().collect();
    par_map(&items, |(i, (args, call, lit, want))| {
        let main = format!(
            "fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may\n    let r = {}\n    core.check.eq(r, {})\n    Ok(())\nend t.main\n",
            call, lit
        );
        let t = Tmp::new(&format!("kir_review_{}_{}", tag, i), &format!("{}\n{}", fns, main));
        let o = run(&["run"], &t.0);
        assert_eq!(o.status.code(), Some(0), "tree-walker: {}\n{}", call, text(&o.stderr) + &text(&o.stdout));
        let mut kargs = vec!["kir", f, "--run"];
        kargs.extend(args.iter().copied());
        let k = kir_run(&kargs, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), *want, "kir value of {}", call);
    });
}

/// `Fn*` 境界の効果実引数の省略 (4.3): 省略した `E` / `P` は関数の暗黙の効果変数・panic 変数で、
/// 渡したクロージャの効果が呼び出し側に伝わる。
pub const ELIDE_AP: &str = "fn t.ap[F: core.ops.Fn<(usize,), u8>](f: F) -> u8 uses [] panics never\n    f.call((0,))\nend t.ap\n\n";

pub fn elide_main(uses: &str, panics: &str, body: &str) -> String {
    format!(
        "{}fn t.boom() -> u8 uses [] panics may\n    core.panic.panic(\"x\")\nend t.boom\n\nfn t.main() -> Result<(), std.io.Error> uses [{}] panics {}\n    core.mem.drop(t.ap(fn(_i: usize) -> u8 capture [] {{\n{}\n    }}))\n    Ok(())\nend t.main\n",
        ELIDE_AP, uses, panics, body
    )
}

/// `derive [ToJson, FromJson]` (std.json): 入れ子の型 (`Vec` / `Option` / `String` / 列挙を含む) を JSON に書き出して解析し直し、
/// 元と同じ JSON に戻る。`test --both` と、`run` と `build` (LLVM → 下ろせなければバンドル) の出力の一致も見る。
pub const DERIVE_JSON_SRC: &str = r#"type t.Doc derive [ToJson, FromJson] pub
    items: alloc.vec.Vec<t.Item, std.alloc.System>
    name: alloc.string.String<std.alloc.System>
    note: Option<alloc.string.String<std.alloc.System>>
    shape: t.Shape
    tags: alloc.vec.Vec<Option<u8>, std.alloc.System>
end t.Doc

type t.Item derive [ToJson, FromJson] pub
    count: u32
    id: i64
    label: alloc.string.String<std.alloc.System>
    ok: bool
end t.Item

enum t.Shape derive [ToJson, FromJson] pub
    Circle(u32)
    Empty
    Rect(u32, u32)
end t.Shape

fn t.decode(text: &str) -> Result<t.Doc, std.json.Error> uses [alloc] panics never
    let doc = std.json.parse(text)?
    t.Doc.from_json(&doc, doc.root())
end t.decode

fn t.error_of(text: &str) -> std.json.Error uses [alloc] panics may
    match t.decode(text) {
        Ok(_d) => core.panic.panic("expected an error")
        Err(e) => e
    }
end t.error_of

fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let text = t.sample_text()
    let doc = match t.decode(text.as_str()) {
        Ok(d) => d
        Err(_e) => core.panic.panic("decode")
    }
    let again = match std.json.to_string(&doc) {
        Ok(s) => s
        Err(_e) => core.panic.panic("encode")
    }
    std.io.print_line(again.as_str())?
    core.check.eq(again.as_str(), text.as_str())
    Ok(())
end t.main

fn t.sample() -> Result<t.Doc, alloc.alloc.AllocError> uses [alloc] panics never
    let mut items = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let label = alloc.string.String.from_str(std.alloc.SYSTEM, "a\"b\n")?
    items.push(t.Item { count: 3u32, id: -5i64, label: label, ok: true })?
    let mut tags = alloc.vec.Vec.new(std.alloc.SYSTEM)
    tags.push(Some(1u8))?
    tags.push(None)?
    let name = alloc.string.String.from_str(std.alloc.SYSTEM, "doc")?
    let note = Some(alloc.string.String.from_str(std.alloc.SYSTEM, "n")?)
    let shape = t.Shape.Rect(2u32, 3u32)
    Ok(t.Doc { items: items, name: name, note: note, shape: shape, tags: tags })
end t.sample

fn t.sample_text() -> alloc.string.String<std.alloc.System> uses [alloc] panics may
    let doc = match t.sample() {
        Ok(d) => d
        Err(_e) => core.panic.panic("sample")
    }
    match std.json.to_string(&doc) {
        Ok(s) => s
        Err(_e) => core.panic.panic("encode")
    }
end t.sample_text

test t.main.test_errors
    core.check.eq(t.error_of("[1,2"), std.json.Error.Eof)
    core.check.eq(t.error_of("{\"a\" 1}"), std.json.Error.Unexpected(5usize))
    core.check.eq(t.error_of("[1] x"), std.json.Error.Trailing(4usize))
    core.check.eq(t.error_of("{\"items\": 1}"), std.json.Error.TypeMismatch(std.json.Kind.Number))
    core.check.eq(t.error_of("{}"), std.json.Error.MissingField(0usize))
end t.main.test_errors

test t.main.test_round_trip
    let text = t.sample_text()
    match t.decode(text.as_str()) {
        Ok(d) => core.check.eq(d.items.len(), 1usize)
        Err(_e) => core.panic.panic("decode")
    }
end t.main.test_round_trip
"#;

/// ケースの横の `<名前>.expect` の `key: value` の行 (`expect:` / `target:` / `ok:` / `note:`)。
/// 試験の入力の `.ksnl` にはコメントを置けないので、印はここに書く。無ければ空。
pub fn expect_entries(ksnl: &Path) -> Vec<(String, String)> {
    let Ok(t) = fs::read_to_string(ksnl.with_extension("expect")) else { return vec![] };
    t.lines()
        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect()
}

/// `expect_entries` の最初の `key` の値。
pub fn expect_value(ksnl: &Path, key: &str) -> Option<String> {
    expect_entries(ksnl).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// 書き方は 1 通り (段 3): 型で決まる揺れ (整数リテラルの接尾辞 E2028・メソッドの呼び方 E2029・構造体リテラルの欄の順 E2048) は
/// `ksn fmt` では直らないので、診断の `fix` だけを当てて (意味の誤りの診断には触れない)、もう一度整形する。試験の入力を正規形にそろえる。
/// これらの揺れそのものを試験するときは `Tmp::new_raw` を使う。
pub fn one_way_fix(dir: &Path) {
    for _ in 0..4 {
        let o = run(&["check", "--json"], dir);
        let out = text(&o.stdout);
        let mut edits: std::collections::BTreeMap<String, Vec<(usize, usize, usize, usize, String)>> = Default::default();
        for l in out.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(l.trim()) else { continue };
            if !matches!(v["code"].as_str(), Some("E2028" | "E2029" | "E2048")) {
                continue;
            }
            for f in v["fix"].as_array().into_iter().flatten() {
                let n = |k: &str| f[k].as_u64().unwrap_or(0) as usize;
                edits.entry(f["file"].as_str().unwrap_or("").to_string()).or_default().push((n("line"), n("col"), n("end_line"), n("end_col"), f["replace"].as_str().unwrap_or("").to_string()));
            }
        }
        if edits.is_empty() {
            return;
        }
        for (file, mut es) in edits {
            let path = dir.join(&file);
            let Ok(src) = fs::read_to_string(&path) else { continue };
            let lines: Vec<&str> = src.split('\n').collect();
            let mut starts = vec![0usize];
            for l in &lines {
                starts.push(starts.last().unwrap() + l.chars().count() + 1);
            }
            let chars: Vec<char> = src.chars().collect();
            es.sort();
            es.dedup();
            let mut chosen: Vec<(usize, usize, String)> = vec![];
            let mut last = 0usize;
            for (l, c, el, ec, r) in es {
                let (s0, e0) = (starts[l - 1] + c - 1, starts[el - 1] + ec - 1);
                if chosen.is_empty() || s0 >= last {
                    last = e0;
                    chosen.push((s0, e0, r));
                }
            }
            let mut out_chars = chars;
            for (s0, e0, r) in chosen.into_iter().rev() {
                out_chars.splice(s0..e0, r.chars());
            }
            fs::write(&path, out_chars.into_iter().collect::<String>()).unwrap();
        }
        let _ = run(&["fmt"], dir);
    }
}

/// 入力を並列に処理し、結果を入力の順に返す (コーパスを 1 本ずつ回す試験を速くする。各項目の確かめ方は変えない)。
/// 同時に動かす本数は `KSN_TEST_PAR` (既定 8)。
#[allow(dead_code)]
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    let n: usize = std::env::var("KSN_TEST_PAR").ok().and_then(|v| v.parse().ok()).unwrap_or(8).max(1);
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|sc| {
        for _ in 0..n.min(items.len().max(1)) {
            sc.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::SeqCst);
                if k >= items.len() {
                    break;
                }
                let r = f(&items[k]);
                out.lock().unwrap()[k] = Some(r);
            });
        }
    });
    out.into_inner().unwrap().into_iter().map(|r| r.expect("worker panicked")).collect()
}
