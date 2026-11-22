### 2.10?? 閉包の戻り値の関数値の効果 (F4)

`tast_rules/effects_conv.rs` が閉包の戻り値 (本体の末尾) を宣言した戻り値の型と `walk` していなかった。`TKind::Closure` ごとに本体の型を戻り値の型へ照らし、閉包の中の `return` は一番内側の閉包の戻り値の型と比べる (今まで外側の定義の戻り値の型と比べていた)。試験: `unsound/closure_ret_*`、`check_ok/closure_ret_fn_effect_ok`。

P1 の追補: 関数型の値を式で呼ぶ `g()()` (呼び出しの結果・`if`/`match` の値など、経路でない被呼び出し式) は、その関数型の効果・panic を呼び出し元が負う (`effects_inst.rs`、E4001 / E4002)。試験: `unsound/call_result_fn_value_*`、`check_ok/call_fn_value_*`。欄の関数値は `(h.f)()` の書き方が無く、`let g = h.f` を経由する (既に検査済み)。
