//! 新しい検査 (TAST の上の検査、設計_型付き検査.md 4 章の S2・S3) の差し込み口。
//! 今は何も検査せず、診断 0 件を返す。`KSN_CHECKER=shadow` で旧い検査との差を取る (`tests/shadow_diff.py`)。
//! 規則を移すときは、この `check` の中だけを育てる (呼び出しは cli.rs の 1 か所)。

use crate::check::Diag;
use crate::program::Program;

/// 新しい検査。`prog` は構築済みのプログラム、`user` は利用者の定義の添字。
pub fn check(prog: &Program, user: &[usize]) -> Vec<Diag> {
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
    out.extend(crate::tast_rules::unsafe_calls::check(prog, user, &tast));
    // 型パラメータの境界の充足 (規則 I の拡張)。
    out.extend(crate::tast_rules::bounds::check(prog, user, &tast));
    // 書き方は 1 通り (規則 O、設計_書き方は1通り.md 12 章)。
    out.extend(crate::tast_rules::oneway::check(prog, user, &tast));
    // 同じ診断を 2 つの規則が出すことがある (参照の段数と型の一致): (ファイル, 行, コード, 文面) で重ねる。
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|d| seen.insert((d.file.clone(), d.line, d.code.clone(), d.msg.clone())));
    out
}

/// `KSN_CHECKER` の値。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Old,
    New,
    Shadow,
    /// S4 が既定になるときの形: 新しい規則 + 残す旧パス (設計_S4の切り替え.md 1.1・1.2)。
    S4,
}

pub fn mode() -> Mode {
    match std::env::var("KSN_CHECKER").as_deref() {
        Ok("new") => Mode::New,
        Ok("shadow") => Mode::Shadow,
        Ok("old") => Mode::Old,
        // 既定は s4 (2026-11-20 に old から切り替え。old / new / shadow は 1 リリースだけ残す)。
        _ => Mode::S4,
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

/// 型の食い違いの E2023 (引数・欄・要素・ペイロード・パターン)。型パラメータの個数の E2023 (`takes N explicit type argument(s)`)
/// は旧の解決・推論側に残るので除く (`select` の Old 分岐でだけ使う)。
fn is_tast_e2023(d: &Diag) -> bool {
    d.code == "E2023" && !d.msg.contains("type argument")
}

/// (ファイル, 行, コード, 文面) で重ねる。
fn dedup(mut out: Vec<Diag>) -> Vec<Diag> {
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|d| seen.insert((d.file.clone(), d.line, d.code.clone(), d.msg.clone())));
    out
}

fn js(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// 旧い検査の診断と新しい検査の診断を (ファイル, 行, コード) で比べ、差を stderr に 1 行 1 件の JSON で出す。
/// 返すのは実際に報告する診断 (`old` / `new` / `shadow` のどれでも旧に従う。`new` は新だけ)。
pub fn select(prog: &Program, user: &[usize], old: Vec<Diag>) -> Vec<Diag> {
    match mode() {
        Mode::Old => {
            // 効果の診断 (E4001/E4002) は旧の検査を持たず、TAST の効果の規則 (effects_*) だけが出す
            // (設計_S4の旧い検査の取り外し.md 3 段目 1。旧の効果検査は check.rs から消した)。
            // impl の署名 (E1122) も旧の検査を持たず、TAST の impl_sig 規則が出す (設計_S4の旧い検査の取り外し.md 3 段目 2)。
            // 他の旧の診断はそのまま出す (check.rs のフィールド可視性の E1122 を含む)。KSN_DUMP_TAST は木を出す確認用。
            let new = check(prog, user);
            // E2023 の参照の段数 (引数・欄・ペイロードの `&` の食い違い) は旧の検査を持たず、TAST の ref_levels が出す
            // (設計_S4の旧い検査の取り外し.md 3 段目 4)。旧に残る型の食い違い (総称・クロージャ・dyn の単一化) は旧が出し、
            // TAST の E2023 は旧が同じ行で出していない所だけ足す。
            let old_pos: std::collections::BTreeSet<(String, usize)> =
                old.iter().filter(|d| d.code == "E2023").map(|d| (d.file.clone(), d.line)).collect();
            // 寿命・クロージャの E3010/E3030/E3050 も旧の検査 (borrowck.rs の check_lifetime_store / check_return_flow /
            // check_closure_*) を消したので TAST が出す。旧の競合の検査 (analyze) が同じ (ファイル, 行, コード) で出す所は旧に任せる。
            let old_life: std::collections::BTreeSet<(String, usize, String)> = old
                .iter()
                .filter(|d| matches!(d.code.as_str(), "E3010" | "E3020" | "E3030" | "E3050"))
                .map(|d| (d.file.clone(), d.line, d.code.clone()))
                .collect();
            let kept: Vec<Diag> = old.into_iter().filter(|d| !is_tast_code(&d.code)).collect();
            let eff: Vec<Diag> = new
                .into_iter()
                .filter(|d| {
                    is_tast_code(&d.code)
                        || (matches!(d.code.as_str(), "E3010" | "E3020" | "E3030" | "E3050") && !old_life.contains(&(d.file.clone(), d.line, d.code.clone())))
                        || d.code == "E1122"
                        || (is_tast_e2023(d) && !old_pos.contains(&(d.file.clone(), d.line)))
                })
                .collect();
            dedup([kept, eff].concat())
        }
        Mode::New => check(prog, user),
        Mode::S4 => {
            // 新しい規則 + 残す旧パス。置き換えた旧パス (S4_REPLACED) の診断だけ旧から外す。
            let kept: Vec<Diag> = old.into_iter().filter(|d| !S4_REPLACED.contains(&d.code.as_str())).collect();
            dedup([kept, check(prog, user)].concat())
        }
        Mode::Shadow => {
            let new = check(prog, user);
            let key = |d: &Diag| (d.file.clone(), d.line, d.code.clone());
            let ok: std::collections::BTreeSet<_> = old.iter().map(key).collect();
            let nk: std::collections::BTreeSet<_> = new.iter().map(key).collect();
            for (side, a, b) in [("old_only", &ok, &nk), ("new_only", &nk, &ok)] {
                for (f, l, c) in a.difference(b) {
                    eprintln!("{{\"shadow\": \"{}\", \"file\": {}, \"line\": {}, \"code\": {}}}", side, js(f), l, js(c));
                }
            }
            old
        }
    }
}
