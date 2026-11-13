# Kasane (`ksn`)

`Kasane 簡易仕様書.md` (v0.10) に基づく処理系。実装は Rust 製 (`stage0/`)。拡張子は `.ksnl`。

関連文書: `実装計画.md` (仕様との対応表・仕様の修正案・ロードマップ・引き継ぎ)、`設計プラン.md` (フルスタック化のロードマップ)、`チートシート.md` (LLM に渡す書き方の要約)、`bench/README.md` (LLM ベンチ)、`bench/http/README.md` (HTTP サーバの性能比較)。

## セットアップ

```
cp .env.example .env          # 必要な環境変数を埋める (.env はコミットされない)
bash scripts/setup.sh         # 依存の確認と ksn のビルド、examples/demo の動作確認
bash scripts/setup.sh --test  # さらにフル検証 (python tests/run_tests.py)
bash scripts/setup.sh --bench # さらに NVIDIA NIM ベンチの準備確認 (API キーの有無だけを見る)
```

必要なもの: Rust (cargo)、gcc か clang (ネイティブ生成のリンクと C ランタイムのコンパイル)、Python 3.10 以上、git。Go は任意 (`bench/http` の比較用)。

クラウドの開発環境（Claude Code のクラウド）では、環境設定の「セットアップスクリプト」に `scripts/cloud-setup.sh` の中身を貼り（または `bash scripts/cloud-setup.sh` を指定し）、「環境変数」に `.env.cloud.example` の内容を貼って `NVIDIA_API_KEY` の値を入れる。ネットワークは crates.io・rustup（Rust が無い環境のみ）・`integrate.api.nvidia.com`（ベンチのみ）への外向きの通信が要る。ビルド後の `ksn` は `stage0/target/release/ksn`、`ksn.sh` / `ksn.cmd` はそれを起動する。

## コマンド

| コマンド | 内容 |
| --- | --- |
| `new` `get` `sig` `context` `todo` `explain` `update` `vendor` | 雛形・定義の取得・説明・依存 |
| `check [--json]` | 構文・正規形・名前解決・型 (fail-closed: 決まらなければ E2044)・可視性・所有権と借用 (NLL)・線形型 (全経路の消費)・効果と panic・`stack`・パッケージ検証 |
| `fix` / `fmt [--check]` | 修正案の適用 / 正規形への整形 |
| `refs` / `rename [--dry-run]` | 参照の一覧 / 名前の変更と定義・モジュールの移動 (書き込む前に `check` する) |
| `run [--watch]` / `test [--both]` | インタプリタで実行 / テスト (`--both` はネイティブとの一致も確認。`tests/` の統合テストを含む) |
| `build [--release] [--backend clif\|c\|llvm]` | ネイティブ実行ファイル (既定は Cranelift。LLVM は KIR 経由で `--backend llvm`、正式なネイティブ生成は今後 LLVM) と可搬バンドル。キャッシュつき |
| `doc [--std] [--private]` | 静的 HTML の API 文書 |
| `lsp` / `dap` | Language Server / Debug Adapter |
| `debug [--json]` / `test --debug <FQN>` | インタプリタ上のデバッガ |

## 制限 (仕様との差)

`実装計画.md` の 2 章 (実装状況) と 7 章 (リスク) を参照。主なもの: 型付き IR は未完成 (ブランチ `ir/tir` に途中経過)、クロスコンパイルなし、LLVM は KIR 経由 (`ksn kir --llvm` / `--native`、`設計_KIR.md`) で途中、WASM なし、セルフホストなし。

## テスト

```
python tests/run_tests.py   # spec 検証 + 参照解 + ベンチのハーネス + cargo test + ビルド/ホットリロード/キャッシュ
```
