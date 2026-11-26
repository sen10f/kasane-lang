# build-hash-reuse (P-01 + P-10)

## P-01: `ksn build` のたびに 136 MB の ksn 本体を SHA-256 で数え直している (opus-perf P-01)

原因: `cmd_build` が共有の `ksn-<BUILD_ID>` に毎回 `set_modified` をかけ、ハードリンクの追加も
inode の ctime を変えるため、`sha_file_memo` のメモが毎回外れて `self_hash()` / `is_self()` が
136 MB を数え直す。`build_key` も実行体を丸ごと読んでハッシュしていた。

直し (`stage0/src/cli.rs`):

1. `is_self(p)`: unix ではまず `p` と `current_exe()` の記録済みの同一性
   (dev・inode・サイズ・mtime) を比べ、一致なら同じファイルなのでハッシュせず `true`。
   ctime はハードリンクの作成で変わるので比べない (実体を書き換えると mtime が変わり、
   それを戻せるのはファイルの持ち主だけ = 0700 のキャッシュを書ける人と同じ)。
   不一致なら従来どおりハッシュで比べる (置き換えは新しい inode になるので必ず検出)。
2. 使用の印 (`prune_shared` の LRU 用) は実体の mtime ではなく別ファイル
   `ksn-<識別子>.used` に書く。実体の inode には触らない。
   `prune_shared` は `.used` の mtime を使い (無ければ実体の mtime)、`.used`/`.tmp` は実体として数えない。
3. `sha_file_memo` のメモの鍵から ctime を外す (ハードリンク追加で毎回 ctime が変わっていた)。
   dev・inode・サイズ・mtime が同じなら同じ中身 (書き換えは mtime・置き換えは inode が変わる)。
4. `build_key` は実行体を丸ごと読む代わりにメモ付きの `self_hash()` を使う。

検査の強さは変えない: 置き換え・改竄された共有 ksn は従来どおりハッシュで検出される
(`cache_integrity::a_tampered_shared_ksn_is_not_used` が通ること)。

## P-10: `ksn test --both` のバンドル退避がテストごとに `ksn build` をやり直す (opus-perf P-10 原因 1)

`run_in_bundle` は `build_key(l, false, "host", "c")` で鍵を調べていたが、
`cmd_build(&[])` は既定の backend `"llvm"` の鍵を `BUILD_STAMP` に書くので不一致で毎回作り直し。
鍵を `"llvm"` にそろえる (1 行)。テストを追加: バンドルへ退避するテストが 2 本あっても
ビルドは 1 回だけ (`cache hit` が 1 回) になること。

## 計測

- nbody (`xlang-bench/src/nbody/ksn` の写し) の `ksn build --release` (キャッシュヒット時) の時間を前後で。
- apps/wiki (写し) の `ksn test --both <prefix>` (バンドル退避を含む小さな部分集合) を前後で。
