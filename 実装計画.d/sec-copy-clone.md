### 2.10?? Copy / Clone の判定を 1 か所にそろえる (Mythos M4・M5)
- 原因: 境界 (`T: Copy` / `T: Clone`) の充足と `.clone()` の受け手の検査が、型を `strip()` (参照を剥がす) してから `unsatisfied` を呼んでいたため、`&mut T`・`Option<&mut T>`・取り込みが `move` の閉包が通っていた (二重所有・型の取り違え)。
- 直し: `types.rs` の `marker_unsat` が構造で決める (`&mut` は不可、`&` は可、タプル・配列・型引数は中身、閉包は取り込み (`&mut` と `FnOnce` は不可、`move` は中身)、残りは従来の `unsatisfied`)。境界の呼び出し (`tast_rules/bounds.rs`) と `.clone()` (`cmp_trait_missing`) の両方がこれを通る。
- 試験: `unsound/copy_*` 5 本、`check_ok/copy_clone_shared_ok`。
