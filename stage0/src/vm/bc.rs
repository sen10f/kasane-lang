//! VM のバイトコード (設計_KIR.md 4 章、段階 2 の第 1 段)。
//!
//! - フレームは 8 バイトのスロット (`u64`) の列。各 SSA 値は型の幅 (`slots_of`) の連続したスロットを使う:
//!   64 ビット以下の整数・bool・浮動小数・ポインタ・参照は 1 スロット (符号付きは符号拡張、符号なしはゼロ拡張、`f32` は下位 32 ビット)、
//!   `i128` / `u128` は 2 スロット (下位, 上位)、集成体は clif と同じバイト配置の `ceil(size / 8)` スロット (フレームの中のバイト)、
//!   `Option` は `[タグ, ペイロード…]`、`Result` は `[タグ (1 = Err), ペイロード…]` (幅は大きい方の腕)。
//!   参照実行器 (`kir_exec.rs`) の `i128` 符号化とは、`run.rs` の `rd` / `wr` (遅い命令の変換) でだけ行き来する。
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

/// 型 `t` の値が使うスロット数。
pub fn slots_of(aggs: &[AggDef], t: Ty) -> u32 {
    match t {
        Ty::I128 | Ty::U128 => 2,
        Ty::Agg(i) => ((aggs[i as usize].size + 7) / 8).max(1),
        Ty::Opt(p) => 1 + slots_of(aggs, p.ty()),
        Ty::Res(a, e) => 1 + slots_of(aggs, a.ty()).max(slots_of(aggs, e.ty())),
        _ => 1,
    }
}

/// 符号つきか符号なしかを織り込んだ整数の比較 (64 ビットのスロットの値を直接比べる)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cx {
    Eq,
    Ne,
    Lts,
    Les,
    Gts,
    Ges,
    Ltu,
    Leu,
    Gtu,
    Geu,
}

pub fn cx_of(cc: Cc, signed: bool) -> Cx {
    match (cc, signed) {
        (Cc::Eq, _) => Cx::Eq,
        (Cc::Ne, _) => Cx::Ne,
        (Cc::Lt, true) => Cx::Lts,
        (Cc::Le, true) => Cx::Les,
        (Cc::Gt, true) => Cx::Gts,
        (Cc::Ge, true) => Cx::Ges,
        (Cc::Lt, false) => Cx::Ltu,
        (Cc::Le, false) => Cx::Leu,
        (Cc::Gt, false) => Cx::Gtu,
        (Cc::Ge, false) => Cx::Geu,
    }
}

#[inline(always)]
pub fn cx_eval(cx: Cx, x: u64, y: u64) -> bool {
    match cx {
        Cx::Eq => x == y,
        Cx::Ne => x != y,
        Cx::Lts => (x as i64) < (y as i64),
        Cx::Les => (x as i64) <= (y as i64),
        Cx::Gts => (x as i64) > (y as i64),
        Cx::Ges => (x as i64) >= (y as i64),
        Cx::Ltu => x < y,
        Cx::Leu => x <= y,
        Cx::Gtu => x > y,
        Cx::Geu => x >= y,
    }
}

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
    /// 結果が使うスロット数。
    pub nslots: u32,
    /// 列挙型の変種の添字 (先頭 4 バイトのタグ)。構造体・タプルは `None`。
    pub variant: Option<u32>,
    /// フィールドごとの (バイトオフセット, バイト数)。値の下位 `バイト数` バイトをそのまま写す (`unit` は 0 バイト)。
    pub fields: Vec<(u32, u32)>,
    /// フィールドごとの `Option` / `Result` の形 (枠のバイト数 `sb`、語数 `pw`)。`sb == 0` は普通の値 (そのまま写す)。
    pub packs: Vec<(u8, u8)>,
}

/// 呼び出しの引数などの表 (`VFunc::pool`) の項目: 下位 16 ビットがスロット、上位がスロット数。
pub fn pool_ent(s: S, n: u32) -> u32 {
    s as u32 | (n << 16)
}

