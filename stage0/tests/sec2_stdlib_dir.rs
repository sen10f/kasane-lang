mod common;
use std::fs;
use std::path::Path;

fn pkg(dir: &Path, name: &str, src: &str) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("kasane.toml"), format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"{name}.main\"\n\n[dependencies]\n")).unwrap();
    fs::write(dir.join("kasane.lock"), "version = 1\n").unwrap();
    fs::write(dir.join(format!("src/{name}.ksnl")), src).unwrap();
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.path().is_dir() {
            copy_tree(&e.path(), &t);
        } else {
            fs::copy(e.path(), t).unwrap();
        }
    }
}

// X2 (Mythos 2): 落としてきたリポジトリの stdlib/ が標準ライブラリになり、`ksn check` / `ksn run` が副作用を起こした。
// 単体で置いた ksn (開発の配置ではない) を、悪意のある stdlib/ を持つ作業ディレクトリの下で動かす。
#[test]
fn planted_stdlib_in_cwd_has_no_side_effects() {
    let w = std::env::temp_dir().join(format!("ksn_sec2_x2_{}", std::process::id()));
    let _ = fs::remove_dir_all(&w);
    fs::create_dir_all(w.join("bin")).unwrap();
    fs::copy(common::exe(), w.join("bin/ksn")).unwrap();
    let run_mark = w.join("pwned_run");
    let check_mark = w.join("pwned_check");
    let std_real = common::root().join("spec/stdlib");
    for sub in ["stdlib", "spec/stdlib"] {
        let d = w.join("repo").join(sub);
        copy_tree(&std_real, &d);
        let evil = format!(
            "extern std.evil.system(cmd: *const u8) -> i32 link \"system\"\n\nfn std.evil.run() -> () uses [] panics never pub\n    let cmd = \"touch {}\\0\".as_bytes()\n    core.mem.drop(unsafe \"evil\" \"call system\" {{\n        std.evil.system(core.ptr.from_ref(cmd))\n    }})\nend std.evil.run\n",
            run_mark.display()
        );
        fs::write(d.join("std/evil.ksnl"), evil).unwrap();
        let fs_sig = d.join("std/fs.sig");
        let s = fs::read_to_string(&fs_sig).unwrap().replace(
            "fn std.fs.create_dir(path: &str) -> Result<(), std.io.Error> uses [io.fs]",
            "fn std.fs.create_dir(path: &str) -> Result<(), std.io.Error> const uses []",
        );
        fs::write(fs_sig, s).unwrap();
    }
    let a = w.join("repo/pkgs/a");
    pkg(&a, "t", "fn t.main() -> () uses [] panics never\n    std.evil.run()\nend t.main\n");
    let b = w.join("repo/pkgs/b");
    pkg(
        &b,
        "t",
        &format!(
            "static t.X: bool = std.fs.create_dir(\"{}\").is_ok()\n\nfn t.main() -> () uses [] panics never\n    core.mem.drop(t.X)\nend t.main\n",
            check_mark.display()
        ),
    );
    let ksn = w.join("bin/ksn");
    let cache = w.join("cache");
    let env = |c: &Path, arg: &str| {
        std::process::Command::new(&ksn)
            .arg(arg)
            .current_dir(c)
            .env("KSN_CACHE_DIR", &cache)
            .env_remove("KSN_STDLIB")
            .output()
            .unwrap()
    };
    let _ = env(&a, "check");
    let _ = env(&a, "run");
    let _ = env(&b, "check");
    let ok = !run_mark.exists() && !check_mark.exists();
    let _ = fs::remove_dir_all(&w);
    assert!(ok, "a planted stdlib in the working directory produced a side effect");
}

// 開発の配置 (stage0/target/<profile>/ksn → repo/spec/stdlib) は今までどおり動く。
#[test]
fn dev_layout_still_works() {
    let w = std::env::temp_dir().join(format!("ksn_sec2_x2b_{}", std::process::id()));
    let _ = fs::remove_dir_all(&w);
    pkg(&w.join("demo"), "demo", "fn demo.main() -> () uses [] panics never\n    core.mem.drop(1u32)\nend demo.main\n");
    let o = common::run(&["check"], &w.join("demo"));
    assert!(o.status.success(), "{}{}", common::text(&o.stdout), common::text(&o.stderr));
    let _ = fs::remove_dir_all(&w);
}
