//! 中断 `uses [suspend]` と協調実行器 `std.task` (仕様 5.6、第 1 段)。
//!
//! 実行は木走査 (`ksn run`) で行う。効果の伝播 (E4001)、中断をまたぐ借用 (E3050)、ネイティブ生成の退避 (E9040) を
//! 確かめる。実行の結果 (標準出力・終了コード) は、合図の順が決定的であることを確かめるために正確に比べる。

#![allow(dead_code, unused_imports)]

mod common;

use common::*;

/// `io.console` で 1 行出す `t.say` (`print_line` の `Result` を捨てる定型)。
const SAY: &str = "fn t.say(text: &str) -> () uses [io.console] panics never
    match std.io.print_line(text) {
        Ok(u) => u
        Err(_e) => ()
    }
end t.say

";

/// `t.say` を前に付けたソース。
fn with_say(rest: &str) -> String {
    format!("{}{}", SAY, rest)
}

/// `ksn run` の (標準出力, 終了コード, 標準エラー)。標準出力には非致命の診断 (`src/...` で始まる行) も
/// 混ざるので、プログラム自身の出力だけを返す。
fn run_src(name: &str, src: &str) -> (String, Option<i32>, String) {
    let t = Tmp::new(name, src);
    let o = run(&["run"], &t.0);
    let out: String = text(&o.stdout).lines().filter(|l| !l.starts_with("src/")).map(|l| format!("{}\n", l)).collect();
    (out, o.status.code(), text(&o.stderr))
}

/// `ksn check --json` の診断コード。
fn check_codes(name: &str, src: &str) -> Vec<String> {
    let t = Tmp::new(name, src);
    let o = run(&["check", "--json"], &t.0);
    codes(&text(&o.stdout))
}

// ------------------------------------------------------------ 実行: 協調実行の順と結果

