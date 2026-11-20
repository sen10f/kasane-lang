//! インタプリタの FFI の本体 (`interp.rs` の子モジュール: `Interp` の非公開の状態を触る)。
//!
//! - `layout c` の構造体の値 (`Val::Struct`) と C のメモリ配置 (`ffi::CTy`) の相互変換
//! - `extern` の呼び出し (System V AMD64 の分類で構造体も値渡し・値返し)、`link "lib:name"` の動的ロード
//! - コールバック: Kasane の関数 / キャプチャなしのクロージャを C の関数ポインタにして渡す
//! - 生ポインタが指す Kasane の配列・構造体・変数 (`core.ptr.from_mut` / `from_ref`) の、C のメモリへの写し (ピン)

use super::*;
use crate::ffi::{self, ArgV, CTy, Pass, RetTy, RetV};

/// 配列・変数・構造体を C に見せるときの写し (ピン)。C の呼び出しの前に Kasane の値から書き込み、
/// 呼び出しのあとに C のメモリから Kasane の値へ書き戻す。
pub struct Pin {
    addr: usize,
    buf: Vec<u64>,
    elem: CTy,
    count: usize,
    target: PinTarget,
}

enum PinTarget {
    List(Rc<RefCell<Vec<Val>>>),
    /// 部分スライス (`Val::View`): 元の配列の `[from, to)` だけを C へ渡す。書き戻しは同じ範囲へ。
    Range(Rc<RefCell<Vec<Val>>>, usize, usize),
    One(Rc<dyn Place>),
}

/// コールバックの引数・戻り値の種類。`Agg` は `layout c` の構造体を値で渡す/返す。
#[derive(Clone)]
enum CbTy {
    Int(&'static str),
    Ptr,
    F32,
    F64,
    Unit,
    Agg(CTy),
}

pub struct CbSlot {
    key: usize,
    f: Val,
    params: Vec<(CbTy, V)>,
    ret: CbTy,
}

thread_local! {
    /// いま `extern` を呼んでいるインタプリタ (コールバックの入口が戻ってくる先)。
    static CUR: std::cell::Cell<*mut Interp> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}

fn scalar_name(p: &str) -> Option<&'static str> {
    Some(match p {
        "i8" => "i8",
        "i16" => "i16",
        "i32" => "i32",
        "i64" => "i64",
        "isize" => "isize",
        "u8" => "u8",
        "u16" => "u16",
        "u32" => "u32",
        "u64" => "u64",
        "usize" => "usize",
        "bool" => "bool",
        "char" => "char",
        "f32" => "f32",
        "f64" => "f64",
        _ => return None,
    })
}

fn cty_name(t: &CTy) -> String {
    match t {
        CTy::Scalar("ptr") => "usize".into(),
        CTy::Scalar(n) => n.to_string(),
        CTy::Struct(s) => s.name.clone(),
        CTy::Array(..) => "array".into(),
    }
}

fn put_int(out: &mut [u8], size: usize, v: i128) {
    out[..size].copy_from_slice(&v.to_le_bytes()[..size]);
}

fn get_u(b: &[u8], size: usize) -> u64 {
    let mut x = [0u8; 8];
    x[..size].copy_from_slice(&b[..size]);
    u64::from_le_bytes(x)
}

impl Interp {
    // ------------------------------------------------------------ 型 → C の配置

    /// 型の名前 (`ptr_elem` の値、または宣言の中の名前) を、定義の完全修飾名にする。
    pub(super) fn type_fqn(&self, name: &str, file: &str) -> Option<String> {
        if self.p.types.contains_key(name) {
            return Some(name.to_string());
        }
        let segs: Vec<String> = name.split('.').map(String::from).collect();
        let full = self.p.expand(&segs, file).join(".");
        if self.p.types.contains_key(&full) {
            return Some(full);
        }
        // 名前だけ (短い形) で一意に決まるとき
        let tail = format!(".{}", name);
        let mut hit = self.p.types.keys().filter(|k| k.ends_with(&tail));
        match (hit.next(), hit.next()) {
            (Some(k), None) => Some(k.clone()),
            _ => None,
        }
    }

    pub(super) fn cty_of(&self, t: &V, file: &str) -> Result<CTy, String> {
        use crate::check::{tag, tel};
        match tag(t).as_str() {
            "prim" => {
                let p = st(tel(t, 1));
                scalar_name(&p).map(CTy::Scalar).ok_or_else(|| format!("the type `{}` is not a C type", p))
            }
            "ptr" | "fn" => Ok(CTy::Scalar("ptr")),
            "array" => {
                let e = self.cty_of(tel(t, 1), file)?;
                match tel(t, 2) {
                    V::N(n) => {
                        let n: u32 = n.parse().map_err(|_| "array length is not a literal".to_string())?;
                        Ok(CTy::Array(Rc::new(e), n))
                    }
                    _ => Err("an array length in a `layout c` type must be an integer literal".into()),
                }
            }
            "named" => {
                let name = st(tel(t, 1));
                let fqn = self.type_fqn(&name, file).ok_or_else(|| format!("the type `{}` is not a C type", name))?;
                self.cty_of_fqn(&fqn, 0)
            }
            _ => Err("the type is not a C type".into()),
        }
    }

