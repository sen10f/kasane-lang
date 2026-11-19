//! C ABI への変換 (`extern` と C のコールバックで構造体を値渡し・値返しするとき)。
//!
//! KIR の集成体の値は LLVM では `iN` (N = 大きさ × 8) の整数で、配置は C の `layout c` と同じバイト列。
//! C の呼び出し規約は構造体を次のように渡すので、境界で `iN` と C の形を相互に変換する。
//! - x86_64 System V: 16 バイト以下は 8 バイトごとに INTEGER / SSE に分類して別々の引数にし
//!   (レジスタが足りなければ全体をメモリ)、16 バイト超はメモリ (引数は `byval`、戻り値は `sret`)。
//! - AArch64 (AAPCS64): 同じ浮動小数 1〜4 個の HFA は `[N x float/double]`、16 バイト以下は
//!   `i64` か `[2 x i64]`、16 バイト超は写しへのポインタ (戻り値は `sret`、x8)。
//!
//! 出力は clang が同じ C の構造体に対して作る IR と同じ形 (呼び出し規約を決める部分だけ)。

use crate::kir::{AggDef, AggKind, Ty};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    X86_64,
    Aarch64,
}

/// 葉の型の分類 (浮動小数か整数相当か) と大きさ。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Leaf {
    Int,
    Float,
}

/// 構造体を葉 (オフセット, 種類, 大きさ) に平らにする。扱えない形 (列挙型など) は `None`。
pub fn leaves(aggs: &[AggDef], ai: u32, base: u32, out: &mut Vec<(u32, Leaf, u32)>) -> Option<()> {
    let def = &aggs[ai as usize];
    match &def.kind {
        AggKind::Struct(fs) | AggKind::Tuple(fs) => {
            for f in fs {
                leaf_ty(aggs, f.ty, base + f.off, out)?;
            }
            Some(())
        }
        _ => None,
    }
}

fn leaf_ty(aggs: &[AggDef], t: Ty, at: u32, out: &mut Vec<(u32, Leaf, u32)>) -> Option<()> {
    match t {
        Ty::F32 => out.push((at, Leaf::Float, 4)),
        Ty::F64 => out.push((at, Leaf::Float, 8)),
        Ty::Agg(i) => return leaves(aggs, i, at, out),
        Ty::Unit | Ty::Opt(_) | Ty::Res(..) => return None,
        t => out.push((at, Leaf::Int, crate::kir::size_of(aggs, t))),
    }
    Some(())
}

/// 引数・戻り値の 1 つの部品 (レジスタ 1 つ分)。`ty` は LLVM の型、`vbits` は値の `iN` から切り出す幅、
/// `off` はその位置 (バイト)、`float` なら切り出した整数を `ty` にビット変換する。
#[derive(Clone, Debug)]
pub struct Slot {
    pub ty: String,
    pub vbits: u32,
    pub off: u32,
    pub float: bool,
}

#[derive(Clone, Debug)]
pub enum Abi {
    /// 部品ごとに別の引数 (戻り値は 2 部品なら `{a, b}`)。
    Regs(Vec<Slot>),
    /// 部品を 1 つの配列型の引数にまとめる (AArch64)。`elem` は要素の LLVM 型。
    Array(Vec<Slot>, String),
    /// メモリ渡し: 引数は `byval` (x86_64)。戻り値は `sret`。
    Mem { size: u32, align: u32 },
    /// 写しへのポインタ (AArch64 の 16 バイト超の引数)。
    Ptr { size: u32 },
}

