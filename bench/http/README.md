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

この環境には不安定な CPU (10 と 11。`実装計画.md` 7 章) があるので、計測では `taskset -c 0-9,12-31` の範囲だけを使う。
サーバを `0-9`、負荷生成器を `12-31` に分けて固定している (`--server-cpus` / `--client-cpus` で変えられる)。
loopback なので、生成器とサーバが CPU を取り合わないように分けるのが目的。

## 読み方の注意

- 接続を使い捨てる (1 接続 1 リクエスト) ので、計測の大半はカーネルの TCP 接続の確立と破棄。言語の差より、受付と配布の形の差が出る。
- Kasane 版の「1 接続ずつ」は、従来の `examples/shortener` の動作 (参考)。Go の `goroutine per connection` は Go の慣用形 (参考)。
- ストアのロックを握る時間は短く (ベクタの参照と追記だけ)、RPS を決めるのは主に受付と TCP。

## 計測結果 (2026-11-12)

> 注: 以下の表は Cranelift（`--backend clif`、削除済み）での計測。LLVM（`ksn build --release`）での測り直しは `修正プラン.md` 2-3 で行う。それまで、この表の数値は現在の処理系では再現できない。

環境: Intel Core i9-13900KF (32 論理 CPU)、Linux 7.3、loopback。サーバは `taskset -c 0-9`、負荷生成器は `taskset -c 12-31` (CPU 10 / 11 は使わない)。
rustc 1.99.1 (`-C opt-level=3`)、Go 1.27.3、Kasane は当時 `ksn build --backend clif --release` (Cranelift。削除済み)。10 秒 x 3 回の RPS の中央値の回。エラー (期待と違うステータス・接続の失敗) は全回 0。

並行接続 64 / ワーカー 4:

| 実装 | RPS | p50 (ms) | p99 (ms) | 実行ファイル (KiB) | ビルド (秒) |
| --- | ---: | ---: | ---: | ---: | ---: |
| Rust (std, thread pool) | 481,488 | 0.12 | 0.30 | 553 | 0.5 |
| Go (net, worker pool) | 303,464 | 0.20 | 0.35 | 3,587 | 1.5 |
| Kasane (native, worker pool) | 434,453 | 0.14 | 0.22 | 133 | 0.4 |
| (参考) Go, goroutine per connection | 282,644 | 0.22 | 0.39 | 3,587 | 1.5 |
| (参考) Kasane, 1 接続ずつ (従来) | 140,717 | 0.45 | 0.54 | 133 | 0.4 |

並行接続 256 / ワーカー 16:

| 実装 | RPS | p50 (ms) | p99 (ms) | 実行ファイル (KiB) | ビルド (秒) |
| --- | ---: | ---: | ---: | ---: | ---: |
| Rust (std, thread pool) | 554,362 | 0.31 | 0.77 | 553 | 0.5 |
| Go (net, worker pool) | 289,947 | 0.83 | 1.75 | 3,587 | 1.5 |
| Kasane (native, worker pool) | 391,270 | 0.44 | 0.60 | 133 | 0.4 |
| (参考) Go, goroutine per connection | 241,453 | 0.97 | 2.37 | 3,587 | 1.5 |
| (参考) Kasane, 1 接続ずつ (従来) | 38,909 | 0.92 | 1.04 | 133 | 0.4 |

- ビルド時間は、コンパイラのキャッシュなし (Go は `GOCACHE` を空にした初回)。Kasane は `ksn build` 全体で、可搬なインタプリタの束 (ksn のコピー) も含む。実行ファイルの大きさは `shortener.native` (束は含めない)。
- Kasane のワーカーの形は Rust の約 0.9 倍 (64 接続) / 0.7 倍 (256 接続)、Go の約 1.4 倍。1 接続ずつの従来形からは 3.1 倍 (64 接続)。（Cranelift での計測。LLVM での測り直しは `修正プラン.md` 2-3）
- 1 接続ずつの形が 256 接続で急に落ちるのは、受け付けが待つ間に待ち行列 (listen のバックログ) があふれて SYN が再送されるため。
- 1 回の計測の揺れは数 % ある (3 回の中央値を採っている)。上の差のうち、Kasane と Rust の約 10〜30 % の差は揺れより大きいが、原因 (Cranelift の生成コード・受付の形・確保など) は切り分けていない。
