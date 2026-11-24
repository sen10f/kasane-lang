//! スレッド・チャネル・Mutex・TSan のテスト。

#![allow(dead_code, unused_imports)]

mod common;

#[path = "common/scratch.rs"]
mod scratch;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use common::*;

// ------------------------------------------------------------ スレッド・Mutex・チャネル・std.env・std.time (実スレッド)

/// `tests/data/threads/<name>.ksnl` を 1 つのパッケージとして取り込む。
fn threads_pkg(name: &str, tag: &str) -> Tmp {
    let src = fs::read_to_string(root().join("stage0").join("tests").join("data").join("threads").join(format!("{}.ksnl", name))).unwrap();
    Tmp::new(&format!("thr_{}_{}", tag, name), &src)
}

/// ビルドして (LLVM、下ろせなければ理由付きでバンドル) 実行ファイルを返す。
fn threads_build(t: &Tmp, release: bool) -> PathBuf {
    let cache = scratch::Scratch::at(format!("ksn_cache_thr_{}_{}", std::process::id(), t.0.file_name().unwrap().to_string_lossy()));
    let mut c = Command::new(exe());
    c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
    if release {
        c.args(["--release", "--allow-bundle"]);
    }
    let o = c.output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_native_or_reasoned_skip(&out);
    let kind = if release { "release" } else { "debug" };
    let p = t.0.join("target").join(kind).join(if cfg!(windows) { "t.exe" } else { "t" });
    let p = native_or_launcher(p, &out);
    let _ = fs::remove_dir_all(&cache);
    p
}

/// インタプリタと、ネイティブ (debug / release) の出力が同じで、終了コード 0 であること。
fn threads_same_output(name: &str) {
    if cfg!(windows) {
        return; // 実スレッドは POSIX のみ (Windows はビルド時にスキップ)
    }
    let t = threads_pkg(name, "same");
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    for release in [false, true] {
        let p = threads_build(&t, release);
        let r = Command::new(&p).output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), text(&interp.stdout), "native and interpreter outputs differ ({})", name);
    }
}

#[test]
fn threads_spawn_and_join_return_the_closure_result() {
    threads_same_output("spawn_join");
}

/// 4 つのスレッドが `static` の `Mutex` を 1 万回ずつ、長い臨界区間 (読む → 数えて待つ → 書く) で増やす。
/// 排他が働かないと更新が失われて最後の値が 4 万に届かない (ロックを無操作にすると必ず失敗する)。
#[test]
fn threads_mutex_gives_mutual_exclusion() {
    threads_same_output("mutex_counter");
}

#[test]
fn threads_channels_carry_values_and_strings_between_threads() {
    threads_same_output("channel");
}

#[test]
fn threads_a_panic_in_a_spawned_thread_aborts_the_process() {
    let t = threads_pkg("thread_panic", "panic");
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(101));
    assert!(text(&interp.stderr).contains("panic: boom in thread"), "{}", text(&interp.stderr));
    if cfg!(windows) {
        return;
    }
    let p = threads_build(&t, false);
    let r = Command::new(&p).output().unwrap();
    assert_eq!(r.status.code(), Some(101));
    assert!(text(&r.stderr).contains("panic: boom in thread"), "{}", text(&r.stderr));
    assert!(!text(&r.stdout).contains("not reached"));
}

/// 5.2: スタック溢れは `stack overflow` と終了コード 102 (メインスレッドでも、`spawn` したスレッドの中でも)。
/// インタプリタとネイティブ (debug / release) で、標準出力・標準エラー・終了コードが同じ。
#[test]
fn threads_stack_overflow_stops_the_process_with_102_in_main_and_in_a_spawned_thread() {
    for name in ["overflow_main", "overflow_thread"] {
        let t = threads_pkg(name, "so");
        let interp = run(&["run"], &t.0);
        assert_eq!(interp.status.code(), Some(102), "{}: {}", name, text(&interp.stderr));
        assert_eq!(text(&interp.stderr), "stack overflow\n");
        assert_eq!(text(&interp.stdout), "start\n");
        if cfg!(windows) {
            continue;
        }
        for release in [false, true] {
            let p = threads_build(&t, release);
            let r = Command::new(&p).output().unwrap();
            assert_eq!(r.status.code(), Some(102), "{} (release={}): {:?} {}", name, release, r.status, text(&r.stderr));
            assert_eq!(text(&r.stderr), "stack overflow\n");
            assert_eq!(text(&r.stdout), text(&interp.stdout));
        }
    }
}

#[test]
fn threads_env_arguments_variables_and_time_work_natively() {
    let t = threads_pkg("env_time", "env");
    let interp = Command::new(exe()).args(["run", "alpha", "b c"]).env("KSN_THREADS_TEST_VAR", "hello-env").current_dir(&t.0).output().unwrap();
    assert_eq!(interp.status.code(), Some(0), "{}", text(&interp.stderr));
    let want = "args=2\nalpha\nb c\nno more\nhello-env\nmissing is unset\ntime ok\n";
    assert_eq!(text(&interp.stdout), want);
    if cfg!(windows) {
        return;
    }
    for release in [false, true] {
        let p = threads_build(&t, release);
        let r = Command::new(&p).args(["alpha", "b c"]).env("KSN_THREADS_TEST_VAR", "hello-env").output().unwrap();
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), want);
    }
}

/// 走っているスレッドが、待っている側 (main) からの後の `send` を待つ往復の通信。ネイティブでは進むが、
/// 単一スレッドのインタプリタには割り当てられないので、止まらずに明確なメッセージで失敗する。
#[test]
fn threads_interpreter_fails_clearly_when_no_single_threaded_schedule_exists() {
    let t = threads_pkg("ping_pong", "pingpong");
    let interp = run(&["run"], &t.0);
    assert_eq!(interp.status.code(), Some(101));
    assert!(text(&interp.stderr).contains("deadlock"), "{}", text(&interp.stderr));
    if cfg!(windows) {
        return;
    }
    let p = threads_build(&t, false);
    let r = Command::new(&p).output().unwrap();
    // LLVM (KIR) の `spawn` は pthread の実スレッドなので、往復の通信はネイティブでは進んで終わる
    // (インタプリタの単一スレッドの割り当てには載らない)。
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
}

