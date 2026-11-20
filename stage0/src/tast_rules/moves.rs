//! 移動と線形の規則 (仕様 4.1 の move・4.4 の線形): E3002 (move 済みの使用・参照の先からの move)、E3003 (線形の値を使わずに捨てる)、
//! E3006 (線形な型を `linear` と宣言していない型パラメータに渡す)、E3040 (`with` の本体で束縛を消費する)。
//!
//! 式の形ごとの場合分けはせず、TAST の**場所 (`Place`) と型** (Copy か・線形か) だけを見るデータフロー:
//! - 状態 `St` = `moved` (どこかの経路で move 済みの場所 → 行。合流は和集合) と `must` (全経路で move 済みの場所。合流は共通部分)。
//!   場所は (局所の束縛, 欄の連なり)。使用は 2 つの文脈 `Cx::Value` (値として取り出す = Copy でなければ move) と `Cx::Borrow` (借りる・読むだけ)。
//! - 線形の束縛は `lin` に義務として積み、`must` が (欄ごとに) 覆っているかを、スコープの終わり・`return` / `break` / `continue` / `?` の出口・
//!   分岐の合流 (一部の経路だけ消費) で見る。
//! - 制御の流れ: `if` / `match` の腕の合流、`loop` / `while` / `for` は本体を 2 回流して (1 回目の終わりの状態を入口に足す) 繰り返しの move を拾い、
//!   `return` / `break` / `continue` は状態を死なせる (`St::dead`)。
//! 旧い検査 (`check.rs` の `use_place` / `lin_*`、`with_check.rs`) と同じコード・同じ行・同じ文面で出す。

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::check::Diag;
use crate::parser::V;
use crate::program::{Def, Program};
use crate::tast::*;
use crate::types::{from_v, Cap, Ty, TypeCk};

type Root = (String, PatId);
type PKey = (Root, Vec<String>);

