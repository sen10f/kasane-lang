# check/o04-static-annot — O-04/O-05: `'static` の場所への借用の保存を TAST 検査で拒む

対象: opus-vuln report の O-04 (`&'static` 注釈の局所への代入で借用が落ちる) と 2 回目の
(a) `let r: &'static u64 = &<非リテラル式>`・(b) O-05 (`static Mutex<&'static T>` へガード越しに
局所・省略引数の借用を入れる)。どちらも今は `[kir]` の E3010/E3030 だけが止め、KIR に下ろせない
関数では `ksn check` が 0 件で通す。

## 根

`tast_rules/lifetimes.rs` (規則 L) は `'static` の型を `all_static` として「領域を持たない値」
(`carries` 偽) と扱い、`let` の注釈・代入で入った借用を束縛に運ばない。書き込み先の型
(場所の型) に `'static` があっても、入る値の借用は見ない。

## 直し

- `W::static_store(v, tt, line)` を新設: 入る値 `v` が運ぶ借用 (Local/Dead/Temp/Arg/ArgIn) が、
  束縛・代入先の型 `tt` の `'static` を要求する欄に入るとき E2037。組・Option/Result は欄ごとに再帰。
  引数の内側の参照の層の領域が `'static` と宣言されている借用 (`ArgIn` の層が `'static`) は本物の
  `'static` なので許す (`ret_position` と同じ判定)。
- `let` 文の束縛 (注釈つき) で `bind_pat_ty` の PBind に `static_store` を当てる (`let_check` フラグで
  `let` からだけ有効化。`for`/`match`/`with` の束縛では誤検出を避けるため無効)。
- `assign()` の先頭で `static_store(&v, &place.ty, line)` — `r = x` だけでなく `*guard.get_mut() = x`・
  `s.f = ..`・`arr[i] = ..` など書き込み先の型が `&'static T` の場所すべてを覆う。
- リテラル昇格 (`&7u64`) は `is_const` 由来で借用を運ばないので従来どおり通る。

## 試験

- `stage0/tests/data/unsound/` に 4 Cases (O-04a・O-04b・O-05a・O-05b の再現 + 注釈 let の非リテラル形)
  を 1 ファイル 1 つで追加 (`expect: E2037`)。
- 回帰: `cargo test --release --test check_diag`、examples/・参照解・linear_ok の診断 0 件、
  FAST GATE (check_all / verify_reference / native_ratio / par_test --tier fast)、最後にフル `cargo test --release`。
