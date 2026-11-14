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
