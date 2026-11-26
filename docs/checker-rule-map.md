# 検査の規則の地図

どの診断コードをどのファイルのどの関数が出すか。`stage0/src/` を grep して作った表です (規則を動かすと変わる)。

## 検査の流れ

1. `check.rs` (旧い検査の残り。宣言・トレイト・効果の宣言・線形の消費義務・unsafe の表面) と `types.rs` (型推論。型の規則の一部もここで報告) が走る。
2. `tast_check.rs::check` が `types::TypeCk` で型を決め、`tast::build` で TAST を作り、`tast_rules/*.rs` の規則を順に走らせる。**借用・ライフタイム・移動・可変性・効果の判断の本体は TAST**。
3. `kir_borrowck.rs` は KIR 上の借用検査 (`ksn kir --borrowck` 相当、`cli.rs:1210`)。通常の `ksn check` の判断には使わない (照らし合わせの補助)。
4. `borrowck.rs` は括弧・カンマの文面の補助だけ (診断なし)。`refs_check.rs` は字句と推論済みの型だけで E3060 などを出す。`with_check.rs` は `with` の E3043 など (大部分は TAST へ移した)。

## TAST の規則 (`tast_rules/`)

| ファイル | 出す診断 |
|---|---|
| `moves.rs` | E3002 E3003 E3006 E3040 E3041 E3042 (移動・線形・`with`) |
| `borrows.rs` | E2074 E3010 E3050 (借用の重なり・スレッド越し) |
| `lifetimes.rs` | E2037 E3010 E3020 E3030 E3043 (領域・持ち出し) |
| `mutability.rs` | E3031 |
| `effects_exact.rs` / `effects_inst.rs` / `effects_conv.rs` / `captures.rs` | E4020 E4025 / E4001 E4002 (効果の厳密・実体化ごと・変換・閉包の取り込み) |
| `impl_sig.rs` | E1112 E1122 E2046 E4021 E4025 |
| `type_eq.rs` / `ref_levels.rs` | E2020 E2023 E2024 |
| `exhaustive.rs` | E2021 E2052 |
| `bounds.rs` | E2030 |
| `discard.rs` | E2042 |
| `oneway.rs` | E2055〜E2073 (書き方は 1 通り系) |
| `unsafe_calls.rs` | E5001 E5003 |

## 旧いパスに残る規則 (`check.rs` / `types.rs` / `infer.rs`)

| 系 | コード | 出す場所 (関数) | 残る理由 |
|---|---|---|---|
| 型推論 | E2001 E2022 E2025〜E2029 E2044 E2045 E2049 E2053 E2054 | `types.rs` (`infer_inner`・`finish_literals`・`report_undetermined`・`check_try_err` ほか)、`infer.rs` (`annotate_int_lits`) | TAST を作るための型決めそのもの。TAST の前段なので移せない |
| 型の一致・境界 (旧側) | E2020 E2023 E2024 E2030 E2037 | `types.rs` | TAST の規則と二重。型決め中の報告が残る (TAST 版が主) |
| 可変性 (旧側) | E3031 | `types.rs` (`need_mut`) | 型決め中の判断。TAST 版 `mutability.rs` と併存 |
| ぶら下がり | E3030 | `types.rs::check_dangling` | 型の位置に依る旧い判断 |
| 宣言・トレイト・impl | E1021〜E1126 E2031〜E2043 E2047〜E2051 | `check.rs` (`check_declarations`・`check_impls`・`check_derive`・`check_dyn_types`・`check_static*`・`visit`) | 定義の構造だけを見る検査で TAST 不要 |
| 線形の消費義務 | E2032 E3001 E3004 E3005 E3021 | `check.rs` (`check_linear`・`tc_stmt`・`visit`・`count_lifetime`) | 文の並びの走査に結び付く。移動の規則 (`moves.rs`) とは別に残る |
| 参照は二級 | E3060 | `check.rs::typeck`、`refs_check.rs` | 型の宣言と使用の位置の字句検査 |
| 効果・再帰 | E4003 E4004 E4010〜E4015 E4022 E4023 | `check.rs` (`app`・`visit`・`check_declarations`・`check_nested_block_on`) | 呼び出し図の走査 (効果の宣言と `stack N`)。厳密版は `effects_exact.rs` |
| unsafe・FFI・Send | E5001〜E5011 | `check.rs` (`check_unsafe_surface`・`check_extern_sig`・`check_callback_args`・`check_layout_c`・`check_send_sync_site`) | 宣言・署名の形の検査 |
| 呼び出しの引数 | E2040 E2041 E2048 | `check.rs::visit`、`types.rs::check_spawn_captures` | 式の形の走査 |

そのほか: `kir_lower_check.rs` は E2036、`program.rs` は E0002・E0067・E1126・E2053・E3020・E4014 (構築時)、`parser.rs` は E0002・E0020・E2021・E2042・E3003 の一部 (構文の段) を出す。

## KIR の検査

`kir_borrowck.rs` (`check`・`check_func`) が E3002 E3003 E3010 E3030 を KIR の上で判断する。`Outcome::Unknown` のときは判断しない。
