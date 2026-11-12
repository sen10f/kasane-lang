//! ツリーウォーキング・インタプリタ (stage0)。Python 参照実装 ksn/interp.py に対応する。

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::rc::{Rc, Weak};

use crate::check::{children as _children, items, nf, nline, nname, st, strs};
use crate::ffi::{FArg, Out, Ret};
use crate::parser::V;
use crate::program::{Def, Program, Resolved, Target};
use crate::value::*;

const PRIMS: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64",
    "bool", "char", "str",
];

pub struct Interp {
    pub p: Rc<Program>,
    pub depth: usize,
    pub const_cache: HashMap<String, Val>,
    pub args: Vec<String>,
    pub out: std::io::BufWriter<std::io::Stdout>,
    pub reload: Option<std::sync::mpsc::Receiver<Program>>,
    keep: Vec<Rc<Program>>,
    /// std.fs で開いたファイル (記述子 -> ファイル)
    pub files: HashMap<i128, std::fs::File>,
    pub next_fd: i128,
    /// const の印を付けた場所ポインタ (`from_ref` / `as_const` の結果)。番地 -> 場所の Weak。
    /// Weak が番地を確保しているので、印の付いた場所の番地は別の場所に再利用されない。
    const_refs: HashMap<usize, Weak<dyn Place>>,
    const_prune_at: usize,
}

fn unhex(s: &str) -> String {
    let bytes: Vec<u8> = (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap_or(0)).collect();
    String::from_utf8_lossy(&bytes).to_string()
}

fn parse_int(s: &str) -> i128 {
    s.parse::<i128>().unwrap_or_else(|_| s.parse::<u128>().map(|u| u as i128).unwrap_or(0))
}

impl Interp {
    pub fn new(p: Rc<Program>) -> Interp {
        Interp {
            p,
            depth: 0,
            const_cache: HashMap::new(),
            args: vec![],
            out: std::io::BufWriter::new(std::io::stdout()),
            reload: None,
            keep: vec![],
            files: HashMap::new(),
            next_fd: 3,
            const_refs: HashMap::new(),
            const_prune_at: 1024,
        }
    }

    /// 場所ポインタに const の印を付けて返す。`write` は印の付いたポインタでは panic する (`as_mut` で印を外す)。
    fn mark_const(&mut self, p: Rc<dyn Place>) -> Rc<dyn Place> {
        if self.const_refs.len() >= self.const_prune_at {
            self.const_refs.retain(|_, w| w.strong_count() > 0);
            self.const_prune_at = (self.const_refs.len() * 2).max(1024);
        }
        self.const_refs.insert(Rc::as_ptr(&p) as *const u8 as usize, Rc::downgrade(&p));
        p
    }

    /// 場所ポインタに const の印が付いているか。
    fn is_const_place(&self, p: &Rc<dyn Place>) -> bool {
        self.const_refs.get(&(Rc::as_ptr(p) as *const u8 as usize)).and_then(|w| w.upgrade()).is_some_and(|s| Rc::ptr_eq(&s, p))
    }

