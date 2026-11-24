# check/sec4-closure-params

- K1-K3: 閉包の引数どうしの保存 (`*b = Some(a)`) を `fn` と同じ省略規則 (参照ごとに別の領域) で E3010 にする。`lifetimes.rs` の `unrelated_store` の閉包の枝から、引数どうしは `fn` の共通の判定へ落とす。
- S1: 型パラメータの受け手 `x.m(..)` / `T` の経路形で、境界のトレイトの署名 (`lifetimes::bound_sig_of`) を `borrows.rs` の `call` の `dparams` に使う。
- S2: 局所の閉包の呼び出しの瞬間にも閉包を使う (`Ev::Use`) ので、取り込んだ共有借用が `&mut` の引数と重なれば E3010。
- 試験: `unsound/c4_*`、`check_ok/c4_*`。
