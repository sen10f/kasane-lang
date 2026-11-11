//! インタプリタの FFI (`extern` 関数の動的呼び出し)。ホストの C ライブラリのシンボルを実行時に探して、
//! 整数・ポインタ・`bool` は GP レジスタ、`f32` / `f64` は XMM レジスタで、C の呼び出し規約で呼ぶ
//! (引数は最大 6 個)。構造体の値渡しは未対応。
//! 生ポインタの読み書き (`core.ptr.read` / `write` / `add`) は、インタプリタが値の型を持たないので、
//! ポインタを作った側が `Interp::ptr_elem` (アドレス → 要素型) に覚えた型から引く。分からない
//! アドレスは未対応 (ネイティブ生成 `ksn build --backend clif` で使える)。

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

/// C の呼び出し規約での実引数。整数・ポインタ・`bool` は GP レジスタ、`f32` / `f64` は XMM レジスタで
/// 渡る。`f32` は、下位 32 ビットにビットパターンを置いた `f64` として渡す (どちらも XMM に置かれ、
/// 呼ばれた側は下位 32 ビットを `float` として読む) ので、引数の型は位置ごとに GP / XMM の 2 種類で足りる。
#[derive(Clone, Copy)]
pub enum Arg {
    Int(i64),
    F32(f32),
    F64(f64),
}

/// 呼び出しの戻り値 (レジスタの生の中身)。`Sse` は XMM0 の 64 ビットで、`float` を返す関数は下位
/// 32 ビットにビットパターンが入っている (呼び出し側が宣言の型に合わせて読み直す)。
pub enum Ret {
    Int(i64),
    Sse(f64),
}

/// 引数の位置のクラス (`i64` = GP レジスタ / `f64` = XMM レジスタ) を型にする。
macro_rules! abi_ty {
    (I) => { i64 };
    (F) => { f64 };
}

/// クラスに合わせて実引数を取り出す。
macro_rules! abi_pick {
    (I, $args:expr, $i:tt) => { match $args.get($i) { Some(Arg::Int(v)) => *v, _ => 0 } };
    (F, $args:expr, $i:tt) => { match $args.get($i) { Some(Arg::F64(v)) => *v, Some(Arg::F32(v)) => f64::from_bits(v.to_bits() as u64), _ => 0.0 } };
}

/// クラスに合わせて戻り値を包む。
macro_rules! abi_ret {
    (I, $e:expr) => { Ret::Int($e) };
    (F, $e:expr) => { Ret::Sse($e) };
}

/// 引数位置ごとのクラスの組合せ (2^6 = 64 通り) ぶんの腕を機械的に作る。
/// 引数のクラスは位置ごとの `I` / `F` の並び (文字列) で表し、末尾の `0 1 2 3 4 5` は引数の添字。
macro_rules! abi_dispatch {
    // すべての引数位置にクラスを割り当て終えたら、その組合せの腕を 1 つ出す。
    ($key:expr, $p:expr, $args:expr, $ret:ident; [$($c:ident $i:tt),*];) => {
        if $key == concat!($(stringify!($c)),*) {
            return Ok(abi_ret!($ret, std::mem::transmute::<*mut c_void, extern "C" fn($(abi_ty!($c)),*) -> abi_ty!($ret)>($p)($(abi_pick!($c, $args, $i)),*)));
        }
    };
    // 未使用の引数位置に `I` / `F` を割り当てて再帰する。
    ($key:expr, $p:expr, $args:expr, $ret:ident; [$($c:ident $i:tt),*]; $n:tt $($rest:tt)*) => {
        abi_dispatch!($key, $p, $args, $ret; [$($c $i,)* I $n]; $($rest)*);
        abi_dispatch!($key, $p, $args, $ret; [$($c $i,)* F $n]; $($rest)*);
    };
}

/// C の関数 `symbol` を、クラス付けした引数 `args` で呼ぶ。`float_ret` は戻り値が `f32` / `f64` か。
pub fn call(symbol: &str, args: &[Arg], float_ret: bool) -> Result<Ret, String> {
    let p = lookup(symbol).ok_or_else(|| format!("symbol `{}` was not found in the host C libraries", symbol))?;
    if args.len() > 6 {
        return Err("an extern call takes at most 6 arguments in the interpreter".into());
    }
    let a = |i: usize| match args.get(i) {
        Some(Arg::Int(v)) => *v,
        _ => 0,
    };
    unsafe {
        // 引数も戻り値も整数・ポインタだけの呼び出しは、従来どおり整数の型で呼ぶ (浮動小数の無い最短路)。
        if !float_ret && args.iter().all(|x| matches!(x, Arg::Int(_))) {
            return Ok(Ret::Int(match args.len() {
                0 => std::mem::transmute::<*mut c_void, extern "C" fn() -> i64>(p)(),
                1 => std::mem::transmute::<*mut c_void, extern "C" fn(i64) -> i64>(p)(a(0)),
                2 => std::mem::transmute::<*mut c_void, extern "C" fn(i64, i64) -> i64>(p)(a(0), a(1)),
                3 => std::mem::transmute::<*mut c_void, extern "C" fn(i64, i64, i64) -> i64>(p)(a(0), a(1), a(2)),
                4 => std::mem::transmute::<*mut c_void, extern "C" fn(i64, i64, i64, i64) -> i64>(p)(a(0), a(1), a(2), a(3)),
                5 => std::mem::transmute::<*mut c_void, extern "C" fn(i64, i64, i64, i64, i64) -> i64>(p)(a(0), a(1), a(2), a(3), a(4)),
                _ => std::mem::transmute::<*mut c_void, extern "C" fn(i64, i64, i64, i64, i64, i64) -> i64>(p)(a(0), a(1), a(2), a(3), a(4), a(5)),
            }));
        }
    }
    // 浮動小数を含む呼び出しは、位置ごとのクラスに合う関数型へ組み替える。6 個に満たない分は整数として
    // 0 を渡す (呼ばれた側は読まない)。
    let key: String = (0..6).map(|i| if matches!(args.get(i), Some(Arg::F32(_)) | Some(Arg::F64(_))) { 'F' } else { 'I' }).collect();
    unsafe {
        if float_ret {
            abi_dispatch!(key, p, args, F; []; 0 1 2 3 4 5);
        } else {
            abi_dispatch!(key, p, args, I; []; 0 1 2 3 4 5);
        }
    }
    Err(format!("extern call to `{}`: this argument pattern is not supported by the interpreter", symbol))
}
