### 2.10?? `ksn fix` の Rust 風の表面の書き換え (3 回目)

- 足した書き換え (`stage0/src/surface_fix.rs`): 定義の波括弧 `fn f(..) -> T {` … `}` / `type X {` / `impl T for Y {` (メソッドの `{ }` も) を `end <名前>` の形に (E0043)、単独の `end` と `impl` 内の字下げした `end` / `end impl ..`、`type N: f: T, g: U` / `enum N: A, B` で `end` が無い形に `end N` を足す、`trait N[..]: fn a; fn b` を 1 行 1 メソッドに、`derive []` を trait でも消す、`impl` の中の FQN 名のメソッド、`if c { a } else { b }` (else if の連なりも) の行分け、代入文の末尾の `,`、型の欄の `<A, B>` 内のコンマで割らない。
- `rustisms.rs` の E0053: `} else if X.Y {` の行を構造体リテラルの折り返しと誤検出していたのを直した (行頭が `}` のときは見ない)。
- 直さない形 (一意に決まらない / 意味が変わる): パターンの `mut` (`Some(mut x)`)、`&s[..w]` の範囲の添字、`Some(t) if guard =>` の match のガード、`for i in 0..n` の範囲、`as` (E0042)、`Vec` の短い名前 (E0052)。
- 計測 (api_bench の 2 ディレクトリ 34 本、最初の診断が E00xx の本数): 23 → 15。
