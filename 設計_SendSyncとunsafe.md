# 設計: Send / Sync の手動 impl と unsafe の境目 (B10 の Send の件)

2026-11-24 起草 (利用者の承認待ち)。4 波の計画の元の文書が見つからなかったので、再構成した。

## 1. 問題と決定

1. Mythos max の大規模レビュー B10 (`~/kasane-verify/agents/mythos-scale/report.md`): 生ポインタを持つ構造体は `impl core.marker.Send for T` の 1 行で `Send` にできる。検査の外の表明で、スレッド間のデータ競合の責任が見えない。
2. 2026-11-23 の決定 (案 A): 新しい書き方 (`unsafe impl`) は作らない。`core.marker.Send` / `Sync` を付ける `impl` は `[unsafe] modules` に載ったモジュールの中でしか書けず、それ以外は誤り。
3. 決定の「それ以外は誤り」は E5002 として 2026-11-10 から入っている。残る仕事は、E5002 の取りこぼし (下の穴) を塞ぐことと、利用者が許可モジュールの責任を把握できるようにすること。
4. 仕様は 5.5 (`Send` / `Sync` の自動導出と手動 impl)・3.8 (`unsafe` 許可モジュール)・8.1 (`[unsafe] modules`)。

## 2. 実装の現状

### 入っているもの

| 規則 | 診断 | 場所・コミット |
|---|---|---|
| 手動の `Send` / `Sync` impl は許可モジュールのみ | E5002 | `check.rs` の impl 行の検査、`9b175b1b` (11/10)、`explain.rs`・`diag/E5002.ksnl` |
| 生ポインタの型は許可モジュールのみ | E5001 | 同上 |
| 生ポインタを持つ型は自動で Send/Sync でない | E2040 (spawn の捕捉) | `unsound/send_raw_ptr_spawn_without_impl` |
| 許可モジュール内の impl は通る / 外は E5002 | | `9bfaf842` の `send_impl_for_raw_pointer_only_in_unsafe_module`、`unsound/send_impl_raw_ptr_normal_module` (`2c26c71f` で統合) |
| `static` は `Sync` でなければ誤り | E2031 | 3.1 |
| 依存の `[unsafe] modules` が増えたらロックの食い違い | E9060 | 8.1 |

### 試した結果 (再現は `/tmp/sendsync_<名前>/`。道具は `kasane-verify/bin/current/ksn`)

型は許可モジュール `t.sys` の `Shared { p: *const u8 }`、使う側は許可の無い `t`。

| 名前 | 内容 | 結果 |
|---|---|---|
| A | `Shared` を `spawn` に `move` | E2040 (期待どおり) |
| B | `Outer { inner: Shared }` を `spawn` (欄の欄) | E2040 |
| C | `Vec<Shared>` を `spawn` | E2040 |
| D | `Shared` が `layout c` | E2040 |
| E | `static S: Shared` | E2031 (と E2038) |
| G | `&Shared` を `capture [&s]` で `spawn` | E2040・E3060 で拒む |
| F | **許可モジュール `t.sys` に、別モジュール `t` の型 `Outer` への `impl Send`** | **通る (穴 H1)** |
| H | **`impl Send for core.sync.AtomicBool` (標準の型)、`impl Sync for Shared` を許可モジュールに** | **通る (穴 H2)** |
| I | **`impl[T] Send for W<T>` (`T` に境界なし)、`impl Send for std.sync.Receiver<u8>`、同じ型に 2 つ目の `impl Send`** | **全部通る (穴 H3・H4)** |
| J | **`fn need[T: core.marker.Send](x: T)` に `Shared` を渡す** | **通る (穴 H5。`bounds.rs` が `Send`/`Sync` の境界を `continue` で飛ばす)** |
| K | `channel::<Shared, _>` | 通る。`channel` の `T` に `Send` の境界は無い。`Sender<T>` の `Send` が `T: Send` を要求するので、端をスレッドへ送る所で止まる (設計どおり、穴ではない) |

### 穴 (開いたもの)

- H1 孤児: 許可モジュールなら、自分のものでない型 (別モジュール・別パッケージ・標準の型) にも `Send` / `Sync` を付けられる。許可の責任が型の持ち主の外へ漏れる。
- H2 標準の型: 許可モジュールを持つ利用者パッケージが `core.*` / `std.*` の型の `Send` / `Sync` を上書きできる (自動導出・標準の手動 impl との重複も E が出ない)。
- H3 境界なしの総称: `impl[T] Send for W<T>` が通る。`T` が `Send` でなくても `W<T>` が `Send` になる (Rc 型の持ち出しの典型)。標準は `impl[T: Send] ...` と書くが、強制はされない。
- H4 重複: 同じ型への二重の impl、自動導出が成り立つ型への手動の impl に診断が無い。
- H5 境界の未検査: 利用者の総称関数の `T: Send` / `Sync` の境界を呼び出し側が検査しない (spawn だけが見ている)。`Send` の入り口が spawn の捕捉に偏る。
- H6 (未確認・調べる): `fnptr` で C へ渡す生ポインタ入りのコールバック、`extern` が返す生ポインタ、`Arena` の `Handle<T>` 経由で `T` を持ち出す形は未試験。
- H7 見える化: どの許可モジュールにいくつ手動 `Send/Sync` があるか、一覧を出す道具が無い (レビューは `grep impl` に頼る)。

