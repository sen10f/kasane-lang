# sec-self-alias: 2 段階借用の &mut 受け手と先行する共有借用

- 穴: `let sl = v.as_slice(); v.extend_from_slice(sl)` が通り、再確保後に解放済みの領域を読む (UAF)。
- 原因: `tast_rules/borrows.rs` の `call()` は `&mut` の受け手の借用を引数の評価後に遅らせるが、最後の引数が運ぶ借用は一時変数に束ねず、有効化の時点で生きていなかった。
- 直し: 遅らせる受け手があるときは、全ての引数の借用を一時変数に束ね、受け手の借用の後で使う (`v.push(v.len())` のように借用を持たない引数は今まで通り通る)。
- 試験: `unsound/selfalias_*` (slice, inline, string, field, append, copy_sub, reborrow, chain, map_insert, user_method)、許す形は `check_ok/selfalias_two_phase_ok`。
