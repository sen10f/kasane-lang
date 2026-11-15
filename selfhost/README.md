# セルフホスト予行 (修正プラン 5-5 の前段): Kasane で書いた Kasane の字句・構文解析器

2026-11-14。ブランチ `selfhost/parser-probe`。`stage0/src`・既存のテスト・`examples/`・`perf/`・`bench/` は変えていない (新規は `selfhost/` だけ)。

結論を先に: **字句解析器と構文解析器 (`spec/grammar.ebnf` の全体) は、Kasane で書いて、`ksn build --release` で LLVM のネイティブ実行ファイルになった。** 退避せず、E9040 も出ない。例・標準ライブラリ・参照解・`linear_ok`・`unsound` の 464 ファイルをすべて通し、Rust 版と定義一覧が 464 件一致した。木走査 (`ksn run`) との出力は同じで、全入力の処理は木走査 44.5 秒に対してネイティブ 0.04 秒。ネイティブを止めたものは 4 種類あり (下の 5 章)、どれも書き方の変更で回避できた。回避できない不足は、構文解析そのものではなく、その先のコンパイラ本体 (記号表・出力・外部コマンド) で現れる (6 章)。

## 1. 構成と行数

`selfhost/parse/` が ksn パッケージ (`kasane.toml` `kasane.lock`、`entry = "parse.main"`)。

| ファイル | 行数 | 中身 |
| --- | ---: | --- |
| `src/parse.ksnl` | 255 | 入口: 引数のファイルを読み、要約を出す。単体テスト 3 本 |
| `src/parse/err.ksnl` | 58 | エラー型 `Syntax(行, バイト位置, 符号, 期待)`・メッセージ |
| `src/parse/tok.ksnl` | 240 | トークン型・予約語/記号の符号表 (語表の文字列を `split` して引く) |
| `src/parse/lex.ksnl` | 406 | 字句解析 (Rust 版 `lexer.rs` と同じ規則: 改行の抑止、`.` 始まりの継続行、負リテラル、文字とライフタイムの区別) |
| `src/parse/ast.ksnl` | 335 | AST: ノードの配列 (`Node{kind,tok,a..f}`)。子は添字、可変長の子は `links` に並べた `List` ノード |
| `src/parse/cur.ksnl` | 189 | 構文解析器の状態 `P{ast,depth,pos,toks}` と、先読み・期待・FQN・入れ子の深さ制限 |
| `src/parse/ty.ksnl` | 385 | 型・型引数・効果集合・ジェネリクス・fn ヘッダの属性 |
| `src/parse/pat.ksnl` | 138 | リテラルとパターン |
| `src/parse/expr.ksnl` | 516 | 式・文・ブロック・`if`/`match`/`for`/`while`/`loop`/`with`/クロージャ/構造体リテラル |
| `src/parse/def.ksnl` | 326 | 定義 (`import const static extern type enum trait impl fn test`)、`end FQN` の照合、`.sig` |
| **Kasane 合計** | **2,848** | 154 関数。`panics may` は 0 (全部 `panics never`)。ただし本体の `+ - * /` と添字は使えず `wrapping_*` と `get` |
| `rust_summary.py` | 96 | Rust 版の要約 (`ksn defs` + `headers` + `bodies`) |
| `compare.py` | 250 | 比較・時間測定 |
| `broken/*.ksnl` | 20 本 | わざと壊した入力と `expected.txt` (正しいエラー行) |

`selfhost/probes/` は 6 章の不足を確かめた小さなパッケージ 5 つ (`hashmap` `strmatch` `boxtree` `btree` `vecstr`)。

AST の形 (要点): 再帰型を使わない。ノードは `Vec<Node>` に追記し、子は添字 (0 = なし。ノード 0 は `Nil`)。`Path` や定義の名前は元ソースのバイト範囲 `(a, b)` で持つ。定義の頭は 6 個までの子 (`add6`)。ノードの種類は 79 (`parse.ast.Kind`。式・文・パターン・型・定義・補助)。

## 2. 使い方

