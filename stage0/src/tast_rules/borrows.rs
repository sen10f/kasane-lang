//! 規則 B (借用の重なり、仕様 4.1 / 4.2、E3010): 場所 (`Place`)・型 (領域つき)・変換の印 (`AutoRef` / `Reborrow`) だけを見て、
//! 借用が生きている間の競合するアクセスを E3010 にする。式の形ごとの場合分けは「値の持ち主を結ぶ」ところ (`Walker::expr`) に 1 回だけ置く。
//!
//! 作り (設計_型付き検査.md 9.4): 本体を制御フローグラフ (基本ブロックの事象列) に落とす。
//! - 借用 `&p` / `&mut p`・メソッドの自動参照・参照の自動再借用・閉包の取り込みが**貸し出し** (loan: 場所と可変か) を作る。
//! - 値を受ける変数 (と式の途中の一時の持ち主 `$e`) が、その値の型が領域を持つとき、貸し出しの集合を持つ (前向きの流れ)。
//!   呼び出しの結果は、署名の領域・型パラメータが戻り値へ運ぶ引数の貸し出しだけを持つ (`SideTabs::regions.sigs`)。型に領域が無い結果
//!   (`Copy` の値を借用から取り出した形) は何も持たない。
//! - 持ち主が**この後使われる** (後ろ向きの生存) 間、その貸し出しの場所への競合するアクセスが E3010。
//!   「使われる最後の所まで」を求める最小の形で、S3D の領域の制約を解く部分が入ったらここを置き換えられる (`analyze` の生存の部分)。

use std::collections::{BTreeSet, HashMap};

use crate::check::Diag;
use crate::program::Program;
use crate::tast::*;
use crate::types::{from_v, Ty, TypeCk};

type Path = Vec<String>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Acc {
    Read,
    Write,
    MutB,
    SharedB,
    Move,
}

#[derive(Clone, Debug)]
enum Src {
    Loan(usize),
    Var(String),
}

#[derive(Clone, Debug)]
enum Ev {
    Use(String),
    /// ロックのガードを含む値の持ち主が生まれた (落とされるまでロックを持つ)。
    Hold(String),
    /// その持ち主が移動・`drop`・スコープの終わりで落とされた。
    Release(String),
    Bind { var: String, srcs: Vec<Src>, replace: bool },
    Acc { root: String, path: Path, kind: Acc, line: usize },
    /// 配列の定数の添字の要素への強い更新 (`core.mem.replace(&mut a[0], ..)`): 要素が持っていた貸し出しが終わる。
    Kill { var: String, ids: Vec<usize> },
    /// `Sender.send(v)`: `v` の持つ貸し出しは、同じチャネルの他の持ち主 (主に `Receiver`) へも流れる。
    ChanSend { from: String, srcs: Vec<Src> },
    /// 中断点 (`uses [suspend]` の呼び出し・クロージャや `Fn` 境界越しの呼び出しを含む、5.6)。
    /// ここで生きている借用は中断をまたぐので E3050。
    Suspend { line: usize },
}

struct Loan {
    root: String,
    path: Path,
    mutable: bool,
}

struct Cfg {
    evs: Vec<Vec<Ev>>,
    succ: Vec<Vec<usize>>,
    loans: Vec<Loan>,
    cur: usize,
    /// (継続の行き先, 脱出の行き先)
    loops: Vec<(usize, usize)>,
    exit: usize,
}

impl Cfg {
    fn new() -> Cfg {
        let mut c = Cfg { evs: vec![], succ: vec![], loans: vec![], cur: 0, loops: vec![], exit: 0 };
        c.cur = c.node();
        c.exit = c.node();
        c
    }
    fn node(&mut self) -> usize {
        self.evs.push(vec![]);
        self.succ.push(vec![]);
        self.evs.len() - 1
    }
    fn edge(&mut self, a: usize, b: usize) {
        if !self.succ[a].contains(&b) {
            self.succ[a].push(b);
        }
    }
    fn emit(&mut self, e: Ev) {
        let c = self.cur;
        self.evs[c].push(e);
    }
    fn loan(&mut self, root: String, path: Path, mutable: bool) -> usize {
        self.loans.push(Loan { root, path, mutable });
        self.loans.len() - 1
    }
}

pub fn check(p: &Program, user: &[usize], tast: &Tast, tk: &TypeCk) -> Vec<Diag> {
    let mut out = vec![];
    // susp_lines は (定義, Pack した位置) なので行だけにほどく。
    let susp: std::collections::HashSet<(usize, usize)> =
        tk.susp_lines.iter().map(|(d, l)| (*d, crate::parser::unpack_pos(*l).0)).collect();
    for &di in user {
        let Some(b) = tast.bodies.get(&di) else { continue };
        let mut w = Walker { p, tk, susp: &susp, di, scopes: vec![HashMap::new()], cfg: Cfg::new(), tmp: 0, diags: vec![], susp_diags: vec![], guards: BTreeSet::new(), slots: HashMap::new(), slot_unsafe: BTreeSet::new(), last_array: None, slot_ok: None };
        for q in &b.params {
            w.scopes[0].insert(q.name.clone(), (0, q.ty.clone()));
            if has_guard(p, &q.ty) {
                w.guards.insert(key(&q.name, 0));
                w.cfg.emit(Ev::Hold(key(&q.name, 0)));
            }
        }
        w.expr(&b.body);
        let mut cfg = std::mem::replace(&mut w.cfg, Cfg::new());
        for evs in cfg.evs.iter_mut() {
            evs.retain(|e| !matches!(e, Ev::Kill { var, .. } if w.slot_unsafe.contains(var)));
        }
        w.analyze(cfg);
        let d = &p.defs[di];
        let mut seen = std::collections::BTreeSet::new();
        for (line, msg) in w.diags {
            if seen.insert((line, msg.clone())) {
                out.push(Diag { file: d.file.clone(), code: "E3010".into(), fqn: d.name.clone(), line, col: 0, msg });
            }
        }
        for (line, msg) in w.susp_diags {
            if seen.insert((line, msg.clone())) {
                out.push(Diag { file: d.file.clone(), code: "E3050".into(), fqn: d.name.clone(), line, col: 0, msg });
            }
        }
    }
    out
}

// ------------------------------------------------------------ 型だけを見る判定

/// 値が借用を運び得る型か (領域を持つ参照・ライフタイム引数・`dyn`・閉包・欄に参照を持つ利用者の型)。
fn holds(p: &Program, t: &Ty) -> bool {
    holds2(p, t, false)
}

/// 式の値・呼び出しの戻り値が借用を運び得るか。`&'static` はどの局部も借りないので運ばない
/// (`Arena::insert` の `Handle<&'static str>` が `Arena` の借り出しを運ぶと見なさない)。
/// 欄や場所の側は宣言が `&'static` でも実際の借用を持ち得るので `holds` の方を呼ぶ。
fn holds_val(p: &Program, t: &Ty) -> bool {
    holds2(p, t, true)
}

fn holds2(p: &Program, t: &Ty, static_is_no_borrow: bool) -> bool {
    match t {
        Ty::Ref(..) => !(static_is_no_borrow && matches!(t, Ty::Ref(_, _, r) if matches!(r, crate::types::Region::Static))),
        Ty::Dyn(..) => true,
        // 型注釈から組んだ関数型 (`id` なし。`core.ffi.trampoline` の戻りの関数ポインタなど) は何も取り込めない。
        Ty::Closure { id, .. } => id.is_some(),
        Ty::Named(n, a, rg) => {
            !rg.0.is_empty() || a.iter().any(|x| holds2(p, x, static_is_no_borrow)) || p.types.get(n).map_or(false, |&i| !p.defs[i].field_lifetimes.iter().all(|(_, l)| l.iter().all(|x| x == "static")))
        }
        Ty::Tuple(a) => a.iter().any(|x| holds2(p, x, static_is_no_borrow)),
        Ty::Array(x, _) | Ty::Slice(x) => holds2(p, x, static_is_no_borrow),
        _ => false,
    }
}

