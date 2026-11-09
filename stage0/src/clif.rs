//! Cranelift バックエンド (stage0)。Cranelift の IR から直接オブジェクトファイルを作り、
//! C コンパイラをリンカとして使って実行ファイルにする。
//!
//! 対応する範囲:
//! - 整数 (i8..i64 / u8..u64 / isize / usize)・bool・f32 / f64。
//! - 整数の `+ - * / %` と単項 `-` は、オーバーフローとゼロ除算で panic (終了コード 101)。`wrapping_*`。
//! - `let` / 代入・`if` / `while` / `loop` / `break` / `continue` / `return`・`match` (列挙型・整数・bool)。
//! - 構造体 (`type`) と列挙型 (`enum`, ペイロード付き)。メモリ上に置き、値は先頭アドレスで持ち回る。
//!   `&T` / `&mut T` も同じアドレス。関数の戻り値が構造体・列挙型のときは、呼び出し側が確保した領域に書く (sret)。
//! - 関数呼び出し・`T.method(..)`・`std.io.print_line("..")`・`core.check.eq`・`core.mem.drop`。
//! 総称型・文字列・Vec・`Option` / `Result`・スカラーへの参照は未対応 (Err を返し、呼び出し側が別の経路に回す)。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{types, AbiParam, Block, InstBuilder, MemFlags, StackSlotData, StackSlotKind, TrapCode, Type, Value};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{default_libcall_names, DataDescription, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};

use crate::check::{items, nf, nname, st, strs, tag, tel};
use crate::parser::V;
use crate::program::Def;

type R<T> = Result<T, String>;
type Env = HashMap<String, (Variable, String)>;

fn is_int(p: &str) -> bool {
    matches!(p, "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize")
}

fn is_float(p: &str) -> bool {
    p == "f32" || p == "f64"
}

/// `&mut スカラー` を表す型名 (`mutref<u32>`)。値は要素や変数のアドレス。
fn is_mutref(p: &str) -> bool {
    p.starts_with("mutref<")
}

/// `mutref<T>` の `T`。
fn mutref_inner(p: &str) -> Option<&str> {
    p.strip_prefix("mutref<").and_then(|r| r.strip_suffix('>'))
}

fn is_scalar(p: &str) -> bool {
    is_int(p) || p == "bool" || is_float(p) || is_mutref(p)
}

/// 構造体・列挙型 (メモリ上に置く値) の型名か。
fn is_agg(p: &str) -> bool {
    p != "void" && !is_scalar(p)
}

fn cl(p: &str) -> Type {
    match p {
        "i8" | "u8" | "bool" => types::I8,
        "i16" | "u16" => types::I16,
        "i32" | "u32" => types::I32,
        "f32" => types::F32,
        "f64" => types::F64,
        _ => types::I64,
    }
}

fn signed(p: &str) -> bool {
    p.starts_with('i')
}

const STRING: &str = "alloc.string.String";

fn mangle(fqn: &str) -> String {
    format!("k_{}", fqn.replace('.', "_"))
}

fn align_up(n: u32, a: u32) -> u32 {
    (n + a - 1) / a * a
}

fn scalar_size(p: &str) -> u32 {
    match p {
        "i8" | "u8" | "bool" => 1,
        "i16" | "u16" => 2,
        "i32" | "u32" | "f32" => 4,
        _ => 8,
    }
}

/// 型の定義表とレイアウト計算。
struct Cx<'a> {
    types: &'a HashMap<String, &'a Def>,
    /// `Option<u32>` のような組み込みの総称列挙型の具体化: 名前 → [(バリアント名, ペイロードの型)]
    gen: RefCell<HashMap<String, Vec<(String, Vec<String>)>>>,
    /// `alloc.vec.Vec<u32>` のような Vec の具体化: 名前 → 要素の型
    vecs: RefCell<HashMap<String, String>>,
    /// `slice<u8>` のようなスライスの具体化: 名前 → 要素の型
    slices: RefCell<HashMap<String, String>>,
    /// `tuple<u32,usize>` のようなタプルの具体化: 名前 → 要素の型の列
    tuples: RefCell<HashMap<String, Vec<String>>>,
}

/// 構造体・列挙型のメモリ配置。
#[derive(Clone)]
struct Layout {
    size: u32,
    align: u32,
    /// 構造体: (フィールド名, 型, オフセット)
    fields: Vec<(String, String, u32)>,
    is_enum: bool,
    /// 列挙型: (バリアント名, [(型, オフセット)])
    variants: Vec<(String, Vec<(String, u32)>)>,
}

impl<'a> Cx<'a> {
    fn pty(&self, t: &V) -> R<String> {
        if tag(t) == "tuple" && items(tel(t, 1)).is_empty() {
            return Ok("void".into());
        }
        if tag(t) == "tuple" {
            let elems: Vec<String> = items(tel(t, 1)).iter().map(|x| self.pty_owned(x)).collect::<R<Vec<String>>>()?;
            if elems.iter().any(|e| e == "void") {
                return Err("tuple of unit".into());
            }
            return Ok(self.tuple_of(&elems));
        }
        match tag(t).as_str() {
            "prim" => {
                let p = st(tel(t, 1));
                if is_scalar(&p) {
                    return Ok(p);
                }
                if p == "str" {
                    return Ok(p);
                }
            }
            "ref" | "slice" => {
                let target = if tag(t) == "ref" { tel(t, 2) } else { t };
                if tag(target) == "slice" {
                    let elem = self.pty_owned(tel(target, 1))?;
                    return Ok(self.slice_of(&elem));
                }
                let inner = self.pty(target)?;
                if is_agg(&inner) {
                    return Ok(inner);
                }
                // 不変の参照 `&scalar` は、借用の間は値が変わらないので、値そのものとして扱う。
                if tag(t) == "ref" && !matches!(tel(t, 1), V::B(true)) {
                    return Ok(inner);
                }
                // `&mut scalar` は、要素や変数のアドレス (`mutref<T>`)。
                if tag(t) == "ref" && !is_mutref(&inner) {
                    return Ok(format!("mutref<{}>", inner));
                }
                return Err("mutable reference to a scalar".into());
            }
            "named" => {
                let n = st(tel(t, 1));
                if n == "alloc.vec.Vec" {
                    let first = items(tel(t, 2)).iter().find(|a| matches!(a, V::L(_))).ok_or("Vec without an element type")?;
                    let elem = self.pty_owned(first)?;
                    if elem == "void" {
                        return Err("Vec of unit".into());
                    }
                    return Ok(self.vec_of(&elem));
                }
                if n == "alloc.alloc.AllocError" || n == STRING {
                    return Ok(n);
                }
                if n == "Option" || n == "Result" {
                    let args: Vec<String> =
                        items(tel(t, 2)).iter().filter(|a| matches!(a, V::L(_))).map(|a| self.pty_owned(a)).collect::<R<Vec<String>>>()?;
                    return self.builtin_enum(&n, &args);
                }
                if let Some(d) = self.types.get(&n) {
                    if d.generics.is_empty() && (d.kw == "type" || d.kw == "enum") {
                        return Ok(n);
                    }
                }
            }
            _ => {}
        }
        Err(format!("type {}", t.dump()))
    }

    /// 参照でない型 (フィールド・ペイロード・要素の型に使う)。参照を値として持つと所有と取り違えるため。
    fn pty_owned(&self, t: &V) -> R<String> {
        if tag(t) == "ref" || tag(t) == "slice" {
            let r = self.pty(t)?;
            // `&str` / `&[T]` / `&scalar` は所有を伴わないのでよい。それ以外の参照は持てない。
            if r == "str" || r.starts_with("slice<") || !is_agg(&r) {
                return Ok(r);
            }
            return Err("a reference stored in a type".into());
        }
        self.pty(t)
    }

    fn tuple_of(&self, elems: &[String]) -> String {
        let name = format!("tuple<{}>", elems.join(","));
        self.tuples.borrow_mut().insert(name.clone(), elems.to_vec());
        name
    }

    fn slice_of(&self, elem: &str) -> String {
        let name = format!("slice<{}>", elem);
        self.slices.borrow_mut().insert(name.clone(), elem.to_string());
        name
    }

    /// Vec またはスライスの要素の型。
    fn seq_elem(&self, ty: &str) -> Option<String> {
        let v = self.vecs.borrow().get(ty).cloned();
        v.or_else(|| self.slices.borrow().get(ty).cloned())
    }

    /// 型 `ty` の値が、解放の必要なメモリ (Vec / String) を持つか。
    fn needs_drop(&self, ty: &str) -> bool {
        self.needs_drop_d(ty, 0)
    }

    fn needs_drop_d(&self, ty: &str, d: u32) -> bool {
        if d > 8 || !is_agg(ty) || ty == "str" || ty == "alloc.alloc.AllocError" {
            return false;
        }
        if ty == STRING || self.vecs.borrow().contains_key(ty) {
            return true;
        }
        match self.layout(ty, 0) {
            Ok(l) => {
                if l.is_enum {
                    l.variants.iter().any(|v| v.1.iter().any(|(t, _)| self.needs_drop_d(t, d + 1)))
                } else {
                    l.fields.iter().any(|f| self.needs_drop_d(&f.1, d + 1))
                }
            }
            Err(_) => false,
        }
    }

    fn layout(&self, name: &str, depth: u32) -> R<Layout> {
        if depth > 8 {
            return Err(format!("recursive type {}", name));
        }
        if name == "str" {
            return Ok(Layout {
                size: 16,
                align: 8,
                fields: vec![("ptr".into(), "usize".into(), 0), ("len".into(), "usize".into(), 8)],
                is_enum: false,
                variants: vec![],
            });
        }
        let tup = self.tuples.borrow().get(name).cloned();
        if let Some(elems) = tup {
            let mut off = 0u32;
            let mut align = 1u32;
            let mut fields = vec![];
            for (k, ty) in elems.iter().enumerate() {
                let (sz, al) = if is_scalar(ty) {
                    (scalar_size(ty), scalar_size(ty))
                } else {
                    let l = self.layout(ty, depth + 1)?;
                    (l.size, l.align)
                };
                off = align_up(off, al);
                fields.push((k.to_string(), ty.clone(), off));
                off += sz;
                align = align.max(al);
            }
            return Ok(Layout { size: align_up(off.max(1), align), align, fields, is_enum: false, variants: vec![] });
        }
        if self.slices.borrow().contains_key(name) {
            return Ok(Layout {
                size: 16,
                align: 8,
                fields: vec![("ptr".into(), "usize".into(), 0), ("len".into(), "usize".into(), 8)],
                is_enum: false,
                variants: vec![],
            });
        }
        if name == STRING || self.vecs.borrow().contains_key(name) {
            return Ok(Layout {
                size: 24,
                align: 8,
                fields: vec![("ptr".into(), "usize".into(), 0), ("len".into(), "usize".into(), 8), ("cap".into(), "usize".into(), 16)],
                is_enum: false,
                variants: vec![],
            });
        }
        if name == "alloc.alloc.AllocError" {
            return Ok(Layout { size: 1, align: 1, fields: vec![], is_enum: false, variants: vec![] });
        }
        let registered = self.gen.borrow().get(name).cloned();
        if let Some(vs) = registered {
            return self.enum_layout(vs, depth);
        }
        let d = self.types.get(name).ok_or_else(|| format!("unknown type {}", name))?;
        let fsize = |ty: &str| -> R<(u32, u32)> {
            if is_scalar(ty) {
                Ok((scalar_size(ty), scalar_size(ty)))
            } else {
                let l = self.layout(ty, depth + 1)?;
                Ok((l.size, l.align))
            }
        };
        if d.kw == "type" {
            let mut off = 0u32;
            let mut align = 1u32;
            let mut fields = vec![];
            for (n, t, _) in &d.fields {
                let ty = self.pty_owned(t)?;
                let (sz, al) = fsize(&ty)?;
                off = align_up(off, al);
                fields.push((n.clone(), ty, off));
                off += sz;
                align = align.max(al);
            }
            return Ok(Layout { size: align_up(off.max(1), align), align, fields, is_enum: false, variants: vec![] });
        }
        let mut vs = vec![];
        for (vn, tys) in &d.variants {
            let mut ts = vec![];
            for t in tys {
                ts.push(self.pty_owned(t)?);
            }
            vs.push((vn.clone(), ts));
        }
        self.enum_layout(vs, depth)
    }

