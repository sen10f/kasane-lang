# HashMap 性能 (kir/hashmap-perf)

決定 (2026-11-24): 中の作りを詰めるだけ。挿入順を保つ。Fnv64 は変えない。

## 変更
1. remove は墓石 (`cells[p] = Empty`、`dead` を数える)。以前は `Vec.remove` + 全再索引で 1 回 O(n)。
   索引の `Index(p, h)` は残すので探索の列は切れない。挿入で索引が半分を超えたら `compact` (前詰め、順序は保つ) してから、足りなければ倍に。
2. 索引の項に鍵のハッシュも持たせた (`Index(pos, h)`)。ハッシュが違う項では実体の項を読まない (miss が主に速くなる)。型は `dead: usize` の欄と `Index(usize, usize)` が増えた。
3. `for i in range(..)` の局所を `bind_let` で束縛 (`kir_lower_coll.rs`)。`&i` が下ろせるようになった (`get(&i)` / `remove(&i)`)。`let k = i; get(&k)` は元から下りた。

## 測定 (u64 1M、ms、3 回、負荷 19〜40 で雑音大。Rust は `-C opt-level=3`)
| 操作 | 変更前 | 変更後 | Rust |
|---|---|---|---|
| insert | 285 | 210 | 185 |
| get 命中 | 120 | 60 | 145 |
| get 外れ | 175 | 97 | 48 |
| iter | 3 | 3 | 4 |
| remove 半分 | 65 (n=5000 では O(n^2)) | 52 | 71 |
(変更前は 変更 1・2 の前、3 だけ入れた状態。insert+iter の perfcg は 0.26 → 約 0.22 s 対 Rust 0.16〜0.20。)
- 常駐メモリは変わらず (98 MB 対 Rust 54 MB)。索引は同じ列の 40 バイト級の項のまま。

## 未達と理由
- u32 の索引表への分離 (メモリ 1/2 以下): 索引用に 2 つ目の `Buf<u32, A>` が要るが、`Allocator` は複製できず (`A` の値は `cells` が持つ)、`Clone` 境界を足すと型の公開の API が変わる。生ポインタで `cells` の allocator を借りる案は unsafe を標準の外へ広げる。決定が要る。
- get 外れは Rust の約 2 倍: Fnv64 の下位 32 ビットを使う線形探索の群れ (ハッシュを変えない決定)。
- 呼び出しの inline (`kasane_base_inst` に HashMap の関数を足す) は効果なし (0.27 → 0.29 s)。足していない。
