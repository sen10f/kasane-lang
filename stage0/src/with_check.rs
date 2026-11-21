//! `with 名前 = 初期化式 close 後始末式 { 本体 }` (仕様 4.4) の検査。check.rs の子モジュール
//! (`#[path = "with_check.rs"] mod with_check;`) で、`Checker` の非公開の項目を使う。
//!
//! AST: `With [PBind(名前), mut, 初期化式, 後始末式, 本体ブロック]`。
//! 検査 (すべて「型が決まらない所では黙る」作法に従う):
//! - E3040: TAST の移動の規則 (tast_rules/moves.rs) が出す (旧の検査は取り外した)。
//! - E3041: 後始末式が束縛を消費していない。
//! - E3042: 後始末式の中に `return` / `break` / `continue` / `?` がある (後始末は必ず最後まで走る)。
//! - E3043: 本体の値が束縛から借りた参照 (後始末のあとに残る)。
//! - 線形値の消費義務 (check.rs の `self.lin`): `with` の束縛は `lin_declare` しない。後始末式が全出口で消費するので、
//!   本体の `?` / `return` / `break` / `continue` の漏れ報告 (`lin_leaks`) に載らない。外側の線形値は従来どおり。
//! - E2045 (後始末式が `()` でない) は型推論 (types.rs の `With` の枝) が報告する。
//!
//! 効果 (`uses`) と panic は、後始末式も囲む関数の本体の一部なので、既存の呼び出し検査がそのまま数える。

use super::*;

impl<'a> Checker<'a> {
    /// 名前解決・効果・シャドーイングの走査 (`visit`) の `With` の枝。
    pub(super) fn visit_with(&mut self, e: &V, locals: &mut HashSet<String>, di: usize, free: bool) {
        let f = nf(e);
        self.visit(&f[2], locals, di, free);
        let name = st(&nf(&f[0])[0]);
        self.shadow_check(std::slice::from_ref(&name), locals, di, nline(e));
        let mut inner = locals.clone();
        inner.insert(name);
        self.visit(&f[3], &mut inner, di, free);
        self.visit(&f[4], &mut inner, di, free);
        if let Some((what, line)) = cleanup_exit(&f[3]) {
            self.diag(
                "E3042",
                format!("`{}` is not allowed in the `close` expression of `with`; cleanup always runs to the end (4.4)", what),
                di,
                line,
            );
        }
    }

    /// 型・所有権の走査 (`tc_expr`) の `With` の枝。
    pub(super) fn tc_with(&mut self, e: &V, env: &Env, muts: &BTreeSet<String>, consume: bool) {
        let f = nf(e);
        let name = st(&nf(&f[0])[0]);
        self.tc_expr(&f[2], env, muts, true);
        let mut ty = self.ty_of(&f[2], env);
        if is_nil(&ty) {
            // 初期化式の型は `?` などで自前では決まらないので、型推論の結果を使う。
            if let Some(t) = self.let_tys.get(&(self.d, crate::parser::nid(&f[0]))) {
                ty = crate::types::TypeCk::to_v(t).unwrap_or(V::Nil);
            }
        }
        let mut inner = env.clone();
        inner.insert(name.clone(), ty.clone());
        let mut inner_muts = muts.clone();
        if matches!(f[1], V::B(true)) {
            inner_muts.insert(name.clone());
        } else {
            inner_muts.remove(&name);
        }
        self.tc_expr(&f[4], &inner, &inner_muts, consume);
        // 型が分かり、Copy でも参照でもない束縛だけを検査する。
        let checkable = !is_nil(&ty) && tag(&ty) != "ref" && !self.is_copy(&ty);
        // E3043 (本体の値・外の変数への代入が束縛の借用を運ぶ) は、借用検査 (borrowck.rs の `With`) が見る。
        // 本体での move 記録は、後始末の検査に持ち越さない (後始末は本体とは別の経路)。
        self.tc_expr(&f[3], &inner, &inner_muts, false);
        if checkable && self.consuming_use(&f[3], &name, &inner, true, true).is_none() {
            self.tdiag("E3041", format!("the `close` expression of `with` must consume `{}` (4.4)", name), nline(&f[3]));
        }
    }

