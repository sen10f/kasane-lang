//! 型引数付きの利用者の関数・メソッドの単相化 (設計_KIR.md 20 章)。
//!
//! 呼び出しごとに型引数を決め (`::<..>` の明示、なければ引数と結果の型からの推論)、`fqn<型引数,..>` を鍵とする
//! 別の KIR 関数として下ろす。インスタンスの本体は、型パラメータを具体的な型で置き換えて (`Lw::subst`) 下ろす。
//! 効果 (`uses` / `panics`) や定数のパラメータを持つ宣言は対象外 (理由付きで拒否)。

use std::collections::{HashMap, HashSet};

use crate::check::{is_nil, items, nf, nlineno as nline};
use crate::parser::V;
use crate::program::Def;
use crate::types::{from_v, Ty as Src};

use super::{Lw, LR};

/// 宣言の型パラメータの名前の集合 (`from_v` の第 2 引数)。
pub(super) fn tnames(d: &Def) -> HashSet<String> {
    d.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.clone()).collect()
}

/// 型パラメータを `bind` で置き換える。`bind` に無い名前はそのまま。
pub(super) fn subst_ty(t: &Src, bind: &HashMap<String, Src>) -> Src {
    if bind.is_empty() {
        return t.clone();
    }
    let go = |x: &Src| subst_ty(x, bind);
    match t {
        Src::Param(n) => bind.get(n).cloned().unwrap_or_else(|| t.clone()),
        Src::Ref(m, x) => Src::Ref(*m, Box::new(go(x))),
        Src::Ptr(m, x) => Src::Ptr(*m, Box::new(go(x))),
        Src::Named(n, a) => Src::Named(n.clone(), a.iter().map(go).collect()),
        Src::Tuple(xs) => Src::Tuple(xs.iter().map(go).collect()),
        Src::Array(x, n) => Src::Array(Box::new(go(x)), *n),
        Src::Slice(x) => Src::Slice(Box::new(go(x))),
        Src::Closure { args, ret, cap, id, eff } => Src::Closure { args: args.iter().map(go).collect(), ret: Box::new(go(ret)), cap: cap.clone(), id: id.clone(), eff: eff.clone() },
        _ => t.clone(),
    }
}

/// 型が決まっているか (未確定・型変数・型パラメータが残っていない)。
pub(super) fn concrete(t: &Src) -> bool {
    match t {
        Src::Unknown | Src::Var(_) | Src::Param(_) | Src::Never => false,
        Src::Ref(_, x) | Src::Ptr(_, x) | Src::Array(x, _) | Src::Slice(x) => concrete(x),
        Src::Named(_, a) | Src::Tuple(a) => a.iter().all(concrete),
        _ => true,
    }
}

/// 宣言の型 `pat` (型パラメータを含む) と実際の型 `act` を突き合わせて、`bind` を埋める。
pub(super) fn unify(pat: &Src, act: &Src, bind: &mut HashMap<String, Src>) {
    match (pat, act) {
        (Src::Param(n), a) => {
            if concrete(a) {
                bind.entry(n.clone()).or_insert_with(|| a.clone());
            }
        }
        (Src::Ref(_, p), Src::Ref(_, a)) | (Src::Ptr(_, p), Src::Ptr(_, a)) | (Src::Array(p, _), Src::Array(a, _)) | (Src::Slice(p), Src::Slice(a)) => {
            unify(p, a, bind)
        }
        // `&[T]` の引数に配列・`Vec<T, A>` を渡す (参照外しの強制): 要素型を突き合わせる。
        (Src::Slice(p), Src::Array(a, _)) => unify(p, a, bind),
        (Src::Slice(p), Src::Named(v, a)) if v == "alloc.vec.Vec" && !a.is_empty() => unify(p, &a[0], bind),
        (Src::Named(n, ps), Src::Named(m, as_)) if n == m && ps.len() == as_.len() => {
            for (p, a) in ps.iter().zip(as_) {
                unify(p, a, bind)
            }
        }
        (Src::Tuple(ps), Src::Tuple(as_)) if ps.len() == as_.len() => {
            for (p, a) in ps.iter().zip(as_) {
                unify(p, a, bind)
            }
        }
        _ => {}
    }
}

impl<'a> Lw<'a> {
    /// 宣言された型 (型パラメータは、いま下ろしているインスタンスの具体的な型に置き換える)。
    pub(super) fn decl_ty(&self, v: &V) -> Src {
        subst_ty(&from_v(v, &tnames(self.def)), &self.subst)
    }

