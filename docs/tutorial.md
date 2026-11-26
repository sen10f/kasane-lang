日本語 | [English](en/tutorial.md)

# Kasane チュートリアル

このチュートリアルでは、Kasane の基本的なプログラムを3つ作ります。

1. 標準出力に文字を表示する Hello プログラム
2. ファイルを読み、`HashMap` で単語ごとの出現回数を数えるプログラム
3. `std.http` を使う、1往復だけ行う小さな HTTP サーバ

各プログラムのソースは、この文書の隣の `docs/tutorial/1`、`docs/tutorial/2`、`docs/tutorial/3` に実パッケージとして置いています。下列の `ksn` コマンドは、対象の `kasane.toml` があるディレクトリで実行してください。

## 1. Hello

### パッケージ

```text
docs/tutorial/1/
├── kasane.lock
├── kasane.toml
└── src/
    └── hello.ksnl
```

`kasane.toml` の `entry` は `hello.main` なので、実行入口は `hello.main` です。

### ソース

`src/hello.ksnl` の内容は次のとおりです。

```kasane
fn hello.main() -> Result<(), std.io.Error> uses [io.console] panics never
    std.io.print_line("こんにちは、Kasane!")?
    Ok(())
end hello.main
```

関数の見出しは、戻り値、効果、panic の契約を表します。この例はコンソール出力だけを使うので `uses [io.console]`、panic しないので `panics never` と宣言しています。本文の `?` は、標準出力の `Result` が失敗した場合に `Err` を返す演算子です。

### 実行

```bash
cd docs/tutorial/1
ksn fmt
ksn check
ksn run
```

`ksn fmt` はソースを正規形へ整えます。`ksn check` は診断 0 件を確認し、出力を表示しません。`ksn run` の出力は次のとおりです。

```text
こんにちは、Kasane!
```

## 2. ファイルを読み、単語ごとの出現回数を数える

### パッケージ

```text
docs/tutorial/2/
├── kasane.lock
├── kasane.toml
├── sample.txt
└── src/
    └── wordcount.ksnl
```

入力ファイルの `sample.txt` は次のとおりです。

```text
Kasane is a small systems language.
Kasane checks effects and borrows.
Language design matters.
```

### ソース

`src/wordcount.ksnl` の内容は次のとおりです。

```kasane
enum wordcount.Error derive [Debug, Clone, Copy, PartialEq, Eq, From] pub
    Alloc(alloc.alloc.AllocError)
    Io(std.io.Error)
    Utf8(core.str.Utf8Error)
end wordcount.Error

fn wordcount.add_word(counts: &mut std.collections.HashMap<std.string.String, usize>, word: &str) -> Result<(), wordcount.Error> uses [alloc] panics never
    match counts.get_mut_str(word) {
        Some(count) => {
            *count = count.wrapping_add(1usize)
            Ok(())
        }
        None => {
            let key = std.string.String.from_str(word)?
            match counts.insert(key, 1usize) {
                Ok(_) => Ok(())
                Err(error) => Err(wordcount.Error.Alloc(error))
            }
        }
    }
end wordcount.add_word

fn wordcount.count_text(text: &str) -> Result<std.collections.HashMap<std.string.String, usize>, wordcount.Error> uses [alloc] panics never
    let mut counts = std.collections.HashMap.new()
    let mut word = std.string.String.new()
    for c in text.chars() {
        if c.is_ascii_whitespace() {
            if !word.is_empty() {
                wordcount.add_word(&mut counts, word.as_str())?
                word.clear()
            }
        } else {
            word.push(c)?
        }
    }
    if !word.is_empty() {
        wordcount.add_word(&mut counts, word.as_str())?
    }
    Ok(counts)
end wordcount.count_text

fn wordcount.main() -> Result<(), wordcount.Error> uses [alloc, io.console, io.fs] panics never
    let text = wordcount.read_file("sample.txt")?
    let counts = wordcount.count_text(text.as_str())?
    wordcount.print_count(&counts, "Kasane")?
    wordcount.print_count(&counts, "checks")?
    wordcount.print_count(&counts, "Language")?
    let mut line = std.string.String.new()
    line.push_str("unique words: ")?
    line.push_u64(u64.from_usize(counts.len()))?
    std.io.print_line(line.as_str())?
    Ok(())
end wordcount.main

fn wordcount.print_count(counts: &std.collections.HashMap<std.string.String, usize>, word: &str) -> Result<(), wordcount.Error> uses [alloc, io.console] panics never
    let count = match counts.get_str(word) {
        Some(value) => *value
        None => 0usize
    }
    let mut line = std.string.String.new()
    line.push_str(word)?
    line.push_str(": ")?
    line.push_u64(u64.from_usize(count))?
    std.io.print_line(line.as_str())?
    Ok(())
end wordcount.print_count

fn wordcount.read_file(path: &str) -> Result<std.string.String, wordcount.Error> uses [alloc, io.fs] panics never
    let mut data = std.vec.Vec.new()
    with mut file = std.fs.open(path)? close core.mem.drop(file.close()) {
        core.mem.drop(std.fs.read_to_end(&mut file, &mut data)?)
    }
    let mut text = std.string.String.new()
    text.push_str(core.str.from_utf8(data.as_slice())?)?
    Ok(text)
end wordcount.read_file
```

