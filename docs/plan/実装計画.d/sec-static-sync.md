# check/sec-static-sync
- M1: `static` の Sync は `types.rs` の `auto_marker` (spawn の E2040 と同じ計算) で `check_def` が見る。`Option`/`Result` は引数ごと、`Mutex<T>`・`RwLock<T>` は std の impl どおり。旧 `sync_v` の呼びは外した。
- K3: `lifetimes.rs` の `call` の受け手が `Callee::Methods` などのとき `use_var` を呼んでいなかった (`r.len()` が使用と数えられず、文末で尽きた一時値の借用の E3010 が出ない)。一時値だけでなく経路形の受け手すべてに効く。
- 試験: `unsound/static_sync_*`・`unsound/temp_*_len`、`check_ok/static_sync_ok`。
