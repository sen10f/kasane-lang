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

use crate::check::{items, nf, nline, nname, st, strs, tag, tel};
use crate::parser::V;
use crate::program::{Def, Program};

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

/// 生ポインタ `*const T` / `*mut T` を表す型名 (`rawptr<u8>`)。値はアドレス (64 ビット)。
fn is_rawptr(p: &str) -> bool {
    p.starts_with("rawptr<")
}

fn rawptr_inner(p: &str) -> Option<&str> {
    p.strip_prefix("rawptr<").and_then(|r| r.strip_suffix('>'))
}

fn is_scalar(p: &str) -> bool {
    is_int(p) || p == "bool" || p == "char" || is_float(p) || is_mutref(p) || is_rawptr(p)
}

/// 構造体・列挙型 (メモリ上に置く値) の型名か。
fn is_agg(p: &str) -> bool {
    p != "void" && !is_scalar(p)
}

fn cl(p: &str) -> Type {
    match p {
        "i8" | "u8" | "bool" => types::I8,
        "i16" | "u16" => types::I16,
        "i32" | "u32" | "char" => types::I32,
        "f32" => types::F32,
        "f64" => types::F64,
        _ => types::I64,
    }
}

fn signed(p: &str) -> bool {
    p.starts_with('i')
}

const STRING: &str = "alloc.string.String";

/// `Name<a,b>` の `Name`。
fn base_of(ty: &str) -> &str {
    match ty.find('<') {
        Some(i) => &ty[..i],
        None => ty,
    }
}

/// `Name<a,b<c>>` を (`Name`, [`a`, `b<c>`]) に分ける (最上位のコンマだけで区切る)。
fn split_ty_args(ty: &str) -> Option<(String, Vec<String>)> {
    let i = ty.find('<')?;
    if !ty.ends_with('>') {
        return None;
    }
    let inner = &ty[i + 1..ty.len() - 1];
    let mut out = vec![];
    let (mut depth, mut cur) = (0i32, String::new());
    for ch in inner.chars() {
        match ch {
            '<' => {
                depth += 1;
                cur.push(ch);
            }
            '>' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Some((ty[..i].to_string(), out))
}

/// 定義 `d` の型パラメータの名前 (ライフタイムや効果の変数は除く)。
fn type_params(d: &Def) -> Vec<String> {
    d.generics.iter().filter(|g| g.0 == "type").map(|g| g.1.clone()).collect()
}

/// 型パラメータ `tp` の `core.ops.Fn*` 境界。
fn fn_bound_of(d: &Def, tp: &str) -> Option<V> {
    for g in &d.generics {
        if g.0 == "type" && g.1 == tp {
            for b in items(&g.2) {
                if matches!(b, V::L(bx) if st(&bx[0]).starts_with("core.ops.Fn")) {
                    return Some(b.clone());
                }
            }
        }
    }
    None
}

/// 宣言の型 `v` が型パラメータそのもの (`F` / `&F`) なら、その名前。
fn param_type_name(v: &V) -> Option<String> {
    match tag(v).as_str() {
        "named" => Some(st(tel(v, 1))),
        "ref" => param_type_name(tel(v, 2)),
        _ => None,
    }
}

/// 関数の1回の具体化 (単相化): 型パラメータを具体的な型に置いた版。
#[derive(Clone)]
struct Inst<'a> {
    key: String,
    def: &'a Def,
    subst: HashMap<String, String>,
    entry: bool,
}

/// 取り込んだ変数を関数に渡すときの型: `&mut` のスカラーと構造体などはアドレス、それ以外のスカラーは値。
fn cap_abi(kind: &str, ty: &str, ptr: Type) -> Type {
    if is_agg(ty) || (kind == "&mut" && !is_mutref(ty)) { ptr } else { cl(ty) }
}

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
        "i32" | "u32" | "f32" | "char" => 4,
        _ => 8,
    }
}

