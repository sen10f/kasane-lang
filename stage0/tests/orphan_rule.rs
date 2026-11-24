// 3.2 の孤児規則と impl の置き場所 (1 つに決まる)。二つのパッケージ (ライブラリ `orl` と `app`) で、
// トレイトを定義したモジュールに置いた impl を app が使えること、違う場所の impl が拒まれることを確かめる。
// 規則: T がこのパッケージの名義型なら T のモジュール、そうでなく Tr がこのパッケージのトレイトなら Tr のモジュール、
// どちらも部外なら E2043 (置けない)。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Work {
    dir: PathBuf,
    cache: PathBuf,
}

impl Work {
    fn new(name: &str) -> Work {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ksn_orl_{}_{}_{}", std::process::id(), name, n));
        let _ = fs::remove_dir_all(&dir);
        let cache = dir.join("cache");
        fs::create_dir_all(&cache).unwrap();
        Work { dir, cache }
    }
    fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }
    fn ksn(&self, args: &[&str], cwd: &Path) -> Output {
        Command::new(exe()).args(args).current_dir(cwd).env("KSN_CACHE_DIR", &self.cache).output().expect("spawn ksn")
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn all(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).replace("\r\n", "\n")
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// パッケージを作り、`ksn fmt` で正規形にそろえる (試験の入力は fmt の出力と 1 バイトも違わないこと)。
fn make_pkg(root: &Path, name: &str, entry: bool, deps: &str, files: &[(&str, &str)]) {
    let e = if entry { format!("entry = \"{}.main\"\n", name) } else { String::new() };
    write(
        &root.join("kasane.toml"),
        &format!("[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = 1\n{}\n[dependencies]\n{}", name, e, deps),
    );
    write(&root.join("kasane.lock"), "version = 1\n");
    for (rel, text) in files {
        write(&root.join(rel), text);
    }
}

/// ライブラリ `orl`: トレイト `orl.tr.Small` (モジュール `orl.tr`) と、その impl を trait 側に置く。
/// `extra` で追加のファイル (型の定義や別モジュールの impl) を足す。
const LIB_TR: &str = "trait orl.tr.Small pub
    fn is_small(self: &Self) -> bool uses [] panics never
end orl.tr.Small

impl orl.tr.Small for alloc.vec.Vec<u32, std.alloc.System>
fn orl.tr.Small.alloc.vec.Vec.is_small(self: &alloc.vec.Vec<u32, std.alloc.System>) -> bool uses [] panics never impls orl.tr.Small
    self.len() < 2usize
end orl.tr.Small.alloc.vec.Vec.is_small

impl orl.tr.Small for u32
fn orl.tr.Small.u32.is_small(self: &u32) -> bool uses [] panics never impls orl.tr.Small
    *self < 10u32
end orl.tr.Small.u32.is_small
";

const LIB_TY: &str = "type orl.ty.P pub
    a: u32 pub
end orl.ty.P
";

/// 型 `orl.ty.P` の impl は型のモジュール `orl.ty` に置く (同じパッケージの名義型なので、型側だけ)。
const LIB_TY_IMPL: &str = "impl orl.tr.Small for orl.ty.P
fn orl.ty.P.is_small(self: &orl.ty.P) -> bool uses [] panics never impls orl.tr.Small
    self.a < 3u32
end orl.ty.P.is_small
";

const APP_MAIN: &str = "fn app.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
    core.mem.drop(v.push(1u32))
    core.check.eq(v.is_small(), true)
    let n = 7u32
    core.check.eq(n.is_small(), true)
    let p = orl.ty.P { a: 1u32 }
    core.check.eq(p.is_small(), true)
    std.io.print_line(\"ok\")?
    Ok(())
end app.main
";

/// `lib` (`orl`) と `app` を作る。`lib_files` はライブラリの追加のファイル、`app_files` はアプリの追加のファイル。
fn two_packages(w: &Work, lib_files: &[(&str, &str)], app_main: &str, app_files: &[(&str, &str)]) -> PathBuf {
    let lib = w.path("lib");
    let mut files = vec![("src/orl/tr.ksnl", LIB_TR)];
    files.extend_from_slice(lib_files);
    make_pkg(&lib, "orl", false, "", &files);
    let app = w.path("app");
    let mut afiles = vec![("src/app.ksnl", app_main)];
    afiles.extend_from_slice(app_files);
    make_pkg(&app, "app", true, "orl = { path = \"../lib\" }\n", &afiles);
    for p in [&lib, &app] {
        let o = Command::new(exe()).arg("fmt").current_dir(p).output().expect("fmt");
        assert!(o.status.success(), "{}", all(&o));
    }
    app
}

/// `ksn check --json` の出力に、その診断コードがあるか。
fn has_code(out: &str, code: &str) -> bool {
    out.contains(&format!("\"code\": \"{}\"", code))
}

#[test]
fn library_trait_implemented_in_its_module_runs_in_the_app_and_builds_native() {
    // 依存のトレイトのモジュールに置いた impl (Vec<u32, System> と u32) を app が使う。インタプリタと native が一致する。
    let w = Work::new("ok");
    let ty = format!("{}\n{}", LIB_TY, LIB_TY_IMPL);
    let app = two_packages(&w, &[("src/orl/ty.ksnl", &ty)], APP_MAIN, &[]);
    assert_eq!(w.ksn(&["update"], &app).status.code(), Some(0));
    let c = w.ksn(&["check"], &app);
    assert_eq!(c.status.code(), Some(0), "{}", all(&c));
    let r = w.ksn(&["run"], &app);
    assert_eq!(text(&r.stdout), "ok\n", "{}", all(&r));
    let b = w.ksn(&["build", "--backend", "llvm"], &app);
    assert_eq!(b.status.code(), Some(0), "{}", all(&b));
    if text(&b.stdout).contains("native executable") {
        let n = Command::new(app.join("target").join("debug").join("app")).current_dir(&app).output().expect("native");
        assert_eq!(text(&n.stdout), text(&r.stdout), "native と run の出力");
        assert_eq!(n.status.code(), r.status.code());
    } else {
        eprintln!("native をつなげなかったので native の比較は飛ばす: {}", text(&b.stdout));
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

#[test]
fn impl_of_a_library_trait_outside_the_trait_module_is_rejected_naming_the_trait_module() {
    // ライブラリの別モジュール (orl.other) に置いた Vec への impl は拒まれ、正しいモジュール orl.tr を名指す。
    let w = Work::new("other");
    let other = "impl orl.tr.Small for alloc.vec.Vec<u32, std.alloc.System>\nfn orl.other.alloc.vec.Vec.is_small(self: &alloc.vec.Vec<u32, std.alloc.System>) -> bool uses [] panics never impls orl.tr.Small\n    true\nend orl.other.alloc.vec.Vec.is_small\n";
    let app = two_packages(&w, &[("src/orl/other.ksnl", other)], APP_MAIN, &[]);
    let lib = w.path("lib");
    let c = w.ksn(&["check", "--json"], &lib);
    let out = text(&c.stdout);
    assert!(has_code(&out, "E2043"), "{}", out);
    assert!(out.contains("must be in module orl.tr"), "{}", out);
    let _ = app;
}

#[test]
fn impl_of_a_library_type_in_the_trait_module_is_rejected_naming_the_type_module() {
    // 同じパッケージの型 `orl.ty.P` への impl は、型のモジュール orl.ty にだけ置ける。トレイトのモジュールは不可。
    let w = Work::new("ty_in_tr");
    let app = two_packages(&w, &[("src/orl/ty.ksnl", LIB_TY)], APP_MAIN, &[]);
    let lib = w.path("lib");
    // トレイトのモジュールに型への impl を置く。
    let mut tr = String::from(LIB_TR);
    tr.push('\n');
    tr.push_str(LIB_TY_IMPL);
    write(&lib.join("src/orl/tr.ksnl"), &tr);
    let c = w.ksn(&["check", "--json"], &lib);
    let out = text(&c.stdout);
    assert!(has_code(&out, "E2043"), "{}", out);
    assert!(out.contains("must be in module orl.ty"), "{}", out);
    let _ = app;
}

#[test]
fn impl_of_a_foreign_trait_for_a_foreign_type_is_rejected_in_the_app() {
    // 両方が部外 (orl の trait、標準ライブラリの u32 ではなく Vec) を app が実装すると、どこに置いても E2043。
    let w = Work::new("both_foreign");
    let app_extra = "impl orl.tr.Small for alloc.string.String\nfn app.extra.alloc.string.String.is_small(self: &alloc.string.String) -> bool uses [] panics never impls orl.tr.Small\n    true\nend app.extra.alloc.string.String.is_small\n";
    let app = two_packages(&w, &[], APP_MAIN, &[("src/app/extra.ksnl", app_extra)]);
    let c = w.ksn(&["check", "--json"], &app);
    let out = text(&c.stdout);
    assert!(has_code(&out, "E2043"), "{}", out);
    assert!(out.contains("neither the trait nor the type"), "{}", out);
}

#[test]
fn a_local_trait_impl_for_a_foreign_type_must_be_in_the_trait_module_in_one_package() {
    // 同じパッケージでも、トレイトが自分のモジュールにあり型が部外なら、置き場所はトレイトのモジュール 1 つ。
    // 別のモジュール app.main_mod に置いた impl は拒まれる。
    let w = Work::new("local_wrong");
    let app = w.path("app");
    make_pkg(
        &app,
        "app",
        true,
        "",
        &[
            ("src/app.ksnl", "fn app.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"ok\")?\n    Ok(())\nend app.main\n"),
            ("src/app/tr.ksnl", "trait app.tr.Show pub\n    fn show(self: &Self) -> u32 uses [] panics never\nend app.tr.Show\n"),
            ("src/app/wrong.ksnl", "impl app.tr.Show for u32\nfn app.wrong.u32.show(self: &u32) -> u32 uses [] panics never impls app.tr.Show\n    *self\nend app.wrong.u32.show\n"),
        ],
    );
    let o = Command::new(exe()).arg("fmt").current_dir(&app).output().expect("fmt");
    assert!(o.status.success(), "{}", all(&o));
    let c = w.ksn(&["check", "--json"], &app);
    let out = text(&c.stdout);
    assert!(has_code(&out, "E2043"), "{}", out);
    assert!(out.contains("must be in module app.tr"), "{}", out);
}

#[test]
fn a_blanket_impl_must_be_in_the_trait_module() {
    let w = Work::new("blanket_wrong");
    let app = w.path("app");
    make_pkg(
        &app,
        "app",
        true,
        "",
        &[
            ("src/app.ksnl", "fn app.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"ok\")?\n    Ok(())\nend app.main\n"),
            ("src/app/tr.ksnl", "trait app.tr.Show pub\n    fn show(self: &Self) -> u32 uses [] panics never\nend app.tr.Show\n"),
            ("src/app/wrong.ksnl", "impl[T] app.tr.Show for T\nfn app.tr.Show.blanket.show[T](self: &T) -> u32 uses [] panics never impls app.tr.Show\n    1u32\nend app.tr.Show.blanket.show\n"),
        ],
    );
    let o = Command::new(exe()).arg("fmt").current_dir(&app).output().expect("fmt");
    let c = w.ksn(&["check", "--json"], &app);
    let out = text(&c.stdout);
    assert!(has_code(&out, "E2043"), "{} {}", all(&o), out);
    assert!(out.contains("must be in module app.tr"), "{}", out);
}
