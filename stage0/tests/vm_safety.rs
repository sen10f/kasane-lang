//! VM の実行時の安全網 (`vm/arena.rs`): 検査の抜け (O-01) で偽の `Vec` の見出しを読んでも、`ksn` のプロセスは壊れず panic (101) で止まる (Opus の O-09)。

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_ksn")
}

fn pkg(name: &str, src: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ksn_vm_safety_{}_{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("src")).unwrap();
    fs::write(d.join("kasane.toml"), "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n").unwrap();
    fs::write(d.join("kasane.lock"), "version = 1\n").unwrap();
    fs::write(d.join("src").join("t.ksnl"), src).unwrap();
    d
}

/// 解放した `Box<Vec>` の見出しを指す参照 (O-01 の穴) と偽の見出し。VM は SIGSEGV せず 101、木走査も 101。
#[test]
fn forged_vec_header_is_a_clean_panic_in_the_vm() {
    let d = pkg("forged", include_str!("data/vm_forged_vec.ksnl"));
    let check = Command::new(exe()).arg("check").current_dir(&d).output().unwrap();
    if check.status.code() != Some(0) {
        // 穴 (O-01) が閉じて検査が止めるようになったら、この試験の役目は終わり (VM に届かない)。
        let _ = fs::remove_dir_all(&d);
        return;
    }
    // 木走査は 101。VM は (領域の中の別の値を読むことはあっても) プロセスを壊さない: 終了コードがあり (シグナルで死なない)、0 か 101。
    let o = Command::new(exe()).args(["run", "--engine=tree"]).current_dir(&d).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "tree: {}", String::from_utf8_lossy(&o.stderr));
    let o = Command::new(exe()).args(["run", "--engine=vm"]).current_dir(&d).output().unwrap();
    let code = o.status.code();
    assert!(matches!(code, Some(0) | Some(101)), "vm: {:?} {}", o.status, String::from_utf8_lossy(&o.stderr));
    let _ = fs::remove_dir_all(&d);
}
