//! VM 上のソースレベル・デバッガ (`ksn debug` / `ksn test --debug`)。木走査版 (`debug.rs`) と同じコマンドと JSON の事件を出す。
//!
//! 仕組み: デバッグ用にコンパイルしたバイトコードは行が変わるところに `Ins::Line` を持つ。VM は付いているデバッガがあるときだけ
//! `Vm::dbg_line` を呼ぶ。停止中のローカル変数は、下ろしが記録した履歴 (`VFunc::evs`) と、フレームのスロットから読む。
//! 式の評価 (条件・`print`) は `uses [] panics never` の範囲に限る (`check_pure` と `pure_def` を木走査版と共有する)。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{BufRead, Write};
use std::rc::Rc;

use serde_json::{json, Value};

use super::*;
use crate::check::{children, nf, nname};
use crate::kir::{AggKind, Ty};
use crate::parser::V;

// ------------------------------------------------------------ 型

pub struct Bp {
    id: u32,
    kind: BpKind,
    cond: Option<(String, V)>,
    hits: u32,
}

enum BpKind {
    Line { file: String, line: usize },
    Func { name: String },
}

#[derive(Clone, Copy)]
enum Mode {
    Run,
    In,
    /// 呼び出しの深さがこれ以下になった最初の行で止まる
    Over(usize),
    /// これより浅くなった最初の行で止まる
    Out(usize),
}

pub struct BpRes {
    id: u32,
    kind: &'static str,
    verified: bool,
    file: String,
    line: usize,
    function: String,
    cond: Option<String>,
    message: String,
}

pub enum Resume {
    Continue,
    In,
    Next,
    Out,
    Quit,
}

/// 1 つの呼び出し。
#[derive(Clone, Copy)]
pub struct Fr {
    vi: u32,
    fi: u32,
    /// 行を探す命令の位置 (現在のフレームは `Line` 自身、呼び出し元は呼び出し命令)。
    pc: usize,
    base: usize,
}

pub struct Dbg {
    pub json: bool,
    pub user_files: HashSet<String>,
    pub pure: HashSet<String>,
    /// ソースのファイル → コードのある行 (ブレークポイントの置き直しの元)。
    pub code_lines: HashMap<String, BTreeSet<usize>>,
    pub func_at: HashMap<String, (String, usize)>,
    bps: Vec<Bp>,
    next_id: u32,
    mode: Mode,
    pub entry_stop: bool,
    pub started: bool,
    sel: usize,
    /// `finish` で戻る呼び出しの (戻り値の型, 受けるスロット, 呼び出し元の底)。
    finish: Option<(Ty, S, usize)>,
    src_cache: HashMap<String, Vec<String>>,
    pub last_err: Rc<std::cell::RefCell<Vec<u8>>>,
    pub quit: bool,
    /// 直前の `Line` の (関数, 底, 行)。同じ行の途中のブロックの先頭で、続けて止まらないための印。
    last_ev: Option<(u32, usize, u32)>,
    /// DAP のとき (端末のコマンドの代わりに DAP の要求を読む)。
    pub dap: Option<DapSt>,
    pub stop_on_panic: bool,
}

enum Flow {
    Stay,
    Run,
    Resume(Resume),
}

fn rel(p: &str) -> String {
    crate::cli::rel(p)
}

fn same_file(a: &str, b: &str) -> bool {
    let (a, b) = (a.replace('\\', "/"), b.replace('\\', "/"));
    let (a, b) = (a.trim_start_matches("./"), b.trim_start_matches("./"));
    if a == b || a.ends_with(&format!("/{}", b)) || b.ends_with(&format!("/{}", a)) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

// ------------------------------------------------------------ 値

#[derive(Clone, Debug)]
pub enum DV {
    /// 型が決まらない整数のリテラルは `None`。
    Int(i128, Option<Ty>),
    Bool(bool),
    F64(f64),
    F32(f32),
    Str(String),
    Unit,
    Struct(String, Vec<(String, DV)>),
    Tuple(Vec<DV>),
    Variant(String, Vec<DV>),
    Array(Vec<DV>),
    Opaque(String),
}

impl DV {
    fn show(&self) -> String {
        match self {
            DV::Int(x, _) => x.to_string(),
            DV::Bool(b) => b.to_string(),
            DV::F64(x) => crate::interp::py_float(*x),
            DV::F32(x) => crate::interp::py_float32(*x),
            DV::Str(s) => format!("{:?}", s),
            DV::Unit => "()".into(),
            DV::Struct(n, fs) => {
                if fs.is_empty() {
                    n.clone()
                } else {
                    format!("{} {{ {} }}", n, fs.iter().map(|(k, v)| format!("{}: {}", k, v.show())).collect::<Vec<_>>().join(", "))
                }
            }
            DV::Tuple(xs) => format!("({})", xs.iter().map(|x| x.show()).collect::<Vec<_>>().join(", ")),
            DV::Variant(n, xs) => {
                if xs.is_empty() {
                    n.clone()
                } else {
                    format!("{}({})", n, xs.iter().map(|x| x.show()).collect::<Vec<_>>().join(", "))
                }
            }
            DV::Array(xs) => format!("[{}]", xs.iter().map(|x| x.show()).collect::<Vec<_>>().join(", ")),
            DV::Opaque(s) => s.clone(),
        }
    }
}

fn short(n: &str) -> &str {
    n.rsplit('.').next().unwrap_or(n)
}

fn size_of(aggs: &[crate::kir::AggDef], t: Ty) -> usize {
    match t {
        Ty::Unit => 0,
        Ty::Agg(i) => aggs[i as usize].size as usize,
        Ty::Opt(_) | Ty::Res(..) => slots_of(aggs, t) as usize * 8,
        Ty::I128 | Ty::U128 => 16,
        Ty::Bool | Ty::I8 | Ty::U8 => 1,
        Ty::I16 | Ty::U16 => 2,
        Ty::I32 | Ty::U32 | Ty::F32 => 4,
        _ => 8,
    }
}

fn le(b: &[u8], n: usize) -> u128 {
    let mut x = 0u128;
    for i in (0..n.min(16)).rev() {
        x = (x << 8) | *b.get(i).unwrap_or(&0) as u128;
    }
    x
}

/// メモリの `n` バイト (範囲外・解放済みなら `None`)。
fn read_mem(a: u64, n: usize) -> Option<Vec<u8>> {
    if n == 0 {
        return Some(vec![]);
    }
    let p = super::super::arena::check(a, n)?;
    if super::super::arena::dead(a, n) {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(p as *const u8, n) }.to_vec())
}

