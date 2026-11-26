//! VM の `extern` 呼び出し・コールバックで、構造体を値で受け渡すための橋 (C の ABI の分類は `ffi::classify` を使う。LLVM の `kir_cabi` と同じ System V の分類)。

use crate::ffi::{self, ArgV, CStruct, CTy, RetTy, RetV};
use crate::kir::{AggDef, AggKind, Func, Ty};
use std::rc::Rc;

/// KIR の型 → C の型。`layout c` 相当の構造体・配列・スカラーだけ。
pub fn cty_of(aggs: &[AggDef], t: Ty) -> Result<CTy, String> {
    Ok(CTy::Scalar(match t {
        Ty::I8 => "i8",
        Ty::I16 => "i16",
        Ty::I32 => "i32",
        Ty::I64 => "i64",
        Ty::Isize => "isize",
        Ty::U8 => "u8",
        Ty::U16 => "u16",
        Ty::U32 => "u32",
        Ty::U64 => "u64",
        Ty::Usize => "usize",
        Ty::F32 => "f32",
        Ty::F64 => "f64",
        Ty::Bool => "bool",
        Ty::Ptr | Ty::Ref(..) => "ptr",
        Ty::Agg(i) => {
            let d = &aggs[i as usize];
            return match &d.kind {
                AggKind::Struct(fs) if !fs.is_empty() => {
                    let mut fields = vec![];
                    for f in fs {
                        fields.push((f.name.clone(), f.off, cty_of(aggs, f.ty)?));
                    }
                    Ok(CTy::Struct(Rc::new(CStruct { name: d.name.clone(), size: d.size, align: d.align.max(1), fields })))
                }
                AggKind::Array(e, n) => Ok(CTy::Array(Rc::new(cty_of(aggs, *e)?), *n)),
                _ => Err(format!("{} は C の値渡しにできない", d.name)),
            };
        }
        _ => return Err("C の値渡しにできない型".into()),
    }))
}

pub fn has_agg(f: &Func) -> bool {
    matches!(f.ret, Ty::Agg(_)) || f.params().iter().any(|(_, t)| matches!(t, Ty::Agg(_)))
}

/// 構造体を値で受け渡す `extern` の呼び出し。引数はスロット列 (`slots[k] = (先頭のスロット, 語数)`)、結果は語の列。
/// SAFETY: `rp` の `slots` はこのフレームの生きているスロット。
pub unsafe fn call_ext(aggs: &[AggDef], func: &Func, rp: *mut u64, slots: &[(usize, usize)]) -> Result<Vec<u64>, String> {
    let link = func.link.as_ref().ok_or("extern 関数に link がない")?;
    let (lib, sym) = ffi::split_link(link);
    let fnptr = ffi::lookup(lib, sym).map_err(|e| format!("extern {}: {}", func.fqn, e))?;
    let mut abi = vec![];
    for ((_, t), (s, n)) in func.params().iter().zip(slots) {
        abi.push(match t {
            Ty::F32 => ArgV::F32(f32::from_bits(*rp.add(*s) as u32)),
            Ty::F64 => ArgV::F64(f64::from_bits(*rp.add(*s))),
            Ty::Agg(_) => {
                let ct = cty_of(aggs, *t).map_err(|e| format!("extern {}: {}", func.fqn, e))?;
                let b = std::slice::from_raw_parts(rp.add(*s) as *const u8, ct.size() as usize);
                let _ = n;
                ArgV::Agg(b.to_vec(), ct)
            }
            _ => ArgV::Int(*rp.add(*s)),
        });
    }
    let rty = match func.ret {
        Ty::F32 => RetTy::F32,
        Ty::F64 => RetTy::F64,
        Ty::Unit => RetTy::Void,
        Ty::Agg(_) => RetTy::Agg(cty_of(aggs, func.ret).map_err(|e| format!("extern {}: {}", func.fqn, e))?),
        _ => RetTy::Int,
    };
    let r = ffi::call_generic(fnptr, &abi, &rty).map_err(|e| format!("extern {}: {}", func.fqn, e))?;
    Ok(match r {
        RetV::Int(x) => vec![x],
        RetV::F32(f) => vec![f.to_bits() as u64],
        RetV::F64(f) => vec![f.to_bits()],
        RetV::None => vec![0],
        RetV::Agg(b) => {
            let mut w = vec![0u64; b.len().div_ceil(8).max(1)];
            std::ptr::copy_nonoverlapping(b.as_ptr(), w.as_mut_ptr() as *mut u8, b.len());
            w
        }
    })
}

/// コールバックの実引数の構造体 1 個を、レジスタ・スタックから集める。語の列 (スロット数ぶん) を返す。
pub fn cb_arg(ct: &CTy, gp: &[u64; 6], sse: &[u64; 8], stack: *const u64, gi: &mut usize, si: &mut usize, stack_off: &mut usize, nwords: usize) -> Vec<u64> {
    let mut w = vec![0u64; nwords.max(1)];
    let size = ct.size() as usize;
    let buf = w.as_mut_ptr() as *mut u8;
    match ffi::classify(ct) {
        ffi::Pass::Regs(cls) => {
            for (j, &is_sse) in cls.iter().enumerate() {
                let v = if is_sse {
                    *si += 1;
                    sse[*si - 1]
                } else {
                    *gi += 1;
                    gp[*gi - 1]
                };
                let lo = j * 8;
                let hi = (lo + 8).min(size);
                unsafe { std::ptr::copy_nonoverlapping(v.to_le_bytes().as_ptr(), buf.add(lo), hi - lo) };
            }
        }
        ffi::Pass::Memory => {
            unsafe { std::ptr::copy_nonoverlapping(stack.add(*stack_off) as *const u8, buf, size) };
            *stack_off += size.div_ceil(8);
        }
    }
    w
}

/// コールバックの構造体の戻り値を `out` (レジスタ) か sret の先 (`gp[0]`) に書く。
pub fn cb_ret(ct: &CTy, words: &[u64], gp: &[u64; 6], out: &mut ffi::RetRegs) {
    let size = ct.size() as usize;
    let b = unsafe { std::slice::from_raw_parts(words.as_ptr() as *const u8, size.min(words.len() * 8)) };
    match ffi::classify(ct) {
        ffi::Pass::Memory => unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), gp[0] as *mut u8, b.len()) },
        ffi::Pass::Regs(cls) => {
            let (mut g, mut s) = (0, 0);
            for (j, &is_sse) in cls.iter().enumerate() {
                let mut x = [0u8; 8];
                let lo = j * 8;
                let hi = (lo + 8).min(b.len());
                x[..hi - lo].copy_from_slice(&b[lo..hi]);
                let v = u64::from_le_bytes(x);
                match (is_sse, if is_sse { s } else { g }) {
                    (true, 0) => out.x0 = v,
                    (true, _) => out.x1 = v,
                    (false, 0) => out.rax = v,
                    (false, _) => out.rdx = v,
                }
                if is_sse { s += 1 } else { g += 1 }
            }
        }
    }
}
