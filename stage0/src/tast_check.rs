//! 新しい検査 (TAST の上の検査、設計_型付き検査.md 4 章の S2・S3) の差し込み口。
//! 検査の判断は TAST の規則 (tast_rules/) に一本化した (旧い検査は推論と残すパスだけ)。
//! 規則を移すときは、この `check` の中だけを育てる (呼び出しは cli.rs の 1 か所)。

use crate::check::Diag;
use crate::program::Program;

/// 新しい検査。`prog` は構築済みのプログラム、`user` は利用者の定義の添字。
pub fn check(prog: &Program, user: &[usize], unsafe_modules: &[String]) -> Vec<Diag> {
    let mut tk = crate::types::TypeCk::new(prog);
    for &i in user {
        let d = &prog.defs[i];
        if (d.kw == "fn" && d.body.is_some()) || d.kw == "test" || (matches!(d.kw.as_str(), "const" | "static") && d.init.is_some()) {
            tk.check_def(i);
        }
    }
    let tast = crate::tast::build(prog, user, &tk.tytab, &tk.side, &tk.let_tys);
    if std::env::var_os("KSN_DUMP_TAST").is_some() {
        eprint!("{}", crate::tast::dump(prog, &tast));
    }
    // 規則 I (impl の署名・過不足・効果) と規則 D (`&T` → `&dyn Tr` の実装の有無) (S2B)。
    let mut out = Vec::new();
    out.extend(crate::tast_rules::impl_sig::check(prog, user, &tast, &tk.side));
    out.extend(crate::tast_rules::ref_levels::check(prog, user, &tast, &tk));
    // 規則 X (効果の完全一致、D5・D13)。
    out.extend(crate::tast_rules::effects_exact::check(prog, user, &tast));
    // 規則 T (型の一致、S2C)。
    out.extend(crate::tast_rules::type_eq::check(prog, user, &tast, &tk));
    // 規則 B (借用の重なり、S3E)。
    out.extend(crate::tast_rules::borrows::check(prog, user, &tast, &tk));
    // 規則 L (借用の寿命と出どころ、S3D)。
    out.extend(crate::tast_rules::lifetimes::check(prog, user, &tast, &tk));
    // 移動と線形 (S3B)。
    out.extend(crate::tast_rules::moves::check(prog, user, &tast, &tk));
    // 閉包の取り込み (S3C)。
    out.extend(crate::tast_rules::captures::check(prog, user, &tast));
    // 効果変数・panic 変数の実体化 (規則 E)。
    out.extend(crate::tast_rules::effects_inst::check(prog, user, &tast));
    // 関数型・`dyn` の効果の食い違い (規則 F)。
    out.extend(crate::tast_rules::effects_conv::check(prog, user, &tast));
    // 捨てた値 (規則 D)。
    out.extend(crate::tast_rules::discard::check(prog, user, &tast));
    // `match` の網羅性 (規則 M)。
    out.extend(crate::tast_rules::exhaustive::check(prog, user, &tast));
    // 可変にできない場所 (規則 W)。
    out.extend(crate::tast_rules::mutability::check(prog, user, &tast));
    // `extern` / `core.ptr` の呼び出し (規則 U)。
    out.extend(crate::tast_rules::unsafe_calls::check(prog, user, &tast, unsafe_modules));
    // 型パラメータの境界の充足 (規則 I の拡張)。
    out.extend(crate::tast_rules::bounds::check(prog, user, &tast));
    // 同じ診断を 2 つの規則が出すことがある (参照の段数と型の一致): (ファイル, 行, コード, 文面) で重ねる。
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|d| seen.insert((d.file.clone(), d.line, d.code.clone(), d.msg.clone())));
    out
}

/// `KSN_CHECKER` の旧い値 (old / new / shadow) は、検査器が 1 つになったので何も変えない。
/// 1 リリースだけ受け付け、stderr に非推奨を 1 行出す (s4 は黙って受け付ける)。
fn warn_retired_mode() {
    if let Ok(v) = std::env::var("KSN_CHECKER") {
        if matches!(v.as_str(), "old" | "new" | "shadow") {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| eprintln!("warning: KSN_CHECKER={} is deprecated and ignored: there is a single checker now (it will be rejected in the next release)", v));
        }
    }
}

/// S4 で TAST の規則が置き換える旧の「式の形を見る検査」の診断コード (設計_S4の切り替え.md 1.1)。
/// 旧のうちこれらだけを落とし、それ以外 (字句 E3060・名前解決・推論・署名の字句・E5001 など 1.2) は
/// そのまま旧の診断を使う。E2023 / E20xx / E3020・E3043 のような共有コードはコードでは区別できないので
/// フィルタに入れず、borrowck の呼び出し側 (check.rs) を止めるなど場で分ける。
const S4_REPLACED: &[&str] = &[
    "E3010", "E3030", "E3050", // borrowck.rs
    "E3002", "E3003", "E3006", "E3040", // tc_fn 系の移動・線形
    "E4001", "E4002",           // 効果の実体化・階層
    // E1122 はフィルタしない (フィールドの可視性 check.rs:1923 も同じコード)。impl の署名のほうは
    // check_impl_method_sigs の呼び出しを s4 で止める (check.rs)。
    "E2042",                    // 捨てた値
    "E2021", "E2052",           // 網羅性
    "E3031",                    // 可変性
    "E5003",                    // unsafe 呼び出し
    "E2030",                    // 境界
];

/// 旧の検査を持たず、TAST の規則だけが出す診断コード (Old)。効果 (E4001/E4002)、捨てた値 (E2042、
/// discard.rs)、網羅性 (E2021/E2052、exhaustive.rs)、可変性 (E3031、mutability.rs)、境界 (E2030、bounds.rs)。
/// 設計_S4の旧い検査の取り外し.md 3 段目 1・3。
fn is_tast_code(code: &str) -> bool {
    matches!(code, "E4001" | "E4002" | "E2042" | "E2021" | "E2052" | "E3031" | "E2030" | "E3002" | "E3003" | "E3006" | "E3040")
}

/// (ファイル, 行, コード, 文面) で重ねる。
fn dedup(mut out: Vec<Diag>) -> Vec<Diag> {
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|d| seen.insert((d.file.clone(), d.line, d.code.clone(), d.msg.clone())));
    out
}

/// 報告する診断を選ぶ: 新しい規則 (TAST) + 残す旧パス。置き換えた旧パス (S4_REPLACED) の診断だけ旧から外す。
pub fn select(prog: &Program, user: &[usize], unsafe_modules: &[String], old: Vec<Diag>) -> Vec<Diag> {
    warn_retired_mode();
    let kept: Vec<Diag> = old.into_iter().filter(|d| !S4_REPLACED.contains(&d.code.as_str())).collect();
    dedup([kept, check(prog, user, unsafe_modules)].concat())
}
