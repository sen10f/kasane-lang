# kir/indirect-call: 関数値の間接呼び出し

- 目的: `fn(A) -> R` の欄・局所を呼ぶプログラムを KIR に下ろす (以前は E9040 / 木走査へ退避)。
- 追加: `Op::FnAddr { callee }` (関数のアドレスを `ptr` で。C の包みなし) と `Op::CallIndirect { f, args, uses, may }` (テキスト: `fnaddr K` / `calli %f(args) uses[..] never|may`)。
- 対応: parse / verify (効果の包含)、`kir_exec`、VM (`Ins::FnAddr` / `CallInd`)、LLVM (基本の入口を呼ぶので深さの上限は直接呼び出しと同じ)、借用検査 (呼び先不明の呼び出し)、最適化 (副作用あり・不透明)。FnAddr の先は `taken` に入り純粋域から外れ、CallIndirect を含む関数も純粋域でない。
- 下ろし: 構造体の `fn` 欄に関数の項目を入れると FnAddr、`fn` 型の局所・式の呼び出しは CallIndirect (`CollCx::fnptr_tys`)。
- 対象外: 戻り値・列挙子・Option・タプルなどに入れた関数値、`!` を返す関数値。クロージャ (取り込みあり) は従来の経路。
- 未解決 (持ち主へ): 関数値を通した再帰は検査器が再帰と見ず、`stack N` が E4011、E4010 も要求されない。
- 試験: `stage0/tests/kir_indirect_call.rs` (木走査 = VM = native、深さ 102、ASan)。