### 実行

```bash
cd docs/tutorial/2
ksn fmt
ksn check
ksn run
```

出力は次のとおりです。

```text
Kasane: 2
checks: 1
Language: 1
unique words: 13
```

### 処理の流れ

`read_file` は `std.fs.open` でファイルを開き、`std.fs.read_to_end` でバイト列を `Vec` へ読みます。`core.str.from_utf8` で UTF-8 として検証してから `String` へ移します。

`count_text` は文字を先頭から順に見て、ASCII 空白ごとに単語を区切ります。`String` は `push` で一文字ずつ追加し、単語の区切りで `clear` します。`str.split_whitespace` は提供されていないため、この例では `char.is_ascii_whitespace` を使います。

集計表の型は `HashMap<String, usize>` です。キーは所有する `String` として保存します。検索だけは所有値を 新しく作らない `get_str`、更新は `get_mut_str` を使います。`String` キーの辞書で `get` や `get_mut` を直接使う書き方は認められていません。

`core.mem.drop` は、非線形の値を明示的に消費して捨てる演算子です。`read_file` では、`file.close()` が線形のファイルを値で消費して閉じ、その `Result` を `core.mem.drop` で捨てています。`std.fs.read_to_end` が返した読み込みバイト数も `core.mem.drop` で捨てています。`with ... close` を使うと、通常の制御フローで本体を抜けたときに指定した後始末を実行できます（panic では実行しません）。

## 3. 小さな HTTP サーバ

### パッケージ

```text
docs/tutorial/3/
├── kasane.lock
├── kasane.toml
└── src/
    └── httpserver.ksnl
```

### ソース

`src/httpserver.ksnl` の内容は次のとおりです。

```kasane
enum httpserver.Error derive [Debug, Clone, Copy, PartialEq, Eq, From] pub
    Alloc(alloc.alloc.AllocError)
    Http(std.http.Error)
    Io(std.io.Error)
    Net(std.net.Error)
end httpserver.Error

fn httpserver.address(port: u16) -> Result<std.string.String, httpserver.Error> uses [alloc] panics never
    let mut address = std.string.String.new()
    address.push_str("127.0.0.1:")?
    address.push_u64(u64.from_u16(port))?
    Ok(address)
end httpserver.address

fn httpserver.exchange(listener: &std.net.TcpListener) -> Result<(), httpserver.Error> uses [alloc, io.console, io.net] panics never
    let port = listener.local_port()?
    let address = httpserver.address(port)?
    with mut client = std.net.connect(address.as_str())? close core.mem.drop(client.close()) {
        with mut server = listener.accept()? close core.mem.drop(server.close()) {
            let request_bytes = "GET /hello HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
            std.http.write_all(&mut client, request_bytes.as_bytes())?
            let mut decoder = std.http.Decoder.new(8192usize, 65_536usize)
            match std.http.read_request(&mut server, &mut decoder)? {
                Some(request) => {
                    std.io.print_line(request.method.as_str())?
                    std.io.print_line(request.path.as_str())?
                    httpserver.send_response(&mut server, &request)?
                }
                None => return Err(httpserver.Error.Http(std.http.Error.Truncated))
            }
        }
    }
    Ok(())
end httpserver.exchange

fn httpserver.main() -> Result<(), httpserver.Error> uses [alloc, io.console, io.net] panics never
    with listener = std.net.listen("127.0.0.1:0")? close core.mem.drop(listener.close()) {
        httpserver.exchange(&listener)?
    }
    std.io.print_line("200 OK")?
    Ok(())
end httpserver.main

fn httpserver.send_response(stream: &mut std.net.TcpStream, request: &std.http.Request) -> Result<(), httpserver.Error> uses [alloc, io.net] panics never
    let body = "Hello, Kasane!\n"
    let head = "Content-Type: text/plain; charset=utf-8\r\n"
    let bytes = std.http.render(200u16, head, body.as_bytes(), request.keep_alive())?
    std.http.write_all(stream, bytes.as_slice())?
    Ok(())
end httpserver.send_response
```

