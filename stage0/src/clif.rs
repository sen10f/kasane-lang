//! Cranelift バックエンド (stage0)。`cnative.rs` (C 経由) と同じ部分集合を、
//! Cranelift の IR から直接オブジェクトファイルにして、C コンパイラをリンカとして使って実行ファイルにする。
//!
//! 対応する範囲: 整数 (i8..i64 / u8..u64 / isize / usize)・bool・`let` / 代入・`if` / `while` / `loop` /
//! `break` / `continue` / `return`・関数呼び出し・`wrapping_*`・比較・ビット演算・`&&` / `||`・
//! `std.io.print_line("..")`・`core.check.eq`・`core.mem.drop`。それ以外は Err を返す (呼び出し側が別の経路に回す)。

use std::collections::{HashMap, HashSet};

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{types, AbiParam, Block, InstBuilder, TrapCode, Type, Value};
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

fn is_scalar(p: &str) -> bool {
    is_int(p) || p == "bool"
}

fn cl(p: &str) -> Type {
    match p {
        "i8" | "u8" | "bool" => types::I8,
        "i16" | "u16" => types::I16,
        "i32" | "u32" => types::I32,
        _ => types::I64,
    }
}

fn signed(p: &str) -> bool {
    p.starts_with('i')
}

fn mangle(fqn: &str) -> String {
    format!("k_{}", fqn.replace('.', "_"))
}

fn pty(t: &V) -> R<String> {
    if tag(t) == "tuple" && items(tel(t, 1)).is_empty() {
        return Ok("void".into());
    }
    if tag(t) == "prim" {
        let p = st(tel(t, 1));
        if is_scalar(&p) {
            return Ok(p);
        }
    }
    Err(format!("type {}", t.dump()))
}

/// ランタイムとして使う C ライブラリ関数の ID。
struct Rt {
    puts: FuncId,
    write: FuncId,
    exit: FuncId,
}

struct FnGen<'a, 'b> {
    b: FunctionBuilder<'b>,
    module: &'a mut ObjectModule,
    funcs: &'a HashMap<String, &'a Def>,
    ids: &'a HashMap<String, FuncId>,
    rt: &'a Rt,
    data_n: &'a mut usize,
    entry: bool,
    ret: V,
    loops: Vec<(Block, Block)>,
    term: bool,
    nvar: usize,
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

