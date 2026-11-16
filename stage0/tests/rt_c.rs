//! `sysrt.c` など C の実行時の部品を、小さい C の駆動で直接確かめる (review2 13)。

use std::process::Command;

/// カーネルが送信元を書かない socket (unix の dgram) の `recvfrom` が、読んだ長さと空の送信元を返す
/// (未初期化の領域を整形して InvalidInput を返し、受け取ったデータを失わない、ではない)。
#[test]
fn sysrt_recvfrom_without_a_source_address_returns_the_data_and_an_empty_from() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let Some(cc) = ["gcc", "cc", "clang"].iter().find(|c| Command::new(c).arg("--version").output().is_ok()) else { return };
    let dir = std::env::temp_dir().join(format!("ksn_test_rt_c_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("drv");
    let root = env!("CARGO_MANIFEST_DIR");
    let r = Command::new(cc)
        .arg("-o")
        .arg(&exe)
        .arg(format!("{}/tests/data/rt/recvfrom_unix_dgram.c", root))
        .arg(format!("{}/src/sysrt.c", root))
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let o = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout), "2 e=0 from=[]\n");
    let _ = std::fs::remove_dir_all(&dir);
}
