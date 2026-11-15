# 標準ライブラリの Vec / String を Kasane で書く (修正プラン 3-3)

## 再開後の進捗 (2026-11-14、方式 A: `alloc.raw.Buf`)

| 段 | 状態 |
| --- | --- |
| 1 仕様 | 済み: 8.3・9 章 15・10 章と `spec/stdlib/alloc/raw.sig` (`Buf.new` を含む)。`alloc.vec.Vec` の型定義を `{ buf: alloc.raw.Buf<T, A> }` に |
| 2 `Buf` の実装 | 済み: 木走査 (`natives.rs` の `buf_method`)、KIR (`kir_lower_buf.rs`。`Vec` と同じ 24 バイトのヘッダ、cap は要素数)、VM と LLVM (KIR を共有)。`stage0/tests/raw_buf.rs` |
| (a)(b) 一般の下ろし | 済み: 型引数付き構造体の単相化、受け手からの型引数の推論、`Buf` フィールドの暗黙の drop、参照を返すメソッド、`T` と `F: Fn` (効果変数つき) を取る総称関数の単相化 (`core.slice.sort` / `sort_by` / `binary_search` ほか、`Vec.append` / `contains` / `dedup` / `extend_from_slice` がネイティブに)、整数の `cmp` / `eq` / `try_clone`、`slice.swap` / `sub_mut`、`[T]` の比較 |
| (c) `Vec` の基本メソッドを Kasane 版に | **KIR で既定**。`spec/stdlib/alloc/vec.ksnl` に `as_mut_slice` `as_slice` `capacity` `clear` `insert` `is_empty` `len` `pop` `push` `remove` `reserve` `truncate` を `Buf` の上に Kasane で書いた。木走査は組み込みのまま (`interp.rs` の `KASANE_VEC_BASE` は `Val::Vec` に Kasane 版を使わない) |
| (d) 組み込みの下ろしを外す | **一部済み**: KIR の `vec_push` / `vec_insert` / `vec_reserve` (と `Vec.push` / `insert` / `reserve` の分岐) を削除。残りは下 |

### 方式 (段階的)

`Vec<T, A>` と `Buf<T, A>` は同じ 24 バイトのヘッダ {ptr, len, cap} なので、KIR は `Vec` の値・局所変数・フィールド・`v[i]`・`for`・drop を今までどおりヘッダで扱い、
**基本メソッドの呼び出しだけ**を `alloc.vec.Vec.<m>[T, A]` の単相インスタンス (ヘッダのアドレスを `&self` に取る) にする (`kir_lower_coll.rs` の `method_on` が
`kir_lower_ext.rs` の `ext_call` へ。切り替えの環境変数はない: 組み込みの `push` / `insert` / `reserve` を削除したので、戻す道は git の履歴だけ)。要素型が決まらない `Vec` は `push` / `insert` の引数の型で決める。インスタンスの本体の `self.buf.<m>(..)` は `self` のヘッダそのもの (`kir_lower_own.rs` の `field_call`) として `Buf` の下ろし (`kir_lower_buf.rs` と
従来の `Vec` の操作) に渡る。`alloc.vec.Vec.*` の本体の中では組み込みの下ろしを使う (再帰しない)。

### 検証 (Kasane 版が既定)

- `cargo test --release` 全件が通る。`tests/native_ratio.py`: linear_ok 105/105、examples 14/14、perf 8/8 (下がっていない)。
- `examples/` 全パッケージの `ksn test --both` が通り (LLVM 比較、バンドル退避 0)、`ksn run` と `ksn build` の出力が一致する (demo / tour / more_coll / features / more_str / plugins / json)。
- `stage0/tests/raw_buf.rs` の `vec_methods_written_in_kasane_match_the_builtin_ones_natively`: 木走査 (組み込み) と Kasane 版のネイティブの出力が一致し、`alloc.vec.Vec.push<..>` が KIR に現れる。

