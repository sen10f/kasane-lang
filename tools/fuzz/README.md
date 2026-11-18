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

## 生成器の第 2 版 (`gen2.py`、既定)

- KIR に下ろせる範囲の機能 (整数の演算、構造体の欄の読み書きと `&mut` 渡し、ペイロード付き enum と `match`、
  `Option` と `?`、Vec の push / get / `for`、上限付きの `while`、関数の呼び出し) を、引数と前の値に依存する
  計算として組み合わせ、中間値をすべて畳み込んだ u64 を返す。panic はまれに起きる (`+` の溢れ、範囲外の添字)。
- 第 1 版 (`--gen 1`) は、宣言した型を使わず値が定数ばかりで、ほとんどが KIR に下ろせなかった。
- 比べる相手は 5 つ: 木走査・参照実行器・`--opt` の参照実行器・機械語・ASan。
- 目安 (`--seed 2 --count 400`): 400 本すべてが KIR に下ろせ、32 本が panic で終わり、食い違いは 0。
- KIR が未対応のため使っていないもの: `u64.try_from_i64` などの `try_from_*`、`derive [Clone]` の `clone`、浮動小数。

## 借用・ライフタイムの穴探し (`fuzz_borrow.py` / `gen_borrow.py`)

検査器の健全性 (修正プラン 1-5) を探す。差分実行ではなく、生成時に付けたラベルと `ksn check` の判定を照合する。

```sh
python -I tools/fuzz/fuzz_borrow.py --ksn stage0/target/release/ksn --seed 1 --count 2000 --jobs 8
FZB_MSG=1 ...   # 診断コードにメッセージ (90 字) を付ける
```

- 借用の元 (`as_str` / u32 / String の参照 / `&mut u32`) × 包み方 (そのまま・恒等関数・再借用・`fst`/`snd`・`if`・`let`) ×
  出口 (代入・ブロックの値・フィールド・構造体・Option・タプル・Vec の push・`&mut` 引数・メソッドのセッタ・
  クロージャの取り込み・ヘッダ付き戻り値) × 元が内側 / 外側、に加えて借用中の変更・move・代入の形を生成する。
- `bad` (不健全) なのに診断 0 件 → `hole`。`ok` なのに診断が出る → `false_pos`。`false_pos` の診断コードが
  E0xxx / E1xxx / E2xxx なら生成器の誤りなので、まず `FZB_MSG=1` で確かめる。
- 出力: `--out` (既定 `tools/fuzz/found_borrow/`、gitignore) に形ごと 1 本。穴は手で最小化し、
  `stage0/tests/data/unsound/` に (横の `.expect` に `expect: Exxxx` を書いて) 足し、`stage0/tests/check_diag.rs` の `KNOWN_HOLES` へ載せる。
- タグの `A1` / `A2` は修正プランの既知の穴 (ヘッダ照合・セッタ)。新しい穴の判定からは除く。
- 並列度は 8 まで。
