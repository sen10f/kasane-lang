# 標準ライブラリの Vec / String を Kasane で書く (修正プラン 3-3)

状態 (2026-11-14): **段 1 に入る前に、前提が足りず止まった**。実装は書いていない。
下の試作は `core.ptr` + `alloc.alloc.Allocator` の上に書いた最小の `Vec` (`new` / `push` / `get`、
`T` と `A` はジェネリクス、`unsafe` 許可モジュール)。`ksn check` は診断 0 件で通る。実行は次のとおり。

## 前提が足りない点 (実機で確認)

| # | 経路 | 症状 | 必要なこと |
| --- | --- | --- | --- |
| 1 | 木走査 (`ksn run` / `test`) | `std.alloc.System` の `alloc` / `dealloc` / `realloc` がインタプリタに無い。`panic: no method alloc found for value System { id: 0 }` | `Allocator` の `System` を木走査・VM に実装する (malloc / realloc / free) |
| 2 | KIR / LLVM | `kv.Kvec<u64, std.alloc.System>` は「型引数付きで段階 1 の対象外」。型引数を外し `u64` / `System` に固定した版も「戻り値の型は段階 1 の対象外」(生ポインタを持つ構造体) | 修正プラン 3-1 (構造体・単相化の一般化) に、生ポインタフィールドを持つ構造体を含める |
| 3 | 仕様 (`core.slice`) | `as_slice` / `as_mut_slice` / `String.as_str` は (ポインタ, 長さ) から `&[T]` / `&str` を作る。`core.ptr` にも `core.slice` にもその原始操作が無い | `core.slice.from_raw_parts` / `from_raw_parts_mut`、`core.str.from_utf8_unchecked` 相当を `unsafe` 許可モジュール限定で追加 (仕様 3.8 と .sig の変更。オーナー判断) |
| 4 | 仕様 | `Vec<T>` の添字・`for`・`get` が返す参照 (`Option<&T>`)、`Drop` で要素を破棄する glue (`T` が `Vec` / `String` の場合) を、Kasane で書く道具が KIR に無い | 3-1 に、`Drop` 実装を持つジェネリック型の下ろしと、`core.ptr.read` の要素型が集成体の場合を含める |
| 5 | 検査 | `kv.Kvec` のような名前は `UpperCamelCase` 規約 (連続大文字不可) に従う必要があり、`alloc.vec.Vec` を別名で並べる切り替え (`KSN_STDLIB_VEC`) は、型検査が `alloc.vec.Vec` の組み込みの扱いを名前で固定している (`kir_lower_coll.rs`、`types.rs`) ため、モジュールの差し替えだけでは動かない | 切り替えは名前解決の層で行う必要がある (1〜4 の後) |

## 判断

- 段 2 (perf の比較)・段 3 (組み込みの除去) は、1・2・3 が済むまで測れない。組み込みの `Vec` / `String` の下ろし (`kir_lower_coll.rs`)
  と `natives.rs` の木走査の組み込みは **外せない** (外すと木走査で `Vec` が動かなくなる)。
- 先に 3-1 (KIR の一般化) を済ませるのが順序として正しい。3-3 はその後に、上の 1〜4 を条件として再開する。
- 4-4 (`String` の `==` / `Debug`) の担当とは、`String` の比較・表示の組み込みで衝突しうる。本文書は触らない。

## 再開の手順 (案)

1. `System` アロケータを木走査・VM・KIR に実装 (1)。
2. `core.slice.from_raw_parts(_mut)` を仕様に足して実装 (3)。
3. 3-1 が生ポインタ入りのジェネリック構造体を下ろせるようにしたのを確認 (2・4)。
4. `spec/stdlib/alloc/vec.ksnl` / `string.ksnl` に本体を書き、`examples/` と `perf/` を組み込みと比べて、ここに表を足す。
