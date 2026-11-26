# o01: 関数値の型の領域 (O-01)

関数型 (`fn(..)` 型・`F: Fn<..>` の境界・構造体の欄・引数) の適合で、署名の参照の領域を比べる。
`stage0/src/tast_rules/type_eq.rs` の `fn_regions_ok`: 受け側の領域は不透明な定数、渡す側の `'a`/省略領域は量化変数。
引数 (反変) で変数を受け側の領域に束縛し、`&mut` の中は不変、戻り値 (共変) は束縛した領域か `'static` で足りるかを見る。
`unify` の Closure 同士と `fn_bound` から呼ぶ。`subst` が領域を落としていたのも直した。診断は E2023。
再現は `unsound/o01_*`。`sec6_fnvalue_genericvar_borrow` は別の穴 (suspend をまたぐ借用) で残る。