/// 型の中に参照があるか。
fn has_ref(t: &Ty) -> bool {
    match t {
        Ty::Ref(..) => true,
        Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().any(has_ref),
        Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => has_ref(x),
        Ty::Closure { args, ret, .. } => args.iter().any(has_ref) || has_ref(ret),
        _ => false,
    }
}

/// 参照の欄を持つ利用者の型も値は借用を運び得る (`t.H { r: &x }` は `x` の借用を持つ)。
fn holds_field(p: &Program, t: &Ty) -> bool {
    if holds(p, t) {
        return true;
    }
    if let Ty::Named(n, _, _) = t {
        if let Some(&i) = p.types.get(n) {
            let gn = p.defs[i].gnames();
            return p.defs[i].fields.iter().any(|f| has_ref(&from_v(&f.1, &gn)));
        }
    }
    false
}

const GUARD_TYPES: [&str; 3] = ["std.sync.MutexGuard", "std.sync.RwLockReadGuard", "std.sync.RwLockWriteGuard"];

/// 型がロックのガードを (欄・タプル・`Option` などの中に) 所有するか。参照 `&Guard` は所有しない。
fn has_guard(p: &Program, t: &Ty) -> bool {
    fn go(p: &Program, t: &Ty, seen: &mut Vec<String>) -> bool {
        match t {
            Ty::Tuple(a) => a.iter().any(|x| go(p, x, seen)),
            Ty::Array(x, _) | Ty::Slice(x) => go(p, x, seen),
            Ty::Named(n, a, _) => {
                if GUARD_TYPES.contains(&n.as_str()) {
                    return true;
                }
                if a.iter().any(|x| go(p, x, seen)) {
                    return true;
                }
                if seen.contains(n) {
                    return false;
                }
                seen.push(n.clone());
                match p.types.get(n) {
                    Some(&i) => {
                        let gn = p.defs[i].gnames();
                        p.defs[i].fields.iter().any(|f| go(p, &from_v(&f.1, &gn), seen))
                    }
                    None => false,
                }
            }
            _ => false,
        }
    }
    go(p, t, &mut vec![])
}

/// 値として使うと移動する型 (確実にそうと分かるものだけ。分からない型は移動にしない)。
fn not_copy(p: &Program, t: &Ty) -> bool {
    if has_guard(p, t) {
        return true;
    }
    match t {
        Ty::Ref(m, ..) => *m,
        Ty::Tuple(a) => a.iter().any(|x| not_copy(p, x)),
        Ty::Array(x, _) => not_copy(p, x),
        Ty::Named(n, a, _) => {
            if n.ends_with("Option") || n.ends_with("Result") {
                return a.iter().any(|x| not_copy(p, x));
            }
            match p.types.get(n) {
                Some(&i) if p.defs[i].derive.iter().any(|x| x == "Copy") => a.iter().any(|x| not_copy(p, x)),
                Some(_) => true,
                None => false,
            }
        }
        _ => false,
    }
}

fn param_names(t: &Ty, out: &mut BTreeSet<String>) {
    match t {
        Ty::Param(n) => {
            out.insert(n.clone());
        }
        Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().for_each(|x| param_names(x, out)),
        Ty::Ref(_, x, _) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) => param_names(x, out),
        Ty::Closure { args, ret, .. } => {
            args.iter().for_each(|x| param_names(x, out));
            param_names(ret, out);
        }
        _ => {}
    }
}

fn overlaps(a: &[String], b: &[String]) -> bool {
    let n = a.len().min(b.len());
    a[..n] == b[..n]
}

fn key(name: &str, id: u32) -> String {
    format!("{}@{}", name, id)
}

fn show_var(k: &str) -> String {
    k.split('@').next().unwrap_or(k).to_string()
}

fn show_place(root: &str, path: &[String]) -> String {
    let mut s = show_var(root);
    for x in path {
        match x.as_str() {
            "*" => s = format!("*{}", s),
            "[]" => s.push_str("[..]"),
            f => {
                s.push('.');
                s.push_str(f);
            }
        }
    }
    s
}

// ------------------------------------------------------------ 本体の走査

struct Walker<'a> {
    p: &'a Program,
    tk: &'a TypeCk<'a>,
    /// 型検査が中断点と決めた呼び出し (定義, 行)。(クロージャ・`Fn` 境界越しの呼び出しを含む)
    susp: &'a std::collections::HashSet<(usize, usize)>,
    di: DefId,
    /// 名前 → (束縛の ID, 型)。
    scopes: Vec<HashMap<String, (u32, Ty)>>,
    cfg: Cfg,
    tmp: usize,
    diags: Vec<(usize, String)>,
    /// 中断点をまたぐ借用 (E3050)。コードが違うので分けて出す。
    susp_diags: Vec<(usize, String)>,
    /// ロックのガード (`MutexGuard` など) を持つ束縛の持ち主。中断点で生きていれば E3050。
    guards: BTreeSet<String>,
    /// 配列リテラルで作った変数 → 要素ごとの、その要素だけが持つ貸し出し (`[&mut x, &mut z]` なら `[[x], [z]]`)。
    slots: HashMap<String, Vec<Vec<usize>>>,
    /// 要素の持ち主が崩れ得る変数 (定数添字の `replace` 以外の可変借用・代入・流入があったもの)。`Kill` を捨てる。
    slot_unsafe: BTreeSet<String>,
    /// 直近に評価した配列リテラルの、要素ごとの貸し出し。
    last_array: Option<Vec<Vec<usize>>>,
    /// `replace` の第 1 引数の評価中だけ、この根への `&mut a[i]` を可変借用として数えない。
    slot_ok: Option<String>,
}

impl<'a> Walker<'a> {
    fn lookup(&self, n: &str) -> Option<(u32, &Ty)> {
        self.scopes.iter().rev().find_map(|s| s.get(n)).map(|(i, t)| (*i, t))
    }

    fn fresh(&mut self) -> String {
        self.tmp += 1;
        format!("$e{}", self.tmp)
    }

    /// 場所 `e` を (根の変数, 道) で。値の式・局所でない根は `None`。
    fn pl(&self, e: &TExpr) -> Option<(String, Path)> {
        match &e.kind {
            TKind::Path(segs) => {
                let (id, ty) = self.lookup(&segs[0])?;
                let mut path = vec![];
                if segs.len() > 1 && matches!(ty, Ty::Ref(..)) {
                    path.push("*".to_string());
                }
                path.extend(segs[1..].iter().cloned());
                Some((key(&segs[0], id), path))
            }
            TKind::Field { base, name } => {
                let (r, mut p) = self.pl(base)?;
                if matches!(base.ty, Ty::Ref(..)) {
                    p.push("*".into());
                }
                p.push(name.clone());
                Some((r, p))
            }
            TKind::Index { base, .. } => {
                let (r, mut p) = self.pl(base)?;
                if matches!(base.ty, Ty::Ref(..)) {
                    p.push("*".into());
                }
                p.push("[]".into());
                Some((r, p))
            }
            TKind::Unary { op, operand } if op == "*" => {
                let (r, mut p) = self.pl(operand)?;
                p.push("*".into());
                Some((r, p))
            }
            _ => None,
        }
    }

    /// 場所の式の副作用 (添字の評価・根の使用) を出して、場所を返す。場所でなければ式を値として評価して `None`。
    fn place(&mut self, e: &TExpr) -> Option<(String, Path)> {
        let pl = self.pl(e);
        match (&e.kind, &pl) {
            (_, Some((root, _))) => {
                self.cfg.emit(Ev::Use(root.clone()));
                self.index_effects(e);
                pl
            }
            _ => {
                // 根が局所でない場所の式: 子の値だけを評価する (自分を `expr` に戻すと `read_place` と循環する)。
                match &e.kind {
                    TKind::Path(_) => {}
                    TKind::Field { base, .. } => {
                        self.expr(base);
                    }
                    TKind::Index { base, index } => {
                        self.expr(base);
                        self.expr(index);
                    }
                    TKind::Unary { op, operand } if op == "*" => {
                        self.expr(operand);
                    }
                    _ => {
                        self.expr(e);
                    }
                }
                None
            }
        }
    }

