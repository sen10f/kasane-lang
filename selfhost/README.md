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

## 10. 効果の検査の段 (修正プラン 5-5 の次の段)

2026-11-15。ブランチ `selfhost/effects`。新規は `selfhost/effects/` と、この節の追記だけ (`stage0/`・`examples/`・`spec/`・`apps/`・`bench/`・`perf/` と `selfhost/` の他の部品は変えていない)。

結論を先に: **効果 (E4001)・panic (E4002)・添字 (E4003)・整数演算 (E4004)・再帰の stack (E4010) の検査は、型を見ずに Kasane で書けて、`ksn build --release` で退避なしにネイティブになった** (ELF、終了コード 0、E9040 なし)。自分自身 (+ parse・resolve・標準ライブラリ) の検査は 0.09 秒。Rust 版と比べると、効果・panic の分野の 58 件 (Rust 版が E4001〜E4010 で拒否する unsound) のうち **判断できたのは 0 件**。残りは型が要る形 (55 件) と、関数項目の型注釈への変換 (3 件、型の段が要る) で、名前だけの検査は一致も余分も出さない。診断が出た形 (直接の呼び出し・添字・整数の演算・unwrap・相互再帰) は Rust 版と一致した。

### 10.1 構成と行数

`selfhost/effects/` が ksn パッケージ (`kasane.toml` `kasane.lock`、`entry = "effects.main"`、依存 `parse` と `resolve` を `path` で)。

| ファイル | 行数 | 中身 |
| --- | ---: | --- |
| `src/effects.ksnl` | 99 | 入口: 読み込み → 解析と表 → 関数の表 → 本体の走査 → 再帰 (E4010) → 集計 |
| `src/effects/fns.ksnl` | 128 | 関数の表: FnDef ごとの行・本体・panic (never=1/may=2/変数=3)・`stack` の有無・uses (空白区切り) と FQN → 番号 |
| `src/effects/walk.ksnl` | 542 | 本体の走査 (局所束縛と名前解決は `resolve.walk` と同じ並び)、呼び出しの検査、添字・整数演算・unwrap、辺の収集 |
| `src/effects/run.ksnl` | 58 | 関数 1 本の走査の入口、E4010 の報告、末尾の集計 |
| `compare.py` | 195 | Rust 版 `ksn check --json` との比較 (`clean` `diag` `unsound` `all`) |
| **合計** | **827 (Kasane) + 195 (Python)** | |

### 10.2 検査の中身

- **E4001**: 呼び出しの先の宣言 `uses` の各要素が、呼び出し元の `uses` に覆われるか (5.1 の階層: `io` は `io.fs` を覆う)。先の宣言が無い (推論される) ときは判断できない。
- **E4002**: `panics never` の関数から `panics may` の関数を呼ぶ。`.unwrap()` は `Option`・`Result` の `unwrap` が `panics may` なので同じ扱い (利用者が `panics never` の `unwrap` を定義していれば判断できない)。
- **E4003**: 添字 (`panics never` の関数の `a[i]`)。Rust 版の番号に合わせた (修正プランの「E4004」は整数演算)。
- **E4004**: `+ - * / %` の一方が接尾辞つきの整数リテラル (`1u32`) のとき。リテラルが無い二項演算は型が要るので判断できない。
- **E4010**: 自パッケージの `panics never`・`stack` なし関数について、呼び出しの辺 (解決できた `f(..)` の呼び出し、依存の本体も含む) で自分に戻るかを幅優先で見る (強連結成分に入るかと同じ)。戻るなら E4010。
- 名前は `resolve.scope.find` (3.3 の順: 局所 → ジェネリクス → import → FQN) で解く。局所変数の束縛は `resolve.walk` と同じ規則で押す。閉包の本体は、呼び出しを「閉包の中」として数えるだけで検査しない。

### 10.3 確かめ

| 対象 | 件数 | 結果 |
| --- | ---: | --- |
| `examples/*`・`apps/*`・`selfhost/{parse,resolve,effects}` (パッケージ) | 21 | Kasane 版 0 件、Rust 版 0 件。不一致 0。判断できない形は合計 (下の 10.4) |
| `stage0/tests/data/linear_ok/*.ksnl` | 118 | 0 件で一致 118 |
| `stage0/tests/data/diag/E4001〜E4010*` (1 ファイル 1 ケース) | 13 | **一致 4** (E4001 E4002 E4003 E4010) / 判断できない 9 / 取りこぼし 0 / 余分 0 |
| `stage0/tests/data/unsound/*` (Rust 版が E4001〜E4010 で拒否するもの) | 58 / 408 | **一致 0** / 判断できない 55 / 取りこぼし 3 / 余分 0 |

- 「一致」= Kasane 版の E40 の集合が Rust 版と同じ。「判断できない」= 一致せず、型が要る形を数えた。「取りこぼし」= 一致せず、数えたものも無い。「余分」= Rust 版に無いコードを Kasane 版が出した。
- 判断できなかった 9 件 (diag): メソッドと `dyn` 越しの呼び出し (`E4001b` `E4002b` `E4002c` `E4002e` `E4001d` `E4010b`)、`Fn` 境界に渡した閉包 (`E4001c` `E4002d` `E4010c`)。
- unsound の取りこぼし 3 件 (`fn_conv_option_decl` `fn_conv_return_decl` `fn_type_return`): 関数項目や関数型の値を、効果の小さい関数型へ渡す変換 (戻り値 `-> fn() uses []` に `t.loud` を返す、`Some(l)` の中の `l`)。関数型の注釈と値の照合が要るので、名前だけでは出ない (10.5 の 1)。
- 追加の確かめ (リポジトリには入れていない小さな入力): 相互再帰 `a ⇄ b` (E4010 ×2)、`stack` 付きの再帰 (出ない)、`n + 1u32` (E4004)、`xs[0usize]` (E4003)、`v.unwrap()` (E4002) を Rust 版と比べて、E4010・E4003・E4004 は一致、`unwrap` は Rust 版の `core.option.Option.unwrap` の E4002 と一致。

### 10.4 判断できない形 (21 パッケージの合計)

| 形 | 件数 | 理由 |
| --- | ---: | --- |
| メソッド呼び出し (`x.m()`、局所の `v.m()` を含む) | 1845 | レシーバの型が要る |
| 再帰の候補 (`panics never`・stack なし) で未確定の呼び出しを含む | 341 | 辺が張れない呼び出し (メソッド・閉包) が自分に戻るかもしれない (重なるので合計には入れない) |
| 解決できない名前 | 127 | 基本型の関数 (`u64.from_usize` 等、標準の宣言なし) と `extern` 宣言 (表に入れていない) |
| 推論された宣言 (`uses` を省略した非 pub 関数) | 35 | 本体からの推論は未実装 |
| 効果・panic の変数 (`E: uses` `P: panics`) | 14 | 実体化が要る |
| 閉包の中の呼び出し | 10 | 閉包の効果は呼び出されるまで決まらない |
| 整数演算 (リテラルなし) | 2 | 型が要る |

### 10.5 処理系の不足 (重要度順)

1. **型の段が要る** (unsound の 55/58 と取りこぼし 3): 関数値・`Fn` 境界・`dyn`・閉包の推論・効果変数の実体化・関数型の注釈と値の照合は、名前だけでは決まらない。この段は型の段 (`selfhost/types`) の出力を使うのが筋で、名前の検査は「型が要らない部分」しか確かめられない。
2. **呼び出しの表現が名前解決に頼る**: 局所変数のメソッド `v.m(..)` は `Call(Path "v.m")` として書かれ (`MCall` ではない)、AST だけでは関数の経路呼び出し `a.b(..)` と区別できない。`unwrap` の判定も、`Path` の呼び出しと `MCall` の両方で書く必要があった。AST に「レシーバが局所か」を持たせるか、名前解決の結果 (`ksn resolve` 相当) を入力にできると、1845 件の判断できない形の大半が区別できる。
3. **字句・構文の診断と書式の摩擦**: 予約語 `panics`・`uses`・`stack` をフィールド名に使うと `E0002: expected a name (found 'panics')` が **1:1** に出る (位置も説明も誤っている)。構造体リテラルは 1 行 100 字 (E0053・E0055) なので、長いものは関数に切り出す必要があった (`effects.fns.mk`)。`Vec` の要素を書き換える API がない (`as_mut_slice().get_mut` で代用)。`ksn fmt` は並びを直すが、関数の並びの違反は `check` の非致命的な E0020 で出る。

その他: `extern` 宣言 (`ExternDef`) は `resolve` の表に入らず、呼び出しは判断できない扱い。`ksn defs` は `.sig` を読まない (`resolve` と同じ)。推論された `uses`・`panics` (5.1 の非 pub) は未実装なので、そうした関数は判断できない。E4011・E4012・E4013 は対象外。

### 10.6 再現

```
. ~/.cargo/env
cd stage0 && cargo build --release && cd ..
cd selfhost/effects
ksn check && ksn fmt --check
ksn build --release                             # target/release/effects.native (退避なし)
./target/release/effects.native . ../../spec/stdlib    # 自分自身。末尾の summary 行
cd ../.. && python3 -I selfhost/effects/compare.py --group all   # 約 3.5 分 (Rust 版を 2 回ずつ呼ぶ)
```


### 10.7 型の表を使った後 (修正プラン 5-5 の型の表の段)

2026-11-15。ブランチ `selfhost/type-export`。`effects` は `types.export.run` (型検査の結果) を読む。メソッド呼び出し (`x.m()`・`s.len()`) の解決先を、型の表の項目 (FQN) から効果の表へ引き、定義の効果・panic と比べる。単位の添字は型の env と揃えた (`effects.main` は `types.env.unit` で単位を取る)。

| 対象 | 前 (10.3) | 後 |
| --- | ---: | ---: |
| 21 パッケージ (clean) の判断できない形 | 2366 | **325** |
| 同・診断の不一致 (E40) | 0 | 0 |
| `stage0/tests/data/linear_ok/*.ksnl` | 一致 118 / 誤検出 0 | 一致 118 / **誤検出 0** |
| `diag/E40*` (13 件) | 一致 4 / 判断できない 9 | **一致 7** / 判断できない 6 / 余分 0 |
| `unsound/` の効果・panic (58 件) | 一致 0 / 判断できない 55 / 取りこぼし 3 | 同じ (変化なし) |

- `apps/todo` の判断できなかったメソッド呼び出しは 511 → 0 (解決した呼び出しは 456 → 967 件を検査した)。`examples/demo` も誤検出 0。
- 変わらなかった理由: `unsound/` の 55 件は閉包・関数値・`dyn` の呼び出しで、型の表に解決先がない。`K.Opaque` (`dyn`) は trait を持たず、`Fnty` は効果を持たないため。ここは型の表に項目を足すまで判断できない (10.5 の 1 の残り)。
- 作り方: `types.rec` が節 (ノード) ごとに型・呼び出しの種別 (`ck`: 1 関数・2 メソッド・3 局所の関数値・4 列挙子・5 局所のフィールド)・解決先の項目を記録する。`effects.tyinfo` が種別 2 の解決先を効果の表の番号へ変える。
- 注意 (処理系と無関係の落とし穴): `resolve.table.add_file` は `rep.sig` で宣言ファイルかを見る。`resolve.parse_file` を通さずに呼ぶと `rep.sig` が古いままになり、標準の `.sig` で E1103 (`duplicate definition`) が出る。直接呼ぶ側で設定すること。

再現 (10.6 と同じ。`effects.native` は `ksn build --release`):

```
cd selfhost/effects && ksn check && ksn fmt --check && ksn build --release
python3 -I compare.py --group all      # 約 105 秒
```


## 11. ドライバの段 (修正プラン 5-5)

2026-11-15。ブランチ `selfhost/driver`。新規は `selfhost/driver/` と、この節の追記だけ (`stage0/`・`examples/`・`spec/`・`apps/`・`bench/`・`perf/` と `selfhost/` の他の部品は変えていない)。

結論を先に: **ビルドのドライバ `ksnc` は Kasane で書けて、`ksnc build --release` で自分自身を作れる (3 段の不動点を確認)。** 読み込みと名前解決は `selfhost/resolve` (Kasane 製) を使う。検査と LLVM IR への下ろしは Rust 版 `ksn` の中で行い、実行ファイルを作る clang の段だけを `ksnc` が担う。作った実行ファイルは、Rust 版 `ksn build --release` とバイト単位で同一 (下の 10.3 章)。

### 11.1 構成と使い方

`selfhost/driver/` が ksn パッケージ (`name = "ksnc"`、`entry = "ksnc.main"`、依存 `parse` と `resolve` を path で)。Kasane 396 行 (`src/ksnc.ksnl` 69、`src/ksnc/front.ksnl` 40、`src/ksnc/build.ksnl` 59、`src/ksnc/clang.ksnl` 117、`src/ksnc/tool.ksnl` 106、`src/ksnc/plan.ksnl` 5 など)。

```
cd selfhost/driver
ksn build --release                              # Rust 版で ksnc を作る (不動点の出発点)
./target/release/ksnc.native --stages            # 段の表 (どれが Kasane 製か Rust 製か)
./target/release/ksnc.native check [DIR]         # 読み込み・名前解決・ksn check
./target/release/ksnc.native build [--release] [--target <三つ組>] [DIR]
```

`DIR` は省略時 `.`。入口の場所は、PWD と上へ辿った先の `spec/stdlib` から決める (`KSN_STDLIB` があればそれ)。`ksn` は `KSNC_KSN`、無ければ `<根>/stage0/target/release/ksn`、それも無ければ PATH の `ksn`。実際の clang は `KSNC_CLANG`、無ければ `KSN_CLANG`、それも無ければ `clang`。

出力は `target/<kind>/<名前>.native` (`--target` 付きは `target/<三つ組>/<kind>/`)。隣に `<名前>.ll` (LLVM IR) が残る。

### 11.2 段の並び

| 段 | 担当 | 実装 |
| --- | --- | --- |
| 1 読み込み | Kasane | `selfhost/resolve`: `kasane.toml`・`src/`・直接の path 依存・`spec/stdlib` |
| 2 名前解決 | Kasane | `selfhost/resolve`: 名前の診断 (誤りがあれば表示して失敗) |
| 3 検査 | Rust | `ksn check` (型・効果・借用) |
| 4 下ろし | Rust | `ksn build` の中の KIR → LLVM IR (下の 10.4 章 1) |
| 5 実行ファイル | Kasane | `ksnc` の clang 段。`ksn` が `KSN_CLANG` で `ksnc` (この実行ファイル) を呼ぶ |

差し替え方: 段 1・2 は `front.ksnl` の 1 関数、段 3 は `build.ksnl` の `check`、段 4・5 は `build.ksnl` の `make` が `ksn build` を呼ぶ入口と `clang.ksnl` の `shim`。段 5 を Rust 版に戻すには `KSN_CLANG` を外せばよい (ksn の既定の clang が動く)。

段 4・5 の仕組み: `ksn build` が LLVM IR を作ると clang を呼ぶ。その呼び先を `KSN_CLANG=<ksnc>` と `KSNC_SHIM=1` で `ksnc` にする。`ksnc` は `KSNC_SHIM` が付いていれば clang の段として動き、引数 (`-x ir <IR>`・`-o <実行ファイル>`・最適化の段階) を読み、ランタイム C (`stage0/src/kir_rt.c`・`sysrt.c`) を `<名前>.ll.rt.c` などの名前で隣に写して、clang を起動する。ksn が同じ名前の一時ファイルを書くので、実行ファイルに埋まる文字列も同じになる。

### 11.3 確かめ

| 対象 | 結果 |
| --- | --- |
| `examples/*` と `apps/todo` (エントリのある 16 パッケージ) を `ksnc build --release` と `ksn build --release` で作る | **15 / 16 がバイト単位で同一** (`cmp`)。残り 1 件 `examples/ffi/shapes` は Rust 版も作れない (`Ptr(true, Named("shapes.Rect"))` は未対応、E9040) ので、ksnc は同じ理由で失敗する |
| 同じ 15 件の実行結果 (標準出力と終了コード、入力なし、10 秒の制限) | **15 / 15 同じ** |
| `examples/demo` の `--target aarch64-linux-gnu` | Rust 版とバイト単位で同一 |
| `examples/tour` の `--target x86_64-linux-gnu` | Rust 版とバイト単位で同一 |
| 不動点 (`ksnc` が `ksnc` を作る) | 0 段 (Rust 版で作ったもの) → 1 段 (それで作ったもの) → 2 段 (1 段で作ったもの)。**3 つのバイト列がすべて同一** |
| 診断 | `resolve/broken/undef` で `ksnc check` が 11 件の名前解決の誤りを出して終了コード 1 |

ksnc が作ったものの確かめは、比べる前に `ksn` のビルドキャッシュを空にする必要があった (10.4 章 2)。最初の比べ方では、16 件中 16 件が `cache hit` で Rust 版の実行ファイルを写しただけになり、一致の数が意味を持たなかった。

### 11.4 処理系の不足 (重要度順、処理系は直していない)

