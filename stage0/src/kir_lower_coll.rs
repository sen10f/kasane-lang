//! KIR の下ろし: `Vec` / `String` / `&str` / `for` / 添字 / クロージャ (設計_KIR.md 10.7、kir/collections)。
//!
//! ヒープは clif と同じ実メモリ表現を使う。`Vec<T>` と `String` は {ptr, len, cap} の 24 バイトのヘッダ、
//! `&str` は {ptr, len} の 16 バイトのヘッダ。KIR の値は「ヘッダのアドレス」(`ptr` 型) で表し、
//! ヘッダ自体はフレームの領域 (`frame.bytes`、関数の入口で確保) に置く。所有するバッファは `heap.free` で明示的に解放する。
//!
//! - 解放: 所有する値を作った時点で、そのスコープの所有の一覧 (`CollCx::owned`) に入れる。スコープの終わり、
//!   `return`、`break` / `continue` のたびに、対象のスコープの値を後入れ先出しで `free` する。
//! - 移動はしない: `let w = v` や値としての受け渡しは拒否する。`for x in v` は `v` を借りて反復するだけで、
//!   解放は `v` の寿命の終わりに行う (観測できない違いだけ生じる)。
//! - クロージャ: 捕獲のないクロージャは、本体を別の関数 (鍵 `関数#c式ID`) として下ろす。値は `unit` の印で、
//!   `let` で束縛した名前の呼び出しは直接の呼び出しになる。型引数に型パラメータを持つ関数 (`Fn` 境界) は、
//!   渡されたクロージャごとに単相インスタンス (鍵 `関数<クロージャの鍵>`) を作る。
//! - 対象外 (理由を付けて拒否): `Vec` の `push` / `pop` / `get` など (戻り値が `Result` / `Option` のため)、
//!   `String` の `push_str` (同じ理由)、`&[T]` のスライス (参照)、`capture` を持つクロージャ (参照の捕獲)、
//!   `for` の対象が `core.iter.range(a, b)` と要素型の分かる Vec の変数以外のもの。

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::check::{is_nil, items, nf, nline, nname, st, strs};
use crate::kir::{Aop, Block, Cc, Func, Inst, Mode, Op, Term, Ty, VId};
use crate::kir_lower::{conv, Lw, St, LR};
use crate::parser::{nid, V};
use crate::program::Target;
use crate::types::{from_v, Ty as Src};

const VEC: &str = "alloc.vec.Vec";
const STRING: &str = "alloc.string.String";
/// `Vec<T>` と `String` のヘッダの大きさ (ptr, len, cap)。
const HDR_VEC: u32 = 24;
/// `&str` のヘッダの大きさ (ptr, len)。
const HDR_STR: u32 = 16;

/// コレクション系の値の種類 (スロットごと、または式ごと)。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    /// `Vec<T>`。要素型が分かれば `Some`。
    Vec(Option<Ty>),
    /// `String` (`len` と `is_empty` だけ)。
    String,
    /// `&str` (値は借用なので複製してよい)。
    Str,
    /// 下ろした関数の鍵と、引数・戻り値の型。捕獲のないクロージャだけ。
    Closure { key: String, args: Vec<Ty>, ret: Ty },
}

/// 下ろしの状態。`Lw` が 1 つ持つ。
#[derive(Default)]
pub(crate) struct CollCx {
    /// スロット番号 → 種類。
    pub(crate) kinds: BTreeMap<usize, Kind>,
    /// 所有する値 (スコープの番号, ヘッダのアドレス) の登録順。
    pub(crate) owned: Vec<(usize, VId)>,
    /// 関数の入口で確保する `alloca` (ブロック 0 の先頭に置く)。
    pub(crate) allocas: Vec<Inst>,
    /// この関数から下ろしたクロージャの関数 (本体の関数とは別に出す)。
    pub(crate) extra: Vec<(Func, BTreeSet<String>)>,
    /// 下ろし済み (または下ろし中) の単相インスタンスの鍵。再帰で同じものを二度下ろさないため。
    pub(crate) instances: BTreeSet<String>,
    /// クロージャの鍵の接頭辞。単相インスタンスの中では、同じ式でもインスタンスごとに別の関数になるので鍵を分ける。
    pub(crate) prefix: String,
}

/// 型表の式の型 (式 ID が 0 なら無い)。
fn table_ty(lw: &Lw, e: &V) -> Option<Src> {
    let id = nid(e);
    if id == 0 {
        return None;
    }
    lw.tys.get(&(lw.def.name.clone(), id)).map(|x| x.ty.clone())
}

fn strip_ref(t: &Src) -> &Src {
    let mut t = t;
    while let Src::Ref(_, inner) = t {
        t = inner;
    }
    t
}

