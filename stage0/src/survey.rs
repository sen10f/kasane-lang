//! TAST (型付き AST) の事前調査の計測 (隠し機能、設計_型付き検査.md 7 章)。
//!
//! `KSN_TYPE_SURVEY=1 ksn check` のとき、ユーザー定義の本体の AST を全部辿り、ノードの種別ごとに
//! 「型表 (`tytab`) にある / 型が確定 / 未確定」を数え、呼び出しは「解決先の記録 (`call_res`)」の有無を数えて
//! stderr に TSV で出す。検査の結果は変えない (読むだけ)。`KSN_TYPE_SURVEY=2` は 1 件ごとの未記録の箇所も出す。

use std::collections::{BTreeMap, HashMap};

use crate::check::{items, nf, nname, st, strs};
use crate::parser::{nid, V};
use crate::program::{Program, Target};
use crate::types::{ExprTy, Ty};
use crate::typecov::{classify, Cls};

#[derive(Default)]
struct Row {
    n: usize,
    known: usize,
    partial: usize,
    absent: usize,
}

#[derive(Default)]
struct Calls {
    /// MCall と、局所変数から始まる経路形のメソッド呼び出し (`s.m(..)`)。
    mcalls: usize,
    m_unique: usize,
    m_multi: usize,
    m_none: usize,
    m_absent: usize,
    /// 同じ (定義, 行, 名前) に 2 つ以上のメソッド呼び出しがあり、`call_res` の鍵がぶつかる呼び出しの数。
    m_key_collide: usize,
    /// 受け手に暗黙の自動参照 (`x` → `&x` / `&mut x`) が要る (受け手の型が参照でなく、`self` が参照) 呼び出し。
    m_autoref: usize,
    m_autoref_known: usize,
    /// 経路形の関数呼び出し。
    free: usize,
    free_fn: usize,
    free_generic: usize,
    free_variant: usize,
    free_other_target: usize,
    free_unresolved: usize,
    /// 経路でない呼び先の呼び出し (`(h.f)(x)`、`fs[i](x)`)。
    indirect: usize,
}

pub fn run(p: &Program, user: &[usize], tytab: &BTreeMap<(String, u32), ExprTy>, call_res: &HashMap<(usize, usize, String), Option<Vec<usize>>>, let_tys: &HashMap<(usize, usize, String), Ty>) {
    let verbose = std::env::var("KSN_TYPE_SURVEY").is_ok_and(|v| v == "2");
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut c = Calls::default();
    let mut bind_names = 0usize;
    let (mut binds, mut binds_let, mut caps, mut closures, mut closures_typed) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut tries = 0usize;
    let mut key_count: HashMap<(usize, usize, String), usize> = HashMap::new();
    for &di in user {
        let d = &p.defs[di];
        for root in [&d.body, &d.init].into_iter().flatten() {
            count_keys(root, di, &mut key_count);
        }
    }
    for &di in user {
        let d = &p.defs[di];
        for root in [&d.body, &d.init].into_iter().flatten() {
            let mut cx = Cx { p, di, tytab, call_res, let_tys, key_count: &key_count, rows: &mut rows, c: &mut c, verbose };
            cx.walk(root);
            let (b, bl, cp, cl, clt, tr, _, bn) = cx.misc(root);
            binds += b;
            bind_names += bn;
            binds_let += bl;
            caps += cp;
            closures += cl;
            closures_typed += clt;
            tries += tr;
        }
    }
    eprintln!("# survey kind\ttotal\tknown\tpartial_or_unknown\tno_tytab_entry");
    for (k, r) in &rows {
        eprintln!("kind\t{}\t{}\t{}\t{}\t{}", k, r.n, r.known, r.partial, r.absent);
    }
    eprintln!(
        "calls\tmcalls={} unique={} multi={} unresolved_none={} absent={} key_collide={} autoref_needed={} (recv_known={})",
        c.mcalls, c.m_unique, c.m_multi, c.m_none, c.m_absent, c.m_key_collide, c.m_autoref, c.m_autoref_known
    );
    eprintln!(
        "calls\tfree={} fn={} (generic={}) variant={} other_target={} unresolved(local/closure)={} indirect={}",
        c.free, c.free_fn, c.free_generic, c.free_variant, c.free_other_target, c.free_unresolved, c.indirect
    );
    eprintln!("binds\tPBind_nodes={} names_in_let/arm/for_patterns={} with_let_tys={}", binds, bind_names, binds_let);
    eprintln!("closures\tn={} typed_closure_ty={} capture_items={}", closures, closures_typed, caps);
    eprintln!("try\tn={}", tries);
}

