Friction records for apps/wiki (Markdown wiki HTTP server for ksn).

環境 (2026-11): Xeon E5-2620 v4, rustup + ~/.cargo/env, stage0/target/release/ksn ビルド済み。KSN_AVOID_CPUS / taskset 不要。/home/kasane/kasane-wt/wiki/apps/wiki で ksn コマンドを実行。並列ビルド時は stage0/target が worktree ごとに別。

1. KIR 段階 1 の制約: `Vec.dedup()` は `String::eq` を使うため未対応。`link.ksnl` と `store.ksnl` で手動 O(n^2) 重複除去に置き換えた。
2. KIR 段階 1 の制約: `HashMap` の `&mut self` メソッドが未対応。検索インデックスを `HashMap<String, Vec<String>>` からフラットな `Vec<String>` (`"word name"` エントリ) に書き換えた。
3. ネイティブバックエンド: `core.slice.sort` が segfault する。`store.list_pages`, `store.page_versions`, `link.backlinks_for`, `link.extract_wiki_links`, `search.index_search` の 4 箇所で使用をやめ、ソートなしまたは O(n^2) dedup にした。
4. 処理系バグ: `std.rand.fill_secure` (CSRF トークン生成で使用) の呼び出し後に、文字列リテラルを関数引数として渡すとデータセグメントが破損する。`router.route` では `path` をすぐヒープ文字列にコピーし、テストではリテラルを避けて 1 文字ずつ `push` で組み立てる。
5. 処理系バグ: `try_mcall` が `method_on` エラー時に `Ok(None)` を返すように修正し、`user_mcall` で通常の関数呼び出しとして下ろせるようにした (`kir_lower_coll.rs` 行 ~1928)。
6. 処理系バグ: 空の配列リテラル `&[]` は段階 1 の KIR 対象外。ログ出力で `&[]` を使うと E9040 になるため、要素を 1 つ入れた配列に置き換えた。
7. 処理系バグ: `core.str.find` は `Option<usize>` を返すため段階 1 で未対応。`search.ksnl` でスペース探索を手動バイト走査に置き換え、`split_once` も同様に手動化した。
8. 処理系バグ: `break`/`continue` は `for` ループ内で段階 1 未対応。`markdown.ksnl` で `if-else` 連鎖と `done` フラグに置き換えた。
9. 処理系バグ: `&[T]` スライスパラメータで `.as_slice().get()` が型推論に失敗する。`html.ksnl` 等で `.get()` を直接スライスに呼び出すようにした。
10. モジュール名の罠: `src/wiki/wiki.ksnl` とするとモジュールが `wiki.wiki` になるため、トップレベルの `src/wiki.ksnl` (エントリ `wiki.main`) にした。
11. `std.http.read_request` は段階 1 インタプリタで `read()` が 0 を返しやすく本体が切れる。`serve_all` で `Decoder.feed_part` + `Decoder.next` の自前読みループに置き換えた。
12. `CsrfManager` 構造体を完全に削除。`generate_token` / `verify_token` を `&mut Vec<String>` を直接取る自由関数にし、参照越しフィールド可変アクセスの段階 1 制約を回避した。

プロセッサバグ候補 (トップ 3):
1. `std.rand.fill_secure` (セグメント破損): ランダムバイト生成後の文字列リテラル引数が破壊される。最小再現: `fn t.main` で `let mut buf = alloc.vec.Vec.new(std.alloc.SYSTEM); buf.push(0u8)?; buf.push(0u8)?; std.rand.fill_secure(buf.as_mut_slice())?; let s = alloc.string.String.from_str(std.alloc.SYSTEM, "literal")?;` とし、from_str の直後の戻り値や後続の文字列操作が壊れるか確認する。
2. `core.slice.sort` (ネイティブ segfault): 空ベクトルを含むスライスに対して `core.slice.sort` を呼ぶとネイティブバックエンドで segfault。最小再現: `fn t.main` で `let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM); core.slice.sort(v.as_mut_slice());` だけを実行し、クラッシュするか確認する。
3. `try_mcall` / `method_on` (KIR 段階 1): 未知のメソッド呼び出しが `Err` を返したとき `Ok(None)` にしないと `user_mcall` へのフォールバックが発動せず、組み込みメソッド扱いで型チェックに失敗する。最小再現: `fn t.main` で `let mut v = alloc.vec.Vec.new(std.alloc.SYSTEM); v.push(1u8)?;` とし、`push` が組み込みメソッドでない場合に `user_mcall` フォールバックが働くか確認する。

## 追記 (2026-11-15、統括者)

処理系の不具合として挙げた 3 件 (`std.rand.fill_secure` の後のリテラルの破損、空のスライスへの `core.slice.sort` の segfault、`try_mcall` が `user_mcall` へ回らない) は、後の `dev/plan` では ASan 付きでも再現しなかった。回帰テストを `stage0/tests/kir_wiki_bugs.rs` に置いた。アプリの回避 (並べ替えをやめた箇所など) は残している。
