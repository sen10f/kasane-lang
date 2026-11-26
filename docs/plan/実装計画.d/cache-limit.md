# fix/cache-limit

- `fix/cache-evict` を取り込み、共有 `ksn-<識別子>` を数えて消す。上限の既定は 2 GiB。`KSN_CACHE_GRACE` 秒 (既定 60) 以内のエントリは消さない。
- release でネイティブ実行ファイルができたときは、インタプリタ同梱のバンドル (`target/release/pkg` ほか) を書かない。
- `tests/packages.rs` の `release_build_excludes_tests_and_native_build_ignores_test_helpers` は、バンドルを書かない release の挙動に合わせて直した。
- `check/` の置き場は `check_cache.rs` が別の上限 (`KSN_CHECK_CACHE_MAX` 既定 64 MiB・件数 8192、猶予なしの LRU) を持つので、`evict` の対象にしない。