/// 型の定義表とレイアウト計算。
struct Cx<'a> {
    types: &'a HashMap<String, &'a Def>,
    prog: &'a Program,
    /// いま名前を解決しているファイル (`use` による短い名前を完全修飾名に直す)
    file: RefCell<String>,
    /// `Option<u32>` のような組み込みの総称列挙型の具体化: 名前 → [(バリアント名, ペイロードの型)]
    gen: RefCell<HashMap<String, Vec<(String, Vec<String>)>>>,
    /// `alloc.vec.Vec<u32>` のような Vec の具体化: 名前 → 要素の型
    vecs: RefCell<HashMap<String, String>>,
    /// `alloc.collections.VecDeque<u32>` のような VecDeque の具体化: 名前 → 要素の型。
    /// 実体は Vec と同じ `{ptr, len, cap}` の 24 バイト (先頭の出し入れは要素をずらす)。
    deques: RefCell<HashMap<String, String>>,
    /// `slice<u8>` のようなスライスの具体化: 名前 → 要素の型
    slices: RefCell<HashMap<String, String>>,
    /// `tuple<u32,usize>` のようなタプルの具体化: 名前 → 要素の型の列
    tuples: RefCell<HashMap<String, Vec<String>>>,
    /// `hashmap<K,V>` / `btreemap<K,V>`: 名前 → (整列するか, キーの型, 値の型)。実体は組 `(K, V)` の列 (Vec と同じ配置)。
    maps: RefCell<HashMap<String, (bool, String, String)>>,
    /// 単相化している関数の型パラメータの束縛 (`T` → `u32`)。型の名前の解決で使う。
    subst: RefCell<HashMap<String, String>>,
    /// 総称な `type` / `enum` の具体化 (`pkg.Pair<u32,bool>` → (`pkg.Pair`, [`u32`, `bool`]))
    inst_types: RefCell<HashMap<String, (String, Vec<String>)>>,
    /// `Fn` 境界の型パラメータに渡されたクロージャ: `closure<id>` → (クロージャの式, 書かれたファイル, そのときの型パラメータの束縛)
    /// 取り込む変数も持つ: (種類 `&` / `&mut` / `move`, 名前, 型) の列
    closure_types: RefCell<HashMap<String, (V, String, HashMap<String, String>, Vec<(String, String, String)>)>>,
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
            "never" => return Ok("void".into()),
            "ptr" => {
                // `*const T` / `*mut T`: 要素の型が `()` の場合 (void*) は `u8` とみなす。
                let inner = self.pty(tel(t, 2))?;
                let inner = if inner == "void" { "u8".to_string() } else { inner };
                return Ok(format!("rawptr<{}>", inner));
            }
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
                if let Some(r) = self.subst.borrow().get(&n) {
                    return Ok(r.clone());
                }
                if n == "alloc.vec.Vec" {
                    let first = items(tel(t, 2)).iter().find(|a| matches!(a, V::L(_))).ok_or("Vec without an element type")?;
                    let elem = self.pty_owned(first)?;
                    if elem == "void" {
                        return Err("Vec of unit".into());
                    }
                    return Ok(self.vec_of(&elem));
                }
                if n == "alloc.collections.VecDeque" {
                    let first = items(tel(t, 2)).iter().find(|a| matches!(a, V::L(_))).ok_or("VecDeque without an element type")?;
                    let elem = self.pty_owned(first)?;
                    if elem == "void" {
                        return Err("VecDeque of unit".into());
                    }
                    return Ok(self.deque_of(&elem));
                }
                if n == "std.thread.JoinHandle" {
                    let first = items(tel(t, 2)).iter().find(|a| matches!(a, V::L(_))).ok_or("JoinHandle without a type")?;
                    let inner = self.pty_owned(first)?;
                    if inner == "void" {
                        return Err("JoinHandle of unit".into());
                    }
                    return Ok(self.handle_of(&inner));
                }
                if n == "std.sync.Mutex" {
                    let first = items(tel(t, 2)).iter().find(|a| matches!(a, V::L(_))).ok_or("Mutex without a type")?;
                    let inner = self.pty_owned(first)?;
                    if inner == "void" {
                        return Err("Mutex of unit".into());
                    }
                    return Ok(self.mutex_of(&inner));
                }
                if n == "std.sync.MutexGuard" {
                    // `MutexGuard<'a, T>`: ライフタイムの引数を飛ばして `T` を取る。
                    let first = items(tel(t, 2)).iter().find(|a| matches!(a, V::L(_)) && tag(a) != "life").ok_or("MutexGuard without a type")?;
                    let inner = self.pty_owned(first)?;
                    if inner == "void" {
                        return Err("MutexGuard of unit".into());
                    }
                    return Ok(self.guard_of(&inner));
                }
                if n == "alloc.collections.HashMap" || n == "alloc.collections.BtreeMap" {
                    let args: Vec<&V> = items(tel(t, 2)).iter().filter(|a| matches!(a, V::L(_))).collect();
                    if args.len() < 2 {
                        return Err("map without type arguments".into());
                    }
                    let (k, v) = (self.pty_owned(args[0])?, self.pty_owned(args[1])?);
                    return self.map_of(n.ends_with("BtreeMap"), &k, &v);
                }
                if n == "alloc.alloc.AllocError" || n == STRING || n == "std.io.Error" || n == "core.str.Utf8Error" || n == "core.num.TryFromIntError" || n == "std.thread.SpawnError" || n == "std.rand.Rng" || n == "core.num.ParseIntError" {
                    return Ok(n);
                }
                if n == "Option" || n == "Result" {
                    let args: Vec<String> =
                        items(tel(t, 2)).iter().filter(|a| matches!(a, V::L(_))).map(|a| self.pty_owned(a)).collect::<R<Vec<String>>>()?;
                    return self.builtin_enum(&n, &args);
                }
                let q = self.qual_name(&n);
                if let Some(d) = self.types.get(&q) {
                    if d.kw == "type" || d.kw == "enum" {
                        if d.generics.iter().any(|g| matches!(g.0.as_str(), "const" | "uses" | "panics")) {
                            return Err("generic type with unsupported parameters".into());
                        }
                        let tps = type_params(d);
                        if tps.is_empty() {
                            return Ok(q);
                        }
                        let args: Vec<String> =
                            items(tel(t, 2)).iter().filter(|a| matches!(a, V::L(_))).map(|a| self.pty_owned(a)).collect::<R<Vec<String>>>()?;
                        if args.len() != tps.len() {
                            return Err(format!("{}: wrong number of type arguments", q));
                        }
                        return Ok(self.type_inst(&q, &args));
                    }
                }
            }
            _ => {}
        }
        Err(format!("type {}", t.dump()))
    }

    /// 名前の列を、現在のファイルの `use` に従って完全修飾名にする。
    fn qual_segs(&self, segs: &[String]) -> Vec<String> {
        if segs.is_empty() {
            return vec![];
        }
        self.prog.expand(segs, &self.file.borrow())
    }

    fn qual_name(&self, n: &str) -> String {
        if self.types.contains_key(n) {
            return n.to_string();
        }
        let segs: Vec<String> = n.split('.').map(String::from).collect();
        self.qual_segs(&segs).join(".")
    }

    /// 関数 `d` の宣言の型 `t` を、`d` が書かれたファイルの名前で、型パラメータの束縛 `subst` のもとで解決する。
    fn pty_in(&self, d: &Def, subst: &HashMap<String, String>, t: &V) -> R<String> {
        let old = self.file.replace(d.file.clone());
        let olds = self.subst.replace(subst.clone());
        let r = self.pty(t);
        *self.file.borrow_mut() = old;
        *self.subst.borrow_mut() = olds;
        r
    }

    fn pty_for(&self, d: &Def, t: &V) -> R<String> {
        self.pty_in(d, &HashMap::new(), t)
    }

    /// クロージャの式を、`Fn` 境界の型パラメータに渡す名前 `closure<id>` として登録する (同じ式・同じ文脈は同じ名前)。
    fn closure_ref(&self, node: &V, caps: &[(String, String, String)]) -> String {
        use std::hash::{Hash, Hasher};
        let file = self.file.borrow().clone();
        let subst = self.subst.borrow().clone();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        node.dump().hash(&mut h);
        file.hash(&mut h);
        let mut kv: Vec<(&String, &String)> = subst.iter().collect();
        kv.sort();
        kv.hash(&mut h);
        caps.hash(&mut h);
        let name = format!("closure<{:x}>", h.finish());
        self.closure_types.borrow_mut().insert(name.clone(), (node.clone(), file, subst, caps.to_vec()));
        name
    }

    /// 総称な型 `base` に型引数 `args` を与えた具体化の名前 (`pkg.Pair<u32,bool>`)。
    fn type_inst(&self, base: &str, args: &[String]) -> String {
        let name = format!("{}<{}>", base, args.join(","));
        self.inst_types.borrow_mut().insert(name.clone(), (base.to_string(), args.to_vec()));
        name
    }

    /// 型 `v` (宣言の側) と実際の型 `actual` を突き合わせて、型パラメータの束縛を `bind` に集める。
    fn unify_v(&self, v: &V, actual: &str, params: &HashSet<String>, bind: &mut HashMap<String, String>) -> bool {
        match tag(v).as_str() {
            "ref" => {
                let a = mutref_inner(actual).unwrap_or(actual);
                self.unify_v(tel(v, 2), a, params, bind)
            }
            "slice" => match split_ty_args(actual) {
                Some((b, args)) if b == "slice" && args.len() == 1 => self.unify_v(tel(v, 1), &args[0], params, bind),
                _ => true,
            },
            "tuple" => match split_ty_args(actual) {
                Some((b, args)) if b == "tuple" => {
                    for (x, a) in items(tel(v, 1)).iter().zip(args.iter()) {
                        if !self.unify_v(x, a, params, bind) {
                            return false;
                        }
                    }
                    true
                }
                _ => true,
            },
            "named" => {
                let n = st(tel(v, 1));
                if params.contains(&n) {
                    return match bind.get(&n) {
                        Some(x) => x == actual,
                        None => {
                            bind.insert(n, actual.to_string());
                            true
                        }
                    };
                }
                let vargs: Vec<&V> = items(tel(v, 2)).iter().filter(|a| matches!(a, V::L(_))).collect();
                if vargs.is_empty() {
                    return true;
                }
                let Some((ab, aargs)) = split_ty_args(actual) else { return true };
                let nq = self.qual_name(&n);
                let ok = ab == n
                    || ab == nq
                    || (n.ends_with("HashMap") && ab == "hashmap")
                    || (n.ends_with("BtreeMap") && ab == "btreemap");
                if !ok {
                    return true;
                }
                for (x, a) in vargs.iter().zip(aargs.iter()) {
                    if !self.unify_v(x, a, params, bind) {
                        return false;
                    }
                }
                true
            }
            _ => true,
        }
    }

    /// 総称な関数 `d` の呼び出しで、実引数の型 (`arg_tys`、レシーバの型 `recv_ty`) と期待される戻り値の型から、型パラメータを決める。
    /// 総称でなければ空の束縛。決まらない、または `Fn` 境界を持つ (クロージャを値として渡す) ときは None。
    fn infer_subst(&self, d: &Def, arg_tys: &[Option<String>], recv_ty: Option<&str>, expect: Option<&str>) -> Option<HashMap<String, String>> {
        let tps = type_params(d);
        if tps.is_empty() {
            return Some(HashMap::new());
        }
        let params: HashSet<String> = tps.iter().cloned().collect();
        let mut bind: HashMap<String, String> = HashMap::new();
        let old = self.file.replace(d.file.clone());
        let off = if recv_ty.is_some() { 1 } else { 0 };
        let mut ok = true;
        for (k, (_, pt)) in d.params.iter().enumerate() {
            let actual: Option<&str> = if k == 0 && recv_ty.is_some() {
                recv_ty
            } else {
                arg_tys.get(k - off).and_then(|x| x.as_deref())
            };
            if let Some(a) = actual {
                if !self.unify_v(pt, a, &params, &mut bind) {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            if let Some(e) = expect {
                let mut tmp = bind.clone();
                if self.unify_v(&d.ret, e, &params, &mut tmp) {
                    bind = tmp;
                }
            }
        }
        *self.file.borrow_mut() = old;
        if ok { Some(bind) } else { None }
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

    /// `HashMap` / `BtreeMap` の具体化を登録して名前を返す。キーは整数・`bool`・`char`・`&str`、値は解放が要らない型。
    fn map_of(&self, sorted: bool, k: &str, v: &str) -> R<String> {
        let key_ok = (is_scalar(k) && !is_float(k) && !is_mutref(k)) || k == "str";
        if !key_ok {
            return Err("map key type".into());
        }
        if sorted && k == "str" {
            return Err("BtreeMap with str keys".into());
        }
        if v == "void" || self.needs_drop(v) {
            return Err("map value type".into());
        }
        let name = format!("{}<{},{}>", if sorted { "btreemap" } else { "hashmap" }, k, v);
        let tup = self.tuple_of(&[k.to_string(), v.to_string()]);
        self.vecs.borrow_mut().insert(name.clone(), tup);
        self.maps.borrow_mut().insert(name.clone(), (sorted, k.to_string(), v.to_string()));
        Ok(name)
    }

    fn map_info(&self, name: &str) -> Option<(bool, String, String)> {
        self.maps.borrow().get(name).cloned()
    }

    /// `std.thread.JoinHandle<T>`: 結果の `T` を 1 つ持つ値 (スレッドは `spawn` のときに同期して実行する)。
    fn handle_of(&self, inner: &str) -> String {
        let name = format!("joinhandle<{}>", inner);
        self.tuples.borrow_mut().insert(name.clone(), vec![inner.to_string()]);
        name
    }

    /// `std.sync.Mutex<T>`: 値 `T` を 1 つ持つ値 (同期実行なので `flag` は持たない)。実体は `{T}`。
    fn mutex_of(&self, inner: &str) -> String {
        let name = format!("mutex<{}>", inner);
        self.tuples.borrow_mut().insert(name.clone(), vec![inner.to_string()]);
        name
    }

    /// `mutex<T>` の `T`。
    fn mutex_inner(&self, ty: &str) -> Option<String> {
        if !ty.starts_with("mutex<") {
            return None;
        }
        self.tuples.borrow().get(ty).and_then(|v| v.first().cloned())
    }

    /// `std.sync.MutexGuard<'a, T>`: 借用なので所有は持たない。実体はミューテックスの値のアドレス (型は `mutexguard<T>`)。
    fn guard_of(&self, inner: &str) -> String {
        let name = format!("mutexguard<{}>", inner);
        self.tuples.borrow_mut().insert(name.clone(), vec![inner.to_string()]);
        name
    }

    /// `mutexguard<T>` の `T`。
    fn guard_inner(&self, ty: &str) -> Option<String> {
        if !ty.starts_with("mutexguard<") {
            return None;
        }
        self.tuples.borrow().get(ty).and_then(|v| v.first().cloned())
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
        // `MutexGuard` はミューテックスの値のアドレスを借りているだけで、所有しない (二重解放を避ける)。
        if ty.starts_with("mutexguard<") {
            return false;
        }
        if ty == STRING || self.vecs.borrow().contains_key(ty) || self.deques.borrow().contains_key(ty) {
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
        let inst = self.inst_types.borrow().get(name).cloned();
        let def_name = inst.as_ref().map(|x| x.0.clone()).unwrap_or_else(|| name.to_string());
        let file = self.types.get(&def_name).map(|d| d.file.clone());
        match file {
            Some(f) => {
                let old = self.file.replace(f);
                let new_subst: HashMap<String, String> = match &inst {
                    Some((b, args)) => type_params(self.types[b]).into_iter().zip(args.iter().cloned()).collect(),
                    None => HashMap::new(),
                };
                let olds = self.subst.replace(new_subst);
                let r = self.layout_raw(name, depth);
                *self.file.borrow_mut() = old;
                *self.subst.borrow_mut() = olds;
                r
            }
            None => self.layout_raw(name, depth),
        }
    }

    fn layout_raw(&self, name: &str, depth: u32) -> R<Layout> {
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
        if name == STRING || self.vecs.borrow().contains_key(name) || self.deques.borrow().contains_key(name) {
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
        if name == "std.rand.Rng" {
            return Ok(Layout { size: 8, align: 8, fields: vec![("state".into(), "u64".into(), 0)], is_enum: false, variants: vec![] });
        }
        if name == "std.io.Error" || name == "core.str.Utf8Error" || name == "core.num.TryFromIntError" || name == "std.thread.SpawnError" || name == "core.num.ParseIntError" {
            return Ok(Layout { size: 8, align: 8, fields: vec![], is_enum: false, variants: vec![] });
        }
        let registered = self.gen.borrow().get(name).cloned();
        if let Some(vs) = registered {
            return self.enum_layout(vs, depth);
        }
        let base_name = self.inst_types.borrow().get(name).map(|x| x.0.clone()).unwrap_or_else(|| name.to_string());
        let d = self.types.get(&base_name).ok_or_else(|| format!("unknown type {}", name))?;
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

    fn deque_of(&self, elem: &str) -> String {
        let name = format!("alloc.collections.VecDeque<{}>", elem);
        self.deques.borrow_mut().insert(name.clone(), elem.to_string());
        name
    }

    fn deque_elem(&self, ty: &str) -> Option<String> {
        self.deques.borrow().get(ty).cloned()
    }

    /// 組み込み型 (str / Vec / AllocError) のメソッドの戻り値の型。
    fn builtin_method_ty(&self, recv_ty: &str, name: &str) -> Option<String> {
        if recv_ty == "std.rand.Rng" && name == "next_u64" {
            return Some("u64".into());
        }
        if recv_ty.starts_with("joinhandle<") && name == "join" {
            return self.tuples.borrow().get(recv_ty).and_then(|v| v.first().cloned());
        }
        // `std.sync.Mutex`: 同期実行なので `lock` は値のアドレスをそのままガードとして返す。
        if let Some(inner) = self.mutex_inner(recv_ty) {
            if name == "lock" {
                return Some(self.guard_of(&inner));
            }
        }
        // `std.sync.MutexGuard`: `get` は `&T`、`get_mut` は `&mut T`。
        // スカラーの `&T` は値、`&mut T` はアドレス (`mutref<T>`)。構造体などはどちらもアドレス。
        if let Some(inner) = self.guard_inner(recv_ty) {
            return match name {
                "get" => Some(inner),
                "get_mut" => Some(if is_agg(&inner) { inner } else { format!("mutref<{}>", inner) }),
                _ => None,
            };
        }
        if let Some(elem) = self.deque_elem(recv_ty) {
            return match name {
                "len" => Some("usize".into()),
                "is_empty" => Some("bool".into()),
                "push_back" | "push_front" => self.builtin_enum("Result", &["void".into(), "alloc.alloc.AllocError".into()]).ok(),
                "pop_back" | "pop_front" => self.builtin_enum("Option", &[elem]).ok(),
                _ => None,
            };
        }
        if let Some((_, _, v)) = self.map_info(recv_ty) {
            return match name {
                "len" => Some("usize".into()),
                "is_empty" | "contains_key" => Some("bool".into()),
                "clear" => Some("void".into()),
                "get" | "remove" => self.builtin_enum("Option", &[v]).ok(),
                "get_mut" if is_scalar(&v) && !is_mutref(&v) => self.builtin_enum("Option", &[format!("mutref<{}>", v)]).ok(),
                "insert" => {
                    let o = self.builtin_enum("Option", &[v]).ok()?;
                    self.builtin_enum("Result", &[o, "alloc.alloc.AllocError".into()]).ok()
                }
                _ => None,
            };
        }
        if recv_ty == "char" {
            return match name {
                "to_u32" => Some("u32".into()),
                "to_ascii_uppercase" | "to_ascii_lowercase" => Some("char".into()),
                "is_ascii_alphabetic" | "is_ascii_digit" | "is_ascii_whitespace" => Some("bool".into()),
                "len_utf8" => Some("usize".into()),
                _ => None,
            };
        }
        if is_int(recv_ty) && (name == "to_f64" || name == "to_f32") {
            return Some(if name == "to_f64" { "f64" } else { "f32" }.to_string());
        }
        if is_int(recv_ty) && matches!(name, "leading_zeros" | "trailing_zeros" | "count_ones" | "count_zeros") {
            return Some("u32".to_string());
        }
        if recv_ty.starts_with("Option<") && name == "take" {
            return Some(recv_ty.to_string());
        }
        // `derive Clone` の `clone()`: 解放が要らない型は、ビット列のコピー。
        if name == "clone" && !is_mutref(recv_ty) && !self.needs_drop(recv_ty) && !self.types.contains_key(&format!("{}.clone", recv_ty)) {
            return Some(recv_ty.to_string());
        }
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
                "starts_with" => Some("bool".into()),
                "find" => self.builtin_enum("Option", &["usize".into()]).ok(),
                "split_at" => {
                    let t = self.tuple_of(&["str".to_string(), "str".to_string()]);
                    self.builtin_enum("Option", &[t]).ok()
                }
                _ => None,
            };
        }
        if is_float(recv_ty) && matches!(name, "sqrt" | "floor" | "ceil" | "trunc" | "abs") {
            return Some(recv_ty.to_string());
        }
        // 浮動小数から整数への切り捨て変換 `x.trunc_to_i8()` などは `Option<i8>` を返す。
        // 128 ビット整数 (`i128` / `u128`) は未対応 (型が決まらないのでバックエンド全体をスキップ)。
        if is_float(recv_ty) {
            if let Some(to) = name.strip_prefix("trunc_to_") {
                if is_int(to) {
                    return self.builtin_enum("Option", &[to.to_string()]).ok();
                }
            }
        }
        let slice_elem = self.slices.borrow().get(recv_ty).cloned();
        if let Some(elem) = slice_elem {
            return self.seq_method_ty(&elem, name);
        }
        if recv_ty == STRING {
            return match name {
                "len" => Some("usize".into()),
                "is_empty" => Some("bool".into()),
                "clear" => Some("void".into()),
                "as_str" => Some("str".into()),
                "push_str" | "push" => self.builtin_enum("Result", &["void".into(), "alloc.alloc.AllocError".into()]).ok(),
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
            "remove" => self.builtin_enum("Option", &[elem]).ok(),
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
            "get" | "first" | "last" if is_scalar(elem) || !self.needs_drop(elem) => self.builtin_enum("Option", &[elem.to_string()]).ok(),
            "contains" if is_scalar(elem) && !is_mutref(elem) => Some("bool".into()),
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
        let segs = &self.qual_segs(segs);
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
    memmove: FuncId,
    /// UTF-8 の検証 (ptr, len) -> 1 (正しい) / 0
    utf8: FuncId,
}

struct FnGen<'a, 'b, 'm> {
    b: FunctionBuilder<'b>,
    module: &'m mut ObjectModule,
    cx: &'a Cx<'a>,
    funcs: &'a HashMap<String, &'a Def>,
    ids: &'a RefCell<HashMap<String, FuncId>>,
    /// 呼び出しで見つかった、まだ生成していない関数 (総称な関数の具体化など)
    pending: &'a RefCell<Vec<Inst<'a>>>,
    rt: &'a Rt,
    data_n: &'m mut usize,
    entry: bool,
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
    /// `let f = fn(..) ..` で束縛したクロージャ (呼び出しの場所で本体を展開する)
    closures: HashMap<String, V>,
    /// `Fn` 境界の引数として渡されたクロージャが書かれた文脈 (ファイル, 型パラメータの束縛)
    closure_ctx: HashMap<String, (String, HashMap<String, String>, Vec<(String, Variable, String, String)>)>,
    /// スロットに置いた変数の番号 (変数の値はスロットのアドレス)
    promoted: HashSet<u32>,
}

/// イテレータの始点。
enum Src {
    Range(V, V),
    /// `s.chars()`: 文字列の文字 (UTF-8 を復号する)
    Chars(V),
    /// `next` を持つ独自のイテレータ (値の式)
    Custom(V),
    /// (列の式, `iter_mut` か)
    Seq(V, bool),
    /// マップの `iter()`: 要素は `(K, V)` の組の値 (保存されている順)。
    Map(V),
}

/// イテレータの中間の段。
enum Stage {
    Map(V),
    Filter(V),
    Enumerate,
    Take(V),
}

struct Pipe {
    src: Src,
    stages: Vec<Stage>,
}

/// イテレータの終端。
enum Sink<'a> {
    /// (パターン, 本体)
    For(&'a V, &'a V),
    /// (初期値, クロージャ)
    Fold(&'a V, &'a V),
    Count,
}

/// 解放の責任を持つ変数。`flag` が真の間だけ、スコープの終わりで解放する (移動したら偽にする)。
#[derive(Clone)]
struct Owner {
    ptr: Variable,
    flag: Variable,
    ty: String,
}

impl<'a, 'b, 'm> FnGen<'a, 'b, 'm> {
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
        self.funcs.get(&format!("{}.{}", base_of(recv_ty), name)).copied()
    }

    /// `P.from_S(x)` / `P.try_from_S(x)` / `char.try_from_u32(x)` の結果の型。
    fn conv_ty(&self, segs: &[String]) -> Option<String> {
        if let [p, n] | [_, _, p, n] = segs {
            if n == "from_str_radix" && is_int(p) {
                return self.cx.builtin_enum("Result", &[p.clone(), "core.num.ParseIntError".into()]).ok();
            }
        }
        let (p, name) = match segs {
            [p, n] => (p.clone(), n.clone()),
            [c, n, p, nm] if c == "core" && n == "num" => (p.clone(), nm.clone()),
            [c, n, p, nm] if c == "core" && n == "char" && p == "char" => ("char".to_string(), nm.clone()),
            _ => return None,
        };
        if p == "char" && name == "try_from_u32" {
            return self.cx.builtin_enum("Option", &["char".into()]).ok();
        }
        if !is_int(&p) {
            return None;
        }
        if let Some(src) = name.strip_prefix("from_") {
            return if is_int(src) && cl(src).bits() <= cl(&p).bits() { Some(p) } else { None };
        }
        if let Some(src) = name.strip_prefix("try_from_") {
            return if is_int(src) { self.cx.builtin_enum("Result", &[p, "core.num.TryFromIntError".into()]).ok() } else { None };
        }
        None
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

    /// 引数の式が、クロージャの式、または `let` / 引数で束縛されたクロージャの名前なら、そのクロージャの式。
    fn resolve_closure_node(&self, a: &V) -> Option<V> {
        if nname(a) == "Closure" {
            return Some(a.clone());
        }
        if nname(a) == "Path" && strs(&nf(a)[0]).len() == 1 {
            return self.closures.get(&strs(&nf(a)[0])[0]).cloned();
        }
        None
    }

    /// クロージャ `a` (式、または名前) が取り込む変数の (種類, 名前, 型)。関数の引数として渡されて来たクロージャなら、その取り込みを引き継ぐ。
    fn closure_caps(&self, a: &V, node: &V, env: &Env) -> Option<Vec<(String, String, String)>> {
        if nname(a) == "Path" && strs(&nf(a)[0]).len() == 1 {
            if let Some((_, _, caps)) = self.closure_ctx.get(&strs(&nf(a)[0])[0]) {
                return Some(caps.iter().map(|(n, _, t, k)| (k.clone(), n.clone(), t.clone())).collect());
            }
        }
        let mut out = vec![];
        for c in items(&nf(node)[2]) {
            let kind = st(tel(c, 0));
            let name = st(tel(c, 1));
            let ty = env.get(&name)?.1.clone();
            if self.cx.needs_drop(&ty) {
                return None; // 解放が要る値を取り込む (移す) クロージャは未対応
            }
            out.push((kind, name, ty));
        }
        Some(out)
    }

    /// 呼び出しの引数の型 (わかるもの) から、総称な呼び出し先の型パラメータを決める。
    /// `Fn` 境界の型パラメータには、渡されたクロージャ (キャプチャなし) を `closure<id>` として束縛する。
    fn callee_subst(&self, d: &Def, args: &[V], recv_ty: Option<&str>, env: &Env, expect: Option<&str>) -> Option<HashMap<String, String>> {
        let tps = type_params(d);
        if tps.is_empty() {
            return Some(HashMap::new());
        }
        let off = if recv_ty.is_some() { 1 } else { 0 };
        let mut closure_binds: Vec<(String, V, Vec<(String, String, String)>)> = vec![];
        let mut arg_tys: Vec<Option<String>> = vec![];
        for (k, a) in args.iter().enumerate() {
            let tp = d.params.get(k + off).and_then(|p| param_type_name(&p.1)).filter(|n| tps.contains(n) && fn_bound_of(d, n).is_some());
            if let Some(tp) = tp {
                let node = self.resolve_closure_node(a)?;
                let caps = self.closure_caps(a, &node, env)?;
                closure_binds.push((tp, node, caps));
                arg_tys.push(None);
            } else {
                arg_tys.push(self.ty_of(a, env, None));
            }
        }
        let mut bind = self.cx.infer_subst(d, &arg_tys, recv_ty, expect)?;
        let params: HashSet<String> = tps.iter().cloned().collect();
        for (tp, node, caps) in closure_binds {
            let f = nf(&node);
            // クロージャの引数・戻り値の型 (クロージャが書かれた文脈で解決する)
            let actual_args: Vec<Option<String>> = items(&f[0]).iter().map(|p| self.cx.pty(&items(p)[1]).ok()).collect();
            let actual_ret = self.cx.pty(&f[1]).ok();
            let name = self.cx.closure_ref(&node, &caps);
            bind.insert(tp.clone(), name);
            let bound = fn_bound_of(d, &tp)?;
            let targs = tel(&bound, 1);
            let old = self.cx.file.replace(d.file.clone());
            let want_args = tel(&targs, 0);
            if tag(want_args) == "tuple" {
                for (bv, act) in items(tel(want_args, 1)).iter().zip(actual_args.iter()) {
                    if let Some(a) = act {
                        self.cx.unify_v(bv, a, &params, &mut bind);
                    }
                }
            }
            if let Some(r) = &actual_ret {
                self.cx.unify_v(tel(&targs, 1), r, &params, &mut bind);
            }
            *self.cx.file.borrow_mut() = old;
        }
        if tps.iter().all(|p| bind.contains_key(p)) { Some(bind) } else { None }
    }

    /// 呼び出しの戻り値の型 (総称な呼び出し先は、引数の型から決める)。
    fn call_ret_ty(&self, d: &Def, recv_ty: Option<&str>, args: &[V], env: &Env, expect: Option<&str>) -> Option<String> {
        let sub = self.callee_subst(d, args, recv_ty, env, expect)?;
        self.cx.pty_in(d, &sub, &d.ret).ok()
    }

    /// 型リテラル `Name { .. }` が作る型 (総称な型は、期待される型かフィールドの値の型から具体化する)。
    fn struct_lit_type(&self, f: &[V], env: &Env, expect: Option<&str>) -> Option<String> {
        let base = self.cx.qual_name(&st(&f[0]));
        let d = *self.cx.types.get(&base)?;
        let tps = type_params(d);
        if tps.is_empty() {
            return Some(base);
        }
        if let Some(e) = expect.filter(|e| base_of(e) == base) {
            return Some(e.to_string());
        }
        let params: HashSet<String> = tps.iter().cloned().collect();
        let mut bind = HashMap::new();
        let old = self.cx.file.replace(d.file.clone());
        for fl in items(&f[1]) {
            let fi = items(fl);
            let fname = st(&fi[0]);
            if let Some((_, fv, _)) = d.fields.iter().find(|x| x.0 == fname) {
                *self.cx.file.borrow_mut() = old.clone();
                let actual = self.ty_of(&fi[1], env, None);
                *self.cx.file.borrow_mut() = d.file.clone();
                if let Some(a) = actual {
                    self.cx.unify_v(fv, &a, &params, &mut bind);
                }
            }
        }
        *self.cx.file.borrow_mut() = old;
        if tps.iter().all(|p| bind.contains_key(p)) {
            let args: Vec<String> = tps.iter().map(|p| bind[p].clone()).collect();
            Some(self.cx.type_inst(&base, &args))
        } else {
            None
        }
    }

    /// 総称な列挙型 `en` のバリアント `idx` を作る式の型 (期待される型か、引数の型から具体化する)。
    fn variant_enum_type(&self, en: &str, idx: usize, args: &[V], env: &Env, expect: Option<&str>) -> Option<String> {
        let d = *self.cx.types.get(en)?;
        let tps = type_params(d);
        if tps.is_empty() {
            return Some(en.to_string());
        }
        if let Some(e) = expect.filter(|e| base_of(e) == en) {
            return Some(e.to_string());
        }
        let params: HashSet<String> = tps.iter().cloned().collect();
        let mut bind = HashMap::new();
        let old = self.cx.file.replace(d.file.clone());
        for (a, fv) in args.iter().zip(d.variants.get(idx)?.1.iter()) {
            *self.cx.file.borrow_mut() = old.clone();
            let actual = self.ty_of(a, env, None);
            *self.cx.file.borrow_mut() = d.file.clone();
            if let Some(t) = actual {
                self.cx.unify_v(fv, &t, &params, &mut bind);
            }
        }
        *self.cx.file.borrow_mut() = old;
        if tps.iter().all(|p| bind.contains_key(p)) {
            let targs: Vec<String> = tps.iter().map(|p| bind[p].clone()).collect();
            Some(self.cx.type_inst(en, &targs))
        } else {
            None
        }
    }

    /// クロージャ引数 `a` (型 `closure_ty`) が取り込む変数の、隠し引数の値。
    fn closure_cap_values(&mut self, a: &V, closure_ty: &str, env: &Env) -> R<Vec<Value>> {
        let caps = self.cx.closure_types.borrow().get(closure_ty).map(|x| x.3.clone()).unwrap_or_default();
        // 関数の引数として渡されて来たクロージャは、その隠し引数の値をそのまま引き継ぐ。
        if nname(a) == "Path" && strs(&nf(a)[0]).len() == 1 {
            if let Some((_, _, hidden)) = self.closure_ctx.get(&strs(&nf(a)[0])[0]).cloned() {
                return Ok(hidden.iter().map(|(_, var, _, _)| self.b.use_var(*var)).collect());
            }
        }
        let mut out = vec![];
        for (kind, name, ty) in &caps {
            let (var, _) = env.get(name).cloned().ok_or("captured variable not found")?;
            let v = if is_agg(ty) {
                self.b.use_var(var)
            } else if self.promoted.contains(&var.as_u32()) {
                let slot = self.b.use_var(var);
                if kind == "&mut" { slot } else { self.b.ins().load(cl(ty), MemFlags::new(), slot, 0) }
            } else if kind == "&mut" {
                return Err("mutable capture of a variable that is not addressable".into());
            } else {
                self.b.use_var(var)
            };
            out.push(v);
        }
        Ok(out)
    }

    /// `extern` の関数 `d` を、外部シンボルとして取り込んで呼ぶ (C の呼び出し規約)。引数・戻り値はスカラーとポインタだけ。
    fn extern_call(&mut self, d: &Def, args: &[V], env: &mut Env) -> R<(Option<Value>, String)> {
        let mut sig = self.module.make_signature();
        let mut ptys = vec![];
        for (_, t) in &d.params {
            let pt = self.cx.pty_for(d, t)?;
            if !is_scalar(&pt) {
                return Err(format!("extern {}: parameter of type {}", d.name, pt));
            }
            sig.params.push(AbiParam::new(cl(&pt)));
            ptys.push(pt);
        }
        let rty = self.cx.pty_for(d, &d.ret)?;
        if rty != "void" && !is_scalar(&rty) {
            return Err(format!("extern {}: return type {}", d.name, rty));
        }
        if rty != "void" {
            sig.returns.push(AbiParam::new(cl(&rty)));
        }
        if args.len() != ptys.len() {
            return Err(format!("extern {}: argument count", d.name));
        }
        let key = format!("extern:{}", d.link);
        let cached = self.ids.borrow().get(&key).copied();
        let id = match cached {
            Some(id) => id,
            None => {
                let id = self
                    .module
                    .declare_function(&d.link, Linkage::Import, &sig)
                    .map_err(|e| format!("extern {} (symbol {}): {}", d.name, d.link, e))?;
                self.ids.borrow_mut().insert(key, id);
                id
            }
        };
        let mut vals = vec![];
        for (a, pt) in args.iter().zip(&ptys) {
            let (v, t) = self.expr(a, env, Some(pt))?;
            if &t != pt {
                return Err(format!("extern {}: argument of type {} where {} is expected", d.name, t, pt));
            }
            vals.push(v.ok_or("bad argument")?);
        }
        let res = self.call_rt(id, &vals);
        if tag(&d.ret) == "never" {
            self.b.ins().trap(TrapCode::user(6).unwrap());
            self.term = true;
        }
        Ok((res.first().copied(), rty))
    }

    /// `core.ptr.*`: 生ポインタの操作。扱わない名前なら None。
    fn ptr_call(&mut self, name: &str, args: &[V], env: &mut Env, expect: Option<&str>) -> R<Option<(Option<Value>, String)>> {
        let i64t = types::I64;
        match (name, args.len()) {
            ("null_mut", 0) => {
                let t = expect.filter(|t| is_rawptr(t)).unwrap_or("rawptr<u8>").to_string();
                let z = self.b.ins().iconst(i64t, 0);
                Ok(Some((Some(z), t)))
            }
            ("is_null", 1) => {
                let (p, _) = self.expr(&args[0], env, None)?;
                let r = self.b.ins().icmp_imm(IntCC::Equal, p.ok_or("bad pointer")?, 0);
                Ok(Some((Some(r), "bool".into())))
            }
            ("as_const", 1) | ("as_mut", 1) => {
                let (p, t) = self.expr(&args[0], env, None)?;
                if !is_rawptr(&t) {
                    return Err("pointer conversion of a non-pointer".into());
                }
                Ok(Some((p, t)))
            }
            ("cast", 1) => {
                let (p, t) = self.expr(&args[0], env, None)?;
                if !is_rawptr(&t) {
                    return Err("cast of a non-pointer".into());
                }
                let to = expect.filter(|t| is_rawptr(t)).ok_or("cast needs an expected pointer type")?.to_string();
                Ok(Some((p, to)))
            }
            ("add", 2) => {
                let (p, t) = self.expr(&args[0], env, None)?;
                let elem = rawptr_inner(&t).ok_or("add on a non-pointer")?.to_string();
                let (n, _) = self.expr(&args[1], env, Some("usize"))?;
                let sz = self.elem_size(&elem)?;
                let off = self.b.ins().imul_imm(n.ok_or("bad count")?, sz as i64);
                let r = self.b.ins().iadd(p.ok_or("bad pointer")?, off);
                Ok(Some((Some(r), t)))
            }
            ("read", 1) => {
                let (p, t) = self.expr(&args[0], env, None)?;
                let elem = rawptr_inner(&t).ok_or("read of a non-pointer")?.to_string();
                let p = p.ok_or("bad pointer")?;
                if is_agg(&elem) {
                    let dst = self.alloc_for(&elem)?;
                    let size = self.cx.layout(&elem, 0)?.size;
                    self.copy_mem(dst, p, size);
                    return Ok(Some((Some(dst), elem)));
                }
                let v = self.b.ins().load(cl(&elem), MemFlags::new(), p, 0);
                Ok(Some((Some(v), elem)))
            }
            ("write", 2) => {
                let (p, t) = self.expr(&args[0], env, None)?;
                let elem = rawptr_inner(&t).ok_or("write to a non-pointer")?.to_string();
                let (v, _) = self.expr(&args[1], env, Some(&elem))?;
                self.consume(&args[1], &elem, env)?;
                self.store_value(p.ok_or("bad pointer")?, &elem, v.ok_or("bad value")?)?;
                Ok(Some((None, "void".into())))
            }
            ("from_mut", 1) | ("from_ref", 1) => {
                let (v, t) = self.expr(&args[0], env, None)?;
                if let Some(inner) = mutref_inner(&t) {
                    return Ok(Some((v, format!("rawptr<{}>", inner))));
                }
                if is_agg(&t) {
                    return Ok(Some((v, format!("rawptr<{}>", t))));
                }
                Err("from_ref of a scalar value (use from_mut on a mutable variable)".into())
            }
            _ => Ok(None),
        }
    }

    /// 関数 `d` の具体化 (`subst` が空なら元の関数) の識別子を返す。まだ宣言していなければ宣言して、生成の待ち行列に入れる。
    fn func_id(&mut self, d: &'a Def, fq: &str, subst: &HashMap<String, String>) -> R<FuncId> {
        let tps = type_params(d);
        let key = if tps.is_empty() {
            fq.to_string()
        } else {
            format!("{}<{}>", fq, tps.iter().map(|p| subst.get(p).cloned().unwrap_or_default()).collect::<Vec<_>>().join(","))
        };
        if let Some(&id) = self.ids.borrow().get(&key) {
            return Ok(id);
        }
        let sig = make_sig(self.module, self.cx, d, false, subst)?;
        let sym = format!("{}_{}", mangle(&key), self.ids.borrow().len());
        let id = self.module.declare_function(&sym, Linkage::Local, &sig).map_err(|e| e.to_string())?;
        self.ids.borrow_mut().insert(key.clone(), id);
        self.pending.borrow_mut().push(Inst { key, def: d, subst: subst.clone(), entry: false });
        Ok(id)
    }

    /// 型注釈のない `let x = Vec.new(..)` / `let x = None` の型を、後ろの文での使われ方から決める。
    fn infer_let_type(&self, init: &V, name: &str, env: &Env) -> Option<String> {
        let is_vec_new = nname(init) == "Call"
            && nname(&nf(init)[0]) == "Path"
            && strs(&nf(&nf(init)[0])[0]) == ["alloc", "vec", "Vec", "new"];
        let is_deque_new = nname(init) == "Call"
            && nname(&nf(init)[0]) == "Path"
            && strs(&nf(&nf(init)[0])[0]) == ["alloc", "collections", "VecDeque", "new"];
        let is_none = nname(init) == "Path" && strs(&nf(init)[0]) == ["None"];
        let map_kind: Option<bool> = if nname(init) == "Call" && nname(&nf(init)[0]) == "Path" {
            let sg = strs(&nf(&nf(init)[0])[0]);
            if sg.len() == 4 && sg[0] == "alloc" && sg[1] == "collections" && sg[3] == "new" && (sg[2] == "HashMap" || sg[2] == "BtreeMap") {
                Some(sg[2] == "BtreeMap")
            } else {
                None
            }
        } else {
            None
        };
        if !is_vec_new && !is_none && map_kind.is_none() && !is_deque_new {
            return None;
        }
        let fits = |t: &str| {
            if is_vec_new {
                self.cx.elem_of(t).is_some() && self.cx.map_info(t).is_none()
            } else if is_deque_new {
                self.cx.deque_elem(t).is_some()
            } else if map_kind.is_some() {
                self.cx.map_info(t).is_some()
            } else {
                t.starts_with("Option<")
            }
        };
        let refers = |a: &V| -> bool {
            let a = if nname(a) == "Unary" && matches!(st(&nf(a)[0]).as_str(), "&" | "&mut") { &nf(a)[1] } else { a };
            nname(a) == "Path" && strs(&nf(a)[0]) == [name.to_string()]
        };
        let rest = self.rest.clone();
        let mut nodes: Vec<&V> = vec![];
        for st_ in &rest {
            crate::check::walk(st_, &mut nodes);
        }
        // 後ろの文で定義される変数 (`let` と、範囲を回す `for` の変数) の型を、順に足していく。
        let mut local = env.clone();
        let ph = Variable::from_u32(u32::MAX);
        for n in &nodes {
            if nname(n) == "Let" && nname(&nf(n)[0]) == "PBind" {
                let ann = if matches!(nf(n)[2], V::Nil) { None } else { self.cx.pty(&nf(n)[2]).ok() };
                if let Some(t) = ann.or_else(|| self.ty_of(&nf(n)[3], &local, None)) {
                    local.insert(st(&nf(&nf(n)[0])[0]), (ph, t));
                }
            }
            if nname(n) == "For" && nname(&nf(n)[0]) == "PBind" {
                let it = &nf(n)[1];
                if nname(it) == "Call" && nname(&nf(it)[0]) == "Path" && strs(&nf(&nf(it)[0])[0]) == ["core", "iter", "range"] {
                    let a = items(&nf(it)[1]);
                    if a.len() == 2 {
                        if let Some(t) = self.ty_of(&a[0], &local, None).or_else(|| self.ty_of(&a[1], &local, None)) {
                            local.insert(st(&nf(&nf(n)[0])[0]), (ph, t));
                        }
                    }
                }
            }
            match nname(n) {
                // m.insert(k, v) の k, v の型
                "MCall" if map_kind.is_some() && st(&nf(n)[1]) == "insert" && items(&nf(n)[2]).len() == 2 => {
                    let r = &nf(n)[0];
                    if nname(r) == "Path" && strs(&nf(r)[0]) == [name.to_string()] {
                        let a = items(&nf(n)[2]);
                        if let (Some(k), Some(v)) = (self.ty_of(&a[0], &local, None), self.ty_of(&a[1], &local, None)) {
                            if let Ok(t) = self.cx.map_of(map_kind.unwrap_or(false), &k, &v) {
                                return Some(t);
                            }
                        }
                    }
                }
                // q.push_back(x) / q.push_front(x) の x の型
                "MCall" if is_deque_new && matches!(st(&nf(n)[1]).as_str(), "push_back" | "push_front") && items(&nf(n)[2]).len() == 1 => {
                    let r = &nf(n)[0];
                    if nname(r) == "Path" && strs(&nf(r)[0]) == [name.to_string()] {
                        if let Some(t) = self.ty_of(&items(&nf(n)[2])[0], &local, None) {
                            if t != "void" && !is_mutref(&t) {
                                return Some(self.cx.deque_of(&t));
                            }
                        }
                    }
                }
                // v.push(x) の x の型
                "MCall" if is_vec_new && st(&nf(n)[1]) == "push" && items(&nf(n)[2]).len() == 1 => {
                    let r = &nf(n)[0];
                    if nname(r) == "Path" && strs(&nf(r)[0]) == [name.to_string()] {
                        if let Some(t) = self.ty_of(&items(&nf(n)[2])[0], &local, None) {
                            if t != "void" && !is_mutref(&t) {
                                return Some(self.cx.vec_of(&t));
                            }
                        }
                    }
                }
                "Call" if nname(&nf(n)[0]) == "Path" => {
                    let segs = strs(&nf(&nf(n)[0])[0]);
                    let args = items(&nf(n)[1]);
                    if map_kind.is_some() && segs.len() == 2 && segs[0] == name && segs[1] == "insert" && args.len() == 2 {
                        if let (Some(k), Some(v)) = (self.ty_of(&args[0], &local, None), self.ty_of(&args[1], &local, None)) {
                            if let Ok(t) = self.cx.map_of(map_kind.unwrap_or(false), &k, &v) {
                                return Some(t);
                            }
                        }
                    }
                    if is_vec_new && segs.len() == 2 && segs[0] == name && segs[1] == "push" && args.len() == 1 {
                        if let Some(t) = self.ty_of(&args[0], &local, None) {
                            if t != "void" && !is_mutref(&t) {
                                return Some(self.cx.vec_of(&t));
                            }
                        }
                    }
                    if is_deque_new && segs.len() == 2 && segs[0] == name && matches!(segs[1].as_str(), "push_back" | "push_front") && args.len() == 1 {
                        if let Some(t) = self.ty_of(&args[0], &local, None) {
                            if t != "void" && !is_mutref(&t) {
                                return Some(self.cx.deque_of(&t));
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
                            if let Some(t) = self.ty_of(&items(&nf(rhs)[1])[0], &local, None) {
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

    /// パターン `pat` が型 `ty` の値に当たったときの束縛の型を、`env` に足す (コードは出さない)。
    fn bind_types(&self, pat: &V, ty: &str, env: &mut Env) {
        let ph = Variable::from_u32(u32::MAX);
        match nname(pat) {
            "PBind" => {
                env.insert(st(&nf(pat)[0]), (ph, ty.to_string()));
            }
            "PTuple" => {
                if let Ok(l) = self.cx.layout(ty, 0) {
                    for (sp, (_, fty, _)) in items(&nf(pat)[0]).iter().zip(l.fields.iter()) {
                        self.bind_types(sp, fty, env);
                    }
                }
            }
            "PVariant" => {
                let segs = strs(&nf(pat)[0]);
                if let Ok(l) = self.cx.layout(ty, 0) {
                    if let Some(idx) = l.variants.iter().position(|v| v.0 == segs[segs.len() - 1]) {
                        for (sp, (fty, _)) in items(&nf(pat)[1]).iter().zip(l.variants[idx].1.iter()) {
                            self.bind_types(sp, fty, env);
                        }
                    }
                }
            }
            _ => {}
        }
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
                "char" => Some("char".into()),
                "byte" => Some("u8".into()),
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
                let q = self.cx.qual_segs(&segs);
                if let Some((t, _, _)) = numeric_const(&q) {
                    return Some(t);
                }
                if let Some(&ci) = self.cx.prog.consts.get(&q.join(".")) {
                    let d = &self.cx.prog.defs[ci];
                    return self.cx.pty_for(d, &d.ret).ok();
                }
                let (en, idx, _) = self.cx.split_variant(&segs)?;
                self.variant_enum_type(&en, idx, &[], env, expect)
            }
            "Field" => {
                let segs = place_segs(e)?;
                if env.contains_key(&segs[0]) { self.field_chain_ty(&segs, env) } else { None }
            }
            "Try" => {
                if is_print(&f[0]) {
                    return Some("void".into());
                }
                let t = self.ty_of(&f[0], env, None)?;
                let ok_idx = if t.starts_with("Option<") { 1 } else if t.starts_with("Result<") { 0 } else { return None };
                let l = self.cx.layout(&t, 0).ok()?;
                Some(l.variants.get(ok_idx)?.1.first().map(|x| x.0.clone()).unwrap_or_else(|| "void".to_string()))
            }
            "ArrayE" => {
                let want = expect.and_then(|t| self.cx.slices.borrow().get(t).cloned());
                let first = items(&f[0]).first()?;
                let elem = self.ty_of(first, env, want.as_deref())?;
                if elem == "void" {
                    return None;
                }
                Some(self.cx.slice_of(&elem))
            }
            "Unsafe" => self.body_ty(&f[2], env, expect),
            "TupleE" => {
                let want: Option<Vec<String>> = expect.and_then(|t| self.cx.tuples.borrow().get(t).cloned());
                let mut elems = vec![];
                for (k, x) in items(&f[0]).iter().enumerate() {
                    let ex = want.as_ref().and_then(|w| w.get(k)).map(|s| s.as_str());
                    elems.push(self.ty_of(x, env, ex)?);
                }
                Some(self.cx.tuple_of(&elems))
            }
            "StructLit" => self.struct_lit_type(f, env, expect),
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
                // 腕の本体の型 (パターンで束縛される変数の型を足して求める)。`panic` など値を返さない腕は飛ばす。
                let sty = self.ty_of(&f[0], env, None);
                for arm in items(&f[1]) {
                    let a = items(arm);
                    let mut local = env.clone();
                    if let Some(st_) = &sty {
                        self.bind_types(&a[0], st_, &mut local);
                    }
                    if let Some(t) = self.body_ty(&a[1], &local, expect) {
                        if t != "void" {
                            return Some(t);
                        }
                    }
                }
                None
            }
            "MCall" => {
                let name = st(&f[1]);
                let recv_ty = self.ty_of(&f[0], env, expect).map(|t| mutref_inner(&t).map(String::from).unwrap_or(t));
                if let (Some(rt), "map_err") = (&recv_ty, name.as_str()) {
                    if let Some(a) = items(&f[2]).first() {
                        return self.map_err_ty(rt, a, env);
                    }
                }
                if let Some(rt) = &recv_ty {
                    if let Some(t) = self.cx.builtin_method_ty(rt, &name) {
                        return Some(t);
                    }
                    if is_agg(rt) {
                        return self.method_target(rt, &name).and_then(|d| self.call_ret_ty(d, Some(rt), items(&f[2]), env, expect));
                    }
                }
                self.ty_of(&f[0], env, expect).or_else(|| items(&f[2]).first().and_then(|a| self.ty_of(a, env, expect)))
            }
            "Call" => {
                if nname(&f[0]) != "Path" {
                    return None;
                }
                let segs = strs(&nf(&f[0])[0]);
                let segs = if env.contains_key(&segs[0]) { segs } else { self.cx.qual_segs(&segs) };
                if segs.len() >= 2 && env.contains_key(&segs[0]) {
                    let recv_ty = self.field_chain_ty(&segs[..segs.len() - 1], env)?;
                    let name = &segs[segs.len() - 1];
                    if name == "map_err" && items(&f[1]).len() == 1 {
                        return self.map_err_ty(&recv_ty, &items(&f[1])[0], env);
                    }
                    if let Some(t) = self.cx.builtin_method_ty(&recv_ty, name) {
                        return Some(t);
                    }
                    if is_agg(&recv_ty) {
                        return self.method_target(&recv_ty, name).and_then(|d| self.call_ret_ty(d, Some(&recv_ty), items(&f[1]), env, expect));
                    }
                    return Some(recv_ty);
                }
                if let Some((en, idx, _)) = self.cx.split_variant(&segs) {
                    return self.variant_enum_type(&en, idx, items(&f[1]), env, expect);
                }
                if segs.len() >= 3 {
                    if let Some((en, _, _)) = self.cx.split_variant(&segs[..segs.len() - 1]) {
                        let name = &segs[segs.len() - 1];
                        return self.method_target(&en, name).and_then(|d| self.call_ret_ty(d, Some(&en), items(&f[1]), env, expect));
                    }
                }
                if segs.len() == 1 && matches!(segs[0].as_str(), "Some" | "Ok" | "Err") && !self.funcs.contains_key(&segs[0]) {
                    return self.ctor_type(&segs[0], items(&f[1]), env, expect);
                }
                if segs == ["alloc", "vec", "Vec", "new"] {
                    return expect.filter(|t| self.cx.elem_of(t).is_some()).map(String::from);
                }
                if segs == ["alloc", "collections", "VecDeque", "new"] {
                    return expect.filter(|t| self.cx.deque_elem(t).is_some()).map(String::from);
                }
                // `std.sync.Mutex.new(value)`: 値の型が `T`。
                if segs == ["std", "sync", "Mutex", "new"] && items(&f[1]).len() == 1 {
                    let inner = self.ty_of(&items(&f[1])[0], env, None)?;
                    if inner == "void" {
                        return None;
                    }
                    return Some(self.cx.mutex_of(&inner));
                }
                if segs == ["core", "iter", "count"] {
                    return Some("usize".into());
                }
                if segs.len() == 4
                    && segs[0] == "alloc"
                    && segs[1] == "collections"
                    && (segs[2] == "HashMap" || segs[2] == "BtreeMap")
                    && segs[3] == "new"
                {
                    return expect.filter(|t| self.cx.map_info(t).is_some()).map(String::from);
                }
                if segs == ["core", "str", "from_utf8"] {
                    return self.cx.builtin_enum("Result", &["str".into(), "core.str.Utf8Error".into()]).ok();
                }
                if let Some(t) = self.conv_ty(&segs) {
                    return Some(t);
                }
                if segs.len() == 3 && segs[0] == "core" && segs[1] == "ptr" {
                    let args = items(&f[1]);
                    return match segs[2].as_str() {
                        "null_mut" => Some(expect.filter(|t| is_rawptr(t)).unwrap_or("rawptr<u8>").to_string()),
                        "is_null" => Some("bool".into()),
                        "write" => Some("void".into()),
                        "as_const" | "as_mut" | "add" => self.ty_of(args.first()?, env, None),
                        "cast" => expect.filter(|t| is_rawptr(t)).map(String::from),
                        "read" => self.ty_of(args.first()?, env, None).and_then(|t| rawptr_inner(&t).map(String::from)),
                        "from_mut" | "from_ref" => {
                            let t = self.ty_of(args.first()?, env, None)?;
                            Some(format!("rawptr<{}>", mutref_inner(&t).unwrap_or(&t)))
                        }
                        _ => None,
                    };
                }
                if let Some(&ei) = self.cx.prog.sigs.get(&segs.join(".")) {
                    let ed = &self.cx.prog.defs[ei];
                    if ed.kw == "extern" {
                        return self.cx.pty_for(ed, &ed.ret).ok();
                    }
                }
                if segs == ["std", "rand", "Rng", "from_seed"] || segs == ["std", "rand", "Rng", "from_entropy"] {
                    return Some("std.rand.Rng".into());
                }
                if segs == ["std", "thread", "spawn"] && items(&f[1]).len() == 2 {
                    let clo = &items(&f[1])[1];
                    let node = if nname(clo) == "Closure" {
                        Some(clo.clone())
                    } else if nname(clo) == "Path" && strs(&nf(clo)[0]).len() == 1 {
                        self.closures.get(&strs(&nf(clo)[0])[0]).cloned()
                    } else {
                        None
                    };
                    let t = self.cx.pty(&nf(&node?)[1]).ok()?;
                    if t == "void" {
                        return None;
                    }
                    let hty = self.cx.handle_of(&t);
                    return self.cx.builtin_enum("Result", &[hty, "std.thread.SpawnError".into()]).ok();
                }
                if segs == ["core", "panic", "panic"] {
                    return Some("void".into());
                }
                if segs == ["core", "iter", "fold"] && items(&f[1]).len() == 3 {
                    return self.ty_of(&items(&f[1])[1], env, expect);
                }
                if segs.len() == 1 && self.closures.contains_key(&segs[0]) {
                    let clo = &self.closures[&segs[0]];
                    return match self.closure_ctx.get(&segs[0]) {
                        Some((file, sub, _)) => {
                            let of = self.cx.file.replace(file.clone());
                            let os = self.cx.subst.replace(sub.clone());
                            let r = self.cx.pty(&nf(clo)[1]).ok();
                            *self.cx.file.borrow_mut() = of;
                            *self.cx.subst.borrow_mut() = os;
                            r
                        }
                        None => self.cx.pty(&nf(clo)[1]).ok(),
                    };
                }
                if segs == ["alloc", "string", "String", "new"] {
                    return Some(STRING.to_string());
                }
                if segs == ["alloc", "string", "String", "from_str"] {
                    return self.cx.builtin_enum("Result", &[STRING.to_string(), "alloc.alloc.AllocError".to_string()]).ok();
                }
                if segs == ["std", "io", "print_line"] {
                    return self.cx.builtin_enum("Result", &["void".into(), "std.io.Error".into()]).ok();
                }
                if segs == ["core", "check", "eq"] || segs == ["core", "mem", "drop"] {
                    return Some("void".into());
                }
                self.funcs.get(&segs.join(".")).and_then(|d| self.call_ret_ty(d, None, items(&f[1]), env, expect))
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
            "StructLit" | "MCall" | "TupleE" | "ArrayE" => true,
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
        let seq_elem = self.cx.elem_of(ty).or_else(|| self.cx.deque_elem(ty));
        if ty == STRING || seq_elem.is_some() {
            if let Some(elem) = seq_elem {
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

    /// `main` が `Err` を返した: 標準エラーに出して終了コード 1 で終わる。
    fn entry_error_exit(&mut self) -> R<()> {
        self.write_stderr(format!("Error: main returned an error{}", char::from(10u8)).as_bytes())?;
        let code = self.b.ins().iconst(types::I32, 1);
        let x = self.rt.exit;
        self.call_rt(x, &[code]);
        self.b.ins().trap(TrapCode::user(4).unwrap());
        self.term = true;
        Ok(())
    }

    /// 条件が真のとき `emit` が出すコード (終端する) へ分岐し、偽のときは続きへ進む。
    fn guard_with(&mut self, cond: Value, emit: impl FnOnce(&mut Self) -> R<()>) -> R<()> {
        let (yes, no) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(cond, yes, &[], no, &[]);
        self.b.switch_to_block(yes);
        emit(self)?;
        self.b.switch_to_block(no);
        self.term = false;
        Ok(())
    }

    /// 定数のバイト列を標準エラーに書く。
    fn write_stderr(&mut self, bytes: &[u8]) -> R<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let n = bytes.len() as i64;
        let p = self.string_ptr(bytes.to_vec())?;
        let fd = self.b.ins().iconst(types::I32, 2);
        let len = self.b.ins().iconst(types::I32, n);
        let w = self.rt.write;
        self.call_rt(w, &[fd, p, len]);
        Ok(())
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
                if nname(n) == "Closure" {
                    for c in items(&nf(n)[2]) {
                        if st(tel(c, 0)) == "&mut" {
                            self.addr_taken.insert(st(tel(c, 1)));
                        }
                    }
                }
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
            if ty.starts_with("closure<") {
                // `Fn` 境界の引数: 実行時の値は持たず、呼び出し (`f(x)`) の場所で本体を展開する。
                let reg = self.cx.closure_types.borrow().get(&ty).cloned();
                let (node, file, sub, caps) = reg.ok_or("unknown closure type")?;
                let mut hidden = vec![];
                for (kind, cname, cty) in &caps {
                    let v = params_vals[k];
                    k += 1;
                    let abi = cap_abi(kind, cty, self.ptr_ty());
                    let var = if abi == self.ptr_ty() && !is_agg(cty) { self.new_var("usize") } else { self.new_var(cty) };
                    self.b.def_var(var, v);
                    if is_agg(cty) {
                        self.refs.insert(var.as_u32());
                    } else if kind == "&mut" {
                        // スカラーの `&mut` の取り込みは、アドレスを持つ変数 (読み書きはアドレス経由)
                        self.promoted.insert(var.as_u32());
                    }
                    hidden.push((cname.clone(), var, cty.clone(), kind.clone()));
                }
                self.closures.insert(n.clone(), node);
                self.closure_ctx.insert(n.clone(), (file, sub, hidden));
                continue;
            }
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
        // 最初に失敗した文の行を、エラーに付ける (原因を探しやすくする)。
        self.stmt_inner(s, env, tail).map_err(|e| if e.contains("(line ") { e } else { format!("{} (line {})", e, nline(s)) })
    }

    fn stmt_inner(&mut self, s: &V, env: &mut Env, tail: bool) -> R<()> {
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
            "Let" if nname(&f[0]) == "PBind" && nname(&f[3]) == "Closure" => {
                // クロージャは値を持たず、呼び出しの場所で本体を展開する。
                self.closures.insert(st(&nf(&f[0])[0]), f[3].clone());
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
                // 接尾辞のない浮動小数リテラルだけの初期値は f64
                let known = known.or_else(|| {
                    if nname(&f[3]) == "Lit" && st(&nf(&f[3])[0]) == "float" { Some("f64".to_string()) } else { None }
                });
                let ty = match known {
                    Some(t) if t != "void" => t,
                    _ => return Err(format!("let of unknown type: {}", lname)),
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
            "Unsafe" => {
                let mut inner = env.clone();
                self.block(&f[2], &mut inner, tail)
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
                        // 末尾が `Err(..)`、または `Result` を返す式なら、`Err` のとき終了コード 1 で終わる。
                        if nname(e) == "Call" && nname(&nf(e)[0]) == "Path" && strs(&nf(&nf(e)[0])[0]) == ["Err"] {
                            self.entry_error_exit()?;
                            return Ok(());
                        }
                        let (v, t) = self.expr(e, env, None)?;
                        if !t.starts_with("Result<") {
                            return Err("entry tail expression".into());
                        }
                        let p = v.ok_or("bad tail value")?;
                        let tg = self.b.ins().load(types::I32, MemFlags::new(), p, 0);
                        let is_err = self.b.ins().icmp_imm(IntCC::NotEqual, tg, 0);
                        self.guard_with(is_err, |g| g.entry_error_exit())?;
                        return Ok(());
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

    // ------------------------------------------------------------ イテレータの連なり
    /// `for` や `fold` / `count` の元になるイテレータの式を、始点と中間の段に分ける。
    fn parse_pipe(&self, it: &V, env: &Env) -> Option<Pipe> {
        if nname(it) == "Call" && nname(&nf(it)[0]) == "Path" {
            let segs = strs(&nf(&nf(it)[0])[0]);
            let segs = if env.contains_key(&segs[0]) { segs } else { self.cx.qual_segs(&segs) };
            let args = items(&nf(it)[1]);
            let core_iter = |n: &str| segs.len() == 3 && segs[0] == "core" && segs[1] == "iter" && segs[2] == n;
            if core_iter("range") && args.len() == 2 {
                return Some(Pipe { src: Src::Range(args[0].clone(), args[1].clone()), stages: vec![] });
            }
            for (name, nargs) in [("map", 2usize), ("filter", 2), ("enumerate", 1), ("take", 2)] {
                if core_iter(name) && args.len() == nargs {
                    let mut p = self.parse_pipe(&args[0], env)?;
                    p.stages.push(match name {
                        "map" => Stage::Map(args[1].clone()),
                        "filter" => Stage::Filter(args[1].clone()),
                        "enumerate" => Stage::Enumerate,
                        _ => Stage::Take(args[1].clone()),
                    });
                    return Some(p);
                }
            }
            if segs.len() >= 2 && env.contains_key(&segs[0]) && segs[segs.len() - 1] == "chars" && args.is_empty() {
                let recv = V::Node("Path", vec![V::L(segs[..segs.len() - 1].iter().map(|x| V::S(x.clone())).collect()), V::Nil], 0);
                return Some(Pipe { src: Src::Chars(recv), stages: vec![] });
            }
            if segs.len() >= 2 && env.contains_key(&segs[0]) && matches!(segs[segs.len() - 1].as_str(), "iter" | "iter_mut") && args.is_empty() {
                let recv = V::Node("Path", vec![V::L(segs[..segs.len() - 1].iter().map(|x| V::S(x.clone())).collect()), V::Nil], 0);
                return Some(Pipe { src: Src::Seq(recv, segs[segs.len() - 1] == "iter_mut"), stages: vec![] });
            }
        }
        if nname(it) == "MCall" && st(&nf(it)[1]) == "chars" && items(&nf(it)[2]).is_empty() {
            return Some(Pipe { src: Src::Chars(nf(it)[0].clone()), stages: vec![] });
        }
        if nname(it) == "MCall" && matches!(st(&nf(it)[1]).as_str(), "iter" | "iter_mut") && items(&nf(it)[2]).is_empty() {
            let recv = nf(it)[0].clone();
            // マップの `iter()` は、保存されている組の列を順に見る (`BtreeMap` は挿入時にキー順を保つ)。
            if self.ty_of(&recv, env, None).map_or(false, |t| self.cx.map_info(&t).is_some()) {
                return Some(Pipe { src: Src::Map(recv), stages: vec![] });
            }
            return Some(Pipe { src: Src::Seq(recv, st(&nf(it)[1]) == "iter_mut"), stages: vec![] });
        }
        // 配列リテラルやスライスの値そのもの (`for x in [a, b, c]`)
        let t = self.ty_of(it, env, None)?;
        if self.cx.slices.borrow().contains_key(&t) {
            return Some(Pipe { src: Src::Seq(it.clone(), false), stages: vec![] });
        }
        // `next(&mut self) -> Option<T>` を持つ型の値
        if is_agg(&t) && !self.cx.needs_drop(&t) && self.funcs.contains_key(&format!("{}.next", t)) {
            return Some(Pipe { src: Src::Custom(it.clone()), stages: vec![] });
        }
        None
    }

    /// クロージャ (式、または `let` で束縛した名前) の本体を、引数を束縛してその場に展開する。
    fn inline_closure(&mut self, clo: &V, args: &[(Value, String)], env: &Env) -> R<(Option<Value>, String)> {
        // 関数の引数として渡されたクロージャは、渡した側の文脈 (ファイル・型パラメータの束縛) で評価する。
        let ctx = if nname(clo) == "Path" && strs(&nf(clo)[0]).len() == 1 {
            self.closure_ctx.get(&strs(&nf(clo)[0])[0]).cloned()
        } else {
            None
        };
        if let Some((file, sub, hidden)) = ctx {
            let of = self.cx.file.replace(file);
            let os = self.cx.subst.replace(sub);
            // 取り込んだ変数の名前を、隠し引数の変数に結び付ける (関数の局所変数と名前が重なっても、こちらが優先)
            let mut env2 = env.clone();
            for (n, var, ty, _) in &hidden {
                env2.insert(n.clone(), (*var, ty.clone()));
            }
            let r = self.inline_closure_in(clo, args, &env2);
            *self.cx.file.borrow_mut() = of;
            *self.cx.subst.borrow_mut() = os;
            return r;
        }
        self.inline_closure_in(clo, args, env)
    }

    fn inline_closure_in(&mut self, clo: &V, args: &[(Value, String)], env: &Env) -> R<(Option<Value>, String)> {
        let node: V = if nname(clo) == "Closure" {
            clo.clone()
        } else if nname(clo) == "Path" && strs(&nf(clo)[0]).len() == 1 {
            self.closures.get(&strs(&nf(clo)[0])[0]).cloned().ok_or("call of an unknown closure")?
        } else {
            return Err("call of a non-closure".into());
        };
        let f = nf(&node);
        let mut nodes: Vec<&V> = vec![];
        crate::check::walk(&f[3], &mut nodes);
        if nodes.iter().any(|n| nname(n) == "Return") {
            return Err("return inside a closure".into());
        }
        let ps = items(&f[0]);
        if ps.len() != args.len() {
            return Err("closure arity".into());
        }
        let mut inner = env.clone();
        for (p, (val, ty)) in ps.iter().zip(args) {
            let pty = self.cx.pty(&items(p)[1])?;
            if &pty != ty {
                return Err(format!("closure parameter type mismatch: {} vs {}", pty, ty));
            }
            let var = self.new_var(&pty);
            self.b.def_var(var, *val);
            if is_agg(&pty) {
                self.refs.insert(var.as_u32());
            }
            inner.insert(st(&items(p)[0]), (var, pty));
        }
        let rty = self.cx.pty(&f[1])?;
        self.scopes.push(vec![]);
        let (v, t) = self.arm_value(&f[3], &mut inner, if rty == "void" { None } else { Some(&rty) })?;
        let d = self.scopes.len() - 1;
        self.drop_scopes_from(d)?;
        self.scopes.pop();
        if is_agg(&t) && self.cx.needs_drop(&t) {
            return Err("closure returning a droppable value".into());
        }
        Ok((v, if rty != "void" { rty } else { t }))
    }

    /// 束縛パターン (`x` / `_` / `(a, b)`) に、型 `ty` の値 `val` を束縛する。
    fn bind_item(&mut self, pat: &V, val: Value, ty: &str, inner: &mut Env) -> R<()> {
        match nname(pat) {
            "PBind" => {
                let var = self.new_var(ty);
                self.b.def_var(var, val);
                if is_agg(ty) {
                    self.refs.insert(var.as_u32());
                }
                inner.insert(st(&nf(pat)[0]), (var, ty.to_string()));
                Ok(())
            }
            "PWild" => Ok(()),
            "PTuple" => {
                if !self.cx.tuples.borrow().contains_key(ty) {
                    return Err("tuple pattern on a non-tuple item".into());
                }
                let l = self.cx.layout(ty, 0)?;
                let pats = items(&nf(pat)[0]).to_vec();
                if pats.len() != l.fields.len() {
                    return Err("tuple pattern arity".into());
                }
                for (sp, (_, fty, off)) in pats.iter().zip(l.fields.iter()) {
                    let addr = self.b.ins().iadd_imm(val, *off as i64);
                    let v = self.load_value(addr, fty);
                    self.bind_item(sp, v, fty, inner)?;
                }
                Ok(())
            }
            _ => Err("for pattern".into()),
        }
    }

    /// イテレータの連なりを 1 つのループにまとめて出す。クロージャは本体をその場に展開する。
    fn run_pipe(&mut self, pipe: &Pipe, env: &mut Env, sink: Sink) -> R<(Option<Value>, String)> {
        let i64t = types::I64;
        let pt = self.ptr_ty();
        let (head, body_b, cont, exit) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
        // 段ごとの状態 (take の上限と数、enumerate の番号) は、ループの前に用意する。
        let mut take_state: Vec<(Variable, Value)> = vec![];
        let mut enum_vars: Vec<Variable> = vec![];
        for st_ in &pipe.stages {
            match st_ {
                Stage::Take(n) => {
                    let (lim, _) = self.expr(n, env, Some("usize"))?;
                    let var = self.new_var("usize");
                    let z = self.b.ins().iconst(i64t, 0);
                    self.b.def_var(var, z);
                    take_state.push((var, lim.ok_or("bad take count")?));
                }
                Stage::Enumerate => {
                    let var = self.new_var("usize");
                    let z = self.b.ins().iconst(i64t, 0);
                    self.b.def_var(var, z);
                    enum_vars.push(var);
                }
                _ => {}
            }
        }
        // 終端の状態
        let mut acc: Option<(Variable, String)> = None;
        match &sink {
            Sink::Fold(init, _) => {
                let t = self.ty_of(init, env, None).filter(|t| t != "void").ok_or("fold initial value type")?;
                if is_agg(&t) && self.cx.needs_drop(&t) {
                    return Err("fold with a droppable accumulator".into());
                }
                if is_agg(&t) {
                    return Err("fold with an aggregate accumulator".into());
                }
                let (iv, _) = self.expr(init, env, Some(&t))?;
                let var = self.new_var(&t);
                self.b.def_var(var, iv.ok_or("bad initial value")?);
                acc = Some((var, t));
            }
            Sink::Count => {
                let var = self.new_var("usize");
                let z = self.b.ins().iconst(i64t, 0);
                self.b.def_var(var, z);
                acc = Some((var, "usize".to_string()));
            }
            Sink::For(..) => {}
        }
        // 始点
        let (index_ty, start, item_ty, elem_info): (Type, Value, String, Option<(Value, Value, String, bool)>);
        let limit: Value;
        // 独自のイテレータ: (値のアドレス, `next` の完全修飾名, `Option<Item>` の型)
        let mut custom: Option<(Value, String, String)> = None;
        // `chars()`: (文字列の先頭アドレス, 1 文字ぶん進める幅を入れる変数)
        let mut chars_state: Option<(Value, Variable)> = None;
        // マップの `iter()`: (先頭アドレス, キーの型, 値の型, キーのオフセット, 値のオフセット, 組の大きさ)
        let mut map_state: Option<(Value, String, String, i64, i64, i64)> = None;
        match &pipe.src {
            Src::Chars(e) => {
                let (sp, sty) = self.expr(e, env, None)?;
                let sp = sp.ok_or("bad string")?;
                if sty != "str" && sty != STRING {
                    return Err("chars of a non-string".into());
                }
                let base = self.b.ins().load(pt, MemFlags::new(), sp, 0);
                let len = self.b.ins().load(i64t, MemFlags::new(), sp, 8);
                let step = self.new_var("usize");
                let one = self.b.ins().iconst(i64t, 1);
                self.b.def_var(step, one);
                index_ty = i64t;
                start = self.b.ins().iconst(i64t, 0);
                limit = len;
                item_ty = "char".to_string();
                elem_info = None;
                chars_state = Some((base, step));
            }
            Src::Custom(e) => {
                let (cp, ct) = self.expr(e, env, None)?;
                let cp = cp.ok_or("bad iterator")?;
                let fq = format!("{}.next", ct);
                let d = self.funcs.get(&fq).copied().ok_or("iterator without next")?;
                let opt_ty = self.cx.pty_for(d, &d.ret)?;
                if !opt_ty.starts_with("Option<") {
                    return Err("next must return an Option".into());
                }
                let l = self.cx.layout(&opt_ty, 0)?;
                let item = l.variants[1].1.first().map(|x| x.0.clone()).ok_or("iterator of unit")?;
                index_ty = i64t;
                start = self.b.ins().iconst(i64t, 0);
                limit = self.b.ins().iconst(i64t, -1);
                item_ty = item;
                elem_info = None;
                custom = Some((cp, fq, opt_ty));
            }
            Src::Range(a, b) => {
                let t = self
                    .ty_of(a, env, None)
                    .or_else(|| self.ty_of(b, env, None))
                    .filter(|t| is_int(t))
                    .ok_or("range element type")?;
                let (av, _) = self.expr(a, env, Some(&t))?;
                let (bv, _) = self.expr(b, env, Some(&t))?;
                index_ty = cl(&t);
                start = av.ok_or("bad range")?;
                limit = bv.ok_or("bad range")?;
                item_ty = t;
                elem_info = None;
            }
            Src::Seq(recv, by_mut) => {
                let (sp, sty) = self.expr(recv, env, None)?;
                let sp = sp.ok_or("bad iterable")?;
                let elem = self.cx.seq_elem(&sty).ok_or("iteration over a non-sequence")?;
                let base = self.b.ins().load(pt, MemFlags::new(), sp, 0);
                let len = self.b.ins().load(i64t, MemFlags::new(), sp, 8);
                index_ty = i64t;
                start = self.b.ins().iconst(i64t, 0);
                limit = len;
                item_ty = elem.clone();
                elem_info = Some((base, len, elem, *by_mut));
            }
            Src::Map(e) => {
                let (sp, sty) = self.expr(e, env, None)?;
                let sp = sp.ok_or("bad iterable")?;
                let (_sorted, kty, vty) = self.cx.map_info(&sty).ok_or("iteration over a non-map")?;
                let tup = self.cx.tuple_of(&[kty.clone(), vty.clone()]);
                let l = self.cx.layout(&tup, 0)?;
                let (koff, voff, stride) = (l.fields[0].2 as i64, l.fields[1].2 as i64, l.size as i64);
                let base = self.b.ins().load(pt, MemFlags::new(), sp, 0);
                let len = self.b.ins().load(i64t, MemFlags::new(), sp, 8);
                index_ty = i64t;
                start = self.b.ins().iconst(i64t, 0);
                limit = len;
                item_ty = tup;
                elem_info = None;
                map_state = Some((base, kty, vty, koff, voff, stride));
            }
        }
        self.b.append_block_param(head, index_ty);
        self.b.ins().jump(head, &[start]);
        self.b.switch_to_block(head);
        let i = self.b.block_params(head)[0];
        // take の上限に達していたら、元のイテレータを進めずに終わる。
        let more = if matches!(pipe.src, Src::Range(..)) {
            let cc = if signed(&item_ty) { IntCC::SignedLessThan } else { IntCC::UnsignedLessThan };
            self.b.ins().icmp(cc, i, limit)
        } else {
            self.b.ins().icmp(IntCC::UnsignedLessThan, i, limit)
        };
        let mut go = more;
        for (cvar, lim) in &take_state {
            let c = self.b.use_var(*cvar);
            let reached = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, c, *lim);
            let not_reached = self.b.ins().bxor_imm(reached, 1);
            go = self.b.ins().band(go, not_reached);
        }
        self.b.ins().brif(go, body_b, &[], exit, &[]);
        self.b.switch_to_block(body_b);
        // 要素
        let (mut val, mut ty) = match &elem_info {
            None if chars_state.is_some() => {
                // UTF-8 を 1 文字ぶん復号して、進める幅を記録する。
                let (base, step) = chars_state.ok_or("chars state")?;
                let at = self.b.ins().iadd(base, i);
                let byte = |g: &mut Self, off: i64| -> Value {
                    let a = g.b.ins().iadd_imm(at, off);
                    let v = g.b.ins().load(types::I8, MemFlags::new(), a, 0);
                    g.b.ins().uextend(types::I32, v)
                };
                let b0 = byte(self, 0);
                let (w1, w2, w3, w4, merge) =
                    (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.append_block_param(merge, types::I32);
                let (t12, t34, t3) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                let lt80 = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, b0, 0x80);
                self.b.ins().brif(lt80, w1, &[], t12, &[]);
                self.b.switch_to_block(t12);
                let lte0 = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, b0, 0xE0);
                self.b.ins().brif(lte0, w2, &[], t34, &[]);
                self.b.switch_to_block(t34);
                let ltf0 = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, b0, 0xF0);
                self.b.ins().brif(ltf0, w3, &[], t3, &[]);
                self.b.switch_to_block(t3);
                self.b.ins().jump(w4, &[]);
                // 1 バイト
                self.b.switch_to_block(w1);
                let one = self.b.ins().iconst(i64t, 1);
                self.b.def_var(step, one);
                self.b.ins().jump(merge, &[b0]);
                // 2 バイト
                self.b.switch_to_block(w2);
                let b1 = byte(self, 1);
                let hi = self.b.ins().band_imm(b0, 0x1F);
                let hi = self.b.ins().ishl_imm(hi, 6);
                let lo = self.b.ins().band_imm(b1, 0x3F);
                let c2 = self.b.ins().bor(hi, lo);
                let two = self.b.ins().iconst(i64t, 2);
                self.b.def_var(step, two);
                self.b.ins().jump(merge, &[c2]);
                // 3 バイト
                self.b.switch_to_block(w3);
                let b1 = byte(self, 1);
                let b2 = byte(self, 2);
                let p0 = self.b.ins().band_imm(b0, 0x0F);
                let p0 = self.b.ins().ishl_imm(p0, 12);
                let p1 = self.b.ins().band_imm(b1, 0x3F);
                let p1 = self.b.ins().ishl_imm(p1, 6);
                let p2 = self.b.ins().band_imm(b2, 0x3F);
                let c3 = self.b.ins().bor(p0, p1);
                let c3 = self.b.ins().bor(c3, p2);
                let three = self.b.ins().iconst(i64t, 3);
                self.b.def_var(step, three);
                self.b.ins().jump(merge, &[c3]);
                // 4 バイト
                self.b.switch_to_block(w4);
                let b1 = byte(self, 1);
                let b2 = byte(self, 2);
                let b3 = byte(self, 3);
                let q0 = self.b.ins().band_imm(b0, 0x07);
                let q0 = self.b.ins().ishl_imm(q0, 18);
                let q1 = self.b.ins().band_imm(b1, 0x3F);
                let q1 = self.b.ins().ishl_imm(q1, 12);
                let q2 = self.b.ins().band_imm(b2, 0x3F);
                let q2 = self.b.ins().ishl_imm(q2, 6);
                let q3 = self.b.ins().band_imm(b3, 0x3F);
                let c4 = self.b.ins().bor(q0, q1);
                let c4 = self.b.ins().bor(c4, q2);
                let c4 = self.b.ins().bor(c4, q3);
                let four = self.b.ins().iconst(i64t, 4);
                self.b.def_var(step, four);
                self.b.ins().jump(merge, &[c4]);
                self.b.switch_to_block(merge);
                (self.b.block_params(merge)[0], "char".to_string())
            }
            None if custom.is_some() => {
                // 独自のイテレータ: `next` を呼んで、`None` なら終わる。
                let (cp, fq, opt_ty) = custom.clone().ok_or("custom iterator state")?;
                let d = self.funcs.get(&fq).copied().ok_or("iterator without next")?;
                let (res, _) = self.call_fn_values(&fq, d, &[cp])?;
                let res = res.ok_or("next returned nothing")?;
                let tg = self.b.ins().load(types::I32, MemFlags::new(), res, 0);
                let some = self.b.ins().icmp_imm(IntCC::Equal, tg, 1);
                let got = self.b.create_block();
                self.b.ins().brif(some, got, &[], exit, &[]);
                self.b.switch_to_block(got);
                let l = self.cx.layout(&opt_ty, 0)?;
                let (ity, ioff) = l.variants[1].1[0].clone();
                let addr = self.b.ins().iadd_imm(res, ioff as i64);
                (self.load_value(addr, &ity), ity)
            }
            None if map_state.is_some() => {
                // マップの組を 1 つ読み、`(K, V)` の組を作る (値はコピー)。
                let (base, kty, vty, koff, voff, stride) = map_state.clone().ok_or("map iterator state")?;
                let off = self.b.ins().imul_imm(i, stride);
                let ea = self.b.ins().iadd(base, off);
                let tname = self.cx.tuple_of(&[kty.clone(), vty.clone()]);
                let l = self.cx.layout(&tname, 0)?;
                let slot = self.alloc(l.size, l.align);
                let ka = self.b.ins().iadd_imm(ea, koff);
                let kv = self.load_value(ka, &kty);
                let kd = self.b.ins().iadd_imm(slot, l.fields[0].2 as i64);
                self.store_value(kd, &kty, kv)?;
                let va = self.b.ins().iadd_imm(ea, voff);
                let vv = self.load_value(va, &vty);
                let vd = self.b.ins().iadd_imm(slot, l.fields[1].2 as i64);
                self.store_value(vd, &vty, vv)?;
                (slot, tname)
            }
            None => (i, item_ty.clone()),
            Some((base, _, elem, by_mut)) => {
                let sz = self.elem_size(elem)?;
                let off = self.b.ins().imul_imm(i, sz as i64);
                let addr = self.b.ins().iadd(*base, off);
                if *by_mut && !is_agg(elem) {
                    (addr, format!("mutref<{}>", elem))
                } else {
                    (self.load_value(addr, elem), elem.clone())
                }
            }
        };
        // 中間の段
        let (mut ti, mut ei) = (0usize, 0usize);
        for st_ in &pipe.stages {
            match st_ {
                Stage::Map(f) => {
                    let (v, t) = self.inline_closure(f, &[(val, ty.clone())], env)?;
                    val = v.ok_or("map closure returns unit")?;
                    ty = t;
                }
                Stage::Filter(f) => {
                    let (c, _) = self.inline_closure(f, &[(val, ty.clone())], env)?;
                    let pass = self.b.create_block();
                    self.b.ins().brif(c.ok_or("filter closure returns unit")?, pass, &[], cont, &[]);
                    self.b.switch_to_block(pass);
                }
                Stage::Enumerate => {
                    let var = enum_vars[ei];
                    ei += 1;
                    let tname = self.cx.tuple_of(&["usize".to_string(), ty.clone()]);
                    let l = self.cx.layout(&tname, 0)?;
                    let slot = self.alloc(l.size, l.align);
                    let cur = self.b.use_var(var);
                    let a0 = self.b.ins().iadd_imm(slot, l.fields[0].2 as i64);
                    self.b.ins().store(MemFlags::new(), cur, a0, 0);
                    let a1 = self.b.ins().iadd_imm(slot, l.fields[1].2 as i64);
                    self.store_value(a1, &ty, val)?;
                    let nx = self.b.ins().iadd_imm(cur, 1);
                    self.b.def_var(var, nx);
                    val = slot;
                    ty = tname;
                }
                Stage::Take(_) => {
                    let (cvar, _) = take_state[ti];
                    ti += 1;
                    let c = self.b.use_var(cvar);
                    let nx = self.b.ins().iadd_imm(c, 1);
                    self.b.def_var(cvar, nx);
                }
            }
        }
        // 終端
        match &sink {
            Sink::For(pat, body) => {
                let mut inner = env.clone();
                self.bind_item(pat, val, &ty, &mut inner)?;
                let depth = self.scopes.len();
                self.loops.push((cont, exit, depth));
                self.block(body, &mut inner, false)?;
                self.loops.pop();
                if !self.term {
                    self.b.ins().jump(cont, &[]);
                }
            }
            Sink::Fold(_, f) => {
                let (var, aty) = acc.clone().ok_or("fold state")?;
                let cur = self.b.use_var(var);
                let (nv, nt) = self.inline_closure(f, &[(cur, aty.clone()), (val, ty.clone())], env)?;
                if nt != aty {
                    return Err(format!("fold closure returns {} but the accumulator is {}", nt, aty));
                }
                self.b.def_var(var, nv.ok_or("fold closure returns unit")?);
                self.b.ins().jump(cont, &[]);
            }
            Sink::Count => {
                let (var, _) = acc.clone().ok_or("count state")?;
                let cur = self.b.use_var(var);
                let nx = self.b.ins().iadd_imm(cur, 1);
                self.b.def_var(var, nx);
                self.b.ins().jump(cont, &[]);
            }
        }
        self.b.switch_to_block(cont);
        let n = match chars_state {
            Some((_, step)) => {
                let w = self.b.use_var(step);
                self.b.ins().iadd(i, w)
            }
            None => self.b.ins().iadd_imm(i, 1),
        };
        self.b.ins().jump(head, &[n]);
        self.b.switch_to_block(exit);
        self.term = false;
        match acc {
            Some((var, t)) => Ok((Some(self.b.use_var(var)), t)),
            None => Ok((None, "void".into())),
        }
    }

    fn for_stmt(&mut self, e: &V, env: &mut Env) -> R<()> {
        let f = nf(e);
        let pipe = self.parse_pipe(&f[1], env).ok_or("unsupported iterator in for")?;
        self.run_pipe(&pipe, env, Sink::For(&f[0], &f[2]))?;
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
    /// 束縛 (名前, 値, 型) を返す。パターンが必ず当てはまるなら true。入れ子のパターンにも対応する。
    fn arm_test(
        &mut self,
        pat: &V,
        scrut: Value,
        sty: &str,
        _tagv: Option<Value>,
        body: Block,
        next: Block,
    ) -> R<(Vec<(String, Value, String)>, bool)> {
        let mut binds = vec![];
        let refutable = self.pat_chain(pat, scrut, sty, next, &mut binds)?;
        self.b.ins().jump(body, &[]);
        Ok((binds, !refutable))
    }

    /// パターンを値 `val` (スカラーは値、それ以外はアドレス) に当てる。外れたら `next` へ飛び、当たれば続きのブロックで続ける。
    /// 検査が 1 つでもあれば true (必ず当てはまるのではない)。
    fn pat_chain(&mut self, pat: &V, val: Value, ty: &str, next: Block, binds: &mut Vec<(String, Value, String)>) -> R<bool> {
        let pf = nf(pat);
        match nname(pat) {
            "PWild" => Ok(false),
            "PBind" => {
                binds.push((st(&pf[0]), val, ty.to_string()));
                Ok(false)
            }
            "PLit" => {
                if is_agg(ty) || is_float(ty) {
                    return Err("literal pattern on this type".into());
                }
                let lit = &pf[0];
                let want: i64 = match st(&nf(lit)[0]).as_str() {
                    "int" => int_value(&st(&nf(lit)[1]))?,
                    "bool" => {
                        if matches!(nf(lit)[1], V::B(true)) { 1 } else { 0 }
                    }
                    "char" | "byte" => st(&nf(lit)[1]).parse().map_err(|_| "character literal")?,
                    k => return Err(format!("pattern literal {}", k)),
                };
                let c = self.b.ins().icmp_imm(IntCC::Equal, val, want);
                let cont = self.b.create_block();
                self.b.ins().brif(c, cont, &[], next, &[]);
                self.b.switch_to_block(cont);
                Ok(true)
            }
            "PTuple" => {
                if !self.cx.tuples.borrow().contains_key(ty) {
                    return Err("tuple pattern on a non-tuple".into());
                }
                let l = self.cx.layout(ty, 0)?;
                let pats = items(&pf[0]).to_vec();
                if pats.len() != l.fields.len() {
                    return Err("tuple pattern arity".into());
                }
                let mut refutable = false;
                for (sp, (_, fty, off)) in pats.iter().zip(l.fields.iter()) {
                    let addr = self.b.ins().iadd_imm(val, *off as i64);
                    let v = self.load_value(addr, fty);
                    refutable |= self.pat_chain(sp, v, fty, next, binds)?;
                }
                Ok(refutable)
            }
            "PPath" | "PVariant" => {
                let segs = strs(&pf[0]);
                let l = self.cx.layout(ty, 0)?;
                if !l.is_enum {
                    return Err("variant pattern on a struct".into());
                }
                let vn = segs[segs.len() - 1].clone();
                let idx = l.variants.iter().position(|v| v.0 == vn).ok_or_else(|| format!("no variant {}", vn))?;
                let tg = self.b.ins().load(types::I32, MemFlags::new(), val, 0);
                let c = self.b.ins().icmp_imm(IntCC::Equal, tg, idx as i64);
                let cont = self.b.create_block();
                self.b.ins().brif(c, cont, &[], next, &[]);
                self.b.switch_to_block(cont);
                if nname(pat) == "PVariant" {
                    for (j, sp) in items(&pf[1]).iter().enumerate() {
                        let Some((fty, off)) = l.variants[idx].1.get(j).cloned() else {
                            // `Ok(_)` / `Ok(())` / `Ok(_u)`: ペイロードが空 (`()`) のバリアントに対するパターン。
                            if matches!(nname(sp), "PWild" | "PBind") || (nname(sp) == "PLit" && st(&nf(&nf(sp)[0])[0]) == "unit") {
                                continue;
                            }
                            return Err("too many sub-patterns".into());
                        };
                        let addr = self.b.ins().iadd_imm(val, off as i64);
                        let v = self.load_value(addr, &fty);
                        self.pat_chain(sp, v, &fty, next, binds)?;
                    }
                }
                Ok(true)
            }
            n => Err(format!("pattern {}", n)),
        }
    }

    /// 束縛を env に入れる (値はそのまま新しい変数に入れる)。
    fn bind_arm(&mut self, binds: Vec<(String, Value, String)>, env: &mut Env, _from_variant: bool) {
        for (name, v, ty) in binds {
            let var = self.new_var(&ty);
            self.b.def_var(var, v);
            if is_agg(&ty) {
                self.refs.insert(var.as_u32());
            }
            env.insert(name, (var, ty));
        }
    }

    /// `match` の共通部分。`value` が Some のとき、各腕の値をそのブロック引数に渡す。
    fn match_common(&mut self, e: &V, env: &mut Env, tail: bool, value: Option<(&str, Block, Option<Value>)>) -> R<()> {
        let f = nf(e);
        let (sv, sty) = self.expr(&f[0], env, None)?;
        let sv = sv.ok_or("match on unit")?;
        let tagv = if is_agg(&sty) { Some(self.b.ins().load(types::I32, MemFlags::new(), sv, 0)) } else { None };
        let end = match value {
            Some((_, end, _)) => end,
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
                Some((ty, end, out)) => {
                    let (v, _) = self.arm_value(&a[1], &mut inner, Some(ty))?;
                    if !self.term {
                        if let Some(slot) = out {
                            // 構造体・列挙型を作る `match`: 各腕の値を結果の領域に写す。
                            if let Some(v) = v {
                                self.store_value(slot, ty, v)?;
                            }
                            self.b.ins().jump(end, &[]);
                        } else {
                            match v {
                                Some(v) => self.b.ins().jump(end, &[v]),
                                None => self.b.ins().jump(end, &[]),
                            };
                        }
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
    fn call_user(
        &mut self,
        fq: &str,
        d: &'a Def,
        recv: Option<(Value, String)>,
        args: &[V],
        env: &mut Env,
        expect: Option<&str>,
    ) -> R<(Option<Value>, String)> {
        if tag(&d.ret) == "ref" {
            return Err("call of a function returning a reference".into());
        }
        let recv_ty = recv.as_ref().map(|r| r.1.clone());
        let subst = if type_params(d).is_empty() {
            HashMap::new()
        } else {
            self.callee_subst(d, args, recv_ty.as_deref(), env, expect).ok_or("cannot infer the type arguments of a generic call")?
        };
        let id = self.func_id(d, fq, &subst)?;
        let rt = self.cx.pty_in(d, &subst, &d.ret)?;
        let mut vals = vec![];
        let mut out = None;
        if is_agg(&rt) {
            let p = self.alloc_for(&rt)?;
            vals.push(p);
            out = Some(p);
        }
        let mut params = d.params.iter();
        if let Some((r, _)) = recv {
            params.next();
            vals.push(r);
        }
        for ((_, pt), a) in params.zip(args) {
            let want = self.cx.pty_in(d, &subst, pt)?;
            if want.starts_with("closure<") {
                // クロージャそのものは実行時の引数にしない (関数の中で本体を展開する)。取り込む変数だけを渡す。
                let cvals = self.closure_cap_values(a, &want, env)?;
                vals.extend(cvals);
                continue;
            }
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
                "char" | "byte" => {
                    let n: i64 = st(&f[1]).parse().map_err(|_| "character literal")?;
                    let t = if st(&f[0]) == "char" { "char" } else { "u8" };
                    let c = self.b.ins().iconst(cl(t), n);
                    Ok((Some(c), t.to_string()))
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
                let q = self.cx.qual_segs(&segs);
                if let Some((t, iv, fv)) = numeric_const(&q) {
                    let c = match (iv, fv) {
                        (Some(i), _) => self.b.ins().iconst(cl(&t), i),
                        (_, Some(f)) => {
                            if t == "f32" { self.b.ins().f32const(f as f32) } else { self.b.ins().f64const(f) }
                        }
                        _ => return Err("numeric constant".into()),
                    };
                    return Ok((Some(c), t));
                }
                if let Some(&ci) = self.cx.prog.consts.get(&q.join(".")) {
                    // ユーザー定義の定数: 使う場所で、初期値の式を (その定数のファイルの名前で) 評価する。
                    let d = &self.cx.prog.defs[ci];
                    let ty = self.cx.pty_for(d, &d.ret)?;
                    let init = d.init.clone().ok_or("constant without a value")?;
                    let old = self.cx.file.replace(d.file.clone());
                    let mut empty = Env::new();
                    let r = self.expr(&init, &mut empty, Some(&ty));
                    *self.cx.file.borrow_mut() = old;
                    return r;
                }
                if let Some((en, idx, _)) = self.cx.split_variant(&segs) {
                    let en = self.variant_enum_type(&en, idx, &[], env, expect).ok_or("cannot infer the type arguments of an enum")?;
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
                if is_print(inner) {
                    // `print_line(..)?`: 標準出力への出力は失敗しない扱い。
                    self.print_value(&items(&nf(inner)[1])[0], env)?;
                    return Ok((None, "void".into()));
                }
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
            "Unsafe" => {
                // `unsafe "名前" "理由" { .. }`: 本体のブロックの値。範囲を抜けるときの解放も行う。
                self.scopes.push(vec![]);
                let mut inner = env.clone();
                let r = self.arm_value(&f[2], &mut inner, expect);
                let d = self.scopes.len() - 1;
                if r.is_ok() && !self.term {
                    self.drop_scopes_from(d)?;
                }
                self.scopes.pop();
                r
            }
            "ArrayE" => {
                let ty = self.ty_of(e, env, expect).ok_or("array element type")?;
                let elem = self.cx.slices.borrow().get(&ty).cloned().ok_or("array type")?;
                if self.cx.needs_drop(&elem) {
                    return Err("array of droppable elements".into());
                }
                let sz = self.elem_size(&elem)?;
                let n = items(&f[0]).len() as u32;
                let buf = self.alloc((sz * n).max(1), if is_agg(&elem) { 8 } else { sz.max(1) });
                for (k, x) in items(&f[0]).iter().enumerate() {
                    let (v, _) = self.expr(x, env, Some(&elem))?;
                    let addr = self.b.ins().iadd_imm(buf, (k as u32 * sz) as i64);
                    self.store_value(addr, &elem, v.ok_or("bad array element")?)?;
                }
                let slot = self.alloc(16, 8);
                self.b.ins().store(MemFlags::new(), buf, slot, 0);
                let len = self.b.ins().iconst(types::I64, n as i64);
                self.b.ins().store(MemFlags::new(), len, slot, 8);
                Ok((Some(slot), ty))
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
                let name = self.struct_lit_type(f, env, expect).ok_or("cannot infer the type arguments of a struct literal")?;
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
                let out = if is_agg(&ty) {
                    if self.cx.needs_drop(&ty) {
                        return Err("match producing a droppable value".into());
                    }
                    Some(self.alloc_for(&ty)?)
                } else {
                    None
                };
                let end = self.b.create_block();
                if ty != "void" && out.is_none() {
                    self.b.append_block_param(end, cl(&ty));
                }
                self.match_common(e, env, false, Some((&ty, end, out)))?;
                if let Some(slot) = out {
                    return Ok((Some(slot), ty));
                }
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
                let segs = if env.contains_key(&segs[0]) { segs } else { self.cx.qual_segs(&segs) };
                let args = items(&f[1]);
                if segs.len() >= 2 && env.contains_key(&segs[0]) {
                    let recv = V::Node("Path", vec![V::L(segs[..segs.len() - 1].iter().map(|x| V::S(x.clone())).collect()), V::Nil], 0);
                    return self.method_call(&recv, &segs[segs.len() - 1], args, env, expect);
                }
                if let Some((en, idx, _)) = self.cx.split_variant(&segs) {
                    let en = self.variant_enum_type(&en, idx, args, env, expect).ok_or("cannot infer the type arguments of an enum")?;
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
                if segs == ["core", "panic", "panic"] && args.len() == 1 {
                    // メッセージを標準エラーに出して、終了コード 101 で終わる。
                    self.write_stderr(b"panic: ")?;
                    if nname(&args[0]) == "Lit" && st(&nf(&args[0])[0]) == "str" {
                        let bytes = lit_bytes(&args[0]);
                        self.write_stderr(&bytes)?;
                    } else {
                        let (sv, t) = self.expr(&args[0], env, Some("str"))?;
                        if t != "str" {
                            return Err("panic with a non-string message".into());
                        }
                        let sv = sv.ok_or("bad message")?;
                        let pt = self.ptr_ty();
                        let ptr = self.b.ins().load(pt, MemFlags::new(), sv, 0);
                        let len = self.b.ins().load(types::I64, MemFlags::new(), sv, 8);
                        let len32 = self.b.ins().ireduce(types::I32, len);
                        let fd = self.b.ins().iconst(types::I32, 2);
                        let w = self.rt.write;
                        self.call_rt(w, &[fd, ptr, len32]);
                    }
                    self.write_stderr(&[10u8])?;
                    let code = self.b.ins().iconst(types::I32, 101);
                    let x = self.rt.exit;
                    self.call_rt(x, &[code]);
                    self.b.ins().trap(TrapCode::user(5).unwrap());
                    self.term = true;
                    return Ok((None, "void".into()));
                }
                if let Some(r) = self.conv_call(&segs, args, env)? {
                    return Ok(r);
                }
                if segs.len() == 3 && segs[0] == "core" && segs[1] == "ptr" {
                    if let Some(r) = self.ptr_call(&segs[2], args, env, expect)? {
                        return Ok(r);
                    }
                }
                if let Some(&ei) = self.cx.prog.sigs.get(&segs.join(".")) {
                    let ed = &self.cx.prog.defs[ei];
                    if ed.kw == "extern" {
                        return self.extern_call(ed, args, env);
                    }
                }
                if segs == ["std", "rand", "Rng", "from_seed"] && args.len() == 1 {
                    let (sv, _) = self.expr(&args[0], env, Some("u64"))?;
                    let slot = self.alloc(8, 8);
                    self.b.ins().store(MemFlags::new(), sv.ok_or("bad seed")?, slot, 0);
                    return Ok((Some(slot), "std.rand.Rng".into()));
                }
                if segs == ["std", "rand", "Rng", "from_entropy"] && args.is_empty() {
                    let slot = self.alloc(8, 8);
                    let c = self.b.ins().iconst(types::I64, 0x9E3779B97F4A7C15u64 as i64);
                    self.b.ins().store(MemFlags::new(), c, slot, 0);
                    return Ok((Some(slot), "std.rand.Rng".into()));
                }
                if segs == ["std", "thread", "spawn"] && args.len() == 2 {
                    // スレッドは spawn のときに同期して実行する (インタプリタと同じ)。
                    let (v, t) = self.inline_closure(&args[1], &[], env)?;
                    let v = v.ok_or("spawn of a closure returning unit")?;
                    let hty = self.cx.handle_of(&t);
                    let hl = self.cx.layout(&hty, 0)?;
                    let handle = self.alloc(hl.size, hl.align);
                    let faddr = self.b.ins().iadd_imm(handle, hl.fields[0].2 as i64);
                    self.store_value(faddr, &t, v)?;
                    let rty = self.cx.builtin_enum("Result", &[hty.clone(), "std.thread.SpawnError".into()])?;
                    let rl = self.cx.layout(&rty, 0)?;
                    let res = self.alloc(rl.size, rl.align);
                    let t0 = self.b.ins().iconst(types::I32, 0);
                    self.b.ins().store(MemFlags::new(), t0, res, 0);
                    let dst = self.b.ins().iadd_imm(res, rl.variants[0].1[0].1 as i64);
                    self.store_value(dst, &hty, handle)?;
                    return Ok((Some(res), rty));
                }
                if segs == ["core", "str", "from_utf8"] && args.len() == 1 {
                    let (bv, bt) = self.expr(&args[0], env, None)?;
                    let bv = bv.ok_or("bad bytes")?;
                    let ok_elem = self.cx.seq_elem(&bt).map_or(false, |e| e == "u8");
                    if !ok_elem {
                        return Err("from_utf8 of a non-byte sequence".into());
                    }
                    let pt = self.ptr_ty();
                    let ptr = self.b.ins().load(pt, MemFlags::new(), bv, 0);
                    let len = self.b.ins().load(types::I64, MemFlags::new(), bv, 8);
                    let u = self.rt.utf8;
                    let r = self.call_rt(u, &[ptr, len])[0];
                    let valid = self.b.ins().icmp_imm(IntCC::NotEqual, r, 0);
                    let rty = self.cx.builtin_enum("Result", &["str".into(), "core.str.Utf8Error".into()])?;
                    let l = self.cx.layout(&rty, 0)?;
                    let res = self.alloc(l.size, l.align);
                    let (ok_b, bad_b, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                    self.b.ins().brif(valid, ok_b, &[], bad_b, &[]);
                    self.b.switch_to_block(ok_b);
                    let t0 = self.b.ins().iconst(types::I32, 0);
                    self.b.ins().store(MemFlags::new(), t0, res, 0);
                    let poff = l.variants[0].1[0].1 as i64;
                    let dst = self.b.ins().iadd_imm(res, poff);
                    self.b.ins().store(MemFlags::new(), ptr, dst, 0);
                    self.b.ins().store(MemFlags::new(), len, dst, 8);
                    self.b.ins().jump(done, &[]);
                    self.b.switch_to_block(bad_b);
                    let t1 = self.b.ins().iconst(types::I32, 1);
                    self.b.ins().store(MemFlags::new(), t1, res, 0);
                    self.b.ins().jump(done, &[]);
                    self.b.switch_to_block(done);
                    return Ok((Some(res), rty));
                }
                if segs == ["core", "iter", "fold"] && args.len() == 3 {
                    let pipe = self.parse_pipe(&args[0], env).ok_or("unsupported iterator in fold")?;
                    return self.run_pipe(&pipe, env, Sink::Fold(&args[1], &args[2]));
                }
                if segs == ["core", "iter", "count"] && args.len() == 1 {
                    let pipe = self.parse_pipe(&args[0], env).ok_or("unsupported iterator in count")?;
                    return self.run_pipe(&pipe, env, Sink::Count);
                }
                if segs.len() == 1 && self.closures.contains_key(&segs[0]) {
                    let clo = self.closures[&segs[0]].clone();
                    let ps = items(&nf(&clo)[0]).to_vec();
                    if ps.len() != args.len() {
                        return Err("closure arity".into());
                    }
                    let mut vals = vec![];
                    for (p, a) in ps.iter().zip(args) {
                        let pt = self.cx.pty(&items(p)[1])?;
                        let (v, t) = self.expr(a, env, Some(&pt))?;
                        vals.push((v.ok_or("bad closure argument")?, t));
                    }
                    // 名前の Path で展開する (関数の引数として渡されたクロージャの文脈・取り込み変数を使うため)
                    return self.inline_closure(&f[0], &vals, env);
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
                    // `Result<(), std.io.Error>` の `Ok(())` (標準出力への出力は失敗しない扱い)
                    let rty = self.cx.builtin_enum("Result", &["void".into(), "std.io.Error".into()])?;
                    let l = self.cx.layout(&rty, 0)?;
                    let res = self.alloc(l.size, l.align);
                    let t0 = self.b.ins().iconst(types::I32, 0);
                    self.b.ins().store(MemFlags::new(), t0, res, 0);
                    return Ok((Some(res), rty));
                }
                if segs.len() == 4
                    && segs[0] == "alloc"
                    && segs[1] == "collections"
                    && (segs[2] == "HashMap" || segs[2] == "BtreeMap")
                    && segs[3] == "new"
                {
                    let Some(t) = expect.filter(|t| self.cx.map_info(t).is_some()) else { return Err("map.new without a type".into()) };
                    let v = self.alloc(24, 8);
                    let z = self.b.ins().iconst(types::I64, 0);
                    for off in [0, 8, 16] {
                        self.b.ins().store(MemFlags::new(), z, v, off);
                    }
                    return Ok((Some(v), t.to_string()));
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
                // `VecDeque.new` は Vec と同じ `{ptr, len, cap}` を 0 で初期化する (先頭・末尾・容量を 0 に)。
                if segs == ["alloc", "collections", "VecDeque", "new"] {
                    let Some(t) = expect.filter(|t| self.cx.deque_elem(t).is_some()) else { return Err("VecDeque.new without an element type".into()) };
                    let v = self.alloc(24, 8);
                    let z = self.b.ins().iconst(types::I64, 0);
                    for off in [0, 8, 16] {
                        self.b.ins().store(MemFlags::new(), z, v, off);
                    }
                    return Ok((Some(v), t.to_string()));
                }
                // `std.sync.Mutex.new(value)`: `{T}` の領域に値を入れる (`flag` は持たない)。
                if segs == ["std", "sync", "Mutex", "new"] && args.len() == 1 {
                    let inner = self.ty_of(&args[0], env, None).ok_or("Mutex.new without a value type")?;
                    if inner == "void" {
                        return Err("Mutex of unit".into());
                    }
                    let (v, _) = self.expr(&args[0], env, Some(&inner))?;
                    self.consume(&args[0], &inner, env)?;
                    let t = self.cx.mutex_of(&inner);
                    let l = self.cx.layout(&t, 0)?;
                    let p = self.alloc(l.size, l.align);
                    self.store_value(p, &inner, v.ok_or("bad Mutex value")?)?;
                    return Ok((Some(p), t));
                }
                let fq = segs.join(".");
                let Some(d) = self.funcs.get(&fq).copied() else { return Err(format!("call to {}", fq)) };
                self.call_user(&fq, d, None, args, env, expect)
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
            // `std.sync.Mutex.lock`: 同期実行なので、値のアドレスをそのままガードとして返す。
            if let Some(inner) = self.cx.mutex_inner(rt) {
                if name == "lock" && args.is_empty() {
                    let (p, _) = self.expr(recv, env, None)?;
                    let p = p.ok_or("bad receiver")?;
                    let l = self.cx.layout(rt, 0)?;
                    let off = l.fields[0].2 as i64;
                    let addr = if off == 0 { p } else { self.b.ins().iadd_imm(p, off) };
                    return Ok((Some(addr), self.cx.guard_of(&inner)));
                }
            }
            // `std.sync.MutexGuard.get` / `get_mut`: ガードは値のアドレス。
            if let Some(inner) = self.cx.guard_inner(rt) {
                if (name == "get" || name == "get_mut") && args.is_empty() {
                    let (p, _) = self.expr(recv, env, None)?;
                    let p = p.ok_or("bad receiver")?;
                    if name == "get_mut" {
                        // `&mut スカラー` はアドレス (`mutref<T>`)、構造体などはアドレスをそのまま。
                        return Ok((Some(p), if is_agg(&inner) { inner } else { format!("mutref<{}>", inner) }));
                    }
                    if is_agg(&inner) {
                        return Ok((Some(p), inner));
                    }
                    let v = self.b.ins().load(cl(&inner), MemFlags::new(), p, 0);
                    return Ok((Some(v), inner));
                }
            }
            if name == "clone" && args.is_empty() && self.cx.builtin_method_ty(rt, "clone").is_some() && !self.funcs.contains_key(&format!("{}.clone", rt)) {
                let (v, _) = self.expr(recv, env, None)?;
                let v = v.ok_or("clone of unit")?;
                if is_agg(rt) {
                    let dst = self.alloc_for(rt)?;
                    let size = self.cx.layout(rt, 0)?.size;
                    self.copy_mem(dst, v, size);
                    return Ok((Some(dst), rt.clone()));
                }
                return Ok((Some(v), rt.clone()));
            }
            if self.cx.map_info(rt).is_some() {
                let (p, _) = self.expr(recv, env, None)?;
                let p = p.ok_or("bad receiver")?;
                return self.map_method(p, rt, name, args, env);
            }
            if rt.starts_with("Result<") && name == "map_err" && args.len() == 1 {
                let (p, _) = self.expr(recv, env, None)?;
                let p = p.ok_or("bad receiver")?;
                return self.map_err(recv, p, rt, &args[0], env);
            }
            if rt == "std.rand.Rng" && name == "next_u64" {
                let (p, _) = self.expr(recv, env, None)?;
                let p = p.ok_or("bad receiver")?;
                let st0 = self.b.ins().load(types::I64, MemFlags::new(), p, 0);
                let (ns, z) = self.splitmix(st0);
                self.b.ins().store(MemFlags::new(), ns, p, 0);
                return Ok((Some(z), "u64".into()));
            }
            if rt.starts_with("joinhandle<") && name == "join" && args.is_empty() {
                let (p, _) = self.expr(recv, env, None)?;
                let p = p.ok_or("bad receiver")?;
                self.consume(recv, rt, env)?;
                let l = self.cx.layout(rt, 0)?;
                let (fty, off) = (l.fields[0].1.clone(), l.fields[0].2);
                let addr = self.b.ins().iadd_imm(p, off as i64);
                let v = self.load_value(addr, &fty);
                return Ok((Some(v), fty));
            }
            if rt.starts_with("Option<") && name == "take" && args.is_empty() {
                let (p, _) = self.expr(recv, env, None)?;
                let p = p.ok_or("bad receiver")?;
                let size = self.cx.layout(rt, 0)?.size;
                let res = self.alloc_for(rt)?;
                self.copy_mem(res, p, size);
                let t0 = self.b.ins().iconst(types::I32, 0);
                self.b.ins().store(MemFlags::new(), t0, p, 0);
                return Ok((Some(res), rt.clone()));
            }
            if rt.starts_with("Option<") || rt.starts_with("Result<") {
                if self.cx.builtin_method_ty(rt, name).is_some() {
                    return self.option_method(recv, rt, name, args, env);
                }
            }
            if rt == "str" || rt == STRING || self.cx.seq_elem(rt).is_some() || self.cx.deque_elem(rt).is_some() {
                let (p, _) = self.expr(recv, env, None)?;
                let p = p.ok_or("bad receiver")?;
                return self.builtin_method(p, rt, name, args, env);
            }
            if is_agg(rt) {
                let fq = format!("{}.{}", base_of(rt), name);
                let d = self.funcs.get(&fq).copied().ok_or_else(|| format!("method {}", fq))?;
                if !d.is_method() {
                    return Err(format!("{} is not a method", fq));
                }
                let (p, rty2) = self.expr(recv, env, None)?;
                if tag(&d.params[0].1) != "ref" {
                    self.consume(recv, &rty2, env)?;
                }
                return self.call_user(&fq, d, Some((p.ok_or("bad receiver")?, rty2.clone())), args, env, expect);
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
                if !is_scalar(elem) && (name == "get_mut" || self.cx.needs_drop(elem)) {
                    return Err(format!("{} on this kind of aggregate elements", name));
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
                let poff = l.variants[1].1[0].1;
                let dst = self.b.ins().iadd_imm(res, poff as i64);
                if is_agg(elem) {
                    // 解放が要らない構造体・列挙型は、値のコピーを `Some` の中に置く (読むだけの参照の代わり)。
                    self.store_value(dst, elem, addr)?;
                } else {
                    let val = if by_addr { addr } else { self.b.ins().load(cl(elem), MemFlags::new(), addr, 0) };
                    self.b.ins().store(MemFlags::new(), val, dst, 0);
                }
                let t1 = self.b.ins().iconst(types::I32, 1);
                self.b.ins().store(MemFlags::new(), t1, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
                Ok(Some((Some(res), rty)))
            }
            "contains" => {
                if !is_scalar(elem) || is_float(elem) {
                    return Err("contains on this element type".into());
                }
                let (x, _) = self.expr(args.first().ok_or("contains needs a value")?, env, Some(elem))?;
                let x = x.ok_or("bad value")?;
                let base = self.b.ins().load(pt, MemFlags::new(), p, 0);
                let (head, body, cont, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.append_block_param(head, i64t);
                self.b.append_block_param(done, types::I8);
                let zero = self.b.ins().iconst(i64t, 0);
                self.b.ins().jump(head, &[zero]);
                self.b.switch_to_block(head);
                let i = self.b.block_params(head)[0];
                let more = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
                let no = self.b.ins().iconst(types::I8, 0);
                self.b.ins().brif(more, body, &[], done, &[no]);
                self.b.switch_to_block(body);
                let off = self.b.ins().imul_imm(i, sz as i64);
                let addr = self.b.ins().iadd(base, off);
                let v = self.b.ins().load(cl(elem), MemFlags::new(), addr, 0);
                let eq = self.b.ins().icmp(IntCC::Equal, v, x);
                let yes = self.b.ins().iconst(types::I8, 1);
                self.b.ins().brif(eq, done, &[yes], cont, &[]);
                self.b.switch_to_block(cont);
                let n = self.b.ins().iadd_imm(i, 1);
                self.b.ins().jump(head, &[n]);
                self.b.switch_to_block(done);
                Ok(Some((Some(self.b.block_params(done)[0]), "bool".into())))
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
            "push" => {
                let arg = args.first().ok_or("push needs a char")?;
                let (cv, ct) = self.expr(arg, env, Some("char"))?;
                if ct != "char" {
                    return Err("String.push of a non-char".into());
                }
                let c = cv.ok_or("bad char")?;
                // UTF-8 に符号化して 4 バイトの領域に置き、n バイト足す。
                let i32t = types::I32;
                let lt80 = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, c, 0x80);
                let lt800 = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, c, 0x800);
                let lt10000 = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, c, 0x10000);
                let (one, two, three, four) = (
                    self.b.ins().iconst(i64t, 1),
                    self.b.ins().iconst(i64t, 2),
                    self.b.ins().iconst(i64t, 3),
                    self.b.ins().iconst(i64t, 4),
                );
                let s34 = self.b.ins().select(lt10000, three, four);
                let s234 = self.b.ins().select(lt800, two, s34);
                let n = self.b.ins().select(lt80, one, s234);
                let sh6 = self.b.ins().ushr_imm(c, 6);
                let sh12 = self.b.ins().ushr_imm(c, 12);
                let sh18 = self.b.ins().ushr_imm(c, 18);
                let low6 = self.b.ins().band_imm(c, 0x3f);
                let mid6 = self.b.ins().band_imm(sh6, 0x3f);
                let hi6 = self.b.ins().band_imm(sh12, 0x3f);
                let b0_2 = self.b.ins().bor_imm(sh6, 0xC0);
                let b0_3 = self.b.ins().bor_imm(sh12, 0xE0);
                let b0_4 = self.b.ins().bor_imm(sh18, 0xF0);
                let t34 = self.b.ins().select(lt10000, b0_3, b0_4);
                let t234 = self.b.ins().select(lt800, b0_2, t34);
                let b0 = self.b.ins().select(lt80, c, t234);
                let low_c = self.b.ins().bor_imm(low6, 0x80);
                let mid_c = self.b.ins().bor_imm(mid6, 0x80);
                let hi_c = self.b.ins().bor_imm(hi6, 0x80);
                let r34 = self.b.ins().select(lt10000, mid_c, hi_c);
                let b1 = self.b.ins().select(lt800, low_c, r34);
                let b2 = self.b.ins().select(lt10000, low_c, mid_c);
                let b3 = low_c;
                let slot = self.alloc(4, 1);
                for (k, b) in [b0, b1, b2, b3].iter().enumerate() {
                    let b8 = self.b.ins().ireduce(types::I8, *b);
                    self.b.ins().store(MemFlags::new(), b8, slot, k as i32);
                }
                let _ = i32t;
                let failed = self.string_append(p, slot, n)?;
                let (res, rty) = self.alloc_result(failed)?;
                return Ok((Some(res), rty));
            }
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
        if let Some(elem) = self.cx.seq_elem(ty) {
            // 要素が整数などのスカラーの列は、長さが同じで、バイト列が同じなら等しい。
            if !is_scalar(&elem) || is_float(&elem) || is_mutref(&elem) {
                return Err("equality of sequences of this element type".into());
            }
            let sz = scalar_size(&elem) as i64;
            return Ok(self.bytes_eq(a, b, sz));
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

    /// `r.map_err(f)` の結果の型 (`f` は関数の名前かクロージャ)。
    fn map_err_ty(&self, recv_ty: &str, f: &V, env: &Env) -> Option<String> {
        if !recv_ty.starts_with("Result<") {
            return None;
        }
        let l = self.cx.layout(recv_ty, 0).ok()?;
        let ok_ty = l.variants.first()?.1.first().map(|x| x.0.clone()).unwrap_or_else(|| "void".to_string());
        let new_err = if nname(f) == "Closure" {
            self.cx.pty(&nf(f)[1]).ok()?
        } else if nname(f) == "Path" && !env.contains_key(&strs(&nf(f)[0])[0]) {
            let q = self.cx.qual_segs(&strs(&nf(f)[0])).join(".");
            let d = self.funcs.get(&q).copied()?;
            self.cx.pty_for(d, &d.ret).ok()?
        } else {
            return None;
        };
        self.cx.builtin_enum("Result", &[ok_ty, new_err]).ok()
    }

    /// 関数 `d` を、評価済みの値で呼ぶ。
    fn call_fn_values(&mut self, fq: &str, d: &'a Def, vals: &[Value]) -> R<(Option<Value>, String)> {
        let id = self.func_id(d, fq, &HashMap::new())?;
        let rt = self.cx.pty_for(d, &d.ret)?;
        let mut all = vec![];
        let mut out = None;
        if is_agg(&rt) {
            let p = self.alloc_for(&rt)?;
            all.push(p);
            out = Some(p);
        }
        all.extend_from_slice(vals);
        let res = self.call_rt(id, &all);
        Ok((out.or(res.first().copied()), rt))
    }

    /// `r.map_err(f)`: `Err(e)` を `Err(f(e))` に変える。`Ok` はそのまま。
    fn map_err(&mut self, recv: &V, p: Value, rt: &str, f: &V, env: &mut Env) -> R<(Option<Value>, String)> {
        let newty = self.map_err_ty(rt, f, env).ok_or("map_err with an unsupported function")?;
        let l = self.cx.layout(rt, 0)?;
        let nl = self.cx.layout(&newty, 0)?;
        let ok_pay = l.variants[0].1.first().cloned();
        let (err_ty, err_off) = l.variants[1].1.first().cloned().ok_or("map_err on an empty error")?;
        if self.cx.needs_drop(&err_ty) {
            return Err("map_err on a droppable error".into());
        }
        self.consume(recv, rt, env)?;
        let res = self.alloc(nl.size, nl.align);
        let tg = self.b.ins().load(types::I32, MemFlags::new(), p, 0);
        let is_ok = self.b.ins().icmp_imm(IntCC::Equal, tg, 0);
        let (ok_b, err_b, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.ins().brif(is_ok, ok_b, &[], err_b, &[]);
        self.b.switch_to_block(ok_b);
        let t0 = self.b.ins().iconst(types::I32, 0);
        self.b.ins().store(MemFlags::new(), t0, res, 0);
        if let Some((oty, ooff)) = ok_pay {
            let src = self.b.ins().iadd_imm(p, ooff as i64);
            let v = self.load_value(src, &oty);
            let noff = nl.variants[0].1[0].1;
            let dst = self.b.ins().iadd_imm(res, noff as i64);
            self.store_value(dst, &oty, v)?;
        }
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(err_b);
        let esrc = self.b.ins().iadd_imm(p, err_off as i64);
        let ev = self.load_value(esrc, &err_ty);
        let (fv, _) = if nname(f) == "Closure" {
            self.inline_closure(f, &[(ev, err_ty.clone())], env)?
        } else {
            let q = self.cx.qual_segs(&strs(&nf(f)[0])).join(".");
            let d = self.funcs.get(&q).copied().ok_or("map_err function not found")?;
            self.call_fn_values(&q, d, &[ev])?
        };
        let (nety, noff) = nl.variants[1].1.first().cloned().ok_or("map_err result type")?;
        let dst = self.b.ins().iadd_imm(res, noff as i64);
        self.store_value(dst, &nety, fv.ok_or("map_err function returns unit")?)?;
        let t1 = self.b.ins().iconst(types::I32, 1);
        self.b.ins().store(MemFlags::new(), t1, res, 0);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        Ok((Some(res), newty))
    }

    /// splitmix64: (次の状態, 出力)。インタプリタの `splitmix` と同じ計算。
    fn splitmix(&mut self, state: Value) -> (Value, Value) {
        let s = self.b.ins().iadd_imm(state, 0x9E3779B97F4A7C15u64 as i64);
        let a = self.b.ins().ushr_imm(s, 30);
        let x = self.b.ins().bxor(s, a);
        let z1 = self.b.ins().imul_imm(x, 0xBF58476D1CE4E5B9u64 as i64);
        let b = self.b.ins().ushr_imm(z1, 27);
        let y = self.b.ins().bxor(z1, b);
        let z2 = self.b.ins().imul_imm(y, 0x94D049BB133111EBu64 as i64);
        let c = self.b.ins().ushr_imm(z2, 31);
        let out = self.b.ins().bxor(z2, c);
        (s, out)
    }

    /// 満杯なら列を広げて、1 つ入る余地を作る。(失敗したか, 先頭のアドレス) を返す。
    fn grow_for_one(&mut self, p: Value, stride: i64) -> (Value, Value) {
        let i64t = types::I64;
        let pt = self.ptr_ty();
        let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
        let cap = self.b.ins().load(i64t, MemFlags::new(), p, 16);
        let ptr0 = self.b.ins().load(pt, MemFlags::new(), p, 0);
        let full = self.b.ins().icmp(IntCC::Equal, len, cap);
        let (grow, ok_b, set_b, fail_b, done) =
            (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.append_block_param(done, types::I8);
        self.b.append_block_param(done, pt);
        self.b.ins().brif(full, grow, &[], ok_b, &[]);
        self.b.switch_to_block(ok_b);
        let zero = self.b.ins().iconst(types::I8, 0);
        self.b.ins().jump(done, &[zero, ptr0]);
        self.b.switch_to_block(grow);
        let four = self.b.ins().iconst(i64t, 4);
        let dbl = self.b.ins().ishl_imm(cap, 1);
        let cap_zero = self.b.ins().icmp_imm(IntCC::Equal, cap, 0);
        let newcap = self.b.ins().select(cap_zero, four, dbl);
        let bytes = self.b.ins().imul_imm(newcap, stride);
        let ra = self.rt.realloc;
        let np = self.call_rt(ra, &[ptr0, bytes])[0];
        let isnull = self.b.ins().icmp_imm(IntCC::Equal, np, 0);
        self.b.ins().brif(isnull, fail_b, &[], set_b, &[]);
        self.b.switch_to_block(set_b);
        self.b.ins().store(MemFlags::new(), np, p, 0);
        self.b.ins().store(MemFlags::new(), newcap, p, 16);
        let zero = self.b.ins().iconst(types::I8, 0);
        self.b.ins().jump(done, &[zero, np]);
        self.b.switch_to_block(fail_b);
        let one = self.b.ins().iconst(types::I8, 1);
        self.b.ins().jump(done, &[one, ptr0]);
        self.b.switch_to_block(done);
        (self.b.block_params(done)[0], self.b.block_params(done)[1])
    }

    /// 表 `p` から、キー `key` を持つ組を探す。(見つかったか, 添字) を返す。
    fn map_find(&mut self, p: Value, kty: &str, key: Value, koff: i64, stride: i64) -> (Value, Value) {
        let i64t = types::I64;
        let pt = self.ptr_ty();
        let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
        let base = self.b.ins().load(pt, MemFlags::new(), p, 0);
        let (head, body, cont, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.append_block_param(head, i64t);
        self.b.append_block_param(done, types::I8);
        self.b.append_block_param(done, i64t);
        let zero = self.b.ins().iconst(i64t, 0);
        self.b.ins().jump(head, &[zero]);
        self.b.switch_to_block(head);
        let i = self.b.block_params(head)[0];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
        let no = self.b.ins().iconst(types::I8, 0);
        self.b.ins().brif(more, body, &[], done, &[no, i]);
        self.b.switch_to_block(body);
        let off = self.b.ins().imul_imm(i, stride);
        let ea = self.b.ins().iadd(base, off);
        let ka = self.b.ins().iadd_imm(ea, koff);
        let eq = if kty == "str" {
            self.bytes_eq(ka, key, 1)
        } else {
            let sk = self.b.ins().load(cl(kty), MemFlags::new(), ka, 0);
            self.b.ins().icmp(IntCC::Equal, sk, key)
        };
        let yes = self.b.ins().iconst(types::I8, 1);
        self.b.ins().brif(eq, done, &[yes, i], cont, &[]);
        self.b.switch_to_block(cont);
        let n = self.b.ins().iadd_imm(i, 1);
        self.b.ins().jump(head, &[n]);
        self.b.switch_to_block(done);
        (self.b.block_params(done)[0], self.b.block_params(done)[1])
    }

    /// `HashMap` / `BtreeMap` のメソッド。実体は組の列で、検索は線形 (`BtreeMap` は挿入のときにキー順を保つ)。
    fn map_method(&mut self, p: Value, rt: &str, name: &str, args: &[V], env: &mut Env) -> R<(Option<Value>, String)> {
        let i64t = types::I64;
        let pt = self.ptr_ty();
        let (sorted, kty, vty) = self.cx.map_info(rt).ok_or("not a map")?;
        let tup = self.cx.tuple_of(&[kty.clone(), vty.clone()]);
        let el = self.cx.layout(&tup, 0)?;
        let (koff, voff, stride) = (el.fields[0].2 as i64, el.fields[1].2 as i64, el.size as i64);
        match name {
            "len" => {
                let v = self.b.ins().load(i64t, MemFlags::new(), p, 8);
                return Ok((Some(v), "usize".into()));
            }
            "is_empty" => {
                let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
                let r = self.b.ins().icmp_imm(IntCC::Equal, len, 0);
                return Ok((Some(r), "bool".into()));
            }
            "clear" => {
                let z = self.b.ins().iconst(i64t, 0);
                self.b.ins().store(MemFlags::new(), z, p, 8);
                return Ok((None, "void".into()));
            }
            "contains_key" | "get" | "remove" | "get_mut" | "insert" => {}
            _ => return Err(format!("map method {}", name)),
        }
        let key_arg = args.first().ok_or("map method needs a key")?;
        let (key, kt) = self.expr(key_arg, env, Some(&kty))?;
        if kt != kty {
            return Err(format!("map key type mismatch: {} vs {}", kt, kty));
        }
        let key = key.ok_or("bad key")?;
        let (found, idx) = self.map_find(p, &kty, key, koff, stride);
        match name {
            "contains_key" => Ok((Some(found), "bool".into())),
            "get" | "remove" | "get_mut" => {
                // `get_mut` は `Option<&mut V>` を返す。値がスカラーのときだけ、そのアドレスを持ち回る。
                if name == "get_mut" && !(is_scalar(&vty) && !is_mutref(&vty)) {
                    return Err("get_mut on this map value type".into());
                }
                let pay_ty = if name == "get_mut" { format!("mutref<{}>", vty) } else { vty.clone() };
                let rty = self.cx.builtin_enum("Option", &[pay_ty])?;
                let l = self.cx.layout(&rty, 0)?;
                let res = self.alloc(l.size, l.align);
                let (yes, no, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().brif(found, yes, &[], no, &[]);
                self.b.switch_to_block(no);
                let t0 = self.b.ins().iconst(types::I32, 0);
                self.b.ins().store(MemFlags::new(), t0, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(yes);
                let base = self.b.ins().load(pt, MemFlags::new(), p, 0);
                let off = self.b.ins().imul_imm(idx, stride);
                let ea = self.b.ins().iadd(base, off);
                let va = self.b.ins().iadd_imm(ea, voff);
                let poff = l.variants[1].1[0].1 as i64;
                let dst = self.b.ins().iadd_imm(res, poff);
                if name == "get_mut" {
                    self.b.ins().store(MemFlags::new(), va, dst, 0);
                } else {
                    let v = self.load_value(va, &vty);
                    self.store_value(dst, &vty, v)?;
                }
                if name == "remove" {
                    let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
                    let after = self.b.ins().isub(len, idx);
                    let tail_n = self.b.ins().iadd_imm(after, -1);
                    let bytes = self.b.ins().imul_imm(tail_n, stride);
                    let src = self.b.ins().iadd_imm(ea, stride);
                    let mm = self.rt.memmove;
                    self.call_rt(mm, &[ea, src, bytes]);
                    let newlen = self.b.ins().iadd_imm(len, -1);
                    self.b.ins().store(MemFlags::new(), newlen, p, 8);
                }
                let t1 = self.b.ins().iconst(types::I32, 1);
                self.b.ins().store(MemFlags::new(), t1, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
                Ok((Some(res), rty))
            }
            _ => {
                // insert(key, value) -> Result<Option<V>, AllocError>
                let varg = args.get(1).ok_or("insert needs a value")?;
                let (val, _) = self.expr(varg, env, Some(&vty))?;
                let val = val.ok_or("bad value")?;
                let optty = self.cx.builtin_enum("Option", &[vty.clone()])?;
                let ol = self.cx.layout(&optty, 0)?;
                let rty = self.cx.builtin_enum("Result", &[optty.clone(), "alloc.alloc.AllocError".into()])?;
                let rl = self.cx.layout(&rty, 0)?;
                let res = self.alloc(rl.size, rl.align);
                let tmp = self.alloc(ol.size, ol.align);
                let (upd, add, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().brif(found, upd, &[], add, &[]);
                // 既にあるキー: 値を入れ替えて、古い値を返す
                self.b.switch_to_block(upd);
                let base = self.b.ins().load(pt, MemFlags::new(), p, 0);
                let off = self.b.ins().imul_imm(idx, stride);
                let ea = self.b.ins().iadd(base, off);
                let va = self.b.ins().iadd_imm(ea, voff);
                let old = self.load_value(va, &vty);
                let opoff = ol.variants[1].1[0].1 as i64;
                let od = self.b.ins().iadd_imm(tmp, opoff);
                self.store_value(od, &vty, old)?;
                let t1 = self.b.ins().iconst(types::I32, 1);
                self.b.ins().store(MemFlags::new(), t1, tmp, 0);
                self.store_value(va, &vty, val)?;
                let okoff = rl.variants[0].1[0].1 as i64;
                let rd = self.b.ins().iadd_imm(res, okoff);
                self.store_value(rd, &optty, tmp)?;
                let t0 = self.b.ins().iconst(types::I32, 0);
                self.b.ins().store(MemFlags::new(), t0, res, 0);
                self.b.ins().jump(done, &[]);
                // 新しいキー: 余地を作って、挿入位置に入れる
                self.b.switch_to_block(add);
                let (failed, nbase) = self.grow_for_one(p, stride);
                let (ins_b, err_b) = (self.b.create_block(), self.b.create_block());
                self.b.ins().brif(failed, err_b, &[], ins_b, &[]);
                self.b.switch_to_block(err_b);
                let e1 = self.b.ins().iconst(types::I32, 1);
                self.b.ins().store(MemFlags::new(), e1, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(ins_b);
                let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
                let pos = if sorted {
                    // キーが自分より大きい最初の位置 (なければ末尾)
                    let (head, body, cont, found_pos) =
                        (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
                    self.b.append_block_param(head, i64t);
                    self.b.append_block_param(found_pos, i64t);
                    let zero = self.b.ins().iconst(i64t, 0);
                    self.b.ins().jump(head, &[zero]);
                    self.b.switch_to_block(head);
                    let i = self.b.block_params(head)[0];
                    let more = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
                    self.b.ins().brif(more, body, &[], found_pos, &[len]);
                    self.b.switch_to_block(body);
                    let o = self.b.ins().imul_imm(i, stride);
                    let a = self.b.ins().iadd(nbase, o);
                    let ka = self.b.ins().iadd_imm(a, koff);
                    let sk = self.b.ins().load(cl(&kty), MemFlags::new(), ka, 0);
                    let cc = if signed(&kty) { IntCC::SignedGreaterThan } else { IntCC::UnsignedGreaterThan };
                    let bigger = self.b.ins().icmp(cc, sk, key);
                    self.b.ins().brif(bigger, found_pos, &[i], cont, &[]);
                    self.b.switch_to_block(cont);
                    let n = self.b.ins().iadd_imm(i, 1);
                    self.b.ins().jump(head, &[n]);
                    self.b.switch_to_block(found_pos);
                    self.b.block_params(found_pos)[0]
                } else {
                    len
                };
                let o = self.b.ins().imul_imm(pos, stride);
                let slot = self.b.ins().iadd(nbase, o);
                let rest = self.b.ins().isub(len, pos);
                let bytes = self.b.ins().imul_imm(rest, stride);
                let dst = self.b.ins().iadd_imm(slot, stride);
                let mm = self.rt.memmove;
                self.call_rt(mm, &[dst, slot, bytes]);
                let kd = self.b.ins().iadd_imm(slot, koff);
                self.store_value(kd, &kty, key)?;
                let vd = self.b.ins().iadd_imm(slot, voff);
                self.store_value(vd, &vty, val)?;
                let newlen = self.b.ins().iadd_imm(len, 1);
                self.b.ins().store(MemFlags::new(), newlen, p, 8);
                let n0 = self.b.ins().iconst(types::I32, 0);
                self.b.ins().store(MemFlags::new(), n0, tmp, 0);
                let okoff = rl.variants[0].1[0].1 as i64;
                let rd = self.b.ins().iadd_imm(res, okoff);
                self.store_value(rd, &optty, tmp)?;
                let ok0 = self.b.ins().iconst(types::I32, 0);
                self.b.ins().store(MemFlags::new(), ok0, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
                Ok((Some(res), rty))
            }
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
        self.bytes_eq(a, b, 1)
    }

    /// 先頭の 2 語が {ptr, len} の 2 つの列 (要素 1 つが `sz` バイト) が、長さもバイト列も同じなら真 (I8)。
    fn bytes_eq(&mut self, a: Value, b: Value, sz: i64) -> Value {
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
        let nbytes = if sz == 1 { la } else { self.b.ins().imul_imm(la, sz) };
        let r = self.call_rt(mc, &[pa, pb, nbytes])[0];
        let eq = self.b.ins().icmp_imm(IntCC::Equal, r, 0);
        self.b.ins().jump(done, &[eq]);
        self.b.switch_to_block(done);
        self.b.block_params(done)[0]
    }

    /// 整数 `v` を型 `from` から `to` に変える (広げるときは符号つきなら符号拡張、縮めるときは下位を残す)。
    fn int_cast(&mut self, v: Value, from: &str, to: &str) -> Value {
        let (fb, tb) = (cl(from).bits(), cl(to).bits());
        if tb > fb {
            if signed(from) { self.b.ins().sextend(cl(to), v) } else { self.b.ins().uextend(cl(to), v) }
        } else if tb < fb {
            self.b.ins().ireduce(cl(to), v)
        } else {
            v
        }
    }

    /// `P.from_S(x)` / `P.try_from_S(x)` / `char.try_from_u32(x)` (P は整数)。変換でなければ None。
    fn conv_call(&mut self, segs: &[String], args: &[V], env: &mut Env) -> R<Option<(Option<Value>, String)>> {
        if let [p, n] | [_, _, p, n] = segs {
            if n == "from_str_radix" && is_int(p) && args.len() == 2 && (segs.len() == 2 || (segs[0] == "core" && segs[1] == "num")) {
                return self.from_str_radix(p, args, env).map(Some);
            }
        }
        let (p, name) = match segs {
            [p, n] => (p.clone(), n.clone()),
            [c, n, p, nm] if c == "core" && n == "num" => (p.clone(), nm.clone()),
            [c, n, p, nm] if c == "core" && n == "char" && p == "char" => ("char".to_string(), nm.clone()),
            _ => return Ok(None),
        };
        if args.len() != 1 {
            return Ok(None);
        }
        if p == "char" && name == "try_from_u32" {
            let (x, _) = self.expr(&args[0], env, Some("u32"))?;
            let x = x.ok_or("bad value")?;
            let rty = self.cx.builtin_enum("Option", &["char".into()])?;
            let l = self.cx.layout(&rty, 0)?;
            let res = self.alloc(l.size, l.align);
            let lt_d800 = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, x, 0xD800);
            let t = self.b.ins().iadd_imm(x, -0xE000);
            let hi_ok = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, t, 0x10FFFF - 0xE000);
            let valid = self.b.ins().bor(lt_d800, hi_ok);
            let (yes, no, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
            self.b.ins().brif(valid, yes, &[], no, &[]);
            self.b.switch_to_block(no);
            let t0 = self.b.ins().iconst(types::I32, 0);
            self.b.ins().store(MemFlags::new(), t0, res, 0);
            self.b.ins().jump(done, &[]);
            self.b.switch_to_block(yes);
            let poff = l.variants[1].1[0].1 as i64;
            let dst = self.b.ins().iadd_imm(res, poff);
            self.b.ins().store(MemFlags::new(), x, dst, 0);
            let t1 = self.b.ins().iconst(types::I32, 1);
            self.b.ins().store(MemFlags::new(), t1, res, 0);
            self.b.ins().jump(done, &[]);
            self.b.switch_to_block(done);
            return Ok(Some((Some(res), rty)));
        }
        if !is_int(&p) {
            return Ok(None);
        }
        if let Some(src) = name.strip_prefix("from_") {
            if !is_int(src) || (cl(src).bits() > cl(&p).bits()) {
                return Ok(None);
            }
            let (x, _) = self.expr(&args[0], env, Some(src))?;
            let r = self.int_cast(x.ok_or("bad value")?, src, &p);
            return Ok(Some((Some(r), p)));
        }
        if let Some(src) = name.strip_prefix("try_from_") {
            if !is_int(src) {
                return Ok(None);
            }
            let (x, _) = self.expr(&args[0], env, Some(src))?;
            let x = x.ok_or("bad value")?;
            let rty = self.cx.builtin_enum("Result", &[p.clone(), "core.num.TryFromIntError".into()])?;
            let l = self.cx.layout(&rty, 0)?;
            let res = self.alloc(l.size, l.align);
            // 値の範囲の検査は 64 ビットで行う (符号なしは u64、符号つきは i64)。
            let (pb, sb) = (cl(&p).bits() as u32, cl(src).bits() as u32);
            let ok = if !signed(src) {
                let wide = self.int_cast(x, src, "u64");
                if signed(&p) {
                    let max: u64 = if pb >= 64 { i64::MAX as u64 } else { (1u64 << (pb - 1)) - 1 };
                    self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, wide, max as i64)
                } else if pb >= 64 {
                    self.b.ins().iconst(types::I8, 1)
                } else {
                    let max: u64 = (1u64 << pb) - 1;
                    self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, wide, max as i64)
                }
            } else {
                let _ = sb;
                let wide = self.int_cast(x, src, "i64");
                if signed(&p) {
                    if pb >= 64 {
                        self.b.ins().iconst(types::I8, 1)
                    } else {
                        let max: i64 = (1i64 << (pb - 1)) - 1;
                        let min: i64 = -(1i64 << (pb - 1));
                        let ge = self.b.ins().icmp_imm(IntCC::SignedGreaterThanOrEqual, wide, min);
                        let le = self.b.ins().icmp_imm(IntCC::SignedLessThanOrEqual, wide, max);
                        self.b.ins().band(ge, le)
                    }
                } else {
                    let nonneg = self.b.ins().icmp_imm(IntCC::SignedGreaterThanOrEqual, wide, 0);
                    if pb >= 64 {
                        nonneg
                    } else {
                        let max: i64 = ((1u64 << pb) - 1) as i64;
                        let le = self.b.ins().icmp_imm(IntCC::SignedLessThanOrEqual, wide, max);
                        self.b.ins().band(nonneg, le)
                    }
                }
            };
            let (yes, no, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
            self.b.ins().brif(ok, yes, &[], no, &[]);
            self.b.switch_to_block(no);
            let t1 = self.b.ins().iconst(types::I32, 1);
            self.b.ins().store(MemFlags::new(), t1, res, 0);
            self.b.ins().jump(done, &[]);
            self.b.switch_to_block(yes);
            let v = self.int_cast(x, src, &p);
            let poff = l.variants[0].1[0].1 as i64;
            let dst = self.b.ins().iadd_imm(res, poff);
            self.b.ins().store(MemFlags::new(), v, dst, 0);
            let t0 = self.b.ins().iconst(types::I32, 0);
            self.b.ins().store(MemFlags::new(), t0, res, 0);
            self.b.ins().jump(done, &[]);
            self.b.switch_to_block(done);
            return Ok(Some((Some(res), rty)));
        }
        Ok(None)
    }

    /// `P.from_str_radix(text, radix)`: 文字列を `radix` 進数の整数として解析する (符号つきの型は先頭の `-` / `+`、符号なしは `+`)。
    fn from_str_radix(&mut self, p: &str, args: &[V], env: &mut Env) -> R<(Option<Value>, String)> {
        let i64t = types::I64;
        let pt = self.ptr_ty();
        let bits = cl(p).bits() as u32;
        let sg = signed(p);
        let (sv, st_) = self.expr(&args[0], env, Some("str"))?;
        if st_ != "str" {
            return Err("from_str_radix of a non-string".into());
        }
        let sv = sv.ok_or("bad string")?;
        let (rv, _) = self.expr(&args[1], env, Some("u32"))?;
        let rv = rv.ok_or("bad radix")?;
        let radix = self.b.ins().uextend(i64t, rv);
        let rty = self.cx.builtin_enum("Result", &[p.to_string(), "core.num.ParseIntError".into()])?;
        let l = self.cx.layout(&rty, 0)?;
        let res = self.alloc(l.size, l.align);
        let base = self.b.ins().load(pt, MemFlags::new(), sv, 0);
        let len = self.b.ins().load(i64t, MemFlags::new(), sv, 8);
        let (err_b, done) = (self.b.create_block(), self.b.create_block());
        // 空なら Err
        let (nonempty, sign_b) = (self.b.create_block(), self.b.create_block());
        let empty = self.b.ins().icmp_imm(IntCC::Equal, len, 0);
        self.b.ins().brif(empty, err_b, &[], nonempty, &[]);
        self.b.switch_to_block(nonempty);
        let c0 = {
            let v = self.b.ins().load(types::I8, MemFlags::new(), base, 0);
            self.b.ins().uextend(types::I32, v)
        };
        let is_minus = self.b.ins().icmp_imm(IntCC::Equal, c0, 0x2D);
        let is_plus = self.b.ins().icmp_imm(IntCC::Equal, c0, 0x2B);
        let neg = if sg { is_minus } else { self.b.ins().iconst(types::I8, 0) };
        let has_sign = self.b.ins().bor(neg, is_plus);
        let one = self.b.ins().iconst(i64t, 1);
        let zero = self.b.ins().iconst(i64t, 0);
        let start = self.b.ins().select(has_sign, one, zero);
        // 符号だけなら Err
        let only_sign = self.b.ins().icmp(IntCC::Equal, start, len);
        self.b.ins().brif(only_sign, err_b, &[], sign_b, &[]);
        self.b.switch_to_block(sign_b);
        let (head, body, cont, fin) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.append_block_param(head, i64t);
        self.b.append_block_param(head, i64t);
        self.b.ins().jump(head, &[start, zero]);
        self.b.switch_to_block(head);
        let i = self.b.block_params(head)[0];
        let acc = self.b.block_params(head)[1];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
        self.b.ins().brif(more, body, &[], fin, &[]);
        self.b.switch_to_block(body);
        let at = self.b.ins().iadd(base, i);
        let cb = self.b.ins().load(types::I8, MemFlags::new(), at, 0);
        let c = self.b.ins().uextend(types::I32, cb);
        let t0 = self.b.ins().iadd_imm(c, -48);
        let is_num = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, t0, 9);
        let lower = self.b.ins().bor_imm(c, 0x20);
        let t1 = self.b.ins().iadd_imm(lower, -97);
        let is_alpha = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, t1, 25);
        let t1p = self.b.ins().iadd_imm(t1, 10);
        let bad = self.b.ins().iconst(types::I32, 255);
        let d_alpha = self.b.ins().select(is_alpha, t1p, bad);
        let d32 = self.b.ins().select(is_num, t0, d_alpha);
        let d = self.b.ins().uextend(i64t, d32);
        let valid = self.b.ins().icmp(IntCC::UnsignedLessThan, d, radix);
        let (mul_b, _) = (self.b.create_block(), 0);
        self.b.ins().brif(valid, mul_b, &[], err_b, &[]);
        self.b.switch_to_block(mul_b);
        let (m, of1) = self.b.ins().umul_overflow(acc, radix);
        let add_b = self.b.create_block();
        self.b.ins().brif(of1, err_b, &[], add_b, &[]);
        self.b.switch_to_block(add_b);
        let (a2, of2) = self.b.ins().uadd_overflow(m, d);
        self.b.ins().brif(of2, err_b, &[], cont, &[]);
        self.b.switch_to_block(cont);
        let n = self.b.ins().iadd_imm(i, 1);
        self.b.ins().jump(head, &[n, a2]);
        // 範囲の検査と、符号
        self.b.switch_to_block(fin);
        let ok_range = if sg {
            let half: u64 = 1u64 << (bits - 1);
            // 負なら |値| <= 2^(bits-1)、正なら <= 2^(bits-1) - 1
            let le_neg = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, acc, half as i64);
            let le_pos = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, acc, (half - 1) as i64);
            self.b.ins().select(neg, le_neg, le_pos)
        } else if bits >= 64 {
            self.b.ins().iconst(types::I8, 1)
        } else {
            let max: u64 = (1u64 << bits) - 1;
            self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, acc, max as i64)
        };
        let ok_b = self.b.create_block();
        self.b.ins().brif(ok_range, ok_b, &[], err_b, &[]);
        self.b.switch_to_block(ok_b);
        let negated = self.b.ins().ineg(acc);
        let val64 = if sg { self.b.ins().select(neg, negated, acc) } else { acc };
        let val = if bits < 64 { self.b.ins().ireduce(cl(p), val64) } else { val64 };
        let poff = l.variants[0].1[0].1 as i64;
        let dst = self.b.ins().iadd_imm(res, poff);
        self.b.ins().store(MemFlags::new(), val, dst, 0);
        let t0 = self.b.ins().iconst(types::I32, 0);
        self.b.ins().store(MemFlags::new(), t0, res, 0);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(err_b);
        let t1 = self.b.ins().iconst(types::I32, 1);
        self.b.ins().store(MemFlags::new(), t1, res, 0);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        Ok((Some(res), rty))
    }

    /// `char` と、整数の浮動小数への変換のメソッド。
    fn scalar_method(&mut self, recv: &V, name: &str, env: &mut Env, expect: Option<&str>) -> R<Option<(Option<Value>, String)>> {
        let Some(t) = self.ty_of(recv, env, expect) else { return Ok(None) };
        if t == "char" {
            if self.cx.builtin_method_ty("char", name).is_none() {
                return Ok(None);
            }
            let (c, _) = self.expr(recv, env, Some("char"))?;
            let c = c.ok_or("bad char")?;
            let r = match name {
                "to_u32" => (c, "u32"),
                "to_ascii_uppercase" => {
                    let t = self.b.ins().iadd_imm(c, -0x61);
                    let is_lower = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, t, 25);
                    let up = self.b.ins().iadd_imm(c, -32);
                    (self.b.ins().select(is_lower, up, c), "char")
                }
                "to_ascii_lowercase" => {
                    let t = self.b.ins().iadd_imm(c, -0x41);
                    let is_upper = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, t, 25);
                    let low = self.b.ins().iadd_imm(c, 32);
                    (self.b.ins().select(is_upper, low, c), "char")
                }
                "is_ascii_digit" => {
                    let t = self.b.ins().iadd_imm(c, -0x30);
                    (self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, t, 9), "bool")
                }
                "is_ascii_alphabetic" => {
                    let lower = self.b.ins().bor_imm(c, 0x20);
                    let t = self.b.ins().iadd_imm(lower, -0x61);
                    (self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, t, 25), "bool")
                }
                "is_ascii_whitespace" => {
                    // ' ' \t \n \x0c \r
                    let sp = self.b.ins().icmp_imm(IntCC::Equal, c, 0x20);
                    let t = self.b.ins().iadd_imm(c, -9);
                    let ctl = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, t, 4);
                    let not_vt = self.b.ins().icmp_imm(IntCC::NotEqual, c, 0x0b);
                    let ctl2 = self.b.ins().band(ctl, not_vt);
                    (self.b.ins().bor(sp, ctl2), "bool")
                }
                "len_utf8" => {
                    let one = self.b.ins().iconst(types::I64, 1);
                    let two = self.b.ins().iconst(types::I64, 2);
                    let three = self.b.ins().iconst(types::I64, 3);
                    let four = self.b.ins().iconst(types::I64, 4);
                    let lt80 = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, c, 0x80);
                    let lt800 = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, c, 0x800);
                    let lt10000 = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, c, 0x10000);
                    let s34 = self.b.ins().select(lt10000, three, four);
                    let s234 = self.b.ins().select(lt800, two, s34);
                    (self.b.ins().select(lt80, one, s234), "usize")
                }
                _ => return Ok(None),
            };
            return Ok(Some((Some(r.0), r.1.to_string())));
        }
        if is_float(&t) && matches!(name, "sqrt" | "floor" | "ceil" | "trunc" | "abs") {
            let (x, _) = self.expr(recv, env, Some(&t))?;
            let x = x.ok_or("bad value")?;
            let r = match name {
                "sqrt" => self.b.ins().sqrt(x),
                "floor" => self.b.ins().floor(x),
                "ceil" => self.b.ins().ceil(x),
                "trunc" => self.b.ins().trunc(x),
                _ => self.b.ins().fabs(x),
            };
            return Ok(Some((Some(r), t)));
        }
        if is_float(&t) {
            if let Some(to) = name.strip_prefix("trunc_to_") {
                if !is_int(to) {
                    // `i128` / `u128` は未対応。呼び出し側がバックエンド全体をあきらめる。
                    return Ok(None);
                }
                return self.trunc_to_int(recv, &t, to, env).map(Some);
            }
        }
        if is_int(&t) && matches!(name, "leading_zeros" | "trailing_zeros" | "count_ones" | "count_zeros") {
            let (x, _) = self.expr(recv, env, Some(&t))?;
            let x = x.ok_or("bad value")?;
            let bits = cl(&t).bits() as i64;
            let r = match name {
                "leading_zeros" => self.b.ins().clz(x),
                "trailing_zeros" => self.b.ins().ctz(x),
                "count_ones" => self.b.ins().popcnt(x),
                _ => {
                    let ones = self.b.ins().popcnt(x);
                    let total = self.b.ins().iconst(cl(&t), bits);
                    self.b.ins().isub(total, ones)
                }
            };
            let r32 = if cl(&t).bits() < 32 {
                self.b.ins().uextend(types::I32, r)
            } else if cl(&t).bits() > 32 {
                self.b.ins().ireduce(types::I32, r)
            } else {
                r
            };
            return Ok(Some((Some(r32), "u32".to_string())));
        }
        if is_int(&t) && (name == "to_f64" || name == "to_f32") {
            let (x, _) = self.expr(recv, env, Some(&t))?;
            let x = x.ok_or("bad value")?;
            let ft = if name == "to_f64" { types::F64 } else { types::F32 };
            let r = if signed(&t) { self.b.ins().fcvt_from_sint(ft, x) } else { self.b.ins().fcvt_from_uint(ft, x) };
            return Ok(Some((Some(r), if name == "to_f64" { "f64" } else { "f32" }.to_string())));
        }
        Ok(None)
    }

    /// `x.trunc_to_i8()` など (レシーバは `f32` / `f64`、`to` は整数): `Some(切り捨てた値)` を返す。
    /// 有限で、切り捨てた値が目標の型の範囲に入るときだけ `Some`。NaN・±無限大・範囲外は `None`。
    fn trunc_to_int(&mut self, recv: &V, ft: &str, to: &str, env: &mut Env) -> R<(Option<Value>, String)> {
        let (x, _) = self.expr(recv, env, Some(ft))?;
        let x = x.ok_or("bad value")?;
        // 範囲の検査と変換は f64 で行う (`f32` の値は f64 で厳密に表せる)。インタプリタと同じ。
        let x = if ft == "f32" { self.b.ins().fpromote(types::F64, x) } else { x };
        let bits = cl(to).bits() as i32;
        let sg = signed(to);
        // 切り捨て `trunc(x)` が目標の範囲に入る条件は `min - 1 < x < max + 1`。
        // 上限 `max + 1` は 2 の冪なので常に厳密。下限 `min - 1` は 64 ビット符号つきだけ
        // 丸まるので、その場合は `x >= min` とする (`min - 1` と `min` の間に f64 はない)。
        let (lo, hi, lo_strict) = if sg {
            let half = 2f64.powi(bits - 1);
            if bits >= 64 { (-half, half, false) } else { (-half - 1.0, half, true) }
        } else {
            (-1.0, 2f64.powi(bits), true)
        };
        let (lc, hc) = (self.b.ins().f64const(lo), self.b.ins().f64const(hi));
        let ge = if lo_strict {
            self.b.ins().fcmp(FloatCC::GreaterThan, x, lc)
        } else {
            self.b.ins().fcmp(FloatCC::GreaterThanOrEqual, x, lc)
        };
        let lt = self.b.ins().fcmp(FloatCC::LessThan, x, hc);
        // NaN はどちらの比較も偽なので `None` になる。
        let ok = self.b.ins().band(ge, lt);
        let rty = self.cx.builtin_enum("Option", &[to.to_string()])?;
        let l = self.cx.layout(&rty, 0)?;
        let res = self.alloc(l.size, l.align);
        let poff = l.variants[1].1[0].1 as i64;
        let (some_b, none_b, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.ins().brif(ok, some_b, &[], none_b, &[]);
        self.b.switch_to_block(none_b);
        let t0 = self.b.ins().iconst(types::I32, 0);
        self.b.ins().store(MemFlags::new(), t0, res, 0);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(some_b);
        // 範囲内なので飽和変換でも値は変わらない。64 ビットに変換してから目標の幅に縮める。
        let conv = if sg { self.b.ins().fcvt_to_sint_sat(types::I64, x) } else { self.b.ins().fcvt_to_uint_sat(types::I64, x) };
        let v = if sg { self.int_cast(conv, "i64", to) } else { self.int_cast(conv, "u64", to) };
        let dst = self.b.ins().iadd_imm(res, poff);
        self.b.ins().store(MemFlags::new(), v, dst, 0);
        let t1 = self.b.ins().iconst(types::I32, 1);
        self.b.ins().store(MemFlags::new(), t1, res, 0);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        Ok((Some(res), rty))
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
                "starts_with" | "find" => {
                    let (pv, pt_) = self.expr(args.first().ok_or("needs a pattern")?, env, Some("str"))?;
                    if pt_ != "str" {
                        return Err("pattern must be a str".into());
                    }
                    let pv = pv.ok_or("bad pattern")?;
                    let pt = self.ptr_ty();
                    let sp = self.b.ins().load(pt, MemFlags::new(), p, 0);
                    let pp = self.b.ins().load(pt, MemFlags::new(), pv, 0);
                    let plen = self.b.ins().load(i64t, MemFlags::new(), pv, 8);
                    let mc = self.rt.memcmp;
                    if name == "starts_with" {
                        let fits = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, plen, len);
                        let (cmp_b, done) = (self.b.create_block(), self.b.create_block());
                        self.b.append_block_param(done, types::I8);
                        let no = self.b.ins().iconst(types::I8, 0);
                        self.b.ins().brif(fits, cmp_b, &[], done, &[no]);
                        self.b.switch_to_block(cmp_b);
                        let r = self.call_rt(mc, &[sp, pp, plen])[0];
                        let eq = self.b.ins().icmp_imm(IntCC::Equal, r, 0);
                        self.b.ins().jump(done, &[eq]);
                        self.b.switch_to_block(done);
                        return Ok((Some(self.b.block_params(done)[0]), "bool".into()));
                    }
                    // find: 先頭から 1 バイトずつずらして比べる。見つからなければ None。
                    let rty = self.cx.builtin_enum("Option", &["usize".into()])?;
                    let l = self.cx.layout(&rty, 0)?;
                    let res = self.alloc(l.size, l.align);
                    let poff = l.variants[1].1[0].1 as i64;
                    let (head, body, cont, found, none, done) = (
                        self.b.create_block(),
                        self.b.create_block(),
                        self.b.create_block(),
                        self.b.create_block(),
                        self.b.create_block(),
                        self.b.create_block(),
                    );
                    self.b.append_block_param(head, i64t);
                    self.b.append_block_param(found, i64t);
                    let zero = self.b.ins().iconst(i64t, 0);
                    self.b.ins().jump(head, &[zero]);
                    self.b.switch_to_block(head);
                    let i = self.b.block_params(head)[0];
                    // i + plen <= len か
                    let end = self.b.ins().iadd(i, plen);
                    let ok = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, end, len);
                    self.b.ins().brif(ok, body, &[], none, &[]);
                    self.b.switch_to_block(body);
                    let at = self.b.ins().iadd(sp, i);
                    let r = self.call_rt(mc, &[at, pp, plen])[0];
                    let eq = self.b.ins().icmp_imm(IntCC::Equal, r, 0);
                    self.b.ins().brif(eq, found, &[i], cont, &[]);
                    self.b.switch_to_block(cont);
                    let n = self.b.ins().iadd_imm(i, 1);
                    self.b.ins().jump(head, &[n]);
                    self.b.switch_to_block(found);
                    let at_i = self.b.block_params(found)[0];
                    let dst = self.b.ins().iadd_imm(res, poff);
                    self.b.ins().store(MemFlags::new(), at_i, dst, 0);
                    let t1 = self.b.ins().iconst(types::I32, 1);
                    self.b.ins().store(MemFlags::new(), t1, res, 0);
                    self.b.ins().jump(done, &[]);
                    self.b.switch_to_block(none);
                    let t0 = self.b.ins().iconst(types::I32, 0);
                    self.b.ins().store(MemFlags::new(), t0, res, 0);
                    self.b.ins().jump(done, &[]);
                    self.b.switch_to_block(done);
                    return Ok((Some(res), rty));
                }
                "split_at" => {
                    let (mid, _) = self.expr(args.first().ok_or("split_at needs an index")?, env, Some("usize"))?;
                    let mid = mid.ok_or("bad index")?;
                    let pt = self.ptr_ty();
                    let tup = self.cx.tuple_of(&["str".to_string(), "str".to_string()]);
                    let rty = self.cx.builtin_enum("Option", &[tup.clone()])?;
                    let l = self.cx.layout(&rty, 0)?;
                    let res = self.alloc(l.size, l.align);
                    let tl = self.cx.layout(&tup, 0)?;
                    let sp = self.b.ins().load(pt, MemFlags::new(), p, 0);
                    // mid <= len かつ、文字の境界 (mid == len または、その位置のバイトが継続バイトでない)
                    let inb = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, mid, len);
                    let (chk, yes, no, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
                    self.b.ins().brif(inb, chk, &[], no, &[]);
                    self.b.switch_to_block(chk);
                    let at_end = self.b.ins().icmp(IntCC::Equal, mid, len);
                    let rd = self.b.create_block();
                    self.b.ins().brif(at_end, yes, &[], rd, &[]);
                    self.b.switch_to_block(rd);
                    let ba = self.b.ins().iadd(sp, mid);
                    let bt = self.b.ins().load(types::I8, MemFlags::new(), ba, 0);
                    let b32 = self.b.ins().uextend(types::I32, bt);
                    let m = self.b.ins().band_imm(b32, 0xC0);
                    let boundary = self.b.ins().icmp_imm(IntCC::NotEqual, m, 0x80);
                    self.b.ins().brif(boundary, yes, &[], no, &[]);
                    self.b.switch_to_block(no);
                    let t0 = self.b.ins().iconst(types::I32, 0);
                    self.b.ins().store(MemFlags::new(), t0, res, 0);
                    self.b.ins().jump(done, &[]);
                    self.b.switch_to_block(yes);
                    let poff = l.variants[1].1[0].1 as i64;
                    let base = self.b.ins().iadd_imm(res, poff);
                    let a0 = self.b.ins().iadd_imm(base, tl.fields[0].2 as i64);
                    self.b.ins().store(MemFlags::new(), sp, a0, 0);
                    self.b.ins().store(MemFlags::new(), mid, a0, 8);
                    let a1 = self.b.ins().iadd_imm(base, tl.fields[1].2 as i64);
                    let rest = self.b.ins().iadd(sp, mid);
                    let rest_len = self.b.ins().isub(len, mid);
                    self.b.ins().store(MemFlags::new(), rest, a1, 0);
                    self.b.ins().store(MemFlags::new(), rest_len, a1, 8);
                    let t1 = self.b.ins().iconst(types::I32, 1);
                    self.b.ins().store(MemFlags::new(), t1, res, 0);
                    self.b.ins().jump(done, &[]);
                    self.b.switch_to_block(done);
                    return Ok((Some(res), rty));
                }
                _ => Err(format!("str method {}", name)),
            };
        }
        if rt == STRING {
            return self.string_method(p, name, args, env);
        }
        let is_slice = self.cx.slices.borrow().contains_key(rt);
        let is_deque = self.cx.deques.borrow().contains_key(rt);
        let elem = self.cx.seq_elem(rt).or_else(|| self.cx.deque_elem(rt)).ok_or("not a sequence")?;
        let sz = self.elem_size(&elem)?;
        if is_slice || matches!(name, "get" | "first" | "last" | "get_mut" | "sub" | "as_slice" | "as_mut_slice" | "contains") {
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
            "remove" => {
                let rty = self.cx.builtin_enum("Option", &[elem.clone()])?;
                let l = self.cx.layout(&rty, 0)?;
                let res = self.alloc(l.size, l.align);
                let (iv, _) = self.expr(args.first().ok_or("remove needs an index")?, env, Some("usize"))?;
                let iv = iv.ok_or("bad index")?;
                let len = self.b.ins().load(i64t, MemFlags::new(), p, 8);
                let inb = self.b.ins().icmp(IntCC::UnsignedLessThan, iv, len);
                let (yes, no, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().brif(inb, yes, &[], no, &[]);
                self.b.switch_to_block(no);
                let t0 = self.b.ins().iconst(types::I32, 0);
                self.b.ins().store(MemFlags::new(), t0, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(yes);
                let base = self.b.ins().load(i64t, MemFlags::new(), p, 0);
                let off = self.b.ins().imul_imm(iv, sz as i64);
                let addr = self.b.ins().iadd(base, off);
                let val = self.load_value(addr, &elem);
                let poff = l.variants[1].1[0].1;
                let dst = self.b.ins().iadd_imm(res, poff as i64);
                self.store_value(dst, &elem, val)?;
                let after = self.b.ins().isub(len, iv);
                let tail_n = self.b.ins().iadd_imm(after, -1);
                let bytes = self.b.ins().imul_imm(tail_n, sz as i64);
                let src = self.b.ins().iadd_imm(addr, sz as i64);
                let mm = self.rt.memmove;
                self.call_rt(mm, &[addr, src, bytes]);
                let newlen = self.b.ins().iadd_imm(len, -1);
                self.b.ins().store(MemFlags::new(), newlen, p, 8);
                let t1 = self.b.ins().iconst(types::I32, 1);
                self.b.ins().store(MemFlags::new(), t1, res, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
                Ok((Some(res), rty))
            }
            "pop" | "pop_back" => {
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
            "push" | "push_back" => {
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
            // VecDeque の先頭への積み込み: 容量が足りなければ倍に確保し直してから、要素を一つ後ろへずらす。
            "push_front" if is_deque => {
                let arg = args.first().ok_or("push_front needs an argument")?;
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
                let shift = self.b.ins().imul_imm(len, sz as i64);
                let moved = self.b.ins().iadd_imm(base, sz as i64);
                let mm = self.rt.memmove;
                self.call_rt(mm, &[moved, base, shift]);
                self.store_value(base, &elem, v)?;
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
            // VecDeque の先頭からの取り出し: 空なら None、そうでなければ先頭を返して残りを詰める。
            "pop_front" if is_deque => {
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
                let base = self.b.ins().load(i64t, MemFlags::new(), p, 0);
                let val = self.load_value(base, &elem);
                let poff = l.variants[1].1[0].1;
                let dst = self.b.ins().iadd_imm(res, poff as i64);
                self.store_value(dst, &elem, val)?;
                let tail_n = self.b.ins().iadd_imm(len, -1);
                let bytes = self.b.ins().imul_imm(tail_n, sz as i64);
                let src = self.b.ins().iadd_imm(base, sz as i64);
                let mm = self.rt.memmove;
                self.call_rt(mm, &[base, src, bytes]);
                let newlen = self.b.ins().iadd_imm(len, -1);
                self.b.ins().store(MemFlags::new(), newlen, p, 8);
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
        if let Some(r) = self.scalar_method(recv, name, env, expect)? {
            return Ok(r);
        }
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
        if op == "shl" || op == "shr" {
            // シフト量は `u32`。型の幅で割った余りにする (wrapping)。
            let (amt, _) = self.expr(&args[0], env, Some("u32"))?;
            let amt = amt.ok_or("bad shift amount")?;
            let bits = cl(&t).bits() as i64;
            let m = self.b.ins().band_imm(amt, bits - 1);
            let r = match (op, signed(&t)) {
                ("shl", _) => self.b.ins().ishl(a, m),
                ("shr", true) => self.b.ins().sshr(a, m),
                _ => self.b.ins().ushr(a, m),
            };
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

/// 数値型の定数 (`u8.MAX` / `core.num.i32.MIN` / `f64.EPSILON` など)。(型, 整数の値 または 浮動小数の値)
fn numeric_const(segs: &[String]) -> Option<(String, Option<i64>, Option<f64>)> {
    let (p, name) = match segs {
        [p, n] => (p.as_str(), n.as_str()),
        [c, m, p, n] if c == "core" && m == "num" => (p.as_str(), n.as_str()),
        _ => return None,
    };
    if is_int(p) {
        let bits = cl(p).bits() as u32;
        let sg = signed(p);
        return match name {
            "BITS" => Some(("u32".to_string(), Some(bits as i64), None)),
            "MAX" => Some((p.to_string(), Some(if sg { i64::MAX >> (64 - bits) } else { -1i64 }), None)),
            "MIN" => Some((p.to_string(), Some(if sg { i64::MIN >> (64 - bits) } else { 0 }), None)),
            _ => None,
        };
    }
    if is_float(p) {
        let is32 = p == "f32";
        let v = match name {
            "EPSILON" => if is32 { f32::EPSILON as f64 } else { f64::EPSILON },
            "INFINITY" => f64::INFINITY,
            "NEG_INFINITY" => f64::NEG_INFINITY,
            "NAN" => f64::NAN,
            "MAX" => if is32 { f32::MAX as f64 } else { f64::MAX },
            "MIN_POSITIVE" => if is32 { f32::MIN_POSITIVE as f64 } else { f64::MIN_POSITIVE },
            _ => return None,
        };
        return Some((p.to_string(), None, Some(v)));
    }
    None
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

/// 関数 `d` (型パラメータは `subst` で具体化した版) の Cranelift のシグネチャ。
fn make_sig(module: &ObjectModule, cx: &Cx, d: &Def, is_entry: bool, subst: &HashMap<String, String>) -> R<cranelift_codegen::ir::Signature> {
    let ptr = module.target_config().pointer_type();
    let old = cx.file.replace(d.file.clone());
    let olds = cx.subst.replace(subst.clone());
    let r = (|| -> R<cranelift_codegen::ir::Signature> {
        let mut sig = module.make_signature();
        if !is_entry && tag(&d.ret) == "ref" {
            return Err(format!("{}: returns a reference", d.name));
        }
        let rty = if is_entry { "void".to_string() } else { cx.pty(&d.ret).map_err(|e| format!("{}: {}", d.name, e))? };
        if !is_entry && is_agg(&rty) {
            sig.params.push(AbiParam::new(ptr));
        }
        for (_, t) in &d.params {
            let pt = cx.pty(t).map_err(|e| format!("{}: {}", d.name, e))?;
            if pt.starts_with("closure<") {
                // 取り込む変数は、隠し引数として渡す
                let caps = cx.closure_types.borrow().get(&pt).map(|x| x.3.clone()).unwrap_or_default();
                for (kind, _, ty) in &caps {
                    sig.params.push(AbiParam::new(cap_abi(kind, ty, ptr)));
                }
                continue;
            }
            sig.params.push(AbiParam::new(if is_agg(&pt) { ptr } else { cl(&pt) }));
        }
        if is_entry {
            sig.returns.push(AbiParam::new(types::I32));
        } else if rty != "void" && !is_agg(&rty) {
            sig.returns.push(AbiParam::new(cl(&rty)));
        }
        Ok(sig)
    })();
    *cx.file.borrow_mut() = old;
    *cx.subst.borrow_mut() = olds;
    r
}

pub fn compile_clif(
    prog: &Program,
    funcs: &HashMap<String, &Def>,
    type_defs: &HashMap<String, &Def>,
    entry: &str,
    out_path: &str,
    release: bool,
) -> R<()> {
    if !funcs.contains_key(entry) {
        return Err("entry not found".into());
    }
    let cx = Cx { types: type_defs, prog, file: RefCell::new(String::new()), gen: RefCell::new(HashMap::new()), vecs: RefCell::new(HashMap::new()), deques: RefCell::new(HashMap::new()), slices: RefCell::new(HashMap::new()), tuples: RefCell::new(HashMap::new()), maps: RefCell::new(HashMap::new()), subst: RefCell::new(HashMap::new()), inst_types: RefCell::new(HashMap::new()), closure_types: RefCell::new(HashMap::new()) };
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
        if !type_params(d).is_empty() {
            // 総称な関数は、呼び出しのたびに型引数を決めて、そのときに具体化する。
            continue;
        }
        order.push(n.clone());
        let mut calls = vec![];
        crate::cnative::calls_in(d.body.as_ref().unwrap(), &mut calls);
        *cx.file.borrow_mut() = d.file.clone();
        for c in calls {
            let segs: Vec<String> = c.split('.').map(String::from).collect();
            todo.push(cx.qual_segs(&segs).join("."));
        }
        // 関数の名前が値として渡されている場合 (`r.map_err(json.oom)`) も拾う。
        let mut nodes: Vec<&V> = vec![];
        crate::check::walk(d.body.as_ref().unwrap(), &mut nodes);
        for nd in nodes {
            if nname(nd) == "Path" {
                let segs = strs(&nf(nd)[0]);
                let q = cx.qual_segs(&segs).join(".");
                if funcs.contains_key(&q) {
                    todo.push(q);
                }
            }
        }
        // 独自のイテレータの `next` (`impls core.iter.Iterator<..>`) は、`for` の中に名前が現れないので、ここで取り込む。
        for (fq, fd) in funcs.iter() {
            if fd.has_impls && fd.is_method() && fq.ends_with(".next") {
                todo.push(fq.clone());
            }
        }
        // メソッド呼び出しの候補: 同じ名前を持つ、型に属する関数をすべて取り込む。
        let mut mnames = vec![];
        method_names(d.body.as_ref().unwrap(), &mut mnames);
        for m in mnames {
            for (fq, fd) in funcs.iter() {
                if fd.is_method() && type_params(fd).is_empty() && fq.rsplit('.').next() == Some(m.as_str()) {
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
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(ptr));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I32));
    let utf8 = module.declare_function("ksn_utf8_valid", Linkage::Local, &sig).map_err(|e| e.to_string())?;
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(ptr));
    sig.params.push(AbiParam::new(ptr));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(ptr));
    let memmove = module.declare_function("memmove", Linkage::Import, &sig).map_err(|e| e.to_string())?;
    let rt = Rt { puts, write, exit, realloc, free, memcpy, memcmp, memmove, utf8 };

    // 関数の宣言。入口は C の `main` にする。構造体・列挙型を返す関数は、先頭に戻り値用のアドレスを取る。
    let ids: RefCell<HashMap<String, FuncId>> = RefCell::new(HashMap::new());
    let pending: RefCell<Vec<Inst>> = RefCell::new(vec![]);
    let empty: HashMap<String, String> = HashMap::new();
    let mut queue: Vec<Inst> = vec![];
    for n in &order {
        let d = funcs[n];
        let sig = make_sig(&module, &cx, d, n == entry, &empty)?;
        let (name, link) = if n == entry { ("main".to_string(), Linkage::Export) } else { (mangle(n), Linkage::Local) };
        let id = module.declare_function(&name, link, &sig).map_err(|e| e.to_string())?;
        ids.borrow_mut().insert(n.clone(), id);
        queue.push(Inst { key: n.clone(), def: d, subst: HashMap::new(), entry: n == entry });
    }

    let mut data_n = 0usize;
    let mut fbc = FunctionBuilderContext::new();
    define_utf8_valid(&mut module, rt.utf8, &mut fbc)?;
    // 生成の途中で見つかった関数 (総称な関数の具体化など) も、待ち行列に足して順に生成する。
    let mut qi = 0usize;
    while qi < queue.len() {
        let inst = queue[qi].clone();
        qi += 1;
        let d = inst.def;
        *cx.file.borrow_mut() = d.file.clone();
        *cx.subst.borrow_mut() = inst.subst.clone();
        let fid = *ids.borrow().get(&inst.key).ok_or("function not declared")?;
        let mut ctx = module.make_context();
        ctx.func.signature = module.declarations().get_function_decl(fid).signature.clone();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
            let eb = b.create_block();
            b.append_block_params_for_function_params(eb);
            b.switch_to_block(eb);
            let params_vals = b.block_params(eb).to_vec();
            let ret_ty = if inst.entry { "void".to_string() } else { cx.pty(&d.ret)? };
            let mut g = FnGen {
                b,
                module: &mut module,
                cx: &cx,
                funcs,
                ids: &ids,
                pending: &pending,
                rt: &rt,
                data_n: &mut data_n,
                entry: inst.entry,
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
                closures: HashMap::new(),
                closure_ctx: HashMap::new(),
                promoted: HashSet::new(),
            };
            g.func(d, &params_vals).map_err(|e| format!("{}: {}", d.name, e))?;
            let FnGen { mut b, .. } = g;
            b.seal_all_blocks();
            b.finalize();
        }
        module.define_function(fid, &mut ctx).map_err(|e| format!("{}: {}", d.name, e))?;
        module.clear_context(&mut ctx);
        *cx.subst.borrow_mut() = HashMap::new();
        let newly: Vec<Inst> = pending.borrow_mut().drain(..).collect();
        queue.extend(newly);
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

/// バイト `ptr[idx]` を I32 に広げて読む。
fn byte_at(b: &mut FunctionBuilder, ptr: Value, idx: Value) -> Value {
    let a = b.ins().iadd(ptr, idx);
    let v = b.ins().load(types::I8, MemFlags::new(), a, 0);
    b.ins().uextend(types::I32, v)
}

/// 継続バイト (`10xxxxxx`) か。
fn is_cont(b: &mut FunctionBuilder, v: Value) -> Value {
    let m = b.ins().band_imm(v, 0xC0);
    b.ins().icmp_imm(IntCC::Equal, m, 0x80)
}

/// `ksn_utf8_valid(ptr, len) -> i32` を定義する (正しい UTF-8 なら 1)。
fn define_utf8_valid(module: &mut ObjectModule, id: FuncId, fbc: &mut FunctionBuilderContext) -> R<()> {
    let mut ctx = module.make_context();
    ctx.func.signature = module.declarations().get_function_decl(id).signature.clone();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, fbc);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let ptr = b.block_params(entry)[0];
        let len = b.block_params(entry)[1];
        let (head, body, ok, bad) = (b.create_block(), b.create_block(), b.create_block(), b.create_block());
        b.append_block_param(head, types::I64);
        let zero = b.ins().iconst(types::I64, 0);
        b.ins().jump(head, &[zero]);
        b.switch_to_block(head);
        let i = b.block_params(head)[0];
        let more = b.ins().icmp(IntCC::UnsignedLessThan, i, len);
        b.ins().brif(more, body, &[], ok, &[]);
        b.switch_to_block(body);
        let b0 = byte_at(&mut b, ptr, i);
        // 1 バイト
        let (ascii, not_ascii) = (b.create_block(), b.create_block());
        let c_ascii = b.ins().icmp_imm(IntCC::UnsignedLessThan, b0, 0x80);
        b.ins().brif(c_ascii, ascii, &[], not_ascii, &[]);
        b.switch_to_block(ascii);
        let n1 = b.ins().iadd_imm(i, 1);
        b.ins().jump(head, &[n1]);
        // 2 バイト (C2..DF)
        b.switch_to_block(not_ascii);
        let (two, not2) = (b.create_block(), b.create_block());
        let t2 = b.ins().iadd_imm(b0, -0xC2);
        let is2 = b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, t2, 0x1D);
        b.ins().brif(is2, two, &[], not2, &[]);
        b.switch_to_block(two);
        let (two_chk, _) = (b.create_block(), 0);
        let need = b.ins().iadd_imm(i, 1);
        let have = b.ins().icmp(IntCC::UnsignedLessThan, need, len);
        b.ins().brif(have, two_chk, &[], bad, &[]);
        b.switch_to_block(two_chk);
        let i1 = b.ins().iadd_imm(i, 1);
        let b1 = byte_at(&mut b, ptr, i1);
        let c1 = is_cont(&mut b, b1);
        let next2 = b.create_block();
        b.ins().brif(c1, next2, &[], bad, &[]);
        b.switch_to_block(next2);
        let n2 = b.ins().iadd_imm(i, 2);
        b.ins().jump(head, &[n2]);
        // 3 バイト (E0..EF)
        b.switch_to_block(not2);
        let (three, not3) = (b.create_block(), b.create_block());
        let t3 = b.ins().iadd_imm(b0, -0xE0);
        let is3 = b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, t3, 0xF);
        b.ins().brif(is3, three, &[], not3, &[]);
        b.switch_to_block(three);
        let three_chk = b.create_block();
        let need = b.ins().iadd_imm(i, 2);
        let have = b.ins().icmp(IntCC::UnsignedLessThan, need, len);
        b.ins().brif(have, three_chk, &[], bad, &[]);
        b.switch_to_block(three_chk);
        let i1 = b.ins().iadd_imm(i, 1);
        let i2 = b.ins().iadd_imm(i, 2);
        let b1 = byte_at(&mut b, ptr, i1);
        let b2 = byte_at(&mut b, ptr, i2);
        let c1 = is_cont(&mut b, b1);
        let c2 = is_cont(&mut b, b2);
        let both = b.ins().band(c1, c2);
        let is_e0 = b.ins().icmp_imm(IntCC::Equal, b0, 0xE0);
        let lt_a0 = b.ins().icmp_imm(IntCC::UnsignedLessThan, b1, 0xA0);
        let bad_e0 = b.ins().band(is_e0, lt_a0);
        let is_ed = b.ins().icmp_imm(IntCC::Equal, b0, 0xED);
        let gt_9f = b.ins().icmp_imm(IntCC::UnsignedGreaterThan, b1, 0x9F);
        let bad_ed = b.ins().band(is_ed, gt_9f);
        let any_bad = b.ins().bor(bad_e0, bad_ed);
        let not_bad = b.ins().bxor_imm(any_bad, 1);
        let valid = b.ins().band(both, not_bad);
        let next3 = b.create_block();
        b.ins().brif(valid, next3, &[], bad, &[]);
        b.switch_to_block(next3);
        let n3 = b.ins().iadd_imm(i, 3);
        b.ins().jump(head, &[n3]);
        // 4 バイト (F0..F4)
        b.switch_to_block(not3);
        let four = b.create_block();
        let t4 = b.ins().iadd_imm(b0, -0xF0);
        let is4 = b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, t4, 4);
        b.ins().brif(is4, four, &[], bad, &[]);
        b.switch_to_block(four);
        let four_chk = b.create_block();
        let need = b.ins().iadd_imm(i, 3);
        let have = b.ins().icmp(IntCC::UnsignedLessThan, need, len);
        b.ins().brif(have, four_chk, &[], bad, &[]);
        b.switch_to_block(four_chk);
        let i1 = b.ins().iadd_imm(i, 1);
        let i2 = b.ins().iadd_imm(i, 2);
        let i3 = b.ins().iadd_imm(i, 3);
        let b1 = byte_at(&mut b, ptr, i1);
        let b2 = byte_at(&mut b, ptr, i2);
        let b3 = byte_at(&mut b, ptr, i3);
        let c1 = is_cont(&mut b, b1);
        let c2 = is_cont(&mut b, b2);
        let c3 = is_cont(&mut b, b3);
        let c12 = b.ins().band(c1, c2);
        let both = b.ins().band(c12, c3);
        let is_f0 = b.ins().icmp_imm(IntCC::Equal, b0, 0xF0);
        let lt_90 = b.ins().icmp_imm(IntCC::UnsignedLessThan, b1, 0x90);
        let bad_f0 = b.ins().band(is_f0, lt_90);
        let is_f4 = b.ins().icmp_imm(IntCC::Equal, b0, 0xF4);
        let gt_8f = b.ins().icmp_imm(IntCC::UnsignedGreaterThan, b1, 0x8F);
        let bad_f4 = b.ins().band(is_f4, gt_8f);
        let any_bad = b.ins().bor(bad_f0, bad_f4);
        let not_bad = b.ins().bxor_imm(any_bad, 1);
        let valid = b.ins().band(both, not_bad);
        let next4 = b.create_block();
        b.ins().brif(valid, next4, &[], bad, &[]);
        b.switch_to_block(next4);
        let n4 = b.ins().iadd_imm(i, 4);
        b.ins().jump(head, &[n4]);
        // 結果
        b.switch_to_block(ok);
        let one = b.ins().iconst(types::I32, 1);
        b.ins().return_(&[one]);
        b.switch_to_block(bad);
        let z = b.ins().iconst(types::I32, 0);
        b.ins().return_(&[z]);
        b.seal_all_blocks();
        b.finalize();
    }
    module.define_function(id, &mut ctx).map_err(|e| format!("utf8 validator: {}", e))?;
    module.clear_context(&mut ctx);
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
