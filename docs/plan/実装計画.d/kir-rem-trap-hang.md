### 2.10?? 下ろしの繰り返しが止まらない (`panics never` の関数に trap 付きの `rem`)

- 報告: 実行器の KIR の `panics never` の関数に `rem.usize.trap` を入れると、コンパイラが止まらない。
- 原因: `kir_lower.rs` の `lower_defs` の下ろしの繰り返しは、検査 (`kir::verify_module`・`kir_suspend::transform` の失敗) の鍵が
  下ろす候補の関数 (`ok`) に当たったときだけ進む。変換で作った関数や集成体の型の失敗は `ok` に当たらず、同じ検査を永久に繰り返した。
- 直し: 判断を `settle_round` に出した。当たらない失敗があれば、残りの関数を理由つきで下ろして止める (`bad` に入る → 参照の処理系へ退避)。
- 利用者のプログラムからの経路: `panics never` の関数の `%` / `/` (trap) は検査 E4004 で先に拒まれる (`ksn check`)。KIR 側の `trap` は
  `verify_module` が「panics never の関数に trap 付きの算術」で拒む。どちらも止まらないことを確かめた。
- 試験: `kir_lower::settle_tests` (一致する失敗は進む / 一致しない失敗は止まる)。