### 実行

```bash
cd docs/tutorial/3
ksn fmt
ksn check
ksn run
```

出力は次のとおりです。

```text
GET
/hello
200 OK
```

### 処理の流れ

`std.net.listen` は待受側の TCP リスナを作ります。アドレスのポート番号に `0` を指定すると、空いているポートが選ばれます。`local_port` でその番号を取得し、同じプログラムのクライアントが接続します。

サーバ側では `std.http.Decoder` を用意し、`std.http.read_request` で要求を解析します。解析後の `request.method` と `request.path` を表示し、`std.http.render` で応答を作り、`std.http.write_all` で送ります。

この例は外部の HTTP クライアントや無限待ちを使わず、リスナとクライアントが同じプログラム内で 1 往復します。そのため `ksn run` は外部入力を待たずに終了します。

`httpserver.Error` は `std.http.Error` と `std.net.Error` を別々に保持し、`derive [From]` によって `?` のエラー変換を行います。エラーの種類が複数ある場合も、変換可能な型を `From` で明示します。

## Kasane を書くとき

### 定義は FQN で終える。順序は `ksn fmt` に任せる

すべての定義は `fn 名前.関数` や `type 名前.型` のような完全修飾名（FQN）で書き、`end 同じ完全修飾名` で閉じます。定義の順番に意味はありません。`ksn fmt` は定義を FQN 順に並べ、本文や空行などの共通の形も整えます。

### タプルの要素に `.0` は使わない

Kasane に `t.0` のような位置によるフィールド参照はありません。タプルは分解して名前付きの値として取り出します。`t.0` の代わりに `let (first, second) = t` のように書きます。

### 参照は二級の値

`&T` と `&mut T` は、既存の値を短時間だけ借りる二級の値です。関数へ渡したり、局所変数に一時的に保持することはできますが、型の欄、コンテナ、チャネルへ格納できません。長期保存したい値には、所有する `String` などを使います。上記の `HashMap` が `String` をキーにするのも、この規則によるものです。

### 効果と panic の宣言は本文と完全一致させる

`uses [...]` は I/O や確保などの効果を表し、`panics ...` は panic の可否を表します。宣言は本文と完全に一致する必要があり、宣言も検査の対象です。省略や過大な宣言も誤りになります。本文から一意に決まる場合、`ksn fix` が `uses` と `panics` の宣言を補完・書き換えできます。

### 整数リテラルには型を表す接尾辞を付ける

整数リテラルの型は `1u32`、`1u64`、`1usize`、`200u16` のように接尾辞で明示します。型が推論できる整数リテラルで接尾辞を省略すると、`ksn check` は E2028 を出します。推論できる場合は `ksn fix` が接尾辞を補えます。`ksn fmt` は 5 桁以上の 10 進数を 3 桁ずつ `_` で区切ります。

### 線形値は必ず消費する

線形値（`linear` と宣言した型や、線形値を含む型）は、消費せずスコープを抜けると誤りになります。消費するには、`self` を値で受け取る関数へ渡す、分解して値を受け取る、所有者の `with` に後始末として渡すなどします。`core.mem.drop` は非線形の値専用なので、線形値の消費には使いません。

### `ksn fmt` と `ksn check` を併用する

`ksn fmt` は書き方の揺れを直し、`ksn check` は意味の誤りを検査します。`ksn fmt --check` は、正規形でなければ E0020 を出します。新しく書いたコードは、まず `ksn fmt`、次に `ksn check`、最後に `ksn run` の順で確認してください。

## 4. C の関数を呼ぶ（`extern` + `link`、`layout c` 構造体、コールバック）

Kasane から C の共有ライブラリを呼ぶには、`extern` 定義に `link "<パス>:<シンボル>"` を書き、`unsafe` ブロックの中で呼びます。構造体は `layout c` で C と同じメモリ配置を宣言し、関数ポインタを引数に取る C の関数に Kasane の関数をコールバックとして渡せます。

