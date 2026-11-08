//! 実行時の値表現 (stage0 インタプリタ)。Python 参照実装 ksn/values.py に対応する。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use crate::parser::V;

pub type Ity = Option<&'static str>;

pub const INT_TYPES: &[&str] = &["i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize"];

pub fn ity(s: &str) -> &'static str {
    INT_TYPES.iter().copied().find(|x| *x == s).unwrap_or("i32")
}
pub fn is_int_ty(s: &str) -> bool {
    INT_TYPES.contains(&s)
}
pub fn bits(ty: &str) -> u32 {
    match ty {
        "i8" | "u8" => 8,
        "i16" | "u16" => 16,
        "i32" | "u32" => 32,
        "i128" | "u128" => 128,
        _ => 64,
    }
}
pub fn signed(ty: &str) -> bool {
    ty.starts_with('i')
}
pub fn tmin(ty: &str) -> i128 {
    if !signed(ty) {
        0
    } else if bits(ty) == 128 {
        i128::MIN
    } else {
        -(1i128 << (bits(ty) - 1))
    }
}
pub fn tmax(ty: &str) -> i128 {
    let b = bits(ty);
    if b == 128 {
        // u128 はビットパターンで保持する (u128::MAX = 全ビット1 = -1)。
        if signed(ty) { i128::MAX } else { -1 }
    } else if signed(ty) {
        (1i128 << (b - 1)) - 1
    } else {
        (1i128 << b) - 1
    }
}
pub fn wrap(v: i128, ty: &str) -> i128 {
    let b = bits(ty);
    if b == 128 {
        return v;
    }
    let m = (1i128 << b) - 1;
    let mut x = v & m;
    if signed(ty) && (x >> (b - 1)) & 1 == 1 {
        x -= 1i128 << b;
    }
    x
}

pub trait Place {
    fn get(&self) -> Val;
    fn set(&self, v: Val);
}

pub struct ScopeD {
    pub vars: HashMap<String, Val>,
    pub parent: Option<Scope>,
    pub file: Rc<str>,
}
pub type Scope = Rc<RefCell<ScopeD>>;

pub fn new_scope(parent: Option<&Scope>, file: Rc<str>) -> Scope {
    Rc::new(RefCell::new(ScopeD {
        vars: HashMap::new(),
        parent: parent.cloned(),
        file: parent.map_or(file.clone(), |p| p.borrow().file.clone()),
    }))
}
pub fn sc_owner(sc: &Scope, name: &str) -> Option<Scope> {
    let mut cur = Some(sc.clone());
    while let Some(s) = cur {
        if s.borrow().vars.contains_key(name) {
            return Some(s);
        }
        cur = s.borrow().parent.clone();
    }
    None
}
pub fn sc_get(sc: &Scope, name: &str) -> Option<Val> {
    sc_owner(sc, name).map(|s| s.borrow().vars[name].clone())
}
pub fn sc_declare(sc: &Scope, name: &str, v: Val) {
    sc.borrow_mut().vars.insert(name.to_string(), v);
}

pub struct StructV {
    pub ty: String,
    pub f: RefCell<Vec<(String, Val)>>,
}
impl StructV {
    pub fn get(&self, name: &str) -> Option<Val> {
        self.f.borrow().iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }
    pub fn set(&self, name: &str, v: Val) {
        let mut f = self.f.borrow_mut();
        if let Some(e) = f.iter_mut().find(|(k, _)| k == name) {
            e.1 = v;
        } else {
            f.push((name.to_string(), v));
        }
    }
}

pub struct EnumV {
    pub en: String,
    pub var: String,
    pub pl: Vec<Val>,
}

pub struct ClosureV {
    pub params: Vec<(String, V)>,
    pub ret: V,
    pub body: V,
    pub scope: Scope,
}

pub enum IterV {
    List { items: Rc<RefCell<Vec<Val>>>, i: usize, end: Option<usize> },
    Pre { items: Vec<Val>, i: usize },
    Range { cur: Val, end: Val },
    Map { src: Rc<RefCell<IterV>>, f: Val },
    Filter { src: Rc<RefCell<IterV>>, f: Val },
    Enumerate { src: Rc<RefCell<IterV>>, i: i128 },
    Take { src: Rc<RefCell<IterV>>, n: i128 },
    User { place: Rc<dyn Place>, def: usize },
}

pub struct MapV {
    pub kind: String,
    pub d: Vec<(Val, Val)>,
}

#[derive(Clone)]
pub enum Val {
    Unit,
    Bool(bool),
    Int(i128, Ity),
    Float(f64),
    F32(f32),
    Char(char),
    Str(Rc<str>),
    Tuple(Rc<Vec<Val>>),
    Arr(Rc<RefCell<Vec<Val>>>),
    Vec(Rc<RefCell<Vec<Val>>>),
    View(Rc<RefCell<Vec<Val>>>, usize, usize),
    Struct(Rc<StructV>),
    Enum(Rc<EnumV>),
    Ref(Rc<dyn Place>),
    Closure(Rc<ClosureV>),
    Func(usize),
    Native(Rc<str>),
    Ctor(Rc<(String, String)>),
    Iter(Rc<RefCell<IterV>>),
    Map(Rc<RefCell<MapV>>),
    StrBuf(Rc<RefCell<String>>),
    Fmt(Rc<RefCell<String>>),
    Chan(Rc<RefCell<VecDeque<Val>>>),
    Mutex(Rc<CellPlace>),
    Join(Rc<Val>),
    Rng(Rc<Cell<u64>>),
}

