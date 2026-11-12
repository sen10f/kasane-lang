//! KIR: 型付き・単相化済みの中間表現 (設計_KIR.md 2 章、段階 1)。
//!
//! - 1 関数 = 1 つの単相インスタンス (`key`)。値は SSA (`%n` は 1 回だけ定義) で、制御の合流はブロック引数で渡す。
//! - 型は整数 (`i8`..`i64`、`isize`、`u8`..`u64`、`usize`)、`bool`、`unit` と、組み込みの `Option<P>` / `Result<T, E>` (ペイロードはスカラー。設計_KIR.md 11 章)。128 ビット整数と他の集成体は未対応。
//! - テキスト形式は `kir 1` (設計_KIR.md 7 章 18: 版が違えば読み込みを拒否する)。
//!   `print_module` の出力を `parse_module` で読み直すと、同じ構造になり、もう一度 `print_module` すると同じ文字列になる。
//! - `verify_module` は SSA の一意性、型、ブロック引数の個数、効果 (trap の有無、`uses`、`panics`) を検査する。

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
    Isize,
    U8,
    U16,
    U32,
    U64,
    Usize,
    Bool,
    Unit,
    /// 参照 `ref<T>` (false) / `mutref<T>` (true)。値はアドレス (参照実行器ではセルの番号)。
    /// 指す先はスカラー (整数と bool) だけ。`Ty` は `Copy` を保つため、中身は `Ty` ではなく `Pointee` にする。
    Ref(bool, Pointee),
    /// 組み込みの `Option<P>` (設計_KIR.md 11 章)。値はタグ (Some か) とペイロード 1 つ。
    Opt(Pay),
    /// 組み込みの `Result<T, E>` (設計_KIR.md 11 章)。値はタグ (Err か) とペイロード 1 つ。
    Res(Pay, Pay),
}

/// 参照の指す先。整数と bool だけ (unit と参照は入れない)。`Ty::Ref` の中身。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pointee {
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
    Bool,
}

impl Pointee {
    pub fn ty(self) -> Ty {
        match self {
            Pointee::I8 => Ty::I8,
            Pointee::I16 => Ty::I16,
            Pointee::I32 => Ty::I32,
            Pointee::I64 => Ty::I64,
            Pointee::Isize => Ty::Isize,
            Pointee::U8 => Ty::U8,
            Pointee::U16 => Ty::U16,
            Pointee::U32 => Ty::U32,
            Pointee::U64 => Ty::U64,
            Pointee::Usize => Ty::Usize,
            Pointee::Bool => Ty::Bool,
        }
    }

    /// 指す先に使える型なら、その `Pointee`。unit と参照は `None`。
    pub fn of(t: Ty) -> Option<Pointee> {
        Some(match t {
            Ty::I8 => Pointee::I8,
            Ty::I16 => Pointee::I16,
            Ty::I32 => Pointee::I32,
            Ty::I64 => Pointee::I64,
            Ty::Isize => Pointee::Isize,
            Ty::U8 => Pointee::U8,
            Ty::U16 => Pointee::U16,
            Ty::U32 => Pointee::U32,
            Ty::U64 => Pointee::U64,
            Ty::Usize => Pointee::Usize,
            Ty::Bool => Pointee::Bool,
            Ty::Unit | Ty::Ref(..) | Ty::Opt(_) | Ty::Res(..) => return None,
        })
    }
}

/// Option / Result のペイロードの型。スカラーだけ (ネストした Option や集成体は持てない)。
/// `Ty` が `Copy` のままで入れ子を作れないように、スカラーを別の列挙で持つ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    Bool,
    Unit,
}

const ALL_TYS: [Ty; 12] = [
    Ty::I8,
    Ty::I16,
    Ty::I32,
    Ty::I64,
    Ty::Isize,
    Ty::U8,
    Ty::U16,
    Ty::U32,
    Ty::U64,
    Ty::Usize,
    Ty::Bool,
    Ty::Unit,
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
            Pay::Bool => Ty::Bool,
            Pay::Unit => Ty::Unit,
        }
    }

    /// スカラーの型なら対応する `Pay`。`Option` / `Result` / 参照は `None`。
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
            Ty::Bool => Pay::Bool,
            Ty::Unit => Pay::Unit,
            Ty::Opt(_) | Ty::Res(..) | Ty::Ref(..) => return None,
        })
    }
}

