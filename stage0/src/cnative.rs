//! ネイティブバックエンド (C 経由, stage0)。Python 参照実装 ksn/native.py の移植。
//! 整数・bool・制御フロー・関数呼び出しの部分集合を C にして gcc で機械語にする。

use std::collections::{HashMap, HashSet};

use crate::check::{items, nf, nname, st, strs, tag, tel};
use crate::parser::V;
use crate::program::Def;

fn ct(p: &str) -> Option<&'static str> {
    Some(match p {
        "i8" => "int8_t",
        "i16" => "int16_t",
        "i32" => "int32_t",
        "i64" | "isize" => "int64_t",
        "u8" => "uint8_t",
        "u16" => "uint16_t",
        "u32" => "uint32_t",
        "u64" | "usize" => "uint64_t",
        "bool" => "bool",
        _ => return None,
    })
}
fn is_int(p: &str) -> bool {
    ct(p).is_some() && p != "bool"
}

type R<T> = Result<T, String>;

fn mangle(fqn: &str) -> String {
    format!("k_{}", fqn.replace('.', "_"))
}

struct Gen<'a> {
    funcs: &'a HashMap<String, &'a Def>,
    out: Vec<String>,
    sigs: Vec<String>,
    entry: bool,
    ret: V,
}

type Env = HashMap<String, String>;

impl<'a> Gen<'a> {
    fn pty(&self, t: &V) -> R<String> {
        if tag(t) == "tuple" && items(tel(t, 1)).is_empty() {
            return Ok("void".into());
        }
        if tag(t) == "prim" {
            let p = st(tel(t, 1));
            if ct(&p).is_some() {
                return Ok(p);
            }
        }
        Err(format!("type {}", t.dump()))
    }
    fn cty(&self, t: &V) -> R<String> {
        let p = self.pty(t)?;
        Ok(if p == "void" { "void".into() } else { ct(&p).unwrap().into() })
    }

    fn func(&mut self, d: &Def, entry: bool) -> R<()> {
        if !d.generics.is_empty() {
            return Err(format!("generic function {}", d.name));
        }
        self.entry = entry;
        self.ret = if entry { V::L(vec![V::S("tuple".into()), V::L(vec![])]) } else { d.ret.clone() };
        let mut ps = vec![];
        for (n, t) in &d.params {
            ps.push(format!("{} {}", self.cty(t)?, n));
        }
        let params = if ps.is_empty() { "void".to_string() } else { ps.join(", ") };
        let rt = if entry { "int".to_string() } else { self.cty(&d.ret)? };
        self.sigs.push(format!("{} {}({});", rt, mangle(&d.name), params));
        self.out.push(format!("{} {}({}) {{", rt, mangle(&d.name), params));
        let mut env: Env = HashMap::new();
        for (n, t) in &d.params {
            env.insert(n.clone(), self.pty(t)?);
        }
        let stmts = items(&nf(d.body.as_ref().unwrap())[0]).to_vec();
        for (i, s) in stmts.iter().enumerate() {
            self.stmt(s, &mut env, i == stmts.len() - 1, 1)?;
        }
        if entry {
            self.out.push("    return 0;".into());
        }
        self.out.push("}".into());
        Ok(())
    }

    fn stmt(&mut self, s: &V, env: &mut Env, tail: bool, depth: usize) -> R<()> {
        let ind = "    ".repeat(depth);
        let f = nf(s);
        match nname(s) {
            "Let" => {
                if nname(&f[0]) != "PBind" {
                    return Err("destructuring let".into());
                }
                let ty = if matches!(f[2], V::Nil) { None } else { Some(self.pty(&f[2])?) };
                let (code, ty) = self.expr(&f[3], env, ty.as_deref())?;
                if ty == "void" {
                    return Err("let of unit".into());
                }
                let name = st(&nf(&f[0])[0]);
                env.insert(name.clone(), ty.clone());
                self.out.push(format!("{}{} {} = {};", ind, ct(&ty).unwrap(), name, code));
                Ok(())
            }
            "Assign" => {
                let tg = &f[0];
                if nname(tg) != "Path" || items(&nf(tg)[0]).len() != 1 || !env.contains_key(&st(&items(&nf(tg)[0])[0])) {
                    return Err("assignment target".into());
                }
                let n = st(&items(&nf(tg)[0])[0]);
                let ety = env[&n].clone();
                let (code, _) = self.expr(&f[1], env, Some(&ety))?;
                self.out.push(format!("{}{} = {};", ind, n, code));
                Ok(())
            }
            _ => self.exprstmt(&f[0], env, tail, depth),
        }
    }