1. **プログラム全体の LLVM IR を外へ出す入口がない**。`ksn kir <FQN> --llvm` は `emit` を使い、テスト用の `main` (結果を `printf` して終了コード 0) を付ける。`Result` を返す入口は「この入口では扱えない」と断る。実際の実行ファイルに使う `emit_program` (入口の後始末つき) は `ksn build` の中からしか呼ばれない。そのため段 4 は `ksn build` を呼び、clang の段だけを差し替える (10.2 章)。`ksn kir` に `--target` もない。
2. **ビルドキャッシュの鍵が clang を含まない** (`KSN_CLANG` を変えても、前の結果を再利用する)。ksnc の clang 段に差し替えたあと、`ksn` の既定のキャッシュ (`~/.ksn/cache`) に当たると clang の段を通らず、前に作った実行ファイルを写すだけになる。ksnc は `KSN_CACHE_DIR=target/ksnc-cache` を使い、呼ぶ前に毎回そこを空にする。`ksn` 側は、鍵に clang の場所とその内容 (または `KSN_CLANG` の指す実行ファイルの版) を入れるべき。
3. **ネイティブの下ろしの不足が 4 つ** (ドライバを書く途中で当たった):
   - `std.process.exit` が段階 1 の対象外 (`呼び出し std.process.exit は段階 1 の対象外`)。終了コードは、入口が `Result` を返して `Error: <Debug>` と終了コード 1 になる形に頼った (2 にはできない)。
   - カレントディレクトリを得る API がない (`std.env` は引数と環境変数だけ)。`PWD` を読んで絶対パスを作った。
   - ディレクトリの再帰削除がない (`std.fs.remove_dir` は空のディレクトリだけ)。キャッシュの掃除は `rm -rf` の子プロセスにした。
   - `for a in v.as_slice().iter()` で `Vec<String>` (関数の戻り値) の要素に `a.as_str()` を呼ぶと、「要素の借用の種類 (Vec / String) が分からない」。`Vec<&'static str>` なら通る。
4. (ほか) `ksn build --release` は、ネイティブが成功しても「portable interpreter bundle」(`ksn` の複製と `stdlib`) を `target/<kind>/` に作り、その文言を出す。ksnc はそのまま表示する。`match` の arm に文字列リテラルを直接書くと `E0057` (書式の検査)。`resolve.main` は表示するだけで結果を返さないので、ドライバは同じ手順を `resolve` の公開 API で書き直した (パッケージの API の不足で、処理系ではない)。

### 11.5 まだ確かめていないこと

- `examples/ffi/shapes` は両方とも作れないので、ksnc 固有の経路 (clang の段) は通っていない。
- `apps/shortener_https`・`apps/todo_spa` と、エントリのない `examples/kvlib` (ライブラリ) は対象外。
- `aarch64` は `demo` の 1 件だけ (qemu での実行は見ていない。バイト列は Rust 版と同一)。
- 実行時間と並列化は測っていない (`ksn check` を 2 回呼ぶ分だけ遅い)。
- 検査 (`ksn check`) は `ksnc` の前後で 2 回走る (段 3 と、`ksn build` の中)。


## 12. 型検査の段 (修正プラン 5-5 の型の段)

2026-11-15。ブランチ `selfhost/types`。新規は `selfhost/types/` だけ (`stage0/`・`examples/`・`spec/`・`apps/`・`bench/`・`perf/`・他の `selfhost/` 部品は変えていない)。

結論を先に: **関数本体の型検査は Kasane で書けて、`ksn build --release` で退避なしにネイティブになった** (`types.native`、約 0.04 秒で 63 ファイルの `apps/todo`)。ネイティブと木走査の出力は `apps/todo` で同一。正しいコードの受理は **139 / 139** (例 15、apps 3、selfhost の parse・resolve・types 3、`linear_ok` 118)。型の診断は、Rust 版と (コード, 行) が一致したのが `diag/E20*` で 38 件中 **10**、`unsound/` の型の診断を出すべきもので 66 件中 **15** (コードだけなら 23)。残りは型の規則そのものが未実装 (E2033 線形・E2036 derive・E2046 `?`・E2049 effects など) か、推論の不足。

### 12.1 構成と行数

`selfhost/types/` が ksn パッケージ (`entry = "types.main"`、依存 `parse` と `resolve` を `path` で直接)。

| ファイル | 行数 | 中身 |
| --- | ---: | --- |
| `src/types.ksnl` | 25 | 入口: 読み込み → 検査 → 集計 (`summary errors N unknown M files K`) |
| `src/types/env.ksnl` | 277 | 読み込み (`resolve.manifest` / `resolve.load` / `resolve.parse_file` を再利用)、項目の表 (FQN → 項目、列挙子、import)、名前の解決 (プレリュード、基本型の関連関数 `core.num.<T>.`) |
| `src/types/ty.ksnl` | 350 | 型の arena (添字)、型変数の束縛 (`bind`)、単一化 (`unify`、参照は段数を見ず、不明・仮の型・`!` は一致とみなす)、整数リテラルの変数 (`c=1`) と小数の変数 (`c=2`) |
| `src/types/lower.ksnl` | 130 | 型の注釈 → 型 id (総称は検査の並びから引く。`&str` は `Str`、`fn(..)` は `Fnty`) |
| `src/types/chk.ksnl` | 97 | 検査の状態 (名前の並び、関数の終わりに決める項目、診断の文) |
| `src/types/infer.ksnl` | 1,232 | 式・文・パターン・呼び出し・メソッド探索 (`.sig` の規範。`FQN 接頭辞.名前`)、フィールド、`match` の網羅、`for` の要素 (`next` の戻り値) |
| `src/types/check.ksnl` | 144 | 関数 (頭の総称・引数・戻り値・本体)、テスト、定数・静的。関数の終わりの決定 |
| `compare.py` | 201 | Rust 版 `ksn check --json` との比較 (受理・一致の数、時間) |

Kasane 合計 2,255 行 (`compare.py` 除く)。`ksn check` は通る。

使い方:

```
cd selfhost/types
ksn build --release                       # target/release/types.native (退避なし)
./target/release/types.native PKG_DIR STD_DIR   # 例: ../../apps/todo ../../spec/stdlib
python3 -I compare.py --group examples|apps|self|linear_ok|diag|unsound   # 比較 (Rust 版 stage0/target/release/ksn が要る)
```

出力は `error CODE パス:行: 内容` と末尾の `summary errors N unknown M files K`。

### 12.2 確かめ

| 対象 | 受理 / 全体 | 備考 |
| --- | ---: | --- |
| `examples/*` (ksn パッケージ) | 15 / 15 | Rust 版も 0 件 |
| `apps/*` | 3 / 3 | |
| `selfhost/parse`・`resolve`・`types` (自分自身) | 3 / 3 | |
| `stage0/tests/data/linear_ok/*.ksnl` | 118 / 118 | 単一ファイルは一時パッケージにする |
| 正しいコードの合計 | **139 / 139** | |

| 対象 | 結果 |
| --- | --- |
| `diag/E20*.ksnl` (Rust 版が型の診断を出すもの 38 件) | (コード, 行) が一致 **10 / 38**、コードだけ一致 10 |
| `unsound/*.ksnl` (Rust 版が型の診断を出すもの 66 件) | (コード, 行) が一致 **15 / 66**、コードだけ一致 23 |
| `unsound/` で Rust 版は通すのに自分は型の診断を出したもの | 10 件 (下の 12.5) |

時間: ネイティブ `types.native` は `examples/demo` 0.11 秒、`apps/todo` 0.04 秒 (標準ライブラリ 52 ファイルの読み込みを含む)。木走査 `ksn run` は `apps/todo` で約 47 秒 (標準ライブラリの解析が大半)。`compare.py` は 139 件の正しいコードを約 10 秒で回す。

### 12.3 この段が見る規則

- E2020 (型の不一致): 両辺が具体的で単一化できないときだけ。引数・戻り値 (本体の末尾式の行に出す)・let の注釈・代入・`if` / `match` の腕・算術の両辺・条件 (bool)。
- E2044 (型が決まらない): 型の決まらない `let` (初期化式が未知)、型変数が残る `let` (初期化式が `None` の名前だけのとき)、未決まりの型変数へのメソッド呼び出し。
- E2027 (接尾辞のないリテラル): 関数の終わりまで型が決まらない整数・小数リテラル (既定の i32 は当てない)。
- E2001 (引数の個数)、E2010 / E2011 (構造体リテラルの未知・不足のフィールド)、E2021 (enum / bool の `match` は `_` と束縛を使わず全部を列挙する)、E2042 (`()` 以外の値の捨て。下の 12.5 の範囲のみ)。
- メソッド: 受け手の型から `FQN 接頭辞.名前` を引く (`core.num.<T>.`、`core.str.`、`core.slice.`、`core.bool.bool.`、`core.char.char.`、Adt は項目の FQN)。見つからなければ `Vec → [T]`、`String → str` の参照外しを試す。`clone` だけは Self を返す (derive は表に出ないため)。
- ジェネリクス: 呼び出しごとに総称を新しい型変数にして署名を下ろす。受け手で単一化して引数を見る。

### 12.4 処理系の不足 (重要度順)

1. **ネイティブの下ろしの制限が、書き方を大きく縛る** (このパッケージで直した数は 20 箇所を超える)。E9040 の理由は 1 件ずつしか出ず、直しては再ビルドした: (a) 構造体の `Vec` を、フィールド経由 (`env.srcs.as_slice().iter()`、`env.tab.imps`) や `&mut` の引数の `Vec<String>` で `for` すると下りない (「要素型の分かる Vec の変数だけ」)。添字の `for k in core.iter.range(0usize, v.len())` と `match v.as_slice().get(k)` で書き直した。(b) `match` の腕に `continue` を書くと型が合わないと言われる。(c) `CONST.method()` (定数の `usize` や `&str` への直接の呼び出し) は下りない (局所に受けてから呼ぶ)。(d) 100 桁を超える行・1 行に収まらない構造体リテラルは `ksn check` が止める (`E0053` / `E0055`)。`if c { a } else { b }` の 1 行は `E0056`。書式の規則が多く、生成したコードは `ksn fmt` 後も手で割る必要があった。
2. **構文木が「局所の受け手のメソッド」と「フィールド」を区別しない**。`s.len()` も `p.x` も `Path("s.len")` / `Path("p.x")` になり、`MCall` / `Dot` にならない (FQN の字句の規則で読まれる)。型検査は、先頭の成分が局所か項目かを見て、残りをフィールドかメソッドとして読み直す必要がある (`types.infer.path_text` / `call` / `method_call`)。`resolve` も同じ読み直しをしている。構文木の段で、局所に続く `.名前` を `Dot` / `MCall` にするのが望ましい (`parse.expr.path` で `FQN` の先頭が局所かは分からないので、名前解決の結果を見る後段で直すしかない)。
3. **標準ライブラリの署名の表が、推論に足りない** (E2044 の誤りの大半の原因)。(a) トレイトの既定メソッド (`map` / `filter` などの反復子の操作) の FQN が `.sig` の表にないので、受け手の型から引けない。(b) 総称の型を閉包の境界 (`F: FnOnce<(T,), U, E, P>`) から決める推論がない (`std.thread.spawn` の結果が決まらない)。(c) `derive` の実装 (`clone` など) は表に出ない。(d) 基本型の関連 FQN の綴りが仕様 4.5 の文と違う (`core.char.char.to_u32`、`core.bool.bool.then_some`)。表の綴りをそのまま使う必要があった。仕様かライブラリの名前を揃えるとよい。

### 12.5 Rust 版との差 (報告のみ。処理系は直していない)

- E2042 (`()` 以外の値を捨てる式文): Rust 版は `e?`・`if`・`match`・ブロックの捨てを見ない、`std` の `.sig` の関数呼び出しの捨ても見ない (`io.print_line(..)` の `Result`)。仕様 4.6 からは捨てになるが、ここでは Rust 版の判定に合わせた (`examples/http`・`examples/threads` が Rust 版で 0 件のため)。
- `unsound/` の 10 件は Rust 版が型の診断を出さずに通す (`float_to_string`・`int_to_string`・`str_to_string`・`num_from_not_in_stdlib`・`index_move_vec_string`・`fn_effects_match`・`fn_effects_reassign`・`hunt_a12_derive_tryclone_generic_hole_01`・`fn_bound_elided_*`)。この段は E2044 / E2027 で拒否する。

### 12.6 未実装 (型の段の範囲外)

線形性 (E2033 ほか)、derive の検査 (E2036 / E2050 / E2051)、効果・panic の整合 (E4xxx)、`?` の変換 (E2046)、トレイト境界の検査と既定メソッドの解決、クロージャの境界からの総称の推論、`impl` の孤児規則 (E2043)、`with` の検査 (E2045)、ライフタイム。`uses` / `panics` は見ない。

### 12.7 再現

```
. ~/.cargo/env
cd stage0 && cargo build --release && cd ..
cd selfhost/types
ksn check && ksn build --release
./target/release/types.native ../../apps/todo ../../spec/stdlib
python3 -I compare.py --group examples && python3 -I compare.py --group linear_ok
python3 -I compare.py --group diag && python3 -I compare.py --group unsound
```


### 12.8 型の表 (効果・借用へ出す)

2026-11-15。ブランチ `selfhost/type-export`。新規は `types/src/types/rec.ksnl` (表) と `types/src/types/export.ksnl` (`types.export.run`)、既存の変更は `infer.ksnl`・`check.ksnl`・`chk.ksnl` の記録だけ。`types.main` の診断は変えていない。

- 記録: `types.infer.expr` を、本体の `expr_in` を呼んで節の型を書く包みにした。`pat` は束縛パターンの型、関数の引数 (Param 節) は宣言の型を書く。呼び出しは `types.rec.resolved` で種別と解決先 (env の項目 + 1) を置き、包みで節に書く。局所に続くフィールド (`p.x`) は種別 5 と記録する (構文木で `s.len()` と `p.x` が同じ `Path` になる問題 (12.4 の 2) は、種別で区別する形で読める)。
- 読み出し: `types.export.run(pkg, std_dir)` が `Out { env, ar, cx }` を返す。節の型は `types.rec.ty_of(&out.cx.rec, 単位, 節)` で取り、`types.ty.find` で束縛をたどる。単位の添字は `out.env.srcs` と揃う。
- 診断は変えない: `types.native` の出力は `apps/todo`・`examples/demo`・`examples/threads`・`examples/http` で HEAD と同一 (md5 一致)。受理は `linear_ok` 118/118、`examples` 15/15 で変わらず。
- 使う側: `borrow` (15.8) と `effects` (10.7)。


## 13. LLVM 出力の段 (修正プラン 5-5 の次の段)

`selfhost/emit/` (ブランチ `selfhost/emit`)。KIR のテキスト (`ksn kir t.main` の出力) を読み、Rust 版 `kir_llvm.rs` と同じ規則で LLVM IR のテキスト (入口 `main` の包みを含む実行可能な形) を標準出力へ書く。Kasane で書いた `emit` を `ksn build --release` でネイティブにした。処理系 (`stage0/`) は変えていない。

### 13.1 構成と行数

| ファイル | 行数 | 中身 |
| --- | ---: | --- |
| `src/emit.ksnl` | 338 | 入口 (引数の読み込み・モジュールの順序・表と作業場所の型) |
| `src/emit/ty.ksnl` | 269 | 型の表 (KIR の型の文字列を添字に。LLVM の型の文字列・大きさ・`Option` / `Result` の枠) |
| `src/emit/agg.ksnl` | 201 | 集成体の表 (`agg#N` の行。フィールドの型・オフセット・列挙型の変種) |
| `src/emit/func.ksnl` | 377 | 関数ごと (1 走査: 値の型・ブロックの引数・後続 / 到達性 / 本体 / phi を含む組み立て) |
| `src/emit/inst.ksnl` | 1,662 | 命令 1 つと終端命令の写し (整数・比較・集成体の値のモデル・メモリ・ヒープ・`write` / `eprint` / panic) |
| `src/emit/rt.ksnl` | 175 | 固定の本体 (`ksn.depth`・文言・`ksn.panic.*`・`ksn.write` / `ksn.eprint`・入口 `main`) |
| `src/emit/util.ksnl` | 438 | 文字列の補助 (十進・トークン・引用符の中身・C の文字列の表記・i128 の表示) |
| `compare.py` | 137 | 比較: Rust 版の `ksn build` の実行ファイル と、Kasane 版の `.ll` を clang で繋いだ実行ファイルの出力・終了コードを比べる |

Kasane 側は合計 3,460 行 (空行・注釈を含む)。`panics may` を全部に付けた (`panics never` は添字や `+ - *` を使えないため)。

### 13.2 使い方

```
cd selfhost/emit
ksn check && ksn build --release               # target/release/emit.native
target/release/emit.native FILE.kir t.main > out.ll
python3 -I compare.py --work /tmp/emit-work     # linear_ok の 118 件を比べる (作業場所は既定の外に)
```

### 13.3 確かめ

