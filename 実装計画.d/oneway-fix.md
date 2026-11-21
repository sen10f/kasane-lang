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

#### 引き継ぎ後の追加 (2026-11-21)

- E2062 (何も変えない整数演算のリテラル `wrapping_add(0)` / `wrapping_sub(0)` / `wrapping_mul(1)`)・E2063 (`bool.then_some` → 複数行の `if`。行全体がこの式のときだけ fix)・E2064 (`from_str_radix(s, 10)` → `parse(s)`)。`explain` と `tests/data/diag/` と `fmt_fix.rs` の `oneway_*` に試験。
- 決定の変更 (統括者): `x == None` / `x != None` の `fmt` の書き換えを `.is_none()` / `.is_some()` から `is None` / `is Some(_)` に変えた (`parser.rs` の `cmp_rewrites`)。`oneway_stage3.rs` の `d11_*` の期待を新しい正規形に直した (意図した変更)。E2055 の `scalar_payload` の制限は外した (`is` の `Some(_)` は借用だけ)。`parser.rs` の `sort_unstable` の黙った書き換えは削除 (E2058 が拒み `ksn fix` が直す)。
- `ksn todo` を廃止 (`cmd_todo`・仕様書・チートシートの行)。`check_diag.rs` の `todo_is_a_typed_hole_that_panics_and_is_listed` は `todo_is_abolished_and_ksn_fix_rewrites_it` (E2057 → fix → 走る) に置き換えた (意図した変更)。
- 移行: `examples/native`、`spec/stdlib/core/slice.ksnl` (`b.wrapping_sub(0usize)` → `b`)、`tests/data` の 3 本 (`linear_ok/*`・`unsound/iter_panic_fold_map`)、試験の中の inline プログラム (`core.check.eq(x.is_some(), true)` → `core.check.is_true(x is Some(_))`、`from_str_radix(s, 10u32)` → `parse(s)`、`sort_unstable` → `sort`、`then_some` → `if`)。
- 未了 (理由): `todo` / `sort_unstable` / `then_some` を `spec/stdlib/*.sig`・組み込みから消すこと (消すと E2057/E2058/E2063 に届く前に名前の未解決で止まり、`ksn fix` が使えなくなる。次の版で消す)。bool の `match` → `if` と 1 腕 `match` → `if let`・数え上げの `while` → `for range` (複数行の書き換えで、意味が同じと言い切れる形の見極めが要る)。`== Some(v)` → `is Some(v)`。監査項目の (a) のうち `shl(0)`・`x | 0` などの二項演算と二重の `wrapping_neg` (E2062 はメソッド呼びの 3 形だけ)、(b) `let mut` の未変更 (E1020 の範囲の確認が要る)、(d) の `alloc.fmt.write`、(g) 閉包の関数値への置き換え。
- 追加 (統括者の依頼): E2065 (基本型の `.clone()` → 値)・E2066 (整数の `!(a < b)` → `a >= b` など。小数は対象外)・E2067 (引数で決まる `f::<T>(..)` の `::<T>` を消す。書いた型引数を引数の宣言の型に入れた型が、引数の式 (リテラル・名前・その `&`) の型と一致するときだけ。食い違いは E2023 のままで、直して通さない)。試験は `fmt_fix.rs` の `oneway_*` と `tests/data/diag/E2065〜E2067`。移行は `ui/kasane` のターボフィッシュ 7 か所と `check_ok` の 2 本。
- 訂正: 線形の値を含む `Option` / `Result` に `x is Some(_)` は使えない (E3003: 線形の値が `_` で捨てられる)。E2055 は「線形を含まない」型 (`plain`) だけを直す。このため `fmt` の `== None` の書き換えは `.is_none()` / `.is_some()` のまま (字句の段では型が分からない) で、`oneway_stage3.rs` の `d11_*` は「fmt → check が E2055 → `ksn fix` で `is`」の 2 段を見る (統括者の決定の実装上の修正)。
- 台帳: `shadow_diff` は廃止されたので `shadow_baseline.json` は触らない (前任の差分の 6 行は残っている)。
