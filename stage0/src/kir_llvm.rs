//! KIR から LLVM IR のテキスト (`.ll`) を作る (設計_KIR.md 段階 4 (B)、7 章 22)。
//!
//! Rust の LLVM 束縛は使わず、外部の `clang` が読むテキストだけを作る。意味は参照実行器 `kir_exec.rs` と同じにする。
//!
//! - 値の写像は `lty` / `mem_ty` の 1 か所にまとめる。整数 `iN`、`bool` は `i1` (メモリ上は `i8`)、`unit` は `i8` の 0、
//!   `f32` / `f64` は `float` / `double`、`ptr` と参照は `ptr`、
//!   集成体は大きさ (バイト) × 8 ビットの整数 `iN` (フィールドは `Module::aggs` のオフセットどおりのビット位置に置く)、
//!   `Option` / `Result` は整数 `iN` (N = 1 + 枠。タグは最上位のビット、枠は 64 ビット以上でペイロードのどれも入る幅。設計_KIR.md 11.6)。
//! - panic は stderr に `panic: <文言>` を出して終了コード 101。呼び出し深さは大域の計数で数え、
//!   参照実行器と同じ条件 (呼び出し元の深さが 2500 以上) で `stack overflow` と終了コード 102。
//! - 浮動小数の超越関数は `fmath` の `Ops` を LLVM IR の命令列に写した内部関数 (`@ksn.m.*`) を呼ぶ。libm は使わない
//!   (`floor` / `sqrt` / `frem` など丸めが一意に決まる演算は LLVM の組み込みを使う)。
//! - 出力は決定的 (同じ KIR からは同じバイト列)。
//! - 入口 `main` は引数を 16 進の語 (下位・上位) で受け取り、結果も 16 進の 2 語で標準出力の最後に出す (前に制御文字 `\x01` を付ける。プログラム自身の出力と区別するため)。表示は呼び出し側
//!   (`kir::show_value`) が行うので、`--run` と同じ表示になる。

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::fmath::{self, Ops};
use crate::kir::{
    size_of, verify_module, AggDef, AggKind, Aop, BitK, BoolOp, Cc, FBin, FUn, Func, Inst, Mode, Module, Op, Term, Ty, VId,
};

/// 呼び出し深さの上限 (`kir_exec::MAX_DEPTH` と同じ)。
const MAX_DEPTH: u32 = 2500;

type R<T> = Result<T, String>;

fn agg_bits(aggs: &[AggDef], i: u32) -> u32 {
    aggs[i as usize].size.max(1) * 8
}

/// 値 `t` を整数として表すときの幅 (ペイロードの枠に入る大きさ)。単位型は 0 (枠に置かない)、`bool` は 1、
/// 集成体は `agg_bits`、`Option` / `Result` は タグを含む幅 (`1 + 枠`)。
fn vbits(aggs: &[AggDef], t: Ty) -> u32 {
    match t {
        Ty::Unit => 0,
        Ty::Bool => 1,
        Ty::F32 => 32,
        Ty::F64 | Ty::Ptr | Ty::Ref(..) => 64,
        Ty::Agg(i) => agg_bits(aggs, i),
        Ty::Opt(_) | Ty::Res(..) => 1 + slot_bits(aggs, t),
        t => t.bits(),
    }
}

/// `Option` / `Result` の枠 (ペイロードの語の幅)。64 ビット以上で、ペイロードのどれも入る幅。
/// 枠の上にタグを 1 ビット置く (`Option` は Some = 1、`Result` は Err = 1)。
fn slot_bits(aggs: &[AggDef], t: Ty) -> u32 {
    let pays: Vec<Ty> = match t {
        Ty::Opt(p) => vec![p.ty()],
        Ty::Res(a, e) => vec![a.ty(), e.ty()],
        _ => vec![],
    };
    pays.into_iter().map(|p| vbits(aggs, p)).max().unwrap_or(0).max(64)
}

/// 集成体 (フィールドや引数に入る値) を含むか。`Option<Point>` や `Result<i64, E>` (E が集成体) が対象。
fn has_agg(t: Ty) -> bool {
    match t {
        Ty::Agg(_) => true,
        Ty::Opt(p) => has_agg(p.ty()),
        Ty::Res(a, e) => has_agg(a.ty()) || has_agg(e.ty()),
        _ => false,
    }
}

// ---------------------------------------------------------------- 型の写像 (ここだけ)

/// SSA の値の LLVM の型。
fn lty(aggs: &[AggDef], t: Ty) -> String {
    match t {
        Ty::I8 | Ty::U8 | Ty::Unit => "i8".into(),
        Ty::I16 | Ty::U16 => "i16".into(),
        Ty::I32 | Ty::U32 => "i32".into(),
        Ty::I64 | Ty::U64 | Ty::Isize | Ty::Usize => "i64".into(),
        Ty::I128 | Ty::U128 => "i128".into(),
        Ty::F32 => "float".into(),
        Ty::F64 => "double".into(),
        Ty::Bool => "i1".into(),
        Ty::Ptr | Ty::Ref(..) => "ptr".into(),
        Ty::Opt(_) | Ty::Res(..) => format!("i{}", 1 + slot_bits(aggs, t)),
        Ty::Agg(i) => format!("i{}", agg_bits(aggs, i)),
    }
}

/// メモリ上の LLVM の型 (`bool` だけ 1 バイト)。
fn mem_ty(aggs: &[AggDef], t: Ty) -> String {
    if t == Ty::Bool {
        "i8".into()
    } else {
        lty(aggs, t)
    }
}

/// 整数の定数 (型の幅に 2 の補数で収めた符号付きの十進)。
fn int_lit(t: Ty, v: i128) -> String {
    let w = t.bits();
    if w >= 128 {
        return v.to_string();
    }
    let s = 128 - w;
    ((v << s) >> s).to_string()
}

fn float_lit(t: Ty, bits: i128) -> String {
    match t {
        Ty::F32 => format!("0x{:016X}", (f32::from_bits(bits as u32) as f64).to_bits()),
        _ => format!("0x{:016X}", bits as u64),
    }
}

/// LLVM の大域名 (引用符つき)。
fn gname(prefix: &str, key: &str) -> String {
    let mut s = String::from("@\"");
    s.push_str(prefix);
    for c in key.chars() {
        match c {
            '"' => s.push_str("\\22"),
            '\\' => s.push_str("\\5C"),
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

fn fname(key: &str) -> String {
    gname("k.", key)
}

// ---------------------------------------------------------------- モジュール全体

/// 超越関数の内部関数 (`fmath` を命令列に写す)。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Math {
    Sin,
    Cos,
    Tan,
    Exp,
    Ln,
    Atan2,
    Powi,
}

impl Math {
    fn name(self) -> &'static str {
        match self {
            Math::Sin => "sin",
            Math::Cos => "cos",
            Math::Tan => "tan",
            Math::Exp => "exp",
            Math::Ln => "ln",
            Math::Atan2 => "atan2",
            Math::Powi => "powi",
        }
    }
}

/// panic の文言の種類 (関数 `@ksn.panic.<名前>` が文言を出して終了コード 101 で終わる)。
const PANICS: [(&str, &str); 8] = [
    ("add", "attempt to add with overflow"),
    ("sub", "attempt to subtract with overflow"),
    ("mul", "attempt to multiply with overflow"),
    ("div", "attempt to divide with overflow"),
    ("rem", "attempt to calculate the remainder with overflow"),
    ("div0", "attempt to divide by zero"),
    ("rem0", "attempt to calculate the remainder with a divisor of by zero"),
    ("neg", "attempt to negate with overflow"),
];

struct Ctx {
    decls: BTreeSet<String>,
    globals: String,
    math: BTreeSet<Math>,
    /// `write` 命令を使う (`@ksn.write` を出す)。
    write: bool,
    /// `eprint` 命令を使う (`@ksn.eprint` を出す。エラーは捨てる)。
    eprint: bool,
    /// `FmtFloat` 命令を使う (浮動小数の書式化補助関数を出す)。
    fmt_float: bool,
    /// `ParseInt` 命令を使う (`kir_rt.c` の `ksn_parse_int` を呼ぶ)。
    parse_int: bool,
}

impl Ctx {
    fn decl(&mut self, s: impl Into<String>) {
        self.decls.insert(s.into());
    }
}

fn c_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\' {
            out.push(b as char);
        } else {
            out.push_str(&format!("\\{:02X}", b));
        }
    }
    out
}

fn str_const(name: &str, s: &str) -> String {
    format!("@{} = private unnamed_addr constant [{} x i8] c\"{}\"\n", name, s.len(), c_escape(s))
}

/// `entry` を入口とする実行可能な LLVM IR を作る。KIR の検査に通らない・入口が扱えない形は理由つきの `Err`。
pub fn emit(m: &Module, entry: &str) -> R<String> {
    emit_with(m, entry, false)
}

/// `main` から始まるプログラム全体の実行ファイルの LLVM IR。入口は引数なしの関数 (`()` か `Result<T, E>` を返す) を呼び、
/// `Err(e)` なら stderr に `Error: <e の Debug 表示>` と改行を出して終了コード 1、それ以外は 0 で終わる (設計_KIR.md 12.3)。
/// `E` の表示は、KIR の関数 `ksn.entry.debug` (降格が作る) を呼ぶ。
pub fn emit_program(m: &Module, entry: &str) -> R<String> {
    emit_with(m, entry, true)
}

