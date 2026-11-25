# 実装計画.d/strplus.md (check/m4-str-plus-panics)

### 2.10?? 文字列・名前付き型への算術演算子を型検査で拒む (E2020)

穴 (mythos4-ux 2.3, `h01_str_plus_str_passes_check`): `"a" + name` (`&str + &str`) は
`ksn check` を `panics never` 付きで通っていたが、`ksn run` は `panic: bad operands for +`
(終了 101)、`ksn build --release` は E9040。同じ穴が `&str - &str`・`String + String`・
ユーザー構造体の `a + b` / `a % b` にもあった。

原因: 仕様 4.1 は `+ - * /` を整数と浮動小数だけ、`%` を整数だけ、`& | ^` を整数と
`bool` だけと定めるが、検査 (`tast_rules/type_eq.rs` と旧道の `types.rs`) は
prim 型の拒否リスト (`bool` / `char` など) しか持たず、`str` と名前付き型
(`Ty::Named` 全般) が素通りしていた。実行系 (interp.rs) は整数・浮動小数・bool 以外の
被演算子で `bad operands` panic するだけなので、検査をすり抜けると実行時 panic になる。

直し: 両道の `Binary` 規則を許可リストに変えた。`+ - * /` は整数 prim と `f32`/`f64`、
`%` は整数 prim、`& | ^` は整数 prim と `bool` だけを通し、それ以外は E2020
("operator `+` cannot be applied to `str`")。`str` / `string.String` のときは
代わりを suggestion に付ける (「`alloc.string.String.join` または `String` への
`push_str`」)。panics 推論側は変更不要 (`[]` は E4003、`/ %` は E4004 で既に数えている。
str への算術は型の誤りで止まるため `panics never` と両立しないコードはそもそも通らない)。

試験: `stage0/tests/data/unsound/` に 5 ケース追加
(`str_plus_str_panics_never`・`string_plus_string_panics_never`・
`str_minus_str_panics_never`・`struct_plus_struct_panics_never`・
`struct_rem_struct_panics_never`、いずれも `expect: E2020`)。
回帰: `cargo test --release --test check_diag` 314 件・`--test rephrase` 3 件合格、
`cargo check --release --tests` 合格、`spec/tools/check_all.py` ALL CHECKS PASSED、
`bench/verify_reference.py` bad 0、examples/apps/selfhost/ui/perf の `ksn check` 全パッケージ診断 0。
