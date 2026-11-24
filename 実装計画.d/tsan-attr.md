# kir/tsan-attr: TSan の sanitize_thread 付与 (hunt3 T1)

- `KSN_CLANG_ARGS` に `-fsanitize=thread` があるとき、生成する全ての `define` に `sanitize_thread` を付ける (`sanitize_address` と同じ経路)。`-fsanitize=memory` の経路は kir_llvm に無いので対象外。
- 陽性対照 `tsan_positive_control_reports_a_deliberate_race` を追加 (`stage0/tests/data/threads/tsan_race_control.ksnl`、hunt3 の H5 と同じ生ポインタの競合、`[unsafe] modules = ["t"]`)。TSan が "WARNING: ThreadSanitizer" を出さなければ失敗する。通過。
- 付けたら既存の TSan 試験で本物の競合が出た (2 本が失敗、直さず報告):
  - `kir_native_threads_have_no_data_races_under_threadsanitizer` (channel_clone)
  - `kir_native_pool_guards_and_deadlock_have_no_data_races_under_threadsanitizer` (pool_shared)
  - 競合の場所: グローバル `@ksn.rt.live` と `@ksn.rt.blocked` (kir_llvm.rs の `k.ksn.chan.rt.wait` の検査ループ ~378 行で、`ksn.rt.mu` を持たずに `load atomic ... monotonic` する) と、`ksn.rt.tramp` / `k.t.spawn_one` / `ksn.thr.unlock` の平文の `store` (`ksn.rt.mu` の下) が、互いに同期しない。
  - 直し方の候補 (未実施): 書き手も読み手も `atomic ... monotonic` にそろえる (`store atomic`)、または検査ループの読みを `ksn.rt.mu` の下に移す。
