### 2.10?? 診断に修正の案内を付ける (E2036 / E2021)

設計原則 4（診断はどう直すかを示す）に従い、LLM のベンチ回答がヒントなしで止まっていた 2 つの診断に修正案内を付ける。

- E2036 `derive [X] on T: the field type U does not implement Tr`:
  - U がユーザー定義の型なら、`— add `derive [X]` to `U`` を付ける（例: `add `derive [Clone]` to `bench.Mode``）。
  - U が `String` / `Vec` / `HashMap` など確保を行う標準ライブラリの型で `X == Clone` なら、`— remove `Clone` from the derive list` を付ける。`try_clone`（確保が失敗しうる、`Result<Self, AllocError>`）を spec/stdlib/alloc/string.sig と spec/stdlib/alloc/vec.sig で確認済みなので、`String` / `Vec` には `; copy with `try_clone` (fallible) instead` を、`HashMap` には複製 API が無いので名前を出さない。
  - コード (E2036) は変えず、`ksn explain E2036` を更新。
- E2021 `_` / 束縛パターンで列挙型を照合:
  - 列挙型が特定できたら、全バリアント名を列挙する（例: `bench.Tok.Num(..)`, `bench.Tok.Plus`）。
  - コード (E2021) は変えず、`ksn explain E2021` を更新。
- 試験: `stage0/tests/check_diag.rs` にヒント文言をアサートする試験を 4 本追加（E2036 のユーザー型 / String-Vec / HashMap、E2021 のバリアント列挙）。
- `examples/`・`perf/`・`bench/tasks/*/reference.ksnl` は変更しない。`rephrase` 台帳はこの変更で差分なし（`REPHRASE_UPDATE=1` 未使用）。
- ゲート: `cargo test --release --test check_diag --test rephrase --test tooling` と `cargo check --release --tests` は全件通過。