### 性能 (ネイティブ、`perf/run_perf.py --repeat 5`、LLVM の列、ミリ秒。切り替え前後)

| ワークロード | 組み込み (切り替え前) | Kasane 版 | 比 |
| --- | ---: | ---: | ---: |
| Vec の push・索引読み・索引書き (N=300,000) | 3.1 | 3.0 | 0.97 |
| 同 (再測定、削除後) | 3.1 | 3.0 | 0.97 |
| 他の 4 ワークロード (Vec を使わない/ほぼ使わない) | 差なし (ばらつきの範囲) | | |

入力が小さい (数 ms) ので、`Vec` だけを N = 30,000,000 で測り直した: 組み込み 0.26 / 0.26 / 0.23 秒、Kasane 版 0.25 / 0.27 / 0.27 秒、削除後の Kasane 版 0.20 / 0.19 / 0.20 秒 (比 約 1.0)。目安の 1.2 倍以内。

### まだ外せないもの (d)

- **木走査の `Val::Vec`** (`value.rs` / `natives.rs` の `seq_method`): 添字・反復・`Debug`・`==` が `Val::Vec` を直接読む。木走査を Kasane 版にするには `Vec` を構造体 `{ buf }` にして、これらを構造体経由にする (または `Buf` を `Val::Vec` で持つ) 必要がある。
- **KIR の `Kind::Vec` (24 バイトのヘッダ)**: `Vec` の局所変数・フィールド・`v[i]`・`for`・`v.get(i)`・drop はヘッダの専用経路のまま。`Buf` の下ろし (`kir_lower_buf.rs`) と `pop` / `remove` / `clear` / `get` / `as_slice` の組み込みの操作がその上にあるので、
  外すには `Vec` を単相化した構造体にして `v.get(i)` → `v.as_slice().get(i)` の読み替えと、`Buf` のヘッダ操作を `Kind::Buf` として独立させる必要がある。
- `String` の基本メソッドは段 1〜3 を済ませた (下の「String」の節)。整形・比較・`from_str` / `join` / `replace` は組み込みのまま。`try_clone` と `with_capacity` の Kasane 版は、木走査が `Val::Vec` を作る組み込みと衝突するため入れていない。

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

## HashMap / BtreeMap (修正プラン 3-4、2026-11-15)

### 段 1 (済み): 本体を Kasane で書く

`spec/stdlib/alloc/collections.ksnl` に規範の API を全て書いた (`new` だけは組み込みのまま: 木走査の `Val::Map` と KIR の下ろしが作る)。
`ksn check-stdlib` は 0 件、`stage0/tests/map_kasane.rs` が素朴なモデル (線形探索の配列) と 1200 手で比べて通る (木走査)。

表現は **アロケータを 1 本の `Buf` にまとめる**。`A: Allocator` は `Clone` を要求しないので、2 本目の `Buf` を作るアロケータがない
(`Buf` は `allocator: A` を持つ)。そこで 1 つのコンテナは `Buf` を 1 本だけ持つ。

| 型 | 内部表現 | 反復順 | 計算量 (n = 組の数、B = 索引の大きさ、B ≥ 2n) |
| --- | --- | --- | --- |
| `HashMap<K, V, A>` | `cells: Buf<HashCell<K,V>, A>`、`slots: usize` (= B)。先頭 B 個が開番地法の索引 (`Empty` / `Index(位置)`、線形探査)、その後に `Entry(ハッシュ, キー, 値)` が挿入順に並ぶ | 挿入順。削除後も相対順を保つ (8.3・表 `挙動一致`) | `get` / `insert` / `contains_key` 平均 O(1)。`insert` の増設は B を 2 倍にして組の列を回転 (区間の入れ替え) し索引を組み直すので、増設 1 回が O(B + n)、償却 O(1)。`remove` は組を `Buf.remove` で詰めて索引を組み直す O(B + n)。`iter` は組の列を前から見る O(B + n) |
| `BtreeMap<K, V, A>` | `entries: Buf<MapEntry<K,V>, A>` (キーで整列) | キーの順 (`Ord`) | `get` / `contains_key` O(log n) 回の比較。`insert` / `remove` は要素の移動で O(n) (挿入 `insert_within_cap`) |
| `HashMapIter` | `cells: &[HashCell]` と位置 | 同上 | 1 件 O(1) (空きは飛ばす) |

