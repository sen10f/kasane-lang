//! KIR: 型付き・単相化済みの中間表現 (設計_KIR.md 2 章、段階 1)。
//!
//! - 1 関数 = 1 つの単相インスタンス (`key`)。値は SSA (`%n` は 1 回だけ定義) で、制御の合流はブロック引数で渡す。
//! - 型は整数 (`i8`..`i128`、`isize`、`u8`..`u128`、`usize`)、浮動小数 (`f32` / `f64`)、`bool`、`unit`、参照 (`ref<T>` / `mutref<T>`)、
//!   組み込みの `Option<P>` / `Result<T, E>` (ペイロードはスカラー・unit・集成体・入れ子の Option / Result。設計_KIR.md 11 章)、
//!   生のアドレス `ptr` (ヒープ・フレーム・静的領域)、集成体 (`Module::aggs`)。
//!   値は全部 `i128` の 1 語で持つ。`u128` はビットパターン、浮動小数は IEEE のビット列 (`f32` は下位 32 ビット)。
//! - テキスト形式は `kir 1` (設計_KIR.md 7 章 18: 版が違えば読み込みを拒否する)。
//!   `print_module` の出力を `parse_module` で読み直すと、同じ構造になり、もう一度 `print_module` すると同じ文字列になる。
//! - `verify_module` は SSA の一意性、型、ブロック引数の個数、効果 (trap の有無、`uses`、`panics`) を検査する。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// テキスト形式の版の行 (7 章 18)。
pub const VERSION_LINE: &str = "kir 1";

/// 値番号 (関数の中で一意の SSA 名。テキストでは `%n`)。
pub type VId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    I8,
    I16,
    I32,
    I64,
    I128,
    Isize,
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
    F32,
    F64,
    Bool,
    Unit,
    /// 生のアドレス (ヒープ・フレーム・静的領域の先頭)。`ptr` と書く。算術と比較はしない (値は 0 が null)。
    Ptr,
    /// 参照 `ref<T>` (false) / `mutref<T>` (true)。値はアドレス (参照実行器ではセルの番号)。
    /// 指す先はスカラー (整数と bool) だけ。`Ty` は `Copy` を保つため、中身は `Ty` ではなく `Pointee` にする。
    Ref(bool, Pointee),
    /// 組み込みの `Option<P>` (設計_KIR.md 11 章)。値はタグ (Some か) とペイロード 1 つ。
    Opt(Pay),
    /// 組み込みの `Result<T, E>` (設計_KIR.md 11 章)。値はタグ (Err か) とペイロード 1 つ。
    Res(Pay, Pay),
    /// 集成体 (構造体・タプル・列挙型)。`Module::aggs` の添字。値は集成体の配置 (バイト列) をもつ不変の値。
    Agg(u32),
}

/// 参照の指す先。整数・浮動小数・bool・集成体 (unit・ptr・参照・Option・Result は入れない)。`Ty::Ref` の中身。
/// 値はバイト記憶のアドレス。指す先の配置は集成体なら型表のとおり、スカラーなら幅のとおり (設計_KIR.md 10.10)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pointee {
    I8,
    I16,
    I32,
    I64,
    I128,
    Isize,
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
    F32,
    F64,
    Bool,
    /// 生ポインタ (番地。`std.tls` の SSL の値などを、構造体のフィールドとして参照越しに読む)。
    Ptr,
    /// 集成体 (型表 `Module::aggs` の添字)。
    Agg(u32),
    /// `Option<..>` の値 (`&mut Option<Box<Node>>`。枠の語とタグの配置は `Ty::Opt` のメモリ上の形と同じ)。
    Opt(Pay),
}

impl Pointee {
    pub fn ty(self) -> Ty {
        match self {
            Pointee::I8 => Ty::I8,
            Pointee::I16 => Ty::I16,
            Pointee::I32 => Ty::I32,
            Pointee::I64 => Ty::I64,
            Pointee::I128 => Ty::I128,
            Pointee::Isize => Ty::Isize,
            Pointee::U8 => Ty::U8,
            Pointee::U16 => Ty::U16,
            Pointee::U32 => Ty::U32,
            Pointee::U64 => Ty::U64,
            Pointee::U128 => Ty::U128,
            Pointee::Usize => Ty::Usize,
            Pointee::F32 => Ty::F32,
            Pointee::F64 => Ty::F64,
            Pointee::Bool => Ty::Bool,
            Pointee::Ptr => Ty::Ptr,
            Pointee::Agg(i) => Ty::Agg(i),
            Pointee::Opt(p) => Ty::Opt(p),
        }
    }

    /// 指す先に使える型なら、その `Pointee`。unit・参照・Option・Result は `None`。
    pub fn of(t: Ty) -> Option<Pointee> {
        Some(match t {
            Ty::I8 => Pointee::I8,
            Ty::I16 => Pointee::I16,
            Ty::I32 => Pointee::I32,
            Ty::I64 => Pointee::I64,
            Ty::I128 => Pointee::I128,
            Ty::Isize => Pointee::Isize,
            Ty::U8 => Pointee::U8,
            Ty::U16 => Pointee::U16,
            Ty::U32 => Pointee::U32,
            Ty::U64 => Pointee::U64,
            Ty::U128 => Pointee::U128,
            Ty::Usize => Pointee::Usize,
            Ty::F32 => Pointee::F32,
            Ty::F64 => Pointee::F64,
            Ty::Bool => Pointee::Bool,
            Ty::Ptr => Pointee::Ptr,
            Ty::Agg(i) => Pointee::Agg(i),
            Ty::Opt(p) => Pointee::Opt(p),
            Ty::Unit | Ty::Ref(..) | Ty::Res(..) => return None,
        })
    }
}

/// Option / Result のペイロードの型 (設計_KIR.md 11.5)。スカラー、unit、集成体 (`Agg`)、入れ子の `Option` / `Result`。
/// 128 ビット整数は値の枠 (下位 64 ビットと tag の bit) に入らないので持てない (`Pay::of` が `None`)。浮動小数は 64 ビット以内なので持てる。
/// `Ty` は `Copy` を保つため、入れ子の中身は `&'static Pay` で持つ。同じ型は `intern` で 1 つにまとめる (作った箱は型の数だけ残る)。
/// `Pay::Opt(x)` は型 `Option<x>`、`Pay::Res(t, e)` は型 `Result<t, e>` を表す (`Ty::Opt(p)` の `p` は `Option` の中の型)。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pay {
    I8,
    I16,
    I32,
    I64,
    Isize,
    U8,
    U16,
    U32,
    U64,
    Usize,
    F32,
    F64,
    Bool,
    /// ヒープの番地 (`&String` / `&Vec<T>` の借用のヘッダのアドレスなど)。
    Ptr,
    Unit,
    /// 集成体 (`Module::aggs` の添字)。
    Agg(u32),
    /// 型 `Option<x>` (入れ子)。
    Opt(&'static Pay),
    /// 型 `Result<t, e>` (入れ子)。
    Res(&'static Pay, &'static Pay),
}

thread_local! {
    static INTERNED: RefCell<HashSet<&'static Pay>> = RefCell::new(HashSet::new());
}

/// 入れ子の `Pay` を 1 つにまとめた参照にする。同じ値には同じ参照を返す (`Pay` の比較は値で行う)。
fn intern(p: Pay) -> &'static Pay {
    INTERNED.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(r) = s.get(&p) {
            return *r;
        }
        let r: &'static Pay = Box::leak(Box::new(p));
        s.insert(r);
        r
    })
}

const ALL_TYS: [Ty; 17] = [
    Ty::I8,
    Ty::I16,
    Ty::I32,
    Ty::I64,
    Ty::I128,
    Ty::Isize,
    Ty::U8,
    Ty::U16,
    Ty::U32,
    Ty::U64,
    Ty::U128,
    Ty::Usize,
    Ty::F32,
    Ty::F64,
    Ty::Bool,
    Ty::Unit,
    Ty::Ptr,
];

/// Option / Result の値の中で、タグを置く bit (Option は 1 = Some、Result は 1 = Err)。
pub const TAG_BIT: i128 = 1 << 64;
/// ペイロードの bit (下位 64 ビット)。
pub const PAY_MASK: i128 = (1 << 64) - 1;

impl Pay {
    pub fn ty(self) -> Ty {
        match self {
            Pay::I8 => Ty::I8,
            Pay::I16 => Ty::I16,
            Pay::I32 => Ty::I32,
            Pay::I64 => Ty::I64,
            Pay::Isize => Ty::Isize,
            Pay::U8 => Ty::U8,
            Pay::U16 => Ty::U16,
            Pay::U32 => Ty::U32,
            Pay::U64 => Ty::U64,
            Pay::Usize => Ty::Usize,
            Pay::F32 => Ty::F32,
            Pay::F64 => Ty::F64,
            Pay::Bool => Ty::Bool,
            Pay::Ptr => Ty::Ptr,
            Pay::Unit => Ty::Unit,
            Pay::Agg(i) => Ty::Agg(i),
            Pay::Opt(x) => Ty::Opt(*x),
            Pay::Res(t, e) => Ty::Res(*t, *e),
        }
    }

    /// 型なら対応する `Pay`。128 ビット整数・参照・`ptr` は `None` (値の枠に入らない、または指せない)。
    pub fn of(t: Ty) -> Option<Pay> {
        Some(match t {
            Ty::I8 => Pay::I8,
            Ty::I16 => Pay::I16,
            Ty::I32 => Pay::I32,
            Ty::I64 => Pay::I64,
            Ty::Isize => Pay::Isize,
            Ty::U8 => Pay::U8,
            Ty::U16 => Pay::U16,
            Ty::U32 => Pay::U32,
            Ty::U64 => Pay::U64,
            Ty::Usize => Pay::Usize,
            Ty::F32 => Pay::F32,
            Ty::F64 => Pay::F64,
            Ty::Bool => Pay::Bool,
            Ty::Ptr => Pay::Ptr,
            Ty::Unit => Pay::Unit,
            Ty::Agg(i) => Pay::Agg(i),
            Ty::Opt(p) => Pay::Opt(intern(p)),
            Ty::Res(t, e) => Pay::Res(intern(t), intern(e)),
            Ty::I128 | Ty::U128 | Ty::Ref(..) => return None,
        })
    }

    /// 集成体の添字を `f` で写した `Pay` (入れ子の中も写す)。`prune_aggs` が添字を振り直すときに使う。
    pub fn map_aggs(self, f: &impl Fn(u32) -> u32) -> Pay {
        match self {
            Pay::Agg(i) => Pay::Agg(f(i)),
            Pay::Opt(x) => Pay::Opt(intern(x.map_aggs(f))),
            Pay::Res(t, e) => Pay::Res(intern(t.map_aggs(f)), intern(e.map_aggs(f))),
            p => p,
        }
    }
}

/// ペイロードの値 (`v` は `Option` / `Result` の値全体の符号化) を、型 `t` の値に読み直す。
/// タグは見ない。bool は 0 / 1、unit は 0、整数は下位 64 ビットを型の範囲へ写す。
/// 集成体と入れ子の `Option` / `Result` は、語 (下位 64 ビット) が置き場の添字なので、そのまま返す (読み方は実行器が決める)。
pub fn payload_of(t: Ty, v: i128) -> i128 {
    match t {
        Ty::Bool => v & 1,
        Ty::Unit => 0,
        Ty::Agg(_) | Ty::Opt(_) | Ty::Res(..) => v & PAY_MASK,
        _ => t.wrap(v & PAY_MASK),
    }
}

/// ペイロードの符号化: 整数・bool を下位 64 ビットの 2 の補数で持つ。
pub fn payload_bits(v: i128) -> i128 {
    v & PAY_MASK
}

/// 値を型に従って人が読む形にする (`ksn kir --run` の表示と同じ)。
pub fn show_value(t: Ty, v: i128) -> String {
    match t {
        Ty::Bool => (v != 0).to_string(),
        Ty::Unit => "()".to_string(),
        Ty::U128 => (v as u128).to_string(),
        Ty::F32 => f32::from_bits(v as u32).to_string(),
        Ty::F64 => f64::from_bits(v as u64).to_string(),
        Ty::Opt(p) => {
            if v & TAG_BIT != 0 {
                format!("Some({})", show_value(p.ty(), payload_of(p.ty(), v)))
            } else {
                "None".to_string()
            }
        }
        Ty::Res(t0, e0) => {
            if v & TAG_BIT != 0 {
                format!("Err({})", show_value(e0.ty(), payload_of(e0.ty(), v)))
            } else {
                format!("Ok({})", show_value(t0.ty(), payload_of(t0.ty(), v)))
            }
        }
        _ => v.to_string(),
    }
}

impl Ty {
    pub fn name(self) -> String {
        match self {
            Ty::I8 => "i8".into(),
            Ty::I16 => "i16".into(),
            Ty::I32 => "i32".into(),
            Ty::I64 => "i64".into(),
            Ty::I128 => "i128".into(),
            Ty::Isize => "isize".into(),
            Ty::U8 => "u8".into(),
            Ty::U16 => "u16".into(),
            Ty::U32 => "u32".into(),
            Ty::U64 => "u64".into(),
            Ty::U128 => "u128".into(),
            Ty::Usize => "usize".into(),
            Ty::F32 => "f32".into(),
            Ty::F64 => "f64".into(),
            Ty::Bool => "bool".into(),
            Ty::Unit => "unit".into(),
            Ty::Ptr => "ptr".into(),
            Ty::Ref(m, p) => format!("{}<{}>", if m { "mutref" } else { "ref" }, p.ty().name()),
            // 区切りは `,` だけ (空白を含めない)。テキスト形式の 1 語を保つため。
            Ty::Opt(p) => format!("Option<{}>", p.ty().name()),
            Ty::Res(t, e) => format!("Result<{},{}>", t.ty().name(), e.ty().name()),
            Ty::Agg(i) => format!("agg#{}", i),
        }
    }

    /// 集成体の添字を `f` で写した型 (`Ty::Agg` と、`Option` / `Result` の中の型)。
    pub fn map_aggs(self, f: &impl Fn(u32) -> u32) -> Ty {
        match self {
            Ty::Agg(i) => Ty::Agg(f(i)),
            Ty::Ref(m, Pointee::Agg(i)) => Ty::Ref(m, Pointee::Agg(f(i))),
            Ty::Ref(m, Pointee::Opt(p)) => Ty::Ref(m, Pointee::Opt(p.map_aggs(f))),
            Ty::Opt(p) => Ty::Opt(p.map_aggs(f)),
            Ty::Res(t, e) => Ty::Res(t.map_aggs(f), e.map_aggs(f)),
            t => t,
        }
    }

    pub fn parse(s: &str) -> Option<Ty> {
        if let Some(n) = s.strip_prefix("agg#") {
            return n.parse::<u32>().ok().map(Ty::Agg);
        }
        if let Some(x) = ALL_TYS.iter().copied().find(|t| t.name() == s) {
            return Some(x);
        }
        if let Some(inner) = s.strip_prefix("Option<").and_then(|r| r.strip_suffix('>')) {
            return Pay::of(Ty::parse(inner)?).map(Ty::Opt);
        }
        if let Some(inner) = s.strip_prefix("Result<").and_then(|r| r.strip_suffix('>')) {
            // 入れ子の `Result<..>` の `,` では割らない (`split_top`)。
            let parts = split_top(inner);
            if parts.len() != 2 {
                return None;
            }
            return Some(Ty::Res(Pay::of(Ty::parse(parts[0])?)?, Pay::of(Ty::parse(parts[1])?)?));
        }
        if let Some(inner) = s.strip_prefix("ref<").and_then(|r| r.strip_suffix('>')) {
            return Pointee::of(Ty::parse(inner)?).map(|p| Ty::Ref(false, p));
        }
        if let Some(inner) = s.strip_prefix("mutref<").and_then(|r| r.strip_suffix('>')) {
            return Pointee::of(Ty::parse(inner)?).map(|p| Ty::Ref(true, p));
        }
        None
    }

    /// 整数型 (`i8`..`usize`、`i128` / `u128` を含む)。`bool` / `unit` / 浮動小数 / `Option` / `Result` / 参照は含まない。
    pub fn is_int(self) -> bool {
        !matches!(self, Ty::Bool | Ty::Unit | Ty::Ptr | Ty::F32 | Ty::F64 | Ty::Ref(..) | Ty::Opt(_) | Ty::Res(..) | Ty::Agg(_))
    }

    pub fn is_float(self) -> bool {
        matches!(self, Ty::F32 | Ty::F64)
    }

    pub fn is_ref(self) -> bool {
        matches!(self, Ty::Ref(..))
    }

    /// スカラー (整数・浮動小数・bool・unit・ptr)。`Option` / `Result` / 参照は含まない。`Cmp` や定数の型はこれに限る (ptr の比較は verify が拒む)。
    pub fn is_scalar(self) -> bool {
        !matches!(self, Ty::Opt(_) | Ty::Res(..) | Ty::Ref(..) | Ty::Agg(_))
    }

    pub fn signed(self) -> bool {
        matches!(self, Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::I128 | Ty::Isize)
    }

    /// 整数の幅 (ビット)。`isize` / `usize` はターゲットのポインタの幅 (`ptrwidth`。既定は 64 ビット、wasm32 は 32 ビット)。
    pub fn bits(self) -> u32 {
        match self {
            Ty::I8 | Ty::U8 => 8,
            Ty::I16 | Ty::U16 => 16,
            Ty::I32 | Ty::U32 | Ty::F32 => 32,
            Ty::I128 | Ty::U128 => 128,
            Ty::Isize | Ty::Usize => crate::ptrwidth::bits(),
            _ => 64,
        }
    }

    /// 最小値。`u128` は 0。
    pub fn min(self) -> i128 {
        match self {
            Ty::I128 => i128::MIN,
            t if t.signed() => -(1i128 << (t.bits() - 1)),
            _ => 0,
        }
    }

    /// 最大値。`u128` の最大値は `i128` で表せないので、ビットパターン (-1) を返す (木走査 `tmax` と同じ)。
    pub fn max(self) -> i128 {
        match self {
            Ty::I128 => i128::MAX,
            Ty::U128 => -1,
            t if t.signed() => (1i128 << (t.bits() - 1)) - 1,
            t => (1i128 << t.bits()) - 1,
        }
    }

    /// 整数の値 `v` が型の範囲にあるか。`u128` は値 (ビットパターン) を範囲外とみなさない (`v` が非負のとき、呼び出し側が確かめる)。
    pub fn in_range(self, v: i128) -> bool {
        match self {
            Ty::U128 => v >= 0,
            _ => v >= self.min() && v <= self.max(),
        }
    }

    /// 2 の補数の切り捨て (`wrapping_*` の結果を型の範囲に収める)。128 ビットは値をそのまま持つ。
    pub fn wrap(self, v: i128) -> i128 {
        let b = self.bits();
        if b == 128 {
            return v;
        }
        let x = v & ((1i128 << b) - 1);
        if self.signed() && (x >> (b - 1)) & 1 == 1 {
            x - (1i128 << b)
        } else {
            x
        }
    }
}

/// 算術演算。`div` / `rem` は 0 除算と溢れで trap する (trap モードのとき)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aop {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl Aop {
    pub fn name(self) -> &'static str {
        match self {
            Aop::Add => "add",
            Aop::Sub => "sub",
            Aop::Mul => "mul",
            Aop::Div => "div",
            Aop::Rem => "rem",
        }
    }
}

/// 溢れの扱い。`Wrap` は `wrapping_*` (溢れを切り捨てる)、`Trap` は `+ - * / %` (溢れで panic。現行どおり)、
/// `Sat` は `saturating_*` (型の範囲に飽和させる)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Wrap,
    Trap,
    Sat,
}

/// 浮動小数の 1 引数の演算 (`f32` は `f64` へ広げて計算し、結果を `f32` へ丸める。木走査と同じ)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FUn {
    Neg,
    Abs,
    Sqrt,
    Floor,
    Ceil,
    Trunc,
    RoundEven,
    Sin,
    Cos,
    Tan,
    Exp,
    Ln,
    IsNan,
    IsFinite,
}

impl FUn {
    pub fn name(self) -> &'static str {
        match self {
            FUn::Neg => "fneg",
            FUn::Abs => "fabs",
            FUn::Sqrt => "fsqrt",
            FUn::Floor => "ffloor",
            FUn::Ceil => "fceil",
            FUn::Trunc => "ftrunc",
            FUn::RoundEven => "fround_even",
            FUn::Sin => "fsin",
            FUn::Cos => "fcos",
            FUn::Tan => "ftan",
            FUn::Exp => "fexp",
            FUn::Ln => "fln",
            FUn::IsNan => "fis_nan",
            FUn::IsFinite => "fis_finite",
        }
    }

    pub fn parse(s: &str) -> Option<FUn> {
        const ALL: [FUn; 14] = [
            FUn::Neg,
            FUn::Abs,
            FUn::Sqrt,
            FUn::Floor,
            FUn::Ceil,
            FUn::Trunc,
            FUn::RoundEven,
            FUn::Sin,
            FUn::Cos,
            FUn::Tan,
            FUn::Exp,
            FUn::Ln,
            FUn::IsNan,
            FUn::IsFinite,
        ];
        ALL.iter().copied().find(|f| f.name() == s)
    }
}

/// 浮動小数の 2 引数の演算。`Powi` の第 2 引数だけは `i32` (指数)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FBin {
    Max,
    Min,
    Rem,
    Atan2,
    Powi,
}

