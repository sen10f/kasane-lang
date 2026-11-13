//! KIR の参照実行器 (設計_KIR.md 5 章 段階 1 の「素朴なインタプリタ」)。
//!
//! 目的は、木走査インタプリタ (`interp.rs`) と出力・終了コード・panic の文言が一致することを確かめる基準を作ること。
//! 速さは求めない。呼び出しは明示的なフレーム列で持ち、Rust の再帰を使わない。深さの上限は木走査と同じ 2500 (終了コード 102)。
//! 浮動小数と 128 ビット整数の規則は `kir_scalar.rs` にある。

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use crate::interp::{py_float, py_float32};
use crate::kir::{payload_bits, payload_of, size_of, AggDef, AggKind, Aop, BoolOp, Field, Func, Mode, Module, Op, Pay, Term, Ty, PAY_MASK, TAG_BIT, VId};
use crate::kir_scalar;
use crate::natives::io_kind_name;

// Option / Result の値の語 (設計_KIR.md 11.1)。タグは bit 64、ペイロードの語は下位 64 ビット。
// 語の意味はペイロードの型で決まる: スカラーは値そのもの、集成体は置き場の添字、入れ子の Option / Result は
// 全体の値 (i128) を入れた 16 バイトの項目の添字 (置き場の項目として持つ)。

/// 型 `t` の値が置き場の項目を参照するか (集成体、または入れ子の Option / Result を含む)。
fn has_heap(t: Ty) -> bool {
    match t {
        Ty::Agg(_) => true,
        Ty::Opt(p) => pay_has_heap(p),
        Ty::Res(a, e) => pay_has_heap(a) || pay_has_heap(e),
        _ => false,
    }
}

/// ペイロードの語が置き場の項目の添字になるか (集成体、または入れ子の Option / Result は項目を持つ)。
fn pay_has_heap(p: Pay) -> bool {
    matches!(p.ty(), Ty::Agg(_) | Ty::Opt(_) | Ty::Res(..))
}

/// ペイロードの語を作る。`v` は型 `p` の値 (`Some(a)` などの被演算子)。入れ子の値は置き場に項目を積む。
fn pay_word(p: Pay, v: i128, heap: &mut Vec<Vec<u8>>) -> i128 {
    match p.ty() {
        Ty::Agg(_) => v,
        Ty::Opt(_) | Ty::Res(..) => {
            heap.push(v.to_le_bytes().to_vec());
            (heap.len() - 1) as i128
        }
        _ => payload_bits(v),
    }
}

/// ペイロードの語 `w` (型 `t` の値) を読み直す。入れ子の項目が無い (タグが別の腕の値を読んだ) ときは 0。
fn pay_read(t: Ty, w: i128, heap: &[Vec<u8>]) -> i128 {
    match t {
        Ty::Agg(_) => w,
        Ty::Opt(_) | Ty::Res(..) => match heap.get(w as usize) {
            Some(b) if b.len() == 16 => i128::from_le_bytes(b[..].try_into().expect("16 バイト")),
            _ => 0,
        },
        _ => payload_of(t, w),
    }
}

/// ホストのソケット (記述子表の値)。
enum NetSock {
    Listener(TcpListener),
    Stream(TcpStream),
}

/// `std.net` の記述子を割り当てる (ソケット表と共に、呼び出しの間だけ生きる)。
fn alloc_next_fd(next: &mut i128) -> i128 {
    let fd = *next;
    *next += 1;
    fd
}

/// 呼び出し先が作った置き場の項目 (添字 `base` 以上) を、呼び出し元に残す形へ写す。`base` より前は呼び出し元のものなので、そのまま使う。
/// `blobs` は添字 `base` から始まる項目、`out` は戻った後の置き場に積む項目。戻り値は新しい添字。
fn relocate(t: Ty, v: i128, blobs: &[Vec<u8>], base: usize, out: &mut Vec<Vec<u8>>) -> i128 {
    match t {
        Ty::Agg(_) => place(v as usize, blobs, base, out),
        Ty::Opt(p) => {
            if v & TAG_BIT == 0 {
                v
            } else {
                TAG_BIT | relocate_word(p.ty(), v & PAY_MASK, blobs, base, out)
            }
        }
        Ty::Res(a, e) => {
            let pt = if v & TAG_BIT != 0 { e.ty() } else { a.ty() };
            (v & TAG_BIT) | relocate_word(pt, v & PAY_MASK, blobs, base, out)
        }
        _ => v,
    }
}

fn relocate_word(t: Ty, w: i128, blobs: &[Vec<u8>], base: usize, out: &mut Vec<Vec<u8>>) -> i128 {
    match t {
        Ty::Agg(_) => place(w as usize, blobs, base, out),
        Ty::Opt(_) | Ty::Res(..) => {
            if (w as usize) < base {
                return w;
            }
            let inner = i128::from_le_bytes(blobs[w as usize - base][..].try_into().expect("16 バイト"));
            let moved = relocate(t, inner, blobs, base, out);
            out.push(moved.to_le_bytes().to_vec());
            (base + out.len() - 1) as i128
        }
        _ => w,
    }
}

fn place(idx: usize, blobs: &[Vec<u8>], base: usize, out: &mut Vec<Vec<u8>>) -> i128 {
    if idx < base {
        return idx as i128;
    }
    out.push(blobs[idx - base].clone());
    (base + out.len() - 1) as i128
}

/// 木走査インタプリタの呼び出し深さの上限 (`interp.rs` の `depth > 2500`)。
const MAX_DEPTH: usize = 2500;

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// 正常終了。値は型に応じた符号化 (整数はその値、`u128` はビットパターン、浮動小数はビット列、bool は 0 / 1、unit は 0、Option / Result はタグ bit とペイロード。`show_value` で読む)。
    Value(i128),
    /// 実行時 panic (文言は木走査と同じ)。
    Panic(String),
    /// 文言を `eprint` で書き終えた panic の終わり (終了コード 101。文言は出さない)。
    Abort,
    /// 呼び出し深さの超過 (木走査と同じく終了コード 102)。
    StackOverflow,
}

struct Frame {
    f: usize,
    blk: usize,
    /// 次に実行する命令の番号。
    ip: usize,
    vals: Vec<i128>,
    /// 値が定義済みか (ブロック引数・命令の結果が書かれたら立つ)。定義が使用を支配しない KIR を、未定義の読みとして捕まえる (設計_KIR.md 17.9)。
    defd: Vec<bool>,
    /// 呼び出し元で結果を受ける値番号。
    dst: Option<VId>,
    /// この呼び出しが始まったときの集成体の置き場の長さ (戻るときにここまで切り詰める)。
    hbase: usize,
    /// この関数の `frame.bytes` で確保した領域の先頭。関数が戻るときに解放する。
    allocs: Vec<i128>,
    /// この呼び出しが始まったときのバイト記憶の大きさ。これ以降に確保された領域が、この呼び出しの確保 (漏れの検査の対象)。
    mem_base: usize,
}

/// 未初期化の領域のバイト (読むと内部エラー。`check_leaks` の語の走査がアドレスと取り違えない値)。
const POISON: u8 = 0xA5;

/// アドレスの基準値 (0 は null として空けておく)。
const BASE: i128 = 0x1000;

/// 仮想のバイト記憶: `ptr` が指す領域 (ヒープ・フレームの領域・静的な領域)。
/// `clif` と同じく {ptr, len, cap} の実メモリを、バイト列の上で表す。集成体の置き場 (`heap`) とは別。
/// 解放済みの領域は再利用しない (誤用の検出を優先する)。生存する領域は `live` で、静的な領域は `pinned` で区別する。
struct ByteMem {
    mem: Vec<u8>,
    /// `mem` の各バイトが書かれたか。`heap.alloc` / `heap.realloc` の新しい領域は未初期化 (毒の値 `POISON` を入れ、読むと内部エラー)。
    /// `frame.bytes` / `alloca` は機械語が 0 で埋めるので、書かれた扱い (0)。
    init: Vec<bool>,
    /// 先頭アドレス → 領域。
    blocks: BTreeMap<i128, Blk>,
}