#[derive(Clone, Debug)]
pub enum Ins {
    /// 小さい定数 (`i64` に入る値)。
    ConstI { d: S, v: i64 },
    /// 大きい定数 (`Prog::wide` の添字。下位 64 ビットをスロットに入れる)。
    ConstW { d: S, k: u32 },
    /// 128 ビットの定数 (2 スロット)。
    Const128 { d: S, k: u32 },
    /// `n` スロットを 0 にする (`None` など)。
    Zero { d: S, n: u16 },
    Mov { d: S, s: S },
    /// `n` スロットの複写 (重なってもよい)。
    MovN { d: S, s: S, n: u16 },
    /// `d = a + imm` (`fieldaddr`)。
    AddImm { d: S, a: S, imm: i64 },
    /// `d = p + off` (`ptr.add`)。
    PtrAdd { d: S, p: S, off: S },

    // ---- 即値の演算 (定数の被演算子を命令に埋める。`i32` に収まる定数だけ)
    // `溢れで panic する演算 (N = 64 ビット未満 / S = i64・isize / U = u64・usize)` と `wrapping_*` (W64 = 64 ビット、W = 64 ビット未満)。
    AddNI { d: S, a: S, t: u8, imm: i32 },
    SubNI { d: S, a: S, t: u8, imm: i32 },
    MulNI { d: S, a: S, t: u8, imm: i32 },
    AddSI { d: S, a: S, imm: i32 },
    SubSI { d: S, a: S, imm: i32 },
    MulSI { d: S, a: S, imm: i32 },
    AddUI { d: S, a: S, imm: i32 },
    SubUI { d: S, a: S, imm: i32 },
    MulUI { d: S, a: S, imm: i32 },
    AddW64I { d: S, a: S, imm: i32 },
    SubW64I { d: S, a: S, imm: i32 },
    MulW64I { d: S, a: S, imm: i32 },
    AddWI { d: S, a: S, t: u8, imm: i32 },
    SubWI { d: S, a: S, t: u8, imm: i32 },
    MulWI { d: S, a: S, t: u8, imm: i32 },
    CmpII { d: S, a: S, cx: Cx, imm: i32 },
    /// `a cx imm` が偽なら `to` へ。
    JCmpNotI { cx: Cx, a: S, imm: i32, to: u32 },
    /// `a cx b` が真なら `to` へ (ループの後ろ辺に、先頭の比較を写したもの)。
    JCmp { cx: Cx, a: S, b: S, to: u32 },
    JCmpI { cx: Cx, a: S, imm: i32, to: u32 },

    // ---- 制御
    Jmp { to: u32 },
    /// `c` が 0 でなければ `to` へ。
    JmpIf { c: S, to: u32 },
    /// `c` が 0 なら `to` へ。
    JmpIfNot { c: S, to: u32 },
    /// 整数 (128 ビットを除く) の比較 `a cx b` が偽なら `to` へ (`icmp` + `br_if` の融合)。
    JCmpNot { cx: Cx, a: S, b: S, to: u32 },
    /// 関数の戻り値 (`VFunc::ret_w` スロット) を呼び出し元へ返す。
    Ret { v: S },
    Unreachable,

