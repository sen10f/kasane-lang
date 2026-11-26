//! 供給網の穴 (S1-S4): 依存パッケージの本体も根と同じ検査を受ける、`core` / `alloc` / `std` の名前は標準ライブラリだけのもの、
//! git・レジストリの依存があるとき lock を版 1 へ落とせない。二つのパッケージの置き場を一時の場所に作って確かめる。

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Work {
    dir: PathBuf,
}
impl Work {
    fn new(name: &str) -> Work {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ksn_secdeps_{}_{}_{}", std::process::id(), name, n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Work { dir }
    }
    fn put(&self, rel: &str, text: &str) {
        let p = self.dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    fn ksn(&self, args: &[&str], rel: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ksn"))
            .args(args)
            .current_dir(self.dir.join(rel))
            .env("KSN_CACHE_DIR", self.dir.join("cache"))
            .output()
            .expect("spawn ksn")
    }
    /// `ksn check` の出力 (標準出力 + 標準エラー) と終了コード。
    fn check(&self, rel: &str) -> (String, i32) {
        let o = self.ksn(&["check"], rel);
        (format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)), o.status.code().unwrap_or(-1))
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        scratch::remove(&self.dir);
    }
}

const ROOT_TOML: &str = "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"a.main\"\n\n[dependencies]\ndep = { path = \"../dep\" }\n";
const ROOT_LOCK: &str = "version = 2\n\n[[package]]\nname = \"dep\"\nsource = \"path+../dep\"\ndeps = []\n";
const DEP_TOML: &str = "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = 1\n\n[dependencies]\n";

fn two(w: &Work, a_main: &str, dep_src: &str) {
    w.put("a/kasane.toml", ROOT_TOML);
    w.put("a/kasane.lock", ROOT_LOCK);
    w.put("a/src/a.ksnl", a_main);
    w.put("dep/kasane.toml", DEP_TOML);
    w.put("dep/kasane.lock", "version = 1\n");
    w.put("dep/src/dep.ksnl", dep_src);
}

fn has(out: &str, code: &str) -> bool {
    out.contains(&format!(": {}:", code))
}

/// S1: 依存の本体の unsafe / extern / 効果 / panic が、根から検査される。
#[test]
fn s1_dependency_bodies_are_checked_from_the_root() {
    let w = Work::new("s1");
    two(
        &w,
        "fn a.main() -> i32 uses [] panics never\n    let n = dep.pid()\n    core.mem.drop(dep.pure(1u32))\n    n\nend a.main\n",
        "extern dep.getpid() -> i32 link \"getpid\"\n\nfn dep.pid() -> i32 uses [] panics never pub\n    unsafe \"pid\" \"call libc\" {\n        dep.getpid()\n    }\nend dep.pid\n\nfn dep.pure(x: u32) -> u32 uses [] panics never pub\n    core.mem.drop(std.io.print_line(\"dep: side effect from a pure function\"))\n    core.panic.panic(\"dep: panic from panics never\")\nend dep.pure\n",
    );
    let (out, rc) = w.check("a");
    assert_ne!(rc, 0, "{}", out);
    for c in ["E5001", "E4001", "E4002"] {
        assert!(has(&out, c), "{} missing:\n{}", c, out);
    }
    // `run` / `build` も同じ検査を通る。
    for cmd in ["run", "build"] {
        let o = w.ksn(&[cmd], "a");
        assert!(!o.status.success(), "ksn {} must fail", cmd);
    }
}

/// S1: 依存が自分の `[unsafe] modules` を宣言していれば、その依存の unsafe は通る (正しいコードは受け入れる)。
#[test]
fn s1_dependency_with_its_own_unsafe_modules_is_accepted() {
    let w = Work::new("s1ok");
    two(
        &w,
        "fn a.main() -> i32 uses [] panics never\n    dep.pid()\nend a.main\n",
        "extern dep.getpid() -> i32 link \"getpid\"\n\nfn dep.pid() -> i32 uses [] panics never pub\n    unsafe \"pid\" \"call libc\" {\n        dep.getpid()\n    }\nend dep.pid\n",
    );
    w.put("dep/kasane.toml", &format!("{}\n[unsafe]\nmodules = [\"dep\"]\n", DEP_TOML.trim_end_matches('\n')));
    let (out, rc) = w.check("a");
    assert_eq!(rc, 0, "{}", out);
}

/// S1: 依存は他のパッケージのモジュールへ `[unsafe] modules` を与えられない (根の `a` は許可を宣言していない)。
#[test]
fn s1_dependency_cannot_grant_unsafe_to_the_roots_modules() {
    let w = Work::new("s1grant");
    two(
        &w,
        "extern a.getpid() -> i32 link \"getpid\"\n\nfn a.main() -> i32 uses [] panics never\n    unsafe \"pid\" \"call libc\" {\n        a.getpid()\n    }\nend a.main\n",
        "fn dep.ok() -> u32 uses [] panics never pub\n    1u32\nend dep.ok\n",
    );
    w.put("dep/kasane.toml", &format!("{}\n[unsafe]\nmodules = [\"a\"]\n", DEP_TOML.trim_end_matches('\n')));
    let (out, rc) = w.check("a");
    assert_ne!(rc, 0, "{}", out);
    // 読み込みの段の検証 (E9012: 自分のパッケージのモジュールでないもの) が先に止める。
    assert!(out.contains("E9012") || has(&out, "E5001"), "{}", out);
}