- ハッシュ: `core.hash.Fnv64` (FNV-1a 64) を `spec/stdlib/core/hash.ksnl` に Kasane で書いた (従来は規範だけで実装がなかった)。`hash_key` は `key.hash(&mut state)` と `state.finish()`。
- 整数・`bool`・`char` の `Hash` (仕様 4.2 の組み込み) は、木走査で値のリトルエンディアンのバイト列を `Fnv64.write_bytes` へ渡す (`natives.rs` の `hash_bytes`)。以前は何もしなかった (`Hash` の結果が常に同じで、索引が 1 本の連なりになっていた)。反復順は挿入順なので、ハッシュの値は出力に影響しない。
- `String` のキーは `Hash` の既存の実装 (`alloc/string.sig`) を通す。木走査での動作は未確認 (Kasane 版の試験は整数キーだけ)。KIR での下ろしは段 2 の対象。

### 段 2 (未): KIR の下ろし (`kir_lower_map.rs` を外す)

現状は KIR の `HashMap` が専用の 24 バイトのヘッダ (`kir_lower_map.rs` の `Shape` は `ksz` 8 / 16 / 24 の 3 種類のキーだけ)。Kasane 版へ切り替えるには、下の 5 つが要る。確かめた結果 (2026-11-15、一時的な計測):

1. **型表**: `core.hash.Fnv64` を `STD_TYPES` に足すと `Fnv64.new` が下りる (足した実験で確認。戻した)。`HashCell` / `HashMapIter` / `MapEntry` は利用者の型でも `STD_TYPES` でもないので、`lower_ty` が `未対応` で拒否する (`kir_lower_agg.rs`)。
2. **整数の `hash`**: KIR に `x.hash(&mut state)` の下ろしがない (`メソッド hash は段階 1 の対象外`)。`FNV` の 1 バイトずつの更新を IR で出すか、`write_bytes` へ `[u8; N]` を渡す。
3. **`HashMap` の表現**: `HashMap` は `kir_lower_agg.rs` で `vec_hdr()` (専用のヘッダ) になっている。`cells` 1 本の構造体にすれば 24 バイトで `Vec` と同じ配置になり、`Vec` の方式 (基本メソッドを単相インスタンスへ、ヘッダのアドレスを `&self` に) を使える。ただし `slots` を `cells[0]` の `Header(usize)` に入れる必要がある (構造体を 1 本のフィールドにする)。
4. **反復**: KIR は自作の反復子の `for` を下ろさない (`kir_lower_iter.rs` の対象外に「自作の反復子」)。`HashMapIter` の `for` と `next` 呼び出しは、その下ろしが要る。
5. **`new`**: `HashMap.new` / `BtreeMap.new` は規範の宣言だけで本体がない。KIR のために Kasane の本体を書くと、木走査も Kasane 版を見る (`NATIVE_NAMES` の扱いを確かめる必要がある)。

性能の表 (目安 1.2 倍以内) は段 2 が済むまで作れない。

### 段 3 (調べた結果): チャネルと `HashMap` の型ごとの下ろし

- `kir_lower_map.rs` の型ごとの下ろしは、段 2 が済むまで外せない (上の 1〜5)。
- チャネル (`std.sync`) は Kasane の `Mutex` + `Buf` の待ち行列に**移せない**。ネイティブの待ちは `ksn.thr.wait` (条件変数) と `ksn.thr.{lock,unlock}` で、`gen` / `blocked` / `live` の数で全員待ちの deadlock を検出する (設計_KIR.md 22.7)。Kasane の側に条件変数の原語がなく、待ちを Kasane から書けない。木走査 (協調実行) と VM は別の待ち方をする。移すには、条件変数を標準ライブラリの原語 (unsafe の境界の中) として決める必要がある。