struct Blk {
    off: usize,
    size: usize,
    live: bool,
    pinned: bool,
}

impl ByteMem {
    fn new() -> ByteMem {
        ByteMem { mem: vec![], init: vec![], blocks: BTreeMap::new() }
    }

    /// 新しい領域 (`malloc` と同じく未初期化。読むまでに書かれていなければ内部エラー)。
    fn alloc(&mut self, size: usize, pinned: bool) -> i128 {
        let off = self.mem.len();
        self.mem.resize(off + size.max(1), POISON);
        self.init.resize(off + size.max(1), false);
        let addr = BASE + off as i128;
        self.blocks.insert(addr, Blk { off, size, live: true, pinned });
        addr
    }

    /// 0 で埋めた新しい領域 (`frame.bytes` / `alloca`。機械語は先頭で 0 埋めする)。
    fn alloc_zeroed(&mut self, size: usize) -> i128 {
        let a = self.alloc(size, false);
        let at = self.locate(a, 0).expect("確保したばかり");
        for k in at..at + size.max(1) {
            self.mem[k] = 0;
            self.init[k] = true;
        }
        a
    }

    /// `addr` から `n` バイトが生存する領域の内側にあるとき、`mem` 上の位置を返す。
    fn locate(&self, addr: i128, n: usize) -> Result<usize, String> {
        let bad = || format!("不正なメモリアクセス {:#x} ({} バイト)", addr, n);
        let (&start, b) = self.blocks.range(..=addr).next_back().ok_or_else(bad)?;
        if !b.live || addr + n as i128 > start + b.size as i128 {
            return Err(bad());
        }
        Ok(b.off + (addr - start) as usize)
    }

    fn read(&self, addr: i128, ty: Ty) -> Result<i128, String> {
        let n = width(ty);
        let at = self.locate(addr, n)?;
        if self.init[at..at + n].iter().any(|b| !b) {
            return Err(format!("未初期化の領域を読んだ {:#x} ({} バイト)", addr, n));
        }
        let mut buf = [0u8; 16];
        buf[..n].copy_from_slice(&self.mem[at..at + n]);
        let u = u128::from_le_bytes(buf) as i128;
        Ok(match ty {
            Ty::Bool => (u != 0) as i128,
            Ty::Ptr => u,
            _ => ty.wrap(u),
        })
    }

    fn write(&mut self, addr: i128, ty: Ty, v: i128) -> Result<(), String> {
        let n = width(ty);
        let at = self.locate(addr, n)?;
        let bytes = (v as u128).to_le_bytes();
        self.mem[at..at + n].copy_from_slice(&bytes[..n]);
        self.init[at..at + n].iter_mut().for_each(|b| *b = true);
        Ok(())
    }

    /// `addr` から `n` バイトをそのまま読む (集成体の読み出し用)。
    fn read_bytes(&self, addr: i128, n: usize) -> Result<Vec<u8>, String> {
        let at = self.locate(addr, n)?;
        Ok(self.mem[at..at + n].to_vec())
    }

    fn write_bytes(&mut self, addr: i128, bytes: &[u8]) -> Result<(), String> {
        let at = self.locate(addr, bytes.len())?;
        self.mem[at..at + bytes.len()].copy_from_slice(bytes);
        self.init[at..at + bytes.len()].iter_mut().for_each(|b| *b = true);
        Ok(())
    }

    /// 解放 (`null` は何もしない)。二重解放や静的領域の解放は誤用として `Err` にする。
    fn free(&mut self, addr: i128) -> Result<(), String> {
        if addr == 0 {
            return Ok(());
        }
        match self.blocks.get_mut(&addr) {
            Some(b) if b.live && !b.pinned => {
                b.live = false;
                Ok(())
            }
            _ => Err(format!("不正な解放 {:#x}", addr)),
        }
    }

    /// `old` の内容を先頭から複写した、新しい大きさ `size` の領域を作って旧い領域を解放する (`old` が 0 なら確保だけ)。
    fn realloc(&mut self, old: i128, size: usize) -> Result<i128, String> {
        if old == 0 {
            return Ok(self.alloc(size, false));
        }
        let old_size = match self.blocks.get(&old) {
            Some(b) if b.live && !b.pinned => b.size,
            _ => return Err(format!("不正な再確保 {:#x}", old)),
        };
        let n = old_size.min(size);
        let src = self.locate(old, n)?;
        let bytes: Vec<u8> = self.mem[src..src + n].to_vec();
        let flags: Vec<bool> = self.init[src..src + n].to_vec();
        let new = self.alloc(size, false);
        // 旧い領域の書かれたバイトだけを引き継ぐ (増えた部分は未初期化のまま)。
        let dst = self.locate(new, n)?;
        self.mem[dst..dst + n].copy_from_slice(&bytes);
        self.init[dst..dst + n].copy_from_slice(&flags);
        self.free(old)?;
        Ok(new)
    }

    /// フレームの終わりの解放 (誤用の検査は行わない: 戻り値に含まれない領域だけが対象)。
    fn release(&mut self, addr: i128) {
        if let Some(b) = self.blocks.get_mut(&addr) {
            b.live = false;
        }
    }

    /// 関数の終わりの漏れの検査: この呼び出し (`base` 以降の確保) が作った領域のうち、まだ生きていて、
    /// 戻り値・呼び出し前から生きている領域のどちらからも指されていないものがあれば `Err`。
    /// 指しているかは、8 バイト境界の語が領域の先頭アドレスと等しいかで見る (戻り値は `roots` の語と `blobs` の中身)。
    fn check_leaks(&self, base: usize, roots: &[i128], blobs: &[Vec<u8>]) -> Result<(), String> {
        let cand: std::collections::BTreeSet<i128> =
            self.blocks.iter().filter(|(_, b)| b.live && !b.pinned && b.off >= base).map(|(a, _)| *a).collect();
        if cand.is_empty() {
            return Ok(());
        }
        let mut reached: std::collections::BTreeSet<i128> = std::collections::BTreeSet::new();
        let mut work: Vec<i128> = vec![];
        let see = |w: i128, reached: &mut std::collections::BTreeSet<i128>, work: &mut Vec<i128>| {
            if cand.contains(&w) && reached.insert(w) {
                work.push(w);
            }
        };
        for r in roots {
            see(*r, &mut reached, &mut work);
        }
        for blob in blobs {
            for ch in blob.chunks_exact(8) {
                see(u64::from_le_bytes(ch.try_into().expect("8 バイト")) as i128, &mut reached, &mut work);
            }
        }
        // 呼び出し前から生きている領域 (呼び出し元のヘッダ・バッファ) の中の語。
        for b in self.blocks.values().filter(|b| b.live && !b.pinned && b.off < base) {
            for ch in self.mem[b.off..b.off + b.size].chunks_exact(8) {
                see(u64::from_le_bytes(ch.try_into().expect("8 バイト")) as i128, &mut reached, &mut work);
            }
        }
        while let Some(a) = work.pop() {
            let b = &self.blocks[&a];
            for ch in self.mem[b.off..b.off + b.size].chunks_exact(8) {
                see(u64::from_le_bytes(ch.try_into().expect("8 バイト")) as i128, &mut reached, &mut work);
            }
        }
        let leaked = cand.len() - reached.len();
        if leaked > 0 {
            return Err(format!("{} 個の領域が解放されず、戻り値からも指されていない", leaked));
        }
        Ok(())
    }

    /// 生存する (解放されていない、静的でない) 領域の数。
    fn leaks(&self) -> usize {
        self.blocks.values().filter(|b| b.live && !b.pinned).count()
    }
}

/// バイト記憶の 1 語の幅 (バイト)。`unit` は持たない (verify が拒む)。
fn width(ty: Ty) -> usize {
    match ty {
        Ty::Bool => 1,
        Ty::Ptr => 8,
        t => (t.bits() / 8) as usize,
    }
}