実パッケージは `examples/tutorial_ffi/` にあります。

### パッケージ

```text
examples/tutorial_ffi/
├── kasane.lock        (version = 1)
├── kasane.toml        ([unsafe] modules = ["tutorial_ffi"])
├── build.sh           (cc -shared -fPIC -o libffi.so c/ffi.c)
├── c/
│   └── ffi.c          (rect_area, rect_from, rect_grow, rect_each_vertex)
└── src/
    ├── tutorial_ffi.ksnh  (コメント付き。正のソース)
    └── tutorial_ffi.ksnl  (`ksn strip` で生成。コメント無しの正規形)
```

`kasane.toml` の `[unsafe] modules` にモジュール名を列挙します（`extern` / `unsafe` / `core.ptr` を使うモジュールはここで許可する必要があります）。

### ソース（主要部）

C 側（`c/ffi.c`）は `Rect` / `Point` 構造体を値で渡したり値で返したり、コールバックを呼んだりします。

```c
typedef struct { double x; double y; } Point;
typedef struct { Point min; Point max; } Rect;
double rect_area(Rect r) { return (r.max.x - r.min.x) * (r.max.y - r.min.y); }
Rect rect_from(Point a, Point b) { Rect r = { a, b }; return r; }
void rect_grow(Rect *r, double d) { ... }
typedef void (*vertex_fn)(double x, double y, void *user);
void rect_each_vertex(Rect r, vertex_fn f, void *user) { ... }
```

`build.sh` で `libffi.so` を作ります（`ksn run` / `ksn test` はこのディレクトリを作業ディレクトリにして動くので、`link "./libffi.so:..."` は相対パスで辿れます）。

Kasane 側は `layout c` の構造体を定義し、`extern` でリンクします。

```ksnl fragment
type tutorial_ffi.Rect layout c derive [Clone, Copy]
    min: tutorial_ffi.Point
    max: tutorial_ffi.Point
end tutorial_ffi.Rect

extern tutorial_ffi.rect_area(r: tutorial_ffi.Rect) -> f64 link "./libffi.so:rect_area"
extern tutorial_ffi.rect_grow(r: *mut tutorial_ffi.Rect, d: f64) -> () link "./libffi.so:rect_grow"
extern tutorial_ffi.rect_each_vertex(r: tutorial_ffi.Rect, f: fn(f64, f64, *mut tutorial_ffi.Acc) -> () uses [] panics never, user: *mut tutorial_ffi.Acc) -> () link "./libffi.so:rect_each_vertex"
```

`core.ptr.from_mut` でローカルの構造体への生ポインタを取り、`core.ptr.read` / `core.ptr.write` でコールバック側のユーザーデータを読み書きします。すべて `unsafe "ffi" "..."` ブロックの中に閉じ込めます。

### 実行

```bash
cd examples/tutorial_ffi
sh build.sh
ksn strip
ksn fmt
ksn fmt --check
ksn check
ksn test --both
```

出力（`ksn test --both`）は次のとおりです。

```text
test tutorial_ffi.area.test_area ... ok (both: llvm)
test tutorial_ffi.vertex_stats.test_vertices ... ok (both: llvm)

2 passed; 0 failed
```

`ksn run` も `tutorial_ffi ok` と出力して終了コード 0 になります（`libffi.so` がカレントディレクトリにあることが前提です）。

### 補足

- `.ksnl` にはコメントを書けない（E0059）。コメントは `.ksnh` に書き、`ksn strip` で `.ksnl` を生成します（両方コミットします）。
- `link "./libffi.so:..."` のように `./` を付けると、インタプリタがカレントディレクトリの共有ライブラリを `dlopen` できます。`link "libffi.so:..."` のような裸のファイル名は解決できないことがあります。

## 5. `ksn bindgen --c` で C の見出しからバインディングを生成

`ksn bindgen --c <見出し.h>` は libclang（`dlopen` で実行時にだけ開く；本体にはリンクしない）で C の見出しを解析し、Kasane の `extern` 宣言と `pub fn` ラッパーを生成します。生成物は `ksn fmt` の正規形そのもので、`.ksnh` と `.ksnl` の両方を書き出します。

実パッケージは `examples/tutorial_bindgen/` にあります。

### パッケージ