    fn def(&self, i: usize) -> &'static Def {
        // Program は Rc で実行中ずっと生きるため、'static として扱う(参照は内部でのみ使う)。
        unsafe { &*(&self.p.defs[i] as *const Def) }
    }

    // ------------------------------------------------------------ 型による変換
    pub fn coerce(&self, v: Val, ty: &V) -> Val {
        use crate::check::{is_nil, tag, tel};
        if is_nil(ty) {
            return v;
        }
        match tag(ty).as_str() {
            "prim" => {
                let p = st(tel(ty, 1));
                if is_int_ty(&p) {
                    if let Val::Int(x, None) = v {
                        return Val::Int(x, Some(ity(&p)));
                    }
                } else if p == "f32" {
                    match v {
                        Val::Int(x, _) => return Val::F32(x as f32),
                        Val::Float(x) => return Val::F32(x as f32),
                        _ => {}
                    }
                } else if p == "f64" {
                    if let Val::Int(x, _) = v {
                        return Val::Float(x as f64);
                    }
                }
                v
            }
            "tuple" => {
                if let Val::Tuple(t) = &v {
                    let tys = items(tel(ty, 1));
                    if t.len() == tys.len() {
                        return Val::Tuple(Rc::new(t.iter().zip(tys).map(|(x, y)| self.coerce(x.clone(), y)).collect()));
                    }
                }
                v
            }
            "array" => {
                if let Val::Arr(l) = &v {
                    let items: Vec<Val> = l.borrow().iter().map(|x| self.coerce(x.clone(), tel(ty, 1))).collect();
                    return Val::Arr(new_list(items));
                }
                v
            }
            "ref" => {
                if matches!(v, Val::Ref(_)) {
                    v
                } else {
                    self.coerce(v, tel(ty, 2))
                }
            }
            "named" => {
                if st(tel(ty, 1)) == "Option" && !items(tel(ty, 2)).is_empty() {
                    if let Val::Enum(e) = &v {
                        if !e.pl.is_empty() {
                            let pl = e.pl.iter().map(|x| self.coerce(x.clone(), &items(tel(ty, 2))[0])).collect();
                            return Val::Enum(Rc::new(EnumV { en: e.en.clone(), var: e.var.clone(), pl }));
                        }
                    }
                }
                v
            }
            _ => v,
        }
    }

    pub fn is_copy(&self, tyfqn: &str) -> bool {
        self.p.types.get(tyfqn).map_or(false, |&i| self.p.defs[i].derive.iter().any(|x| x == "Copy"))
    }

    pub fn cp(&self, v: Val) -> Val {
        if let Val::Struct(s) = &v {
            if self.is_copy(&s.ty) {
                let f = s.f.borrow().iter().map(|(k, x)| (k.clone(), self.cp(x.clone()))).collect();
                return Val::Struct(Rc::new(StructV { ty: s.ty.clone(), f: RefCell::new(f) }));
            }
        }
        v
    }

    // ------------------------------------------------------------ 定数
    pub fn const_value(&mut self, di: usize) -> Res {
        let d = self.def(di);
        if let Some(v) = self.const_cache.get(&d.name) {
            return Ok(v.clone());
        }
        let sc = new_scope(None, Rc::from(d.file.as_str()));
        let v = self.eval(d.init.as_ref().unwrap(), &sc)?;
        let v = self.coerce(v, &d.ret);
        self.const_cache.insert(d.name.clone(), v.clone());
        Ok(v)
    }

    // ------------------------------------------------------------ 呼び出し
    fn poll_reload(&mut self) {
        let np = match &self.reload {
            Some(rx) => rx.try_recv().ok(),
            None => None,
        };
        if let Some(np) = np {
            let old = std::mem::replace(&mut self.p, Rc::new(np));
            self.keep.push(old);
            eprintln!("hot reload: function bodies updated");
        }
    }

    pub fn call_def(&mut self, di: usize, args: Vec<Val>) -> Res {
        self.poll_reload();
        let d = self.def(di);
        if args.len() != d.params.len() {
            return panic(format!("arity mismatch calling {}: expected {}, got {}", d.name, d.params.len(), args.len()));
        }
        let sc = new_scope(None, Rc::from(d.file.as_str()));
        for ((pn, pty), a) in d.params.iter().zip(args) {
            let a = self.coerce(a, pty);
            sc_declare(&sc, pn, a);
        }
        self.depth += 1;
        if self.depth > 2500 {
            eprintln!("stack overflow");
            return Err(Ctl::Exit(102));
        }
        let r = self.eval_block_body(d.body.as_ref().unwrap(), &sc);
        self.depth -= 1;
        let v = match r {
            Ok(v) => v,
            Err(Ctl::Return(v)) => v,
            Err(e) => return Err(e),
        };
        Ok(self.coerce(v, &d.ret))
    }

    /// テスト本体など、引数なしの本体だけを実行する。
    pub fn call_body(&mut self, di: usize) -> Res {
        let d = self.def(di);
        let sc = new_scope(None, Rc::from(d.file.as_str()));
        match self.eval_block_body(d.body.as_ref().unwrap(), &sc) {
            Ok(v) => Ok(v),
            Err(Ctl::Return(v)) => Ok(v),
            Err(e) => Err(e),
        }
    }

    pub fn call_value(&mut self, f: &Val, args: Vec<Val>) -> Res {
        let f = dr(f);
        match &f {
            Val::Closure(c) => {
                let sc = new_scope(Some(&c.scope), Rc::from(""));
                for ((pn, pty), a) in c.params.iter().zip(args) {
                    let a = self.coerce(a, pty);
                    sc_declare(&sc, pn, a);
                }
                let r = match self.eval(&c.body, &sc) {
                    Ok(v) => v,
                    Err(Ctl::Return(v)) => v,
                    Err(e) => return Err(e),
                };
                Ok(self.coerce(r, &c.ret))
            }
            Val::Func(i) => self.call_def(*i, args),
            Val::Native(n) => self.call_native(&n.clone(), args),
            Val::Ctor(c) => {
                let (en, vn) = (&c.0, &c.1);
                self.mk_variant(en, vn, args)
            }
            _ => panic("not callable"),
        }
    }

    fn mk_variant(&self, en: &str, vn: &str, args: Vec<Val>) -> Res {
        if let Some(&ei) = self.p.types.get(en) {
            let d = &self.p.defs[ei];
            if let Some((_, tys)) = d.variants.iter().find(|v| v.0 == vn) {
                let pl = args.into_iter().zip(tys).map(|(a, t)| self.coerce(a, t)).collect();
                return Ok(mk_enum(en, vn, pl));
            }
        }
        Ok(mk_enum(en, vn, args))
    }

    // ------------------------------------------------------------ 評価
    pub fn eval_block_body(&mut self, blk: &V, sc: &Scope) -> Res {
        let mut v = Val::Unit;
        for s in items(&nf(blk)[0]) {
            v = self.exec_stmt(s, sc)?;
        }
        Ok(v)
    }

    fn exec_stmt(&mut self, s: &V, sc: &Scope) -> Res {
        let f = nf(s);
        match nname(s) {
            "Let" => {
                let v = self.eval(&f[3], sc)?;
                let mut v = self.cp(v);
                if !matches!(f[2], V::Nil) {
                    v = self.coerce(v, &f[2]);
                }
                if !self.bind(&f[0], v, sc)? {
                    return panic("refutable let pattern failed");
                }
                Ok(Val::Unit)
            }
            "Assign" => {
                let v = self.eval(&f[1], sc)?;
                let v = self.cp(v);
                self.assign(&f[0], v, sc)?;
                Ok(Val::Unit)
            }
            _ => self.eval(&f[0], sc),
        }
    }

    fn eval_block(&mut self, blk: &V, sc: &Scope) -> Res {
        let inner = new_scope(Some(sc), Rc::from(""));
        let mut v = Val::Unit;
        for s in items(&nf(blk)[0]) {
            v = self.exec_stmt(s, &inner)?;
        }
        Ok(v)
    }

    pub fn eval(&mut self, e: &V, sc: &Scope) -> Res {
        let f = nf(e);
        match nname(e) {
            "Lit" => Ok(self.lit(e)),
            "Block" => self.eval_block(e, sc),
            "Unsafe" => self.eval_block(&f[2], sc),
            "TupleE" => {
                let mut out = vec![];
                for x in items(&f[0]) {
                    let v = self.eval(x, sc)?;
                    out.push(self.cp(v));
                }
                Ok(Val::Tuple(Rc::new(out)))
            }
            "ArrayE" => {
                let mut out = vec![];
                for x in items(&f[0]) {
                    let v = self.eval(x, sc)?;
                    out.push(self.cp(v));
                }
                Ok(Val::Arr(new_list(out)))
            }
            "ArrayRep" => {
                let v = self.eval(&f[0], sc)?;
                let n = match &f[1] {
                    V::N(s) => parse_int(s) as usize,
                    V::L(xs) if xs.len() == 2 => {
                        let segs: Vec<V> = st(&xs[1]).split('.').map(|x| V::S(x.to_string())).collect();
                        let p = V::Node("Path", vec![V::L(segs), V::Nil], 0);
                        match dr(&self.eval(&p, sc)?) {
                            Val::Int(x, _) => x as usize,
                            _ => 0,
                        }
                    }
                    _ => 0,
                };
                let out: Vec<Val> = (0..n).map(|_| self.cp(v.clone())).collect();
                Ok(Val::Arr(new_list(out)))
            }
            "StructLit" => {
                let name = st(&f[0]);
                let segs: Vec<String> = name.split('.').map(String::from).collect();
                let file = sc.borrow().file.to_string();
                let fq = self.p.expand(&segs, &file).join(".");
                let ftypes: Vec<(String, V)> = self
                    .p
                    .types
                    .get(&fq)
                    .map(|&i| self.p.defs[i].fields.iter().map(|x| (x.0.clone(), x.1.clone())).collect())
                    .unwrap_or_default();
                let mut fields = vec![];
                for fe in items(&f[1]) {
                    let fnm = st(&items(fe)[0]);
                    let v = self.eval(&items(fe)[1], sc)?;
                    let v = self.cp(v);
                    let v = match ftypes.iter().find(|x| x.0 == fnm) {
                        Some((_, t)) => self.coerce(v, t),
                        None => v,
                    };
                    fields.push((fnm, v));
                }
                Ok(Val::Struct(Rc::new(StructV { ty: fq, f: RefCell::new(fields) })))
            }
            "Closure" => {
                let params = items(&f[0]).iter().map(|p| (st(&items(p)[0]), items(p)[1].clone())).collect();
                Ok(Val::Closure(Rc::new(ClosureV { params, ret: f[1].clone(), body: f[3].clone(), scope: sc.clone() })))
            }
            "If" => {
                if self.truthy(&f[0], sc)? {
                    self.eval(&f[1], sc)
                } else if !matches!(f[2], V::Nil) {
                    self.eval(&f[2], sc)
                } else {
                    Ok(Val::Unit)
                }
            }
            "Match" => {
                let v = self.eval(&f[0], sc)?;
                for arm in items(&f[1]) {
                    let s2 = new_scope(Some(sc), Rc::from(""));
                    if self.bind(&items(arm)[0], v.clone(), &s2)? {
                        return self.eval(&items(arm)[1], &s2);
                    }
                }
                panic(format!("non-exhaustive match at line {}", nline(e)))
            }
            "Loop" => {
                loop {
                    match self.eval(&f[0], sc) {
                        Ok(_) | Err(Ctl::Continue) => {}
                        Err(Ctl::Break) => break,
                        Err(e) => return Err(e),
                    }
                }
                Ok(Val::Unit)
            }
            "While" => {
                while self.truthy(&f[0], sc)? {
                    match self.eval(&f[1], sc) {
                        Ok(_) | Err(Ctl::Continue) => {}
                        Err(Ctl::Break) => break,
                        Err(e) => return Err(e),
                    }
                }
                Ok(Val::Unit)
            }
            "For" => {
                let it = self.eval(&f[1], sc)?;
                let it = self.to_iter(&it)?;
                loop {
                    let o = self.iter_next(&it)?;
                    let Val::Enum(en) = &o else { break };
                    if en.var == "None" {
                        break;
                    }
                    let s2 = new_scope(Some(sc), Rc::from(""));
                    self.bind(&f[0], en.pl[0].clone(), &s2)?;
                    match self.eval(&f[2], &s2) {
                        Ok(_) | Err(Ctl::Continue) => {}
                        Err(Ctl::Break) => break,
                        Err(e) => return Err(e),
                    }
                }
                Ok(Val::Unit)
            }
            "Break" => Err(Ctl::Break),
            "Continue" => Err(Ctl::Continue),
            "Return" => {
                let v = if matches!(f[0], V::Nil) { Val::Unit } else { self.eval(&f[0], sc)? };
                Err(Ctl::Return(v))
            }
            "ExprStmt" | "Let" | "Assign" => self.exec_stmt(e, sc),
            "Try" => {
                let v = dr(&self.eval(&f[0], sc)?);
                if let Val::Enum(en) = &v {
                    if en.en == "Result" {
                        if en.var == "Ok" {
                            return Ok(en.pl[0].clone());
                        }
                        return Err(Ctl::Return(v.clone()));
                    }
                    if en.en == "Option" {
                        if en.var == "Some" {
                            return Ok(en.pl[0].clone());
                        }
                        return Err(Ctl::Return(v.clone()));
                    }
                }
                panic("? applied to non-Option/Result")
            }
            "Unary" => self.unary(e, sc),
            "Bin" => self.binary(e, sc),
            "Path" => self.e_path(e, sc),
            "Field" => {
                let b = dr(&self.eval(&f[0], sc)?);
                let name = st(&f[1]);
                if let Val::Struct(s) = &b {
                    if let Some(v) = s.get(&name) {
                        return Ok(self.cp(v));
                    }
                }
                panic(format!("no field {}", name))
            }
            "Index" => {
                let b = dr(&self.eval(&f[0], sc)?);
                let i = match dr(&self.eval(&f[1], sc)?) {
                    Val::Int(x, _) => x,
                    _ => return panic("bad index"),
                };
                // 要素 1 個だけを複製する (列全体を複製すると読み取りが長さに比例する)。
                let len = seq_len(&b).unwrap_or(0);
                if i < 0 || i as usize >= len {
                    return panic(format!("index out of bounds: the len is {} but the index is {}", len, i));
                }
                Ok(seq_elem(&b, i as usize).unwrap_or(Val::Unit))
            }
            "Call" => self.e_call(e, sc),
            "MCall" => {
                let recv = self.eval(&f[0], sc)?;
                let name = st(&f[1]);
                let mut args = vec![];
                for a in items(&f[2]) {
                    let v = self.eval(a, sc)?;
                    args.push(self.cp(v));
                }
                let recv_expr = f[0].clone();
                let sc2 = sc.clone();
                self.invoke_method(recv, &name, args, Some((&recv_expr, &sc2)))
            }
            n => panic(format!("cannot evaluate node {}", n)),
        }
    }

    fn truthy(&mut self, e: &V, sc: &Scope) -> Result<bool, Ctl> {
        Ok(matches!(dr(&self.eval(e, sc)?), Val::Bool(true)))
    }

    fn lit(&self, e: &V) -> Val {
        let f = nf(e);
        match st(&f[0]).as_str() {
            "int" => {
                let sfx = match &f[2] {
                    V::S(s) => Some(ity(s)),
                    _ => None,
                };
                Val::Int(parse_int(&st(&f[1])), sfx)
            }
            "float" => {
                let x = st(&f[2]).parse::<f64>().unwrap_or(0.0);
                if matches!(&f[1], V::S(s) if s.as_str() == "f32") { Val::F32(x as f32) } else { Val::Float(x) }
            }
            "str" => Val::Str(Rc::from(unhex(&st(&f[1])).as_str())),
            "char" => Val::Char(char::from_u32(parse_int(&st(&f[1])) as u32).unwrap_or('\0')),
            "bool" => Val::Bool(matches!(f[1], V::B(true))),
            "byte" => int(parse_int(&st(&f[1])), "u8"),
            "bstr" => Val::Arr(new_list(items(&f[1]).iter().map(|x| int(parse_int(&st(x)), "u8")).collect())),
            _ => Val::Unit,
        }
    }

    fn unary(&mut self, e: &V, sc: &Scope) -> Res {
        let f = nf(e);
        let op = st(&f[0]);
        match op.as_str() {
            "&mut" => return self.eval_place(&f[1], sc),
            "&" => return self.eval(&f[1], sc),
            "*" => {
                let v = self.eval(&f[1], sc)?;
                return Ok(self.cp(dr(&v)));
            }
            _ => {}
        }
        let v = dr(&self.eval(&f[1], sc)?);
        match (op.as_str(), &v) {
            ("-", Val::Int(x, t)) => match t {
                Some(ty) if *ty == "u128" || !signed(ty) => {
                    if *x == 0 { Ok(Val::Int(0, *t)) } else { panic("attempt to negate an unsigned integer".to_string()) }
                }
                Some(ty) if -*x > tmax(ty) => panic("attempt to negate with overflow".to_string()),
                _ => Ok(Val::Int(-x, *t)),
            },
            ("-", Val::Float(x)) => Ok(Val::Float(-x)),
            ("-", Val::F32(x)) => Ok(Val::F32(-x)),
            ("!", Val::Bool(b)) => Ok(Val::Bool(!b)),
            ("!", Val::Int(x, t)) => Ok(Val::Int(wrap(!x, t.unwrap_or("i32")), *t)),
            _ => panic(format!("bad unary {}", op)),
        }
    }

    fn binary(&mut self, e: &V, sc: &Scope) -> Res {
        let f = nf(e);
        let op = st(&f[0]);
        if op == "&&" {
            return Ok(Val::Bool(self.truthy(&f[1], sc)? && self.truthy(&f[2], sc)?));
        }
        if op == "||" {
            return Ok(Val::Bool(self.truthy(&f[1], sc)? || self.truthy(&f[2], sc)?));
        }
        let a = dr(&self.eval(&f[1], sc)?);
        let b = dr(&self.eval(&f[2], sc)?);
        match op.as_str() {
            "==" | "!=" => {
                let r = self.v_eq(&a, &b)?;
                return Ok(Val::Bool(if op == "==" { r } else { !r }));
            }
            "<" | "<=" | ">" | ">=" => {
                let c = self.v_cmp(&a, &b)?;
                let r = match c {
                    None => false,
                    Some(c) => match op.as_str() {
                        "<" => c < 0,
                        "<=" => c <= 0,
                        ">" => c > 0,
                        _ => c >= 0,
                    },
                };
                return Ok(Val::Bool(r));
            }
            _ => {}
        }
        if let (Val::Bool(x), Val::Bool(y)) = (&a, &b) {
            return Ok(Val::Bool(match op.as_str() {
                "&" => x & y,
                "|" => x | y,
                _ => x ^ y,
            }));
        }
        let fl = |v: &Val| match v {
            Val::Float(x) => Some(*x),
            Val::Int(x, _) => Some(*x as f64),
            _ => None,
        };
        if matches!(a, Val::F32(_)) || matches!(b, Val::F32(_)) {
            let g = |v: &Val| match v {
                Val::F32(x) => *x,
                Val::Float(x) => *x as f32,
                Val::Int(x, _) => *x as f32,
                _ => 0.0,
            };
            let (x, y) = (g(&a), g(&b));
            return match op.as_str() {
                "+" => Ok(Val::F32(x + y)),
                "-" => Ok(Val::F32(x - y)),
                "*" => Ok(Val::F32(x * y)),
                "/" => Ok(Val::F32(x / y)),
                _ => panic(format!("bad float operator {}", op)),
            };
        }
        if matches!(a, Val::Float(_)) || matches!(b, Val::Float(_)) {
            let (x, y) = (fl(&a).unwrap_or(0.0), fl(&b).unwrap_or(0.0));
            return match op.as_str() {
                "+" => Ok(Val::Float(x + y)),
                "-" => Ok(Val::Float(x - y)),
                "*" => Ok(Val::Float(x * y)),
                "/" => {
                    if y == 0.0 {
                        Ok(Val::Float(if x == 0.0 || x.is_nan() { f64::NAN } else { f64::INFINITY.copysign(x) * 1f64.copysign(y) }))
                    } else {
                        Ok(Val::Float(x / y))
                    }
                }
                _ => panic(format!("bad float operator {}", op)),
            };
        }
        if let (Val::Int(x, ta), Val::Int(y, tb)) = (&a, &b) {
            let ty = ta.or(*tb);
            let r = match op.as_str() {
                "&" => x & y,
                "|" => x | y,
                "^" => x ^ y,
                "+" | "-" | "*" | "/" | "%" => return int_arith(op.as_str(), *x, *y, ty),
                _ => return panic(format!("operator {} is not defined for integers", op)),
            };
            return Ok(Val::Int(r, ty));
        }
        panic(format!("bad operands for {}", op))
    }

    pub fn v_eq(&mut self, a: &Val, b: &Val) -> Result<bool, Ctl> {
        if let Val::Struct(s) = a {
            if let Some(&di) = self.p.funcs.get(&format!("{}.eq", s.ty)) {
                let r = self.call_def(di, vec![a.clone(), b.clone()])?;
                return Ok(matches!(r, Val::Bool(true)));
            }
        }
        if let Val::Enum(en) = a {
            if en.en != "Option" && en.en != "Result" {
                if let Some(&di) = self.p.funcs.get(&format!("{}.eq", en.en)) {
                    let r = self.call_def(di, vec![a.clone(), b.clone()])?;
                    return Ok(matches!(r, Val::Bool(true)));
                }
            }
        }
        Ok(veq(a, b))
    }

    fn order_of(&self, en: &str) -> Box<dyn Fn(&str, &str) -> (usize, usize)> {
        let names: Vec<String> = match en {
            "Option" => vec!["None".into(), "Some".into()],
            "Result" => vec!["Ok".into(), "Err".into()],
            _ => self
                .p
                .types
                .get(en)
                .map(|&i| self.p.defs[i].variants.iter().map(|v| v.0.clone()).collect())
                .unwrap_or_default(),
        };
        Box::new(move |a, b| {
            (names.iter().position(|x| x == a).unwrap_or(0), names.iter().position(|x| x == b).unwrap_or(0))
        })
    }

    fn ordering(v: &Val) -> i32 {
        if let Val::Enum(e) = v {
            return match e.var.as_str() {
                "Less" => -1,
                "Equal" => 0,
                _ => 1,
            };
        }
        0
    }

    pub fn v_cmp(&mut self, a: &Val, b: &Val) -> Result<Option<i32>, Ctl> {
        if let Val::Struct(s) = a {
            if let Some(&di) = self.p.funcs.get(&format!("{}.partial_cmp", s.ty)) {
                let r = self.call_def(di, vec![a.clone(), b.clone()])?;
                if let Val::Enum(e) = &r {
                    if e.var == "None" {
                        return Ok(None);
                    }
                    return Ok(Some(Self::ordering(&e.pl[0])));
                }
            }
            if let Some(&di) = self.p.funcs.get(&format!("{}.cmp", s.ty)) {
                let r = self.call_def(di, vec![a.clone(), b.clone()])?;
                return Ok(Some(Self::ordering(&r)));
            }
        }
        let order = match a {
            Val::Enum(e) => self.order_of(&e.en),
            _ => self.order_of(""),
        };
        Ok(vcmp(a, b, &*order))
    }

    // ------------------------------------------------------------ 場所・代入
    pub fn eval_place(&mut self, e: &V, sc: &Scope) -> Res {
        let f = nf(e);
        match nname(e) {
            "Path" => {
                let segs = strs(&f[0]);
                if let Some(owner) = sc_owner(sc, &segs[0]) {
                    if segs.len() == 1 {
                        let cur = owner.borrow().vars[&segs[0]].clone();
                        if matches!(cur, Val::Ref(_)) {
                            return Ok(cur);
                        }
                        return Ok(Val::Ref(Rc::new(VarPlace(owner, segs[0].clone()))));
                    }
                    let mut base = dr(&owner.borrow().vars[&segs[0]].clone());
                    for s in &segs[1..segs.len() - 1] {
                        base = match &base {
                            Val::Struct(x) => dr(&x.get(s).unwrap_or(Val::Unit)),
                            _ => return panic("bad place"),
                        };
                    }
                    if let Val::Struct(x) = base {
                        return Ok(Val::Ref(Rc::new(FieldPlace(x, segs[segs.len() - 1].clone()))));
                    }
                    return panic("bad place");
                }
                let v = self.eval(e, sc)?;
                Ok(Val::Ref(Rc::new(CellPlace(RefCell::new(v)))))
            }
            "Field" => {
                let b = dr(&self.eval(&f[0], sc)?);
                match b {
                    Val::Struct(x) => Ok(Val::Ref(Rc::new(FieldPlace(x, st(&f[1]))))),
                    _ => panic("bad place"),
                }
            }
            "Index" => {
                let b = dr(&self.eval(&f[0], sc)?);
                let i = match dr(&self.eval(&f[1], sc)?) {
                    Val::Int(x, _) => x,
                    _ => return panic("bad index"),
                };
                match b {
                    Val::Arr(l) | Val::Vec(l) => {
                        if i < 0 || i as usize >= l.borrow().len() {
                            return panic(format!("index out of bounds: the len is {} but the index is {}", l.borrow().len(), i));
                        }
                        Ok(Val::Ref(Rc::new(IdxPlace(l, i as usize))))
                    }
                    Val::View(l, a, bnd) => {
                        if i < 0 || i as usize >= bnd - a {
                            return panic(format!("index out of bounds: the len is {} but the index is {}", bnd - a, i));
                        }
                        Ok(Val::Ref(Rc::new(IdxPlace(l, a + i as usize))))
                    }
                    _ => panic("bad place"),
                }
            }
            "Unary" if st(&f[0]) == "*" => {
                let v = self.eval(&f[1], sc)?;
                if matches!(v, Val::Ref(_)) {
                    Ok(v)
                } else {
                    Ok(Val::Ref(Rc::new(CellPlace(RefCell::new(v)))))
                }
            }
            _ => {
                let v = self.eval(e, sc)?;
                Ok(Val::Ref(Rc::new(CellPlace(RefCell::new(v)))))
            }
        }
    }

    fn assign(&mut self, target: &V, v: Val, sc: &Scope) -> Result<(), Ctl> {
        let f = nf(target);
        if nname(target) == "Unary" && st(&f[0]) == "*" {
            let r = self.eval(&f[1], sc)?;
            if let Val::Ref(p) = r {
                p.set(v);
                return Ok(());
            }
            return panic("assignment through non-reference");
        }
        if nname(target) == "Path" {
            let segs = strs(&f[0]);
            if segs.len() == 1 {
                if let Some(owner) = sc_owner(sc, &segs[0]) {
                    let cur = owner.borrow().vars[&segs[0]].clone();
                    if let Val::Ref(p) = &cur {
                        if !matches!(v, Val::Ref(_)) {
                            p.set(v);
                            return Ok(());
                        }
                    }
                    owner.borrow_mut().vars.insert(segs[0].clone(), v);
                    return Ok(());
                }
            }
        }
        if let Val::Ref(p) = self.eval_place(target, sc)? {
            p.set(v);
        }
        Ok(())
    }

    // ------------------------------------------------------------ パターン
    pub fn bind(&mut self, pat: &V, v: Val, sc: &Scope) -> Result<bool, Ctl> {
        let f = nf(pat);
        let n = nname(pat);
        if n == "PWild" {
            return Ok(true);
        }
        if n == "PBind" {
            sc_declare(sc, &st(&f[0]), v);
            return Ok(true);
        }
        let v = dr(&v);
        match n {
            "PLit" => {
                let lv = self.lit(&f[0]);
                Ok(veq(&v, &lv))
            }
            "PRef" => self.bind(&f[0], v, sc),
            "PTuple" => {
                let Val::Tuple(t) = &v else { return Ok(false) };
                let ps = items(&f[0]);
                if t.len() != ps.len() {
                    return Ok(false);
                }
                for (p, x) in ps.iter().zip(t.iter()) {
                    if !self.bind(p, x.clone(), sc)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            "PPath" => {
                let segs = strs(&f[0]);
                let file = sc.borrow().file.to_string();
                match self.p.resolve(&segs, &file) {
                    Some(r) => match &r.target {
                        Target::Variant(en, vn) => Ok(matches!(&v, Val::Enum(e) if &e.en == en && &e.var == vn)),
                        Target::Const(i) => {
                            let cv = self.const_value(*i)?;
                            Ok(veq(&v, &cv))
                        }
                        _ => panic(format!("unresolved pattern {}", segs.join("."))),
                    },
                    None => panic(format!("unresolved pattern {}", segs.join("."))),
                }
            }
            "PVariant" => {
                let segs = strs(&f[0]);
                let file = sc.borrow().file.to_string();
                let Some(r) = self.p.resolve(&segs, &file) else {
                    return panic(format!("unresolved variant pattern {}", segs.join(".")));
                };
                let Target::Variant(en, vn) = &r.target else {
                    return panic(format!("unresolved variant pattern {}", segs.join(".")));
                };
                let Val::Enum(e) = &v else { return Ok(false) };
                if &e.en != en || &e.var != vn {
                    return Ok(false);
                }
                let ps = items(&f[1]);
                if e.pl.len() != ps.len() {
                    return Ok(false);
                }
                for (p, x) in ps.iter().zip(e.pl.iter()) {
                    if !self.bind(p, x.clone(), sc)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            "PStruct" => {
                let Val::Struct(s) = &v else { return Ok(false) };
                for fp in items(&f[1]) {
                    let fnm = st(&items(fp)[0]);
                    let x = s.get(&fnm).unwrap_or(Val::Unit);
                    if !self.bind(&items(fp)[1], x, sc)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => panic(format!("bad pattern {}", n)),
        }
    }

    // ------------------------------------------------------------ パス・呼び出し
    fn e_path(&mut self, e: &V, sc: &Scope) -> Res {
        let segs = strs(&nf(e)[0]);
        if let Some(owner) = sc_owner(sc, &segs[0]) {
            let mut v = owner.borrow().vars[&segs[0]].clone();
            for s in &segs[1..] {
                v = match dr(&v) {
                    Val::Struct(x) => x.get(s).unwrap_or(Val::Unit),
                    _ => return panic(format!("no field {}", s)),
                };
            }
            return Ok(if segs.len() > 1 { self.cp(v) } else { v });
        }
        let file = sc.borrow().file.to_string();
        let Some(r) = self.p.resolve(&segs, &file) else {
            return panic(format!("unresolved name {} (line {})", segs.join("."), nline(e)));
        };
        self.path_value(&r, 0)
    }

    fn path_value(&mut self, r: &Resolved, drop_last: usize) -> Res {
        let mut v = match &r.target {
            Target::Const(i) => self.const_value(*i)?,
            Target::Variant(en, vn) => {
                let has_payload = match en.as_str() {
                    "Option" => vn == "Some",
                    "Result" => true,
                    _ => self
                        .p
                        .types
                        .get(en)
                        .map_or(false, |&i| self.p.defs[i].variants.iter().any(|x| &x.0 == vn && !x.1.is_empty())),
                };
                if has_payload {
                    Val::Ctor(Rc::new((en.clone(), vn.clone())))
                } else {
                    mk_enum(en, vn, vec![])
                }
            }
            Target::Fn(i) => Val::Func(*i),
            Target::Native(n) => Val::Native(Rc::from(n.as_str())),
            Target::Value(n) => self.num_const_val(n),
            _ => return panic("cannot use definition as value"),
        };
        let rest = &r.rest[..r.rest.len() - drop_last];
        for s in rest {
            v = match dr(&v) {
                Val::Struct(x) => x.get(s).unwrap_or(Val::Unit),
                _ => return panic(format!("no field {}", s)),
            };
        }
        Ok(v)
    }

    fn e_call(&mut self, e: &V, sc: &Scope) -> Res {
        let f = nf(e);
        let fnn = &f[0];
        if nname(fnn) == "Path" {
            let segs = strs(&nf(fnn)[0]);
            if let Some(owner) = sc_owner(sc, &segs[0]) {
                let first = owner.borrow().vars[&segs[0]].clone();
                if segs.len() == 1 {
                    let args = self.eval_args(&f[1], sc)?;
                    return self.call_value(&first, args);
                }
                let mut base = first;
                for s in &segs[1..segs.len() - 1] {
                    base = match dr(&base) {
                        Val::Struct(x) => x.get(s).unwrap_or(Val::Unit),
                        _ => return panic(format!("no field {}", s)),
                    };
                }
                let args = self.eval_args(&f[1], sc)?;
                let place_path = V::Node(
                    "Path",
                    vec![V::L(segs[..segs.len() - 1].iter().map(|x| V::S(x.clone())).collect()), V::Nil],
                    0,
                );
                return self.invoke_method(base, &segs[segs.len() - 1], args, Some((&place_path, sc)));
            }
            let file = sc.borrow().file.to_string();
            let Some(r) = self.p.resolve(&segs, &file) else {
                return panic(format!("unresolved function {} (line {})", segs.join("."), nline(e)));
            };
            if !r.rest.is_empty() {
                let base = self.path_value(&r, 1)?;
                let args = self.eval_args(&f[1], sc)?;
                return self.invoke_method(base, &r.rest[r.rest.len() - 1], args, None);
            }
            // `from_ref(&x)` は `x` の場所を指す。`&x` を値として評価すると複製になり、以後の書き込みが見えなくなる。
            let args = match (&r.target, ref_arg_inner(&f[1])) {
                (Target::Sig(i), Some(inner)) if self.def(*i).name == "core.ptr.from_ref" => vec![self.eval_place(inner, sc)?],
                _ => self.eval_args(&f[1], sc)?,
            };
            if let Target::Sig(i) = &r.target {
                let name = self.def(*i).name.clone();
                if name.starts_with("core.ptr.") {
                    // 型引数 (`core.ptr.cast::<T, U>`) は呼び出し式の Path の 2 つ目の成分にある。
                    let targs = items(&nf(fnn)[1]).to_vec();
                    if let Some(out) = self.typed_ptr_op(&name, &args, &targs) {
                        return out;
                    }
                }
            }
            return self.call_resolved(&r, args);
        }
        let callee = self.eval(fnn, sc)?;
        let args = self.eval_args(&f[1], sc)?;
        self.call_value(&callee, args)
    }

    fn eval_args(&mut self, args: &V, sc: &Scope) -> Result<Vec<Val>, Ctl> {
        let mut out = vec![];
        for a in items(args) {
            let v = self.eval(a, sc)?;
            out.push(self.cp(v));
        }
        Ok(out)
    }

    /// `extern` の関数を、ホストの C ライブラリから探して呼ぶ。引数・戻り値は整数・`bool`・ポインタ・`f32` / `f64`。
    /// 宣言の型で引数の渡し方 (整数レジスタか浮動小数レジスタか) と戻り値の読み方を決める。
    fn extern_call(&mut self, d: &Def, args: Vec<Val>) -> Res {
        if args.len() != d.params.len() {
            return panic(format!("extern {}: argument count", d.name));
        }
        let mut ints = vec![];
        let mut floats = vec![];
        for (a, (_, pt)) in args.iter().zip(&d.params) {
            if matches!(a, Val::Ref(_)) && crate::check::tag(pt) == "ptr" {
                return panic(format!("extern {}: a pointer to a Kasane place (from from_mut / from_ref) cannot be passed to C", d.name));
            }
            let v = dr(a);
            match (arg_kind(pt), &v) {
                (Some(ArgK::Int), Val::Int(i, _)) => ints.push(*i as i64),
                (Some(ArgK::Int), Val::Bool(b)) => ints.push(*b as i64),
                (Some(ArgK::F64), Val::Float(x)) => floats.push(FArg::F64(*x)),
                (Some(ArgK::F64), Val::F32(x)) => floats.push(FArg::F64(*x as f64)),
                (Some(ArgK::F32), Val::F32(x)) => floats.push(FArg::F32(*x)),
                (Some(ArgK::F32), Val::Float(x)) => floats.push(FArg::F32(*x as f32)),
                // 整数の値を浮動小数の引数に渡すのは型が合わないので、下の `_` で panic (f32 ← f64 の変換だけ許す)。
                _ => return panic(format!("extern {}: argument of an unsupported type or value", d.name)),
            }
        }
        let ret = &d.ret;
        let rk = match crate::check::tag(ret).as_str() {
            "tuple" | "ptr" => Ret::Int,
            "never" => return panic(format!("extern {} returned although it is declared `!`", d.name)),
            "prim" => match crate::check::st(crate::check::tel(ret, 1)).as_str() {
                "f32" => Ret::F32,
                "f64" => Ret::F64,
                n if is_int_ty(n) || n == "bool" => Ret::Int,
                n => return panic(format!("extern {}: unsupported return type {}", d.name, n)),
            },
            _ => return panic(format!("extern {}: unsupported return type", d.name)),
        };
        let out = match crate::ffi::call_typed(&d.link, &ints, &floats, rk) {
            Ok(o) => o,
            Err(e) => return panic(format!("extern {}: {}", d.name, e)),
        };
        let r = match out {
            Out::F32(x) => return Ok(Val::F32(x)),
            Out::F64(x) => return Ok(Val::Float(x)),
            Out::Int(r) => r,
        };
        // 戻り値は、宣言の型に合わせて切り詰める。
        Ok(match crate::check::tag(ret).as_str() {
            "tuple" => Val::Unit,
            "ptr" => {
                // ポインタの要素型が分かる (スカラーの `*mut T`) なら、値に要素型を載せる。
                let elem = prim_name(&crate::check::tel(ret, 2));
                match elem.as_deref().and_then(ptr_tag) {
                    Some(t) => Val::Int((r as u64) as i128, Some(t)),
                    None => Val::Int((r as u64) as i128, Some("usize")),
                }
            }
            _ => {
                let n = crate::check::st(crate::check::tel(ret, 1));
                match n.as_str() {
                    "bool" => Val::Bool(r & 0xff != 0),
                    "i8" => int((r as i8) as i128, "i8"),
                    "i16" => int((r as i16) as i128, "i16"),
                    "i32" => int((r as i32) as i128, "i32"),
                    "i64" | "isize" => int(r as i128, &n),
                    "u8" => int((r as u8) as i128, "u8"),
                    "u16" => int((r as u16) as i128, "u16"),
                    "u32" => int((r as u32) as i128, "u32"),
                    "u64" | "usize" => int((r as u64) as i128, &n),
                    _ => return panic(format!("extern {}: unsupported return type {}", d.name, n)),
                }
            }
        })
    }

    /// 型引数や値の種類 (生アドレス / 場所) で動きが決まる `core.ptr.*`。対象外なら `None` (呼び出し側で通常の経路へ)。
    fn typed_ptr_op(&mut self, name: &str, args: &[Val], targs: &[V]) -> Option<Res> {
        let op = name.strip_prefix("core.ptr.")?;
        match (op, args.len()) {
            ("null_mut", 0) => {
                // 要素型が分かれば、null にも要素型を載せる (型引数が無ければ従来どおり usize の 0)。
                let tag = targs.first().and_then(prim_name).and_then(|n| ptr_tag(&n)).unwrap_or("usize");
                Some(Ok(Val::Int(0, Some(tag))))
            }
            ("from_mut", 1) => Some(Ok(match &args[0] {
                // `&mut x` は場所そのもの (Val::Ref) なので、同じ場所を指すポインタになる。
                // 値として渡された場合 (`&mut` でない式など) は、その値を入れた新しい場所を指す。
                Val::Ref(_) => args[0].clone(),
                v => Val::Ref(Rc::new(CellPlace(RefCell::new(v.clone())))),
            })),
            // `from_ref` は const の印を付ける。書き込みは `as_mut` を通してだけ許す。印は別名 (AliasPlace) に付けるので、
            // 元の場所のポインタ (from_mut など) には影響しない。
            ("from_ref", 1) => {
                let inner: Rc<dyn Place> = match &args[0] {
                    Val::Ref(p) => Rc::new(AliasPlace(p.clone())),
                    v => Rc::new(CellPlace(RefCell::new(v.clone()))),
                };
                Some(Ok(Val::Ref(self.mark_const(inner))))
            }
            ("cast", 1) => Some(self.cast_ptr(name, &args[0], targs)),
            _ => None,
        }
    }

    /// `core.ptr.cast::<T, U>(p)`。生アドレスは要素型の Ity だけ付け替える。場所を指すポインタは T と U が同じときだけ恒等 (幅の違う読み替えは再現できない)。
    fn cast_ptr(&mut self, name: &str, p: &Val, targs: &[V]) -> Res {
        let Some(to_t) = targs.get(1) else {
            return panic(format!("{}: the target type needs an explicit type argument (`{}::<T, U>`)", name, name));
        };
        let to = prim_name(to_t);
        match p {
            Val::Ref(_) => {
                let from = targs.first().and_then(prim_name);
                if from.is_some() && from == to {
                    Ok(p.clone())
                } else {
                    panic(format!("{}: a pointer to a Kasane place can only be cast to the same type in the interpreter", name))
                }
            }
            _ => match to.as_deref().and_then(ptr_tag) {
                Some(t) => match dr(p) {
                    Val::Int(x, _) => Ok(Val::Int(x, Some(t))),
                    _ => panic(format!("{}: the argument is not a raw pointer", name)),
                },
                None => panic(format!("{}: only scalar pointee types are supported by the interpreter", name)),
            },
        }
    }

    fn call_resolved(&mut self, r: &Resolved, args: Vec<Val>) -> Res {
        match &r.target {
            Target::Fn(i) => self.call_def(*i, args),
            Target::Native(n) => self.call_native(&n.clone(), args),
            Target::Variant(en, vn) => self.mk_variant(en, vn, args),
            Target::Type(i) => panic(format!("cannot call type {}", self.p.defs[*i].name)),
            Target::Sig(i) => {
                let d = self.def(*i);
                let parts: Vec<&str> = d.name.split('.').collect();
                if parts.len() >= 4 && parts[0] == "core" && matches!(parts[1], "num" | "bool" | "char") && PRIMS.contains(&parts[2]) {
                    if crate::natives::known_num(parts[3]) {
                        let n = format!("{}.{}", parts[2], parts[3]);
                        return self.call_native(&n, args);
                    }
                }
                if d.is_method() {
                    let mut it = args.into_iter();
                    let recv = it.next().unwrap();
                    return self.invoke_method(recv, parts[parts.len() - 1], it.collect(), None);
                }
                if d.kw == "extern" {
                    return self.extern_call(d, args);
                }
                if d.name == "core.ptr.null_mut" {
                    return Ok(int(0, "usize"));
                }
                if d.name == "core.ptr.is_null" {
                    // 場所を指すポインタ (`Val::Ref`) は null にならない。dr で中身を見ると誤判定する。
                    return Ok(Val::Bool(matches!(args.first(), Some(Val::Int(0, _)))));
                }
                if d.name.starts_with("core.ptr.") {
                    if let Some(Val::Ref(p)) = args.first() {
                        // 場所を指すポインタの const の扱い: `as_mut` は印を外した別名、`as_const` は印を付けた別名を返す。
                        // 印の付いた場所への `write` は panic (`from_ref` は読み取り専用)。
                        match d.name.as_str() {
                            "core.ptr.as_mut" => return Ok(Val::Ref(Rc::new(AliasPlace(p.clone())))),
                            "core.ptr.as_const" => {
                                let c = self.mark_const(Rc::new(AliasPlace(p.clone())));
                                return Ok(Val::Ref(c));
                            }
                            "core.ptr.write" if self.is_const_place(p) => {
                                return panic("core.ptr.write: the pointer is const (from from_ref or as_const); use as_mut to write through it");
                            }
                            _ => {}
                        }
                    }
                    return raw_ptr_op(&d.name, &args);
                }
                panic(format!("function {} has no implementation in this interpreter", d.name))
            }
            _ => panic("cannot call value"),
        }
    }

    // ------------------------------------------------------------ メソッド
    fn find_method(&self, v: &Val, name: &str) -> Option<usize> {
        match v {
            Val::Struct(s) => self.p.funcs.get(&format!("{}.{}", s.ty, name)).copied(),
            Val::Enum(e) => self.p.funcs.get(&format!("{}.{}", e.en, name)).copied(),
            _ => None,
        }
    }

    fn prim_kind(v: &Val) -> Option<&'static str> {
        match v {
            Val::Bool(_) => Some("bool"),
            Val::Int(_, t) => Some(t.unwrap_or("i32")),
            Val::Float(_) => Some("f64"),
            Val::F32(_) => Some("f32"),
            Val::Char(_) => Some("char"),
            Val::Str(_) => Some("str"),
            _ => None,
        }
    }

    pub fn invoke_method(&mut self, recv: Val, name: &str, args: Vec<Val>, place: Option<(&V, &Scope)>) -> Res {
        let v = dr(&recv);
        let mut d = self.find_method(&v, name);
        if d.is_none() && !matches!(v, Val::Struct(_) | Val::Enum(_)) {
            if let Some(kind) = Self::prim_kind(&v) {
                if let Some(cands) = self.p.method_idx.get(name) {
                    for &ci in cands {
                        let cd = &self.p.defs[ci];
                        if cd.body.is_none() {
                            continue;
                        }
                        let parts: Vec<&str> = cd.name.split('.').collect();
                        if parts.len() >= 3 && PRIMS.contains(&parts[parts.len() - 2]) && parts[parts.len() - 2] == kind {
                            d = Some(ci);
                            break;
                        }
                    }
                }
            }
        }
        if let Some(di) = d {
            let def = self.def(di);
            let selfty = &def.params[0].1;
            let by_mut = crate::check::tag(selfty) == "ref" && matches!(crate::check::tel(selfty, 1), V::B(true));
            let first = if by_mut {
                if matches!(recv, Val::Ref(_)) {
                    recv.clone()
                } else if let Some((pe, sc)) = place {
                    self.eval_place(pe, sc)?
                } else {
                    Val::Ref(Rc::new(CellPlace(RefCell::new(v.clone()))))
                }
            } else {
                v.clone()
            };
            let mut all = vec![first];
            all.extend(args);
            return self.call_def(di, all);
        }
        self.native_method(&recv, &v, name, args, place)
    }

    // ------------------------------------------------------------ イテレータ
    pub fn to_iter(&mut self, v: &Val) -> Result<Rc<RefCell<IterV>>, Ctl> {
        let v = dr(v);
        match &v {
            Val::Iter(it) => Ok(it.clone()),
            Val::Arr(l) | Val::Vec(l) => Ok(Rc::new(RefCell::new(IterV::List { items: l.clone(), i: 0, end: None }))),
            Val::View(l, a, b) => Ok(Rc::new(RefCell::new(IterV::List { items: l.clone(), i: *a, end: Some(*b) }))),
            Val::Struct(s) => match self.p.funcs.get(&format!("{}.next", s.ty)) {
                Some(&di) => Ok(Rc::new(RefCell::new(IterV::User { place: Rc::new(CellPlace(RefCell::new(v.clone()))), def: di }))),
                None => panic(format!("{} is not an iterator", s.ty)),
            },
            _ => panic("not an iterator"),
        }
    }

    pub fn iter_next(&mut self, it: &Rc<RefCell<IterV>>) -> Res {
        enum Step {
            Done(Val),
            Map(Rc<RefCell<IterV>>, Val),
            Filter(Rc<RefCell<IterV>>, Val),
            Enum(Rc<RefCell<IterV>>, i128),
            Take(Rc<RefCell<IterV>>),
            User(Rc<dyn Place>, usize),
        }
        let step = {
            let mut b = it.borrow_mut();
            match &mut *b {
                IterV::List { items, i, end } => {
                    let l = items.borrow();
                    let limit = end.unwrap_or(l.len()).min(l.len());
                    if *i >= limit {
                        Step::Done(none())
                    } else {
                        let x = l[*i].clone();
                        *i += 1;
                        Step::Done(some(x))
                    }
                }
                IterV::Pre { items, i } => {
                    if *i >= items.len() {
                        Step::Done(none())
                    } else {
                        let x = items[*i].clone();
                        *i += 1;
                        Step::Done(some(x))
                    }
                }
                IterV::Range { cur, end } => {
                    let (c, e) = (cur.clone(), end.clone());
                    match (&c, &e) {
                        (Val::Int(x, t), Val::Int(y, _)) => {
                            if x >= y {
                                Step::Done(none())
                            } else {
                                *cur = Val::Int(x + 1, *t);
                                Step::Done(some(c))
                            }
                        }
                        _ => Step::Done(none()),
                    }
                }
                IterV::Map { src, f } => Step::Map(src.clone(), f.clone()),
                IterV::Filter { src, f } => Step::Filter(src.clone(), f.clone()),
                IterV::Enumerate { src, i } => {
                    let idx = *i;
                    *i += 1;
                    Step::Enum(src.clone(), idx)
                }
                IterV::Take { src, n } => {
                    if *n <= 0 {
                        Step::Done(none())
                    } else {
                        *n -= 1;
                        Step::Take(src.clone())
                    }
                }
                IterV::User { place, def } => Step::User(place.clone(), *def),
            }
        };
        match step {
            Step::Done(v) => Ok(v),
            Step::Map(src, f) => {
                let o = self.iter_next(&src)?;
                match &o {
                    Val::Enum(e) if e.var == "Some" => {
                        let r = self.call_value(&f, vec![e.pl[0].clone()])?;
                        Ok(some(r))
                    }
                    _ => Ok(o),
                }
            }
            Step::Filter(src, f) => loop {
                let o = self.iter_next(&src)?;
                match &o {
                    Val::Enum(e) if e.var == "Some" => {
                        if matches!(dr(&self.call_value(&f, vec![e.pl[0].clone()])?), Val::Bool(true)) {
                            return Ok(o);
                        }
                    }
                    _ => return Ok(o),
                }
            },
            Step::Enum(src, idx) => {
                let o = self.iter_next(&src)?;
                match &o {
                    Val::Enum(e) if e.var == "Some" => {
                        Ok(some(Val::Tuple(Rc::new(vec![int(idx, "usize"), e.pl[0].clone()]))))
                    }
                    _ => Ok(o),
                }
            }
            Step::Take(src) => self.iter_next(&src),
            Step::User(place, di) => self.call_def(di, vec![Val::Ref(place)]),
        }
    }

    // ------------------------------------------------------------ 表示
    pub fn debug_str(&mut self, v: &Val) -> Result<String, Ctl> {
        let v = dr(v);
        Ok(match &v {
            Val::Bool(b) => (if *b { "true" } else { "false" }).into(),
            Val::Char(c) => format!("{:?}", c),
            Val::Str(s) => py_quote(s),
            Val::StrBuf(b) => py_quote(&b.borrow()),
            Val::Float(f) => py_float(*f),
            Val::F32(f) => py_float32(*f),
            Val::Int(x, t) => {
                if *t == Some("u128") { (*x as u128).to_string() } else { x.to_string() }
            }
            Val::Unit => "()".into(),
            Val::Tuple(t) => {
                let parts: Result<Vec<String>, Ctl> = t.iter().map(|x| self.debug_str(x)).collect();
                let parts = parts?;
                if parts.len() == 1 { format!("({},)", parts[0]) } else { format!("({})", parts.join(", ")) }
            }
            Val::Arr(_) | Val::Vec(_) | Val::View(..) => {
                let items = seq_items(&v).unwrap_or_default();
                let parts: Result<Vec<String>, Ctl> = items.iter().map(|x| self.debug_str(x)).collect();
                format!("[{}]", parts?.join(", "))
            }
            Val::Struct(s) => {
                if let Some(&di) = self.p.funcs.get(&format!("{}.debug_fmt", s.ty)) {
                    let fm = Rc::new(RefCell::new(String::new()));
                    self.call_def(di, vec![v.clone(), Val::Fmt(fm.clone())])?;
                    let r = fm.borrow().clone();
                    return Ok(r);
                }
                let nm = s.ty.rsplit('.').next().unwrap().to_string();
                let fields: Vec<(String, Val)> = s.f.borrow().clone();
                let mut parts = vec![];
                for (k, x) in fields {
                    parts.push(format!("{}: {}", k, self.debug_str(&x)?));
                }
                format!("{} {{ {} }}", nm, parts.join(", "))
            }
            Val::Enum(e) => {
                if let Some(&di) = self.p.funcs.get(&format!("{}.debug_fmt", e.en)) {
                    let fm = Rc::new(RefCell::new(String::new()));
                    self.call_def(di, vec![v.clone(), Val::Fmt(fm.clone())])?;
                    let r = fm.borrow().clone();
                    return Ok(r);
                }
                if e.pl.is_empty() {
                    e.var.clone()
                } else {
                    let parts: Result<Vec<String>, Ctl> = e.pl.iter().map(|x| self.debug_str(x)).collect();
                    format!("{}({})", e.var, parts?.join(", "))
                }
            }
            _ => "<value>".into(),
        })
    }

    pub fn display_str(&mut self, v: &Val) -> Result<String, Ctl> {
        let v = dr(v);
        if let Some(s) = str_of(&v) {
            return Ok(s);
        }
        if let Val::Struct(s) = &v {
            if let Some(&di) = self.p.funcs.get(&format!("{}.display_fmt", s.ty)) {
                let fm = Rc::new(RefCell::new(String::new()));
                self.call_def(di, vec![v.clone(), Val::Fmt(fm.clone())])?;
                let r = fm.borrow().clone();
                return Ok(r);
            }
        }
        self.debug_str(&v)
    }

    pub fn write_out(&mut self, s: &str) {
        let _ = self.out.write_all(s.as_bytes());
    }

    // ------------------------------------------------------------ 実行
    pub fn run_entry(&mut self, di: usize) -> i32 {
        let r = self.call_def(di, vec![]);
        let code = match r {
            Err(Ctl::Panic(m)) => {
                let _ = self.out.flush();
                eprintln!("panic: {}", m);
                return 101;
            }
            Err(Ctl::Exit(c)) => c,
            Err(_) => 0,
            Ok(v) => {
                let v = dr(&v);
                if let Val::Enum(e) = &v {
                    if e.en == "Result" && e.var == "Err" {
                        let _ = self.out.flush();
                        let s = self.debug_str(&e.pl[0]).unwrap_or_default();
                        eprintln!("Error: {}", s);
                        return 1;
                    }
                }
                0
            }
        };
        let _ = self.out.flush();
        code
    }
}

pub fn py_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

pub fn py_float32(f: f32) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == f.trunc() && f.abs() < 1e16 {
        return format!("{:.1}", f);
    }
    format!("{}", f)
}

pub fn py_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == f.trunc() && f.abs() < 1e16 {
        return format!("{:.1}", f);
    }
    format!("{}", f)
}

#[allow(dead_code)]
fn unused() {
    let _ = _children;
}

// ------------------------------------------------------------ extern の引数・生ポインタ
// 生ポインタは `Val::Int` のアドレスで、要素型を Ity の名前 (`*f64` など) に載せる。
// 要素型が載らない (extern の戻り値が `*mut Struct` など) ポインタは、読み書きできない。

/// 生ポインタの要素型と、ポインタ値の Ity 名の対応。扱うのはスカラーだけ。
const PTR_TAGS: &[(&str, &str)] = &[
    ("i8", "*i8"),
    ("i16", "*i16"),
    ("i32", "*i32"),
    ("i64", "*i64"),
    ("isize", "*isize"),
    ("u8", "*u8"),
    ("u16", "*u16"),
    ("u32", "*u32"),
    ("u64", "*u64"),
    ("usize", "*usize"),
    ("f32", "*f32"),
    ("f64", "*f64"),
    ("bool", "*bool"),
];

fn ptr_tag(elem: &str) -> Option<&'static str> {
    PTR_TAGS.iter().find(|(e, _)| *e == elem).map(|(_, t)| *t)
}

fn ptr_elem(tag: &str) -> Option<&'static str> {
    PTR_TAGS.iter().find(|(_, t)| *t == tag).map(|(e, _)| *e)
}

