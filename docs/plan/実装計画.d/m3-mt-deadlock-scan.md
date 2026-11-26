# m3-mt-deadlock-scan (Mythos3 R1 / P1)

- R1: `ksn_rt_sys_mt_sleep` の `idle` は、世代が進む前に数えた眠りと後に数えた眠りを混ぜていた (bump が `idle` を 0 に戻す前に別の者が数えると、古い眠りが残って「全員待ち」と誤る)。`idle_ep` (数えた世代) を持ち、世代が違えば数え直す。bump は `idle` に触らない。
- P1: join 待ちを待たれる枠 (`+48` = 待つ者の番号 + 1) に持ち、完了で全表を走査しない (`wake_waiters`)。根 (0) の待ちだけ別に見る。`join_task` が結果を読んだ後 `ksn.task.release` で枠を空き連結 (`g[104]`) へ戻す (`Task` は move で 1 回しか join できないので古いハンドルは作れない。別の `block_on` の古いハンドルは従来どおり世代 `g[48]` で拒む)。mt のワーカーは `finish` 後に自分の枠を `release_local` で戻し、結果の枠は join 側が解放する。
- 枠は 56 バイト、`ksn.task.g` は 112 バイト。