/// バイト列 (型 `t` の配置) を値にする。`Option` / `Result` は 8 バイトのタグ + ペイロード (スロット列と同じ並び)。
fn from_bytes(aggs: &[crate::kir::AggDef], t: Ty, b: &[u8], depth: usize) -> DV {
    if depth > 6 {
        return DV::Opaque("...".into());
    }
    let int = |t: Ty| -> DV {
        let n = size_of(aggs, t);
        let raw = le(b, n);
        let v = if t.signed() {
            let sh = 128 - 8 * n as u32;
            ((raw << sh) as i128) >> sh
        } else {
            raw as i128
        };
        DV::Int(v, Some(t))
    };
    match t {
        Ty::Unit => DV::Unit,
        Ty::Bool => DV::Bool(b.first().copied().unwrap_or(0) & 1 != 0),
        Ty::F64 => DV::F64(f64::from_bits(le(b, 8) as u64)),
        Ty::F32 => DV::F32(f32::from_bits(le(b, 4) as u32)),
        Ty::Ptr => {
            // 局所の `Vec` / `String` はフレームのヘッダを指す `ptr` で持つ (型は配置に無い)。
            // 指す先が {ptr, len, cap} の形 (len <= cap、中身が読める) ならヘッダとして中身を出す。
            let a = le(b, 8) as u64;
            if let Some(h) = read_mem(a, 24) {
                let (p, len, cap) = (le(&h, 8) as u64, le(&h[8..], 8), le(&h[16..], 8));
                if len <= cap && cap < (1 << 40) && p != 0 && read_mem(p, (len as usize).min(4096)).is_some() {
                    return vec_value(aggs, "vec", &h);
                }
            }
            DV::Opaque(format!("ptr {:#x}", a))
        }
        Ty::Ref(_, p) => {
            let a = le(b, 8) as u64;
            let pt = p.ty();
            match read_mem(a, size_of(aggs, pt)) {
                Some(m) => from_bytes(aggs, pt, &m, depth + 1),
                None => DV::Opaque(format!("&{:#x}", a)),
            }
        }
        Ty::Opt(p) => {
            if le(b, 8) == 0 {
                DV::Variant("None".into(), vec![])
            } else {
                DV::Variant("Some".into(), vec![from_bytes(aggs, p.ty(), b.get(8..).unwrap_or(&[]), depth + 1)])
            }
        }
        Ty::Res(o, e) => {
            if le(b, 8) == 0 {
                DV::Variant("Ok".into(), vec![from_bytes(aggs, o.ty(), b.get(8..).unwrap_or(&[]), depth + 1)])
            } else {
                DV::Variant("Err".into(), vec![from_bytes(aggs, e.ty(), b.get(8..).unwrap_or(&[]), depth + 1)])
            }
        }
        Ty::Agg(i) => {
            let d = &aggs[i as usize];
            let fld = |f: &crate::kir::Field| from_bytes(aggs, f.ty, b.get(f.off as usize..).unwrap_or(&[]), depth + 1);
            if d.name == "fat" {
                let (p, n) = (le(b, 8) as u64, le(b.get(8..).unwrap_or(&[]), 8) as usize);
                return match read_mem(p, n.min(4096)) {
                    Some(m) => DV::Str(String::from_utf8_lossy(&m).to_string()),
                    None => DV::Opaque(format!("&[..; {}]", n)),
                };
            }
            match &d.kind {
                AggKind::Struct(fs) => {
                    if d.name == "vec" || d.name.starts_with("vec<#") {
                        return vec_value(aggs, &d.name, b);
                    }
                    DV::Struct(short(&d.name).to_string(), fs.iter().filter(|f| !f.name.starts_with('$')).map(|f| (f.name.clone(), fld(f))).collect())
                }
                AggKind::Tuple(fs) => DV::Tuple(fs.iter().map(fld).collect()),
                AggKind::Array(et, n) => {
                    let w = size_of(aggs, *et).max(1);
                    DV::Array((0..(*n as usize).min(64)).map(|k| from_bytes(aggs, *et, b.get(k * w..).unwrap_or(&[]), depth + 1)).collect())
                }
                AggKind::Enum(vs) => {
                    let tag = le(b, 4) as usize;
                    match vs.get(tag) {
                        Some(v) => DV::Variant(v.name.clone(), v.fields.iter().map(fld).collect()),
                        None => DV::Opaque(format!("{}(tag {})", short(&d.name), tag)),
                    }
                }
            }
        }
        _ => int(t),
    }
}

/// `Vec` / `String` のヘッダ {ptr, len, cap} から中身を出す。要素が所有物 (`vec<#N>`) なら要素の型 N で最大 64 個、
/// 要素がスカラーの `vec` は型が配置に無いので、読める印字可能な UTF-8 なら `String` / `Vec<u8>` として文字列、それ以外は `<vec len=N>`。
fn vec_value(aggs: &[crate::kir::AggDef], name: &str, b: &[u8]) -> DV {
    let (p, n) = (le(b, 8) as u64, le(b.get(8..).unwrap_or(&[]), 8) as usize);
    if let Some(ai) = name.strip_prefix("vec<#").and_then(|x| x.strip_suffix('>')).and_then(|x| x.parse::<u32>().ok()) {
        let et = Ty::Agg(ai);
        let w = size_of(aggs, et).max(1);
        let k = n.min(64);
        return match read_mem(p, k * w) {
            Some(m) if k > 0 => DV::Array((0..k).map(|i| from_bytes(aggs, et, &m[i * w..], 2)).collect()),
            Some(_) => DV::Array(vec![]),
            None => DV::Opaque(format!("<vec len={}>", n)),
        };
    }
    if n == 0 {
        return DV::Str(String::new());
    }
    match read_mem(p, n.min(4096)) {
        Some(m) => match std::str::from_utf8(&m) {
            Ok(t) if n <= 4096 && t.chars().all(|c| !c.is_control() || c == '\n' || c == '\t') => DV::Str(t.to_string()),
            _ => DV::Opaque(format!("<vec len={}>", n)),
        },
        None => DV::Opaque(format!("<vec len={}>", n)),
    }
}

fn dbg_method(recv: DV, name: &str, args: &[DV]) -> Result<DV, String> {
    let opaque_len = |s: &str| s.strip_prefix("<vec len=").and_then(|x| x.strip_suffix('>')).and_then(|x| x.parse::<i128>().ok());
    match (name, &recv, args) {
        ("len", DV::Str(s), []) => Ok(DV::Int(s.len() as i128, Some(Ty::Usize))),
        ("len", DV::Array(xs), []) => Ok(DV::Int(xs.len() as i128, Some(Ty::Usize))),
        ("len", DV::Opaque(s), []) if opaque_len(s).is_some() => Ok(DV::Int(opaque_len(s).unwrap_or(0), Some(Ty::Usize))),
        ("is_empty", DV::Str(s), []) => Ok(DV::Bool(s.is_empty())),
        ("is_empty", DV::Array(xs), []) => Ok(DV::Bool(xs.is_empty())),
        ("as_str", DV::Str(_), []) => Ok(recv),
        ("contains", DV::Str(s), [DV::Str(t)]) => Ok(DV::Bool(s.contains(t.as_str()))),
        ("starts_with", DV::Str(s), [DV::Str(t)]) => Ok(DV::Bool(s.starts_with(t.as_str()))),
        ("ends_with", DV::Str(s), [DV::Str(t)]) => Ok(DV::Bool(s.ends_with(t.as_str()))),
        _ => Err(format!("method `{}` is not supported on this value in a debugger expression", name)),
    }
}

fn is_dbg_method(m: &str) -> bool {
    matches!(m, "len" | "is_empty" | "as_str" | "contains" | "starts_with" | "ends_with")
}

fn words_to_bytes(w: &[u64]) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_le_bytes()).collect()
}

// ------------------------------------------------------------ Vm 側

