//! インタプリタの FFI (`extern` 関数の動的呼び出し)。ホストの C ライブラリのシンボルを実行時に探して、
//! 整数・ポインタ・`bool` の引数 (最大 6 個) と `f32` / `f64` の引数 (最大 8 個) で C の呼び出し規約で呼ぶ。
//! 戻り値は整数・ポインタ・`bool`・`f32` / `f64`。構造体の値渡しは未対応。
//! 生ポインタの読み書きは `interp.rs` が担う (ポインタの要素型を値に持たせて、ここでは扱わない)。
//! 浮動小数の引数・戻り値は Windows では未対応 (x64 の引数規約が位置ごとに整数レジスタか xmm を選ぶため)。

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

/// 浮動小数の引数。`F32` は C の `float` として渡す。
#[derive(Clone, Copy, Debug)]
pub enum FArg {
    F32(f32),
    F64(f64),
}

/// 戻り値の読み方 (宣言の型に合わせて呼び出し側が選ぶ)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ret {
    /// 整数・ポインタ・`bool` (64 ビットで受け、呼び出し側が型に合わせて切り詰める)。
    Int,
    F32,
    F64,
}

/// 呼び出しの結果。
#[derive(Clone, Copy, Debug)]
pub enum Out {
    Int(i64),
    F32(f32),
    F64(f64),
}

/// C の関数 `symbol` を呼ぶ。整数系の引数 `ints` (最大 6 個) と浮動小数の引数 `floats` (最大 8 個) を、
/// 種類ごとに先頭から順に渡す。
///
/// System V (Linux / macOS) と AAPCS64 では、整数と浮動小数の引数は別々のレジスタ列に割り当てられる
/// (整数は rdi.. / x0.., 浮動小数は xmm0.. / v0..)。そこで両方のレジスタを埋めた関数型で呼ぶ。使わない引数は 0 にする
/// (呼ばれた側は読まない)。`float` の引数は、下位 32 ビットに値のビットを入れた `f64` として渡す (xmm の下位 32 ビットが `float` として読まれる)。
pub fn call_typed(symbol: &str, ints: &[i64], floats: &[FArg], ret: Ret) -> Result<Out, String> {
    let p = lookup(symbol).ok_or_else(|| format!("symbol `{}` was not found in the host C libraries", symbol))?;
    if ints.len() > 6 {
        return Err("an extern call takes at most 6 integer arguments in the interpreter".into());
    }
    if floats.len() > 8 {
        return Err("an extern call takes at most 8 floating-point arguments in the interpreter".into());
    }
    let a = |i: usize| ints.get(i).copied().unwrap_or(0);
    #[cfg(windows)]
    unsafe {
        if !floats.is_empty() || ret != Ret::Int {
            return Err("floating-point arguments and results of extern calls are not supported on Windows by the interpreter".into());
        }
        // Windows x64 の規約では引数の位置が決め手なので、整数だけの 6 引数で呼ぶ (従来どおり)。
        Ok(Out::Int(std::mem::transmute::<*mut c_void, extern "C" fn(i64, i64, i64, i64, i64, i64) -> i64>(p)(
            a(0),
            a(1),
            a(2),
            a(3),
            a(4),
            a(5),
        )))
    }
    #[cfg(not(windows))]
    {
        let f = |i: usize| -> f64 {
            match floats.get(i) {
                Some(FArg::F64(x)) => *x,
                Some(FArg::F32(x)) => f64::from_bits(x.to_bits() as u64),
                None => 0.0,
            }
        };
        let (i0, i1, i2, i3, i4, i5) = (a(0), a(1), a(2), a(3), a(4), a(5));
        let (f0, f1, f2, f3, f4, f5, f6, f7) = (f(0), f(1), f(2), f(3), f(4), f(5), f(6), f(7));
        // 関数型は可変長 (`, ...`) にする。System V x86_64 では、可変長の関数を呼ぶ側が `%al` に使う xmm レジスタ数を
        // 入れる約束で、非可変長の型で呼ぶと `%al` が 0 のままになり、`snprintf` などが浮動小数の引数を読み捨てる。
        // 名前付きの引数は可変長の型でも従来どおりレジスタに入るので、非可変長の C 関数の呼び出しも変わらない。
        // AAPCS64 (Linux) では可変長の引数もレジスタ渡しなので同じ型で足りる (macOS arm64 の可変長の引数はスタック渡しで未対応)。
        unsafe {
            Ok(match ret {
                Ret::Int => Out::Int(std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, f64, f64, f64, f64, f64, f64, f64, f64, ...) -> i64,
                >(p)(i0, i1, i2, i3, i4, i5, f0, f1, f2, f3, f4, f5, f6, f7)),
                Ret::F64 => Out::F64(std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, f64, f64, f64, f64, f64, f64, f64, f64, ...) -> f64,
                >(p)(i0, i1, i2, i3, i4, i5, f0, f1, f2, f3, f4, f5, f6, f7)),
                Ret::F32 => Out::F32(std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, f64, f64, f64, f64, f64, f64, f64, f64, ...) -> f32,
                >(p)(i0, i1, i2, i3, i4, i5, f0, f1, f2, f3, f4, f5, f6, f7)),
            })
        }
    }
}
