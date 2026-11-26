# BETA.md — Kasane (ksn) 閉じたベータ版 0.1.0-beta.1 向け案内

## 1. Kasane とは

Kasane は「LLM が書き、機械が検証する、契約重視のシステム言語」です（実装計画 1.1）。人間が読むのは、関数のヘッダ（`uses` / `panics` の契約）、`test` 定義、公開 API の 3 つだけで、本体は検査器が型・借用・効果を保証します。処理系は Rust 製の単一バイナリ `ksn` で、Linux x86_64 ホスト上で動作します。ソース拡張子は `.ksnl`（コメント無しの正規形）と `.ksnh`（コメント付き、人が書く形式）です。公開するパッケージが配るのは `.ksnl` だけで、依存の `.ksnh` は読まれず検査もされません（根のパッケージの `.ksnh` は E9050 で `.ksnl` との一致を確かめます）。

## 2. 動作要件

- ホスト OS: Linux（x86_64）。macOS は未対応です。Windows ホストでの開発も未対応ですが、Linux から Windows 向けへのクロスコンパイルは可能です。
- このバイナリ版 (`ksn-<版>-x86_64-linux-gnu.tar.gz`): glibc 2.30 以上の x86_64 Linux で動きます (Debian 12・Debian 13・Arch の現行で確認する対象。tarball の `REQUIRES.txt` に要る glibc の版を書いてあります)。LLVM とリンカ (lld) は実行体に入っているので、clang・ld・LLVM は要りません。共有ライブラリは glibc の `libc` / `libm` / `libpthread` / `libdl` だけです。
- `ksn build` (ネイティブ生成) が使う、利用者のマシン側の C の開発ファイル (crt 目的ファイルと libgcc):
  - Debian / Ubuntu: `sudo apt install libc6-dev libgcc-12-dev` (Debian 12。Debian 13 は `libgcc-14-dev`。`gcc` を入れれば一式入ります)
  - Arch: `sudo pacman -S gcc glibc` (`gcc` が `/usr/lib/gcc/x86_64-pc-linux-gnu/<版>/` の crtbeginS.o・libgcc を持ちます)
  - 足りないとき `ksn build` は見つからないファイルと入れる package を示して失敗します。`ksn check` / `test` / `run` はこれらを要しません。
- ソースから自分でビルドするだけなら: Rust（cargo）、clang か gcc、Python 3.10 以上、git。ネイティブ生成の既定ビルドには LLVM 20 開発ファイル・zlib/zstd 開発ファイルも必要です（README の「必要な環境」）。

## 3. 導入（実際に確認した手順）

配布用の tarball（`ksn-<版>-x86_64-linux-gnu.tar.gz`）を GitHub の Release から取得して展開します。Arch Linux と Debian 12 以降を対象にしています（要る glibc は 2.30 以上）。
配布用の tarball を展開し、`ksn` を PATH に置きます。ライセンスは `LICENSE-MIT` / `LICENSE-APACHE`、同梱の第三者のライセンスは `THIRD_PARTY_LICENSES`、整合の確認は `SHA256SUMS` で行います。

```bash
# ksn を PATH に置く（例）
tar xzf ksn-<版>-x86_64-linux-gnu.tar.gz
cd ksn-<版>-x86_64-linux-gnu
# ksn 単体で動作します（標準ライブラリは実行体に埋め込まれています）
mkdir -p "$HOME/.local/opt" && cp -r . "$HOME/.local/opt/ksn"
ln -sf "$HOME/.local/opt/ksn/ksn" "$HOME/.local/bin/ksn"
export PATH="$HOME/.local/bin:$PATH"

# キャッシュは既定で ~/.ksn/cache（自分だけが書ける 0700 のディレクトリ）に置かれます。
# KSN_CACHE_DIR で変える場合も、ほかの利用者が書き込める場所（/tmp など）は避けてください。

ksn --version   # ksn 0.1.0
```

`ksn` コマンドは、パッケージ（`kasane.toml` がある所）の中で実行します。下のディレクトリからでも、上へたどってパッケージを見つけます。

## 4. 10 分チュートリアル

### 4.1 パッケージを作る

```bash
mkdir -p /tmp/beta_try && cd /tmp/beta_try
ksn new hello && cd hello
```

生成されるファイル: `kasane.toml`、`kasane.lock`、`src/hello.ksnl`。

### 4.2 コードを書く（`uses` / `panics` を使う関数とテスト）

`src/hello.ksnl` を次の内容に書き換えます。

```kasane
fn hello.double(n: i32) -> i32 uses [] panics never
    n.wrapping_add(n)
end hello.double

fn hello.main() -> Result<(), std.io.Error> uses [io.console] panics never
    std.io.print_line("double: 4")?
    Ok(())
end hello.main

test hello.double.test_basic
    core.check.eq(hello.double(2i32), 4i32)
end hello.double.test_basic
```

- `uses [...]` は副作用（コンソール出力・確保など）を宣言します。
- `panics never` は panic しないことを保証します（添字 `a[i]` や整数の `+` など panic しうる式は不可）。デッドロック検出（終了コード101）、資源の枯渇、呼び出しの深さの上限（終了コード102）はこの保証の対象外です。
- `?` は `Result` の失敗を呼び出し元に伝えます。

### 4.3 検査・整形・実行

```bash
ksn fmt        # 正規形へ整える（書き方は 1 通り）
ksn check      # 診断 0 件なら何も出力しない
ksn test       # テストを実行
ksn run        # 実行 (既定は VM。--engine=tree で木走査)
```

`ksn test` の出力: `test hello.double.test_basic ... ok` / `1 passed; 0 failed`。
`ksn run` の出力: `double: 4`。

