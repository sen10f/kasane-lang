/* クロス LLVM (AArch64 のみ) には含まれない X86 / WebAssembly の
   ターゲット初期化関数の弱いスタブ。host のフル LLVM では強い
   シンボル (libksn_llvm_all に含まれる) が勝ち、クロスではこの
   スタブ (何もしない) が使われる。embed_llvm::init() は ksn run の
   インタプリタ経路からは呼ばれないので、スタブでも問題ない。 */
__attribute__((weak)) void LLVMInitializeX86TargetInfo(void) {}
__attribute__((weak)) void LLVMInitializeX86Target(void) {}
__attribute__((weak)) void LLVMInitializeX86TargetMC(void) {}
__attribute__((weak)) void LLVMInitializeX86AsmPrinter(void) {}
__attribute__((weak)) void LLVMInitializeWebAssemblyTargetInfo(void) {}
__attribute__((weak)) void LLVMInitializeWebAssemblyTarget(void) {}
__attribute__((weak)) void LLVMInitializeWebAssemblyTargetMC(void) {}
__attribute__((weak)) void LLVMInitializeWebAssemblyAsmPrinter(void) {}