## 3. 4 波の計画

各波は数日。どの波も回帰基準は、`examples/` の全パッケージと `bench/tasks/*/reference.ksnl` の参照解が診断 0 件、`stage0/tests/data/unsound/` が全件エラー、標準ライブラリ本体 (`spec/stdlib`) に診断 0 件、速い段 (check_all・参照解・ネイティブ率・`par_test.py --tier fast`) が先に通ること。

### 第 1 波: 孤児と重複 (H1・H2・H4)

- 規則: `impl core.marker.Send/Sync for X` は、X の型 (または総称の外側の型) を定義したモジュールか、そのモジュールと同じ許可の範囲のモジュールでだけ書ける。標準の型 (`core`/`alloc`/`std`) への impl は、そのパッケージだけ。違反は新しい E5004 (「他の持ち主の型への Send/Sync」)。
- 規則: 同じ型・同じトレイトの二重の impl は E1113 系で止める (すでに重複の診断があれば流用)。自動導出が成り立つ型への手動 impl は、冗長として E5005 か `ksn fix` で消す (書き方は 1 通り)。
- `explain` に E5004・E5005、`diag/E5004.ksnl`・`diag/E5005.ksnl` を追加。`unsound/` に F・H・重複の 3 本。
- 標準の `.sig` を全部通す: 標準の手動 impl (`alloc` `std` の 20 件) は全て自分の型であることを確かめる。
- 仕様 5.5 に孤児の 1 文。

### 第 2 波: 総称の境界 (H3・H5)

- 規則: 型パラメータ `T` を欄に持つ型への手動 `Send` impl は、`T` に `Send` (または `Sync` なら `Sync`) の境界が無ければ誤り (E5006。標準の全 impl は既に境界あり)。例外は欄が `T` を値で持たない (`PhantomData` 相当の) 場合だが、Kasane に無いので例外なし。
- `bounds.rs` の `Send` / `Sync` の `continue` を外し、呼び出し側が実引数の型の `Send` / `Sync` を確かめる (E2040 の文面を「spawn の捕捉」だけでなく境界の不成立にも使う)。標準の総称関数 (`channel`・`spawn` ほか) の境界の audit を同時に行い、偽陽性は `unsound/` の逆 (`linear_ok/`) に正しい例を足して守る。
- 推論が決まらない型は fail-closed (E2044)。偽陽性が出たら、型の注釈で直す形を `ksn explain` に書く。

### 第 3 波: 道具と見える化 (H7・H6)

- `ksn check --unsafe-report` (または `ksn explain` とは別の読み出し専用): 許可モジュールごとに、生ポインタの型・`extern`・`unsafe` ブロック・手動 `Send/Sync` の数と場所を一覧する。ロックの `unsafe_modules` と並べて、依存が増えたらレビューの見る場所が分かる。
- H6 の確かめ: コールバック・`extern` の戻り値の生ポインタ・`Handle` 経由の持ち出しを `/tmp/sendsync_*` で試し、通ったものを `unsound/` に入れる。塞げないものは `KNOWN_HOLES` に載せる。
- 依存パッケージの許可モジュールの手動 `Send/Sync` が増えた場合、E9060 の広がりの判定に数えるか (§4 の質問 3)。

### 第 4 波: 文書と締め

- 仕様 5.5・3.8 に、孤児・境界・重複・報告の規則を書き、版を上げる。
- チートシートに「`Send` を付ける書き方」を 1 節: 許可モジュールの中、自分の型だけ、`impl[T: core.marker.Send] core.marker.Send for 型<T>`。通らない例と直し方 (欄をアリーナの取っ手や `Mutex` に置き換える) つき。
- `ksn explain E5002/E5004/E5005/E5006` の文面をそろえる (いまの E5002 は 1 行)。
- `実装計画.md` 2 章を更新。Mythos の 3 回目の診断で `Send` 周りを重点に再確認する (使用量が戻ってから)。

## 4. 利用者への質問

1. 孤児の規則 (第 1 波) はどこまで厳しくするか。案 A: 型を定義したモジュールだけ。案 B: 同じパッケージの許可モジュールならどこでもよい (現状は全パッケージで可)。推奨は A (書く場所が 1 つ、書き方は 1 通り)。
2. 標準の型 (`core`/`std`) への手動 impl を、同梱の標準以外では全面禁止にしてよいか (推奨: 禁止)。
3. 手動 `Send/Sync` の数をロックに記録し、増えたら E9060 にするか。重い (依存の更新が頻繁に止まる) ので、まず報告だけにして、記録は後に回す案を推奨。
4. 第 2 波で `bounds.rs` の `Send/Sync` 境界を検査すると、今まで通っていた利用者のコードが落ちうる。1 リリース警告にするか、すぐ誤りにするか (fail-closed の既定どおりなら即誤り)。
5. `unsafe impl` の形は作らない決定を、報告の道具 (第 3 波) で補う方針で確定してよいか。
