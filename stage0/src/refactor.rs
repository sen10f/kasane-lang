//! `ksn refs` / `ksn rename` の本体 (参照箇所の列挙と改名計画)。LSP も同じ計画を使う。
//!
//! 方針:
//! - 参照箇所は字句 (トークン) の連鎖 `a.b.c` として見つけ、局所変数で始まるものは除く。
//!   宣言の文脈 (ヘッダ・`end`・`type`/`enum`/`trait` の本体) では連鎖は常に FQN、
//!   式の文脈では `import` の末尾名による短縮形を展開してから解決する (3.3)。
//! - メソッド呼び出し `x.m()` とフィールド参照 `x.f` は、型推論が記録したレシーバの型 (`RecvSite`) で決める。
//!   型が決まらないときは、名前がプログラム全体で一意なら採用し、そうでなければ曖昧な箇所として失敗する。
//! - 定義の移動・モジュールの改名では、移した参照を「同一モジュールは FQN、`import` 済みは短縮形」の
//!   規則で書き直し、`import` 行を過不足なく作り直し、最後に `ksn fmt` と同じ整形で正規の並びにする。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::check::{children, nf, nname, st};
use crate::cli::{build_program, Loaded};
use crate::lexer::{self, Tok};
use crate::parser::{unpack_pos, V};
use crate::program::{Def, Program};
use crate::project::{norm, RESERVED_NAMES};
use crate::types::{Ty, TypeCk};

// ------------------------------------------------------------ 公開する型

/// 参照箇所 1 つ。
#[derive(Clone, Debug)]
pub struct Site {
    /// ソースファイルの絶対パス (移動前)。
    pub file: String,
    pub line: usize,
    /// 1 始まりの文字位置 [col, end_col)。
    pub col: usize,
    pub end_col: usize,
    /// 含む定義の名前 (`import` 行は "(import)"、`impl` 行は "(impl)")。
    pub owner: String,
    pub kind: &'static str,
    /// 元の綴り。
    pub text: String,
    /// `rename` が書き換える後の綴り (書き換えない箇所は `None`)。
    pub new: Option<String>,
}

/// 変更後のファイル 1 つ。
pub struct FileChange {
    pub old_path: Option<String>,
    pub new_path: String,
    pub old_text: String,
    pub new_text: String,
}

pub struct Plan {
    pub changes: Vec<FileChange>,
    /// `kasane.toml` の変更 (パス, 新しい本文)。
    pub manifest: Option<(String, String, String)>,
    /// 書き換えない言及 (ドキュメントコメント) など、利用者への注意。
    pub notes: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Plain,
    Module,
    Method,
    Field,
    Variant,
    TraitMethod,
}

struct Op {
    kind: Kind,
    old: String,
    new: String,
    pairs: Vec<(Vec<String>, Vec<String>)>,
    /// メンバー (メソッド・フィールド・列挙子) の旧名・新名。
    member_old: String,
    member_new: String,
    /// メンバー改名の所有者 (型の FQN)。TraitMethod では実装を持つ型の集合。
    owners: BTreeSet<String>,
    /// TraitMethod のトレイトの FQN。
    trait_fqn: String,
}

fn comps(s: &str) -> Vec<String> {
    s.split('.').map(String::from).collect()
}

fn is_prefix(a: &[String], full: &[String]) -> bool {
    full.len() >= a.len() && full[..a.len()] == *a
}

fn boundary_ok(text: &str, i: usize, len: usize) -> bool {
    let w = |c: char| c.is_alphanumeric() || c == '_';
    let before = text[..i].chars().next_back();
    let after = text[i + len..].chars().next();
    before.map_or(true, |c| !(w(c) || c == '.')) && after.map_or(true, |c| !w(c))
}

/// 1 行を字句に分ける (失敗したら空)。行番号は `ln`。
fn lex_line(line: &str, ln: usize) -> Vec<Tok> {
    match lexer::lex(&format!("{}\n", line)) {
        Ok(mut ts) => {
            ts.retain(|t| t.kind != "nl" && t.kind != "eof");
            for t in ts.iter_mut() {
                t.line = ln;
            }
            ts
        }
        Err(_) => vec![],
    }
}

/// 文字位置 (1 始まり) → 行内のバイト位置。
fn byte_of(line: &str, col: usize) -> usize {
    line.char_indices().nth(col.saturating_sub(1)).map(|(i, _)| i).unwrap_or(line.len())
}

fn char_col(line: &str, byte: usize) -> usize {
    line[..byte].chars().count() + 1
}

struct Chain {
    b0: usize,
    b1: usize,
    segs: Vec<String>,
    /// 先頭トークンの添字。
    tok: usize,
}