| 項目 | 結果 |
| --- | --- |
| `linear_ok/` の 118 件: Kasane 版 `.ll` を Rust 版と同じランタイム (`kir_rt.c`・`sysrt.c`) と clang でリンクして走らせ、標準出力・標準エラー・終了コードが Rust 版のネイティブと一致 | **118 / 118** |
| `ksn build --release` (退避なし、E9040 なし) | ネイティブ (`target/release/emit.native`)、約 5.5 秒 (この機械の負荷込み) |
| 118 件の変換の時間 (release、プロセス起動を含む) | 合計 0.16 秒。debug 版とは全件で出力が同じ |
| IR の文字列の一致 (Rust 版の `ksn kir --llvm` と) | 測っていない。Rust 版は入口の包みや一時の名前の付け方が違い、文字列としては一致しない見込み。比べたのは実行結果 |

一致の数え方: `compare.py` は Rust 版の `ksn build` の実行ファイルを基準にする。Rust 版が退避 (インタプリタ同梱) になった組は比較から外して数に出す (今回は 0)。`h04_sibling_blocks_same_name_move` は、入口が引数を取るため Rust 版も「引数の個数が合わない」panic (終了コード 101) で走る。Kasane 版も同じ文言・同じ終了コードで出すようにした。

### 13.4 対応していない形 (`emit` が止まる、または未確認)

- 浮動小数 (型の表で `f32` / `f64` を拒否する。KIR の `fadd` / `fcmp` / `conv` 系、`fmtfloat`)。linear_ok の 118 件の KIR には `f32` / `f64` の型が一つも出てこない (確認済み)。
- `std.net` / `std.fs` / `std.env` / `std.time` / `std.process` / `std.rand` / `std.thread`、`ParseInt`、`Utf8Valid`、`checked.*`、`saturating` の算術、`bitcount`、外部関数 (`link`)、関数ポインタ (`fnptr`)。
- 入口が `()` 以外を返す形 (`Result` の表示)。wasm32・aarch64 の出力。
- 引数を受ける入口は、Rust 版と同じく「引数の個数が合わない」panic (101) を出す形だけを写している。

### 13.5 処理系の不足 (重要度順)

この段で当たったもの。処理系は直していない (コードの書き方で回避した)。エラーの文言はそのまま。

1. **段階 1 の下ろしで、`!` を返すユーザ関数と `std.process.exit`、`core.mem.replace` が使えない。** 最初は `fn emit.fail(msg) -> !` (`std.process.exit` を呼ぶ) を作ったが、`呼び先 emit.fail の戻り値の型は段階 1 の対象外` で止まった。`std.process.exit` は `呼び出し std.process.exit は段階 1 の対象外`、`core.mem.replace` は `呼び出し core.mem.replace は段階 1 の対象外`。結果: 致命的な終了 (終了コードを選ぶ) を書けず、`core.panic.panic` (101) だけで止めた。`&mut` の中の文字列を取り出して差し替える手段もなく、`let d = g.f` は `E3002 cannot move out of ... borrow it` (参照越しに移せない)。文字列は中身を複写して作り直した。
2. **`for` の対象が限られる。** `for x in s.split(sep)`・`for b in bytes.iter()` は `for の対象は core.iter.range(a, b) か、要素型の分かる Vec の変数 ... だけ段階 1 の対象` で止まる。この部品では、区切りの個数と `piece_at` (k 番目の片) を返す関数で `while` に書き直した (`util.ksnl`)。文字列を扱うセルフホストでは、この書き直しが全体に出る。
3. **借用検査の誤検出らしきもの (E3010 / E3020 / E3021)。** `let l = f(text, k)` で借りた `&str` を、同じ関数の中で分解し、その後に `&mut G` を使うと `use of ... borrows l which does not live long enough` になる (最小の例で再現)。引数として渡す形にすると通る。また `fn f(lines: &Vec<&str>, i) -> &str` は寿命を書いても `E3021 lifetimes can be elided` と言われ、本体の借用が外まで伸びる。`fn f['a](text: &'a str, starts: &Vec<usize>, k) -> &'a str` (文字列の本体を最初の引数に置く) なら通った。
4. **構造体の値の書き方が狭い (E0053)。** 構造体の値は一行で、フィールドの値は識別子だけ。フィールドが 10 を超えると一行に収まらず、状態を 5 つ前後の群に分けた (`emit.G` の `tab` `lay` `flow` `buf` `outs` `misc` `flags`)。
5. **書式の制約。** 1 行の `if c { a } else { b }` (E0056)、腕のタプルの束縛 `Some((a, b)) =>` と腕の本体の代入 (E0057)、`let _ = e` (E2041、`core.mem.drop` を使う)、本体の 100 文字超の行 (E0002 / E0055。署名の行は対象外)。`ksn fmt` は並べ替えと空行を直すが、長い行と上の 5 つは直さない。
6. **未対応の標準ライブラリ型 (段階 1)。** `core.fmt.Formatter`・`core.hash.Fnv64` は、到達する関数の戻り値・引数にあると段階 1 で拒否される (この部品では到達しなかったので、影響は出ていない)。

### 13.6 まだ書いていないこと

- 浮動小数・外部関数・標準ライブラリの実行系の呼び出し (10.4)。
- IR の文字列の一致 (Rust 版との比較は実行結果だけ)。
- 型検査・KIR への下ろし (前段) は Kasane で書いていない。入力は Rust 版の `ksn kir` の出力。
- 大きな入力での性能は未計測 (118 件は各数 KB)。

### 13.7 再現

```
. ~/.cargo/env
cd stage0 && cargo build --release && cd ..
cd selfhost/emit
ksn check && ksn build --release
python3 -I compare.py --work /tmp/emit-work     # 118 / 118 (約 5 分)
```


## 14. 整形の段 (修正プラン 5-5 の次の段)

2026-11-15。ブランチ `selfhost/fmt`。新規は `selfhost/fmt/` と、この節の追記だけ (`stage0/`・`examples/`・`spec/`・`apps/`・`bench/`・`perf/` は変えていない)。

結論を先に: **字句・構文解析後の整形 (`ksn fmt`) は Kasane で書けて、`ksn build --release` で退避なしにネイティブになった。** Rust 版 `stage0/src/fmt.rs` の並び順・空行・インデント規則を Kasane で再実装し、`ksn check` / `ksn fmt --check` は通る。`examples/*/src/**/*.ksnl`、`apps/*/src/**/*.ksnl`、`selfhost/*/src/**/*.ksnl`、`stage0/tests/data/linear_ok/*.ksnl`、`stage0/tests/data/unsound/*.ksnl` の 594 ファイルすべてについて、Kasane 版と Rust 版の出力がバイト単位で一致した。

### 14.1 構成と行数

`selfhost/fmt/` が ksn パッケージ (`kasane.toml` `kasane.lock`、`entry = "fmt.main"`、依存 `parse = { path = "../parse" }`)。

| ファイル | 行数 | 中身 |
| --- | ---: | --- |
| `src/fmt.ksnl` | 約 820 | 入口: 引数のファイルを読み、機械的整形 (タブ→4スペース・末尾空白除去・行末空行削除) → 定義走査 → 7 グループ (import/const-static/type-enum-trait/extern/impl/fn/その他) に分類 → UTF-8 バイト順で各グループ内を挿入ソート → impl は最初の子 fn と隣接して出力 → チャンク間に空行 1 行 → 最終改行 |
| `compare.py` | 約 90 | Kasane 版 (`fmt.native`) と Rust 版 (`ksn fmt`) の出力をバイト単位で比較。4 行出力: コミットハッシュ・総行数・一致数/総数・native か否か |

Kasane 合計 約 910 行。`ksn check` / `ksn fmt --check` / `ksn build --release` はすべて通る。

### 14.2 実装上の注意

- 段階 1 で使えない std API (`starts_with` `split_at` `trim_end` `trim_start` `String.replace` `slice.sub` `slice.swap` `slice.len` `str.contains` `str.find` `core.str.contains` ほか) は、バイト配列 (`as_bytes().get()`) と `Vec<u8>` バッファ・手動ループで再実装した。
- `Vec<Vec<usize>>` は段階 1 で `&mut Vec` が拒否されるため、7 つの独立した `Vec<usize>` (g0〜g6) に分けた。
- `sort_by` は手動の挿入ソート (`fmt.sort_def_indices`) で再実装。`get_mut` の並行借用を避けるため、2 回の独立した `get_mut` 呼び出しでスワップする。
- `end` は予約語なので、変数名に使わない (`end_pos` など)。
- `break` は `match` アームの初期化子では使えない (`0u8` や `false` で代替)。`if matched { true }` は関数から返らないので、`return true` を使う。
- `impl` の名前は行の残り全部 (`fn` などは識別子部分のみ)。

### 14.3 使い方

```
cd selfhost/fmt
ksn build --release                                   # target/release/fmt.native (退避なし)
./target/release/fmt.native FILE...                   # 整形結果を stdout に出力
python3 -I compare.py [--group examples|apps|selfhost|linear_ok|unsound|all] [--files F...] [-v]
```

### 14.4 再現

```
. ~/.cargo/env
cd stage0 && cargo build --release && cd ..
cd selfhost/fmt
ksn check && ksn fmt --check
ksn build --release
python3 -I compare.py --group all -v                  # 594 件すべて一致
```


## 15. 借用・線形性の検査の段 (修正プラン 5-5 の借用の段)

2026-11-15。ブランチ `selfhost/borrow`。新規は `selfhost/borrow/` と、この節の追記だけ (`stage0/`・`examples/`・`spec/`・`apps/`・`bench/`・`perf/` と `selfhost/` の他の部品は変えていない)。

結論を先に: **関数本体の借用・線形性の検査は Kasane で書けて、`ksn build --release` で退避なしにネイティブになった** (`borrow.native`、自分自身で 0.02 秒)。正しいコードの誤検出は **0 件** (例 15・apps 3・selfhost 5・`linear_ok` 118 の計 141 件)。ただし拒否できた範囲は狭い。Rust 版が借用・線形性の診断 (E30xx) を出す `unsound/` の 262 件のうち、**同じコードで拒否したのが 31 件、コードの多重集合まで一致したのが 27 件**。型の情報が外へ出ないため、Copy か移動かの判定を型注釈と式の形の近似に頼り、判断できない形は「判断保留 (undec)」として数えた。

### 15.1 構成と行数

`selfhost/borrow/` が ksn パッケージ (`entry = "borrow.main"`、依存 `parse` と `resolve` を `path` で)。

| ファイル | 行数 | 中身 |
| --- | ---: | --- |
| `src/borrow.ksnl` | 74 | 入口: 読み込み → 型の表 → 関数の戻り値の表 → 検査 → 集計 (`summary errors N undec M files K`) |
| `src/borrow/base.ksnl` | 278 | 値の分類 (`UNK` `COPY` `MOVE` `LIN` `SREF` `MREF`)、型と関数の表、型の注釈からの分類、ブロックの閉じ位置 |
| `src/borrow/cx.ksnl` | 373 | 局所 (名前・分類・状態・有効範囲)、借用 (借り手と借り先)、状態の写しと合流、借用の生存 (`alive`)、使い方の記録 (`access`) |
| `src/borrow/walk.ksnl` | 721 | 式・文・ブロック・分岐 (`if` `match`)・ループ・`let`・代入・呼び出し・クロージャの捕捉・`with` |
| `src/borrow/esc.ksnl` | 116 | 戻り値の検査: 局所の借用を外へ出す (E3030)、戻り値の寿命と合わない引数 (E3010) |
| `src/borrow/fns.ksnl` | 77 | 関数 1 本の駆動 (頭の寿命と省略の源の決定、引数の線形値の漏れ) |
| `src/borrow/tabfill.ksnl` | 49 | 型の表 (`linear` と `derive [Copy]`)、関数の戻り値の分類 |
| `compare.py` | 204 | Rust 版 `ksn check --json` との比較 (`--group examples|apps|self|linear_ok|unsound|diag|all`) |

Kasane 合計 1,688 行。`ksn check` は通る (`E0020` の並びも `ksn fmt` で直した)。

### 15.2 検査の中身

- **E3002** (移動した後の使用): 移動 (`MOVE` の値を値として渡す・代入する・返す) の後の読み書き。分岐は「どれかの枝で移動したら移動済み」に合流する。
- **E3003** (線形値の消費漏れ・二重消費): 生存のまま有効範囲を出る・`return`・`?` で早期に出る・引数の線形値を消費せずに関数を出る・`_` で捨てる。消費済みの線形値をもう一度使う。
- **E3010** (借用中の変更・移動・借用): `let r = &[mut] x` (`x.f`・`x[i]` も根の `x`) の借り手 `r` が、後の文で使われる間に `x` を書く・移動する・(可変なら) 借りる。容器 (`String` `Vec` など) の `as_str` `as_slice` `get` `iter` などが返す借用も同じ扱い。同じ式の中で、同じ局所を一方が可変で借りてもう一方も借りる組 (引数・組・構造体の欄・`Some`/`Ok`/`Err` の中まで)。
- **E3030** (局所の借用を外へ出す): 戻り値 (末尾の式・`return`・`Some`/`Ok`/`Err`・構造体や組の欄) が、値の局所 (引数を含む) を借りている。
- **寿命の照合** (A1 の一部): 戻り値の参照の寿命と、返す参照引数の寿命が合わなければ E3010 (`fn t.pick['a, 'b](x: &'a u32, y: &'b u32) -> &'a u32 { y }`)。`'static` の引数は何にでも合う。省略は、`self` が参照なら `self`、参照の引数がちょうど 1 つならその引数。それ以外の省略は判断保留。
- **クロージャ**: 本体は見ない。捕捉 (`move` は移動、`&` / `&mut` は借用として `access` に記録) だけを見る。捕捉されず本体で使う外側の名前は仕様上 `capture` に列挙が要るので、その分だけ見える。
- **判断保留** (`undec`): 分類が不明の値を移動しうる形で使った・その後の使用・`match` で不明な値の束縛を取り出した・ループの本体で状態が変わった局所・クロージャ・`with` の名前。誤りとは数えない。

### 15.3 分類の決め方

型検査器 (`selfhost/types`) は式の型を外へ出さない (`types.main` は診断だけ)。そこで分類は次の順で決め、決まらなければ `UNK` (不明) にした。

1. 引数と注釈付きの `let`: 型の注釈から (`TPrim` は `COPY`、`&T` は `SREF`、`&mut T` は `MREF`、`String` `Vec` `Box` `HashMap` などは `MOVE`、`Option` と `Result` は引数の合成、同じパッケージの型は `linear` と `derive [Copy]` から)。
2. 注釈のない `let`: 初期化式の形から (リテラル、`&` `&mut`、局所の複写、構造体リテラル、同じパッケージの関数の戻り値、`String::new` などの容器の生成)。
3. それ以外は `UNK`。`UNK` の局所を移動しうる形で使うと状態は判断保留になる。

同じ名前の型・関数が二つあれば `UNK` にする (末尾の成分で引く)。

### 15.4 確かめ

| 対象 | 件数 | 結果 |
| --- | ---: | --- |
| 正しいコード: `examples/*` | 15 | 誤検出 **0**。判断保留なしで受理 8 (判断保留は計 52) |
| 正しいコード: `apps/*` | 3 | 誤検出 **0**。判断保留なしで受理 0 (計 73。stdlib の戻り値とメソッドの受け手が `UNK`) |
| 正しいコード: `selfhost/{parse,resolve,types,effects,borrow}` | 5 | 誤検出 **0**。判断保留 324 |
| 正しいコード: `stage0/tests/data/linear_ok/*` | 118 | 誤検出 **0**。判断保留なしで受理 98 |
| 正しいコードの合計 | **141** | 誤検出 **0**。判断保留なしで受理 **106** |
| 借用の拒否すべきもの: `unsound/*` (Rust 版が E30xx を出す) | 262 | 拒否 **31**。コードの多重集合まで一致 **27**。Rust 版が出さないのに拒否 (余分) **0** |
| `diag/E30*` (Rust 版が E30xx を出す) | 27 | 拒否 5。一致 5。余分 0 |

- E30xx の内訳 (Rust 版が出すもの): E3010 100、E3002 83、E3003 64、E3030 38、E3006 12、E3031 7、E3040・E3041 各 2、E3050・E3043 各 1。自分が出したのは E3010・E3002・E3003・E3030。
- 小さな確かめ (リポジトリに入れていない): 移動の後の使用 (E3002、行まで一致)、`let r = &mut x` の後の代入 (E3010、行まで一致)、`&x` の返却 (E3030、行まで一致)、正しい借用 (誤り 0)。寿命の照合の 4 本: `'b` の引数を `'a` で返す (E3010。Rust 版も E3010 で一致)、`'static` の引数と `'a` の引数を返す (OK。Rust 版も 0 件で一致)、省略された入力を `'b` で返す (E3010。Rust 版は何も出さない。仕様 4.3 では省略された入力は独立の新しい寿命なので、Rust 版の取りこぼしとみる)。
- 時間: ネイティブ `borrow.native` は自分自身 0.02 秒、`apps/todo` 0.03 秒。木走査 `ksn run` は自分自身で 24.7 秒 (標準ライブラリ 52 本の解析が大半)。Rust 版 `ksn check` は自分自身で 0.23 秒。

