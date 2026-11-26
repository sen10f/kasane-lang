# fix/interp-allocerror

- 症状: check_ok/question_ok_fs_read_utf8_with が ksn run で AllocError (ネイティブは正常)。
- 原因: インタプリタの std.fs.read_to_end が Vec の要素だけ伸ばし、buf.capacity を更新しない。次の push が push_within_cap 失敗 → grow(cap*2+4) が長さに足りず AllocError。
- 直し: natives.rs の read_to_end で長さが容量を超えたら capacity を長さに合わせる。
- 試験: stage0/tests/interp_fs_with_alloc.rs (インタプリタとネイティブの出力一致)。

- 2026-11-25 掃除: 組み込みが Vec<u8> へ書く経路 (read_dir_raw・process の output) も extend_vec_bytes を通す。debug_check_vec_cap (デバッグ版) で不変条件を検査。試験 builtin_vec_writes_then_push_interp_and_native_agree。
