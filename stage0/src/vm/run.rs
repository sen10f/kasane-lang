//! VM の実行ループ (設計_KIR.md 4.2 / 4.3)。呼び出しは明示的なフレーム列 (Rust の再帰を使わない)。
//!
//! 意味は参照実行器 (`kir_exec.rs`) と同じ: 値の符号化、panic の文言、呼び出し深さの上限 (2500、超えたら 102)、
//! 集成体の置き場 (`heap`) と戻るときの複製 (`relocate`)。違いは次の 3 点:
//! - バイト記憶は実メモリ (`malloc` / `realloc` / `free` の実アドレス。7 章 1 の決定)。仮想アドレスの検査・未初期化の検出・漏れの検査はしない
//!   (null 付近のアドレスだけは内部エラーにする)。
//! - 定義される前の読みを検出しない (検査は `verify_module` の責務)。
//! - stdout は書き込みをためて、終了・panic・stderr への出力の前に流す。

use super::bc::*;
use crate::interp::{py_float, py_float32};
use crate::kir::{payload_bits, payload_of, Aop, Mode, Ty, PAY_MASK, TAG_BIT};
use crate::kir_exec::{self, Outcome};
use crate::kir_scalar;

/// 木走査インタプリタの呼び出し深さの上限 (`interp.rs` の `depth > 2500`)。
const MAX_DEPTH: usize = 2500;

extern "C" {
    fn malloc(n: usize) -> *mut std::ffi::c_void;
    fn calloc(n: usize, m: usize) -> *mut std::ffi::c_void;
    fn realloc(p: *mut std::ffi::c_void, n: usize) -> *mut std::ffi::c_void;
    fn free(p: *mut std::ffi::c_void);
}

struct Frame {
    /// 戻り先 (呼び出し元) の関数・命令番号・フレームの底。
    f: u32,
    pc: u32,
    base: u32,
    /// 呼び出し元で結果を受けるスロット。
    dst: S,
    /// 呼び出し先が始まったときの集成体の置き場の長さ・フレームの領域の数。
    hbase: usize,
    abase: usize,
}

pub struct Vm<'a> {
    prog: &'a Prog,
    regs: Vec<i128>,
    frames: Vec<Frame>,
    /// 集成体の置き場 (値は添字)。
    pub heap: Vec<Vec<u8>>,
    /// 関数のフレームの領域 (`frame.bytes` / `alloca`)。戻るときに解放する。
    allocs: Vec<*mut std::ffi::c_void>,
    out: Vec<u8>,
    /// `Some` なら stderr へ書かずにためる (テストの panic の文言を取り出すため)。
    pub capture: Option<Vec<u8>>,
    /// プログラムへの引数 (`std.env.arg`。プログラム名は含まない)。
    pub args: Vec<String>,
    /// `std.net` のソケット表 (記述子 → ソケット)。
    socks: std::collections::HashMap<i128, NetSock>,
    next_fd: i128,
}

enum NetSock {
    Listener(std::net::TcpListener),
    Stream(std::net::TcpStream),
}

#[inline(always)]
fn fast_lo_hi(t: u8) -> (i128, i128) {
    (LO[t as usize], HI[t as usize])
}

fn bad_addr(a: i128, n: usize) -> String {
    format!("不正なメモリアクセス {:#x} ({} バイト)", a, n)
}

#[inline(always)]
fn addr(a: i128, n: usize) -> Result<*mut u8, String> {
    if a < 0x1000 || a > u64::MAX as i128 {
        return Err(bad_addr(a, n));
    }
    Ok(a as usize as *mut u8)
}