### 15.5 処理系の不足 (重要度順、処理系は直していない)

1. **型の情報が外へ出ない**。`types.main` は診断しか出さず、式の型・局所の型・メソッドの受け手が値で受けるかを返す入口がない。そのため Copy か移動かを型注釈と式の形から近似し、判断保留が多い (正しいコードの受理が 141 件中 106、`apps` は 0)。E3002 の取りこぼしの主因。`types` の内部 (`types.infer.expr` の戻り値の型 id) は使えるが、局所ごとの型を取り出す公開の関数がないので、型の段を呼ぶ道が要る。
2. **クロージャと、列挙型の中身の型**。捕捉の使い方は見るが、`move` で取り込んだ線形値を本体が消費するか、`FnOnce` を一度だけ呼ぶかは本体の走査と型が要る。`if let Ok(f) = r` の `else` で `Err` の中の線形値が漏れる E3003 (`unsound/if_let_linear_else_err_leak`) は、列挙子の payload の型 (`Result<L, L>` の `Err`) が要る。E3002 (`closure_*`) と E3003 (`if_let_*`) の取りこぼしの大半がこれ。
3. **ネイティブの下ろしと書式の制限**。`Vec<構造体>.clear()` が下りない (`clear の要素の型は段階 1 の対象外 (スカラーと所有物だけ)`。`truncate(0)` で回避)。一行の `if c { a } else { b }` は `E0056` で書けない (関数に切った)。構造体リテラルは 1 行 100 字 (`E0055`) で、長い場合は短いフィールド名にした (`Loc { a, b, cl, mu, .. }`)。`parse.expr.OP_REF_MUT` は非公開で (`E1119`)、同じ値 (1000) を `borrow.base.OP_REF_MUT` に複製した。`stack` は再帰する `panics never` 関数にだけ書ける (`E4011`)。再帰は `E4010` で `stack` が要る。

### 15.6 未実装 (借用の段の範囲外・取りこぼし)

- E3031・E3006・E3040・E3041・E3043・E3050 は見ない (E3006 はクロージャ、E3040・E3041 は `with`、E3050 は中断をまたぐ借用)。
- 2 相借用・NLL の細部 (借り手が使われる最後の位置は、同じ文の後だけを見る)、ループの二回目の反復の効果 (本体は一度だけ走査し、外の状態が変わった局所は判断保留)、部分的な移動 (`x.f` の移動は読みとして扱う)、`match` の束縛での移動 (判断保留)、`with` の後始末。
- 借用を返す関数の呼び出し (`let r = f(&x)` で `r` が `x` を借りる) は見ない。容器のメソッドの借用は `as_str` `as_slice` `as_bytes` `get` `first` `last` `iter` `keys` と `*_mut` の一覧だけ。
- 戻り値の寿命の省略は、ヘッダの省略規則の一部だけ (`self` と参照の引数が 1 つ)。ヘッダの検査 (E3021 など) はしない。

### 15.7 再現

```
. ~/.cargo/env
cd stage0 && cargo build --release && cd ..
cd selfhost/borrow
ksn check && ksn fmt --check
ksn build --release                              # target/release/borrow.native (退避なし)
./target/release/borrow.native ../../apps/todo ../../spec/stdlib   # 末尾の summary 行
python3 -I compare.py --group linear_ok          # 誤検出と受理 (118)
python3 -I compare.py --group unsound            # 拒否・一致・余分 (Rust 版を 409 回呼ぶ。約 16 秒)
python3 -I compare.py --group all                # 全部
```


### 15.8 型の表を使った後 (修正プラン 5-5 の借用の段、型の表つき)

2026-11-15。ブランチ `selfhost/type-export`。`borrow` は `types.export.run` の型を読む。Copy か移動かを型で決める: `borrow.tcls` が型を分類に直す (基本の値・`&str` は SREF、参照は SREF / MREF、組・配列は要素の合成、Adt は Option・Result が引数の合成、容器は MOVE、他は型の表 `borrow.tabfill` の線形・Copy の派生)。節に型があれば、式の値 (`expr_cls`)・`let` の束縛・パターンの束縛 (`declare_pat`、`match`・`for` の中も) がその分類になり、型が不明な節だけ 15.3 の近似に任せる。

| `compare.py --group all` | 前 (15.4 の近似) | 後 (型の表) |
| --- | ---: | ---: |
| 正しいコードの誤検出 (141 件) | 0 | **0** |
| 正しいコードで判断保留なしに受理 | 106 | **112** |
| 判断保留の合計 (577 件) | 481 | **95** |
| Rust 版が E30xx を出すもの (289 件): 拒否 | 36 | **55** |
| 同: コードの多重集合まで一致 | 32 | **48** |
| 同: Rust 版が出さないのに拒否 (余分) | 0 | 0 |

- 15.4 の表の 262 件・31 件・27 件は `unsound/` だけの数で、この表は `all` (diag を含む) の数。前後は同じ `all` で比べた。
- 15.5 の 1 (型の情報が外へ出ない) は解決した。残りは 15.5 の 2 (閉包の本体、列挙子の payload の型で `Err` の中の線形値を追う) と、ループの二回目の反復。
- 型の段の `expr` の包みは節ごとに記録する (12.8)。借用の表は `borrow.cx.Cx.tcls` に単位ごとに持つ。

再現:

```
. ~/.cargo/env
cd selfhost/borrow && ksn check && ksn fmt --check && ksn build --release
python3 -I compare.py --group all      # 約 110 秒
```

## 16. KIR への下ろしの段 (修正プラン 5-5)

2026-11-15。ブランチ `selfhost/lower`。新規は `selfhost/lower/` と、この節の追記だけ (`stage0/`・`examples/`・`spec/`・`apps/`・`bench/`・`perf/` と `selfhost/` の他の部品は変えていない。`dev/plan` は `selfhost/emit` の統合のために取り込んだ)。

結論を先に: **関数の本体を KIR のテキストに下ろす段は Kasane で書けて、`ksn build --release` で退避なしにネイティブになった (`lower.native`。probe の 5 関数で 0.06 秒。木走査 `ksn run` の 26〜32 秒に比べる)。** 下ろせるのは、整数・`bool`・`if` / `while` / `loop` / `break` / `continue` / `return`・列挙型の `match`・構造体の構築と読み出し・関数呼び出し・`core.mem.drop` (単位の値) まで。`Option` / `Result` / `?` / `Vec` / `String` / 参照 / 総称関数は未着手で、ここが対象の大半 (14.4 章)。出力は Rust 版 `ksn kir` と、値・ブロック・集成体の番号を出た順に直して比べると、`linear_ok` の 439 関数のうち **47 が一致** (下ろせたのは 90)。下ろせた関数だけを数えると、一致は 47 / 90。

### 16.1 構成と行数

`selfhost/lower/` が ksn パッケージ (`entry = "lower.main"`、依存 `parse`・`resolve`・`types` を `path` で直接)。

| ファイル | 行数 | 中身 |
| --- | ---: | --- |
| `src/lower.ksnl` | 117 | 入口: 読み込み (`types.env.load`)、呼ばれた関数の列 (FQN の順に広げる)、名前の順の出力 |
| `src/lower/cx.ksnl` | 395 | 下ろしの状態 (`Cx` = 型検査の arena・`types.chk.Ctx`・表・関数ごとの出力)、値・ブロック・命令の組み立て、可変の局所の引き回し |
| `src/lower/tyk.ksnl` | 356 | 型の下ろし (`types` の型 id → KIR の型の文字列)、集成体のレイアウト (大きさ・そろえ・オフセット)、`agg#N` の行 |
| `src/lower/expr.ksnl` | 1,053 | 式と文 (`lower.expr.expr` が `types.infer.expr` で型を得て、形ごとに下ろす)、`if` / `while` / `loop` / `match` の合流 |
| `src/lower/func.ksnl` | 142 | 関数 1 本の頭 (`uses`・`panics`・`stack`・`ret`) と本体、結果の記録 |
| `compare.py` | 211 | 比較: Rust 版 `ksn kir FQN` と、この段の出力を関数ごとに並べ、番号だけ正規化して比べる |
| `probes/probe/` | 1 パッケージ | 手で確かめた小さな入力 (整数・`while`・構造体・列挙の `match`) |

Kasane 合計 2,063 行 (`compare.py` を除く)。`ksn check` と `ksn fmt --check` は通る。

### 16.2 使い方

```
cd selfhost/lower
ksn build --release                              # target/release/lower.native
./target/release/lower.native PKG_DIR STD_DIR FQN...   # 例: ./target/release/lower.native ../../examples/demo ../../spec/stdlib demo.main
python3 -I compare.py --group probe --mine native           # 比較 (probe / examples / linear_ok / all)
```

出力は `kir 1` で始まり、集成体の行 (`agg#N`)、関数 (FQN の順)、末尾に下ろせなかった関数の `unsupported FQN: 理由` の行。`unsupported` の行を外すと、`ksn kir` と同じ形になる。`ksn run` でも動くが、標準ライブラリの読み込みが木走査で 26〜32 秒かかる (ネイティブは 0.06 秒)。

### 16.3 確かめ

| 対象 | 結果 |
| --- | --- |
| `ksn build --release` | ネイティブ (`target/release/lower.native`)。退避なし |
| `probes/probe` (5 関数) | `add`・`mk`・`cnt` が Rust 版と一致。`pick` (列挙の `match`) と `main` (`core.mem.drop` の後の値の扱い) は違う (14.5 章) |
| `stage0/tests/data/linear_ok/*.ksnl` (118 本、定義された関数 439 本) | **一致 47 / 439** (下ろせた 90、未対応 481 の理由) |
| `examples/*` (172 関数) | 一致 0 / 172 (下ろせた 4)。ほとんどが `Vec` / `String` / `Option` / 総称の関数 |
| 端から端まで (`t.main` が呼ぶ関数がすべて下ろせた 9 本) | Kasane 版の KIR → `selfhost/emit` (`emit.native`) → clang (`kir_rt.c`・`sysrt.c`) → 実行。Rust 版のネイティブと、標準出力・標準エラー・終了コードが **9 / 9 一致** |

9 本の内訳: `temp_linear_consumed` `ok_pair_destructured` `for_owned_vec_by_ref` `for_linear_array_consumed` `generic_linear_consumed` `linear_ok_method_on_bound` `linear_ok_pair_fields_consumed` `ok_pair_both_fields` `ok_pair_match_both`。いずれも標準出力は空 (終了コードは 0)。実行の確かめは「下ろして、LLVM まで通して、同じ結果になる」ことまでで、出力の中身は見ていない。

一致しない関数の差は、(a) Rust 版の `own.*` (所有の注釈。線形の値の移動)、(b) `frame.bytes` / `mem.store` (借用の値を書き出す形)、(c) `if` の合流で使う値の番号と単位の定数の作り方 (番号は正規化で消えるが、単位の定数を作る場所は違う)。(a) と (b) は、下ろしていない型 (参照・`Vec`・`String`) に伴うもの。

### 16.4 下ろせなかった形 (`linear_ok` の 118 本、理由の件数)

| 理由 | 件数 | 理由の中身 |
| --- | ---: | --- |
| 型の下ろし (未対応の型) | 215 | `Option` / `Result` / `Vec` / `String` / 参照 / タプル。KIR の枠とタグの表現 (設計_KIR 27 章) を写していない |
| 本体のない宣言 | 94 | `.sig` だけの関数 (`std.io`・`Vec` の基本メソッドなど)。KIR の命令を写す表がない (`core.mem.drop` だけ写した) |
| 総称の関数 | 46 | 単相化 (型引数ごとのインスタンス) を書いていない |
| 名前が局所でない | 35 | 定数・関数の値・列挙子の値 (`E.B` の値) |
| `let` の形 | 33 | 分解の束縛 (`let (a, b) = ...`)、初期化式のない `let` |
| メソッド呼び出し | 15 | `wrapping_*` 以外 |
| 式の形・`match` (列挙型以外)・単項 (`!` 以外)・列挙子の構築 | 37 | 整数・`bool` 以外の `match`、`-x`、`Some(x)` など |

算術 (`+` など) は `panics may` の関数で溢れの検査を伴うので、まだ書いていない (`add.T.trap` と panic の枝)。`&&` / `||` も未対応 (短絡の枝を書いていない)。

### 16.5 処理系の不足 (重要度順)

この段で当たったもの。処理系は直していない。

1. **型検査器が式ごとの型を残さない。** 下ろしは、式ごとに `types.infer.expr` を呼び直して型を得ている (設計_KIR 2.4 の (a) が未実装)。入れ子の深さ分だけ同じ部分木を検査し直すので、大きい関数では無駄が多い。型のないリテラル (`let x = 0`) は、呼び直すときに型が決まらないことがあり、既定の `i32` で補った (検査器の既定と同じ。後で型が決まる場合は違う可能性がある)。
2. **ネイティブの下ろしが、文字列と参照の書き方を狭くする。** `for` が `Vec<String>` (構造体の欄や引数) を回せない (`要素の借用の種類 (Vec / String) が分からない`)。`String == "unit"` は比較の両辺の型が違うと言われる (`as_str()` が要る)。`match` の束縛から取った `&[u8]` を関数の値として返すと、借用検査が `does not live long enough` (E3010) と言う。どれも添字での回し・`as_str()`・局所への取り出しで回避した (`lower.cx.name_of` など)。
3. **E9040 の報告が根の理由を隠す。** `ksn build --release` は、最初に失敗した関数の「呼び先 X が未対応」の連鎖だけを出し、根の理由 (葉) は他の行に紛れる。根を探すために、関数を 1 本ずつ外して何度もビルドした。さらに `cache hit` の表示のもとで、古い結果を見て誤った判定をしかけた。根の理由を先頭に出す、または全件を出すとよい。

### 16.6 まだ書いていないこと

- `Option` / `Result` / `?`、`Vec` / `String` (`alloc.vec` / `alloc.string` の基本メソッドの単相インスタンス)、参照 (`&T` / `&mut T`)、タプル、関数値、クロージャ (14.4 章)。
- 総称の関数の単相化 (`key=` が型引数付きの鍵になる形)。
- 算術の溢れの検査 (`trap` と panic の枝)、`&&` / `||`、`-x`、`!` 以外の単項。
- 読み込み (`ksn kir` の `--opt`・`--llvm`) との比較。`unsupported` の関数は本体を出さないので、関数ごとの一致だけを数えた。
- 大きな入力 (数千行) での時間は測っていない。

### 16.7 再現

```
. ~/.cargo/env
cd stage0 && cargo build --release && cd ..
cd selfhost/lower
ksn check && ksn fmt --check && ksn build --release
python3 -I compare.py --group probe --mine native
python3 -I compare.py --group linear_ok --mine native --jobs 16   # 一致 47 / 439 (約 1 分)
python3 -I compare.py --group examples --mine native               # 一致 0 / 172
```

## 17. 統合の段 (修正プラン 5-5 の統合)

2026-11-15。ブランチ `selfhost/ksnc-stages`。変えたのは `selfhost/driver/` と、この節の追記だけ。`selfhost/` の他の部品 (parse・resolve・types・effects・borrow・fmt・lower・emit) の中身と公開の関数は変えていない (driver から path 依存で呼ぶだけ)。`stage0/`・`examples/`・`spec/`・`apps/`・`bench/`・`perf/` は変えていない。

結論を先に: **ドライバ `ksnc` の各段に Kasane 版を差し込んだ。** `ksnc --stages=kasane` は、検査 (名前解決・型・効果・借用)・整形・下ろし・LLVM 出力を Kasane 製の部品で行い、clang だけを外部のものにする。`--stages=both` は Rust 版と Kasane 版を並べて出す。既定 (`--stages=rust`) は従来どおり。作れるものはまだ少ない: 下ろしは linear_ok の 123 件のうち 81 件だけ作れ (その 81 件はすべて Rust 版と実行結果が同じ)、`ksnc` 自身は作れない (数は 17.9 で測り直した)。

### 17.1 使い方

```
cd selfhost/driver
ksn build --release                                  # Rust 版で ksnc を作る (既定の経路)
./target/release/ksnc.native --stages                # 段の表 (既定と --stages=kasane の担当)
./target/release/ksnc.native check [--stages=rust|kasane|both] [DIR]
./target/release/ksnc.native fmt   [--stages=rust|kasane|both] [--check] [DIR]
./target/release/ksnc.native build [--stages=rust|kasane|both] [--release] [--target host] [DIR]
python3 -I compare.py check|fmt --pkgs all|examples|apps|selfhost
python3 -I compare.py build [--limit N] [-v]         # linear_ok を作って、実行を Rust 版と比べる
```