    pub(super) fn cty_of_fqn(&self, fqn: &str, depth: u32) -> Result<CTy, String> {
        if depth > 16 {
            return Err(format!("the struct `{}` is too deeply nested", fqn));
        }
        let d = &self.p.defs[self.p.types[fqn]];
        if d.kw != "type" || !d.layout_c {
            return Err(format!("`{}` is not a `layout c` struct, so it cannot cross the C boundary", fqn));
        }
        let mut fields = vec![];
        for (n, t, _) in &d.fields {
            fields.push((n.clone(), self.cty_of(t, &d.file)?));
        }
        Ok(CTy::Struct(Rc::new(ffi::make_struct(fqn, fields)?)))
    }

    /// `ptr_elem` の要素型名 (スカラー名か構造体の完全修飾名) の C の型。
    pub(super) fn cty_of_name(&self, name: &str) -> Result<CTy, String> {
        if let Some(n) = scalar_name(name) {
            return Ok(CTy::Scalar(n));
        }
        let fqn = self.type_fqn(name, "").ok_or_else(|| format!("unknown element type `{}`", name))?;
        self.cty_of_fqn(&fqn, 0)
    }

    // ------------------------------------------------------------ 値 <-> メモリ

    pub(super) fn val_to_mem(&self, v: &Val, t: &CTy, out: &mut [u8]) -> Result<(), String> {
        let v = dr(v);
        match t {
            CTy::Scalar(n) => {
                let size = ffi::scalar_size(n) as usize;
                match (&v, *n) {
                    (Val::Int(x, _), "f32") => out[..4].copy_from_slice(&(*x as f32).to_le_bytes()),
                    (Val::Int(x, _), "f64") => out[..8].copy_from_slice(&(*x as f64).to_le_bytes()),
                    (Val::Int(x, _), _) => put_int(out, size, *x),
                    (Val::Bool(b), _) => put_int(out, size, *b as i128),
                    (Val::Char(c), _) => put_int(out, size, *c as i128),
                    (Val::F32(f), "f64") => out[..8].copy_from_slice(&(*f as f64).to_le_bytes()),
                    (Val::F32(f), _) => out[..4].copy_from_slice(&f.to_le_bytes()),
                    (Val::Float(f), "f32") => out[..4].copy_from_slice(&(*f as f32).to_le_bytes()),
                    (Val::Float(f), _) => out[..8].copy_from_slice(&f.to_le_bytes()),
                    _ => return Err("a value that is not a C scalar was written to C memory".into()),
                }
                Ok(())
            }
            CTy::Struct(s) => {
                let Val::Struct(sv) = &v else { return Err(format!("expected a `{}` value", s.name)) };
                for (n, off, ft) in &s.fields {
                    let fv = sv.get(n).ok_or_else(|| format!("the field `{}` of `{}` is missing", n, s.name))?;
                    self.val_to_mem(&fv, ft, &mut out[*off as usize..])?;
                }
                Ok(())
            }
            CTy::Array(e, n) => {
                let items = seq_items(&v).ok_or("expected an array value")?;
                if items.len() != *n as usize {
                    return Err(format!("expected an array of {} elements, got {}", n, items.len()));
                }
                let es = e.size() as usize;
                for (i, x) in items.iter().enumerate() {
                    self.val_to_mem(x, e, &mut out[i * es..])?;
                }
                Ok(())
            }
        }
    }

    pub(super) fn val_from_mem(&mut self, t: &CTy, b: &[u8]) -> Val {
        match t {
            CTy::Scalar(n) => {
                let size = ffi::scalar_size(n) as usize;
                let u = get_u(b, size);
                match *n {
                    "bool" => Val::Bool(u & 0xff != 0),
                    "i8" => int((u as u8 as i8) as i128, "i8"),
                    "i16" => int((u as u16 as i16) as i128, "i16"),
                    "i32" => int((u as u32 as i32) as i128, "i32"),
                    "i64" | "isize" => int((u as i64) as i128, n),
                    "u8" | "u16" | "u32" | "u64" | "usize" => int(u as i128, n),
                    "char" => Val::Char(char::from_u32(u as u32).unwrap_or('\0')),
                    "f32" => Val::F32(f32::from_bits(u as u32)),
                    "f64" => Val::Float(f64::from_bits(u)),
                    _ => int(u as i128, "usize"),
                }
            }
            CTy::Struct(s) => {
                let mut fields = vec![];
                for (n, off, ft) in &s.fields {
                    let v = self.val_from_mem(ft, &b[*off as usize..]);
                    fields.push((n.clone(), v));
                }
                self.mk_struct(s.name.clone(), fields)
            }
            CTy::Array(e, n) => {
                let es = e.size() as usize;
                let items = (0..*n as usize).map(|i| self.val_from_mem(e, &b[i * es..])).collect();
                Val::Arr(new_list(items))
            }
        }
    }

