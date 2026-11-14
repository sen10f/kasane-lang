# 標準ライブラリの Vec / String を Kasane で書く (修正プラン 3-3)

## 再開後の進捗 (2026-11-14、方式 A: `alloc.raw.Buf`)

- 段 1 済み: 仕様 8.3・9 章 15・10 章と `spec/stdlib/alloc/raw.sig`。unsafe の原始操作は足さず、処理系が安全な箱 `alloc.raw.Buf<T, A>` を持つ。
- 段 2 は木走査だけ済み: `natives.rs` の `buf_method` (構造体 { allocator, capacity, items })。テストは `stage0/tests/raw_buf.rs`
  (`run --engine=tree` / `--engine=vm` / `build` の出力が一致)。ただし `--engine=vm` と `build` は KIR に下ろせず
  (`alloc.raw.Buf.with_capacity は段階 1 の対象外`) 木走査・インタプリタ同梱のバンドルへ退避しているので、**KIR / VM / LLVM の実装は未着手**。
- 未着手の理由: KIR の下ろし (`kir_lower_coll.rs` / `kir_lower_own.rs`) は `Vec` / `String` の局所変数を {ptr, len, cap} のヘッダで持つ専用の経路で、
  `Buf` を構造体のフィールドにしたジェネリックな `Vec<T, A>` (`Drop`・参照を返す `get`・要素が集成体のときの破棄) を下ろす一般の道がまだない。
  `Buf` を KIR の `Kind::Vec` の別名として足すだけでは、Kasane で書いた `Vec` がそのフィールドに持つ形を下ろせない。
- 段 3 (Kasane 版の `vec.ksnl` / `string.ksnl`・切り替え・perf の表)、段 4 (組み込みの除去) は未着手。組み込みの下ろしは **外せない** (外すと KIR / LLVM の `Vec` が無くなる)。
- 次の作業の順: (a) KIR に `Buf` の型 (ヘッダ 24 バイト + 要素型) と各メソッドの下ろし、(b) ジェネリック構造体のフィールドに `Buf` を持てるようにし `Drop` の glue、(c) `vec.ksnl` / `string.ksnl`、(d) 切り替えと比較。


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
