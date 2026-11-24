# rt/chan-futex: チャネルごとのロックと待ち (ネイティブ)

- 前: 大域の 1 つの mutex + 条件変数 (`ksn.thr.lock/unlock/wait`)。`unlock` ごとに `gen` を進め全員を `broadcast`。全員待ちの検出は大域の `blocked >= live`。
- 後: 状態 (152 バイト) に チャネルごとの `pthread_mutex_t` (+48、アダプティブ) / `pthread_cond_t` (+88) / 待ち数 (+136) / 起こしの札 (+144)。
  `ksn.chan.rt.{init,lock,unlock,unlock1,unlockall,wait}` (実行器では no-op、`wait` は保留スレッドを 1 本走らせる。ネイティブは kir_llvm.rs が差し替え)。
  send は `unlock1` (待ちがいて、キューの件数が札より多いときだけ 1 本起こす)、端が落ちる/close は `unlockall`。
  待ちは 100ms の `cond_timedwait`。全員待ちは `cblk (札なしで待つ数) + blocked >= live` (待ちに入るとき・期限切れのたび)。起こす側が札を足して `cblk` を引くので `cblk` は正確。
- 容量は従来どおり無視 (送りは待たない)。Mutex の経路 (`mlock` など) には触れていない。
- 試験: `threads.rs` に 16 送り手 × 10 万件 (合計と送り手ごとの FIFO)。deadlock は既存の `deadlock_join` / `deadlock_mutual`。
