#!/bin/bash
# linear_ok の各ファイルを ksn build し、ネイティブか退避かと理由を集計する。
# ネイティブになったものは、出力・終了コードが `ksn run` と一致するかも確かめる。
# 使い方: tools/linear_native.sh [ksn のパス] [名前の部分文字列...]
KSN=$(readlink -f "${1:-$(dirname "$0")/../stage0/target/release/ksn}")
shift
SRC=$(readlink -f "$(dirname "$0")/../stage0/tests/data/linear_ok")
W=$(mktemp -d)
n=0; bad=0; mism=0
for f in "$SRC"/*.ksnl; do
  s=$(basename "$f" .ksnl)
  if [ $# -gt 0 ]; then ok=0; for a in "$@"; do [[ $s == *$a* ]] && ok=1; done; [ $ok = 1 ] || continue; fi
  d=$W/$s; mkdir -p "$d/src"
  printf '[package]\nname = "t"\nversion = "0.1.0"\nedition = 1\nentry = "t.main"\n\n[dependencies]\n' > "$d/kasane.toml"
  echo 'version = 1' > "$d/kasane.lock"
  cp "$f" "$d/src/t.ksnl"
  out=$(cd "$d" && "$KSN" build 2>&1)
  n=$((n+1))
  if echo "$out" | grep -q "native backend skipped"; then
    bad=$((bad+1)); echo "BUNDLE $s: $(echo "$out" | grep "native backend skipped" | head -1 | sed 's/.*skipped: //')"
  else
    exe=$(ls "$d"/target/debug/t.native 2>/dev/null)
    if [ -z "$exe" ]; then bad=$((bad+1)); echo "NONATIVE $s: $out"; continue; fi
    a=$("$exe" 2>&1; echo "rc=$?"); b=$(cd "$d" && "$KSN" run 2>&1; echo "rc=$?")
    if [ "$a" != "$b" ]; then mism=$((mism+1)); echo "MISMATCH $s"; echo "  native: $a" | head -3; echo "  run:    $b" | head -3; fi
  fi
done
echo "native $((n-bad)) / $n, mismatch $mism"
rm -rf "$W"
