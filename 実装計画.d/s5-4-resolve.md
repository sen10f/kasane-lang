# S5-4 名前の解決 (途中: 経路の入口だけ)

- TAST の `TExpr` に `pres: Option<PathRes>` (`Path` 節点の解決先) を足し、`tast::build` が `p.resolve` を引いて持つ。下ろしは `TyIdx::paths` ((関数 FQN, 式 ID) → 解決先) を `Lw::path_res` で引く。
- 置き換えた所: `kir_lower.rs` の経路の値と呼び出しの callee (2 か所)。
- `.resolve(` の呼び出し (`kir_lower*.rs`): 前 34 → 後 33 (置換 2、`path_res` の ID 0 退避 1 を足した)。
- 残り: 式 ID 0 の合成節点 (`variant_method_call` の受け手)、`segs` だけを受ける補助 (coll・agg・ffi・fs・iter・method・strord・sync・enum・dyn・proc)、UFCS の別名の引き直し、定数・型のパターン。節点の ID を渡して `path_res` に寄せるのが続き。`from_v` の削除は未着手。
