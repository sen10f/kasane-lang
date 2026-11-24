//! KIR から LLVM IR のテキスト (`.ll`) を作る (設計_KIR.md 段階 4 (B)、7 章 22)。
//!
//! Rust の LLVM 束縛は使わず、外部の `clang` が読むテキストだけを作る。意味は参照実行器 `kir_exec.rs` と同じにする。
//!
//! - 値の写像は `lty` / `mem_ty` の 1 か所にまとめる。整数 `iN`、`bool` は `i1` (メモリ上は `i8`)、`unit` は `i8` の 0、
//!   `f32` / `f64` は `float` / `double`、`ptr` と参照は `ptr`、
//!   集成体は大きさ (バイト) × 8 ビットの整数 `iN` (フィールドは `Module::aggs` のオフセットどおりのビット位置に置く)、
//!   `Option` / `Result` は整数 `iN` (N = 1 + 枠。タグは最上位のビット、枠は 64 ビット以上でペイロードのどれも入る幅。設計_KIR.md 11.6)。
//! - panic は stderr に `panic: <文言>` を出して終了コード 101。呼び出し深さは大域の計数で数え、
//!   参照実行器と同じ条件 (呼び出し元の深さが `MAX_DEPTH` 以上) で `stack overflow` と終了コード 102。
//! - 浮動小数の超越関数は `fmath` の `Ops` を LLVM IR の命令列に写した内部関数 (`@ksn.m.*`) を呼ぶ。libm は使わない
//!   (`floor` / `sqrt` / `frem` など丸めが一意に決まる演算は LLVM の組み込みを使う)。
//! - 出力は決定的 (同じ KIR からは同じバイト列)。
//! - 入口 `main` は引数を 16 進の語 (下位・上位) で受け取り、結果も 16 進の 2 語で標準出力の最後に出す (前に制御文字 `\x01` を付ける。プログラム自身の出力と区別するため)。表示は呼び出し側
//!   (`kir::show_value`) が行うので、`--run` と同じ表示になる。

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::fmath::{self, Ops};
use crate::kir::{
    size_of, verify_module, AggDef, AggKind, Aop, BitK, BoolOp, Cc, FBin, FUn, Func, FsK, Inst, Mode, Module, NetK, Op, Pay, ProcFn, Term, Ty, VId,
};

/// 呼び出し深さの既定の上限 (`kir_exec::MAX_DEPTH` と同じ)。`spawn` したスレッドの既定で、主スレッドは入口の `depth M` (`Module::depth`)。
const MAX_DEPTH: u32 = crate::kir_exec::MAX_DEPTH as u32;
/// 1 回の呼び出しの枠の大きさの見積もり (上限の根拠は `kir_exec::MAX_DEPTH` の注)。スタックの大きさは「深さの上限 × この値」から決める。
const FRAME_EST: u64 = 8192;
/// 主スレッドのスタックの既定の大きさ。`MAX_DEPTH` × 枠の見積もり (8 KiB) が収まる (`kir_exec::MAX_DEPTH` の注を見る)。
pub const NATIVE_STACK_BYTES: u64 = 1 << 30;
/// `spawn` したスレッドの既定のスタックの大きさ (深さの既定は主スレッドと同じ 100000 のままで、枠が大きくて先に尽きるときは
/// スタックの残りの検査が `stack overflow` (102) で止める)。`spawn_deep` は `深さ × 8 KiB + この値`。
const THREAD_STACK_BYTES: u64 = 64 << 20;
/// スタックの残りの検査の余白: スタックの底からこの量だけ手前で止める (C のランタイムや libc の呼び出しが使う分)。
const STACK_MARGIN: u64 = 1 << 20;
/// `spawn_deep` の深さの置き場 (スレッドの項目の `+248`。`depth + 1`、0 は既定。`kir_lower_thread.rs` の `DEPTH_OFF`)。
const THREAD_DEPTH_OFF: u32 = 248;

/// 主スレッドのスタックの大きさ: 既定の深さまでは `NATIVE_STACK_BYTES`、それを超える `depth M` は `M × 8 KiB + 64 MiB`。
fn main_stack_bytes(depth: u64) -> u64 {
    let want = crate::kir_exec::depth_limit(depth) as u64 * FRAME_EST + THREAD_STACK_BYTES;
    want.max(NATIVE_STACK_BYTES)
}

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
        Ty::F64 => 64,
        Ty::Ptr | Ty::Ref(..) => crate::ptrwidth::bits(),
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
        Ty::I64 | Ty::U64 => "i64".into(),
        Ty::Isize | Ty::Usize => size_ty(),
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

/// スレッドの補助の関数 (`kir_lower_thread.rs`。実行器では協調実行の本体) を、pthread の本体に差し替える (設計_KIR.md 22.7)。
/// - 項目 (`ksn.thr.new`) は `calloc` した領域で、「番号」(usize) はその番地。`+16` に `pthread_t` を置く。
/// - `start` は 16 MiB のスタックの `pthread_create` で `ksn.thr.exec` を走らせ、`join` は `pthread_join`、`release` は `free`。
/// - チャネルは大域の 1 つの mutex + 条件変数 (`lock` / `unlock` (起こしてから外す) / `wait`)。
/// スレッドの項目の大きさ (`kir_lower_thread.rs` の `ESZ` と同じ。余裕を持たせて 256)。
const THREAD_ENTRY_SIZE: usize = 256;

/// 全員待ちの検出 (設計_KIR.md 22.7)。Mutex の lock の待ちの文言 (木走査は Mutex の待ちを持たないので、ネイティブだけの文言)。
use crate::interp::DEADLOCK_LOCK;

/// 待ちの本体 (`mu` を持って呼ぶ)。待っている数 `blocked` を 1 増やし、生きている数 `live` 以上なら全員が待っているので
/// `msg` (`@ksn.rt.deadlock*`) を出して終了コード 101 で終わる。そうでなければ状態が変わる (`gen` が進む) まで条件変数で待つ。
/// ラベル `dead` / `w` / `lp` / `done` を使うので、1 つの関数に 1 回だけ入れる。最後は `done:` の後に続ける。
fn block_body(msg: &str) -> String {
    format!(
        "  %b = load i64, ptr @ksn.rt.blocked\n  %b1 = add i64 %b, 1\n  store i64 %b1, ptr @ksn.rt.blocked\n  %l = load i64, ptr @ksn.rt.live\n  %d = icmp uge i64 %b1, %l\n  br i1 %d, label %dead, label %w\ndead:\n  %p = call i32 (i32, ptr, ...) @dprintf(i32 2, ptr {msg})\n  call void @exit(i32 101)\n  unreachable\nw:\n  %g0 = load i64, ptr @ksn.rt.gen\n  br label %lp\nlp:\n  %r = call i32 @pthread_cond_wait(ptr @ksn.rt.cv, ptr @ksn.rt.mu)\n  %g1 = load i64, ptr @ksn.rt.gen\n  %same = icmp eq i64 %g1, %g0\n  br i1 %same, label %lp, label %done\ndone:\n"
    )
}

/// `ksn.atm.<op>.<cell>` の実アトミック版 (常に seq_cst。木走査も順序を無視するので同じ結果)。
fn atm_override(rest: &str) -> Option<String> {
    let (op, sfx) = rest.split_once('.')?;
    let (ity, al) = match sfx {
        "u8" => ("i8", 1),
        "u64" | "usize" => ("i64", 8),
        _ => return None,
    };
    let name = fname(&format!("ksn.atm.{}", rest));
    let def = |ret: &str, params: &str, body: &str| Some(format!("define internal {ret} {name}({params}) {{\nentry:\n{body}}}\n"));
    match op {
        "load" => def(ity, "ptr %a0", &format!("  %r = load atomic {ity}, ptr %a0 seq_cst, align {al}\n  ret {ity} %r\n")),
        "store" => def("i8", &format!("ptr %a0, {ity} %a1"), &format!("  store atomic {ity} %a1, ptr %a0 seq_cst, align {al}\n  ret i8 0\n")),
        "swap" => def(ity, &format!("ptr %a0, {ity} %a1"), &format!("  %r = atomicrmw xchg ptr %a0, {ity} %a1 seq_cst, align {al}\n  ret {ity} %r\n")),
        "add" => def(ity, &format!("ptr %a0, {ity} %a1"), &format!("  %r = atomicrmw add ptr %a0, {ity} %a1 seq_cst, align {al}\n  ret {ity} %r\n")),
        "cas" => def(ity, &format!("ptr %a0, {ity} %a1, {ity} %a2"), &format!("  %r = cmpxchg ptr %a0, {ity} %a1, {ity} %a2 seq_cst seq_cst, align {al}\n  %old = extractvalue {{ {ity}, i1 }} %r, 0\n  ret {ity} %old\n")),
        _ => None,
    }
}

