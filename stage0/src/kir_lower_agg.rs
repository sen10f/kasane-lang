//! 集成体 (構造体・タプル・列挙型) の下ろし (設計_KIR.md 2.3、段階 1 の続き)。
//!
//! `kir_lower.rs` の子モジュールとして、その非公開の部品 (`Lw` の操作) を使う。
//!
//! - 型の表 (`AggTab`) は `lower_user` が 1 つ作り、全関数で共有する。添字は `Module::aggs` と同じ。
//! - 配置は clif.rs の `layout` / `enum_layout` と同じ規則で求める: 大きさと整列は clif と同じ、
//!   構造体・タプルのフィールドは宣言の順に、列挙型は先頭 4 バイトのタグ (i32、バリアントの添字) の後ろに、
//!   バリアントのペイロードを重ねて置く (整列は 4 以上)。
//! - 値は不変。フィールドの書き換えは新しい値 (`setfield`) を作り、変数へ代入し直す。
//! - `match` は変種の試験 (タグの比較) を順に並べた連鎖に下ろし、束縛は試験の後の `field` で作る。

use std::collections::{HashMap, HashSet};

use crate::check::{items, nf, nline, nname, st, strs};
use crate::kir::{align_of, size_of, AggDef, AggKind, Field, Op, Term, Ty, VId, Variant};
use crate::parser::V;
use crate::program::{Program, Target as Tgt};
use crate::types::{from_v, Ty as Src};

use super::{conv, parse_int, show, Lw, St, LR};

/// 集成体の型表。`lower_user` が 1 つ作り、全関数で共有する。
#[derive(Default)]
pub struct AggTab {
    pub defs: Vec<AggDef>,
    /// 型の名前 (構造体・列挙型は FQN、タプルは `(i32,bool)`) → 添字。
    by_name: HashMap<String, u32>,
    /// 組み立て中の型 (再帰的な型を見つけるため)。
    busy: HashSet<String>,
    /// 利用者のパッケージの定義 (`Program::defs` の添字)。標準の型は clif が特別扱いする配置を持つので、ここに入るものだけ扱う。
    user: HashSet<usize>,
}

fn align_up(n: u32, a: u32) -> u32 {
    (n + a - 1) / a * a
}

/// 構造体・タプル: フィールドを宣言の順に並べる。大きさは `max(末尾, 1)` を整列に切り上げる (clif と同じ)。
fn layout_fields(defs: &[AggDef], fs: &[(String, Ty)]) -> (u32, u32, Vec<Field>) {
    let mut off = 0u32;
    let mut align = 1u32;
    let mut out = vec![];
    for (name, t) in fs {
        let (sz, al) = (size_of(defs, *t), align_of(defs, *t));
        off = align_up(off, al);
        out.push(Field { name: name.clone(), ty: *t, off });
        off += sz;
        align = align.max(al);
    }
    (align_up(off.max(1), align), align, out)
}

/// 列挙型: 整列の最大 (4 以上) を求めてから、タグの後ろ (`start`) にバリアントごとのペイロードを重ねる。
fn layout_enum(defs: &[AggDef], vs: &[(String, Vec<Ty>)]) -> (u32, u32, Vec<Variant>) {
    let mut max_align = 4u32;
    let mut sizes = vec![];
    let mut rel: Vec<Vec<(Ty, u32)>> = vec![];
    for (_, tys) in vs {
        let mut cur = 0u32;
        let mut fs = vec![];
        for t in tys {
            let (sz, al) = (size_of(defs, *t), align_of(defs, *t));
            cur = align_up(cur, al);
            fs.push((*t, cur));
            cur += sz;
            max_align = max_align.max(al);
        }
        sizes.push(cur);
        rel.push(fs);
    }
    let start = align_up(4, max_align);
    let max_payload = sizes.iter().copied().max().unwrap_or(0);
    let variants = vs
        .iter()
        .zip(rel)
        .map(|((vn, _), fs)| Variant {
            name: vn.clone(),
            fields: fs.into_iter().map(|(ty, off)| Field { name: String::new(), ty, off: start + off }).collect(),
        })
        .collect();
    (align_up(start + max_payload, max_align), max_align, variants)
}

