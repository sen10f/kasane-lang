# arith-range-proof: `panics never` の `+` / `-` の範囲の証明 (2026-11-26 の決定 案 C)

- `stage0/src/arith_proof.rs`: 関数の中・構文の形だけ・流れに沿う証明 (迷えば証明しない)。`i + 1` (`i < n` のあと)、`x - k` (`x >= k` / `x > k-1` / `x > 0`、符号なしは `x != 0`)、`v.len() - 1` (`!v.is_empty()` のあと)。`&&` の右、`while` の本体、`if`/`else`、早期の `return`/`break`/`continue` のあとを見る。代入・`&mut`・shadow・(len は) 読み取り以外の使い方で事実が消える。閉包の中で代入される名前は事実を作らない。`*` `/` `%` は証明しない。
- `check.rs`: 証明できた `+` / `-` は E4004 を出さない。メッセージは意図別 (checked+`?` / saturating / わざとの wrapping / 守りの条件)。`explain E4004` も更新。
- 移行: 証明できる `wrapping_add(c)` / `wrapping_sub(k)` は check を通す (`arith_proof::PROVEN_WRAPPING_IS_ERROR = false` が唯一のスイッチ。誤りへの切り替えは後)。`ksn fix` が `x + c` / `x - k` に直す (`wrap_fixes`。単独に立つ文脈だけ)。fix の間は検査の差分キャッシュを切る。
- 下ろし: 変更なし。証明できた `+` / `-` は溢れないので、今の下ろし (VM・LLVM とも検査つきの加減算) と結果が同じ。nuw/nsw は渡さない。
- 試験: `tests/arith_proof.rs`、`tests/data/check_ok/arith_proven_ok.ksnl`、`tests/data/unsound/arith_*.ksnl` (近い誤りの 19 本)。examples・標準ライブラリはまだ移行しない。