fn thread_override(key: &str, m: &Module) -> Option<String> {
    if let Some(rest) = key.strip_prefix("ksn.atm.") {
        return atm_override(rest);
    }
    let f = m.find(key)?;
    let name = fname(key);
    let def = |ret: &str, params: &str, body: &str| Some(format!("define internal {ret} {name}({params}) {{\nentry:\n{body}}}\n"));
    match key {
        "ksn.time.mono" => def("i64", "", "  %r = call i64 @ksn_rt_sys_now_ns()\n  ret i64 %r\n"),
        "ksn.time.sleep" => def("i8", "i64 %arg0", "  call void @ksn_rt_sys_sleep_ms(i64 %arg0)\n  ret i8 0\n"),
        "ksn.thr.lock" => def("i8", "", "  %r = call i32 @pthread_mutex_lock(ptr @ksn.rt.mu)\n  ret i8 0\n"),
        "ksn.thr.unlock" => def("i8", "", "  %g = load i64, ptr @ksn.rt.gen\n  %g1 = add i64 %g, 1\n  store i64 %g1, ptr @ksn.rt.gen\n  store i64 0, ptr @ksn.rt.blocked\n  %a = call i32 @pthread_cond_broadcast(ptr @ksn.rt.cv)\n  %r = call i32 @pthread_mutex_unlock(ptr @ksn.rt.mu)\n  ret i8 0\n"),
        "ksn.thr.minit" => def("i8", "ptr %arg0", "  store atomic i32 0, ptr %arg0 release, align 4\n  ret i8 0\n"),
        // Mutex の lock: 語 (0 空き / 1 持たれている / 2 待ちがいる) の CAS が速い道。取れなければ短く回り、それでも取れなければ
        // `mu` の下で語を 2 にして (取れたらそのまま持つ) 条件変数で待つ。解放 (`munlock`) は語を 0 にし、待ちがいたとき (旧い値が 2) だけ
        // `mu` の下で `gen` を進めて起こす (起こし損ねない: 待ちは `mu` を持ったまま語を 2 にして待ちに入る)。
        "ksn.thr.mlock" => def(
            "i8",
            "ptr %arg0",
            "  %cf = alloca i32, align 4\n  store i32 0, ptr %cf\n  %c0 = cmpxchg ptr %arg0, i32 0, i32 1 acquire monotonic, align 4\n  %ok0 = extractvalue { i32, i1 } %c0, 1\n  br i1 %ok0, label %got, label %chk0\nchk0:\n  %op = getelementptr i8, ptr %arg0, i64 8\n  %ow = load atomic i64, ptr %op monotonic, align 8\n  %me0 = call i64 @pthread_self()\n  %mine = icmp eq i64 %ow, %me0\n  br i1 %mine, label %dead, label %spin\ngot:\n  br label %fin\nspin:\n  %i = phi i32 [ 0, %chk0 ], [ %i1, %spin2 ]\n  %v = load atomic i32, ptr %arg0 monotonic, align 4\n  %free = icmp eq i32 %v, 0\n  br i1 %free, label %try, label %spin2\ntry:\n  %c1 = cmpxchg ptr %arg0, i32 0, i32 1 acquire monotonic, align 4\n  %ok1 = extractvalue { i32, i1 } %c1, 1\n  br i1 %ok1, label %got, label %spin2\nspin2:\n  %i1 = add i32 %i, 1\n  %more = icmp ult i32 %i1, 100\n  br i1 %more, label %spin, label %slow\nslow:\n  %o = atomicrmw xchg ptr %arg0, i32 2 acquire, align 4\n  %z = icmp eq i32 %o, 0\n  br i1 %z, label %took, label %wl\nwl:\n  %to = call i32 @ksn_rt_sys_futex_wait(ptr %arg0, i32 2, i64 20)\n  %o2 = atomicrmw xchg ptr %arg0, i32 2 acquire, align 4\n  %z2 = icmp eq i32 %o2, 0\n  br i1 %z2, label %took, label %chk\nchk:\n  %tz = icmp ne i32 %to, 0\n  br i1 %tz, label %tmo, label %wl\ntmo:\n  %c = load i32, ptr %cf\n  %nc = icmp eq i32 %c, 0\n  br i1 %nc, label %inc, label %dchk\ninc:\n  %ma = atomicrmw add ptr @ksn.rt.mwait, i64 1 seq_cst, align 8\n  store i32 1, ptr %cf\n  br label %dchk\ndchk:\n  %lk = call i32 @pthread_mutex_lock(ptr @ksn.rt.mu)\n  %b = load i64, ptr @ksn.rt.blocked\n  %l = load i64, ptr @ksn.rt.live\n  %m = load atomic i64, ptr @ksn.rt.mwait seq_cst, align 8\n  %sm = add i64 %b, %m\n  %d = icmp uge i64 %sm, %l\n  %um = call i32 @pthread_mutex_unlock(ptr @ksn.rt.mu)\n  br i1 %d, label %rech, label %wl\nrech:\n  %o3 = atomicrmw xchg ptr %arg0, i32 2 acquire, align 4\n  %z3 = icmp eq i32 %o3, 0\n  br i1 %z3, label %took, label %dead\ndead:\n  %p = call i32 (i32, ptr, ...) @dprintf(i32 2, ptr @ksn.rt.deadlock_lock)\n  call void @exit(i32 101)\n  unreachable\ntook:\n  %cc = load i32, ptr %cf\n  %hc = icmp ne i32 %cc, 0\n  br i1 %hc, label %dec, label %fin\ndec:\n  %md = atomicrmw sub ptr @ksn.rt.mwait, i64 1 seq_cst, align 8\n  br label %fin\nfin:\n  %op2 = getelementptr i8, ptr %arg0, i64 8\n  %me = call i64 @pthread_self()\n  store atomic i64 %me, ptr %op2 monotonic, align 8\n  ret i8 0\n",
        ),
        "ksn.thr.munlock" => def(
            "i8",
            "ptr %arg0",
            "  %op = getelementptr i8, ptr %arg0, i64 8\n  store atomic i64 0, ptr %op monotonic, align 8\n  %o = atomicrmw xchg ptr %arg0, i32 0 release, align 4\n  %w = icmp eq i32 %o, 2\n  br i1 %w, label %wake, label %done\nwake:\n  call void @ksn_rt_sys_futex_wake(ptr %arg0)\n  br label %done\ndone:\n  ret i8 0\n",
        ),
        "ksn.thr.wait" => def("i8", "", &format!("{}  ret i8 0\n", block_body("@ksn.rt.deadlock"))),
        "ksn.thr.new" => def("i64", "", &format!("  %p = call ptr @calloc(i64 1, i64 {})\n  %z = icmp eq ptr %p, null\n  br i1 %z, label %oom, label %ok\noom:\n  call void @exit(i32 101)\n  unreachable\nok:\n  %i = ptrtoint ptr %p to i64\n  ret i64 %i\n", THREAD_ENTRY_SIZE)),
        "ksn.thr.ent" => def("ptr", "i64 %arg0", "  %p = inttoptr i64 %arg0 to ptr\n  ret ptr %p\n"),
        "ksn.thr.release" => def("i8", "i64 %arg0", "  %p = inttoptr i64 %arg0 to ptr\n  call void @free(ptr %p)\n  ret i8 0\n"),
        // join: 項目の完了の旗 (`+0` = 2、`tramp` が `mu` の下で立てる) を待ってから `pthread_join` する。待つ間は待ちに数える。
        "ksn.thr.join" => def(
            "i8",
            "i64 %arg0",
            &format!(
                "  %ip = inttoptr i64 %arg0 to ptr\n  %lk = call i32 @pthread_mutex_lock(ptr @ksn.rt.mu)\n  br label %chk\nchk:\n  %s = load i64, ptr %ip\n  %fin = icmp eq i64 %s, 2\n  br i1 %fin, label %joined, label %blk\nblk:\n{}  br label %chk\njoined:\n  %um = call i32 @pthread_mutex_unlock(ptr @ksn.rt.mu)\n  %tg = getelementptr i8, ptr %ip, i64 16\n  %tid = load i64, ptr %tg\n  %jr = call i32 @pthread_join(i64 %tid, ptr null)\n  ret i8 0\n",
                block_body("@ksn.rt.deadlock_join")
            ),
        ),
        "ksn.thr.start" => {
            let exec = m.find("ksn.thr.exec")?;
            let body = format!(
                "  %p = inttoptr i64 %arg0 to ptr\n  %t = getelementptr i8, ptr %p, i64 16\n  %attr = alloca [64 x i8], align 16\n  %a0 = call i32 @pthread_attr_init(ptr %attr)\n  %sb = call i64 @ksn.rt.tbytes(ptr %p)\n  %a1 = call i32 @pthread_attr_setstacksize(ptr %attr, i64 %sb)\n  %m = call i32 @pthread_mutex_lock(ptr @ksn.rt.mu)\n  %l = load i64, ptr @ksn.rt.live\n  %l1 = add i64 %l, 1\n  store i64 %l1, ptr @ksn.rt.live\n  %mu = call i32 @pthread_mutex_unlock(ptr @ksn.rt.mu)\n  %r = call i32 @pthread_create(ptr %t, ptr %attr, ptr @ksn.rt.tramp, ptr %p)\n  %a2 = call i32 @pthread_attr_destroy(ptr %attr)\n  %bad = icmp ne i32 %r, 0\n  br i1 %bad, label %fail, label %ok\nfail:\n  call void @exit(i32 101)\n  unreachable\nok:\n  ret i8 0\n"
            );
            let tramp = format!(
                "define internal i64 @ksn.rt.tdepth(ptr %p) {{\nentry:\n  %dp = getelementptr i8, ptr %p, i64 {off}\n  %sv = load i64, ptr %dp\n  %z = icmp eq i64 %sv, 0\n  %d0 = sub i64 %sv, 1\n  %lo = icmp ult i64 %d0, 1\n  %d1 = select i1 %lo, i64 1, i64 %d0\n  %hi = icmp ugt i64 %d1, {lim}\n  %d2 = select i1 %hi, i64 {lim}, i64 %d1\n  %d = select i1 %z, i64 {dflt}, i64 %d2\n  ret i64 %d\n}}\n\ndefine internal i64 @ksn.rt.tbytes(ptr %p) {{\nentry:\n  %dp = getelementptr i8, ptr %p, i64 {off}\n  %sv = load i64, ptr %dp\n  %z = icmp eq i64 %sv, 0\n  %d = call i64 @ksn.rt.tdepth(ptr %p)\n  %w = mul i64 %d, {frame}\n  %b = add i64 %w, {base}\n  %r = select i1 %z, i64 {base}, i64 %b\n  ret i64 %r\n}}\n\ndefine internal ptr @ksn.rt.tramp(ptr %p) {{\nentry:\n  %td = call i64 @ksn.rt.tdepth(ptr %p)\n  store i64 %td, ptr @ksn.dmax\n  %tb = call i64 @ksn.rt.tbytes(ptr %p)\n  %spp = call ptr @llvm.stacksave.p0()\n  %sp = ptrtoint ptr %spp to i64\n  %t0 = sub i64 %sp, %tb\n  %fl = add i64 %t0, {margin}\n  store i64 %fl, ptr @ksn.floor\n  %i = ptrtoint ptr %p to i64\n  %r = call i8 {}(i64 %i)\n  %m = call i32 @pthread_mutex_lock(ptr @ksn.rt.mu)\n  %l = load i64, ptr @ksn.rt.live\n  %l1 = sub i64 %l, 1\n  store i64 %l1, ptr @ksn.rt.live\n  store i64 2, ptr %p\n  %g = load i64, ptr @ksn.rt.gen\n  %g1 = add i64 %g, 1\n  store i64 %g1, ptr @ksn.rt.gen\n  store i64 0, ptr @ksn.rt.blocked\n  %bc = call i32 @pthread_cond_broadcast(ptr @ksn.rt.cv)\n  %um = call i32 @pthread_mutex_unlock(ptr @ksn.rt.mu)\n  ret ptr null\n}}\n\n",
                fname(&exec.key),
                off = THREAD_DEPTH_OFF,
                lim = crate::kir_exec::MAX_DEPTH_LIMIT,
                dflt = MAX_DEPTH,
                frame = FRAME_EST,
                base = THREAD_STACK_BYTES,
                margin = STACK_MARGIN
            );
            def("i8", "i64 %arg0", &body).map(|d| tramp + &d)
        }
        _ => {
            let _ = f;
            None
        }
    }
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
    /// `std.net` 命令を使う (`@ksn.net_*` を出す)。
    net: bool,
    /// `std.env` 命令を使う (`@ksn_rt_sys_*` を呼ぶ)。
    env: bool,
    /// `std.time` の実時刻命令を使う (`@ksn_rt_sys_wall_ns` を呼ぶ)。
    time: bool,
    /// `std.fs` 命令を使う (`@ksn_rt_sys_*` を呼ぶ)。
    fs: bool,
    /// `FmtFloat` 命令を使う (浮動小数の書式化補助関数を出す)。
    fmt_float: bool,
    /// `ParseInt` 命令を使う (`kir_rt.c` の `ksn_parse_int` を呼ぶ)。
    parse_int: bool,
    /// C ABI の構造体の渡し方 (`kir_cabi`)。ターゲットの CPU で決まる。
    arch: crate::kir_cabi::Arch,
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

/// wasm32 で作れない形 (スレッド・ファイル・ソケット・プロセス・環境・実時刻・浮動小数の書式化) を使っていれば、その理由。
/// 出力は素の wasm で、libc もホストの資源もないので、対応しないものは理由つきで失敗させる (`WASM ABI` の章)。
/// `wasi` のとき (`wasm32-wasi`) は、ファイル・環境・実時刻を WASI の import で実現する (`wasi.c`) ので通す。
/// スレッド・ソケット・プロセス・浮動小数の書式化は WASI でも対応しない。
fn wasm_unsupported(ir: &str, wasi: bool) -> Option<String> {
    let has = |p: &str| ir.contains(p);
    let why = if has("pthread_") || has("@ksn.thr.") {
        "スレッド (spawn / join / チャネル / Mutex) は wasm32 では未対応 (単一スレッドの素の wasm)"
    } else if has("@ksn_rt_sys_net_") || has("@ksn.net_") || has("@accept(") || has("@getsockname(") || has("@socket(") {
        "std.net (ソケット) は wasm32 では未対応"
    } else if has("@ksn_rt_sys_proc_") {
        "std.process (外部コマンド) は wasm32 では未対応"
    } else if !wasi && (has("@ksn_rt_sys_open(") || has("@ksn_rt_sys_exists(") || has("@ksn_rt_sys_remove(") || has("@ksn_rt_sys_rename(") || has("@ksn_rt_sys_stat(") || has("@ksn_rt_sys_readdir(") || has("@ksn_rt_sys_opendir(") || has("@ksn_rt_sys_openbeneath(") || has("@ksn_rt_sys_fstat(")) {
        "std.fs (ファイル) は wasm32 では未対応"
    } else if !wasi && (has("@ksn_rt_sys_arg") || has("@ksn_rt_sys_var(")) {
        "std.env・引数 (ホストの環境) は wasm32 では未対応"
    } else if !wasi && has("@ksn_rt_sys_wall_ns") {
        "実時刻 (std.time.SystemTime) は wasm32 では未対応"
    } else if has("@ksn_rt_sys_now_ns") || has("@ksn_rt_sys_sleep_ms") {
        "単調時計 (std.time.Instant) と sleep は wasm32 では未対応"
    } else if has("@ksn_fmt_") {
        "浮動小数の書式化 (表示・文字列化) は wasm32 では未対応 (最短の往復表示に libc の snprintf / strtod が要る)"
    } else {
        return None;
    };
    let ok = if wasi {
        "WASM ABI の章 (仕様) の WASI の項を参照: wasm32-wasi で使えるのは入出力・ファイル (前開きのディレクトリの下)・引数と環境・実時刻・乱数・確保・整数と論理の計算・文字列と配列"
    } else {
        "WASM ABI の章 (仕様) を参照: wasm32 で使えるのは入出力 (標準出力・標準エラー)・確保・整数と論理の計算・文字列と配列"
    };
    Some(format!("{}。{}", why, ok))
}

/// クロスコンパイルの対象 (三つ組)。LLVM IR の `target triple` / `target datalayout` と、clang に渡す探索先を決める。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    /// `ksn build --target` の値 (三つ組)。
    pub name: &'static str,
    /// LLVM IR の `target triple`。
    pub triple: &'static str,
    /// LLVM IR の `target datalayout` (LLVM 20 の既定の値に合わせる)。
    pub datalayout: &'static str,
    /// 既定の sysroot (C の開発ファイルと libc / libm)。空ならホストの既定 (clang に任せる)。`KSN_SYSROOT` で上書きする。
    pub sysroot: &'static str,
    /// ポインタの幅 (ビット)。`usize` / `isize` / 生ポインタ / 参照の幅 (`ptrwidth`)。wasm32 は 32、ほかは 64。
    pub ptr_bits: u32,
}

/// 対応するターゲット。ほかの三つ組は `target_of` が `None` を返す (呼び出し側が理由を出す)。
/// `wasm32-unknown-unknown` は、ブラウザが主目標なので WASI ではなく素の wasm (import は `env` の数個だけ、`WASM ABI` 参照)。
/// `wasm32-wasi` は WASI preview 1 の import (`wasi_snapshot_preview1`) で、ファイル・引数と環境・実時刻・乱数を使える (`WASM ABI` の WASI の項)。
pub const TARGETS: [Target; 7] = [
    Target {
        name: "x86_64-linux-gnu",
        triple: "x86_64-unknown-linux-gnu",
        datalayout: "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128",
        sysroot: "",
        ptr_bits: 64,
    },
    Target {
        name: "aarch64-linux-gnu",
        triple: "aarch64-unknown-linux-gnu",
        datalayout: "e-m:e-i8:8:32-i16:16:32-i64:64-i128:128-n32:64-S128",
        sysroot: "/usr/aarch64-linux-gnu",
        ptr_bits: 64,
    },
    Target {
        name: "x86_64-windows-gnu",
        triple: "x86_64-w64-windows-gnu",
        datalayout: "e-m:w-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128",
        sysroot: "/usr/x86_64-w64-mingw32",
        ptr_bits: 64,
    },
    Target {
        name: "wasm32-unknown-unknown",
        triple: "wasm32-unknown-unknown",
        datalayout: "e-m:e-p:32:32-p10:8:8-p20:8:8-i64:64-i128:128-n32:64-S128-ni:1:10:20",
        sysroot: "",
        ptr_bits: 32,
    },
    Target {
        name: "wasm32-wasi",
        triple: "wasm32-unknown-wasi",
        datalayout: "e-m:e-p:32:32-p10:8:8-p20:8:8-i64:64-i128:128-n32:64-S128-ni:1:10:20",
        sysroot: "",
        ptr_bits: 32,
    },
    Target {
        name: "aarch64-apple-darwin",
        triple: "aarch64-apple-darwin",
        datalayout: "e-m:e-p:64:64-i64:64-i128:128-n32:64-S128",
        sysroot: "",
        ptr_bits: 64,
    },
    Target {
        name: "x86_64-apple-darwin",
        triple: "x86_64-apple-darwin",
        datalayout: "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128",
        sysroot: "",
        ptr_bits: 64,
    },
];

/// `wasm32` の対象か (素の wasm と WASI。ファイルは `.wasm`、実行は `tools/wasm/run.mjs` / `run_wasi.mjs` か JS の `WebAssembly` で行う)。
pub fn is_wasm(t: Option<Target>) -> bool {
    t.is_some_and(|t| t.ptr_bits == 32 && t.triple.starts_with("wasm32"))
}

/// Windows (MinGW-w64) の対象か。実行ファイルは `.exe`、リンクは組み込みの lld の MinGW 版。
pub fn is_windows(t: Option<Target>) -> bool {
    t.is_some_and(|t| t.triple.contains("windows"))
}

/// macOS の対象か。実行ファイルは Mach-O、`ld64.lld` でつなぐ。
pub fn is_macos(t: Option<Target>) -> bool {
    t.is_some_and(|t| t.triple.contains("darwin"))
}

/// Windows 用に、IR が直接呼ぶ POSIX の libc を `ksn_w_*` の薄い殻 (`kir_rt.c`) へ向ける。
/// Win64 の `write` / `read` は `int` と `unsigned` で、ソケットは別の記述子、`dprintf` と `__errno_location` は無い。
fn windows_shims(ll: &str) -> String {
    // winpthreads: 再帰 mutex の種類は 2 (Linux は 1)、静的な初期化子は 0 ではなく -1 (`PTHREAD_MUTEX_INITIALIZER`)。
    let mut s = ll.replace("@pthread_mutexattr_settype(ptr %attr, i32 1)", "@pthread_mutexattr_settype(ptr %attr, i32 2)");
    for g in ["mu", "cv"] {
        s = s.replace(&format!("@ksn.rt.{g} = internal global [64 x i8] zeroinitializer, align 16"), &format!("@ksn.rt.{g} = internal global i64 -1, align 16"));
    }
    for n in ["write", "read", "close", "accept", "getsockname", "socket", "dprintf", "__errno_location"] {
        s = s.replace(&format!("@{}(", n), &format!("@ksn_w_{}(", n));
    }
    s
}

/// `wasm32-wasi` の対象か (WASI preview 1 の import を使う。`wasm32-unknown-unknown` は素の wasm のまま)。
pub fn is_wasi(t: Option<Target>) -> bool {
    t.is_some_and(|t| t.name == "wasm32-wasi")
}

/// wasm32 の import の名前: `link "env:名前"` の `名前` (WASM ABI。JS の `env` の関数を呼ぶ)。`env:` 以外の前置きは `None`。
pub fn wasm_import_of(link: &str) -> Option<&str> {
    link.strip_prefix("env:").filter(|n| !n.is_empty())
}

/// wasm32 の export 名: FQN の最後の成分が `ksn_` で始まる関数 (`ksn_rt_` と `ksn_main` は予約) は、その名前で JS から呼べる。
/// 対象は wasm32 だけ (ほかの三つ組では `None` で、出力を変えない)。
pub fn wasm_export_name(key: &str) -> Option<&str> {
    if crate::ptrwidth::bits() != 32 {
        return None;
    }
    let last = key.rsplit('.').next()?;
    (last.starts_with("ksn_") && !last.starts_with("ksn_rt_") && last != "ksn_main").then_some(last)
}

/// `usize` / `isize` の LLVM の型 (今のポインタの幅)。
fn size_ty() -> String {
    format!("i{}", crate::ptrwidth::bits())
}

/// 生ポインタを整数で表すときの型 (今のポインタの幅。`ptrtoint` / `inttoptr` の相手)。
fn pty() -> String {
    format!("i{}", crate::ptrwidth::bits())
}

/// 三つ組の名前から対象を引く。
pub fn target_of(name: &str) -> Option<Target> {
    TARGETS.iter().copied().find(|t| t.name == name)
}

/// 今の機械の対象 (LLVM IR の triple を書けるもの)。対応していないホストでは `None` (IR に triple を書かない従来の形)。
pub fn host_target() -> Option<Target> {
    let name = if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        "x86_64-linux-gnu"
    } else if cfg!(all(target_arch = "aarch64", target_os = "linux")) {
        "aarch64-linux-gnu"
    } else if cfg!(all(target_arch = "x86_64", target_os = "windows")) {
        // Windows 製の ksn.exe は既定で Windows 向け実行ファイルを作る。
        "x86_64-windows-gnu"
    } else {
        return None;
    };
    target_of(name)
}

