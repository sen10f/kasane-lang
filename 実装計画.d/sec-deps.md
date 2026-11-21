### 2.10?? 供給網の穴: 依存の本体の検査・std 名前の予約・lock の格下げ (S1-S4)

- **S1 依存の本体が検査されなかった**: `build_program_with` は依存の定義を `Program` に入れても、検査の対象 (`user`) は根の定義だけだった。いまは根と依存の全定義を `Checker` と `tast_check` に渡す (`full_check_loaded` の `checked`)。依存のソースにも字句・整形 (E0020)・lint・文法の検査を掛ける。`ksn check` / `build` / `run` / `test` が同じ経路を通る。`const` / `static` の評価と KIR・文書・テストの対象は根のままにした。
- **パッケージごとの `[unsafe] modules`**: `Checker.unsafe_modules` を (パッケージの `src/`・`tests/` ディレクトリ, モジュール) の組にし、許可はそのパッケージのファイルの定義にだけ効く。依存が根や他のパッケージのモジュールへ許可を与えることはできない (`kasane.toml` の検証 E9012 とあわせて)。
- **S2/S3 標準ライブラリの名前**: 利用者のパッケージ (根・依存とも) が `core` / `alloc` / `std` を先頭にもつ定義・モジュールを置くと E1001。`module_may_use_unsafe` の暗黙の許可は標準ライブラリのソースだけに残る (利用者はその名前を持てないため、`src/std/…` で unsafe を得られない)。パッケージ名と最上位モジュールの一致は求めない (`ui/kasane/*` は名前が `ui_event` でモジュールが `ui.event`)。
- **S4 lock の格下げ**: git・レジストリの依存があるのに lock が `version = 1` (api の記録なし) なら E9021 (`verify_lock`)。直し方は `ksn update` (版 3 を書く)。path 依存だけの lock は版 1 / 2 のままでよい。
- **S1c は未対応**: 依存の `.ksnh` と `.ksnl` の食い違い (E9050)。既存の決定 (`check_diag::ksnh_in_a_dependency_is_not_read`: 依存は `.ksnl` だけを読み、ハッシュも `ksn vendor` も `.ksnl` だけ) と衝突するため入れなかった。入れるなら、その決定を変える (`.ksnh` をハッシュと vendor に入れる) 判断が要る。
- **試験の入力の直し** (依存の本体が検査されるようになったため。担当外の期待は変えていない): `deps_resolve.rs` の依存の置き場 `src/lib.ksnl` → `src/<名前>.ksnl`、`lock_v2.rs` / `lock_v3.rs` の依存 `gl` の `gl.pi` などの宣言を本体と合う形にして `ksn fmt` を通す、`toml_strict.rs` の `lib.hello` を完全な形に。版 1 の lock を読む 3 本は、git 依存では E9021 で止まることの確認に変えた。
- 試験: `stage0/tests/sec_deps.rs` (S1 の unsafe / 効果 / panic・依存自身の `[unsafe] modules` が通る・他パッケージへ許可を与えられない・S2・S3・S3b・S4)。
