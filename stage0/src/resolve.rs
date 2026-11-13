//! 名前解決 (stage0)。Python 参照実装 ksn/interp.py の add_def / expand / resolve_global と同じ規則。

use std::collections::{HashMap, HashSet};

use crate::defs;
use crate::lexer;
use crate::parser::{parse_body, V};

const PRIMS: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64",
    "bool", "char", "str",
];
const INTS: &[&str] = &["i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize"];

const NUM_EXTRA: &[&str] = &[
    "abs", "max", "min", "count_ones", "leading_zeros", "trailing_zeros", "successor", "to_f64", "from_str_radix", "parse",
    "to_bits", "from_bits", "rem", "sqrt", "floor", "ceil", "sin", "cos", "tan", "exp", "ln", "powi", "trunc",
    "round_ties_even", "is_nan", "is_finite", "atan2", "nearest_to_f32", "then_some", "is_ascii_alphabetic",
    "is_ascii_digit", "is_ascii_whitespace", "len_utf8", "to_ascii_lowercase", "to_ascii_uppercase", "to_u32",
    "try_from_u32",
];

fn known_num(name: &str) -> bool {
    ["wrapping_", "checked_", "saturating_", "from_", "try_from_", "trunc_to_"]
        .iter()
        .any(|p| name.starts_with(p))
        || NUM_EXTRA.contains(&name)
}

fn num_const(ty: &str, name: &str) -> bool {
    if INTS.contains(&ty) {
        return matches!(name, "MAX" | "MIN" | "BITS");
    }
    if ty == "f32" || ty == "f64" {
        return matches!(name, "INFINITY" | "NEG_INFINITY" | "NAN" | "EPSILON" | "MAX" | "MIN_POSITIVE");
    }
    false
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub kind: &'static str,
    pub name: String,
    pub rest: Vec<String>,
}

#[derive(Default)]
pub struct Program {
    pub funcs: HashSet<String>,
    pub sigs: HashSet<String>,
    pub consts: HashSet<String>,
    pub types: HashSet<String>,
    pub variants: HashSet<String>,
    pub natives: HashSet<String>,
    pub imports: HashMap<String, HashMap<String, String>>,
}

