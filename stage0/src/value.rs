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
    /// 同じ場所を指す別の `Place` 値 (`&x` を評価するたびに作られる) を見分ける鍵。分からなければ `None`。
    fn key(&self) -> Option<(usize, String)> {
        None
    }
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
#[allow(dead_code)]
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
    /// `capture [..]` の (種類, 名前)。`std.thread.spawn` が取り込む値を、その時点の値で固定するのに使う。
    pub caps: Vec<(String, String)>,
}

/// チャネルの状態 (インタプリタ)。送信側・受信側は `clone` で増える (端の参照数を数える)。
/// 端の数が 0 になった側は閉じたとみなす。容量は無視する (満杯で待つことはしない。結果が実行順に依存しないプログラムでは観測できない)。
pub struct ChanState {
    pub q: RefCell<VecDeque<Val>>,
    pub senders: Cell<usize>,
    pub receivers: Cell<usize>,
}

/// チャネルの端 (論理的な 1 つ)。最後の参照が無くなった (= 値が落とされた) とき、その端を閉じる。
/// `close` で先に畳んだ端は `closed` を立て、落とされたときに二重に数えない。
pub struct ChanEnd {
    pub st: Rc<ChanState>,
    pub is_sender: bool,
    pub closed: Cell<bool>,
}

impl ChanEnd {
    pub fn new(st: Rc<ChanState>, is_sender: bool) -> ChanEnd {
        let n = if is_sender { &st.senders } else { &st.receivers };
        n.set(n.get() + 1);
        ChanEnd { st, is_sender, closed: Cell::new(false) }
    }
    /// 端を 1 つ畳む (まだ畳んでいなければ)。
    pub fn release(&self) {
        if !self.closed.replace(true) {
            let n = if self.is_sender { &self.st.senders } else { &self.st.receivers };
            n.set(n.get() - 1);
        }
    }
}

impl Drop for ChanEnd {
    fn drop(&mut self) {
        self.release();
    }
}

/// `spawn` したスレッド (インタプリタ)。`body` は `join` かチャネルの待ちで走らせるまで保留する (走るのは 1 回、最後まで)。
pub struct ThreadV {
    pub body: RefCell<Option<Val>>,
    pub result: RefCell<Option<Val>>,
    /// 0 = 未実行、1 = 実行中、2 = 完了
    pub state: Cell<u8>,
}

