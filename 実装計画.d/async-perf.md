# async-perf: 複数スレッドの実行器の高速化 (途中)

済み (sysrt.c のみ。KIR・インタプリタは不変):
- 全体の mutex を外した経路: epoch・unfinished を atomic に、注入列が空なら `mt_take` は mutex を取らない (qlen)、`bump` は cond で眠る者 (nsleep) がいるときだけ mutex を取る。
- epoll で待つワーカーは eventfd で起こす (bump が全ワーカーの eventfd に書く)。`io_poll(2)` の 1 ms の上限を廃止 (eventfd が作れないときだけ 1 ms)。
- 期限つきの待ちは fd の一覧 (`ksn_io_dl`) だけを走査し、`ksn_io_tab` の全表走査をやめた。

未了: `ksn.task.pick` の O(n) 走査 (用意リスト化)、`io_wake` の全タスク走査 (fd からタスクへの直接の対応)。set_state が各所に散っているため、
状態 0 への遷移を 1 か所に集めてキューに積む作りが要る (join 完了・cancel_flag は事象でなく条件の判定なので別途)。
測定 (256 接続 × 400 往復、load 60〜70 で絶対値は不安定): 旧 async-1 15〜19k / async-4 20k / 新 async-1 20.5k / async-4 20.5〜22.6k / async-16 13〜21.6k req/s。
