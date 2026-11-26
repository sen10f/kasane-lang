# sec4-send-nested

G1: E5008 は impl の対象に現れる全ての型パラメータを見る (`collect_ty_params`)。G2: トレイトメソッド自身の型パラメータの `Send`/`Sync` 境界を、総称の受け手の呼び出しで確かめる (`seed_method_generics` → `fn_value_markers`)。dyn は総称メソッドが非対応 (E2033) なので不要。
