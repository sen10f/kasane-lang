# std/btree-real: BtreeMap を O(log n) にする (P-13)

- `alloc.collections.BtreeMap` を、アリーナ (`Buf<BtreeNode>`) の上の treap に替えた (添字の親・子・前後のリンク、ループだけで再帰なし)。API・順序・`range` の半開区間は同じ。`remove` は `swap_remove` で詰める。
- 優先度は挿入ごとの xorshift (キーと独立。キーの生成と同じ数列でも崩れない)。
- 計測 (20 万のランダムな u64、native release、min of 3): 挿入 189 ms・検索 167・反復 19・削除 180 (Rust BTreeMap: 59・62・3・69)。以前は挿入だけで推定 14 s。
- 試験: `stage0/tests/btreemap_treap.rs` (整列した Vec との乱数の突き合わせ。VM・木走査・ネイティブ)。
