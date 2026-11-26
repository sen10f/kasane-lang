# apps/*/FRICTION.md の再確認 (2026-11-26、dev/plan `0cec620a`)

外部レビュー「摩擦 23 件の多くは KIR・ネイティブに下りないための書き換え (E9040 など、11/15 前後の記録)」を、現行の dev/plan で確かめた。
各項目の元の自然な形を最小のプログラムにし (`~/coord_tmp/friction_probes/<名前>`、使い捨て)、`ksn check` / `ksn run` (VM 既定) / `ksn build --release` / `ksn test --both` を回した。結果は下表。
アプリは戻した前後で `ksn test --both` が全件通る (notes 9、todo 26、wiki 108。`llvm` 一致・退避 0)。wiki は戻す前後のバイナリで同じ要求 (`/new` `/list` `/nope`、不正な CSRF の POST、壊れた要求の 400) の応答コードが一致することも確かめた。

## 結果の数

| 区分 | 件数 |
| --- | --- |
| 解消済み (元の形が今は通り、ネイティブ・VM で同じ結果) | 16 |
| 検査器が拒む (意図どおり) | 1 (todo 3) |
| 処理系の不足ではない (API の事実・アプリの誤り・命名の罠) | 6 (todo 5〜9、wiki 10) |
| 今も残る処理系の不足 | **0** |

解消済み 16 件のうち、アプリを自然な形へ戻したのは 6 件 (notes 1・2、todo 2、wiki 7・11・12)。戻す前から自然な形になっていたのは 4 件 (todo 1・4、wiki 6・8)。解消していても戻さなかったのは 5 件 (wiki 1・2・3・4・9) と wiki 5 (処理系側の修正で、アプリに回避が無い)。

## 表