- `kasane` の `build` の出力は `target/kasane/<release|debug>/<名前>.native` と `.ll` (Rust 版の成果物と分けて、上書きしない)。`--target` は host だけ (それ以外は失敗)。
- `kasane` の `fmt` は書き換え。`both` の整形は書き換えず、両方とも `--check` として並べる。
- `kasane` の `build` は、先に Kasane 版の検査 (名前解決・型・効果・借用) を通す。Rust 版の検査は通らない (Rust 版より寛容な検査で作ることがある)。
- 下ろせない関数があると、理由 (`unsupported FQN: 理由`) を出して失敗する。Rust 版へは戻らない。

### 17.2 差し替えの形

| 段 | 既定 (`rust`) | `kasane` の実装 (ファイル) |
| --- | --- | --- |
| 読み込み・名前解決 | Kasane (`resolve`) | 同じ (`ksnc/front.ksnl` の `front.text`: 診断の本文と数を返す) |
| 検査 (型・効果・借用) | Rust (`ksn check`) | Kasane: `types.export.run` → `effects` の本体・`borrow` の本体 (`ksnc/kcheck.ksnl`。`effects.main`・`borrow.main` の手順を組み直した) |
| 整形 | Rust (`ksn fmt`) | Kasane: `fmt.format_source` (`ksnc/kfmt.ksnl`。最終の改行は `fmt.process` と同じく付ける) |
| 下ろし | Rust (`ksn build` の中) | Kasane: `lower` の手順 (`ksnc/kir.ksnl`。`lower.main` の写し、本文は文字列で返す) |
| LLVM IR | Rust (`ksn build` の中) | Kasane: `emit.new_g` と `emit.module` (`ksnc/kbuild.ksnl`) |
| 実行ファイル | Kasane (`ksnc` の clang 段) | 同じ。kasane は ksnc が clang を直接呼ぶ (`kbuild.ksnl`)。実行時補助は `stage0/src/kir_rt.c`・`sysrt.c` を読むだけで写さない |

依存: `kasane.toml` に `types` `effects` `borrow` `fmt` `lower` `emit` を path 依存で足した (`kasane.lock` は `ksn update` で更新)。Rust 版の経路 (既定) は、ksnc 自身の構築も従来どおり動く: `KSNC_SELF` に複製を渡して `ksnc build --release` を回し、作った 1 段目 (gen1) と、それで作った 2 段目 (gen2) のバイト列が同一 (不動点)。

### 17.3 到達度の表 (段 / 対象 / 一致 / 残り)

| 段 | 対象 | 一致 | 残り |
| --- | --- | --- | --- |
| 1・2 読み込み・名前解決 (`resolve`) | 検査の 34 パッケージ | 検査の表 (下の行) に含む。単独では数えていない | E1020 (未使用の変数・引数)・E1021 (シャドーイング) など、`resolve` の外の検査は Rust 版だけ |
| 3 検査 (`types`・`effects`・`borrow`) | examples 16・apps 4・selfhost 15 (計 35 パッケージ、17.9 で測り直し) | **35 / 35** (すべて一致。`apps/wiki` の不一致は 17.10 で直した: Kasane 版が E2021・E1119 を 7 件出していた。Rust 版が 0 件のもののうち Kasane 版も 0 件: 34/34) | 残りの不足は 17.5 の 2 (let の経路で欄を区別しない形) |
| 4 整形 (`fmt`) | 34 パッケージ | **34 / 34** (書き換えの要否のファイルの集合が Rust 版と同じ。書き換えた後の内容の一致は、1 件 (`examples/tour` に末尾の空行を足した複製) で確かめた) | `ksn fmt` の lint 行 (`not auto-fixable`) と文法検査 (`grammar.check`) は Kasane 版になし |
| 5 下ろし (`lower`) | linear_ok 123 件 (Rust 版 `ksn build --release` が作れたのは 123 件、17.9 で測り直し) | 作れた **81 / 123** (作れた 81 件は、実行の出力・終了コードが Rust 版と一致) | 作れなかった 42 件の理由 (1 ファイルに複数の理由があるので合計は 42 を超える): 本体のない宣言 18、式の形 13、型の下ろし 10、名前が局所でない 3、panics の値 2、Vec のメソッド 1、Diag 1、match 1、for 1、理由が `Tool` のもの 5 |
| 6 LLVM 出力 (`emit`) | 下ろせた 81 件 | **81 / 81** (実行の標準出力と終了コードが Rust 版の実行ファイルと同じ) | `emit` の 13.4 章の範囲 (浮動小数・外部関数・`std.fs` など)。下ろしが広がらない限り対象が増えない |
| 7 実行ファイル (clang 段) | 81 件 | **81 / 81** (Kasane 版の clang 段で作れた件数と、上の一致の件数は同じ) | `--target` は host だけ (aarch64 などは失敗する) |
| 8 ksnc 自身 (`ksnc build --stages=kasane`) | `selfhost/driver` | **0** (作れない。17.9 で測り直し) | 下ろせない関数 78 件 (理由の件数: 型の下ろし (未対応の型) 31・本体のない宣言 23・文字列のエスケープ 7・呼び出しの解決先が記録されていない 6・for 4・`Vec` の `as_slice` 3・型の大きさが分からない 1・スライスの型 1・局所以外への代入 1・`String` の `push_u64` 1)。届く先が増えるのは、下ろしの範囲が広がったとき |

### 17.4 確かめ方と注意

- `compare.py check`: 検査は `ksn check` の `path:line:col: CODE` と、Kasane 版の `error CODE path:line:` を、(コード・パス・行) の多重集合で比べる。Rust 版の non-fatal 行 (`E0020`) は比べない。
- `compare.py fmt`: 書き換えの要否のファイルの集合を比べる。内容の一致は、一時の複製で `ksn fmt` と `ksnc fmt` の結果を比べて確かめた (`examples/tour` に末尾の空行を足したもの)。
- `compare.py build`: 作業場所は既定で `/tmp/ksnc_stages_work` (リポジトリの外)。各ファイルを `t` パッケージに入れ、`ksn build --release` と `ksnc build --stages=kasane --release` を作って、標準入力を空にして実行する (10 秒の制限)。
- 比べ方の注意: `ksnc` は作業ディレクトリを `PWD` から得る (`cwd=` だけでは変わらない)。最初の比べ方ではこれを忘れ、別のパッケージを見ていた (検査の一致 34 が偽だった)。`compare.py` は `PWD` を合わせるようにした。

### 17.5 処理系・部品の不足 (重要度順)

1. **下ろしの範囲** (表の 5): 型の下ろし (`Option` `Result` `Vec` `String` 参照) と、署名だけの標準ライブラリの呼び出しが、ほぼすべての関数に当たる。`ksnc` 自身を作るにはこの 2 つが要る (表の 8)。
2. **借用の誤検出 (let の経路)**: 直した (17.8)。同じ呼び出しの形は 17.7、`let a = &mut c.ar` と `let b = &c.tc` を順に書く形は 17.8。
3. **構文解析の不足 (`is` を引数に書く形)** (解決): `o is Some(_)` を呼び出しの引数に書くと、`selfhost/parse` が `expected ')'` で止まっていた (`stage0/tests/data/unsound/is_linear_payload_discarded.ksnl`。Rust 版は通す。`cmp_expr` で `is` パターンを処理し `Match` ノードに下ろす形で直した。unsound の取りこぼしは解消)。
4. **検査の不足** (表の 3(b)): E4013 は 17.7 で直した。
5. **IR を外へ出す入口がない** (11.4 章 1 と同じ): `ksn` に IR を外へ出す CLI がない。`ksnc` は `emit.module` を直接呼び、`lower.main` の手順は表示しない版 (`kir.ksnl`) に写した。
6. **Kasane の `build` の検査は、Rust の検査より寛容** (表の 3 の残り): 型・効果・借用の検査で、Kasane 版が見落とすものは、`kasane` の `build` でも通る。`unsound/` の一部がそれ。

### 17.6 まだ確かめていないこと

- `examples/*` と `apps/*` の `build --stages=kasane` は、下ろしの範囲の外なので作れないと見込んでいる (試していない。`examples/` を変えないように、一時の複製でだけ試す場合は別に)。
- `linear_ok` の検査 (`check --stages=kasane`) の一致は数えていない (`compare.py check` は examples・apps・selfhost の 34 パッケージだけ)。
- `fmt` の Kasane 版が書く内容は、`ksn fmt` の結果と一致したのは一時の複製 1 件の確かめ。全件の内容の一致は数えていない (`fmt` の一致は書き換えの要否の集合)。
- `--stages=both` の build の出力 (両方の実行ファイルの比較) は作っていない。
- `aarch64` などの `--target` は試していない (kasane は失敗する)。


### 17.7 修正 (修正プラン 5-5 の統合の後始末、2026-11-15)

ブランチ `selfhost/fixes`。変えたのは `selfhost/parse`・`selfhost/borrow`・`selfhost/effects`・`selfhost/driver` だけ (`stage0/`・`examples/`・`spec/`・`apps/`・`bench/`・`perf/` は変えていない)。

- **値としての `if let`** (`selfhost/parse`): `let x = if let P = e { .. } else { .. }` を構文エラーにしていた。`parse.expr.if_let_rest` を足し、`stage0/src/parser.rs` と同じく `match 式 { P => {..}, _ => else側 }` に下ろす (else が無ければ空のブロック)。確かめ: `linear_ok` 118/118 が通る (修正前 117/118。`if_let_ref_scrut_trim_ok` が通るようになった)。`compare.py` の一致は下がっていない (examples・stdlib・bench・self の一致は前後とも同じ)。
- **借用の誤検出 13 件** (`selfhost/borrow`): 原因は、同じ呼び出しの引数で根 (`c`) の別の欄 (`&mut c.ar` と `&c.tc`) を借りる組を、欄を見ずに競合と数えていたこと (`walk.check_pairs`)。根の直下の欄 (`borrow.cx.first_field`) を借用ごとに記録し、欄の名前が違う組は競合と数えない。欄が決まらない形 (根そのもの・添字) は従来どおり競合と数える。確かめ: `selfhost/lower` の E3010 が 13 → 0。`compare.py --group all` の結果 (誤検出 0、拒否の一致 48、DIFF の一覧) は修正前と同一。手で作った例で Rust 版と一致 (`&mut c.ar`・`&c.tc` は通る。同じ欄 `&c.ar` と `&mut c` と `&c.tc` は E3010)。
- **E4013** (`selfhost/effects`): 再帰する型に `core.ops.Drop` の明示実装か `derive [Drop]` が無いもの。`effects/drop.ksnl` を足し、`effects.main` と driver の `kcheck` (効果の段) から呼ぶ。再帰は型の欄に書かれた型名の有向グラフの閉路 (Rust 版 `recursive_types` と同じ)。確かめ: `probes/boxtree` が E4013 を出す (Rust 版と一致)。`compare.py` の比べる集合に E4013 を足した (diag E40 は 7/13 → 9/15 で、E4013・E4013b が一致)。clean・linear_ok で誤検出 0。手で作った例 (`derive [Drop]`・手書き Drop・Option 経由の自己参照は通す。相互再帰・自己参照は出す) は Rust 版と完全に一致。
- **判定**: `ksnc check --stages=both` の 34 パッケージの一致は **32/34 → 34/34** (修正前の driver を一時的に戻して測った)。

残り (この節で直していないもの): 17.5 の 2 (let の経路の欄の誤検出)。

### 17.8 借用の誤検出 (let の経路の欄)

2026-11-15。ブランチ `selfhost/borrow-fields`。`selfhost/borrow` の借用の状態を (根の局所, 欄の道) で持つ形にした。新規は無し (`selfhost/borrow/src/borrow/cx.ksnl` と `walk.ksnl` の修正と、この節の追記だけ)。

- 原因: 借用 `Br` が根の局所だけを持ち、`access` が根単位で競合を見ていたので、`let a = &mut c.ar` の後の `let b = &c.tc` が E3010 になった。
- 直し: `Br` に欄の道 `ps`・`pe` (ソース上の範囲。空は根全体) を足し、`access_at` は道が前置関係で重なるときだけ競合とする (`paths_overlap`)。道は `place_path` が式から求める (添字より先は見ない: `c.v[i].x` は `v`。関数名 `c.ar.len` は受け手の `ar`)。同じ呼び出しの組 (17.7) も同じ道の判定を使う。`first_field` と `same_field` は消した。
- 確かめ: 手で作った例 (`h1` = `let a = &mut c.ar; let b = &c.tc`、`&mut c.ar` と `c.tc = ..` など 10 本) は Rust 版と一致。`compare.py --group all` は修正前と同一 (誤検出 0、拒否の一致 54、DIFF の一覧は不変、undec 96)。`selfhost/borrow` 自身の undec は 3 のまま。
- 別に見つけたもの (直していない):
  1. **`borrow.base.block_end` は関数の本体の終わりを正しく求めない。** 本体が `fn ... end` (波括弧を持たない) のとき、本体の中の最初の波括弧の組 (例: 構造体リテラル `t.C { .. }`) の閉じで本体が終わったとみなす。そのため、後の文で使われる借り手が「もう使われない」と判定され、E3010 を取りこぼす (`let mut c = t.C { .. }` の関数で `let a = &mut c; let b = &c; *a = ..` が通る。Rust 版は E3010)。取りこぼしの方向で、誤検出ではない。
  2. **`selfhost/driver` が HEAD で作れない**: `src/ksnc/kir.ksnl` の `lower.cx.new`・`lower.expr.enqueue`・`lower.func.lower_item` の引数の数が `selfhost/lower` と合わない (E2001)。この節の変更とは無関係。そのため `ksnc check --stages=both` の 34/34 は今回は測れず、代わりに借用の段を 32 パッケージ (examples 16・apps 3・selfhost 13) で Rust 版 `ksn check` と比べ、E30xx が両方 0 件で一致した。
  3. `selfhost/borrow` の `ksn check` が HEAD で通らなかった (E2042: `walk.ksnl` の `if ln.kind == Index { expr(..)? }` の値を捨てていた)。`core.mem.drop` で捨てる形に直した。

### 17.9 driver の API 合わせと再発防止 (2026-11-15、ブランチ `selfhost/driver-api`)

- 原因: `selfhost/lower` の公開の関数 (`lower.cx.new`・`lower.expr.enqueue`・`lower.func.lower_item`) の引数が、型の表を使う拡張で変わったのに、`ksnc/kir.ksnl` が古い呼び方のままだった (E2001)。また `lower.cx.Glob` の欄名が変わっていた (`names` → `nms`、`texts` → `tx`)。
- 直し: `ksnc/kir.ksnl` を `lower.main` と同じ手順に合わせた (`types.export.run` → `lower.cx.new(tout.ar, tout.cx)` → `lower.expr.enqueue` → `name_of`・`item_at`・`subs_at`・`ret_at` → `lower.func.lower_item(.., key, &subs, ret)`)。`selfhost/lower` は変えていない。ドライバが呼ぶ関数は、この段が使うものだけ。
- 測り直し (`selfhost/lower` の統合 `1fa7f31` を取り込んだ後):
  - `compare.py check --pkgs all`: 35 パッケージ中 35 が一致 (17.3 の表の 3 行目)。
  - `compare.py build`: linear_ok 123 件のうち 81 件が作れ、すべて Rust 版と一致 (17.3 の表の 5〜7 行目)。
  - `ksnc build --stages=kasane` の自己ビルド: 0 件 (78 件が未対応。17.3 の表の 8 行目)。
- 再発防止: `tests/run_tests.py` の段 11 で、`selfhost/*` の全パッケージ (`selfhost/probes/*`・`selfhost/lower/probes/*` を含む、`target/`・`broken/`・`tests/` を除く 15 件) の Rust 版 `ksn check` の診断を見る。既定は 0 件。期待で診断を出すパッケージだけを `SELFHOST_EXPECT` の表に書く (`selfhost/probes/boxtree`: E4013 1 件)。表にない診断・件数の違い・表に書いたパッケージが無いことは失敗にする。段の時間は、全件検査の実行では 36.6 秒 (他の実行と並行した負荷時。`ksn check` を 15 件、直列)。無負荷の計測では 12.6 秒。
- 注意: 段 11 は Rust 版 `ksn check` だけを見る。Kasane 版 (`ksnc --stages=kasane check`) の一致は見ていない (`compare.py check` が担当)。

### 17.10 `if let` / `is` の暗黙のキャッチアール腕と `std` 可視性の誤検出 (2026-11-15)

