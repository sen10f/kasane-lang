#!/bin/sh
EXE="$(dirname "$0")/stage0/target/release/ksn"
[ -x "$EXE" ] || { echo "build first: cd stage0 && cargo build --release" >&2; exit 1; }
exec "$EXE" "$@"
