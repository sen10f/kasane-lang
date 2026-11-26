# rt/chan-wakeups (P-05 / P-06)
- `ksn.chan.rt.wait` / `waitsend` (kir_llvm.rs): 眠る前にロックを外して 400 回回り、head/len/senders/receivers が変わったら起きずに戻る (呼び出し側は再検査のループ)。ロックを取り直した後にもう一度確かめて起こしの取りこぼしを避ける。TSan のときは回転を飛ばす (ロックなしの読みが普通の書き込みと競合するため)。
- P-06: 待ちの期限を 100ms から倍々 (上限 1.6s) に。全員待ちの検出は期限切れのたびのまま (検出は最大 1.6s 遅れる)。
- 測定 (chanb 1:1 500万件、混んだ機械): CPU 約 6.7〜14.9s → 約 2.1s。
- 追記: 回転が読む欄 (head/len/senders/receivers) の書き込みは `ksn.chan.rt.st{8,16,32,40}` (原子的な release の store)、回転の読みは acquire。TSan でも回転を飛ばさない。回転は 60 回に縮めた。
- 再測 (負荷 25〜30、CPU = user+sys、3 回): 1:1 5M 容量 64 は dev/plan の ksn と同等 (約 1.5〜2 s)、Rust sync_channel 約 1.1〜1.4 s。回転は 1:1 の CPU を減らさない (最初の「3 倍減」は負荷の違う比較で誤り)。待機 300 本 10 s の CPU: 1.10 s → 0.56 s。