    fn index_effects(&mut self, e: &TExpr) {
        match &e.kind {
            TKind::Index { base, index } => {
                self.index_effects(base);
                self.expr(index);
            }
            TKind::Field { base, .. } => self.index_effects(base),
            TKind::Unary { operand, .. } => self.index_effects(operand),
            _ => {}
        }
    }

    fn pop_scope(&mut self) {
        if let Some(sc) = self.scopes.pop() {
            let mut ks: Vec<String> = sc.iter().map(|(n, (id, _))| key(n, *id)).filter(|k| self.guards.contains(k)).collect();
            ks.sort();
            for k in ks {
                self.cfg.emit(Ev::Release(k));
            }
        }
    }

    fn acc(&mut self, root: &str, path: &[String], kind: Acc, line: usize) {
        if kind == Acc::Move && path.is_empty() && self.guards.contains(root) {
            self.cfg.emit(Ev::Release(root.to_string()));
        }
        self.cfg.emit(Ev::Acc { root: root.to_string(), path: path.to_vec(), kind, line });
    }

    /// 場所から借りて、持たせる元 (貸し出し + 根が参照を通すなら根の持つ貸し出し) を返す。
    fn borrow(&mut self, root: &str, path: &[String], mutable: bool, line: usize) -> Vec<Src> {
        self.acc(root, path, if mutable { Acc::MutB } else { Acc::SharedB }, line);
        if mutable && self.slot_ok.as_deref() != Some(root) {
            self.slot_unsafe.insert(root.to_string());
        }
        let id = self.cfg.loan(root.to_string(), path.to_vec(), mutable);
        // 借りた先が持つ貸し出し (`&r`・`&*r`・`w.get()` の `w`) も、借用が生きている間は運ばれる。
        vec![Src::Loan(id), Src::Var(root.to_string())]
    }

    fn bind(&mut self, var: String, srcs: Vec<Src>, replace: bool) {
        self.slot_unsafe.insert(var.clone());
        self.cfg.emit(Ev::Bind { var, srcs, replace });
    }

    /// 値の位置の式を評価して、その値が持つ貸し出しの元を返す。
    fn expr(&mut self, e: &TExpr) -> Vec<Src> {
        match &e.kind {
            TKind::Lit(_) => vec![],
            TKind::Path(_) | TKind::Field { .. } | TKind::Index { .. } => self.read_place(e),
            TKind::Unary { op, operand } => match op.as_str() {
                "*" => self.read_place(e),
                "&" | "&mut" => {
                    let m = op == "&mut";
                    if self.pl(operand).is_some() {
                        match self.place(operand) {
                            Some((r, p)) => self.borrow(&r, &p, m, e.line),
                            None => vec![],
                        }
                    } else {
                        // 一時の値 (`&[&x]`・`&f(&x)`) を借りる: 一時が持つ貸し出しがそのまま運ばれる。
                        let s = self.expr(operand);
                        self.keep(&e.ty, s)
                    }
                }
                _ => {
                    self.expr(operand);
                    vec![]
                }
            },
            TKind::Binary { lhs, rhs, .. } => {
                self.operands(&[&**lhs, &**rhs]);
                vec![]
            }
            TKind::Call { callee, args } => self.call(e, Some(callee), None, args),
            TKind::MCall { recv, args, .. } => self.call(e, None, Some(recv), args),
            TKind::Block(stmts) => self.block(stmts, &e.ty),
            TKind::If { cond, then, els } => {
                self.expr(cond);
                let from = self.cfg.cur;
                let join = self.cfg.node();
                let t = self.cfg.node();
                self.cfg.edge(from, t);
                self.cfg.cur = t;
                let mut srcs = self.expr(then);
                let c = self.cfg.cur;
                self.cfg.edge(c, join);
                if let Some(x) = els {
                    let n = self.cfg.node();
                    self.cfg.edge(from, n);
                    self.cfg.cur = n;
                    srcs.extend(self.expr(x));
                    let c = self.cfg.cur;
                    self.cfg.edge(c, join);
                } else {
                    self.cfg.edge(from, join);
                }
                self.cfg.cur = join;
                self.keep(&e.ty, srcs)
            }
            TKind::Match { scrut, arms } => {
                let ssrc = match self.place_scrut(scrut, arms) {
                    Some(s) => s,
                    None => self.expr(scrut),
                };
                let from = self.cfg.cur;
                let join = self.cfg.node();
                let mut srcs = vec![];
                for a in arms {
                    let n = self.cfg.node();
                    self.cfg.edge(from, n);
                    self.cfg.cur = n;
                    self.scopes.push(HashMap::new());
                    self.bind_pat(&a.pat, &ssrc);
                    if let Some(g) = &a.guard {
                        self.expr(g);
                    }
                    srcs.extend(self.expr(&a.body));
                    self.pop_scope();
                    let c = self.cfg.cur;
                    self.cfg.edge(c, join);
                }
                if arms.is_empty() {
                    self.cfg.edge(from, join);
                }
                self.cfg.cur = join;
                self.keep(&e.ty, srcs)
            }
            TKind::Closure { params, captures, body, .. } => self.closure(e, params, captures, body),
            TKind::StructLit { fields, .. } => {
                let es: Vec<&TExpr> = fields.iter().map(|(_, x)| x).collect();
                let s = self.operands(&es);
                // 参照の欄がある構造体は、欄の宣言が `'static` でも実際に入った借用を持つ。
                if holds_field(self.p, &e.ty) {
                    s
                } else {
                    self.keep(&e.ty, s)
                }
            }
            TKind::Other { name, children } => self.other(e, name, children),
        }
    }

    /// 型が借用を運ばないなら元を捨てる (`Copy` の値を借用から取り出した形は重ならない)。
    fn keep(&self, ty: &Ty, srcs: Vec<Src>) -> Vec<Src> {
        if holds_val(self.p, ty) {
            srcs
        } else {
            vec![]
        }
    }

    /// `match` の対象が場所のとき: 読むだけ (束縛が移動するかは移動と線形の規則に任せる)。
    fn place_scrut(&mut self, e: &TExpr, arms: &[TArm]) -> Option<Vec<Src>> {
        let (r, p) = self.pl(e).map(|x| x)?;
        self.place(e);
        // 参照でない場所の中身を、所有する束縛が移動で取り出すなら移動。
        fn moves(p: &Program, q: &TPat) -> bool {
            let here = q.kind == "PBind" && q.info.as_ref().and_then(|i| i.bind.as_ref()).map_or(false, |b| b.2 == BindMode::Move && not_copy(p, &b.1));
            here || q.children.iter().any(|c| moves(p, c))
        }
        let mv = !matches!(e.ty, Ty::Ref(..)) && arms.iter().any(|a| moves(self.p, &a.pat));
        self.acc(&r, &p, if mv { Acc::Move } else { Acc::Read }, e.line);
        Some(self.keep(&e.ty, vec![Src::Var(r)]))
    }

    /// 場所を値として読む。
    fn read_place(&mut self, e: &TExpr) -> Vec<Src> {
        match self.place(e) {
            Some((r, p)) => {
                // 参照・添字の先からの取り出し (`*r`・`v[i]`) は移動ではない (移動と線形の規則が E3002 にする)。
                let through = p.iter().any(|x| x == "*" || x == "[]");
                let kind = if not_copy(self.p, &e.ty) && !through { Acc::Move } else { Acc::Read };
                self.acc(&r, &p, kind, e.line);
                self.keep(&e.ty, vec![Src::Var(r)])
            }
            None => vec![],
        }
    }

