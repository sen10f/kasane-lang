//! VM の実行ループ (設計_KIR.md 4.2 / 4.3)。呼び出しは明示的なフレーム列 (Rust の再帰を使わない)。
//!
//! 意味は参照実行器 (`kir_exec.rs`) と同じ: panic の文言、呼び出し深さの上限 (`kir_exec::MAX_DEPTH`、超えたら 102)。値の表現は `bc.rs` の先頭の通り
//! (8 バイトのスロット、集成体と `Option` / `Result` はフレームの中のスロット列。置き場も戻るときの複製もない)。違いは次の 3 点:
//! - バイト記憶は実メモリ (`malloc` / `realloc` / `free` の実アドレス。7 章 1 の決定)。仮想アドレスの検査・未初期化の検出・漏れの検査はしない
//!   (null 付近のアドレスだけは内部エラーにする)。
//! - 定義される前の読みを検出しない (検査は `verify_module` の責務)。
//! - stdout は書き込みをためて、終了・panic・stderr への出力の前に流す。

#![allow(unused_unsafe)]

pub mod dbg;
mod netsys;
mod rtsys;

use super::bc::*;
use crate::interp::{py_float, py_float32};
use crate::kir::{payload_of, Aop, Mode, Ty, TAG_BIT};
use crate::kir_exec::{self, Outcome};
use crate::kir_scalar;

/// 呼び出し深さの上限 (`kir_exec::MAX_DEPTH` と同じ)。

extern "C" {
    fn malloc(n: usize) -> *mut std::ffi::c_void;
    fn calloc(n: usize, m: usize) -> *mut std::ffi::c_void;
    fn realloc(p: *mut std::ffi::c_void, n: usize) -> *mut std::ffi::c_void;
    fn free(p: *mut std::ffi::c_void);
}

thread_local! {
    /// いま `CallExt` で C を呼んでいる `Vm` (コールバックの入口が戻る先)。
    static CUR_VM: std::cell::Cell<*mut Vm<'static>> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}

/// `ffi::tramp` の VM 用ハンドラ。
fn vm_cb_handler(
    slot: usize,
    gp: &[u64; 6],
    sse: &[u64; 8],
    _stack: *const u64,
    out: &mut crate::ffi::RetRegs,
) {
    let p = CUR_VM.with(|c| c.get());
    if p.is_null() {
        eprintln!("panic: a C callback was called outside of an extern call");
        std::process::exit(101);
    }
    // SAFETY: 呼び出し元の `CallExt` は、C から戻るまで `self` に触らない。
    let vm = unsafe { &mut *p };
    vm.run_callback(slot - crate::ffi::VM_SLOT_BASE, gp, sse, _stack, out)
}

const PROF: bool = option_env!("KSN_VM_PROF").is_some();
static PROF_T: std::sync::Mutex<Vec<(std::mem::Discriminant<Ins>, u64, String)>> = std::sync::Mutex::new(Vec::new());
fn prof_count(ins: &Ins) {
    let d = std::mem::discriminant(ins);
    let mut t = PROF_T.lock().unwrap();
    match t.iter_mut().find(|e| e.0 == d) {
        Some(e) => e.1 += 1,
        None => t.push((d, 1, format!("{:?}", ins).split(|c| c == ' ' || c == '(').next().unwrap().to_string())),
    }
}
pub fn prof_report() {
    if !PROF {
        return;
    }
    let t = PROF_T.lock().unwrap();
    let total: u64 = t.iter().map(|e| e.1).sum();
    let mut v: Vec<(u64, &str)> = t.iter().map(|e| (e.1, e.2.as_str())).collect();
    v.sort();
    v.reverse();
    eprintln!("[prof] total {}", total);
    for (n, name) in v.iter().take(14) {
        eprintln!("[prof] {:>10} {:5.1}% {}", n, *n as f64 * 100.0 / total as f64, name);
    }
}

struct Frame {
    /// 戻り先 (呼び出し元) の関数・命令番号・フレームの底。`v` は呼び出し元の関数が属する版 (`Vm::vers` の添字)。
    v: u32,
    f: u32,
    pc: u32,
    base: u32,
    /// 呼び出し元で結果を受けるスロット。
    dst: S,
    /// 呼び出し先が始まったときのフレームの領域の数。
    abase: usize,
}

pub struct Vm<'a> {
    prog: &'a Prog,
    regs: Vec<u64>,
    frames: Vec<Frame>,
    /// 最後に戻った関数の戻り値 (`ret_w` スロット)。
    pub result: Vec<u64>,
    /// 関数のフレームの領域 (`frame.bytes` / `alloca`)。戻るときに解放する。
    allocs: Vec<*mut std::ffi::c_void>,
    out: Vec<u8>,
    /// `Some` なら stderr へ書かずにためる (テストの panic の文言を取り出すため)。
    pub capture: Option<Vec<u8>>,
    /// プログラムへの引数 (`std.env.arg`。プログラム名は含まない)。
    pub args: Vec<String>,
    /// `std.net` のソケット表 (記述子 → ソケット)。
    socks: std::collections::HashMap<i128, NetSock>,
    /// `std.fs` のファイル表 (記述子 → ファイル)。記述子の番号は `next_fd` と共有する。
    files: std::collections::HashMap<i128, std::fs::File>,
    /// `std.process` の未起動の `Command` (記述子 → 設定) と、起動した子 (pid → `Child`)。
    cmds: std::collections::HashMap<i32, std::process::Command>,
    /// 不正な環境変数の鍵・値を渡された `Command` (起動で `InvalidInput`。`env_clear` で解除。木走査と同じ)。
    bad_cmds: std::collections::HashSet<i32>,
    children: std::collections::HashMap<i32, std::process::Child>,
    next_fd: i128,
    /// `block_on_threads` の共有の核 (注入列・完了の印。`run/rtsys.rs`)。
    mt: rtsys::MtCore,
    /// 非同期ソケットのリアクター (epoll)。
    io: rtsys::IoCore,
    /// コールバックのフレームを積む位置 (`CallExt` の呼び出し元のフレームの上)。
    top: usize,
    /// 最後の panic が起きた (版, 関数番号, 命令番号)。
    panic_at: Option<(u32, u32, u32)>,
    /// C に渡した関数 (版, 関数番号)。添字がコールバックの入口の番号。
    cbs: Vec<(u32, u32)>,
    /// ホットリロードの版 (0 が最初)。各フレームは、呼び出されたときの版の関数を最後まで走らせる。新しい呼び出しは最新の版へ解決する。
    pub(super) vers: Vec<*const Prog>,
    pub(super) owned: Vec<Box<Prog>>,
    /// `--watch` のとき。安全点 (呼び出しの入口) でエポック (`flag`) を見て、新しい版を取り込む。
    pub(super) hook: Option<super::reload::Reload>,
    /// `ksn debug`: デバッガ (`Ins::Line` で呼ぶ)。付いていなければ何もしない。
    pub(super) dbg: Option<Box<dbg::Dbg>>,
    /// `ksn debug`: 出力の横取り (1 = stdout、2 = stderr)。
    pub(super) out_hook: Option<Box<dyn FnMut(u8, &[u8])>>,
    /// 最後の panic が起きたフレームの底。
    panic_base: usize,
    /// `std.net` の読み・書きの期限 (記述子 → (読み, 書き))。
    deadlines: std::collections::HashMap<i128, (Option<std::time::Instant>, Option<std::time::Instant>)>,
}

enum NetSock {
    Listener(std::net::TcpListener),
    Stream(std::net::TcpStream),
    Udp(std::net::UdpSocket),
}

#[inline(always)]
fn fast_lo_hi(t: u8) -> (i128, i128) {
    (LO[t as usize], HI[t as usize])
}

fn bad_addr(a: u64, n: usize) -> String {
    if a < 0x1000 {
        // drop したスロットは番地を 0 にするので、ドロップ済みの値を借用越しに読むとここに来る。
        return format!("不正なメモリアクセス {:#x} ({} バイト): 解放済みの値の可能性 (use after free)", a, n);
    }
    format!("不正なメモリアクセス {:#x} ({} バイト)", a, n)
}

#[inline(always)]
fn addr(a: u64, n: usize) -> Result<*mut u8, String> {
    match super::arena::check(a, n) {
        Some(p) => {
            if super::arena::dead(a, n) {
                return Err(format!("use after free: 解放済みの領域を読み書きした {:#x} ({} バイト)", a, n));
            }
            Ok(p)
        }
        None => Err(bad_addr(a, n)),
    }
}

/// スロットの表現 → 参照実行器の `i128` の符号化 (型 `t`)。
#[inline(always)]
fn rd(rp: *mut u64, t: Ty, s: usize) -> i128 {
    unsafe {
        match t {
            Ty::I128 | Ty::U128 => (((*rp.add(s + 1) as u128) << 64) | *rp.add(s) as u128) as i128,
            t if t.signed() => *rp.add(s) as i64 as i128,
            _ => *rp.add(s) as i128,
        }
    }
}

/// 参照実行器の `i128` の符号化 → スロットの表現 (型 `t`)。
#[inline(always)]
fn wr(rp: *mut u64, t: Ty, s: usize, v: i128) {
    unsafe {
        match t {
            Ty::I128 | Ty::U128 => {
                *rp.add(s) = v as u64;
                *rp.add(s + 1) = (v >> 64) as u64;
            }
            _ => *rp.add(s) = v as u64,
        }
    }
}

/// アドレス `a` から `k` の幅で読む (128 ビットは呼び出し側が 2 語で読む)。
#[inline(always)]
unsafe fn ld_k(a: *const u8, k: LK) -> u64 {
    match k {
        LK::U8 => *a as u64,
        LK::I8 => *(a as *const i8) as i64 as u64,
        LK::Bool => (*a != 0) as u64,
        LK::U16 => (a as *const u16).read_unaligned() as u64,
        LK::I16 => (a as *const i16).read_unaligned() as i64 as u64,
        LK::U32 => (a as *const u32).read_unaligned() as u64,
        LK::I32 => (a as *const i32).read_unaligned() as i64 as u64,
        LK::U64 | LK::I64 | LK::I128 => (a as *const u64).read_unaligned(),
    }
}