## String (修正プラン 3-3 の残り、2026-11-15、`std/string-kasane`)

`alloc.string.String` を `{ buf: alloc.raw.Buf<u8, A> }` にして (`spec/stdlib/alloc/string.sig`)、基本メソッドを `spec/stdlib/alloc/string.ksnl` に unsafe なしの Kasane で書いた。
`Vec` と同じ 24 バイトのヘッダなので、KIR は要素型 `u8` の `Buf` (`Kind::Vec(Some(u8))`) として `self.buf` を下ろす。

| 段 | 状態 |
| --- | --- |
| 1 仕様 | 済み。Kasane 版: `as_str` `clear` `is_empty` `len` `push` (char の UTF-8 符号化) `push_bytes` (`core.str.from_utf8` を通す) `push_i64` `push_str` `push_u64`、内部の `reserve_bytes` (非公開)。`.sig` からこれらの宣言を外した |
| 2 KIR | 済み。`KASANE_STRING_BASE` の名前は `string_call` (`kir_lower_ext.rs`) で `alloc.string.String.<m>` の単相インスタンスへ。`ext_lower` の後半は `kasane_call` に切り出して Vec と共用 |
| 3 組み込みの除去 | 済み (下の一覧)。木走査 (`natives.rs` の `StrBuf` 系) は組み込みのまま。`interp.rs` の `KASANE_STRING_BASE` が `Val::StrBuf` に Kasane 版を使わないようにしている |

### 外した組み込み (KIR)

- `kir_lower_own.rs`: `String` の `push_str` / `push` / `clear` と、その補助 `string_push_str` / `string_push` / `utf8_store` / `ok_unit` (`Vec` の `clear` は残す)。
- `kir_lower_coll.rs`: `String` の `push_str` / `push_bytes` / `push_u64` / `push_i64` の分岐と `string_push_str` / `string_push_bytes` / `string_push_u64` / `string_push_i64` / `push_u64_digits`。`String` の `as_str` の分岐 (`len` / `is_empty` は `Str` / スライス / `Vec` / `Buf` の本体で使うので残す)。

### 残した組み込み (理由)

- `new`: KIR の `new_header` (`std.alloc.System` 固定の構築)。
- `from_str`: KIR の `from_str_call` だけが下ろす。`join` / `replace` は KIR に下ろす経路がなく、木走査だけ。A を受け取る構築の Kasane 化は、総称の関数の単相化 (A の推論) が要るので後回し。
- `push_display` / `push_debug` / `debug_fmt` / `display_fmt`: 整形 (`Formatter`) は 4-4 の担当と衝突しうるので、本文書は触らない。
- `eq` / `cmp` / `partial_cmp` / `hash`: `string_cmp_method` と `str` の比較。同じく 4-4 の担当と衝突しうる。
- `contains` / `starts_with` / `ends_with` / `as_bytes`: `str` の組み込みの経路 (`str_method_call`)。`.sig` に宣言がない。
- `Buf` の基礎操作 (`append_str_within_cap` / `push_within_cap` / `insert_within_cap` / `grow` / `cap` / `as_str` の UTF-8 検査): 段 1 の土台 (`kir_lower_buf.rs`)。外すと `Vec` も外れる。

### 変えたこと (性能のための KIR の変更)

