#!/usr/bin/env bash
# scripts/release.sh VERSION
#
# リリース用のビルド済み ksn を dist/ に作成する。
#
#   bash scripts/release.sh 0.1.0-beta.1
#
# 作られるもの:
#   dist/ksn-VERSION-x86_64-linux-gnu.tar.gz
#     ksn (実行体)、LICENSE-MIT、LICENSE-APACHE、README.md、INSTALL.txt
#   dist/SHA256SUMS
#
# ksn は KSN_LLVM_PREFIX=$HOME/llvm-ksn/install (scripts/build_llvm.sh の自前 LLVM)
# で CARGO_TARGET_DIR=dist/build の別ターゲットディレクトリにリリースビルドし、
# strip してから ldd で実行時の共有ライブラリが libc / libm / libgcc_s / ld-linux のみか検査する。
# スクリプト内でスモークテストも行う (展開先で clang の無い PATH で ksn --version / new / run / build --release / ./target/release/hello.native)。
set -euo pipefail

[ "$#" -eq 1 ] || { echo "usage: $0 VERSION" >&2; exit 2; }
VERSION="$1"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PREFIX="$HOME/llvm-ksn/install"
if [ ! -d "$PREFIX" ]; then
  echo "release: LLVM のプレフィックスが見つからない: $PREFIX" >&2
  echo "   まず scripts/build_llvm.sh を実行して KSN_LLVM_PREFIX=$PREFIX を作ってください" >&2
  exit 1
fi
export KSN_LLVM_PREFIX="$PREFIX"

rm -rf "$ROOT/dist"
mkdir -p "$ROOT/dist"

echo "release: ksn をリリースビルドする (KSN_LLVM_PREFIX=$KSN_LLVM_PREFIX, CARGO_TARGET_DIR=$ROOT/dist/build)"
(cd stage0 && CARGO_TARGET_DIR="$ROOT/dist/build" cargo build --release)

KSN="$ROOT/dist/build/release/ksn"
[ -x "$KSN" ] || { echo "release: ksn がビルドされていない: $KSN" >&2; exit 1; }

strip --strip-all "$KSN"

# 実行時の共有ライブラリは libc / libm / libgcc_s / ld-linux のみ (LLVM は静的リンク)。
bad="$(ldd "$KSN" | grep -vE 'linux-vdso\.so\.1|libm\.so|libc\.so|libgcc_s|ld-linux' || true)"
if [ -n "$bad" ]; then
  echo "release: ksn に許可されていない共有ライブラリがあります:" >&2
  echo "$bad" >&2
  echo "   許可: libc / libm / libgcc_s / ld-linux のみ" >&2
  exit 1
fi

name="ksn-$VERSION-x86_64-linux-gnu"
stage="$(mktemp -d)"
cp "$KSN" "$stage/ksn"
cp "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$ROOT/README.md" "$ROOT/INSTALL.txt" "$stage/"
tar -czf "$ROOT/dist/$name.tar.gz" -C "$stage" ksn LICENSE-MIT LICENSE-APACHE README.md INSTALL.txt
rm -rf "$stage"

# チェックサム (dist/ に置き、install.sh で隣の SHA256SUMS として使う)。
(cd "$ROOT/dist" && sha256sum "$name.tar.gz" > SHA256SUMS)

echo "release: $ROOT/dist/$name.tar.gz"
echo "release: $ROOT/dist/SHA256SUMS"
ls -l "$ROOT/dist/$name.tar.gz" | cut -c1-120

# --- スモークテスト: 展開して、clang の無い PATH で動かす ---
smoke="$(mktemp -d)"
tar -xzf "$ROOT/dist/$name.tar.gz" -C "$smoke"
KSN_SMOKE="$smoke/ksn"

# clang のあるディレクトリを PATH から外す (実行時に clang を要らないことを確かめる)。
clang_dir="$(dirname "$(command -v clang)")"
PATH_smoke="$(echo "$PATH" | tr ':' '\n' | grep -vxF "$clang_dir" | paste -sd: -)"
export PATH="$PATH_smoke"

ver="$("$KSN_SMOKE" --version)"
[ "$ver" = "ksn 0.1.0" ] || { echo "release: smoke: --version が想定外: $ver" >&2; exit 1; }

(cd "$smoke" && "$KSN_SMOKE" new hello)
cd "$smoke/hello"
out_run="$("$KSN_SMOKE" run)"
"$KSN_SMOKE" build --release
out_native="$(./target/release/hello.native)"
[ "$out_run" = "Hello, Kasane!" ] || { echo "release: smoke: ksn run の出力が想定外: $out_run" >&2; exit 1; }
[ "$out_native" = "Hello, Kasane!" ] || { echo "release: smoke: hello.native の出力が想定外: $out_native" >&2; exit 1; }

echo "release: スモークテスト OK (ksn --version / ksn run / ksn build --release / ./target/release/hello.native)"