    fn exprstmt(&mut self, e: &V, env: &mut Env, tail: bool, depth: usize) -> R<()> {
        let ind = "    ".repeat(depth);
        let f = nf(e);
        match nname(e) {
            "Return" => {
                if self.entry {
                    self.out.push(format!("{}return 0;", ind));
                } else if matches!(f[0], V::Nil) {
                    self.out.push(format!("{}return;", ind));
                } else {
                    let rt = self.pty(&self.ret.clone())?;
                    let (code, _) = self.expr(&f[0], env, Some(&rt))?;
                    self.out.push(format!("{}return {};", ind, code));
                }
                Ok(())
            }
            "Break" => {
                self.out.push(format!("{}break;", ind));
                Ok(())
            }
            "Continue" => {
                self.out.push(format!("{}continue;", ind));
                Ok(())
            }
            "If" => self.if_stmt(e, env, depth, tail),
            "While" => {
                let (c, _) = self.expr(&f[0], env, Some("bool"))?;
                self.out.push(format!("{}while ({}) {{", ind, c));
                let mut inner = env.clone();
                self.block(&f[1], &mut inner, depth + 1, false)?;
                self.out.push(format!("{}}}", ind));
                Ok(())
            }
            "Loop" => {
                self.out.push(format!("{}for (;;) {{", ind));
                let mut inner = env.clone();
                self.block(&f[0], &mut inner, depth + 1, false)?;
                self.out.push(format!("{}}}", ind));
                Ok(())
            }
            "Block" => {
                self.out.push(format!("{}{{", ind));
                let mut inner = env.clone();
                self.block(e, &mut inner, depth + 1, tail)?;
                self.out.push(format!("{}}}", ind));
                Ok(())
            }
            _ => {
                if self.entry {
                    if tail {
                        if self.is_ok_unit(e) || self.is_print(e) {
                            if self.is_print(e) {
                                self.print_call(e, &ind)?;
                            }
                            return Ok(());
                        }
                        return Err("entry tail expression".into());
                    }
                    if self.is_print(e) {
                        return self.print_call(e, &ind);
                    }
                }
                let expect = if tail && !self.entry { Some(self.pty(&self.ret.clone())?) } else { None };
                let (code, ty) = self.expr(e, env, expect.as_deref())?;
                if tail && !self.entry && ty != "void" {
                    self.out.push(format!("{}return {};", ind, code));
                } else {
                    self.out.push(format!("{}{};", ind, code));
                }
                Ok(())
            }
        }
    }

    fn block(&mut self, b: &V, env: &mut Env, depth: usize, tail: bool) -> R<()> {
        let stmts = items(&nf(b)[0]).to_vec();
        for (i, s) in stmts.iter().enumerate() {
            self.stmt(s, env, tail && i == stmts.len() - 1, depth)?;
        }
        Ok(())
    }

    fn if_stmt(&mut self, e: &V, env: &mut Env, depth: usize, tail: bool) -> R<()> {
        let ind = "    ".repeat(depth);
        let f = nf(e);
        let (c, _) = self.expr(&f[0], env, Some("bool"))?;
        self.out.push(format!("{}if ({}) {{", ind, c));
        let mut inner = env.clone();
        self.block(&f[1], &mut inner, depth + 1, tail)?;
        if matches!(f[2], V::Nil) {
            self.out.push(format!("{}}}", ind));
            return Ok(());
        }
        self.out.push(format!("{}}} else {{", ind));
        if nname(&f[2]) == "If" {
            self.if_stmt(&f[2], env, depth + 1, tail)?;
        } else {
            let mut inner = env.clone();
            self.block(&f[2], &mut inner, depth + 1, tail)?;
        }
        self.out.push(format!("{}}}", ind));
        Ok(())
    }