    /// 呼び先 `di` の型引数 (名前 → 具体的な型)。`targs` は `::<..>` の明示 (なければ `Nil`)、
    /// なければ引数 `args` (先頭の `skip` 個の宣言の引数は受け手なので飛ばす) と呼び出しの結果の型から決める。
    pub(super) fn mono_targs(&self, e: &V, di: usize, args: &[V], skip: usize, targs: &V, recv: Option<Src>) -> LR<HashMap<String, Src>> {
        let line = nline(e);
        let cd = &self.p.defs[di];
        if let Some(g) = cd.generics.iter().find(|g| g.0 != "life" && g.0 != "type" && (!matches!(g.0.as_str(), "uses" | "panics") || !is_nil(targs))) {
            return self.err(line, format!("{} は {} のパラメータ ({}) を持つので、単相化は対象外", cd.name, g.0, g.1));
        }
        let names: Vec<String> = cd.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.clone()).collect();
        let tn = tnames(cd);
        let mut bind: HashMap<String, Src> = HashMap::new();
        let explicit = items(targs);
        if !explicit.is_empty() {
            if explicit.len() != names.len() {
                return self.err(line, format!("{} の型引数の個数が違う (宣言 {} 個、明示 {} 個)", cd.name, names.len(), explicit.len()));
            }
            for (n, t) in names.iter().zip(explicit) {
                bind.insert(n.clone(), subst_ty(&from_v(t, &tnames(self.def)), &self.subst));
            }
        }
        // メソッドの受け手の型 (`self: &Kvec<T, A>` に対する `Kvec<u32, System>`)。参照の段数は両側から剥がして突き合わせる。
        if let (Some(r), Some((_, pt))) = (recv, cd.params.first().filter(|_| skip == 1)) {
            let mut pat = from_v(pt, &tn);
            while let Src::Ref(_, x) = pat {
                pat = *x;
            }
            let mut act = r;
            while let Src::Ref(_, x) = act {
                act = *x;
            }
            unify(&pat, &act, &mut bind);
        }
        for ((_, pt), a) in cd.params.iter().skip(skip).zip(args) {
            if let Ok(at) = self.src_ty(a) {
                unify(&from_v(pt, &tn), &at, &mut bind);
            }
        }
        if let Ok(rt) = self.src_ty(e) {
            unify(&from_v(&cd.ret, &tn), &rt, &mut bind);
        }
        for n in &names {
            match bind.get(n) {
                Some(t) if concrete(t) => {}
                Some(t) => return self.err(line, format!("{} の型引数 {} が決まらない ({})", cd.name, n, super::show(t))),
                None => return self.err(line, format!("{} の型引数 {} が決まらない (推論できない。`::<..>` で明示する)", cd.name, n)),
            }
        }
        Ok(bind)
    }

    /// 型引数 `tm` での単相インスタンスの鍵。まだ下ろしていなければ下ろして `coll.extra` に積む。
    pub(super) fn mono_instance(&mut self, di: usize, tm: &HashMap<String, Src>, line: usize) -> LR<String> {
        let p = self.p;
        let cd = &p.defs[di];
        // 名前に空白を入れない (KIR のテキストの関数のヘッダは空白で区切る)。
        let shown: Vec<String> = cd.generics.iter().filter(|g| g.0 == "type").map(|g| tm[&g.1].show().replace(", ", ",")).collect();
        let key = format!("{}<{}>", cd.name, shown.join(","));
        self.callees.insert(key.clone());
        if !self.mono_done.borrow_mut().insert(key.clone()) {
            return Ok(key);
        }
        let mut child = Lw::new(p, self.tys, self.aggs, cd);
        child.subst = tm.clone();
        child.inst_key = Some(key.clone());
        child.coll.prefix = key.clone();
        child.mono_done = self.mono_done.clone();
        let r = child.lower();
        match r {
            Ok(fs) => {
                self.coll.extra.extend(fs);
                Ok(key)
            }
            Err(why) => self.err(line, format!("{} の単相化 ({}) に失敗: {}", cd.name, key, why)),
        }
    }
}

/// 呼び出し `e` のうち、型引数を書ける位置 (`Call` は 3 番目、`MCall` は 4 番目)。
pub(super) fn targs_of(e: &V) -> &V {
    let f = nf(e);
    if crate::check::nname(e) == "MCall" { &f[3] } else { &f[2] }
}
