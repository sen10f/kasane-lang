mod common;
use std::fs;

const SRC: &str = "fn demo.main() uses [io]\n    io.println(\"hi\")\nend demo.main\n";

fn pkg(dir: &std::path::Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("kasane.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"demo.main\"\n\n[dependencies]\n").unwrap();
    fs::write(dir.join("kasane.lock"), "version = 1\n").unwrap();
    fs::write(dir.join("main.ksnl"), SRC).unwrap();
}

// X2: 作業ディレクトリの stdlib/ と spec/stdlib は標準ライブラリにならない。
#[test]
fn planted_stdlib_in_cwd_is_ignored() {
    let w = std::env::temp_dir().join(format!("ksn_sec2_x2_{}", std::process::id()));
    let _ = fs::remove_dir_all(&w);
    fs::create_dir_all(w.join("stdlib/std")).unwrap();
    fs::create_dir_all(w.join("spec/stdlib/std")).unwrap();
    fs::write(w.join("stdlib/std/evil.ksnl"), "garbage\n").unwrap();
    fs::write(w.join("spec/stdlib/std/evil.ksnl"), "garbage\n").unwrap();
    let p = w.join("pkgs/demo");
    pkg(&p);
    // 暗黙の探索の結果を見るため KSN_STDLIB は外す。
    let o = common::run_with_env(&["check"], &p, &[]);
    let t = common::text(&o.stdout) + &common::text(&o.stderr);
    assert!(o.status.success() && !t.contains("evil"), "{t}");
    assert!(!w.join("pwned").exists());
    let _ = fs::remove_dir_all(&w);
}

// 開発の配置 (stage0/target/<profile>/ksn → repo/spec/stdlib) は今までどおり動く。
#[test]
fn dev_layout_still_works() {
    let w = std::env::temp_dir().join(format!("ksn_sec2_x2b_{}", std::process::id()));
    let _ = fs::remove_dir_all(&w);
    let p = w.join("demo");
    pkg(&p);
    let o = common::run(&["check"], &p);
    assert!(o.status.success(), "{}{}", common::text(&o.stdout), common::text(&o.stderr));
    let _ = fs::remove_dir_all(&w);
}