    /// 兄弟のオペランドを順に評価する。先のオペランドが持つ貸し出しは、最後のオペランドまで一時の持ち主 `$e` で生かす。
    fn operands(&mut self, es: &[&TExpr]) -> Vec<Src> {
        let mut all = vec![];
        let mut temps = vec![];
        let mut guard_temps = vec![];
        let n = es.len();
        for (i, x) in es.iter().enumerate() {
            let s = self.expr(x);
            if i + 1 < n && has_guard(self.p, &x.ty) {
                let t = self.fresh();
                self.guards.insert(t.clone());
                self.cfg.emit(Ev::Hold(t.clone()));
                guard_temps.push(t);
            }
            if !s.is_empty() {
                all.extend(s.iter().cloned());
                if i + 1 < n {
                    let t = self.fresh();
                    self.bind(t.clone(), s, true);
                    temps.push(t);
                }
            }
        }
        for t in temps {
            self.cfg.emit(Ev::Use(t));
        }
        for t in guard_temps {
            self.cfg.emit(Ev::Release(t));
        }
        all
    }

    fn block(&mut self, stmts: &[TStmt], ty: &Ty) -> Vec<Src> {
        self.scopes.push(HashMap::new());
        let mut tail = vec![];
        let n = stmts.len();
        for (i, s) in stmts.iter().enumerate() {
            match s {
                TStmt::Let { pat, init } => {
                    self.last_array = None;
                    let srcs = match init {
                        Some(x) => self.expr(x),
                        None => vec![],
                    };
                    let arr = if matches!(init, Some(TExpr { kind: TKind::Other { name: "ArrayE", .. }, .. })) { self.last_array.take() } else { None };
                    let before: BTreeSet<String> = self.slot_unsafe.clone();
                    self.bind_pat(pat, &srcs);
                    if let (Some(a), true, Some(n)) = (arr, pat.kind == "PBind", &pat.name) {
                        let k = key(n, pat.id);
                        if !before.contains(&k) {
                            self.slot_unsafe.remove(&k);
                        }
                        self.slots.insert(k, a);
                    }
                }
                TStmt::Expr(x) => {
                    let s = self.expr(x);
                    if i + 1 == n {
                        tail = s;
                    }
                }
                TStmt::Assign { place, value } => self.assign(place, value),
            }
        }
        self.pop_scope();
        self.keep(ty, tail)
    }

    fn bind_pat(&mut self, q: &TPat, srcs: &[Src]) {
        if q.kind == "PBind" {
            if let (Some(n), Some(info)) = (&q.name, &q.info) {
                let ty = info.bind.as_ref().map(|b| b.1.clone()).unwrap_or_else(|| info.ty.clone());
                self.scopes.last_mut().unwrap().insert(n.clone(), (q.id, ty.clone()));
                let k = key(n, q.id);
                if has_guard(self.p, &ty) {
                    self.guards.insert(k.clone());
                    self.cfg.emit(Ev::Hold(k.clone()));
                }
                let s = if holds_field(self.p, &ty) { srcs.to_vec() } else { vec![] };
                self.bind(k, s, true);
            } else if let Some(n) = &q.name {
                self.scopes.last_mut().unwrap().insert(n.clone(), (q.id, Ty::Unknown));
                self.bind(key(n, q.id), vec![], true);
            }
        }
        for c in &q.children {
            self.bind_pat(c, srcs);
        }
    }

    fn assign(&mut self, place: &TExpr, value: &TExpr) {
        let srcs = self.expr(value);
        // 変数そのものへの代入 (`r = &x`) は `r` を読まない。
        let whole = matches!(self.pl(place), Some((_, p)) if p.is_empty());
        let Some((r, p)) = (if whole { self.pl(place) } else { self.place(place) }) else { return };
        self.acc(&r, &p, Acc::Write, place.line);
        let srcs = self.keep(&place.ty, srcs);
        // 定数の添字の要素への代入 (`a[0] = ..`・`a` は配列リテラルで作った局所で、要素の持ち主が崩れていない): その要素の貸し出しだけを入れ替える (強い更新)。
        if let (1, Some(idx)) = (p.len(), self.elem_slot(place, &r)) {
            let old = self.slots.get(&r).and_then(|v| v.get(idx)).cloned().unwrap_or_default();
            if !old.is_empty() {
                self.cfg.emit(Ev::Kill { var: r.clone(), ids: old });
            }
            let new_ids: Vec<usize> = srcs.iter().filter_map(|s| if let Src::Loan(i) = s { Some(*i) } else { None }).collect();
            if !srcs.is_empty() {
                // `bind` を通さない: 要素の持ち主の追跡 (`slots`) を崩さない。
                self.cfg.emit(Ev::Bind { var: r.clone(), srcs, replace: false });
            }
            if let Some(v) = self.slots.get_mut(&r) {
                v[idx] = new_ids;
            }
            return;
        }
        // 添字が定数でない・追跡できない配列への代入は弱い更新 (どの要素か分からないので、元の貸し出しは残す)。
        if p.is_empty() {
            self.bind(r, srcs, true);
        } else if !srcs.is_empty() {
            self.bind(r, srcs, false);
        }
    }

    fn closure(&mut self, e: &TExpr, params: &[TBinding], captures: &[Capture], body: &TExpr) -> Vec<Src> {
        let mut srcs = vec![];
        // 参照の取り込みごとの (束縛の鍵, 型が借用を持てるか, 作った貸し出し)。
        let mut by_ref: Vec<(String, bool, Vec<Src>)> = vec![];
        for c in captures {
            let Some((id, _)) = self.lookup(&c.name) else { continue };
            let root = key(&c.name, id);
            self.cfg.emit(Ev::Use(root.clone()));
            match c.kind.as_str() {
                "&" | "&mut" => {
                    let b = self.borrow(&root, &[], c.kind == "&mut", e.line);
                    srcs.extend(b.iter().cloned());
                    by_ref.push((root.clone(), c.kind == "&mut" && holds(self.p, &c.ty), b));
                }
                _ => {
                    let k = if not_copy(self.p, &c.ty) { Acc::Move } else { Acc::Read };
                    self.acc(&root, &[], k, e.line);
                    if holds(self.p, &c.ty) {
                        srcs.push(Src::Var(root));
                    }
                }
            }
        }
        // 取り込みごとの領域は外側の場所の領域。本体は取り込み A から得た借用を、`&mut` で取り込んだ B の先へ書き得る (M7): 外側で
        // `B = ..A の借用..` と書いたのと同じに、B が A の貸し出し (と A が持つ貸し出し) を持つとして扱う。
        for (bi, (broot, bholds, _)) in by_ref.iter().enumerate() {
            if !*bholds {
                continue;
            }
            let mut add: Vec<Src> = vec![];
            for (ai, (aroot, _, aloans)) in by_ref.iter().enumerate() {
                if ai != bi {
                    add.extend(aloans.iter().cloned());
                    add.push(Src::Var(aroot.clone()));
                }
            }
            if !add.is_empty() {
                self.bind(broot.clone(), add, false);
            }
        }
        // 本体は別のグラフで検査する (外側の束縛は見える)。
        let outer = std::mem::replace(&mut self.cfg, Cfg::new());
        self.scopes.push(HashMap::new());
        for q in params {
            self.scopes.last_mut().unwrap().insert(q.name.clone(), (0, q.ty.clone()));
        }
        // 共有の取り込み `&x` は、本体の間ずっと `x` を借りている (本体の中の `x` への書き込みは E3010)。
        let mut caps = vec![];
        for c in captures.iter().filter(|c| c.kind == "&") {
            if let Some((id, _)) = self.lookup(&c.name) {
                let r = key(&c.name, id);
                caps.push(Src::Loan(self.cfg.loan(r, vec![], false)));
            }
        }
        let had_caps = !caps.is_empty();
        if had_caps {
            self.bind("$cap".into(), caps, true);
        }
        self.expr(body);
        if had_caps {
            let (c, ex) = (self.cfg.cur, self.cfg.exit);
            self.cfg.edge(c, ex);
            self.cfg.evs[ex].push(Ev::Use("$cap".into()));
        }
        self.pop_scope();
        let inner = std::mem::replace(&mut self.cfg, outer);
        self.analyze(inner);
        srcs
    }