fn elem_size(elem: &str) -> u64 {
    match elem {
        "i8" | "u8" | "bool" => 1,
        "i16" | "u16" => 2,
        "i32" | "u32" | "f32" => 4,
        _ => 8,
    }
}

/// 型が `prim` (スカラー名) なら、その名前。
fn prim_name(t: &V) -> Option<String> {
    (crate::check::tag(t) == "prim").then(|| crate::check::st(crate::check::tel(t, 1)))
}

/// extern の引数を渡すレジスタの種類。
#[derive(Clone, Copy)]
enum ArgK {
    Int,
    F32,
    F64,
}

fn arg_kind(t: &V) -> Option<ArgK> {
    match crate::check::tag(t).as_str() {
        "ptr" => Some(ArgK::Int),
        "prim" => match crate::check::st(crate::check::tel(t, 1)).as_str() {
            "f32" => Some(ArgK::F32),
            "f64" => Some(ArgK::F64),
            n if is_int_ty(n) || n == "bool" => Some(ArgK::Int),
            _ => None,
        },
        _ => None,
    }
}

/// 生ポインタの値から (アドレス, 要素型) を取る。
fn raw_ptr(v: &Val) -> Result<(u64, &'static str), String> {
    match dr(v) {
        Val::Int(a, Some(t)) => match ptr_elem(t) {
            Some(e) => Ok((a as u64, e)),
            None => Err("the pointer's element type is unknown to the interpreter (a pointer from an extern declaration or core.ptr.null_mut::<T>() / core.ptr.cast::<T, U>() with a scalar T)".into()),
        },
        _ => Err("the argument is not a raw pointer".into()),
    }
}

