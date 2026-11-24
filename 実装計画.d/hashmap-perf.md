# HashMap 性能 (kir/hashmap-perf)

結論: 今回は処理系を変えていない (未達)。測定と原因の整理だけ。

## 測定 (`/tmp/perfcg_hashmap`、u64 1M insert + iter の和、5〜7 回の中央値、負荷 12〜25 で雑音大)
- 変更前 Kasane 0.26〜0.27 s / Rust (`-C opt-level=3`) 0.16〜0.20 s = 約 1.3〜1.6 倍。
- 試し: `kasane_base_inst` (`kir_llvm.rs`) に `core.hash.fnv_int`・`hash_key`・`HashMap.find_hashed`・`place`・`HashMapIter.next` を足して alwaysinline + 深さ記録省略 → 0.29 s。改善なし (戻した)。
  呼び出しの壁が主因ではない。
- get/miss/remove の測定、perf は未実施 (perf は入っていない)。

## 原因の見立て (未検証の部分あり)
- HashMap は `spec/stdlib/alloc/collections.ksnl` の Kasane 実装。`cells: Vec<HashCell>` (挿入順の本体、`Entry(h,K,V)` は 40 バイト級) + `Index(p)` の open addressing。
  挿入順はこの cells の並びで保つ。1 要素あたりのメモリが Rust (16 バイト + 制御バイト) の 2〜3 倍で、メモリ帯域・キャッシュが支配的。
- ハッシュは FNV-1a のバイトごとの 8 回の乗算 (`fnv_int`)。仕様 4.2 が Fnv64 を定めるので、速い整数ハッシュへ替えるのは仕様変更 (決定が要る)。
- `get(&k)` で `&local` が下ろせない件は `kir_lower_refs.rs` の段 1 の制限で未着手。

## 次の手 (案)
1. `Entry` から `h` を外す/cells と index を分ける (構造の変更。挿入順を保つ)。
2. 整数キーだけ 8 バイトを 1 回の乗算で混ぜる経路 (仕様 4.2 の改訂が要る)。
