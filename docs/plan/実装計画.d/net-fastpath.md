# std/net-fastpath

- 事実: API サーバの 1 要求は sendto 1 + recvfrom 1 で Rust と同数 (perf1)。残りの差は共有の錠。
- 原因: `ksn_rt_sys_net_rw` (全接続の読み書き) と `ksn_io_deadline` が、期限を使わなくても毎回大域の `ksn_dl_mu` を取り 256 枠を走査していた。
- 直し: 使用中の枠数 `ksn_dl_live` を持ち、0 の間は錠と走査を飛ばす (`sysrt.c`)。意味は変えない。