/// 引数が `&e` ひとつだけなら、`e` を返す。
fn ref_arg_inner(args: &V) -> Option<&V> {
    let [a] = items(args) else { return None };
    (nname(a) == "Unary" && st(&nf(a)[0]) == "&").then(|| &nf(a)[1])
}

/// 同じ場所を別の名前で指すポインタ (`as_mut` / `as_const` / `from_ref` の結果)。読み書きは元の場所に通す。
/// const の印は、この別名の `Rc` の番地に付く (`Interp::mark_const`)。
struct AliasPlace(Rc<dyn Place>);
impl Place for AliasPlace {
    fn get(&self) -> Val {
        self.0.get()
    }
    fn set(&self, v: Val) {
        self.0.set(v)
    }
}

/// 場所を指すポインタ (`from_mut` / `from_ref` の結果) の操作。アドレスを持たないので、読み書きは場所を通して行う。
/// `as_mut` / `as_const` と、印の付いた場所への `write` は呼び出し側 (`call_resolved`) で扱う。
fn place_ptr_op(name: &str, op: &str, p: &dyn Place, args: &[Val]) -> Res {
    match (op, args.len()) {
        ("read", 1) => Ok(p.get()),
        ("write", 2) => {
            p.set(args[1].clone());
            Ok(Val::Unit)
        }
        ("is_null", 1) => Ok(Val::Bool(false)),
        // 要素が 1 つしかないので、0 だけ進める加算は恒等。それ以外は配列の要素を指さないので扱えない。
        ("add", 2) if matches!(dr(&args[1]), Val::Int(0, _)) => Ok(args[0].clone()),
        _ => panic(format!(
            "{} is not supported for a pointer to a single place (only read, write, as_const, as_mut, is_null, add by 0)",
            name
        )),
    }
}

