#!/usr/bin/env bash
# Kasane 開発環境のセットアップ (ローカル・クラウド共通)。
#
#   bash scripts/setup.sh          # 依存の確認と ksn のビルド
#   bash scripts/setup.sh --test   # さらにフル検証 (python tests/run_tests.py)
#   bash scripts/setup.sh --bench  # さらに LLM ベンチの準備確認 (API キーの有無だけを見る。値は表示しない)
#
# 環境変数はリポジトリ直下の .env (コミットしない。テンプレートは .env.example) から読む。
# 必要なもの: Rust (cargo, 2021 edition), C コンパイラ (gcc か clang。ネイティブ生成のリンクと
# 実行時の C ランタイムのコンパイルに使う), Python 3.10 以上, git。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

want_test=0
want_bench=0
for a in "$@"; do
    case "$a" in
        --test) want_test=1 ;;
        --bench) want_bench=1 ;;
        -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
        *) echo "setup: unknown option: $a" >&2; exit 2 ;;
    esac
done

ok() { printf 'ok   %s\n' "$1"; }
ng() { printf 'NG   %s\n' "$1" >&2; missing=1; }
missing=0

# .env があれば読み込む (値は表示しない)。
if [ -f .env ]; then
    set -a
    # shellcheck disable=SC1091
    . ./.env
    set +a
    ok ".env を読み込んだ"
fi

# 1. 依存の確認
if command -v cargo >/dev/null 2>&1; then ok "cargo: $(cargo --version)"; else ng "cargo がない (https://rustup.rs で Rust を入れる)"; fi
if command -v gcc >/dev/null 2>&1; then
    ok "C コンパイラ: $(gcc --version | head -1)"
elif command -v clang >/dev/null 2>&1; then
    ok "C コンパイラ: $(clang --version | head -1)"
elif command -v cc >/dev/null 2>&1; then
    ok "C コンパイラ: cc"
else
    ng "gcc / clang / cc がない (ネイティブ生成のテストに必要)"
fi
if command -v python3 >/dev/null 2>&1 && python3 -c 'import sys; sys.exit(0 if sys.version_info >= (3, 10) else 1)'; then
    ok "python3: $(python3 --version)"
else
    ng "python3 (3.10 以上) がない"
fi
if command -v git >/dev/null 2>&1; then ok "git: $(git --version)"; else ng "git がない (git 依存のテストに必要)"; fi
if command -v go >/dev/null 2>&1; then ok "go (任意。bench/http の比較用): $(go version)"; else echo "--   go はない (任意。bench/http の Go 版を飛ばす)"; fi
[ "$missing" -eq 0 ] || { echo "setup: 足りないものがある" >&2; exit 1; }

# 2. ksn のビルド
(cd stage0 && cargo build --release)
KSN="$ROOT/stage0/target/release/ksn"
[ -x "$KSN" ] || { echo "setup: ksn がビルドされていない" >&2; exit 1; }
ok "ksn: $KSN"

# 3. 動作確認 (小さな例 1 本)
(cd examples/demo && "$KSN" check && "$KSN" test --both >/dev/null)
ok "examples/demo の check と test --both"

# 4. フル検証
if [ "$want_test" -eq 1 ]; then
    # 手元の開発機は CPU 10/11 が不安定 (実装計画.md 7 章)。KSN_AVOID_CPUS が設定されていれば、そのコアを避けて回す。
    if [ -n "${KSN_AVOID_CPUS:-}" ] && command -v taskset >/dev/null 2>&1; then
        python3 - "$KSN_AVOID_CPUS" <<'EOF' > /tmp/ksn_cpus.$$
import os, sys
avoid = {int(x) for x in sys.argv[1].split(',') if x}
print(','.join(str(c) for c in sorted(os.sched_getaffinity(0)) if c not in avoid))
EOF
        cpus="$(cat /tmp/ksn_cpus.$$)"; rm -f /tmp/ksn_cpus.$$
        echo "--   CPU $KSN_AVOID_CPUS を避けて実行 (taskset -c $cpus)"
        taskset -c "$cpus" python3 tests/run_tests.py
    else
        python3 tests/run_tests.py
    fi
fi

# 5. ベンチの準備確認 (キーの値は表示しない)
if [ "$want_bench" -eq 1 ]; then
    if [ -n "${NVIDIA_API_KEY:-}" ]; then
        ok "NVIDIA_API_KEY が設定されている (${#NVIDIA_API_KEY} 文字)"
    else
        ng "NVIDIA_API_KEY が未設定 (クラウドでは環境のシークレット、ローカルでは .env に書く)"
        exit 1
    fi
    python3 -I bench/test_harness.py
    echo "--   実行例: python bench/run_bench.py --provider nim --docs full"
fi

echo "setup: 完了"
