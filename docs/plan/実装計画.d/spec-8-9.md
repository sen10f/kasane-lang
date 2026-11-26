# 仕様の文書化 (branch docs/spec-8-9)

ドキュメントのみの変更。`.rs` は触らず、stage0 のビルドもしない。

## 変更点

1. `Kasane 簡易仕様書.md`
   - (8) `panics never` の約束の範囲を明記：添字・整数オーバーフロー・ゼロ除算・`unwrap`・明示的な `panic` のみを約束。デッドロック検出（終了コード101）、資源の枯渇、呼び出しの深さの上限（終了コード102）は約束の外（挙動は変えず）。`panics never` の定義箇所と、exit 101/102・デッドロックが記述されている箇所に記述。
   - (9) E3060 を拡大：`impl` 行のトレイトの型引数に参照を置けない（`impl Iterator<&u64, ..> for X` は E3060）。`&'static` と標準のライフタイムを運ぶ型は例外。既存の E3060 記述の隣に追記。
   - 変更履歴（10 章）に次バージョン（0.10.44、パッチ番号を1つ上げる）のエントリを追加。
2. `チートシート.md`
   - (8b) `panics never` 下の整数の勧めを `checked_*` + `ok_or(..)?` / `match` の基本に、`wrapping_*` は巻き戻しの意図のときだけ、`saturating_*` はクランプの意図のときだけ。例を3つ（各メソッド1つ）示し、スクラッチパッケージで `ksn check` と `ksn fmt --check` を通す。
3. `BETA.md`
   - `panics never` の説明に、101/102/デッドロックが約束の外である旨を追記。
4. コミット（worktree内のみ。pushはしない）。

## 確認

- `python3 -I spec/tools/check_all.py` （spec/ のみ。`.md` 変更なので影響なし、念のため）
- スクラッチパッケージ（`/tmp/opencode/specm8_scratch`）で `KSN_CACHE_DIR=/home/kasane/coord_tmp/specm8_cache /home/kasane/kasane-verify/bin/current/ksn check` と `ksn fmt --check`
- `tests/doc_examples.py` は存在しないのでスキップ