```
cd selfhost/parse
ksn build --release              # target/release/parse.native (冷えた状態で約 4 秒)
./target/release/parse.native FILE...     # または: ksn run FILE...  (木走査。引数は -- なしで渡る)
```

出力 (ファイルごと): `== パス`、定義ごとに `種別 FQN 行番号`、`nodes N`。解析できなければ `error パス:行:桁: 説明 ['期待したトークン']`。`--kinds` を先頭に付けると AST の種別ごとの個数も出す。

```
python3 -I compare.py [--mine native|interp] [--group examples|stdlib|linear_ok|bench|unsound|self|broken] [--kinds]
python3 -I rust_summary.py FILE...       # Rust 版の同形の要約
```

Rust 版に「全体を構文解析して要約を出す」コマンドはないので、既存の `ksn defs` (行頭の予約語からの定義の走査: 種別・名前・行)、`ksn headers` (fn/type/enum/trait の頭の構文解析)、`ksn bodies` (fn/test の本体の構文解析と S 式ダンプ) を組み合わせた。`stage0/src` は変えていない。

## 3. 正しさ

対象: `examples/*/src/**/*.ksnl` 27、`spec/stdlib/**/*.ksnl` 9、`stage0/tests/data/linear_ok/*.ksnl` 105、`bench/tasks/*/reference.ksnl` 19、`stage0/tests/data/unsound/*.ksnl` 304 (合計 464 ファイル、14,628 行、397,001 バイト、AST ノード 65,737)。

| グループ | 全体 | エラーなしで通った | Rust 版が通った | 定義一覧が一致 | 本体のノード種別の個数が一致 |
| --- | ---: | ---: | ---: | ---: | ---: |
| examples | 27 | 27 | 27 | 27 | 27 |
| stdlib (.ksnl) | 9 | 9 | 9 | 9 | 9 |
| linear_ok | 105 | 105 | 105 | 105 | 105 |
| bench 参照解 | 19 | 19 | 19 | 19 | 19 |
| unsound | 304 | 304 | 304 | 304 | 304 |
| **合計** | **464** | **464** | **464** | **464** | **464** |

- 「定義一覧が一致」= 全定義の `種別 FQN 行番号` が `ksn defs` と同じ。
- 「ノード種別の個数」= 各 fn/test 本体について、AST の 35 種 (`Bin Unary Call MCall Dot Index Try Lit Path StructLit TupleE ArrayE ArrayRep Closure If Match Loop While For Unsafe With Block Let Assign ExprStmt Break Continue Return PBind PLit PWild PPath PVariant PStruct PTuple PRef`) の個数が `ksn bodies` のダンプに出るノードの個数と同じ (`const`/`static` の初期化式は Rust 版が解析しないので数えない。`With` は Rust 版が `PBind` を作るので補正)。定義一覧だけでは本体の形が合っているか分からないので足した検査。これが 464 件とも一致したことが、構文解析の再現の根拠になる。
- 型・ジェネリクス・属性・フィールド・列挙子・トレイト・impl の頭は、個数の比較ができる出力が Rust 版にない。構造としては比べていない (Rust 版の `headers` が通る = 通る、までを確かめた)。
- `parse` 自身のソース 10 ファイル (2,812 行) も通り、Rust 版と定義・ノード種別とも一致 (`--group self`)。
- 正しいことの限界: 正規形 (インデント・空行・並び順) は検査しない (Rust 版の `bodies` と同じ寛容さ。それは `ksn fmt` / `grammar.rs` の担当)。数値リテラルの値の検査と文字列のエスケープの展開もしていない (Rust 版は `split_number` / `unescape` で検査する)。`///` ドキュメントは字句の段階で捨てる。

### 壊した入力 (`broken/`、20 本)

| 結果 | 件数 |
| --- | ---: |
| 想定したエラー行 (`expected.txt`。壊した場所から人が決めた) を報告 | **20 / 20** |
| Rust 版もエラーにして、行番号が同じ | 12 / 19 |
| Rust 版もエラーにして、行番号が違う | 7 |
| Rust 版は通してしまう | 1 (`toplevel_junk`: 行頭が予約語でない行を `ksn defs` が読み飛ばす) |

