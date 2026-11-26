# vm/lane-mt-executor

方針は `設計_VM一本化.md` 6 章 (案 C: 協調・1 本のスレッド、核を Rust に移植)。

## M-A/B (完了)
- `stage0/src/vm/run/rtsys.rs`: `ksn_rt_sys_mt_*` (注入列・state・cancel の印・epoch・sleep・stop・session) と `ksn_rt_sys_io_*` (epoll の端起こし・期限・fq) を VM に移植。`Ins::Rt` (`bc.rs` の `Rt::of` が `ksn.mt.*` / `ksn.io.*` / `ksn.task.session` の名前を引く)。
- 退避を外した: `block_on_threads` と `*_async` を VM で走らせる (`vm/mod.rs`)。`listen` の待ち行列を 1024 に (256 本の同時接続)。`NetClose` は `io.forget`。
- 通った試験: `net_async`・`net_async_mt` (7 本の退避プログラムを含む)・`suspend`・`suspend_native`・`threads`・`task_cancel_heap`・`vm_parity`。`engine_compare.py`: block_on_threads / net_async の skip 0。
- 既知の差: 本物の並列はしない (ワーカー 1 本)。ワーカーが複数いることに頼る同期のチャネル待ちは全員待ち (101)。