/// `bytes` を `d` から始まるスロット列に書く (末尾のスロットの余りは 0)。
fn put_bytes(rp: *mut u64, d: usize, bytes: &[u8]) {
    let ns = (bytes.len() + 7) / 8;
    unsafe {
        for i in 0..ns {
            *rp.add(d + i) = 0;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), rp.add(d) as *mut u8, bytes.len());
    }
}

/// `std.io.Error { kind, code }` のバイト列 (型表の `std.io.Error` と、その `kind` の列挙型を探す)。
fn io_error_bytes(aggs: &[crate::kir::AggDef], kind: &str, code: i32) -> Result<Vec<u8>, String> {
    use crate::kir::AggKind;
    let d = aggs.iter().find(|d| d.name == "std.io.Error").ok_or("型表に std.io.Error がない")?;
    let AggKind::Struct(fs) = &d.kind else { return Err("std.io.Error が構造体でない".into()) };
    let (Some(kf), Some(cf)) = (fs.iter().find(|f| f.name == "kind"), fs.iter().find(|f| f.name == "code")) else {
        return Err("std.io.Error に kind と code がない".into());
    };
    let Ty::Agg(ki) = kf.ty else { return Err("std.io.Error.kind が列挙型でない".into()) };
    let kd = &aggs[ki as usize];
    let AggKind::Enum(vs) = &kd.kind else { return Err("std.io.ErrorKind が列挙型でない".into()) };
    let vi = vs.iter().position(|v| v.name == kind).ok_or_else(|| format!("ErrorKind に {} がない", kind))?;
    let mut buf = vec![0u8; d.size as usize];
    buf[kf.off as usize..kf.off as usize + 4].copy_from_slice(&(vi as i32).to_le_bytes());
    buf[cf.off as usize..cf.off as usize + 4].copy_from_slice(&code.to_le_bytes());
    Ok(buf)
}

impl<'a> Vm<'a> {
    pub fn new(prog: &'a Prog) -> Vm<'a> {
        super::arena::ensure();
        // C の関数を呼ぶプログラムは、C が返す番地も読むので、領域の外も通す (FFI は `[unsafe]` の許可の内側)。
        // 標準ライブラリ (`std.` / `core.`) の extern は数えない (安全なプログラムで網を切らない)。
        super::arena::set_permissive(prog.ext.iter().any(|f| !(f.fqn.starts_with("std.") || f.fqn.starts_with("core."))));
        Vm { prog, regs: vec![0; 1 << 14], frames: vec![], result: vec![], allocs: vec![], out: Vec::new(), capture: None, args: vec![], socks: Default::default(), files: Default::default(), cmds: Default::default(), bad_cmds: Default::default(), children: Default::default(), next_fd: 3, mt: Default::default(), io: Default::default(), top: 0, panic_at: None, cbs: vec![], vers: vec![prog as *const Prog], owned: vec![], hook: None, dbg: None, out_hook: None, panic_base: 0, deadlines: Default::default() }
    }

    /// 最後の panic の位置 (`in <関数> at <ファイル>:<行>`。デバッガと同じ形)。行表がない関数では `None`。
    pub fn panic_location(&self) -> Option<String> {
        let (vi, fi, pc) = self.panic_at?;
        let prog: &Prog = unsafe { &*self.vers[vi as usize] };
        let f = &prog.funcs[fi as usize];
        let line = f.line_at(pc as usize);
        if line == 0 || f.file.is_empty() {
            return None;
        }
        Some(format!("in {} at {}:{}", f.key, crate::cli::rel(&f.file), line))
    }

    /// ためた stdout を流す。
    pub fn flush(&mut self) {
        use std::io::Write;
        if !self.out.is_empty() {
            if let Some(h) = &mut self.out_hook {
                h(1, &self.out);
                self.out.clear();
                return;
            }
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(&self.out);
            let _ = o.flush();
            self.out.clear();
        }
    }

    /// stderr へ書く (stdout を先に流す。`capture` があればためる)。
    pub fn err_write(&mut self, bytes: &[u8]) {
        if self.out_hook.is_some() {
            self.flush();
            if let Some(h) = &mut self.out_hook {
                h(2, bytes);
            }
            return;
        }
        if let Some(c) = &mut self.capture {
            c.extend_from_slice(bytes);
            return;
        }
        self.flush();
        let _ = kir_exec::write_fd(2, bytes);
    }

    fn out_write(&mut self, fd: i128, bytes: &[u8]) -> std::io::Result<()> {
        match fd {
            1 => {
                self.out.extend_from_slice(bytes);
                // ネイティブは 1 行ごとに書くので、同じく改行で終わる書き込みのたびに流す (SIGTERM で出力を失わない。S-11)。
                if self.out.len() >= 1 << 16 || bytes.last() == Some(&b'\n') {
                    self.flush();
                }
                Ok(())
            }
            2 => {
                self.err_write(bytes);
                Ok(())
            }
            _ => Err(std::io::Error::from_raw_os_error(9)),
        }
    }

    /// C から呼ばれた入口 (スロット `slot`) の処理: レジスタの中身を引数にして VM の関数を走らせ、戻り値をレジスタの中身にする。
    fn run_callback(
        &mut self,
        slot: usize,
        gp: &[u64; 6],
        sse: &[u64; 8],
        _stack: *const u64,
        out: &mut crate::ffi::RetRegs,
    ) {
        let (vi, ci) = self.cbs[slot];
        let prog: &Prog = unsafe { &*self.vers[vi as usize] };
        let cf = &prog.funcs[ci as usize];
        let (mut gi, mut si) = (0, 0);
        let mut args: Vec<u64> = vec![];
        for t in &cf.ptys {
            args.push(match t {
                Ty::F32 | Ty::F64 => {
                    si += 1;
                    sse[si - 1] & if *t == Ty::F32 { 0xffff_ffff } else { u64::MAX }
                }
                Ty::Ptr => {
                    gi += 1;
                    gp[gi - 1]
                }
                t => {
                    gi += 1;
                    t.wrap(gp[gi - 1] as i128) as u64
                }
            });
        }
        let ret = cf.ret;
        match self.exec(vi, ci, &args, true) {
            Ok(Outcome::Value(_)) => {
                let v = self.result.first().copied().unwrap_or(0);
                match ret {
                    Ty::F32 | Ty::F64 => out.x0 = v,
                    _ => out.rax = v,
                }
            }
            // コールバックは `panics never`。C のフレームを越えて巻き戻せない異常は、その場でプロセスを止める。
            Ok(Outcome::Panic(m)) => self.die(101, Some(format!("panic: {}", m))),
            Ok(Outcome::Abort) => self.die(101, None),
            Ok(Outcome::Exit(c)) => self.die(c, None),
            Ok(Outcome::StackOverflow) => self.die(102, Some("stack overflow".to_string())),
            Err(e) => self.die(3, Some(format!("ksn: vm: 内部エラー: {}", e))),
        }
    }

    fn die(&mut self, code: i32, msg: Option<String>) -> ! {
        self.flush();
        if let Some(m) = msg {
            eprintln!("{}", m);
        }
        std::process::exit(code)
    }

    /// スロット `s` から始まる fat (`{ptr, len}`) のバイト列。
    fn fat_bytes(rp: *mut u64, s: usize) -> Result<&'static [u8], String> {
        let (p, n) = unsafe { (*rp.add(s), *rp.add(s + 1) as usize) };
        if n == 0 {
            return Ok(&[]);
        }
        let ptr = addr(p, n)?;
        // SAFETY: KIR の検査済みの fat は、生きている領域の `len` バイトを指す。
        Ok(unsafe { std::slice::from_raw_parts(ptr as *const u8, n) })
    }

    /// `bytes` を複写した (解放しない) 領域を指す fat (`ptr`, `len`) を返す。
    fn leak_fat(bytes: &[u8]) -> (u64, u64) {
        (super::arena::leak(bytes) as u64, bytes.len() as u64)
    }

    fn new_file(&mut self, f: std::fs::File) -> i128 {
        let fd = self.next_fd;
        self.next_fd += 1;
        self.files.insert(fd, f);
        fd
    }

    fn new_fd(&mut self, sock: NetSock) -> i128 {
        let fd = self.next_fd;
        self.next_fd += 1;
        self.socks.insert(fd, sock);
        fd
    }

    /// 関数 `key` を引数 `args` (スロットの列) で実行する。同じ `Vm` で続けて呼べる。戻り値は `result`。
    pub fn run(&mut self, key: &str, args: &[u64]) -> Result<Outcome, String> {
        let vi = (self.vers.len() - 1) as u32;
        let prog: &Prog = unsafe { &*self.vers[vi as usize] };
        let Some(entry) = prog.find(key) else {
            return Err(format!("関数 {} がない", key));
        };
        let f0 = &prog.funcs[entry];
        if args.len() != f0.nparams as usize {
            return Err(format!("関数 {} の引数のスロット数が違う", key));
        }
        if f0.nslots == 0 && f0.code.is_empty() {
            return Err(format!("関数 {} は本体がない", key));
        }
        self.frames.clear();
        self.allocs.clear();
        self.top = 0;
        // 領域の外の番地を読もうとした (検査の抜けや偽の見出し) ときは、プロセスを壊さずに panic (101) で止める。
        match self.exec(vi, entry as u32, args, false) {
            Err(e) if (e.starts_with("不正なメモリアクセス") || e.starts_with("use after free")) => Ok(Outcome::Panic(e)),
            r => r,
        }
    }

    /// 版 `vi` の関数 `entry` を走らせる。`nested` は、C の関数 (`CallExt`) から呼び戻された (コールバック) とき。
    /// その場合は、呼び出し元のフレームの上 (`self.top`) にフレームを積み、呼び出し元のフレーム列・領域には触らない。
    fn exec(&mut self, vi: u32, entry: u32, args: &[u64], nested: bool) -> Result<Outcome, String> {
        let saved = if nested { Some(std::mem::take(&mut self.frames)) } else { None };
        let r = self.exec_inner(vi, entry, args, nested);
        if let Some(fr) = saved {
            self.frames = fr;
        }
        r
    }