- **E2021 (match misses variants / catch-all arm)** (`selfhost/parse` + `selfhost/types`): `if let` / `is` の暗黙の「他の全部」腕が `PWild` (フラグなし) として出力され、`types.infer.is_catch` がどの `PWild` もキャッチアールと見なして E2021 を出していた。`stage0/src/check.rs` と同じく、`PWild [true]` (フラグ付き) はキャッチアール対象外とする。`parse.expr.if_let_rest` と `parse.expr.cmp_expr` (`is`) で、暗黙のキャッチアール腕の `PWild` にフラグ子 (`add1` で `Lit` を1つ持つ) を付ける。`is_catch` はフラグ付き `PWild` で `false` を返し、`matchx` はフラグ付き `PWild` 腕があるときは「match misses variants」チェックをスキップする (`from_if_let`)。かつキャッチアール腕の `E2021` もスキップする (`!from_if_let`)。
- **E1119 (visibility)** (`selfhost/resolve`): `resolve.scope.lookup_key` が非 `pub` の定義を別ファイルから参照すると常に `Private` を返していたが、`stage0/src/check.rs` と同じく `core`/`alloc`/`std` のルートは可視性チェックの対象外とする。`std.http.Decoder.feed_part` などが別パッケージから参照できる。
- 確かめ: `apps/wiki` が `ksnc --stages=kasane check` で 0 件 (`compare.py check --pkgs all` が 35/35)。`selfhost/types`/`selfhost/resolve`/`selfhost/parse` の `ksn check`/`ksn fmt --check`/`ksn build --release --allow-bundle` が通る。最小の再現コードを 8 本 (`e1119_feed_part` `e1119_consume` `e2021_iflet_some` `e2021_iflet_none` `e2021_iflet_no_else` `e2021_is_op` `e2021_result_ok` `e2021_iflet_chain`) で Rust 版と Kasane 版の両方で 0 件を確かめた。

## 18. KIR への下ろしの段 (2): 型の表を使い、Option / Result / 参照 を下ろす (修正プラン 5-5)

2026-11-15。ブランチ `selfhost/lower-2` (`dev/plan` から)。`selfhost/lower/` を直した。新規は `selfhost/lower/src/lower/opt.ksnl` と、この節の追記だけ。`stage0/`・`examples/`・`spec/`・`apps/`・`bench/`・`perf/` と他の `selfhost/` 部品は変えていない。

結論を先に: **式ごとの型の推論のやり直しをやめ、型検査の表 (`types.export.run` の `types.rec.ty_of` / `callee_of`) から型と呼び出しの解決先を取る。** 局所の並びは下ろしの側で持つ。その上で `Option` / `Result` / `?` / 参照 (`&`・`&mut`・`*`・参照越しのフィールド) / 借用される局所のスロット / 列挙子の構築 / Option・Result の `match` / 整数の定数 / 値のない変種を下ろした。`Vec` / `String` (基本メソッドの単相インスタンス) と総称関数の単相化、`str` は未着手。

| 対象 | 前 (16 章) | 後 |
| --- | ---: | ---: |
| `linear_ok` (439 関数): 下ろせた | 90 | **193** |
| 同: Rust 版の `ksn kir` と一致 (番号の正規化後) | 47 | **94** |
| `linear_ok` の全体が下ろせた本 (`t.main` が呼ぶもの全部) | 9 | **46** |
| 同: `selfhost/emit` → clang → 実行の出力・終了コードが Rust 版と一致 | 9 / 9 | **46 / 46** |
| `examples` (172 関数): 下ろせた / 一致 | 4 / 0 | 14 / 6 |
| `examples/demo`・`tour` | 2 と 0 / 0 と 0 | 2 と 0 / 0 と 0 (`demo` は所有の注釈 `own.consume` の差) |

- 型の表: `types.export.run` を一度だけ呼び、arena・`types.chk.Ctx` (表 `rec` を含む) を下ろしに渡す。式の型は `types.rec.ty_of`、`f(..)` の解決先は `types.rec.callee_of` で読む。`types.infer.expr` / `let_stmt` / `pat` の呼び直しはなくした。局所は `Vs.sl` (名前・型・スロットの印・借用される名前) で持つ。
- 参照: 借用される局所 (`&x` / `&mut x` の `x`。関数の中の走査で集める) はスロット (`alloca` の `mutref<T>`)。読みは `load`、`&x` は `asshared`、`&mut x` は `reborrow`、`&*r` は再借用。一時の値の参照は一時のスロットを作る。参照越しのフィールドは `fieldaddr` + `load`。パターンの既定の束縛で参照になるものは、値をスロットに置いて参照にする。
- Option / Result: `some` / `none` (`const.Option<T> none`)、`ok` / `err`、判別は `tag` (Option は Some、Result は Err のとき真)、中身は `payload`。`?` は `tag` で分けて、失敗は戻り値の型で `err` / `none` を作って返す。

### 18.1 残り (下ろせていない 377 件の理由、`linear_ok`)

| 理由 | 件数 | 中身 |
| --- | ---: | --- |
| 本体のない宣言 | 96 | `std.io`・`Vec` の基本メソッドなど、`.sig` だけの組み込み。Rust 版の `kir_lower_*.rs` の写しが要る |
| 名前が局所でない | 56 | 定数 (整数以外の綴り) と関数の値 |
| 総称の関数 | 46 | 単相化 (`key=f<u32>` の鍵、型引数の代入で本体を下ろす) が要る |
| メソッド呼び出し (`wrapping_*` 以外) | 45 | 受け手の自動参照 (`&self`) と、基本メソッドの単相インスタンス |
| `let` の形 | 34 | 分解の束縛 (タプル・構造体) |
| 型: `str` / `alloc.vec.Vec` / 関数の型 ほか | 約 40 | `&str` の `fat` 表現、`Vec` の枠、関数値 |

### 18.2 処理系の不足 (この段で当たったもの)

1. 型検査の表には「自動参照・自動逆参照 (メソッドの受け手)」と「パターンの既定の束縛モード」の記録がない。下ろしは `Option` の match で値を読み、参照束縛は値をスロットに置く形で補った (Rust 版と同じ形ではない箇所がある)。
2. `ksn build --release` の E9040 の根の理由は、連鎖の先頭に隠れる (16 章の 14.5 章 3)。この段でも、根を探すのに関数を外して何度も作った。
3. 所有の注釈 (`own.consume` / `own.use`、線形の値の移動) の出力を写していない。`demo.shape.area` などの差はこれ。

### 18.3 再現

```
cd selfhost/lower && ksn check && ksn build --release
python3 -I compare.py --group linear_ok --mine native --jobs 16    # 一致 94 / 全体 439 (下ろせた 193)
python3 -I compare.py --group examples --mine native                # 一致 6 / 全体 172 (下ろせた 14)
```

## 19. KIR への下ろしの段 (3): 総称・文字列・入出力・for・Vec・メソッド (修正プラン 5-5)

2026-11-15。ブランチ `selfhost/lower-3` (`dev/plan` から)。`selfhost/lower/` を直した。新規は `src/lower/inst.ksnl` `io.ksnl` `loops.ksnl` `vec.ksnl`、既存の変更は `expr.ksnl` `opt.ksnl` `tyk.ksnl` `cx.ksnl` `func.ksnl` `lower.ksnl`。他の部品は変えていない。

結論を先に: **総称の関数とメソッドの単相化、`&str`、`std.io.print_line`、整数の範囲の `for`、フィールドへの代入、利用者のメソッド (自動参照)、`Vec` (`new` `push` `pop` `len` `is_empty` と `v[i]`) を下ろした。** `examples/demo` と `tour` はまだ全部は下ろせない (残りは 19.3 章)。

| 対象 | 前 (18 章) | 後 |
| --- | ---: | ---: |
| `linear_ok` (439 関数): 下ろせた | 193 | **291** |
| 同: Rust 版と一致 (番号の正規化後) | 94 | **115** |
| `linear_ok` で `t.main` が呼ぶものがすべて下ろせた本 | 46 | 48 (48 / 48 が emit → clang → 実行で Rust 版と一致) |
| `examples` (172 関数): 下ろせた / 一致 | 14 / 6 | **35 / 11** |
| `examples/demo` (4 関数) | 2 / 0 | 3 / 2 |
| `examples/tour` (7 関数) | 0 / 0 | 2 / 0 |

- 単相化: 鍵は `fqn<型,..>` (Rust 版と同じ形)。型引数は、宣言の引数の型 (型検査の表に型引数 `Param` のまま記録) と実際の引数の型の照合で決める。受け手 (`self`) も同じ照合で決まるので、総称の型の impl のメソッドも下ろせる。本体の下ろしは代入 (`lower.inst.subst`) を型の表の読み出し (`lower.expr.tyof`) と型の文字列 (`lower.tyk.text`) に作用させる。
- `&str`: 集成体 `fat` = {ptr, len} (擬似の項目 `lower.tyk.FAT`)。文字列の値は `static.bytes <16 進>` と `make`。`std.io.print_line` は `write` を fd 1 に 2 回 (本文・改行) 出す形で、Rust 版 `kir_lower.rs` の `print_line` と同じ命令列。
- `for`: 整数の範囲 `core.iter.range(lo, hi)` だけ。先頭で `icmp.lt`、続きのブロックで `add.T.wrap` の 1 増やし、可変の局所は引数で引き回す (Rust 版 `kir_lower_for.rs` と同じ形)。
- `Vec`: 枠 `vec` = {ptr, len, cap} (Rust 版と同じ 24 バイト)。`push` は `cap * 2 + 4` に `heap.realloc` で広げ、末尾へ `mem.store`。`pop` は空なら `none`。`v[i]` は `index.check` の後で `mem.load`。Kasane の本体 (`Buf` を使う版) は下ろさず、命令を直に作る。
- 自動参照: メソッドの受け手は、宣言の `self` が参照なら番地 (借用される局所はスロット、参照は値のまま)、値なら読み出す。`Vec` のメソッドの `self` は常に参照扱い (宣言が `.sig` で型の表に記録がないため)。

### 19.1 検証

- 完全な 48 本 (`t.main` が呼ぶものが全部下ろせた本) の emit → clang → 実行: 48 / 48 が Rust 版と標準出力・標準エラー・終了コードまで一致。
- 追加の確かめ (`/tmp` の小さな入力): `print_line("hello")` と `Vec` の push・pop・len・添字は Rust 版と一致。

### 19.2 まだ書いていないこと

- 所有の注釈 (`own.consume` / `own.use`)。実行の意味は変えないので e2e は通るが、テキストは Rust 版と違う (`demo.shape.area` など)。
- `String` (基本メソッドの単相インスタンス)、`str` のメソッド、`&[T]` のスライス、`Vec` の反復 (`for x in v.iter()`)、`core.iter.fold`・`core.check.eq`・`core.panic.panic`・`from_str_radix`・`try_from_u32`・`BtreeMap`。
- 利用者の列挙型の値のない変種の値 (`E.B`) は `make.vK()` で下ろす。Option / Result の変種の値は `None` だけ。
- `selfhost/parse` と `selfhost/driver` (ksnc) の自身の下ろし: まだ試していない (`Vec<String>`・`String`・`&[T]`・`BtreeMap`・`HashMap` が要る)。

### 19.3 残り (`linear_ok` の未対応 359 件の理由)

| 理由 | 件数 | 中身 |
| --- | ---: | --- |
| 本体のない宣言 | 106 | `std`・`core` の組み込み (`String`・`fold`・`eq`・`panic`・`BtreeMap` など) |
| 名前が局所でない | 55 | 整数以外の定数・関数の値・`Option` 以外の変種の値 |
| 型: その他 | 53 | `String`・`&[T]` などの未対応の型 |
| `let` の形 | 34 | 分解の束縛 (タプル・構造体)、初期化式のない `let` |
| 型引数の未解決 | 29 | 総称の型の中の `Param` が代入に無い形 (要調査) |
| 式の形・`panics` の値・`match` の形 | 約 40 | 整数・`bool` の `match`、`panics may` の算術の溢れ検査 |

### 19.4 再現

```
cd selfhost/lower && ksn check && ksn build --release
python3 -I compare.py --group linear_ok --mine native --jobs 16   # 一致 115 / 下ろせた 291
python3 -I compare.py --group examples --mine native               # 一致 11 / 下ろせた 35
```

## 20. KIR への下ろしの段 (4a): 標準ライブラリの呼び出し (修正プラン 5-5)

2026-11-15。ブランチ `selfhost/lower-std` (`dev/plan` から)。新規は `selfhost/lower/src/lower/std_calls.ksnl` (`lower.std_calls`) と、この節の追記だけ。既存の変更は、呼び出しの分岐と E2042 の修正 (下の 20.4 章) で、`expr.ksnl` と `opt.ksnl` に数行。`stage0/`・`examples/`・`spec/`・`apps/`・`bench/`・`perf/` と他の `selfhost/` 部品は変えていない。

結論を先に: **本体のない標準の宣言のうち、Rust 版が KIR の命令に下ろすものを表 (`lower.std_calls`) で下ろした。** 表は 7 件 (自由関数 2・メソッド 5)。呼び出し側は、解決先の FQN が表にあるときだけそこへ回す。`linear_ok` の一致は 119 → **120**、下ろせた関数は 301 → **302**。`examples` の下ろせた関数は 35 → **39** (一致は 11 のまま)。端から端まで (emit → clang → 実行) で Rust 版と一致する本は 51 件のまま、差は 0 件。

### 20.1 下ろした宣言

| 宣言 (FQN) | Rust 版の KIR | この段の形 |
| --- | --- | --- |
| `alloc.string.String.new(SYSTEM)` | 枠 `make(null, 0, 0)` (`Vec.new` と同じ) | `lower.vec.new` を流用。ただし `String` の型そのものが下ろせない (20.3 章 1) ので、届いても止まる |
| `core.panic.panic("文言")` | `panic "文言"` の後に `unreachable` | 引用符の中の綴りをそのまま使う。`\` と `"` を含む文言は未対応 |
| `alloc.string.String.as_str` | `mem.load.ptr` / `mem.load.usize` (+8) / `make(p, l)` | 受け手は枠の番地 (参照の値) をそのまま使う |
| `core.str.len` | `field v, +8` | fat の値をそのまま使う (受け手が参照なら `load`) |
| `core.option.Option.is_none` | `tag` と `not.bool` | 受け手の値 (参照なら `load`) |
| `core.option.Option.is_some` | `tag` | 同上 |
| `core.option.Option.unwrap_or` | `tag`・`br_if`・`payload`・join の `br` | ブロックは some・none・join の順。既定の値は引数として先に評価する (Rust 版は `tag` の後に作る) |

- 受け手の評価は `lower.std_calls.recv_call` にまとめた。局所なら `lower.opt.local_val` (スロットなら読み出し)、式なら式の値。`lower.opt.recv_value` の一時スロットは作らない。
- 呼び出しの入口は 2 箇所: `lower.expr.call` (自由関数。`core.mem.drop` の前) と `lower.opt.user_method` (メソッド。先頭で判定)。どちらも表にない FQN は従来の経路 (本体のない宣言で止まる)。

### 20.2 数字

| 対象 | 前 (この段の開始時の実測) | 後 |
| --- | ---: | ---: |
| `linear_ok` (448 関数): 下ろせた | 301 | **302** |
| 同: Rust 版と一致 (番号の正規化後) | 119 | **120** |
| `examples` (172 関数): 下ろせた / 一致 | 35 / 11 | **39 / 11** |
| `linear_ok` の「本体のない宣言」の理由 (件数) | 108 | **35** |
| 端から端まで (`t.main` が全部下ろせた本 → emit → clang → 実行で Rust 版と一致) | 51 / 51 | **51 / 51** (差 0) |

- 「前」は 19 章の記録 (291 / 115) とは違う。この段の基底 (`d542ed4`、19 章の後の統合) で、表を外して測り直した値。
- 「本体のない宣言」の 108 → 35 は、同じ宣言が消えたのではない: `String.new` の 43 件は、届いても `String` の型で止まるので、「型の下ろし」の理由へ移った (44 件)。表で実際に下ろせた呼び出しは、`String.as_str` 18・`core.panic.panic` 6・`core.str.len` 4・`Option.unwrap_or` と述語 (少数)。
- 端から端まで: `linear_ok` の 123 本のうち 51 本が `t.main` まで全部下ろせて、Rust 版と出力・終了コードが一致した (差 0)。表の追加では、この集合は増えていない (表を外しても 51 件)。

確かめに使った小さな入力 (`/tmp` の 2 本、リポジトリの外):
- `t.main` が `unwrap_or` (Some と None)、`is_none`・`is_some`、`str.len`、`print_line` を使う: Rust 版と同じ 5 行を出す (`a ok` ... `len ok`)、終了コード 0。
- `core.panic.panic("boom here")` を通る: 標準エラーの `panic: boom here`、終了コード 101 (Rust 版と同じ)。

### 20.3 残り (この段で届いたが下ろせなかったもの)

`linear_ok` で `本体のない宣言` のまま残った 35 件 (延べ件数):

| 宣言 | 件数 | 理由 |
| --- | ---: | --- |
| `core.check.eq` | 11 | Rust 版は `u32.$eq` と失敗時の `$debug` の展開 (生成された関数) を作る。生成関数の仕組みがない |
| `std.thread.spawn` / `JoinHandle.join` / `Receiver.*` / `Sender.send` | 12 | 並行処理。Rust 版の `kir_lower_thread.rs` などの写しが要る |
| `core.str.trim` | 3 | 空白の走査の形 (Rust 版の専用の形) |
| `alloc.string.String.from_str` | 2 | `Result` と `AllocError` の型が要る |
| `core.slice.get` / `get_mut` | 3 | `Option<&T>` の値の形 (Rust 版は `carry` を使う) |
| `core.num.u32.wrapping_add`・`core.str.len` (残り 1) | 2 | 関数の形 (`u32.wrapping_add(a, b)`) と、受け手が式の形 |

