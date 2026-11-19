//! build.rs が作る埋め込みの資産 (L2): 実行時の C のビットコード (`rt_bc`) と標準ライブラリの宣言 (`STDLIB`)。
include!(concat!(env!("OUT_DIR"), "/assets.rs"));
