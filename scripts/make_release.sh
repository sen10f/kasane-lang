#!/usr/bin/env bash
# make_release.sh VERSION [OUTDIR]
# 配布用 tarball ksn-<版>-x86_64-linux-gnu.tar.gz を作る。glibc 2.31 を対象に cargo-zigbuild でビルドするので、
# Debian 12 (2.36)・Debian 13・Arch の現行で動く。標準ライブラリは実行体に埋め込み済み (stdlib ディレクトリは同梱しない)。
# 前提 (利用者領域に入れる): zig 0.15.x (PATH か ZIG_DIR)、`cargo install cargo-zigbuild`、LLVM 20 の静的ライブラリ
# (scripts/build_llvm.sh の成果、KSN_LLVM_PREFIX、既定 ~/llvm-ksn/install)、PATH に clang (実行時の C のビットコード用)。
# 既にビルド済みの実行体を使うなら KSN=<パス> を渡す (版の確認だけして詰める)。
set -euo pipefail
[ $# -ge 1 ] || { echo "usage: $0 VERSION [OUTDIR]" >&2; exit 2; }
VER="$1"; OUT=$(readlink -f "${2:-./dist}")
REPO=$(cd "$(dirname "$0")/.." && pwd)
MAXGLIBC="${MAXGLIBC:-2.36}"
if [ -z "${KSN:-}" ]; then
  [ -n "${ZIG_DIR:-}" ] && PATH="$ZIG_DIR:$PATH"; [ -d "$HOME/zig-tools/z" ] && PATH="$HOME/zig-tools/z:$PATH"
  export PATH KSN_LLVM_PREFIX="${KSN_LLVM_PREFIX:-$HOME/llvm-ksn/install}"
  # zig は libstdc++ を持たない (-lstdc++ は libc++ になる) ので、LLVM がつながる libstdc++.a を明示する。
  SA=$(c++ -print-file-name=libstdc++.a); [ -f "$SA" ] || { echo "libstdc++.a が見つからない" >&2; exit 1; }
  export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=$SA"
  (cd "$REPO/stage0" && cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.31 --features glibc-compat)
  KSN="$REPO/stage0/target/x86_64-unknown-linux-gnu/release/ksn"
fi
KSN=$(readlink -f "$KSN")
for f in "$KSN" "$REPO/LICENSE-APACHE" "$REPO/LICENSE-MIT" "$REPO/README.md" "$REPO/BETA.md"; do [ -e "$f" ] || { echo "無い: $f" >&2; exit 1; }; done
bad=$(ldd "$KSN" | grep -vE 'linux-vdso|libm\.so|libc\.so|libpthread\.so|libdl\.so|librt\.so|libgcc_s|ld-linux' || true)
[ -z "$bad" ] || { echo "想定外の共有ライブラリ: $bad" >&2; exit 1; }
need=$(objdump -T "$KSN" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -Vu | tail -1)
[ "$(printf '%s\n%s\n' "$need" "$MAXGLIBC" | sort -V | tail -1)" = "$MAXGLIBC" ] || { echo "glibc $need が要る (上限 $MAXGLIBC)" >&2; exit 1; }
echo "必要な glibc: $need"
name="ksn-$VER-x86_64-linux-gnu"; stage=$(mktemp -d /tmp/ksn_release_stage.XXXXXX)
mkdir -p "$stage/$name" "$OUT"
install -m 0755 "$KSN" "$stage/$name/ksn"
cp "$REPO/LICENSE-APACHE" "$REPO/LICENSE-MIT" "$REPO/README.md" "$REPO/BETA.md" "$stage/$name/"
[ -f "$REPO/INSTALL.txt" ] && cp "$REPO/INSTALL.txt" "$stage/$name/"
python3 -I "$REPO/scripts/gen_third_party.py" "$stage/$name/THIRD_PARTY_LICENSES" ${LLVM_SRC:+"$LLVM_SRC"}
echo "glibc $need" > "$stage/$name/REQUIRES.txt"
tar --owner=0 --group=0 --numeric-owner --sort=name -czf "$OUT/$name.tar.gz" -C "$stage" "$name"
(cd "$OUT" && sha256sum "$name.tar.gz" > SHA256SUMS && cat SHA256SUMS)
rm -rf "$stage"; echo "作った: $OUT/$name.tar.gz"
