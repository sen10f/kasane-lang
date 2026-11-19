//! 試験の一時ディレクトリの後始末 (`common/scratch.rs`): 成功で消え、失敗 (panic) と `KSN_KEEP_TMP=1` で残る。
//! 他の試験のものには触れない (名前は pid と試験名を含む)。

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::panic;

#[test]
fn scratch_is_removed_when_the_test_passes() {
    let p;
    {
        let s = scratch::Scratch::at(format!("ksn_scratch_pass_{}", std::process::id()));
        fs::create_dir_all(s.join("sub")).unwrap();
        fs::write(s.join("sub").join("f"), "x").unwrap();
        p = s.0.clone();
        assert!(p.exists());
    }
    assert!(!p.exists(), "成功した試験の一時ディレクトリが残った: {}", p.display());
}

#[test]
fn scratch_is_kept_when_the_test_panics() {
    let name = format!("ksn_scratch_panic_{}", std::process::id());
    let p = std::env::temp_dir().join(&name);
    let r = panic::catch_unwind(|| {
        let s = scratch::Scratch::at(name.clone());
        fs::create_dir_all(s.join("sub")).unwrap();
        panic!("試験の失敗を模す");
    });
    assert!(r.is_err());
    assert!(p.exists(), "失敗した試験の一時ディレクトリは調べるために残す: {}", p.display());
    let _ = fs::remove_dir_all(&p);
}

#[test]
fn scratch_is_kept_with_ksn_keep_tmp() {
    // 同じ実行ファイルの他の試験は `KSN_KEEP_TMP` を見ない (この試験だけが設定して、すぐ戻す)。
    let name = format!("ksn_scratch_keep_{}", std::process::id());
    let p = std::env::temp_dir().join(&name);
    std::env::set_var("KSN_KEEP_TMP", "1");
    {
        let s = scratch::Scratch::at(name.clone());
        fs::create_dir_all(&*s).unwrap();
    }
    std::env::remove_var("KSN_KEEP_TMP");
    assert!(p.exists(), "KSN_KEEP_TMP=1 のときは残す");
    let _ = fs::remove_dir_all(&p);
}