/// 集成体の値の読み出し: `t` の値を、バイト列 `buf` の `off` から取る (集成体は置き場に複製して、その添字を返す)。
fn read_scalar(buf: &[u8], off: usize, t: Ty) -> i128 {
    match t {
        Ty::Bool => (buf[off] != 0) as i128,
        Ty::Unit => 0,
        _ => {
            let n = size_of(&[], t) as usize;
            let mut raw = [0u8; 16];
            raw[..n].copy_from_slice(&buf[off..off + n]);
            t.wrap(u128::from_le_bytes(raw) as i128)
        }
    }
}

pub(crate) const DIV_ZERO: &str = "attempt to divide by zero";
pub(crate) const REM_ZERO: &str = "attempt to calculate the remainder with a divisor of by zero";

/// `u128` の算術 (値はビットパターン)。
fn arith_u128(op: Aop, mode: Mode, x: u128, y: u128) -> Result<u128, String> {
    match mode {
        Mode::Wrap => Ok(match op {
            Aop::Add => x.wrapping_add(y),
            Aop::Sub => x.wrapping_sub(y),
            _ => x.wrapping_mul(y),
        }),
        Mode::Sat => Ok(match op {
            Aop::Add => x.saturating_add(y),
            Aop::Sub => x.saturating_sub(y),
            _ => x.saturating_mul(y),
        }),
        Mode::Trap => {
            if matches!(op, Aop::Div | Aop::Rem) && y == 0 {
                return Err(if op == Aop::Div { DIV_ZERO.to_string() } else { REM_ZERO.to_string() });
            }
            let r = match op {
                Aop::Add => x.checked_add(y),
                Aop::Sub => x.checked_sub(y),
                Aop::Mul => x.checked_mul(y),
                Aop::Div => x.checked_div(y),
                Aop::Rem => x.checked_rem(y),
            };
            r.ok_or_else(|| overflow_msg(op))
        }
    }
}

/// 値 `v` (スカラー、または置き場の添字の集成体) を、バイト列 `buf` の `off` に書く。
fn write_val(buf: &mut [u8], off: usize, aggs: &[AggDef], t: Ty, v: i128, heap: &[Vec<u8>]) {
    if let Ty::Agg(_) = t {
        let src = &heap[v as usize];
        buf[off..off + src.len()].copy_from_slice(src);
    } else {
        let n = size_of(aggs, t) as usize;
        let bytes = (v as u128).to_le_bytes();
        buf[off..off + n].copy_from_slice(&bytes[..n]);
    }
}

/// `write` の実体: プロセスの stdout (1) / stderr (2) へ全バイトを書く。stdout は書くたびに流す (panic や終了の前に順序が崩れない)。
fn write_fd(fd: i128, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    match fd {
        1 => {
            let mut o = std::io::stdout().lock();
            o.write_all(bytes)?;
            o.flush()
        }
        2 => std::io::stderr().lock().write_all(bytes),
        _ => Err(std::io::Error::from_raw_os_error(9)),
    }
}

/// `std.io.Error { kind, code }` の値を置き場に作り、その添字を返す (型表の `std.io.Error` と、その `kind` の列挙型を探す)。
fn io_error_value(aggs: &[AggDef], heap: &mut Vec<Vec<u8>>, kind: &str, code: i32) -> Result<i128, String> {
    let d = aggs.iter().find(|d| d.name == "std.io.Error").ok_or("型表に std.io.Error がない")?;
    let AggKind::Struct(fs) = &d.kind else { return Err("std.io.Error が構造体でない".into()) };
    let (Some(kf), Some(cf)) = (fs.iter().find(|f| f.name == "kind"), fs.iter().find(|f| f.name == "code")) else {
        return Err("std.io.Error に kind と code がない".into());
    };
    let Ty::Agg(ki) = kf.ty else { return Err("std.io.Error.kind が列挙型でない".into()) };
    let kd = &aggs[ki as usize];
    let AggKind::Enum(vs) = &kd.kind else { return Err("std.io.ErrorKind が列挙型でない".into()) };
    let vi = vs.iter().position(|v| v.name == kind).ok_or_else(|| format!("ErrorKind に {} がない", kind))?;
    let mut kb = vec![0u8; kd.size as usize];
    kb[0..4].copy_from_slice(&(vi as i32).to_le_bytes());
    let mut buf = vec![0u8; d.size as usize];
    buf[kf.off as usize..kf.off as usize + kb.len()].copy_from_slice(&kb);
    buf[cf.off as usize..cf.off as usize + 4].copy_from_slice(&code.to_le_bytes());
    heap.push(buf);
    Ok((heap.len() - 1) as i128)
}

/// `{ descriptor: i32 }` のソケット集成体を置き場に作り、その添字を返す。
fn desc_agg(heap: &mut Vec<Vec<u8>>, fd: i128) -> Result<i128, String> {
    let mut buf = vec![0u8; 4];
    buf[0..4].copy_from_slice(&(fd as i32).to_le_bytes());
    heap.push(buf);
    Ok((heap.len() - 1) as i128)
}

/// 構造体・タプルのオフセット `off` のフィールドの型。
fn field_ty_at(d: &AggDef, off: u32) -> Option<Ty> {
    match &d.kind {
        AggKind::Struct(fs) | AggKind::Tuple(fs) => fs.iter().find(|f: &&Field| f.off == off).map(|f| f.ty),
        AggKind::Enum(_) => None,
    }
}

pub(crate) fn overflow_msg(op: Aop) -> String {
    match op {
        Aop::Add => "attempt to add with overflow",
        Aop::Sub => "attempt to subtract with overflow",
        Aop::Mul => "attempt to multiply with overflow",
        Aop::Div => "attempt to divide with overflow",
        Aop::Rem => "attempt to calculate the remainder with overflow",
    }
    .to_string()
}

/// 整数の算術。結果は型の範囲内に収まる値か、panic の文言。
fn arith(op: Aop, mode: Mode, ty: Ty, x: i128, y: i128) -> Result<i128, String> {
    if ty == Ty::U128 {
        return arith_u128(op, mode, x as u128, y as u128).map(|v| v as i128);
    }
    if mode == Mode::Wrap {
        let r = match op {
            Aop::Add => x.wrapping_add(y),
            Aop::Sub => x.wrapping_sub(y),
            _ => x.wrapping_mul(y),
        };
        return Ok(ty.wrap(r));
    }
    if mode == Mode::Sat {
        // 16 ビット以下と i128 の両方を扱う。i128 の溢れは結果の符号で飽和する方向を決める (和と差は x の符号、積は符号の積)。
        let r = match op {
            Aop::Add => x.checked_add(y),
            Aop::Sub => x.checked_sub(y),
            _ => x.checked_mul(y),
        };
        let neg = match op {
            Aop::Add | Aop::Sub => x < 0,
            _ => (x < 0) != (y < 0),
        };
        return Ok(match r {
            Some(v) => v.clamp(ty.min(), ty.max()),
            None => {
                if neg {
                    ty.min()
                } else {
                    ty.max()
                }
            }
        });
    }
    if matches!(op, Aop::Div | Aop::Rem) && y == 0 {
        // 文言は木走査 (interp.rs の `int_arith`) と同じにする。剰余の文言は木走査の誤植 (`of by zero`) を写している。
        return Err(if op == Aop::Div { DIV_ZERO.to_string() } else { REM_ZERO.to_string() });
    }
    let r = match op {
        Aop::Add => x.checked_add(y),
        Aop::Sub => x.checked_sub(y),
        Aop::Mul => x.checked_mul(y),
        Aop::Div => x.checked_div(y),
        Aop::Rem => checked_rem(ty, x, y),
    };
    match r {
        Some(v) if ty.in_range(v) => Ok(v),
        _ => Err(overflow_msg(op)),
    }
}

/// 整数の `checked_rem` (案 A)。符号付きの `MIN % -1` は割り算が溢れるので余りも `None`。
/// i128 の `checked_rem` はこの組を溢れとして返さないので、型の最小値で別に判定する。
fn checked_rem(ty: Ty, x: i128, y: i128) -> Option<i128> {
    if ty.signed() && y == -1 && x == ty.min() {
        return None;
    }
    x.checked_rem(y)
}

