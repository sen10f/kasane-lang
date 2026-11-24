# 型検査の逃げ道の廃止 (check/remove-no-strict-types)

- 決定 (オーナー、2026-11-24): E2044 は常に有効。移行用の `--no-strict-types` と `KSN_STRICT_TYPES=0` は廃止。
- 実装: `cli.rs` の `main_cli` で、これらが付けば終了 2 と廃止の文面を出す (黙って無視しない)。`--strict-types` / `KSN_STRICT_TYPES=1` は 1 リリースだけ受け付けて何も変えない。
- `types.rs`: `strict` フィールドと `strict_types()` / `set_strict_types()` / `STRICT_OFF` を削除。`strict` のための分岐 (`report_undetermined`・`infer_site`・束縛の site 登録) は常に通る形にした。
- 試験: `kir_native.rs` の `strict_types_is_always_on_and_escape_hatches_are_removed` を、廃止の終了 2 と文面を確かめる形に変えた。
- 文書: AGENTS.md・実装計画.md 4.3 行とその履歴 (注記を追記)・設計_増分検査.md・.env.example・explain の E2044・CHANGELOG 2.10dj。