| 項目 | 元の形 | 結果 | アプリ |
| --- | --- | --- | --- |
| notes 1 `std.json.to_string` が E9040 | `derive [ToJson]` の `n.to_json(&mut out)` (`std.json.to_string` という関数は無い。ToJson の `to_json` が正) | 解消 (check・run・build・test --both が llvm で一致。引用符の escape も一致) | 戻した: `note_to_json` を `to_json` 1 行に、手書きの `write_json_string` を削除 |
| notes 2 `&self.notes` を返すと E9040 | `fn all(self: &Store) -> &Vec<i64>`、`&self.notes` | 解消 (同上。`as_slice()` で返す形も通る) | 一部戻した: `get` と `next_id` を `for memo in self.notes.iter()` に。ほかの `get_at` の呼び出し (添字ループの 9 か所) は添字で比較や「最初だけ , を付ける」をしているので残した (回避ではなく書き方) |
| todo 1 `Rng.from_entropy` が E9040 | `std.rand.Rng.from_entropy()` | 解消 (`uses [rand]` で全部通る)。ただしアプリは `fill_secure` の方が良く (トークンは暗号用乱数が要る)、それを正とする | 戻さない (元から `fill_secure`。トークンは OS の乱数を使うべきで、`from_entropy` は性質が違う) |
| todo 2 `Option<Vec<..>>` の欄が E9040 | `tags: Option<Vec<String>>` を持つ構造体と戻り値 | 解消 (`x is None` の判定も通る) | 戻した: `UpdateFields.tags` を `Option<Vec<..>>` に、欠けたら `None` (どこからも呼ばれない公開関数なので動作は変わらない) |
| todo 3 `Some(v) => v.as_str()` が生参照になる | `let q = match o { Some(v) => v.as_str() ... }` | **検査器が拒む (意図どおり)**: E3030 `returns a reference to the local variable v ...`・E3010 | 戻せない (`String` を先に束縛する形が正しい。記録の書き方が「回避」ではなく「安全な形」) |
| todo 4 モジュール修飾の呼び出しが E1108 | `p.model.q(..)` と `import` 後の `model.q(..)` | 解消 (両方通る) | 戻した状態 (`todo.model.push_quoted(..)` を全部で使っている。記録の「インライン展開した」は古い) |
| todo 5 `i64.from_u64` が無い | — | API の事実 (`spec/stdlib` に無い)。不足ではない | — |
| todo 6 `AllocError` の欄は `u32` | — | API の事実 | — |
| todo 7 `decode_array` は `&doc` | — | API の事実 | — |
| todo 8 検索 JSON の先頭カンマ | — | アプリの誤り (`count > 0` と `i > 0` の取り違え)。処理系は関係しない | 触らない (動作を変えない約束のため。別の直しに回す) |
| todo 9 `Value { doc, at }` の直接構築・`v.at` | `doc.value_at(at)` と `v.index()` | 方針どおり (欄は非公開。`value_at` と `index()` が正) | 既にその形 |
| wiki 1 `Vec.dedup()` が未対応 | `v.dedup()` | 解消 (全経路で通る)。ただし `dedup` は隣り合う重複だけを消す | 戻さない: 今は初出の順を保つ全体の重複除去。`sort` + `dedup` にすると並びが昇順に変わり、逆引き・リンクの出力が変わる |
| wiki 2 `HashMap` の `&mut self` が未対応 | `HashMap<String, u32>` の `insert`・`get_mut_str`・`len` | 解消 (全経路で通る) | 戻さない: 索引を `HashMap<String, Vec<String>>` に作り直す設計の変更になる (検索結果の並びの検証も要る) |
| wiki 3 `core.slice.sort` が segfault | `v.as_mut_slice().sort()` (中身ありと空) | 解消 (全経路で通る。ASan でも既に再現しないと 11/15 に確認済み) | 戻さない: 並べ替えを入れると出力の並びが変わる (wiki 1 と同じ) |
| wiki 4 `fill_secure` の後のリテラルが壊れる | `fill_secure` の後に `String.from_str("literal")` | 解消 (全経路で通る) | 戻さない: 回避はテストの書き方 (1 文字ずつ `push`) だけで、本体には無い。テストの入力は担当外のため触らない |
| wiki 5 `try_mcall` が `user_mcall` に回らない | 利用者定義のメソッド (`self: &mut T`) の呼び出し | 解消 (処理系側の修正が入っている。wiki 12 の形で確認) | アプリに回避は無い |
| wiki 6 空の `&[]` が E9040 | `p.f(&[])` | 解消 (全経路で通る) | 既に自然な形 (ログは `&[("reason", "truncated")]` など) |
| wiki 7 `core.str.find` が未対応 | `s.find(" ")` | 解消 (全経路で通る) | 戻した: 手書きのバイト走査を `es.find(" ")` + `if let Some(sp_idx)` に |
| wiki 8 `for` の `break`/`continue` が未対応 | `for` の中の `continue` と `break` | 解消 (全経路で通る) | 既に自然な形 (`markdown`・`link`・`search` が使っている) |
| wiki 9 `&[T]` の `.as_slice().get()` が型推論に失敗 | `xs.get(1usize)` と `v.as_slice().get(0usize)` | 解消 (全経路で通る) | 戻さない: `html` などの書き方は同じ意味の別形で、回避ではない |
| wiki 10 `src/wiki/wiki.ksnl` が `wiki.wiki` になる | — | 仕様どおり (FQN が名前になる)。不足ではない | — |
| wiki 11 `std.http.read_request` が本体を切る | `std.http.read_request(stream, decoder)` | 解消 (todo・shortener も同じ形で通っている) | 戻した: `serve_all` を `read_request` の `match` に。生成した実行ファイルで、戻す前後の応答 (200/503/405/400) が一致 |
| wiki 12 `CsrfManager` を消した (参照越しの欄の可変) | `&mut self` のメソッドで欄を更新 | 解消 (全経路で通る) | 戻した: `CsrfManager.generate_token(self)` と `verify_token(self, ..)` に。`router`・`server`・`wiki` の 70 か所ほどを `&mut wiki.csrf.CsrfManager` に |

## 戻せなかったもの (理由別、7 件)

- 振る舞いが変わるので戻さない (3 件): wiki 1・3 (昇順になる)、wiki 2 (設計の作り直し)。
- テスト・同じ意味の別形 (2 件): wiki 4・9。
- 検査器が正しく拒む (1 件): todo 3。
- 呼び出しの形が回避ではない (1 件): notes 2 の添字ループ (部分的に戻した)。

## 処理系の不足として統括者に渡すもの

なし。23 件のうち、今も落ちる形は無かった (E9040 や退避は 1 件も出ない。`ksn test --both` はどのアプリも `llvm` 一致、退避 0)。
注意: ネイティブへ下りない形が 11/15 以降に直ったことは、`FRICTION.md` の旧い記録が実態より悪く見える原因でもある。各 `FRICTION.md` の先頭に現状を足した。
