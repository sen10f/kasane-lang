//! コンパイラ組み込み(ネイティブ)の標準ライブラリ実装 (stage0)。Python 参照実装 ksn/natives.py に対応する。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use crate::check::{items, st};
use crate::parser::V;
use crate::interp::Interp;
use crate::value::*;
use crate::value::Scope;

pub const NATIVE_NAMES: &[&str] = &[
    "core.mem.drop", "core.mem.swap", "core.mem.replace", "core.mem.size_of", "core.panic.panic",
    "core.panic.todo", "core.panic.unreachable", "core.check.eq", "core.check.is_true", "std.io.print_line", "std.process.exit",
    "std.process.abort", "std.env.arg", "std.env.arg_count", "std.env.var", "core.iter.range", "core.iter.map",
    "core.iter.filter", "core.iter.enumerate", "core.iter.take", "core.iter.count", "core.iter.fold",
    "core.str.from_utf8", "alloc.vec.Vec.new", "alloc.vec.Vec.with_capacity", "alloc.string.String.new",
    "alloc.string.String.from_str", "alloc.collections.HashMap.new", "alloc.collections.BtreeMap.new",
    "alloc.collections.VecDeque.new",
    "alloc.boxed.Box.new", "std.time.Instant.now", "std.time.Duration.from_millis", "std.thread.spawn",
    "std.thread.sleep", "std.thread.yield_now", "std.sync.channel", "std.sync.Mutex.new",
    "std.rand.Rng.from_seed", "std.rand.Rng.from_entropy", "std.fs.exists", "std.fs.remove_file", "std.fs.open",
    "std.fs.create", "std.fs.read_to_end",
];

const NUM_EXTRA: &[&str] = &[
    "abs", "max", "min", "count_ones", "leading_zeros", "trailing_zeros", "successor", "to_f64", "from_str_radix",
    "to_bits", "from_bits", "rem", "sqrt", "floor", "ceil", "sin", "cos", "tan", "exp", "ln", "powi", "trunc",
    "round_ties_even", "is_nan", "is_finite", "atan2", "nearest_to_f32", "then_some", "is_ascii_alphabetic",
    "is_ascii_digit", "is_ascii_whitespace", "len_utf8", "to_ascii_lowercase", "to_ascii_uppercase", "to_u32",
    "try_from_u32",
];

pub fn known_num(name: &str) -> bool {
    ["wrapping_", "checked_", "saturating_", "from_", "try_from_", "trunc_to_", "to_"].iter().any(|p| name.starts_with(p))
        || NUM_EXTRA.contains(&name)
}

fn pick_ty(args: &[Val], default: &str) -> &'static str {
    for a in args {
        if let Val::Int(_, Some(t)) = a {
            return t;
        }
    }
    ity(default)
}

fn trunc_div(a: i128, b: i128) -> i128 {
    a / b
}
fn trunc_rem(a: i128, b: i128) -> i128 {
    a % b
}

fn ordering(c: i32) -> Val {
    mk_enum("core.cmp.Ordering", ["Less", "Equal", "Greater"][(c + 1) as usize], vec![])
}

fn splitmix(s: u64) -> (u64, u64) {
    let s = s.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    (s, z ^ (z >> 31))
}

fn parse_err(kind: i128) -> Val {
    Val::Struct(Rc::new(StructV {
        ty: "core.num.ParseIntError".into(),
        f: RefCell::new(vec![("kind".into(), int(kind, "u8"))]),
    }))
}
fn try_err() -> Val {
    Val::Struct(Rc::new(StructV {
        ty: "core.num.TryFromIntError".into(),
        f: RefCell::new(vec![("kind".into(), int(0, "u8"))]),
    }))
}

