//! インタプリタの FFI (`extern` 関数の動的呼び出し)。ホストの C ライブラリのシンボルを実行時に探して、
//! 整数・ポインタ・`bool` の引数 (最大 6 個) で C の呼び出し規約で呼ぶ。浮動小数の引数・戻り値と、構造体の値渡しは未対応。
//! 生ポインタの読み書き (`core.ptr.read` / `write` / `add`) は、インタプリタが値の型を持たないので未対応
//! (ネイティブ生成 `ksn build --backend clif` で使える)。

use std::ffi::c_void;

#[cfg(windows)]
mod sys {
    use std::ffi::c_void;
    extern "system" {
        pub fn LoadLibraryA(name: *const u8) -> *mut c_void;
        pub fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod sys {
    use std::ffi::c_void;
    extern "C" {
        pub fn dlsym(handle: *mut c_void, symbol: *const u8) -> *mut c_void;
    }
    pub const RTLD_DEFAULT: *mut c_void = std::ptr::null_mut();
}

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::c_void;
    extern "C" {
        pub fn dlsym(handle: *mut c_void, symbol: *const u8) -> *mut c_void;
    }
    pub const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
}

/// シンボルのアドレスを探す。
fn lookup(symbol: &str) -> Option<*mut c_void> {
    let mut name: Vec<u8> = symbol.bytes().collect();
    name.push(0);
    #[cfg(windows)]
    unsafe {
        for lib in ["ucrtbase.dll", "msvcrt.dll", "kernel32.dll", "user32.dll"] {
            let mut l: Vec<u8> = lib.bytes().collect();
            l.push(0);
            let h = sys::LoadLibraryA(l.as_ptr());
            if h.is_null() {
                continue;
            }
            let p = sys::GetProcAddress(h, name.as_ptr());
            if !p.is_null() {
                return Some(p);
            }
        }
        None
    }
    #[cfg(unix)]
    unsafe {
        let p = sys::dlsym(sys::RTLD_DEFAULT, name.as_ptr());
        if p.is_null() { None } else { Some(p) }
    }
}

/// C の関数 `symbol` を、整数の引数 `args` で呼ぶ。戻り値は 64 ビットの整数 (呼び出し側が型に合わせて切り詰める)。
pub fn call(symbol: &str, args: &[i64]) -> Result<i64, String> {
    let p = lookup(symbol).ok_or_else(|| format!("symbol `{}` was not found in the host C libraries", symbol))?;
    if args.len() > 6 {
        return Err("an extern call takes at most 6 arguments in the interpreter".into());
    }
    let a = |i: usize| args.get(i).copied().unwrap_or(0);
    unsafe {
        // C の規約では、整数の引数は先頭から順にレジスタで渡る。未使用の引数は 0 を渡してよい (呼ばれた側は読まない)。
        Ok(match args.len() {
            0 => std::mem::transmute::<*mut c_void, extern "C" fn() -> i64>(p)(),
            1 => std::mem::transmute::<*mut c_void, extern "C" fn(i64) -> i64>(p)(a(0)),
            2 => std::mem::transmute::<*mut c_void, extern "C" fn(i64, i64) -> i64>(p)(a(0), a(1)),
            3 => std::mem::transmute::<*mut c_void, extern "C" fn(i64, i64, i64) -> i64>(p)(a(0), a(1), a(2)),
            4 => std::mem::transmute::<*mut c_void, extern "C" fn(i64, i64, i64, i64) -> i64>(p)(a(0), a(1), a(2), a(3)),
            5 => std::mem::transmute::<*mut c_void, extern "C" fn(i64, i64, i64, i64, i64) -> i64>(p)(a(0), a(1), a(2), a(3), a(4)),
            _ => std::mem::transmute::<*mut c_void, extern "C" fn(i64, i64, i64, i64, i64, i64) -> i64>(p)(a(0), a(1), a(2), a(3), a(4), a(5)),
        })
    }
}