    fn is_ok_unit(&self, e: &V) -> bool {
        nname(e) == "Call"
            && nname(&nf(e)[0]) == "Path"
            && strs(&nf(&nf(e)[0])[0]) == ["Ok"]
            && items(&nf(e)[1]).len() == 1
            && nname(&items(&nf(e)[1])[0]) == "Lit"
            && st(&nf(&items(&nf(e)[1])[0])[0]) == "unit"
    }
    fn is_print(&self, e: &V) -> bool {
        nname(e) == "Call" && nname(&nf(e)[0]) == "Path" && strs(&nf(&nf(e)[0])[0]) == ["std", "io", "print_line"]
    }
    fn print_call(&mut self, e: &V, ind: &str) -> R<()> {
        let a = &items(&nf(e)[1])[0];
        if nname(a) != "Lit" || st(&nf(a)[0]) != "str" {
            return Err("print_line of non-literal".into());
        }
        let hex = st(&nf(a)[1]);
        let bytes: Vec<u8> = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or(0)).collect();
        let s = String::from_utf8_lossy(&bytes).to_string();
        self.out.push(format!("{}fputs({}, stdout); fputc(10, stdout);", ind, cstr(&s)));
        Ok(())
    }

    fn typed(&mut self, e: &V, env: &Env) -> bool {
        let mut tmp = env.clone();
        self.expr(e, &mut tmp, None).is_ok()
    }

    fn expr(&mut self, e: &V, env: &mut Env, expect: Option<&str>) -> R<(String, String)> {
        let f = nf(e);
        match nname(e) {
            "Lit" => match st(&f[0]).as_str() {
                "int" => {
                    let sfx = if matches!(f[2], V::Nil) { None } else { Some(st(&f[2])) };
                    let ty = sfx.or(expect.map(String::from));
                    match ty {
                        Some(t) if is_int(&t) => {
                            let v = st(&f[1]);
                            let c = ct(&t).unwrap();
                            if v.starts_with('-') {
                                Ok((format!("(({})({}LL))", c, v), t))
                            } else {
                                Ok((format!("(({}){}LL)", c, v), t))
                            }
                        }
                        _ => Err("untyped integer literal".into()),
                    }
                }
                "bool" => Ok((if matches!(f[1], V::B(true)) { "true" } else { "false" }.into(), "bool".into())),
                k => Err(format!("literal {}", k)),
            },
            "Path" => {
                let segs = strs(&f[0]);
                if segs.len() == 1 && env.contains_key(&segs[0]) {
                    return Ok((segs[0].clone(), env[&segs[0]].clone()));
                }
                Err(format!("path {}", segs.join(".")))
            }
            "Bin" => {
                let op = st(&f[0]);
                if op == "&&" || op == "||" {
                    let (a, _) = self.expr(&f[1], env, Some("bool"))?;
                    let (b, _) = self.expr(&f[2], env, Some("bool"))?;
                    return Ok((format!("({} {} {})", a, op, b), "bool".into()));
                }
                let cmp = matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=");
                let (a, ta) = self.expr(&f[1], env, if cmp { None } else { expect })?;
                let (b, tb) = self.expr(&f[2], env, Some(&ta))?;
                let (a, ta) = if ta != tb { self.expr(&f[1], env, Some(&tb))? } else { (a, ta) };
                if cmp {
                    return Ok((format!("({} {} {})", a, op, b), "bool".into()));
                }
                if matches!(op.as_str(), "&" | "|" | "^") {
                    return Ok((format!("(({})({} {} {}))", ct(&ta).unwrap(), a, op, b), ta));
                }
                Err(format!("operator {}", op))
            }
            "Unary" if st(&f[0]) == "!" => {
                let (a, ta) = self.expr(&f[1], env, expect)?;
                Ok(if ta == "bool" { (format!("(!{})", a), ta) } else { (format!("(({})(~{}))", ct(&ta).unwrap(), a), ta) })
            }
            "If" => Err("if as expression".into()),
            "MCall" => self.method(&f[0], &st(&f[1]), items(&f[2]), env, expect),
            "Call" => {
                let fnn = &f[0];
                if nname(fnn) != "Path" {
                    return Err("call of non-path".into());
                }
                let segs = strs(&nf(fnn)[0]);
                if segs.len() >= 2 && env.contains_key(&segs[0]) {
                    let recv = V::Node("Path", vec![V::L(segs[..segs.len() - 1].iter().map(|x| V::S(x.clone())).collect()), V::Nil], 0);
                    return self.method(&recv, &segs[segs.len() - 1], items(&f[1]), env, expect);
                }
                let args = items(&f[1]);
                if segs == ["core", "check", "eq"] {
                    let (a0, ta) = if self.typed(&args[0], env) { self.expr(&args[0], env, None)? } else { self.expr(&args[1], env, None)? };
                    let (b, _) = self.expr(&args[1], env, Some(&ta))?;
                    let (a, _) = self.expr(&args[0], env, Some(&ta))?;
                    let _ = a0;
                    return Ok((format!("(({}) == ({}) ? (void)0 : ksn_fail(__LINE__))", a, b), "void".into()));
                }
                if segs == ["core", "mem", "drop"] {
                    let (a, _) = self.expr(&args[0], env, None)?;
                    return Ok((format!("((void)({}))", a), "void".into()));
                }
                let fq = segs.join(".");
                let Some(d) = self.funcs.get(&fq).copied() else { return Err(format!("call to {}", fq)) };
                let mut cargs = vec![];
                for ((_, pt), a) in d.params.iter().zip(args) {
                    let want = self.pty(pt)?;
                    let (c, _) = self.expr(a, env, Some(&want))?;
                    cargs.push(c);
                }
                Ok((format!("{}({})", mangle(&fq), cargs.join(", ")), self.pty(&d.ret)?))
            }
            n => Err(format!("expression {}", n)),
        }
    }

    fn method(&mut self, recv: &V, name: &str, args: &[V], env: &mut Env, expect: Option<&str>) -> R<(String, String)> {
        if !name.starts_with("wrapping_") {
            return Err(format!("method {}", name));
        }
        let (a, ta) = self.expr(recv, env, expect)?;
        let op = &name["wrapping_".len()..];
        let c = ct(&ta).ok_or("bad type")?;
        if op == "neg" {
            return Ok((format!("(({})(0 - (uint64_t){}))", c, a), ta));
        }
        let (b, _) = self.expr(&args[0], env, Some(&ta))?;
        let sym = match op {
            "add" => "+",
            "sub" => "-",
            "mul" => "*",
            _ => return Err(format!("method {}", name)),
        };
        Ok((format!("(({})((uint64_t){} {} (uint64_t){}))", c, a, sym, b), ta))
    }
}

