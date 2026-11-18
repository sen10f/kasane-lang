//! TAST の上の規則 (設計_型付き検査.md 2.3・4 章の S2)。1 規則 1 ファイル。`tast_check::check` が順に呼ぶ。

pub mod impl_sig;
pub mod ref_levels;
pub mod type_eq;
