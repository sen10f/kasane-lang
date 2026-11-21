### 2.10?? 関数値の効果は型から (Mythos F1〜F3)

- `sort_by` / `sort_unstable_by`: `Callee::Methods` (`.sig` と本体の同名の候補) を `effects_inst` が見ていなかった。同じ宣言を 1 つにして全候補を検査する。
- `effects_conv`: 代入 `g = f`・`*r = f` (右辺の型が左辺の型へ入る)、`mem.replace` / `Vec.push` (型パラメータを先の引数の型で束縛して仮引数へ)、局所の関数値・閉包の呼び出し (束縛の型の仮引数へ) でも関数型の効果を比べる (E4001 / E4002)。
- 試験: `unsound/fn_eff_*` 6 本、`check_ok/fn_eff_values_ok`。
