# release-inline: 小さい非再帰関数の展開

- 確認: `ksn build --release` は `kir_opt::optimize` を通さない (`--opt` は `ksn kir` だけ)。ただし主因は `inline_small` ではなく、全関数に付く深さの入口の検査 (`ksn.depth` の load/store) で、LLVM が展開できなかったこと。`optimize` の配線は不要になったので足していない。
- 実装 (`kir_llvm.rs` の `pure_region`): 呼び出しの輪に入らず、輪の関数・`extern`・`fnptr`・入口・公開 wasm 関数も使わない関数 (純粋域) は、入口の検査と加算を外す。呼び出す側 (純粋域でない関数) が `@ksn.chk(depth+1, 鎖の高さ-1)` を先に呼んで、深さの超過 (102) が出る深さをインタプリタと同じにする (鎖の最深の入口が `dmax` 以上になる場合)。`@ksn.chk` は alwaysinline。
- 既知の差: 超過が純粋域の鎖の途中で起きる場合、その鎖の副作用は出ない (先に検査するため)。スタックの底 (`ksn.floor`) の検査は純粋域の中では行わない (底から 1 MiB の余裕と鎖の高さは小さい)。
- 試験: `stage0/tests/kir_inline.rs` (純粋域に深さの記録がない・`-O2` 後に連鎖への呼び出しが残らない)。
- 測定 (負荷の高い機械、1 回。旧 → 新): callchain 457 → 19 ms、loops 15 → 7、hashmap 308 → 284、bytescan 423 → 429、soa 194 → 210、strbuild 165 → 168、vecsum 142 → 198 (ほか負荷の揺れ)。出力はすべて一致。