/// `entry` を入口とする実行可能な LLVM IR を作る。KIR の検査に通らない・入口が扱えない形は理由つきの `Err`。
/// `target` が `None` なら triple と datalayout を書かない (`ksn kir --llvm` の従来の出力)。
pub fn emit(m: &Module, entry: &str) -> R<String> {
    emit_with(m, entry, false, None)
}

/// `main` から始まるプログラム全体の実行ファイルの LLVM IR。入口は引数なしの関数 (`()` か `Result<T, E>` を返す) を呼び、
/// `Err(e)` なら stderr に `Error: <e の Debug 表示>` と改行を出して終了コード 1、それ以外は 0 で終わる (設計_KIR.md 12.3)。
/// `E` の表示は、KIR の関数 `ksn.entry.debug` (降格が作る) を呼ぶ。
/// `target` の triple と datalayout を書く。対象が x86_64 以外なら、構造体の値渡し・値返しの extern は未対応として `Err` を返す。
pub fn emit_program(m: &Module, entry: &str, target: Option<Target>) -> R<String> {
    emit_with(m, entry, true, target)
}

/// `extern` 関数の宣言のパラメータと戻り値の型 (構造体は C の呼び出し規約の形、`kir_cabi`)。
fn c_decl_sig(arch: crate::kir_cabi::Arch, aggs: &[AggDef], f: &Func) -> R<(Vec<String>, String)> {
    let ptys: Vec<Ty> = f.params().iter().map(|(_, t)| *t).collect();
    if !ptys.iter().any(|t| matches!(t, Ty::Agg(_))) && !matches!(f.ret, Ty::Agg(_)) {
        let rt = if f.ret == Ty::Unit { "void".to_string() } else { lty(aggs, f.ret) };
        return Ok((ptys.iter().map(|t| lty(aggs, *t)).collect(), rt));
    }
    let sg = crate::kir_cabi::sig(arch, aggs, &ptys, f.ret).map_err(|e| format!("extern {}: {}", f.key, e))?;
    let mut ps = vec![];
    let mut rt = if f.ret == Ty::Unit { "void".to_string() } else { lty(aggs, f.ret) };
    if let Some(ra) = &sg.ret {
        rt = crate::kir_cabi::ret_ty(ra);
        if let crate::kir_cabi::Abi::Mem { size, align } = ra {
            ps.push(format!("ptr sret([{} x i8]) align {}", size, align));
        }
    }
    for (t, a) in ptys.iter().zip(sg.args.iter()) {
        match a {
            Some(abi) => ps.extend(crate::kir_cabi::param_tys(abi)),
            None => ps.push(lty(aggs, *t)),
        }
    }
    Ok((ps, rt))
}

