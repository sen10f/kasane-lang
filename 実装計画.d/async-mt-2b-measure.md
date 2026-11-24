# 256 接続の echo の測定 (2026-11-24、クライアントは C の 256 スレッド、1 接続 400 往復 × 200 バイト、全部 127.0.0.1)

機械の負荷が非常に高い (`uptime` の load average 約 100、32 スレッド): 絶対値は 3 倍ほど落ちていて、並びと比だけを見る。3 回の範囲。

| サーバ | req/s |
| --- | --- |
| Kasane 非同期 1 スレッド (`block_on` + `spawn_local`) | 12.8k〜15.1k |
| Kasane 非同期 4 ワーカー (`block_on_threads`) | 19k〜45k (別の回: 23k〜26k) |
| Kasane 非同期 16 ワーカー | 27k〜38k |
| Kasane スレッド 1 接続 1 本 (`std.thread.spawn`) | 123k〜129k |
| Rust (`std::net` + スレッド 1 接続 1 本、`rustc -O`) | 144k〜177k |

非同期は 1 スレッドの約 2〜3 倍まで伸びたが、スレッド 1 接続 1 本の Kasane の約 1/4 で、まだ遅い。原因の見立て (未測定): リアクターの 1 要求あたりの syscall (`recv` の EAGAIN → park → `epoll_wait` → 再試行 → `send`) とタスクの枠の確保、全体の mutex の競合。次の手は strace / perf での内訳。
