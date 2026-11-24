# async-mt: 複数スレッドの実行器 (非同期ソケットの第 2 段)

ブランチ `task/async-mt`。設計は `設計_多重化したネット.md` 9〜17 章。

## 状況

- 手順 1 (設計): 済み。`block_on_threads(allocator, n, f)` + `spawn[F: Send]`、枠の `Send` は E2074 (新設)、スケジューラの核は `sysrt.c`。仕様との衝突は無い。
- 手順 2a (検査・`.sig`・木走査と KIR の決定的な代替): 済み。枠の `Send` は E2074 (E2041 は `let _ = e` の禁止で使用済みだったので替えた)。`std.task.spawn` / `block_on_threads` の閉包は E2040 (取り込み・結果) と E2037 (`static` 境界) を受ける。`n` と allocator は木走査・KIR では無視する。
- 手順 2c (仕様 0.10.40・チートシート): 済み。
- 手順 3 (試験): `stage0/tests/net_async_mt.rs` (256 接続 echo と全バイトの確認、10 万の小タスクの合計が厳密、Send の拒否 4 件、木走査 == ネイティブ)。10 万タスクの試験で、1 スレッドの実行器の `pick` / `first_undone` / `free_from` が再帰で書かれていて stack overflow (102) になる不具合が見つかり、ループに直した (`kir_suspend_exec.kir`)。
- 手順 2b (ネイティブの実スレッド実行器): **未着手**。今のネイティブの `block_on_threads` は `n` を無視して 1 スレッドの実行器で動く (結果は木走査と同じ)。測定 (手順 3 の後半) も 2b の後。

## 2b の方針 (調べた結果。設計 11 章を次のように具体化する)

- 実行器の状態 `ksn.task.g` (`global.bytes` 56 バイト) とタスクの表は、今はプロセスに 1 つ。ワーカーごとに持つため、LLVM の下ろしに **スレッドごとの大域 (`thread_local`)** を足す (`global.bytes` の TLS 版)。これで既存の 1 スレッドの実行器を、ワーカーごとにそのまま走らせられる。リアクター (epoll) は既にスレッドごとなので、タスクが始まったワーカーに居続ける限り共有の epoll は要らない (設計 11 章の「共有のリアクター」より単純)。
- 横取りは **まだ始まっていないタスクだけ**: `spawn` は全体の注入列 (`sysrt.c`、Mutex + futex) に (tag, 枠) を積み、ワーカーは走れる局所のタスクが無いときと 61 回に 1 回注入列から取って自分の表へ登録する。始まったタスクの枠はそのワーカーから動かない (枠の `Send` は将来の移動に備えた規則で、いまは厳しめ)。
- `join_task` は **全体の結果の表** (id → 終了・枠・待ち手) を通す。待たれる側が終わったら待ち手のワーカーを eventfd で起こす。取り消しは、別のワーカーで走行中なら印だけ立てて次の中断点で抜ける。
- 全員待ち: 眠る直前に `idle == n && 注入列が空 && 待つ fd と期限が無い` を数える。眠りは `epoll_wait` (eventfd つき) で、futex は使わない。
- 参照: 木走査・KIR の参照実行器は今のまま (決定的、`n` を無視)。Windows / wasm32 は E9040。


## FAST GATE の結果 (2026-11-24、機械の負荷が高い)

- `check_all` 通過、`verify_reference` bad 0、`native_ratio` linear_ok 187/187・examples 18/19 (ui_demo は `ui/native/libksnui.a` が worktree に無いだけ)・perf 8/8 (床どおり)。`check_diag` は 1 本 (`trait_side_impls_work_for_foreign_types_in_run_and_llvm_build`) を除いて 308 件通過、`suspend` / `suspend_native` / `net_async_mt` 通過、パッケージの `ksn check` は全て診断 0。
- `par_test.py --tier fast` は**最後まで回らなかった**。共有のビルドキャッシュ `~/.ksn/cache` (2199 件) が重くて、`ksn build` がどの束でも (dev/plan のバイナリでも) 1 分以上かかる (専用の `KSN_CACHE_DIR` なら 4 秒)。fix/cache-limit の件で、この枠の変更とは無関係。キャッシュを片付けたあとで回し直す。