- Kasane 版の単相インスタンス (`alloc.string.String.*` / `alloc.vec.Vec.*`) を `alwaysinline` にし、再帰しない (呼び先も有界) ので `ksn.depth` の記録を省く (`kir_llvm.rs`)。
- 初めは `alwaysinline` だけで、`push_str` の呼び出し 1 回ごとに `ksn.depth` の 3 回の書き込みが残り、組み込み比 1.4 倍だった。省いて約 1.0 倍になった。
- 基本メソッドは再帰せず、呼び先も有界なので、上限 (2500) の意味は保たれる。ただし `extend_from_slice` のように `T` の関数 (利用者の関数) を呼ぶ形では、その関数の再帰の 1 段ごとに基本メソッドの 1 フレームが深さに数えられない (スタックの使用量は定数倍だけ増える)。

### 性能 (ネイティブ、LLVM)

- strcat (`s.push_str("ab")` を N 回)、N = 20,000,000 の複製 (`perf/strcat` の入力だけ増やした。リポジトリの外の `/tmp` で計測):

| 計測 | 版 | 最小 | 中央値 |
| --- | --- | ---: | ---: |
| 1 回目 (交互 31 回) | 組み込み (変更前) | 39.3 ms | 45.9 ms |
| 1 回目 (交互 31 回) | Kasane 版 | 39.7 ms | 43.9 ms |
| 2 回目 (別の時間) | 組み込み (変更前) | 28.8 ms | 37.6 ms |
| 2 回目 (別の時間) | Kasane 版 (最終) | 28.8 ms | 40.2 ms |

  比は 0.96〜1.07 (最小値の比 1.00〜1.01)。目安の 1.2 倍以内。N = 200,000 の `perf/run_perf.py` は起動の時間 (1 ms 前後) と同じ大きさで、ばらつきの範囲 (文字列 LLVM 1.3〜1.9 ms、変更前も同じ)。
- Vec・構造体の `run_perf` (`vecops` / `structs`) は変更前と差なし (ばらつきの範囲)。

### 検証

- `ksn check-stdlib`: 0 件。`tests/native_ratio.py`: linear_ok 111/111、examples 14/14、perf 8/8 (下限 105 / 14 / 8 を下回らない。下限のファイルは書き換えていない)。
- examples・apps・selfhost・perf の全パッケージで `ksn check` 0 件、`ksn test --both` 通過、`ksn run` と `ksn build` の出力が一致 (`examples/kvlib` はライブラリで入口がない)。`bench/verify_reference.py`: 19 件中 0 件の不一致。
- `stage0/tests/string_kasane.rs`: push 系の全経路 (1〜4 バイトの文字、最大・最小・0 の整数、`push_bytes`) の出力が木走査とネイティブで期待値と一致し、ネイティブの KIR が `alloc.string.String.push_str<std.alloc.System>` を呼ぶことを見る。
- `cargo test --release` 全件 通過。

### 残り・確かめていないこと

- `String::as_str` は 0.10.18 で検査を省く組み込みにした (下の「`String::as_str` の O(1) 化」)。`Buf<u8>::as_str` は検査つきのまま。
- 木走査の `String` は `Val::StrBuf` の内容を、`len` / `is_empty` / `as_str` 以外のメソッドで複製する (`str_method` の先頭)。他のメソッドを O(1) にするのは値の表現を変える作業で、今回は範囲外。
- `todo_spa_scenario` (`tests/wasm_spa.rs`) は `dev/plan` (`f008117`) の時点で既に落ちる。この変更を退避しても落ちる。`035e20f` (wasm 枝) に String の 3 コミットだけを載せると通る。失敗が出るのは wasm 枝を String を含む主流へ取り込んだ `cc939da` からで (`RuntimeError: null function or function signature mismatch`)、主流側のどの変更と組み合わさるかは未確定。
- （前の作業の記録）初回の `cargo test --release` (ビルドの直後) で、別々のテストが一度ずつ落ちた (`spawn ... Text file busy`、ネイティブの実行体の不在、`exit code None`)。直後の再実行では全件通過 (計 4 回)。並列実行でのテストの競合と見られるが、原因は未確定。テストに再試行は足していない。
- `native_ratio_floor.json` の linear_ok の下限 (105) は、現在の 111 に上げられる (note どおり。書き換えていない)。


