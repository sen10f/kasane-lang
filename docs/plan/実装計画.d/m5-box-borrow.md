# m5-box-borrow
B1: types.rs の call_method_in で Box 自身のメソッド (as_ref/as_mut ほか) の解決先 (last_fit) を確定し、Callee::Def になるようにした (以前は Unknown で借用が結ばれなかった)。
G2: 一時ガードへの参照を実引数に渡し、参照を返す呼び出しを E3010 にする (borrows.rs)。
再現は stage0/tests/data/unsound/{B1a..f,G2a..d}。
