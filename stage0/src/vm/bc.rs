//! VM のバイトコード (設計_KIR.md 4 章、段階 2 の第 1 段)。
//!
//! - 各 SSA 値は関数のフレームの 1 スロット (`i128`)。値の符号化は参照実行器 (`kir_exec.rs`) と同じ
//!   (整数は符号拡張した値、`u128` はビットパターン、浮動小数は IEEE のビット列、`Option` / `Result` は tag bit 64 + ペイロードの語、
//!   集成体は置き場 (`Vm::heap`) の添字)。設計の 8 バイトのスロットは第 2 段で検討する (128 ビット整数が 2 スロットになる)。
//! - 命令は Rust の enum (`Ins`) で、被演算子はフレーム相対のスロット番号 (`u32`)。呼び先の関数・型・集成体の配置は
//!   コンパイル時に決めて、実行時の名前引きと型の判定はしない。
//! - 制御の合流 (ブロック引数) は辺ごとの `Mov` に展開し、分岐は命令番号への `Jmp` になる。

use crate::kir::{AggDef, Aop, BitK, Cc, FBin, FUn, Func, Mode, Ty};

/// フレーム相対のスロット番号。
pub type S = u16;

/// `Ins::Call` などで「結果を受けない」ことを表すスロット。
pub const NO_SLOT: S = u16::MAX;

/// 1 関数のスロット数の上限 (`S` が `u16` なので)。超える関数は VM が扱えない (木走査へ退避)。
pub const MAX_SLOTS: u32 = u16::MAX as u32 - 1;

/// メモリ読み出しの幅と符号 (`MemLoad` / `Load`)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LK {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    I128,
    Bool,
}

/// 64 ビット以下の整数型の番号 (`AddT` などの専用命令が引く表の添字)。
pub const FAST_TYS: [Ty; 10] = [Ty::I8, Ty::I16, Ty::I32, Ty::I64, Ty::Isize, Ty::U8, Ty::U16, Ty::U32, Ty::U64, Ty::Usize];

pub fn fast_ti(t: Ty) -> Option<u8> {
    FAST_TYS.iter().position(|x| *x == t).map(|i| i as u8)
}

/// 構築 (`make`) の情報。
#[derive(Clone, Debug)]
pub struct MakeInfo {
    pub size: u32,
    /// 列挙型の変種の添字 (先頭 4 バイトのタグ)。構造体・タプルは `None`。
    pub variant: Option<u32>,
    /// フィールドごとの (オフセット, 型)。
    pub fields: Vec<(u32, Ty)>,
}

#[derive(Clone, Debug)]
pub enum Ins {
    /// 小さい定数 (`i64` に入る値)。
    ConstI { d: S, v: i64 },
    /// 大きい定数 (`Prog::wide` の添字)。
    ConstW { d: S, k: u32 },
    Mov { d: S, s: S },
    /// `d = a + imm` (`fieldaddr`)。
    AddImm { d: S, a: S, imm: i64 },
    /// `d = p + off` (`ptr.add`)。
    PtrAdd { d: S, p: S, off: S },

    // ---- 即値の演算 (定数の被演算子を命令に埋める。`i32` に収まる定数だけ)
    AddTI { d: S, a: S, t: u8, imm: i32 },
    SubTI { d: S, a: S, t: u8, imm: i32 },
    MulTI { d: S, a: S, t: u8, imm: i32 },
    AddWI { d: S, a: S, t: u8, imm: i32 },
    SubWI { d: S, a: S, t: u8, imm: i32 },
    MulWI { d: S, a: S, t: u8, imm: i32 },
    CmpII { d: S, a: S, cc: Cc, imm: i32 },
    /// `a cc imm` が偽なら `to` へ。
    JCmpNotI { cc: Cc, a: S, imm: i32, to: u32 },
    /// `a cc b` が真なら `to` へ (ループの後ろ辺に、先頭の比較を写したもの)。
    JCmp { cc: Cc, a: S, b: S, to: u32 },
    JCmpI { cc: Cc, a: S, imm: i32, to: u32 },

