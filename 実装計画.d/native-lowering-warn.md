### 2.10?? ネイティブに下ろせない形の警告 (W9040) と E9040 の一覧の雑音

- 背景: `std.thread.sleep`・`std.time.Instant.now`・`core.ptr.add_const` などネイティブに下ろせない形は、`ksn check` と `ksn test --both` を通った後の `ksn build --release` で初めて E9040 になっていた (Mythos max の B9)。
- 直し方: `ksn check` は、`kir_lower::lower_user` が出す `unsupported` の理由をそのまま使い、利用者の関数ごとに非致命の警告 `W9040` を出す (`cli.rs` の `native_lowering_warnings`)。別の一覧は持たない。「呼び先が未対応」だけの関数は原因の関数の所で出すので出さない。致命の診断があるときは出さない。
- E9040 の一覧の雑音: 下ろせなかった `extern` (`core.panic.rt_panic` など) は、呼ぶ関数が下ろせないときの理由としてだけ使う (`ext_bad`)。どの利用者の関数からも呼ばれない宣言は一覧に出ない。
- 注意: `W9040` は非致命なので `tests/data/diag/` (終了コード 1 を要求する) には置かない。試験は `stage0/tests/native_lowering_warn.rs`。`explain` に `W9040` を載せた。
- 残り: 1 つの関数の下ろしは最初に失敗した所で止まるので、1 関数に 1 件の理由 (行つき)。
