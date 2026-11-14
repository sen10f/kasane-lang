# bench/http: URL 短縮サーバの比較ベンチマーク (段階 C)

`examples/shortener` (Kasane、ネイティブ生成のワーカープール) を、同じ HTTP/1.1 の部分集合を話す Rust 版と Go 版と比べる。
外部クレート・外部モジュールは使わない (ネットワークなしで動く)。

## 比べるもの

| 実装 | ファイル | 形 |
| --- | --- | --- |
| Kasane | `examples/shortener` (`ksn build --release`) | 受付スレッドが接続を `Sender<TcpStream>` に送り、ワーカー N 本が `Receiver` の複製で取り合う。ストアは容量 1 のチャネルで持ち回す |
| Rust | `rust/server.rs` (std のみ) | 受付スレッド + ワーカー N 本 (`Arc<Mutex<Receiver>>`)。ストアは `Mutex<Vec<String>>` |
| Go | `go/server.go` (標準ライブラリのみ。Go が入っているときだけ) | 受付 + ワーカー goroutine N 本 (既定)。`-mode spawn` は接続ごとに goroutine |

3 つとも次の要求・応答を **バイト列まで同じ**に返す (`parity.py` で確かめる)。1 接続 1 リクエスト (`Connection: close`)、保存先はメモリのみ。

- `POST /shorten` (本文 = URL) -> `201 {"code":"1000","url":"..."}`。不正な URL は `400 {"error":"invalid url"}`
- `GET /<code>` -> `302` + `Location` (本文は `201` と同じ JSON)。無いコードは `404 {"error":"not found"}`
- `GET /stats` -> `200 {"links":N}`
- そのほか `405` / 壊れた要求は `400` / 大きすぎる本文は `413`

コードは `46656 + 連番` を 36 進で書いたもの (1000, 1001, ...)。

## 負荷

`loadgen/loadgen.rs` (Rust std のみ): N 本のスレッドが、それぞれ「接続 -> 1 リクエスト -> 閉じるまで読む」を計測時間だけ繰り返す。
10 回に 1 回が `POST /shorten`、1 回が `GET /stats`、残り 8 回は事前に作った 100 件への `GET /<code>`。
各リクエストの応答のステータスを確かめ、違えばエラーに数える。最初の 1 秒 (ウォームアップ) は数えない。
出力は `rps=.. p50_us=.. p99_us=.. max_us=.. requests=.. errors=..` の 1 行。

## 手順 (リポジトリ直下から)

```sh
# 1. ksn をビルドする (Kasane 版のビルドに使う)
(cd stage0 && cargo build --release)

# 2. 3 つのサーバと負荷生成器をビルドして、同じ応答を返すことを確かめる (Go がなければ Go は飛ばす)
python3 bench/http/run.py --seconds 1 --repeat 1      # ビルドも兼ねる
python3 bench/http/parity.py

# 3. 計測する (表を標準出力に出す)
python3 bench/http/run.py --seconds 10 --repeat 3 --conns 64 --workers 4
python3 bench/http/run.py --seconds 10 --repeat 3 --conns 256 --workers 16
```

`run.py` がやること: ビルド (キャッシュなしの時間と実行ファイルの大きさを測る) -> 実装ごとにサーバを起動 -> `loadgen` で計測 (`--repeat` 回、RPS が中央値の回を採る) -> 表。

ポートは計測のたびに `free_port` で選ぶ。前の計測の TIME_WAIT が残るポートでは、Kasane 版 (SO_REUSEADDR を付けずに bind する) が起動に失敗する (`server exited early`) ため。

個別に動かすなら:

```sh
rustc -C opt-level=3 -o bench/http/target/server_rust bench/http/rust/server.rs
GO111MODULE=off go build -o bench/http/target/server_go bench/http/go/server.go
rustc -C opt-level=3 -o bench/http/target/loadgen bench/http/loadgen/loadgen.rs
(cd examples/shortener && ../../stage0/target/release/ksn build --release)

taskset -c 0-9 bench/http/target/server_rust 127.0.0.1:18080 4
taskset -c 0-9 bench/http/target/server_go -addr 127.0.0.1:18080 -workers 4
taskset -c 0-9 examples/shortener/target/release/shortener.native 127.0.0.1:18080 0 "" 4
taskset -c 12-31 bench/http/target/loadgen 127.0.0.1:18080 64 10
```

Kasane 版の引数は `shortener.native <アドレス> <接続数> <保存先> [ワーカー数]`。接続数 0 は、ワーカーの形では「無制限」
(従来の 1 接続ずつの形では 0 件で終わる)。保存先 `""` はメモリのみ (ファイルへ書かない)。ワーカー数を省くか 0 にすると従来の 1 接続ずつ。
インタプリタ (`ksn run`) でもワーカーの形は動くが、スレッドは 1 つずつ最後まで走るので、先に接続数ぶんの接続が来ていないと受付で止まる
(テストはそう書いてある)。実サーバはネイティブで動かす。

## CPU の固定

開発機 (i9-13900KF) には不安定な CPU (10 と 11。`実装計画.md` 7 章) があるので、計測では `taskset -c 0-9,12-31` の範囲だけを使う。2026-11-14 の計測は Xeon の VM で、この問題はないが、同じ範囲で測った。
サーバを `0-9`、負荷生成器を `12-31` に分けて固定している (`--server-cpus` / `--client-cpus` で変えられる)。
loopback なので、生成器とサーバが CPU を取り合わないように分けるのが目的。

