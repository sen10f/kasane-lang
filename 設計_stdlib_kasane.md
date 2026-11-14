# 標準ライブラリの Vec / String を Kasane で書く (修正プラン 3-3)

## 再開後の進捗 (2026-11-14、方式 A: `alloc.raw.Buf`)

| 段 | 状態 |
| --- | --- |
| 1 仕様 | 済み: 8.3・9 章 15・10 章と `spec/stdlib/alloc/raw.sig` (`Buf.new` を含む)。`alloc.vec.Vec` の型定義を `{ buf: alloc.raw.Buf<T, A> }` に |
| 2 `Buf` の実装 | 済み: 木走査 (`natives.rs` の `buf_method`)、KIR (`kir_lower_buf.rs`。`Vec` と同じ 24 バイトのヘッダ、cap は要素数)、VM と LLVM (KIR を共有)。`stage0/tests/raw_buf.rs` |
| (a)(b) 一般の下ろし | 済み: 型引数付き構造体の単相化、受け手からの型引数の推論、`Buf` フィールドの暗黙の drop、参照を返すメソッド、`T` と `F: Fn` (効果変数つき) を取る総称関数の単相化 (`core.slice.sort` / `sort_by` / `binary_search` ほか、`Vec.append` / `contains` / `dedup` / `extend_from_slice` がネイティブに)、整数の `cmp` / `eq` / `try_clone`、`slice.swap` / `sub_mut`、`[T]` の比較 |
| (c) `Vec` の基本メソッドを Kasane 版に | **KIR で済み (隠しの切り替え `KSN_VEC_IMPL=kasane`)**。`spec/stdlib/alloc/vec.ksnl` に `as_mut_slice` `as_slice` `capacity` `clear` `insert` `is_empty` `len` `pop` `push` `remove` `reserve` `truncate` を `Buf` の上に Kasane で書いた。木走査は組み込みのまま (`interp.rs` の `KASANE_VEC_BASE` は `Val::Vec` に Kasane 版を使わない) |
| (d) 組み込みの下ろしを外す | 未着手 (下の理由) |

### 方式 (段階的)

`Vec<T, A>` と `Buf<T, A>` は同じ 24 バイトのヘッダ {ptr, len, cap} なので、KIR は `Vec` の値・局所変数・フィールド・`v[i]`・`for`・drop を今までどおりヘッダで扱い、
**基本メソッドの呼び出しだけ**を `alloc.vec.Vec.<m>[T, A]` の単相インスタンス (ヘッダのアドレスを `&self` に取る) にする (`kir_lower_coll.rs` の `method_on` が `KSN_VEC_IMPL=kasane` のとき
`kir_lower_ext.rs` の `ext_call` へ)。インスタンスの本体の `self.buf.<m>(..)` は `self` のヘッダそのもの (`kir_lower_own.rs` の `field_call`) として `Buf` の下ろし (`kir_lower_buf.rs` と
従来の `Vec` の操作) に渡る。`alloc.vec.Vec.*` の本体の中では組み込みの下ろしを使う (再帰しない)。

### 検証 (`KSN_VEC_IMPL=kasane`)

- `cargo test --release` 全件が通る (切り替えなしでも通る)。`tests/native_ratio.py`: linear_ok 105/105、examples 14/14、perf 8/8 (切り替えの有無で同じ)。
- `examples/` 全パッケージの `ksn test --both` が通り (LLVM 比較、バンドル退避 0)、`ksn run` と `ksn build` の出力が一致する (demo / tour / more_coll / features / more_str / plugins / json)。
- `stage0/tests/raw_buf.rs` の `vec_methods_written_in_kasane_match_the_builtin_ones_natively`: 木走査 (組み込み) と Kasane 版のネイティブの出力が一致し、`alloc.vec.Vec.push<..>` が KIR に現れる。

### 性能 (ネイティブ、`perf/run_perf.py --repeat 5`、LLVM の列、ミリ秒)

| ワークロード | 組み込み | Kasane 版 | 比 |
| --- | ---: | ---: | ---: |
| Vec の push・索引読み・索引書き (N=300,000) | 3.1 | 3.0 | 0.97 |
| 文字列連結 (`String`、Vec を使わない) | 1.3 | 1.6 | (対象外・ばらつき) |
| 整数ループ | 1.9 | 1.6 | (対象外・ばらつき) |
| 構造体とメソッド呼び出し | 1.4 | 1.5 | (対象外・ばらつき) |
| HashMap | 1.2 | 1.2 | (対象外) |

入力が小さく (数 ms) ばらつきが大きいので、`Vec` だけを N = 30,000,000 で測り直した: 組み込み 0.26 / 0.26 / 0.23 秒、Kasane 版 0.25 / 0.27 / 0.27 秒 (比 約 1.0)。目安の 1.2 倍以内。
`Kvec<T, A>` (`{ buf }` の構造体、`stage0/tests/data/raw_buf/kvec.ksnl`) の素朴な版も 0.9 倍前後。

### まだ外せないもの (d)

- **木走査の `Val::Vec`** (`value.rs` / `natives.rs` の `seq_method`): 添字・反復・`Debug`・`==` が `Val::Vec` を直接読む。木走査を Kasane 版にするには `Vec` を構造体 `{ buf }` にして、これらを構造体経由にする (または `Buf` を `Val::Vec` で持つ) 必要がある。
- **KIR の `Kind::Vec` (ヘッダ)**: `Vec` の局所変数・フィールド・`v[i]`・`for`・`v.get(i)` は `Kasane 版でもヘッダの専用経路を使う` (`Buf` の下ろしが `Kind::Vec` の上にあるため)。外すには `Vec` を単相化した構造体にして
  `v.get(i)` → `v.as_slice().get(i)` の読み替えと、`Buf` のヘッダ操作を `Kind::Buf` として独立させる作業が要る。今回は基本メソッド (push / pop / insert / remove / ...) の組み込みの下ろし (`vec_push` `vec_pop` `vec_insert` `vec_remove` `clear` ほか) が
  Kasane 版に置き換わっただけで、`Buf` 経由で同じ関数を使うので**削除はまだできない**。
- `String` (`Vec<u8>` の上の UTF-8 `push` / 整形 / 比較) は未着手。`try_clone` と `with_capacity` の Kasane 版は、木走査が `Val::Vec` を作る組み込みと衝突するため入れていない。

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
