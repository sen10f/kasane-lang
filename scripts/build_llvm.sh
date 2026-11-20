#!/usr/bin/env bash
# ksn 用の LLVM + lld を静的ライブラリとして自前でビルドする (L3b)。
# `ksn` を `--features embedded-llvm` でビルドするとき、`KSN_LLVM_PREFIX` を指すと
# distro の LLVM (動的に zlib / zstd / libstdc++ を引く) の代わりに、これを静的にリンクする。
#
# 使い方:
#   scripts/build_llvm.sh                       # ホスト (x86_64-linux-gnu) 用, $HOME/llvm-ksn/install
#   scripts/build_llvm.sh --target aarch64-unknown-linux-gnu   # クロスビルド, $HOME/llvm-ksn/<トリプル>/
#   scripts/build_llvm.sh --target x86_64-pc-windows-gnu
#
# 既存の x86_64 ホストキャッシュ ($HOME/llvm-ksn/install) は触らない。
# クロスビルドの成果物は $HOME/llvm-ksn/<トリプル>/ にキャッシュされ、
# 中間ビルドディレクトリはビルド後に削除する。
set -euo pipefail

TARGET=""
while [ $# -gt 0 ]; do
  case "$1" in
    --target) TARGET="$2"; shift 2 ;;
    --target=*) TARGET="${1#--target=}"; shift ;;
    *) echo "usage: $0 [--target <トリプル>]" >&2; exit 2 ;;
  esac
done

LLVM_VER="${LLVM_VER:-20.1.2}"
ROOT="${ROOT:-$HOME/llvm-ksn}"
JOBS="${JOBS:-12}"
SRC_TAR="llvm-project-$LLVM_VER.src"
SRC="$ROOT/$SRC_TAR"
SRC_XZ="$ROOT/$SRC_TAR.tar.xz"

# cmake / ninja が PATH に無ければ $ROOT/tools の物を使う。
for d in "$ROOT"/tools "$ROOT"/tools/cmake-*/bin; do [ -d "$d" ] && PATH="$d:$PATH"; done
export PATH

command -v cmake >/dev/null 2>&1 || { echo "build_llvm: cmake が見つからない ($ROOT/tools に cmake を入れてください)" >&2; exit 1; }
command -v ninja >/dev/null 2>&1 || { echo "build_llvm: ninja が見つからない ($ROOT/tools に ninja を入れてください)" >&2; exit 1; }

if [ ! -d "$SRC" ]; then
  [ -f "$SRC_XZ" ] || curl -fL -o "$SRC_XZ" \
    "https://github.com/llvm/llvm-project/releases/download/llvmorg-$LLVM_VER/llvm-project-$LLVM_VER.src.tar.xz"
  tar -C "$ROOT" -xf "$SRC_XZ"
fi

mkdir -p "$ROOT"

if [ -z "$TARGET" ]; then
  # ---- ホストビルド (既存キャッシュ $HOME/llvm-ksn/install をそのまま使う) ----
  PREFIX="$ROOT/install"
  BUILD="$ROOT/build"
  echo "build_llvm: ホストビルド (PREFIX=$PREFIX)"
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
  nice -n 10 ninja -C "$BUILD" -j "$JOBS" install
  rm -rf "$BUILD"
  echo "done: KSN_LLVM_PREFIX=$PREFIX"
  exit 0
fi

# ---- クロスビルド ----
case "$TARGET" in
  aarch64-linux-gnu|aarch64-unknown-linux-gnu)
    CC="${CROSS_CC:-aarch64-linux-gnu-gcc}"
    CXX="${CROSS_CXX:-aarch64-linux-gnu-g++}"
    LLVM_TARGETS="AArch64"
    CMAKE_SYSTEM_NAME="Linux"
    CMAKE_SYSTEM_PROCESSOR="aarch64"
    ;;
  x86_64-windows-gnu|x86_64-pc-windows-gnu)
    CC="${CROSS_CC:-x86_64-w64-mingw32-gcc}"
    CXX="${CROSS_CXX:-x86_64-w64-mingw32-g++}"
    LLVM_TARGETS="X86"
    CMAKE_SYSTEM_NAME="Windows"
    CMAKE_SYSTEM_PROCESSOR="x86_64"
    ;;
  *)
    echo "build_llvm: サポートしていないターゲット: $TARGET (aarch64-linux-gnu / x86_64-windows-gnu)" >&2
    exit 1
    ;;