行番号が違う 7 件のうち 5 件 (`bad_char` `bad_escape` `tab` `unexpected_char` `unterminated_string`) は、Rust 版の字句エラーの行が**本体の中での相対行** (本体を別に字句解析するため。例: 2 行目の誤りを 1 と報告) で、こちらが正しい。`bad_enum` は Rust 版が位置なし (`0:0`)。`missing_end` は Rust 版が `end` の名前不一致 (6 行目) として報告し、こちらは `end` が無いまま次の `fn` に入った場所 (4 行目) を報告する。エラーには桁と期待したトークンも付く (例: `missing_paren.ksnl:2:19: expected ')'`)。

## 4. 実行 (診断・整形・テスト・実行・ビルド)

| コマンド | 結果 |
| --- | --- |
| `ksn check` | 診断 0 件 (0.2 秒) |
| `ksn fmt --check` | 通る |
| `ksn test --both` | 3 件通る。1 件 (`test_defs`) だけ `as_str` がネイティブに下りず、バンドルに退避 (テストの書き方の問題で、本体ではない) |
| `ksn run` (木走査) | 動く |
| `ksn build --release` | **ネイティブ生成 (LLVM) 成功**。退避なし。実行ファイル 129 KB、冷えた状態で 4.1 秒 |

出力の一致: 464 ファイルをまとめて `--kinds` 付きで解析した出力 (173,333 バイト) が、木走査とネイティブで**バイト単位で同一**。

### 時間 (全 464 ファイル、397 KB、14,628 行を 1 プロセスで)

| 実行 | 時間 | 最大メモリ |
| --- | ---: | ---: |
| 木走査 `ksn run` | **44.5 秒** (約 330 行/秒) | 38 MB |
| ネイティブ `parse.native` | **0.038 秒** (20 回平均、約 38 万行/秒) | 2.9 MB |
| 比 | 約 1,170 倍 | |

(参考: 比較用の Rust 版基準は 464 ファイル × 3 コマンドのプロセス起動を含めて 2.1 秒で、解析器そのものの速度ではない。)

入れ子の深さ: 式・ブロック・型・パターンの入れ子が 100 段 (`parse.cur.MAX_DEPTH`) を超えると `nesting too deep` を位置付きで返す (括弧 90 段は成功、400 段はエラー。木走査もネイティブも同じ)。木走査の呼び出し深さ上限 (約 2,500) の手前で止めるための値で、1 段あたり約 15 フレーム使う。

## 5. ネイティブにならなかった形と回避策

`ksn build --release` は最初に見つかった 1 件の理由しか出さないので、直しては再ビルドを 4 回繰り返した。回避した書き方は次のとおり。

| # | 下りなかった形 | `ksn build` の理由 | 回避した書き方 |
| --- | --- | --- | --- |
| 1 | `const` の `&str` に直接メソッド: `parse.tok.OPS1.as_bytes().contains(&c)`、`parse.tok.SUFFIXES.split(" ")` | `呼び出し parse.tok.OPS1.as_bytes は段階 1 の対象外` | ローカルに受ける: `let ops = parse.tok.OPS1` → `ops.as_bytes()` |
| 2 | `&構造体` の参照外し: `match all.last() { Some(t) => *t }`、`Some(n) => *n` (Copy の `Tok` `Node`) | `* の被演算子が参照でない` | フィールドから作り直す: `parse.tok.new(t.kind, t.code, ..)`、`parse.ast.copy(n)` (全フィールドを書く) |
| 3 | `for n in p.ast.nodes.as_slice().iter()` (構造体の Vec を、フィールド経由で) | `for の対象は core.iter.range(a, b) か、要素型の分かる Vec の変数 ... だけ` | 添字: `for at in core.iter.range(0usize, n)` → `parse.ast.get(&p.ast, at)`。(同じ書き方でも要素が `usize` の `p.ast.defs.as_slice().iter()` は下りた) |
| 4 | `text.as_str().starts_with(..)` (`match` の束縛 `text: String` に対して、テスト内) | `メソッド as_str は段階 1 の対象外` | 本体ではなくテストだけ。バンドル退避のまま (本体では使っていない) |

