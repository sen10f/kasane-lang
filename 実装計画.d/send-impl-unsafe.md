# check/send-impl-unsafe: 生ポインタを持つ型への Send/Sync impl

- 決定 (2026-11-23 案 A): 新しい構文は作らない。
- 現状の検査: `check.rs` の `E5002` が、手動の `core.marker.Send` / `Sync` impl を `[unsafe] modules` の外で拒む (型が生ポインタを持つかは見ない = 持たない型も同じ扱いで、既存の振る舞いのまま)。許可モジュールの中では通る。
- 追加した試験: `unsound/send_impl_raw_ptr_normal_module` (E5002)、`unsound/send_raw_ptr_spawn_without_impl` (E2040)、`check_diag::send_impl_for_raw_pointer_only_in_unsafe_module` (許可の中は通る・外は E5002)。
- 数え上げ: `unsafe` の使用を数える道具は今の stage0 に無い (`audit` 等なし)。よって数える所は未変更。
