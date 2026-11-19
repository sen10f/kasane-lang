//! 型推論の被覆率の計測 (隠し機能)。
//!
//! `KSN_TYPE_COVERAGE=1 ksn check` のとき、`TypeCk` が式を推論するたびに結果を記録し、関数の検査の終わりに
//! 型変数を解決して分類する。集計は stderr に出す (`KSN_TYPE_COVERAGE_TOP` で未知の原因の表示件数を変える、既定 15)。
//! 既定の出力・診断には影響しない (`TypeCk::cov` が `None` のときは何もしない)。

use std::collections::{BTreeMap, HashMap};

use crate::types::Ty;

/// 式の型の分類。`Known` だけが「確定」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cls {
    /// 型変数・未知・型パラメータを含まない
    Known,
    /// 一部 (型引数の中) が未知
    Partial,
    /// ジェネリクスの型パラメータを含む (単相化前の型)
    Param,
    /// 型変数が解決されなかった
    Var,
    /// 型が分からない (最上位が未知)
    Unknown,
}

/// `infer_node` の match の腕に対応する種別。これ以外は既定の腕 (型規則なし) で、結果は必ず未知になる。
/// `types.rs` の `infer_node` を変えたら、ここも変えること。
pub const HANDLED: &[&str] = &[
    "Block", "Lit", "Path", "Call", "MCall", "Field", "Try", "Index", "Unary", "Bin", "StructLit", "TupleE", "ArrayE",
    "If", "Match", "For", "Closure", "Return", "Break", "Continue",
];

/// 1 回の推論の記録。型変数は未解決のまま持ち、関数の終わりに解決する。
pub struct Rec {
    /// 式 ID (`parser::nid`。同じ関数の中で一意)。同じ式の再推論は上書きする。
    pub key: usize,
    pub kind: String,
    /// 原因の手掛かり (メソッド名、パス、演算子など)。
    pub detail: String,
    pub ty: Ty,
    /// 型が子の型から来るノードの式 ID。子が未知なら未知は伝播したとみなす。
    pub subject: Option<usize>,
    /// 先頭が局所変数のパス (`x` / `x.f` / `x.m()` の `x`) の、その局所変数の型。
    /// 局所変数の型が未知・型変数なら、未知は伝播したとみなす。
    pub local: Option<Ty>,
    /// `file:line`
    pub loc: String,
}

/// 種別ごとの件数。
#[derive(Default, Clone, Copy)]
pub struct Counts {
    pub known: usize,
    pub partial: usize,
    pub param: usize,
    pub var: usize,
    pub unknown: usize,
    /// うち未知の根 (子からの伝播ではない未知)
    pub root: usize,
}

impl Counts {
    pub fn total(&self) -> usize {
        self.known + self.partial + self.param + self.var + self.unknown
    }
    fn add(&mut self, c: Cls, root: bool) {
        match c {
            Cls::Known => self.known += 1,
            Cls::Partial => self.partial += 1,
            Cls::Param => self.param += 1,
            Cls::Var => self.var += 1,
            Cls::Unknown => {
                self.unknown += 1;
                if root {
                    self.root += 1;
                }
            }
        }
    }
}

#[derive(Default)]
pub struct Cov {
    pending: Vec<Rec>,
    index: HashMap<usize, usize>,
    pub by_kind: BTreeMap<String, Counts>,
    pub total: Counts,
    /// 未知の根の原因 (種別 + 詳細) → (件数, 最初に見つかった位置)
    pub causes: BTreeMap<String, (usize, String)>,
    /// 型変数が残った式の種別 + 詳細 → (件数, 最初に見つかった位置)
    pub var_causes: BTreeMap<String, (usize, String)>,
}

impl Cov {
    pub fn record(&mut self, rec: Rec) {
        match self.index.get(&rec.key) {
            Some(&i) => self.pending[i] = rec,
            None => {
                self.index.insert(rec.key, self.pending.len());
                self.pending.push(rec);
            }
        }
    }

    /// 関数の終わりに、未解決の記録を取り出す。
    pub fn take_pending(&mut self) -> Vec<Rec> {
        self.index.clear();
        std::mem::take(&mut self.pending)
    }

