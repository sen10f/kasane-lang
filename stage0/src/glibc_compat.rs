//! 配布用 (`--features glibc-compat`、glibc 2.31 を対象にする cargo-zigbuild) のときだけ入る小さな互換層。
//! 新しい glibc のヘッダでビルドされた静的 LLVM が参照する C23 版の `strto*` (glibc 2.38 の `__isoc23_*`) を、
//! 古い glibc に在る `strto*` へ回す。Debian 12 (glibc 2.36) などで動かすために要る。
//! `__isoc23_strto*` は C23 の `0b` 接頭辞 (基数 0 か 2) の解釈だけが `strto*` と違う。LLVM / libstdc++ の呼び出し点
//! (JSON.cpp・TargetInstrInfo.cpp・rpmalloc・`std::stoi` 系) はすべて基数 10 なので、`strto*` に回して同じ結果になる。
use std::os::raw::{c_char, c_int, c_long, c_longlong, c_ulong, c_ulonglong};

#[no_mangle]
pub unsafe extern "C" fn __isoc23_strtol(s: *const c_char, e: *mut *mut c_char, b: c_int) -> c_long {
    libc::strtol(s, e, b)
}
#[no_mangle]
pub unsafe extern "C" fn __isoc23_strtoul(s: *const c_char, e: *mut *mut c_char, b: c_int) -> c_ulong {
    libc::strtoul(s, e, b)
}
#[no_mangle]
pub unsafe extern "C" fn __isoc23_strtoll(s: *const c_char, e: *mut *mut c_char, b: c_int) -> c_longlong {
    libc::strtoll(s, e, b)
}
#[no_mangle]
pub unsafe extern "C" fn __isoc23_strtoull(s: *const c_char, e: *mut *mut c_char, b: c_int) -> c_ulonglong {
    libc::strtoull(s, e, b)
}

// glibc 2.31 を対象にするとき、静的リンクした libstdc++ / LLVM が参照する新しい名前 (mallinfo2 は 2.33、
// `__libc_single_threaded` は 2.32、arc4random は 2.36)。いずれも古い glibc には無いので、ここで補う。
// `__libc_single_threaded = 0` は「複数スレッドかもしれない」(原子操作を省かない) 側の保守的な値。
#[no_mangle]
pub static mut __libc_single_threaded: u8 = 0;

#[repr(C)]
pub struct Mallinfo2 {
    f: [usize; 10],
}

/// メモリ使用量の統計 (LLVM の Process::GetMallocUsage が見るだけ。診断用なので 0 を返す)。
#[no_mangle]
pub extern "C" fn mallinfo2() -> Mallinfo2 {
    Mallinfo2 { f: [0; 10] }
}

/// 呼び出し元は libstdc++ の `std::random_device` と LLVM Support の `llvm::sys::Process::GetRandom` 系。
/// glibc の arc4random と同じく、失敗も縮退も許さない: EINTR は繰り返し、それ以外の失敗は abort する (弱い値は返さない)。
#[no_mangle]
pub unsafe extern "C" fn arc4random() -> u32 {
    let mut v: u32 = 0;
    let mut got = 0usize;
    while got < 4 {
        let n = libc::getrandom((&mut v as *mut u32 as *mut u8).add(got) as *mut libc::c_void, 4 - got, 0);
        if n < 0 {
            if *libc::__errno_location() == libc::EINTR {
                continue;
            }
            libc::abort();
        }
        if n == 0 {
            libc::abort();
        }
        got += n as usize;
    }
    v
}
