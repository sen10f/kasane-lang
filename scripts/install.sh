#!/usr/bin/env bash
# scripts/install.sh URL
#
# リリースアーカイブをダウンロードし、同じ場所にある SHA256SUMS で SHA-256 を検証して
# ~/.local/bin/ksn にインストールする (sudo 不要)。
#
#   bash scripts/install.sh https://example.com/ksn-0.1.0-alpha.1-x86_64-linux-gnu.tar.gz
set -euo pipefail

[ "$#" -eq 1 ] || { echo "usage: $0 URL" >&2; exit 2; }
url="$1"
case "$url" in
  */) url="${url%/}" ;;
esac
archive="${url##*/}"
[ -n "$archive" ] || { echo "install: アーカイブ名が URL から取れない: $url" >&2; exit 2; }

tmp="$(mktemp -d)"
cleanup() { rm -rf "$tmp"; }
trap cleanup EXIT

echo "install: ダウンロード中: $archive"
curl -fL -o "$tmp/$archive" "$url"

echo "install: 隣の SHA256SUMS を取得中"
curl -fL -o "$tmp/SHA256SUMS" "$(dirname "$url")/SHA256SUMS"

grep -F "$archive" "$tmp/SHA256SUMS" > "$tmp/check.sums"
echo "install: SHA-256 を検証中"
(cd "$tmp" && sha256sum -c check.sums)

echo "install: 展開中"
tar -xzf "$tmp/$archive" -C "$tmp"
ksn_bin="$(find "$tmp" -maxdepth 3 -type f -name ksn -perm -111 | head -1)"
[ -n "$ksn_bin" ] || { echo "install: アーカイブに ksn 実行体が含まれていない" >&2; exit 1; }

echo "install: ~/.local/bin にインストール中"
install -d "$HOME/.local/bin"
install -m 0755 "$ksn_bin" "$HOME/.local/bin/ksn"

echo "install: $HOME/.local/bin/ksn にインストールした ($HOME/.local/bin が PATH に含まれていることを確認してください)"
