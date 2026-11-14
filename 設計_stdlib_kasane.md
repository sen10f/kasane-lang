# 標準ライブラリの Vec / String を Kasane で書く (修正プラン 3-3)

## 再開後の進捗 (2026-11-14、方式 A: `alloc.raw.Buf`)

| 段 | 状態 |
| --- | --- |
| 1 仕様 | 済み: 8.3・9 章 15・10 章と `spec/stdlib/alloc/raw.sig` (`Buf.new` を含む) |
| 2 `Buf` の実装 | 済み: 木走査 (`natives.rs` の `buf_method`)、KIR (`kir_lower_buf.rs`。`Vec` と同じ 24 バイトのヘッダ、cap は要素数)、VM と LLVM (KIR を共有)。`stage0/tests/raw_buf.rs` が run / `--engine=vm` / `ksn build` のネイティブの出力の一致を見る |
| (a) KIR の `Buf` | 済み: `with_capacity` / `new` / `push_within_cap` / `insert_within_cap` / `swap_remove` / `truncate` / `grow` / `append_str_within_cap` / `as_str`。共通の `len` `pop` `remove` `get` `clear` `as_slice` は `Vec` の下ろしを使う。要素はスカラー・所有物 (`String` など) |
| (b) 一般の下ろし | 済み: 型引数付きの構造体の単相化 (`AggTab::intern_generic`、名前 `FQN<引数>`)、メソッドの型引数を受け手の型から推論 (`mono_targs` の `recv`)、フィールドの型の置換 (`field_src`)、`Buf` をフィールドに持つ構造体の暗黙の drop (既存のフィールドの drop glue)、参照を返すメソッド、`AllocError` を仕様どおり `{code: u32}` に、整数フィールドの構造体定数 (`std.alloc.SYSTEM`)。ほかに、`&mut self.f` が値の写しを指していた既存の誤りを直した |
| (c) `vec.ksnl` / `string.ksnl` | **未着手**: Kasane で書いた `Vec` の試作は `stage0/tests/data/raw_buf/kvec.ksnl` (`Kvec<T, A>` = `{ buf: Buf<T, A> }`。`Vec` の .sig の全メソッドのうち `try_clone` 以外)。名前 `alloc.vec.Vec` への差し替え (切り替え) と `String` は未実装 |
| (d) 性能 | `Kvec` と組み込み `Vec` の素朴な比較 (下) だけ。`perf/run_perf.py` での全体の比較は切り替えが無いので未実施 |

### 性能 (ネイティブ、`ksn build --release`、perf/vecops と同じ形で N = 30,000,000、3 回、壁時計)

| 版 | 時間 (秒) | 最大 RSS |
| --- | --- | --- |
| 組み込み `Vec` (`v.get(i)`・`v[i] = x`) | 0.97 / 1.04 / 0.92 | 236 MB |
| Kasane の `Kvec` (`v.as_slice().get(i)`・`let s = v.as_mut_slice(); s[i] = x`) | 0.96 / 0.61 / 0.85 | 236 MB |

比は約 0.9 倍 (目安の 1.2 倍以内)。ばらつきが大きい (同じ版でも 0.6〜1.0 秒) ので、確かな比は切り替えの後に `perf/run_perf.py` で測り直す。
`Kvec<String>` と入れ子の `Kvec<Kvec<u32>>` の push / insert / remove / truncate を 300 万回繰り返しても RSS は 1.7 MB のまま (漏れなし)。

### 切り替え (c) が止まっている理由

`alloc.vec.Vec` を Kasane 版に差し替えるには、次の「名前で固定された組み込み」を外す必要がある。

- 検査 (`types.rs` / `check.rs` / `borrowck.rs`): `Vec` から `core.slice` への自動の参照外し、`v[i]`・`for x in v` の要素型。名前 `alloc.vec.Vec` のままなら、型の宣言を差し替えるだけで動くはず (未確認)。
- 木走査 (`value.rs` の `Val::Vec`、`natives.rs` の `seq_method`): `Vec` を値 `Val::Vec` で持ち、添字・反復・`Debug`・`==` がそれを直接読む。Kasane 版 (構造体 `{ buf }`) にするには、これらを構造体経由にするか、`Buf` を `Val::Vec` で持つ。
- KIR (`kir_lower_coll.rs`): `Vec` の局所変数・フィールド・`v[i]`・`for`・`v.get(i)` が `Kind::Vec` の専用経路。Kasane 版では `v.get(i)` を `v.as_slice().get(i)`、`v[i]` を `v.as_slice()[i]` に読み替える下ろしが要る (今の `Kvec` の例は利用者が手で書き換えた)。`v.as_mut_slice().get_mut(i)` の `match` も未対応 (`chain_of` が `Buf` でない構造体を見ない)。
- `String` は `Vec<u8>` の上で `push(char)` の UTF-8 符号化、`push_display` / `push_debug` (整形)、`replace` / `join`、`Eq` / `Ord` / `Hash` / `Debug` / `Display` を Kasane で書く必要がある。

組み込みの下ろし (`kir_lower_coll.rs`) と木走査の組み込みは、この切り替えが済むまで **外せない**。

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