fn num_op(name: &str, ty: &str, a: &[Val]) -> Res {
    let x = a.first().cloned().unwrap_or(Val::Unit);
    if let Val::Bool(b) = x {
        if name == "then_some" {
            return Ok(if b { some(a[1].clone()) } else { none() });
        }
    }
    if let Val::Char(c) = x {
        match name {
            "is_ascii_alphabetic" => return Ok(Val::Bool(c.is_ascii_alphabetic())),
            "is_ascii_digit" => return Ok(Val::Bool(c.is_ascii_digit())),
            "is_ascii_whitespace" => return Ok(Val::Bool(matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c'))),
            "len_utf8" => return Ok(usz(c.len_utf8())),
            "to_ascii_lowercase" => return Ok(Val::Char(c.to_ascii_lowercase())),
            "to_ascii_uppercase" => return Ok(Val::Char(c.to_ascii_uppercase())),
            "to_u32" => return Ok(int(c as i128, "u32")),
            _ => {}
        }
    }
    if name == "try_from_u32" && ty == "char" {
        if let Val::Int(v, _) = x {
            return Ok(match char::from_u32(v as u32) {
                Some(c) if v <= 0x10FFFF => some(Val::Char(c)),
                _ => none(),
            });
        }
    }
    if ty == "f32" || ty == "f64" || matches!(x, Val::Float(_) | Val::F32(_)) {
        return float_op(name, ty, a);
    }
    let iv = |v: &Val| match v {
        Val::Int(x, _) => *x,
        Val::Float(f) => *f as i128,
        _ => 0,
    };
    if name == "from_str_radix" {
        let text = str_of(&x).unwrap_or_default();
        let radix = iv(&a[1]) as u32;
        let ty = ity(ty);
        if ty == "u128" {
            return Ok(match u128::from_str_radix(&text, radix) {
                Ok(v) => ok(int(v as i128, ty)),
                Err(_) => err(parse_err(0)),
            });
        }
        return Ok(match i128::from_str_radix(&text, radix) {
            Err(_) => err(parse_err(0)),
            Ok(v) if v < tmin(ty) || v > tmax(ty) => err(parse_err(1)),
            Ok(v) => ok(int(v, ty)),
        });
    }
    let ty = ity(ty);
    let src_big = matches!(x, Val::Int(v, Some("u128")) if v < 0);
    if name.starts_with("try_from_") {
        let v = iv(&x);
        let fits = if ty == "u128" {
            src_big || v >= 0
        } else {
            !src_big && v >= tmin(ty) && v <= tmax(ty)
        };
        return Ok(if fits { ok(int(v, ty)) } else { err(try_err()) });
    }
    if ty == "u128" {
        if let Some(r) = u128_op(name, a) {
            return r;
        }
    }
    if name.starts_with("from_") && !name.ends_with("_nearest") && name != "from_bits" {
        return Ok(int(iv(&x), ty));
    }
    // `nearest_to_f64` は sig (`core.num.<int>.nearest_to_f64`) にある。最も近い f64 へ丸める。
    if name == "to_f64" || name == "nearest_to_f64" {
        let v = iv(&x);
        return Ok(Val::Float(if ty == "u128" { v as u128 as f64 } else { v as f64 }));
    }
    let s = iv(&x);
    let o = if a.len() > 1 { iv(&a[1]) } else { 0 };
    let b = bits(ty) as i128;
    let (lo, hi) = (tmin(ty), tmax(ty));
    let mask = if b == 128 { -1i128 } else { (1i128 << b) - 1 };
    match name {
        "max" => return Ok(int(s.max(o), ty)),
        "min" => return Ok(int(s.min(o), ty)),
        "abs" => return Ok(int(s.abs(), ty)),
        "successor" => return Ok(if s < hi { some(int(s + 1, ty)) } else { none() }),
        "count_ones" => return Ok(int(((s & mask) as u128).count_ones() as i128, "u32")),
        "leading_zeros" => return Ok(int(b - (128 - ((s & mask) as u128).leading_zeros() as i128), "u32")),
        "trailing_zeros" => {
            let u = (s & mask) as u128;
            return Ok(int(if u == 0 { b } else { u.trailing_zeros() as i128 }, "u32"));
        }
        _ => {}
    }
    let (pre, op) = name.split_once('_').unwrap_or((name, ""));
    let w = |v: i128| int(wrap(v, ty), ty);
    let shr = |v: i128, n: i128| if signed(ty) { v >> n } else { (v & mask) >> n };
    match pre {
        "wrapping" => match op {
            "add" => return Ok(w(s.wrapping_add(o))),
            "sub" => return Ok(w(s.wrapping_sub(o))),
            "mul" => return Ok(w(s.wrapping_mul(o))),
            "neg" => return Ok(w(s.wrapping_neg())),
            "shl" => return Ok(w(s.wrapping_shl((o % b) as u32))),
            "shr" => return Ok(w(shr(s, o % b))),
            _ => {}
        },
        "checked" => {
            let r: Option<i128> = match op {
                "add" => s.checked_add(o),
                "sub" => s.checked_sub(o),
                "mul" => s.checked_mul(o),
                "neg" => s.checked_neg(),
                "abs" => s.checked_abs(),
                "div" | "rem" => {
                    if o == 0 {
                        return Ok(none());
                    }
                    Some(if op == "div" { trunc_div(s, o) } else { trunc_rem(s, o) })
                }
                "shl" => {
                    if o >= b {
                        return Ok(none());
                    }
                    return Ok(some(w(s.wrapping_shl(o as u32))));
                }
                "shr" => {
                    if o >= b {
                        return Ok(none());
                    }
                    return Ok(some(w(shr(s, o))));
                }
                _ => return panic(format!("unknown integer operation {}.{}", ty, name)),
            };
            return Ok(match r {
                Some(v) if v >= lo && v <= hi => some(int(v, ty)),
                _ => none(),
            });
        }
        "saturating" => {
            let r = match op {
                "add" => s.checked_add(o),
                "sub" => s.checked_sub(o),
                "mul" => s.checked_mul(o),
                "neg" => s.checked_neg(),
                _ => return panic(format!("unknown integer operation {}.{}", ty, name)),
            };
            return Ok(int(r.map_or(if (op == "sub" && o > 0) || (op == "neg" && s > 0) { lo } else { hi }, |v| v.clamp(lo, hi)), ty));
        }
        _ => {}
    }
    panic(format!("unknown integer operation {}.{}", ty, name))
}

fn float_op(name: &str, ty: &str, a: &[Val]) -> Res {
    let f32mode = ty == "f32" || matches!(a.first(), Some(Val::F32(_)));
    let ty = if f32mode { "f32" } else { ty };
    if f32mode && name == "to_f64" {
        if let Some(Val::F32(x)) = a.first() {
            return Ok(Val::Float(*x as f64));
        }
    }
    if name == "nearest_to_f32" {
        if let Some(Val::Float(x)) = a.first() {
            return Ok(Val::F32(*x as f32));
        }
    }
    let r = float_op_f64(name, ty, a)?;
    if f32mode {
        if let Val::Float(x) = r {
            return Ok(Val::F32(x as f32));
        }
    }
    Ok(r)
}

fn float_op_f64(name: &str, ty: &str, a: &[Val]) -> Res {
    let x = match a.first() {
        Some(Val::Float(f)) => *f,
        Some(Val::F32(f)) => *f as f64,
        Some(Val::Int(i, _)) => *i as f64,
        _ => 0.0,
    };
    let y = match a.get(1) {
        Some(Val::Float(f)) => *f,
        Some(Val::F32(f)) => *f as f64,
        Some(Val::Int(i, _)) => *i as f64,
        _ => 0.0,
    };
    if name == "from_bits" {
        if let Some(Val::Int(n, _)) = a.first() {
            return Ok(Val::Float(if ty == "f64" { f64::from_bits(*n as u64) } else { f32::from_bits(*n as u32) as f64 }));
        }
    }
    if name.starts_with("from_") && name.ends_with("_nearest") {
        return Ok(Val::Float(x));
    }
    if name == "to_bits" {
        if ty == "f32" {
            return Ok(int((x as f32).to_bits() as i128, "u32"));
        }
        return Ok(int(x.to_bits() as i128, "u64"));
    }
    if let Some(t) = name.strip_prefix("trunc_to_") {
        if x.is_nan() || x.is_infinite() {
            return Ok(none());
        }
        let v = x.trunc() as i128;
        return Ok(if v >= tmin(t) && v <= tmax(t) { some(int(v, t)) } else { none() });
    }
    let simple = |f: fn(f64) -> f64| -> Res {
        if x.is_nan() || x.is_infinite() {
            Ok(Val::Float(x))
        } else {
            Ok(Val::Float(f(x)))
        }
    };
    match name {
        "abs" => simple(f64::abs),
        "sqrt" => simple(|v| if v >= 0.0 { v.sqrt() } else { f64::NAN }),
        "floor" => simple(f64::floor),
        "ceil" => simple(f64::ceil),
        "sin" => simple(f64::sin),
        "cos" => simple(f64::cos),
        "tan" => simple(f64::tan),
        "exp" => simple(|v| if v < 709.78 { v.exp() } else { f64::INFINITY }),
        "ln" => simple(|v| if v > 0.0 { v.ln() } else if v == 0.0 { f64::NEG_INFINITY } else { f64::NAN }),
        "trunc" => simple(f64::trunc),
        "round_ties_even" => simple(|v| {
            let r = v.round();
            if (v - v.trunc()).abs() == 0.5 && r % 2.0 != 0.0 { r - v.signum() } else { r }
        }),
        "nearest_to_f32" => simple(|v| v),
        "is_nan" => Ok(Val::Bool(x.is_nan())),
        "is_finite" => Ok(Val::Bool(!(x.is_nan() || x.is_infinite()))),
        "max" => Ok(Val::Float(x.max(y))),
        "min" => Ok(Val::Float(x.min(y))),
        "powi" => Ok(Val::Float(x.powi(y as i32))),
        "rem" => Ok(Val::Float(if y != 0.0 { x % y } else { f64::NAN })),
        "atan2" => Ok(Val::Float(x.atan2(y))),
        _ => panic(format!("unknown float operation {}.{}", ty, name)),
    }
}

/// u128 はビットパターン (i128 に再解釈) で保持している。u128 の演算はここで行う。
fn u128_op(name: &str, a: &[Val]) -> Option<Res> {
    let u = |v: &Val| match v {
        Val::Int(x, _) => *x as u128,
        _ => 0,
    };
    let s = a.first().map(u).unwrap_or(0);
    let o = a.get(1).map(u).unwrap_or(0);
    let r = |v: u128| int(v as i128, "u128");
    let ck = |v: Option<u128>| Ok(match v {
        Some(x) => some(r(x)),
        None => none(),
    });
    Some(match name {
        "max" => Ok(r(s.max(o))),
        "min" => Ok(r(s.min(o))),
        "abs" => Ok(r(s)),
        "to_f64" => Ok(Val::Float(s as f64)),
        "successor" => ck(s.checked_add(1)),
        "count_ones" => Ok(int(s.count_ones() as i128, "u32")),
        "leading_zeros" => Ok(int(s.leading_zeros() as i128, "u32")),
        "trailing_zeros" => Ok(int(s.trailing_zeros() as i128, "u32")),
        "wrapping_add" => Ok(r(s.wrapping_add(o))),
        "wrapping_sub" => Ok(r(s.wrapping_sub(o))),
        "wrapping_mul" => Ok(r(s.wrapping_mul(o))),
        "wrapping_neg" => Ok(r(s.wrapping_neg())),
        "wrapping_shl" => Ok(r(s.wrapping_shl((o % 128) as u32))),
        "wrapping_shr" => Ok(r(s.wrapping_shr((o % 128) as u32))),
        "checked_add" => ck(s.checked_add(o)),
        "checked_sub" => ck(s.checked_sub(o)),
        "checked_mul" => ck(s.checked_mul(o)),
        "checked_div" => ck(s.checked_div(o)),
        "checked_rem" => ck(s.checked_rem(o)),
        "checked_neg" => ck(if s == 0 { Some(0) } else { None }),
        "checked_abs" => ck(Some(s)),
        "checked_shl" => ck(if o >= 128 { None } else { Some(s << o) }),
        "checked_shr" => ck(if o >= 128 { None } else { Some(s >> o) }),
        "saturating_add" => Ok(r(s.saturating_add(o))),
        "saturating_sub" => Ok(r(s.saturating_sub(o))),
        "saturating_mul" => Ok(r(s.saturating_mul(o))),
        "saturating_neg" => Ok(r(0)),
        _ => return None,
    })
}

fn item_list(v: &Val) -> Option<Rc<RefCell<Vec<Val>>>> {
    match v {
        Val::Arr(l) | Val::Vec(l) => Some(l.clone()),
        _ => None,
    }
}

fn deep(v: &Val) -> Val {
    match v {
        Val::Struct(s) => Val::Struct(Rc::new(StructV {
            ty: s.ty.clone(),
            f: RefCell::new(s.f.borrow().iter().map(|(k, x)| (k.clone(), deep(x))).collect()),
        })),
        Val::Enum(e) => mk_enum(&e.en, &e.var, e.pl.iter().map(deep).collect()),
        Val::Tuple(t) => Val::Tuple(Rc::new(t.iter().map(deep).collect())),
        Val::Arr(l) => Val::Arr(new_list(l.borrow().iter().map(deep).collect())),
        Val::Vec(l) => Val::Vec(new_list(l.borrow().iter().map(deep).collect())),
        Val::View(l, a, b) => Val::Arr(new_list(l.borrow()[*a..*b].iter().map(deep).collect())),
        Val::StrBuf(b) => Val::StrBuf(Rc::new(RefCell::new(b.borrow().clone()))),
        Val::Map(m) => Val::Map(Rc::new(RefCell::new(MapV {
            kind: m.borrow().kind.clone(),
            d: m.borrow().d.iter().map(|(k, x)| (deep(k), deep(x))).collect(),
        }))),
        other => other.clone(),
    }
}

impl Interp {
    pub fn num_const_val(&self, name: &str) -> Val {
        let (ty, n) = name.split_once('.').unwrap_or(("", name));
        if is_int_ty(ty) {
            return match n {
                "MAX" => int(tmax(ty), ty),
                "MIN" => int(tmin(ty), ty),
                _ => int(bits(ty) as i128, "u32"),
            };
        }
        let fv = match n {
            "INFINITY" => f64::INFINITY,
            "NEG_INFINITY" => f64::NEG_INFINITY,
            "NAN" => f64::NAN,
            "EPSILON" => {
                if ty == "f64" { 2.220446049250313e-16 } else { 1.1920929e-07 }
            }
            "MAX" => {
                if ty == "f64" { 1.7976931348623157e308 } else { 3.4028234663852886e38 }
            }
            _ => {
                if ty == "f64" { 2.2250738585072014e-308 } else { 1.1754943508222875e-38 }
            }
        };
        if ty == "f32" { Val::F32(fv as f32) } else { Val::Float(fv) }
    }

    fn place_of(&mut self, recv: &Val, v: &Val, place: Option<(&V, &Scope)>) -> Result<Rc<dyn Place>, Ctl> {
        if let Val::Ref(p) = recv {
            return Ok(p.clone());
        }
        if let Some((pe, sc)) = place {
            if let Val::Ref(p) = self.eval_place(pe, sc)? {
                return Ok(p);
            }
        }
        Ok(Rc::new(CellPlace(RefCell::new(v.clone()))))
    }

    pub fn call_native(&mut self, name: &str, args: Vec<Val>) -> Res {
        let a0 = args.first().cloned().unwrap_or(Val::Unit);
        match name {
            "core.mem.drop" => Ok(Val::Unit),
            "core.mem.swap" => {
                if let (Val::Ref(a), Val::Ref(b)) = (&args[0], &args[1]) {
                    let (x, y) = (a.get(), b.get());
                    a.set(y);
                    b.set(x);
                }
                Ok(Val::Unit)
            }
            "core.mem.replace" => {
                if let Val::Ref(d) = &args[0] {
                    let old = d.get();
                    d.set(args[1].clone());
                    return Ok(old);
                }
                panic("replace needs a reference")
            }
            "core.mem.size_of" => Ok(usz(8)),
            "core.panic.panic" => panic(self.display_str(&a0)?),
            "core.panic.todo" => panic("not yet implemented"),
            "core.panic.unreachable" => panic("unreachable"),
            "core.check.eq" => {
                let (a, b) = (dr(&args[0]), dr(&args[1]));
                if !self.v_eq(&a, &b)? {
                    let (l, r) = (self.debug_str(&args[0])?, self.debug_str(&args[1])?);
                    return panic(format!("assertion failed: left: {}, right: {}", l, r));
                }
                Ok(Val::Unit)
            }
            "core.check.is_true" => {
                if !matches!(dr(&a0), Val::Bool(true)) {
                    return panic("assertion failed: condition is false");
                }
                Ok(Val::Unit)
            }
            "std.io.print_line" => {
                let s = self.display_str(&a0)?;
                self.write_out(&s);
                self.write_out("\n");
                Ok(ok(Val::Unit))
            }
            "std.process.exit" => match dr(&a0) {
                Val::Int(c, _) => Err(Ctl::Exit(c as i32)),
                _ => Err(Ctl::Exit(1)),
            },
            "std.process.abort" => Err(Ctl::Exit(134)),
            "std.env.arg" => {
                let i = if let Val::Int(i, _) = dr(&a0) { i as usize } else { 0 };
                Ok(opt(self.args.get(i).map(|s| s_val(s))))
            }
            "std.env.arg_count" => Ok(usz(self.args.len())),
            "std.env.var" => Ok(none()),
            "core.iter.range" => Ok(Val::Iter(Rc::new(RefCell::new(IterV::Range { cur: dr(&args[0]), end: dr(&args[1]) })))),
            "core.iter.map" => {
                let src = self.to_iter(&args[0])?;
                Ok(Val::Iter(Rc::new(RefCell::new(IterV::Map { src, f: args[1].clone() }))))
            }
            "core.iter.filter" => {
                let src = self.to_iter(&args[0])?;
                Ok(Val::Iter(Rc::new(RefCell::new(IterV::Filter { src, f: args[1].clone() }))))
            }
            "core.iter.enumerate" => {
                let src = self.to_iter(&args[0])?;
                Ok(Val::Iter(Rc::new(RefCell::new(IterV::Enumerate { src, i: 0 }))))
            }
            "core.iter.take" => {
                let src = self.to_iter(&args[0])?;
                let n = if let Val::Int(n, _) = dr(&args[1]) { n } else { 0 };
                Ok(Val::Iter(Rc::new(RefCell::new(IterV::Take { src, n }))))
            }
            "core.iter.count" => {
                let src = self.to_iter(&args[0])?;
                let mut n = 0;
                loop {
                    let o = self.iter_next(&src)?;
                    if matches!(&o, Val::Enum(e) if e.var == "None") {
                        break;
                    }
                    n += 1;
                }
                Ok(usz(n))
            }
            "core.iter.fold" => {
                let src = self.to_iter(&args[0])?;
                let mut acc = args[1].clone();
                loop {
                    let o = self.iter_next(&src)?;
                    match &o {
                        Val::Enum(e) if e.var == "Some" => {
                            acc = self.call_value(&args[2], vec![acc, e.pl[0].clone()])?;
                        }
                        _ => return Ok(acc),
                    }
                }
            }
            "core.str.from_utf8" => {
                let bytes: Vec<u8> = seq_items(&dr(&a0)).unwrap_or_default().iter().map(|x| if let Val::Int(i, _) = x { *i as u8 } else { 0 }).collect();
                match String::from_utf8(bytes) {
                    Ok(s) => Ok(ok(s_val(&s))),
                    Err(e) => Ok(err(Val::Struct(Rc::new(StructV {
                        ty: "core.str.Utf8Error".into(),
                        f: RefCell::new(vec![("valid_up_to".into(), usz(e.utf8_error().valid_up_to()))]),
                    })))),
                }
            }
            "alloc.vec.Vec.new" => Ok(Val::Vec(new_list(vec![]))),
            "alloc.vec.Vec.with_capacity" => Ok(ok(Val::Vec(new_list(vec![])))),
            "alloc.string.String.new" => Ok(Val::StrBuf(Rc::new(RefCell::new(String::new())))),
            "alloc.string.String.from_str" => Ok(ok(Val::StrBuf(Rc::new(RefCell::new(str_of(&args[1]).unwrap_or_default()))))),
            "alloc.collections.VecDeque.new" => Ok(Val::Vec(new_list(vec![]))),
            "alloc.collections.HashMap.new" => Ok(Val::Map(Rc::new(RefCell::new(MapV { kind: "HashMap".into(), d: vec![] })))),
            "alloc.collections.BtreeMap.new" => Ok(Val::Map(Rc::new(RefCell::new(MapV { kind: "BtreeMap".into(), d: vec![] })))),
            "alloc.boxed.Box.new" => Ok(ok(Val::Struct(Rc::new(StructV {
                ty: "alloc.boxed.Box".into(),
                f: RefCell::new(vec![("value".into(), args[1].clone())]),
            })))),
            "std.time.Instant.now" => Ok(Val::Struct(Rc::new(StructV {
                ty: "std.time.Instant".into(),
                f: RefCell::new(vec![("ticks".into(), int(0, "u64"))]),
            }))),
            "std.time.Duration.from_millis" => {
                let ms = if let Val::Int(i, _) = dr(&a0) { i } else { 0 };
                Ok(Val::Struct(Rc::new(StructV {
                    ty: "std.time.Duration".into(),
                    f: RefCell::new(vec![("nanos".into(), int(ms * 1_000_000, "u64"))]),
                })))
            }
            "std.thread.spawn" => {
                let r = self.call_value(&args[1], vec![])?;
                Ok(ok(Val::Join(Rc::new(r))))
            }
            "std.thread.sleep" => {
                if let Val::Int(ms, _) = dr(&a0) {
                    std::thread::sleep(std::time::Duration::from_millis(ms as u64));
                }
                Ok(Val::Unit)
            }
            "std.thread.yield_now" => Ok(Val::Unit),
            "std.sync.channel" => {
                let q = Rc::new(RefCell::new(VecDeque::new()));
                Ok(ok(Val::Tuple(Rc::new(vec![Val::Chan(q.clone()), Val::Chan(q)]))))
            }
            "std.sync.Mutex.new" => Ok(Val::Mutex(Rc::new(CellPlace(RefCell::new(a0))))),
            "std.rand.Rng.from_seed" => Ok(Val::Rng(Rc::new(Cell::new(if let Val::Int(i, _) = dr(&a0) { i as u64 } else { 0 })))),
            "std.rand.Rng.from_entropy" => Ok(Val::Rng(Rc::new(Cell::new(0x9E3779B97F4A7C15)))),
            "std.fs.exists" => Ok(Val::Bool(std::path::Path::new(&str_of(&a0).unwrap_or_default()).exists())),
            "std.fs.remove_file" => {
                let path = str_of(&a0).unwrap_or_default();
                Ok(match std::fs::remove_file(&path) {
                    Ok(()) => ok(Val::Unit),
                    Err(e) => err(io_error(&e)),
                })
            }
            "std.fs.open" | "std.fs.create" => {
                let path = str_of(&a0).unwrap_or_default();
                let r = if name == "std.fs.open" {
                    std::fs::OpenOptions::new().read(true).open(&path)
                } else {
                    std::fs::File::create(&path)
                };
                Ok(match r {
                    Ok(f) => {
                        let fd = self.next_fd;
                        self.next_fd += 1;
                        self.files.insert(fd, f);
                        ok(Val::Struct(Rc::new(StructV {
                            ty: "std.fs.File".into(),
                            f: RefCell::new(vec![("descriptor".into(), int(fd, "i32"))]),
                        })))
                    }
                    Err(e) => err(io_error(&e)),
                })
            }
            "std.fs.read_to_end" => {
                use std::io::Read;
                let Some(fd) = file_fd(&a0) else { return panic("read_to_end: not a file") };
                let mut buf = Vec::new();
                let res = match self.files.get_mut(&fd) {
                    Some(f) => f.read_to_end(&mut buf),
                    None => return Ok(err(io_error_kind("InvalidInput"))),
                };
                match res {
                    Ok(n) => {
                        if let Some(out) = args.get(1) {
                            if let Some(l) = item_list(&dr(out)) {
                                l.borrow_mut().extend(buf.iter().map(|b| int(*b as i128, "u8")));
                            }
                        }
                        Ok(ok(int(n as i128, "usize")))
                    }
                    Err(e) => Ok(err(io_error(&e))),
                }
            }
            _ => {
                if let Some((ty, f)) = name.split_once('.') {
                    if is_int_ty(ty) || ty == "f32" || ty == "f64" || ty == "bool" || ty == "char" {
                        let args: Vec<Val> = args.iter().map(dr).collect();
                        let t = if is_int_ty(ty) && !(f.starts_with("from_") || f.starts_with("try_from_")) { pick_ty(&args, ty) } else { ty };
                        return num_op(f, t, &args);
                    }
                }
                panic(format!("unknown native {}", name))
            }
        }
    }

    /// `Ord` の提供メソッド `min` / `max` (仕様 4.3)。`cmp` に従い、等しいときは `min` が `self`、`max` が `other`。
    fn derived_min_max(&mut self, name: &str, me: &Val, other: &Val) -> Res {
        let c = self.v_cmp(me, other)?.unwrap_or(0);
        let pick_self = if name == "min" { c <= 0 } else { c > 0 };
        Ok(if pick_self { me.clone() } else { other.clone() })
    }

    pub fn native_method(&mut self, recv: &Val, v: &Val, name: &str, args: Vec<Val>, place: Option<(&V, &Scope)>) -> Res {
        let a0 = args.first().cloned().unwrap_or(Val::Unit);
        if name == "clone" || name == "try_clone" {
            let c = if matches!(v, Val::Closure(_) | Val::Func(_)) { v.clone() } else { deep(v) };
            return Ok(if name == "try_clone" { ok(c) } else { c });
        }
        // 派生比較メソッド: `&str`・`&[T]`・`[T; N]` (構成要素が比較できるとき) と Option / Result (derive)。
        // 演算子 `==` `<` 等と同じ `v_eq` / `v_cmp` に委ねる (spec 3.2 / 8.3)。
        if matches!(name, "eq" | "ne" | "cmp" | "partial_cmp")
            && (matches!(v, Val::Str(_) | Val::StrBuf(_) | Val::Arr(_) | Val::Vec(_) | Val::View(..))
                || matches!(v, Val::Enum(e) if e.en == "Option" || e.en == "Result"))
        {
            return self.derived_cmp(v, name, &a0);
        }
        if matches!(v, Val::Closure(_) | Val::Func(_)) {
            if matches!(name, "call" | "call_mut" | "call_once") {
                let a = seq_items(&dr(&a0)).unwrap_or_default();
                return self.call_value(v, a);
            }
            return panic(format!("no method {} on function", name));
        }
        match v {
            Val::Bool(_) | Val::Char(_) | Val::Int(..) | Val::Float(_) | Val::F32(_) => {
                // 派生メソッド (types.rs `derived_method` / check.rs `method_known`) の基本型への適用。
                match name {
                    "eq" => return Ok(Val::Bool(self.v_eq(v, &dr(&a0))?)),
                    "ne" => return Ok(Val::Bool(!self.v_eq(v, &dr(&a0))?)),
                    "cmp" => {
                        let c = self.v_cmp(v, &dr(&a0))?.unwrap_or(0);
                        return Ok(ordering(c));
                    }
                    "partial_cmp" => {
                        let c = self.v_cmp(v, &dr(&a0))?;
                        return Ok(c.map_or_else(none, |c| some(ordering(c))));
                    }
                    "min" | "max" if matches!(v, Val::Bool(_) | Val::Char(_)) => {
                        let c = self.v_cmp(v, &dr(&a0))?.unwrap_or(0);
                        let pick_self = if name == "min" { c <= 0 } else { c > 0 };
                        return Ok(if pick_self { v.clone() } else { dr(&a0) });
                    }
                    "hash" => return Ok(Val::Unit),
                    // `Display` は基本型に組み込み (仕様 4.2)。`to_string` はその表示文字列。
                    "to_string" => return Ok(Val::Str(self.display_str(v)?.into())),
                    // `core.fmt.Debug` / `Display` の組み込み実装 (仕様 4.2): 整形先の Formatter に追記する。
                    "debug_fmt" | "display_fmt" => {
                        let text = if name == "debug_fmt" { self.debug_str(v)? } else { self.display_str(v)? };
                        return match dr(&a0) {
                            Val::Fmt(b) => {
                                b.borrow_mut().push_str(&text);
                                Ok(ok(Val::Unit))
                            }
                            _ => panic(format!("{} expects a Formatter", name)),
                        };
                    }
                    _ => {}
                }
                let ty = match v {
                    Val::Float(_) => "f64",
                    Val::F32(_) => "f32",
                    Val::Char(_) => "char",
                    Val::Bool(_) => "bool",
                    _ => {
                        let mut all = vec![v.clone()];
                        all.extend(args.iter().cloned());
                        pick_ty(&all, "i32")
                    }
                };
                let mut all = vec![v.clone()];
                all.extend(args.iter().map(dr));
                num_op(name, ty, &all)
            }
            Val::Enum(e) if e.en == "Option" || e.en == "Result" => self.opt_res_method(recv, v, e.clone(), name, args, place),
            Val::Arr(_) | Val::Vec(_) | Val::View(..) => self.seq_method(v, name, args),
            Val::Str(_) | Val::StrBuf(_) => self.str_method(v, name, args),
            Val::Map(m) => self.map_method(m.clone(), name, args),
            Val::Fmt(b) => {
                if name == "write_str" {
                    b.borrow_mut().push_str(&str_of(&a0).unwrap_or_default());
                    return Ok(ok(Val::Unit));
                }
                panic("no such method on Formatter")
            }
            Val::Iter(it) => {
                if name == "next" {
                    return self.iter_next(&it.clone());
                }
                // core.slice.Iter.count (spec/stdlib/core/slice.sig): 残りを消費して数える。
                if name == "count" {
                    let src = it.clone();
                    let mut n = 0;
                    loop {
                        let o = self.iter_next(&src)?;
                        if matches!(&o, Val::Enum(e) if e.var == "None") {
                            break;
                        }
                        n += 1;
                    }
                    return Ok(usz(n));
                }
                panic(format!("no method {} on iterator", name))
            }
            Val::Join(r) => {
                if name == "join" {
                    return Ok((**r).clone());
                }
                panic("no such method on JoinHandle")
            }
            Val::Chan(q) => match name {
                "send" => {
                    q.borrow_mut().push_back(a0);
                    Ok(ok(Val::Unit))
                }
                "recv" => {
                    let x = q.borrow_mut().pop_front();
                    Ok(match x {
                        Some(x) => ok(x),
                        None => err(Val::Struct(Rc::new(StructV { ty: "std.sync.RecvError".into(), f: RefCell::new(vec![]) }))),
                    })
                }
                "drop" => Ok(Val::Unit),
                _ => panic(format!("no method {} on channel", name)),
            },
            Val::Mutex(c) => {
                if name == "lock" {
                    return Ok(Val::Struct(Rc::new(StructV {
                        ty: "std.sync.MutexGuard".into(),
                        f: RefCell::new(vec![("cell".into(), Val::Ref(c.clone()))]),
                    })));
                }
                panic("no such method on Mutex")
            }
            Val::Rng(s) => {
                if name == "next_u64" {
                    let (ns, z) = splitmix(s.get());
                    s.set(ns);
                    return Ok(int(z as i128, "u64"));
                }
                panic("no such method on Rng")
            }
            Val::Struct(s) => {
                if s.ty == "std.fs.File" {
                    return self.file_method(s, name, &args);
                }
                if s.ty == "std.sync.MutexGuard" && (name == "get" || name == "get_mut") {
                    return Ok(s.get("cell").unwrap());
                }
                if s.ty == "alloc.boxed.Box" {
                    if name == "as_ref" || name == "as_mut" {
                        return Ok(Val::Ref(Rc::new(FieldPlace(s.clone(), "value".into()))));
                    }
                    if name == "into_inner" {
                        return Ok(s.get("value").unwrap());
                    }
                }
                match name {
                    "partial_cmp" => {
                        let c = self.v_cmp(v, &dr(&a0))?;
                        return Ok(c.map_or_else(none, |c| some(ordering(c))));
                    }
                    "cmp" => {
                        let c = self.v_cmp(v, &dr(&a0))?.unwrap_or(0);
                        return Ok(ordering(c));
                    }
                    "eq" => return Ok(Val::Bool(self.v_eq(v, &dr(&a0))?)),
                    "min" | "max" => return self.derived_min_max(name, v, &dr(&a0)),
                    _ => {}
                }
                if s.ty == "std.time.Instant" && name == "elapsed" {
                    return Ok(Val::Struct(Rc::new(StructV {
                        ty: "std.time.Duration".into(),
                        f: RefCell::new(vec![("nanos".into(), int(0, "u64"))]),
                    })));
                }
                let d = self.debug_str(v)?;
                panic(format!("no method {} found for value {}", name, d))
            }
            Val::Enum(_) => match name {
                "eq" => Ok(Val::Bool(self.v_eq(v, &dr(&a0))?)),
                "cmp" => {
                    let c = self.v_cmp(v, &dr(&a0))?.unwrap_or(0);
                    Ok(ordering(c))
                }
                "min" | "max" => self.derived_min_max(name, v, &dr(&a0)),
                _ => {
                    let d = self.debug_str(v)?;
                    panic(format!("no method {} found for value {}", name, d))
                }
            },
            _ => {
                let d = self.debug_str(v)?;
                panic(format!("no method {} found for value {}", name, d))
            }
        }
    }

    fn opt_res_method(&mut self, recv: &Val, v: &Val, e: Rc<EnumV>, name: &str, args: Vec<Val>, place: Option<(&V, &Scope)>) -> Res {
        let isopt = e.en == "Option";
        let good = e.var == "Some" || e.var == "Ok";
        let x = e.pl.first().cloned();
        let a0 = args.first().cloned().unwrap_or(Val::Unit);
        match name {
            "is_some" | "is_ok" => Ok(Val::Bool(good)),
            "is_none" | "is_err" => Ok(Val::Bool(!good)),
            "unwrap" => {
                if good {
                    return Ok(x.unwrap());
                }
                if isopt {
                    panic("called `Option::unwrap()` on a `None` value")
                } else {
                    let d = self.debug_str(&x.unwrap())?;
                    panic(format!("called `Result::unwrap()` on an `Err` value: {}", d))
                }
            }
            "unwrap_or" => Ok(if good { x.unwrap() } else { a0 }),
            "map" => {
                if !good {
                    return Ok(v.clone());
                }
                let r = self.call_value(&a0, vec![x.unwrap()])?;
                Ok(mk_enum(&e.en, &e.var, vec![r]))
            }
            "and_then" => {
                if good { self.call_value(&a0, vec![x.unwrap()]) } else { Ok(v.clone()) }
            }
            "map_err" => {
                if good {
                    return Ok(v.clone());
                }
                let r = self.call_value(&a0, vec![x.unwrap()])?;
                Ok(err(r))
            }
            "ok_or" => Ok(if good { ok(x.unwrap()) } else { err(a0) }),
            "ok" => Ok(if good { some(x.unwrap()) } else { none() }),
            "as_ref" => Ok(v.clone()),
            "take" | "replace" | "as_mut" => {
                let p = self.place_of(recv, v, place)?;
                match name {
                    "take" => {
                        let old = p.get();
                        p.set(none());
                        Ok(old)
                    }
                    "replace" => {
                        let old = p.get();
                        p.set(some(a0));
                        Ok(old)
                    }
                    _ => {
                        let cur = p.get();
                        if matches!(&cur, Val::Enum(c) if c.var == "Some") {
                            Ok(some(Val::Ref(Rc::new(PlPlace(p, 0)))))
                        } else {
                            Ok(none())
                        }
                    }
                }
            }
            _ => panic(format!("no method {} on {}", name, e.en)),
        }
    }

    /// `eq` / `ne` / `cmp` / `partial_cmp` のメソッド形。演算子と同じ比較を使う。
    fn derived_cmp(&mut self, v: &Val, name: &str, a0: &Val) -> Res {
        let b = dr(a0);
        match name {
            "eq" => Ok(Val::Bool(self.v_eq(v, &b)?)),
            "ne" => Ok(Val::Bool(!self.v_eq(v, &b)?)),
            "cmp" => Ok(ordering(self.v_cmp(v, &b)?.unwrap_or(0))),
            _ => Ok(self.v_cmp(v, &b)?.map_or_else(none, |c| some(ordering(c)))),
        }
    }

    fn seq_method(&mut self, v: &Val, name: &str, args: Vec<Val>) -> Res {
        let a0 = args.first().cloned().unwrap_or(Val::Unit);
        let idx = |a: &Val| if let Val::Int(i, _) = dr(a) { i } else { -1 };
        let (list, base, len) = match v {
            Val::Arr(l) | Val::Vec(l) => (l.clone(), 0usize, l.borrow().len()),
            Val::View(l, a, b) => (l.clone(), *a, b - a),
            _ => unreachable!(),
        };
        match name {
            "len" | "capacity" => return Ok(usz(len)),
            "is_empty" => return Ok(Val::Bool(len == 0)),
            "get" => {
                let i = idx(&a0);
                return Ok(if i >= 0 && (i as usize) < len { some(list.borrow()[base + i as usize].clone()) } else { none() });
            }
            "get_mut" => {
                let i = idx(&a0);
                return Ok(if i >= 0 && (i as usize) < len { some(Val::Ref(Rc::new(IdxPlace(list, base + i as usize)))) } else { none() });
            }
            "first" => return Ok(if len > 0 { some(list.borrow()[base].clone()) } else { none() }),
            "last" => return Ok(if len > 0 { some(list.borrow()[base + len - 1].clone()) } else { none() }),
            "iter" => return Ok(Val::Iter(self.to_iter(v)?)),
            "sub" | "sub_mut" => {
                let (a, b) = (idx(&a0), idx(&args[1]));
                return Ok(if 0 <= a && a <= b && (b as usize) <= len {
                    some(Val::View(list, base + a as usize, base + b as usize))
                } else {
                    none()
                });
            }
            "split_at" => {
                let m = idx(&a0);
                return Ok(if 0 <= m && (m as usize) <= len {
                    some(Val::Tuple(Rc::new(vec![
                        Val::View(list.clone(), base, base + m as usize),
                        Val::View(list, base + m as usize, base + len),
                    ])))
                } else {
                    none()
                });
            }
            "swap" => {
                let (i, j) = (idx(&a0), idx(&args[1]));
                if !(0 <= i && (i as usize) < len && 0 <= j && (j as usize) < len) {
                    return Ok(err(Val::Struct(Rc::new(StructV {
                        ty: "core.slice.IndexError".into(),
                        f: RefCell::new(vec![("index".into(), usz(i.max(j) as usize))]),
                    }))));
                }
                list.borrow_mut().swap(base + i as usize, base + j as usize);
                return Ok(ok(Val::Unit));
            }
            "contains" => {
                let items = seq_items(v).unwrap_or_default();
                let t = dr(&a0);
                for x in &items {
                    if self.v_eq(x, &t)? {
                        return Ok(Val::Bool(true));
                    }
                }
                return Ok(Val::Bool(false));
            }
            "copy_from" => {
                let src = seq_items(&dr(&a0)).unwrap_or_default();
                if src.len() != len {
                    return Ok(err(Val::Struct(Rc::new(StructV {
                        ty: "core.slice.LengthError".into(),
                        f: RefCell::new(vec![("expected".into(), usz(len))]),
                    }))));
                }
                for (i, x) in src.into_iter().enumerate() {
                    list.borrow_mut()[base + i] = x;
                }
                return Ok(ok(Val::Unit));
            }
            "as_slice" | "as_mut_slice" => return Ok(v.clone()),
            _ => {}
        }
        if let Val::Vec(l) = v {
            match name {
                "push" => {
                    l.borrow_mut().push(a0);
                    return Ok(ok(Val::Unit));
                }
                "pop" => return Ok(opt(l.borrow_mut().pop())),
                // VecDeque は Vec と同じ実体 (先頭の出し入れは O(n))。
                "push_back" => {
                    l.borrow_mut().push(a0);
                    return Ok(ok(Val::Unit));
                }
                "push_front" => {
                    l.borrow_mut().insert(0, a0);
                    return Ok(ok(Val::Unit));
                }
                "pop_back" => return Ok(opt(l.borrow_mut().pop())),
                "pop_front" => {
                    let front = if l.borrow().is_empty() { None } else { Some(l.borrow_mut().remove(0)) };
                    return Ok(opt(front));
                }
                "clear" => {
                    l.borrow_mut().clear();
                    return Ok(Val::Unit);
                }
                "insert" => {
                    let i = idx(&a0);
                    if i < 0 || i as usize > l.borrow().len() {
                        return Ok(err(mk_enum("alloc.vec.InsertError", "OutOfBounds", vec![])));
                    }
                    l.borrow_mut().insert(i as usize, args[1].clone());
                    return Ok(ok(Val::Unit));
                }
                "remove" => {
                    let i = idx(&a0);
                    return Ok(if i >= 0 && (i as usize) < l.borrow().len() { some(l.borrow_mut().remove(i as usize)) } else { none() });
                }
                "truncate" => {
                    let n = idx(&a0).max(0) as usize;
                    l.borrow_mut().truncate(n);
                    return Ok(Val::Unit);
                }
                "reserve" => return Ok(ok(Val::Unit)),
                _ => {}
            }
        }
        panic(format!("no method {} on slice/Vec", name))
    }

    fn str_method(&mut self, v: &Val, name: &str, args: Vec<Val>) -> Res {
        let s = str_of(v).unwrap_or_default();
        let a0 = args.first().map(dr);
        match name {
            "len" => Ok(usz(s.len())),
            "is_empty" => Ok(Val::Bool(s.is_empty())),
            "chars" => Ok(Val::Iter(Rc::new(RefCell::new(IterV::Pre { items: s.chars().map(Val::Char).collect(), i: 0 })))),
            "as_bytes" => Ok(Val::Arr(new_list(s.bytes().map(|b| int(b as i128, "u8")).collect()))),
            "find" => {
                let t = a0.as_ref().and_then(str_of).unwrap_or_default();
                Ok(s.find(&t).map_or_else(none, |i| some(usz(i))))
            }
            "starts_with" => Ok(Val::Bool(s.starts_with(&a0.as_ref().and_then(str_of).unwrap_or_default()))),
            "split_at" => {
                let m = if let Some(Val::Int(m, _)) = a0 { m as usize } else { 0 };
                if m > s.len() || !s.is_char_boundary(m) {
                    return Ok(none());
                }
                Ok(some(Val::Tuple(Rc::new(vec![s_val(&s[..m]), s_val(&s[m..])]))))
            }
            "as_str" => Ok(s_val(&s)),
            // `Display` の文字列そのもの。型は `alloc.string.String` (types.rs) なので所有の StrBuf で返す。
            "to_string" => Ok(Val::StrBuf(Rc::new(RefCell::new(s)))),
            "push" | "push_str" | "clear" => {
                if let Val::StrBuf(b) = v {
                    match name {
                        "push" => {
                            if let Some(Val::Char(c)) = a0 {
                                b.borrow_mut().push(c);
                            }
                        }
                        "push_str" => b.borrow_mut().push_str(&a0.as_ref().and_then(str_of).unwrap_or_default()),
                        _ => b.borrow_mut().clear(),
                    }
                    return Ok(if name == "clear" { Val::Unit } else { ok(Val::Unit) });
                }
                panic(format!("no method {} on str", name))
            }
            _ => panic(format!("no method {} on str", name)),
        }
    }

    fn map_method(&mut self, m: Rc<RefCell<MapV>>, name: &str, args: Vec<Val>) -> Res {
        let a0 = args.first().cloned().unwrap_or(Val::Unit);
        let find = |m: &Rc<RefCell<MapV>>, k: &Val| {
            let hk = hkey(k);
            m.borrow().d.iter().position(|(x, _)| hkey(x) == hk)
        };
        match name {
            "insert" => {
                let pos = find(&m, &a0);
                let old = match pos {
                    Some(i) => {
                        let o = m.borrow().d[i].1.clone();
                        m.borrow_mut().d[i] = (a0, args[1].clone());
                        Some(o)
                    }
                    None => {
                        m.borrow_mut().d.push((a0, args[1].clone()));
                        None
                    }
                };
                Ok(ok(opt(old)))
            }
            "get" => Ok(find(&m, &a0).map_or_else(none, |i| some(m.borrow().d[i].1.clone()))),
            "get_mut" => Ok(find(&m, &a0).map_or_else(none, |i| some(Val::Ref(Rc::new(MapPlace(m.clone(), i)))))),
            "contains_key" => Ok(Val::Bool(find(&m, &a0).is_some())),
            "remove" => Ok(find(&m, &a0).map_or_else(none, |i| some(m.borrow_mut().d.remove(i).1))),
            "len" => Ok(usz(m.borrow().d.len())),
            "is_empty" => Ok(Val::Bool(m.borrow().d.is_empty())),
            "iter" => {
                let mut items: Vec<(Val, Val)> = m.borrow().d.clone();
                if m.borrow().kind == "BtreeMap" {
                    items.sort_by(|x, y| {
                        let c = vcmp(&x.0, &y.0, &|_, _| (0, 0)).unwrap_or(0);
                        c.cmp(&0)
                    });
                }
                Ok(Val::Iter(Rc::new(RefCell::new(IterV::Pre {
                    items: items.into_iter().map(|(k, v)| Val::Tuple(Rc::new(vec![k, v]))).collect(),
                    i: 0,
                }))))
            }
            _ => panic(format!("no method {} on map", name)),
        }
    }
}

#[allow(dead_code)]
fn unused(_: &V) {
    let _ = (items, st);
}

/// std.io.Error の値を作る。
fn io_error_kind(kind: &str) -> Val {
    Val::Struct(Rc::new(StructV {
        ty: "std.io.Error".into(),
        f: RefCell::new(vec![("kind".into(), mk_enum("std.io.ErrorKind", kind, vec![])), ("code".into(), int(0, "i32"))]),
    }))
}

fn io_error(e: &std::io::Error) -> Val {
    use std::io::ErrorKind as K;
    let kind = match e.kind() {
        K::NotFound => "NotFound",
        K::PermissionDenied => "PermissionDenied",
        K::AlreadyExists => "AlreadyExists",
        K::InvalidInput => "InvalidInput",
        K::UnexpectedEof => "UnexpectedEof",
        K::BrokenPipe => "BrokenPipe",
        K::Interrupted => "Interrupted",
        K::OutOfMemory => "OutOfMemory",
        _ => "Other",
    };
    Val::Struct(Rc::new(StructV {
        ty: "std.io.Error".into(),
        f: RefCell::new(vec![
            ("kind".into(), mk_enum("std.io.ErrorKind", kind, vec![])),
            ("code".into(), int(e.raw_os_error().unwrap_or(0) as i128, "i32")),
        ]),
    }))
}

fn file_fd(v: &Val) -> Option<i128> {
    if let Val::Struct(s) = dr(v) {
        if s.ty == "std.fs.File" {
            if let Some(Val::Int(fd, _)) = s.get("descriptor") {
                return Some(fd);
            }
        }
    }
    None
}

/// `&[u8]` / `&mut [u8]` / Vec<u8> の中身をバイト列にする。
fn bytes_of(v: &Val) -> Vec<u8> {
    seq_items(&dr(v)).unwrap_or_default().iter().map(|x| if let Val::Int(i, _) = dr(x) { i as u8 } else { 0 }).collect()
}

impl Interp {
    /// std.fs.File のメソッド (read / write / flush / close)。
    fn file_method(&mut self, s: &Rc<StructV>, name: &str, args: &[Val]) -> Res {
        use std::io::{Read, Write};
        let Some(Val::Int(fd, _)) = s.get("descriptor") else { return panic("invalid file") };
        match name {
            "close" => {
                self.files.remove(&fd);
                Ok(ok(Val::Unit))
            }
            "flush" => {
                if let Some(f) = self.files.get_mut(&fd) {
                    let _ = f.flush();
                }
                Ok(ok(Val::Unit))
            }
            "write" => {
                let data = bytes_of(args.first().unwrap_or(&Val::Unit));
                match self.files.get_mut(&fd) {
                    Some(f) => Ok(match f.write_all(&data) {
                        Ok(()) => ok(int(data.len() as i128, "usize")),
                        Err(e) => err(io_error(&e)),
                    }),
                    None => Ok(err(io_error_kind("InvalidInput"))),
                }
            }
            "read" => {
                let target = dr(args.first().unwrap_or(&Val::Unit));
                let cap = seq_items(&target).map_or(0, |x| x.len());
                let mut buf = vec![0u8; cap];
                let n = match self.files.get_mut(&fd) {
                    Some(f) => match f.read(&mut buf) {
                        Ok(n) => n,
                        Err(e) => return Ok(err(io_error(&e))),
                    },
                    None => return Ok(err(io_error_kind("InvalidInput"))),
                };
                match &target {
                    Val::View(l, a, _) => {
                        let mut l = l.borrow_mut();
                        for (i, b) in buf[..n].iter().enumerate() {
                            l[*a + i] = int(*b as i128, "u8");
                        }
                    }
                    Val::Arr(l) | Val::Vec(l) => {
                        let mut l = l.borrow_mut();
                        for (i, b) in buf[..n].iter().enumerate() {
                            l[i] = int(*b as i128, "u8");
                        }
                    }
                    _ => {}
                }
                Ok(ok(int(n as i128, "usize")))
            }
            _ => panic(format!("no method {} on File", name)),
        }
    }
}
