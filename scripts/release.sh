#!/usr/bin/env bash
# scripts/release.sh [--target <トリプル>] VERSION
#
# リリース用のビルド済み ksn を dist/ に作成する。
#
#   bash scripts/release.sh 0.1.0-beta.1
#   bash scripts/release.sh --target aarch64-unknown-linux-gnu 0.1.0-beta.1
#   bash scripts/release.sh --target x86_64-pc-windows-gnu 0.1.0-beta.1
#
# 作られるもの:
#   dist/ksn-VERSION-<トリプル>.tar.gz   (Linux / macOS)
#   dist/ksn-VERSION-<トリプル>.zip       (Windows)
#     ksn(.exe) (実行体)、LICENSE-MIT、LICENSE-APACHE、README.md、INSTALL.txt
#   dist/SHA256SUMS
#
# ksn は KSN_LLVM_PREFIX=$HOME/llvm-ksn/<トリプル> (scripts/build_llvm.sh --target の自前 LLVM)
# で CARGO_TARGET_DIR=dist/build の別ターゲットディレクトリにリリースビルドし、
# strip してから ldd / objdump で実行時の共有ライブラリが libc / libm / libgcc_s / ld-linux のみか検査する。
# スクリプト内でスモークテストも行う (展開先で clang の無い PATH で ksn --version / new / run / build --release)。
set -euo pipefail

TARGET=""
while [ $# -gt 0 ]; do
  case "$1" in
    --target) TARGET="$2"; shift 2 ;;
    --target=*) TARGET="${1#--target=}"; shift ;;
    -*) echo "usage: $0 [--target <トリプル>] VERSION" >&2; exit 2 ;;
    *) break ;;
  esac
done

[ "$#" -eq 1 ] || { echo "usage: $0 [--target <トリプル>] VERSION" >&2; exit 2; }
VERSION="$1"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [ -n "$TARGET" ]; then
  PREFIX="$HOME/llvm-ksn/$TARGET"
  case "$TARGET" in
    *-windows-gnu) EXE=".exe"; ARCHIVE="zip" ;;
    *) EXE=""; ARCHIVE="tar.gz" ;;
  esac
else
  PREFIX="$HOME/llvm-ksn/install"
  EXE=""; ARCHIVE="tar.gz"
fi

if [ ! -d "$PREFIX" ]; then
  echo "release: LLVM のプレフィックスが見つからない: $PREFIX" >&2
  echo "   まず scripts/build_llvm.sh [--target $TARGET] を実行して KSN_LLVM_PREFIX=$PREFIX を作ってください" >&2
  exit 1
fi
export KSN_LLVM_PREFIX="$PREFIX"

rm -rf "$ROOT/dist"
mkdir -p "$ROOT/dist"

TARGET_FLAG=""
BUILD_CONFIG=""
if [ -n "$TARGET" ]; then
  TARGET_FLAG="--target=$TARGET"
  # rustc の既定のクロスリンカはホストの cc + 同梱の gcc-ld + host の ld.lld を使い、
  # ターゲット固有の修正フラグ (--fix-cortex-a53-843419 等) がホストの ld.lld で拒否される。
  # クロス用 gcc をリンカに明示する (build スクリプトはホスト向けなので target 限定の --config を使う)。
  case "$TARGET" in
    aarch64-unknown-linux-gnu) BUILD_CONFIG='--config target.aarch64-unknown-linux-gnu.linker="aarch64-linux-gnu-gcc"' ;;
    x86_64-pc-windows-gnu)     BUILD_CONFIG='--config target.x86_64-pc-windows-gnu.linker="x86_64-w64-mingw32-gcc"' ;;
  esac
  echo "release: クロスリリースビルド ターゲット=$TARGET (KSN_LLVM_PREFIX=$KSN_LLVM_PREFIX, CARGO_TARGET_DIR=$ROOT/dist/build)"
else
  echo "release: ksn をリリースビルドする (KSN_LLVM_PREFIX=$KSN_LLVM_PREFIX, CARGO_TARGET_DIR=$ROOT/dist/build)"
fi
(cd stage0 && CARGO_TARGET_DIR="$ROOT/dist/build" cargo build --release $BUILD_CONFIG $TARGET_FLAG)

if [ -n "$TARGET" ]; then
  case "$TARGET" in
    *-windows-gnu) KSN="$ROOT/dist/build/$TARGET/release/ksn.exe" ;;
    *) KSN="$ROOT/dist/build/$TARGET/release/ksn" ;;
  esac
else
  KSN="$ROOT/dist/build/release/ksn"
fi
[ -x "$KSN" ] || { echo "release: ksn がビルドされていない: $KSN" >&2; exit 1; }

if [ "$EXE" = ".exe" ]; then
  x86_64-w64-mingw32-strip --strip-all "$KSN" || strip --strip-all "$KSN"
else
  strip --strip-all "$KSN"
fi

# 実行時の共有ライブラリは libc / libm / libgcc_s / ld-linux のみ (LLVM は静的リンク)。Linux のみ検査。
if [ "$EXE" != ".exe" ]; then
  bad="$(ldd "$KSN" | grep -vE 'linux-vdso\.so\.1|libm\.so|libc\.so|libgcc_s|ld-linux' || true)"
  if [ -n "$bad" ]; then
    echo "release: ksn に許可されていない共有ライブラリがあります:" >&2
    echo "$bad" >&2
    echo "   許可: libc / libm /libgcc_s / ld-linux のみ" >&2
    exit 1
  fi
