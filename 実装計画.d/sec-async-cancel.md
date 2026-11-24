# sec-async-cancel

hunt3 async の D1/C1/A1/A2/E1。E4022 は閉包の中の Let と StructLit 等の一時値を見る (check.rs)。ロックのガードの束縛が中断点の後に使われるなら E3050 (tast_rules/borrows.rs)。限界: 使われないままスコープに残るガード、構造体の欄に入れたガード、引数の位置の一時のガードは未検出。
