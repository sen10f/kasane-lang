### 2.10?? Rust の癖の形: `fix` の 3 形と文面の 4 形 (B4)

- `ksn fix` の前段 (`surface_fix.rs`): 1 行の `match x { A => a, B => b }` を腕ごとの行に分ける (腕の `,` は消す)。`fn` の見出しで戻り値を省いたもの (`uses` / `panics` の前) に `-> ()` を足す。`Pat => place = v` (`*` 付きを含む) を `Pat => {` ・代入 ・ `}` の 3 行にする。
- 文面 (`rustisms.rs` の `guided_forms`、E0002 として出す。`fix` は出さない): パターン・引数の `mut` (`let mut s = s0` を言う)、`end` の変数・引数・メソッドの名前、欄つきの列挙の要素 (組か `type` を言う)、値を持つ `break` (`break` だけ・結果は `let mut` で運ぶ)。
- 新しい診断コードは足していない (試験は `fmt_fix.rs` の `fix_surface_*`、`check_diag.rs` の `rust_forms_forbidden_by_decision_get_guidance_not_a_fix`)。
