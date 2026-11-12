//! インタプリタと Cranelift の `extern` 呼び出しで共有する C ABI の規則、およびインタプリタの FFI (ホストの C ライブラリの動的呼び出し)。
//!
//! 引数は整数系 (`ints`, 最大 6 個) と浮動小数 (`floats`, 最大 8 個) のレジスタ、その外はスタックのワード (`stack`) で渡す。
//! 構造体 (`layout c`、16 バイト以下) は eightbyte ごとに INTEGER / SSE に分類して、それぞれのレジスタに入れる。
//! レジスタが足りない構造体と 16 バイトを超える構造体はメモリ渡し (スタックに置く)。メモリに返す構造体は、
//! 呼び出し側が用意した領域へのポインタを第 1 整数引数として渡す (sret)。
//! 生ポインタの読み書きは `interp.rs` が担う (ポインタの要素型を値に持たせて、ここでは扱わない)。
//! 構造体と、2 つ以上のレジスタにまたがる戻り値は x86_64 SysV (Linux / macOS) だけで扱う。Windows と aarch64 では明示エラーにする。

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

/// 1 つの eightbyte の分類 (SysV x86_64)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// 整数レジスタ (rdi, rsi, ... / rax, rdx)。
    Int,
    /// 浮動小数レジスタ (xmm0 ..)。
    Sse,
}

/// 構造体の中のスカラー 1 つ (オフセット・大きさ・浮動小数か)。
#[derive(Clone, Copy, Debug)]
pub struct Leaf {
    pub off: u32,
    pub size: u32,
    pub float: bool,
}

/// 値の大きさ (バイト) から、eightbyte の数。
pub fn words(size: u32) -> usize {
    size.div_ceil(8) as usize
}

/// SysV x86_64 の分類。`size` バイトの値の eightbyte ごとの class を返す。
/// 16 バイトを超えるもの (MEMORY class) と大きさ 0 は None。`leaves` は値の中のスカラーの並び。
/// eightbyte の中のスカラーが全部浮動小数なら SSE、1 つでも整数なら INTEGER (SysV の併合規則)。
pub fn classify(size: u32, leaves: &[Leaf]) -> Option<Vec<Class>> {
    if size == 0 || size > 16 {
        return None;
    }
    let mut out = vec![];
    for k in 0..words(size) {
        let (lo, hi) = (8 * k as u32, 8 * k as u32 + 8);
        let inside: Vec<&Leaf> = leaves.iter().filter(|l| l.off < hi && l.off + l.size > lo).collect();
        let sse = !inside.is_empty() && inside.iter().all(|l| l.float);
        out.push(if sse { Class::Sse } else { Class::Int });
    }
    Some(out)
}

/// 引数・戻り値の場所 (番号は、その種類の中での順番)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loc {
    Int(usize),
    Sse(usize),
    /// スタックの引数の並びの何番目のワードか (呼び出し側が `stack` に積む順)。
    Stack(usize),
}

pub const INT_REGS: usize = 6;
pub const SSE_REGS: usize = 8;
/// インタプリタがスタックに積むワードの上限。
pub const MAX_STACK_WORDS: usize = 8;

/// 引数をレジスタとスタックに割り当てる (SysV の規則。引数は宣言の順に置く)。
pub struct Placer {
    ints: usize,
    sses: usize,
    stack: usize,
}

impl Placer {
    /// `sret` が真なら、先頭の整数レジスタは戻り値の領域へのポインタに使われている。
    pub fn new(sret: bool) -> Self {
        Placer { ints: sret as usize, sses: 0, stack: 0 }
    }

    /// 1 つの引数を置く。`classes` が None (メモリ渡し) か、残りのレジスタに全体が入らないときは、
    /// 全部をスタックのワードに置く (SysV: 構造体はレジスタに全部入るか、全部スタックに入る)。
    pub fn place(&mut self, classes: Option<&[Class]>, size: u32) -> Vec<Loc> {
        if let Some(cs) = classes {
            let ni = cs.iter().filter(|c| **c == Class::Int).count();
            let ns = cs.len() - ni;
            if self.ints + ni <= INT_REGS && self.sses + ns <= SSE_REGS {
                let mut out = vec![];
                for c in cs {
                    out.push(match c {
                        Class::Int => {
                            self.ints += 1;
                            Loc::Int(self.ints - 1)
                        }
                        Class::Sse => {
                            self.sses += 1;
                            Loc::Sse(self.sses - 1)
                        }
                    });
                }
                return out;
            }
        }
        (0..words(size.max(1))).map(|_| self.stack_word()).collect()
    }

