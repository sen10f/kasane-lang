### 2.10?? const / static の初期化式は効果を持てない (X1)

`ksn check` は `const` / `static` の初期化式を評価する (4.1、E4024) ので、初期化式から効果のある関数を呼べると検査しただけで任意のプロセスが動いた (Mythos X1)。二重に塞ぐ。
1. 検査器: 初期化式に `const fn` の規則 (`in_const_fn`) を適用。`uses` が空でない関数 (利用者の関数・組み込み・`.sig` の宣言) の呼び出しは E2038、`const fn` 自身も `uses []` でなければ E2038。
2. 評価: `Interp::const_guard` を初期化式の評価中だけ立て、`uses` が空でない関数・メソッド・組み込み、`extern` の呼び出しを止める (E4024)。LSP・`ksn check` が通る `eval_const_inits` の第二の防御線。
試験: `unsound/constinit_*`、`check_diag.rs` の `check_never_runs_effects_of_const_initializers` (検査が副作用のファイルを作らない)。