/// `core.ptr.*` の操作 (`null_mut` / `is_null` は呼び出し側で扱う)。読み書きはホストのメモリに直接触れる。
fn raw_ptr_op(name: &str, args: &[Val]) -> Res {
    let op = name.strip_prefix("core.ptr.").unwrap_or(name);
    if let Some(Val::Ref(p)) = args.first() {
        // 場所を指すポインタは dr で中身に潜らず、場所として扱う。
        return place_ptr_op(name, op, p.as_ref(), args);
    }
    match (op, args.len()) {
        ("as_const", 1) | ("as_mut", 1) => Ok(args[0].clone()),
        ("add", 2) => {
            let (a, e) = match raw_ptr(&args[0]) {
                Ok(x) => x,
                Err(m) => return panic(format!("{}: {}", name, m)),
            };
            let n = match dr(&args[1]) {
                Val::Int(n, _) => n as u64,
                _ => return panic(format!("{}: the count is not an integer", name)),
            };
            let r = a.wrapping_add(n.wrapping_mul(elem_size(e)));
            Ok(Val::Int(r as i128, Some(ptr_tag(e).unwrap_or("usize"))))
        }
        ("read", 1) => {
            if matches!(args[0], Val::Int(0, _)) {
                return panic(format!("{}: null pointer dereference", name));
            }
            let (a, e) = match raw_ptr(&args[0]) {
                Ok(x) => x,
                Err(m) => return panic(format!("{}: {}", name, m)),
            };
            Ok(unsafe { read_elem(a, e) })
        }
        ("write", 2) => {
            if matches!(args[0], Val::Int(0, _)) {
                return panic(format!("{}: null pointer dereference", name));
            }
            let (a, e) = match raw_ptr(&args[0]) {
                Ok(x) => x,
                Err(m) => return panic(format!("{}: {}", name, m)),
            };
            if let Err(m) = unsafe { write_elem(a, e, &dr(&args[1])) } {
                return panic(format!("{}: {}", name, m));
            }
            Ok(Val::Unit)
        }
        _ => panic(format!(
            "{} is not supported by the interpreter (raw pointer memory); use `ksn build --backend clif`",
            name
        )),
    }
}

