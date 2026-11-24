# check/sendsync-bounds (第 2 波)

- H5: `tast_rules/bounds.rs` が Send/Sync の境界を `TypeCk::marker_in` (spawn と同じ auto_marker) で検査 (E2030)。構造体リテラルの型引数も。クロージャの取り込みは `side.closure_caps` から引く。
- H3: `check.rs` の手動 Send/Sync impl に E5008 (型パラメータの境界)。
- `unsound/sendbound_*`・`send_impl_unbounded_generic`、`diag/E5008`、explain。
