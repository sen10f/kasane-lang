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

### 段 2 (済): KIR の下ろし (`kir_lower_map.rs` を外した、2026-11-15、`std/map-native`)

KIR の `HashMap` / `BtreeMap` は、専用の 24 バイトのヘッダ (`kir_lower_map.rs` と `kir_lower_mapstr.rs`、合わせて約 2,000 行) から、
`spec/stdlib/alloc/collections.ksnl` の Kasane 版の集成体 (`{cells: Buf<HashCell<K, V>>, slots: usize}` と `{entries: Buf<MapEntry<K, V>>}`) に変えた。
メソッドは利用者の総称型と同じ経路で、単相インスタンス (`alloc.collections.HashMap.insert<K,V,A>` など) として下ろす。
前提の 5 つは次のように解いた:

1. **型表**: `core.hash.Fnv64` は `STD_TYPES` に入れた。`HashCell` / `HashMapIter` / `MapEntry` / `HashMap` / `BtreeMap` は
   `is_kasane_std_generic` (`kir_lower_agg.rs`) で、利用者の総称型と同じく `intern_generic` で集成体にする。列挙子の値 `HashCell.Empty` は、型引数を
   式の型 (型検査の結果) から取る (`variant_make` の `e`)。
2. **整数の `hash`**: `x.hash(&mut state)` は `core.hash.fnv_int` (`hash.ksnl`、FNV-1a を型幅のバイトに対して回す) の呼び出しに下ろす。
   木走査の `Hash` (`natives.rs` の `hash_bytes`) と同じバイト列。`&str` / `String` の `hash` は `Fnv64.write_bytes` へ、`eq` / `cmp` / `partial_cmp` は `str_eq` / `string_cmp_method` へ下ろす。
3. **`HashMap` の表現**: 集成体 (32 バイト、`cells` の `Buf` が 24 バイト) で、フィールドの `Buf` は既存の `Buf` の下ろしを使う。
   `HashMap.new` は KIR だけ `new_empty` (本体のある Kasane 版) を呼ぶ (`call` の振り分け)。木走査は `NATIVE_NAMES` の `Val::Map` のまま (本体のある定義が優先されるため、名前を変えた)。
4. **反復子の `for`**: `for PAT in IT BODY` で `IT` が `next` を持つ集成体のとき、反復子を**スタックの局所**に置いて毎周 `next(&mut it)` を呼び、`Some(PAT)` を束縛する
   (`user_iter_for`、`kir_lower_coll.rs`)。局所の反復子は従来の `user_iter` (値を合流の引数に載せる) のまま。値のままだと周ごとに初めの値へ戻るので、局所は番地で持つ。
5. **`HashMap.new`**: 上の 3 と同じ。`new_empty` は `alloc.raw.Buf.new` と構造体の組み立てだけ。

このほか、下ろしに足したもの: `core.mem.replace` (`mem_replace`)、列挙型の参照の `match` (`match e { E.A(x) => .. }` の `x` は番地。`FieldAddr` は列挙型に使えないので `PtrAdd`)、
`Option<&T>` / `Option<&mut T>` の番地 (`bref`) を集成体の要素と `&mut` のスカラーにも広げた (`bref_ref`、`pick_elem` の `get` の要素)、
`bref` の組の束縛 (`PTuple`)、`unwrap` の `Option<&T>` (`unwrap_bref`)、`&&str` の型 (`ref<fat>`)、
総称型の名前に空白を入れない (KIR のテキストのヘッダは空白で区切るため)、欄が `&スカラー` で要素が値のときは一時の枠に置いて番地を渡す (`tuple_e`)。

**借用の穴を塞いだ点**: `m.insert(k.as_str(), v)` のあと同じ関数で `k` を変えると、マップが解放後の文字列を読む。KIR の借用検査は、汎用の型 (マップ) の中に
保存された借用を追わない。そこで、キーが `&str` で、同じ関数の本体に `k` を変えるメソッド (`push_str` など) があれば、下ろしを拒む
(`call_method` の検査、理由は「元の String が関数の中で変更される」)。`ksn check` は通るが、KIR はこの形を下ろさない。

**残した下ろしと理由**: 集成体の値を読む `Op::Load` は、スカラー・ポインタ・集成体の番地だけ (集成体の番地からは欄を読む)。`Option<&スカラー>` は値のまま (設計_KIR.md 10.10)。

**外した下ろし**: `kir_lower_map.rs` (1,333 行、`Kind::Map` / `Kind::BMap` の全部、`Shape` の 3 種類のキー、`try_for`、`drop_owned_keys`)、
`kir_lower_mapstr.rs` (705 行、文字列キーの `BtreeMap` と `drop`)。残したもの (マップに依らない):
`Fb` (関数の組み立て) は `kir_lower_fb.rs` へ、文字列の比較 (`string_order` / `string_cmp_method` / `const_str_call` と `ksn.bs.cmp`) は `kir_lower_strord.rs` へ移した。

