### 2.10?? `ksn fix` が編集距離の「did you mean」を自動適用しない（セキュリティ修正 B3）

- `fixes.rs` の E1108 (`unknown method \`X\`; did you mean \`Y\``) と E1101 (`unresolved name X; did you mean Y?`) から、編集距離の当て推量をソースに当てはめる fix 生成を削除した。
- 診断の文面（`did you mean` の案内）はそのまま残る。`cli.rs` の `("ksn fix` で直せる)" 注記は fix が空のときに付かないので、これらの診断から外れる。
- 意味が同じと言い切れる書き換えだけが fix の対象 (`to_<整数型>` → `from_<整数型>(x)` の無損失変換、rustisms、`as` キャスト拡大などは従来どおり)。
- 試験: `stage0/tests/fmt_fix.rs` に 3 本 (trailing_ones→trailing_zeros、success→successor、core.cmp.max→core.cmp.Eq)。各々 `ksn fix` 後はファイル不変、`ksn check` はまだエラー。