    fn exec_inner(&mut self, mut vi: u32, entry: u32, args: &[u64], nested: bool) -> Result<Outcome, String> {
        let mut prog: &Prog = unsafe { &*self.vers[vi as usize] };
        let entry = entry as usize;
        let f0 = &prog.funcs[entry];
        let top_abase = self.allocs.len();
        let mut fi = entry as u32;
        let mut f = f0;
        let mut pc = 0usize;
        let mut base = self.top;
        if self.regs.len() < base + f.nslots as usize {
            self.regs.resize((base + f.nslots as usize) * 2, 0);
        }
        for (i, a) in args.iter().enumerate() {
            self.regs[base + i] = *a;
        }
        let mut rp: *mut u64 = unsafe { self.regs.as_mut_ptr().add(base) };
        macro_rules! r {
            ($s:expr) => {
                unsafe { *rp.add($s as usize) }
            };
        }
        macro_rules! w {
            ($s:expr, $v:expr) => {{
                let v: u64 = $v;
                unsafe { *rp.add($s as usize) = v }
            }};
        }
        macro_rules! rdv {
            ($t:expr, $s:expr) => {
                rd(rp, $t, $s as usize)
            };
        }
        macro_rules! wrv {
            ($t:expr, $s:expr, $v:expr) => {
                wr(rp, $t, $s as usize, $v)
            };
        }
        macro_rules! tyof {
            ($i:expr) => {
                prog.tys[*$i as usize]
            };
        }
        macro_rules! panic_out {
            ($m:expr) => {{
                self.panic_at = Some((vi, fi, pc as u32 - 1));
                self.panic_base = base;
                self.flush();
                return Ok(Outcome::Panic($m));
            }};
        }
        loop {
            // SAFETY: コンパイラは各関数の最後に `Ret` / `Jmp` / `Unreachable` を置くので `pc` は範囲内。
            let ins = unsafe { f.code.get_unchecked(pc) };
            pc += 1;
            if PROF {
                prof_count(ins);
            }
            match ins {
                Ins::ConstI { d, v } => w!(*d, *v as u64),
                Ins::ConstW { d, k } => w!(*d, prog.wide[*k as usize] as u64),
                Ins::Const128 { d, k } => wrv!(Ty::U128, *d, prog.wide[*k as usize]),
                Ins::Zero { d, n } => unsafe { std::ptr::write_bytes(rp.add(*d as usize), 0, *n as usize) },
                Ins::Mov { d, s } => w!(*d, r!(*s)),
                Ins::MovN { d, s, n } => unsafe { std::ptr::copy(rp.add(*s as usize), rp.add(*d as usize), *n as usize) },
                Ins::AddImm { d, a, imm } => w!(*d, r!(*a).wrapping_add(*imm as u64)),
                Ins::PtrAdd { d, p, off } => w!(*d, r!(*p).wrapping_add(r!(*off))),
                Ins::AddNI { d, a, t, imm } => {
                    let v = r!(*a) as i64 + *imm as i64;
                    if v < LO64[*t as usize] || v > HI64[*t as usize] {
                        panic_out!(kir_exec::overflow_msg(Aop::Add));
                    }
                    w!(*d, v as u64);
                }
                Ins::SubNI { d, a, t, imm } => {
                    let v = r!(*a) as i64 - *imm as i64;
                    if v < LO64[*t as usize] || v > HI64[*t as usize] {
                        panic_out!(kir_exec::overflow_msg(Aop::Sub));
                    }
                    w!(*d, v as u64);
                }
                Ins::MulNI { d, a, t, imm } => {
                    let v = (r!(*a) as i64).wrapping_mul(*imm as i64);
                    if v < LO64[*t as usize] || v > HI64[*t as usize] {
                        panic_out!(kir_exec::overflow_msg(Aop::Mul));
                    }
                    w!(*d, v as u64);
                }
                Ins::AddSI { d, a, imm } => match (r!(*a) as i64).checked_add(*imm as i64) {
                    Some(v) => w!(*d, v as u64),
                    None => panic_out!(kir_exec::overflow_msg(Aop::Add)),
                },
                Ins::SubSI { d, a, imm } => match (r!(*a) as i64).checked_sub(*imm as i64) {
                    Some(v) => w!(*d, v as u64),
                    None => panic_out!(kir_exec::overflow_msg(Aop::Sub)),
                },
                Ins::MulSI { d, a, imm } => match (r!(*a) as i64).checked_mul(*imm as i64) {
                    Some(v) => w!(*d, v as u64),
                    None => panic_out!(kir_exec::overflow_msg(Aop::Mul)),
                },
                Ins::AddUI { d, a, imm } => match r!(*a).checked_add(*imm as i64 as u64) {
                    Some(v) if *imm >= 0 => w!(*d, v),
                    _ => panic_out!(kir_exec::overflow_msg(Aop::Add)),
                },
                Ins::SubUI { d, a, imm } => match r!(*a).checked_sub(*imm as i64 as u64) {
                    Some(v) if *imm >= 0 => w!(*d, v),
                    _ => panic_out!(kir_exec::overflow_msg(Aop::Sub)),
                },
                Ins::MulUI { d, a, imm } => match r!(*a).checked_mul(*imm as i64 as u64) {
                    Some(v) if *imm >= 0 => w!(*d, v),
                    _ => panic_out!(kir_exec::overflow_msg(Aop::Mul)),
                },
                Ins::AddW64I { d, a, imm } => w!(*d, r!(*a).wrapping_add(*imm as i64 as u64)),
                Ins::SubW64I { d, a, imm } => w!(*d, r!(*a).wrapping_sub(*imm as i64 as u64)),
                Ins::MulW64I { d, a, imm } => w!(*d, r!(*a).wrapping_mul(*imm as i64 as u64)),
                Ins::AddWI { d, a, t, imm } => w!(*d, wrap_fast(*t, r!(*a).wrapping_add(*imm as i64 as u64))),
                Ins::SubWI { d, a, t, imm } => w!(*d, wrap_fast(*t, r!(*a).wrapping_sub(*imm as i64 as u64))),
                Ins::MulWI { d, a, t, imm } => w!(*d, wrap_fast(*t, r!(*a).wrapping_mul(*imm as i64 as u64))),
                Ins::CmpII { d, a, cx, imm } => w!(*d, cx_eval(*cx, r!(*a), *imm as i64 as u64) as u64),
                Ins::JCmpNotI { cx, a, imm, to } => {
                    if !cx_eval(*cx, r!(*a), *imm as i64 as u64) {
                        pc = *to as usize;
                    }
                }
                Ins::JCmp { cx, a, b, to } => {
                    if cx_eval(*cx, r!(*a), r!(*b)) {
                        pc = *to as usize;
                    }
                }
                Ins::JCmpI { cx, a, imm, to } => {
                    if cx_eval(*cx, r!(*a), *imm as i64 as u64) {
                        pc = *to as usize;
                    }
                }
                Ins::Jmp { to } => pc = *to as usize,
                Ins::JmpIf { c, to } => {
                    if r!(*c) != 0 {
                        pc = *to as usize;
                    }
                }
                Ins::JmpIfNot { c, to } => {
                    if r!(*c) == 0 {
                        pc = *to as usize;
                    }
                }
                Ins::JCmpNot { cx, a, b, to } => {
                    if !cx_eval(*cx, r!(*a), r!(*b)) {
                        pc = *to as usize;
                    }
                }
                Ins::AddN { d, a, b, t } => {
                    let v = r!(*a) as i64 + r!(*b) as i64;
                    if v < LO64[*t as usize] || v > HI64[*t as usize] {
                        panic_out!(kir_exec::overflow_msg(Aop::Add));
                    }
                    w!(*d, v as u64);
                }
                Ins::SubN { d, a, b, t } => {
                    let v = r!(*a) as i64 - r!(*b) as i64;
                    if v < LO64[*t as usize] || v > HI64[*t as usize] {
                        panic_out!(kir_exec::overflow_msg(Aop::Sub));
                    }
                    w!(*d, v as u64);
                }
                Ins::MulN { d, a, b, t } => {
                    let v = (r!(*a) as i64).wrapping_mul(r!(*b) as i64);
                    if v < LO64[*t as usize] || v > HI64[*t as usize] {
                        panic_out!(kir_exec::overflow_msg(Aop::Mul));
                    }
                    w!(*d, v as u64);
                }
                Ins::AddS { d, a, b } => match (r!(*a) as i64).checked_add(r!(*b) as i64) {
                    Some(v) => w!(*d, v as u64),
                    None => panic_out!(kir_exec::overflow_msg(Aop::Add)),
                },
                Ins::SubS { d, a, b } => match (r!(*a) as i64).checked_sub(r!(*b) as i64) {
                    Some(v) => w!(*d, v as u64),
                    None => panic_out!(kir_exec::overflow_msg(Aop::Sub)),
                },
                Ins::MulS { d, a, b } => match (r!(*a) as i64).checked_mul(r!(*b) as i64) {
                    Some(v) => w!(*d, v as u64),
                    None => panic_out!(kir_exec::overflow_msg(Aop::Mul)),
                },
                Ins::AddU { d, a, b } => match r!(*a).checked_add(r!(*b)) {
                    Some(v) => w!(*d, v),
                    None => panic_out!(kir_exec::overflow_msg(Aop::Add)),
                },
                Ins::SubU { d, a, b } => match r!(*a).checked_sub(r!(*b)) {
                    Some(v) => w!(*d, v),
                    None => panic_out!(kir_exec::overflow_msg(Aop::Sub)),
                },
                Ins::MulU { d, a, b } => match r!(*a).checked_mul(r!(*b)) {
                    Some(v) => w!(*d, v),
                    None => panic_out!(kir_exec::overflow_msg(Aop::Mul)),
                },
                Ins::AddW64 { d, a, b } => w!(*d, r!(*a).wrapping_add(r!(*b))),
                Ins::SubW64 { d, a, b } => w!(*d, r!(*a).wrapping_sub(r!(*b))),
                Ins::MulW64 { d, a, b } => w!(*d, r!(*a).wrapping_mul(r!(*b))),
                Ins::AddW { d, a, b, t } => w!(*d, wrap_fast(*t, r!(*a).wrapping_add(r!(*b)))),
                Ins::SubW { d, a, b, t } => w!(*d, wrap_fast(*t, r!(*a).wrapping_sub(r!(*b)))),
                Ins::MulW { d, a, b, t } => w!(*d, wrap_fast(*t, r!(*a).wrapping_mul(r!(*b)))),
                Ins::WrapT { d, a, t } => w!(*d, wrap_fast(*t, r!(*a))),
                Ins::Arith { d, a, b, op, mode, ty } => {
                    let t = tyof!(ty);
                    match kir_exec::arith(*op, *mode, t, rdv!(t, *a), rdv!(t, *b)) {
                        Ok(v) => wrv!(t, *d, v),
                        Err(m) => panic_out!(m),
                    }
                }
                Ins::CmpI { d, a, b, cx } => w!(*d, cx_eval(*cx, r!(*a), r!(*b)) as u64),
                Ins::Cmp { d, a, b, cc, ty } => {
                    let t = tyof!(ty);
                    w!(*d, kir_scalar::cmp(*cc, t, rdv!(t, *a), rdv!(t, *b)) as u64)
                }
                Ins::Neg { d, a, mode, ty } => {
                    let t = tyof!(ty);
                    let x = rdv!(t, *a);
                    match mode {
                        Mode::Wrap => wrv!(t, *d, kir_scalar::wrap_neg(t, x)),
                        _ => {
                            if x == t.min() {
                                panic_out!("attempt to negate with overflow".to_string());
                            }
                            wrv!(t, *d, -x);
                        }
                    }
                }
                Ins::FArith { d, a, b, op, ty } => {
                    let t = tyof!(ty);
                    wrv!(t, *d, kir_scalar::farith(*op, t, rdv!(t, *a), rdv!(t, *b)))
                }
                Ins::Shift { d, a, n, left, ty } => {
                    let t = tyof!(ty);
                    wrv!(t, *d, kir_scalar::shift(*left, t, rdv!(t, *a), rdv!(Ty::U32, *n)))
                }
                Ins::MinMax { d, a, b, max, ty } => {
                    let t = tyof!(ty);
                    wrv!(t, *d, kir_scalar::minmax(*max, t, rdv!(t, *a), rdv!(t, *b)))
                }
                Ins::FUn { d, a, f: fu, src, res } => {
                    let (st, rt) = (tyof!(src), tyof!(res));
                    wrv!(rt, *d, kir_scalar::funary(*fu, st, rt, rdv!(st, *a)))
                }
                Ins::FBin { d, a, b, f: fb, res } => {
                    let rt = tyof!(res);
                    let bt = if *fb == crate::kir::FBin::Powi { Ty::I32 } else { rt };
                    wrv!(rt, *d, kir_scalar::fbin(*fb, rt, rdv!(rt, *a), rdv!(bt, *b)))
                }
                Ins::Conv { d, a, src, dst } => {
                    let (st, dt) = (tyof!(src), tyof!(dst));
                    wrv!(dt, *d, kir_scalar::conv(st, dt, rdv!(st, *a)))
                }
                Ins::CanonF { a, bits } => {
                    let x = r!(*a);
                    let c = crate::kir_scalar::canon_bits(if *bits == 32 { Ty::F32 } else { Ty::F64 }, x as i128) as u64;
                    w!(*a, c)
                }
                Ins::NotB { d, a } => w!(*d, (r!(*a) == 0) as u64),
                Ins::NotI { d, a, ty } => {
                    let t = tyof!(ty);
                    wrv!(t, *d, kir_scalar::not_int(t, rdv!(t, *a)))
                }
                Ins::BitOp { d, a, b, k } => {
                    let (x, y) = (r!(*a), r!(*b));
                    w!(*d, match k {
                        0 => x & y,
                        1 => x | y,
                        _ => x ^ y,
                    });
                }
                Ins::BitOp128 { d, a, b, k } => {
                    let (x, y) = (rdv!(Ty::U128, *a), rdv!(Ty::U128, *b));
                    wrv!(Ty::U128, *d, match k {
                        0 => x & y,
                        1 => x | y,
                        _ => x ^ y,
                    });
                }
                Ins::BitCount { d, a, k, src } => {
                    let t = tyof!(src);
                    w!(*d, kir_scalar::bitcount(*k, t, rdv!(t, *a)) as u64)
                }
                Ins::Checked { d, a, b, op, ty } => {
                    let ot = tyof!(ty);
                    let Ty::Opt(p) = ot else { return Err("checked の結果は Option のはず".to_string()) };
                    let pt = p.ty();
                    let old = kir_exec::checked(*op, ot, rdv!(pt, *a), rdv!(pt, *b));
                    if old & TAG_BIT != 0 {
                        w!(*d, 1);
                        w!(*d + 1, payload_of(pt, old) as u64);
                    } else {
                        w!(*d, 0);
                        w!(*d + 1, 0);
                    }
                }
                Ins::FnAddr { d, f: ci } => w!(*d, *ci as u64 + 1),
                Ins::Call { f: ci, d, argp, argn } | Ins::CallInd { f: ci, d, argp, argn } => {
                    if self.frames.len() + 1 >= crate::kir_exec::depth_limit(self.prog.depth) {
                        self.flush();
                        return Ok(Outcome::StackOverflow);
                    }
                    let ci = &(if matches!(ins, Ins::CallInd { .. }) {
                        let fv = r!(*ci);
                        if fv == 0 || fv as usize > prog.funcs.len() {
                            return Err(format!("間接呼び出しの関数値 {} が不正", fv));
                        }
                        (fv - 1) as u32
                    } else {
                        *ci
                    });
                    // 安全点 (関数の入口): `--watch` なら新しい版を取り込み、呼び先を最新の版へ解決する。
                    let (cvi, ci) = if self.hook.is_some() { self.resolve_call(vi, *ci) } else { (vi, *ci) };
                    let cprog: &Prog = if cvi == vi { prog } else { unsafe { &*self.vers[cvi as usize] } };
                    let cf = &cprog.funcs[ci as usize];
                    let nb = base + f.nslots as usize;
                    if self.regs.len() < nb + cf.nslots as usize {
                        let n = (nb + cf.nslots as usize) * 2;
                        self.regs.resize(n, 0);
                        rp = unsafe { self.regs.as_mut_ptr().add(base) };
                    }
                    // 引数を新しいフレームの先頭へ写す。
                    let mut at = f.nslots as usize;
                    for k in 0..*argn as usize {
                        let e = unsafe { *f.pool.get_unchecked(*argp as usize + k) };
                        let (s, n) = ((e & 0xffff) as usize, (e >> 16) as usize);
                        if n == 1 {
                            unsafe { *rp.add(at) = *rp.add(s) };
                        } else {
                            unsafe { std::ptr::copy_nonoverlapping(rp.add(s), rp.add(at), n) };
                        }
                        at += n;
                    }
                    self.frames.push(Frame { v: vi, f: fi, pc: pc as u32, base: base as u32, dst: *d, abase: self.allocs.len() });
                    fi = ci;
                    vi = cvi;
                    prog = cprog;
                    f = cf;
                    pc = 0;
                    base = nb;
                    rp = unsafe { self.regs.as_mut_ptr().add(base) };
                    continue;
                }
                Ins::CallExt { e, d, argp, argn } => {
                    let func = &prog.ext[*e as usize];
                    let ps = func.params();
                    let argv: Vec<i128> = (0..*argn as usize)
                        .map(|k| {
                            let ent = f.pool[*argp as usize + k];
                            rdv!(ps[k].1, ent & 0xffff)
                        })
                        .collect();
                    // C から呼び戻された (コールバック) とき、この `Vm` の上にフレームを積む。
                    self.top = base + f.nslots as usize;
                    let prev = CUR_VM.with(|c| c.replace(self as *mut Vm<'a> as *mut Vm<'static>));
                    let v = kir_exec::extern_call(func, &argv);
                    CUR_VM.with(|c| c.set(prev));
                    // コールバックが `regs` を伸ばしたかもしれない。
                    rp = unsafe { self.regs.as_mut_ptr().add(base) };
                    // 外部の関数の解決・呼び出しの失敗は木走査と同じく panic (終了コード 101)。
                    match v {
                        Ok(x) => w!(*d, x as u64),
                        Err(m) => return Ok(Outcome::Panic(m)),
                    }
                }
                Ins::Ret { v } => {
                    let abase = match self.frames.last() {
                        Some(fr) => fr.abase,
                        None => top_abase,
                    };
                    if self.allocs.len() > abase {
                        for p in self.allocs.drain(abase..) {
                            super::arena::free(p as usize);
                        }
                    }
                    let n = f.ret_w as usize;
                    let src = unsafe { rp.add(*v as usize) };
                    match self.frames.pop() {
                        None => {
                            self.result.clear();
                            self.result.extend_from_slice(unsafe { std::slice::from_raw_parts(src, n) });
                            if !nested {
                                self.flush();
                                prof_report();
                            }
                            return Ok(Outcome::Value(0));
                        }
                        Some(fr) => {
                            fi = fr.f;
                            if fr.v != vi {
                                vi = fr.v;
                                prog = unsafe { &*self.vers[vi as usize] };
                            }
                            f = &prog.funcs[fi as usize];
                            pc = fr.pc as usize;
                            base = fr.base as usize;
                            rp = unsafe { self.regs.as_mut_ptr().add(base) };
                            if fr.dst != NO_SLOT {
                                if n == 1 {
                                    unsafe { *rp.add(fr.dst as usize) = *src };
                                } else {
                                    unsafe { std::ptr::copy_nonoverlapping(src, rp.add(fr.dst as usize), n) };
                                }
                            }
                        }
                    }
                }
                Ins::FnPtr { d, f: ci } => {
                    let key = (vi, *ci);
                    let cf = &prog.funcs[*ci as usize];
                    // 構造体の値渡し・値返しは、レジスタをまたぐため VM (レジスタ 1 個 = 1 引数) では扱えない。
                    if cf.ptys.iter().any(|t| matches!(t, Ty::Agg(_))) || matches!(cf.ret, Ty::Agg(_)) {
                        return Err("コールバックで構造体を値渡し・値返しするには木走査エンジンを使う (`ksn run`、または `--engine=tree`)".to_string());
                    }
                    let rabi = match cf.ret {
                        Ty::F32 | Ty::F64 => crate::ffi::RetAbi::F64,
                        Ty::Unit => crate::ffi::RetAbi::Void,
                        _ => crate::ffi::RetAbi::Int,
                    };
                    let slot = match self.cbs.iter().position(|c| *c == key) {
                        Some(i) => i,
                        None => {
                            let ng = cf.ptys.iter().filter(|t| !matches!(t, Ty::F32 | Ty::F64)).count();
                            let nf = cf.ptys.len() - ng;
                            if ng > 6 || nf > 8 {
                                return Err("コールバックの引数は整数・ポインタ 6 個、浮動小数 8 個まで".to_string());
                            }
                            if self.cbs.len() >= crate::ffi::CALLBACK_SLOTS - crate::ffi::VM_SLOT_BASE {
                                return Err(format!("C に渡す関数が多すぎる (VM では {} 個まで)", crate::ffi::CALLBACK_SLOTS - crate::ffi::VM_SLOT_BASE));
                            }
                            self.cbs.push(key);
                            self.cbs.len() - 1
                        }
                    };
                    crate::ffi::set_vm_callback_handler(vm_cb_handler);
                    let Some(a) = crate::ffi::trampoline(crate::ffi::VM_SLOT_BASE + slot, rabi) else {
                        return Err("コールバック (C に渡す関数) は x86_64 Linux だけで使える".to_string());
                    };
                    w!(*d, a as u64);
                }
                Ins::Line { line, vid } => {
                    if self.dbg.is_some() {
                        if let Some(o) = self.dbg_line(vi, fi, pc - 1, base, *line, *vid) {
                            return Ok(o);
                        }
                        // 停止中の式の評価が `regs` を伸ばしたかもしれない。
                        rp = unsafe { self.regs.as_mut_ptr().add(base) };
                    }
                }
                Ins::Unreachable => return Err("到達しないブロックに入った".to_string()),
                Ins::Make { d, info, argp } => {
                    let mi = &prog.makes[*info as usize];
                    unsafe {
                        let dp = rp.add(*d as usize) as *mut u8;
                        std::ptr::write_bytes(dp, 0, mi.nslots as usize * 8);
                        if let Some(k) = mi.variant {
                            (dp as *mut i32).write_unaligned(k as i32);
                        }
                        for (k, (off, nb)) in mi.fields.iter().enumerate() {
                            let e = *f.pool.get_unchecked(*argp as usize + k);
                            let sp = rp.add((e & 0xffff) as usize) as *const u8;
                            match mi.packs.get(k) {
                                Some(&(sb, pw)) if sb > 0 => pack_opt(dp.add(*off as usize), sp as *const u64, sb, pw),
                                _ => std::ptr::copy(sp, dp.add(*off as usize), *nb as usize),
                            }
                        }
                    }
                }
                Ins::LoadOpt { d, p, off, sb, pw } => unsafe {
                    let src = addr(r!(*p) + *off as u64, *sb as usize + 1)? as *const u8;
                    unpack_opt(rp.add(*d as usize), src, *sb, *pw);
                },
                Ins::StoreOpt { p, off, v, sb, pw } => unsafe {
                    let dst = addr(r!(*p) + *off as u64, *sb as usize + 1)?;
                    pack_opt(dst, rp.add(*v as usize) as *const u64, *sb, *pw);
                },
                Ins::FieldOpt { d, a, off, sb, pw } => unsafe {
                    let src = (rp.add(*a as usize) as *const u8).add(*off as usize);
                    unpack_opt(rp.add(*d as usize), src, *sb, *pw);
                },
                Ins::SetFieldOpt { d, a, v, off, ns, sb, pw } => unsafe {
                    std::ptr::copy(rp.add(*a as usize), rp.add(*d as usize), *ns as usize);
                    pack_opt((rp.add(*d as usize) as *mut u8).add(*off as usize), rp.add(*v as usize) as *const u64, *sb, *pw);
                },
                Ins::FieldS { d, a, off, k } => unsafe {
                    let p = (rp.add(*a as usize) as *const u8).add(*off as usize);
                    if *k == LK::I128 {
                        let (lo, hi) = ((p as *const u64).read_unaligned(), (p.add(8) as *const u64).read_unaligned());
                        w!(*d, lo);
                        w!(*d + 1, hi);
                    } else {
                        w!(*d, ld_k(p, *k));
                    }
                },
                Ins::FieldA { d, a, off, nb } => unsafe {
                    let ns = ((*nb as usize + 7) / 8).max(1);
                    let sp = (rp.add(*a as usize) as *const u8).add(*off as usize);
                    // 先に読み出し元を読む (`d` と重なってもよいように `copy`)。
                    *rp.add(*d as usize + ns - 1) = 0;
                    std::ptr::copy(sp, rp.add(*d as usize) as *mut u8, *nb as usize);
                },
                Ins::SetField { d, a, v, off, ns, nb } => unsafe {
                    std::ptr::copy(rp.add(*a as usize), rp.add(*d as usize), *ns as usize);
                    std::ptr::copy(rp.add(*v as usize) as *const u8, (rp.add(*d as usize) as *mut u8).add(*off as usize), *nb as usize);
                },
                Ins::TagEnum { d, a } => {
                    let t = unsafe { (rp.add(*a as usize) as *const i32).read() };
                    w!(*d, t as i64 as u64);
                }
                Ins::TagOpt { d, a } => w!(*d, (r!(*a) != 0) as u64),
                Ins::WrapN { d, a, n, tag } => unsafe {
                    if *n == 1 {
                        let x = r!(*a);
                        w!(*d, *tag as u64);
                        w!(*d + 1, x);
                    } else {
                        std::ptr::copy(rp.add(*a as usize), rp.add(*d as usize + 1), *n as usize);
                        w!(*d, *tag as u64);
                    }
                },
                Ins::Payload1 { d, a } => w!(*d, r!(*a + 1)),
                Ins::PayloadN { d, a, n } => unsafe {
                    std::ptr::copy(rp.add(*a as usize + 1), rp.add(*d as usize), *n as usize);
                },
                Ins::FrameBytes { d, size } => {
                    let p = super::arena::alloc_zeroed(*size as usize) as *mut std::ffi::c_void;
                    self.allocs.push(p);
                    w!(*d, p as usize as u64);
                }
                Ins::Load { d, p, off, k } => {
                    let a = addr(r!(*p).wrapping_add(*off as u64), 1)?;
                    // SAFETY: KIR の検査済みの参照・ptr は、生きている領域の十分な幅を指す。
                    unsafe {
                        if *k == LK::I128 {
                            w!(*d, (a as *const u64).read_unaligned());
                            w!(*d + 1, (a.add(8) as *const u64).read_unaligned());
                        } else {
                            w!(*d, ld_k(a, *k));
                        }
                    }
                }
                Ins::LoadU64 { d, p, off } => {
                    let a = addr(r!(*p).wrapping_add(*off as u64), 8)?;
                    w!(*d, unsafe { (a as *const u64).read_unaligned() });
                }
                Ins::LoadI64 { d, p, off } => {
                    let a = addr(r!(*p).wrapping_add(*off as u64), 8)?;
                    w!(*d, unsafe { (a as *const u64).read_unaligned() });
                }
                Ins::LoadU32 { d, p, off } => {
                    let a = addr(r!(*p).wrapping_add(*off as u64), 4)?;
                    w!(*d, unsafe { (a as *const u32).read_unaligned() } as u64);
                }
                Ins::LoadI32 { d, p, off } => {
                    let a = addr(r!(*p).wrapping_add(*off as u64), 4)?;
                    w!(*d, unsafe { (a as *const i32).read_unaligned() } as i64 as u64);
                }
                Ins::LoadU8 { d, p, off } => {
                    let a = addr(r!(*p).wrapping_add(*off as u64), 1)?;
                    w!(*d, unsafe { *a } as u64);
                }
                Ins::Store64 { p, off, v } => {
                    let a = addr(r!(*p).wrapping_add(*off as u64), 8)?;
                    let xv = r!(*v);
                    unsafe { (a as *mut u64).write_unaligned(xv) };
                }
                Ins::Store8 { p, off, v } => {
                    let a = addr(r!(*p).wrapping_add(*off as u64), 1)?;
                    let xv = r!(*v);
                    unsafe { *a = xv as u8 };
                }
                Ins::LoadX { d, p, x, off, k } => {
                    let a = addr(r!(*p).wrapping_add(r!(*x)).wrapping_add(*off as u64), 1)?;
                    unsafe {
                        if *k == LK::I128 {
                            w!(*d, (a as *const u64).read_unaligned());
                            w!(*d + 1, (a.add(8) as *const u64).read_unaligned());
                        } else {
                            w!(*d, ld_k(a, *k));
                        }
                    }
                }
                Ins::StoreX { p, x, off, v, w: wd } => {
                    let a = addr(r!(*p).wrapping_add(r!(*x)).wrapping_add(*off as u64), *wd as usize)?;
                    let xv = r!(*v);
                    unsafe {
                        match wd {
                            1 => *a = xv as u8,
                            2 => (a as *mut u16).write_unaligned(xv as u16),
                            4 => (a as *mut u32).write_unaligned(xv as u32),
                            8 => (a as *mut u64).write_unaligned(xv),
                            _ => {
                                (a as *mut u64).write_unaligned(xv);
                                (a.add(8) as *mut u64).write_unaligned(r!(*v + 1));
                            }
                        }
                    }
                }
                Ins::LoadAgg { d, p, size } => {
                    let n = *size as usize;
                    let a = addr(r!(*p), n)?;
                    unsafe {
                        *rp.add(*d as usize + ((n + 7) / 8).max(1) - 1) = 0;
                        std::ptr::copy(a as *const u8, rp.add(*d as usize) as *mut u8, n);
                    }
                }
                Ins::Store { p, off, v, w: wd } => {
                    let a = addr(r!(*p).wrapping_add(*off as u64), *wd as usize)?;
                    let x = r!(*v);
                    unsafe {
                        match wd {
                            1 => *a = x as u8,
                            2 => (a as *mut u16).write_unaligned(x as u16),
                            4 => (a as *mut u32).write_unaligned(x as u32),
                            8 => (a as *mut u64).write_unaligned(x),
                            _ => {
                                (a as *mut u64).write_unaligned(x);
                                (a.add(8) as *mut u64).write_unaligned(r!(*v + 1));
                            }
                        }
                    }
                }
                Ins::StoreAgg { p, v, size } => {
                    let n = *size as usize;
                    let a = addr(r!(*p), n)?;
                    unsafe { std::ptr::copy(rp.add(*v as usize) as *const u8, a, n) };
                }
                Ins::HeapAlloc { d, n } => {
                    let nn = (r!(*n) as usize).max(1);
                    w!(*d, super::arena::alloc(nn) as u64);
                }
                Ins::HeapRealloc { d, p, n } => {
                    let old = r!(*p);
                    let nn = (r!(*n) as usize).max(1);
                    match super::arena::realloc(old as usize, nn) {
                        Ok(q) => w!(*d, q as u64),
                        Err(()) => return Err(bad_addr(old, nn)),
                    }
                }
                Ins::HeapFree { p } => {
                    let a = r!(*p);
                    if a != 0 {
                        if !super::arena::free(a as usize) {
                            return Err(bad_addr(a, 0));
                        }
                    }
                }
                Ins::IndexCheck { i, len } => {
                    let (ix, n) = (r!(*i), r!(*len));
                    if ix >= n {
                        panic_out!(format!("index out of bounds: the len is {} but the index is {}", n, ix));
                    }
                }
                Ins::Utf8Valid { d, p, len } => {
                    let n = r!(*len) as usize;
                    let pa = r!(*p);
                    let bytes: &[u8] = if n == 0 { &[] } else { unsafe { std::slice::from_raw_parts(addr(pa, n)? as *const u8, n) } };
                    w!(*d, match std::str::from_utf8(bytes) {
                        Ok(_) => n as u64,
                        Err(e) => e.valid_up_to() as u64,
                    });
                }
                Ins::Panic { msg, arg, aty } => {
                    let m = &prog.strs[*msg as usize];
                    let m = if *arg == NO_SLOT { m.clone() } else { format!("{}: {}", m, rdv!(tyof!(aty), *arg)) };
                    panic_out!(m);
                }
                Ins::Abort => {
                    self.panic_at = Some((vi, fi, pc as u32 - 1));
                    self.panic_base = base;
                    self.flush();
                    return Ok(Outcome::Abort);
                }
                Ins::Exit { code } => {
                    let c = r!(*code) as i32;
                    self.flush();
                    return Ok(Outcome::Exit(c));
                }
                Ins::Write { d, fd, data, nl } => {
                    let bytes = Self::fat_bytes(rp, *data as usize)?;
                    let joined;
                    let bytes = if *nl {
                        joined = [bytes, b"\n"].concat();
                        &joined[..]
                    } else {
                        bytes
                    };
                    match self.out_write(r!(*fd) as i32 as i128, bytes) {
                        Ok(()) => w!(*d, 0),
                        Err(e) => {
                            let b = io_error_bytes(&prog.aggs, crate::natives::io_kind_name(&e), e.raw_os_error().unwrap_or(0))?;
                            w!(*d, 1);
                            put_bytes(rp, *d as usize + 1, &b);
                        }
                    }
                }
                Ins::Eprint { data } => {
                    let bytes = Self::fat_bytes(rp, *data as usize)?;
                    self.err_write(bytes);
                }
                Ins::FloatText { d, a, dst, bits } => {
                    let x = r!(*a);
                    let s = if *bits == 32 { py_float32(f32::from_bits(x as u32)) } else { py_float(f64::from_bits(x)) };
                    let pa = r!(*dst);
                    unsafe { std::ptr::copy_nonoverlapping(s.as_ptr(), addr(pa, s.len())? as *mut u8, s.len()) };
                    w!(*d, s.len() as u64);
                }
                Ins::FmtFloat { a, bits } => {
                    let x = r!(*a);
                    let s = if *bits == 32 { py_float32(f32::from_bits(x as u32)) } else { py_float(f64::from_bits(x)) };
                    self.err_write(s.as_bytes());
                }
                Ins::EnvArgCount { d } => {
                    w!(*d, self.args.len() as u64);
                }
                Ins::Rt { d, op, a, b } => self.rt(rp, *op, *d, *a, *b)?,
                Ins::TimeNowNs { d } => {
                    // 実時刻 (UNIX 時刻からのナノ秒)。`i64` の bit 列で書く。失敗したら 0。
                    let ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|x| x.as_nanos() as i64).unwrap_or(0);
                    w!(*d, ns as u64);
                }
                Ins::EnvArg { d, i } => {
                    let i = r!(*i) as usize;
                    if i >= self.args.len() {
                        w!(*d, 0);
                    } else {
                        let (p, n) = Self::leak_fat(self.args[i].as_bytes());
                        w!(*d, 1);
                        w!(*d + 1, p);
                        w!(*d + 2, n);
                    }
                }
                Ins::EnvVar { d, name } => {
                    let bytes = Self::fat_bytes(rp, *name as usize)?;
                    match std::env::var(std::str::from_utf8(bytes).unwrap_or("")) {
                        Ok(v) => {
                            let (p, n) = Self::leak_fat(v.as_bytes());
                            w!(*d, 1);
                            w!(*d + 1, p);
                            w!(*d + 2, n);
                        }
                        Err(_) => w!(*d, 0),
                    }
                }
                Ins::NetListen { d, addr } | Ins::NetConnect { d, addr } => {
                    let bytes = Self::fat_bytes(rp, *addr as usize)?;
                    let a = std::str::from_utf8(bytes).unwrap_or("");
                    let is_listen = matches!(ins, Ins::NetListen { .. });
                    let res = if is_listen {
                        std::net::TcpListener::bind(a).map(|l| {
                            // 待ち行列を大きくする (ネイティブの `listen(s, 1024)` と同じ。多数の接続が先に来る非同期サーバのため)。
                            unsafe { libc::listen(std::os::fd::AsRawFd::as_raw_fd(&l), 1024) };
                            NetSock::Listener(l)
                        })
                    } else { std::net::TcpStream::connect(a).map(NetSock::Stream) };
                    match res {
                        Ok(sock) => {
                            let fd = self.new_fd(sock);
                            w!(*d, 0);
                            w!(*d + 1, fd as i32 as u32 as u64);
                        }
                        Err(e) => {
                            // listen / connect / accept の失敗は `std.net.Error` (`{ code: i32 }`。kir_exec と同じ。kir/net-refs)。
                            w!(*d, 1);
                            w!(*d + 1, e.raw_os_error().unwrap_or(-1) as u32 as u64);
                        }
                    }
                }
                Ins::NetAccept { d, sock } => {
                    let fd = r!(*sock) as i32 as i128;
                    let Some(NetSock::Listener(l)) = self.socks.get_mut(&fd) else { return Err(format!("無効なソケット記述子 {}", fd)) };
                    match l.accept() {
                        Ok((st, _)) => {
                            let nfd = self.new_fd(NetSock::Stream(st));
                            w!(*d, 0);
                            w!(*d + 1, nfd as i32 as u32 as u64);
                        }
                        Err(e) => {
                            w!(*d, 1);
                            w!(*d + 1, e.raw_os_error().unwrap_or(-1) as u32 as u64);
                        }
                    }
                }
                Ins::NetRead { d, sock, buf, len } => {
                    use std::io::Read;
                    let (fd, p, n) = (r!(*sock) as i32 as i128, r!(*buf), r!(*len) as usize);
                    let at = self.deadlines.get(&fd).and_then(|x| x.0);
                    let Some(NetSock::Stream(st)) = self.socks.get_mut(&fd) else { return Err(format!("読み出しできるのはストリームソケットだけ: {}", fd)) };
                    let mut tmp = vec![0u8; n];
                    match kir_exec::dl_call(st, at, false, |s| s.read(&mut tmp)) {
                        Ok(nr) => {
                            if nr > 0 {
                                unsafe { std::ptr::copy_nonoverlapping(tmp.as_ptr(), addr(p, nr)?, nr) };
                            }
                            w!(*d, 0);
                            w!(*d + 1, nr as u64);
                        }
                        Err(e) => fs_err(rp, &prog.aggs, *d, &e)?,
                    }
                }
                Ins::NetWrite { d, sock, buf, len } => {
                    use std::io::Write;
                    let (fd, p, n) = (r!(*sock) as i32 as i128, r!(*buf), r!(*len) as usize);
                    let at = self.deadlines.get(&fd).and_then(|x| x.1);
                    let Some(NetSock::Stream(st)) = self.socks.get_mut(&fd) else { return Err(format!("書き込みできるのはストリームソケットだけ: {}", fd)) };
                    let bytes: &[u8] = if n == 0 { &[] } else { unsafe { std::slice::from_raw_parts(addr(p, n)? as *const u8, n) } };
                    match kir_exec::dl_call(st, at, true, |s| s.write_all(bytes)) {
                        Ok(()) => {
                            w!(*d, 0);
                            w!(*d + 1, n as u64);
                        }
                        Err(e) => fs_err(rp, &prog.aggs, *d, &e)?,
                    }
                }
                Ins::NetSys { d, kind, argp, argn } => {
                    let args: Vec<(usize, usize)> = (0..*argn as usize)
                        .map(|k| {
                            let e = unsafe { *f.pool.get_unchecked(*argp as usize + k) };
                            ((e & 0xffff) as usize, (e >> 16) as usize)
                        })
                        .collect();
                    self.net_sys(rp, &prog.aggs, *d, *kind, &args)?;
                }
                Ins::NetFlush { d, sock } => {
                    use std::io::Write;
                    let fd = r!(*sock) as i32 as i128;
                    let Some(NetSock::Stream(st)) = self.socks.get_mut(&fd) else { return Err(format!("flush できるのはストリームソケットだけ: {}", fd)) };
                    st.flush().map_err(|e| format!("flush: {e}"))?;
                    w!(*d, 0);
                }
                Ins::NetClose { d, sock } => {
                    let fd = r!(*sock) as i32 as i128;
                    self.io.forget(fd as i32);
                    self.socks.remove(&fd);
                    w!(*d, 0);
                }
                Ins::Proc { d, f, a } => {
                    self.proc_op(rp, &prog.aggs, *d, *f, *a)?;
                }
                Ins::RandFill { d, buf, len, fatal } => {
                    let (p, n) = (r!(*buf), r!(*len) as usize);
                    let mut bytes = vec![0u8; n];
                    match (crate::natives::os_random(&mut bytes), *fatal) {
                        (Ok(()), _) => {
                            if n > 0 {
                                let dst = addr(p, n)?;
                                unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, n) };
                            }
                            w!(*d, 0);
                        }
                        // 致命的な形 (`Rng.from_entropy`): 文言を出して終わる (kir_exec と同じ)。
                        (Err(_), true) => {
                            self.flush();
                            eprint!("{}", crate::natives::ENTROPY_FAILED);
                            return Ok(Outcome::Abort);
                        }
                        (Err(e), false) => fs_err(rp, &prog.aggs, *d, &e)?,
                    }
                }
                Ins::FsOpen { d, path, create } => {
                    let s = std::str::from_utf8(Self::fat_bytes(rp, *path as usize)?).unwrap_or("").to_string();
                    let opened = if *create {
                        std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&s)
                    } else {
                        std::fs::OpenOptions::new().read(true).open(&s)
                    };
                    match opened {
                        Ok(f) => {
                            let fd = self.new_file(f);
                            w!(*d, 0);
                            w!(*d + 1, fd as i32 as u32 as u64);
                        }
                        Err(e) => fs_err(rp, &prog.aggs, *d, &e)?,
                    }
                }
                Ins::FsExists { d, path } => {
                    let s = std::str::from_utf8(Self::fat_bytes(rp, *path as usize)?).unwrap_or("").to_string();
                    w!(*d, std::path::Path::new(&s).exists() as u64);
                }
                Ins::FsRemove { d, path } => {
                    let s = std::str::from_utf8(Self::fat_bytes(rp, *path as usize)?).unwrap_or("").to_string();
                    match std::fs::remove_file(&s) {
                        Ok(()) => w!(*d, 0),
                        Err(e) => fs_err(rp, &prog.aggs, *d, &e)?,
                    }
                }
                Ins::FsSys { d, kind, a, b, c } => {
                    // kind: 0 rename・1 create_dir・2 remove_dir・3 open_append・4 stat・5 read_dir・6 open_dir・7 open_beneath・8 fstat (kir_exec の `FsK` と同じ)。
                    // 7 は a が記述子、b がパス。8 は a が記述子、b が field。
                    let p0 = match *kind {
                        7 => std::str::from_utf8(Self::fat_bytes(rp, *b as usize)?).unwrap_or("").to_string(),
                        8 => String::new(),
                        _ => std::str::from_utf8(Self::fat_bytes(rp, *a as usize)?).unwrap_or("").to_string(),
                    };
                    let res: std::io::Result<u64> = match *kind {
                        6 => crate::fs_beneath::open_dir(&p0).map(|f| self.new_file(f) as u64),
                        7 => {
                            let dirfd = r!(*a) as i32 as i128;
                            let opened = match self.files.get(&dirfd) {
                                Some(d) => crate::fs_beneath::open_beneath(d, &p0),
                                None => Err(std::io::Error::from(std::io::ErrorKind::InvalidInput)),
                            };
                            opened.map(|f| self.new_file(f) as u64)
                        }
                        8 => {
                            let fd = r!(*a) as i32 as i128;
                            let field = r!(*b);
                            match self.files.get(&fd) {
                                Some(f) => crate::fs_beneath::stat_field(f, field),
                                None => Err(std::io::Error::from(std::io::ErrorKind::InvalidInput)),
                            }
                        }
                        0 => {
                            let p1 = std::str::from_utf8(Self::fat_bytes(rp, *b as usize)?).unwrap_or("").to_string();
                            std::fs::rename(&p0, &p1).map(|_| 0)
                        }
                        1 => std::fs::create_dir(&p0).map(|_| 0),
                        2 => std::fs::remove_dir(&p0).map(|_| 0),
                        3 => std::fs::OpenOptions::new().append(true).create(true).open(&p0).map(|f| self.new_file(f) as u64),
                        4 => {
                            let field = r!(*b);
                            std::fs::metadata(&p0).map(|md| match field {
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
                                _ => md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos().min(u64::MAX as u128) as u64).unwrap_or(0),
                            })
                        }
                        _ => {
                            let buf_p = r!(*b);
                            let cap = r!(*c) as usize;
                            std::fs::read_dir(&p0).and_then(|rd| {
                                let mut ents: Vec<(Vec<u8>, u8)> = Vec::new();
                                for ent in rd {
                                    let ent = ent?;
                                    let name = crate::kir_exec::dir_entry_name(&ent).ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
                                    let kd = match std::fs::metadata(ent.path()) {
                                        Ok(md) if md.is_file() => 0u8,
                                        Ok(md) if md.is_dir() => 1u8,
                                        _ => 2u8,
                                    };
                                    ents.push((name, kd));
                                }
                                ents.sort();
                                let mut out = Vec::new();
                                for (n, kd) in &ents {
                                    out.push(*kd);
                                    out.extend_from_slice(n);
                                    out.push(0);
                                }
                                if !out.is_empty() && out.len() <= cap {
                                    let dst = addr(buf_p, out.len()).map_err(std::io::Error::other)?;
                                    unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), dst as *mut u8, out.len()) };
                                }
                                Ok(out.len() as u64)
                            })
                        }
                    };
                    match res {
                        Ok(v) => {
                            w!(*d, 0);
                            w!(*d + 1, v);
                        }
                        Err(e) => fs_err(rp, &prog.aggs, *d, &e)?,
                    }
                }
                Ins::FsClose { d, fd } => {
                    let fd = r!(*fd) as i32 as i128;
                    self.files.remove(&fd);
                    w!(*d, 0);
                }
                Ins::FsSync { d, fd } => {
                    let fd = r!(*fd) as i32 as i128;
                    let Some(f) = self.files.get(&fd) else { return Err(format!("無効なファイル記述子 {}", fd)) };
                    match f.sync_all() {
                        Ok(()) => w!(*d, 0),
                        Err(e) => fs_err(rp, &prog.aggs, *d, &e)?,
                    }
                }
                Ins::FsRead { d, fd, buf, len } => {
                    use std::io::Read;
                    let (fd, p, n) = (r!(*fd) as i32 as i128, r!(*buf), r!(*len) as usize);
                    if fd == 0 {
                        // 記述子 0 は標準入力 (出力を先に流す。kir_exec と同じ)。
                        self.flush();
                        let mut tmp = vec![0u8; n];
                        let res = loop {
                            match std::io::stdin().lock().read(&mut tmp) {
                                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                                r => break r,
                            }
                        };
                        match res {
                            Ok(nr) => {
                                if nr > 0 {
                                    unsafe { std::ptr::copy_nonoverlapping(tmp.as_ptr(), addr(p, nr)?, nr) };
                                }
                                w!(*d, 0);
                                w!(*d + 1, nr as u64);
                            }
                            Err(e) => fs_err(rp, &prog.aggs, *d, &e)?,
                        }
                        continue;
                    }
                    let Some(f) = self.files.get_mut(&fd) else { return Err(format!("無効なファイル記述子 {}", fd)) };
                    let mut tmp = vec![0u8; n];
                    // 中断 (EINTR) は読み直す (kir_exec と同じ)。
                    let res = loop {
                        match f.read(&mut tmp) {
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                            r => break r,
                        }
                    };
                    match res {
                        Ok(nr) => {
                            if nr > 0 {
                                unsafe { std::ptr::copy_nonoverlapping(tmp.as_ptr(), addr(p, nr)?, nr) };
                            }
                            w!(*d, 0);
                            w!(*d + 1, nr as u64);
                        }
                        Err(e) => fs_err(rp, &prog.aggs, *d, &e)?,
                    }
                }
                Ins::FsWrite { d, fd, buf, len } => {
                    use std::io::Write;
                    let (fd, p, n) = (r!(*fd) as i32 as i128, r!(*buf), r!(*len) as usize);
                    let bytes: &[u8] = if n == 0 { &[] } else { unsafe { std::slice::from_raw_parts(addr(p, n)? as *const u8, n) } };
                    // 記述子 2 は標準エラー (`std.io.write_stderr`、kir_exec と同じ)。それ以外は開いているファイルだけ。
                    if fd == 1 {
                        let _ = self.out_write(1, bytes);
                        w!(*d, 0);
                        w!(*d + 1, n as u64);
                    } else if fd == 2 {
                        self.err_write(bytes);
                        w!(*d, 0);
                        w!(*d + 1, n as u64);
                    } else {
                        let Some(f) = self.files.get_mut(&fd) else { return Err(format!("書き込みできるのはファイル記述子だけ: {}", fd)) };
                        match f.write_all(bytes) {
                            Ok(()) => {
                                w!(*d, 0);
                                w!(*d + 1, n as u64);
                            }
                            Err(e) => fs_err(rp, &prog.aggs, *d, &e)?,
                        }
                    }
                }
                Ins::NetLocalPort { d, sock } => {
                    let fd = r!(*sock) as i32 as i128;
                    let la = match self.socks.get(&fd) {
                        Some(NetSock::Listener(l)) => l.local_addr(),
                        Some(NetSock::Udp(u)) => u.local_addr(),
                        _ => return Err(format!("local_port できるのはリスナーソケットか UDP ソケットだけ: {}", fd)),
                    };
                    let port = la.map(|a| a.port() as u64).map_err(|e| format!("local_port: {e}"))?;
                    w!(*d, 0);
                    w!(*d + 1, port);
                }
                Ins::ParseInt { d, s, radix, ty, strict } => {
                    let t = tyof!(ty);
                    let bytes = Self::fat_bytes(rp, *s as usize)?;
                    let (ret, lo, hi) = kir_exec::parse_int(bytes, r!(*radix) as u32, t.bits(), t.signed(), *strict);
                    if ret == 0 {
                        let v = ((hi as u128) << 64) | (lo as u128);
                        let bits = t.bits();
                        let mask = if bits == 128 { u128::MAX } else { (1u128 << bits) - 1 };
                        w!(*d, 0);
                        w!(*d + 1, t.wrap(((v & mask) as u64) as i128) as u64);
                    } else {
                        w!(*d, 1);
                        w!(*d + 1, (ret - 1) as u64);
                    }
                }
            }
        }
    }
}

