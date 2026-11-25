//! 複数スレッドの実行器 (`std.task.block_on_threads` / `std.task.spawn`、設計_多重化したネット.md 9〜17 章) の試験。
//! - 検査: 枠の `Send` (E2074)、取り込み・結果の `Send` (E2040)。
//! - 木走査は `n` を無視した決定的な協調実行で、結果がネイティブと同じ。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;

const SUM: &str = include_str!("data/net_async_mt_sum.ksnl");

#[test]
fn interp_sum_of_spawned_tasks() {
    let t = Tmp::new("net_async_mt_sum_interp", SUM);
    let o = run(&["run"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert_eq!(text(&o.stdout).trim(), "sum ok", "{}", out);
}

#[test]
fn check_accepts_send_frames() {
    let t = Tmp::new("net_async_mt_sum_check", SUM);
    let o = run(&["check"], &t.0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout) + &text(&o.stderr));
}

#[test]
fn generic_with_send_bound_is_accepted_across_suspension() {
    let src = SUM.replace(
        "fn t.leaf(i: usize) -> usize uses [suspend] panics never\n    std.task.yield_now()\n    i\nend t.leaf",
        "fn t.keep[T: core.marker.Send](x: T) -> T uses [suspend] panics never\n    std.task.yield_now()\n    x\nend t.keep\n\nfn t.leaf(i: usize) -> usize uses [suspend] panics never\n    t.keep(i)\nend t.leaf",
    );
    let t = Tmp::new("net_async_mt_keep", &src);
    let o = run(&["run"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
}

const STRESS: &str = include_str!("data/net_async_mt_stress.ksnl");
const ECHO: &str = include_str!("data/net_async_mt_echo.ksnl");

/// `batches` 個のタスクが、それぞれ `per` 個の小さなタスクを作る。合計は 1..=N の和 (N = batches * per)。
fn stress_src(batches: usize, per: usize) -> String {
    let n = (batches * per) as u64;
    STRESS
        .replace("core.iter.range(0usize, 1000usize)", &format!("core.iter.range(0usize, {}usize)", batches))
        .replace("b.wrapping_mul(100usize)", &format!("b.wrapping_mul({}usize)", per))
        .replace("t.batch(base, 100usize)", &format!("t.batch(base, {}usize)", per))
        .replace("5000050000usize", &format!("{}usize", n * (n + 1) / 2))
}

fn echo_src(clients: usize) -> String {
    ECHO.replace("let clients = 256usize", &format!("let clients = {}usize", clients))
}

#[test]
fn interp_stress_small_sum_is_exact() {
    let t = Tmp::new("net_async_mt_stress_interp", &stress_src(20, 25));
    let o = run(&["run"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert_eq!(text(&o.stdout).trim(), "stress done", "{}", out);
}

#[test]
fn native_stress_100k_tasks_sum_is_exact() {
    let (t, bin, log) = build_native_strict("net_async_mt_stress_native", &stress_src(1000, 100), false);
    let Some(bin) = bin else {
        if cfg!(target_os = "linux") {
            panic!("ネイティブが作れない: {}", log);
        }
        return;
    };
    let o = std::process::Command::new(&bin).current_dir(&t.0).output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert_eq!(text(&o.stdout).trim(), "stress done", "{}", out);
}

#[test]
fn interp_echo_256_threads() {
    let t = Tmp::new("net_async_mt_echo_interp", &echo_src(256));
    let o = run(&["run"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert_eq!(text(&o.stdout).trim(), "async echo done", "{}", out);
}

#[test]
fn native_echo_256_threads_matches_interp() {
    let (t, bin, log) = build_native_strict("net_async_mt_echo_native", &echo_src(256), true);
    let Some(bin) = bin else {
        if cfg!(target_os = "linux") {
            panic!("ネイティブが作れない: {}", log);
        }
        return;
    };
    let o = std::process::Command::new(&bin).current_dir(&t.0).output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    let i = Tmp::new("net_async_mt_echo_cmp", &echo_src(256));
    let oi = run(&["run"], &i.0);
    assert_eq!(text(&oi.stdout), text(&o.stdout));
}

fn expect_code(name: &str, src: &str, code: &str) {
    let t = Tmp::new_strict(name, src);
    let o = run(&["check"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{}", out);
    assert!(out.contains(code), "want {}: {}", code, out);
}

#[test]
fn non_send_frame_capture_and_result_are_rejected() {
    let d = |n: &str| std::fs::read_to_string(root().join("stage0/tests/data/unsound").join(n)).unwrap();
    expect_code("net_async_mt_rej_frame", &d("task_threads_frame_nonsend_generic.ksnl"), "E2074");
    expect_code("net_async_mt_rej_cap", &d("task_threads_capture_nonsend.ksnl"), "E2040");
    expect_code("net_async_mt_rej_res", &d("task_threads_result_nonsend.ksnl"), "E2040");
    expect_code("net_async_mt_rej_borrow", &d("task_threads_spawn_borrow_local.ksnl"), "E2037");
}

// ------------------------------------------------------------ 2b: ネイティブの実スレッドの実行器 (n 本のワーカー、注入列、完了の印、眠り)

fn with_workers(src: &str, n: usize) -> String {
    let out = src.replace("std.alloc.SYSTEM,\n        4usize,", &format!("std.alloc.SYSTEM,\n        {}usize,", n));
    assert_ne!(out, src, "n の置き換えが効いていない");
    out
}

/// ネイティブ (release) を作って走らせる。作れなければ Linux では失敗。Linux 以外は None。
fn run_native(name: &str, src: &str) -> Option<(String, Option<i32>)> {
    let (t, bin, log) = build_native_strict(name, src, true);
    let Some(bin) = bin else {
        if cfg!(target_os = "linux") {
            panic!("ネイティブが作れない: {}", log);
        }
        return None;
    };
    let o = std::process::Command::new(&bin).current_dir(&t.0).output().unwrap();
    Some((text(&o.stdout) + &text(&o.stderr), o.status.code()))
}

#[test]
fn native_echo_16_workers_exact_bytes() {
    if let Some((out, rc)) = run_native("net_async_mt_echo16", &with_workers(&echo_src(256), 16)) {
        assert_eq!(rc, Some(0), "{}", out);
        assert_eq!(out.trim(), "async echo done", "{}", out);
    }
}

#[test]
fn native_echo_1_worker_still_works() {
    if let Some((out, rc)) = run_native("net_async_mt_echo1", &with_workers(&echo_src(64), 1)) {
        assert_eq!(rc, Some(0), "{}", out);
        assert_eq!(out.trim(), "async echo done", "{}", out);
    }
}

const PANIC: &str = include_str!("data/net_async_mt_panic.ksnl");
const CANCEL: &str = include_str!("data/net_async_mt_cancel.ksnl");

/// ワーカーのタスクの panic は、メッセージを出して終了コード 101。
#[test]
fn native_panic_in_a_worker_task_exits_101_with_the_message() {
    if let Some((out, rc)) = run_native("net_async_mt_panic", PANIC) {
        assert_eq!(rc, Some(101), "{}", out);
        assert!(out.contains("worker boom"), "{}", out);
        assert!(!out.contains("unreachable"), "{}", out);
    }
}

/// 走っているタスクと始まる前のタスクの取り消し。取り消された走行中のタスクで全体が止まらない。
#[test]
fn native_cancel_running_and_unstarted_tasks() {
    if let Some((out, rc)) = run_native("net_async_mt_cancel", CANCEL) {
        assert_eq!(rc, Some(0), "{}", out);
        assert_eq!(out.trim(), "cancel done\ncount ok", "{}", out);
    }
}

/// clang が `-fsanitize=thread` の実行ファイルを作って走らせられるか (`KSN_NO_TSAN=1` で無効)。
fn tsan_available() -> bool {
    use std::sync::OnceLock;
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        if cfg!(windows) || !clang_available() || std::env::var("KSN_NO_TSAN").is_ok() {
            return false;
        }
        let d = std::env::temp_dir().join(format!("ksn_tsan_probe_amt_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let c = d.join("a.c");
        let o = d.join("a.out");
        std::fs::write(&c, "int main(void){return 0;}\n").unwrap();
        let ok = std::process::Command::new("clang").args(["-fsanitize=thread"]).arg(&c).arg("-o").arg(&o).output().map(|r| r.status.success()).unwrap_or(false)
            && std::process::Command::new(&o).output().map(|r| r.status.success()).unwrap_or(false);
        let _ = std::fs::remove_dir_all(&d);
        ok
    })
}

/// ThreadSanitizer 付きのネイティブで走らせ、競合の警告が無く、標準出力が `want` と一致する。
fn tsan_run(name: &str, src: &str, want: &str) {
    if !tsan_available() || !cfg!(target_os = "linux") {
        return;
    }
    let t = Tmp::new(name, src);
    let cache = std::env::temp_dir().join(format!("ksn_cache_tsan_amt_{}_{}", std::process::id(), name));
    let o = std::process::Command::new(exe())
        .args(["build", "--backend", "llvm"])
        .current_dir(&t.0)
        .env("KSN_CACHE_DIR", &cache)
        .env("KSN_CLANG_ARGS", "-fsanitize=thread -g -O1")
        .output()
        .unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    let bin = t.0.join("target").join("debug").join("t");
    let kind = std::fs::read_to_string(t.0.join("target").join("debug").join(".ksn-artifact")).unwrap_or_default();
    assert!(bin.exists() && kind.trim() == "native", "TSan 付きのネイティブが作れない: {}", out);
    let r = std::process::Command::new(&bin).current_dir(&t.0).env("TSAN_OPTIONS", "halt_on_error=0").output().unwrap();
    let _ = std::fs::remove_dir_all(&cache);
    let err = text(&r.stderr);
    assert!(!err.contains("WARNING: ThreadSanitizer"), "{}: TSan が競合を報告した\n{}", name, err);
    assert_eq!(r.status.code(), Some(0), "{}: {}", name, err);
    assert_eq!(text(&r.stdout).trim(), want, "{}", name);
}

#[test]
fn tsan_echo_4_and_16_workers_have_no_data_races() {
    tsan_run("net_async_mt_tsan_echo4", &echo_src(64), "async echo done");
    tsan_run("net_async_mt_tsan_echo16", &with_workers(&echo_src(64), 16), "async echo done");
}

#[test]
fn tsan_stress_spawned_tasks_have_no_data_races() {
    tsan_run("net_async_mt_tsan_stress", &stress_src(100, 100), "stress done");
}

#[test]
fn tsan_cancel_has_no_data_races() {
    tsan_run("net_async_mt_tsan_cancel", CANCEL, "cancel done\ncount ok");
}

/// 全員待ち (注入列が空・全ワーカーが眠り・IO の待ちが無い) の検出: 共有の核 (`sysrt.c` の `ksn_rt_sys_mt_*`) を C から直接試す。
/// 2 本のうち 1 本が眠っているところへ 2 本目が眠ろうとすると 3 (deadlock) が返り、起こす出来事 (stop) で眠っていた方が 2 で戻る。
#[test]
fn mt_core_detects_all_workers_blocked() {
    if !cfg!(target_os = "linux") || !clang_available() {
        return;
    }
    let d = std::env::temp_dir().join(format!("ksn_amt_core_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("sysrt.c"), include_str!("../src/sysrt.c")).unwrap();
    std::fs::write(
        d.join("h.c"),
        r#"#include "sysrt.c"
int ksn_rt_argc; char **ksn_rt_argv;
static i64 seen;
static uint32_t r1 = 99;
static void *w(void *a) { (void)a; r1 = ksn_rt_sys_mt_sleep(seen); return NULL; }
int main(void) {
    ksn_rt_sys_mt_begin(2);
    seen = ksn_rt_sys_mt_epoch();
    pthread_t t;
    pthread_create(&t, NULL, w, NULL);
    usleep(200000);
    uint32_t r2 = ksn_rt_sys_mt_sleep(seen);
    ksn_rt_sys_mt_stop();
    pthread_join(t, NULL);
    printf("%u %u\n", r2, r1);
    /* push が世代を進めるので、古い世代で眠ろうとしても眠らず 0 */
    ksn_rt_sys_mt_begin(2);
    i64 e = ksn_rt_sys_mt_epoch();
    ksn_rt_sys_mt_push(0, NULL);
    printf("%u\n", ksn_rt_sys_mt_sleep(e));
    return 0;
}
"#,
    )
    .unwrap();
    let c = std::process::Command::new("clang").args(["-w", "-O1", "-o"]).arg(d.join("h")).arg(d.join("h.c")).arg("-lpthread").output().unwrap();
    assert!(c.status.success(), "{}", text(&c.stderr));
    let o = std::process::Command::new(d.join("h")).output().unwrap();
    let _ = std::fs::remove_dir_all(&d);
    assert_eq!(text(&o.stdout), "3 2\n0\n", "{}", text(&o.stderr));
}

// ------------------------------------------------------------ sec-async-mt: 古い Task・入れ子・spawn_local の取り違え (木走査とネイティブで同じ panic)

fn both_panic(name: &str, src: &str, msg: &str) {
    let t = Tmp::new_strict(&format!("{}_i", name), src);
    let o = run(&["run"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(101), "{}", out);
    assert!(out.contains(msg), "want {}: {}", msg, out);
    if let Some((out, rc)) = run_native(&format!("{}_n", name), src) {
        assert_eq!(rc, Some(101), "{}", out);
        assert!(out.contains(msg), "want {}: {}", msg, out);
    }
}

#[test]
fn stale_task_join_across_block_on_threads_panics_everywhere() {
    both_panic("net_async_mt_stale", include_str!("data/net_async_mt_stale_task.ksnl"), "generation mismatch");
}

// ------------------------------------------------------------ K1: 別スレッドの実行器へ持ち込んだ Task (セッション番号はプロセス全体で一意)

fn k1_src(name: &str) -> String {
    std::fs::read_to_string(root().join("stage0/tests/data/k1_cross_thread").join(name)).unwrap()
}

/// K1a: 主スレッドの `block_on` が返した `Task<Vec<u64>>` を別スレッドの `block_on` で join すると、
/// 別のタスクの結果を Vec として読んていた (型の取り違え)。木走査とネイティブで同じ panic・終了コード。
#[test]
fn cross_thread_join_of_vec_task_panics_everywhere() {
    both_panic("k1_join_a", &k1_src("task_cross_thread_join_a.ksnl"), "join_task: task from a different block_on (generation mismatch)");
}

/// K1b: 逆向き (`Task<(u64, u64, u64)>` の取っ手で Vec の結果を読む)。ヒープの番地が数として漏れる形。
#[test]
fn cross_thread_join_of_tuple_task_panics_everywhere() {
    both_panic("k1_join_b", &k1_src("task_cross_thread_join_b.ksnl"), "join_task: task from a different block_on (generation mismatch)");
}

/// `block_on_threads` で作った取っ手を別スレッドの `block_on` で join する形 (複数スレッドの実行器の側)。
#[test]
fn cross_thread_join_of_mt_task_panics_everywhere() {
    both_panic("k1_mt_join", &k1_src("task_cross_thread_mt_join.ksnl"), "join_task: task from a different block_on (generation mismatch)");
}

#[test]
fn nested_block_on_threads_reports_already_running_everywhere() {
    both_panic("net_async_mt_nested", include_str!("data/net_async_mt_nested.ksnl"), "already running");
}

#[test]
fn spawn_local_inside_block_on_threads_panics_everywhere() {
    both_panic("net_async_mt_spl", include_str!("data/net_async_mt_spawn_local.ksnl"), "spawn_local: not allowed inside std.task.block_on_threads");
}

const SPAWN_JOIN_LOOP: &str = include_str!("data/net_async_mt_spawn_join_loop.ksnl");
const SEQ_SPAWN_JOIN: &str = include_str!("data/task_seq_spawn_join.ksnl");

/// R1: spawn → join の繰り返しが、全員待ちの判定の競合で偽の deadlock の panic にならない (4 本のワーカー、200 回)。
#[test]
fn native_spawn_join_loop_never_reports_false_deadlock() {
    let (t, bin, log) = build_native_strict("net_async_mt_spawn_join_loop", SPAWN_JOIN_LOOP, false);
    let Some(bin) = bin else {
        if cfg!(target_os = "linux") {
            panic!("ネイティブが作れない: {}", log);
        }
        return;
    };
    for k in 0..200 {
        let o = std::process::Command::new(&bin).current_dir(&t.0).output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "run {}: {}", k, out);
        assert_eq!(text(&o.stdout).trim(), "124750", "run {}: {}", k, out);
    }
}

/// P1: 順に spawn_local → join_task を 20 万回。空き枠を再利用し、完了ごとの全表走査をしないので、すぐ終わる。
#[test]
fn native_sequential_spawn_join_200k_is_linear() {
    let (t, bin, log) = build_native_strict("task_seq_spawn_join", SEQ_SPAWN_JOIN, false);
    let Some(bin) = bin else {
        if cfg!(target_os = "linux") {
            panic!("ネイティブが作れない: {}", log);
        }
        return;
    };
    let start = std::time::Instant::now();
    let o = std::process::Command::new(&bin).current_dir(&t.0).output().unwrap();
    let el = start.elapsed();
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert_eq!(text(&o.stdout).trim(), "19999900000", "{}", out);
    assert!(el.as_secs_f64() < 5.0, "遅すぎる (二乗の伸びに戻った?): {:?}", el);
}

#[test]
fn interp_sequential_spawn_join_small_matches() {
    let src = SEQ_SPAWN_JOIN.replace("200_000u64", "300u64");
    let t = Tmp::new("task_seq_spawn_join_interp", &src);
    let o = run(&["run"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{}", out);
    assert_eq!(text(&o.stdout).trim(), "44850", "{}", out);
}
