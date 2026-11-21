//! `with 名前 = 初期化式 close 後始末式 { 本体 }` (仕様 4.4) の検査。check.rs の子モジュール
//! (`#[path = "with_check.rs"] mod with_check;`) で、`Checker` の非公開の項目を使う。
//!
//! AST: `With [PBind(名前), mut, 初期化式, 後始末式, 本体ブロック]`。
//! 検査 (すべて「型が決まらない所では黙る」作法に従う):
//! - E3040: TAST の移動の規則 (tast_rules/moves.rs) が出す (旧の検査は取り外した)。
//! - E3041・E3042: TAST の移動の規則 (tast_rules/moves.rs の `with`) が出す。
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
        // E3043 (本体の値・外の変数への代入が束縛の借用を運ぶ) は、借用検査 (borrowck.rs の `With`) が見る。
        // 本体での move 記録は、後始末の検査に持ち越さない (後始末は本体とは別の経路)。
        self.tc_expr(&f[3], &inner, &inner_muts, false);
    }
}
