# release/compat: 配布 tarball を glibc 2.31 以上で動かす

- 原因: LLVM の静的ライブラリ (glibc 2.41 のヘッダでビルド) が `__isoc23_strto*` (2.38)、libm の `fmod@2.38`、Rust std の `pidfd_spawnp` (2.39、弱い)。
- 直し: `cargo zigbuild --target x86_64-unknown-linux-gnu.2.31 --features glibc-compat`。zig は libstdc++ を持たないので `libstdc++.a` を RUSTFLAGS で明示。
  `glibc_compat.rs` が `__isoc23_strto*`・`mallinfo2`・`__libc_single_threaded`・`arc4random` を補う (feature のときだけ)。
- `native/lld.rs`: Arch の gcc の場所 (`/usr/lib/gcc/x86_64-pc-linux-gnu`) も探す。
- 利用者のプログラム側: 実行時の C は埋め込みのビットコードから作り、新しい glibc の名前は出ない (examples の全成果物で最大 2.34 = Scrt1.o / pthread の統合)。
- `scripts/make_release.sh` と `scripts/gen_third_party.py` で tarball (LICENSE・THIRD_PARTY_LICENSES・SHA256SUMS つき) を作る。