pub fn check(p: &Program, user: &[usize], tast: &Tast, tk: &TypeCk) -> Vec<Diag> {
    let mut out = vec![];
    let mut seen = HashSet::new();
    for &di in user {
        let mut w = W::new(p, tk, di, &mut out, &mut seen);
        w.sig_check();
        if let Some(b) = tast.bodies.get(&di) {
            w.body(b);
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cx {
    Value,
    Borrow,
}

#[derive(Clone, Default)]
struct St {
    dead: bool,
    moved: BTreeMap<PKey, usize>,
    must: BTreeMap<PKey, usize>,
}

struct LinLocal {
    root: Root,
    name: String,
    line: usize,
    ty: Ty,
}

struct LoopCtx {
    lin_base: usize,
    breaks: Vec<(String, St)>,
    conts: Vec<(String, St)>,
    /// 所有する線形な要素の列を回している (早い抜けで残りの要素が漏れる)。
    leaky: bool,
    line: usize,
}

/// 場所の式を平らにした結果。
struct PR {
    root: Option<Root>,
    path: Vec<String>,
    /// 参照・`Box` の先・添字を通る (そこから値を取り出す move はできない)。
    via: bool,
    text: String,
}

enum Scr {
    Place(PR, Ty),
    Val(Ty),
}

struct W<'a> {
    p: &'a Program,
    tk: &'a TypeCk<'a>,
    di: DefId,
    out: &'a mut Vec<Diag>,
    seen: &'a mut HashSet<(String, usize, String, String)>,
    st: St,
    tys: HashMap<Root, Ty>,
    scopes: Vec<HashMap<String, PatId>>,
    lin: Vec<LinLocal>,
    lin_floor: usize,
    loops: Vec<LoopCtx>,
    /// `with` の束縛と、本体の中でその束縛そのものへ再代入した行。
    withs: Vec<(Root, Option<usize>)>,
    /// 囲んでいるクロージャが参照 (`&` / `&mut`) で取り込んだ名前 → クロージャの行。
    refcaps: Vec<HashMap<String, usize>>,
    /// 参照越しの分解 (`&欄の型` の束縛モード) で得た束縛。
    refbound: HashSet<Root>,
    lin_cache: HashMap<String, bool>,
    gens: Vec<(String, bool, bool, bool)>,
}

/// 整数リテラルの添字 (`ls[0usize]`)。
fn const_index(e: &TExpr) -> Option<usize> {
    match &e.kind {
        TKind::Lit(s) => s.strip_prefix("int:")?.split(':').next()?.parse().ok(),
        _ => None,
    }
}

fn is_cmp(op: &str) -> bool {
    matches!(op, "==" | "!=" | "<" | "<=" | ">" | ">=")
}

fn prefix(a: &[String], b: &[String]) -> bool {
    a.len() <= b.len() && a.iter().zip(b).all(|(x, y)| x == y)
}

fn key_text(root: &str, path: &[String]) -> String {
    if path.is_empty() {
        root.to_string()
    } else {
        format!("{}.{}", root, path.join("."))
    }
}

impl<'a> W<'a> {
    fn new(p: &'a Program, tk: &'a TypeCk<'a>, di: DefId, out: &'a mut Vec<Diag>, seen: &'a mut HashSet<(String, usize, String, String)>) -> Self {
        let d = &p.defs[di];
        // 現在の定義の型パラメータ: (名前, Copy でありうる, `linear` 宣言あり)。
        let gens = d
            .generics
            .iter()
            .filter(|g| g.0 == "type")
            .map(|g| {
                let mut copy = false;
                let mut lin = false;
                let mut once = false;
                if let V::L(bs) = &g.2 {
                    for b in bs {
                        let bn = match b {
                            V::L(bx) if !bx.is_empty() => vs(&bx[0]),
                            V::S(x) => x.clone(),
                            _ => {
                                copy = true;
                                continue;
                            }
                        };
                        if bn == "FnOnce" || bn.ends_with(".FnOnce") {
                            once = true;
                        }
                        if bn == "linear" {
                            lin = true;
                        } else if bn == "Copy" || bn.ends_with(".Copy") {
                            copy = true;
                        } else if !bn.starts_with("core.") && !bn.starts_with("std.") && !bn.starts_with("alloc.") {
                            // ユーザー定義のトレイトの境界は上位トレイトに Copy を持ちうるので黙る。
                            copy = true;
                        }
                    }
                }
                (g.1.clone(), copy, lin, once)
            })
            .collect();
        W { p, tk, di, out, seen, st: St::default(), tys: HashMap::new(), scopes: vec![HashMap::new()], lin: vec![], lin_floor: 0, loops: vec![], withs: vec![], refcaps: vec![], refbound: HashSet::new(), lin_cache: HashMap::new(), gens }
    }

    // ------------------------------------------------------------ 診断

    fn diag(&mut self, code: &str, msg: String, line: usize) {
        if self.st.dead {
            return;
        }
        let d = &self.p.defs[self.di];
        let line = if line != 0 { line } else { crate::parser::unpack_pos(d.line).0 };
        if self.seen.insert((d.file.clone(), line, code.to_string(), msg.clone())) {
            self.out.push(Diag { file: d.file.clone(), code: code.into(), fqn: d.name.clone(), line, col: 0, msg });
        }
    }

    // ------------------------------------------------------------ 型だけを見る判定

    fn copy(&self, t: &Ty) -> bool {
        match t {
            Ty::Unknown | Ty::Var(_) => true,
            Ty::Never | Ty::Unit | Ty::Prim(_) | Ty::Ptr(..) | Ty::Const(_) | Ty::Dyn(..) => true,
            Ty::Ref(m, _, _) => !m,
            Ty::Tuple(xs) => xs.iter().all(|x| self.copy(x)),
            Ty::Array(x, _) | Ty::Slice(x) => self.copy(x),
            Ty::Param(n) => self.gens.iter().find(|g| &g.0 == n).map_or(true, |g| g.1),
            Ty::Closure { cap, id, .. } => {
                if *cap == Cap::FnOnce {
                    return false;
                }
                match id.as_ref().and_then(|n| self.tk.side.closure_caps.get(n)) {
                    Some(cs) => cs.iter().all(|c| match c.kind.as_str() {
                        "&mut" => false,
                        "move" => self.copy(&c.ty),
                        _ => true,
                    }),
                    None => true,
                }
            }
            Ty::Named(n, args, _) => {
                if n == "Option" || n == "Result" {
                    return args.iter().all(|a| self.copy(a));
                }
                // 手書きの `impl core.ops.Copy` は型引数に依らず Copy (`is_copy_v` と同じ。
                // `Handle<T>` のような型引数を欄に持たない取っ手を Copy にできる)。
                if crate::check::has_manual_copy(self.p, n) {
                    return true;
                }
                let Some(&i) = self.p.types.get(n) else { return true };
                let d = &self.p.defs[i];
                let derived = d.derive.iter().any(|x| x == "Copy") || self.p.impls.contains(&("core.ops.Copy".to_string(), n.clone()));
                derived && args.iter().all(|a| self.copy(a))
            }
        }
    }

    fn linear(&mut self, t: &Ty) -> bool {
        match t {
            Ty::Tuple(xs) => xs.iter().any(|x| self.linear(x)),
            Ty::Array(x, _) | Ty::Slice(x) => self.linear(x),
            Ty::Param(n) => self.gens.iter().find(|g| &g.0 == n).map_or(false, |g| g.2),
            Ty::Closure { id, .. } => match id.as_ref().and_then(|n| self.tk.side.closure_caps.get(n)).cloned() {
                Some(cs) => cs.iter().any(|c| c.kind == "move" && self.linear(&c.ty)),
                None => false,
            },
            Ty::Named(n, args, _) => {
                if n == "std.sync.Sender" {
                    return false;
                }
                if args.iter().any(|a| self.linear(a)) {
                    return true;
                }
                if let Some(&b) = self.lin_cache.get(n) {
                    return b;
                }
                let Some(&i) = self.p.types.get(n) else { return false };
                self.lin_cache.insert(n.clone(), false);
                let d: &Def = &self.p.defs[i];
                let gn = d.gnames();
                let mut r = d.linear;
                if !r {
                    let tys: Vec<Ty> = d.fields.iter().map(|f| from_v(&f.1, &gn)).chain(d.variants.iter().flat_map(|v| v.1.iter().map(|x| from_v(x, &gn)))).collect();
                    r = tys.iter().any(|x| self.linear(x));
                }
                self.lin_cache.insert(n.clone(), r);
                r
            }
            _ => false,
        }
    }

    fn linear_peek(&self, t: &Ty) -> bool {
        // `linear` はキャッシュを更新するので可変借用が要る。欄の型の線形判定だけの軽い読み (キャッシュ済みの型と型引数だけ)。
        match t {
            Ty::Tuple(xs) => xs.iter().any(|x| self.linear_peek(x)),
            Ty::Array(x, _) | Ty::Slice(x) => self.linear_peek(x),
            Ty::Param(n) => self.gens.iter().find(|g| &g.0 == n).map_or(false, |g| g.2),
            Ty::Named(n, args, _) => {
                if n == "std.sync.Sender" {
                    return false;
                }
                if args.iter().any(|a| self.linear_peek(a)) {
                    return true;
                }
                match self.p.types.get(n) {
                    Some(&i) => {
                        let d = &self.p.defs[i];
                        d.linear || self.lin_cache.get(n).copied().unwrap_or(false)
                    }
                    None => false,
                }
            }
            _ => false,
        }
    }

    /// 構造体 (欄を持つ `type`) の欄の名前と型 (型引数を入れた)。列挙型・欄のない型は `None`。
    fn struct_fields(&self, t: &Ty) -> Option<Vec<(String, Ty)>> {
        let Ty::Named(n, args, _) = t else { return None };
        let &i = self.p.types.get(n)?;
        let d = &self.p.defs[i];
        if d.kw != "type" || d.fields.is_empty() {
            return None;
        }
        let gn = d.gnames();
        let names: Vec<&String> = d.generics.iter().filter(|g| g.0 == "type").map(|g| &g.1).collect();
        let bind: HashMap<String, Ty> = if names.len() == args.len() { names.into_iter().cloned().zip(args.iter().cloned()).collect() } else { HashMap::new() };
        Some(d.fields.iter().map(|f| (f.0.clone(), subst(&from_v(&f.1, &gn), &bind))).collect())
    }

    /// 欄の連なりをたどった型 (分からなければ `None`) と、途中に参照があったか。
    fn walk_fields(&self, mut t: Ty, path: &[String]) -> (Option<Ty>, bool) {
        let mut via = false;
        for f in path {
            while let Ty::Ref(_, i, _) = t {
                via = true;
                t = *i;
            }
            match self.struct_fields(&t).and_then(|fs| fs.into_iter().find(|x| &x.0 == f)) {
                Some((_, ft)) => t = ft,
                None => return (None, via),
            }
        }
        (Some(t), via)
    }

    // ------------------------------------------------------------ 状態

    fn overlap(&self, root: &Root, path: &[String]) -> Option<(Vec<String>, usize)> {
        for ((r, k), l) in self.st.moved.range((root.clone(), vec![])..) {
            if r != root {
                break;
            }
            if prefix(k, path) || prefix(path, k) {
                return Some((k.clone(), *l));
            }
        }
        None
    }

    fn record_move(&mut self, root: &Root, path: &[String], line: usize) {
        self.st.moved.insert((root.clone(), path.to_vec()), line);
        self.st.must.insert((root.clone(), path.to_vec()), line);
    }

    /// `path` 以下 (`path` で始まる) の move の記録を消す (再代入・新しい束縛)。
    fn revive(&mut self, root: &Root, path: &[String]) {
        let ks: Vec<PKey> = self.st.moved.keys().chain(self.st.must.keys()).filter(|(r, k)| r == root && prefix(path, k)).cloned().collect();
        for k in ks {
            self.st.moved.remove(&k);
            self.st.must.remove(&k);
        }
    }

    fn covered(&mut self, root: &Root, path: &[String], ty: &Ty) -> bool {
        let hit = self.st.must.keys().any(|(r, k)| r == root && prefix(k, path));
        if hit || !self.linear(ty) {
            return true;
        }
        // `linear` と宣言された型は、全体を消費しなければ義務を果たさない (欄ごとの消費では足りない)。
        if let Ty::Named(n, _, _) = ty {
            if self.p.types.get(n).map_or(false, |&i| self.p.defs[i].linear) {
                return false;
            }
        }
        if let Ty::Array(x, n) = ty {
            let Some(n) = *n else { return false };
            if n > 4096 {
                return false;
            }
            let x = (**x).clone();
            for i in 0..n as usize {
                let mut pp = path.to_vec();
                pp.push(format!("[{}]", i));
                if !self.covered(root, &pp, &x) {
                    return false;
                }
            }
            return true;
        }
        match self.struct_fields(ty) {
            Some(fs) => {
                // 線形なのが欄の型ではなく型引数 (`Receiver<Res>`) なら、全体を消費しなければ果たさない。
                if !fs.iter().any(|(_, ft)| self.linear_peek(ft)) {
                    return false;
                }
                for (f, ft) in fs {
                    let mut pp = path.to_vec();
                    pp.push(f);
                    if !self.covered(root, &pp, &ft) {
                        return false;
                    }
                }
                true
            }
            None => false,
        }
    }

    /// 配列の束縛で、最初に消費されていない要素の名前 (`ls[1]`)。要素単位に分けられないときは束縛の名前。
    fn uncovered_elem(&mut self, root: &Root, name: &str, ty: &Ty) -> String {
        if let Ty::Array(x, Some(n)) = ty {
            let any = self.st.must.keys().any(|(r, k)| r == root && !k.is_empty());
            if any && *n <= 4096 {
                let x = (**x).clone();
                for i in 0..*n as usize {
                    if !self.covered(root, &[format!("[{}]", i)], &x) {
                        return format!("{}[{}]", name, i);
                    }
                }
            }
        }
        name.to_string()
    }

    fn declare(&mut self, name: &str, id: PatId, ty: &Ty, line: usize, owes: bool) {
        let root = (name.to_string(), id);
        self.revive(&root, &[]);
        self.tys.insert(root.clone(), ty.clone());
        if let Some(sc) = self.scopes.last_mut() {
            sc.insert(name.to_string(), id);
        }
        if owes && self.linear(ty) {
            self.lin.push(LinLocal { root, name: name.to_string(), line, ty: ty.clone() });
        }
    }

    fn lookup(&self, n: &str) -> Option<PatId> {
        self.scopes.iter().rev().find_map(|s| s.get(n).copied())
    }

    fn open_scope(&mut self) -> usize {
        self.scopes.push(HashMap::new());
        self.lin.len()
    }

    /// スコープの終わり: 未消費の線形な束縛は、宣言の行で E3003。
    fn close_scope(&mut self, base: usize) {
        self.scopes.pop();
        let base = base.min(self.lin.len());
        let gone: Vec<LinLocal> = self.lin.split_off(base);
        if self.st.dead {
            return;
        }
        for x in gone {
            if !self.covered(&x.root, &[], &x.ty) {
                let nm = self.uncovered_elem(&x.root, &x.name, &x.ty);
                self.diag("E3003", format!("linear value `{}` is never consumed", nm), x.line);
            }
        }
    }

    /// 出口 (`return` / `break` / `continue` / `?`): `from` 以降の束縛のうち未消費のものを `line` で E3003。
    fn leaks(&mut self, from: usize, line: usize, how: &str) {
        if self.st.dead {
            return;
        }
        let from = from.max(self.lin_floor);
        let xs: Vec<(Root, String, usize, Ty)> = self.lin.iter().skip(from).map(|x| (x.root.clone(), x.name.clone(), x.line, x.ty.clone())).collect();
        for (root, name, decl, ty) in xs {
            if !self.covered(&root, &[], &ty) {
                self.diag("E3003", format!("linear value `{}` (declared at line {}) is not consumed {}", name, crate::parser::unpack_pos(decl).0, how), line);
            }
        }
    }

    /// 経路の合流。生きている経路だけを集め、move 済みは和集合・全経路での move 済みは共通部分にする。
    /// 外側の線形な束縛が一部の経路でだけ消費済みなら、未消費の経路ごとに `line` で E3003 (報告したら消費済みとして続ける)。
    fn join(&mut self, ss: Vec<(String, St)>, line: usize) -> St {
        let live: Vec<(String, St)> = ss.into_iter().filter(|(_, s)| !s.dead).collect();
        if live.is_empty() {
            return St { dead: true, ..Default::default() };
        }
        let mut res = St::default();
        for (_, s) in &live {
            for (k, l) in &s.moved {
                res.moved.entry(k.clone()).or_insert(*l);
            }
        }
        for (k, l) in &live[0].1.must {
            if live.iter().all(|(_, s)| s.must.contains_key(k)) {
                res.must.insert(k.clone(), *l);
            }
        }
        if live.len() > 1 {
            let locals: Vec<(Root, String, usize, Ty)> = self.lin.iter().map(|x| (x.root.clone(), x.name.clone(), x.line, x.ty.clone())).collect();
            let saved = std::mem::take(&mut self.st);
            for (root, name, decl, ty) in locals {
                let flags: Vec<bool> = live
                    .iter()
                    .map(|(_, s)| {
                        self.st = s.clone();
                        self.covered(&root, &[], &ty)
                    })
                    .collect();
                if flags.iter().any(|&b| b) && !flags.iter().all(|&b| b) {
                    for (k, (desc, _)) in live.iter().enumerate() {
                        if !flags[k] {
                            self.st = saved.clone();
                            self.diag(
                                "E3003",
                                format!("linear value `{}` (declared at line {}) is not consumed on the path where {}", name, crate::parser::unpack_pos(decl).0, desc),
                                line,
                            );
                        }
                    }
                    res.must.insert((root, vec![]), line);
                }
            }
            self.st = saved;
        }
        res
    }

    // ------------------------------------------------------------ 場所

    /// 場所の式 (局所・欄・添字・`*`) を平らにする。途中の式 (添字・一時値の基) はここで評価する。
    fn place_of(&mut self, e: &TExpr) -> PR {
        match &e.kind {
            TKind::Path(segs) => {
                if let Some(Place::Local(n, id)) = flatten_local(e.place.as_ref()) {
                    let root = (n.clone(), id);
                    let mut path: Vec<String> = segs[1..].to_vec();
                    // 欄の連なりが 1 つの `Path` に入っている (`s.a.b`)。
                    if let Some(Place::Field(..)) = e.place {
                        path = fields_of(e.place.as_ref().unwrap());
                    }
                    let rt = self.tys.get(&root).cloned().unwrap_or(Ty::Unknown);
                    let (_, via) = self.walk_fields(rt.clone(), &path);
                    let via = via || (matches!(rt, Ty::Ref(..)) && !path.is_empty());
                    let text = key_text(&n, &path);
                    return PR { root: Some(root), path, via, text };
                }
                PR { root: None, path: vec![], via: false, text: segs.join(".") }
            }
            TKind::Field { base, name } if !self.is_place(base) && !matches!(base.ty, Ty::Ref(..)) && matches!(base.kind, TKind::Call { .. } | TKind::MCall { .. } | TKind::StructLit { .. }) => {
                // 一時値の欄: 欄を取り出すと残りの欄だけが残る。残りに線形な欄があれば漏れる (宣言が `linear` の型は欄だけでは消費できない)。
                self.eval(base, Cx::Value);
                let bt = base.ty.clone();
                if self.linear(&bt) {
                    let declared = matches!(&bt, Ty::Named(n, _, _) if self.p.types.get(n).map_or(false, |&i| self.p.defs[i].linear));
                    let others = self.struct_fields(&bt).map_or(false, |fs| fs.iter().any(|(n, t)| n != name && self.linear(&t.clone())));
                    if declared {
                        self.diag("E3003", "a linear temporary is borrowed or read through a field and is never consumed (bind it, or consume it)".into(), e.line);
                    } else if others {
                        self.diag("E3003", format!("a linear temporary's field `{}` is consumed but the temporary is not fully consumed (other linear fields are leaked)", name), e.line);
                    }
                }
                PR { root: None, path: vec![], via: false, text: format!("(call result).{}", name) }
            }
            TKind::Field { base, name } => {
                let mut r = self.place_of(base);
                // 一時値から取り出した欄の欄 (`(t.Box { .. }).r.id`): 取り出した欄が線形なら、読むだけでは消費されない。
                if r.root.is_none() && !self.is_place(base) && matches!(base.kind, TKind::Field { .. }) && !matches!(base.ty, Ty::Ref(..)) {
                    let bt = base.ty.clone();
                    if self.linear(&bt) {
                        self.diag("E3003", "a linear temporary is borrowed or read through a field and is never consumed (bind it, or consume it)".into(), e.line);
                    }
                }
                if matches!(base.ty, Ty::Ref(..)) {
                    r.via = true;
                }
                r.path.push(name.clone());
                r.text = format!("{}.{}", r.text, name);
                r
            }
            TKind::Index { base, index } if const_index(index).is_some() && matches!(&base.ty, Ty::Array(x, _) if self.linear(&(**x).clone())) => {
                // 配列の定数添字の要素は 1 つずつ move できる (線形な配列は要素ごとに消費する、4.4)。
                let mut r = self.place_of(base);
                let k = const_index(index).unwrap_or(0);
                r.path.push(format!("[{}]", k));
                r.text = format!("{}[{}]", r.text, k);
                r
            }
            TKind::Index { base, index } => {
                let mut r = self.place_of(base);
                self.eval(index, Cx::Value);
                r.via = true;
                r.text = format!("{}[..]", r.text);
                r
            }
            TKind::Unary { op, operand } if op == "*" => {
                let mut r = self.place_of(operand);
                r.via = true;
                r.text = format!("*{}", r.text);
                r
            }
            _ => {
                // 一時値: 評価して、線形なら借りるだけでは消費されない。
                self.eval(e, Cx::Borrow);
                PR { root: None, path: vec![], via: false, text: "(call result)".into() }
            }
        }
    }

    fn is_place(&self, e: &TExpr) -> bool {
        match &e.kind {
            TKind::Path(_) => e.place.is_some(),
            TKind::Field { base, .. } => self.is_place(base),
            TKind::Index { base, .. } => self.is_place(base),
            TKind::Unary { op, operand } => op == "*" && self.is_place(operand),
            _ => false,
        }
    }

    /// 場所の使用。move 済みと重なれば E3002、値として取り出す (Copy でない) なら move を記録 (参照の先・添字の先からは E3002)。
    fn apply(&mut self, pr: &PR, ty: &Ty, cx: Cx, line: usize) {
        let Some(root) = &pr.root else {
            // 一時値の欄: 参照の先からは取り出せない。
            if cx == Cx::Value && pr.via && !self.copy(ty) {
                self.diag(
                    "E3002",
                    format!("cannot move out of `{}`: it is behind a reference and its type is not Copy; borrow it with `&{}` or call `.clone()`", pr.text, pr.text),
                    line,
                );
            }
            return;
        };
        if let Some((mk, ml)) = self.overlap(root, &pr.path) {
            let ml = crate::parser::unpack_pos(ml).0;
            let key = key_text(&root.0, &pr.path);
            let mt = key_text(&root.0, &mk);
            let msg = if mk == pr.path {
                format!("use of moved value `{}` (moved at line {})", key, ml)
            } else if mk.len() < pr.path.len() {
                format!("use of moved value `{}` (moved `{}` at line {})", key, mt, ml)
            } else {
                format!("use of partially moved value `{}` (moved `{}` at line {})", key, mt, ml)
            };
            self.diag("E3002", msg, line);
            return;
        }
        if cx == Cx::Value && !self.copy(ty) {
            if pr.via {
                self.diag(
                    "E3002",
                    format!("cannot move out of `{}`: it is behind a reference or an index and its type is not Copy; borrow it with `&{}` or call `.clone()`", pr.text, pr.text),
                    line,
                );
                return;
            }
            let root = root.clone();
            self.record_move(&root, &pr.path.clone(), line);
        }
    }

    // ------------------------------------------------------------ 式

    fn eval(&mut self, e: &TExpr, cx: Cx) {
        self.eval_inner(e, cx);
        // 借りるだけの文脈に置かれた、線形な値を作る式 (一時値) は消費されない。
        if cx == Cx::Borrow && matches!(e.kind, TKind::Call { .. } | TKind::MCall { .. } | TKind::StructLit { .. }) {
            let t = e.ty.clone();
            if self.linear(&t) {
                self.diag("E3003", "a linear temporary is borrowed or read through a field and is never consumed (bind it, or consume it)".into(), e.line);
            }
        }
    }

    fn eval_inner(&mut self, e: &TExpr, cx: Cx) {
        match &e.kind {
            TKind::Lit(_) => {}
            TKind::Path(_) | TKind::Field { .. } | TKind::Index { .. } => {
                if self.is_place(e) {
                    let pr = self.place_of(e);
                    self.apply(&pr, &e.ty, cx, e.line);
                } else {
                    let pr = self.place_of(e);
                    self.apply(&pr, &e.ty, cx, e.line);
                }
            }
            TKind::Unary { op, operand } => match op.as_str() {
                "*" => {
                    if self.is_place(e) {
                        let pr = self.place_of(e);
                        self.apply(&pr, &e.ty, cx, e.line);
                    } else {
                        self.eval(operand, Cx::Borrow);
                    }
                }
                "&" | "&mut" => self.eval(operand, Cx::Borrow),
                _ => self.eval(operand, Cx::Value),
            },
            TKind::Binary { op, lhs, rhs } => {
                if op == "&&" || op == "||" {
                    self.eval(lhs, Cx::Value);
                    let before = self.st.clone();
                    self.eval(rhs, Cx::Value);
                    let after = std::mem::take(&mut self.st);
                    self.st = self.join(vec![("the right operand is not evaluated".into(), before), ("the right operand is evaluated".into(), after)], e.line);
                } else {
                    let c = if is_cmp(op) { Cx::Borrow } else { Cx::Value };
                    self.eval(lhs, c);
                    self.eval(rhs, c);
                }
            }
            TKind::Call { callee, args } => self.call(e, callee, args),
            TKind::MCall { recv, args, .. } => {
                let d = self.method_def(e);
                let by_value = d.map_or(false, |d| self.self_by_value(d));
                if self.is_place(recv) || !by_value {
                    let c = if by_value { Cx::Value } else { Cx::Borrow };
                    self.recv(recv, c, d, e.line);
                } else {
                    self.eval(recv, Cx::Value);
                    if let Ty::Ref(_, inner, _) = &recv.ty {
                        if !self.copy(&inner.clone()) {
                            let name = d.map(|d| d.name.rsplit('.').next().unwrap_or("").to_string()).unwrap_or_default();
                            self.diag(
                                "E3002",
                                format!("cannot move out of `(call result)`: `{}` takes `self` by value, but the receiver is behind a reference or an index; call it on an owned value or clone first", name),
                                e.line,
                            );
                        }
                    }
                }
                let skip = 1;
                self.args(args, d, skip);
                self.linear_targs(e, d);
            }
            TKind::StructLit { fields, .. } => {
                for (_, x) in fields {
                    self.eval(x, Cx::Value);
                }
            }
            TKind::Block(stmts) => {
                let base = self.open_scope();
                let n = stmts.len();
                for (i, s) in stmts.iter().enumerate() {
                    self.stmt(s, i + 1 == n, cx);
                }
                self.close_scope(base);
            }
            TKind::If { cond, then, els } => {
                self.eval(cond, Cx::Value);
                let before = std::mem::take(&mut self.st);
                self.st = before.clone();
                self.eval(then, cx);
                let a = std::mem::take(&mut self.st);
                self.st = before;
                if let Some(x) = els {
                    self.eval(x, cx);
                }
                let b = std::mem::take(&mut self.st);
                self.st = self.join(vec![("the `if` branch is taken".into(), a), ("the `else` path is taken".into(), b)], e.line);
            }
            TKind::Match { scrut, arms } => {
                let scr = self.scrut_pre(scrut);
                let s0 = self.st.clone();
                let mut outs = vec![];
                for (k, a) in arms.iter().enumerate() {
                    self.st = s0.clone();
                    let base = self.open_scope();
                    self.bind_pat(&a.pat, &scr, scrut.line, true);
                    if let Some(g) = &a.guard {
                        self.eval(g, Cx::Value);
                    }
                    self.eval(&a.body, cx);
                    self.close_scope(base);
                    outs.push((format!("arm {} of the `match` is taken", k + 1), std::mem::take(&mut self.st)));
                    // 取りこぼしのないパターン (`if let` の構造体の分解など) の後ろの腕には届かない。
                    if a.guard.is_none() && self.irrefutable(&a.pat) {
                        break;
                    }
                }
                self.st = self.join(outs, e.line);
            }
            TKind::Closure { params, captures, body, .. } => self.closure(e, params, captures, body),
            TKind::Other { name, children } => self.other(e, name, children),
        }
    }

    fn stmt(&mut self, s: &TStmt, tail: bool, cx: Cx) {
        match s {
            TStmt::Let { pat, init } => match init {
                Some(i) => {
                    let scr = self.scrut_pre(i);
                    self.bind_pat(pat, &scr, pat.line, false);
                }
                None => self.declare_pat(pat),
            },
            TStmt::Expr(x) => {
                if tail {
                    self.eval(x, cx);
                } else {
                    self.eval(x, Cx::Value);
                    let t = x.ty.clone();
                    if !self.is_place(x) && !matches!(t, Ty::Unit | Ty::Never) && self.linear(&t) {
                        self.diag("E3003", "a linear value produced by this statement is discarded without being consumed".into(), x.line);
                    }
                }
            }
            TStmt::Assign { place, value } => {
                self.eval(value, Cx::Value);
                self.assign(place, value);
            }
        }
    }

    fn assign(&mut self, place: &TExpr, _value: &TExpr) {
        let line = place.line;
        let lt = place.ty.clone();
        if !self.is_place(place) {
            let _ = self.place_of(place);
            return;
        }
        let pr = self.place_of(place);
        let Some(root) = pr.root.clone() else { return };
        if pr.via {
            // 参照・添字の先への代入は、その場所の読み (move 済みの参照の使用) と、線形な古い値の上書き。
            if let Some((_, ml)) = self.overlap(&root, &[]) {
                let ml = crate::parser::unpack_pos(ml).0;
                let key = key_text(&root.0, &[]);
                self.diag("E3002", format!("use of moved value `{}` (moved at line {})", key, ml), line);
            }
            if self.linear(&lt) {
                self.diag("E3003", "a linear value behind a reference is overwritten by the assignment without being consumed".into(), line);
            }
            return;
        }
        // 全体の再代入: 古い値が線形で未消費なら漏れ。
        if self.linear(&lt) && !self.covered(&root, &pr.path, &lt) {
            self.diag("E3003", format!("linear value `{}` is not consumed before the assignment at line {} overwrites it", pr.text, line), line);
        }
        if pr.path.is_empty() {
            for w in self.withs.iter_mut() {
                if w.0 == root && w.1.is_none() {
                    w.1 = Some(line);
                }
            }
        }
        self.revive(&root, &pr.path);
    }

    // ------------------------------------------------------------ 呼び出し

    fn method_def(&self, e: &TExpr) -> Option<&'a Def> {
        match &e.res {
            Some(Callee::Def(d)) => Some(&self.p.defs[*d]),
            Some(Callee::Methods(v)) if !v.is_empty() => {
                let first = &self.p.defs[v[0]];
                let bv = self.self_by_value(first);
                if v.iter().all(|i| self.self_by_value(&self.p.defs[*i]) == bv) {
                    Some(first)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn self_by_value(&self, d: &Def) -> bool {
        d.is_method() && !matches!(from_v(&d.params[0].1, &d.gnames()), Ty::Ref(..))
    }

    fn recv(&mut self, recv: &TExpr, cx: Cx, d: Option<&Def>, line: usize) {
        if cx == Cx::Value && self.is_place(recv) {
            let pr = self.place_of(recv);
            let name = d.map(|d| d.name.rsplit('.').next().unwrap_or("").to_string()).unwrap_or_default();
            self.recv_by_value(&pr, &recv.ty, &name, line);
        } else {
            self.eval(recv, cx);
        }
    }

    /// `self` を値で受けるメソッドのレシーバ (場所)。参照のレシーバは先を自動で外すので、先が Copy なら借りるだけ、そうでなければ参照の先からの move。
    fn recv_by_value(&mut self, pr: &PR, rt: &Ty, name: &str, line: usize) {
        let (inner_copy, is_ref) = match rt {
            Ty::Ref(_, i, _) => (self.copy(i), true),
            _ => (self.copy(rt), false),
        };
        let moved_out = !inner_copy && (is_ref || pr.via);
        if moved_out {
            if let Some(root) = &pr.root {
                if self.overlap(root, &pr.path).is_none() {
                    self.diag(
                        "E3002",
                        format!("cannot move out of `{}`: `{}` takes `self` by value, but the receiver is behind a reference or an index; call it on an owned value or clone first", pr.text, name),
                        line,
                    );
                    return;
                }
            }
        }
        let cx = if is_ref { Cx::Borrow } else { Cx::Value };
        self.apply(pr, rt, cx, line);
    }

    /// 実引数: 宣言の型が参照なら再借用 (move しない)。それ以外は値で渡す。呼び先が分からなければ `&mut` の引数は動かさない。
    fn args(&mut self, args: &[TExpr], d: Option<&Def>, skip: usize) {
        for (k, a) in args.iter().enumerate() {
            let cx = match d.and_then(|d| d.params.get(k + skip)) {
                Some((_, pt)) => {
                    if matches!(from_v(pt, &d.unwrap().gnames()), Ty::Ref(..)) {
                        Cx::Borrow
                    } else {
                        Cx::Value
                    }
                }
                None => {
                    if d.is_none() && matches!(a.ty, Ty::Ref(true, _, _)) {
                        Cx::Borrow
                    } else {
                        Cx::Value
                    }
                }
            };
            // 参照越しの分解で得た束縛 (参照) を、所有する型の仮引数へ渡す: 値の持ち主から取り出さなければならない。
            if let (Some(dd), TKind::Path(_)) = (d, &a.kind) {
                if let (Some((_, pt)), Ty::Ref(..), Some(Place::Local(n, id))) = (dd.params.get(k + skip), &a.ty, a.place.as_ref()) {
                    let want = from_v(pt, &dd.gnames());
                    if self.refbound.contains(&(n.clone(), *id)) && matches!(want, Ty::Named(..) | Ty::Prim(_) | Ty::Tuple(_)) && !self.copy(&want) {
                        self.diag(
                            "E3002",
                            format!("cannot move out of `{}`: it was bound by matching through a reference, so it is a reference; pass `{}.clone()` or take the value out of the owner", n, n),
                            a.line,
                        );
                    }
                }
            }
            self.eval(a, cx);
        }
    }

    fn call(&mut self, e: &TExpr, callee: &TExpr, args: &[TExpr]) {
        let TKind::Path(segs) = &callee.kind else {
            self.eval(callee, Cx::Value);
            self.args(args, None, 0);
            return;
        };
        // `x.m(..)` の経路形: 先頭が局所なら、受け手は `x` (と欄の連なり)。
        if let Some(id) = self.lookup(&segs[0]) {
            let root = (segs[0].clone(), id);
            let rty = self.tys.get(&root).cloned().unwrap_or(Ty::Unknown);
            if segs.len() == 1 {
                // 局所の関数値・クロージャの呼び出し: 一度しか呼べない (`FnOnce`) ものは呼ぶと move される。
                let pr = PR { root: Some(root), path: vec![], via: false, text: segs[0].clone() };
                let once = match &rty {
                    Ty::Closure { cap, .. } => *cap == Cap::FnOnce || self.linear(&rty.clone()),
                    Ty::Param(n) => self.gens.iter().any(|g| &g.0 == n && g.3),
                    _ => false,
                };
                // 参照で取り込んだ `FnOnce` を、取り込んだ先のクロージャの本体で呼ぶと、元の値を移すことになる。
                if once {
                    if let Some(cl) = self.refcaps.last().and_then(|m| m.get(&segs[0])).copied() {
                        self.diag(
                            "E3002",
                            format!("cannot move out of `{}`: it is an `FnOnce` captured by reference, and calling it moves it; capture it with `move {}`", segs[0], segs[0]),
                            cl,
                        );
                        self.args(args, None, 0);
                        return;
                    }
                }
                self.apply(&pr, &rty, if once { Cx::Value } else { Cx::Borrow }, e.line);
                self.args(args, None, 0);
                return;
            }
            let path: Vec<String> = segs[1..segs.len() - 1].to_vec();
            let (rt, via0) = self.walk_fields(rty.clone(), &path);
            let via = via0 || (matches!(rty, Ty::Ref(..)) && !path.is_empty());
            let d = self.method_def(e);
            let name = segs.last().cloned().unwrap_or_default();
            let by_value = d.map_or(false, |d| self.self_by_value(d)) || (d.is_none() && name == "call_once" && path.is_empty());
            let text = key_text(&segs[0], &path);
            let pr = PR { root: Some(root), path, via, text };
            let rt = rt.unwrap_or(Ty::Unknown);
            if by_value {
                self.recv_by_value(&pr, &rt, &name, e.line);
            } else {
                self.apply(&pr, &rt, Cx::Borrow, e.line);
            }
            self.args(args, d, 1);
            self.linear_targs(e, d);
            return;
        }
        if matches!(e.res, Some(Callee::Variant(..))) {
            for a in args {
                self.eval(a, Cx::Value);
            }
            return;
        }
        let d = match &e.res {
            Some(Callee::Def(d)) => Some(&self.p.defs[*d]),
            _ => None,
        };
        self.args(args, d, 0);
        self.linear_targs(e, d);
    }

    /// E3006: 線形な型を、`linear` と宣言していない型パラメータに渡している。
    fn linear_targs(&mut self, e: &TExpr, d: Option<&Def>) {
        let Some(d) = d else { return };
        if e.targs.is_empty() {
            return;
        }
        let gs: Vec<&(String, String, V)> = d.generics.iter().filter(|g| g.0 == "type" || g.0 == "const").collect();
        if gs.len() != e.targs.len() {
            return;
        }
        for (g, t) in gs.iter().zip(&e.targs) {
            if g.0 != "type" {
                continue;
            }
            let declared = matches!(&g.2, V::L(bs) if bs.iter().any(|b| matches!(b, V::S(x) if x == "linear")));
            if !declared && self.linear(t) {
                self.diag(
                    "E3006",
                    format!("linear value of type {} is passed for the type parameter `{}` of {}, which is not declared `linear` (4.4)", t.show(), g.1, d.name),
                    e.line,
                );
            }
        }
    }

    // ------------------------------------------------------------ クロージャ

    fn closure(&mut self, e: &TExpr, params: &[TBinding], captures: &[Capture], body: &TExpr) {
        // 取り込み: `move` は Copy でなければ移し、`&` / `&mut` は読むだけ。
        let seed = self.st.moved.clone();
        let mut refc: HashMap<String, usize> = HashMap::new();
        let mut moved_lin: Vec<(String, PatId, Ty)> = vec![];
        for c in captures {
            let Some(id) = self.lookup(&c.name) else { continue };
            let root = (c.name.clone(), id);
            let ty = self.tys.get(&root).cloned().unwrap_or_else(|| c.ty.clone());
            let pr = PR { root: Some(root), path: vec![], via: false, text: c.name.clone() };
            if c.kind == "move" {
                self.apply(&pr, &ty, Cx::Value, e.line);
                if self.linear(&ty) {
                    moved_lin.push((c.name.clone(), id, ty));
                }
            } else {
                refc.insert(c.name.clone(), e.line);
                self.apply(&pr, &ty, Cx::Borrow, e.line);
            }
        }
        // 本体は別の枠: 状態・出口・繰り返しを切り離す。
        let saved_st = std::mem::replace(&mut self.st, St { moved: seed, ..St::default() });
        self.refcaps.push(refc);
        let saved_floor = self.lin_floor;
        let saved_loops = std::mem::take(&mut self.loops);
        let saved_withs = std::mem::take(&mut self.withs);
        self.lin_floor = self.lin.len();
        let base = self.open_scope();
        for (name, id, ty) in moved_lin {
            // 取り込んだ線形な値の義務は、クロージャ本体が全経路で消費する (宣言の行はクロージャの行)。
            let dl = crate::parser::unpack_pos(self.p.defs[self.di].line).0;
            self.lin.push(LinLocal { root: (name, id), name: String::new(), line: dl, ty });
            let last = self.lin.last_mut().unwrap();
            last.name = last.root.0.clone();
        }
        for q in params {
            self.declare(&q.name, 0, &q.ty, e.line, true);
        }
        self.eval(body, Cx::Value);
        self.close_scope(base);
        self.refcaps.pop();
        self.st = saved_st;
        self.lin_floor = saved_floor;
        self.loops = saved_loops;
        self.withs = saved_withs;
    }

    // ------------------------------------------------------------ 制御・その他

    fn other(&mut self, e: &TExpr, name: &str, ch: &[TExpr]) {
        match name {
            "Return" => {
                for c in ch {
                    self.eval(c, Cx::Value);
                }
                self.leaks(0, e.line, "before this `return`");
                self.leak_elems(e.line, true);
                self.st.dead = true;
            }
            "Try" => {
                for c in ch {
                    self.eval(c, Cx::Value);
                }
                self.leaks(0, e.line, "when `?` returns early");
                self.leak_elems(e.line, true);
            }
            "Break" | "Continue" => {
                let brk = name == "Break";
                if let Some(base) = self.loops.last().map(|l| l.lin_base) {
                    self.leaks(base, e.line, if brk { "before this `break`" } else { "before this `continue`" });
                    if brk {
                        self.leak_elems(e.line, false);
                    }
                    let s = self.st.clone();
                    let l = self.loops.last_mut().unwrap();
                    if brk {
                        l.breaks.push((format!("the loop exits at line {}", e.line), s));
                    } else {
                        l.conts.push((format!("the loop continues at line {}", e.line), s));
                    }
                }
                self.st.dead = true;
            }
            "Loop" => self.run_loop(e, None, ch.first(), None, false, false, true),
            "While" => self.run_loop(e, ch.first(), ch.get(1), None, false, false, false),
            "For" => {
                let it = &ch[0];
                // 所有する `Vec` と配列だけが move。それ以外の反復子 (利用者の `Iterator`) は `for` がその場で進める (旧と同じ、仕様が未規定の所)。
                let owned = matches!(&it.ty, Ty::Array(..)) || matches!(&it.ty, Ty::Named(n, a, _) if n == "alloc.vec.Vec" && !a.is_empty());
                self.eval(it, if owned { Cx::Value } else { Cx::Borrow });
                let leaky = match &it.ty {
                    Ty::Array(x, _) => self.linear(&x.clone()),
                    Ty::Named(n, a, _) if (n.ends_with("Vec") || n.ends_with("IntoIter")) && !a.is_empty() => self.linear(&a[0].clone()),
                    _ => false,
                };
                self.run_loop(e, None, ch.get(1), e.pats.first(), leaky, !matches!(it.ty, Ty::Ref(..)), false);
            }
            "With" => self.with(e, ch),
            "ArrayRep" => {
                if let Some(first) = ch.first() {
                    if !self.copy(&first.ty) {
                        self.diag(
                            "E3002",
                            "the value repeated by `[e; N]` must be `Copy`: its type is not; build the elements one by one (or use `alloc.vec.Vec`)".into(),
                            e.line,
                        );
                    }
                }
                for c in ch {
                    self.eval(c, Cx::Value);
                }
            }
            _ => {
                for c in ch {
                    self.eval(c, Cx::Value);
                }
            }
        }
    }

    /// 所有する線形な要素の列を回している間の早い抜けは、残りの要素を漏らす。
    fn leak_elems(&mut self, line: usize, all: bool) {
        if self.st.dead {
            return;
        }
        let hit = if all { self.loops.iter().rev().find(|l| l.leaky) } else { self.loops.last().filter(|l| l.leaky) };
        let _ = line;
        if let Some(l) = hit {
            let ll = l.line;
            self.diag("E3003", "linear values of the iterated sequence may not all be consumed when the loop exits early (break or `return`)".into(), ll);
        }
    }

    fn run_loop(&mut self, e: &TExpr, cond: Option<&TExpr>, body: Option<&TExpr>, pat: Option<&TPat>, leaky: bool, by_val_iter: bool, is_loop: bool) {
        let entry = self.st.clone();
        let mut in_state = entry.clone();
        let mut exits: Vec<(String, St)> = vec![];
        for pass in 0..2 {
            // 2 回目: 1 回目の終わりの状態が入口に加わる (繰り返しでの move を拾う)。
            self.st = in_state.clone();
            self.loops.push(LoopCtx { lin_base: self.lin.len(), breaks: vec![], conts: vec![], leaky, line: e.line });
            if let Some(c) = cond {
                self.eval(c, Cx::Value);
            }
            let after_cond = self.st.clone();
            let base = self.open_scope();
            if let Some(q) = pat {
                if leaky || by_val_iter {
                    self.pat_leaks(q);
                }
                self.declare_pat(q);
            }
            if let Some(b) = body {
                self.eval(b, Cx::Value);
            }
            self.close_scope(base);
            let ctx = self.loops.pop().unwrap();
            let mut ends = vec![("the loop body ends".to_string(), std::mem::take(&mut self.st))];
            ends.extend(ctx.conts);
            let end = self.join(ends, e.line);
            exits = vec![];
            if !is_loop {
                exits.push(("the loop condition fails".to_string(), after_cond));
            }
            exits.extend(ctx.breaks);
            if pass == 0 {
                in_state = self.join(vec![("the loop is not entered".into(), entry.clone()), ("the loop continues".into(), end)], e.line);
            }
        }
        self.st = self.join(exits, e.line);
    }

    fn with(&mut self, e: &TExpr, ch: &[TExpr]) {
        // ch = [init, close?, body]: `close` は常にある (構文)。
        let Some(init) = ch.first() else { return };
        self.eval(init, Cx::Value);
        let base = self.open_scope();
        let pat = e.pats.first();
        let (name, id, ty) = match pat.and_then(|q| q.info.as_ref()).and_then(|i| i.bind.clone()) {
            Some((n, t, _)) => (n, pat.unwrap().id, t),
            None => {
                self.close_scope(base);
                return;
            }
        };
        // 束縛は義務を負わない (`close` が全出口で消費する)。
        self.declare(&name, id, &ty, e.line, false);
        let root = (name.clone(), id);
        self.withs.push((root.clone(), None));
        let (close, body) = match ch.len() {
            3 => (Some(&ch[1]), &ch[2]),
            _ => (None, &ch[ch.len() - 1]),
        };
        self.eval(body, Cx::Value);
        let (_, reassigned) = self.withs.pop().unwrap();
        // E3040: 本体の終わりで束縛 (かその欄) が move 済み、または再代入されている。
        let checkable = !matches!(ty, Ty::Ref(..)) && !self.copy(&ty);
        if checkable {
            let moved = self.st.moved.iter().filter(|((r, _), _)| *r == root).map(|(_, l)| *l).min();
            if let Some(line) = moved.or(reassigned) {
                self.diag("E3040", format!("`{}` cannot be moved or reassigned in the `with` body; the `close` expression consumes it (4.4)", name), line);
            }
        }
        // `close` は本体とは別の経路。束縛の move の記録を消して評価する。
        self.revive(&root, &[]);
        if let Some(c) = close {
            self.eval(c, Cx::Value);
        }
        self.revive(&root, &[]);
        self.close_scope(base);
    }

    // ------------------------------------------------------------ パターン

    /// 走査対象 (`match` の被検査式・`let` の初期化式) を評価する。場所なら使用の検査だけで move は記録しない (パターンが決める)。
    fn scrut_pre(&mut self, scrut: &TExpr) -> Scr {
        if self.is_place(scrut) {
            let pr = self.place_of(scrut);
            let mut ok = true;
            if let Some(root) = &pr.root {
                if let Some((mk, ml)) = self.overlap(root, &pr.path) {
                    let ml = crate::parser::unpack_pos(ml).0;
                    let key = key_text(&root.0, &pr.path);
                    let msg = if mk == pr.path {
                        format!("use of moved value `{}` (moved at line {})", key, ml)
                    } else {
                        format!("use of moved value `{}` (moved `{}` at line {})", key, key_text(&root.0, &mk), ml)
                    };
                    self.diag("E3002", msg, scrut.line);
                    ok = false;
                }
            }
            if ok {
                return Scr::Place(pr, scrut.ty.clone());
            }
            return Scr::Place(PR { root: None, path: vec![], via: false, text: pr.text }, scrut.ty.clone());
        }
        self.eval(scrut, Cx::Value);
        Scr::Val(scrut.ty.clone())
    }

    fn irrefutable(&self, q: &TPat) -> bool {
        match q.kind {
            "PWild" | "PBind" => true,
            "PTuple" => q.children.iter().all(|c| self.irrefutable(c)),
            "PStruct" => q.info.as_ref().map_or(false, |i| self.struct_fields(&i.ty).is_some()) && q.children.iter().all(|c| self.irrefutable(c)),
            _ => false,
        }
    }

    /// 分解で値として取り出す (Copy でない) 部分の、被検査式の場所からの相対の道。
    fn pat_moves(&self, q: &TPat, path: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        match q.kind {
            "PBind" => {
                if let Some((_, t, BindMode::Move)) = q.info.as_ref().and_then(|i| i.bind.clone()) {
                    if !self.copy(&t) {
                        out.push(path.clone());
                    }
                }
            }
            "PStruct" => {
                for (f, c) in q.lit.iter().zip(&q.children) {
                    path.push(f.clone());
                    self.pat_moves(c, path, out);
                    path.pop();
                }
            }
            "PTuple" | "PVariant" => {
                let mut sub = vec![];
                for c in &q.children {
                    self.pat_moves(c, &mut vec![], &mut sub);
                }
                if !sub.is_empty() {
                    out.push(path.clone());
                }
            }
            _ => {}
        }
    }

    /// パターンが取り出さない線形な部分 (`_`・書かなかった欄) は、その場で捨てることになる。
    fn pat_leaks(&mut self, q: &TPat) {
        let ty = q.info.as_ref().map(|i| i.ty.clone()).unwrap_or(Ty::Unknown);
        match q.kind {
            "PWild" => {
                if self.linear(&ty) {
                    self.diag("E3003", "a linear value matched by the pattern `_` is discarded without being consumed".into(), q.line);
                }
            }
            "PStruct" => {
                if let Some(fs) = self.struct_fields(&ty) {
                    for (f, ft) in fs {
                        if !q.lit.contains(&f) && self.linear(&ft) {
                            self.diag("E3003", format!("a linear value in the field `{}` is not named by the pattern and is discarded without being consumed", f), q.line);
                        }
                    }
                }
                for c in &q.children {
                    self.pat_leaks(c);
                }
            }
            "PTuple" | "PVariant" => {
                for c in &q.children {
                    self.pat_leaks(c);
                }
            }
            _ => {}
        }
    }

    /// `Drop` を実装した型を所有値のままパターンで分解して欄を取り出すことはできない (4.4)。
    fn drop_destructure(&mut self, q: &TPat, line: usize) {
        fn binds(q: &TPat) -> bool {
            q.kind == "PBind" || q.children.iter().any(binds)
        }
        let mut stack = vec![q];
        let mut reported: HashSet<String> = HashSet::new();
        while let Some(x) = stack.pop() {
            stack.extend(x.children.iter());
            if !matches!(x.kind, "PStruct" | "PVariant" | "PTuple") {
                continue;
            }
            let Some(Ty::Named(n, _, _)) = x.info.as_ref().map(|i| i.ty.clone()) else { continue };
            let Some(&i) = self.p.types.get(&n) else { continue };
            let drops = self.p.impls.contains(&("core.ops.Drop".to_string(), n.clone())) || self.p.defs[i].derive.iter().any(|d| d == "Drop");
            if drops && x.children.iter().any(binds) && reported.insert(n.clone()) {
                self.diag(
                    "E3002",
                    format!("cannot destructure `{}` by value: it implements Drop, so its fields cannot be moved out (4.4); match on a reference or use accessors", n),
                    line,
                );
            }
        }
    }

    fn declare_pat(&mut self, q: &TPat) {
        self.declare_pat_at(q, None)
    }

    /// `line`: 束縛の宣言の行 (`None` なら束縛の行)。旧い検査は `match` の腕の束縛を関数ヘッダの行で報告する。
    fn declare_pat_at(&mut self, q: &TPat, line: Option<usize>) {
        if q.kind == "PBind" {
            if let Some((n, t, mode)) = q.info.as_ref().and_then(|i| i.bind.clone()) {
                if mode != BindMode::Move {
                    self.refbound.insert((n.clone(), q.id));
                }
                self.declare(&n, q.id, &t, line.unwrap_or(q.line), mode == BindMode::Move);
            }
        }
        for c in &q.children {
            self.declare_pat_at(c, line);
        }
    }

    fn bind_pat(&mut self, q: &TPat, scr: &Scr, line: usize, arm: bool) {
        let sty = match scr {
            Scr::Place(_, t) | Scr::Val(t) => t.clone(),
        };
        let by_value = !matches!(sty, Ty::Ref(..));
        let trivial = matches!(q.kind, "PWild");
        if by_value {
            self.drop_destructure(q, q.line);
        }
        match scr {
            Scr::Place(pr, ty) => {
                if let Some(root) = &pr.root {
                    let mut moves = vec![];
                    self.pat_moves(q, &mut vec![], &mut moves);
                    // 線形な値を分解 (または全体を束縛) するときは、全体を消費する。
                    let lin = self.linear(ty) && by_value;
                    if lin && !trivial {
                        moves = vec![vec![]];
                    }
                    // 型の注釈つきの `let y: &mut T = x` は再借用 (x を移さない)。
                    if q.kind == "PBind" && q.lit.iter().any(|l| l == "ann") && matches!(ty, Ty::Ref(..)) {
                        moves.clear();
                    }
                    if !moves.is_empty() {
                        if pr.via {
                            self.diag(
                                "E3002",
                                format!(
                                    "cannot move out of `{}` in a pattern: it is behind a reference or an index and the pattern binds a value that is not Copy; borrow it with `&{}` or match on a reference instead",
                                    pr.text, pr.text
                                ),
                                line,
                            );
                        } else {
                            let root = root.clone();
                            for m in moves {
                                let mut pp = pr.path.clone();
                                pp.extend(m);
                                self.record_move(&root, &pp, line);
                            }
                        }
                    }
                    if lin && !trivial {
                        self.pat_leaks(q);
                    }
                    // `_` で受ける線形な値は、捨てることになる (場所は消費済みとして続ける)。
                    if lin && trivial {
                        self.pat_leaks(q);
                        let root = root.clone();
                        self.record_move(&root, &pr.path.clone(), line);
                    }
                }
            }
            Scr::Val(ty) => {
                if by_value && self.linear(ty) {
                    self.pat_leaks(q);
                }
            }
        }
        let at = if arm { Some(crate::parser::unpack_pos(self.p.defs[self.di].line).0) } else { None };
        self.declare_pat_at(q, at);
    }

    // ------------------------------------------------------------ 定義の型

    /// E3006 (型の位置): 型定義の欄・変種のペイロード・仮引数・戻り値の型に、`linear` と宣言していない型パラメータへ線形な型を渡す適用がある。
    fn sig_check(&mut self) {
        let d = &self.p.defs[self.di];
        let gn = d.gnames();
        let mut tys: Vec<V> = d.fields.iter().map(|f| f.1.clone()).collect();
        tys.extend(d.variants.iter().flat_map(|v| v.1.iter().cloned()));
        tys.extend(d.params.iter().map(|q| q.1.clone()));
        tys.push(d.ret.clone());
        let line = crate::parser::unpack_pos(d.line).0;
        for v in tys {
            if matches!(v, V::Nil) {
                continue;
            }
            let t = from_v(&v, &gn);
            if t.has_param() {
                continue;
            }
            self.ty_linear_args(&t, line);
        }
    }

    fn ty_linear_args(&mut self, t: &Ty, line: usize) {
        match t {
            Ty::Named(n, args, _) => {
                if let Some(&i) = self.p.types.get(n) {
                    let d = &self.p.defs[i];
                    let params: Vec<(String, bool)> = d
                        .generics
                        .iter()
                        .filter(|g| g.0 == "type")
                        .map(|g| (g.1.clone(), matches!(&g.2, V::L(bs) if bs.iter().any(|b| matches!(b, V::S(x) if x == "linear")))))
                        .collect();
                    if params.len() == args.len() {
                        for ((pn, declared), a) in params.iter().zip(args) {
                            if !declared && self.linear(a) {
                                self.diag(
                                    "E3006",
                                    format!("linear type {} is given for the type parameter `{}` of {}, which is not declared `linear` (4.4)", a.show(), pn, n),
                                    line,
                                );
                            }
                        }
                    }
                }
                for a in args {
                    self.ty_linear_args(a, line);
                }
            }
            Ty::Ref(_, x, _) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => self.ty_linear_args(x, line),
            Ty::Tuple(xs) => {
                for x in xs {
                    self.ty_linear_args(x, line);
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ 本体

    fn body(&mut self, b: &TBody) {
        let d = &self.p.defs[self.di];
        let dl = crate::parser::unpack_pos(d.line).0;
        for q in &b.params {
            self.declare(&q.name, 0, &q.ty, dl, true);
        }
        self.eval(&b.body, Cx::Value);
        self.close_scope(0);
    }
}

// ---------------------------------------------------------------- 補助

fn flatten_local(p: Option<&Place>) -> Option<Place> {
    let mut cur = p?;
    loop {
        match cur {
            Place::Local(..) => return Some(cur.clone()),
            Place::Field(b, _) => cur = b,
            _ => return None,
        }
    }
}

fn fields_of(p: &Place) -> Vec<String> {
    match p {
        Place::Field(b, f) => {
            let mut v = fields_of(b);
            v.push(f.clone());
            v
        }
        _ => vec![],
    }
}

fn subst(t: &Ty, bind: &HashMap<String, Ty>) -> Ty {
    match t {
        Ty::Param(p) => bind.get(p).cloned().unwrap_or(Ty::Unknown),
        Ty::Named(n, a, r) => Ty::Named(n.clone(), a.iter().map(|x| subst(x, bind)).collect(), r.clone()),
        Ty::Ref(m, x, r) => Ty::Ref(*m, Box::new(subst(x, bind)), r.clone()),
        Ty::Ptr(m, x) => Ty::Ptr(*m, Box::new(subst(x, bind))),
        Ty::Tuple(xs) => Ty::Tuple(xs.iter().map(|x| subst(x, bind)).collect()),
        Ty::Array(x, n) => Ty::Array(Box::new(subst(x, bind)), *n),
        Ty::Slice(x) => Ty::Slice(Box::new(subst(x, bind))),
        other => other.clone(),
    }
}


fn vs(v: &V) -> String {
    match v {
        V::S(s) | V::N(s) => s.clone(),
        _ => String::new(),
    }
}