/// 線形値 (`TcpStream`) を `spawn` のクロージャへ move し、返す。`Receiver` を複製して複数のワーカーが 1 つのキューから
/// 接続を取り合い、`close` で残りを受け取る。容量 1 のチャネルを持ち回して共有の状態を守る。
#[test]
fn threads_linear_values_move_between_threads_and_through_channels() {
    threads_same_output("linear");
}

/// `Sender` / `Receiver` の `clone`: 複数の送信側が 1 つのチャネルに送り、受信側は最後の 1 つだけが残りを破棄する。
#[test]
fn threads_channel_ends_clone_and_share_one_queue() {
    threads_same_output("channel_clone");
}

/// チャネルの端を複製・`close`・破棄しても、漏れも二重解放もない。`malloc` / `free` を数える `LD_PRELOAD` で、
/// 周回数を 5 倍の 80 倍にしても「確保 - 解放」が変わらないことを調べる (スレッドを起こすたびに libc 内部の確保が 1〜2 件ずれるので、
/// 絶対値ではなく周回の数に比例して増えないこと = 1 周ごとの漏れがないことを見る。二重解放なら glibc が abort する)。
/// 1 周は、複製した `Sender` / `Receiver` で文字列を送り、最後の受信側が残りを破棄する経路と、線形な値の受信側を `close` する経路。
#[test]
fn native_channel_clone_and_close_free_every_allocation_exactly_once() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let cc = ["gcc", "cc", "clang"].iter().find(|c| Command::new(c).arg("--version").output().is_ok());
    let Some(cc) = cc else { return };
    let dir = scratch::Scratch::at(format!("ksn_malloc_count_thr_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let c_path = dir.join("malloc_count.c");
    let so_path = dir.join("malloc_count.so");
    fs::write(&c_path, include_str!("data/malloc_count.c")).unwrap();
    let r = Command::new(cc).args(["-shared", "-fPIC", "-O1", "-o"]).arg(&so_path).arg(&c_path).arg("-ldl").output().unwrap();
    assert!(r.status.success(), "{}", text(&r.stderr));
    let src = include_str!("data/threads/channel_rounds.ksnl");
    for release in [false, true] {
        let mut balance = vec![];
        for rounds in [5u32, 400u32] {
            let t = Tmp::new(&format!("thr_rounds_{}_{}", release, rounds), &src.replace("ROUNDS_N", &format!("{}u32", rounds)));
            let p = threads_build(&t, release);
            let o = Command::new(&p).env("LD_PRELOAD", &so_path).output().unwrap();
            assert_eq!(o.status.code(), Some(0), "{} {}", text(&o.stdout), text(&o.stderr));
            assert_eq!(text(&o.stdout), "channel rounds ok\n");
            let err = text(&o.stderr);
            let line = err.lines().find(|l| l.starts_with("allocs=")).unwrap_or_else(|| panic!("no counter output: {}", err));
            let nums: Vec<i64> = line.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).map(|s| s.parse().unwrap()).collect();
            // 1 周あたり、チャネルが 2 つ (本体 + バッファ) と文字列 5 件の確保がある (10 件弱)。
            assert!(nums[0] >= i64::from(rounds) * 8, "too few allocations ({})", line);
            balance.push(nums[0] - nums[1]);
        }
        assert_eq!(balance[0], balance[1], "allocations minus frees grew with the number of rounds ({:?}): leak or double free", balance);
    }
    let _ = fs::remove_dir_all(&dir);
}