## 読み方の注意

- 接続を使い捨てる (1 接続 1 リクエスト) ので、計測の大半はカーネルの TCP 接続の確立と破棄。言語の差より、受付と配布の形の差が出る。
- Kasane 版の「1 接続ずつ」は、従来の `examples/shortener` の動作 (参考)。Go の `goroutine per connection` は Go の慣用形 (参考)。
- ストアのロックを握る時間は短く (ベクタの参照と追記だけ)、RPS を決めるのは主に受付と TCP。

## 計測結果 (2026-11-14、LLVM)

旧計測 (2026-11-12、Cranelift。`--backend clif` は 2026-11-14 に削除済み) は、Kasane のワーカーの形が Rust の 0.9 倍 (64 接続) / 0.7 倍 (256 接続)、Go の約 1.4 倍だった。Cranelift では再現できないので、下の表で置き換える。旧環境 (i9-13900KF) と今の環境 (Xeon E5-2620 v4 の VM) は別の機械なので、旧と新の RPS は比べない。

環境: Intel Xeon E5-2620 v4 (32 論理 CPU、Proxmox VM)、Linux 7.0.14、loopback。rustc 1.100.0 (`-C opt-level=3 -C codegen-units=1`)、clang 20.1.2 (`ksn build --release` が使う)。Kasane は `ksn build --release` (LLVM。ワーカーは pthread の実スレッド)。サーバは `taskset -c 0-9`、負荷生成器は `taskset -c 12-31`。10 秒 × 3 回の RPS の中央値の回、ウォームアップ 1 秒。エラー (期待と違うステータス・接続の失敗) は全回 0。3 実装の応答のバイト列は `parity.py` で一致を確認 (13 要求、不一致 0)。

**負荷の状態 (注意)。** 他の作業が並行して CPU を使っていた。計測の前後の `uptime` の load average (1 分値) は、64/4 の表が 58.9 → 67.1、256/16 の表が 65.8 → 82.2 (32 論理 CPU に対して)。CPU が詰まった状態の数値なので、RPS の揺れは大きい。下の「3 回の RPS」を見て読むこと。Go は手元に無いので飛ばした (`run.py` は `go` が無いと Go の行を作らない)。

### 並行接続 64 / ワーカー 4

`python3 bench/http/run.py --seconds 10 --repeat 3 --conns 64 --workers 4`

| 実装 | RPS | Rust との比 | p50 (ms) | p99 (ms) | エラー | 実行ファイル (KiB) | ビルド (秒) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Rust (std, thread pool) | 37,851 | 1.00 | 1.43 | 4.97 | 0 | 4,509 | 4.2 |
| Kasane (native, worker pool) | 21,051 | 0.56 | 2.72 | 8.46 | 0 | 78 | 4.2 |
| (参考) Kasane, 1 接続ずつ (従来) | 10,853 | 0.29 | 4.78 | 13.80 | 0 | 78 | 4.2 |
| Go (net, worker pool) | 未計測 (go が無い) | | | | | | |

3 回の RPS: Rust 34,864 / 37,851 / 48,774、Kasane 16,929 / 21,051 / 23,981、1 接続ずつ 9,102 / 10,853 / 11,530。

### 並行接続 256 / ワーカー 16

`python3 bench/http/run.py --seconds 10 --repeat 3 --conns 256 --workers 16`

| 実装 | RPS | Rust との比 | p50 (ms) | p99 (ms) | エラー | 実行ファイル (KiB) | ビルド (秒) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Rust (std, thread pool) | 94,937 | 1.00 | 1.45 | 4.54 | 0 | 4,509 | 2.9 |
| Kasane (native, worker pool) | 33,080 | 0.35 | 3.49 | 12.08 | 0 | 78 | 4.5 |
| (参考) Kasane, 1 接続ずつ (従来) | 7,033 | 0.07 | 12.99 | 644.46 | 0 | 78 | 4.5 |
| Go (net, worker pool) | 未計測 (go が無い) | | | | | | |

3 回の RPS: Rust 46,802 / 94,937 / 136,497、Kasane 32,085 / 33,080 / 34,669、1 接続ずつ 6,912 / 7,033 / 9,068。

- 実行ファイルの大きさとビルド時間は、各表の計測のたびに測った値。Rust の実行ファイルは旧表 (553 KiB) より大きいが、rustc の版と設定が違うので比べない。ビルド時間は Kasane が `ksn build --release` 全体 (clang を含む。キャッシュは空の一時ディレクトリ)、Rust が `rustc` 1 回。
- Kasane のワーカーの形は Rust の 0.56 倍 (64/4)、0.35 倍 (256/16)。3 回を RPS の順に組にした比は 0.49〜0.56 (64/4) と 0.25〜0.69 (256/16) に散る。Rust の 256 接続は揺れが大きい (46.8k〜136.5k) ので、0.35 は中央値の回の値でしかない。
- 1 接続ずつの形は 256 接続で大きく落ちる (p99 644 ms)。受け付けが詰まる形で、原因は確かめていない。
- 1 接続 1 要求なので、計測の大半は TCP の確立と破棄。言語の差より受付と配布の形の差が出る (旧計測と同じ)。
- Go は未計測なので、Go との比は書けない。
