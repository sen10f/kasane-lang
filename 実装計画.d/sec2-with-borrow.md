# sec2-with-borrow (M8)

`tast_rules/borrows.rs` の `Walker::other` に `"With"` 枝を足した。初期化式 → 束縛 (`bind_pat`、`let` と同じ) → 本体 → 後始末の順で歩くので、`with` の束縛も E3010 などの対象になる。
再現: `unsound/with_vec_push_while_borrowed` (M8b)、`unsound/with_swap_while_borrowed`。

追加: unsound に with_{swap_outer,field_assign,capture_mut,for_iter}_* を足した。check_ok に with_borrow_ends_before_mutation_ok / with_mutate_inside_body_ok。他の規則の監査: moves(E3040)・mutability(E3001)・discard(E2042)・effects(E4001)・oneway(E2055)・ref_levels(E2023)・exhaustive(E2021) は with の本体で働くのを確認。lifetimes は既存の with_* 試験で覆われる。