以上の 4 つを直した後は、154 関数すべてが下りた。なお、次は**書かなくて済ませた**ので、下りるかは未確認 (1 件は 6 章): `&'a str` を持つ構造体 (トークンの文字列は `start`/`stop` のバイト位置で持った。`修正プラン.md` C が「退避する形」に挙げている)、文字列リテラルの `match` (6 章 2)、`String` をキーにした `HashMap` (6 章 1)。エラーの文字列を持つ `enum` (`Msg(usize, &'static str)`) は小さな試験でネイティブに下りたが、本体では符号 (`u32`) と `message(code)` の `match` にした。

## 6. セルフホストに向けて処理系に必要なこと (重要度順)

解析器そのものはネイティブで動いたので、ここは「Kasane で Kasane の検査器・コンパイラまで書く」ときに当たるものの一覧。`selfhost/probes/` で確かめたものと、署名 (`spec/stdlib/**/*.sig`) から読んだものを分けて書く。

1. **外部コマンドの実行がない** (署名: `std.process` は `exit` と `abort` だけ)。コンパイラが生成した IR を `clang`/`ld` に渡せないので、ドライバを Kasane で書けない。子プロセスの起動・待機・標準入出力のパイプ・環境変数の受け渡しが要る。(未試験: 署名から)
2. **`HashMap<String, V>` がネイティブに下りない** (試験 `probes/hashmap`: `HashMap のキーの型 agg#5 は段階 1 の対象外 (整数・bool・char・&str だけ)`)。記号表・名前解決・文字列の内部化に必須。`BtreeMap<u32, u32>` は下りた (`probes/btree`)。`String` の `<` / `cmp` もネイティブ非対応 (`チートシート.md` の記載どおり) なので `BtreeMap<String, _>` も同様のはず (未試験)。`&str` キーの `HashMap` は署名上は対象だが、`&'a str` の寿命を持ち回る構造体が退避するため使いにくい。
3. **文字列リテラルの `match` がネイティブに下りない** (試験 `probes/strmatch`: `リテラルのパターン str は段階 1 の対象外`)。キーワード・組み込み名の分岐が書けず、この予行では語表の文字列を `split` して位置を数える形にした (関数 1 本 8 行。数十個の名前を持つコンパイラでは遅く、読みにくい)。
4. **ファイルシステムが足りない** (署名: `std.fs` は `open create exists read_to_end remove_file` だけ)。ディレクトリの列挙 (`src/**/*.ksnl` を集める)、`create_dir`、`rename`、メタデータ、パス操作 (結合・拡張子・親) がない。`修正プラン.md` D の「`std.fs` に追記・リネーム・ディレクトリ操作がない」と同じ。
5. **整列・検索がない** (署名に `sort` / `sort_by` / `binary_search` / `dedup` なし。`Vec` に `extend` / `append` / `contains` / `iter().collect()` もない)。`ksn fmt` が定義を UTF-8 バイト順に並べ直すので、コンパイラには整列が必須。自前で書くと `swap` (`core.slice.swap` はある) とループになる。(未試験: 署名から)
6. **`enum` の `match` に `_` が使えない** (言語の決定。`E2021`)。トークンや AST の種別が 79 あると、`kind_name` `kind_index` のような全列挙の関数を 1 つ書くたびに 79 行になる (ここでは `ast.ksnl` 335 行のうち `kind_name` と `kind_index` で約 160 行。スクリプトで生成した)。種別の分岐は `u32` の符号と `==` の連鎖で書いたが、型の安全を捨てている。`matches!` 相当か、ある値だけ見る `if let` / `Kind.X == k` の下ろしがあると楽 (`==` は下りた)。
7. **再帰する型に `Drop` の明示実装が要る** (試験 `probes/boxtree`: `E4013 recursive type requires an explicit core.ops.Drop impl`)。AST は配列と添字で書けたが、`Box` の木を書く道が重い。派生 (`derive [Drop]` 相当) があると、セルフホストする検査器の型・IR を素直に書ける。この予行は添字方式なので影響を受けなかった。
8. **`stack N` は 1 関数の枠の見積りを検査するだけ**: `estimated stack use 1728 exceeds declared 16` (`E4012`)。見積りは関数ごと (最大約 2 KB) で、再帰の総量は見ない。宣言値が各関数の見積り以上ならよく、実際の再帰の深さは検査されない。実際に落ちないよう `MAX_DEPTH` を自前で持った。再帰下降は深さを制限するのが普通だが、メインスレッドの実スタックを選べる手段 (`std.thread` の大きなスタック) も要る。
9. **木走査 (`ksn run`) が遅い**: 同じ解析が約 1,170 倍遅い (44.5 秒 対 0.04 秒)。Rust の `ksn` で stage1 をネイティブに作る道 (stage0 → stage1) は成立するが、Kasane で書いたコンパイラを木走査で回して自分自身を処理する (stage1 を `ksn run` で動かす) のは、解析だけで約 2 万行/分なので、検査まで含めると現実的でない。5-5 の「stage1 と stage2 の出力が一致」には、stage1 のネイティブ生成が前提。
10. **`Copy` の構造体の参照外しと、構造体の `Vec` の `for` がフィールド経由だと下りない** (5 章 2・3)。いずれも回避は簡単 (コードが数行ふえる) だが、AST/IR を扱うコードでは毎回当たる形なので、下ろしを足す価値がある。`const` の `&str` へのメソッド (5 章 1) も同様。
11. **標準出力**: `std.io.print_line` だけが使いやすい。改行なしの出力・整形 (`alloc.fmt` はある) で組み立てて 1 回で書く形になる (ここでは `String` に溜めて 1 ファイル 1 回の `print_line`)。大量出力の性能は未計測。
12. **言語・ツールの摩擦** (処理系の不足ではないが、書いた感触): `end` が予約語で FQN の成分にも使えず `parse.def.import` が通らない (成分に予約語が入る `import` `end` ほか)、100 文字超の本体行は `ksn fmt` が折らず `check` が致命的な E0002 (長い `||`/`&&` の連鎖と構造体リテラルは手で `let` に分ける。十数箇所)、他モジュールから見えるものは全部 `pub` が要る、確保が失敗しうるので `push` のたびに `?` と `From` 付きのエラー型が要る。どれも `ksn fmt`/`check` の指摘で素早く直せた。