/// 式の値の段階 1 の型 (コレクション系は `ptr`、クロージャは `unit` の値)。対象外なら `None`。
pub(crate) fn value_ty(t: &Src) -> Option<Ty> {
    match strip_ref(t) {
        Src::Named(n, _) if n == VEC || n == STRING => Some(Ty::Ptr),
        Src::Prim(s) if s == "str" => Some(Ty::Ptr),
        Src::Closure { .. } => Some(Ty::Unit),
        _ => None,
    }
}

fn kind_of_src(t: &Src) -> Option<Kind> {
    match strip_ref(t) {
        Src::Named(n, args) if n == VEC => Some(Kind::Vec(args.first().and_then(conv))),
        Src::Named(n, _) if n == STRING => Some(Kind::String),
        Src::Prim(s) if s == "str" => Some(Kind::Str),
        _ => None,
    }
}

/// クロージャの鍵。式 ID は関数 (または単相インスタンス) の中で一意なので、同じ中の別のクロージャと衝突しない。
fn closure_key(lw: &Lw, id: u32) -> String {
    let base = if lw.coll.prefix.is_empty() { lw.def.name.as_str() } else { lw.coll.prefix.as_str() };
    format!("{}#c{}", base, id)
}

/// 単一の名前の経路なら、その名前のスロット (見えているもの)。
fn local_slot(lw: &Lw, e: &V) -> Option<usize> {
    if nname(e) != "Path" {
        return None;
    }
    let segs = strs(&nf(e)[0]);
    if segs.len() != 1 {
        return None;
    }
    lw.lookup(&segs[0])
}

/// 受け手の種類。局所変数はスロットの種類、それ以外は型表の型から決める。
fn kind_of_expr(lw: &Lw, e: &V) -> Option<Kind> {
    if let Some(s) = local_slot(lw, e) {
        return lw.coll.kinds.get(&s).cloned();
    }
    table_ty(lw, e).and_then(|t| kind_of_src(&t))
}

fn emit_alloca(lw: &mut Lw, size: u32) -> VId {
    let dst = lw.new_val(Ty::Ptr);
    lw.coll.allocas.push(Inst { dst, ty: Ty::Ptr, op: Op::FrameBytes { size } });
    dst
}

fn load_len(lw: &mut Lw, hdr: VId) -> VId {
    lw.emit(Ty::Usize, Op::MemLoad { p: hdr, off: 8 })
}

fn size_of(t: Ty) -> Option<u32> {
    Some(match t {
        Ty::I8 | Ty::U8 | Ty::Bool => 1,
        Ty::I16 | Ty::U16 => 2,
        Ty::I32 | Ty::U32 => 4,
        Ty::I64 | Ty::U64 | Ty::Isize | Ty::Usize => 8,
        _ => return None,
    })
}

/// 所有するバッファを解放する命令列を、現在のブロックに置く (`null` なら何もしない)。
fn emit_free(lw: &mut Lw, hdr: VId) {
    let buf = lw.emit(Ty::Ptr, Op::MemLoad { p: hdr, off: 0 });
    lw.emit(Ty::Unit, Op::HeapFree(buf));
}

/// スコープ `depth` 以降に登録された所有値を、後入れ先出しで解放する (`break` / `continue` / `return` の後始末)。
/// 現在のブロックが無い (到達しない) ときは何もしない。登録は消さない (他の経路で必要なため)。
pub(crate) fn drop_from(lw: &mut Lw, depth: usize) {
    if lw.cur.is_none() {
        return;
    }
    let heads: Vec<VId> = lw.coll.owned.iter().rev().filter(|(d, _)| *d >= depth).map(|(_, h)| *h).collect();
    for h in heads {
        emit_free(lw, h);
    }
}

/// スコープの終わり: 所有値を解放し、そのスコープの登録を消す。`out` (ブロックの値) が所有値なら拒否する。
/// スコープを閉じるのは呼び出し側 (`scopes.pop`)。
pub(crate) fn block_end(lw: &mut Lw, out: Option<VId>, line: usize) -> LR<()> {
    let depth = lw.scopes.len() - 1;
    if let Some(v) = out {
        check_not_owned(lw, v, line)?;
    }
    drop_from(lw, depth);
    lw.coll.owned.retain(|(d, _)| *d < depth);
    Ok(())
}

pub(crate) fn check_not_owned(lw: &Lw, v: VId, line: usize) -> LR<()> {
    if lw.coll.owned.iter().any(|(_, h)| *h == v) {
        return lw.err(line, "Vec / String の値を値として返す (所有の移動) は段階 1 の対象外");
    }
    Ok(())
}

