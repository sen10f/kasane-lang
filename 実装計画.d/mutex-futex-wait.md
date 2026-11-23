# rt/mutex-futex-wait (2026-11-24)

- `ksn.thr.mlock` / `munlock` (LLVM の下ろし、`kir_llvm.rs`) の遅い道を、大域の `mu` + 条件変数から、Mutex の語そのものの futex の待ちに替えた (Drepper の 0/1/2)。待ちは `ksn_rt_sys_futex_wait(語, 2, 20ms)` (`sysrt.c`、Linux は `FUTEX_WAIT_PRIVATE`)、解放は旧い値が 2 のときだけ `FUTEX_WAKE_PRIVATE` 1 つ。速い道も遅い道も大域のロックは取らない。
- 全員待ちの検出は時間切れ (20ms) のときだけ: 初めての時間切れで大域の `mwait` (アトミック) に数え、`mu` の下で `blocked + mwait >= live` を見る。真なら語をもう一度 xchg で取りにいき (取れたらそのまま持つ)、取れなければ従来の文言で 101。読んだ後に語を取りにいくので、解放の直後に別スレッドが待ちに入る競合の偽陽性はない。同じスレッドの単独の再ロックは初回の時間切れ (約 20ms) で 101。
- 非 Linux (Windows ほか): `futex_wait` は `ms` 眠って「時間切れ」を返し、`futex_wake` は何もしない (待ちが遅いだけで動きは保つ)。wasm32 はスレッド未対応のまま。
- 試験 (`rt_sync.rs`): `rt_sync_mutex_stress_16_threads_exact_total` (16 x 20 万 = 320 万ちょうど)、`rt_sync_contended_relock_is_a_deadlock_101_native` (待ちが 3 本いる中の再ロックが 101)。既存の 5 本は変更なし。
- 計測 (同じ静的 Mutex の 2M 回/スレッド、プロセス CPU 時間 / 操作数、3 回の中央値、ns/op、1/4/8/16 スレッド)。マシンの負荷が高く (load 22〜26 / 32) 絶対値はぶれる。同じ条件の 2 回:
  - 前: 29/328/1450/4591、24/354/804/3558
  - 後: 28/309/642/1577、23/285/698/1555
  - Rust std (同時): 24/366/119/2229、22/312/872/2185
  - 静かなマシンでの再計測は未 (前の公平な計測の Rust は 20/105/145/148)。
