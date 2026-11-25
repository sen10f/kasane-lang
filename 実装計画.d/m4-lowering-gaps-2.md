# m4-lowering-gaps-2: KIR に下ろせない形 (2026-11-25)

試験は `stage0/tests/kir_m4_lowering.rs` (tree == VM == release ネイティブ)。

直した: `Option.ok_or` / `bs[i].v = e` (添字越しの構造体欄への代入) / 入れ子配列の `get` /
`for x in v.iter() { match x { Some(n) => .. } }` (`&Option<スカラー>`) / `for (i, x) in core.iter.enumerate(..)`。

直していない:
- 関数ポインタの欄 `b.on_click` と、値の関数の呼び出し: KIR に間接呼び出しの命令が無い (`Op::FnPtr` は C に渡す用)。命令の追加と全バックエンド (VM・LLVM・参照実行器) が要る。
- `HashMap<&str, V>` への `get_str` (hunt3 の 4・17・30): 型 `HashMap<&str, ..>` を `get_str` (`HashMap<String<B>, ..>` 宣言) が受けてしまう検査側のゆるさで、`B` は決まらない。下ろしの穴ではない。
- `VecDeque`: `.sig` だけで本体が組み込み。Kasane の本体 (`collections.ksnl`) を書く仕事になる。
- 並んだ分岐で同じ名前を別の型で束縛 (P-16.4): 手元の最小形 (if / match の腕) では再現せず。元の `r2/vmin/sl.ksnl` が無い。
- `Vec<Option<_>>` の VM の格納 (`mem.store` の型 Option) と Option の配列は未対応。
