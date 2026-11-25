# s6-kir-run: `ksn run` / `ksn test` を KIR (VM) で走らせる (S6、ブランチ `vm/s6-kir-run`)

目的: 木走査 (`interp.rs`) ではなく KIR → VM (`vm/`) を既定の実行系にする。出力・終了コード・panic の文言は同じ。

## M1 計測 (2026-11-25、dev/plan `76f3dc12`)
- 既存の `perf/vm_readiness.py` の結果: examples 18 本は run / test とも VM と木走査が一致。perf は VM が fib 7.6x・loops 14x・vecops 28x 速いが、noop は 58 ms (木) 対 420 ms (VM)。
- 原因: `lower_program` が標準ライブラリを全部下ろしていた (`ll_types` 336 ms + `ll_fns` 226 ms)。入口から呼ばれる先だけを下ろす遅延の下ろし (`lower_program_lazy` / `lower_test_lazy`) にして noop は数 ms。
- VM の食い違い 2 件を直した: (1) 仮想のファイル記述子が 0 から始まり 3 つ目のファイルが fd 2 (標準エラー) に化けた (VM と `kir_exec` は 3 から)。(2) `block_on_threads` の `spawn_local` 禁止が VM に無い → 当面は VM 未対応として木走査へ退避。

## M2 既定の切り替え
- `--engine=vm|tree|both`、`KSN_ENGINE=tree|vm`。既定は VM で、下ろせない入口は黙って木走査へ (明示 `--engine=vm` のときだけ理由を stderr に出す)。`--watch` は VM を明示しない限り木走査 (ホットリロード)。デバッガは今までどおり別実装。
- 比較: `python3 -I tests/engine_compare.py` (linear_ok・check_ok・data・examples・perf・bench と `--extra` の生成器の出力。tree 対 vm の標準出力・終了コード・panic 行)。