fn chains_in(line: &str, toks: &[Tok]) -> Vec<Chain> {
    let mut out = vec![];
    let mut i = 0;
    let is_seg = |t: &Tok| matches!(t.kind, "ident" | "upper" | "prim") || (t.kind == "kw" && t.text == "blanket");
    while i < toks.len() {
        let t = &toks[i];
        let prev_dot = i > 0 && toks[i - 1].kind == "op" && toks[i - 1].text == ".";
        if matches!(t.kind, "ident" | "upper") && !prev_dot {
            let mut segs = vec![t.text.clone()];
            let mut j = i;
            while j + 2 < toks.len() && toks[j + 1].kind == "op" && toks[j + 1].text == "." && is_seg(&toks[j + 2]) {
                segs.push(toks[j + 2].text.clone());
                j += 2;
            }
            let b0 = byte_of(line, t.col);
            let b1 = byte_of(line, toks[j].col) + toks[j].text.len();
            out.push(Chain { b0, b1, segs, tok: i });
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

fn ty_strip(t: &Ty) -> &Ty {
    let mut t = t;
    while let Ty::Ref(_, inner, _) = t {
        t = inner;
    }
    t
}

fn nid(v: &V) -> usize {
    match v {
        V::N(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn walk_nodes<'a>(v: &'a V, out: &mut Vec<&'a V>) {
    out.push(v);
    for c in children(v) {
        walk_nodes(c, out);
    }
}

// ------------------------------------------------------------ 解析器

struct Ref {
    si: usize,
    di: usize,
    line: usize,
    b0: usize,
    b1: usize,
    full: Vec<String>,
    short: bool,
    decl: bool,
}

struct Edit {
    b0: usize,
    b1: usize,
    text: String,
}

pub struct Engine<'a> {
    l: &'a Loaded,
    prog: Program,
    /// (si, di) → prog.defs の添字。
    pidx: Vec<Vec<usize>>,
    pkg: String,
    srcroot: String,
    pkg_mods: BTreeSet<String>,
    all_mods: HashSet<String>,
    recv: HashMap<(usize, usize, usize), (usize, Ty)>,
    toks: HashMap<(usize, usize), Vec<Tok>>,
}

enum Decision {
    Hit,
    Skip,
    Ambig,
}

impl<'a> Engine<'a> {
    pub fn new(l: &'a Loaded) -> Result<Engine<'a>, String> {
        for s in &l.pkg.sources {
            if let Some(e) = &s.error {
                return Err(format!("{}: {}: {} (fix the syntax error first)", crate::cli::rel(&s.path), e.code, e.msg));
            }
        }
        let (prog, user) = build_program(l);
        let mut pidx = vec![];
        let mut k = 0;
        for s in &l.pkg.sources {
            let mut v = vec![];
            for _ in &s.defs {
                v.push(user[k]);
                k += 1;
            }
            pidx.push(v);
        }
        let mut pkg_mods = BTreeSet::new();
        let mut all_mods: HashSet<String> = HashSet::new();
        for s in &l.pkg.sources {
            pkg_mods.insert(s.module.clone());
            all_mods.insert(s.module.clone());
        }
        for dp in &l.deps {
            for s in &dp.sources {
                all_mods.insert(s.module.clone());
            }
        }
        for s in &l.std {
            all_mods.insert(s.module.clone());
        }
        // 型の推論結果 (レシーバの型) を集める。
        let mut recv = HashMap::new();
        {
            let mut tk = TypeCk::new(&prog);
            for &i in &user {
                let d = &prog.defs[i];
                if (d.kw == "fn" && d.body.is_some()) || d.kw == "test" {
                    tk.check_def(i);
                }
            }
            for r in std::mem::take(&mut tk.recv_log) {
                recv.insert((r.writer, r.pos, r.seg), (r.field as usize, r.recv));
            }
        }
        Ok(Engine {
            l,
            prog,
            pidx,
            pkg: l.pkg.name.clone(),
            srcroot: norm(&l.pkg.root.join("src")),
            pkg_mods,
            all_mods,
            recv,
            toks: HashMap::new(),
        })
    }

    fn src(&self, si: usize) -> &'a crate::project::Source {
        let l: &'a Loaded = self.l;
        &l.pkg.sources[si]
    }

    fn line_toks(&mut self, si: usize, ln: usize) -> &Vec<Tok> {
        if !self.toks.contains_key(&(si, ln)) {
            let t = lex_line(&self.l.pkg.sources[si].lines[ln - 1], ln);
            self.toks.insert((si, ln), t);
        }
        &self.toks[&(si, ln)]
    }

    fn mod_prefix_len(&self, full: &[String], mods: &dyn Fn(&str) -> bool) -> Option<usize> {
        (1..=full.len()).rev().find(|&k| mods(&full[..k].join(".")))
    }

    // -------------------------------------------------------- 分類

    fn def_named(&self, name: &str) -> Option<&Def> {
        self.prog
            .funcs
            .get(name)
            .or(self.prog.sigs.get(name))
            .or(self.prog.consts.get(name))
            .or(self.prog.types.get(name))
            .map(|&i| &self.prog.defs[i])
            .or_else(|| self.prog.defs.iter().find(|d| d.kw == "test" && d.name == name))
    }

    fn classify(&self, old: &str) -> Option<Kind> {
        if self.all_mods.contains(old) {
            return Some(Kind::Module);
        }
        if let Some(d) = self.def_named(old) {
            let head = old.rsplit_once('.').map(|x| x.0).unwrap_or("");
            if d.kw == "fn" && self.prog.types.contains_key(head) {
                return Some(Kind::Method);
            }
            return Some(Kind::Plain);
        }
        let (head, last) = old.rsplit_once('.')?;
        let td = self.prog.types.get(head).map(|&i| &self.prog.defs[i])?;
        match td.kw.as_str() {
            "type" if td.fields.iter().any(|f| f.0 == last) => Some(Kind::Field),
            "enum" if td.variants.iter().any(|v| v.0 == last) => Some(Kind::Variant),
            "trait" if td.methods.iter().any(|m| m == last) => Some(Kind::TraitMethod),
            _ => None,
        }
    }

    fn make_op(&self, old: &str, new: &str) -> Result<Op, String> {
        let kind = self.classify(old).ok_or_else(|| format!("{} not found", old))?;
        let mut op = Op {
            kind,
            old: old.to_string(),
            new: new.to_string(),
            pairs: vec![(comps(old), comps(new))],
            member_old: old.rsplit('.').next().unwrap().to_string(),
            member_new: new.rsplit('.').next().unwrap().to_string(),
            owners: BTreeSet::new(),
            trait_fqn: String::new(),
        };
        let head = old.rsplit_once('.').map(|x| x.0).unwrap_or("").to_string();
        match kind {
            Kind::Method | Kind::Field | Kind::Variant => {
                op.owners.insert(head);
            }
            Kind::TraitMethod => {
                op.trait_fqn = head.clone();
                for d in &self.prog.defs {
                    if d.kw == "fn" && d.has_impls && d.impls_trait == head && d.name.rsplit('.').next() == Some(op.member_old.as_str()) {
                        if let Some((x, _)) = d.name.rsplit_once('.') {
                            op.owners.insert(x.to_string());
                        }
                        // トレイト側に置いた impl (3.2) は、呼び出し側の受け手の型 (X) でも当たる。
                        if let Some(x) = d.trait_side_owner() {
                            op.owners.insert(x);
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(op)
    }

    fn map_full(op: &Op, full: &[String]) -> Option<Vec<String>> {
        for (a, b) in &op.pairs {
            if is_prefix(a, full) {
                let mut v = b.clone();
                v.extend_from_slice(&full[a.len()..]);
                return Some(v);
            }
        }
        None
    }

    // -------------------------------------------------------- 検証 (改名のみ)

    fn validate(&self, op: &mut Op) -> Result<(), String> {
        let new = op.new.clone();
        let old = op.old.clone();
        let nc = comps(&new);
        let oc = comps(&old);
        let seg_ok = regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap();
        if nc.iter().any(|c| !seg_ok.is_match(c)) {
            return Err(format!("invalid new name: {}", new));
        }
        if nc.iter().any(|c| RESERVED_NAMES.contains(&c.as_str())) && op.kind != Kind::Method && op.kind != Kind::Field {
            return Err(format!("invalid new name: {} contains a reserved word", new));
        }
        if matches!(op.kind, Kind::Method | Kind::Field | Kind::TraitMethod) && RESERVED_NAMES.contains(&op.member_new.as_str()) {
            return Err(format!("invalid new name: {} is a reserved word", op.member_new));
        }
        if old == new {
            return Err("the new name is the same as the old name".into());
        }
        let belongs = |name: &str| name == self.pkg || name.starts_with(&format!("{}.", self.pkg));
        if !belongs(&old) {
            return Err(format!("{} is not a definition of this package (run the rename in the package that defines it)", old));
        }
        if !belongs(&new) {
            return Err(format!("the new name must start with the package name `{}.`", self.pkg));
        }
        let all_names: HashSet<String> = self.l.pkg.sources.iter().flat_map(|s| s.defs.iter()).filter(|d| d.kw != "import" && d.kw != "impl").map(|d| d.name.clone()).collect();
        let name_taken = |n: &str| all_names.contains(n) || self.pkg_mods.contains(n);
        match op.kind {
            Kind::Module => {
                if old == self.pkg {
                    return Err("the package root module cannot be renamed".into());
                }
                if new.starts_with(&format!("{}.", old)) {
                    return Err("a module cannot be moved into itself".into());
                }
                let mapped: Vec<(String, String)> = self
                    .pkg_mods
                    .iter()
                    .filter(|m| is_prefix(&oc, &comps(m)))
                    .map(|m| (m.clone(), Self::map_full(op, &comps(m)).unwrap().join(".")))
                    .collect();
                let moving: HashSet<&String> = mapped.iter().map(|x| &x.0).collect();
                for (_, nm) in &mapped {
                    if name_taken(nm) && !moving.contains(nm) {
                        return Err(format!("collision: {} already exists", nm));
                    }
                    let nc = comps(nm);
                    for k in 1..nc.len() {
                        if all_names.contains(&nc[..k].join(".")) {
                            return Err(format!("collision: {} is a definition, so it cannot also be a module prefix", nc[..k].join(".")));
                        }
                    }
                    let path = format!("{}/{}.ksnl", self.srcroot, nm.replace('.', "/"));
                    if std::path::Path::new(&path).exists() && !self.l.pkg.sources.iter().any(|s| s.path == path && moving.contains(&s.module)) {
                        return Err(format!("collision: the file {} already exists", crate::cli::rel(&path)));
                    }
                }
                let nm_set: HashSet<&String> = mapped.iter().map(|x| &x.1).collect();
                if nm_set.len() != mapped.len() {
                    return Err("collision: two modules would get the same name".into());
                }
            }
            Kind::Plain => {
                let d = self.def_named(&old).unwrap();
                if d.kw == "test" {
                    let (oh, _) = old.rsplit_once('.').unwrap();
                    let (nh, nl) = new.rsplit_once('.').ok_or("invalid new name")?;
                    if oh != nh && !all_names.contains(nh) {
                        return Err(format!("the test target {} does not exist", nh));
                    }
                    if !nl.starts_with("test_") {
                        return Err("a test name must start with `test_`".into());
                    }
                    if name_taken(&new) {
                        return Err(format!("collision with existing definition {}", new));
                    }
                } else {
                    if nc.len() < 2 {
                        return Err("a definition name must be <module>.<name>".into());
                    }
                    let nmod = nc[..nc.len() - 1].join(".");
                    if !self.pkg_mods.contains(&nmod) {
                        return Err(format!("the module {} does not exist (a definition can only be moved to an existing module of this package)", nmod));
                    }
                    if name_taken(&new) {
                        return Err(format!("collision with existing definition {}", new));
                    }
                    if all_names.iter().any(|n| n.starts_with(&format!("{}.", new))) {
                        return Err(format!("collision with existing definition {}.*", new));
                    }
                    // 移動先のメンバー名の衝突は、新しい名前が空いていれば起きない。
                    let omod = oc[..oc.len() - 1].join(".");
                    if omod != nmod && d.kw == "fn" && d.has_impls {
                        return Err("a trait-implementing method cannot be moved on its own".into());
                    }
                }
            }
            Kind::Method | Kind::Field | Kind::Variant | Kind::TraitMethod => {
                if comps(&new).len() != oc.len() || nc[..nc.len() - 1] != oc[..oc.len() - 1] {
                    return Err(format!("a member can only be renamed within its owner: {} -> {}.<name>", old, oc[..oc.len() - 1].join(".")));
                }
                let d = self.def_named(&old);
                if op.kind == Kind::Method {
                    if d.map_or(false, |d| d.has_impls) {
                        return Err(format!(
                            "{} implements a trait method (impls {}); rename the trait's required method instead (`ksn rename {}.{} {}.{}`)",
                            old,
                            d.unwrap().impls_trait,
                            d.unwrap().impls_trait,
                            op.member_old,
                            d.unwrap().impls_trait,
                            op.member_new
                        ));
                    }
                }
                let owners: Vec<String> = if op.kind == Kind::TraitMethod {
                    let mut o: Vec<String> = op.owners.iter().cloned().collect();
                    o.push(op.trait_fqn.clone());
                    o
                } else {
                    op.owners.iter().cloned().collect()
                };
                for o in &owners {
                    let nn = format!("{}.{}", o, op.member_new);
                    if name_taken(&nn) {
                        return Err(format!("collision with existing definition {}", nn));
                    }
                    if let Some(td) = self.prog.types.get(o).map(|&i| &self.prog.defs[i]) {
                        if td.fields.iter().any(|f| f.0 == op.member_new)
                            || td.variants.iter().any(|v| v.0 == op.member_new)
                            || (td.kw == "trait" && td.methods.iter().any(|m| *m == op.member_new))
                        {
                            return Err(format!("collision: {} already has a member named {}", o, op.member_new));
                        }
                    }
                }
                if op.kind == Kind::TraitMethod {
                    for x in &op.owners.clone() {
                        op.pairs.push((comps(&format!("{}.{}", x, op.member_old)), comps(&format!("{}.{}", x, op.member_new))));
                    }
                }
            }
        }
        // 呼び出し元を持つ (pub の) 定義の注意は、呼び出し側で出す。
        Ok(())
    }

    // -------------------------------------------------------- レシーバの判定

    fn decide_recv(&self, op: &Op, writer: usize, ty: Option<&Ty>, field: bool) -> Decision {
        let unknown = |this: &Self| -> Decision {
            // 型が決まらないとき: 名前がプログラム全体で一意なら、その呼び出しは対象のものである。
            let name = &op.member_old;
            let unique = match op.kind {
                Kind::Field => {
                    let mut owners: BTreeSet<&String> = BTreeSet::new();
                    for d in &this.prog.defs {
                        if d.kw == "type" && d.fields.iter().any(|f| &f.0 == name) {
                            owners.insert(&d.name);
                        }
                    }
                    owners.len() == 1 && owners.iter().all(|o| op.owners.contains(*o))
                }
                Kind::Method | Kind::TraitMethod => {
                    let mut fns: BTreeSet<String> = BTreeSet::new();
                    if let Some(v) = this.prog.method_idx.get(name) {
                        for &i in v {
                            fns.insert(this.prog.defs[i].name.clone());
                        }
                    }
                    let mut traits: BTreeSet<&String> = BTreeSet::new();
                    for d in &this.prog.defs {
                        if d.kw == "trait" && d.methods.iter().any(|m| m == name) {
                            traits.insert(&d.name);
                        }
                    }
                    let mine: BTreeSet<String> = op.owners.iter().map(|o| format!("{}.{}", o, name)).collect();
                    let fns_ok = fns.iter().all(|f| mine.contains(f));
                    let traits_ok = if op.kind == Kind::TraitMethod { traits.iter().all(|t| **t == op.trait_fqn) } else { traits.is_empty() };
                    fns_ok && traits_ok
                }
                _ => false,
            };
            if unique { Decision::Hit } else { Decision::Ambig }
        };
        let Some(ty) = ty else { return unknown(self) };
        let t = ty_strip(ty);
        match t {
            Ty::Named(n, _, _) => {
                if op.owners.contains(n) {
                    Decision::Hit
                } else {
                    Decision::Skip
                }
            }
            Ty::Prim(n) if op.owners.contains(n) => Decision::Hit,
            Ty::Unknown | Ty::Var(_) => unknown(self),
            Ty::Param(p) => {
                if op.kind != Kind::TraitMethod || field {
                    return Decision::Skip;
                }
                let d = &self.prog.defs[writer];
                let mut text = String::new();
                for g in &d.generics {
                    if &g.1 == p {
                        text.push_str(&g.2.dump());
                        text.push(' ');
                    }
                }
                if self.bound_reaches(&text, &op.trait_fqn) { Decision::Hit } else { Decision::Skip }
            }
            Ty::Dyn(tr, _, _) => {
                if op.kind != Kind::TraitMethod || field {
                    return Decision::Skip;
                }
                if self.bound_reaches(tr, &op.trait_fqn) { Decision::Hit } else { Decision::Skip }
            }
            _ => Decision::Skip,
        }
    }

    /// 境界の文字列 `text` が、トレイト `tr` (またはそれを `requires` するトレイト) を含むか。
    fn bound_reaches(&self, text: &str, tr: &str) -> bool {
        let mentions = |t: &str| text.match_indices(t).any(|(i, _)| boundary_ok(text, i, t.len()));
        if mentions(tr) {
            return true;
        }
        for d in &self.prog.defs {
            if d.kw == "trait" && mentions(&d.name) {
                let mut seen = HashSet::new();
                if self.requires_reach(&d.name, tr, &mut seen) {
                    return true;
                }
            }
        }
        false
    }

    fn requires_reach(&self, from: &str, tr: &str, seen: &mut HashSet<String>) -> bool {
        if from == tr {
            return true;
        }
        if !seen.insert(from.to_string()) {
            return false;
        }
        let Some(d) = self.prog.types.get(from).map(|&i| &self.prog.defs[i]) else { return false };
        d.requires.iter().any(|(n, _)| self.requires_reach(n, tr, seen))
    }

    // -------------------------------------------------------- 参照箇所の収集

    /// 全参照箇所。`ambig` に曖昧なメソッド・フィールド参照を入れる。
    fn collect(&mut self, op: &Op) -> (Vec<Ref>, Vec<Site>, BTreeMap<(usize, usize), Vec<Edit>>, Vec<Site>) {
        let mut refs: Vec<Ref> = vec![];
        let mut sites: Vec<Site> = vec![];
        let mut member_edits: BTreeMap<(usize, usize), Vec<Edit>> = BTreeMap::new();
        let mut ambig: Vec<Site> = vec![];
        let nsrc = self.l.pkg.sources.len();
        for si in 0..nsrc {
            let s = self.src(si);
            for (di, d) in s.defs.iter().enumerate() {
                if d.kw == "import" {
                    continue;
                }
                let last = if d.end_line > 0 { d.end_line } else { d.line };
                let locals: HashSet<String> = match &d.body {
                    Some(b) => {
                        let mut l: HashSet<String> = crate::infer::local_names(d, b).into_iter().collect();
                        l.insert("self".into());
                        l
                    }
                    None => HashSet::new(),
                };
                for ln in d.line..=last {
                    let text = &s.lines[ln - 1];
                    if text.trim_start().starts_with("//") || text.trim().is_empty() {
                        continue;
                    }
                    let is_edge = ln == d.line || ln == d.end_line;
                    let body_expr = !is_edge && matches!(d.kw.as_str(), "fn" | "test");
                    let toks = self.line_toks(si, ln).clone();
                    // const / static の初期化式 (`=` より後ろ) は式の文脈。
                    let eq = if ln == d.line && matches!(d.kw.as_str(), "const" | "static") {
                        toks.iter().position(|t| t.kind == "op" && t.text == "=")
                    } else {
                        None
                    };
                    for ch in chains_in(text, &toks) {
                        let decl = !(body_expr || eq.map_or(false, |e| ch.tok > e));
                        if !decl && locals.contains(&ch.segs[0]) {
                            continue;
                        }
                        if ch.segs.len() < 2 {
                            continue;
                        }
                        let mut full = ch.segs.clone();
                        let mut short = false;
                        // 仕様 0.10: ヘッダでも本体でも、import 済みモジュールの短縮形・FQN のどちらも書ける。
                        if let Some(m) = self.prog.imports.get(&s.path) {
                            if let Some(f) = m.get(&ch.segs[0]) {
                                full = comps(f);
                                full.extend_from_slice(&ch.segs[1..]);
                                short = true;
                            }
                        }
                        let all = &self.all_mods;
                        if self.mod_prefix_len(&full, &|m| all.contains(m)).is_none() {
                            continue;
                        }
                        refs.push(Ref { si, di, line: ln, b0: ch.b0, b1: ch.b1, full, short, decl });
                    }
                }
                // メンバーの宣言行
                if matches!(op.kind, Kind::Field | Kind::Variant | Kind::TraitMethod) && d.name == op.new_owner() {
                    let k = match (op.kind, d.kw.as_str()) {
                        (Kind::Field, "type") => d.fields.iter().position(|f| f.0 == op.member_old),
                        (Kind::Variant, "enum") => d.variants.iter().position(|v| v.0 == op.member_old),
                        (Kind::TraitMethod, "trait") => d.methods.iter().position(|m| *m == op.member_old),
                        _ => None,
                    };
                    if let Some(k) = k {
                        let ln = d.line + 1 + k;
                        let text = &s.lines[ln - 1];
                        let toks = self.line_toks(si, ln).clone();
                        let idx = if op.kind == Kind::TraitMethod { 1 } else { 0 };
                        if let Some(t) = toks.get(idx).filter(|t| t.text == op.member_old) {
                            let b0 = byte_of(text, t.col);
                            let kind = match op.kind {
                                Kind::Field => "field declaration",
                                Kind::Variant => "variant declaration",
                                _ => "required method declaration",
                            };
                            sites.push(Site {
                                file: s.path.clone(),
                                line: ln,
                                col: t.col,
                                end_col: t.col + t.text.chars().count(),
                                owner: d.name.clone(),
                                kind,
                                text: op.member_old.clone(),
                                new: Some(op.member_new.clone()),
                            });
                            member_edits.entry((si, ln)).or_default().push(Edit { b0, b1: b0 + t.text.len(), text: op.member_new.clone() });
                        }
                    }
                }
                // 式の中のメソッド呼び出し・フィールド参照・構造体リテラル・構造体パターン
                if matches!(op.kind, Kind::Method | Kind::Field | Kind::TraitMethod) {
                    let roots: Vec<&V> = match (&d.body, &d.init) {
                        (Some(b), _) => vec![b],
                        (None, Some(i)) if d.kw == "const" || d.kw == "static" => vec![i],
                        _ => vec![],
                    };
                    let pi = self.pidx[si][di];
                    for r in roots {
                        let mut all = vec![];
                        walk_nodes(r, &mut all);
                        let callee_pos: HashSet<usize> =
                            all.iter().filter(|n| nname(n) == "Call" && nname(&nf(n)[0]) == "Path").map(|n| crate::check::nline(&nf(n)[0])).collect();
                        for n in all {
                            let f = nf(n);
                            match nname(n) {
                                "MCall" | "Field" => {
                                    let is_field = nname(n) == "Field";
                                    if is_field != (op.kind == Kind::Field) || st(&f[1]) != op.member_old {
                                        continue;
                                    }
                                    let pos = nid(f.get(if is_field { 2 } else { 4 }).unwrap_or(&V::Nil));
                                    if pos == 0 {
                                        continue;
                                    }
                                    let (ln, col) = unpack_pos(pos);
                                    let ty = self.recv.get(&(pi, pos, usize::MAX)).map(|x| &x.1);
                                    self.member_site(op, s, d, pi, (ln, col), is_field, ty, &mut sites, &mut member_edits, &mut ambig);
                                }
                                "Path" => {
                                    // 局所変数から始まる連鎖 `x.f.m(..)`: 最後が呼び出しならメソッド、他はフィールド。
                                    let segs = crate::check::strs(&f[0]);
                                    if segs.len() < 2 {
                                        continue;
                                    }
                                    let pos = crate::check::nline(n);
                                    let is_callee = callee_pos.contains(&pos);
                                    let (ln, col) = unpack_pos(pos);
                                    for i in 1..segs.len() {
                                        let is_method = is_callee && i == segs.len() - 1;
                                        if is_method == (op.kind == Kind::Field) || segs[i] != op.member_old {
                                            continue;
                                        }
                                        let logged = self.recv.get(&(pi, pos, i));
                                        if logged.is_none() && !locals.contains(&segs[0]) {
                                            continue;
                                        }
                                        let toks = self.line_toks(si, ln).clone();
                                        let Some(st0) = toks.iter().position(|t| t.col == col) else { continue };
                                        let Some(t) = toks.get(st0 + 2 * i) else { continue };
                                        if t.text != op.member_old {
                                            continue;
                                        }
                                        let ty = self.recv.get(&(pi, pos, i)).map(|x| &x.1);
                                        self.member_site(op, s, d, pi, (ln, t.col), !is_method, ty, &mut sites, &mut member_edits, &mut ambig);
                                    }
                                }
                                "StructLit" | "PStruct" if op.kind == Kind::Field => {
                                    let segs: Vec<String> = if nname(n) == "StructLit" { st(&f[0]).split('.').map(String::from).collect() } else { crate::check::strs(&f[0]) };
                                    let full = self.prog.expand(&segs, &s.path);
                                    if !op.owners.contains(&full.join(".")) {
                                        continue;
                                    }
                                    let (ln0, col0) = unpack_pos(crate::check::nline(n));
                                    self.struct_fields(si, d, op, ln0, col0, nname(n) == "StructLit", &mut sites, &mut member_edits);
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        (refs, sites, member_edits, ambig)
    }


    /// メンバー参照 1 つ (位置 `at`) をレシーバの型で判定し、`sites` / `edits` / `ambig` に入れる。
    #[allow(clippy::too_many_arguments)]
    fn member_site(
        &self,
        op: &Op,
        s: &crate::project::Source,
        d: &Def,
        pi: usize,
        at: (usize, usize),
        is_field: bool,
        ty: Option<&Ty>,
        sites: &mut Vec<Site>,
        edits: &mut BTreeMap<(usize, usize), Vec<Edit>>,
        ambig: &mut Vec<Site>,
    ) {
        let (ln, col) = at;
        let text = &s.lines[ln - 1];
        let b0 = byte_of(text, col);
        if !text[b0..].starts_with(op.member_old.as_str()) {
            return;
        }
        let dec = self.decide_recv(op, pi, ty, is_field);
        let kind = match (&dec, is_field, ty.is_some()) {
            (Decision::Hit, true, _) => "field access",
            (Decision::Hit, false, true) => "method call",
            (Decision::Hit, false, false) => "method call (unique name)",
            (Decision::Ambig, true, _) => "field access (receiver type unknown)",
            _ => "method call (receiver type unknown)",
        };
        let site = Site {
            file: s.path.clone(),
            line: ln,
            col,
            end_col: col + op.member_old.chars().count(),
            owner: d.name.clone(),
            kind,
            text: op.member_old.clone(),
            new: Some(op.member_new.clone()),
        };
        let si = self.l.pkg.sources.iter().position(|x| x.path == s.path).unwrap();
        match dec {
            Decision::Hit => {
                edits.entry((si, ln)).or_default().push(Edit { b0, b1: b0 + op.member_old.len(), text: op.member_new.clone() });
                sites.push(site);
            }
            Decision::Ambig => ambig.push(Site { new: None, ..site }),
            Decision::Skip => {}
        }
    }

    /// 構造体リテラル・パターン (開始位置 `ln0:col0`) の `{ .. }` の直下にあるフィールド名。
    fn struct_fields(&mut self, si: usize, d: &Def, op: &Op, ln0: usize, col0: usize, lit: bool, sites: &mut Vec<Site>, edits: &mut BTreeMap<(usize, usize), Vec<Edit>>) {
        let s = self.src(si);
        let last = if d.end_line > 0 { d.end_line } else { d.line };
        let mut toks: Vec<(usize, Tok)> = vec![];
        for ln in ln0..=last {
            for t in self.line_toks(si, ln).clone() {
                toks.push((ln, t));
            }
        }
        let Some(start) = toks.iter().position(|(l, t)| *l == ln0 && t.col == col0) else { return };
        let mut i = start;
        while i < toks.len() && !(toks[i].1.kind == "op" && toks[i].1.text == "{") {
            i += 1;
        }
        let mut depth = 0i32;
        while i < toks.len() {
            let t = &toks[i].1;
            if t.kind == "op" {
                match t.text.as_str() {
                    "{" | "(" | "[" => depth += 1,
                    "}" | ")" | "]" => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if depth == 1 && t.text == op.member_old && matches!(t.kind, "ident" | "upper") {
                let prev_ok = i > 0 && toks[i - 1].1.kind == "op" && matches!(toks[i - 1].1.text.as_str(), "{" | ",");
                let next_ok = i + 1 < toks.len() && toks[i + 1].1.kind == "op" && toks[i + 1].1.text == ":";
                if prev_ok && next_ok {
                    let ln = toks[i].0;
                    let text = &s.lines[ln - 1];
                    let b0 = byte_of(text, t.col);
                    edits.entry((si, ln)).or_default().push(Edit { b0, b1: b0 + t.text.len(), text: op.member_new.clone() });
                    sites.push(Site {
                        file: s.path.clone(),
                        line: ln,
                        col: t.col,
                        end_col: t.col + t.text.chars().count(),
                        owner: d.name.clone(),
                        kind: if lit { "struct literal field" } else { "pattern field" },
                        text: op.member_old.clone(),
                        new: Some(op.member_new.clone()),
                    });
                }
            }
            i += 1;
        }
    }

    // -------------------------------------------------------- 参照一覧 (ksn refs)

    pub fn refs(&mut self, target: &str) -> Result<Vec<Site>, String> {
        let kind = self.classify(target).ok_or_else(|| format!("{} not found", target))?;
        let mut op = self.make_op(target, target)?;
        op.new = target.to_string();
        let _ = kind;
        let (refs, mut sites, _me, ambig) = self.collect(&op);
        let a = comps(target);
        for r in &refs {
            if !is_prefix(&a, &r.full) {
                continue;
            }
            let s = self.src(r.si);
            let d = &s.defs[r.di];
            let text = &s.lines[r.line - 1];
            let kind = self.ref_kind(r, d, &op);
            sites.push(Site {
                file: s.path.clone(),
                line: r.line,
                col: char_col(text, r.b0),
                end_col: char_col(text, r.b1),
                owner: d.name.clone(),
                kind,
                text: text[r.b0..r.b1].to_string(),
                new: None,
            });
        }
        self.import_sites(&op, &mut sites);
        sites.extend(ambig);
        self.doc_sites(&op, &mut sites);
        sites.sort_by(|a, b| (&a.file, a.line, a.col, a.kind).cmp(&(&b.file, b.line, b.col, b.kind)));
        sites.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.col == b.col && a.kind == b.kind);
        Ok(sites)
    }

    fn ref_kind(&self, r: &Ref, d: &Def, op: &Op) -> &'static str {
        let s = self.src(r.si);
        let in_edge = r.line == d.line || r.line == d.end_line;
        if r.decl {
            if r.line == d.end_line && d.end_line != d.line {
                return "end";
            }
            if r.line == d.line {
                let text = &s.lines[r.line - 1];
                let chain = &text[r.b0..r.b1];
                if d.kw == "impl" {
                    return "impl";
                }
                if chain == d.name || (d.kw == "test" && chain == d.name) {
                    return if d.kw == "test" { "test name" } else { "definition" };
                }
            }
            return "header";
        }
        let _ = (in_edge, op);
        if r.short {
            "short form"
        } else {
            "full name"
        }
    }

    fn import_sites(&self, op: &Op, sites: &mut Vec<Site>) {
        for s in &self.l.pkg.sources {
            for d in &s.defs {
                if d.kw == "import" && Self::map_full(op, &comps(&d.name)).is_some() {
                    let text = &s.lines[d.line - 1];
                    let b0 = text.find(&d.name).unwrap_or(0);
                    sites.push(Site {
                        file: s.path.clone(),
                        line: d.line,
                        col: char_col(text, b0),
                        end_col: char_col(text, b0 + d.name.len()),
                        owner: "(import)".into(),
                        kind: "import",
                        text: d.name.clone(),
                        new: None,
                    });
                }
            }
        }
    }

    fn doc_sites(&self, op: &Op, sites: &mut Vec<Site>) {
        for s in &self.l.pkg.sources {
            for (i, text) in s.lines.iter().enumerate() {
                if !text.trim_start().starts_with("//") {
                    continue;
                }
                for (b, _) in text.match_indices(op.old.as_str()) {
                    if boundary_ok(text, b, op.old.len()) {
                        sites.push(Site {
                            file: s.path.clone(),
                            line: i + 1,
                            col: char_col(text, b),
                            end_col: char_col(text, b + op.old.len()),
                            owner: "(comment)".into(),
                            kind: "doc comment (not rewritten)",
                            text: op.old.clone(),
                            new: None,
                        });
                    }
                }
            }
        }
    }

    // -------------------------------------------------------- 改名計画

    pub fn plan(&mut self, old: &str, new: &str) -> Result<Plan, String> {
        let mut op = self.make_op(old, new)?;
        self.validate(&mut op)?;
        let (refs, mut sites, mut edits, ambig) = self.collect(&op);
        if !ambig.is_empty() {
            let mut m = format!(
                "cannot rename {}: the receiver type of {} call site(s) could not be determined (nothing was changed). Add type annotations or fix these sites, then retry:",
                old,
                ambig.len()
            );
            let mut v = ambig.clone();
            v.sort_by(|a, b| (&a.file, a.line, a.col).cmp(&(&b.file, b.line, b.col)));
            for a in v {
                m.push_str(&format!("\n  {}:{}:{}: {} `.{}`", crate::cli::rel(&a.file), a.line, a.col, a.kind, a.text));
            }
            return Err(m);
        }
        let nsrc = self.l.pkg.sources.len();
        let oc = comps(old);
        // 新しいモジュール名の集合 (パッケージ内)
        let map_mod = |m: &str| -> String {
            if op.kind == Kind::Module {
                Self::map_full(&op, &comps(m)).map(|v| v.join(".")).unwrap_or_else(|| m.to_string())
            } else {
                m.to_string()
            }
        };
        let new_pkg_mods: BTreeSet<String> = self.pkg_mods.iter().map(|m| map_mod(m)).collect();
        let new_all: HashSet<String> = {
            let mut s: HashSet<String> = self.all_mods.iter().filter(|m| !self.pkg_mods.contains(*m)).cloned().collect();
            s.extend(new_pkg_mods.iter().cloned());
            s
        };
        // 各定義の移動先 (新しいモジュール)
        let mut dest_mod: Vec<Vec<String>> = vec![];
        for si in 0..nsrc {
            let s = self.src(si);
            let mut v = vec![];
            for d in &s.defs {
                v.push(map_mod(&s.module));
                let _ = d;
            }
            dest_mod.push(v);
        }
        if op.kind == Kind::Plain {
            let d0 = self.def_named(old).unwrap().clone();
            if d0.kw != "test" {
                let nmod = new.rsplit_once('.').unwrap().0.to_string();
                let omod = d0.module.clone();
                if nmod != omod {
                    for si in 0..nsrc {
                        let s = self.src(si);
                        if s.module != omod {
                            continue;
                        }
                        for (di, d) in s.defs.iter().enumerate() {
                            let moves = if d.kw == "impl" {
                                self.impl_belongs(d, old)
                            } else {
                                d.kw != "import" && (d.name == old || d.name.starts_with(&format!("{}.", old)))
                            };
                            if moves {
                                dest_mod[si][di] = nmod.clone();
                            }
                        }
                    }
                }
            }
        }
        let path_of_mod = |m: &str| -> String {
            if let Some(s) = self.l.pkg.sources.iter().find(|s| s.module == m) {
                return s.path.clone();
            }
            format!("{}/{}.ksnl", self.srcroot, m.replace('.', "/"))
        };
        // 参照ごとの新しい綴りを決める。
        // 1. 各ファイルが必要とする import (移動先ファイル → モジュール集合)
        let mut need: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut used_old: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
        struct Resolved {
            idx: usize,
            nf: Vec<String>,
            nk: usize,
            dm: String,
            x_old_is_own: bool,
        }
        let mut rs: Vec<Resolved> = vec![];
        for (idx, r) in refs.iter().enumerate() {
            let s = self.src(r.si);
            let nfull = Self::map_full(&op, &r.full).unwrap_or_else(|| r.full.clone());
            let dm = dest_mod[r.si][r.di].clone();
            let dest = path_of_mod(&dm);
            let allm = &new_all;
            let Some(nk) = self.mod_prefix_len(&nfull, &|m| allm.contains(m)) else {
                return Err(format!("internal: no module for {}", nfull.join(".")));
            };
            let ok = self.mod_prefix_len(&r.full, &|m| self.all_mods.contains(m)).unwrap();
            let x_old = r.full[..ok].join(".");
            if r.short {
                used_old.entry(r.si).or_default().insert(x_old.clone());
            }
            let x_new = nfull[..nk].join(".");
            if x_new != dm && (r.short || (!r.decl && x_old == s.module)) {
                need.entry(dest.clone()).or_default().insert(x_new);
            }
            rs.push(Resolved { idx, nf: nfull, nk, dm, x_old_is_own: x_old == s.module });
        }
        // 古い import のうち、参照が検出されなかったもの (使われ方が分からないので残す)
        let mut old_imports: BTreeMap<usize, Vec<(String, usize)>> = BTreeMap::new();
        for si in 0..nsrc {
            let s = self.src(si);
            for d in &s.defs {
                if d.kw == "import" {
                    old_imports.entry(si).or_default().push((d.name.clone(), d.line));
                }
            }
        }
        for (si, imps) in &old_imports {
            let s = self.src(*si);
            let dest = path_of_mod(&map_mod(&s.module));
            for (m, _) in imps {
                let used = used_old.get(si).map_or(false, |u| u.contains(m));
                if !used {
                    let nm = map_mod(m);
                    if nm != map_mod(&s.module) {
                        need.entry(dest.clone()).or_default().insert(nm);
                    }
                }
            }
        }
        // 末尾名の衝突
        for (dest, mods) in &need {
            let mut tails: HashMap<&str, &String> = HashMap::new();
            for m in mods {
                let t = m.rsplit('.').next().unwrap();
                if let Some(o) = tails.insert(t, m) {
                    return Err(format!(
                        "ambiguous short name: {} would import both {} and {} as `{}` (imports of two modules with the same last name are not allowed; nothing was changed)",
                        crate::cli::rel(dest),
                        o,
                        m,
                        t
                    ));
                }
            }
        }
        // 2. 最終の綴り
        for r in &rs {
            let rf = &refs[r.idx];
            let s = self.src(rf.si);
            let text = &s.lines[rf.line - 1];
            let old_text = &text[rf.b0..rf.b1];
            // 仕様 0.10: 書かれた形を保つ。短縮形は短縮形のまま (自モジュールになるときだけ FQN)、
            // FQN は FQN のまま。ただし同一モジュールの定義を指していた本体の FQN が、
            // 別モジュールを指すようになったときは、import を足して短縮形にする (3.3 の正規形)。
            let new_text = {
                let x = r.nf[..r.nk].join(".");
                let short_ok = rf.short || (!rf.decl && r.x_old_is_own);
                if short_ok && x != r.dm && r.nf.len() > r.nk {
                    format!("{}.{}", r.nf[r.nk - 1], r.nf[r.nk..].join("."))
                } else {
                    r.nf.join(".")
                }
            };
            if new_text != old_text {
                edits.entry((rf.si, rf.line)).or_default().push(Edit { b0: rf.b0, b1: rf.b1, text: new_text.clone() });
            }
            if is_prefix(&oc, &rf.full) {
                let d = &s.defs[rf.di];
                let kind = self.ref_kind(rf, d, &op);
                sites.push(Site {
                    file: s.path.clone(),
                    line: rf.line,
                    col: char_col(text, rf.b0),
                    end_col: char_col(text, rf.b1),
                    owner: d.name.clone(),
                    kind,
                    text: old_text.to_string(),
                    new: Some(new_text),
                });
            }
        }
        // 3. ファイルの組み立て
        let mut notes = vec![];
        self.doc_sites(&op, &mut sites);
        for st_ in sites.iter().filter(|s| s.new.is_none()) {
            notes.push(format!("{}:{}: doc comment mentions {} (not rewritten)", crate::cli::rel(&st_.file), st_.line, st_.text));
        }
        let mut chunks_in: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
        let mut file_lines: Vec<Vec<String>> = vec![];
        let mut removed: Vec<Vec<bool>> = vec![];
        let mut touched: BTreeSet<usize> = BTreeSet::new();
        for si in 0..nsrc {
            let s = self.src(si);
            let mut lines: Vec<String> = s.lines.clone();
            if let Some(es) = edits.keys().filter(|k| k.0 == si).map(|k| *k).collect::<Vec<_>>().first() {
                let _ = es;
            }
            let keys: Vec<(usize, usize)> = edits.keys().filter(|k| k.0 == si).cloned().collect();
            for k in keys {
                let mut es = edits.remove(&k).unwrap();
                es.sort_by(|a, b| b.b0.cmp(&a.b0));
                es.dedup_by(|a, b| a.b0 == b.b0 && a.b1 == b.b1);
                let line = &mut lines[k.1 - 1];
                for e in es {
                    line.replace_range(e.b0..e.b1, &e.text);
                }
                touched.insert(si);
            }
            file_lines.push(lines);
            removed.push(vec![false; s.lines.len()]);
        }
        // 移動する定義のかたまり (コメント行から末尾まで)
        for si in 0..nsrc {
            let s = self.src(si);
            let own_dest = path_of_mod(&map_mod(&s.module));
            for (di, d) in s.defs.iter().enumerate() {
                if d.kw == "import" {
                    continue;
                }
                let dest = path_of_mod(&dest_mod[si][di]);
                if dest == own_dest {
                    continue;
                }
                let mut st0 = d.line - 1;
                while st0 > 0 && s.lines[st0 - 1].starts_with("//") {
                    st0 -= 1;
                }
                let e = if d.end_line > 0 { d.end_line } else { d.line };
                let chunk: Vec<String> = file_lines[si][st0..e].to_vec();
                for k in st0..e {
                    removed[si][k] = true;
                }
                chunks_in.entry(dest).or_default().push(chunk);
                touched.insert(si);
            }
        }
        // 出力するファイル
        let mut out_files: BTreeMap<String, (Option<usize>, Vec<String>)> = BTreeMap::new();
        for si in 0..nsrc {
            let s = self.src(si);
            let dest = path_of_mod(&map_mod(&s.module));
            let mut keep: Vec<String> = vec![];
            for (k, l) in file_lines[si].iter().enumerate() {
                if !removed[si][k] {
                    keep.push(l.clone());
                }
            }
            out_files.insert(dest, (Some(si), keep));
        }
        let mut changes: Vec<FileChange> = vec![];
        let dests: Vec<String> = out_files.keys().cloned().collect();
        for dest in dests {
            let (si, _) = out_files.remove(&dest).unwrap();
            let si = si.unwrap();
            let s = self.src(si);
            let incoming = chunks_in.remove(&dest).unwrap_or_default();
            let imports_new: BTreeSet<String> = need.get(&dest).cloned().unwrap_or_default();
            let old_set: BTreeSet<String> = old_imports.get(&si).map(|v| v.iter().map(|x| x.0.clone()).collect()).unwrap_or_default();
            let moved_file = dest != s.path;
            let imports_changed = imports_new != old_set;
            let edits_changed = file_lines[si] != s.lines;
            let moved_out = removed[si].iter().any(|&b| b);
            if !(edits_changed || moved_out || !incoming.is_empty() || imports_changed || moved_file) {
                continue;
            }
            // import 行は全部作り直す (整形が並べ替える)。
            let import_lines: HashSet<usize> = s.defs.iter().filter(|d| d.kw == "import").map(|d| d.line).collect();
            let mut kept: Vec<String> = vec![];
            for (k, l) in file_lines[si].iter().enumerate() {
                if removed[si][k] || import_lines.contains(&(k + 1)) {
                    continue;
                }
                kept.push(l.clone());
            }
            while kept.first().map_or(false, |l| l.is_empty()) {
                kept.remove(0);
            }
            let mut lines: Vec<String> = imports_new.iter().map(|m| format!("import {}", m)).collect();
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.extend(kept);
            for c in incoming {
                lines.push(String::new());
                lines.extend(c);
            }
            let joined = lines.join("\n");
            let joined = if joined.ends_with('\n') { joined } else { format!("{}\n", joined) };
            let formatted = crate::fmt::format_source_with(&joined, &dest, false);
            if formatted == s.text && dest == s.path {
                continue;
            }
            changes.push(FileChange { old_path: Some(s.path.clone()), new_path: dest, old_text: s.text.clone(), new_text: formatted });
        }
        if !chunks_in.is_empty() {
            return Err("internal: a definition has no destination file".into());
        }
        // kasane.toml
        let manifest = self.manifest_plan(&op);
        sites.sort_by(|a, b| (&a.file, a.line, a.col, a.kind).cmp(&(&b.file, b.line, b.col, b.kind)));
        let _ = sites;
        if op.kind == Kind::Plain || op.kind == Kind::Module {
            let pubdef = self.def_named(old).map_or(false, |d| d.header.trim_end().ends_with(" pub"));
            if pubdef {
                notes.push(format!("{} is `pub`: packages that depend on this package are not updated", old));
            }
        }
        Ok(Plan { changes, manifest, notes })
    }

    /// `impl` 行が定義 `a` に付随するか (対象の型が `a`、または `a` がトレイトで対象が基本型)。
    fn impl_belongs(&self, d: &Def, a: &str) -> bool {
        let h = &d.header;
        let Some((lhs, rhs)) = h.split_once(" for ") else { return false };
        let rhs = rhs.trim();
        let expanded: String = {
            let first: String = rhs.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            match self.prog.imports.get(&d.file).and_then(|m| m.get(&first)) {
                Some(full) if rhs[first.len()..].starts_with('.') => format!("{}{}", full, &rhs[first.len()..]),
                _ => rhs.to_string(),
            }
        };
        let rhs = expanded.as_str();
        if rhs.starts_with(a) && boundary_ok(rhs, 0, a.len()) {
            return true;
        }
        let prim = rhs.chars().next().map_or(false, |c| c.is_ascii_lowercase()) && !rhs.contains('.');
        let lhs_tr = lhs.rsplit(|c: char| c == ']' || c == ' ').next().unwrap_or("");
        prim && lhs_tr == a
    }

    fn manifest_plan(&self, op: &Op) -> Option<(String, String, String)> {
        let path = norm(&self.l.pkg.root.join("kasane.toml"));
        let text = crate::project::read_text(&path)?;
        let mut out = String::new();
        let mut table = String::new();
        let mut in_modules = false;
        for line in text.split_inclusive('\n') {
            let t = line.trim();
            if t.starts_with('[') && t.ends_with(']') && !t.starts_with("[[") {
                table = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
                in_modules = false;
            }
            let target = (table == "package" && t.starts_with("entry")) || (table == "unsafe" && (t.starts_with("modules") || in_modules));
            if table == "unsafe" && t.starts_with("modules") {
                in_modules = !t.contains(']') || t.ends_with('[');
            }
            if table == "unsafe" && in_modules && t.contains(']') && !t.starts_with("modules") {
                in_modules = false;
            }
            if target {
                let mut l2 = String::new();
                let mut rest = line;
                while let Some(a) = rest.find('"') {
                    let Some(b) = rest[a + 1..].find('"') else { break };
                    let val = &rest[a + 1..a + 1 + b];
                    let mapped = if table == "unsafe" || op.kind == Kind::Module || true { Self::map_full(op, &comps(val)).map(|v| v.join(".")) } else { None };
                    l2.push_str(&rest[..a + 1]);
                    l2.push_str(mapped.as_deref().unwrap_or(val));
                    l2.push('"');
                    rest = &rest[a + 1 + b + 1..];
                }
                l2.push_str(rest);
                out.push_str(&l2);
            } else {
                out.push_str(line);
            }
        }
        if out != text {
            Some((path, text, out))
        } else {
            None
        }
    }
}

impl Op {
    /// メンバーの宣言を持つ定義の名前 (`T.f` の `T`)。
    fn new_owner(&self) -> String {
        self.old.rsplit_once('.').map(|x| x.0.to_string()).unwrap_or_default()
    }
}

// ------------------------------------------------------------ 公開関数

pub fn refs(l: &Loaded, target: &str) -> Result<Vec<Site>, String> {
    Engine::new(l)?.refs(target)
}

pub fn plan_rename(l: &Loaded, old: &str, new: &str) -> Result<Plan, String> {
    let mut plan = Engine::new(l)?.plan(old, new)?;
    if old.rsplit_once('.').map(|x| x.0) != new.rsplit_once('.').map(|x| x.0) {
        fix_visibility(l, &mut plan);
    }
    Ok(plan)
}

// ------------------------------------------------------------ 移動に伴う `pub` の補い

/// 定義 `name` のヘッダ行 (`fn a.b` / `type a.B` ...) の添字。
fn header_line(lines: &[String], name: &str) -> Option<usize> {
    lines.iter().position(|l| {
        ["fn", "type", "enum", "trait", "const", "static", "extern"].iter().any(|kw| {
            l.strip_prefix(kw)
                .and_then(|r| r.strip_prefix(' '))
                .and_then(|r| r.strip_prefix(name))
                .map_or(false, |r| r.is_empty() || r.starts_with([' ', '(', '[', ':']))
        })
    })
}

/// 計画を (上書きとして) 適用して `ksn check` を走らせ、移動で新しく生じた「別モジュールの非 `pub` の
/// 参照」(E1119 / E1122) を、参照される側に `pub` を足して解消する。足した定義は `plan.notes` に載せる。
/// 最小限にするため、計画の前から出ている診断と、足しても直らないものには触れない。
fn fix_visibility(l: &Loaded, plan: &mut Plan) {
    use crate::project::{overlay_clear, overlay_hide, overlay_set};
    let root = l.pkg.root.clone();
    let before: HashSet<String> = crate::cli::check_error_lines(&root).iter().map(|x| diag_msg(x)).collect();
    let mut added: Vec<String> = vec![];
    for _ in 0..16 {
        overlay_clear();
        for c in &plan.changes {
            if let Some(o) = &c.old_path {
                if *o != c.new_path {
                    overlay_hide(o);
                }
            }
            overlay_set(&c.new_path, Some(c.new_text.clone()));
        }
        if let Some((p, _, n)) = &plan.manifest {
            overlay_set(p, Some(n.clone()));
        }
        let after = crate::cli::check_error_lines(&root);
        overlay_clear();
        // (種別, 名前, フィールド名)
        let mut want: Vec<(String, Option<String>)> = vec![];
        for a in &after {
            let m = diag_msg(a);
            if before.contains(&m) || !(m.starts_with("E1119") || m.starts_with("E1122")) {
                continue;
            }
            let body = m[6..].trim_start();
            if let Some(r) = body.strip_prefix("field ") {
                // field F of T is private ...
                let mut it = r.split_whitespace();
                let (f, _, t) = (it.next(), it.next(), it.next());
                if let (Some(f), Some(t)) = (f, t) {
                    want.push((t.to_string(), Some(f.to_string())));
                }
            } else {
                let r = body.strip_prefix("method ").unwrap_or(body);
                if let Some(n) = r.split_whitespace().next() {
                    want.push((n.to_string(), None));
                }
            }
        }
        if want.is_empty() {
            break;
        }
        let mut progressed = false;
        for (name, field) in want {
            progressed |= add_pub(l, plan, &name, field.as_deref(), &mut added);
        }
        if !progressed {
            break;
        }
    }
    for a in added {
        plan.notes.push(a);
    }
}

fn diag_msg(line: &str) -> String {
    // "file:line: CODE: message" -> "CODE: message"
    match line.splitn(3, ": ").collect::<Vec<_>>().as_slice() {
        [_, rest @ ..] if rest.len() == 2 => format!("{}: {}", rest[0], rest[1]),
        _ => line.to_string(),
    }
}

/// 計画の中 (なければディスク) の、パッケージのソースから定義 `name` を探して `pub` を足す。
/// `field` があれば `name` の型のそのフィールド行に足す。`impls` メソッドは、非 `pub` の型とトレイトに足す。
fn add_pub(l: &Loaded, plan: &mut Plan, name: &str, field: Option<&str>, added: &mut Vec<String>) -> bool {
    // 現在の本文 (計画の新しい本文を優先) を持つファイルの一覧。
    let mut files: Vec<(String, String)> = vec![];
    for s in &l.pkg.sources {
        let cur = plan.changes.iter().find(|c| c.old_path.as_deref() == Some(s.path.as_str())).map(|c| c.new_text.clone());
        files.push((s.path.clone(), cur.unwrap_or_else(|| s.text.clone())));
    }
    for c in &plan.changes {
        if c.old_path.is_none() || !l.pkg.sources.iter().any(|s| Some(&s.path) == c.old_path.as_ref()) {
            files.push((c.new_path.clone(), c.new_text.clone()));
        }
    }
    // 移動先が新しいファイルのときは、変更の新パスで探す。
    for c in &plan.changes {
        if !files.iter().any(|f| f.0 == c.new_path) {
            files.push((c.new_path.clone(), c.new_text.clone()));
        }
    }
    let mut targets: Vec<(String, Option<String>)> = vec![(name.to_string(), field.map(String::from))];
    // `impls` メソッドなら、可視性を決める型とトレイトのほうに足す。
    if field.is_none() {
        for (_, t) in &files {
            let lines: Vec<String> = t.lines().map(String::from).collect();
            if let Some(i) = header_line(&lines, name) {
                if lines[i].starts_with("fn ") {
                    if let Some(tr) = lines[i].split(" impls ").nth(1) {
                        let tr: String = tr.split([' ', '<']).next().unwrap_or("").to_string();
                        targets.clear();
                        if let Some((ty, _)) = name.rsplit_once('.') {
                            targets.push((ty.to_string(), None));
                        }
                        targets.push((tr, None));
                    }
                }
                break;
            }
        }
    }
    let mut progressed = false;
    for (tn, fld) in targets {
        for (path, text) in &files {
            let mut lines: Vec<String> = text.lines().map(String::from).collect();
            let Some(i) = header_line(&lines, &tn) else { continue };
            let k = match &fld {
                None => i,
                Some(f) => {
                    let end = format!("end {}", tn);
                    let Some(e) = lines[i..].iter().position(|x| *x == end) else { continue };
                    let Some(k) = lines[i..i + e].iter().position(|x| x.trim_start().starts_with(&format!("{}:", f))) else { continue };
                    i + k
                }
            };
            if lines[k].ends_with(" pub") {
                break;
            }
            lines[k].push_str(" pub");
            let new_text = format!("{}\n", lines.join("\n"));
            match plan.changes.iter_mut().find(|c| c.new_path == *path) {
                Some(c) => c.new_text = new_text,
                None => {
                    let old_text = text.clone();
                    plan.changes.push(FileChange { old_path: Some(path.clone()), new_path: path.clone(), old_text, new_text });
                }
            }
            added.push(match &fld {
                Some(f) => format!("made field {} of {} `pub` (it is now referenced from another module)", f, tn),
                None => format!("made {} `pub` (it is now referenced from another module)", tn),
            });
            progressed = true;
            break;
        }
    }
    progressed
}

// ------------------------------------------------------------ 検証と適用

/// `.ksnh` の変更ごとに、横の `.ksnl` (コメントを外したもの) の変更を足す。`.ksnh` が正で、`ksn check` が一致を確かめる (E9050)。
/// `.ksnh` から移るファイル、またはコメントを持つ新しいファイルは、`.ksnh` として書く (`.ksnl` にコメントは置けない)。
pub(crate) fn with_ksnl(changes: &[FileChange]) -> Vec<FileChange> {
    let ksnl_of = |p: &str| format!("{}.ksnl", p.trim_end_matches(".ksnh"));
    let ksnh_of = |p: &str| format!("{}.ksnh", p.trim_end_matches(".ksnl").trim_end_matches(".ksnh"));
    let mut out = vec![];
    for c in changes {
        let from_ksnh = c.old_path.as_deref().map_or(false, |o| o.ends_with(".ksnh"));
        let has_comment = crate::ksnh::strip(&c.new_text) != c.new_text;
        let as_ksnh = c.new_path.ends_with(".ksnh") || (c.new_path.ends_with(".ksnl") && (from_ksnh || has_comment));
        if !as_ksnh {
            out.push(FileChange { old_path: c.old_path.clone(), new_path: c.new_path.clone(), old_text: c.old_text.clone(), new_text: c.new_text.clone() });
            continue;
        }
        let new_path = ksnh_of(&c.new_path);
        out.push(FileChange { old_path: c.old_path.clone(), new_path: new_path.clone(), old_text: c.old_text.clone(), new_text: c.new_text.clone() });
        out.push(FileChange {
            old_path: c.old_path.as_deref().filter(|o| o.ends_with(".ksnh")).map(ksnl_of),
            new_path: ksnl_of(&new_path),
            old_text: c.old_path.as_deref().filter(|o| o.ends_with(".ksnh")).and_then(|o| crate::project::read_text(&ksnl_of(o))).unwrap_or_default(),
            new_text: crate::ksnh::strip(&c.new_text),
        });
    }
    out
}

/// 計画をディスクに触れずに (上書きとして) 適用し、`ksn check` の診断が増えないことを確かめる。
pub fn verify(root: &std::path::Path, plan: &Plan) -> Result<(), String> {
    let changes = with_ksnl(&plan.changes);
    use crate::project::{overlay_clear, overlay_hide, overlay_set};
    let before = crate::cli::check_error_lines(root);
    overlay_clear();
    for c in &changes {
        if let Some(o) = &c.old_path {
            if *o != c.new_path {
                overlay_hide(o);
            }
        }
        overlay_set(&c.new_path, Some(c.new_text.clone()));
    }
    if let Some((p, _, n)) = &plan.manifest {
        overlay_set(p, Some(n.clone()));
    }
    let after = crate::cli::check_error_lines(root);
    overlay_clear();
    if after.len() > before.len() {
        let old: HashSet<&String> = before.iter().collect();
        let mut m = String::from("the rename would make `ksn check` fail (nothing was changed):");
        for a in after.iter().filter(|a| !old.contains(a)).take(8) {
            m.push_str(&format!("\n  {}", a));
        }
        return Err(m);
    }
    Ok(())
}

/// 計画をディスクに書く。途中で失敗したら元に戻す。
pub fn apply(plan: &Plan) -> Result<(), String> {
    let changes = with_ksnl(&plan.changes);
    let mut undo: Vec<(String, Option<Vec<u8>>)> = vec![];
    let touch = |p: &str, undo: &mut Vec<(String, Option<Vec<u8>>)>| {
        if !undo.iter().any(|u| u.0 == p) {
            undo.push((p.to_string(), std::fs::read(p).ok()));
        }
    };
    let mut res: Result<(), String> = Ok(());
    let mut removed_dirs: Vec<String> = vec![];
    for c in &changes {
        touch(&c.new_path, &mut undo);
        if let Some(o) = &c.old_path {
            touch(o, &mut undo);
        }
    }
    if let Some((p, _, _)) = &plan.manifest {
        touch(p, &mut undo);
    }
    'outer: {
        for c in &changes {
            if let Some(dir) = std::path::Path::new(&c.new_path).parent() {
                if let Err(e) = std::fs::create_dir_all(dir) {
                    res = Err(format!("cannot create {}: {}", crate::cli::rel(&norm(dir)), e));
                    break 'outer;
                }
            }
            if let Err(e) = std::fs::write(&c.new_path, &c.new_text) {
                res = Err(format!("cannot write {}: {}", crate::cli::rel(&c.new_path), e));
                break 'outer;
            }
        }
        for c in &changes {
            if let Some(o) = &c.old_path {
                if *o != c.new_path && !changes.iter().any(|d| d.new_path == *o) {
                    if let Err(e) = std::fs::remove_file(o) {
                        res = Err(format!("cannot remove {}: {}", crate::cli::rel(o), e));
                        break 'outer;
                    }
                    if let Some(dir) = std::path::Path::new(o).parent() {
                        removed_dirs.push(norm(dir));
                    }
                }
            }
        }
        if let Some((p, _, n)) = &plan.manifest {
            if let Err(e) = std::fs::write(p, n) {
                res = Err(format!("cannot write {}: {}", crate::cli::rel(p), e));
                break 'outer;
            }
        }
    }
    if let Err(m) = res {
        for (p, bytes) in undo.iter().rev() {
            match bytes {
                Some(b) => {
                    let _ = std::fs::write(p, b);
                }
                None => {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
        return Err(format!("{} (the changes were rolled back)", m));
    }
    // 空になった旧ディレクトリを片付ける (src 自体は残す)。
    for d in removed_dirs {
        let mut dir = std::path::PathBuf::from(&d);
        while dir.file_name().map_or(false, |n| n != "src") && std::fs::remove_dir(&dir).is_ok() {
            if !dir.pop() {
                break;
            }
        }
    }
    Ok(())
}
