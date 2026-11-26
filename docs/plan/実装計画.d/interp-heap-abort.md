# 木走査の heap 破壊 (`malloc_consolidate(): unaligned fastbin chunk`) の修正

- 症状: `ksn run --engine=tree` で、タスクの取り消しを含む例が 1/40〜1/300 で abort。
- 原因: `task.rs` のタスクのスレッドで、`match body { Some(b) if !cancelled => .. }` は、始まる前に取り消されたとき `b` を move しない。
  本体の `Val` (`Rc`) が `body` に残り、スレッドの終了 (= 合図を渡したあと) に落とされる。非原子の参照数・解放が、合図を得た別スレッドと競合した。
- 直し: `match &body` にし、合図を渡す前に `drop(body)`。
- 監査: `task_cancel` / `task_switch` の生ポインタ、`interp.rs` の `run_entry` (join で待つので安全)、`natives.rs` の `alloc` 系は同種の誤りなし。
  原則: 合図を渡したあとのスレッドは `Val` を持たない・落とさない。
- 試験: `stage0/tests/task_cancel_heap.rs` (未開始の取り消しを 60 回走らせる)。旧バイナリは 300 回中 1 回 abort、修正後は 0。