/// 所有する値を値として使う (移す) ことを拒否する。
pub(crate) fn check_value_use(lw: &Lw, s: usize, line: usize) -> LR<()> {
    if matches!(lw.coll.kinds.get(&s), Some(Kind::Vec(_)) | Some(Kind::String)) {
        return lw.err(line, "Vec / String の値を移す (所有の移動) は段階 1 の対象外 (len・添字・for で使う)");
    }
    Ok(())
}

/// 代入の先: 所有する値と、クロージャの値は代入できない (`&str` は差し替えてよい)。
pub(crate) fn check_assign(lw: &Lw, s: usize, line: usize) -> LR<()> {
    if matches!(lw.coll.kinds.get(&s), Some(Kind::Vec(_)) | Some(Kind::String) | Some(Kind::Closure { .. })) {
        return lw.err(line, "Vec / String / クロージャの変数への代入は段階 1 の対象外");
    }
    Ok(())
}

/// `let x = init` の後に、`x` の種類を決める。
pub(crate) fn after_let(lw: &mut Lw, name: &str, init: &V) -> LR<()> {
    let Some(slot) = lw.lookup(name) else { return Ok(()) };
    if nname(init) == "Closure" {
        if let Some(k) = closure_kind_from_table(lw, init) {
            lw.coll.kinds.insert(slot, k);
        }
        return Ok(());
    }
    if let Some(s) = local_slot(lw, init) {
        if let Some(k) = lw.coll.kinds.get(&s).cloned() {
            lw.coll.kinds.insert(slot, k);
        }
        return Ok(());
    }
    if let Some(k) = table_ty(lw, init).and_then(|t| kind_of_src(&t)) {
        lw.coll.kinds.insert(slot, k);
    }
    Ok(())
}

/// クロージャの式 (本体は `closure_lit` で下ろした後) の種類。型表の `Closure` 型から作る。
fn closure_kind_from_table(lw: &Lw, e: &V) -> Option<Kind> {
    let Some(Src::Closure { args, ret, .. }) = table_ty(lw, e) else { return None };
    let ats: Option<Vec<Ty>> = args.iter().map(conv).collect();
    let (args, ret) = (ats?, conv(&ret)?);
    Some(Kind::Closure { key: closure_key(lw, nid(e)), args, ret })
}

/// 関数の本体の後: 入口の `alloca` をブロック 0 の先頭に置く。
pub(crate) fn finish(blocks: &mut [Block], allocas: Vec<Inst>) {
    if allocas.is_empty() {
        return;
    }
    let rest = std::mem::take(&mut blocks[0].insts);
    blocks[0].insts = allocas.into_iter().chain(rest).collect();
}

/// `Vec::new(std.alloc.SYSTEM)` / `String::new(std.alloc.SYSTEM)`: ヘッダを作り、空 (null, 0, 0) にする。
fn new_header(lw: &mut Lw, e: &V, size: u32) -> LR<Option<VId>> {
    let f = nf(e);
    let line = nline(e);
    let args = items(&f[1]);
    let system = args.len() == 1 && nname(&args[0]) == "Path" && strs(&nf(&args[0])[0]) == ["std", "alloc", "SYSTEM"];
    if !system {
        return lw.err(line, "Vec / String の生成は std.alloc.SYSTEM だけ段階 1 の対象");
    }
    let hdr = emit_alloca(lw, size);
    let null = lw.emit(Ty::Ptr, Op::Const(0));
    let zero = lw.emit(Ty::Usize, Op::Const(0));
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: hdr, off: 0, v: null });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: hdr, off: 8, v: zero });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: hdr, off: 16, v: zero });
    let scope = lw.scopes.len() - 1;
    lw.coll.owned.push((scope, hdr));
    Ok(Some(hdr))
}

/// 関数呼び出しのうち、コレクションの生成と、クロージャ値の呼び出しを扱う。扱わないなら `None`。
pub(crate) fn try_call(lw: &mut Lw, e: &V) -> Option<LR<Option<VId>>> {
    let f = nf(e);
    if nname(&f[0]) != "Path" {
        return None;
    }
    let segs = strs(&nf(&f[0])[0]);
    if segs.len() == 1 {
        if let Some(s) = lw.lookup(&segs[0]) {
            if matches!(lw.coll.kinds.get(&s), Some(Kind::Closure { .. })) {
                return Some(call_closure(lw, e, s));
            }
        }
    }
    // 型引数に型パラメータを持つ関数 (`Fn` 境界の引数) は、渡されたクロージャごとに単相化する。
    if let Some(Target::Fn(di)) = lw.p.resolve(&segs, &lw.def.file).map(|r| r.target) {
        if lw.p.defs[di].generics.iter().any(|g| g.0 == "type") {
            return Some(generic_call(lw, e, di));
        }
    }
    if segs.len() == 1 {
        return None;
    }
    if segs.len() == 2 {
        if let Some(s) = lw.lookup(&segs[0]) {
            if let Some(kind) = lw.coll.kinds.get(&s).cloned() {
                if !matches!(kind, Kind::Closure { .. }) {
                    return Some(try_local_method(lw, e, s, kind, &segs[1]));
                }
            }
        }
    }
    let segs: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    match segs.as_slice() {
        ["alloc", "vec", "Vec", "new"] => Some(new_header(lw, e, HDR_VEC)),
        ["alloc", "string", "String", "new"] => Some(new_header(lw, e, HDR_VEC)),
        _ => None,
    }
}

