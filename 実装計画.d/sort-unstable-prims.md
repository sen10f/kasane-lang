# sort-unstable-prims: `slice.ksnl` の `sort`/`sort_by` を高速化 (P-02)

- 問題: `sort`/`sort_by` が SymMerge (交換 O(n log² n)、`s.swap` 1 回ごとに境界検査 + Result) で u64 200 万個 1.52 s 対 Rust 0.18 s (8.5 倍)。`sort_unstable`/`sort_unstable_by` はヒープソート (不安定) で既に存在し、公式 API として追加しない。
- 方針: 型による特殊化は Kasane 標準ライブラリにはできない (`kir_lower_ext.rs` の `sort` は通常の総称呼び出し、要素型を見て分岐する仕組みなし)。`sort` は仕様で安定・`uses [] panics never`（確保なし）・シグネチャ不変更なので、**型独立の原位置安定ソート**に置き換える: 12 要素ずつの挿入整列のあと、2 倍ずつの幅で**回転による安定な併合** (`merge_by`)。等しい要素は左側を優先するので安定、`uses [E] panics P` を `sort_by` からそのまま転送。インタプリタとネイティブで同じ Kasane ソースを走らせるので結果は一致。
- `symmerge_by` を削除し `merge_by` に置き換え (`rotate_range` はそのまま使用)。再帰をなくし追加記憶 O(1)。最悪の互い違い入力では移動が増えるが、試験の random/sorted/reversed/duplicates は線形〜O(n log n) 程度。
- スカラー型の高速パスは既存の `sort_unstable()`（ヒープソート、不安定・等しい要素が見分けられない型では結果が同一）でカバー。
- テスト: `stage0/tests/slice_sort.rs` に `sort`/`sort_by` を挿入ソートの基準と比較する試験 (random/sorted/reversed/多数重複、レコードの `sort_by` で安定性) を追加。測定は前後で ksn 対 Rust (200 万 u64)。
- 文書: `Kasane 簡易仕様書.md` の整列の節で SymMerge の記述を回転併合に差し替え。