```text
examples/tutorial_bindgen/
├── kasane.lock   (version = 1)
├── kasane.toml   ([unsafe] modules = ["tutorial_bindgen"])
├── build.sh      (cc -shared -fPIC -o libdemo.so c/demo.c)
├── c/
│   ├── demo.h    (Color 列挙, Point 構造体, MAX_POINTS 定数, area/make_point/for_each)
│   └── demo.c
└── src/
    └── tutorial_bindgen.ksnl  (bindgen の生成物 + main)
```

### 生成

```bash
cd examples/tutorial_bindgen
sh build.sh
ksn bindgen --c c/demo.h --prefix tutorial_bindgen --link libdemo.so -o src/tutorial_bindgen.ksnh
```

生成例（`src/tutorial_bindgen.ksnh` の一部）：

```ksnl fragment
const tutorial_bindgen.RED: u32 = 0u32 pub
const tutorial_bindgen.MAX_POINTS: i32 = 100i32 pub

type tutorial_bindgen.Point layout c derive [Clone, Copy] pub
    x: f64
    y: f64
end tutorial_bindgen.Point

extern tutorial_bindgen.raw_area(p: tutorial_bindgen.Point) -> f64 link "./libdemo.so:area"
extern tutorial_bindgen.raw_make_point(x: f64, y: f64) -> tutorial_bindgen.Point link "./libdemo.so:make_point"

fn tutorial_bindgen.area(p: tutorial_bindgen.Point) -> f64 uses [] panics may pub
    unsafe "bindgen" "C の関数 area を呼ぶ" {
        tutorial_bindgen.raw_area(p)
    }
end tutorial_bindgen.area
```

`extern` はモジュール内部からしか見えないため `raw_` 接頭辞をつけ、`pub fn` ラッパー（`unsafe "bindgen" "..."` で包む）で外部から呼べるようにします。`#define` の整数定数は最小の整数型の `const` に、列挙体は各要素を `const` に、構造体は `layout c` に、関数ポインタは `fn(..) -> ..` になります。ビットフィールド・`union`・可変長引数・`static`/`inline` 関数などは飛ばされ、理由が冒頭コメントに残されます。

生成後に `main` を同じモジュールに追記し、`[unsafe] modules` にパッケージ名（= prefix）を入れます。

### 実行

```bash
cd examples/tutorial_bindgen
ksn fmt --check
ksn check
ksn test --both
ksn run
```

出力：

```text
tutorial_bindgen ok
```

### 補足

- 出力パスはモジュール名に対応する `src/<モジュール>.ksnh` に置く（違えば注意メッセージが出る）。
- 生成物は `ksn fmt` の形なので、`ksn fmt --check` はそのまま通ります。`--link` でリンクするライブラリ名を指定し、`build.sh` でその共有ライブラリを用意します（`ksn run` / `ksn test` で `dlopen` するためカレントディレクトリに置く）。
- `ksn bindgen` は C++ の見出し、`long double` / `_Complex`、入れ子の無名 struct/union は未対応です（実装計画.md の 2.10dj 節 "残り" 参照）。

## 6. クロスビルド（`x86_64-windows-gnu` / `aarch64-linux-gnu` / `wasm32-wasi`）

`ksn build --target <三つ組>` でホストと異なるターゲット向けのネイティブ実行ファイルを作れます。出力は `target/<三つ組>/<debug|release>/` に、バンドルは作りません。

実パッケージは `examples/tutorial_cross/` にあります（単純な「tutorial_cross ok」を出力するプログラム）。

### 対応するターゲット

`x86_64-linux-gnu`・`aarch64-linux-gnu`・`x86_64-windows-gnu`・`wasm32-unknown-unknown`・`wasm32-wasi`（省略は今の機械）。

### 実行と結果

```bash
cd examples/tutorial_cross
ksn fmt --check
ksn check
ksn build --target x86_64-windows-gnu
ksn build --target aarch64-linux-gnu
ksn build --target wasm32-wasi
```

各ターゲットの出力例：

```text
native executable: target/x86_64-windows-gnu/debug/tutorial_cross.exe
built target/x86_64-windows-gnu/debug (debug, target x86_64-windows-gnu; cross-compiled, not run here)
native executable: target/aarch64-linux-gnu/debug/tutorial_cross
built target/aarch64-linux-gnu/debug (debug, target aarch64-linux-gnu; cross-compiled, not run here)
native executable: target/wasm32-wasi/debug/tutorial_cross.wasm
built target/wasm32-wasi/debug (debug, target wasm32-wasi; cross-compiled, not run here)
```