/// 型引数に捕獲のないクロージャを取る関数の呼び出し。鍵は `関数名<クロージャの鍵,...>` で、
/// クロージャの引数は実行時の値を持たず (鍵で呼び先が決まる)、値の引数だけを渡す。
fn generic_call(lw: &mut Lw, e: &V, di: usize) -> LR<Option<VId>> {
    let line = nline(e);
    let p = lw.p;
    let cd = &p.defs[di];
    let f = nf(e);
    if cd.kw != "fn" || cd.body.is_none() || cd.is_method() || !matches!(f.get(2), Some(V::Nil) | None) {
        return lw.err(line, format!("呼び先 {} は段階 1 の対象外 (型引数の単相化は自由関数だけ)", cd.name));
    }
    let tnames: HashSet<String> = cd.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.clone()).collect();
    let argv = items(&f[1]);
    if argv.len() != cd.params.len() {
        return lw.err(line, format!("呼び先 {} の引数の個数が違う", cd.name));
    }
    let mut bound: BTreeMap<String, Kind> = BTreeMap::new();
    let mut vals = vec![];
    for ((_, pt), a) in cd.params.iter().zip(argv) {
        match from_v(pt, &tnames) {
            Src::Param(g) => {
                let Some(k) = closure_arg_kind(lw, a)? else {
                    return lw.err(line, "型引数に渡せるのは捕獲のないクロージャ (段階 1) だけ");
                };
                bound.insert(g, k);
            }
            t => {
                let Some(pty) = conv(&t) else {
                    return lw.err(line, format!("呼び先 {} の引数の型は段階 1 の対象外", cd.name));
                };
                let Some(v) = lw.expr(a)? else { return Ok(None) };
                if lw.vty(v) != pty {
                    return lw.err(line, format!("呼び先 {} の引数の型が合わない", cd.name));
                }
                vals.push(v);
            }
        }
    }
    let mut keys = vec![];
    for g in cd.generics.iter().filter(|g| g.0 == "type") {
        match bound.get(&g.1) {
            Some(Kind::Closure { key, .. }) => keys.push(key.clone()),
            _ => return lw.err(line, format!("型引数 {} が決まらない", g.1)),
        }
    }
    let ret = match ret_ty(cd, &tnames) {
        Some(x) => x,
        None => return lw.err(line, format!("呼び先 {} の戻り値の型は段階 1 の対象外", cd.name)),
    };
    let key = format!("{}<{}>", cd.name, keys.join(","));
    lw.callees.insert(key.clone());
    if !lw.coll.instances.contains(&key) {
        lw.coll.instances.insert(key.clone());
        let mut child = Lw::new(lw.p, lw.tys, cd);
        child.coll.instances = std::mem::take(&mut lw.coll.instances);
        let r = lower_instance(&mut child, cd, &key, &tnames, &bound);
        lw.coll.instances = std::mem::take(&mut child.coll.instances);
        let (f, callees) = r?;
        lw.coll.extra.push((f, callees));
        let nested = std::mem::take(&mut child.coll.extra);
        lw.coll.extra.extend(nested);
    }
    Ok(Some(lw.emit(ret, Op::Call { callee: key, args: vals })))
}

/// 戻り値の型 (省略は unit)。段階 1 の型に入らないなら `None`。
fn ret_ty(cd: &crate::program::Def, tnames: &HashSet<String>) -> Option<Ty> {
    if is_nil(&cd.ret) {
        return Some(Ty::Unit);
    }
    conv(&from_v(&cd.ret, tnames))
}

/// 型引数に渡すクロージャの種類 (クロージャの式、または種類の分かる局所変数)。
fn closure_arg_kind(lw: &mut Lw, a: &V) -> LR<Option<Kind>> {
    if nname(a) == "Closure" {
        if lw.expr(a)?.is_none() {
            return Ok(None);
        }
        return Ok(closure_kind_from_table(lw, a));
    }
    Ok(local_slot(lw, a).and_then(|s| lw.coll.kinds.get(&s).cloned()).filter(|k| matches!(k, Kind::Closure { .. })))
}

