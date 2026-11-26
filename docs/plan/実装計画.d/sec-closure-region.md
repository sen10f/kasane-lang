# check/sec-closure-region: 閉包の取り込みの領域 (C1〜C5)

根の規則: `capture [&s]` の借用は閉包の環境の領域で縛られる。流してよいのは戻り値と本体の局所だけ。
引数の指す先・取り込んだ `&mut` の先 (`move` した `&mut` 越しも)・入れ子の閉包の引数へは書けない (E3030)。

実装 (`stage0/src/tast_rules/lifetimes.rs`): 
- `unrelated_store` に逆向き (取り込み → 引数の指す先) を足す。
- `store_through` で `ArgIn` の取り込みを `Arg` と同じに扱う (入れ子で `&&T` の層がずれる)。`move` した `&mut` の先 (外側の局所) への書き込みは `cap_stores` へ。
- `closure_body` の後処理で `cap_stores` を外側へ持ち上げる (入れ子)・引数の指す先なら E3030。
再現: `tests/data/unsound/sec3_closure_region_*`。正しい形: `check_ok/closure_region_ok.ksnl`。