/// 要素型 `elem` の値を、アドレス `addr` から読む。
///
/// # Safety
/// `addr` は読んでよい `elem` の領域を指していること (extern が返したポインタの責任)。
unsafe fn read_elem(addr: u64, elem: &'static str) -> Val {
    let p = addr as usize as *const u8;
    match elem {
        "i8" => Val::Int(std::ptr::read_unaligned(p as *const i8) as i128, Some("i8")),
        "i16" => Val::Int(std::ptr::read_unaligned(p as *const i16) as i128, Some("i16")),
        "i32" => Val::Int(std::ptr::read_unaligned(p as *const i32) as i128, Some("i32")),
        "i64" => Val::Int(std::ptr::read_unaligned(p as *const i64) as i128, Some("i64")),
        "isize" => Val::Int(std::ptr::read_unaligned(p as *const isize) as i128, Some("isize")),
        "u8" => Val::Int(std::ptr::read_unaligned(p) as i128, Some("u8")),
        "u16" => Val::Int(std::ptr::read_unaligned(p as *const u16) as i128, Some("u16")),
        "u32" => Val::Int(std::ptr::read_unaligned(p as *const u32) as i128, Some("u32")),
        "u64" => Val::Int(std::ptr::read_unaligned(p as *const u64) as i128, Some("u64")),
        "usize" => Val::Int(std::ptr::read_unaligned(p as *const usize) as i128, Some("usize")),
        "f32" => Val::F32(std::ptr::read_unaligned(p as *const f32)),
        "f64" => Val::Float(std::ptr::read_unaligned(p as *const f64)),
        _ => Val::Bool(std::ptr::read_unaligned(p) != 0),
    }
}