fi

base="ksn-$VERSION-$TARGET"
[ -z "$TARGET" ] && base="ksn-$VERSION-x86_64-linux-gnu"
stage="$(mktemp -d)"
if [ "$EXE" = ".exe" ]; then
  cp "$KSN" "$stage/ksn.exe"
else
  cp "$KSN" "$stage/ksn"
fi
cp "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$ROOT/README.md" "$ROOT/INSTALL.txt" "$stage/"

if [ "$ARCHIVE" = "zip" ]; then
  if ! command -v zip >/dev/null 2>&1; then
    echo "release: zip コマンドが見つからない (.zip アーカイブを作れません)" >&2; exit 1
  fi
  (cd "$stage" && zip -q -r "$ROOT/dist/$base.zip" ksn.exe LICENSE-MIT LICENSE-APACHE README.md INSTALL.txt)
  archive="$ROOT/dist/$base.zip"
else
  tar -czf "$ROOT/dist/$base.tar.gz" -C "$stage" ksn LICENSE-MIT LICENSE-APACHE README.md INSTALL.txt
  archive="$ROOT/dist/$base.tar.gz"
fi
rm -rf "$stage"

# チェックサム (dist/ に置き、install.sh で隣の SHA256SUMS として使う)。
(cd "$ROOT/dist" && sha256sum "$(basename "$archive")" > SHA256SUMS)

echo "release: $archive"
echo "release: $ROOT/dist/SHA256SUMS"
ls -l "$archive" | cut -c1-120

# --- スモークテスト: 展開して、clang の無い PATH で動かす ---
smoke="$(mktemp -d)"
if [ "$ARCHIVE" = "zip" ]; then
  unzip -q "$archive" -d "$smoke"
else
  tar -xzf "$archive" -C "$smoke"
fi
if [ "$EXE" = ".exe" ]; then
  KSN_SMOKE="$smoke/ksn.exe"
else
  KSN_SMOKE="$smoke/ksn"
fi

# clang のあるディレクトリを PATH から外す (実行時に clang を要らないことを確かめる)。
clang_dir="$(dirname "$(command -v clang 2>/dev/null || true)")"
PATH_smoke="$(echo "$PATH" | tr ':' '\n' | grep -vxF "$clang_dir" | paste -sd: -)"
export PATH="$PATH_smoke"

# クロスビルドの Linux 実行体はホストで直接 exec できない (binfmt_misc に qemu-aarch64 の登録がない)。
# Windows は wine、クロス Linux は qemu-aarch64 -L でラップする。
QEMU=""
case "$TARGET" in
  *-linux-gnu)
    case "$TARGET" in
      x86_64-unknown-linux-gnu|x86_64-linux-gnu|"$HOSTTYPE-unknown-linux-gnu"|"$(uname -m)-unknown-linux-gnu") ;; # ホストネイティブ
      *)
        if command -v qemu-aarch64 >/dev/null 2>&1 && [ -d /usr/aarch64-linux-gnu ]; then
          QEMU="qemu-aarch64 -L /usr/aarch64-linux-gnu"
        fi
        ;;
    esac
    ;;
esac

run_ksn() { # run_ksn <args...>  (実行体をラップして実行、出力を stdout に)
  if [ -n "$QEMU" ]; then
    $QEMU "$KSN_SMOKE" "$@"
  else
    "$KSN_SMOKE" "$@"
  fi
}

case "$TARGET" in
  *-windows-gnu)
    # wine で実行 (smoke はこの x86_64 Linux マシン上で)。
    if ! command -v wine >/dev/null 2>&1; then
      echo "release: wine が見つからない (Windows のスモークテストをスキップ)" >&2
      exit 0
    fi
    ver="$(wine "$KSN_SMOKE" --version)"
    [ "$ver" = "ksn 0.1.0" ] || { echo "release: smoke: --version が想定外: $ver" >&2; exit 1; }
    (cd "$smoke" && wine "$KSN_SMOKE" new hello)
    cd "$smoke/hello"
    out_run="$(wine "$KSN_SMOKE" run)"
    wine "$KSN_SMOKE" build --release
    echo "release: Windows スモークテスト OK (wine ksn.exe --version / ksn.exe run)"
    ;;
  *)
    ver="$(run_ksn --version)"
    [ "$ver" = "ksn 0.1.0" ] || { echo "release: smoke: --version が想定外: $ver" >&2; exit 1; }
    (cd "$smoke" && run_ksn new hello)
    cd "$smoke/hello"
    out_run="$(run_ksn run)"
    run_ksn build --release
    if [ -z "$QEMU" ]; then
      out_native="$(./target/release/hello.native)"
      [ "$out_run" = "Hello, Kasane!" ] || { echo "release: smoke: ksn run の出力が想定外: $out_run" >&2; exit 1; }
      [ "$out_native" = "Hello, Kasane!" ] || { echo "release: smoke: hello.native の出力が想定外: $out_native" >&2; exit 1; }
      echo "release: スモークテスト OK (ksn --version / ksn run / ksn build --release / ./target/release/hello.native)"
    else
      [ "$out_run" = "Hello, Kasane!" ] || { echo "release: smoke: ksn run の出力が想定外: $out_run" >&2; exit 1; }
      echo "release: スモークテスト OK (qemu-aarch64 ksn --version / ksn new / ksn run / ksn build --release)"
    fi
    ;;
esac