## 7. 未確認のこと

- ネイティブの試験は、構文解析という「`Vec` と整数と `enum` が中心で、文字列の処理が少ない」プログラムのもの。型検査・単相化・コード生成まで含めたときに、下りない形がどれだけ増えるかは分からない (6 章 1〜3・5 は、その手前の兆候)。
- AST は Rust 版の `V` の S 式とは別の形 (ノードの配列)。型・ヘッダ側の構造の一致は個数でも比べていない。
- 並行実行・`unsafe`・FFI・`dyn` は使っていない。
- 10 万行級の入力での性能・メモリは測っていない (最大でも 1 ファイル数百行)。
- 全 464 ファイルは「正しいはずのもの」で、`stage0/tests/data/diag/` (わざと誤らせた診断用の入力) は対象外。

## 8. 再現

```
. ~/.cargo/env
cd stage0 && cargo build --release && cd ..
cd selfhost/parse
ksn check && ksn fmt --check && ksn test --both
ksn build --release
python3 -I compare.py --kinds                       # 464 件の正しさ (ネイティブ)
python3 -I compare.py --group broken                # 壊した入力
python3 -I compare.py --mine interp --kinds         # 木走査 (約 45 秒 + 基準の 2 秒)
```

## 9. 名前解決の段 (修正プラン 5-5 の次の段)

2026-11-15。ブランチ `selfhost/resolve`。新規は `selfhost/resolve/` と、この節の追記だけ (`stage0/`・`examples/`・`spec/`・`apps/`・`bench/`・`perf/` は変えていない)。

結論を先に: **名前解決と定義の表は Kasane で書けて、`ksn build --release` で退避なしにネイティブになった。** 構文解析は `selfhost/parse` を `path` 依存 (`kasane.toml` の `[dependencies]`) で使い、写していない。定義の表は Rust 版 `ksn defs` と **113 ファイル・2,329 件すべて一致** (例・apps・parse・標準ライブラリ)。診断は、正常な入力では両方 0 件で一致し、わざと未定義の名前を入れた入力では **12 件中 7 件が一致** (残りは Rust 版の取りこぼし 3 件と、行の決め方の違い 2 件。下の 9.5 章)。

