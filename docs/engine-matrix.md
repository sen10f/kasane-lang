# 実行系の対応表

どの機能がどの実行系で動くかの一覧です。セルは `ok` / `退避: X` / `E****` / `未対応` のいずれかで、根拠はソースの場所 (または `docs/known-limitations.md`・仕様) を書きます。実機の小さな試験で確かめたものではなく、**ソースと既存文書から起こした表**です (確かめていない所は「要確認」)。食い違いを見つけたら、この表を直します。

実行系の略号: 木 = 木走査 (`interp.rs`、`--engine=tree`)、VM = `ksn run` / `ksn test` の既定 (`vm/`)、KIR = 参照実行器 `kir_exec.rs` (`ksn kir --run` で利用者から届く)、LLVM = `ksn build` の既定 (`kir_llvm.rs`)、C = `--backend c` (`cnative.rs`、整数・bool・制御フロー・関数呼び出しの部分集合だけ)、wasm = `--target wasm32-unknown-unknown` / `wasm32-wasi`、バンドル = ネイティブへ下ろせないときのインタプリタ同梱の退避 (`cli.rs` の `run_in_bundle` 近辺、デバッグビルドのみ)、デバッガ = `ksn debug` / `ksn dap` (`debug.rs`、木走査)。

| 機能 | 木 | VM | KIR | LLVM | C | wasm | バンドル | デバッガ |
|---|---|---|---|---|---|---|---|---|
| 手書きの `Drop` | 非対応 (追跡しない。パッケージは VM へ送る) | ok | ok | ok | 未対応 | 要確認 | 未対応 (リリースは E9040、デバッグは Drop 付きなら失敗) | E9031 (`cli.rs:1738`) |
| クロージャ (取り込み・返す・欄に持つ) | ok | ok | ok | ok (下ろせない形は退避) | 未対応 | ok (単一スレッド) | ok | ok |
| 関数値・関数ポインタ | ok | ok | ok | ok | 未対応 | ok | ok | ok |
| 総称 (単相化) | ok | ok | ok | ok (`kir_lower_mono.rs`) | 未対応 | ok | ok | ok |
| トレイトオブジェクト | ok | ok | ok | ok (`kir_lower_dyn.rs`) | 未対応 | ok | ok | ok |
| `spawn` / `join` | ok | ok | ok | ok | 未対応 | 未対応 (`kir_llvm.rs:566`、E9040) | ok | ok |
| `spawn` の失敗 | `Err(SpawnError)` でクロージャを返す (`natives.rs:817`) | panic `spawn failed: ..` (101) | 同左 | 同左 | — | — | 要確認 | 木と同じ |
| チャネル・`Mutex` | ok | ok | ok | ok (`kir_lower_chan.rs`・`kir_lower_sync.rs`) | 未対応 | 未対応 (スレッドと同じ理由) | ok | ok |
| `block_on` | ok | ok | ok | ok | 未対応 | ok (単一スレッド) | ok | 要確認 |
| `block_on_threads` | ok (取り消しは次の中断点) | ok | 要確認 | ok | 未対応 | 未対応 | 要確認 | 要確認 |
| テストの `?` (Err/None で失敗) | ok | ok (理由を自前で出す) | — | ok (`ksn test --both`) | — | — | ok (`_exec --test`) | — |
| FFI・`[native]` | ok (`ffi_interp.rs`) | ok | 要確認 | ok | 未対応 (E9040 `cli.rs:4938`) | E9040 (`cli.rs:4934`) | 要確認 | 要確認 |
| `std.tls` | ok (`net.rs`。記述子は実 fd) | ok | 要確認 | ok | 未対応 | 未対応 | 要確認 | 要確認 |
| `std.net` の非同期 (`accept_async` ほか) | ok | ok | 要確認 | ok (`kir_lower_net.rs`) | 未対応 | 未対応 (`kir_llvm.rs:569`) | 要確認 | 要確認 |
| 大きな配列 | ok | ok (領域の余白あり、`ulimit -v` で縮む) | ok | ok | 部分集合 | ok | ok | ok |
| `char` の Debug | ok | ok | ok | ok | — | ok | ok | ok |
| NaN | 木・VM・KIR・LLVM で同じ表現 (メモリの NaN のビットは保証しない。仕様) | 同左 | 同左 | 同左 | — | 浮動小数の書式化は未対応 (`kir_llvm.rs:582`) | 同左 | 同左 |
| 呼び出し深さ 102 | ok (`MAX_DEPTH` = 100000) | ok (`vm/run.rs`) | ok (`kir_exec.rs:145`) | 要確認 (OS のスタック。`stack N`) | — | 要確認 | ok | ok |
| 標準出力のフラッシュ | ok | ok (行ごと。`vm/run.rs:248`) | 要確認 | ok | — | 標準出力のみ | ok | ok |
| ホットリロード | 要確認 | ok (`vm/reload.rs`、`ksn run --watch` 相当) | 未対応 | 未対応 | 未対応 | 未対応 | 未対応 | 未対応 |
| デバッガ (`ksn debug` / `dap`) | ok | 未対応 (木走査) | 未対応 | 未対応 | 未対応 | 未対応 | 未対応 | — |

## 補足

- `ksn run` / `ksn test` の既定は VM (`cli.rs:1817`)。`--engine=tree` か `KSN_ENGINE=tree` で木に戻る。`--engine=both` で比較する。手書きの `Drop` を持つパッケージは `--engine=tree` を明示しても VM で走る。
- `ksn build` の既定は LLVM。下ろせない・clang が無いときは、デバッグならバンドルへ退避し、`--release` は E9040 で失敗する (`--allow-bundle` で退避を許す)。`--backend clif` は廃止 (E9040)。
- 対応する target は `x86_64-linux-gnu`・`aarch64-linux-gnu`・`x86_64-windows-gnu`・`wasm32-unknown-unknown`・`wasm32-wasi` (`cli.rs:1312` 近辺)。wasm の「未対応」は `kir_llvm.rs` の `wasm_unsupported` が理由つきで拒む。
- 関連: `docs/known-limitations.md`、`docs/checker-rule-map.md`。
