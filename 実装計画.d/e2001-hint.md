### 2.10?? E2001 にデフォルト確保器の短い書き方を案内する

- `alloc.x.Y.new()`（アロケータ未指定）で E2001 が出るとき、メッセージに `std.x.Y.new()`（デフォルト確保器）と `alloc.x.Y.new(std.alloc.SYSTEM)` の両方を案内するようにした（`stage0/src/check.rs`）。
- `ksn explain E2001` の本文を更新（`stage0/src/explain.rs`）。
- `ksn fix` で `alloc.vec.Vec.new()` / `alloc.string.String.new()` を `std.vec.Vec.new()` / `std.string.String.new()` に短縮する修正を追加（`stage0/src/fixes.rs`）。`from_str`/`with_capacity` 系には短縮形が無い（`short_ctor_call` は `.new` だけ）ので案内文を付けず E2001 だけ報告する。
- 試験を `stage0/tests/std_default_alloc.rs` に追加（ヒント文・`fix` 後の短縮・`from_str`/`with_capacity` は案内文なし）。

「two spellings」の調査（コード修正は行わず、報告のみ）:
- (a) 裸の `alloc.string.String` は `ksn fmt` でも短縮されず `std.string.String` にならない。対象ファイル 3 本（`selfhost/lower/src/lower/tyk.ksnh/.ksnl`、`selfhost/types/src/types/ty.ksnh`）。
- (b) `alloc.boxed.Box<dyn ...<[], never> + 'static, std.alloc.System>` は `short_type_args` で短縮されない。原因は `parse_group_args` が `]`/`)` を深さカウントに入れず、型実引数の中の `[]` でパースが失敗するため。対象ファイル 2 本（`examples/plugins/src/plugins.ksnl`、`bench/tasks_large/discount/reference.ksnl`）。
- どちらも `examples/`/`perf/`/`bench/tasks/*/reference.ksnl` は編集せず、ファイル数だけ報告。統括者が移行する。