/// 値が (入れ子を含めて) チャネルの端を持つか。持つ値は、変数から別へ渡すとき元の変数を空にする (移動の近似)。
pub fn has_chan(v: &Val, depth: u32) -> bool {
    if depth > 4 {
        return false;
    }
    match v {
        Val::Chan(_) => true,
        Val::Tuple(t) => t.iter().any(|x| has_chan(x, depth + 1)),
        Val::Enum(e) => e.pl.iter().any(|x| has_chan(x, depth + 1)),
        Val::Struct(s) => s.f.borrow().iter().any(|x| has_chan(&x.1, depth + 1)),
        _ => false,
    }
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

/// `HashMap` / `BtreeMap` の実体。`d` は挿入順の組の列で、反復順はこれに従う（規範）。
/// `idx` は `hkey` から `d` の添字への索引で、検索を平均 O(1) にする。`d` と常に対応する。
pub struct MapV {
    pub kind: String,
    pub d: Vec<(Val, Val)>,
    pub idx: HashMap<String, usize>,
}

impl MapV {
    /// 組の列から表を作る（索引も作り直す）。
    pub fn new(kind: &str, d: Vec<(Val, Val)>) -> MapV {
        let idx = d.iter().enumerate().map(|(i, (k, _))| (hkey(k), i)).collect();
        MapV { kind: kind.into(), d, idx }
    }
}

/// `String` の木走査の値: 内容と、直前の `as_str` の結果 (`Rc<str>`)。書き換え (`edit`) で結果を捨てるので、
/// 変更がない間の `as_str` は同じ `Rc<str>` を返す (O(1))。
#[derive(Debug)]
pub struct StrBufV {
    s: RefCell<String>,
    snap: RefCell<Option<Rc<str>>>,
}

impl StrBufV {
    pub fn new(s: String) -> Rc<StrBufV> {
        Rc::new(StrBufV { s: RefCell::new(s), snap: RefCell::new(None) })
    }
    /// 読む (複製しない)。
    pub fn get(&self) -> std::cell::Ref<'_, String> {
        self.s.borrow()
    }
    /// 書き換える。直前の `as_str` の結果は捨てる。
    pub fn edit(&self) -> std::cell::RefMut<'_, String> {
        *self.snap.borrow_mut() = None;
        self.s.borrow_mut()
    }
    /// 内容を `Rc<str>` で返す (変わるまでは同じものを共有する)。
    pub fn snapshot(&self) -> Rc<str> {
        if let Some(r) = self.snap.borrow().as_ref() {
            return r.clone();
        }
        let r: Rc<str> = Rc::from(self.s.borrow().as_str());
        *self.snap.borrow_mut() = Some(r.clone());
        r
    }
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
    StrBuf(Rc<StrBufV>),
    Fmt(Rc<RefCell<String>>),
    Chan(Rc<ChanEnd>),
    Mutex(Rc<CellPlace>),
    Join(Rc<ThreadV>),
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
    fn key(&self) -> Option<(usize, String)> {
        Some((Rc::as_ptr(&self.0) as *const u8 as usize, self.1.clone()))
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
    /// 取り消されたタスクの巻き戻し (仕様 5.6 の第 4 段)。タスクの本体を抜けるだけで、エラーにはしない。
    Cancel,
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

/// `alloc.vec.Vec<T, A>` の値の型名 (構造体 `{ buf: alloc.raw.Buf<T, A> }`、仕様 8.3 と `spec/stdlib/alloc/vec.sig`)。
pub const VEC_TY: &str = "alloc.vec.Vec";

/// 列の実体 (要素の `Rc<RefCell<Vec<Val>>>`) を返す: 配列 `Arr`、生の列 `Val::Vec` (`Buf` の格納と `VecDeque`)、
/// `Vec` の値 (`buf` の `items`)。列でなければ None。View の範囲は含まない (呼び出し側が範囲を見る)。
pub fn raw_list(v: &Val) -> Option<Rc<RefCell<Vec<Val>>>> {
    match v {
        Val::Arr(l) | Val::Vec(l) => Some(l.clone()),
        Val::Struct(s) if s.ty == VEC_TY => match s.get("buf") {
            Some(Val::Struct(b)) => match b.get("items") {
                Some(Val::Vec(l)) => Some(l),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// `Vec` の値なら、その格納の生の列 (`Val::Vec`) に置き換えた値を返す。それ以外は複製 (Rc の増分だけ)。
/// 添字・反復・表示・比較など、列を直接読む経路が `Val::Vec` の形で扱えるようにする。
pub fn as_raw(v: &Val) -> Val {
    if let Val::Struct(s) = v {
        if s.ty == VEC_TY {
            if let Some(l) = raw_list(v) {
                return Val::Vec(l);
            }
        }
    }
    v.clone()
}

/// 列として見られる値の要素を複製して返す (Arr / Vec / View / Tuple)。
pub fn seq_items(v: &Val) -> Option<Vec<Val>> {
    if let Some(l) = raw_list(v) {
        return Some(l.borrow().clone());
    }
    match v {
        Val::View(l, a, b) => Some(l.borrow()[*a..*b].to_vec()),
        Val::Tuple(t) => Some(t.to_vec()),
        _ => None,
    }
}

/// 列の長さ (Arr / Vec / View / Tuple)。列でなければ None。複製はしない。
pub fn seq_len(v: &Val) -> Option<usize> {
    if let Some(l) = raw_list(v) {
        return Some(l.borrow().len());
    }
    match v {
        Val::View(_, a, b) => Some(b - a),
        Val::Tuple(t) => Some(t.len()),
        _ => None,
    }
}

/// 列の i 番目の要素だけを複製して返す (範囲外・列でない場合は None)。O(1)。
pub fn seq_elem(v: &Val, i: usize) -> Option<Val> {
    if let Some(l) = raw_list(v) {
        return l.borrow().get(i).cloned();
    }
    match v {
        Val::View(l, a, b) => {
            if i < b - a {
                l.borrow().get(a + i).cloned()
            } else {
                None
            }
        }
        Val::Tuple(t) => t.get(i).cloned(),
        _ => None,
    }
}

pub fn seq_get(v: &Val, i: usize) -> Option<Val> {
    seq_elem(v, i)
}

pub fn str_of(v: &Val) -> Option<String> {
    match dr(v) {
        Val::Str(s) => Some(s.to_string()),
        Val::StrBuf(b) => Some(b.get().clone()),
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
        // `Vec` は格納 (`buf` の容量を含む) で比べず、要素の列として表す (下の `seq_items` の枝)。
        Val::Struct(s) if s.ty != VEC_TY => format!("S{}{{{}}}", s.ty, s.f.borrow().iter().map(|(k, x)| format!("{}={}", k, hkey(x))).collect::<Vec<_>>().join(",")),
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
