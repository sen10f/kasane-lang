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

1. 種を固定した乱数で小さな Kasane プログラムを生成し `ksn check` を通す。
   - 複数関数（相互呼び出し、再帰なし）、構造体（生成・フィールド読み書き・値／`&`／`&mut` 渡し）、利用者列挙体＋`match`、Option／Result を返す関数と `?`。
   - String（`push_str`／`push_u64`／`len`／`as_str`）と `&str` 比較、f64 四則・比較（表示は `to_bits()` を整数で出力）。
   - 整数型 u8〜u64／i8〜i64、シフト、`wrapping_*` / `checked_*` / `saturating_*`。
   - 添字範囲外・`unwrap` の None・0 除算などの panic 経路。
   - 1 行は 100 文字まで（超える式は `let` に分ける）。
   - `ksn check` を通らないプログラムは `check_fail`（生成器の誤り）としてカウント。
2. 4 つの実行系で `fuzz.target` を比較：
   - 木走査 (`ksn run`)、参照実行器 (`kir --run`)、機械語 (`kir --native`)、ASan 機械語。
   - KIR に下ろせない関数は `unsupported` として飛ばす。
   - 終了コード・stdout・panic 文言を比較。`main` が `target` の戻り値をそのまま出力するため、両者で一致するはず。
3. 食い違いが出たら `tools/fuzz/found/<ケース>/` に `program.ksnl` と `result.json` を書き出し、縮小する：
   - 文／ブロック（if／while／match）を単位で消す、式をリテラルに置き換える、使われない関数を消す。
   - 食い違いの種類（どの 2 者がどう違うか）が保たれるかで採否を判断。

## 成果

`tools/fuzz/found/` に食い違いの再現資材が保存される。修正は別の担当。

## 注意

- Rust 処理系 (`stage0/src`) は変更しない。新しいファイルのみ追加。
- `ksn check` に通るプログラムだけを対象とする（`check_fail` は生成器側の問題として報告）。
- `tools/fuzz/found/` は gitignore に入り、コミットしない。
- 動作確認は `stage0` で `cargo build --release` 後のビルド済みバイナリで行う。