impl<'a> Vm<'a> {
    pub fn new(prog: &'a Prog) -> Vm<'a> {
        Vm { prog, regs: vec![0; 1 << 14], frames: vec![], heap: vec![], allocs: vec![], out: Vec::new(), capture: None, args: vec![], socks: Default::default(), next_fd: 0 }
    }

    /// ためた stdout を流す。
    pub fn flush(&mut self) {
        use std::io::Write;
        if !self.out.is_empty() {
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(&self.out);
            let _ = o.flush();
            self.out.clear();
        }
    }

    /// stderr へ書く (stdout を先に流す。`capture` があればためる)。
    pub fn err_write(&mut self, bytes: &[u8]) {
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
                if self.out.len() >= 1 << 16 {
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

    fn fat(&self, h: i128) -> Result<(i128, usize), String> {
        let b = self.heap.get(h as usize).filter(|b| b.len() >= 16).ok_or("fat の項目が不正")?;
        let ptr = i64::from_le_bytes(b[0..8].try_into().expect("fat の ptr")) as i128;
        let len = u64::from_le_bytes(b[8..16].try_into().expect("fat の len")) as usize;
        Ok((ptr, len))
    }

    /// `bytes` を複写した (解放しない) 領域を指す fat を置き場に作り、その添字を返す。
    fn leak_fat(&mut self, bytes: &[u8]) -> i128 {
        let p = unsafe { malloc(bytes.len().max(1)) } as *mut u8;
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len()) };
        let mut b = vec![0u8; 16];
        b[0..8].copy_from_slice(&(p as u64).to_le_bytes());
        b[8..16].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
        self.heap.push(b);
        (self.heap.len() - 1) as i128
    }

    fn new_fd(&mut self, sock: NetSock) -> i128 {
        let fd = self.next_fd;
        self.next_fd += 1;
        self.socks.insert(fd, sock);
        fd
    }

    /// 記述子 `fd` を持つ集成体 (`i32` 1 つ) を置き場に作り、その添字を返す。
    fn desc(&mut self, fd: i128) -> i128 {
        self.heap.push((fd as i32).to_le_bytes().to_vec());
        (self.heap.len() - 1) as i128
    }

    fn fat_bytes(&self, h: i128) -> Result<&'static [u8], String> {
        let (p, n) = self.fat(h)?;
        if n == 0 {
            return Ok(&[]);
        }
        let ptr = addr(p, n)?;
        // SAFETY: KIR の検査済みの fat は、生きている領域の `len` バイトを指す。
        Ok(unsafe { std::slice::from_raw_parts(ptr as *const u8, n) })
    }