    // ---- 整数の算術 (64 ビット以下。`t` は `FAST_TYS` の添字)
    AddN { d: S, a: S, b: S, t: u8 },
    SubN { d: S, a: S, b: S, t: u8 },
    MulN { d: S, a: S, b: S, t: u8 },
    AddS { d: S, a: S, b: S },
    SubS { d: S, a: S, b: S },
    MulS { d: S, a: S, b: S },
    AddU { d: S, a: S, b: S },
    SubU { d: S, a: S, b: S },
    MulU { d: S, a: S, b: S },
    AddW64 { d: S, a: S, b: S },
    SubW64 { d: S, a: S, b: S },
    MulW64 { d: S, a: S, b: S },
    AddW { d: S, a: S, b: S, t: u8 },
    SubW { d: S, a: S, b: S, t: u8 },
    MulW { d: S, a: S, b: S, t: u8 },
    /// 型の幅への切り捨て (狭める変換、符号の変わる変換)。
    WrapT { d: S, a: S, t: u8 },
    /// それ以外の整数の算術 (除算・剰余・飽和・128 ビット)。`ty` は `Prog::tys` の添字。
    Arith { d: S, a: S, b: S, op: Aop, mode: Mode, ty: u32 },
    /// 整数 (128 ビットを除く) と bool の比較。
    CmpI { d: S, a: S, b: S, cx: Cx },
    /// それ以外の比較 (浮動小数・128 ビット)。`ty` は被演算子の型。
    Cmp { d: S, a: S, b: S, cc: Cc, ty: u32 },
    Neg { d: S, a: S, mode: Mode, ty: u32 },
    FArith { d: S, a: S, b: S, op: Aop, ty: u32 },
    Shift { d: S, a: S, n: S, left: bool, ty: u32 },
    MinMax { d: S, a: S, b: S, max: bool, ty: u32 },
    FUn { d: S, a: S, f: FUn, src: u32, res: u32 },
    FBin { d: S, a: S, b: S, f: FBin, res: u32 },
    Conv { d: S, a: S, src: u32, dst: u32 },
    NotB { d: S, a: S },
    /// 浮動小数の NaN を、その場で正の quiet NaN にそろえる (ビットが見える所の前。設計_KIR.md 17.8)。
    CanonF { a: S, bits: u32 },
    NotI { d: S, a: S, ty: u32 },
    /// `& | ^` (0 = and, 1 = or, 2 = xor)。64 ビット以下と bool。
    BitOp { d: S, a: S, b: S, k: u8 },
    /// 128 ビットの `& | ^`。
    BitOp128 { d: S, a: S, b: S, k: u8 },
    BitCount { d: S, a: S, k: BitK, src: u32 },
    Checked { d: S, a: S, b: S, op: Aop, ty: u32 },

    // ---- 呼び出し
    /// 関数のアドレス (`FnPtr`): C に渡すコールバックの入口 (`ffi::tramp`) を返す。`f` は `Prog::funcs` の添字。
    FnPtr { d: S, f: u32 },
    /// 関数のアドレスを値として取る (`FnAddr`): `Prog::funcs` の添字 + 1 (`CallInd` が読む)。
    FnAddr { d: S, f: u32 },
    /// 間接呼び出し。`f` は関数値のスロット (`FnAddr` の値)。ほかは `Call` と同じ。
    CallInd { f: u32, d: S, argp: u32, argn: u32 },
    /// 直接呼び出し。引数は `VFunc::pool[argp .. argp + argn]` (`pool_ent`)。
    Call { f: u32, d: S, argp: u32, argn: u32 },
    /// `extern` 宣言の C 関数 (`Prog::ext` の添字)。
    CallExt { e: u32, d: S, argp: u32, argn: u32 },

