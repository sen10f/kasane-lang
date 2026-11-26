# vm/unify-plan

方針と順序は `設計_VM一本化.md`。M0 完了、次は M1 (デバッガを VM に)。

## M1 (完了)
- `ksn debug` / `ksn test --debug` / `ksn dap` の既定を VM にした (`--engine=tree` で木走査版)。実装は `stage0/src/vm/run/dbg.rs`、入口は `vm::debug`。
- 仕組み: `kir_lower::set_debug_vars` が局所変数の履歴 (`FnLines::evs`) を記録、`compile::set_debug` が行ごとに `Ins::Line` を出し畳み込みを止める。VM は `dbg` が付いているときだけ `Line` で止まり判定する。
- 手書きの Drop はデバッグできる。E9031 は廃止 (explain・README を整理)。木走査のデバッガは Drop を拒むが、コードなしの文言。
- 既知の差 (木走査版との): 式の評価は演算・欄・純粋な関数の呼び出しだけ (メソッド呼び出しは未対応)、Vec / String の中身は `<vec>` 表示、`unsafe` ブロックのあるパッケージはデバッグ不可 (M3 以降で VM の退避を塞ぐ)、スレッドは単一 (`threads` は main だけ)。

## M2 (完了)
- `ksn run --watch` を既定の VM で走らせる (`--engine=vm` の明示が要らない)。`cli.rs` の「watch は VM 明示のときだけ」の条件を外した。試験 `vm_reload.rs::hot_reload_runs_on_the_vm_by_default`。
- 残り: プログラムが VM に下ろせない形 (例: `std.thread.sleep`) を含むと、これまでどおり木走査へ退避する (退避の理由を塞ぐのは M3 以降)。

## M3 (進行中)
`engine_compare.py` (V=1 で退避の名前つき) の最初の集計 (linear_ok 189 / check_ok 66 / data 29 / examples 19 / perf 8 / bench): 退避 37 件。
- `block_on_threads` 7: VM に実行器 1 本だけの協調実行を試すと、取り消し (自分への cancel の拒否)・入れ子・panic の伝わり・`spawn_join_loop` で木走査と食い違う (止まる)。設計が要る (別の束)。
- 配列の要素・長さ (所有物を持つ要素、リテラルでない長さ) 5、クロージャ・関数値 (戻り値・引数・変数への代入) 6、`clone` 3、`Box::into_dyn` 2、`std.net` の追加分 2、構造体を値で渡す extern 2、そのほか 1 件ずつ 10 件。
- 閉じた: `std.net` の追加分 (`NetSys` の全種類: タイムアウト・期限・shutdown・アドレス・resolve・UDP・read_try / write_try) を VM に足した (`vm/run/netsys.rs`)。`net_more` が VM で走る。
- 退避にした: `std.net` の `*_async` は VM で止まる (実行器にリアクターが無い。`net_async` が 256 クライアントで止まった) ので、VM が理由つきで退避する。
