### 2.10?? S4G-8: 旧い出力の最後を TAST の規則へ

- **E3041 / E3042** (`with` の `close`): `with_check.rs` の旧い出力と `consuming_use` / `node_consumes` / `cleanup_exit` を削除。`tast_rules/moves.rs` の `with` が出す。E3042 は `close` の中の `return`/`break`/`continue`/`?` (閉包の中は数えない)、E3041 は `close` を流した後に束縛の全体が `must` (全経路で move 済み) に入っているか。
- **E3020** (署名・型の欄のライフタイムの省略): `check.rs` の 4 か所 (`check_elision`・`check_trait_lifetimes`・`check_field_elision`) の出力を削除し、`tast_rules/lifetimes.rs` の `sig_elision` へ。`missing_life_args` もそちらへ (E3021 の早期 return のため `check.rs` も呼ぶ)。
- **E2023** (総称の実体化・閉包の引数・`dyn` と境界越しのメソッド): `types.rs` の `TypeCk` は推論だけを行い、E2023 を出さない (16 か所を削除。リテラル・タプル・構造体・列挙子のパターン、配列の要素、構造体リテラルの欄、明示型引数の数も、既存の規則 T が見る)。規則 T の追加:
  - 組み込みの自由関数 (解決先が `Native`。`core.iter.range` など) の引数を、宣言の型へ型引数を入れて照らす (これまでメソッドだけだった)。
  - `F: Fn<(..), R, ..>` 型パラメータに渡した閉包の引数の個数・型・戻り値を境界と照らす (`fn_bound`)。
  - `dyn` 越し・型パラメータの境界越しのトレイトメソッド: `TypeCk` が実体化した仮引数の型を `side.arg_wants` (呼び出しの鍵) に残し、規則 T が実引数と照らす (`trait_args`)。境界の `PartialEq<Self>` の `Self` はレシーバの参照を外した型にそろえた (`&K` のレシーバで `&&K` を求めていた)。
- **移せなかったもの**: `check.rs` の「型の注釈の中の型引数の個数」(`Option<u32, u8>` など、`targs_*` の `unsound/` 4 本) の E2023。型の構文 (`V`) を見る宣言の検査で、TAST に型の構文が無いので旧パスに残す。
- 回したもの: `check_diag` `check_borrow` `tast_type_eq` `tast_lifetimes` `tast_borrows` `tast_moves` `rephrase` (全部通過)、`verify_reference` bad 0、`cargo check --release --tests`、`examples/` `apps/` `selfhost/` `ui/kasane/*` の `ksn check` (期待の 2 件以外 0)。
