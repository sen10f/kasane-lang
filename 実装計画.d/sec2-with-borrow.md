# sec2-with-borrow (M8)

`tast_rules/borrows.rs` の `Walker::other` に `"With"` 枝を足した。初期化式 → 束縛 (`bind_pat`、`let` と同じ) → 本体 → 後始末の順で歩くので、`with` の束縛も E3010 などの対象になる。
再現: `unsound/with_vec_push_while_borrowed` (M8b)、`unsound/with_swap_while_borrowed`。