impl AggTab {
    /// `user` は利用者のパッケージの定義の添字 (`lower_user` の引数と同じ)。
    pub fn new(user: &[usize]) -> AggTab {
        AggTab { defs: vec![], by_name: HashMap::new(), busy: HashSet::new(), user: user.iter().copied().collect() }
    }

    /// 推論済みの型 `t` を段階 1 の型にする。`file` は名前を解決するファイル。対象外なら理由付きで `Err`。
    pub fn lower_ty(&mut self, p: &Program, file: &str, t: &Src) -> Result<Ty, String> {
        if let Some(x) = conv(t) {
            return Ok(x);
        }
        match t {
            Src::Named(name, args) => {
                if !args.is_empty() {
                    return Err(format!("型 {} は型引数付きで段階 1 の対象外", name));
                }
                let segs: Vec<String> = name.split('.').map(String::from).collect();
                match p.resolve(&segs, file).map(|r| r.target) {
                    Some(Tgt::Type(di)) if self.user.contains(&di) => self.intern_def(p, di),
                    _ => Err(format!("型 {} は段階 1 の対象外 (利用者の定義でない)", name)),
                }
            }
            Src::Tuple(xs) => {
                let mut elems = vec![];
                for x in xs {
                    elems.push(self.field_ty(p, file, x)?);
                }
                self.intern_tuple(elems)
            }
            other => Err(format!("型 {} は段階 1 の対象外", show(other))),
        }
    }

    /// フィールド・要素の型。ユニット型は値を置けないので拒否する。
    fn field_ty(&mut self, p: &Program, file: &str, t: &Src) -> Result<Ty, String> {
        match self.lower_ty(p, file, t)? {
            Ty::Unit => Err("ユニット型のフィールドは段階 1 の対象外".to_string()),
            Ty::Ref(..) | Ty::Opt(_) | Ty::Res(..) => Err("参照・Option・Result のフィールドは段階 1 の対象外".to_string()),
            x => Ok(x),
        }
    }

    fn name_of(&self, t: Ty) -> String {
        match t {
            Ty::Agg(i) => self.defs[i as usize].name.clone(),
            t => t.name().to_string(),
        }
    }

    fn push(&mut self, name: String, size: u32, align: u32, kind: AggKind) -> Ty {
        let i = self.defs.len() as u32;
        self.by_name.insert(name.clone(), i);
        self.defs.push(AggDef { name, size, align, kind });
        Ty::Agg(i)
    }

    fn intern_tuple(&mut self, elems: Vec<Ty>) -> Result<Ty, String> {
        let name = format!("({})", elems.iter().map(|t| self.name_of(*t)).collect::<Vec<_>>().join(","));
        if let Some(&i) = self.by_name.get(&name) {
            return Ok(Ty::Agg(i));
        }
        let fs: Vec<(String, Ty)> = elems.into_iter().map(|t| (String::new(), t)).collect();
        let (size, align, fields) = layout_fields(&self.defs, &fs);
        Ok(self.push(name, size, align, AggKind::Tuple(fields)))
    }

    /// 定義 `p.defs[di]` (構造体 `type` または列挙型 `enum`) を型表に登録する。
    fn intern_def(&mut self, p: &Program, di: usize) -> Result<Ty, String> {
        let d = &p.defs[di];
        if let Some(&i) = self.by_name.get(&d.name) {
            return Ok(Ty::Agg(i));
        }
        if (d.kw != "type" && d.kw != "enum") || !d.generics.is_empty() {
            return Err(format!("型 {} は段階 1 の対象外 (構造体・列挙型でない、またはジェネリクス)", d.name));
        }
        if !self.busy.insert(d.name.clone()) {
            return Err(format!("再帰的な型 {} は段階 1 の対象外", d.name));
        }
        let r = self.build_def(p, di);
        self.busy.remove(&d.name);
        r
    }