/// ペイロードの値 (`v` は `Option` / `Result` の値全体の符号化) を、型 `t` の値に読み直す。
/// タグは見ない。bool は 0 / 1、unit は 0、整数は下位 64 ビットを型の範囲へ写す。
pub fn payload_of(t: Ty, v: i128) -> i128 {
    match t {
        Ty::Bool => v & 1,
        Ty::Unit => 0,
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
            Ty::Isize => "isize".into(),
            Ty::U8 => "u8".into(),
            Ty::U16 => "u16".into(),
            Ty::U32 => "u32".into(),
            Ty::U64 => "u64".into(),
            Ty::Usize => "usize".into(),
            Ty::Bool => "bool".into(),
            Ty::Unit => "unit".into(),
            Ty::Ref(m, p) => format!("{}<{}>", if m { "mutref" } else { "ref" }, p.ty().name()),
            // 区切りは `,` だけ (空白を含めない)。テキスト形式の 1 語を保つため。
            Ty::Opt(p) => format!("Option<{}>", p.ty().name()),
            Ty::Res(t, e) => format!("Result<{},{}>", t.ty().name(), e.ty().name()),
        }
    }

    pub fn parse(s: &str) -> Option<Ty> {
        if let Some(x) = ALL_TYS.iter().copied().find(|t| t.name() == s) {
            return Some(x);
        }
        let pay = |x: &str| Pay::of(ALL_TYS.iter().copied().find(|t| t.name() == x)?);
        if let Some(inner) = s.strip_prefix("Option<").and_then(|r| r.strip_suffix('>')) {
            return pay(inner).map(Ty::Opt);
        }
        if let Some(inner) = s.strip_prefix("Result<").and_then(|r| r.strip_suffix('>')) {
            let (a, b) = inner.split_once(',')?;
            return Some(Ty::Res(pay(a)?, pay(b)?));
        }
        if let Some(inner) = s.strip_prefix("ref<").and_then(|r| r.strip_suffix('>')) {
            return Pointee::of(Ty::parse(inner)?).map(|p| Ty::Ref(false, p));
        }
        if let Some(inner) = s.strip_prefix("mutref<").and_then(|r| r.strip_suffix('>')) {
            return Pointee::of(Ty::parse(inner)?).map(|p| Ty::Ref(true, p));
        }
        None
    }

    /// 整数型 (`i8`..`usize`)。`bool` / `unit` / `Option` / `Result` / 参照は含まない。
    pub fn is_int(self) -> bool {
        !matches!(self, Ty::Bool | Ty::Unit | Ty::Ref(..) | Ty::Opt(_) | Ty::Res(..))
    }

    pub fn is_ref(self) -> bool {
        matches!(self, Ty::Ref(..))
    }

    /// スカラー (整数・bool・unit)。`Option` / `Result` / 参照は含まない。`Cmp` や定数の型はこれに限る。
    pub fn is_scalar(self) -> bool {
        !matches!(self, Ty::Opt(_) | Ty::Res(..) | Ty::Ref(..))
    }

    pub fn signed(self) -> bool {
        matches!(self, Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::Isize)
    }

    /// 整数の幅 (ビット)。`isize` / `usize` は 64 ビット。
    pub fn bits(self) -> u32 {
        match self {
            Ty::I8 | Ty::U8 => 8,
            Ty::I16 | Ty::U16 => 16,
            Ty::I32 | Ty::U32 => 32,
            _ => 64,
        }
    }

    pub fn min(self) -> i128 {
        if self.signed() {
            -(1i128 << (self.bits() - 1))
        } else {
            0
        }
    }

    pub fn max(self) -> i128 {
        if self.signed() {
            (1i128 << (self.bits() - 1)) - 1
        } else {
            (1i128 << self.bits()) - 1
        }
    }

    /// 2 の補数の切り捨て (`wrapping_*` の結果を型の範囲に収める)。
    pub fn wrap(self, v: i128) -> i128 {
        let b = self.bits();
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

/// 溢れの扱い。`Wrap` は `wrapping_*` (溢れを切り捨てる)、`Trap` は `+ - * / %` (溢れで panic。現行どおり)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Wrap,
    Trap,
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

/// 命令の本体。結果の型と番号は `Inst` が持つ。
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// 定数。`bool` は 0 / 1、`unit` は 0。
    Const(i128),
    /// 算術。結果の型 (`Inst::ty`) が演算の型。
    Arith { op: Aop, mode: Mode, a: VId, b: VId },
    /// 比較。`ty` は被演算子の型 (結果は `bool`)。
    Cmp { cc: Cc, ty: Ty, a: VId, b: VId },
    /// 単項マイナス (符号付き整数。溢れで trap)。
    Neg { a: VId },
    /// `bool` の否定。
    Not(VId),
    /// `bool` の非短絡の論理演算 (`& | ^`)。
    Bool(BoolOp, VId, VId),
    /// 直接呼び出し (単相インスタンスの鍵で指す)。
    Call { callee: String, args: Vec<VId> },
    /// スタックスロットの確保 (結果は `mutref<T>`、`T` は `Inst::ty` の指す先)。関数の入口ブロックに置く。
    Alloca,
    /// 参照を通した読み出し `*p` (結果の型は指す先の型)。
    Load(VId),
    /// 参照を通した書き込み `*p = v` (`p` は `mutref<T>`、`v` は `T`。結果は unit)。
    Store { ptr: VId, val: VId },
    /// `&mut T` を `&T` として使う (同じアドレス。結果は `ref<T>`)。
    AsShared(VId),
    /// `mutref<T>` を同じアドレスの別の値として複製する。スタックスロットの値番号と、参照の値を
    /// 取り違えないために使う (スロットの番号を参照の値に流用すると、読み書きの区別がつかなくなる)。
    Reborrow(VId),
    /// `Some(a)`: 結果の型 `Option<P>` (`a` は `P`)。`None` は `Const(0)` (型 `Option<P>`)。
    Some(VId),
    /// `Ok(a)`: 結果の型 `Result<T, E>` (`a` は `T`)。
    Ok(VId),
    /// `Err(a)`: 結果の型 `Result<T, E>` (`a` は `E`)。
    Err(VId),
    /// タグを読む (bool)。`Option` なら Some か、`Result` なら Err か。
    Tag(VId),
    /// ペイロードを読む (結果の型 `Inst::ty` で読む)。タグは見ない。呼び出し側が `Tag` で確かめてから使う。
    Payload(VId),
    /// `checked_<op>` (結果の型 `Option<P>`、被演算子は `P`)。溢れと 0 除算は `None`。panic しないので `panics never` でも使える。
    Checked { op: Aop, a: VId, b: VId },
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

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Module {
    /// 鍵の順 (UTF-8 順) に並べる。
    pub funcs: Vec<Func>,
}

impl Module {
    pub fn find(&self, key: &str) -> Option<&Func> {
        self.funcs.iter().find(|f| f.key == key)
    }

    /// `key` から呼び出しで到達できる関数だけを取り出した部分モジュール (鍵の順は保つ)。
    pub fn reachable(&self, key: &str) -> Module {
        let mut seen: HashSet<String> = HashSet::new();
        let mut todo = vec![key.to_string()];
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
                    }
                }
            }
        }
        Module { funcs: self.funcs.iter().filter(|f| seen.contains(&f.key)).cloned().collect() }
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

