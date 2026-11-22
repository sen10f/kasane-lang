# sec2-closure-store (M7): 閉包の取り込み間の借用の保存

- 根: 取り込み `&A` / `&mut B` の領域は外側の場所の領域。本体で A から得た借用を B の先へ書く形は、外側で `B = ..A の借用..` と書くのと同じに扱う。
- `lifetimes.rs`: 本体の `unrelated_store` が (取り込み A → 取り込み B) の保存を `cap_stores` に記録し、`closure_body` の終わりで外側の B の値へ A の借用を足す。A が B より内側のスコープなら E3030。A が尽きた後に B を使えば E3010。
- `borrows.rs` `closure`: `&mut` で取り込んだ B が借用を持てる型なら、他の参照の取り込み A の貸し出しを B が持つとして扱う (M7b/M7c)。
- 言い換え: `mem.replace`・ヘルパー関数 (`['a]` つき) は止まる。`Vec<&str>` の `push` は E3060、タプルの `.0` は文法にないので型や文法の時点で止まる。
- テスト: `unsound/sec2_closure_*` 5 本、`linear_ok/sec2_closure_two_captures_owned_store`。
- 過剰側: 本体が書かなくても、`&mut` で取り込んだ借用を持てる B は他の取り込みの借用を持つと見る (borrows.rs)。
