### 2.10?? const-eval / 汎用呼び出し結果の呼び出しの robustness 修正 (P1, P2, P3)

- P1 (check/sec2-const-eval): `t.id(t.eff)()` のように、呼び出しの結果をそのまま呼ぶ式で `Program::resolve` が空の `segs` に対して `segs[0]` にアクセスし panic (rc=2) していた。`resolve` で `segs` が空のときは `None` を返すようにし、段階 1 の「式を呼ぶ形は段階 1 の対象外」という通常の扱いに任せる (`stage0/src/program.rs`)。`let f = t.id(t.eff); f()` の E4001 はそのまま。
- P2: 定数評価の `[v; n]` 配列反復の要素数 `n` をステップ上限 (1,000 万) に数え入れず、巨大な配列 (例 `[0u64; 4_000_000_000]`) が上限をすり抜けて巨大メモリを確保し abort していた。`ArrayRep` で `n` を `step_limit` に加算し、超えたら `Ctl::Panic(STEP_LIMIT_MSG)` (E4024) を早く出す (`stage0/src/interp.rs`)。
- P3: `const` / `static` の初期化式の評価がスタックオーバーフロー (`Ctl::Exit(102)`) で止まっても、`ksn check` / `ksn build` が診断 0 件・rc=0 だった。`eval_const_inits` で `Ctl::Exit` も `Ctl::Panic` と同じく E4024 診断 (初期化式が終わらなかった) として出し rc=1 にする (`stage0/src/cli.rs`)。

試験: `stage0/tests/sec2_const_eval.rs` に P1/P2/P3 それぞれ 1 本ずつ追加。`cargo test --release --test sec2_const_eval` および `cargo test --release --test check_diag` で回帰なし。