fn cstr(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.bytes() {
        match ch {
            10 => out.push_str("\\n"),
            9 => out.push_str("\\t"),
            34 => out.push_str("\\\""),
            92 => out.push_str("\\\\"),
            32..=126 => out.push(ch as char),
            _ => out.push_str(&format!("\\{:03o}", ch)),
        }
    }
    out.push('"');
    out
}

pub(crate) fn calls_in(v: &V, out: &mut Vec<String>) {
    if nname(v) == "Call" && nname(&nf(v)[0]) == "Path" {
        out.push(strs(&nf(&nf(v)[0])[0]).join("."));
    }
    for c in crate::check::children(v) {
        calls_in(c, out);
    }
}

pub fn generate_c(funcs: &HashMap<String, &Def>, entry: &str) -> R<String> {
    let mut g = Gen { funcs, out: vec![], sigs: vec![], entry: false, ret: V::Nil };
    let mut todo = vec![entry.to_string()];
    let mut seen: HashSet<String> = HashSet::new();
    let mut order = vec![];
    while let Some(n) = todo.pop() {
        if !seen.insert(n.clone()) {
            continue;
        }
        let Some(d) = funcs.get(&n) else { continue };
        order.push(n.clone());
        calls_in(d.body.as_ref().unwrap(), &mut todo);
    }
    for n in &order {
        g.func(funcs[n], *n == entry)?;
    }
    let head = [
        "#include <stdint.h>",
        "#include <stdbool.h>",
        "#include <stdio.h>",
        "#include <stdlib.h>",
        "static int ksn_fail(int line) { fputs(\"panic: assertion failed\\n\", stderr); exit(101); }",
        "",
    ];
    let mut all: Vec<String> = head.iter().map(|x| x.to_string()).collect();
    all.extend(g.sigs.clone());
    all.push(String::new());
    all.extend(g.out.clone());
    all.push(String::new());
    all.push(format!("int main(void) {{ return {}(); }}", mangle(entry)));
    Ok(all.join("\n") + "\n")
}

pub fn compile_native(funcs: &HashMap<String, &Def>, entry: &str, out_path: &str, release: bool) -> R<()> {
    if !funcs.contains_key(entry) {
        return Err("entry not found".into());
    }
    let csrc = generate_c(funcs, entry)?;
    let cc = ["gcc", "cc", "clang"].iter().find(|c| which(c)).ok_or("no C compiler found")?;
    let cpath = format!("{}.c", out_path);
    std::fs::write(&cpath, csrc).map_err(|e| e.to_string())?;
    let r = std::process::Command::new(cc)
        .args([if release { "-O2" } else { "-O0" }, "-fwrapv", "-o", out_path, &cpath])
        .output()
        .map_err(|e| e.to_string())?;
    if !r.status.success() {
        let err: String = String::from_utf8_lossy(&r.stderr).trim().chars().take(300).collect();
        return Err(format!("C compiler failed: {}", err));
    }
    Ok(())
}

fn which(cmd: &str) -> bool {
    std::process::Command::new(cmd).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}
