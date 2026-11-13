//! インタプリタの FFI (`extern` 関数の動的呼び出し、C から Kasane へのコールバック、動的ロード)。
//!
//! x86_64 Linux では、System V AMD64 の呼び出し規約を自前の薄いアセンブリ (`ksn_ffi_call_raw`) で実装する。
//! 引数はレジスタ (GP 6 本・XMM 8 本) とスタックへ振り分け、構造体は INTEGER / SSE / MEMORY に分類して渡す
//! (戻り値も同じ)。引数の個数の上限はない。コールバックは、固定個数の入口 (`tramp::<N>`) の表から
//! Kasane の関数ごとに 1 つ割り当て、レジスタを読んでインタプリタへ戻る。
//! 他のターゲット (AArch64・Windows など) は従来の限定版 (整数・ポインタ・浮動小数のみ、6 個まで) で、
//! 構造体の値渡し・コールバックは、呼び出しの名前つきの明確なエラーになる (ネイティブ生成なら x86_64 のみ対応)。
//! 生ポインタの読み書き (`core.ptr.read` / `write` / `add`) は、インタプリタが値の型を持たないので、
//! ポインタを作った側が `Interp::ptr_elem` (アドレス → 要素型) に覚えた型から引く。

use std::ffi::c_void;
use std::rc::Rc;

// ---------------------------------------------------------------- 動的ロード

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
    #[link(name = "dl")]
    extern "C" {
        pub fn dlopen(path: *const u8, flags: i32) -> *mut c_void;
        pub fn dlsym(handle: *mut c_void, symbol: *const u8) -> *mut c_void;
        pub fn dlerror() -> *const u8;
    }
    pub const RTLD_DEFAULT: *mut c_void = std::ptr::null_mut();
}

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::c_void;
    extern "C" {
        pub fn dlopen(path: *const u8, flags: i32) -> *mut c_void;
        pub fn dlsym(handle: *mut c_void, symbol: *const u8) -> *mut c_void;
        pub fn dlerror() -> *const u8;
    }
    pub const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
}

fn cstr(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = s.bytes().collect();
    v.push(0);
    v
}

/// 共有ライブラリを開く (同じ名前は 1 度だけ)。失敗したら OS のメッセージ。
fn open_lib(lib: &str) -> Result<*mut c_void, String> {
    thread_local! {
        static LIBS: std::cell::RefCell<std::collections::HashMap<String, usize>> = Default::default();
    }
    if let Some(h) = LIBS.with(|l| l.borrow().get(lib).copied()) {
        return Ok(h as *mut c_void);
    }
    let name = cstr(lib);
    #[cfg(windows)]
    let h = unsafe { sys::LoadLibraryA(name.as_ptr()) };
    #[cfg(unix)]
    let h = unsafe { sys::dlopen(name.as_ptr(), 2) };
    if h.is_null() {
        #[cfg(unix)]
        let why = unsafe {
            let e = sys::dlerror();
            if e.is_null() { String::new() } else { std::ffi::CStr::from_ptr(e as *const std::ffi::c_char).to_string_lossy().to_string() }
        };
        #[cfg(windows)]
        let why = String::new();
        return Err(if why.is_empty() { format!("cannot load the shared library `{}`", lib) } else { format!("cannot load the shared library `{}`: {}", lib, why) });
    }
    LIBS.with(|l| l.borrow_mut().insert(lib.to_string(), h as usize));
    Ok(h)
}

/// シンボルのアドレスを探す。`lib` があればその共有ライブラリ (`dlopen` / `LoadLibrary`)、なければホストの C ライブラリ。
pub fn lookup(lib: Option<&str>, symbol: &str) -> Result<*mut c_void, String> {
    let name = cstr(symbol);
    if let Some(lib) = lib {
        let h = open_lib(lib)?;
        #[cfg(windows)]
        let p = unsafe { sys::GetProcAddress(h, name.as_ptr()) };
        #[cfg(unix)]
        let p = unsafe { sys::dlsym(h, name.as_ptr()) };
        return if p.is_null() { Err(format!("symbol `{}` was not found in `{}`", symbol, lib)) } else { Ok(p) };
    }
    #[cfg(windows)]
    unsafe {
        for l in ["ucrtbase.dll", "msvcrt.dll", "kernel32.dll", "user32.dll"] {
            let h = sys::LoadLibraryA(cstr(l).as_ptr());
            if h.is_null() {
                continue;
            }
            let p = sys::GetProcAddress(h, name.as_ptr());
            if !p.is_null() {
                return Ok(p);
            }
        }
        Err(format!("symbol `{}` was not found in the host C libraries", symbol))
    }
    #[cfg(unix)]
    unsafe {
        let p = sys::dlsym(sys::RTLD_DEFAULT, name.as_ptr());
        if !p.is_null() {
            return Ok(p);
        }
        // 数学関数 (`sqrt` など) は libm にある。インタプリタ自身が libm を使わないとき (超越関数は自前実装) は
        // プロセスに読み込まれていないので、ホストの libm を明示的に開いて探す (macOS は libSystem にある)。
        #[cfg(not(target_os = "macos"))]
        if let Ok(h) = open_lib("libm.so.6") {
            let p = sys::dlsym(h, name.as_ptr());
            if !p.is_null() {
                return Ok(p);
            }
        }
        Err(format!("symbol `{}` was not found in the host C libraries", symbol))
    }
}

