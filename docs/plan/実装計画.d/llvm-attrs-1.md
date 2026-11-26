# llvm-attrs-1: LLVM に Kasane の保証を伝える (手順 1-2)

## 変更
- `nounwind`: 生成する全 `define` (ユーザ関数・main・ksn.rt.entry・ksn_resume・深さの包み) に付けた。panic は abort で巻き戻さない。FFI のコールバックも C から巻き戻されない。
- 製品ビルドの最適化を `-O2` → `-O3` (`cli.rs` の 2 か所)。
- `&T` 引数 (`Ty::Ref(false, p)`) に `noalias readonly`。`kir_llvm.rs` の `may_have_interior_mut` が「内部可変性があるかも」を 1 か所で決める (迷えば真 = 付けない)。
  偽になるのは整数・浮動小数・bool と、それだけからなる構造体・タプル・配列。名前に atomic/mutex/rwlock/cell/chan/sender/receiver/arc/once/lazy/sync/thread/condvar/barrier を含む型、enum、ptr/Option を含む型は真。
  `shared_ref_attrs_ok` で、extern・`core.ptr`/`std.ffi`/`ksn.` の関数、`ptr` の引数・戻り値・値を持つ関数 (unsafe/FFI に近いもの) は属性なし。
- `&mut` の `noalias`、`nonnull`/`dereferenceable`/`align` は付けていない (参照が生ポインタ由来で無効でないと KIR では言い切れないため。手順 3 以降)。

## null_pointer_is_valid (9 か所) — 全部残した
理由は `core.ptr.read(null)` を最適化で消さず実際の fault にすること。ユーザ関数の定義 1 (`kir_llvm.rs` の関数定義)、`main` 4 (通常・スレッド版・wasm・ほか)、`ksn.rt.entry`/`entry0`、`ksn_resume`、深さの包み。
手書きの入口・包みは生ポインタを触らないが、`entry0` は本体を呼ぶだけで、外しても利得が測れていない。未検証のため外していない (次の課題)。

## 未実施
- ランタイム C の LTO・`-fno-builtin` の見直し (`-O1 -fno-builtin` のまま)。
- ASan の `examples/` 一周。TSan は `cargo test --release` の threads 系で通過。

## 検証
`cargo test --release` 全件通過、FAST GATE: check_all OK、verify_reference bad 0、native_ratio 187/187・18/19 (ui_demo は libksnui.a 無しで元から)・perf 8/8、par_test fast 940 passed。
`perf/run_perf.py` は fib の EXPECT 不一致で元の ksn でも落ちる (既存)。

## 計測 (release、3 回の中央値、秒。load 8 前後。micro の元の 6 本の資料は /tmp から消えていたため、`&T` 引数を使う 2 本を書き直した)
| 課題 | 前 | 後 |
|---|---|---|
| callchain (`&u64` 2 段、4 億回) | 0.56 | 0.58 |
| sref (`&struct` 3 欄、3 億回) | 0.24 | 0.10 |
perf/ の各パッケージは入力が小さく (数 ms) 差は出ない。Rust との比較は未実施。

## 追加 (2026-11-25、統括者の依頼への対応)
- `&T` 引数に `nonnull dereferenceable(<size>)` を追加 (大きさが分かるときだけ)。`align` は付けない: KIR の `load`/`store` が `align 1` で、参照の整列が型で保証されていない。
- 正しさの照合 `tests/native_vs_interp.py` (`ksn run` と release のネイティブの標準出力・終了コードを比較): linear_ok 187 一致、check_ok 29 一致・21 skip (入口なし/ネイティブ不可)・1 不一致、examples 16 一致・3 skip、perf 8 一致。
  不一致の 1 本 `check_ok/question_ok_fs_read_utf8_with` は元の ksn の `run` も `Alloc(AllocError)` で落ちる (インタプリタ側の既存の問題、属性とは無関係)。
  ASan (`KSN_CLANG_ARGS=-fsanitize=address`) の examples 16 + perf 8 も不一致 0。`cargo test --release --test threads --test net_async_mt` (TSan) 通過 (16 / 26)。
- `perf/run_perf.py` の EXPECT 不一致の原因は、定数の整数リテラルの `_` 区切り (`196_418u64`) を正規表現が読めなかったこと (実行ファイル名の件ではない)。直した。
- micro 6 本は `~/kasane-verify/agents/perf-codegen/bench/` (Kasane + `rs/` の Rust、`run.py`)。
| 課題 | dev/plan | 本ブランチ | Rust -O3 |
|---|---|---|---|
| vecsum | 0.20 | 0.20 | 0.21 |
| strbuild | 0.11 | 0.10 | 0.11 |
| hashmap | 0.26 | 0.25 | 0.22 |
| soa | 0.21 | 0.18 | 0.17 |
| callchain (値渡し) | 0.15 | 0.16 | 0.21 |
| bytescan | 0.30 | 0.29 | 0.26 |
(load 12 前後、3 回の中央値。値渡しの課題には `&T` の属性は効かない。`&T` 引数の sref は 0.24 → 0.10。)