impl Program {
    /// ファイルの定義を登録する。`sig` は .sig (fn に本体なし)。
    pub fn add_file(&mut self, path: &str, src: &str) -> Result<(), String> {
        let sig = path.ends_with(".sig");
        let lines: Vec<&str> = src.split('\n').collect();
        for d in defs::scan_with(src, sig)? {
            match d.kw.as_str() {
                "fn" => {
                    if sig {
                        self.sigs.insert(d.name);
                    } else {
                        self.funcs.insert(d.name);
                    }
                }
                "const" | "static" => {
                    self.consts.insert(d.name);
                }
                "type" | "trait" => {
                    self.types.insert(d.name);
                }
                "enum" => {
                    for l in &lines[d.line..d.end_line - 1] {
                        let t = l.trim();
                        if t.is_empty() {
                            continue;
                        }
                        let vn: String = t.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                        self.variants.insert(format!("{}.{}", d.name, vn));
                    }
                    self.types.insert(d.name);
                }
                "import" => {
                    let tail = d.name.rsplit('.').next().unwrap().to_string();
                    self.imports.entry(path.to_string()).or_default().insert(tail, d.name);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn expand(&self, segs: &[String], file: &str) -> Vec<String> {
        if let Some(imps) = self.imports.get(file) {
            if let Some(full) = imps.get(&segs[0]) {
                let mut v: Vec<String> = full.split('.').map(String::from).collect();
                v.extend_from_slice(&segs[1..]);
                return v;
            }
        }
        segs.to_vec()
    }

    pub fn resolve(&self, segs: &[String], file: &str) -> Option<Resolved> {
        if segs.len() == 1 {
            let pre = match segs[0].as_str() {
                "Some" => Some("Option.Some"),
                "None" => Some("Option.None"),
                "Ok" => Some("Result.Ok"),
                "Err" => Some("Result.Err"),
                _ => None,
            };
            if let Some(n) = pre {
                return Some(Resolved { kind: "variant", name: n.to_string(), rest: vec![] });
            }
        }
        let segs: Vec<String> = if PRIMS.contains(&segs[0].as_str()) {
            let m = if INTS.contains(&segs[0].as_str()) || segs[0] == "f32" || segs[0] == "f64" {
                "num".to_string()
            } else {
                segs[0].clone()
            };
            let mut v = vec!["core".to_string(), m];
            v.extend_from_slice(segs);
            v
        } else {
            self.expand(segs, file)
        };
        for k in (1..=segs.len()).rev() {
            let fq = segs[..k].join(".");
            let rest = segs[k..].to_vec();
            let kind = if self.funcs.contains(&fq) {
                "fn"
            } else if self.consts.contains(&fq) {
                "const"
            } else if self.variants.contains(&fq) {
                "variant"
            } else if self.natives.contains(&fq) {
                "native"
            } else if self.types.contains(&fq) {
                "type"
            } else if self.sigs.contains(&fq) {
                "sig"
            } else {
                continue;
            };
            return Some(Resolved { kind, name: fq, rest });
        }
        if segs.len() >= 4
            && segs[0] == "core"
            && matches!(segs[1].as_str(), "num" | "bool" | "char")
            && PRIMS.contains(&segs[2].as_str())
        {
            let nm = format!("{}.{}", segs[2], segs[3]);
            if known_num(&segs[3]) {
                return Some(Resolved { kind: "native", name: nm, rest: segs[4..].to_vec() });
            }
            if num_const(&segs[2], &segs[3]) {
                return Some(Resolved { kind: "value", name: nm, rest: segs[4..].to_vec() });
            }
        }
        None
    }
}

/// 木を前順で辿り、名前を持つノード(Path / StructLit / パターンの経路)の segs を集める。
pub fn collect_paths(v: &V, out: &mut Vec<Vec<String>>) {
    match v {
        V::Node(name, fs, _, _) => {
            match *name {
                "Path" | "PPath" | "PVariant" | "PStruct" => {
                    if let Some(V::L(xs)) = fs.first() {
                        out.push(xs.iter().map(|x| x.dump()).collect());
                    }
                }
                "StructLit" => {
                    if let Some(V::S(n)) = fs.first() {
                        out.push(n.split('.').map(String::from).collect());
                    }
                }
                _ => {}
            }
            for f in fs {
                collect_paths(f, out);
            }
        }
        V::L(xs) => {
            for x in xs {
                collect_paths(x, out);
            }
        }
        _ => {}
    }
}

/// `resolve` コマンド本体: natives ファイル、ライブラリ数 n、ファイル群 (先頭 n 個はライブラリ、残りが対象)。
pub fn run(args: &[String]) -> Result<Vec<String>, String> {
    let natives = std::fs::read_to_string(&args[0]).map_err(|e| e.to_string())?;
    let nlib: usize = args[1].parse().map_err(|_| "bad lib count")?;
    let files = &args[2..];
    let mut prog = Program::default();
    for n in natives.lines() {
        prog.natives.insert(n.to_string());
    }
    let mut srcs = Vec::new();
    for f in files {
        let src = std::fs::read_to_string(f).map_err(|e| format!("{}: {}", f, e))?;
        prog.add_file(f, &src)?;
        srcs.push(src);
    }
    let mut out = Vec::new();
    for (idx, f) in files.iter().enumerate() {
        if idx < nlib || f.ends_with(".sig") {
            continue;
        }
        let lines: Vec<&str> = srcs[idx].split('\n').collect();
        for d in defs::scan(&srcs[idx])? {
            if d.kw != "fn" && d.kw != "test" {
                continue;
            }
            let body = lines[d.line..d.end_line - 1].join("\n") + "\n";
            let ast = parse_body(lexer::lex(&body)?, d.line)?;
            let mut paths = Vec::new();
            collect_paths(&ast, &mut paths);
            for p in paths {
                let r = match prog.resolve(&p, f) {
                    Some(r) => format!("{}:{}:{}", r.kind, r.name, r.rest.join(".")),
                    None => "none".to_string(),
                };
                out.push(format!("{}\t{}\t{}", d.name, p.join("."), r));
            }
        }
    }
    Ok(out)
}