/// 引数の分類。`int_free` / `sse_free` は残りのレジスタ数 (x86_64)。
fn classify(arch: Arch, aggs: &[AggDef], ai: u32, int_free: &mut u32, sse_free: &mut u32, ret: bool) -> Option<Abi> {
    let def = &aggs[ai as usize];
    let size = def.size.max(1);
    let align = def.align.max(1);
    let mut lv = vec![];
    leaves(aggs, ai, 0, &mut lv)?;
    match arch {
        Arch::X86_64 => {
            if size > 16 {
                return Some(Abi::Mem { size, align: align.max(8) });
            }
            let nb = (size + 7) / 8;
            let mut cls = vec![None::<Leaf>; nb as usize];
            for (off, k, sz) in &lv {
                let first = off / 8;
                let last = (off + sz.max(&1) - 1) / 8;
                for e in first..=last {
                    let c = &mut cls[e as usize];
                    *c = match (*c, k) {
                        (Some(Leaf::Int), _) | (_, Leaf::Int) => Some(Leaf::Int),
                        _ => Some(Leaf::Float),
                    };
                }
            }
            let mut slots = vec![];
            for e in 0..nb {
                let n = (size - 8 * e).min(8);
                match cls[e as usize] {
                    Some(Leaf::Float) => {
                        let fs: Vec<&(u32, Leaf, u32)> = lv.iter().filter(|(o, _, _)| o / 8 == e).collect();
                        let (ty, vb) = if fs.len() == 1 && fs[0].2 == 8 {
                            ("double".to_string(), 64)
                        } else if fs.len() == 1 {
                            ("float".to_string(), 32)
                        } else {
                            ("<2 x float>".to_string(), 64)
                        };
                        slots.push(Slot { ty, vbits: vb, off: 8 * e, float: true });
                    }
                    // 空 (詰め物だけ) の 8 バイトも INTEGER として渡す。
                    _ => slots.push(Slot { ty: format!("i{}", n * 8), vbits: n * 8, off: 8 * e, float: false }),
                }
            }
            let ni = slots.iter().filter(|s| !s.float).count() as u32;
            let ns = slots.iter().filter(|s| s.float).count() as u32;
            // 戻り値のレジスタは別の組 (rax/rdx、xmm0/xmm1) なので数えない。
            if !ret {
                if ni > *int_free || ns > *sse_free {
                    return Some(Abi::Mem { size, align: align.max(8) });
                }
                *int_free -= ni;
                *sse_free -= ns;
            }
            Some(Abi::Regs(slots))
        }
        Arch::Aarch64 => {
            // HFA: 同じ大きさの浮動小数だけ 1〜4 個。
            if !lv.is_empty() && lv.len() <= 4 && lv.iter().all(|(_, k, s)| *k == Leaf::Float && *s == lv[0].2) && size == lv.len() as u32 * lv[0].2 {
                let (ty, vb) = if lv[0].2 == 8 { ("double", 64) } else { ("float", 32) };
                let slots: Vec<Slot> = (0..lv.len() as u32).map(|j| Slot { ty: ty.to_string(), vbits: vb, off: j * lv[0].2, float: true }).collect();
                return Some(Abi::Array(slots, ty.to_string()));
            }
            if size > 16 {
                return Some(if ret { Abi::Mem { size, align: align.max(8) } } else { Abi::Ptr { size } });
            }
            if size <= 8 {
                return Some(Abi::Regs(vec![Slot { ty: "i64".into(), vbits: size * 8, off: 0, float: false }]));
            }
            let slots = vec![
                Slot { ty: "i64".into(), vbits: 64, off: 0, float: false },
                Slot { ty: "i64".into(), vbits: (size - 8) * 8, off: 8, float: false },
            ];
            Some(Abi::Array(slots, "i64".into()))
        }
    }
}

/// 関数の C ABI の形。`args[i]` が `None` のものはスカラー (そのまま渡す)。
#[derive(Clone, Debug)]
pub struct Sig {
    pub args: Vec<Option<Abi>>,
    /// 戻り値が集成体なら、その分類。
    pub ret: Option<Abi>,
}