    fn build_def(&mut self, p: &Program, di: usize) -> Result<Ty, String> {
        let d = &p.defs[di];
        let (size, align, kind) = if d.kw == "type" {
            let mut fs = vec![];
            for (fname, fv, _) in &d.fields {
                let t = self.field_ty(p, &d.file, &from_v(fv, &HashSet::new()))?;
                fs.push((fname.clone(), t));
            }
            let (size, align, fields) = layout_fields(&self.defs, &fs);
            (size, align, AggKind::Struct(fields))
        } else {
            let mut vs = vec![];
            for (vn, tys) in &d.variants {
                let mut ts = vec![];
                for tv in tys {
                    ts.push(self.field_ty(p, &d.file, &from_v(tv, &HashSet::new()))?);
                }
                vs.push((vn.clone(), ts));
            }
            let (size, align, variants) = layout_enum(&self.defs, &vs);
            (size, align, AggKind::Enum(variants))
        };
        Ok(self.push(d.name.clone(), size, align, kind))
    }
}

impl<'a> Lw<'a> {
    /// 推論済みの型を段階 1 の型にする (集成体は型表に登録する)。対象外なら `None`。
    pub(super) fn conv_t(&self, t: &Src) -> Option<Ty> {
        self.aggs.borrow_mut().lower_ty(self.p, &self.def.file, t).ok()
    }

    fn agg_def(&self, ai: u32) -> AggDef {
        self.aggs.borrow().defs[ai as usize].clone()
    }

    /// 値 `v` の集成体の型 (番号と定義)。集成体でなければ止める。
    fn agg_of(&self, v: VId, line: usize) -> LR<(u32, AggDef)> {
        match self.vty(v) {
            Ty::Agg(ai) => Ok((ai, self.agg_def(ai))),
            t => self.err(line, format!("型 {} は集成体でない", crate::kir::fmt_ty(t))),
        }
    }

    /// 集成体の式の振り分け (`expr` から呼ぶ)。
    pub(super) fn agg_expr(&mut self, e: &V) -> LR<Option<VId>> {
        match nname(e) {
            "StructLit" => self.struct_lit(e),
            "TupleE" => self.tuple_e(e),
            "Field" => {
                let f = nf(e);
                let Some(b) = self.expr(&f[0])? else { return Ok(None) };
                let name = st(&f[1]);
                self.field_of(b, &name, nline(e)).map(Some)
            }
            other => self.err(nline(e), format!("式 {} は段階 1 の対象外", other)),
        }
    }

    /// 構造体リテラル `S { f: e, ... }`。式は書かれた順に評価し、値は宣言の順に並べる。
    fn struct_lit(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let line = nline(e);
        let segs: Vec<String> = st(&f[0]).split('.').map(String::from).collect();
        let Some(Tgt::Type(_)) = self.p.resolve(&segs, &self.def.file).map(|r| r.target) else {
            return self.err(line, "構造体リテラルの名前が段階 1 の対象外");
        };
        let ty = self.ty_of(e)?;
        let Ty::Agg(ai) = ty else {
            return self.err(line, "構造体リテラルの型が集成体でない");
        };
        let d = self.agg_def(ai);
        let AggKind::Struct(decl) = &d.kind else {
            return self.err(line, "構造体リテラルの型が構造体でない");
        };
        let mut vals: Vec<Option<VId>> = vec![None; decl.len()];
        for it in items(&f[1]) {
            let V::L(xs) = it else {
                return self.err(line, "構造体リテラルの形が不正");
            };
            let fname = st(&xs[0]);
            let Some(k) = decl.iter().position(|x| x.name == fname) else {
                return self.err(line, format!("フィールド {} がない", fname));
            };
            if vals[k].is_some() {
                return self.err(line, format!("フィールド {} が二度書かれている", fname));
            }
            let Some(v) = self.expr(&xs[1])? else { return Ok(None) };
            if self.vty(v) != decl[k].ty {
                return self.err(line, format!("フィールド {} の型が違う", fname));
            }
            vals[k] = Some(v);
        }
        let Some(fields) = vals.into_iter().collect::<Option<Vec<VId>>>() else {
            return self.err(line, "構造体リテラルにフィールドが足りない");
        };
        Ok(Some(self.emit(ty, Op::Make { variant: None, fields })))
    }