pub struct CellPlace(pub RefCell<Val>);
impl Place for CellPlace {
    fn get(&self) -> Val {
        self.0.borrow().clone()
    }
    fn set(&self, v: Val) {
        *self.0.borrow_mut() = v;
    }
}
pub struct VarPlace(pub Scope, pub String);
impl Place for VarPlace {
    fn get(&self) -> Val {
        self.0.borrow().vars[&self.1].clone()
    }
    fn set(&self, v: Val) {
        self.0.borrow_mut().vars.insert(self.1.clone(), v);
    }
}
pub struct FieldPlace(pub Rc<StructV>, pub String);
impl Place for FieldPlace {
    fn get(&self) -> Val {
        self.0.get(&self.1).unwrap_or(Val::Unit)
    }
    fn set(&self, v: Val) {
        self.0.set(&self.1, v);
    }
}
pub struct IdxPlace(pub Rc<RefCell<Vec<Val>>>, pub usize);
impl Place for IdxPlace {
    fn get(&self) -> Val {
        self.0.borrow()[self.1].clone()
    }
    fn set(&self, v: Val) {
        self.0.borrow_mut()[self.1] = v;
    }
}
pub struct PlPlace(pub Rc<dyn Place>, pub usize);
impl Place for PlPlace {
    fn get(&self) -> Val {
        match self.0.get() {
            Val::Enum(e) => e.pl[self.1].clone(),
            _ => Val::Unit,
        }
    }
    fn set(&self, v: Val) {
        if let Val::Enum(e) = self.0.get() {
            let mut pl = e.pl.clone();
            pl[self.1] = v;
            self.0.set(Val::Enum(Rc::new(EnumV { en: e.en.clone(), var: e.var.clone(), pl })));
        }
    }
}
pub struct MapPlace(pub Rc<RefCell<MapV>>, pub usize);
impl Place for MapPlace {
    fn get(&self) -> Val {
        self.0.borrow().d[self.1].1.clone()
    }
    fn set(&self, v: Val) {
        self.0.borrow_mut().d[self.1].1 = v;
    }
}

pub enum Ctl {
    Break,
    Continue,
    Return(Val),
    Panic(String),
    Exit(i32),
}
pub type Res = Result<Val, Ctl>;

pub fn panic<T>(msg: impl Into<String>) -> Result<T, Ctl> {
    Err(Ctl::Panic(msg.into()))
}

pub fn dr(v: &Val) -> Val {
    let mut cur = v.clone();
    while let Val::Ref(p) = &cur {
        let n = p.get();
        cur = n;
    }
    cur
}

pub fn mk_enum(en: &str, var: &str, pl: Vec<Val>) -> Val {
    Val::Enum(Rc::new(EnumV { en: en.into(), var: var.into(), pl }))
}
pub fn some(v: Val) -> Val {
    mk_enum("Option", "Some", vec![v])
}
pub fn none() -> Val {
    mk_enum("Option", "None", vec![])
}
pub fn ok(v: Val) -> Val {
    mk_enum("Result", "Ok", vec![v])
}
pub fn err(v: Val) -> Val {
    mk_enum("Result", "Err", vec![v])
}
pub fn opt(v: Option<Val>) -> Val {
    v.map_or_else(none, some)
}
pub fn int(v: i128, ty: &str) -> Val {
    Val::Int(v, Some(ity(ty)))
}
pub fn usz(v: usize) -> Val {
    Val::Int(v as i128, Some("usize"))
}
pub fn s_val(s: &str) -> Val {
    Val::Str(Rc::from(s))
}
pub fn new_list(items: Vec<Val>) -> Rc<RefCell<Vec<Val>>> {
    Rc::new(RefCell::new(items))
}

/// 列として見られる値の要素を複製して返す (Arr / Vec / View / Tuple)。
pub fn seq_items(v: &Val) -> Option<Vec<Val>> {
    match v {
        Val::Arr(l) | Val::Vec(l) => Some(l.borrow().clone()),
        Val::View(l, a, b) => Some(l.borrow()[*a..*b].to_vec()),
        Val::Tuple(t) => Some(t.to_vec()),
        _ => None,
    }
}

pub fn str_of(v: &Val) -> Option<String> {
    match dr(v) {
        Val::Str(s) => Some(s.to_string()),
        Val::StrBuf(b) => Some(b.borrow().clone()),
        _ => None,
    }
}

