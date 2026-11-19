# Kasane (`ksn`)

Kasane は、LLM が書き、機械が検証する契約重視の言語と、その処理系です。処理系は Rust 製で、ソースの拡張子は `.ksnl`、人がコメントを書く形式は `.ksnh` です。最初のベータ版は `0.1.0-beta.1` として配布予定です。

## 必要な環境

ベータ版をソースからビルドして使う場合、次の環境が必要です。

| 項目 | 内容 |
| --- | --- |
| ホスト OS | Linux（x86_64） |
| Rust | Cargo と Rust 2021 edition 対応ツールチェーン |
| C・C++ ビルド道具 | `clang`（または `gcc`）、`c++`、`ar` |
| LLVM | 既定の組み込み LLVM ビルド用に LLVM 20 の開発ファイル、zlib、zstd の開発ファイル |
| Python | 3.10 以上（仕様検証と統合試験用） |
| Git | ソースの取得と、`git` 型の依存パッケージ用 |

macOS でのホスト実行は未対応です。Windows バイナリは Linux からクロスコンパイルできますが、Windows 上のホスト開発環境も未対応です。詳細は [既知の制限](docs/known-limitations.md) を参照してください。

## 導入

### ビルド済みバイナリ

配布用のビルド済みバイナリは準備中です。

`Prebuilt binary: TBD`

### ソースからビルド

```bash
git clone https://github.com/sen10f/kasane.git
cd kasane
bash scripts/setup.sh
```

`scripts/setup.sh` は必要な道具を確認し、`stage0/target/release/ksn` をビルドして、`examples/demo` の動作確認まで行います。LLVM 20 の開発ファイルと zlib・zstd の開発ファイルも入っていないと、既定の組み込み LLVM 付きビルドは失敗します。ベンチ用の API キーを使う場合だけ、先に `.env.example` を `.env` へコピーして `NVIDIA_API_KEY` を設定してください。

## クイックスタート

ビルドした `ksn` を PATH へ追加します。すべての `ksn` コマンドは、`kasane.toml` があるパッケージ直下で実行します。

```bash
export PATH="$PWD/stage0/target/release:$PATH"
cd docs/tutorial/1
ksn --version
ksn fmt
ksn check
ksn run
```

`ksn fmt` はソースを Kasane の正規形へ整え、`ksn check` は診断を検査します。診断がなければ `ksn check` は何も表示しません。最後の `ksn run` は次の出力を表示します。

```text
こんにちは、Kasane!
```

3つの実行例と詳しい説明は [チュートリアル](docs/tutorial.md) にあります。

## 主なコマンド

| コマンド | 内容 |
| --- | --- |
| `new` / `check` / `fix` | パッケージの作成、型・借用・効果などの検査、自動修正 |
| `fmt` / `fmt --check` | 正規形への整形、整形済みかの検査 |
| `run` / `test` / `test --both` | インタプリタでの実行、テスト、LLVM との結果比較 |
| `build` | LLVM によるネイティブ実行ファイルと可搬バンドルの作成 |
| `debug` / `dap` | インタプリタ上のデバッガ、Debug Adapter Protocol サーバ |
| `lsp` | Language Server Protocol サーバ |
| `doc` / `index` / `context` / `explain` | API 文書、索引、定義の文脈、診断の説明 |
| `refs` / `rename` | 参照の列挙、定義と参照の改名 |
| `get` / `sig` / `update` / `vendor` / `lock` | 依存パッケージとロックファイルの管理 |

## エディタ対応

Kasane 専用のエディタ拡張はまだ提供していません。代わりに、処理系に組み込まれた LSP と DAP を、LSP 対応クライアントまたは DAP 対応クライアントから利用できます。

- `ksn lsp` は標準入出力で LSP を話します。診断、ホバー、定義、参照、リネーム、整形、シンボル、クイックフィックスに対応します。
- `ksn dap` は標準入出力で DAP を話します。インタプリタ上のソースレベルデバッグとして、ブレークポイント、ステップ実行、呼び出し履歴、局所変数に対応します。ネイティブ機械語の DWARF デバッグではありません。

エディタ側の具体的な接続手順は、利用するクライアントの文档に従ってください。

## 文書

- [チュートリアル](docs/tutorial.md)
- [既知の制限](docs/known-limitations.md)
- [変更履歴](CHANGELOG.md)
- [言語仕様](<Kasane 簡易仕様書.md>)
- [実装計画](実装計画.md)
- [設計プラン](設計プラン.md)
- [チートシート](チートシート.md)

## 開発時の検証

```bash
python3 tests/run_tests.py
```

このコマンドは、仕様の機械検証、参照解、Rust のテスト、ビルド、ホットリロード、キャッシュの動作をまとめて検証します。

## ライセンス

`LICENSE: TBD`
