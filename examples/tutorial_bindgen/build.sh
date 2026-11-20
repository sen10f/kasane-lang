#!/bin/sh
# C ライブラリを共有ライブラリにする。`ksn run` / `ksn test` はこのディレクトリ
# (kasane.toml のあるところ) を作業ディレクトリにして動かす
# (link "libdemo.so:..." は相対パスでこのディレクトリの libdemo.so を引く)。
set -e
cd "$(dirname "$0")"
cc -shared -fPIC -O2 -o libdemo.so c/demo.c