/// 引数と戻り値の型から C ABI の形を決める。扱えない型 (列挙型など) は `Err`。
pub fn sig(arch: Arch, aggs: &[AggDef], params: &[Ty], ret: Ty) -> Result<Sig, String> {
    let mut int_free = 6u32;
    let mut sse_free = 8u32;
    let mut r = None;
    if let Ty::Agg(ai) = ret {
        let (mut a, mut b) = (6, 8);
        let c = classify(arch, aggs, ai, &mut a, &mut b, true).ok_or_else(|| format!("構造体 {} は C の戻り値にできない", aggs[ai as usize].name))?;
        if matches!(c, Abi::Mem { .. }) && arch == Arch::X86_64 {
            int_free -= 1; // sret の隠し引数が rdi を使う。
        }
        r = Some(c);
    }
    let mut args = vec![];
    for t in params {
        match t {
            Ty::Agg(ai) => {
                let c = classify(arch, aggs, *ai, &mut int_free, &mut sse_free, false).ok_or_else(|| format!("構造体 {} は C の引数にできない", aggs[*ai as usize].name))?;
                args.push(Some(c));
            }
            Ty::F32 | Ty::F64 => {
                sse_free = sse_free.saturating_sub(1);
                args.push(None);
            }
            _ => {
                int_free = int_free.saturating_sub(1);
                args.push(None);
            }
        }
    }
    Ok(Sig { args, ret: r })
}

/// LLVM の IR の行を作る小さな出力先 (名前は `%<prefix><n>`)。
pub struct Em {
    pub body: String,
    n: u32,
    pfx: String,
    /// スタックに領域を取った (呼び出しの前後で `stacksave` / `stackrestore` が要る)。
    pub used_stack: bool,
}

impl Em {
    pub fn new(pfx: &str) -> Em {
        Em { body: String::new(), n: 0, pfx: pfx.to_string(), used_stack: false }
    }
    pub fn line(&mut self, s: String) {
        self.body.push_str("  ");
        self.body.push_str(&s);
        self.body.push('\n');
    }
    pub fn t(&mut self, rhs: String) -> String {
        self.n += 1;
        let n = format!("%{}{}", self.pfx, self.n);
        self.line(format!("{} = {}", n, rhs));
        n
    }
}

fn int_of(em: &mut Em, bits: u32, v: &str, ib: u32) -> String {
    // `iN` の値 `v` から幅 `ib` を取り出した整数 (位置は呼び出し側で lshr 済み)。
    if bits == ib {
        v.to_string()
    } else if bits > ib {
        em.t(format!("trunc i{} {} to i{}", bits, v, ib))
    } else {
        em.t(format!("zext i{} {} to i{}", bits, v, ib))
    }
}

/// 値 `v` (`iN`) から部品 `s` を取り出す。
fn extract(em: &mut Em, nbits: u32, v: &str, s: &Slot) -> String {
    let sh = if s.off == 0 { v.to_string() } else { em.t(format!("lshr i{} {}, {}", nbits, v, s.off * 8)) };
    let iv = int_of(em, nbits, &sh, s.vbits);
    if s.float {
        em.t(format!("bitcast i{} {} to {}", s.vbits, iv, s.ty))
    } else if let Some(w) = s.ty.strip_prefix('i').and_then(|w| w.parse::<u32>().ok()) {
        if w > s.vbits {
            em.t(format!("zext i{} {} to i{}", s.vbits, iv, w))
        } else {
            iv
        }
    } else {
        iv
    }
}

/// 部品 `pv` (型 `s.ty`) を `iN` の中の位置に置いた整数 (`or` で重ねる前の形)。
fn place(em: &mut Em, nbits: u32, pv: &str, s: &Slot) -> String {
    let iv = if s.float { em.t(format!("bitcast {} {} to i{}", s.ty, pv, s.vbits)) } else if let Some(w) = s.ty.strip_prefix('i').and_then(|w| w.parse::<u32>().ok()) {
        if w > s.vbits {
            em.t(format!("trunc i{} {} to i{}", w, pv, s.vbits))
        } else {
            pv.to_string()
        }
    } else {
        pv.to_string()
    };
    let z = if s.vbits == nbits { iv } else if s.vbits < nbits { em.t(format!("zext i{} {} to i{}", s.vbits, iv, nbits)) } else { em.t(format!("trunc i{} {} to i{}", s.vbits, iv, nbits)) };
    if s.off == 0 { z } else { em.t(format!("shl i{} {}, {}", nbits, z, s.off * 8)) }
}

