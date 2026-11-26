# vm/unify-plan

方針と順序は `設計_VM一本化.md`。M0 完了、次は M1 (デバッガを VM に)。

## M1 (完了)
- `ksn debug` / `ksn test --debug` / `ksn dap` の既定を VM にした (`--engine=tree` で木走査版)。実装は `stage0/src/vm/run/dbg.rs`、入口は `vm::debug`。
- 仕組み: `kir_lower::set_debug_vars` が局所変数の履歴 (`FnLines::evs`) を記録、`compile::set_debug` が行ごとに `Ins::Line` を出し畳み込みを止める。VM は `dbg` が付いているときだけ `Line` で止まり判定する。
- 手書きの Drop はデバッグできる。E9031 は廃止 (explain・README を整理)。木走査のデバッガは Drop を拒むが、コードなしの文言。
- 既知の差 (木走査版との): 式の評価は演算・欄・純粋な関数の呼び出しだけ (メソッド呼び出しは未対応)、Vec / String の中身は `<vec>` 表示、`unsafe` ブロックのあるパッケージはデバッグ不可 (M3 以降で VM の退避を塞ぐ)、スレッドは単一 (`threads` は main だけ)。