/// `link "lib:symbol"` を (ライブラリ, シンボル) に分ける。`:` が無ければライブラリなし。区切りは最後の `:`
/// (Windows のドライブ文字つきパスを許すため)。
pub fn split_link(link: &str) -> (Option<&str>, &str) {
    match link.rsplit_once(':') {
        Some((lib, sym)) if !lib.is_empty() && !sym.is_empty() => (Some(lib), sym),
        _ => (None, link),
    }
}

// ---------------------------------------------------------------- C の型の配置 (`layout c`)

/// C の型。構造体の配置は C の規則 (宣言順・自然境界・末尾を整列に切り上げ)。
#[derive(Clone, Debug)]
pub enum CTy {
    /// `i8`..`u64` / `isize` / `usize` / `bool` / `char` / `f32` / `f64` / `ptr` (生ポインタ・関数ポインタ)
    Scalar(&'static str),
    Struct(Rc<CStruct>),
    Array(Rc<CTy>, u32),
}

#[derive(Debug)]
pub struct CStruct {
    pub name: String,
    pub size: u32,
    pub align: u32,
    /// (名前, オフセット, 型)
    pub fields: Vec<(String, u32, CTy)>,
}

pub fn scalar_size(n: &str) -> u32 {
    match n {
        "i8" | "u8" | "bool" => 1,
        "i16" | "u16" => 2,
        "i32" | "u32" | "f32" | "char" => 4,
        _ => 8,
    }
}

pub fn is_float_name(n: &str) -> bool {
    n == "f32" || n == "f64"
}

impl CTy {
    pub fn size(&self) -> u32 {
        match self {
            CTy::Scalar(n) => scalar_size(n),
            CTy::Struct(s) => s.size,
            CTy::Array(e, n) => e.size() * n,
        }
    }
    pub fn align(&self) -> u32 {
        match self {
            CTy::Scalar(n) => scalar_size(n),
            CTy::Struct(s) => s.align,
            CTy::Array(e, _) => e.align(),
        }
    }
    /// 葉のスカラーを (オフセット, 大きさ, 浮動小数か) で並べる。
    pub fn flatten(&self, base: u32, out: &mut Vec<(u32, u32, bool)>) {
        match self {
            CTy::Scalar(n) => out.push((base, scalar_size(n), is_float_name(n))),
            CTy::Struct(s) => {
                for (_, off, t) in &s.fields {
                    t.flatten(base + off, out);
                }
            }
            CTy::Array(e, n) => {
                for i in 0..*n {
                    e.flatten(base + i * e.size(), out);
                }
            }
        }
    }
}

/// 構造体の配置を計算する。空の構造体は C にない (サイズ 0) のでエラー。
pub fn make_struct(name: &str, fields: Vec<(String, CTy)>) -> Result<CStruct, String> {
    if fields.is_empty() {
        return Err(format!("`layout c` struct `{}` has no fields", name));
    }
    let (mut off, mut align) = (0u32, 1u32);
    let mut out = vec![];
    for (n, t) in fields {
        let a = t.align();
        off = off.div_ceil(a) * a;
        let sz = t.size();
        out.push((n, off, t));
        off += sz;
        align = align.max(a);
    }
    Ok(CStruct { name: name.to_string(), size: off.div_ceil(align) * align, align, fields: out })
}

/// 引数・戻り値の渡し方 (System V AMD64): 8 バイトごとの分類 (`true` = SSE) か、メモリ。
#[derive(Clone, Debug, PartialEq)]
pub enum Pass {
    Regs(Vec<bool>),
    Memory,
}

pub fn classify(t: &CTy) -> Pass {
    let mut flat = vec![];
    t.flatten(0, &mut flat);
    classify_flat(&flat, t.size())
}

/// 葉のスカラー (オフセット, 大きさ, 浮動小数か) の並びと全体の大きさから分類する。
pub fn classify_flat(flat: &[(u32, u32, bool)], size: u32) -> Pass {
    if size == 0 || size > 16 {
        return Pass::Memory;
    }
    let n = size.div_ceil(8) as usize;
    // 各 8 バイトは、INTEGER のフィールドが 1 つでもあれば INTEGER、浮動小数だけなら SSE
    let mut sse = vec![true; n];
    for (off, _, f) in flat {
        if !*f {
            sse[(off / 8) as usize] = false;
        }
    }
    Pass::Regs(sse)
}

// ---------------------------------------------------------------- 呼び出し

/// 実引数。`Agg` は構造体 (メモリ上のバイト列と型)。
pub enum ArgV {
    Int(u64),
    F32(f32),
    F64(f64),
    Agg(Vec<u8>, CTy),
}

#[derive(Clone)]
pub enum RetTy {
    Void,
    Int,
    F32,
    F64,
    Agg(CTy),
}

pub enum RetV {
    None,
    Int(u64),
    F32(f32),
    F64(f64),
    Agg(Vec<u8>),
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub const GENERIC: bool = true;
#[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
pub const GENERIC: bool = false;

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
mod x64 {
    use super::*;

