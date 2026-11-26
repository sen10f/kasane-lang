# apps/notes 摩擦記録

> **再確認 (2026-11-26、dev/plan `0cec620a`)**: 2 件とも解消 (derive の `to_json`、`&self.notes` を返す形が通る)。`note_to_json` を `to_json` に、`get`/`next_id` を `iter` に戻した。`ksn test --both` 9 件 llvm 一致。 詳細は `apps/FRICTION_recheck.md`。

本パッケージ (`apps/notes`, CLI ノート帳) を実装する上で、想定通りに動作しなかった点や回避策を要した点を記録する。

## 摩擦の件数: 2 件 (処理系の不足)

統括者の注: 担当の初版は、自分のコードの誤り 2 件 (`cmd.edit` の引数の数、`match` の腕の外での束縛の使用) を摩擦に数えていた。言語・処理系の書きにくさではないので外した。

1. **`std.json.to_string` がネイティブに下りない (E9040)**
   `ksn build --release` で退避する。`notes.store.note_to_json` と `notes.cmd.write_json_string` で、フィールドごとに手で JSON を組んで回避した。

2. **`&self.notes` (フィールドの `Vec` への参照) を返すメソッドが E9040**
   `all_notes()` が `&self.notes` を返すと、ネイティブの下ろしが「フィールドの移動」として退避する。`all_notes_len()` / `get_at()` に分け、添字で取り出して回避した (`store.ksnl`)。

## 動作確認

`ksn check` / `ksn fmt --check` / `ksn test` (9 件, 0 失敗) / `ksn build --release` すべて通過。`add`/`list`/`show`/`edit`/`rm`/`tag`/`search`/`export` (JSON+MD) を手動で動作確認済み。