impl FBin {
    pub fn name(self) -> &'static str {
        match self {
            FBin::Max => "fmax",
            FBin::Min => "fmin",
            FBin::Rem => "frem",
            FBin::Atan2 => "fatan2",
            FBin::Powi => "fpowi",
        }
    }

    pub fn parse(s: &str) -> Option<FBin> {
        [FBin::Max, FBin::Min, FBin::Rem, FBin::Atan2, FBin::Powi].into_iter().find(|f| f.name() == s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cc {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Cc {
    pub fn name(self) -> &'static str {
        match self {
            Cc::Eq => "eq",
            Cc::Ne => "ne",
            Cc::Lt => "lt",
            Cc::Le => "le",
            Cc::Gt => "gt",
            Cc::Ge => "ge",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
    Xor,
}

/// `std.process` の命令の種類 (`Op::Proc`)。`Command` は実行環境側の表のハンドル (i32) で、`Child` は `{ pid, stdin, stdout, stderr }`
/// (4 つの i32。パイプの記述子で、ないものは -1) の集成体。パイプの読み書き・閉じるは `fs.read` / `fs.write` / `fs.close` をそのまま使う
/// (`fs.*` は `uses [process]` の関数にも許す)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcFn {
    /// `Command.new(program)`: `(fat) -> i32` (ハンドル)。
    New,
    /// `Command.arg(a)` / `env_remove(k)` / `current_dir(p)`: `(i32, fat) -> unit`。
    Arg,
    EnvRemove,
    Dir,
    /// `Command.env(k, v)`: `(i32, fat, fat) -> unit`。
    Env,
    /// `Command.env_clear()` / `stdin_piped()` ほか: `(i32) -> unit`。
    EnvClear,
    InPiped,
    InNull,
    OutPiped,
    OutNull,
    ErrPiped,
    ErrNull,
    /// `Command.cancel()`: `(i32) -> unit`。設定を捨てる。
    Cancel,
    /// `Command.spawn()`: `(i32) -> Result<Child, std.io.Error>`。設定を消費する。
    Spawn,
    /// `Command.output()` の前半: `(i32) -> Result<Child, std.io.Error>`。標準入力は /dev/null、標準出力・標準エラーは無名の一時ファイルに
    /// 受けて、子の終了を待ち、先頭へ巻き戻す。結果の `Child` は `pid` = 終了コード、`stdin` = 終了させたシグナル (なければ 0)、`stdout` / `stderr` = 巻き戻した記述子。
    Output,
    /// `Child.wait()`: `(i32 pid) -> Result<ExitStatus, std.io.Error>` (`ExitStatus` は `{ code: i32, signal: i32 }`)。
    Wait,
    /// `Child.kill()`: `(i32 pid) -> Result<(), std.io.Error>`。
    Kill,
}

impl ProcFn {
    pub const ALL: [ProcFn; 17] = [
        ProcFn::New,
        ProcFn::Arg,
        ProcFn::EnvRemove,
        ProcFn::Dir,
        ProcFn::Env,
        ProcFn::EnvClear,
        ProcFn::InPiped,
        ProcFn::InNull,
        ProcFn::OutPiped,
        ProcFn::OutNull,
        ProcFn::ErrPiped,
        ProcFn::ErrNull,
        ProcFn::Cancel,
        ProcFn::Spawn,
        ProcFn::Output,
        ProcFn::Wait,
        ProcFn::Kill,
    ];
    pub fn name(self) -> &'static str {
        match self {
            ProcFn::New => "new",
            ProcFn::Arg => "arg",
            ProcFn::EnvRemove => "env_remove",
            ProcFn::Dir => "current_dir",
            ProcFn::Env => "env",
            ProcFn::EnvClear => "env_clear",
            ProcFn::InPiped => "stdin_piped",
            ProcFn::InNull => "stdin_null",
            ProcFn::OutPiped => "stdout_piped",
            ProcFn::OutNull => "stdout_null",
            ProcFn::ErrPiped => "stderr_piped",
            ProcFn::ErrNull => "stderr_null",
            ProcFn::Cancel => "cancel",
            ProcFn::Spawn => "spawn",
            ProcFn::Output => "output",
            ProcFn::Wait => "wait",
            ProcFn::Kill => "kill",
        }
    }
    /// ランタイムへ渡す番号 (`ALL` の添字)。
    pub fn code(self) -> i32 {
        ProcFn::ALL.iter().position(|f| *f == self).unwrap_or(0) as i32
    }
    pub fn parse(s: &str) -> Option<ProcFn> {
        ProcFn::ALL.iter().copied().find(|f| f.name() == s)
    }
    /// 被演算子の個数。
    pub fn arity(self) -> usize {
        match self {
            ProcFn::New => 1,
            ProcFn::Arg | ProcFn::EnvRemove | ProcFn::Dir => 2,
            ProcFn::Env => 3,
            _ => 1,
        }
    }
}


/// `Op::FsSys` の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsK {
    /// `rename(from, to)`: 結果は `Result<(), std.io.Error>`。引数は fat のパス 2 つ。
    Rename,
    /// `create_dir(path)`: 結果は `Result<(), std.io.Error>`。
    CreateDir,
    /// `remove_dir(path)`: 結果は `Result<(), std.io.Error>`。
    RemoveDir,
    /// `open_append(path)`: 追記で開く (なければ作る)。結果は `Result<std.fs.File, std.io.Error>`。
    OpenAppend,
    /// `stat(path, field: u32)`: リンクを辿る。field 0 = 大きさ、1 = 種別 (0 ファイル / 1 ディレクトリ / 2 それ以外)、2 = 更新時刻 (UNIX 時刻からのナノ秒、範囲外は 0)。結果は `Result<u64, std.io.Error>`。
    Stat,
    /// `read_dir(path, buf, cap)`: 名前のバイト順に整列した `[種別 1 バイト][名前][0]` の並びを `buf` (容量 `cap` バイト) へ書き、必要な総バイト数を返す (`cap` を超えたら何も書かず、呼び直す)。結果は `Result<usize, std.io.Error>`。
    ReadDir,
    /// `open_dir(path)`: ディレクトリを読み取りで開く。結果は `Result<std.fs.Dir, std.io.Error>`。ディレクトリでなければ ENOTDIR。
    OpenDir,
    /// `open_beneath(dir, rel)`: `dir` (記述子 i32) の下だけを開く。`..`・絶対・どの symlink も拒む。結果は `Result<std.fs.File, std.io.Error>`。
    OpenBeneath,
    /// `fstat(fd, field)`: 開いている記述子の情報 (`stat` と同じ field)。リンクを辿らない。結果は `Result<u64, std.io.Error>`。
    FStat,
}

impl FsK {
    pub fn name(self) -> &'static str {
        match self {
            FsK::Rename => "rename",
            FsK::CreateDir => "create_dir",
            FsK::RemoveDir => "remove_dir",
            FsK::OpenAppend => "open_append",
            FsK::Stat => "stat",
            FsK::ReadDir => "read_dir",
            FsK::OpenDir => "open_dir",
            FsK::OpenBeneath => "open_beneath",
            FsK::FStat => "fstat",
        }
    }
    pub fn arity(self) -> usize {
        match self {
            FsK::Rename | FsK::Stat | FsK::OpenBeneath | FsK::FStat => 2,
            FsK::CreateDir | FsK::RemoveDir | FsK::OpenAppend | FsK::OpenDir => 1,
            FsK::ReadDir => 3,
        }
    }
    /// fat (`&str`) のパスの引数の位置。`OpenBeneath` は `args[0]` が記述子 (i32)、`args[1]` がパス。`FStat` はパスを取らない。
    pub fn path_slots(self) -> &'static [usize] {
        match self {
            FsK::Rename => &[0, 1],
            FsK::OpenBeneath => &[1],
            FsK::FStat => &[],
            _ => &[0],
        }
    }
    pub const ALL: [FsK; 9] = [
        FsK::Rename,
        FsK::CreateDir,
        FsK::RemoveDir,
        FsK::OpenAppend,
        FsK::Stat,
        FsK::ReadDir,
        FsK::OpenDir,
        FsK::OpenBeneath,
        FsK::FStat,
    ];
}

/// `Op::NetSys` の種類 (`std.net` の原始操作のうち、`listen` / `connect` / `accept` / `read` / `write` / `flush` / `close` / `local_port` 以外)。
/// 引数の並びと結果の型は種類ごとに決まる (`kir.rs` の検査 / `kir_exec.rs` / `kir_llvm.rs` が同じ並びを読む)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetK {
    /// `set_read_timeout(sock, ms: u64)`: `SO_RCVTIMEO`。0 = なし。結果は `Result<(), std.io.Error>`。
    ReadTimeout,
    /// `set_write_timeout(sock, ms: u64)`: `SO_SNDTIMEO`。0 = なし。結果は `Result<(), std.io.Error>`。
    WriteTimeout,
    /// `shutdown(sock, how: i32)`: 0 = 読み、1 = 書き、2 = 両方。結果は `Result<(), std.io.Error>`。
    Shutdown,
    /// `peer_addr_raw(sock, buf, cap)`: 相手のアドレス (`ip:port`) を `buf` へ書く。結果は `Result<usize, std.io.Error>`。
    PeerAddr,
    /// `local_addr_raw(sock, buf, cap)`: 自分のアドレス。`PeerAddr` と同じ形。
    LocalAddr,
    /// `resolve_raw(host, port, buf, cap)`: `getaddrinfo` の結果を `アドレス\0` の並びで `buf` へ書く。結果は `Result<usize, std.io.Error>`。
    Resolve,
    /// `UdpSocket.bind(addr)`: 結果は `Result<UdpSocket 集成体, std.net.Error>`。
    UdpBind,
    /// `UdpSocket.send_to(sock, buf, len, addr)`。結果は `Result<usize, std.io.Error>`。
    UdpSendTo,
    /// `UdpSocket.recv_from_raw(sock, buf, len, from, fromcap)`: 送り元を `from` へ `ip:port\0` で書く。結果は `Result<usize, std.io.Error>`。
    UdpRecvFrom,
    /// `set_read_deadline(sock, ms: u64)` / `set_write_deadline`: 今から `ms` 後を、以後の読み / 書きの期限にする。結果は `Result<(), std.io.Error>`。
    ReadDeadline,
    WriteDeadline,
    /// `clear_deadlines(sock)`: 読み・書きの期限を両方外す。結果は `Result<(), std.io.Error>`。
    ClearDeadlines,
    /// `set_nodelay(sock, on: bool)`: `TCP_NODELAY`。結果は `Result<(), std.io.Error>`。
    SetNoDelay,
    /// `accept_timeout(sock, ms: u64)`: 期限つきの受理。期限切れは ETIMEDOUT。結果は `Result<TcpStream 集成体, std.net.Error>`。
    AcceptTimeout,
    /// `connect_timeout(addr, ms: u64)`: 期限つきの接続。結果は `Result<TcpStream 集成体, std.net.Error>`。
    ConnectTimeout,
    /// `read_try(sock, buf, len)` / `write_try`: 待たずに 1 回だけ読み書きする (`MSG_DONTWAIT`)。待つことになるときは EAGAIN (番号 11) の `std.io.Error`。結果は `Result<usize, std.io.Error>`。非同期ソケットが使う。
    ReadTry,
    WriteTry,
}

impl NetK {
    pub fn name(self) -> &'static str {
        match self {
            NetK::ReadTimeout => "set_read_timeout",
            NetK::WriteTimeout => "set_write_timeout",
            NetK::Shutdown => "shutdown",
            NetK::PeerAddr => "peer_addr",
            NetK::LocalAddr => "local_addr",
            NetK::Resolve => "resolve",
            NetK::UdpBind => "udp_bind",
            NetK::UdpSendTo => "udp_send_to",
            NetK::UdpRecvFrom => "udp_recv_from",
            NetK::ReadDeadline => "set_read_deadline",
            NetK::WriteDeadline => "set_write_deadline",
            NetK::ClearDeadlines => "clear_deadlines",
            NetK::SetNoDelay => "set_nodelay",
            NetK::AcceptTimeout => "accept_timeout",
            NetK::ConnectTimeout => "connect_timeout",
            NetK::ReadTry => "read_try",
            NetK::WriteTry => "write_try",
        }
    }
    pub fn arity(self) -> usize {
        match self {
            NetK::ReadTimeout | NetK::WriteTimeout | NetK::Shutdown => 2,
            NetK::PeerAddr | NetK::LocalAddr | NetK::ReadTry | NetK::WriteTry => 3,
            NetK::Resolve | NetK::UdpSendTo => 4,
            NetK::UdpBind => 1,
            NetK::UdpRecvFrom => 5,
            NetK::ReadDeadline | NetK::WriteDeadline | NetK::SetNoDelay | NetK::AcceptTimeout | NetK::ConnectTimeout => 2,
            NetK::ClearDeadlines => 1,
        }
    }
    pub const ALL: [NetK; 17] = [
        NetK::ReadTry,
        NetK::WriteTry,
        NetK::ReadTimeout,
        NetK::WriteTimeout,
        NetK::Shutdown,
        NetK::PeerAddr,
        NetK::LocalAddr,
        NetK::Resolve,
        NetK::UdpBind,
        NetK::UdpSendTo,
        NetK::UdpRecvFrom,
        NetK::ReadDeadline,
        NetK::WriteDeadline,
        NetK::ClearDeadlines,
        NetK::SetNoDelay,
        NetK::AcceptTimeout,
        NetK::ConnectTimeout,
    ];
}

/// 命令の本体。結果の型と番号は `Inst` が持つ。
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// `std.process` の命令 (`ProcFn` の説明を見る)。`uses [process]` の関数だけ。
    Proc { f: ProcFn, args: Vec<VId> },
    /// 定数。`bool` は 0 / 1、`unit` は 0。
    Const(i128),
    /// 算術。結果の型 (`Inst::ty`) が演算の型。
    Arith { op: Aop, mode: Mode, a: VId, b: VId },
    /// 比較。`ty` は被演算子の型 (結果は `bool`)。
    Cmp { cc: Cc, ty: Ty, a: VId, b: VId },
    /// 単項マイナス (整数)。`Trap` は符号付き整数で溢れを panic、`Wrap` は `wrapping_neg` (全整数)。
    Neg { a: VId, mode: Mode },
    /// 浮動小数の算術 `+ - * /` (panic しない)。結果の型 (`Inst::ty`) が演算の型。`Aop::Rem` は使わない。
    FArith { op: Aop, a: VId, b: VId },
    /// 整数のシフト (`wrapping_shl` / `wrapping_shr`)。量 `n` は `u32`。量は型幅で剰余する。
    Shift { left: bool, a: VId, n: VId },
    /// 整数の `max` / `min`。
    MinMax { max: bool, a: VId, b: VId },
    /// 浮動小数の 1 引数の演算。`src` は被演算子の型 (`IsNan` などは結果が `bool`)。
    FUn { f: FUn, src: Ty, a: VId },
    /// 浮動小数の 2 引数の演算 (`Powi` だけは第 2 引数が `i32`)。
    FBin { f: FBin, a: VId, b: VId },
    /// 数値の変換。`src` から結果の型 (`Inst::ty`) へ。整数どうしは値を保つ (型の範囲に入る場合だけ検査が通す)。
    Conv { src: Ty, a: VId },
    /// ビット列の再解釈 (`to_bits` / `from_bits`)。`src` と結果の型は同じ幅。値はそのまま。
    Bitcast { src: Ty, a: VId },
    /// `bool` の否定 (`not.bool`) と整数のビット反転 (`not.<int>`)。
    Not(VId),
    /// 非短絡の論理・ビット演算 (`& | ^`)。`bool` と整数の両方。
    Bool(BoolOp, VId, VId),
    /// ビットの数え上げ (`count_ones` / `leading_zeros` / `trailing_zeros`)。結果は `u32`。`src` は被演算子の整数型。
    BitCount { k: BitK, src: Ty, a: VId },
    /// 直接呼び出し (単相インスタンスの鍵で指す)。
    Call { callee: String, args: Vec<VId> },
    /// 関数のアドレス (`ptr`)。C のコールバックとして関数を渡すときに使う。
    /// `callee` は関数の鍵。取り込みの無い `fn` のみ (段階 1)。
    FnPtr { callee: String },
    /// 集成体の構築。`variant` が `Some(k)` なら列挙型のバリアント k (先頭にタグを置く)、`None` なら構造体・タプル。
    /// `fields` は宣言の順 (列挙型ではそのバリアントのペイロードの順)。
    Make { variant: Option<u32>, fields: Vec<VId> },
    /// 集成体の読み出し: バイトオフセット `off` から `Inst::ty` の値を取る。
    Field { a: VId, off: u32 },
    /// 構造体・タプルのフィールドを置き換えた新しい値 (元の値は変わらない)。`v` の型は `off` のフィールドの型。
    SetField { a: VId, off: u32, v: VId },
    /// タグを読む。被演算子が列挙型なら先頭 4 バイトの i32 (変種の添字、結果は i32)。
    /// `Option` / `Result` なら bool (Some / Err か、結果は bool)。結果の型で区別する。
    Tag(VId),
    /// スタックスロットの確保 (結果は `mutref<T>`、`T` は `Inst::ty` の指す先)。関数の入口ブロックに置く。
    Alloca,
    /// 参照を通した読み出し `*p` (結果の型は指す先の型)。
    Load(VId),
    /// 参照を通した書き込み `*p = v` (`p` は `mutref<T>`、`v` は `T`。結果は unit)。
    Store { ptr: VId, val: VId },
    /// フィールドのアドレス: `a` (`ref<agg>` / `mutref<agg>`) の構造体のバイトオフセット `off` のフィールドを指す参照。
    /// 結果は同じ可変性の `ref<F>` / `mutref<F>` (`F` はそのフィールドの型)。タプルと列挙型には使わない。
    FieldAddr { a: VId, off: u32 },
    /// `&mut T` を `&T` として使う (同じアドレス。結果は `ref<T>`)。
    AsShared(VId),
    /// 借用の開始 (注釈。設計_KIR.md 24 章)。`a` と同じ値・同じ型の別名を作る (実行では複製と同じ)。`&x` / `&mut x` の結果として使い、
    /// 借用検査が「この値が生きている間が借用の区間」と読む。`mutable` は `&mut`。
    Borrow { a: VId, mutable: bool },
    /// 局所変数の寿命の終わり (注釈。結果は unit、実行では何もしない)。`a` は局所のスロット (`alloca` / `frame.bytes`) の値。
    /// この後も生きている、このスロットを指す値は、借用検査が違反とする。文の終わりの一時値にも使う。
    ScopeEnd(VId),
    /// 借用の運び (注釈。設計_KIR.md 24.11): 値 `b` が参照 `a` の指す先から読まれた値なら、`b` は `a` の借用を運ぶ。実行には影響しない。
    Carry { a: VId, b: VId },
    /// 所有の注釈 (設計_KIR.md 24 章。結果は unit、実行では何もしない)。`owe` なら、`a` の値に線形 (消費の義務) が生じた。
    /// そうでなければ、`a` の値が move された (Copy でない局所変数の値渡し)。`a` は SSA の値か、Vec / String のヘッダのスロット。
    Own { a: VId, kind: OwnKind },
    /// `mutref<T>` を同じアドレスの別の値として複製する。スタックスロットの値番号と、参照の値を
    /// 取り違えないために使う (スロットの番号を参照の値に流用すると、読み書きの区別がつかなくなる)。
    Reborrow(VId),
    /// `Some(a)`: 結果の型 `Option<P>` (`a` は `P`)。`None` は `Const(0)` (型 `Option<P>`)。
    Some(VId),
    /// `Ok(a)`: 結果の型 `Result<T, E>` (`a` は `T`)。
    Ok(VId),
    /// `Err(a)`: 結果の型 `Result<T, E>` (`a` は `E`)。
    Err(VId),
    /// ペイロードを読む (結果の型 `Inst::ty` で読む)。タグは見ない。呼び出し側が `Tag` で確かめてから使う。
    Payload(VId),
    /// `checked_<op>` (結果の型 `Option<P>`、被演算子は `P`)。溢れと 0 除算は `None`。panic しないので `panics never` でも使える。
    Checked { op: Aop, a: VId, b: VId },
    /// フレームの領域 (`size` バイト、0 にはしない)。関数が戻るときに解放される。結果は `ptr`。
    FrameBytes { size: u32 },
    /// 静的な読み取り専用のバイト列の先頭。結果は `ptr`。同じ命令は常に同じアドレスを返す。
    StaticBytes(Vec<u8>),
    /// 名前付きの大域領域 (`global.bytes NAME SIZE`)。0 で初期化され、プログラムの間ずっと生き、同じ名前は同じ領域。書き込める (`static` の `Mutex` など。設計_KIR.md 22 章)。
    GlobalBytes { name: String, size: u32 },
    /// `p + off` (`off` は usize の値。境界は見ない)。結果は `ptr`。
    PtrAdd { p: VId, off: VId },
    /// `p + off` から `Inst::ty` の幅で読む (リトルエンディアン)。
    MemLoad { p: VId, off: u32 },
    /// `p + off` へ `v` (型 `ty`) を書く。結果は unit。
    MemStore { ty: Ty, p: VId, off: u32, v: VId },
    /// ヒープの確保 (`size` バイト、usize の値)。結果は `ptr`。`uses [alloc]` の関数だけ。
    HeapAlloc(VId),
    /// ヒープの再確保: 先頭 `min(旧, 新)` バイトを複写して旧い領域を解放する。`p` が `null` なら `malloc` と同じ。結果は `ptr`。`uses [alloc]` のみ。
    HeapRealloc { p: VId, size: VId },
    /// 解放。`null` (0) は何もしない。結果は unit。
    HeapFree(VId),
    /// 添字の境界検査: `i < len` でなければ panic (文言は木走査の添字と同じ)。結果は unit。`panics may` のときだけ。
    IndexCheck { i: VId, len: VId },
    /// panic: 文言 `msg` を出して終了コード 101 (木走査の panic と同じ文言)。`arg` があれば `: <値>` を続ける
    /// (値は整数の 10 進。`Result::unwrap` の Err の値)。結果は unit。`panics may` のときだけ。
    Panic { msg: String, arg: Option<VId> },
    /// 書き出し: ファイル記述子 `fd` (`i32`。1 = stdout、2 = stderr) へ `data` (`fat` = `{ptr, len}`) の全バイトを書く。
    /// 結果は `Result<unit, std.io.Error>` (設計_KIR.md 12.2)。短い書き込みは繰り返し、EINTR は再試行する。`uses [io.console]` の関数だけ。
    /// `nl` が真なら、`data` の後に改行 1 バイトを足して、全体を 1 回の書き込みにする (`std.io.print_line`。スレッド同士で行が混ざらない)。
    Write { fd: VId, data: VId, nl: bool },
    /// `std.net.listen(addr)`: `socket`+`bind`+`listen`。`addr` は fat (`&str`)。結果は `Result<i32, std.io.Error>`。
    /// 記述子はリスニングソケット。`uses [io.net]` の関数だけ。`panics never` でも置ける。
    NetListen { addr: VId },
    /// `std.net.connect(addr)`: `socket`+`connect`。`addr` は fat (`&str`)。結果は `Result<i32, std.io.Error>`。
    /// 記述子は接続済みストリームソケット。`uses [io.net]` の関数だけ。`panics never` でも置ける。
    NetConnect { addr: VId },
    /// `TcpListener.accept()`: `accept`。結果は `Result<i32, std.io.Error>`。`uses [io.net]` の関数だけ。
    NetAccept { sock: VId },
    /// `TcpStream.read(buffer)`: `read(sock, buf, len)`。結果は `Result<usize, std.io.Error>`。`uses [io.net]` の関数だけ。
    NetRead { sock: VId, buf: VId, len: VId },
    /// `TcpStream.write(buffer)`: `write(sock, buf, len)`。結果は `Result<usize, std.io.Error>`。`uses [io.net]` の関数だけ。
    NetWrite { sock: VId, buf: VId, len: VId },
    /// `TcpStream.flush()`: `flush`。結果は `Result<(), std.io.Error>`。`uses [io.net]` の関数だけ。
    NetFlush { sock: VId },
    /// `TcpStream.close()` / `TcpListener.close()`: `close`。結果は `Result<(), std.io.Error>`。`uses [io.net]` の関数だけ。
    NetClose { sock: VId },
    /// `TcpListener.local_port()`: `local_addr` のポート。結果は `Result<u16, std.io.Error>`。`uses [io.net]` の関数だけ。
    NetLocalPort { sock: VId },
    /// `std.net` の原始操作 (`NetK`)。`args` の個数と型、結果の型は `NetK` ごとに決まる。`uses [io.net]` の関数だけ。
    /// 戻り値の失敗の型は `UdpBind` だけ `std.net.Error`、ほかは `std.io.Error`。
    NetSys { kind: NetK, args: Vec<VId> },
    /// `std.fs.create(path)`: 作成して切り詰め、書き込み専用で開く。`path` は fat (`&str`)。結果は `Result<std.fs.File, std.io.Error>`。
    /// 記述子はファイル。`uses [io.fs]` の関数だけ。
    FsCreate { path: VId },
    /// `std.fs.open(path)`: 読み取り専用で開く。`path` は fat (`&str`)。結果は `Result<std.fs.File, std.io.Error>`。
    /// `uses [io.fs]` の関数だけ。
    FsOpen { path: VId },
    /// `std.fs.exists(path)`: パスが存在すれば `true`。結果は `bool`。`uses [io.fs]` の関数だけ。
    FsExists { path: VId },
    /// `std.fs.remove_file(path)`: ファイルを削除する。結果は `Result<(), std.io.Error>`。`uses [io.fs]` の関数だけ。
    FsRemove { path: VId },
    /// `std.fs` のその他の原始操作 (`FsK`)。`args` の個数と結果の型は `FsK` ごとに決まる。`uses [io.fs]` の関数だけ。
    FsSys { kind: FsK, args: Vec<VId> },
    /// `std.fs.File.close()`: 記述子を閉じる。結果は `Result<(), std.io.Error>`。`uses [io.fs]` の関数だけ。
    FsClose { fd: VId },
    /// `std.fs.File.flush()`: OS へ永続化する (`sync_all`)。結果は `Result<(), std.io.Error>`。`uses [io.fs]` の関数だけ。
    FsFlush { fd: VId },
    /// `std.fs.File.read(buffer)`: `read(fd, buf, len)`。結果は `Result<usize, std.io.Error>`。`uses [io.fs]` の関数だけ。
    FsRead { fd: VId, buf: VId, len: VId },
    /// `std.fs.File.sync()`: OS へ永続化する (`sync_all`)。結果は `Result<(), std.io.Error>`。`uses [io.fs]` の関数だけ。
    FsSync { fd: VId },
    /// `std.fs.File.write(buffer)`: `write_all(fd, buf, len)`。結果は `Result<usize, std.io.Error>`。`uses [io.fs]` の関数だけ。
    FsWrite { fd: VId, buf: VId, len: VId },
    /// `std.env.arg_count()`: プログラムへの引数の個数 (プログラム名は含まない)。結果は `usize`。
    /// `uses [env]` の関数だけ。
    EnvArgCount,
    /// `std.env.arg(i)`: 第 `i` 個目の引数 (0 起算、プログラム名は含まない)。結果は `Option<&'static str>` (`Option<fat>`)。
    /// 範囲外なら `None`。`uses [env]` の関数だけ。
    EnvArg { index: VId },
    /// `std.env.var(name)`: 環境変数 `name` の値。結果は `Option<&'static str>` (`Option<fat>`)。未設定なら `None`。
    /// `uses [env]` の関数だけ。
    EnvVar { name: VId },
    /// `std.time.system_nanos()`: 実時刻 (UNIX 時刻からのナノ秒、`i64`)。`clock_gettime(CLOCK_REALTIME)`。
    /// 失敗時は 0。`uses [time]` の関数だけ。
    TimeNowNs,
    /// OS の乱数で `buf` (`ptr`) の `len` バイトを埋める (`getrandom(2)`、無ければ `/dev/urandom`。設計_KIR.md 26 章)。
    /// `fatal` が偽なら結果は `Result<(), std.io.Error>` (`std.rand.fill_secure`)。真なら結果は unit で、失敗は文言
    /// (`natives::ENTROPY_FAILED`) を出して終了コード 101 で止まる (`Rng.from_entropy`。`panics never` の関数で使える)。
    /// `uses [rand]` の関数だけ。暗号用途に使える。
    RandFill { buf: VId, len: VId, fatal: bool },
    /// panic の途中の文言を stderr へ書く (`data` = `fat`)。`write` と違って `uses` を要らない: 文言は panic の一部で、
    /// `core.check.eq` のような効果の宣言のない関数からも呼べるため (設計_KIR.md 13.2)。結果は unit。`panics may` のときだけ。
    Eprint { data: VId },
    /// 文言を出さずに終了コード 101 で終わる (`Eprint` で文言を書いた後の終わり)。結果は unit。`panics may` のときだけ。
    Abort,
    /// `std.process.exit(code)`: 終了コード `code` (i32) で戻らずに終わる。結果は unit (後続は `unreachable` で切る)。
    /// `uses [process]` の関数だけ。`panics never` の関数からも使える (panic の経路ではない)。
    Exit { code: VId },
    /// 浮動小数を `py_float` / `py_float32` と同じ規則で書式化して stderr へ出力する。
    /// 被演算子は `f32` または `f64`、結果は `unit`。`panics may` のときだけ。
    FmtFloat { bits: u32, a: VId },
    /// 浮動小数を `FmtFloat` と同じ規則で書式化して、`dst` (512 バイト以上の領域) へ書き、書いたバイト数 (`usize`) を返す。
    /// 被演算子は `f32` または `f64`。`String` へ追記する `Display` が使う (`push_display`)。
    FloatText { bits: u32, a: VId, dst: VId },
    /// `p` から `len` バイトを UTF-8 として検証し、正しい接頭辞の長さ (`valid_up_to`) を返す。全体が正しければ `len`。
    Utf8Valid { p: VId, len: VId },
    /// `core.num.<int>.from_str_radix(text, radix)`。`ty` は結果の整数型、`s` は `&str` (fat)、`radix` は `u32`。
    /// 結果は `Result<ty, core.num.ParseIntError>` (tag bit が 0 なら Ok でペイロードは整数、1 なら Err でペイロードは ParseIntError の kind)。
    /// `panics never` だけで使える (エラーは値として返す)。128 ビット整数は戻り値の型で表せる範囲だけ扱う。
    ParseInt { ty: Ty, s: VId, radix: VId },
}

