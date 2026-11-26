# m4-closure-ret-capture (M9)

閉包の戻り値が `&mut` / `move` の取り込みの中を借りたら E3030。`'static` の戻り値は `&` の取り込み由来も誤り。再現は unsound/m4_M9a..g。