/// 2 つのタスクが `yield_now` で交互に進む。`join` は生成の順に待つ。結果の順は決定的。
#[test]
fn yield_alternates_two_tasks() {
    let src = with_say(
        "fn t.worker_a() -> u32 uses [io.console, suspend] panics may
    let mut i = 0u32
    while i < 3u32 {
        t.say(\"A\")
        std.task.yield_now()
        i = i + 1u32
    }
    7u32
end t.worker_a

fn t.worker_b() -> u32 uses [io.console, suspend] panics may
    let mut i = 0u32
    while i < 3u32 {
        t.say(\"B\")
        std.task.yield_now()
        i = i + 1u32
    }
    9u32
end t.worker_b

fn t.body() -> () uses [io.console, suspend] panics may
    let a = std.task.spawn_local(
        fn() -> u32 capture [] {
            t.worker_a()
        },
    )
    let b = std.task.spawn_local(
        fn() -> u32 capture [] {
            t.worker_b()
        },
    )
    let ra = a.join_task()
    if ra == 7u32 {
        t.say(\"ra\")
    }
    let rb = b.join_task()
    if rb == 9u32 {
        t.say(\"rb\")
    }
    t.say(\"body done\")
end t.body

fn t.main() -> () uses [io.console, suspend] panics may
    std.task.block_on(fn() -> () capture [] {
        t.body()
    })
    t.say(\"main done\")
end t.main
",
    );
    let (out, rc, err) = run_src("suspend_alternate", &src);
    assert_eq!(rc, Some(0), "stderr: {}", err);
    assert_eq!(out, "A\nB\nA\nB\nA\nB\nra\nrb\nbody done\nmain done\n");
}

/// 中断する関数の戻り値は、`join` で呼び出し側に戻る。`block_on` の戻り値も本体の値。
#[test]
fn suspend_fn_return_value_and_block_on_result() {
    let src = with_say(
        "fn t.five() -> u32 uses [suspend] panics never
    std.task.yield_now()
    5u32
end t.five

fn t.main() -> () uses [io.console, suspend] panics may
    let v = std.task.block_on(fn() -> u32 capture [] {
        t.five()
    })
    if v == 5u32 {
        t.say(\"five\")
    }
end t.main
",
    );
    let (out, rc, err) = run_src("suspend_return", &src);
    assert_eq!(rc, Some(0), "stderr: {}", err);
    assert_eq!(out, "five\n");
}

/// `block_on` は、本体が終わったあとも、まだ終わっていないタスクを待ってから返る (join されないタスクも走る)。
#[test]
fn block_on_drains_unjoined_tasks() {
    let src = with_say(
        "fn t.tick(n: u32) -> () uses [io.console, suspend] panics never
    let mut i = 0u32
    while i < n {
        std.task.yield_now()
        i = i.wrapping_add(1u32)
    }
    t.say(\"unjoined task finished\")
end t.tick

fn t.main() -> () uses [io.console, suspend] panics may
    std.task.block_on(fn() -> () capture [] {
        let a = std.task.spawn_local(fn() -> () capture [] {
            t.tick(2u32)
        })
        t.say(\"root body done\")
    })
    t.say(\"after block_on\")
end t.main
",
    );
    let (out, rc, err) = run_src("suspend_drain", &src);
    assert_eq!(rc, Some(0), "stderr: {}", err);
    assert_eq!(out, "root body done\nunjoined task finished\nafter block_on\n");
}

/// 多数のタスク (60 本) と、中断をはさむ深さ 500 の再帰が動く。
#[test]
fn many_tasks_and_deep_suspending_recursion() {
    let src = with_say(
        "fn t.many(tag: u32) -> u32 uses [suspend] panics never
    std.task.yield_now()
    tag
end t.many

fn t.rec(n: u32) -> u32 uses [suspend] panics never stack 4096
    if n == 0u32 {
        return 0u32
    }
    std.task.yield_now()
    1u32.wrapping_add(t.rec(n.wrapping_sub(1u32)))
end t.rec

fn t.main() -> () uses [io.console, suspend] panics may
    std.task.block_on(fn() -> () capture [] {
        let mut hs = 0u32
        let mut j = 0u32
        while j < 60u32 {
            let h = std.task.spawn_local(fn() -> u32 capture [move j] {
                t.many(j)
            })
            let v = h.join_task()
            hs = hs.wrapping_add(v)
            j = j.wrapping_add(1u32)
        }
        if hs == 1770u32 {
            t.say(\"many ok\")
        }
    })
    let r = std.task.block_on(fn() -> u32 capture [] {
        t.rec(500u32)
    })
    if r == 500u32 {
        t.say(\"rec ok\")
    }
end t.main
",
    );
    let (out, rc, err) = run_src("suspend_many", &src);
    assert_eq!(rc, Some(0), "stderr: {}", err);
    assert_eq!(out, "many ok\nrec ok\n");
}

/// タスクの中の panic は、プロセスを中断する (終了コード 101、5.2)。`join` していなくても同じ。
#[test]
fn panic_in_task_aborts_with_101() {
    let src = "fn t.boom() -> u32 uses [suspend] panics may
    std.task.yield_now()
    core.panic.panic(\"boom in task\")
end t.boom

fn t.main() -> () uses [suspend] panics may
    std.task.block_on(fn() -> () capture [] {
        let a = std.task.spawn_local(fn() -> u32 capture [] {
            t.boom()
        })
        let r = a.join_task()
        core.mem.drop(r)
    })
end t.main
";
    let (_out, rc, err) = run_src("suspend_panic", src);
    assert_eq!(rc, Some(101), "stderr: {}", err);
    assert!(err.contains("panic: boom in task"), "stderr: {}", err);
}

/// join されなかったタスクの panic も、`block_on` の終わりで根へ届いて 101 になる。
#[test]
fn unjoined_task_panic_reaches_block_on() {
    let src = "fn t.main() -> () uses [suspend] panics may
    std.task.block_on(fn() -> () capture [] {
        let a = std.task.spawn_local(fn() -> () capture [] {
            std.task.yield_now()
            core.panic.panic(\"late failure\")
        })
    })
end t.main
";
    let (_out, rc, err) = run_src("suspend_unjoined_panic", src);
    assert_eq!(rc, Some(101), "stderr: {}", err);
    assert!(err.contains("late failure"), "stderr: {}", err);
}

/// `block_on` の外で `spawn_local` すると実行時の panic (実行器がない)。
#[test]
fn spawn_outside_block_on_panics() {
    let src = "fn t.main() -> () uses [suspend] panics may
    let a = std.task.spawn_local(fn() -> u32 capture [] {
        7u32
    })
    let r = a.join_task()
    core.mem.drop(r)
end t.main
";
    let (_out, rc, err) = run_src("suspend_no_executor", src);
    assert_eq!(rc, Some(101), "stderr: {}", err);
    assert!(err.contains("no executor"), "stderr: {}", err);
}

/// `block_on` の入れ子は第 1 段では実行時の panic。
#[test]
fn nested_block_on_panics() {
    let src = "fn t.main() -> () uses [suspend] panics may
    std.task.block_on(fn() -> () capture [] {
        std.task.block_on(fn() -> () capture [] {
            std.task.yield_now()
        })
    })
end t.main
";
    let (_out, rc, err) = run_src("suspend_nested", src);
    assert_eq!(rc, Some(101), "stderr: {}", err);
    assert!(err.contains("already running"), "stderr: {}", err);
}

/// 実行器の外の `yield_now` は何もしない (木走査で実行器が無いときは戻るだけ)。
#[test]
fn yield_outside_executor_returns() {
    let src = with_say(
        "fn t.main() -> () uses [io.console, suspend] panics never
    std.task.yield_now()
    t.say(\"yielded\")
end t.main
",
    );
    let (out, rc, err) = run_src("suspend_yield_outside", &src);
    assert_eq!(rc, Some(0), "stderr: {}", err);
    assert_eq!(out, "yielded\n");
}

// ------------------------------------------------------------ 検査: 効果の伝播 (E4001)

/// `uses [suspend]` を持たない関数から中断する関数を呼ぶと E4001 (5.1 の効果の規則)。
#[test]
fn suspend_is_propagated_to_callers() {
    let codes = check_codes(
        "suspend_e4001",
        "fn t.main() -> () uses [] panics never
    std.task.yield_now()
end t.main
",
    );
    assert!(codes.iter().any(|c| c == "E4001"), "codes: {:?}", codes);
}

/// `block_on` の本体は中断できる。ただし `block_on` を呼ぶ側も `suspend` を宣言しなければならない (第 1 段の決定、5.6)。
#[test]
fn block_on_caller_must_declare_suspend() {
    let codes = check_codes(
        "suspend_block_on_caller",
        "fn t.main() -> () uses [] panics may
    std.task.block_on(fn() -> () capture [] {
        std.task.yield_now()
    })
end t.main
",
    );
    assert!(codes.iter().any(|c| c == "E4001"), "codes: {:?}", codes);
}

/// 中断を許す関数 (`uses [suspend]`) は、中断しない関数 (`uses []`) に渡せる (部分型の向きは 5.6 のとおり)。
#[test]
fn pure_fn_is_accepted_where_suspend_is_allowed() {
    let codes = check_codes(
        "suspend_pure_ok",
        "fn t.pure(x: u32) -> u32 uses [] panics never
    x
end t.pure

fn t.main() -> () uses [suspend] panics may
    let v = t.pure(3u32)
    let w = std.task.block_on(fn() -> u32 capture [] {
        t.pure(v)
    })
    core.mem.drop(w)
end t.main
",
    );
    assert!(!codes.iter().any(|c| c.starts_with("E4")), "codes: {:?}", codes);
}

// ------------------------------------------------------------ 検査: 中断をまたぐ借用 (E3050)

/// 中断点の後で使われる借用は E3050 (保守側の第 1 段の規則、5.6)。
#[test]
fn borrow_held_across_yield_is_e3050() {
    let codes = check_codes(
        "suspend_e3050",
        "fn t.main() -> () uses [suspend] panics never
    let x = 1u32
    let r = &x
    std.task.yield_now()
    let y = *r
    core.mem.drop(y)
end t.main
",
    );
    assert!(codes.iter().any(|c| c == "E3050"), "codes: {:?}", codes);
}

/// 中断を呼ぶ関数 (E3050 の経路を通る) の呼び出しも中断点。借用を持つ変数が後で使われると E3050。
#[test]
fn borrow_held_across_suspending_call_is_e3050() {
    let codes = check_codes(
        "suspend_e3050_call",
        "fn t.wait() -> () uses [suspend] panics never
    std.task.yield_now()
end t.wait

fn t.main() -> () uses [suspend] panics never
    let mut x = 1u32
    let r = &mut x
    t.wait()
    *r = 2u32
end t.main
",
    );
    assert!(codes.iter().any(|c| c == "E3050"), "codes: {:?}", codes);
}

/// 借用を、中断点より前で使い終えていれば通る。引数として渡す借用も、呼び出しの間だけなので通る。
#[test]
fn borrow_dead_before_yield_is_ok() {
    let codes = check_codes(
        "suspend_borrow_ok",
        "fn t.bump(p: &mut u32) -> () uses [suspend] panics never
    std.task.yield_now()
    *p = 2u32
end t.bump

fn t.main() -> () uses [suspend] panics never
    let mut x = 1u32
    let r = &x
    let y = *r
    t.bump(&mut x)
    core.mem.drop(y)
end t.main
",
    );
    assert!(!codes.iter().any(|c| c.starts_with("E3")), "codes: {:?}", codes);
}

// ------------------------------------------------------------ ネイティブ生成 (第 1 段は退避しない)

/// `ksn build --release` は、中断を木走査へ退避せず E9040 で失敗する (第 1 段はネイティブ未対応、設計_KIR.md)。
#[test]
fn release_native_build_refuses_std_task_with_e9040() {
    let t = Tmp::new(
        "suspend_native_release",
        "fn t.main() -> () uses [suspend] panics may
    std.task.block_on(fn() -> () capture [] {
        std.task.yield_now()
    })
end t.main
",
    );
    let o = run(&["build", "--release"], &t.0);
    let out = text(&o.stdout) + &text(&o.stderr);
    assert!(!o.status.success(), "{}", out);
    assert!(out.contains("E9040"), "{}", out);
}

/// 同じ名前 `join` の `std.thread.JoinHandle.join` (中断しない) の上の借用は、中断点ではないので通る。
#[test]
fn thread_join_with_live_borrow_is_not_a_suspension_point() {
    let codes = check_codes(
        "suspend_thread_join_ok",
        "fn t.main() -> () uses [alloc, thread] panics may
    let x = 1u32
    let r = &x
    let h = std.thread.spawn(std.alloc.SYSTEM, fn() -> u32 capture [] {
        7u32
    })
    match h {
        Ok(hh) => {
            let v = hh.join()
            core.mem.drop(v)
        }
        Err(_e) => ()
    }
    let y = *r
    core.mem.drop(y)
end t.main
",
    );
    assert!(!codes.iter().any(|c| c == "E3050"), "codes: {:?}", codes);
}