    /// タプル `(a, b, ...)`。
    fn tuple_e(&mut self, e: &V) -> LR<Option<VId>> {
        let line = nline(e);
        let xs = items(&nf(e)[0]);
        let ty = self.ty_of(e)?;
        let Ty::Agg(ai) = ty else {
            return self.err(line, "タプルの型が集成体でない");
        };
        let d = self.agg_def(ai);
        let AggKind::Tuple(decl) = &d.kind else {
            return self.err(line, "タプルの型がタプルでない");
        };
        if decl.len() != xs.len() {
            return self.err(line, "タプルの要素数が型と合わない");
        }
        let mut fields = vec![];
        for (x, fd) in xs.iter().zip(decl) {
            let Some(v) = self.expr(x)? else { return Ok(None) };
            if self.vty(v) != fd.ty {
                return self.err(line, "タプルの要素の型が違う");
            }
            fields.push(v);
        }
        Ok(Some(self.emit(ty, Op::Make { variant: None, fields })))
    }

    /// 構造体のフィールドの読み出し (`field`)。
    pub(super) fn field_of(&mut self, base: VId, name: &str, line: usize) -> LR<VId> {
        let (_, d) = self.agg_of(base, line)?;
        let AggKind::Struct(fs) = &d.kind else {
            return self.err(line, format!("{} は構造体でないのでフィールド {} を読めない", d.name, name));
        };
        let Some(fd) = fs.iter().find(|x| x.name == name) else {
            return self.err(line, format!("フィールド {} がない", name));
        };
        Ok(self.emit(fd.ty, Op::Field { a: base, off: fd.off }))
    }

    /// `x.f.g` の形の名前 (局所変数が先頭) のフィールドをたどる。
    pub(super) fn field_path(&mut self, base: VId, names: &[String], line: usize) -> LR<VId> {
        let mut v = base;
        for n in names {
            v = self.field_of(v, n, line)?;
        }
        Ok(v)
    }

    /// 列挙型のバリアントを作る (引数は宣言の順)。`args` が空なら、ペイロードのない変種。
    pub(super) fn variant_make(&mut self, en: &str, vn: &str, args: &[V], line: usize) -> LR<Option<VId>> {
        let Some(ty) = self.conv_t(&Src::Named(en.to_string(), vec![])) else {
            return self.err(line, format!("列挙型 {} は段階 1 の対象外", en));
        };
        let Ty::Agg(ai) = ty else {
            return self.err(line, format!("列挙型 {} は集成体でない", en));
        };
        let d = self.agg_def(ai);
        let AggKind::Enum(vs) = &d.kind else {
            return self.err(line, format!("{} は列挙型でない", en));
        };
        let Some(idx) = vs.iter().position(|v| v.name == vn) else {
            return self.err(line, format!("列挙子 {} がない", vn));
        };
        let decl = vs[idx].fields.clone();
        if decl.len() != args.len() {
            return self.err(line, format!("列挙子 {} の引数の個数が違う", vn));
        }
        let mut vals = vec![];
        for (a, fd) in args.iter().zip(&decl) {
            let Some(v) = self.expr(a)? else { return Ok(None) };
            if self.vty(v) != fd.ty {
                return self.err(line, format!("列挙子 {} の引数の型が違う", vn));
            }
            vals.push(v);
        }
        Ok(Some(self.emit(ty, Op::Make { variant: Some(idx as u32), fields: vals })))
    }