const THREAD_SRC: &str = r#"fn t.in_order() -> u64 uses [alloc, thread] panics may
    let a = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u64 capture [] {
        10u64
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let b = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u64 capture [] {
        20u64
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let x = a.join()
    let y = b.join()
    x.wrapping_mul(100u64).wrapping_add(y)
end t.in_order

fn t.reversed() -> u64 uses [alloc, thread] panics may
    let a = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u64 capture [] {
        10u64
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let b = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u64 capture [] {
        20u64
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let c = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u64 capture [] {
        30u64
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let z = c.join()
    let y = b.join()
    let x = a.join()
    x.wrapping_mul(10000u64).wrapping_add(y.wrapping_mul(100u64)).wrapping_add(z)
end t.reversed

fn t.say(n: u32) -> u32 uses [io.console] panics never
    core.mem.drop(std.io.print_line("thread"))
    n
end t.say

fn t.order_print() -> u32 uses [alloc, io.console, thread] panics may
    let a = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u32 capture [] {
        t.say(1u32)
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let b = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u32 capture [] {
        t.say(2u32)
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let c = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u32 capture [] {
        t.say(3u32)
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    core.mem.drop(std.io.print_line("spawned"))
    let r = b.join()
    core.mem.drop(std.io.print_line("joined b"))
    let ra = a.join()
    let rc = c.join()
    core.mem.drop(std.io.print_line("joined all"))
    ra.wrapping_mul(100u32).wrapping_add(r.wrapping_mul(10u32)).wrapping_add(rc)
end t.order_print

fn t.moved(k: u32) -> u32 uses [alloc, thread] panics may
    let mut n = k
    let h = match std.thread.spawn(
        std.alloc.SYSTEM,
        fn() -> u32 capture [move n] {
            n.wrapping_mul(2u32)
        },
    ) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    n = 100u32
    let r = h.join()
    r.wrapping_add(n)
end t.moved

fn t.string_result() -> usize uses [alloc, thread] panics may
    let h = match std.thread.spawn(
        std.alloc.SYSTEM,
        fn() -> alloc.string.String<std.alloc.System> capture [] {
            let mut s = alloc.string.String.new(std.alloc.SYSTEM)
            core.mem.drop(s.push_str("hello"))
            s
        },
    ) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let s = h.join()
    let n = s.len()
    core.mem.drop(s)
    n
end t.string_result

fn t.vec_result() -> usize uses [alloc, thread] panics may
    let h = match std.thread.spawn(
        std.alloc.SYSTEM,
        fn() -> alloc.vec.Vec<u32, std.alloc.System> capture [] {
            let mut v: alloc.vec.Vec<u32, std.alloc.System> = alloc.vec.Vec.new(std.alloc.SYSTEM)
            core.mem.drop(v.push(5u32))
            core.mem.drop(v.push(6u32))
            core.mem.drop(v.push(7u32))
            v
        },
    ) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let v = h.join()
    let n = v.len()
    core.mem.drop(v)
    n
end t.vec_result
"#;

#[test]
fn kir_thread_spawn_join_match_tree_walker() {
    // join の順 (生成順・逆順)、join の時まで走らない遅延、join しないハンドル、結果が Vec / String、move の取り込み。
    let main = "\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may\n    core.check.eq(t.in_order(), 1020u64)\n    core.check.eq(t.reversed(), 102030u64)\n    core.check.eq(t.order_print(), 123u32)\n    core.check.eq(t.moved(1u32), 102u32)\n    core.check.eq(t.string_result(), 5usize)\n    core.check.eq(t.vec_result(), 3usize)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_thread", &format!("{}{}", THREAD_SRC, main));
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(0), "tree-walker main\n{}{}", text(&tw.stdout), text(&tw.stderr));
    let tw_out = text(&tw.stdout);
    for (f, want) in [("t.in_order", "1020"), ("t.reversed", "102030"), ("t.moved", "102"), ("t.string_result", "5"), ("t.vec_result", "3")] {
        let args: Vec<&str> = if f == "t.moved" { vec!["kir", f, "--run", "1"] } else { vec!["kir", f, "--run"] };
        let k = kir_run(&args, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
    // 出力の順 (spawn の時は何も出ず、b を join すると a, b の順に走り、c は c の join の時に走る) を木走査の表示と比べる。
    // 出力の順は実スレッドのネイティブでは決まらない (spawn した側が先に出す) ので、参照実行器だけで比べる。
    let k = run(&["kir", "t.order_print", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir order_print\n{}", text(&k.stderr));
    let want = ["spawned", "thread", "thread", "joined b", "thread", "joined all"];
    assert_eq!(tw_out.lines().collect::<Vec<_>>(), want, "木走査: join の時に生成順で走る");
    assert_eq!(text(&k.stdout).lines().collect::<Vec<_>>(), [&want[..], &["123"][..]].concat(), "kir の表示が木走査と違う");
}

const CHAN_AGG_SRC: &str = r#"type t.Job
    id: u32
    name: alloc.string.String<std.alloc.System>
    data: alloc.vec.Vec<u32, std.alloc.System>
end t.Job

fn t.oom(_e: alloc.alloc.AllocError) -> std.io.Error uses [] panics never
    std.io.Error { kind: std.io.ErrorKind.OutOfMemory, code: 0i32 }
end t.oom

fn t.word(n: u32) -> alloc.string.String<std.alloc.System> uses [alloc] panics never
    let mut s = alloc.string.String.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(s.push_str("ab"))
        i = i.wrapping_add(1u32)
    }
    s
end t.word

fn t.nums(n: u32) -> alloc.vec.Vec<u32, std.alloc.System> uses [alloc] panics never
    let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM)
    let mut i = 0u32
    while i < n {
        core.mem.drop(v.push(i))
        i = i.wrapping_add(1u32)
    }
    v
end t.nums

fn t.job(id: u32) -> t.Job uses [alloc] panics never
    t.Job { id: id, name: t.word(id), data: t.nums(id) }
end t.job

fn t.str_pipe() -> Result<usize, std.io.Error> uses [alloc, thread] panics may
    let made = std.sync.channel::<alloc.string.String<std.alloc.System>, std.alloc.System>(
        std.alloc.SYSTEM,
        4usize,
    )
    let (tx, rx) = made.map_err(t.oom)?
    let spawned = std.thread.spawn(
        std.alloc.SYSTEM,
        fn() -> () capture [move tx] {
            let mut i = 1u32
            while i < 6u32 {
                core.mem.drop(match tx.send(t.word(i)) {
                    Ok(_u) => 0u32
                    Err(_e) => core.panic.panic("send failed")
                })
                i = i.wrapping_add(1u32)
            }
        },
    )
    let h = match spawned {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let mut total = 0usize
    loop {
        match rx.recv() {
            Ok(s) => {
                total = total + s.len()
            }
            Err(_e) => break
        }
    }
    h.join()
    Ok(total)
end t.str_pipe

fn t.str_pipe_value() -> usize uses [alloc, thread] panics may
    match t.str_pipe() {
        Ok(n) => n
        Err(_e) => 0usize
    }
end t.str_pipe_value

fn t.vec_same() -> Result<usize, std.io.Error> uses [alloc, thread] panics may
    let made = std.sync.channel::<alloc.vec.Vec<u32, std.alloc.System>, std.alloc.System>(
        std.alloc.SYSTEM,
        2usize,
    )
    let (tx, rx) = made.map_err(t.oom)?
    let tx2 = tx.clone()
    core.mem.drop(tx.send(t.nums(3u32)))
    core.mem.drop(tx2.send(t.nums(5u32)))
    core.mem.drop(tx)
    core.mem.drop(tx2)
    let mut total = 0usize
    loop {
        match rx.recv() {
            Ok(v) => {
                total = total + v.len() * 10usize
            }
            Err(_e) => break
        }
    }
    Ok(total)
end t.vec_same

fn t.vec_same_value() -> usize uses [alloc, thread] panics may
    match t.vec_same() {
        Ok(n) => n
        Err(_e) => 0usize
    }
end t.vec_same_value

fn t.struct_loop(n: u32) -> Result<usize, std.io.Error> uses [alloc, thread] panics may
    let made = std.sync.channel::<t.Job, std.alloc.System>(std.alloc.SYSTEM, 64usize)
    let (tx, rx) = made.map_err(t.oom)?
    let mut i = 0u32
    while i < n {
        core.mem.drop(tx.send(t.job(i)))
        i = i.wrapping_add(1u32)
    }
    core.mem.drop(tx)
    let mut total = 0usize
    loop {
        match rx.recv() {
            Ok(j) => {
                total = total + j.name.len() + j.data.len()
            }
            Err(_e) => break
        }
    }
    Ok(total)
end t.struct_loop

fn t.struct_loop_value(n: u32) -> usize uses [alloc, thread] panics may
    match t.struct_loop(n) {
        Ok(v) => v
        Err(_e) => 0usize
    }
end t.struct_loop_value

fn t.drop_unreceived() -> Result<usize, std.io.Error> uses [alloc, thread] panics may
    let made = std.sync.channel::<t.Job, std.alloc.System>(std.alloc.SYSTEM, 64usize)
    let (tx, rx) = made.map_err(t.oom)?
    let mut i = 0u32
    while i < 40u32 {
        core.mem.drop(tx.send(t.job(i)))
        i = i.wrapping_add(1u32)
    }
    let first = match rx.recv() {
        Ok(j) => j.data.len()
        Err(_e) => 99usize
    }
    Ok(first + 1usize)
end t.drop_unreceived

fn t.drop_unreceived_value() -> usize uses [alloc, thread] panics may
    match t.drop_unreceived() {
        Ok(v) => v
        Err(_e) => 0usize
    }
end t.drop_unreceived_value

fn t.drop_strings() -> Result<usize, std.io.Error> uses [alloc, thread] panics may
    let made = std.sync.channel::<alloc.string.String<std.alloc.System>, std.alloc.System>(
        std.alloc.SYSTEM,
        2usize,
    )
    let (tx, rx) = made.map_err(t.oom)?
    core.mem.drop(tx.send(t.word(3u32)))
    core.mem.drop(tx.send(t.word(4u32)))
    core.mem.drop(rx)
    match tx.send(t.word(7u32)) {
        Ok(_u) => Ok(0usize)
        Err(e) => {
            let back = e.value
            Ok(back.len())
        }
    }
end t.drop_strings

fn t.drop_strings_value() -> usize uses [alloc, thread] panics may
    match t.drop_strings() {
        Ok(v) => v
        Err(_e) => 0usize
    }
end t.drop_strings_value
"#;

const SYNC_SRC: &str = r#"static t.TOTAL: std.sync.Mutex<u64> = std.sync.Mutex.new(5u64) pub

fn t.oom(_e: alloc.alloc.AllocError) -> std.io.Error uses [] panics never
    std.io.Error { kind: std.io.ErrorKind.OutOfMemory, code: 0i32 }
end t.oom

fn t.rng_values() -> u64 uses [] panics may
    let mut rng = std.rand.Rng.from_seed(7u64)
    let a = rng.next_u64()
    let b = rng.next_u64()
    let mut again = std.rand.Rng.from_seed(7u64)
    core.check.eq(again.next_u64(), a)
    core.check.eq(again.next_u64(), b)
    let mut other = std.rand.Rng.from_seed(8u64)
    core.check.eq(other.next_u64() == a, false)
    a.wrapping_add(b)
end t.rng_values

fn t.rng_step(r: &mut std.rand.Rng) -> u64 uses [] panics never
    r.next_u64()
end t.rng_step

fn t.rng_ref() -> u64 uses [] panics may
    let mut rng = std.rand.Rng.from_seed(99u64)
    let a = t.rng_step(&mut rng)
    let b = t.rng_step(&mut rng)
    let mut same = std.rand.Rng.from_seed(99u64)
    core.check.eq(same.next_u64(), a)
    core.check.eq(same.next_u64(), b)
    a ^ b
end t.rng_ref

fn t.bump(n: u32) -> () uses [thread] panics may
    let mut i = 0u32
    while i < n {
        let mut guard = t.TOTAL.lock()
        let seen = *guard.get()
        *guard.get_mut() = seen + 1u64
        i = i + 1u32
    }
end t.bump

fn t.mutex_adds() -> u64 uses [thread] panics may
    t.bump(10u32)
    t.bump(5u32)
    let total = t.TOTAL.lock()
    let v = *total.get()
    core.mem.drop(total)
    v
end t.mutex_adds

fn t.produce(tx: std.sync.Sender<u32>, from: u32, count: u32) -> () uses [thread] panics may
    let mut i = 0u32
    while i < count {
        core.mem.drop(match tx.send(from + i) {
            Ok(_u) => 0u32
            Err(_e) => core.panic.panic("send failed")
        })
        i = i + 1u32
    }
end t.produce

fn t.spawn_producer(tx: std.sync.Sender<u32>, from: u32, count: u32) -> std.thread.JoinHandle<(), std.alloc.System> uses [alloc, thread] panics may
    let spawned = std.thread.spawn(
        std.alloc.SYSTEM,
        fn() -> () capture [move tx, move from, move count] {
            t.produce(tx, from, count)
        },
    )
    match spawned {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
end t.spawn_producer

fn t.pipe() -> Result<u32, std.io.Error> uses [alloc, thread] panics may
    let made = std.sync.channel::<u32, std.alloc.System>(std.alloc.SYSTEM, 4usize)
    let (tx, rx) = made.map_err(t.oom)?
    let producer = t.spawn_producer(tx, 1u32, 100u32)
    let mut sum = 0u32
    loop {
        match rx.recv() {
            Ok(n) => {
                sum = sum + n
            }
            Err(_e) => break
        }
    }
    producer.join()
    Ok(sum)
end t.pipe

fn t.pipe_sum() -> u32 uses [alloc, thread] panics may
    match t.pipe() {
        Ok(n) => n
        Err(_e) => 0u32
    }
end t.pipe_sum

fn t.same_thread() -> Result<u32, std.io.Error> uses [alloc, thread] panics may
    let made = std.sync.channel::<u32, std.alloc.System>(std.alloc.SYSTEM, 4usize)
    let (tx, rx) = made.map_err(t.oom)?
    let tx2 = tx.clone()
    core.mem.drop(tx.send(1u32))
    core.mem.drop(tx2.send(20u32))
    core.mem.drop(tx.send(300u32))
    core.mem.drop(tx)
    core.mem.drop(tx2)
    let mut sum = 0u32
    let mut n = 0u32
    loop {
        match rx.recv() {
            Ok(v) => {
                sum = sum + v
                n = n + 1u32
            }
            Err(_e) => break
        }
    }
    Ok(sum * 10u32 + n)
end t.same_thread

fn t.same_thread_sum() -> u32 uses [alloc, thread] panics may
    match t.same_thread() {
        Ok(n) => n
        Err(_e) => 0u32
    }
end t.same_thread_sum

fn t.send_after_close() -> Result<u32, std.io.Error> uses [alloc, thread] panics may
    let made = std.sync.channel::<u32, std.alloc.System>(std.alloc.SYSTEM, 2usize)
    let (tx, rx) = made.map_err(t.oom)?
    core.mem.drop(rx)
    match tx.send(7u32) {
        Ok(_u) => Ok(0u32)
        Err(e) => Ok(e.value)
    }
end t.send_after_close

fn t.send_after_close_value() -> u32 uses [alloc, thread] panics may
    match t.send_after_close() {
        Ok(n) => n
        Err(_e) => 1u32
    }
end t.send_after_close_value

fn t.two_producers() -> Result<u32, std.io.Error> uses [alloc, thread] panics may
    let made = std.sync.channel::<u32, std.alloc.System>(std.alloc.SYSTEM, 4usize)
    let (tx, rx) = made.map_err(t.oom)?
    let tx2 = tx.clone()
    let a = t.spawn_producer(tx, 1u32, 3u32)
    let b = t.spawn_producer(tx2, 10u32, 3u32)
    let mut order = 0u32
    loop {
        match rx.recv() {
            Ok(n) => {
                order = order * 2u32 + n / 10u32
            }
            Err(_e) => break
        }
    }
    b.join()
    a.join()
    Ok(order)
end t.two_producers

fn t.two_producers_order() -> u32 uses [alloc, thread] panics may
    match t.two_producers() {
        Ok(n) => n
        Err(_e) => 1u32
    }
end t.two_producers_order

fn t.empty_recv() -> Result<u32, std.io.Error> uses [alloc, thread] panics may
    let made = std.sync.channel::<u32, std.alloc.System>(std.alloc.SYSTEM, 2usize)
    let (tx, rx) = made.map_err(t.oom)?
    let got = match rx.recv() {
        Ok(n) => n
        Err(_e) => 0u32
    }
    core.mem.drop(tx)
    Ok(got)
end t.empty_recv

fn t.empty_recv_value() -> u32 uses [alloc, thread] panics may
    match t.empty_recv() {
        Ok(n) => n
        Err(_e) => 1u32
    }
end t.empty_recv_value

fn t.loop_spawn() -> u32 uses [alloc, thread] panics may
    let mut total = 0u32
    let mut i = 0u32
    while i < 3u32 {
        let h = match std.thread.spawn(
            std.alloc.SYSTEM,
            fn() -> u32 capture [move i] {
                i * 10u32 + 1u32
            },
        ) {
            Ok(h) => h
            Err(_e) => core.panic.panic("spawn failed")
        }
        total = total + h.join()
        i = i + 1u32
    }
    total
end t.loop_spawn
"#;

#[test]
fn kir_sync_values_match_tree_walker() {
    let main = "\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may\n    core.check.eq(t.mutex_adds(), 20u64)\n    core.check.eq(t.pipe_sum(), 5050u32)\n    core.check.eq(t.same_thread_sum(), 3213u32)\n    core.check.eq(t.send_after_close_value(), 7u32)\n    core.check.eq(t.two_producers_order(), 7u32)\n    core.check.eq(t.empty_recv_value(), 0u32)\n    core.check.eq(t.loop_spawn(), 33u32)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_sync", &format!("{}{}", SYNC_SRC, main));
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(101), "木走査: 空の recv は送り手が生きたまま deadlock\n{}{}", text(&tw.stdout), text(&tw.stderr));
    // 木走査の値は各関数を `core.check.eq` で確かめる (empty_recv は別に deadlock を見る)。
    let main_ok = main.replace("    core.check.eq(t.empty_recv_value(), 0u32)\n", "");
    let t = Tmp::new("kir_sync_ok", &format!("{}{}", SYNC_SRC, main_ok));
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(0), "木走査 main\n{}{}", text(&tw.stdout), text(&tw.stderr));
    for (f, want) in [
        ("t.mutex_adds", "20"),
        ("t.pipe_sum", "5050"),
        ("t.same_thread_sum", "3213"),
        ("t.send_after_close_value", "7"),
        ("t.loop_spawn", "33"),
    ] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
    // 2 本の生産者の受信の順は、実スレッドのネイティブでは決まらない (協調実行の生成順ではない)。参照実行器だけで比べる。
    let k = run(&["kir", "t.two_producers_order", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(0), "kir: two_producers_order\n{}", text(&k.stderr));
    assert_eq!(text(&k.stdout).trim(), "7");
    // Rng: 木走査と同じ列 (シードが同じなら同じ、別のシードなら別)。値は木走査の core.check と --run / --native で比べる。
    for f in ["t.rng_values", "t.rng_ref"] {
        let mut v = vec![];
        for engine in ["--run", "--native"] {
            let k = run(&["kir", f, engine], &t.0);
            assert_eq!(k.status.code(), Some(0), "kir {} {}\n{}", f, engine, text(&k.stderr));
            v.push(text(&k.stdout));
        }
        assert_eq!(v[0], v[1], "{}: --run と --native の値", f);
    }
    // 木走査との値の一致: Rng の値を木走査の `core.check.eq` へ埋め込んで確かめる。
    let want = text(&run(&["kir", "t.rng_values", "--run"], &t.0).stdout).trim().to_string();
    let m = format!("\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may\n    core.check.eq(t.rng_values(), {}u64)\n    Ok(())\nend t.main\n", want);
    let t2 = Tmp::new("kir_sync_rng", &format!("{}{}", SYNC_SRC, m));
    assert_eq!(run(&["run"], &t2.0).status.code(), Some(0), "Rng の値が木走査と違う ({})", want);
    let want = text(&run(&["kir", "t.rng_ref", "--run"], &t.0).stdout).trim().to_string();
    let m = format!("\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may\n    core.check.eq(t.rng_ref(), {}u64)\n    Ok(())\nend t.main\n", want);
    let t3 = Tmp::new("kir_sync_rng2", &format!("{}{}", SYNC_SRC, m));
    assert_eq!(run(&["run"], &t3.0).status.code(), Some(0), "Rng (&mut の引数) の値が木走査と違う ({})", want);
}

#[test]
fn kir_chan_aggregate_elements_match_tree_walker() {
    // チャネルの要素が String / Vec / 構造体 (所有物を持つ)。送ると所有が受け手へ移り、受け取らずに最後の端が落ちたら残りを解放する
    // (設計_KIR.md 22.6)。木走査と `--run` / `--native` (と ASan) の値が一致し、参照実行器が漏れ・二重解放を検出しない。
    let cases: &[(&str, &str)] = &[
        ("t.str_pipe_value", "30usize"),
        ("t.vec_same_value", "80usize"),
        ("t.struct_loop_value", "0usize"),
        ("t.drop_unreceived_value", "1usize"),
        ("t.drop_strings_value", "14usize"),
    ];
    for (i, (f, lit)) in cases.iter().enumerate() {
        let call = if *f == "t.struct_loop_value" { format!("{}(6u32)", f) } else { format!("{}()", f) };
        let lit = if *f == "t.struct_loop_value" { "45usize" } else { *lit };
        let main = format!("\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may\n    core.check.eq({}, {})\n    Ok(())\nend t.main\n", call, lit);
        let t = Tmp::new(&format!("kir_chan_agg_{}", i), &format!("{}{}", CHAN_AGG_SRC, main));
        let tw = run(&["run"], &t.0);
        assert_eq!(tw.status.code(), Some(0), "木走査: {}\n{}{}", call, text(&tw.stdout), text(&tw.stderr));
        let mut args = vec!["kir", *f, "--run"];
        if *f == "t.struct_loop_value" {
            args.push("6");
        }
        let k = kir_run(&args, &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", call, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), lit.trim_end_matches("usize"), "kir value of {}", call);
    }
}

#[test]
fn kir_sync_empty_recv_with_live_sender_deadlocks_like_the_tree_walker() {
    let main = "\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may\n    core.check.eq(t.empty_recv_value(), 0u32)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_sync_dead", &format!("{}{}", SYNC_SRC, main));
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(101));
    let tw_err = text(&tw.stderr);
    assert!(tw_err.contains("deadlock: recv() on an empty channel"), "{}", tw_err);
    // kir_run は panic (101) の文言も --run と --native で比べる。
    let k = kir_run(&["kir", "t.empty_recv_value", "--run"], &t.0);
    assert_eq!(k.status.code(), Some(101), "kir: {}", text(&k.stderr));
    assert!(text(&k.stderr).contains("deadlock: recv() on an empty channel"), "{}", text(&k.stderr));
}

const REAL_THREADS_SRC: &str = r#"static t.COUNTER: std.sync.Mutex<u64> = std.sync.Mutex.new(0u64) pub

fn t.add_many(n: u32) -> () uses [thread] panics may
    let mut i = 0u32
    while i < n {
        let g = t.COUNTER.lock()
        core.mem.drop(g)
        let mut h = t.COUNTER.lock()
        let r = h.get_mut()
        *r = *r + 1u64
        core.mem.drop(h)
        i = i + 1u32
    }
end t.add_many

fn t.contend(left: u32, per: u32) -> u64 uses [alloc, thread] panics may
    if left == 0u32 {
        t.add_many(per)
        return 0u64
    }
    let handle = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u64 capture [move per] {
        t.add_many(per)
        1u64
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let inner = t.contend(left - 1u32, per)
    let v = handle.join()
    inner + v
end t.contend

fn t.contended() -> u64 uses [alloc, thread] panics may
    let spawned = t.contend(7u32, 2000u32)
    let total = t.COUNTER.lock()
    let v = *total.get()
    core.mem.drop(total)
    v + spawned * 1000000u64
end t.contended

fn t.fan(left: u32) -> u64 uses [alloc, thread] panics may
    if left == 0u32 {
        return 0u64
    }
    let handle = match std.thread.spawn(std.alloc.SYSTEM, fn() -> u64 capture [move left] {
        u64.from_u32(left)
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let inner = t.fan(left - 1u32)
    let v = handle.join()
    inner + v
end t.fan

fn t.fan40() -> u64 uses [alloc, thread] panics may
    t.fan(40u32)
end t.fan40

fn t.fan300() -> u64 uses [alloc, thread] panics may
    t.fan(300u32)
end t.fan300

fn t.pc_produce(tx: std.sync.Sender<u32>, from: u32, count: u32) -> () uses [thread] panics may
    let mut i = 0u32
    while i < count {
        core.mem.drop(match tx.send(from + i) {
            Ok(_u) => 0u32
            Err(_e) => core.panic.panic("send failed")
        })
        i = i + 1u32
    }
end t.pc_produce

fn t.pc_sum() -> u32 uses [alloc, thread] panics may
    let made = std.sync.channel::<u32, std.alloc.System>(std.alloc.SYSTEM, 4usize)
    let (tx, rx) = match made {
        Ok(p) => p
        Err(_e) => core.panic.panic("channel failed")
    }
    let tx2 = tx.clone()
    let a = match std.thread.spawn(std.alloc.SYSTEM, fn() -> () capture [move tx] {
        t.pc_produce(tx, 0u32, 500u32)
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let b = match std.thread.spawn(std.alloc.SYSTEM, fn() -> () capture [move tx2] {
        t.pc_produce(tx2, 1000u32, 500u32)
    }) {
        Ok(h) => h
        Err(_e) => core.panic.panic("spawn failed")
    }
    let mut sum = 0u32
    let mut n = 0u32
    while n < 1000u32 {
        match rx.recv() {
            Ok(v) => {
                sum = sum + v
            }
            Err(_e) => core.panic.panic("recv failed")
        }
        n = n + 1u32
    }
    a.join()
    b.join()
    sum
end t.pc_sum
"#;

#[test]
fn kir_native_threads_are_real_pthreads() {
    // 実スレッド (pthread): 表の上限 (参照実行器の 1024) を超える 300 本、複数スレッドからの Mutex の相互排他、
    // 2 本の生産者と 1 本の消費者のチャネル。値は ksn run (木走査) と --run / --native が一致する。
    if !clang_available() {
        return;
    }
    let main = "\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may\n    core.check.eq(t.fan40(), 820u64)\n    core.check.eq(t.fan300(), 45150u64)\n    core.check.eq(t.contended(), 7016000u64)\n    core.check.eq(t.pc_sum(), 749500u32)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_real_threads", &format!("{}{}", REAL_THREADS_SRC, main));
    let tw = run(&["run"], &t.0);
    assert_eq!(tw.status.code(), Some(0), "木走査 main\n{}{}", text(&tw.stdout), text(&tw.stderr));
    for (f, want) in [("t.fan40", "820"), ("t.contended", "7016000"), ("t.pc_sum", "749500")] {
        let k = kir_run(&["kir", f, "--run"], &t.0);
        assert_eq!(k.status.code(), Some(0), "kir: {}\n{}", f, text(&k.stderr));
        assert_eq!(text(&k.stdout).trim(), want, "kir value of {}", f);
    }
    // 300 本 (協調実行の表は 1024 本まで。ネイティブは上限なし)。繰り返して競合を拾う。
    for _ in 0..5 {
        for (f, want) in [("t.fan300", "45150"), ("t.contended", "7016000"), ("t.pc_sum", "749500")] {
            let k = run(&["kir", f, "--native"], &t.0);
            assert_eq!(k.status.code(), Some(0), "native: {}\n{}", f, text(&k.stderr));
            assert_eq!(text(&k.stdout).trim(), want, "native value of {}", f);
        }
    }
}

/// clang が `-fsanitize=thread` の実行ファイルを作って走らせられるか (環境変数 `KSN_NO_TSAN=1` で無効にできる)。
fn tsan_available() -> bool {
    use std::sync::OnceLock;
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        if cfg!(windows) || !clang_available() || std::env::var("KSN_NO_TSAN").is_ok() {
            return false;
        }
        let d = std::env::temp_dir().join(format!("ksn_tsan_probe_{}", std::process::id()));
        let _ = fs::create_dir_all(&d);
        let c = d.join("a.c");
        let o = d.join("a.out");
        fs::write(&c, "int main(void){return 0;}\n").unwrap();
        let ok = Command::new("clang").args(["-fsanitize=thread"]).arg(&c).arg("-o").arg(&o).output().map(|r| r.status.success()).unwrap_or(false)
            && Command::new(&o).output().map(|r| r.status.success()).unwrap_or(false);
        let _ = fs::remove_dir_all(&d);
        if !ok {
            eprintln!("skip: clang の ThreadSanitizer が使えないので、TSan の実行は飛ばす");
        }
        ok
    })
}

/// ThreadSanitizer 付きのネイティブで、実スレッド (pthread) のプログラムを走らせてもデータ競合の警告が出ない (設計_KIR.md 22.7)。
/// `KSN_CLANG_ARGS` で `-fsanitize=thread` を渡す。TSan が使えない環境では黙って飛ばす。
#[test]
fn kir_native_threads_have_no_data_races_under_threadsanitizer() {
    if !tsan_available() {
        return;
    }
    let tsan = |cmd: &mut Command| {
        cmd.env("KSN_CLANG_ARGS", "-fsanitize=thread -g -O1").env("TSAN_OPTIONS", "halt_on_error=0");
    };
    // 1. Mutex の相互排他・チャネルを `ksn build` で TSan 付きにして走らせる。
    for name in ["mutex_counter", "channel", "channel_clone"] {
        let t = threads_pkg(name, "tsan");
        let mut c = Command::new(exe());
        let cache = scratch::Scratch::at(format!("ksn_cache_tsan_{}_{}", std::process::id(), name));
        c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
        tsan(&mut c);
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        assert_native_or_reasoned_skip(&out);
        let p = native_or_launcher(t.0.join("target").join("debug").join("t"), &out);
        let r = Command::new(&p).env("TSAN_OPTIONS", "halt_on_error=0").output().unwrap();
        let err = text(&r.stderr);
        assert!(!err.contains("WARNING: ThreadSanitizer"), "{}: TSan が競合を報告した\n{}", name, err);
        assert_eq!(r.status.code(), Some(0), "{}: {}", name, err);
    }
    // 2. 300 本・Mutex の競合・2 生産者 1 消費者 (`kir_native_threads_are_real_pthreads` と同じプログラム)。
    let main = "\nfn t.main() -> Result<(), std.io.Error> uses [alloc, io.console, thread] panics may\n    core.check.eq(t.fan300(), 45150u64)\n    Ok(())\nend t.main\n";
    let t = Tmp::new("kir_tsan_threads", &format!("{}{}", REAL_THREADS_SRC, main));
    for (f, want) in [("t.fan300", "45150"), ("t.contended", "7016000"), ("t.pc_sum", "749500")] {
        let mut c = Command::new(exe());
        c.args(["kir", f, "--native"]).current_dir(&t.0);
        tsan(&mut c);
        let k = c.output().unwrap();
        let err = text(&k.stderr);
        assert!(!err.contains("WARNING: ThreadSanitizer"), "{}: TSan が競合を報告した\n{}", f, err);
        assert_eq!(k.status.code(), Some(0), "{}: {}", f, err);
        assert_eq!(text(&k.stdout).trim(), want, "{}", f);
    }
}

/// 陽性対照: 故意にデータ競合を起こすプログラム (`threads/tsan_race_control.ksnl`、生ポインタを `[unsafe]` モジュールで 2 スレッドから書く)。
/// TSan が生成コードを計測していれば必ず "WARNING: ThreadSanitizer" を出す。出なければ TSan の試験は空振りなので失敗させる。
#[test]
fn tsan_positive_control_reports_a_deliberate_race() {
    if !tsan_available() {
        return;
    }
    let src = fs::read_to_string(root().join("stage0").join("tests").join("data").join("threads").join("tsan_race_control.ksnl")).unwrap();
    let t = Tmp::new_raw("thr_tsan_control", &src);
    let toml = fs::read_to_string(t.0.join("kasane.toml")).unwrap();
    fs::write(t.0.join("kasane.toml"), toml.replacen("[dependencies]", "[unsafe]\nmodules = [\"t\"]\n\n[dependencies]", 1)).unwrap();
    let cache = scratch::Scratch::at(format!("ksn_cache_tsan_control_{}", std::process::id()));
    let mut c = Command::new(exe());
    c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache);
    c.env("KSN_CLANG_ARGS", "-fsanitize=thread -g -O1");
    let o = c.output().unwrap();
    let out = text(&o.stdout) + &text(&o.stderr);
    assert_native_or_reasoned_skip(&out);
    let p = native_or_launcher(t.0.join("target").join("debug").join("t"), &out);
    let r = Command::new(&p).env("TSAN_OPTIONS", "halt_on_error=0").output().unwrap();
    let err = text(&r.stderr);
    assert!(err.contains("WARNING: ThreadSanitizer"), "陽性対照で TSan が競合を報告しない (TSan の試験が空振り)\n{}", err);
}

// ------------------------------------------------------------ 全員待ちの検出・Mutex のガードの寿命 (設計_KIR.md 22.7)

/// 子プロセスを待つ。`Command::output` だと、全員待ちを検出できずに止まったとき、テストも止まってしまう。
/// 制限時間を過ぎたら殺して失敗にする。
fn output_within(mut c: Command, secs: u64) -> Output {
    use std::time::{Duration, Instant};
    c.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(secs) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{} 秒経っても終わらない (全員待ちを検出できていない)", secs);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

/// 全員が待ちで誰も進めないとき、止まらずに `deadlock` の panic (終了コード 101) で終わる (木走査と同じ文言)。
/// `name` は `tests/data/threads/<name>.ksnl`。ネイティブは debug / release の両方で確かめる。
fn assert_deadlock_exits_101(name: &str) {
    let t = threads_pkg(name, "dl");
    let mut c = Command::new(exe());
    c.args(["run"]).current_dir(&t.0);
    let interp = output_within(c, 60);
    assert_eq!(interp.status.code(), Some(101), "木走査 {}: {}", name, text(&interp.stderr));
    assert!(text(&interp.stderr).contains("panic: deadlock"), "木走査 {}: {}", name, text(&interp.stderr));
    if cfg!(windows) {
        return;
    }
    for release in [false, true] {
        let p = threads_build(&t, release);
        let r = output_within(Command::new(&p), 60);
        assert_eq!(r.status.code(), Some(101), "{} (release={}): {}", name, release, text(&r.stderr));
        assert!(text(&r.stderr).contains("panic: deadlock"), "{} (release={}): {}", name, release, text(&r.stderr));
    }
}

/// 2 本のスレッドが互いの `recv` を待つ (main は worker の返事を、worker は main の送信を待つ)。どちらも進めないので deadlock。
#[test]
fn threads_native_mutual_recv_wait_is_a_deadlock_101() {
    assert_deadlock_exits_101("deadlock_mutual");
}

/// main が worker を `join` し、worker は main が送り手を持っている空のチャネルを `recv` で待つ。`join` の待ちも数えて検出する。
#[test]
fn threads_native_join_wait_with_a_blocked_worker_is_a_deadlock_101() {
    assert_deadlock_exits_101("deadlock_join");
}

/// worker が `static` の `Mutex` のガードを持ったまま `recv` で待ち、main が同じ `Mutex` を `lock` で待つ。Mutex の待ちも数える。
#[test]
fn threads_native_mutex_wait_with_a_blocked_holder_is_a_deadlock_101() {
    assert_deadlock_exits_101("deadlock_mutex");
}

/// 正しいプール (受付の main、ワーカー 3 本、容量 2 のチャネル、Mutex の共有カウンタ) は誤検出されず、木走査と同じ結果で終わる。
#[test]
fn threads_worker_pool_with_shared_mutex_is_not_a_false_deadlock() {
    threads_same_output("pool_shared");
}

/// `core.mem.drop(g)` で外したガードは、その時点で外れる (スコープの終わりまで保たない)。外したあとで別のスレッドが同じ `Mutex` を取れる。
#[test]
fn threads_mutex_guard_dropped_with_core_mem_drop_is_released_at_once() {
    threads_same_output("guard_drop");
}

/// 文の中で取って束縛しなかったガードは文の終わりで外れる。`join` の前に外すので、worker は同じ `Mutex` を取れる。
#[test]
fn threads_mutex_temporary_guard_is_released_at_the_end_of_its_statement() {
    threads_same_output("guard_temp");
}

/// `while` の条件の中の `Mutex` の取得は、条件の評価のたびに対で外れる (ループの後に残らない)。
#[test]
fn threads_mutex_guard_in_a_loop_is_released_each_round() {
    threads_same_output("guard_while");
}

/// ThreadSanitizer: 正しいプール・ガードの試験と、全員待ちの試験 (101 で終わる) で、データ競合の警告が出ない。
#[test]
fn kir_native_pool_guards_and_deadlock_have_no_data_races_under_threadsanitizer() {
    if !tsan_available() {
        return;
    }
    for (name, want) in [("pool_shared", 0), ("guard_drop", 0), ("guard_temp", 0), ("guard_while", 0), ("deadlock_mutex", 101)] {
        let t = threads_pkg(name, "tsan2");
        let cache = scratch::Scratch::at(format!("ksn_cache_tsan2_{}_{}", std::process::id(), name));
        let mut c = Command::new(exe());
        c.args(["build", "--backend", "llvm"]).current_dir(&t.0).env("KSN_CACHE_DIR", &cache).env("KSN_CLANG_ARGS", "-fsanitize=thread -g -O1");
        let o = c.output().unwrap();
        let out = text(&o.stdout) + &text(&o.stderr);
        assert_native_or_reasoned_skip(&out);
        let p = native_or_launcher(t.0.join("target").join("debug").join("t"), &out);
        let mut r = Command::new(&p);
        r.env("TSAN_OPTIONS", "halt_on_error=0");
        let r = output_within(r, 120);
        let err = text(&r.stderr);
        assert!(!err.contains("WARNING: ThreadSanitizer"), "{}: TSan が競合を報告した\n{}", name, err);
        assert_eq!(r.status.code(), Some(want), "{}: {}", name, err);
        let _ = fs::remove_dir_all(&cache);
    }
}

/// 16 本の送り手 × 10 万件を 1 つのチャネルへ流し、受け手が合計 (厳密) と送り手ごとの順序 (FIFO) を確かめる (ネイティブのチャネルごとのロック・待ち)。
/// 木走査は遅いので走らせない (意味は他の `channel` の試験が同じ出力で確かめる)。空の recv の deadlock は `deadlock_join` / `deadlock_mutual` が見る。
#[test]
fn threads_native_channel_stress_16_producers_keep_exact_sum_and_per_producer_order() {
    if cfg!(windows) {
        return;
    }
    let t = threads_pkg("channel_stress", "stress");
    for release in [false, true] {
        let p = threads_build(&t, release);
        let r = output_within(Command::new(&p), 300);
        assert_eq!(r.status.code(), Some(0), "release={}: {}", release, text(&r.stderr));
        assert_eq!(text(&r.stdout), "channel stress ok\n");
    }
}