### 9.1 構成と行数

`selfhost/resolve/` が ksn パッケージ (`entry = "resolve.main"`、依存 `parse = { path = "../parse" }`)。

| ファイル | 行数 | 中身 |
| --- | ---: | --- |
| `src/resolve.ksnl` | 97 | 入口。読み込み → 表 → 本体の解決 → 集計。`Failure` 型 |
| `src/resolve/manifest.ksnl` | 91 | `kasane.toml` の `[package] name` と `[dependencies]` の `path` 依存だけ読む |
| `src/resolve/load.ksnl` | 70 | ルートの下の `.ksnl` / `.sig` を再帰で集め、パスからモジュール名を決める (`a/b.ksnl` → `a.b`) |
| `src/resolve/table.ksnl` | 134 | 定義の表 (FQN → 種別・ファイル・行・pub)。列挙子は別の表。重複は E1103 |
| `src/resolve/scope.ksnl` | 240 | 名前の解決 (3.3 の順: 局所 → ジェネリクス → import の末尾名 → FQN)。`値.メソッド` の接頭辞判定 |
| `src/resolve/walk.ksnl` | 492 | ヘッダと本体の走査 (型・パターン・式・文・閉包・`with`・`match`) |
| `src/resolve/report.ksnl` | 140 | 出力 (`種別 FQN 行` と診断 1 行) |
| `src/resolve/ast.ksnl` | 54 | List の読み出し、バイト範囲の文字列 |
| `compare.py` | 220 | Kasane 版と Rust 版の突き合わせ (定義・診断・時間) |
| `broken/undef/` | 2 ファイル | わざと未定義の名前を入れた入力 (Rust 版と比べる) |

Kasane 合計 1,318 行。テスト 2 本 (`ksn test`: `prefix` の規則)。`ksn check` / `ksn fmt --check` は通る。

使い方:

```
cd selfhost/resolve
ksn build --release                                   # target/release/resolve.native (退避なし)
./target/release/resolve.native PKG_DIR STD_DIR       # 例: ./target/release/resolve.native ../../examples/demo ../../spec/stdlib
python3 -I compare.py [--group examples|apps|parse|broken|stdlib|all] [--mine interp] [-v]
```

出力は自パッケージの各ファイルの `種別 FQN 行` (ksn defs と同じ並び)、`error コード パス:行: 説明 名前` (診断)、末尾の `summary refs N errors M files K`。

### 9.2 処理の順序

1. `kasane.toml` を読む。依存は `path` のものだけ (git 依存は読まない。読まれないので、そこから来る名前は E1101 になる)。
2. 自パッケージの `src/`、直接の path 依存の `src/`、`spec/stdlib` を集める。モジュール名はパスから決める。
3. 全ファイルを構文解析し、定義と列挙子を表に入れる。`.sig` (宣言) と `.ksnl` (実装) の同名は重複にしない (実装の方を残す)。
4. 自パッケージのファイルについて、定義の一覧を出し、ヘッダ (ジェネリクス・引数・戻り値・フィールド・トレイトの頭) と本体の名前を 3.3 の順に解く。
   - 値の位置 (E1101): 局所・引数・閉包のキャプチャ・構造体リテラル (`unknown struct`)・パターン (`unresolved pattern`)。
   - 型の位置 (E1102)、トレイト (E1104)、別モジュールの非 pub (E1119)、型名を値として使う (E1109)。
   - 閉包の本体には、キャプチャした名前と引数だけが見える (外側の束縛は見えない)。
   - 大文字で始まる単独のパターン名 (`Red`) は束縛ではなく、列挙子・定数として解く。小文字は束縛。
5. `値.メソッド` (`threads.TOTAL.lock`、`realtime.Phase.Idle.next`) は、接頭辞が列挙子・定数・静的なら残りはメソッドとして受ける (型検査はしない)。