    // ---- 制御
    Jmp { to: u32 },
    /// `c` が 0 でなければ `to` へ。
    JmpIf { c: S, to: u32 },
    /// `c` が 0 なら `to` へ。
    JmpIfNot { c: S, to: u32 },
    /// 整数 (`u128` を除く) の比較 `a cc b` が偽なら `to` へ (`icmp` + `br_if` の融合)。
    JCmpNot { cc: Cc, a: S, b: S, to: u32 },
    Ret { v: S },
    Unreachable,

    // ---- 整数の算術 (64 ビット以下)
    /// 溢れで panic する `+ - *`。`t` は `FAST_TYS` の添字。
    AddT { d: S, a: S, b: S, t: u8 },
    SubT { d: S, a: S, b: S, t: u8 },
    MulT { d: S, a: S, b: S, t: u8 },
    /// `wrapping_*`。
    AddW { d: S, a: S, b: S, t: u8 },
    SubW { d: S, a: S, b: S, t: u8 },
    MulW { d: S, a: S, b: S, t: u8 },
    /// 型の幅への切り捨て (狭める変換)。
    WrapT { d: S, a: S, t: u8 },
    /// それ以外の整数の算術 (除算・剰余・飽和・128 ビット)。`ty` は `Prog::tys` の添字。
    Arith { d: S, a: S, b: S, op: Aop, mode: Mode, ty: u32 },
    /// 整数 (`u128` を除く) と bool の比較。値は符号拡張した `i128` なので直接比べる。
    CmpI { d: S, a: S, b: S, cc: Cc },
    /// それ以外の比較 (浮動小数・`u128`)。`ty` は被演算子の型。
    Cmp { d: S, a: S, b: S, cc: Cc, ty: u32 },
    Neg { d: S, a: S, mode: Mode, ty: u32 },
    FArith { d: S, a: S, b: S, op: Aop, ty: u32 },
    Shift { d: S, a: S, n: S, left: bool, ty: u32 },
    MinMax { d: S, a: S, b: S, max: bool, ty: u32 },
    FUn { d: S, a: S, f: FUn, src: u32, res: u32 },
    FBin { d: S, a: S, b: S, f: FBin, res: u32 },
    Conv { d: S, a: S, src: u32, dst: u32 },
    NotB { d: S, a: S },
    NotI { d: S, a: S, ty: u32 },
    /// `& | ^` (0 = and, 1 = or, 2 = xor)。
    BitOp { d: S, a: S, b: S, k: u8 },
    BitCount { d: S, a: S, k: BitK, src: u32 },
    Checked { d: S, a: S, b: S, op: Aop, ty: u32 },

    // ---- 呼び出し
    /// 直接呼び出し。引数のスロットは `VFunc::pool[argp .. argp + argn]`。
    Call { f: u32, d: S, argp: u32, argn: u32 },
    /// `extern` 宣言の C 関数 (`Prog::ext` の添字)。
    CallExt { e: u32, d: S, argp: u32, argn: u32 },

    // ---- 集成体と Option / Result
    Make { d: S, info: u32, argp: u32 },
    Field { d: S, a: S, off: u32, ty: u32 },
    SetField { d: S, a: S, v: S, off: u32, agg: u32 },
    /// 列挙型のタグ (i32) を読む。
    TagEnum { d: S, a: S },
    /// `Option` が Some か / `Result` が Err か。
    TagOpt { d: S, a: S },
    Some { d: S, a: S, pay: u32 },
    Ok { d: S, a: S, pay: u32 },
    Err { d: S, a: S, pay: u32 },
    Payload { d: S, a: S, ty: u32 },

