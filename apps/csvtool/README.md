# csvtool

CSV/TSV の集計・抽出ツール。`ksn` で動く。

## 使い方

```
csvtool <summary|groupby|head|select|count|cols> <path> [column...]
```

列の指定は 1 基準の番号 (`2`) またはヘッダー名 (`amount`) のどちらでも可。区切り文字は拡張子 (`.csv`=カンマ, `.tsv`/`.tab`=タブ) と中身から自動判別。

## サブコマンド

| コマンド | 説明 |
| --- | --- |
| `summary <path> [column...]` | 列ごとの件数・合計・最小・最大・平均を表示。列を省略すると全列。 |
| `groupby <path> <key> <val>` | キー列の値ごとに値列を集計し、キー順に表示。 |
| `head <path> <n>` | 先頭 n 行をヘッダー付きで表示。 |
| `select <path> <column...>` | 指定列だけを抽出して表示。 |
| `count <path>` | データ行数だけを表示。 |
| `cols <path>` | 列番号とヘッダー名の一覧を表示。 |

## 実行例

```bash
# インタプリタで実行
ksn run -- summary data.csv
ksn run -- groupby data.csv city age
ksn run -- head data.csv 5
ksn run -- select data.csv name city
ksn run -- count data.csv
ksn run -- cols data.csv

# デバッグビルド (インタプリタ同梱バンドル) を作って実行
ksn build
./target/debug/csvtool summary data.csv

# リリースビルド (ネイティブ実行ファイル) を作る場合
# ※現状、このアプリは KIR 経由のネイティブ生成に未対応 (E9040)
# ksn build --release --allow-bundle
```

## 開発

```bash
ksn check           # 型検査など
ksn fmt --check     # 正規形チェック
ksn test --both     # テスト (インタプリタとネイティブの一致確認)
ksn build           # デバッグビルド
```