impl<'a> Vm<'a> {
    fn prog_of(&self, vi: u32) -> &'a Prog {
        unsafe { &*self.vers[vi as usize] }
    }

    /// 現在の呼び出し列 (内側が先頭)。利用者のコードのフレームだけ。
    fn dbg_frames(&self, cur: Fr, d: &Dbg) -> Vec<Fr> {
        let mut out = vec![cur];
        for fr in self.frames.iter().rev() {
            out.push(Fr { vi: fr.v, fi: fr.f, pc: (fr.pc as usize).saturating_sub(1), base: fr.base as usize });
        }
        out.retain(|f| {
            let file = &self.prog_of(f.vi).funcs[f.fi as usize].file;
            !file.is_empty() && d.user_files.contains(file)
        });
        out
    }

    /// フレームの (行, その行の最初の命令の値番号)。
    fn fr_pos(&self, f: &Fr) -> (usize, u32) {
        let func = &self.prog_of(f.vi).funcs[f.fi as usize];
        let mut pc = f.pc.min(func.code.len().saturating_sub(1));
        loop {
            if let Ins::Line { line, vid } = &func.code[pc] {
                return (*line as usize, *vid);
            }
            if pc == 0 {
                return (func.line_at(f.pc) as usize, 0);
            }
            pc -= 1;
        }
    }

    /// フレームから見えるローカル変数 (宣言順)。
    fn fr_vars(&self, f: &Fr) -> Vec<(String, Ty, u32, bool)> {
        let (_, vid) = self.fr_pos(f);
        let func = &self.prog_of(f.vi).funcs[f.fi as usize];
        let mut cur: HashMap<u32, Option<&VEv>> = HashMap::new();
        for e in &func.evs {
            if e.at > vid {
                break;
            }
            cur.insert(e.bid, if e.kill { None } else { Some(e) });
        }
        let mut live: Vec<(u32, &VEv)> = cur.into_iter().filter_map(|(b, e)| e.map(|e| (b, e))).collect();
        live.sort_by_key(|x| x.0);
        let mut by_name: HashMap<&str, usize> = HashMap::new();
        for (i, (_, e)) in live.iter().enumerate() {
            by_name.insert(e.name.as_str(), i);
        }
        live.iter().enumerate().filter(|(i, (_, e))| by_name.get(e.name.as_str()) == Some(i)).map(|(_, (_, e))| (e.name.clone(), e.ty, e.slot, e.mem)).collect()
    }

    fn var_value(&self, f: &Fr, ty: Ty, slot: u32, mem: bool) -> DV {
        let aggs = &self.prog_of(f.vi).aggs;
        if mem {
            let a = self.regs[f.base + slot as usize];
            return match read_mem(a, size_of(aggs, ty)) {
                Some(m) => from_bytes(aggs, ty, &m, 0),
                None => DV::Opaque("<unreadable>".into()),
            };
        }
        let n = slots_of(aggs, ty) as usize;
        let w = &self.regs[f.base + slot as usize..f.base + slot as usize + n];
        from_bytes(aggs, ty, &words_to_bytes(w), 0)
    }

    pub(super) fn dbg_line(&mut self, vi: u32, fi: u32, pc: usize, base: usize, line: u32, _vid: u32) -> Option<Outcome> {
        let mut d = self.dbg.take()?;
        let func = &self.prog_of(vi).funcs[fi as usize];
        if func.file.is_empty() || !d.user_files.contains(&func.file) {
            self.dbg = Some(d);
            return None;
        }
        let ev = (fi, base, line);
        if d.last_ev.replace(ev) == Some(ev) {
            self.dbg = Some(d);
            return None;
        }
        let cur = Fr { vi, fi, pc, base };
        let depth = self.frames.len();
        let line = line as usize;
        let mut hit: Vec<u32> = vec![];
        let mut reason = String::new();
        let mut message = String::new();
        let first_line = func.code.iter().position(|i| matches!(i, Ins::Line { .. })) == Some(pc);
        for i in 0..d.bps.len() {
            let m = match &d.bps[i].kind {
                BpKind::Line { file, line: l } => *l == line && same_file(&func.file, file),
                BpKind::Func { name } => first_line && func.key == *name,
            };
            if !m {
                continue;
            }
            if let Some((_, ast)) = d.bps[i].cond.clone() {
                match d.eval(self, &cur, &ast) {
                    Ok(DV::Bool(false)) => continue,
                    Ok(DV::Bool(true)) => {}
                    Ok(_) => message = "breakpoint condition is not a bool; stopping".into(),
                    Err(e) => message = format!("breakpoint condition failed ({}); stopping", e),
                }
            }
            d.bps[i].hits += 1;
            hit.push(d.bps[i].id);
            if reason.is_empty() {
                reason = if matches!(d.bps[i].kind, BpKind::Func { .. }) { "function breakpoint" } else { "breakpoint" }.into();
            }
        }
        if reason.is_empty() {
            let stepping = match d.mode {
                Mode::Run => false,
                Mode::In => true,
                Mode::Over(n) => depth <= n,
                Mode::Out(n) => depth < n,
            };
            if !stepping {
                self.dbg = Some(d);
                return None;
            }
            reason = if std::mem::replace(&mut d.entry_stop, false) { "entry" } else { "step" }.into();
            if let (Mode::Out(_), Some((ty, dst, cbase))) = (d.mode, d.finish.take()) {
                if dst != NO_SLOT {
                    let aggs = &self.prog_of(vi).aggs;
                    let n = slots_of(aggs, ty) as usize;
                    let w = &self.regs[cbase + dst as usize..cbase + dst as usize + n];
                    message = format!("returned {}", from_bytes(aggs, ty, &words_to_bytes(w), 0).show());
                }
            }
        }
        d.finish = None;
        let r = d.stop(self, cur, &reason, &message, &hit);
        let out = match r {
            Resume::Quit => {
                d.quit = true;
                Some(Outcome::Exit(0))
            }
            _ => None,
        };
        self.dbg = Some(d);
        out
    }

    /// panic で終わった直後に、panic の場所で止まる (`stopped: panic`)。
    pub fn dbg_panic_stop(&mut self, message: &str) {
        let Some(mut d) = self.dbg.take() else { return };
        if !d.stop_on_panic {
            self.dbg = Some(d);
            return;
        }
        if let Some((vi, fi, pc)) = self.panic_at {
            let cur = Fr { vi, fi, pc: pc as usize, base: self.panic_base };
            let frames = self.dbg_frames(cur, &d);
            if !frames.is_empty() {
                d.stop(self, frames[0], "panic", message, &[]);
            }
        }
        self.dbg = Some(d);
    }
}

// ------------------------------------------------------------ 式の評価

impl Dbg {
    fn lookup(&self, vm: &Vm, f: &Fr, name: &str) -> Option<DV> {
        vm.fr_vars(f).into_iter().find(|v| v.0 == name).map(|(_, ty, slot, mem)| vm.var_value(f, ty, slot, mem))
    }

    pub fn parse_cond(&self, text: &str) -> Result<V, String> {
        let ast = crate::debug::parse_expr(text)?;
        crate::debug::check_pure(&ast)?;
        self.check_calls(&ast)?;
        Ok(ast)
    }

    fn check_calls(&self, e: &V) -> Result<(), String> {
        use crate::check::strs;
        match nname(e) {
            "Call" if nname(&nf(e)[0]) == "Path" => {
                let segs = strs(&nf(&nf(e)[0])[0]);
                if segs.len() >= 2 && !self.pure.contains(&segs.join(".")) && !(segs.len() == 2 && is_dbg_method(&segs[1]) && !segs[0].contains('.')) {
                    return Err(format!("`{}` is not allowed in a debugger expression: it is not `uses [] panics never` (or takes `&mut`)", segs.join(".")));
                }
            }
            "MCall" => {
                let m = crate::check::st(&nf(e)[1]);
                if !matches!(m.as_str(), "len" | "is_empty" | "as_str" | "contains" | "starts_with" | "ends_with") {
                    return Err(format!("method `{}` is not supported in a debugger expression (len is_empty as_str contains starts_with ends_with)", m));
                }
            }
            _ => {}
        }
        for c in children(e) {
            self.check_calls(c)?;
        }
        Ok(())
    }

