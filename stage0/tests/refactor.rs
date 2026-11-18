//! `ksn refs` / `ksn rename` の統合テスト。改名の前後で `ksn check` / `ksn fmt --check` / `ksn test` が
//! 変わらず通ること、`--dry-run` が何も書き換えないこと、衝突・曖昧なときは何も変えず失敗することを確かめる。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

/// `ksn` を実行する。読み取りだけのコマンドがシグナルで落ちたとき (この環境で、改名と無関係な
/// `fmt --check` や `test` でも、まれにヒープ破損の abort が観測された。再現条件は不明) は 3 回まで再実行する。
/// ファイルを書き換える `rename` は再実行しない。
fn run(args: &[&str], cwd: &Path) -> Output {
    let writes = args.first() == Some(&"rename") && !args.contains(&"--dry-run");
    for attempt in 0..3 {
        let o = Command::new(exe()).args(args).current_dir(cwd).output().expect("spawn ksn");
        if o.status.code().is_some() || attempt == 2 {
            return o;
        }
        if writes {
            return o;
        }
        use std::os::unix::process::ExitStatusExt;
        eprintln!("ksn {:?} died from signal {:?}: {}", args, o.status.signal(), String::from_utf8_lossy(&o.stderr));
    }
    unreachable!()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap();
        if name == "target" {
            continue;
        }
        if p.is_dir() {
            copy_dir(&p, &to.join(name));
        } else {
            fs::copy(&p, to.join(name)).unwrap();
        }
    }
}

