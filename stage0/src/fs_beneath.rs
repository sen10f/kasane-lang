//! `std.fs.Dir` (ディレクトリの取っ手) と `open_file_beneath` の本体。
//! 参照実行器・木走査の実行器・バイトコードの VM が同じものを呼ぶ (結果がそろうように)。
//!
//! 規則: `rel` は相対パスだけ。空・`/` で始まる・NUL を含む・`..` を含むものは拒む (InvalidInput)。
//! 途中のどの成分も symlink なら拒む (ELOOP)。Linux は `openat2` (`RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`) を使い、
//! カーネルが無い (ENOSYS) ときだけ成分ごとの `openat(O_NOFOLLOW)` に落ちる。他の Unix も成分ごとの検査。
//! Windows は未対応 (Unsupported) で、黙ってリンクを辿ることはしない。

use std::fs::File;
use std::io;

/// `path` をディレクトリとして開く。ディレクトリでなければ ENOTDIR。
pub(crate) fn open_dir(path: &str) -> io::Result<File> {
    if path.contains('\0') {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let f = File::open(path)?;
    if !f.metadata()?.is_dir() {
        return Err(io::Error::from_raw_os_error(not_dir_code()));
    }
    Ok(f)
}

fn not_dir_code() -> i32 {
    #[cfg(unix)]
    {
        libc::ENOTDIR
    }
    #[cfg(not(unix))]
    {
        20
    }
}

/// `rel` の形を検査する。拒むのは空・絶対・NUL・`..` と、`.` だけの成分列。
fn check_rel(rel: &str) -> io::Result<()> {
    let bad = rel.is_empty() || rel.starts_with('/') || rel.contains('\0') || rel.split('/').any(|c| c == "..");
    let has_name = rel.split('/').any(|c| !c.is_empty() && c != ".");
    if bad || !has_name {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn open_beneath(dir: &File, rel: &str) -> io::Result<File> {
    use std::os::unix::io::AsRawFd;
    check_rel(rel)?;
    #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        match openat2_beneath(dir.as_raw_fd(), rel) {
            Err(e) if e.raw_os_error() == Some(libc::ENOSYS) => {}
            other => return other,
        }
    }
    componentwise(dir.as_raw_fd(), rel)
}

#[cfg(not(unix))]
pub(crate) fn open_beneath(_dir: &File, rel: &str) -> io::Result<File> {
    check_rel(rel)?;
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
fn openat2_beneath(dirfd: i32, rel: &str) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::unix::io::FromRawFd;
    // openat2(2) の番号は x86_64 と aarch64 で 437。RESOLVE_* の値はカーネルの uapi (linux/openat2.h) と同じ。
    const SYS_OPENAT2: libc::c_long = 437;
    const RESOLVE_NO_SYMLINKS: u64 = 0x04;
    const RESOLVE_BENEATH: u64 = 0x08;
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let cpath = CString::new(rel).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS,
    };
    // SAFETY: cpath は NUL 終端の有効な文字列、how は openat2 の期待する並びの構造体で、大きさも正しく渡す。
    let fd = unsafe { libc::syscall(SYS_OPENAT2, dirfd, cpath.as_ptr(), &how as *const OpenHow, std::mem::size_of::<OpenHow>()) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: 成功した openat2 の戻り値は新しい記述子で、この File だけが持つ。
    Ok(unsafe { File::from_raw_fd(fd as i32) })
}

/// 成分ごとに `openat(O_NOFOLLOW)` で下る。途中の成分は `O_DIRECTORY` で開き、どれも symlink なら ELOOP。
#[cfg(unix)]
fn componentwise(dirfd: i32, rel: &str) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::unix::io::{AsRawFd, FromRawFd};
    let comps: Vec<&str> = rel.split('/').filter(|c| !c.is_empty() && *c != ".").collect();
    let mut cur: Option<File> = None;
    let last = comps.len() - 1;
    for (i, c) in comps.iter().enumerate() {
        let cname = CString::new(*c).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let base = cur.as_ref().map_or(dirfd, |f| f.as_raw_fd());
        let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        if i != last {
            flags |= libc::O_DIRECTORY;
        }
        // SAFETY: base は開いている記述子 (dirfd か cur)、cname は NUL 終端。
        let fd = unsafe { libc::openat(base, cname.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: 成功した openat の戻り値は新しい記述子で、この File だけが持つ。
        cur = Some(unsafe { File::from_raw_fd(fd) });
    }
    cur.ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))
}

/// `fstat` の field (0 大きさ、1 種別 (0 ファイル / 1 ディレクトリ / 2 それ以外)、2 更新時刻 (UNIX からのナノ秒、範囲外は 0))。
pub(crate) fn stat_field(f: &File, field: u64) -> io::Result<u64> {
    let md = f.metadata()?;
    Ok(match field {
        0 => md.len(),
        1 => {
            if md.is_file() {
                0
            } else if md.is_dir() {
                1
            } else {
                2
            }
        }
        _ => md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos().min(u64::MAX as u128) as u64)
            .unwrap_or(0),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;

    /// `openat2` を使わない成分ごとの経路 (他の Unix と、カーネルに無い Linux の経路) も、symlink を拒み、通常の入れ子を開くこと。
    #[test]
    fn componentwise_refuses_symlinks_and_opens_nested_files() {
        let d = std::env::temp_dir().join(format!("ksn_fsbeneath_unit_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub").join("f.txt"), "x").unwrap();
        std::os::unix::fs::symlink("sub/f.txt", d.join("ln")).unwrap();
        std::os::unix::fs::symlink("sub", d.join("dl")).unwrap();
        let root = open_dir(d.to_str().unwrap()).unwrap();
        assert!(componentwise(root.as_raw_fd(), "sub/f.txt").is_ok());
        assert!(componentwise(root.as_raw_fd(), "ln").is_err());
        assert!(componentwise(root.as_raw_fd(), "dl/f.txt").is_err());
        assert!(open_beneath(&root, "../x").is_err());
        drop(root);
        let _ = std::fs::remove_dir_all(&d);
    }
}