/// 単相インスタンス (`cd` を `bound` のクロージャで束縛したもの) を下ろす。
/// 入口の引数のうち型引数の型のものは値を持たず、その型の種類 (クロージャ) で呼び先が決まる。
fn lower_instance(
    child: &mut Lw,
    cd: &crate::program::Def,
    key: &str,
    tnames: &HashSet<String>,
    bound: &BTreeMap<String, Kind>,
) -> LR<(Func, BTreeSet<String>)> {
    child.coll.prefix = key.to_string();
    let mut value_params = vec![];
    for (_, pt) in &cd.params {
        if let Src::Param(_) = from_v(pt, tnames) {
            continue;
        }
        match conv(&from_v(pt, tnames)) {
            Some(x) => value_params.push(x),
            None => return child.err(0, format!("呼び先 {} の引数の型は段階 1 の対象外", cd.name)),
        }
    }
    let (_, pv) = child.new_block(&value_params);
    child.cur = Some(0);
    child.scopes.push(vec![]);
    let mut vi = 0;
    for (pname, pt) in &cd.params {
        if let Src::Param(g) = from_v(pt, tnames) {
            let Some(k) = bound.get(&g).cloned() else {
                return child.err(0, format!("型引数 {} が決まらない", g));
            };
            let tok = child.emit(Ty::Unit, Op::Const(0));
            child.bind(pname, tok, Ty::Unit);
            let s = child.lookup(pname).expect("直前に束縛した名前");
            child.coll.kinds.insert(s, k);
        } else {
            let t = conv(&from_v(pt, tnames)).expect("上で検査済み");
            child.bind(pname, pv[vi], t);
            vi += 1;
        }
    }
    child.ret = ret_ty(cd, tnames).ok_or_else(|| format!("呼び先 {} の戻り値の型は段階 1 の対象外", cd.name))?;
    let body = cd.body.as_ref().expect("候補は本体を持つ");
    if let Some(v) = child.expr(body)? {
        if child.vty(v) != child.ret {
            return child.err(nline(body), "本体の値の型が戻り値の型と合わない");
        }
        child.terminate(Term::Ret(v));
    }
    // 効果の型引数 (`uses [E]`) は、捕獲のないクロージャでは空の集合に束縛される。
    let mut uses: Vec<String> = cd.uses.iter().filter(|u| !cd.generics.iter().any(|g| g.1 == **u)).cloned().collect();
    uses.sort();
    uses.dedup();
    let mut blocks: Vec<Block> = std::mem::take(&mut child.blocks)
        .into_iter()
        .map(|b| Block { params: b.params, insts: b.insts, term: b.term.unwrap_or(Term::Unreachable) })
        .collect();
    finish(&mut blocks, std::mem::take(&mut child.coll.allocas));
    let func = Func {
        fqn: cd.name.clone(),
        key: key.to_string(),
        uses,
        may_panic: cd.panics.as_deref() != Some("never"),
        stack: cd.stack,
        ret: child.ret,
        blocks,
    };
    Ok((func, std::mem::take(&mut child.callees)))
}

fn call_closure(lw: &mut Lw, e: &V, s: usize) -> LR<Option<VId>> {
    let line = nline(e);
    let Some(Kind::Closure { key, args, ret }) = lw.coll.kinds.get(&s).cloned() else {
        return lw.err(line, "クロージャの呼び出しの内部不整合");
    };
    let argv = items(&nf(e)[1]);
    if argv.len() != args.len() {
        return lw.err(line, "クロージャの引数の個数が違う");
    }
    let mut vs = vec![];
    for a in argv {
        match lw.expr(a)? {
            Some(v) => vs.push(v),
            None => return Ok(None),
        }
    }
    for (v, t) in vs.iter().zip(args.iter()) {
        if lw.vty(*v) != *t {
            return lw.err(line, "クロージャの引数の型が合わない");
        }
    }
    lw.callees.insert(key.clone());
    Ok(Some(lw.emit(ret, Op::Call { callee: key, args: vs })))
}

/// 受け手の値 (ヘッダのアドレス)。局所変数は移さずにそのまま借りる。
fn place(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    if let Some(s) = local_slot(lw, e) {
        return Ok(Some(lw.slots[s].0));
    }
    lw.expr(e)
}

/// `e.m(args)` の形 (`MCall`)。受け手がコレクション系のときだけ扱い、それ以外は `None` (既定の経路へ)。
pub(crate) fn try_mcall(lw: &mut Lw, e: &V) -> Option<LR<Option<VId>>> {
    let f = nf(e);
    let kind = kind_of_expr(lw, &f[0])?;
    let line = nline(e);
    if !matches!(f.get(3), Some(V::Nil) | None) {
        return Some(lw.err(line, "型引数付きのメソッドは段階 1 の対象外"));
    }
    let m = st(&f[1]);
    let nargs = items(&f[2]).len();
    Some(match place(lw, &f[0]) {
        Ok(Some(h)) => coll_method(lw, line, kind, h, &m, nargs),
        Ok(None) => Ok(None),
        Err(e) => Err(e),
    })
}