    fn other(&mut self, e: &TExpr, name: &str, ch: &[TExpr]) -> Vec<Src> {
        match name {
            "While" => {
                let head = self.cfg.node();
                let body = self.cfg.node();
                let exit = self.cfg.node();
                let c = self.cfg.cur;
                self.cfg.edge(c, head);
                self.cfg.cur = head;
                if let Some(x) = ch.first() {
                    self.expr(x);
                }
                self.cfg.edge(head, body);
                self.cfg.edge(head, exit);
                self.cfg.cur = body;
                self.cfg.loops.push((head, exit));
                for x in ch.iter().skip(1) {
                    self.expr(x);
                }
                self.cfg.loops.pop();
                let c = self.cfg.cur;
                self.cfg.edge(c, head);
                self.cfg.cur = exit;
                vec![]
            }
            "Loop" => {
                let body = self.cfg.node();
                let exit = self.cfg.node();
                let c = self.cfg.cur;
                self.cfg.edge(c, body);
                self.cfg.cur = body;
                self.cfg.loops.push((body, exit));
                for x in ch {
                    self.expr(x);
                }
                self.cfg.loops.pop();
                let c = self.cfg.cur;
                self.cfg.edge(c, body);
                self.cfg.cur = exit;
                vec![]
            }
            "For" => {
                // 反復子は繰り返しの間ずっと生きる (`$iter`)。束縛は反復子の持つ貸し出しを持つ。
                let srcs = ch.first().map(|x| self.expr(x)).unwrap_or_default();
                let it = format!("$iter{}", self.tmp);
                self.tmp += 1;
                self.bind(it.clone(), srcs.clone(), true);
                let head = self.cfg.node();
                let body = self.cfg.node();
                let exit = self.cfg.node();
                let c = self.cfg.cur;
                self.cfg.edge(c, head);
                self.cfg.cur = head;
                self.cfg.emit(Ev::Use(it.clone()));
                self.cfg.edge(head, body);
                self.cfg.edge(head, exit);
                self.cfg.cur = body;
                self.cfg.loops.push((head, exit));
                // ループ変数は反復子の持つ貸し出しを持つ (毎回の反復で束縛し直す)。
                self.scopes.push(HashMap::new());
                for q in &e.pats {
                    self.bind_pat(q, &srcs);
                }
                for x in ch.iter().skip(1) {
                    self.expr(x);
                }
                self.pop_scope();
                self.cfg.loops.pop();
                let c = self.cfg.cur;
                self.cfg.edge(c, head);
                self.cfg.cur = exit;
                vec![]
            }
            "With" => {
                // 束縛は普通の局所 (`let`) と同じ。子は [初期化, (後始末), 本体]、実行順は 初期化 → 本体 → 後始末。
                let srcs = ch.first().map(|x| self.expr(x)).unwrap_or_default();
                self.scopes.push(HashMap::new());
                for q in &e.pats {
                    self.bind_pat(q, &srcs);
                }
                let tail = ch.last().filter(|_| ch.len() > 1).map(|x| self.expr(x)).unwrap_or_default();
                if ch.len() > 2 {
                    self.expr(&ch[1]);
                }
                self.pop_scope();
                tail
            }
            "Break" | "Continue" => {
                for x in ch {
                    self.expr(x);
                }
                if let Some(&(cont, brk)) = self.cfg.loops.last() {
                    let c = self.cfg.cur;
                    self.cfg.edge(c, if name == "Break" { brk } else { cont });
                }
                let n = self.cfg.node();
                self.cfg.cur = n;
                vec![]
            }
            "Return" => {
                for x in ch {
                    self.expr(x);
                }
                let (c, ex) = (self.cfg.cur, self.cfg.exit);
                self.cfg.edge(c, ex);
                let n = self.cfg.node();
                self.cfg.cur = n;
                vec![]
            }
            "Try" => {
                let s = ch.first().map(|x| self.expr(x)).unwrap_or_default();
                let (c, ex) = (self.cfg.cur, self.cfg.exit);
                self.cfg.edge(c, ex);
                let n = self.cfg.node();
                self.cfg.edge(c, n);
                self.cfg.cur = n;
                self.keep(&e.ty, s)
            }
            "ArrayE" => {
                // `operands` と同じ評価の順で、要素ごとの元を取る。要素が借用式 (`&x` / `&mut x`) そのものなら、その貸し出しは要素だけのもの。
                let mut all = vec![];
                let mut temps = vec![];
                let mut per = vec![];
                let n = ch.len();
                for (i, x) in ch.iter().enumerate() {
                    let s = self.expr(x);
                    let direct = matches!(&x.kind, TKind::Unary { op, operand } if (op == "&" || op == "&mut") && self.pl(operand).is_some());
                    per.push(if direct { s.iter().filter_map(|y| if let Src::Loan(i) = y { Some(*i) } else { None }).collect() } else { vec![] });
                    if !s.is_empty() {
                        all.extend(s.iter().cloned());
                        if i + 1 < n {
                            let t = self.fresh();
                            self.bind(t.clone(), s, true);
                            temps.push(t);
                        }
                    }
                }
                for t in temps {
                    self.cfg.emit(Ev::Use(t));
                }
                self.last_array = Some(per);
                self.keep(&e.ty, all)
            }
            _ => {
                let es: Vec<&TExpr> = ch.iter().collect();
                let s = self.operands(&es);
                self.keep(&e.ty, s)
            }
        }
    }

    // -------------------------------------------------------- 呼び出し

