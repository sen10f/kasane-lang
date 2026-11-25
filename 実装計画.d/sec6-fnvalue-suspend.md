# sec6-fnvalue-suspend

関数の値の呼び出しの効果 (`types.rs` call_closure は decl 以外も記録、`effects_exact.rs` は let 束縛の関数値の効果を数える)。再現は unsound/sec6_*、正しい形は check_ok/sec6_*。

## 効果変数の呼び出し (`Fn<.., E, P>`)
- ロックのガード: `E` が変数の呼び出しは `Ev::SuspendGuard` (borrows.rs) で中断点扱い (fail-closed)。`unsound/sec6_fnvalue_genericvar_guard`。
- 開いている穴: 同じ呼び出しをまたぐ局所の借用・`Send` でない値・線形の値 (E3050 の借用 / E2074) は黙る (誤検出を避けるため。`closure_with_unknown_effects_stays_silent`)。実体化で `E` に `suspend` が入ったときの呼び出し側の検査は未実装。`unsound/sec6_fnvalue_genericvar_borrow` を `KNOWN_HOLES` に載せた。
- `uncertain` (effects_exact.rs) の追跡: E4025 の完全一致だけを黙らせる (広すぎる宣言を見逃すだけで、狭い側は E4001 が見る)。E3050 / E2074 は `uncertain` を使わず中断の印 (`susp_lines`) で決まる。E4022 は呼び先が決まらない・局所の関数値の呼び出しを `spawn_unsafe` (fail-closed)。したがって fail-open は上の 1 件 (効果変数) だけ。