    /// 先頭に 4 バイトのタグ、その後ろにバリアントごとのペイロードを重ねて置く。
    fn enum_layout(&self, vs: Vec<(String, Vec<String>)>, depth: u32) -> R<Layout> {
        let fsize = |ty: &str| -> R<(u32, u32)> {
            if is_scalar(ty) {
                Ok((scalar_size(ty), scalar_size(ty)))
            } else {
                let l = self.layout(ty, depth + 1)?;
                Ok((l.size, l.align))
            }
        };
        let mut max_align = 4u32;
        let mut sizes = vec![];
        let mut rel: Vec<Vec<(String, u32)>> = vec![];
        for (_, tys) in &vs {
            let mut cur = 0u32;
            let mut fs = vec![];
            for ty in tys {
                let (sz, al) = fsize(ty)?;
                cur = align_up(cur, al);
                fs.push((ty.clone(), cur));
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
            .map(|((vn, _), fs)| (vn.clone(), fs.into_iter().map(|(ty, off)| (ty, start + off)).collect()))
            .collect();
        Ok(Layout { size: align_up(start + max_payload, max_align), align: max_align, fields: vec![], is_enum: true, variants })
    }

    fn vec_of(&self, elem: &str) -> String {
        let name = format!("alloc.vec.Vec<{}>", elem);
        self.vecs.borrow_mut().insert(name.clone(), elem.to_string());
        name
    }

    fn elem_of(&self, vty: &str) -> Option<String> {
        self.vecs.borrow().get(vty).cloned()
    }

    /// 組み込み型 (str / Vec / AllocError) のメソッドの戻り値の型。
    fn builtin_method_ty(&self, recv_ty: &str, name: &str) -> Option<String> {
        if is_int(recv_ty) && name.starts_with("checked_") {
            return self.builtin_enum("Option", &[recv_ty.to_string()]).ok();
        }
        if recv_ty.starts_with("Option<") || recv_ty.starts_with("Result<") {
            let ok_idx = if recv_ty.starts_with("Option<") { 1 } else { 0 };
            return match name {
                "is_some" | "is_none" if ok_idx == 1 => Some("bool".into()),
                "is_ok" | "is_err" if ok_idx == 0 => Some("bool".into()),
                "unwrap" | "expect" | "unwrap_or" => {
                    let l = self.layout(recv_ty, 0).ok()?;
                    Some(l.variants.get(ok_idx)?.1.first().map(|x| x.0.clone()).unwrap_or_else(|| "void".to_string()))
                }
                _ => None,
            };
        }
        if recv_ty == "str" {
            return match name {
                "len" => Some("usize".into()),
                "is_empty" => Some("bool".into()),
                "as_bytes" => Some(self.slice_of("u8")),
                _ => None,
            };
        }
        if let Some(elem) = self.slices.borrow().get(recv_ty).cloned() {
            return self.seq_method_ty(&elem, name);
        }
        if recv_ty == STRING {
            return match name {
                "len" => Some("usize".into()),
                "is_empty" => Some("bool".into()),
                "clear" => Some("void".into()),
                "as_str" => Some("str".into()),
                "push_str" => self.builtin_enum("Result", &["void".into(), "alloc.alloc.AllocError".into()]).ok(),
                _ => None,
            };
        }
        let elem = self.elem_of(recv_ty)?;
        if let Some(t) = self.seq_method_ty(&elem, name) {
            if name != "len" && name != "is_empty" {
                return Some(t);
            }
        }
        match name {
            "len" | "capacity" => Some("usize".into()),
            "is_empty" => Some("bool".into()),
            "clear" => Some("void".into()),
            "pop" => self.builtin_enum("Option", &[elem]).ok(),
            "push" => self.builtin_enum("Result", &["void".into(), "alloc.alloc.AllocError".into()]).ok(),
            _ => None,
        }
    }

    /// スライス・Vec に共通のメソッドの戻り値の型 (要素がスカラーのときの `get` など)。
    fn seq_method_ty(&self, elem: &str, name: &str) -> Option<String> {
        match name {
            "len" => Some("usize".into()),
            "is_empty" => Some("bool".into()),
            "get" | "first" | "last" if is_scalar(elem) => self.builtin_enum("Option", &[elem.to_string()]).ok(),
            "get_mut" if is_scalar(elem) && !is_mutref(elem) => self.builtin_enum("Option", &[format!("mutref<{}>", elem)]).ok(),
            "sub" => {
                let sl = self.slice_of(elem);
                self.builtin_enum("Option", &[sl]).ok()
            }
            "as_slice" | "as_mut_slice" => Some(self.slice_of(elem)),
            _ => None,
        }
    }

    /// `Option<T>` / `Result<T, E>` の具体化を登録して、その名前を返す。`()` はペイロードなし。
    fn builtin_enum(&self, base: &str, args: &[String]) -> R<String> {
        let name = format!("{}<{}>", base, args.join(","));
        let payload = |t: &String| if t == "void" { vec![] } else { vec![t.clone()] };
        let vs = if base == "Option" {
            if args.len() != 1 {
                return Err("Option needs one type argument".into());
            }
            vec![("None".to_string(), vec![]), ("Some".to_string(), payload(&args[0]))]
        } else {
            if args.len() != 2 {
                return Err("Result needs two type arguments".into());
            }
            vec![("Ok".to_string(), payload(&args[0])), ("Err".to_string(), payload(&args[1]))]
        };
        self.gen.borrow_mut().insert(name.clone(), vs);
        Ok(name)
    }

    /// `A.B.C` を (列挙型 `A.B`, バリアント `C`) に分ける。
    fn split_variant(&self, segs: &[String]) -> Option<(String, usize, String)> {
        if segs.len() < 2 {
            return None;
        }
        let en = segs[..segs.len() - 1].join(".");
        let d = self.types.get(&en)?;
        if d.kw != "enum" {
            return None;
        }
        let vn = segs[segs.len() - 1].clone();
        let idx = d.variants.iter().position(|v| v.0 == vn)?;
        Some((en, idx, vn))
    }
}

/// ランタイムとして使う C ライブラリ関数の ID。
struct Rt {
    puts: FuncId,
    write: FuncId,
    exit: FuncId,
    realloc: FuncId,
    free: FuncId,
    memcpy: FuncId,
    memcmp: FuncId,
}

struct FnGen<'a, 'b> {
    b: FunctionBuilder<'b>,
    module: &'a mut ObjectModule,
    cx: &'a Cx<'a>,
    funcs: &'a HashMap<String, &'a Def>,
    ids: &'a HashMap<String, FuncId>,
    rt: &'a Rt,
    data_n: &'a mut usize,
    entry: bool,
    ret: V,
    ret_ty: String,
    sret: Option<Value>,
    /// (ループの先頭, 出口, ループに入る前のスコープの深さ)
    loops: Vec<(Block, Block, usize)>,
    term: bool,
    nvar: usize,
    /// スコープごとの、所有する変数 (先頭アドレスを持つ変数の番号) の列。
    scopes: Vec<Vec<u32>>,
    owners: HashMap<u32, Owner>,
    /// 参照 (`&T` / `&mut T`) である変数の番号。値のコピーではなく別名として扱う。
    refs: HashSet<u32>,
    /// `&mut x` が取られるスカラーの局所変数の名前 (スタックのスロットに置く)
    addr_taken: HashSet<String>,
    /// いま処理している文の後ろに続く文 (型を使われ方から決めるために先読みする)
    rest: Vec<V>,
    /// スロットに置いた変数の番号 (変数の値はスロットのアドレス)
    promoted: HashSet<u32>,
}

/// 解放の責任を持つ変数。`flag` が真の間だけ、スコープの終わりで解放する (移動したら偽にする)。
#[derive(Clone)]
struct Owner {
    ptr: Variable,
    flag: Variable,
    ty: String,
}

impl<'a, 'b> FnGen<'a, 'b> {
    /// 直前で終端した (return / break など) なら、到達できない新しいブロックに移る。
    fn live(&mut self) {
        if self.term {
            let nb = self.b.create_block();
            self.b.switch_to_block(nb);
            self.term = false;
        }
    }

    fn ptr_ty(&self) -> Type {
        self.module.target_config().pointer_type()
    }

    // ------------------------------------------------------------ 型の決定 (コードを出さない)
    fn field_chain_ty(&self, segs: &[String], env: &Env) -> Option<String> {
        let mut ty = env.get(&segs[0])?.1.clone();
        for f in &segs[1..] {
            if !is_agg(&ty) {
                return None;
            }
            let l = self.cx.layout(&ty, 0).ok()?;
            ty = l.fields.iter().find(|x| &x.0 == f)?.1.clone();
        }
        Some(ty)
    }

    fn method_target(&self, recv_ty: &str, name: &str) -> Option<&'a Def> {
        self.funcs.get(&format!("{}.{}", recv_ty, name)).copied()
    }

    /// `Some(x)` / `Ok(x)` / `Err(e)` が作る列挙型の名前。期待される型があればそれ、なければ引数から決める。
    fn ctor_type(&self, ctor: &str, args: &[V], env: &Env, expect: Option<&str>) -> Option<String> {
        let want = if ctor == "Some" { "Option<" } else { "Result<" };
        if let Some(t) = expect.filter(|t| t.starts_with(want)) {
            return Some(t.to_string());
        }
        if ctor == "Some" {
            let a = self.ty_of(args.first()?, env, None)?;
            return self.cx.builtin_enum("Option", &[a]).ok();
        }
        None
    }

    /// 型注釈のない `let x = Vec.new(..)` / `let x = None` の型を、後ろの文での使われ方から決める。
    fn infer_let_type(&self, init: &V, name: &str, env: &Env) -> Option<String> {
        let is_vec_new = nname(init) == "Call"
            && nname(&nf(init)[0]) == "Path"
            && strs(&nf(&nf(init)[0])[0]) == ["alloc", "vec", "Vec", "new"];
        let is_none = nname(init) == "Path" && strs(&nf(init)[0]) == ["None"];
        if !is_vec_new && !is_none {
            return None;
        }
        let fits = |t: &str| if is_vec_new { self.cx.elem_of(t).is_some() } else { t.starts_with("Option<") };
        let refers = |a: &V| -> bool {
            let a = if nname(a) == "Unary" && matches!(st(&nf(a)[0]).as_str(), "&" | "&mut") { &nf(a)[1] } else { a };
            nname(a) == "Path" && strs(&nf(a)[0]) == [name.to_string()]
        };
        let rest = self.rest.clone();
        let mut nodes: Vec<&V> = vec![];
        for st_ in &rest {
            crate::check::walk(st_, &mut nodes);
        }
        for n in &nodes {
            match nname(n) {
                // v.push(x) の x の型
                "MCall" if is_vec_new && st(&nf(n)[1]) == "push" && items(&nf(n)[2]).len() == 1 => {
                    let r = &nf(n)[0];
                    if nname(r) == "Path" && strs(&nf(r)[0]) == [name.to_string()] {
                        if let Some(t) = self.ty_of(&items(&nf(n)[2])[0], env, None) {
                            if t != "void" && !is_mutref(&t) {
                                return Some(self.cx.vec_of(&t));
                            }
                        }
                    }
                }
                "Call" if nname(&nf(n)[0]) == "Path" => {
                    let segs = strs(&nf(&nf(n)[0])[0]);
                    let args = items(&nf(n)[1]);
                    if is_vec_new && segs.len() == 2 && segs[0] == name && segs[1] == "push" && args.len() == 1 {
                        if let Some(t) = self.ty_of(&args[0], env, None) {
                            if t != "void" && !is_mutref(&t) {
                                return Some(self.cx.vec_of(&t));
                            }
                        }
                    }
                    // 関数の引数に渡される: その引数の型
                    if let Some(d) = self.funcs.get(&segs.join(".")).copied() {
                        for (k, a) in args.iter().enumerate() {
                            if refers(a) {
                                if let Some((_, pt)) = d.params.get(k) {
                                    if let Ok(t) = self.cx.pty(pt) {
                                        if fits(&t) {
                                            return Some(t);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // 構造体リテラルのフィールドに入る: そのフィールドの型
                "StructLit" => {
                    let sname = st(&nf(n)[0]);
                    if let Ok(l) = self.cx.layout(&sname, 0) {
                        for fl in items(&nf(n)[1]) {
                            let fi = items(fl);
                            if refers(&fi[1]) {
                                let fname = st(&fi[0]);
                                if let Some((_, fty, _)) = l.fields.iter().find(|x| x.0 == fname) {
                                    if fits(fty) {
                                        return Some(fty.clone());
                                    }
                                }
                            }
                        }
                    }
                }
                // x = Some(e)
                "Assign" if is_none => {
                    let tg = &nf(n)[0];
                    let rhs = &nf(n)[1];
                    if nname(tg) == "Path" && strs(&nf(tg)[0]) == [name.to_string()] {
                        if nname(rhs) == "Call" && nname(&nf(rhs)[0]) == "Path" && strs(&nf(&nf(rhs)[0])[0]) == ["Some"] {
                            if let Some(t) = self.ty_of(&items(&nf(rhs)[1])[0], env, None) {
                                return self.cx.builtin_enum("Option", &[t]).ok();
                            }
                        }
                    }
                }
                // return v
                "Return" if !matches!(nf(n)[0], V::Nil) && refers(&nf(n)[0]) => {
                    if fits(&self.ret_ty) {
                        return Some(self.ret_ty.clone());
                    }
                }
                _ => {}
            }
        }
        // 関数の最後の式として返される
        if let Some(last) = rest.last() {
            if nname(last) == "ExprStmt" && refers(&nf(last)[0]) && fits(&self.ret_ty) {
                return Some(self.ret_ty.clone());
            }
        }
        None
    }

    fn body_ty(&self, body: &V, env: &Env, expect: Option<&str>) -> Option<String> {
        if nname(body) == "Block" {
            let stmts = items(&nf(body)[0]);
            let last = stmts.last()?;
            if nname(last) == "ExprStmt" {
                return self.ty_of(&nf(last)[0], env, expect);
            }
            return None;
        }
        self.ty_of(body, env, expect)
    }

    fn ty_of(&self, e: &V, env: &Env, expect: Option<&str>) -> Option<String> {
        let f = nf(e);
        match nname(e) {
            "Lit" => match st(&f[0]).as_str() {
                "int" => {
                    let sfx = if matches!(f[2], V::Nil) { None } else { Some(st(&f[2])) };
                    sfx.or(expect.map(String::from)).filter(|t| is_int(t))
                }
                "bool" => Some("bool".into()),
                "float" => {
                    let sfx = if matches!(f[1], V::Nil) { None } else { Some(st(&f[1])) };
                    sfx.or(expect.map(String::from)).filter(|t| is_float(t))
                }
                "str" => Some("str".into()),
                _ => None,
            },
            "Index" => {
                let bt = self.ty_of(&f[0], env, None)?;
                self.cx.seq_elem(&bt)
            }
            "Path" => {
                let segs = strs(&f[0]);
                if env.contains_key(&segs[0]) {
                    return self.field_chain_ty(&segs, env);
                }
                if segs == ["None"] {
                    return expect.filter(|t| t.starts_with("Option<")).map(String::from);
                }
                self.cx.split_variant(&segs).map(|v| v.0)
            }
            "Field" => {
                let segs = place_segs(e)?;
                if env.contains_key(&segs[0]) { self.field_chain_ty(&segs, env) } else { None }
            }
            "Try" => {
                let t = self.ty_of(&f[0], env, None)?;
                let ok_idx = if t.starts_with("Option<") { 1 } else if t.starts_with("Result<") { 0 } else { return None };
                let l = self.cx.layout(&t, 0).ok()?;
                Some(l.variants.get(ok_idx)?.1.first().map(|x| x.0.clone()).unwrap_or_else(|| "void".to_string()))
            }
            "TupleE" => {
                let want: Option<Vec<String>> = expect.and_then(|t| self.cx.tuples.borrow().get(t).cloned());
                let mut elems = vec![];
                for (k, x) in items(&f[0]).iter().enumerate() {
                    let ex = want.as_ref().and_then(|w| w.get(k)).map(|s| s.as_str());
                    elems.push(self.ty_of(x, env, ex)?);
                }
                Some(self.cx.tuple_of(&elems))
            }
            "StructLit" => {
                let n = st(&f[0]);
                if self.cx.types.contains_key(&n) { Some(n) } else { None }
            }
            "Bin" => {
                let op = st(&f[0]);
                if matches!(op.as_str(), "&&" | "||" | "==" | "!=" | "<" | "<=" | ">" | ">=") {
                    return Some("bool".into());
                }
                self.ty_of(&f[1], env, expect).or_else(|| self.ty_of(&f[2], env, expect))
            }
            "Unary" => match st(&f[0]).as_str() {
                "!" | "-" | "&" => self.ty_of(&f[1], env, expect),
                "&mut" => {
                    let t = self.ty_of(&f[1], env, expect)?;
                    if is_agg(&t) || is_mutref(&t) { Some(t) } else { Some(format!("mutref<{}>", t)) }
                }
                "*" => {
                    let t = self.ty_of(&f[1], env, None)?;
                    Some(mutref_inner(&t).map(String::from).unwrap_or(t))
                }
                _ => None,
            },
            "Match" => {
                for arm in items(&f[1]) {
                    let a = items(arm);
                    if let Some(t) = self.body_ty(&a[1], env, expect) {
                        return Some(t);
                    }
                }
                None
            }
            "MCall" => {
                let name = st(&f[1]);
                let recv_ty = self.ty_of(&f[0], env, expect).map(|t| mutref_inner(&t).map(String::from).unwrap_or(t));
                if let Some(rt) = &recv_ty {
                    if let Some(t) = self.cx.builtin_method_ty(rt, &name) {
                        return Some(t);
                    }
                    if is_agg(rt) {
                        return self.method_target(rt, &name).and_then(|d| self.cx.pty(&d.ret).ok());
                    }
                }
                self.ty_of(&f[0], env, expect).or_else(|| items(&f[2]).first().and_then(|a| self.ty_of(a, env, expect)))
            }
            "Call" => {
                if nname(&f[0]) != "Path" {
                    return None;
                }
                let segs = strs(&nf(&f[0])[0]);
                if segs.len() >= 2 && env.contains_key(&segs[0]) {
                    let recv_ty = self.field_chain_ty(&segs[..segs.len() - 1], env)?;
                    let name = &segs[segs.len() - 1];
                    if let Some(t) = self.cx.builtin_method_ty(&recv_ty, name) {
                        return Some(t);
                    }
                    if is_agg(&recv_ty) {
                        return self.method_target(&recv_ty, name).and_then(|d| self.cx.pty(&d.ret).ok());
                    }
                    return Some(recv_ty);
                }
                if let Some((en, _, _)) = self.cx.split_variant(&segs) {
                    return Some(en);
                }
                if segs.len() >= 3 {
                    if let Some((en, _, _)) = self.cx.split_variant(&segs[..segs.len() - 1]) {
                        let name = &segs[segs.len() - 1];
                        return self.method_target(&en, name).and_then(|d| self.cx.pty(&d.ret).ok());
                    }
                }
                if segs.len() == 1 && matches!(segs[0].as_str(), "Some" | "Ok" | "Err") && !self.funcs.contains_key(&segs[0]) {
                    return self.ctor_type(&segs[0], items(&f[1]), env, expect);
                }
                if segs == ["alloc", "vec", "Vec", "new"] {
                    return expect.filter(|t| self.cx.elem_of(t).is_some()).map(String::from);
                }
                if segs == ["alloc", "string", "String", "new"] {
                    return Some(STRING.to_string());
                }
                if segs == ["alloc", "string", "String", "from_str"] {
                    return self.cx.builtin_enum("Result", &[STRING.to_string(), "alloc.alloc.AllocError".to_string()]).ok();
                }
                if segs == ["std", "io", "print_line"] {
                    return Some("void".into());
                }
                if segs == ["core", "check", "eq"] || segs == ["core", "mem", "drop"] {
                    return Some("void".into());
                }
                self.funcs.get(&segs.join(".")).and_then(|d| self.cx.pty(&d.ret).ok())
            }
            _ => None,
        }
    }

    // ------------------------------------------------------------ 補助
    fn new_var(&mut self, ty: &str) -> Variable {
        let v = Variable::from_u32(self.nvar as u32);
        self.nvar += 1;
        self.b.declare_var(v, cl(ty));
        v
    }

    fn call_rt(&mut self, id: FuncId, args: &[Value]) -> Vec<Value> {
        let fr = self.module.declare_func_in_func(id, self.b.func);
        let c = self.b.ins().call(fr, args);
        self.b.inst_results(c).to_vec()
    }

    fn string_ptr(&mut self, bytes: Vec<u8>) -> R<Value> {
        let name = format!("ksn_str_{}", *self.data_n);
        *self.data_n += 1;
        let id = self.module.declare_data(&name, Linkage::Local, false, false).map_err(|e| e.to_string())?;
        let mut desc = DataDescription::new();
        desc.define(bytes.into_boxed_slice());
        self.module.define_data(id, &desc).map_err(|e| e.to_string())?;
        let gv = self.module.declare_data_in_func(id, self.b.func);
        let pt = self.ptr_ty();
        Ok(self.b.ins().global_value(pt, gv))
    }

    /// スタック上に `size` バイトの領域を確保して、先頭アドレスを返す。
    fn alloc(&mut self, size: u32, align: u32) -> Value {
        let shift = align.max(1).trailing_zeros() as u8;
        let ss = self.b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size.max(1), shift));
        let pt = self.ptr_ty();
        self.b.ins().stack_addr(pt, ss, 0)
    }

    fn alloc_for(&mut self, ty: &str) -> R<Value> {
        let l = self.cx.layout(ty, 0)?;
        Ok(self.alloc(l.size, l.align))
    }

    fn copy_mem(&mut self, dst: Value, src: Value, size: u32) {
        let cfg = self.module.target_config();
        self.b.emit_small_memory_copy(cfg, dst, src, size as u64, 1, 1, true, MemFlags::new());
    }

    /// 型 `ty` の値 `v` を、アドレス `addr` に書く (スカラーは store、構造体・列挙型はコピー)。
    fn store_value(&mut self, addr: Value, ty: &str, v: Value) -> R<()> {
        if is_agg(ty) {
            let l = self.cx.layout(ty, 0)?;
            self.copy_mem(addr, v, l.size);
        } else {
            self.b.ins().store(MemFlags::new(), v, addr, 0);
        }
        Ok(())
    }

    /// アドレス `addr` から型 `ty` の値を読む (構造体・列挙型はアドレスそのもの)。
    fn load_value(&mut self, addr: Value, ty: &str) -> Value {
        if is_agg(ty) { addr } else { self.b.ins().load(cl(ty), MemFlags::new(), addr, 0) }
    }

    /// 式が一時的な領域を新しく作る (コピーせず束縛してよい) か。
    fn is_fresh(&self, e: &V) -> bool {
        match nname(e) {
            "StructLit" | "MCall" | "TupleE" => true,
            "Try" => self.is_fresh(&nf(e)[0]),
            "Call" => !(nname(&nf(e)[0]) == "Path" && strs(&nf(&nf(e)[0])[0]).len() >= 2 && false),
            "Path" => {
                let segs = strs(&nf(e)[0]);
                self.cx.split_variant(&segs).is_some()
            }
            _ => false,
        }
    }

    /// `a.b.c` の最後のフィールドのアドレスと型。
    fn field_addr(&mut self, segs: &[String], env: &Env) -> R<(Value, String)> {
        let (var, mut ty) = env.get(&segs[0]).cloned().ok_or("not a local")?;
        let mut p = self.b.use_var(var);
        for f in &segs[1..] {
            if !is_agg(&ty) {
                return Err(format!("field {} of a scalar", f));
            }
            let l = self.cx.layout(&ty, 0)?;
            let (_, fty, off) = l.fields.iter().find(|x| &x.0 == f).cloned().ok_or_else(|| format!("no field {}", f))?;
            p = self.b.ins().iadd_imm(p, off as i64);
            ty = fty;
        }
        Ok((p, ty))
    }

    // ------------------------------------------------------------ 所有と解放
    fn register_owner(&mut self, ptr: Variable, ty: &str) {
        let flag = self.new_var("bool");
        let one = self.b.ins().iconst(types::I8, 1);
        self.b.def_var(flag, one);
        let id = ptr.as_u32();
        self.owners.insert(id, Owner { ptr, flag, ty: ty.to_string() });
        if let Some(sc) = self.scopes.last_mut() {
            sc.push(id);
        }
    }

    /// 式 `e` の根の変数が所有者なら、そのフラグ。
    fn root_owner(&self, e: &V, env: &Env) -> Option<u32> {
        let segs = place_segs(e)?;
        let var = env.get(&segs[0])?.0;
        let id = var.as_u32();
        if self.owners.contains_key(&id) { Some(id) } else { None }
    }

    /// 値 `e` (型 `ty`) を別の持ち主へ渡す (移動)。元が所有者ならフラグを下ろす。
    fn consume(&mut self, e: &V, ty: &str, env: &Env) -> R<()> {
        if !is_agg(ty) || !self.cx.needs_drop(ty) || self.is_fresh(e) {
            return Ok(());
        }
        match self.root_owner(e, env) {
            Some(id) => {
                let flag = self.owners[&id].flag;
                let z = self.b.ins().iconst(types::I8, 0);
                self.b.def_var(flag, z);
                Ok(())
            }
            None => Err("move out of a value this function does not own".into()),
        }
    }

    fn drop_owner_if_live(&mut self, id: u32) -> R<()> {
        let o = self.owners[&id].clone();
        let fl = self.b.use_var(o.flag);
        let (d, skip) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(fl, d, &[], skip, &[]);
        self.b.switch_to_block(d);
        let p = self.b.use_var(o.ptr);
        self.drop_value(p, &o.ty)?;
        let z = self.b.ins().iconst(types::I8, 0);
        self.b.def_var(o.flag, z);
        self.b.ins().jump(skip, &[]);
        self.b.switch_to_block(skip);
        Ok(())
    }

    /// 深さ `depth` 以降のスコープの所有者をすべて (内側から) 解放する。スコープ自体は残す。
    fn drop_scopes_from(&mut self, depth: usize) -> R<()> {
        for i in (depth..self.scopes.len()).rev() {
            let ids = self.scopes[i].clone();
            for id in ids.iter().rev() {
                self.drop_owner_if_live(*id)?;
            }
        }
        Ok(())
    }

    /// アドレス `addr` にある型 `ty` の値が持つメモリを解放する。
    fn drop_value(&mut self, addr: Value, ty: &str) -> R<()> {
        let i64t = types::I64;
        let pt = self.ptr_ty();
        if ty == STRING || self.cx.elem_of(ty).is_some() {
            if let Some(elem) = self.cx.elem_of(ty) {
                if self.cx.needs_drop(&elem) {
                    let sz = self.elem_size(&elem)? as i64;
                    let len = self.b.ins().load(i64t, MemFlags::new(), addr, 8);
                    let base = self.b.ins().load(pt, MemFlags::new(), addr, 0);
                    let (head, body, exit) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                    self.b.append_block_param(head, i64t);
                    let zero = self.b.ins().iconst(i64t, 0);
                    self.b.ins().jump(head, &[zero]);
                    self.b.switch_to_block(head);
                    let i = self.b.block_params(head)[0];
                    let c = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
                    self.b.ins().brif(c, body, &[], exit, &[]);
                    self.b.switch_to_block(body);
                    let off = self.b.ins().imul_imm(i, sz);
                    let ea = self.b.ins().iadd(base, off);
                    self.drop_value(ea, &elem)?;
                    let i1 = self.b.ins().iadd_imm(i, 1);
                    self.b.ins().jump(head, &[i1]);
                    self.b.switch_to_block(exit);
                }
            }
            let ptr = self.b.ins().load(pt, MemFlags::new(), addr, 0);
            let fr = self.rt.free;
            self.call_rt(fr, &[ptr]);
            return Ok(());
        }
        let l = self.cx.layout(ty, 0)?;
        if !l.is_enum {
            for (_, fty, off) in &l.fields {
                if self.cx.needs_drop(fty) {
                    let a = self.b.ins().iadd_imm(addr, *off as i64);
                    self.drop_value(a, fty)?;
                }
            }
            return Ok(());
        }
        let tg = self.b.ins().load(types::I32, MemFlags::new(), addr, 0);
        for (idx, (_, fields)) in l.variants.iter().enumerate() {
            if !fields.iter().any(|(t, _)| self.cx.needs_drop(t)) {
                continue;
            }
            let c = self.b.ins().icmp_imm(IntCC::Equal, tg, idx as i64);
            let (yes, next) = (self.b.create_block(), self.b.create_block());
            self.b.ins().brif(c, yes, &[], next, &[]);
            self.b.switch_to_block(yes);
            for (fty, off) in fields {
                if self.cx.needs_drop(fty) {
                    let a = self.b.ins().iadd_imm(addr, *off as i64);
                    self.drop_value(a, fty)?;
                }
            }
            self.b.ins().jump(next, &[]);
            self.b.switch_to_block(next);
        }
        Ok(())
    }

    /// 式 `e` が参照 (`&x` / 参照である変数) か。参照はコピーせず別名にする。
    fn is_ref_expr(&self, e: &V, env: &Env) -> bool {
        match nname(e) {
            "Unary" => matches!(st(&nf(e)[0]).as_str(), "&" | "&mut"),
            "Path" => {
                let segs = strs(&nf(e)[0]);
                segs.len() == 1 && env.get(&segs[0]).map_or(false, |v| self.refs.contains(&v.0.as_u32()))
            }
            _ => false,
        }
    }

    /// 条件 `bad` が真なら panic して終了コード 101 で終わる。
    fn guard(&mut self, bad: Value, msg: &str) -> R<()> {
        let (pb, ok) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(bad, pb, &[], ok, &[]);
        self.b.switch_to_block(pb);
        self.fail_block(msg)?;
        self.b.switch_to_block(ok);
        Ok(())
    }

    fn fail_block(&mut self, text: &str) -> R<()> {
        let msg = format!("panic: {}{}", text, char::from(10u8)).into_bytes();
        let n = msg.len() as i64;
        let p = self.string_ptr(msg)?;
        let fd = self.b.ins().iconst(types::I32, 2);
        let len = self.b.ins().iconst(types::I32, n);
        let w = self.rt.write;
        self.call_rt(w, &[fd, p, len]);
        let code = self.b.ins().iconst(types::I32, 101);
        let x = self.rt.exit;
        self.call_rt(x, &[code]);
        self.b.ins().trap(TrapCode::user(1).unwrap());
        Ok(())
    }

    // ------------------------------------------------------------ 関数・文
    fn func(&mut self, d: &Def, params_vals: &[Value]) -> R<()> {
        let mut env: Env = HashMap::new();
        let mut k = 0;
        self.scopes.push(vec![]);
        {
            let mut nodes: Vec<&V> = vec![];
            crate::check::walk(d.body.as_ref().unwrap(), &mut nodes);
            for n in nodes {
                if nname(n) == "Unary" && st(&nf(n)[0]) == "&mut" && nname(&nf(n)[1]) == "Path" {
                    let segs = strs(&nf(&nf(n)[1])[0]);
                    if segs.len() == 1 {
                        self.addr_taken.insert(segs[0].clone());
                    }
                }
            }
        }
        if is_agg(&self.ret_ty) && !self.entry {
            self.sret = Some(params_vals[0]);
            k = 1;
        }
        for (n, t) in d.params.iter() {
            let ty = self.cx.pty(t)?;
            let mut v = params_vals[k];
            k += 1;
            if is_agg(&ty) && tag(t) != "ref" {
                // 値渡しの構造体・列挙型は、呼び出し側の領域を書き換えないよう自分の領域にコピーする。
                let dst = self.alloc_for(&ty)?;
                let size = self.cx.layout(&ty, 0)?.size;
                self.copy_mem(dst, v, size);
                v = dst;
            }
            let var = self.new_var(&ty);
            if !is_agg(&ty) && !is_mutref(&ty) && self.addr_taken.contains(n) {
                // `&mut n` を取られる引数は、スロットに移す。
                let slot = self.alloc(8, 8);
                self.b.ins().store(MemFlags::new(), v, slot, 0);
                let pvar = self.new_var("usize");
                self.b.def_var(pvar, slot);
                self.promoted.insert(pvar.as_u32());
                env.insert(n.clone(), (pvar, ty));
                continue;
            }
            self.b.def_var(var, v);
            if is_agg(&ty) && tag(t) == "ref" {
                self.refs.insert(var.as_u32());
            } else if is_agg(&ty) && self.cx.needs_drop(&ty) {
                self.register_owner(var, &ty);
            }
            env.insert(n.clone(), (var, ty));
        }
        let stmts = items(&nf(d.body.as_ref().unwrap())[0]).to_vec();
        for (i, s) in stmts.iter().enumerate() {
            self.rest = stmts[i + 1..].to_vec();
            self.stmt(s, &mut env, i == stmts.len() - 1)?;
        }
        if !self.term {
            self.drop_scopes_from(0)?;
            if self.entry {
                let z = self.b.ins().iconst(types::I32, 0);
                self.b.ins().return_(&[z]);
            } else if self.ret_ty == "void" {
                self.b.ins().return_(&[]);
            } else {
                self.b.ins().trap(TrapCode::user(2).unwrap());
            }
        }
        Ok(())
    }

    fn block(&mut self, b: &V, env: &mut Env, tail: bool) -> R<()> {
        let stmts = items(&nf(b)[0]).to_vec();
        self.scopes.push(vec![]);
        for (i, s) in stmts.iter().enumerate() {
            self.rest = stmts[i + 1..].to_vec();
            self.stmt(s, env, tail && i == stmts.len() - 1)?;
        }
        if !self.term {
            let d = self.scopes.len() - 1;
            self.drop_scopes_from(d)?;
        }
        self.scopes.pop();
        Ok(())
    }

    fn stmt(&mut self, s: &V, env: &mut Env, tail: bool) -> R<()> {
        self.live();
        let f = nf(s);
        match nname(s) {
            "Let" if nname(&f[0]) == "PTuple" => {
                let pats = items(&nf(&f[0])[0]).to_vec();
                let ann = if matches!(f[2], V::Nil) { None } else { Some(self.cx.pty(&f[2])?) };
                let ty = match ann.or_else(|| self.ty_of(&f[3], env, None)) {
                    Some(t) if self.cx.tuples.borrow().contains_key(&t) => t,
                    _ => return Err("destructuring of an unknown type".into()),
                };
                let (v, _) = self.expr(&f[3], env, Some(&ty))?;
                let v = v.ok_or("destructuring of unit")?;
                let l = self.cx.layout(&ty, 0)?;
                if pats.len() != l.fields.len() {
                    return Err("tuple pattern arity".into());
                }
                let fresh = self.is_fresh(&f[3]);
                // 解放が要る要素を取り出すには、元が新しい値か、所有者の変数でなければならない。
                let moves_out = l.fields.iter().any(|x| self.cx.needs_drop(&x.1));
                if moves_out {
                    if fresh {
                    } else if self.root_owner(&f[3], env).is_some() {
                        self.consume(&f[3], &ty, env)?;
                    } else {
                        return Err("destructuring a droppable value this function does not own".into());
                    }
                }
                for (pt, (_, fty, off)) in pats.iter().zip(l.fields.iter()) {
                    match nname(pt) {
                        "PBind" => {
                            let addr = self.b.ins().iadd_imm(v, *off as i64);
                            let val = self.load_value(addr, fty);
                            let var = self.new_var(fty);
                            self.b.def_var(var, val);
                            if is_agg(fty) && self.cx.needs_drop(fty) {
                                self.register_owner(var, fty);
                            } else if is_agg(fty) {
                                // 値のコピーを持つ (元の組を書き換えても影響しない)
                                let dst = self.alloc_for(fty)?;
                                let size = self.cx.layout(fty, 0)?.size;
                                self.copy_mem(dst, val, size);
                                self.b.def_var(var, dst);
                            }
                            env.insert(st(&nf(pt)[0]), (var, fty.clone()));
                        }
                        "PWild" => {}
                        _ => return Err("nested pattern in let".into()),
                    }
                }
                Ok(())
            }
            "Let" => {
                if nname(&f[0]) != "PBind" {
                    return Err("destructuring let".into());
                }
                let ann = if matches!(f[2], V::Nil) { None } else { Some(self.cx.pty(&f[2])?) };
                let lname = st(&nf(&f[0])[0]);
                let known = ann.clone().or_else(|| self.ty_of(&f[3], env, None));
                let known = known.or_else(|| self.infer_let_type(&f[3], &lname, env));
                let ty = match known {
                    Some(t) if t != "void" => t,
                    _ => return Err("let of unknown type".into()),
                };
                let (v, _) = self.expr(&f[3], env, Some(&ty))?;
                let mut v = v.ok_or("let of unit")?;
                let mut is_ref = false;
                let mut owner = false;
                if is_agg(&ty) {
                    let drops = self.cx.needs_drop(&ty);
                    if self.is_ref_expr(&f[3], env) {
                        // 参照は別名のまま (コピーしない)。
                        is_ref = true;
                    } else if self.is_fresh(&f[3]) {
                        owner = drops;
                    } else if drops {
                        // 解放が要る値を場所から取り出す = 移動。元が所有者ならフラグを下ろして、こちらが持つ。
                        if self.root_owner(&f[3], env).is_some() {
                            let dst = self.alloc_for(&ty)?;
                            let size = self.cx.layout(&ty, 0)?.size;
                            self.copy_mem(dst, v, size);
                            self.consume(&f[3], &ty, env)?;
                            v = dst;
                            owner = true;
                        }
                    } else {
                        let dst = self.alloc_for(&ty)?;
                        let size = self.cx.layout(&ty, 0)?.size;
                        self.copy_mem(dst, v, size);
                        v = dst;
                    }
                }
                let var = self.new_var(&ty);
                let lname = st(&nf(&f[0])[0]);
                if !is_agg(&ty) && !is_mutref(&ty) && self.addr_taken.contains(&lname) {
                    // `&mut 名前` を取られるスカラーの局所変数は、スタックのスロットに置く。
                    let slot = self.alloc(8, 8);
                    self.b.ins().store(MemFlags::new(), v, slot, 0);
                    let pvar = self.new_var("usize");
                    self.b.def_var(pvar, slot);
                    self.promoted.insert(pvar.as_u32());
                    env.insert(lname, (pvar, ty));
                    return Ok(());
                }
                self.b.def_var(var, v);
                if is_ref {
                    self.refs.insert(var.as_u32());
                }
                if owner {
                    self.register_owner(var, &ty);
                }
                env.insert(st(&nf(&f[0])[0]), (var, ty));
                Ok(())
            }
            "Assign" => {
                let tg = &f[0];
                if nname(tg) == "Unary" && st(&nf(tg)[0]) == "*" {
                    let (r, rt) = self.expr(&nf(tg)[1], env, None)?;
                    let inner = mutref_inner(&rt).ok_or("assignment through a non-mutable reference")?.to_string();
                    let (v, _) = self.expr(&f[1], env, Some(&inner))?;
                    let (r, v) = (r.ok_or("bad reference")?, v.ok_or("assign of unit")?);
                    self.b.ins().store(MemFlags::new(), v, r, 0);
                    return Ok(());
                }
                if nname(tg) == "Index" {
                    let (bp, bt) = self.expr(&nf(tg)[0], env, None)?;
                    let bp = bp.ok_or("bad index base")?;
                    let elem = self.cx.seq_elem(&bt).ok_or("indexing a non-sequence")?;
                    if self.cx.needs_drop(&elem) {
                        return Err("assignment to a droppable element".into());
                    }
                    let (i, _) = self.expr(&nf(tg)[1], env, Some("usize"))?;
                    let i = i.ok_or("bad index")?;
                    let (v, _) = self.expr(&f[1], env, Some(&elem))?;
                    let v = v.ok_or("assign of unit")?;
                    let len = self.b.ins().load(types::I64, MemFlags::new(), bp, 8);
                    let oob = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, i, len);
                    self.guard(oob, "index out of bounds")?;
                    let pt = self.ptr_ty();
                    let base = self.b.ins().load(pt, MemFlags::new(), bp, 0);
                    let sz = self.elem_size(&elem)?;
                    let off = self.b.ins().imul_imm(i, sz as i64);
                    let addr = self.b.ins().iadd(base, off);
                    return self.store_value(addr, &elem, v);
                }
                let Some(segs) = place_segs(tg) else { return Err("assignment target".into()) };
                if !env.contains_key(&segs[0]) {
                    return Err("assignment target".into());
                }
                if segs.len() == 1 {
                    let (var, ty) = env[&segs[0]].clone();
                    let (v, _) = self.expr(&f[1], env, Some(&ty))?;
                    let v = v.ok_or("assign of unit")?;
                    if is_agg(&ty) {
                        let drops = self.cx.needs_drop(&ty);
                        if self.refs.contains(&var.as_u32()) {
                            return Err("assignment to a reference variable".into());
                        }
                        if drops {
                            let id = var.as_u32();
                            if !self.owners.contains_key(&id) {
                                return Err("assignment to a droppable value this function does not own".into());
                            }
                            self.consume(&f[1], &ty, env)?;
                            self.drop_owner_if_live(id)?;
                        }
                        let dst = self.b.use_var(var);
                        let size = self.cx.layout(&ty, 0)?.size;
                        self.copy_mem(dst, v, size);
                        if drops {
                            let flag = self.owners[&var.as_u32()].flag;
                            let one = self.b.ins().iconst(types::I8, 1);
                            self.b.def_var(flag, one);
                        }
                    } else if self.promoted.contains(&var.as_u32()) {
                        let slot = self.b.use_var(var);
                        self.b.ins().store(MemFlags::new(), v, slot, 0);
                    } else {
                        self.b.def_var(var, v);
                    }
                    return Ok(());
                }
                let (addr, fty) = self.field_addr(&segs, env)?;
                let (v, _) = self.expr(&f[1], env, Some(&fty))?;
                let v = v.ok_or("assign of unit")?;
                if is_agg(&fty) && self.cx.needs_drop(&fty) {
                    self.consume(&f[1], &fty, env)?;
                    self.drop_value(addr, &fty)?;
                }
                self.store_value(addr, &fty, v)
            }
            _ => self.exprstmt(&f[0], env, tail),
        }
    }

    /// 戻り値を返す (構造体・列挙型は sret にコピー)。
    fn emit_return(&mut self, v: Option<Value>, ty: &str) -> R<()> {
        if is_agg(ty) {
            let dst = self.sret.ok_or("aggregate return without sret")?;
            let size = self.cx.layout(ty, 0)?.size;
            self.copy_mem(dst, v.ok_or("return value")?, size);
            self.drop_scopes_from(0)?;
            self.b.ins().return_(&[]);
        } else if let Some(v) = v {
            self.drop_scopes_from(0)?;
            self.b.ins().return_(&[v]);
        } else {
            self.drop_scopes_from(0)?;
            self.b.ins().return_(&[]);
        }
        self.term = true;
        Ok(())
    }

    fn exprstmt(&mut self, e: &V, env: &mut Env, tail: bool) -> R<()> {
        let f = nf(e);
        match nname(e) {
            "Return" => {
                if self.entry {
                    self.drop_scopes_from(0)?;
                    let z = self.b.ins().iconst(types::I32, 0);
                    self.b.ins().return_(&[z]);
                    self.term = true;
                } else if matches!(f[0], V::Nil) {
                    self.drop_scopes_from(0)?;
                    self.b.ins().return_(&[]);
                    self.term = true;
                } else {
                    let rt = self.ret_ty.clone();
                    let (v, ty) = self.expr(&f[0], env, Some(&rt))?;
                    self.consume(&f[0], &ty, env)?;
                    self.emit_return(v, &ty)?;
                }
                Ok(())
            }
            "Break" => {
                let Some(&(_, exit, depth)) = self.loops.last() else { return Err("break outside loop".into()) };
                self.drop_scopes_from(depth)?;
                self.b.ins().jump(exit, &[]);
                self.term = true;
                Ok(())
            }
            "Continue" => {
                let Some(&(head, _, depth)) = self.loops.last() else { return Err("continue outside loop".into()) };
                self.drop_scopes_from(depth)?;
                self.b.ins().jump(head, &[]);
                self.term = true;
                Ok(())
            }
            "If" => self.if_stmt(e, env, tail),
            "Match" => self.match_stmt(e, env, tail),
            "For" => self.for_stmt(e, env),
            "While" => {
                let (head, body, exit) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().jump(head, &[]);
                self.b.switch_to_block(head);
                let (c, _) = self.expr(&f[0], env, Some("bool"))?;
                self.b.ins().brif(c.ok_or("bad condition")?, body, &[], exit, &[]);
                self.b.switch_to_block(body);
                let depth = self.scopes.len();
                self.loops.push((head, exit, depth));
                let mut inner = env.clone();
                self.block(&f[1], &mut inner, false)?;
                self.loops.pop();
                if !self.term {
                    self.b.ins().jump(head, &[]);
                }
                self.b.switch_to_block(exit);
                self.term = false;
                Ok(())
            }
            "Loop" => {
                let (body, exit) = (self.b.create_block(), self.b.create_block());
                self.b.ins().jump(body, &[]);
                self.b.switch_to_block(body);
                let depth = self.scopes.len();
                self.loops.push((body, exit, depth));
                let mut inner = env.clone();
                self.block(&f[0], &mut inner, false)?;
                self.loops.pop();
                if !self.term {
                    self.b.ins().jump(body, &[]);
                }
                self.b.switch_to_block(exit);
                self.term = false;
                Ok(())
            }
            "Block" => {
                let mut inner = env.clone();
                self.block(e, &mut inner, tail)
            }
            _ => {
                if self.entry {
                    if tail {
                        if is_ok_unit(e) || is_print(e) {
                            if is_print(e) {
                                self.print_value(&items(&nf(e)[1])[0], env)?;
                            }
                            return Ok(());
                        }
                        return Err("entry tail expression".into());
                    }
                    if is_print(e) {
                        return self.print_value(&items(&nf(e)[1])[0], env);
                    }
                }
                let expect = if tail && !self.entry { Some(self.ret_ty.clone()) } else { None };
                let (v, ty) = self.expr(e, env, expect.as_deref())?;
                if tail && !self.entry && ty != "void" {
                    self.consume(e, &ty, env)?;
                    self.emit_return(v, &ty)?;
                }
                Ok(())
            }
        }
    }

    /// `for` の反復の元: `core.iter.range(a, b)`、`x.iter()`、`core.iter.enumerate(x.iter())`。
    fn iter_source(&self, it: &V, env: &Env) -> Option<(V, bool, bool)> {
        if nname(it) == "Call" && nname(&nf(it)[0]) == "Path" {
            let segs = strs(&nf(&nf(it)[0])[0]);
            let args = items(&nf(it)[1]);
            if segs == ["core", "iter", "enumerate"] && args.len() == 1 {
                let (r, _, m) = self.iter_source(&args[0], env)?;
                return Some((r, true, m));
            }
            if segs.len() >= 2 && env.contains_key(&segs[0]) && matches!(segs[segs.len() - 1].as_str(), "iter" | "iter_mut") && args.is_empty() {
                let recv = V::Node("Path", vec![V::L(segs[..segs.len() - 1].iter().map(|x| V::S(x.clone())).collect()), V::Nil], 0);
                return Some((recv, false, segs[segs.len() - 1] == "iter_mut"));
            }
        }
        if nname(it) == "MCall" && matches!(st(&nf(it)[1]).as_str(), "iter" | "iter_mut") && items(&nf(it)[2]).is_empty() {
            return Some((nf(it)[0].clone(), false, st(&nf(it)[1]) == "iter_mut"));
        }
        None
    }

    fn for_stmt(&mut self, e: &V, env: &mut Env) -> R<()> {
        let f = nf(e);
        let (pat, it, body) = (&f[0], &f[1], &f[2]);
        let i64t = types::I64;
        let pt = self.ptr_ty();
        let (head, body_b, cont, exit) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
        // 範囲 `core.iter.range(a, b)`
        let range = nname(it) == "Call"
            && nname(&nf(it)[0]) == "Path"
            && strs(&nf(&nf(it)[0])[0]) == ["core", "iter", "range"]
            && items(&nf(it)[1]).len() == 2;
        if range {
            let args = items(&nf(it)[1]);
            let t = self
                .ty_of(&args[0], env, None)
                .or_else(|| self.ty_of(&args[1], env, None))
                .filter(|t| is_int(t))
                .ok_or("range element type")?;
            let (a, _) = self.expr(&args[0], env, Some(&t))?;
            let (b, _) = self.expr(&args[1], env, Some(&t))?;
            let (a, b) = (a.ok_or("bad range")?, b.ok_or("bad range")?);
            self.b.append_block_param(head, cl(&t));
            self.b.ins().jump(head, &[a]);
            self.b.switch_to_block(head);
            let i = self.b.block_params(head)[0];
            let cc = if signed(&t) { IntCC::SignedLessThan } else { IntCC::UnsignedLessThan };
            let c = self.b.ins().icmp(cc, i, b);
            self.b.ins().brif(c, body_b, &[], exit, &[]);
            self.b.switch_to_block(body_b);
            let mut inner = env.clone();
            match nname(pat) {
                "PBind" => {
                    let var = self.new_var(&t);
                    self.b.def_var(var, i);
                    inner.insert(st(&nf(pat)[0]), (var, t.clone()));
                }
                "PWild" => {}
                _ => return Err("for pattern".into()),
            }
            let depth = self.scopes.len();
            self.loops.push((cont, exit, depth));
            self.block(body, &mut inner, false)?;
            self.loops.pop();
            if !self.term {
                self.b.ins().jump(cont, &[]);
            }
            self.b.switch_to_block(cont);
            let n = self.b.ins().iadd_imm(i, 1);
            self.b.ins().jump(head, &[n]);
            self.b.switch_to_block(exit);
            self.term = false;
            return Ok(());
        }
        // 列の要素 `x.iter()`
        let (recv, enumerate, by_mut) = self.iter_source(it, env).ok_or("unsupported iterator in for")?;
        let (sp, sty) = self.expr(&recv, env, None)?;
        let sp = sp.ok_or("bad iterable")?;
        let elem = self.cx.seq_elem(&sty).ok_or("iteration over a non-sequence")?;
        let sz = self.elem_size(&elem)?;
        let base = self.b.ins().load(pt, MemFlags::new(), sp, 0);
        let len = self.b.ins().load(i64t, MemFlags::new(), sp, 8);
        self.b.append_block_param(head, i64t);
        let zero = self.b.ins().iconst(i64t, 0);
        self.b.ins().jump(head, &[zero]);
        self.b.switch_to_block(head);
        let i = self.b.block_params(head)[0];
        let c = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
        self.b.ins().brif(c, body_b, &[], exit, &[]);
        self.b.switch_to_block(body_b);
        let off = self.b.ins().imul_imm(i, sz as i64);
        let addr = self.b.ins().iadd(base, off);
        // `iter_mut()` のスカラー要素は、値ではなく要素のアドレス (`mutref<T>`) を束縛する。
        let mut_scalar = by_mut && !is_agg(&elem);
        let val = if mut_scalar { addr } else { self.load_value(addr, &elem) };
        let bound_ty = if mut_scalar { format!("mutref<{}>", elem) } else { elem.clone() };
        let mut inner = env.clone();
        let (idx_pat, elem_pat) = if enumerate {
            if nname(pat) != "PTuple" || items(&nf(pat)[0]).len() != 2 {
                return Err("enumerate needs a (index, item) pattern".into());
            }
            (Some(&items(&nf(pat)[0])[0]), &items(&nf(pat)[0])[1])
        } else {
            (None, pat)
        };
        if let Some(ip) = idx_pat {
            if nname(ip) == "PBind" {
                let var = self.new_var("usize");
                self.b.def_var(var, i);
                inner.insert(st(&nf(ip)[0]), (var, "usize".to_string()));
            }
        }
        match nname(elem_pat) {
            "PBind" => {
                let var = self.new_var(&bound_ty);
                self.b.def_var(var, val);
                if is_agg(&elem) {
                    self.refs.insert(var.as_u32());
                }
                inner.insert(st(&nf(elem_pat)[0]), (var, bound_ty.clone()));
            }
            "PWild" => {}
            _ => return Err("for pattern".into()),
        }
        let depth = self.scopes.len();
        self.loops.push((cont, exit, depth));
        self.block(body, &mut inner, false)?;
        self.loops.pop();
        if !self.term {
            self.b.ins().jump(cont, &[]);
        }
        self.b.switch_to_block(cont);
        let n = self.b.ins().iadd_imm(i, 1);
        self.b.ins().jump(head, &[n]);
        self.b.switch_to_block(exit);
        self.term = false;
        Ok(())
    }

    fn if_stmt(&mut self, e: &V, env: &mut Env, tail: bool) -> R<()> {
        let f = nf(e);
        let (c, _) = self.expr(&f[0], env, Some("bool"))?;
        let (then_b, else_b, merge) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.ins().brif(c.ok_or("bad condition")?, then_b, &[], else_b, &[]);
        self.b.switch_to_block(then_b);
        let mut inner = env.clone();
        self.block(&f[1], &mut inner, tail)?;
        if !self.term {
            self.b.ins().jump(merge, &[]);
        }
        self.b.switch_to_block(else_b);
        self.term = false;
        if !matches!(f[2], V::Nil) {
            if nname(&f[2]) == "If" {
                self.if_stmt(&f[2], env, tail)?;
            } else {
                let mut inner = env.clone();
                self.block(&f[2], &mut inner, tail)?;
            }
        }
        if !self.term {
            self.b.ins().jump(merge, &[]);
        }
        self.b.switch_to_block(merge);
        self.term = false;
        Ok(())
    }

    // ------------------------------------------------------------ match
    /// 腕のパターンが当てはまるか調べて、当てはまれば `body`、そうでなければ `next` へ分岐する。
    /// 当てはまった側で使う束縛を返す。パターンが必ず当てはまるなら true。
    fn arm_test(
        &mut self,
        pat: &V,
        scrut: Value,
        sty: &str,
        tagv: Option<Value>,
        body: Block,
        next: Block,
    ) -> R<(Vec<(String, Value, String)>, bool)> {
        let pf = nf(pat);
        match nname(pat) {
            "PWild" => {
                self.b.ins().jump(body, &[]);
                Ok((vec![], true))
            }
            "PBind" => {
                self.b.ins().jump(body, &[]);
                if is_agg(sty) {
                    Ok((vec![(st(&pf[0]), scrut, sty.to_string())], true))
                } else {
                    Ok((vec![(st(&pf[0]), scrut, sty.to_string())], true))
                }
            }
            "PLit" => {
                if is_agg(sty) {
                    return Err("literal pattern on an aggregate".into());
                }
                let lit = &pf[0];
                let want = match st(&nf(lit)[0]).as_str() {
                    "int" => int_value(&st(&nf(lit)[1]))?,
                    "bool" => {
                        if matches!(nf(lit)[1], V::B(true)) { 1 } else { 0 }
                    }
                    k => return Err(format!("pattern literal {}", k)),
                };
                let c = self.b.ins().icmp_imm(IntCC::Equal, scrut, want);
                self.b.ins().brif(c, body, &[], next, &[]);
                Ok((vec![], false))
            }
            "PTuple" => {
                if !self.cx.tuples.borrow().contains_key(sty) {
                    return Err("tuple pattern on a non-tuple".into());
                }
                let l = self.cx.layout(sty, 0)?;
                let pats = items(&pf[0]);
                if pats.len() != l.fields.len() {
                    return Err("tuple pattern arity".into());
                }
                let mut binds = vec![];
                for (sp, (_, fty, off)) in pats.iter().zip(l.fields.iter()) {
                    match nname(sp) {
                        "PBind" => {
                            let addr = self.b.ins().iadd_imm(scrut, *off as i64);
                            binds.push((st(&nf(sp)[0]), addr, fty.clone()));
                        }
                        "PWild" => {}
                        _ => return Err("nested pattern".into()),
                    }
                }
                self.b.ins().jump(body, &[]);
                Ok((binds, true))
            }
            "PPath" | "PVariant" => {
                let segs = strs(&pf[0]);
                let l = self.cx.layout(sty, 0)?;
                if !l.is_enum {
                    return Err("variant pattern on a struct".into());
                }
                let vn = segs[segs.len() - 1].clone();
                let idx = l.variants.iter().position(|v| v.0 == vn).ok_or_else(|| format!("no variant {}", vn))?;
                let tg = tagv.ok_or("no tag")?;
                let c = self.b.ins().icmp_imm(IntCC::Equal, tg, idx as i64);
                // 束縛するペイロードのアドレスは、分岐の前に計算しておく (本体のブロックで読む)。
                let mut binds = vec![];
                if nname(pat) == "PVariant" {
                    for (j, sp) in items(&pf[1]).iter().enumerate() {
                        let Some((fty, off)) = l.variants[idx].1.get(j).cloned() else {
                            // `Ok(_)` / `Ok(())` のように、ペイロードが空 (`()`) のバリアントに対するパターン。
                            if matches!(nname(sp), "PWild" | "PBind") || (nname(sp) == "PLit" && st(&nf(&nf(sp)[0])[0]) == "unit") {
                                continue;
                            }
                            return Err("too many sub-patterns".into());
                        };
                        match nname(sp) {
                            "PBind" => {
                                let addr = self.b.ins().iadd_imm(scrut, off as i64);
                                binds.push((st(&nf(sp)[0]), addr, fty));
                            }
                            "PWild" => {}
                            _ => return Err("nested pattern".into()),
                        }
                    }
                }
                self.b.ins().brif(c, body, &[], next, &[]);
                Ok((binds, false))
            }
            n => Err(format!("pattern {}", n)),
        }
    }

    /// 束縛のアドレスから値を読み、新しい変数として env に入れる。
    fn bind_arm(&mut self, binds: Vec<(String, Value, String)>, env: &mut Env, from_variant: bool) {
        for (name, v, ty) in binds {
            let val = if from_variant { self.load_value(v, &ty) } else { v };
            let var = self.new_var(&ty);
            self.b.def_var(var, val);
            env.insert(name, (var, ty));
        }
    }

    /// `match` の共通部分。`value` が Some のとき、各腕の値をそのブロック引数に渡す。
    fn match_common(&mut self, e: &V, env: &mut Env, tail: bool, value: Option<(&str, Block)>) -> R<()> {
        let f = nf(e);
        let (sv, sty) = self.expr(&f[0], env, None)?;
        let sv = sv.ok_or("match on unit")?;
        let tagv = if is_agg(&sty) { Some(self.b.ins().load(types::I32, MemFlags::new(), sv, 0)) } else { None };
        let end = match value {
            Some((_, end)) => end,
            None => self.b.create_block(),
        };
        let arms = items(&f[1]).to_vec();
        let mut covered = false;
        for arm in &arms {
            let a = items(arm);
            let (body, next) = (self.b.create_block(), self.b.create_block());
            let (binds, always) = self.arm_test(&a[0], sv, &sty, tagv, body, next)?;
            self.b.switch_to_block(body);
            self.term = false;
            let mut inner = env.clone();
            // 変数の束縛 (名前 `x` の PBind) はスクルーティニーそのもの、バリアントの束縛はペイロードのアドレス。
            let is_variant_pat = matches!(nname(&a[0]), "PVariant" | "PTuple");
            self.bind_arm(binds, &mut inner, is_variant_pat);
            match value {
                Some((ty, end)) => {
                    let (v, _) = self.arm_value(&a[1], &mut inner, Some(ty))?;
                    if !self.term {
                        match v {
                            Some(v) => self.b.ins().jump(end, &[v]),
                            None => self.b.ins().jump(end, &[]),
                        };
                    }
                }
                None => {
                    if nname(&a[1]) == "Block" {
                        self.block(&a[1], &mut inner, tail)?;
                    } else {
                        self.exprstmt(&a[1], &mut inner, tail)?;
                    }
                    if !self.term {
                        self.b.ins().jump(end, &[]);
                    }
                }
            }
            self.b.switch_to_block(next);
            self.term = false;
            if always {
                covered = true;
                break;
            }
        }
        if !covered {
            // どの腕にも当てはまらない場合は起こらない (網羅性は検査済み)。
            self.b.ins().trap(TrapCode::user(3).unwrap());
        } else {
            // 最後の腕が必ず当てはまるので、next ブロックは到達できない。
            self.b.ins().trap(TrapCode::user(3).unwrap());
        }
        self.b.switch_to_block(end);
        self.term = false;
        Ok(())
    }

    fn match_stmt(&mut self, e: &V, env: &mut Env, tail: bool) -> R<()> {
        self.match_common(e, env, tail, None)
    }

    /// 腕の本体 (式またはブロック) を値として評価する。
    fn arm_value(&mut self, body: &V, env: &mut Env, expect: Option<&str>) -> R<(Option<Value>, String)> {
        if nname(body) == "Block" {
            let stmts = items(&nf(body)[0]).to_vec();
            for (i, s) in stmts.iter().enumerate() {
                if i == stmts.len() - 1 && nname(s) == "ExprStmt" {
                    return self.expr(&nf(s)[0], env, expect);
                }
                self.stmt(s, env, false)?;
            }
            return Ok((None, "void".into()));
        }
        self.expr(body, env, expect)
    }

    fn print_call(&mut self, e: &V) -> R<()> {
        let mut env = Env::new();
        self.print_value(&items(&nf(e)[1])[0], &mut env)
    }

    /// `std.io.print_line(s)`。文字列リテラルは `puts`、`str` の値は `write` で出す。
    fn print_value(&mut self, a: &V, env: &mut Env) -> R<()> {
        if nname(a) == "Lit" && st(&nf(a)[0]) == "str" {
            let mut bytes = lit_bytes(a);
            bytes.push(0);
            let p = self.string_ptr(bytes)?;
            let id = self.rt.puts;
            self.call_rt(id, &[p]);
            return Ok(());
        }
        let (v, ty) = self.expr(a, env, None)?;
        if ty != "str" {
            return Err("print_line of a non-string".into());
        }
        let sv = v.ok_or("bad string")?;
        let pt = self.ptr_ty();
        let ptr = self.b.ins().load(pt, MemFlags::new(), sv, 0);
        let len = self.b.ins().load(types::I64, MemFlags::new(), sv, 8);
        let len32 = self.b.ins().ireduce(types::I32, len);
        let fd = self.b.ins().iconst(types::I32, 1);
        let w = self.rt.write;
        self.call_rt(w, &[fd, ptr, len32]);
        let nl = self.string_ptr(vec![10u8])?;
        let one = self.b.ins().iconst(types::I32, 1);
        let fd = self.b.ins().iconst(types::I32, 1);
        self.call_rt(w, &[fd, nl, one]);
        Ok(())
    }

    // ------------------------------------------------------------ 式
    /// 列挙型のバリアントを作る (ペイロードの式を評価して書き込む)。
    fn make_variant(&mut self, en: &str, idx: usize, args: &[V], env: &mut Env) -> R<(Option<Value>, String)> {
        let l = self.cx.layout(en, 0)?;
        let p = self.alloc(l.size, l.align);
        let tg = self.b.ins().iconst(types::I32, idx as i64);
        self.b.ins().store(MemFlags::new(), tg, p, 0);
        let fields = l.variants[idx].1.clone();
        let args: Vec<&V> = args.iter().filter(|a| !(nname(a) == "Lit" && st(&nf(a)[0]) == "unit")).collect();
        if fields.len() != args.len() {
            return Err("variant arity".into());
        }
        for ((fty, off), a) in fields.iter().zip(args) {
            let (v, _) = self.expr(a, env, Some(fty))?;
            self.consume(a, fty, env)?;
            let addr = self.b.ins().iadd_imm(p, *off as i64);
            self.store_value(addr, fty, v.ok_or("bad payload")?)?;
        }
        Ok((Some(p), en.to_string()))
    }

    /// 関数 `d` を呼ぶ。`recv` があれば第 1 引数 (self)。
    fn call_user(&mut self, fq: &str, d: &Def, recv: Option<Value>, args: &[V], env: &mut Env) -> R<(Option<Value>, String)> {
        if tag(&d.ret) == "ref" {
            return Err("call of a function returning a reference".into());
        }
        let id = *self.ids.get(fq).ok_or("call target not declared")?;
        let rt = self.cx.pty(&d.ret)?;
        let mut vals = vec![];
        let mut out = None;
        if is_agg(&rt) {
            let p = self.alloc_for(&rt)?;
            vals.push(p);
            out = Some(p);
        }
        let mut params = d.params.iter();
        if let Some(r) = recv {
            params.next();
            vals.push(r);
        }
        for ((_, pt), a) in params.zip(args) {
            let want = self.cx.pty(pt)?;
            let (v, got) = self.expr(a, env, Some(&want))?;
            let mut v = v.ok_or("bad argument")?;
            if got != want {
                v = self.coerce(v, &got, &want)?;
            }
            if tag(pt) != "ref" {
                self.consume(a, &want, env)?;
            }
            vals.push(v);
        }
        let res = self.call_rt(id, &vals);
        Ok((out.or(res.first().copied()), rt))
    }

    fn expr(&mut self, e: &V, env: &mut Env, expect: Option<&str>) -> R<(Option<Value>, String)> {
        let f = nf(e);
        match nname(e) {
            "Lit" => match st(&f[0]).as_str() {
                "int" => {
                    let Some(t) = self.ty_of(e, env, expect) else { return Err("untyped integer literal".into()) };
                    let v = int_value(&st(&f[1]))?;
                    let c = self.b.ins().iconst(cl(&t), v);
                    Ok((Some(c), t))
                }
                "bool" => {
                    let c = self.b.ins().iconst(types::I8, if matches!(f[1], V::B(true)) { 1 } else { 0 });
                    Ok((Some(c), "bool".into()))
                }
                "str" => {
                    let bytes = lit_bytes(e);
                    let n = bytes.len() as i64;
                    let ptr = self.string_ptr(bytes)?;
                    let slot = self.alloc(16, 8);
                    self.b.ins().store(MemFlags::new(), ptr, slot, 0);
                    let len = self.b.ins().iconst(types::I64, n);
                    self.b.ins().store(MemFlags::new(), len, slot, 8);
                    Ok((Some(slot), "str".into()))
                }
                "float" => {
                    let Some(t) = self.ty_of(e, env, expect) else { return Err("untyped float literal".into()) };
                    let text = st(&f[2]).replace('_', "");
                    let c = if t == "f32" {
                        self.b.ins().f32const(text.parse::<f32>().map_err(|_| format!("float literal {}", text))?)
                    } else {
                        self.b.ins().f64const(text.parse::<f64>().map_err(|_| format!("float literal {}", text))?)
                    };
                    Ok((Some(c), t))
                }
                k => Err(format!("literal {}", k)),
            },
            "Path" => {
                let segs = strs(&f[0]);
                if env.contains_key(&segs[0]) {
                    let (addr, ty) = if segs.len() == 1 {
                        let (var, ty) = env[&segs[0]].clone();
                        if self.promoted.contains(&var.as_u32()) {
                            let slot = self.b.use_var(var);
                            let val = self.b.ins().load(cl(&ty), MemFlags::new(), slot, 0);
                            return Ok((Some(val), ty));
                        }
                        (self.b.use_var(var), ty)
                    } else {
                        self.field_addr(&segs, env)?
                    };
                    if segs.len() == 1 || is_agg(&ty) {
                        return Ok((Some(if segs.len() == 1 { addr } else { addr }), ty));
                    }
                    let v = self.b.ins().load(cl(&ty), MemFlags::new(), addr, 0);
                    return Ok((Some(v), ty));
                }
                if segs == ["None"] {
                    let Some(t) = expect.filter(|t| t.starts_with("Option<")) else { return Err("None without an expected type".into()) };
                    return self.make_variant(&t.to_string(), 0, &[], env);
                }
                if let Some((en, idx, _)) = self.cx.split_variant(&segs) {
                    return self.make_variant(&en, idx, &[], env);
                }
                Err(format!("path {}", segs.join(".")))
            }
            "Index" => {
                let (bp, bt) = self.expr(&f[0], env, None)?;
                let bp = bp.ok_or("bad index base")?;
                let elem = self.cx.seq_elem(&bt).ok_or("indexing a non-Vec")?;
                let (i, _) = self.expr(&f[1], env, Some("usize"))?;
                let i = i.ok_or("bad index")?;
                let len = self.b.ins().load(types::I64, MemFlags::new(), bp, 8);
                let oob = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, i, len);
                self.guard(oob, "index out of bounds")?;
                let base = self.b.ins().load(types::I64, MemFlags::new(), bp, 0);
                let sz = self.elem_size(&elem)?;
                let off = self.b.ins().imul_imm(i, sz as i64);
                let addr = self.b.ins().iadd(base, off);
                let v = self.load_value(addr, &elem);
                Ok((Some(v), elem))
            }
            "Field" => {
                let segs = place_segs(e).ok_or("field of a non-place")?;
                if !env.contains_key(&segs[0]) {
                    return Err("field of a non-local".into());
                }
                let (addr, ty) = self.field_addr(&segs, env)?;
                if is_agg(&ty) {
                    return Ok((Some(addr), ty));
                }
                let v = self.b.ins().load(cl(&ty), MemFlags::new(), addr, 0);
                Ok((Some(v), ty))
            }
            "Try" => {
                let inner = &f[0];
                let (v, t) = self.expr(inner, env, None)?;
                let p = v.ok_or("? on a unit value")?;
                let is_opt = t.starts_with("Option<");
                if !is_opt && !t.starts_with("Result<") {
                    return Err("? on a value that is not Option or Result".into());
                }
                let ok_idx: i64 = if is_opt { 1 } else { 0 };
                let l = self.cx.layout(&t, 0)?;
                let tg = self.b.ins().load(types::I32, MemFlags::new(), p, 0);
                let is_ok = self.b.ins().icmp_imm(IntCC::Equal, tg, ok_idx);
                let (cont, bad) = (self.b.create_block(), self.b.create_block());
                self.b.ins().brif(is_ok, cont, &[], bad, &[]);
                self.b.switch_to_block(bad);
                if self.entry {
                    // main が Err を返した: 標準エラーに出して終了コード 1。
                    let msg = format!("Error: main returned an error{}", char::from(10u8)).into_bytes();
                    let n = msg.len() as i64;
                    let mp = self.string_ptr(msg)?;
                    let fd = self.b.ins().iconst(types::I32, 2);
                    let len = self.b.ins().iconst(types::I32, n);
                    let w = self.rt.write;
                    self.call_rt(w, &[fd, mp, len]);
                    let code = self.b.ins().iconst(types::I32, 1);
                    let x = self.rt.exit;
                    self.call_rt(x, &[code]);
                    self.b.ins().trap(TrapCode::user(4).unwrap());
                } else {
                    let rt = self.ret_ty.clone();
                    let same_family = (is_opt && rt.starts_with("Option<")) || (!is_opt && rt.starts_with("Result<"));
                    if !same_family {
                        return Err("? in a function whose return type differs".into());
                    }
                    let sret = self.sret.ok_or("? without an aggregate return")?;
                    let rl = self.cx.layout(&rt, 0)?;
                    if is_opt {
                        let t0 = self.b.ins().iconst(types::I32, 0);
                        self.b.ins().store(MemFlags::new(), t0, sret, 0);
                    } else {
                        let src = l.variants[1].1.first().cloned();
                        let dst = rl.variants[1].1.first().cloned();
                        match (src, dst) {
                            (Some((st_, so)), Some((dt_, doff))) => {
                                if st_ != dt_ {
                                    return Err("? with a different error type".into());
                                }
                                let size = if is_scalar(&st_) { scalar_size(&st_) } else { self.cx.layout(&st_, 0)?.size };
                                let sa = self.b.ins().iadd_imm(p, so as i64);
                                let da = self.b.ins().iadd_imm(sret, doff as i64);
                                self.copy_mem(da, sa, size);
                            }
                            (None, None) => {}
                            _ => return Err("? with a different error type".into()),
                        }
                        let t1 = self.b.ins().iconst(types::I32, 1);
                        self.b.ins().store(MemFlags::new(), t1, sret, 0);
                    }
                    self.drop_scopes_from(0)?;
                    self.b.ins().return_(&[]);
                }
                self.b.switch_to_block(cont);
                match l.variants[ok_idx as usize].1.first().cloned() {
                    Some((pty, off)) => {
                        let addr = self.b.ins().iadd_imm(p, off as i64);
                        let val = self.load_value(addr, &pty);
                        Ok((Some(val), pty))
                    }
                    None => Ok((None, "void".into())),
                }
            }
            "TupleE" => {
                let ty = self.ty_of(e, env, expect).ok_or("tuple type")?;
                let l = self.cx.layout(&ty, 0)?;
                let p = self.alloc(l.size, l.align);
                for (x, (_, fty, off)) in items(&f[0]).iter().zip(l.fields.iter()) {
                    let (v, _) = self.expr(x, env, Some(fty))?;
                    self.consume(x, fty, env)?;
                    let addr = self.b.ins().iadd_imm(p, *off as i64);
                    self.store_value(addr, fty, v.ok_or("bad tuple element")?)?;
                }
                Ok((Some(p), ty))
            }
            "StructLit" => {
                let name = st(&f[0]);
                let l = self.cx.layout(&name, 0)?;
                if l.is_enum {
                    return Err("struct literal of an enum".into());
                }
                let p = self.alloc(l.size, l.align);
                for fl in items(&f[1]) {
                    let fi = items(fl);
                    let fname = st(&fi[0]);
                    let (_, fty, off) = l.fields.iter().find(|x| x.0 == fname).cloned().ok_or_else(|| format!("no field {}", fname))?;
                    let (v, _) = self.expr(&fi[1], env, Some(&fty))?;
                    self.consume(&fi[1], &fty, env)?;
                    let addr = self.b.ins().iadd_imm(p, off as i64);
                    self.store_value(addr, &fty, v.ok_or("bad field")?)?;
                }
                Ok((Some(p), name))
            }
            "Bin" => {
                let op = st(&f[0]);
                if op == "&&" || op == "||" {
                    let (a, _) = self.expr(&f[1], env, Some("bool"))?;
                    let a = a.ok_or("bad operand")?;
                    let (rhs, end) = (self.b.create_block(), self.b.create_block());
                    self.b.append_block_param(end, types::I8);
                    if op == "&&" {
                        self.b.ins().brif(a, rhs, &[], end, &[a]);
                    } else {
                        self.b.ins().brif(a, end, &[a], rhs, &[]);
                    }
                    self.b.switch_to_block(rhs);
                    let (b, _) = self.expr(&f[2], env, Some("bool"))?;
                    let b = b.ok_or("bad operand")?;
                    self.b.ins().jump(end, &[b]);
                    self.b.switch_to_block(end);
                    let r = self.b.block_params(end)[0];
                    return Ok((Some(r), "bool".into()));
                }
                let cmp = matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=");
                let t = self
                    .ty_of(&f[1], env, if cmp { None } else { expect })
                    .or_else(|| self.ty_of(&f[2], env, None))
                    .or_else(|| expect.map(String::from).filter(|t| is_scalar(t)))
                    .ok_or("operand type")?;
                if is_agg(&t) && matches!(op.as_str(), "==" | "!=") {
                    let (a, _) = self.expr(&f[1], env, Some(&t))?;
                    let (b, _) = self.expr(&f[2], env, Some(&t))?;
                    let eq = self.value_eq(a.ok_or("bad operand")?, b.ok_or("bad operand")?, &t)?;
                    let r = if op == "==" { eq } else { self.b.ins().bxor_imm(eq, 1) };
                    return Ok((Some(r), "bool".into()));
                }
                if !is_scalar(&t) {
                    return Err("operator on an aggregate".into());
                }
                let (a, _) = self.expr(&f[1], env, Some(&t))?;
                let (b, _) = self.expr(&f[2], env, Some(&t))?;
                let (a, b) = (a.ok_or("bad operand")?, b.ok_or("bad operand")?);
                if cmp && is_float(&t) {
                    let cc = match op.as_str() {
                        "==" => FloatCC::Equal,
                        "!=" => FloatCC::NotEqual,
                        "<" => FloatCC::LessThan,
                        "<=" => FloatCC::LessThanOrEqual,
                        ">" => FloatCC::GreaterThan,
                        _ => FloatCC::GreaterThanOrEqual,
                    };
                    let r = self.b.ins().fcmp(cc, a, b);
                    return Ok((Some(r), "bool".into()));
                }
                if cmp {
                    let sg = signed(&t);
                    let cc = match (op.as_str(), sg) {
                        ("==", _) => IntCC::Equal,
                        ("!=", _) => IntCC::NotEqual,
                        ("<", true) => IntCC::SignedLessThan,
                        ("<=", true) => IntCC::SignedLessThanOrEqual,
                        (">", true) => IntCC::SignedGreaterThan,
                        (">=", true) => IntCC::SignedGreaterThanOrEqual,
                        ("<", false) => IntCC::UnsignedLessThan,
                        ("<=", false) => IntCC::UnsignedLessThanOrEqual,
                        (">", false) => IntCC::UnsignedGreaterThan,
                        _ => IntCC::UnsignedGreaterThanOrEqual,
                    };
                    let r = self.b.ins().icmp(cc, a, b);
                    return Ok((Some(r), "bool".into()));
                }
                if is_float(&t) {
                    let r = match op.as_str() {
                        "+" => self.b.ins().fadd(a, b),
                        "-" => self.b.ins().fsub(a, b),
                        "*" => self.b.ins().fmul(a, b),
                        "/" => self.b.ins().fdiv(a, b),
                        _ => return Err(format!("operator {}", op)),
                    };
                    return Ok((Some(r), t));
                }
                if t == "bool" && !matches!(op.as_str(), "&" | "|" | "^") {
                    return Err(format!("operator {} on bool", op));
                }
                let sg = signed(&t);
                let r = match op.as_str() {
                    "&" => self.b.ins().band(a, b),
                    "|" => self.b.ins().bor(a, b),
                    "^" => self.b.ins().bxor(a, b),
                    "+" | "-" | "*" => {
                        let (r, of) = match (op.as_str(), sg) {
                            ("+", true) => self.b.ins().sadd_overflow(a, b),
                            ("+", false) => self.b.ins().uadd_overflow(a, b),
                            ("-", true) => self.b.ins().ssub_overflow(a, b),
                            ("-", false) => self.b.ins().usub_overflow(a, b),
                            (_, true) => self.b.ins().smul_overflow(a, b),
                            (_, false) => self.b.ins().umul_overflow(a, b),
                        };
                        self.guard(of, "arithmetic overflow")?;
                        r
                    }
                    "/" | "%" => {
                        let zero = self.b.ins().icmp_imm(IntCC::Equal, b, 0);
                        self.guard(zero, "division by zero")?;
                        if sg {
                            let bits = cl(&t).bits();
                            let min: i64 = if bits >= 64 { i64::MIN } else { -(1i64 << (bits - 1)) };
                            let is_min = self.b.ins().icmp_imm(IntCC::Equal, a, min);
                            let is_m1 = self.b.ins().icmp_imm(IntCC::Equal, b, -1);
                            let both = self.b.ins().band(is_min, is_m1);
                            self.guard(both, "arithmetic overflow")?;
                        }
                        match (op.as_str(), sg) {
                            ("/", true) => self.b.ins().sdiv(a, b),
                            ("/", false) => self.b.ins().udiv(a, b),
                            (_, true) => self.b.ins().srem(a, b),
                            (_, false) => self.b.ins().urem(a, b),
                        }
                    }
                    _ => return Err(format!("operator {}", op)),
                };
                Ok((Some(r), t))
            }
            "Unary" => {
                let op = st(&f[0]);
                match op.as_str() {
                    "&mut" if self.ty_of(&f[1], env, None).map_or(false, |t| !is_agg(&t)) => {
                        // `&mut スカラー`: 変数のスロット・フィールド・要素のアドレス。
                        let operand = &f[1];
                        match nname(operand) {
                            "Path" | "Field" => {
                                let segs = place_segs(operand).ok_or("mutable reference to a non-place")?;
                                if !env.contains_key(&segs[0]) {
                                    return Err("mutable reference to a non-local".into());
                                }
                                if segs.len() == 1 {
                                    let (var, ty) = env[&segs[0]].clone();
                                    if !self.promoted.contains(&var.as_u32()) {
                                        return Err("mutable reference to a local that is not addressable".into());
                                    }
                                    let slot = self.b.use_var(var);
                                    return Ok((Some(slot), format!("mutref<{}>", ty)));
                                }
                                let (addr, fty) = self.field_addr(&segs, env)?;
                                Ok((Some(addr), format!("mutref<{}>", fty)))
                            }
                            "Index" => {
                                let (bp, bt) = self.expr(&nf(operand)[0], env, None)?;
                                let bp = bp.ok_or("bad index base")?;
                                let elem = self.cx.seq_elem(&bt).ok_or("indexing a non-sequence")?;
                                let (i, _) = self.expr(&nf(operand)[1], env, Some("usize"))?;
                                let i = i.ok_or("bad index")?;
                                let len = self.b.ins().load(types::I64, MemFlags::new(), bp, 8);
                                let oob = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, i, len);
                                self.guard(oob, "index out of bounds")?;
                                let pt = self.ptr_ty();
                                let base = self.b.ins().load(pt, MemFlags::new(), bp, 0);
                                let sz = self.elem_size(&elem)?;
                                let off = self.b.ins().imul_imm(i, sz as i64);
                                let addr = self.b.ins().iadd(base, off);
                                Ok((Some(addr), format!("mutref<{}>", elem)))
                            }
                            _ => Err("mutable reference to a temporary".into()),
                        }
                    }
                    "*" => {
                        let (v, t) = self.expr(&f[1], env, expect)?;
                        if let Some(inner) = mutref_inner(&t) {
                            let inner = inner.to_string();
                            let r = v.ok_or("bad reference")?;
                            let val = self.b.ins().load(cl(&inner), MemFlags::new(), r, 0);
                            return Ok((Some(val), inner));
                        }
                        Ok((v, t))
                    }
                    "&" | "&mut" => {
                        let (v, t) = self.expr(&f[1], env, expect)?;
                        // 不変の参照 `&scalar` は値そのもの。
                        if !is_agg(&t) && op == "&mut" {
                            return Err("mutable reference to a scalar".into());
                        }
                        Ok((v, t))
                    }
                    "!" => {
                        let t = self.ty_of(&f[1], env, expect).ok_or("operand type")?;
                        let (a, _) = self.expr(&f[1], env, Some(&t))?;
                        let a = a.ok_or("bad operand")?;
                        let r = if t == "bool" { self.b.ins().bxor_imm(a, 1) } else { self.b.ins().bnot(a) };
                        Ok((Some(r), t))
                    }
                    "-" => {
                        let t = self.ty_of(&f[1], env, expect).ok_or("operand type")?;
                        let (a, _) = self.expr(&f[1], env, Some(&t))?;
                        let a = a.ok_or("bad operand")?;
                        if is_float(&t) {
                            let r = self.b.ins().fneg(a);
                            return Ok((Some(r), t));
                        }
                        if !signed(&t) || t == "bool" {
                            return Err("negation of an unsigned value".into());
                        }
                        let z = self.b.ins().iconst(cl(&t), 0);
                        let (r, of) = self.b.ins().ssub_overflow(z, a);
                        self.guard(of, "arithmetic overflow")?;
                        Ok((Some(r), t))
                    }
                    o => Err(format!("unary {}", o)),
                }
            }
            "Match" => {
                let ty = self
                    .ty_of(e, env, expect)
                    .or_else(|| expect.map(String::from))
                    .ok_or("match value type")?;
                if is_agg(&ty) {
                    return Err("match producing an aggregate".into());
                }
                let end = self.b.create_block();
                if ty != "void" {
                    self.b.append_block_param(end, cl(&ty));
                }
                self.match_common(e, env, false, Some((&ty, end)))?;
                if ty == "void" {
                    Ok((None, ty))
                } else {
                    let r = self.b.block_params(end)[0];
                    Ok((Some(r), ty))
                }
            }
            "MCall" => {
                let name = st(&f[1]);
                self.method_call(&f[0], &name, items(&f[2]), env, expect)
            }
            "Call" => {
                if nname(&f[0]) != "Path" {
                    return Err("call of non-path".into());
                }
                let segs = strs(&nf(&f[0])[0]);
                let args = items(&f[1]);
                if segs.len() >= 2 && env.contains_key(&segs[0]) {
                    let recv = V::Node("Path", vec![V::L(segs[..segs.len() - 1].iter().map(|x| V::S(x.clone())).collect()), V::Nil], 0);
                    return self.method_call(&recv, &segs[segs.len() - 1], args, env, expect);
                }
                if let Some((en, idx, _)) = self.cx.split_variant(&segs) {
                    return self.make_variant(&en, idx, args, env);
                }
                if segs.len() >= 3 && self.cx.split_variant(&segs[..segs.len() - 1]).is_some() {
                    // `E.V.method(..)`: 単位バリアントに対するメソッド呼び出し
                    let recv = V::Node("Path", vec![V::L(segs[..segs.len() - 1].iter().map(|x| V::S(x.clone())).collect()), V::Nil], 0);
                    return self.method_call(&recv, &segs[segs.len() - 1], args, env, expect);
                }
                if segs.len() == 1 && matches!(segs[0].as_str(), "Some" | "Ok" | "Err") && !self.funcs.contains_key(&segs[0]) {
                    let Some(t) = self.ctor_type(&segs[0], args, env, expect) else { return Err("constructor without an expected type".into()) };
                    let idx = match segs[0].as_str() {
                        "Some" => 1,
                        "Ok" => 0,
                        _ => 1,
                    };
                    return self.make_variant(&t, idx, args, env);
                }
                if segs == ["core", "check", "eq"] {
                    let t = self.ty_of(&args[0], env, None).or_else(|| self.ty_of(&args[1], env, None)).ok_or("operand type")?;
                    if t == "str" {
                        let (a, _) = self.expr(&args[0], env, Some("str"))?;
                        let (b, _) = self.expr(&args[1], env, Some("str"))?;
                        let eq = self.str_eq(a.ok_or("bad operand")?, b.ok_or("bad operand")?);
                        let ne = self.b.ins().bxor_imm(eq, 1);
                        self.guard(ne, "assertion failed")?;
                        return Ok((None, "void".into()));
                    }
                    if is_agg(&t) {
                        let (a, _) = self.expr(&args[0], env, Some(&t))?;
                        let (b, _) = self.expr(&args[1], env, Some(&t))?;
                        let eq = self.value_eq(a.ok_or("bad operand")?, b.ok_or("bad operand")?, &t)?;
                        let ne = self.b.ins().bxor_imm(eq, 1);
                        self.guard(ne, "assertion failed")?;
                        return Ok((None, "void".into()));
                    }
                    if !is_scalar(&t) {
                        return Err("check.eq on an aggregate".into());
                    }
                    let (a, _) = self.expr(&args[0], env, Some(&t))?;
                    let (b, _) = self.expr(&args[1], env, Some(&t))?;
                    let (a, b) = (a.ok_or("bad operand")?, b.ok_or("bad operand")?);
                    let c = if is_float(&t) { self.b.ins().fcmp(FloatCC::Equal, a, b) } else { self.b.ins().icmp(IntCC::Equal, a, b) };
                    let ne = self.b.ins().bxor_imm(c, 1);
                    self.guard(ne, "assertion failed")?;
                    return Ok((None, "void".into()));
                }
                if segs == ["core", "mem", "drop"] {
                    let (_, t) = self.expr(&args[0], env, None)?;
                    if is_agg(&t) && self.cx.needs_drop(&t) {
                        if let Some(id) = self.root_owner(&args[0], env) {
                            // 明示的な drop: 持ち主なら今すぐ解放する (フラグが下りる)。
                            self.drop_owner_if_live(id)?;
                        }
                    }
                    return Ok((None, "void".into()));
                }
                if segs == ["std", "io", "print_line"] {
                    self.print_value(&args[0], env)?;
                    return Ok((None, "void".into()));
                }
                if segs == ["alloc", "string", "String", "new"] {
                    let v = self.alloc(24, 8);
                    let z = self.b.ins().iconst(types::I64, 0);
                    for off in [0, 8, 16] {
                        self.b.ins().store(MemFlags::new(), z, v, off);
                    }
                    return Ok((Some(v), STRING.to_string()));
                }
                if segs == ["alloc", "string", "String", "from_str"] {
                    if args.len() != 2 {
                        return Err("from_str arity".into());
                    }
                    let (sv, t) = self.expr(&args[1], env, Some("str"))?;
                    if t != "str" {
                        return Err("from_str of a non-str".into());
                    }
                    let sv = sv.ok_or("bad argument")?;
                    let pt = self.ptr_ty();
                    let src = self.b.ins().load(pt, MemFlags::new(), sv, 0);
                    let n = self.b.ins().load(types::I64, MemFlags::new(), sv, 8);
                    let tmp = self.alloc(24, 8);
                    let z = self.b.ins().iconst(types::I64, 0);
                    for off in [0, 8, 16] {
                        self.b.ins().store(MemFlags::new(), z, tmp, off);
                    }
                    let failed = self.string_append(tmp, src, n)?;
                    let rty = self.cx.builtin_enum("Result", &[STRING.to_string(), "alloc.alloc.AllocError".to_string()])?;
                    let l = self.cx.layout(&rty, 0)?;
                    let res = self.alloc(l.size, l.align);
                    let tg = self.b.ins().uextend(types::I32, failed);
                    self.b.ins().store(MemFlags::new(), tg, res, 0);
                    let (ok_b, done) = (self.b.create_block(), self.b.create_block());
                    self.b.ins().brif(failed, done, &[], ok_b, &[]);
                    self.b.switch_to_block(ok_b);
                    let poff = l.variants[0].1[0].1;
                    let dst = self.b.ins().iadd_imm(res, poff as i64);
                    self.copy_mem(dst, tmp, 24);
                    self.b.ins().jump(done, &[]);
                    self.b.switch_to_block(done);
                    return Ok((Some(res), rty));
                }
                if segs == ["alloc", "vec", "Vec", "new"] {
                    let Some(t) = expect.filter(|t| self.cx.elem_of(t).is_some()) else { return Err("Vec.new without an element type".into()) };
                    let v = self.alloc(24, 8);
                    let z = self.b.ins().iconst(types::I64, 0);
                    for off in [0, 8, 16] {
                        self.b.ins().store(MemFlags::new(), z, v, off);
                    }
                    return Ok((Some(v), t.to_string()));
                }
                let fq = segs.join(".");
                let Some(d) = self.funcs.get(&fq).copied() else { return Err(format!("call to {}", fq)) };
                self.call_user(&fq, d, None, args, env)
            }
            n => Err(format!("expression {}", n)),
        }
    }

    fn method_call(&mut self, recv: &V, name: &str, args: &[V], env: &mut Env, expect: Option<&str>) -> R<(Option<Value>, String)> {
        let rty = self.ty_of(recv, env, expect);
        if rty.as_deref().map_or(false, is_mutref) {
            // `r.method()` (r: &mut スカラー) は `(*r).method()`
            let deref = V::Node("Unary", vec![V::S("*".into()), recv.clone()], 0);
            return self.method_call(&deref, name, args, env, expect);
        }
        if let Some(rt) = &rty {
            if rt.starts_with("Option<") || rt.starts_with("Result<") {
                if self.cx.builtin_method_ty(rt, name).is_some() {
                    return self.option_method(recv, rt, name, args, env);
                }
            }
            if rt == "str" || rt == STRING || self.cx.seq_elem(rt).is_some() {
                let (p, _) = self.expr(recv, env, None)?;
                let p = p.ok_or("bad receiver")?;
                return self.builtin_method(p, rt, name, args, env);
            }
            if is_agg(rt) {
                let fq = format!("{}.{}", rt, name);
                let d = self.funcs.get(&fq).copied().ok_or_else(|| format!("method {}", fq))?;
                if !d.is_method() {
                    return Err(format!("{} is not a method", fq));
                }
                let (p, rty2) = self.expr(recv, env, None)?;
                if tag(&d.params[0].1) != "ref" {
                    self.consume(recv, &rty2, env)?;
                }
                return self.call_user(&fq, d, Some(p.ok_or("bad receiver")?), args, env);
            }
        }
        self.method(recv, name, args, env, expect)
    }

    /// スライス・Vec に共通のメソッド。`p` は先頭アドレス (先頭の 2 語が ptr, len)。扱わないなら None。
    fn seq_method(
        &mut self,
        p: Value,
        elem: &str,
        sz: u32,
        is_slice: bool,
        name: &str,
        args: &[V],
        env: &mut Env,
    ) -> R<Option<(Option<Value>, String)>> {
        let i64t = types::I64;
        let pt = self.ptr_ty();
        let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
        match name {
            "len" if is_slice => Ok(Some((Some(len), "usize".into()))),
            "is_empty" if is_slice => {
                let r = self.b.ins().icmp_imm(IntCC::Equal, len, 0);
                Ok(Some((Some(r), "bool".into())))
            }
            "as_slice" | "as_mut_slice" => {
                let slot = self.alloc(16, 8);
                let ptr = self.b.ins().load(pt, MemFlags::new(), p, 0);
                self.b.ins().store(MemFlags::new(), ptr, slot, 0);
                self.b.ins().store(MemFlags::new(), len, slot, 8);
                Ok(Some((Some(slot), self.cx.slice_of(elem))))
            }
            "get" | "first" | "last" | "get_mut" => {
                if !is_scalar(elem) {
                    return Err(format!("{} on aggregate elements", name));
                }
                let by_addr = name == "get_mut";
                let pay_ty = if by_addr { format!("mutref<{}>", elem) } else { elem.to_string() };
                let rty = self.cx.builtin_enum("Option", &[pay_ty])?;
                let l = self.cx.layout(&rty, 0)?;
                let res = self.alloc(l.size, l.align);
                let idx = match name {
                    "get" | "get_mut" => {
                        let (i, _) = self.expr(args.first().ok_or("get needs an index")?, env, Some("usize"))?;
                        i.ok_or("bad index")?
                    }
                    "first" => self.b.ins().iconst(i64t, 0),
                    _ => self.b.ins().iadd_imm(len, -1),
                };
                // last の空列では len - 1 が巻き戻るが、範囲検査 (idx < len) で弾かれる。
                let inb = self.b.ins().icmp(IntCC::UnsignedLessThan, idx, len);
                let (yes, no, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().brif(inb, yes, &[], no, &[]);
                self.b.switch_to_block(no);
                let t0 = self.b.ins().iconst(types::I32, 0);
                self.b.ins().store(MemFlags::new(), t0, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(yes);
                let base = self.b.ins().load(pt, MemFlags::new(), p, 0);
                let off = self.b.ins().imul_imm(idx, sz as i64);
                let addr = self.b.ins().iadd(base, off);
                let val = if by_addr { addr } else { self.b.ins().load(cl(elem), MemFlags::new(), addr, 0) };
                let poff = l.variants[1].1[0].1;
                let dst = self.b.ins().iadd_imm(res, poff as i64);
                self.b.ins().store(MemFlags::new(), val, dst, 0);
                let t1 = self.b.ins().iconst(types::I32, 1);
                self.b.ins().store(MemFlags::new(), t1, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
                Ok(Some((Some(res), rty)))
            }
            "sub" => {
                if args.len() != 2 {
                    return Err("sub needs two arguments".into());
                }
                let sl = self.cx.slice_of(elem);
                let rty = self.cx.builtin_enum("Option", &[sl.clone()])?;
                let l = self.cx.layout(&rty, 0)?;
                let res = self.alloc(l.size, l.align);
                let (from, _) = self.expr(&args[0], env, Some("usize"))?;
                let (to, _) = self.expr(&args[1], env, Some("usize"))?;
                let (from, to) = (from.ok_or("bad index")?, to.ok_or("bad index")?);
                let ok1 = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, from, to);
                let ok2 = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, to, len);
                let ok = self.b.ins().band(ok1, ok2);
                let (yes, no, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().brif(ok, yes, &[], no, &[]);
                self.b.switch_to_block(no);
                let t0 = self.b.ins().iconst(types::I32, 0);
                self.b.ins().store(MemFlags::new(), t0, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(yes);
                let base = self.b.ins().load(pt, MemFlags::new(), p, 0);
                let off = self.b.ins().imul_imm(from, sz as i64);
                let np = self.b.ins().iadd(base, off);
                let nl = self.b.ins().isub(to, from);
                let poff = l.variants[1].1[0].1 as i64;
                let dst = self.b.ins().iadd_imm(res, poff);
                self.b.ins().store(MemFlags::new(), np, dst, 0);
                self.b.ins().store(MemFlags::new(), nl, dst, 8);
                let t1 = self.b.ins().iconst(types::I32, 1);
                self.b.ins().store(MemFlags::new(), t1, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
                Ok(Some((Some(res), rty)))
            }
            _ => Ok(None),
        }
    }

    /// 文字列 `p` (`String`) の末尾に、`src` から `n` バイトを足す。失敗したら真 (I8) を返す。
    fn string_append(&mut self, p: Value, src: Value, n: Value) -> R<Value> {
        let i64t = types::I64;
        let pt = self.ptr_ty();
        let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
        let cap = self.b.ins().load(i64t, MemFlags::new(), p, 16);
        let ptr0 = self.b.ins().load(pt, MemFlags::new(), p, 0);
        let need = self.b.ins().iadd(len, n);
        let fits = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, need, cap);
        let (grow, copy_b, fail_b, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.append_block_param(copy_b, pt);
        self.b.append_block_param(done, types::I8);
        self.b.ins().brif(fits, copy_b, &[ptr0], grow, &[]);
        self.b.switch_to_block(grow);
        let dbl = self.b.ins().ishl_imm(cap, 1);
        let bigger = self.b.ins().icmp(IntCC::UnsignedGreaterThan, need, dbl);
        let newcap = self.b.ins().select(bigger, need, dbl);
        let ra = self.rt.realloc;
        let np = self.call_rt(ra, &[ptr0, newcap])[0];
        let isnull = self.b.ins().icmp_imm(IntCC::Equal, np, 0);
        let set_b = self.b.create_block();
        self.b.ins().brif(isnull, fail_b, &[], set_b, &[]);
        self.b.switch_to_block(set_b);
        self.b.ins().store(MemFlags::new(), np, p, 0);
        self.b.ins().store(MemFlags::new(), newcap, p, 16);
        self.b.ins().jump(copy_b, &[np]);
        self.b.switch_to_block(copy_b);
        let base = self.b.block_params(copy_b)[0];
        let dst = self.b.ins().iadd(base, len);
        let mc = self.rt.memcpy;
        self.call_rt(mc, &[dst, src, n]);
        self.b.ins().store(MemFlags::new(), need, p, 8);
        let zero = self.b.ins().iconst(types::I8, 0);
        self.b.ins().jump(done, &[zero]);
        self.b.switch_to_block(fail_b);
        let one = self.b.ins().iconst(types::I8, 1);
        self.b.ins().jump(done, &[one]);
        self.b.switch_to_block(done);
        Ok(self.b.block_params(done)[0])
    }

    /// `Result<void, AllocError>` の値を、失敗フラグから作る。
    fn alloc_result(&mut self, failed: Value) -> R<(Value, String)> {
        let rty = self.cx.builtin_enum("Result", &["void".into(), "alloc.alloc.AllocError".into()])?;
        let l = self.cx.layout(&rty, 0)?;
        let res = self.alloc(l.size, l.align);
        let tg = self.b.ins().uextend(types::I32, failed);
        self.b.ins().store(MemFlags::new(), tg, res, 0);
        Ok((res, rty))
    }

    fn string_method(&mut self, p: Value, name: &str, args: &[V], env: &mut Env) -> R<(Option<Value>, String)> {
        let i64t = types::I64;
        let pt = self.ptr_ty();
        match name {
            "len" => {
                let v = self.b.ins().load(i64t, MemFlags::new(), p, 8);
                Ok((Some(v), "usize".into()))
            }
            "is_empty" => {
                let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
                let r = self.b.ins().icmp_imm(IntCC::Equal, len, 0);
                Ok((Some(r), "bool".into()))
            }
            "clear" => {
                let z = self.b.ins().iconst(i64t, 0);
                self.b.ins().store(MemFlags::new(), z, p, 8);
                Ok((None, "void".into()))
            }
            "as_str" => {
                let slot = self.alloc(16, 8);
                let ptr = self.b.ins().load(pt, MemFlags::new(), p, 0);
                let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
                self.b.ins().store(MemFlags::new(), ptr, slot, 0);
                self.b.ins().store(MemFlags::new(), len, slot, 8);
                Ok((Some(slot), "str".into()))
            }
            "push_str" => {
                let arg = args.first().ok_or("push_str needs an argument")?;
                let (sv, t) = self.expr(arg, env, Some("str"))?;
                if t != "str" {
                    return Err("push_str of a non-str".into());
                }
                let sv = sv.ok_or("bad argument")?;
                let src = self.b.ins().load(pt, MemFlags::new(), sv, 0);
                let n = self.b.ins().load(i64t, MemFlags::new(), sv, 8);
                let failed = self.string_append(p, src, n)?;
                let (res, rty) = self.alloc_result(failed)?;
                Ok((Some(res), rty))
            }
            _ => Err(format!("String method {}", name)),
        }
    }

    /// 型 `ty` の 2 つの値 (スカラーは値、それ以外はアドレス) が等しければ真 (I8)。構造体・タプル・列挙型は構造的に比べる。
    fn value_eq(&mut self, a: Value, b: Value, ty: &str) -> R<Value> {
        if is_scalar(ty) {
            return Ok(if is_float(ty) { self.b.ins().fcmp(FloatCC::Equal, a, b) } else { self.b.ins().icmp(IntCC::Equal, a, b) });
        }
        if ty == "str" || ty == STRING {
            // どちらも先頭の 2 語が {ptr, len}
            return Ok(self.str_eq(a, b));
        }
        if self.cx.slices.borrow().contains_key(ty) || self.cx.elem_of(ty).is_some() {
            return Err("equality of sequences".into());
        }
        let l = self.cx.layout(ty, 0)?;
        if !l.is_enum {
            let mut acc = self.b.ins().iconst(types::I8, 1);
            for (_, fty, off) in &l.fields {
                let (x, y) = self.load_pair(a, b, fty, *off)?;
                let e = self.value_eq(x, y, fty)?;
                acc = self.b.ins().band(acc, e);
            }
            return Ok(acc);
        }
        // 列挙型: タグが同じで、そのバリアントのペイロードが等しい。
        let ta = self.b.ins().load(types::I32, MemFlags::new(), a, 0);
        let tb = self.b.ins().load(types::I32, MemFlags::new(), b, 0);
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I8);
        let first = self.b.create_block();
        let same = self.b.ins().icmp(IntCC::Equal, ta, tb);
        let zero = self.b.ins().iconst(types::I8, 0);
        self.b.ins().brif(same, first, &[], done, &[zero]);
        self.b.switch_to_block(first);
        for (idx, (_, fields)) in l.variants.iter().enumerate() {
            let is_it = self.b.ins().icmp_imm(IntCC::Equal, ta, idx as i64);
            let (yes, next) = (self.b.create_block(), self.b.create_block());
            self.b.ins().brif(is_it, yes, &[], next, &[]);
            self.b.switch_to_block(yes);
            let mut acc = self.b.ins().iconst(types::I8, 1);
            for (fty, off) in fields {
                let (x, y) = self.load_pair(a, b, fty, *off)?;
                let e = self.value_eq(x, y, fty)?;
                acc = self.b.ins().band(acc, e);
            }
            self.b.ins().jump(done, &[acc]);
            self.b.switch_to_block(next);
        }
        let one = self.b.ins().iconst(types::I8, 1);
        self.b.ins().jump(done, &[one]);
        self.b.switch_to_block(done);
        Ok(self.b.block_params(done)[0])
    }

    /// 2 つの値の、オフセット `off` にあるフィールドを取り出す (スカラーは値、それ以外はアドレス)。
    fn load_pair(&mut self, a: Value, b: Value, fty: &str, off: u32) -> R<(Value, Value)> {
        if is_scalar(fty) {
            let x = self.b.ins().load(cl(fty), MemFlags::new(), a, off as i32);
            let y = self.b.ins().load(cl(fty), MemFlags::new(), b, off as i32);
            Ok((x, y))
        } else {
            let x = self.b.ins().iadd_imm(a, off as i64);
            let y = self.b.ins().iadd_imm(b, off as i64);
            Ok((x, y))
        }
    }

    /// `Option` / `Result` のメソッド: `is_some` / `is_none` / `is_ok` / `is_err` / `unwrap` / `expect` / `unwrap_or`。
    fn option_method(&mut self, recv: &V, rt: &str, name: &str, args: &[V], env: &mut Env) -> R<(Option<Value>, String)> {
        let (p, _) = self.expr(recv, env, None)?;
        let p = p.ok_or("bad receiver")?;
        let ok_idx: i64 = if rt.starts_with("Option<") { 1 } else { 0 };
        let l = self.cx.layout(rt, 0)?;
        let tg = self.b.ins().load(types::I32, MemFlags::new(), p, 0);
        match name {
            "is_some" | "is_ok" => {
                let r = self.b.ins().icmp_imm(IntCC::Equal, tg, ok_idx);
                return Ok((Some(r), "bool".into()));
            }
            "is_none" | "is_err" => {
                let r = self.b.ins().icmp_imm(IntCC::NotEqual, tg, ok_idx);
                return Ok((Some(r), "bool".into()));
            }
            _ => {}
        }
        // unwrap / expect / unwrap_or: 値を取り出す (所有を移す)。
        self.consume(recv, rt, env)?;
        let payload = l.variants[ok_idx as usize].1.first().cloned();
        let default = if name == "unwrap_or" {
            let a = args.first().ok_or("unwrap_or needs a default")?;
            match &payload {
                Some((pty, _)) => {
                    let (d, _) = self.expr(a, env, Some(pty))?;
                    self.consume(a, pty, env)?;
                    d
                }
                None => None,
            }
        } else {
            None
        };
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, tg, ok_idx);
        let (yes, no, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        let ty = payload.as_ref().map(|x| x.0.clone()).unwrap_or_else(|| "void".to_string());
        if ty != "void" {
            let bt = if is_agg(&ty) { self.ptr_ty() } else { cl(&ty) };
            self.b.append_block_param(done, bt);
        }
        self.b.ins().brif(is_ok, yes, &[], no, &[]);
        self.b.switch_to_block(yes);
        if let Some((pty, off)) = &payload {
            let addr = self.b.ins().iadd_imm(p, *off as i64);
            let v = self.load_value(addr, pty);
            self.b.ins().jump(done, &[v]);
        } else {
            self.b.ins().jump(done, &[]);
        }
        self.b.switch_to_block(no);
        if name == "unwrap_or" {
            match default {
                Some(d) => self.b.ins().jump(done, &[d]),
                None => self.b.ins().jump(done, &[]),
            };
        } else {
            self.fail_block("called unwrap on a None or Err value")?;
        }
        self.b.switch_to_block(done);
        if ty == "void" {
            Ok((None, ty))
        } else {
            Ok((Some(self.b.block_params(done)[0]), ty))
        }
    }

    /// 2 つの `str` (アドレス) が等しければ真 (I8)。
    fn str_eq(&mut self, a: Value, b: Value) -> Value {
        let i64t = types::I64;
        let pt = self.ptr_ty();
        let la = self.b.ins().load(i64t, MemFlags::new(), a, 8);
        let lb = self.b.ins().load(i64t, MemFlags::new(), b, 8);
        let pa = self.b.ins().load(pt, MemFlags::new(), a, 0);
        let pb = self.b.ins().load(pt, MemFlags::new(), b, 0);
        let (cmp_b, done) = (self.b.create_block(), self.b.create_block());
        self.b.append_block_param(done, types::I8);
        let same_len = self.b.ins().icmp(IntCC::Equal, la, lb);
        let zero = self.b.ins().iconst(types::I8, 0);
        self.b.ins().brif(same_len, cmp_b, &[], done, &[zero]);
        self.b.switch_to_block(cmp_b);
        let mc = self.rt.memcmp;
        let r = self.call_rt(mc, &[pa, pb, la])[0];
        let eq = self.b.ins().icmp_imm(IntCC::Equal, r, 0);
        self.b.ins().jump(done, &[eq]);
        self.b.switch_to_block(done);
        self.b.block_params(done)[0]
    }

    /// `&Vec<T>` → `&[T]`、`&String` → `&str` の暗黙の変換 (どちらも {ptr, len} を作る)。
    fn coerce(&mut self, v: Value, from: &str, to: &str) -> R<Value> {
        let ok = (from == STRING && to == "str")
            || (self.cx.elem_of(from).is_some() && self.cx.slices.borrow().get(to).cloned() == self.cx.elem_of(from));
        if !ok {
            return Err(format!("argument of type {} where {} is expected", from, to));
        }
        let pt = self.ptr_ty();
        let slot = self.alloc(16, 8);
        let ptr = self.b.ins().load(pt, MemFlags::new(), v, 0);
        let len = self.b.ins().load(types::I64, MemFlags::new(), v, 8);
        self.b.ins().store(MemFlags::new(), ptr, slot, 0);
        self.b.ins().store(MemFlags::new(), len, slot, 8);
        Ok(slot)
    }

    fn elem_size(&self, elem: &str) -> R<u32> {
        if is_scalar(elem) { Ok(scalar_size(elem)) } else { Ok(self.cx.layout(elem, 0)?.size) }
    }

    /// `str` と `Vec` の組み込みメソッド。`p` はレシーバの先頭アドレス。
    fn builtin_method(&mut self, p: Value, rt: &str, name: &str, args: &[V], env: &mut Env) -> R<(Option<Value>, String)> {
        let i64t = types::I64;
        if rt == "str" {
            let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
            return match name {
                "len" => Ok((Some(len), "usize".into())),
                "is_empty" => {
                    let r = self.b.ins().icmp_imm(IntCC::Equal, len, 0);
                    Ok((Some(r), "bool".into()))
                }
                "as_bytes" => {
                    // `str` と `slice<u8>` は同じ配置 ({ptr, len}) なので、そのまま使える。
                    Ok((Some(p), self.cx.slice_of("u8")))
                }
                _ => Err(format!("str method {}", name)),
            };
        }
        if rt == STRING {
            return self.string_method(p, name, args, env);
        }
        let is_slice = self.cx.slices.borrow().contains_key(rt);
        let elem = self.cx.seq_elem(rt).ok_or("not a sequence")?;
        let sz = self.elem_size(&elem)?;
        if is_slice || matches!(name, "get" | "first" | "last" | "get_mut" | "sub" | "as_slice" | "as_mut_slice") {
            if let Some(r) = self.seq_method(p, &elem, sz, is_slice, name, args, env)? {
                return Ok(r);
            }
        }
        match name {
            "len" | "capacity" => {
                let off = if name == "len" { 8 } else { 16 };
                let v = self.b.ins().load(i64t, MemFlags::new(), p, off);
                Ok((Some(v), "usize".into()))
            }
            "is_empty" => {
                let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
                let r = self.b.ins().icmp_imm(IntCC::Equal, len, 0);
                Ok((Some(r), "bool".into()))
            }
            "clear" => {
                if self.cx.needs_drop(&elem) {
                    // 要素を解放してから長さを 0 にする (領域は残す)。
                    let sz = sz as i64;
                    let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
                    let base = self.b.ins().load(i64t, MemFlags::new(), p, 0);
                    let (head, body, exit) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                    self.b.append_block_param(head, i64t);
                    let zero = self.b.ins().iconst(i64t, 0);
                    self.b.ins().jump(head, &[zero]);
                    self.b.switch_to_block(head);
                    let i = self.b.block_params(head)[0];
                    let c = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
                    self.b.ins().brif(c, body, &[], exit, &[]);
                    self.b.switch_to_block(body);
                    let off = self.b.ins().imul_imm(i, sz);
                    let ea = self.b.ins().iadd(base, off);
                    self.drop_value(ea, &elem)?;
                    let i1 = self.b.ins().iadd_imm(i, 1);
                    self.b.ins().jump(head, &[i1]);
                    self.b.switch_to_block(exit);
                }
                let z = self.b.ins().iconst(i64t, 0);
                self.b.ins().store(MemFlags::new(), z, p, 8);
                Ok((None, "void".into()))
            }
            "pop" => {
                let rty = self.cx.builtin_enum("Option", &[elem.clone()])?;
                let l = self.cx.layout(&rty, 0)?;
                let res = self.alloc(l.size, l.align);
                let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
                let empty = self.b.ins().icmp_imm(IntCC::Equal, len, 0);
                let (none_b, some_b, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().brif(empty, none_b, &[], some_b, &[]);
                self.b.switch_to_block(none_b);
                let t0 = self.b.ins().iconst(types::I32, 0);
                self.b.ins().store(MemFlags::new(), t0, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(some_b);
                let newlen = self.b.ins().iadd_imm(len, -1);
                self.b.ins().store(MemFlags::new(), newlen, p, 8);
                let base = self.b.ins().load(i64t, MemFlags::new(), p, 0);
                let off = self.b.ins().imul_imm(newlen, sz as i64);
                let addr = self.b.ins().iadd(base, off);
                let val = self.load_value(addr, &elem);
                let poff = l.variants[1].1[0].1;
                let dst = self.b.ins().iadd_imm(res, poff as i64);
                self.store_value(dst, &elem, val)?;
                let t1 = self.b.ins().iconst(types::I32, 1);
                self.b.ins().store(MemFlags::new(), t1, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
                Ok((Some(res), rty))
            }
            "push" => {
                let arg = args.first().ok_or("push needs an argument")?;
                let (v, _) = self.expr(arg, env, Some(&elem))?;
                let v = v.ok_or("bad push value")?;
                self.consume(arg, &elem, env)?;
                let rty = self.cx.builtin_enum("Result", &["void".into(), "alloc.alloc.AllocError".into()])?;
                let l = self.cx.layout(&rty, 0)?;
                let res = self.alloc(l.size, l.align);
                let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
                let cap = self.b.ins().load(i64t, MemFlags::new(), p, 16);
                let ptr0 = self.b.ins().load(i64t, MemFlags::new(), p, 0);
                let full = self.b.ins().icmp(IntCC::Equal, len, cap);
                let (grow, store_b, fail_b, done) =
                    (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.append_block_param(store_b, i64t);
                self.b.ins().brif(full, grow, &[], store_b, &[ptr0]);
                self.b.switch_to_block(grow);
                let four = self.b.ins().iconst(i64t, 4);
                let dbl = self.b.ins().ishl_imm(cap, 1);
                let cap_zero = self.b.ins().icmp_imm(IntCC::Equal, cap, 0);
                let newcap = self.b.ins().select(cap_zero, four, dbl);
                let bytes = self.b.ins().imul_imm(newcap, sz as i64);
                let ra = self.rt.realloc;
                let newptr = self.call_rt(ra, &[ptr0, bytes])[0];
                let isnull = self.b.ins().icmp_imm(IntCC::Equal, newptr, 0);
                let set_b = self.b.create_block();
                self.b.ins().brif(isnull, fail_b, &[], set_b, &[]);
                self.b.switch_to_block(set_b);
                self.b.ins().store(MemFlags::new(), newptr, p, 0);
                self.b.ins().store(MemFlags::new(), newcap, p, 16);
                self.b.ins().jump(store_b, &[newptr]);
                self.b.switch_to_block(store_b);
                let base = self.b.block_params(store_b)[0];
                let off = self.b.ins().imul_imm(len, sz as i64);
                let addr = self.b.ins().iadd(base, off);
                self.store_value(addr, &elem, v)?;
                let newlen = self.b.ins().iadd_imm(len, 1);
                self.b.ins().store(MemFlags::new(), newlen, p, 8);
                let t0 = self.b.ins().iconst(types::I32, 0);
                self.b.ins().store(MemFlags::new(), t0, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(fail_b);
                let t1 = self.b.ins().iconst(types::I32, 1);
                self.b.ins().store(MemFlags::new(), t1, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
                Ok((Some(res), rty))
            }
            _ => Err(format!("Vec method {}", name)),
        }
    }

    /// `a.checked_add(b)` など: オーバーフロー・ゼロ除算なら `None`。
    fn checked_method(&mut self, recv: &V, op: &str, args: &[V], env: &mut Env, expect: Option<&str>) -> R<(Option<Value>, String)> {
        let t = self
            .ty_of(recv, env, expect.filter(|e| is_int(e)))
            .or_else(|| args.first().and_then(|a| self.ty_of(a, env, None)))
            .filter(|t| is_int(t))
            .ok_or("operand type")?;
        let (a, _) = self.expr(recv, env, Some(&t))?;
        let (b, _) = self.expr(args.first().ok_or("checked op needs an argument")?, env, Some(&t))?;
        let (a, b) = (a.ok_or("bad operand")?, b.ok_or("bad operand")?);
        let sg = signed(&t);
        let rty = self.cx.builtin_enum("Option", &[t.clone()])?;
        let l = self.cx.layout(&rty, 0)?;
        let res = self.alloc(l.size, l.align);
        let poff = l.variants[1].1[0].1 as i64;
        let (some_b, none_b, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        match op {
            "add" | "sub" | "mul" => {
                let (r, of) = match (op, sg) {
                    ("add", true) => self.b.ins().sadd_overflow(a, b),
                    ("add", false) => self.b.ins().uadd_overflow(a, b),
                    ("sub", true) => self.b.ins().ssub_overflow(a, b),
                    ("sub", false) => self.b.ins().usub_overflow(a, b),
                    (_, true) => self.b.ins().smul_overflow(a, b),
                    (_, false) => self.b.ins().umul_overflow(a, b),
                };
                self.b.ins().brif(of, none_b, &[], some_b, &[]);
                self.b.switch_to_block(some_b);
                let dst = self.b.ins().iadd_imm(res, poff);
                self.b.ins().store(MemFlags::new(), r, dst, 0);
            }
            "div" | "rem" => {
                let mut bad = self.b.ins().icmp_imm(IntCC::Equal, b, 0);
                if sg {
                    let bits = cl(&t).bits();
                    let min: i64 = if bits >= 64 { i64::MIN } else { -(1i64 << (bits - 1)) };
                    let is_min = self.b.ins().icmp_imm(IntCC::Equal, a, min);
                    let is_m1 = self.b.ins().icmp_imm(IntCC::Equal, b, -1);
                    let both = self.b.ins().band(is_min, is_m1);
                    bad = self.b.ins().bor(bad, both);
                }
                self.b.ins().brif(bad, none_b, &[], some_b, &[]);
                self.b.switch_to_block(some_b);
                let r = match (op, sg) {
                    ("div", true) => self.b.ins().sdiv(a, b),
                    ("div", false) => self.b.ins().udiv(a, b),
                    (_, true) => self.b.ins().srem(a, b),
                    (_, false) => self.b.ins().urem(a, b),
                };
                let dst = self.b.ins().iadd_imm(res, poff);
                self.b.ins().store(MemFlags::new(), r, dst, 0);
            }
            _ => return Err(format!("method checked_{}", op)),
        }
        let t1 = self.b.ins().iconst(types::I32, 1);
        self.b.ins().store(MemFlags::new(), t1, res, 0);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(none_b);
        let t0 = self.b.ins().iconst(types::I32, 0);
        self.b.ins().store(MemFlags::new(), t0, res, 0);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        Ok((Some(res), rty))
    }

    /// `a.saturating_add(b)` など (符号なし整数のみ): 範囲を超えたら端の値に止まる。
    fn saturating_method(&mut self, recv: &V, op: &str, args: &[V], env: &mut Env, expect: Option<&str>) -> R<(Option<Value>, String)> {
        let t = self
            .ty_of(recv, env, expect.filter(|e| is_int(e)))
            .or_else(|| args.first().and_then(|a| self.ty_of(a, env, None)))
            .filter(|t| is_int(t))
            .ok_or("operand type")?;
        if signed(&t) {
            return Err("saturating arithmetic on a signed integer".into());
        }
        let (a, _) = self.expr(recv, env, Some(&t))?;
        let (b, _) = self.expr(args.first().ok_or("saturating op needs an argument")?, env, Some(&t))?;
        let (a, b) = (a.ok_or("bad operand")?, b.ok_or("bad operand")?);
        let max = self.b.ins().iconst(cl(&t), -1);
        let zero = self.b.ins().iconst(cl(&t), 0);
        let r = match op {
            "add" => {
                let (r, of) = self.b.ins().uadd_overflow(a, b);
                self.b.ins().select(of, max, r)
            }
            "sub" => {
                let (r, of) = self.b.ins().usub_overflow(a, b);
                self.b.ins().select(of, zero, r)
            }
            "mul" => {
                let (r, of) = self.b.ins().umul_overflow(a, b);
                self.b.ins().select(of, max, r)
            }
            _ => return Err(format!("method saturating_{}", op)),
        };
        Ok((Some(r), t))
    }

    fn method(&mut self, recv: &V, name: &str, args: &[V], env: &mut Env, expect: Option<&str>) -> R<(Option<Value>, String)> {
        if let Some(op) = name.strip_prefix("checked_") {
            return self.checked_method(recv, op, args, env, expect);
        }
        if let Some(op) = name.strip_prefix("saturating_") {
            return self.saturating_method(recv, op, args, env, expect);
        }
        let Some(op) = name.strip_prefix("wrapping_") else { return Err(format!("method {}", name)) };
        let t = self
            .ty_of(recv, env, expect)
            .or_else(|| args.first().and_then(|a| self.ty_of(a, env, expect)))
            .ok_or("operand type")?;
        let (a, _) = self.expr(recv, env, Some(&t))?;
        let a = a.ok_or("bad operand")?;
        if op == "neg" {
            let r = self.b.ins().ineg(a);
            return Ok((Some(r), t));
        }
        let (b, _) = self.expr(&args[0], env, Some(&t))?;
        let b = b.ok_or("bad operand")?;
        let r = match op {
            "add" => self.b.ins().iadd(a, b),
            "sub" => self.b.ins().isub(a, b),
            "mul" => self.b.ins().imul(a, b),
            _ => return Err(format!("method {}", name)),
        };
        Ok((Some(r), t))
    }
}

/// 文字列リテラル (`Lit str`) の中身。16 進の文字列で持っている。
fn lit_bytes(a: &V) -> Vec<u8> {
    let hex = st(&nf(a)[1]);
    (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or(0)).collect()
}

/// `a.b.c` の形の場所 (Path か、Path の上の Field の連なり) を名前の列にする。
fn place_segs(e: &V) -> Option<Vec<String>> {
    match nname(e) {
        "Path" => Some(strs(&nf(e)[0])),
        "Field" => {
            let mut v = place_segs(&nf(e)[0])?;
            v.push(st(&nf(e)[1]));
            Some(v)
        }
        _ => None,
    }
}

fn is_ok_unit(e: &V) -> bool {
    nname(e) == "Call"
        && nname(&nf(e)[0]) == "Path"
        && strs(&nf(&nf(e)[0])[0]) == ["Ok"]
        && items(&nf(e)[1]).len() == 1
        && nname(&items(&nf(e)[1])[0]) == "Lit"
        && st(&nf(&items(&nf(e)[1])[0])[0]) == "unit"
}

fn is_print(e: &V) -> bool {
    nname(e) == "Call" && nname(&nf(e)[0]) == "Path" && strs(&nf(&nf(e)[0])[0]) == ["std", "io", "print_line"]
}

/// 整数リテラルの綴り (10 / 16 / 8 / 2 進、`_` 区切り、負号) を 64 ビットの値にする。
fn int_value(text: &str) -> R<i64> {
    let neg = text.starts_with('-');
    let t = text.trim_start_matches('-').replace('_', "");
    let (digits, radix) = if let Some(r) = t.strip_prefix("0x") {
        (r.to_string(), 16)
    } else if let Some(r) = t.strip_prefix("0b") {
        (r.to_string(), 2)
    } else if let Some(r) = t.strip_prefix("0o") {
        (r.to_string(), 8)
    } else {
        (t.clone(), 10)
    };
    let m = u64::from_str_radix(&digits, radix).map_err(|_| format!("integer literal {}", text))?;
    Ok(if neg { (m as i64).wrapping_neg() } else { m as i64 })
}

pub fn compile_clif(
    funcs: &HashMap<String, &Def>,
    type_defs: &HashMap<String, &Def>,
    entry: &str,
    out_path: &str,
    release: bool,
) -> R<()> {
    if !funcs.contains_key(entry) {
        return Err("entry not found".into());
    }
    let cx = Cx { types: type_defs, gen: RefCell::new(HashMap::new()), vecs: RefCell::new(HashMap::new()), slices: RefCell::new(HashMap::new()), tuples: RefCell::new(HashMap::new()) };
    let mut flags = settings::builder();
    flags.set("opt_level", if release { "speed" } else { "none" }).map_err(|e| e.to_string())?;
    flags.set("is_pic", "false").map_err(|e| e.to_string())?;
    let isa = cranelift_native::builder().map_err(|e| e.to_string())?.finish(settings::Flags::new(flags)).map_err(|e| e.to_string())?;
    let ob = ObjectBuilder::new(isa, "kasane", default_libcall_names()).map_err(|e| e.to_string())?;
    let mut module = ObjectModule::new(ob);
    let ptr = module.target_config().pointer_type();

    // 呼び出される関数を集める。メソッド呼び出しも辿るため、`T.name` の形の関数は型の名前で拾う。
    let mut todo = vec![entry.to_string()];
    let mut seen: HashSet<String> = HashSet::new();
    let mut order = vec![];
    while let Some(n) = todo.pop() {
        if !seen.insert(n.clone()) {
            continue;
        }
        let Some(d) = funcs.get(&n) else { continue };
        if !d.generics.is_empty() {
            return Err(format!("generic function {}", d.name));
        }
        order.push(n.clone());
        let mut calls = vec![];
        crate::cnative::calls_in(d.body.as_ref().unwrap(), &mut calls);
        todo.extend(calls);
        // メソッド呼び出しの候補: 同じ名前を持つ、型に属する関数をすべて取り込む。
        let mut mnames = vec![];
        method_names(d.body.as_ref().unwrap(), &mut mnames);
        for m in mnames {
            for (fq, fd) in funcs.iter() {
                if fd.is_method() && fq.rsplit('.').next() == Some(m.as_str()) {
                    todo.push(fq.clone());
                }
            }
        }
    }

    // ランタイム (C ライブラリ) の宣言。
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(ptr));
    sig.returns.push(AbiParam::new(types::I32));
    let puts = module.declare_function("puts", Linkage::Import, &sig).map_err(|e| e.to_string())?;
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I32));
    sig.params.push(AbiParam::new(ptr));
    sig.params.push(AbiParam::new(types::I32));
    sig.returns.push(AbiParam::new(types::I32));
    let write = module.declare_function("write", Linkage::Import, &sig).map_err(|e| e.to_string())?;
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I32));
    let exit = module.declare_function("exit", Linkage::Import, &sig).map_err(|e| e.to_string())?;
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(ptr));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(ptr));
    let realloc = module.declare_function("realloc", Linkage::Import, &sig).map_err(|e| e.to_string())?;
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(ptr));
    let free = module.declare_function("free", Linkage::Import, &sig).map_err(|e| e.to_string())?;
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(ptr));
    sig.params.push(AbiParam::new(ptr));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(ptr));
    let memcpy = module.declare_function("memcpy", Linkage::Import, &sig).map_err(|e| e.to_string())?;
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(ptr));
    sig.params.push(AbiParam::new(ptr));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I32));
    let memcmp = module.declare_function("memcmp", Linkage::Import, &sig).map_err(|e| e.to_string())?;
    let rt = Rt { puts, write, exit, realloc, free, memcpy, memcmp };

