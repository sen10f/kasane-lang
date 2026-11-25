# user-drop: 手書き `Drop` の実行 (仕様 4.2 / 4.4)

Mythos3 D1 の直し。今は手書きの `impl core.ops.Drop` を呼ぶ実行系がない。前の分析は `m3-user-drop.md`。

## 意味 (仕様どおり。実装の正は KIR の下ろし)

- `Drop.drop(&mut self)` は `uses [] panics never`。効果を持てないので、観測は `static` のアトミック・自分の欄の書き換えなどに限る。
- 呼ばれる時: (1) 束縛のスコープの終わりに、宣言の逆順。 (2) 初期化済みの場所への代入で、古い値 (新しい値を評価したあと、書き込む前)。
  (3) `core.mem.drop(x)`。 (4) 文の一時値は文の終わり。 (5) 関数に値で渡した引数は呼ばれた側が (その関数のスコープの終わりで) 破棄する。
  (6) 一部だけ move した値: Drop 型自身の欄の move は E3002 (中間の規則) で拒むので、Drop 型の値は丸ごと生きているか、丸ごと move 済み。
  Drop 型を欄に持つ外側の型の欄の move 済みは、残りの欄だけ破棄する。
  (7) `return` / `?` / `break` / `continue` は、抜けるスコープ分を内側から順に。 (8) `with ... close` は close の式を実行してから、束縛した値を破棄 (束縛は close が消費する)。
  (9) panic・`std.process.exit` は巻き戻さない (破棄は走らない)。 (10) `linear` 型は `Drop` を持てない (E2032)。消費されるだけで破棄されない。
- 順序: 外側の `Drop.drop` を先に呼び、そのあと欄を宣言の逆順で破棄する (Rust と違い宣言順の仕様なら、それに従う。仕様 4.4 の表は「フィールドは宣言順」: 実装は仕様に従い **宣言順**)。
  実装の現状は欄を逆順に解放している (所有物のバッファ。観測できない)。Drop を持つ欄が観測できるので、ここは仕様どおり宣言順に直す。
  配列・`Vec<D>` の要素は添字の昇順。
- `derive [Drop]` (再帰の型) は手書きと併用不可 (E2051)。生成物は観測できない。変更なし。

## 実装

- KIR: `AggTab.user_drop` (集成体の添字 → `T.drop` の関数名) を `intern_def` で登録する。`owns_heap` が真になるので、既存の所有の印
  (`Owned::Val` + 移動の印、`drop_from` / `block_end` の `return` / `?` / `break` / `continue` 経路、一時値の `tmp_hold`) がそのまま使える。
  `drop_value` は Drop 型なら、値を一時スロットに置いて `T.drop(&mut slot)` を呼び、そのあと欄の破棄 (入れ子の Drop 型・Vec・Box など) を続ける。
  条件付きの move の印は `Owned::Val` の実行時の印。LLVM とバイトコード VM は KIR から同じ結果を得る。
- 総称型の Drop 実装 (`impl[T] Drop for X<T>`) は初版では下ろさない (理由付きで KIR の対象外)。
- 木走査: `Rc` 共有で所有を追跡しないので、手書き Drop を持つパッケージは `ksn run` / `ksn test` を VM (KIR) で実行する。
  下ろせないときは木走査へ退避せず、理由付きで失敗する (退避すると Drop が黙って飛ぶ。E9043 を足す)。
- スレッド: 値を spawn の閉包に move すると、スレッドの終わりに閉包の環境の破棄で呼ばれる (環境の破棄が所有の欄を見る範囲)。

## 試験

`stage0/tests/user_drop.rs` + `tests/data/user_drop/*.ksnl`: VM と LLVM (debug / release) の出力が同じ。観測は `static` の `AtomicU64` の履歴 (`log * 10 + id`)。

## 状況 (2026-11-25)

- 済: KIR の `drop_value` が手書きの `Drop` を呼ぶ (`AggTab.user_drop`、隠しの欄 `$live` で、移し出した後の「空」の値と部分的な move の二重呼びを防ぐ)。
  欄の破棄は宣言順に直した。`&式` の一時値は `Owned::TmpSlot` (入口で 0 にする印) で文の終わり (`if` / `while` の条件の終わり) に破棄。
  `ksn run` / `ksn test` は手書きの `Drop` があれば VM 固定で、下ろせなければ理由付きで失敗。spawn / FnOnce の move の取り込みと、`&` / `&mut` の取り込みは可。
- 試験: `stage0/tests/user_drop.rs` (VM・LLVM debug / release・ASan で同じ)。
- 未: 総称型の `Drop`、`Fn` のクロージャの `move` の取り込み (環境の破棄が要る)、呼ばれなかった `FnOnce` の環境の破棄、`Vec` ほか標準の型の一時値の破棄 (`&Vec` の一時値は今も漏れる)。
