# 作業メモ (最終的に README.md へ)

## 書き方の制約 (checker / fmt)
- `end` は予約語: フィールド名・引数名・変数名に使えない (`stop` / `fin` に改名)。
- 100 文字超の本体行は `ksn check` が致命的な E0002 (`ksn fmt` は長い `||`/`&&` 連鎖を折らない)。let に分ける。
- 他モジュールの定数・型・フィールドは全部 `pub` が要る。`ksn fmt` が定義順を直す。
- `ksn run <引数>` は `--` なしで引数を渡す (`--` は引数として渡る)。

## ネイティブ (LLVM) に下りない形 → 回避
1. `parse.tok.OPS1.as_bytes()` (const の `&str` に直接メソッド) → `let ops = parse.tok.OPS1` に受けてから `ops.as_bytes()`。
2. `match slice.last() { Some(t) => *t }` (構造体 Tok の参照外し) → `Some(t) => parse.tok.new(t.kind, t.code, ..)` とフィールドから再構築。
