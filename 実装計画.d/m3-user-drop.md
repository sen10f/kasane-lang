# m3-user-drop

- 済: Drop 型の欄の move を E3002 で拒む (moves.rs の bind_pat / apply)。diag/E3002d・explain 追記。
- 済: BETA.md / docs/known-limitations.md / 実装計画.md 4.2 に「手書き Drop は呼ばれない」を明記。
- 未: 手書き Drop の実行 (木走査・KIR・LLVM)。仕様 4.4: スコープ終端で宣言の逆順・欄は宣言順・一時値は文の終端・move 済みは呼ばない・panic では走らない。KIR 側にドロップ推論 (move フラグ) が無く、木走査は値が Rc 共有で所有を追跡していないため、別の作業にする。
