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

fn wordcount.add_word(counts: &mut alloc.collections.HashMap<alloc.string.String<std.alloc.System>, usize, std.alloc.System>, word: &str) -> Result<(), wordcount.Error> uses [alloc] panics never
    match counts.get_mut_str(word) {
        Some(count) => {
            *count = count.wrapping_add(1usize)
            Ok(())
        }
        None => {
            let key = alloc.string.String.from_str(std.alloc.SYSTEM, word)?
            match counts.insert(key, 1usize) {
                Ok(_old) => Ok(())
                Err(error) => Err(wordcount.Error.Alloc(error))
            }
        }
    }
end wordcount.add_word

fn wordcount.count_text(text: &str) -> Result<alloc.collections.HashMap<alloc.string.String<std.alloc.System>, usize, std.alloc.System>, wordcount.Error> uses [alloc] panics never
    let mut counts = alloc.collections.HashMap.new(std.alloc.SYSTEM)
    let mut word = alloc.string.String.new(std.alloc.SYSTEM)
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
    let mut line = alloc.string.String.new(std.alloc.SYSTEM)
    line.push_str("unique words: ")?
    line.push_u64(u64.from_usize(counts.len()))?
    std.io.print_line(line.as_str())?
    Ok(())
end wordcount.main

fn wordcount.print_count(counts: &alloc.collections.HashMap<alloc.string.String<std.alloc.System>, usize, std.alloc.System>, word: &str) -> Result<(), wordcount.Error> uses [alloc, io.console] panics never
    let count = match counts.get_str(word) {
        Some(value) => *value
        None => 0usize
    }
    let mut line = alloc.string.String.new(std.alloc.SYSTEM)
    line.push_str(word)?
    line.push_str(": ")?
    line.push_u64(u64.from_usize(count))?
    std.io.print_line(line.as_str())?
    Ok(())
end wordcount.print_count

fn wordcount.read_file(path: &str) -> Result<alloc.string.String<std.alloc.System>, wordcount.Error> uses [alloc, io.fs] panics never
    let mut data = alloc.vec.Vec.new(std.alloc.SYSTEM)
    with mut file = std.fs.open(path)? close core.mem.drop(file.close()) {
        core.mem.drop(std.fs.read_to_end(&mut file, &mut data)?)
    }
    let mut text = alloc.string.String.new(std.alloc.SYSTEM)
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

集計表の型は `HashMap<String, usize>` です。キーは所有する `String` として保存します。検索だけは所有値を 새로作らない `get_str`、更新は `get_mut_str` を使います。`String` キーの辞書で `get` や `get_mut` を直接使う書き方は認められていません。

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

fn httpserver.address(port: u16) -> Result<alloc.string.String<std.alloc.System>, httpserver.Error> uses [alloc] panics never
    let mut address = alloc.string.String.new(std.alloc.SYSTEM)
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

`&T` と `&mut T` は、既存の値を短時間だけ借りる二級の値です。関数へ渡したり、局所変数に一時的に保持することはできますが、型の欄、コンテナ、チャネルへ格納できません。长期保存したい値には、所有する `String` などを使います。上記の `HashMap` が `String` をキーにするのも、この規則によるものです。

### 効果と panic の宣言は本文と完全一致させる

`uses [...]` は I/O や確保などの効果を表し、`panics ...` は panic の可否を表します。宣言は本文と完全に一致する必要があり、宣言も検査の対象です。省略や過大な宣言も誤りになります。本文から一意に決まる場合、`ksn fix` が `uses` と `panics` の宣言を補完・書き換えできます。

### 整数リテラルには型を表す接尾辞を付ける

整数リテラルの型は `1u32`、`1u64`、`1usize`、`200u16` のように接尾辞で明示します。型が推論できる整数リテラルで接尾辞を省略すると、`ksn check` は E2028 を出します。推論できる場合は `ksn fix` が接尾辞を補えます。`ksn fmt` は 5 桁以上の 10 進数を 3 桁ずつ `_` で区切ります。

### 線形値は必ず消費する

線形値（`linear` と宣言した型や、線形値を含む型）は、消費せずスコープを抜けると誤りになります。消費するには、`self` を値で受け取る関数へ渡す、分解して値を受け取る、所有者の `with` に後始末として渡すなどします。`core.mem.drop` は非線形の値専用なので、線形値の消費には使いません。

### `ksn fmt` と `ksn check` を 함께使う

`ksn fmt` は書き方の揺れを直し、`ksn check` は意味の誤りを検査します。`ksn fmt --check` は、正規形でなければ E0020 を出します。新しく書いたコードは、まず `ksn fmt`、次に `ksn check`、最後に `ksn run` の順で確認してください。

## 4. `Rc` / `Arc` を使わずにデータを共有する（`alloc.arena` と `Handle<T>`）

Kasane には `Rc` / `Arc` はありません。複数の場所から同じ値を共有したいときは `alloc.arena.Arena<T, A>` に入れ、取っ手 `Handle<T>`（添字と世代だけを持つ `Copy` の値で、`Arena` の側しか作れない）を渡します。要素は 1 本のバッファのスロットに置かれ、`grow` でも添字は動かないので、値を借りている間も取っ手は有効です。

`insert` で取っ手を作り、`get` / `get_mut` で借り、`remove` で捨てます。`remove` した取っ手は「弱い参照」と同じで `get` は `None` を返します。スロットが再利用されると世代が 1 つ上がるので、古い取っ手も当たらず、後の `insert` で作った取っ手と区別されます。

```kasane
fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut a = alloc.arena.Arena.new(std.alloc.SYSTEM)
    core.check.is_true(a.is_empty())
    core.check.eq(a.len(), 0usize)
    let h0 = a.insert(1u32)?
    let h1 = a.insert(2u32)?
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
    let h2 = a.insert(3u32)?
    core.check.is_true(h2 != h1)
    core.check.eq(a.get(h2), Some(&3u32))
    core.mem.drop(std.io.print_line("arena ok"))?
    Ok(())
end t.main
```

`h1` を `remove` した後、同じスロットに `3u32` が入り、`h2 != h1`（世代が違う）。`a.get(h1)` は `None` で、古い取っ手は使い回しのスロットに当たらない。取っ手は `Copy` なので `Vec` に入れて回すこともでき、`T: linear` や参照を運ぶ型は `Arena` に入れられない。取っ手は（添字, 世代）だけを持つので、別の `Arena` の同じ（添字, 世代）の取っ手とは当たってしまう（型の違う取っ手は E2023）。

## 5. 集合と範囲走査（`HashSet` と `BtreeMap.range`）

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
    core.mem.drop(std.io.print_line("hashset ok"))?
    Ok(())
end t.main
```

```kasane
fn t.main() -> Result<(), std.io.Error> uses [alloc, io.console] panics may
    let mut b = alloc.collections.BtreeMap.new(std.alloc.SYSTEM)
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
    core.mem.drop(std.io.print_line("btree ok"))?
    Ok(())
end t.main
```

## 6. C のコールバックにユーザーデータを渡す（`core.ffi.trampoline`）

C の API が `void (*cb)(void *user, int x)` のように `void *user` を受け渡す形なら、`extern` の両側で同じ生ポインタ型（`*mut t.Acc` など）を宣言し、Kasane 側は `core.ptr.from_mut(&mut acc)` でユーザデータへのポインタを渡します。キャプチャを持つクロージャを C の関数ポインタとして渡すには `core.ffi.trampoline(&mut cb)` を使い、戻り値の `(fn(*mut (), A) -> R, *mut ())` を C のコールバックと `user` に渡します（`spec/stdlib/core/ffi.sig`）。

```kasane
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
