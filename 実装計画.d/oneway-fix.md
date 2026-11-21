### 2.10?? 書き方は 1 通り (規則 O): Option の判定・todo・sort_unstable・is_true・trim・push_display の正規形 (2026-11-21)

- 形: `tast_rules/oneway.rs` (TAST の型と解決先で見る)。診断の `ksn fix` は `fixes.rs` の `oneway_fix` (行の中で一意に決まる形だけ直す)。
- E2055: `Option` / `Result` の `is_some`・`is_none`・`is_ok`・`is_err` を `式 is Some(_)` 等にする (`!x.is_none()` は `!(x is None)`)。`Option` と `None` の `==` / `!=` も同じ規則 (ただし fmt が `.is_some()` 等へ先に書き換えるので、この形は届かない)。**値の型が基本型 (`Copy`) の `Option` / `Result` だけ**: `is` は値を取るので、非 `Copy` (線形を含む) では移動が増える。
- E2057: `core.panic.todo()` → `core.panic.panic("todo")`。E2058: `sort_unstable*` → `sort*`。E2059: `core.check.is_true(a == b)` → `core.check.eq(a, b)`、**基本型の両辺だけ** (`eq` は値を取るので、String・ハンドルでは移動が増える。`Debug` の無い型も同じ理由で除く)。
- E2060: `trim_start().trim_end()` (両順) → `trim()`。E2061: `push_display(&"lit")` → `push_str("lit")`。
- 試験: `tests/fmt_fix.rs` の `oneway_*` (診断 → fix → check → fix が冪等)、`tests/data/diag/E2055`・`E2057`・`E2058`・`E2059`・`E2060`・`E2061`。
- 移行: `examples/ffi`・`examples/ffi/shapes`・`examples/tutorial_ffi`・`examples/tutorial_bindgen`・`apps/csvtool`・`ui/kasane/app` の `is_true(a == b)` (`.ksnh` と `.ksnl` を `ksn strip` で揃えた)。`stage0/tests/data/ffi/*`・`linear_ok` の基本型の `is_true(x == y)`。
- 残り (未了): 規則 (2) `match` の `bool` → `if` (複数行の書き換え)、(6) 数え上げの `while` → `for range`、`Option` の 1 腕 `match` と `if let`、`core.panic.todo` と `sort_unstable` と `then_some` の stdlib からの削除 (今は診断と fix だけ。`parser.rs` の `sort_unstable` の黙った書き換えも残る)、新しい 9 点の残り (a) 恒等の算術・(b) 不要な `mut`・(c) `then_some`・(e) `from_str_radix(s, 10)`・(g) 閉包の eta。
- 競合: `設計_書き方は1通り.md` 12 章 (2026-11-19) の `x.is_none()` の正規形と、2026-11-21 の `is` の決定が食い違う。ここでは後者 (`is`) に従った。
- 試験の入力の `is_true(..)` のうち、型が基本型でないもの (`Handle`・`String`・`Ordering`) は今は直さず残した (診断が出ない)。