/// 局所変数の受け手 `x.m(args)`。パーサは `x.m(..)` を `Path [x, m]` の呼び出しとして作るので、こちらで扱う。
fn try_local_method(lw: &mut Lw, e: &V, s: usize, kind: Kind, m: &str) -> LR<Option<VId>> {
    let f = nf(e);
    let line = nline(e);
    if !matches!(f.get(2), Some(V::Nil) | None) {
        return lw.err(line, "型引数付きのメソッドは段階 1 の対象外");
    }
    let hdr = lw.slots[s].0;
    coll_method(lw, line, kind, hdr, m, items(&f[1]).len())
}

fn coll_method(lw: &mut Lw, line: usize, kind: Kind, h: VId, m: &str, nargs: usize) -> LR<Option<VId>> {
    if matches!(kind, Kind::Closure { .. }) {
        return lw.err(line, format!("メソッド {} は段階 1 の対象外", m));
    }
    if !matches!(m, "len" | "is_empty") {
        return lw.err(line, format!("メソッド {} は段階 1 の対象外 (戻り値が Option / Result / 参照のため)", m));
    }
    if nargs != 0 {
        return lw.err(line, format!("メソッド {} の引数は段階 1 の対象外", m));
    }
    match m {
        "len" => Ok(Some(load_len(lw, h))),
        "is_empty" => {
            let len = load_len(lw, h);
            let zero = lw.emit(Ty::Usize, Op::Const(0));
            Ok(Some(lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Eq, ty: Ty::Usize, a: len, b: zero })))
        }
        other => lw.err(line, format!("メソッド {} は段階 1 の対象外", other)),
    }
}

/// 添字の値の番地: 境界検査のあと `ptr + i * size`。
fn elem_addr(lw: &mut Lw, hdr: VId, i: VId, elem: Ty) -> LR<VId> {
    let Some(sz) = size_of(elem) else {
        return lw.err(0, format!("要素の型 {} は段階 1 の対象外", elem.name()));
    };
    let len = load_len(lw, hdr);
    lw.emit(Ty::Unit, Op::IndexCheck { i, len });
    let base = lw.emit(Ty::Ptr, Op::MemLoad { p: hdr, off: 0 });
    let off = if sz == 1 {
        i
    } else {
        let k = lw.emit(Ty::Usize, Op::Const(sz as i128));
        lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: i, b: k })
    };
    Ok(lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off }))
}

/// 添字・反復の対象になる Vec の変数: (ヘッダのアドレス, 要素の型)。要素型が分からなければ拒否する。
fn vec_place(lw: &Lw, e: &V, line: usize) -> LR<(VId, Ty)> {
    if let Some(s) = local_slot(lw, e) {
        if let Some(Kind::Vec(Some(elem))) = lw.coll.kinds.get(&s) {
            return Ok((lw.slots[s].0, *elem));
        }
    }
    lw.err(line, "添字・for の対象は要素型の分かる Vec の変数だけ段階 1 の対象")
}

/// 局所変数の型 (スロットの型)。式が局所変数の経路でないなら `None`。
pub(crate) fn local_slot_ty(lw: &Lw, e: &V) -> Option<Ty> {
    local_slot(lw, e).map(|s| lw.slots[s].1)
}

/// `v[i]` の読み取り。
pub(crate) fn index_read(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    let f = nf(e);
    let line = nline(e);
    let (hdr, elem) = vec_place(lw, &f[0], line)?;
    let Some(i) = lw.expr(&f[1])? else { return Ok(None) };
    if lw.vty(i) != Ty::Usize {
        return lw.err(line, "添字の型が usize ではない");
    }
    let addr = elem_addr(lw, hdr, i, elem)?;
    Ok(Some(lw.emit(elem, Op::MemLoad { p: addr, off: 0 })))
}

/// `v[i] = x`。右辺を先に評価する (Rust の順序)。
pub(crate) fn assign_index(lw: &mut Lw, s: &V) -> LR<St> {
    let f = nf(s);
    let line = nline(s);
    let tf = nf(&f[0]);
    let (hdr, elem) = vec_place(lw, &tf[0], line)?;
    let Some(v) = lw.expr(&f[1])? else { return Ok(St::Div) };
    let Some(i) = lw.expr(&tf[1])? else { return Ok(St::Div) };
    if lw.vty(i) != Ty::Usize {
        return lw.err(line, "添字の型が usize ではない");
    }
    if lw.vty(v) != elem {
        return lw.err(line, "代入の型が要素の型と違う");
    }
    let addr = elem_addr(lw, hdr, i, elem)?;
    lw.emit(Ty::Unit, Op::MemStore { ty: elem, p: addr, off: 0, v });
    Ok(St::Unit)
}