fn fmt_inst(i: &Inst) -> String {
    let body = match &i.op {
        Op::Const(v) => {
            let lit = match i.ty {
                Ty::Bool => (if *v != 0 { "true" } else { "false" }).to_string(),
                Ty::Unit => "()".to_string(),
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
            let m = if *mode == Mode::Wrap { "wrap" } else { "trap" };
            format!("{}.{}.{} {}", op.name(), i.ty.name(), m, fmt_vals(&[*a, *b]))
        }
        Op::Cmp { cc, ty, a, b } => format!("icmp.{}.{} {}", cc.name(), ty.name(), fmt_vals(&[*a, *b])),
        Op::Neg { a } => format!("neg.{}.trap {}", i.ty.name(), fmt_val(*a)),
        Op::Not(a) => format!("not.bool {}", fmt_val(*a)),
        Op::Bool(op, a, b) => {
            let n = match op {
                BoolOp::And => "and",
                BoolOp::Or => "or",
                BoolOp::Xor => "xor",
            };
            format!("{}.bool {}", n, fmt_vals(&[*a, *b]))
        }
        Op::Call { callee, args } => format!("call {}({})", callee, fmt_vals(args)),
        Op::Alloca => "alloca".to_string(),
        Op::Load(p) => format!("load {}", fmt_val(*p)),
        Op::Store { ptr, val } => format!("store {}", fmt_vals(&[*ptr, *val])),
        Op::AsShared(p) => format!("asshared {}", fmt_val(*p)),
        Op::Reborrow(p) => format!("reborrow {}", fmt_val(*p)),
    };
    format!("%{}: {} = {}", i.dst, i.ty.name(), body)
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
        f.ret.name()
    ));
    for (bi, b) in f.blocks.iter().enumerate() {
        let ps: Vec<String> = b.params.iter().map(|(v, t)| format!("%{}: {}", v, t.name())).collect();
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
    Ty::parse(s.trim()).ok_or_else(|| perr(line, format!("型 {} は KIR の段階 1 にない", s.trim())))
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
    if let Some(r) = rhs.strip_prefix("call ") {
        let open = r.find('(').ok_or_else(|| perr(line, "call に ( がない"))?;
        let close = r.rfind(')').ok_or_else(|| perr(line, "call に ) がない"))?;
        return Ok(Op::Call { callee: r[..open].to_string(), args: parse_vals(&r[open + 1..close], line)? });
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
                (Ty::Opt(_), "none") => 0,
                (t, x) if t.is_int() => x.parse::<i128>().map_err(|_| perr(line, format!("整数 {} が不正", x)))?,
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
        ("icmp", [cc, t]) => {
            let cc = parse_cc(cc).ok_or_else(|| perr(line, format!("比較 {} が不正", cc)))?;
            let oty = parse_ty(t, line)?;
            let v = operands(2)?;
            Ok(Op::Cmp { cc, ty: oty, a: v[0], b: v[1] })
        }
        ("neg", [t, "trap"]) => {
            same_ty(t)?;
            Ok(Op::Neg { a: operands(1)?[0] })
        }
        ("not", ["bool"]) => Ok(Op::Not(operands(1)?[0])),
        (name @ ("and" | "or" | "xor"), ["bool"]) => {
            let op = match name {
                "and" => BoolOp::And,
                "or" => BoolOp::Or,
                _ => BoolOp::Xor,
            };
            let v = operands(2)?;
            Ok(Op::Bool(op, v[0], v[1]))
        }
        (name, [t, m]) if parse_aop(name).is_some() && matches!(*m, "wrap" | "trap") => {
            same_ty(t)?;
            let v = operands(2)?;
            let mode = if *m == "wrap" { Mode::Wrap } else { Mode::Trap };
            Ok(Op::Arith { op: parse_aop(name).unwrap(), mode, a: v[0], b: v[1] })
        }
        ("alloca", []) => Ok(Op::Alloca),
        ("load", []) => Ok(Op::Load(operands(1)?[0])),
        ("store", []) => {
            let v = operands(2)?;
            Ok(Op::Store { ptr: v[0], val: v[1] })
        }
        ("asshared", []) => Ok(Op::AsShared(operands(1)?[0])),
        ("reborrow", []) => Ok(Op::Reborrow(operands(1)?[0])),
        _ => Err(perr(line, format!("命令 {} が不明", head))),
    }
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
            blocks,
        });
    }
    Ok(Module { funcs })
}

