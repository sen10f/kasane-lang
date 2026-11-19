//! プロセス内の LLVM (`embedded-llvm`、設計_KIR.md 12.6): LLVM IR のテキストを最適化して目的ファイルにする。
//! 外部の `clang -x ir` の代わり。X86 / AArch64 / WebAssembly だけを初期化する。
use llvm_sys::core::*;
use llvm_sys::error::LLVMGetErrorMessage;
use llvm_sys::ir_reader::LLVMParseIRInContext;
use llvm_sys::prelude::*;
use llvm_sys::target::*;
use llvm_sys::target_machine::*;
use llvm_sys::transforms::pass_builder::*;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::Once;

static INIT: Once = Once::new();

fn init() {
    INIT.call_once(|| unsafe {
        LLVMInitializeX86TargetInfo();
        LLVMInitializeX86Target();
        LLVMInitializeX86TargetMC();
        LLVMInitializeX86AsmPrinter();
        LLVMInitializeAArch64TargetInfo();
        LLVMInitializeAArch64Target();
        LLVMInitializeAArch64TargetMC();
        LLVMInitializeAArch64AsmPrinter();
        LLVMInitializeWebAssemblyTargetInfo();
        LLVMInitializeWebAssemblyTarget();
        LLVMInitializeWebAssemblyTargetMC();
        LLVMInitializeWebAssemblyAsmPrinter();
    });
}

unsafe fn take_msg(p: *mut c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = CStr::from_ptr(p).to_string_lossy().trim().to_string();
    LLVMDisposeMessage(p);
    s
}

/// `opt` は clang と同じ綴り (`-O0` `-O1` `-O2` `-O3` `-Os` `-Oz`)。`triple` が空なら今の機械。
/// 返すのは `obj` に書いた目的ファイル。
pub fn ir_to_object(ll: &str, opt: &str, triple: &str, obj: &std::path::Path) -> Result<(), String> {
    init();
    unsafe {
        let ctx = LLVMContextCreate();
        let r = ir_to_object_in(ctx, ll, opt, triple, obj);
        LLVMContextDispose(ctx);
        r
    }
}

unsafe fn ir_to_object_in(ctx: LLVMContextRef, ll: &str, opt: &str, triple: &str, obj: &std::path::Path) -> Result<(), String> {
    let buf = LLVMCreateMemoryBufferWithMemoryRangeCopy(ll.as_ptr() as *const c_char, ll.len(), c"ksn.ll".as_ptr());
    let mut module: LLVMModuleRef = std::ptr::null_mut();
    let mut err: *mut c_char = std::ptr::null_mut();
    // 成功すると buf は module が引き取る。
    if LLVMParseIRInContext(ctx, buf, &mut module, &mut err) != 0 {
        return Err(format!("LLVM IR を読めない: {}", take_msg(err)));
    }
    let r = (|| {
        let triple_c = if triple.is_empty() {
            let t = LLVMGetDefaultTargetTriple();
            let s = CStr::from_ptr(t).to_owned();
            LLVMDisposeMessage(t);
            s
        } else {
            CString::new(triple).map_err(|e| e.to_string())?
        };
        let mut target: LLVMTargetRef = std::ptr::null_mut();
        let mut err: *mut c_char = std::ptr::null_mut();
        if LLVMGetTargetFromTriple(triple_c.as_ptr(), &mut target, &mut err) != 0 {
            return Err(format!("LLVM の対象 {:?} を使えない: {}", triple_c, take_msg(err)));
        }
        let t = triple_c.to_string_lossy();
        // clang の既定の CPU (x86-64 は `x86-64`、ほかは generic) と、PIC (clang の Linux の PIE の既定)。
        let cpu: &CStr = if t.starts_with("x86_64") { c"x86-64" } else { c"generic" };
        let (level, pipeline) = match opt {
            "-O0" => (LLVMCodeGenOptLevel::LLVMCodeGenLevelNone, "default<O0>"),
            "-O2" => (LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault, "default<O2>"),
            "-O3" => (LLVMCodeGenOptLevel::LLVMCodeGenLevelAggressive, "default<O3>"),
            "-Os" => (LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault, "default<Os>"),
            "-Oz" => (LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault, "default<Oz>"),
            _ => (LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault, "default<O1>"),
        };
        let reloc = if t.starts_with("wasm") { LLVMRelocMode::LLVMRelocDefault } else { LLVMRelocMode::LLVMRelocPIC };
        let tm = LLVMCreateTargetMachine(target, triple_c.as_ptr(), cpu.as_ptr(), c"".as_ptr(), level, reloc, LLVMCodeModel::LLVMCodeModelDefault);
        if tm.is_null() {
            return Err("LLVM の TargetMachine を作れない".to_string());
        }
        let r = (|| {
            LLVMSetTarget(module, triple_c.as_ptr());
            let dl = LLVMCreateTargetDataLayout(tm);
            let dls = LLVMCopyStringRepOfTargetData(dl);
            LLVMSetDataLayout(module, dls);
            LLVMDisposeMessage(dls);
            LLVMDisposeTargetData(dl);
            let po = LLVMCreatePassBuilderOptions();
            let pl = CString::new(pipeline).unwrap();
            let e = LLVMRunPasses(module, pl.as_ptr(), tm, po);
            LLVMDisposePassBuilderOptions(po);
            if !e.is_null() {
                let m = LLVMGetErrorMessage(e);
                let s = CStr::from_ptr(m).to_string_lossy().to_string();
                llvm_sys::error::LLVMDisposeErrorMessage(m);
                return Err(format!("LLVM の最適化が失敗した: {}", s));
            }
            let path = CString::new(obj.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
            let mut err: *mut c_char = std::ptr::null_mut();
            if LLVMTargetMachineEmitToFile(tm, module, path.as_ptr() as *mut c_char, LLVMCodeGenFileType::LLVMObjectFile, &mut err) != 0 {
                return Err(format!("LLVM の目的ファイル生成が失敗した: {}", take_msg(err)));
            }
            Ok(())
        })();
        LLVMDisposeTargetMachine(tm);
        r
    })();
    LLVMDisposeModule(module);
    r
}