/// 文字列リテラル: 静的なバイト列と、{ptr, len} のヘッダ。
pub(crate) fn str_lit(lw: &mut Lw, e: &V) -> LR<VId> {
    let f = nf(e);
    let bytes = unhex(&st(&f[1]));
    let hdr = emit_alloca(lw, HDR_STR);
    let data = lw.emit(Ty::Ptr, Op::StaticBytes(bytes.clone()));
    let n = lw.emit(Ty::Usize, Op::Const(bytes.len() as i128));
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Ptr, p: hdr, off: 0, v: data });
    lw.emit(Ty::Unit, Op::MemStore { ty: Ty::Usize, p: hdr, off: 8, v: n });
    Ok(hdr)
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()).collect()
}

/// `for PAT in IT BODY`。`IT` は `core.iter.range(a, b)` か、要素型の分かる Vec の変数。
pub(crate) fn for_expr(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    let f = nf(e);
    let line = nline(e);
    let name = match nname(&f[0]) {
        "PBind" if !matches!(nf(&f[0]).get(1), Some(V::B(true))) => Some(st(&nf(&f[0])[0])),
        "PWild" => None,
        _ => return lw.err(line, "for の変数はただの名前か _ だけ段階 1 の対象"),
    };
    let it = &f[1];
    if nname(it) == "Call" {
        let cf = nf(it);
        let is_range = nname(&cf[0]) == "Path" && strs(&nf(&cf[0])[0]) == ["core", "iter", "range"];
        let args = items(&cf[1]);
        if is_range && args.len() == 2 {
            let Some(a) = lw.expr(&args[0])? else { return Ok(None) };
            let Some(b) = lw.expr(&args[1])? else { return Ok(None) };
            let ity = lw.ty_of(&args[0])?;
            if !ity.is_int() || lw.vty(b) != ity {
                return lw.err(line, "core.iter.range の範囲は同じ整数の型だけ段階 1 の対象");
            }
            return counted(lw, line, Counted { start: a, end: b, ity, elem: None }, name, &f[2]);
        }
    }
    if let Some(s) = local_slot(lw, it) {
        if let Some(Kind::Vec(Some(elem))) = lw.coll.kinds.get(&s).cloned() {
            let hdr = lw.slots[s].0;
            let len = load_len(lw, hdr);
            let base = lw.emit(Ty::Ptr, Op::MemLoad { p: hdr, off: 0 });
            let zero = lw.emit(Ty::Usize, Op::Const(0));
            return counted(lw, line, Counted { start: zero, end: len, ity: Ty::Usize, elem: Some((base, elem)) }, name, &f[2]);
        }
    }
    lw.err(line, "for の対象は core.iter.range(a, b) か、要素型の分かる Vec の変数だけ段階 1 の対象")
}

struct Counted {
    start: VId,
    end: VId,
    /// 添字 (範囲の要素) の型。
    ity: Ty,
    /// Vec を反復するとき: (要素の先頭アドレス, 要素の型)。
    elem: Option<(VId, Ty)>,
}

/// 添字を 1 ずつ増やす数え上げのループ。代入される変数は、ループの先頭のブロック引数で受け渡す。
/// 構造: header(i, slots) → (i < end) ? body : exit。body の終わりと continue は incr(slots) へ、incr は header(i+1, slots) へ戻る。
fn counted(lw: &mut Lw, line: usize, c: Counted, name: Option<String>, body: &V) -> LR<Option<VId>> {
    let slots = lw.assigned(&[body]);
    let slot_tys: Vec<Ty> = slots.iter().map(|s| lw.slots[*s].1).collect();
    let (xb, xp) = lw.new_block(&slot_tys);
    let (ib, ip) = lw.new_block(&slot_tys);
    let mut hty = vec![c.ity];
    hty.extend(slot_tys.iter().copied());
    let (hb, hp) = lw.new_block(&hty);
    let mut entry = vec![c.start];
    entry.extend(lw.vals_of(&slots));
    lw.goto(hb, entry);

    lw.cur = Some(hb);
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (hp[k + 1], slot_tys[k]);
    }
    let (bb, _) = lw.new_block(&[]);
    let cond = lw.emit(Ty::Bool, Op::Cmp { cc: Cc::Lt, ty: c.ity, a: hp[0], b: c.end });
    let exit_args = lw.vals_of(&slots);
    lw.branch(cond, (bb, vec![]), (xb, exit_args));

    lw.cur = Some(bb);
    lw.scopes.push(vec![]);
    let depth = lw.scopes.len() - 1;
    if let Some(n) = &name {
        match c.elem {
            None => lw.bind(n, hp[0], c.ity),
            Some((base, ety)) => {
                let addr = elem_addr_from_base(lw, base, hp[0], ety)?;
                let v = lw.emit(ety, Op::MemLoad { p: addr, off: 0 });
                lw.bind(n, v, ety);
            }
        }
    }
    lw.loops.push(crate::kir_lower::LoopCx { header: ib, exit: xb, slots: slots.clone(), depth });
    let r = lw.expr(body)?;
    if r.is_some() {
        let args = lw.vals_of(&slots);
        lw.goto(ib, args);
    }
    lw.loops.pop();
    block_end(lw, None, line)?;
    lw.scopes.pop();

    // incr: 添字を 1 進めて先頭へ戻る (添字は body の中で変わらないので、先頭の値をそのまま使う)
    lw.cur = Some(ib);
    let one = lw.emit(c.ity, Op::Const(1));
    let next = lw.emit(c.ity, Op::Arith { op: Aop::Add, mode: Mode::Wrap, a: hp[0], b: one });
    let mut back = vec![next];
    back.extend(ip.iter().copied());
    lw.goto(hb, back);

    lw.cur = Some(xb);
    for (k, s) in slots.iter().enumerate() {
        lw.slots[*s] = (xp[k], slot_tys[k]);
    }
    Ok(Some(lw.unit()))
}