fn count_keys(v: &V, di: usize, m: &mut HashMap<(usize, usize, String), usize>) {
    match v {
        V::Node(k, fs, line, _) => {
            if *k == "MCall" {
                *m.entry((di, *line, st(&fs[1]))).or_default() += 1;
            }
            for f in fs {
                count_keys(f, di, m);
            }
        }
        V::L(xs) => xs.iter().for_each(|x| count_keys(x, di, m)),
        _ => {}
    }
}

struct Cx<'a> {
    p: &'a Program,
    di: usize,
    tytab: &'a BTreeMap<(String, u32), ExprTy>,
    call_res: &'a HashMap<(usize, usize, String), Option<Vec<usize>>>,
    let_tys: &'a HashMap<(usize, usize, String), Ty>,
    key_count: &'a HashMap<(usize, usize, String), usize>,
    rows: &'a mut BTreeMap<String, Row>,
    c: &'a mut Calls,
    verbose: bool,
}

impl Cx<'_> {
    fn ty_of(&self, e: &V) -> Option<&ExprTy> {
        let id = nid(e);
        if id == 0 {
            return None;
        }
        self.tytab.get(&(self.p.defs[self.di].name.clone(), id))
    }

    fn walk(&mut self, v: &V) {
        match v {
            V::Node(k, fs, line, _) => {
                let t = self.ty_of(v).map(|e| classify(&e.ty));
                let r = self.rows.entry(k.to_string()).or_default();
                r.n += 1;
                match t {
                    None => r.absent += 1,
                    Some(Cls::Known | Cls::Param) => r.known += 1,
                    Some(_) => r.partial += 1,
                }
                if self.verbose && t.is_none() {
                    eprintln!("absent\t{}\t{}\tline {}", self.p.defs[self.di].name, k, line);
                }
                match *k {
                    "MCall" => self.mcall(v, fs, *line),
                    "Call" => self.call(v, fs, *line),
                    _ => {}
                }
                for (i, f) in fs.iter().enumerate() {
                    if *k == "Call" && i == 0 && nname(f) == "Path" {
                        // 呼び先の経路は式として推論されない (`call` が解決する)。別の行に数える。
                        let has = self.ty_of(f).is_some();
                        let r = self.rows.entry("Path(callee)".to_string()).or_default();
                        r.n += 1;
                        if has {
                            r.known += 1;
                        } else {
                            r.absent += 1;
                        }
                        continue;
                    }
                    self.walk(f);
                }
            }
            V::L(xs) => xs.iter().for_each(|x| self.walk(x)),
            _ => {}
        }
    }

    fn mcall(&mut self, _v: &V, fs: &[V], line: usize) {
        let name = st(&fs[1]);
        self.method_site(&name, line, self.ty_of(&fs[0]).map(|e| e.ty.clone()));
    }

    fn method_site(&mut self, name: &str, line: usize, recv: Option<Ty>) {
        self.c.mcalls += 1;
        let key = (self.di, line, name.to_string());
        if self.key_count.get(&key).is_some_and(|&n| n > 1) {
            self.c.m_key_collide += 1;
        }
        let callee = match self.call_res.get(&key) {
            Some(Some(v)) if v.len() == 1 => {
                self.c.m_unique += 1;
                Some(v[0])
            }
            Some(Some(_)) => {
                self.c.m_multi += 1;
                None
            }
            Some(None) => {
                self.c.m_none += 1;
                None
            }
            None => {
                self.c.m_absent += 1;
                None
            }
        };
        // 自動参照: 解決先の `self` が参照で、受け手の型が参照でない。
        if let (Some(ci), Some(rt)) = (callee, recv) {
            let d = &self.p.defs[ci];
            if let Some((_, pt)) = d.params.first() {
                let self_ref = matches!(crate::types::from_v(pt, &d.gnames()), Ty::Ref(..));
                if self_ref {
                    self.c.m_autoref += 1;
                    if classify(&rt) != Cls::Unknown && !matches!(rt.strip(), Ty::Unknown) && !matches!(rt, Ty::Ref(..)) {
                        self.c.m_autoref_known += 1;
                    }
                }
            }
        }
    }

    fn call(&mut self, _v: &V, fs: &[V], line: usize) {
        let callee = &fs[0];
        if nname(callee) != "Path" {
            self.c.indirect += 1;
            return;
        }
        let segs = strs(&nf(callee)[0]);
        let file = self.p.defs[self.di].file.clone();
        match self.p.resolve(&segs, &file) {
            Some(r) if r.rest.is_empty() => {
                self.c.free += 1;
                match r.target {
                    Target::Fn(i) | Target::Sig(i) => {
                        self.c.free_fn += 1;
                        if self.p.defs[i].generics.iter().any(|g| g.0 == "type") {
                            self.c.free_generic += 1;
                        }
                    }
                    Target::Variant(..) => self.c.free_variant += 1,
                    _ => self.c.free_other_target += 1,
                }
            }
            _ => {
                // 局所変数の経路 (`s.m(..)` はメソッド、`f(x)` はクロージャ)、または `Enum.V.m()` など。
                if segs.len() >= 2 {
                    let name = segs.last().unwrap().clone();
                    self.method_site(&name, line, None);
                } else {
                    self.c.free += 1;
                    self.c.free_unresolved += 1;
                }
            }
        }
    }

    /// (PBind 数, うち let_tys にある数, capture 項目数, Closure 数, 型が Closure の数, `?` の数, 誤差のある `?`)
    fn misc(&self, root: &V) -> (usize, usize, usize, usize, usize, usize, usize, usize) {
        let mut o = (0, 0, 0, 0, 0, 0, 0, 0);
        self.misc_walk(root, &mut o);
        o
    }

    fn misc_walk(&self, v: &V, o: &mut (usize, usize, usize, usize, usize, usize, usize, usize)) {
        match v {
            V::Node(k, fs, line, _) => {
                match *k {
                    "PBind" => o.0 += 1,
                    "Let" | "For" | "Match" => {
                        let mut sites: Vec<(&V, usize)> = vec![];
                        match *k {
                            "Let" | "For" => sites.push((&fs[0], *line)),
                            _ => {
                                for a in items(&fs[1]) {
                                    if let Some(pt) = items(a).first() {
                                        sites.push((pt, crate::check::nline(pt)));
                                    }
                                }
                            }
                        }
                        for (pt, l) in sites {
                            let mut names = vec![];
                            crate::check::pat_binds(pt, &mut names);
                            o.7 += names.len();
                            o.1 += names.iter().filter(|n| self.let_tys.contains_key(&(self.di, l, (*n).clone()))).count();
                        }
                    }
                    "Closure" => {
                        o.3 += 1;
                        o.2 += items(&fs[2]).len();
                        if matches!(self.ty_of(v).map(|e| &e.ty), Some(Ty::Closure { id: Some(_), .. })) {
                            o.4 += 1;
                        }
                    }
                    "Try" => {
                        o.5 += 1;
                    }
                    _ => {}
                }
                for f in fs {
                    self.misc_walk(f, o);
                }
            }
            V::L(xs) => xs.iter().for_each(|x| self.misc_walk(x, o)),
            _ => {}
        }
    }
}