    #[repr(C)]
    pub struct Frame {
        pub fnptr: u64,
        pub gp: [u64; 6],
        pub sse: [u64; 8],
        pub stack: *const u64,
        pub nstack: u64,
        pub ret: [u64; 4],
    }

    std::arch::global_asm!(
        ".text",
        ".globl ksn_ffi_call_raw",
        ".type ksn_ffi_call_raw,@function",
        "ksn_ffi_call_raw:",
        "push rbp",
        "mov rbp, rsp",
        "push rbx",
        "push r12",
        "mov rbx, rdi",
        "mov rax, [rbx + 128]",
        "lea rcx, [rax*8 + 15]",
        "and rcx, -16",
        "sub rsp, rcx",
        "mov rsi, [rbx + 120]",
        "mov rdi, rsp",
        "mov rcx, rax",
        "cld",
        "rep movsq",
        "movq xmm0, [rbx + 56]",
        "movq xmm1, [rbx + 64]",
        "movq xmm2, [rbx + 72]",
        "movq xmm3, [rbx + 80]",
        "movq xmm4, [rbx + 88]",
        "movq xmm5, [rbx + 96]",
        "movq xmm6, [rbx + 104]",
        "movq xmm7, [rbx + 112]",
        "mov rdi, [rbx + 8]",
        "mov rsi, [rbx + 16]",
        "mov rdx, [rbx + 24]",
        "mov rcx, [rbx + 32]",
        "mov r8, [rbx + 40]",
        "mov r9, [rbx + 48]",
        "mov r11, [rbx]",
        "mov eax, 8",
        "call r11",
        "mov [rbx + 136], rax",
        "mov [rbx + 144], rdx",
        "movq [rbx + 152], xmm0",
        "movq [rbx + 160], xmm1",
        "lea rsp, [rbp - 16]",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
        ".size ksn_ffi_call_raw, .-ksn_ffi_call_raw",
    );

    extern "C" {
        fn ksn_ffi_call_raw(f: *mut Frame);
    }

    fn bytes_at(b: &[u8], i: usize) -> u64 {
        let mut x = [0u8; 8];
        let end = (i * 8 + 8).min(b.len());
        x[..end - i * 8].copy_from_slice(&b[i * 8..end]);
        u64::from_le_bytes(x)
    }