    // ---- 集成体と Option / Result (フレームの中のスロット列)
    Make { d: S, info: u32, argp: u32 },
    /// 集成体の中のスカラーを読む (`a` のスロット列のバイトオフセット `off`)。
    FieldS { d: S, a: S, off: u32, k: LK },
    /// 集成体の中の集成体を `nb` バイト写す。
    FieldA { d: S, a: S, off: u32, nb: u32 },
    /// `a` (`ns` スロット) を `d` に写し、バイトオフセット `off` に `v` の下位 `nb` バイトを書く。
    SetField { d: S, a: S, v: S, off: u32, ns: u16, nb: u16 },
    /// `Option` / `Result` の値を集成体のフィールドへ書く (KIR のメモリの形: 枠の `sb` バイトに続けてタグの 1 バイト。`pw` は枠の語数)。
    SetFieldOpt { d: S, a: S, v: S, off: u32, ns: u16, sb: u8, pw: u8 },
    /// `Option` / `Result` の値を集成体のフィールドから読む (`SetFieldOpt` の逆)。
    FieldOpt { d: S, a: S, off: u32, sb: u8, pw: u8 },
    /// `Option` / `Result` の値を番地 `p` + `off` から読む / そこへ書く (`Op::Load` / `Op::Store`)。
    LoadOpt { d: S, p: S, off: u32, sb: u8, pw: u8 },
    StoreOpt { p: S, off: u32, v: S, sb: u8, pw: u8 },
    /// `std.process` の命令 (`ProcFn::ALL` の添字 `f`、引数は `a` の先頭から `n` 個)。結果は `d`。子プロセスの表は `Vm` が持つ。
    Proc { d: S, f: u8, a: [S; 3] },
    /// 列挙型のタグ (i32) を読む。
    TagEnum { d: S, a: S },
    /// `Option` が Some か / `Result` が Err か。
    TagOpt { d: S, a: S },
    /// `d = [tag, a (n スロット)]`。
    WrapN { d: S, a: S, n: u16, tag: u8 },
    /// ペイロード (1 スロット)。
    Payload1 { d: S, a: S },
    PayloadN { d: S, a: S, n: u16 },

    // ---- メモリ
    FrameBytes { d: S, size: u32 },
    Load { d: S, p: S, off: u32, k: LK },
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
    /// 集成体 (`size` バイト) を参照の先から読む。
    LoadAgg { d: S, p: S, size: u32 },
    Store { p: S, off: u32, v: S, w: u8 },
    /// 集成体 (`size` バイト) を参照の先へ書く。
    StoreAgg { p: S, v: S, size: u32 },
    HeapAlloc { d: S, n: S },
    HeapRealloc { d: S, p: S, n: S },
    HeapFree { p: S },
    IndexCheck { i: S, len: S },
    Utf8Valid { d: S, p: S, len: S },

    // ---- 入出力・panic
    Panic { msg: u32, arg: S, aty: u32 },
    Abort,
    Exit { code: S },
    Write { d: S, fd: S, data: S, nl: bool },
    Eprint { data: S },
    FmtFloat { a: S, bits: u32 },
    FloatText { d: S, a: S, dst: S, bits: u32 },
    ParseInt { d: S, s: S, radix: S, ty: u32, strict: bool },
    EnvArgCount { d: S },
    EnvArg { d: S, i: S },
    EnvVar { d: S, name: S },
    TimeNowNs { d: S },
    /// `std.task.block_on_threads` の中か (n > 1)。ワーカーは起こさない (決定的な協調実行) が、`spawn_local` の禁止などの意味を保つ。
    MtOn { d: S },
    MtBegin { d: S, n: S },
    MtEnd,

    // ---- std.net (kir_exec と同じ意味。ソケット表は `Vm` が持つ)
    NetListen { d: S, addr: S },
    NetConnect { d: S, addr: S },
    NetAccept { d: S, sock: S },
    NetRead { d: S, sock: S, buf: S, len: S },
    NetWrite { d: S, sock: S, buf: S, len: S },
    NetFlush { d: S, sock: S },
    NetClose { d: S, sock: S },
    NetLocalPort { d: S, sock: S },