#[allow(dead_code)]
fn _unused() -> i128 {
    0
}

/// `std.io.Error` を結果 (`Result<..., std.io.Error>`) の Err として `d` に書く (タグ 1、エラーの値は `d + 1` から)。
fn fs_err(rp: *mut u64, aggs: &[crate::kir::AggDef], d: S, e: &std::io::Error) -> Result<(), String> {
    let b = io_error_bytes(aggs, crate::natives::io_kind_name(e), e.raw_os_error().unwrap_or(0))?;
    unsafe { *rp.add(d as usize) = 1 };
    put_bytes(rp, d as usize + 1, &b);
    Ok(())
}

/// スロットの形の `Option` / `Result` (先頭がタグの語、続けて `pw` 語の枠) を、集成体のメモリの形 (枠の `sb` バイト、続けてタグの 1 バイト) へ書く。
/// `dst` は `sb + 1` バイト以上あること。
unsafe fn pack_opt(dst: *mut u8, src: *const u64, sb: u8, pw: u8) {
    let sb = sb as usize;
    std::ptr::write_bytes(dst, 0, sb + 1);
    let n = sb.min(pw as usize * 8);
    std::ptr::copy_nonoverlapping(src.add(1) as *const u8, dst, n);
    *dst.add(sb) = (*src != 0) as u8;
}