### 実行に必要なもの（この機で確かめた範囲）

- `aarch64-linux-gnu`: `/usr/aarch64-linux-gnu` の sysroot と `qemu-aarch64` があれば実行できます。

  ```bash
  qemu-aarch64 -L /usr/aarch64-linux-gnu target/aarch64-linux-gnu/debug/tutorial_cross
  # 出力: tutorial_cross ok（終了コード 0）
  ```

- `x86_64-windows-gnu`: 組み込みの lld (MinGW) と `/usr/x86_64-w64-mingw32` の sysroot で `.exe`（PE32+）をビルドできます。実行には `wine64` が必要ですが、この機には `wine64` が入っていないためビルドのみ確かめています（実行未確認）。

- `wasm32-wasi`: `.wasm` をビルドできます。WASI ランタイム（`wasmtime` など）がこの機に入っていないため、ビルドのみ確かめています（実行未確認）。

### 補足

- `aarch64-linux-gnu` は C の開発ファイル（`/usr/aarch64-linux-gnu`）を使い、`KSN_SYSROOT` で上書きできます。
- `x86_64-windows-gnu` は `gcc-mingw-w64-x86-64-posix` と `mingw-w64-x86-64-dev` が必要です（ここでは導入済み）。
- `wasm32` と `--backend c` は C のライブラリを持てないので、`[native]` を指定すると E9040 になります。
- `ffi` の例（`libc.so.6` を引く）は Windows では動きません（実装計画.md の 2.10dg 節 "未対応" 参照）。

## 7. UI パッケージの第一歩（layout + tree + event、GPU は使わない）

`ui/` 以下に Kasane 製の UI パッケージがあります。この節では `ui.layout`（レイアウト）・`ui.tree`（要素木・差分更新）・`ui.event`（イベント送達・ヒットテスト）の3つだけを触り、GPU 描画は扱いません。詳細は各パッケージの `ui/kasane/<名前>/README.md` を参照してください。

実パッケージは `examples/tutorial_ui/` にあります（`ui/` の3パッケージを `path` 依存として使います）。

### パッケージ

```text
examples/tutorial_ui/
├── kasane.lock   (version = 1、path 依存を [[package]] で記録)
├── kasane.toml   ([dependencies] に ui_layout / ui_tree / ui_event)
└── src/
    └── tutorial_ui.ksnl
```

`kasane.toml` は `ui/kasane/` への相対パスで依存を指定します：

```toml
[dependencies]
ui_layout = { path = "../../ui/kasane/layout" }
ui_tree   = { path = "../../ui/kasane/tree" }
ui_event  = { path = "../../ui/kasane/event" }
```

`ksn update` で `kasane.lock` に `[[package]]` エントリを書き込みます（`version = 1` のまま、`source = "path+..."` 形式）。

### ソース（主要部）

```ksnl fragment
fn tutorial_ui.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut tree = ui.layout.Tree.new()
    let root_style = ui.layout.Style.default()
    let root = tree.push_node(root_style)
    let child_style = ui.layout.Style.default()
    let c = tree.push_node(child_style)
    tree.push_child(root, c)
    tree.layout(root, 300.0f32, 200.0f32)
    let rect = tree.rect(c)
    core.mem.drop(rect)

    let mut utree = ui.tree.Tree.new()
    let root2 = utree.push_node(ui.tree.ViewKind.Block, ui.layout.Style.default(), None).unwrap()
    let child2 = utree.push_node(ui.tree.ViewKind.Block, ui.layout.Style.default(), None).unwrap()
    core.mem.drop(utree.push_child(root2, child2))
    core.check.eq(utree.child_count(root2), 1usize)

    let target = ui.event.hit_test(&tree, root, 150.0f32, 150.0f32)
    core.mem.drop(target)

    std.io.print_line("tutorial_ui ok")?
    Ok(())
end tutorial_ui.main
```

`ui.layout.Style` / `Size` の欄はモジュール非公開なので、外部からは `Style.default()` などの既定値を使い、`push_node` / `push_child` / `layout` / `rect` で木を組んで矩形を読みます。`ui.tree.Tree` は `push_node` / `push_child` / `child_count` で要素木を構築し、`ui.event.hit_test` でレイアウト矩形上のヒットテストを行います。