    fn call(&mut self, e: &TExpr, callee: Option<&TExpr>, recv: Option<&TExpr>, args: &[TExpr]) -> Vec<Src> {
        let cands: Vec<DefId> = match &e.res {
            Some(Callee::Def(i)) => vec![*i],
            Some(Callee::Methods(v)) => v.clone(),
            _ => vec![],
        };
        let def = if cands.len() == 1 { Some(cands[0]) } else { None };
        let gn = def.map(|i| self.p.defs[i].gnames()).unwrap_or_default();
        let mut dparams: Vec<Ty> = def.map(|i| self.p.defs[i].params.iter().map(|(_, t)| from_v(t, &gn)).collect()).unwrap_or_default();
        let is_method = !cands.is_empty() && cands.iter().all(|&i| self.p.defs[i].is_method());
        if def.is_none() && is_method {
            // 候補が複数: 受け手の宣言は、全員が参照ならそれ (1 つでも可変なら可変)、そろわなければ不明。
            let sh: Vec<Ty> = cands.iter().map(|&i| from_v(&self.p.defs[i].params[0].1, &self.p.defs[i].gnames())).collect();
            if sh.iter().all(|t| matches!(t, Ty::Ref(..))) {
                let m = sh.iter().any(|t| matches!(t, Ty::Ref(true, ..)));
                dparams = vec![Ty::Ref(m, Box::new(Ty::Unknown), crate::types::Region::Unk)];
            }
        }
        // 受け手 (メソッド形 `x.m(..)` と経路形 `x.m(..)` の局所から始まる経路)。
        let mut local_callee: Option<String> = None;
        let mut recv_place: Option<(Option<(String, Path)>, Ty, Option<&TExpr>)> = None;
        if let Some(r) = recv {
            recv_place = Some((self.pl(r), r.ty.clone(), Some(r)));
        } else if let Some(c) = callee {
            if let TKind::Path(segs) = &c.kind {
                if segs.len() >= 2 && is_method {
                    if let Some((id, ty)) = self.lookup(&segs[0]) {
                        let mut path = vec![];
                        if matches!(ty, Ty::Ref(..)) && segs.len() > 2 {
                            path.push("*".to_string());
                        }
                        path.extend(segs[1..segs.len() - 1].iter().cloned());
                        recv_place = Some((Some((key(&segs[0], id), path)), c.ty.clone(), None));
                    }
                }
            } else if let TKind::Path(_) = &c.kind {
            }
            if let TKind::Path(segs) = &c.kind {
                if segs.len() == 1 && matches!(e.res, Some(Callee::Local(_)) | None) {
                    if let Some((id, _)) = self.lookup(&segs[0]) {
                        self.cfg.emit(Ev::Use(key(&segs[0], id)));
                        local_callee = Some(key(&segs[0], id));
                    }
                }
            }
        }
        let mut autoref = std::iter::once(&e.conv).chain(recv.map(|r| &r.conv)).flatten().find_map(|c| if let Coercion::AutoRef(m) = c { Some(*m) } else { None });
        // 印が無くても、宣言の `self` が参照で受け手が参照でない (`Box<T>` 越し・基本型) なら自動参照。
        if autoref.is_none() {
            if let (Some((_, rty, _)), Some(Ty::Ref(m, ..))) = (&recv_place, dparams.first()) {
                if !matches!(rty, Ty::Ref(..)) {
                    autoref = Some(*m);
                }
            }
        }
        let mut srcs_per: Vec<Vec<Src>> = vec![];
        let mut tys: Vec<Ty> = vec![];
        let mut temps = vec![];
        let mut call_guard_temps: Vec<String> = vec![];
        let mut idx = 0usize;
        // 受け手の可変借用は 2 段階借用 (`db.add(user.id)`、`v.push(v.len())`): 引数の評価が終わってから借りる。
        let has_recv = recv_place.is_some();
        let recv_root = recv_place.as_ref().and_then(|(pl, _, _)| pl.as_ref().map(|(r, _)| r.clone()));
        let def_name = def.map(|i| self.p.defs[i].name.clone()).unwrap_or_default();
        let _ = &local_callee;
        let mut late_recv = None;
        if let Some((pl, ty, ex)) = recv_place.take() {
            let decl = dparams.first().cloned();
            if autoref == Some(true) || matches!(ty, Ty::Ref(true, ..)) {
                srcs_per.push(vec![]);
                tys.push(ty.clone());
                late_recv = Some((pl, ty, decl, ex));
            } else {
                let s = self.receiver(pl, &ty, autoref, decl.as_ref(), ex, e.line);
                srcs_per.push(s);
                tys.push(ty);
            }
            idx += 1;
        }
        let first_arg = idx;
        // 参照の変数をそのまま渡す引数 (暗黙の再借用) は、ほかの引数の評価が終わってから借りる:
        // `f(cx, g(cx))` の入れ子の呼び出しは、外側の再借用が呼び出しの直前に起きるものとして競合させない。
        let deferred: Vec<bool> = args.iter().map(|a| matches!(a.ty, Ty::Ref(true, ..)) && self.pl(a).is_some()).collect();
        let order: Vec<usize> = (0..args.len()).filter(|j| !deferred[*j]).chain((0..args.len()).filter(|j| deferred[*j])).collect();
        let mut got: Vec<Option<Vec<Src>>> = vec![None; args.len()];
        let slot_target = if recv.is_none() && def.map_or(false, |i| self.p.defs[i].name == "core.mem.replace") { args.first().and_then(|a| self.const_slot(a)) } else { None };
        for (pos, &j) in order.iter().enumerate() {
            let a = &args[j];
            let pt = dparams.get(first_arg + j).cloned();
            if j == 0 {
                self.slot_ok = slot_target.as_ref().map(|t| t.0.clone());
            }
            let s = self.arg(a, pt.as_ref());
            self.slot_ok = None;
            if pos + 1 < order.len() && has_guard(self.p, &a.ty) {
                let t = self.fresh();
                self.guards.insert(t.clone());
                self.cfg.emit(Ev::Hold(t.clone()));
                call_guard_temps.push(t);
            }
            if !s.is_empty() && pos + 1 < order.len() {
                let t = self.fresh();
                self.bind(t.clone(), s.clone(), true);
                temps.push(t);
            }
            got[j] = Some(s);
        }
        for (j, a) in args.iter().enumerate() {
            srcs_per.push(got[j].take().unwrap_or_default());
            tys.push(a.ty.clone());
            idx += 1;
        }
        // 同じ呼び出しの中で、先の引数が `&mut` の変数そのものを使い、後の引数がそれを可変に再借用するのは競合。
        for j in first_arg..srcs_per.len() {
            let mut mroots: Vec<String> = vec![];
            for s in &srcs_per[j] {
                if let Src::Loan(i) = s {
                    let l = &self.cfg.loans[*i];
                    if l.mutable && l.path.first().map_or(false, |x| x == "*") {
                        mroots.push(l.root.clone());
                    }
                }
            }
            for r in mroots {
                let hit = (first_arg..j).any(|k| {
                    srcs_per[k].iter().any(|x| matches!(x, Src::Var(v) if *v == r))
                        && args.get(k - first_arg).map_or(true, |a| self.pl(a).map_or(true, |(_, p)| p.is_empty()))
                });
                if hit {
                    self.diags.push((e.line, format!("cannot use `{}` while it is mutably borrowed by an earlier operand of the same expression", show_var(&r))));
                }
            }
        }
        if let Some((pl, ty, decl, ex)) = late_recv {
            srcs_per[0] = self.receiver(pl, &ty, autoref, decl.as_ref(), ex, e.line);
        }
        for t in temps {
            self.cfg.emit(Ev::Use(t));
        }
        // 引数が運ぶ貸し出しの、戻り値への流れ。
        let flow = self.flows(def, &srcs_per, &tys, &e.ty, first_arg);
        let mut res = vec![];
        for (j, s) in srcs_per.iter().enumerate() {
            if flow[j] {
                res.extend(s.iter().cloned());
            }
        }
        // 保存場所へ: `&mut` の引数の指す先が借用を持ち得るなら、他の引数の貸し出しがその持ち主へ流れる (`v.push(&x)`)。
        for j in 0..srcs_per.len() {
            let inner = match &tys[j] {
                Ty::Ref(true, inner, _) => (**inner).clone(),
                t if j == 0 && has_recv && autoref == Some(true) => t.clone(),
                _ => continue,
            };
            if !holds_field(self.p, &inner) {
                continue;
            }
            let target = srcs_per[j].iter().find_map(|s| match s {
                Src::Loan(i) => Some(self.cfg.loans[*i].root.clone()),
                Src::Var(v) => Some(v.clone()),
            });
            let Some(t) = target else { continue };
            let mut inflow = vec![];
            for (k, s) in srcs_per.iter().enumerate() {
                if k == j || s.is_empty() {
                    continue;
                }
                // 相手も `&mut` の入れ物 (`mem.swap(&mut a, &mut b)`) なら、流れるのは相手が持つ中身の貸し出し。
                match &tys[k] {
                    Ty::Ref(true, inner, _) if holds_field(self.p, inner) => {
                        for x in s {
                            if let Src::Loan(i) = x {
                                inflow.push(Src::Var(self.cfg.loans[*i].root.clone()));
                            }
                        }
                    }
                    _ => inflow.extend(s.iter().cloned()),
                }
            }
            if !inflow.is_empty() {
                // 定数添字の `replace` が `a` へ足す流入は、要素の持ち主を崩さない (古い要素の貸し出しは `Kill` で外す)。
                let keep_slots = j == 0 && slot_target.as_ref().map_or(false, |x| x.0 == t) && !self.slot_unsafe.contains(&t);
                self.bind(t.clone(), inflow, false);
                if keep_slots {
                    self.slot_unsafe.remove(&t);
                }
            }
        }
        // 定数の添字の要素を `replace` で上書きすると、その要素が持っていた貸し出しは古い値 (戻り値) へ移って、`a` からは消える。
        if let Some((root, idx)) = slot_target {
            let ids = self.slots.get(&root).and_then(|v| v.get(idx)).cloned().unwrap_or_default();
            if !ids.is_empty() {
                res.extend(ids.iter().map(|i| Src::Loan(*i)));
                self.cfg.emit(Ev::Kill { var: root, ids });
            }
        }
        if let Some(c) = local_callee {
            res.push(Src::Var(c));
        }
        // `Sender.send(v)`: 送った値の貸し出しを、同じチャネルを持つ他の変数 (`rx`) の側へも流す。
        if def_name == "std.sync.Sender.send" {
            if let Some(from) = recv_root {
                let srcs = srcs_per.get(first_arg).cloned().unwrap_or_default();
                self.cfg.emit(Ev::ChanSend { from, srcs });
            }
        }
        // 中断点なら印を立てる: 直接の `uses [suspend]` の呼び先に加え、型検査が効果の実体化から
        // 中断点と決めた呼び出し (クロージャ・`Fn` 境界越し) も含む (types.rs の susp_lines)。
        let may_suspend = self.susp.contains(&(self.di, e.line))
            || cands.iter().any(|&i| self.p.defs[i].uses.iter().any(|u| u == "suspend"));
        if may_suspend {
            self.cfg.emit(Ev::Suspend { line: e.line });
        }
        for t in call_guard_temps {
            self.cfg.emit(Ev::Release(t));
        }
        let mut res = self.keep(&e.ty, res);
        // `std.sync.channel(..)` の結果はチャネル自体への擬似の貸し出しを持つ (送信が受信側へ借用を流す印)。
        if def_name == "std.sync.channel" {
            let id = self.cfg.loan(format!("$chan{}", e.id), vec![], false);
            res.push(Src::Loan(id));
        }
        res
    }