    /// 呼び出し形 `E.V(args)` (`call` から呼ぶ)。
    pub(super) fn variant_call(&mut self, e: &V, en: &str, vn: &str) -> LR<Option<VId>> {
        let args = items(&nf(e)[1]);
        self.variant_make(en, vn, args, nline(e))
    }

    /// 値の形 `E.V` (ペイロードのない列挙子、`path_val` から呼ぶ)。
    pub(super) fn variant_value(&mut self, en: &str, vn: &str, line: usize) -> LR<VId> {
        match self.variant_make(en, vn, &[], line)? {
            Some(v) => Ok(v),
            None => self.err(line, "列挙子の値が作れない"),
        }
    }

    /// `let` の束縛 (名前だけでない形: タプル・構造体・列挙型の反駁できない形)。
    pub(super) fn let_pat(&mut self, pat: &V, init: &V) -> LR<St> {
        let Some(v) = self.expr(init)? else { return Ok(St::Div) };
        let t = self.vty(v);
        let binds = self.pat(pat, v, t, None)?;
        for (n, bv, bt) in binds {
            self.bind(&n, bv, bt);
        }
        Ok(St::Unit)
    }

    /// `p.f = e` / `p.f.g = e` (局所変数を起点とする名前への代入)。値を作り直して変数に入れ直す。
    pub(super) fn assign_path(&mut self, target: &V, rhs: &V, line: usize) -> LR<St> {
        let segs = strs(&nf(target)[0]);
        let Some(s) = self.lookup(&segs[0]) else {
            return self.err(line, format!("代入先 {} が見つからない", segs[0]));
        };
        let Some(v) = self.expr(rhs)? else { return Ok(St::Div) };
        // chain[j] は segs[..=j] の値 (最後の名前の手前まで)
        let mut chain = vec![self.read_slot(s)];
        for name in &segs[1..segs.len() - 1] {
            let p = *chain.last().expect("鎖");
            let nv = self.field_of(p, name, line)?;
            chain.push(nv);
        }
        let mut newv = v;
        for j in (1..segs.len()).rev() {
            newv = self.set_field(chain[j - 1], &segs[j], newv, line)?;
        }
        if self.vty(newv) != self.slots[s].1 {
            return self.err(line, "代入の型が変数の型と違う");
        }
        self.assign_slot(s, newv);
        Ok(St::Unit)
    }

    fn set_field(&mut self, parent: VId, name: &str, newv: VId, line: usize) -> LR<VId> {
        let pty = self.vty(parent);
        let (_, d) = self.agg_of(parent, line)?;
        let AggKind::Struct(fs) = &d.kind else {
            return self.err(line, format!("{} は構造体でないのでフィールド {} に代入できない", d.name, name));
        };
        let Some(fd) = fs.iter().find(|x| x.name == name) else {
            return self.err(line, format!("フィールド {} がない", name));
        };
        if self.vty(newv) != fd.ty {
            return self.err(line, format!("フィールド {} への代入の型が違う", name));
        }
        Ok(self.emit(pty, Op::SetField { a: parent, off: fd.off, v: newv }))
    }

    /// 試験 `c` が偽なら `fail` へ、真なら新しいブロック (現在のブロックになる) へ進む。
    fn test(&mut self, c: VId, fail: usize) {
        let (cont, _) = self.new_block(&[]);
        self.branch(c, (cont, vec![]), (fail, vec![]));
        self.cur = Some(cont);
    }