    /// 関数 `key` を引数 `args` で実行する。同じ `Vm` で続けて呼べる (置き場は戻り値が指す項目だけ残る)。
    pub fn run(&mut self, key: &str, args: &[i128]) -> Result<Outcome, String> {
        let Some(entry) = self.prog.find(key) else {
            return Err(format!("関数 {} がない", key));
        };
        let prog = self.prog;
        let f0 = &prog.funcs[entry];
        if args.len() != f0.nparams as usize {
            return Err(format!("関数 {} の引数の個数が違う", key));
        }
        if f0.nslots == 0 && f0.code.is_empty() {
            return Err(format!("関数 {} は本体がない", key));
        }
        self.frames.clear();
        self.allocs.clear();
        let top_hbase = self.heap.len();
        let mut fi = entry as u32;
        let mut f = f0;
        let mut pc = 0usize;
        let mut base = 0usize;
        if self.regs.len() < f.nslots as usize {
            self.regs.resize(f.nslots as usize * 2, 0);
        }
        for (i, a) in args.iter().enumerate() {
            self.regs[i] = *a;
        }
        let mut rp: *mut i128 = self.regs.as_mut_ptr();
        macro_rules! r {
            ($s:expr) => {
                unsafe { *rp.add($s as usize) }
            };
        }
        macro_rules! w {
            ($s:expr, $v:expr) => {{
                let v = $v;
                unsafe { *rp.add($s as usize) = v }
            }};
        }
        macro_rules! panic_out {
            ($m:expr) => {{
                self.flush();
                return Ok(Outcome::Panic($m));
            }};
        }
        loop {
            // SAFETY: コンパイラは各関数の最後に `Ret` / `Jmp` / `Unreachable` を置くので `pc` は範囲内。
            let ins = unsafe { f.code.get_unchecked(pc) };
            pc += 1;
            match ins {
                Ins::ConstI { d, v } => w!(*d, *v as i128),
                Ins::ConstW { d, k } => w!(*d, prog.wide[*k as usize]),
                Ins::Mov { d, s } => w!(*d, r!(*s)),
                Ins::AddImm { d, a, imm } => w!(*d, r!(*a) + *imm as i128),
                Ins::PtrAdd { d, p, off } => w!(*d, r!(*p) + r!(*off)),
                Ins::Jmp { to } => pc = *to as usize,
                Ins::JmpIfNot { c, to } => {
                    if r!(*c) == 0 {
                        pc = *to as usize;
                    }
                }
                Ins::JCmpNot { cc, a, b, to } => {
                    let (x, y) = (r!(*a), r!(*b));
                    let t = match cc {
                        crate::kir::Cc::Eq => x == y,
                        crate::kir::Cc::Ne => x != y,
                        crate::kir::Cc::Lt => x < y,
                        crate::kir::Cc::Le => x <= y,
                        crate::kir::Cc::Gt => x > y,
                        crate::kir::Cc::Ge => x >= y,
                    };
                    if !t {
                        pc = *to as usize;
                    }
                }
                Ins::AddT { d, a, b, t } => {
                    let v = r!(*a) + r!(*b);
                    let (lo, hi) = fast_lo_hi(*t);
                    if v < lo || v > hi {
                        panic_out!(kir_exec::overflow_msg(Aop::Add));
                    }
                    w!(*d, v);
                }
                Ins::SubT { d, a, b, t } => {
                    let v = r!(*a) - r!(*b);
                    let (lo, hi) = fast_lo_hi(*t);
                    if v < lo || v > hi {
                        panic_out!(kir_exec::overflow_msg(Aop::Sub));
                    }
                    w!(*d, v);
                }
                Ins::MulT { d, a, b, t } => {
                    let (lo, hi) = fast_lo_hi(*t);
                    match r!(*a).checked_mul(r!(*b)) {
                        Some(v) if v >= lo && v <= hi => w!(*d, v),
                        _ => panic_out!(kir_exec::overflow_msg(Aop::Mul)),
                    }
                }
                Ins::AddW { d, a, b, t } => w!(*d, wrap_fast(*t, r!(*a).wrapping_add(r!(*b)))),
                Ins::SubW { d, a, b, t } => w!(*d, wrap_fast(*t, r!(*a).wrapping_sub(r!(*b)))),
                Ins::MulW { d, a, b, t } => w!(*d, wrap_fast(*t, r!(*a).wrapping_mul(r!(*b)))),
                Ins::WrapT { d, a, t } => w!(*d, wrap_fast(*t, r!(*a))),
                Ins::Arith { d, a, b, op, mode, ty } => match kir_exec::arith(*op, *mode, prog.tys[*ty as usize], r!(*a), r!(*b)) {
                    Ok(v) => w!(*d, v),
                    Err(m) => panic_out!(m),
                },
                Ins::CmpI { d, a, b, cc } => {
                    let (x, y) = (r!(*a), r!(*b));
                    let t = match cc {
                        crate::kir::Cc::Eq => x == y,
                        crate::kir::Cc::Ne => x != y,
                        crate::kir::Cc::Lt => x < y,
                        crate::kir::Cc::Le => x <= y,
                        crate::kir::Cc::Gt => x > y,
                        crate::kir::Cc::Ge => x >= y,
                    };
                    w!(*d, t as i128);
                }
                Ins::Cmp { d, a, b, cc, ty } => w!(*d, kir_scalar::cmp(*cc, prog.tys[*ty as usize], r!(*a), r!(*b)) as i128),
                Ins::Neg { d, a, mode, ty } => {
                    let t = prog.tys[*ty as usize];
                    let x = r!(*a);
                    match mode {
                        Mode::Wrap => w!(*d, kir_scalar::wrap_neg(t, x)),
                        _ => {
                            if x == t.min() {
                                panic_out!("attempt to negate with overflow".to_string());
                            }
                            w!(*d, -x);
                        }
                    }
                }
                Ins::FArith { d, a, b, op, ty } => w!(*d, kir_scalar::farith(*op, prog.tys[*ty as usize], r!(*a), r!(*b))),
                Ins::Shift { d, a, n, left, ty } => w!(*d, kir_scalar::shift(*left, prog.tys[*ty as usize], r!(*a), r!(*n))),
                Ins::MinMax { d, a, b, max, ty } => w!(*d, kir_scalar::minmax(*max, prog.tys[*ty as usize], r!(*a), r!(*b))),
                Ins::FUn { d, a, f: fu, src, res } => w!(*d, kir_scalar::funary(*fu, prog.tys[*src as usize], prog.tys[*res as usize], r!(*a))),
                Ins::FBin { d, a, b, f: fb, res } => w!(*d, kir_scalar::fbin(*fb, prog.tys[*res as usize], r!(*a), r!(*b))),
                Ins::Conv { d, a, src, dst } => w!(*d, kir_scalar::conv(prog.tys[*src as usize], prog.tys[*dst as usize], r!(*a))),
                Ins::NotB { d, a } => w!(*d, (r!(*a) == 0) as i128),
                Ins::NotI { d, a, ty } => w!(*d, kir_scalar::not_int(prog.tys[*ty as usize], r!(*a))),
                Ins::BitOp { d, a, b, k } => {
                    let (x, y) = (r!(*a), r!(*b));
                    w!(*d, match k {
                        0 => x & y,
                        1 => x | y,
                        _ => x ^ y,
                    });
                }
                Ins::BitCount { d, a, k, src } => w!(*d, kir_scalar::bitcount(*k, prog.tys[*src as usize], r!(*a))),
                Ins::Checked { d, a, b, op, ty } => w!(*d, kir_exec::checked(*op, prog.tys[*ty as usize], r!(*a), r!(*b))),
                Ins::Call { f: ci, d, argp, argn } => {
                    if self.frames.len() + 1 >= MAX_DEPTH {
                        self.flush();
                        return Ok(Outcome::StackOverflow);
                    }
                    let cf = &prog.funcs[*ci as usize];
                    let nb = base + f.nslots as usize;
                    if self.regs.len() < nb + cf.nslots as usize {
                        let n = (nb + cf.nslots as usize) * 2;
                        self.regs.resize(n, 0);
                        rp = unsafe { self.regs.as_mut_ptr().add(base) };
                    }
                    // 引数を新しいフレームの先頭へ写す。
                    for k in 0..*argn as usize {
                        let s = f.pool[*argp as usize + k];
                        let v = r!(s);
                        unsafe { *rp.add(f.nslots as usize + k) = v };
                    }
                    self.frames.push(Frame { f: fi, pc: pc as u32, base: base as u32, dst: *d, hbase: self.heap.len(), abase: self.allocs.len() });
                    fi = *ci;
                    f = cf;
                    pc = 0;
                    base = nb;
                    rp = unsafe { self.regs.as_mut_ptr().add(base) };
                    continue;
                }
                Ins::CallExt { e, d, argp, argn } => {
                    let func = &prog.ext[*e as usize];
                    let argv: Vec<i128> = (0..*argn as usize).map(|k| r!(f.pool[*argp as usize + k])).collect();
                    let v = kir_exec::extern_call(func, &argv)?;
                    w!(*d, v);
                }
                Ins::Ret { v } => {
                    let mut val = r!(*v);
                    let (hbase, abase) = match self.frames.last() {
                        Some(fr) => (fr.hbase, fr.abase),
                        None => (top_hbase, 0),
                    };
                    for p in self.allocs.drain(abase..) {
                        unsafe { free(p) };
                    }
                    // 戻る関数が作った置き場の項目は捨てる。戻り値が参照する項目だけは、呼び出し元の置き場へ複製して残す。
                    if f.ret_heap {
                        let blobs = self.heap.split_off(hbase);
                        let mut out: Vec<Vec<u8>> = vec![];
                        val = kir_exec::relocate(f.ret, val, &blobs, hbase, &mut out);
                        self.heap.extend(out);
                    } else {
                        self.heap.truncate(hbase);
                    }
                    match self.frames.pop() {
                        None => {
                            self.flush();
                            return Ok(Outcome::Value(val));
                        }
                        Some(fr) => {
                            fi = fr.f;
                            f = &prog.funcs[fi as usize];
                            pc = fr.pc as usize;
                            base = fr.base as usize;
                            rp = unsafe { self.regs.as_mut_ptr().add(base) };
                            if fr.dst != NO_SLOT {
                                w!(fr.dst, val);
                            }
                        }
                    }
                }
                Ins::Unreachable => return Err("到達しないブロックに入った".to_string()),
                Ins::Make { d, info, argp } => {
                    let mi = &prog.makes[*info as usize];
                    let mut buf = vec![0u8; mi.size as usize];
                    if let Some(k) = mi.variant {
                        buf[0..4].copy_from_slice(&(k as i32).to_le_bytes());
                    }
                    for (k, (off, t)) in mi.fields.iter().enumerate() {
                        let v = r!(f.pool[*argp as usize + k]);
                        kir_exec::write_val(&mut buf, *off as usize, &prog.aggs, *t, v, &self.heap);
                    }
                    self.heap.push(buf);
                    w!(*d, (self.heap.len() - 1) as i128);
                }
                Ins::Field { d, a, off, ty } => {
                    let t = prog.tys[*ty as usize];
                    let h = r!(*a) as usize;
                    let o = *off as usize;
                    // タグが別の腕の値を、判定の前に読むことがある (`Option<Vec>` の `None` のペイロード)。項目が無い・短いときは 0 として読む。
                    let need = crate::kir::size_of(&prog.aggs, t) as usize;
                    let ok = matches!(self.heap.get(h), Some(b) if b.len() >= o + need);
                    let v = match t {
                        Ty::Agg(ai) => {
                            let n = prog.aggs[ai as usize].size as usize;
                            let sub = if ok { self.heap[h][o..o + n].to_vec() } else { vec![0u8; n] };
                            self.heap.push(sub);
                            (self.heap.len() - 1) as i128
                        }
                        t => {
                            if ok {
                                kir_exec::read_scalar(&self.heap[h], o, t)
                            } else {
                                kir_exec::read_scalar(&[0u8; 16], 0, t)
                            }
                        }
                    };
                    w!(*d, v);
                }
                Ins::SetField { d, a, v, off, agg } => {
                    let h = r!(*a) as usize;
                    let dd = &prog.aggs[*agg as usize];
                    let Some(fty) = kir_exec::field_ty_at(dd, *off) else {
                        return Err("setfield のフィールドがない".to_string());
                    };
                    let mut buf = self.heap[h].clone();
                    kir_exec::write_val(&mut buf, *off as usize, &prog.aggs, fty, r!(*v), &self.heap);
                    self.heap.push(buf);
                    w!(*d, (self.heap.len() - 1) as i128);
                }
                Ins::TagEnum { d, a } => {
                    let h = r!(*a) as usize;
                    let b = &self.heap[h];
                    w!(*d, i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i128);
                }
                Ins::TagOpt { d, a } => w!(*d, (r!(*a) & TAG_BIT != 0) as i128),
                Ins::Some { d, a, pay } => {
                    let Ty::Opt(p) = prog.tys[*pay as usize] else { return Err("some の結果は Option のはず".to_string()) };
                    let v = kir_exec::pay_word(p, r!(*a), &mut self.heap);
                    w!(*d, TAG_BIT | v);
                }
                Ins::Ok { d, a, pay } => {
                    let Ty::Res(t0, _) = prog.tys[*pay as usize] else { return Err("ok の結果は Result のはず".to_string()) };
                    let v = kir_exec::pay_word(t0, r!(*a), &mut self.heap);
                    w!(*d, v);
                }
                Ins::Err { d, a, pay } => {
                    let Ty::Res(_, e0) = prog.tys[*pay as usize] else { return Err("err の結果は Result のはず".to_string()) };
                    let v = kir_exec::pay_word(e0, r!(*a), &mut self.heap);
                    w!(*d, TAG_BIT | v);
                }
                Ins::Payload { d, a, ty } => w!(*d, kir_exec::pay_read(prog.tys[*ty as usize], r!(*a) & PAY_MASK, &self.heap)),
                Ins::FrameBytes { d, size } => {
                    let p = unsafe { calloc((*size as usize).max(1), 1) };
                    self.allocs.push(p);
                    w!(*d, p as usize as i128);
                }
                Ins::Load { d, p, off, k } => {
                    let a = addr(r!(*p) + *off as i128, 1)?;
                    // SAFETY: KIR の検査済みの参照・ptr は、生きている領域の十分な幅を指す。
                    let v: i128 = unsafe {
                        match k {
                            LK::U8 => *a as i128,
                            LK::I8 => *(a as *const i8) as i128,
                            LK::Bool => (*a != 0) as i128,
                            LK::U16 => (a as *const u16).read_unaligned() as i128,
                            LK::I16 => (a as *const i16).read_unaligned() as i128,
                            LK::U32 => (a as *const u32).read_unaligned() as i128,
                            LK::I32 => (a as *const i32).read_unaligned() as i128,
                            LK::U64 => (a as *const u64).read_unaligned() as i128,
                            LK::I64 => (a as *const i64).read_unaligned() as i128,
                            LK::I128 => (a as *const i128).read_unaligned(),
                        }
                    };
                    w!(*d, v);
                }
                Ins::LoadAgg { d, p, size } => {
                    let n = *size as usize;
                    let a = addr(r!(*p), n)?;
                    let blob = unsafe { std::slice::from_raw_parts(a as *const u8, n) }.to_vec();
                    self.heap.push(blob);
                    w!(*d, (self.heap.len() - 1) as i128);
                }
                Ins::Store { p, off, v, w: wd } => {
                    let a = addr(r!(*p) + *off as i128, *wd as usize)?;
                    let x = r!(*v);
                    unsafe {
                        match wd {
                            1 => *a = x as u8,
                            2 => (a as *mut u16).write_unaligned(x as u16),
                            4 => (a as *mut u32).write_unaligned(x as u32),
                            8 => (a as *mut u64).write_unaligned(x as u64),
                            _ => (a as *mut u128).write_unaligned(x as u128),
                        }
                    }
                }
                Ins::StoreAgg { p, v } => {
                    let blob = &self.heap[r!(*v) as usize];
                    let a = addr(r!(*p), blob.len())?;
                    unsafe { std::ptr::copy_nonoverlapping(blob.as_ptr(), a, blob.len()) };
                }
                Ins::HeapAlloc { d, n } => {
                    let nn = (r!(*n) as usize).max(1);
                    let p = unsafe { malloc(nn) };
                    w!(*d, p as usize as i128);
                }
                Ins::HeapRealloc { d, p, n } => {
                    let old = r!(*p);
                    let nn = (r!(*n) as usize).max(1);
                    let q = unsafe { realloc(old as usize as *mut std::ffi::c_void, nn) };
                    w!(*d, q as usize as i128);
                }
                Ins::HeapFree { p } => {
                    let a = r!(*p);
                    if a != 0 {
                        unsafe { free(a as usize as *mut std::ffi::c_void) };
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
                        Ok(_) => n as i128,
                        Err(e) => e.valid_up_to() as i128,
                    });
                }
                Ins::Panic { msg, arg } => {
                    let m = &prog.strs[*msg as usize];
                    let m = if *arg == NO_SLOT { m.clone() } else { format!("{}: {}", m, r!(*arg)) };
                    panic_out!(m);
                }
                Ins::Abort => {
                    self.flush();
                    return Ok(Outcome::Abort);
                }
                Ins::Write { d, fd, data } => {
                    let bytes = self.fat_bytes(r!(*data))?;
                    match self.out_write(r!(*fd), bytes) {
                        Ok(()) => w!(*d, 0),
                        Err(e) => {
                            let v = kir_exec::io_error_value(&prog.aggs, &mut self.heap, crate::natives::io_kind_name(&e), e.raw_os_error().unwrap_or(0))?;
                            w!(*d, TAG_BIT | v);
                        }
                    }
                }
                Ins::Eprint { data } => {
                    let bytes = self.fat_bytes(r!(*data))?;
                    self.err_write(bytes);
                }
                Ins::FmtFloat { a, bits } => {
                    let x = r!(*a);
                    let s = if *bits == 32 { py_float32(f32::from_bits(x as u32)) } else { py_float(f64::from_bits(x as u64)) };
                    self.err_write(s.as_bytes());
                }
                Ins::EnvArgCount { d } => {
                    w!(*d, self.args.len() as i128);
                }
                Ins::EnvArg { d, i } => {
                    let i = r!(*i) as usize;
                    if i >= self.args.len() {
                        w!(*d, 0);
                    } else {
                        let a = self.args[i].clone();
                        let h = self.leak_fat(a.as_bytes());
                        w!(*d, TAG_BIT | h);
                    }
                }
                Ins::EnvVar { d, name } => {
                    let bytes = self.fat_bytes(r!(*name))?;
                    match std::env::var(std::str::from_utf8(bytes).unwrap_or("")) {
                        Ok(v) => {
                            let h = self.leak_fat(v.as_bytes());
                            w!(*d, TAG_BIT | h);
                        }
                        Err(_) => w!(*d, 0),
                    }
                }
                Ins::NetListen { d, addr } | Ins::NetConnect { d, addr } => {
                    let bytes = self.fat_bytes(r!(*addr))?;
                    let a = std::str::from_utf8(bytes).unwrap_or("");
                    let is_listen = matches!(ins, Ins::NetListen { .. });
                    let res = if is_listen { std::net::TcpListener::bind(a).map(NetSock::Listener) } else { std::net::TcpStream::connect(a).map(NetSock::Stream) };
                    match res {
                        Ok(sock) => {
                            let fd = self.new_fd(sock);
                            w!(*d, self.desc(fd));
                        }
                        Err(e) => {
                            // listen / connect / accept の失敗は `std.net.Error` (`{ code: i32 }`。kir_exec と同じ。kir/net-refs)。
                            let v = kir_exec::net_error_agg(&mut self.heap, e.raw_os_error().unwrap_or(-1))?;
                            w!(*d, TAG_BIT | v);
                        }
                    }
                }
                Ins::NetAccept { d, sock } => {
                    let fd = r!(*sock);
                    let Some(NetSock::Listener(l)) = self.socks.get_mut(&fd) else { return Err(format!("無効なソケット記述子 {}", fd)) };
                    match l.accept() {
                        Ok((st, _)) => {
                            let nfd = self.new_fd(NetSock::Stream(st));
                            w!(*d, self.desc(nfd));
                        }
                        Err(e) => {
                            // listen / connect / accept の失敗は `std.net.Error` (`{ code: i32 }`。kir_exec と同じ。kir/net-refs)。
                            let v = kir_exec::net_error_agg(&mut self.heap, e.raw_os_error().unwrap_or(-1))?;
                            w!(*d, TAG_BIT | v);
                        }
                    }
                }
                Ins::NetRead { d, sock, buf, len } => {
                    use std::io::Read;
                    let (fd, p, n) = (r!(*sock), r!(*buf), r!(*len) as usize);
                    let Some(NetSock::Stream(st)) = self.socks.get_mut(&fd) else { return Err(format!("読み出しできるのはストリームソケットだけ: {}", fd)) };
                    let mut tmp = vec![0u8; n];
                    let nr = st.read(&mut tmp).map_err(|e| format!("read: {e}"))?;
                    if nr > 0 {
                        unsafe { std::ptr::copy_nonoverlapping(tmp.as_ptr(), addr(p, nr)?, nr) };
                    }
                    w!(*d, nr as i128);
                }
                Ins::NetWrite { d, sock, buf, len } => {
                    use std::io::Write;
                    let (fd, p, n) = (r!(*sock), r!(*buf), r!(*len) as usize);
                    let Some(NetSock::Stream(st)) = self.socks.get_mut(&fd) else { return Err(format!("書き込みできるのはストリームソケットだけ: {}", fd)) };
                    let bytes: &[u8] = if n == 0 { &[] } else { unsafe { std::slice::from_raw_parts(addr(p, n)? as *const u8, n) } };
                    st.write_all(bytes).map_err(|e| format!("write: {e}"))?;
                    w!(*d, n as i128);
                }
                Ins::NetFlush { d, sock } => {
                    use std::io::Write;
                    let fd = r!(*sock);
                    let Some(NetSock::Stream(st)) = self.socks.get_mut(&fd) else { return Err(format!("flush できるのはストリームソケットだけ: {}", fd)) };
                    st.flush().map_err(|e| format!("flush: {e}"))?;
                    w!(*d, 0);
                }
                Ins::NetClose { d, sock } => {
                    let fd = r!(*sock);
                    self.socks.remove(&fd);
                    w!(*d, 0);
                }
                Ins::NetLocalPort { d, sock } => {
                    let fd = r!(*sock);
                    let Some(NetSock::Listener(l)) = self.socks.get(&fd) else { return Err(format!("local_port できるのはリスナーソケットだけ: {}", fd)) };
                    w!(*d, l.local_addr().map(|a| a.port() as i128).map_err(|e| format!("local_port: {e}"))?);
                }
                Ins::ParseInt { d, s, radix, ty } => {
                    let t = prog.tys[*ty as usize];
                    let bytes = self.fat_bytes(r!(*s))?;
                    let (ret, lo, hi) = kir_exec::parse_int(bytes, r!(*radix) as u32, t.bits(), t.signed());
                    if ret == 0 {
                        let v = ((hi as u128) << 64) | (lo as u128);
                        let bits = t.bits();
                        let mask = if bits == 128 { u128::MAX } else { (1u128 << bits) - 1 };
                        w!(*d, (v & mask) as u64 as i128);
                    } else {
                        let kind = (ret - 1) as u8;
                        self.heap.push(vec![kind]);
                        w!(*d, TAG_BIT | (self.heap.len() - 1) as i128);
                    }
                }
            }
        }
    }
}

#[allow(dead_code)]
fn _unused(_: i128) -> i128 {
    payload_bits(0) + payload_of(Ty::Unit, 0)
}