/// Vec の要素の番地 (境界は呼び出し側が保証する)。
fn elem_addr_from_base(lw: &mut Lw, base: VId, i: VId, elem: Ty) -> LR<VId> {
    let Some(sz) = size_of(elem) else {
        return lw.err(0, format!("要素の型 {} は段階 1 の対象外", elem.name()));
    };
    let off = if sz == 1 {
        i
    } else {
        let k = lw.emit(Ty::Usize, Op::Const(sz as i128));
        lw.emit(Ty::Usize, Op::Arith { op: Aop::Mul, mode: Mode::Wrap, a: i, b: k })
    };
    Ok(lw.emit(Ty::Ptr, Op::PtrAdd { p: base, off }))
}

/// 捕獲のないクロージャ。本体は別の関数 (鍵 `closure_key`) として下ろし、式の値は `unit` の印にする。
pub(crate) fn closure_lit(lw: &mut Lw, e: &V) -> LR<Option<VId>> {
    let f = nf(e);
    let line = nline(e);
    if !matches!(&f[2], V::L(xs) if xs.is_empty()) {
        return lw.err(line, "capture を持つクロージャは段階 1 の対象外");
    }
    let Some(Src::Closure { args, ret, .. }) = table_ty(lw, e) else {
        return lw.err(line, "クロージャの型が決まらない");
    };
    let ats: Option<Vec<Ty>> = args.iter().map(conv).collect();
    let (Some(ats), Some(rt)) = (ats, conv(&ret)) else {
        return lw.err(line, "クロージャの引数か戻り値の型は段階 1 の対象外");
    };
    let names: Vec<String> = items(&f[0])
        .iter()
        .map(|p| match p {
            V::L(v) => st(&v[0]),
            _ => String::new(),
        })
        .collect();
    if names.len() != ats.len() {
        return lw.err(line, "クロージャの引数の個数が型と合わない");
    }
    let key = closure_key(lw, nid(e));
    let mut child = Lw::new(lw.p, lw.tys, lw.def);
    child.coll.prefix = lw.coll.prefix.clone();
    child.ret = rt;
    let (b0, pv) = child.new_block(&ats);
    debug_assert_eq!(b0, 0);
    child.cur = Some(0);
    child.scopes.push(vec![]);
    for ((n, v), t) in names.iter().zip(pv.iter()).zip(ats.iter()) {
        child.bind(n, *v, *t);
    }
    if let Some(v) = child.expr(&f[3])? {
        if child.vty(v) != rt {
            return lw.err(line, "クロージャの本体の型が戻り値の型と違う");
        }
        child.terminate(Term::Ret(v));
    }
    let mut blocks: Vec<Block> = child
        .blocks
        .into_iter()
        .map(|b| Block { params: b.params, insts: b.insts, term: b.term.unwrap_or(Term::Unreachable) })
        .collect();
    finish(&mut blocks, std::mem::take(&mut child.coll.allocas));
    let func = Func {
        fqn: key.clone(),
        key,
        uses: vec![],
        may_panic: lw.def.panics.as_deref() != Some("never"),
        stack: None,
        ret: rt,
        blocks,
    };
    lw.coll.extra.push((func, child.callees));
    let nested = std::mem::take(&mut child.coll.extra);
    lw.coll.extra.extend(nested);
    Ok(Some(lw.unit()))
}