    pub fn eval(&mut self, vm: &mut Vm, f: &Fr, e: &V) -> Result<DV, String> {
        use crate::check::{st, strs};
        let fs = nf(e);
        match nname(e) {
            "Lit" => match st(&fs[0]).as_str() {
                "int" => {
                    let sfx = match &fs[2] {
                        V::S(s) => ity(s),
                        _ => None,
                    };
                    Ok(DV::Int(crate::interp::parse_int(&st(&fs[1])), sfx))
                }
                "float" => Ok(DV::F64(st(&fs[2]).parse::<f64>().unwrap_or(0.0))),
                "bool" => Ok(DV::Bool(matches!(fs[1], V::B(true)))),
                "str" => Ok(DV::Str(crate::interp::unhex(&st(&fs[1])))),
                _ => Err("this literal is not supported in a debugger expression".into()),
            },
            "Path" => {
                let segs = strs(&fs[0]);
                if let Some(mut v) = self.lookup(vm, f, &segs[0]) {
                    for name in &segs[1..] {
                        v = match v {
                            DV::Struct(_, xs) => xs.into_iter().find(|(k, _)| k == name).map(|x| x.1).ok_or(format!("no field {}", name))?,
                            DV::Tuple(xs) => name.parse::<usize>().ok().and_then(|i| xs.get(i).cloned()).ok_or(format!("no field {}", name))?,
                            _ => return Err(format!("no field {}", name)),
                        };
                    }
                    return Ok(v);
                }
                Err(format!("no such variable `{}`", segs.join(".")))
            }
            "Field" => {
                let b = self.eval(vm, f, &fs[0])?;
                let name = st(&fs[1]);
                match b {
                    DV::Struct(_, xs) => xs.into_iter().find(|(k, _)| *k == name).map(|x| x.1).ok_or(format!("no field {}", name)),
                    DV::Tuple(xs) => name.parse::<usize>().ok().and_then(|i| xs.get(i).cloned()).ok_or(format!("no field {}", name)),
                    _ => Err(format!("no field {}", name)),
                }
            }
            "Unary" => {
                let op = st(&fs[0]);
                let v = self.eval(vm, f, &fs[1])?;
                match (op.as_str(), v) {
                    ("&", v) | ("*", v) => Ok(v),
                    ("-", DV::Int(x, t)) => Ok(DV::Int(-x, t)),
                    ("-", DV::F64(x)) => Ok(DV::F64(-x)),
                    ("!", DV::Bool(b)) => Ok(DV::Bool(!b)),
                    _ => Err(format!("bad unary {}", op)),
                }
            }
            "Bin" => {
                let op = st(&fs[0]);
                if op == "&&" || op == "||" {
                    let a = self.eval(vm, f, &fs[1])?;
                    let DV::Bool(a) = a else { return Err("`&&` / `||` need bool operands".into()) };
                    if (op == "&&") != a {
                        return Ok(DV::Bool(a));
                    }
                    return match self.eval(vm, f, &fs[2])? {
                        DV::Bool(b) => Ok(DV::Bool(b)),
                        _ => Err("`&&` / `||` need bool operands".into()),
                    };
                }
                let a = self.eval(vm, f, &fs[1])?;
                let b = self.eval(vm, f, &fs[2])?;
                bin(&op, a, b)
            }
            "Call" => {
                let tgt = &fs[0];
                if nname(tgt) != "Path" {
                    return Err("only calls of named functions are supported in a debugger expression".into());
                }
                let segs = strs(&nf(tgt)[0]);
                let key = segs.join(".");
                let args: Vec<DV> = crate::check::items(&fs[1]).iter().map(|a| self.eval(vm, f, a)).collect::<Result<_, _>>()?;
                // `s.len()` は `s` と `len` の 2 部のパスの呼び出しとして読まれる。`s` が局所変数なら受け手 `s` の組み込みメソッド。
                if segs.len() == 2 && !self.pure.contains(&key) {
                    if let Some(recv) = self.lookup(vm, f, &segs[0]) {
                        return dbg_method(recv, &segs[1], &args);
                    }
                }
                self.call(vm, f, &key, &args)
            }
            "MCall" => {
                let recv = self.eval(vm, f, &fs[0])?;
                let name = st(&fs[1]);
                let args: Vec<DV> = crate::check::items(&fs[2]).iter().map(|a| self.eval(vm, f, a)).collect::<Result<_, _>>()?;
                dbg_method(recv, &name, &args)
            }
            "Index" => {
                let b = self.eval(vm, f, &fs[0])?;
                let i = self.eval(vm, f, &fs[1])?;
                match (b, i) {
                    (DV::Array(xs), DV::Int(i, _)) => usize::try_from(i).ok().and_then(|i| xs.get(i).cloned()).ok_or_else(|| "index out of range".to_string()),
                    _ => Err("only an integer index into an array or Vec is supported in a debugger expression".into()),
                }
            }
            n => Err(format!("`{}` is not allowed in a debugger expression", n)),
        }
    }

    /// 純粋な関数 `key` を、停止中の VM の上で走らせる (ネストした実行)。
    fn call(&mut self, vm: &mut Vm, f: &Fr, key: &str, args: &[DV]) -> Result<DV, String> {
        if !self.pure.contains(key) {
            return Err(format!("`{}` is not allowed in a debugger expression: it is not `uses [] panics never` (or takes `&mut`)", key));
        }
        let prog = vm.prog_of(f.vi);
        let Some(&idx) = prog.by_key.get(key) else { return Err(format!("`{}` is not part of the running program", key)) };
        let func = &prog.funcs[idx as usize];
        if func.ptys.len() != args.len() {
            return Err(format!("`{}` takes {} arguments", key, func.ptys.len()));
        }
        let mut words: Vec<u64> = vec![];
        for (a, t) in args.iter().zip(func.ptys.iter()) {
            match (a, t) {
                (DV::Int(x, _), t) if t.is_int() => words.push(*x as u64),
                (DV::Bool(b), Ty::Bool) => words.push(*b as u64),
                (DV::F64(x), Ty::F64) => words.push(x.to_bits()),
                _ => return Err("only integer, bool and float arguments are supported in a debugger call".into()),
            }
        }
        let cur_func = &prog.funcs[f.fi as usize];
        let saved_top = vm.top;
        let saved_res = vm.result.clone();
        let saved_panic = (vm.panic_at, vm.panic_base);
        vm.top = f.base + cur_func.nslots as usize + 64;
        // 呼び出し元の現在のフレームより上だけを使う (最も内側のフレームの `nslots` を超える部分)。
        let r = vm.exec(f.vi, idx, &words, true);
        let out = vm.result.clone();
        vm.top = saved_top;
        vm.result = saved_res;
        (vm.panic_at, vm.panic_base) = saved_panic;
        match r {
            Ok(Outcome::Value(_)) => Ok(from_bytes(&prog.aggs, func.ret, &words_to_bytes(&out), 0)),
            Ok(Outcome::Panic(m)) => Err(format!("the call panicked: {}", m)),
            _ => Err("the call did not finish".into()),
        }
    }
}

fn ity(s: &str) -> Option<Ty> {
    Some(match s {
        "i8" => Ty::I8,
        "i16" => Ty::I16,
        "i32" => Ty::I32,
        "i64" => Ty::I64,
        "i128" => Ty::I128,
        "isize" => Ty::Isize,
        "u8" => Ty::U8,
        "u16" => Ty::U16,
        "u32" => Ty::U32,
        "u64" => Ty::U64,
        "u128" => Ty::U128,
        "usize" => Ty::Usize,
        _ => return None,
    })
}

