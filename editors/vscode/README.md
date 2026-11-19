# Kasane VS Code 拡張

Kasane の構文強調と、`ksn lsp` による補完・診断・定義参照などを提供する VS Code 拡張です。

## 必要なもの

- VS Code 1.82 以上
- Node.js 20（確認環境: v20.18.1）
- Kasane の `ksn` 実行ファイル（通常は `PATH` に置きます）

## インストール

リポジトリの `editors/vscode` で次を実行します。

```sh
npm install
npx tsc -p .
npx @vscode/vsce@2.24.0 package --allow-missing-repository
```

`@vscode/vsce` の最新版は Node.js 20 で `ERR_REQUIRE_ESM` となるため、`2.24.0` を固定しています。`--no-dependencies` は指定しません。起動に必要な `vscode-languageclient` を VSIX に含めるためです。

作成した `.vsix` は、VS Code の「拡張機能: VSIX からインストール...」（Install from VSIX）コマンドでインストールできます。

## 設定

| 設定 | 既定値 | 説明 |
| --- | --- | --- |
| `kasane.ksnPath` | `ksn` | `ksn` 実行ファイルへのパスです。PATH にない場合は絶対パスを指定してください。 |

## 動作

- `.ksnl` と `.ksnh` を Kasane 言語として扱います。
- `ksn lsp` を stdio で起動し、診断・補完・ホバー・定義参照・参照検索・改名・文書記号・クイックフィックスを利用します。
- ファイルを保存すると、言語サーバの `textDocument/formatting` で整形します。現在の `ksn lsp` はこの機能をサポートしています。
- コメントは `//` 形式です。コメントを許すのは人間向けの `.ksnh` だけで、`.ksnl` にコメントを書くと言語サーバが診断します。

## 開発時の検査

```sh
npm run check
```

文法的 JSON を確認し、`vscode-textmate` があれば `examples/demo/src/*.ksnl` もトークン化します。