/// 要素型 `elem` の値を、アドレス `addr` へ書く。整数は要素の幅に切り詰める。
///
/// # Safety
/// `addr` は書いてよい `elem` の領域を指していること。
unsafe fn write_elem(addr: u64, elem: &str, v: &Val) -> Result<(), String> {
    let p = addr as usize as *mut u8;
    let int: Option<i128> = match v {
        Val::Int(x, _) => Some(*x),
        Val::Bool(b) => Some(*b as i128),
        _ => None,
    };
    let flt: Option<f64> = match v {
        Val::Float(x) => Some(*x),
        Val::F32(x) => Some(*x as f64),
        Val::Int(x, _) => Some(*x as f64),
        _ => None,
    };
    let not_scalar = || Err("the value to write is not a scalar".to_string());
    match elem {
        "f32" | "f64" => {
            let Some(x) = flt else { return not_scalar() };
            if elem == "f32" {
                std::ptr::write_unaligned(p as *mut f32, x as f32);
            } else {
                std::ptr::write_unaligned(p as *mut f64, x);
            }
        }
        _ => {
            let Some(x) = int else { return not_scalar() };
            match elem {
                "bool" => std::ptr::write_unaligned(p, (x != 0) as u8),
                "i8" | "u8" => std::ptr::write_unaligned(p, x as u8),
                "i16" | "u16" => std::ptr::write_unaligned(p as *mut u16, x as u16),
                "i32" | "u32" => std::ptr::write_unaligned(p as *mut u32, x as u32),
                _ => std::ptr::write_unaligned(p as *mut u64, x as u64),
            }
        }
    }
    Ok(())
}

