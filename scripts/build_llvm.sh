#!/usr/bin/env bash
# ksn 用の LLVM + lld を静的ライブラリとして自前でビルドする (L3b)。
# `ksn` を `--features embedded-llvm` でビルドするとき、`KSN_LLVM_PREFIX=$PREFIX` を指すと
# distro の LLVM (動的に zlib / zstd / libstdc++ を引く) の代わりに、これを静的にリンクする。
# 使い方: scripts/build_llvm.sh   (環境変数: LLVM_VER, ROOT, JOBS)
set -euo pipefail
LLVM_VER="${LLVM_VER:-20.1.2}"
ROOT="${ROOT:-$HOME/llvm-ksn}"
JOBS="${JOBS:-12}"
PREFIX="$ROOT/install"
SRC="$ROOT/llvm-project-$LLVM_VER.src"
BUILD="$ROOT/build"
mkdir -p "$ROOT"
# cmake / ninja が PATH に無ければ $ROOT/tools の物を使う。
for d in "$ROOT"/tools "$ROOT"/tools/cmake-*/bin; do [ -d "$d" ] && PATH="$d:$PATH"; done
export PATH
if [ ! -d "$SRC" ]; then
  [ -f "$ROOT/llvm-project-$LLVM_VER.src.tar.xz" ] || curl -fL -o "$ROOT/llvm-project-$LLVM_VER.src.tar.xz" \
    "https://github.com/llvm/llvm-project/releases/download/llvmorg-$LLVM_VER/llvm-project-$LLVM_VER.src.tar.xz"
  tar -C "$ROOT" -xf "$ROOT/llvm-project-$LLVM_VER.src.tar.xz"
fi
cmake -G Ninja -S "$SRC/llvm" -B "$BUILD" \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$PREFIX" \
  -DLLVM_ENABLE_PROJECTS="lld" \
  -DLLVM_TARGETS_TO_BUILD="X86;AArch64;WebAssembly" \
  -DLLVM_BUILD_LLVM_DYLIB=OFF -DLLVM_LINK_LLVM_DYLIB=OFF -DBUILD_SHARED_LIBS=OFF \
  -DLLVM_ENABLE_ASSERTIONS=OFF -DLLVM_ENABLE_ZLIB=OFF -DLLVM_ENABLE_ZSTD=OFF \
  -DLLVM_ENABLE_TERMINFO=OFF -DLLVM_ENABLE_LIBXML2=OFF -DLLVM_ENABLE_LIBEDIT=OFF \
  -DLLVM_INCLUDE_TESTS=OFF -DLLVM_INCLUDE_BENCHMARKS=OFF -DLLVM_INCLUDE_EXAMPLES=OFF -DLLVM_INCLUDE_DOCS=OFF \
  -DLLVM_ENABLE_PIC=ON -DLLVM_ENABLE_PLUGINS=OFF -DLLVM_INSTALL_UTILS=OFF
nice -n 10 ninja -C "$BUILD" -j "$JOBS" install-llvm-libraries install-llvm-headers install-cmake-exports \
  install-lld-libraries install-lld-headers install-llvm-config 2>&1 | tail -n 5 || true
# 上の対象が無い版でも、全部入れれば良い (ツール込みで少し遅い)。
nice -n 10 ninja -C "$BUILD" -j "$JOBS" install
echo "done: KSN_LLVM_PREFIX=$PREFIX"