/// `checked_<op>` の結果 (`Option<P>` の符号化)。溢れと 0 除算は `None` (= 0)。
/// 符号付きの `MIN / -1` と `MIN % -1` は溢れで `None` (案 A。木走査 `interp.rs` と同じ)。
fn checked(op: Aop, t: crate::kir::Ty, x: i128, y: i128) -> i128 {
    let crate::kir::Ty::Opt(p) = t else { return 0 };
    let pt = p.ty();
    if matches!(op, Aop::Div | Aop::Rem) && y == 0 {
        return 0;
    }
    let r = match op {
        Aop::Add => x.checked_add(y),
        Aop::Sub => x.checked_sub(y),
        Aop::Mul => x.checked_mul(y),
        Aop::Div => x.checked_div(y),
        Aop::Rem => checked_rem(pt, x, y),
    };
    match r {
        Some(v) if v >= pt.min() && v <= pt.max() => TAG_BIT | payload_bits(v),
        _ => 0,
    }
}

/// `extern` 宣言の C 関数をホストの呼び出し規約で呼ぶ (`ffi::call_generic` と同じ仕組み)。
/// 引数・戻り値はスカラー・ポインタに限る。
fn extern_call(func: &Func, argv: &[i128]) -> Result<i128, String> {
    use crate::ffi::{self, ArgV, RetTy};
    let link = func.link.as_ref().expect("extern 関数に link がない");
    let (lib, sym) = crate::ffi::split_link(link);
    let fnptr = ffi::lookup(lib, sym).map_err(|e| format!("extern {}: {}", func.fqn, e))?;
    let mut abi: Vec<ArgV> = vec![];
    for (v, (_, pt)) in argv.iter().zip(func.params().iter()) {
        abi.push(match pt {
            Ty::F32 => ArgV::F32(f32::from_bits(*v as u32)),
            Ty::F64 => ArgV::F64(f64::from_bits(*v as u64)),
            _ => ArgV::Int(*v as u64),
        });
    }
    let rty = match func.ret {
        Ty::F32 => RetTy::F32,
        Ty::F64 => RetTy::F64,
        _ => RetTy::Int,
    };
    let r = ffi::call_generic(fnptr, &abi, &rty).map_err(|e| format!("extern {}: {}", func.fqn, e))?;
    Ok(match r {
        ffi::RetV::Int(x) => x as i128,
        ffi::RetV::F32(f) => f.to_bits() as i128,
        ffi::RetV::F64(f) => f.to_bits() as i128,
        ffi::RetV::None => 0,
        ffi::RetV::Agg(_) => return Err(format!("extern {}: 集成体の戻り値は未対応", func.fqn)),
    })
}

