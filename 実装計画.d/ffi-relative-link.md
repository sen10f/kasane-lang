# 実装計画.d/ffi-relative-link — B10 相対 link パスの CWD 脆弱性修正

## 目的 (B10)
`link "./libX.so:..."` などの相対パスを、実行時のプロセス CWD に対して `dlopen` / `LoadLibrary` で解くのをやめ、常に同じ起点（パッケージルート、ロード時に絶対パスへ展開）から解く。インタプリタ FFI（`ffi.rs::open_lib`/`dlopen`）とネイティブリンク（`kir_rt.c::ksn_ext_resolve`/`dlopen`、LLVM IR の `ksn.ext.lib.*` 文字列）の両方を同一の解決に一本化する。CWD は一切使わない。

## 方針（採択）
`Def.link` はロード時（`load_package` / `full_check` が `Package.root` を知っている段階）に絶対パスへ正規化し、そのまま下ろす（インタプリタ・ネイティブ両方で共有）。

- 相対パス（`/` で始まる、または `\` で始まる、かつルートからの相対であるもの → package root からの相対として解決）: `package_root.join(link)` に展開し、`Def.link` に絶対パスを書き込む。
- スラッシュのない単純な soname（`libm.so.6` / `qsort` / `abs`）は「パスではない」としてそのまま残す（OS のローダー / `dlopen` の探索規則に委ねる。これは CWD の穴ではない）。
- すでに絶対パス（`/` や `C:\` で始まる）はそのまま。
- `--release` でも相対パスを拒否しない（現行の `link "./libX.so"` 例が 0 診断で動く必要があるため）。解決不能な相対パスは、実行時に従来どおり「cannot load the shared library」で失敗（exit 101 / パニック）。check/build では診断しない。
- 例外（CWD での穴を塞ぐ対象）はパス区切りを含む相対パスのみ。

## 変更箇所
1. `stage0/src/project.rs::load_package`（または `load_package_src_opt` / `Package` の生成後）: `Package.root` を使って、各ソースの `Def.link` が相対パス（`/` または `\` で始まる）なら絶対パスへ正規化して書き戻す。これが唯一の解決地点。
2. `stage0/src/explain.rs`: 新規 E コード（例 E5010「相対リンクパスはパッケージルートから解く」）の説明を追加（後述の「拒否」を採用しなければ追加しない。当面は追加しない方針）。
3. `stage0/src/kir_rt.c`: 変更なし（`ksn_ext_resolve` は `Def.link` に渡された（絶対化済みの）パスをそのまま `dlopen` するだけ）。
4. `stage0/src/kir_llvm.rs`: 変更なし（`ksn.ext.lib.*` 文字列は `Def.link` の値をそのまま使う。絶対化済みなら CWD 依存にならない）。
5. `stage0/src/ffi.rs`: 変更なし（`open_lib` は `Def.link` の値をそのまま `dlopen` する。絶対化済みなら CWD 依存にならない）。
6. `stage0/tests/ffi_relative_link.rs`: 新規試験。相対 `link "./libX.so:..."` のパッケージを作り、異なる CWD に同名の evil `.so` を置いて実行し、evil がロードされない（正しいパッケージ内の `.so` がロードされる、または意図したシンボルが得られる）ことを assert。インタプリタ + ネイティブの両方で確かめる。

## 回帰基準
- `examples/tutorial_ffi`、`examples/ffi/shapes`、`examples/tutorial_bindgen`（`link "./lib*.so:..."`）が `ksn check` で 0 診断のまま（`.so` は `build.sh` で作る、コミットしない）。
- 既存 FFI 試験（`ffi_native.rs`、`ffi_userdata.rs`、`kir_native.rs` など）が通る。
- `cargo check --release --tests` が通る。
- 新規 `ffi_relative_link.rs` が新バイナリで通り、旧バイナリ `/home/kasane/kasane-verify/bin/current/ksn` では落ちる（手動確認）。

## 作業順序
1. この計画を書く（本ファイル）。
2. `project.rs` で `Def.link` の絶対化を実装。
3. 新規 E コード／説明を必要なら `explain.rs` に追加（当面は未追加）。
4. `stage0/tests/ffi_relative_link.rs` を作成。
5. `cargo check --release --tests` + 新規試験 + 既存 FFI 試験を回す。
6. 旧バイナリで新規試験が落ちることを手動確認。
7. `stage0/target` は worktree ごとに別なので初回ビルドに時間がかかる。
8. 30 分ごとにコミット。

## メモ
- `KSN_AVOID_CPUS` / `taskset` はこのマシンでは不要（Xeon VM）。
- `sccache` を使う: `export RUSTC_WRAPPER=~/.cargo/bin/sccache SCCACHE_CACHE_SIZE=8G`。
- `/tmp` は tmpfs。一時ファイルは `ffilink_*` のみ、12 時間より古いものは片付ける。
