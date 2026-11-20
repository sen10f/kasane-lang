#!/usr/bin/env bash
# ui/tools/run_golden.sh: Z10 ヘッドレスレンダリングの金色画像テスト。
#
# ui/kasane/golden の 5 シーン (rect, round_border, clip, text, alpha) を
#   (1) ksn run (インタプリタ / 木走査)
#   (2) ネイティブビルド (ui/kasane/golden/target/release/golden.native)
# で描き, ui/tools/golden.py で ui/golden/linux-llfmpipe/*.png と比較する.
#
# 使い方:
#   ui/tools/run_golden.sh              # 比較のみ (全一致で終了コード 0)
#   GOLDEN_UPDATE=1 ui/tools/run_golden.sh  # 金色画像を現在のネイティブ描画で更新
#
# 環境変数:
#   KSN             ksn バイナリのパス (既定: /home/kasane/kasane-verify/bin/current/ksn)
#   GOLDEN_TMP      一時ディレクトリ (既定: mktemp)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KSNS="${KSN:-/home/kasane/kasane-verify/bin/current/ksn}"
NATIVE="$ROOT/ui/native"
THIRD_PARTY="$NATIVE/third_party"
GOLDEN_DIR="$ROOT/ui/golden/linux-llvmpipe"
PKG="$ROOT/ui/kasane/golden"
TMP="${GOLDEN_TMP:-$(mktemp -d)}"
SCENES="rect round_border clip text alpha demo"
NATIVE_BIN="$PKG/target/release/golden.native"

trap 'rm -rf "$TMP"' EXIT

if [ ! -x "$KSNS" ]; then
    echo "SKIP: ksn バイナリがない ($KSNS)" >&2
    exit 0
fi

# 第三パーティ (wgpu-native 等) が未ビルドならスキップする (明確なメッセージ付き)。
if [ ! -f "$THIRD_PARTY/lib/libwgpu_native.so" ] && [ ! -f "$THIRD_PARTY/lib/libwgpu_native.a" ]; then
    echo "SKIP: $THIRD_PARTY がビルドされていない ($ROOT/ui/native/build.sh を実行してください)"
    exit 0
fi

export LD_LIBRARY_PATH="$NATIVE:$THIRD_PARTY/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

fail=0
n=0
for scene in $SCENES; do
    n=$((n + 1))
    expected="$GOLDEN_DIR/$scene.png"
    if [ ! -f "$expected" ]; then
        echo "ERROR: 金色画像がない: $expected" >&2
        fail=1
        continue
    fi

    # --- (1) ksn run (インタプリタ) ---
    actual="$TMP/run-$scene.png"
    log="$TMP/run-$scene.log"
    if [ "$scene" = "demo" ]; then
        # demo は --headless-frames / --output フラグを受け取る (golden.main と引数形式が異なる)。
        if ! (cd "$ROOT/examples/ui_demo" && "$KSNS" run demo.main --headless-frames 6 --output "$actual" >"$log" 2>&1); then
            echo "FAIL: ksn run の demo が終了エラー (log: $log)" >&2
            cat "$log" >&2
            fail=1
            continue
        fi
    else
        if ! (cd "$PKG" && "$KSNS" run golden.main "$scene" "$actual" >"$log" 2>&1); then
            echo "FAIL: ksn run の $scene が終了エラー (log: $log)" >&2
            cat "$log" >&2
            fail=1
            continue
        fi
    fi
    if ! python3 "$ROOT/ui/tools/golden.py" "$actual" "$expected" "$TMP/diff-run-$scene.png" >>"$log" 2>&1; then
        echo "FAIL: ksn run の $scene が金色画像と一致しない (diff: $TMP/diff-run-$scene.png)" >&2
        fail=1
    fi

    # --- (2) ネイティブビルド ---
    if [ ! -x "$NATIVE_BIN" ]; then
        echo "WARN: $NATIVE_BIN がない (ui/kasane/golden で ksn build --release してください); ネイティブ比較をスキップ" >&2
        continue
    fi
    actual="$TMP/native-$scene.png"
    if ! "$NATIVE_BIN" -- "$scene" "$actual" >"$log" 2>&1; then
        echo "FAIL: ネイティブの $scene が終了エラー (log: $log)" >&2
        cat "$log" >&2
        fail=1
        continue
    fi
    if ! python3 "$ROOT/ui/tools/golden.py" "$actual" "$expected" "$TMP/diff-native-$scene.png" >>"$log" 2>&1; then
        echo "FAIL: ネイティブの $scene が金色画像と一致しない (diff: $TMP/diff-native-$scene.png)" >&2
        fail=1
    fi

    # 金色画像の更新 (ネイティブ出力を基準にする)
    if [ "${GOLDEN_UPDATE:-0}" = "1" ]; then
        cp "$actual" "$expected"
        echo "UPDATE: $expected をネイティブ出力で更新"
    fi
done

if [ "$fail" = 0 ]; then
    echo "run_golden: 全 $n シーン 一致 (ui/golden/linux-llvmpipe)"
    exit 0
fi
echo "run_golden: 不一致あり" >&2
exit 1
