# async-net: 多数接続を 1 スレッドで捌く (非同期ソケット、段階 1)

ブランチ `task/async-net`。設計は `設計_多重化したネット.md`（所有者の決定: 選択肢 B。公開は `accept_async` / `read_async` / `write_async` だけで、epoll の包みは内部）。

## できたこと

- API (`spec/stdlib/std/net.ksnl`、効果 `[io.net, suspend]`): `TcpListener.accept_async(&self)`、`TcpStream.read_async(&mut self, &mut [u8])`、`TcpStream.write_async(&mut self, &[u8])`（一度に 16384 バイトまで）。**`&mut [u8]` のままでよい**: E3050 は「引数として渡す借用」を対象外にするので、初稿の `Vec<u8>` の受け渡しは不要（呼び出し側が `&mut stream` と `&mut buf` を持ったまま呼べることを試験で確かめ、設計書 3 章を訂正した）。
- 実装は Kasane の本体: 内部の中断点 `std.task.wait_io(fd, write) -> i32`（`task.sig`、`pub` なし。0 = 準備完了、1 = 期限切れ）で待ってから、ブロッキング版の `accept` / `read` / `write` を呼ぶ。新しい `ErrorKind`・公開の fd・EAGAIN は無い。
- ネイティブ (LLVM、Linux): `ksn.task.io` が葉の中断点（`await_js` と同じ作り、枠 32 バイト）。`sysrt.c` の `ksn_rt_sys_io_park / poll / ready / take / pending`（スレッドごとの epoll、`EPOLLONESHOT`、fd ごとの印の表）が実体。実行器 (`kir_suspend_exec.kir`) は、切り替えごとに `io_tick`（8 回に 1 回 `epoll_wait(0)`）、走れるタスクが無いときは `io_idle`（`epoll_wait` を最も近い期限まで待ち、起きた fd のタスクを走れる状態に戻す）。期限は `set_read_deadline` の表と `SO_RCVTIMEO` / `SO_SNDTIMEO` を見て、切れたら `TimedOut`（`accept_async` は `std.net.Error { code: 110 }`）。すでに読み書きできる fd は epoll に載せず素通り（続けて 16 回まで。ほかのタスクが飢えないように）。
- 木走査: `poll(fd, 0)` で調べて他のタスクへ譲る（`natives.rs` / `net.rs` の `std.task.wait_io`。何も進まないとき 100 µs 眠る）。出力は決定的でネイティブと一致。KIR 参照実行器は `ksn.io.*` の KIR の本体（中断しない）で動く。
- Windows のネイティブは E9040（「非同期ソケットは Windows では未対応」）。wasm32 は `std.net` ごと未対応で E9040。macOS はリアクター無し（中断せずブロッキング版と同じ）。
- `listen` の待ち行列を 128 → 1024（ネイティブと木走査）: 256 本が先に来ても、同じスレッドで accept する単一スレッドのサーバが詰まらない。
- `std.task.block_on` / `spawn_local` の `F` に `linear` を許した（`task.sig`）: 線形の `TcpStream` を `move` で接続ごとのタスクに渡すため。タスクは最後まで走るので取り込んだ値は本体が消費する。取り消し (E4022) は従来どおり。

## 試験

`stage0/tests/net_async.rs`（データ `tests/data/net_async.ksnl`）: 256 接続の echo（1 スレッド）、何も送らない 1 本があっても他が止まらない、その 1 本は 300 ms で `TimedOut`、木走査とネイティブの出力一致、2 / 3 / 64 接続、Windows / wasm32 の E9040。

## 計測 (2026-11-24。機械の負荷が高い: load average 約 50 / 32 スレッド。目安)

echo / HTTP 風 (要求 64 バイト → 応答 128 バイト、持続接続、閉ループ。同じ機械の Rust のクライアント 1 接続 1 スレッド、合計 20 万要求):

| 接続数 | Kasane 非同期 (1 スレッド) | Kasane 接続ごとにスレッド | Rust 接続ごとにスレッド |
|---|---|---|---|
| 64 | 約 1.9 万 req/s | 約 27 万 | 約 25 万 |
| 256 | 約 1.8 万 req/s | 約 34 万 | 約 28 万 |

- 単一スレッドの非同期は、32 コアで並列に動く「接続ごとにスレッド」にスループットで **勝てない** (約 0.07 倍)。1 要求あたり sys 約 24 µs + user 約 7 µs。syscall は 1 要求 4 回 (`poll` ×2・`recv`・`send`)。`epoll_wait` は 1 回で約 38 本をまとめて起こす。
- 改善の余地: (1) `poll` してから blocking の `recv` / `send` を呼ぶので 4 syscall。`MSG_DONTWAIT` で直接試し、EAGAIN のときだけ待てば 2 syscall。(2) `pick` は O(タスク数) の走査。走れるタスクの列を持てば 256 本でも一定。(3) 複数スレッドの実行器 (設計 7 章の項目 3)。
- 注意: 「接続ごとにスレッド」は持続接続 256 本なら 256 スレッド (スタックとスレッド生成が前提)。非同期は 1 スレッドで 256 本を捌き、無言の接続がワーカーを占有しない。性能を詰める前の最初の測定で、静かな機械で測り直す。

## 残り

- (1) `MSG_DONTWAIT` の試し読み・試し書き、(2) 走れるタスクの列、(3) N スレッド実行器、Windows (IOCP / WSAPoll) は次の段階。
- 例 (`examples/` に非同期 echo) は足していない（このブランチは `examples/` を触らない規則）。
