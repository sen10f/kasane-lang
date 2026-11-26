# wrapping_* 監査

`.ksnl` 中の `wrapping_*` 全 1029 件の分類 (`wrapping-audit.tsv`)。一次分類はスクリプトの文面照合 (直前 14 行の `while`/`if` の条件、関数名・ファイル名の hash/rng 等、`^ << >>`) で、ガードが 14 行より遠い・条件が式として複雑なものは (c) に落ちる。そのため (c) は上限値で、実際に誤値を生む件数はこれより少ない。(c) の上位は抜き取りで読んだ。

## 件数

| 場所 | a 守られている | b 意図的 | c 無防備 | d 範囲で有界 | 計 |
|---|---|---|---|---|---|
| selfhost | 101 | 25 | 188 | 86 | 400 |
| examples | 15 | 16 | 70 | 19 | 120 |
| ui | 34 | 3 | 40 | 18 | 95 |
| apps | 14 | 12 | 35 | 6 | 67 |
| spec/stdlib | 49 | 99 | 158 | 18 | 324 |
| bench/tasks | 8 | 5 | 6 | 4 | 23 |
| 合計 | 221 | 160 | 497 | 151 | 1029 |

## 気になる (c) 20 件

1. `selfhost/lower/src/lower/cx.ksnl:116` `out.push(lower.cx.get_val(c, *k).wrapping_sub(1usize))?` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
2. `selfhost/lower/src/lower/cx.ksnl:188` `let cur = lower.cx.get_val(c, *k).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
3. `selfhost/lower/src/lower/expr.ksnl:88` `let slot = lower.cx.get_val(c, loc - 1usize).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
4. `selfhost/lower/src/lower/expr.ksnl:609` `let sv = lower.cx.get_val(c, k).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
5. `selfhost/lower/src/lower/expr.ksnl:619` `let sv = lower.cx.get_val(c, k).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
6. `selfhost/lower/src/lower/expr.ksnl:741` `bv = lower.cx.get_val(c, loc - 1usize).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
7. `selfhost/lower/src/lower/expr.ksnl:806` `let bv = lower.cx.get_val(c, k).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
8. `selfhost/lower/src/lower/expr.ksnl:986` `let mut cur = lower.cx.get_val(c, k).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
9. `selfhost/lower/src/lower/expr.ksnl:2258` `slot = lower.cx.get_val(c, loc - 1usize).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
10. `selfhost/lower/src/lower/expr.ksnl:2435` `let mut cur = lower.cx.get_val(c, k).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
11. `selfhost/lower/src/lower/expr.ksnl:2560` `let bv = lower.cx.get_val(c, k).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
12. `selfhost/lower/src/lower/loops.ksnl:306` `let hv = lower.cx.get_val(c, loc - 1usize).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
13. `selfhost/lower/src/lower/loops.ksnl:374` `let hv = lower.cx.get_val(c, loc - 1usize).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
14. `selfhost/lower/src/lower/opt.ksnl:89` `let owner = types.infer.item_owner(env, item).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
15. `selfhost/lower/src/lower/opt.ksnl:298` `let v = lower.cx.get_val(c, k).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
16. `selfhost/lower/src/lower/opt.ksnl:653` `let owner = types.infer.item_owner(env, item).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
17. `selfhost/lower/src/lower/opt.ksnl:716` `recv = lower.cx.get_val(c, loc - 1usize).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
18. `selfhost/lower/src/lower/opt.ksnl:718` `let slot = lower.cx.get_val(c, loc - 1usize).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
19. `selfhost/lower/src/lower/std_calls.ksnl:350` `vs.push(lower.cx.get_val(c, loc - 1usize).wrapping_sub(1usize))?` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX
20. `selfhost/lower/src/lower/vec.ksnl:76` `let sv = lower.cx.get_val(c, k).wrapping_sub(1usize)` — handle stored +1 (0=none); sub(1) on 0 -> usize::MAX

上位は、ハンドルを +1 して 0 を「無し」にする表現 (`get_val(..).wrapping_sub(1usize)`) と、`len`・`hi` の `wrapping_sub(1)` で、前段の `len > 0` 等の保証が 14 行より遠いか、不変条件として暗黙。0 のときは usize::MAX になり、後続の `get`/添字は範囲外で Option/panic になるが、値としては静かに誤る。

## 自動で証明できそうな繰り返しの形

1. `while i < n { ...; i = i.wrapping_add(1) }` (i は本体で他に代入されない)。i < n ≤ MAX なので `i + 1` は溢れない。`for i in 0..n` に書き換えられる。
2. `if x > 0 { x.wrapping_sub(1) }` / `x >= k` のあとの `x - k`、`len > 5` のあとの `len - 5`。範囲の述語を流し、`-` を許す形 (区間解析) で足りる。
3. ハンドル +1 の表現 (`h - 1`) は `Option<Handle>` か NonZero 型を作れば、不変条件が型に出る。
4. `slice.len() - 1` は `is_empty` の分岐の後。`last()`/`split_last` に置き換えられる。
5. 単調な 32/64 ビット統計カウンタ (`hits + 1`) は `saturating_add` か `checked_add(..).ok_or(..)?` が正しい。wrapping は誤り。
6. (b) は `wrapping_mul` や hash 専用で、`Wrapping<T>` 型か `hash_*` 専用演算にすれば、意図が名前に出て (c) と区別できる。