/// 鍵 `key` の関数を引数 `args` で実行する。鍵が無い・引数の個数が違う場合は `Err`。
pub fn run(m: &Module, key: &str, args: &[i128]) -> Result<Outcome, String> {
    let index: HashMap<&str, usize> = m.funcs.iter().enumerate().map(|(i, f)| (f.key.as_str(), i)).collect();
    let Some(&entry) = index.get(key) else {
        return Err(format!("関数 {} がない", key));
    };
    if args.len() != m.funcs[entry].params().len() {
        return Err(format!("関数 {} の引数の個数が違う", key));
    }
    // エントリ自体が `extern` なら、ホストの C 関数を直接呼ぶ。
    if m.funcs[entry].link.is_some() {
        let result = extern_call(&m.funcs[entry], args)?;
        return Ok(Outcome::Value(result));
    }
    let counts: Vec<usize> = m.funcs.iter().map(|f| f.value_count()).collect();
    // 関数ごとの値番号 → 型 (`store` が書く値の型を知るため)。
    let vtys: Vec<Vec<Ty>> = m
        .funcs
        .iter()
        .zip(&counts)
        .map(|(f, n)| {
            let mut t = vec![Ty::Unit; *n];
            for b in &f.blocks {
                for (v, ty) in &b.params {
                    t[*v as usize] = *ty;
                }
                for i in &b.insts {
                    t[i.dst as usize] = i.ty;
                }
            }
            t
        })
        .collect();
    // 集成体の置き場: 集成体の値は、ここにバイト列として置き、値 (i128) はその添字で表す。値は不変。
    let mut heap: Vec<Vec<u8>> = vec![];
    if has_heap(m.funcs[entry].ret) || m.funcs[entry].params().iter().any(|(_, t)| has_heap(*t)) {
        return Err(format!("関数 {} は集成体を引数・戻り値に持つ (この入口では扱えない)", key));
    }
    let enter = |f: usize, args: &[i128], dst: Option<VId>, hbase: usize, mem_base: usize| -> Frame {
        let mut vals = vec![0i128; counts[f]];
        let mut defd = vec![false; counts[f]];
        for ((v, _), a) in m.funcs[f].params().iter().zip(args.iter()) {
            vals[*v as usize] = *a;
            defd[*v as usize] = true;
        }
        Frame { f, blk: 0, ip: 0, vals, defd, dst, hbase, allocs: vec![], mem_base }
    };
    // バイト記憶 (`ptr` の領域)。静的な領域 (`static.bytes`) は、命令の位置ごとに 1 回だけ確保する。
    let mut bm = ByteMem::new();
    let mut statics: HashMap<(usize, usize, usize), i128> = HashMap::new();
    // ソケット表 (記述子 -> ホストのソケット)。リスナとストリームは同じ番号空間を使う。
    let mut sockets: HashMap<i128, NetSock> = HashMap::new();
    let mut next_fd: i128 = 0;
    let mut stack = vec![enter(entry, args, None, 0, 0)];
    loop {
        let top = stack.len() - 1;
        let (fi, bi, ip) = (stack[top].f, stack[top].blk, stack[top].ip);
        let func = &m.funcs[fi];
        let blk = &func.blocks[bi];
        if ip < blk.insts.len() {
            stack[top].ip += 1;
            let inst = &blk.insts[ip];
            if let Some(v) = crate::kir::operands(&inst.op).into_iter().find(|v| !stack[top].defd[*v as usize]) {
                return Err(format!("内部エラー: 関数 {} の %{} が定義される前に読まれた (定義が使用を支配しない)", func.key, v));
            }
            let get = |fr: &Frame, v: &VId| fr.vals[*v as usize];
            let result: i128 = match &inst.op {
                Op::Const(v) => *v,
                Op::FrameBytes { size } => {
                    let a = bm.alloc_zeroed(*size as usize);
                    stack[top].allocs.push(a);
                    a
                }
                Op::StaticBytes(bytes) => match statics.get(&(fi, bi, ip)) {
                    Some(a) => *a,
                    None => {
                        let a = bm.alloc(bytes.len(), true);
                        bm.write_bytes(a, bytes)?;
                        statics.insert((fi, bi, ip), a);
                        a
                    }
                },
                Op::PtrAdd { p, off } => get(&stack[top], p) + get(&stack[top], off),
                Op::Utf8Valid { p, len } => {
                    let n = get(&stack[top], len) as usize;
                    let bytes = if n == 0 { vec![] } else { bm.read_bytes(get(&stack[top], p), n)? };
                    match std::str::from_utf8(&bytes) {
                        Ok(_) => n as i128,
                        Err(e) => e.valid_up_to() as i128,
                    }
                }
                Op::MemLoad { p, off } => bm.read(get(&stack[top], p) + *off as i128, inst.ty)?,
                Op::MemStore { ty, p, off, v } => {
                    let (a, x) = (get(&stack[top], p) + *off as i128, get(&stack[top], v));
                    bm.write(a, *ty, x)?;
                    0
                }
                Op::HeapAlloc(n) => bm.alloc(get(&stack[top], n) as usize, false),
                Op::HeapRealloc { p, size } => bm.realloc(get(&stack[top], p), get(&stack[top], size) as usize)?,
                Op::HeapFree(p) => {
                    bm.free(get(&stack[top], p))?;
                    0
                }
                Op::Write { fd, data } => {
                    let h = get(&stack[top], data) as usize;
                    let ptr = i64::from_le_bytes(heap[h][0..8].try_into().expect("fat の ptr")) as i128;
                    let len = u64::from_le_bytes(heap[h][8..16].try_into().expect("fat の len")) as usize;
                    let bytes = if len == 0 { vec![] } else { bm.read_bytes(ptr, len)? };
                    match write_fd(get(&stack[top], fd), &bytes) {
                        Ok(()) => 0,
                        Err(e) => {
                            let w = io_error_value(&m.aggs, &mut heap, crate::natives::io_kind_name(&e), e.raw_os_error().unwrap_or(0))?;
                            TAG_BIT | w
                        }
                    }
                }
                Op::NetListen { addr } | Op::NetConnect { addr } => {
                    let h = get(&stack[top], addr) as usize;
                    let ptr = i64::from_le_bytes(heap[h][0..8].try_into().expect("fat の ptr")) as i128;
                    let len = u64::from_le_bytes(heap[h][8..16].try_into().expect("fat の len")) as usize;
                    let bytes = if len == 0 { vec![] } else { bm.read_bytes(ptr, len)? };
                    let s = std::str::from_utf8(&bytes).unwrap_or("");
                    let is_listen = matches!(inst.op, Op::NetListen { .. });
                    let res: i128 = match if is_listen {
                        TcpListener::bind(s).map(NetSock::Listener)
                    } else {
                        TcpStream::connect(s).map(NetSock::Stream)
                    } {
                        Ok(sock) => {
                            let fd = alloc_next_fd(&mut next_fd);
                            sockets.insert(fd, sock);
                            desc_agg(&mut heap, fd)?
                        }
                        Err(e) => TAG_BIT | io_error_value(&m.aggs, &mut heap, io_kind_name(&e), e.raw_os_error().unwrap_or(0))?,
                    };
                    res
                }
                Op::NetAccept { sock } => {
                    let fd = get(&stack[top], sock);
                    let Some(NetSock::Listener(l)) = sockets.get_mut(&fd) else {
                        return Err(format!("無効なソケット記述子 {}", fd));
                    };
                    let res: i128 = match l.accept() {
                        Ok((st, _)) => {
                            let nfd = alloc_next_fd(&mut next_fd);
                            sockets.insert(nfd, NetSock::Stream(st));
                            desc_agg(&mut heap, nfd)?
                        }
                        Err(e) => TAG_BIT | io_error_value(&m.aggs, &mut heap, io_kind_name(&e), e.raw_os_error().unwrap_or(0))?,
                    };
                    res
                }
                Op::NetRead { sock, buf, len } => {
                    let (fd, p, n) = (
                        get(&stack[top], sock),
                        get(&stack[top], buf),
                        get(&stack[top], len),
                    );
                    let Some(NetSock::Stream(s)) = sockets.get_mut(&fd) else {
                        return Err(format!("読み出しできるのはストリームソケットだけ: {}", fd));
                    };
                    let mut vec = vec![0u8; n as usize];
                    let nr = s.read(&mut vec).map_err(|e| format!("read: {e}"))?;
                    bm.write_bytes(p, &vec[..nr])?;
                    nr as i128
                }
                Op::NetWrite { sock, buf, len } => {
                    let (fd, p, n) = (
                        get(&stack[top], sock),
                        get(&stack[top], buf),
                        get(&stack[top], len),
                    );
                    let Some(NetSock::Stream(s)) = sockets.get_mut(&fd) else {
                        return Err(format!("書き込みできるのはストリームソケットだけ: {}", fd));
                    };
                    let bytes = bm.read_bytes(p, n as usize)?;
                    s.write_all(&bytes).map_err(|e| format!("write: {e}"))?;
                    n
                }
                Op::NetFlush { sock } => {
                    let fd = get(&stack[top], sock);
                    let Some(NetSock::Stream(s)) = sockets.get_mut(&fd) else {
                        return Err(format!("flush できるのはストリームソケットだけ: {}", fd));
                    };
                    s.flush().map_err(|e| format!("flush: {e}"))?;
                    0
                }
                Op::NetClose { sock } => {
                    let fd = get(&stack[top], sock);
                    sockets.remove(&fd);
                    0
                }
                Op::NetLocalPort { sock } => {
                    let fd = get(&stack[top], sock);
                    let Some(NetSock::Listener(l)) = sockets.get(&fd) else {
                        return Err(format!("local_port できるのはリスナーソケットだけ: {}", fd));
                    };
                    l.local_addr().map(|a| a.port() as i128).map_err(|e| format!("local_port: {e}"))?
                }
                Op::IndexCheck { i, len } => {
                    let (ix, n) = (get(&stack[top], i), get(&stack[top], len));
                    if ix >= n {
                        return Ok(Outcome::Panic(format!("index out of bounds: the len is {} but the index is {}", n, ix)));
                    }
                    0
                }
                Op::Eprint { data } => {
                    let h = get(&stack[top], data) as usize;
                    let ptr = i64::from_le_bytes(heap[h][0..8].try_into().expect("fat の ptr")) as i128;
                    let len = u64::from_le_bytes(heap[h][8..16].try_into().expect("fat の len")) as usize;
                    let bytes = if len == 0 { vec![] } else { bm.read_bytes(ptr, len)? };
                    // panic の文言は書けなくても終わる (木走査の panic と同じく、書き込みの失敗は捨てる)
                    let _ = write_fd(2, &bytes);
                    0
                }
                Op::FmtFloat { bits, a } => {
                    let x = get(&stack[top], a);
                    let s = if *bits == 32 {
                        py_float32(f32::from_bits(x as u32))
                    } else {
                        py_float(f64::from_bits(x as u64))
                    };
                    let _ = write_fd(2, s.as_bytes());
                    0
                }
                Op::Abort => return Ok(Outcome::Abort),
                Op::Panic { msg, arg } => {
                    let m = match arg {
                        Some(v) => format!("{}: {}", msg, get(&stack[top], v)),
                        None => msg.clone(),
                    };
                    return Ok(Outcome::Panic(m));
                }
                Op::Arith { op, mode, a, b } => {
                    let (x, y) = (get(&stack[top], a), get(&stack[top], b));
                    match arith(*op, *mode, inst.ty, x, y) {
                        Ok(v) => v,
                        Err(msg) => return Ok(Outcome::Panic(msg)),
                    }
                }
                Op::FArith { op, a, b } => kir_scalar::farith(*op, inst.ty, get(&stack[top], a), get(&stack[top], b)),
                Op::Cmp { cc, ty, a, b } => kir_scalar::cmp(*cc, *ty, get(&stack[top], a), get(&stack[top], b)) as i128,
                Op::Shift { left, a, n } => kir_scalar::shift(*left, inst.ty, get(&stack[top], a), get(&stack[top], n)),
                Op::MinMax { max, a, b } => kir_scalar::minmax(*max, inst.ty, get(&stack[top], a), get(&stack[top], b)),
                Op::FUn { f, src, a } => kir_scalar::funary(*f, *src, inst.ty, get(&stack[top], a)),
                Op::FBin { f, a, b } => kir_scalar::fbin(*f, inst.ty, get(&stack[top], a), get(&stack[top], b)),
                Op::Conv { src, a } => kir_scalar::conv(*src, inst.ty, get(&stack[top], a)),
                Op::Bitcast { a, .. } => get(&stack[top], a),
                Op::BitCount { k, src, a } => kir_scalar::bitcount(*k, *src, get(&stack[top], a)),
                Op::Neg { a, mode } => {
                    let x = get(&stack[top], a);
                    match mode {
                        Mode::Wrap => kir_scalar::wrap_neg(inst.ty, x),
                        _ => {
                            if x == inst.ty.min() {
                                return Ok(Outcome::Panic("attempt to negate with overflow".to_string()));
                            }
                            -x
                        }
                    }
                }
                Op::Not(a) => {
                    let x = get(&stack[top], a);
                    if inst.ty == Ty::Bool {
                        (x == 0) as i128
                    } else {
                        kir_scalar::not_int(inst.ty, x)
                    }
                }
                Op::Bool(bop, a, b) => {
                    // bool は 0 / 1 なので、ビット演算がそのまま論理演算になる。整数は符号拡張した 1 語なので、型の範囲の値を保つ。
                    let (x, y) = (get(&stack[top], a), get(&stack[top], b));
                    match bop {
                        BoolOp::And => x & y,
                        BoolOp::Or => x | y,
                        BoolOp::Xor => x ^ y,
                    }
                }
                Op::Some(a) => {
                    let Ty::Opt(p) = inst.ty else {
                        return Err("some の結果は Option のはず".to_string());
                    };
                    TAG_BIT | pay_word(p, get(&stack[top], a), &mut heap)
                }
                Op::Ok(a) => {
                    let Ty::Res(t0, _) = inst.ty else {
                        return Err("ok の結果は Result のはず".to_string());
                    };
                    pay_word(t0, get(&stack[top], a), &mut heap)
                }
                Op::Err(a) => {
                    let Ty::Res(_, e0) = inst.ty else {
                        return Err("err の結果は Result のはず".to_string());
                    };
                    TAG_BIT | pay_word(e0, get(&stack[top], a), &mut heap)
                }
                Op::Payload(a) => pay_read(inst.ty, get(&stack[top], a) & PAY_MASK, &heap),
                Op::Checked { op, a, b } => checked(*op, inst.ty, get(&stack[top], a), get(&stack[top], b)),
                Op::Call { callee, args } => {
                    if stack.len() >= MAX_DEPTH {
                        return Ok(Outcome::StackOverflow);
                    }
                    let Some(&ci) = index.get(callee.as_str()) else {
                        return Err(format!("呼び先 {} がない", callee));
                    };
                    let func = &m.funcs[ci];
                    if func.link.is_some() {
                        let argv: Vec<i128> = args.iter().map(|a| get(&stack[top], a)).collect();
                        let result = extern_call(func, &argv)?;
                        stack[top].vals[inst.dst as usize] = result;
                        stack[top].defd[inst.dst as usize] = true;
                        continue;
                    }
                    let argv: Vec<i128> = args.iter().map(|a| get(&stack[top], a)).collect();
                    stack.push(enter(ci, &argv, Some(inst.dst), heap.len(), bm.mem.len()));
                    continue;
                }
                Op::Make { variant, fields } => {
                    let Ty::Agg(ai) = inst.ty else {
                        return Err("make の結果が集成体でない".to_string());
                    };
                    let d = &m.aggs[ai as usize];
                    let mut buf = vec![0u8; d.size as usize];
                    let fdefs: &[Field] = match (&d.kind, variant) {
                        (AggKind::Enum(vs), Some(k)) => {
                            buf[0..4].copy_from_slice(&(*k as i32).to_le_bytes());
                            &vs[*k as usize].fields
                        }
                        (AggKind::Struct(fs) | AggKind::Tuple(fs), None) => fs,
                        _ => return Err("make の変種が集成体の種類と合わない".to_string()),
                    };
                    for (fd, a) in fdefs.iter().zip(fields) {
                        let v = get(&stack[top], a);
                        write_val(&mut buf, fd.off as usize, &m.aggs, fd.ty, v, &heap);
                    }
                    heap.push(buf);
                    (heap.len() - 1) as i128
                }
                Op::Field { a, off } => {
                    let h = get(&stack[top], a) as usize;
                    let o = *off as usize;
                    // タグが別の腕の値を、判定の前に読むことがある (`Option<Vec>` の `None` のペイロード)。
                    // 項目が無い・短いときは 0 のバイト列として読む (判定の後で使われることはない)。
                    let zero = vec![0u8; 16];
                    let blob: &Vec<u8> = match heap.get(h) {
                        Some(b) if b.len() >= o + size_of(&m.aggs, inst.ty) as usize => b,
                        _ => &zero,
                    };
                    let o = if std::ptr::eq(blob, &zero) { 0 } else { o };
                    match inst.ty {
                        Ty::Agg(ai) => {
                            let n = m.aggs[ai as usize].size as usize;
                            let sub = if std::ptr::eq(blob, &zero) { vec![0u8; n] } else { blob[o..o + n].to_vec() };
                            heap.push(sub);
                            (heap.len() - 1) as i128
                        }
                        t => read_scalar(blob, o, t),
                    }
                }
                Op::SetField { a, off, v } => {
                    let h = get(&stack[top], a) as usize;
                    let Ty::Agg(ai) = inst.ty else {
                        return Err("setfield の結果が集成体でない".to_string());
                    };
                    let d = &m.aggs[ai as usize];
                    let Some(fty) = field_ty_at(d, *off) else {
                        return Err("setfield のフィールドがない".to_string());
                    };
                    let mut buf = heap[h].clone();
                    let vv = get(&stack[top], v);
                    write_val(&mut buf, *off as usize, &m.aggs, fty, vv, &heap);
                    heap.push(buf);
                    (heap.len() - 1) as i128
                }
                Op::Tag(a) => {
                    // 結果の型で区別する: i32 なら集成体の列挙型 (置き場の添字)、bool なら Option / Result (タグのビット)。
                    let v = get(&stack[top], a);
                    if inst.ty == Ty::I32 {
                        let h = v as usize;
                        i32::from_le_bytes([heap[h][0], heap[h][1], heap[h][2], heap[h][3]]) as i128
                    } else {
                        (v & TAG_BIT != 0) as i128
                    }
                }
                Op::Alloca => {
                    // スタックスロットはバイト記憶の領域 (関数が戻るときに解放する)。大きさは指す先の型の幅。
                    let Ty::Ref(_, p) = inst.ty else {
                        return Err("alloca の結果が参照でない".to_string());
                    };
                    let a = bm.alloc_zeroed(size_of(&m.aggs, p.ty()) as usize);
                    stack[top].allocs.push(a);
                    a
                }
                Op::Load(p) => {
                    let a = get(&stack[top], p);
                    match inst.ty {
                        Ty::Agg(ai) => {
                            let blob = bm.read_bytes(a, m.aggs[ai as usize].size as usize)?;
                            heap.push(blob);
                            (heap.len() - 1) as i128
                        }
                        t => bm.read(a, t)?,
                    }
                }
                Op::Store { ptr, val } => {
                    let (a, v) = (get(&stack[top], ptr), get(&stack[top], val));
                    match vtys[fi][*val as usize] {
                        Ty::Agg(_) => {
                            let blob = heap[v as usize].clone();
                            bm.write_bytes(a, &blob)?;
                        }
                        t => bm.write(a, t, v)?,
                    }
                    0
                }
                Op::FieldAddr { a, off } => get(&stack[top], a) + *off as i128,
                Op::AsShared(p) | Op::Reborrow(p) => get(&stack[top], p),
            };
            stack[top].vals[inst.dst as usize] = result;
            stack[top].defd[inst.dst as usize] = true;
            continue;
        }
        // 終端命令
        match &blk.term {
            Term::Br((t, targs)) => {
                undefined(&stack[top], &func.key, targs)?;
                jump(&mut stack[top], m, fi, *t, targs);
            }
            Term::BrIf(c, (t1, a1), (t2, a2)) => {
                undefined(&stack[top], &func.key, &[*c])?;
                undefined(&stack[top], &func.key, a1)?;
                undefined(&stack[top], &func.key, a2)?;
                if stack[top].vals[*c as usize] != 0 {
                    jump(&mut stack[top], m, fi, *t1, a1);
                } else {
                    jump(&mut stack[top], m, fi, *t2, a2);
                }
            }
            Term::Ret(v) => {
                undefined(&stack[top], &func.key, &[*v])?;
                let mut val = stack[top].vals[*v as usize];
                let done = stack.pop().expect("フレーム");
                for a in &done.allocs {
                    bm.release(*a);
                }
                // 戻る関数が作った置き場の項目は捨てる。戻り値が参照する項目だけは、呼び出し元の置き場へ複製して残す。
                if has_heap(func.ret) {
                    let blobs = heap.split_off(done.hbase);
                    let mut out: Vec<Vec<u8>> = vec![];
                    val = relocate(func.ret, val, &blobs, done.hbase, &mut out);
                    heap.extend(out);
                } else {
                    heap.truncate(done.hbase);
                }
                // 関数の終わりで、この呼び出しが作った領域が漏れていないかを検査する (所有の下ろしの誤りをその関数で捕まえる)。
                let roots: Vec<i128> = if func.ret == Ty::Ptr { vec![val] } else { vec![] };
                let blobs: &[Vec<u8>] = if has_heap(func.ret) { &heap[done.hbase.min(heap.len())..] } else { &[] };
                if let Err(why) = bm.check_leaks(done.mem_base, &roots, blobs) {
                    return Err(format!("関数 {} の終わりで{}", func.key, why));
                }
                match stack.last_mut() {
                    None => {
                        // 正常終了では、解放されていない領域があってはならない (所有の下ろしの誤り)。
                        let n = bm.leaks();
                        if n > 0 {
                            return Err(format!("解放されていない領域が {} 個ある", n));
                        }
                        return Ok(Outcome::Value(val));
                    }
                    Some(caller) => {
                        if let Some(d) = done.dst {
                            caller.vals[d as usize] = val;
                            caller.defd[d as usize] = true;
                        }
                    }
                }
            }
            Term::Unreachable => return Err("到達しないブロックに入った".to_string()),
        }
    }
}

