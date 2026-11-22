# S5-4 名前の解決

- `Tast::pres` ((定義, 節点 ID) → 解決先): `Path` 式・`PVariant`/`PPath`/`PStruct` パターン・構造体リテラルの型名を `tast::build` が残す。`E.V.m` / `C.iter` の前置 (`ID | PREFIX_BIT`) も残し、下ろしが作る受け手の節点 (`variant_method_call`・`for`・`iter`) が引く。下ろしは `Lw::path_res` で引く。
- FQN で持っている名前 (型・`Named`・`Type::next` など) は `Program::lookup_fqn` (import の展開なし)。`ufcs_rewrite` は `TyIdx::paths` を引く。
- `.resolve(` (`kir_lower*.rs`): 34 → 0。`from_v` (約 80 か所) の削除は未着手。
- 注意: 前置の合成節点は型表にない (`const_val` が `PREFIX_BIT` を ID 0 と同じに扱う)。