    // 関数の宣言。入口は C の `main` にする。構造体・列挙型を返す関数は、先頭に戻り値用のアドレスを取る。
    let mut ids: HashMap<String, FuncId> = HashMap::new();
    for n in &order {
        let d = funcs[n];
        let mut sig = module.make_signature();
        if n != entry && tag(&d.ret) == "ref" {
            return Err(format!("{}: returns a reference", d.name));
        }
        let rty = if n == entry { "void".to_string() } else { cx.pty(&d.ret).map_err(|e| format!("{}: {}", d.name, e))? };
        if n != entry && is_agg(&rty) {
            sig.params.push(AbiParam::new(ptr));
        }
        for (_, t) in &d.params {
            let pt = cx.pty(t).map_err(|e| format!("{}: {}", d.name, e))?;
            sig.params.push(AbiParam::new(if is_agg(&pt) { ptr } else { cl(&pt) }));
        }
        if n == entry {
            sig.returns.push(AbiParam::new(types::I32));
        } else if rty != "void" && !is_agg(&rty) {
            sig.returns.push(AbiParam::new(cl(&rty)));
        }
        let (name, link) = if n == entry { ("main".to_string(), Linkage::Export) } else { (mangle(n), Linkage::Local) };
        ids.insert(n.clone(), module.declare_function(&name, link, &sig).map_err(|e| e.to_string())?);
    }