1. **`String` の型が下ろせない** (最も効く未対応): `alloc.string.String` の値 (引数・局所) は `型の下ろし` で止まる (44 件)。`String.new` と `as_str` は表に入ったが、その型を使う関数はまだ下ろせない。`String` / `str` のメソッドを進めている別の担当の範囲。
2. **受け手の一時スロット (借用の印)**: `lower.expr.scan` は、局所に対する `&self` の呼び出しを名前だけで「借用」と数え、関数の先頭で `alloca` に置く。`Option` の述語・`str.len` の受け手は、そのため `alloca` / `store` / `load` の形になる (Rust 版は値のまま `tag` / `field`)。意味は同じで、テキストの一致だけが落ちる。直すには型を知る段で「借用」を決める必要がある (`scan` は型を持たない)。
3. **`unwrap_or` の順序**: 既定の値は引数として先に評価するので、`const` が `tag` の前に出る (Rust 版は後)。

### 20.4 処理系・部品の修正 (この段の基底で必要だったもの)

- **E2042 (文の位置の `if` の値の捨て)**: この基底の `ksn check` が、`selfhost/lower` の 3 箇所で E2042 を出し、ビルドできなかった (`dev/plan` にある検査が、この枝より後に入ったため)。直し方は 3 箇所の `if` の then 枝の末尾の値を `core.mem.drop(...)` で捨てる形 (`expr.ksnl` の `matchx`・`retx`・`unary` の 3 箇所)。意味は変えていない。`stage0/` は変えていない。
- 処理系の不足は 20.3 章 1・2 のとおり。`ksn build --release` の E9040 の根の理由は、この段でも先頭に出ない (16 章 14.5 章 3 と同じ)。

### 20.5 確かめ方

```
cd selfhost/lower && ksn check && ksn fmt --check && ksn build --release
python3 -I compare.py --group linear_ok --mine native --jobs 16   # 一致 120 / 全体 448 (下ろせた 302)
python3 -I compare.py --group examples --mine native               # 一致 11 / 全体 172 (下ろせた 39)
```

端から端まで (`t.main` が全部下ろせた本だけ): `selfhost/emit` の `emit.native` を `ksn build` で作り (`selfhost/emit/target/debug/emit.native`)、各 `linear_ok` の本を `lower.native` → `emit.native` → clang (`kir_rt.c`・`sysrt.c`) → 実行で、Rust 版の実行ファイルと標準出力・標準エラー・終了コードを比べた。作業は `/tmp` (リポジトリの外)。結果は 51 件が一致、差 0。


## 21. KIR への下ろしの段 (4b)

前の章 (19) の残りを下ろした段。目標の `examples/demo`・`tour` の全体と `selfhost/parse` 自身までは届いていない (下の「まだ」を参照)。

### 21.1 下ろしたもの

- `let` の分解: 構造体の形 `S { f: p, .. }`、組 `(a, b)`、`_`。値が参照 (`x: &(..)`) なら欄の番地から取り、非スカラーの欄は番地で束縛する (`&T`)、スカラーは読み出して値で束縛する (Rust 版の既定の束縛の形と同じ)。
- 組: 匿名の集成体 `tuple (T1,T2)` (位置の欄)。組の構築は `make`、読み出しは `field`。登録は綴りで重複を除く。
- 整数以外の定数 (`const K: T = ...`): 初期化式を定数の書かれたファイルの中で下ろす (`std.alloc.SYSTEM` などが入る)。
- 総称の型の中の呼び出し・構成子・局所の読み出し: 表の型が決まらないときは宣言から決める (`lower.inst.decl_ret` / `ret_of`)。局所の読み出しは局所の型を使う。
- `String`: 枠は `Vec<u8>` と同じ `vec` = {ptr, len, cap}。`new`・`from_str`・`as_str`・`len`・`push_str` のみ (バイトの写しは命令のループ、Rust 版 `append_str` と同じ形)。
- `for x in v.iter()` (要素の参照)・`for x in v` (要素の値) の `Vec`。添字の番号で回す。
- 整数の算術 `+ - * / %` は `add.T.trap` などの trap 形、単項マイナスは `neg.T.trap`、ビット演算は `and.T` / `or.T` / `xor.T`、`&&` / `||` は短絡 (ブロックと bool の結び)。
- 小数: 定数は Rust 版の表示 (`9.0`)、算術は `fadd.f64` 形 (trap なし)、比較は `fcmp`。型の決まらない小数は f64。
- 総称の構造体 (利用者の定義のもの): 型の引数ごとに具体化の集成体 (`t.Box<alloc.string.String<std.alloc.System>>`)。欄の型は定義の総称の名前を `Param` として読み、具体化の代入を当てる。
- `Option` / `Result` の `match` の真偽の向き: `Err` が真、`Some` が真 (Rust 版と同じ)。

### 21.2 検証

- 完全なプログラム (`t.main` が呼ぶものが全部下ろせた本) の emit → clang → 実行: 85 / 85 が Rust 版と標準出力・標準エラー・終了コードまで一致 (前の段の 48 から増えた分は、総称の具体化・分解・String・Vec の反復などの追加による)。
- `linear_ok` 448 件: 一致 132 (前の段は 115)、下ろせた 392 (前の段は 291)。
- `examples/demo`: 一致 2 / 4 (下ろせた 3)。`examples/tour`: 一致 0 / 7 (下ろせた 4)。

### 21.3 まだ

- 標準の総称の型 (`core.iter.Range<T>`、`alloc.collections.BtreeMap<K,V,A>` など) の具体化は未対応。試すと `BtreeMap::new` でも下ろしが再帰して落ちたので、`lower.tyk.is_generic_adt` で `alloc.`・`core.`・`std.` の型を外してある (標準の型は前と同じく未対応の理由になる)。原因は未特定 (ここでの `ksn` の実行ファイルには backtrace がない)。
- `tour.main` の `wrapping_*` 以外のメソッド呼び出し、`tour.shapes` / `tour.csv_sum` の `for` (範囲と `Vec` 以外の対象) が残る。`demo.main` は `core.iter.Range` の値 (上の理由)。
- 所有の注釈 `own.consume` / `own.use` は出していない (実行は変わらないが、テキストは Rust 版と違う)。
- 型引数の未解決 (29 件) はまだ残る (総称の関数の型の値・`Param` の別の形)。
- `String` の残りのメソッド、`str` のメソッド、`&[T]` のスライス、配列 `[T; N]`。
- `selfhost/parse` 自身の下ろし: 未着手 (`String`・`&[T]`・`BtreeMap`・`HashMap` が要る)。

### 21.4 未解決の理由 (`linear_ok` の未対応 152 件)

| 理由 | 件数 | 中身 |
| --- | ---: | --- |
| 本体のない宣言 | 44 | 他の担当の `std` / `core` の組み込み (`std_calls.ksnl` 側) |
| 型の下ろし: 型引数 | 29 | 総称の型の中の `Param` が代入に無い形 |
| 式の形 | 18 | 未対応の式 |
| panics の値 | 16 | `panics` の値の扱い |
| match (列挙型以外) | 13 | 整数・`bool` などの `match` |
| 型: 関数の型 | 10 | 関数の値 |
| 型: その他 | 8 | 標準の総称の型 (上の理由) |

### 21.5 再現

```
cd selfhost/lower && ksn check && timeout 1500 ksn build --release
python3 -I compare.py --group linear_ok --mine native --jobs 16   # 一致 132 / 下ろせた 392
python3 -I compare.py --group examples --mine native             # demo 2/4, tour 0/7
```


## 22. KIR への下ろしの段 (4c)

2026-11-16。ブランチ `selfhost/lower-4c` (`dev/plan` の `fd52f66` から)。触ったのは `selfhost/lower/src/lower/` の 7 ファイル (`cx` `expr` `inst` `loops` `opt` `std_calls` `tyk`) とこの章だけ。`stage0/`・`examples/`・テストの期待値は変えていない。

結論を先に: **標準の総称の型の具体化が通り (「下ろしが再帰して落ちる」原因を特定)、Option / Result / 列挙の `match`・代入文・範囲の `for`・繰り返しの局所の順・数値のメソッドが Rust 版と同じ形になった。** `examples` の一致は 11 → **22** (`tour` 0/7 → 4/7、`plugins` 1/7 → 3/7)。`linear_ok` は 135 → **136**、下ろせた 401 → 403。`demo` は 2/4 のまま (残りの差は所有の注釈 `own.consume` だけ)。

### 22.1 下ろしたもの・直したもの

- **標準の総称の型 (`core.iter.Range<T>`・`alloc.collections.BtreeMap<K,V,A>` など) の具体化**: `lower.tyk.is_generic_adt` の「`alloc.`・`core.`・`std.` は外す」を外した。落ちた原因は 2 つ。(1) 本体のない宣言 (`.sig` の fn) は型検査の記録 (`types.rec.ty_of`) がないので、`lower.inst.args_of` / `decl_ret` / `method_args` が型引数を決められず、`core.iter.range` が `<>` (空の代入) のまま `Range<unit>` になり、Param が残った型を具体化しようとして展開が止まらなかった (スタックあふれ)。(2) 型引数が未決め (Param 入り) の総称の具体化 (`plugins.Bus.register<H>` を直接下ろす場合) も同じ。直し方: 本体のない宣言は署名の `[K, V, A]` と引数の注釈から読む (`lower.inst.sig_ty`。型引数の番号は項目ごとに決まった値で、`args_of` と `decl_ret` で食い違わない)。Param を含む総称の型は `is_generic_adt` が false を返し、未対応の理由として出る。これで `core.iter.range<u32>` / `alloc.boxed.Box.new<...>` の鍵が正しく出る。**ただし Rust 版 (`ksn kir`) 自身が、`core.iter.Range` / `BtreeMap` の値を返す・受ける関数を「段階 1 の対象外」としている** (`t.mk` を `ksn kir` すると断られる) ので、標準の総称の型を値として持つ関数は、一致の比較の対象にならない。`demo.main` の `core.iter.fold(core.iter.range(..), ..)` も同じ。型を引数に持つ関数 (`fn t.f(m: &BtreeMap<u32,u32,System>)`) は Rust 版と同じ集成体の行 (`struct alloc.raw.Buf<alloc.collections.MapEntry<u32,u32>,std.alloc.System>` まで) を出して一致する。
- **範囲の `for` のブロックの順**: Rust 版は 出口・続き・先頭 の順にブロックを作る。写した (`for i in core.iter.range(a, b)` の出力が一致)。
- **代入文**: 文の位置の代入は単位の定数を作らない (値は `store` そのもの)。ブロックの最後が代入なら、ブロックの終わりで単位を 1 つ作る (Rust 版と同じ)。
- **繰り返しで引き回す局所の順**: 宣言の順ではなく、名前が出た順 (Rust 版 `assigned` と同じ。`lower.cx.mlist_in`)。`loop` / `while` / `for` の `br b1(i, total)` の引数の順が揃う。
- **`Option` / `Result` の `match`**: Rust 版の形に揃えた。腕ごとに `tag` を読み直し (`Err` / `Some` 以外は `not.bool`)、部分パターン (`x` / `_`) があれば `payload` と `const.bool true` と `and.bool` で条件を結び、最後の腕は無条件の `br` (`unreachable` のブロックを作らない)。入れ子の部分パターンは、前は束縛を黙って落としていたが、今は未対応の理由で止める。
- **利用者の列挙の `match`**: 腕ごとに `tag` を読み直し、「次の腕」のブロックを「腕」より先に作る (ブロックの並びが Rust 版と同じ)。
- **数値のメソッド (`lower.std_calls` の表を拡張)**: 整数の `saturating_add/sub/mul` → `add.T.sat`、`checked_add/sub/mul` → `checked.add`、浮動小数の `sqrt` `floor` `ceil` `trunc` `abs` → `fsqrt.f64` など。受け手を値で取るので `lower.expr.is_wrapping` の名前の表に足し、`alloca` / `store` / `load` の一時を作らない (Rust 版と同じ形)。`tour.floats` と `tour.ints` の下ろしが一致した。

### 22.2 数字 (この段の開始時に測り直した値との比較)

| 対象 | 前 | 後 |
| --- | ---: | ---: |
| `linear_ok` (459 関数): 一致 / 下ろせた | 135 / 401 | **136 / 403** |
| `examples` (172 関数): 一致 / 下ろせた | 11 / 53 | **22 / 52** |
| `examples/demo`: 一致 / 全体 | 2 / 4 | 2 / 4 |
| `examples/tour`: 一致 / 全体 | 0 / 7 | **4 / 7** |
| `examples/plugins`: 一致 / 全体 | 1 / 7 | **3 / 7** |
| 端から端まで (emit → clang → 実行で Rust 版と一致 / `t.main` が全部下ろせた本) | 84 / 89 | 84 / 89 (差 0 件の変化。下の 22.4) |

- 「前」は 21 章の記録 (一致 132 / 下ろせた 392、demo 2/4、tour 0/7) と違う。`dev/plan` の `fd52f66` にある `linear_ok` が増えているので、この基底で測り直した値。
- `examples` の下ろせた 53 → 52 は、入れ子の部分パターンを止めるようにした分 (前は束縛が落ちた誤った KIR を出していた)。
- `ksn check`: `selfhost/` の全パッケージで診断 0 件 (`probes/boxtree` の E4013 だけ期待どおり)。`selfhost/lower` の `ksn fmt --check` は通る (`lower/probes/probe` と `borrow` の `fmt --check` の指摘は、この段の前からのもので触っていない)。

### 22.3 まだ

- **所有の注釈 (`own.consume` / `own.use` / `const.unit` の識別子)**: `linear_ok` の食い違い 267 関数のうち 164 が、Rust 版の KIR に `own.` を含む (残り 103 は別の理由)。Copy でない局所・引数の束縛で単位の定数を作り、値の読み出しで `own.consume`、部分の移動で `own.part` を出す規則 (`kir_lower.rs` の `own_declare` / `own_emit`、`src_is_copy` / `src_is_linear` が要る)。`demo.shape.area` の差はこれだけ。**次の段で最も効く。**
- **`Vec` / `String` の下ろしの形**: Rust 版は、局所の `String` / `Vec` を `frame.bytes 24` のスロットに置き、`v.len()` / `is_empty()` / `push` を `alloc.vec.Vec.len<T,A>` のような**生成された小さい関数の呼び出し**にする。`&String` / `&Vec` の引数の型は `ptr`。こちらは枠を `vec` の値として持ち、メソッドを命令にインライン展開し、引数を `ref<agg#N>` とする。`t.peek(s: &String) -> usize { s.len() }` のような関数 (`linear_ok` に多い) の食い違いの主因。同じ理由で、`String.as_str` を含む 5 本の本 (`temp_as_str_same_statement` ほか) は下ろせるが、emit した `.ll` が clang で通らない (`i192` を `ptr` として使う。この段の前から: 22.4)。直すには、枠をスロットで持つ形への変更と、小さい関数の生成が要る。
- **`core.check.eq`** (`examples` で 46 件): Rust 版は `u32.$eq` と、失敗時の `eprint` と `u32.$debug` (整数を文字に直す 2 つのブロックの関数) を生成する。型ごとの `$eq` / `$debug` の生成が要る。
- `for` の対象 (`for s in [a, b, c]`・`for cell in line.split(",")` など、範囲と `Vec` 以外) 13 件、スライス `&[T]` の型 16 件、配列 `[T; N]`、`Vec.as_slice` 7 件、`str` のメソッド (`starts_with` `split_once` `trim` `strip_prefix` `is_empty` `as_bytes` ほか)、`String.push_u64` / `is_empty`、`core.slice.len/get/sub`、文字列のエスケープ 8 件、`panics` の値 25 件 (`panics P` の総称)。
- 型引数の未解決 (`linear_ok` で 29 件) は、この段で直した本体のない宣言の分は減ったが、総称の関数の型の値・`Param` の別の形は残る。数え直していない。

### 22.4 処理系の不足 (stage0。直していない)

1. **`for x in v.as_slice().iter()` が、`v` が参照引数 (`&Vec<String>`) のとき `ksn build --release` で「要素の借用の種類 (Vec / String) が分からない」になり、LLVM に下ろせない。** `v` が局所の `Vec<usize>` のときは通る。再現 (`selfhost/lower/src/lower/cx.ksnl` の `mlist_in` を一時的に次の形にしたとき):
   ```
   fn lower.cx.f(names: &alloc.vec.Vec<alloc.string.String<std.alloc.System>, std.alloc.System>) -> usize uses [] panics may
       let mut n = 0usize
       for w in names.as_slice().iter() {
           n = n + w.len()
       }
       n
   end lower.cx.f
   ```
   (`ksn build --release` で E9040)。回避: `while` と `names.as_slice().get(i)`。