    // ---- メモリ
    FrameBytes { d: S, size: u32 },
    Load { d: S, p: S, off: u32, k: LK },
    /// 集成体を参照の先から読む (置き場に写す)。
    /// `*(p + x + off)`。直前の `PtrAdd` と融合したもの。
    LoadX { d: S, p: S, x: S, off: u32, k: LK },
    StoreX { p: S, x: S, off: u32, v: S, w: u8 },
    LoadU64 { d: S, p: S, off: u32 },
    LoadI64 { d: S, p: S, off: u32 },
    LoadU32 { d: S, p: S, off: u32 },
    LoadI32 { d: S, p: S, off: u32 },
    LoadU8 { d: S, p: S, off: u32 },
    Store64 { p: S, off: u32, v: S },
    Store8 { p: S, off: u32, v: S },
    LoadAgg { d: S, p: S, size: u32 },
    Store { p: S, off: u32, v: S, w: u8 },
    /// 集成体 (置き場の項目) を参照の先へ書く。
    StoreAgg { p: S, v: S },
    HeapAlloc { d: S, n: S },
    HeapRealloc { d: S, p: S, n: S },
    HeapFree { p: S },
    IndexCheck { i: S, len: S },
    Utf8Valid { d: S, p: S, len: S },

    // ---- 入出力・panic
    Panic { msg: u32, arg: S },
    Abort,
    Write { d: S, fd: S, data: S },
    Eprint { data: S },
    FmtFloat { a: S, bits: u32 },
    ParseInt { d: S, s: S, radix: S, ty: u32 },
    EnvArgCount { d: S },
    EnvArg { d: S, i: S },
    EnvVar { d: S, name: S },

    // ---- std.net (kir_exec と同じ意味。ソケット表は `Vm` が持つ)
    NetListen { d: S, addr: S },
    NetConnect { d: S, addr: S },
    NetAccept { d: S, sock: S },
    NetRead { d: S, sock: S, buf: S, len: S },
    NetWrite { d: S, sock: S, buf: S, len: S },
    NetFlush { d: S, sock: S },
    NetClose { d: S, sock: S },
    NetLocalPort { d: S, sock: S },
}

const _: () = assert!(std::mem::size_of::<Ins>() <= 16);

#[derive(Clone, Debug)]
pub struct VFunc {
    pub key: String,
    pub code: Vec<Ins>,
    pub pool: Vec<u32>,
    /// フレームのスロット数 (辺の一時スロットを含む)。
    pub nslots: u32,
    pub nparams: u32,
    pub ret: Ty,
    /// 戻り値が置き場の項目を指すか (戻るとき `relocate` が要る)。
    pub ret_heap: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Prog {
    pub funcs: Vec<VFunc>,
    pub aggs: Vec<AggDef>,
    pub tys: Vec<Ty>,
    pub wide: Vec<i128>,
    pub strs: Vec<String>,
    pub makes: Vec<MakeInfo>,
    pub ext: Vec<Func>,
    /// 関数の鍵 → `funcs` の添字 (ホットリロードで、古い版の呼び出しを新しい版の関数へ解決する)。
    pub by_key: std::collections::HashMap<String, u32>,
}

impl Prog {
    pub fn find(&self, key: &str) -> Option<usize> {
        self.funcs.iter().position(|f| f.key == key)
    }
}

/// `ty` の整数の最小値・最大値 (`FAST_TYS` の添字)。
pub const LO: [i128; 10] = [
    i8::MIN as i128,
    i16::MIN as i128,
    i32::MIN as i128,
    i64::MIN as i128,
    i64::MIN as i128,
    0,
    0,
    0,
    0,
    0,
];
pub const HI: [i128; 10] = [
    i8::MAX as i128,
    i16::MAX as i128,
    i32::MAX as i128,
    i64::MAX as i128,
    i64::MAX as i128,
    u8::MAX as i128,
    u16::MAX as i128,
    u32::MAX as i128,
    u64::MAX as i128,
    u64::MAX as i128,
];

/// 2 の補数の切り捨て (`Ty::wrap` の 64 ビット以下の専用版)。
#[inline(always)]
pub fn wrap_fast(t: u8, v: i128) -> i128 {
    match t {
        0 => v as i8 as i128,
        1 => v as i16 as i128,
        2 => v as i32 as i128,
        3 | 4 => v as i64 as i128,
        5 => v as u8 as i128,
        6 => v as u16 as i128,
        7 => v as u32 as i128,
        _ => v as u64 as i128,
    }
}