    pub unsafe fn call(p: *mut c_void, args: &[ArgV], ret: &RetTy) -> Result<RetV, String> {
        let mut gp: Vec<u64> = vec![];
        let mut sse: Vec<u64> = vec![];
        let mut stack: Vec<u64> = vec![];
        // 戻り値がメモリのときは、隠し引数 (戻り先のアドレス) が最初の GP
        let mut sret: Vec<u64> = vec![];
        if let RetTy::Agg(t) = ret {
            if classify(t) == Pass::Memory {
                sret = vec![0u64; (t.size() as usize).div_ceil(8) + 2];
                gp.push(sret.as_mut_ptr() as u64);
            }
        }
        for a in args {
            match a {
                ArgV::Int(v) => {
                    if gp.len() < 6 { gp.push(*v) } else { stack.push(*v) }
                }
                ArgV::F32(f) => {
                    let v = f.to_bits() as u64;
                    if sse.len() < 8 { sse.push(v) } else { stack.push(v) }
                }
                ArgV::F64(f) => {
                    let v = f.to_bits();
                    if sse.len() < 8 { sse.push(v) } else { stack.push(v) }
                }
                ArgV::Agg(b, t) => match classify(t) {
                    Pass::Regs(cls) => {
                        let need_sse = cls.iter().filter(|s| **s).count();
                        let need_gp = cls.len() - need_sse;
                        if gp.len() + need_gp <= 6 && sse.len() + need_sse <= 8 {
                            for (i, s) in cls.iter().enumerate() {
                                let v = bytes_at(b, i);
                                if *s { sse.push(v) } else { gp.push(v) }
                            }
                        } else {
                            for i in 0..b.len().div_ceil(8) {
                                stack.push(bytes_at(b, i));
                            }
                        }
                    }
                    Pass::Memory => {
                        for i in 0..b.len().div_ceil(8) {
                            stack.push(bytes_at(b, i));
                        }
                    }
                },
            }
        }
        let mut frame = Frame { fnptr: p as u64, gp: [0; 6], sse: [0; 8], stack: stack.as_ptr(), nstack: stack.len() as u64, ret: [0; 4] };
        frame.gp[..gp.len()].copy_from_slice(&gp);
        frame.sse[..sse.len()].copy_from_slice(&sse);
        ksn_ffi_call_raw(&mut frame);
        let [rax, rdx, x0, x1] = frame.ret;
        Ok(match ret {
            RetTy::Void => RetV::None,
            RetTy::Int => RetV::Int(rax),
            RetTy::F32 => RetV::F32(f32::from_bits(x0 as u32)),
            RetTy::F64 => RetV::F64(f64::from_bits(x0)),
            RetTy::Agg(t) => {
                let size = t.size() as usize;
                let mut out = vec![0u8; size.div_ceil(8) * 8];
                match classify(t) {
                    Pass::Memory => {
                        let src = std::slice::from_raw_parts(sret.as_ptr() as *const u8, size);
                        out[..size].copy_from_slice(src);
                    }
                    Pass::Regs(cls) => {
                        let (mut gi, mut si) = (0, 0);
                        for (i, s) in cls.iter().enumerate() {
                            let v = if *s {
                                si += 1;
                                if si == 1 { x0 } else { x1 }
                            } else {
                                gi += 1;
                                if gi == 1 { rax } else { rdx }
                            };
                            out[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
                        }
                    }
                }
                out.truncate(size);
                RetV::Agg(out)
            }
        })
    }
}

/// C の関数 `p` を、System V AMD64 の規則で呼ぶ (x86_64 Linux)。それ以外では `call_basic` を使う。
pub fn call_generic(p: *mut c_void, args: &[ArgV], ret: &RetTy) -> Result<RetV, String> {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    unsafe {
        x64::call(p, args, ret)
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
    {
        call_basic(p, args, ret)
    }
}

// ---------------------------------------------------------------- 従来の限定版 (x86_64 Linux 以外)

/// C の呼び出し規約での実引数 (限定版)。`f32` は、下位 32 ビットにビットパターンを置いた `f64` として渡す。
#[derive(Clone, Copy)]
enum Arg {
    Int(i64),
    F32(f32),
    F64(f64),
}

macro_rules! abi_ty {
    (I) => { i64 };
    (F) => { f64 };
}
macro_rules! abi_pick {
    (I, $args:expr, $i:tt) => { match $args.get($i) { Some(Arg::Int(v)) => *v, _ => 0 } };
    (F, $args:expr, $i:tt) => { match $args.get($i) { Some(Arg::F64(v)) => *v, Some(Arg::F32(v)) => f64::from_bits(v.to_bits() as u64), _ => 0.0 } };
}
macro_rules! abi_dispatch {
    (@wrap I, $e:expr) => { RetV::Int($e as u64) };
    (@wrap F, $e:expr) => { RetV::F64($e) };
    ($key:expr, $p:expr, $args:expr, $ret:ident; [$($c:ident $i:tt),*];) => {
        if $key == concat!($(stringify!($c)),*) {
            return Ok(abi_dispatch!(@wrap $ret, std::mem::transmute::<*mut c_void, extern "C" fn($(abi_ty!($c)),*) -> abi_ty!($ret)>($p)($(abi_pick!($c, $args, $i)),*)));
        }
    };
    ($key:expr, $p:expr, $args:expr, $ret:ident; [$($c:ident $i:tt),*]; $n:tt $($rest:tt)*) => {
        abi_dispatch!($key, $p, $args, $ret; [$($c $i,)* I $n]; $($rest)*);
        abi_dispatch!($key, $p, $args, $ret; [$($c $i,)* F $n]; $($rest)*);
    };
}

#[allow(dead_code)]
fn call_basic(p: *mut c_void, args: &[ArgV], ret: &RetTy) -> Result<RetV, String> {
    if args.len() > 6 {
        return Err("an extern call takes at most 6 arguments on this target (the interpreter's generic call is x86_64 Linux only)".into());
    }
    let mut a: Vec<Arg> = vec![];
    for x in args {
        a.push(match x {
            ArgV::Int(v) => Arg::Int(*v as i64),
            ArgV::F32(f) => Arg::F32(*f),
            ArgV::F64(f) => Arg::F64(*f),
            ArgV::Agg(..) => return Err("passing a struct by value is supported by the interpreter on x86_64 Linux only".into()),
        });
    }
    if matches!(ret, RetTy::Agg(_)) {
        return Err("returning a struct by value is supported by the interpreter on x86_64 Linux only".into());
    }
    let float_ret = matches!(ret, RetTy::F32 | RetTy::F64);
    let key: String = (0..6).map(|i| if matches!(a.get(i), Some(Arg::F32(_)) | Some(Arg::F64(_))) { 'F' } else { 'I' }).collect();
    // 引数の位置ごとのクラス (I / F) の組合せの腕を、関数型へ組み替えて呼ぶ (6 個に満たない分は 0 を渡す)。
    let r = (|| -> Result<RetV, String> {
        unsafe {
            if float_ret {
                abi_dispatch!(key, p, a, F; []; 0 1 2 3 4 5);
            } else {
                abi_dispatch!(key, p, a, I; []; 0 1 2 3 4 5);
            }
        }
        Err("this argument pattern is not supported by the interpreter".into())
    })()?;
    Ok(match (r, ret) {
        (RetV::F64(x), RetTy::F32) => RetV::F32(f32::from_bits(x.to_bits() as u32)),
        (_, RetTy::Void) => RetV::None,
        (r, _) => r,
    })
}

// ---------------------------------------------------------------- コールバック (C から Kasane)

/// コールバックの入口の表の大きさ。Kasane の関数 (またはクロージャ式) ごとに 1 つ使う。
pub const CALLBACK_SLOTS: usize = 64;

/// 入口から呼ばれるハンドラ: (スロット, GP レジスタ 6 本, XMM レジスタ 8 本の下位 64 ビット) → (RAX, XMM0)。
pub type CallbackHandler = fn(usize, &[u64; 6], &[u64; 8]) -> (u64, u64);

/// 入口の番号の振り分け: `0..VM_SLOT_BASE` は木走査 (`ffi_interp`)、`VM_SLOT_BASE..` はバイトコード VM (`vm/run.rs`)。
/// 同じプロセスで両方が動いても (`ksn test --engine=both`)、ハンドラが混ざらない。
pub const VM_SLOT_BASE: usize = 48;

static HANDLER: std::sync::OnceLock<CallbackHandler> = std::sync::OnceLock::new();
static VM_HANDLER: std::sync::OnceLock<CallbackHandler> = std::sync::OnceLock::new();

pub fn set_vm_callback_handler(h: CallbackHandler) {
    let _ = VM_HANDLER.set(h);
}

pub fn set_callback_handler(h: CallbackHandler) {
    let _ = HANDLER.set(h);
}

#[repr(C)]
pub struct RetPair(u64, f64);

#[allow(clippy::too_many_arguments)]
extern "C" fn tramp<const N: usize>(a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, f0: f64, f1: f64, f2: f64, f3: f64, f4: f64, f5: f64, f6: f64, f7: f64) -> RetPair {
    let gp = [a0, a1, a2, a3, a4, a5];
    let sse = [f0.to_bits(), f1.to_bits(), f2.to_bits(), f3.to_bits(), f4.to_bits(), f5.to_bits(), f6.to_bits(), f7.to_bits()];
    let h = if N >= VM_SLOT_BASE { VM_HANDLER.get() } else { HANDLER.get() };
    let (a, b) = (h.expect("callback handler"))(N, &gp, &sse);
    RetPair(a, f64::from_bits(b))
}

macro_rules! pool {
    ($($n:literal)*) => { [$(tramp::<$n> as *const () as usize),*] };
}

/// スロット `i` の入口 (C から見た関数ポインタ)。x86_64 Linux のみ。
pub fn trampoline(i: usize) -> Option<usize> {
    if !GENERIC || i >= CALLBACK_SLOTS {
        return None;
    }
    let t: [usize; CALLBACK_SLOTS] = pool!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31
        32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59 60 61 62 63);
    Some(t[i])
}
