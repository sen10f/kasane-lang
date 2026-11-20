/* 全ターゲットの初期化関数を参照するオブジェクト。
   link_llvm() で libksn_llvm_all.a に組み込み、リンク時に
   LLVMInitialize* シンボルが未定義になるよう保証する。
   これにより、libksn_llvm_stubs.a の弱いスタブが正しく解決される
   (アーカイブの処理順序による未解決を防ぐ)。 */
void LLVMInitializeX86TargetInfo(void);
void LLVMInitializeX86Target(void);
void LLVMInitializeX86TargetMC(void);
void LLVMInitializeX86AsmPrinter(void);
void LLVMInitializeAArch64TargetInfo(void);
void LLVMInitializeAArch64Target(void);
void LLVMInitializeAArch64TargetMC(void);
void LLVMInitializeAArch64AsmPrinter(void);
void LLVMInitializeWebAssemblyTargetInfo(void);
void LLVMInitializeWebAssemblyTarget(void);
void LLVMInitializeWebAssemblyTargetMC(void);
void LLVMInitializeWebAssemblyAsmPrinter(void);

void ksn_ref_llvm_targets(void) {
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
}
