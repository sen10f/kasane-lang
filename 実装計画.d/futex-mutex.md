# rt/futex-mutex (2026-11-23)

- `std.sync.Mutex` (static) のネイティブ: 値ごとの 32 ビットの語 (0 空き / 1 持たれている / 2 待ちがいる) の CAS が速い道。取れなければ 100 回回ってから、大域の `mu` の下で語を 2 にして条件変数で待つ (全員待ちの検出は従来どおり `blocked`/`live`/`gen`)。解放は語を 0 にし、旧い値が 2 のときだけ `mu` の下で起こす。
  `static_lock` の初期化判定は旗のアトミックな読みで済ませ、初期化前だけ大域のロックを取る (従来は毎回 lock/unlock + broadcast していた)。
- 再帰ロックは不可になった (旧: 再帰 mutex。同じスレッドの 2 回目の lock は全員待ちの deadlock として 101)。
- `static` の `core.sync.Atomic*`: 大域領域 16 バイト (`+0` 旗、`+8` セル)。`ksn.atm.*` を使う。
- `std.thread.sleep` / `Instant.now|elapsed|since` / `Duration.from_millis|as_nanos`: `ksn.time.mono` / `ksn.time.sleep` (LLVM は `ksn_rt_sys_now_ns` / `ksn_rt_sys_sleep_ms`、KIR 実行器は `TimeNowNs` の待ち回し)。wasm32 は未対応。
- 試験: `stage0/tests/rt_sync.rs`。
- 計測 (1 操作あたり、全体の時間 / 操作数): 前 406 / 1719 / 3547 / 4187 ns → 後 111 / 43 / 43 / 77 ns (1 / 4 / 8 / 16 スレッド)。
