# std/spawn-error-closure-2 (O-08)

- `std.thread.spawn` / `spawn_deep` の失敗は `Err(std.thread.SpawnError<F>)` (`code: i32`・`closure: F`、`derive [Debug]`)。閉包が線形の値を取り込めば `SpawnError<F>` も線形で、`Err(_)` で捨てると E3003。
- 木走査: 試験用の旗 `KSN_TEST_SPAWN_FAIL` で失敗を作る。KIR (VM・ネイティブ) の `spawn` は失敗しない (閉包の値が KIR に無い) ので旗は見ない。`Err` の腕の `task()` は KIR では到達不能として下ろす。
- `std.task.spawn` / `spawn_local` / `block_on_threads` は `Result` を返さないので対象外。