    fn flows(&self, def: Option<DefId>, srcs: &[Vec<Src>], tys: &[Ty], ret: &Ty, first_arg: usize) -> Vec<bool> {
        let n = srcs.len();
        let _ = first_arg;
        let all: Vec<bool> = (0..n).map(|j| !srcs[j].is_empty()).collect();
        let Some(i) = def else { return all };
        let d = &self.p.defs[i];
        let Some(sig) = self.tk.side.regions.sigs.get(&i) else { return all };
        if sig.params.len() != n || d.params.len() != n {
            return all;
        }
        let gn = d.gnames();
        let dret = from_v(&d.ret, &gn);
        let mut rnames = BTreeSet::new();
        param_names(&dret, &mut rnames);
        let rregs: Vec<_> = sig.ret.iter().filter(|r| !matches!(r, crate::types::Region::Unk | crate::types::Region::Static)).collect();
        let mut out = vec![false; n];
        let mut any = false;
        for j in 0..n {
            let mut pn = BTreeSet::new();
            param_names(&from_v(&d.params[j].1, &gn), &mut pn);
            let reg = sig.params[j].1.iter().any(|r| rregs.iter().any(|q| q.same(r)));
            let tp = pn.intersection(&rnames).next().is_some();
            out[j] = (reg || tp) && all[j];
            any |= out[j];
        }
        // 戻り値が借用を運ぶ型なのに流れ先が決まらない (署名の領域が省略・隠れている) ときは、保守側に全引数。
        if !any && holds(self.p, ret) && rregs.is_empty() && rnames.is_empty() && !matches!(dret, Ty::Named(..) if false) {
            let hidden = holds(self.p, &dret);
            if hidden {
                return all;
            }
        }
        out
    }

    fn receiver(&mut self, pl: Option<(String, Path)>, ty: &Ty, autoref: Option<bool>, decl: Option<&Ty>, ex: Option<&TExpr>, line: usize) -> Vec<Src> {
        let Some((root, path)) = pl else {
            return match ex {
                Some(x) => self.expr(x),
                None => vec![],
            };
        };
        if let Some(x) = ex {
            // 受け手の式の副作用 (添字など)。
            self.cfg.emit(Ev::Use(root.clone()));
            self.index_effects(x);
        } else {
            self.cfg.emit(Ev::Use(root.clone()));
        }
        if let Some(m) = autoref {
            return self.borrow(&root, &path, m, line);
        }
        if let Ty::Ref(rm, _, _) = ty {
            let want = match decl {
                Some(Ty::Ref(m, ..)) => Some(*m),
                Some(_) => None,
                None => Some(*rm),
            };
            return match want {
                Some(m) => {
                    let mut p2 = path.clone();
                    p2.push("*".into());
                    let mut s = self.borrow(&root, &p2, m, line);
                    s.push(Src::Var(root));
                    s
                }
                None => {
                    self.acc(&root, &path, Acc::Read, line);
                    vec![Src::Var(root)]
                }
            };
        }
        let kind = if not_copy(self.p, ty) { Acc::Move } else { Acc::Read };
        self.acc(&root, &path, kind, line);
        if holds(self.p, ty) {
            vec![Src::Var(root)]
        } else {
            vec![]
        }
    }

    /// 引数 1 つ。参照の変数・欄をそのまま渡す形は暗黙の再借用 (`Coercion::Reborrow`)。
    fn arg(&mut self, a: &TExpr, pt: Option<&Ty>) -> Vec<Src> {
        if let Ty::Ref(am, _, _) = &a.ty {
            if self.pl(a).is_some() && !matches!(a.kind, TKind::Unary { .. } if false) {
                // `&mut` の参照を渡すと再借用。共有の参照は `Copy` (持つ貸し出しがそのまま移る)。
                let to_mut = a.conv.iter().find_map(|c| if let Coercion::Reborrow { to_mut } = c { Some(*to_mut) } else { None });
                let m = match (to_mut, pt) {
                    (Some(m), _) => m,
                    (None, Some(Ty::Ref(pm, ..))) => *pm && *am,
                    _ => *am,
                };
                if *am {
                    let (root, mut path) = self.place(a).unwrap();
                    path.push("*".into());
                    let mut s = self.borrow(&root, &path, m, a.line);
                    s.push(Src::Var(root));
                    return s;
                }
            }
        }
        self.expr(a)
    }

    // -------------------------------------------------------- 解析

    /// `a[<定数>]` への代入先なら、その要素の番号 (`a` は追跡中の配列リテラルの局所で、持ち主がまだ崩れていないとき)。
    fn elem_slot(&self, place: &TExpr, root: &str) -> Option<usize> {
        let TKind::Index { base, index } = &place.kind else { return None };
        if !matches!(base.ty, Ty::Array(..)) || self.slot_unsafe.contains(root) {
            return None;
        }
        let TKind::Lit(l) = &index.kind else { return None };
        let mut it = l.split(':');
        if it.next() != Some("int") {
            return None;
        }
        let i: usize = it.next()?.parse().ok()?;
        (i < self.slots.get(root)?.len()).then_some(i)
    }

    /// `&mut a[<定数>]` (`a` は局所の配列) なら (根, 添字)。
    fn const_slot(&self, a: &TExpr) -> Option<(String, usize)> {
        let TKind::Unary { op, operand } = &a.kind else { return None };
        if op != "&mut" {
            return None;
        }
        let TKind::Index { base, index } = &operand.kind else { return None };
        let TKind::Lit(l) = &index.kind else { return None };
        if !matches!(base.ty, Ty::Array(..)) {
            return None;
        }
        let (root, path) = self.pl(base)?;
        if !path.is_empty() {
            return None;
        }
        let mut it = l.split(':');
        if it.next() != Some("int") {
            return None;
        }
        Some((root, it.next()?.parse().ok()?))
    }