2. **`ksn kir` が `core.iter.Range<T>` / `alloc.collections.BtreeMap` の値を返す関数 (`fn t.mk() -> core.iter.Range<u32>`) を下ろせない** (「型 core.iter.Range は型引数付きで段階 1 の対象外」)。引数に取る関数は下ろせる。セルフホストの側が先へ進んでも、比較の相手がない。
3. 22.3 の `Vec` / `String` の形は処理系の不足ではなく、こちらの未実装。ただし `temp_as_str_same_statement` ほか 5 本は、`ksn kir` の出力と違う形の IR (`String` を `i192` の値で持つ) を emit が作り、clang が受け付けない (この段の前から。84 / 89)。

### 22.5 再現

```
cd selfhost/lower && ksn check && ksn fmt --check && ksn build --release
python3 -I compare.py --group linear_ok --mine native --jobs 8   # 一致 136 / 全体 459 (下ろせた 403)
python3 -I compare.py --group examples --mine native             # 一致 22 / 全体 172 (下ろせた 52)、demo 2/4、tour 4/7
```

端から端まで: 16.2 章・13.2 章の手順で、`lower.native` の出力 (`unsupported` の行がない本) を `selfhost/emit` の `emit.native` → clang (`kir_rt.c`・`sysrt.c`) → 実行し、Rust 版の `ksn build` の実行ファイルと比べた (作業は `/tmp`)。84 / 89 一致 (前も同じ)。

## 23. KIR への下ろしの段 (4d)

2026-11-16。ブランチ `selfhost/lower-4d` (`selfhost/lower-4c` の先頭から)。触ったのは `selfhost/lower/src/` (新規 `lower/own.ksnl`・`lower/chk.ksnl`、既存の `cx` `expr` `opt` `func` `inst` `tyk`、`lower.ksnl`) とこの章だけ。`stage0/`・`examples/`・テストの期待値は変えていない。

結論を先に: **所有の注釈 (`own.*`) が Rust 版と同じ規則で出るようになり、`linear_ok` の一致が 136 → 201、`examples` が 22 → 31、`demo` は 3/4 になった。** `core.check.eq` の呼び出し側と、`$eq` / `$debug` の生成も入った。`Vec` / `String` の形 (22.3 の 2 つ目) は手を付けていない。

### 23.1 下ろしたもの・直したもの

- **所有の注釈** (`lower.own`。Rust 版 `kir_lower.rs` の `own_declare` / `own_emit` と同じ規則): 局所ごとに注釈の識別子 (`const.unit ()` の値) を `c.v.sl.own` に持つ。Copy でも線形でもない型 (`lower.own.is_copy` / `is_linear`。`derive Copy`・`Option` / `Result` は引数の合成・線形は宣言の印と欄・変種の型を再帰して調べる) の引数と `let` の束縛で、束縛の位置に単位の定数を作る (線形なら続けて `own.owe`)。出す場所は次のとおり。
  - 局所の値としての読み出し: Copy でなければ `own.consume`。`局所.フィールド` は Copy でなければ `own.part`、Copy なら `own.use`。
  - `&x` / `&mut x` の局所、借用の受け手 (`x.len()` の `&self`): `own.use`。値の受け手 (`x.into_name()`): `own.consume`。
  - `Option` / `Result` の `match`: 対象が注釈つきの局所なら読むだけ (`own.use`)。腕の束縛は `own_declare`、非 Copy を束縛する腕 (と、線形な対象の `_` 以外の腕) の入口で対象を `own.consume`。利用者の列挙型の `match` は Rust 版も普通の読み (move) なので、そのまま。
  - 再代入: 注釈つきの局所なら `own.init`、線形な値なら `own.owe`。
  - これで `demo.shape.area`・`move_*`・`arm_*`・`temp_*` の差が消えた。入れ子の部分パターン・`let` のパターン・`with` の束縛・`?` や `break` の出口の注釈は未対応 (Rust 版の `drop_from` 側)。
- **`core.check.eq`** (`lower.chk`): 整数と bool。呼び出し側は Rust 版と同じ形 (`call T.$eq` → `br_if` → 失敗側は `eprint` と `call T.$debug` と `abort` と `unreachable`)。`T.$eq` / `T.$debug` は生成した関数として出力に入る (`main` の下ろしの繰り返しで、項目の印 `lower.chk.GEN` の鍵から文字列を作る)。`$debug` は符号なし・符号つき (`conv.i64.T` で広げて `-` を先に書く)・bool。char・小数・文字列は未対応の理由で止める。
- **`alloca` の位置**: Rust 版 `flush_allocas` と同じく、入口ブロックの先頭に集める (出力時に並べ替える)。ブロックの終わりで、そのブロックの局所のうちスロットのものを逆順に `scope.end`。
- **`?` のブロックの順**: 成功の枝を先に作る (Rust 版の並び)。
- **文字列・文字・バイトのリテラル**: 文字列のエスケープ (`\n \t \r \0 \\ \" \'`) を外した値で `static.bytes` を作る (`\x` / `\u` は未対応)。`'a'` は `const.u32 97`、`b'0'` は `const.u8 48` (UTF-8 も値にする)。**`char` の型の綴りを `char` から `u32` にした** (Rust 版の KIR で `char` は `u32`)。
- **小さい直し**: `uses=[a,b]` の区切りを Rust 版と同じ `,` (空白なし) に。`wrapping_shl` / `wrapping_shr` (`shl.T.wrap`)、`T.from_str_radix` (`parseint.T`)、`std.env.arg_count` (`env.arg_count`)。型引数のない総称の鍵は `f<>` ではなく `f`。

### 23.2 数字 (この段の開始時に測り直した値との比較)

| 対象 | 前 | 後 |
| --- | ---: | ---: |
| `linear_ok` (459 関数): 一致 / 下ろせた | 136 / 403 | **201 / 403** |
| `examples` (172 関数): 一致 / 下ろせた | 22 / 52 | **31 / 57** |
| `examples/demo`: 一致 / 全体 | 2 / 4 | **3 / 4** |
| `examples/tour`: 一致 / 全体 | 4 / 7 | 4 / 7 |
| 端から端まで (emit → clang → 実行で Rust 版と一致 / `t.main` が全部下ろせた本) | 84 / 89 | **91 / 98** |

- `demo` の残り 1 本 (`demo.main`) は 22.1 で書いたとおり `core.iter.fold(core.iter.range(..), ..)` を含み、Rust 版が段階 1 の対象外とするので、一致の相手がない。`tour` は 22 章と同じ 4/7。
- 端から端まで: `stage0/tests/data/linear_ok` の 127 本のうち、`t.main` が全部下ろせたものを `lower.native` → `emit.native` → clang → 実行して、Rust 版 `ksn build` の実行ファイルと標準出力・標準エラー・終了コードを比べた。91 本が一致、7 本が clang で通らない (調べた `p09`・`p12`・`e02_d22_move_s_after_v_dead` は `String` を `i192` の値で持つ形で、22.4 の 3 と同じ原因。`lower` が `t.main` を全部下ろせる本が 89 → 98 に増えた)。差のある実行は 0 本。作業は `/tmp`。
- `ksn check`: `selfhost/` の全パッケージで診断 0 件 (`probes/boxtree` の E4013 だけ期待どおり)。`selfhost/lower` の `ksn fmt --check` は通る。

### 23.3 まだ

- **`Vec` / `String` の形** (22.3 から変わらず。`linear_ok` の食い違い 202 関数のうち、最初の差がこの形のものが約 180): Rust 版は局所を `frame.bytes 24` のスロットに置き、`len` / `is_empty` / `push` を生成した小さい関数 (`alloc.string.String.len<std.alloc.System>` など) の呼び出しにする。`&String` / `&Vec` の引数・戻り値の型は `ptr`。こちらは枠を `vec` の集成体で持ち、メソッドを命令に展開し、型は `ref<agg#N>`。出力の形が型の綴りから変わるので、枠の持ち方 (`lower.vec`・`lower.str`・`lower.loops`・`opt.user_method` の全部) と、生成する小関数の本体 (emit が `mem.load` などを通す必要もある) をまとめて作り直す必要があり、この段では手を付けなかった。
- **`for` の対象**: 範囲と `Vec` 以外 (`[a, b, c]`・`xs.iter()` の `&[T]`・`split`) 13 件。**スライス `&[T]`** は `fat` の値 (`field %0, +0` が ptr、`+8` が len。`xs.iter()` の `for` は要素を `ptr.add` と `mem.load` で読む。`m.sum` の KIR を `ksn kir` で見られる) で、型の下ろし 22 件・`as_slice` 7 件・`str` のメソッド・`core.slice.*` と一緒に要る。
- **`panics` / `uses` を省略した関数** (`panics の値 (未対応)` が `examples` で 25 件): Rust 版は `infer.rs` の効果の推論 (不動点) で `uses` / `panics` を決めて頭に書く。`selfhost/effects` は宣言の検査で、推論はまだ。
- 注釈の残り: 入れ子の部分パターン・`let` のパターン・`with`・出口 (`return` / `?` / `break` / `continue`) の `own.*` / `scope.end`、一時値の `scope.end`。
- `shortener` ほか: クロージャ (`式の形 (未対応)`)・`HashMap` などの総称の標準型・`never` の型。

### 23.4 処理系の不足 (stage0。直していない)

22.4 の 3 つのまま。この段で新しく見つかったものはない。

### 23.5 再現

```
cd selfhost/lower && ksn check && ksn fmt --check && ksn build --release
python3 -I compare.py --group linear_ok --mine native --jobs 8   # 一致 201 / 全体 459 (下ろせた 403)
python3 -I compare.py --group examples --mine native             # 一致 31 / 全体 172 (下ろせた 57)、demo 3/4、tour 4/7
```

端から端まで: 22.5 と同じ手順 (`selfhost/emit` の `ksn build --release` で `emit.native` を作り、各 `linear_ok` の本を `lower.native` → `emit.native` → clang (`kir_rt.c`・`sysrt.c`) → 実行)。91 / 98 一致。

## 24. KIR への下ろしの段 (4e)

2026-11-16。ブランチ `selfhost/lower-4e` (`selfhost/lower-4d` の先頭から)。触ったのは `selfhost/lower/src/` (`lower/vec.ksnl`・`tyk` `opt` `expr` `loops` `std_calls` `func`、`lower.ksnl`) とこの章だけ。`stage0/`・`examples/`・テストの期待値は変えていない。

結論を先に: **`Vec` / `String` の形を Rust 版に揃えた** (局所は枠 `frame.bytes 24`、`&Vec` / `&String` は `ptr`、メソッドは生成した小関数の呼び出し、出口で解放)。`linear_ok` の一致が 201 → **345**、`examples` が 31 → **37**、端から端まで 91/98 → **98/98** (22.4 の 3 の `i192` で clang が通らない形も直った)。

### 24.1 下ろしたもの・直したもの

- **局所の `Vec` / `String` は枠 (`ptr = frame.bytes 24`)** (`lower.vec.is_hdr` で型から決める。借用の有無によらない): `let`・引数の束縛 (`lower.opt.bind`) で `field %v,+0/+8/+16` を読み、`mem.store` で 3 欄を書く。`frame.bytes 24` は `alloca` と同じく入口ブロックの先頭へ集める (`lower.func.body_text`)。
- **値としての読み出しは移動** (`lower.opt.local_val` → `lower.vec.load_hdr`): 3 欄を `mem.load`、先頭の番地を `const.ptr 0` で上書き、`make(ptr, len, cap)`。借りる文脈 (`c.s.peek`) は上書きしない。後ろに `own.consume`。
- **型の綴り**: `&Vec<T>` / `&mut Vec<T>` / `&String` は `ptr` (`lower.tyk.text`)。`&x` / `&mut x` は `borrow.shared` / `borrow.mut`。メソッドの受け手 (`v.len()`) は枠の値をそのまま渡し、`own.use` は出さない。一時値の `Vec` / `String` は枠に置いて番地を渡す (`recv_value`・`wrap_ref`・`std_calls.recv_call`)。
- **メソッドは生成した小関数の呼び出し** (`lower.vec.call` / `lower.vec.gen`): `alloc.string.String.{len,is_empty,push_str}<std.alloc.System>`、`alloc.vec.Vec.{len,is_empty,push,pop}<T,std.alloc.System>`。鍵の型引数は源の綴り (`lower.tyk.src`: `u32`・`&str`・`&alloc.string.String<std.alloc.System>`、省略した割り当て器は補う)。`as_str` だけは Rust 版と同じく展開 (枠から ptr と len を読んで `make`)。小関数の本体は **Rust 版と同じではなく、これまでの展開を関数にしたもの** (呼び出し側の形と、実行結果が一致する)。出力には `lower.chk.GEN` の項目として入る (`$eq` / `$debug` と同じ仕組み)。
- **解放**: ブロックの終わり (`lower.expr.block`)・本体の終わり (値で受けた引数。`lower.func`)・`return`・`?` の失敗の枝で、所有する枠を逆順に `mem.load.ptr` + `heap.free` + `scope.end`。`core.mem.drop(s)` は値の先頭の番地 (`field +0`) を `heap.free`。再代入は古い値を解放してから欄を書く。
- **`Vec` の `for`** (`lower.loops.forvec`): `len` と `ptr` をループの前に読み (Rust 版と同じ)、ブロックは 出口・続き・先頭・本体 の順、要素の番地は `ptr.add` (要素が 1 バイトなら `mul` を省く)。`.iter()` の要素は生の `ptr` で、`*x` は `mem.load.T x +0` になる (`lower.expr.unary`)。`v[i]` の読みは `index.check` の後に ptr と要素の大きさを読む順。

### 24.2 数字 (この段の開始時に測り直した値との比較)

| 対象 | 前 | 後 |
| --- | ---: | ---: |
| `linear_ok` (459 関数): 一致 / 下ろせた | 201 / 403 | **345 / 403** |
| `examples` (172 関数): 一致 / 下ろせた | 31 / 57 | **37 / 57** |
| `examples/demo`: 一致 / 全体 | 3 / 4 | 3 / 4 |
| `examples/tour`: 一致 / 全体 | 4 / 7 | 4 / 7 |
| 端から端まで (emit → clang → 実行で Rust 版と一致 / `t.main` が全部下ろせた本) | 91 / 98 | **98 / 98** |

- 端から端まで: 22.5 と同じ手順 (`linear_ok` 127 本のうち `t.main` が全部下ろせた 98 本)。差のある実行は 0 本、clang が通らない本も 0 本。`String` を `i192` の値で持つ形 (22.4 の 3) は、枠の番地で持つようにして消えた。
- `ksn check`: `selfhost/` の全パッケージで診断 0 件 (`probes/boxtree` の E4013 だけ期待どおり)。`selfhost/lower` の `ksn fmt --check` は通る。

### 24.3 まだ

- `linear_ok` の食い違い 114 関数のうち、最初の差が **`Vec` / `String` 以外**のものが大半: 構造体・列挙型・`Option` / `Result` の局所の「移したかの印」(`frame.bytes 1`。15 件)、`Option<&T>` の中の参照の型 (`ptr`)、欄の参照 (`fieldaddr` の型が `ref<T>` と `ptr` で食い違う)、`match` の腕の束縛の `scope.end`・一時の `Result` の解放 (`String::from_str` の `match` など)、`if` / `while` の合流で引き回す局所の組 (move の後の復活)。
- 一時値の `Vec` / `String` の `scope.end` と解放 (文の終わり) は未対応。`temp_as_str_same_statement` は実行は一致するが、KIR は違う (一時の枠を作る位置)。
- 生成した小関数の本体は Rust 版の形ではない (呼び出し側だけ一致)。`Vec` のメソッドは `len` `is_empty` `push` `pop`、`String` は `len` `is_empty` `push_str` `as_str` のみ。`Vec.get` `clear` `as_slice` `iter` など、`String.from_str` の Rust 版の形 (`heap.alloc` とコピーのループ) は未着手 (`from_str` は従来の展開)。
- **2 つ目の項目** (`core.check.eq` の char・小数・文字列、範囲と `Vec` 以外の `for`、スライス、`str` のメソッド) は、この段では手を付けていない。
- `lower.inst.key` (利用者の総称の関数の鍵) は `lower.tyk.text` のまま。`lower.tyk.src` を使うと Rust 版の鍵の綴りになるはずだが、確かめていない。

### 24.4 処理系の不足 (stage0。直していない)

新しく見つかったものはない。22.4 の 1・2 は変わらず、22.4 の 3 はこの段で解消した。

### 24.5 再現

```
cd selfhost/lower && ksn check && ksn fmt --check && ksn build --release
python3 -I compare.py --group linear_ok --mine native --jobs 8   # 一致 345 / 全体 459 (下ろせた 403)
python3 -I compare.py --group examples --mine native             # 一致 37 / 全体 172 (下ろせた 57)、demo 3/4、tour 4/7
```

端から端まで: `selfhost/emit` を `ksn build --release` して、各 `linear_ok` の本を `lower.native` → `emit.native` → clang (`kir_rt.c`・`sysrt.c`) → 実行し、Rust 版 `ksn build` の実行ファイルと比べる。98 / 98 一致。
