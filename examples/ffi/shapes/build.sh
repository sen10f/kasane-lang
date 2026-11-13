#!/bin/sh
# C ライブラリを共有ライブラリにする。`ksn run` / `ksn test` / ネイティブ生成した実行ファイルは、
# このディレクトリ (kasane.toml のあるところ) を作業ディレクトリにして動かす (link "./libshapes.so:..." は相対パス)。
set -e
cd "$(dirname "$0")"
cc -shared -fPIC -O2 -o libshapes.so c/shapes.c