    /// 列挙型の変種の試験 (タグの比較)。ペイロードのフィールドを返す。
    fn variant_test(&mut self, segs: &[String], val: VId, ty: Ty, fail: Option<usize>, line: usize) -> LR<Vec<Field>> {
        let Some(Tgt::Variant(en, vn)) = self.p.resolve(segs, &self.def.file).map(|r| r.target) else {
            return self.err(line, format!("パターン {} は段階 1 の対象外", segs.join(".")));
        };
        let Ty::Agg(ai) = ty else {
            return self.err(line, "列挙型のパターンの対象が集成体でない");
        };
        let d = self.agg_def(ai);
        let AggKind::Enum(vs) = &d.kind else {
            return self.err(line, "列挙型のパターンの対象が列挙型でない");
        };
        if d.name != en {
            return self.err(line, format!("パターン {} の型が対象の型 {} と違う", vn, d.name));
        }
        let Some(idx) = vs.iter().position(|v| v.name == vn) else {
            return self.err(line, format!("列挙子 {} がない", vn));
        };
        let Some(fl) = fail else {
            return self.err(line, "let の束縛は反駁可能でない");
        };
        let tg = self.emit(Ty::I32, Op::Tag(val));
        let k = self.emit(Ty::I32, Op::Const(idx as i128));
        let c = self.emit(Ty::Bool, Op::Cmp { cc: crate::kir::Cc::Eq, ty: Ty::I32, a: tg, b: k });
        self.test(c, fl);
        Ok(vs[idx].fields.clone())
    }

    /// パターン `p` を値 `val` (型 `ty`) に当てる。試験が偽なら `fail` へ飛ぶ (`None` は反駁できない形のみ)。
    /// 返すのは束縛 (名前、値、型) の列。試験は現在のブロックから連鎖で作る。
    fn pat(&mut self, p: &V, val: VId, ty: Ty, fail: Option<usize>) -> LR<Vec<(String, VId, Ty)>> {
        let line = nline(p);
        let mut out = vec![];
        match nname(p) {
            "PWild" => {}
            "PBind" => out.push((st(&nf(p)[0]), val, ty)),
            "PLit" => {
                let lit = &nf(p)[0];
                let lf = nf(lit);
                match st(&lf[0]).as_str() {
                    "unit" => {}
                    "bool" => {
                        let Some(fl) = fail else {
                            return self.err(line, "let の束縛は反駁可能でない");
                        };
                        if ty != Ty::Bool {
                            return self.err(line, "bool のパターンの型が違う");
                        }
                        let b = matches!(lf[1], V::B(true)) as i128;
                        let k = self.emit(Ty::Bool, Op::Const(b));
                        let c = self.emit(Ty::Bool, Op::Cmp { cc: crate::kir::Cc::Eq, ty: Ty::Bool, a: val, b: k });
                        self.test(c, fl);
                    }
                    "int" => {
                        let Some(fl) = fail else {
                            return self.err(line, "let の束縛は反駁可能でない");
                        };
                        if !ty.is_int() {
                            return self.err(line, "整数のパターンの型が整数でない");
                        }
                        let v = parse_int(&lf[1]).ok_or_else(|| format!("行 {}: 整数パターンが扱えない", line))?;
                        if v < ty.min() || v > ty.max() {
                            return self.err(line, format!("整数パターンが {} の範囲にない", ty.name()));
                        }
                        let k = self.emit(ty, Op::Const(v));
                        let c = self.emit(Ty::Bool, Op::Cmp { cc: crate::kir::Cc::Eq, ty, a: val, b: k });
                        self.test(c, fl);
                    }
                    other => return self.err(line, format!("リテラルのパターン {} は段階 1 の対象外", other)),
                }
            }
            "PTuple" => {
                let Ty::Agg(ai) = ty else {
                    return self.err(line, "タプルのパターンの対象が集成体でない");
                };
                let d = self.agg_def(ai);
                let AggKind::Tuple(fs) = &d.kind else {
                    return self.err(line, "タプルのパターンの対象がタプルでない");
                };
                let subs = items(&nf(p)[0]);
                if subs.len() != fs.len() {
                    return self.err(line, "タプルのパターンの要素数が型と合わない");
                }
                for (sp, fd) in subs.iter().zip(fs) {
                    let fv = self.emit(fd.ty, Op::Field { a: val, off: fd.off });
                    out.extend(self.pat(sp, fv, fd.ty, fail)?);
                }
            }
            "PStruct" => {
                let Ty::Agg(ai) = ty else {
                    return self.err(line, "構造体のパターンの対象が集成体でない");
                };
                let d = self.agg_def(ai);
                let AggKind::Struct(fs) = &d.kind else {
                    return self.err(line, "構造体のパターンの対象が構造体でない");
                };
                for it in items(&nf(p)[1]) {
                    let V::L(xs) = it else {
                        return self.err(line, "構造体のパターンの形が不正");
                    };
                    let fname = st(&xs[0]);
                    let Some(fd) = fs.iter().find(|x| x.name == fname) else {
                        return self.err(line, format!("フィールド {} がない", fname));
                    };
                    let fv = self.emit(fd.ty, Op::Field { a: val, off: fd.off });
                    out.extend(self.pat(&xs[1], fv, fd.ty, fail)?);
                }
            }
            "PVariant" => {
                let segs = strs(&nf(p)[0]);
                let subs = items(&nf(p)[1]);
                let fields = self.variant_test(&segs, val, ty, fail, line)?;
                if subs.len() != fields.len() {
                    return self.err(line, "列挙子のパターンの要素数が違う");
                }
                for (sp, fd) in subs.iter().zip(&fields) {
                    let fv = self.emit(fd.ty, Op::Field { a: val, off: fd.off });
                    out.extend(self.pat(sp, fv, fd.ty, fail)?);
                }
            }
            "PPath" => {
                let segs = strs(&nf(p)[0]);
                self.variant_test(&segs, val, ty, fail, line)?;
            }
            other => return self.err(line, format!("パターン {} は段階 1 の対象外", other)),
        }
        Ok(out)
    }

