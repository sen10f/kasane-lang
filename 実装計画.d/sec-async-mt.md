# check/sec-async-mt (hunt4/mt の穴)

- M1: ネイティブの `join_task` が複数スレッドの印つき id では世代を見なかった。世代の検査を先にし、`spawn_any` が現在の世代を取っ手に入れる。コンパイル時に `Task<T>` が `block_on` の外へ出ることを拒む案は、`Task` が `'static` な値で型に実行器の寿命を持てない (E3060 の設計、案 A を保つ) ため見送り。実行時の世代の検査 (木走査と同じ文面) で閉じる。
- M2: 先に評価した被演算子 (引数・タプル・配列の要素) を `$snd` の一時値として、後の中断点をまたぐ `Send` を見る (E2074)。
- M3: `ksn.task.mt_begin` で `mt.on` なら `already running` で panic (入れ子)。
- M5: `spawn_local` は `block_on_threads` (n > 1) の中で panic (木走査・ネイティブ)。
- M4 (既知の制限): 木走査の実行器は、別の OS スレッドが後から起こす `accept_async` を待てずに 20 秒以上止まる (`block_on` も同じ)。ネイティブは正しい。木走査の fd 待ちの作り直しが要るので、ここでは直さず制限として残す。
