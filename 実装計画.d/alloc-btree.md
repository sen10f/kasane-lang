# alloc-btree: 入口を主スレッドのままスタックだけ入れ替える

- 原因: 生成した `main` が別スレッド (`pthread_create`) で本体を動かしていたため、glibc の `malloc` が非主アリーナ (確保ごとに原子操作の錠) を使い、確保の多い binarytrees が 1.4 倍ほど遅かった (C でも 5.1 s → 7.5 s)。
- 直し: 64 ビット Linux (glibc。ホストか `*-linux-gnu`) では `mmap` した `main_stack_bytes(depth)` のスタック (最下ページは `PROT_NONE` の guard) へ `makecontext` / `swapcontext` で乗り換え、主スレッドのまま走る。スタックの床 (`ksn.floor`) は新しいスタックの SP から求め、深さの上限 (終了コード 102) は従来と同じ。`mmap` に失敗したら主スレッドのスタックで直接走る (従来の退避と同じ)。
- 従来のスレッドの経路のまま: Windows / macOS / wasm / ASan・TSan (`KSN_CLANG_ARGS` に `-fsanitize*`。fiber の注釈を足すまでは、スタックの入れ替えが偽の報告を出すため)。
- `KSN_THREAD_MAIN=1` (ビルド時): 従来のスレッドの経路を選ぶ。実行時の配置の選択だけで、深さ・スタックの検査は変わらない。
- 確認: depth_limit / depth_explicit (巨大フレーム) / kir_inline_depth / process / threads / native_vs_interp (差 0) / gen.py 100 / パニック 101 と `std.process.exit` (深い再帰の中から) が stdout を流して正しい終了コード。
- 結果: binarytrees (depth 18) の user 9.4 s → 約 6.0–6.7 s。残りは `Option<Box>` の枠 (ノード 32 バイト対 16) と呼び出しごとの深さの検査。