    // ---- std.fs (kir_exec と同じ意味。ファイル表は `Vm` が持つ)。結果は `Result<..., std.io.Error>` か `bool`。
    /// `create`: 作成して切り詰め (`FsCreate`)、false なら読み取り専用 (`FsOpen`)。
    FsOpen { d: S, path: S, create: bool },
    FsExists { d: S, path: S },
    FsRemove { d: S, path: S },
    /// `kind`: 0 rename・1 create_dir・2 remove_dir・3 open_append・4 stat・5 read_dir (`FsK` の順)。`a` `b` `c` は引数の slot (使わなければ 0)。
    FsSys { d: S, kind: u8, a: S, b: S, c: S },
    FsClose { d: S, fd: S },
    /// `flush` と `sync` はどちらも `sync_all` (kir_exec と同じ)。
    FsSync { d: S, fd: S },
    FsRead { d: S, fd: S, buf: S, len: S },
    FsWrite { d: S, fd: S, buf: S, len: S },
    /// `std.rand` の OS の乱数 (`fatal`: `Rng.from_entropy` の致命的な形)。
    RandFill { d: S, buf: S, len: S, fatal: bool },
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
    /// 命令ごとの行表 (設計_KIR.md 7 章 13): `(命令番号, 行)` の昇順。命令 `pc` の行は、番号が `pc` 以下の最後の項目の行。
    pub marks: Vec<(u32, u32)>,
    /// 行表のソースファイル (合成した関数は空)。
    pub file: String,
    /// 引数の型 (コールバックの引数をレジスタから読むのに使う)。
    pub ptys: Vec<Ty>,
    pub ret: Ty,
    /// 戻り値のスロット数。
    pub ret_w: u32,
}

impl VFunc {
    /// 命令 `pc` を作った式の行 (0 は不明)。
    pub fn line_at(&self, pc: usize) -> u32 {
        let i = self.marks.partition_point(|m| m.0 as usize <= pc);
        if i == 0 {
            0
        } else {
            self.marks[i - 1].1
        }
    }
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
    /// `global.bytes` の領域 (名前 → アドレス。プログラムごとに 1 つ、解放しない)。
    pub globals: std::collections::HashMap<String, usize>,
    /// 呼び出しの深さの上限 (`Module::depth` から。0 は既定)。
    pub depth: u64,
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

/// 2 の補数の切り捨て (`Ty::wrap` の 64 ビット以下の専用版)。結果はスロットの表現 (符号付きは符号拡張)。
#[inline(always)]
pub fn wrap_fast(t: u8, v: u64) -> u64 {
    match t {
        0 => v as i8 as i64 as u64,
        1 => v as i16 as i64 as u64,
        2 => v as i32 as i64 as u64,
        3 | 4 => v,
        5 => v as u8 as u64,
        6 => v as u16 as u64,
        7 => v as u32 as u64,
        _ => v,
    }
}

/// 64 ビット未満の整数の最小値・最大値 (`i64` で比べる。`FAST_TYS` の添字)。
pub const LO64: [i64; 10] = [i8::MIN as i64, i16::MIN as i64, i32::MIN as i64, 0, 0, 0, 0, 0, 0, 0];
pub const HI64: [i64; 10] = [i8::MAX as i64, i16::MAX as i64, i32::MAX as i64, 0, 0, u8::MAX as i64, u16::MAX as i64, u32::MAX as i64, 0, 0];

/// `FAST_TYS` の添字 `t` の型の分類: 0 = 64 ビット未満、1 = i64 / isize、2 = u64 / usize。
pub fn arith_class(t: u8) -> u8 {
    match t {
        3 | 4 => 1,
        8 | 9 => 2,
        _ => 0,
    }
}

/// `Option` / `Result` の値の集成体の中の形 (`(枠のバイト数, 語数)`)。`None` は `Option` / `Result` ではない。
/// KIR のメモリの形 (`kir::opt_slot_bits`): 枠の `sb` バイトに続けてタグの 1 バイト (1 = Some / Err)。VM のスロットの形は
/// 先頭がタグの語、続けて `pw` 語の枠 (`WrapN`)。この 2 つは並びが違うので、フィールドの読み書きで変換する。
pub fn opt_shape(aggs: &[AggDef], t: Ty) -> Option<(u8, u8)> {
    if !matches!(t, Ty::Opt(_) | Ty::Res(..)) {
        return None;
    }
    let sb = crate::kir::opt_slot_bits(aggs, t) / 8;
    let pw = slots_of(aggs, t) - 1;
    Some((sb.min(255) as u8, pw.min(255) as u8))
}