    fn ty_of(&self, e: &V, env: &Env, expect: Option<&str>) -> Option<String> {
        let f = nf(e);
        match nname(e) {
            "Lit" => match st(&f[0]).as_str() {
                "int" => {
                    let sfx = if matches!(f[2], V::Nil) { None } else { Some(st(&f[2])) };
                    sfx.or(expect.map(String::from)).filter(|t| is_int(t))
                }
                "bool" => Some("bool".into()),
                _ => None,
            },
            "Path" => {
                let segs = strs(&f[0]);
                if segs.len() == 1 { env.get(&segs[0]).map(|x| x.1.clone()) } else { None }
            }
            "Bin" => {
                let op = st(&f[0]);
                if matches!(op.as_str(), "&&" | "||" | "==" | "!=" | "<" | "<=" | ">" | ">=") {
                    return Some("bool".into());
                }
                self.ty_of(&f[1], env, expect).or_else(|| self.ty_of(&f[2], env, expect))
            }
            "Unary" if st(&f[0]) == "!" => self.ty_of(&f[1], env, expect),
            "MCall" => self.ty_of(&f[0], env, expect).or_else(|| items(&f[2]).first().and_then(|a| self.ty_of(a, env, expect))),
            "Call" => {
                if nname(&f[0]) != "Path" {
                    return None;
                }
                let segs = strs(&nf(&f[0])[0]);
                if segs.len() >= 2 && env.contains_key(&segs[0]) {
                    let recv = V::Node("Path", vec![V::L(segs[..segs.len() - 1].iter().map(|x| V::S(x.clone())).collect()), V::Nil], 0);
                    return self.ty_of(&recv, env, expect).or_else(|| items(&f[1]).first().and_then(|a| self.ty_of(a, env, expect)));
                }
                if segs == ["core", "check", "eq"] || segs == ["core", "mem", "drop"] {
                    return Some("void".into());
                }
                self.funcs.get(&segs.join(".")).and_then(|d| pty(&d.ret).ok())
            }
            _ => None,
        }
    }

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
        let pt = self.module.target_config().pointer_type();
        Ok(self.b.ins().global_value(pt, gv))
    }

    fn fail_block(&mut self) -> R<()> {
        let msg = b"panic: assertion failed\n".to_vec();
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

    fn func(&mut self, d: &Def, params_vals: &[Value]) -> R<()> {
        let mut env: Env = HashMap::new();
        for (k, (n, t)) in d.params.iter().enumerate() {
            let ty = pty(t)?;
            let var = self.new_var(&ty);
            self.b.def_var(var, params_vals[k]);
            env.insert(n.clone(), (var, ty));
        }
        let stmts = items(&nf(d.body.as_ref().unwrap())[0]).to_vec();
        for (i, s) in stmts.iter().enumerate() {
            self.stmt(s, &mut env, i == stmts.len() - 1)?;
        }
        if !self.term {
            if self.entry {
                let z = self.b.ins().iconst(types::I32, 0);
                self.b.ins().return_(&[z]);
            } else if pty(&self.ret)? == "void" {
                self.b.ins().return_(&[]);
            } else {
                self.b.ins().trap(TrapCode::user(2).unwrap());
            }
        }
        Ok(())
    }

    fn block(&mut self, b: &V, env: &mut Env, tail: bool) -> R<()> {
        let stmts = items(&nf(b)[0]).to_vec();
        for (i, s) in stmts.iter().enumerate() {
            self.stmt(s, env, tail && i == stmts.len() - 1)?;
        }
        Ok(())
    }

    fn stmt(&mut self, s: &V, env: &mut Env, tail: bool) -> R<()> {
        self.live();
        let f = nf(s);
        match nname(s) {
            "Let" => {
                if nname(&f[0]) != "PBind" {
                    return Err("destructuring let".into());
                }
                let ann = if matches!(f[2], V::Nil) { None } else { Some(pty(&f[2])?) };
                let ty = match ann.clone().or_else(|| self.ty_of(&f[3], env, None)) {
                    Some(t) if t != "void" => t,
                    _ => return Err("let of unknown type".into()),
                };
                let (v, _) = self.expr(&f[3], env, Some(&ty))?;
                let v = v.ok_or("let of unit")?;
                let var = self.new_var(&ty);
                self.b.def_var(var, v);
                env.insert(st(&nf(&f[0])[0]), (var, ty));
                Ok(())
            }
            "Assign" => {
                let tg = &f[0];
                if nname(tg) != "Path" || items(&nf(tg)[0]).len() != 1 {
                    return Err("assignment target".into());
                }
                let n = st(&items(&nf(tg)[0])[0]);
                let Some((var, ty)) = env.get(&n).cloned() else { return Err("assignment target".into()) };
                let (v, _) = self.expr(&f[1], env, Some(&ty))?;
                let v = v.ok_or("assign of unit")?;
                self.b.def_var(var, v);
                Ok(())
            }
            _ => self.exprstmt(&f[0], env, tail),
        }
    }

    fn exprstmt(&mut self, e: &V, env: &mut Env, tail: bool) -> R<()> {
        let f = nf(e);
        match nname(e) {
            "Return" => {
                if self.entry {
                    let z = self.b.ins().iconst(types::I32, 0);
                    self.b.ins().return_(&[z]);
                } else if matches!(f[0], V::Nil) {
                    self.b.ins().return_(&[]);
                } else {
                    let rt = pty(&self.ret)?;
                    let (v, _) = self.expr(&f[0], env, Some(&rt))?;
                    match v {
                        Some(v) => self.b.ins().return_(&[v]),
                        None => self.b.ins().return_(&[]),
                    };
                }
                self.term = true;
                Ok(())
            }
            "Break" => {
                let Some(&(_, exit)) = self.loops.last() else { return Err("break outside loop".into()) };
                self.b.ins().jump(exit, &[]);
                self.term = true;
                Ok(())
            }
            "Continue" => {
                let Some(&(head, _)) = self.loops.last() else { return Err("continue outside loop".into()) };
                self.b.ins().jump(head, &[]);
                self.term = true;
                Ok(())
            }
            "If" => self.if_stmt(e, env, tail),
            "While" => {
                let (head, body, exit) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().jump(head, &[]);
                self.b.switch_to_block(head);
                let (c, _) = self.expr(&f[0], env, Some("bool"))?;
                self.b.ins().brif(c.ok_or("bad condition")?, body, &[], exit, &[]);
                self.b.switch_to_block(body);
                self.loops.push((head, exit));
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
                self.loops.push((body, exit));
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
                                self.print_call(e)?;
                            }
                            return Ok(());
                        }
                        return Err("entry tail expression".into());
                    }
                    if is_print(e) {
                        return self.print_call(e);
                    }
                }
                let expect = if tail && !self.entry { Some(pty(&self.ret)?) } else { None };
                let (v, ty) = self.expr(e, env, expect.as_deref())?;
                if tail && !self.entry && ty != "void" {
                    self.b.ins().return_(&[v.ok_or("tail value")?]);
                    self.term = true;
                }
                Ok(())
            }
        }
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

    fn print_call(&mut self, e: &V) -> R<()> {
        let a = &items(&nf(e)[1])[0];
        if nname(a) != "Lit" || st(&nf(a)[0]) != "str" {
            return Err("print_line of non-literal".into());
        }
        let hex = st(&nf(a)[1]);
        let mut bytes: Vec<u8> = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or(0)).collect();
        bytes.push(0);
        let p = self.string_ptr(bytes)?;
        let id = self.rt.puts;
        self.call_rt(id, &[p]);
        Ok(())
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
                k => Err(format!("literal {}", k)),
            },
            "Path" => {
                let segs = strs(&f[0]);
                if segs.len() == 1 {
                    if let Some((var, ty)) = env.get(&segs[0]).cloned() {
                        return Ok((Some(self.b.use_var(var)), ty));
                    }
                }
                Err(format!("path {}", segs.join(".")))
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
                let (a, _) = self.expr(&f[1], env, Some(&t))?;
                let (b, _) = self.expr(&f[2], env, Some(&t))?;
                let (a, b) = (a.ok_or("bad operand")?, b.ok_or("bad operand")?);
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
                let r = match op.as_str() {
                    "&" => self.b.ins().band(a, b),
                    "|" => self.b.ins().bor(a, b),
                    "^" => self.b.ins().bxor(a, b),
                    _ => return Err(format!("operator {}", op)),
                };
                Ok((Some(r), t))
            }
            "Unary" if st(&f[0]) == "!" => {
                let t = self.ty_of(&f[1], env, expect).ok_or("operand type")?;
                let (a, _) = self.expr(&f[1], env, Some(&t))?;
                let a = a.ok_or("bad operand")?;
                let r = if t == "bool" { self.b.ins().bxor_imm(a, 1) } else { self.b.ins().bnot(a) };
                Ok((Some(r), t))
            }
            "MCall" => self.method(&f[0], &st(&f[1]), items(&f[2]), env, expect),
            "Call" => {
                if nname(&f[0]) != "Path" {
                    return Err("call of non-path".into());
                }
                let segs = strs(&nf(&f[0])[0]);
                if segs.len() >= 2 && env.contains_key(&segs[0]) {
                    let recv = V::Node("Path", vec![V::L(segs[..segs.len() - 1].iter().map(|x| V::S(x.clone())).collect()), V::Nil], 0);
                    return self.method(&recv, &segs[segs.len() - 1], items(&f[1]), env, expect);
                }
                let args = items(&f[1]);
                if segs == ["core", "check", "eq"] {
                    let t = self.ty_of(&args[0], env, None).or_else(|| self.ty_of(&args[1], env, None)).ok_or("operand type")?;
                    let (a, _) = self.expr(&args[0], env, Some(&t))?;
                    let (b, _) = self.expr(&args[1], env, Some(&t))?;
                    let c = self.b.ins().icmp(IntCC::Equal, a.ok_or("bad operand")?, b.ok_or("bad operand")?);
                    let (ok, bad) = (self.b.create_block(), self.b.create_block());
                    self.b.ins().brif(c, ok, &[], bad, &[]);
                    self.b.switch_to_block(bad);
                    self.fail_block()?;
                    self.b.switch_to_block(ok);
                    return Ok((None, "void".into()));
                }
                if segs == ["core", "mem", "drop"] {
                    self.expr(&args[0], env, None)?;
                    return Ok((None, "void".into()));
                }
                let fq = segs.join(".");
                let Some(d) = self.funcs.get(&fq).copied() else { return Err(format!("call to {}", fq)) };
                let id = *self.ids.get(&fq).ok_or("call target not declared")?;
                let mut vals = vec![];
                for ((_, pt), a) in d.params.iter().zip(args) {
                    let want = pty(pt)?;
                    let (v, _) = self.expr(a, env, Some(&want))?;
                    vals.push(v.ok_or("bad argument")?);
                }
                let res = self.call_rt(id, &vals);
                let rt = pty(&d.ret)?;
                Ok((res.first().copied(), rt))
            }
            n => Err(format!("expression {}", n)),
        }
    }

    fn method(&mut self, recv: &V, name: &str, args: &[V], env: &mut Env, expect: Option<&str>) -> R<(Option<Value>, String)> {
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

pub fn compile_clif(funcs: &HashMap<String, &Def>, entry: &str, out_path: &str, release: bool) -> R<()> {
    if !funcs.contains_key(entry) {
        return Err("entry not found".into());
    }
    let mut flags = settings::builder();
    flags.set("opt_level", if release { "speed" } else { "none" }).map_err(|e| e.to_string())?;
    flags.set("is_pic", "false").map_err(|e| e.to_string())?;
    let isa = cranelift_native::builder().map_err(|e| e.to_string())?.finish(settings::Flags::new(flags)).map_err(|e| e.to_string())?;
    let ob = ObjectBuilder::new(isa, "kasane", default_libcall_names()).map_err(|e| e.to_string())?;
    let mut module = ObjectModule::new(ob);
    let ptr = module.target_config().pointer_type();

    // 呼び出される関数を集める。
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
        crate::cnative::calls_in(d.body.as_ref().unwrap(), &mut todo);
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
    let rt = Rt { puts, write, exit };

    // 関数の宣言。入口は C の `main` にする。
    let mut ids: HashMap<String, FuncId> = HashMap::new();
    for n in &order {
        let d = funcs[n];
        let mut sig = module.make_signature();
        for (_, t) in &d.params {
            sig.params.push(AbiParam::new(cl(&pty(t)?)));
        }
        if n == entry {
            sig.returns.push(AbiParam::new(types::I32));
        } else {
            let r = pty(&d.ret)?;
            if r != "void" {
                sig.returns.push(AbiParam::new(cl(&r)));
            }
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
            let mut g = FnGen {
                b,
                module: &mut module,
                funcs,
                ids: &ids,
                rt: &rt,
                data_n: &mut data_n,
                entry: n == entry,
                ret: d.ret.clone(),
                loops: vec![],
                term: false,
                nvar: 0,
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

fn which(cmd: &str) -> bool {
    std::process::Command::new(cmd).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}