esac

PREFIX="$ROOT/$TARGET"
BUILD="$ROOT/build-$TARGET"
TOOLCHAIN="$ROOT/toolchain-$TARGET.cmake"

# CROSS_CC / CROSS_CXX が見つからなければ、ここで止めて要件を報告する (fail-closed)。
command -v "$CC" >/dev/null 2>&1 || { echo "build_llvm: クロス C コンパイラが見つからない: $CC (CROSS_CC で上書き可)" >&2; exit 1; }
command -v "$CXX" >/dev/null 2>&1 || { echo "build_llvm: クロス C++ コンパイラが見つからない: $CXX (CROSS_CXX で上書き可。aarch64 は g++-aarch64-linux-gnu が必要)" >&2; exit 1; }

cat > "$TOOLCHAIN" <<EOF
set(CMAKE_SYSTEM_NAME $CMAKE_SYSTEM_NAME)
set(CMAKE_SYSTEM_PROCESSOR $CMAKE_SYSTEM_PROCESSOR)
set(CMAKE_C_COMPILER $CC)
set(CMAKE_CXX_COMPILER $CXX)
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
EOF

echo "build_llvm: クロスビルド ターゲット=$TARGET (PREFIX=$PREFIX, CC=$CC, CXX=$CXX, LLVM_TARGETS=$LLVM_TARGETS)"

CMAKE_ARGS=(
  -G Ninja -S "$SRC/llvm" -B "$BUILD"
  -DCMAKE_TOOLCHAIN_FILE="$TOOLCHAIN"
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$PREFIX"
  -DLLVM_HOST_TRIPLE=x86_64-linux-gnu
  -DLLVM_ENABLE_PROJECTS="lld"
  -DLLVM_TARGETS_TO_BUILD="$LLVM_TARGETS"
  -DLLVM_BUILD_LLVM_DYLIB=OFF -DLLVM_LINK_LLVM_DYLIB=OFF -DBUILD_SHARED_LIBS=OFF
  -DLLVM_ENABLE_ASSERTIONS=OFF -DLLVM_ENABLE_ZLIB=OFF -DLLVM_ENABLE_ZSTD=OFF
  -DLLVM_ENABLE_TERMINFO=OFF -DLLVM_ENABLE_LIBXML2=OFF -DLLVM_ENABLE_LIBEDIT=OFF
  -DLLVM_INCLUDE_TESTS=OFF -DLLVM_INCLUDE_BENCHMARKS=OFF -DLLVM_INCLUDE_EXAMPLES=OFF -DLLVM_INCLUDE_DOCS=OFF
  -DLLVM_ENABLE_PIC=ON -DLLVM_ENABLE_PLUGINS=OFF -DLLVM_INSTALL_UTILS=OFF
  -DLLVM_BUILD_TOOLS=OFF -DLLVM_BUILD_LLVM_DYLIB=OFF
)

# クロスビルド LLVM はホスト上でビルドした tblgen などを LLVM_TABLEGEN で渡す必要がある。
# ホスト用の既存キャッシュ ($ROOT/install) が無い場合、cmake は tblgen を見つけられずに失敗する。
# その場合は先にホストビルド (scripts/build_llvm.sh 引数なし) を実行して $ROOT/install を作ること。
if [ -x "$ROOT/install/bin/llvm-tblgen" ]; then
  CMAKE_ARGS+=(-DLLVM_TABLEGEN="$ROOT/install/bin/llvm-tblgen")
elif command -v llvm-tblgen >/dev/null 2>&1; then
  CMAKE_ARGS+=(-DLLVM_TABLEGEN="$(command -v llvm-tblgen)")
fi

cmake "${CMAKE_ARGS[@]}"

nice -n 10 ninja -C "$BUILD" -j "$JOBS" install-llvm-libraries install-llvm-headers install-lld install-lld-cmake-exports \
  2>&1 | tail -n 5 || true
# ldd 静的ライブラリ/ヘッダも必要 (install-lld はツールのみ)。ビルドディレクトリからコピー。
find "$BUILD" -name "liblld*.a" -exec cp {} "$PREFIX/lib/" \; 2>/dev/null || true
cp -r "$SRC/lld/include/lld" "$PREFIX/include/" 2>/dev/null || true
rm -rf "$BUILD" "$TOOLCHAIN"
echo "done: KSN_LLVM_PREFIX=$PREFIX"
