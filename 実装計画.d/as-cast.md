### 2.10gr `ksn fix` で `as` キャスト (E0042) を変換関数に書き換え

- 背景: `as` は Kasane の予約語で構文エラーになる。`ksn check` は `as` の行に届かず、rustisms のみで E0042 を報告する。`ksn fix` はこの診断に修正案を付けられず、ユーザーは手で変換関数を書く必要があった。
- 直し: `stage0/src/fixes.rs` の `fixes_for` に E0042 の腕を追加。`x as T` の `x` が直上の `let x: Type =` で型注釈付きのとき、かつ `Type` と `T` がどちらも整数型で幅が広がる変換なら `T.from_Type(x)` に書き換える（`stage0/src/types.rs` の `conv_hint` と同じ広がり判定）。縮小・符号変更は `try_from` が `Result` を返すため機械適用不可とし、そのまま残す。float や型が決まらない場合も同様にそのまま残す。
- 文面: `stage0/src/explain.rs` の E0042 を更新し、`try_from` が `Result` を返すこと、周囲で `?` や `match` で受ける必要があることを明記。
- 試験: `stage0/tests/fmt_fix.rs` に `fix_as_cast_widening`（`u8` → `u64` の `as` が `u64.from_u8(x)` に直り `check` が通り冪等）と `fix_as_cast_narrowing_left_unfixed`（`u64` → `u8` は書き換えられず E0042 がそのまま）を追加。
- ゲート: `cargo test --release --test fmt_fix --test check_diag --test tooling`、`cargo check --release --tests` は全件通過。
