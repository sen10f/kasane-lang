# async-mt: 複数スレッドの実行器 (非同期ソケットの第 2 段)

ブランチ `task/async-mt`。設計は `設計_多重化したネット.md` 9〜17 章。

## 状況

- 手順 1 (設計): 済み。`block_on_threads(allocator, n, f)` + `spawn[F: Send]`、枠の `Send` は E2074 (新設)、スケジューラの核は `sysrt.c`。仕様との衝突は無い。
- 手順 2a (検査・`.sig`・木走査と KIR の決定的な代替): 済み。枠の `Send` は E2074 (E2041 は `let _ = e` の禁止で使用済みだったので替えた)。`std.task.spawn` / `block_on_threads` の閉包は E2040 (取り込み・結果) と E2037 (`static` 境界) を受ける。`n` と allocator は木走査・KIR では無視する。
- 手順 2b (ネイティブの実スレッド実行器) 以降: 未着手。