fn combine(em: &mut Em, nbits: u32, parts: Vec<String>) -> String {
    let mut acc = parts[0].clone();
    for p in &parts[1..] {
        acc = em.t(format!("or i{} {}, {}", nbits, acc, p));
    }
    acc
}

/// 呼び出しの実引数 (`型 値` の並び)。`v` は `iN` の値。
pub fn call_args(em: &mut Em, abi: &Abi, nbits: u32, v: &str) -> Vec<String> {
    match abi {
        Abi::Regs(slots) => slots.iter().map(|s| { let x = extract(em, nbits, v, s); format!("{} {}", s.ty, x) }).collect(),
        Abi::Array(slots, elem) => {
            let n = slots.len();
            let aty = format!("[{} x {}]", n, elem);
            let mut agg = "undef".to_string();
            for (j, s) in slots.iter().enumerate() {
                let x = extract(em, nbits, v, s);
                agg = em.t(format!("insertvalue {} {}, {} {}, {}", aty, agg, s.ty, x, j));
            }
            vec![format!("{} {}", aty, agg)]
        }
        Abi::Mem { size, align } => {
            let buf = mem_buf(em, *size, nbits, v);
            vec![format!("ptr byval([{} x i8]) align {} {}", size, align, buf)]
        }
        Abi::Ptr { size } => {
            let buf = mem_buf(em, *size, nbits, v);
            vec![format!("ptr {}", buf)]
        }
    }
}

fn mem_buf(em: &mut Em, size: u32, nbits: u32, v: &str) -> String {
    em.used_stack = true;
    let buf = em.t(format!("alloca [{} x i8], align 16", size.max(16)));
    em.line(format!("store i{} {}, ptr {}, align 1", nbits, v, buf));
    buf
}

/// 戻り値の LLVM の型 (`sret` は void)。
pub fn ret_ty(abi: &Abi) -> String {
    match abi {
        Abi::Regs(s) if s.len() == 1 => s[0].ty.clone(),
        Abi::Regs(s) => format!("{{ {} }}", s.iter().map(|x| x.ty.clone()).collect::<Vec<_>>().join(", ")),
        Abi::Array(s, e) => format!("[{} x {}]", s.len(), e),
        Abi::Mem { .. } | Abi::Ptr { .. } => "void".into(),
    }
}

/// 呼び出しの前の準備: `sret` なら結果の領域を作って隠しの第 1 引数の文字列を返す。
pub fn sret_arg(em: &mut Em, abi: &Abi) -> Option<(String, String)> {
    if let Abi::Mem { size, align } = abi {
        em.used_stack = true;
        let buf = em.t(format!("alloca [{} x i8], align 16", (*size).max(16)));
        return Some((format!("ptr sret([{} x i8]) align {} {}", size, align, buf), buf));
    }
    None
}

/// 戻り値 `r` (C の形) から `iN` の値を作る。`sret` なら `buf` から読む。
pub fn ret_value(em: &mut Em, abi: &Abi, nbits: u32, r: &str, buf: Option<&str>) -> String {
    match abi {
        Abi::Mem { .. } | Abi::Ptr { .. } => {
            let b = buf.expect("sret の領域");
            em.t(format!("load i{}, ptr {}, align 1", nbits, b))
        }
        Abi::Regs(s) if s.len() == 1 => place(em, nbits, r, &s[0]),
        Abi::Regs(s) => {
            let sty = ret_ty(abi);
            let parts: Vec<String> = s.iter().enumerate().map(|(j, sl)| {
                let x = em.t(format!("extractvalue {} {}, {}", sty, r, j));
                place(em, nbits, &x, sl)
            }).collect();
            combine(em, nbits, parts)
        }
        Abi::Array(s, e) => {
            let aty = format!("[{} x {}]", s.len(), e);
            let parts: Vec<String> = s.iter().enumerate().map(|(j, sl)| {
                let x = em.t(format!("extractvalue {} {}, {}", aty, r, j));
                place(em, nbits, &x, sl)
            }).collect();
            combine(em, nbits, parts)
        }
    }
}

