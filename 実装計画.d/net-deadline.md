### 2.10?? std.net の期限 (B5: slowloris)

- 追加: `TcpStream.set_read_deadline(ms)` / `set_write_deadline(ms)` (今から ms 後を、以後の全部の読み・書きの期限にする。壁時計)、`clear_deadlines()`、`set_nodelay(bool)`、`TcpListener.accept_timeout(ms)`、`std.net.connect_timeout(addr, ms)`。期限切れは読み書きでは `std.io.Error` の TimedOut (code は ETIMEDOUT)、受理・接続では `std.net.Error` の code が ETIMEDOUT。
- 実装: 期限は fd ごとの絶対時刻の表。木走査は `kir_exec.rs` の `dl_call` / `accept_until` / `connect_until` を共有 (`net.rs` も使う)。ネイティブは `sysrt.c` の `ksn_rt_sys_net_rw` などで同じ表 (単調時計)。読み書きは残りの時間を SO_RCVTIMEO / SO_SNDTIMEO に入れて待つので、1 バイトずつ届く送り手にも効く。
- NetK: `ReadDeadline` / `WriteDeadline` / `ClearDeadlines` / `SetNoDelay` / `AcceptTimeout` / `ConnectTimeout` を追加。読み書きの `Op::NetRead` / `NetWrite` は両エンジンとも io.Error を値で返す (従来の木走査は実行時の panic だった)。
- 試験: `stage0/tests/net_deadline.rs` (1 バイト/秒の送り手に対する 300 ms の読み期限、閉じたポートの接続・届かない宛先・無接続の受理の期限、nodelay/期限の設定。木走査とネイティブ)。
- 未対応: Decoder (`std.http`) の全体の期限 (`with_deadline`) は入れていない。Windows の期限つき操作は InvalidInput で失敗する (sysrt.c の `#else` の空実装)。