/// `vs` のどれかが未定義 (定義される前の読み) なら内部エラー。
fn undefined(fr: &Frame, key: &str, vs: &[VId]) -> Result<(), String> {
    match vs.iter().find(|v| !fr.defd[**v as usize]) {
        Some(v) => Err(format!("内部エラー: 関数 {} の %{} が定義される前に読まれた (定義が使用を支配しない)", key, v)),
        None => Ok(()),
    }
}

/// 移動先のブロックへ入る。引数は移動の前の値で受け取る (並列代入)。
fn jump(fr: &mut Frame, m: &Module, fi: usize, t: usize, args: &[VId]) {
    let vals: Vec<i128> = args.iter().map(|a| fr.vals[*a as usize]).collect();
    let params = &m.funcs[fi].blocks[t].params;
    for ((p, _), v) in params.iter().zip(vals) {
        fr.vals[*p as usize] = v;
        fr.defd[*p as usize] = true;
    }
    fr.blk = t;
    fr.ip = 0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kir::{Block, Func, Inst, Pay, Ty};

    #[test]
    fn arith_matches_interpreter_messages() {
        assert_eq!(arith(Aop::Add, Mode::Trap, Ty::U8, 200, 100), Err("attempt to add with overflow".into()));
        assert_eq!(arith(Aop::Sub, Mode::Trap, Ty::U32, 0, 1), Err("attempt to subtract with overflow".into()));
        assert_eq!(arith(Aop::Div, Mode::Trap, Ty::I8, 1, 0), Err("attempt to divide by zero".into()));
        assert_eq!(arith(Aop::Div, Mode::Trap, Ty::I8, -128, -1), Err("attempt to divide with overflow".into()));
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I8, 1, 0), Err("attempt to calculate the remainder with a divisor of by zero".into()));
        assert_eq!(arith(Aop::Mul, Mode::Wrap, Ty::U8, 16, 17), Ok(16));
    }

    #[test]
    fn min_rem_minus_one_overflows_for_every_signed_width() {
        // 案 A: 符号付きの `MIN % -1` は割り算が溢れるので余りも溢れ (`checked_rem` は None)。
        let rem = "attempt to calculate the remainder with overflow";
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I8, -128, -1), Err(rem.into()));
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I16, i16::MIN as i128, -1), Err(rem.into()));
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I32, i32::MIN as i128, -1), Err(rem.into()));
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I64, i64::MIN as i128, -1), Err(rem.into()));
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I128, i128::MIN, -1), Err(rem.into()));
        assert_eq!(arith(Aop::Rem, Mode::Trap, Ty::I8, -7, 2), Ok(-1));
        assert_eq!(checked(Aop::Rem, Ty::Opt(Pay::I8), -128, -1), 0);
    }

    #[test]
    fn arith_saturates_and_handles_128_bits() {
        assert_eq!(arith(Aop::Add, Mode::Sat, Ty::U8, 250, 10), Ok(255));
        assert_eq!(arith(Aop::Sub, Mode::Sat, Ty::U8, 3, 10), Ok(0));
        assert_eq!(arith(Aop::Add, Mode::Sat, Ty::I128, i128::MIN, i128::MIN), Ok(i128::MIN));
        assert_eq!(arith(Aop::Add, Mode::Sat, Ty::I128, i128::MAX, 1), Ok(i128::MAX));
        assert_eq!(arith(Aop::Mul, Mode::Sat, Ty::I128, i128::MIN, 2), Ok(i128::MIN));
        // u128 の最大値 (-1 のビットパターン) に 1 を足すと panic、wrap は 0
        assert_eq!(arith(Aop::Add, Mode::Trap, Ty::U128, -1, 1), Err("attempt to add with overflow".into()));
        assert_eq!(arith(Aop::Add, Mode::Wrap, Ty::U128, -1, 1), Ok(0));
    }

    /// `fn f(n: u64) -> u64`: 引数を 1 つ減らして呼ぶ再帰 (深さの検査用)。
    fn deep() -> Module {
        let f = Func {
            fqn: "t.deep".into(),
            key: "t.deep".into(),
            uses: vec![],
            may_panic: false,
            stack: None,
            ret: Ty::U64,
            link: None,
            blocks: vec![Block {
                params: vec![(0, Ty::U64)],
                insts: vec![
                    Inst { dst: 1, ty: Ty::U64, op: Op::Const(1) },
                    Inst { dst: 2, ty: Ty::U64, op: Op::Call { callee: "t.deep".into(), args: vec![0] } },
                ],
                term: Term::Ret(2),
            }],
        };
        Module { funcs: vec![f], ..Default::default() }
    }

    /// テキストの 1 関数のモジュールを実行する (鍵は関数名)。
    fn exec(text: &str, key: &str) -> Result<Outcome, String> {
        let m = crate::kir::parse_module(text).expect("parse");
        run(&m, key, &[])
    }

    #[test]
    fn nested_field_addresses_read_and_write_in_the_byte_memory() {
        // 集成体を alloca に置き、入れ子のフィールドのアドレス (`fieldaddr`) を通して書き換える。他のフィールドは変わらない。
        let text = "kir 1

agg#0 struct pt size=8 align=4 {x:i32@0 y:i32@4}
agg#1 struct line size=20 align=4 {a:agg#0@0 b:agg#0@8 n:u32@16}

fn t.nested key=t.nested uses=[] panics=never stack=- ret=i32
  b0():
    %0: mutref<agg#1> = alloca
    %1: i32 = const.i32 1
    %2: i32 = const.i32 2
    %3: agg#0 = make(%1, %2)
    %4: agg#0 = make(%2, %1)
    %5: u32 = const.u32 9
    %6: agg#1 = make(%3, %4, %5)
    %7: unit = store %0, %6
    %8: mutref<agg#0> = fieldaddr %0, +8
    %9: mutref<i32> = fieldaddr %8, +4
    %10: i32 = const.i32 40
    %11: unit = store %9, %10
    %12: agg#1 = load %0
    %13: agg#0 = field %12, +8
    %14: i32 = field %13, +4
    %15: agg#0 = field %12, +0
    %16: i32 = field %15, +0
    %17: i32 = add.i32.wrap %14, %16
    ret %17
end t.nested
";
        assert_eq!(exec(text, "t.nested"), Ok(Outcome::Value(41)));
        // 共有参照 ref<T> への store は検査が拒む。
        let bad = text.replace("%9: mutref<i32> = fieldaddr %8, +4", "%9: ref<i32> = fieldaddr %8, +4");
        let m = crate::kir::parse_module(&bad).expect("parse");
        assert!(!crate::kir::verify_module(&m).is_empty());
    }

    #[test]
    fn heap_round_trip_and_realloc_keep_contents() {
        let t = crate::kir::MEM_TEXT;
        assert_eq!(exec(t, "t.mem"), Ok(Outcome::Value(77)));
    }

    #[test]
    fn leaks_and_bad_frees_are_reported() {
        let leak = "kir 1

fn t.leak key=t.leak uses=[alloc] panics=never stack=- ret=unit
  b0():
    %0: usize = const.usize 8
    %1: ptr = heap.alloc %0
    %2: unit = const.unit ()
    ret %2
end t.leak
";
        assert_eq!(exec(leak, "t.leak"), Err("関数 t.leak の終わりで1 個の領域が解放されず、戻り値からも指されていない".to_string()));
        let twice = leak.replace("    %2: unit = const.unit ()\n    ret %2", "    %2: unit = heap.free %1\n    %3: unit = heap.free %1\n    ret %2")
            .replace("t.leak", "t.twice");
        assert!(exec(&twice, "t.twice").unwrap_err().contains("不正な解放"));
    }

    #[test]
    fn reading_uninitialized_heap_bytes_is_an_internal_error() {
        // `heap.alloc` の領域は未初期化 (malloc と同じ)。書く前に読むと内部エラー。`frame.bytes` は 0 で埋めてあるので読める。
        let src = "kir 1

fn t.raw key=t.raw uses=[alloc] panics=never stack=- ret=u8
  b0():
    %0: usize = const.usize 4
    %1: ptr = heap.alloc %0
    %2: u8 = mem.load.u8 %1 +0
    %3: unit = heap.free %1
    ret %2
end t.raw

fn t.frame key=t.frame uses=[] panics=never stack=- ret=u8
  b0():
    %0: ptr = frame.bytes 4
    %1: u8 = mem.load.u8 %0 +3
    ret %1
end t.frame
";
        assert!(exec(src, "t.raw").unwrap_err().contains("未初期化"));
        assert_eq!(exec(src, "t.frame"), Ok(Outcome::Value(0)));
        // `heap.realloc` で増えた部分は未初期化、引き継いだ部分は書かれたまま。
        let grow = "kir 1

fn t.grow key=t.grow uses=[alloc] panics=never stack=- ret=u8
  b0():
    %0: usize = const.usize 2
    %1: ptr = heap.alloc %0
    %2: u8 = const.u8 9
    %3: unit = mem.store.u8 %1 +1, %2
    %4: usize = const.usize 8
    %5: ptr = heap.realloc %1, %4
    %6: u8 = mem.load.u8 %5 +1
    %7: unit = heap.free %5
    ret %6
end t.grow
";
        assert_eq!(exec(grow, "t.grow"), Ok(Outcome::Value(9)));
        let tail = grow.replace("mem.load.u8 %5 +1", "mem.load.u8 %5 +5");
        assert!(exec(&tail, "t.grow").unwrap_err().contains("未初期化"));
    }

    #[test]
    fn reading_a_value_before_its_definition_is_an_internal_error() {
        // %2 は b1 でだけ定義されるのに、b2 (b1 を通らない経路もある) で読む。定義が使用を支配しないので、通らなかった経路で内部エラー。
        let src = "kir 1

fn t.dom key=t.dom uses=[] panics=never stack=- ret=u8
  b0(%0: bool):
    br_if %0, b1(), b2()
  b1():
    %2: u8 = const.u8 7
    br b2()
  b2():
    ret %2
end t.dom
";
        let m = crate::kir::parse_module(src).expect("parse");
        assert_eq!(run(&m, "t.dom", &[1]), Ok(Outcome::Value(7)));
        assert!(run(&m, "t.dom", &[0]).unwrap_err().contains("定義される前に読まれた"));
    }

    #[test]
    fn out_of_bounds_access_is_an_internal_error() {
        let src = "kir 1

fn t.oob key=t.oob uses=[alloc] panics=never stack=- ret=u8
  b0():
    %0: usize = const.usize 2
    %1: ptr = heap.alloc %0
    %2: usize = const.usize 2
    %3: ptr = ptr.add %1, %2
    %4: u8 = mem.load.u8 %3 +0
    %5: unit = heap.free %1
    ret %4
end t.oob
";
        assert!(exec(src, "t.oob").unwrap_err().contains("不正なメモリアクセス"));
    }

    #[test]
    fn a_leak_is_reported_at_the_end_of_the_function_that_made_it() {
        // 呼び先が確保して解放せずに戻る: 呼び出し元の終わりを待たず、呼び先の終わりで報告する。
        let src = "kir 1

fn t.mk key=t.mk uses=[alloc] panics=never stack=- ret=unit
  b0():
    %0: usize = const.usize 8
    %1: ptr = heap.alloc %0
    %2: unit = const.unit ()
    ret %2
end t.mk

fn t.outer key=t.outer uses=[alloc] panics=never stack=- ret=unit
  b0():
    %0: unit = call t.mk()
    ret %0
end t.outer
";
        let e = exec(src, "t.outer").unwrap_err();
        assert!(e.contains("関数 t.mk の終わり"), "{}", e);
    }

    #[test]
    fn a_region_returned_to_the_caller_or_stored_in_its_header_is_not_a_leak() {
        // 確保した領域を戻り値で返し (呼び出し元が解放)、または呼び出し元のヘッダに書いて渡す形は漏れではない。
        let src = "kir 1

fn t.give key=t.give uses=[alloc] panics=never stack=- ret=ptr
  b0():
    %0: usize = const.usize 8
    %1: ptr = heap.alloc %0
    ret %1
end t.give

fn t.fill key=t.fill uses=[alloc] panics=never stack=- ret=unit
  b0(%0: ptr):
    %1: usize = const.usize 8
    %2: ptr = heap.alloc %1
    %3: unit = mem.store.ptr %0 +0, %2
    %4: unit = const.unit ()
    ret %4
end t.fill

fn t.outer key=t.outer uses=[alloc] panics=never stack=- ret=unit
  b0():
    %0: ptr = frame.bytes 8
    %1: unit = call t.fill(%0)
    %2: ptr = mem.load.ptr %0 +0
    %3: unit = heap.free %2
    %4: ptr = call t.give()
    %5: unit = heap.free %4
    ret %1
end t.outer
";
        assert_eq!(exec(src, "t.outer"), Ok(Outcome::Value(0)));
    }

    #[test]
    fn frame_allocas_are_released_on_return() {
        let src = "kir 1

fn t.inner key=t.inner uses=[] panics=never stack=- ret=u8
  b0():
    %0: ptr = frame.bytes 4
    %1: u8 = const.u8 9
    %2: unit = mem.store.u8 %0 +3, %1
    %3: u8 = mem.load.u8 %0 +3
    ret %3
end t.inner

fn t.outer key=t.outer uses=[] panics=never stack=- ret=u8
  b0():
    %0: u8 = call t.inner()
    ret %0
end t.outer
";
        assert_eq!(exec(src, "t.outer"), Ok(Outcome::Value(9)));
    }

    #[test]
    fn bounds_check_reports_the_interpreter_message() {
        let src = "kir 1

fn t.bc key=t.bc uses=[] panics=may stack=- ret=unit
  b0():
    %0: usize = const.usize 2
    %1: usize = const.usize 5
    %2: unit = index.check %1, %0
    ret %2
end t.bc
";
        assert_eq!(
            exec(src, "t.bc"),
            Ok(Outcome::Panic("index out of bounds: the len is 2 but the index is 5".to_string()))
        );
    }

    #[test]
    fn data_is_static_and_not_a_leak() {
        let src = "kir 1

fn t.d key=t.d uses=[] panics=never stack=- ret=u8
  b0():
    %0: ptr = static.bytes 0102
    %1: usize = const.usize 1
    %2: ptr = ptr.add %0, %1
    %3: u8 = mem.load.u8 %2 +0
    ret %3
end t.d
";
        assert_eq!(exec(src, "t.d"), Ok(Outcome::Value(2)));
    }

    #[test]
    fn deep_recursion_reports_stack_overflow() {
        assert_eq!(run(&deep(), "t.deep", &[1]), Ok(Outcome::StackOverflow));
    }
}
