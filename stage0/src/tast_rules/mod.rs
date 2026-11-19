//! TAST の上の規則 (設計_型付き検査.md 2.3・4 章の S2)。1 規則 1 ファイル。`tast_check::check` が順に呼ぶ。

pub mod effects_exact;
pub mod borrows;
pub mod impl_sig;
pub mod lifetimes;
pub mod ref_levels;
pub mod type_eq;
pub mod moves;
pub mod captures;
pub mod effects_inst;
pub mod effects_conv;
pub mod discard;
pub mod exhaustive;
pub mod mutability;
pub mod unsafe_calls;
pub mod bounds;
