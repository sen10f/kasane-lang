### 2.10?? `ksn check --signatures-only` (インターフェース先行の検査)

- 本体の無い `fn` の形: ヘッダの直後に `end <FQN>` が来る (本体の行が 0 の定義)。`program.rs` の `Def.header_only` が立つ。
- `ksn check --signatures-only`: その定義の本体を `None` にして、`.sig` と同じ扱い (シグネチャだけ検査)。`check.rs` の `run` で本体なしの `fn` はシグネチャの型と境界 (`check_header_types` / `check_generic_bounds`) だけを見る。本体に依る検査 (E2020・E1020・E4025・借用) は飛ばす。E1020 は `cli.rs` で本体のない行だけ落とす。
- フラグの無い `ksn check`: 従来どおり (戻り値が `()` でない本体の無い関数は E2020 など。`-> ()` の空の関数は正しい無動作の関数なので通す。既存の試験の入力 `check_ok/fn_eff_values_ok` などがこれに頼る)。
- `ksn build` / `run` / `test` (`ksn check` 以外の入口): 本体の無い `fn` は E0067 で止める。
- 試験: `stage0/tests/signatures_only.rs` (2 モジュールが通る・フラグの無いのと build/run は失敗・署名の型の誤り・本体の側からの未定義の呼び出し)。
- 残り: 依存パッケージの本体なし定義は E0067 (フラグは根のパッケージだけ)。`.ksnh` を持つ根のパッケージは未対応 (本体なしの行を `.ksnl` から取るため)。
- 標準ライブラリの cache の形式を `KSNSTD04` に上げた (`Def.header_only` を書く)。
