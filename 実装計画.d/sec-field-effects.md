# sec-field-effects

- 原因: `tast_rules/effects_inst.rs` の `bound_recv` と `f.call(x)` の規則が、経路形の呼び出し `a.m()` を 2 区間 (`s.len() == 2`) に限っていた。`h.f.call(())`・`w.x.go()`・`w.it.next()` のような欄越し (3 区間以上) は受け手の型 (`RecvPath`) を持つのに検査されなかった。
- 直し: 2 区間以上で、最後の区間をメソッド名とする。F1〜F5 と 2 段の欄を `unsound/field_recv_effects_*` に、正しい宣言を `check_ok/field_recv_effects_ok` に。
