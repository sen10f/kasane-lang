# release-inline: 小さい非再帰関数の展開

- 確認: `ksn build --release` は `kir_opt::optimize` を通さない (`--opt` は `ksn kir` だけ)。ただし主因は `inline_small` ではなく、全関数に付く深さの入口の検査 (`ksn.depth` の load/store) で、LLVM が展開できなかったこと。`optimize` の配線は不要になったので足していない。
- 実装 (`kir_llvm.rs` の `pure_region`): 呼び出しの輪に入らず、輪の関数・`extern`・`fnptr`・入口・公開 wasm 関数も使わない関数 (純粋域) は 2 本出す。深さの記録なしの本体 `k.g` (展開される) と、元の入口の検査つきの複製 `k.g.slow`。純粋域でない呼び出し側は `k.g.w` (alwaysinline) を呼び、`.w` が残りの深さ (`depth + 鎖の高さ < dmax`) とスタックの底 (`ksn.floor` + 高さ × 512 B) を見て、足りれば速い本体、足りなければ `.slow` を呼ぶ。`.slow` の中の呼び出しは `.slow` どうし。超過は元と同じ点・同じ副作用のあとで 102 になる。
- 試験: `stage0/tests/kir_inline_depth.rs` (再帰が上限の手前まで届いたあとで、表示する 5 段の鎖を呼ぶ。木走査・VM・ネイティブ debug/release で表示と終了コードが一致)、`stage0/tests/kir_inline.rs` (純粋域に深さの記録がない・`-O2` 後に連鎖への呼び出しが残らない)。
- 再測定 (3 回ずつ): callchain 193 → 2 ms、vecsum 121 → 121 ms (差は雑音)。
- 最初の測定 (負荷の高い機械、1 回。旧 → 新): callchain 457 → 19 ms、loops 15 → 7、hashmap 308 → 284、bytescan 423 → 429、soa 194 → 210、strbuild 165 → 168、vecsum 142 → 198 (ほか負荷の揺れ)。出力はすべて一致。