    /// `match e { p => body ... }`。腕は順に試し、最初に当たった腕の値が結果になる。
    /// 代入された変数は `if` と同じく合流のブロック引数で渡す。
    pub(super) fn match_e(&mut self, e: &V) -> LR<Option<VId>> {
        let f = nf(e);
        let line = nline(e);
        let Some(sv) = self.expr(&f[0])? else { return Ok(None) };
        let sty = self.vty(sv);
        let res: Option<Ty> = match self.src_ty(e)? {
            Src::Never => None,
            t => Some(self.conv_t(&t).ok_or_else(|| format!("行 {}: match の型 {} は段階 1 の対象外", line, show(&t)))?),
        };
        let carry = matches!(res, Some(t) if t != Ty::Unit);
        let arms: Vec<(&V, &V)> = items(&f[1])
            .iter()
            .map(|a| match a {
                V::L(xs) if xs.len() == 2 => Ok((&xs[0], &xs[1])),
                _ => Err(format!("行 {}: match の腕の形が不正", line)),
            })
            .collect::<Result<_, _>>()?;
        let bodies: Vec<&V> = arms.iter().map(|(_, b)| *b).collect();
        let slots = self.assigned(&bodies);
        let slot_tys: Vec<Ty> = slots.iter().map(|s| self.slots[*s].1).collect();
        let mut params: Vec<Ty> = if carry { res.into_iter().collect() } else { vec![] };
        params.extend(slot_tys.iter().copied());
        let (jb, jp) = self.new_block(&params);
        let saved = self.slots.clone();
        for (pat, body) in arms {
            let next = self.new_block(&[]).0;
            self.slots = saved.clone();
            let binds = self.pat(pat, sv, sty, Some(next))?;
            self.scopes.push(vec![]);
            for (n, v, t) in binds {
                self.bind(&n, v, t);
            }
            if let Some(v) = self.expr(body)? {
                if let Some(t) = res {
                    if self.vty(v) != t && carry {
                        return self.err(line, "match の腕の型が結果の型と違う");
                    }
                }
                let mut args: Vec<VId> = if carry { vec![v] } else { vec![] };
                args.extend(self.vals_of(&slots));
                self.goto(jb, args);
            }
            self.scopes.pop();
            self.cur = Some(next);
        }
        // 網羅されていれば、最後の試験は到達しない。
        self.terminate(Term::Unreachable);
        self.slots = saved;
        if self.blocks[jb].preds == 0 {
            return Ok(None);
        }
        self.cur = Some(jb);
        let off = carry as usize;
        for (i, s) in slots.iter().enumerate() {
            self.slots[*s] = (jp[off + i], slot_tys[i]);
        }
        Ok(Some(if carry { jp[0] } else { self.unit() }))
    }
}