    /// 解決済みの 1 件を集計する。`root` は、未知が子からの伝播でないこと。
    pub fn add(&mut self, rec: &Rec, c: Cls, root: bool) {
        self.total.add(c, root);
        self.by_kind.entry(rec.kind.clone()).or_default().add(c, root);
        let cause = if rec.detail.is_empty() { rec.kind.clone() } else { format!("{} {}", rec.kind, rec.detail) };
        if c == Cls::Unknown && root {
            let e = self.causes.entry(cause).or_insert((0, rec.loc.clone()));
            e.0 += 1;
        } else if c == Cls::Var {
            let e = self.var_causes.entry(cause).or_insert((0, rec.loc.clone()));
            e.0 += 1;
        }
    }
}

/// 解決済みの型を分類する。
pub fn classify(t: &Ty) -> Cls {
    if *t == Ty::Unknown {
        return Cls::Unknown;
    }
    if matches!(t, Ty::Var(_)) {
        return Cls::Var;
    }
    let mut fl = Flags::default();
    scan(t, &mut fl);
    if fl.var {
        Cls::Var
    } else if fl.unknown {
        Cls::Partial
    } else if fl.param {
        Cls::Param
    } else {
        Cls::Known
    }
}

#[derive(Default)]
struct Flags {
    var: bool,
    unknown: bool,
    param: bool,
}

fn scan(t: &Ty, fl: &mut Flags) {
    match t {
        Ty::Unknown => fl.unknown = true,
        Ty::Var(_) => fl.var = true,
        Ty::Param(_) => fl.param = true,
        Ty::Named(_, a, _) | Ty::Tuple(a) => a.iter().for_each(|x| scan(x, fl)),
        Ty::Ref(_, x, _) | Ty::Array(x, _) | Ty::Slice(x) => scan(x, fl),
        Ty::Closure { args, ret, .. } => {
            args.iter().for_each(|x| scan(x, fl));
            scan(ret, fl);
        }
        _ => {}
    }
}

/// 集計を stderr に出す。
pub fn report(c: &Cov, top: usize) {
    let t = c.total;
    let n = t.total();
    let pct = |x: usize| if n == 0 { 0.0 } else { 100.0 * x as f64 / n as f64 };
    eprintln!("[type-coverage] 式 {} 件: 確定 {} ({:.1}%) / 一部未知 {} / 型パラメータ {} / 型変数 {} / 未知 {} ({:.1}%, うち根 {})",
        n, t.known, pct(t.known), t.partial, t.param, t.var, t.unknown, pct(t.unknown), t.root);
    eprintln!("[type-coverage] 種別ごと (件数 降順): 種別 件数 確定 一部未知 型P 型変数 未知 うち根 確定率");
    let mut rows: Vec<(&String, &Counts)> = c.by_kind.iter().collect();
    rows.sort_by(|a, b| b.1.total().cmp(&a.1.total()).then(a.0.cmp(b.0)));
    for (k, r) in rows {
        let tot = r.total();
        let rate = if tot == 0 { 0.0 } else { 100.0 * r.known as f64 / tot as f64 };
        eprintln!("[type-coverage]   {:<12} {:>6} {:>6} {:>6} {:>5} {:>6} {:>6} {:>6} {:>6.1}%", k, tot, r.known, r.partial, r.param, r.var, r.unknown, r.root, rate);
    }
    let mut causes: Vec<(&String, &(usize, String))> = c.causes.iter().collect();
    causes.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.0.cmp(b.0)));
    eprintln!("[type-coverage] 未知の根の原因 (上位 {} 件、件数 原因 代表例)", top);
    for (cause, (cnt, loc)) in causes.into_iter().take(top) {
        eprintln!("[type-coverage]   {:>6}  {:<36} {}", cnt, cause, loc);
    }
    let mut vars: Vec<(&String, &(usize, String))> = c.var_causes.iter().collect();
    vars.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.0.cmp(b.0)));
    eprintln!("[type-coverage] 型変数が残った式の原因 (上位 {} 件、件数 原因 代表例)", top);
    for (cause, (cnt, loc)) in vars.into_iter().take(top) {
        eprintln!("[type-coverage]   {:>6}  {:<36} {}", cnt, cause, loc);
    }
}
