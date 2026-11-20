//! S5-0 / S6-0 の計器 (`設計_S5S6_型付きIRの一本化.md` 4 章・7 章)。動作は変えない。
//!
//! 有効にする方法は 2 つ: 環境変数 `KSN_S5_METER=1` (下ろしの終わりに報告を stderr へ出す)、
//! または隠しの `ksn s5-meter` (カレントのパッケージを下ろして報告を stdout へ出す)。
//! 報告は JSON 行。`kind` が `site` (箇所ごとの回数)・`disagree` (TAST と食い違った式)・
//! `unsupported` (KIR に下ろせなかった関数と理由)。
//!
//! 突き合わせの相手は `TExpr::ty` (`tast::build` が `tytab` から組んだ木)。下ろしが `tytab` から読む型と、
//! 同じ `(def名, 式 ID)` の TAST の型を比べる。TAST の型は `tytab` から機械的に組むので、ずれるのは
//! 下ろし側が型を読み直す・補う所 (スロット型での補い、`Ref` の特例、単相化の代入) だけになる想定。

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::program::Program;
use crate::tast::Tast;
use crate::types::Ty;

#[derive(Default, Clone, Debug)]
struct Site {
    /// 箇所の実行回数 (同じ式でも下ろしが何度読めば増える)。
    count: usize,
    /// TAST に同じ (def, 式 ID) の型が無かった回数。
    no_tast: usize,
    /// TAST の型が `Unknown` だった回数。
    tast_unknown: usize,
    /// 食い違った回数 (重複を除く前)。
    disagree: usize,
}

thread_local! {
    static FORCED: Cell<bool> = const { Cell::new(false) };
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static TAST: RefCell<HashMap<(String, u32), Ty>> = RefCell::new(HashMap::new());
    static SITES: RefCell<BTreeMap<&'static str, Site>> = RefCell::new(BTreeMap::new());
    static SEEN: RefCell<HashSet<(String, String, u32)>> = RefCell::new(HashSet::new());
    static DIS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static UNSUP: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static PKG: RefCell<(usize, usize)> = const { RefCell::new((0, 0)) };
}

/// 計器が有効か (環境変数、または `force` で強制されたとき)。
pub fn on() -> bool {
    FORCED.with(|f| f.get()) || std::env::var_os("KSN_S5_METER").is_some_and(|v| !v.is_empty() && v != "0")
}

/// 隠しの `ksn s5-meter` から呼ぶ。
pub fn force() {
    FORCED.with(|f| f.set(true));
}

/// 下ろしの本体の間だけ立てる。`from_v` / `Program::resolve` / 種類の導出の回数は、この間だけ数える。
pub fn set_active(on: bool) {
    ACTIVE.with(|a| a.set(on));
}

/// 下ろしの回数 (下ろしの中だけ)。
pub fn count_active(site: &'static str) {
    if ACTIVE.with(|a| a.get()) {
        SITES.with(|s| s.borrow_mut().entry(site).or_default().count += 1);
    }
}

/// 数えるだけの箇所 (突き合わせをしない) の回数を足す。`count_active` と同じだが、下ろしの外でも数える。
pub fn count(site: &'static str) {
    SITES.with(|s| s.borrow_mut().entry(site).or_default().count += 1);
}

/// TAST の型の表を作る (`tast::build` の結果から、式 ID → 型)。
pub fn set_tast(p: &Program, t: &Tast) {
    let mut m: HashMap<(String, u32), Ty> = HashMap::new();
    for (di, body) in &t.bodies {
        let name = p.defs[*di].name.clone();
        crate::tast_rules::impl_sig::walk(&body.body, &mut |e| {
            if e.id != 0 {
                m.entry((name.clone(), e.id)).or_insert_with(|| e.ty.clone());
            }
        });
    }
    TAST.with(|t| *t.borrow_mut() = m);
}

/// TAST の型 (無ければ `None`)。
pub fn tast_ty(def: &str, id: u32) -> Option<Ty> {
    TAST.with(|t| t.borrow().get(&(def.to_string(), id)).cloned())
}

/// 下ろしが読んだ型 `lowered` (表示) を TAST の型 (表示と `Unknown` かどうか) と比べる。
/// 食い違いは重複を除いて 1 行ずつ残す。
pub fn compare(site: &'static str, def: &str, id: u32, line: usize, lowered: &str, tast: Option<(String, bool)>) {
    count(site);
    match tast {
        None => SITES.with(|s| s.borrow_mut().entry(site).or_default().no_tast += 1),
        Some((_, true)) => SITES.with(|s| s.borrow_mut().entry(site).or_default().tast_unknown += 1),
        Some((shown, false)) => {
            if shown == lowered {
                return;
            }
            SITES.with(|s| s.borrow_mut().entry(site).or_default().disagree += 1);
            let fresh = SEEN.with(|s| s.borrow_mut().insert((site.to_string(), def.to_string(), id)));
            if fresh {
                let line_json = serde_json::json!({
                    "kind": "disagree", "site": site, "def": def, "id": id, "line": line,
                    "tast": shown, "lowered": lowered,
                });
                DIS.with(|d| d.borrow_mut().push(line_json.to_string()));
            }
        }
    }
}

/// 下ろしが KIR に下ろせなかった関数 (S6-0)。
pub fn unsupported(def: &str, file: &str, reason: &str, std: bool) {
    let origin = if std { "std" } else { "own" };
    let v = serde_json::json!({ "kind": "unsupported", "fn": def, "file": file, "reason": reason, "origin": origin });
    UNSUP.with(|u| u.borrow_mut().push(v.to_string()));
}

/// パッケージの被覆 (下ろした関数の数 / 対象の関数の数)。
pub fn coverage(lowered: usize, total: usize) {
    PKG.with(|p| *p.borrow_mut() = (lowered, total));
}

/// 報告を JSON 行で作り、計器の状態を空にする。
pub fn take_report() -> String {
    let mut out = String::new();
    let (lowered, total) = PKG.with(|p| *p.borrow());
    out.push_str(&serde_json::json!({ "kind": "coverage", "lowered": lowered, "total": total }).to_string());
    out.push('\n');
    SITES.with(|s| {
        for (name, st) in s.borrow().iter() {
            let v = serde_json::json!({
                "kind": "site", "site": name, "count": st.count, "no_tast": st.no_tast,
                "tast_unknown": st.tast_unknown, "disagree": st.disagree,
            });
            out.push_str(&v.to_string());
            out.push('\n');
        }
    });
    for l in DIS.with(|d| d.borrow().clone()) {
        out.push_str(&l);
        out.push('\n');
    }
    for l in UNSUP.with(|u| u.borrow().clone()) {
        out.push_str(&l);
        out.push('\n');
    }
    SITES.with(|s| s.borrow_mut().clear());
    SEEN.with(|s| s.borrow_mut().clear());
    DIS.with(|d| d.borrow_mut().clear());
    UNSUP.with(|u| u.borrow_mut().clear());
    TAST.with(|t| t.borrow_mut().clear());
    PKG.with(|p| *p.borrow_mut() = (0, 0));
    out
}