fn emit_with(m: &Module, entry: &str, program: bool) -> R<String> {
    let errs = verify_module(m);
    if let Some((k, e)) = errs.first() {
        return Err(format!("KIR の検査に通らない ({}): {}", k, e));
    }
    let Some(ef) = m.find(entry) else {
        return Err(format!("関数 {} がない", entry));
    };
    let has_fmt_float = m.funcs.iter().any(|f| f.blocks.iter().any(|b| b.insts.iter().any(|i| matches!(i.op, Op::FmtFloat { .. }))));
    let has_parse_int = m.funcs.iter().any(|f| f.blocks.iter().any(|b| b.insts.iter().any(|i| matches!(i.op, Op::ParseInt { .. }))));
    let mut cx = Ctx { decls: BTreeSet::new(), globals: String::new(), math: BTreeSet::new(), write: false, eprint: false, fmt_float: has_fmt_float, parse_int: has_parse_int };
    // `extern` 宣言は `define` ではなく `declare` として出す。本体の emission はスキップする。
    let extern_links: HashMap<String, String> = m.funcs.iter()
        .filter_map(|f| f.link.as_ref().map(|l| {
            let sym = l.rsplit_once(':').map(|(_, s)| s).unwrap_or(l);
            (f.key.clone(), sym.to_string())
        }))
        .collect();
    for (key, sym) in &extern_links {
        let Some(f) = m.funcs.iter().find(|f| f.key == *key) else { continue };
        let params: Vec<String> = f.params().iter().map(|(_, t)| lty(&m.aggs, *t)).collect();
        let rt = lty(&m.aggs, f.ret);
        cx.decl(format!("declare {} @{}({})", rt, sym, params.join(", ")));
    }
    let mut funcs = String::new();
    for (fi, f) in m.funcs.iter().enumerate() {
        if f.link.is_some() {
            continue;
        }
        funcs.push('\n');
        funcs.push_str(&Fe::new(&mut cx, &m.aggs, f, &extern_links, fi).emit().map_err(|e| format!("{}: {}", f.key, e))?);
    }
    let main = if program { emit_program_main(&mut cx, m, ef)? } else { emit_main(&mut cx, &m.aggs, ef)? };
    let mut math = String::new();
    for mf in cx.math.clone() {
        math.push('\n');
        math.push_str(&emit_math(&mut cx, mf));
    }
    for d in [
        "declare i32 @dprintf(i32, ptr, ...)",
        "declare i32 @printf(ptr, ...)",
        "declare void @exit(i32) noreturn",
        "declare ptr @malloc(i64)",
        "declare ptr @realloc(ptr, i64)",
        "declare void @free(ptr)",
        "declare i64 @strtoull(ptr, ptr, i32)",
    ] {
        cx.decls.insert(d.to_string());
    }
    if cx.fmt_float {
        cx.decls.insert("declare i32 @snprintf(ptr, i64, ptr, ...)".to_string());
        cx.decls.insert("declare i64 @strlen(ptr)".to_string());
        cx.decls.insert("declare double @strtod(ptr, ptr)".to_string());
        cx.decls.insert("declare float @strtof(ptr, ptr)".to_string());
    }
    if cx.parse_int {
        cx.decls.insert("declare i32 @ksn_parse_int(ptr, i64, i32, i32, i32, ptr, ptr)".to_string());
    }
    let mut out = String::from("; ksn kir llvm 1\n\n");
    for d in &cx.decls {
        out.push_str(d);
        out.push('\n');
    }
    out.push('\n');
    out.push_str("@ksn.depth = internal global i64 0\n");
    out.push_str(&str_const("ksn.fmt.res", "\x01%llx %llx\n\0"));
    out.push_str(&str_const("ksn.fmt.so", "stack overflow\n\0"));
    out.push_str(&str_const("ksn.fmt.index", "panic: index out of bounds: the len is %llu but the index is %llu\n\0"));
    for (n, msg) in PANICS {
        out.push_str(&str_const(&format!("ksn.msg.{}", n), &format!("panic: {}\n\0", msg)));
    }
    out.push_str(&cx.globals);
    out.push('\n');
    for (n, _) in PANICS {
        out.push_str(&format!(
            "define private void @ksn.panic.{n}() noreturn cold {{\n  %r = call i32 (i32, ptr, ...) @dprintf(i32 2, ptr @ksn.msg.{n})\n  call void @exit(i32 101)\n  unreachable\n}}\n\n"
        ));
    }
    out.push_str("define private void @ksn.panic.index(i64 %len, i64 %ix) noreturn cold {\n  %r = call i32 (i32, ptr, ...) @dprintf(i32 2, ptr @ksn.fmt.index, i64 %len, i64 %ix)\n  call void @exit(i32 101)\n  unreachable\n}\n\n");
    out.push_str("define private void @ksn.overflow() noreturn cold {\n  %r = call i32 (i32, ptr, ...) @dprintf(i32 2, ptr @ksn.fmt.so)\n  call void @exit(i32 102)\n  unreachable\n}\n");
    if cx.fmt_float {
        out.push_str(&fmt_float_helpers());
    }
    if cx.write {
        out.push_str(&write_helper(&m.aggs)?);
    }
    if cx.eprint {
        out.push_str(EPRINT_HELPER);
    }
    out.push_str(&funcs);
    out.push_str(&math);
    out.push('\n');
    out.push_str(&main);
    // AddressSanitizer が有効なら、全ての関数定義に sanitize_address を付ける (設計_KIR.md 17.9)。
    // `declare` は `define` ではないので置換されない。
    if std::env::var("KSN_CLANG_ARGS")
        .map(|s| s.split_whitespace().any(|a| a == "-fsanitize=address"))
        .unwrap_or(false)
    {
        // `define ... {` → `define ... sanitize_address {`（関数属性は `{` の前に置く）。`declare` は対象外。
        out = out
            .lines()
            .map(|l| {
                let trimmed = l.trim_start();
                if trimmed.starts_with("define ") && trimmed.ends_with('{') {
                    let indent = &l[..l.len() - trimmed.len()];
                    let body = &trimmed[..trimmed.len() - 1];
                    format!("{} {}sanitize_address {{\n", indent, body)
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
    }
    Ok(out)
}

/// プログラムの入口 `main` (`emit_program`)。
fn emit_program_main(cx: &mut Ctx, m: &Module, f: &Func) -> R<String> {
    let aggs = &m.aggs;
    if !f.params().is_empty() {
        return Err(format!("入口 {} に引数がある", f.key));
    }
    let rt = lty(aggs, f.ret);
    let mut s = String::from("define i32 @main(i32 %argc, ptr %argv) {\nentry:\n");
    s.push_str(&format!("  %r = call {} {}()\n", rt, fname(&f.key)));
    match f.ret {
        Ty::Res(_, e) => {
            let sb = slot_bits(aggs, f.ret);
            let n = sb + 1;
            let dk = m.find(crate::kir_lower::debug::ENTRY_DEBUG).ok_or("Err の表示の関数 (ksn.entry.debug) がない")?;
            let et = e.ty();
            let w = vbits(aggs, et);
            cx.decl("declare i64 @write(i32, ptr, i64)");
            cx.globals.push_str(&str_const("ksn.s.err", "Error: "));
            cx.globals.push_str(&str_const("ksn.s.nl", "\n"));
            s.push_str(&format!("  %t = lshr i{n} %r, {sb}\n  %tb = trunc i{n} %t to i1\n  br i1 %tb, label %err, label %ok\nerr:\n"));
            s.push_str(&format!("  %pw = trunc i{n} %r to i{sb}\n"));
            let x = if w == sb { "%pw".to_string() } else { s.push_str(&format!("  %pe = trunc i{sb} %pw to i{w}\n")); "%pe".to_string() };
            s.push_str("  %w1 = call i64 @write(i32 2, ptr @ksn.s.err, i64 7)\n");
            s.push_str(&format!("  %d = call i8 {}({} {})\n", fname(&dk.key), lty(aggs, et), x));
            s.push_str("  %w2 = call i64 @write(i32 2, ptr @ksn.s.nl, i64 1)\n  ret i32 1\nok:\n  ret i32 0\n}\n");
        }
        _ => s.push_str("  ret i32 0\n}\n"),
    }
    Ok(s)
}

fn errno_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "__error"
    } else {
        "__errno_location"
    }
}

/// `eprint` 命令の実体 `@ksn.eprint(ptr, len)`: stderr (fd 2) へ全バイトを書く。エラーは捨てる (panic の文言の途中なので、`write` の結果の表は要らない)。
/// `write` と違い `std.io.ErrorKind` の表を使わないので、型表にそれが無くても作れる。
const EPRINT_HELPER: &str = "define private void @ksn.eprint(ptr %p, i64 %n) {\nentry:\n  br label %loop\nloop:\n  %off = phi i64 [0, %entry], [%off2, %adv]\n  %left = sub i64 %n, %off\n  %done = icmp eq i64 %left, 0\n  br i1 %done, label %ret, label %try\ntry:\n  %q = getelementptr i8, ptr %p, i64 %off\n  %w = call i64 @write(i32 2, ptr %q, i64 %left)\n  %neg = icmp slt i64 %w, 0\n  br i1 %neg, label %ret, label %adv\nadv:\n  %off2 = add i64 %off, %w\n  br label %loop\nret:\n  ret void\n}\n";

/// `write` 命令の実体 `@ksn.write(fd, ptr, len) -> i64` (設計_KIR.md 12.2)。全バイトを書くまで `write(2)` を繰り返し、EINTR は再試行する。
/// 成功は 0。失敗は `(errno << 32) | ErrorKind の変種の添字` (errno は 0 にならないので、0 と区別できる)。
/// errno と `ErrorKind` の対応は木走査 (`natives::io_kind_name`、Rust の `io::Error::kind`) と同じ。
fn write_helper(aggs: &[AggDef]) -> R<String> {
    let kinds = aggs.iter().find(|d| d.name == "std.io.ErrorKind").ok_or("型表に std.io.ErrorKind がない")?;
    let AggKind::Enum(vs) = &kinds.kind else { return Err("std.io.ErrorKind が列挙型でない".into()) };
    let idx = |n: &str| vs.iter().position(|v| v.name == n).ok_or_else(|| format!("ErrorKind に {} がない", n));
    let errno_fn = errno_name();
    let mut s = String::new();
    s.push_str("define private i64 @ksn.write(i32 %fd, ptr %p, i64 %n) {\nentry:\n  br label %loop\nloop:\n");
    s.push_str("  %off = phi i64 [0, %entry], [%off2, %adv], [%off, %fail]\n  %left = sub i64 %n, %off\n  %done = icmp eq i64 %left, 0\n  br i1 %done, label %ok, label %try\n");
    s.push_str("try:\n  %q = getelementptr i8, ptr %p, i64 %off\n  %w = call i64 @write(i32 %fd, ptr %q, i64 %left)\n  %neg = icmp slt i64 %w, 0\n  br i1 %neg, label %fail, label %adv\n");
    s.push_str("adv:\n  %off2 = add i64 %off, %w\n  br label %loop\n");
    s.push_str(&format!("fail:\n  %ep = call ptr @{}()\n  %e = load i32, ptr %ep\n  %intr = icmp eq i32 %e, 4\n  br i1 %intr, label %loop, label %err\n", errno_fn));
    s.push_str("ok:\n  ret i64 0\nerr:\n");
    let mut cur = format!("{}", idx("Other")?);
    for (k, (errno, name)) in [(1, "PermissionDenied"), (13, "PermissionDenied"), (2, "NotFound"), (17, "AlreadyExists"), (22, "InvalidInput"), (32, "BrokenPipe"), (12, "OutOfMemory")].iter().enumerate() {
        s.push_str(&format!("  %c{k} = icmp eq i32 %e, {errno}\n  %k{k} = select i1 %c{k}, i64 {}, i64 {cur}\n", idx(name)?));
        cur = format!("%k{k}");
    }
    s.push_str(&format!("  %ez = zext i32 %e to i64\n  %eh = shl i64 %ez, 32\n  %r = or i64 %eh, {cur}\n  ret i64 %r\n}}\n\n"));
    Ok(s)
}

/// 浮動小数の書式化は、C の実行時補助 `kir_rt.c` の `ksn_fmt_f64` / `ksn_fmt_f32` に任せる (設計_KIR.md 17.11)。
/// 規則は `py_float` / `py_float32` と同じ。結果は静的バッファへのポインタ (NUL 終端)。
fn fmt_float_helpers() -> String {
    "declare ptr @ksn_fmt_f64(double)\ndeclare ptr @ksn_fmt_f32(float)\n".to_string()
}

/// C の実行時補助 (`kir_rt.c`) の本文。生成した IR が `@ksn_fmt_` を呼ぶときだけ、clang に一緒に渡す。
pub const RUNTIME_C: &str = include_str!("kir_rt.c");

/// IR が C の実行時補助を要するか。
pub fn needs_runtime(ll: &str) -> bool {
    ll.contains("@ksn_fmt_") || ll.contains("@ksn_parse_int")
}

fn emit_main(cx: &mut Ctx, aggs: &[AggDef], f: &Func) -> R<String> {
    // 入口は語 1 つ (128 ビット) で受け渡しするので、枠が 64 ビットの Option / Result と整数・bool・浮動小数だけ扱う。
    let bad = |t: Ty| matches!(t, Ty::Ptr | Ty::Ref(..) | Ty::Agg(_)) || (matches!(t, Ty::Opt(_) | Ty::Res(..)) && (has_agg(t) || slot_bits(aggs, t) > 64));
    if f.params().iter().any(|(_, t)| bad(*t)) || bad(f.ret) {
        return Err(format!("関数 {} は集成体・参照・ptr を引数か戻り値に持つ (この入口では扱えない)", f.key));
    }
    let mut s = String::from("define i32 @main(i32 %argc, ptr %argv) {\nentry:\n");
    let mut args = vec![];
    for (j, (_, t)) in f.params().iter().enumerate() {
        let (lo_i, hi_i) = (1 + 2 * j, 2 + 2 * j);
        for (nm, idx) in [("lo", lo_i), ("hi", hi_i)] {
            s.push_str(&format!(
                "  %a{j}{nm}p = getelementptr ptr, ptr %argv, i64 {idx}\n  %a{j}{nm}s = load ptr, ptr %a{j}{nm}p\n  %a{j}{nm} = call i64 @strtoull(ptr %a{j}{nm}s, ptr null, i32 16)\n"
            ));
        }
        s.push_str(&format!(
            "  %a{j}l = zext i64 %a{j}lo to i128\n  %a{j}h0 = zext i64 %a{j}hi to i128\n  %a{j}h = shl i128 %a{j}h0, 64\n  %a{j}w = or i128 %a{j}l, %a{j}h\n"
        ));
        let w = format!("%a{j}w");
        let v = format!("%a{j}v");
        match t {
            Ty::I8 | Ty::U8 => s.push_str(&format!("  {v} = trunc i128 {w} to i8\n")),
            Ty::I16 | Ty::U16 => s.push_str(&format!("  {v} = trunc i128 {w} to i16\n")),
            Ty::I32 | Ty::U32 => s.push_str(&format!("  {v} = trunc i128 {w} to i32\n")),
            Ty::I64 | Ty::U64 | Ty::Isize | Ty::Usize => s.push_str(&format!("  {v} = trunc i128 {w} to i64\n")),
            Ty::I128 | Ty::U128 => s.push_str(&format!("  {v} = add i128 {w}, 0\n")),
            Ty::Bool => s.push_str(&format!("  {v} = trunc i128 {w} to i1\n")),
            Ty::Unit => s.push_str(&format!("  {v} = trunc i128 {w} to i8\n")),
            Ty::F32 => s.push_str(&format!("  %a{j}b = trunc i128 {w} to i32\n  {v} = bitcast i32 %a{j}b to float\n")),
            Ty::F64 => s.push_str(&format!("  %a{j}b = trunc i128 {w} to i64\n  {v} = bitcast i64 %a{j}b to double\n")),
            // 枠が 64 ビットなら、語の 128 ビットの下位 65 ビットが (タグ, 枠) と一致する (タグは bit 64)。
            Ty::Opt(_) | Ty::Res(..) => s.push_str(&format!("  {v} = trunc i128 {w} to {}\n", lty(aggs, *t))),
            Ty::Ptr | Ty::Ref(..) | Ty::Agg(_) => unreachable!(),
        }
        args.push(format!("{} {}", lty(aggs, *t), v));
    }
    let rt = lty(aggs, f.ret);
    let callee = if let Some(sym) = &f.link {
        format!("@{}", sym)
    } else {
        fname(&f.key)
    };
    s.push_str(&format!("  %r = call {} {}({})\n", rt, callee, args.join(", ")));
    let e = "%e";
    match f.ret {
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::Isize => s.push_str(&format!("  {e} = sext {rt} %r to i128\n")),
        Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize | Ty::Bool => s.push_str(&format!("  {e} = zext {rt} %r to i128\n")),
        Ty::I128 | Ty::U128 => s.push_str(&format!("  {e} = add i128 %r, 0\n")),
        Ty::Unit => s.push_str(&format!("  {e} = add i128 0, 0\n")),
        Ty::F32 => s.push_str(&format!("  %rb = bitcast float %r to i32\n  {e} = zext i32 %rb to i128\n")),
        Ty::F64 => s.push_str(&format!("  %rb = bitcast double %r to i64\n  {e} = zext i64 %rb to i128\n")),
        Ty::Opt(_) | Ty::Res(..) => s.push_str(&format!("  {e} = zext {rt} %r to i128\n")),
        Ty::Ptr | Ty::Ref(..) | Ty::Agg(_) => unreachable!(),
    }
    s.push_str(
        "  %elo = trunc i128 %e to i64\n  %eh0 = lshr i128 %e, 64\n  %ehi = trunc i128 %eh0 to i64\n  %pr = call i32 (ptr, ...) @printf(ptr @ksn.fmt.res, i64 %elo, i64 %ehi)\n  ret i32 0\n}\n",
    );
    let _ = cx;
    Ok(s)
}

// ---------------------------------------------------------------- 超越関数 (fmath の Ops を命令列へ)

/// `fmath` の演算の値: 一時変数 (`%x<n>`) か定数のビット列。
#[derive(Clone, Copy)]
enum Val {
    T(u32),
    C(u64),
}

fn fv(v: Val) -> String {
    match v {
        Val::T(n) => format!("%x{}", n),
        Val::C(b) => format!("0x{:016X}", b),
    }
}

fn iv(v: Val) -> String {
    match v {
        Val::T(n) => format!("%x{}", n),
        Val::C(b) => (b as i64).to_string(),
    }
}

struct LlvmOps {
    lines: RefCell<String>,
    n: Cell<u32>,
    decls: RefCell<BTreeSet<String>>,
}

impl LlvmOps {
    fn tmp(&self) -> u32 {
        let n = self.n.get();
        self.n.set(n + 1);
        n
    }

    fn emit(&self, rhs: String) -> Val {
        let n = self.tmp();
        self.lines.borrow_mut().push_str(&format!("  %x{} = {}\n", n, rhs));
        Val::T(n)
    }

    fn fbin(&self, op: &str, a: Val, b: Val) -> Val {
        self.emit(format!("{} double {}, {}", op, fv(a), fv(b)))
    }

    fn ibin(&self, op: &str, a: Val, b: Val) -> Val {
        self.emit(format!("{} i64 {}, {}", op, iv(a), iv(b)))
    }

    fn intr(&self, name: &str, ret: &str, sig: &str, args: String) -> Val {
        self.decls.borrow_mut().insert(format!("declare {} @{}({})", ret, name, sig));
        self.emit(format!("call {} @{}({})", ret, name, args))
    }
}

impl Ops for LlvmOps {
    type F = Val;
    type I = Val;
    type B = Val;
    fn c(&self, v: f64) -> Val {
        Val::C(v.to_bits())
    }
    fn ic(&self, v: i64) -> Val {
        Val::C(v as u64)
    }
    fn add(&self, a: Val, b: Val) -> Val {
        self.fbin("fadd", a, b)
    }
    fn sub(&self, a: Val, b: Val) -> Val {
        self.fbin("fsub", a, b)
    }
    fn mul(&self, a: Val, b: Val) -> Val {
        self.fbin("fmul", a, b)
    }
    fn div(&self, a: Val, b: Val) -> Val {
        self.fbin("fdiv", a, b)
    }
    fn neg(&self, a: Val) -> Val {
        self.emit(format!("fneg double {}", fv(a)))
    }
    fn floor(&self, a: Val) -> Val {
        self.intr("llvm.floor.f64", "double", "double", format!("double {}", fv(a)))
    }
    fn lt(&self, a: Val, b: Val) -> Val {
        self.fbin("fcmp olt", a, b)
    }
    fn gt(&self, a: Val, b: Val) -> Val {
        self.fbin("fcmp ogt", a, b)
    }
    fn eq(&self, a: Val, b: Val) -> Val {
        self.fbin("fcmp oeq", a, b)
    }
    fn ne(&self, a: Val, b: Val) -> Val {
        self.fbin("fcmp une", a, b)
    }
    fn sel(&self, c: Val, a: Val, b: Val) -> Val {
        self.emit(format!("select i1 {}, double {}, double {}", iv(c), fv(a), fv(b)))
    }
    fn isel(&self, c: Val, a: Val, b: Val) -> Val {
        self.emit(format!("select i1 {}, i64 {}, i64 {}", iv(c), iv(a), iv(b)))
    }
    fn to_i(&self, a: Val) -> Val {
        self.intr("llvm.fptosi.sat.i64.f64", "i64", "double", format!("double {}", fv(a)))
    }
    fn from_i(&self, a: Val) -> Val {
        self.emit(format!("sitofp i64 {} to double", iv(a)))
    }
    fn bits(&self, a: Val) -> Val {
        self.emit(format!("bitcast double {} to i64", fv(a)))
    }
    fn from_bits(&self, a: Val) -> Val {
        self.emit(format!("bitcast i64 {} to double", iv(a)))
    }
    fn iadd(&self, a: Val, b: Val) -> Val {
        self.ibin("add", a, b)
    }
    fn isub(&self, a: Val, b: Val) -> Val {
        self.ibin("sub", a, b)
    }
    fn iand(&self, a: Val, b: Val) -> Val {
        self.ibin("and", a, b)
    }
    fn ishr(&self, a: Val, n: u32) -> Val {
        self.ibin("lshr", a, Val::C(n as u64))
    }
    fn ishl(&self, a: Val, n: u32) -> Val {
        self.ibin("shl", a, Val::C(n as u64))
    }
    fn nz(&self, a: Val) -> Val {
        self.emit(format!("icmp ne i64 {}, 0", iv(a)))
    }
}

/// `fmath` の 1 関数を、`double` (と `powi` の指数 `i64`) を受ける内部関数にする。
fn emit_math(cx: &mut Ctx, mf: Math) -> String {
    let o = LlvmOps { lines: RefCell::new(String::new()), n: Cell::new(2), decls: RefCell::new(BTreeSet::new()) };
    let x = Val::T(0);
    let y = Val::T(1);
    let (sig, r) = match mf {
        Math::Sin => ("double %x0", fmath::sin(&o, x)),
        Math::Cos => ("double %x0", fmath::cos(&o, x)),
        Math::Tan => ("double %x0", fmath::tan(&o, x)),
        Math::Exp => ("double %x0", fmath::exp(&o, x)),
        Math::Ln => ("double %x0", fmath::ln(&o, x)),
        Math::Atan2 => ("double %x0, double %x1", fmath::atan2(&o, x, y)),
        Math::Powi => ("double %x0, i64 %x1", fmath::powi(&o, x, y)),
    };
    for d in o.decls.borrow().iter() {
        cx.decls.insert(d.clone());
    }
    format!("define internal double @ksn.m.{}({}) {{\nentry:\n{}  ret double {}\n}}\n", mf.name(), sig, o.lines.borrow(), fv(r))
}

// ---------------------------------------------------------------- 関数

struct Fe<'a> {
    cx: &'a mut Ctx,
    aggs: &'a [AggDef],
    f: &'a Func,
    /// `extern` 関数の `キー → LLVM シンボル名`。`Op::Call` で `declare` したシンボルを直接呼ぶ。
    extern_links: &'a HashMap<String, String>,
    fi: usize,
    tys: HashMap<VId, Ty>,
    /// 定数と unit の値 (命令を作らずに、使う側へ直接書く)。
    lit: HashMap<VId, String>,
    n: u32,
    body: String,
    cur: String,
    /// ブロックごとの入ってくる辺 (前のラベル、引数の値)。
    edges: BTreeMap<usize, Vec<(String, Vec<String>)>>,
}

