# chan-bounded: channel の capacity を効かせる

## 1. 影響調査 (結果)
channel を使う全プログラムを読んだ (examples/threads, examples/shortener, apps/todo, stage0/tests/data/{threads,rt_sync,rt_static_shared,linear_ok,unsound})。
selfhost/ ui/ perf/ bench/tasks には channel の使用なし。
上限つきの send で詰まるものは無い:
- 生産者スレッドと消費者 (main) が別: threads.ksnl(4)、channel.ksnl(2,3)、channel_clone(4)、channel_rounds(4)、channel_stress(64)、channel_many(3)、pool_shared(2、main が 30 送るがワーカーが受ける)
- 同じスレッドで capacity 以内: channel_clone の left (8 に 3)、channel_rounds の round (8 に 5)、linear の left (2 に 1)
- 容量 1 の持ち回し (give/take、state): shortener/todo pool、kir_match_push_send_ok、linear の state、ping_pong。送る前に必ず受けて空いている
- deadlock_*・rw_blocked は send せず recv で詰まるだけ
- unsound/・flow_default_ok は check のみ。実行しない
=> examples/perf/reference の書き換えは不要。tests/data の capacity 変更も不要。

## 2. 実装 (進行)
実装済み (WIP): interp (natives.rs / DEADLOCK_SEND)、KIR 下ろし (kir_lower_chan.rs: LIMIT/SWAIT、GROW で詰め直し)、LLVM (waitsend / unlockrecv)。
ネイティブは通る (stress・ping_pong・16 送り手)。協調実行の interp / KIR 実行器は退行:
入れ子で最後まで走るスレッドは満杯の send で main に戻れないので、`examples/threads` (cap 4 に 100 件) が `ksn run` で
deadlock 101 になる。ほか threads の channel / channel_clone / linear / pool_shared / kir_* 約 10 本。
=> 指示どおり、examples が壊れるので止めて報告。続けるには interp のスレッドを実コルーチン化する (別の大工事) か、
   協調実行側だけ満杯でも伸ばす (仕様の食い違い) かの決定が要る。