### 9.3 確かめ

| 対象 | 結果 |
| --- | --- |
| 定義の表 (`ksn defs`) | `examples/*` 27 ファイル (15 パッケージ)・`apps/*` 22 ファイル (3 パッケージ)・`selfhost/parse` 10・標準ライブラリ 52 ファイル、**113 ファイル・2,329 件すべて一致** (`import`・`impl` 行を含む)。`.sig` は `ksn defs` が読めない (空を返す) ので `ksn headers` の種別と名前で比べた (行は比べていない) |
| 正常な入力の診断 | 同じ 113 ファイルで Rust 版も Kasane 版も 0 件 |
| 未定義の名前 (`broken/undef/`) | Kasane 版 11 件、Rust 版 8 件。(コード, 行) の多重集合で **12 件中 7 件一致** |
| 木走査 (`ksn run`) との出力 | `apps/todo`・`selfhost/parse`・`examples/shortener` で、ネイティブと**バイト単位で同一** (`diff` で確認) |
| 時間 (ネイティブ / 木走査) | `apps/todo` 0.05 秒 / 61 秒、`selfhost/parse` 0.05 秒 / 53 秒、`examples/shortener` 0.03 秒 / 37 秒 (約 1,000 倍以上)。木走査の大半は標準ライブラリ 52 ファイルの読み込み |
| 全部まとめて (26 パッケージ + 標準ライブラリ) | ネイティブ 0.63 秒 (プロセス起動を含む) |

### 9.4 ネイティブにならなかった形と回避策

`ksn build --release` は最初の 1 件しか言わないので、直しては再ビルドを繰り返した。

| # | 下りなかった形 | `ksn build` の理由 | 回避した書き方 |
| --- | --- | --- | --- |
| 1 | `HashMap<String, 構造体>` (値が構造体) | `HashMap の値の型 agg#8 は段階 1 の対象外 (整数・bool・char だけ)` | 値を添字 (`usize`) にし、本体は `Vec<Def>` に入れる (`table.ksnl`) |
| 2 | フィールド越しの `HashMap` の操作: `t.m.insert(k, v)` / `m.m.get(k)` (`t: &mut 構造体`) | `メソッド insert は段階 1 の対象外 (利用者の型のメソッド呼び出しは未対応。&mut の受け手は参照の対応が要る)` (`get` も同じ) | 局所の参照に取ってから呼ぶ: `let mm = &mut t.defs.m` → `mm.insert(..)`、`let dm = &t.defs.m` → `dm.get(..)` |
| 3 | フィールド経由の `for`: `for d in man.deps.as_slice().iter()` (`man` は局所の構造体、要素は構造体) | `for の対象は core.iter.range(a, b) か、要素型の分かる Vec の変数 ... だけ` | 局所に取り出す (`let deps = man.deps`)。`self.gens` (`String` の Vec) も同じ書き方を避け、添字で回す: `for k in core.iter.range(0usize, self.gens.len())` と `get(k)` |
| 4 | `Copy` 構造体の参照外し `*d` (`d: &Def`) | 踏んでいない。前段 (5 章 2) で `* の被演算子が参照でない` を確認済み。この段では予防的に避けた | フィールドから作り直す (`resolve.table.copy`)。`*ix` (usize) は下りた |
| 5 | 文字列 API の `rfind` | `メソッド rfind は段階 1 の対象外 (戻り値が Option / Result / 参照のため)` | `split(".")` を回して接頭辞を組む (`resolve.scope.prefix`)。`find` / `split_once` / `split_at` / `strip_*` / `trim*` は下りた |
| 6 | `match` の arm の本体に `""` を直接書く | ksn check `E0057` (arm の書式の検査) | 変数を通す (`let bad = "?"` を `bad` で返す) |

### 9.5 Rust 版の取りこぼし (検査。処理系は直していない)

`broken/undef/` で Kasane 版と比べて見つけた。`stage0/tests/data/unsound/` に 1 件 1 ケースで足す候補。

