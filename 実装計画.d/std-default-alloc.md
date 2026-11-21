# 実装計画.d/std-default-alloc.md — デフォルト確保器の短縮型名

## 2.10ga デフォルト確保器の短縮型名とコンストラクタの省略 (std.x.Y)

### 概要
確保器を明示させない既定の確保器 (`std.alloc.System`) の短縮型名を 7 種追加:
`std.vec.Vec<T>`、`std.string.String`、`std.collections.HashMap<K, V>`、
`std.collections.BtreeMap<K, V>`、`std.collections.HashSet<T>`、
`std.collections.VecDeque<T>`、`std.boxed.Box<T>`。
正式名は `alloc.x.Y<.., std.alloc.System>`。

### 実装箇所
- `stage0/src/types.rs`: `ALLOC_ALIASES`、 `std_to_alloc` / `alloc_std_alias` / `alloc_short_name` / `alloc_ctor_allocator_param`。`from_v` で短縮名→正式名＋`std.alloc.System` 補完。`Ty::show`/`show_r` で短縮表示。`call_def` で `argt.len() == d.params.len() - 1` かつ短縮名呼び出しならアロケータ型パラメータに `System` を束縛。
- `stage0/src/program.rs`: `expand_alias_segs` で `std.x.Y` → `alloc.x.Y`。
- `stage0/src/check.rs`: E2001 で `alloc_ctor_allocator_param` を許容；型不一致比較で `std_to_alloc` 正規化；`alloc.boxed.Box` の `dyn` チェックに短縮名も含める。
- `stage0/src/fmt.rs`: `rewrite_alloc` / `canon_line_with_alloc` で `alloc.x.Y<.., System>` → `std.x.Y<..>` と `alloc.x.Y.new(std.alloc.SYSTEM)` → `std.x.Y.new()`。`end <mod>.new` 行はスキップ。
- `stage0/src/cli.rs`: `cmd_fmt_paths`（非パッケージの `ksn fmt`）。
- `spec/tools/xref.py`: `ALLOC_ALIASES` で `std.x.Y` 参照を known と見なす。

### 試験
- `stage0/tests/std_default_alloc.rs`: fmt 書き換え、長い形式の E0020、`ksn run == native`、他の確保器付き型の短縮名。
- `check_diag`: `arity_vec_new_zero`（長い形の裸 `new()`）を削除（fmt で短縮形に正規化され有効になるため廃止）。
- 回帰基準: `examples/` 全パッケージ、`bench/tasks/*/reference.ksnl` が診断 0 件。

### ドキュメント
- `実装計画.md` に `### 2.10ga` を追加（本ファイルの要約）。
- `チートシート.md`、`Kasane 簡易仕様書.md`（8.3 の「既定アロケータ版の型」行）に反映。

### コミット履歴 (この worktree)
1. `e1c7cbcd` 実装 + 試験 (stage0/src, stage0/tests/std_default_alloc.rs, check_diag.rs, fmt_fix.rs, arity_vec_new_zero 削除)
2. `03da5b76` コーパス再フォーマット (examples/apps/selfhost/ui/spec/stdlib/std/bench/tests data)
3. `6afc9dfa` xref.py で短縮型名を認識
4. (未) 実装計画.md / チートシート.md / 仕様書 更新 + 実装計画.d/std-default-alloc.md

### 残り作業
- チートシート.md と Kasane 簡易仕様書.md の更新。
- 実装計画.md と本ファイルのコミット。
- フルの `cargo test --release --no-fail-fast` は 30 分超でタイムアウトのため回さない（coordinator 規則）。回帰は `check_diag` (303 件 pass)、`fmt_fix` (33 pass)、`sig_impl_parity` (2 pass)、`verify_reference.py` (bad 0)、`check_all.py` (ALL CHECKS PASSED)、`cargo check --release --tests` (クリーン) で代替。