    fn analyze(&mut self, cfg: Cfg) {
        if std::env::var_os("KSN_DUMP_BORROWS").is_some() {
            for (i, e) in cfg.evs.iter().enumerate() {
                eprintln!("node {} -> {:?}", i, cfg.succ[i]);
                for x in e {
                    eprintln!("    {:?}", x);
                }
            }
            for (i, l) in cfg.loans.iter().enumerate() {
                eprintln!("loan {} {} {:?} mut={}", i, l.root, l.path, l.mutable);
            }
        }
        let n = cfg.evs.len();
        // 前向き: 変数ごとの貸し出しの集合。
        type St = HashMap<String, BTreeSet<usize>>;
        let step = |st: &mut St, ev: &Ev| {
            if let Ev::Kill { var, ids } = ev {
                if let Some(x) = st.get_mut(var) {
                    for i in ids {
                        x.remove(i);
                    }
                }
            }
            if let Ev::ChanSend { from, srcs } = ev {
                // 送り手の持つチャネルの貸し出しを持つ他の変数へ、送った値の貸し出しを足す。
                let chans: Vec<usize> = st
                    .get(from)
                    .map(|s| s.iter().copied().filter(|i| cfg.loans[*i].root.starts_with("$chan")).collect())
                    .unwrap_or_default();
                if !chans.is_empty() {
                    let mut add: BTreeSet<usize> = BTreeSet::new();
                    for s in srcs {
                        match s {
                            Src::Loan(i) => {
                                add.insert(*i);
                            }
                            Src::Var(v) => {
                                if let Some(x) = st.get(v) {
                                    add.extend(x.iter().copied());
                                }
                            }
                        }
                    }
                    if !add.is_empty() {
                        let from = from.clone();
                        for (v, set) in st.iter_mut() {
                            if *v == from {
                                continue;
                            }
                            if chans.iter().any(|c| set.contains(c)) {
                                set.extend(add.iter().copied());
                            }
                        }
                    }
                }
            }
            if let Ev::Bind { var, srcs, replace } = ev {
                let mut set = BTreeSet::new();
                for s in srcs {
                    match s {
                        Src::Loan(i) => {
                            set.insert(*i);
                        }
                        Src::Var(v) => {
                            if let Some(x) = st.get(v) {
                                set.extend(x.iter().copied());
                            }
                        }
                    }
                }
                if *replace {
                    if set.is_empty() {
                        st.remove(var);
                    } else {
                        st.insert(var.clone(), set);
                    }
                } else if !set.is_empty() {
                    st.entry(var.clone()).or_default().extend(set);
                }
            }
        };
        let mut sin: Vec<Option<St>> = vec![None; n];
        sin[0] = Some(HashMap::new());
        let mut work: Vec<usize> = vec![0];
        let mut guard = 0usize;
        while let Some(b) = work.pop() {
            guard += 1;
            if guard > 200_000 {
                return;
            }
            let mut st = sin[b].clone().unwrap();
            for ev in &cfg.evs[b] {
                step(&mut st, ev);
            }
            for &s in &cfg.succ[b] {
                let changed = match &mut sin[s] {
                    None => {
                        sin[s] = Some(st.clone());
                        true
                    }
                    Some(old) => {
                        let mut ch = false;
                        for (k, v) in &st {
                            let e = old.entry(k.clone()).or_default();
                            let before = e.len();
                            e.extend(v.iter().copied());
                            ch |= e.len() != before;
                        }
                        ch
                    }
                };
                if changed {
                    work.push(s);
                }
            }
        }
        // 後向き: 生きている変数。
        let mut live_in: Vec<BTreeSet<String>> = vec![BTreeSet::new(); n];
        let mut changed = true;
        while changed {
            changed = false;
            for b in (0..n).rev() {
                let mut cur: BTreeSet<String> = BTreeSet::new();
                for &s in &cfg.succ[b] {
                    cur.extend(live_in[s].iter().cloned());
                }
                for ev in cfg.evs[b].iter().rev() {
                    match ev {
                        Ev::Use(v) => {
                            cur.insert(v.clone());
                        }
                        Ev::Bind { var, replace: true, .. } => {
                            cur.remove(var);
                        }
                        _ => {}
                    }
                }
                if cur != live_in[b] {
                    live_in[b] = cur;
                    changed = true;
                }
            }
        }
        // ロックのガードを持ったまま中断点に着く (E3050): 落とされる (移動・`drop`・スコープの終わり) までロックを持つ。
        {
            let mut held_in: Vec<BTreeSet<String>> = vec![BTreeSet::new(); n];
            let step = |ev: &Ev, cur: &mut BTreeSet<String>| match ev {
                Ev::Hold(v) => {
                    cur.insert(v.clone());
                }
                Ev::Release(v) => {
                    cur.remove(v);
                }
                _ => {}
            };
            let mut changed = true;
            while changed {
                changed = false;
                for b in 0..n {
                    let mut cur = held_in[b].clone();
                    for ev in &cfg.evs[b] {
                        step(ev, &mut cur);
                    }
                    for &s in &cfg.succ[b] {
                        if !cur.is_subset(&held_in[s]) {
                            held_in[s].extend(cur.iter().cloned());
                            changed = true;
                        }
                    }
                }
            }
            for b in 0..n {
                let mut cur = held_in[b].clone();
                for ev in &cfg.evs[b] {
                    if let Ev::Suspend { line } = ev {
                        for g in cur.iter().filter(|g| self.guards.contains(*g)) {
                            let who = if g.starts_with("$") { "a temporary".to_string() } else { format!("`{}`", show_var(g)) };
                            self.susp_diags.push((*line, format!("cannot hold a lock guard across a suspension point: {} still holds the lock (drop it with `core.mem.drop` before the call that may suspend)", who)));
                        }
                    }
                    step(ev, &mut cur);
                }
            }
        }
        // 競合の検出。
        for b in 0..n {
            let Some(mut st) = sin[b].clone() else { continue };
            let evs = &cfg.evs[b];
            let mut live_after: Vec<BTreeSet<String>> = vec![BTreeSet::new(); evs.len()];
            let mut cur: BTreeSet<String> = BTreeSet::new();
            for &s in &cfg.succ[b] {
                cur.extend(live_in[s].iter().cloned());
            }
            for (k, ev) in evs.iter().enumerate().rev() {
                live_after[k] = cur.clone();
                match ev {
                    Ev::Use(v) => {
                        cur.insert(v.clone());
                    }
                    Ev::Bind { var, replace: true, .. } => {
                        cur.remove(var);
                    }
                    _ => {}
                }
            }
            for (k, ev) in evs.iter().enumerate() {
                if let Ev::Suspend { line } = ev {
                    // 中断点で生きている (この後使われる) 持ち主の貸し出しは E3050 (5.6)。
                    let mut holders: Vec<(&String, &BTreeSet<usize>)> = st.iter().collect();
                    holders.sort();
                    for (h, ids) in holders {
                        if !live_after[k].contains(h) {
                            continue;
                        }
                        for &id in ids {
                            let l = &cfg.loans[id];
                            let who = if h.starts_with("$iter") {
                                "the loop iterator".to_string()
                            } else if h.starts_with("$cap") {
                                "the shared capture of the closure".to_string()
                            } else if h.starts_with("$e") {
                                "an earlier operand of the same expression".to_string()
                            } else {
                                format!("`{}`", show_var(h))
                            };
                            self.susp_diags.push((*line, format!("cannot hold a borrow of `{}` across a suspension point: {} is used after the call that may suspend", show_var(&l.root), who)));
                        }
                    }
                }
                if let Ev::Acc { root, path, kind, line } = ev {
                    let mut holders: Vec<(&String, &BTreeSet<usize>)> = st.iter().collect();
                    holders.sort();
                    for (h, ids) in holders {
                        if !live_after[k].contains(h) {
                            continue;
                        }
                        for &id in ids {
                            let l = &cfg.loans[id];
                            if &l.root != root || !overlaps(&l.path, path) {
                                continue;
                            }
                            if h == root && l.path.first().map_or(false, |x| x == "*") {
                                continue;
                            }
                            if *kind == Acc::Write && path.is_empty() && !l.mutable && l.path.first().map_or(false, |x| x == "*") {
                                continue;
                            }
                            let who = if h.starts_with("$iter") {
                                "the loop iterator".to_string()
                            } else if h.starts_with("$cap") {
                                "the shared capture of the closure".to_string()
                            } else if h.starts_with("$e") {
                                "an earlier operand of the same expression".to_string()
                            } else {
                                format!("`{}`", show_var(h))
                            };
                            let name = show_var(root);
                            if l.mutable {
                                self.diags.push((*line, format!("cannot use `{}` while it is mutably borrowed by {}", name, who)));
                            } else if matches!(kind, Acc::Write | Acc::MutB) {
                                self.diags.push((*line, format!("cannot mutate `{}` while it is borrowed by {}", name, who)));
                            } else if *kind == Acc::Move {
                                self.diags.push((*line, format!("cannot move `{}` while it is borrowed by {}", name, who)));
                            }
                        }
                    }
                }
                step(&mut st, ev);
            }
        }
        let _ = show_place;
    }
}
