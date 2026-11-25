# f64-nbody

原因: Vec への参照の引数は KIR 上 ptr で、noalias が付かず、f64 の書き込みのたびに Vec のヘッダ (ptr/len) を読み直し境界検査も残っていた。
直し: 下ろしが `&Vec`/`&mut Vec` の引数に印を付け (`kir::VEC_REFS`)、LLVM で noalias (+readonly) を付ける。nbody 2.17s → 1.65s (負荷高で測定、Rust 0.50)。残りは f64 の書き込みごとの NaN の正規化。fcmp uno 版は遅く (1.78s) 採用せず。
