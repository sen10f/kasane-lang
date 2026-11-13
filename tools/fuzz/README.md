# tools/fuzz — Kasane 差分ファジング道具

標準ライブラリのみ (`python -I` で実行可能)。

## 使い方

```sh
python -I tools/fuzz/fuzz.py \
  --ksn <ksn バイナリのパス> \
  --seed <n> \
  --count <n> \
  --jobs <n>
```

- `--ksn`: `stage0/target/release/ksn`（リリースビルド済み）。
- `--seed`: 乱数の種。
- `--count`: 生成・比較するプログラム数。
- `--jobs`: 並列子プロセス数（`taskset -c 0-9,12-31` で起動。CPU 10/11 は不安定のため）。

## 動作

1. 種を固定した乱数で小さな Kasane プログラムを生成（`examples/` の作法に従う）。
   - 整数・bool・浮動小数の式（四則、比較、`wrapping_*` / `checked_*`、シフト）、`if` / `while` / `match`、関数呼び出し、`Option` / `Result`（`Vec.get`）、Vec の push と添字、ループ、panic。
   - 1 行は 100 文字まで。
   - `ksn check` を通らないプログラムは捨て、`check_fail` としてカウント（生成器の誤り）。
2. 4 つの実行系で比較：
   - 木走査 (`ksn run`)
   - 参照実行器 (`ksn kir <FQN> --run <args>`)
   - 機械語 (`ksn kir <FQN> --native`)
   - ASan 機械語 (`KSN_CLANG_ARGS="-fsanitize=address -O0 -g"`)
   - stdout・終了コード・panic 文言を比較。KIR に下ろせない関数は `unsupported` として飛ばし、件数を数える。
3. 食い違いが出たら `tools/fuzz/found/<ケース>/` に `program.ksnl` と `result.json` を書き出し、文を 1 つずつ消して食い違いが残るか確かめながら縮小する。

## 成果

`tools/fuzz/found/` に食い違いの再現資材が保存される。修正は別の担当。

## 注意

- Rust 処理系 (`stage0/src`) は変更しない。新しいファイルのみ追加。
- `ksn check` に通るプログラムだけを対象とする（`check_fail` は生成器側の問題として報告）。
- `--count 500` の動作確認は `stage0` で `cargo build --release` 後のビルド済みバイナリで行う。