impl<'a> Fe<'a> {
    fn new(cx: &'a mut Ctx, aggs: &'a [AggDef], f: &'a Func, extern_links: &'a HashMap<String, String>, fi: usize) -> Fe<'a> {
        Fe { cx, aggs, f, extern_links, fi, tys: HashMap::new(), lit: HashMap::new(), n: 0, body: String::new(), cur: String::new(), edges: BTreeMap::new() }
    }

    fn lt(&self, t: Ty) -> String {
        lty(self.aggs, t)
    }

    fn mt(&self, t: Ty) -> String {
        mem_ty(self.aggs, t)
    }

    fn r(&self, v: VId) -> String {
        self.lit.get(&v).cloned().unwrap_or_else(|| format!("%v{}", v))
    }

    fn ty(&self, v: VId) -> Ty {
        self.tys[&v]
    }

    fn tmp(&mut self) -> String {
        self.n += 1;
        format!("%t{}", self.n)
    }

    fn line(&mut self, s: String) {
        self.body.push_str("  ");
        self.body.push_str(&s);
        self.body.push('\n');
    }

    fn label(&mut self, l: &str) {
        self.body.push_str(l);
        self.body.push_str(":\n");
        self.cur = l.to_string();
    }

    /// `rhs` の結果を一時変数に置いて返す。
    fn t(&mut self, rhs: String) -> String {
        let t = self.tmp();
        self.line(format!("{} = {}", t, rhs));
        t
    }

    fn decl(&mut self, s: String) {
        self.cx.decl(s);
    }

    /// 同じ値を別の名前で持つ (同じ型の bitcast は LLVM が畳む)。
    fn mov(&mut self, dst: &str, ty: &str, v: &str) {
        if ty.starts_with('i') && !ty.contains('{') {
            self.line(format!("{} = add {} {}, 0", dst, ty, v));
        } else {
            self.line(format!("{} = bitcast {} {} to {}", dst, ty, v, ty));
        }
    }

    /// 条件 `cond` が真なら文言 `name` の panic。以降は新しいブロックで続く。
    fn trap(&mut self, cond: &str, name: &str) {
        self.n += 1;
        let (p, c) = (format!("p{}", self.n), format!("c{}", self.n));
        self.line(format!("br i1 {}, label %{}, label %{}", cond, p, c));
        self.label(&p);
        self.line(format!("call void @ksn.panic.{}()", name));
        self.line("unreachable".into());
        self.label(&c);
    }

    /// 整数の幅の変換 (`signed` は拡張の仕方)。幅が同じなら値をそのまま名前だけ付ける。
    fn int_cast(&mut self, dst: &str, v: &str, from: u32, to: u32, signed: bool) {
        if from == to {
            self.mov(dst, &format!("i{}", to), v);
        } else if from < to {
            self.line(format!("{} = {} i{} {} to i{}", dst, if signed { "sext" } else { "zext" }, from, v, to));
        } else {
            self.line(format!("{} = trunc i{} {} to i{}", dst, from, v, to));
        }
    }

    // ---- Option / Result のペイロード

    /// ペイロード (型 `t`、値 `v`) を枠 `s` ビットの整数にする (設計_KIR.md 11.6)。
    /// 符号付き整数は符号拡張 (参照実行器の語と同じ)、その他の整数・集成体・入れ子はゼロ拡張、浮動小数と参照はビット列、単位型は 0。
    fn pay_to_slot(&mut self, t: Ty, v: &str, s: u32) -> R<String> {
        let w = vbits(self.aggs, t);
        if w == 0 {
            return Ok("0".into());
        }
        let x = match t {
            Ty::F32 => self.t(format!("bitcast float {} to i32", v)),
            Ty::F64 => self.t(format!("bitcast double {} to i64", v)),
            Ty::Ptr | Ty::Ref(..) => self.t(format!("ptrtoint ptr {} to i64", v)),
            _ => v.to_string(),
        };
        if w > s {
            return Err(format!("Option / Result の枠 {} ビットに {} が入らない", s, t.name()));
        }
        if w == s {
            return Ok(x);
        }
        let op = if t.is_int() && t.signed() { "sext" } else { "zext" };
        Ok(self.t(format!("{} i{} {} to i{}", op, w, x, s)))
    }

    /// 枠 `s` ビットの語 `w` から、型 `t` の値を読む (`pay_to_slot` の逆)。単位型は 0。
    fn slot_to_pay(&mut self, t: Ty, w: &str, s: u32) -> String {
        let n = vbits(self.aggs, t);
        if n == 0 {
            return "0".into();
        }
        let x = if n == s { w.to_string() } else { self.t(format!("trunc i{} {} to i{}", s, w, n)) };
        match t {
            Ty::F32 => self.t(format!("bitcast i32 {} to float", x)),
            Ty::F64 => self.t(format!("bitcast i64 {} to double", x)),
            Ty::Ptr | Ty::Ref(..) => self.t(format!("inttoptr i64 {} to ptr", x)),
            _ => x,
        }
    }

    /// `Option` / `Result` の値 (幅 `s + 1`): 枠 `pay` (幅 `s`) と、タグ `tag` (i1) を最上位に置く。
    fn pack(&mut self, dst: &str, s: u32, tag: &str, pay: &str) {
        let n = s + 1;
        let z = self.t(format!("zext i{} {} to i{}", s, pay, n));
        let tg = self.t(format!("zext i1 {} to i{}", tag, n));
        let top = self.t(format!("shl i{} {}, {}", n, tg, s));
        self.line(format!("{} = or i{} {}, {}", dst, n, z, top));
    }

    // ---- 集成体 (大きさ × 8 ビットの整数)

    fn field_w(&self, t: Ty) -> R<u32> {
        match t {
            Ty::Unit | Ty::Opt(_) | Ty::Res(..) | Ty::Ref(..) => Err(format!("型 {} は集成体のフィールドにできない", t.name())),
            t => Ok(size_of(self.aggs, t) * 8),
        }
    }

    /// フィールドの値を、その幅の整数にする。
    fn to_bits(&mut self, t: Ty, v: &str) -> R<String> {
        Ok(match t {
            Ty::Bool => self.t(format!("zext i1 {} to i8", v)),
            Ty::F32 => self.t(format!("bitcast float {} to i32", v)),
            Ty::F64 => self.t(format!("bitcast double {} to i64", v)),
            Ty::Ptr => self.t(format!("ptrtoint ptr {} to i64", v)),
            t => {
                self.field_w(t)?;
                v.to_string()
            }
        })
    }

    fn from_bits(&mut self, dst: &str, t: Ty, b: &str) {
        match t {
            Ty::Bool => self.line(format!("{} = icmp ne i8 {}, 0", dst, b)),
            Ty::F32 => self.line(format!("{} = bitcast i32 {} to float", dst, b)),
            Ty::F64 => self.line(format!("{} = bitcast i64 {} to double", dst, b)),
            Ty::Ptr => self.line(format!("{} = inttoptr i64 {} to ptr", dst, b)),
            t => {
                let ty = self.lt(t);
                self.mov(dst, &ty, b)
            }
        }
    }

    /// 集成体 `a` (`n` ビットの整数) のバイト `off` から `w` ビットを取り出す。
    fn extract(&mut self, a: &str, n: u32, off: u32, w: u32) -> String {
        let mut v = a.to_string();
        if off > 0 {
            v = self.t(format!("lshr i{} {}, {}", n, v, off * 8));
        }
        if w < n {
            v = self.t(format!("trunc i{} {} to i{}", n, v, w));
        }
        v
    }

    /// `w` ビットの値を `n` ビットへ広げて、バイト `off` に置く。
    fn place(&mut self, v: &str, w: u32, n: u32, off: u32) -> String {
        let mut x = v.to_string();
        if w < n {
            x = self.t(format!("zext i{} {} to i{}", w, x, n));
        }
        if off > 0 {
            x = self.t(format!("shl i{} {}, {}", n, x, off * 8));
        }
        x
    }

    // ---- 命令

    fn arith(&mut self, i: &Inst, op: Aop, mode: Mode, a: VId, b: VId) {
        let ty = i.ty;
        let t = self.lt(ty);
        let w = ty.bits();
        let (x, y) = (self.r(a), self.r(b));
        let d = format!("%v{}", i.dst);
        let s = if ty.signed() { "s" } else { "u" };
        let oname = match op {
            Aop::Add => "add",
            Aop::Sub => "sub",
            Aop::Mul => "mul",
            Aop::Div => "div",
            Aop::Rem => "rem",
        };
        let pair = format!("{{ {}, i1 }}", t);
        match mode {
            Mode::Wrap => self.line(format!("{} = {} {} {}, {}", d, oname, t, x, y)),
            Mode::Trap => match op {
                Aop::Add | Aop::Sub | Aop::Mul => {
                    let name = format!("llvm.{}{}.with.overflow.i{}", s, oname, w);
                    self.decl(format!("declare {} @{}({}, {})", pair, name, t, t));
                    let p = self.t(format!("call {} @{}({} {}, {} {})", pair, name, t, x, t, y));
                    self.line(format!("{} = extractvalue {} {}, 0", d, pair, p));
                    let f = self.t(format!("extractvalue {} {}, 1", pair, p));
                    self.trap(&f, oname);
                }
                Aop::Div | Aop::Rem => {
                    let z = self.t(format!("icmp eq {} {}, 0", t, y));
                    self.trap(&z, if op == Aop::Div { "div0" } else { "rem0" });
                    if ty.signed() {
                        let m = self.t(format!("icmp eq {} {}, {}", t, x, int_lit(ty, ty.min())));
                        let n1 = self.t(format!("icmp eq {} {}, -1", t, y));
                        let o = self.t(format!("and i1 {}, {}", m, n1));
                        self.trap(&o, oname);
                    }
                    let k = match (op, ty.signed()) {
                        (Aop::Div, true) => "sdiv",
                        (Aop::Div, false) => "udiv",
                        (_, true) => "srem",
                        (_, false) => "urem",
                    };
                    self.line(format!("{} = {} {} {}, {}", d, k, t, x, y));
                }
            },
            Mode::Sat => match op {
                Aop::Add | Aop::Sub => {
                    let name = format!("llvm.{}{}.sat.i{}", s, oname, w);
                    self.decl(format!("declare {} @{}({}, {})", t, name, t, t));
                    self.line(format!("{} = call {} @{}({} {}, {} {})", d, t, name, t, x, t, y));
                }
                _ => {
                    let name = format!("llvm.{}mul.with.overflow.i{}", s, w);
                    self.decl(format!("declare {} @{}({}, {})", pair, name, t, t));
                    let p = self.t(format!("call {} @{}({} {}, {} {})", pair, name, t, x, t, y));
                    let r = self.t(format!("extractvalue {} {}, 0", pair, p));
                    let o = self.t(format!("extractvalue {} {}, 1", pair, p));
                    let sat = if ty.signed() {
                        let xn = self.t(format!("icmp slt {} {}, 0", t, x));
                        let yn = self.t(format!("icmp slt {} {}, 0", t, y));
                        let ng = self.t(format!("xor i1 {}, {}", xn, yn));
                        self.t(format!("select i1 {}, {} {}, {} {}", ng, t, int_lit(ty, ty.min()), t, int_lit(ty, ty.max())))
                    } else {
                        int_lit(ty, ty.max())
                    };
                    self.line(format!("{} = select i1 {}, {} {}, {} {}", d, o, t, sat, t, r));
                }
            },
        }
    }

    fn checked(&mut self, i: &Inst, op: Aop, a: VId, b: VId) -> R<()> {
        let Ty::Opt(p) = i.ty else { return Err("checked の結果が Option でない".into()) };
        let pt = p.ty();
        let t = self.lt(pt);
        let w = pt.bits();
        let (x, y) = (self.r(a), self.r(b));
        let s = if pt.signed() { "s" } else { "u" };
        let (res, ok) = match op {
            Aop::Add | Aop::Sub | Aop::Mul => {
                let oname = match op {
                    Aop::Add => "add",
                    Aop::Sub => "sub",
                    _ => "mul",
                };
                let pair = format!("{{ {}, i1 }}", t);
                let name = format!("llvm.{}{}.with.overflow.i{}", s, oname, w);
                self.decl(format!("declare {} @{}({}, {})", pair, name, t, t));
                let pr = self.t(format!("call {} @{}({} {}, {} {})", pair, name, t, x, t, y));
                let r = self.t(format!("extractvalue {} {}, 0", pair, pr));
                let o = self.t(format!("extractvalue {} {}, 1", pair, pr));
                let ok = self.t(format!("xor i1 {}, true", o));
                (r, ok)
            }
            Aop::Div | Aop::Rem => {
                let mut bad = self.t(format!("icmp eq {} {}, 0", t, y));
                if pt.signed() {
                    let m = self.t(format!("icmp eq {} {}, {}", t, x, int_lit(pt, pt.min())));
                    let n1 = self.t(format!("icmp eq {} {}, -1", t, y));
                    let o = self.t(format!("and i1 {}, {}", m, n1));
                    bad = self.t(format!("or i1 {}, {}", bad, o));
                }
                let y2 = self.t(format!("select i1 {}, {} 1, {} {}", bad, t, t, y));
                let k = match (op, pt.signed()) {
                    (Aop::Div, true) => "sdiv",
                    (Aop::Div, false) => "udiv",
                    (_, true) => "srem",
                    (_, false) => "urem",
                };
                let r = self.t(format!("{} {} {}, {}", k, t, x, y2));
                let ok = self.t(format!("xor i1 {}, true", bad));
                (r, ok)
            }
        };
        // 整数の結果 (枠は 64 ビット)。溢れや 0 除算のときは枠を 0 にする。
        let pay = self.pay_to_slot(pt, &res, 64)?;
        let pay0 = self.t(format!("select i1 {}, i64 {}, i64 0", ok, pay));
        self.pack(&format!("%v{}", i.dst), 64, &ok, &pay0);
        Ok(())
    }

    /// 浮動小数の被演算子を `double` へ広げる。
    fn widen(&mut self, src: Ty, v: &str) -> String {
        if src == Ty::F32 {
            self.t(format!("fpext float {} to double", v))
        } else {
            v.to_string()
        }
    }

    /// `double` の結果を型 `res` へ丸めて `dst` に置く。
    fn narrow(&mut self, dst: &str, res: Ty, v: &str) {
        if res == Ty::F32 {
            self.line(format!("{} = fptrunc double {} to float", dst, v));
        } else {
            self.mov(dst, "double", v);
        }
    }

    /// NaN の規則 (`fmath::nan_rule64` / `nan_rule32` と同じ、設計_KIR.md 17.8): 結果 `r` が NaN のとき、`a` が NaN なら `a` (quiet 化)、
    /// そうでなく `b` が NaN なら `b` (quiet 化)、どちらでもなければ正の quiet NaN。結果が NaN でなければ `r`。
    /// 定数畳み込み (正の NaN) と実機の実行時 (符号ビットの立った NaN) の違いを消す。`t` は `float` か `double`。
    fn nan_rule(&mut self, t: &str, a: &str, b: &str, r: &str) -> String {
        let (it, quiet, canon) = if t == "float" { ("i32", 0x0040_0000u64, 0x7FC0_0000u64) } else { ("i64", 0x0008_0000_0000_0000u64, 0x7FF8_0000_0000_0000u64) };
        let rn = self.t(format!("fcmp uno {} {}, {}", t, r, r));
        let an = self.t(format!("fcmp uno {} {}, {}", t, a, a));
        let bn = self.t(format!("fcmp uno {} {}, {}", t, b, b));
        let ab = self.t(format!("bitcast {} {} to {}", t, a, it));
        let aq = self.t(format!("or {} {}, {}", it, ab, quiet));
        let af = self.t(format!("bitcast {} {} to {}", it, aq, t));
        let bb = self.t(format!("bitcast {} {} to {}", t, b, it));
        let bq = self.t(format!("or {} {}, {}", it, bb, quiet));
        let bf = self.t(format!("bitcast {} {} to {}", it, bq, t));
        let cb = self.t(format!("or {} 0, {}", it, canon));
        let cf = self.t(format!("bitcast {} {} to {}", it, cb, t));
        let s1 = self.t(format!("select i1 {}, {} {}, {} {}", bn, t, bf, t, cf));
        let s2 = self.t(format!("select i1 {}, {} {}, {} {}", an, t, af, t, s1));
        self.t(format!("select i1 {}, {} {}, {} {}", rn, t, s2, t, r))
    }

    fn math_call(&mut self, m: Math, args: &str) -> String {
        self.cx.math.insert(m);
        self.t(format!("call double @ksn.m.{}({})", m.name(), args))
    }

    fn f64_intr(&mut self, name: &str, v: &str) -> String {
        let full = format!("llvm.{}.f64", name);
        self.decl(format!("declare double @{}(double)", full));
        self.t(format!("call double @{}(double {})", full, v))
    }

    fn funary(&mut self, i: &Inst, f: FUn, src: Ty, a: VId) {
        let d = format!("%v{}", i.dst);
        let x = self.r(a);
        let st = self.lt(src);
        match f {
            FUn::Neg => self.line(format!("{} = fneg {} {}", d, st, x)),
            FUn::IsNan => self.line(format!("{} = fcmp uno {} {}, {}", d, st, x, x)),
            FUn::IsFinite => {
                let full = format!("llvm.fabs.{}", if src == Ty::F32 { "f32" } else { "f64" });
                self.decl(format!("declare {} @{}({})", st, full, st));
                let ab = self.t(format!("call {} @{}({} {})", st, full, st, x));
                self.line(format!("{} = fcmp olt {} {}, 0x7FF0000000000000", d, st, ab));
            }
            // abs は符号ビットを落とすだけ (f64 へ広げず、整数のビット演算で。signaling NaN も、最適化の水準にもよらず変わらない)。
            FUn::Abs => {
                let (it, mask) = if src == Ty::F32 { ("i32", 0x7FFF_FFFFu64) } else { ("i64", 0x7FFF_FFFF_FFFF_FFFFu64) };
                let b = self.t(format!("bitcast {} {} to {}", st, x, it));
                let m = self.t(format!("and {} {}, {}", it, b, mask));
                self.line(format!("{} = bitcast {} {} to {}", d, it, m, st));
            }
            _ => {
                let w = self.widen(src, &x);
                let r = match f {
                    FUn::Abs => unreachable!(),
                    FUn::Sqrt => {
                        let r = self.f64_intr("sqrt", &w);
                        self.nan_rule("double", &w, &w, &r)
                    }
                    FUn::Floor => self.f64_intr("floor", &w),
                    FUn::Ceil => self.f64_intr("ceil", &w),
                    FUn::Trunc => self.f64_intr("trunc", &w),
                    FUn::RoundEven => self.f64_intr("roundeven", &w),
                    FUn::Sin => self.math_call(Math::Sin, &format!("double {}", w)),
                    FUn::Cos => self.math_call(Math::Cos, &format!("double {}", w)),
                    FUn::Tan => self.math_call(Math::Tan, &format!("double {}", w)),
                    FUn::Exp => self.math_call(Math::Exp, &format!("double {}", w)),
                    FUn::Ln => self.math_call(Math::Ln, &format!("double {}", w)),
                    FUn::Neg | FUn::IsNan | FUn::IsFinite => unreachable!(),
                };
                self.narrow(&d, i.ty, &r);
            }
        }
    }

    fn fbin(&mut self, i: &Inst, f: FBin, a: VId, b: VId) {
        let d = format!("%v{}", i.dst);
        let x = self.widen(i.ty, &self.r(a));
        let r = match f {
            FBin::Powi => {
                let n = self.t(format!("sext i32 {} to i64", self.r(b)));
                self.math_call(Math::Powi, &format!("double {}, i64 {}", x, n))
            }
            _ => {
                let y = self.widen(i.ty, &self.r(b));
                match f {
                    FBin::Max | FBin::Min => {
                        let nm = if f == FBin::Max { "maxnum" } else { "minnum" };
                        let full = format!("llvm.{}.f64", nm);
                        self.decl(format!("declare double @{}(double, double)", full));
                        self.t(format!("call double @{}(double {}, double {})", full, x, y))
                    }
                    FBin::Rem => {
                        let q = self.t(format!("frem double {}, {}", x, y));
                        self.nan_rule("double", &x, &y, &q)
                    }
                    FBin::Atan2 => self.math_call(Math::Atan2, &format!("double {}, double {}", x, y)),
                    FBin::Powi => unreachable!(),
                }
            }
        };
        self.narrow(&d, i.ty, &r);
    }

    fn farith(&mut self, i: &Inst, op: Aop, a: VId, b: VId) {
        let d = format!("%v{}", i.dst);
        let t = self.lt(i.ty);
        let (x, y) = (self.r(a), self.r(b));
        match op {
            _ => {
                let ins = match op {
                    Aop::Add => "fadd",
                    Aop::Sub => "fsub",
                    Aop::Mul => "fmul",
                    _ => "fdiv",
                };
                let q = self.t(format!("{} {} {}, {}", ins, t, x, y));
                // NaN になる結果は、木走査・参照実行器と同じ規則のビット列にする (17.8)。
                let r = self.nan_rule(&t, &x, &y, &q);
                self.mov(&d, &t, &r);
            }
        }
    }

    fn conv(&mut self, i: &Inst, src: Ty, a: VId) {
        let d = format!("%v{}", i.dst);
        let v = self.r(a);
        let dst = i.ty;
        if src.is_int() && dst.is_int() {
            self.int_cast(&d, &v, src.bits(), dst.bits(), src.signed());
        } else if src.is_int() {
            let st = self.lt(src);
            let k = if src.signed() { "sitofp" } else { "uitofp" };
            if dst == Ty::F32 {
                let w = self.t(format!("{} {} {} to double", k, st, v));
                self.line(format!("{} = fptrunc double {} to float", d, w));
            } else {
                self.line(format!("{} = {} {} {} to double", d, k, st, v));
            }
        } else if src == dst {
            let t = self.lt(src);
            self.mov(&d, &t, &v);
        } else if src == Ty::F32 {
            self.line(format!("{} = fpext float {} to double", d, v));
        } else {
            self.line(format!("{} = fptrunc double {} to float", d, v));
        }
    }

    fn bitcount(&mut self, i: &Inst, k: BitK, src: Ty, a: VId) {
        let d = format!("%v{}", i.dst);
        let t = self.lt(src);
        let w = src.bits();
        let v = self.r(a);
        let (nm, extra) = match k {
            BitK::Ones => ("ctpop", ""),
            BitK::Leading => ("ctlz", ", i1 false"),
            BitK::Trailing => ("cttz", ", i1 false"),
        };
        let name = format!("llvm.{}.i{}", nm, w);
        if extra.is_empty() {
            self.decl(format!("declare {} @{}({})", t, name, t));
        } else {
            self.decl(format!("declare {} @{}({}, i1)", t, name, t));
        }
        let r = self.t(format!("call {} @{}({} {}{})", t, name, t, v, extra));
        self.int_cast(&d, &r, w, 32, false);
    }

    fn shift(&mut self, i: &Inst, left: bool, a: VId, n: VId) {
        let d = format!("%v{}", i.dst);
        let t = self.lt(i.ty);
        let w = i.ty.bits();
        let amt = self.t(format!("urem i32 {}, {}", self.r(n), w));
        let amt2 = self.tmp();
        self.int_cast(&amt2, &amt, 32, w, false);
        let k = if left {
            "shl"
        } else if i.ty.signed() {
            "ashr"
        } else {
            "lshr"
        };
        self.line(format!("{} = {} {} {}, {}", d, k, t, self.r(a), amt2));
    }

    /// 命令 1 つを出す。
    fn inst(&mut self, i: &Inst) -> R<()> {
        let d = format!("%v{}", i.dst);
        let aggs = self.aggs;
        // unit の結果は値を持たない (使う側が 0 を書く)。副作用のある命令だけ出す。
        if i.ty == Ty::Unit && !matches!(i.op, Op::Call { .. }) {
            return match &i.op {
                Op::Store { ptr, val } => self.store(*ptr, *val),
                Op::MemStore { ty, p, off, v } => {
                    let (pp, vv) = (self.r(*p), self.r(*v));
                    let addr = self.t(format!("getelementptr i8, ptr {}, i64 {}", pp, off));
                    self.mem_store(*ty, &addr, &vv)
                }
                Op::HeapFree(p) => {
                    let pp = self.r(*p);
                    self.line(format!("call void @free(ptr {})", pp));
                    Ok(())
                }
                Op::IndexCheck { i: ix, len } => {
                    let (x, n) = (self.r(*ix), self.r(*len));
                    let c = self.t(format!("icmp uge i64 {}, {}", x, n));
                    self.n += 1;
                    let (p, k) = (format!("p{}", self.n), format!("c{}", self.n));
                    self.line(format!("br i1 {}, label %{}, label %{}", c, p, k));
                    self.label(&p);
                    self.line(format!("call void @ksn.panic.index(i64 {}, i64 {})", n, x));
                    self.line("unreachable".into());
                    self.label(&k);
                    Ok(())
                }
                Op::Eprint { data } => {
                    // panic の文言の一部を stderr へ書く (`@ksn.eprint`。エラーは捨てる)。
                    self.cx.eprint = true;
                    self.cx.decl("declare i64 @write(i32, ptr, i64)");
                    let av = self.r(*data);
                    let pi = self.extract(&av, 128, 0, 64);
                    let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                    let n = self.extract(&av, 128, 8, 64);
                    self.line(format!("call void @ksn.eprint(ptr {}, i64 {})", p, n));
                    Ok(())
                }
                Op::Abort => {
                    // 文言は出さず、終了コード 101 で終わる (Panic と同じく、後ろは新しいラベルから続ける)
                    let n = self.n + 1;
                    self.n = n;
                    let (p, k) = (format!("p{}", n), format!("c{}", n));
                    self.line(format!("br label %{}", p));
                    self.label(&p);
                    self.line("call void @exit(i32 101)".into());
                    self.line("unreachable".into());
                    self.label(&k);
                    Ok(())
                }
                Op::Panic { msg, arg } => {
                    // 文言は `panic: <msg>` (値があれば `: %lld` / `%llu`)。`%` は書式で `%%` にする。
                    let mut fmt = format!("panic: {}", msg.replace('%', "%%"));
                    let mut vals = String::new();
                    if let Some(v) = arg {
                        let vt = *self.tys.get(v).ok_or("panic の値の型が分からない")?;
                        if vt.bits() > 64 {
                            return Err("panic の値が 64 ビットを超える".into());
                        }
                        let x = self.r(*v);
                        let x = if vt.bits() == 64 {
                            x
                        } else {
                            let ext = if vt.signed() { "sext" } else { "zext" };
                            self.t(format!("{} i{} {} to i64", ext, vt.bits(), x))
                        };
                        vals = format!(", i64 {}", x);
                        fmt.push_str(if vt.signed() { ": %lld" } else { ": %llu" });
                    }
                    fmt.push('\n');
                    let g = format!("ksn.pm.{}.{}", self.fi, i.dst);
                    self.cx.globals.push_str(&str_const(&g, &format!("{}\0", fmt)));
                    self.n += 1;
                    let (p, k) = (format!("p{}", self.n), format!("c{}", self.n));
                    self.line(format!("br label %{}", p));
                    self.label(&p);
                    self.line(format!("call i32 (i32, ptr, ...) @dprintf(i32 2, ptr @{}{})", g, vals));
                    self.line("call void @exit(i32 101)".into());
                    self.line("unreachable".into());
                    self.label(&k);
                    Ok(())
                }
                Op::FmtFloat { bits, a } => {
                    self.cx.fmt_float = true;
                    let (func, arg_ty) = if *bits == 64 {
                        ("@ksn_fmt_f64", "double")
                    } else {
                        ("@ksn_fmt_f32", "float")
                    };
                    let fp = self.t(format!("call ptr {}({} {})", func, arg_ty, self.r(*a)));
                    self.cx.decl("declare i64 @strlen(ptr)".to_string());
                    let len = self.t(format!("call i64 @strlen(ptr {})", fp));
                    let fp_i = self.t(format!("ptrtoint ptr {} to i64", fp));
                    let ptr_i128 = self.t(format!("zext i64 {} to i128", fp_i));
                    let len_i128 = self.t(format!("zext i64 {} to i128", len));
                    let len_shl = self.t(format!("shl i128 {}, 64", len_i128));
                    let fat = self.t(format!("or i128 {}, {}", len_shl, ptr_i128));
                    self.cx.eprint = true;
                    self.cx.decl("declare i64 @write(i32, ptr, i64)".to_string());
                    let pi = self.extract(&fat, 128, 0, 64);
                    let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                    let n = self.extract(&fat, 128, 8, 64);
                    self.line(format!("call void @ksn.eprint(ptr {}, i64 {})", p, n));
                    Ok(())
                }
                _ => Ok(()),
            };
        }
        match &i.op {
            Op::Const(_) => {}
            Op::ParseInt { ty, s, radix } => {
                self.cx.parse_int = true;
                self.cx.decl("declare i32 @ksn_parse_int(ptr, i64, i32, i32, i32, ptr, ptr)".to_string());
                let av = self.r(*s);
                let pi = self.extract(&av, 128, 0, 64);
                let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                let n = self.extract(&av, 128, 8, 64);
                let rv = self.r(*radix);
                let bits = ty.bits();
                let iss = if ty.signed() { 1 } else { 0 };
                let lo_ptr = self.t("alloca i64".to_string());
                let hi_ptr = self.t("alloca i64".to_string());
                let ret = self.t(format!(
                    "call i32 @ksn_parse_int(ptr {}, i64 {}, i32 {}, i32 {}, i32 {}, ptr {}, ptr {})",
                    p, n, rv, bits, iss, lo_ptr, hi_ptr
                ));
                let lo = self.t(format!("load i64, i64* {}", lo_ptr));
                let is_ok = self.t(format!("icmp eq i32 {}, 0", ret));
                let tag = self.t(format!("xor i1 {}, true", is_ok));
                let kind = self.t(format!("sub i32 {}, 1", ret));
                let kind_z = self.t(format!("zext i32 {} to i64", kind));
                let pay = self.t(format!("select i1 {}, i64 {}, i64 {}", is_ok, lo, kind_z));
                self.pack(&d, 64, &tag, &pay);
            }
            Op::Arith { op, mode, a, b } => self.arith(i, *op, *mode, *a, *b),
            Op::FArith { op, a, b } => self.farith(i, *op, *a, *b),
            Op::Cmp { cc, ty, a, b } => {
                let t = self.lt(*ty);
                let (x, y) = (self.r(*a), self.r(*b));
                if ty.is_float() {
                    let p = match cc {
                        Cc::Eq => "oeq",
                        Cc::Ne => "une",
                        Cc::Lt => "olt",
                        Cc::Le => "ole",
                        Cc::Gt => "ogt",
                        Cc::Ge => "oge",
                    };
                    self.line(format!("{} = fcmp {} {} {}, {}", d, p, t, x, y));
                } else {
                    let sg = ty.signed();
                    let p = match cc {
                        Cc::Eq => "eq",
                        Cc::Ne => "ne",
                        Cc::Lt => if sg { "slt" } else { "ult" },
                        Cc::Le => if sg { "sle" } else { "ule" },
                        Cc::Gt => if sg { "sgt" } else { "ugt" },
                        Cc::Ge => if sg { "sge" } else { "uge" },
                    };
                    self.line(format!("{} = icmp {} {} {}, {}", d, p, t, x, y));
                }
            }
            Op::Neg { a, mode } => {
                let t = self.lt(i.ty);
                let x = self.r(*a);
                if *mode != Mode::Wrap {
                    let m = self.t(format!("icmp eq {} {}, {}", t, x, int_lit(i.ty, i.ty.min())));
                    self.trap(&m, "neg");
                }
                self.line(format!("{} = sub {} 0, {}", d, t, x));
            }
            Op::Shift { left, a, n } => self.shift(i, *left, *a, *n),
            Op::MinMax { max, a, b } => {
                let t = self.lt(i.ty);
                let name = format!("llvm.{}{}.i{}", if i.ty.signed() { "s" } else { "u" }, if *max { "max" } else { "min" }, i.ty.bits());
                self.decl(format!("declare {} @{}({}, {})", t, name, t, t));
                let (x, y) = (self.r(*a), self.r(*b));
                self.line(format!("{} = call {} @{}({} {}, {} {})", d, t, name, t, x, t, y));
            }
            Op::FUn { f, src, a } => self.funary(i, *f, *src, *a),
            Op::FBin { f, a, b } => self.fbin(i, *f, *a, *b),
            Op::Conv { src, a } => self.conv(i, *src, *a),
            Op::Bitcast { src, a } => {
                let (st, dt) = (self.lt(*src), self.lt(i.ty));
                let v = self.r(*a);
                self.line(format!("{} = bitcast {} {} to {}", d, st, v, dt));
            }
            Op::Not(a) => {
                let t = self.lt(i.ty);
                let x = self.r(*a);
                let ones = if i.ty == Ty::Bool { "true" } else { "-1" };
                self.line(format!("{} = xor {} {}, {}", d, t, x, ones));
            }
            Op::Bool(bop, a, b) => {
                let t = self.lt(i.ty);
                let k = match bop {
                    BoolOp::And => "and",
                    BoolOp::Or => "or",
                    BoolOp::Xor => "xor",
                };
                self.line(format!("{} = {} {} {}, {}", d, k, t, self.r(*a), self.r(*b)));
            }
            Op::BitCount { k, src, a } => self.bitcount(i, *k, *src, *a),
            Op::Call { callee, args } => {
                let rt = self.lt(i.ty);
                let a: Vec<String> = args.iter().map(|v| format!("{} {}", lty(aggs, self.ty(*v)), self.r(*v))).collect();
                if let Some(sym) = self.extern_links.get(callee) {
                    self.line(format!("{} = call {} @{}({})", d, rt, sym, a.join(", ")));
                } else {
                    self.line(format!("{} = call {} {}({})", d, rt, fname(callee), a.join(", ")));
                }
            }
            Op::Make { variant, fields } => {
                let Ty::Agg(ai) = i.ty else { return Err("make の結果が集成体でない".into()) };
                let def = &aggs[ai as usize];
                let n = agg_bits(aggs, ai);
                let fdefs = match (&def.kind, variant) {
                    (AggKind::Enum(vs), Some(k)) => &vs[*k as usize].fields,
                    (AggKind::Struct(fs) | AggKind::Tuple(fs), None) => fs,
                    _ => return Err("make の変種が集成体の種類と合わない".into()),
                };
                let mut acc = "0".to_string();
                let mut parts: Vec<String> = vec![];
                if let Some(k) = variant {
                    parts.push(self.place(&k.to_string(), 32, n, 0));
                }
                for (fd, a) in fdefs.iter().zip(fields) {
                    let v = self.r(*a);
                    let bits = self.to_bits(fd.ty, &v)?;
                    let w = self.field_w(fd.ty)?;
                    parts.push(self.place(&bits, w, n, fd.off));
                }
                for p in parts {
                    acc = if acc == "0" { p } else { self.t(format!("or i{} {}, {}", n, acc, p)) };
                }
                self.mov(&d, &format!("i{}", n), &acc);
            }
            Op::Field { a, off } => {
                let Ty::Agg(ai) = self.ty(*a) else { return Err("field の対象が集成体でない".into()) };
                let n = agg_bits(aggs, ai);
                let w = self.field_w(i.ty)?;
                let av = self.r(*a);
                let x = self.extract(&av, n, *off, w);
                self.from_bits(&d, i.ty, &x);
            }
            Op::SetField { a, off, v } => {
                let Ty::Agg(ai) = i.ty else { return Err("setfield の結果が集成体でない".into()) };
                let n = agg_bits(aggs, ai);
                let vt = self.ty(*v);
                let w = self.field_w(vt)?;
                let vv = self.r(*v);
                let bits = self.to_bits(vt, &vv)?;
                let av = self.r(*a);
                if w == n && *off == 0 {
                    self.mov(&d, &format!("i{}", n), &bits);
                } else {
                    let z = self.t(format!("zext i{} -1 to i{}", w, n));
                    let sh = if *off > 0 { self.t(format!("shl i{} {}, {}", n, z, off * 8)) } else { z };
                    let inv = self.t(format!("xor i{} {}, -1", n, sh));
                    let kept = self.t(format!("and i{} {}, {}", n, av, inv));
                    let put = self.place(&bits, w, n, *off);
                    self.line(format!("{} = or i{} {}, {}", d, n, kept, put));
                }
            }
            Op::Tag(a) => match self.ty(*a) {
                Ty::Agg(ai) => {
                    let n = agg_bits(aggs, ai);
                    let av = self.r(*a);
                    let x = self.extract(&av, n, 0, 32);
                    self.mov(&d, "i32", &x);
                }
                _ => {
                    // Option / Result: タグは最上位の 1 ビット (枠の直上)
                    let s = slot_bits(aggs, self.ty(*a));
                    let av = self.r(*a);
                    let sh = self.t(format!("lshr i{} {}, {}", s + 1, av, s));
                    self.line(format!("{} = trunc i{} {} to i1", d, s + 1, sh));
                }
            },
            Op::Alloca => return Err("alloca は関数の本体の組み立てで出す".into()),
            Op::Load(p) => {
                let Ty::Ref(_, q) = self.ty(*p) else { return Err("load の被演算子が参照でない".into()) };
                let mt = self.mt(q.ty());
                let pv = self.r(*p);
                if q.ty() == Ty::Bool {
                    let x = self.t(format!("load {}, ptr {}, align 1", mt, pv));
                    self.line(format!("{} = icmp ne i8 {}, 0", d, x));
                } else {
                    self.line(format!("{} = load {}, ptr {}, align 1", d, mt, pv));
                }
            }
            Op::AsShared(p) | Op::Reborrow(p) => {
                let pv = self.r(*p);
                self.line(format!("{} = getelementptr i8, ptr {}, i64 0", d, pv));
            }
            // フィールドのアドレス: 集成体の先頭 + バイトオフセット (配置は型表のとおり)。
            Op::FieldAddr { a, off } => {
                let pv = self.r(*a);
                self.line(format!("{} = getelementptr i8, ptr {}, i64 {}", d, pv, off));
            }
            Op::Some(a) | Op::Ok(a) | Op::Err(a) => {
                let s = slot_bits(aggs, i.ty);
                let v = self.r(*a);
                let pay = self.pay_to_slot(self.ty(*a), &v, s)?;
                let tag = if matches!(i.op, Op::Ok(_)) { "false" } else { "true" };
                self.pack(&d, s, tag, &pay);
            }
            Op::Payload(a) => {
                // タグは見ない。枠の下位 s ビットを、型 `i.ty` の値として読む。
                let s = slot_bits(aggs, self.ty(*a));
                let av = self.r(*a);
                let w = self.t(format!("trunc i{} {} to i{}", s + 1, av, s));
                let v = self.slot_to_pay(i.ty, &w, s);
                let t = self.lt(i.ty);
                self.mov(&d, &t, &v);
            }
            Op::Checked { op, a, b } => self.checked(i, *op, *a, *b)?,
            Op::FrameBytes { size } => {
                self.line(format!("{} = alloca i8, i64 {}, align 16", d, size));
                self.decl("declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)".into());
                self.line(format!("call void @llvm.memset.p0.i64(ptr {}, i8 0, i64 {}, i1 false)", d, size));
            }
            Op::StaticBytes(bytes) => {
                let g = format!("ksn.s.{}.{}", self.fi, i.dst);
                let esc: String = bytes.iter().map(|b| format!("\\{:02X}", b)).collect();
                self.cx.globals.push_str(&format!("@{} = private unnamed_addr constant [{} x i8] c\"{}\"\n", g, bytes.len(), esc));
                self.line(format!("{} = getelementptr i8, ptr @{}, i64 0", d, g));
            }
            Op::PtrAdd { p, off } => {
                let (pv, o) = (self.r(*p), self.r(*off));
                self.line(format!("{} = getelementptr i8, ptr {}, i64 {}", d, pv, o));
            }
            Op::MemLoad { p, off } => {
                let pv = self.r(*p);
                let addr = self.t(format!("getelementptr i8, ptr {}, i64 {}", pv, off));
                let mt = self.mt(i.ty);
                if i.ty == Ty::Bool {
                    let x = self.t(format!("load {}, ptr {}, align 1", mt, addr));
                    self.line(format!("{} = icmp ne i8 {}, 0", d, x));
                } else {
                    self.line(format!("{} = load {}, ptr {}, align 1", d, mt, addr));
                }
            }
            Op::MemStore { .. } | Op::Store { .. } | Op::HeapFree(_) | Op::IndexCheck { .. } | Op::Panic { .. } | Op::Eprint { .. } | Op::Abort => {
                return Err("unit 以外の結果を持つ副作用命令".into())
            }
            Op::Write { fd, data } => {
                let Ty::Res(_, e) = i.ty else { return Err("write の結果が Result でない".into()) };
                if vbits(aggs, e.ty()) != 64 {
                    return Err("std.io.Error の幅が 8 バイトでない".into());
                }
                self.cx.write = true;
                self.cx.decl("declare i64 @write(i32, ptr, i64)");
                self.cx.decl(format!("declare ptr @{}()", errno_name()));
                let (f, av) = (self.r(*fd), self.r(*data));
                let pi = self.extract(&av, 128, 0, 64);
                let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                let n = self.extract(&av, 128, 8, 64);
                let r = self.t(format!("call i64 @ksn.write(i32 {}, ptr {}, i64 {})", f, p, n));
                let ok = self.t(format!("icmp eq i64 {}, 0", r));
                let tag = self.t(format!("xor i1 {}, true", ok));
                self.pack(&d, 64, &tag, &r);
            }
            Op::ParseInt { ty, s, radix } => {
                self.cx.parse_int = true;
                self.cx.decl("declare i32 @ksn_parse_int(ptr, i64, i32, i32, i32, ptr, ptr)".to_string());
                let av = self.r(*s);
                let pi = self.extract(&av, 128, 0, 64);
                let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                let n = self.extract(&av, 128, 8, 64);
                let rv = self.r(*radix);
                let bits = ty.bits();
                let iss = if ty.signed() { 1 } else { 0 };
                let lo_ptr = self.t("alloca i64".to_string());
                let hi_ptr = self.t("alloca i64".to_string());
                let ret = self.t(format!(
                    "call i32 @ksn_parse_int(ptr {}, i64 {}, i32 {}, i32 {}, i32 {}, ptr {}, ptr {})",
                    p, n, rv, bits, iss, lo_ptr, hi_ptr
                ));
                let lo = self.t(format!("load i64, i64* {}", lo_ptr));
                let is_ok = self.t(format!("icmp eq i32 {}, 0", ret));
                let tag = self.t(format!("xor i1 {}, true", is_ok));
                let kind = self.t(format!("sub i32 {}, 1", ret));
                let kind_z = self.t(format!("zext i32 {} to i64", kind));
                let pay = self.t(format!("select i1 {}, i64 {}, i64 {}", is_ok, lo, kind_z));
                self.pack(&d, 64, &tag, &pay);
            }
            Op::FmtFloat { bits, a } => {
                self.cx.fmt_float = true;
                let (func, arg_ty) = if *bits == 64 {
                    ("@ksn_fmt_f64", "double")
                } else {
                    ("@ksn_fmt_f32", "float")
                };
                let fp = self.t(format!("call ptr {}({} {})", func, arg_ty, self.r(*a)));
                self.cx.decl("declare i64 @strlen(ptr)".to_string());
                self.cx.decl("declare i32 @snprintf(ptr, i64, ptr, ...)".to_string());
                let len = self.t(format!("call i64 @strlen(ptr {})", fp));
                let ptr_i128 = self.t(format!("zext i64 {} to i128", fp));
                let len_i128 = self.t(format!("zext i64 {} to i128", len));
                let len_shl = self.t(format!("shl i128 {}, 64", len_i128));
                let fat = self.t(format!("or i128 {}, {}", len_shl, ptr_i128));
                self.cx.eprint = true;
                self.cx.decl("declare i64 @write(i32, ptr, i64)".to_string());
                let pi = self.extract(&fat, 128, 0, 64);
                let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                let n = self.extract(&fat, 128, 8, 64);
                self.line(format!("call void @ksn.eprint(ptr {}, i64 {})", p, n));
            }
            Op::HeapAlloc(n) => {
                let nv = self.r(*n);
                let z = self.t(format!("icmp eq i64 {}, 0", nv));
                let m = self.t(format!("select i1 {}, i64 1, i64 {}", z, nv));
                self.line(format!("{} = call ptr @malloc(i64 {})", d, m));
            }
            Op::HeapRealloc { p, size } => {
                let (pv, nv) = (self.r(*p), self.r(*size));
                let z = self.t(format!("icmp eq i64 {}, 0", nv));
                let m = self.t(format!("select i1 {}, i64 1, i64 {}", z, nv));
                self.line(format!("{} = call ptr @realloc(ptr {}, i64 {})", d, pv, m));
            }
        }
        Ok(())
    }

    fn store(&mut self, ptr: VId, val: VId) -> R<()> {
        let (pv, vv) = (self.r(ptr), self.r(val));
        let t = self.ty(val);
        // 参照の指す先の実際のアドレスは型の既定の整列 (`i192` なら 16) を満たすとは限らない (ヘッダは 8 の倍数、`malloc` の領域の途中など)。
        // 整列を仮定すると未定義動作になるので、常に `align 1` で読み書きする。
        self.mem_store_to(t, &pv, &vv, ", align 1")
    }

    fn mem_store(&mut self, t: Ty, addr: &str, v: &str) -> R<()> {
        self.mem_store_to(t, addr, v, ", align 1")
    }

    fn mem_store_to(&mut self, t: Ty, addr: &str, v: &str, align: &str) -> R<()> {
        if t == Ty::Bool {
            let z = self.t(format!("zext i1 {} to i8", v));
            self.line(format!("store i8 {}, ptr {}{}", z, addr, align));
        } else {
            let lt = self.lt(t);
            self.line(format!("store {} {}, ptr {}{}", lt, v, addr, align));
        }
        Ok(())
    }

    // ---- 関数全体

    fn emit(mut self) -> R<String> {
        let f = self.f;
        for b in &f.blocks {
            for (v, t) in &b.params {
                self.tys.insert(*v, *t);
            }
            for i in &b.insts {
                self.tys.insert(i.dst, i.ty);
                if i.ty == Ty::Unit && !matches!(i.op, Op::Call { .. }) {
                    self.lit.insert(i.dst, "0".into());
                    continue;
                }
                if let Op::Const(v) = &i.op {
                    let s = match i.ty {
                        Ty::Bool => (if *v != 0 { "true" } else { "false" }).to_string(),
                        Ty::F32 | Ty::F64 => float_lit(i.ty, *v),
                        Ty::Ptr => "null".into(),
                        Ty::Opt(_) => "0".into(),
                        t => int_lit(t, *v),
                    };
                    self.lit.insert(i.dst, s);
                }
            }
        }
        // 入口から到達できるブロックだけを出す。
        let mut reach = vec![false; f.blocks.len()];
        let mut todo = vec![0usize];
        while let Some(k) = todo.pop() {
            if std::mem::replace(&mut reach[k], true) {
                continue;
            }
            match &f.blocks[k].term {
                Term::Br((t, _)) => todo.push(*t),
                Term::BrIf(_, (a, _), (b, _)) => {
                    todo.push(*a);
                    todo.push(*b);
                }
                Term::Ret(_) | Term::Unreachable => {}
            }
        }
        let mut hoisted = String::new();
        let mut texts: Vec<String> = vec![String::new(); f.blocks.len()];
        for (k, b) in f.blocks.iter().enumerate() {
            if !reach[k] {
                continue;
            }
            self.body = String::new();
            self.cur = format!("b{}", k);
            for i in &b.insts {
                // 確保はどのブロックのものも関数の先頭 (静的な確保) に集め、初期化 (0 埋め) だけをここで行う。
                // ループの中の確保 (配列リテラルなど) を評価のたびに `alloca` すると、周回ごとにスタックが伸びて溢れる。
                // 1 つの確保命令の領域は、次に評価されるときには前の値を使い終えている (SSA の定義は各周で新しい)。
                {
                    match &i.op {
                        Op::Alloca => {
                            let Ty::Ref(_, p) = i.ty else { return Err("alloca の結果が参照でない".into()) };
                            let mt = self.mt(p.ty());
                            hoisted.push_str(&format!("  %v{} = alloca {}\n", i.dst, mt));
                            let z = if mt.starts_with('i') { "0" } else { "zeroinitializer" };
                            let zz = if mt == "ptr" { "null" } else { z };
                            self.line(format!("store {} {}, ptr %v{}", mt, zz, i.dst));
                            continue;
                        }
                        Op::FrameBytes { size } => {
                            hoisted.push_str(&format!("  %v{} = alloca i8, i64 {}, align 16\n", i.dst, size));
                            self.decl("declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)".into());
                            self.line(format!("call void @llvm.memset.p0.i64(ptr %v{}, i8 0, i64 {}, i1 false)", i.dst, size));
                            continue;
                        }
                        _ => {}
                    }
                }
                self.inst(i)?;
            }
            match &b.term {
                Term::Br((t, args)) => {
                    let a: Vec<String> = args.iter().map(|v| self.r(*v)).collect();
                    self.line(format!("br label %b{}", t));
                    let from = self.cur.clone();
                    self.edges.entry(*t).or_default().push((from, a));
                }
                Term::BrIf(c, (t1, a1), (t2, a2)) => {
                    let cv = self.r(*c);
                    let (x1, x2): (Vec<String>, Vec<String>) = (a1.iter().map(|v| self.r(*v)).collect(), a2.iter().map(|v| self.r(*v)).collect());
                    if t1 == t2 {
                        // 同じ移動先への 2 本の辺は、中継のブロックで区別する。
                        self.n += 1;
                        let (ea, eb) = (format!("ea{}", self.n), format!("eb{}", self.n));
                        self.line(format!("br i1 {}, label %{}, label %{}", cv, ea, eb));
                        for (l, xs) in [(ea, x1), (eb, x2)] {
                            self.label(&l);
                            self.line(format!("br label %b{}", t1));
                            self.edges.entry(*t1).or_default().push((l, xs));
                        }
                    } else {
                        self.line(format!("br i1 {}, label %b{}, label %b{}", cv, t1, t2));
                        let from = self.cur.clone();
                        self.edges.entry(*t1).or_default().push((from.clone(), x1));
                        self.edges.entry(*t2).or_default().push((from, x2));
                    }
                }
                Term::Ret(v) => {
                    let t = self.lt(f.ret);
                    let rv = self.r(*v);
                    self.line("store i64 %depth, ptr @ksn.depth".into());
                    self.line(format!("ret {} {}", t, rv));
                }
                Term::Unreachable => self.line("unreachable".into()),
            }
            texts[k] = std::mem::take(&mut self.body);
        }
        // 先頭 (静的な確保と深さの検査) と本体。
        let params: Vec<String> = f.params().iter().enumerate().map(|(j, (_, t))| format!("{} %arg{}", lty(self.aggs, *t), j)).collect();
        let mut s = format!("define internal {} {}({}) {{\nentry:\n", self.lt(f.ret), fname(&f.key), params.join(", "));
        s.push_str(&hoisted);
        s.push_str(&format!(
            "  %depth = load i64, ptr @ksn.depth\n  %over = icmp uge i64 %depth, {}\n  br i1 %over, label %so, label %go\nso:\n  call void @ksn.overflow()\n  unreachable\ngo:\n  %depth1 = add i64 %depth, 1\n  store i64 %depth1, ptr @ksn.depth\n  br label %b0\n",
            MAX_DEPTH
        ));
        for (k, b) in f.blocks.iter().enumerate() {
            if !reach[k] {
                continue;
            }
            s.push_str(&format!("b{}:\n", k));
            let mut inc: Vec<(String, Vec<String>)> = self.edges.get(&k).cloned().unwrap_or_default();
            if k == 0 {
                inc.insert(0, ("go".into(), (0..b.params.len()).map(|j| format!("%arg{}", j)).collect()));
            }
            for (j, (v, t)) in b.params.iter().enumerate() {
                let ins: Vec<String> = inc.iter().map(|(l, a)| format!("[ {}, %{} ]", a[j], l)).collect();
                if ins.is_empty() {
                    return Err(format!("ブロック b{} の引数に入ってくる辺がない", k));
                }
                s.push_str(&format!("  %v{} = phi {} {}\n", v, lty(self.aggs, *t), ins.join(", ")));
            }
            s.push_str(&texts[k]);
        }
        s.push_str("}\n");
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kir_exec::{overflow_msg, DIV_ZERO, REM_ZERO};

    #[test]
    fn emission_is_deterministic_and_has_the_entry() {
        let m = crate::kir::parse_module(crate::kir::MEM_TEXT).expect("parse");
        let a = emit(&m, "t.mem").expect("emit");
        let b = emit(&m, "t.mem").expect("emit");
        assert_eq!(a, b);
        assert!(a.contains("define i32 @main") && a.contains("@malloc"), "{}", a);
    }

    #[test]
    fn panic_messages_match_the_reference_executor() {
        let want = |op: Aop| overflow_msg(op);
        assert_eq!(PANICS[0].1, want(Aop::Add));
        assert_eq!(PANICS[1].1, want(Aop::Sub));
        assert_eq!(PANICS[2].1, want(Aop::Mul));
        assert_eq!(PANICS[3].1, want(Aop::Div));
        assert_eq!(PANICS[4].1, want(Aop::Rem));
        assert_eq!(PANICS[5].1, DIV_ZERO);
        assert_eq!(PANICS[6].1, REM_ZERO);
    }

    #[test]
    fn aggregates_with_non_scalar_entries_are_refused_with_a_reason() {
        let text = "kir 1\n\nfn t.r key=t.r uses=[] panics=never stack=- ret=ref<i32>\n  b0(%0: ref<i32>):\n    ret %0\nend t.r\n";
        let m = crate::kir::parse_module(text).expect("parse");
        assert!(emit(&m, "t.r").unwrap_err().contains("入口では扱えない"));
    }
}
