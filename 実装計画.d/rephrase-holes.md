# 言い換えの台帳 (hole 37 件) の仕分け (check/rephrase-holes、2026-11-25)

結果: 本物 22 件 → 直した 14 件 (リテラルの昇格 12 + `mem.swap` の再借用 3 のうち台帳に載っていた 12+3 の 15 件、うち下の表) / 道具のうそ 14 件 → 道具を直して台帳から外した / 残り 8 件 (sec6、仕様の判断が要る)。

| 台帳の項目 | 判定 | 理由 | 対処 |
|---|---|---|---|
| fold dangling_static_field / dangling_via_let_struct / return_local_* (10 件) | 本物 (a) | `&7u32` を関数の外へ返す形。仕様 (昇格は const/static の初期化式と `&'static` の注釈つき `let` だけ、戻り値の `&10u32` は E3030) どおりなら誤り。ネイティブは `alloca` を指す (`ksn build --release --backend llvm --emit-llvm` で確認) のでぶら下がる | `Loan::Promoted` を足し、昇格は局所では通し (文の終わりで尽きない)、戻り値・`'static` の欄・外へ生きる保存・閉包の戻りでは `Loan::Temp` と同じに誤り。`let r: &'static T = &10u32` は静的として通す。`unsound/promoted_ref_*` に 12 本 |
| hoist flow_swap_ref_vars / option_ref_vars / generic_relay `#3` | 本物 (a) | `let p = &mut ry; mem.swap(&mut rx, p)` で `rx` と `ry` の指す先が入れ替わるのに、`p` を介すと流入が落ちた (`x = 3` の後の `*ry` が `x` を指す) | `Src::Pointee` と `Ev::BindThrough` を足し、`&mut *p` の再借用の経由で `p` が指す持ち主へ流す。`unsound/reborrow_var_flow_swap_*` に 3 本 (block / hoist 2 回 / id の言い換えも塞がる) |
| fold linear_leak_on_try | 道具のうそ (b) | `n` を式へ折りたたむと `?` の評価が `close(r)` の後ろになり、`r` は先に消費される (元と評価の順が違う) | fold は `?` を含む初期化子を飛ばす |
| fold linear_short_circuit_and/or_rhs_consume | 道具のうそ (b) | `open()` を `&&` / `||` の右辺の中へ移すと、作る所と消費が同じ条件の中になり漏れない | fold は `&&` / `||` の右辺への折りたたみを飛ばす |
| fold arr_repeat_len_local | 道具のうそ (b) | `[1u32; n]` の `n` をリテラルにすると定数の長さで正しい | fold は `[e; n]` の `n` を飛ばす |
| fold temp_vec_same_len / temp_string_same_len | 道具のうそ (b) | `let r = same(&mk()); r.len()` → `same(&mk()).len()` は一時値が文の終わりまで生きて正しい | `rhs_borrows_temporary` に「引数の `&<呼び出し>`」を足した |
| fold escape_tuple_block_value / borrow_mut_immutable_generic_result | 道具のうそ (b) | `&x` / `&mut a` の `x` を式にすると、局所の借用が昇格・一時値の借用になり別の意味 | fold は `&` / `&mut` の直後を飛ばす |
| fold hunt_a13_assign_through_shared_ref_04 | 道具のうそ (b) | `r.v` の `r` を `&h` に替えると `&h.v` (優先順位がちがう) | `&` `*` `-` `!` で始まる初期化子は括弧で包む (今は E3031 で拒まれる = changed の台帳) |
| fold hunt_a17_reserved_false_bind | 道具のうそ (b) | `let false = 5i32` の `false` を名前として扱った | `true` / `false` は名前にしない |
| match2iflet exh_match_* (2 件) | 道具のうそ (b) | `else` を足すので網羅しない `match` が網羅する `if let .. else` になる | `SOUND_REWRITES` で飛ばす |
| iflet2match if_let_linear_else_leak | 道具のうそ (b) | `else` の腕で `o` は `None` と決まるので `match` の `None` の腕に漏れは無い (`if let` の方が保守的) | `SOUND_REWRITES` で飛ばす |
| block / extract / hoist / id / fold sec6_fnvalue_genericvar_borrow (8 件、fold は昇格の形で外れた) | 本物 (a)・仕様の判断待ち | 元のケース自体が `KNOWN_HOLES` (効果が変数 `E` の `f()` は中断点にならず、借用を跨いで持てる)。`gs` を本体で一度だけ検査するため、呼び出し元の実体化 (`suspend`) が届かない | 台帳に残す (下の判断) |

## 判断が要るもの

- sec6 (E3050 の総称の効果): 案 A: `Fn<.., E, P>` の `E` の呼び出しを、借用 (ガード以外) を持つ間は fail-closed で E3050 にする (コールバックを取る総称関数の多くが誤検出になるはず)。案 B: 関数ごとに実体化した呼び出し元から再検査する (単相化後の検査、コストが大きい)。案 C: `E` に `suspend` を含みうる関数は借用を持って呼ばない、と仕様に書き、`gs` の側で `E: uses` に `suspend` を含まないことを求める境界を足す。
- 仕様どおりだが、`let r: &'static u32 = &10u32` の後に `r` を返す形は昇格を静的として通す (ネイティブは今 `alloca` のまま: 静的な置き場に出す下ろしが要る)。

## 続き (2026-11-25 夜)

- 昇格した定数の参照 (`&10u32`) は、KIR で `Op::StaticBytes` (LLVM は `private unnamed_addr constant ... align 16`) に置く。`stage0/tests/promoted_static.rs` で木走査・VM・release ネイティブが一致、ASan (`clang -fsanitize=address`) も通る。
- `HashMap<&str, V>` の `get_str` は E1108 (`types.rs` のレシーバの型引数で `String` と `str` を区別)。`unsound/hashmap_str_key_get_str`。
- sec6 は案 B: `suspend` を渡して実体化した総称関数 (`TypeCk::susp_inst`、効果変数のまま呼ぶ先へ `edges` でひろげる) は、本体の `E` の呼び出しを中断点にして借用検査をやり直す (E3050)。`KNOWN_HOLES` は空、台帳の hole は 0。`tests/check_diff.py` で 1783 件、差は期待した 28 件 (新しい unsound の拒否・get_str の 8 本・sec6 の 2 本ほか) だけ。
