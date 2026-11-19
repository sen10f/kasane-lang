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
    // 規則 T (型の一致、S2C)。
    out.extend(crate::tast_rules::type_eq::check(prog, user, &tast, &tk));
    // 移動と線形 (S3B)。
    out.extend(crate::tast_rules::moves::check(prog, user, &tast, &tk));
    // 閉包の取り込み (S3C)。
    out.extend(crate::tast_rules::captures::check(prog, user, &tast));
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
}

pub fn mode() -> Mode {
    match std::env::var("KSN_CHECKER").as_deref() {
        Ok("new") => Mode::New,
        Ok("shadow") => Mode::Shadow,
        _ => Mode::Old,
    }
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
            // 確認用: 木を出すだけ (診断は旧いまま)。
            if std::env::var_os("KSN_DUMP_TAST").is_some() {
                let _ = check(prog, user);
            }
            old
        }
        Mode::New => check(prog, user),
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
