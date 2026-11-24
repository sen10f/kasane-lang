# rt/static-mutex-struct

## 再現 (着手前)
`static app.STORE: std.sync.Mutex<app.Store> = std.sync.Mutex.new(app.mk())` (`Store` は `Vec` / `String` / `HashMap` を持つ構造体、`app.mk` は引数なしの `const fn`) は、検査・木走査は通り、ネイティブだけ E9040 (KIR 下ろし `static_lock`: スカラーだけ・初期値はリテラルだけ)。書き方は既存の仕様のまま (`const fn` で組んで呼ぶ。仕様 8.3)。新しい構文はなし。

## 実装
- `kir_lower_sync.rs` `static_lock`: 集成体の値、初期値 = 引数なし const fn の呼び出し (`call_def`)。値の場所 `+80`。`read` / `write` (RwLock) を同じ関数で下ろす。
- `kir_lower_coll.rs`: ガードの印を 2 バイトにし 2 バイト目を種類にして、外すとき Mutex は `munlock`、RwLock は `rwunlock`。ロック/アンロックの算法は触らない。
- RwLock のネイティブ: `pthread_rwlock_*` (`kir_llvm.rs` の `thread_override`)。
- 木走査: `natives.rs` (`RwLock.new`・`read`・`write`)。KIR 実行器: 協調なので rw 関数は無操作、終了時の漏れ検査は大域から辿れる領域を除く。