    fn stack_word(&mut self) -> Loc {
        self.stack += 1;
        Loc::Stack(self.stack - 1)
    }

    /// スタックに積むワードの数。
    pub fn stack_words(&self) -> usize {
        self.stack
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
    /// 1 つの eightbyte の INTEGER 分類の構造体もこれで受ける。
    Int,
    /// `f32`。
    F32,
    /// `f64`。1 つの eightbyte の SSE 分類の構造体は、この 64 ビットをそのまま読む。
    F64,
    /// 2 つの eightbyte の構造体 (9〜16 バイト)。(1 つ目の class, 2 つ目の class)。
    Pair(Class, Class),
}

/// 呼び出しの結果。
#[derive(Clone, Copy, Debug)]
pub enum Out {
    Int(i64),
    F32(f32),
    F64(f64),
    /// 2 つの eightbyte のビット列 (SSE の eightbyte も、ビットとして返す)。
    Pair(u64, u64),
}

/// 2 つの eightbyte の戻り値を、SysV の分類どおりのレジスタ対 (rax:rdx / rax:xmm0 / xmm0:rax / xmm0:xmm1) で受ける repr(C) 構造体。
#[cfg(all(unix, target_arch = "x86_64"))]
#[repr(C)]
#[derive(Clone, Copy)]
struct P<A, B>(A, B);

/// C の関数 `symbol` を呼ぶ。`ints` (最大 6 個) と `floats` (最大 8 個) は、それぞれのレジスタの番号順に入れる。
/// `stack` はスタックに積むワードを宣言の順に入れる (最大 `MAX_STACK_WORDS`)。
pub fn call_typed(symbol: &str, ints: &[i64], floats: &[FArg], stack: &[i64], ret: Ret) -> Result<Out, String> {
    let p = lookup(symbol).ok_or_else(|| format!("symbol `{}` was not found in the host C libraries", symbol))?;
    if ints.len() > INT_REGS {
        return Err("an extern call takes at most 6 integer register arguments in the interpreter".into());
    }
    if floats.len() > SSE_REGS {
        return Err("an extern call takes at most 8 floating-point register arguments in the interpreter".into());
    }
    if stack.len() > MAX_STACK_WORDS {
        return Err("an extern call passes at most 8 words on the stack in the interpreter".into());
    }
    let mut i = [0i64; INT_REGS];
    i[..ints.len()].copy_from_slice(ints);
    let mut f = [0f64; SSE_REGS];
    for (k, x) in floats.iter().enumerate() {
        // `float` は下位 32 ビットに値のビットを入れた `f64` として渡す (xmm の下位 32 ビットが `float` として読まれる)。
        f[k] = match x {
            FArg::F64(x) => *x,
            FArg::F32(x) => f64::from_bits(x.to_bits() as u64),
        };
    }
    let mut s = [0i64; MAX_STACK_WORDS];
    s[..stack.len()].copy_from_slice(stack);
    #[cfg(windows)]
    {
        if !floats.is_empty() || ret != Ret::Int || !stack.is_empty() {
            return Err("floating-point arguments, results and stack arguments of extern calls are not supported on Windows by the interpreter".into());
        }
    }
    #[cfg(not(any(windows, all(unix, target_arch = "x86_64"))))]
    {
        if !stack.is_empty() || matches!(ret, Ret::Pair(..)) {
            return Err("stack arguments and two-word results of extern calls are supported by the interpreter only on x86_64 Unix".into());
        }
    }
    Ok(unsafe { call_regs(p, &i, &f, &s, ret) })
}

/// Windows x64: 引数の位置が決め手なので、整数だけの 6 引数で呼ぶ (従来どおり)。
#[cfg(windows)]
unsafe fn call_regs(p: *mut c_void, i: &[i64; 6], _f: &[f64; 8], _s: &[i64; 8], _ret: Ret) -> Out {
    Out::Int(std::mem::transmute::<*mut c_void, extern "C" fn(i64, i64, i64, i64, i64, i64) -> i64>(p)(i[0], i[1], i[2], i[3], i[4], i[5]))
}

/// System V x86_64 (Linux / macOS): 整数と浮動小数は別々のレジスタ列に割り当てられる。両方を埋めた関数型で呼ぶ。
/// 関数型は可変長 (`, ...`) にする。可変長の関数を呼ぶ側は `%al` に xmm レジスタの数を入れる約束で、非可変長の型で呼ぶと
/// `snprintf` などが浮動小数の引数を読み捨てる。名前付きの引数は可変長の型でもレジスタに入るので、非可変長の C 関数も変わらない。
/// 後ろの 8 ワードは整数・浮動小数のレジスタを使い切った後なので、スタックに宣言の順に積まれる (余りは 0。呼ばれた側は読まない)。
#[cfg(all(unix, target_arch = "x86_64"))]
unsafe fn call_regs(p: *mut c_void, i: &[i64; 6], f: &[f64; 8], s: &[i64; 8], ret: Ret) -> Out {
    let (i0, i1, i2, i3, i4, i5) = (i[0], i[1], i[2], i[3], i[4], i[5]);
    let (f0, f1, f2, f3, f4, f5, f6, f7) = (f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7]);
    let (s0, s1, s2, s3, s4, s5, s6, s7) = (s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]);
    match ret {
        Ret::Int => Out::Int(std::mem::transmute::<
            *mut c_void,
            unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, f64, f64, f64, f64, f64, f64, f64, f64, ...) -> i64,
        >(p)(i0, i1, i2, i3, i4, i5, f0, f1, f2, f3, f4, f5, f6, f7, s0, s1, s2, s3, s4, s5, s6, s7)),
        Ret::F64 => Out::F64(std::mem::transmute::<
            *mut c_void,
            unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, f64, f64, f64, f64, f64, f64, f64, f64, ...) -> f64,
        >(p)(i0, i1, i2, i3, i4, i5, f0, f1, f2, f3, f4, f5, f6, f7, s0, s1, s2, s3, s4, s5, s6, s7)),
        Ret::F32 => Out::F32(std::mem::transmute::<
            *mut c_void,
            unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, f64, f64, f64, f64, f64, f64, f64, f64, ...) -> f32,
        >(p)(i0, i1, i2, i3, i4, i5, f0, f1, f2, f3, f4, f5, f6, f7, s0, s1, s2, s3, s4, s5, s6, s7)),
        Ret::Pair(c0, c1) => {
            use Class::{Int, Sse};
            let ib = |x: u64| x;
            let sb = |x: f64| x.to_bits();
            macro_rules! pair {
                ($a:ty, $b:ty, $ea:expr, $eb:expr) => {{
                    let r = std::mem::transmute::<
                        *mut c_void,
                        unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, f64, f64, f64, f64, f64, f64, f64, f64, ...) -> P<$a, $b>,
                    >(p)(i0, i1, i2, i3, i4, i5, f0, f1, f2, f3, f4, f5, f6, f7, s0, s1, s2, s3, s4, s5, s6, s7);
                    Out::Pair($ea(r.0), $eb(r.1))
                }};
            }
            match (c0, c1) {
                (Int, Int) => pair!(u64, u64, ib, ib),
                (Int, Sse) => pair!(u64, f64, ib, sb),
                (Sse, Int) => pair!(f64, u64, sb, ib),
                (Sse, Sse) => pair!(f64, f64, sb, sb),
            }
        }
    }
}

/// その他の Unix (aarch64 など): 引数は従来どおり整数 6 個と浮動小数 8 個のレジスタだけ (`call_typed` が他を拒否する)。
#[cfg(all(unix, not(target_arch = "x86_64")))]
unsafe fn call_regs(p: *mut c_void, i: &[i64; 6], f: &[f64; 8], _s: &[i64; 8], ret: Ret) -> Out {
    let (i0, i1, i2, i3, i4, i5) = (i[0], i[1], i[2], i[3], i[4], i[5]);
    let (f0, f1, f2, f3, f4, f5, f6, f7) = (f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7]);
    match ret {
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
        Ret::Pair(..) => unreachable!("call_typed refuses two-word results on this target"),
    }
}
