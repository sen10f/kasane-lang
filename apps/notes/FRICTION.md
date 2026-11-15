# apps/notes 摩擦記録

本パッケージ (`apps/notes`, CLI ノート帳) を実装する上で、想定通りに動作しなかった点や回避策を要した点を記録する。

## 摩擦の件数: 4 件

## 上位 3 件

1. **`notes.cmd.edit` の引数数とディスパッチ側の呼び出しが一致しない**
   `edit` は `<id> <title> <body>` の 3 引数を取るが、`notes.ksnl` のディスパッチが `cmd.edit(1usize, 2usize)` と 2 引数しか渡しておらず `E2001` (argument count mismatch) になる。`cmd.edit(1usize, 2usize, 3usize)` に修正。

2. **`edit` の `match` アームが空で `memo` がスコープ外になる**
   `notes.store.Store.get(&db, id)` の `Some(memo)` アームが空 (`{}`) で、アームの外で `memo.tags.as_slice()` を参照するとスコープ外 / 未解決 (`E1020` 未使用のパターン束縛、`E1101` 未解決名) になる。`tag` と同様に、アーム内でタグを複製し `note.Note.new` で再構築して `Store.set(&mut db, id, updated)` までを行うよう再構成。

3. **`std.json.to_string` が KIR ステージ 1 で未対応**
   `std.json.to_string` を使った JSON シリアライズが `E9040` でネイティブビルドに失敗。`notes.store.note_to_json` と `notes.cmd.write_json_string` でフィールドごとに手動シリアライズするよう置き換え。

### その他

4. **`&self.notes` のフィールド移動 (E9040)**
   `all_notes()` が `&self.notes` を返すとステージ 1 で `E9040` (フィールドの移動) になる。`all_notes()` を `all_notes_len()` / `get_at()` に分割し、参照をインデクス経由で取り出すようにした (`store.ksnl`)。

## 動作確認

`ksn check` / `ksn fmt --check` / `ksn test` (9 件, 0 失敗) / `ksn build --release` すべて通過。`add`/`list`/`show`/`edit`/`rm`/`tag`/`search`/`export` (JSON+MD) を手動で動作確認済み。
