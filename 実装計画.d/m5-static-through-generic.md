# m5-static-through-generic (S1)
- 総称の `&mut T` / Vec.push などへ格納する値は、実引数の指す先の型の `'static` 層に対して static_store で E2037。
- パス形の受け手の Arg に var.ty を入れる。`&mut` の定数は昇格せず Temp (E3030)。
- 再現は unsound/m5_S1*。
