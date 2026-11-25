# ### 2.10?? 文字列・名前つき型への算術演算子の拒否 (E2020) — `check/m4-str-plus-panics`

## 穴

`ksn check` は `+ - * / % & | ^` の被演算子がプリミティブのときだけ型を検査していた
(`bool` / `char` だけを拒否)。`str` (`"a" + name`) と、`String`・ユーザー構造体などの
名前つき型 (`Ty::Named`) は素通りし、`ksn run` は `panic: bad operands for +` (終了 101)、
`ksn build --release` は E9040 になった。`panics never` の関数でも通った
(mythos4-ux report 2.3、`trials/diag2/h01_*`)。

仕様 (簡易仕様書 4.1): `+ - * /` は整数と浮動小数だけ、`%` は整数だけ、
`& | ^` は整数と `bool` だけ。文字列の `+` は言語に無い
(`alloc.string.String.join` / `push_str` で連結する)。

## 直し

- `stage0/src/tast_rules/type_eq.rs` (新検査) と `stage0/src/types.rs` (旧パス) の
  `Binary`規則を、拒否リストから許可リストへ作り替えた: `strip()` した被演算子が
  整数プリム (または演算子ごとに `f32/f64`・`bool`) のときだけ通す。
  それ以外は E2020「operator `X` cannot be applied to `T`」。
  `str` / `string.String` のときは連結の代案を文面に足す
  (`alloc.string.String.join` / `push_str`)。
- panics 推論 (`effects_exact.rs`) は変更不要: str・名前つき型の二項演算は
  型検査で拒否されるため `panics never` と両立しなくなった。
  `[]` (E4003)・`/` `%` (E4004)・シフト (メソッドの宣言) は既に数えている。

## 試験

- `stage0/tests/data/unsound/` に 5 ケース追加 (すべて `expect: E2020`、fmt 正規形):
  `str_plus_str_panics_never`・`str_minus_str_panics_never`・
  `string_plus_string_panics_never`・`struct_plus_struct_panics_never`・
  `struct_rem_struct_panics_never`。
- 回帰: `cargo test --release --test check_diag` 314 件 ok、`--test rephrase` 3 件 ok、
  `cargo check --release --tests` ok、`spec/tools/check_all.py` ALL PASSED、
  `bench/verify_reference.py` bad 0、examples/apps/selfhost/ui/perf の `ksn check` 全パッケージ 0 診断。