/// C ABI のパラメータ宣言 (`型 属性 名前`) の並び。`name` は仮引数の名前の元。
pub fn param_decls(abi: &Abi, name: &str) -> Vec<String> {
    match abi {
        Abi::Regs(s) => s.iter().enumerate().map(|(j, x)| format!("{} {}_{}", x.ty, name, j)).collect(),
        Abi::Array(s, e) => vec![format!("[{} x {}] {}_0", s.len(), e, name)],
        Abi::Mem { size, align } => vec![format!("ptr byval([{} x i8]) align {} {}_0", size, align, name)],
        Abi::Ptr { .. } => vec![format!("ptr {}_0", name)],
    }
}

/// 宣言 (`declare`) 用のパラメータの型の並び。
pub fn param_tys(abi: &Abi) -> Vec<String> {
    match abi {
        Abi::Regs(s) => s.iter().map(|x| x.ty.clone()).collect(),
        Abi::Array(s, e) => vec![format!("[{} x {}]", s.len(), e)],
        Abi::Mem { size, align } => vec![format!("ptr byval([{} x i8]) align {}", size, align)],
        Abi::Ptr { .. } => vec!["ptr".into()],
    }
}

/// コールバックの入口: 仮引数 (`param_decls` の名前) から `iN` の値を作る。
pub fn value_from_params(em: &mut Em, abi: &Abi, nbits: u32, name: &str) -> String {
    match abi {
        Abi::Regs(s) => {
            let parts: Vec<String> = s.iter().enumerate().map(|(j, sl)| place(em, nbits, &format!("{}_{}", name, j), sl)).collect();
            combine(em, nbits, parts)
        }
        Abi::Array(s, e) => {
            let aty = format!("[{} x {}]", s.len(), e);
            let parts: Vec<String> = s.iter().enumerate().map(|(j, sl)| {
                let x = em.t(format!("extractvalue {} {}_0, {}", aty, name, j));
                place(em, nbits, &x, sl)
            }).collect();
            combine(em, nbits, parts)
        }
        Abi::Mem { .. } | Abi::Ptr { .. } => em.t(format!("load i{}, ptr {}_0, align 1", nbits, name)),
    }
}

/// コールバックの出口: `iN` の値 `v` を C の戻り値にして `ret` の行を出す。`sret` は `%sret` に書く。
pub fn emit_ret(em: &mut Em, abi: &Abi, nbits: u32, v: &str) {
    match abi {
        Abi::Mem { .. } | Abi::Ptr { .. } => {
            em.line(format!("store i{} {}, ptr %sret, align 1", nbits, v));
            em.line("ret void".into());
        }
        Abi::Regs(s) if s.len() == 1 => {
            let x = extract(em, nbits, v, &s[0]);
            em.line(format!("ret {} {}", s[0].ty, x));
        }
        Abi::Regs(s) => {
            let sty = ret_ty(abi);
            let mut agg = "undef".to_string();
            for (j, sl) in s.iter().enumerate() {
                let x = extract(em, nbits, v, sl);
                agg = em.t(format!("insertvalue {} {}, {} {}, {}", sty, agg, sl.ty, x, j));
            }
            em.line(format!("ret {} {}", sty, agg));
        }
        Abi::Array(s, e) => {
            let aty = format!("[{} x {}]", s.len(), e);
            let mut agg = "undef".to_string();
            for (j, sl) in s.iter().enumerate() {
                let x = extract(em, nbits, v, sl);
                agg = em.t(format!("insertvalue {} {}, {} {}, {}", aty, agg, sl.ty, x, j));
            }
            em.line(format!("ret {} {}", aty, agg));
        }
    }
}