/// `pack_opt` の逆: 集成体のメモリの形から、スロットの形 (タグの語、続けて枠の語) へ読む。
unsafe fn unpack_opt(dst: *mut u64, src: *const u8, sb: u8, pw: u8) {
    let sb = sb as usize;
    let pwb = pw as usize * 8;
    std::ptr::write_bytes(dst.add(1) as *mut u8, 0, pwb);
    std::ptr::copy(src, dst.add(1) as *mut u8, sb.min(pwb));
    *dst = (*src.add(sb) & 1) as u64;
}

/// 子プロセスの一時の出力先 (`output` が標準出力・標準エラーを受ける無名のファイル)。作って直ちに名前を消す (Unix)。
#[cfg(unix)]
fn anon_temp_file() -> std::io::Result<std::fs::File> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!("ksn-proc-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let f = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&p)?;
    let _ = std::fs::remove_file(&p);
    Ok(f)
}

/// `Child` (`{ pid, stdin, stdout, stderr }` の 4 つの i32、ないものは -1) の 16 バイト。
fn child_bytes(v: [i32; 4]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// `std.process` の命令。`ProcFn::ALL` の順。結果は `d` に書く (`Result` の Err は `std.io.Error`)。
impl<'a> Vm<'a> {
    fn proc_op(&mut self, rp: *mut u64, aggs: &[crate::kir::AggDef], d: S, f: u8, a: [S; 3]) -> Result<(), String> {
        use crate::kir::ProcFn as P;
        let kind = *crate::kir::ProcFn::ALL.get(f as usize).ok_or("std.process の未知の命令")?;
        let h = |s: S| unsafe { *rp.add(s as usize) as u32 as i32 };
        let fat = |s: S| -> Result<String, String> { Ok(std::str::from_utf8(Self::fat_bytes(rp, s as usize)?).unwrap_or("").to_string()) };
        let unit = |rp: *mut u64, d: S| unsafe { *rp.add(d as usize) = 0 };
        #[cfg(not(unix))]
        {
            let _ = (h, fat, unit);
            let e = std::io::Error::new(std::io::ErrorKind::Unsupported, "std.process は Windows の VM では未対応");
            return fs_err(rp, aggs, d, &e);
        }
        #[cfg(unix)]
        {

            use std::process::{Command, Stdio};
            let wr = |s: S, v: u64| unsafe { *rp.add(s as usize) = v };
            match kind {
                P::New => {
                    let prog = fat(a[0])?;
                    let id = self.next_fd as u32 as u64;
                    self.next_fd += 1;
                    self.cmds.insert(id as i32, Command::new(prog));
                    wr(d, id);
                }
                P::Arg => {
                    let s = fat(a[1])?;
                    if let Some(c) = self.cmds.get_mut(&h(a[0])) {
                        c.arg(s);
                    }
                    unit(rp, d);
                }
                P::EnvRemove => {
                    let s = fat(a[1])?;
                    if s.is_empty() || s.contains('=') || s.contains('\0') {
                        self.bad_cmds.insert(h(a[0]));
                    } else if let Some(c) = self.cmds.get_mut(&h(a[0])) {
                        c.env_remove(s);
                    }
                    unit(rp, d);
                }
                P::Dir => {
                    let s = fat(a[1])?;
                    if let Some(c) = self.cmds.get_mut(&h(a[0])) {
                        c.current_dir(s);
                    }
                    unit(rp, d);
                }
                P::Env => {
                    let (k, v) = (fat(a[1])?, fat(a[2])?);
                    if k.is_empty() || k.contains('=') || k.contains('\0') || v.contains('\0') {
                        self.bad_cmds.insert(h(a[0]));
                    } else if let Some(c) = self.cmds.get_mut(&h(a[0])) {
                        c.env(k, v);
                    }
                    unit(rp, d);
                }
                P::EnvClear => {
                    self.bad_cmds.remove(&h(a[0]));
                    if let Some(c) = self.cmds.get_mut(&h(a[0])) {
                        c.env_clear();
                    }
                    unit(rp, d);
                }
                P::InPiped | P::InNull | P::OutPiped | P::OutNull | P::ErrPiped | P::ErrNull => {
                    let mode = |piped: bool| if piped { Stdio::piped() } else { Stdio::null() };
                    if let Some(c) = self.cmds.get_mut(&h(a[0])) {
                        match kind {
                            P::InPiped => c.stdin(mode(true)),
                            P::InNull => c.stdin(mode(false)),
                            P::OutPiped => c.stdout(mode(true)),
                            P::OutNull => c.stdout(mode(false)),
                            P::ErrPiped => c.stderr(mode(true)),
                            _ => c.stderr(mode(false)),
                        };
                    }
                    unit(rp, d);
                }
                P::Cancel => {
                    self.cmds.remove(&h(a[0]));
                    unit(rp, d);
                }
                P::Spawn => {
                    // 子が継承した標準出力へ書く前に、ためた出力を流す (順序を保つ)。
                    self.flush();
                    if self.bad_cmds.remove(&h(a[0])) {
                        self.cmds.remove(&h(a[0]));
                        let e = std::io::Error::new(std::io::ErrorKind::InvalidInput, "環境変数の鍵か値が不正");
                        return fs_err(rp, aggs, d, &e);
                    }
                    let Some(mut c) = self.cmds.remove(&h(a[0])) else { return Err("無効な Command の記述子".into()) };
                    match c.spawn() {
                        Ok(mut ch) => {
                            let mut fds = [-1i32; 3];
                            if let Some(s) = ch.stdin.take() {
                                fds[0] = self.new_file(std::fs::File::from(std::os::unix::io::OwnedFd::from(s))) as i32;
                            }
                            if let Some(s) = ch.stdout.take() {
                                fds[1] = self.new_file(std::fs::File::from(std::os::unix::io::OwnedFd::from(s))) as i32;
                            }
                            if let Some(s) = ch.stderr.take() {
                                fds[2] = self.new_file(std::fs::File::from(std::os::unix::io::OwnedFd::from(s))) as i32;
                            }
                            let pid = ch.id() as i32;
                            self.children.insert(pid, ch);
                            wr(d, 0);
                            put_bytes(rp, d as usize + 1, &child_bytes([pid, fds[0], fds[1], fds[2]]));
                        }
                        Err(e) => fs_err(rp, aggs, d, &e)?,
                    }
                }
                P::Output => {
                    if self.bad_cmds.remove(&h(a[0])) {
                        self.cmds.remove(&h(a[0]));
                        let e = std::io::Error::new(std::io::ErrorKind::InvalidInput, "環境変数の鍵か値が不正");
                        return fs_err(rp, aggs, d, &e);
                    }
                    let Some(mut c) = self.cmds.remove(&h(a[0])) else { return Err("無効な Command の記述子".into()) };
                    c.stdin(Stdio::null());
                    match c.output() {
                        Ok(o) => {
                            let (code, sig) = status_parts(&o.status);
                            let mut fds = [-1i32; 2];
                            for (k, bytes) in [o.stdout, o.stderr].into_iter().enumerate() {
                                let mut f = anon_temp_file().map_err(|e| format!("一時ファイル: {e}"))?;
                                std::io::Write::write_all(&mut f, &bytes).map_err(|e| format!("一時ファイル: {e}"))?;
                                std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(0)).map_err(|e| format!("一時ファイル: {e}"))?;
                                fds[k] = self.new_file(f) as i32;
                            }
                            wr(d, 0);
                            put_bytes(rp, d as usize + 1, &child_bytes([code, sig, fds[0], fds[1]]));
                        }
                        Err(e) => fs_err(rp, aggs, d, &e)?,
                    }
                }
                P::Wait => {
                    let pid = h(a[0]);
                    let Some(mut ch) = self.children.remove(&pid) else {
                        let e = std::io::Error::new(std::io::ErrorKind::InvalidInput, "子プロセスではない記述子");
                        return fs_err(rp, aggs, d, &e);
                    };
                    match ch.wait() {
                        Ok(st) => {
                            let (code, sig) = status_parts(&st);
                            wr(d, 0);
                            put_bytes(rp, d as usize + 1, &child_bytes([code, sig, 0, 0])[..8]);
                        }
                        Err(e) => fs_err(rp, aggs, d, &e)?,
                    }
                }
                P::Kill => {
                    let pid = h(a[0]);
                    let r = match self.children.get_mut(&pid) {
                        Some(ch) => ch.kill(),
                        None => Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "子プロセスではない記述子")),
                    };
                    match r {
                        Ok(()) => unit(rp, d),
                        Err(e) => fs_err(rp, aggs, d, &e)?,
                    }
                }
            }
            Ok(())
        }
    }
}

/// 終了の状態 (`code`、シグナルで終わったときは `-1`、`sig`)。Unix のシグナル番号は `sig`、それ以外は 0 (`sysrt.c` の `wait_status` と同じ)。
#[cfg(unix)]
fn status_parts(st: &std::process::ExitStatus) -> (i32, i32) {
    use std::os::unix::process::ExitStatusExt;
    match st.code() {
        Some(c) => (c, 0),
        None => (-1, st.signal().unwrap_or(0)),
    }
}
