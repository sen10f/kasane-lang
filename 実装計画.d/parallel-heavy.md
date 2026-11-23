# テストの並列化 (parallel-heavy)

- 担当: 1 試験の中のコーパスの loop を `common::par_map` (既存、KSN_TEST_PAR 既定 8) で並列に回す。結果は入力の順、失敗は項目名つきで同じ文面。
- 対象: check_diag (diag / unsound / linear_ok / check_ok の 4 本)、check_borrow (linear_corpus・linear_ok)、tast_borrows (STEMS・corpus)、tast_lifetimes (CASES・linear_ok)。
- 未対象: rephrase (生成した変種を畳む形で loop の形が違う)、kir_lower*/wasm*/cross (今回は手をつけず)。
- 計測 (--test-threads=1): check_diag 555 s (前 1762 s、負荷下)、check_borrow 133 s (前 760 s)、tast_borrows 56 s (前 916 s)、tast_lifetimes 46 s (前 741 s)。
- slow_tests.txt: check_diag・check_borrow を追加 (60 s 超)。