    /// アドレスから要素 1 つぶんを読む (構造体を含む)。
    pub(super) fn read_elem(&mut self, name: &str, p: i128, elem: &str) -> Res {
        if scalar_name(elem).is_some() || self.type_fqn(elem, "").is_none() {
            return unsafe { read_scalar(name, p, elem) };
        }
        let t = match self.cty_of_name(elem) {
            Ok(t) => t,
            Err(e) => return panic(format!("{}: {}", name, e)),
        };
        let bytes = unsafe { std::slice::from_raw_parts(p as usize as *const u8, t.size() as usize) }.to_vec();
        Ok(self.val_from_mem(&t, &bytes))
    }

    pub(super) fn write_elem(&mut self, name: &str, p: i128, elem: &str, v: &Val) -> Result<(), Ctl> {
        if scalar_name(elem).is_some() || self.type_fqn(elem, "").is_none() {
            return unsafe { write_scalar(name, p, elem, v) };
        }
        let t = match self.cty_of_name(elem) {
            Ok(t) => t,
            Err(e) => return panic(format!("{}: {}", name, e)),
        };
        let mut buf = vec![0u8; t.size() as usize];
        if let Err(e) = self.val_to_mem(v, &t, &mut buf) {
            return panic(format!("{}: {}", name, e));
        }
        unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), p as usize as *mut u8, buf.len()) };
        Ok(())
    }

    /// 要素 1 つのバイト数 (スカラーと `layout c` の構造体)。
    pub(super) fn elem_bytes(&self, name: &str, elem: &str) -> Result<i128, Ctl> {
        if let Some(n) = scalar_name(elem) {
            return Ok(ffi::scalar_size(n) as i128);
        }
        if matches!(elem, "i128" | "u128") {
            return Ok(16);
        }
        match self.cty_of_name(elem) {
            Ok(t) => Ok(t.size() as i128),
            Err(_) => panic(unsupported_elem(name, elem)),
        }
    }

    // ------------------------------------------------------------ ピン

    fn cty_of_val(&self, v: &Val) -> Option<CTy> {
        Some(match as_raw(&dr(v)) {
            Val::Int(_, t) => CTy::Scalar(scalar_name(t.unwrap_or("i32"))?),
            Val::Bool(_) => CTy::Scalar("bool"),
            Val::Char(_) => CTy::Scalar("char"),
            Val::F32(_) => CTy::Scalar("f32"),
            Val::Float(_) => CTy::Scalar("f64"),
            Val::Struct(s) => self.cty_of_fqn(&s.ty, 0).ok()?,
            Val::Arr(l) | Val::Vec(l) => {
                let first = l.borrow().first().cloned()?;
                CTy::Array(Rc::new(self.cty_of_val(&first)?), l.borrow().len() as u32)
            }
            _ => return None,
        })
    }

    /// `core.ptr.from_mut` / `from_ref`: Kasane の配列・構造体・変数を指す生ポインタを作る。C のメモリへ写して、
    /// そのアドレスを返す (C の呼び出しの前後と `core.ptr.read` / `write` で、元の値と同期する)。
    pub(super) fn make_pin(&mut self, name: &str, arg: &Val) -> Res {
        let raw_arg = as_raw(arg);
        let (target, cur) = match &raw_arg {
            Val::Ref(p) => {
                let cur = as_raw(&dr(&p.get()));
                match &cur {
                    Val::Arr(l) | Val::Vec(l) => (PinTarget::List(l.clone()), cur.clone()),
                    Val::View(l, a, b) => (PinTarget::Range(l.clone(), *a, *b), cur.clone()),
                    _ => (PinTarget::One(p.clone()), cur),
                }
            }
            Val::Arr(l) | Val::Vec(l) => (PinTarget::List(l.clone()), raw_arg.clone()),
            Val::View(l, a, b) => (PinTarget::Range(l.clone(), *a, *b), raw_arg.clone()),
            other => (PinTarget::One(Rc::new(CellPlace(RefCell::new(other.clone())))), other.clone()),
        };
        let (elem, count) = match (&target, &cur) {
            (PinTarget::List(l), _) => {
                let first = l.borrow().first().cloned();
                match first.and_then(|f| self.cty_of_val(&f)) {
                    Some(e) => (e, l.borrow().len()),
                    None => return panic(format!("{}: the interpreter cannot pass an empty or non-C array to C", name)),
                }
            }
            (PinTarget::Range(l, a, b), _) => {
                let first = l.borrow().get(*a).filter(|_| a < b).cloned();
                match first.and_then(|f| self.cty_of_val(&f)) {
                    Some(e) => (e, b.saturating_sub(*a)),
                    None => return panic(format!("{}: the interpreter cannot pass an empty or non-C slice to C", name)),
                }
            }
            (_, v) => match self.cty_of_val(v) {
                Some(e) => (e, 1),
                None => return panic(format!("{}: the value is not a C type (a `layout c` struct, a number or an array of them)", name)),
            },
        };
        // 同じ配列・変数のピンは使い回す
        let same = |p: &Pin| match (&p.target, &target) {
            (PinTarget::List(a), PinTarget::List(b)) => Rc::ptr_eq(a, b) && p.count == count,
            (PinTarget::Range(a, a0, a1), PinTarget::Range(b, b0, b1)) => Rc::ptr_eq(a, b) && (a0, a1) == (b0, b1) && p.count == count,
            (PinTarget::One(a), PinTarget::One(b)) => Rc::ptr_eq(a, b) || a.key().is_some_and(|k| Some(k) == b.key()),
            _ => false,
        };
        let idx = match self.pins.iter().position(same) {
            Some(i) => i,
            None => {
                let words = (elem.size() as usize * count).div_ceil(8) + 2;
                let mut pin = Pin { addr: 0, buf: vec![0u64; words], elem: elem.clone(), count, target };
                pin.addr = pin.buf.as_mut_ptr() as usize;
                self.pins.push(pin);
                self.pins.len() - 1
            }
        };
        if let Err(e) = self.pin_push(idx) {
            return panic(format!("{}: {}", name, e));
        }
        let addr = self.pins[idx].addr as i128;
        self.ptr_elem.borrow_mut().insert(addr, cty_name(&elem));
        Ok(int(addr, "usize"))
    }

    fn pin_bytes(&mut self, idx: usize) -> &mut [u8] {
        let p = &mut self.pins[idx];
        let n = p.elem.size() as usize * p.count;
        unsafe { std::slice::from_raw_parts_mut(p.buf.as_mut_ptr() as *mut u8, n) }
    }

    /// Kasane の値 → C のメモリ
    fn pin_push(&mut self, idx: usize) -> Result<(), String> {
        let (elem, vals): (CTy, Vec<Val>) = {
            let p = &self.pins[idx];
            let vals = match &p.target {
                PinTarget::List(l) => l.borrow().clone(),
                PinTarget::Range(l, a, b) => l.borrow().get(*a..*b).map(|s| s.to_vec()).unwrap_or_default(),
                PinTarget::One(pl) => vec![pl.get()],
            };
            (p.elem.clone(), vals)
        };
        let es = elem.size() as usize;
        let mut tmp = vec![0u8; es * vals.len()];
        for (i, v) in vals.iter().enumerate() {
            self.val_to_mem(v, &elem, &mut tmp[i * es..])?;
        }
        self.pin_bytes(idx)[..tmp.len()].copy_from_slice(&tmp);
        Ok(())
    }

    /// C のメモリ → Kasane の値
    fn pin_pull(&mut self, idx: usize) {
        let (elem, count) = (self.pins[idx].elem.clone(), self.pins[idx].count);
        let es = elem.size() as usize;
        let bytes = self.pin_bytes(idx).to_vec();
        let vals: Vec<Val> = (0..count).map(|i| self.val_from_mem(&elem, &bytes[i * es..])).collect();
        match &self.pins[idx].target {
            PinTarget::List(l) => {
                let mut l = l.borrow_mut();
                for (i, v) in vals.into_iter().enumerate() {
                    if i < l.len() {
                        l[i] = v;
                    }
                }
            }
            PinTarget::Range(l, a, _) => {
                let mut l = l.borrow_mut();
                for (i, v) in vals.into_iter().enumerate() {
                    if a + i < l.len() {
                        l[a + i] = v;
                    }
                }
            }
            PinTarget::One(pl) => {
                if let Some(v) = vals.into_iter().next() {
                    // 構造体は、同じ値 (Rc) のフィールドを書き換える (他の参照からも見える)
                    match (&dr(&pl.get()), &v) {
                        (Val::Struct(old), Val::Struct(new)) => {
                            for (k, x) in new.f.borrow().iter() {
                                old.set(k, x.clone());
                            }
                        }
                        _ => pl.set(v),
                    }
                }
            }
        }
    }

    /// アドレスが、ピンの範囲に入るならそのピンの番号。
    pub(super) fn pin_at(&self, a: i128) -> Option<usize> {
        let a = a as usize;
        self.pins.iter().position(|p| a >= p.addr && a < p.addr + (p.elem.size() as usize * p.count).max(1))
    }

    /// `read` / `write` の番地が、ピンの確保範囲 (C へ渡す写しのバッファ) の中なら、要素 (`elem`) の幅が値の範囲に
    /// 収まるかを確かめる。収まらなければ panic (値の後ろの余白は、読むと不定の値、書くと別の値を壊す)。
    /// 番地はバッファの中まで見る (`add` で値の末尾ちょうどへ進んだ番地も対象)。
    pub(super) fn pin_check(&self, name: &str, a: i128, elem: &str) -> Result<(), Ctl> {
        let au = a as usize;
        let Some(p) = self.pins.iter().find(|p| au >= p.addr && au < p.addr + p.buf.len() * 8) else {
            return Ok(());
        };
        let w = self.elem_bytes(name, elem)? as usize;
        let size = p.elem.size() as usize * p.count;
        if au + w > p.addr + size {
            return panic(format!(
                "{}: a {}-byte element at {:#x} runs past the {}-byte value passed to C (from {:#x})",
                name, w, au, size, p.addr
            ));
        }
        Ok(())
    }

    /// C を呼ぶ前後以外で、ピンのメモリを読む前に最新にする。
    pub(super) fn pin_before_read(&mut self, a: i128) {
        if self.ffi_depth == 0 {
            if let Some(i) = self.pin_at(a) {
                let _ = self.pin_push(i);
            }
        }
    }

    pub(super) fn pin_after_write(&mut self, a: i128) {
        if self.ffi_depth == 0 {
            if let Some(i) = self.pin_at(a) {
                self.pin_pull(i);
            }
        }
    }

    // ------------------------------------------------------------ extern の呼び出し

    /// 引数の値を、宣言した型に従って C の引数にする。
    fn to_argv(&mut self, d: &Def, v: Val, t: &V) -> Result<ArgV, String> {
        use crate::check::{st, tag, tel};
        // 整数の値を浮動小数の引数に渡すのは型が合わないので、(`coerce` の暗黙の変換に任せず) 止める。
        if tag(t) == "prim" && matches!(st(tel(t, 1)).as_str(), "f32" | "f64") && matches!(dr(&v), Val::Int(..)) {
            return Err("argument of an unsupported type or value".into());
        }
        let v = self.coerce(v, t);
        let raw = v.clone();
        let v = dr(&v);
        match tag(t).as_str() {
            "fn" => {
                let a = self.callback_addr(&raw, t, &d.file)?;
                Ok(ArgV::Int(a as u64))
            }
            "named" => {
                let ct = self.cty_of(t, &d.file)?;
                let mut buf = vec![0u8; ct.size() as usize];
                self.val_to_mem(&v, &ct, &mut buf)?;
                Ok(ArgV::Agg(buf, ct))
            }
            "prim" => {
                let p = st(tel(t, 1));
                Ok(match (&v, p.as_str()) {
                    // 整数の値を浮動小数の引数に渡すのは型が合わないので、黙って変換しない (f32 ← f64 の変換だけ許す)。
                    (Val::Int(..), "f32" | "f64") => return Err("argument of an unsupported type or value".into()),
                    (Val::Int(i, _), _) => ArgV::Int(*i as i64 as u64),
                    (Val::Bool(b), _) => ArgV::Int(*b as u64),
                    (Val::Char(c), _) => ArgV::Int(*c as u64),
                    (Val::F32(f), "f64") => ArgV::F64(*f as f64),
                    (Val::F32(f), _) => ArgV::F32(*f),
                    (Val::Float(f), "f32") => ArgV::F32(*f as f32),
                    (Val::Float(f), _) => ArgV::F64(*f),
                    _ => return Err("only integer, pointer, bool, float, `layout c` struct and callback arguments are supported".into()),
                })
            }
            _ => match &v {
                Val::Int(i, _) => Ok(ArgV::Int(*i as i64 as u64)),
                _ => Err("only integer, pointer, bool, float, `layout c` struct and callback arguments are supported".into()),
            },
        }
    }

    pub(super) fn extern_call(&mut self, d: &Def, args: Vec<Val>) -> Res {
        use crate::check::{tag, tel};
        let (lib, sym) = ffi::split_link(&d.link);
        let fnptr = match ffi::lookup(lib, sym) {
            Ok(p) => p,
            Err(e) => return panic(format!("extern {}: {}", d.name, e)),
        };
        if args.len() != d.params.len() {
            return panic(format!("extern {}: expected {} arguments, got {}", d.name, d.params.len(), args.len()));
        }
        let mut abi = vec![];
        for (a, (_, t)) in args.into_iter().zip(d.params.iter()) {
            match self.to_argv(d, a, t) {
                Ok(x) => abi.push(x),
                Err(e) => return panic(format!("extern {}: {}", d.name, e)),
            }
        }
        let ret = &d.ret;
        let ret_prim = if tag(ret) == "prim" { st(tel(ret, 1)) } else { String::new() };
        let rty = match tag(ret).as_str() {
            "tuple" | "never" => RetTy::Void,
            "ptr" => RetTy::Int,
            "prim" => match ret_prim.as_str() {
                "f32" => RetTy::F32,
                "f64" => RetTy::F64,
                _ => RetTy::Int,
            },
            "named" => match self.cty_of(ret, &d.file) {
                Ok(c) => RetTy::Agg(c),
                Err(e) => return panic(format!("extern {}: unsupported return type: {}", d.name, e)),
            },
            _ => return panic(format!("extern {}: unsupported return type", d.name)),
        };
        // C の呼び出し: ピンを最新にして、コールバックの戻り先を設定して、呼ぶ。
        for i in 0..self.pins.len() {
            if let Err(e) = self.pin_push(i) {
                return panic(format!("extern {}: {}", d.name, e));
            }
        }
        let prev = CUR.with(|c| c.replace(self as *mut Interp));
        self.ffi_depth += 1;
        let r = ffi::call_generic(fnptr, &abi, &rty);
        self.ffi_depth -= 1;
        CUR.with(|c| c.set(prev));
        let r = match r {
            Ok(r) => r,
            Err(e) => return panic(format!("extern {}: {}", d.name, e)),
        };
        for i in 0..self.pins.len() {
            self.pin_pull(i);
        }
        Ok(match tag(ret).as_str() {
            "tuple" => Val::Unit,
            "never" => return panic(format!("extern {} returned although it is declared `!`", d.name)),
            "ptr" => {
                let RetV::Int(x) = r else { return panic("extern: bad return") };
                // 生ポインタの指す型を覚える (`core.ptr.read` / `add` が要素の幅を知るため)。
                let a = x as i128;
                if let Some(e) = elem_name(tel(ret, 2)).map(|n| self.type_fqn(&n, &d.file).unwrap_or(n)) {
                    self.ptr_elem.borrow_mut().insert(a, e);
                }
                int(a, "usize")
            }
            "prim" => match (r, ret_prim.as_str()) {
                (RetV::F32(f), _) => Val::F32(f),
                (RetV::F64(f), _) => Val::Float(f),
                (RetV::Int(x), p) => match p {
                    "bool" => Val::Bool(x & 0xff != 0),
                    "i8" => int((x as i8) as i128, "i8"),
                    "i16" => int((x as i16) as i128, "i16"),
                    "i32" => int((x as i32) as i128, "i32"),
                    "i64" | "isize" => int((x as i64) as i128, p),
                    "u8" => int((x as u8) as i128, "u8"),
                    "u16" => int((x as u16) as i128, "u16"),
                    "u32" => int((x as u32) as i128, "u32"),
                    "u64" | "usize" => int((x as u64) as i128, p),
                    "char" => Val::Char(char::from_u32(x as u32).unwrap_or('\0')),
                    _ => return panic(format!("extern {}: unsupported return type {}", d.name, p)),
                },
                _ => return panic(format!("extern {}: bad return", d.name)),
            },
            "named" => {
                let RetV::Agg(b) = r else { return panic("extern: bad return") };
                let ct = match self.cty_of(ret, &d.file) {
                    Ok(c) => c,
                    Err(e) => return panic(format!("extern {}: {}", d.name, e)),
                };
                self.val_from_mem(&ct, &b)
            }
            _ => return panic(format!("extern {}: unsupported return type", d.name)),
        })
    }

    // ------------------------------------------------------------ コールバック

    /// 関数 / キャプチャなしのクロージャ `f` を、C の関数ポインタ (入口) にする。`t` は宣言の関数型 `fn(..) -> R uses [] panics never`。
    fn callback_addr(&mut self, f: &Val, t: &V, file: &str) -> Result<usize, String> {
        use crate::check::tel;
        let f = match f {
            Val::Ref(_) => dr(f),
            other => other.clone(),
        };
        let key = match &f {
            Val::Func(i) => *i,
            Val::Closure(c) => {
                if !c.caps.is_empty() {
                    return Err("a closure passed to C cannot capture variables (write `capture []`)".into());
                }
                Rc::as_ptr(c) as usize
            }
            _ => return Err("a function type argument must be a function or a closure without captures".into()),
        };
        // 戻り値のクラスを先に決める (キャッシュヒット時も trampoline に rabi を渡す必要がある)。
        let ret = self.cb_ty(tel(t, 2), file)?;
        let rabi = match &ret {
            CbTy::Unit => ffi::RetAbi::Void,
            CbTy::Int(_) | CbTy::Ptr => ffi::RetAbi::Int,
            CbTy::F32 | CbTy::F64 => ffi::RetAbi::F64,
            CbTy::Agg(cty) => {
                // Windows x64: 8 バイト以下の構造体は RAX (整数レジスタ) で返す、
                // 8 バイト超は sret (RCX に隠しポインタ)。SysV の classify に基づく
                // ret_abi は Windows では浮動小数フィールドを含む構造体を XMM0 に
                // しようとするが、Win64 では整数レジスタになるので、ここで上書きする。
                #[cfg(windows)]
                {
                    if cty.size() <= 8 {
                        ffi::RetAbi::Int
                    } else {
                        ffi::RetAbi::Void
                    }
                }
                #[cfg(not(windows))]
                {
                    ffi::ret_abi(cty)
                }
            }
        };
        if let Some(i) = self.cbs.iter().position(|s| s.key == key) {
            return ffi::trampoline(i, rabi).ok_or_else(|| callback_unsupported());
        }
        let mut params = vec![];
        let (mut ng, mut nf) = (0, 0);
        for pt in crate::check::items(tel(t, 1)) {
            let k = self.cb_ty(pt, file)?;
            match &k {
                CbTy::F32 | CbTy::F64 => nf += 1,
                CbTy::Agg(cty) => match ffi::classify(cty) {
                    ffi::Pass::Regs(cls) => {
                        ng += cls.iter().filter(|c| !**c).count();
                        nf += cls.iter().filter(|c| **c).count();
                    }
                    // メモリクラスの構造体はスタック経由で渡され、レジスタは消費しない。
                    ffi::Pass::Memory => {}
                },
                _ => ng += 1,
            }
            params.push((k, pt.clone()));
        }
        // メモリ返し (`sret`) の構造体は、第 1 整数レジスタを戻り先のポインタに使う。
        let sret = matches!(&ret, CbTy::Agg(cty) if ffi::classify(cty) == ffi::Pass::Memory);
        if sret {
            ng += 1;
        }
        if ng > 6 || nf > 8 {
            return Err("a callback takes at most 6 integer/pointer and 8 floating-point parameters in the interpreter".into());
        }
        if self.cbs.len() >= ffi::VM_SLOT_BASE {
            return Err(format!("too many distinct callbacks ({} at most in the interpreter)", ffi::VM_SLOT_BASE));
        }
        ffi::set_callback_handler(cb_handler);
        self.cbs.push(CbSlot { key, f, params, ret });
        ffi::trampoline(self.cbs.len() - 1, rabi).ok_or_else(|| callback_unsupported())
    }

    fn cb_ty(&self, t: &V, file: &str) -> Result<CbTy, String> {
        use crate::check::{tag, tel};
        Ok(match tag(t).as_str() {
            "tuple" => CbTy::Unit,
            "ptr" => CbTy::Ptr,
            "prim" => {
                let p = st(tel(t, 1));
                match p.as_str() {
                    "f32" => CbTy::F32,
                    "f64" => CbTy::F64,
                    _ => CbTy::Int(scalar_name(&p).ok_or("a callback parameter type is not a C scalar")?),
                }
            }
            "named" => match self.cty_of(t, file)? {
                CTy::Struct(s) => CbTy::Agg(CTy::Struct(Rc::clone(&s))),
                _ => return Err("a callback takes and returns only integers, floats, bool, char, pointers, `()` and `layout c` structs by value".into()),
            },
            _ => return Err("a callback takes and returns only integers, floats, bool, char, pointers, `()` and `layout c` structs by value".into()),
        })
    }

    /// 入口から呼ばれる: レジスタの中身を Kasane の値にして関数を呼び、戻り値をレジスタの中身にする。
    fn run_callback(
        &mut self,
        slot: usize,
        gp: &[u64; 6],
        sse: &[u64; 8],
        stack: *const u64,
        out: &mut ffi::RetRegs,
    ) {
        let (f, params, ret) = {
            let s = &self.cbs[slot];
            (s.f.clone(), s.params.clone(), s.ret.clone())
        };
        // メモリ返し (`sret`) なら第 1 GP 引数は戻り先のポインタで、実引数はそれから始まる。
        let sret = matches!(&ret, CbTy::Agg(t) if ffi::classify(t) == ffi::Pass::Memory);
        let (mut gi, mut si) = (if sret { 1 } else { 0 }, 0);
        let mut stack_off = 0usize;
        let mut args = vec![];
        for (k, _) in &params {
            args.push(match k {
                CbTy::F32 => {
                    si += 1;
                    Val::F32(f32::from_bits(sse[si - 1] as u32))
                }
                CbTy::F64 => {
                    si += 1;
                    Val::Float(f64::from_bits(sse[si - 1]))
                }
                CbTy::Ptr => {
                    gi += 1;
                    int(gp[gi - 1] as i128, "usize")
                }
                CbTy::Int(n) => {
                    gi += 1;
                    let u = gp[gi - 1];
                    match *n {
                        "bool" => Val::Bool(u & 0xff != 0),
                        "i8" => int((u as i8) as i128, "i8"),
                        "i16" => int((u as i16) as i128, "i16"),
                        "i32" => int((u as i32) as i128, "i32"),
                        "i64" | "isize" => int((u as i64) as i128, n),
                        "u8" => int((u as u8) as i128, "u8"),
                        "u16" => int((u as u16) as i128, "u16"),
                        "u32" => int((u as u32) as i128, "u32"),
                        "char" => Val::Char(char::from_u32(u as u32).unwrap_or('\0')),
                        _ => int(u as i128, n),
                    }
                }
                CbTy::Unit => Val::Unit,
                CbTy::Agg(cty) => match ffi::classify(cty) {
                    ffi::Pass::Regs(cls) => {
                        let mut buf = vec![0u8; cty.size() as usize];
                        let (mut gj, mut sj) = (gi, si);
                        for (j, &is_sse) in cls.iter().enumerate() {
                            let v = if is_sse { sse[sj] } else { gp[gj] };
                            if is_sse { sj += 1 } else { gj += 1 };
                            let lo = j * 8;
                            let hi = (lo + 8).min(buf.len());
                            buf[lo..hi].copy_from_slice(&v.to_le_bytes()[..hi - lo]);
                        }
                        gi = gj;
                        si = sj;
                        self.val_from_mem(cty, &buf)
                    }
                    ffi::Pass::Memory => {
                        // メモリクラスの構造体はスタック (戻り番地のすぐ後、8 バイトずつ) に渡され、
                        // レジスタは消費しない。それ以前のスタック渡しの構造体の分だけオフセットを進める。
                        let n = (cty.size() as usize).div_ceil(8);
                        let b = unsafe {
                            std::slice::from_raw_parts(stack.add(stack_off) as *const u8, cty.size() as usize)
                        };
                        stack_off += n;
                        self.val_from_mem(cty, b)
                    }
                },
            });
        }
        // コールバックの中の C を呼ぶ処理は、C のメモリが正 (ピンの同期をしない) のまま、深さを保つ。
        let r = self.call_value(&f, args);
        match r {
            Ok(v) => {
                let dv = dr(&v);
                match (ret, dv) {
                    (CbTy::Unit, _) => {}
                    (CbTy::F32, Val::F32(x)) => out.x0 = x.to_bits() as u64,
                    (CbTy::F64, Val::Float(x)) => out.x0 = x.to_bits(),
                    (CbTy::Int(_) | CbTy::Ptr, Val::Int(x, _)) => out.rax = x as i64 as u64,
                    (CbTy::Int(_), Val::Bool(b)) => out.rax = b as u64,
                    (CbTy::Int(_), Val::Char(c)) => out.rax = c as u64,
                    (CbTy::Agg(cty), Val::Struct(..)) => {
                        let mut buf = vec![0u8; cty.size() as usize];
                        if let Err(e) = self.val_to_mem(&v, &cty, &mut buf) {
                            self.die(101, Some(e));
                        }
                        #[cfg(windows)]
                        {
                            if cty.size() <= 8 {
                                // Windows x64: 8 バイト以下の構造体は RAX (整数レジスタ) で返す
                                let mut arr = [0u8; 8];
                                let n = buf.len().min(8);
                                arr[..n].copy_from_slice(&buf[..n]);
                                out.rax = u64::from_le_bytes(arr);
                            } else {
                                // 8 バイト超は sret ポインタ (gp[0] = RCX) に書き込む
                                unsafe {
                                    std::ptr::copy_nonoverlapping(
                                        buf.as_ptr(),
                                        gp[0] as *mut u8,
                                        cty.size() as usize,
                                    )
                                };
                            }
                        }
                        #[cfg(not(windows))]
                        {
                            match ffi::classify(&cty) {
                                ffi::Pass::Memory => {
                                    unsafe {
                                        std::ptr::copy_nonoverlapping(
                                            buf.as_ptr(),
                                            gp[0] as *mut u8,
                                            cty.size() as usize,
                                        )
                                    };
                                }
                                ffi::Pass::Regs(cls) => {
                                    let (mut gj, mut sj) = (0, 0);
                                    for (j, &is_sse) in cls.iter().enumerate() {
                                        let mut x = [0u8; 8];
                                        let lo = j * 8;
                                        let hi = (lo + 8).min(buf.len());
                                        x[..hi - lo].copy_from_slice(&buf[lo..hi]);
                                        let vv = u64::from_le_bytes(x);
                                        if is_sse {
                                            if sj == 0 {
                                                out.x0 = vv;
                                            } else {
                                                out.x1 = vv;
                                            }
                                            sj += 1;
                                        } else {
                                            if gj == 0 {
                                                out.rax = vv;
                                            } else {
                                                out.rdx = vv;
                                            }
                                            gj += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            // コールバックは `panics never` なので panic しない。スタックの深さの超過 (5.2) など、C のフレームを越えて
            // 巻き戻せない異常は、その場でプロセスを止める (panic は終了コード 101、スタックオーバーフローは 102)。
            Err(Ctl::Exit(c)) => self.die(c, None),
            Err(Ctl::Panic(m)) => self.die(101, Some(m)),
            Err(_) => self.die(101, Some("unexpected control flow in a callback".into())),
        }
    }

    fn die(&mut self, code: i32, msg: Option<String>) -> ! {
        let _ = self.out.flush();
        if let Some(m) = msg {
            eprintln!("panic: {}", m);
        }
        std::process::exit(code)
    }
}

fn callback_unsupported() -> String {
    "callbacks (a Kasane function passed to C) are supported by the interpreter on x86_64 Linux only".into()
}

/// 入口 (`ffi::tramp`) から呼ばれるハンドラ。
fn cb_handler(slot: usize, gp: &[u64; 6], sse: &[u64; 8], stack: *const u64, out: &mut ffi::RetRegs) {
    let p = CUR.with(|c| c.get());
    if p.is_null() {
        eprintln!("panic: a C callback was called outside of an extern call");
        std::process::exit(101);
    }
    // 呼び出し元の `extern_call` は、C から戻るまで `self` に触らない。
    let it = unsafe { &mut *p };
    it.run_callback(slot, gp, sse, stack, out)
}
