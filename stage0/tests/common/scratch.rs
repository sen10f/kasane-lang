//! 試験が作る一時ディレクトリ・一時ファイルの後始末 (全テストファイルで共有)。
//! 残す条件は 2 つだけ: 試験が失敗中 (`thread::panicking()`) のとき (原因を調べるため)、`KSN_KEEP_TMP=1` のとき。
//! それ以外 (成功したとき) は、試験が自分で作ったものを消す。他のプロセスのものには触れない (名前は自分の pid を含む)。
#![allow(dead_code)]

use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};

/// 残すか。失敗中か `KSN_KEEP_TMP=1` のときは残す。
pub fn keep() -> bool {
    std::thread::panicking() || std::env::var("KSN_KEEP_TMP").map(|v| v == "1").unwrap_or(false)
}

/// 一時のディレクトリかファイルを消す。`keep()` のときは何もしない。
pub fn remove(p: &Path) {
    if keep() {
        return;
    }
    if p.is_dir() {
        let _ = fs::remove_dir_all(p);
    } else {
        let _ = fs::remove_file(p);
    }
}

/// `temp_dir()` の下の一時の名前 (試験の pid を含む)。解体時に `remove` する。
pub struct Scratch(pub PathBuf);

impl Scratch {
    /// `std::env::temp_dir()/name` を作る。同じ名前が残っていれば先に消す (pid を含むので他の試験とは衝突しない)。
    pub fn at(name: String) -> Scratch {
        let p = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&p);
        let _ = fs::remove_file(&p);
        Scratch(p)
    }
}

impl Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for Scratch {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        remove(&self.0);
    }
}