### 段 3 (済の部分と外せなかった部分): チャネル

- `kir_lower_map.rs` の型ごとの下ろしは、段 2 で外した (上)。
- チャネル (`std.sync`) は、Kasane の `Mutex` + `Buf` の待ち行列には**まだ移せない**。ネイティブの待ちは `ksn.thr.wait` (条件変数) と `ksn.thr.{lock,unlock}` で、
  `gen` / `blocked` / `live` の数で全員待ちの deadlock を検出する (設計_KIR.md 22.7)。Kasane の側に条件変数の原語がなく、待ちを Kasane から書けない。
  木走査 (協調実行) と VM は別の待ち方をする。移すには、条件変数を標準ライブラリの原語 (`unsafe` の境界の中) として決める必要がある (未決)。

### 段 2・段 3 の性能 (ネイティブ、LLVM、`perf/run_perf.py`)

制御した計測 (`perf/hashmap` の入力だけを N = 300,000 に増やした copy を `/tmp` で作り、`ksn build --release` の実行体を交互に 10 回ずつ走らせた。
挿入 300,000 件と検索 600,000 件 (半分は不在)。単位 ms、中央値 (最小値)):

| 版 | 中央値 | 最小値 |
| --- | ---: | ---: |
| 組み込みの KIR の下ろし (dev/plan、`kir_lower_map.rs`) | 68 | 63 |
| Kasane 版 (本段) | 81 | 78 |

比は 中央値 1.19、最小値 1.24。目安の 1.2 倍にほぼ並ぶが、わずかに超える。ハッシュは `Fnv64` の FNV-1a を型幅のバイトに対して回すので、
組み込み (`kir_lower_map.rs` の混ぜ関数) より 1 件あたりの命令が多い。改善の候補は、整数キーの `fnv_int` の展開と、`find` の 2 段の `get`。

`perf/run_perf.py` (N = 2,000、5 回の中央値。負荷の高い時間帯で、起動の時間 (1〜2 ms) が大半):
hashmap 基準 6.0 ms / 新 1.3 ms、vecops 4.5 / 2.9、structs 2.6 / 1.6。入力が小さく、同じ時間帯の計測でないので、比としては使わない
(退行は見えないが、ばらつきの範囲)。

### 段 2・段 3 の検証

- `tests/native_ratio.py`: 下限を下回らない (下の数字)。`ksn check-stdlib`: 0 件。examples・`selfhost/parse`・`apps/todo` の `check` は 0 件。
- examples の全パッケージで `ksn build --backend llvm` が下り (バンドルの退避 0)、ネイティブと `ksn run` の出力が一致 (`kvlib` は library のため対象外)。
- 新しいテスト `stage0/tests/kir_map_kasane.rs`: 構造体のフィールドに置いた `HashMap<String, 構造体>` を `self.map.insert` / `get` / `get_mut` で使う (記号表の必須形)、
  `get_mut` が書いた値が残る、`for (k, v) in m.iter()` の挿入順 (削除後も相対順を保つ)、`BtreeMap<u32, _>` の操作。すべて木走査とネイティブ (退避なし) の出力が一致。
- 既存のテストの期待の変更 (理由つき): `kir_lower3.rs` の `kir_map_reports_unsupported_methods_with_a_reason` は、`get_mut` が下りるようになったので
  `kir_map_get_mut_in_a_match_lowers_and_writes_through` に置き換えた (限界の表示の試験から、値が書き換わる試験へ)。
  `tooling.rs` の `str_slice_option_result_sig_methods_run_in_both` と `derived_cmp_and_iter_count_boundaries_run_native` は、`&str` の比較が
  下りるようになったので、その 2 件 (`test_c_str_derived_cmp`、`test_g_str_boundaries`) の期待を「機械語で動く」に変えた。

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

- `String::as_str` は `Buf::as_str` の UTF-8 検査を通るので、呼び出しごとに長さに比例する (以前は O(1) のヘッダの見せ方だけ)。文字列の性能の計測には入っていない。検査を省くには不変条件に頼る経路 (unsafe か、検査済みの印) が要る。オーナーの判断。
- 初回の `cargo test --release` (ビルドの直後) で、別々のテストが一度ずつ落ちた (`spawn ... Text file busy`、ネイティブの実行体の不在、`exit code None`)。直後の再実行では全件通過 (計 4 回)。並列実行でのテストの競合と見られるが、原因は未確定。テストに再試行は足していない。
- `native_ratio_floor.json` の linear_ok の下限 (105) は、現在の 111 に上げられる (note どおり。書き換えていない)。