// ---------------------------------------------------------------- 検証

/// 関数 1 つの検査。値の一意性と使用前の定義、型、ブロック引数、効果を調べる。
fn verify_func(m: &Module, f: &Func) -> Result<(), String> {
    let fail = |msg: String| Err(format!("{}: {}", f.key, msg));
    if f.blocks.is_empty() {
        return fail("ブロックがない".into());
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
                        if matches!(i.ty, Ty::Res(..)) {
                            return Err("Result の定数はない (Ok / Err で作る)".into());
                        }
                        if matches!(i.ty, Ty::Opt(_)) && *v != 0 {
                            return Err("Option の定数は none (0) だけ".into());
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
                    Op::Tag(a) => {
                        if i.ty != Ty::Bool {
                            return Err("tag の結果は bool のはず".into());
                        }
                        if !matches!(ty_of(*a)?, Ty::Opt(_) | Ty::Res(..)) {
                            return Err("tag の被演算子は Option か Result のはず".into());
                        }
                    }
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
                        if *mode == Mode::Wrap && !matches!(op, Aop::Add | Aop::Sub | Aop::Mul) {
                            return Err(format!("{} に wrap は使えない", op.name()));
                        }
                        if *mode == Mode::Trap && !f.may_panic {
                            return Err("panics never の関数に trap 付きの算術".into());
                        }
                        if ty_of(*a)? != i.ty || ty_of(*b)? != i.ty {
                            return Err("算術の被演算子の型が結果と合わない".into());
                        }
                    }
                    Op::Cmp { cc, ty, a, b } => {
                        if i.ty != Ty::Bool {
                            return Err("比較の結果は bool のはず".into());
                        }
                        if !ty.is_scalar() || *ty == Ty::Unit || (*ty == Ty::Bool && !matches!(cc, Cc::Eq | Cc::Ne)) {
                            return Err(format!("型 {} の比較 {} は使えない", ty.name(), cc.name()));
                        }
                        if ty_of(*a)? != *ty || ty_of(*b)? != *ty {
                            return Err("比較の被演算子の型が合わない".into());
                        }
                    }
                    Op::Neg { a } => {
                        if !(i.ty.is_int() && i.ty.signed()) {
                            return Err("符号なし整数や bool の単項 - は使えない".into());
                        }
                        if !f.may_panic {
                            return Err("panics never の関数に trap 付きの neg".into());
                        }
                        if ty_of(*a)? != i.ty {
                            return Err("neg の被演算子の型が合わない".into());
                        }
                    }
                    Op::Not(a) => {
                        if i.ty != Ty::Bool || ty_of(*a)? != Ty::Bool {
                            return Err("not は bool のはず".into());
                        }
                    }
                    Op::Bool(_, a, b) => {
                        if i.ty != Ty::Bool || ty_of(*a)? != Ty::Bool || ty_of(*b)? != Ty::Bool {
                            return Err("論理演算は bool のはず".into());
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
                    Op::Reborrow(a) => {
                        if !matches!(i.ty, Ty::Ref(true, _)) || ty_of(*a)? != i.ty {
                            return Err("reborrow は mutref を同じ型で複製するだけのはず".into());
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
                        if g.may_panic && !f.may_panic {
                            return Err(format!("panics never の関数が panics may の {} を呼ぶ", callee));
                        }
                        if let Some(u) = g.uses.iter().find(|u| !fu.contains(u.as_str())) {
                            return Err(format!("{} の uses {} が呼び出し元にない", callee, u));
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

fn operands(op: &Op) -> Vec<VId> {
    match op {
        Op::Const(_) | Op::Alloca => vec![],
        Op::Arith { a, b, .. } | Op::Cmp { a, b, .. } | Op::Bool(_, a, b) | Op::Checked { a, b, .. } => vec![*a, *b],
        Op::Neg { a } | Op::Not(a) | Op::Load(a) | Op::AsShared(a) | Op::Reborrow(a) | Op::Some(a) | Op::Ok(a) | Op::Err(a) | Op::Tag(a) | Op::Payload(a) => vec![*a],
        Op::Store { ptr, val } => vec![*ptr, *val],
        Op::Call { args, .. } => args.clone(),
    }
}

/// モジュールを検査する。違反のあった関数を (鍵, 理由) で返す。空なら正しい。
pub fn verify_module(m: &Module) -> Vec<(String, String)> {
    let mut out = vec![];
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Module {
        // fib 風: b0(%0: u64) → 比較 → 分岐。wrap 付きの減算と加算を含む。
        let f = Func {
            fqn: "demo.fib".into(),
            key: "demo.fib".into(),
            uses: vec![],
            may_panic: false,
            stack: Some(4096),
            ret: Ty::U64,
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
        Module { funcs: vec![f] }
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
    fn parse_rejects_other_versions() {
        assert!(parse_module("kir 2\n").is_err());
    }

    #[test]
    fn wrap_and_ranges() {
        assert_eq!(Ty::U8.wrap(256 + 3), 3);
        assert_eq!(Ty::I8.wrap(128), -128);
        assert_eq!(Ty::I8.min(), -128);
        assert_eq!(Ty::U64.max(), u64::MAX as i128);
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
        Module { funcs: vec![f] }
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
}
