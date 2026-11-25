# 実装計画.d (task/m4-cross-thread-task)

### 2.10?? K1: 別スレッドの実行器へ持ち込んだ `Task` の検出 (ネイティブ実行時の型の取り違え)

- **穴** (mythos4-vuln K1): ネイティブの実行器のセッション番号 (`ksn.task.g` の `+48`) がスレッド局所で 1 から
  数えられていたため、あるスレッドの `block_on` で作った `Task` の取っ手を別スレッドの `block_on` へ
  持ち込むと世代が一致して検査をすり抜け、`join_task` が別のスロットの結果を読み (型の取り違え・
  SIGSEGV・ヒープ番地の漏れ)、release native がメモリを壊した。木走査は正しく panic (101)。
- **直し** (根: セッション番号をプロセス全体で一意にする):
  - `sysrt.c`: プロセス全体の原子カウンタ `ksn_task_sess` と `ksn_rt_sys_task_session()` を追加。
    `block_on_threads` 実行中 (Linux) は共有の核のセッション番号 `ksn_mt.sess` (`mt_begin` ごとに同じ
    カウンタから採番) を返す。
  - `kir_suspend_exec.kir`: `ksn.task.run.new` は `+48` に `ksn.task.session()` の返り値を入れる
    (KIR のスタブは従来どおり `+48 + 1`。wasm32 は単一スレッドなのでスタブのまま)。
    `ksn.task.worker_main` は `+48` にも `ksn.task.session()` を入れ、`block_on_threads` のワーカーが
    根と同じセッション番号を取っ手に刻むようにした。
  - `kir_llvm.rs`: `ksn.task.session` を `ksn_rt_sys_task_session()` の呼び出しに差し替える
    (wasm32 は `sysrt.c` を持たないので KIR のスタブのまま)。
  - `ksn.task.cancel` は世代の検査を marked (mt) の分岐より先に行い (木走査と同じ順序・文面)、
   ついでに番号の範囲の検査より先にも行う。
- **試験**: `stage0/tests/data/k1_cross_thread/` に K1a (Vec の取っ手でタプルの結果を読む)、K1b (逆向き)、
  `block_on_threads` の取っ手を別スレッドの `block_on` で join する形の 3 本。
  `net_async_mt.rs` の `both_panic` で木走査と release ネイティブの両方が同じ文面・終了コード 101 になることを検査。
  `tests/native_vs_interp.py` は data/ の任意のディレクトリを group に取れるようにし、
  `--groups k1_cross_thread` で ASan 付き (`--clang-args=-fsanitize=address`) でも回せる。
  `Task.cancel` の別セッションの取っ手は検査が E4022 で静的に拒むので、実行時の世代検査は
  防衛のためだけ入れた ( accepted なコードからは unreachable)。
