# sec5-rephrase-holes

- 引数の `{ e }` を見通す `peel_block` (borrows.rs) で `x.go({x})` の自己の重なりを検出。
- 閉包の本体の walk 中は `closure_held` を退避 (本体の束縛が外の閉包のガード保持を横取りしていた)。
