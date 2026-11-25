# rephrase-fold

### 2.10?? 言い換え `fold` の一時オブジェクト生存期間の修正（精密版）

- `fold` (`let` のインライン展開) で、初期化子が一時オブジェクトを借用する値を束ねる `let` のみを飛ばす。
  - (a) トップレベルが `&expr` / `&mut expr` で、`expr` がプレーンなプレース (ローカル名、それに `.field` / `[単純インデックス]` を続けた形) でない (`&t.make().v`、`&t.make_string().as_str()` など)。`&h` や `&p.v` のようなプレーンなプレースへの借用は飛ばさない。
  - (b) レシーバ自身が呼び出し結果（一時オブジェクト）であるメソッド呼び出し (`x.lock().get()`、`v.iter().next()` のように `.<メソッド>(` のレシーバが `)` で終わる形)。`t.id(s.n)`、`t.open()`、`t.pick(&t.mk(), n)` などの FQN/関数呼び出しは飛ばさない。
- `&&` / `*&` / `**` など記号が並んで文法エラー (E0002) になる挿入は、従来どおりガードで防ぐ。
- このためだけに生じていた ledger 行を台帳から外す (FIXED)。`NEW` は 0 件（実 hole を隠していない）。

台帳から外した行（計 11）:
- rephrase_known.txt: `changed fold flow_ref_field_borrow_inherits_loan#0`, `o02__replace_vec_through_shared_dangling#0`, `ref_of_ref_deref#0`, `temp_let_as_str_ref#0`, `temp_let_field_ref#0`, `temp_let_string_field#0`, `temp_let_whole_ref#0`、`hole fold flow_region3_reborrow_index_store#1`, `temp_ref_str_len#0`, `temp_ref_vec_len#0`
- rephrase_known_fp.txt: `fp fold borrow_scalar_results_ok#0`

残った fold 行（実 hole / まだ生成されるもの）は維持: `hunt_a05_*` (2)、`linear_array_len_discard#0`、`sec6_fnvalue_borrow#1`、`sendbound_fn_value_let#0`、`temp_two_arg_elision_len#0`、`borrow_mut_immutable_generic_result#0`、`hunt_a13_assign_through_shared_ref_04#0`、`linear_leak_on_try#0`、`linear_short_circuit_and_rhs_consume#0`、`linear_short_circuit_or_rhs_consume#0`、`temp_string_same_len#0`、`temp_vec_same_len#0`、fp の `flow_default_ok#2`、`temp_named_and_in_statement#0`、`with_body_ref_let#0`。
