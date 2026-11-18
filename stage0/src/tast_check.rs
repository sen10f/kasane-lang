//! 新しい検査 (TAST の上の検査、設計_型付き検査.md 4 章の S2・S3) の差し込み口。
//! 今は何も検査せず、診断 0 件を返す。`KSN_CHECKER=shadow` で旧い検査との差を取る (`tests/shadow_diff.py`)。
//! 規則を移すときは、この `check` の中だけを育てる (呼び出しは cli.rs の 1 か所)。

use crate::check::Diag;
use crate::program::Program;

/// 新しい検査。`prog` は構築済みのプログラム、`user` は利用者の定義の添字。
pub fn check(_prog: &Program, _user: &[usize]) -> Vec<Diag> {
    Vec::new()
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
        Mode::Old => old,
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
