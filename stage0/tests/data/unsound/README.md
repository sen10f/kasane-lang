# unsound/ — 通ってはいけないのに通るコード

1 ファイル 1 ケース。`// expect: Exxxx` の診断が出ること、`ksn fmt --check` が通ることをテストが検査する。
まだ塞げないものは `stage0/tests/check_diag.rs` の `KNOWN_HOLES` に載せる。

## borrowck.rs の近似 (修正プラン 1-4)

方針は fail-closed: 借用を持つか分からない値は「持つ」とみなす。型が決まって借用を持ち得ない束縛
(`plain_binds`) だけは配らない。

| 近似の場所 (関数名) | 見落とす形 | 対応するケース | 状態 |
| --- | --- | --- | --- |
| `hold_pat` / `hold_whole_f` / 腕の `!known` | 形が分からない分解 `let (a, n) = f(&x)` / `match f(&x) { (a, n) => .. }` の束縛へ借用を配らない | `destructure_call_let`, `destructure_call_match` | 塞いだ (`plain` な束縛だけ除く) |
| `apply` の `Ev::Load` | 保存場所からの読み戻し `let a = h.r` で元の保存ローンを消す (move 扱い) | `stored_borrow_read_copy_keeps_source` | 塞いだ (コピーとして残す) |
| `apply` の `Ev::Access` | `&mut v` / `v.clear()` など Write 以外のアクセスで保存ローンを殺す | `stored_borrow_survives_mut_method` | 塞いだ (殺すのは `Write` のみ) |
| `flow_params` / `ret_contains_ref` | 戻り値が型パラメータ (`-> T`) の関数に、引数の借用が流れない | `generic_id_passes_borrow`, `generic_id_passes_struct_borrow` | 塞いだ |
| `method_sources` / `passthrough_sources` | `v.pop()` / `o.unwrap()` など戻り値が `T` のメソッドが、レシーバの中の借用を落とす | `vec_pop_returns_stored_borrow`, `option_unwrap_returns_borrow` | 塞いだ |
| `loan_sources` の `Unary *` | `let r2 = *rr` (`rr: &&T`) が内側の借用を落とす | `ref_of_ref_deref` | 塞いだ (`plain` でない束縛のみ) |
| `loan_sources` の `Call` | 局所のクロージャ呼び出しの結果が、取り込んだ借用を落とす | `closure_returns_capture` | 塞いだ |
| `hold_pat` の読み戻し (変数が持つ借用の欄) | `let w = mk(&x); let r = w.r` で欄の型が分からず借用が移らない | `boundary_field_read_back` | 穴 (`KNOWN_HOLES`) |
| `loan_sources` の `&*P` / `&mut *P` (再借用) | 再借用が元の参照 `P` の上の借用を作らず、`P` の読み書きや再借用を止めない | `hunt_a01_*` | 塞いだ (`P.*` への借用とアクセス) |
| `loan_sources` の `Closure` (`move r`) | `move` で取り込んだ参照が持つ借用 (`&mut x`) がクロージャへ移らない | `hunt_a05_move_andmut_ref_then_use_x_01` | 塞いだ (取り込んだ値の借用を写す) |
| `types.rs` の `call` (`f(x)`、E3031) | `FnMut` のクロージャを `&c` 越しに呼べる | `hunt_a05_andmut_via_shared_ref_01`, `hunt_a05_fnmut_*` | 塞いだ (型で判定。`&` を一つでも通して FnMut に届くと E3031) |
| 閉包の借用の寿命 (NLL) | `&mut` を取り込んだ `c` の最後の呼び出しのあとの借用は終わっている。`hunt_a05_andmut_capture_then_assign_01` などはこの正しい形 | `hunt_a05_andmut_capture_then_assign_01`, `_read_after_last_call_01`, `_via_shared_borrow_01`, `hunt_a05_nested_andmut_capture_01` | 仕様の判断待ち (`KNOWN_HOLES`。拒むと `inc(); inc(); x` を誤って拒む) |

精密化の例 (誤検出を出さない側): `linear_ok/borrow_scalar_results_ok.ksnl`。