/// `Op::Own` の種類 (設計_KIR.md 24 章)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnKind {
    /// 線形な値の束縛 (消費の義務が生じる)。再代入でも出る。
    Owe,
    /// 値渡し (move)。
    Consume,
    /// フィールドの move (義務は果たすが、別のフィールドの move は許す)。
    Part,
    /// 再代入 (move 済みが解ける。義務は変わらない)。
    Init,
    /// 読み (フィールドの読み・借用)。move 済みなら E3002 (義務は変わらない)。
    Use,
}

/// `BitCount` の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitK {
    Ones,
    Leading,
    Trailing,
}

impl BitK {
    pub fn name(self) -> &'static str {
        match self {
            BitK::Ones => "count_ones",
            BitK::Leading => "leading_zeros",
            BitK::Trailing => "trailing_zeros",
        }
    }

    pub fn parse(s: &str) -> Option<BitK> {
        [BitK::Ones, BitK::Leading, BitK::Trailing].into_iter().find(|k| k.name() == s)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Inst {
    pub dst: VId,
    pub ty: Ty,
    pub op: Op,
}

/// 制御の移動先: ブロック番号と引数。
pub type Target = (usize, Vec<VId>);

#[derive(Clone, Debug, PartialEq)]
pub enum Term {
    Br(Target),
    BrIf(VId, Target, Target),
    Ret(VId),
    /// 到達しないブロック (下ろしで合流先が無くなった場合など)。
    Unreachable,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub params: Vec<(VId, Ty)>,
    pub insts: Vec<Inst>,
    pub term: Term,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Func {
    /// 定義の FQN。
    pub fqn: String,
    /// 単相インスタンスの鍵 (段階 1 では FQN と同じ。型引数を持つ形は未対応)。
    pub key: String,
    /// 副作用の集合 (`uses`、整列済み)。
    pub uses: Vec<String>,
    /// `panics may` のとき true。`never` のとき false (trap 命令を置けない)。
    pub may_panic: bool,
    /// 宣言された `stack` の上界。
    pub stack: Option<u64>,
    pub ret: Ty,
    /// `extern` 宣言なら C のリンク名 (`lib:symbol` の `symbol` 部分)。ユーザ関数は `None`。
    pub link: Option<String>,
    /// ブロック 0 が入口。その引数が関数の引数。
    pub blocks: Vec<Block>,
}

impl Func {
    /// 関数の引数 (ブロック 0 の引数)。
    pub fn params(&self) -> &[(VId, Ty)] {
        &self.blocks[0].params
    }

    /// 関数の中で使う値番号の上限 + 1 (実行器のフレームの大きさ)。
    pub fn value_count(&self) -> usize {
        let mut n = 0u32;
        for b in &self.blocks {
            for (v, _) in &b.params {
                n = n.max(v + 1);
            }
            for i in &b.insts {
                n = n.max(i.dst + 1);
            }
        }
        n as usize
    }
}

/// 集成体の型の定義。配置は clif.rs の `layout` / `enum_layout` と同じ規則で求める (設計_KIR.md 2.3、7 章 4)。
#[derive(Clone, Debug, PartialEq)]
pub struct AggDef {
    /// 構造体・列挙型は定義の FQN、タプルは `(i32,bool)` の形。
    pub name: String,
    pub size: u32,
    pub align: u32,
    pub kind: AggKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AggKind {
    Struct(Vec<Field>),
    Tuple(Vec<Field>),
    /// 固定長配列 `[T; N]`: 要素 `T` を `N` 個、隙間なく並べたもの (要素は所有物を持たない)。大きさは `N * size(T)`。
    Array(Ty, u32),
    /// 先頭 4 バイトがタグ (バリアントの添字を i32 で書く)。各バリアントのペイロードは、タグの後ろに重ねて置く。
    Enum(Vec<Variant>),
}

/// フィールド: 名前 (タプルと列挙型のペイロードでは空)、型、バイトオフセット。
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    pub off: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Module {
    /// 鍵の順 (UTF-8 順) に並べる。
    pub funcs: Vec<Func>,
    /// 集成体の型表 (`Ty::Agg` の添字)。定義は構成要素より前に並ぶ。
    pub aggs: Vec<AggDef>,
    /// 入口の主スレッドの呼び出しの深さの上限 (入口の `depth M`)。0 は既定 (`kir_exec::MAX_DEPTH`)。
    pub depth: u64,
}

impl Module {
    pub fn find(&self, key: &str) -> Option<&Func> {
        self.funcs.iter().find(|f| f.key == key)
    }

    /// `key` から呼び出しで到達できる関数だけを取り出した部分モジュール (鍵の順は保つ)。
    pub fn reachable(&self, key: &str) -> Module {
        self.reachable_from(&[key])
    }

    /// `keys` のどれかから呼び出しで到達できる関数だけの部分モジュール。
    pub fn reachable_from(&self, keys: &[&str]) -> Module {
        let mut seen: HashSet<String> = HashSet::new();
        let mut todo: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        while let Some(k) = todo.pop() {
            if !seen.insert(k.clone()) {
                continue;
            }
            if let Some(f) = self.find(&k) {
                for b in &f.blocks {
                    for i in &b.insts {
                        if let Op::Call { callee, .. } = &i.op {
                            todo.push(callee.clone());
                        }
                        if let Op::FnPtr { callee } = &i.op {
                            todo.push(callee.clone());
                        }
                    }
                }
            }
        }
        let mut m = Module {
            funcs: self.funcs.iter().filter(|f| seen.contains(&f.key)).cloned().collect(),
            aggs: self.aggs.clone(),
            depth: self.depth,
        };
        prune_aggs(&mut m);
        m
    }
}

/// 集成体の大きさ (バイト)。`Ty::Agg` は型表を引く。
pub fn size_of(aggs: &[AggDef], t: Ty) -> u32 {
    match t {
        Ty::I8 | Ty::U8 | Ty::Bool => 1,
        Ty::I16 | Ty::U16 => 2,
        Ty::I32 | Ty::U32 | Ty::F32 => 4,
        Ty::I64 | Ty::U64 | Ty::F64 => 8,
        // `usize` / `isize` / 生ポインタ / 参照はターゲットのポインタの幅 (既定 64 ビット、wasm32 は 32 ビット)。
        Ty::Isize | Ty::Usize | Ty::Ptr => crate::ptrwidth::bits() / 8,
        Ty::I128 | Ty::U128 => 16,
        Ty::Unit => 0,
        // 参照は集成体のフィールドに来ない (`field_ty` が拒否する)。値の幅だけ与える。
        Ty::Ref(..) => crate::ptrwidth::bits() / 8,
        // `Option` / `Result` は「枠 (ペイロードの語の幅) + タグ 1 ビット」の整数 (`opt_slot_bits`)。フィールドに置くときは
        // その整数を 8 の倍数のバイト数に広げる (枠の後ろにタグのバイトが来る: 設計_KIR.md 11.1 / 26 章)。
        Ty::Opt(_) | Ty::Res(..) => {
            let bits = 1 + opt_slot_bits(aggs, t);
            (bits.div_ceil(8)).div_ceil(8) * 8
        }
        Ty::Agg(i) => aggs[i as usize].size,
    }
}

/// `Option` / `Result` の枠の幅 (ビット)。ペイロードのどれもが入る幅で、64 ビット以上 (設計_KIR.md 11.1 と LLVM の `slot_bits` と同じ)。
/// 集成体は `size * 8` (0 のときも 8)、入れ子の `Option` / `Result` は タグを含む幅 (`1 + 枠`)。
pub fn opt_slot_bits(aggs: &[AggDef], t: Ty) -> u32 {
    fn pay_bits(aggs: &[AggDef], t: Ty) -> u32 {
        match t {
            Ty::Agg(i) => aggs[i as usize].size.max(1) * 8,
            Ty::Opt(_) | Ty::Res(..) => 1 + opt_slot_bits(aggs, t),
            Ty::Bool => 1,
            t => size_of(aggs, t) * 8,
        }
    }
    match t {
        Ty::Opt(p) => pay_bits(aggs, p.ty()).max(64),
        Ty::Res(a, e) => pay_bits(aggs, a.ty()).max(pay_bits(aggs, e.ty())).max(64),
        _ => 64,
    }
}

/// 集成体の整列 (バイト)。スカラーは大きさと同じ (clif.rs の `scalar_size` と同じ)。
/// `Option` / `Result` は 8 バイトの整列 (枠の語が 8 バイトの単位で置かれる)。
pub fn align_of(aggs: &[AggDef], t: Ty) -> u32 {
    match t {
        Ty::Unit => 1,
        Ty::Agg(i) => aggs[i as usize].align,
        Ty::Opt(_) | Ty::Res(..) => 8,
        t => size_of(aggs, t),
    }
}

/// 型表から、関数に現れない集成体を外し、残りを元の添字の順に振り直す。
/// 集成体の構成要素の型も参照として数える。
pub fn prune_aggs(m: &mut Module) {
    let n = m.aggs.len();
    let mut used = vec![false; n];
    let mut todo: Vec<u32> = vec![];
    fn mark(t: Ty, used: &mut [bool], todo: &mut Vec<u32>) {
        match t {
            Ty::Agg(i) => {
                if !used[i as usize] {
                    used[i as usize] = true;
                    todo.push(i);
                }
            }
            // 集成体の指す先の参照 (`ref<agg>`) も、その集成体を使っている
            Ty::Ref(_, Pointee::Agg(i)) => mark(Ty::Agg(i), used, todo),
            Ty::Ref(_, Pointee::Opt(p)) => mark(Ty::Opt(p), used, todo),
            // `Option` / `Result` のペイロードに入る集成体も使われている
            Ty::Opt(p) => mark(p.ty(), used, todo),
            Ty::Res(t0, e0) => {
                mark(t0.ty(), used, todo);
                mark(e0.ty(), used, todo);
            }
            _ => {}
        }
    }
    for f in &m.funcs {
        mark(f.ret, &mut used, &mut todo);
        for b in &f.blocks {
            for (_, t) in &b.params {
                mark(*t, &mut used, &mut todo);
            }
            for i in &b.insts {
                mark(i.ty, &mut used, &mut todo);
                // 命令の中に持つ型も使われている (付け替えの対象と同じ並び。`remap` の下の注を見る)。
                for t in op_tys(&i.op) {
                    mark(t, &mut used, &mut todo);
                }
            }
        }
    }
    while let Some(i) = todo.pop() {
        let fields: Vec<Ty> = match &m.aggs[i as usize].kind {
            AggKind::Struct(fs) | AggKind::Tuple(fs) => fs.iter().map(|f| f.ty).collect(),
            AggKind::Array(t, _) => vec![*t],
            AggKind::Enum(vs) => vs.iter().flat_map(|v| v.fields.iter().map(|f| f.ty)).collect(),
        };
        for t in fields {
            mark(t, &mut used, &mut todo);
        }
    }
    let mut map = vec![u32::MAX; n];
    let mut next = 0u32;
    for i in 0..n {
        if used[i] {
            map[i] = next;
            next += 1;
        }
    }
    let remap = |t: Ty| t.map_aggs(&|i: u32| map[i as usize]);
    let old = std::mem::take(&mut m.aggs);
    for (i, mut d) in old.into_iter().enumerate() {
        if !used[i] {
            continue;
        }
        match &mut d.kind {
            AggKind::Struct(fs) | AggKind::Tuple(fs) => fs.iter_mut().for_each(|f| f.ty = remap(f.ty)),
            AggKind::Array(t, _) => *t = remap(*t),
            AggKind::Enum(vs) => vs.iter_mut().for_each(|v| v.fields.iter_mut().for_each(|f| f.ty = remap(f.ty))),
        }
        m.aggs.push(d);
    }
    for f in &mut m.funcs {
        f.ret = remap(f.ret);
        for b in &mut f.blocks {
            for (_, t) in &mut b.params {
                *t = remap(*t);
            }
            for i in &mut b.insts {
                i.ty = remap(i.ty);
                // 命令が持つ型 (参照から ptr への bitcast の元の型、比較・メモリ書き込みの型など) も付け替える。
                // 付け替えが漏れると、添字が古いまま残り、VM などが型表の外を読む (2026-11-14 の `std.net` の追加で見つかった)。
                match &mut i.op {
                    Op::Bitcast { src, .. } | Op::Conv { src, .. } | Op::FUn { src, .. } | Op::BitCount { src, .. } => *src = remap(*src),
                    Op::Cmp { ty, .. } | Op::MemStore { ty, .. } | Op::ParseInt { ty, .. } => *ty = remap(*ty),
                    _ => {}
                }
            }
        }
    }
}

/// 命令が持つ型 (`Ty`) の一覧。`prune_aggs` が使われている型を数えるのと、付け替えの対象を一致させるために使う。
fn op_tys(op: &Op) -> Vec<Ty> {
    match op {
        Op::Bitcast { src, .. } | Op::Conv { src, .. } | Op::FUn { src, .. } | Op::BitCount { src, .. } => vec![*src],
        Op::Cmp { ty, .. } | Op::MemStore { ty, .. } | Op::ParseInt { ty, .. } => vec![*ty],
        _ => vec![],
    }
}

// ---------------------------------------------------------------- テキスト形式 (出力)

fn fmt_val(v: VId) -> String {
    format!("%{}", v)
}

fn fmt_vals(vs: &[VId]) -> String {
    vs.iter().map(|v| fmt_val(*v)).collect::<Vec<_>>().join(", ")
}

fn fmt_target(t: &Target) -> String {
    format!("b{}({})", t.0, fmt_vals(&t.1))
}

/// 型の表示。集成体は `agg#<添字>` と書く (定義は `print_module` の先頭の `agg#` 行)。
pub fn fmt_ty(t: Ty) -> String {
    match t {
        Ty::Agg(i) => format!("agg#{}", i),
        t => t.name().to_string(),
    }
}

fn fmt_agg(i: usize, d: &AggDef) -> String {
    let fmt_fields = |fs: &[Field]| -> String {
        fs.iter()
            .map(|f| {
                let nm = if f.name.is_empty() { String::new() } else { format!("{}:", f.name) };
                format!("{}{}@{}", nm, fmt_ty(f.ty), f.off)
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (kind, body) = match &d.kind {
        AggKind::Struct(fs) => ("struct", fmt_fields(fs)),
        AggKind::Tuple(fs) => ("tuple", fmt_fields(fs)),
        AggKind::Array(t, n) => ("array", format!("{}x{}", fmt_ty(*t), n)),
        AggKind::Enum(vs) => (
            "enum",
            vs.iter()
                .map(|v| {
                    let ps: Vec<String> = v.fields.iter().map(|f| format!("{}@{}", fmt_ty(f.ty), f.off)).collect();
                    format!("{}({})", v.name, ps.join(","))
                })
                .collect::<Vec<_>>()
                .join(" "),
        ),
    };
    format!("agg#{} {} {} size={} align={} {{{}}}", i, kind, d.name, d.size, d.align, body)
}

/// 浮動小数の定数を、読み直すと同じ値になる十進の形で出す (`{:?}` は最短の往復表記)。
fn fmt_float_const(ty: Ty, bits: i128) -> String {
    match ty {
        Ty::F32 => format!("{:?}", f32::from_bits(bits as u32)),
        _ => format!("{:?}", f64::from_bits(bits as u64)),
    }
}

fn fmt_inst(i: &Inst) -> String {
    let body = match &i.op {
        Op::Const(v) => {
            let lit = match i.ty {
                Ty::Bool => (if *v != 0 { "true" } else { "false" }).to_string(),
                Ty::Unit => "()".to_string(),
                Ty::U128 => (*v as u128).to_string(),
                Ty::F32 | Ty::F64 => fmt_float_const(i.ty, *v),
                Ty::Opt(_) => "none".to_string(),
                _ => v.to_string(),
            };
            format!("const.{} {}", i.ty.name(), lit)
        }
        Op::Some(a) => format!("some {}", fmt_val(*a)),
        Op::Ok(a) => format!("ok {}", fmt_val(*a)),
        Op::Err(a) => format!("err {}", fmt_val(*a)),
        Op::Tag(a) => format!("tag {}", fmt_val(*a)),
        Op::Payload(a) => format!("payload {}", fmt_val(*a)),
        Op::Checked { op, a, b } => format!("checked.{} {}", op.name(), fmt_vals(&[*a, *b])),
        Op::Arith { op, mode, a, b } => {
            let m = match mode {
                Mode::Wrap => "wrap",
                Mode::Trap => "trap",
                Mode::Sat => "sat",
            };
            format!("{}.{}.{} {}", op.name(), i.ty.name(), m, fmt_vals(&[*a, *b]))
        }
        Op::Cmp { cc, ty, a, b } => {
            let p = if ty.is_float() { "fcmp" } else { "icmp" };
            format!("{}.{}.{} {}", p, cc.name(), ty.name(), fmt_vals(&[*a, *b]))
        }
        Op::Neg { a, mode } => {
            let m = if *mode == Mode::Wrap { "wrap" } else { "trap" };
            format!("neg.{}.{} {}", i.ty.name(), m, fmt_val(*a))
        }
        Op::FArith { op, a, b } => format!("f{}.{} {}", op.name(), i.ty.name(), fmt_vals(&[*a, *b])),
        Op::Shift { left, a, n } => {
            let m = if *left { "shl" } else { "shr" };
            format!("{}.{}.wrap {}", m, i.ty.name(), fmt_vals(&[*a, *n]))
        }
        Op::MinMax { max, a, b } => {
            let m = if *max { "max" } else { "min" };
            format!("{}.{} {}", m, i.ty.name(), fmt_vals(&[*a, *b]))
        }
        Op::FUn { f, src, a } => format!("{}.{} {}", f.name(), src.name(), fmt_val(*a)),
        Op::FBin { f, a, b } => format!("{}.{} {}", f.name(), i.ty.name(), fmt_vals(&[*a, *b])),
        Op::Conv { src, a } => format!("conv.{}.{} {}", i.ty.name(), src.name(), fmt_val(*a)),
        Op::Bitcast { src, a } => format!("bitcast.{}.{} {}", i.ty.name(), src.name(), fmt_val(*a)),
        Op::Not(a) => format!("not.{} {}", i.ty.name(), fmt_val(*a)),
        Op::Bool(op, a, b) => {
            let n = match op {
                BoolOp::And => "and",
                BoolOp::Or => "or",
                BoolOp::Xor => "xor",
            };
            format!("{}.{} {}", n, i.ty.name(), fmt_vals(&[*a, *b]))
        }
        Op::BitCount { k, src, a } => format!("{}.{} {}", k.name(), src.name(), fmt_val(*a)),
        Op::Call { callee, args } => format!("call {}({})", callee, fmt_vals(args)),
        Op::FnPtr { callee } => format!("fnptr {}", callee),
        Op::Make { variant, fields } => {
            let head = match variant {
                Some(k) => format!("make.v{}", k),
                None => "make".to_string(),
            };
            format!("{}({})", head, fmt_vals(fields))
        }
        Op::Field { a, off } => format!("field {}, +{}", fmt_val(*a), off),
        Op::SetField { a, off, v } => format!("setfield {}, +{}, {}", fmt_val(*a), off, fmt_val(*v)),
        Op::Alloca => "alloca".to_string(),
        Op::Load(p) => format!("load {}", fmt_val(*p)),
        Op::Store { ptr, val } => format!("store {}", fmt_vals(&[*ptr, *val])),
        Op::FieldAddr { a, off } => format!("fieldaddr {}, +{}", fmt_val(*a), off),
        Op::AsShared(p) => format!("asshared {}", fmt_val(*p)),
        Op::Reborrow(p) => format!("reborrow {}", fmt_val(*p)),
        Op::Borrow { a, mutable } => format!("borrow.{} {}", if *mutable { "mut" } else { "shared" }, fmt_val(*a)),
        Op::ScopeEnd(a) => format!("scope.end {}", fmt_val(*a)),
        Op::Carry { a, b } => format!("carry {}, {}", fmt_val(*a), fmt_val(*b)),
        Op::Own { a, kind } => format!("own.{} {}", match kind { OwnKind::Owe => "owe", OwnKind::Consume => "consume", OwnKind::Part => "part", OwnKind::Init => "init", OwnKind::Use => "use" }, fmt_val(*a)),
        Op::FrameBytes { size } => format!("frame.bytes {}", size),
        Op::GlobalBytes { name, size } => format!("global.bytes {} {}", name, size),
        Op::StaticBytes(bytes) => {
            let hex: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
            if hex.is_empty() {
                "static.bytes".to_string()
            } else {
                format!("static.bytes {}", hex)
            }
        }
        Op::PtrAdd { p, off } => format!("ptr.add {}, {}", fmt_val(*p), fmt_val(*off)),
        Op::Utf8Valid { p, len } => format!("utf8.valid {}, {}", fmt_val(*p), fmt_val(*len)),
        Op::MemLoad { p, off } => format!("mem.load.{} {} +{}", fmt_ty(i.ty), fmt_val(*p), off),
        Op::MemStore { ty, p, off, v } => format!("mem.store.{} {} +{}, {}", fmt_ty(*ty), fmt_val(*p), off, fmt_val(*v)),
        Op::HeapAlloc(n) => format!("heap.alloc {}", fmt_val(*n)),
        Op::HeapRealloc { p, size } => format!("heap.realloc {}, {}", fmt_val(*p), fmt_val(*size)),
        Op::HeapFree(p) => format!("heap.free {}", fmt_val(*p)),
        Op::IndexCheck { i, len } => format!("index.check {}, {}", fmt_val(*i), fmt_val(*len)),
        Op::Panic { msg, arg } => match arg {
            Some(v) => format!("panic {} {}", quote_msg(msg), fmt_val(*v)),
            None => format!("panic {}", quote_msg(msg)),
        },
        Op::Write { fd, data, nl } => format!("write{} {}, {}", if *nl { ".nl" } else { "" }, fmt_val(*fd), fmt_val(*data)),
        Op::RandFill { buf, len, fatal } => format!("rand.fill{} {}, {}", if *fatal { ".fatal" } else { "" }, fmt_val(*buf), fmt_val(*len)),
        Op::NetListen { addr } => format!("net.listen {}", fmt_val(*addr)),
        Op::NetConnect { addr } => format!("net.connect {}", fmt_val(*addr)),
        Op::NetAccept { sock } => format!("net.accept {}", fmt_val(*sock)),
        Op::NetRead { sock, buf, len } => {
            format!("net.read {}, {}, {}", fmt_val(*sock), fmt_val(*buf), fmt_val(*len))
        }
        Op::NetWrite { sock, buf, len } => {
            format!("net.write {}, {}, {}", fmt_val(*sock), fmt_val(*buf), fmt_val(*len))
        }
        Op::NetFlush { sock } => format!("net.flush {}", fmt_val(*sock)),
        Op::NetClose { sock } => format!("net.close {}", fmt_val(*sock)),
        Op::NetLocalPort { sock } => format!("net.local_port {}", fmt_val(*sock)),
        Op::NetSys { kind, args } => format!("net.{} {}", kind.name(), args.iter().map(|a| fmt_val(*a)).collect::<Vec<_>>().join(", ")),
        Op::Proc { f, args } => format!("proc.{} {}", f.name(), args.iter().map(|a| fmt_val(*a)).collect::<Vec<_>>().join(", ")),
        Op::FsCreate { path } => format!("fs.create {}", fmt_val(*path)),
        Op::FsOpen { path } => format!("fs.open {}", fmt_val(*path)),
        Op::FsExists { path } => format!("fs.exists {}", fmt_val(*path)),
        Op::FsRemove { path } => format!("fs.remove_file {}", fmt_val(*path)),
        Op::FsSys { kind, args } => format!("fs.{} {}", kind.name(), args.iter().map(|a| fmt_val(*a)).collect::<Vec<_>>().join(", ")),
        Op::FsClose { fd } => format!("fs.close {}", fmt_val(*fd)),
        Op::FsFlush { fd } => format!("fs.flush {}", fmt_val(*fd)),
        Op::FsRead { fd, buf, len } => format!("fs.read {}, {}, {}", fmt_val(*fd), fmt_val(*buf), fmt_val(*len)),
        Op::FsSync { fd } => format!("fs.sync {}", fmt_val(*fd)),
        Op::FsWrite { fd, buf, len } => format!("fs.write {}, {}, {}", fmt_val(*fd), fmt_val(*buf), fmt_val(*len)),
        Op::Exit { code } => format!("exit {}", fmt_val(*code)),
        Op::EnvArgCount => "env.arg_count".to_string(),
        Op::EnvArg { index } => format!("env.arg {}", fmt_val(*index)),
        Op::EnvVar { name } => format!("env.var {}", fmt_val(*name)),
        Op::TimeNowNs => "time.now_ns".to_string(),
        Op::Eprint { data } => format!("eprint {}", fmt_val(*data)),
        Op::Abort => "abort".to_string(),
        Op::FmtFloat { bits, a } => format!("fmtfloat.{bits} {}", fmt_val(*a)),
        Op::FloatText { bits, a, dst } => format!("floattext.{bits} {}", fmt_vals(&[*a, *dst])),
        Op::ParseInt { ty, s, radix } => format!("parseint.{} {}", ty.name(), fmt_vals(&[*s, *radix])),
    };
    format!("%{}: {} = {}", i.dst, fmt_ty(i.ty), body)
}

fn fmt_term(t: &Term) -> String {
    match t {
        Term::Br(tg) => format!("br {}", fmt_target(tg)),
        Term::BrIf(c, a, b) => format!("br_if {}, {}, {}", fmt_val(*c), fmt_target(a), fmt_target(b)),
        Term::Ret(v) => format!("ret {}", fmt_val(*v)),
        Term::Unreachable => "unreachable".to_string(),
    }
}

/// 1 関数を `kir 1` の本文として出す (版の行は付けない)。
pub fn print_func(f: &Func) -> String {
    let mut s = String::new();
    let uses = f.uses.join(",");
    let stack = f.stack.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string());
    s.push_str(&format!(
        "fn {} key={} uses=[{}] panics={} stack={} ret={}\n",
        f.fqn,
        f.key,
        uses,
        if f.may_panic { "may" } else { "never" },
        stack,
        fmt_ty(f.ret)
    ));
    for (bi, b) in f.blocks.iter().enumerate() {
        let ps: Vec<String> = b.params.iter().map(|(v, t)| format!("%{}: {}", v, fmt_ty(*t))).collect();
        s.push_str(&format!("  b{}({}):\n", bi, ps.join(", ")));
        for i in &b.insts {
            s.push_str("    ");
            s.push_str(&fmt_inst(i));
            s.push('\n');
        }
        s.push_str("    ");
        s.push_str(&fmt_term(&b.term));
        s.push('\n');
    }
    s.push_str(&format!("end {}\n", f.fqn));
    s
}

/// モジュール全体を出す。先頭に版の行、関数ごとに空行を挟む。
pub fn print_module(m: &Module) -> String {
    let mut s = format!("{}\n", VERSION_LINE);
    if m.depth != 0 {
        s.push_str(&format!("depth {}\n", m.depth));
    }
    if !m.aggs.is_empty() {
        s.push('\n');
        for (i, d) in m.aggs.iter().enumerate() {
            s.push_str(&fmt_agg(i, d));
            s.push('\n');
        }
    }
    for f in &m.funcs {
        s.push('\n');
        s.push_str(&print_func(f));
    }
    s
}

// ---------------------------------------------------------------- テキスト形式 (読み込み)

fn perr(line: usize, msg: impl Into<String>) -> String {
    format!("kir: 行 {}: {}", line, msg.into())
}

fn parse_val(s: &str, line: usize) -> Result<VId, String> {
    let body = s.trim().strip_prefix('%').ok_or_else(|| perr(line, format!("値番号 {} が % で始まらない", s)))?;
    body.parse::<VId>().map_err(|_| perr(line, format!("値番号 {} が不正", s)))
}

fn parse_vals(s: &str, line: usize) -> Result<Vec<VId>, String> {
    if s.trim().is_empty() {
        return Ok(vec![]);
    }
    s.split(',').map(|x| parse_val(x, line)).collect()
}

/// `,` で割る。`<...>` の中の `,` (`Result<i64,bool>`) では割らない。
fn split_top(s: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn parse_ty(s: &str, line: usize) -> Result<Ty, String> {
    let s = s.trim();
    if let Some(n) = s.strip_prefix("agg#") {
        return n.parse::<u32>().map(Ty::Agg).map_err(|_| perr(line, format!("集成体の番号 {} が不正", s)));
    }
    Ty::parse(s).ok_or_else(|| perr(line, format!("型 {} は KIR の段階 1 にない", s)))
}

fn parse_off(s: &str, line: usize) -> Result<u32, String> {
    s.trim().strip_prefix('+').and_then(|x| x.parse::<u32>().ok()).ok_or_else(|| perr(line, format!("オフセット {} が不正", s)))
}

/// 集成体の命令 (`make` / `field` / `setfield` / `tag`) の右辺を読む。対象外なら `None`。
fn parse_agg_rhs(rhs: &str, line: usize) -> Option<Result<Op, String>> {
    if let Some(r) = rhs.strip_prefix("make") {
        if r.starts_with('(') || r.starts_with(".v") {
            let (variant, args) = match r.strip_prefix(".v") {
                Some(x) => {
                    let Some(open) = x.find('(') else { return Some(Err(perr(line, "make に ( がない"))) };
                    let Ok(k) = x[..open].parse::<u32>() else { return Some(Err(perr(line, "バリアントの番号が不正"))) };
                    (Some(k), &x[open..])
                }
                None => (None, r),
            };
            let Some(inner) = args.strip_prefix('(').and_then(|x| x.strip_suffix(')')) else {
                return Some(Err(perr(line, "make の引数の形が不正")));
            };
            return Some(parse_vals(inner, line).map(|fields| Op::Make { variant, fields }));
        }
    }
    if let Some(r) = rhs.strip_prefix("field ") {
        let p: Vec<&str> = r.split(',').collect();
        if p.len() != 2 {
            return Some(Err(perr(line, "field の形が不正")));
        }
        return Some(parse_val(p[0], line).and_then(|a| Ok(Op::Field { a, off: parse_off(p[1], line)? })));
    }
    if let Some(r) = rhs.strip_prefix("fieldaddr ") {
        let p: Vec<&str> = r.split(',').collect();
        if p.len() != 2 {
            return Some(Err(perr(line, "fieldaddr の形が不正")));
        }
        return Some(parse_val(p[0], line).and_then(|a| Ok(Op::FieldAddr { a, off: parse_off(p[1], line)? })));
    }
    if let Some(r) = rhs.strip_prefix("setfield ") {
        let p: Vec<&str> = r.split(',').collect();
        if p.len() != 3 {
            return Some(Err(perr(line, "setfield の形が不正")));
        }
        return Some(
            parse_val(p[0], line).and_then(|a| Ok(Op::SetField { a, off: parse_off(p[1], line)?, v: parse_val(p[2], line)? })),
        );
    }
    None
}

/// `b<N>(<args>)` を読む。
fn parse_target(s: &str, line: usize) -> Result<Target, String> {
    let s = s.trim();
    let rest = s.strip_prefix('b').ok_or_else(|| perr(line, format!("移動先 {} が b で始まらない", s)))?;
    let open = rest.find('(').ok_or_else(|| perr(line, format!("移動先 {} に ( がない", s)))?;
    let close = rest.rfind(')').ok_or_else(|| perr(line, format!("移動先 {} に ) がない", s)))?;
    let n = rest[..open].parse::<usize>().map_err(|_| perr(line, format!("移動先の番号 {} が不正", s)))?;
    Ok((n, parse_vals(&rest[open + 1..close], line)?))
}

fn parse_aop(s: &str) -> Option<Aop> {
    Some(match s {
        "add" => Aop::Add,
        "sub" => Aop::Sub,
        "mul" => Aop::Mul,
        "div" => Aop::Div,
        "rem" => Aop::Rem,
        _ => return None,
    })
}

fn parse_cc(s: &str) -> Option<Cc> {
    Some(match s {
        "eq" => Cc::Eq,
        "ne" => Cc::Ne,
        "lt" => Cc::Lt,
        "le" => Cc::Le,
        "gt" => Cc::Gt,
        "ge" => Cc::Ge,
        _ => return None,
    })
}

/// 命令の右辺 (`<mnemonic> <operands>` または `call f(args)`) を、結果の型 `ty` のもとで読む。
fn parse_rhs(rhs: &str, ty: Ty, line: usize) -> Result<Op, String> {
    if let Some(r) = parse_agg_rhs(rhs, line) {
        return r;
    }
    if let Some(r) = rhs.strip_prefix("call ") {
        let open = r.find('(').ok_or_else(|| perr(line, "call に ( がない"))?;
        let close = r.rfind(')').ok_or_else(|| perr(line, "call に ) がない"))?;
        return Ok(Op::Call { callee: r[..open].to_string(), args: parse_vals(&r[open + 1..close], line)? });
    }
    if let Some(r) = rhs.strip_prefix("fnptr ") {
        if ty != Ty::Ptr {
            return Err(perr(line, format!("fnptr の結果は ptr ではない")));
        }
        return Ok(Op::FnPtr { callee: r.to_string() });
    }
    let (head, rest) = rhs.split_once(' ').unwrap_or((rhs, ""));
    let mut parts = head.split('.');
    let mn = parts.next().unwrap_or("");
    let suffix: Vec<&str> = parts.collect();
    let operands = |n: usize| -> Result<Vec<VId>, String> {
        let v = parse_vals(rest, line)?;
        if v.len() != n {
            return Err(perr(line, format!("{} の被演算子は {} 個のはず", head, n)));
        }
        Ok(v)
    };
    // 結果の型の接尾辞 (`const.T`、`add.T.wrap` など) は結果の型と一致しなければならない。
    let same_ty = |t: &str| -> Result<(), String> {
        match Ty::parse(t) {
            Some(x) if x == ty => Ok(()),
            _ => Err(perr(line, format!("{} の型が結果の型 {} と合わない", head, ty.name()))),
        }
    };
    match (mn, suffix.as_slice()) {
        ("const", [t]) => {
            same_ty(t)?;
            let v = match (ty, rest.trim()) {
                (Ty::Bool, "true") => 1,
                (Ty::Bool, "false") => 0,
                (Ty::Unit, "()") => 0,
                (Ty::Ptr, x) => x.parse::<i128>().map_err(|_| perr(line, format!("ポインタの定数 {} が不正", x)))?,
                (Ty::U128, x) => x.parse::<u128>().map_err(|_| perr(line, format!("整数 {} が不正", x)))? as i128,
                (Ty::Opt(_), "none") => 0,
                (t, x) if t.is_int() => x.parse::<i128>().map_err(|_| perr(line, format!("整数 {} が不正", x)))?,
                (Ty::F32, x) => x.parse::<f32>().map_err(|_| perr(line, format!("浮動小数 {} が不正", x)))?.to_bits() as i128,
                (Ty::F64, x) => x.parse::<f64>().map_err(|_| perr(line, format!("浮動小数 {} が不正", x)))?.to_bits() as i128,
                _ => return Err(perr(line, format!("型 {} の定数 {} が不正", ty.name(), rest))),
            };
            Ok(Op::Const(v))
        }
        ("some", []) => Ok(Op::Some(operands(1)?[0])),
        ("ok", []) => Ok(Op::Ok(operands(1)?[0])),
        ("err", []) => Ok(Op::Err(operands(1)?[0])),
        ("tag", []) => Ok(Op::Tag(operands(1)?[0])),
        ("payload", []) => Ok(Op::Payload(operands(1)?[0])),
        ("checked", [name]) if parse_aop(name).is_some() => {
            let v = operands(2)?;
            Ok(Op::Checked { op: parse_aop(name).unwrap(), a: v[0], b: v[1] })
        }
        (p @ ("icmp" | "fcmp"), [cc, t]) => {
            let cc = parse_cc(cc).ok_or_else(|| perr(line, format!("比較 {} が不正", cc)))?;
            let oty = parse_ty(t, line)?;
            if (p == "fcmp") != oty.is_float() {
                return Err(perr(line, format!("{} の被演算子の型 {} と命令が合わない", p, oty.name())));
            }
            let v = operands(2)?;
            Ok(Op::Cmp { cc, ty: oty, a: v[0], b: v[1] })
        }
        ("neg", [t, m]) if matches!(*m, "trap" | "wrap") => {
            same_ty(t)?;
            let mode = if *m == "wrap" { Mode::Wrap } else { Mode::Trap };
            Ok(Op::Neg { a: operands(1)?[0], mode })
        }
        ("not", [t]) => {
            same_ty(t)?;
            Ok(Op::Not(operands(1)?[0]))
        }
        (name @ ("and" | "or" | "xor"), [t]) => {
            same_ty(t)?;
            let op = match name {
                "and" => BoolOp::And,
                "or" => BoolOp::Or,
                _ => BoolOp::Xor,
            };
            let v = operands(2)?;
            Ok(Op::Bool(op, v[0], v[1]))
        }
        (name, [t, m]) if parse_aop(name).is_some() && matches!(*m, "wrap" | "trap" | "sat") => {
            same_ty(t)?;
            let v = operands(2)?;
            let mode = match *m {
                "wrap" => Mode::Wrap,
                "trap" => Mode::Trap,
                _ => Mode::Sat,
            };
            Ok(Op::Arith { op: parse_aop(name).unwrap(), mode, a: v[0], b: v[1] })
        }
        (name, [t]) if name.starts_with('f') && matches!(parse_aop(&name[1..]), Some(Aop::Add | Aop::Sub | Aop::Mul | Aop::Div)) => {
            same_ty(t)?;
            let v = operands(2)?;
            Ok(Op::FArith { op: parse_aop(&name[1..]).unwrap(), a: v[0], b: v[1] })
        }
        (name @ ("shl" | "shr"), [t, "wrap"]) => {
            same_ty(t)?;
            let v = operands(2)?;
            Ok(Op::Shift { left: name == "shl", a: v[0], n: v[1] })
        }
        (name @ ("max" | "min"), [t]) if Ty::parse(t).is_some_and(|x| x.is_int()) => {
            same_ty(t)?;
            let v = operands(2)?;
            Ok(Op::MinMax { max: name == "max", a: v[0], b: v[1] })
        }
        (name, [t]) if FUn::parse(name).is_some() => {
            let f = FUn::parse(name).unwrap();
            let src = parse_ty(t, line)?;
            if !src.is_float() {
                return Err(perr(line, format!("{} の被演算子は浮動小数のはず", name)));
            }
            if matches!(f, FUn::IsNan | FUn::IsFinite) {
                if ty != Ty::Bool {
                    return Err(perr(line, format!("{} の結果は bool のはず", name)));
                }
            } else {
                same_ty(t)?;
            }
            Ok(Op::FUn { f, src, a: operands(1)?[0] })
        }
        (name, [t]) if FBin::parse(name).is_some() => {
            same_ty(t)?;
            let v = operands(2)?;
            Ok(Op::FBin { f: FBin::parse(name).unwrap(), a: v[0], b: v[1] })
        }
        (name, [t]) if BitK::parse(name).is_some() => {
            let k = BitK::parse(name).unwrap();
            let src = parse_ty(t, line)?;
            if !src.is_int() || ty != Ty::U32 {
                return Err(perr(line, format!("{} は整数の被演算子と u32 の結果のはず", name)));
            }
            Ok(Op::BitCount { k, src, a: operands(1)?[0] })
        }
        ("conv" | "bitcast", [dst, src]) => {
            same_ty(dst)?;
            let src = parse_ty(src, line)?;
            let a = operands(1)?[0];
            Ok(if mn == "conv" { Op::Conv { src, a } } else { Op::Bitcast { src, a } })
        }
        ("alloca", []) => Ok(Op::Alloca),
        ("load", []) => Ok(Op::Load(operands(1)?[0])),
        ("store", []) => {
            let v = operands(2)?;
            Ok(Op::Store { ptr: v[0], val: v[1] })
        }
        ("asshared", []) => Ok(Op::AsShared(operands(1)?[0])),
        ("reborrow", []) => Ok(Op::Reborrow(operands(1)?[0])),
        ("borrow", ["shared"]) => Ok(Op::Borrow { a: operands(1)?[0], mutable: false }),
        ("borrow", ["mut"]) => Ok(Op::Borrow { a: operands(1)?[0], mutable: true }),
        ("scope", ["end"]) if ty == Ty::Unit => Ok(Op::ScopeEnd(operands(1)?[0])),
        ("carry", []) if ty == Ty::Unit => {
            let v = operands(2)?;
            Ok(Op::Carry { a: v[0], b: v[1] })
        }
        ("own", ["owe"]) if ty == Ty::Unit => Ok(Op::Own { a: operands(1)?[0], kind: OwnKind::Owe }),
        ("own", ["consume"]) if ty == Ty::Unit => Ok(Op::Own { a: operands(1)?[0], kind: OwnKind::Consume }),
        ("own", ["init"]) if ty == Ty::Unit => Ok(Op::Own { a: operands(1)?[0], kind: OwnKind::Init }),
        ("own", ["part"]) if ty == Ty::Unit => Ok(Op::Own { a: operands(1)?[0], kind: OwnKind::Part }),
        ("own", ["use"]) if ty == Ty::Unit => Ok(Op::Own { a: operands(1)?[0], kind: OwnKind::Use }),
        ("frame", ["bytes"]) if ty == Ty::Ptr => {
            let n = rest.trim().parse::<u32>().map_err(|_| perr(line, format!("frame.bytes の大きさ {} が不正", rest)))?;
            Ok(Op::FrameBytes { size: n })
        }
        ("global", ["bytes"]) if ty == Ty::Ptr => {
            let mut it = rest.split_whitespace();
            let (Some(name), Some(n), None) = (it.next(), it.next(), it.next()) else {
                return Err(perr(line, "global.bytes は名前と大きさを取る"));
            };
            let size = n.parse::<u32>().map_err(|_| perr(line, format!("global.bytes の大きさ {} が不正", n)))?;
            Ok(Op::GlobalBytes { name: name.to_string(), size })
        }
        ("static", ["bytes"]) if ty == Ty::Ptr => {
            let t = rest.trim();
            if t.len() % 2 != 0 {
                return Err(perr(line, "static.bytes の 16 進数が奇数長"));
            }
            let bytes: Result<Vec<u8>, String> = (0..t.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&t[i..i + 2], 16).map_err(|_| perr(line, format!("static.bytes の 16 進数 {} が不正", t))))
                .collect();
            Ok(Op::StaticBytes(bytes?))
        }
        ("ptr", ["add"]) if ty == Ty::Ptr => {
            let v = operands(2)?;
            Ok(Op::PtrAdd { p: v[0], off: v[1] })
        }
        ("utf8", ["valid"]) if ty == Ty::Usize => {
            let v = operands(2)?;
            Ok(Op::Utf8Valid { p: v[0], len: v[1] })
        }
        ("mem", ["load", t]) => {
            same_ty(t)?;
            let (p, off) = parse_mem(rest, line)?;
            Ok(Op::MemLoad { p, off })
        }
        ("mem", ["store", t]) => {
            let vty = parse_ty(t, line)?;
            if ty != Ty::Unit {
                return Err(perr(line, "mem.store の結果は unit のはず"));
            }
            let (m, v) = rest.split_once(',').ok_or_else(|| perr(line, "mem.store に , がない"))?;
            let (p, off) = parse_mem(m, line)?;
            Ok(Op::MemStore { ty: vty, p, off, v: parse_val(v, line)? })
        }
        ("heap", ["free"]) if ty == Ty::Unit => Ok(Op::HeapFree(parse_val(rest, line)?)),
        ("heap", ["alloc"]) if ty == Ty::Ptr => Ok(Op::HeapAlloc(parse_val(rest, line)?)),
        ("heap", ["realloc"]) if ty == Ty::Ptr => {
            let v = operands(2)?;
            Ok(Op::HeapRealloc { p: v[0], size: v[1] })
        }
        ("write", []) => {
            let v = operands(2)?;
            Ok(Op::Write { fd: v[0], data: v[1], nl: false })
        }
        ("write", ["nl"]) => {
            let v = operands(2)?;
            Ok(Op::Write { fd: v[0], data: v[1], nl: true })
        }
        ("net", ["listen"]) => {
            let v = operands(1)?;
            Ok(Op::NetListen { addr: v[0] })
        }
        ("net", ["connect"]) => {
            let v = operands(1)?;
            Ok(Op::NetConnect { addr: v[0] })
        }
        ("net", ["accept"]) => {
            let v = operands(1)?;
            Ok(Op::NetAccept { sock: v[0] })
        }
        ("net", ["read"]) => {
            let v = operands(3)?;
            Ok(Op::NetRead { sock: v[0], buf: v[1], len: v[2] })
        }
        ("net", ["write"]) => {
            let v = operands(3)?;
            Ok(Op::NetWrite { sock: v[0], buf: v[1], len: v[2] })
        }
        ("net", ["flush"]) => {
            let v = operands(1)?;
            Ok(Op::NetFlush { sock: v[0] })
        }
        ("net", ["close"]) => {
            let v = operands(1)?;
            Ok(Op::NetClose { sock: v[0] })
        }
        ("net", ["local_port"]) => {
            let v = operands(1)?;
            Ok(Op::NetLocalPort { sock: v[0] })
        }
        ("net", [k]) if NetK::ALL.iter().any(|f| f.name() == *k) => {
            let kind = *NetK::ALL.iter().find(|f| f.name() == *k).expect("種別");
            let v = operands(kind.arity())?;
            Ok(Op::NetSys { kind, args: v })
        }
        ("proc", [name]) => {
            let Some(f) = ProcFn::parse(name) else { return Err(perr(line, format!("proc.{} は不明", name))) };
            let v = operands(f.arity())?;
            Ok(Op::Proc { f, args: v })
        }
        ("fs", ["create"]) => {
            let v = operands(1)?;
            Ok(Op::FsCreate { path: v[0] })
        }
        ("fs", ["open"]) => {
            let v = operands(1)?;
            Ok(Op::FsOpen { path: v[0] })
        }
        ("fs", ["exists"]) => {
            let v = operands(1)?;
            Ok(Op::FsExists { path: v[0] })
        }
        ("fs", ["remove_file"]) => {
            let v = operands(1)?;
            Ok(Op::FsRemove { path: v[0] })
        }
        ("fs", [k]) if FsK::ALL.iter().any(|f| f.name() == *k) => {
            let kind = *FsK::ALL.iter().find(|f| f.name() == *k).expect("種別");
            let v = operands(kind.arity())?;
            Ok(Op::FsSys { kind, args: v })
        }
        ("fs", ["close"]) => {
            let v = operands(1)?;
            Ok(Op::FsClose { fd: v[0] })
        }
        ("fs", ["flush"]) => {
            let v = operands(1)?;
            Ok(Op::FsFlush { fd: v[0] })
        }
        ("fs", ["read"]) => {
            let v = operands(3)?;
            Ok(Op::FsRead { fd: v[0], buf: v[1], len: v[2] })
        }
        ("fs", ["sync"]) => {
            let v = operands(1)?;
            Ok(Op::FsSync { fd: v[0] })
        }
        ("fs", ["write"]) => {
            let v = operands(3)?;
            Ok(Op::FsWrite { fd: v[0], buf: v[1], len: v[2] })
        }
        ("rand", ["fill"]) => {
            let v = operands(2)?;
            Ok(Op::RandFill { buf: v[0], len: v[1], fatal: false })
        }
        ("rand", ["fill", "fatal"]) => {
            let v = operands(2)?;
            Ok(Op::RandFill { buf: v[0], len: v[1], fatal: true })
        }
        ("env", ["arg_count"]) => {
            let _ = operands(0)?;
            Ok(Op::EnvArgCount)
        }
        ("env", ["arg"]) => {
            let v = operands(1)?;
            Ok(Op::EnvArg { index: v[0] })
        }
        ("env", ["var"]) => {
            let v = operands(1)?;
            Ok(Op::EnvVar { name: v[0] })
        }
        ("time", ["now_ns"]) => {
            let _ = operands(0)?;
            Ok(Op::TimeNowNs)
        }
        ("eprint", []) if ty == Ty::Unit => {
            let v = operands(1)?;
            Ok(Op::Eprint { data: v[0] })
        }
        ("abort", []) if ty == Ty::Unit => Ok(Op::Abort),
        ("exit", []) if ty == Ty::Unit => {
            let v = operands(1)?;
            Ok(Op::Exit { code: v[0] })
        }
        ("fmtfloat", [bits]) if ty == Ty::Unit => {
            let bits = bits.parse::<u32>().map_err(|_| perr(line, "fmtfloat のビット数は 32 または 64"))?;
            if bits != 32 && bits != 64 {
                return Err(perr(line, "fmtfloat のビット数は 32 または 64"));
            }
            let v = operands(1)?;
            Ok(Op::FmtFloat { bits, a: v[0] })
        }
        ("floattext", [bits]) if ty == Ty::Usize => {
            let bits = bits.parse::<u32>().map_err(|_| perr(line, "floattext のビット数は 32 または 64"))?;
            if bits != 32 && bits != 64 {
                return Err(perr(line, "floattext のビット数は 32 または 64"));
            }
            let v = operands(2)?;
            Ok(Op::FloatText { bits, a: v[0], dst: v[1] })
        }
        ("parseint", [t]) => {
            let Some(ty) = Ty::parse(t) else {
                return Err(perr(line, format!("parseint の型 {} は不明", t)));
            };
            if !ty.is_int() {
                return Err(perr(line, format!("parseint の型 {} は整数ではない", ty.name())));
            }
            let vs = operands(2)?;
            Ok(Op::ParseInt { ty, s: vs[0], radix: vs[1] })
        }
        ("index", ["check"]) if ty == Ty::Unit => {
            let v = operands(2)?;
            Ok(Op::IndexCheck { i: v[0], len: v[1] })
        }
        ("panic", []) if ty == Ty::Unit => parse_panic(rest, line),
        _ => Err(perr(line, format!("命令 {} が不明", head))),
    }
}

/// panic の文言を引用符で囲んだ形 (`"..."`)。`"` `\` と制御文字だけ書き換える (`\"` `\\` `\n` `\xHH`)。
fn quote_msg(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 || c == '\x7f' => o.push_str(&format!("\\x{:02x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// `"文言"` か `"文言" %v` (`quote_msg` の逆)。
fn parse_panic(s: &str, line: usize) -> Result<Op, String> {
    let body = s.trim_start().strip_prefix('"').ok_or_else(|| perr(line, "panic の文言が引用符で始まらない"))?;
    let b = body.as_bytes();
    let mut out: Vec<u8> = vec![];
    let mut i = 0;
    loop {
        let Some(&c) = b.get(i) else { return Err(perr(line, "panic の文言が閉じていない")) };
        match c {
            b'"' => {
                i += 1;
                break;
            }
            b'\\' => {
                let Some(&e) = b.get(i + 1) else { return Err(perr(line, "panic の文言の書き方が不正")) };
                match e {
                    b'\\' | b'"' => out.push(e),
                    b'n' => out.push(b'\n'),
                    b'x' => {
                        let h = body.get(i + 2..i + 4).ok_or_else(|| perr(line, "panic の \\x の書き方が不正"))?;
                        out.push(u8::from_str_radix(h, 16).map_err(|_| perr(line, "panic の \\x の値が不正"))?);
                        i += 2;
                    }
                    _ => return Err(perr(line, "panic の文言の書き方が不正")),
                }
                i += 2;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    let msg = String::from_utf8(out).map_err(|_| perr(line, "panic の文言が UTF-8 でない"))?;
    let rest = body[i..].trim();
    let arg = if rest.is_empty() { None } else { Some(parse_val(rest, line)?) };
    Ok(Op::Panic { msg, arg })
}

/// `%p +off` (メモリの番地) を読む。
fn parse_mem(s: &str, line: usize) -> Result<(VId, u32), String> {
    let (p, off) = s.trim().split_once(" +").ok_or_else(|| perr(line, format!("番地 {} が %p +off の形でない", s)))?;
    let off = off.trim().parse::<u32>().map_err(|_| perr(line, format!("オフセット {} が不正", off)))?;
    Ok((parse_val(p, line)?, off))
}

fn parse_inst(line_no: usize, text: &str) -> Result<Inst, String> {
    // text = "%n: T = 右辺"
    let (lhs, rhs) = text.split_once(" = ").ok_or_else(|| perr(line_no, "命令に ` = ` がない"))?;
    let (dst_s, ty_s) = lhs.split_once(':').ok_or_else(|| perr(line_no, "命令に `%n:` がない"))?;
    let dst = parse_val(dst_s, line_no)?;
    let ty = parse_ty(ty_s, line_no)?;
    let op = parse_rhs(rhs.trim(), ty, line_no)?;
    Ok(Inst { dst, ty, op })
}

fn parse_term(line_no: usize, text: &str) -> Result<Term, String> {
    if text == "unreachable" {
        return Ok(Term::Unreachable);
    }
    if let Some(r) = text.strip_prefix("ret ") {
        return Ok(Term::Ret(parse_val(r, line_no)?));
    }
    if let Some(r) = text.strip_prefix("br_if ") {
        let (c, rest) = r.split_once(',').ok_or_else(|| perr(line_no, "br_if に , がない"))?;
        let close = rest.find(')').ok_or_else(|| perr(line_no, "br_if の移動先に ) がない"))?;
        let t1 = parse_target(&rest[..=close], line_no)?;
        let t2 = parse_target(rest[close + 1..].trim_start_matches(',').trim(), line_no)?;
        return Ok(Term::BrIf(parse_val(c, line_no)?, t1, t2));
    }
    if let Some(r) = text.strip_prefix("br ") {
        return Ok(Term::Br(parse_target(r, line_no)?));
    }
    Err(perr(line_no, format!("終端命令 {} が不明", text)))
}

/// `kir 1` のモジュールを読む。版が違えば拒否する。
pub fn parse_module(src: &str) -> Result<Module, String> {
    let lines: Vec<(usize, &str)> = src
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim_end()))
        .filter(|(_, l)| !l.trim().is_empty())
        .collect();
    let mut it = lines.into_iter().peekable();
    match it.next() {
        Some((_, l)) if l == VERSION_LINE => {}
        Some((n, l)) => return Err(perr(n, format!("版が {} ではない (読めるのは {})", l, VERSION_LINE))),
        None => return Err("kir: 空の入力".to_string()),
    }
    let mut depth = 0u64;
    if let Some(&(n, l)) = it.peek() {
        if let Some(v) = l.strip_prefix("depth ") {
            depth = v.trim().parse::<u64>().map_err(|_| perr(n, "depth の値が不正"))?;
            it.next();
        }
    }
    // 集成体の型表 (版の行の直後に `agg#<添字>` の行が並ぶ)。
    let mut aggs: Vec<AggDef> = vec![];
    while let Some(&(n, l)) = it.peek() {
        if !l.starts_with("agg#") {
            break;
        }
        it.next();
        aggs.push(parse_agg_decl(n, l, aggs.len())?);
    }
    let mut funcs = vec![];
    while let Some((n, l)) = it.next() {
        let Some(hdr) = l.strip_prefix("fn ") else {
            return Err(perr(n, format!("関数の定義 fn が必要 (見つかったのは {})", l.trim())));
        };
        let mut toks = hdr.split_whitespace();
        let fqn = toks.next().ok_or_else(|| perr(n, "fn に名前がない"))?.to_string();
        let mut key = None;
        let mut uses = None;
        let mut may = None;
        let mut stack = None;
        let mut ret = None;
        for t in toks {
            let (k, v) = t.split_once('=').ok_or_else(|| perr(n, format!("ヘッダの {} が key=value でない", t)))?;
            match k {
                "key" => key = Some(v.to_string()),
                "uses" => {
                    let inner = v.strip_prefix('[').and_then(|x| x.strip_suffix(']')).ok_or_else(|| perr(n, "uses が [..] でない"))?;
                    uses = Some(if inner.is_empty() { vec![] } else { inner.split(',').map(|x| x.to_string()).collect() });
                }
                "panics" => {
                    may = Some(match v {
                        "may" => true,
                        "never" => false,
                        _ => return Err(perr(n, format!("panics の値 {} が不正", v))),
                    })
                }
                "stack" => {
                    stack = Some(if v == "-" {
                        None
                    } else {
                        Some(v.parse::<u64>().map_err(|_| perr(n, format!("stack の値 {} が不正", v)))?)
                    })
                }
                "ret" => ret = Some(parse_ty(v, n)?),
                _ => return Err(perr(n, format!("ヘッダの項目 {} が不明", k))),
            }
        }
        let f_key = key.ok_or_else(|| perr(n, "key がない"))?;
        let f_uses = uses.ok_or_else(|| perr(n, "uses がない"))?;
        let f_may = may.ok_or_else(|| perr(n, "panics がない"))?;
        let f_stack = stack.ok_or_else(|| perr(n, "stack がない"))?;
        let f_ret = ret.ok_or_else(|| perr(n, "ret がない"))?;
        let mut blocks: Vec<Block> = vec![];
        let mut cur: Option<(Vec<(VId, Ty)>, Vec<Inst>)> = None;
        loop {
            let Some((ln, text)) = it.next() else {
                return Err(perr(n, format!("関数 {} の end がない", fqn)));
            };
            let t = text.trim();
            if t == format!("end {}", fqn) {
                if cur.is_some() {
                    return Err(perr(ln, "終端命令のない ブロック がある"));
                }
                break;
            }
            if let Some(rest) = t.strip_prefix('b').filter(|r| r.contains('(') && r.ends_with(':')) {
                if cur.is_some() {
                    return Err(perr(ln, "前のブロックに終端命令がない"));
                }
                let open = rest.find('(').unwrap();
                let idx: usize = rest[..open].parse().map_err(|_| perr(ln, "ブロック番号が不正"))?;
                if idx != blocks.len() {
                    return Err(perr(ln, format!("ブロック番号 b{} が順番どおりでない", idx)));
                }
                let inner = &rest[open + 1..rest.len() - 2];
                let mut ps = vec![];
                if !inner.trim().is_empty() {
                    for p in split_top(inner) {
                        let (v, ty) = p.split_once(':').ok_or_else(|| perr(ln, "ブロック引数の形が不正"))?;
                        ps.push((parse_val(v, ln)?, parse_ty(ty, ln)?));
                    }
                }
                cur = Some((ps, vec![]));
                continue;
            }
            if cur.is_none() {
                return Err(perr(ln, "ブロックの前に命令がある"));
            }
            if t.starts_with('%') {
                let inst = parse_inst(ln, t)?;
                cur.as_mut().unwrap().1.push(inst);
            } else {
                let term = parse_term(ln, t)?;
                let (ps, insts) = cur.take().unwrap();
                blocks.push(Block { params: ps, insts, term });
            }
        }
        if blocks.is_empty() {
            return Err(perr(n, format!("関数 {} にブロックがない", fqn)));
        }
        let mut uses_v: Vec<String> = f_uses;
        uses_v.sort();
        funcs.push(Func {
            fqn,
            key: f_key,
            uses: uses_v,
            may_panic: f_may,
            stack: f_stack,
            ret: f_ret,
            link: None,
            blocks,
        });
    }
    Ok(Module { funcs, aggs, depth })
}

/// `agg#<k> <kind> <名前> size=<n> align=<n> {<本体>}` を読む (`k` は期待する添字)。
fn parse_agg_decl(line_no: usize, text: &str, k: usize) -> Result<AggDef, String> {
    let (head, body) = text.split_once(" {").ok_or_else(|| perr(line_no, "集成体の行に { がない"))?;
    let body = body.strip_suffix('}').ok_or_else(|| perr(line_no, "集成体の行が } で終わらない"))?;
    let toks: Vec<&str> = head.split_whitespace().collect();
    if toks.len() != 5 || toks[0] != format!("agg#{}", k) {
        return Err(perr(line_no, format!("集成体の宣言 {} の形が不正 (期待は agg#{})", head, k)));
    }
    let size = toks[3].strip_prefix("size=").and_then(|x| x.parse::<u32>().ok()).ok_or_else(|| perr(line_no, "size が不正"))?;
    let align = toks[4].strip_prefix("align=").and_then(|x| x.parse::<u32>().ok()).ok_or_else(|| perr(line_no, "align が不正"))?;
    let name = toks[2].to_string();
    // 構造体は `name:ty@off`、タプルは `ty@off` を空白で区切る。
    let fields = |body: &str| -> Result<Vec<Field>, String> {
        body.split_whitespace()
            .map(|t| {
                let (nm, rest) = match t.split_once(':') {
                    Some((n, r)) => (n.to_string(), r),
                    None => (String::new(), t),
                };
                let (ty, off) = rest.split_once('@').ok_or_else(|| perr(line_no, format!("フィールド {} の形が不正", t)))?;
                let off = off.parse::<u32>().map_err(|_| perr(line_no, format!("オフセット {} が不正", off)))?;
                Ok(Field { name: nm, ty: parse_ty(ty, line_no)?, off })
            })
            .collect()
    };
    let kind = match toks[1] {
        "struct" => AggKind::Struct(fields(body)?),
        "tuple" => AggKind::Tuple(fields(body)?),
        "array" => {
            let (t, n) = body.trim().rsplit_once('x').ok_or_else(|| perr(line_no, "配列の本体の形が不正 (<型>x<個数>)"))?;
            AggKind::Array(parse_ty(t, line_no)?, n.parse::<u32>().map_err(|_| perr(line_no, "配列の個数が不正"))?)
        }
        "enum" => {
            let mut vs = vec![];
            for t in body.split_whitespace() {
                let open = t.find('(').ok_or_else(|| perr(line_no, format!("バリアント {} に ( がない", t)))?;
                let inner = t[open + 1..]
                    .strip_suffix(')')
                    .ok_or_else(|| perr(line_no, format!("バリアント {} に ) がない", t)))?;
                let mut fs = vec![];
                if !inner.is_empty() {
                    for p in inner.split(',') {
                        let (ty, off) = p.split_once('@').ok_or_else(|| perr(line_no, format!("ペイロード {} の形が不正", p)))?;
                        let off = off.parse::<u32>().map_err(|_| perr(line_no, format!("オフセット {} が不正", off)))?;
                        fs.push(Field { name: String::new(), ty: parse_ty(ty, line_no)?, off });
                    }
                }
                vs.push(Variant { name: t[..open].to_string(), fields: fs });
            }
            AggKind::Enum(vs)
        }
        other => return Err(perr(line_no, format!("集成体の種類 {} が不明", other))),
    };
    Ok(AggDef { name, size, align, kind })
}

// ---------------------------------------------------------------- 検証

/// 関数 1 つの検査。値の一意性と使用前の定義、型、ブロック引数、効果を調べる。
fn verify_func(m: &Module, f: &Func) -> Result<(), String> {
    let fail = |msg: String| Err(format!("{}: {}", f.key, msg));
    if f.blocks.is_empty() {
        return fail("ブロックがない".into());
    }
    // 型に現れる集成体の番号 (Option / Result の中も) が型表にあるか
    let all_tys = std::iter::once(f.ret).chain(f.blocks.iter().flat_map(|b| b.params.iter().map(|(_, t)| *t).chain(b.insts.iter().map(|i| i.ty))));
    for t in all_tys {
        check_agg_ids(m, t).map_err(|e| format!("{}: {}", f.key, e))?;
    }
    let mut tys: HashMap<VId, Ty> = HashMap::new();
    let mut def = |v: VId, t: Ty| -> Result<(), String> {
        if tys.insert(v, t).is_some() {
            return Err(format!("{}: %{} が二重に定義されている", f.key, v));
        }
        Ok(())
    };
    for b in &f.blocks {
        for (v, t) in &b.params {
            def(*v, *t)?;
        }
        for i in &b.insts {
            def(i.dst, i.ty)?;
        }
    }
    let ty_of = |v: VId| -> Result<Ty, String> {
        tys.get(&v).copied().ok_or_else(|| format!("{}: %{} が定義されていない", f.key, v))
    };
    // 定数の定義 (除数が正の定数の `/` `%` は 0 除算も `MIN / -1` も起こさないので trap しない)。
    let consts: HashMap<VId, i128> = f
        .blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .filter_map(|i| if let Op::Const(v) = &i.op { Some((i.dst, *v)) } else { None })
        .collect();
    let callees: HashMap<&str, &Func> = m.funcs.iter().map(|g| (g.key.as_str(), g)).collect();
    let fu: HashSet<&str> = f.uses.iter().map(|s| s.as_str()).collect();
    let check_target = |t: &Target| -> Result<(), String> {
        let b = f.blocks.get(t.0).ok_or_else(|| format!("{}: 移動先 b{} がない", f.key, t.0))?;
        if b.params.len() != t.1.len() {
            return fail(format!("b{} へ渡す引数が {} 個、期待は {} 個", t.0, t.1.len(), b.params.len()));
        }
        for (a, (_, pt)) in t.1.iter().zip(b.params.iter()) {
            if ty_of(*a)? != *pt {
                return fail(format!("b{} の引数の型が合わない (%{})", t.0, a));
            }
        }
        Ok(())
    };
    for b in &f.blocks {
        for i in &b.insts {
            let res = (|| -> Result<(), String> {
                match &i.op {
                    Op::Const(v) => {
                        if matches!(i.ty, Ty::Ref(..)) {
                            return Err("参照の定数は作れない (ポインタは null だけ。参照は alloca / as_shared などで作る)".into());
                        }
                        if matches!(i.ty, Ty::Agg(_)) {
                            return Err("集成体の型の定数は作れない".into());
                        }
                        if matches!(i.ty, Ty::Res(..)) {
                            return Err("Result の定数はない (Ok / Err で作る)".into());
                        }
                        if matches!(i.ty, Ty::Opt(_)) && *v != 0 {
                            return Err("Option の定数は none (0) だけ".into());
                        }
                        if i.ty == Ty::Ptr && *v != 0 {
                            return Err("ポインタの定数は null (0) だけ".into());
                        }
                    }
                    Op::Some(a) => {
                        let Ty::Opt(p) = i.ty else { return Err("some の結果は Option のはず".into()) };
                        if ty_of(*a)? != p.ty() {
                            return Err("some の被演算子の型が Option の要素と合わない".into());
                        }
                    }
                    Op::Ok(a) => {
                        let Ty::Res(t, _) = i.ty else { return Err("ok の結果は Result のはず".into()) };
                        if ty_of(*a)? != t.ty() {
                            return Err("ok の被演算子の型が Result の Ok 側と合わない".into());
                        }
                    }
                    Op::Err(a) => {
                        let Ty::Res(_, e) = i.ty else { return Err("err の結果は Result のはず".into()) };
                        if ty_of(*a)? != e.ty() {
                            return Err("err の被演算子の型が Result の Err 側と合わない".into());
                        }
                    }
                    Op::Tag(a) => match ty_of(*a)? {
                        Ty::Agg(ai) => {
                            let d = m.aggs.get(ai as usize).ok_or("集成体の番号が表にない")?;
                            if i.ty != Ty::I32 || !matches!(d.kind, AggKind::Enum(_)) {
                                return Err("tag は列挙型の i32 のはず".into());
                            }
                        }
                        Ty::Opt(_) | Ty::Res(..) => {
                            if i.ty != Ty::Bool {
                                return Err("tag の結果は bool のはず".into());
                            }
                        }
                        _ => return Err("tag の被演算子は集成体の列挙型・Option・Result のはず".into()),
                    },
                    Op::Payload(a) => {
                        let ok = match ty_of(*a)? {
                            Ty::Opt(p) => p.ty() == i.ty,
                            Ty::Res(t, e) => t.ty() == i.ty || e.ty() == i.ty,
                            _ => false,
                        };
                        if !ok {
                            return Err("payload の結果の型が被演算子のペイロードの型と合わない".into());
                        }
                    }
                    Op::Checked { a, b, .. } => {
                        let Ty::Opt(p) = i.ty else { return Err("checked の結果は Option のはず".into()) };
                        if ty_of(*a)? != p.ty() || ty_of(*b)? != p.ty() {
                            return Err("checked の被演算子の型が Option の要素と合わない".into());
                        }
                    }
                    Op::Arith { op, mode, a, b } => {
                        if !i.ty.is_int() {
                            return Err(format!("算術の結果の型 {} は整数ではない", i.ty.name()));
                        }
                        if *mode != Mode::Trap && !matches!(op, Aop::Add | Aop::Sub | Aop::Mul) {
                            return Err(format!("{} に {} は使えない", op.name(), if *mode == Mode::Wrap { "wrap" } else { "sat" }));
                        }
                        let safe_div = matches!(op, Aop::Div | Aop::Rem) && consts.get(b).map_or(false, |&c| c > 0);
                        if *mode == Mode::Trap && !f.may_panic && !safe_div {
                            return Err("panics never の関数に trap 付きの算術".into());
                        }
                        if ty_of(*a)? != i.ty || ty_of(*b)? != i.ty {
                            return Err("算術の被演算子の型が結果と合わない".into());
                        }
                    }
                    Op::FArith { op, a, b } => {
                        if !i.ty.is_float() || !matches!(op, Aop::Add | Aop::Sub | Aop::Mul | Aop::Div) {
                            return Err("浮動小数の算術は f32 / f64 の + - * / だけ".into());
                        }
                        if ty_of(*a)? != i.ty || ty_of(*b)? != i.ty {
                            return Err("浮動小数の被演算子の型が結果と合わない".into());
                        }
                    }
                    Op::Shift { a, n, .. } => {
                        if !i.ty.is_int() || ty_of(*a)? != i.ty || ty_of(*n)? != Ty::U32 {
                            return Err("シフトは整数どうし (量は u32) のはず".into());
                        }
                    }
                    Op::MinMax { a, b, .. } => {
                        if !i.ty.is_int() || ty_of(*a)? != i.ty || ty_of(*b)? != i.ty {
                            return Err("max / min は同じ整数型のはず".into());
                        }
                    }
                    Op::FUn { f: fun, src, a } => {
                        if !src.is_float() || ty_of(*a)? != *src {
                            return Err("浮動小数の単項演算の被演算子の型が合わない".into());
                        }
                        let want = if matches!(fun, FUn::IsNan | FUn::IsFinite) { Ty::Bool } else { *src };
                        if i.ty != want {
                            return Err(format!("{} の結果の型は {} のはず", fun.name(), want.name()));
                        }
                    }
                    Op::FBin { f: fb, a, b } => {
                        if !i.ty.is_float() || ty_of(*a)? != i.ty {
                            return Err("浮動小数の 2 引数演算の被演算子の型が合わない".into());
                        }
                        let bt = if *fb == FBin::Powi { Ty::I32 } else { i.ty };
                        if ty_of(*b)? != bt {
                            return Err(format!("{} の第 2 引数の型は {} のはず", fb.name(), bt.name()));
                        }
                    }
                    Op::Conv { src, a } => {
                        if ty_of(*a)? != *src || !(conv_ok(*src, i.ty) || float_to_int_ok(*src, i.ty)) {
                            return Err(format!("{} から {} への変換は使えない", src.name(), i.ty.name()));
                        }
                    }
                    Op::Bitcast { src, a } => {
                        // 参照 (集成体への) と ptr の相互変換: 構造体のフィールドの Vec / String のヘッダを借用のヘッダとして扱う (設計_KIR.md 13.7)。
                        let ok = matches!((src, i.ty), (Ty::F64, Ty::U64) | (Ty::U64, Ty::F64) | (Ty::F32, Ty::U32) | (Ty::U32, Ty::F32))
                            || matches!((src, i.ty), (Ty::Ref(..), Ty::Ptr) | (Ty::Ptr, Ty::Ref(..)))
                            // 参照どうしは同じ指す先なら機械語では同じ (アドレス) なので読み替えを許す (`&T` → `&mut T` の受け手など)。
                            || matches!((src, i.ty), (Ty::Ref(_, p), Ty::Ref(_, q)) if *p == q);
                        if !ok || ty_of(*a)? != *src {
                            return Err(format!("{} から {} へのビット再解釈は使えない", src.name(), i.ty.name()));
                        }
                    }
                    Op::Cmp { cc, ty, a, b } => {
                        if i.ty != Ty::Bool {
                            return Err("比較の結果は bool のはず".into());
                        }
                        if !ty.is_scalar() || *ty == Ty::Unit || (*ty == Ty::Ptr && !matches!(cc, Cc::Eq | Cc::Ne)) || (*ty == Ty::Bool && !matches!(cc, Cc::Eq | Cc::Ne)) {
                            return Err(format!("型 {} の比較 {} は使えない", fmt_ty(*ty), cc.name()));
                        }
                        if ty_of(*a)? != *ty || ty_of(*b)? != *ty {
                            return Err("比較の被演算子の型が合わない".into());
                        }
                    }
                    Op::BitCount { src, a, .. } => {
                        if !src.is_int() || i.ty != Ty::U32 || ty_of(*a)? != *src {
                            return Err("count_ones などは整数の被演算子と u32 の結果のはず".into());
                        }
                    }
                    Op::Neg { a, mode } => {
                        if !i.ty.is_int() {
                            return Err("単項 - は整数の型のはず".into());
                        }
                        match mode {
                            Mode::Trap => {
                                if !i.ty.signed() {
                                    return Err("符号なし整数の単項 - は使えない".into());
                                }
                                if !f.may_panic {
                                    return Err("panics never の関数に trap 付きの neg".into());
                                }
                            }
                            Mode::Wrap => {}
                            Mode::Sat => return Err("neg に sat は使えない".into()),
                        }
                        if ty_of(*a)? != i.ty {
                            return Err("neg の被演算子の型が合わない".into());
                        }
                    }
                    Op::Not(a) => {
                        if !(i.ty == Ty::Bool || i.ty.is_int()) || ty_of(*a)? != i.ty {
                            return Err("not は bool か整数のはず".into());
                        }
                    }
                    Op::Bool(_, a, b) => {
                        if !(i.ty == Ty::Bool || i.ty.is_int()) || ty_of(*a)? != i.ty || ty_of(*b)? != i.ty {
                            return Err("論理・ビット演算は同じ bool か整数の型のはず".into());
                        }
                    }
                    Op::Alloca => {
                        if !matches!(i.ty, Ty::Ref(true, _)) {
                            return Err("alloca の結果は mutref のはず".into());
                        }
                    }
                    Op::Load(p) => {
                        let Ty::Ref(_, q) = ty_of(*p)? else {
                            return Err("load の被演算子は参照のはず".into());
                        };
                        if q.ty() != i.ty {
                            return Err("load の結果の型が指す先の型と合わない".into());
                        }
                    }
                    Op::Store { ptr, val } => {
                        if i.ty != Ty::Unit {
                            return Err("store の結果は unit のはず".into());
                        }
                        let vt = ty_of(*val)?;
                        match ty_of(*ptr)? {
                            Ty::Ref(true, q) if q.ty() == vt => {}
                            Ty::Ref(false, _) => return Err("共有参照 ref<T> への store は使えない".into()),
                            _ => return Err("store の被演算子の型が合わない".into()),
                        }
                    }
                    Op::Borrow { a, .. } => {
                        if ty_of(*a)? != i.ty || !matches!(i.ty, Ty::Ptr | Ty::Ref(..)) {
                            return Err("borrow は ptr / 参照を同じ型で複製するだけのはず".into());
                        }
                    }
                    Op::Own { a, .. } => {
                        ty_of(*a)?;
                        if i.ty != Ty::Unit {
                            return Err("own.* の結果は unit のはず".into());
                        }
                    }
                    Op::ScopeEnd(a) => {
                        ty_of(*a)?;
                        if i.ty != Ty::Unit {
                            return Err("scope.end の結果は unit のはず".into());
                        }
                    }
                    Op::Carry { a, b } => {
                        let ta = ty_of(*a)?;
                        ty_of(*b)?;
                        if !(ta.is_ref() || ta == Ty::Ptr) || i.ty != Ty::Unit {
                            return Err("carry は参照 (a) と値 (b) を取り、結果は unit のはず".into());
                        }
                    }
                    Op::Reborrow(a) => {
                        if !matches!(i.ty, Ty::Ref(true, _)) || ty_of(*a)? != i.ty {
                            return Err("reborrow は mutref を同じ型で複製するだけのはず".into());
                        }
                    }
                    Op::FieldAddr { a, off } => {
                        let Ty::Ref(mt, Pointee::Agg(ai)) = ty_of(*a)? else {
                            return Err("fieldaddr の対象は集成体への参照のはず".into());
                        };
                        let d = m.aggs.get(ai as usize).ok_or("集成体の番号が表にない")?;
                        if matches!(d.kind, AggKind::Enum(_)) {
                            return Err("fieldaddr は構造体・タプルだけ (列挙型には使わない)".into());
                        }
                        let Ty::Ref(rm, q) = i.ty else {
                            return Err("fieldaddr の結果は参照のはず".into());
                        };
                        if rm != mt || !has_field(d, *off, q.ty()) {
                            return Err(format!("{} に +{} の {} のフィールドがない (可変性も同じはず)", d.name, off, fmt_ty(q.ty())));
                        }
                    }
                    Op::AsShared(a) => {
                        let Ty::Ref(false, q) = i.ty else {
                            return Err("asshared の結果は ref のはず".into());
                        };
                        if ty_of(*a)? != Ty::Ref(true, q) {
                            return Err("asshared の被演算子は mutref で、指す先が同じはず".into());
                        }
                    }
                    Op::Make { variant, fields } => {
                        let Ty::Agg(ai) = i.ty else {
                            return Err("make の結果は集成体のはず".into());
                        };
                        let d = m.aggs.get(ai as usize).ok_or("集成体の番号が表にない")?;
                        let want: Vec<Ty> = match (&d.kind, variant) {
                            (AggKind::Enum(vs), Some(k)) => {
                                let v = vs.get(*k as usize).ok_or("バリアントの番号が不正")?;
                                v.fields.iter().map(|f| f.ty).collect()
                            }
                            (AggKind::Struct(fs) | AggKind::Tuple(fs), None) => fs.iter().map(|f| f.ty).collect(),
                            _ => return Err("make の変種の指定が集成体の種類と合わない".into()),
                        };
                        if want.len() != fields.len() {
                            return Err("make の要素数が型と合わない".into());
                        }
                        for (a, t) in fields.iter().zip(want) {
                            if ty_of(*a)? != t {
                                return Err("make の要素の型が合わない".into());
                            }
                        }
                    }
                    Op::Field { a, off } => {
                        let Ty::Agg(ai) = ty_of(*a)? else {
                            return Err("field の対象は集成体のはず".into());
                        };
                        let d = m.aggs.get(ai as usize).ok_or("集成体の番号が表にない")?;
                        if !has_field(d, *off, i.ty) {
                            return Err(format!("{} に +{} の {} のフィールドがない", d.name, off, fmt_ty(i.ty)));
                        }
                    }
                    Op::SetField { a, off, v } => {
                        let ta = ty_of(*a)?;
                        let Ty::Agg(ai) = ta else {
                            return Err("setfield の対象は集成体のはず".into());
                        };
                        let d = m.aggs.get(ai as usize).ok_or("集成体の番号が表にない")?;
                        if i.ty != ta || matches!(d.kind, AggKind::Enum(_)) {
                            return Err("setfield は構造体・タプルの同じ型の値を返すはず".into());
                        }
                        if !has_field(d, *off, ty_of(*v)?) {
                            return Err(format!("{} に +{} の型の合うフィールドがない", d.name, off));
                        }
                    }
                    Op::FrameBytes { size } => {
                        if i.ty != Ty::Ptr || *size == 0 {
                            return Err("frame.bytes は ptr の型で、大きさは 1 以上のはず".into());
                        }
                    }
                    Op::GlobalBytes { size, .. } => {
                        if i.ty != Ty::Ptr || *size == 0 {
                            return Err("global.bytes は ptr の型で、大きさは 1 以上のはず".into());
                        }
                    }
                    Op::StaticBytes(_) => {
                        if i.ty != Ty::Ptr {
                            return Err("static.bytes は ptr の型のはず".into());
                        }
                    }
                    Op::PtrAdd { p, off } => {
                        if i.ty != Ty::Ptr || ty_of(*p)? != Ty::Ptr || ty_of(*off)? != Ty::Usize {
                            return Err("ptr.add は ptr と usize の被演算子のはず".into());
                        }
                    }
                    Op::Utf8Valid { p, len } => {
                        if i.ty != Ty::Usize || ty_of(*p)? != Ty::Ptr || ty_of(*len)? != Ty::Usize {
                            return Err("utf8.valid は ptr と usize を受けて usize を返すはず".into());
                        }
                    }
                    Op::MemLoad { p, .. } => {
                        // 集成体の読み出し (`Vec<Struct>` の要素など) と `Option` / `Result` の枠 (`Vec<Option<T>>` の要素など) は認める。
                        if !(memable(i.ty) || matches!(i.ty, Ty::Agg(_) | Ty::Opt(_) | Ty::Res(..))) || ty_of(*p)? != Ty::Ptr {
                            return Err("load は ptr から整数・浮動小数・bool・ptr・集成体を読むはず".into());
                        }
                    }
                    Op::MemStore { ty, p, v, .. } => {
                        if i.ty != Ty::Unit || !(memable(*ty) || matches!(*ty, Ty::Agg(_) | Ty::Opt(_) | Ty::Res(..))) || ty_of(*p)? != Ty::Ptr || ty_of(*v)? != *ty {
                            return Err("store の型が合わない".into());
                        }
                    }
                    Op::HeapAlloc(n) => {
                        if i.ty != Ty::Ptr || ty_of(*n)? != Ty::Usize {
                            return Err("malloc は usize の大きさで、結果は ptr のはず".into());
                        }
                        if !f.uses.iter().any(|u| u == "alloc") {
                            return Err("確保 (malloc) は uses [alloc] の関数だけ".into());
                        }
                    }
                    Op::HeapRealloc { p, size } => {
                        if i.ty != Ty::Ptr || ty_of(*p)? != Ty::Ptr || ty_of(*size)? != Ty::Usize {
                            return Err("realloc は ptr と usize の被演算子のはず".into());
                        }
                        if !f.uses.iter().any(|u| u == "alloc") {
                            return Err("再確保 (realloc) は uses [alloc] の関数だけ".into());
                        }
                    }
                    Op::HeapFree(p) => {
                        if i.ty != Ty::Unit || ty_of(*p)? != Ty::Ptr {
                            return Err("free は ptr を解放するはず".into());
                        }
                    }
                    Op::IndexCheck { i: ix, len } => {
                        if !f.may_panic {
                            return Err("panics never の関数に添字の境界検査".into());
                        }
                        if i.ty != Ty::Unit || ty_of(*ix)? != Ty::Usize || ty_of(*len)? != Ty::Usize {
                            return Err("添字の境界検査は usize の被演算子のはず".into());
                        }
                    }
                    Op::Panic { arg, .. } => {
                        if !f.may_panic {
                            return Err("panics never の関数に panic".into());
                        }
                        if i.ty != Ty::Unit {
                            return Err("panic の結果は unit のはず".into());
                        }
                        if let Some(a) = arg {
                            let at = ty_of(*a)?;
                            if !at.is_int() || at.bits() > 64 {
                                return Err("panic の値は 64 ビット以下の整数のはず".into());
                            }
                        }
                    }
                    Op::Write { fd, data, .. } => {
                        if !f.uses.iter().any(|u| u == "io.console") {
                            return Err("write は uses [io.console] の関数だけ".into());
                        }
                        if ty_of(*fd)? != Ty::I32 || !matches!(ty_of(*data)?, Ty::Agg(d) if m.aggs[d as usize].name == "fat") {
                            return Err("write の被演算子は i32 の fd と fat のはず".into());
                        }
                        let ok = matches!(i.ty, Ty::Res(Pay::Unit, e) if matches!(e.ty(), Ty::Agg(a) if m.aggs[a as usize].name == "std.io.Error"));
                        if !ok {
                            return Err("write の結果は Result<unit, std.io.Error> のはず".into());
                        }
                    }
                    Op::NetListen { addr } | Op::NetConnect { addr } => {
                        if !f.uses.iter().any(|u| u == "io.net") {
                            return Err("net.listen / net.connect は uses [io.net] の関数だけ".into());
                        }
                        if ty_of(*addr)? != fat_ty(m)? {
                            return Err("net.listen / net.connect のアドレスは fat (&str) のはず".into());
                        }
                        let ok = matches!(i.ty, Ty::Res(Pay::Agg(_), e) if is_net_error(m, e.ty()));
                        if !ok {
                            return Err("net.listen / net.connect の結果は Result<ソケット集成体, std.net.Error> のはず".into());
                        }
                    }
                    Op::NetAccept { sock } | Op::NetFlush { sock } | Op::NetClose { sock } | Op::NetLocalPort { sock } => {
                        if !f.uses.iter().any(|u| u == "io.net") {
                            return Err("net.accept / net.flush / net.close / net.local_port は uses [io.net] の関数だけ".into());
                        }
                        if ty_of(*sock)? != Ty::I32 {
                            return Err("net.accept / net.flush / net.close / net.local_port のソケットは i32 のはず".into());
                        }
                        match &i.op {
                            Op::NetAccept { .. } => {
                                let ok = matches!(i.ty, Ty::Res(Pay::Agg(_), e) if is_net_error(m, e.ty()));
                                if !ok {
                                    return Err("net.accept の結果は Result<ソケット集成体, std.net.Error> のはず".into());
                                }
                            }
                            Op::NetLocalPort { .. } => {
                                let ok = matches!(i.ty, Ty::Res(Pay::U16, e) if is_net_error(m, e.ty()));
                                if !ok {
                                    return Err("net.local_port の結果は Result<u16, std.net.Error> のはず".into());
                                }
                            }
                            Op::NetClose { .. } => {
                                let ok = matches!(i.ty, Ty::Res(Pay::Unit, e) if is_net_error(m, e.ty()));
                                if !ok {
                                    return Err("net.close の結果は Result<(), std.net.Error> のはず".into());
                                }
                            }
                            _ => {
                                let ok = matches!(i.ty, Ty::Res(Pay::Unit, e) if is_io_error(m, e.ty()));
                                if !ok {
                                    return Err("net.flush の結果は Result<(), std.io.Error> のはず".into());
                                }
                            }
                        }
                    }
                    Op::Proc { f: pf, args } => {
                        if !f.uses.iter().any(|u| u == "process") {
                            return Err("std.process の命令は uses [process] の関数だけ".into());
                        }
                        if args.len() != pf.arity() {
                            return Err(format!("proc.{} の被演算子は {} 個のはず", pf.name(), pf.arity()));
                        }
                        for (k, a) in args.iter().enumerate() {
                            let want_fat = matches!((pf, k), (ProcFn::New, 0) | (ProcFn::Arg | ProcFn::EnvRemove | ProcFn::Dir | ProcFn::Env, 1) | (ProcFn::Env, 2));
                            let want = if want_fat { fat_ty(m)? } else { Ty::I32 };
                            if ty_of(*a)? != want {
                                return Err(format!("proc.{} の被演算子 {} の型が違う", pf.name(), k));
                            }
                        }
                        let res_ok = match pf {
                            ProcFn::New => i.ty == Ty::I32,
                            ProcFn::Spawn | ProcFn::Output => matches!(i.ty, Ty::Res(Pay::Agg(_), e) if is_io_error(m, e.ty())),
                            ProcFn::Wait => matches!(i.ty, Ty::Res(Pay::Agg(_), e) if is_io_error(m, e.ty())),
                            ProcFn::Kill => matches!(i.ty, Ty::Res(Pay::Unit, e) if is_io_error(m, e.ty())),
                            _ => i.ty == Ty::Unit,
                        };
                        if !res_ok {
                            return Err(format!("proc.{} の結果の型が違う", pf.name()));
                        }
                    }
                    Op::FsCreate { path } | Op::FsOpen { path } => {
                        if !f.uses.iter().any(|u| u == "io.fs" || u == "process") {
                            return Err("std.fs.create / std.fs.open は uses [io.fs] の関数だけ".into());
                        }
                        if ty_of(*path)? != fat_ty(m)? {
                            return Err("std.fs.create / std.fs.open のパスは fat (&str) のはず".into());
                        }
                        let ok = matches!(i.ty, Ty::Res(Pay::Agg(_), e) if is_io_error(m, e.ty()));
                        if !ok {
                            return Err("std.fs.create / std.fs.open の結果は Result<std.fs.File, std.io.Error> のはず".into());
                        }
                    }
                    Op::FsExists { path } => {
                        if !f.uses.iter().any(|u| u == "io.fs" || u == "process") {
                            return Err("std.fs.exists は uses [io.fs] の関数だけ".into());
                        }
                        if ty_of(*path)? != fat_ty(m)? {
                            return Err("std.fs.exists のパスは fat (&str) のはず".into());
                        }
                        if i.ty != Ty::Bool {
                            return Err("std.fs.exists の結果は bool のはず".into());
                        }
                    }
                    Op::NetSys { kind, args } => {
                        if !f.uses.iter().any(|u| u == "io.net") {
                            return Err(format!("net.{} は uses [io.net] の関数だけ", kind.name()));
                        }
                        if args.len() != kind.arity() {
                            return Err(format!("net.{} の引数は {} 個のはず", kind.name(), kind.arity()));
                        }
                        let fat = fat_ty(m)?;
                        let want: Vec<Ty> = match kind {
                            NetK::ReadTimeout | NetK::WriteTimeout => vec![Ty::I32, Ty::U64],
                            NetK::Shutdown => vec![Ty::I32, Ty::I32],
                            NetK::PeerAddr | NetK::LocalAddr | NetK::ReadTry | NetK::WriteTry => vec![Ty::I32, Ty::Ptr, Ty::Usize],
                            NetK::Resolve => vec![fat, Ty::U16, Ty::Ptr, Ty::Usize],
                            NetK::UdpBind => vec![fat],
                            NetK::UdpSendTo => vec![Ty::I32, Ty::Ptr, Ty::Usize, fat],
                            NetK::UdpRecvFrom => vec![Ty::I32, Ty::Ptr, Ty::Usize, Ty::Ptr, Ty::Usize],
                            NetK::ReadDeadline | NetK::WriteDeadline => vec![Ty::I32, Ty::U64],
                            NetK::ClearDeadlines => vec![Ty::I32],
                            NetK::SetNoDelay => vec![Ty::I32, Ty::Bool],
                            NetK::AcceptTimeout => vec![Ty::I32, Ty::U64],
                            NetK::ConnectTimeout => vec![fat, Ty::U64],
                        };
                        for (a, w) in args.iter().zip(want.iter()) {
                            if ty_of(*a)? != *w {
                                return Err(format!("net.{} の引数の型が合わない", kind.name()));
                            }
                        }
                        let ok = match kind {
                            NetK::UdpBind | NetK::AcceptTimeout | NetK::ConnectTimeout => {
                                matches!(i.ty, Ty::Res(Pay::Agg(_), e) if is_net_error(m, e.ty()))
                            }
                            NetK::ReadTimeout
                            | NetK::WriteTimeout
                            | NetK::Shutdown
                            | NetK::ReadDeadline
                            | NetK::WriteDeadline
                            | NetK::ClearDeadlines
                            | NetK::SetNoDelay => {
                                matches!(i.ty, Ty::Res(Pay::Unit, e) if is_io_error(m, e.ty()))
                            }
                            _ => matches!(i.ty, Ty::Res(Pay::Usize, e) if is_io_error(m, e.ty())),
                        };
                        if !ok {
                            return Err(format!("net.{} の結果の型が合わない", kind.name()));
                        }
                    }
                    Op::FsRemove { path } => {
                        if !f.uses.iter().any(|u| u == "io.fs" || u == "process") {
                            return Err("std.fs.remove_file は uses [io.fs] の関数だけ".into());
                        }
                        if ty_of(*path)? != fat_ty(m)? {
                            return Err("std.fs.remove_file のパスは fat (&str) のはず".into());
                        }
                        let ok = matches!(i.ty, Ty::Res(Pay::Unit, e) if is_io_error(m, e.ty()));
                        if !ok {
                            return Err("std.fs.remove_file の結果は Result<(), std.io.Error> のはず".into());
                        }
                    }
                    Op::FsSys { kind, args } => {
                        if !f.uses.iter().any(|u| u == "io.fs") {
                            return Err("std.fs の原始操作は uses [io.fs] の関数だけ".into());
                        }
                        if args.len() != kind.arity() {
                            return Err(format!("fs.{} の引数は {} 個のはず", kind.name(), kind.arity()));
                        }
                        for &ix in kind.path_slots() {
                            if ty_of(args[ix])? != fat_ty(m)? {
                                return Err(format!("fs.{} のパスは fat (&str) のはず", kind.name()));
                            }
                        }
                        let ok = match kind {
                            FsK::Rename | FsK::CreateDir | FsK::RemoveDir => matches!(i.ty, Ty::Res(Pay::Unit, e) if is_io_error(m, e.ty())),
                            FsK::OpenAppend | FsK::OpenDir | FsK::OpenBeneath => matches!(i.ty, Ty::Res(Pay::Agg(_), e) if is_io_error(m, e.ty())),
                            FsK::Stat => matches!(i.ty, Ty::Res(_, e) if is_io_error(m, e.ty())) && ty_of(args[1])? == Ty::U32,
                            FsK::FStat => matches!(i.ty, Ty::Res(_, e) if is_io_error(m, e.ty())) && ty_of(args[0])? == Ty::I32 && ty_of(args[1])? == Ty::U32,
                            FsK::ReadDir => matches!(i.ty, Ty::Res(_, e) if is_io_error(m, e.ty())),
                        };
                        if *kind == FsK::OpenBeneath && ty_of(args[0])? != Ty::I32 {
                            return Err("fs.open_beneath の記述子は i32 のはず".into());
                        }
                        if !ok {
                            return Err(format!("fs.{} の結果の型が合わない", kind.name()));
                        }
                    }
                    Op::FsClose { fd } | Op::FsFlush { fd } | Op::FsSync { fd } => {
                        if !f.uses.iter().any(|u| u == "io.fs" || u == "process") {
                            return Err("std.fs.File.{close,flush,sync} は uses [io.fs] の関数だけ".into());
                        }
                        if ty_of(*fd)? != Ty::I32 {
                            return Err("std.fs.File の記述子は i32 のはず".into());
                        }
                        let ok = matches!(i.ty, Ty::Res(Pay::Unit, e) if is_io_error(m, e.ty()));
                        if !ok {
                            return Err("std.fs.File.{close,flush,sync} の結果は Result<(), std.io.Error> のはず".into());
                        }
                    }
                    Op::FsRead { fd, buf, len } | Op::FsWrite { fd, buf, len } => {
                        // `std.io.write_stderr` (fd 2 への書き込み) は `io.console` で使える。
                        if !f.uses.iter().any(|u| u == "io.fs" || u == "process" || u == "io.console") {
                            return Err("std.fs.File.{read,write} は uses [io.fs] の関数だけ".into());
                        }
                        if ty_of(*fd)? != Ty::I32 || ty_of(*buf)? != Ty::Ptr || ty_of(*len)? != Ty::Usize {
                            return Err("std.fs.File.{read,write} は i32 の記述子と ptr のバッファと usize の長さを取る".into());
                        }
                        let ok = matches!(i.ty, Ty::Res(Pay::Usize, e) if is_io_error(m, e.ty()));
                        if !ok {
                            return Err("std.fs.File.{read,write} の結果は Result<usize, std.io.Error> のはず".into());
                        }
                    }
                    Op::RandFill { buf, len, fatal } => {
                        if !f.uses.iter().any(|u| u == "rand") {
                            return Err("std.rand.fill_secure は uses [rand] の関数だけ".into());
                        }
                        if ty_of(*buf)? != Ty::Ptr || ty_of(*len)? != Ty::Usize {
                            return Err("rand.fill は ptr のバッファと usize の長さを取る".into());
                        }
                        let ok = if *fatal { i.ty == Ty::Unit } else { matches!(i.ty, Ty::Res(Pay::Unit, e) if is_io_error(m, e.ty())) };
                        if !ok {
                            return Err("rand.fill の結果は Result<(), std.io.Error> (fatal なら unit) のはず".into());
                        }
                    }
                    Op::EnvArgCount => {
                        if !f.uses.iter().any(|u| u == "env") {
                            return Err("std.env.arg_count は uses [env] の関数だけ".into());
                        }
                        if i.ty != Ty::Usize {
                            return Err("std.env.arg_count の結果は usize のはず".into());
                        }
                    }
                    Op::EnvArg { index } | Op::EnvVar { name: index } => {
                        if !f.uses.iter().any(|u| u == "env") {
                            return Err("std.env.arg / std.env.var は uses [env] の関数だけ".into());
                        }
                        let at = ty_of(*index)?;
                        match &i.op {
                            Op::EnvArg { .. } => {
                                if at != Ty::Usize {
                                    return Err("std.env.arg の引数は usize のはず".into());
                                }
                            }
                            Op::EnvVar { .. } => {
                                let fat = fat_ty(m)?;
                                if at != fat {
                                    return Err("std.env.var の引数は &str (fat) のはず".into());
                                }
                            }
                            _ => {}
                        }
                        let fat = fat_ty(m)?;
                        let Ty::Agg(fat_idx) = fat else { return Err("fat 型が見つからない".into()); };
                        let ok = matches!(i.ty, Ty::Opt(Pay::Agg(a)) if a == fat_idx);
                        if !ok {
                            return Err("std.env.arg / std.env.var の結果は Option<&'static str> (Option<fat>) のはず".into());
                        }
                    }
                    Op::TimeNowNs => {
                        if !f.uses.iter().any(|u| u == "time") {
                            return Err("std.time.system_nanos は uses [time] の関数だけ".into());
                        }
                        if i.ty != Ty::I64 {
                            return Err("std.time.system_nanos の結果は i64 のはず".into());
                        }
                    }
                    Op::NetRead { sock, buf, len } | Op::NetWrite { sock, buf, len } => {
                        if !f.uses.iter().any(|u| u == "io.net") {
                            return Err("net.read / net.write は uses [io.net] の関数だけ".into());
                        }
                        if ty_of(*sock)? != Ty::I32 || ty_of(*buf)? != Ty::Ptr || ty_of(*len)? != Ty::Usize {
                            return Err("net.read / net.write は i32 のソケットと ptr のバッファと usize の長さを取る".into());
                        }
                        let want = match &i.op {
                            Op::NetRead { .. } | Op::NetWrite { .. } => {
                                matches!(i.ty, Ty::Res(Pay::Usize, e) if is_io_error(m, e.ty()))
                            }
                            _ => false,
                        };
                        if !want {
                            return Err("net.read / net.write の結果は Result<usize, std.io.Error> のはず".into());
                        }
                    }
                    Op::Eprint { data } => {
                        if !f.may_panic {
                            return Err("panics never の関数に eprint".into());
                        }
                        if i.ty != Ty::Unit || !matches!(ty_of(*data)?, Ty::Agg(d) if m.aggs[d as usize].name == "fat") {
                            return Err("eprint の被演算子は fat のはず".into());
                        }
                    }
                    Op::Exit { code } => {
                        if !f.uses.iter().any(|u| u == "process") {
                            return Err("std.process.exit は uses [process] の関数だけ".into());
                        }
                        if i.ty != Ty::Unit || ty_of(*code)? != Ty::I32 {
                            return Err("std.process.exit の引数は i32 で、結果は unit のはず".into());
                        }
                    }
                    Op::Abort => {
                        if !f.may_panic {
                            return Err("panics never の関数に abort".into());
                        }
                        if i.ty != Ty::Unit {
                            return Err("abort の結果は unit のはず".into());
                        }
                    }
                    Op::FmtFloat { bits, a } => {
                        if !f.may_panic {
                            return Err("panics never の関数に fmtfloat".into());
                        }
                        if i.ty != Ty::Unit {
                            return Err("fmtfloat の結果は unit のはず".into());
                        }
                        let at = ty_of(*a)?;
                        let ok = match bits {
                            32 => at == Ty::F32,
                            64 => at == Ty::F64,
                            _ => false,
                        };
                        if !ok {
                            return Err(format!("fmtfloat の被演算子は f{} のはず", bits));
                        }
                    }
                    Op::FloatText { bits, a, dst } => {
                        if i.ty != Ty::Usize {
                            return Err("floattext の結果は usize のはず".into());
                        }
                        let at = ty_of(*a)?;
                        if !(matches!((bits, at), (32, Ty::F32) | (64, Ty::F64))) {
                            return Err(format!("floattext の被演算子は f{} のはず", bits));
                        }
                        if ty_of(*dst)? != Ty::Ptr {
                            return Err("floattext の書き先は ptr のはず".into());
                        }
                    }
                    Op::ParseInt { ty, s, radix } => {
                        if !ty.is_int() {
                            return Err(format!("parseint の結果の整数型 {} は整数ではない", ty.name()));
                        }
                        // 結果は Result<整数, ParseIntError>。
                        let Ty::Res(_, _) = i.ty else {
                            return Err(format!("parseint の結果の型 {} は Result ではない", i.ty.name()));
                        };
                        // `s` は `&str` (fat: 集成体)、`radix` は `u32`。
                        if !matches!(ty_of(*s)?, Ty::Agg(_)) {
                            return Err("parseint の第 1 被演算子は &str (fat) のはず".into());
                        }
                        if ty_of(*radix)? != Ty::U32 {
                            return Err("parseint の第 2 被演算子は u32 のはず".into());
                        }
                    }
                    Op::Call { callee, args } => {
                        let Some(g) = callees.get(callee.as_str()) else {
                            return Err(format!("呼び先 {} がモジュールにない", callee));
                        };
                        if g.params().len() != args.len() {
                            return Err(format!("{} の引数の個数が違う", callee));
                        }
                        for (a, (_, pt)) in args.iter().zip(g.params().iter()) {
                            if ty_of(*a)? != *pt {
                                return Err(format!("{} の引数の型が合わない (%{})", callee, a));
                            }
                        }
                        if i.ty != g.ret {
                            return Err(format!("{} の戻り値の型が合わない", callee));
                        }
                        // スレッドの補助の関数 (`ksn.thr.*`、設計_KIR.md 22 章) は、spawn した側のクロージャの効果を呼ぶので、
                        // 効果と panic の包含は spawn 側 (クロージャの下ろし) で確かめ済みとして、ここでは見ない。
                        let thr = callee.starts_with("ksn.thr.") || callee.starts_with("ksn.chan.") || callee.starts_with("ksn.atm.");
                        // 中断の枠の関数 (`ksn.task.*`、`F.new` / `F.poll`) も、呼び出し元の宣言で効果を検査済みとして見ない (設計_KIR.md 26 章)。
                        let thr = thr || crate::kir_suspend::exempt_uses(callee);
                        if g.may_panic && !f.may_panic && !thr && !f.key.starts_with("ksn.thr.") && !f.key.starts_with("ksn.atm.") {
                            return Err(format!("panics never の関数が panics may の {} を呼ぶ", callee));
                        }
                        if let Some(u) = g.uses.iter().find(|u| !fu.contains(u.as_str())) {
                            if !thr && !f.key.starts_with("ksn.thr.") && !f.key.starts_with("ksn.atm.") && !crate::kir_suspend::exempt_uses(&f.key) {
                                return Err(format!("{} の uses {} が呼び出し元にない", callee, u));
                            }
                        }
                    }
                    Op::FnPtr { callee } => {
                        if i.ty != Ty::Ptr {
                            return Err("fnptr の結果は ptr のはず".into());
                        }
                        if callees.get(callee.as_str()).is_none() {
                            return Err(format!("fnptr の呼び先 {} がモジュールにない", callee));
                        }
                    }
                }
                Ok(())
            })();
            if let Err(msg) = res {
                return fail(format!("%{}: {}", i.dst, msg));
            }
            // 被演算子の定義も確かめる (ty_of が未定義を報告する)
            for a in operands(&i.op) {
                ty_of(a)?;
            }
        }
        match &b.term {
            Term::Br(t) => check_target(t)?,
            Term::BrIf(c, t1, t2) => {
                if ty_of(*c)? != Ty::Bool {
                    return fail("br_if の条件は bool のはず".into());
                }
                check_target(t1)?;
                check_target(t2)?;
            }
            Term::Ret(v) => {
                if ty_of(*v)? != f.ret {
                    return fail("ret の値の型が戻り値の型と合わない".into());
                }
            }
            Term::Unreachable => {}
        }
    }
    Ok(())
}

/// 型の中の集成体の番号が型表 (`Module::aggs`) にあるか (`Option` / `Result` の中も見る)。
fn check_agg_ids(m: &Module, t: Ty) -> Result<(), String> {
    match t {
        Ty::Agg(i) | Ty::Ref(_, Pointee::Agg(i)) if (i as usize) < m.aggs.len() => Ok(()),
        Ty::Agg(_) | Ty::Ref(_, Pointee::Agg(_)) => Err("集成体の番号が表にない".into()),
        Ty::Ref(_, Pointee::Opt(p)) => check_agg_ids(m, Ty::Opt(p)),
        Ty::Opt(p) => check_agg_ids(m, p.ty()),
        Ty::Res(t0, e0) => {
            check_agg_ids(m, t0.ty())?;
            check_agg_ids(m, e0.ty())
        }
        _ => Ok(()),
    }
}

/// バイト記憶 (`mem.load` / `mem.store`) で読み書きできる型: 整数・浮動小数・bool・ptr (集成体・参照・Option は持たない)。
pub(crate) fn memable(t: Ty) -> bool {
    t.is_int() || t.is_float() || matches!(t, Ty::Bool | Ty::Ptr)
}

/// `fat` (`{ptr, len}`) 集成体の型。型表に無ければ `None`。
fn fat_ty(m: &Module) -> Result<Ty, String> {
    let i = m
        .aggs
        .iter()
        .position(|d| d.name == "fat")
        .ok_or_else(|| "型表に fat がない".to_string())?;
    Ok(Ty::Agg(i as u32))
}

/// 型 `t` が `std.io.Error` 集成体か。
fn is_net_error(m: &Module, t: Ty) -> bool {
    matches!(
        t,
        Ty::Agg(i) if (i as usize) < m.aggs.len() && m.aggs[i as usize].name == "std.net.Error"
    )
}

fn is_io_error(m: &Module, t: Ty) -> bool {
    matches!(
        t,
        Ty::Agg(i) if (i as usize) < m.aggs.len() && m.aggs[i as usize].name == "std.io.Error"
    )
}

pub(crate) fn operands(op: &Op) -> Vec<VId> {
    match op {
        Op::Const(_) | Op::Alloca | Op::FrameBytes { .. } | Op::StaticBytes(_) | Op::GlobalBytes { .. } => vec![],
        Op::PtrAdd { p, off } => vec![*p, *off],
        Op::Utf8Valid { p, len } => vec![*p, *len],
        Op::MemLoad { p, .. } | Op::HeapFree(p) => vec![*p],
        Op::HeapAlloc(n) => vec![*n],
        Op::HeapRealloc { p, size } => vec![*p, *size],
        Op::MemStore { p, v, .. } => vec![*p, *v],
        Op::IndexCheck { i, len } => vec![*i, *len],
        Op::Panic { arg, .. } => arg.iter().copied().collect(),
        Op::Write { fd, data, .. } => vec![*fd, *data],
        Op::NetListen { addr } | Op::NetConnect { addr } => vec![*addr],
        Op::NetAccept { sock } | Op::NetFlush { sock } | Op::NetClose { sock } | Op::NetLocalPort { sock } => vec![*sock],
        Op::NetRead { sock, buf, len } | Op::NetWrite { sock, buf, len } => {
            vec![*sock, *buf, *len]
        }
        Op::Proc { args, .. } => args.clone(),
        Op::FsCreate { path } | Op::FsOpen { path } | Op::FsExists { path } | Op::FsRemove { path } => vec![*path],
        Op::FsSys { args, .. } => args.clone(),
        Op::NetSys { args, .. } => args.clone(),
        Op::FsClose { fd } | Op::FsFlush { fd } | Op::FsSync { fd } => vec![*fd],
        Op::FsRead { fd, buf, len } | Op::FsWrite { fd, buf, len } => vec![*fd, *buf, *len],
        Op::EnvArgCount => vec![],
        Op::EnvArg { index } | Op::EnvVar { name: index } => vec![*index],
        Op::TimeNowNs => vec![],
        Op::RandFill { buf, len, .. } => vec![*buf, *len],
        Op::Eprint { data } => vec![*data],
        Op::Exit { code } => vec![*code],
        Op::Abort => vec![],
        Op::Arith { a, b, .. }
        | Op::Cmp { a, b, .. }
        | Op::Bool(_, a, b)
        | Op::Checked { a, b, .. }
        | Op::FArith { a, b, .. }
        | Op::MinMax { a, b, .. }
        | Op::FBin { a, b, .. } => vec![*a, *b],
        Op::Shift { a, n, .. } => vec![*a, *n],
        Op::FmtFloat { a, .. } => vec![*a],
        Op::FloatText { a, dst, .. } => vec![*a, *dst],
        Op::ParseInt { s, radix, .. } => vec![*s, *radix],
        Op::Store { ptr, val } => vec![*ptr, *val],
        Op::Carry { a, b } => vec![*a, *b],
        Op::Neg { a, .. }
        | Op::Not(a)
        | Op::FUn { a, .. }
        | Op::Conv { a, .. }
        | Op::Bitcast { a, .. }
        | Op::BitCount { a, .. }
        | Op::Load(a)
        | Op::AsShared(a)
        | Op::Reborrow(a)
        | Op::Borrow { a, .. }
        | Op::ScopeEnd(a)
        | Op::Own { a, .. }
        | Op::Some(a)
        | Op::Ok(a)
        | Op::Err(a)
        | Op::Tag(a)
        | Op::Payload(a) => vec![*a],
        Op::Call { args, .. } => args.clone(),
        Op::FnPtr { .. } => vec![],
        Op::Make { fields, .. } => fields.clone(),
        Op::Field { a, .. } | Op::FieldAddr { a, .. } => vec![*a],
        Op::SetField { a, v, .. } => vec![*a, *v],
    }
}

/// 集成体の型 `d` に、オフセット `off` で型 `t` のフィールドがあるか (構造体・タプルは本体、列挙型は全バリアント)。
fn has_field(d: &AggDef, off: u32, t: Ty) -> bool {
    let hit = |fs: &[Field]| fs.iter().any(|f| f.off == off && f.ty == t);
    match &d.kind {
        AggKind::Struct(fs) | AggKind::Tuple(fs) => hit(fs),
        AggKind::Array(..) => false,
        AggKind::Enum(vs) => vs.iter().any(|v| hit(&v.fields)),
    }
}

/// 整数 `src` の値の集合が、整数 `dst` に含まれるか (`from_*` の変換は値を保つ組だけ許す)。
fn int_into(src: Ty, dst: Ty) -> bool {
    if src == Ty::U128 {
        return false;
    }
    if dst == Ty::U128 {
        return !src.signed();
    }
    if dst == Ty::I128 {
        return true;
    }
    src.bits() <= 64 && dst.bits() <= 64
}

/// 変換 `src -> dst` が KIR にあるか。整数どうしは `int_into`、整数・浮動小数から浮動小数へ、`f32` と `f64` の間。
pub fn conv_ok(src: Ty, dst: Ty) -> bool {
    if src.is_int() && dst.is_int() {
        return int_into(src, dst);
    }
    (src.is_int() && dst.is_float()) || (src == Ty::F32 && dst == Ty::F64) || (src == Ty::F64 && dst == Ty::F32)
}

/// 浮動小数から 64 ビット以下の整数への変換 (飽和し、NaN は 0。`trunc_to_*` が範囲を確かめた後に使う)。
pub fn float_to_int_ok(src: Ty, dst: Ty) -> bool {
    src.is_float() && dst.is_int() && dst.bits() <= 64
}

/// モジュールを検査する。違反のあった関数を (鍵, 理由) で返す。空なら正しい。
pub fn verify_module(m: &Module) -> Vec<(String, String)> {
    let mut out = vec![];
    // 集成体の型表: フィールドの型が表の中にあり、ユニットでないこと。
    let n = m.aggs.len() as u32;
    let agg_ok = |t: Ty| match t {
        Ty::Agg(i) => i < n,
        Ty::Unit => false,
        _ => true,
    };
    for d in &m.aggs {
        let fs: Vec<Ty> = match &d.kind {
            AggKind::Struct(fs) | AggKind::Tuple(fs) => fs.iter().map(|f| f.ty).collect(),
            AggKind::Array(t, _) => vec![*t],
            AggKind::Enum(vs) => vs.iter().flat_map(|v| v.fields.iter().map(|f| f.ty)).collect(),
        };
        if let Some(t) = fs.into_iter().find(|t| !agg_ok(*t)) {
            out.push((d.name.clone(), format!("{}: フィールドの型 {} が不正", d.name, fmt_ty(t))));
        }
    }
    let mut keys = HashSet::new();
    for f in &m.funcs {
        if !keys.insert(f.key.as_str()) {
            out.push((f.key.clone(), format!("{}: 鍵が重複している", f.key)));
            continue;
        }
        if let Err(e) = verify_func(m, f) {
            out.push((f.key.clone(), e));
        }
    }
    out
}

/// 記憶域の命令を全部使う関数 (確保・格納・読み出し・再確保・解放・境界検査・フレームの領域・静的な領域)。
#[cfg(test)]
pub(crate) const MEM_TEXT: &str = "kir 1

fn t.mem key=t.mem uses=[alloc] panics=may stack=- ret=u64
  b0():
    %0: usize = const.usize 16
    %1: ptr = heap.alloc %0
    %2: u64 = const.u64 77
    %3: unit = mem.store.u64 %1 +8, %2
    %4: ptr = heap.realloc %1, %0
    %5: u64 = mem.load.u64 %4 +8
    %6: usize = const.usize 3
    %7: unit = index.check %6, %0
    %8: ptr = frame.bytes 24
    %9: ptr = static.bytes 6869
    %10: ptr = ptr.add %9, %6
    %11: unit = mem.store.ptr %8 +0, %10
    %12: unit = heap.free %4
    ret %5
end t.mem
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_op_reads_back_with_escapes_and_a_value() {
        // 文言の引用符・バックスラッシュ・改行・% と、値の付いた panic / 値のない panic が、書いて読んでも同じ。
        let f = Func {
            fqn: "t.p".into(),
            key: "t.p".into(),
            uses: vec![],
            may_panic: true,
            stack: None,
            ret: Ty::Unit,
            link: None,
            blocks: vec![Block {
                params: vec![(0, Ty::U32)],
                insts: vec![
                    Inst { dst: 1, ty: Ty::Unit, op: Op::Panic { msg: "a\"b\\c\nd 100%".into(), arg: Some(0) } },
                    Inst { dst: 2, ty: Ty::Unit, op: Op::Panic { msg: "plain".into(), arg: None } },
                ],
                term: Term::Ret(2),
            }],
        };
        let m = Module { funcs: vec![f], aggs: vec![], depth: 0 };
        let text = print_module(&m);
        assert!(text.contains("panic \"a\\\"b\\\\c\\nd 100%\" %0"), "{}", text);
        let back = parse_module(&text).expect("parse");
        assert_eq!(back.funcs[0].blocks, m.funcs[0].blocks);
        assert!(verify_module(&back).is_empty(), "{:?}", verify_module(&back));
        // panics never の関数に panic は検査で落ちる
        let mut never = back.clone();
        never.funcs[0].may_panic = false;
        assert!(!verify_module(&never).is_empty());
    }

    #[test]
    fn eprint_and_abort_read_back_and_need_panics_may() {
        // `eprint` は fat を書き、`abort` は文言を出さずに終わる。`uses` は要らず、`panics never` では検査で落ちる (設計_KIR.md 13.2)。
        let src = |panics: &str| {
            format!(
                "kir 1

agg#0 struct fat size=16 align=8 {{ptr:ptr@0 len:usize@8}}

fn t.p key=t.p uses=[] panics={panics} stack=- ret=unit
  b0(%0: agg#0):
    %1: unit = eprint %0
    %2: unit = abort
    ret %2
end t.p
"
            )
        };
        let m = parse_module(&src("may")).expect("読める");
        assert!(verify_module(&m).is_empty(), "{:?}", verify_module(&m));
        assert_eq!(print_module(&m), src("may"));
        let bad = verify_module(&parse_module(&src("never")).expect("読める"));
        assert!(bad.iter().any(|(_, msg)| msg.contains("panics never")), "{:?}", bad);
    }

    fn sample() -> Module {
        // fib 風: b0(%0: u64) → 比較 → 分岐。wrap 付きの減算と加算を含む。
        let f = Func {
            fqn: "demo.fib".into(),
            key: "demo.fib".into(),
            uses: vec![],
            may_panic: false,
            stack: Some(4096),
            ret: Ty::U64,
            link: None,
            blocks: vec![
                Block {
                    params: vec![(0, Ty::U64)],
                    insts: vec![
                        Inst { dst: 1, ty: Ty::U64, op: Op::Const(2) },
                        Inst { dst: 2, ty: Ty::Bool, op: Op::Cmp { cc: Cc::Lt, ty: Ty::U64, a: 0, b: 1 } },
                    ],
                    term: Term::BrIf(2, (1, vec![]), (2, vec![0])),
                },
                Block { params: vec![], insts: vec![], term: Term::Ret(0) },
                Block {
                    params: vec![(3, Ty::U64)],
                    insts: vec![
                        Inst { dst: 4, ty: Ty::U64, op: Op::Const(1) },
                        Inst { dst: 5, ty: Ty::U64, op: Op::Arith { op: Aop::Sub, mode: Mode::Wrap, a: 3, b: 4 } },
                        Inst { dst: 6, ty: Ty::U64, op: Op::Call { callee: "demo.fib".into(), args: vec![5] } },
                    ],
                    term: Term::Ret(6),
                },
            ],
        };
        Module { funcs: vec![f], ..Default::default() }
    }

    #[test]
    fn print_parse_print_is_stable() {
        let m = sample();
        let text = print_module(&m);
        let back = parse_module(&text).expect("parse");
        assert_eq!(back, m);
        assert_eq!(print_module(&back), text);
    }

    #[test]
    fn verify_accepts_sample_and_rejects_trap_in_never() {
        let m = sample();
        assert!(verify_module(&m).is_empty(), "{:?}", verify_module(&m));
        let mut bad = sample();
        bad.funcs[0].blocks[2].insts[1].op = Op::Arith { op: Aop::Sub, mode: Mode::Trap, a: 3, b: 4 };
        let errs = verify_module(&bad);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].1.contains("panics never"), "{:?}", errs);
    }

    #[test]
    fn verify_rejects_a_constant_reference() {
        // 参照の定数 (null の参照など) は作れない。参照は alloca / as_shared / reborrow で作る。
        let mut bad = sample();
        bad.funcs[0].blocks[0].insts.push(Inst { dst: 900, ty: Ty::Ref(false, Pointee::U32), op: Op::Const(0) });
        let errs = verify_module(&bad);
        assert_eq!(errs.len(), 1, "{:?}", errs);
        assert!(errs[0].1.contains("参照の定数"), "{:?}", errs);
    }

    #[test]
    fn parse_rejects_other_versions() {
        assert!(parse_module("kir 2\n").is_err());
    }

    #[test]
    fn memory_ops_print_parse_and_verify() {
        let m = parse_module(MEM_TEXT).expect("parse");
        assert_eq!(print_module(&m), MEM_TEXT);
        assert!(verify_module(&m).is_empty(), "{:?}", verify_module(&m));
    }

    #[test]
    fn memory_ops_are_checked_by_effects_and_types() {
        // 確保は uses [alloc] の関数だけ
        let no_alloc = MEM_TEXT.replace("uses=[alloc]", "uses=[]");
        let errs = verify_module(&parse_module(&no_alloc).expect("parse"));
        assert!(errs.iter().any(|(_, e)| e.contains("uses [alloc]")), "{:?}", errs);
        // 境界検査は panics may の関数だけ
        let never = MEM_TEXT.replace("panics=may", "panics=never");
        let errs = verify_module(&parse_module(&never).expect("parse"));
        assert!(errs.iter().any(|(_, e)| e.contains("境界検査")), "{:?}", errs);
        // 格納の型と値の型が合わない
        let bad = MEM_TEXT.replace("store.u64 %1 +8, %2", "store.u32 %1 +8, %2");
        let errs = verify_module(&parse_module(&bad).expect("parse"));
        assert!(errs.iter().any(|(_, e)| e.contains("store")), "{:?}", errs);
    }

    #[test]
    fn wrap_and_ranges() {
        assert_eq!(Ty::U8.wrap(256 + 3), 3);
        assert_eq!(Ty::I8.wrap(128), -128);
        assert_eq!(Ty::I8.min(), -128);
        assert_eq!(Ty::U64.max(), u64::MAX as i128);
    }

    /// 集成体の型表 (構造体 `t.P`、列挙型 `t.S`) と、それを使う関数 (構築・読み書き・タグ・変種の構築)。
    fn agg_sample() -> Module {
        let p = AggDef {
            name: "t.P".into(),
            size: 8,
            align: 4,
            kind: AggKind::Struct(vec![
                Field { name: "x".into(), ty: Ty::I32, off: 0 },
                Field { name: "y".into(), ty: Ty::I32, off: 4 },
            ]),
        };
        let s = AggDef {
            name: "t.S".into(),
            size: 12,
            align: 4,
            kind: AggKind::Enum(vec![
                Variant { name: "Circle".into(), fields: vec![Field { name: String::new(), ty: Ty::I32, off: 4 }] },
                Variant {
                    name: "Rect".into(),
                    fields: vec![
                        Field { name: String::new(), ty: Ty::I32, off: 4 },
                        Field { name: String::new(), ty: Ty::I32, off: 8 },
                    ],
                },
                Variant { name: "Empty".into(), fields: vec![] },
            ]),
        };
        let f = Func {
            fqn: "t.f".into(),
            key: "t.f".into(),
            uses: vec![],
            may_panic: false,
            stack: None,
            ret: Ty::I32,
            link: None,
            blocks: vec![Block {
                params: vec![(0, Ty::I32)],
                insts: vec![
                    Inst { dst: 1, ty: Ty::Agg(0), op: Op::Make { variant: None, fields: vec![0, 0] } },
                    Inst { dst: 2, ty: Ty::I32, op: Op::Field { a: 1, off: 4 } },
                    Inst { dst: 3, ty: Ty::Agg(0), op: Op::SetField { a: 1, off: 0, v: 2 } },
                    Inst { dst: 4, ty: Ty::Agg(1), op: Op::Make { variant: Some(1), fields: vec![0, 2] } },
                    Inst { dst: 5, ty: Ty::I32, op: Op::Tag(4) },
                ],
                term: Term::Ret(5),
            }],
        };
        Module { funcs: vec![f], aggs: vec![p, s], depth: 0 }
    }

    #[test]
    fn agg_decls_print_and_read_back() {
        let m = agg_sample();
        let text = print_module(&m);
        assert!(text.contains("agg#0 struct t.P size=8 align=4 {x:i32@0 y:i32@4}"), "{}", text);
        assert!(text.contains("agg#1 enum t.S size=12 align=4 {Circle(i32@4) Rect(i32@4,i32@8) Empty()}"), "{}", text);
        let back = parse_module(&text).expect("parse");
        assert_eq!(back, m);
        assert_eq!(print_module(&back), text);
    }

    /// 参照の型と命令 (`ref<T>` / `mutref<T>`、`alloca` / `load` / `store` / `asshared` / `reborrow`) の往復。
    #[test]
    fn refs_print_and_parse_round_trip() {
        let text = "kir 1

fn t.r key=t.r uses=[] panics=never stack=- ret=ref<i32>
  b0(%0: ref<i32>):
    ret %0
end t.r

fn t.w key=t.w uses=[] panics=never stack=- ret=i32
  b0(%0: i32):
    %1: mutref<i32> = alloca
    %2: unit = store %1, %0
    %3: mutref<i32> = reborrow %1
    %4: ref<i32> = asshared %3
    %5: i32 = load %4
    ret %5
end t.w
";
        let m = parse_module(text).expect("parse");
        assert_eq!(print_module(&m), text);
        assert!(verify_module(&m).is_empty(), "{:?}", verify_module(&m));
    }

    #[test]
    fn verify_rejects_a_field_that_is_not_at_the_declared_offset() {
        let mut bad = agg_sample();
        bad.funcs[0].blocks[0].insts[1].op = Op::Field { a: 1, off: 2 };
        let errs = verify_module(&bad);
        assert_eq!(errs.len(), 1, "{:?}", errs);
        assert!(errs[0].1.contains("フィールド"), "{:?}", errs);
    }

    #[test]
    fn prune_keeps_only_referenced_aggs_and_renumbers() {
        // t.S (添字 1) だけを使う関数: 型表の t.P (添字 0) は外れ、t.S は添字 0 になる。
        let mut m = agg_sample();
        m.funcs[0].blocks[0].insts.retain(|i| !matches!(i.op, Op::Make { variant: None, .. } | Op::Field { .. } | Op::SetField { .. }));
        m.funcs[0].blocks[0].insts[0].dst = 4;
        m.funcs[0].blocks[0].insts[0].ty = Ty::Agg(1);
        m.funcs[0].blocks[0].insts[1].dst = 5;
        m.funcs[0].blocks[0].term = Term::Ret(5);
        m.funcs[0].blocks[0].insts[0].op = Op::Make { variant: Some(1), fields: vec![0, 0] };
        prune_aggs(&mut m);
        assert_eq!(m.aggs.len(), 1);
        assert_eq!(m.aggs[0].name, "t.S");
        assert_eq!(m.funcs[0].blocks[0].insts[0].ty, Ty::Agg(0));
    }

    #[test]
    fn refs_verify_rejects_stores_through_shared_refs_and_wrong_loads() {
        // 共有参照 ref<T> への store は使えない
        let shared = "kir 1

fn t.s key=t.s uses=[] panics=never stack=- ret=unit
  b0(%0: ref<i32>):
    %1: i32 = const.i32 1
    %2: unit = store %0, %1
    %3: unit = const.unit ()
    ret %3
end t.s
";
        let errs = verify_module(&parse_module(shared).expect("parse"));
        assert_eq!(errs.len(), 1, "{:?}", errs);
        assert!(errs[0].1.contains("共有参照"), "{:?}", errs);

        // 指す先と違う型で読む load は使えない
        let wrong = "kir 1

fn t.l key=t.l uses=[] panics=never stack=- ret=i64
  b0(%0: ref<i32>):
    %1: i64 = load %0
    ret %1
end t.l
";
        let errs = verify_module(&parse_module(wrong).expect("parse"));
        assert_eq!(errs.len(), 1, "{:?}", errs);
        assert!(errs[0].1.contains("load の結果の型"), "{:?}", errs);

        // 比較は参照どうしでは行わない (アドレスの比較は値の比較ではない)
        let cmp = "kir 1

fn t.c key=t.c uses=[] panics=never stack=- ret=bool
  b0(%0: ref<i32>, %1: ref<i32>):
    %2: bool = icmp.eq.ref<i32> %0, %1
    ret %2
end t.c
";
        let errs = verify_module(&parse_module(cmp).expect("parse"));
        assert_eq!(errs.len(), 1, "{:?}", errs);
        assert!(errs[0].1.contains("使えない"), "{:?}", errs);
    }

    /// Option / Result を含むモジュール: 型の表示と読み直し、ブロック引数の `Result<i64,bool>` (区切りの `,`) を確かめる。
    fn enum_sample() -> Module {
        let f = Func {
            fqn: "t.pick".into(),
            key: "t.pick".into(),
            uses: vec![],
            may_panic: false,
            stack: None,
            ret: Ty::Res(Pay::U16, Pay::U16),
            link: None,
            blocks: vec![
                Block {
                    params: vec![(0, Ty::Bool)],
                    insts: vec![
                        Inst { dst: 1, ty: Ty::Res(Pay::I64, Pay::Bool), op: Op::Ok(2) },
                        Inst { dst: 2, ty: Ty::I64, op: Op::Const(3) },
                    ],
                    term: Term::BrIf(0, (1, vec![1]), (2, vec![])),
                },
                Block {
                    params: vec![(3, Ty::Res(Pay::I64, Pay::Bool))],
                    insts: vec![
                        Inst { dst: 4, ty: Ty::Opt(Pay::I64), op: Op::Some(2) },
                        Inst { dst: 5, ty: Ty::Opt(Pay::I64), op: Op::Const(0) },
                        Inst { dst: 6, ty: Ty::Opt(Pay::I64), op: Op::Checked { op: Aop::Add, a: 2, b: 2 } },
                        Inst { dst: 7, ty: Ty::Bool, op: Op::Tag(6) },
                        Inst { dst: 8, ty: Ty::I64, op: Op::Payload(6) },
                    ],
                    term: Term::Ret(9),
                },
                Block {
                    params: vec![],
                    insts: vec![Inst { dst: 9, ty: Ty::Res(Pay::U16, Pay::U16), op: Op::Err(10) }, Inst { dst: 10, ty: Ty::U16, op: Op::Const(9) }],
                    term: Term::Unreachable,
                },
            ],
        };
        Module { funcs: vec![f], aggs: vec![], depth: 0 }
    }

    #[test]
    fn enum_types_print_and_read_back() {
        let m = enum_sample();
        let text = print_module(&m);
        assert!(text.contains("ret=Result<u16,u16>"), "{}", text);
        assert!(text.contains("b0(%0: bool):") && text.contains("b1(%3: Result<i64,bool>):"), "{}", text);
        assert!(text.contains("%5: Option<i64> = const.Option<i64> none"), "{}", text);
        assert!(text.contains("%6: Option<i64> = checked.add %2, %2"), "{}", text);
        let back = parse_module(&text).expect("parse");
        assert_eq!(back, m);
        assert_eq!(print_module(&back), text);
    }

    #[test]
    fn verify_rejects_misused_enum_ops() {
        // 比較は scalar だけ
        let mut bad = enum_sample();
        bad.funcs[0].blocks[1].insts.push(Inst { dst: 20, ty: Ty::Bool, op: Op::Cmp { cc: Cc::Eq, ty: Ty::Opt(Pay::I64), a: 4, b: 5 } });
        assert!(verify_module(&bad).iter().any(|(_, e)| e.contains("比較")), "{:?}", verify_module(&bad));
        // None 以外の Option 定数はない
        let mut bad = enum_sample();
        bad.funcs[0].blocks[1].insts[1].op = Op::Const(1);
        assert!(verify_module(&bad).iter().any(|(_, e)| e.contains("none")), "{:?}", verify_module(&bad));
        // ok の被演算子は Ok 側の型
        let mut bad = enum_sample();
        bad.funcs[0].blocks[0].insts[0].op = Op::Ok(0);
        assert!(!verify_module(&bad).is_empty());
        // tag は Option / Result だけ
        let mut bad = enum_sample();
        bad.funcs[0].blocks[1].insts[3].op = Op::Tag(2);
        assert!(verify_module(&bad).iter().any(|(_, e)| e.contains("tag")), "{:?}", verify_module(&bad));
        // checked は panics never でも使える (trap を持たない)
        let mut ok = enum_sample();
        ok.funcs[0].may_panic = false;
        assert!(verify_module(&ok).is_empty(), "{:?}", verify_module(&ok));
    }

    #[test]
    fn enum_payload_encoding_round_trips() {
        // 符号付きのペイロードは下位 64 ビットの 2 の補数で持ち、型で読み直す
        let v = TAG_BIT | payload_bits(-1);
        assert_eq!(payload_of(Ty::I64, v), -1);
        assert_eq!(show_value(Ty::Opt(Pay::I64), v), "Some(-1)");
        assert_eq!(show_value(Ty::Opt(Pay::I64), 0), "None");
        assert_eq!(show_value(Ty::Res(Pay::U8, Pay::Bool), payload_bits(1)), "Ok(1)");
        assert_eq!(show_value(Ty::Res(Pay::U8, Pay::Bool), TAG_BIT | 1), "Err(true)");
        assert_eq!(show_value(Ty::Res(Pay::Unit, Pay::U8), 0), "Ok(())");
    }

    #[test]
    fn nested_and_aggregate_payload_types_read_back() {
        // 入れ子の Option / Result と集成体のペイロードは、名前を読み直すと同じ型になる (`Result<..,..>` の `,` は深さで割る)。
        let cases = [
            Ty::Opt(Pay::Opt(intern(Pay::I64))),
            Ty::Opt(Pay::Res(intern(Pay::I64), intern(Pay::Bool))),
            Ty::Res(Pay::Res(intern(Pay::I64), intern(Pay::Bool)), Pay::U8),
            Ty::Res(Pay::Agg(1), Pay::Opt(intern(Pay::Agg(0)))),
            Ty::Res(Pay::Unit, Pay::Agg(2)),
        ];
        for t in cases {
            let name = t.name();
            assert_eq!(Ty::parse(&name), Some(t), "{}", name);
        }
        assert_eq!(Ty::Res(Pay::Res(intern(Pay::I64), intern(Pay::Bool)), Pay::U8).name(), "Result<Result<i64,bool>,u8>");
        // 入れ子の型は `Pay` の値で比べられる (同じ型は同じ値)
        assert_eq!(Pay::of(Ty::Opt(Pay::I64)), Some(Pay::Opt(intern(Pay::I64))));
        assert_eq!(Pay::of(Ty::Agg(3)).and_then(|p| Some(p.ty())), Some(Ty::Agg(3)));
        // 128 ビット整数と参照は payload にできない
        assert_eq!(Pay::of(Ty::I128), None);
        assert_eq!(Ty::parse("Option<i128>"), None);
    }

    #[test]
    fn verify_rejects_an_aggregate_index_outside_the_table() {
        // `Option<agg#5>` の 5 が型表にないなら、検査が止める (入れ子の中の番号も見る)。
        let src = "kir 1

fn t.bad key=t.bad uses=[] panics=never stack=- ret=Option<agg#5>
  b0():
    %0: Option<agg#5> = const.Option<agg#5> none
    ret %0
end t.bad
";
        let m = parse_module(src).expect("読める");
        let bad = verify_module(&m);
        assert!(bad.iter().any(|(_, msg)| msg.contains("集成体の番号が表にない")), "{:?}", bad);
    }

    #[test]
    fn write_reads_back_and_needs_io_console() {
        // `write fd, data` は読み直せ、`uses [io.console]` の関数だけが持てる (設計_KIR.md 12.2)。
        let src = |uses: &str| {
            format!(
                "kir 1

agg#0 enum std.io.ErrorKind size=4 align=4 {{Other()}}
agg#1 struct std.io.Error size=8 align=4 {{kind:agg#0@0 code:i32@4}}
agg#2 struct fat size=16 align=8 {{ptr:ptr@0 len:usize@8}}

fn t.w key=t.w uses=[{uses}] panics=never stack=- ret=Result<unit,agg#1>
  b0():
    %0: ptr = static.bytes 6869
    %1: usize = const.usize 2
    %2: agg#2 = make(%0, %1)
    %3: i32 = const.i32 2
    %4: Result<unit,agg#1> = write %3, %2
    ret %4
end t.w
"
            )
        };
        let m = parse_module(&src("io.console")).expect("読める");
        assert!(verify_module(&m).is_empty(), "{:?}", verify_module(&m));
        assert_eq!(print_module(&m), src("io.console"));
        let bad = verify_module(&parse_module(&src("")).expect("読める"));
        assert!(bad.iter().any(|(_, msg)| msg.contains("write は uses [io.console]")), "{:?}", bad);
    }
}