    let mut data_n = 0usize;
    let mut fbc = FunctionBuilderContext::new();
    for n in &order {
        let d = funcs[n];
        let mut ctx = module.make_context();
        ctx.func.signature = module.declarations().get_function_decl(ids[n]).signature.clone();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
            let eb = b.create_block();
            b.append_block_params_for_function_params(eb);
            b.switch_to_block(eb);
            let params_vals = b.block_params(eb).to_vec();
            let ret_ty = if n == entry { "void".to_string() } else { cx.pty(&d.ret)? };
            let mut g = FnGen {
                b,
                module: &mut module,
                cx: &cx,
                funcs,
                ids: &ids,
                rt: &rt,
                data_n: &mut data_n,
                entry: n == entry,
                ret: d.ret.clone(),
                ret_ty,
                sret: None,
                loops: vec![],
                term: false,
                nvar: 0,
                scopes: vec![],
                owners: HashMap::new(),
                refs: HashSet::new(),
                addr_taken: HashSet::new(),
                rest: vec![],
                promoted: HashSet::new(),
            };
            g.func(d, &params_vals).map_err(|e| format!("{}: {}", d.name, e))?;
            let FnGen { mut b, .. } = g;
            b.seal_all_blocks();
            b.finalize();
        }
        module.define_function(ids[n], &mut ctx).map_err(|e| format!("{}: {}", d.name, e))?;
        module.clear_context(&mut ctx);
    }

    let bytes = module.finish().emit().map_err(|e| e.to_string())?;
    let opath = format!("{}.o", out_path);
    std::fs::write(&opath, bytes).map_err(|e| e.to_string())?;
    let cc = ["gcc", "cc", "clang"].iter().find(|c| which(c)).ok_or("no linker found (gcc / cc / clang)")?;
    let r = std::process::Command::new(cc).args(["-o", out_path, &opath]).output().map_err(|e| e.to_string())?;
    if !r.status.success() {
        let err: String = String::from_utf8_lossy(&r.stderr).trim().chars().take(300).collect();
        return Err(format!("link failed: {}", err));
    }
    Ok(())
}

/// 本体に現れるメソッド呼び出し (`x.name(..)`) の名前を集める。
fn method_names(v: &V, out: &mut Vec<String>) {
    match nname(v) {
        "MCall" => out.push(st(&nf(v)[1])),
        "Call" if nname(&nf(v)[0]) == "Path" => {
            let segs = strs(&nf(&nf(v)[0])[0]);
            if segs.len() >= 2 {
                out.push(segs[segs.len() - 1].clone());
            }
        }
        _ => {}
    }
    for c in crate::check::children(v) {
        method_names(c, out);
    }
}

fn which(cmd: &str) -> bool {
    std::process::Command::new(cmd).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}