/// 整数の `+ - * / %`。オーバーフローとゼロ除算は panic (4.1)。u128 はビットパターンで保持している。
fn int_arith(op: &str, x: i128, y: i128, ty: Ity) -> Res {
    let name = match op {
        "+" => "add",
        "-" => "subtract",
        "*" => "multiply",
        "/" => "divide",
        _ => "calculate the remainder",
    };
    if (op == "/" || op == "%") && y == 0 {
        return panic(format!("attempt to {} by zero", if op == "/" { "divide" } else { "calculate the remainder with a divisor of" }));
    }
    let overflow = || panic(format!("attempt to {} with overflow", name));
    if ty == Some("u128") {
        let (a, b) = (x as u128, y as u128);
        let r = match op {
            "+" => a.checked_add(b),
            "-" => a.checked_sub(b),
            "*" => a.checked_mul(b),
            "/" => a.checked_div(b),
            _ => a.checked_rem(b),
        };
        return match r {
            Some(v) => Ok(Val::Int(v as i128, ty)),
            None => overflow(),
        };
    }
    let r = match op {
        "+" => x.checked_add(y),
        "-" => x.checked_sub(y),
        "*" => x.checked_mul(y),
        "/" => x.checked_div(y),
        _ => x.checked_rem(y),
    };
    match (r, ty) {
        (Some(v), Some(t)) if v >= tmin(t) && v <= tmax(t) => Ok(Val::Int(v, ty)),
        (Some(v), None) => Ok(Val::Int(v, ty)),
        _ => overflow(),
    }
}
