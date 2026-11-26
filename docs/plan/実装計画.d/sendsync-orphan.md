# check/sendsync-orphan (第 1 波: H1・H2・H4)
- 手動 Send/Sync は型を定義したモジュールだけ (E5010)。標準の型は標準ライブラリ内だけ。二重は E1114、全欄が数値の型への impl は E5011 (判定は保守的。総称・参照を含む型は見ない)。
- E5002 が出た impl には重ねて出さない。診断枠は単一ファイルなので、試験は `check_diag.rs` の `send_sync_impl_site_rules` (パッケージ)。
