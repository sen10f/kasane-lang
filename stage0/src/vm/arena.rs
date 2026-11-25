//! VM の確保領域 (実行時の安全網)。
//!
//! VM のバイト記憶は、1 つの大きな仮想領域 (`mmap` の `MAP_NORESERVE`、既定 16 GiB。読み書きは全域でできる) の中だけに置く。
//! 検査の抜けや偽の `Vec` の見出しで偽の番地が読まれても、`addr` が領域の外を弾いて panic (101) にするので、`ksn` のプロセスは壊れない
//! (領域の内側の読み書きは、解放済みの別の値を読むことはあっても、ホストのメモリには届かない)。
//! 確保は 16 バイトの見出し (大きさの区分) + 2 の冪の区分ごとの空きの一覧。`C` の関数を呼ぶ (`CallExt`) プログラムは、C が返す番地も
//! 読むので `permissive` で領域の外も通す (FFI は `[unsafe]` の許可の内側の仕事)。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

extern "C" {
    fn munmap(addr: *mut std::ffi::c_void, len: usize) -> i32;
    fn mmap(addr: *mut std::ffi::c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut std::ffi::c_void;
}

static BASE: AtomicUsize = AtomicUsize::new(0);
static LIMIT: AtomicUsize = AtomicUsize::new(0);
static SHADOW: AtomicUsize = AtomicUsize::new(0);
static DEAD_ANY: AtomicBool = AtomicBool::new(false);
/// 解放した領域を寝かせる上限 (バイト)。
const QUARANTINE_MAX: usize = 4 << 20;
static PERMISSIVE: AtomicBool = AtomicBool::new(false);

const HDR: usize = 16;
const NCLASS: usize = 48;

struct Arena {
    top: usize,
    quarantine: std::collections::VecDeque<(usize, usize, usize)>,
    qbytes: usize,
    free: [Vec<usize>; NCLASS],
}

static ARENA: Mutex<Option<Arena>> = Mutex::new(None);

fn init() -> bool {
    if BASE.load(Ordering::Relaxed) != 0 {
        return true;
    }
    // PROT_READ|PROT_WRITE = 3、MAP_PRIVATE|MAP_ANONYMOUS|MAP_NORESERVE = 0x2 | 0x20 | 0x4000。
    // 既定は 1 TiB (遅延確保なので実メモリは使わない)。仮想メモリの上限 (`ulimit -v`) などで取れなければ 1/8 ずつ縮めて取れる最大を探す。
    let mut len: usize = 1 << 40;
    while len >= 1 << 28 {
        let p = unsafe { mmap(std::ptr::null_mut(), len, 3, 0x4022, -1, 0) };
        if p as isize != -1 && !p.is_null() {
            // 16 バイトごとに 1 ビットの「解放済み」の影 (遅延確保)。
            let sh = unsafe { mmap(std::ptr::null_mut(), len / 128 + 4096, 3, 0x4022, -1, 0) };
            if sh as isize == -1 || sh.is_null() {
                unsafe { munmap(p, len) };
                len -= len / 8;
                continue;
            }
            BASE.store(p as usize, Ordering::Relaxed);
            LIMIT.store(len, Ordering::Relaxed);
            SHADOW.store(sh as usize, Ordering::Relaxed);
            return true;
        }
        len -= len / 8;
    }
    false
}

/// 領域を使えるようにする (最初の `Vm` / コンパイルの前に呼ぶ)。失敗したら `false` (そのとき VM は走らせない)。
pub fn ensure() -> bool {
    init()
}

/// 領域の外の番地 (C が返す番地) も通すか。
pub fn set_permissive(on: bool) {
    PERMISSIVE.store(on, Ordering::Relaxed);
}

fn class_of(n: usize) -> usize {
    let n = n.max(16);
    (usize::BITS - (n - 1).leading_zeros()) as usize
}

/// `n` バイト (0 でない) の領域。足りなければ 0。中身は未初期化 (空きの再利用では前の値が残る)。
pub fn alloc(n: usize) -> usize {
    if !init() {
        return 0;
    }
    let c = class_of(n);
    if c >= NCLASS {
        return 0;
    }
    let mut g = ARENA.lock().unwrap_or_else(|e| e.into_inner());
    let a = g.get_or_insert_with(|| Arena { top: 64, quarantine: Default::default(), qbytes: 0, free: std::array::from_fn(|_| Vec::new()) });
    if let Some(p) = a.free[c].pop() {
        return p;
    }
    let size = 1usize << c;
    let start = (a.top + 15) & !15;
    let limit = LIMIT.load(Ordering::Relaxed);
    let Some(end) = start.checked_add(HDR + size) else { return 0 };
    if end > limit {
        return 0;
    }
    a.top = end;
    let base = BASE.load(Ordering::Relaxed);
    unsafe { ((base + start) as *mut u64).write(c as u64) };
    base + start + HDR
}

/// 0 で埋めた領域。
pub fn alloc_zeroed(n: usize) -> usize {
    let p = alloc(n);
    if p != 0 {
        unsafe { std::ptr::write_bytes(p as *mut u8, 0, n.max(1)) };
    }
    p
}

/// `p` の区分 (領域の中の、見出しが正しく読める番地だけ)。
fn class_at(p: usize) -> Option<usize> {
    let base = BASE.load(Ordering::Relaxed);
    let limit = LIMIT.load(Ordering::Relaxed);
    let off = p.wrapping_sub(base);
    if base == 0 || off < 64 + HDR || off >= limit || off % 16 != 0 {
        return None;
    }
    let c = unsafe { ((p - HDR) as *const u64).read() } as usize;
    if c >= NCLASS || c < 4 || off + (1usize << c) > limit {
        return None;
    }
    Some(c)
}

fn shadow_set(p: usize, size: usize, on: bool) {
    let base = BASE.load(Ordering::Relaxed);
    let sh = SHADOW.load(Ordering::Relaxed) as *const std::sync::atomic::AtomicU8;
    let (g0, g1) = ((p - base) >> 4, (p - base + size) >> 4);
    let mut g = g0;
    while g < g1 {
        let b = unsafe { &*sh.add(g >> 3) };
        if g & 7 == 0 && g + 8 <= g1 {
            b.store(if on { 0xff } else { 0 }, Ordering::Relaxed);
            g += 8;
        } else {
            if on { b.fetch_or(1 << (g & 7), Ordering::Relaxed) } else { b.fetch_and(!(1 << (g & 7)), Ordering::Relaxed) };
            g += 1;
        }
    }
}

/// `[a, a + n)` が解放済み (寝かせ中を含む) の領域に触れるか。
#[inline(always)]
pub fn dead(a: u64, n: usize) -> bool {
    if !DEAD_ANY.load(Ordering::Relaxed) || n == 0 {
        return false;
    }
    let base = BASE.load(Ordering::Relaxed) as u64;
    let off = a.wrapping_sub(base);
    if off >= LIMIT.load(Ordering::Relaxed) as u64 {
        return false;
    }
    let sh = SHADOW.load(Ordering::Relaxed) as *const std::sync::atomic::AtomicU8;
    let (g0, g1) = ((off >> 4) as usize, ((off as usize).saturating_add(n - 1)) >> 4);
    let mut g = g0;
    while g <= g1 {
        let b = unsafe { (*sh.add(g >> 3)).load(Ordering::Relaxed) };
        if g & 7 == 0 && g + 7 <= g1 {
            if b != 0 {
                return true;
            }
            g += 8;
        } else {
            if b & (1 << (g & 7)) != 0 {
                return true;
            }
            g += 1;
        }
    }
    false
}

/// 解放。領域の外・見出しが壊れた番地・解放済みの番地は `false` (呼び出し側が panic にする)。
/// 解放した領域は影に印を付け、上限までは再利用せず寝かせる。
pub fn free(p: usize) -> bool {
    let Some(c) = class_at(p) else { return false };
    if dead(p as u64, 1) {
        return false;
    }
    let mut g = ARENA.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(a) = g.as_mut() {
        let size = 1usize << c;
        shadow_set(p, size, true);
        DEAD_ANY.store(true, Ordering::Relaxed);
        a.quarantine.push_back((p, size, c));
        a.qbytes += size;
        while a.qbytes > QUARANTINE_MAX {
            let Some((q, sz, qc)) = a.quarantine.pop_front() else { break };
            a.qbytes -= sz;
            shadow_set(q, sz, false);
            a.free[qc].push(q);
        }
    }
    true
}

/// 再確保。`p` が不正なら `Err`、足りなければ `Ok(0)`。
pub fn realloc(p: usize, n: usize) -> Result<usize, ()> {
    if p == 0 {
        return Ok(alloc(n));
    }
    let Some(c) = class_at(p) else { return Err(()) };
    if dead(p as u64, 1) {
        return Err(());
    }
    if class_of(n) == c {
        return Ok(p);
    }
    let q = alloc(n);
    if q == 0 {
        return Ok(0);
    }
    unsafe { std::ptr::copy_nonoverlapping(p as *const u8, q as *mut u8, (1usize << c).min(n)) };
    free(p);
    Ok(q)
}

/// `[a, a + n)` が領域の内側 (または `permissive` で 0x1000 以上) なら、その生の番地。
#[inline(always)]
pub fn check(a: u64, n: usize) -> Option<*mut u8> {
    let off = a.wrapping_sub(BASE.load(Ordering::Relaxed) as u64);
    let limit = LIMIT.load(Ordering::Relaxed) as u64;
    if off < limit && (n as u64) <= limit - off {
        return Some(a as usize as *mut u8);
    }
    if a >= 0x1000 && PERMISSIVE.load(Ordering::Relaxed) {
        return Some(a as usize as *mut u8);
    }
    None
}

/// 静的な領域 (リテラル・`static`・`global`)。解放しない。
pub fn leak(bytes: &[u8]) -> usize {
    let p = alloc(bytes.len().max(1));
    if p != 0 {
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len()) };
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 偽の番地 (領域の外・NULL に近い・領域の端をまたぐ長さ) は `check` が弾く。`permissive` でなければ C の番地も弾く。
    #[test]
    fn forged_addresses_are_rejected() {
        assert!(ensure());
        set_permissive(false);
        assert!(check(0x4141_4141_4141_4141, 16).is_none());
        assert!(check(0, 1).is_none());
        let p = alloc(64);
        assert!(p != 0);
        assert!(check(p as u64, 64).is_some());
        assert!(check(p as u64, usize::MAX).is_none());
        let base = BASE.load(Ordering::Relaxed) as u64;
        let limit = LIMIT.load(Ordering::Relaxed) as u64;
        assert!(check(base + limit - 8, 16).is_none());
        assert!(!free(0x4141_4141_4141_4140));
        assert!(realloc(0x4141_4141_4141_4140, 8).is_err());
        assert!(free(p));
    }
}