#[cfg(test)]
mod tests {
    use crate::kir::AggKind;

    /// 配置の照合用の定義。clif の `layout` と KIR の型表の大きさ・整列・オフセットが一致することを確かめる。
    /// 入れ子の構造体、整列の違うフィールド、タグだけの列挙型、バリアントごとに違う大きさを含む。
    const SRC: &str = "type t.A
    a: u8
    b: u64
    c: bool
end t.A

type t.B
    x: i16
    inner: t.A
    y: i32
end t.B

type t.C
    p: t.B
    q: bool
end t.C

enum t.E
    One(u8, u32)
    Two(i64)
    Three(bool, i16, u8)
    Zero
end t.E

fn t.keep(_a: t.A, _b: t.B, _c: t.C, _e: t.E, _p: (u8, i64), _q: (bool, i16, u8, i32)) -> i32 uses [] panics never
    0i32
end t.keep

fn t.main() -> Result<(), std.io.Error> uses [] panics never
    Ok(())
end t.main
";

    #[test]
    fn kir_agg_layout_matches_clif() {
        let root = std::env::temp_dir().join(format!("ksn_kir_agg_layout_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("kasane.toml"), "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = 1\nentry = \"t.main\"\n\n[dependencies]\n").unwrap();
        std::fs::write(root.join("kasane.lock"), "version = 1\n").unwrap();
        std::fs::write(root.join("src").join("t.ksnl"), SRC).unwrap();
        let checked = crate::cli::checked_program_for_test(&root);
        let _ = std::fs::remove_dir_all(&root);
        let (prog, user) = checked.expect("試験の入力は検査を通るはず");
        let lo = crate::kir_lower::lower_user(&prog, &user);
        assert!(lo.module.find("t.keep").is_some(), "{:?}", lo.unsupported);
        let names: Vec<&str> = lo.module.aggs.iter().map(|d| d.name.as_str()).collect();
        for want in ["t.A", "t.B", "t.C", "t.E", "(u8,i64)", "(bool,i16,u8,i32)"] {
            assert!(names.contains(&want), "型表に {} がない: {:?}", want, names);
        }
        for d in &lo.module.aggs {
            let cname = match d.kind {
                AggKind::Tuple(_) => format!("tuple<{}>", &d.name[1..d.name.len() - 1]),
                _ => d.name.clone(),
            };
            let (size, align, offs, vars) = crate::clif::layout_probe(&prog, &cname).expect("clif の配置");
            assert_eq!((d.size, d.align), (size, align), "大きさ・整列: {}", d.name);
            match &d.kind {
                AggKind::Struct(fs) | AggKind::Tuple(fs) => {
                    let mine: Vec<u32> = fs.iter().map(|f| f.off).collect();
                    assert_eq!(mine, offs, "フィールドのオフセット: {}", d.name);
                }
                AggKind::Enum(vs) => {
                    let mine: Vec<Vec<u32>> = vs.iter().map(|v| v.fields.iter().map(|f| f.off).collect()).collect();
                    assert_eq!(mine, vars, "バリアントのオフセット: {}", d.name);
                }
            }
        }
    }
}