| 入力 | Rust 版 | 期待 (Kasane 版 / 仕様) |
| --- | --- | --- |
| 本体の `let q: Foo = 1i32` (未定義の型の注釈) | 何も出さない | E1102 |
| 閉包 `fn(x: i32) -> i32 capture [&zz] { x }` (外側にない名前のキャプチャ) | 何も出さない | E1101 |
| 列挙子の不在 `undef.other.E.Nope` | E2044 (型が決まらない) だけ | E1101 |
| パターンの未解決名 (`Nope.X(v)`) の行 | 関数の先頭行 (本体の 0 行扱い) | パターンの行 (Kasane 版はこちら) |

### 9.6 処理系の不足 (重要度順)

1. **`HashMap` が段階 1 で使いにくい** (9.4 の 1・2)。値は整数・bool・char だけで、フィールド越しの `insert` / `get` が下りない。記号表は「名前 → 定義」の表そのものなので、ここが最大の摩擦。`HashMap<String, 構造体>` が下りれば、添字と `Vec` の二段は要らない。書き換えで回避できたが、コンパイラ本体の記号表・型の表でも同じ形が出ると見込まれる。
2. **参照越しの操作の下ろし** (9.4 の 3・4)。フィールド経由の `for` (要素が構造体や `String` の Vec) が下りない。`Copy` 構造体の参照外し `*d` も前段で下りなかった (5 章 2)。どちらも局所への取り出し・添字・作り直しで回避できるが、AST・IR を扱うコードで毎回当たる形。`self.field` の要素を参照で回す書き方を下ろせると楽になる。
3. **文字列 API が狭い** (9.4 の 5)。`rfind` は段階 1 の対象外 (E9040 で確認)。文字列の対応表 (`stage0/src/kir_lower_str.rs`) に載っているのは `split_at` `find` `split_once` `strip_prefix/suffix` `trim*` `starts_with` `ends_with` `eq` `eq_ignore_ascii_case` だけ。ほかの Option を返す API は確かめていない。名前の処理 (`a.b.c` を成分に分ける、末尾を外す) で毎回要る。
4. (続き・摩擦) `ksn check` の書式規則が厳しい: 構造体リテラルは 1 行 (`E0053`)、1 行 100 字 (`E0055`)。`alloc.collections.HashMap<alloc.string.String<std.alloc.System>, V, std.alloc.System>` は 1 行に入らないので、`type Map[V]` で包んだ。`ksn fmt` が定義の並びを直す (`E0020`) のを毎回回す必要がある。
5. (続き) 検査の取りこぼし (9.5)。どれも通すべきでないコードが `ksn check` を通る。
6. (続き) `ksn defs` は `.sig` を読めない (空を返し、エラーも出さない)。宣言の比較には `ksn headers` を使う。
7. (続き) ネイティブ生成の失敗が 1 件ずつしか出ない (5 章と同じ)。`ksn build` の全件表示があると、直す回数が減る。

### 9.7 まだ書いていないこと (名前解決の範囲外・未実装)

- 検査: E1020 (未使用の import・変数)、E1021 (シャドーイング)、E1105 (モジュールの循環)、E1111、E1116 (`test` の対象)、E1120・E1121 (`tests/`)、E1122 (フィールドの pub)、E1112・E1113 (`impls` の対応)。`tests/` の統合テストも読んでいない。
- メソッド呼び出し (`x.m()`) の解決、トレイト境界、型の中身 (型引数の個数、効果)、`uses` / `panics` の整合。
- 基本型 (`i32` など) の先頭の名前は、種類だけ見て中身 (`i32.MAX` の `MAX`) は確かめていない。
- 依存は直接の `path` だけ (推移的な依存は読まない。git 依存は読み飛ばす)。
- 診断の文言は Rust 版と違う (コードと行だけを比べた)。
- 解決した参照の一覧 (`ksn resolve` 相当) は出していない。数 (`refs`) だけ。

### 9.8 再現

```
. ~/.cargo/env
cd stage0 && cargo build --release && cd ..
cd selfhost/resolve
ksn check && ksn fmt --check && ksn test
ksn build --release
python3 -I compare.py --group all          # 定義 113 ファイル・診断 (正常 0 件、broken 7/12)
python3 -I compare.py --group broken -v    # 差の中身
```