pub fn veq(a: &Val, b: &Val) -> bool {
    let (a, b) = (dr(a), dr(b));
    if let (Some(x), Some(y)) = (str_of(&a), str_of(&b)) {
        return x == y;
    }
    match (&a, &b) {
        (Val::Struct(x), Val::Struct(y)) => {
            x.ty == y.ty && x.f.borrow().iter().all(|(k, v)| y.get(k).map_or(false, |w| veq(v, &w)))
        }
        (Val::Enum(x), Val::Enum(y)) => {
            x.var == y.var && x.en == y.en && x.pl.len() == y.pl.len() && x.pl.iter().zip(&y.pl).all(|(p, q)| veq(p, q))
        }
        (Val::Int(x, _), Val::Int(y, _)) => x == y,
        (Val::Float(x), Val::Float(y)) => x == y,
        (Val::F32(x), Val::F32(y)) => x == y,
        (Val::F32(x), Val::Float(y)) | (Val::Float(y), Val::F32(x)) => (*x as f64) == *y,
        (Val::Bool(x), Val::Bool(y)) => x == y,
        (Val::Char(x), Val::Char(y)) => x == y,
        (Val::Unit, Val::Unit) => true,
        _ => match (seq_items(&a), seq_items(&b)) {
            (Some(x), Some(y)) => x.len() == y.len() && x.iter().zip(&y).all(|(p, q)| veq(p, q)),
            _ => false,
        },
    }
}

/// -1 / 0 / 1 (NaN は None)。列挙子は宣言順 `order` で比較する。
pub fn vcmp(a: &Val, b: &Val, order: &dyn Fn(&str, &str) -> (usize, usize)) -> Option<i32> {
    let (a, b) = (dr(a), dr(b));
    match (&a, &b) {
        (Val::Enum(x), Val::Enum(y)) => {
            if x.var != y.var {
                let (ia, ib) = order(&x.var, &y.var);
                return Some(if ia < ib { -1 } else { 1 });
            }
            vcmp_seq(&x.pl, &y.pl, order)
        }
        (Val::Struct(x), Val::Struct(y)) => {
            let xs: Vec<Val> = x.f.borrow().iter().map(|(_, v)| v.clone()).collect();
            let ys: Vec<Val> = y.f.borrow().iter().map(|(_, v)| v.clone()).collect();
            vcmp_seq(&xs, &ys, order)
        }
        (Val::Int(x, tx), Val::Int(y, ty)) => {
            if *tx == Some("u128") || *ty == Some("u128") {
                let (x, y) = (*x as u128, *y as u128);
                return Some((x > y) as i32 - (x < y) as i32);
            }
            Some((x > y) as i32 - (x < y) as i32)
        }
        (Val::F32(x), Val::F32(y)) => {
            if x.is_nan() || y.is_nan() {
                None
            } else {
                Some((x > y) as i32 - (x < y) as i32)
            }
        }
        (Val::Float(x), Val::Float(y)) => {
            if x.is_nan() || y.is_nan() {
                None
            } else {
                Some((x > y) as i32 - (x < y) as i32)
            }
        }
        (Val::Char(x), Val::Char(y)) => Some((x > y) as i32 - (x < y) as i32),
        (Val::Bool(x), Val::Bool(y)) => Some((*x as i32) - (*y as i32)),
        _ => {
            if let (Some(x), Some(y)) = (str_of(&a), str_of(&b)) {
                return Some((x > y) as i32 - (x < y) as i32);
            }
            match (seq_items(&a), seq_items(&b)) {
                (Some(x), Some(y)) => vcmp_seq(&x, &y, order),
                _ => Some(0),
            }
        }
    }
}

fn vcmp_seq(x: &[Val], y: &[Val], order: &dyn Fn(&str, &str) -> (usize, usize)) -> Option<i32> {
    for (p, q) in x.iter().zip(y) {
        match vcmp(p, q, order) {
            Some(0) => {}
            r => return r,
        }
    }
    Some((x.len() > y.len()) as i32 - (x.len() < y.len()) as i32)
}

/// ハッシュ可能なキーの文字列表現 (辞書の同一性判定用)。
pub fn hkey(v: &Val) -> String {
    let v = dr(v);
    match &v {
        Val::Struct(s) => format!("S{}{{{}}}", s.ty, s.f.borrow().iter().map(|(k, x)| format!("{}={}", k, hkey(x))).collect::<Vec<_>>().join(",")),
        Val::Enum(e) => format!("E{}.{}({})", e.en, e.var, e.pl.iter().map(hkey).collect::<Vec<_>>().join(",")),
        Val::Int(x, _) => format!("i{}", x),
        Val::Char(c) => format!("c{}", c),
        Val::Bool(b) => format!("b{}", b),
        Val::Float(f) => format!("f{}", f),
        Val::F32(f) => format!("f{}", f),
        Val::Unit => "()".into(),
        _ => {
            if let Some(s) = str_of(&v) {
                return format!("s{}", s);
            }
            if let Some(xs) = seq_items(&v) {
                return format!("L[{}]", xs.iter().map(hkey).collect::<Vec<_>>().join(","));
            }
            "?".into()
        }
    }
}
