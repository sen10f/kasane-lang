# async-perf: 複数スレッドの実行器の高速化 (途中)

済み (sysrt.c のみ。KIR・インタプリタは不変):
- 全体の mutex を外した経路: epoch・unfinished を atomic に、注入列が空なら `mt_take` は mutex を取らない (qlen)、`bump` は cond で眠る者 (nsleep) がいるときだけ mutex を取る。
- epoll で待つワーカーは eventfd で起こす (bump が全ワーカーの eventfd に書く)。`io_poll(2)` の 1 ms の上限を廃止 (eventfd が作れないときだけ 1 ms)。
- 期限つきの待ちは fd の一覧 (`ksn_io_dl`) だけを走査し、`ksn_io_tab` の全表走査をやめた。

済み (2 回目): 用意リスト (FIFO の輪) と fd→タスクの直接の対応。
- `ksn.task.g` を 104 バイトに (`[56]` 輪、`[64]`/`[72]` 先頭/末尾、`[80]`/`[88]` join 待ちの出入りの数、`[96]` pick の回数)。各タスク枠の `+4` は「輪に載っている」印。
- `set_state` が唯一の入口: 状態 0 への遷移で輪に積む (`rq_push`)、状態 2/3 への遷移で join 待ち (状態 1 で target が一致) を起こす (`wake_waiters`)。
  spawn は積む。pick は「直前に走った (カーソルの) タスクが状態 0 なら (yield)」積んでから先頭を取る。状態 4/5 への遷移では起こさない (早すぎると wait.poll が結果を読んでしまう)。
- `pick` は O(1): 輪の先頭 (状態が 0 でなければ捨てる)。join 待ちがいるとき (出入りの数が違う) は 64 回に 1 回と輪が空のときに `wake_blocked` (状態 1 を全部見て、target 完了 / 取り消しの印があれば起こす。別スレッドが終えた join 先はここで拾う)。
- `io_wake` は全タスク走査をやめ、C 側の「印が付いた待ちのタスク番号の列」(`ksn.io.nfired` / `ksn.io.next`、`park` の直後に `ksn.io.bind(fd, task)`) だけを見る。非ネイティブ (インタプリタ・wasm) の本体は何もしない (park しないので到達しない)。
- 注意: `panics=never` の関数に `rem.usize.trap` を入れると、コンパイラが無限ループする (原因は未調査)。輪の添字は `and` (容量は 8×2^k)。