### 実行

```bash
cd examples/tutorial_ui
ksn fmt --check
ksn check
ksn test --both
ksn run
```

出力：

```text
tutorial_ui ok
```

`ksn test --both` は `ui.layout` / `ui.tree` / `ui.event` 各パッケージの内部テスト（それぞれ 16/13/15 件）も 0 件の失敗で通ります。

### 補足

- 3パッケージとも `ksn test --both` でインタープリタとネイティブバンドルの両方で一致を確認しています（浮動小数点比較や `dyn.call` など、KIR に下ろせない一部はインタプリタ・バンドル実行にフォールバックします）。
- GPU 描画やウィンドウバックエンドはこの3パッケージには含まれていません。`ui/` の `native/` ディレクトリや GPU 関連は別のロードマップ項目です。
- 各パッケージの設計・API 全容は `ui/kasane/layout/README.md`、`ui/kasane/tree/README.md`、`ui/kasane/event/README.md` を参照してください。



## 8. `Rc` / `Arc` を使わずにデータを共有する（`alloc.arena` と `Handle<T>`）

Kasane には `Rc` / `Arc` はありません。複数の場所から同じ値を共有したいときは `alloc.arena.Arena<T, A>` に入れ、取っ手 `Handle<T>`（添字と世代だけを持つ `Copy` の値で、`Arena` の側しか作れない）を渡します。要素は 1 本のバッファのスロットに置かれ、`grow` でも添字は動かないので、値を借りている間も取っ手は有効です。

`insert` で取っ手を作り、`get` / `get_mut` で借り、`remove` で捨てます。`remove` した取っ手は「弱い参照」と同じで `get` は `None` を返します。スロットが再利用されると世代が 1 つ上がるので、古い取っ手も当たらず、後の `insert` で作った取っ手と区別されます。

```kasane
fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut a = alloc.arena.Arena.new(std.alloc.SYSTEM)
    core.check.is_true(a.is_empty())
    core.check.eq(a.len(), 0usize)
    let h0 = a.insert(1u32).map_err(t.oom)?
    let h1 = a.insert(2u32).map_err(t.oom)?
    core.check.eq(a.len(), 2usize)
    core.check.is_true(a.contains(h0))
    core.check.eq(a.get(h0), Some(&1u32))
    let copied = h0
    core.check.eq(a.get(copied), Some(&1u32))
    core.check.is_true(h0 == copied)
    match a.get_mut(h1) {
        Some(v) => core.mem.drop(core.mem.replace(v, 20u32))
        None => core.panic.panic("no h1")
    }
    core.check.eq(a.get(h1), Some(&20u32))
    core.check.eq(a.remove(h1), Some(20u32))
    core.check.is_true(a.get(h1).is_none())
    core.check.is_true(!a.contains(h1))
    core.check.eq(a.len(), 1usize)
    let h2 = a.insert(3u32).map_err(t.oom)?
    core.check.is_true(h2 != h1)
    core.check.eq(a.get(h2), Some(&3u32))
    core.mem.drop(std.io.print_line("arena ok"))
    Ok(())
end t.main

fn t.oom(_e: alloc.alloc.AllocError) -> std.io.Error uses [] panics never
    std.io.Error {
        kind: std.io.ErrorKind.OutOfMemory
        code: 0i32
    }
end t.oom
```

`h1` を `remove` した後、同じスロットに `3u32` が入り、`h2 != h1`（世代が違う）。`a.get(h1)` は `None` で、古い取っ手は使い回しのスロットに当たらない。取っ手は `Copy` なので `Vec` に入れて回すこともでき、`T: linear` や参照を運ぶ型は `Arena` に入れられない。取っ手は（添字, 世代）だけを持つので、別の `Arena` の同じ（添字, 世代）の取っ手とは当たってしまう（型の違う取っ手は E2023）。

## 9. 集合と範囲走査（`HashSet` と `BtreeMap.range`）

`HashSet<T>` は `new`・`insert`・`remove`・`contains`・`len`・`is_empty`・`iter` を持ち、要素が `String` のときは `contains_str` / `remove_str` のように `&str` を渡す（`&String` で引くと E2054）。`BtreeMap<K, V>` は `iter` で鍵の順に走査し、`range(&lo, &hi)` で半開区間 `[lo, hi)` を、`first` / `last` で両端を取ります。