/// 依存の `.ksnh` は読まず検査もしない (食い違っていても `.ksnl` だけで通る)。
#[test]
fn dependency_ksnh_is_ignored() {
    let w = Work::new("s1c");
    two(
        &w,
        "fn a.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(dep.greeting())\nend a.main\n",
        "fn dep.greeting() -> &'static str uses [] panics never pub\n    \"DIFFERENT: the .ksnl that actually runs\"\nend dep.greeting\n",
    );
    w.put("dep/src/dep.ksnh", "// c\nfn dep.greeting() -> &'static str uses [] panics never pub\n    \"hello from the .ksnh\"\nend dep.greeting\n");
    let (out, rc) = w.check("a");
    assert_eq!(rc, 0, "{}", out);
    assert!(!has(&out, "E9050"), "{}", out);
}

/// S2: 依存が `std.io.print_line` を定義できない。
#[test]
fn s2_dependency_cannot_define_std_names() {
    let w = Work::new("s2");
    two(
        &w,
        "fn a.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    core.mem.drop(dep.ok())\n    std.io.print_line(\"hello\")\nend a.main\n",
        "fn dep.ok() -> u32 uses [] panics never pub\n    1u32\nend dep.ok\n\nfn std.io.print_line(text: &str) -> Result<(), std.io.Error> uses [io.console] panics never pub\n    core.mem.drop(text)\n    Ok(())\nend std.io.print_line\n",
    );
    let (out, rc) = w.check("a");
    assert_ne!(rc, 0, "{}", out);
    assert!(has(&out, "E1001"), "{}", out);
    let o = w.ksn(&["run"], "a");
    assert!(!o.status.success());
}

/// S3: 根の `src/std/…` は暗黙の unsafe を得られず、std の再定義もできない。
#[test]
fn s3_root_module_under_std_gets_no_unsafe_and_cannot_redefine_std() {
    let w = Work::new("s3");
    w.put("kasane.toml", "[package]\nname = \"b\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"b.main\"\n\n[dependencies]\n");
    w.put("kasane.lock", "version = 1\n");
    w.put("src/std/evil.ksnl", "extern std.evil.getpid() -> i32 link \"getpid\"\n\nfn std.evil.pid() -> i32 uses [] panics never pub\n    unsafe \"pid\" \"libc\" {\n        std.evil.getpid()\n    }\nend std.evil.pid\n");
    w.put("src/b.ksnl", "fn b.main() -> Result<(), std.io.Error> uses [io.console] panics may\n    core.check.is_true(std.evil.pid() > 1i32)\n    std.io.print_line(\"called getpid through an undeclared unsafe module\")\nend b.main\n");
    let (out, rc) = w.check(".");
    assert_ne!(rc, 0, "{}", out);
    assert!(has(&out, "E1001"), "{}", out);

    let w = Work::new("s3b");
    w.put("kasane.toml", "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"c.main\"\n\n[dependencies]\n");
    w.put("kasane.lock", "version = 1\n");
    w.put("src/c.ksnl", "fn c.main() -> Result<(), std.io.Error> uses [io.console] panics never\n    std.io.print_line(\"hello\")\nend c.main\n");
    w.put("src/std/io.ksnl", "fn std.io.print_line(text: &str) -> Result<(), std.io.Error> uses [io.console] panics never pub\n    core.mem.drop(text)\n    Ok(())\nend std.io.print_line\n");
    let (out, rc) = w.check(".");
    assert_ne!(rc, 0, "{}", out);
    assert!(has(&out, "E1001"), "{}", out);
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "tag.gpgsign=false", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(o.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// S4: git 依存があるのに lock を版 1 へ落とすと、承認の関門を黙って無効にするので拒む (E9021)。
#[test]
fn s4_lock_v1_downgrade_with_git_dependency_is_rejected() {
    let w = Work::new("s4");
    let repo = w.dir.join("gl_repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("kasane.toml"), "[package]\nname = \"gl\"\nversion = \"1.0.0\"\nedition = 1\n\n[dependencies]\n").unwrap();
    fs::write(repo.join("kasane.lock"), "version = 1\n").unwrap();
    fs::write(repo.join("src/gl.ksnl"), "fn gl.double(x: u32) -> u32 uses [] panics never pub\n    x.wrapping_mul(2u32)\nend gl.double\n").unwrap();
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "-m", "one"]);
    let rev = git(&repo, &["rev-parse", "HEAD"]);
    w.put(
        "app/kasane.toml",
        &format!("[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"app.main\"\n\n[dependencies]\ngl = {{ git = \"file://{}\", rev = \"{}\" }}\n", repo.display(), rev),
    );
    w.put("app/src/app.ksnl", "fn app.main() -> () uses [] panics never\n    core.mem.drop(gl.double(2u32))\nend app.main\n");
    let u = w.ksn(&["update"], "app");
    assert!(u.status.success(), "{}", String::from_utf8_lossy(&u.stderr));
    let (out, rc) = w.check("app");
    assert_eq!(rc, 0, "v3 lock must pass:\n{}", out);
    let lock = fs::read_to_string(w.dir.join("app/kasane.lock")).unwrap();
    let hash = lock.lines().find(|l| l.starts_with("hash")).unwrap().to_string();
    w.put("app/kasane.lock", &format!("version = 1\n\n[[package]]\nname = \"gl\"\nsource = \"git+file://{}#{}\"\n{}\ndeps = []\n", repo.display(), rev, hash));
    let (out, rc) = w.check("app");
    assert_ne!(rc, 0, "{}", out);
    assert!(has(&out, "E9021"), "{}", out);
}