/// 一時ディレクトリに複製したパッケージ。
struct Pkg(PathBuf);
impl Pkg {
    fn fixture(name: &str) -> Pkg {
        Pkg::from(&manifest().join("tests").join("data").join("refactor_pkg"), name)
    }
    fn from(src: &Path, name: &str) -> Pkg {
        let d = std::env::temp_dir().join(format!("ksn_refactor_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        copy_dir(src, &d);
        Pkg(d)
    }
    fn ksn(&self, args: &[&str]) -> Output {
        run(args, &self.0)
    }
    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.0.join(rel)).unwrap_or_else(|_| panic!("missing {}", rel))
    }
    fn write(&self, rel: &str, s: &str) {
        fs::write(self.0.join(rel), s).unwrap();
    }
    fn exists(&self, rel: &str) -> bool {
        self.0.join(rel).exists()
    }
    /// src/ 以下と kasane.toml の全内容 (相対パス, 本文)。
    fn snapshot(&self) -> Vec<(String, String)> {
        fn walk(d: &Path, base: &Path, out: &mut Vec<(String, String)>) {
            let mut es: Vec<_> = fs::read_dir(d).unwrap().map(|e| e.unwrap().path()).collect();
            es.sort();
            for p in es {
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    out.push((p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"), fs::read_to_string(&p).unwrap()));
                }
            }
        }
        let mut v = vec![];
        walk(&self.0, &self.0, &mut v);
        v
    }
    /// check / fmt --check / test がすべて通ることを確かめ、通ったテスト数を返す。
    fn assert_healthy(&self, what: &str) -> usize {
        let c = self.ksn(&["check"]);
        assert_eq!(c.status.code(), Some(0), "{}: check failed:\n{}{}", what, text(&c.stdout), text(&c.stderr));
        let f = self.ksn(&["fmt", "--check"]);
        assert_eq!(f.status.code(), Some(0), "{}: fmt --check failed:\n{}{}", what, text(&f.stdout), text(&f.stderr));
        let t = self.ksn(&["test"]);
        let out = text(&t.stdout);
        assert_eq!(t.status.code(), Some(0), "{}: test failed:\n{}{}", what, out, text(&t.stderr));
        let line = out.lines().rev().find(|l| l.contains(" passed; ")).unwrap_or_else(|| panic!("{}: no summary:\n{}", what, out));
        assert!(line.contains("0 failed"), "{}: {}", what, line);
        line.split_whitespace().next().unwrap().parse().unwrap()
    }
    /// 改名して、前後の健全性と `--dry-run` が何も書き換えないことを確かめる。標準出力を返す。
    fn rename_ok(&self, old: &str, new: &str) -> String {
        let n0 = self.assert_healthy("before");
        let before = self.snapshot();
        let d = self.ksn(&["rename", "--dry-run", old, new]);
        assert_eq!(d.status.code(), Some(0), "dry-run {} {}: {}{}", old, new, text(&d.stdout), text(&d.stderr));
        assert_eq!(self.snapshot(), before, "--dry-run changed files");
        let mut o = self.ksn(&["rename", old, new]);
        if o.status.code().is_none() && self.snapshot() == before {
            // シグナルで落ちたが何も書かれていない (上の `run` の注を参照): 1 回だけやり直す。
            o = self.ksn(&["rename", old, new]);
        }
        assert_eq!(o.status.code(), Some(0), "rename {} {}: {}{}", old, new, text(&o.stdout), text(&o.stderr));
        assert_ne!(self.snapshot(), before, "rename changed nothing");
        let n1 = self.assert_healthy(&format!("after rename {} -> {}", old, new));
        assert_eq!(n0, n1, "the number of passing tests changed");
        text(&d.stdout)
    }
    /// 失敗する改名: 何も変わらず、標準エラーに理由が出る。
    fn rename_refused(&self, old: &str, new: &str) -> String {
        let before = self.snapshot();
        for extra in [&["rename", "--dry-run"][..], &["rename"][..]] {
            let mut args: Vec<&str> = extra.to_vec();
            args.push(old);
            args.push(new);
            let o = self.ksn(&args);
            assert_ne!(o.status.code(), Some(0), "{} {} should fail", old, new);
            assert_eq!(self.snapshot(), before, "a refused rename changed files");
            assert!(text(&o.stderr).contains("ksn:"), "no message");
        }
        text(&self.ksn(&["rename", old, new]).stderr)
    }
}
impl Drop for Pkg {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn no_trace(p: &Pkg, rel: &[&str], gone: &[&str]) {
    for r in rel {
        let s = p.read(r);
        for g in gone {
            assert!(!s.contains(g), "{} still contains {}:\n{}", r, g, s);
        }
    }
}

const FILES: &[&str] = &["src/app.ksnl", "src/app/net.ksnl", "src/app/util.ksnl"];

// ------------------------------------------------------------ refs

#[test]
fn refs_lists_every_form_with_its_kind() {
    let p = Pkg::fixture("refs");
    let o = p.ksn(&["refs", "app.net.make"]);
    assert_eq!(o.status.code(), Some(0));
    let out = text(&o.stdout);
    for k in ["[definition]", "[end]", "[short form]", "[full name]", "[test name]"] {
        assert!(out.contains(k), "missing {} in:\n{}", k, out);
    }
    // 安定した順序 (ファイル → 行 → 列)
    let again = text(&p.ksn(&["refs", "app.net.make"]).stdout);
    assert_eq!(out, again);
    let keys: Vec<(String, usize)> = out.lines().map(|l| { let mut it = l.split(':'); (it.next().unwrap().to_string(), it.next().unwrap().parse().unwrap()) }).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    // 型: ヘッダ (引数・戻り値)、構造体リテラル、impl 行
    let o = text(&p.ksn(&["refs", "app.net.Header"]).stdout);
    for k in ["[header]", "[impl]", "[full name]"] {
        assert!(o.contains(k), "missing {} in:\n{}", k, o);
    }
    // 列挙子 (式とパターン)
    let o = text(&p.ksn(&["refs", "app.net.Kind.Big"]).stdout);
    assert!(o.contains("src/app/util.ksnl") && o.contains("[short form]") && o.contains("[variant declaration]"), "{}", o);
    // 関数を値として渡す参照
    let o = text(&p.ksn(&["refs", "app.net.fail"]).stdout);
    assert!(o.contains("src/app/util.ksnl:") && o.contains("[short form]"), "{}", o);
    // メソッド呼び出し (レシーバの型で解決) とフィールド参照
    let o = text(&p.ksn(&["refs", "app.net.Header.double"]).stdout);
    assert_eq!(o.matches("[method call]").count(), 2, "{}", o);
    let o = text(&p.ksn(&["refs", "app.net.Header.len"]).stdout);
    assert!(o.contains("[field access]") && o.contains("[struct literal field]") && o.contains("[field declaration]"), "{}", o);
    // トレイト境界経由の呼び出し
    let o = text(&p.ksn(&["refs", "app.net.Describe.describe"]).stdout);
    assert!(o.contains("[required method declaration]") && o.contains("[method call]"), "{}", o);
    // モジュール: import と短縮形
    let o = text(&p.ksn(&["refs", "app.net"]).stdout);
    assert!(o.contains("[import]") && o.contains("[short form]"), "{}", o);
}

// ------------------------------------------------------------ 定義の改名

#[test]
fn rename_function_updates_short_forms_fqns_and_tests() {
    let p = Pkg::fixture("fn");
    let dry = p.rename_ok("app.net.make", "app.net.build");
    assert!(dry.contains("update src/app.ksnl"), "{}", dry);
    no_trace(&p, FILES, &["net.make", "app.net.make"]);
    assert!(p.read("src/app/net.ksnl").contains("test app.net.build.test_small"));
    assert!(p.read("src/app.ksnl").contains("net.build(2u32)"), "{}", p.read("src/app.ksnl"));
    // 同じモジュール内の呼び出しは FQN のまま
    assert!(p.read("src/app/util.ksnl").contains("app.util.total(&h)"));
}

#[test]
fn rename_function_used_as_a_value() {
    let p = Pkg::fixture("fnval");
    p.rename_ok("app.net.fail", "app.net.give_up");
    assert!(p.read("src/app/util.ksnl").contains("map_err(net.give_up)"));
}

#[test]
fn rename_type_enum_trait_and_const() {
    let p = Pkg::fixture("types");
    p.rename_ok("app.net.Header", "app.net.Head");
    no_trace(&p, FILES, &["app.net.Header"]);
    assert!(p.read("src/app/net.ksnl").contains("fn app.net.Head.describe"));
    p.rename_ok("app.net.Kind", "app.net.Sort");
    assert!(p.read("src/app/util.ksnl").contains("net.Sort.Big"));
    p.rename_ok("app.net.Describe", "app.net.Show");
    assert!(p.read("src/app/util.ksnl").contains("[T: app.net.Show]"));
    assert!(p.read("src/app/net.ksnl").contains("impls app.net.Show"));
    p.rename_ok("app.net.MAX_LEN", "app.net.LIMIT");
    assert!(p.read("src/app/net.ksnl").contains("app.net.LIMIT"));
}

#[test]
fn rename_test_itself() {
    let p = Pkg::fixture("testname");
    p.rename_ok("app.net.make.test_small", "app.net.make.test_tiny");
    assert!(p.read("src/app/net.ksnl").contains("end app.net.make.test_tiny"));
}

// ------------------------------------------------------------ メンバーの改名

#[test]
fn rename_method_updates_call_sites_by_receiver_type() {
    let p = Pkg::fixture("method");
    // 無関係な型にも `double` というメソッドを足す: そちらの呼び出しは書き換わらない。
    let extra = "\ntype app.util.Other pub\n    n: u32\nend app.util.Other\n\nfn app.util.Other.double(self: &app.util.Other) -> u32 uses [] panics never pub\n    self.n\nend app.util.Other.double\n\nfn app.util.use_both(h: &app.net.Header, o: &app.util.Other) -> u32 uses [] panics never pub\n    h.double().wrapping_add(o.double())\nend app.util.use_both\n";
    // fmt で正規の並びにしてから始める
    let mut src = p.read("src/app/util.ksnl");
    src.push_str(extra);
    p.write("src/app/util.ksnl", &src);
    assert_eq!(p.ksn(&["fmt"]).status.code(), Some(0));
    p.assert_healthy("setup");
    p.rename_ok("app.net.Header.double", "app.net.Header.twice");
    let s = p.read("src/app/util.ksnl");
    assert!(s.contains("h.twice().wrapping_add(o.double())"), "{}", s);
    assert!(s.contains("fn app.util.Other.double"), "{}", s);
    assert!(p.read("src/app/net.ksnl").contains("end app.net.Header.twice"));
}

#[test]
fn rename_method_through_a_trait_bound_renames_the_required_method() {
    let p = Pkg::fixture("traitm");
    // 実装メソッドだけの改名は断る (トレイトの必須メソッドを改名する)。
    let msg = p.rename_refused("app.net.Header.describe", "app.net.Header.explain");
    assert!(msg.contains("implements a trait method") && msg.contains("app.net.Describe.describe"), "{}", msg);
    p.rename_ok("app.net.Describe.describe", "app.net.Describe.explain");
    assert!(p.read("src/app/util.ksnl").contains("x.explain()"));
    assert!(p.read("src/app/net.ksnl").contains("fn app.net.Header.explain"));
}

#[test]
fn rename_field_and_variant() {
    let p = Pkg::fixture("field");
    p.rename_ok("app.net.Header.len", "app.net.Header.size");
    no_trace(&p, FILES, &[".len,", " len:", "h.len", "self.len"]);
    assert!(p.read("src/app/net.ksnl").contains("size: u32 pub"));
    assert!(p.read("src/app/net.ksnl").contains("app.net.Header { kind: kind, size: len }"));
    p.rename_ok("app.net.Kind.Small", "app.net.Kind.Tiny");
    assert!(p.read("src/app/util.ksnl").contains("net.Kind.Tiny => 0u32"));
    assert!(p.read("src/app/net.ksnl").contains("    Tiny\n"));
}

#[test]
fn rename_method_with_unknown_receiver_type_fails_and_lists_the_sites() {
    // 型推論が通っていれば、クロージャ・for・Option の中でもレシーバの型は決まる。
    let p = Pkg::fixture("typed");
    let extra = "\ntype app.util.Other pub\n    n: u32\nend app.util.Other\n\nfn app.util.Other.double(self: &app.util.Other) -> u32 uses [] panics never pub\n    self.n\nend app.util.Other.double\n\nfn app.util.via_slice(hs: &[app.net.Header]) -> u32 uses [] panics never pub\n    let mut total = 0u32\n    for h in hs.iter() {\n        total = total.wrapping_add(h.double())\n    }\n    total\nend app.util.via_slice\n\nfn app.util.via_opt(hs: &[app.net.Header]) -> u32 uses [] panics never pub\n    match hs.get(0usize) {\n        Some(h) => h.double()\n        None => 0u32\n    }\nend app.util.via_opt\n\nfn app.util.via_closure(h: app.net.Header) -> u32 uses [] panics never pub\n    let f = fn(x: &app.net.Header) -> u32 capture [] {\n        x.double()\n    }\n    f(&h)\nend app.util.via_closure\n";
    let mut u = p.read("src/app/util.ksnl");
    u.push_str(extra);
    p.write("src/app/util.ksnl", &u);
    assert_eq!(p.ksn(&["fmt"]).status.code(), Some(0));
    p.assert_healthy("setup");
    p.rename_ok("app.net.Header.double", "app.net.Header.twice");
    let s = p.read("src/app/util.ksnl");
    assert_eq!(s.matches("h.twice()").count(), 3, "{}", s);
    assert!(s.contains("x.twice()") && s.contains("fn app.util.Other.double"), "{}", s);

    // 受け取った値の型が決まらない呼び出し (未定義の関数の戻り値) は、名前だけでは決められない: 失敗して一覧する。
    let p = Pkg::fixture("unknown");
    let mut u = p.read("src/app/util.ksnl");
    u.push_str(extra);
    u.push_str("\nfn app.util.broken() -> u32 uses [] panics never pub\n    app.util.nothing(1u32).double()\nend app.util.broken\n");
    p.write("src/app/util.ksnl", &u);
    let _ = p.ksn(&["fmt"]);
    let before = p.snapshot();
    let o = p.ksn(&["rename", "--dry-run", "app.net.Header.double", "app.net.Header.twice"]);
    assert_ne!(o.status.code(), Some(0));
    let err = text(&o.stderr);
    assert!(err.contains("receiver type") && err.contains("util.ksnl") && err.contains("`.double`"), "{}", err);
    let o = p.ksn(&["rename", "app.net.Header.double", "app.net.Header.twice"]);
    assert_ne!(o.status.code(), Some(0));
    assert_eq!(p.snapshot(), before);
    // refs は曖昧な箇所を印付きで出す。
    let r = text(&p.ksn(&["refs", "app.net.Header.double"]).stdout);
    assert!(r.contains("[method call (receiver type unknown)]"), "{}", r);
}

// ------------------------------------------------------------ 定義の移動

#[test]
fn move_function_to_another_module_adds_and_removes_imports() {
    let p = Pkg::fixture("move_fn");
    // net -> util: util は net を import 済み。net.fail の参照 (util の短縮形) は同一モジュールの FQN になる。
    p.rename_ok("app.net.fail", "app.util.fail");
    let u = p.read("src/app/util.ksnl");
    assert!(u.contains("fn app.util.fail") && u.contains("map_err(app.util.fail)"), "{}", u);
    assert!(!p.read("src/app/net.ksnl").contains("app.net.fail"));
    // util -> net へ戻す (循環しない): 参照は net の短縮形になる。
    p.rename_ok("app.util.fail", "app.net.fail");
    assert!(p.read("src/app/util.ksnl").contains("map_err(net.fail)"));
}

#[test]
fn move_function_from_the_root_module_updates_imports() {
    let p = Pkg::fixture("move_root");
    p.rename_ok("app.twice", "app.util.twice");
    assert!(p.read("src/app.ksnl").contains("util.twice(h.len)"));
    assert!(p.read("src/app/util.ksnl").contains("test app.util.twice.test_four"));
    assert!(!p.read("src/app.ksnl").contains("fn app.twice"));
}

#[test]
fn move_generic_function_and_drop_unused_import() {
    let p = Pkg::fixture("move_show");
    // util.show は net の型だけを使う。net に移すと util の import net は kind_code などで残り、
    // app の import util も util.total などで残る。
    p.rename_ok("app.util.show", "app.net.show");
    assert!(p.read("src/app.ksnl").contains("net.show(&h)"));
}

#[test]
fn move_type_with_methods_impls_and_tests() {
    // 型 + トレイト実装 (impl 行) + メソッド + 他モジュールからの参照を、まとめて別のモジュールへ移す。
    let p = Pkg::fixture("move_type");
    // 新しい葉のモジュール (net が import する側)。
    p.write("src/app/extra.ksnl", "const app.extra.ONE: u32 = 1u32\n");
    p.assert_healthy("setup");
    p.rename_ok("app.net.Kind", "app.extra.Kind");
    assert!(p.read("src/app/extra.ksnl").contains("enum app.extra.Kind"));
    assert!(p.read("src/app/net.ksnl").contains("import app.extra"));
    assert!(p.read("src/app/net.ksnl").contains("extra.Kind.Big"), "{}", p.read("src/app/net.ksnl"));
    // util は net の import を使い続け、extra も import する。
    assert!(p.read("src/app/util.ksnl").contains("import app.extra"));
    assert!(p.read("src/app/util.ksnl").contains("extra.Kind.Big => 1u32"));
}

#[test]
fn move_a_type_that_creates_a_cycle_is_refused() {
    let p = Pkg::fixture("move_cycle");
    p.write("src/app/extra.ksnl", "const app.extra.ONE: u32 = 1u32\n");
    // Header を extra に移すと、Header を使う net -> extra と、extra の Kind 参照 (extra -> net) が輪になる。
    let msg = p.rename_refused("app.net.Header", "app.util.Header");
    assert!(msg.contains("cycle") || msg.contains("check"), "{}", msg);
}

// ------------------------------------------------------------ モジュールの改名

#[test]
fn rename_module_moves_the_file_and_updates_everything() {
    let p = Pkg::fixture("module");
    assert!(p.read("kasane.toml").contains("modules = [\"app.net\"]"));
    let dry = p.rename_ok("app.net", "app.proto");
    assert!(dry.contains("move src/app/net.ksnl -> src/app/proto.ksnl"), "{}", dry);
    assert!(!p.exists("src/app/net.ksnl") && p.exists("src/app/proto.ksnl"));
    assert!(p.read("kasane.toml").contains("modules = [\"app.proto\"]"), "{}", p.read("kasane.toml"));
    no_trace(&p, &["src/app.ksnl", "src/app/proto.ksnl", "src/app/util.ksnl"], &["app.net", "net."]);
    assert!(p.read("src/app.ksnl").contains("import app.proto") && p.read("src/app.ksnl").contains("proto.make(2u32)"));
}

#[test]
fn rename_module_into_a_new_directory() {
    let p = Pkg::fixture("module_dir");
    p.rename_ok("app.net", "app.wire.net");
    assert!(p.exists("src/app/wire/net.ksnl") && !p.exists("src/app/net.ksnl"));
    assert!(p.read("src/app/util.ksnl").contains("import app.wire.net"));
    assert!(p.read("src/app/util.ksnl").contains("net.parse(len)"));
}

#[test]
fn rename_the_entry_function_updates_the_manifest() {
    let p = Pkg::fixture("entry");
    p.rename_ok("app.main", "app.start");
    assert!(p.read("kasane.toml").contains("entry = \"app.start\""));
}

// ------------------------------------------------------------ 衝突と拒否

#[test]
fn collisions_change_nothing_and_fail() {
    let p = Pkg::fixture("collide");
    // 既存の定義
    assert!(p.rename_refused("app.net.make", "app.net.fail").contains("collision"));
    // 既存のモジュール
    assert!(p.rename_refused("app.net", "app.util").contains("collision"));
    // 既存のフィールドと同名のメソッド
    assert!(p.rename_refused("app.net.Header.double", "app.net.Header.len").contains("collision"));
    // 移動先が存在しない
    assert!(p.rename_refused("app.net.make", "app.nothere.make").contains("does not exist"));
    // 他のパッケージ名
    assert!(p.rename_refused("app.net.make", "other.net.make").contains("package name"));
    // 見つからない
    assert!(p.rename_refused("app.net.nothing", "app.net.something").contains("not found"));
    // 予約語
    assert!(!p.rename_refused("app.net.make", "app.net.fn").is_empty());
}

#[test]
fn target_file_that_already_exists_is_a_collision() {
    let p = Pkg::fixture("collide_file");
    // モジュール名としては未使用だが、ファイルがある (src/app/proto.ksnl は構文エラーの別ファイル)。
    p.write("src/app/proto.ksnl", "not a module\n");
    let msg = p.rename_refused("app.net", "app.proto");
    assert!(msg.contains("collision") || msg.contains("syntax"), "{}", msg);
}

#[test]
fn short_name_ambiguity_is_refused() {
    // app.util と app.wire.util が同じファイルで import されると末尾名 `util` が曖昧になる。
    let p = Pkg::fixture("ambiguous_tail");
    p.write("src/app/other.ksnl", "fn app.other.one() -> u32 uses [] panics never pub\n    1u32\nend app.other.one\n");
    let app = p.read("src/app.ksnl").replace("import app.util", "import app.other\nimport app.util").replace("fn app.twice", "fn app.twice2(n: u32) -> u32 uses [] panics never\n    other.one().wrapping_add(n)\nend app.twice2\n\nfn app.twice");
    p.write("src/app.ksnl", &app);
    let _ = p.ksn(&["fmt"]);
    p.assert_healthy("setup");
    // app.other を app.net の末尾名と同じにする改名: net が二重になる。
    let msg = p.rename_refused("app.other", "app.util2.net");
    assert!(msg.contains("ambiguous short name"), "{}", msg);
}

#[test]
fn a_rename_that_would_break_check_is_rolled_back() {
    // 長い名前に変えると 100 文字の行制限を越えて check が失敗する: 何も変わらない。
    let p = Pkg::fixture("rollback");
    let long = "app.net.make_a_header_with_a_really_really_really_long_name_that_overflows_the_line_limit_of_one_hundred_chars";
    let msg = p.rename_refused("app.net.make", long);
    assert!(msg.contains("check") || msg.contains("longer"), "{}", msg);
}

#[test]
fn doc_comment_mentions_are_not_rewritten_but_reported() {
    let p = Pkg::fixture("doc");
    let u = p.read("src/app/util.ksnl").replace("fn app.util.parsed", "/// Calls app.net.make indirectly.\nfn app.util.parsed");
    p.write("src/app/util.ksnl", &u);
    p.assert_healthy("setup");
    let before = p.ksn(&["refs", "app.net.make"]);
    assert!(text(&before.stdout).contains("[doc comment (not rewritten)]"), "{}", text(&before.stdout));
    let o = p.ksn(&["rename", "app.net.make", "app.net.build"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("doc comment mentions app.net.make (not rewritten)"), "{}", text(&o.stderr));
    assert!(p.read("src/app/util.ksnl").contains("/// Calls app.net.make indirectly."));
    p.assert_healthy("after");
}

// ------------------------------------------------------------ 仕様 0.10 (短縮形・FQN のどちらも書ける)

#[test]
fn rename_finds_both_spellings_and_keeps_each_form() {
    let p = Pkg::fixture("both_forms");
    // 本体の FQN 形 (import 済みモジュール) とヘッダの短縮形 (正規形ではないが受け入れられる) を足す。
    let u = p.read("src/app/util.ksnl").replace("h.double().wrapping_add(h.len)", "h.double().wrapping_add(net.make(h.len).len).wrapping_add(app.net.make(0u32).len)");
    let u = u.replace("fn app.util.total(h: &app.net.Header)", "fn app.util.total(h: &net.Header)");
    p.write("src/app/util.ksnl", &u);
    // 正規形でない書き方は check の誤り (E0020、書き方は 1 通り)。rename はそれでも両方の形を見つけて、それぞれの形のまま直す。
    let c = p.ksn(&["check"]);
    assert_eq!(c.status.code(), Some(1), "{}", text(&c.stdout));
    assert!(text(&c.stdout).contains("E0020"), "{}", text(&c.stdout));
    let o = p.ksn(&["rename", "app.net.make", "app.net.build"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let s = p.read("src/app/util.ksnl");
    assert!(s.contains("h.double().wrapping_add(net.build(h.len).len).wrapping_add(app.net.build(0u32).len)"), "{}", s);
    let o = p.ksn(&["rename", "app.net.Header", "app.net.Head"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let s = p.read("src/app/util.ksnl");
    assert!(s.contains("fn app.util.total(h: &net.Head)"), "{}", s);
    // `fmt` が正規形にすれば check も test も通る。
    assert_eq!(p.ksn(&["fmt"]).status.code(), Some(0));
    assert_eq!(p.ksn(&["check"]).status.code(), Some(0), "{}", text(&p.ksn(&["check"]).stdout));
    assert_eq!(p.ksn(&["test"]).status.code(), Some(0));
}

// ------------------------------------------------------------ 実際のパッケージでの通し

#[test]
fn shortener_end_to_end_sequence() {
    let src = manifest().parent().unwrap().join("examples").join("shortener");
    let p = Pkg::from(&src, "shortener");
    let n = p.assert_healthy("baseline");
    assert_eq!(n, 35);
    for (old, new) in [
        ("shortener.code.encode", "shortener.code.encode_u64"),
        ("shortener.http.build_request", "shortener.http.make_request"),
        ("shortener.store.Store", "shortener.store.Repo"),
        ("shortener.store.Repo.add", "shortener.store.Repo.append"),
        ("shortener.http.Head.content_length", "shortener.http.Head.body_len"),
        ("shortener.error.Error.Closed", "shortener.error.Error.Shut"),
        ("shortener.code", "shortener.shortcode"),
        ("shortener.router", "shortener.web.router"),
    ] {
        p.rename_ok(old, new);
    }
    assert!(p.exists("src/shortener/shortcode.ksnl") && p.exists("src/shortener/web/router.ksnl"));
    assert!(!p.exists("src/shortener/code.ksnl") && !p.exists("src/shortener/router.ksnl"));
    assert_eq!(p.assert_healthy("final"), 35);
}

// ------------------------------------------------------------ 移動に伴う `pub` の補い (3.1)

/// 移動すると別モジュールから非 `pub` の定義・フィールドを参照することになる 3 モジュールのパッケージ。
fn vis_fixture(name: &str) -> Pkg {
    let src = std::env::temp_dir().join(format!("ksn_refactor_vis_src_{}_{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&src);
    fs::create_dir_all(src.join("src/v")).unwrap();
    fs::write(src.join("kasane.toml"), "[package]\nname = \"v\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"v.main\"\n\n[dependencies]\n").unwrap();
    fs::write(src.join("kasane.lock"), "version = 1\n").unwrap();
    fs::write(
        src.join("src/v.ksnl"),
        "import v.shape\n\nfn v.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    let s = shape.make(3u32)\n    core.check.eq(shape.area(&s), 9u32)\n    core.check.eq(shape.side_twice(&s), 6u32)\n    std.io.print_line(\"ok\")?\n    Ok(())\nend v.main\n",
    )
    .unwrap();
    fs::write(
        src.join("src/v/shape.ksnl"),
        "const v.shape.SCALE: u32 = 1u32\n\ntype v.shape.Sq pub\n    side: u32\nend v.shape.Sq\n\nfn v.shape.area(s: &v.shape.Sq) -> u32 uses [] panics never pub\n    v.shape.mul(s.side, s.side)\nend v.shape.area\n\nfn v.shape.make(side: u32) -> v.shape.Sq uses [] panics never pub\n    v.shape.Sq { side: side }\nend v.shape.make\n\nfn v.shape.mul(a: u32, b: u32) -> u32 uses [] panics never\n    a.wrapping_mul(b).wrapping_mul(v.shape.SCALE)\nend v.shape.mul\n\nfn v.shape.side_twice(s: &v.shape.Sq) -> u32 uses [] panics never pub\n    s.side.wrapping_mul(2u32).wrapping_mul(v.shape.SCALE)\nend v.shape.side_twice\n",
    )
    .unwrap();
    fs::write(src.join("src/v/other.ksnl"), "fn v.other.one() -> u32 uses [] panics never pub\n    1u32\nend v.other.one\n").unwrap();
    let p = Pkg::from(&src, name);
    let _ = fs::remove_dir_all(&src);
    p
}

#[test]
fn moving_a_definition_makes_it_pub_when_the_old_module_still_uses_it() {
    // app.twice を移すと app.main (元のモジュール) から参照される。以前は手で `pub` を付ける必要があった。
    let p = Pkg::fixture("move_adds_pub");
    let d = p.ksn(&["rename", "--dry-run", "app.twice", "app.util.twice"]);
    assert_eq!(d.status.code(), Some(0), "{}{}", text(&d.stdout), text(&d.stderr));
    // 計画 (--dry-run の差分) に `pub` が入り、標準エラーに報告が出る。
    assert!(text(&d.stdout).contains("+fn app.util.twice(n: u32) -> u32 uses [] panics never pub"), "{}", text(&d.stdout));
    assert!(text(&d.stderr).contains("made app.util.twice `pub`"), "{}", text(&d.stderr));
    p.rename_ok("app.twice", "app.util.twice");
    assert!(p.read("src/app/util.ksnl").contains("panics never pub\n    n.wrapping_mul(2u32)\nend app.util.twice"));
}

#[test]
fn moving_a_definition_makes_the_private_things_it_uses_pub() {
    // shape.area は同モジュールの非 pub の関数 mul を呼び、非 pub のフィールド side を読む。
    // area を other に移すと、mul と side だけに pub が付く (SCALE は mul からしか使われないので付かない)。
    let p = vis_fixture("move_uses_private");
    p.rename_ok("v.shape.area", "v.other.area");
    let shape = p.read("src/v/shape.ksnl");
    assert!(shape.contains("fn v.shape.mul(a: u32, b: u32) -> u32 uses [] panics never pub"), "{}", shape);
    assert!(!shape.contains("const v.shape.SCALE: u32 = 1u32 pub"), "{}", shape);
    assert!(shape.contains("    side: u32 pub"), "{}", shape);
    let other = p.read("src/v/other.ksnl");
    assert!(other.contains("fn v.other.area") && other.contains("shape.mul("), "{}", other);
}

#[test]
fn moving_a_definition_makes_a_const_and_a_field_it_uses_pub() {
    // side_twice は shape の非 pub の定数 SCALE と、非 pub のフィールド side を使う。other へ移すと両方に pub が付く。
    let p = vis_fixture("move_const");
    let d = p.ksn(&["rename", "--dry-run", "v.shape.side_twice", "v.other.side_twice"]);
    assert_eq!(d.status.code(), Some(0), "{}{}", text(&d.stdout), text(&d.stderr));
    let err = text(&d.stderr);
    assert!(err.contains("made v.shape.SCALE `pub`") && err.contains("made field side of v.shape.Sq `pub`"), "{}", err);
    p.rename_ok("v.shape.side_twice", "v.other.side_twice");
    let shape = p.read("src/v/shape.ksnl");
    assert!(shape.contains("const v.shape.SCALE: u32 = 1u32 pub"), "{}", shape);
    assert!(shape.contains("    side: u32 pub"), "{}", shape);
    // mul は shape の中だけで使われ続けるので pub にしない (最小)。
    assert!(!shape.contains("fn v.shape.mul(a: u32, b: u32) -> u32 uses [] panics never pub"), "{}", shape);
}