fn range(t: Ty) -> (i128, i128) {
    let bits = match t {
        Ty::I8 | Ty::U8 => 8,
        Ty::I16 | Ty::U16 => 16,
        Ty::I32 | Ty::U32 => 32,
        Ty::I128 | Ty::U128 => 127,
        _ => 64,
    };
    if t.signed() {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else if bits == 127 {
        (0, i128::MAX)
    } else {
        (0, (1i128 << bits) - 1)
    }
}

fn bin(op: &str, a: DV, b: DV) -> Result<DV, String> {
    match (a, b) {
        (DV::Int(x, tx), DV::Int(y, ty)) => {
            let t = tx.or(ty);
            let c = |r: Option<i128>, what: &str| -> Result<DV, String> {
                let r = r.ok_or(format!("attempt to {} with overflow", what))?;
                let (lo, hi) = range(t.unwrap_or(Ty::I32));
                if r < lo || r > hi {
                    return Err(format!("attempt to {} with overflow", what));
                }
                Ok(DV::Int(r, t))
            };
            match op {
                "+" => c(x.checked_add(y), "add"),
                "-" => c(x.checked_sub(y), "subtract"),
                "*" => c(x.checked_mul(y), "multiply"),
                "/" => {
                    if y == 0 {
                        return Err("attempt to divide by zero".into());
                    }
                    c(x.checked_div(y), "divide")
                }
                "%" => {
                    if y == 0 {
                        return Err("attempt to calculate the remainder with a divisor of zero".into());
                    }
                    c(x.checked_rem(y), "calculate the remainder")
                }
                "==" => Ok(DV::Bool(x == y)),
                "!=" => Ok(DV::Bool(x != y)),
                "<" => Ok(DV::Bool(x < y)),
                "<=" => Ok(DV::Bool(x <= y)),
                ">" => Ok(DV::Bool(x > y)),
                ">=" => Ok(DV::Bool(x >= y)),
                "&" => Ok(DV::Int(x & y, t)),
                "|" => Ok(DV::Int(x | y, t)),
                "^" => Ok(DV::Int(x ^ y, t)),
                _ => Err(format!("bad operator {}", op)),
            }
        }
        (DV::Bool(x), DV::Bool(y)) => match op {
            "==" => Ok(DV::Bool(x == y)),
            "!=" => Ok(DV::Bool(x != y)),
            "&" => Ok(DV::Bool(x & y)),
            "|" => Ok(DV::Bool(x | y)),
            "^" => Ok(DV::Bool(x ^ y)),
            _ => Err(format!("bad operator {}", op)),
        },
        (DV::F64(x), DV::F64(y)) => match op {
            "+" => Ok(DV::F64(x + y)),
            "-" => Ok(DV::F64(x - y)),
            "*" => Ok(DV::F64(x * y)),
            "/" => Ok(DV::F64(x / y)),
            "==" => Ok(DV::Bool(x == y)),
            "!=" => Ok(DV::Bool(x != y)),
            "<" => Ok(DV::Bool(x < y)),
            "<=" => Ok(DV::Bool(x <= y)),
            ">" => Ok(DV::Bool(x > y)),
            ">=" => Ok(DV::Bool(x >= y)),
            _ => Err(format!("bad operator {}", op)),
        },
        (DV::Str(x), DV::Str(y)) => match op {
            "==" => Ok(DV::Bool(x == y)),
            "!=" => Ok(DV::Bool(x != y)),
            _ => Err(format!("bad operator {}", op)),
        },
        _ => Err(format!("operands of `{}` do not match", op)),
    }
}

// ------------------------------------------------------------ 端末フロントエンド

impl Dbg {
    pub fn new(json: bool, user_files: HashSet<String>, pure: HashSet<String>) -> Dbg {
        Dbg {
            json,
            user_files,
            pure,
            code_lines: HashMap::new(),
            func_at: HashMap::new(),
            bps: vec![],
            next_id: 1,
            mode: Mode::Run,
            entry_stop: false,
            started: false,
            sel: 0,
            finish: None,
            src_cache: HashMap::new(),
            last_err: Default::default(),
            quit: false,
            last_ev: None,
            dap: None,
            stop_on_panic: true,
        }
    }

    /// 最初の行で止まる (`step` を `run` の前に打った)。
    pub fn stop_on_entry(&mut self) {
        self.mode = Mode::In;
        self.entry_stop = true;
    }

    pub fn emit(&self, j: Value, text: String) {
        if self.json {
            println!("{}", j);
        } else if !text.is_empty() {
            println!("{}", text);
        }
        let _ = std::io::stdout().flush();
    }

    fn err(&self, msg: &str) {
        self.emit(json!({"event": "error", "message": msg}), format!("error: {}", msg));
    }

    fn source_line(&mut self, file: &str, line: usize) -> String {
        let lines = self.src_cache.entry(file.to_string()).or_insert_with(|| std::fs::read_to_string(file).unwrap_or_default().lines().map(String::from).collect());
        lines.get(line.wrapping_sub(1)).map(|l| l.trim().to_string()).unwrap_or_default()
    }

    /// 文の桁 (1 始まり)。書き方は 1 通りなので、1 行に 1 文で、桁は字下げの幅 + 1。
    fn col_of(&mut self, file: &str, line: usize) -> usize {
        let lines = self.src_cache.entry(file.to_string()).or_insert_with(|| std::fs::read_to_string(file).unwrap_or_default().lines().map(String::from).collect());
        lines.get(line.wrapping_sub(1)).map_or(1, |l| l.len() - l.trim_start().len() + 1)
    }

    fn find_user_file(&self, pat: &str) -> Option<String> {
        let mut fs: Vec<&String> = self.user_files.iter().filter(|f| same_file(f, pat)).collect();
        fs.sort();
        fs.first().map(|s| s.to_string())
    }

    fn show_bp(&self, r: &BpRes) {
        let j = json!({
            "event": "breakpoint", "id": r.id, "kind": r.kind, "verified": r.verified,
            "file": if r.file.is_empty() { Value::Null } else { json!(rel(&r.file)) },
            "line": r.line, "function": r.function, "condition": r.cond, "message": r.message,
        });
        let t = if r.verified {
            let at = if r.kind == "line" { format!("{}:{}", rel(&r.file), r.line) } else { r.function.clone() };
            let c = r.cond.as_ref().map_or(String::new(), |c| format!(" if {}", c));
            format!("breakpoint {} at {}{}", r.id, at, c)
        } else {
            format!("breakpoint {} not set: {}", r.id, r.message)
        };
        self.emit(j, t);
    }

    fn make_bp(&mut self, kind_line: Option<(&str, usize)>, func: Option<&str>, cond: Option<&str>) -> BpRes {
        let id = self.next_id;
        self.next_id += 1;
        let mut r = BpRes { id, kind: if kind_line.is_some() { "line" } else { "function" }, verified: false, file: String::new(), line: 0, function: String::new(), cond: cond.map(String::from), message: String::new() };
        let cond_ast = match cond.map(str::trim).filter(|c| !c.is_empty()) {
            None => None,
            Some(t) => match self.parse_cond(t) {
                Ok(a) => Some((t.to_string(), a)),
                Err(e) => {
                    r.function = func.unwrap_or("").to_string();
                    r.message = e;
                    return r;
                }
            },
        };
        if let Some((pat, line)) = kind_line {
            r.file = pat.to_string();
            r.line = line;
            let Some(file) = self.find_user_file(pat) else {
                r.message = format!("no source file matches `{}` in this package", pat);
                return r;
            };
            let Some(&actual) = self.code_lines.get(&file).and_then(|s| s.range(line..).next()) else {
                r.file = file;
                r.message = format!("no code at or after line {}", line);
                return r;
            };
            r.verified = true;
            r.file = file.clone();
            r.line = actual;
            self.bps.push(Bp { id, kind: BpKind::Line { file, line: actual }, cond: cond_ast, hits: 0 });
        } else if let Some(name) = func {
            let name = name.trim();
            let name = name.strip_prefix("fn ").unwrap_or(name).trim().to_string();
            r.function = name.clone();
            let Some((file, line)) = self.func_at.get(&name).cloned() else {
                r.message = format!("no function `{}`", name);
                return r;
            };
            r.verified = true;
            r.file = file;
            r.line = line;
            self.bps.push(Bp { id, kind: BpKind::Func { name }, cond: cond_ast, hits: 0 });
        }
        r
    }

    fn set_bp(&mut self, kind_line: Option<(&str, usize)>, func: Option<&str>, cond: Option<&str>) {
        let r = self.make_bp(kind_line, func, cond);
        self.show_bp(&r);
    }

    fn cmd_break(&mut self, sel_file: Option<String>, rest: &str) {
        let (target, cond) = match rest.find(" if ") {
            Some(i) => (rest[..i].trim(), Some(rest[i + 4..].trim())),
            None => (rest.trim(), None),
        };
        if target.is_empty() {
            return self.err("usage: break <file>:<line> | <function FQN> [if <expr>]");
        }
        if let Some(t) = target.strip_prefix("fn ") {
            self.set_bp(None, Some(t), cond);
        } else if let Some((f, l)) = target.rsplit_once(':').filter(|(_, l)| l.parse::<usize>().is_ok()) {
            self.set_bp(Some((f, l.parse().unwrap())), None, cond);
        } else if let Ok(l) = target.parse::<usize>() {
            match sel_file {
                Some(f) => self.set_bp(Some((&f, l)), None, cond),
                None => self.err("a bare line number needs a stopped frame; use <file>:<line>"),
            }
        } else {
            self.set_bp(None, Some(target), cond);
        }
    }

    fn frame_json(&mut self, vm: &Vm, i: usize, f: &Fr) -> Value {
        let func = &vm.prog_of(f.vi).funcs[f.fi as usize];
        let (line, _) = vm.fr_pos(f);
        let src = self.source_line(&func.file, line);
        json!({"index": i, "function": func.key, "file": rel(&func.file), "line": line, "column": 0, "source": src})
    }

    fn cmd_bt(&mut self, vm: &Vm, frames: &[Fr]) {
        let mut arr = vec![];
        let mut text = vec![];
        for (i, f) in frames.iter().enumerate() {
            let j = self.frame_json(vm, i, f);
            text.push(format!("#{} {} {}:{}", i, j["function"].as_str().unwrap_or(""), j["file"].as_str().unwrap_or(""), j["line"]));
            arr.push(j);
        }
        self.emit(json!({"event": "stack", "thread": 1, "frames": arr}), text.join("\n"));
    }

    fn cmd_locals(&mut self, vm: &Vm, frames: &[Fr], arg: &str) {
        let idx = arg.trim().parse::<usize>().unwrap_or(self.sel);
        let Some(f) = frames.get(idx) else { return self.err("no such frame") };
        let mut arr = vec![];
        let mut text = vec![];
        for (name, ty, slot, mem) in vm.fr_vars(f) {
            let s = vm.var_value(f, ty, slot, mem).show();
            text.push(format!("{} = {}", name, s));
            arr.push(json!({"name": name, "value": s}));
        }
        if text.is_empty() {
            text.push("(no locals)".into());
        }
        self.emit(json!({"event": "locals", "frame": idx, "vars": arr}), text.join("\n"));
    }

    fn cmd_print(&mut self, vm: &mut Vm, frames: &[Fr], expr: &str) {
        if expr.trim().is_empty() {
            return self.err("usage: print <expr>");
        }
        let Some(f) = frames.get(self.sel).copied() else { return self.err("no frame (the program is not stopped)") };
        let r = self.parse_cond(expr).and_then(|a| self.eval(vm, &f, &a));
        match r {
            Ok(v) => {
                let v = v.show();
                self.emit(json!({"event": "value", "expr": expr.trim(), "value": v}), format!("{} = {}", expr.trim(), v));
            }
            Err(e) => self.err(&e),
        }
    }

    /// 1 つのコマンド。`stop` は停止中なら (VM, 呼び出し列)。
    fn handle(&mut self, mut stop: Option<(&mut Vm, &[Fr])>, line: &str) -> Flow {
        let line = line.trim();
        let (cmd, rest) = match line.split_once(char::is_whitespace) {
            Some((c, r)) => (c, r.trim()),
            None => (line, ""),
        };
        let stopped = stop.is_some();
        macro_rules! need_stop {
            () => {
                if !stopped {
                    self.err("the program is not stopped");
                    return Flow::Stay;
                }
            };
        }
        match cmd {
            "break" | "b" => {
                let file = stop.as_ref().and_then(|(vm, fr)| fr.get(self.sel).map(|f| vm.prog_of(f.vi).funcs[f.fi as usize].file.clone()));
                self.cmd_break(file, rest);
            }
            "delete" | "d" => {
                if rest == "all" || rest.is_empty() {
                    self.bps.clear();
                    self.emit(json!({"event": "deleted", "ids": "all"}), "all breakpoints deleted".into());
                } else {
                    for w in rest.split_whitespace() {
                        let id = w.trim_start_matches('#').parse::<u32>().ok();
                        match id.filter(|i| self.bps.iter().any(|b| b.id == *i)) {
                            Some(id) => {
                                self.bps.retain(|b| b.id != id);
                                self.emit(json!({"event": "deleted", "ids": [id]}), format!("breakpoint {} deleted", id));
                            }
                            None => self.err(&format!("no such breakpoint `{}`", w)),
                        }
                    }
                }
            }
            "breakpoints" | "info" => {
                let mut arr = vec![];
                let mut text = vec![];
                for b in &self.bps {
                    let (kind, at) = match &b.kind {
                        BpKind::Line { file, line } => ("line", format!("{}:{}", rel(file), line)),
                        BpKind::Func { name } => ("function", name.clone()),
                    };
                    arr.push(json!({"id": b.id, "kind": kind, "at": at, "condition": b.cond.as_ref().map(|c| c.0.clone()), "hits": b.hits}));
                    text.push(format!("{} {} {}{} (hits {})", b.id, kind, at, b.cond.as_ref().map_or(String::new(), |c| format!(" if {}", c.0)), b.hits));
                }
                if text.is_empty() {
                    text.push("(no breakpoints)".into());
                }
                self.emit(json!({"event": "breakpoints", "breakpoints": arr}), text.join("\n"));
            }
            "run" | "r" | "start" => {
                if self.started {
                    self.err("the program is already running; use `continue`");
                } else {
                    self.started = true;
                    return Flow::Run;
                }
            }
            "continue" | "c" => {
                if !self.started {
                    self.err("the program has not started; use `run`");
                } else {
                    need_stop!();
                    return Flow::Resume(Resume::Continue);
                }
            }
            "step" | "s" | "stepin" => {
                if !self.started {
                    self.stop_on_entry();
                    self.started = true;
                    return Flow::Run;
                }
                need_stop!();
                return Flow::Resume(Resume::In);
            }
            "next" | "n" => {
                need_stop!();
                return Flow::Resume(Resume::Next);
            }
            "finish" | "out" => {
                need_stop!();
                return Flow::Resume(Resume::Out);
            }
            "bt" | "backtrace" | "where" => {
                need_stop!();
                if let Some((vm, fr)) = &stop {
                    self.cmd_bt(vm, fr);
                }
            }
            "frame" | "f" => {
                need_stop!();
                let (vm, fr) = stop.as_ref().unwrap();
                match rest.parse::<usize>() {
                    Ok(n) if n < fr.len() => {
                        self.sel = n;
                        let j = self.frame_json(vm, n, &fr[n]);
                        self.emit(json!({"event": "frame", "frame": j}), format!("#{} {} {}:{}", n, j["function"].as_str().unwrap_or(""), j["file"].as_str().unwrap_or(""), j["line"]));
                    }
                    _ => self.err("usage: frame <index>"),
                }
            }
            "locals" | "l" => {
                need_stop!();
                if let Some((vm, fr)) = &stop {
                    self.cmd_locals(vm, fr, rest);
                }
            }
            "print" | "p" | "eval" => {
                need_stop!();
                if let Some((vm, fr)) = stop.as_mut() {
                    self.cmd_print(vm, fr, rest);
                }
            }
            "threads" => self.emit(json!({"event": "threads", "threads": [{"id": 1, "name": "main"}]}), "1 main".into()),
            "quit" | "q" | "exit" => return Flow::Resume(Resume::Quit),
            "" => {}
            c => self.err(&format!("unknown command `{}` (break delete breakpoints run continue step next finish bt frame locals print threads quit)", c)),
        }
        Flow::Stay
    }

    pub fn read_line() -> Option<String> {
        let mut s = String::new();
        loop {
            s.clear();
            match std::io::stdin().lock().read_line(&mut s) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {
                    let t = s.trim();
                    if t.is_empty() || t.starts_with('#') {
                        continue;
                    }
                    return Some(t.to_string());
                }
            }
        }
    }

    /// `run` の前のコマンド。`Run` を返したら実行を始める。`None` は終了。
    pub fn prompt_before_run(&mut self) -> Option<bool> {
        loop {
            let line = Self::read_line()?;
            match self.handle(None, &line) {
                Flow::Run => return Some(true),
                Flow::Resume(Resume::Quit) => return Some(false),
                _ => {}
            }
        }
    }

    fn stop(&mut self, vm: &mut Vm, cur: Fr, reason: &str, message: &str, bp_ids: &[u32]) -> Resume {
        vm.flush();
        let frames = vm.dbg_frames(cur, self);
        self.sel = 0;
        let r = if self.dap.is_some() { self.dap_stopped(vm, &frames, reason, message, bp_ids) } else { self.term_stopped(vm, &frames, reason, message, bp_ids) };
        let depth = vm.frames.len();
        self.mode = match r {
            Resume::Continue => Mode::Run,
            Resume::In => Mode::In,
            Resume::Next => Mode::Over(depth),
            Resume::Out => {
                let ty = vm.prog_of(cur.vi).funcs[cur.fi as usize].ret;
                self.finish = vm.frames.last().map(|fr| (ty, fr.dst, fr.base as usize));
                Mode::Out(depth)
            }
            Resume::Quit => Mode::Run,
        };
        r
    }

    fn term_stopped(&mut self, vm: &mut Vm, frames: &[Fr], reason: &str, message: &str, bp_ids: &[u32]) -> Resume {
        if let Some(f0) = frames.first() {
            let fj = self.frame_json(vm, 0, f0);
            let (name, file, line) = (fj["function"].as_str().unwrap_or("").to_string(), fj["file"].as_str().unwrap_or("").to_string(), fj["line"].clone());
            let src = fj["source"].as_str().unwrap_or("").to_string();
            let mut t = match reason {
                "panic" => format!("stopped: panic \"{}\" in {} at {}:{}", message, name, file, line),
                r => format!("stopped: {}{} in {} at {}:{}", r, if bp_ids.is_empty() { String::new() } else { format!(" #{}", bp_ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")) }, name, file, line),
            };
            if reason != "panic" && !message.is_empty() {
                t.push_str(&format!(" ({})", message));
            }
            t.push_str(&format!("\n  {} | {}", line, src));
            self.emit(json!({"event": "stopped", "reason": reason, "message": message, "breakpoints": bp_ids, "thread": 1, "frame": fj}), t);
        }
        loop {
            let Some(line) = Self::read_line() else { break Resume::Quit };
            match self.handle(Some((vm, frames)), &line) {
                Flow::Resume(r) => break r,
                Flow::Run | Flow::Stay => {}
            }
        }
    }
}

pub fn flush_json_out(json: bool, dap: bool) -> Box<dyn FnMut(u8, &[u8])> {
    Box::new(move |fd, b| {
        if dap {
            dap_event("output", json!({"category": if fd == 1 { "stdout" } else { "stderr" }, "output": String::from_utf8_lossy(b).to_string()}));
        } else if json {
            let s = String::from_utf8_lossy(b).to_string();
            println!("{}", json!({"event": "output", "stream": if fd == 1 { "stdout" } else { "stderr" }, "text": s}));
            let _ = std::io::stdout().flush();
        } else if fd == 1 {
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(b);
            let _ = o.flush();
        } else {
            let _ = crate::kir_exec::write_fd(2, b);
        }
    })
}

// ------------------------------------------------------------ DAP

static SEQ: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);

pub fn dap_write(mut v: Value) {
    v["seq"] = json!(SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
    let body = v.to_string();
    let out = std::io::stdout();
    let mut o = out.lock();
    let _ = write!(o, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = o.flush();
}

pub fn dap_event(name: &str, body: Value) {
    dap_write(json!({"type": "event", "event": name, "body": body}));
}

enum VarRef {
    Locals(usize),
    Val(DV),
}

pub struct DapSt {
    pub lines1: bool,
    pub cols1: bool,
    refs: Vec<VarRef>,
    stopped_now: bool,
    pub stop_on_entry: bool,
}

impl DapSt {
    pub fn new(lines1: bool, cols1: bool, stop_on_entry: bool) -> DapSt {
        DapSt { lines1, cols1, refs: vec![], stopped_now: false, stop_on_entry }
    }
}

enum DFlow {
    Stay,
    Run,
    Resume(Resume),
    Disconnect,
}

fn children_of(v: &DV) -> Vec<(String, DV)> {
    match v {
        DV::Struct(_, fs) => fs.clone(),
        DV::Tuple(xs) | DV::Array(xs) => xs.iter().enumerate().map(|(i, x)| (i.to_string(), x.clone())).collect(),
        DV::Variant(_, xs) => xs.iter().enumerate().map(|(i, x)| (i.to_string(), x.clone())).collect(),
        _ => vec![],
    }
}

fn respond(req: &Value, ok: bool, body: Value, message: Option<&str>) {
    let mut r = json!({"type": "response", "request_seq": req["seq"], "success": ok, "command": req["command"], "body": body});
    if let Some(m) = message {
        r["message"] = json!(m);
    }
    dap_write(r);
}

pub fn dap_read() -> Option<Value> {
    use std::io::Read;
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if lock.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let l = line.trim_end();
        if l.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().ok();
            }
        }
    }
    let mut buf = vec![0u8; len?];
    lock.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

impl Dbg {
    fn dl(&self) -> &DapSt {
        self.dap.as_ref().unwrap()
    }

    fn line_out(&self, l: usize) -> usize {
        if self.dl().lines1 { l } else { l.saturating_sub(1) }
    }

    fn col_out(&self, c: usize) -> usize {
        if self.dl().cols1 { c } else { c.saturating_sub(1) }
    }

    fn line_in(&self, l: usize) -> usize {
        if self.dl().lines1 { l } else { l + 1 }
    }

    fn bp_json(&self, r: &BpRes) -> Value {
        let mut j = json!({"id": r.id, "verified": r.verified});
        if r.verified && r.kind == "line" {
            j["line"] = json!(self.line_out(r.line));
        }
        if !r.message.is_empty() {
            j["message"] = json!(r.message);
        }
        j
    }

    fn new_ref(&mut self, r: VarRef) -> i64 {
        let d = self.dap.as_mut().unwrap();
        d.refs.push(r);
        d.refs.len() as i64
    }

    fn var_json(&mut self, name: &str, v: &DV) -> Value {
        let r = if children_of(v).is_empty() { 0 } else { self.new_ref(VarRef::Val(v.clone())) };
        json!({"name": name, "value": v.show(), "variablesReference": r})
    }

    fn dap_handle(&mut self, mut stop: Option<(&mut Vm, &[Fr])>, req: &Value) -> DFlow {
        let cmd = req["command"].as_str().unwrap_or("");
        let a = &req["arguments"];
        match cmd {
            "setBreakpoints" => {
                let path = a["source"]["path"].as_str().unwrap_or("").to_string();
                self.bps.retain(|b| !matches!(&b.kind, BpKind::Line { file, .. } if same_file(file, &path)));
                let mut out = vec![];
                let reqs: Vec<(usize, Option<String>)> = match a["breakpoints"].as_array() {
                    Some(bs) => bs.iter().map(|b| (b["line"].as_u64().unwrap_or(0) as usize, b["condition"].as_str().map(String::from))).collect(),
                    None => a["lines"].as_array().map(|ls| ls.iter().map(|l| (l.as_u64().unwrap_or(0) as usize, None)).collect()).unwrap_or_default(),
                };
                for (l, c) in reqs {
                    let r = self.make_bp(Some((&path, self.line_in(l))), None, c.as_deref());
                    out.push(self.bp_json(&r));
                }
                respond(req, true, json!({"breakpoints": out}), None);
            }
            "setFunctionBreakpoints" => {
                self.bps.retain(|b| !matches!(b.kind, BpKind::Func { .. }));
                let mut out = vec![];
                for b in a["breakpoints"].as_array().cloned().unwrap_or_default() {
                    let r = self.make_bp(None, Some(b["name"].as_str().unwrap_or("")), b["condition"].as_str());
                    out.push(self.bp_json(&r));
                }
                respond(req, true, json!({"breakpoints": out}), None);
            }
            "setExceptionBreakpoints" => {
                self.stop_on_panic = a["filters"].as_array().map_or(true, |f| f.iter().any(|x| x == "panic"));
                respond(req, true, json!({}), None);
            }
            "configurationDone" => {
                respond(req, true, json!({}), None);
                return DFlow::Run;
            }
            "threads" => respond(req, true, json!({"threads": [{"id": 1, "name": "main"}]}), None),
            "stackTrace" => {
                let Some((vm, frames)) = stop.as_ref() else {
                    respond(req, true, json!({"stackFrames": [], "totalFrames": 0}), None);
                    return DFlow::Stay;
                };
                let start = a["startFrame"].as_u64().unwrap_or(0) as usize;
                let levels = a["levels"].as_u64().filter(|&n| n > 0).map_or(usize::MAX, |n| n as usize);
                let mut out = vec![];
                for (i, f) in frames.iter().enumerate().skip(start).take(levels) {
                    let func = &vm.prog_of(f.vi).funcs[f.fi as usize];
                    let (line, _) = vm.fr_pos(f);
                    let col = self.col_of(&func.file, line);
                    let name = std::path::Path::new(&func.file).file_name().map_or(String::new(), |n| n.to_string_lossy().to_string());
                    out.push(json!({
                        "id": i + 1, "name": func.key,
                        "source": {"name": name, "path": func.file},
                        "line": self.line_out(line), "column": self.col_out(col),
                        "presentationHint": "normal",
                    }));
                }
                respond(req, true, json!({"stackFrames": out, "totalFrames": frames.len()}), None);
            }
            "scopes" => {
                let fid = a["frameId"].as_u64().unwrap_or(1) as usize - 1;
                let r = self.new_ref(VarRef::Locals(fid));
                respond(req, true, json!({"scopes": [{"name": "Locals", "variablesReference": r, "expensive": false}]}), None);
            }
            "variables" => {
                let r = a["variablesReference"].as_i64().unwrap_or(0);
                let kind = if r >= 1 {
                    self.dl().refs.get(r as usize - 1).map(|k| match k {
                        VarRef::Locals(i) => (Some(*i), None),
                        VarRef::Val(v) => (None, Some(v.clone())),
                    })
                } else {
                    None
                };
                let mut out = vec![];
                match kind {
                    Some((Some(idx), _)) => {
                        if let Some((vm, frames)) = stop.as_ref() {
                            if let Some(f) = frames.get(idx) {
                                let vals: Vec<(String, DV)> = vm.fr_vars(f).into_iter().map(|(n, ty, slot, mem)| (n, vm.var_value(f, ty, slot, mem))).collect();
                                for (k, v) in vals {
                                    out.push(self.var_json(&k, &v));
                                }
                            }
                        }
                    }
                    Some((_, Some(v))) => {
                        for (k, c) in children_of(&v) {
                            out.push(self.var_json(&k, &c));
                        }
                    }
                    _ => {}
                }
                respond(req, true, json!({"variables": out}), None);
            }
            "evaluate" => {
                let idx = match a["frameId"].as_u64() {
                    Some(f) if f >= 1 => f as usize - 1,
                    _ => 0,
                };
                let r = match stop.as_mut() {
                    Some((vm, frames)) => match frames.get(idx).copied() {
                        Some(f) => self.parse_cond(a["expression"].as_str().unwrap_or("")).and_then(|ast| self.eval(vm, &f, &ast)),
                        None => Err("no frame (the program is not stopped)".to_string()),
                    },
                    None => Err("no frame (the program is not stopped)".to_string()),
                };
                match r {
                    Ok(v) => {
                        let r = if children_of(&v).is_empty() { 0 } else { self.new_ref(VarRef::Val(v.clone())) };
                        respond(req, true, json!({"result": v.show(), "variablesReference": r}), None);
                    }
                    Err(e) => respond(req, false, json!({}), Some(&e)),
                }
            }
            "continue" | "next" | "stepIn" | "stepOut" => {
                if !self.dl().stopped_now {
                    respond(req, false, json!({}), Some("the program is not stopped"));
                    return DFlow::Stay;
                }
                let body = if cmd == "continue" { json!({"allThreadsContinued": true}) } else { json!({}) };
                respond(req, true, body, None);
                return DFlow::Resume(match cmd {
                    "continue" => Resume::Continue,
                    "next" => Resume::Next,
                    "stepIn" => Resume::In,
                    _ => Resume::Out,
                });
            }
            "disconnect" | "terminate" => {
                respond(req, true, json!({}), None);
                return DFlow::Disconnect;
            }
            "exceptionInfo" => respond(req, true, json!({"exceptionId": "panic", "breakMode": "always"}), None),
            other => respond(req, false, json!({}), Some(&format!("unsupported request `{}`", other))),
        }
        DFlow::Stay
    }

    /// 構成の要求 (ブレークポイントなど) を `configurationDone` まで受ける。`false` は切断。
    pub fn dap_prelude(&mut self) -> bool {
        loop {
            let Some(req) = dap_read() else { return false };
            match self.dap_handle(None, &req) {
                DFlow::Run => return true,
                DFlow::Disconnect => return false,
                _ => {}
            }
        }
    }

    fn dap_stopped(&mut self, vm: &mut Vm, frames: &[Fr], reason: &str, message: &str, bp_ids: &[u32]) -> Resume {
        let dap = self.dap.as_mut().unwrap();
        dap.stopped_now = true;
        dap.refs.clear();
        let rs = if reason == "panic" { "exception" } else { reason };
        let mut body = json!({"reason": rs, "threadId": 1, "allThreadsStopped": true});
        if !message.is_empty() {
            body["text"] = json!(message);
            body["description"] = json!(if reason == "panic" { format!("panic: {}", message) } else { message.to_string() });
        }
        if !bp_ids.is_empty() {
            body["hitBreakpointIds"] = json!(bp_ids);
        }
        if reason == "panic" {
            dap_event("output", json!({"category": "stderr", "output": format!("panic: {}\n", message)}));
        }
        dap_event("stopped", body);
        let r = loop {
            let Some(req) = dap_read() else { break Resume::Quit };
            match self.dap_handle(Some((vm, frames)), &req) {
                DFlow::Resume(r) => break r,
                DFlow::Disconnect => break Resume::Quit,
                _ => {}
            }
        };
        let dap = self.dap.as_mut().unwrap();
        dap.stopped_now = false;
        dap.refs.clear();
        r
    }

    /// DAP の終わり: 出力 (panic・`Error:`)、`exited` / `terminated`、切断を待つ。
    pub fn dap_finished(&mut self, code: i32, panic_msg: Option<String>, err_msg: Option<String>, quit: bool) {
        if quit {
            return;
        }
        if let Some(m) = panic_msg {
            dap_event("output", json!({"category": "stderr", "output": format!("panic: {}\n", m)}));
        }
        if let Some(m) = err_msg {
            dap_event("output", json!({"category": "stderr", "output": format!("Error: {}\n", m.trim_start_matches("Error: "))}));
        }
        dap_event("exited", json!({"exitCode": code}));
        dap_event("terminated", json!({}));
        while let Some(req) = dap_read() {
            let cmd = req["command"].as_str().unwrap_or("");
            if matches!(cmd, "disconnect" | "terminate") {
                respond(&req, true, json!({}), None);
                break;
            }
            if cmd == "threads" {
                respond(&req, true, json!({"threads": []}), None);
            } else {
                respond(&req, false, json!({}), Some("the program has exited"));
            }
        }
    }
}
