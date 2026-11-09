# Kasane (`ksn`)

`Kasane 簡易仕様書.md` (v0.9) に基づく処理系。実装は Rust 製 (`stage0/`)。拡張子は `.ksnl`。

`cd stage0 && cargo build --release` で `stage0/target/release/ksn` ができる。`ksn.sh` / `ksn.cmd` はそれを起動する。
字句・構文・ヘッダ・名前解決・lint・文法検査・型/副作用検査・インタプリタ・C 経由のネイティブ生成・ビルドキャッシュ・ホットリロードを含む。
関連文書: `実装状況.md` (仕様との対応表)、`方針まとめ.md`、`設計案_小型モデル向け.md`。

| コマンド | 状態 |
| --- | --- |
| `new` `get` `sig` `refs` `rename` `explain` `update` `vendor` | 実装済み |
| `check [--json]` | 構文・正規形(文法+lint)・名前解決・引数の個数と型(局所推論)・可変性・move後使用・線形値の未消費・`match` 網羅・再帰と `stack`・`uses`/`panics`・層・循環・パッケージ検証 |
| `fix` | `check` の修正案(`fix`)を適用して `fmt` で整える。未使用の `_` 化、`let _ = e` / 値を捨てる式文、存在しないメソッド名の置換 |
| `fmt [--check]` | 空白・空行・定義順・`derive` 順・メソッド `impl` 行の位置を直す。本体の折り返し等は `check` が報告 |
| `run` / `test [--both]` | インタプリタで実行。`--both` はインタプリタとビルド成果物で結果の一致も確認 |
| `run --watch` | ホットリロード(外部面不変の関数本体のみ差し替え、他は restart 要求) |
| `build [--release] [--target T] [--backend c\|clif]` | 自己完結のバンドル(`target/<debug|release>/`)を生成。キャッシュ(キー=ソース+版+edition+種別+ターゲット、LRU、`KSN_CACHE_MAX`)つき |

## 制限 (仕様との差)

- `build` は常にインタプリタ入りの可搬バンドルを作り、収まる場合は C(gcc)経由のネイティブ実行ファイルも作る。`--backend clif` で Cranelift バックエンドを使うと、構造体・列挙型・`Option` / `Result`・`Vec` / `String`・クロージャ・イテレータ・`?` を含む範囲までネイティブにできる([バックエンド.md](バックエンド.md))。LLVM は無い。
- 型検査は関数内の局所推論(リテラル接尾辞・注釈・関数シグネチャ)に基づく保守的なもので、完全な型推論・トレイト解決・ライフタイム/NLL 借用検査ではない。`stack` の上界は簡易見積り。
- 型検査には、トレイト境界(E2030)・字句的な借用衝突(E3010)・ライフタイム省略(E3020)も含む。
- スレッドは `spawn` 時に同期実行。セルフホストは無い。

## テスト

```
python tests/run_tests.py   # spec 検証 + cargo test + ビルド/ホットリロード
```
