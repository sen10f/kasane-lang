# apps/todo 摩擦記録

本パッケージ (`apps/todo`) を実装する上で、想定通りに動作しなかった点や回避策を要した点を記録する。

## 摩擦の件数: 9 件

## 上位 3 件

1. **`rand` エフェクトが KIR ステージ1で未対応** → 解決 (fill_secure)
   `auth.make_token` / `auth.make_salt` で `rand.Rng.from_entropy()` を使用すると `E9040` でネイティブビルド失敗。`std.rand.fill_secure(&mut [u8; 16])` で OS の乱数を生成し、`bytes_to_hex` で 16 進数文字列にするよう置き換え。`uses [alloc, time]` を `uses [alloc, rand]` に戻し、`router.register`/`router.login`/`router.route` とその呼び出し元全体から `rand` を除去した。

2. **構造体フィールドに `Option<Vec<...>>` があるとステージ1で下ろせない**
   `UpdateFields.tags: Option<alloc.vec.Vec<...>>` が `from_update_value` の戻り値型として `E9040` を引き起こす。`tags` を `Option<Vec<...>>` から plain `Vec<...>` に変更し、欠損時は空ベクトルを返すように `from_update_value` を修正。

3. **マッチアーム内の `String.as_str()` が生参照になりうる**
   `router.route` で `query_param` の `Some(v) => v.as_str()` をマッチアームから直接返すと、`v` のドロップ後 `q` がダングリング参照になる。`q_string` 変数に `String` を束縛してから `as_str()` を呼ぶことで回避。

### その他

4. **モジュール修飾の関数呼び出しがメソッド呼び出しとして誤認される**
   `todo.model.push_quoted(...)` がメソッド呼び出しと解釈され `E1108`/`E2044` になるため、`to_json` 内にインライン展開して回避（`push_quoted` 関数自体は残存）。

5. **`i64.from_u64` が存在しない**
   数値変換に `i64.try_from_usize` を使用した。

6. **`AllocError` のフィールドは `u32`**
   `AllocError { code: 0u32 }` とする。

7. **`decode_array` は `&doc` 引数で呼び出す**
   デコード関数にドキュメントへの不変参照を渡す必要がある。

8. **検索JSON出力に先頭カンマが入る**
   `store.search` の出力ループで `if i > 0usize` とすべきところ `if count > 0usize` とすべき。最初の一致アイテムがインデックス0以外の場合、`[,{...}]` のように不正なカンマが入る。動作上の問題はないがJSONとして不正。

9. **`std.json.Value { doc, at }` の直接構築と `v.at` が非公開**
   `dev/plan` では `std.json.Doc.value_at(doc, at)` と `v.index()` を使用。本ブランチではフィールドがまだ公開されているため一時的にそのまま使用しているが、将来的に非公開化された場合の移行が必要。
