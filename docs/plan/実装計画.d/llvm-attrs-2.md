# llvm-attrs-2: `&mut` の noalias とメモリ効果の属性 (手順 3-4)

## 変更 (`stage0/src/kir_llvm.rs`)
- 手順 3: `&mut T` 引数に `noalias nonnull dereferenceable(size)`。除外は手順 2 と同じ (`shared_ref_attrs_ok`: extern・`core.ptr`/`std.ffi`/`ksn.`・Ptr を引数/戻り値/値に持つ関数、`may_have_interior_mut` が真の指す先) に加え、キーが `std.` `alloc.` `core.` で始まる関数 (`split_at_mut` 系や 1 つの確保から 2 つの `&mut` を作る unsafe を含みうる) には付けない。スライス (`&mut [T]`) は 128 ビットの値渡しなので対象外。
- 手順 4: `mem_effects`。純粋域 (`pure_region`、深さの記録の無い版) で `uses` が空・`panics never`・`.slow` でない関数に `memory(none)` / `memory(argmem: read)` / `memory(argmem: readwrite)`。本体の命令は許す一覧 (算術 (Trap と除算・剰余を除く)・比較・集成体の組み立て・Load/Store/FieldAddr・alloca・注釈・純粋域の呼び先) だけで、1 つでも外れたら付けない (確保・静的領域・生ポインタ・IO・数学関数 (errno)・FnPtr など)。呼び先の効果の最大を採る。`willreturn` は付けない。

## 正しさ
- `tests/native_vs_interp.py`: 不一致 0 (linear_ok 187、check_ok 33 (skip 25)、examples 16、perf 8)。ASan (examples + perf) 不一致 0。
- 差分生成器 (hunt3 の `gen.py` を /tmp にコピーして 100 本、インタプリタとネイティブ): 不一致 0 (93 本が実行、7 本は元から skip)。
- TSan 系 `cargo test --release --test threads --test net_async_mt`: 19 / 26 通過。
- `unsound/` を許して通す旗は無いので省いた。

## 計測 (release、3 回の中央値、秒。load 4〜5)
| 課題 | dev/plan | 本ブランチ | Rust -O3 |
|---|---|---|---|
| vecsum | 0.20 | 0.19 | 0.20 |
| strbuild | 0.11 | 0.11 | 0.12 |
| hashmap | 0.25 | 0.24 | 0.17 |
| soa | 0.18 | 0.18 | 0.17 |
| callchain | 0.12 | 0.12 | 0.17 |
| bytescan | 0.24 | 0.24 | 0.21 |
| mutref (新規: `&mut struct` を 4 億回 + `&mut u64` + `&mut [u64]` カーネル) | 0.18 | 0.17 | 1.10* |
\* Rust 側は `#[inline(never)]` で関数呼び出しを残したため不公平 (Kasane は展開・畳み込み済み)。差は誤差の範囲で、既存の課題に `&mut` の noalias が効く形が無い。`bench/mutref/` と `rs/mutref.rs`。

## 未実施・注意
- `&mut` の `noalias` は、検査器が `&mut` の重なりを拒む前提に依る (穴は修正済みという依頼)。穴が再発すると静かな誤コンパイルになるので、`unsound/` の再現を足したら本スクリプトも回す。
- スライスの `&mut [T]` は ptr+len の値渡しで、`noalias` の対象外 (別の手段が要る)。