### `String::as_str` の O(1) 化 (2026-11-15、`std/string-utf8-flag`)

- 最初は `Buf<u8>::as_str` (UTF-8 の検査つき) を通していたので、`as_str` は長さに比例していた (ネイティブで 1 MB を 1 万回: 約 4.6 秒)。
- 直し方: 2 案 (`Buf` に UTF-8 のフラグを持たせる / `String` 専用の組み込み) のうち後者。フラグを持つには、ヘッダの 4 語目か `cap` の最上位ビットが要り、`off: 16` を読む 109 箇所 (Vec・マップ・ファイル入出力を含む) を直すことになるため。
- `String` の組み込み `as_str` は `hdr_view` (ヘッダの {ptr, len} の借用) に戻した。根拠: バイト列 `buf` は非公開 (`private to module alloc.string`、E1122) で、書くのは UTF-8 を保つ `String` のメソッドだけ。`push(char)` の 1〜4 バイトは容量を先に確保してから書き、途中で `as_str` は呼べない。
- 木走査: `Val::StrBuf` を `StrBufV` (内容 + 直前の `as_str` の結果 `Rc<str>`) にした。書き換えは `edit()` を通って結果を捨てる。`len` / `is_empty` / `as_str` は内容を複製しない (`str_method` の先頭)。

計測 (`ksn run` / `ksn run --engine=vm` / `ksn build` の実行体。入力は `as_str().len()` を回数だけ呼ぶプログラム。充填は 100 バイトずつ。最小値):

| 入力 (長さ × 回数) | 版 | ネイティブ | 木走査 | VM |
| --- | --- | ---: | ---: | ---: |
| 100 KB × 100 | 変更前 | 6.0 ms | 28.6 ms | 132.7 ms |
| 100 KB × 100 | 変更後 | 1.0 ms | 29.2 ms | 136.7 ms |
| 1 MB × 1000 | 変更前 | 621.0 ms | 811.2 ms | 255.3 ms |
| 1 MB × 1000 | 変更後 | 1.8 ms | 516.0 ms | 184.7 ms |
| 1 MB × 10000 | 変更前 | 4632.5 ms | 2325.0 ms | 545.9 ms |
| 1 MB × 10000 | 変更後 | 1.7 ms | 443.6 ms | 142.0 ms |

- 変更後は回数に依らない (1 MB の 1000 回と 1 万回で、ネイティブは 1.8 と 1.7 ms、木走査 516 と 444 ms、VM 185 と 142 ms)。木走査と VM の残りの時間は、充填 (`push_str` の繰り返し) と起動。
- 変更前は回数と長さに比例 (1 MB の 1 万回は 1000 回の約 7.5 倍 (ネイティブ)、長さ 10 倍で約 10 倍)。

検証: `stage0/tests/string_kasane.rs` に、書き換えを挟んだ `as_str` の検査 (3 経路で同じ出力) を足した。
- `cargo test --release --no-fail-fast`: 1064 件通過、2 件失敗。`linear_ok` (`kir_native`) は並列実行の一時ファイルの競合 (単独で 89/89 通過)。`todo_spa_scenario` は下の「残り」を参照。
- `native_ratio`: linear_ok 118/118 (下限 105)、examples 14/14、perf 8/8。下限のファイルは書き換えていない。
- `check-stdlib` 0 件、`spec/tools/check_all.py` 全件通過 (`string.sig` の空行を fmtsig の形に直した)。
- examples・apps・selfhost・perf の `ksn check` 0 件、`test --both` 通過、`run` と `build` の出力一致。`examples/kvlib` はライブラリ、`apps/shortener_https` は引数が要る、`apps/todo_spa` は wasm の外部関数が要る (いずれも `run` の失敗は想定どおり)。
- `bench/verify_reference.py`: 19 件中 0 件の不一致。