fn emit_with(m: &Module, entry: &str, program: bool, target: Option<Target>) -> R<String> {
    let errs = verify_module(m);
    if let Some((k, e)) = errs.first() {
        return Err(format!("KIR の検査に通らない ({}): {}", k, e));
    }
    // Win64: 大きさが 1・2・4・8 バイトの構造体は整数 1 個として渡る。それより大きいものは呼び出し側の
    // 複製へのポインタ (引数)・sret (戻り値)。`kir_cabi` が分類するので、ここで拒むことはしない
    // (2026-11-20 の N1w3: wine で `tests/windows_ffi_struct.rs` が通ることを確かめた)。
    let Some(ef) = m.find(entry) else {
        return Err(format!("関数 {} がない", entry));
    };
    if let Some(tg) = target.filter(|tg| tg.ptr_bits == 32) {
        // C の関数を呼ぶ `extern` (FFI) は、wasm32 では呼び先がない。`link "env:名前"` だけは JS から来る import (WASM ABI) として許す。
        if let Some(f) = m.funcs.iter().find(|f| f.link.as_deref().is_some_and(|l| wasm_import_of(l).is_none())) {
            return Err(format!("extern {}: C の関数を呼ぶ FFI は {} では未対応 (libc もリンク先もない。JS の関数は link \"env:名前\" で呼ぶ)", f.key, tg.name));
        }
        for f in m.funcs.iter().filter(|f| f.link.is_some()) {
            if f.params().iter().any(|(_, pt)| matches!(pt, Ty::Agg(_))) || matches!(f.ret, Ty::Agg(_)) {
                return Err(format!("extern {}: wasm32 の import は構造体を値で渡せない (スカラーとポインタだけ)", f.key));
            }
        }
        // 公開する関数 (`ksn_` で始まる名前) は、同じ名前の export になるので重ねられない。構造体の値渡しも不可。
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for f in m.funcs.iter() {
            let Some(name) = wasm_export_name(&f.key) else { continue };
            if !seen.insert(name) {
                return Err(format!("export {}: 同じ名前 {} の関数が二つある (export 名は一意)", f.key, name));
            }
            if f.params().iter().any(|(_, pt)| matches!(pt, Ty::Agg(_))) || matches!(f.ret, Ty::Agg(_)) {
                return Err(format!("export {}: wasm32 の export は構造体を値で渡せない (スカラーとポインタだけ)", f.key));
            }
        }
    }
    let has_fmt_float = m.funcs.iter().any(|f| f.blocks.iter().any(|b| b.insts.iter().any(|i| matches!(i.op, Op::FmtFloat { .. } | Op::FloatText { .. }))));
    let has_parse_int = m.funcs.iter().any(|f| f.blocks.iter().any(|b| b.insts.iter().any(|i| matches!(i.op, Op::ParseInt { .. }))));
    let arch = if is_windows(target) {
        crate::kir_cabi::Arch::Win64
    } else if target.map_or(false, |t| is_macos(Some(t)) && t.triple.starts_with("aarch64")) {
        // Apple arm64: 構造体の値渡しは AAPCS64 と同じ。可変引数はスタックに置く。
        crate::kir_cabi::Arch::Aarch64Apple
    } else if target.or_else(host_target).is_some_and(|t| t.name == "aarch64-linux-gnu") {
        crate::kir_cabi::Arch::Aarch64
    } else {
        crate::kir_cabi::Arch::X86_64
    };
    let mut cx = Ctx { decls: BTreeSet::new(), globals: String::new(), math: BTreeSet::new(), write: false, eprint: false, fmt_float: has_fmt_float, net: false, env: false, time: false, fs: false, parse_int: has_parse_int, arch };
    // `extern` 宣言は `define` ではなく `declare` として出す。本体の emission はスキップする。
    let extern_links: HashMap<String, String> = m.funcs.iter()
        .filter_map(|f| f.link.as_ref().map(|l| {
            let sym = l.rsplit_once(':').map(|(_, s)| s).unwrap_or(l);
            (f.key.clone(), sym.to_string())
        }))
        .collect();
    // `link "ライブラリ:名前"` (動的ロード、3.8): 実行時に `dlopen` / `dlsym` で引く。呼び出しは `ksn_ext_resolve` が覚えた
    // 関数ポインタを間接に呼ぶ。静的な `link "名前"` は従来どおり直接リンクする。
    // wasm32 の `link "env:名前"` は動的ロードではなく JS の関数の import (WASM ABI) なので、ここには入れない。
    let wasm32 = crate::ptrwidth::bits() == 32;
    let extern_dl: HashMap<String, (String, String)> = m.funcs.iter()
        .filter(|f| !(wasm32 && f.link.as_deref().and_then(wasm_import_of).is_some()))
        .filter_map(|f| f.link.as_ref().and_then(|l| l.rsplit_once(':').map(|(lib, s)| (f.key.clone(), (lib.to_string(), s.to_string())))))
        .collect();
    let mut dl_seen: BTreeSet<String> = BTreeSet::new();
    for (key, sym) in &extern_links {
        let Some(f) = m.funcs.iter().find(|f| f.key == *key) else { continue };
        if let Some((lib, name)) = extern_dl.get(key) {
            if dl_seen.insert(sym.clone()) {
                cx.globals.push_str(&format!("@ksn.ext.slot.{} = internal global ptr null\n", sym));
                cx.globals.push_str(&str_const(&format!("ksn.ext.lib.{}", sym), &format!("{}\0", lib)));
                cx.globals.push_str(&str_const(&format!("ksn.ext.sym.{}", sym), &format!("{}\0", name)));
            }
            cx.decl("declare ptr @ksn_ext_resolve(ptr, ptr, ptr)".to_string());
            continue;
        }
        let (params, rt) = c_decl_sig(cx.arch, &m.aggs, f)?;
        // wasm32 の `link "env:名前"` は JS の関数の import (`env.名前`)。属性で import の綴りを固定する (WASM ABI)。
        let imp = match f.link.as_deref().and_then(wasm_import_of) {
            Some(n) if crate::ptrwidth::bits() == 32 => format!(" \"wasm-import-module\"=\"env\" \"wasm-import-name\"=\"{}\"", n),
            _ => String::new(),
        };
        cx.decl(format!("declare {} @{}({}){}", rt, sym, params.join(", "), imp));
    }
    let mut funcs = String::new();
    let mut pthread_used = false;
    for (fi, f) in m.funcs.iter().enumerate() {
        if f.link.is_some() {
            continue;
        }
        funcs.push('\n');
        if let Some(text) = thread_override(&f.key, m) {
            if f.key.starts_with("ksn.time.") {
                cx.decl("declare i64 @ksn_rt_sys_now_ns()");
                cx.decl("declare void @ksn_rt_sys_sleep_ms(i64)");
            } else if !pthread_used {
                pthread_used = true;
                cx.globals.push_str("@ksn.rt.mu = internal global [64 x i8] zeroinitializer, align 16\n@ksn.rt.cv = internal global [64 x i8] zeroinitializer, align 16\n@ksn.rt.live = internal global i64 1\n@ksn.rt.blocked = internal global i64 0\n@ksn.rt.gen = internal global i64 0\n@ksn.rt.mwait = internal global i64 0, align 8\n");
                cx.globals.push_str(&str_const("ksn.rt.deadlock", &format!("panic: {}\n\0", crate::interp::DEADLOCK_RECV)));
                cx.globals.push_str(&str_const("ksn.rt.deadlock_join", &format!("panic: {}\n\0", crate::interp::DEADLOCK_JOIN)));
                cx.globals.push_str(&str_const("ksn.rt.deadlock_lock", &format!("panic: {}\n\0", DEADLOCK_LOCK)));
                cx.decl("declare i32 @ksn_rt_sys_futex_wait(ptr, i32, i64)");
                cx.decl("declare void @ksn_rt_sys_futex_wake(ptr)");
                cx.decl("declare i64 @pthread_self()");
                for d in [
                    "declare i32 @pthread_mutex_trylock(ptr)",
                    "declare i32 @pthread_create(ptr, ptr, ptr, ptr)",
                    "declare i32 @pthread_join(i64, ptr)",
                    "declare i32 @pthread_attr_init(ptr)",
                    "declare i32 @pthread_attr_setstacksize(ptr, i64)",
                    "declare i32 @pthread_attr_destroy(ptr)",
                    "declare i32 @pthread_mutex_lock(ptr)",
                    "declare i32 @pthread_mutex_unlock(ptr)",
                    "declare i32 @pthread_cond_wait(ptr, ptr)",
                    "declare i32 @pthread_cond_broadcast(ptr)",
                    "declare ptr @calloc(i64, i64)",
                    "declare i32 @pthread_mutexattr_init(ptr)",
                    "declare i32 @pthread_mutexattr_settype(ptr, i32)",
                    "declare i32 @pthread_mutexattr_destroy(ptr)",
                    "declare i32 @pthread_mutex_init(ptr, ptr)",
                ] {
                    cx.decl(d);
                }
            }
            funcs.push_str(&text);
            continue;
        }
        funcs.push_str(&Fe::new(&mut cx, &m.aggs, f, &extern_links, &extern_dl, &m.funcs, fi).emit().map_err(|e| format!("{}: {}", f.key, e))?);
    }
    let main = if program { emit_program_main(&mut cx, m, ef)? } else { emit_main(&mut cx, &m.aggs, ef)? };
    let main = if is_wasm(target) { main } else { on_big_stack(&mut cx, main, m.depth) };
    let mut math = String::new();
    for mf in cx.math.clone() {
        math.push('\n');
        math.push_str(&emit_math(&mut cx, mf));
    }
    // libc の `size_t` / `ssize_t` は、ターゲットのポインタの幅 (wasm32 は i32)。
    let sz = size_ty();
    for d in [
        "declare i32 @dprintf(i32, ptr, ...)".to_string(),
        "declare i32 @printf(ptr, ...)".to_string(),
        "declare void @exit(i32) noreturn".to_string(),
        format!("declare ptr @malloc({sz})"),
        format!("declare ptr @realloc(ptr, {sz})"),
        "declare void @free(ptr)".to_string(),
        "declare i64 @strtoull(ptr, ptr, i32)".to_string(),
    ] {
        cx.decls.insert(d);
    }
    if cx.fmt_float {
        cx.decls.insert(format!("declare i32 @snprintf(ptr, {sz}, ptr, ...)"));
        cx.decls.insert(format!("declare {sz} @strlen(ptr)"));
        cx.decls.insert("declare double @strtod(ptr, ptr)".to_string());
        cx.decls.insert("declare float @strtof(ptr, ptr)".to_string());
    }
    if cx.parse_int {
        // 長さはポインタの幅 (wasm32 は i32)。呼び出し側の宣言 (`ParseInt`) と同じ型にする。x86_64 / aarch64 は従来どおり i64。
        cx.decls.insert(format!("declare i32 @ksn_parse_int(ptr, {}, i32, i32, i32, i32, ptr, ptr)", size_ty()));
    }
    if cx.env {
        cx.decls.insert("declare i64 @ksn_rt_sys_arg_count()".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_arg(i64, ptr)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_var(ptr, i64, ptr)".to_string());
    }
    if cx.time {
        cx.decls.insert("declare i64 @ksn_rt_sys_wall_ns()".to_string());
    }
    if cx.fs {
        cx.decls.insert("declare i64 @ksn_rt_sys_open(ptr, i64, i32, i32*)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_exists(ptr, i64)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_remove(ptr, i64, i32*)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_rename(ptr, i64, ptr, i64, i32*)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_mkdir(ptr, i64, i32*)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_rmdir(ptr, i64, i32*)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_stat(ptr, i64, i32, i32*)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_readdir(ptr, i64, ptr, i64, i32*)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_opendir(ptr, i64, i32*)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_openbeneath(i32, ptr, i64, i32*)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_fstat(i32, i32, i32*)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_read(i32, ptr, i64, i32*)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_write(i32, ptr, i64, i32*)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_close(i32)".to_string());
        cx.decls.insert("declare i64 @ksn_rt_sys_sync(i32, i32*)".to_string());
    }
    let mut out = String::from("; ksn kir llvm 1\n\n");
    if let Some(tg) = target {
        out.push_str(&format!("target datalayout = \"{}\"\ntarget triple = \"{}\"\n\n", tg.datalayout, tg.triple));
    }
    if !is_wasm(target) {
        out.push_str("declare ptr @llvm.stacksave.p0()\n");
    }
    for d in &cx.decls {
        out.push_str(d);
        out.push('\n');
    }
    out.push('\n');
    // 単一スレッドの wasm32 には TLS がない (`thread_local` は LLVM が拒む)。大域の計数で足りる。
    if is_wasm(target) {
        out.push_str(&format!("@ksn.depth = internal global i64 0\n@ksn.dmax = internal global i64 {}\n", crate::kir_exec::depth_limit(m.depth)));
    } else {
        // 上限 (`dmax`) とスタックの底 (`floor`、0 は検査なし) はスレッドごと。主スレッドは `ksn.rt.entry`、`spawn` したスレッドは `ksn.rt.tramp` が入れる。
        out.push_str(&format!(
            "@ksn.depth = internal thread_local global i64 0\n@ksn.dmax = internal thread_local global i64 {}\n@ksn.floor = internal thread_local global i64 0\n",
            MAX_DEPTH
        ));
    }
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
    {
        // `%llu` は 64 ビットの可変引数を読む。wasm32 の `usize` は 32 ビットなので、先に広げて渡す (x86_64 は広げない)。
        let sz = size_ty();
        let (a, b) = if sz == "i64" {
            ("%len".to_string(), "%ix".to_string())
        } else {
            ("%len64".to_string(), "%ix64".to_string())
        };
        let widen = if sz == "i64" {
            String::new()
        } else {
            format!("  %len64 = zext {sz} %len to i64\n  %ix64 = zext {sz} %ix to i64\n")
        };
        out.push_str(&format!(
            "define private void @ksn.panic.index({sz} %len, {sz} %ix) noreturn cold {{\n{widen}  %r = call i32 (i32, ptr, ...) @dprintf(i32 2, ptr @ksn.fmt.index, i64 {a}, i64 {b})\n  call void @exit(i32 101)\n  unreachable\n}}\n\n"
        ));
    }
    out.push_str("define private void @ksn.overflow() noreturn cold {\n  %r = call i32 (i32, ptr, ...) @dprintf(i32 2, ptr @ksn.fmt.so)\n  call void @exit(i32 102)\n  unreachable\n}\n");
    if cx.fmt_float {
        out.push_str(&fmt_float_helpers());
    }
    if cx.write {
        out.push_str(&write_helper(&m.aggs)?);
        out.push_str(&writeln_helper());
    }
    if cx.eprint {
        out.push_str(&eprint_helper());
    }
    if cx.net {
        out.push_str(&net_helpers(&mut cx, &m.aggs)?);
    }
    out.push_str(&funcs);
    out.push_str(&math);
    out.push('\n');
    out.push_str(&main);
    if is_wasm(target) {
        if let Some(why) = wasm_unsupported(&out, is_wasi(target)) {
            return Err(why);
        }
    }
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
    if is_windows(target) {
        out = windows_shims(&out);
    }
    Ok(out)
}

/// 入口 `main` を `NATIVE_STACK_BYTES` のスタックの別スレッドで動かす形にする (呼び出し深さの上限 `MAX_DEPTH` まで OS のスタックが足りるように。
/// 主スレッドのスタックは通常 8 MiB)。元の `main` は `@ksn.rt.entry0(argc, argv)` に改名して残す。スレッドを作れなければ主スレッドのまま走らせる。
fn on_big_stack(cx: &mut Ctx, main: String, depth: u64) -> String {
    let head = "define i32 @main(i32 %argc, ptr %argv) null_pointer_is_valid {\nentry:\n";
    let Some(rest) = main.strip_prefix(head) else { return main };
    for d in [
        "declare i32 @pthread_create(ptr, ptr, ptr, ptr)",
        "declare i32 @pthread_join(i64, ptr)",
        "declare i32 @pthread_attr_init(ptr)",
        "declare i32 @pthread_attr_setstacksize(ptr, i64)",
        "declare i32 @pthread_attr_destroy(ptr)",
    ] {
        cx.decl(d);
    }
    cx.globals.push_str("@ksn.rt.main_argc = internal global i32 0\n@ksn.rt.main_argv = internal global ptr null\n");
    format!(
        "define internal ptr @ksn.rt.entry(ptr %p) null_pointer_is_valid {{\nentry:\n  %spp = call ptr @llvm.stacksave.p0()\n  %sp = ptrtoint ptr %spp to i64\n  %fl0 = sub i64 %sp, {fl}\n  store i64 %fl0, ptr @ksn.floor\n  store i64 {dm}, ptr @ksn.dmax\n  %a = load i32, ptr @ksn.rt.main_argc\n  %v = load ptr, ptr @ksn.rt.main_argv\n  %c = call i32 @ksn.rt.entry0(i32 %a, ptr %v)\n  %w = zext i32 %c to i64\n  %q = inttoptr i64 %w to ptr\n  ret ptr %q\n}}\n\n\
         define i32 @main(i32 %argc, ptr %argv) null_pointer_is_valid {{\nentry:\n  store i32 %argc, ptr @ksn.rt.main_argc\n  store ptr %argv, ptr @ksn.rt.main_argv\n  %attr = alloca [64 x i8], align 16\n  %tid = alloca i64\n  %res = alloca ptr\n  store ptr null, ptr %res\n  %a0 = call i32 @pthread_attr_init(ptr %attr)\n  %a1 = call i32 @pthread_attr_setstacksize(ptr %attr, i64 {})\n  %r = call i32 @pthread_create(ptr %tid, ptr %attr, ptr @ksn.rt.entry, ptr null)\n  %a2 = call i32 @pthread_attr_destroy(ptr %attr)\n  %bad = icmp ne i32 %r, 0\n  br i1 %bad, label %direct, label %joined\ndirect:\n  %spp2 = call ptr @llvm.stacksave.p0()\n  %sp2 = ptrtoint ptr %spp2 to i64\n  %fl2 = sub i64 %sp2, 6291456\n  store i64 %fl2, ptr @ksn.floor\n  store i64 {dm}, ptr @ksn.dmax\n  %d = call i32 @ksn.rt.entry0(i32 %argc, ptr %argv)\n  ret i32 %d\njoined:\n  %t = load i64, ptr %tid\n  %j = call i32 @pthread_join(i64 %t, ptr %res)\n  %pv = load ptr, ptr %res\n  %iv = ptrtoint ptr %pv to i64\n  %code = trunc i64 %iv to i32\n  ret i32 %code\n}}\n\n\
         define internal i32 @ksn.rt.entry0(i32 %argc, ptr %argv) null_pointer_is_valid {{\nentry:\n{}",
        main_stack_bytes(depth), rest, fl = main_stack_bytes(depth) - STACK_MARGIN, dm = crate::kir_exec::depth_limit(depth)
    )
}

/// プログラムの入口 `main` (`emit_program`)。
fn emit_program_main(cx: &mut Ctx, m: &Module, f: &Func) -> R<String> {
    let aggs = &m.aggs;
    if !f.params().is_empty() {
        return Err(format!("入口 {} に引数がある", f.key));
    }
    let rt = lty(aggs, f.ret);
    let mut s = String::from("define i32 @main(i32 %argc, ptr %argv) null_pointer_is_valid {\nentry:\n");
    // std.env の C ランタイムが argc/argv を参照するため、グローバルに保存する。
    // シンボル名は C から `extern` 参照できるようドットなしにする。
    cx.globals.push_str("@ksn_rt_argc = global i32 0\n");
    cx.globals.push_str("@ksn_rt_argv = global ptr null\n");
    s.push_str("  store i32 %argc, ptr @ksn_rt_argc\n");
    s.push_str("  store ptr %argv, ptr @ksn_rt_argv\n");
    s.push_str(&format!("  %r = call {} {}()\n", rt, fname(&f.key)));
    // JS の完了を待つ入口 (`await_js` を使う wasm32): 待ちが残っていれば終了の戻り値 -1 (JS は `ksn_resume` で再開する)。
    if await_mode(m) {
        s.push_str(&format!("  %pend = call i1 {}()\n  br i1 %pend, label %waiting, label %go\nwaiting:\n  ret i32 -1\ngo:\n", fname(&format!("{}.pending", f.key))));
    }
    s.push_str(&exit_tail(cx, m, f)?);
    if await_mode(m) {
        // 再開の入口 `ksn_resume(h, v)`: JS が待っていた Promise の結果 `v` を `h` に入れて、枠を進める。戻り値は `main` と同じ。
        // 実行器が動いていない (正常に終わった) か、`exit` を通った (panic など) なら、何もせず -2 を返す。
        s.push_str("@ksn_exited = external global i32\n");
        s.push_str(&format!(
            "define i32 @ksn_resume(i32 %h, i32 %v) \"wasm-export-name\"=\"ksn_resume\" null_pointer_is_valid {{\nentry:\n  %ex = load i32, ptr @ksn_exited\n  %exd = icmp ne i32 %ex, 0\n  br i1 %exd, label %dead, label %live\nlive:\n  %running = call i1 {}()\n  br i1 %running, label %active, label %dead\nactive:\n  %put = call i1 {}(i32 %h, i32 %v)\n  %r = call {} {}()\n  %pend = call i1 {}()\n  br i1 %pend, label %waiting, label %go\nwaiting:\n  ret i32 -1\ngo:\n",
            fname("ksn.task.running"),
            fname("ksn.js.put"),
            rt,
            fname(&format!("{}.resume", f.key)),
            fname(&format!("{}.pending", f.key))
        ));
        let tail = exit_tail(cx, m, f)?;
        // `exit_tail` は関数を `}` で閉じるので、終了後の入口 `dead` はその手前に置く。
        let close = tail.rfind('}').expect("exit_tail ends the function");
        s.push_str(&tail[..close]);
        s.push_str("dead:\n  ret i32 -2\n");
        s.push_str(&tail[close..]);
    }
    Ok(s)
}

/// `ksn_resume` / `main` の終わり: 戻り値 `%r` を終了コードにする (Err の表示を含む)。`%r` を使う直前までは呼び出し側が作る。
fn exit_tail(cx: &mut Ctx, m: &Module, f: &Func) -> R<String> {
    let aggs = &m.aggs;
    let mut s = String::new();
    match f.ret {
        Ty::Res(_, e) => {
            let sb = slot_bits(aggs, f.ret);
            let n = sb + 1;
            let dk = m.find(crate::kir_lower::debug::ENTRY_DEBUG).ok_or("Err の表示の関数 (ksn.entry.debug) がない")?;
            let et = e.ty();
            let w = vbits(aggs, et);
            cx.decl(write_decl());
            // 入口と `ksn_resume` の両方が終わりの処理を書くので、文字列の定義は一度だけ出す。
            if !cx.globals.contains("@ksn.s.err =") {
                cx.globals.push_str(&str_const("ksn.s.err", "Error: "));
                cx.globals.push_str(&str_const("ksn.s.nl", "\n"));
            }
            s.push_str(&format!("  %t = lshr i{n} %r, {sb}\n  %tb = trunc i{n} %t to i1\n  br i1 %tb, label %err, label %ok\nerr:\n"));
            s.push_str(&format!("  %pw = trunc i{n} %r to i{sb}\n"));
            let x = if w == sb { "%pw".to_string() } else { s.push_str(&format!("  %pe = trunc i{sb} %pw to i{w}\n")); "%pe".to_string() };
            let z = size_ty();
            s.push_str(&format!("  %w1 = call {z} @write(i32 2, ptr @ksn.s.err, {z} 7)\n"));
            s.push_str(&format!("  %d = call i8 {}({} {})\n", fname(&dk.key), lty(aggs, et), x));
            s.push_str(&format!("  %w2 = call {z} @write(i32 2, ptr @ksn.s.nl, {z} 1)\n  ret i32 1\nok:\n  ret i32 0\n}}\n"));
        }
        _ => s.push_str("  ret i32 0\n}\n"),
    }
    Ok(s)
}

/// 入口が JS の完了を待つ形か (`ksn.task.await` の呼び出しが、変換で `ksn.task.await.poll` になって残っている)。
pub fn await_mode(m: &Module) -> bool {
    m.funcs.iter().any(|f| f.blocks.iter().any(|b| b.insts.iter().any(|i| matches!(&i.op, Op::Call { callee, .. } if callee == "ksn.task.await.poll"))))
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
/// libc の `write(int, const void *, size_t) -> ssize_t` の宣言。`size_t` はポインタの幅 (wasm32 は i32)。
fn write_decl() -> String {
    let z = size_ty();
    format!("declare {z} @write(i32, ptr, {z})")
}

fn eprint_helper() -> String {
    "define private void @ksn.eprint(ptr %p, i64 %n) {\nentry:\n  br label %loop\nloop:\n  %off = phi i64 [0, %entry], [%off2, %adv]\n  %left = sub i64 %n, %off\n  %done = icmp eq i64 %left, 0\n  br i1 %done, label %ret, label %try\ntry:\n  %q = getelementptr i8, ptr %p, i64 %off\n  %w = call i64 @write(i32 2, ptr %q, i64 %left)\n  %neg = icmp slt i64 %w, 0\n  br i1 %neg, label %ret, label %adv\nadv:\n  %off2 = add i64 %off, %w\n  br label %loop\nret:\n  ret void\n}\n".replace("i64", &size_ty())
}

/// `write` 命令の実体 `@ksn.write(fd, ptr, len) -> i64` (設計_KIR.md 12.2)。全バイトを書くまで `write(2)` を繰り返し、EINTR は再試行する。
/// 成功は 0。失敗は `(errno << 32) | ErrorKind の変種の添字` (errno は 0 にならないので、0 と区別できる)。
/// errno と `ErrorKind` の対応は木走査 (`natives::io_kind_name`、Rust の `io::Error::kind`) と同じ。
fn write_helper(aggs: &[AggDef]) -> R<String> {
    let kinds = aggs.iter().find(|d| d.name == "std.io.ErrorKind").ok_or("型表に std.io.ErrorKind がない")?;
    let AggKind::Enum(vs) = &kinds.kind else { return Err("std.io.ErrorKind が列挙型でない".into()) };
    let idx = |n: &str| vs.iter().position(|v| v.name == n).ok_or_else(|| format!("ErrorKind に {} がない", n));
    let errno_fn = errno_name();
    let mut s = String::new();
    let z = size_ty();
    s.push_str(&format!("define private i64 @ksn.write(i32 %fd, ptr %p, {z} %n) {{\nentry:\n  br label %loop\nloop:\n"));
    s.push_str(&format!("  %off = phi {z} [0, %entry], [%off2, %adv], [%off, %fail]\n  %left = sub {z} %n, %off\n  %done = icmp eq {z} %left, 0\n  br i1 %done, label %ok, label %try\n"));
    s.push_str(&format!("try:\n  %q = getelementptr i8, ptr %p, {z} %off\n  %w = call {z} @write(i32 %fd, ptr %q, {z} %left)\n  %neg = icmp slt {z} %w, 0\n  br i1 %neg, label %fail, label %adv\n"));
    s.push_str(&format!("adv:\n  %off2 = add {z} %off, %w\n  br label %loop\n"));
    s.push_str(&format!("fail:\n  %ep = call ptr @{}()\n  %e = load i32, ptr %ep\n  %intr = icmp eq i32 %e, 4\n  br i1 %intr, label %loop, label %err\n", errno_fn));
    s.push_str("ok:\n  ret i64 0\nerr:\n");
    let mut cur = format!("{}", idx("Other")?);
    for (k, (errno, name)) in [(1, "PermissionDenied"), (13, "PermissionDenied"), (2, "NotFound"), (17, "AlreadyExists"), (22, "InvalidInput"), (32, "BrokenPipe"), (12, "OutOfMemory"), (11, "TimedOut"), (110, "TimedOut")].iter().enumerate() {
        s.push_str(&format!("  %c{k} = icmp eq i32 %e, {errno}\n  %k{k} = select i1 %c{k}, i64 {}, i64 {cur}\n", idx(name)?));
        cur = format!("%k{k}");
    }
    s.push_str(&format!("  %ez = zext i32 %e to i64\n  %eh = shl i64 %ez, 32\n  %r = or i64 %eh, {cur}\n  ret i64 %r\n}}\n\n"));
    Ok(s)
}

/// `write.nl` の実体 `@ksn.writeln(fd, ptr, len) -> i64`: 本文と改行を 1 回の `write(2)` で出す (スレッド同士で 1 行が混ざらない)。
/// 4096 バイト未満なら枠へ複写して 1 回で書く。それ以上は 2 回に分ける (Rust の `println` も巨大な行は途中で切れうる)。結果は `@ksn.write` と同じ。
fn writeln_helper() -> String {
    let z = size_ty();
    format!(
        "define private i64 @ksn.writeln(i32 %fd, ptr %p, {z} %n) {{\nentry:\n  %small = icmp ult {z} %n, 4096\n  br i1 %small, label %one, label %two\none:\n  %m = add {z} %n, 1\n  %buf = alloca i8, {z} %m\n  call void @llvm.memcpy.p0.p0.{z}(ptr %buf, ptr %p, {z} %n, i1 false)\n  %e = getelementptr i8, ptr %buf, {z} %n\n  store i8 10, ptr %e\n  %r1 = call i64 @ksn.write(i32 %fd, ptr %buf, {z} %m)\n  ret i64 %r1\ntwo:\n  %r2 = call i64 @ksn.write(i32 %fd, ptr %p, {z} %n)\n  %bad = icmp ne i64 %r2, 0\n  br i1 %bad, label %fail, label %nl\nfail:\n  ret i64 %r2\nnl:\n  %nlb = alloca i8\n  store i8 10, ptr %nlb\n  %r3 = call i64 @ksn.write(i32 %fd, ptr %nlb, {z} 1)\n  ret i64 %r3\n}}\n\n"
    )
}

/// errno を `(errno << 32) | ErrorKind の変種の添字` の i64 値にする式 (`%e` は `i32` の errno)。
/// 書き方は `write_helper` と同じ対応表。
fn errno_chain(errno_fn: &str, idx: &impl Fn(&str) -> Result<usize, String>, s: &mut String) -> Result<String, String> {
    s.push_str(&format!("  %ep = call ptr @{}()\n  %e = load i32, ptr %ep\n", errno_fn));
    let mut cur = format!("{}", idx("Other")?);
    for (k, (errno, name)) in [(1, "PermissionDenied"), (13, "PermissionDenied"), (2, "NotFound"), (17, "AlreadyExists"), (22, "InvalidInput"), (32, "BrokenPipe"), (12, "OutOfMemory"), (11, "TimedOut"), (110, "TimedOut")].iter().enumerate() {
        s.push_str(&format!("  %c{k} = icmp eq i32 %e, {errno}\n  %k{k} = select i1 %c{k}, i64 {}, i64 {cur}\n", idx(name)?));
        cur = format!("%k{k}");
    }
    s.push_str(&format!("  %ez = zext i32 %e to i64\n  %eh = shl i64 %ez, 32\n  %enc = or i64 %eh, {cur}\n  %r = or i64 %enc, 9223372036854775808\n  ret i64 %r\n"));
    Ok(cur)
}

/// `std.net` 命令の実体 (`@ksn.net_*`)。
/// 成功は値 (fd / バイト数 / 0) を bit 63=0 で返す。失敗は `(1<<63) | (errno<<32) | ErrorKind` を返す。
/// `inst` 側で bit 63 をタグ、下位 64 ビットをペイロードとして `pack` する。
fn net_helpers(cx: &mut Ctx, aggs: &[AggDef]) -> R<String> {
    // `std.io.ErrorKind` は `read` / `write` / `flush` の失敗 (`std.io.Error`) を使うときだけ型表に入る。
    // `listen` などの失敗 (`std.net.Error`) は種類の添字を使わないので、型表に無ければ 0 を入れる (読まれない)。
    let vs: Vec<String> = match aggs.iter().find(|d| d.name == "std.io.ErrorKind") {
        Some(kinds) => match &kinds.kind {
            AggKind::Enum(vs) => vs.iter().map(|v| v.name.clone()).collect(),
            _ => return Err("std.io.ErrorKind が列挙型でない".into()),
        },
        None => vec![],
    };
    let idx = |n: &str| -> Result<usize, String> {
        if vs.is_empty() {
            return Ok(0);
        }
        vs.iter().position(|v| v == n).ok_or_else(|| format!("ErrorKind に {} がない", n))
    };
    let errno_fn = errno_name();
    let mut s = String::new();
    // `write` の libc 宣言は、`cx.write` (write/eprint 命令) が立っていればトップの declare ループ
    // (`cx.decls`) で出力される。そうでないときだけここに出力する。
    let write_decl = write_decl();
    let write_already = cx.decls.iter().any(|d| *d == write_decl);
    // 既に `cx.decls` に入っている宣言 (`errno` の取得など) は、トップの宣言の並びに任せて二重に出さない。
    for d in [
        "declare i32 @accept(i32, ptr, ptr)",
        "declare i32 @read(i32, ptr, i64)",
        "declare i32 @close(i32)",
        "declare i32 @getsockname(i32, ptr, ptr)",
        &format!("declare ptr @{}()", errno_fn),
    ] {
        if !cx.decls.iter().any(|x| x == d) {
            s.push_str(d);
            s.push('\n');
        }
    }
    if !write_already {
        s.push_str(&write_decl);
        s.push('\n');
    }
    macro_rules! hdr {
        ($name:expr, $ret:expr) => {
            s.push_str(&format!("define private i64 @{}({}) {{\nentry:\n", $name, $ret));
        };
    }
    // net.accept(fd)
    hdr!("ksn.net_accept", "i32 %fd");
    s.push_str("  %a = call i32 @accept(i32 %fd, ptr null, ptr null)\n  %neg = icmp slt i32 %a, 0\n  br i1 %neg, label %err, label %done\ndone:\n  %v = zext i32 %a to i64\n  ret i64 %v\nerr:\n");
    errno_chain(errno_fn, &idx, &mut s)?;
    s.push_str("}\n\n");
    // net.read(fd, ptr, len)
    hdr!("ksn.net_read", "i32 %fd, ptr %p, i64 %n");
    s.push_str("  %raw = call i32 @read(i32 %fd, ptr %p, i64 %n)\n  %neg = icmp slt i32 %raw, 0\n  br i1 %neg, label %err, label %done\ndone:\n  %v = zext i32 %raw to i64\n  ret i64 %v\nerr:\n");
    errno_chain(errno_fn, &idx, &mut s)?;
    s.push_str("}\n\n");
    // net.write(fd, ptr, len)
    hdr!("ksn.net_write", "i32 %fd, ptr %p, i64 %n");
    s.push_str("  %raw = call i32 @write(i32 %fd, ptr %p, i64 %n)\n  %neg = icmp slt i32 %raw, 0\n  br i1 %neg, label %err, label %done\ndone:\n  %v = zext i32 %raw to i64\n  ret i64 %v\nerr:\n");
    errno_chain(errno_fn, &idx, &mut s)?;
    s.push_str("}\n\n");
    // net.flush(fd): ソケットへの書き出しは直ちに反映されるため成功（no-op）とする。
    hdr!("ksn.net_flush", "i32 %fd");
    s.push_str("  ret i64 0\n}\n\n");
    // net.close(fd)
    hdr!("ksn.net_close", "i32 %fd");
    s.push_str("  %raw = call i32 @close(i32 %fd)\n  %neg = icmp slt i32 %raw, 0\n  br i1 %neg, label %err, label %done\ndone:\n  ret i64 0\nerr:\n");
    errno_chain(errno_fn, &idx, &mut s)?;
    s.push_str("}\n\n");
    // net.local_port(fd): getsockname で sockaddr_in の sin_port (オフセット 2、ネットワークバイトオーダ) を取る。
    hdr!("ksn.net_local_port", "i32 %fd");
    s.push_str("  %addr = alloca i8, i64 16\n  %len = alloca i64, i64 8\n  store i64 16, ptr %len\n  %raw = call i32 @getsockname(i32 %fd, ptr %addr, ptr %len)\n  %neg = icmp slt i32 %raw, 0\n  br i1 %neg, label %err, label %done\ndone:\n  %p = getelementptr i8, ptr %addr, i64 2\n  %pv = load i16, ptr %p, align 2\n  %hi = lshr i16 %pv, 8\n  %lo = shl i16 %pv, 8\n  %port = or i16 %hi, %lo\n  %v = zext i16 %port to i64\n  ret i64 %v\nerr:\n");
    errno_chain(errno_fn, &idx, &mut s)?;
    s.push_str("}\n\n");
    Ok(s)
}

// ---------------------------------------------------------------- 入口 (main)

/// 浮動小数の書式化は、C の実行時補助 `kir_rt.c` の `ksn_fmt_f64` / `ksn_fmt_f32` に任せる (設計_KIR.md 17.11)。
/// 規則は `py_float` / `py_float32` と同じ。結果は静的バッファへのポインタ (NUL 終端)。
fn fmt_float_helpers() -> String {
    "declare ptr @ksn_fmt_f64(double)\ndeclare ptr @ksn_fmt_f32(float)\n".to_string()
}

/// C の実行時補助 (`kir_rt.c`) の本文。生成した IR が `@ksn_fmt_` / `@ksn_utf8_` を呼ぶときだけ、clang に一緒に渡す。
pub const RUNTIME_C: &str = include_str!("kir_rt.c");

/// Kasane で書いた基本メソッドの単相インスタンス (`alloc/vec.ksnl` / `alloc/string.ksnl`) か。再帰せず、呼び先も有界。
fn kasane_base_inst(key: &str) -> bool {
    key.starts_with("alloc.string.String.") || key.starts_with("alloc.vec.Vec.")
}

/// IR が C の実行時補助を要するか。
pub fn needs_runtime(ll: &str) -> bool {
    ll.contains("@ksn_w_") || ll.contains("@ksn_fmt_") || ll.contains("@ksn_utf8_") || ll.contains("@ksn_parse_int") || ll.contains("@ksn_rt_sys_") || ll.contains("@ksn_ext_resolve")
}

fn emit_main(cx: &mut Ctx, aggs: &[AggDef], f: &Func) -> R<String> {
    // 入口は語 1 つ (128 ビット) で受け渡しするので、枠が 64 ビットの Option / Result と整数・bool・浮動小数だけ扱う。
    let bad = |t: Ty| matches!(t, Ty::Ptr | Ty::Ref(..) | Ty::Agg(_)) || (matches!(t, Ty::Opt(_) | Ty::Res(..)) && (has_agg(t) || slot_bits(aggs, t) > 64));
    if f.params().iter().any(|(_, t)| bad(*t)) || bad(f.ret) {
        return Err(format!("関数 {} は集成体・参照・ptr を引数か戻り値に持つ (この入口では扱えない)", f.key));
    }
    let mut s = String::from("define i32 @main(i32 %argc, ptr %argv) null_pointer_is_valid {\nentry:\n");
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
    /// 動的ロードの `extern` (`キー → (ライブラリ, 名前)`)。
    extern_dl: &'a HashMap<String, (String, String)>,
    /// 全関数 (`Op::Fnptr` の呼び先の型を引くため)。
    funcs: &'a [Func],
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
    fn new(cx: &'a mut Ctx, aggs: &'a [AggDef], f: &'a Func, extern_links: &'a HashMap<String, String>, extern_dl: &'a HashMap<String, (String, String)>, funcs: &'a [Func], fi: usize) -> Fe<'a> {
        Fe { cx, aggs, f, extern_links, extern_dl, funcs, fi, tys: HashMap::new(), lit: HashMap::new(), n: 0, body: String::new(), cur: String::new(), edges: BTreeMap::new() }
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

    /// `usize` の値を、C のランタイムの長さの引数 (`i64`) の形にする。wasm32 の `usize` は i32 なので広げる
    /// (x86_64 / aarch64 は `usize` が i64 なので、値の名前をそのまま返す。出力は変わらない)。
    fn len64(&mut self, v: VId) -> String {
        let s = self.r(v);
        if size_ty() == "i64" {
            s
        } else {
            self.t(format!("zext {} {} to i64", size_ty(), s))
        }
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

    /// 64 ビットの語の成分 `v` (`i64`) を、長さ・添字の `usize` の型にする (wasm32 は trunc。x86_64 / aarch64 は同じ値)。
    fn lane_sz(&mut self, v: &str) -> String {
        let z = size_ty();
        if z == "i64" {
            v.to_string()
        } else {
            self.t(format!("trunc i64 {} to {}", v, z))
        }
    }

    /// `usize` の値 `v` を `i64` の値にする (C の `long long` の引数など、幅を変えない境界のため)。
    fn sz_to_i64(&mut self, v: VId) -> String {
        let z = size_ty();
        let x = self.r(v);
        if z == "i64" {
            x
        } else {
            self.t(format!("zext {} {} to i64", z, x))
        }
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
            Ty::F32 | Ty::F64 => {
                self.canon_i(t, v)
            }
            Ty::Ptr | Ty::Ref(..) => self.t(format!("ptrtoint ptr {} to {}", v, pty())),
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
            Ty::Ptr | Ty::Ref(..) => self.t(format!("inttoptr {} {} to ptr", pty(), x)),
            _ => x,
        }
    }

    /// `std.net` の i64 結果 (bit 63 = エラー、下位 64 ビット = ペイロード) を `Result` の i65 値に pack する。
    /// `net_err` の命令 (`listen` / `connect` / `accept` / `close` / `local_port`) の失敗は `std.net.Error` (`{code}`) なので、
    /// 失敗のペイロードは errno (上位 32 ビットの値を下位へ) にする。`read` / `write` / `flush` は `std.io.Error` のまま。
    fn net_result(&mut self, dst: &str, r: &str, net_err: bool) -> R<()> {
        let neg = self.t(format!("icmp slt i64 {}, 0", r));
        let ok_pay = self.t(format!("and i64 {}, 9223372036854775807", r));
        let pay = if net_err {
            let sh = self.t(format!("lshr i64 {}, 32", r));
            let errno = self.t(format!("and i64 {}, 4294967295", sh));
            self.t(format!("select i1 {}, i64 {}, i64 {}", neg, errno, ok_pay))
        } else {
            ok_pay
        };
        self.pack(dst, 64, &neg, &pay);
        Ok(())
    }

    /// `std.net.Error` の失敗 (`Result<_, std.net.Error>`) を pack する。`ep` は `fs_err_slot` (上位 32 ビットが errno)。
    /// 成功のペイロードは `r` (負でない値)。`std.net.Error` は `{ code: i32 }` なので、失敗のペイロードは errno の 32 ビット。
    fn net_err_result(&mut self, dst: &str, r: &str, ep: &str) {
        let neg = self.t(format!("icmp slt i64 {}, 0", r));
        let ev = self.t(format!("load i64, ptr {}, align 4", ep));
        let errno = self.t(format!("lshr i64 {}, 32", ev));
        let ok_pay = self.t(format!("and i64 {}, 9223372036854775807", r));
        let pay = self.t(format!("select i1 {}, i64 {}, i64 {}", neg, errno, ok_pay));
        self.pack(dst, 64, &neg, &pay);
    }

    /// fat (`&str`) の値を `(ptr, len)` の SSA 名に分ける (`std.net` の命令の引数)。
    fn fat_parts(&mut self, v: VId) -> R<(String, String)> {
        let Ty::Agg(_) = self.ty(v) else { return Err("std.net のアドレスが fat (&str) でない".into()) };
        let av = self.r(v);
        let pi = self.extract(&av, 128, 0, 64);
        let p = self.t(format!("inttoptr i64 {} to ptr", pi));
        let n = self.extract(&av, 128, 8, 64);
        Ok((p, n))
    }

    /// `std.process` の命令 (`ProcFn`)。ランタイム `sysrt.c` の `ksn_rt_sys_proc_*` を呼ぶ。
    fn proc_op(&mut self, d: &str, i: &Inst, f: ProcFn, args: &[VId]) -> R<()> {
        use ProcFn::*;
        self.cx.fs = true;
        self.cx.decl("declare i32 @ksn_rt_sys_proc_new(ptr, i64)");
        self.cx.decl("declare void @ksn_rt_sys_proc_cmd(i32, i32, ptr, i64, ptr, i64)");
        self.cx.decl("declare i64 @ksn_rt_sys_proc_spawn(i32, i32, ptr, ptr)");
        self.cx.decl("declare i64 @ksn_rt_sys_proc_wait(i32, ptr, ptr)");
        self.cx.decl("declare i64 @ksn_rt_sys_proc_kill(i32, ptr)");
        let mut fat = |me: &mut Self, v: VId| -> (String, String) {
            let av = me.r(v);
            let pi = me.extract(&av, 128, 0, 64);
            let p = me.t(format!("inttoptr i64 {} to ptr", pi));
            let n = me.extract(&av, 128, 8, 64);
            (p, n)
        };
        match f {
            New => {
                let (p, n) = fat(self, args[0]);
                self.line(format!("{} = call i32 @ksn_rt_sys_proc_new(ptr {}, i64 {})", d, p, n));
            }
            Arg | EnvRemove | Dir | Env | EnvClear | InPiped | InNull | OutPiped | OutNull | ErrPiped | ErrNull | Cancel => {
                let id = self.r(args[0]);
                let (a, an) = if args.len() > 1 { fat(self, args[1]) } else { ("null".to_string(), "0".to_string()) };
                let (b, bn) = if args.len() > 2 { fat(self, args[2]) } else { ("null".to_string(), "0".to_string()) };
                self.line(format!("call void @ksn_rt_sys_proc_cmd(i32 {}, i32 {}, ptr {}, i64 {}, ptr {}, i64 {})", id, f.code(), a, an, b, bn));
            }
            Spawn | Output => {
                let Ty::Res(Pay::Agg(ca), _) = i.ty else { return Err("proc.spawn の結果が Result<Child, _> でない".into()) };
                let w = agg_bits(self.aggs, ca);
                if w != 128 {
                    return Err("std.process.Child の大きさが 16 バイトでない".into());
                }
                let cs = self.t("alloca [4 x i32], align 4".to_string());
                let ep = self.fs_err_slot();
                let cap = if f == Output { 1 } else { 0 };
                let r = self.t(format!("call i64 @ksn_rt_sys_proc_spawn(i32 {}, i32 {}, ptr {}, ptr {})", self.r(args[0]), cap, cs, ep));
                let neg = self.t(format!("icmp slt i64 {}, 0", r));
                let okp = self.t(format!("load i128, ptr {}, align 4", cs));
                let ev = self.t(format!("load i64, ptr {}, align 4", ep));
                let ev = self.t(format!("zext i64 {} to i128", ev));
                let pay = self.t(format!("select i1 {}, i128 {}, i128 {}", neg, ev, okp));
                self.pack(d, 128, &neg, &pay);
            }
            Wait => {
                let Ty::Res(Pay::Agg(ca), _) = i.ty else { return Err("proc.wait の結果が Result<ExitStatus, _> でない".into()) };
                if agg_bits(self.aggs, ca) != 64 {
                    return Err("std.process.ExitStatus の大きさが 8 バイトでない".into());
                }
                let st = self.t("alloca [2 x i32], align 4".to_string());
                let ep = self.fs_err_slot();
                let r = self.t(format!("call i64 @ksn_rt_sys_proc_wait(i32 {}, ptr {}, ptr {})", self.r(args[0]), st, ep));
                let neg = self.t(format!("icmp slt i64 {}, 0", r));
                let okp = self.t(format!("load i64, ptr {}, align 4", st));
                let ev = self.t(format!("load i64, ptr {}, align 4", ep));
                let pay = self.t(format!("select i1 {}, i64 {}, i64 {}", neg, ev, okp));
                self.pack(d, 64, &neg, &pay);
            }
            Kill => {
                let ep = self.fs_err_slot();
                let r = self.t(format!("call i64 @ksn_rt_sys_proc_kill(i32 {}, ptr {})", self.r(args[0]), ep));
                self.fs_result(d, &r, &ep);
            }
        }
        Ok(())
    }

    /// `std.fs` の失敗の受け口 (`[ErrorKind の添字, errno]` の 2 つの i32)。`sysrt.c` の `fail` と同じ書き方。
    fn fs_err_slot(&mut self) -> String {
        let ep = self.t("alloca [2 x i32], align 4".to_string());
        self.line(format!("store i64 0, ptr {}, align 4", ep));
        ep
    }

    /// `std.fs` の i64 結果 (負 = 失敗) を `Result<_, std.io.Error>` の値に pack する。
    /// 失敗のペイロードは `fs_err_slot` の 8 バイト (下位 = ErrorKind、上位 = errno) をそのまま使う (clif の `sys_result` と同じ並び)。
    /// 成功のペイロードは `r` (fd / バイト数 / 0)。
    fn fs_result(&mut self, dst: &str, r: &str, ep: &str) {
        let neg = self.t(format!("icmp slt i64 {}, 0", r));
        let ev = self.t(format!("load i64, ptr {}, align 4", ep));
        let ok_pay = self.t(format!("and i64 {}, 9223372036854775807", r));
        let pay = self.t(format!("select i1 {}, i64 {}, i64 {}", neg, ev, ok_pay));
        self.pack(dst, 64, &neg, &pay);
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
            Ty::Unit | Ty::Res(..) => Err(format!("型 {} は集成体のフィールドにできない", t.name())),
            // `Option<P>` の幅はバイト単位に広げた枠 (kir.rs の `size_of` と同じ)。
            Ty::Opt(_) => Ok(size_of(self.aggs, t) * 8),
            t => Ok(size_of(self.aggs, t) * 8),
        }
    }

    /// フィールドの値を、その幅の整数にする。`Option` は枠とタグの幅 (`1 + 枠`) の整数を、フィールドの幅へ 0 で広げる。
    fn to_bits(&mut self, t: Ty, v: &str) -> R<String> {
        Ok(match t {
            Ty::Bool => self.t(format!("zext i1 {} to i8", v)),
            Ty::F32 | Ty::F64 => {
                self.canon_i(t, v)
            }
            Ty::Ptr | Ty::Ref(..) => self.t(format!("ptrtoint ptr {} to {}", v, pty())),
            Ty::Opt(_) => {
                let w = self.field_w(t)?;
                let src = self.lt(t);
                self.t(format!("zext {} {} to i{}", src, v, w))
            }
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
            Ty::Ptr | Ty::Ref(..) => self.line(format!("{} = inttoptr {} {} to ptr", dst, pty(), b)),
            Ty::Opt(_) => {
                // フィールドの幅 (上のバイトは 0) から、枠とタグの幅 (`1 + 枠`) へ切り詰める。
                let ty = self.lt(t);
                let w = self.field_w(t).unwrap_or_else(|_| self.opt_bits_of(t));
                self.line(format!("{} = trunc i{} {} to {}", dst, w, b, ty));
            }
            t => {
                let ty = self.lt(t);
                self.mov(dst, &ty, b)
            }
        }
    }

    /// `Option` / `Result` の値の幅 (タグを含む、`1 + 枠`)。フィールドの幅 (`field_w`) 以下。
    fn opt_bits_of(&self, t: Ty) -> u32 {
        1 + slot_bits(self.aggs, t)
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

    /// ビットが見える所 (`to_bits`・メモリへの書き出し・FFI の引数) で NaN を正の quiet NaN にそろえる (設計_KIR.md 17.8)。
    /// 演算ごとには行わない (ベクトル化を妨げるため)。NaN 以外は変えない。
    fn canon_f(&mut self, ty: Ty, v: &str) -> String {
        let (t, it) = match ty {
            Ty::F32 => ("float", "i32"),
            Ty::F64 => ("double", "i64"),
            _ => return v.to_string(),
        };
        let c = self.canon_i(ty, v);
        self.t(format!("bitcast {} {} to {}", it, c, t))
    }

    /// `canon_f` の整数版 (ビット列を返す)。浮動小数の `select` は LLVM が NaN どうしを同一視して畳むので、整数のビットで判定する。
    fn canon_i(&mut self, ty: Ty, v: &str) -> String {
        let (t, it, inf, canon) = match ty {
            Ty::F32 => ("float", "i32", 0x7F80_0000u64, 0x7FC0_0000u64),
            _ => ("double", "i64", 0x7FF0_0000_0000_0000u64, 0x7FF8_0000_0000_0000u64),
        };
        let mask = if ty == Ty::F32 { 0x7FFF_FFFFu64 } else { 0x7FFF_FFFF_FFFF_FFFFu64 };
        let b = self.t(format!("bitcast {} {} to {}", t, v, it));
        let ab = self.t(format!("and {} {}, {}", it, b, mask));
        let n = self.t(format!("icmp ugt {} {}, {}", it, ab, inf));
        self.t(format!("select i1 {}, {} {}, {} {}", n, it, canon, it, b))
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
                        self.f64_intr("sqrt", &w)
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
                        self.t(format!("frem double {}, {}", x, y))
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
                self.line(format!("{} = {} {} {}, {}", d, ins, t, x, y));
            }
        }
    }

    fn conv(&mut self, i: &Inst, src: Ty, a: VId) {
        let d = format!("%v{}", i.dst);
        let v = self.r(a);
        let dst = i.ty;
        if src.is_int() && dst.is_int() {
            self.int_cast(&d, &v, src.bits(), dst.bits(), src.signed());
        } else if src.is_float() && dst.is_int() {
            // 飽和する変換 (NaN は 0)。`trunc_to_*` は範囲を確かめてから使う。
            let (st, ft) = (self.lt(src), if src == Ty::F32 { "f32" } else { "f64" });
            let k = if dst.signed() { "fptosi" } else { "fptoui" };
            let name = format!("llvm.{}.sat.i{}.{}", k, dst.bits(), ft);
            let dt = self.lt(dst);
            self.decl(format!("declare {} @{}({})", dt, name, st));
            self.line(format!("{} = call {} @{}({} {})", d, dt, name, st, v));
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
                    let sz = size_ty();
                    let c = self.t(format!("icmp uge {} {}, {}", sz, x, n));
                    self.n += 1;
                    let (p, k) = (format!("p{}", self.n), format!("c{}", self.n));
                    self.line(format!("br i1 {}, label %{}, label %{}", c, p, k));
                    self.label(&p);
                    self.line(format!("call void @ksn.panic.index({} {}, {} {})", sz, n, sz, x));
                    self.line("unreachable".into());
                    self.label(&k);
                    Ok(())
                }
                // 致命的な乱数 (`Rng.from_entropy`): 受け口を null にすると、ランタイムは失敗で文言を出して終了コード 101 で終わる。
                Op::RandFill { buf, len, fatal: true } => {
                    self.decl("declare i64 @ksn_rt_sys_random(ptr, i64, ptr)".to_string());
                    let (b, n) = (self.r(*buf), self.r(*len));
                    self.line(format!("call i64 @ksn_rt_sys_random(ptr {}, i64 {}, ptr null)", b, n));
                    Ok(())
                }
                Op::Eprint { data } => {
                    // panic の文言の一部を stderr へ書く (`@ksn.eprint`。エラーは捨てる)。
                    self.cx.eprint = true;
                    self.cx.decl(write_decl());
                    let av = self.r(*data);
                    let pi = self.extract(&av, 128, 0, 64);
                    let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                    let n = self.extract(&av, 128, 8, 64);
                    let n = self.lane_sz(&n);
                    self.line(format!("call void @ksn.eprint(ptr {}, {} {})", p, size_ty(), n));
                    Ok(())
                }
                Op::Proc { f, args } => self.proc_op(&d, i, *f, args),
                Op::Exit { code } => {
                    // `std.process.exit`: 終了コードで戻らずに終わる。後続は下ろし側が `unreachable` で切る。
                    let c = self.r(*code);
                    self.line(format!("call void @exit(i32 {})", c));
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
                    self.cx.decl(write_decl());
                    let pi = self.extract(&fat, 128, 0, 64);
                    let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                    let n = self.extract(&fat, 128, 8, 64);
                    let n = self.lane_sz(&n);
                    self.line(format!("call void @ksn.eprint(ptr {}, {} {})", p, size_ty(), n));
                    Ok(())
                }
                _ => Ok(()),
            };
        }
        match &i.op {
            Op::Const(_) => {}
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
                if matches!(*src, Ty::F32 | Ty::F64) {
                    let c = self.canon_i(*src, &v);
                    self.line(format!("{} = add {} {}, 0", d, dt, c));
                } else {
                    self.line(format!("{} = bitcast {} {} to {}", d, st, v, dt));
                }
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
                if self.extern_links.contains_key(callee) && (matches!(i.ty, Ty::Agg(_)) || args.iter().any(|v| matches!(self.ty(*v), Ty::Agg(_)))) {
                    return self.c_call(&d, i, callee, args);
                }
                let rt = self.lt(i.ty);
                let is_ext = self.extern_links.contains_key(callee);
                let mut a: Vec<String> = vec![];
                for v in args {
                    let (vt, vr) = (self.ty(*v), self.r(*v));
                    let vr = if is_ext { self.canon_f(vt, &vr) } else { vr };
                    a.push(format!("{} {}", lty(aggs, vt), vr));
                }
                if let Some(sym) = self.extern_links.get(callee) {
                    // C の関数の戻り値が `()` のときは `void` で呼ぶ (宣言も `void`。実行時の補助の宣言と食い違わない)。
                    let unit = i.ty == Ty::Unit;
                    let target = if self.extern_dl.contains_key(callee) {
                        let fp = self.tmp();
                        self.line(format!(
                            "{} = call ptr @ksn_ext_resolve(ptr @ksn.ext.lib.{}, ptr @ksn.ext.sym.{}, ptr @ksn.ext.slot.{})",
                            fp, sym, sym, sym
                        ));
                        fp
                    } else {
                        format!("@{}", sym)
                    };
                    if unit {
                        self.line(format!("call void {}({})", target, a.join(", ")));
                        self.line(format!("{} = add i8 0, 0", d));
                    } else {
                        self.line(format!("{} = call {} {}({})", d, rt, target, a.join(", ")));
                    }
                } else {
                    self.line(format!("{} = call {} {}({})", d, rt, fname(callee), a.join(", ")));
                }
            }
            // 関数のアドレスを `ptr` として取る (C のコールバック用)。
            // `callee` の関数は C 呼び出し規約と互換のシグネチャ (`ptr`/`i32` など) で下ろされている。
            Op::FnPtr { callee } => {
                let Some(f) = self.funcs.iter().find(|f| f.key == *callee) else {
                    return Err(format!("fnptr の呼び先 {} が見つからない", callee));
                };
                if f.params().iter().any(|(_, t)| matches!(t, Ty::Agg(_))) || matches!(f.ret, Ty::Agg(_)) {
                    let th = self.c_thunk(f)?;
                    self.line(format!("{} = getelementptr i8, ptr {}, i64 0", d, th));
                    return Ok(());
                }
                let rt = self.lt(f.ret);
                let params: Vec<String> = f.params().iter().map(|(_, t)| lty(self.aggs, *t)).collect();
                let fnty = format!("{} ({})*", rt, params.join(", "));
                self.line(format!("{} = bitcast {} {} to ptr", d, fnty, fname(callee)));
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
            Op::ScopeEnd(_) | Op::Carry { .. } | Op::Own { .. } => {}
            Op::AsShared(p) | Op::Reborrow(p) | Op::Borrow { a: p, .. } => {
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
            Op::GlobalBytes { name, size } => {
                let g = format!("@\"ksn.g.{}\"", name);
                // 実行器の状態 (`ksn.task.g`・`ksn.wrap.pend`) はスレッドごとに持つ (2 本のスレッドの `block_on` が同じ表を触らない)。単一スレッドの wasm32 には TLS がない。
                let tls = (name.starts_with("ksn.task.") || name == "ksn.wrap.pend") && size_ty() == "i64";
                let def = format!("{} = internal {}global [{} x i8] zeroinitializer, align 16\n", g, if tls { "thread_local " } else { "" }, size);
                if !self.cx.globals.contains(&def) {
                    self.cx.globals.push_str(&def);
                }
                self.line(format!("{} = getelementptr i8, ptr {}, i64 0", d, g));
            }
            Op::StaticBytes(bytes) => {
                let g = format!("ksn.s.{}.{}", self.fi, i.dst);
                let esc: String = bytes.iter().map(|b| format!("\\{:02X}", b)).collect();
                self.cx.globals.push_str(&format!("@{} = private unnamed_addr constant [{} x i8] c\"{}\"\n", g, bytes.len(), esc));
                self.line(format!("{} = getelementptr i8, ptr @{}, i64 0", d, g));
            }
            Op::PtrAdd { p, off } => {
                let (pv, o) = (self.r(*p), self.r(*off));
                let ot = self.lt(self.ty(*off));
                self.line(format!("{} = getelementptr i8, ptr {}, {} {}", d, pv, ot, o));
            }
            Op::FloatText { bits, a, dst } => {
                self.cx.fmt_float = true;
                let (func, arg_ty) = if *bits == 64 { ("@ksn_fmt_f64_to", "double") } else { ("@ksn_fmt_f32_to", "float") };
                self.cx.decl(format!("declare i64 {}({}, ptr)", func, arg_ty));
                let r = self.t(format!("call i64 {}({} {}, ptr {})", func, arg_ty, self.r(*a), self.r(*dst)));
                if size_ty() == "i64" {
                    self.line(format!("{} = add i64 {}, 0", d, r));
                } else {
                    self.line(format!("{} = trunc i64 {} to {}", d, r, size_ty()));
                }
            }
            Op::Utf8Valid { p, len } => {
                self.cx.decl("declare i64 @ksn_utf8_valid(ptr, i64)".to_string());
                let pv = self.r(*p);
                let n = self.sz_to_i64(*len);
                if size_ty() == "i64" {
                    self.line(format!("{} = call i64 @ksn_utf8_valid(ptr {}, i64 {})", d, pv, n));
                } else {
                    // 結果は `usize` (wasm32 は 32 ビット)。C の戻り値 (64 ビット) を狭める。
                    let r = self.t(format!("call i64 @ksn_utf8_valid(ptr {}, i64 {})", pv, n));
                    self.line(format!("{} = trunc i64 {} to {}", d, r, size_ty()));
                }
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
            Op::MemStore { .. } | Op::Store { .. } | Op::HeapFree(_) | Op::IndexCheck { .. } | Op::Panic { .. } | Op::Eprint { .. } | Op::Abort | Op::Exit { .. } => {
                return Err("unit 以外の結果を持つ副作用命令".into())
            }
            Op::Write { fd, data, nl } => {
                let Ty::Res(_, e) = i.ty else { return Err("write の結果が Result でない".into()) };
                if vbits(aggs, e.ty()) != 64 {
                    return Err("std.io.Error の幅が 8 バイトでない".into());
                }
                self.cx.write = true;
                self.cx.decl(write_decl());
                self.cx.decl(format!("declare ptr @{}()", errno_name()));
                self.cx.decl(format!("declare void @llvm.memcpy.p0.p0.{}(ptr, ptr, {}, i1)", size_ty(), size_ty()));
                let (f, av) = (self.r(*fd), self.r(*data));
                let pi = self.extract(&av, 128, 0, 64);
                let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                let n = self.extract(&av, 128, 8, 64);
                let n = self.lane_sz(&n);
                let callee = if *nl { "ksn.writeln" } else { "ksn.write" };
                let r = self.t(format!("call i64 @{}(i32 {}, ptr {}, {} {})", callee, f, p, size_ty(), n));
                let ok = self.t(format!("icmp eq i64 {}, 0", r));
                let tag = self.t(format!("xor i1 {}, true", ok));
                self.pack(&d, 64, &tag, &r);
            }
            Op::NetListen { addr } | Op::NetConnect { addr } => {
                // 木走査 (`TcpListener::bind` / `TcpStream::connect`) と同じく、"host:port" を getaddrinfo で解決する (sysrt.c)。
                self.cx.net = true;
                let is_listen = matches!(i.op, Op::NetListen { .. });
                let fnn = if is_listen { "ksn_rt_sys_listen" } else { "ksn_rt_sys_connect" };
                self.decl(format!("declare i64 @{}(ptr, i64, ptr)", fnn));
                let (p, ni) = self.fat_parts(*addr)?;
                let ep = self.fs_err_slot();
                let r = self.t(format!("call i64 @{}(ptr {}, i64 {}, ptr {})", fnn, p, ni, ep));
                self.net_err_result(&d, &r, &ep);
            }
            Op::NetAccept { sock } => {
                self.cx.net = true;
                let r = self.t(format!("call i64 @ksn.net_accept(i32 {})", self.r(*sock)));
                self.net_result(&d, &r, true)?;
            }
            Op::NetRead { sock, buf, len } | Op::NetWrite { sock, buf, len } => {
                // 期限を見る読み書き (sysrt.c の `ksn_rt_sys_net_rw`)。失敗は std.io.Error の受け口へ。
                self.cx.net = true;
                let write_side = if matches!(i.op, Op::NetWrite { .. }) { 1 } else { 0 };
                self.decl("declare i64 @ksn_rt_sys_net_rw(i32, i32, ptr, i64, ptr)".to_string());
                let ep = self.fs_err_slot();
                let r = self.t(format!(
                    "call i64 @ksn_rt_sys_net_rw(i32 {}, i32 {}, ptr {}, i64 {}, ptr {})",
                    self.r(*sock),
                    write_side,
                    self.r(*buf),
                    self.r(*len),
                    ep
                ));
                self.fs_result(&d, &r, &ep);
            }
            Op::NetFlush { sock } => {
                self.cx.net = true;
                let r = self.t(format!("call i64 @ksn.net_flush(i32 {})", self.r(*sock)));
                self.net_result(&d, &r, false)?;
            }
            Op::NetClose { sock } => {
                self.cx.net = true;
                let r = self.t(format!("call i64 @ksn.net_close(i32 {})", self.r(*sock)));
                self.net_result(&d, &r, true)?;
            }
            Op::NetLocalPort { sock } => {
                self.cx.net = true;
                let r = self.t(format!("call i64 @ksn.net_local_port(i32 {})", self.r(*sock)));
                self.net_result(&d, &r, true)?;
            }
            Op::NetSys { kind, args } => {
                // 追加分 (`NetK`)。実体は sysrt.c の `ksn_rt_sys_net_*` (木走査 net.rs と同じ結果)。
                self.cx.net = true;
                match kind {
                    NetK::ReadTimeout | NetK::WriteTimeout => {
                        let write_side = if matches!(kind, NetK::WriteTimeout) { 1 } else { 0 };
                        self.decl("declare i64 @ksn_rt_sys_net_timeout(i32, i32, i64, ptr)".to_string());
                        let ep = self.fs_err_slot();
                        let r = self.t(format!("call i64 @ksn_rt_sys_net_timeout(i32 {}, i32 {}, i64 {}, ptr {})", self.r(args[0]), write_side, self.r(args[1]), ep));
                        self.fs_result(&d, &r, &ep);
                    }
                    NetK::ReadDeadline | NetK::WriteDeadline => {
                        let write_side = if matches!(kind, NetK::WriteDeadline) { 1 } else { 0 };
                        self.decl("declare i64 @ksn_rt_sys_net_deadline(i32, i32, i64, ptr)".to_string());
                        let ep = self.fs_err_slot();
                        let r = self.t(format!("call i64 @ksn_rt_sys_net_deadline(i32 {}, i32 {}, i64 {}, ptr {})", self.r(args[0]), write_side, self.r(args[1]), ep));
                        self.fs_result(&d, &r, &ep);
                    }
                    NetK::ClearDeadlines => {
                        self.decl("declare i64 @ksn_rt_sys_net_clear_deadlines(i32, ptr)".to_string());
                        let ep = self.fs_err_slot();
                        let r = self.t(format!("call i64 @ksn_rt_sys_net_clear_deadlines(i32 {}, ptr {})", self.r(args[0]), ep));
                        self.fs_result(&d, &r, &ep);
                    }
                    NetK::SetNoDelay => {
                        let on = self.t(format!("zext i1 {} to i32", self.r(args[1])));
                        self.decl("declare i64 @ksn_rt_sys_net_nodelay(i32, i32, ptr)".to_string());
                        let ep = self.fs_err_slot();
                        let r = self.t(format!("call i64 @ksn_rt_sys_net_nodelay(i32 {}, i32 {}, ptr {})", self.r(args[0]), on, ep));
                        self.fs_result(&d, &r, &ep);
                    }
                    NetK::AcceptTimeout => {
                        self.decl("declare i64 @ksn_rt_sys_accept_timeout(i32, i64, ptr)".to_string());
                        let ep = self.fs_err_slot();
                        let r = self.t(format!("call i64 @ksn_rt_sys_accept_timeout(i32 {}, i64 {}, ptr {})", self.r(args[0]), self.r(args[1]), ep));
                        self.net_err_result(&d, &r, &ep);
                    }
                    NetK::ConnectTimeout => {
                        let (p, n) = self.fat_parts(args[0])?;
                        self.decl("declare i64 @ksn_rt_sys_connect_timeout(ptr, i64, i64, ptr)".to_string());
                        let ep = self.fs_err_slot();
                        let r = self.t(format!("call i64 @ksn_rt_sys_connect_timeout(ptr {}, i64 {}, i64 {}, ptr {})", p, n, self.r(args[1]), ep));
                        self.net_err_result(&d, &r, &ep);
                    }
                    NetK::Shutdown => {
                        self.decl("declare i64 @ksn_rt_sys_net_shutdown(i32, i32, ptr)".to_string());
                        let ep = self.fs_err_slot();
                        let r = self.t(format!("call i64 @ksn_rt_sys_net_shutdown(i32 {}, i32 {}, ptr {})", self.r(args[0]), self.r(args[1]), ep));
                        self.fs_result(&d, &r, &ep);
                    }
                    NetK::PeerAddr | NetK::LocalAddr => {
                        let peer = if matches!(kind, NetK::PeerAddr) { 1 } else { 0 };
                        self.decl("declare i64 @ksn_rt_sys_net_addr(i32, i32, ptr, i64, ptr)".to_string());
                        let ep = self.fs_err_slot();
                        let r = self.t(format!("call i64 @ksn_rt_sys_net_addr(i32 {}, i32 {}, ptr {}, i64 {}, ptr {})", self.r(args[0]), peer, self.r(args[1]), self.r(args[2]), ep));
                        self.fs_result(&d, &r, &ep);
                    }
                    NetK::Resolve => {
                        let (hp, hn) = self.fat_parts(args[0])?;
                        let port = self.t(format!("zext i16 {} to i32", self.r(args[1])));
                        self.decl("declare i64 @ksn_rt_sys_net_resolve(ptr, i64, i32, ptr, i64, ptr)".to_string());
                        let ep = self.fs_err_slot();
                        let r = self.t(format!(
                            "call i64 @ksn_rt_sys_net_resolve(ptr {}, i64 {}, i32 {}, ptr {}, i64 {}, ptr {})",
                            hp,
                            hn,
                            port,
                            self.r(args[2]),
                            self.r(args[3]),
                            ep
                        ));
                        self.fs_result(&d, &r, &ep);
                    }
                    NetK::UdpBind => {
                        let (p, n) = self.fat_parts(args[0])?;
                        self.decl("declare i64 @ksn_rt_sys_net_udp_bind(ptr, i64, ptr)".to_string());
                        let ep = self.fs_err_slot();
                        let r = self.t(format!("call i64 @ksn_rt_sys_net_udp_bind(ptr {}, i64 {}, ptr {})", p, n, ep));
                        self.net_err_result(&d, &r, &ep);
                    }
                    NetK::UdpSendTo => {
                        let (ap, an) = self.fat_parts(args[3])?;
                        self.decl("declare i64 @ksn_rt_sys_net_sendto(i32, ptr, i64, ptr, i64, ptr)".to_string());
                        let ep = self.fs_err_slot();
                        let r = self.t(format!(
                            "call i64 @ksn_rt_sys_net_sendto(i32 {}, ptr {}, i64 {}, ptr {}, i64 {}, ptr {})",
                            self.r(args[0]),
                            self.r(args[1]),
                            self.r(args[2]),
                            ap,
                            an,
                            ep
                        ));
                        self.fs_result(&d, &r, &ep);
                    }
                    NetK::UdpRecvFrom => {
                        self.decl("declare i64 @ksn_rt_sys_net_recvfrom(i32, ptr, i64, ptr, i64, ptr)".to_string());
                        let ep = self.fs_err_slot();
                        let r = self.t(format!(
                            "call i64 @ksn_rt_sys_net_recvfrom(i32 {}, ptr {}, i64 {}, ptr {}, i64 {}, ptr {})",
                            self.r(args[0]),
                            self.r(args[1]),
                            self.r(args[2]),
                            self.r(args[3]),
                            self.r(args[4]),
                            ep
                        ));
                        self.fs_result(&d, &r, &ep);
                    }
                }
            }
            Op::EnvArgCount => {
                self.cx.env = true;
                let r = self.t(format!("call i64 @ksn_rt_sys_arg_count()"));
                // 結果は `usize` (wasm32 は i32 なので狭める。x86_64 / aarch64 は従来どおり i64)。
                let r = if size_ty() == "i64" { r } else { self.t(format!("trunc i64 {} to {}", r, size_ty())) };
                self.mov(&d, &size_ty(), &r);
            }
            Op::EnvArg { index } => {
                self.cx.env = true;
                let iv = self.len64(*index);
                let lenp = self.t("alloca i64, align 8".to_string());
                self.line(format!("store i64 0, ptr {}, align 8", lenp));
                let ptr = self.t(format!("call i64 @ksn_rt_sys_arg(i64 {}, ptr {})", iv, lenp));
                let len = self.t(format!("load i64, ptr {}, align 8", lenp));
                let isnull = self.t(format!("icmp eq i64 {}, 0", ptr));
                let len128 = self.t(format!("zext i64 {} to i128", len));
                let fat_hi = self.t(format!("shl i128 {}, 64", len128));
                let ptr128 = self.t(format!("zext i64 {} to i128", ptr));
                let fat = self.t(format!("or i128 {}, {}", fat_hi, ptr128));
                let tag = self.t("shl i129 1, 128".to_string());
                let fat129 = self.t(format!("zext i128 {} to i129", fat));
                let some = self.t(format!("or i129 {}, {}", tag, fat129));
                let result = self.t(format!("select i1 {}, i129 0, i129 {}", isnull, some));
                self.mov(&d, "i129", &result);
            }
            Op::TimeNowNs => {
                self.cx.time = true;
                let r = self.t(format!("call i64 @ksn_rt_sys_wall_ns()"));
                self.mov(&d, "i64", &r);
            }
            Op::EnvVar { name } => {
                self.cx.env = true;
                let av = self.r(*name);
                let pi = self.extract(&av, 128, 0, 64);
                let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                let ni = self.extract(&av, 128, 8, 64);
                let lenp = self.t("alloca i64, align 8".to_string());
                self.line(format!("store i64 0, ptr {}, align 8", lenp));
                let ptr = self.t(format!("call i64 @ksn_rt_sys_var(ptr {}, i64 {}, ptr {})", p, ni, lenp));
                let len = self.t(format!("load i64, ptr {}, align 8", lenp));
                let isnull = self.t(format!("icmp eq i64 {}, 0", ptr));
                let len128 = self.t(format!("zext i64 {} to i128", len));
                let fat_hi = self.t(format!("shl i128 {}, 64", len128));
                let ptr128 = self.t(format!("zext i64 {} to i128", ptr));
                let fat = self.t(format!("or i128 {}, {}", fat_hi, ptr128));
                let tag = self.t("shl i129 1, 128".to_string());
                let fat129 = self.t(format!("zext i128 {} to i129", fat));
                let some = self.t(format!("or i129 {}, {}", tag, fat129));
                let result = self.t(format!("select i1 {}, i129 0, i129 {}", isnull, some));
                self.mov(&d, "i129", &result);
            }
            Op::Proc { f, args } => self.proc_op(&d, i, *f, args)?,
            Op::FsCreate { path } | Op::FsOpen { path } => {
                self.cx.fs = true;
                let is_create = matches!(i.op, Op::FsCreate { .. });
                let Ty::Agg(_) = self.ty(*path) else { return Err("fs.create / fs.open のパスが fat でない".into()) };
                let av = self.r(*path);
                let pi = self.extract(&av, 128, 0, 64);
                let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                let ni = self.extract(&av, 128, 8, 64);
                let ep = self.fs_err_slot();
                let create = if is_create { "1" } else { "0" };
                let r = self.t(format!("call i64 @ksn_rt_sys_open(ptr {}, i64 {}, i32 {}, ptr {})", p, ni, create, ep));
                self.fs_result(&d, &r, &ep);
            }
            Op::FsExists { path } => {
                self.cx.fs = true;
                let Ty::Agg(_) = self.ty(*path) else { return Err("fs.exists のパスが fat でない".into()) };
                let av = self.r(*path);
                let pi = self.extract(&av, 128, 0, 64);
                let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                let ni = self.extract(&av, 128, 8, 64);
                let r = self.t(format!("call i64 @ksn_rt_sys_exists(ptr {}, i64 {})", p, ni));
                let b = self.t(format!("icmp ne i64 {}, 0", r));
                self.mov(&d, "i1", &b);
            }
            Op::FsRemove { path } => {
                self.cx.fs = true;
                let Ty::Agg(_) = self.ty(*path) else { return Err("fs.remove_file のパスが fat でない".into()) };
                let av = self.r(*path);
                let pi = self.extract(&av, 128, 0, 64);
                let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                let ni = self.extract(&av, 128, 8, 64);
                let ep = self.fs_err_slot();
                let r = self.t(format!("call i64 @ksn_rt_sys_remove(ptr {}, i64 {}, ptr {})", p, ni, ep));
                self.fs_result(&d, &r, &ep);
            }
            Op::FsSys { kind, args } => {
                self.cx.fs = true;
                let mut pl: Vec<(String, String)> = Vec::new();
                for &ix in kind.path_slots() {
                    let a = &args[ix];
                    let Ty::Agg(_) = self.ty(*a) else { return Err("fs のパスが fat でない".into()) };
                    let av = self.r(*a);
                    let pi = self.extract(&av, 128, 0, 64);
                    let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                    let ni = self.extract(&av, 128, 8, 64);
                    pl.push((p, ni));
                }
                let ep = self.fs_err_slot();
                let r = match kind {
                    FsK::Rename => self.t(format!("call i64 @ksn_rt_sys_rename(ptr {}, i64 {}, ptr {}, i64 {}, ptr {})", pl[0].0, pl[0].1, pl[1].0, pl[1].1, ep)),
                    FsK::CreateDir => self.t(format!("call i64 @ksn_rt_sys_mkdir(ptr {}, i64 {}, ptr {})", pl[0].0, pl[0].1, ep)),
                    FsK::RemoveDir => self.t(format!("call i64 @ksn_rt_sys_rmdir(ptr {}, i64 {}, ptr {})", pl[0].0, pl[0].1, ep)),
                    FsK::OpenAppend => self.t(format!("call i64 @ksn_rt_sys_open(ptr {}, i64 {}, i32 2, ptr {})", pl[0].0, pl[0].1, ep)),
                    FsK::Stat => self.t(format!("call i64 @ksn_rt_sys_stat(ptr {}, i64 {}, i32 {}, ptr {})", pl[0].0, pl[0].1, self.r(args[1]), ep)),
                    FsK::OpenDir => self.t(format!("call i64 @ksn_rt_sys_opendir(ptr {}, i64 {}, ptr {})", pl[0].0, pl[0].1, ep)),
                    FsK::OpenBeneath => {
                        let dirfd = self.r(args[0]);
                        self.t(format!("call i64 @ksn_rt_sys_openbeneath(i32 {}, ptr {}, i64 {}, ptr {})", dirfd, pl[0].0, pl[0].1, ep))
                    }
                    FsK::FStat => {
                        let fd = self.r(args[0]);
                        self.t(format!("call i64 @ksn_rt_sys_fstat(i32 {}, i32 {}, ptr {})", fd, self.r(args[1]), ep))
                    }
                    FsK::ReadDir => {
                        let cap = self.len64(args[2]);
                        self.t(format!("call i64 @ksn_rt_sys_readdir(ptr {}, i64 {}, ptr {}, i64 {}, ptr {})", pl[0].0, pl[0].1, self.r(args[1]), cap, ep))
                    }
                };
                self.fs_result(&d, &r, &ep);
            }
            Op::FsClose { fd } => {
                // 木走査 (clif と同じ) と同じく、閉じる失敗は報告しない。
                self.cx.fs = true;
                let r = self.t(format!("call i64 @ksn_rt_sys_close(i32 {})", self.r(*fd)));
                let ep = self.fs_err_slot();
                self.fs_result(&d, &r, &ep);
            }
            Op::FsFlush { .. } => {
                // ファイルは内部でバッファしないので、flush は常に成功 (clif と同じ)。
                self.cx.fs = true;
                self.pack(&d, 64, "false", "0");
            }
            Op::FsSync { fd } => {
                self.cx.fs = true;
                let ep = self.fs_err_slot();
                let r = self.t(format!("call i64 @ksn_rt_sys_sync(i32 {}, ptr {})", self.r(*fd), ep));
                self.fs_result(&d, &r, &ep);
            }
            Op::FsRead { fd, buf, len } => {
                self.cx.fs = true;
                let ep = self.fs_err_slot();
                let n = self.len64(*len);
                let r = self.t(format!("call i64 @ksn_rt_sys_read(i32 {}, ptr {}, i64 {}, ptr {})", self.r(*fd), self.r(*buf), n, ep));
                self.fs_result(&d, &r, &ep);
            }
            Op::FsWrite { fd, buf, len } => {
                self.cx.fs = true;
                let ep = self.fs_err_slot();
                let n = self.len64(*len);
                let r = self.t(format!("call i64 @ksn_rt_sys_write(i32 {}, ptr {}, i64 {}, ptr {})", self.r(*fd), self.r(*buf), n, ep));
                self.fs_result(&d, &r, &ep);
            }
            // OS の乱数 (sysrt.c の `ksn_rt_sys_random`)。失敗は ErrorKind と errno の受け口を `fs_result` が Err に詰める。
            Op::RandFill { buf, len, .. } => {
                self.decl("declare i64 @ksn_rt_sys_random(ptr, i64, ptr)".to_string());
                let ep = self.fs_err_slot();
                let n = self.len64(*len);
                let r = self.t(format!("call i64 @ksn_rt_sys_random(ptr {}, i64 {}, ptr {})", self.r(*buf), n, ep));
                self.fs_result(&d, &r, &ep);
            }
            Op::ParseInt { ty, s, radix, strict } => {
                self.cx.parse_int = true;
                self.cx.decl(format!("declare i32 @ksn_parse_int(ptr, {}, i32, i32, i32, i32, ptr, ptr)", size_ty()));
                let av = self.r(*s);
                let pi = self.extract(&av, 128, 0, 64);
                let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                let n = self.extract(&av, 128, 8, 64);
                let n = self.lane_sz(&n);
                let rv = self.r(*radix);
                let bits = ty.bits();
                let iss = if ty.signed() { 1 } else { 0 };
                let strict_v = if *strict { 1 } else { 0 };
                let lo_ptr = self.t("alloca i64".to_string());
                let hi_ptr = self.t("alloca i64".to_string());
                let ret = self.t(format!(
                    "call i32 @ksn_parse_int(ptr {}, {} {}, i32 {}, i32 {}, i32 {}, i32 {}, ptr {}, ptr {})",
                    p, size_ty(), n, rv, bits, iss, strict_v, lo_ptr, hi_ptr
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
                self.cx.decl(write_decl());
                let pi = self.extract(&fat, 128, 0, 64);
                let p = self.t(format!("inttoptr i64 {} to ptr", pi));
                let n = self.extract(&fat, 128, 8, 64);
                let n = self.lane_sz(&n);
                self.line(format!("call void @ksn.eprint(ptr {}, {} {})", p, size_ty(), n));
            }
            Op::HeapAlloc(n) => {
                let nv = self.r(*n);
                let sz = size_ty();
                let z = self.t(format!("icmp eq {} {}, 0", sz, nv));
                let m = self.t(format!("select i1 {}, {} 1, {} {}", z, sz, sz, nv));
                self.line(format!("{} = call ptr @malloc({} {})", d, sz, m));
            }
            Op::HeapRealloc { p, size } => {
                let (pv, nv) = (self.r(*p), self.r(*size));
                let sz = size_ty();
                let z = self.t(format!("icmp eq {} {}, 0", sz, nv));
                let m = self.t(format!("select i1 {}, {} 1, {} {}", z, sz, sz, nv));
                self.line(format!("{} = call ptr @realloc(ptr {}, {} {})", d, pv, sz, m));
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

    /// 構造体を引数・戻り値に持つ関数を C から呼ぶための入口 (C の呼び出し規約 → 内部の形)。thunk の名前を返す。
    fn c_thunk(&mut self, f: &Func) -> R<String> {
        use crate::kir_cabi as cabi;
        let aggs = self.aggs;
        let ptys: Vec<Ty> = f.params().iter().map(|(_, t)| *t).collect();
        let sg = cabi::sig(self.cx.arch, aggs, &ptys, f.ret).map_err(|e| format!("コールバック {}: {}", f.key, e))?;
        let name = gname("cth.", &f.key);
        let mut ps: Vec<String> = vec![];
        let mut em = cabi::Em::new("t");
        let rty = match &sg.ret {
            Some(ra) => {
                if let cabi::Abi::Mem { size, align } = ra {
                    ps.push(format!("ptr sret([{} x i8]) align {} %sret", size, align));
                }
                cabi::ret_ty(ra)
            }
            None if f.ret == Ty::Unit => "void".to_string(),
            None => lty(aggs, f.ret),
        };
        let mut iargs = vec![];
        for (k, (t, a)) in ptys.iter().zip(sg.args.iter()).enumerate() {
            let pn = format!("%p{}", k);
            match (a, t) {
                (Some(abi), Ty::Agg(ai)) => {
                    ps.extend(cabi::param_decls(abi, &pn));
                    let v = cabi::value_from_params(&mut em, abi, agg_bits(aggs, *ai), &pn);
                    iargs.push(format!("{} {}", lty(aggs, *t), v));
                }
                _ => {
                    ps.push(format!("{} {}", lty(aggs, *t), pn));
                    iargs.push(format!("{} {}", lty(aggs, *t), pn));
                }
            }
        }
        let r = em.t(format!("call {} {}({})", lty(aggs, f.ret), fname(&f.key), iargs.join(", ")));
        match (&sg.ret, f.ret) {
            (Some(ra), Ty::Agg(ai)) => cabi::emit_ret(&mut em, ra, agg_bits(aggs, ai), &r),
            _ if f.ret == Ty::Unit => em.line("ret void".into()),
            _ => em.line(format!("ret {} {}", rty, r)),
        }
        let text = format!("define {} {}({}) {{\nentry:\n{}}}\n", rty, name, ps.join(", "), em.body);
        self.cx.decl(text);
        Ok(name)
    }

    /// 構造体を値で受け渡す `extern` の呼び出し (C の呼び出し規約、`kir_cabi`)。
    fn c_call(&mut self, d: &str, i: &Inst, callee: &str, args: &[VId]) -> R<()> {
        use crate::kir_cabi as cabi;
        let aggs = self.aggs;
        let ptys: Vec<Ty> = args.iter().map(|v| self.ty(*v)).collect();
        let sg = cabi::sig(self.cx.arch, aggs, &ptys, i.ty).map_err(|e| format!("extern {}: {}", callee, e))?;
        self.n += 1;
        let mut em = cabi::Em::new(&format!("c{}_", self.n));
        let mut cargs: Vec<String> = vec![];
        let mut rbuf = None;
        if let Some(ra) = &sg.ret {
            if let Some((a, b)) = cabi::sret_arg(&mut em, ra) {
                cargs.push(a);
                rbuf = Some(b);
            }
        }
        for ((v, t), a) in args.iter().zip(ptys.iter()).zip(sg.args.iter()) {
            match (a, t) {
                (Some(abi), Ty::Agg(ai)) => {
                    let r = self.r(*v);
                    cargs.extend(cabi::call_args(&mut em, abi, agg_bits(aggs, *ai), &r));
                }
                _ => cargs.push(format!("{} {}", lty(aggs, *t), self.r(*v))),
            }
        }
        let rt = match &sg.ret { Some(ra) => cabi::ret_ty(ra), None => self.lt(i.ty) };
        let sym = self.extern_links[callee].clone();
        let target = if self.extern_dl.contains_key(callee) {
            em.t(format!("call ptr @ksn_ext_resolve(ptr @ksn.ext.lib.{0}, ptr @ksn.ext.sym.{0}, ptr @ksn.ext.slot.{0})", sym))
        } else {
            format!("@{}", sym)
        };
        let save = if em.used_stack {
            self.cx.decl("declare void @llvm.stackrestore.p0(ptr)".to_string());
            Some(self.tmp())
        } else {
            None
        };
        if let Some(sv) = &save {
            self.line(format!("{} = call ptr @llvm.stacksave.p0()", sv));
        }
        self.body.push_str(&em.body);
        let mut em2 = cabi::Em::new(&format!("c{}r_", self.n));
        match (&sg.ret, i.ty) {
            (Some(ra), Ty::Agg(ai)) => {
                let bits = agg_bits(aggs, ai);
                if rt == "void" {
                    self.line(format!("call void {}({})", target, cargs.join(", ")));
                    let val = cabi::ret_value(&mut em2, ra, bits, "", rbuf.as_deref());
                    self.body.push_str(&em2.body);
                    self.line(format!("{} = add i{} {}, 0", d, bits, val));
                } else {
                    let r = self.t(format!("call {} {}({})", rt, target, cargs.join(", ")));
                    let val = cabi::ret_value(&mut em2, ra, bits, &r, rbuf.as_deref());
                    self.body.push_str(&em2.body);
                    self.line(format!("{} = add i{} {}, 0", d, bits, val));
                }
            }
            _ if i.ty == Ty::Unit => {
                self.line(format!("call void {}({})", target, cargs.join(", ")));
                self.line(format!("{} = add i8 0, 0", d));
            }
            _ => self.line(format!("{} = call {} {}({})", d, rt, target, cargs.join(", "))),
        }
        if let Some(sv) = save {
            self.line(format!("call void @llvm.stackrestore.p0(ptr {})", sv));
        }
        Ok(())
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
            let v = self.canon_f(t, v);
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
        // Kasane の基本メソッドの単相インスタンス (下の先頭と戻りで深さの記録を省く)。
        let bare = kasane_base_inst(&f.key);
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
                    if !bare {
                        self.line("store i64 %depth, ptr @ksn.depth".into());
                    }
                    self.line(format!("ret {} {}", t, rv));
                }
                Term::Unreachable => self.line("unreachable".into()),
            }
            texts[k] = std::mem::take(&mut self.body);
        }
        // 先頭 (静的な確保と深さの検査) と本体。
        let params: Vec<String> = f.params().iter().enumerate().map(|(j, (_, t))| format!("{} %arg{}", lty(self.aggs, *t), j)).collect();
        // `null-pointer-is-valid`: `core.ptr.read(null)` などの null 参照を、最適化で消える未定義動作にせず、実際の fault (SIGSEGV) にする。
        // Kasane で書いた基本メソッド (`alloc/vec.ksnl` / `alloc/string.ksnl` の単相インスタンス) は、組み込みの下ろしと同じく呼び出しを残さない。
        // 再帰せず、呼び先も有界なので、呼び出しの深さの記録 (`ksn.depth`) を省いて、LLVM が展開できるようにする
        // (組み込みは呼び出しなしで展開されていたので、深さの記録の分だけ遅くなるのを防ぐ。深さの上限の検査は呼び出し側が行う)。
        let inline = if bare { "alwaysinline " } else { "" };
        // wasm32 で公開する関数は外部リンケージの `define` にして、`wasm-export-name` で export の名前を決める (WASM ABI)。
        let (head, exp) = match wasm_export_name(&f.key) {
            Some(n) => ("define", format!(" \"wasm-export-name\"=\"{}\"", n)),
            None => ("define internal", String::new()),
        };
        let mut s = format!("{} {} {}({}){} {}null_pointer_is_valid {{\nentry:\n", head, self.lt(f.ret), fname(&f.key), params.join(", "), exp, inline);
        s.push_str(&hoisted);
        if bare {
            // `b0` の phi は `go` を前の辺として参照するので、空の `go` を残す (最適化で消える)。
            s.push_str("  br label %go\ngo:\n  br label %b0\n");
        } else {
            // 深さの上限 (`ksn.dmax`、スレッドごと) とスタックの残り (`ksn.floor` より低ければ尽きる前に止める。wasm32 は深さだけ)。
            let check = if crate::ptrwidth::bits() == 32 {
                "  %depth = load i64, ptr @ksn.depth\n  %dmax = load i64, ptr @ksn.dmax\n  %over = icmp uge i64 %depth, %dmax\n".to_string()
            } else {
                "  %depth = load i64, ptr @ksn.depth\n  %dmax = load i64, ptr @ksn.dmax\n  %dov = icmp uge i64 %depth, %dmax\n  %spp = call ptr @llvm.stacksave.p0()\n  %sp = ptrtoint ptr %spp to i64\n  %fl = load i64, ptr @ksn.floor\n  %low = icmp ult i64 %sp, %fl\n  %over = or i1 %dov, %low\n".to_string()
            };
            s.push_str(&format!(
                "{}  br i1 %over, label %so, label %go\nso:\n  call void @ksn.overflow()\n  unreachable\ngo:\n  %depth1 = add i64 %depth, 1\n  store i64 %depth1, ptr @ksn.depth\n  br label %b0\n",
                check
            ));
        }
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
