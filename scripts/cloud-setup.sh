#!/usr/bin/env bash
# Claude Code のクラウド環境の「セットアップスクリプト」に貼る (またはこのファイルを実行する) 用。
# セッションの開始時に 1 回走る想定。何度走っても同じ結果になる (冪等)。
#
# 必要な外向きの通信 (環境のネットワーク設定で許可する):
#   - static.rust-lang.org, sh.rustup.rs   (Rust が入っていないときの rustup)
#   - crates.io, static.crates.io, index.crates.io   (cargo の依存: cranelift, regex, toml, sha2, similar, serde_json)
#   - integrate.api.nvidia.com   (LLM ベンチを回すときだけ)
#   - パッケージのミラー (apt など。gcc / clang / python3 / git が無いときだけ)
set -euo pipefail

cd "${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"

log() { printf '[cloud-setup] %s\n' "$*"; }

# root でなければ sudo を使う (無ければそのまま試す)
SUDO=""
if [ "$(id -u)" -ne 0 ] && command -v sudo >/dev/null 2>&1; then SUDO="sudo -n"; fi

# 1. OS のパッケージ: C コンパイラ (ネイティブ生成のリンクと C ランタイム), Python 3, git, taskset (util-linux)
need=()
command -v cc >/dev/null 2>&1 || command -v gcc >/dev/null 2>&1 || command -v clang >/dev/null 2>&1 || need+=(gcc)
command -v python3 >/dev/null 2>&1 || need+=(python3)
command -v git >/dev/null 2>&1 || need+=(git)
command -v curl >/dev/null 2>&1 || need+=(curl)
if [ "${#need[@]}" -gt 0 ]; then
    log "パッケージを入れる: ${need[*]}"
    if command -v apt-get >/dev/null 2>&1; then
        $SUDO apt-get update -qq
        DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y -qq "${need[@]}" build-essential
    elif command -v dnf >/dev/null 2>&1; then
        $SUDO dnf install -y -q "${need[@]}" gcc
    elif command -v apk >/dev/null 2>&1; then
        $SUDO apk add --no-cache "${need[@]}" build-base
    else
        log "パッケージマネージャが分からない。${need[*]} を手で入れること"; exit 1
    fi
fi

# 2. Rust (cargo)。無ければ rustup で stable を入れる。
if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then export PATH="$HOME/.cargo/bin:$PATH"; fi
if ! command -v cargo >/dev/null 2>&1; then
    log "Rust を入れる (rustup, stable, minimal)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
    export PATH="$HOME/.cargo/bin:$PATH"
fi
log "$(cargo --version)"

# 3. Python は 3.10 以上が要る
python3 -c 'import sys; sys.exit(0 if sys.version_info >= (3, 10) else 1)' || { log "python3 が 3.10 未満: $(python3 --version)"; exit 1; }

# 4. 依存の取得と ksn のビルド (以後のセッションではキャッシュが効く)
(cd stage0 && cargo fetch --locked -q 2>/dev/null || cargo fetch -q)
(cd stage0 && cargo build --release -q)
log "ksn: $(pwd)/stage0/target/release/ksn"

# 5. 小さな確認 (重いフル検証は scripts/setup.sh --test で、必要なときに回す)
(cd examples/demo && ../../stage0/target/release/ksn check && ../../stage0/target/release/ksn test --both >/dev/null)
log "examples/demo: check と test --both が通った"

# 6. ベンチの鍵 (値は表示しない)
if [ -n "${NVIDIA_API_KEY:-}" ]; then log "NVIDIA_API_KEY: 設定済み"; else log "NVIDIA_API_KEY: 未設定 (ベンチを回すときだけ必要)"; fi

log "完了"
