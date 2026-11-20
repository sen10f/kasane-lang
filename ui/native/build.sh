#!/usr/bin/env bash
# ui/native/build.sh
# wgpu-native をピンしたバージョンで取得し、libksnui を .a と .so でビルドする。
# 第三パーティは ui/native/third_party/ に展開し、git 管理しない (.gitignore 参照)。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
THIRD_PARTY="$ROOT/third_party"

# ピンしたバージョンと SHA-256 (gfx-rs/wgpu-native のリリースアセット)。
WGPU_VER="29.0.1.1"
WGPU_SHA256="95a4d90c071005a98d03eab348beaa6b07e16eb00d1dcdb9f8348f75eb97ec5a"
WGPU_URL="https://github.com/gfx-rs/wgpu-native/releases/download/v${WGPU_VER}/wgpu-linux-x86_64-release.zip"
ZIP="$THIRD_PARTY/wgpu-linux-x86_64-release.zip"

mkdir -p "$THIRD_PARTY"

# --- wgpu-native の取得と検証 -----------------------------------------------
if [ ! -f "$ZIP" ] || [ "$(sha256sum "$ZIP" | cut -d' ' -f1)" != "$WGPU_SHA256" ]; then
    echo "downloading wgpu-native v${WGPU_VER} ..." >&2
    curl -fsSL -o "$ZIP.part" "$WGPU_URL"
    mv "$ZIP.part" "$ZIP"
fi
actual="$(sha256sum "$ZIP" | cut -d' ' -f1)"
if [ "$actual" != "$WGPU_SHA256" ]; then
    echo "sha256 mismatch: expected $WGPU_SHA256, got $actual" >&2
    exit 1
fi

# --- 展開 (idempotent) ------------------------------------------------------
if [ ! -f "$THIRD_PARTY/include/webgpu/wgpu.h" ]; then
    echo "extracting wgpu-native ..." >&2
    rm -rf "$THIRD_PARTY/wgpu-native"
    unzip -o -q "$ZIP" -d "$THIRD_PARTY/wgpu-native"
    mv "$THIRD_PARTY/wgpu-native/include" "$THIRD_PARTY/include"
    mv "$THIRD_PARTY/wgpu-native/lib" "$THIRD_PARTY/lib"
    rm -rf "$THIRD_PARTY/wgpu-native"
fi

# --- libksnui のビルド -------------------------------------------------------
echo "building libksnui ..." >&2
CFLAGS="-std=c11 -O2 -fPIC -Wall -Wextra -I$THIRD_PARTY/include $(pkg-config --cflags sdl3 freetype2 harfbuzz)"
LIBS="$(pkg-config --libs sdl3 freetype2 harfbuzz) -L$THIRD_PARTY/lib -lwgpu_native -lvulkan -ldl -lm -lpthread"

for c in "$ROOT"/ksnui_*.c; do
    [ -f "$c" ] || { echo "no ksnui_*.c found in $ROOT" >&2; exit 1; }
    o="$THIRD_PARTY/$(basename "$c" .c).o"
    echo "  cc $c -> $o" >&2
    cc $CFLAGS -c "$c" -o "$o"
done

cc -shared -o "$ROOT/libksnui.so" "$THIRD_PARTY"/ksnui_*.o $LIBS
ar rcs "$ROOT/libksnui.a" "$THIRD_PARTY"/ksnui_*.o

echo "done: $ROOT/libksnui.a $ROOT/libksnui.so" >&2
