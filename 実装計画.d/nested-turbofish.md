### 2.10?? E2067 の修正対象を `::<` の列で特定する（ネスト・同一行複数、2026-11-22）

- `tast_rules/oneway.rs` の E2067 は、診断に `::<` の 1 始まりの列 (`col`) を乗せる (`diag` が `col` を取る。`found` は `(line, col, code, msg)`)。同じ行に複数の `::<` があるとき、歩行順で数えた `turbo_seen` からその呼び出しの `::<` が何番目かを求め、`nth_turbofish_col` で列を特定する。他の `found.push` は従来どおり `col = 0`（列不明）。
- `fixes.rs` の `oneway_fix` は `col` パラメータを受け取り、E2067 は `col > 0` のときその列から始まる `::<` を外す。`col == 0`（列不明）の従来の挙動は `::<` が 1 つだけの行のみ直す（`::<` が複数なら直さず診断だけ残す）。
- `apply` はスパンを (start,end) でソートし、重複・無効をスキップするので、列を正しく指定すれば同一行の複数 `::<` を安全に外せる。
- 試験: `stage0/tests/fmt_fix.rs` に `oneway_turbofish_nested_on_one_line_is_removed` を追加。同一行にネストした 2 つの `::<`（内側のみ引数で型が決まる）があり、`ksn fix` で内側だけを外し、`ksn check` が通り、2 回目は変更なし。
- 試験の入力・期待値を他の試験で書き換えず、`examples/`、`perf/`、`bench/tasks/*/reference.ksnl` は変更なし。