`ksn fmt` の出力と違うファイルは E0020（整形未済）になります。ほかの誤りも同時に出るので、`ksn fmt` で整えてから残りを直してください。

### 4.4 ネイティブ実行ファイルをビルドして動かす

```bash
ksn build --release
./target/release/hello
```

出力: `double: 4`。ビルドの最後に `built: target/release/hello (native)` と置き場所が出ます。

## 5. ドキュメントの場所

- チートシート（書き方の一覧、E0042 などの誤り例と正しい書き方）: `チートシート.md`
- 言語仕様: `Kasane 簡易仕様書.md`（文法は `spec/grammar.ebnf`、標準ライブラリ API は `spec/stdlib/`）
- チュートリアル（3 つの実例）: `docs/tutorial.md`
- 既知の制限: `docs/known-limitations.md`
- 診断コードの説明: `ksn explain <コード>`（例 `ksn explain E0042`）
- パッケージの API 文書: `ksn doc`（`target/doc/index.html`）、標準ライブラリ: `ksn doc --std`
- 実装の現状・制限: `実装計画.md`

## 6. 既知の制限（ベータ時点）

- `ksn run` / `ksn test` の既定の実行系は VM（KIR → バイトコード）です。VM に下ろせない形は黙って木走査へ退避します。出力がおかしいときは `--engine=tree`（または `KSN_ENGINE=tree`）と比べて知らせてください。`--watch` は木走査です。VM は起動時に大きな仮想領域を予約するので、`ulimit -v` を付けるなら 8 GB 以上にしてください（それより小さいと、大きな入出力で `AllocError` の panic や木走査への退避になります）。標準出力は木走査・VM・ネイティブのどれも、1 行ごとに書き出します（大量に出すときは文字列にまとめてから 1 回で出すと速い）。
- macOS でのホスト実行・ビルドは未対応。Windows ホストでの開発も未対応（Linux からのクロスビルドは可能）。
- 配布物は x86_64 Linux（Arch、Debian 12 以降）だけです。ライセンスは MIT または Apache-2.0（`LICENSE-MIT` / `LICENSE-APACHE`、同梱の LLVM などは `THIRD_PARTY_LICENSES`）。
- 検査器（`ksn check`）は、通ってはいけないコードを拒むことを目指していますが、穴探しでまだ穴が見つかり続けています（直したものは `stage0/tests/data/unsound/` にあります）。安全性が要るコードでは、ネイティブのビルドを ASan などでも確かめてください。おかしな受理を見つけたら、ぜひ知らせてください。
- `kasane.toml` の `[native]`（C のライブラリのリンク）を使うパッケージは、依存も含めて `[unsafe]` に `native = true` を書く必要があります（無ければ E9024）。C のライブラリは起動時にコードを実行できるためで、`ksn unsafe` で一覧できます。
- 中央の公共パッケージレジストリはありません。`ksn publish --registry <URL>` でレジストリ（またはパス）へ公開できます。依存は `kasane.toml` のローカルパスまたは Git URL + コミットハッシュで指定します。
- `std.net` の非同期（`accept_async` / `read_async` / `write_async`）は Linux のみです。
- ネイティブ生成に下ろせない機能（一部の `std.tls`・FFI など）はインタプリタ同梱バンドルに退避し、`ksn build --release` では E9040 で失敗します（`--release --allow-bundle` で退避を許可）。
- 手書きの `impl core.ops.Drop` の `drop` は、スコープの終わり（宣言の逆順）・再代入・`core.mem.drop`・文の終わりの一時値で呼ばれます（`ksn run` / `ksn test` / ネイティブで同じ。実行は KIR で、手書きの `Drop` を持つパッケージは木走査へ退避せず、KIR に下ろせなければ理由付きで失敗します）。KIR に下ろせない形が含まれる場合の失敗は `docs/known-limitations.md` を参照してください。`ksn debug` / `ksn dap` も VM で走るので、手書きの `Drop` を持つパッケージもデバッグできます。
- `ksn fix` が自動修正できるのは一部の診断（E1108 / E1020 / E2041 / E2042）のみです。
- スレッドを作れずに `std.thread.spawn` が失敗したとき、ネイティブと VM は `Err(SpawnError)` を返さず、`spawn failed: ...` の panic でプログラムを止めます (終了コード 101)。クロージャを返せるのは木走査 (`--engine=tree`) だけです。
- `dyn` 型・クロージャの C 関数ポインタ渡し（`core.ffi.trampoline` が必要）など、一部は未対応または制限あり。

## 7. 問題の報告方法

バグや不明な点は、以下の連絡先までお知らせください。

連絡先: [プレースホルダ：担当者／チームのメールまたはチケットURLをここに記入]

再現を送っていただける場合は、次の情報を添えてください。
- `ksn --version` の出力
- ホスト OS とアーキテクチャ（`uname -a`）
- 再現する最小の `.ksnl` ソースと `kasane.toml`
- `ksn check` / `ksn run` / `ksn build` のエラーメッセージ（コピー＆ペースト可）

## 8. Edition 1 の範囲（何が入る / 何が実験的か）

Edition 1 は処理系の核だけを凍結する。

- 凍結するもの（言語・コア標準ライブラリ・ツール）：検査器（`ksn check`）、実行系（VM + LLVM ネイティブ）、`check` / `run` / `test` / `build` / `fmt` / `fix`、LSP、およびこれらが使う言語仕様とコア標準ライブラリ（`core` / `alloc` / `std` のうち Edition 1 で確定した範囲）。
- 実験的で Edition 1 の範囲外：`ui/`（UI）、WASM SPA、パッケージレジストリ / `publish`、セルフホスト（stage1/stage2）、bindgen。これらは Edition 1 の契約には含まず、v1 以降で検討する。