```kasane
fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut s = alloc.collections.HashSet.new(std.alloc.SYSTEM)
    core.check.eq(s.is_empty(), true)
    core.check.eq(s.insert(7u32), Ok(true))
    core.check.eq(s.insert(7u32), Ok(false))
    core.check.eq(s.len(), 2usize)
    core.check.eq(s.contains(&7u32), true)
    core.check.eq(s.remove(&7u32), true)
    core.check.eq(s.remove(&7u32), false)
    core.mem.drop(std.io.print_line("hashset ok"))
    Ok(())
end t.main
```

```kasane
fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut b = std.collections.BtreeMap.new()
    core.mem.drop(b.insert(1u32, 10u32))
    core.mem.drop(b.insert(2u32, 20u32))
    core.mem.drop(b.insert(3u32, 30u32))
    core.mem.drop(b.insert(5u32, 50u32))
    let mut sum = 0u32
    for (_k, v) in b.range(&2u32, &5u32) {
        sum = sum.wrapping_add(*v)
    }
    core.check.eq(sum, 50u32)
    match b.first() {
        Some((k, v)) => {
            core.check.eq(*k, 1u32)
            core.check.eq(*v, 10u32)
        }
        None => core.panic.panic("no first")
    }
    core.mem.drop(std.io.print_line("btree ok"))
    Ok(())
end t.main
```

## 10. C のコールバックにユーザーデータを渡す（`core.ffi.trampoline`）

C の API が `void (*cb)(void *user, int x)` のように `void *user` を受け渡す形なら、`extern` の両側で同じ生ポインタ型（`*mut t.Acc` など）を宣言し、Kasane 側は `core.ptr.from_mut(&mut acc)` でユーザデータへのポインタを渡します。キャプチャを持つクロージャを C の関数ポインタとして渡すには `core.ffi.trampoline(&mut cb)` を使い、戻り値の `(fn(*mut (), A) -> R, *mut ())` を C のコールバックと `user` に渡します（`spec/stdlib/core/ffi.sig`）。

```ksnl fragment
type t.Acc
    total: i32
    count: i32
end t.Acc

extern t.acc_ticks(f: fn(*mut (), i32) -> () uses [] panics never, user: *mut (), lo: i32, hi: i32) -> () link "LIB:acc_ticks"

extern t.acc_twice(f: fn(*mut (), i32) -> () uses [] panics never, user: *mut (), a: i32, b: i32) -> () link "LIB:acc_twice"

fn t.main() -> Result<(), std.io.Error> uses [io.console] panics may
    let mut acc = t.Acc { total: 0i32, count: 0i32 }
    let mut cb = fn(x: i32) -> () capture [&mut acc] {
        acc.total = acc.total.wrapping_add(x)
        acc.count = acc.count.wrapping_add(1i32)
    }
    let (f, user) = unsafe "ffi" "trampoline for the closure" {
        core.ffi.trampoline(&mut cb)
    }
    unsafe "ffi" "accumulate through the C callback" {
        t.acc_ticks(f, user, 1i32, 10i32)
    }
    core.check.eq(acc.total, 55i32)
    core.check.eq(acc.count, 10i32)
    unsafe "ffi" "two more calls" {
        t.acc_twice(f, user, 5i32, 7i32)
    }
    core.check.eq(acc.total, 67i32)
    core.check.eq(acc.count, 12i32)
    core.mem.drop(std.io.print_line("userdata ok"))?
    Ok(())
end t.main
```

リンクは `kasane.toml` の `[unsafe] modules = ["t"]`（8.1）と `link "LIB:acc_ticks"`（実行時の `dlopen`）が必要で、C 側は `acc_ticks` / `acc_twice`（利用者のデータは `user` を通じて受け渡すだけ）を共有ライブラリにする。

2 つの制限：
1. `extern` のコールバックの実引数にネイティブで渡せるのはプレーンな `fn`（名前付き関数、あるいは `capture []` のクロージャ）だけで、クロージャ値そのものを C の関数ポインタとして渡すことはできない（E5005）。キャプチャを持つクロージャを C に渡すには `core.ffi.trampoline` で関数ポインタと `*mut ()` のユーザデータペアを作る。
2. C が受け取った `user` ポインタを、`extern` の呼び出しから戻った後も保持・参照してはいけない（寿命は C への呼び出しの間のみ。必要ならヒープに逃がす）。