    /// 式 `e` の中に、束縛 `name` を値として消費する使い方があれば、その位置。
    /// 消費: 関数・メソッドの引数に直接渡す、`self` を値で受けるメソッドのレシーバ、`let` / 代入の右辺、
    /// `return` の値、構造体・タプル・配列の要素、ブロックの末尾の値、`move` キャプチャ、束縛そのものへの再代入。
    /// `whole`: 束縛全体の消費だけを数える (フィールドの一部の move は数えない)。
    /// `uncond`: 必ず評価される部分だけをたどる (`if` / `match` の枝・ループの本体・`&&` `||` の右辺・
    /// クロージャの中は除く)。後始末式が束縛を「必ず」消費するかの判定に使う。
    fn consuming_use(&self, e: &V, name: &str, env: &Env, whole: bool, uncond: bool) -> Option<usize> {
        if self.node_consumes(e, name, env, whole) {
            return Some(nline(e));
        }
        let f = nf(e);
        if uncond {
            // 全経路で消費するなら消費とみなす: `if` は両方の腕、`match` はすべての腕 (E3003 と同じ考え方)。
            let all = |bodies: Vec<&V>| -> bool {
                !bodies.is_empty() && bodies.into_iter().all(|b| self.consuming_use(b, name, env, whole, true).is_some())
            };
            match nname(e) {
                "If" if !is_nil(&f[2]) && all(vec![&f[1], &f[2]]) => return Some(nline(e)),
                "Match" => {
                    let scrut_is_place = nname(&f[0]) == "Path" && strs(&nf(&f[0])[0]) == [name.to_string()];
                    if scrut_is_place || all(items(&f[1]).iter().map(|a| &items(a)[1]).collect()) {
                        return Some(nline(e));
                    }
                }
                _ => {}
            }
        }
        let kids: Vec<&V> = match nname(e) {
            "If" if uncond => vec![&f[0]],
            "Match" if uncond => vec![&f[0]],
            "While" if uncond => vec![&f[0]],
            "For" if uncond => vec![&f[1]],
            "Loop" | "Closure" if uncond => vec![],
            "Bin" if uncond && matches!(st(&f[0]).as_str(), "&&" | "||") => vec![&f[1]],
            _ => children(e),
        };
        kids.into_iter().filter(|c| !is_pat(c)).find_map(|c| self.consuming_use(c, name, env, whole, uncond))
    }

    /// ノード `n` そのものが束縛 `name` を消費するか (子は見ない)。
    fn node_consumes(&self, n: &V, name: &str, env: &Env, whole: bool) -> bool {
        let f = nf(n);
        // 場所 `name` / `name.f` を値として使っているか。型が分かる Copy でないときだけ。
        let place = |x: &V| -> bool {
            if nname(x) != "Path" {
                return false;
            }
            let segs = strs(&nf(x)[0]);
            if segs[0] != name {
                return false;
            }
            if segs.len() == 1 {
                return true;
            }
            !whole && self.place_ty(name, &segs[1..], env).map_or(false, |t| !self.is_copy(&t))
        };
        let by_value = |recv: &[String], m: &str| -> bool {
            (!whole || recv.is_empty()) && self.method_takes_self_by_value(name, recv, m, env)
        };
        match nname(n) {
            "Call" => {
                let callee_hit = nname(&f[0]) == "Path" && {
                    let segs = strs(&nf(&f[0])[0]);
                    segs.len() >= 2 && segs[0] == name && by_value(&segs[1..segs.len() - 1], &segs[segs.len() - 1])
                };
                callee_hit || items(&f[1]).iter().any(place)
            }
            "MCall" => {
                let recv_hit = nname(&f[0]) == "Path" && {
                    let segs = strs(&nf(&f[0])[0]);
                    segs[0] == name && by_value(&segs[1..], &st(&f[1]))
                };
                recv_hit || items(&f[2]).iter().any(place)
            }
            "Let" => place(&f[3]),
            // 束縛そのものへの再代入は、元の値を後始末の前に捨てる。
            "Assign" => place(&f[1]) || (nname(&f[0]) == "Path" && strs(&nf(&f[0])[0]) == [name.to_string()]),
            "Return" => place(&f[0]),
            // 入れ子の `with` の初期化式に束縛を渡すのも move (`with b = a close ..`)。
            "With" => place(&f[2]),
            // `match r { 値で分解・束縛するパターン => .. }` (`if let` も同じ形) は束縛を move する。
            "Match" => {
                nname(&f[0]) == "Path"
                    && strs(&nf(&f[0])[0]) == [name.to_string()]
                    && items(&f[1]).iter().any(|a| {
                        let mut bs = Vec::new();
                        pat_binds(&items(a)[0], &mut bs);
                        !bs.is_empty()
                    })
            }
            "StructLit" => items(&f[1]).iter().any(|x| place(&items(x)[1])),
            "TupleE" | "ArrayE" => items(&f[0]).iter().any(place),
            "Block" => items(&f[0]).last().map_or(false, |s| nname(s) == "ExprStmt" && place(&nf(s)[0])),
            "Closure" => items(&f[2]).iter().any(|c| st(tel(c, 0)) == "move" && st(tel(c, 1)) == name),
            _ => false,
        }
    }
}

/// 後始末式の中の `return` / `break` / `continue` / `?` (クロージャの中は数えない)。
pub(crate) fn cleanup_exit(e: &V) -> Option<(&'static str, usize)> {
    match nname(e) {
        "Return" => return Some(("return", nline(e))),
        "Break" => return Some(("break", nline(e))),
        "Continue" => return Some(("continue", nline(e))),
        "Try" => return Some(("?", nline(e))),
        "Closure" => return None,
        _ => {}
    }
    children(e).into_iter().find_map(cleanup_exit)
}
